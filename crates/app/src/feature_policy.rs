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
