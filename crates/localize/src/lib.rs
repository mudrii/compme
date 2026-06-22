//! British-English normalization (design spec §16: "British English", Cotypist
//! 0.22 "Cotypist Labs"). Pure and OS-agnostic: a curated US→UK spelling table
//! maps an American form to its British equivalent, with the query's
//! capitalization reapplied.
//!
//! Like [`autocorrect`](../autocorrect), this is **high-precision, low-recall**.
//! Every key is a form that is American-only — it is *not* itself a valid
//! British spelling — so a word that is already British (or shared by both
//! dialects) is never altered, and there are no false positives on correct
//! input. Genuinely ambiguous forms that are valid British words in some sense
//! (`meter` the SI unit, `tire` to fatigue, `check` the verb, `license` the
//! verb, `practice` the noun, `program` in computing, `draft`) are deliberately
//! excluded: changing them could corrupt correct text.
//!
//! It maps whole words only; stripping surrounding punctuation and deciding
//! *when* to apply (a host toggle, `COMPME_BRITISH_ENGLISH`, off by
//! default) are the host's job, mirroring the `autocorrect`/`thesaurus` split.

use textcase::CasePattern;

/// `(american, british)` — all lowercase. Each key is American-only (never a
/// valid British spelling), so a correctly-spelled British or shared word is
/// never altered. Common inflections are listed explicitly because lookup is
/// whole-word.
const US_TO_UK: &[(&str, &str)] = &[
    // -or → -our
    ("color", "colour"),
    ("colors", "colours"),
    ("colored", "coloured"),
    ("coloring", "colouring"),
    ("colorful", "colourful"),
    ("honor", "honour"),
    ("honors", "honours"),
    ("honored", "honoured"),
    ("honoring", "honouring"),
    ("honorable", "honourable"),
    ("favor", "favour"),
    ("favors", "favours"),
    ("favored", "favoured"),
    ("favoring", "favouring"),
    ("favorite", "favourite"),
    ("favorites", "favourites"),
    ("favorable", "favourable"),
    ("behavior", "behaviour"),
    ("behaviors", "behaviours"),
    ("neighbor", "neighbour"),
    ("neighbors", "neighbours"),
    ("neighborhood", "neighbourhood"),
    ("labor", "labour"),
    ("labored", "laboured"),
    ("flavor", "flavour"),
    ("flavors", "flavours"),
    ("flavored", "flavoured"),
    ("rumor", "rumour"),
    ("rumors", "rumours"),
    ("harbor", "harbour"),
    ("harbors", "harbours"),
    // -ize → -ise (British -ise convention)
    ("organize", "organise"),
    ("organized", "organised"),
    ("organizes", "organises"),
    ("organizing", "organising"),
    ("organization", "organisation"),
    ("organizations", "organisations"),
    ("realize", "realise"),
    ("realized", "realised"),
    ("realizes", "realises"),
    ("realizing", "realising"),
    ("recognize", "recognise"),
    ("recognized", "recognised"),
    ("recognizes", "recognises"),
    ("recognizing", "recognising"),
    ("apologize", "apologise"),
    ("apologized", "apologised"),
    ("apologizing", "apologising"),
    ("criticize", "criticise"),
    ("criticized", "criticised"),
    ("emphasize", "emphasise"),
    ("emphasized", "emphasised"),
    ("summarize", "summarise"),
    ("summarized", "summarised"),
    ("prioritize", "prioritise"),
    ("prioritized", "prioritised"),
    // -yze → -yse
    ("analyze", "analyse"),
    ("analyzed", "analysed"),
    ("analyzes", "analyses"),
    ("analyzing", "analysing"),
    ("paralyze", "paralyse"),
    ("paralyzed", "paralysed"),
    // -er → -re
    ("center", "centre"),
    ("centers", "centres"),
    ("centered", "centred"),
    ("theater", "theatre"),
    ("theaters", "theatres"),
    ("liter", "litre"),
    ("liters", "litres"),
    ("fiber", "fibre"),
    ("fibers", "fibres"),
    // -se → -ce
    ("defense", "defence"),
    ("offense", "offence"),
    // doubled-L before suffix
    ("traveler", "traveller"),
    ("travelers", "travellers"),
    ("traveled", "travelled"),
    ("traveling", "travelling"),
    ("canceled", "cancelled"),
    ("canceling", "cancelling"),
    ("modeled", "modelled"),
    ("modeling", "modelling"),
    ("labeled", "labelled"),
    ("labeling", "labelling"),
    // -og → -ogue
    ("catalog", "catalogue"),
    ("catalogs", "catalogues"),
    ("dialog", "dialogue"),
    ("dialogs", "dialogues"),
    // NB: `analog` is deliberately NOT mapped — it is standard British English in
    // electronics/computing (the domain this app sees most), like the excluded
    // `program`. Mapping it would corrupt correct British technical text.
    // miscellaneous unambiguous American-only forms
    ("jewelry", "jewellery"),
    ("aluminum", "aluminium"),
    ("plow", "plough"),
    ("pajamas", "pyjamas"),
    ("skeptical", "sceptical"),
    ("skeptic", "sceptic"),
    ("maneuver", "manoeuvre"),
    // NB: `mold`/`artifact` are deliberately NOT mapped — both are accepted
    // British spellings (Collins lists `mold`; the OED records `artifact`), and
    // `mold` also has the British "leaf mold" soil sense with no `mould` form.
    // Normalizing them could rewrite correct British text.
];

/// The British spelling for `word` if it is a known American-only form, with the
/// query's capitalization applied; `None` for a word that is already British,
/// shared, or unknown.
pub fn to_british(word: &str) -> Option<String> {
    let key = word.trim().to_lowercase();
    if key.is_empty() {
        return None;
    }
    let british = US_TO_UK
        .iter()
        .find(|(us, _)| *us == key)
        .map(|(_, uk)| *uk)?;
    Some(CasePattern::of(word.trim()).apply(british))
}

/// Whether `word` is a known American-only form with a British equivalent (cheap
/// gate for British-English triggering). Predicate↔table parity:
/// `is_americanism(w) == true` iff `to_british(w)` is `Some`.
pub fn is_americanism(word: &str) -> bool {
    let key = word.trim().to_lowercase();
    !key.is_empty() && US_TO_UK.iter().any(|(us, _)| *us == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn maps_a_known_americanism() {
        assert_eq!(to_british("color").as_deref(), Some("colour"));
        assert_eq!(to_british("organize").as_deref(), Some("organise"));
        assert_eq!(to_british("center").as_deref(), Some("centre"));
        assert_eq!(to_british("analyze").as_deref(), Some("analyse"));
    }

    #[test]
    fn mixed_case_query_classifies_by_first_letter_then_drops_internal_caps() {
        // The lookup lowercases the key, then re-applies the query's CasePattern
        // (which reads only the FIRST letter). So a camelCase query resolves to
        // the all-lowercase British spelling — the internal capital is not
        // preserved. Acceptable, now pinned so a CasePattern change can't
        // silently alter it; a Title-cased query keeps its leading capital.
        assert_eq!(to_british("coLor").as_deref(), Some("colour"));
        assert_eq!(to_british("Color").as_deref(), Some("Colour"));
    }

    #[test]
    fn preserves_query_capitalization() {
        assert_eq!(to_british("Color").as_deref(), Some("Colour"));
        assert_eq!(to_british("COLOR").as_deref(), Some("COLOUR"));
        assert_eq!(to_british("color").as_deref(), Some("colour"));
        // Title-case multi-syllable
        assert_eq!(to_british("Organization").as_deref(), Some("Organisation"));
    }

    #[test]
    fn already_british_or_shared_word_is_not_changed() {
        for word in [
            "colour",
            "centre",
            "organise",
            "analyse",
            "behaviour",
            "the",
            "hello",
            "running",
        ] {
            assert_eq!(to_british(word), None, "{word}");
            assert!(!is_americanism(word), "{word}");
        }
    }

    #[test]
    fn deliberately_excluded_ambiguous_words_never_change() {
        // These are valid British words in some sense; mapping them could corrupt
        // correct text, so the table omits them. Pin the contract.
        for word in [
            "meter",
            "tire",
            "check",
            "license",
            "practice",
            "program",
            "draft",
            "story",
            "ton",
            "gray", // accepted-British / shared forms removed after review (false-positive risk):
            "analog",
            "mold",
            "molds",
            "artifact",
            "artifacts",
        ] {
            assert_eq!(to_british(word), None, "{word}");
            assert!(!is_americanism(word), "{word}");
        }
    }

    #[test]
    fn lookup_is_case_insensitive_and_trims() {
        assert!(is_americanism("COLOR"));
        assert!(is_americanism("  color  "));
        assert_eq!(to_british("  Color  ").as_deref(), Some("Colour"));
    }

    #[test]
    fn multibyte_first_char_query_is_handled_without_panic() {
        // `to_british` lowercases the query and looks it up, then re-applies the
        // query's CasePattern (which reads the first char) to the British value.
        // Every table key is ASCII, so a word whose first character is multibyte
        // can never match — it must return None, never panic on the non-ASCII
        // first-char path (no byte slicing into the codepoint).
        assert_eq!(to_british("Élan"), None);
        assert_eq!(to_british("élan"), None);
        assert_eq!(to_british("Ünder"), None);
        assert!(!is_americanism("Élan"));
        // A trailing-multibyte query is likewise a clean miss (lowercasing keeps
        // the multibyte tail, so it never equals an ASCII key).
        assert_eq!(to_british("coloré"), None);
    }

    #[test]
    fn empty_input_is_none() {
        assert_eq!(to_british(""), None);
        assert_eq!(to_british("   "), None);
        assert!(!is_americanism(""));
    }

    #[test]
    fn trailing_punctuation_is_not_a_known_form() {
        // `to_british` matches a bare word; punctuation stripping is the host's
        // job. Pin the contract so a future change is deliberate.
        assert_eq!(to_british("color."), None);
        assert_eq!(to_british("color,"), None);
    }

    #[test]
    fn predicate_matches_table_parity() {
        // is_americanism(w) is Some-iff for every table key (and a sample miss).
        for (us, _) in US_TO_UK {
            assert!(is_americanism(us), "{us}");
            assert!(to_british(us).is_some(), "{us}");
        }
        assert!(!is_americanism("colour"));
        assert!(to_british("colour").is_none());
    }

    #[test]
    fn every_americanism_maps_to_its_exact_value() {
        // Round-trips the whole table — a corrupted British value would pass the
        // narrower spot-checks but fail here.
        for (us, uk) in US_TO_UK {
            assert_eq!(to_british(us).as_deref(), Some(*uk), "{us}");
        }
    }

    #[test]
    fn british_values_are_not_themselves_americanisms_so_applying_twice_is_stable() {
        // Idempotence: normalizing a British value must be a no-op (no A→B→C
        // loops, and no British form that is itself an American key).
        for (_, uk) in US_TO_UK {
            assert_eq!(
                to_british(uk),
                None,
                "british value {uk:?} is itself an americanism key"
            );
        }
    }

    #[test]
    fn keys_are_unique_and_never_map_to_themselves() {
        let mut seen = HashSet::new();
        for (us, uk) in US_TO_UK {
            assert_ne!(us, uk, "{us} maps to itself");
            assert!(seen.insert(*us), "duplicate americanism key in table: {us}");
        }
    }
}
