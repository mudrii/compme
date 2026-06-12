//! Pure model-picker helpers (D14 3b.4 slice a): catalog-index resolution
//! for the Setup tab's download target. The popup UI and its label
//! composition land with the AppKit slice — this module ships the
//! resolution half early because the consume edge can use it today
//! (selected-or-recommended, identical to `recommended()` until a picker
//! writes a different index).

use model_catalog::{catalog, recommended, ModelEntry};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommended_index_points_at_recommended() {
        let entry = recommended().expect("catalog has an unencumbered entry");
        assert_eq!(catalog()[recommended_index()].name, entry.name);
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
