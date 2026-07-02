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
    AcceptAction, Capabilities, CorrectionRange, FieldHandle, InsertStrategy, KeyInterceptMode,
    OverlayPlacement, OverlayPresenter, PlatformAdapter, SecurityState, Toolkit,
};
use std::time::Duration;

const SYNTHETIC_INSERT_HIDE_DELAY: Duration = Duration::from_millis(50);

/// A text edit reported by the host, carrying the contract's metadata: the
/// `edit` kind and `trigger` policy (which the machine gates on). `inserted_text`
/// carries the host-derived insertion delta when there is an established prior
/// value; consumers that need privacy-preserving typing history can use it
/// without storing the whole field snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextChange {
    pub field: FieldHandle,
    pub value: String,
    pub caret: usize,
    pub edit: EditKind,
    pub inserted_text: Option<String>,
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
    pub domain: Option<String>,
    pub snapshot: SnapshotId,
    pub prompt: String,
    pub max_tokens: usize,
    pub kind: RequestKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestKind {
    Completion,
    GrammarFix {
        word: String,
        left_ctx: String,
        correction_range: CorrectionRange,
    },
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

    /// Drop + re-register the platform's accept tap against the current
    /// keymap (recorder 5b live rebind). `Ok` with no subscription. Callers
    /// must NOT persist a rebind when this returns `Err` — the registered
    /// keys and the persisted config would desync.
    pub fn rearm_accept_keys(&self) -> Result<(), platform::PlatformError> {
        self.accept
            .as_ref()
            .map_or(Ok(()), |accept| accept.rearm_accept_tap())
    }

    fn set_tap_visible(
        &self,
        visible: bool,
        action: Option<AcceptAction>,
    ) -> Result<(), platform::PlatformError> {
        if let Some(accept) = &self.accept {
            if visible {
                accept.set_accept_action(action)?;
                accept.set_suggestion_visible(true)?;
            } else {
                accept.set_suggestion_visible(false)?;
                accept.set_accept_action(action)?;
            }
        }
        Ok(())
    }

    fn hide_tap_after(&self, delay: Duration) -> Result<(), platform::PlatformError> {
        if let Some(accept) = &self.accept {
            accept.hide_suggestion_after(delay)?;
        }
        Ok(())
    }

    fn reconcile_visible_failure(&mut self) {
        let _ = self.overlay.hide();
        let _ = self.set_tap_visible(false, None);
        let _ = self.machine.on_event(Event::Dismiss);
    }

    /// Like `reconcile_visible_failure`, but for a failure DURING a ghost show:
    /// the machine has already transitioned to showing, so also rewind its
    /// last-shown bookkeeping (`cancel_last_shown`) before dismissing.
    fn reconcile_failed_show(&mut self) {
        self.machine.cancel_last_shown();
        self.reconcile_visible_failure();
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

    /// Force-show the suggestion the engine currently holds (Item 4 always-on
    /// hotkey): re-present the on-screen candidate verbatim — same candidate, no
    /// rotation (unlike `on_cycle`) and no fresh inference. A no-op when nothing
    /// is currently held.
    pub fn on_force_show(&mut self) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        let commands = self.machine.on_event(Event::ForceShow);
        self.dispatch(commands)
    }

    /// Offer a local replacement (emoji/thesaurus/typo) with one or more
    /// candidates (design spec §8/§16).
    pub fn on_replacement(
        &mut self,
        field: &FieldHandle,
        candidates: Vec<String>,
        replace_left: usize,
    ) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        let commands = self
            .machine
            .offer_replacement_multi(field, candidates, replace_left);
        self.dispatch(commands)
    }

    pub fn arm_manual_grammar_request(&mut self, field: &FieldHandle) -> Option<(u64, SnapshotId)> {
        self.machine.arm_manual_grammar_request(field)
    }

    pub fn on_correction(
        &mut self,
        request: &CompletionRequest,
        suggestion: String,
        correction_range: CorrectionRange,
    ) -> Result<Vec<CompletionRequest>, platform::PlatformError> {
        let commands = self.machine.on_event(Event::CorrectionReady {
            generation: request.generation,
            field: request.field.clone(),
            snapshot: request.snapshot,
            original: match &request.kind {
                RequestKind::GrammarFix { word, .. } => word.clone(),
                RequestKind::Completion => String::new(),
            },
            suggestion,
            correction_range,
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
            AcceptAction::Correction => Event::AcceptCorrection,
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

    pub fn preview_accept_correction(&self) -> Option<(FieldHandle, String, CorrectionRange)> {
        self.machine.preview_accept_correction()
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
                    domain: None,
                    snapshot,
                    prompt,
                    max_tokens: self.max_tokens,
                    kind: RequestKind::Completion,
                }),
                Command::ShowGhost { field, text, .. } => {
                    // Inline placement uses the caret rect; popup mode (no caret
                    // geometry) falls back to the adapter's popup anchor. Mirror
                    // mode (MirrorOnly apps) renders at the popup/mirror anchor
                    // directly, since these apps have no usable inline caret.
                    // Asymmetry is intentional: `Ok(None)` (no geometry) falls
                    // back to the other anchor, but an `Err` (a real AX failure)
                    // is fail-loud — it aborts the dispatch rather than papering
                    // over a broken accessibility tree with a fallback anchor.
                    let anchor = if self.mirror_mode {
                        self.adapter
                            .popup_anchor(&field)
                            .and_then(|rect| match rect {
                                Some(rect) => Ok(Some(rect)),
                                None => self.adapter.caret_rect(&field),
                            })
                    } else {
                        self.adapter.caret_rect(&field).and_then(|rect| match rect {
                            Some(rect) => Ok(Some(rect)),
                            None => self.adapter.popup_anchor(&field),
                        })
                    };
                    let rect = match anchor {
                        Ok(rect) => rect,
                        Err(err) => {
                            // A real AX failure resolving the anchor. The machine
                            // already transitioned to showing; reconcile before
                            // returning so the Shown stat is retracted and a later
                            // accept cannot phantom-insert an unpainted ghost —
                            // mirroring the show_ghost/set_tap_visible paths below.
                            self.reconcile_failed_show();
                            return Err(err);
                        }
                    };
                    if let Some(rect) = rect {
                        if let Err(err) = self.overlay.show_ghost(rect, &text) {
                            // The machine has already transitioned to showing,
                            // but the UI never painted. Reconcile before returning
                            // so a later accept cannot insert an invisible ghost.
                            self.reconcile_failed_show();
                            return Err(err);
                        }
                        if let Err(err) = self.set_tap_visible(true, Some(AcceptAction::Full)) {
                            // The ghost was painted but cannot be accepted. Reconcile
                            // immediately so a visible-but-unarmed suggestion does not
                            // remain in the UI or machine state.
                            self.reconcile_failed_show();
                            return Err(err);
                        }
                    } else {
                        // No caret rect and no popup anchor: we cannot place the
                        // ghost. The machine already marked itself showing, so
                        // reconcile it back to not-showing (below) — otherwise its
                        // state would lie and a later accept could insert a ghost
                        // the user never saw.
                        show_failed = true;
                    }
                }
                Command::ShowCorrection {
                    field,
                    suggestion,
                    correction_range,
                    ..
                } => {
                    let anchor = self
                        .adapter
                        .text_range_rect(&field, correction_range)
                        .and_then(|rect| match rect {
                            Some(rect) => Ok(Some(rect)),
                            None => self.adapter.caret_rect(&field).and_then(|rect| match rect {
                                Some(rect) => Ok(Some(rect)),
                                None => self.adapter.popup_anchor(&field),
                            }),
                        });
                    let rect = match anchor {
                        Ok(rect) => rect,
                        Err(err) => {
                            self.reconcile_failed_show();
                            return Err(err);
                        }
                    };
                    if let Some(rect) = rect {
                        if let Err(err) = self.overlay.show_correction(rect, &suggestion) {
                            self.reconcile_failed_show();
                            return Err(err);
                        }
                        if let Err(err) = self.set_tap_visible(true, Some(AcceptAction::Correction))
                        {
                            self.reconcile_failed_show();
                            return Err(err);
                        }
                    } else {
                        show_failed = true;
                    }
                }
                Command::Insert { field, text } => {
                    // Contract: the adapter must tag this self-inserted text so it is NOT
                    // fed back to the engine as a TextChanged event. Failure breaks the
                    // show→accept→hide cycle.
                    let strategy = self.caps.insert_strategy;
                    if let Err(err) = self.adapter.insert(&field, &text, strategy) {
                        self.reconcile_visible_failure();
                        return Err(err);
                    }
                    // Cross-crate invariant: this flag is consumed by the *next*
                    // `Hide` to delay the synthetic-keys tap teardown. Correct only
                    // because `engine_core` always emits an `Insert`/`Replace`
                    // immediately followed by its `Hide`; reordering them there
                    // would silently misapply (or skip) the teardown delay here.
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
                    if let Err(err) =
                        self.adapter
                            .insert_replacing(&field, &text, replace_left, strategy)
                    {
                        self.reconcile_visible_failure();
                        return Err(err);
                    }
                    delay_next_hide = strategy == InsertStrategy::SyntheticKeys;
                }
                Command::ReplaceRange {
                    field,
                    expected_text,
                    text,
                    correction_range,
                } => {
                    let strategy = self.caps.insert_strategy;
                    if let Err(err) = self.adapter.insert_replacing_range(
                        &field,
                        &expected_text,
                        &text,
                        correction_range,
                        strategy,
                    ) {
                        self.reconcile_visible_failure();
                        return Err(err);
                    }
                    delay_next_hide = strategy == InsertStrategy::SyntheticKeys;
                }
                Command::UpdateGhost { text, .. } => {
                    if let Err(err) = self.overlay.update_ghost(&text) {
                        self.reconcile_visible_failure();
                        return Err(err);
                    }
                }
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
            inserted_text: None,
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
        ShowCorrection(ScreenRect, String),
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
        fn show_correction(
            &mut self,
            rect: ScreenRect,
            suggestion: &str,
        ) -> Result<(), PlatformError> {
            self.calls
                .lock()
                .unwrap()
                .push(OverlayCall::ShowCorrection(rect, suggestion.into()));
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
    /// A recorded range replacement insert: (field, expected_text, text, range, strategy).
    type RangeReplacingInsert = (FieldHandle, String, String, CorrectionRange, InsertStrategy);

    #[derive(Clone)]
    struct FakeAdapter {
        caps: Capabilities,
        rect: Option<ScreenRect>,
        popup: Option<ScreenRect>,
        fail_caret_rect: bool,
        fail_popup: bool,
        fail_range_rect: bool,
        fail_insert: bool,
        inserts: Arc<Mutex<Vec<(FieldHandle, String, InsertStrategy)>>>,
        replacing_inserts: Arc<Mutex<Vec<ReplacingInsert>>>,
        range_rect: Option<ScreenRect>,
        range_replacing_inserts: Arc<Mutex<Vec<RangeReplacingInsert>>>,
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
                fail_range_rect: false,
                fail_insert: false,
                inserts: Arc::new(Mutex::new(Vec::new())),
                replacing_inserts: Arc::new(Mutex::new(Vec::new())),
                range_rect: Some(ScreenRect {
                    x: 30.0,
                    y: 40.0,
                    w: 20.0,
                    h: 12.0,
                }),
                range_replacing_inserts: Arc::new(Mutex::new(Vec::new())),
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
        fn text_range_rect(
            &self,
            _field: &FieldHandle,
            _range: CorrectionRange,
        ) -> Result<Option<ScreenRect>, PlatformError> {
            if self.fail_range_rect {
                return Err(PlatformError::UnsupportedField {
                    reason: "bad correction range".into(),
                });
            }
            Ok(self.range_rect)
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
        fn insert_replacing_range(
            &self,
            field: &FieldHandle,
            expected_text: &str,
            text: &str,
            range: CorrectionRange,
            strategy: InsertStrategy,
        ) -> Result<Inserted, PlatformError> {
            if self.fail_insert {
                return Err(PlatformError::StaleField);
            }
            self.range_replacing_inserts.lock().unwrap().push((
                field.clone(),
                expected_text.into(),
                text.into(),
                range,
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

    fn grammar_request(
        engine: &mut Engine<FakeAdapter, FakeOverlay>,
        range: CorrectionRange,
    ) -> CompletionRequest {
        let (generation, snapshot) = engine
            .arm_manual_grammar_request(&field())
            .expect("grammar request armed");
        CompletionRequest {
            generation,
            field: field(),
            domain: None,
            snapshot,
            prompt: String::new(),
            max_tokens: 8,
            kind: RequestKind::GrammarFix {
                word: "teh".into(),
                left_ctx: "teh".into(),
                correction_range: range,
            },
        }
    }

    #[test]
    fn replacement_accept_forwards_replace_left_through_dispatch() {
        // Integration step 3: a replacement accept must reach the adapter via
        // `insert_replacing` carrying `replace_left` (not the plain insert path).
        let (mut engine, adapter, _overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine
            .on_replacement(&field(), vec!["😄".into()], 5)
            .unwrap();
        engine.on_accept(AcceptAction::Full).unwrap();
        assert_eq!(
            *adapter.replacing_inserts.lock().unwrap(),
            vec![(field(), "😄".to_string(), 5, InsertStrategy::AxSet)]
        );
        // It went through insert_replacing, NOT the append-only insert path.
        assert!(adapter.inserts.lock().unwrap().is_empty());
    }

    #[test]
    fn on_correction_shows_correction_with_range_anchor() {
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();
        let range = CorrectionRange { start: 0, end: 3 };
        let request = grammar_request(&mut engine, range);

        engine.on_correction(&request, "the".into(), range).unwrap();

        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![OverlayCall::ShowCorrection(
                ScreenRect {
                    x: 30.0,
                    y: 40.0,
                    w: 20.0,
                    h: 12.0,
                },
                "the".into(),
            )]
        );
    }

    #[test]
    fn correction_range_geometry_error_fails_closed_without_caret_fallback() {
        let mut adapter = FakeAdapter::new();
        adapter.fail_range_rect = true;
        let range_replacing = Arc::clone(&adapter.range_replacing_inserts);
        let overlay = FakeOverlay::default();
        let mut engine = Engine::new(adapter, overlay.clone(), 200, 4, 32);
        engine.on_focus(field()).unwrap();
        let range = CorrectionRange {
            start: 99,
            end: 100,
        };
        let request = grammar_request(&mut engine, range);

        assert_eq!(
            engine.on_correction(&request, "the".into(), range),
            Err(PlatformError::UnsupportedField {
                reason: "bad correction range".into(),
            })
        );

        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![OverlayCall::Hide],
            "invalid correction ranges must not fall back to a caret-anchored correction"
        );
        engine.on_accept(AcceptAction::Correction).unwrap();
        assert!(
            range_replacing.lock().unwrap().is_empty(),
            "a correction that never showed must not remain accept-able"
        );
    }

    #[test]
    fn accept_correction_emits_replace_range() {
        let (mut engine, adapter, _overlay) = engine();
        engine.on_focus(field()).unwrap();
        let range = CorrectionRange { start: 0, end: 3 };
        let request = grammar_request(&mut engine, range);
        engine.on_correction(&request, "the".into(), range).unwrap();

        engine.on_accept(AcceptAction::Correction).unwrap();

        assert_eq!(
            *adapter.range_replacing_inserts.lock().unwrap(),
            vec![(
                field(),
                "teh".to_string(),
                "the".to_string(),
                range,
                InsertStrategy::AxSet
            )]
        );
        assert!(adapter.inserts.lock().unwrap().is_empty());
        assert!(adapter.replacing_inserts.lock().unwrap().is_empty());
    }

    #[test]
    fn stale_correction_result_is_ignored_after_text_changes() {
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();
        let range = CorrectionRange { start: 0, end: 3 };
        let request = grammar_request(&mut engine, range);
        engine.on_text_changed(typed("tehx", 4, 10)).unwrap();

        engine.on_correction(&request, "the".into(), range).unwrap();

        assert!(
            overlay.calls.lock().unwrap().is_empty(),
            "stale correction must not be shown against the newer snapshot"
        );
    }

    #[test]
    fn on_replacement_gated_after_suppress_records_no_show() {
        // Engine-layer gate: after Esc-suppress in the focused field, a
        // replacement offer must be swallowed by the machine guard — the overlay
        // never gets a Show and no Shown stat is recorded.
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine.on_dismiss_suppress().unwrap();
        overlay.calls.lock().unwrap().clear();
        let _ = engine.take_stat_events();

        engine
            .on_replacement(&field(), vec!["😄".into(), "🙂".into()], 5)
            .unwrap();

        let calls = overlay.calls.lock().unwrap();
        assert!(
            !calls.iter().any(|c| matches!(c, OverlayCall::Show(_, _))),
            "a suppressed replacement must not show a ghost: {calls:?}"
        );
        assert!(!engine.take_stat_events().contains(&StatEvent::Shown));
    }

    #[test]
    fn on_replacement_multi_cycles_then_accepts_the_cycled_candidate_via_insert_replacing() {
        // A MULTI-candidate replacement must cycle at the engine layer and the
        // ACCEPTED cycled candidate ("huge") must reach the adapter via
        // insert_replacing carrying replace_left — not the first candidate, and
        // not the append-only insert path. Existing replacement coverage only
        // passes a single candidate and never cycles.
        let (mut engine, adapter, _overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine
            .on_replacement(&field(), vec!["large".into(), "huge".into()], 3)
            .unwrap();
        engine.on_cycle().unwrap();
        engine.on_accept(AcceptAction::Full).unwrap();
        assert_eq!(
            *adapter.replacing_inserts.lock().unwrap(),
            vec![(field(), "huge".to_string(), 3, InsertStrategy::AxSet)],
            "the cycled candidate replaces via insert_replacing with replace_left"
        );
        assert!(adapter.inserts.lock().unwrap().is_empty());
    }

    #[test]
    fn insert_replacing_error_propagates_on_replacement_accept() {
        // The Replace dispatch branch uses `?` exactly like Insert; a regression
        // that swallowed the replacement adapter's error (or dropped the `?`)
        // would pass every other test, since fail_insert is otherwise only
        // exercised against the plain insert path.
        let mut adapter = FakeAdapter::new();
        adapter.fail_insert = true;
        let overlay = FakeOverlay::default();
        let mut engine = Engine::new(adapter, overlay.clone(), 200, 4, 32);
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
            |_| Ok(()),
            move |action| {
                a.lock().unwrap().push(action);
                Ok(())
            },
        ));

        engine.on_focus(field()).unwrap();
        engine
            .on_replacement(&field(), vec!["😄".into()], 5)
            .unwrap();
        overlay.calls.lock().unwrap().clear();
        visible.lock().unwrap().clear();
        actions.lock().unwrap().clear();

        // The Replace dispatch branch fails mid-batch (insert_replacing errors
        // before the trailing Hide), so reconcile_visible_failure must run its
        // FULL effect: hide the ghost, disarm the accept tap, and dismiss the
        // machine so the engine is not left wedged.
        assert_eq!(
            engine.on_accept(AcceptAction::Full),
            Err(PlatformError::StaleField),
            "a failing insert_replacing must surface, not be swallowed"
        );
        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![OverlayCall::Hide],
            "a failed replacement accept must hide the stale replacement ghost"
        );
        assert_eq!(
            *visible.lock().unwrap(),
            vec![false],
            "a failed replacement accept must disarm the accept tap"
        );
        assert_eq!(
            *actions.lock().unwrap(),
            vec![None],
            "a failed replacement accept must clear the accept action"
        );

        // Not wedged: the Dismiss reset means a fresh focus cycle still produces
        // a request, and a follow-up accept with no pending suggestion is a
        // no-op (no insert, no error).
        overlay.calls.lock().unwrap().clear();
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("y", 1, 1000)).unwrap();
        assert_eq!(
            engine.on_tick(1500).unwrap().len(),
            1,
            "engine must remain usable after a failed replacement accept"
        );
        assert_eq!(
            engine.on_accept(AcceptAction::Full),
            Ok(vec![]),
            "a follow-up accept with nothing pending must be a no-op, not wedged"
        );
    }

    #[test]
    fn rearm_accept_keys_forwards_when_subscribed_and_noops_without() {
        // Recorder 5b slice 2: the run loop's live-rebind path reaches the
        // platform's rearm hook through the engine (the subscription's sole
        // owner). No subscription = Ok (headless/test engines).
        let (bare_engine, _adapter, _overlay) = engine();
        assert!(bare_engine.rearm_accept_keys().is_ok());

        let (mut engine, _adapter, _overlay) = engine();
        let calls = Arc::new(Mutex::new(0usize));
        let c = Arc::clone(&calls);
        let sub = AcceptSubscription::new(Subscription::new(0), |_| Ok(()), |_| Ok(()), |_| Ok(()))
            .with_rearm(move || {
                *c.lock().unwrap() += 1;
                Err(platform::PlatformError::Timeout)
            });
        engine.set_accept_subscription(sub);
        // Forwards AND surfaces the platform's Err (persist gating depends
        // on it).
        assert!(engine.rearm_accept_keys().is_err());
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    #[test]
    fn surfaced_request_has_no_domain_at_engine_layer() {
        // dispatch builds CompletionRequest from a RequestCompletion command with
        // `domain: None` (the engine layer does not resolve a domain — the host
        // loop attaches one downstream). Pin that the surfaced request carries no
        // domain so a regression that smuggled one in here would be caught.
        let (mut engine, _adapter, _overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("hello ", 6, 1000)).unwrap();
        let requests = engine.on_tick(1200).unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].domain, None,
            "the engine layer surfaces a request with no domain"
        );
    }

    #[test]
    fn completion_multi_with_empty_candidates_shows_nothing() {
        // An empty candidate vec has nothing to shape, so the machine emits no
        // ShowGhost: the overlay is never asked to present and no Shown stat is
        // buffered. on_completion_multi returns Ok with an empty dispatch.
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        overlay.calls.lock().unwrap().clear();
        let _ = engine.take_stat_events();

        let followups = engine.on_completion_multi(&requests[0], vec![]).unwrap();

        assert!(followups.is_empty(), "no follow-up requests dispatched");
        assert!(
            overlay.calls.lock().unwrap().is_empty(),
            "an empty-candidate completion must never show a ghost"
        );
        assert_eq!(
            engine.take_stat_events(),
            vec![],
            "an empty-candidate completion records no Shown stat"
        );
    }

    #[test]
    fn rearm_accept_keys_forwards_ok_when_subscribed() {
        // With a subscription whose rearm closure returns Ok, rearm_accept_keys
        // forwards to it and surfaces the Ok — and the closure is actually invoked
        // (the companion test pins the Err direction and the no-subscription Ok).
        let (mut engine, _adapter, _overlay) = engine();
        let calls = Arc::new(Mutex::new(0usize));
        let c = Arc::clone(&calls);
        let sub = AcceptSubscription::new(Subscription::new(0), |_| Ok(()), |_| Ok(()), |_| Ok(()))
            .with_rearm(move || {
                *c.lock().unwrap() += 1;
                Ok(())
            });
        engine.set_accept_subscription(sub);

        assert!(engine.rearm_accept_keys().is_ok());
        assert_eq!(
            *calls.lock().unwrap(),
            1,
            "the rearm closure is invoked exactly once"
        );
    }

    #[test]
    fn accept_tap_arm_error_propagates_through_dispatch() {
        // The ShowGhost dispatch arms the accept tap via set_tap_visible(true);
        // a failure in the subscription's set_suggestion_visible must surface
        // through on_completion, not be swallowed — the same load-bearing
        // error-propagation contract already pinned for the overlay and adapter
        // sinks. A swallowed arm error would silently leave the user unable to
        // Tab-accept a ghost they can see.
        let (mut engine, adapter, overlay) = engine();
        let inserts = Arc::clone(&adapter.inserts);
        engine.set_accept_subscription(AcceptSubscription::new(
            Subscription::new(0),
            // Fail only while ARMING (visible=true) so an incidental disarm
            // stays Ok and the error is attributable to the show path.
            |visible| {
                if visible {
                    Err(platform::PlatformError::Timeout)
                } else {
                    Ok(())
                }
            },
            |_delay| Ok(()),
            |_action| Ok(()),
        ));
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        assert_eq!(
            engine.on_completion(&requests[0], "hello world".into()),
            Err(platform::PlatformError::Timeout),
            "a failed accept-tap arm surfaces through dispatch"
        );
        assert!(
            overlay
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| matches!(call, OverlayCall::Hide)),
            "a painted-but-unarmed ghost must be hidden immediately"
        );

        engine.on_accept(AcceptAction::Full).unwrap();
        assert!(
            inserts.lock().unwrap().is_empty(),
            "accept after a failed accept-tap arm must not insert"
        );
    }

    #[test]
    fn arms_accept_tap_on_show_and_disarms_on_hide() {
        let (mut engine, _adapter, _overlay) = engine();
        let visible: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let actions: Arc<Mutex<Vec<Option<AcceptAction>>>> = Arc::new(Mutex::new(Vec::new()));
        let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let v = Arc::clone(&visible);
        let a = Arc::clone(&actions);
        let order_visible = Arc::clone(&order);
        let order_action = Arc::clone(&order);
        let sub = AcceptSubscription::new(
            Subscription::new(0),
            move |vis| {
                order_visible.lock().unwrap().push(if vis {
                    "visible:true"
                } else {
                    "visible:false"
                });
                v.lock().unwrap().push(vis);
                Ok(())
            },
            |_delay| Ok(()),
            move |act| {
                order_action.lock().unwrap().push(match act {
                    Some(AcceptAction::Full) => "action:full",
                    Some(AcceptAction::Word) => "action:word",
                    Some(AcceptAction::Correction) => "action:correction",
                    None => "action:none",
                });
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
        assert_eq!(
            *order.lock().unwrap(),
            vec!["action:full", "visible:true"],
            "the action must be installed before visible=true arms the platform tap"
        );

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
        assert_eq!(
            *order.lock().unwrap(),
            vec![
                "action:full",
                "visible:true",
                "visible:false",
                "action:none"
            ],
            "hide clears visibility before removing the action override"
        );
    }

    #[test]
    fn accept_word_keeps_tap_armed_for_remaining_words() {
        let (mut engine, adapter, overlay) = engine();
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
            *adapter.inserts.lock().unwrap(),
            vec![(field(), "alpha ".into(), InsertStrategy::AxSet)],
            "the first word is inserted on a partial accept"
        );
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
                    "alpha beta".into()
                ),
                OverlayCall::Update("beta".into()),
            ],
            "the ghost shows the full suggestion then updates to the remainder — no Hide"
        );
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
    fn stale_completion_multi_after_more_typing_shows_no_ghost() {
        // The single-completion staleness drop is pinned elsewhere; this pins the
        // MULTI entry point at the engine layer: a batch whose request went stale
        // (the user typed on before it arrived) is dropped and shows no overlay.
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("hello ", 6, 1000)).unwrap();
        let requests = engine.on_tick(1200).unwrap();
        engine.on_text_changed(typed("hello w", 7, 1300)).unwrap();
        let follow = engine
            .on_completion_multi(&requests[0], vec!["world".into(), "there".into()])
            .unwrap();
        assert!(follow.is_empty());
        assert!(
            overlay.calls.lock().unwrap().is_empty(),
            "a stale multi completion must show no ghost"
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
    fn dismiss_suppress_stales_inflight_request() {
        // Esc-suppress must stale an IN-FLIGHT request at the engine layer: a
        // request already dispatched to the inference worker but not yet
        // completed. When its completion finally arrives AFTER the suppress, the
        // engine must drop it — no ghost — otherwise a result for a request the
        // user already escaped could pop a suggestion back up. The sibling test
        // suppresses AFTER the completion shows; this pins the pre-completion
        // (in-flight) case.
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        assert!(
            !requests.is_empty(),
            "the tick must dispatch an in-flight completion request"
        );
        overlay.calls.lock().unwrap().clear();
        let _ = engine.take_stat_events();

        // Esc before the worker answers: the in-flight request is staled.
        engine.on_dismiss_suppress().unwrap();
        overlay.calls.lock().unwrap().clear();
        let _ = engine.take_stat_events();

        // The worker's late answer for that exact in-flight request must be dropped.
        let followups = engine.on_completion(&requests[0], "ghost".into()).unwrap();

        assert!(
            followups.is_empty(),
            "a completion for a suppressed in-flight request dispatches nothing"
        );
        assert!(
            !overlay
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|c| matches!(c, OverlayCall::Show(_, _))),
            "a suppressed in-flight request must not resurrect a ghost on late completion"
        );
        assert!(!engine.take_stat_events().contains(&StatEvent::Shown));
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
        // take_stat_events DRAINS: a second call sees nothing, not a re-emitted Shown.
        assert_eq!(engine.take_stat_events(), vec![]);
    }

    #[test]
    fn anchor_error_reconciles_machine_and_retracts_shown_stat() {
        // A real AX failure resolving the caret anchor (Err, not Ok(None)) must
        // reconcile exactly like a missing-geometry show: the buffered Shown is
        // retracted and the machine ends up NOT showing, so a later accept can't
        // phantom-insert a ghost the user never saw. Regression for the anchor
        // lookup that previously propagated Err WITHOUT reconciling, unlike the
        // sibling show_ghost/set_tap_visible error paths.
        let mut adapter = FakeAdapter::new();
        adapter.fail_caret_rect = true;
        let inserts = Arc::clone(&adapter.inserts);
        let mut engine = Engine::new(adapter, FakeOverlay::default(), 200, 4, 32);

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();

        assert_eq!(
            engine.on_completion(&requests[0], "hi".into()),
            Err(PlatformError::Timeout)
        );
        // Shown must be retracted — the ghost never painted.
        assert_eq!(engine.take_stat_events(), vec![]);
        // The machine must not be showing: a later accept inserts nothing.
        engine.on_accept(AcceptAction::Full).unwrap();
        assert!(
            inserts.lock().unwrap().is_empty(),
            "accept after an anchor-error show must not insert a never-seen ghost"
        );
    }

    #[test]
    fn completion_filtered_to_nothing_shows_no_ghost_and_no_stat() {
        // A completion whose every candidate the ranker drops (here: empty text)
        // leaves nothing to show — the machine emits no ShowGhost, so the overlay
        // is never asked to present and no Shown stat is buffered. on_completion
        // still returns Ok with an empty dispatch (no reconcile, no requests).
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        overlay.calls.lock().unwrap().clear();
        let _ = engine.take_stat_events();

        let followups = engine.on_completion(&requests[0], "".into()).unwrap();

        assert!(followups.is_empty(), "no follow-up requests dispatched");
        assert!(
            overlay.calls.lock().unwrap().is_empty(),
            "the overlay must never be asked to show a filtered-out completion"
        );
        assert_eq!(
            engine.take_stat_events(),
            vec![],
            "a completion that shows nothing must not record a Shown stat"
        );
    }

    #[test]
    fn on_completion_multi_filtered_to_nothing_shows_no_ghost_and_no_stat() {
        // The multi-candidate path mirrors the single path: when every candidate
        // the model returns is shaped away (here both trip degenerate-repetition
        // detection), the machine emits no ShowGhost, so the overlay is never
        // asked to present and no Shown stat is buffered. on_completion_multi
        // still returns Ok with an empty dispatch (no reconcile, no requests).
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        overlay.calls.lock().unwrap().clear();
        let _ = engine.take_stat_events();

        let followups = engine
            .on_completion_multi(&requests[0], vec!["ha ha ha".into(), "the the the".into()])
            .unwrap();

        assert!(followups.is_empty(), "no follow-up requests dispatched");
        assert!(
            overlay.calls.lock().unwrap().is_empty(),
            "the overlay must never be asked to show a multi completion that filtered to nothing"
        );
        assert_eq!(
            engine.take_stat_events(),
            vec![],
            "a multi completion that shows nothing must not record a Shown stat"
        );
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

        assert_eq!(
            *inserts.lock().unwrap(),
            vec![(field(), "hello world".into(), InsertStrategy::SyntheticKeys)],
            "exactly one insert of the completion into the focused field, using the focus-derived strategy"
        );
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
    fn synthetic_accept_hide_suggestion_after_error_propagates_through_dispatch() {
        // Synthetic-keys accept emits [Insert, Hide]; the Hide arm takes the
        // delayed-teardown branch and calls hide_suggestion_after (the delay
        // hook). A failure there must surface out of on_accept, not be
        // swallowed — otherwise the accept-tap teardown is silently skipped and
        // the tap can fire against an already-dismissed suggestion. Companion to
        // accept_tap_arm_error_propagates_through_dispatch, which only pins the
        // arming (visible=true) hook on the show path.
        let mut adapter = FakeAdapter::new();
        adapter.caps = Capabilities {
            insert_strategy: InsertStrategy::SyntheticKeys,
            ..inline_caps()
        };
        let overlay = FakeOverlay::default();
        let mut engine = Engine::new(adapter, overlay.clone(), 200, 4, 32);
        engine.set_accept_subscription(AcceptSubscription::new(
            Subscription::new(0),
            // Arming/disarming via set_suggestion_visible stays Ok so the error
            // is attributable solely to the delayed hide_suggestion_after hook.
            |_visible| Ok(()),
            |_delay| Err(platform::PlatformError::Timeout),
            |_action| Ok(()),
        ));

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine
            .on_completion(&requests[0], "hello world".into())
            .unwrap();
        overlay.calls.lock().unwrap().clear();

        // The Hide arm runs overlay.hide() first (succeeds), then the failing
        // hide_suggestion_after surfaces via `?`.
        assert_eq!(
            engine.on_accept(AcceptAction::Full),
            Err(PlatformError::Timeout),
            "a failing delayed-tap teardown must surface, not be swallowed"
        );
        // The overlay was still hidden before the teardown hook errored.
        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![OverlayCall::Hide],
            "the ghost is hidden before the delayed teardown failsafe runs"
        );

        // Not wedged: a fresh focus cycle still produces a request and a
        // follow-up accept with nothing pending is a no-op.
        overlay.calls.lock().unwrap().clear();
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("y", 1, 1000)).unwrap();
        assert_eq!(
            engine.on_tick(1500).unwrap().len(),
            1,
            "engine remains usable after a failed delayed-tap teardown"
        );
        assert_eq!(
            engine.on_accept(AcceptAction::Full),
            Ok(vec![]),
            "a follow-up accept with nothing pending must be a no-op"
        );
    }

    #[test]
    fn non_synthetic_hide_disarm_error_propagates_through_dispatch() {
        // Companion to the synthetic delayed-teardown test for the ordinary
        // (non-synthetic) Hide arm. A Full accept under AxSet emits
        // [Insert, Hide]; the Hide arm disarms the accept tap immediately via
        // set_tap_visible(false) -> set_suggestion_visible(false). A failure in
        // that disarm must surface out of on_accept, not be swallowed — a
        // swallowed disarm error would leave the accept tap live against a
        // dismissed suggestion.
        let (mut engine, _adapter, overlay) = engine();
        engine.set_accept_subscription(AcceptSubscription::new(
            Subscription::new(0),
            // Fail only while DISARMING (visible=false) so the show-path arm
            // (visible=true) succeeds and the error is attributable to the Hide
            // arm's disarm, not the ShowGhost arm.
            |visible| {
                if visible {
                    Ok(())
                } else {
                    Err(platform::PlatformError::Timeout)
                }
            },
            |_delay| Ok(()),
            |_action| Ok(()),
        ));

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine
            .on_completion(&requests[0], "hello world".into())
            .unwrap();
        overlay.calls.lock().unwrap().clear();

        // The Hide arm runs overlay.hide() first (succeeds), then the failing
        // set_suggestion_visible(false) disarm surfaces via `?`.
        assert_eq!(
            engine.on_accept(AcceptAction::Full),
            Err(PlatformError::Timeout),
            "a failing non-synthetic accept-tap disarm must surface, not be swallowed"
        );
        // The overlay was still hidden before the disarm hook errored.
        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![OverlayCall::Hide],
            "the ghost is hidden before the immediate disarm runs"
        );
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
    fn insert_failure_hides_stale_ui_and_engine_stays_usable() {
        // A Full accept emits [Insert, Hide]. When Insert fails, dispatch still
        // returns the original adapter error, but it must best-effort reconcile
        // visible state because the batch's trailing Hide is not reached.
        let mut adapter = FakeAdapter::new();
        adapter.fail_insert = true;
        let overlay = FakeOverlay::default();
        let mut engine = Engine::new(adapter, overlay.clone(), 200, 4, 32);
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
            |_| Ok(()),
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
        visible.lock().unwrap().clear();
        actions.lock().unwrap().clear();

        assert_eq!(
            engine.on_accept(AcceptAction::Full),
            Err(PlatformError::StaleField),
            "the Insert error must surface to the caller"
        );
        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![OverlayCall::Hide],
            "Insert failure must hide the stale ghost before surfacing the error"
        );
        assert_eq!(
            *visible.lock().unwrap(),
            vec![false],
            "Insert failure must disarm the accept tap"
        );
        assert_eq!(
            *actions.lock().unwrap(),
            vec![None],
            "Insert failure must clear the accept action"
        );

        // Not wedged: a fresh focus cycle still produces a request.
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("y", 1, 1000)).unwrap();
        assert_eq!(
            engine.on_tick(1500).unwrap().len(),
            1,
            "engine must remain usable after a mid-batch insert failure"
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
    fn mirror_mode_popup_anchor_error_propagates_when_showing() {
        // The non-mirror popup-error path is covered above. In mirror mode the
        // ShowGhost path calls popup_anchor FIRST (before any caret fallback), so
        // a failing popup anchor must surface even though a caret rect exists —
        // the error is fail-loud, not papered over by the caret fallback.
        let mut adapter = FakeAdapter::new(); // caret rect present by default
        adapter.fail_popup = true;
        let mut engine = Engine::new(adapter, FakeOverlay::default(), 200, 4, 32);
        engine.set_mirror_mode(true);

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
    fn on_cycle_with_nothing_showing_is_a_noop() {
        // Cycle (Down arrow) with no ghost up must do nothing: the machine has no
        // `showing` to rotate, so it emits no commands. The engine therefore never
        // asks the overlay to update/show anything and dispatches no follow-up
        // requests. (Mirrors `dismiss_with_nothing_showing_is_noop` for the rotate
        // path.)
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();

        let follow = engine.on_cycle().unwrap();

        assert!(follow.is_empty(), "no follow-up requests on an empty cycle");
        assert!(
            overlay.calls.lock().unwrap().is_empty(),
            "the overlay must not be touched when there is nothing to cycle"
        );
    }

    #[test]
    fn on_force_show_re_presents_the_held_ghost_and_keeps_the_tap_armed() {
        // The always-on Force-Show hotkey routes through `Engine::on_force_show`,
        // not just the core machine. Exercise that engine-level seam: with a ghost
        // already up, force-show must re-dispatch a `ShowGhost` to the overlay
        // (re-present verbatim, covering a prior failed placement) and must NOT
        // disarm the still-valid accept tap — a regression in the dispatch wiring
        // (forgot to dispatch, or toggled the tap off) would silently break the
        // hotkey with no other test to catch it.
        let (mut engine, _adapter, overlay) = engine();
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
            .on_completion(&requests[0], "hi there".into())
            .unwrap();
        // Drop the setup noise so we observe only the force-show effects.
        overlay.calls.lock().unwrap().clear();
        visible.lock().unwrap().clear();

        let follow = engine.on_force_show().unwrap();

        assert!(
            follow.is_empty(),
            "force-show re-presents the held candidate, never kicks a fresh request"
        );
        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![OverlayCall::Show(
                ScreenRect {
                    x: 10.0,
                    y: 20.0,
                    w: 1.0,
                    h: 14.0,
                },
                "hi there".into()
            )],
            "force-show must re-dispatch the held ghost to the overlay verbatim"
        );
        assert!(
            !visible.lock().unwrap().contains(&false),
            "the still-valid accept tap must stay armed across a force-show"
        );
    }

    #[test]
    fn on_force_show_with_nothing_showing_is_a_noop() {
        // No ghost held → the core machine emits no commands, so the engine seam
        // must touch neither the overlay nor request fresh inference.
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();

        let follow = engine.on_force_show().unwrap();

        assert!(
            follow.is_empty(),
            "no follow-up requests on an empty force-show"
        );
        assert!(
            overlay.calls.lock().unwrap().is_empty(),
            "the overlay must not be touched when there is nothing to force-show"
        );
    }

    #[test]
    fn on_force_show_re_presents_a_held_correction_as_a_correction_not_a_ghost() {
        // Companion to the ghost force-show test above, for the grammar-correction
        // path. The engine_core fix makes a held correction re-emit `ShowCorrection`
        // (not `ShowGhost`) on `ForceShow`; this pins the engine-side consequence of
        // that choice end to end. A regression that re-emitted `ShowGhost` would, at
        // this dispatch layer, anchor the overlay at the caret rect (10,20) instead
        // of the correction span (30,40) AND arm the tap as `AcceptAction::Full` —
        // which silently no-ops on a correction, leaving the hotkey dead.
        let (mut engine, _adapter, overlay) = engine();
        let actions: Arc<Mutex<Vec<Option<AcceptAction>>>> = Arc::new(Mutex::new(Vec::new()));
        let visible: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let a = Arc::clone(&actions);
        let v = Arc::clone(&visible);
        engine.set_accept_subscription(AcceptSubscription::new(
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
        ));

        engine.on_focus(field()).unwrap();
        let range = CorrectionRange { start: 0, end: 3 };
        let request = grammar_request(&mut engine, range);
        engine.on_correction(&request, "the".into(), range).unwrap();
        // Drop the setup noise so we observe only the force-show effects.
        overlay.calls.lock().unwrap().clear();
        actions.lock().unwrap().clear();
        visible.lock().unwrap().clear();

        let follow = engine.on_force_show().unwrap();

        assert!(
            follow.is_empty(),
            "force-show re-presents the held correction, never kicks a fresh request"
        );
        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![OverlayCall::ShowCorrection(
                ScreenRect {
                    x: 30.0,
                    y: 40.0,
                    w: 20.0,
                    h: 12.0,
                },
                "the".into(),
            )],
            "force-show must re-present the held correction at the correction span, \
             not a caret-anchored ghost"
        );
        assert_eq!(
            *actions.lock().unwrap(),
            vec![Some(AcceptAction::Correction)],
            "the re-shown correction must re-arm the tap as a Correction, never Full"
        );
        assert!(
            !visible.lock().unwrap().contains(&false),
            "the still-valid correction tap must stay armed across a force-show"
        );
    }

    #[test]
    fn completion_for_wrong_field_is_dropped() {
        // Staleness at the dispatch boundary: a completion stamped for a field
        // OTHER than the one the in-flight request was issued for must be dropped.
        // The machine's request-match guard (`on_completion_ready`) requires the
        // completion's field to equal the requested field, so a mismatch shows no
        // ghost — even though generation/snapshot line up.
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        overlay.calls.lock().unwrap().clear();
        let _ = engine.take_stat_events();

        // Re-stamp the in-flight request for a different field, keeping the same
        // generation/snapshot, then deliver a completion for it.
        let mut wrong = requests[0].clone();
        wrong.field = FieldHandle {
            app: "TextEdit".into(),
            pid: Some(42),
            element_id: "field-OTHER".into(),
            generation: 1,
        };

        let followups = engine.on_completion(&wrong, "ghost".into()).unwrap();

        assert!(
            followups.is_empty(),
            "a wrong-field completion dispatches no follow-up requests"
        );
        assert!(
            overlay.calls.lock().unwrap().is_empty(),
            "a completion for a non-focused field must never show a ghost"
        );
        assert_eq!(
            engine.take_stat_events(),
            vec![],
            "a dropped completion records no Shown stat"
        );
    }

    #[test]
    fn multi_completion_for_wrong_field_is_dropped() {
        // The multi-candidate twin of `completion_for_wrong_field_is_dropped`:
        // candidates stamped for a field OTHER than the in-flight request's must
        // be dropped by the same request-match guard — no ghost, no Shown stat.
        // The single-completion case is pinned; multi had no equivalent, so a
        // regression that skipped the field check only on the multi path would
        // stay green.
        let (mut engine, _adapter, overlay) = engine();
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        overlay.calls.lock().unwrap().clear();
        let _ = engine.take_stat_events();

        // Same generation/snapshot, different field.
        let mut wrong = requests[0].clone();
        wrong.field = FieldHandle {
            app: "TextEdit".into(),
            pid: Some(42),
            element_id: "field-OTHER".into(),
            generation: 1,
        };

        let followups = engine
            .on_completion_multi(&wrong, vec!["one".into(), "two".into()])
            .unwrap();

        assert!(
            followups.is_empty(),
            "a wrong-field multi-completion dispatches no follow-up requests"
        );
        assert!(
            overlay.calls.lock().unwrap().is_empty(),
            "multi candidates for a non-focused field must never show a ghost"
        );
        assert_eq!(
            engine.take_stat_events(),
            vec![],
            "a dropped multi-completion records no Shown stat"
        );
    }

    #[test]
    fn arms_accept_tap_via_popup_anchor_path() {
        let mut adapter = FakeAdapter::new();
        adapter.rect = None;
        let popup = ScreenRect {
            x: 1.0,
            y: 2.0,
            w: 200.0,
            h: 24.0,
        };
        adapter.popup = Some(popup);
        let overlay = FakeOverlay::default();
        let mut engine = Engine::new(adapter, overlay.clone(), 200, 4, 32);

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
        // With no caret rect, inline placement falls back to the popup anchor:
        // the ghost must paint AT that exact popup rect (not the caret rect, which
        // is None here) with the completion text — pins the fallback branch.
        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![OverlayCall::Show(popup, "popup".into())]
        );
    }

    #[test]
    fn inline_mode_prefers_caret_rect_over_popup_anchor() {
        // Inline mode (mirror_mode FALSE, the default). With BOTH a caret rect
        // and a popup anchor available, the ghost must paint at the CARET rect —
        // the popup anchor is only the fallback when caret geometry is absent.
        // Pins the non-mirror branch ordering in dispatch (caret first, popup as
        // `None` fallback): a swapped branch would render at the popup rect.
        let mut adapter = FakeAdapter::new();
        let caret = ScreenRect {
            x: 10.0,
            y: 20.0,
            w: 1.0,
            h: 14.0,
        };
        adapter.rect = Some(caret);
        adapter.popup = Some(ScreenRect {
            x: 500.0,
            y: 600.0,
            w: 200.0,
            h: 24.0,
        });
        let overlay = FakeOverlay::default();
        let mut engine = Engine::new(adapter, overlay.clone(), 200, 4, 32);

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine.on_completion(&requests[0], "hello".into()).unwrap();

        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![OverlayCall::Show(caret, "hello".into())],
            "inline placement uses the caret rect, not the popup anchor"
        );
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

        assert!(
            engine.on_tick(2000).unwrap().is_empty(),
            "a Manual trigger must not auto-surface a request on tick"
        );

        // Proof the trigger field is actually read and routed (not ignored):
        // the SAME text under an Automatic trigger DOES surface a request.
        let mut auto_engine = Engine::new(FakeAdapter::new(), FakeOverlay::default(), 200, 4, 32);
        auto_engine.on_focus(field()).unwrap();
        auto_engine
            .on_text_changed(typed("hello ", 6, 1000))
            .unwrap();
        assert_eq!(
            auto_engine.on_tick(2000).unwrap().len(),
            1,
            "an Automatic trigger on identical text surfaces one request, so the Manual suppression above is the trigger's doing"
        );
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

        let adapter = FakeAdapter::new();
        let inserts = Arc::clone(&adapter.inserts);
        let mut engine = Engine::new(adapter, ErroringOverlay, 200, 4, 32);
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();

        let result = engine.on_completion(&requests[0], "hello".into());

        assert_eq!(result, Err(PlatformError::Timeout));
        assert_eq!(
            engine.take_stat_events(),
            vec![],
            "a ghost that never painted must not count as shown"
        );
        engine.on_accept(AcceptAction::Full).unwrap();
        assert!(
            inserts.lock().unwrap().is_empty(),
            "accept after a failed overlay show must be a no-op"
        );
    }

    #[test]
    fn update_ghost_error_hides_stale_ui_and_propagates() {
        // Accepting a word emits UpdateGhost for the remaining suggestion; a
        // failing update must surface and clear the already-stale visible ghost.
        #[derive(Clone)]
        struct UpdateFailsOverlay {
            calls: Arc<Mutex<Vec<OverlayCall>>>,
        }
        impl OverlayPresenter for UpdateFailsOverlay {
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
                Err(PlatformError::Timeout)
            }
            fn hide(&mut self) -> Result<(), PlatformError> {
                self.calls.lock().unwrap().push(OverlayCall::Hide);
                Ok(())
            }
        }

        let overlay = UpdateFailsOverlay {
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let calls = Arc::clone(&overlay.calls);
        let mut engine = Engine::new(FakeAdapter::new(), overlay, 200, 4, 32);
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        // Multi-word completion so a word accept leaves a remainder → UpdateGhost.
        engine
            .on_completion(&requests[0], "alpha beta gamma".into())
            .unwrap();
        calls.lock().unwrap().clear();

        assert_eq!(
            engine.on_accept(AcceptAction::Word),
            Err(PlatformError::Timeout)
        );
        assert_eq!(
            *calls.lock().unwrap(),
            vec![OverlayCall::Update("beta gamma".into()), OverlayCall::Hide],
            "a failed ghost update must clear the stale visible ghost"
        );
        assert!(
            engine.on_accept(AcceptAction::Full).unwrap().is_empty(),
            "accept after a failed update must be a no-op"
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
    fn replacement_accept_under_axset_disarms_tap_immediately_not_delayed() {
        // The Replace dispatch branch sets `delay_next_hide` from the focus
        // caps' insert strategy exactly like Insert (L432): it defers the
        // accept-tap teardown ONLY under SyntheticKeys, otherwise the trailing
        // Hide disarms immediately. Replacement offers are gated to AxSet fields
        // by the machine (`offer_replacement_multi` requires
        // insert_strategy == AxSet), so the reachable replacement-accept path is
        // the AxSet/immediate-disarm direction: assert the Hide disarms the tap
        // (visible=false) rather than scheduling a delay. This is the Replace
        // counterpart to the Insert delay test and pins that Replace does NOT
        // spuriously defer teardown on the non-synthetic strategy.
        let (mut engine, adapter, overlay) = engine(); // inline_caps() => AxSet
        let replacing = Arc::clone(&adapter.replacing_inserts);
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
        // Drive to a state offering a replacement (replace_left > 0).
        engine
            .on_replacement(&field(), vec!["😄".into()], 5)
            .unwrap();
        overlay.calls.lock().unwrap().clear();
        visible.lock().unwrap().clear();
        actions.lock().unwrap().clear();

        engine.on_accept(AcceptAction::Full).unwrap();

        // The accepted replacement reached the adapter via insert_replacing
        // carrying the field's AxSet strategy...
        assert_eq!(
            *replacing.lock().unwrap(),
            vec![(field(), "😄".to_string(), 5, InsertStrategy::AxSet)]
        );
        assert_eq!(*overlay.calls.lock().unwrap(), vec![OverlayCall::Hide]);
        // ...and the trailing Hide disarmed the tap immediately (visible=false,
        // action cleared) — NO synthetic-keys teardown delay was scheduled.
        assert_eq!(*visible.lock().unwrap(), vec![false]);
        assert_eq!(*actions.lock().unwrap(), vec![None]);
        assert_eq!(*delays.lock().unwrap(), Vec::<Duration>::new());
    }

    #[test]
    fn mirror_mode_with_no_geometry_reconciles_and_shows_nothing() {
        // Mirror mode with NEITHER a popup anchor NOR a caret rect (both
        // Ok(None)) hits the same show_failed reconcile path as the non-mirror
        // no-geometry case (L358-397): cancel_last_shown + Dismiss, and the
        // overlay shows nothing. The non-mirror direction is pinned by
        // `no_overlay_when_neither_caret_nor_popup_anchor`; the mirror-mode
        // branch (popup_anchor first) was unpinned.
        let mut adapter = FakeAdapter::new();
        adapter.rect = None;
        adapter.popup = None;
        let inserts = Arc::clone(&adapter.inserts);
        let overlay = FakeOverlay::default();
        let mut engine = Engine::new(adapter, overlay.clone(), 200, 4, 32);
        engine.set_mirror_mode(true);

        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine.on_completion(&requests[0], "nope".into()).unwrap();

        // No Show ever; the reconcile emits a single idempotent Hide.
        let calls = overlay.calls.lock().unwrap();
        assert!(
            !calls.iter().any(|c| matches!(c, OverlayCall::Show(_, _))),
            "mirror mode without geometry must never show a ghost: {calls:?}"
        );
        assert_eq!(*calls, vec![OverlayCall::Hide]);
        drop(calls);

        // State reconciled: the machine is no longer showing, so a later accept
        // inserts nothing (the user never saw a ghost) and the buffered Shown
        // stat was retracted.
        engine.on_accept(AcceptAction::Full).unwrap();
        assert!(
            inserts.lock().unwrap().is_empty(),
            "accept after a failed mirror-mode show must not phantom-insert"
        );
        assert_eq!(engine.take_stat_events(), vec![]);
    }

    #[test]
    fn cycle_update_ghost_error_propagates_and_reconciles() {
        // A Cycle event rotates the visible ghost via UpdateGhost (L434-439). A
        // failing update must surface the error AND reconcile the stale visible
        // ghost (hide + disarm + Dismiss). The UpdateGhost-error path is pinned
        // only via AcceptWord; the Cycle entry into it was unpinned.
        #[derive(Clone)]
        struct UpdateFailsOverlay {
            calls: Arc<Mutex<Vec<OverlayCall>>>,
        }
        impl OverlayPresenter for UpdateFailsOverlay {
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
                Err(PlatformError::Timeout)
            }
            fn hide(&mut self) -> Result<(), PlatformError> {
                self.calls.lock().unwrap().push(OverlayCall::Hide);
                Ok(())
            }
        }

        let overlay = UpdateFailsOverlay {
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let calls = Arc::clone(&overlay.calls);
        let adapter = FakeAdapter::new();
        let inserts = Arc::clone(&adapter.inserts);
        let mut engine = Engine::new(adapter, overlay, 200, 4, 32);
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        // Multi-candidate so a Cycle rotates the ghost → UpdateGhost.
        engine
            .on_completion_multi(&requests[0], vec!["alpha".into(), "beta".into()])
            .unwrap();
        calls.lock().unwrap().clear();

        assert_eq!(
            engine.on_cycle(),
            Err(PlatformError::Timeout),
            "a failed ghost update on Cycle must surface, not be swallowed"
        );
        assert_eq!(
            *calls.lock().unwrap(),
            vec![OverlayCall::Update("beta".into()), OverlayCall::Hide],
            "a failed Cycle update must clear the stale visible ghost"
        );
        // Reconciled: the machine is dismissed, so a follow-up accept is a no-op.
        assert!(
            engine.on_accept(AcceptAction::Full).unwrap().is_empty(),
            "accept after a failed Cycle update must be a no-op"
        );
        assert!(inserts.lock().unwrap().is_empty());
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

        // Showing a ghost yields no follow-up completion requests, and with no
        // subscription installed the tap-arm no-ops — so the only effect is the
        // ghost painted at the default caret rect with the exact text.
        assert_eq!(result, Ok(vec![]));
        assert_eq!(
            *overlay.calls.lock().unwrap(),
            vec![OverlayCall::Show(
                ScreenRect {
                    x: 10.0,
                    y: 20.0,
                    w: 1.0,
                    h: 14.0,
                },
                "hello".into()
            )]
        );
    }

    #[test]
    fn fresh_engine_shows_nothing_and_requests_nothing_before_focus() {
        // Cold start: Engine::new defaults to unsupported caps, so text + tick
        // before any on_focus must neither surface a request nor paint a ghost.
        let (mut engine, _adapter, overlay) = engine();
        engine.on_text_changed(typed("hello ", 6, 1000)).unwrap();
        assert!(
            engine.on_tick(2000).unwrap().is_empty(),
            "no request may be surfaced before a field is focused"
        );
        assert!(
            overlay.calls.lock().unwrap().is_empty(),
            "no ghost may be painted before a field is focused"
        );
    }

    #[test]
    fn with_trailing_space_builder_reaches_the_machine() {
        // Mirror of runtime_trailing_space_setter_reaches_the_machine, but via the
        // builder: a single-word Full accept inserts the word followed by a space.
        let adapter = FakeAdapter::new();
        let mut engine = Engine::new(adapter.clone(), FakeOverlay::default(), 200, 4, 32)
            .with_trailing_space(true);
        engine.on_focus(field()).unwrap();
        engine.on_text_changed(typed("x", 1, 0)).unwrap();
        let requests = engine.on_tick(500).unwrap();
        engine.on_completion(&requests[0], "solo".into()).unwrap();

        engine.on_accept(AcceptAction::Full).unwrap();

        assert_eq!(
            *adapter.inserts.lock().unwrap(),
            vec![(field(), "solo ".into(), InsertStrategy::AxSet)],
            "the builder's trailing-space flag reaches the machine: the word is inserted with a trailing space"
        );
    }

    #[test]
    fn multibyte_completion_inserts_full_text_through_accept() {
        // A multi-byte completion must be inserted verbatim through the non-replacing
        // accept path — guards char/byte accounting in the insert plumbing.
        let (mut engine, adapter, _overlay) = showing("café ☕");
        engine.on_accept(AcceptAction::Full).unwrap();
        assert_eq!(
            *adapter.inserts.lock().unwrap(),
            vec![(field(), "café ☕".into(), InsertStrategy::AxSet)],
            "the full multi-byte suggestion is inserted verbatim"
        );
    }

    #[test]
    fn preview_accept_insert_with_nothing_showing_is_none() {
        // No suggestion is showing: preview must not fabricate an insert.
        let (engine, _adapter, _overlay) = engine();
        assert_eq!(engine.preview_accept_insert(AcceptAction::Full), None);
        assert_eq!(engine.preview_accept_insert(AcceptAction::Word), None);
    }

    #[test]
    fn preview_accept_correction_exposes_suggestion_and_range_while_showing() {
        // Sibling to the preview_accept_insert previews (which had two tests);
        // this public forwarder had no engine-layer coverage. With a correction
        // ghost up, the engine must expose (field, suggestion, range) for host
        // reconciliation. A plain completion ghost (a different presentation) and
        // a cold engine must both yield None — pinning that the correction
        // discriminator is honoured, not mis-forwarded to the completion preview.
        let (mut correction_engine, _adapter, _overlay) = engine();
        correction_engine.on_focus(field()).unwrap();
        let range = CorrectionRange { start: 0, end: 3 };
        let request = grammar_request(&mut correction_engine, range);
        correction_engine
            .on_correction(&request, "the".into(), range)
            .unwrap();

        assert_eq!(
            correction_engine.preview_accept_correction(),
            Some((field(), "the".into(), range)),
            "a showing correction previews its suggestion and range for the host"
        );

        // A plain completion ghost is NOT an accept-able correction.
        let (completion_engine, _a, _o) = showing("hi there");
        assert_eq!(
            completion_engine.preview_accept_correction(),
            None,
            "a completion ghost must not preview as a correction"
        );

        // Nothing showing at all → None.
        let (cold_engine, _a, _o) = engine();
        assert_eq!(cold_engine.preview_accept_correction(), None);
    }
}
