//! UIA text-range → [`platform::TextContext`] builder — pure, compiled and
//! tested on every host.
//!
//! UIA reports text ranges as opaque objects; the live adapter materializes
//! the one thing it needs from them — the document text plus the selection's
//! start/end as **UTF-16 code-unit** endpoints (`CompareEndpoints` against the
//! document range) — and this module does the rest. UIA offsets count UTF-16
//! code units just like AppKit and Chromium, so astral-plane scalars (emoji)
//! occupy two units and are the classic off-by-one source; the tests pin one.

use platform::{ContextSource, FieldHandle, OffsetEncoding, TextContext, TextRange};

/// Build a [`TextContext`] from the document text and a UTF-16 selection.
///
/// Every input is clamped, never trusted: negative endpoints clamp to 0,
/// endpoints past the document clamp to its UTF-16 length, and an inverted
/// range (start > end) is normalized by swapping — the engine must never see
/// a context this module could not build, and this module must never panic.
pub fn text_context_from_uia_document(
    field: FieldHandle,
    document: &str,
    selection_start_utf16: i64,
    selection_end_utf16: i64,
) -> TextContext {
    let utf16_len = document.encode_utf16().count() as i64;
    let mut start = selection_start_utf16.clamp(0, utf16_len) as usize;
    let mut end = selection_end_utf16.clamp(0, utf16_len) as usize;
    if start > end {
        std::mem::swap(&mut start, &mut end);
    }
    let (left_end, left_scalars) = byte_index_and_scalar_count_for_utf16_units(document, start);
    let right_start = byte_index_for_utf16_units(document, end);
    TextContext {
        left: document[..left_end].to_string(),
        right: document[right_start..].to_string(),
        left_scalars,
        selection: (end > start).then_some(TextRange { start, end }),
        selected_text: (end > start).then(|| document[left_end..right_start].to_string()),
        caret: start,
        source: ContextSource::Accessibility,
        field_id: field,
        offset_encoding: OffsetEncoding::Utf16CodeUnits,
    }
}

fn byte_index_for_utf16_units(value: &str, target_units: usize) -> usize {
    byte_index_and_scalar_count_for_utf16_units(value, target_units).0
}

/// Byte index into `value` for the given UTF-16 unit offset, plus the Unicode
/// scalar count up to that index. An offset that lands mid-surrogate resolves
/// to the boundary *after* the surrogate pair (the pair is kept whole and
/// counted from its start).
fn byte_index_and_scalar_count_for_utf16_units(value: &str, target_units: usize) -> (usize, usize) {
    let mut units_seen = 0usize;
    let mut scalars_seen = 0usize;
    for (byte_index, ch) in value.char_indices() {
        if units_seen >= target_units {
            return (byte_index, scalars_seen);
        }
        units_seen += ch.len_utf16();
        scalars_seen += 1;
    }
    (value.len(), scalars_seen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field() -> FieldHandle {
        FieldHandle {
            app: "smoke".into(),
            pid: Some(1),
            element_id: "uia:pid=1:rid=1".into(),
            generation: 0,
        }
    }

    #[test]
    fn ascii_selection_splits_left_selected_right() {
        let ctx = text_context_from_uia_document(field(), "hello world", 6, 11);
        assert_eq!(ctx.left, "hello ");
        assert_eq!(ctx.selected_text.as_deref(), Some("world"));
        assert_eq!(ctx.right, "");
        assert_eq!(ctx.caret, 6);
        assert_eq!(ctx.left_scalars, 6);
        assert_eq!(ctx.selection, Some(TextRange { start: 6, end: 11 }));
    }

    #[test]
    fn collapsed_selection_has_caret_and_no_selection() {
        let ctx = text_context_from_uia_document(field(), "hello", 2, 2);
        assert_eq!(ctx.left, "he");
        assert_eq!(ctx.right, "llo");
        assert_eq!(ctx.selected_text, None);
        assert_eq!(ctx.selection, None);
        assert_eq!(ctx.caret, 2);
    }

    #[test]
    fn astral_scalar_spans_two_utf16_units_and_stays_whole() {
        // "a😀bc": UTF-16 length 5 (a=1, 😀=2, b=1, c=1). Selecting units
        // [1, 3) selects exactly the emoji — the surrogate pair is never cut.
        let ctx = text_context_from_uia_document(field(), "a😀bc", 1, 3);
        assert_eq!(ctx.left, "a");
        assert_eq!(ctx.selected_text.as_deref(), Some("😀"));
        assert_eq!(ctx.right, "bc");
        assert_eq!(ctx.caret, 1);
        assert_eq!(ctx.left_scalars, 1);
        // Selecting past the emoji lands on 'b', not inside the pair.
        let ctx = text_context_from_uia_document(field(), "a😀bc", 3, 4);
        assert_eq!(ctx.selected_text.as_deref(), Some("b"));
        // Selecting the whole string through its real UTF-16 length.
        let ctx = text_context_from_uia_document(field(), "a😀bc", 0, 5);
        assert_eq!(ctx.selected_text.as_deref(), Some("a😀bc"));
        assert_eq!(ctx.left_scalars, 0);
    }

    #[test]
    fn out_of_range_and_negative_endpoints_clamp() {
        let ctx = text_context_from_uia_document(field(), "hello", -7, 999);
        assert_eq!(ctx.left, "");
        assert_eq!(ctx.selected_text.as_deref(), Some("hello"));
        assert_eq!(ctx.right, "");
        assert_eq!(ctx.caret, 0);
        assert_eq!(ctx.selection, Some(TextRange { start: 0, end: 5 }));
    }

    #[test]
    fn inverted_selection_is_normalized_by_swapping() {
        let ctx = text_context_from_uia_document(field(), "hello", 4, 1);
        assert_eq!(ctx.left, "h");
        assert_eq!(ctx.selected_text.as_deref(), Some("ell"));
        assert_eq!(ctx.right, "o");
        assert_eq!(ctx.selection, Some(TextRange { start: 1, end: 4 }));
    }

    #[test]
    fn empty_document_is_well_formed() {
        let ctx = text_context_from_uia_document(field(), "", 0, 0);
        assert_eq!(ctx.left, "");
        assert_eq!(ctx.right, "");
        assert_eq!(ctx.selected_text, None);
        assert_eq!(ctx.selection, None);
        assert_eq!(ctx.caret, 0);
        assert_eq!(ctx.left_scalars, 0);
        assert_eq!(ctx.offset_encoding, OffsetEncoding::Utf16CodeUnits);
        assert_eq!(ctx.source, ContextSource::Accessibility);
    }
}
