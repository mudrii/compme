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
        let all_upper = word
            .chars()
            .filter(|c| c.is_alphabetic())
            .all(char::is_uppercase);
        let letter_count = word.chars().filter(|c| c.is_alphabetic()).count();
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
