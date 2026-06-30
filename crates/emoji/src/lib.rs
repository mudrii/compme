//! Emoji completion (design spec §8 / §16): suggest an emoji when the user types
//! a `:shortcode`, honoring skin-tone and gender preferences. Pure and
//! OS-agnostic — detection + table lookup + modifier application; the host gates
//! it on the "emoji completion" toggle and performs the actual text replacement.
//!
//! Mirrors Cotypist's `EmojiCompletion_{preferredGender, preferredSkinTone}`
//! preferences. Skin-tone modifiers (Fitzpatrick U+1F3FB..U+1F3FF) are applied to
//! single-glyph people emoji that support them; gendered emoji resolve to their
//! neutral/female/male ZWJ variant. Skin tone *and* gender combine: the modifier
//! splices into the base of the gendered ZWJ sequence (see `with_skin_tone_zwj`).
//!
//! **Wiring status [updated 2026-06-11]:** WIRED — the host reads
//! `COMPME_EMOJI`/`_SKIN_TONE`/`_GENDER`, offers the emoji ghost through the
//! replacement path, and the §16 live gate passed 2026-06-10
//! (docs/ACCEPTANCE.md). The paragraph below is the original pre-wiring
//! plan, kept for the description of the accept mechanics — it was a
//! tracked next-task (engine integration). `includeVanillaVariants` (a Cotypist
//! sub-preference) is intentionally not modeled yet.

/// Fitzpatrick skin-tone preference. `Default` applies no modifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SkinTone {
    #[default]
    Default,
    Light,
    MediumLight,
    Medium,
    MediumDark,
    Dark,
}

impl SkinTone {
    /// The Unicode modifier codepoint to append, or `None` for the default tone.
    fn modifier(self) -> Option<char> {
        match self {
            SkinTone::Default => None,
            SkinTone::Light => Some('\u{1F3FB}'),
            SkinTone::MediumLight => Some('\u{1F3FC}'),
            SkinTone::Medium => Some('\u{1F3FD}'),
            SkinTone::MediumDark => Some('\u{1F3FE}'),
            SkinTone::Dark => Some('\u{1F3FF}'),
        }
    }
}

/// Gender preference for gendered emoji.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Gender {
    #[default]
    Neutral,
    Female,
    Male,
}

/// User emoji preferences (Cotypist parity).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EmojiPrefs {
    pub skin_tone: SkinTone,
    pub gender: Gender,
}

/// A suggested emoji replacement for a typed `:shortcode`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// The canonical shortcode that matched (e.g. `thumbsup` for `:+1`).
    pub shortcode: String,
    /// The rendered glyph, with skin-tone/gender modifiers applied.
    pub glyph: String,
    /// How many characters of the typed text to replace — the `:` plus the typed
    /// shortcode prefix (so the host deletes exactly that before inserting).
    pub replace_chars: usize,
}

struct Entry {
    shortcode: &'static str,
    base: &'static str,
    skin_tone: bool,
    /// `(neutral, female, male)` glyphs for gendered emoji.
    gendered: Option<(&'static str, &'static str, &'static str)>,
}

/// Curated shortcode table. Aliases (e.g. `+1`/`thumbsup`) are separate entries
/// pointing at the same glyph. Kept small and high-signal.
const TABLE: &[Entry] = &[
    Entry {
        shortcode: "smile",
        base: "😄",
        skin_tone: false,
        gendered: None,
    },
    Entry {
        shortcode: "heart",
        base: "❤️",
        skin_tone: false,
        gendered: None,
    },
    Entry {
        shortcode: "fire",
        base: "🔥",
        skin_tone: false,
        gendered: None,
    },
    Entry {
        shortcode: "rocket",
        base: "🚀",
        skin_tone: false,
        gendered: None,
    },
    Entry {
        shortcode: "tada",
        base: "🎉",
        skin_tone: false,
        gendered: None,
    },
    Entry {
        shortcode: "eyes",
        base: "👀",
        skin_tone: false,
        gendered: None,
    },
    Entry {
        shortcode: "100",
        base: "💯",
        skin_tone: false,
        gendered: None,
    },
    Entry {
        shortcode: "thumbsup",
        base: "👍",
        skin_tone: true,
        gendered: None,
    },
    Entry {
        shortcode: "+1",
        base: "👍",
        skin_tone: true,
        gendered: None,
    },
    Entry {
        shortcode: "thumbsdown",
        base: "👎",
        skin_tone: true,
        gendered: None,
    },
    Entry {
        shortcode: "-1",
        base: "👎",
        skin_tone: true,
        gendered: None,
    },
    Entry {
        shortcode: "wave",
        base: "👋",
        skin_tone: true,
        gendered: None,
    },
    Entry {
        shortcode: "clap",
        base: "👏",
        skin_tone: true,
        gendered: None,
    },
    Entry {
        shortcode: "ok_hand",
        base: "👌",
        skin_tone: true,
        gendered: None,
    },
    Entry {
        shortcode: "pray",
        base: "🙏",
        skin_tone: true,
        gendered: None,
    },
    Entry {
        // The neutral glyph 🙋 accepts a Fitzpatrick modifier, so skin_tone is
        // true; the female/male ZWJ variants keep the default tone (see `render`).
        shortcode: "raising_hand",
        base: "🙋",
        skin_tone: true,
        gendered: Some(("🙋", "🙋‍♀️", "🙋‍♂️")),
    },
    Entry {
        shortcode: "shrug",
        base: "🤷",
        skin_tone: true,
        gendered: Some(("🤷", "🤷‍♀️", "🤷‍♂️")),
    },
];

fn is_shortcode_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-')
}

/// Extract a trailing `:shortcode` token from `left_context`: the `:` must start
/// the string or follow whitespace (so `http://` or `ratio:2` don't trigger), and
/// the token after it must be non-empty and all shortcode characters. Returns the
/// token (without the colon) and the character count of the `:token` to replace.
fn trailing_shortcode(left_context: &str) -> Option<(&str, usize)> {
    let colon = left_context.rfind(':')?;
    let before_ok = left_context[..colon]
        .chars()
        .next_back()
        .is_none_or(|c| c.is_whitespace());
    if !before_ok {
        return None;
    }
    let token = &left_context[colon + 1..];
    if token.is_empty() || !token.chars().all(is_shortcode_char) {
        return None;
    }
    Some((token, 1 + token.chars().count()))
}

/// Minimum typed token length for a *prefix* match. A single character (`:+`,
/// `:s`) is too eager — it would fire on the first keystroke after the colon — so
/// prefix completion needs at least two characters. Exact matches still win at
/// any length.
const MIN_PREFIX_LEN: usize = 2;

/// Look up a shortcode: an exact match wins at any length; otherwise (for a typed
/// token of at least [`MIN_PREFIX_LEN`]) the shortest shortcode that the token is
/// a prefix of. `min_by_key` returns the first minimum, so table order is a
/// deterministic tie-break.
fn lookup(token: &str) -> Option<&'static Entry> {
    if let Some(entry) = TABLE.iter().find(|e| e.shortcode == token) {
        return Some(entry);
    }
    // `token` is all-ASCII (enforced by `is_shortcode_char`), so byte len == chars.
    if token.len() < MIN_PREFIX_LEN {
        return None;
    }
    TABLE
        .iter()
        .filter(|e| e.shortcode.starts_with(token))
        .min_by_key(|e| e.shortcode.len())
}

/// Apply a skin-tone modifier to a base people-emoji glyph.
///
/// The Fitzpatrick modifier is appended directly after the base codepoint, so
/// every `skin_tone:true` base MUST be a bare glyph with no trailing VS-16
/// (U+FE0F): appending the modifier after a variation selector produces an
/// invalid sequence (e.g. `☝️🏽` instead of the correct `☝🏽`). This invariant
/// is enforced by `skin_tone_bases_carry_no_variation_selector` below.
fn with_skin_tone(base: &str, skin_tone: SkinTone) -> String {
    let mut glyph = base.to_string();
    if let Some(modifier) = skin_tone.modifier() {
        glyph.push(modifier);
    }
    glyph
}

/// Splice a skin-tone modifier into a pre-composed gendered ZWJ sequence.
///
/// The Fitzpatrick modifier attaches to the base person glyph — the first scalar
/// of the sequence — *before* the ZWJ, not after the whole cluster. So
/// `🤷‍♀️` (person, ZWJ, ♀, VS-16) + medium tone → `🤷🏽‍♀️`
/// (person, tone, ZWJ, ♀, VS-16). The base glyph carries no VS-16 of its own
/// (same invariant as `with_skin_tone`), so inserting after the first char is safe.
fn with_skin_tone_zwj(sequence: &str, skin_tone: SkinTone) -> String {
    let Some(modifier) = skin_tone.modifier() else {
        return sequence.to_string();
    };
    let mut chars = sequence.chars();
    // Every gendered ZWJ sequence in the table is non-empty (base + ZWJ + sign).
    let base = chars.next().expect("gendered ZWJ sequence is non-empty");
    let mut glyph = String::with_capacity(sequence.len() + modifier.len_utf8());
    glyph.push(base);
    glyph.push(modifier);
    glyph.extend(chars);
    glyph
}

fn render(entry: &Entry, prefs: &EmojiPrefs) -> String {
    if let Some((neutral, female, male)) = entry.gendered {
        // Skin tone and gender are orthogonal. The neutral variant is a single
        // people-emoji; female/male are pre-composed ZWJ sequences. Apply skin
        // tone in all three when the entry supports it — `with_skin_tone` appends
        // to the bare neutral glyph; `with_skin_tone_zwj` splices the modifier
        // into the base of the ZWJ sequence. `skin_tone:false` keeps the default
        // tone everywhere (a glyph whose base can't take a Fitzpatrick modifier).
        return match prefs.gender {
            Gender::Neutral if entry.skin_tone => with_skin_tone(neutral, prefs.skin_tone),
            Gender::Neutral => neutral.to_string(),
            Gender::Female if entry.skin_tone => with_skin_tone_zwj(female, prefs.skin_tone),
            Gender::Female => female.to_string(),
            Gender::Male if entry.skin_tone => with_skin_tone_zwj(male, prefs.skin_tone),
            Gender::Male => male.to_string(),
        };
    }
    if entry.skin_tone {
        with_skin_tone(entry.base, prefs.skin_tone)
    } else {
        entry.base.to_string()
    }
}

/// Suggest an emoji for the `:shortcode` currently being typed at the end of
/// `left_context`, or `None` if there is no shortcode token or no match. The host
/// only calls this when emoji completion is enabled (the §16 toggle).
pub fn suggest(left_context: &str, prefs: &EmojiPrefs) -> Option<Suggestion> {
    let (token, replace_chars) = trailing_shortcode(left_context)?;
    let entry = lookup(token)?;
    Some(Suggestion {
        shortcode: entry.shortcode.to_string(),
        glyph: render(entry, prefs),
        replace_chars,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggest_default(left: &str) -> Option<Suggestion> {
        suggest(left, &EmojiPrefs::default())
    }

    /// Table invariant: any glyph that `with_skin_tone` may modify must be a
    /// bare base with no trailing VS-16 (U+FE0F). Appending a Fitzpatrick
    /// modifier after a variation selector yields an invalid sequence, so this
    /// guards future table edits from silently corrupting skin-toned output.
    #[test]
    fn skin_tone_bases_carry_no_variation_selector() {
        const VS16: char = '\u{FE0F}';
        for entry in TABLE.iter().filter(|e| e.skin_tone) {
            assert!(
                !entry.base.contains(VS16),
                "skin_tone:true base {:?} ({}) carries U+FE0F",
                entry.base,
                entry.shortcode
            );
            // The neutral glyph of a gendered entry is also fed through
            // `with_skin_tone` (see `render`), so it carries the same invariant.
            if let Some((neutral, _, _)) = entry.gendered {
                assert!(
                    !neutral.contains(VS16),
                    "skin_tone:true gendered-neutral base {:?} ({}) carries U+FE0F",
                    neutral,
                    entry.shortcode
                );
            }
        }
    }

    #[test]
    fn every_table_shortcode_resolves_to_a_glyph_by_exact_lookup() {
        // Whole-table parity: every TABLE entry's shortcode must resolve via an
        // exact `suggest(":<shortcode>")` to its own canonical shortcode and a
        // non-empty glyph. A typo'd key (or one shadowed by a shorter prefix
        // sibling) would pass the narrower spot-checks but fail here.
        for entry in TABLE.iter() {
            let s = suggest_default(&format!(":{}", entry.shortcode))
                .unwrap_or_else(|| panic!("shortcode {:?} did not resolve", entry.shortcode));
            assert_eq!(s.shortcode, entry.shortcode, "{:?}", entry.shortcode);
            assert!(
                !s.glyph.is_empty(),
                "{:?} resolved to empty glyph",
                entry.shortcode
            );
            // For plain entries (no gender variants, no skin-tone modifier),
            // `render` returns `entry.base` verbatim, so the resolved glyph must
            // equal the canonical base exactly — not merely be non-empty.
            if entry.gendered.is_none() && !entry.skin_tone {
                assert_eq!(
                    s.glyph, entry.base,
                    "plain entry {:?} must resolve to its base glyph",
                    entry.shortcode
                );
            }
        }
    }

    #[test]
    fn exact_shortcode_suggests_its_emoji() {
        let s = suggest_default("nice work :tada").unwrap();
        assert_eq!(s.shortcode, "tada");
        assert_eq!(s.glyph, "🎉");
        assert_eq!(s.replace_chars, 5); // ":tada"
    }

    #[test]
    fn prefix_matches_the_shortest_completion() {
        // ":smil" → "smile".
        let s = suggest_default("so :smil").unwrap();
        assert_eq!(s.shortcode, "smile");
        assert_eq!(s.glyph, "😄");
    }

    #[test]
    fn multibyte_char_after_colon_is_rejected_not_byte_sliced() {
        // `lookup` compares `token.len()` (BYTES) against MIN_PREFIX_LEN and
        // relies on the token being all-ASCII (is_shortcode_char enforces it).
        // A multibyte char after the colon must be rejected by the charset
        // gate, never reach the byte-length math, and never panic on a
        // non-boundary slice.
        assert_eq!(suggest_default(":é"), None);
        assert_eq!(suggest_default(":😄smile"), None);
    }

    #[test]
    fn aliases_resolve_to_the_same_glyph() {
        assert_eq!(suggest_default(":+1").unwrap().glyph, "👍");
        assert_eq!(suggest_default(":thumbsup").unwrap().glyph, "👍");
        assert_eq!(suggest_default(":-1").unwrap().glyph, "👎");
    }

    #[test]
    fn skin_tone_modifier_is_applied_to_people_emoji() {
        let prefs = EmojiPrefs {
            skin_tone: SkinTone::Medium,
            ..Default::default()
        };
        let s = suggest(":thumbsup", &prefs).unwrap();
        assert_eq!(s.glyph, format!("👍{}", '\u{1F3FD}'));
    }

    #[test]
    fn default_skin_tone_adds_no_modifier() {
        assert_eq!(suggest_default(":wave").unwrap().glyph, "👋");
    }

    #[test]
    fn skin_tone_does_not_affect_non_people_emoji() {
        let prefs = EmojiPrefs {
            skin_tone: SkinTone::Dark,
            ..Default::default()
        };
        // 🔥 doesn't support skin tone → unchanged.
        assert_eq!(suggest(":fire", &prefs).unwrap().glyph, "🔥");
    }

    #[test]
    fn gendered_emoji_resolve_by_preference() {
        let neutral = suggest(":shrug", &EmojiPrefs::default()).unwrap();
        assert_eq!(neutral.glyph, "🤷");
        let female = suggest(
            ":shrug",
            &EmojiPrefs {
                gender: Gender::Female,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(female.glyph, "🤷\u{200D}♀\u{FE0F}");
        let male = suggest(
            ":shrug",
            &EmojiPrefs {
                gender: Gender::Male,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(male.glyph, "🤷\u{200D}♂\u{FE0F}");
    }

    #[test]
    fn gendered_match_combines_skin_tone() {
        let prefs = EmojiPrefs {
            gender: Gender::Female,
            skin_tone: SkinTone::Dark,
        };
        // The Fitzpatrick modifier splices into the base of the ZWJ sequence.
        let s = suggest(":raising_hand", &prefs).unwrap();
        assert_eq!(s.glyph, "🙋\u{1F3FF}\u{200D}♀\u{FE0F}");
    }

    #[test]
    fn no_trigger_without_a_colon_token() {
        assert_eq!(suggest_default("just some words"), None);
        assert_eq!(suggest_default("trailing colon :"), None); // empty token
    }

    #[test]
    fn colon_must_start_or_follow_whitespace() {
        // ":" glued to a preceding word (URLs, ratios) must not trigger.
        assert_eq!(suggest_default("http:"), None);
        assert_eq!(suggest_default("word:smile"), None);
        assert_eq!(suggest_default("ratio:2"), None); // also: no entry "2"
    }

    #[test]
    fn unknown_shortcode_yields_no_suggestion() {
        assert_eq!(suggest_default(":zzzznotreal"), None);
    }

    #[test]
    fn colon_at_string_start_triggers() {
        let s = suggest_default(":rocket").unwrap();
        assert_eq!(s.glyph, "🚀");
        assert_eq!(s.replace_chars, 7); // ":rocket"
    }

    #[test]
    fn replace_chars_is_the_typed_length_not_the_matched_length() {
        // Typed ":roc" (4 chars) → matched "rocket" (6 chars). Replace only the 4
        // typed characters, not the canonical shortcode length.
        let s = suggest_default("blast off :roc").unwrap();
        assert_eq!(s.shortcode, "rocket");
        assert_eq!(s.replace_chars, 4);
    }

    #[test]
    fn each_skin_tone_appends_its_fitzpatrick_modifier() {
        let cases = [
            (SkinTone::Default, None),
            (SkinTone::Light, Some('\u{1F3FB}')),
            (SkinTone::MediumLight, Some('\u{1F3FC}')),
            (SkinTone::Medium, Some('\u{1F3FD}')),
            (SkinTone::MediumDark, Some('\u{1F3FE}')),
            (SkinTone::Dark, Some('\u{1F3FF}')),
        ];
        for (tone, modifier) in cases {
            let prefs = EmojiPrefs {
                skin_tone: tone,
                ..Default::default()
            };
            let expected = match modifier {
                Some(m) => format!("👋{m}"),
                None => "👋".to_string(),
            };
            assert_eq!(
                suggest(":wave", &prefs).unwrap().glyph,
                expected,
                "{tone:?}"
            );
        }
    }

    #[test]
    fn neutral_gender_combines_with_skin_tone() {
        // Skin tone IS applied to the neutral people-emoji variant.
        let prefs = EmojiPrefs {
            gender: Gender::Neutral,
            skin_tone: SkinTone::Medium,
        };
        assert_eq!(
            suggest(":raising_hand", &prefs).unwrap().glyph,
            format!("🙋{}", '\u{1F3FD}')
        );
    }

    #[test]
    fn multibyte_text_before_the_colon_is_handled() {
        let s = suggest_default("café :tada").unwrap();
        assert_eq!(s.shortcode, "tada");
        assert_eq!(s.replace_chars, 5); // ":tada", in characters
    }

    #[test]
    fn colon_after_emoji_or_punctuation_does_not_trigger() {
        assert_eq!(suggest_default("😀:smile"), None);
        assert_eq!(suggest_default("):smile"), None);
        assert_eq!(suggest_default("done):tada"), None);
    }

    #[test]
    fn prefix_tie_break_prefers_the_shortest_shortcode() {
        // ":thumbs" prefixes both thumbsup (8) and thumbsdown (10);
        // min_by_key is documented first-minimum, so the shorter shortcode
        // wins deterministically.
        assert_eq!(suggest_default(":thumbs").unwrap().shortcode, "thumbsup");
    }

    #[test]
    fn lookup_is_case_sensitive() {
        assert_eq!(suggest_default(":SMILE"), None);
        assert_eq!(suggest_default(":Tada"), None);
    }

    #[test]
    fn only_the_last_whitespace_anchored_colon_token_is_used() {
        // Last colon after a space → matches that token.
        assert_eq!(suggest_default(":smile :wave").unwrap().shortcode, "wave");
        // Last colon glued to the previous word → no trigger (no backtracking).
        assert_eq!(suggest_default(":smile:wave"), None);
    }

    #[test]
    fn single_character_prefix_does_not_trigger() {
        // One char after the colon is too eager for a prefix match...
        assert_eq!(suggest_default(":s"), None);
        assert_eq!(suggest_default(":+"), None);
        assert_eq!(suggest_default(":-"), None);
        // ...but a full exact shortcode still resolves.
        assert_eq!(suggest_default(":+1").unwrap().glyph, "👍");
        assert_eq!(suggest_default(":sm").unwrap().shortcode, "smile");
    }

    #[test]
    fn emoji_suggest_prefix_completes_short_and_digit_tokens() {
        // A two-char prefix and a digit-leading two-char prefix both meet
        // MIN_PREFIX_LEN (2) and complete to the shortest matching shortcode
        // (lookup, line ~226). ":10" is a prefix of "100" (💯) — the only digit
        // shortcode — and ":ok" is a prefix of "ok_hand" (👌).
        let ten = suggest_default(":10").unwrap();
        assert_eq!(ten.shortcode, "100");
        assert_eq!(ten.glyph, "💯");
        assert_eq!(ten.replace_chars, 3); // ":10"

        let ok = suggest_default(":ok").unwrap();
        assert_eq!(ok.shortcode, "ok_hand");
        assert_eq!(ok.glyph, "👌");
        assert_eq!(ok.replace_chars, 3); // ":ok"
    }

    #[test]
    fn gendered_entry_skin_tone_field_gates_the_neutral_modifier() {
        // raising_hand has skin_tone:true, so neutral + a tone applies the modifier
        // (the field is now meaningful on the gendered path, not dead code).
        let prefs = EmojiPrefs {
            gender: Gender::Neutral,
            skin_tone: SkinTone::Dark,
        };
        assert_eq!(
            suggest(":raising_hand", &prefs).unwrap().glyph,
            format!("🙋{}", '\u{1F3FF}')
        );
    }

    #[test]
    fn gendered_match_splices_skin_tone_into_the_zwj_sequence() {
        // Woman shrugging, medium tone: the Fitzpatrick modifier attaches to the
        // base person glyph (U+1F937), BEFORE the ZWJ — not after the whole
        // cluster. Exact codepoint order: person, tone, ZWJ, ♀, VS-16.
        let prefs = EmojiPrefs {
            gender: Gender::Female,
            skin_tone: SkinTone::Medium,
        };
        assert_eq!(
            suggest(":shrug", &prefs).unwrap().glyph,
            "\u{1F937}\u{1F3FD}\u{200D}\u{2640}\u{FE0F}"
        );
        // Man raising hand, dark tone: same splice point.
        let prefs = EmojiPrefs {
            gender: Gender::Male,
            skin_tone: SkinTone::Dark,
        };
        assert_eq!(
            suggest(":raising_hand", &prefs).unwrap().glyph,
            "\u{1F64B}\u{1F3FF}\u{200D}\u{2642}\u{FE0F}"
        );
    }
}
