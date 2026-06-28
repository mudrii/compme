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

use engine::{CompletionRequest, EditKind, TextChange, TriggerPolicy};
use platform::{FieldHandle, TextContext};

/// Reconstruct the full field value and a char-indexed caret from a context read.
///
/// `context::left_context` is char-indexed (`value.chars().take(caret)`), so pairing
/// `left + right` with `caret = left.chars().count()` reproduces the adapter's
/// left text exactly as the engine's prompt — independent of the adapter's own
/// caret offset encoding (UTF-16 on macOS).
fn value_and_caret(ctx: &TextContext) -> (String, usize) {
    let value = format!("{}{}", ctx.left, ctx.right);
    let caret = ctx.left.chars().count();
    (value, caret)
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

fn inserted_text(prev: &str, new: &str) -> Option<String> {
    let prev_chars: Vec<char> = prev.chars().collect();
    let new_chars: Vec<char> = new.chars().collect();
    if new_chars.len() <= prev_chars.len() {
        return None;
    }

    let mut prefix = 0;
    while prefix < prev_chars.len()
        && prefix < new_chars.len()
        && prev_chars[prefix] == new_chars[prefix]
    {
        prefix += 1;
    }

    let mut suffix = 0;
    while suffix < prev_chars.len().saturating_sub(prefix)
        && suffix < new_chars.len().saturating_sub(prefix)
        && prev_chars[prev_chars.len() - 1 - suffix] == new_chars[new_chars.len() - 1 - suffix]
    {
        suffix += 1;
    }

    if prefix + suffix != prev_chars.len() {
        return None;
    }

    let end = new_chars.len().saturating_sub(suffix);
    (prefix < end).then(|| new_chars[prefix..end].iter().collect())
}

/// The result of observing a context read: either the field's content changed
/// (typing/paste/delete) or the caret moved within unchanged content.
///
/// macOS delivers one selection-changed notification for both cases. Splitting
/// them here lets the run loop feed typing to `on_text_changed` (which schedules
/// a completion) and a bare cursor move to `on_caret_moved` (hide-on-jump
/// invalidation) — instead of re-requesting a completion on every cursor nudge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observation {
    Typed(TextChange),
    CaretMoved { field: FieldHandle, caret: usize },
}

/// Tracks the last-seen value/caret per focused field so successive context reads
/// can be diffed into `Observation`s.
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

    /// Classify a fresh context read against the last one for this field,
    /// updating internal state.
    ///
    /// A change of focused field resets the diff baseline (the new field has no
    /// previous state), so the first edit in a field reads as an `Insert`. When
    /// the reconstructed value is identical to the previous read, this is a pure
    /// caret move, reported as [`Observation::CaretMoved`].
    pub fn observe(
        &mut self,
        field: &FieldHandle,
        ctx: &TextContext,
        trigger: TriggerPolicy,
        now_ms: u64,
    ) -> Observation {
        self.observe_inner(field, ctx, trigger, now_ms, false)
    }

    pub fn observe_with_inserted_text(
        &mut self,
        field: &FieldHandle,
        ctx: &TextContext,
        trigger: TriggerPolicy,
        now_ms: u64,
    ) -> Observation {
        self.observe_inner(field, ctx, trigger, now_ms, true)
    }

    fn observe_inner(
        &mut self,
        field: &FieldHandle,
        ctx: &TextContext,
        trigger: TriggerPolicy,
        now_ms: u64,
        capture_inserted_text: bool,
    ) -> Observation {
        let (value, caret) = value_and_caret(ctx);
        let new_chars = value.chars().count();

        // Snapshot the previous state for this field, then release the borrow so
        // we can update the baseline.
        let prev = match &self.last {
            Some(prev) if &prev.field == field => Some((prev.value.clone(), prev.caret)),
            _ => None,
        };

        self.last = Some(TrackedField {
            field: field.clone(),
            value: value.clone(),
            caret,
        });

        if let Some((prev_value, _)) = &prev {
            if *prev_value == value {
                return Observation::CaretMoved {
                    field: field.clone(),
                    caret,
                };
            }
        }

        let (edit, inserted_text) = match &prev {
            Some((prev_value, _)) => (
                edit_kind(prev_value.chars().count(), new_chars),
                capture_inserted_text
                    .then(|| inserted_text(prev_value, &value))
                    .flatten(),
            ),
            None => (
                if new_chars == 0 {
                    EditKind::Unknown
                } else {
                    EditKind::Insert
                },
                None,
            ),
        };

        Observation::Typed(TextChange {
            field: field.clone(),
            value,
            caret,
            edit,
            inserted_text,
            trigger,
            now_ms,
        })
    }

    /// Forget the diff baseline (e.g. on refocus or after a self-insert) so the
    /// next observation is treated as a fresh field.
    pub fn reset(&mut self) {
        self.last = None;
    }

    pub fn apply_self_insert(&mut self, field: &FieldHandle, text: &str) {
        if text.is_empty() {
            return;
        }
        let Some(last) = &mut self.last else {
            return;
        };
        if &last.field != field {
            return;
        }

        let byte_index = byte_index_for_char(&last.value, last.caret);
        last.value.insert_str(byte_index, text);
        last.caret = last.caret.saturating_add(text.chars().count());
    }

    /// Absorb a *replacement* self-insert (emoji `:smile`→😄, typo fix, US→UK
    /// spelling): delete `replace_left` characters immediately left of the caret,
    /// then insert `text`. Mirrors the field after a `Command::Replace`
    /// (delete-then-insert) so the next AX readback registers the accept's own
    /// edit, not new typing — the same echo-absorption guarantee
    /// `apply_self_insert` gives append-only completions. `replace_left` is
    /// clamped to the characters actually available left of the caret.
    pub fn apply_self_replace(&mut self, field: &FieldHandle, text: &str, replace_left: usize) {
        let Some(last) = &mut self.last else {
            return;
        };
        if &last.field != field {
            return;
        }
        let delete = replace_left.min(last.caret);
        if delete > 0 {
            let start_char = last.caret - delete;
            let start = byte_index_for_char(&last.value, start_char);
            let end = byte_index_for_char(&last.value, last.caret);
            last.value.replace_range(start..end, "");
            last.caret = start_char;
        }
        if !text.is_empty() {
            let byte_index = byte_index_for_char(&last.value, last.caret);
            last.value.insert_str(byte_index, text);
            last.caret = last.caret.saturating_add(text.chars().count());
        }
    }
}

fn byte_index_for_char(value: &str, target_chars: usize) -> usize {
    if target_chars == 0 {
        return 0;
    }
    value
        .char_indices()
        .nth(target_chars)
        .map(|(index, _)| index)
        .unwrap_or(value.len())
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

    pub fn clear(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::{ContextSource, OffsetEncoding, TextRange};

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

    /// Unwrap a `Typed` observation, panicking on a caret move.
    fn typed(obs: Observation) -> TextChange {
        match obs {
            Observation::Typed(change) => change,
            other => panic!("expected Typed, got {other:?}"),
        }
    }

    #[test]
    fn reconstructs_value_and_char_caret() {
        let mut tracker = FieldTracker::new();
        let change = typed(tracker.observe(
            &field("f"),
            &ctx("hello ", "world"),
            TriggerPolicy::Automatic,
            10,
        ));
        assert_eq!(change.value, "hello world");
        assert_eq!(change.caret, 6);
    }

    #[test]
    fn caret_is_char_counted_for_unicode_left() {
        // "café " is 5 chars but 6 UTF-8 bytes; caret must be the char count.
        let mut tracker = FieldTracker::new();
        let change =
            typed(tracker.observe(&field("f"), &ctx("café ", "x"), TriggerPolicy::Automatic, 0));
        assert_eq!(change.caret, 5);
        assert_eq!(change.value, "café x");
    }

    #[test]
    fn first_observation_with_text_is_insert() {
        let mut tracker = FieldTracker::new();
        let change =
            typed(tracker.observe(&field("f"), &ctx("hi", ""), TriggerPolicy::Automatic, 0));
        assert_eq!(change.edit, EditKind::Insert);
        assert_eq!(change.inserted_text, None);
    }

    #[test]
    fn first_observation_empty_is_unknown() {
        let mut tracker = FieldTracker::new();
        let change = typed(tracker.observe(&field("f"), &ctx("", ""), TriggerPolicy::Automatic, 0));
        assert_eq!(change.edit, EditKind::Unknown);
    }

    #[test]
    fn growing_value_is_insert_with_previous_state() {
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx("hel", ""), TriggerPolicy::Automatic, 0);
        let change =
            typed(tracker.observe(&field("f"), &ctx("hell", ""), TriggerPolicy::Automatic, 1));
        assert_eq!(change.edit, EditKind::Insert);
        assert_eq!(change.inserted_text, None);
    }

    #[test]
    fn observe_with_inserted_text_captures_insert_delta() {
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx("hel", ""), TriggerPolicy::Automatic, 0);
        let change = typed(tracker.observe_with_inserted_text(
            &field("f"),
            &ctx("hell", ""),
            TriggerPolicy::Automatic,
            1,
        ));
        assert_eq!(change.inserted_text.as_deref(), Some("l"));
    }

    #[test]
    fn inserted_text_captures_middle_insert_without_existing_text() {
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx("ab", "cd"), TriggerPolicy::Automatic, 0);
        let change = typed(tracker.observe_with_inserted_text(
            &field("f"),
            &ctx("abXY", "cd"),
            TriggerPolicy::Automatic,
            1,
        ));
        assert_eq!(change.inserted_text.as_deref(), Some("XY"));
    }

    #[test]
    fn inserted_text_is_unicode_scalar_safe() {
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx("café ", ""), TriggerPolicy::Automatic, 0);
        let change = typed(tracker.observe_with_inserted_text(
            &field("f"),
            &ctx("café 😄", ""),
            TriggerPolicy::Automatic,
            1,
        ));
        assert_eq!(change.inserted_text.as_deref(), Some("😄"));
    }

    #[test]
    fn inserted_text_rejects_wrapping_existing_text() {
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx("secret", ""), TriggerPolicy::Automatic, 0);
        let change = typed(tracker.observe_with_inserted_text(
            &field("f"),
            &ctx("a secret b", ""),
            TriggerPolicy::Automatic,
            1,
        ));
        assert_eq!(change.inserted_text, None);
    }

    #[test]
    fn inserted_text_rejects_replacements() {
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx("secret", ""), TriggerPolicy::Automatic, 0);
        let change = typed(tracker.observe_with_inserted_text(
            &field("f"),
            &ctx("seXrets", ""),
            TriggerPolicy::Automatic,
            1,
        ));
        assert_eq!(change.inserted_text, None);
    }

    #[test]
    fn shrinking_value_is_delete() {
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx("hell", ""), TriggerPolicy::Automatic, 0);
        let change =
            typed(tracker.observe(&field("f"), &ctx("hel", ""), TriggerPolicy::Automatic, 1));
        assert_eq!(change.edit, EditKind::Delete);
        assert_eq!(change.inserted_text, None);
    }

    #[test]
    fn same_length_change_is_unknown() {
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx("cat", ""), TriggerPolicy::Automatic, 0);
        let change =
            typed(tracker.observe(&field("f"), &ctx("cot", ""), TriggerPolicy::Automatic, 1));
        assert_eq!(change.edit, EditKind::Unknown);
        assert_eq!(change.inserted_text, None);
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
        let change =
            typed(tracker.observe(&field("b"), &ctx("hi", ""), TriggerPolicy::Automatic, 1));
        assert_eq!(change.edit, EditKind::Insert);
    }

    #[test]
    fn explicit_reset_forgets_baseline() {
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx("hello", ""), TriggerPolicy::Automatic, 0);
        tracker.reset();
        let change =
            typed(tracker.observe(&field("f"), &ctx("hi", ""), TriggerPolicy::Automatic, 1));
        assert_eq!(change.edit, EditKind::Insert);
    }

    #[test]
    fn trigger_is_carried_through() {
        let mut tracker = FieldTracker::new();
        let change = typed(tracker.observe(&field("f"), &ctx("hi", ""), TriggerPolicy::Manual, 7));
        assert_eq!(change.trigger, TriggerPolicy::Manual);
        assert_eq!(change.now_ms, 7);
    }

    #[test]
    fn caret_move_within_unchanged_value_is_caret_moved() {
        let mut tracker = FieldTracker::new();
        // First read establishes "hello" with caret at 3.
        let first = tracker.observe(&field("f"), &ctx("hel", "lo"), TriggerPolicy::Automatic, 0);
        assert!(matches!(first, Observation::Typed(_)));
        // Same content, caret now at 5 → pure caret move, not a text change.
        let second = tracker.observe(&field("f"), &ctx("hello", ""), TriggerPolicy::Automatic, 1);
        assert_eq!(
            second,
            Observation::CaretMoved {
                field: field("f"),
                caret: 5
            }
        );
    }

    #[test]
    fn typing_after_a_caret_move_is_typed_again() {
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx("hel", "lo"), TriggerPolicy::Automatic, 0);
        tracker.observe(&field("f"), &ctx("hello", ""), TriggerPolicy::Automatic, 1);
        let change =
            typed(tracker.observe(&field("f"), &ctx("hello!", ""), TriggerPolicy::Automatic, 2));
        assert_eq!(change.edit, EditKind::Insert);
        assert_eq!(change.value, "hello!");
    }

    #[test]
    fn self_insert_updates_baseline_so_readback_is_caret_move() {
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx("hello ", ""), TriggerPolicy::Automatic, 0);

        tracker.apply_self_insert(&field("f"), "world ");
        let observed = tracker.observe(
            &field("f"),
            &ctx("hello world ", ""),
            TriggerPolicy::Automatic,
            1,
        );

        assert_eq!(
            observed,
            Observation::CaretMoved {
                field: field("f"),
                caret: 12
            }
        );
    }

    #[test]
    fn self_insert_uses_char_caret_for_unicode_prefix() {
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx("hé", "llo"), TriggerPolicy::Automatic, 0);

        tracker.apply_self_insert(&field("f"), "!");
        let observed =
            tracker.observe(&field("f"), &ctx("hé!", "llo"), TriggerPolicy::Automatic, 1);

        assert_eq!(
            observed,
            Observation::CaretMoved {
                field: field("f"),
                caret: 3
            }
        );
    }

    #[test]
    fn self_replace_deletes_then_inserts_so_readback_is_caret_move() {
        // User typed ":smile" after "x" (caret at 7); accepting the emoji
        // replacement deletes those 6 chars and inserts the glyph. The baseline
        // must end at "x😄" so the AX readback is absorbed as a caret move.
        let mut tracker = FieldTracker::new();
        tracker.observe(
            &field("f"),
            &ctx("x:smile", ""),
            TriggerPolicy::Automatic,
            0,
        );
        tracker.apply_self_replace(&field("f"), "😄", 6);
        let observed = tracker.observe(&field("f"), &ctx("x😄", ""), TriggerPolicy::Automatic, 1);
        assert_eq!(
            observed,
            Observation::CaretMoved {
                field: field("f"),
                caret: 2
            }
        );
    }

    #[test]
    fn self_replace_clamps_replace_left_to_available_chars() {
        // `replace_left` larger than the caret position clamps to what's there.
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx(":1", ""), TriggerPolicy::Automatic, 0);
        tracker.apply_self_replace(&field("f"), "👍", 99);
        let observed = tracker.observe(&field("f"), &ctx("👍", ""), TriggerPolicy::Automatic, 1);
        assert_eq!(
            observed,
            Observation::CaretMoved {
                field: field("f"),
                caret: 1
            }
        );
    }

    #[test]
    fn self_replace_noops_on_wrong_field() {
        let mut tracker = FieldTracker::new();
        tracker.observe(
            &field("f"),
            &ctx("x:smile", ""),
            TriggerPolicy::Automatic,
            0,
        );
        tracker.apply_self_replace(&field("other"), "😄", 6);
        // Baseline unchanged → the real field's next readback is unaffected.
        let observed = tracker.observe(
            &field("f"),
            &ctx("x:smile", ""),
            TriggerPolicy::Automatic,
            1,
        );
        assert_eq!(
            observed,
            Observation::CaretMoved {
                field: field("f"),
                caret: 7
            }
        );
    }

    #[test]
    fn self_replace_noops_without_baseline() {
        // No observation yet → no baseline → apply_self_replace is a no-op (no
        // panic), and the first real observation still reads as a fresh Insert.
        let mut tracker = FieldTracker::new();
        tracker.apply_self_replace(&field("f"), "😄", 2);
        let first =
            typed(tracker.observe(&field("f"), &ctx("hi", ""), TriggerPolicy::Automatic, 1));
        assert_eq!(first.edit, EditKind::Insert);
    }

    #[test]
    fn self_insert_noops_without_matching_baseline() {
        let mut tracker = FieldTracker::new();
        tracker.apply_self_insert(&field("f"), "ignored");
        let first =
            typed(tracker.observe(&field("f"), &ctx("hi", ""), TriggerPolicy::Automatic, 1));
        assert_eq!(first.edit, EditKind::Insert);

        tracker.apply_self_insert(&field("other"), "ignored");
        let after_wrong_field =
            tracker.observe(&field("f"), &ctx("hi", ""), TriggerPolicy::Automatic, 2);
        assert_eq!(
            after_wrong_field,
            Observation::CaretMoved {
                field: field("f"),
                caret: 2
            }
        );

        tracker.apply_self_insert(&field("f"), "");
        let after_empty_insert =
            tracker.observe(&field("f"), &ctx("hi", ""), TriggerPolicy::Automatic, 3);
        assert_eq!(
            after_empty_insert,
            Observation::CaretMoved {
                field: field("f"),
                caret: 2
            }
        );
    }

    // --- Text-indexing: scalar (char) engine offsets vs UTF-16 AX offsets ---
    // The adapter slices left/right by UTF-16 code units and reports a UTF-16
    // `ctx.caret`; `value_and_caret` ignores that and re-derives a scalar caret
    // from `left.chars().count()`. These pin that boundary for the astral /
    // grapheme cases the plan flagged as unverified (emoji, skin-tone, CJK,
    // combining marks) — each is a place a UTF-16 index would diverge from a
    // scalar index.

    #[test]
    fn caret_is_scalar_counted_for_astral_emoji_left() {
        // "a😀" is 2 Unicode scalars but 3 UTF-16 code units (😀 is a surrogate
        // pair). The engine caret must be the scalar count (2), not 3.
        let mut tracker = FieldTracker::new();
        let change =
            typed(tracker.observe(&field("f"), &ctx("a😀", "b"), TriggerPolicy::Automatic, 0));
        assert_eq!(change.caret, 2);
        assert_eq!(change.value, "a😀b");
    }

    #[test]
    fn selection_and_adapter_offset_metadata_do_not_change_reconstructed_text() {
        let mut ctx = ctx("a😀", "b");
        ctx.selection = Some(TextRange { start: 1, end: 3 });
        ctx.caret = 3;
        ctx.offset_encoding = OffsetEncoding::Utf16CodeUnits;

        let mut tracker = FieldTracker::new();
        let change = typed(tracker.observe(&field("f"), &ctx, TriggerPolicy::Automatic, 0));

        assert_eq!(change.value, "a😀b");
        assert_eq!(change.caret, 2);
    }

    #[test]
    fn caret_is_scalar_counted_for_cjk_left() {
        // CJK ideographs are 1 scalar each (3 UTF-8 bytes, 1 UTF-16 unit).
        let mut tracker = FieldTracker::new();
        let change =
            typed(tracker.observe(&field("f"), &ctx("日本", "語"), TriggerPolicy::Automatic, 0));
        assert_eq!(change.caret, 2);
        assert_eq!(change.value, "日本語");
    }

    #[test]
    fn caret_counts_skin_tone_emoji_as_two_scalars() {
        // "👍🏽" is one grapheme but two scalars (base + skin-tone modifier),
        // four UTF-16 units. Scalar counting yields 2 for the emoji plus 1 for
        // the trailing space = 3.
        let mut tracker = FieldTracker::new();
        let change =
            typed(tracker.observe(&field("f"), &ctx("👍🏽 ", "x"), TriggerPolicy::Automatic, 0));
        assert_eq!(change.caret, 3);
        assert_eq!(change.value, "👍🏽 x");
    }

    #[test]
    fn caret_counts_combining_accent_as_two_scalars() {
        // "e" + U+0301 (combining acute) is one grapheme, two scalars.
        let mut tracker = FieldTracker::new();
        let change = typed(tracker.observe(
            &field("f"),
            &ctx("e\u{0301}", "x"),
            TriggerPolicy::Automatic,
            0,
        ));
        assert_eq!(change.caret, 2);
        assert_eq!(change.value, "e\u{0301}x");
    }

    #[test]
    fn self_insert_uses_scalar_caret_after_astral_prefix() {
        // Self-insert splices at a scalar caret over an astral prefix; the byte
        // index must land after the 4-byte emoji, not mid-surrogate.
        let mut tracker = FieldTracker::new();
        tracker.observe(&field("f"), &ctx("👋", "tail"), TriggerPolicy::Automatic, 0);

        tracker.apply_self_insert(&field("f"), "🎉");
        let observed = tracker.observe(
            &field("f"),
            &ctx("👋🎉", "tail"),
            TriggerPolicy::Automatic,
            1,
        );

        assert_eq!(
            observed,
            Observation::CaretMoved {
                field: field("f"),
                caret: 2
            }
        );
    }

    #[test]
    fn byte_index_for_char_lands_on_scalar_boundaries() {
        // Direct check that scalar caret → byte index never splits a multi-byte
        // scalar, across CJK and astral text.
        assert_eq!(byte_index_for_char("日本語", 0), 0);
        assert_eq!(byte_index_for_char("日本語", 1), 3);
        assert_eq!(byte_index_for_char("日本語", 2), 6);
        assert_eq!(byte_index_for_char("日本語", 3), 9); // past end → len
                                                         // "a😀b": a=1 byte, 😀=4 bytes, b=1 byte.
        assert_eq!(byte_index_for_char("a😀b", 1), 1);
        assert_eq!(byte_index_for_char("a😀b", 2), 5);
    }

    fn request(generation: u64) -> CompletionRequest {
        CompletionRequest {
            generation,
            field: field("f"),
            domain: None,
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

    #[test]
    fn take_on_empty_slot_is_none() {
        let mut latest = LatestRequest::new();
        assert!(latest.take().is_none());
    }

    #[test]
    fn clear_drops_pending_request() {
        let mut latest = LatestRequest::new();
        latest.offer(request(1));
        latest.clear();
        assert!(latest.take().is_none());
    }

    #[test]
    fn equal_generation_request_overwrites() {
        // offer uses `>=`, so a fresh request of the same generation replaces the
        // pending one (a re-read of the same snapshot wins).
        let mut latest = LatestRequest::new();
        latest.offer(CompletionRequest {
            prompt: "first".into(),
            ..request(2)
        });
        latest.offer(CompletionRequest {
            prompt: "second".into(),
            ..request(2)
        });
        assert_eq!(latest.take().unwrap().prompt, "second");
    }
}
