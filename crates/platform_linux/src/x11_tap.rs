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
//! 1. **Normal path.** The event thread resolves *before* it dispatches any
//!    callback: it takes a bounded snapshot of the current grab plan and
//!    action, sends `allow_events` + `flush` immediately, and only then hands
//!    control to the dispatcher.
//! 2. **Engine/user code.** The `AcceptCallback` runs on a **separate dispatcher
//!    thread**, fed by a channel (the macOS adapter's `callback_tx` shape). A
//!    callback that blocks or panics therefore cannot delay a resolve.
//! 3. **Panic.** Each callback invocation is wrapped in `catch_unwind`, and the
//!    event thread's own body is too, so an unwind cannot skip the final thaw.
//! 4. **Watchdog.** A third thread reads its deadlines from atomics every
//!    `WATCHDOG_TICK`, and past [`crate::x11_keys::FREEZE_BUDGET_MS`] it thaws with
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
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;
use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::xproto::{
    Allow, ClientMessageEvent, ConnectionExt, CreateWindowAux, EventMask, GrabMode, Mapping,
    ModMask, Window, WindowClass,
};
use x11rb::protocol::{ErrorKind, Event};
use x11rb::rust_connection::RustConnection;
use x11rb::CURRENT_TIME;

/// How often the watchdog re-checks its deadlines. Short enough that a frozen
/// keyboard is released well inside a keystroke's perceptible latency, long
/// enough to be free at idle.
const WATCHDOG_TICK: Duration = Duration::from_millis(25);
/// How long teardown waits for the three workers before detaching them. Long
/// enough for a watchdog tick plus a wake round trip, short enough that a wedged
/// X server cannot hang the run loop.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// The atom name for the self-wake message that ends the event thread's blocking
/// `wait_for_event`. Namespaced so it cannot collide with another client's atom.
const WAKE_ATOM_NAME: &[u8] = b"_COMPME_ACCEPT_TAP_WAKE";

fn cannot_complete(what: &str, err: impl std::fmt::Display) -> PlatformError {
    PlatformError::CannotComplete {
        reason: format!("platform_linux x11 tap {what}: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_mask_expansion_covers_caps_and_discovered_numlock_without_duplicates() {
        assert_eq!(
            expanded_lock_masks(0, 1 << 5),
            vec![0, 1 << 1, 1 << 5, (1 << 1) | (1 << 5)]
        );
        assert_eq!(
            expanded_lock_masks(1 << 2, 0),
            vec![1 << 2, (1 << 2) | (1 << 1)]
        );
        assert_eq!(expanded_lock_masks(1 << 1, 1 << 1), vec![1 << 1]);
    }

    #[test]
    #[ignore = "needs an X session: run-linux-atspi-session.sh --run-in-session"]
    fn partial_worker_spawn_failure_joins_every_started_worker() {
        for fail_at in [2, 3] {
            let live_workers = Arc::new(AtomicUsize::new(0));
            let result = X11AcceptTap::install_with_spawner(
                Arc::new(|_| {}),
                WorkerSpawner::fail_at(fail_at, Arc::clone(&live_workers)),
            );

            assert!(result.is_err(), "worker spawn {fail_at} must fail");
            assert_eq!(
                live_workers.load(Ordering::SeqCst),
                0,
                "spawn failure {fail_at} returned with a worker still alive"
            );
        }
    }
}

#[derive(Default)]
struct WorkerSpawner {
    #[cfg(test)]
    test_control: Option<SpawnTestControl>,
}

#[cfg(test)]
struct SpawnTestControl {
    next_spawn: usize,
    fail_at: usize,
    live_workers: Arc<AtomicUsize>,
}

#[cfg(test)]
struct WorkerLifetime(Arc<AtomicUsize>);

#[cfg(test)]
impl WorkerLifetime {
    fn new(live_workers: Arc<AtomicUsize>) -> Self {
        live_workers.fetch_add(1, Ordering::SeqCst);
        Self(live_workers)
    }
}

#[cfg(test)]
impl Drop for WorkerLifetime {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl WorkerSpawner {
    #[cfg(test)]
    fn fail_at(fail_at: usize, live_workers: Arc<AtomicUsize>) -> Self {
        Self {
            test_control: Some(SpawnTestControl {
                next_spawn: 0,
                fail_at,
                live_workers,
            }),
        }
    }

    fn spawn<F>(
        &mut self,
        name: &str,
        what: &str,
        worker: F,
    ) -> Result<JoinHandle<()>, PlatformError>
    where
        F: FnOnce() + Send + 'static,
    {
        #[cfg(test)]
        if let Some(control) = self.test_control.as_mut() {
            control.next_spawn += 1;
            if control.next_spawn == control.fail_at {
                return Err(cannot_complete(
                    what,
                    std::io::Error::other("injected worker-spawn failure"),
                ));
            }
        }

        #[cfg(test)]
        let lifetime = self
            .test_control
            .as_ref()
            .map(|control| WorkerLifetime::new(Arc::clone(&control.live_workers)));

        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                #[cfg(test)]
                let _lifetime = lifetime;
                worker();
            })
            .map_err(|err| cannot_complete(what, err))
    }
}

/// One grabbed key: the keysym a binding names and the keycode this layout puts
/// it on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GrabbedKey {
    keysym: u32,
    keycode: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PlannedGrab {
    keysym: u32,
    keycode: u8,
    modifiers: u16,
}

#[derive(Clone, Debug)]
struct GrabPlan {
    bindings: AcceptBindings,
    keys: Vec<GrabbedKey>,
    grabs: Vec<PlannedGrab>,
}

impl GrabPlan {
    fn decision_for_keycode(
        &self,
        keycode: u8,
        modifiers: u16,
        action: Option<AcceptAction>,
    ) -> KeyDecision {
        self.keys
            .iter()
            .filter(|key| key.keycode == keycode)
            .map(|key| key_decision(&self.bindings, key.keysym, modifiers, action))
            .find(|decision| matches!(decision, KeyDecision::Consume(_)))
            .unwrap_or(KeyDecision::PassThrough)
    }
}

/// State shared by the event thread, the watchdog and the engine-side control
/// calls. Deadline decisions use atomics; applying a disarm uses the same short
/// state locks and X request path as an engine-side hide.
struct TapState {
    /// Monotonic epoch for every `*_ms` field below.
    epoch: std::time::Instant,
    root: Window,
    /// One synchronized snapshot of bindings, resolved keycodes, and exact
    /// passive grabs. Mapping changes and live rebinds swap this transactionally.
    plan: Mutex<GrabPlan>,
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
    /// Set by teardown; all worker threads exit at their next check.
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
    /// Disconnects once every worker has exited: each holds a sender clone and
    /// nothing ever sends, so `Disconnected` means "all three are gone". Lets
    /// teardown bound its wait instead of joining a thread that may never wake.
    ///
    /// `Mutex` only for `Sync`: `mpsc::Receiver` is `Send` but not `Sync`, and
    /// this type is handed out as `Arc<X11AcceptTap>`, which needs both. Same
    /// reason `threads` above is wrapped; there is no contention, since only
    /// `Drop` ever touches it.
    stopped: Mutex<mpsc::Receiver<()>>,
}

impl std::fmt::Debug for X11AcceptTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11AcceptTap")
            .field(
                "keys",
                &self
                    .state
                    .plan
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .keys
                    .len(),
            )
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
    let (conn, root, plan) = open_and_build(&crate::x11_keys::configured_bindings())?;
    // The trial grab is what catches a window manager already holding Tab. It
    // exists for microseconds; a keystroke inside that window would activate a
    // grab nobody resolves, so the ungrab is unconditional and the connection is
    // closed immediately after, which thaws the keyboard even if it did.
    let result = grab_plan(&conn, root, &plan);
    ungrab_plan(&conn, root, &plan);
    result
}

fn open_and_build(
    bindings: &AcceptBindings,
) -> Result<(RustConnection, Window, GrabPlan), PlatformError> {
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
    let plan = build_grab_plan(&conn, bindings)?;
    Ok((conn, root, plan))
}

/// Resolve configured chords, the current keyboard layout, and the server's
/// actual NumLock modifier into one exact passive-grab plan.
fn build_grab_plan(
    conn: &RustConnection,
    bindings: &AcceptBindings,
) -> Result<GrabPlan, PlatformError> {
    if bindings.is_empty() {
        return Err(PlatformError::UnsupportedField {
            reason: "platform_linux x11 tap: no accept chord translates to an X11 keysym".into(),
        });
    }
    let setup = conn.setup();
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
        keys.push(GrabbedKey { keysym, keycode });
    }
    if keys.is_empty() {
        return Err(PlatformError::UnsupportedField {
            reason: "platform_linux x11 tap: this layout carries none of the accept keys".into(),
        });
    }
    let modifiers = conn
        .get_modifier_mapping()
        .map_err(|err| cannot_complete("get_modifier_mapping", err))?
        .reply()
        .map_err(|err| cannot_complete("modifier mapping reply", err))?;
    let per = usize::from(mapping.keysyms_per_keycode);
    let modifier_width = usize::from(modifiers.keycodes_per_modifier());
    let numlock_modifier = if modifier_width == 0 {
        0
    } else {
        modifiers
            .keycodes
            .chunks(modifier_width)
            .enumerate()
            .find_map(|(index, modifier_keys)| {
                modifier_keys
                    .iter()
                    .copied()
                    .filter(|keycode| *keycode >= min_keycode)
                    .any(|keycode| {
                        let start = usize::from(keycode - min_keycode).saturating_mul(per);
                        mapping
                            .keysyms
                            .get(start..start.saturating_add(per))
                            .is_some_and(|keysyms| keysyms.contains(&0xff7f))
                    })
                    .then(|| 1u16 << index)
            })
            .unwrap_or(0)
    };
    let mut grabs = Vec::new();
    for binding in bindings.iter() {
        let Some(key) = keys.iter().find(|key| key.keysym == binding.keysym) else {
            continue;
        };
        for modifiers in expanded_lock_masks(binding.modifiers, numlock_modifier) {
            let grab = PlannedGrab {
                keysym: binding.keysym,
                keycode: key.keycode,
                modifiers,
            };
            if grabs.iter().all(|existing: &PlannedGrab| {
                (existing.keycode, existing.modifiers) != (grab.keycode, grab.modifiers)
            }) {
                grabs.push(grab);
            }
        }
    }
    Ok(GrabPlan {
        bindings: bindings.clone(),
        keys,
        grabs,
    })
}

fn expanded_lock_masks(base: u16, numlock_modifier: u16) -> Vec<u16> {
    let mut masks = Vec::with_capacity(4);
    for mask in [
        base,
        base | u16::from(ModMask::LOCK),
        base | numlock_modifier,
        base | u16::from(ModMask::LOCK) | numlock_modifier,
    ] {
        if !masks.contains(&mask) {
            masks.push(mask);
        }
    }
    masks
}

/// Install the passive grabs. `owner_events = false` keeps the event on our grab
/// window rather than letting it reach the focused window first;
/// `GrabMode::SYNC` on the keyboard is what makes the per-keystroke
/// consume/pass-through decision possible at all.
///
/// Each binding is grabbed only for its exact intent modifiers, expanded across
/// CapsLock and the modifier slot this server maps to NumLock. This avoids
/// colliding with unrelated desktop chords such as Alt+Tab while leaving lock
/// state irrelevant to accept matching.
///
/// Any failure ungrabs what was already taken, so a partial grab never survives.
fn grab_plan(conn: &RustConnection, root: Window, plan: &GrabPlan) -> Result<(), PlatformError> {
    for (index, grab) in plan.grabs.iter().enumerate() {
        let outcome = conn
            .grab_key(
                false,
                root,
                ModMask::from(grab.modifiers),
                grab.keycode,
                GrabMode::ASYNC,
                GrabMode::SYNC,
            )
            .map_err(|err| cannot_complete("grab_key", err))
            .and_then(|cookie| cookie.check().map_err(|err| grab_error(grab.keysym, err)));
        if let Err(err) = outcome {
            ungrab_grabs(conn, root, &plan.grabs[..index]);
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
fn ungrab_plan(conn: &RustConnection, root: Window, plan: &GrabPlan) {
    ungrab_grabs(conn, root, &plan.grabs);
}

fn ungrab_grabs(conn: &RustConnection, root: Window, grabs: &[PlannedGrab]) {
    for grab in grabs {
        if let Ok(cookie) = conn.ungrab_key(grab.keycode, root, ModMask::from(grab.modifiers)) {
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

/// Clear all arm/watchdog state while the caller holds `grabbed`. Keeping this
/// transition in one place prevents an error path from releasing the X grab but
/// leaving an armed action or deadline behind.
fn clear_armed_state(state: &TapState, grabbed: &mut bool) {
    *grabbed = false;
    *state.action.lock().unwrap_or_else(PoisonError::into_inner) = None;
    state.armed_since_ms.store(UNSET_MS, Ordering::Release);
    state.hide_deadline_ms.store(UNSET_MS, Ordering::Release);
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
        GrabTransition::Grab => match grab_plan(
            conn,
            state.root,
            &state.plan.lock().unwrap_or_else(PoisonError::into_inner),
        ) {
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
                clear_armed_state(state, &mut grabbed);
                Err(err)
            }
        },
        GrabTransition::Ungrab => {
            ungrab_plan(
                conn,
                state.root,
                &state.plan.lock().unwrap_or_else(PoisonError::into_inner),
            );
            clear_armed_state(state, &mut grabbed);
            Ok(())
        }
        GrabTransition::Unchanged => Ok(()),
    }
}

/// Rebuild and transactionally publish the tap's sole plan. While armed, a
/// grab failure of the new plan restores the old exact grabs; a plan-build
/// failure (no valid chords under the new layout) releases them, disarms, and
/// publishes an empty plan — fail open — so neither the current arm nor any
/// later one can grab or match stale keycodes.
fn regrab(
    conn: &RustConnection,
    state: &TapState,
    bindings: AcceptBindings,
) -> Result<(), PlatformError> {
    let new_plan = match build_grab_plan(conn, &bindings) {
        Ok(plan) => plan,
        Err(err) => {
            // No valid plan exists for the new layout/bindings. Leaving the
            // old exact grabs armed would consume whatever keys now occupy
            // the stale keycodes — key-eating, the wrong failure polarity —
            // so release them, disarm, and publish an *empty* plan: the next
            // arm then grabs nothing and matches nothing (software accept
            // still works) until a later rebuild succeeds. Keeping the stale
            // plan instead would re-grab the stale keycodes on the very next
            // `set_action` arm.
            let mut grabbed = state.grabbed.lock().unwrap_or_else(PoisonError::into_inner);
            let mut current = state.plan.lock().unwrap_or_else(PoisonError::into_inner);
            if *grabbed {
                ungrab_plan(conn, state.root, &current);
            }
            clear_armed_state(state, &mut grabbed);
            *current = GrabPlan {
                bindings,
                keys: Vec::new(),
                grabs: Vec::new(),
            };
            return Err(err);
        }
    };
    let mut grabbed = state.grabbed.lock().unwrap_or_else(PoisonError::into_inner);
    let mut current = state.plan.lock().unwrap_or_else(PoisonError::into_inner);
    if !*grabbed {
        *current = new_plan;
        return Ok(());
    }

    ungrab_plan(conn, state.root, &current);
    match grab_plan(conn, state.root, &new_plan) {
        Ok(()) => {
            *current = new_plan;
            Ok(())
        }
        Err(err) => {
            if grab_plan(conn, state.root, &current).is_err() {
                clear_armed_state(state, &mut grabbed);
            }
            Err(err)
        }
    }
}

/// Owns workers created during `install` until all three spawns succeed. A
/// later spawn error therefore makes earlier workers inert, wakes them, and
/// gives them the existing bounded-join window. A worker still wedged after the
/// bound is detached only after inactive/stop/thaw/ungrab/sender-drop/wake.
struct SpawnGuard<'a> {
    conn: &'a Arc<RustConnection>,
    state: &'a Arc<TapState>,
    wake_window: Window,
    wake_atom: u32,
    dispatch_tx: &'a mut Option<mpsc::Sender<TapControl>>,
    stopped_tx: Option<mpsc::Sender<()>>,
    stopped: &'a mpsc::Receiver<()>,
    threads: Vec<JoinHandle<()>>,
    armed: bool,
}

impl Drop for SpawnGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.state.active.store(false, Ordering::Release);
        self.state.stopping.store(true, Ordering::Release);
        thaw(self.conn);
        ungrab_plan(
            self.conn,
            self.state.root,
            &self
                .state
                .plan
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        self.dispatch_tx.take();
        self.stopped_tx.take();
        wake_event_thread(self.conn, self.wake_window, self.wake_atom);
        let all_exited = matches!(
            self.stopped.recv_timeout(STOP_TIMEOUT),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
        if all_exited {
            for thread in self.threads.drain(..) {
                let _ = thread.join();
            }
        }
    }
}

impl X11AcceptTap {
    /// Install the tap: connect, resolve keycodes, and start the event,
    /// dispatcher and watchdog threads. The grab itself is **not** taken here —
    /// it is taken when a suggestion becomes visible.
    pub fn install(callback: AcceptCallback) -> Result<Arc<Self>, PlatformError> {
        Self::install_with_spawner(callback, WorkerSpawner::default())
    }

    fn install_with_spawner(
        callback: AcceptCallback,
        mut spawner: WorkerSpawner,
    ) -> Result<Arc<Self>, PlatformError> {
        // The chords the run loop's startup key-binding pass configured (G5:
        // persisted macOS chords translated to X11), or the defaults. Read at
        // install time. The returned subscription also rebuilds this same plan
        // transactionally for a later live rebind.
        let bindings = crate::x11_keys::configured_bindings();
        let (conn, root, plan) = open_and_build(&bindings)?;
        let conn = Arc::new(conn);
        let (wake_window, wake_atom) = create_wake_channel(&conn)?;

        let state = Arc::new(TapState {
            epoch: std::time::Instant::now(),
            root,
            plan: Mutex::new(plan),
            action: Mutex::new(None),
            grabbed: Mutex::new(false),
            frozen_since_ms: AtomicU64::new(UNSET_MS),
            armed_since_ms: AtomicU64::new(UNSET_MS),
            hide_deadline_ms: AtomicU64::new(UNSET_MS),
            active: AtomicBool::new(true),
            stopping: AtomicBool::new(false),
        });

        let (dispatch_sender, dispatch_rx) = mpsc::channel::<TapControl>();
        let mut dispatch_tx = Some(dispatch_sender);
        // Every worker takes a clone; the original drops at the end of this
        // function, so the receiver disconnects exactly when the last worker
        // exits. See the `stopped` field.
        let (stopped_tx, stopped) = mpsc::channel::<()>();
        // Join order at teardown is this order. The event thread owns the other
        // sender clone, so it must be joined before the dispatcher can see its
        // channel close.
        let threads = {
            let mut guard = SpawnGuard {
                conn: &conn,
                state: &state,
                wake_window,
                wake_atom,
                dispatch_tx: &mut dispatch_tx,
                stopped_tx: Some(stopped_tx),
                stopped: &stopped,
                threads: Vec::with_capacity(3),
                armed: true,
            };
            guard.threads.push(spawn_event_thread(
                &mut spawner,
                Arc::clone(&conn),
                Arc::clone(&state),
                guard.dispatch_tx.as_ref().expect("sender").clone(),
                guard.stopped_tx.as_ref().expect("stop sender").clone(),
            )?);
            guard.threads.push(spawn_dispatcher(
                &mut spawner,
                callback,
                dispatch_rx,
                guard.stopped_tx.as_ref().expect("stop sender").clone(),
            )?);
            guard.threads.push(spawn_watchdog(
                &mut spawner,
                Arc::clone(&conn),
                Arc::clone(&state),
                guard.stopped_tx.take().expect("stop sender"),
            )?);
            guard.armed = false;
            std::mem::take(&mut guard.threads)
        };

        Ok(Arc::new(Self {
            conn,
            state,
            wake_window,
            wake_atom,
            dispatch_tx,
            threads: Mutex::new(threads),
            stopped: Mutex::new(stopped),
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

    pub fn rearm(&self) -> Result<(), PlatformError> {
        if !self.state.active.load(Ordering::Acquire) {
            return Ok(());
        }
        regrab(
            &self.conn,
            &self.state,
            crate::x11_keys::configured_bindings(),
        )
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
        ungrab_plan(
            &self.conn,
            self.state.root,
            &self
                .state
                .plan
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        *self
            .state
            .grabbed
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = false;
        // End the dispatcher (drop its sender) and wake the event thread out of
        // its blocking wait_for_event.
        self.dispatch_tx = None;
        wake_event_thread(&self.conn, self.wake_window, self.wake_atom);
        // Bounded wait, then detach — the same posture as the events subsystem.
        // An unbounded join here parks the run loop forever if a worker misses
        // its wake (a lost `SendEvent`, an X server that stopped answering), and
        // this runs on the thread that drives the whole product. The keyboard is
        // already thawed and every key ungrabbed above, so a detached worker
        // holds nothing a user or the host can observe.
        let all_exited = matches!(
            self.stopped
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .recv_timeout(STOP_TIMEOUT),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
        let threads =
            std::mem::take(&mut *self.threads.lock().unwrap_or_else(PoisonError::into_inner));
        if all_exited {
            for thread in threads {
                let _ = thread.join();
            }
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
    spawner: &mut WorkerSpawner,
    callback: AcceptCallback,
    rx: mpsc::Receiver<TapControl>,
    stopped: mpsc::Sender<()>,
) -> Result<JoinHandle<()>, PlatformError> {
    spawner.spawn("compme-keytap-dispatch", "dispatcher thread", move || {
        let _stopped = stopped;
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
}

fn spawn_event_thread(
    spawner: &mut WorkerSpawner,
    conn: Arc<RustConnection>,
    state: Arc<TapState>,
    dispatch: mpsc::Sender<TapControl>,
    stopped: mpsc::Sender<()>,
) -> Result<JoinHandle<()>, PlatformError> {
    spawner.spawn("compme-keytap", "event thread", move || {
        let _stopped = stopped;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_event_loop(&conn, &state, &dispatch);
        }));
        // Whatever ended the loop — a normal stop, a connection error, or a
        // panic — leave the keyboard usable and the grabs released.
        thaw(&conn);
        ungrab_plan(
            &conn,
            state.root,
            &state.plan.lock().unwrap_or_else(PoisonError::into_inner),
        );
        state.frozen_since_ms.store(UNSET_MS, Ordering::Release);
        if outcome.is_err() {
            *state.grabbed.lock().unwrap_or_else(PoisonError::into_inner) = false;
        }
    })
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
            Event::MappingNotify(mapping)
                if mapping.request == Mapping::KEYBOARD || mapping.request == Mapping::MODIFIER =>
            {
                let _ = regrab(conn, state, crate::x11_keys::configured_bindings());
            }
            // The teardown wake, or anything else: nothing to resolve.
            _ => {}
        }
    }
}

/// Resolve exactly one grabbed keystroke.
///
/// **The keyboard is frozen for every application while this runs.** Everything
/// before `allow_events` is a bounded snapshot of already-loaded state; the
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
    let decision = {
        let plan = state.plan.lock().unwrap_or_else(PoisonError::into_inner);
        plan.decision_for_keycode(keycode, modifiers, state.armed_action())
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
    spawner: &mut WorkerSpawner,
    conn: Arc<RustConnection>,
    state: Arc<TapState>,
    stopped: mpsc::Sender<()>,
) -> Result<JoinHandle<()>, PlatformError> {
    spawner.spawn("compme-keytap-watchdog", "watchdog thread", move || {
        let _stopped = stopped;
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
                    // Without taking a tap-state lock first, thaw the
                    // keyboard within FREEZE_BUDGET_MS.
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
}
