//! Process-lifetime X11 global shortcuts.
//!
//! Unlike the suggestion-scoped accept tap, these grabs are always active and
//! use `GrabMode::Async`: firing a configured shortcut consumes that chord and
//! dispatches a [`platform::TapControl::Shortcut`], but can never freeze the
//! keyboard. No configured bindings means no X connection and no worker.

use platform::ShortcutAction;
use shell_flags::ShortcutBindings;
use std::sync::{OnceLock, PoisonError, RwLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShortcutBinding {
    pub keysym: u32,
    pub modifiers: u16,
    pub action: ShortcutAction,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShortcutPlan {
    bindings: Vec<ShortcutBinding>,
}

impl ShortcutPlan {
    pub fn from_bindings(bindings: ShortcutBindings) -> Self {
        let bindings = [
            (bindings.force_activate, ShortcutAction::ForceActivate),
            (bindings.toggle_app, ShortcutAction::ToggleApp),
            (bindings.toggle_global, ShortcutAction::ToggleGlobal),
            (bindings.grammar_check, ShortcutAction::GrammarCheck),
        ]
        .into_iter()
        .filter_map(|(chord, action)| {
            let (keycode, mask) = chord?;
            Some(ShortcutBinding {
                keysym: crate::x11_keys::keysym_for_mac_keycode(keycode)?,
                modifiers: crate::x11_keys::x11_modifiers_for_mac_mask(mask),
                action,
            })
        })
        .collect();
        Self { bindings }
    }

    pub fn action_for(&self, keysym: u32, modifiers: u16) -> Option<ShortcutAction> {
        let held = modifiers & crate::x11_keys::SIGNIFICANT_MODIFIERS;
        self.bindings
            .iter()
            .find(|binding| binding.keysym == keysym && binding.modifiers == held)
            .map(|binding| binding.action)
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn iter(&self) -> impl Iterator<Item = &ShortcutBinding> {
        self.bindings.iter()
    }
}

fn configured_cell() -> &'static RwLock<ShortcutBindings> {
    static CONFIGURED: OnceLock<RwLock<ShortcutBindings>> = OnceLock::new();
    CONFIGURED.get_or_init(|| RwLock::new(ShortcutBindings::default()))
}

pub fn set_bindings(bindings: ShortcutBindings) {
    *configured_cell()
        .write()
        .unwrap_or_else(PoisonError::into_inner) = bindings;
}

pub fn configured_bindings() -> ShortcutBindings {
    *configured_cell()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Whether every configured macOS keycode has an X11 keysym translation.
/// Unknown keys must be refused at configuration time; silently dropping one
/// would make the UI/log claim a shortcut that can never fire.
pub fn bindings_supported(bindings: ShortcutBindings) -> bool {
    let configured = [
        bindings.force_activate,
        bindings.toggle_app,
        bindings.toggle_global,
        bindings.grammar_check,
    ]
    .into_iter()
    .flatten()
    .count();
    let plan = ShortcutPlan::from_bindings(bindings);
    plan.bindings.len() == configured
        && !conflicts_with_accept(&plan, &crate::x11_keys::configured_bindings())
}

fn conflicts_with_accept(plan: &ShortcutPlan, accept: &crate::x11_keys::AcceptBindings) -> bool {
    plan.bindings
        .iter()
        .any(|binding| accept.role_for(binding.keysym, binding.modifiers).is_some())
}

#[cfg(target_os = "linux")]
mod live {
    use super::*;
    use platform::{AcceptCallback, PlatformError, TapControl};
    use std::os::fd::AsFd as _;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::Duration;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, GrabMode, Mapping, ModMask, Window};
    use x11rb::protocol::Event;
    use x11rb::rust_connection::RustConnection;

    const STOP_TIMEOUT: Duration = Duration::from_secs(2);

    #[derive(Clone, Copy, Debug)]
    struct ResolvedBinding {
        keycode: u8,
        modifiers: u16,
        action: ShortcutAction,
    }

    #[derive(Debug, Default)]
    struct ResolvedPlan {
        bindings: Vec<ResolvedBinding>,
        grabs: Vec<(u8, u16)>,
    }

    impl ResolvedPlan {
        fn action_for(&self, keycode: u8, state: u16) -> Option<ShortcutAction> {
            let held = state & crate::x11_keys::SIGNIFICANT_MODIFIERS;
            self.bindings
                .iter()
                .find(|binding| binding.keycode == keycode && binding.modifiers == held)
                .map(|binding| binding.action)
        }
    }

    fn cannot_complete(what: &str, err: impl std::fmt::Display) -> PlatformError {
        PlatformError::CannotComplete {
            reason: format!("platform_linux X11 shortcuts {what}: {err}"),
        }
    }

    fn expanded_lock_masks(base: u16, numlock: u16) -> Vec<u16> {
        let mut masks = Vec::with_capacity(4);
        for mask in [
            base,
            base | u16::from(ModMask::LOCK),
            base | numlock,
            base | u16::from(ModMask::LOCK) | numlock,
        ] {
            if !masks.contains(&mask) {
                masks.push(mask);
            }
        }
        masks
    }

    fn build_plan(
        conn: &RustConnection,
        plan: &ShortcutPlan,
    ) -> Result<ResolvedPlan, PlatformError> {
        let setup = conn.setup();
        let min = setup.min_keycode;
        let count = setup.max_keycode.saturating_sub(min).saturating_add(1);
        let mapping = conn
            .get_keyboard_mapping(min, count)
            .map_err(|err| cannot_complete("get_keyboard_mapping", err))?
            .reply()
            .map_err(|err| cannot_complete("keyboard mapping reply", err))?;
        let modifiers = conn
            .get_modifier_mapping()
            .map_err(|err| cannot_complete("get_modifier_mapping", err))?
            .reply()
            .map_err(|err| cannot_complete("modifier mapping reply", err))?;
        let per = usize::from(mapping.keysyms_per_keycode);
        let width = usize::from(modifiers.keycodes_per_modifier());
        let numlock = if width == 0 {
            0
        } else {
            modifiers
                .keycodes
                .chunks(width)
                .enumerate()
                .find_map(|(index, keys)| {
                    keys.iter()
                        .copied()
                        .filter(|key| *key >= min)
                        .any(|key| {
                            let start = usize::from(key - min).saturating_mul(per);
                            mapping
                                .keysyms
                                .get(start..start.saturating_add(per))
                                .is_some_and(|symbols| symbols.contains(&0xff7f))
                        })
                        .then(|| 1u16 << index)
                })
                .unwrap_or(0)
        };

        let mut resolved = ResolvedPlan::default();
        for binding in &plan.bindings {
            let keycode = crate::x11_keys::keycode_for_keysym(
                min,
                mapping.keysyms_per_keycode,
                &mapping.keysyms,
                binding.keysym,
            )
            .ok_or_else(|| PlatformError::UnsupportedField {
                reason: format!(
                    "platform_linux X11 shortcuts: configured {:?} key 0x{:x} is absent from \
                     this keyboard layout; refusing the entire shortcut set",
                    binding.action, binding.keysym
                ),
            })?;
            resolved.bindings.push(ResolvedBinding {
                keycode,
                modifiers: binding.modifiers,
                action: binding.action,
            });
            for mask in expanded_lock_masks(binding.modifiers, numlock) {
                if !resolved.grabs.contains(&(keycode, mask)) {
                    resolved.grabs.push((keycode, mask));
                }
            }
        }
        if resolved.bindings.is_empty() {
            return Err(PlatformError::UnsupportedField {
                reason: "platform_linux X11 shortcuts: this keyboard layout carries none of the configured keys".into(),
            });
        }
        Ok(resolved)
    }

    fn ungrab(conn: &RustConnection, root: Window, grabs: &[(u8, u16)]) {
        for &(keycode, modifiers) in grabs {
            if let Ok(cookie) = conn.ungrab_key(keycode, root, ModMask::from(modifiers)) {
                cookie.ignore_error();
            }
        }
        let _ = conn.flush();
    }

    fn grab(conn: &RustConnection, root: Window, plan: &ResolvedPlan) -> Result<(), PlatformError> {
        for (index, &(keycode, modifiers)) in plan.grabs.iter().enumerate() {
            let result = conn
                .grab_key(
                    false,
                    root,
                    ModMask::from(modifiers),
                    keycode,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )
                .map_err(|err| cannot_complete("grab_key", err))
                .and_then(|cookie| {
                    cookie
                        .check()
                        .map_err(|err| cannot_complete("grab_key rejected", err))
                });
            if let Err(err) = result {
                ungrab(conn, root, &plan.grabs[..index]);
                return Err(err);
            }
        }
        conn.flush()
            .map_err(|err| cannot_complete("flush grabs", err))
    }

    pub struct X11ShortcutTap {
        stopping: Arc<AtomicBool>,
        /// A duplicate of the X11 socket. `shutdown(Both)` interrupts a worker
        /// blocked in either an X reply or `wait_for_event` and makes the X
        /// server release every passive grab before Drop waits.
        shutdown_stream: UnixStream,
        dispatch: Option<mpsc::Sender<TapControl>>,
        threads: Vec<JoinHandle<()>>,
        stopped: Mutex<mpsc::Receiver<()>>,
    }

    impl X11ShortcutTap {
        pub fn install(callback: AcceptCallback) -> Result<Option<Arc<Self>>, PlatformError> {
            let plan = ShortcutPlan::from_bindings(configured_bindings());
            if plan.is_empty() {
                return Ok(None);
            }
            let (conn, screen) = x11rb::connect(None)
                .map_err(|err| cannot_complete("connect (DISPLAY unavailable)", err))?;
            let root = conn
                .setup()
                .roots
                .get(screen)
                .ok_or_else(|| cannot_complete("screen", "the display reported no screen"))?
                .root;
            let resolved = build_plan(&conn, &plan)?;
            grab(&conn, root, &resolved)?;
            let shutdown_stream = UnixStream::from(
                conn.stream()
                    .as_fd()
                    .try_clone_to_owned()
                    .map_err(|err| cannot_complete("duplicate connection for teardown", err))?,
            );
            let stopping = Arc::new(AtomicBool::new(false));
            let (dispatch_tx, dispatch_rx) = mpsc::channel::<TapControl>();
            let (stopped_tx, stopped) = mpsc::channel::<()>();

            // Spawn the dispatcher first. If the event worker fails to spawn,
            // dropping the only sender ends it without ever joining an X11
            // worker that might be blocked in a server round trip.
            let dispatcher_stopped = stopped_tx.clone();
            let dispatcher = std::thread::Builder::new()
                .name("compme-shortcut-dispatch".into())
                .spawn(move || {
                    let _stopped = dispatcher_stopped;
                    while let Ok(control) = dispatch_rx.recv() {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            callback(control);
                        }));
                    }
                })
                .map_err(|err| cannot_complete("dispatcher thread", err))?;

            let event_stopping = Arc::clone(&stopping);
            let event_dispatch = dispatch_tx.clone();
            let event_stopped = stopped_tx;
            let event_thread = match std::thread::Builder::new()
                .name("compme-shortcuts".into())
                .spawn(move || {
                    let _stopped = event_stopped;
                    let mut resolved = resolved;
                    while !event_stopping.load(Ordering::Acquire) {
                        let Ok(event) = conn.wait_for_event() else {
                            break;
                        };
                        match event {
                            Event::KeyPress(press) => {
                                if let Some(action) =
                                    resolved.action_for(press.detail, u16::from(press.state))
                                {
                                    let _ = event_dispatch.send(TapControl::Shortcut(action));
                                }
                            }
                            Event::MappingNotify(mapping)
                                if mapping.request == Mapping::KEYBOARD
                                    || mapping.request == Mapping::MODIFIER =>
                            {
                                // This worker solely owns the plan and X
                                // connection, so no mutex is held across these
                                // potentially blocking replies.
                                ungrab(&conn, root, &resolved.grabs);
                                match build_plan(
                                    &conn,
                                    &ShortcutPlan::from_bindings(configured_bindings()),
                                )
                                .and_then(|next| {
                                    grab(&conn, root, &next)?;
                                    Ok(next)
                                }) {
                                    Ok(next) => resolved = next,
                                    Err(err) => {
                                        eprintln!(
                                            "compme: Linux global shortcuts disabled after \
                                             keyboard-map change: {err}"
                                        );
                                        resolved = ResolvedPlan::default();
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    // Closing this sole owning connection releases every grab;
                    // no final network call is needed on the teardown path.
                }) {
                Ok(thread) => thread,
                Err(err) => {
                    drop(dispatch_tx);
                    let all_exited = matches!(
                        stopped.recv_timeout(STOP_TIMEOUT),
                        Err(mpsc::RecvTimeoutError::Disconnected)
                    );
                    if all_exited {
                        let _ = dispatcher.join();
                    }
                    return Err(cannot_complete("event thread", err));
                }
            };

            Ok(Some(Arc::new(Self {
                stopping,
                shutdown_stream,
                dispatch: Some(dispatch_tx),
                threads: vec![event_thread, dispatcher],
                stopped: Mutex::new(stopped),
            })))
        }
    }

    impl Drop for X11ShortcutTap {
        fn drop(&mut self) {
            self.stopping.store(true, Ordering::Release);
            self.dispatch = None;
            let _ = self.shutdown_stream.shutdown(std::net::Shutdown::Both);
            let all_exited = matches!(
                self.stopped
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .recv_timeout(STOP_TIMEOUT),
                Err(mpsc::RecvTimeoutError::Disconnected)
            );
            if all_exited {
                for thread in self.threads.drain(..) {
                    let _ = thread.join();
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Read as _;
        use std::time::Instant;

        #[test]
        fn drop_interrupts_worker_blocked_in_mapping_reply() {
            let (mut worker_stream, _peer) = UnixStream::pair().expect("socket pair");
            let shutdown_stream = UnixStream::from(
                worker_stream
                    .as_fd()
                    .try_clone_to_owned()
                    .expect("duplicate worker socket"),
            );
            let (ready_tx, ready_rx) = mpsc::channel();
            let (stopped_tx, stopped) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let _stopped = stopped_tx;
                ready_tx.send(()).expect("publish blocked worker");
                let mut byte = [0];
                let _ = worker_stream.read(&mut byte);
            });
            ready_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("worker reached network read");

            let tap = X11ShortcutTap {
                stopping: Arc::new(AtomicBool::new(false)),
                shutdown_stream,
                dispatch: None,
                threads: vec![worker],
                stopped: Mutex::new(stopped),
            };
            let started = Instant::now();
            drop(tap);
            assert!(started.elapsed() < Duration::from_secs(1));
        }
    }
}

#[cfg(target_os = "linux")]
pub use live::X11ShortcutTap;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_all_actions_and_matches_modifiers_exactly() {
        let plan = ShortcutPlan::from_bindings(ShortcutBindings {
            force_activate: Some((96, 1 << 8)),
            toggle_app: Some((97, 1 << 9)),
            toggle_global: Some((98, 1 << 12)),
            grammar_check: Some((100, 1 << 11)),
        });
        assert_eq!(plan.bindings.len(), 4);
        let f5 = crate::x11_keys::keysym_for_mac_keycode(96).unwrap();
        assert_eq!(
            plan.action_for(f5, crate::x11_keys::X11_MOD4),
            Some(ShortcutAction::ForceActivate)
        );
        assert_eq!(plan.action_for(f5, 0), None);
    }

    #[test]
    fn unknown_keycodes_are_omitted_and_empty_configuration_is_inert() {
        let unsupported = ShortcutBindings {
            force_activate: Some((999, 0)),
            ..ShortcutBindings::default()
        };
        let plan = ShortcutPlan::from_bindings(unsupported);
        assert!(plan.is_empty());
        assert!(!bindings_supported(unsupported));
        assert!(ShortcutPlan::from_bindings(ShortcutBindings::default()).is_empty());
    }

    #[test]
    fn always_on_shortcuts_cannot_collide_with_accept_chords() {
        let tab = ShortcutPlan::from_bindings(ShortcutBindings {
            force_activate: Some((48, 0)),
            ..ShortcutBindings::default()
        });
        assert!(conflicts_with_accept(
            &tab,
            &crate::x11_keys::AcceptBindings::defaults()
        ));

        let shifted_tab = ShortcutPlan::from_bindings(ShortcutBindings {
            force_activate: Some((48, 1 << 9)),
            ..ShortcutBindings::default()
        });
        assert!(!conflicts_with_accept(
            &shifted_tab,
            &crate::x11_keys::AcceptBindings::defaults()
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "needs an X session: run-linux-atspi-session.sh --run-in-session"]
    fn live_shortcut_is_consumed_dispatched_and_released_on_drop() {
        use x11rb::connection::Connection;
        use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode, ModMask};
        use x11rb::protocol::xtest::ConnectionExt as _;

        let previous = configured_bindings();
        set_bindings(ShortcutBindings {
            force_activate: Some((96, 0)), // F5
            ..ShortcutBindings::default()
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let tap = X11ShortcutTap::install(std::sync::Arc::new(move |control| {
            let _ = tx.send(control);
        }))
        .expect("install shortcuts")
        .expect("one configured shortcut");

        let (conn, screen) = x11rb::connect(None).expect("test X connection");
        let setup = conn.setup();
        let count = setup
            .max_keycode
            .saturating_sub(setup.min_keycode)
            .saturating_add(1);
        let mapping = conn
            .get_keyboard_mapping(setup.min_keycode, count)
            .expect("mapping request")
            .reply()
            .expect("mapping reply");
        let keycode = crate::x11_keys::keycode_for_keysym(
            setup.min_keycode,
            mapping.keysyms_per_keycode,
            &mapping.keysyms,
            crate::x11_keys::keysym_for_mac_keycode(96).expect("F5 keysym"),
        )
        .expect("F5 keycode");
        conn.xtest_fake_input(2, keycode, 0, 0, 0, 0, 0)
            .expect("fake press")
            .check()
            .expect("fake press accepted");
        conn.xtest_fake_input(3, keycode, 0, 0, 0, 0, 0)
            .expect("fake release")
            .check()
            .expect("fake release accepted");
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(1))
                .expect("shortcut callback"),
            platform::TapControl::Shortcut(ShortcutAction::ForceActivate)
        );

        drop(tap);
        let root = conn.setup().roots[screen].root;
        conn.grab_key(
            false,
            root,
            ModMask::from(0u16),
            keycode,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .expect("post-drop grab request")
        .check()
        .expect("drop releases the shortcut grab");
        let _ = conn.ungrab_key(keycode, root, ModMask::from(0u16));
        set_bindings(previous);
    }
}
