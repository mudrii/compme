//! Pure model-picker helpers (D14 3b.4 slice a): catalog-index resolution
//! for the Setup tab's download target. The popup UI and its label
//! composition land with the AppKit slice — this module ships the
//! resolution half early because the consume edge can use it today
//! (selected-or-recommended, identical to `recommended()` until a picker
//! writes a different index).

use model_catalog::{catalog, ram_verdict, recommended, ModelEntry};

/// Position of [`recommended`] in [`catalog`] — the picker's default
/// selection, and the consume edge's effective index until the popup lands.
///
/// The `unwrap_or(0)` fallback diverges from `recommended()` in one
/// hypothetical: a catalog with NO unencumbered entry would resolve to
/// entry 0 (encumbered) instead of skipping the download — safe either way,
/// because that path lands in the license gate, which prompts and fails
/// closed. Unreachable with today's data; stated so the "identical to
/// recommended()" claim at the consume edge is read as present-tense.
pub fn recommended_index() -> usize {
    let recommended_name = recommended().map(|e| e.name);
    catalog()
        .iter()
        .position(|e| Some(e.name) == recommended_name)
        .unwrap_or(0)
}

/// Picker index → catalog entry, falling back to [`recommended`] on any
/// out-of-range index — total over garbage/truncated selector values, so
/// the download edge never panics on a bad index.
pub fn selected_catalog_entry(index: usize) -> Option<&'static ModelEntry> {
    catalog().get(index).or_else(recommended)
}

/// The popup's item titles: one per catalog row (same order, so the selected
/// index still addresses [`catalog`]), each suffixed with its RAM verdict for
/// `available_gb` — e.g. `"qwen2.5-0.5b-q4_k_m · fits"`. ADVISORY only: the
/// suffix never blocks a download, it just tells the user how the model fits
/// this machine. `model_catalog` is app-side, so the run loop composes these
/// and passes the finished strings to the (model_catalog-blind) settings window.
pub fn model_menu_titles(available_gb: u32) -> Vec<String> {
    catalog()
        .iter()
        .map(|e| {
            format!(
                "{} \u{b7} {}",
                e.name,
                ram_verdict(e, available_gb).advice()
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommended_index_points_at_recommended() {
        let entry = recommended().expect("catalog has an unencumbered entry");
        assert_eq!(catalog()[recommended_index()].name, entry.name);
    }

    #[test]
    fn model_menu_titles_suffix_each_row_with_its_ram_verdict() {
        // 0 GiB available → every model is below its minimum → "exceeds".
        let none = model_menu_titles(0);
        assert_eq!(none.len(), catalog().len());
        for (title, entry) in none.iter().zip(catalog()) {
            assert!(
                title.starts_with(entry.name),
                "title must start with the model name: {title:?}"
            );
            assert!(
                title.ends_with("exceeds available memory"),
                "0 GiB → every row exceeds: {title:?}"
            );
        }
        // Abundant RAM (64 GiB) → the smallest model (index 0) comfortably fits.
        let plenty = model_menu_titles(64);
        assert!(
            plenty[0].ends_with("fits"),
            "64 GiB → smallest model fits: {:?}",
            plenty[0]
        );
        // The suffix tracks ram_verdict exactly (not a fixed string) — pin one
        // row against the helper it composes from.
        let entry = &catalog()[0];
        assert_eq!(
            model_menu_titles(64)[0],
            format!("{} \u{b7} {}", entry.name, ram_verdict(entry, 64).advice())
        );
    }

    #[test]
    fn selected_entry_resolves_in_range_and_falls_back_out_of_range() {
        // In range: the index addresses the catalog directly.
        let last = catalog().len() - 1;
        assert_eq!(
            selected_catalog_entry(last).expect("in range").name,
            catalog()[last].name
        );
        // Out of range (garbage selector value): recommended, never a panic.
        assert_eq!(
            selected_catalog_entry(usize::MAX).expect("fallback").name,
            recommended().expect("recommended").name
        );
        // The consume edge's interim wiring: selected(recommended_index())
        // must be EXACTLY recommended() — zero behavior change today.
        assert_eq!(
            selected_catalog_entry(recommended_index())
                .expect("default")
                .name,
            recommended().expect("recommended").name
        );
    }
}
