//! Deterministic suggestion state machine.

use context::{left_context, right_context, trim_trailing};
use platform::{ux_mode, AcceptAction, Capabilities, FieldHandle, UxMode};
use ranker::{
    cap_words, is_degenerate_repetition, next_word, repetition_penalty, strip_suffix_overlap,
    trim_to_stop_boundary, truncate_at_sentence_end,
};

pub type SnapshotId = u64;

/// Completions whose repetition penalty falls below this floor (i.e. they echo
/// text already to the left of the caret) are dropped rather than shown.
const REPETITION_PENALTY_FLOOR: f64 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    Insert,
    Delete,
    Paste,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerPolicy {
    Automatic,
    Manual,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Focus {
        field: FieldHandle,
        caps: Capabilities,
    },
    TextChanged {
        field: FieldHandle,
        value: String,
        caret: usize,
        edit: EditKind,
        previous_caret: Option<usize>,
        previous_value_hash: Option<u64>,
        trigger: TriggerPolicy,
        now_ms: u64,
    },
    CaretMoved {
        field: FieldHandle,
        caret: usize,
    },
    Tick {
        now_ms: u64,
    },
    CompletionReady {
        generation: u64,
        field: FieldHandle,
        snapshot: SnapshotId,
        text: String,
    },
    /// Multiple candidate continuations for one request (multi-candidate, A2
    /// §16). The first is the primary; `Cycle` rotates through the rest.
    CompletionReadyMulti {
        generation: u64,
        field: FieldHandle,
        snapshot: SnapshotId,
        candidates: Vec<String>,
    },
    /// Rotate to the next candidate while a suggestion is showing.
    Cycle,
    SecureStateChanged {
        caps: Capabilities,
    },
    AcceptFull,
    AcceptWord,
    Dismiss,
    /// Esc: hide any showing ghost AND suppress new completions in the current
    /// field until the user refocuses or makes another edit (Cotypist parity).
    DismissSuppress,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    RequestCompletion {
        generation: u64,
        field: FieldHandle,
        snapshot: SnapshotId,
        prompt: String,
    },
    ShowGhost {
        field: FieldHandle,
        snapshot: SnapshotId,
        text: String,
    },
    UpdateGhost {
        field: FieldHandle,
        snapshot: SnapshotId,
        text: String,
    },
    Hide,
    Insert {
        field: FieldHandle,
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Showing {
    field: FieldHandle,
    snapshot: SnapshotId,
    /// Shaped candidate continuations; `index` selects the one on screen.
    candidates: Vec<String>,
    index: usize,
    caret: usize,
}

impl Showing {
    fn current(&self) -> &str {
        &self.candidates[self.index]
    }
}

pub struct SuggestionMachine {
    caps: Capabilities,
    debounce_ms: u64,
    max_words: usize,
    min_context_chars: usize,
    allow_mid_word: bool,
    generation: u64,
    snapshot: SnapshotId,
    field: Option<FieldHandle>,
    value: String,
    caret: usize,
    pending_since: Option<u64>,
    requested: Option<RequestedCompletion>,
    showing: Option<Showing>,
    /// Set by `DismissSuppress` (Esc); blocks completions in the current field
    /// until cleared by a refocus or the next edit.
    suppressed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RequestedCompletion {
    generation: u64,
    field: FieldHandle,
    snapshot: SnapshotId,
}

impl SuggestionMachine {
    pub fn new(caps: Capabilities, debounce_ms: u64, max_words: usize) -> Self {
        Self {
            caps,
            debounce_ms,
            max_words,
            // Permissive defaults: no minimum context, mid-word allowed. Callers
            // opt into conservative triggering via `with_trigger_gates`.
            min_context_chars: 0,
            allow_mid_word: true,
            generation: 0,
            snapshot: 0,
            field: None,
            value: String::new(),
            caret: 0,
            pending_since: None,
            requested: None,
            showing: None,
            suppressed: false,
        }
    }

    /// Configure conservative trigger gating (spec §4, "protect first-run"):
    /// require at least `min_context_chars` of trimmed left context before
    /// requesting, and (unless `allow_mid_word`) suppress requests when the caret
    /// splits a word. Defaults are permissive so existing callers are unaffected.
    pub fn with_trigger_gates(mut self, min_context_chars: usize, allow_mid_word: bool) -> Self {
        self.min_context_chars = min_context_chars;
        self.allow_mid_word = allow_mid_word;
        self
    }

    fn enabled(&self) -> bool {
        matches!(ux_mode(&self.caps), UxMode::Inline | UxMode::Popup)
    }

    /// Whether the current value/caret passes the conservative trigger gates:
    /// enough left context, and not mid-word unless configured otherwise.
    fn passes_trigger_gates(&self) -> bool {
        let left = left_context(&self.value, self.caret);
        // Minimum context: count only substantive characters — leading and
        // trailing whitespace must not satisfy the minimum.
        if left.trim().chars().count() < self.min_context_chars {
            return false;
        }
        // Mid-word: the caret splits a word only when the characters on *both*
        // sides are word characters. A caret at a word boundary (after a space,
        // at the start of a word, or at end-of-text) is not mid-word.
        if !self.allow_mid_word {
            let is_word = |c: char| c.is_alphanumeric() || c == '_';
            let left_is_word = left.chars().next_back().is_some_and(is_word);
            let right = right_context(&self.value, self.caret);
            let right_is_word = right.chars().next().is_some_and(is_word);
            if left_is_word && right_is_word {
                return false;
            }
        }
        true
    }

    fn hide_if_showing(&mut self, out: &mut Vec<Command>) {
        if self.showing.take().is_some() {
            out.push(Command::Hide);
        }
    }

    fn advance_snapshot(&mut self) {
        self.generation += 1;
        self.snapshot += 1;
        self.requested = None;
    }

    pub fn on_event(&mut self, event: Event) -> Vec<Command> {
        let mut out = Vec::new();

        match event {
            Event::Focus { field, caps } => {
                self.hide_if_showing(&mut out);
                self.caps = caps;
                self.field = Some(field);
                self.value.clear();
                self.caret = 0;
                self.pending_since = None;
                self.suppressed = false;
                self.advance_snapshot();
            }
            Event::TextChanged {
                field,
                value,
                caret,
                edit,
                previous_caret: _,
                previous_value_hash: _,
                trigger,
                now_ms,
            } => {
                self.hide_if_showing(&mut out);
                self.field = Some(field);
                self.value = value;
                self.caret = caret;
                self.advance_snapshot();
                // An edit clears Esc-suppression, but the clearing edit is itself
                // still gated — triggering resumes on the edit after it.
                let was_suppressed = self.suppressed;
                self.suppressed = false;
                self.pending_since = if !was_suppressed
                    && edit != EditKind::Delete
                    && self.enabled()
                    && trigger == TriggerPolicy::Automatic
                    && self.passes_trigger_gates()
                {
                    Some(now_ms)
                } else {
                    None
                };
            }
            Event::Tick { now_ms } => {
                if let (Some(since), Some(field)) = (self.pending_since, self.field.clone()) {
                    if self.enabled() && now_ms.saturating_sub(since) >= self.debounce_ms {
                        let generation = self.generation;
                        let snapshot = self.snapshot;
                        let prompt =
                            trim_trailing(&left_context(&self.value, self.caret)).to_string();
                        self.requested = Some(RequestedCompletion {
                            generation,
                            field: field.clone(),
                            snapshot,
                        });
                        self.pending_since = None;
                        out.push(Command::RequestCompletion {
                            generation,
                            field,
                            snapshot,
                            prompt,
                        });
                    }
                }
            }
            Event::CompletionReady {
                generation,
                field,
                snapshot,
                text,
            } => {
                self.on_completion_ready(generation, &field, snapshot, vec![text], &mut out);
            }
            Event::CompletionReadyMulti {
                generation,
                field,
                snapshot,
                candidates,
            } => {
                self.on_completion_ready(generation, &field, snapshot, candidates, &mut out);
            }
            Event::Cycle => {
                if let Some(showing) = self.showing.as_mut() {
                    if showing.candidates.len() > 1 {
                        showing.index = (showing.index + 1) % showing.candidates.len();
                        out.push(Command::UpdateGhost {
                            field: showing.field.clone(),
                            snapshot: showing.snapshot,
                            text: showing.current().to_string(),
                        });
                    }
                }
            }
            Event::CaretMoved { field, caret } => {
                let moved = self
                    .showing
                    .as_ref()
                    .is_some_and(|showing| showing.field != field || showing.caret != caret);
                if moved {
                    self.hide_if_showing(&mut out);
                    self.advance_snapshot();
                }
                self.field = Some(field);
                self.caret = caret;
            }
            Event::SecureStateChanged { caps } => {
                self.hide_if_showing(&mut out);
                self.caps = caps;
                self.pending_since = None;
                self.advance_snapshot();
            }
            Event::Dismiss => {
                self.hide_if_showing(&mut out);
            }
            Event::DismissSuppress => {
                self.hide_if_showing(&mut out);
                self.suppressed = true;
                self.pending_since = None;
                // Stale any in-flight request so its completion cannot pop a
                // ghost back up after the dismiss.
                self.advance_snapshot();
            }
            Event::AcceptFull => {
                if let Some(showing) = self.showing.take() {
                    out.push(Command::Insert {
                        field: showing.field,
                        text: showing.candidates[showing.index].clone(),
                    });
                    out.push(Command::Hide);
                    self.advance_snapshot();
                }
            }
            Event::AcceptWord => {
                if let Some(mut showing) = self.showing.take() {
                    let (word, rest) = next_word(showing.current());
                    out.push(Command::Insert {
                        field: showing.field.clone(),
                        text: word.clone(),
                    });
                    if rest.is_empty() {
                        out.push(Command::Hide);
                        self.advance_snapshot();
                    } else {
                        showing.caret += word.chars().count();
                        // Collapse to the active candidate: the siblings still
                        // begin with the just-accepted word and would re-offer it
                        // on cycle, so once the user commits word-by-word the
                        // alternatives are dropped.
                        showing.candidates = vec![rest.clone()];
                        showing.index = 0;
                        out.push(Command::UpdateGhost {
                            field: showing.field.clone(),
                            snapshot: showing.snapshot,
                            text: rest,
                        });
                        self.showing = Some(showing);
                    }
                }
            }
        }

        out
    }

    /// Shape raw candidates into inline offerings and, if any survive, show the
    /// first. Shared by the single (`CompletionReady`) and multi
    /// (`CompletionReadyMulti`) paths. Shaping: cut at the first line break, then
    /// the first sentence end, drop any tail that re-states text after the caret,
    /// cap the word count; drop empty/echoing/degenerate candidates and exact
    /// duplicates.
    fn on_completion_ready(
        &mut self,
        generation: u64,
        field: &FieldHandle,
        snapshot: SnapshotId,
        raw_candidates: Vec<String>,
        out: &mut Vec<Command>,
    ) {
        // No explicit `suppressed` check is needed here: `DismissSuppress`
        // advances the snapshot (staling any in-flight request) and clears
        // `requested`, and a suppressed field cannot arm a fresh request
        // (`TextChanged` clears suppression before arming), so no matching
        // completion can arrive while suppressed.
        let matches_request = self.requested.as_ref().is_some_and(|requested| {
            requested.generation == generation
                && requested.snapshot == snapshot
                && requested.field == *field
                && generation == self.generation
                && snapshot == self.snapshot
        });
        if !matches_request {
            return;
        }

        let right = right_context(&self.value, self.caret);
        let recent = left_context(&self.value, self.caret);
        let mut shaped: Vec<String> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for raw in raw_candidates {
            let line = trim_to_stop_boundary(&raw);
            let sentence = truncate_at_sentence_end(line);
            let de_overlapped = strip_suffix_overlap(sentence, &right);
            let capped = cap_words(&de_overlapped, self.max_words);
            let fresh = repetition_penalty(&capped, &recent) >= REPETITION_PENALTY_FLOOR
                && !is_degenerate_repetition(&capped);
            // Dedup on a normalized key (trim + case-fold) so near-duplicates
            // (trailing space, case) don't show as separate cycle options.
            let key = capped.trim().to_lowercase();
            if !capped.is_empty() && fresh && !seen.contains(&key) {
                seen.push(key);
                shaped.push(capped);
            }
        }

        if let Some(first) = shaped.first().cloned() {
            self.showing = Some(Showing {
                field: field.clone(),
                snapshot,
                candidates: shaped,
                index: 0,
                caret: self.caret,
            });
            out.push(Command::ShowGhost {
                field: field.clone(),
                snapshot,
                text: first,
            });
        }
        self.requested = None;
    }

    pub fn preview_accept_insert(&self, action: AcceptAction) -> Option<(FieldHandle, String)> {
        let showing = self.showing.as_ref()?;
        let text = match action {
            AcceptAction::Full => showing.current().to_string(),
            AcceptAction::Word => next_word(showing.current()).0,
        };
        (!text.is_empty()).then(|| (showing.field.clone(), text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::{InsertStrategy, KeyInterceptMode, OverlayPlacement, SecurityState, Toolkit};

    fn field(id: &str) -> FieldHandle {
        FieldHandle {
            app: "TextEdit".into(),
            pid: Some(42),
            element_id: id.into(),
            generation: 1,
        }
    }

    fn inline_caps() -> Capabilities {
        Capabilities {
            readable_text: true,
            readable_caret: true,
            writable: true,
            secure: false,
            security_state: SecurityState::Normal,
            toolkit: Toolkit::AppKit,
            multiline: true,
            insert_strategy: InsertStrategy::AxSet,
            accept_intercept: KeyInterceptMode::CgEventTap,
            overlay_at_caret: OverlayPlacement::NativePanel,
            coords_global_screen: true,
        }
    }

    fn secure_caps() -> Capabilities {
        let mut caps = inline_caps();
        caps.secure = true;
        caps.security_state = SecurityState::SecureField;
        caps
    }

    fn popup_caps() -> Capabilities {
        // No caret geometry → ux_mode derives Popup (not Inline). enabled()
        // accepts both, so this exercises the Popup arm of the predicate.
        let mut caps = inline_caps();
        caps.readable_caret = false;
        caps.overlay_at_caret = OverlayPlacement::None;
        caps
    }

    fn machine() -> SuggestionMachine {
        SuggestionMachine::new(inline_caps(), 200, 4)
    }

    fn text_changed(value: &str, caret: usize, now_ms: u64) -> Event {
        Event::TextChanged {
            field: field("field-a"),
            value: value.into(),
            caret,
            edit: EditKind::Insert,
            previous_caret: None,
            previous_value_hash: None,
            trigger: TriggerPolicy::Automatic,
            now_ms,
        }
    }

    #[test]
    fn no_request_when_context_below_min() {
        // min_context_chars=3; "hi " trims to "hi" (2 chars) < 3 → never arms.
        let mut machine = machine().with_trigger_gates(3, false);
        machine.on_event(text_changed("hi ", 3, 1000));
        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn requests_when_context_meets_min() {
        // "hey " trims to "hey" (3 chars) == min → arms and fires.
        let mut machine = machine().with_trigger_gates(3, false);
        machine.on_event(text_changed("hey ", 4, 1000));
        assert!(machine
            .on_event(Event::Tick { now_ms: 1300 })
            .iter()
            .any(|c| matches!(c, Command::RequestCompletion { .. })));
    }

    #[test]
    fn no_request_mid_word() {
        // Caret at 3 inside "hello" → right context "lo world" starts with an
        // alphanumeric char → mid-word → suppressed when allow_mid_word=false.
        let mut machine = machine().with_trigger_gates(0, false);
        machine.on_event(text_changed("hello world", 3, 1000));
        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn requests_at_word_boundary() {
        // Caret at 5 (after "hello", before the space) → right " world" starts
        // with a non-word char → not mid-word → arms.
        let mut machine = machine().with_trigger_gates(0, false);
        machine.on_event(text_changed("hello world", 5, 1000));
        assert!(machine
            .on_event(Event::Tick { now_ms: 1300 })
            .iter()
            .any(|c| matches!(c, Command::RequestCompletion { .. })));
    }

    #[test]
    fn requests_at_end_of_text() {
        // Caret at end → right context empty → not mid-word → arms.
        let mut machine = machine().with_trigger_gates(0, false);
        machine.on_event(text_changed("hello", 5, 1000));
        assert!(machine
            .on_event(Event::Tick { now_ms: 1300 })
            .iter()
            .any(|c| matches!(c, Command::RequestCompletion { .. })));
    }

    #[test]
    fn caret_at_word_start_is_not_mid_word() {
        // Caret at 4 in "foo bar": left "foo " ends in a space, right "bar"
        // starts a word. The caret is at a word *boundary* (start of "bar"), not
        // splitting a word, so it must arm even with mid-word suppression on.
        let mut machine = machine().with_trigger_gates(0, false);
        machine.on_event(text_changed("foo bar", 4, 1000));
        assert!(machine
            .on_event(Event::Tick { now_ms: 1300 })
            .iter()
            .any(|c| matches!(c, Command::RequestCompletion { .. })));
    }

    #[test]
    fn leading_whitespace_does_not_count_toward_min_context() {
        // "  ab" has 4 left-context chars but only 2 of substance. min=3 must
        // suppress (leading whitespace must not satisfy the minimum).
        let mut machine = machine().with_trigger_gates(3, false);
        machine.on_event(text_changed("  ab", 4, 1000));
        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn trailing_whitespace_does_not_count_toward_min_context() {
        // "ab  " has 4 left-context chars but trims to "ab" (2) < 3 → suppress.
        let mut machine = machine().with_trigger_gates(3, false);
        machine.on_event(text_changed("ab  ", 4, 1000));
        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn underscore_counts_as_a_word_char_for_mid_word() {
        // Caret at 4 in "foo_bar": left ends in '_', right starts with 'b' —
        // both are word chars, so the caret splits an identifier → suppressed.
        let mut machine = machine().with_trigger_gates(0, false);
        machine.on_event(text_changed("foo_bar", 4, 1000));
        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn non_ascii_letters_count_as_word_chars_for_mid_word() {
        // CJK ideographs are alphanumeric: caret inside "日本語" splits a word.
        let mut machine = machine().with_trigger_gates(0, false);
        machine.on_event(text_changed("日本語", 1, 1000));
        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn mid_word_allowed_when_configured() {
        // Same mid-word caret, but allow_mid_word=true → arms anyway.
        let mut machine = machine().with_trigger_gates(0, true);
        machine.on_event(text_changed("hello world", 3, 1000));
        assert!(machine
            .on_event(Event::Tick { now_ms: 1300 })
            .iter()
            .any(|c| matches!(c, Command::RequestCompletion { .. })));
    }

    #[test]
    fn default_machine_has_no_trigger_gates() {
        // new() leaves gates permissive (min 0, mid-word allowed) so existing
        // callers are unaffected; a 1-char mid-word context still arms.
        let mut machine = machine();
        machine.on_event(text_changed("ab", 1, 1000));
        assert!(machine
            .on_event(Event::Tick { now_ms: 1300 })
            .iter()
            .any(|c| matches!(c, Command::RequestCompletion { .. })));
    }

    #[test]
    fn popup_mode_arms_and_shows_like_inline() {
        // enabled() accepts UxMode::Popup as well as Inline; a popup-capable
        // field must still arm a request and show a ghost.
        let mut machine = SuggestionMachine::new(popup_caps(), 200, 4);
        machine.on_event(text_changed("hello ", 6, 1000));
        let armed = machine.on_event(Event::Tick { now_ms: 1200 });
        assert!(armed
            .iter()
            .any(|c| matches!(c, Command::RequestCompletion { .. })));

        let shown = machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            text: "world".into(),
        });
        assert!(shown.iter().any(|c| matches!(c, Command::ShowGhost { .. })));
    }

    #[test]
    fn focus_cancels_a_request_armed_before_it() {
        // A request armed by typing must be cancelled when focus moves away
        // before the debounce fires (Focus clears value/caret/pending_since).
        let mut machine = machine();
        machine.on_event(text_changed("hello ", 6, 1000)); // arms pending_since
        machine.on_event(Event::Focus {
            field: field("field-b"),
            caps: inline_caps(),
        });
        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn requests_completion_after_debounce() {
        let mut machine = machine();

        assert_eq!(machine.on_event(text_changed("hello ", 6, 1000)), vec![]);
        assert_eq!(machine.on_event(Event::Tick { now_ms: 1100 }), vec![]);
        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 1200 }),
            vec![Command::RequestCompletion {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                prompt: "hello".into(),
            }]
        );
    }

    #[test]
    fn delete_edit_does_not_trigger_request() {
        // After a Delete edit, even past the debounce window, no RequestCompletion
        // command should be emitted.
        let mut machine = machine();

        machine.on_event(Event::TextChanged {
            field: field("field-a"),
            value: "hell".into(),
            caret: 4,
            edit: EditKind::Delete,
            previous_caret: Some(5),
            previous_value_hash: Some(123),
            trigger: TriggerPolicy::Automatic,
            now_ms: 1000,
        });

        // Tick well past the debounce window — must not emit RequestCompletion.
        let cmds = machine.on_event(Event::Tick { now_ms: 2000 });
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, Command::RequestCompletion { .. })),
            "expected no RequestCompletion after a Delete edit, got: {cmds:?}"
        );
    }

    #[test]
    fn paste_edit_triggers_request_like_insert() {
        // A paste (Cmd+V) is a non-Delete edit and must arm a completion the
        // same way typing does.
        let mut machine = machine();

        machine.on_event(Event::TextChanged {
            field: field("field-a"),
            value: "pasted text ".into(),
            caret: 12,
            edit: EditKind::Paste,
            previous_caret: Some(0),
            previous_value_hash: Some(1),
            trigger: TriggerPolicy::Automatic,
            now_ms: 1000,
        });

        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 1200 }),
            vec![Command::RequestCompletion {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                prompt: "pasted text".into(),
            }]
        );
    }

    #[test]
    fn unknown_edit_triggers_request() {
        // The trigger gate keys off "not a Delete", so an Unknown edit still
        // arms a request — this pins the gate against regressing to `== Insert`.
        let mut machine = machine();

        machine.on_event(Event::TextChanged {
            field: field("field-a"),
            value: "typed ".into(),
            caret: 6,
            edit: EditKind::Unknown,
            previous_caret: None,
            previous_value_hash: None,
            trigger: TriggerPolicy::Automatic,
            now_ms: 1000,
        });

        let cmds = machine.on_event(Event::Tick { now_ms: 1200 });
        assert!(
            cmds.iter()
                .any(|c| matches!(c, Command::RequestCompletion { .. })),
            "expected RequestCompletion after an Unknown edit, got: {cmds:?}"
        );
    }

    #[test]
    fn secure_state_change_clears_pending_request() {
        // A secure-state flip arriving before debounce must cancel the pending
        // request. Caps stay enabled here so this isolates pending-clearing
        // from the separate "secure field disables requests" path.
        let mut machine = machine();
        machine.on_event(text_changed("hello ", 6, 1000));

        machine.on_event(Event::SecureStateChanged {
            caps: inline_caps(),
        });

        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn manual_trigger_does_not_auto_request() {
        let mut machine = machine();

        machine.on_event(Event::TextChanged {
            field: field("field-a"),
            value: "hello ".into(),
            caret: 6,
            edit: EditKind::Insert,
            previous_caret: Some(5),
            previous_value_hash: Some(123),
            trigger: TriggerPolicy::Manual,
            now_ms: 1000,
        });

        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn shows_ghost_on_matching_completion() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "a b c d e".into(),
            }),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "a b c d".into(),
            }]
        );
    }

    #[test]
    fn shows_only_first_line_of_multiline_completion() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "inline tail\n- bullet\n- bullet".into(),
            }),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "inline tail".into(),
            }]
        );
    }

    #[test]
    fn suppresses_completion_that_repeats_recent_text() {
        let mut machine = machine();
        machine.on_event(text_changed("please repeat me ", 16, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "repeat me".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn truncates_completion_at_sentence_end() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "first done. second thing".into(),
            }),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "first done.".into(),
            }]
        );
    }

    #[test]
    fn strips_completion_overlap_with_text_after_caret() {
        // Caret sits after "the quick" (9 chars) in "the quick fox"; the model
        // regurgitates the trailing " fox", which must be stripped before showing.
        let mut machine = machine();
        machine.on_event(text_changed("the quick fox", 9, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "quick brown fox".into(),
            }),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "quick brown".into(),
            }]
        );
    }

    #[test]
    fn suppresses_completion_fully_overlapping_text_after_caret() {
        // Caret after "the quick" (9 chars); the model echoes exactly the text
        // already to the right (" fox"). Stripping the overlap empties the
        // candidate, so nothing is shown.
        let mut machine = machine();
        machine.on_event(text_changed("the quick fox", 9, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "fox".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn applies_sentence_stop_and_overlap_strip_together() {
        // One completion needs both shapers: cut at the sentence end ("done."),
        // then strip the trailing " fox" already after the caret.
        let mut machine = machine();
        machine.on_event(text_changed("the quick fox", 9, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "brown fox. extra sentence".into(),
            }),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "brown".into(),
            }]
        );
    }

    #[test]
    fn suppresses_degenerate_repetition_created_by_word_cap() {
        // "na na na na ma" is NOT degenerate as-is (5 words), but capping to the
        // 4-word max yields "na na na na" — a loop. The degenerate check runs
        // AFTER cap_words, so the capped loop is still suppressed.
        let mut machine = machine();
        machine.on_event(text_changed("z", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "na na na na ma".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn suppresses_degenerate_repetition_completion() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "ha ha ha".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn discards_stale_completion() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(text_changed("xy", 2, 600));

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "stale".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn discards_completion_for_wrong_field() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-b"),
                snapshot: 1,
                text: "wrong field".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn discards_completion_after_secure_state_advances_boundary() {
        // A request is in flight (gen/snap = 1). A secure-state change advances
        // the boundary; the completion tagged with the now-stale gen/snap must be
        // discarded — distinct stale-race site from text/focus changes.
        let mut machine = machine();
        machine.on_event(text_changed("hello ", 6, 1000));
        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 1200 }),
            vec![Command::RequestCompletion {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                prompt: "hello".into(),
            }]
        );

        machine.on_event(Event::SecureStateChanged {
            caps: inline_caps(),
        });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "world".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn transition_to_secure_blocks_subsequent_requests() {
        // Field starts Normal, then a secure-state change flips caps to secure.
        // Typing afterwards must never arm a request (privacy invariant, §7).
        let mut machine = machine();
        machine.on_event(Event::SecureStateChanged {
            caps: secure_caps(),
        });
        machine.on_event(text_changed("password", 8, 1000));

        assert_eq!(machine.on_event(Event::Tick { now_ms: 9999 }), vec![]);
    }

    #[test]
    fn secure_field_never_requests() {
        let mut machine = SuggestionMachine::new(secure_caps(), 200, 4);
        machine.on_event(text_changed("pw", 2, 0));

        assert_eq!(machine.on_event(Event::Tick { now_ms: 9999 }), vec![]);
    }

    fn showing_machine() -> SuggestionMachine {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            text: "a b".into(),
        });
        machine
    }

    #[test]
    fn text_change_while_showing_hides() {
        let mut machine = showing_machine();

        assert_eq!(
            machine.on_event(text_changed("xy", 2, 600)),
            vec![Command::Hide]
        );
    }

    #[test]
    fn caret_move_while_showing_hides() {
        let mut machine = showing_machine();

        assert_eq!(
            machine.on_event(Event::CaretMoved {
                field: field("field-a"),
                caret: 9,
            }),
            vec![Command::Hide]
        );
    }

    #[test]
    fn caret_move_to_different_field_hides() {
        let mut machine = showing_machine();

        assert_eq!(
            machine.on_event(Event::CaretMoved {
                field: field("field-b"),
                caret: 1,
            }),
            vec![Command::Hide]
        );
    }

    #[test]
    fn caret_move_same_position_keeps_showing() {
        let mut machine = showing_machine();

        assert_eq!(
            machine.on_event(Event::CaretMoved {
                field: field("field-a"),
                caret: 1,
            }),
            vec![]
        );
    }

    #[test]
    fn focus_change_while_showing_hides() {
        let mut machine = showing_machine();

        assert_eq!(
            machine.on_event(Event::Focus {
                field: field("field-b"),
                caps: inline_caps(),
            }),
            vec![Command::Hide]
        );
    }

    #[test]
    fn secure_state_change_while_showing_hides() {
        let mut machine = showing_machine();

        assert_eq!(
            machine.on_event(Event::SecureStateChanged {
                caps: secure_caps(),
            }),
            vec![Command::Hide]
        );
    }

    #[test]
    fn dismiss_hides() {
        let mut machine = showing_machine();

        assert_eq!(machine.on_event(Event::Dismiss), vec![Command::Hide]);
    }

    fn showing_candidates(texts: &[&str]) -> SuggestionMachine {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReadyMulti {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            candidates: texts.iter().map(|s| s.to_string()).collect(),
        });
        machine
    }

    #[test]
    fn multi_candidate_shows_the_first() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        assert_eq!(
            machine.on_event(Event::CompletionReadyMulti {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                candidates: vec!["alpha".into(), "beta".into()],
            }),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "alpha".into(),
            }]
        );
    }

    #[test]
    fn cycle_rotates_to_the_next_candidate_and_wraps() {
        let mut machine = showing_candidates(&["alpha", "beta", "gamma"]);
        assert_eq!(
            machine.on_event(Event::Cycle),
            vec![Command::UpdateGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "beta".into(),
            }]
        );
        assert_eq!(
            machine.on_event(Event::Cycle),
            vec![Command::UpdateGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "gamma".into(),
            }]
        );
        // Wraps back to the first.
        assert_eq!(
            machine.on_event(Event::Cycle),
            vec![Command::UpdateGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "alpha".into(),
            }]
        );
    }

    #[test]
    fn cycle_with_one_candidate_is_a_noop() {
        let mut machine = showing_candidates(&["solo"]);
        assert_eq!(machine.on_event(Event::Cycle), vec![]);
    }

    #[test]
    fn cycle_with_nothing_showing_is_a_noop() {
        let mut machine = machine();
        assert_eq!(machine.on_event(Event::Cycle), vec![]);
    }

    #[test]
    fn accept_full_inserts_the_cycled_candidate() {
        let mut machine = showing_candidates(&["alpha", "beta"]);
        machine.on_event(Event::Cycle); // now showing "beta"
        assert_eq!(
            machine.on_event(Event::AcceptFull),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "beta".into(),
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn accept_word_collapses_to_the_active_candidate() {
        // After a partial (word) accept the sibling candidates are stale — they
        // still begin with the just-accepted word — so cycling must not re-offer
        // them (review finding #1).
        let mut machine = showing_candidates(&["world there friend", "world other text"]);
        machine.on_event(Event::AcceptWord); // inserts "world ", keeps "there friend"
        assert_eq!(machine.on_event(Event::Cycle), vec![]);
    }

    #[test]
    fn near_duplicate_candidates_are_deduped() {
        // Candidates differing only by trailing space / case collapse to one
        // (review finding #4), so cycling never shows a visual duplicate.
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReadyMulti {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            candidates: vec![
                "Hello there".into(),
                "hello there ".into(),
                "other one".into(),
            ],
        });
        // Only "Hello there" and "other one" survive → one Cycle reaches "other
        // one", the next wraps back (no third near-duplicate).
        assert_eq!(
            machine.on_event(Event::Cycle),
            vec![Command::UpdateGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "other one".into(),
            }]
        );
        assert_eq!(
            machine.on_event(Event::Cycle),
            vec![Command::UpdateGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "Hello there".into(),
            }]
        );
    }

    #[test]
    fn duplicate_candidates_are_deduped() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReadyMulti {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            candidates: vec!["same".into(), "same".into(), "other".into()],
        });
        // Two distinct candidates survive → one Cycle reaches "other", the next
        // wraps to "same" (not a third identical entry).
        assert_eq!(
            machine.on_event(Event::Cycle),
            vec![Command::UpdateGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "other".into(),
            }]
        );
        assert_eq!(
            machine.on_event(Event::Cycle),
            vec![Command::UpdateGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "same".into(),
            }]
        );
    }

    #[test]
    fn dismiss_suppress_hides() {
        let mut machine = showing_machine();

        assert_eq!(
            machine.on_event(Event::DismissSuppress),
            vec![Command::Hide]
        );
    }

    #[test]
    fn dismiss_suppress_blocks_the_next_edit_then_resumes() {
        // Esc (DismissSuppress) suppresses completions in the current field. The
        // next user edit clears the flag but is itself still gated (no request);
        // the edit after that resumes normal triggering.
        let mut machine = showing_machine();
        machine.on_event(Event::DismissSuppress);

        // First edit after Esc: clears suppression, but does not arm.
        machine.on_event(text_changed("xy", 2, 1000));
        assert_eq!(machine.on_event(Event::Tick { now_ms: 1200 }), vec![]);

        // Second edit: suppression already cleared → arms and fires normally.
        machine.on_event(text_changed("xyz", 3, 2000));
        assert!(matches!(
            machine.on_event(Event::Tick { now_ms: 2200 }).as_slice(),
            [Command::RequestCompletion { .. }]
        ));
    }

    #[test]
    fn focus_to_other_field_clears_suppression() {
        let mut machine = showing_machine();
        machine.on_event(Event::DismissSuppress);

        // Refocusing (a different field) clears suppression: the next edit arms.
        machine.on_event(Event::Focus {
            field: field("field-b"),
            caps: inline_caps(),
        });
        machine.on_event(Event::TextChanged {
            field: field("field-b"),
            value: "hello".into(),
            caret: 5,
            edit: EditKind::Insert,
            previous_caret: None,
            previous_value_hash: None,
            trigger: TriggerPolicy::Automatic,
            now_ms: 3000,
        });
        assert!(matches!(
            machine.on_event(Event::Tick { now_ms: 3200 }).as_slice(),
            [Command::RequestCompletion { .. }]
        ));
    }

    #[test]
    fn dismiss_suppress_blocks_an_inflight_completion() {
        // A request already in flight when Esc is pressed must not pop a ghost
        // back up after the dismiss.
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 }); // requested gen=1, snapshot=1
        machine.on_event(Event::DismissSuppress);

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "late ghost".into(),
            }),
            vec![]
        );
    }

    fn showing_three_words() -> SuggestionMachine {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            text: "world there friend".into(),
        });
        machine
    }

    #[test]
    fn accept_full_inserts_all_and_hides() {
        let mut machine = showing_three_words();

        assert_eq!(
            machine.on_event(Event::AcceptFull),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "world there friend".into(),
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn accept_word_inserts_word_and_updates_ghost() {
        let mut machine = showing_three_words();

        assert_eq!(
            machine.on_event(Event::AcceptWord),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "world ".into(),
                },
                Command::UpdateGhost {
                    field: field("field-a"),
                    snapshot: 1,
                    text: "there friend".into(),
                },
            ]
        );
    }

    #[test]
    fn preview_accept_word_reports_inserted_word() {
        let machine = showing_three_words();

        assert_eq!(
            machine.preview_accept_insert(AcceptAction::Word),
            Some((field("field-a"), "world ".into()))
        );
    }

    #[test]
    fn preview_accept_full_reports_remaining_completion() {
        let machine = showing_three_words();

        assert_eq!(
            machine.preview_accept_insert(AcceptAction::Full),
            Some((field("field-a"), "world there friend".into()))
        );
    }

    #[test]
    fn accept_last_word_hides() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            text: "solo".into(),
        });

        assert_eq!(
            machine.on_event(Event::AcceptWord),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "solo".into(),
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn accept_with_nothing_showing_is_noop() {
        let mut machine = machine();

        assert_eq!(machine.on_event(Event::AcceptFull), vec![]);
    }

    #[test]
    fn accept_word_with_nothing_showing_is_noop() {
        let mut machine = machine();

        assert_eq!(machine.on_event(Event::AcceptWord), vec![]);
    }

    #[test]
    fn whitespace_only_completion_is_suppressed() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "   \n\t".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn completion_ready_without_request_is_noop() {
        let mut machine = machine();

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "unrequested".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn tick_without_pending_is_noop() {
        let mut machine = machine();

        assert_eq!(machine.on_event(Event::Tick { now_ms: 9999 }), vec![]);
    }

    #[test]
    fn dismiss_with_nothing_showing_is_noop() {
        let mut machine = machine();

        assert_eq!(machine.on_event(Event::Dismiss), vec![]);
    }

    #[test]
    fn caret_moved_with_nothing_showing_is_noop() {
        let mut machine = machine();

        assert_eq!(
            machine.on_event(Event::CaretMoved {
                field: field("field-a"),
                caret: 4,
            }),
            vec![]
        );
    }

    #[test]
    fn secure_state_change_with_nothing_showing_emits_no_hide() {
        let mut machine = machine();

        assert_eq!(
            machine.on_event(Event::SecureStateChanged {
                caps: secure_caps(),
            }),
            vec![]
        );
    }

    #[test]
    fn focus_with_nothing_showing_emits_no_hide() {
        let mut machine = machine();

        assert_eq!(
            machine.on_event(Event::Focus {
                field: field("field-a"),
                caps: inline_caps(),
            }),
            vec![]
        );
    }

    #[test]
    fn accept_word_advances_internal_caret_so_matching_caret_keeps_showing() {
        let mut machine = showing_three_words();
        // Suggestion shown at caret 1 ("x"); accepting "world " (6 chars)
        // advances the tracked caret to 7.
        machine.on_event(Event::AcceptWord);

        // A caret report at the advanced position must NOT hide the remainder.
        assert_eq!(
            machine.on_event(Event::CaretMoved {
                field: field("field-a"),
                caret: 7,
            }),
            vec![]
        );
    }

    #[test]
    fn tick_after_request_fired_does_not_refire() {
        // One arming yields exactly one RequestCompletion: firing clears
        // pending_since, so a second Tick further past the threshold is a noop.
        let mut machine = machine();
        machine.on_event(text_changed("hello ", 6, 1000));

        // First Tick past debounce fires the request.
        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 1200 }),
            vec![Command::RequestCompletion {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                prompt: "hello".into(),
            }]
        );

        // Second Tick even further past the threshold must not re-fire.
        assert_eq!(machine.on_event(Event::Tick { now_ms: 5000 }), vec![]);
    }

    fn showing_three_distinct_words() -> SuggestionMachine {
        // "alpha beta gamma" shares no token with the recent left context
        // ("x"), so it survives the repetition gate, and three words fit under
        // the max-words cap of 4.
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            text: "alpha beta gamma".into(),
        });
        machine
    }

    #[test]
    fn accept_word_to_exhaustion_inserts_each_word_then_hides() {
        let mut machine = showing_three_distinct_words();

        // First word: insert "alpha " and show the remaining two words.
        assert_eq!(
            machine.on_event(Event::AcceptWord),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "alpha ".into(),
                },
                Command::UpdateGhost {
                    field: field("field-a"),
                    snapshot: 1,
                    text: "beta gamma".into(),
                },
            ]
        );

        // Second word: insert "beta " and show the final word.
        assert_eq!(
            machine.on_event(Event::AcceptWord),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "beta ".into(),
                },
                Command::UpdateGhost {
                    field: field("field-a"),
                    snapshot: 1,
                    text: "gamma".into(),
                },
            ]
        );

        // The tracked caret has advanced across both accepted words
        // (1 + "alpha ".len() + "beta ".len() = 12), so a caret report at the
        // advanced position keeps the final word showing rather than hiding it.
        assert_eq!(
            machine.on_event(Event::CaretMoved {
                field: field("field-a"),
                caret: 12,
            }),
            vec![]
        );

        // Third (last) word: insert "gamma" with no trailing space and hide.
        assert_eq!(
            machine.on_event(Event::AcceptWord),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "gamma".into(),
                },
                Command::Hide,
            ]
        );

        // Nothing is showing anymore: a further accept is a noop.
        assert_eq!(machine.on_event(Event::AcceptWord), vec![]);
    }

    #[test]
    fn preview_accept_insert_with_nothing_showing_returns_none() {
        // A fresh machine has no completion showing, so neither preview
        // variant can report an insertion.
        let machine = machine();

        assert_eq!(machine.preview_accept_insert(AcceptAction::Full), None);
        assert_eq!(machine.preview_accept_insert(AcceptAction::Word), None);
    }

    #[test]
    fn completion_ready_discarded_after_focus_advances_boundary() {
        let mut machine = machine();
        // Arm a request: generation/snapshot are now 1.
        machine.on_event(text_changed("hello ", 6, 1000));
        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 1200 }),
            vec![Command::RequestCompletion {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                prompt: "hello".into(),
            }]
        );

        // Focusing a new field advances the snapshot/generation boundary and
        // clears the in-flight request.
        machine.on_event(Event::Focus {
            field: field("field-b"),
            caps: inline_caps(),
        });

        // A completion tagged with the now-stale generation/snapshot must be
        // discarded — nothing is shown.
        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "world".into(),
            }),
            vec![]
        );
    }
}
