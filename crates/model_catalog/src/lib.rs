//! The model catalog (engine-macos §15 D14, designed c95): which local
//! models the General pane offers, where they download from, and whether
//! they plausibly fit this machine.
//!
//! Deliberate deviation from the c95 sketch: the catalog is static Rust
//! data, not a TOML file — same in-repo content, no parser dependency, and
//! invalid entries become compile errors instead of runtime parse failures.
//!
//! Everything here is pure. Download/IO and the RAM probe (`sysctl`) are
//! later slices in other crates.

/// Per-model license class. `GemmaTerms` requires a click-through gate
/// before download (the catalog only labels it; the gate is UI work).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum License {
    Apache2,
    Mit,
    /// Google's Gemma Terms of Use — needs explicit acceptance.
    GemmaTerms,
    /// Meta's Llama Community License — needs explicit acceptance.
    LlamaCommunity,
}

impl License {
    /// Whether the user must accept terms before the first download.
    pub fn needs_acceptance(self) -> bool {
        matches!(self, License::GemmaTerms | License::LlamaCommunity)
    }
}

/// One downloadable model the General pane can offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelEntry {
    /// Display name (also the on-disk file stem).
    pub name: &'static str,
    /// Direct HTTPS download URL (Hugging Face resolve link).
    pub url: &'static str,
    /// Approximate download size, for the picker label.
    pub size_mb: u32,
    /// Advisory minimum unified memory for comfortable inference.
    pub min_ram_gb: u32,
    pub license: License,
}

/// How an entry relates to the machine's available memory. ADVISORY only
/// (D14): the picker labels `Tight`/`Exceeds`, it never blocks a download.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RamVerdict {
    /// Comfortable headroom.
    Fits,
    /// At or barely above the minimum — expect swapping under load.
    Tight,
    /// Below the model's minimum.
    Exceeds,
}

/// The built-in catalog, smallest first.
pub fn catalog() -> &'static [ModelEntry] {
    &[
        ModelEntry {
            name: "qwen2.5-0.5b-q4_k_m",
            url: "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/main/qwen2.5-0.5b-instruct-q4_k_m.gguf",
            size_mb: 398,
            min_ram_gb: 2,
            license: License::Apache2,
        },
        ModelEntry {
            name: "llama-3.2-1b-q4_k_m",
            url: "https://huggingface.co/bartowski/Llama-3.2-1B-Instruct-GGUF/resolve/main/Llama-3.2-1B-Instruct-Q4_K_M.gguf",
            size_mb: 808,
            min_ram_gb: 4,
            license: License::LlamaCommunity,
        },
        ModelEntry {
            name: "qwen2.5-1.5b-q4_k_m",
            url: "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf",
            size_mb: 986,
            min_ram_gb: 4,
            license: License::Apache2,
        },
        ModelEntry {
            name: "gemma-2-2b-q4_k_m",
            url: "https://huggingface.co/bartowski/gemma-2-2b-it-GGUF/resolve/main/gemma-2-2b-it-Q4_K_M.gguf",
            size_mb: 1708,
            min_ram_gb: 6,
            license: License::GemmaTerms,
        },
    ]
}

/// The one-click download target: the smallest catalog entry whose license
/// needs no click-through acceptance (the gate UI is a separate slice).
pub fn recommended() -> Option<&'static ModelEntry> {
    catalog()
        .iter()
        .find(|entry| !entry.license.needs_acceptance())
}

/// Advisory RAM fit: `Exceeds` below the minimum, `Tight` with less than
/// 2 GB of headroom over it, `Fits` otherwise.
pub fn ram_verdict(entry: &ModelEntry, available_gb: u32) -> RamVerdict {
    if available_gb < entry.min_ram_gb {
        RamVerdict::Exceeds
    } else if available_gb < entry.min_ram_gb.saturating_add(2) {
        RamVerdict::Tight
    } else {
        RamVerdict::Fits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommended_is_the_smallest_unencumbered_entry() {
        // The one-click download target (D14 wiring): smallest model whose
        // license needs no click-through — today the shipping default.
        let entry = recommended().expect("catalog has an unencumbered entry");
        assert_eq!(entry.name, "qwen2.5-0.5b-q4_k_m");
        assert!(!entry.license.needs_acceptance());
    }

    #[test]
    fn catalog_entries_are_well_formed_and_ordered() {
        let entries = catalog();
        assert!(!entries.is_empty());
        for e in entries {
            assert!(e.url.starts_with("https://"), "{}: non-https url", e.name);
            assert!(e.size_mb > 0, "{}: zero size", e.name);
            assert!(e.min_ram_gb > 0, "{}: zero min ram", e.name);
        }
        // Smallest first: the picker's default suggestion is the top entry.
        assert!(entries.windows(2).all(|w| w[0].size_mb <= w[1].size_mb));
        // The current shipping default must be in its own catalog.
        assert!(entries.iter().any(|e| e.name.contains("qwen2.5-0.5b")));
    }

    #[test]
    fn ram_verdict_is_advisory_with_a_2gb_tight_band() {
        let entry = ModelEntry {
            name: "test",
            url: "https://example.invalid/m.gguf",
            size_mb: 1000,
            min_ram_gb: 8,
            license: License::Apache2,
        };
        assert_eq!(ram_verdict(&entry, 7), RamVerdict::Exceeds);
        assert_eq!(ram_verdict(&entry, 8), RamVerdict::Tight);
        assert_eq!(ram_verdict(&entry, 9), RamVerdict::Tight);
        assert_eq!(ram_verdict(&entry, 10), RamVerdict::Fits);
    }

    #[test]
    fn gated_licenses_need_acceptance() {
        assert!(License::GemmaTerms.needs_acceptance());
        assert!(License::LlamaCommunity.needs_acceptance());
        assert!(!License::Apache2.needs_acceptance());
        assert!(!License::Mit.needs_acceptance());
    }
}
