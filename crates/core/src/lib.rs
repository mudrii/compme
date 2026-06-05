//! Deterministic suggestion state machine.

use context::{left_context, trim_prefix};
use platform::{ux_mode, Capabilities, FieldHandle, UxMode};
use ranker::{cap_words, next_word};

pub type SnapshotId = u64;

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
    SecureStateChanged {
        caps: Capabilities,
    },
    AcceptFull,
    AcceptWord,
    Dismiss,
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
    remaining: String,
    caret: usize,
}

pub struct SuggestionMachine {
    caps: Capabilities,
    debounce_ms: u64,
    max_words: usize,
    generation: u64,
    snapshot: SnapshotId,
    field: Option<FieldHandle>,
    value: String,
    caret: usize,
    pending_since: Option<u64>,
    requested: Option<RequestedCompletion>,
    showing: Option<Showing>,
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
            generation: 0,
            snapshot: 0,
            field: None,
            value: String::new(),
            caret: 0,
            pending_since: None,
            requested: None,
            showing: None,
        }
    }

    fn enabled(&self) -> bool {
        matches!(ux_mode(&self.caps), UxMode::Inline | UxMode::Popup)
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
                self.advance_snapshot();
            }
            Event::TextChanged {
                field,
                value,
                caret,
                edit: _,
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
                self.pending_since = if self.enabled() && trigger == TriggerPolicy::Automatic {
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
                            trim_prefix(&left_context(&self.value, self.caret)).to_string();
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
                let matches_request = self.requested.as_ref().is_some_and(|requested| {
                    requested.generation == generation
                        && requested.snapshot == snapshot
                        && requested.field == field
                        && generation == self.generation
                        && snapshot == self.snapshot
                });

                if matches_request {
                    let capped = cap_words(&text, self.max_words);
                    if !capped.is_empty() {
                        self.showing = Some(Showing {
                            field: field.clone(),
                            snapshot,
                            remaining: capped.clone(),
                            caret: self.caret,
                        });
                        out.push(Command::ShowGhost {
                            field,
                            snapshot,
                            text: capped,
                        });
                    }
                    self.requested = None;
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
            Event::AcceptFull => {
                if let Some(showing) = self.showing.take() {
                    out.push(Command::Insert {
                        field: showing.field,
                        text: showing.remaining,
                    });
                    out.push(Command::Hide);
                    self.advance_snapshot();
                }
            }
            Event::AcceptWord => {
                if let Some(mut showing) = self.showing.take() {
                    let (word, rest) = next_word(&showing.remaining);
                    out.push(Command::Insert {
                        field: showing.field.clone(),
                        text: word.clone(),
                    });
                    if rest.is_empty() {
                        out.push(Command::Hide);
                        self.advance_snapshot();
                    } else {
                        showing.caret += word.chars().count();
                        showing.remaining = rest.clone();
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
}
