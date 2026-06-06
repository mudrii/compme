//! Pure host-loop wiring helpers, kept out of the AppKit glue so they can be
//! unit-tested without a GUI session.
//!
//! Two jobs:
//! 1. Turn an adapter `TextContext` read into an `engine::TextChange`
//!    (deriving `EditKind` and the previous-state fields). There is no
//!    text-change subscription on macOS, so the caret/selection-changed callback
//!    drives this: read context, derive a change, feed the engine.
//! 2. Coalesce `CompletionRequest`s latest-wins so the inference thread only ever
//!    works the newest request.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use engine::{CompletionRequest, EditKind, TextChange, TriggerPolicy};
use platform::{FieldHandle, TextContext};

/// Reconstruct the full field value and a char-indexed caret from a context read.
///
/// `core::left_context` is char-indexed (`value.chars().take(caret)`), so pairing
/// `left + right` with `caret = left.chars().count()` reproduces the adapter's
/// left text exactly as the engine's prompt — independent of the adapter's own
/// caret offset encoding (UTF-16 on macOS).
fn value_and_caret(ctx: &TextContext) -> (String, usize) {
    let value = format!("{}{}", ctx.left, ctx.right);
    let caret = ctx.left.chars().count();
    (value, caret)
}

fn hash_value(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// Compare previous vs new value by char count to classify the edit. The engine
/// only distinguishes `Delete` (suppressed) from everything else, so a same-length
/// change maps to `Unknown` (still requests).
fn edit_kind(prev_chars: usize, new_chars: usize) -> EditKind {
    use std::cmp::Ordering::*;
    match new_chars.cmp(&prev_chars) {
        Greater => EditKind::Insert,
        Less => EditKind::Delete,
        Equal => EditKind::Unknown,
    }
}

/// Tracks the last-seen value/caret per focused field so successive context reads
/// can be diffed into `TextChange`s.
#[derive(Default)]
pub struct FieldTracker {
    last: Option<TrackedField>,
}

struct TrackedField {
    field: FieldHandle,
    value: String,
    caret: usize,
}

impl FieldTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Derive a `TextChange` from a fresh context read, updating internal state.
    ///
    /// A change of focused field resets the diff baseline (the new field has no
    /// previous state), so the first edit in a field reads as an `Insert`.
    pub fn observe(
        &mut self,
        field: &FieldHandle,
        ctx: &TextContext,
        trigger: TriggerPolicy,
        now_ms: u64,
    ) -> TextChange {
        let (value, caret) = value_and_caret(ctx);
        let new_chars = value.chars().count();

        let prev = match &self.last {
            Some(prev) if &prev.field == field => Some(prev),
            _ => None,
        };

        let (edit, previous_caret, previous_value_hash) = match prev {
            Some(prev) => (
                edit_kind(prev.value.chars().count(), new_chars),
                Some(prev.caret),
                Some(hash_value(&prev.value)),
            ),
            None => (
                if new_chars == 0 {
                    EditKind::Unknown
                } else {
                    EditKind::Insert
                },
                None,
                None,
            ),
        };

        self.last = Some(TrackedField {
            field: field.clone(),
            value: value.clone(),
            caret,
        });

        TextChange {
            field: field.clone(),
            value,
            caret,
            edit,
            previous_caret,
            previous_value_hash,
            trigger,
            now_ms,
        }
    }

    /// Forget the diff baseline (e.g. on refocus or after a self-insert) so the
    /// next observation is treated as a fresh field.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// Holds at most one pending completion request, keeping the newest by
/// generation. The inference thread should only ever work the latest request;
/// any stale result that still arrives is discarded by the engine on generation.
#[derive(Default)]
pub struct LatestRequest {
    pending: Option<CompletionRequest>,
}

impl LatestRequest {
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer a request; it replaces the pending one only if it is at least as new.
    pub fn offer(&mut self, request: CompletionRequest) {
        let newer = self
            .pending
            .as_ref()
            .is_none_or(|cur| request.generation >= cur.generation);
        if newer {
            self.pending = Some(request);
        }
    }

    /// Take the pending request, leaving the slot empty.
    pub fn take(&mut self) -> Option<CompletionRequest> {
        self.pending.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::{ContextSource, OffsetEncoding};

    fn field(id: &str) -> FieldHandle {
        FieldHandle {
            app: "TextEdit".into(),
            pid: Some(42),
            element_id: id.into(),
            generation: 1,
        }
    }

    fn ctx(left: &str, right: &str) -> TextContext {
        TextContext {
            left: left.into(),
            right: right.into(),
            selection: None,
            caret: left.chars().count(),
            source: ContextSource::Accessibility,
            field_id: field("f"),
            offset_encoding: OffsetEncoding::Utf16CodeUnits,
        }
    }

    #[test]
    fn reconstructs_value_and_char_caret() {
        let mut tracker = FieldTracker::new();
        let change = tracker.observe(
            &field("f"),
            &ctx("hello ", "world"),
            TriggerPolicy::Automatic,
            10,
        );
        assert_eq!(change.value, "hello world");
        assert_eq!(change.caret, 6);
    }

    #[test]
    fn caret_is_char_counted_for_unicode_left() {
        // "café " is 5 chars but 6 UTF-8 bytes; caret must be the char count.
        let mut tracker = FieldTracker::new();
        let change = tracker.observe(&field("f"), &ctx("café ", "x"), TriggerPolicy::Automatic, 0);
        assert_eq!(change.caret, 5);
        assert_eq!(change.value, "café x");
    }

    #[test]
    fn first_observation_with_text_is_insert() {
        let mut tracker = FieldTracker::new();
        let change = tracker.observe(&field("f"), &ctx("hi", ""), TriggerPolicy::Automatic, 0);
        assert_eq!(change.edit, EditKind::Insert);
        assert_eq!(change.previous_caret, None);
        assert_eq!(change.previous_value_hash, None);
    }

    #[test]
    fn first_observation_empty_is_unknown() {
        let mut tracker = FieldTracker::new();
        let change = tracker.observe(&field("f"), &ctx("", ""), TriggerPolicy::Automatic, 0);
        assert_eq!(change.edit, EditKind::Unknown);
    }

    #[test]
    fn growing_value_is_insert_with_previous_state() {
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx("hel", ""), TriggerPolicy::Automatic, 0);
        let change = tracker.observe(&field("f"), &ctx("hell", ""), TriggerPolicy::Automatic, 1);
        assert_eq!(change.edit, EditKind::Insert);
        assert_eq!(change.previous_caret, Some(3));
        assert_eq!(change.previous_value_hash, Some(hash_value("hel")));
    }

    #[test]
    fn shrinking_value_is_delete() {
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx("hell", ""), TriggerPolicy::Automatic, 0);
        let change = tracker.observe(&field("f"), &ctx("hel", ""), TriggerPolicy::Automatic, 1);
        assert_eq!(change.edit, EditKind::Delete);
    }

    #[test]
    fn same_length_change_is_unknown() {
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx("cat", ""), TriggerPolicy::Automatic, 0);
        let change = tracker.observe(&field("f"), &ctx("cot", ""), TriggerPolicy::Automatic, 1);
        assert_eq!(change.edit, EditKind::Unknown);
    }

    #[test]
    fn switching_field_resets_baseline() {
        let mut tracker = FieldTracker::new();
        tracker.observe(
            &field("a"),
            &ctx("longtext", ""),
            TriggerPolicy::Automatic,
            0,
        );
        // New field with shorter text must NOT read as a delete.
        let change = tracker.observe(&field("b"), &ctx("hi", ""), TriggerPolicy::Automatic, 1);
        assert_eq!(change.edit, EditKind::Insert);
        assert_eq!(change.previous_caret, None);
    }

    #[test]
    fn explicit_reset_forgets_baseline() {
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx("hello", ""), TriggerPolicy::Automatic, 0);
        tracker.reset();
        let change = tracker.observe(&field("f"), &ctx("hi", ""), TriggerPolicy::Automatic, 1);
        assert_eq!(change.edit, EditKind::Insert);
        assert_eq!(change.previous_caret, None);
    }

    #[test]
    fn trigger_is_carried_through() {
        let mut tracker = FieldTracker::new();
        let change = tracker.observe(&field("f"), &ctx("hi", ""), TriggerPolicy::Manual, 7);
        assert_eq!(change.trigger, TriggerPolicy::Manual);
        assert_eq!(change.now_ms, 7);
    }

    fn request(generation: u64) -> CompletionRequest {
        CompletionRequest {
            generation,
            field: field("f"),
            snapshot: generation,
            prompt: "p".into(),
            max_tokens: 16,
        }
    }

    #[test]
    fn latest_request_keeps_newest() {
        let mut latest = LatestRequest::new();
        latest.offer(request(1));
        latest.offer(request(3));
        assert_eq!(latest.take().unwrap().generation, 3);
    }

    #[test]
    fn latest_request_ignores_older() {
        let mut latest = LatestRequest::new();
        latest.offer(request(5));
        latest.offer(request(2));
        assert_eq!(latest.take().unwrap().generation, 5);
    }

    #[test]
    fn take_empties_the_slot() {
        let mut latest = LatestRequest::new();
        latest.offer(request(1));
        assert!(latest.take().is_some());
        assert!(latest.take().is_none());
    }
}
