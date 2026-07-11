//! Grammar/capitalization fixes, distinct from typo autocorrect. Where
//! [`autocorrect`](../autocorrect) is a high-precision typo table that never
//! alters a real word, this crate deliberately *does* fix one unambiguous
//! grammar case: the first-person pronoun **"i"** (and its common contractions)
//! is always written capital "I". Pure and OS-agnostic.
//!
//! Scope is intentionally narrow and unambiguous: only a bare leading lowercase
//! `i` that stands alone (`i`) or leads a known contraction (`i'm`, `i'll`,
//! `i've`, `i'd`, and the informal possessive `i's` as in "Jim and i's") is
//! capitalized. A word merely *starting* with `i` (`in`, `if`,
//! `it`, `idea`) is never touched, and an already-capital `I`/`I'm` returns
//! `None` (nothing to fix) so the host never offers a no-op replacement. The
//! host passes a single trailing word, so word boundaries are not this crate's
//! concern.

use textcase::CasePattern;

const MAX_EDIT_DISTANCE: usize = 2;

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
    // Standalone pronoun, or an `i`-leading contraction. `'s` covers the
    // informal possessive ("Jim and i's house"), where capitalizing the pronoun
    // is still the right fix even though the construction is nonstandard.
    // Normalize a curly apostrophe to straight only for the suffix match; the
    // original `rest` (apostrophe style and all) is preserved verbatim in the
    // output.
    let rest_key = rest.to_lowercase().replace('\u{2019}', "'");
    if rest.is_empty() || matches!(rest_key.as_str(), "'m" | "'ll" | "'ve" | "'d" | "'s") {
        return Some(format!("I{rest}"));
    }
    None
}

/// Vet a model-proposed single-word correction. The model is advisory only:
/// this filter rejects no-ops, multi-word output, non-ASCII words, and large
/// edits, then reapplies the original word's case pattern.
pub fn vet_correction(original: &str, model_output: &str) -> Option<String> {
    let original = original.trim();
    // The model is advisory: never salvage a plausible first token from a
    // runaway continuation. The safety contract requires the complete response
    // to contain exactly one whitespace-delimited token.
    let mut tokens = model_output.split_whitespace();
    let candidate = tokens
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| c.is_ascii_punctuation() && c != '\'');
    if original.is_empty()
        || candidate.is_empty()
        || tokens.next().is_some()
        || !original.is_ascii()
        || !candidate.is_ascii()
        || !is_ascii_word(original)
        || !is_ascii_word(candidate)
        || original.eq_ignore_ascii_case(candidate)
    {
        return None;
    }

    let original_lower = original.to_ascii_lowercase();
    let candidate_lower = candidate.to_ascii_lowercase();
    if capped_levenshtein(&original_lower, &candidate_lower, MAX_EDIT_DISTANCE) > MAX_EDIT_DISTANCE
    {
        return None;
    }

    Some(CasePattern::of(original).apply(&candidate_lower))
}

fn is_ascii_word(value: &str) -> bool {
    let mut has_alpha = false;
    let mut previous_apostrophe = false;
    for (idx, c) in value.chars().enumerate() {
        match c {
            '\'' if idx == 0 || previous_apostrophe => return false,
            '\'' => previous_apostrophe = true,
            c if c.is_ascii_alphabetic() => {
                has_alpha = true;
                previous_apostrophe = false;
            }
            _ => return false,
        }
    }
    has_alpha && !previous_apostrophe
}

// ponytail: capped at MAX_EDIT_DISTANCE, good enough for word-level typo distance.
fn capped_levenshtein(left: &str, right: &str, max: usize) -> usize {
    if left.len().abs_diff(right.len()) > max {
        return max + 1;
    }

    let mut prev: Vec<usize> = (0..=right.len()).collect();
    let mut curr = vec![0; right.len() + 1];
    for (i, lc) in left.bytes().enumerate() {
        curr[0] = i + 1;
        let mut row_min = curr[0];
        for (j, rc) in right.bytes().enumerate() {
            let substitution = prev[j] + usize::from(lc != rc);
            let insertion = curr[j] + 1;
            let deletion = prev[j + 1] + 1;
            curr[j + 1] = substitution.min(insertion).min(deletion);
            row_min = row_min.min(curr[j + 1]);
        }
        if row_min > max {
            return max + 1;
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[right.len()]
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

    #[test]
    fn vet_correction_accepts_one_edit_and_preserves_case() {
        assert_eq!(vet_correction("teh", "the").as_deref(), Some("the"));
        assert_eq!(vet_correction("Teh", "the").as_deref(), Some("The"));
        assert_eq!(vet_correction("TEH", "the").as_deref(), Some("THE"));
    }

    #[test]
    fn vet_correction_rejects_empty_identical_multi_word_large_edit_and_non_ascii() {
        for output in ["", "   ", "teh", "the cat", "kitten cat", "kitten"] {
            assert_eq!(vet_correction("teh", output), None, "{output:?}");
        }
        assert_eq!(vet_correction("日本", "本日"), None);
        assert_eq!(vet_correction("teh", "thé"), None);
    }

    #[test]
    fn vet_correction_rejects_runaway_output_instead_of_truncating_it() {
        assert_eq!(vet_correction("teh", " the. The correct"), None);
        assert_eq!(vet_correction("teh", "the\ncat"), None);
        // Surrounding punctuation on one token is still a single-word answer.
        assert_eq!(vet_correction("teh", "the,").as_deref(), Some("the"));
        assert_eq!(vet_correction("teh", "\"the\"").as_deref(), Some("the"));
    }

    #[test]
    fn vet_correction_rejects_case_only_difference() {
        // The `eq_ignore_ascii_case` guard (lib.rs L62): a case-only "fix" is a
        // no-op once the original's case pattern is reapplied, so it must be
        // rejected outright — the host never offers a replacement that changes
        // nothing.
        assert_eq!(vet_correction("Teh", "teh"), None);
        assert_eq!(vet_correction("TEH", "teh"), None);
    }

    #[test]
    fn vet_correction_rejects_non_word_tokens() {
        // Edge punctuation is stripped by extraction, but `is_ascii_word`
        // still gates INTERIOR punctuation and digits on either side: a
        // mid-word symbol or a digit in the original is not a single-word
        // correction, however small the edit distance.
        assert_eq!(vet_correction("teh", "th.e"), None);
        assert_eq!(vet_correction("b4", "by"), None);
    }

    #[test]
    fn vet_correction_accepts_apostrophe_contraction() {
        // `is_ascii_word` allows `'` on both sides, so a contraction target
        // survives vetting: one insertion, and the original's all-lowercase
        // pattern is reapplied verbatim.
        assert_eq!(vet_correction("dont", "don't").as_deref(), Some("don't"));
    }

    #[test]
    fn vet_correction_rejects_apostrophe_only_and_boundary_apostrophes() {
        for output in ["'", "''", "'the", "the'", "do''nt"] {
            assert_eq!(vet_correction("teh", output), None, "{output:?}");
        }
    }

    #[test]
    fn vet_correction_rejects_alot_to_a_lot_for_single_word_mode() {
        assert_eq!(vet_correction("alot", "a lot"), None);
    }

    #[test]
    fn vet_correction_pins_max_edit_distance_boundary() {
        // Same-length words so the length short-circuit in `capped_levenshtein`
        // never fires — this exercises the real DP distance against
        // MAX_EDIT_DISTANCE (2). "cat"->"cog" is exactly two substitutions
        // (accepted at the boundary); "cat"->"dog" is three (rejected one past
        // it). The `kitten` reject elsewhere only trips the length guard, so it
        // would pass even if MAX_EDIT_DISTANCE were mis-set — these two pin the
        // constant's exact value.
        assert_eq!(vet_correction("cat", "cog").as_deref(), Some("cog"));
        assert_eq!(vet_correction("cat", "dog"), None);
    }

    #[test]
    fn vet_correction_accepts_length_diff_equal_to_max_edit_distance() {
        // "cat"->"chart" has a length difference of exactly MAX_EDIT_DISTANCE
        // (2) and a true edit distance of 2: the length short-circuit
        // (`abs_diff > max`) must not fire at the boundary, and the DP must
        // still accept. Pins the `>` (not `>=`) in the short-circuit.
        assert_eq!(vet_correction("cat", "chart").as_deref(), Some("chart"));
    }

    #[test]
    fn vet_correction_rejects_past_max_when_lengths_differ() {
        // "cat"->"cogs" passes the length guard (diff 1) but its true edit
        // distance is 3 — the DP itself must reject it. Pins the insertion
        // cost term (`curr[j] + 1`) that the length guard cannot cover.
        assert_eq!(vet_correction("cat", "cogs"), None);
    }
}
