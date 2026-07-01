//! Grammar/capitalization fixes, distinct from typo autocorrect. Where
//! [`autocorrect`](../autocorrect) is a high-precision typo table that never
//! alters a real word, this crate deliberately *does* fix one unambiguous
//! grammar case: the first-person pronoun **"i"** (and its common contractions)
//! is always written capital "I". Pure and OS-agnostic.
//!
//! Scope is intentionally narrow and unambiguous: only a bare leading lowercase
//! `i` that stands alone (`i`) or leads a known contraction (`i'm`, `i'll`,
//! `i've`, `i'd`) is capitalized. A word merely *starting* with `i` (`in`, `if`,
//! `it`, `idea`) is never touched, and an already-capital `I`/`I'm` returns
//! `None` (nothing to fix) so the host never offers a no-op replacement. The
//! host passes a single trailing word, so word boundaries are not this crate's
//! concern.

/// Capitalize the first-person pronoun. Returns the corrected word if `word` is
/// the standalone lowercase pronoun `i` or a known `i`-leading contraction that
/// needs its leading letter capitalized; `None` otherwise (already capital, or
/// not the pronoun).
///
/// ```
/// assert_eq!(grammar::capitalize_pronoun("i").as_deref(), Some("I"));
/// assert_eq!(grammar::capitalize_pronoun("i'm").as_deref(), Some("I'm"));
/// assert_eq!(grammar::capitalize_pronoun("I"), None); // already correct
/// assert_eq!(grammar::capitalize_pronoun("in"), None); // not the pronoun
/// ```
pub fn capitalize_pronoun(word: &str) -> Option<String> {
    let w = word.trim();
    let mut chars = w.chars();
    // The leading letter must be a lowercase ASCII `i`; anything else (an
    // already-capital `I`, or a different word) has nothing to fix.
    if chars.next() != Some('i') {
        return None;
    }
    let rest: String = chars.collect();
    // Standalone pronoun, or an `i`-leading contraction. Normalize a curly
    // apostrophe to straight only for the suffix match; the original `rest`
    // (apostrophe style and all) is preserved verbatim in the output.
    let rest_key = rest.to_lowercase().replace('\u{2019}', "'");
    if rest.is_empty() || matches!(rest_key.as_str(), "'m" | "'ll" | "'ve" | "'d" | "'s") {
        return Some(format!("I{rest}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_lowercase_i_becomes_capital() {
        assert_eq!(capitalize_pronoun("i").as_deref(), Some("I"));
        assert_eq!(capitalize_pronoun("  i  ").as_deref(), Some("I")); // trims
    }

    #[test]
    fn i_leading_contractions_capitalize_only_the_leading_letter() {
        assert_eq!(capitalize_pronoun("i'm").as_deref(), Some("I'm"));
        assert_eq!(capitalize_pronoun("i'll").as_deref(), Some("I'll"));
        assert_eq!(capitalize_pronoun("i've").as_deref(), Some("I've"));
        assert_eq!(capitalize_pronoun("i'd").as_deref(), Some("I'd"));
        assert_eq!(capitalize_pronoun("i's").as_deref(), Some("I's"));
    }

    #[test]
    fn curly_apostrophe_contraction_is_preserved_verbatim() {
        // The suffix matches via a normalized straight apostrophe, but the curly
        // apostrophe survives in the output.
        assert_eq!(
            capitalize_pronoun("i\u{2019}m").as_deref(),
            Some("I\u{2019}m")
        );
    }

    #[test]
    fn already_capital_pronoun_is_left_alone() {
        // No no-op replacement offered.
        assert_eq!(capitalize_pronoun("I"), None);
        assert_eq!(capitalize_pronoun("I'm"), None);
    }

    #[test]
    fn words_that_merely_start_with_i_are_never_touched() {
        for word in ["in", "if", "it", "is", "idea", "iron", "island", "into"] {
            assert_eq!(capitalize_pronoun(word), None, "{word}");
        }
    }

    #[test]
    fn empty_and_non_i_words_return_none() {
        assert_eq!(capitalize_pronoun(""), None);
        assert_eq!(capitalize_pronoun("   "), None);
        assert_eq!(capitalize_pronoun("hello"), None);
        assert_eq!(capitalize_pronoun("the"), None);
    }

    #[test]
    fn applying_twice_is_stable() {
        // The output ("I", "I'm", …) leads with a capital, so a second pass is a
        // no-op — no A→B→A loop when the host re-scans a corrected word.
        for word in ["i", "i'm", "i'll", "i've", "i'd"] {
            let once = capitalize_pronoun(word).expect("should capitalize once");
            assert_eq!(capitalize_pronoun(&once), None, "{word} not idempotent");
        }
    }

    #[test]
    fn multibyte_i_lookalikes_do_not_match_and_do_not_panic() {
        // A dotless/Turkish or accented i is not ASCII `i`, so no false capitalize
        // and no byte-slice panic on the non-ASCII leading scalar.
        for word in ["\u{131}", "\u{ec}", "\u{12b}", "\u{456}"] {
            assert_eq!(capitalize_pronoun(word), None, "{word}");
        }
    }
}
