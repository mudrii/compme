//! Capitalization pattern detection + application, shared by the text-suggestion
//! crates (`thesaurus`, `autocorrect`) so a replacement word/phrase can carry the
//! same case the user typed. Pure and OS-agnostic.

/// How a word was capitalized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CasePattern {
    /// all lowercase, or no cased letters.
    Lower,
    /// First letter uppercase, rest as-is.
    Title,
    /// All letters uppercase (only when there is more than one letter — a single
    /// capital letter is `Title`, not `Upper`).
    Upper,
}

impl CasePattern {
    /// Classify the capitalization of `word`, ignoring non-alphabetic characters.
    pub fn of(word: &str) -> CasePattern {
        let mut letters = word.chars().filter(|c| c.is_alphabetic());
        let Some(first) = letters.next() else {
            return CasePattern::Lower;
        };
        // Single pass over the remaining letters (first already consumed).
        let mut letter_count = 1usize;
        let mut all_upper = first.is_uppercase();
        for c in letters {
            letter_count += 1;
            all_upper &= c.is_uppercase();
        }
        if all_upper && letter_count > 1 {
            return CasePattern::Upper;
        }
        if first.is_uppercase() {
            CasePattern::Title
        } else {
            CasePattern::Lower
        }
    }

    /// Apply this pattern to `text`. `Title` capitalizes only the first character
    /// (multi-byte safe); `Upper` uppercases everything; `Lower` is a pass-through
    /// (callers store replacements lowercase).
    pub fn apply(self, text: &str) -> String {
        match self {
            CasePattern::Lower => text.to_string(),
            CasePattern::Upper => text.to_uppercase(),
            CasePattern::Title => {
                let mut chars = text.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            }
        }
    }
}

/// Look `word` up case-insensitively in a lowercase `(from, to)` table and
/// return the mapped text re-cased to match how `word` was typed (surrounding
/// whitespace ignored). `None` for a blank word or no entry. Shared by the
/// single-answer replacement lookups (`autocorrect::correct`,
/// `localize::to_british`).
pub fn lookup_preserving_case(word: &str, table: &[(&str, &str)]) -> Option<String> {
    let key = word.trim().to_lowercase();
    if key.is_empty() {
        return None;
    }
    let mapped = table
        .iter()
        .find_map(|(from, to)| (*from == key).then_some(*to))?;
    Some(CasePattern::of(word.trim()).apply(mapped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_preserving_case_matches_case_insensitively_and_reapplies_typed_case() {
        let table: &[(&str, &str)] = &[("teh", "the"), ("color", "colour")];
        assert_eq!(lookup_preserving_case("teh", table).as_deref(), Some("the"));
        assert_eq!(
            lookup_preserving_case(" Teh ", table).as_deref(),
            Some("The")
        );
        assert_eq!(
            lookup_preserving_case("COLOR", table).as_deref(),
            Some("COLOUR")
        );
        assert_eq!(lookup_preserving_case("hello", table), None);
        assert_eq!(lookup_preserving_case("   ", table), None);
    }

    #[test]
    fn classifies_lower_title_upper() {
        assert_eq!(CasePattern::of("hi"), CasePattern::Lower);
        assert_eq!(CasePattern::of("Hi"), CasePattern::Title);
        assert_eq!(CasePattern::of("HI"), CasePattern::Upper);
    }

    #[test]
    fn single_capital_letter_is_title_not_upper() {
        assert_eq!(CasePattern::of("I"), CasePattern::Title);
    }

    #[test]
    fn mixed_case_keys_on_first_letter() {
        assert_eq!(CasePattern::of("BiG"), CasePattern::Title);
        assert_eq!(CasePattern::of("bIg"), CasePattern::Lower);
        assert_eq!(CasePattern::of("ABc"), CasePattern::Title); // not all-upper
    }

    #[test]
    fn classifies_camelcase_by_first_letter_only() {
        // An internal capital never makes a word Title/Upper — only the FIRST
        // cased letter decides (all-upper is the multi-letter exception). A
        // camelCase token (first-lower) is Lower; a Mc/Mac proper noun
        // (first-upper, mixed rest) is Title.
        assert_eq!(CasePattern::of("iPhone"), CasePattern::Lower);
        assert_eq!(CasePattern::of("eBay"), CasePattern::Lower);
        assert_eq!(CasePattern::of("McDonald"), CasePattern::Title);
    }

    #[test]
    fn ignores_non_alphabetic_chars() {
        assert_eq!(CasePattern::of("don't"), CasePattern::Lower);
        assert_eq!(CasePattern::of("Well-Being"), CasePattern::Title);
        assert_eq!(CasePattern::of("WELL-BEING"), CasePattern::Upper);
        assert_eq!(CasePattern::of("42"), CasePattern::Lower); // no letters
        assert_eq!(CasePattern::of(""), CasePattern::Lower);
    }

    #[test]
    fn classifies_multibyte_accented_strings() {
        // `of` reasons over Unicode scalars, not bytes. An accented all-caps word
        // ("ÉLAN", É is 2 UTF-8 bytes) must classify as Upper (all letters
        // uppercase, >1 letter), and a multibyte mixed string by its first letter.
        assert_eq!(CasePattern::of("ÉLAN"), CasePattern::Upper);
        assert_eq!(CasePattern::of("Élan"), CasePattern::Title); // first upper, rest lower
        assert_eq!(CasePattern::of("élan"), CasePattern::Lower);
        // A single accented capital is Title, not Upper (the >1-letter rule),
        // mirroring the ASCII `"I"` case.
        assert_eq!(CasePattern::of("É"), CasePattern::Title);
        // CJK has no case → no cased letters → Lower (matches the "42" rule).
        assert_eq!(CasePattern::of("日本"), CasePattern::Lower);
    }

    #[test]
    fn apply_each_pattern() {
        assert_eq!(CasePattern::Lower.apply("Enormous"), "Enormous");
        assert_eq!(CasePattern::Title.apply("enormous"), "Enormous");
        assert_eq!(CasePattern::Upper.apply("enormous"), "ENORMOUS");
    }

    #[test]
    fn title_apply_handles_multibyte_first_char_and_phrases() {
        assert_eq!(CasePattern::Title.apply("élan"), "Élan");
        // A multi-word replacement only capitalizes the first character.
        assert_eq!(CasePattern::Title.apply("a lot"), "A lot");
        assert_eq!(CasePattern::Upper.apply("a lot"), "A LOT");
    }

    #[test]
    fn apply_handles_empty_and_multibyte_uppercasing() {
        assert_eq!(CasePattern::Title.apply(""), ""); // empty branch, no panic
        assert_eq!(CasePattern::Upper.apply("élan"), "ÉLAN");
        assert_eq!(CasePattern::Lower.apply(""), "");
    }

    #[test]
    fn lower_apply_is_passthrough_on_cased_input() {
        // `Lower` intentionally does NOT lowercase — callers already store
        // replacements lowercase, so `apply` is a pure pass-through. Pin this on
        // CASED input (the existing tests only feed already-lowercase strings,
        // which would pass even if `Lower` mistakenly called `to_lowercase`).
        assert_eq!(CasePattern::Lower.apply("MixedCase"), "MixedCase");
        assert_eq!(CasePattern::Lower.apply("ÉLAN"), "ÉLAN");
    }

    #[test]
    fn title_apply_handles_one_to_many_uppercase() {
        // `'ß'.to_uppercase()` yields TWO chars ("SS"), so the first-char
        // uppercasing in `Title` must collect a string, not assume one char.
        // The `collect::<String>()` in the impl is load-bearing here.
        assert_eq!(CasePattern::Title.apply("ßeta"), "SSeta");
    }

    #[test]
    fn handles_combining_marks_nfd() {
        // Decomposed (NFD) "élan" = 'e' + U+0301 COMBINING ACUTE ACCENT. The
        // first scalar is a plain lowercase 'e', so classification is Lower and
        // Title only uppercases the base 'e', leaving the combining mark intact.
        assert_eq!(CasePattern::of("e\u{0301}lan"), CasePattern::Lower);
        assert_eq!(CasePattern::Title.apply("e\u{0301}lan"), "E\u{0301}lan");
    }

    #[test]
    fn emoji_is_caseless_and_apply_is_safe() {
        // An emoji has no cased letters -> Lower (matches the "42"/CJK rule), and
        // Title.apply must not panic on a non-alphabetic leading scalar: 👍 has no
        // uppercase mapping, so it passes through unchanged.
        assert_eq!(CasePattern::of("👍"), CasePattern::Lower);
        assert_eq!(CasePattern::Title.apply("👍ok"), "👍ok");
    }
}
