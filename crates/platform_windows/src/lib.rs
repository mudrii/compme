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
    /// Host OS version via `RtlGetVersion` — cheap and infallible per the
    /// contract. See `win_host::os_version` for the probe and the format —
    /// a plain code span, not an intra-doc link, because that module is
    /// `cfg(windows)` and the off-Windows rustdoc build cannot resolve it.
    fn environment(&self) -> Environment {
        #[cfg(windows)]
        let version = win_host::os_version();
        // Off-Windows this adapter is only constructed by the workspace's own
        // portability tests; report the shared "we could not tell" value rather
        // than inventing a version for a host this adapter does not serve.
        #[cfg(not(windows))]
        let version = UNKNOWN_VERSION.to_string();
        Environment {
            os: OperatingSystem::Windows,
            version,
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

/// Windows implementation of `platform::shell::ShellHost` — fail-closed scaffold
/// apart from the host services that are already real: the Win32 message pump,
/// the RAM probe, and native URL opening. Future real impl: DPAPI key storage,
/// settings deep-links, Explorer reveal, and Startup-approved launch-at-login
/// registration.
#[derive(Debug, Default)]
pub struct WindowsShellHost;

impl WindowsShellHost {
    pub fn new() -> Self {
        Self
    }
}

/// Reported by `environment()` when the host version cannot be determined —
/// the same literal `platform_linux::linux_version` degrades to, so a caller
/// comparing `Environment::version` across adapters sees one "could not tell"
/// value rather than a per-platform spelling of it.
const UNKNOWN_VERSION: &str = "unknown";

/// Heartbeat duration as the `dwMilliseconds` argument of
/// `MsgWaitForMultipleObjectsEx`, clamped strictly below `INFINITE`.
///
/// Win32 reads `0xFFFF_FFFF` as `INFINITE`, so a heartbeat that does not fit in
/// a `u32` must saturate to `INFINITE - 1` (~49 days) and *not* to `u32::MAX`:
/// the difference is "one absurdly long tick" versus "this thread never wakes
/// again", which would wedge the run loop rather than slow it. Pure, and
/// compiled on every host, so the clamp is provable off Windows.
#[cfg(any(windows, test))]
fn wait_timeout_ms(heartbeat: std::time::Duration) -> u32 {
    const INFINITE_MS: u32 = u32::MAX;
    u32::try_from(heartbeat.as_millis())
        .unwrap_or(INFINITE_MS)
        .min(INFINITE_MS - 1)
}

#[cfg(any(windows, test))]
fn open_url_with(
    url: &str,
    launch: impl FnOnce(&[u16]) -> Result<(), PlatformError>,
) -> Result<(), PlatformError> {
    if url.contains('\0') {
        return Err(PlatformError::CannotComplete {
            reason: "URL contains an interior NUL".into(),
        });
    }
    let wide = url
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    launch(&wide)
}

impl platform::shell::ShellHost for WindowsShellHost {
    /// See `win_host::pump_events` for why this is a message wait and not a
    /// sleep on Windows.
    fn pump_events(&self, heartbeat: std::time::Duration) {
        #[cfg(windows)]
        {
            win_host::pump_events(heartbeat);
        }
        // Off-Windows there is no message queue to service; keep the historical
        // sleep for the workspace's own portability tests.
        #[cfg(not(windows))]
        {
            std::thread::sleep(heartbeat);
        }
    }

    /// Installed physical memory, for `model_catalog::ram_verdict`.
    ///
    /// This was hardcoded `0`, and 0 is not a harmless placeholder here: every
    /// catalog entry has `min_ram_gb >= 1`, so `ram_verdict` rated all of them
    /// `Exceeds` and `offerable_by_ram` refused every model on Windows — the
    /// same defect `platform_linux` fixed when it replaced its own hardcoded 0.
    /// It is unreachable today only because Windows has no settings window to
    /// offer a download from; it would have become a "no models available"
    /// Setup pane the moment one existed.
    fn physical_memory_bytes(&self) -> u64 {
        #[cfg(windows)]
        {
            win_host::physical_memory_bytes()
        }
        // Off-Windows this type is only ever constructed by the workspace's own
        // portability tests; 0 keeps the historical value for them rather than
        // inventing a number for a host this adapter does not serve.
        #[cfg(not(windows))]
        {
            0
        }
    }

    fn open_url(&self, url: &str) -> Result<(), PlatformError> {
        #[cfg(windows)]
        {
            open_url_with(url, win_host::open_url)
        }
        #[cfg(not(windows))]
        {
            let _ = url;
            Err(WindowsAdapter::unsupported("open_url"))
        }
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

    fn confirm(&self, _prompt: &shell_flags::ConfirmPrompt<'_>) -> Result<bool, PlatformError> {
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
    fn native_url_launcher_receives_metacharacters_verbatim() {
        let url = "https://example.test/a path?x=1&y=2|3<4>(5)%25\"quoted\"";
        let seen = std::sync::Mutex::new(None);

        open_url_with(url, |wide| {
            assert_eq!(wide.last(), Some(&0));
            *seen.lock().unwrap() = Some(String::from_utf16(&wide[..wide.len() - 1]).unwrap());
            Ok(())
        })
        .unwrap();

        assert_eq!(seen.into_inner().unwrap().as_deref(), Some(url));
    }

    #[test]
    fn heartbeat_wait_timeout_never_reaches_infinite() {
        use std::time::Duration;

        // The ordinary case: the heartbeat passes through verbatim, in ms.
        assert_eq!(wait_timeout_ms(Duration::from_millis(50)), 50);
        // A zero heartbeat is a legal poll-and-drain, not a block.
        assert_eq!(wait_timeout_ms(Duration::ZERO), 0);
        // Sub-millisecond truncates down to that same poll, never wraps up.
        assert_eq!(wait_timeout_ms(Duration::from_micros(999)), 0);
        // u32::MAX ms *is* INFINITE to MsgWaitForMultipleObjectsEx, so the
        // clamp must land one below it — the boundary an obvious
        // `unwrap_or(u32::MAX)` would get wrong, turning a long tick into a
        // thread that never wakes again.
        assert_eq!(
            wait_timeout_ms(Duration::from_millis(u64::from(u32::MAX))),
            u32::MAX - 1
        );
        // And anything past the boundary saturates to that same finite ceiling
        // rather than overflowing the `u32::try_from` into INFINITE.
        assert_eq!(wait_timeout_ms(Duration::MAX), u32::MAX - 1);
    }

    #[test]
    fn open_url_with_rejects_interior_nul_without_launching() {
        let launched = std::sync::Mutex::new(false);
        let err = open_url_with("https://example.test/a\0b", |_| {
            *launched.lock().unwrap() = true;
            Ok(())
        })
        .unwrap_err();
        assert!(matches!(err, PlatformError::CannotComplete { .. }));
        assert!(!*launched.lock().unwrap());
    }

    #[test]
    fn scaffold_reports_windows_and_fails_closed() {
        let adapter = WindowsAdapter::new();
        // environment() is the one cheap, infallible method the scaffold answers.
        assert_eq!(adapter.environment().os, OperatingSystem::Windows);
        // The version probe is real now (`RtlGetVersion`), so what is assertable
        // depends on the host. On Windows: a dotted `major.minor.build` triple
        // of plain integers, majoring at least 10 — every Windows that can run
        // this (and every `windows-latest` runner, which is Server 2022/2025)
        // reports major 10, and the shimmed `GetVersionEx` answer this probe
        // exists to avoid would be 6.2, so the bound is what catches a
        // regression back to the Win32 call.
        let version = adapter.environment().version;
        #[cfg(windows)]
        {
            let parts: Vec<u32> = version
                .split('.')
                .map(|part| {
                    part.parse()
                        .unwrap_or_else(|_| panic!("version component {part:?} in {version:?}"))
                })
                .collect();
            assert_eq!(parts.len(), 3, "expected major.minor.build: {version:?}");
            assert!(
                parts[0] >= 10,
                "RtlGetVersion should report the true (unshimmed) major: {version:?}"
            );
            assert!(parts[2] > 0, "build number should be real: {version:?}");
        }
        // Off Windows the probe cannot run, so the adapter reports the shared
        // "could not tell" literal rather than a fabricated version.
        #[cfg(not(windows))]
        assert_eq!(version, UNKNOWN_VERSION);
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
        use platform::shell::ShellHost;
        use shell_flags::ConfirmPrompt;

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
        // Off Windows this is still the plain sleep, so the whole heartbeat
        // must elapse.
        #[cfg(not(windows))]
        assert!(start.elapsed() >= std::time::Duration::from_millis(5));
        // On Windows only the upper bound is assertable: the contract is "at
        // most `heartbeat`", and the message wait is *supposed* to return early
        // when input arrives, so a lower bound would pin the opposite of the
        // behaviour this exists to provide. Pin instead that the wait honours
        // its timeout rather than blocking indefinitely — the bug a missing
        // `dwMilliseconds`/`INFINITE` clamp would cause. Same shape as
        // `platform_macos`'s `pump_events_returns_within_heartbeat_scale`.
        #[cfg(windows)]
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "pump_events must return on its heartbeat timeout, not block"
        );
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
    use windows::Wdk::System::SystemServices::RtlGetVersion;
    use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL, WAIT_FAILED};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW,
        SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows::Win32::Security::{
        AclSizeInformation, CreateWellKnownSid, EqualSid, GetAce, GetAclInformation,
        GetSecurityDescriptorControl, GetSecurityDescriptorDacl, WinCreatorOwnerRightsSid,
        ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, CONTAINER_INHERIT_ACE,
        DACL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SECURITY_MAX_SID_SIZE, SE_DACL_PROTECTED,
    };
    use windows::Win32::System::Console::SetConsoleCtrlHandler;
    use windows::Win32::System::SystemInformation::{
        GlobalMemoryStatusEx, MEMORYSTATUSEX, OSVERSIONINFOW,
    };
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MsgWaitForMultipleObjectsEx, PeekMessageW, TranslateMessage, MSG,
        MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT, SW_SHOWNORMAL,
    };

    /// Host Windows version as `major.minor.build` (e.g. `10.0.26100`), via
    /// `RtlGetVersion`.
    ///
    /// `RtlGetVersion` is the probe to use, not `GetVersionEx`: the Win32 calls
    /// are subject to the compatibility shim that reports `6.2` to a process
    /// without a matching `supportedOS` manifest entry, so they would make a
    /// Windows 11 host answer "6.2.9200". The ntdll entry point is not shimmed
    /// and answers the true build. It is also cheap and effectively infallible,
    /// which is what `PlatformAdapter::environment` requires.
    ///
    /// `dwOSVersionInfoSize` MUST be set before the call — the same versioning
    /// contract as `GlobalMemoryStatusEx`'s `dwLength` below.
    ///
    /// Format: the build number, not the patch/UBR, is the third component,
    /// because it is what distinguishes Windows releases (10.0.19045 = Win10
    /// 22H2, 10.0.22631 = Win11 23H2). That keeps `Environment::version` a
    /// dotted triple exactly as `platform_macos::macos_version_string` produces
    /// (`major.minor.patch`), so the field stays comparable across adapters; a
    /// refused probe degrades to [`UNKNOWN_VERSION`], the same literal
    /// `platform_linux` uses when its own probe files are missing.
    pub fn os_version() -> String {
        let mut info = OSVERSIONINFOW {
            dwOSVersionInfoSize: u32::try_from(std::mem::size_of::<OSVERSIONINFOW>()).unwrap_or(0),
            ..Default::default()
        };
        if info.dwOSVersionInfoSize == 0 {
            return super::UNKNOWN_VERSION.to_string();
        }
        // SAFETY: `info` is a live, correctly sized, properly aligned
        // OSVERSIONINFOW owned by this frame, with `dwOSVersionInfoSize` set as
        // the API requires. The call only writes into that struct and borrows
        // nothing past its return.
        let status = unsafe { RtlGetVersion(&mut info) };
        if status.is_ok() {
            format!(
                "{}.{}.{}",
                info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber
            )
        } else {
            super::UNKNOWN_VERSION.to_string()
        }
    }

    /// One heartbeat tick of the host thread: drain the queue, block for at
    /// most `heartbeat` while waking on any input, then drain what woke it.
    ///
    /// A bare `std::thread::sleep` — which is what this was — is wrong for a UI
    /// thread. Windows delivers window messages, `WH_KEYBOARD_LL` hook
    /// callbacks and UIA event marshalling *by dispatching into the message
    /// queue of the thread that registered them*; a sleeping thread never pumps
    /// that queue, so none of those callbacks run. The OS notices: it silently
    /// unhooks a low-level keyboard hook whose thread does not answer within
    /// `LowLevelHooksTimeout` (the accept-key hook would just stop firing, with
    /// no error to report), and DWM ghosts a window whose thread has not
    /// pumped for ~5 s as "Not Responding". A sleep also spends the whole
    /// heartbeat on latency: a keystroke arriving 1 ms into a 50 ms sleep is
    /// not seen for 49 ms.
    ///
    /// `MsgWaitForMultipleObjectsEx` is the primitive that fixes all of that:
    /// it blocks (so the thread is not spinning) but returns the moment a
    /// `QS_ALLINPUT` message arrives, and otherwise returns on the
    /// `dwMilliseconds` timeout — so the caller's heartbeat is still the upper
    /// bound, which is exactly what `ShellHost::pump_events` promises ("at most
    /// `heartbeat`"). `MWMO_INPUTAVAILABLE` closes the race the plain
    /// `MsgWaitForMultipleObjects` has: a message already sitting in the queue
    /// that was seen but not removed does not re-signal, so without this flag
    /// the wait would block the full timeout with work already pending.
    ///
    /// The wait only *signals*; it never removes anything, so the drain around
    /// it is what actually dispatches. Draining first honours the trait's
    /// stated order ("drain queued native UI events, then service the main
    /// loop"); draining again afterwards is what handles whatever woke the
    /// wait, and costs one `PeekMessageW` on an empty queue when nothing did.
    pub fn pump_events(heartbeat: std::time::Duration) {
        drain_message_queue();
        // SAFETY: no wait handles are supplied (`None` => count 0, null array),
        // so the call waits only on this thread's own message queue and writes
        // through no pointer of ours.
        let wait = unsafe {
            MsgWaitForMultipleObjectsEx(
                None,
                super::wait_timeout_ms(heartbeat),
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            )
        };
        if wait == WAIT_FAILED {
            // A failed wait returns immediately. Without this the caller's
            // heartbeat loop would spin a core flat out; degrade to the old
            // sleep so a broken wait costs latency, not CPU.
            std::thread::sleep(heartbeat);
        }
        drain_message_queue();
    }

    /// Dispatch every message currently queued for this thread. A null window
    /// filter takes messages for any window this thread owns plus thread-posted
    /// messages, which is what a host that will later own an overlay window and
    /// a keyboard hook needs. `WM_QUIT` is drained like any other message: this
    /// crate posts none, and the run loop's stop signal is the console control
    /// handler's flag, not a quit message.
    fn drain_message_queue() {
        let mut msg = MSG::default();
        // SAFETY: `msg` is a live, properly aligned MSG owned by this frame.
        // `PeekMessageW` writes only into it; `TranslateMessage` and
        // `DispatchMessageW` only read it, and neither retains the pointer.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    /// Installed physical memory in bytes, via `GlobalMemoryStatusEx`.
    ///
    /// `dwLength` MUST be set to the struct size before the call — the API uses
    /// it for versioning and fails outright without it, which is the classic way
    /// this call silently returns nothing. Returns 0 on failure, the same
    /// fail-closed value the scaffold used, so a refused query degrades to "no
    /// model offered" rather than to a fabricated RAM figure that could offer a
    /// model the machine cannot load.
    ///
    /// `GlobalMemoryStatusEx` reports memory visible to the OS, which is
    /// slightly below installed RAM (firmware reservations). That direction is
    /// the safe one for a RAM-fit gate: it can only make the check stricter.
    pub fn physical_memory_bytes() -> u64 {
        let mut status = MEMORYSTATUSEX {
            dwLength: u32::try_from(std::mem::size_of::<MEMORYSTATUSEX>()).unwrap_or(0),
            ..Default::default()
        };
        if status.dwLength == 0 {
            return 0;
        }
        // SAFETY: `status` is a live, correctly sized, properly aligned
        // MEMORYSTATUSEX owned by this frame, and `dwLength` is set to its size
        // as the API requires. The call only writes into that struct and
        // borrows nothing past its return.
        match unsafe { GlobalMemoryStatusEx(&mut status) } {
            Ok(()) => status.ullTotalPhys,
            Err(_) => 0,
        }
    }

    pub fn open_url(url: &[u16]) -> Result<(), platform::PlatformError> {
        let operation = "open\0".encode_utf16().collect::<Vec<_>>();
        // SAFETY: both strings are NUL-terminated for the duration of the call;
        // null optional arguments ask the shell to use its defaults.
        let result = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(operation.as_ptr()),
                PCWSTR(url.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        let code = result.0 as isize;
        if code > 32 {
            Ok(())
        } else {
            Err(platform::PlatformError::CannotComplete {
                reason: format!("ShellExecuteW failed with code {code}"),
            })
        }
    }

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

    /// Verify the exact directory posture required for safe SQLite sidecar
    /// inheritance: protected DACL, one allow ACE, OWNER_RIGHTS SID, full
    /// control, and both object/container inheritance flags.
    pub fn is_owner_only_inherited_dir(path: &Path) -> std::io::Result<bool> {
        const FILE_ALL_ACCESS_MASK: u32 = 0x001F_01FF;
        const ACCESS_ALLOWED_ACE_TYPE_VALUE: u8 = 0;

        let target = wide(path);
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: all out-pointers are valid; `sd` owns the returned DACL and
        // remains live until LocalFree after every inspection completes.
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
            if err != ERROR_SUCCESS {
                return Err(std::io::Error::from_raw_os_error(err.0 as i32));
            }
            let result = (|| -> std::io::Result<bool> {
                if dacl.is_null() {
                    return Ok(false);
                }
                let mut control = 0u16;
                let mut revision = 0u32;
                GetSecurityDescriptorControl(sd, &mut control, &mut revision)
                    .map_err(std::io::Error::other)?;
                if control & SE_DACL_PROTECTED.0 == 0 {
                    return Ok(false);
                }
                let mut info = ACL_SIZE_INFORMATION::default();
                GetAclInformation(
                    dacl,
                    &mut info as *mut _ as *mut _,
                    std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                    AclSizeInformation,
                )
                .map_err(std::io::Error::other)?;
                if info.AceCount != 1 {
                    return Ok(false);
                }
                let mut raw_ace: *mut std::ffi::c_void = std::ptr::null_mut();
                GetAce(dacl, 0, &mut raw_ace).map_err(std::io::Error::other)?;
                let ace = &*(raw_ace as *const ACCESS_ALLOWED_ACE);
                let expected_flags = (OBJECT_INHERIT_ACE.0 | CONTAINER_INHERIT_ACE.0) as u8;
                if ace.Header.AceType != ACCESS_ALLOWED_ACE_TYPE_VALUE
                    || ace.Header.AceFlags != expected_flags
                    || ace.Mask != FILE_ALL_ACCESS_MASK
                {
                    return Ok(false);
                }
                let ace_sid = PSID(std::ptr::addr_of!(ace.SidStart) as *mut _);
                let mut owner_rights = [0u8; SECURITY_MAX_SID_SIZE as usize];
                let mut len = owner_rights.len() as u32;
                CreateWellKnownSid(
                    WinCreatorOwnerRightsSid,
                    None,
                    Some(PSID(owner_rights.as_mut_ptr() as *mut _)),
                    &mut len,
                )
                .map_err(std::io::Error::other)?;
                Ok(EqualSid(ace_sid, PSID(owner_rights.as_mut_ptr() as *mut _)).is_ok())
            })();
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
            AclSizeInformation, CreateWellKnownSid, EqualSid, GetAce, GetAclInformation,
            WinCreatorOwnerRightsSid, ACCESS_ALLOWED_ACE, ACL_SIZE_INFORMATION, PSID,
            SECURITY_MAX_SID_SIZE,
        };

        /// The single ACE must grant OWNER_RIGHTS (S-1-3-4) — a SID-flip
        /// mutation of OWNER_ONLY_SDDL (e.g. `;;;WD` = Everyone) keeps
        /// AceCount == 1 and would otherwise ship world-full-access green.
        fn first_ace_is_owner_rights(path: &Path) -> bool {
            let target = wide(path);
            let mut dacl: *mut ACL = std::ptr::null_mut();
            let mut sd = PSECURITY_DESCRIPTOR::default();
            // SAFETY: out-pointers valid; sd freed below; ace points into dacl
            // which lives inside sd's allocation until the free.
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
                let mut ace: *mut std::ffi::c_void = std::ptr::null_mut();
                GetAce(dacl, 0, &mut ace).expect("GetAce");
                let ace = ace as *mut ACCESS_ALLOWED_ACE;
                let ace_sid = PSID(std::ptr::addr_of_mut!((*ace).SidStart) as *mut _);
                let mut owner_rights = [0u8; SECURITY_MAX_SID_SIZE as usize];
                let mut len = owner_rights.len() as u32;
                CreateWellKnownSid(
                    WinCreatorOwnerRightsSid,
                    None,
                    Some(PSID(owner_rights.as_mut_ptr() as *mut _)),
                    &mut len,
                )
                .expect("CreateWellKnownSid");
                let equal = EqualSid(ace_sid, PSID(owner_rights.as_mut_ptr() as *mut _)).is_ok();
                LocalFree(Some(HLOCAL(sd.0)));
                equal
            }
        }

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
            assert!(
                first_ace_is_owner_rights(&dir),
                "the ACE must grant OWNER_RIGHTS, not a world/group SID"
            );

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
        fn harden_owner_only_accepts_a_plain_file_target() {
            // config.rs hard-fails (`?`) on this path and the memory per-file
            // backstop calls it on the db/sidecars: an OICI ACE applied to a
            // leaf object must succeed, not error.
            let dir = std::env::temp_dir()
                .join(format!("compme-harden-file-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join("leaf.txt");
            std::fs::write(&file, b"x").unwrap();
            harden_owner_only(&file).expect("harden plain file");
            assert_eq!(
                ace_count(&file),
                1,
                "file DACL must be the single owner ACE"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn owner_only_inherited_directory_predicate_matches_exact_hardened_posture() {
            let dir = std::env::temp_dir()
                .join(format!("compme-owner-posture-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();

            assert!(
                !is_owner_only_inherited_dir(&dir).expect("read normal temp DACL"),
                "a normal permissive temp directory must not satisfy the exact posture"
            );
            harden_owner_only(&dir).expect("harden directory");
            assert!(
                is_owner_only_inherited_dir(&dir).expect("read hardened DACL"),
                "the hardener output must satisfy the exact inherited posture"
            );
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
