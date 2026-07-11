//! Linux platform adapter — SCAFFOLD (ROADMAP Tier 1.1).
//!
//! Implements the [`platform::PlatformAdapter`] contract so the cross-platform
//! structure exists and CI can gate it, but the real Linux API integration is
//! **not yet built** — it requires a Linux build+test environment (this scaffold
//! was authored on a macOS-only host). Every method is a fail-closed stub
//! returning [`PlatformError::UnsupportedField`] (IO/subscribe) or a safe empty
//! value, so wiring this adapter in is inert, never a crash. Each method's doc
//! names the Linux API its real implementation will use.

use platform::{
    AcceptCallback, AcceptSubscription, AppId, Capabilities, CaretCallback, Environment,
    FieldHandle, FocusCallback, InsertStrategy, Inserted, OperatingSystem, PlatformAdapter,
    PlatformError, ScreenRect, Subscription, TextContext,
};

/// Linux implementation of [`PlatformAdapter`] — scaffold (see module docs).
/// Implementation map for the real adapter (built on a Linux host):
/// - focus / caret events → AT-SPI2 (`atspi`, accessibility over D-Bus)
/// - capabilities / read_context / caret_rect → AT-SPI2 Text/EditableText interfaces
/// - subscribe_accept → AT-SPI2 device/key listeners (X11), or a compositor path on Wayland
/// - insert / insert_replacing → AT-SPI2 EditableText, else XTEST / `wtype` synthetic keys
///   (Wayland restricts synthetic injection — IBus IME commit is the fallback)
/// - overlay → an override-redirect X11 window, or a layer-shell surface on Wayland
#[derive(Debug, Default)]
pub struct LinuxAdapter;

impl LinuxAdapter {
    pub fn new() -> Self {
        Self
    }

    /// The error every not-yet-implemented method returns. Fail-closed: the host
    /// treats any error as "no suggestion this turn" and leaves the field
    /// untouched, so an unwired Linux adapter is inert, never harmful.
    fn unsupported(method: &str) -> PlatformError {
        PlatformError::UnsupportedField {
            reason: format!("platform_linux::{method} not yet implemented (Tier 1.1 scaffold)"),
        }
    }
}

impl PlatformAdapter for LinuxAdapter {
    /// Real impl: `/etc/os-release` + `uname`. Cheap + infallible per the contract.
    fn environment(&self) -> Environment {
        Environment {
            os: OperatingSystem::Linux,
            version: "unknown".to_string(),
        }
    }

    /// Real impl: AT-SPI2 focus-changed event subscription (D-Bus).
    fn subscribe_focus(&self, _cb: FocusCallback) -> Result<Subscription, PlatformError> {
        Err(Self::unsupported("subscribe_focus"))
    }

    /// Real impl: AT-SPI2 text-caret-moved / bounds-changed events.
    fn subscribe_caret(&self, _cb: CaretCallback) -> Result<Subscription, PlatformError> {
        Err(Self::unsupported("subscribe_caret"))
    }

    /// Real impl: AT-SPI2 device/key listener (X11); a compositor shortcut on Wayland.
    fn subscribe_accept(&self, _cb: AcceptCallback) -> Result<AcceptSubscription, PlatformError> {
        Err(Self::unsupported("subscribe_accept"))
    }

    /// Real impl: AT-SPI2 active-window application name.
    fn front_app(&self) -> Option<AppId> {
        None
    }

    /// Real impl: AT-SPI2 Text/EditableText interface probe + role/state checks.
    fn capabilities(&self, _field: &FieldHandle) -> Result<Capabilities, PlatformError> {
        Err(Self::unsupported("capabilities"))
    }

    /// Real impl: AT-SPI2 Text interface range around the caret.
    fn read_context(&self, _field: &FieldHandle) -> Result<TextContext, PlatformError> {
        Err(Self::unsupported("read_context"))
    }

    /// Real impl: AT-SPI2 character-extents bounding rectangle of the caret.
    fn caret_rect(&self, _field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
        Err(Self::unsupported("caret_rect"))
    }

    /// Real impl: AT-SPI2 EditableText insert, else XTEST / `wtype` synthetic typing.
    fn insert(
        &self,
        _field: &FieldHandle,
        _text: &str,
        _strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        Err(Self::unsupported("insert"))
    }

    /// Real impl: AT-SPI2 range-replace, else backspace×N + XTEST/`wtype` typing.
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

/// Linux implementation of `platform::shell::ShellHost` — fail-closed scaffold.
/// Future real impls: `/proc/meminfo` memory probe, libsecret key storage,
/// zenity/GTK confirmation dialogs, XDG autostart `.desktop`, and desktop portal
/// permission/settings hooks where available.
#[derive(Debug, Default)]
pub struct LinuxShellHost;

impl LinuxShellHost {
    pub fn new() -> Self {
        Self
    }
}

/// Spawn `command`, briefly poll for an immediate exit so a fast-failing
/// launcher (missing handler, bad URL) can be reported fail-closed, then hand
/// any still-running child to a reaper thread. Returns `Ok(Some(status))` when
/// the child exited within the poll window, `Ok(None)` when the reaper owns it;
/// `on_reaped` fires exactly once with the final status either way.
fn spawn_and_reap_with(
    command: &mut std::process::Command,
    on_reaped: impl FnOnce(std::process::ExitStatus) + Send + 'static,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let mut child = command.spawn()?;
    // Keep the window well under the caller-blocking budget pinned by
    // `url_launcher_reaps_child_without_blocking_the_caller` (100ms).
    for _ in 0..10 {
        if let Some(status) = child.try_wait()? {
            on_reaped(status);
            return Ok(Some(status));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    std::thread::Builder::new()
        .name("compme-url-reaper".into())
        .spawn(move || {
            if let Ok(status) = child.wait() {
                on_reaped(status);
            }
        })?;
    Ok(None)
}

impl platform::shell::ShellHost for LinuxShellHost {
    fn pump_events(&self, heartbeat: std::time::Duration) {
        std::thread::sleep(heartbeat);
    }

    fn physical_memory_bytes(&self) -> u64 {
        0
    }

    fn open_url(&self, url: &str) -> Result<(), PlatformError> {
        // Fail closed on an immediate launcher failure, matching the macOS
        // (NSWorkspace bool) and Windows (ShellExecuteW code) launch checks; a
        // child that outlives the poll window is best-effort by construction.
        match spawn_and_reap_with(std::process::Command::new("xdg-open").arg(url), |_| {}) {
            Ok(Some(status)) if !status.success() => Err(PlatformError::CannotComplete {
                reason: format!("xdg-open {url}: exited with {status}"),
            }),
            Ok(_) => Ok(()),
            Err(e) => Err(PlatformError::CannotComplete {
                reason: format!("xdg-open {url}: {e}"),
            }),
        }
    }

    fn open_permission_settings(&self) -> Result<(), PlatformError> {
        Err(LinuxAdapter::unsupported("open_permission_settings"))
    }

    fn reveal_file(&self, _path: &std::path::Path) -> Result<(), PlatformError> {
        Err(LinuxAdapter::unsupported("reveal_file"))
    }

    fn set_launch_at_login(&self, _enabled: bool) -> Result<(), PlatformError> {
        Err(LinuxAdapter::unsupported("set_launch_at_login"))
    }

    fn confirm(&self, _prompt: &platform::shell::ConfirmPrompt<'_>) -> Result<bool, PlatformError> {
        Err(LinuxAdapter::unsupported("confirm"))
    }

    fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError> {
        Err(LinuxAdapter::unsupported("load_or_create_memory_key"))
    }
}

/// Linux ghost overlay scaffold. Future real impl: wlr-layer-shell on Wayland
/// or an override-redirect X11 window.
#[derive(Debug, Default)]
pub struct LinuxOverlayPresenter;

impl LinuxOverlayPresenter {
    pub fn new() -> Self {
        Self
    }
}

impl platform::OverlayPresenter for LinuxOverlayPresenter {
    fn show_ghost(&mut self, _anchor: ScreenRect, _text: &str) -> Result<(), PlatformError> {
        Err(LinuxAdapter::unsupported("show_ghost"))
    }

    fn show_correction(
        &mut self,
        _rect: ScreenRect,
        _suggestion: &str,
    ) -> Result<(), PlatformError> {
        Err(LinuxAdapter::unsupported("show_correction"))
    }

    fn update_ghost(&mut self, _text: &str) -> Result<(), PlatformError> {
        Err(LinuxAdapter::unsupported("update_ghost"))
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
    fn url_launcher_reaps_child_without_blocking_the_caller() {
        let (reaped_tx, reaped_rx) = std::sync::mpsc::channel();
        let latch = std::env::temp_dir().join(format!(
            "compme-linux-url-reaper-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut command = std::process::Command::new("sh");
        command
            .args(["-c", "while [ ! -e \"$1\" ]; do sleep 0.01; done", "sh"])
            .arg(&latch);

        let early = spawn_and_reap_with(&mut command, move |status| {
            reaped_tx.send(status).unwrap();
        })
        .unwrap();

        assert!(early.is_none(), "long-lived child must go to the reaper");
        assert!(matches!(
            reaped_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        std::fs::write(&latch, []).unwrap();
        assert!(reaped_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .success());
        std::fs::remove_file(latch).unwrap();
    }

    #[test]
    fn url_launcher_reports_immediate_child_failure() {
        // A launcher that exits non-zero right away (missing handler, bad URL)
        // must surface within the poll window so open_url can fail closed
        // instead of silently discarding the exit status.
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "exit 3"]);
        let status = spawn_and_reap_with(&mut command, |_| {})
            .unwrap()
            .expect("fast-failing child detected within the poll window");
        assert!(!status.success());
    }

    #[test]
    fn scaffold_reports_linux_and_fails_closed() {
        let adapter = LinuxAdapter::new();
        // environment() is the one cheap, infallible method the scaffold answers.
        assert_eq!(adapter.environment().os, OperatingSystem::Linux);
        // The scaffold has no real version probe yet: it reports the fixed
        // "unknown" version. Pin it so the real `/etc/os-release` + `uname` impl
        // visibly replaces the placeholder.
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
        let adapter = LinuxAdapter::new();
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
        // half-landed (e.g. an EditableText/XTEST branch returning Ok before the
        // others) must not slip past the scaffold gate, so pin BOTH insert and
        // insert_replacing as UnsupportedField across ALL strategies. If a variant
        // is added to InsertStrategy, this match goes non-exhaustive and forces an
        // update.
        let adapter = LinuxAdapter::new();
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
        let adapter = LinuxAdapter::new();
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
        // ("platform_linux::<method> not yet implemented (Tier 1.1 scaffold)")
        // across a representative spread — a subscribe, a capability probe, and an
        // insert — so a future refactor of `unsupported()` can't drop the method
        // name (or the crate prefix) without breaking a test.
        let adapter = LinuxAdapter::new();
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
            reason.contains("platform_linux::"),
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
            reason, "platform_linux::capabilities not yet implemented (Tier 1.1 scaffold)",
            "full reason string format pinned"
        );

        let caret_cb: CaretCallback = Arc::new(|_field, _rect| {});
        let Err(PlatformError::UnsupportedField { reason }) = adapter.subscribe_caret(caret_cb)
        else {
            panic!("subscribe_caret should fail closed with UnsupportedField");
        };
        assert!(
            reason.contains("platform_linux::") && reason.contains("subscribe_caret"),
            "reason should name crate + `subscribe_caret`: {reason:?}"
        );

        let Err(PlatformError::UnsupportedField { reason }) =
            adapter.insert_replacing(&field, "x", 1, InsertStrategy::None)
        else {
            panic!("insert_replacing should fail closed with UnsupportedField");
        };
        assert!(
            reason.contains("platform_linux::") && reason.contains("insert_replacing"),
            "reason should name crate + `insert_replacing`: {reason:?}"
        );
    }

    #[test]
    fn shell_host_is_fail_closed() {
        use platform::shell::{ConfirmPrompt, ShellHost};

        let h = LinuxShellHost::new();
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

        let mut o = LinuxOverlayPresenter::new();
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
                if reason.contains("platform_linux::show_correction")
        ));
        assert!(o.update_ghost("g").is_err());
        o.hide().expect("hide is contractually idempotent-success");
        o.hide().expect("second hide too");
    }
}
