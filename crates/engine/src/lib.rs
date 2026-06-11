//! Impure-but-deterministic wiring between the pure `SuggestionMachine` and the
//! platform adapter + overlay presenter.
//!
//! The engine translates host inputs into `engine_core` events, runs the machine, and
//! dispatches the resulting commands to platform effects. Model inference lives
//! *outside* the engine: `RequestCompletion` commands are surfaced as
//! [`CompletionRequest`] values for the host loop to fulfil, then fed back via
//! [`Engine::on_completion`]. The engine therefore never blocks on inference and
//! stays fully deterministic under test.

use engine_core::{Command, Event, SnapshotId, SuggestionMachine};
pub use engine_core::{EditKind, StatEvent, TriggerPolicy};
use platform::{
    AcceptAction, Capabilities, FieldHandle, InsertStrategy, KeyInterceptMode, OverlayPlacement,
    OverlayPresenter, PlatformAdapter, SecurityState, Toolkit,
};
use std::time::Duration;

const SYNTHETIC_INSERT_HIDE_DELAY: Duration = Duration::from_millis(50);

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
///
/// Created by [`Engine::dispatch`] whenever the `SuggestionMachine` emits a
/// `RequestCompletion` command. The host is responsible for running inference
/// and returning the result through [`Engine::on_completion`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionRequest {
    pub generation: u64,
    pub field: FieldHandle,
    pub snapshot: SnapshotId,
    pub prompt: String,
    pub max_tokens: usize,
}

/// Impure-but-deterministic wiring layer that connects the pure
/// [`SuggestionMachine`] to a [`PlatformAdapter`] and an [`OverlayPresenter`].
///
/// The engine translates host inputs into `engine_core` events, runs the machine, and
/// dispatches the resulting commands to platform effects. It owns no inference
/// logic: `RequestCompletion` commands are surfaced as [`CompletionRequest`]
/// values for the host loop to fulfil and fed back via [`Engine::on_completion`].
pub struct Engine<P, O> {
    machine: SuggestionMachine,
    adapter: P,
    overlay: O,
    caps: Capabilities,
    max_tokens: usize,
    accept: Option<platform::AcceptSubscription>,
    /// When set (MirrorOnly apps — Firefox/Zen, A2 §16), render the ghost in the
    /// floating mirror window (the front-app popup anchor) instead of inline,
    /// since these apps cannot host an inline caret ghost.
    mirror_mode: bool,
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
            mirror_mode: false,
        }
    }

    /// Route ghost rendering to the mirror window (A2 §16): MirrorOnly apps
    /// cannot host an inline caret ghost, so the overlay renders at the front-app
    /// popup anchor instead. The run loop sets this per focused app's tier.
    pub fn set_mirror_mode(&mut self, mirror: bool) {
        self.mirror_mode = mirror;
    }

    /// Runtime flip of the mid-word gate (per-app override / Labs switch).
    /// Re-applied on every focus change and on Labs-switch edges in the run
    /// loop; [`Self::with_trigger_gates`] sets the launch-time default.
    pub fn set_allow_mid_word(&mut self, allow_mid_word: bool) {
        self.machine.set_allow_mid_word(allow_mid_word);
    }

    /// Configure conservative trigger gating on the underlying machine (spec §4):
    /// minimum left-context length and mid-word suppression. Forwards to
    /// [`SuggestionMachine::with_trigger_gates`].
    pub fn with_trigger_gates(mut self, min_context_chars: usize, allow_mid_word: bool) -> Self {
        self.machine = self
            .machine
            .with_trigger_gates(min_context_chars, allow_mid_word);
        self
    }

    /// Runtime flip of the single-word trailing space (General-tab switch).
    /// Read per accept on the machine; applies immediately.
    pub fn set_trailing_space(&mut self, enabled: bool) {
        self.machine.set_trailing_space(enabled);
    }

    /// Forward to [`SuggestionMachine::with_trailing_space`] — enable Cotypist's
    /// "Include trailing space after single-word completions" policy.
    pub fn with_trailing_space(mut self, enabled: bool) -> Self {
        self.machine = self.machine.with_trailing_space(enabled);
        self
    }

    /// Provide the accept-tap subscription so the engine can arm the consuming
    /// tap only while a suggestion is visible (the two-tap design from spec §4).
    // Extended beyond A1b contract table: accept-tap lifecycle requires
    // visibility callbacks not in the original spec.
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

    fn hide_tap_after(&self, delay: Duration) -> Result<(), platform::PlatformError> {
        if let Some(accept) = &self.accept {
            accept.hide_suggestion_after(delay)?;
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

    pub fn on_caret_moved(
        &mut self,
        field: FieldHandle,
        caret: usize,
    ) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        let commands = self.machine.on_event(Event::CaretMoved { field, caret });
        self.dispatch(commands)
    }

    // Not in original event enum; added because secure input mode changes
    // require hiding the ghost immediately.
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

    /// Feed multiple candidate completions for one request (multi-candidate, A2
    /// §16). The engine shows the first and `on_cycle` rotates through the rest.
    pub fn on_completion_multi(
        &mut self,
        request: &CompletionRequest,
        candidates: Vec<String>,
    ) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        let commands = self.machine.on_event(Event::CompletionReadyMulti {
            generation: request.generation,
            field: request.field.clone(),
            snapshot: request.snapshot,
            candidates,
        });
        self.dispatch(commands)
    }

    /// Rotate to the next candidate while a suggestion is showing.
    pub fn on_cycle(&mut self) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        let commands = self.machine.on_event(Event::Cycle);
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

    pub fn preview_accept_insert(
        &self,
        action: AcceptAction,
    ) -> Option<(FieldHandle, String, usize)> {
        self.machine.preview_accept_insert(action)
    }

    /// Offer a local *replacement* suggestion (emoji/typo/spelling): show `text`
    /// as the ghost; accepting it deletes `replace_left` chars left of the caret
    /// before inserting. The host detects the opportunity (e.g. `emoji::suggest`)
    /// and supplies the rendered `text` + count. See the integration-phase design
    /// note; honoring the deletion is the adapter's `insert_replacing`.
    pub fn offer_replacement(
        &mut self,
        field: &FieldHandle,
        text: String,
        replace_left: usize,
    ) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        let commands = self.machine.offer_replacement(field, text, replace_left);
        self.dispatch(commands)
    }

    /// Drain machine-internal Shown/Superseded stat events for the host to record
    /// into local usage stats (design spec §11).
    pub fn take_stat_events(&mut self) -> Vec<StatEvent> {
        self.machine.take_stat_events()
    }

    /// Dismiss any showing suggestion (e.g. the user disabled the app via the
    /// tray). Wraps the machine's `DismissDiscard` event so a visible ghost
    /// hides immediately AND any in-flight request is staled — otherwise a
    /// completion already submitted to the inference worker could pop a ghost
    /// back up after the user disabled the app.
    pub fn on_dismiss(&mut self) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        let commands = self.machine.on_event(Event::DismissDiscard);
        self.dispatch(commands)
    }

    /// Esc: hide the showing ghost AND suppress completions in the current field
    /// until refocus/edit (Cotypist parity, A1b Task 5b / §15 D11).
    pub fn on_dismiss_suppress(
        &mut self,
    ) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        let commands = self.machine.on_event(Event::DismissSuppress);
        self.dispatch(commands)
    }

    fn dispatch(
        &mut self,
        commands: Vec<Command>,
    ) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        let mut requests = Vec::new();
        let mut delay_next_hide = false;
        let mut show_failed = false;
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
                    // geometry) falls back to the adapter's popup anchor. Mirror
                    // mode (MirrorOnly apps) renders at the popup/mirror anchor
                    // directly, since these apps have no usable inline caret.
                    let rect = if self.mirror_mode {
                        match self.adapter.popup_anchor(&field)? {
                            Some(rect) => Some(rect),
                            None => self.adapter.caret_rect(&field)?,
                        }
                    } else {
                        match self.adapter.caret_rect(&field)? {
                            Some(rect) => Some(rect),
                            None => self.adapter.popup_anchor(&field)?,
                        }
                    };
                    if let Some(rect) = rect {
                        self.overlay.show_ghost(rect, &text)?;
                        self.set_tap_visible(true, Some(AcceptAction::Full))?;
                    } else {
                        // No caret rect and no popup anchor: we cannot place the
                        // ghost. The machine already marked itself showing, so
                        // reconcile it back to not-showing (below) — otherwise its
                        // state would lie and a later accept could insert a ghost
                        // the user never saw.
                        show_failed = true;
                    }
                }
                Command::Insert { field, text } => {
                    // Contract: the adapter must tag this self-inserted text so it is NOT
                    // fed back to the engine as a TextChanged event. Failure breaks the
                    // show→accept→hide cycle.
                    let strategy = self.caps.insert_strategy;
                    self.adapter.insert(&field, &text, strategy)?;
                    delay_next_hide = strategy == InsertStrategy::SyntheticKeys;
                }
                Command::Replace {
                    field,
                    text,
                    replace_left,
                } => {
                    // A replacement deletes `replace_left` chars left of the caret
                    // before inserting (emoji/typo/spelling). Same self-insert echo
                    // contract as `Insert`. Adapters that cannot range-replace fall
                    // back to a plain insert (the deletion is the FFI/live residual).
                    let strategy = self.caps.insert_strategy;
                    self.adapter
                        .insert_replacing(&field, &text, replace_left, strategy)?;
                    delay_next_hide = strategy == InsertStrategy::SyntheticKeys;
                }
                Command::UpdateGhost { text, .. } => self.overlay.update_ghost(&text)?,
                Command::Hide => {
                    self.overlay.hide()?;
                    if delay_next_hide {
                        self.hide_tap_after(SYNTHETIC_INSERT_HIDE_DELAY)?;
                        delay_next_hide = false;
                    } else {
                        self.set_tap_visible(false, None)?;
                    }
                }
            }
        }
        if show_failed {
            // Clear the machine's showing state to match reality (nothing was
            // placed, the accept tap was never armed). `Dismiss` emits a `Hide`,
            // which is a no-op against the already-hidden overlay/tap. Depth-1
            // recursion: `Dismiss` never yields another `ShowGhost`.
            // The ghost was emitted but never presented, so retract the `Shown`
            // stat event the machine buffered (design spec §11 accuracy).
            self.machine.cancel_last_shown();
            let dismiss = self.machine.on_event(Event::Dismiss);
            requests.extend(self.dispatch(dismiss)?);
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
        AcceptCallback, AcceptSubscription, AppId, CaretCallback, Environment, FocusCallback,
        Inserted, OperatingSystem, PlatformError, ScreenRect, Subscription, TextContext,
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

    /// A recorded replacement insert: (field, text, replace_left, strategy).
    type ReplacingInsert = (FieldHandle, String, usize, InsertStrategy);

    #[derive(Clone)]
    struct FakeAdapter {
        caps: Capabilities,
        rect: Option<ScreenRect>,
        popup: Option<ScreenRect>,
        fail_caret_rect: bool,
        fail_popup: bool,
        fail_insert: bool,
        inserts: Arc<Mutex<Vec<(FieldHandle, String, InsertStrategy)>>>,
        replacing_inserts: Arc<Mutex<Vec<ReplacingInsert>>>,
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
                fail_caret_rect: false,
                fail_popup: false,
                fail_insert: false,
                inserts: Arc::new(Mutex::new(Vec::new())),
                replacing_inserts: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    fn unsupported_field_caps() -> Capabilities {
        let mut caps = inline_caps();
        caps.readable_text = false;
        caps.insert_strategy = InsertStrategy::None;
        caps
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
            unimplemented!()
        }
        fn caret_rect(&self, _field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
            if self.fail_caret_rect {
                return Err(PlatformError::Timeout);
            }
            Ok(self.rect)
        }
        fn popup_anchor(&self, _field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
            if self.fail_popup {
                return Err(PlatformError::Timeout);
            }
            Ok(self.popup)
        }
        fn insert(
            &self,
            field: &FieldHandle,
            text: &str,
            strategy: InsertStrategy,
        ) -> Result<Inserted, PlatformError> {
            if self.fail_insert {
                return Err(PlatformError::StaleField);
            }
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
        fn insert_replacing(
            &self,
            field: &FieldHandle,
            text: &str,
            replace_left: usize,
            strategy: InsertStrategy,
        ) -> Result<Inserted, PlatformError> {
            if self.fail_insert {
                return Err(PlatformError::StaleField);
            }
            self.replacing_inserts.lock().unwrap().push((
                field.clone(),
                text.into(),
                replace_left,
                strategy,
            ));
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
    fn replacement_accept_forwards_replace_left_through_dispatch() {
        // Integration step 3: a replacement accept must reach the adapter via
        // `insert_replacing` carrying `replace_left` (not the plain insert path).
        let (mut engine, adapter, _overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine.offer_replacement(&field(), "😄".into(), 5).unwrap();
        engine.on_accept(AcceptAction::Full).unwrap();
        assert_eq!(
            *adapter.replacing_inserts.lock().unwrap(),
            vec![(field(), "😄".to_string(), 5, InsertStrategy::AxSet)]
        );
        // It went through insert_replacing, NOT the append-only insert path.
        assert!(adapter.inserts.lock().unwrap().is_empty());
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
    fn accept_word_keeps_tap_armed_for_remaining_words() {
        let (mut engine, _adapter, _overlay) = engine();
        let visible: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let v = Arc::clone(&visible);
        engine.set_accept_subscription(AcceptSubscription::new(
            Subscription::new(0),
            move |vis| {
                v.lock().unwrap().push(vis);
                Ok(())
            },
            |_delay| Ok(()),
            |_act| Ok(()),
        ));

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine
            .on_completion(&requests[0], "alpha beta".into())
            .unwrap();
        assert_eq!(*visible.lock().unwrap(), vec![true], "armed on show");

        // A word accept inserts the first word and emits UpdateGhost (no Hide),
        // so the tap must stay armed — no `false` — for accepting the remainder.
        engine.on_accept(AcceptAction::Word).unwrap();
        assert_eq!(
            *visible.lock().unwrap(),
            vec![true],
            "tap stays armed across a partial word accept"
        );
    }

    #[test]
    fn completion_multi_cycles_overlay_and_wraps() {
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();

        engine
            .on_completion_multi(
                &requests[0],
                vec!["alpha".into(), "beta".into(), "gamma".into()],
            )
            .unwrap();
        engine.on_cycle().unwrap();
        engine.on_cycle().unwrap();
        engine.on_cycle().unwrap();

        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![
                OverlayCall::Show(
                    ScreenRect {
                        x: 10.0,
                        y: 20.0,
                        w: 1.0,
                        h: 14.0,
                    },
                    "alpha".into()
                ),
                OverlayCall::Update("beta".into()),
                OverlayCall::Update("gamma".into()),
                OverlayCall::Update("alpha".into()),
            ]
        );
    }

    #[test]
    fn accept_full_inserts_the_cycled_candidate_and_hides() {
        let (mut engine, adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine
            .on_completion_multi(&requests[0], vec!["alpha".into(), "beta".into()])
            .unwrap();
        engine.on_cycle().unwrap();
        overlay.calls.lock().unwrap().clear();

        engine.on_accept(AcceptAction::Full).unwrap();

        assert_eq!(
            *adapter.inserts.lock().unwrap(),
            vec![(field(), "beta".into(), InsertStrategy::AxSet)]
        );
        assert_eq!(*overlay.calls.lock().unwrap(), vec![OverlayCall::Hide]);
    }

    #[test]
    fn dismiss_suppress_hides_overlay_and_disarms_accept_tap() {
        let (mut engine, _adapter, overlay) = engine();
        let visible: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let actions: Arc<Mutex<Vec<Option<AcceptAction>>>> = Arc::new(Mutex::new(Vec::new()));
        let v = Arc::clone(&visible);
        let a = Arc::clone(&actions);
        engine.set_accept_subscription(AcceptSubscription::new(
            Subscription::new(0),
            move |vis| {
                v.lock().unwrap().push(vis);
                Ok(())
            },
            |_delay| Ok(()),
            move |action| {
                a.lock().unwrap().push(action);
                Ok(())
            },
        ));

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine.on_completion(&requests[0], "hello".into()).unwrap();
        overlay.calls.lock().unwrap().clear();

        engine.on_dismiss_suppress().unwrap();

        assert_eq!(*overlay.calls.lock().unwrap(), vec![OverlayCall::Hide]);
        assert_eq!(*visible.lock().unwrap(), vec![true, false]);
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
    fn mirror_mode_renders_at_the_popup_anchor_even_with_a_caret_rect() {
        // MirrorOnly apps (Firefox/Zen) must render in the floating mirror window
        // (popup anchor), not at the inline caret, even when a caret rect exists.
        let mut adapter = FakeAdapter::new();
        let caret = ScreenRect {
            x: 1.0,
            y: 2.0,
            w: 1.0,
            h: 14.0,
        };
        let mirror = ScreenRect {
            x: 100.0,
            y: 200.0,
            w: 300.0,
            h: 24.0,
        };
        adapter.rect = Some(caret);
        adapter.popup = Some(mirror);
        let overlay = FakeOverlay::default();
        let mut engine = Engine::new(adapter, overlay.clone(), 200, 4, 32);
        engine.set_mirror_mode(true);

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine
            .on_completion(&requests[0], "mirrored".into())
            .unwrap();

        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![OverlayCall::Show(mirror, "mirrored".into())]
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

        // No ghost is ever shown when neither caret rect nor popup anchor exists.
        // The reconciliation emits a single idempotent Hide (machine was marked
        // showing before dispatch); crucially there is no Show.
        let calls = overlay.calls.lock().unwrap();
        assert!(
            !calls.iter().any(|c| matches!(c, OverlayCall::Show(_, _))),
            "no ghost must be shown without geometry"
        );
        assert_eq!(*calls, vec![OverlayCall::Hide]);
    }

    #[test]
    fn failed_show_reconciles_machine_so_accept_does_not_phantom_insert() {
        // No caret rect and no popup anchor → the ghost can't be placed. The
        // machine must end up NOT showing, so a subsequent accept inserts nothing
        // (the user never saw a ghost).
        let mut adapter = FakeAdapter::new();
        adapter.rect = None;
        adapter.popup = None;
        let inserts = Arc::clone(&adapter.inserts);
        let mut engine = Engine::new(adapter, FakeOverlay::default(), 200, 4, 32);

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine.on_completion(&requests[0], "nope".into()).unwrap();

        // Accept after a failed show must be a no-op: nothing was showing.
        engine.on_accept(AcceptAction::Full).unwrap();
        assert!(
            inserts.lock().unwrap().is_empty(),
            "accept after a failed show must not insert a never-seen ghost"
        );
    }

    #[test]
    fn failed_placement_does_not_count_as_shown() {
        // No geometry → ghost emitted but never presented → the buffered Shown is
        // retracted, so usage stats don't overcount an invisible suggestion.
        let mut adapter = FakeAdapter::new();
        adapter.rect = None;
        adapter.popup = None;
        let mut engine = Engine::new(adapter, FakeOverlay::default(), 200, 4, 32);

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine.on_completion(&requests[0], "nope".into()).unwrap();

        assert_eq!(engine.take_stat_events(), vec![]);
    }

    #[test]
    fn successful_placement_counts_as_shown() {
        // With geometry the ghost is presented → exactly one Shown.
        let mut engine = Engine::new(FakeAdapter::new(), FakeOverlay::default(), 200, 4, 32);

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine.on_completion(&requests[0], "nope".into()).unwrap();

        assert_eq!(engine.take_stat_events(), vec![StatEvent::Shown]);
    }

    fn other_field() -> FieldHandle {
        FieldHandle {
            app: "TextEdit".into(),
            pid: Some(42),
            element_id: "field-b".into(),
            generation: 1,
        }
    }

    #[test]
    fn focus_on_unsupported_field_yields_no_request() {
        let mut adapter = FakeAdapter::new();
        adapter.caps = unsupported_field_caps();
        let mut engine = Engine::new(adapter, FakeOverlay::default(), 200, 4, 32);

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();

        assert!(engine.on_tick(9999).unwrap().is_empty());
    }

    #[test]
    fn accept_insert_uses_strategy_from_focus_caps() {
        let mut adapter = FakeAdapter::new();
        adapter.caps = Capabilities {
            insert_strategy: InsertStrategy::SyntheticKeys,
            ..inline_caps()
        };
        let inserts = Arc::clone(&adapter.inserts);
        let mut engine = Engine::new(adapter, FakeOverlay::default(), 200, 4, 32);

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine
            .on_completion(&requests[0], "hello world".into())
            .unwrap();
        engine.on_accept(AcceptAction::Full).unwrap();

        assert_eq!(inserts.lock().unwrap()[0].2, InsertStrategy::SyntheticKeys);
    }

    #[test]
    fn synthetic_accept_hides_overlay_but_delays_tap_teardown() {
        let mut adapter = FakeAdapter::new();
        adapter.caps = Capabilities {
            insert_strategy: InsertStrategy::SyntheticKeys,
            ..inline_caps()
        };
        let overlay = FakeOverlay::default();
        let mut engine = Engine::new(adapter, overlay.clone(), 200, 4, 32);
        let visible: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let delays: Arc<Mutex<Vec<Duration>>> = Arc::new(Mutex::new(Vec::new()));
        let actions: Arc<Mutex<Vec<Option<AcceptAction>>>> = Arc::new(Mutex::new(Vec::new()));
        let v = Arc::clone(&visible);
        let d = Arc::clone(&delays);
        let a = Arc::clone(&actions);
        engine.set_accept_subscription(AcceptSubscription::new(
            Subscription::new(0),
            move |vis| {
                v.lock().unwrap().push(vis);
                Ok(())
            },
            move |delay| {
                d.lock().unwrap().push(delay);
                Ok(())
            },
            move |action| {
                a.lock().unwrap().push(action);
                Ok(())
            },
        ));

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine
            .on_completion(&requests[0], "hello world".into())
            .unwrap();
        overlay.calls.lock().unwrap().clear();
        engine.on_accept(AcceptAction::Full).unwrap();

        assert_eq!(*overlay.calls.lock().unwrap(), vec![OverlayCall::Hide]);
        assert_eq!(*visible.lock().unwrap(), vec![true]);
        assert_eq!(*actions.lock().unwrap(), vec![Some(AcceptAction::Full)]);
        assert_eq!(*delays.lock().unwrap(), vec![SYNTHETIC_INSERT_HIDE_DELAY]);
    }

    #[test]
    fn caret_rect_error_propagates_when_showing() {
        let mut adapter = FakeAdapter::new();
        adapter.fail_caret_rect = true;
        let mut engine = Engine::new(adapter, FakeOverlay::default(), 200, 4, 32);

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();

        assert_eq!(
            engine.on_completion(&requests[0], "hi".into()),
            Err(PlatformError::Timeout)
        );
    }

    #[test]
    fn insert_error_propagates_on_accept() {
        let mut adapter = FakeAdapter::new();
        adapter.fail_insert = true;
        let overlay = FakeOverlay::default();
        let mut engine = Engine::new(adapter, overlay, 200, 4, 32);

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine
            .on_completion(&requests[0], "hi there".into())
            .unwrap();

        assert_eq!(
            engine.on_accept(AcceptAction::Full),
            Err(PlatformError::StaleField)
        );
    }

    #[test]
    fn popup_anchor_error_propagates_when_showing() {
        // No caret rect forces the ShowGhost path to fall back to the popup
        // anchor; that adapter call fails, and the error must surface to the
        // caller rather than being swallowed.
        let mut adapter = FakeAdapter::new();
        adapter.rect = None;
        adapter.fail_popup = true;
        let mut engine = Engine::new(adapter, FakeOverlay::default(), 200, 4, 32);

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();

        assert_eq!(
            engine.on_completion(&requests[0], "hi".into()),
            Err(PlatformError::Timeout)
        );
    }

    #[test]
    fn secure_state_change_hides_showing_ghost() {
        let (mut engine, _adapter, overlay) = showing("hello there");

        engine
            .on_secure_state(Capabilities {
                secure: true,
                security_state: platform::SecurityState::SecureField,
                ..inline_caps()
            })
            .unwrap();

        assert_eq!(*overlay.calls.lock().unwrap(), vec![OverlayCall::Hide]);
    }

    #[test]
    fn global_secure_input_enabled_hides_showing_ghost() {
        // Distinct hard-block trigger from a secure field: global Secure Input
        // (e.g. a background password manager). The ghost must hide immediately.
        let (mut engine, _adapter, overlay) = showing("hello there");

        engine
            .on_secure_state(Capabilities {
                security_state: platform::SecurityState::SecureInputEnabled,
                ..inline_caps()
            })
            .unwrap();

        assert_eq!(*overlay.calls.lock().unwrap(), vec![OverlayCall::Hide]);
    }

    #[test]
    fn refocus_hides_showing_ghost() {
        let (mut engine, _adapter, overlay) = showing("hello there");

        engine.on_focus(other_field()).unwrap();

        assert_eq!(*overlay.calls.lock().unwrap(), vec![OverlayCall::Hide]);
    }

    #[test]
    fn dismiss_hides_a_showing_ghost() {
        let (mut engine, _adapter, overlay) = showing("hello there");

        engine.on_dismiss().unwrap();

        assert_eq!(*overlay.calls.lock().unwrap(), vec![OverlayCall::Hide]);
    }

    #[test]
    fn dismiss_stales_an_in_flight_request_at_the_engine_layer() {
        // Engine::on_dismiss wraps DismissDiscard, NOT plain Dismiss: a
        // completion already submitted to the inference worker must not pop
        // a ghost back up after the user disabled the app via the tray. A
        // regression to Event::Dismiss would pass every other dismiss test.
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();

        engine.on_dismiss().unwrap();
        engine
            .on_completion(&requests[0], "late ghost".into())
            .unwrap();

        assert!(
            !overlay
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|c| matches!(c, OverlayCall::Show(_, _))),
            "a staled in-flight completion must never show after dismiss"
        );
    }

    #[test]
    fn runtime_mid_word_setter_reaches_the_machine() {
        // The run loop calls Engine::set_allow_mid_word on every focus change
        // and Labs-switch edge; only the builder path was pinned. A broken
        // forwarder would surface live only.
        let adapter = FakeAdapter::new();
        let overlay = FakeOverlay::default();
        let mut engine = Engine::new(adapter, overlay, 200, 4, 32).with_trigger_gates(0, false);
        engine.on_focus(field()).unwrap();

        engine.on_text_changed(typed("ab", 1, 0)).unwrap(); // mid-word caret
        assert!(
            engine.on_tick(500).unwrap().is_empty(),
            "gated baseline: mid-word must not arm a request"
        );

        engine.set_allow_mid_word(true);
        engine.on_text_changed(typed("abc", 1, 1000)).unwrap();
        assert_eq!(
            engine.on_tick(1500).unwrap().len(),
            1,
            "runtime flip must reach the machine"
        );
    }

    #[test]
    fn runtime_trailing_space_setter_reaches_the_machine() {
        let (mut engine, adapter, _overlay) = engine();
        engine.set_trailing_space(true);
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine.on_completion(&requests[0], "solo".into()).unwrap();

        engine.on_accept(AcceptAction::Full).unwrap();

        let inserts = adapter.inserts.lock().unwrap();
        assert_eq!(inserts.len(), 1);
        assert_eq!(inserts[0].1, "solo ", "runtime flip applies per accept");
    }

    #[test]
    fn mirror_mode_falls_back_to_the_caret_rect_without_a_popup_anchor() {
        // MirrorOnly apps without a resolvable popup anchor must still render
        // (at the caret rect) — only the popup-wins direction was pinned.
        let adapter = FakeAdapter::new(); // popup: None, caret rect (10,20,1,14)
        let overlay = FakeOverlay::default();
        let mut engine = Engine::new(adapter, overlay.clone(), 200, 4, 32);
        engine.set_mirror_mode(true);
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine.on_completion(&requests[0], "hi".into()).unwrap();

        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![OverlayCall::Show(
                ScreenRect {
                    x: 10.0,
                    y: 20.0,
                    w: 1.0,
                    h: 14.0,
                },
                "hi".into()
            )]
        );
    }

    #[test]
    fn dismiss_with_nothing_showing_is_noop() {
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();

        let follow = engine.on_dismiss().unwrap();

        assert!(follow.is_empty());
        assert!(overlay.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn caret_move_with_nothing_showing_is_noop() {
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();

        engine.on_caret_moved(field(), 5).unwrap();

        assert!(overlay.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn arms_accept_tap_via_popup_anchor_path() {
        let mut adapter = FakeAdapter::new();
        adapter.rect = None;
        adapter.popup = Some(ScreenRect {
            x: 1.0,
            y: 2.0,
            w: 200.0,
            h: 24.0,
        });
        let overlay = FakeOverlay::default();
        let mut engine = Engine::new(adapter, overlay, 200, 4, 32);

        let visible: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let v = Arc::clone(&visible);
        engine.set_accept_subscription(AcceptSubscription::new(
            Subscription::new(0),
            move |vis| {
                v.lock().unwrap().push(vis);
                Ok(())
            },
            |_delay| Ok(()),
            |_action| Ok(()),
        ));

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine.on_completion(&requests[0], "popup".into()).unwrap();

        assert_eq!(*visible.lock().unwrap(), vec![true]);
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

        engine.on_caret_moved(field(), 99).unwrap();

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
    fn double_accept_word_exhausts_completion_inserts_last_word_and_hides() {
        // A 2-word completion accepted word-by-word: the first AcceptWord
        // advances the ghost, the second exhausts it — inserting the final word,
        // hiding the overlay, and disarming the accept tap.
        let (mut engine, adapter, overlay) = showing("world there");
        let visible: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let v = Arc::clone(&visible);
        engine.set_accept_subscription(AcceptSubscription::new(
            Subscription::new(0),
            move |vis| {
                v.lock().unwrap().push(vis);
                Ok(())
            },
            |_delay| Ok(()),
            |_action| Ok(()),
        ));

        // First word: advances the ghost (no Hide yet).
        engine.on_accept(AcceptAction::Word).unwrap();
        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![OverlayCall::Update("there".into())],
            "first AcceptWord advances the ghost without hiding"
        );

        // Second word: exhausts the completion.
        engine.on_accept(AcceptAction::Word).unwrap();

        assert_eq!(
            *adapter.inserts.lock().unwrap(),
            vec![
                (field(), "world ".into(), InsertStrategy::AxSet),
                (field(), "there".into(), InsertStrategy::AxSet),
            ],
            "the second AcceptWord inserts the final word"
        );
        assert_eq!(
            overlay.calls.lock().unwrap().last(),
            Some(&OverlayCall::Hide),
            "exhausting the completion hides the overlay"
        );
        // The accept tap was armed on show, then disarmed when the ghost hid
        // (AxSet insert takes the immediate-disarm path, ending visible=false).
        assert_eq!(
            *visible.lock().unwrap(),
            vec![false],
            "accept tap is disarmed once the completion is exhausted"
        );
    }

    #[test]
    fn preview_accept_word_exposes_inserted_text_for_host_reconciliation() {
        let (engine, _adapter, _overlay) = showing("world there friend");

        assert_eq!(
            engine.preview_accept_insert(AcceptAction::Word),
            Some((field(), "world ".into(), 0))
        );
    }

    #[test]
    fn preview_accept_full_exposes_remaining_text_for_host_reconciliation() {
        let (engine, _adapter, _overlay) = showing("world there friend");

        assert_eq!(
            engine.preview_accept_insert(AcceptAction::Full),
            Some((field(), "world there friend".into(), 0))
        );
    }

    #[test]
    fn accept_word_advances_caret_in_showing_state() {
        // After AcceptWord the engine's SuggestionMachine should advance
        // the internal caret by the number of chars in the accepted word.
        // This verifies the fix documented in A1a plan Task 6.
        //
        // "world there friend": accepting "world " (6 chars) from caret 1
        // advances the tracked caret to 7.  A subsequent on_caret_moved at
        // position 7 must leave the ghost showing (no Hide command) because
        // the machine now agrees on the caret position.
        let (mut engine, _adapter, overlay) = showing("world there friend");

        engine.on_accept(AcceptAction::Word).unwrap();
        overlay.calls.lock().unwrap().clear();

        // Caret arrives at the advanced position — must NOT trigger a hide.
        engine.on_caret_moved(field(), 7).unwrap();

        assert!(
            overlay.calls.lock().unwrap().is_empty(),
            "expected ghost to remain visible after caret moves to accepted position"
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
        // Assert the observable request shape, not the machine's internal
        // generation/snapshot counter values (those are incidental to how many
        // boundary advances happened; pinning them couples the test to internals).
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.field, field());
        assert_eq!(request.prompt, "hello");
        assert_eq!(request.max_tokens, 32);

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

    #[test]
    fn overlay_show_error_propagates() {
        // A failing overlay must surface its error to the caller rather than
        // being swallowed — the show→accept→hide cycle depends on it.
        struct ErroringOverlay;
        impl OverlayPresenter for ErroringOverlay {
            fn show_ghost(&mut self, _rect: ScreenRect, _text: &str) -> Result<(), PlatformError> {
                Err(PlatformError::Timeout)
            }
            fn update_ghost(&mut self, _text: &str) -> Result<(), PlatformError> {
                Ok(())
            }
            fn hide(&mut self) -> Result<(), PlatformError> {
                Ok(())
            }
        }

        let mut engine = Engine::new(FakeAdapter::new(), ErroringOverlay, 200, 4, 32);
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();

        let result = engine.on_completion(&requests[0], "hello".into());

        assert_eq!(result, Err(PlatformError::Timeout));
    }

    #[test]
    fn update_ghost_error_propagates_on_word_accept() {
        // Accepting a word emits UpdateGhost for the remaining suggestion; a
        // failing update must surface, not be swallowed.
        struct UpdateFailsOverlay;
        impl OverlayPresenter for UpdateFailsOverlay {
            fn show_ghost(&mut self, _rect: ScreenRect, _text: &str) -> Result<(), PlatformError> {
                Ok(())
            }
            fn update_ghost(&mut self, _text: &str) -> Result<(), PlatformError> {
                Err(PlatformError::Timeout)
            }
            fn hide(&mut self) -> Result<(), PlatformError> {
                Ok(())
            }
        }

        let mut engine = Engine::new(FakeAdapter::new(), UpdateFailsOverlay, 200, 4, 32);
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        // Multi-word completion so a word accept leaves a remainder → UpdateGhost.
        engine
            .on_completion(&requests[0], "alpha beta gamma".into())
            .unwrap();

        assert_eq!(
            engine.on_accept(AcceptAction::Word),
            Err(PlatformError::Timeout)
        );
    }

    #[test]
    fn hide_error_propagates_on_dismiss() {
        // Dismissing a shown suggestion must surface a failing overlay hide.
        struct HideFailsOverlay;
        impl OverlayPresenter for HideFailsOverlay {
            fn show_ghost(&mut self, _rect: ScreenRect, _text: &str) -> Result<(), PlatformError> {
                Ok(())
            }
            fn update_ghost(&mut self, _text: &str) -> Result<(), PlatformError> {
                Ok(())
            }
            fn hide(&mut self) -> Result<(), PlatformError> {
                Err(PlatformError::Timeout)
            }
        }

        let mut engine = Engine::new(FakeAdapter::new(), HideFailsOverlay, 200, 4, 32);
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine.on_completion(&requests[0], "hello".into()).unwrap();

        assert_eq!(engine.on_dismiss(), Err(PlatformError::Timeout));
    }

    #[test]
    fn with_trigger_gates_suppresses_mid_word_requests() {
        // The builder must actually forward to the machine: with mid-word
        // suppression on, a caret splitting a word arms no request...
        let mut gated = Engine::new(FakeAdapter::new(), FakeOverlay::default(), 200, 4, 32)
            .with_trigger_gates(0, false);
        gated.on_focus(field()).unwrap();
        gated.on_text_changed(typed("ab", 1, 0)).unwrap();
        assert!(
            gated.on_tick(500).unwrap().is_empty(),
            "mid-word change must not arm a request when allow_mid_word=false"
        );

        // ...while the permissive default (no gates) does arm one for the same
        // input, proving the gate — not some other condition — caused suppression.
        let mut ungated = Engine::new(FakeAdapter::new(), FakeOverlay::default(), 200, 4, 32);
        ungated.on_focus(field()).unwrap();
        ungated.on_text_changed(typed("ab", 1, 0)).unwrap();
        assert_eq!(ungated.on_tick(500).unwrap().len(), 1);
    }

    #[test]
    fn show_ghost_is_safe_without_an_accept_subscription() {
        // The engine is shown a completion before any accept subscription was
        // installed. The tap arming must no-op rather than panic.
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();

        let result = engine.on_completion(&requests[0], "hello".into());

        assert!(result.is_ok());
        assert!(matches!(
            overlay.calls.lock().unwrap().last(),
            Some(OverlayCall::Show(_, _))
        ));
    }
}
