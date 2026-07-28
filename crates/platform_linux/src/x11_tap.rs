//! Live X11 accept tap (ROADMAP Phase 2.3) — Linux only.
//!
//! **The mechanism, resolved by measurement** (`tools/acceptance/linux-keytap-spike.c`):
//! a *passive* `XGrabKey` on the accept keys with the keyboard in
//! `GrabModeSync`, resolving each keystroke with `XAllowEvents` —
//! `AsyncKeyboard` consumes it (the accept path), `ReplayKeyboard` delivers it to
//! the focused application as if no grab existed (Tab means Tab). That pair
//! reproduces the macOS `CGEventTap` semantics with no synthetic re-send and no
//! window where a keystroke is dropped or duplicated. The grab exists **only
//! while an accept action is armed**.
//!
//! **Why `x11rb` and not Xlib.** Same reasoning as `atspi_live`'s D-Bus choice:
//! linking a C library would make the compme binary refuse to *start* on a host
//! without it, a hard failure where this project requires fail-closed
//! degradation. `x11rb` is pure Rust, so a host with no X server merely has no
//! tap. It expresses passive grabs (`grab_key`) and `XAllowEvents`
//! (`allow_events`) directly, so nothing was given up.
//!
//! ## The keyboard-freeze hazard, and how every path is covered
//!
//! A `GrabModeSync` grab freezes keyboard processing **system-wide** from the
//! moment the grab activates until `XAllowEvents`. If compme stalls in between,
//! the user's keyboard stops responding in every application. Coverage:
//!
//! 1. **Normal path.** The event thread resolves *before* it does anything else:
//!    the decision is a pure function over local state, `allow_events` +
//!    `flush` go out immediately, and only then is the control handed to the
//!    dispatcher. Nothing between the event and the resolve can block.
//! 2. **Engine/user code.** The `AcceptCallback` runs on a **separate dispatcher
//!    thread**, fed by a channel (the macOS adapter's `callback_tx` shape). A
//!    callback that blocks or panics therefore cannot delay a resolve.
//! 3. **Panic.** Each callback invocation is wrapped in `catch_unwind`, and the
//!    event thread's own body is too, so an unwind cannot skip the final thaw.
//! 4. **Watchdog.** A third thread ticks every [`WATCHDOG_TICK`] using only
//!    atomics, and past [`crate::x11_keys::FREEZE_BUDGET_MS`] it thaws with
//!    `ReplayKeyboard` (fail *open* — the user's keystroke outranks the accept)
//!    and drops the grab. It also enforces a hard cap on how long the grab may
//!    stay armed and the engine's scheduled-hide failsafe.
//! 5. **Teardown / `Drop`.** Drop thaws and ungrabs **before** joining any
//!    thread, so a slow thread exit can never hold the keyboard.
//! 6. **Process death.** The X server releases a client's grabs and thaws the
//!    keyboard when its connection closes, so `abort`, `SIGKILL` and a panicking
//!    process are covered by the protocol itself.
//!
//! The one residual: if the event thread were wedged *inside* an X request while
//! holding `x11rb`'s internal connection lock, the watchdog's own request would
//! queue behind it. Only closing the connection escapes that, which is what
//! process death does.

use crate::x11_keys::{
    arm_transition, key_decision, keycode_for_keysym, watchdog_action, AcceptBindings,
    GrabTransition, KeyDecision, WatchdogAction, UNSET_MS,
};
use platform::{AcceptAction, AcceptCallback, KeyInterceptMode, PlatformError, TapControl};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;
use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::xproto::{
    Allow, ClientMessageEvent, ConnectionExt, CreateWindowAux, EventMask, GrabMode, ModMask,
    Window, WindowClass,
};
use x11rb::protocol::{ErrorKind, Event};
use x11rb::rust_connection::RustConnection;
use x11rb::CURRENT_TIME;

/// How often the watchdog re-checks its deadlines. Short enough that a frozen
/// keyboard is released well inside a keystroke's perceptible latency, long
/// enough to be free at idle.
const WATCHDOG_TICK: Duration = Duration::from_millis(25);

/// The atom name for the self-wake message that ends the event thread's blocking
/// `wait_for_event`. Namespaced so it cannot collide with another client's atom.
const WAKE_ATOM_NAME: &[u8] = b"_COMPME_ACCEPT_TAP_WAKE";

fn cannot_complete(what: &str, err: impl std::fmt::Display) -> PlatformError {
    PlatformError::CannotComplete {
        reason: format!("platform_linux x11 tap {what}: {err}"),
    }
}

/// One grabbed key: the keysym a binding names and the keycode this layout puts
/// it on.
#[derive(Clone, Copy, Debug)]
struct GrabbedKey {
    keysym: u32,
    keycode: u8,
}

/// State shared by the event thread, the watchdog and the engine-side control
/// calls. Everything the watchdog needs is an atomic, so it can never be blocked
/// by a lock the event thread holds.
struct TapState {
    /// Monotonic epoch for every `*_ms` field below.
    epoch: std::time::Instant,
    bindings: AcceptBindings,
    root: Window,
    keys: Vec<GrabbedKey>,
    /// The armed accept action. `None` means disarmed, and the grab's existence
    /// tracks it exactly (see [`arm_transition`]).
    action: Mutex<Option<AcceptAction>>,
    /// Whether the passive grab is currently installed.
    grabbed: Mutex<bool>,
    /// When the event thread dequeued the keystroke that froze the keyboard, or
    /// [`UNSET_MS`].
    frozen_since_ms: AtomicU64,
    armed_since_ms: AtomicU64,
    /// The engine's scheduled-hide failsafe deadline, or [`UNSET_MS`].
    ///
    /// This is the macOS `teardown_generation` guard collapsed to a single slot:
    /// there, each delayed hide is a detached sleeper thread and a generation
    /// counter tells a superseded one to no-op. Here the deadline lives in one
    /// place, so *clearing* it on every visibility transition invalidates a
    /// pending hide directly — same invariant, no counter to keep in sync.
    hide_deadline_ms: AtomicU64,
    /// False once the subscription is dropped: control calls become no-ops
    /// rather than errors, matching the macOS controller.
    active: AtomicBool,
    /// Set by teardown; both worker threads exit at their next check.
    stopping: AtomicBool,
}

impl TapState {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn armed_action(&self) -> Option<AcceptAction> {
        // Poison recovery rather than an error: this is read on the resolve path,
        // where refusing to decide would leave the keyboard frozen.
        *self.action.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn keysym_for_keycode(&self, keycode: u8) -> Option<u32> {
        self.keys
            .iter()
            .find(|key| key.keycode == keycode)
            .map(|key| key.keysym)
    }
}

/// The installed accept tap. Dropping it thaws the keyboard, releases every
/// grab, and stops its threads.
pub struct X11AcceptTap {
    conn: Arc<RustConnection>,
    state: Arc<TapState>,
    /// Our own unmapped `InputOnly` window. `SendEvent` to a window with an empty
    /// event mask goes to the window's creating client, which is how teardown
    /// wakes the event thread out of `wait_for_event` without polling.
    wake_window: Window,
    wake_atom: u32,
    /// Dropped by teardown so the dispatcher's `recv` ends.
    dispatch_tx: Option<mpsc::Sender<TapControl>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl std::fmt::Debug for X11AcceptTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11AcceptTap")
            .field("keys", &self.state.keys.len())
            .finish_non_exhaustive()
    }
}

/// Whether this host can actually install the tap, checked the only way that is
/// evidence rather than inference: connect, resolve the keycodes for the bound
/// keysyms, and trial-grab every one of them.
///
/// Reports [`KeyInterceptMode::None`] on any failure — no display, a layout
/// missing a bound key, or `BadAccess` because a window manager or IME already
/// holds the key. Deliberately **not** `HotkeyOnly`: that variant demotes the UX
/// to an always-on hotkey, and this adapter registers no global shortcuts yet, so
/// claiming it would promise a path compme cannot deliver on Linux. (The plan
/// says "degrade to `UxMode::Hotkey`"; that becomes correct once the shortcut
/// registration lands.)
pub fn probe_accept_intercept() -> KeyInterceptMode {
    match trial_grab() {
        Ok(()) => KeyInterceptMode::XGrabKey,
        Err(_) => KeyInterceptMode::None,
    }
}

fn trial_grab() -> Result<(), PlatformError> {
    let (conn, root, keys) = open_and_resolve(&AcceptBindings::defaults())?;
    // The trial grab is what catches a window manager already holding Tab. It
    // exists for microseconds; a keystroke inside that window would activate a
    // grab nobody resolves, so the ungrab is unconditional and the connection is
    // closed immediately after, which thaws the keyboard even if it did.
    let result = grab_keys(&conn, root, &keys);
    ungrab_keys(&conn, root, &keys);
    result
}

/// Connect to the X server and resolve every bound keysym to a keycode on the
/// current layout.
fn open_and_resolve(
    bindings: &AcceptBindings,
) -> Result<(RustConnection, Window, Vec<GrabbedKey>), PlatformError> {
    if bindings.is_empty() {
        return Err(PlatformError::UnsupportedField {
            reason: "platform_linux x11 tap: no accept chord translates to an X11 keysym".into(),
        });
    }
    let (conn, screen_num) = x11rb::connect(None).map_err(|err| cannot_complete("connect", err))?;
    let setup = conn.setup();
    let root = setup
        .roots
        .get(screen_num)
        .ok_or_else(|| cannot_complete("screen", "the display reported no such screen"))?
        .root;
    let min_keycode = setup.min_keycode;
    let count = setup
        .max_keycode
        .saturating_sub(min_keycode)
        .saturating_add(1);
    let mapping = conn
        .get_keyboard_mapping(min_keycode, count)
        .map_err(|err| cannot_complete("get_keyboard_mapping", err))?
        .reply()
        .map_err(|err| cannot_complete("keyboard mapping reply", err))?;

    let mut keys: Vec<GrabbedKey> = Vec::new();
    for keysym in bindings.distinct_keysyms() {
        let Some(keycode) = keycode_for_keysym(
            min_keycode,
            mapping.keysyms_per_keycode,
            &mapping.keysyms,
            keysym,
        ) else {
            // A layout without this key means that chord is simply not
            // intercepted — the application keeps receiving the key.
            continue;
        };
        // Two keysyms can land on one keycode; grabbing a keycode twice is an
        // Access error against ourselves, so keep the first.
        if keys.iter().all(|key| key.keycode != keycode) {
            keys.push(GrabbedKey { keysym, keycode });
        }
    }
    if keys.is_empty() {
        return Err(PlatformError::UnsupportedField {
            reason: "platform_linux x11 tap: this layout carries none of the accept keys".into(),
        });
    }
    Ok((conn, root, keys))
}

/// Install the passive grabs. `owner_events = false` keeps the event on our grab
/// window rather than letting it reach the focused window first;
/// `GrabMode::SYNC` on the keyboard is what makes the per-keystroke
/// consume/pass-through decision possible at all.
///
/// `ModMask::ANY` grabs the key under every modifier combination — one grab
/// instead of one per lock-state permutation — and the *decision* filters on the
/// event's actual modifiers, so `Ctrl+Tab` is replayed untouched.
///
/// Any failure ungrabs what was already taken, so a partial grab never survives.
fn grab_keys(
    conn: &RustConnection,
    root: Window,
    keys: &[GrabbedKey],
) -> Result<(), PlatformError> {
    for (index, key) in keys.iter().enumerate() {
        let outcome = conn
            .grab_key(
                false,
                root,
                ModMask::ANY,
                key.keycode,
                GrabMode::ASYNC,
                GrabMode::SYNC,
            )
            .map_err(|err| cannot_complete("grab_key", err))
            .and_then(|cookie| cookie.check().map_err(|err| grab_error(key.keysym, err)));
        if let Err(err) = outcome {
            ungrab_keys(conn, root, &keys[..index]);
            return Err(err);
        }
    }
    Ok(())
}

/// Classify a failed grab. `BadAccess` means another client (window manager, IME)
/// already holds that key+modifier combination, which is a supported
/// configuration to degrade from — not a session failure.
fn grab_error(keysym: u32, err: ReplyError) -> PlatformError {
    if let ReplyError::X11Error(ref x11) = err {
        if x11.error_kind == ErrorKind::Access {
            return PlatformError::UnsupportedField {
                reason: format!(
                    "platform_linux x11 tap: keysym {keysym:#x} is already grabbed by another client (BadAccess)"
                ),
            };
        }
    }
    cannot_complete("grab_key", err)
}

/// Release every grab, best effort: a per-key failure must not stop the rest,
/// because leaving one key grabbed is exactly the harm this function prevents.
fn ungrab_keys(conn: &RustConnection, root: Window, keys: &[GrabbedKey]) {
    for key in keys {
        if let Ok(cookie) = conn.ungrab_key(key.keycode, root, ModMask::ANY) {
            cookie.ignore_error();
        }
    }
    let _ = conn.flush();
}

/// Unfreeze the keyboard, fail-open: `ReplayKeyboard` hands any frozen keystroke
/// to the focused application. Harmless when nothing is frozen (`XAllowEvents`
/// on an unfrozen device has no effect), which is what lets the watchdog call it
/// speculatively.
fn thaw(conn: &RustConnection) {
    if let Ok(cookie) = conn.allow_events(Allow::REPLAY_KEYBOARD, CURRENT_TIME) {
        cookie.ignore_error();
    }
    let _ = conn.flush();
}

/// Apply the armed action and bring the grab into line with it. The two are one
/// operation on purpose: "the grab exists exactly while an action is armed" is
/// the contract's key-eating guard.
fn set_action(
    conn: &RustConnection,
    state: &TapState,
    action: Option<AcceptAction>,
) -> Result<(), PlatformError> {
    // Scoped so the action lock is never held across an X round trip — the
    // resolve path reads it per keystroke.
    {
        *state.action.lock().unwrap_or_else(PoisonError::into_inner) = action;
    }
    let mut grabbed = state.grabbed.lock().unwrap_or_else(PoisonError::into_inner);
    match arm_transition(action, *grabbed) {
        GrabTransition::Grab => match grab_keys(conn, state.root, &state.keys) {
            Ok(()) => {
                *grabbed = true;
                state
                    .armed_since_ms
                    .store(state.now_ms(), Ordering::Release);
                Ok(())
            }
            Err(err) => {
                // Degrade, do not half-arm: the action goes back to None so the
                // invariant holds and nothing believes keys are being watched.
                *state.action.lock().unwrap_or_else(PoisonError::into_inner) = None;
                Err(err)
            }
        },
        GrabTransition::Ungrab => {
            ungrab_keys(conn, state.root, &state.keys);
            *grabbed = false;
            state.armed_since_ms.store(UNSET_MS, Ordering::Release);
            state.hide_deadline_ms.store(UNSET_MS, Ordering::Release);
            Ok(())
        }
        GrabTransition::Unchanged => Ok(()),
    }
}

impl X11AcceptTap {
    /// Install the tap: connect, resolve keycodes, and start the event,
    /// dispatcher and watchdog threads. The grab itself is **not** taken here —
    /// it is taken when a suggestion becomes visible.
    pub fn install(callback: AcceptCallback) -> Result<Arc<Self>, PlatformError> {
        let bindings = AcceptBindings::defaults();
        let (conn, root, keys) = open_and_resolve(&bindings)?;
        let conn = Arc::new(conn);
        let (wake_window, wake_atom) = create_wake_channel(&conn)?;

        let state = Arc::new(TapState {
            epoch: std::time::Instant::now(),
            bindings,
            root,
            keys,
            action: Mutex::new(None),
            grabbed: Mutex::new(false),
            frozen_since_ms: AtomicU64::new(UNSET_MS),
            armed_since_ms: AtomicU64::new(UNSET_MS),
            hide_deadline_ms: AtomicU64::new(UNSET_MS),
            active: AtomicBool::new(true),
            stopping: AtomicBool::new(false),
        });

        let (dispatch_tx, dispatch_rx) = mpsc::channel::<TapControl>();
        // Join order at teardown is this order. The event thread owns the other
        // sender clone, so it must be joined before the dispatcher can see its
        // channel close.
        let threads = vec![
            spawn_event_thread(Arc::clone(&conn), Arc::clone(&state), dispatch_tx.clone())?,
            spawn_dispatcher(callback, dispatch_rx)?,
            spawn_watchdog(Arc::clone(&conn), Arc::clone(&state))?,
        ];

        Ok(Arc::new(Self {
            conn,
            state,
            wake_window,
            wake_atom,
            dispatch_tx: Some(dispatch_tx),
            threads: Mutex::new(threads),
        }))
    }

    /// Arm or disarm for a visible suggestion. Preserves an action set by a
    /// preceding [`Self::set_accept_action`] (the engine sets the action, then
    /// reports visibility), defaulting to a full accept — the macOS controller's
    /// `get_or_insert(Full)`.
    pub fn set_suggestion_visible(&self, visible: bool) -> Result<(), PlatformError> {
        if !self.state.active.load(Ordering::Acquire) {
            return Ok(());
        }
        // Any visibility transition invalidates a pending scheduled hide.
        self.state
            .hide_deadline_ms
            .store(UNSET_MS, Ordering::Release);
        let action = if visible {
            // Every "still visible" statement restarts the armed-time cap, so the
            // 30-second failsafe measures silence from the engine rather than the
            // age of the first arm.
            self.state
                .armed_since_ms
                .store(self.state.now_ms(), Ordering::Release);
            Some(self.state.armed_action().unwrap_or(AcceptAction::Full))
        } else {
            None
        };
        set_action(&self.conn, &self.state, action)
    }

    pub fn set_accept_action(&self, action: Option<AcceptAction>) -> Result<(), PlatformError> {
        if !self.state.active.load(Ordering::Acquire) {
            return Ok(());
        }
        set_action(&self.conn, &self.state, action)
    }

    /// Schedule the tap to treat the suggestion as hidden after `delay` — the
    /// engine's failsafe against a missed hide. The watchdog owns the deadline,
    /// so this spawns nothing.
    pub fn hide_suggestion_after(&self, delay: Duration) -> Result<(), PlatformError> {
        if !self.state.active.load(Ordering::Acquire) {
            return Ok(());
        }
        if delay.is_zero() {
            return set_action(&self.conn, &self.state, None);
        }
        let deadline = self
            .state
            .now_ms()
            .saturating_add(u64::try_from(delay.as_millis()).unwrap_or(u64::MAX));
        self.state
            .hide_deadline_ms
            .store(deadline, Ordering::Release);
        Ok(())
    }
}

impl Drop for X11AcceptTap {
    fn drop(&mut self) {
        self.state.active.store(false, Ordering::Release);
        self.state.stopping.store(true, Ordering::Release);
        // ORDER IS LOAD-BEARING: thaw and ungrab BEFORE joining anything, so a
        // slow thread exit can never leave the keyboard frozen or a key grabbed.
        thaw(&self.conn);
        ungrab_keys(&self.conn, self.state.root, &self.state.keys);
        *self
            .state
            .grabbed
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = false;
        // End the dispatcher (drop its sender) and wake the event thread out of
        // its blocking wait_for_event.
        self.dispatch_tx = None;
        wake_event_thread(&self.conn, self.wake_window, self.wake_atom);
        let threads =
            std::mem::take(&mut *self.threads.lock().unwrap_or_else(PoisonError::into_inner));
        for thread in threads {
            let _ = thread.join();
        }
        if let Ok(cookie) = self.conn.destroy_window(self.wake_window) {
            cookie.ignore_error();
        }
        let _ = self.conn.flush();
    }
}

/// A 1x1 unmapped `InputOnly` window plus the atom used to address it. It is
/// never mapped, so it takes no input focus and is invisible to the user; its
/// only job is to be a `SendEvent` destination we own.
fn create_wake_channel(conn: &RustConnection) -> Result<(Window, u32), PlatformError> {
    let root = conn
        .setup()
        .roots
        .first()
        .ok_or_else(|| cannot_complete("wake window", "the display reported no screen"))?
        .root;
    let window = conn
        .generate_id()
        .map_err(|err| cannot_complete("generate_id", err))?;
    conn.create_window(
        0,
        window,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::new(),
    )
    .map_err(|err| cannot_complete("create_window", err))?
    .check()
    .map_err(|err| cannot_complete("create_window", err))?;
    let atom = conn
        .intern_atom(false, WAKE_ATOM_NAME)
        .map_err(|err| cannot_complete("intern_atom", err))?
        .reply()
        .map_err(|err| cannot_complete("intern_atom reply", err))?
        .atom;
    Ok((window, atom))
}

fn wake_event_thread(conn: &RustConnection, window: Window, atom: u32) {
    let event = ClientMessageEvent::new(32, window, atom, [0u32; 5]);
    // An empty event mask delivers to the window's creating client — us.
    if let Ok(cookie) = conn.send_event(false, window, EventMask::NO_EVENT, event) {
        cookie.ignore_error();
    }
    let _ = conn.flush();
}

fn spawn_dispatcher(
    callback: AcceptCallback,
    rx: mpsc::Receiver<TapControl>,
) -> Result<JoinHandle<()>, PlatformError> {
    std::thread::Builder::new()
        .name("compme-keytap-dispatch".into())
        .spawn(move || {
            while let Ok(control) = rx.recv() {
                // The engine's callback is foreign code on the far side of the
                // FFI-shaped boundary: an unwind here must not poison the tap or
                // abort the process, and it must never be able to reach the
                // resolve path — which is why it runs on this thread and not the
                // event thread.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    callback(control);
                }));
            }
        })
        .map_err(|err| cannot_complete("dispatcher thread", err))
}

fn spawn_event_thread(
    conn: Arc<RustConnection>,
    state: Arc<TapState>,
    dispatch: mpsc::Sender<TapControl>,
) -> Result<JoinHandle<()>, PlatformError> {
    std::thread::Builder::new()
        .name("compme-keytap".into())
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_event_loop(&conn, &state, &dispatch);
            }));
            // Whatever ended the loop — a normal stop, a connection error, or a
            // panic — leave the keyboard usable and the grabs released.
            thaw(&conn);
            ungrab_keys(&conn, state.root, &state.keys);
            state.frozen_since_ms.store(UNSET_MS, Ordering::Release);
            if outcome.is_err() {
                *state.grabbed.lock().unwrap_or_else(PoisonError::into_inner) = false;
            }
        })
        .map_err(|err| cannot_complete("event thread", err))
}

fn run_event_loop(conn: &RustConnection, state: &TapState, dispatch: &mpsc::Sender<TapControl>) {
    while !state.stopping.load(Ordering::Acquire) {
        let Ok(event) = conn.wait_for_event() else {
            return;
        };
        match event {
            Event::KeyPress(press) => {
                resolve_key_press(conn, state, dispatch, press.detail, u16::from(press.state));
            }
            // The grab stays active until the key is physically released; the
            // release arrives here too and must not be left frozen.
            Event::KeyRelease(_) => {
                if let Ok(cookie) = conn.allow_events(Allow::ASYNC_KEYBOARD, CURRENT_TIME) {
                    cookie.ignore_error();
                }
                let _ = conn.flush();
            }
            // The teardown wake, or anything else: nothing to resolve.
            _ => {}
        }
    }
}

/// Resolve exactly one grabbed keystroke.
///
/// **The keyboard is frozen for every application while this runs.** Everything
/// before `allow_events` is a pure decision over already-loaded state; the
/// callback is handed to another thread afterwards, never called from here.
fn resolve_key_press(
    conn: &RustConnection,
    state: &TapState,
    dispatch: &mpsc::Sender<TapControl>,
    keycode: u8,
    modifiers: u16,
) {
    state
        .frozen_since_ms
        .store(state.now_ms(), Ordering::Release);
    let decision = match state.keysym_for_keycode(keycode) {
        Some(keysym) => key_decision(&state.bindings, keysym, modifiers, state.armed_action()),
        // A key we did not grab cannot reach us, but if one does it belongs to
        // the application.
        None => KeyDecision::PassThrough,
    };
    let allow = match decision {
        KeyDecision::Consume(_) => Allow::ASYNC_KEYBOARD,
        KeyDecision::PassThrough => Allow::REPLAY_KEYBOARD,
    };
    if let Ok(cookie) = conn.allow_events(allow, CURRENT_TIME) {
        cookie.ignore_error();
    }
    let _ = conn.flush();
    state.frozen_since_ms.store(UNSET_MS, Ordering::Release);
    if let KeyDecision::Consume(control) = decision {
        // A dead dispatcher (teardown raced us) means the control is dropped —
        // the key was already swallowed, so the worst case is one lost accept.
        let _ = dispatch.send(control);
    }
}

fn spawn_watchdog(
    conn: Arc<RustConnection>,
    state: Arc<TapState>,
) -> Result<JoinHandle<()>, PlatformError> {
    std::thread::Builder::new()
        .name("compme-keytap-watchdog".into())
        .spawn(move || {
            while !state.stopping.load(Ordering::Acquire) {
                std::thread::sleep(WATCHDOG_TICK);
                match watchdog_action(
                    state.now_ms(),
                    state.frozen_since_ms.load(Ordering::Acquire),
                    state.armed_since_ms.load(Ordering::Acquire),
                    state.hide_deadline_ms.load(Ordering::Acquire),
                ) {
                    WatchdogAction::Nothing => {}
                    WatchdogAction::ThawAndDisarm => {
                        // Lock-free first: whatever else is stuck, the user's
                        // keyboard comes back within FREEZE_BUDGET_MS.
                        thaw(&conn);
                        state.frozen_since_ms.store(UNSET_MS, Ordering::Release);
                        let _ = set_action(&conn, &state, None);
                    }
                    WatchdogAction::Disarm => {
                        let _ = set_action(&conn, &state, None);
                    }
                }
            }
        })
        .map_err(|err| cannot_complete("watchdog thread", err))
}
