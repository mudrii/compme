//! Pure policy for local text features.
//!
//! The run loop supplies current settings, field capabilities, and live prefs;
//! this module owns feature priority, compatibility/privacy gates, token safety,
//! and exact selection-range construction.

use emoji::EmojiPrefs;
use platform::{Capabilities, CorrectionRange, PlatformError, TextContext};
use prefs::Prefs;

#[derive(Clone, Copy)]
pub(crate) struct SuggestionTarget<'a> {
    pub(crate) app_key: Option<&'a str>,
    pub(crate) assistant_field: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct FeatureSwitches<'a> {
    pub(crate) emoji: Option<&'a EmojiPrefs>,
    pub(crate) autocorrect: bool,
    pub(crate) full_autocorrect: bool,
    pub(crate) british_english: bool,
    pub(crate) thesaurus: bool,
    pub(crate) thesaurus_selection: bool,
}

pub(crate) struct FeaturePolicy<'a> {
    switches: FeatureSwitches<'a>,
    prefs: &'a Prefs,
    target: SuggestionTarget<'a>,
    domain: Option<&'a str>,
    enabled: bool,
    now_ms: u64,
}

impl<'a> FeaturePolicy<'a> {
    pub(crate) fn new(
        switches: FeatureSwitches<'a>,
        prefs: &'a Prefs,
        target: SuggestionTarget<'a>,
        domain: Option<&'a str>,
        enabled: bool,
        now_ms: u64,
    ) -> Self {
        Self {
            switches,
            prefs,
            target,
            domain,
            enabled,
            now_ms,
        }
    }

    pub(crate) fn local_replacement(&self, left: &str) -> Option<(Vec<String>, usize)> {
        if !self.enabled
            || !suggestion_gates_pass(self.target, left, self.domain, self.prefs, self.now_ms)
        {
            return None;
        }
        let autocorrect = self
            .prefs
            .autocorrect_enabled(self.target.app_key, self.switches.autocorrect);
        let thesaurus = self
            .prefs
            .thesaurus_enabled(self.target.app_key, self.switches.thesaurus);
        replacement_offer(left, self.switches, autocorrect, thesaurus)
    }

    pub(crate) fn full_autocorrect(
        &self,
        left: &str,
        spelling_correction: impl FnOnce(&str) -> Result<Option<String>, PlatformError>,
    ) -> Option<(Vec<String>, usize)> {
        let supported_prose_surface = self.target.assistant_field
            || self
                .target
                .app_key
                .is_some_and(compat::supports_statistical_autocorrect);
        if !self.enabled
            || !self
                .prefs
                .autocorrect_enabled(self.target.app_key, self.switches.full_autocorrect)
            || !suggestion_gates_pass(self.target, left, self.domain, self.prefs, self.now_ms)
            || !supported_prose_surface
            || self.target.app_key.is_some_and(|app_key| {
                compat::is_code_editor(app_key) && !self.target.assistant_field
            })
            || code_like_autocorrect_context(left)
        {
            return None;
        }

        let word = trailing_word(left)?;
        let word_len = word.chars().count();
        if !(2..=64).contains(&word_len) {
            return None;
        }
        let correction = spelling_correction(word).ok().flatten()?;
        let correction = correction.trim();
        if correction.is_empty()
            || correction.eq_ignore_ascii_case(word)
            || correction.chars().any(char::is_whitespace)
            || !correction
                .chars()
                .all(|ch| ch.is_alphabetic() || ch == '\'')
        {
            return None;
        }
        Some((vec![correction.to_string()], word_len))
    }

    pub(crate) fn selection_thesaurus(
        &self,
        ctx: &TextContext,
        caps: &Capabilities,
    ) -> Option<(String, Vec<String>, CorrectionRange)> {
        let selection = ctx.selection?;
        if selection.start >= selection.end
            || !self.enabled
            || !self
                .prefs
                .thesaurus_enabled(self.target.app_key, self.switches.thesaurus_selection)
            || !suggestion_gates_pass(
                self.target,
                ctx.selected_text.as_deref().unwrap_or_default(),
                self.domain,
                self.prefs,
                self.now_ms,
            )
            || !caps.insert_strategy.supports_atomic_range_replace()
        {
            return None;
        }

        let original = ctx.selected_text.as_deref()?;
        if original.trim() != original
            || !(2..=64).contains(&original.chars().count())
            || !original.chars().all(char::is_alphabetic)
        {
            return None;
        }
        let candidates = thesaurus::synonyms(original);
        if candidates.is_empty() {
            return None;
        }
        let start = ctx.left_scalars;
        Some((
            original.to_string(),
            candidates,
            CorrectionRange {
                start,
                end: start + original.chars().count(),
            },
        ))
    }
}

pub(crate) fn app_allows_suggestions(target: SuggestionTarget<'_>) -> bool {
    target.app_key.is_none_or(|app_key| {
        let tier = compat::compatibility_tier(app_key);
        tier.allows_suggestions() && (!tier.sidebar_only() || target.assistant_field)
    })
}

pub(crate) fn suggestion_gates_pass(
    target: SuggestionTarget<'_>,
    text: &str,
    domain: Option<&str>,
    prefs: &Prefs,
    now_ms: u64,
) -> bool {
    let terminal_ok = target
        .app_key
        .is_none_or(|app| compat::terminal_prompt_activates(app, text));
    app_allows_suggestions(target)
        && terminal_ok
        && prefs.should_suggest(target.app_key, domain, now_ms)
}

pub(crate) fn emoji_offer(left: &str, prefs: Option<&EmojiPrefs>) -> Option<(String, usize)> {
    let suggestion = emoji::suggest(left, prefs?)?;
    Some((suggestion.glyph, suggestion.replace_chars))
}

pub(crate) fn trailing_word(left: &str) -> Option<&str> {
    let start = left
        .char_indices()
        .rev()
        .take_while(|(_, ch)| ch.is_alphabetic())
        .last()
        .map(|(index, _)| index)?;
    let word = &left[start..];
    (!word.is_empty()).then_some(word)
}

pub(crate) fn replacement_offer(
    left: &str,
    switches: FeatureSwitches<'_>,
    autocorrect_enabled: bool,
    thesaurus_enabled: bool,
) -> Option<(Vec<String>, usize)> {
    if let Some((glyph, len)) = emoji_offer(left, switches.emoji) {
        return Some((vec![glyph], len));
    }
    let word = trailing_word(left)?;
    let word_len = word.chars().count();
    if autocorrect_enabled {
        if let Some(fix) = autocorrect::correct(word) {
            return Some((vec![fix], word_len));
        }
        if let Some(fix) = grammar::capitalize_pronoun(word) {
            return Some((vec![fix], word_len));
        }
    }
    if switches.british_english {
        if let Some(uk) = localize::to_british(word) {
            return Some((vec![uk], word_len));
        }
    }
    if thesaurus_enabled {
        let synonyms = thesaurus::synonyms(word);
        if !synonyms.is_empty() {
            return Some((synonyms, word_len));
        }
    }
    None
}

fn code_like_autocorrect_context(left: &str) -> bool {
    let tail: String = left
        .chars()
        .rev()
        .take(120)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    ["::", "->", "=>", "//", "/*", "*/", "()", "{}", "[]"]
        .iter()
        .any(|marker| tail.contains(marker))
        || tail
            .chars()
            .any(|ch| matches!(ch, '{' | '}' | ';' | '`' | '='))
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::{
        ContextSource, FieldHandle, InsertStrategy, KeyInterceptMode, OffsetEncoding,
        OverlayPlacement, SecurityState, TextRange, Toolkit,
    };
    use prefs::AppPolicyField;
    use std::cell::Cell;

    const TEXTEDIT: &str = "com.apple.TextEdit";
    const VSCODE: &str = "com.microsoft.VSCode";
    const ITERM: &str = "com.googlecode.iterm2";
    const PAGES: &str = "com.apple.iWork.Pages";

    fn all_on(emoji: Option<&EmojiPrefs>) -> FeatureSwitches<'_> {
        FeatureSwitches {
            emoji,
            autocorrect: true,
            full_autocorrect: true,
            british_english: true,
            thesaurus: true,
            thesaurus_selection: true,
        }
    }

    fn target(app_key: Option<&str>) -> SuggestionTarget<'_> {
        SuggestionTarget {
            app_key,
            assistant_field: false,
        }
    }

    fn assistant(app_key: Option<&str>) -> SuggestionTarget<'_> {
        SuggestionTarget {
            app_key,
            assistant_field: true,
        }
    }

    fn policy<'a>(
        switches: FeatureSwitches<'a>,
        prefs: &'a Prefs,
        target: SuggestionTarget<'a>,
        enabled: bool,
    ) -> FeaturePolicy<'a> {
        FeaturePolicy::new(switches, prefs, target, None, enabled, 0)
    }

    fn selection_ctx(left: &str, selected: Option<&str>) -> TextContext {
        let start = left.chars().count();
        let end = start + selected.map_or(0, |s| s.chars().count());
        TextContext {
            left: left.into(),
            right: " today".into(),
            left_scalars: start,
            selection: selected.map(|_| TextRange { start, end }),
            selected_text: selected.map(str::to_string),
            caret: start,
            source: ContextSource::Accessibility,
            field_id: FieldHandle {
                app: "TextEdit".into(),
                pid: Some(7),
                element_id: "policy-field".into(),
                generation: 1,
            },
            offset_encoding: OffsetEncoding::UnicodeScalars,
        }
    }

    fn caps(insert_strategy: InsertStrategy) -> Capabilities {
        Capabilities {
            readable_text: true,
            readable_caret: true,
            writable: true,
            assistant_field: false,
            secure: false,
            security_state: SecurityState::Normal,
            toolkit: Toolkit::AppKit,
            multiline: true,
            insert_strategy,
            accept_intercept: KeyInterceptMode::CgEventTap,
            overlay_at_caret: OverlayPlacement::NativePanel,
            coords_global_screen: true,
        }
    }

    fn spell(word: &str) -> Result<Option<String>, PlatformError> {
        Ok(match word {
            "wrld" => Some("world".into()),
            "dont" => Some("don't".into()),
            _ => None,
        })
    }

    #[test]
    fn code_like_context_flags_each_marker_and_passes_prose() {
        for marker in ["::", "->", "=>", "//", "/*", "*/", "()", "{}", "[]"] {
            assert!(
                code_like_autocorrect_context(&format!("let x{marker} teh")),
                "two-char marker {marker:?} must read as code"
            );
        }
        for ch in ['{', '}', ';', '`', '='] {
            assert!(
                code_like_autocorrect_context(&format!("x {ch} teh")),
                "single char {ch:?} must read as code"
            );
        }
        assert!(!code_like_autocorrect_context("the quick brown fox teh"));
        assert!(!code_like_autocorrect_context(""));
        // Parentheses, brackets and slashes alone are ordinary prose
        // punctuation; only the paired/doubled forms count.
        assert!(!code_like_autocorrect_context(
            "see (this) and [that] or 1/2 teh"
        ));
    }

    #[test]
    fn code_like_context_only_inspects_the_last_120_chars() {
        let far = format!("{{{}", " a".repeat(70));
        assert!(
            !code_like_autocorrect_context(&far),
            "a brace older than the 120-char window must not poison prose"
        );
        let near = format!("{}{{{}", " a".repeat(70), " b".repeat(10));
        assert!(code_like_autocorrect_context(&near));
    }

    #[test]
    fn trailing_word_takes_the_alphabetic_run_at_the_end() {
        assert_eq!(trailing_word("hello wrld"), Some("wrld"));
        assert_eq!(trailing_word("wrld"), Some("wrld"));
        assert_eq!(trailing_word("un café"), Some("café"));
        assert_eq!(trailing_word("hello "), None);
        assert_eq!(trailing_word("x1"), None);
        assert_eq!(trailing_word(""), None);
    }

    #[test]
    fn emoji_offer_needs_prefs_and_a_trailing_shortcode() {
        assert_eq!(emoji_offer("hi :smile", None), None);
        let prefs = EmojiPrefs::default();
        assert_eq!(
            emoji_offer("hi :smile", Some(&prefs)),
            Some(("😄".into(), 6))
        );
        assert_eq!(emoji_offer("hi smile", Some(&prefs)), None);
    }

    #[test]
    fn replacement_offer_priority_is_emoji_typo_pronoun_british_thesaurus() {
        let emoji = EmojiPrefs::default();
        let switches = all_on(Some(&emoji));
        // Emoji wins over everything, even with a typo earlier on the line.
        assert_eq!(
            replacement_offer("teh :tada", switches, true, true),
            Some((vec!["🎉".into()], 5))
        );
        // Typo fix.
        assert_eq!(
            replacement_offer("say teh", switches, true, true),
            Some((vec!["the".into()], 3))
        );
        // Pronoun capitalisation rides the autocorrect switch.
        assert_eq!(
            replacement_offer("then i", switches, true, true),
            Some((vec!["I".into()], 1))
        );
        assert_eq!(replacement_offer("say teh", switches, false, false), None);
        assert_eq!(replacement_offer("then i", switches, false, false), None);
        // British localisation is independent of autocorrect/thesaurus.
        assert_eq!(
            replacement_offer("nice color", switches, false, false),
            Some((vec!["colour".into()], 5))
        );
        let no_uk = FeatureSwitches {
            british_english: false,
            ..switches
        };
        assert_eq!(replacement_offer("nice color", no_uk, false, false), None);
        // Thesaurus is last, and only when enabled.
        let (synonyms, len) =
            replacement_offer("so happy", switches, true, true).expect("synonyms");
        assert!(synonyms.contains(&"glad".to_string()));
        assert_eq!(len, 5);
        assert_eq!(replacement_offer("so happy", switches, true, false), None);
        // Unknown word: nothing at all.
        assert_eq!(replacement_offer("so zzqx", switches, true, true), None);
        assert_eq!(replacement_offer("so ", switches, true, true), None);
    }

    #[test]
    fn app_allows_suggestions_follows_the_compat_tier_and_assistant_flag() {
        assert!(app_allows_suggestions(target(None)));
        assert!(app_allows_suggestions(target(Some(TEXTEDIT))));
        assert!(!app_allows_suggestions(target(Some(VSCODE))));
        assert!(app_allows_suggestions(assistant(Some(VSCODE))));
        assert!(!app_allows_suggestions(target(Some(PAGES))));
        assert!(
            !app_allows_suggestions(assistant(Some(PAGES))),
            "an Unsupported tier is not rescued by an assistant field"
        );
    }

    #[test]
    fn suggestion_gates_combine_tier_terminal_heuristic_and_prefs() {
        let prefs = Prefs::default();
        assert!(suggestion_gates_pass(
            target(Some(TEXTEDIT)),
            "hi",
            None,
            &prefs,
            0
        ));
        assert!(!suggestion_gates_pass(
            target(Some(VSCODE)),
            "hi",
            None,
            &prefs,
            0
        ));
        assert!(!suggestion_gates_pass(
            target(Some(ITERM)),
            "git status && ls -la",
            None,
            &prefs,
            0
        ));
        assert!(suggestion_gates_pass(
            target(Some(ITERM)),
            "explain this ||",
            None,
            &prefs,
            0
        ));

        let mut snoozed = Prefs::default();
        snoozed.snooze(0, 5);
        assert!(!suggestion_gates_pass(
            target(Some(TEXTEDIT)),
            "hi",
            None,
            &snoozed,
            0
        ));
        assert!(
            suggestion_gates_pass(target(Some(TEXTEDIT)), "hi", None, &snoozed, 6 * 60_000),
            "the snooze expires"
        );

        let mut excluded = Prefs::default();
        excluded.excluded_apps.insert(TEXTEDIT.into());
        assert!(!suggestion_gates_pass(
            target(Some(TEXTEDIT)),
            "hi",
            None,
            &excluded,
            0
        ));

        let mut domain_blocked = Prefs::default();
        domain_blocked.excluded_domains.insert("example.com".into());
        assert!(!suggestion_gates_pass(
            target(None),
            "hi",
            Some("www.example.com"),
            &domain_blocked,
            0
        ));
        assert!(suggestion_gates_pass(
            target(None),
            "hi",
            Some("example.org"),
            &domain_blocked,
            0
        ));

        let mut app_off = Prefs::default();
        app_off.set_app_policy_field(TEXTEDIT, AppPolicyField::Enabled, false);
        assert!(!suggestion_gates_pass(
            target(Some(TEXTEDIT)),
            "hi",
            None,
            &app_off,
            0
        ));
    }

    #[test]
    fn local_replacement_honors_enabled_gates_and_per_app_overrides() {
        let prefs = Prefs::default();
        let switches = all_on(None);
        assert_eq!(
            policy(switches, &prefs, target(Some(TEXTEDIT)), true).local_replacement("say teh"),
            Some((vec!["the".into()], 3))
        );
        assert_eq!(
            policy(switches, &prefs, target(Some(TEXTEDIT)), false).local_replacement("say teh"),
            None,
            "master enable off"
        );
        assert_eq!(
            policy(switches, &prefs, target(Some(VSCODE)), true).local_replacement("say teh"),
            None,
            "suggestion gate (sidebar-only editor) blocks local replacements too"
        );

        // Per-app autocorrect override beats the global switch in both directions.
        let mut app_off = Prefs::default();
        app_off.set_app_policy_field(TEXTEDIT, AppPolicyField::Autocorrect, false);
        assert_eq!(
            policy(switches, &app_off, target(Some(TEXTEDIT)), true).local_replacement("say teh"),
            None
        );
        let mut app_on = Prefs::default();
        app_on.set_app_policy_field(TEXTEDIT, AppPolicyField::Autocorrect, true);
        let global_off = FeatureSwitches {
            autocorrect: false,
            british_english: false,
            thesaurus: false,
            ..switches
        };
        assert_eq!(
            policy(global_off, &app_on, target(Some(TEXTEDIT)), true).local_replacement("say teh"),
            Some((vec!["the".into()], 3))
        );

        // Per-app thesaurus override.
        let mut thesaurus_off = Prefs::default();
        thesaurus_off
            .per_app
            .entry(TEXTEDIT.into())
            .or_default()
            .thesaurus = Some(false);
        assert_eq!(
            policy(switches, &thesaurus_off, target(Some(TEXTEDIT)), true)
                .local_replacement("so happy"),
            None
        );
        assert!(policy(switches, &prefs, target(Some(TEXTEDIT)), true)
            .local_replacement("so happy")
            .is_some());
    }

    #[test]
    fn full_autocorrect_offers_a_validated_correction_for_the_trailing_word() {
        let prefs = Prefs::default();
        let switches = all_on(None);
        let seen = Cell::new(None::<String>);
        let offer = policy(switches, &prefs, target(Some(TEXTEDIT)), true).full_autocorrect(
            "hello wrld",
            |word| {
                seen.set(Some(word.into()));
                spell(word)
            },
        );
        assert_eq!(offer, Some((vec!["world".into()], 4)));
        assert_eq!(seen.take().as_deref(), Some("wrld"));

        // An assistant field is a prose surface even with no app key, and an
        // assistant field inside a code editor overrides its editor status.
        assert_eq!(
            policy(switches, &prefs, assistant(None), true).full_autocorrect("hello wrld", spell),
            Some((vec!["world".into()], 4))
        );
        assert_eq!(
            policy(switches, &prefs, assistant(Some(VSCODE)), true)
                .full_autocorrect("hello wrld", spell),
            Some((vec!["world".into()], 4))
        );
        // Apostrophes are the one non-letter a correction may carry.
        assert_eq!(
            policy(switches, &prefs, target(Some(TEXTEDIT)), true)
                .full_autocorrect("i dont", spell),
            Some((vec!["don't".into()], 4))
        );
    }

    #[test]
    fn full_autocorrect_gates_short_circuit_before_the_spell_check() {
        let prefs = Prefs::default();
        let switches = all_on(None);
        let called = Cell::new(false);
        let counting = |word: &str| {
            called.set(true);
            spell(word)
        };
        let closed: [(&str, FeaturePolicy<'_>, &str); 7] = [
            (
                "master enable off",
                policy(switches, &prefs, target(Some(TEXTEDIT)), false),
                "hello wrld",
            ),
            (
                "full-autocorrect switch off",
                policy(
                    FeatureSwitches {
                        full_autocorrect: false,
                        ..switches
                    },
                    &prefs,
                    target(Some(TEXTEDIT)),
                    true,
                ),
                "hello wrld",
            ),
            (
                "unknown app is not a prose surface",
                policy(switches, &prefs, target(None), true),
                "hello wrld",
            ),
            (
                "code editor main pane",
                policy(switches, &prefs, target(Some(VSCODE)), true),
                "hello wrld",
            ),
            (
                "unsupported tier",
                policy(switches, &prefs, target(Some(PAGES)), true),
                "hello wrld",
            ),
            (
                "code-like context",
                policy(switches, &prefs, target(Some(TEXTEDIT)), true),
                "let x = wrld",
            ),
            (
                "terminal shell command",
                policy(switches, &prefs, target(Some(ITERM)), true),
                "git commit -m wrld",
            ),
        ];
        for (label, policy, left) in closed {
            called.set(false);
            assert_eq!(policy.full_autocorrect(left, counting), None, "{label}");
            assert!(
                !called.get(),
                "{label}: gate must not consult the spell checker"
            );
        }

        // Per-app autocorrect override closes the gate as well.
        let mut app_off = Prefs::default();
        app_off.set_app_policy_field(TEXTEDIT, AppPolicyField::Autocorrect, false);
        assert_eq!(
            policy(switches, &app_off, target(Some(TEXTEDIT)), true)
                .full_autocorrect("hello wrld", counting),
            None
        );
        assert!(!called.get());
    }

    #[test]
    fn full_autocorrect_rejects_unusable_words_and_corrections() {
        let prefs = Prefs::default();
        let switches = all_on(None);
        let open = policy(switches, &prefs, target(Some(TEXTEDIT)), true);

        assert_eq!(
            open.full_autocorrect("hello ", spell),
            None,
            "no trailing word"
        );
        assert_eq!(
            open.full_autocorrect("hello w", spell),
            None,
            "one-char word"
        );
        let long = format!("hello {}", "w".repeat(65));
        assert_eq!(
            open.full_autocorrect(&long, |_| Ok(Some("x".into()))),
            None,
            "words over 64 chars are skipped"
        );
        let max = format!("hello {}", "w".repeat(64));
        assert_eq!(
            open.full_autocorrect(&max, |_| Ok(Some("fixed".into()))),
            Some((vec!["fixed".into()], 64))
        );

        assert_eq!(open.full_autocorrect("hello wrld", |_| Ok(None)), None);
        assert_eq!(
            open.full_autocorrect("hello wrld", |_| Err(PlatformError::Timeout)),
            None,
            "a spell-check failure is a silent no-offer"
        );
        for bad in ["", "   ", "wrld", "WRLD", "wor ld", "wor1d", "world."] {
            assert_eq!(
                open.full_autocorrect("hello wrld", |_| Ok(Some(bad.into()))),
                None,
                "correction {bad:?} must be rejected"
            );
        }
        assert_eq!(
            open.full_autocorrect("hello wrld", |_| Ok(Some("  world  ".into()))),
            Some((vec!["world".into()], 4)),
            "surrounding whitespace is trimmed, not rejected"
        );
    }

    #[test]
    fn selection_thesaurus_builds_an_exact_range_for_one_selected_word() {
        let prefs = Prefs::default();
        let switches = all_on(None);
        let ctx = selection_ctx("I am ", Some("happy"));
        let (original, synonyms, range) = policy(switches, &prefs, target(Some(TEXTEDIT)), true)
            .selection_thesaurus(&ctx, &caps(InsertStrategy::AxSet))
            .expect("one selected known word");
        assert_eq!(original, "happy");
        assert!(synonyms.contains(&"glad".to_string()));
        assert!(!synonyms.contains(&"happy".to_string()));
        assert_eq!(range, CorrectionRange { start: 5, end: 10 });

        // The range is built from left_scalars + scalar length, not bytes.
        let multibyte = selection_ctx("café ", Some("happy"));
        let (_, _, range) = policy(switches, &prefs, target(Some(TEXTEDIT)), true)
            .selection_thesaurus(&multibyte, &caps(InsertStrategy::NativeRangeSet))
            .expect("NativeRangeSet also replaces atomically");
        assert_eq!(range, CorrectionRange { start: 5, end: 10 });
    }

    #[test]
    fn selection_thesaurus_closes_on_every_gate() {
        let prefs = Prefs::default();
        let switches = all_on(None);
        let atomic = caps(InsertStrategy::AxSet);
        let open = policy(switches, &prefs, target(Some(TEXTEDIT)), true);
        let happy = selection_ctx("I am ", Some("happy"));

        assert!(open
            .selection_thesaurus(&selection_ctx("I am ", None), &atomic)
            .is_none());
        let mut empty = happy.clone();
        empty.selection = Some(TextRange { start: 5, end: 5 });
        assert!(
            open.selection_thesaurus(&empty, &atomic).is_none(),
            "caret-only selection"
        );
        let mut no_text = happy.clone();
        no_text.selected_text = None;
        assert!(
            open.selection_thesaurus(&no_text, &atomic).is_none(),
            "range without payload"
        );

        assert!(policy(switches, &prefs, target(Some(TEXTEDIT)), false)
            .selection_thesaurus(&happy, &atomic)
            .is_none());
        assert!(policy(
            FeatureSwitches {
                thesaurus_selection: false,
                ..switches
            },
            &prefs,
            target(Some(TEXTEDIT)),
            true
        )
        .selection_thesaurus(&happy, &atomic)
        .is_none());
        assert!(
            policy(switches, &prefs, target(Some(VSCODE)), true)
                .selection_thesaurus(&happy, &atomic)
                .is_none(),
            "suggestion gate"
        );
        assert!(
            open.selection_thesaurus(&happy, &caps(InsertStrategy::SyntheticKeys))
                .is_none(),
            "non-atomic replace strategy"
        );

        for bad in [" happy", "happy ", "h", "two words", "happy1", "zzqx"] {
            let ctx = selection_ctx("I am ", Some(bad));
            assert!(
                open.selection_thesaurus(&ctx, &atomic).is_none(),
                "selected text {bad:?} must not offer"
            );
        }
        let too_long = "a".repeat(65);
        assert!(open
            .selection_thesaurus(&selection_ctx("", Some(&too_long)), &atomic)
            .is_none());
    }
}
