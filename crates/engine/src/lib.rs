//! Impure-but-deterministic wiring between the pure `SuggestionMachine` and the
//! platform adapter + overlay presenter.
//!
//! The engine translates host inputs into `core` events, runs the machine, and
//! dispatches the resulting commands to platform effects. Model inference lives
//! *outside* the engine: `RequestCompletion` commands are surfaced as
//! [`CompletionRequest`] values for the host loop to fulfil, then fed back via
//! [`Engine::on_completion`]. The engine therefore never blocks on inference and
//! stays fully deterministic under test.

use core::{Command, Event, SnapshotId, SuggestionMachine};
pub use core::{EditKind, TriggerPolicy};
use platform::{
    AcceptAction, Capabilities, FieldHandle, InsertStrategy, KeyInterceptMode, OverlayPlacement,
    OverlayPresenter, PlatformAdapter, SecurityState, Toolkit,
};

/// A text edit reported by the host, carrying the metadata the contract
/// requires (`edit` kind, previous caret/value hash, trigger policy) so the
/// machine can gate automatic versus manual requests faithfully.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextChange {
    pub field: FieldHandle,
    pub value: String,
    pub caret: usize,
    pub edit: EditKind,
    pub previous_caret: Option<usize>,
    pub previous_value_hash: Option<u64>,
    pub trigger: TriggerPolicy,
    pub now_ms: u64,
}

/// A model completion the host loop must fulfil and feed back via
/// [`Engine::on_completion`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionRequest {
    pub generation: u64,
    pub field: FieldHandle,
    pub snapshot: SnapshotId,
    pub prompt: String,
    pub max_tokens: usize,
}

pub struct Engine<P, O> {
    machine: SuggestionMachine,
    adapter: P,
    overlay: O,
    caps: Capabilities,
    max_tokens: usize,
    accept: Option<platform::AcceptSubscription>,
}

impl<P: PlatformAdapter, O: OverlayPresenter> Engine<P, O> {
    pub fn new(
        adapter: P,
        overlay: O,
        debounce_ms: u64,
        max_words: usize,
        max_tokens: usize,
    ) -> Self {
        let caps = unsupported_caps();
        Self {
            machine: SuggestionMachine::new(caps.clone(), debounce_ms, max_words),
            adapter,
            overlay,
            caps,
            max_tokens,
            accept: None,
        }
    }

    /// Provide the accept-tap subscription so the engine can arm the consuming
    /// tap only while a suggestion is visible (the two-tap design from spec §4).
    pub fn set_accept_subscription(&mut self, accept: platform::AcceptSubscription) {
        self.accept = Some(accept);
    }

    fn set_tap_visible(
        &self,
        visible: bool,
        action: Option<AcceptAction>,
    ) -> Result<(), platform::PlatformError> {
        if let Some(accept) = &self.accept {
            accept.set_suggestion_visible(visible)?;
            accept.set_accept_action(action)?;
        }
        Ok(())
    }

    pub fn on_focus(
        &mut self,
        field: FieldHandle,
    ) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        let caps = self.adapter.capabilities(&field)?;
        self.caps = caps.clone();
        let commands = self.machine.on_event(Event::Focus { field, caps });
        self.dispatch(commands)
    }

    pub fn on_text_changed(
        &mut self,
        change: TextChange,
    ) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        let commands = self.machine.on_event(Event::TextChanged {
            field: change.field,
            value: change.value,
            caret: change.caret,
            edit: change.edit,
            previous_caret: change.previous_caret,
            previous_value_hash: change.previous_value_hash,
            trigger: change.trigger,
            now_ms: change.now_ms,
        });
        self.dispatch(commands)
    }

    pub fn on_caret(
        &mut self,
        field: FieldHandle,
        caret: usize,
    ) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        let commands = self.machine.on_event(Event::CaretMoved { field, caret });
        self.dispatch(commands)
    }

    pub fn on_secure_state(
        &mut self,
        caps: Capabilities,
    ) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        self.caps = caps.clone();
        let commands = self.machine.on_event(Event::SecureStateChanged { caps });
        self.dispatch(commands)
    }

    pub fn on_tick(
        &mut self,
        now_ms: u64,
    ) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        let commands = self.machine.on_event(Event::Tick { now_ms });
        self.dispatch(commands)
    }

    pub fn on_completion(
        &mut self,
        request: &CompletionRequest,
        text: String,
    ) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        let commands = self.machine.on_event(Event::CompletionReady {
            generation: request.generation,
            field: request.field.clone(),
            snapshot: request.snapshot,
            text,
        });
        self.dispatch(commands)
    }

    pub fn on_accept(
        &mut self,
        action: AcceptAction,
    ) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        let event = match action {
            AcceptAction::Full => Event::AcceptFull,
            AcceptAction::Word => Event::AcceptWord,
        };
        let commands = self.machine.on_event(event);
        self.dispatch(commands)
    }

    fn dispatch(
        &mut self,
        commands: Vec<Command>,
    ) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        let mut requests = Vec::new();
        for command in commands {
            match command {
                Command::RequestCompletion {
                    generation,
                    field,
                    snapshot,
                    prompt,
                } => requests.push(CompletionRequest {
                    generation,
                    field,
                    snapshot,
                    prompt,
                    max_tokens: self.max_tokens,
                }),
                Command::ShowGhost { field, text, .. } => {
                    // Inline placement uses the caret rect; popup mode (no caret
                    // geometry) falls back to the adapter's popup anchor.
                    let rect = match self.adapter.caret_rect(&field)? {
                        Some(rect) => Some(rect),
                        None => self.adapter.popup_anchor(&field)?,
                    };
                    if let Some(rect) = rect {
                        self.overlay.show_ghost(rect, &text)?;
                        self.set_tap_visible(true, Some(AcceptAction::Full))?;
                    }
                }
                Command::Insert { field, text } => {
                    self.adapter
                        .insert(&field, &text, self.caps.insert_strategy)?;
                }
                Command::UpdateGhost { text, .. } => self.overlay.update_ghost(&text)?,
                Command::Hide => {
                    self.overlay.hide()?;
                    self.set_tap_visible(false, None)?;
                }
            }
        }
        Ok(requests)
    }
}

fn unsupported_caps() -> Capabilities {
    Capabilities {
        readable_text: false,
        readable_caret: false,
        writable: false,
        secure: false,
        security_state: SecurityState::Unknown,
        toolkit: Toolkit::Unknown(String::new()),
        multiline: false,
        insert_strategy: InsertStrategy::None,
        accept_intercept: KeyInterceptMode::None,
        overlay_at_caret: OverlayPlacement::None,
        coords_global_screen: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::{
        AcceptCallback, AcceptSubscription, AppId, CaretCallback, ContextSource, Environment,
        FocusCallback, Inserted, OffsetEncoding, OperatingSystem, PlatformError, ScreenRect,
        Subscription, TextContext,
    };
    use std::sync::{Arc, Mutex};

    fn field() -> FieldHandle {
        FieldHandle {
            app: "TextEdit".into(),
            pid: Some(42),
            element_id: "field-a".into(),
            generation: 1,
        }
    }

    fn typed(value: &str, caret: usize, now_ms: u64) -> TextChange {
        TextChange {
            field: field(),
            value: value.into(),
            caret,
            edit: EditKind::Insert,
            previous_caret: None,
            previous_value_hash: None,
            trigger: TriggerPolicy::Automatic,
            now_ms,
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

    #[derive(Clone, Debug, PartialEq)]
    enum OverlayCall {
        Show(ScreenRect, String),
        Update(String),
        Hide,
    }

    #[derive(Clone, Default)]
    struct FakeOverlay {
        calls: Arc<Mutex<Vec<OverlayCall>>>,
    }

    impl OverlayPresenter for FakeOverlay {
        fn show_ghost(&mut self, rect: ScreenRect, text: &str) -> Result<(), PlatformError> {
            self.calls
                .lock()
                .unwrap()
                .push(OverlayCall::Show(rect, text.into()));
            Ok(())
        }
        fn update_ghost(&mut self, text: &str) -> Result<(), PlatformError> {
            self.calls
                .lock()
                .unwrap()
                .push(OverlayCall::Update(text.into()));
            Ok(())
        }
        fn hide(&mut self) -> Result<(), PlatformError> {
            self.calls.lock().unwrap().push(OverlayCall::Hide);
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FakeAdapter {
        caps: Capabilities,
        rect: Option<ScreenRect>,
        popup: Option<ScreenRect>,
        inserts: Arc<Mutex<Vec<(FieldHandle, String, InsertStrategy)>>>,
    }

    impl FakeAdapter {
        fn new() -> Self {
            Self {
                caps: inline_caps(),
                rect: Some(ScreenRect {
                    x: 10.0,
                    y: 20.0,
                    w: 1.0,
                    h: 14.0,
                }),
                popup: None,
                inserts: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl PlatformAdapter for FakeAdapter {
        fn environment(&self) -> Environment {
            Environment {
                os: OperatingSystem::Macos,
                version: "test".into(),
                display_topology: None,
            }
        }
        fn subscribe_focus(&self, _cb: FocusCallback) -> Result<Subscription, PlatformError> {
            unimplemented!()
        }
        fn subscribe_caret(&self, _cb: CaretCallback) -> Result<Subscription, PlatformError> {
            unimplemented!()
        }
        fn subscribe_accept(
            &self,
            _cb: AcceptCallback,
        ) -> Result<AcceptSubscription, PlatformError> {
            unimplemented!()
        }
        fn front_app(&self) -> Option<AppId> {
            None
        }
        fn capabilities(&self, _field: &FieldHandle) -> Result<Capabilities, PlatformError> {
            Ok(self.caps.clone())
        }
        fn read_context(&self, _field: &FieldHandle) -> Result<TextContext, PlatformError> {
            let _ = (ContextSource::Unknown, OffsetEncoding::Utf8Bytes);
            unimplemented!()
        }
        fn caret_rect(&self, _field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
            Ok(self.rect)
        }
        fn popup_anchor(&self, _field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
            Ok(self.popup)
        }
        fn insert(
            &self,
            field: &FieldHandle,
            text: &str,
            strategy: InsertStrategy,
        ) -> Result<Inserted, PlatformError> {
            self.inserts
                .lock()
                .unwrap()
                .push((field.clone(), text.into(), strategy));
            Ok(Inserted {
                bytes: text.len(),
                chars: text.chars().count(),
                strategy,
            })
        }
    }

    fn engine() -> (Engine<FakeAdapter, FakeOverlay>, FakeAdapter, FakeOverlay) {
        let adapter = FakeAdapter::new();
        let overlay = FakeOverlay::default();
        let engine = Engine::new(adapter.clone(), overlay.clone(), 200, 4, 32);
        (engine, adapter, overlay)
    }

    /// Drives the engine to a showing-ghost state with the given completion text.
    fn showing(text: &str) -> (Engine<FakeAdapter, FakeOverlay>, FakeAdapter, FakeOverlay) {
        let (mut engine, adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine.on_completion(&requests[0], text.into()).unwrap();
        overlay.calls.lock().unwrap().clear();
        (engine, adapter, overlay)
    }

    #[test]
    fn arms_accept_tap_on_show_and_disarms_on_hide() {
        let (mut engine, _adapter, _overlay) = engine();
        let visible: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let actions: Arc<Mutex<Vec<Option<AcceptAction>>>> = Arc::new(Mutex::new(Vec::new()));
        let v = Arc::clone(&visible);
        let a = Arc::clone(&actions);
        let sub = AcceptSubscription::new(
            Subscription::new(0),
            move |vis| {
                v.lock().unwrap().push(vis);
                Ok(())
            },
            |_delay| Ok(()),
            move |act| {
                a.lock().unwrap().push(act);
                Ok(())
            },
        );
        engine.set_accept_subscription(sub);

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine
            .on_completion(&requests[0], "hi there".into())
            .unwrap();

        assert_eq!(*visible.lock().unwrap(), vec![true], "armed on show");
        assert_eq!(*actions.lock().unwrap(), vec![Some(AcceptAction::Full)]);

        engine.on_accept(AcceptAction::Full).unwrap();

        assert_eq!(
            *visible.lock().unwrap(),
            vec![true, false],
            "disarmed on hide"
        );
        assert_eq!(
            *actions.lock().unwrap(),
            vec![Some(AcceptAction::Full), None]
        );
    }

    #[test]
    fn popup_anchor_used_when_caret_rect_absent() {
        let mut adapter = FakeAdapter::new();
        adapter.rect = None;
        let anchor = ScreenRect {
            x: 5.0,
            y: 6.0,
            w: 200.0,
            h: 24.0,
        };
        adapter.popup = Some(anchor);
        let overlay = FakeOverlay::default();
        let mut engine = Engine::new(adapter, overlay.clone(), 200, 4, 32);

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine
            .on_completion(&requests[0], "popup text".into())
            .unwrap();

        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![OverlayCall::Show(anchor, "popup text".into())]
        );
    }

    #[test]
    fn no_overlay_when_neither_caret_nor_popup_anchor() {
        let mut adapter = FakeAdapter::new();
        adapter.rect = None;
        adapter.popup = None;
        let overlay = FakeOverlay::default();
        let mut engine = Engine::new(adapter, overlay.clone(), 200, 4, 32);

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine.on_completion(&requests[0], "nope".into()).unwrap();

        assert!(overlay.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn manual_trigger_is_plumbed_and_does_not_auto_request() {
        let (mut engine, _adapter, _overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine
            .on_text_changed(TextChange {
                trigger: TriggerPolicy::Manual,
                ..typed("hello ", 6, 1000)
            })
            .unwrap();

        assert!(engine.on_tick(2000).unwrap().is_empty());
    }

    #[test]
    fn caret_move_hides_a_showing_ghost() {
        let (mut engine, _adapter, overlay) = showing("hello there");

        engine.on_caret(field(), 99).unwrap();

        assert_eq!(*overlay.calls.lock().unwrap(), vec![OverlayCall::Hide]);
    }

    #[test]
    fn accept_full_inserts_with_field_strategy_and_hides() {
        let (mut engine, adapter, overlay) = showing("brave new world");

        let follow = engine.on_accept(AcceptAction::Full).unwrap();
        assert!(follow.is_empty());

        assert_eq!(
            *adapter.inserts.lock().unwrap(),
            vec![(field(), "brave new world".into(), InsertStrategy::AxSet)]
        );
        assert_eq!(*overlay.calls.lock().unwrap(), vec![OverlayCall::Hide]);
    }

    #[test]
    fn accept_word_inserts_word_and_updates_ghost() {
        let (mut engine, adapter, overlay) = showing("world there friend");

        engine.on_accept(AcceptAction::Word).unwrap();

        assert_eq!(
            *adapter.inserts.lock().unwrap(),
            vec![(field(), "world ".into(), InsertStrategy::AxSet)]
        );
        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![OverlayCall::Update("there friend".into())]
        );
    }

    #[test]
    fn stale_completion_after_more_typing_does_not_show_ghost() {
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("hello ", 6, 1000)).unwrap();
        let requests = engine.on_tick(1200).unwrap();

        // User keeps typing before the model answers — invalidates the request.
        engine.on_text_changed(typed("hello w", 7, 1300)).unwrap();

        let follow = engine.on_completion(&requests[0], "world".into()).unwrap();
        assert!(follow.is_empty());
        assert!(overlay.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn tick_surfaces_request_then_completion_shows_ghost() {
        let (mut engine, _adapter, overlay) = engine();

        assert!(engine.on_focus(field()).unwrap().is_empty());
        assert!(engine
            .on_text_changed(typed("hello ", 6, 1000))
            .unwrap()
            .is_empty());

        let requests = engine.on_tick(1200).unwrap();
        assert_eq!(
            requests,
            vec![CompletionRequest {
                generation: 2,
                field: field(),
                snapshot: 2,
                prompt: "hello".into(),
                max_tokens: 32,
            }]
        );

        let follow = engine.on_completion(&requests[0], "world".into()).unwrap();
        assert!(follow.is_empty());

        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![OverlayCall::Show(
                ScreenRect {
                    x: 10.0,
                    y: 20.0,
                    w: 1.0,
                    h: 14.0,
                },
                "world".into()
            )]
        );
    }
}
