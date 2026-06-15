//! Thesaurus / synonym suggestion (design spec §8 / §16). Pure and OS-agnostic:
//! look a word up in a curated synonym table and return the alternatives, with
//! the queried word's capitalization pattern applied so a host can drop a
//! replacement straight in. The host gates this on the "thesaurus" toggle and
//! decides between *selection* mode (user highlights a word → offer synonyms) and
//! *auto* mode (offer as the user types); the lookup itself is identical.
//!
//! Mirrors Cotypist's `featureThesaurus{AutoMode,SelectionMode}`.

/// Curated synonym groups. Every word in a group is interchangeable; looking up
/// any member returns the others. All stored lowercase; case is reapplied from
/// the query. Kept small and high-signal.
const GROUPS: &[&[&str]] = &[
    &[
        "happy",
        "glad",
        "joyful",
        "cheerful",
        "content",
        "pleased",
        "delighted",
    ],
    &["sad", "unhappy", "down", "gloomy"],
    &["big", "large", "huge", "enormous", "massive"],
    &["small", "little", "tiny", "compact", "minor"],
    &["fast", "quick", "rapid", "swift", "speedy"],
    &["slow", "sluggish", "gradual", "unhurried"],
    &["good", "great", "excellent", "fine", "superb"],
    &["bad", "poor", "terrible", "awful", "dreadful"],
    &["important", "crucial", "vital", "essential", "key"],
    &["smart", "clever", "intelligent", "bright", "sharp"],
    &["bright", "luminous", "radiant", "vivid", "sharp"],
    &["begin", "start", "commence", "initiate"],
    &["end", "finish", "conclude", "complete", "wrap"],
    &["show", "display", "demonstrate", "reveal", "present"],
    &["help", "assist", "aid", "support"],
    &["make", "create", "build", "produce", "form"],
    &["use", "utilize", "employ", "apply"],
    &["idea", "concept", "notion", "thought"],
    &["problem", "issue", "trouble", "difficulty"],
];

use textcase::CasePattern;

/// Synonyms for `word`, with its capitalization applied and the word itself
/// excluded. Case-insensitive lookup. Empty when the word is unknown.
///
/// A word can appear in more than one group (different senses); all alternatives
/// across matching groups are returned, de-duplicated, preserving table order.
pub fn synonyms(word: &str) -> Vec<String> {
    let key = word.trim().to_lowercase();
    if key.is_empty() {
        return Vec::new();
    }
    let pattern = CasePattern::of(word.trim());
    let mut out: Vec<String> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for group in GROUPS {
        if group.contains(&key.as_str()) {
            for &syn in *group {
                if syn != key && !seen.contains(&syn) {
                    seen.push(syn);
                    out.push(pattern.apply(syn));
                }
            }
        }
    }
    out
}

/// Whether the word has any synonym (for cheaply gating an auto-mode trigger).
pub fn has_synonyms(word: &str) -> bool {
    let key = word.trim().to_lowercase();
    !key.is_empty() && GROUPS.iter().any(|g| g.contains(&key.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_other_group_members_excluding_the_word() {
        assert_eq!(
            synonyms("big"),
            vec!["large", "huge", "enormous", "massive"]
        );
    }

    #[test]
    fn lookup_is_case_insensitive_and_preserves_query_case() {
        assert_eq!(synonyms("Fast")[0], "Quick"); // Title case
        assert_eq!(synonyms("FAST")[0], "QUICK"); // all-caps
        assert_eq!(synonyms("fast")[0], "quick"); // lower
    }

    #[test]
    fn unknown_word_has_no_synonyms() {
        assert!(synonyms("xylophone").is_empty());
        assert!(!has_synonyms("xylophone"));
    }

    #[test]
    fn has_synonyms_reports_membership() {
        assert!(has_synonyms("good"));
        assert!(has_synonyms("GOOD"));
        assert!(!has_synonyms(""));
        assert!(!has_synonyms("   "));
    }

    #[test]
    fn synonyms_within_a_group_are_mutually_interchangeable() {
        // The "interchangeable" invariant: any member returns all the others, so
        // a query for a non-head member still surfaces the whole group.
        let from_glad = synonyms("glad");
        assert!(from_glad.contains(&"pleased".to_string()));
        assert!(from_glad.contains(&"happy".to_string()));
        assert!(!from_glad.contains(&"glad".to_string())); // self excluded
                                                           // No duplicates anywhere in the result.
        let mut sorted = from_glad.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), from_glad.len());
    }

    #[test]
    fn whitespace_is_trimmed() {
        assert_eq!(synonyms("  big  "), synonyms("big"));
    }

    #[test]
    fn empty_and_whitespace_yield_nothing() {
        assert!(synonyms("").is_empty());
        assert!(synonyms("   ").is_empty());
    }

    #[test]
    fn query_case_is_applied_to_synonyms() {
        // Title-case and lowercase queries; CasePattern itself is unit-tested in
        // the `textcase` crate, here we pin the synonyms-level behavior.
        assert_eq!(synonyms("BiG")[0], "Large");
        assert_eq!(synonyms("bIg")[0], "large");
    }

    #[test]
    fn case_is_applied_to_every_returned_synonym() {
        // Not just index 0 — the whole vector carries the query's case.
        assert_eq!(synonyms("FAST"), vec!["QUICK", "RAPID", "SWIFT", "SPEEDY"]);
        assert_eq!(synonyms("Fast"), vec!["Quick", "Rapid", "Swift", "Speedy"]);
    }

    #[test]
    fn punctuated_unknown_word_finds_nothing_without_panic() {
        assert!(synonyms("don't").is_empty());
    }

    #[test]
    fn member_query_returns_the_whole_group_in_table_order() {
        // ("happy" lives in exactly ONE group — the no-word-in-two-groups
        // invariant below pins that — so this is the single-group order
        // contract, not a multi-group dedup exercise.)
        assert_eq!(
            synonyms("happy"),
            vec![
                "glad",
                "joyful",
                "cheerful",
                "content",
                "pleased",
                "delighted"
            ]
        );
    }

    #[test]
    fn multi_sense_query_merges_matching_groups_and_dedupes() {
        assert_eq!(
            synonyms("bright"),
            vec![
                "smart",
                "clever",
                "intelligent",
                "sharp",
                "luminous",
                "radiant",
                "vivid",
            ]
        );
    }

    #[test]
    fn every_table_word_has_at_least_one_synonym() {
        // Guards against introducing a degenerate <2-member group, which would
        // make has_synonyms() true but synonyms() empty.
        for group in GROUPS {
            for &word in *group {
                assert!(has_synonyms(word), "{word}");
                assert!(!synonyms(word).is_empty(), "{word}");
            }
        }
    }

    #[test]
    fn multi_word_query_finds_nothing() {
        assert!(synonyms("big dog").is_empty());
    }

    #[test]
    fn only_intentional_multi_sense_words_appear_in_more_than_one_group() {
        // Multi-sense words keep the merge/dedup behavior load-bearing without
        // allowing accidental table overlap to silently alter suggestions.
        use std::collections::HashMap;
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for group in GROUPS {
            for &word in *group {
                *counts.entry(word).or_insert(0) += 1;
            }
        }
        let mut dupes: Vec<_> = counts
            .iter()
            .filter_map(|(&word, &count)| (count > 1).then_some((word, count)))
            .collect();
        dupes.sort_unstable_by_key(|&(word, _)| word);
        assert_eq!(dupes, vec![("bright", 2), ("sharp", 2)]);
    }

    #[test]
    fn excludes_the_queried_word_even_in_a_different_case() {
        let syns = synonyms("BIG");
        assert!(!syns.contains(&"BIG".to_string()));
        assert!(!syns.contains(&"big".to_string()));
        assert_eq!(syns[0], "LARGE");
    }
}
