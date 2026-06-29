//! Typo correction (design spec §8 / §16: "typo fix distinct from full
//! autocorrect"). Pure and OS-agnostic: a curated common-misspelling table maps a
//! typo to its correction, with the query's capitalization reapplied.
//!
//! This is the **typo-fix / suggested-fix** half of the §16 gate — a high-
//! precision, low-recall table of unambiguous misspellings (a real word is never
//! "corrected", so there are no false positives on valid input). It deliberately
//! excludes ambiguous strings that are also real words (e.g. `cant`, `wont`,
//! `weve`). Full statistical autocorrect (NSSpellChecker-equivalent) is a separate
//! toggle and a platform concern. The "no false-correct in code fields" §16
//! requirement is a host gate (don't run in code editors); this core only proposes
//! a correction for a known typo and never touches anything else.

use textcase::CasePattern;

/// `(misspelling, correction)` — all lowercase. Only unambiguous typos: each key
/// is NOT itself a valid English word, so a correct word is never altered.
const TYPOS: &[(&str, &str)] = &[
    ("teh", "the"),
    ("recieve", "receive"),
    ("seperate", "separate"),
    ("definately", "definitely"),
    ("occured", "occurred"),
    ("untill", "until"),
    ("wich", "which"),
    ("thier", "their"),
    ("becuase", "because"),
    ("accross", "across"),
    ("beleive", "believe"),
    ("freind", "friend"),
    ("goverment", "government"),
    ("neccessary", "necessary"),
    ("occassion", "occasion"),
    ("persistant", "persistent"),
    ("tommorow", "tomorrow"),
    ("wierd", "weird"),
    ("adress", "address"),
    ("arguement", "argument"),
    ("embarass", "embarrass"),
    ("enviroment", "environment"),
    ("existance", "existence"),
    ("grammer", "grammar"),
    ("independant", "independent"),
    ("occurance", "occurrence"),
    ("priviledge", "privilege"),
    ("recomend", "recommend"),
    ("succesful", "successful"),
    ("truely", "truly"),
    ("usefull", "useful"),
    ("alot", "a lot"),
];

/// The correction for `word` if it is a known typo, with the query's
/// capitalization applied; `None` for a correctly-spelled (or unknown) word.
pub fn correct(word: &str) -> Option<String> {
    let key = word.trim().to_lowercase();
    if key.is_empty() {
        return None;
    }
    let correction = TYPOS
        .iter()
        .find_map(|(typo, fix)| (*typo == key).then_some(*fix))?;
    Some(CasePattern::of(word.trim()).apply(correction))
}

/// Whether `word` is a known typo (cheap gate for auto/typo-fix triggering).
pub fn is_typo(word: &str) -> bool {
    let key = word.trim().to_lowercase();
    !key.is_empty() && TYPOS.iter().any(|(typo, _)| *typo == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrects_a_known_typo() {
        assert_eq!(correct("teh").as_deref(), Some("the"));
        assert_eq!(correct("recieve").as_deref(), Some("receive"));
    }

    #[test]
    fn preserves_query_capitalization() {
        assert_eq!(correct("Teh").as_deref(), Some("The"));
        assert_eq!(correct("TEH").as_deref(), Some("THE"));
        assert_eq!(correct("teh").as_deref(), Some("the"));
    }

    #[test]
    fn multi_word_correction_keeps_case() {
        assert_eq!(correct("alot").as_deref(), Some("a lot"));
        assert_eq!(correct("Alot").as_deref(), Some("A lot"));
        assert_eq!(correct("ALOT").as_deref(), Some("A LOT"));
    }

    #[test]
    fn correctly_spelled_word_is_not_corrected() {
        assert_eq!(correct("the"), None);
        assert_eq!(correct("receive"), None);
        assert_eq!(correct("hello"), None);
        assert!(!is_typo("the"));
    }

    #[test]
    fn lookup_is_case_insensitive_and_trims() {
        assert!(is_typo("TEH"));
        assert!(is_typo("  teh  "));
        assert_eq!(correct("  Teh  ").as_deref(), Some("The"));
    }

    #[test]
    fn empty_input_is_none() {
        assert_eq!(correct(""), None);
        assert_eq!(correct("   "), None);
        assert!(!is_typo(""));
    }

    #[test]
    fn corrections_are_not_themselves_typos_so_applying_twice_is_stable() {
        // Idempotence: correcting a correction must be a no-op (no A→B→C loops,
        // and no correction that is itself a typo key).
        for (_, correction) in TYPOS {
            assert_eq!(
                correct(correction),
                None,
                "correction {correction:?} is itself a typo key"
            );
        }
    }

    #[test]
    fn every_typo_corrects_to_its_exact_value() {
        // Round-trips the whole table — a corrupted correction value would pass
        // the narrower spot-checks but fail here.
        for (typo, fix) in TYPOS {
            assert_eq!(correct(typo).as_deref(), Some(*fix), "{typo}");
        }
    }

    #[test]
    fn deliberately_excluded_ambiguous_real_words_never_correct() {
        // The high-precision contract: real words (incl. the ambiguous ones the
        // table deliberately omits) must never be altered.
        for word in [
            "cant", "wont", "weve", "its", "were", "the", "calender", "address",
        ] {
            assert_eq!(correct(word), None, "{word}");
            assert!(!is_typo(word), "{word}");
        }
    }

    #[test]
    fn trailing_punctuation_is_not_a_known_typo() {
        // `correct` matches a bare word; stripping punctuation is the host's job.
        // Pin the contract so a future change is deliberate.
        assert_eq!(correct("teh."), None);
        assert_eq!(correct("teh,"), None);
    }

    #[test]
    fn no_typo_key_maps_to_itself_and_keys_are_unique() {
        let mut keys: Vec<&str> = TYPOS.iter().map(|(k, _)| *k).collect();
        for (typo, fix) in TYPOS {
            assert_ne!(typo, fix, "{typo} maps to itself");
        }
        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), count, "duplicate typo key in table");
    }

    #[test]
    fn correcting_twice_equals_correcting_once() {
        // True idempotence: feed every correction OUTPUT back into `correct` and
        // require `None` — across the lower/Title/UPPER case variants of each
        // typo, so a correction that is itself a typo key (or whose cased form
        // is) is caught. The existing idempotency test only checked the raw
        // (lowercase) correction values, never the actual round-tripped output.
        for (typo, _) in TYPOS {
            for variant in [
                typo.to_string(),
                CasePattern::Title.apply(typo),
                typo.to_uppercase(),
            ] {
                let once = correct(&variant);
                assert!(once.is_some(), "{variant} should correct once");
                assert_eq!(
                    once.as_deref().and_then(correct),
                    None,
                    "correcting the output of {variant:?} ({once:?}) must be a no-op"
                );
            }
        }
        // Spot checks called out explicitly: a Title typo and the multi-word
        // "a lot" correction (which is itself never a typo key).
        assert_eq!(correct("Teh").and_then(|s| correct(&s)), None);
        assert_eq!(correct("a lot"), None);
    }

    #[test]
    fn mixed_case_query_is_handled_deterministically() {
        // Mixed-case path: `CasePattern::of` keys on the FIRST cased letter, so a
        // first-lower mixed query is Lower (pass-through correction) and a
        // first-upper mixed query is Title. Pin this on length-changing
        // corrections ("recieve"->"receive", "alot"->"a lot") where the case
        // reapplication interacts with the value length.
        assert_eq!(correct("tEh").as_deref(), Some("the")); // first lower -> Lower
        assert_eq!(correct("ReCieve").as_deref(), Some("Receive")); // first upper -> Title
        assert_eq!(correct("aLot").as_deref(), Some("a lot")); // first lower -> Lower
    }
}
