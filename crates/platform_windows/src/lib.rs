//! Windows platform adapter — SCAFFOLD (ROADMAP Tier 1.1).
//!
//! Implements the [`platform::PlatformAdapter`] contract so the cross-platform
//! structure exists and CI can gate it, but the real Windows API integration is
//! **not yet built** — it requires a Windows build+test environment (this
//! scaffold was authored on a macOS-only host). Every method is a fail-closed
//! stub returning [`PlatformError::UnsupportedField`] (IO/subscribe) or a safe
//! empty value, so wiring this adapter in is inert, never a crash. Each method's
//! doc names the Win32 API its real implementation will use.

use platform::{
    AcceptCallback, AcceptSubscription, AppId, Capabilities, CaretCallback, Environment,
    FieldHandle, FocusCallback, InsertStrategy, Inserted, OperatingSystem, PlatformAdapter,
    PlatformError, ScreenRect, Subscription, TextContext,
};

/// Windows implementation of [`PlatformAdapter`] — scaffold (see module docs).
/// Implementation map for the real adapter (built on a Windows host):
/// - focus / caret events → UI Automation (`IUIAutomation` + event handlers)
/// - capabilities / read_context / caret_rect → UIA TextPattern + bounding rects
/// - subscribe_accept → low-level keyboard hook (`WH_KEYBOARD_LL`)
/// - insert / insert_replacing → UIA ValuePattern, else `SendInput` synthetic keys
/// - overlay → a layered, click-through, topmost window (separate `OverlayPresenter`)
#[derive(Debug, Default)]
pub struct WindowsAdapter;

impl WindowsAdapter {
    pub fn new() -> Self {
        Self
    }

    /// The error every not-yet-implemented method returns. Fail-closed: the host
    /// treats any error as "no suggestion this turn" and leaves the field
    /// untouched, so an unwired Windows adapter is inert, never harmful.
    fn unsupported(method: &str) -> PlatformError {
        PlatformError::UnsupportedField {
            reason: format!("platform_windows::{method} not yet implemented (Tier 1.1 scaffold)"),
        }
    }
}

impl PlatformAdapter for WindowsAdapter {
    /// Real impl: `RtlGetVersion`. Cheap + infallible per the contract.
    fn environment(&self) -> Environment {
        Environment {
            os: OperatingSystem::Windows,
            version: "unknown".to_string(),
        }
    }

    /// Real impl: UI Automation focus-changed event handler.
    fn subscribe_focus(&self, _cb: FocusCallback) -> Result<Subscription, PlatformError> {
        Err(Self::unsupported("subscribe_focus"))
    }

    /// Real impl: UIA TextPattern caret + structure-changed events.
    fn subscribe_caret(&self, _cb: CaretCallback) -> Result<Subscription, PlatformError> {
        Err(Self::unsupported("subscribe_caret"))
    }

    /// Real impl: a `WH_KEYBOARD_LL` low-level hook gating the accept/dismiss keys.
    fn subscribe_accept(&self, _cb: AcceptCallback) -> Result<AcceptSubscription, PlatformError> {
        Err(Self::unsupported("subscribe_accept"))
    }

    /// Real impl: `GetForegroundWindow` → `GetWindowThreadProcessId` → module name.
    fn front_app(&self) -> Option<AppId> {
        None
    }

    /// Real impl: UIA control/value/text patterns + secure-desktop probe.
    fn capabilities(&self, _field: &FieldHandle) -> Result<Capabilities, PlatformError> {
        Err(Self::unsupported("capabilities"))
    }

    /// Real impl: UIA TextPattern range around the caret.
    fn read_context(&self, _field: &FieldHandle) -> Result<TextContext, PlatformError> {
        Err(Self::unsupported("read_context"))
    }

    /// Real impl: UIA TextPattern bounding rectangle of the caret/selection.
    fn caret_rect(&self, _field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
        Err(Self::unsupported("caret_rect"))
    }

    /// Real impl: UIA ValuePattern set, else `SendInput` synthetic typing.
    fn insert(
        &self,
        _field: &FieldHandle,
        _text: &str,
        _strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        Err(Self::unsupported("insert"))
    }

    /// Real impl: UIA range-replace, else backspace×N + `SendInput` typing.
    fn insert_replacing(
        &self,
        _field: &FieldHandle,
        _text: &str,
        _replace_left: usize,
        _strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        Err(Self::unsupported("insert_replacing"))
    }
}

/// Windows implementation of `platform::shell::ShellHost` — fail-closed scaffold.
/// Future real impl: Win32 message pump, DPAPI key storage, settings deep-links,
/// Explorer reveal, and Startup-approved launch-at-login registration.
#[derive(Debug, Default)]
pub struct WindowsShellHost;

impl WindowsShellHost {
    pub fn new() -> Self {
        Self
    }
}

impl platform::shell::ShellHost for WindowsShellHost {
    fn pump_events(&self, heartbeat: std::time::Duration) {
        std::thread::sleep(heartbeat);
    }

    fn physical_memory_bytes(&self) -> u64 {
        0
    }

    fn open_url(&self, url: &str) -> Result<(), PlatformError> {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map(drop)
            .map_err(|e| PlatformError::CannotComplete {
                reason: format!("start {url}: {e}"),
            })
    }

    fn open_permission_settings(&self) -> Result<(), PlatformError> {
        Err(WindowsAdapter::unsupported("open_permission_settings"))
    }

    fn reveal_file(&self, _path: &std::path::Path) -> Result<(), PlatformError> {
        Err(WindowsAdapter::unsupported("reveal_file"))
    }

    fn set_launch_at_login(&self, _enabled: bool) -> Result<(), PlatformError> {
        Err(WindowsAdapter::unsupported("set_launch_at_login"))
    }

    fn confirm(&self, _prompt: &platform::shell::ConfirmPrompt<'_>) -> Result<bool, PlatformError> {
        Err(WindowsAdapter::unsupported("confirm"))
    }

    fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError> {
        Err(WindowsAdapter::unsupported("load_or_create_memory_key"))
    }
}

/// Windows ghost overlay scaffold. Future real impl: layered, click-through,
/// topmost window anchored in global screen coordinates.
#[derive(Debug, Default)]
pub struct WindowsOverlayPresenter;

impl WindowsOverlayPresenter {
    pub fn new() -> Self {
        Self
    }
}

impl platform::OverlayPresenter for WindowsOverlayPresenter {
    fn show_ghost(&mut self, _anchor: ScreenRect, _text: &str) -> Result<(), PlatformError> {
        Err(WindowsAdapter::unsupported("show_ghost"))
    }

    fn show_correction(
        &mut self,
        _rect: ScreenRect,
        _suggestion: &str,
    ) -> Result<(), PlatformError> {
        Err(WindowsAdapter::unsupported("show_correction"))
    }

    fn update_ghost(&mut self, _text: &str) -> Result<(), PlatformError> {
        Err(WindowsAdapter::unsupported("update_ghost"))
    }

    fn hide(&mut self) -> Result<(), PlatformError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn scaffold_reports_windows_and_fails_closed() {
        let adapter = WindowsAdapter::new();
        // environment() is the one cheap, infallible method the scaffold answers.
        assert_eq!(adapter.environment().os, OperatingSystem::Windows);
        // The scaffold has no real version probe yet: it reports the fixed
        // "unknown" version. Pin it so the real version impl visibly replaces
        // the placeholder.
        assert_eq!(adapter.environment().version, "unknown");
        // No frontmost app until the real impl lands.
        assert_eq!(adapter.front_app(), None);
        // Subscribe/IO methods fail closed (UnsupportedField), never panic — the
        // host treats this as "no suggestion this turn" and leaves fields alone.
        let cb: FocusCallback = Arc::new(|_field| {});
        assert!(matches!(
            adapter.subscribe_focus(cb),
            Err(PlatformError::UnsupportedField { .. })
        ));
        // insert_replacing is the method whose missing/wrong impl caused the
        // historical `:smile😄` append-only bug, so pin that the scaffold returns
        // an error rather than silently delegating to an append-only insert.
        let field = FieldHandle {
            app: "test".to_string(),
            pid: None,
            element_id: "scaffold".to_string(),
            generation: 0,
        };
        assert!(matches!(
            adapter.insert_replacing(&field, "x", 1, InsertStrategy::None),
            Err(PlatformError::UnsupportedField { .. })
        ));
        assert!(matches!(
            adapter.text_range_rect(&field, platform::CorrectionRange { start: 0, end: 1 }),
            Err(PlatformError::UnsupportedField { .. })
        ));
        assert!(matches!(
            adapter.insert_replacing_range(
                &field,
                "old",
                "x",
                platform::CorrectionRange { start: 0, end: 1 },
                InsertStrategy::AxSet,
            ),
            Err(PlatformError::UnsupportedField { .. })
        ));
        // The two methods the scaffold inherits as trait defaults (fail-OPEN by
        // design: "no anchor / no domain", which is safe) are pinned here so a
        // future change to the trait defaults can't silently alter stub behavior.
        assert!(matches!(adapter.popup_anchor(&field), Ok(None)));
        assert!(matches!(adapter.focused_page_url(&field), Ok(None)));
    }

    #[test]
    fn every_io_and_subscribe_method_fails_closed() {
        // Fail-closed is the scaffold's whole point: the prior test pinned only
        // subscribe_focus + insert_replacing. Pin the rest so any one method
        // regressing to Ok (e.g. an accidental stub returning empty caps) is a
        // test failure, not a silent live-fire of an unimplemented adapter.
        let adapter = WindowsAdapter::new();
        let field = FieldHandle {
            app: "test".to_string(),
            pid: None,
            element_id: "scaffold".to_string(),
            generation: 0,
        };

        let caret_cb: CaretCallback = Arc::new(|_field, _rect| {});
        assert!(matches!(
            adapter.subscribe_caret(caret_cb),
            Err(PlatformError::UnsupportedField { .. })
        ));
        let accept_cb: AcceptCallback = Arc::new(|_tap| {});
        assert!(matches!(
            adapter.subscribe_accept(accept_cb),
            Err(PlatformError::UnsupportedField { .. })
        ));
        assert!(matches!(
            adapter.capabilities(&field),
            Err(PlatformError::UnsupportedField { .. })
        ));
        assert!(matches!(
            adapter.read_context(&field),
            Err(PlatformError::UnsupportedField { .. })
        ));
        assert!(matches!(
            adapter.caret_rect(&field),
            Err(PlatformError::UnsupportedField { .. })
        ));
        assert!(matches!(
            adapter.insert(&field, "x", InsertStrategy::None),
            Err(PlatformError::UnsupportedField { .. })
        ));
    }

    #[test]
    fn insert_fails_closed_for_every_strategy_variant() {
        // The prior tests only exercised InsertStrategy::None. A real adapter that
        // half-landed (e.g. an AxSet/Clipboard branch returning Ok before the
        // others) must not slip past the scaffold gate, so pin BOTH insert and
        // insert_replacing as UnsupportedField across ALL strategies. If a variant
        // is added to InsertStrategy, this match goes non-exhaustive and forces an
        // update.
        let adapter = WindowsAdapter::new();
        let field = FieldHandle {
            app: "test".to_string(),
            pid: None,
            element_id: "scaffold".to_string(),
            generation: 0,
        };
        for strategy in [
            InsertStrategy::AxSet,
            InsertStrategy::NativeRangeSet,
            InsertStrategy::SyntheticKeys,
            InsertStrategy::Clipboard,
            InsertStrategy::ImeCommit,
            InsertStrategy::None,
        ] {
            // Exhaustive, wildcard-free: a new InsertStrategy variant breaks
            // compilation here and forces the array above to be updated too.
            match strategy {
                InsertStrategy::AxSet
                | InsertStrategy::NativeRangeSet
                | InsertStrategy::SyntheticKeys
                | InsertStrategy::Clipboard
                | InsertStrategy::ImeCommit
                | InsertStrategy::None => {}
            }
            assert!(
                matches!(
                    adapter.insert(&field, "x", strategy),
                    Err(PlatformError::UnsupportedField { .. })
                ),
                "insert {strategy:?}"
            );
            assert!(
                matches!(
                    adapter.insert_replacing(&field, "x", 1, strategy),
                    Err(PlatformError::UnsupportedField { .. })
                ),
                "insert_replacing {strategy:?}"
            );
            assert!(
                matches!(
                    adapter.insert_replacing_range(
                        &field,
                        "old",
                        "x",
                        platform::CorrectionRange { start: 0, end: 1 },
                        strategy,
                    ),
                    Err(PlatformError::UnsupportedField { .. })
                ),
                "insert_replacing_range {strategy:?}"
            );
        }
    }

    #[test]
    fn insert_replacing_zero_replace_left_also_fails_closed() {
        // The trait mandates that `replace_left == 0` behaves as a plain insert
        // (no backspaces). The prior matrix test only used replace_left == 1, so
        // pin that the scaffold still fails closed for the insert-like zero case
        // across every strategy — an adapter that special-cased replace_left == 0
        // to an Ok append must not slip past the gate.
        let adapter = WindowsAdapter::new();
        let field = FieldHandle {
            app: "test".to_string(),
            pid: None,
            element_id: "scaffold".to_string(),
            generation: 0,
        };
        for strategy in [
            InsertStrategy::AxSet,
            InsertStrategy::NativeRangeSet,
            InsertStrategy::SyntheticKeys,
            InsertStrategy::Clipboard,
            InsertStrategy::ImeCommit,
            InsertStrategy::None,
        ] {
            assert!(
                matches!(
                    adapter.insert_replacing(&field, "x", 0, strategy),
                    Err(PlatformError::UnsupportedField { .. })
                ),
                "insert_replacing replace_left=0 {strategy:?}"
            );
        }
    }

    #[test]
    fn unsupported_reason_names_the_failing_method() {
        // Fail-closed isn't enough: when a stub rejects, its diagnostic must name
        // BOTH the crate and the exact method, so an operator reading a log can
        // tell *which* unimplemented call fired. Pin the real reason format
        // ("platform_windows::<method> not yet implemented (Tier 1.1 scaffold)")
        // across a representative spread — a subscribe, a capability probe, and an
        // insert — so a future refactor of `unsupported()` can't drop the method
        // name (or the crate prefix) without breaking a test.
        let adapter = WindowsAdapter::new();
        let field = FieldHandle {
            app: "test".to_string(),
            pid: None,
            element_id: "scaffold".to_string(),
            generation: 0,
        };

        let Err(PlatformError::UnsupportedField { reason }) = adapter.capabilities(&field) else {
            panic!("capabilities should fail closed with UnsupportedField");
        };
        assert!(
            reason.contains("platform_windows::"),
            "reason should carry the crate prefix: {reason:?}"
        );
        assert!(
            reason.contains("capabilities"),
            "reason should name the failing method `capabilities`: {reason:?}"
        );
        assert!(
            reason.contains("not yet implemented (Tier 1.1 scaffold)"),
            "reason should explain the stub is a scaffold: {reason:?}"
        );
        assert_eq!(
            reason, "platform_windows::capabilities not yet implemented (Tier 1.1 scaffold)",
            "full reason string format pinned"
        );

        let caret_cb: CaretCallback = Arc::new(|_field, _rect| {});
        let Err(PlatformError::UnsupportedField { reason }) = adapter.subscribe_caret(caret_cb)
        else {
            panic!("subscribe_caret should fail closed with UnsupportedField");
        };
        assert!(
            reason.contains("platform_windows::") && reason.contains("subscribe_caret"),
            "reason should name crate + `subscribe_caret`: {reason:?}"
        );

        let Err(PlatformError::UnsupportedField { reason }) =
            adapter.insert_replacing(&field, "x", 1, InsertStrategy::None)
        else {
            panic!("insert_replacing should fail closed with UnsupportedField");
        };
        assert!(
            reason.contains("platform_windows::") && reason.contains("insert_replacing"),
            "reason should name crate + `insert_replacing`: {reason:?}"
        );
    }

    #[test]
    fn shell_host_is_fail_closed() {
        use platform::shell::{ConfirmPrompt, ShellHost};

        let h = WindowsShellHost::new();
        assert!(!h.secure_input_enabled());
        assert!(!h.screen_capture_permission());
        assert!(
            h.load_or_create_memory_key().is_err(),
            "no key store yet -- must fail closed"
        );
        assert!(h
            .confirm(&ConfirmPrompt {
                title: "t",
                message: "m",
                confirm_label: "c"
            })
            .is_err());
        assert!(h.set_launch_at_login(true).is_err());
        assert!(h.reveal_file(std::path::Path::new("x")).is_err());
        assert!(h.open_permission_settings().is_err());
        let start = std::time::Instant::now();
        h.pump_events(std::time::Duration::from_millis(5));
        assert!(start.elapsed() >= std::time::Duration::from_millis(5));
    }

    #[test]
    fn overlay_is_fail_closed_and_hide_is_idempotent() {
        use platform::OverlayPresenter;

        let mut o = WindowsOverlayPresenter::new();
        assert!(o
            .show_ghost(
                ScreenRect {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                    h: 1.0
                },
                "g",
            )
            .is_err());
        let correction = o
            .show_correction(
                ScreenRect {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                    h: 1.0,
                },
                "c",
            )
            .unwrap_err();
        assert!(matches!(
            correction,
            PlatformError::UnsupportedField { reason }
                if reason.contains("platform_windows::show_correction")
        ));
        assert!(o.update_ghost("g").is_err());
        o.hide().expect("hide is contractually idempotent-success");
        o.hide().expect("second hide too");
    }
}

/// Real Windows host services that do not need the full UIA adapter
/// (cross-platform plan Phase 0.2/0.3). Everything here is `cfg(windows)`:
/// non-Windows hosts compile the fail-closed scaffold above only.
#[cfg(windows)]
pub mod win_host {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;

    use windows::core::{BOOL, PCWSTR};
    use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SetNamedSecurityInfoW,
        SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows::Win32::Security::{
        GetSecurityDescriptorDacl, ACL, DACL_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };
    use windows::Win32::System::Console::SetConsoleCtrlHandler;

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Owner-only DACL, inheritance removed: the Windows analog of the unix
    /// 0700/0600 tightening. `OW` (OWNER_RIGHTS) grants full control to the
    /// current owner only; `OICI` makes children created under a hardened
    /// directory inherit the restriction, and `SetNamedSecurityInfoW`
    /// propagates it to children that already exist.
    const OWNER_ONLY_SDDL: &str = "D:P(A;OICI;FA;;;OW)";

    pub fn harden_owner_only(path: &Path) -> std::io::Result<()> {
        let sddl: Vec<u16> = OWNER_ONLY_SDDL
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: sddl is NUL-terminated; sd receives a LocalAlloc'd buffer we
        // free below; dacl points into that buffer and is not used after free
        // except by SetNamedSecurityInfoW, which copies it.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
            .map_err(std::io::Error::other)?;
            let mut present = BOOL(0);
            let mut defaulted = BOOL(0);
            let mut dacl: *mut ACL = std::ptr::null_mut();
            let got = GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted);
            let result = match got {
                Err(e) => Err(std::io::Error::other(e)),
                Ok(()) if !present.as_bool() || dacl.is_null() => {
                    Err(std::io::Error::other("owner-only SDDL produced no DACL"))
                }
                Ok(()) => {
                    let target = wide(path);
                    let err = SetNamedSecurityInfoW(
                        PCWSTR(target.as_ptr()),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                        None,
                        None,
                        Some(dacl),
                        None,
                    );
                    if err == ERROR_SUCCESS {
                        Ok(())
                    } else {
                        Err(std::io::Error::from_raw_os_error(err.0 as i32))
                    }
                }
            };
            LocalFree(Some(HLOCAL(sd.0)));
            result
        }
    }

    static STOP_FLAG: OnceLock<&'static AtomicBool> = OnceLock::new();

    unsafe extern "system" fn on_console_ctrl(_ctrl_type: u32) -> BOOL {
        // Handler runs on its own thread: only a relaxed atomic store, the
        // same contract as the unix signal handlers. Returning handled is
        // meaningful for CTRL_C/CTRL_BREAK; for CLOSE/LOGOFF/SHUTDOWN the OS
        // ignores the return value and terminates after the grace window —
        // the flag just gives the loop its chance at a clean exit first.
        match STOP_FLAG.get() {
            Some(flag) => {
                flag.store(true, Ordering::Relaxed);
                BOOL(1)
            }
            None => BOOL(0),
        }
    }

    /// Ctrl-C / Ctrl-Break / console-close parity with SIGINT/SIGTERM: sets
    /// `stop` and reports the event handled. Install once; a second install
    /// keeps the first flag (OnceLock) and re-registration is harmless.
    pub fn install_console_ctrl_handler(stop: &'static AtomicBool) -> std::io::Result<()> {
        let _ = STOP_FLAG.set(stop);
        // SAFETY: the handler only touches a static AtomicBool.
        unsafe { SetConsoleCtrlHandler(Some(on_console_ctrl), true).map_err(std::io::Error::other) }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows::Win32::Security::Authorization::GetNamedSecurityInfoW;
        use windows::Win32::Security::{
            AclSizeInformation, GetAclInformation, ACL_SIZE_INFORMATION,
        };

        fn ace_count(path: &Path) -> u32 {
            let target = wide(path);
            let mut dacl: *mut ACL = std::ptr::null_mut();
            let mut sd = PSECURITY_DESCRIPTOR::default();
            // SAFETY: out-pointers are valid; sd freed below.
            unsafe {
                let err = GetNamedSecurityInfoW(
                    PCWSTR(target.as_ptr()),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    None,
                    None,
                    Some(&mut dacl),
                    None,
                    &mut sd,
                );
                assert_eq!(err, ERROR_SUCCESS, "GetNamedSecurityInfoW failed");
                let mut info = ACL_SIZE_INFORMATION::default();
                GetAclInformation(
                    dacl,
                    &mut info as *mut _ as *mut _,
                    std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                    AclSizeInformation,
                )
                .expect("GetAclInformation");
                LocalFree(Some(HLOCAL(sd.0)));
                info.AceCount
            }
        }

        #[test]
        fn harden_owner_only_leaves_a_single_owner_ace() {
            let dir =
                std::env::temp_dir().join(format!("compme-harden-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            harden_owner_only(&dir).expect("harden dir");
            assert_eq!(ace_count(&dir), 1, "dir DACL must be the single owner ACE");

            // A file created AFTER hardening inherits the owner-only ACE.
            let child = dir.join("child.txt");
            std::fs::write(&child, b"x").unwrap();
            assert_eq!(ace_count(&child), 1, "child must inherit owner-only DACL");

            // A file that existed BEFORE a (re-)harden gets the propagated ACE.
            harden_owner_only(&dir).expect("re-harden");
            assert_eq!(ace_count(&child), 1);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn console_ctrl_handler_sets_the_stop_flag() {
            static STOP: AtomicBool = AtomicBool::new(false);
            install_console_ctrl_handler(&STOP).expect("install");
            // Invoke the handler directly (generating a real console event
            // would signal the whole CI process group).
            // SAFETY: handler only stores to the static flag.
            let handled = unsafe { on_console_ctrl(0) };
            assert!(handled.as_bool());
            assert!(STOP.load(Ordering::Relaxed));
        }
    }
}
