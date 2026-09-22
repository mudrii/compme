//! UIA text-range → [`platform::TextContext`] builder — pure, compiled and
//! tested on every host.
//!
//! UIA reports text ranges as opaque objects. `materialize_document_selection`
//! derives UTF-16 offsets from the text of cloned prefix ranges; endpoint
//! comparisons validate ordering only because UIA does not define their
//! magnitude as a distance. The resulting document plus offsets then feed the
//! pure [`TextContext`] builder below.

use platform::{
    byte_index_and_scalar_count_for_utf16_units, byte_index_for_utf16_units, ContextSource,
    FieldHandle, OffsetEncoding, TextContext, TextRange,
};

#[cfg(any(windows, test))]
use platform::PlatformError;

/// A finite ceiling for every provider text read. Reaching the ceiling is
/// rejected because UIA does not separately report whether `GetText` truncated
/// its result, and accepting a truncated prefix would invent an offset.
#[cfg(any(windows, test))]
pub(crate) const MAX_UIA_TEXT_UNITS: i32 = 1_048_576;

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RangeEndpoint {
    Start,
    End,
}

/// Narrow COM-facing seam needed to materialize a document and one selection.
/// The live implementation wraps `IUIAutomationTextRange`; tests use an
/// in-memory provider whose endpoint comparisons deliberately return signs
/// rather than distances.
#[cfg(any(windows, test))]
pub(crate) trait UiaTextRangeOps: Sized {
    fn clone_range(&self) -> Result<Self, PlatformError>;
    fn compare_endpoints(
        &self,
        endpoint: RangeEndpoint,
        other: &Self,
        other_endpoint: RangeEndpoint,
    ) -> Result<i32, PlatformError>;
    fn move_endpoint_by_range(
        &mut self,
        endpoint: RangeEndpoint,
        other: &Self,
        other_endpoint: RangeEndpoint,
    ) -> Result<(), PlatformError>;
    fn get_text(&self, max_units: i32) -> Result<String, PlatformError>;
}

#[cfg(any(windows, test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MaterializedDocument {
    pub document: String,
    pub selection_start_utf16: i64,
    pub selection_end_utf16: i64,
}

/// Materialize a stable UIA document/selection snapshot without interpreting
/// `CompareEndpoints` as a character count.
#[cfg(any(windows, test))]
pub(crate) fn materialize_document_selection<R: UiaTextRangeOps>(
    document: &R,
    selection: &R,
) -> Result<MaterializedDocument, PlatformError> {
    validate_selection_order(document, selection)?;

    let document_text = bounded_range_text(document)?;
    let mut start_prefix = document.clone_range()?;
    start_prefix.move_endpoint_by_range(RangeEndpoint::End, selection, RangeEndpoint::Start)?;
    let start_text = bounded_range_text(&start_prefix)?;

    let mut end_prefix = document.clone_range()?;
    end_prefix.move_endpoint_by_range(RangeEndpoint::End, selection, RangeEndpoint::End)?;
    let end_text = bounded_range_text(&end_prefix)?;
    let selected_text = bounded_range_text(selection)?;

    let Some(expected_selected) = end_text.strip_prefix(&start_text) else {
        return Err(inconsistent_snapshot(
            "selection prefixes do not describe one document snapshot",
        ));
    };
    if !document_text.starts_with(&end_text) || expected_selected != selected_text {
        return Err(inconsistent_snapshot(
            "document, prefixes, and selected text disagree",
        ));
    }

    Ok(MaterializedDocument {
        document: document_text,
        selection_start_utf16: start_text.encode_utf16().count() as i64,
        selection_end_utf16: end_text.encode_utf16().count() as i64,
    })
}

#[cfg(any(windows, test))]
fn validate_selection_order<R: UiaTextRangeOps>(
    document: &R,
    selection: &R,
) -> Result<(), PlatformError> {
    let start_before_document =
        selection.compare_endpoints(RangeEndpoint::Start, document, RangeEndpoint::Start)? < 0;
    let start_after_document =
        selection.compare_endpoints(RangeEndpoint::Start, document, RangeEndpoint::End)? > 0;
    let end_before_document =
        selection.compare_endpoints(RangeEndpoint::End, document, RangeEndpoint::Start)? < 0;
    let end_after_document =
        selection.compare_endpoints(RangeEndpoint::End, document, RangeEndpoint::End)? > 0;
    let inverted =
        selection.compare_endpoints(RangeEndpoint::Start, selection, RangeEndpoint::End)? > 0;
    if start_before_document
        || start_after_document
        || end_before_document
        || end_after_document
        || inverted
    {
        return Err(inconsistent_snapshot(
            "selection endpoints fall outside the document or are inverted",
        ));
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn bounded_range_text<R: UiaTextRangeOps>(range: &R) -> Result<String, PlatformError> {
    let text = range.get_text(MAX_UIA_TEXT_UNITS)?;
    if text.encode_utf16().count() >= MAX_UIA_TEXT_UNITS as usize {
        return Err(PlatformError::CannotComplete {
            reason: format!(
                "UIA text range reached the bounded read limit ({MAX_UIA_TEXT_UNITS} UTF-16 units)"
            ),
        });
    }
    Ok(text)
}

#[cfg(any(windows, test))]
fn inconsistent_snapshot(reason: &str) -> PlatformError {
    PlatformError::CannotComplete {
        reason: format!("UIA returned an inconsistent text snapshot: {reason}"),
    }
}

#[cfg(any(windows, test))]
pub(crate) fn require_single_selection(length: i32) -> Result<(), PlatformError> {
    if length == 1 {
        Ok(())
    } else {
        Err(PlatformError::UnsupportedField {
            reason: format!(
                "focused element exposes {length} UIA selection ranges; exactly one is required"
            ),
        })
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct FakeRange {
        document: String,
        start: usize,
        end: usize,
        selected_override: Option<String>,
    }

    impl FakeRange {
        fn new(document: &str, start: usize, end: usize) -> Self {
            Self {
                document: document.into(),
                start,
                end,
                selected_override: None,
            }
        }

        fn endpoint(&self, endpoint: RangeEndpoint) -> usize {
            match endpoint {
                RangeEndpoint::Start => self.start,
                RangeEndpoint::End => self.end,
            }
        }
    }

    impl UiaTextRangeOps for FakeRange {
        fn clone_range(&self) -> Result<Self, PlatformError> {
            Ok(self.clone())
        }

        fn compare_endpoints(
            &self,
            endpoint: RangeEndpoint,
            other: &Self,
            other_endpoint: RangeEndpoint,
        ) -> Result<i32, PlatformError> {
            // UIA promises ordering, not distance. This fake intentionally
            // returns exactly -1/0/1 so offset extraction cannot accidentally
            // depend on the magnitude.
            Ok(self.endpoint(endpoint).cmp(&other.endpoint(other_endpoint)) as i32)
        }

        fn move_endpoint_by_range(
            &mut self,
            endpoint: RangeEndpoint,
            other: &Self,
            other_endpoint: RangeEndpoint,
        ) -> Result<(), PlatformError> {
            let value = other.endpoint(other_endpoint);
            match endpoint {
                RangeEndpoint::Start => self.start = value,
                RangeEndpoint::End => self.end = value,
            }
            Ok(())
        }

        fn get_text(&self, max_units: i32) -> Result<String, PlatformError> {
            assert_eq!(max_units, MAX_UIA_TEXT_UNITS);
            if let Some(text) = &self.selected_override {
                return Ok(text.clone());
            }
            let units: Vec<u16> = self.document.encode_utf16().collect();
            String::from_utf16(&units[self.start..self.end]).map_err(|err| {
                PlatformError::CannotComplete {
                    reason: format!("fake UTF-16 decode failed: {err}"),
                }
            })
        }
    }

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

    #[test]
    fn sign_only_endpoint_comparisons_produce_exact_ascii_selection() {
        let document = FakeRange::new("hello world", 0, 11);
        let selection = FakeRange::new("hello world", 6, 11);
        let materialized = materialize_document_selection(&document, &selection).unwrap();
        assert_eq!(
            materialized,
            MaterializedDocument {
                document: "hello world".into(),
                selection_start_utf16: 6,
                selection_end_utf16: 11,
            }
        );
        let ctx = text_context_from_uia_document(
            field(),
            &materialized.document,
            materialized.selection_start_utf16,
            materialized.selection_end_utf16,
        );
        assert_eq!(ctx.left, "hello ");
        assert_eq!(ctx.selected_text.as_deref(), Some("world"));
        assert_eq!(ctx.right, "");
        assert_eq!(ctx.caret, 6);
    }

    #[test]
    fn prefix_text_counts_utf16_units_for_emoji_and_multiline_text() {
        let text = "a😀\nsecond line";
        let total = text.encode_utf16().count();
        let selection = FakeRange::new(text, 1, 3);
        let materialized =
            materialize_document_selection(&FakeRange::new(text, 0, total), &selection).unwrap();
        assert_eq!(materialized.selection_start_utf16, 1);
        assert_eq!(materialized.selection_end_utf16, 3);
        let ctx = text_context_from_uia_document(
            field(),
            &materialized.document,
            materialized.selection_start_utf16,
            materialized.selection_end_utf16,
        );
        assert_eq!(ctx.left, "a");
        assert_eq!(ctx.selected_text.as_deref(), Some("😀"));
        assert_eq!(ctx.right, "\nsecond line");
        assert_eq!(ctx.caret, 1);
    }

    #[test]
    fn collapsed_selection_produces_the_exact_caret() {
        let text = "before after";
        let materialized = materialize_document_selection(
            &FakeRange::new(text, 0, text.len()),
            &FakeRange::new(text, 7, 7),
        )
        .unwrap();
        assert_eq!(materialized.selection_start_utf16, 7);
        assert_eq!(materialized.selection_end_utf16, 7);
        let ctx = text_context_from_uia_document(
            field(),
            &materialized.document,
            materialized.selection_start_utf16,
            materialized.selection_end_utf16,
        );
        assert_eq!(ctx.left, "before ");
        assert_eq!(ctx.right, "after");
        assert_eq!(ctx.caret, 7);
        assert_eq!(ctx.selection, None);
    }

    #[test]
    fn rejects_out_of_document_and_inconsistent_provider_snapshots() {
        let document = FakeRange::new("hello", 0, 5);
        assert!(matches!(
            materialize_document_selection(&document, &FakeRange::new("hello", 4, 6)),
            Err(PlatformError::CannotComplete { .. })
        ));

        let mut changed_selection = FakeRange::new("hello", 1, 4);
        changed_selection.selected_override = Some("different".into());
        assert!(matches!(
            materialize_document_selection(&document, &changed_selection),
            Err(PlatformError::CannotComplete { .. })
        ));
    }

    #[test]
    fn missing_or_ambiguous_selection_ranges_are_rejected() {
        assert!(matches!(
            require_single_selection(0),
            Err(PlatformError::UnsupportedField { .. })
        ));
        assert_eq!(require_single_selection(1), Ok(()));
        assert!(matches!(
            require_single_selection(2),
            Err(PlatformError::UnsupportedField { .. })
        ));
    }
}
