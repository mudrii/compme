//! The model catalog (engine-macos §15 D14, designed c95): which local
//! models the Setup pane offers, where they download from, and whether
//! this machine can offer them for download.
//!
//! Deliberate deviation from the c95 sketch: the catalog is static Rust
//! data, not a TOML file — same in-repo content, no parser dependency, and
//! invalid entries become compile errors instead of runtime parse failures.
//!
//! Everything here is pure. Download/IO and the platform RAM probe are
//! later slices in other crates.

/// Per-model license class. `GemmaTerms`/`LlamaCommunity` require a
/// click-through gate before download — [`download_gate`] is the pure
/// decision; the prompt UI is the host's half.
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

    /// Human-readable license name for the click-through prompt.
    pub fn display_name(self) -> &'static str {
        match self {
            License::Apache2 => "Apache License 2.0",
            License::Mit => "MIT License",
            License::GemmaTerms => "Gemma Terms of Use",
            License::LlamaCommunity => "Llama Community License",
        }
    }

    /// Canonical terms URL. Total (every variant has one) — unencumbered
    /// licenses never reach the prompt, but a total fn needs no Option
    /// handling at the call site.
    pub fn terms_url(self) -> &'static str {
        match self {
            License::Apache2 => "https://www.apache.org/licenses/LICENSE-2.0",
            License::Mit => "https://opensource.org/license/mit",
            License::GemmaTerms => "https://ai.google.dev/gemma/terms",
            License::LlamaCommunity => "https://www.llama.com/llama3_2/license/",
        }
    }
}

/// Outcome of the pre-download license gate (D14; c95 "once per model").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadGate {
    /// Unencumbered license, or terms already accepted for this model.
    Proceed,
    /// Click-through required before the first download of THIS model.
    NeedsLicense {
        model: &'static str,
        license_name: &'static str,
        terms_url: &'static str,
    },
}

/// Pure gate decision: prompt only when the entry's license needs
/// acceptance AND this model's name is not in the caller's accepted set
/// (per-MODEL, not per-license-class — a new Gemma-family model re-prompts).
/// The host owns the accepted set (`COMPME_LICENSE_ACCEPTED`) and the
/// prompt; every download path MUST route through this gate or it silently
/// bypasses the license terms.
pub fn download_gate(entry: &ModelEntry, is_accepted: impl Fn(&str) -> bool) -> DownloadGate {
    if entry.license.needs_acceptance() && !is_accepted(entry.name) {
        DownloadGate::NeedsLicense {
            model: entry.name,
            license_name: entry.license.display_name(),
            terms_url: entry.license.terms_url(),
        }
    } else {
        DownloadGate::Proceed
    }
}

/// Whether `hash` is a well-formed pinned SHA-256: exactly 64 lowercase hex
/// chars. The LENGTH is load-bearing — a truncated digest can never equal a
/// real 64-char hash, so it would mean a permanent `HashMismatch` on every
/// download attempt. Lowercase is an authoring convention (runtime comparison
/// is case-insensitive). The authoring-time catalog invariant for any
/// `expected_sha256` that is `Some`.
pub fn is_wellformed_sha256(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// One downloadable model the Setup pane can offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelEntry {
    /// Display name (also the on-disk file stem).
    pub name: &'static str,
    /// Direct HTTPS download URL (Hugging Face resolve link).
    pub url: &'static str,
    /// Approximate download size, for the picker label.
    pub size_mb: u32,
    /// Minimum unified memory required before the download is offered.
    pub min_ram_gb: u32,
    pub license: License,
    /// Pinned SHA-256 of the model file (64 hex chars; the runtime
    /// comparison is case-insensitive — model_fetch lowercases the expected
    /// side — lowercase here is authoring convention, test-enforced), fed
    /// to model_fetch's verify-before-rename. Catalog entries are all
    /// user-downloadable, so release builds keep every entry pinned.
    pub expected_sha256: Option<&'static str>,
}

/// Provenance for a pinned catalog model artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelProvenance {
    pub name: &'static str,
    pub url: &'static str,
    /// Hugging Face repository commit observed on the resolve redirect.
    pub hf_repo_commit: &'static str,
    /// Hugging Face LFS linked ETag. For these GGUF artifacts this is the file
    /// SHA-256 and must match [`ModelEntry::expected_sha256`].
    pub hf_x_linked_etag: &'static str,
}

/// How an entry relates to the machine's available memory. `Exceeds` is a hard
/// download gate; `Tight`/`Fits` are user-facing picker labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RamVerdict {
    /// Comfortable headroom.
    Fits,
    /// At or barely above the minimum — expect swapping under load.
    Tight,
    /// Below the model's minimum.
    Exceeds,
}

impl RamVerdict {
    /// Short user-facing word for the picker label (the suffix on a catalog row,
    /// e.g. "… · tight — may swap under load").
    pub fn advice(self) -> &'static str {
        match self {
            RamVerdict::Fits => "fits",
            RamVerdict::Tight => "tight \u{2014} may swap under load",
            RamVerdict::Exceeds => "exceeds available memory",
        }
    }
}

/// The built-in catalog, smallest first.
pub fn catalog() -> &'static [ModelEntry] {
    &[
        ModelEntry {
            name: "qwen2.5-0.5b-q4_k_m",
            url: "https://huggingface.co/Brianpuz/Qwen2.5-0.5B-Q4_K_M-GGUF/resolve/2188f0ce52503bd130dee9abf56f36f610784c0e/qwen2.5-0.5b-q4_k_m.gguf",
            size_mb: 398,
            min_ram_gb: 2,
            license: License::Apache2,
            expected_sha256: Some(
                "ca6f8885c1d6a14025e705295fe1b240ad5a30c4c696215a341d7e6610a26484",
            ),
        },
        ModelEntry {
            name: "llama-3.2-1b-q4_k_m",
            url: "https://huggingface.co/bartowski/Llama-3.2-1B-Instruct-GGUF/resolve/067b946cf014b7c697f3654f621d577a3e3afd1c/Llama-3.2-1B-Instruct-Q4_K_M.gguf",
            size_mb: 808,
            min_ram_gb: 4,
            license: License::LlamaCommunity,
            expected_sha256: Some(
                "6f85a640a97cf2bf5b8e764087b1e83da0fdb51d7c9fab7d0fece9385611df83",
            ),
        },
        ModelEntry {
            name: "qwen2.5-1.5b-q4_k_m",
            url: "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/91cad51170dc346986eccefdc2dd33a9da36ead9/qwen2.5-1.5b-instruct-q4_k_m.gguf",
            // Pinned file is 1,117,320,736 bytes; size_mb feeds the download
            // max_bytes cap (size_mb * MiB), so undercounting rejects the fetch.
            size_mb: 1118,
            min_ram_gb: 4,
            license: License::Apache2,
            expected_sha256: Some(
                "6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e",
            ),
        },
        ModelEntry {
            name: "gemma-2-2b-q4_k_m",
            url: "https://huggingface.co/bartowski/gemma-2-2b-it-GGUF/resolve/855f67caed130e1befc571b52bd181be2e858883/gemma-2-2b-it-Q4_K_M.gguf",
            size_mb: 1708,
            min_ram_gb: 6,
            license: License::GemmaTerms,
            expected_sha256: Some(
                "e0aee85060f168f0f2d8473d7ea41ce2f3230c1bc1374847505ea599288a7787",
            ),
        },
    ]
}

/// Committed provenance for the built-in catalog hashes.
pub fn catalog_provenance() -> &'static [ModelProvenance] {
    &[
        ModelProvenance {
            name: "qwen2.5-0.5b-q4_k_m",
            url: "https://huggingface.co/Brianpuz/Qwen2.5-0.5B-Q4_K_M-GGUF/resolve/2188f0ce52503bd130dee9abf56f36f610784c0e/qwen2.5-0.5b-q4_k_m.gguf",
            hf_repo_commit: "2188f0ce52503bd130dee9abf56f36f610784c0e",
            hf_x_linked_etag:
                "ca6f8885c1d6a14025e705295fe1b240ad5a30c4c696215a341d7e6610a26484",
        },
        ModelProvenance {
            name: "llama-3.2-1b-q4_k_m",
            url: "https://huggingface.co/bartowski/Llama-3.2-1B-Instruct-GGUF/resolve/067b946cf014b7c697f3654f621d577a3e3afd1c/Llama-3.2-1B-Instruct-Q4_K_M.gguf",
            hf_repo_commit: "067b946cf014b7c697f3654f621d577a3e3afd1c",
            hf_x_linked_etag:
                "6f85a640a97cf2bf5b8e764087b1e83da0fdb51d7c9fab7d0fece9385611df83",
        },
        ModelProvenance {
            name: "qwen2.5-1.5b-q4_k_m",
            url: "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/91cad51170dc346986eccefdc2dd33a9da36ead9/qwen2.5-1.5b-instruct-q4_k_m.gguf",
            hf_repo_commit: "91cad51170dc346986eccefdc2dd33a9da36ead9",
            hf_x_linked_etag:
                "6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e",
        },
        ModelProvenance {
            name: "gemma-2-2b-q4_k_m",
            url: "https://huggingface.co/bartowski/gemma-2-2b-it-GGUF/resolve/855f67caed130e1befc571b52bd181be2e858883/gemma-2-2b-it-Q4_K_M.gguf",
            hf_repo_commit: "855f67caed130e1befc571b52bd181be2e858883",
            hf_x_linked_etag:
                "e0aee85060f168f0f2d8473d7ea41ce2f3230c1bc1374847505ea599288a7787",
        },
    ]
}

/// The one-click download target: the smallest catalog entry whose license
/// needs no click-through acceptance (the gate UI is a separate slice).
pub fn recommended() -> Option<&'static ModelEntry> {
    recommended_in(catalog())
}

/// The smallest unencumbered entry in `entries`, by `size_mb`. Selecting by
/// size (not list position) makes the choice correct-by-construction: the
/// catalog no longer has to be authored smallest-first for `recommended` to
/// pick the smallest. Ties resolve to the first such entry (`min_by_key` is
/// stable), and a fully-gated list yields `None`.
fn recommended_in(entries: &[ModelEntry]) -> Option<&ModelEntry> {
    entries
        .iter()
        .filter(|entry| !entry.license.needs_acceptance())
        .min_by_key(|entry| entry.size_mb)
}

/// RAM fit label: `Exceeds` below the minimum, `Tight` with less than
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

/// Hardware gate for model downloads. Tight models are allowed with a warning;
/// models below their minimum are not offered for download on this machine.
pub fn offerable_by_ram(entry: &ModelEntry, available_gb: u32) -> bool {
    ram_verdict(entry, available_gb) != RamVerdict::Exceeds
}

/// Whole gibibytes of physical memory, floored, from a raw byte count (what the
/// RAM probe — `NSProcessInfo.physicalMemory` / `hw.memsize` — reports). Floors
/// rather than rounds so a machine just under a threshold is never flattered
/// into a better `ram_verdict`; saturates at `u32::MAX` rather than wrapping.
pub fn bytes_to_whole_gb(bytes: u64) -> u32 {
    const GIB: u64 = 1024 * 1024 * 1024;
    u32::try_from(bytes / GIB).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell_assignment<'a>(script: &'a str, name: &str) -> Option<&'a str> {
        let prefix = format!("{name}=\"");
        let value = script
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix(&prefix)?.strip_suffix('"'))?;
        if let Some(default) = value
            .strip_prefix("${")
            .and_then(|v| v.strip_suffix('}'))
            .and_then(|v| v.split_once(":-"))
            .map(|(_, default)| default)
        {
            Some(default)
        } else {
            Some(value)
        }
    }

    fn make(name: &'static str, size_mb: u32, license: License) -> ModelEntry {
        ModelEntry {
            name,
            url: "https://example.invalid/m.gguf",
            size_mb,
            min_ram_gb: 8,
            license,
            expected_sha256: None,
        }
    }

    #[test]
    fn recommended_is_the_smallest_unencumbered_entry() {
        // The one-click download target (D14 wiring): smallest model whose
        // license needs no click-through — today the shipping default.
        let entry = recommended().expect("catalog has an unencumbered entry");
        assert_eq!(entry.name, "qwen2.5-0.5b-q4_k_m");
        assert!(!entry.license.needs_acceptance());
    }

    #[test]
    fn recommended_is_unencumbered_and_the_globally_smallest_such() {
        // Ordering-independent restatement of recommended()'s contract against
        // the REAL catalog: the returned entry needs no acceptance AND no other
        // unencumbered entry is smaller. (The skip-gated-and-pick-smallest logic
        // is isolated against a crafted list in `recommended_in_*` below; this
        // guards the wired-up `recommended()` over the shipping data.)
        let entry = recommended().expect("catalog has an unencumbered entry");
        assert!(
            !entry.license.needs_acceptance(),
            "recommended() must skip gated entries"
        );
        let smallest_unencumbered = catalog()
            .iter()
            .filter(|e| !e.license.needs_acceptance())
            .min_by_key(|e| e.size_mb)
            .expect("at least one unencumbered entry");
        assert_eq!(
            entry.size_mb, smallest_unencumbered.size_mb,
            "recommended() must be the smallest unencumbered entry by size"
        );
        // And no GATED entry is smaller than the pick that was skipped over —
        // i.e. if a smaller entry exists, it is only because it is gated.
        // (Guards the skip-gated branch: a smaller gated entry must NOT win.)
        for e in catalog().iter().filter(|e| e.size_mb < entry.size_mb) {
            assert!(
                e.license.needs_acceptance(),
                "{}: a smaller entry than recommended() must be gated, else recommended() picked wrong",
                e.name
            );
        }
    }

    #[test]
    fn release_catalog_rows_have_download_and_policy_metadata() {
        let catalog = catalog();
        assert!(catalog.len() >= 4);
        let mut names = std::collections::BTreeSet::new();

        for entry in catalog {
            assert!(names.insert(entry.name));
            assert!(!entry.name.trim().is_empty());
            assert!(entry.url.starts_with("https://"));
            assert!(entry.size_mb > 0);
            assert!(entry.min_ram_gb > 0);
        }
        let ram_profiles = catalog
            .iter()
            .map(|entry| entry.min_ram_gb)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(ram_profiles.len() >= 2);
    }

    #[test]
    fn size_mb_download_cap_admits_every_pinned_artifact() {
        // Regression pin for 18fbc4f: the download path caps transfers at
        // `size_mb * MiB` (run_loop's catalog_download_request), so an
        // undercounted size_mb makes that entry's download always fail with
        // SizeExceeded. Real byte sizes below are the upstream Content-Length
        // of each pinned resolve URL (checked 2026-07-10); a new or re-pinned
        // entry must extend this table or the length assert fails.
        let real_bytes: &[(&str, u64)] = &[
            ("qwen2.5-0.5b-q4_k_m", 397_807_520),
            ("llama-3.2-1b-q4_k_m", 807_694_464),
            ("qwen2.5-1.5b-q4_k_m", 1_117_320_736),
            ("gemma-2-2b-q4_k_m", 1_708_582_752),
        ];
        assert_eq!(real_bytes.len(), catalog().len());
        for (name, bytes) in real_bytes {
            let entry = catalog().iter().find(|e| e.name == *name).unwrap();
            assert!(
                u64::from(entry.size_mb) * 1024 * 1024 >= *bytes,
                "{name}: size_mb {} caps the download below the real {bytes}-byte file",
                entry.size_mb
            );
        }
    }

    #[test]
    fn recommended_in_skips_a_smaller_gated_entry_and_picks_the_smallest_unencumbered() {
        // Isolated, order-independent proof of the selection logic: a SMALLER
        // gated entry sits FIRST, so a `.find(first unencumbered)` would still
        // be correct here only by luck of ordering — but a smaller gated entry
        // placed first must NOT win, and the smallest UNENCUMBERED entry must,
        // wherever it sits in the list.
        let entries = [
            make("tiny-but-gated", 100, License::LlamaCommunity),
            make("big-open", 900, License::Apache2),
            make("small-open", 300, License::Mit),
            make("smaller-but-gated", 50, License::GemmaTerms),
        ];
        let pick = recommended_in(&entries).expect("an unencumbered entry exists");
        assert_eq!(pick.name, "small-open");
        assert_eq!(pick.size_mb, 300);
        assert!(!pick.license.needs_acceptance());

        // A fully-gated list yields None.
        let all_gated = [
            make("a", 100, License::GemmaTerms),
            make("b", 50, License::LlamaCommunity),
        ];
        assert!(recommended_in(&all_gated).is_none());
    }

    #[test]
    fn recommended_in_breaks_size_ties_to_the_first_listed_entry() {
        // Documented contract (min_by_key is stable): when two unencumbered
        // entries share the smallest size_mb, the FIRST-listed wins. This fixes
        // which model becomes the one-click default download; an unstable
        // selection picking the LAST equal entry would silently change the
        // default yet pass the all-distinct-size selection test above.
        let entries = [
            make("first-open", 300, License::Apache2),
            make("second-open", 300, License::Mit),
            make("big-open", 900, License::Apache2),
        ];
        let pick = recommended_in(&entries).expect("an unencumbered entry exists");
        assert_eq!(pick.name, "first-open");
        assert_eq!(pick.size_mb, 300);
    }

    #[test]
    fn catalog_entries_are_well_formed_and_ordered() {
        let entries = catalog();
        assert!(!entries.is_empty());
        for e in entries {
            assert!(e.url.starts_with("https://"), "{}: non-https url", e.name);
            assert!(e.size_mb > 0, "{}: zero size", e.name);
            assert!(e.min_ram_gb > 0, "{}: zero min ram", e.name);
            // Names serialize comma-joined into COMPME_LICENSE_ACCEPTED and
            // double as on-disk file stems — keep them strict slugs. A comma
            // would re-parse as two bogus names and re-prompt forever.
            assert!(
                e.name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
                "{}: name must be a [-_.a-z0-9] slug",
                e.name
            );
        }
        // Smallest first: the picker's default suggestion is the top entry.
        assert!(entries.windows(2).all(|w| w[0].size_mb <= w[1].size_mb));
        // The current shipping default must be in its own catalog.
        assert!(entries.iter().any(|e| e.name.contains("qwen2.5-0.5b")));
    }

    #[test]
    fn ram_verdict_uses_a_2gb_tight_band() {
        let entry = ModelEntry {
            name: "test",
            url: "https://example.invalid/m.gguf",
            size_mb: 1000,
            min_ram_gb: 8,
            license: License::Apache2,
            expected_sha256: None,
        };
        assert_eq!(ram_verdict(&entry, 7), RamVerdict::Exceeds);
        assert_eq!(ram_verdict(&entry, 8), RamVerdict::Tight);
        assert_eq!(ram_verdict(&entry, 9), RamVerdict::Tight);
        assert_eq!(ram_verdict(&entry, 10), RamVerdict::Fits);
    }

    #[test]
    fn offerable_by_ram_blocks_only_models_below_their_minimum() {
        let entry = ModelEntry {
            name: "test",
            url: "https://example.invalid/m.gguf",
            size_mb: 1000,
            min_ram_gb: 8,
            license: License::Apache2,
            expected_sha256: None,
        };
        assert!(!offerable_by_ram(&entry, 7));
        assert!(offerable_by_ram(&entry, 8), "tight is allowed");
        assert!(offerable_by_ram(&entry, 10), "fits is allowed");
    }

    #[test]
    fn bytes_to_whole_gb_floors_and_saturates() {
        const GIB: u64 = 1024 * 1024 * 1024;
        // The RAM probe (NSProcessInfo.physicalMemory) hands over raw bytes;
        // ram_verdict wants whole GB. Floor, never round up (so a machine just
        // under a threshold is not flattered into a better verdict).
        assert_eq!(bytes_to_whole_gb(0), 0);
        assert_eq!(
            bytes_to_whole_gb(GIB - 1),
            0,
            "just under 1 GiB floors to 0"
        );
        assert_eq!(bytes_to_whole_gb(GIB), 1);
        assert_eq!(bytes_to_whole_gb(8 * GIB), 8, "an 8 GB machine reports 8");
        assert_eq!(
            bytes_to_whole_gb(16 * GIB + GIB / 2),
            16,
            "the partial GiB is floored off"
        );
        assert_eq!(
            bytes_to_whole_gb(u64::MAX),
            u32::MAX,
            "an absurd byte count saturates, never wraps"
        );
    }

    #[test]
    fn ram_verdict_advice_gives_a_distinct_word_per_state() {
        assert_eq!(RamVerdict::Fits.advice(), "fits");
        assert_eq!(
            RamVerdict::Tight.advice(),
            "tight \u{2014} may swap under load"
        );
        assert_eq!(RamVerdict::Exceeds.advice(), "exceeds available memory");
        // The three labels must be distinct (a collision would make
        // Tight and Exceeds indistinguishable in the picker).
        let all = [
            RamVerdict::Fits.advice(),
            RamVerdict::Tight.advice(),
            RamVerdict::Exceeds.advice(),
        ];
        let unique: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(unique.len(), 3, "each verdict needs a distinct label");
    }

    #[test]
    fn all_catalog_hashes_are_pinned_lowercase_sha256_hex() {
        // expected_sha256 feeds model_fetch's verify-before-rename. The
        // LENGTH check is load-bearing: a truncated hash can never equal a
        // 64-char digest → permanent HashMismatch on every download. The
        // lowercase check is authoring convention only (runtime comparison
        // is case-insensitive). Every catalog entry is user-downloadable, so
        // a missing hash would silently opt that entry out of verification.
        for e in catalog() {
            let hash = e
                .expected_sha256
                .unwrap_or_else(|| panic!("{}: missing pinned hash", e.name));
            assert!(
                is_wellformed_sha256(hash),
                "{}: malformed pinned hash",
                e.name
            );
        }
    }

    #[test]
    fn catalog_hashes_match_recorded_upstream_provenance() {
        let provenance_by_name: std::collections::HashMap<_, _> =
            catalog_provenance().iter().map(|p| (p.name, p)).collect();
        assert_eq!(
            provenance_by_name.len(),
            catalog().len(),
            "provenance should cover every catalog entry"
        );

        for entry in catalog() {
            let provenance = provenance_by_name
                .get(entry.name)
                .unwrap_or_else(|| panic!("{}: missing provenance", entry.name));
            assert_eq!(
                provenance.url, entry.url,
                "{}: provenance URL drift",
                entry.name
            );
            assert!(
                is_wellformed_sha256(provenance.hf_x_linked_etag),
                "{}: malformed provenance etag",
                entry.name
            );
            assert_eq!(
                entry.expected_sha256,
                Some(provenance.hf_x_linked_etag),
                "{}: pinned hash must match recorded upstream LFS etag",
                entry.name
            );
        }
    }

    #[test]
    fn catalog_urls_are_pinned_to_recorded_repo_commits() {
        let provenance_by_name: std::collections::HashMap<_, _> =
            catalog_provenance().iter().map(|p| (p.name, p)).collect();

        for entry in catalog() {
            let provenance = provenance_by_name
                .get(entry.name)
                .unwrap_or_else(|| panic!("{}: missing provenance", entry.name));
            let commit_path = format!("/resolve/{}/", provenance.hf_repo_commit);
            assert!(
                entry.url.contains(&commit_path),
                "{}: catalog URL must pin the recorded Hugging Face commit",
                entry.name
            );
            assert!(
                provenance.url.contains(&commit_path),
                "{}: provenance URL must pin the recorded Hugging Face commit",
                entry.name
            );
            assert!(
                !entry.url.contains("/resolve/main/") && !provenance.url.contains("/resolve/main/"),
                "{}: catalog URLs must not track mutable resolve/main",
                entry.name
            );
        }
    }

    #[test]
    fn model_backed_release_gate_uses_catalog_recommended_artifact() {
        let script = include_str!("../../../tools/release/run-model-gates.sh");
        let recommended = recommended().expect("catalog has a recommended model");
        let model_path =
            shell_assignment(script, "default_model").expect("default model assignment");
        let url = shell_assignment(script, "default_url").expect("default URL assignment");
        assert_eq!(
            model_path.rsplit('/').next(),
            url.rsplit('/').next(),
            "release model path basename must match the downloaded GGUF basename"
        );
        assert_eq!(
            Some(url),
            Some(recommended.url),
            "release model gate must download the catalog recommended artifact"
        );
        assert_eq!(
            shell_assignment(script, "default_expected"),
            recommended.expected_sha256,
            "release model gate hash must match the catalog recommended artifact"
        );
    }

    #[test]
    fn wellformed_sha256_enforces_length_and_lowercase_hex() {
        // The invariant the catalog lint relies on, tested independent of
        // catalog data. 64 lowercase hex = valid; a truncated, over-long,
        // uppercase, or non-hex string is rejected.
        let valid = "a".repeat(64);
        assert!(is_wellformed_sha256(&valid));
        assert!(is_wellformed_sha256(&"0123456789abcdef".repeat(4))); // 64 hex
        assert!(
            !is_wellformed_sha256(&"a".repeat(63)),
            "truncated must fail"
        );
        assert!(
            !is_wellformed_sha256(&"a".repeat(65)),
            "over-long must fail"
        );
        assert!(
            !is_wellformed_sha256(&"A".repeat(64)),
            "uppercase must fail"
        );
        assert!(!is_wellformed_sha256(&"g".repeat(64)), "non-hex must fail");
        assert!(!is_wellformed_sha256(""), "empty must fail");
    }

    #[test]
    fn catalog_names_are_unique() {
        // Names double as on-disk file stems and COMPME_LICENSE_ACCEPTED keys;
        // a duplicate would collide silently (one model's accept unlocks the
        // other, or two downloads clobber one file).
        let names: Vec<&str> = catalog().iter().map(|e| e.name).collect();
        let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
        assert_eq!(names.len(), unique.len(), "catalog names must be unique");
    }

    #[test]
    fn download_gate_requires_exact_name_acceptance_not_a_prefix() {
        // download_gate passes the FULL entry.name to is_accepted; a
        // prefix/substring "accept" must NOT unlock a gated model (the
        // accepted-set is matched by exact name — a loose match would wrongly
        // unlock a sibling, defeating the license click-through).
        let llama = catalog()
            .iter()
            .find(|e| e.license == License::LlamaCommunity)
            .expect("llama entry");
        let prefix = &llama.name[..llama.name.len() - 1];
        assert!(
            matches!(
                download_gate(llama, |n| n == prefix),
                DownloadGate::NeedsLicense { .. }
            ),
            "a prefix-only acceptance must not unlock a gated model"
        );
        assert_eq!(
            download_gate(llama, |n| n == llama.name),
            DownloadGate::Proceed,
            "the exact name must unlock it"
        );
    }

    /// The 40-lowercase-hex commit pinned between `/resolve/` and the next `/`
    /// in a Hugging Face resolve URL, if present.
    fn resolve_commit_segment(url: &str) -> Option<&str> {
        let after = url.split_once("/resolve/")?.1;
        Some(after.split('/').next().unwrap_or(after))
    }

    #[test]
    fn catalog_urls_pin_a_full_40_hex_commit() {
        // Every download/provenance URL must pin a full 40-char lowercase-hex git
        // commit between `/resolve/` and the next `/`. This generalizes the
        // single-literal `/resolve/main/` blocklist: ANY mutable ref (a branch, a
        // tag, a truncated/short SHA) is rejected, not just the literal "main".
        let is_full_commit = |seg: &str| {
            seg.len() == 40
                && seg
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        };
        for e in catalog() {
            let seg = resolve_commit_segment(e.url)
                .unwrap_or_else(|| panic!("{}: url has no /resolve/ commit segment", e.name));
            assert!(
                is_full_commit(seg),
                "{}: catalog url commit segment {seg:?} is not 40 lowercase-hex chars",
                e.name
            );
        }
        for p in catalog_provenance() {
            let seg = resolve_commit_segment(p.url)
                .unwrap_or_else(|| panic!("{}: provenance url has no commit segment", p.name));
            assert!(
                is_full_commit(seg),
                "{}: provenance url commit segment {seg:?} is not 40 lowercase-hex chars",
                p.name
            );
        }
    }

    #[test]
    fn catalog_and_provenance_urls_are_hosted_on_huggingface() {
        // Provenance = trusted SOURCE, not just a well-formed URL. Every
        // download/provenance URL must originate from huggingface.co. The
        // existing checks only pin `https://` + a `/resolve/<40hex>/` path
        // shape, both of which a hostile mirror (e.g. https://evil.example/
        // resolve/<40hex>/m.gguf) satisfies — a supply-chain origin swap would
        // slip through. Pin the host so a non-HF origin is a build failure.
        const TRUSTED: &str = "https://huggingface.co/";
        for e in catalog() {
            assert!(
                e.url.starts_with(TRUSTED),
                "{}: catalog url must be hosted on huggingface.co, got {:?}",
                e.name,
                e.url
            );
        }
        for p in catalog_provenance() {
            assert!(
                p.url.starts_with(TRUSTED),
                "{}: provenance url must be hosted on huggingface.co, got {:?}",
                p.name,
                p.url
            );
        }
    }

    #[test]
    fn catalog_pinned_hashes_are_unique() {
        // Each artifact has its own SHA-256. Two entries sharing a pinned hash
        // means at least one is a copy-paste provenance error (a new row cloned
        // from an existing one without updating expected_sha256) — the mispinned
        // model would then fail verify-before-rename against a foreign file's
        // digest forever. Names are guarded for uniqueness elsewhere; the hash
        // (the integrity anchor) must be distinct too.
        let hashes: Vec<&str> = catalog()
            .iter()
            .map(|e| e.expected_sha256.expect("every entry is pinned"))
            .collect();
        let unique: std::collections::HashSet<&str> = hashes.iter().copied().collect();
        assert_eq!(
            hashes.len(),
            unique.len(),
            "catalog pinned hashes must be unique (a shared hash is a copy-paste mispin)"
        );
    }

    #[test]
    fn gemma_and_llama_named_entries_are_license_gated() {
        // Guards name-implies-license drift: any entry whose name says "llama"
        // must carry the Llama gate, and any "gemma" entry the Gemma gate. A
        // rename or a mis-set license (e.g. a gemma model left Apache2) would
        // silently bypass the click-through.
        for e in catalog() {
            if e.name.contains("llama") {
                assert_eq!(
                    e.license,
                    License::LlamaCommunity,
                    "{}: a llama-named entry must be LlamaCommunity-gated",
                    e.name
                );
            }
            if e.name.contains("gemma") {
                assert_eq!(
                    e.license,
                    License::GemmaTerms,
                    "{}: a gemma-named entry must be GemmaTerms-gated",
                    e.name
                );
            }
        }
    }

    #[test]
    fn zero_available_ram_blocks_every_catalog_model() {
        // A machine reporting 0 GB (or a probe failure flooring to 0) must offer
        // NOTHING: every entry exceeds its minimum, so none is downloadable.
        for e in catalog() {
            assert!(
                !offerable_by_ram(e, 0),
                "{}: must not be offerable on a 0 GB machine",
                e.name
            );
            assert_eq!(
                ram_verdict(e, 0),
                RamVerdict::Exceeds,
                "{}: 0 GB must read as Exceeds",
                e.name
            );
        }
    }

    #[test]
    fn gated_licenses_need_acceptance() {
        assert!(License::GemmaTerms.needs_acceptance());
        assert!(License::LlamaCommunity.needs_acceptance());
        assert!(!License::Apache2.needs_acceptance());
        assert!(!License::Mit.needs_acceptance());
    }

    #[test]
    fn download_gate_passes_unencumbered_licenses() {
        // Apache2/Mit never prompt, even with nothing accepted — and the
        // one-click recommended() target is unencumbered by construction,
        // so the gate is provably INERT on today's only download path.
        for entry in catalog().iter().filter(|e| !e.license.needs_acceptance()) {
            assert_eq!(download_gate(entry, |_| false), DownloadGate::Proceed);
        }
        assert_eq!(
            download_gate(recommended().expect("unencumbered entry"), |_| false),
            DownloadGate::Proceed
        );
    }

    #[test]
    fn download_gate_blocks_unaccepted_gated_licenses() {
        // Assert the gate FORWARDS the entry's own fields, not hardcoded
        // catalog literals: this pins download_gate's behavior (it surfaces
        // model/license-name/terms-url from the entry) without duplicating
        // catalog data that would drift. The literal values are guarded
        // separately by catalog_entries_are_well_formed_and_ordered.
        for license in [License::LlamaCommunity, License::GemmaTerms] {
            let entry = catalog()
                .iter()
                .find(|e| e.license == license)
                .unwrap_or_else(|| panic!("catalog has a {license:?} entry"));
            assert_eq!(
                download_gate(entry, |_| false),
                DownloadGate::NeedsLicense {
                    model: entry.name,
                    license_name: entry.license.display_name(),
                    terms_url: entry.license.terms_url(),
                },
                "{}: gate must forward the entry's own license fields",
                entry.name
            );
        }
    }

    #[test]
    fn download_gate_passes_once_accepted_per_model_not_per_license() {
        let gemma = catalog()
            .iter()
            .find(|e| e.license == License::GemmaTerms)
            .expect("gemma entry");
        // Accepted THIS model → proceed.
        assert_eq!(
            download_gate(gemma, |name| name == "gemma-2-2b-q4_k_m"),
            DownloadGate::Proceed
        );
        // Accepted a DIFFERENT model → still prompts (per-model, not
        // per-license-class: c95 "once per model").
        assert!(matches!(
            download_gate(gemma, |name| name == "some-other-model"),
            DownloadGate::NeedsLicense { .. }
        ));
    }

    #[test]
    fn license_terms_urls_are_https_and_names_nonempty() {
        for license in [
            License::Apache2,
            License::Mit,
            License::GemmaTerms,
            License::LlamaCommunity,
        ] {
            assert!(license.terms_url().starts_with("https://"));
            assert!(!license.display_name().is_empty());
        }
    }

    #[test]
    fn gated_license_terms_urls_and_names_are_pinned_exactly() {
        // The https/non-empty invariant above would pass a typo'd-but-still-https
        // URL pointing at the WRONG license page. These are the exact click-
        // through targets surfaced verbatim through download_gate's NeedsLicense
        // before the first gated download — pin them exactly.
        assert_eq!(
            License::GemmaTerms.terms_url(),
            "https://ai.google.dev/gemma/terms"
        );
        assert_eq!(
            License::LlamaCommunity.terms_url(),
            "https://www.llama.com/llama3_2/license/"
        );
        assert_eq!(License::GemmaTerms.display_name(), "Gemma Terms of Use");
        assert_eq!(
            License::LlamaCommunity.display_name(),
            "Llama Community License"
        );
    }
}
