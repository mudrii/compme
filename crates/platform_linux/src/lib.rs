//! Linux platform adapter — the AT-SPI2 field and event surfaces are real
//! (ROADMAP Phase 2.1/2.2/2.4); the X11 accept tap and overlay are still
//! scaffold (Tier 1.1), as are the session-dependent shell services.
//!
//! Implements the [`platform::PlatformAdapter`] contract so the cross-platform
//! structure exists and CI can gate it. Real on Linux with an accessibility
//! session: the AT-SPI2 read, insert, and focus/caret event surfaces (see
//! `atspi_live` and `atspi_events`) and the ghost/correction overlay (an
//! override-redirect X11 window — see `x11_overlay`). The accept tap is the one
//! remaining surface **not yet built**: it is a fail-closed stub returning
//! [`PlatformError::UnsupportedField`], so wiring this adapter in is inert, never
//! a crash, and its doc names the Linux API its real implementation will use.
//!
//! Real today, because they need neither a display nor an accessibility bus and
//! so are verifiable on a headless Linux host: `environment` (distro + kernel),
//! `physical_memory_bytes` (`/proc/meminfo`), `set_launch_at_login` (XDG
//! autostart entry), and `open_url` (`xdg-open`). Their parsing and path rules
//! are factored into pure functions over file contents and env values, because
//! this crate is also compiled and tested on the macOS development hosts, where
//! `/proc` and `/etc/os-release` do not exist.
//!
//! Real too, but session-dependent (ROADMAP Phase 2.6): the memory-key store
//! (`keyring` + [`memory_key`]), the modal confirm ([`confirm`]), and
//! file-manager reveal ([`reveal`]). Each talks to a desktop service that a
//! headless host does not have, so each fails closed there — that is the half a
//! headless host can prove, and it is proven rather than assumed. Their
//! decision-making (D-Bus lookup classification, dialog argv and exit codes, path
//! → URI) is again pure and tested on every host.

use platform::{
    AcceptCallback, AcceptSubscription, AppId, Capabilities, CaretCallback, Environment,
    FieldHandle, FocusCallback, InsertStrategy, Inserted, OperatingSystem, PlatformAdapter,
    PlatformError, ScreenRect, Subscription, TextContext,
};
use std::path::{Path, PathBuf};

pub mod atspi_caps;
pub mod atspi_event_map;
/// Live AT-SPI2 focus/caret event delivery. Linux-only for the same reason as
/// `atspi_live`; its pure half lives in `atspi_event_map`, which builds everywhere.
#[cfg(target_os = "linux")]
pub mod atspi_events;
pub mod atspi_ids;
/// Live AT-SPI2 read path. Linux-only: it needs the accessibility bus, and the
/// `atspi` dependency is target-gated, so the module cannot exist elsewhere.
#[cfg(target_os = "linux")]
pub mod atspi_live;
/// `zenity`/`kdialog` modal confirmation: argv construction and exit-code
/// interpretation are pure, so they are tested on every host.
pub mod confirm;
/// Secret Service (`org.freedesktop.secrets`) key transport — the Linux
/// counterpart of `platform_macos::keychain`. Linux-only: it needs the session
/// bus, and `zbus` is target-gated, so the module cannot exist elsewhere.
#[cfg(target_os = "linux")]
pub mod keyring;
/// The memory-store key's load-or-create contract (host-independent).
pub mod memory_key;
/// File-manager reveal: `org.freedesktop.FileManager1` with an `xdg-open`
/// fallback, over pure path arithmetic.
pub mod reveal;
/// Font discovery for the overlay. Compiled everywhere: the ranking, the
/// search-path rules, and the directory scan are pure enough to test on the
/// macOS and Windows lanes.
pub mod overlay_font;
/// Overlay placement geometry. Compiled everywhere for the same reason — this is
/// where overlay bugs actually live, and headless pixel assertions cannot see
/// them.
pub mod overlay_geometry;
/// Live override-redirect X11 overlay. Linux-only: `x11rb` is target-gated.
#[cfg(target_os = "linux")]
pub mod x11_overlay;

/// Live AT-SPI2 integration tests. In a sibling file (a `#[path]` module) rather
/// than inline, matching how `run_loop` and `platform_macos` keep their tests —
/// see the repo brief's "Where tests live".
#[cfg(all(test, target_os = "linux"))]
#[path = "atspi_live_tests.rs"]
mod atspi_live_tests;

/// `os-release(5)`: the distro identity file present on every distro compme
/// targets.
const OS_RELEASE_PATH: &str = "/etc/os-release";
/// The kernel release, i.e. what `uname -r` prints — read as a file so the
/// probe needs no subprocess.
const KERNEL_RELEASE_PATH: &str = "/proc/sys/kernel/osrelease";
const MEMINFO_PATH: &str = "/proc/meminfo";
/// XDG autostart entry filename. One entry per application, so it is fixed.
const AUTOSTART_ENTRY: &str = "compme.desktop";

/// Linux implementation of [`PlatformAdapter`] — partly real, partly scaffold (see
/// module docs). Implementation map:
/// - focus / caret events → AT-SPI2 `object:state-changed:focused` /
///   `object:text-caret-moved` signals over D-Bus (built: `atspi_events`)
/// - capabilities / read_context / caret_rect → AT-SPI2 Text/EditableText interfaces
/// - subscribe_accept → X11 `XGrabKey` with `GrabModeSync` + `XAllowEvents`
///   (ROADMAP Phase 2.3's resolved design), or a compositor path on Wayland
/// - insert / insert_replacing → AT-SPI2 EditableText, else XTEST / `wtype` synthetic keys
///   (Wayland restricts synthetic injection — IBus IME commit is the fallback)
/// - overlay → an override-redirect X11 window, or a layer-shell surface on Wayland
#[derive(Debug, Default)]
pub struct LinuxAdapter {
    /// The accessibility-bus session, when one was opened. `None` keeps every
    /// AT-SPI-backed method fail-closed exactly as the pre-2.1 scaffold was.
    #[cfg(target_os = "linux")]
    session: Option<atspi_live::AtspiSession>,
}

impl LinuxAdapter {
    /// An inert adapter: no accessibility bus, so every field operation still
    /// fails closed.
    ///
    /// Opening the bus is deliberately *not* done here. `org.a11y.Bus` is
    /// D-Bus-activatable, so a host with a session bus but no accessibility
    /// service makes the lookup wait out the default 25-second method timeout —
    /// in a constructor that unit tests call dozens of times. Callers that want
    /// the live read path ask for it explicitly with `with_accessibility` (a
    /// plain code span, not an intra-doc link: that method is Linux-only, so the
    /// link would be unresolvable when this crate is documented on macOS, where
    /// the workspace `cargo doc` runs with `-D warnings`). The app wiring still uses
    /// `new()`: the read, insert, and event paths are live, but the accept tap
    /// (Phase 2.3) and the overlay (2.5) are not, so nothing yet drives the adapter
    /// end to end.
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to open the accessibility bus, returning an adapter that uses it when
    /// available and an inert one when not. Never fails: a host without
    /// accessibility is a supported configuration, not an error.
    #[cfg(target_os = "linux")]
    pub fn with_accessibility() -> Self {
        Self {
            session: atspi_live::AtspiSession::open().ok(),
        }
    }

    /// The open accessibility session, or the fail-closed error every
    /// AT-SPI-backed method returns without one.
    #[cfg(target_os = "linux")]
    fn session(&self, method: &str) -> Result<&atspi_live::AtspiSession, PlatformError> {
        self.session
            .as_ref()
            .ok_or_else(|| Self::unsupported(method))
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
    /// Distro name plus kernel release, from `/etc/os-release` and
    /// `/proc/sys/kernel/osrelease`. Cheap and infallible per the contract:
    /// whichever file is unreadable is simply dropped from the string, and
    /// `"unknown"` survives only when neither can be read.
    fn environment(&self) -> Environment {
        Environment {
            os: OperatingSystem::Linux,
            version: linux_version(
                std::fs::read_to_string(OS_RELEASE_PATH).ok().as_deref(),
                std::fs::read_to_string(KERNEL_RELEASE_PATH).ok().as_deref(),
            ),
        }
    }

    /// AT-SPI2 `object:state-changed:focused` signals, delivered by
    /// [`atspi_events::subscribe_focus`]. Each subscription gets its own bus
    /// connection and worker threads, so dropping the `Subscription` closes exactly
    /// that connection and stops exactly that delivery.
    #[cfg(target_os = "linux")]
    fn subscribe_focus(&self, cb: FocusCallback) -> Result<Subscription, PlatformError> {
        // Gate on the adapter's own session so a host without accessibility fails
        // closed here rather than paying the bus-activation timeout per subscribe.
        self.session("subscribe_focus")?;
        atspi_events::subscribe_focus(cb)
    }

    /// Real impl: AT-SPI2 focus-changed event subscription (D-Bus).
    #[cfg(not(target_os = "linux"))]
    fn subscribe_focus(&self, _cb: FocusCallback) -> Result<Subscription, PlatformError> {
        Err(Self::unsupported("subscribe_focus"))
    }

    /// AT-SPI2 `object:text-caret-moved` signals plus the `caret_rect` geometry
    /// probe, coalesced. See [`atspi_events::subscribe_caret`].
    #[cfg(target_os = "linux")]
    fn subscribe_caret(&self, cb: CaretCallback) -> Result<Subscription, PlatformError> {
        self.session("subscribe_caret")?;
        atspi_events::subscribe_caret(cb)
    }

    /// Real impl: AT-SPI2 text-caret-moved / bounds-changed events.
    #[cfg(not(target_os = "linux"))]
    fn subscribe_caret(&self, _cb: CaretCallback) -> Result<Subscription, PlatformError> {
        Err(Self::unsupported("subscribe_caret"))
    }

    /// Real impl: AT-SPI2 device/key listener (X11); a compositor shortcut on Wayland.
    fn subscribe_accept(&self, _cb: AcceptCallback) -> Result<AcceptSubscription, PlatformError> {
        Err(Self::unsupported("subscribe_accept"))
    }

    /// The application owning the focused accessible. `None` without a session,
    /// which is also the honest answer when nothing is focused.
    #[cfg(target_os = "linux")]
    fn front_app(&self) -> Option<AppId> {
        self.session.as_ref()?.focused_app_name()
    }

    /// Real impl: AT-SPI2 active-window application name.
    #[cfg(not(target_os = "linux"))]
    fn front_app(&self) -> Option<AppId> {
        None
    }

    /// AT-SPI2 interface/state/role probe, mapped by
    /// [`atspi_caps::capabilities_from`].
    #[cfg(target_os = "linux")]
    fn capabilities(&self, field: &FieldHandle) -> Result<Capabilities, PlatformError> {
        let session = self.session("capabilities")?;
        let id = atspi_ids::ElementId::decode(&field.element_id).ok_or_else(|| {
            PlatformError::UnsupportedField {
                reason: format!("platform_linux: malformed element id: {}", field.element_id),
            }
        })?;
        session.capabilities(&id)
    }

    /// Real impl: AT-SPI2 Text/EditableText interface probe + role/state checks.
    #[cfg(not(target_os = "linux"))]
    fn capabilities(&self, _field: &FieldHandle) -> Result<Capabilities, PlatformError> {
        Err(Self::unsupported("capabilities"))
    }

    /// AT-SPI2 `Text` around the caret, in Unicode scalars.
    #[cfg(target_os = "linux")]
    fn read_context(&self, field: &FieldHandle) -> Result<TextContext, PlatformError> {
        self.session("read_context")?.read_context(field)
    }

    /// Real impl: AT-SPI2 Text interface range around the caret.
    #[cfg(not(target_os = "linux"))]
    fn read_context(&self, _field: &FieldHandle) -> Result<TextContext, PlatformError> {
        Err(Self::unsupported("read_context"))
    }

    /// AT-SPI2 per-character screen extents at the caret.
    #[cfg(target_os = "linux")]
    fn caret_rect(&self, field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
        self.session("caret_rect")?.caret_rect(field)
    }

    /// Real impl: AT-SPI2 character-extents bounding rectangle of the caret.
    #[cfg(not(target_os = "linux"))]
    fn caret_rect(&self, _field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
        Err(Self::unsupported("caret_rect"))
    }

    /// AT-SPI2 `EditableText.InsertText` at the caret. Only the atomic strategy is
    /// honored: the synthetic-key fallback (XTEST) is not built, and accepting a
    /// non-atomic request here would type into a field the engine believes it set.
    #[cfg(target_os = "linux")]
    fn insert(
        &self,
        field: &FieldHandle,
        text: &str,
        strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        if !strategy.supports_atomic_range_replace() {
            return Err(Self::unsupported("insert (non-atomic strategy)"));
        }
        self.session("insert")?.insert(field, text)
    }

    /// Real impl: AT-SPI2 EditableText insert, else XTEST / `wtype` synthetic typing.
    #[cfg(not(target_os = "linux"))]
    fn insert(
        &self,
        _field: &FieldHandle,
        _text: &str,
        _strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        Err(Self::unsupported("insert"))
    }

    /// Left-of-caret replacement stays **fail-closed on Linux**, deliberately.
    ///
    /// `replace_left` counts scalars to delete before the caret, which AT-SPI can
    /// only express as DeleteText followed by InsertText — two round trips, so a
    /// failure between them leaves the user's field truncated. The atomic path is
    /// `insert_replacing_range`, which carries an explicit range and expected text
    /// and swaps the whole value in one call; the engine already routes
    /// replacements through it for atomic strategies.
    fn insert_replacing(
        &self,
        _field: &FieldHandle,
        _text: &str,
        _replace_left: usize,
        _strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        Err(Self::unsupported(
            "insert_replacing (use insert_replacing_range: AT-SPI cannot delete-then-insert atomically)",
        ))
    }

    /// Exact range replacement, guarded by `expected_text` and verified by
    /// readback. See `atspi_live::AtspiSession::insert_replacing_range`.
    #[cfg(target_os = "linux")]
    fn insert_replacing_range(
        &self,
        field: &FieldHandle,
        expected_text: &str,
        text: &str,
        range: platform::CorrectionRange,
        strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        self.session("insert_replacing_range")?
            .insert_replacing_range(field, expected_text, text, range, strategy)
    }
}

/// Compose the reported host version from the two probe files. Pure over their
/// contents so every branch — including "neither file exists" — is testable on
/// the macOS hosts that also build this crate.
fn linux_version(os_release: Option<&str>, kernel_release: Option<&str>) -> String {
    let distro = os_release.and_then(distro_name);
    let kernel = kernel_release.map(str::trim).filter(|k| !k.is_empty());
    match (distro, kernel) {
        (Some(distro), Some(kernel)) => format!("{distro} (kernel {kernel})"),
        (Some(distro), None) => distro,
        (None, Some(kernel)) => format!("kernel {kernel}"),
        (None, None) => "unknown".to_string(),
    }
}

/// `PRETTY_NAME` per `os-release(5)`, else `NAME` joined with `VERSION_ID` —
/// the same fallback order `systemd` documents for display purposes.
fn distro_name(os_release: &str) -> Option<String> {
    if let Some(pretty) = os_release_field(os_release, "PRETTY_NAME") {
        return Some(pretty);
    }
    let name = os_release_field(os_release, "NAME")?;
    Some(match os_release_field(os_release, "VERSION_ID") {
        Some(version) => format!("{name} {version}"),
        None => name,
    })
}

/// One `KEY=VALUE` lookup in `os-release(5)` format: `#` comment lines are
/// skipped, the value may be single- or double-quoted, and the first occurrence
/// of the key wins. An empty value reads as absent so a blank `PRETTY_NAME=""`
/// falls through to the `NAME` path rather than reporting an empty version.
fn os_release_field(os_release: &str, key: &str) -> Option<String> {
    os_release
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let (found, value) = line.split_once('=')?;
            (found.trim_end() == key).then(|| unquote(value.trim()).to_string())
        })
        .filter(|value| !value.is_empty())
}

/// Strip one layer of matching quotes — the only quoting `os-release(5)` values
/// use in practice.
fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// `MemTotal` from `/proc/meminfo`, in bytes. The kernel prints kibibytes
/// labelled `kB`; any other unit is refused rather than guessed at, and the
/// scale-up is checked so a corrupt value cannot wrap.
fn meminfo_total_bytes(meminfo: &str) -> Option<u64> {
    let value = meminfo
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("MemTotal:"))?;
    let mut fields = value.split_whitespace();
    let kib: u64 = fields.next()?.parse().ok()?;
    match fields.next() {
        Some(unit) if !unit.eq_ignore_ascii_case("kB") => None,
        _ => kib.checked_mul(1024),
    }
}

/// The XDG autostart directory: `$XDG_CONFIG_HOME/autostart`, else
/// `$HOME/.config/autostart`. Empty values are treated as unset, matching
/// `app`'s config-path resolution; a relative `XDG_CONFIG_HOME` is ignored too,
/// because the basedir spec requires an absolute path and a relative base would
/// drop the entry under the process cwd.
///
/// "Absolute" is tested as a leading `/` rather than with `Path::is_absolute`,
/// which answers for the host that *compiled* the code: this crate is also built
/// on Windows CI, where `is_absolute("/home/u")` is false. The rule being encoded
/// is POSIX, so it must not vary by build host.
fn autostart_dir(xdg_config_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let base = match xdg_config_home.filter(|v| !v.is_empty()) {
        Some(xdg) if xdg.starts_with('/') => PathBuf::from(xdg),
        _ => Path::new(home.filter(|v| !v.is_empty())?).join(".config"),
    };
    Some(base.join("autostart"))
}

/// The `Exec=` value for the autostart entry. Quoted unconditionally: a quoted
/// string is valid for every path, so there is no "does this one need quoting"
/// branch to get wrong. Per the desktop-entry spec, `"`, `\`, `` ` `` and `$`
/// are backslash-escaped inside a quoted argument, and a literal `%` is written
/// `%%` so it is not read as a field code.
fn desktop_exec_value(exec: &Path) -> String {
    let mut quoted = String::from("\"");
    for ch in exec.to_string_lossy().chars() {
        match ch {
            '"' | '\\' | '`' | '$' => {
                quoted.push('\\');
                quoted.push(ch);
            }
            '%' => quoted.push_str("%%"),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

/// The autostart desktop entry. `X-GNOME-Autostart-enabled` is the key the GNOME
/// startup UIs toggle; other desktops ignore it. `NoDisplay=true` keeps compme
/// out of application menus — the tray is its entry point.
fn autostart_desktop_entry(exec: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=compme\n\
         Comment=Inline text completion\n\
         Exec={}\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n",
        desktop_exec_value(exec)
    )
}

/// Create or remove the autostart entry under `dir`. Disabling when no entry
/// exists succeeds — the requested state already holds — while every other IO
/// error is returned, so the settings toggle restores its previous visible state
/// instead of persisting a value the session will not honor.
fn apply_autostart(dir: &Path, enabled: bool, exec: &Path) -> std::io::Result<()> {
    let entry = dir.join(AUTOSTART_ENTRY);
    if !enabled {
        return match std::fs::remove_file(&entry) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => result,
        };
    }
    std::fs::create_dir_all(dir)?;
    // Write a sibling temp file and rename, so an interrupted write cannot leave
    // a truncated entry that the session silently fails to parse at login.
    let temp = dir.join(format!("{AUTOSTART_ENTRY}.{}.tmp", std::process::id()));
    std::fs::write(&temp, autostart_desktop_entry(exec))?;
    std::fs::rename(&temp, &entry).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

/// Linux implementation of `platform::shell::ShellHost`.
///
/// Real, and needing no desktop session: the `/proc/meminfo` memory probe, the
/// XDG autostart entry, and the `xdg-open` URL launcher.
///
/// Real, and session-dependent (ROADMAP Phase 2.6) — each is built here and each
/// fails closed when its service is absent, which is the only half a headless host
/// can prove:
/// - `load_or_create_memory_key` — Secret Service over D-Bus
///   (`keyring`, contract in [`memory_key`]). No key store, or a locked
///   keyring, is an error; there is no plaintext fallback.
/// - `confirm` — `zenity`, then `kdialog` ([`confirm`]). `Ok(true)` only on an
///   explicit confirm click; neither helper present is an error.
/// - `reveal_file` — `org.freedesktop.FileManager1.ShowItems`, else `xdg-open` on
///   the containing directory ([`reveal`]).
///
/// Still fail-closed by design: `open_permission_settings` (Linux has no TCC-style
/// pane to open) and the tray, which needs a StatusNotifierItem host.
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
    let early = poll_for_immediate_exit_with(|| child.try_wait(), std::thread::sleep)?;
    if let Some(status) = early {
        on_reaped(status);
        return Ok(Some(status));
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

/// Hand `target` (a URL or a directory) to the desktop's default handler.
///
/// Fails closed on an immediate launcher failure, matching the macOS (NSWorkspace
/// bool) and Windows (ShellExecuteW code) launch checks; a child that outlives the
/// poll window is best-effort by construction. Shared with
/// [`reveal`]'s fallback so both report a failing launcher the same way.
pub(crate) fn xdg_open(target: &str) -> Result<(), PlatformError> {
    match spawn_and_reap_with(std::process::Command::new("xdg-open").arg(target), |_| {}) {
        Ok(Some(status)) if !status.success() => Err(PlatformError::CannotComplete {
            reason: format!("xdg-open {target}: exited with {status}"),
        }),
        Ok(_) => Ok(()),
        Err(e) => Err(PlatformError::CannotComplete {
            reason: format!("xdg-open {target}: {e}"),
        }),
    }
}

fn poll_for_immediate_exit_with(
    mut try_wait: impl FnMut() -> std::io::Result<Option<std::process::ExitStatus>>,
    mut sleep: impl FnMut(std::time::Duration),
) -> std::io::Result<Option<std::process::ExitStatus>> {
    for _ in 0..10 {
        if let Some(status) = try_wait()? {
            return Ok(Some(status));
        }
        sleep(std::time::Duration::from_millis(5));
    }
    Ok(None)
}

impl platform::shell::ShellHost for LinuxShellHost {
    fn pump_events(&self, heartbeat: std::time::Duration) {
        std::thread::sleep(heartbeat);
    }

    /// `MemTotal` from `/proc/meminfo`. 0 only when the file cannot be read or
    /// parsed; the Setup pane's catalog then offers nothing, because
    /// `model_catalog::ram_verdict` rates every entry `Exceeds` at 0 GB. That is
    /// the fail-closed posture, but it is why this probe must be real: the
    /// previous hardcoded 0 made every model unofferable on Linux.
    fn physical_memory_bytes(&self) -> u64 {
        std::fs::read_to_string(MEMINFO_PATH)
            .ok()
            .and_then(|meminfo| meminfo_total_bytes(&meminfo))
            .unwrap_or(0)
    }

    fn open_url(&self, url: &str) -> Result<(), PlatformError> {
        xdg_open(url)
    }

    /// **Deliberately fail-closed.** Linux has no equivalent of the macOS TCC
    /// pane this method exists for: AT-SPI needs no per-application grant, and
    /// what does gate it — `org.gnome.desktop.interface toolkit-accessibility`,
    /// `GTK_MODULES`, `NO_AT_BRIDGE`, or a Wayland compositor's own policy — is
    /// per-desktop, with no portable settings URL and no portal to ask. Opening
    /// `gnome-control-center` would be a lie on KDE, XFCE, and Sway alike, so the
    /// honest answer is that there is nothing to open; the caller shows its own
    /// guidance instead.
    fn open_permission_settings(&self) -> Result<(), PlatformError> {
        Err(PlatformError::UnsupportedField {
            reason: "platform_linux::open_permission_settings: Linux has no per-application \
                     accessibility permission pane to open (no TCC equivalent)"
                .to_string(),
        })
    }

    /// `org.freedesktop.FileManager1.ShowItems`, falling back to `xdg-open` on the
    /// containing directory. See [`reveal`] for why there is no portable
    /// "select this file" call.
    #[cfg(target_os = "linux")]
    fn reveal_file(&self, path: &std::path::Path) -> Result<(), PlatformError> {
        reveal::reveal(path)
    }

    /// Off Linux this adapter has no file manager to talk to; the pure path
    /// arithmetic in [`reveal`] is still compiled and tested here.
    #[cfg(not(target_os = "linux"))]
    fn reveal_file(&self, _path: &std::path::Path) -> Result<(), PlatformError> {
        Err(PlatformError::UnsupportedField {
            reason: "platform_linux::reveal_file requires Linux (FileManager1 over D-Bus)"
                .to_string(),
        })
    }

    /// XDG autostart: write or remove `<config>/autostart/compme.desktop`
    /// pointing at the running executable. Needs no display, portal, or session
    /// bus, so it works on a bare Linux host — unlike macOS `SMAppService`, this
    /// is a plain file, and the session reads it at next login.
    fn set_launch_at_login(&self, enabled: bool) -> Result<(), PlatformError> {
        let dir = autostart_dir(
            std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
            std::env::var("HOME").ok().as_deref(),
        )
        .ok_or_else(|| PlatformError::CannotComplete {
            reason: "autostart: neither XDG_CONFIG_HOME nor HOME is set".to_string(),
        })?;
        let exec = std::env::current_exe().map_err(|err| PlatformError::CannotComplete {
            reason: format!("autostart: cannot resolve the running executable: {err}"),
        })?;
        apply_autostart(&dir, enabled, &exec).map_err(|err| PlatformError::CannotComplete {
            reason: format!("autostart {}: {err}", dir.join(AUTOSTART_ENTRY).display()),
        })
    }

    /// Blocking modal confirm via `zenity`, then `kdialog`. `Ok(true)` only on an
    /// explicit confirm click; Return declines. See [`confirm`].
    #[cfg(target_os = "linux")]
    fn confirm(&self, prompt: &shell_flags::ConfirmPrompt<'_>) -> Result<bool, PlatformError> {
        confirm::confirm_with(prompt, confirm::spawn_and_wait)
    }

    /// Off Linux this adapter must not spawn a Linux dialog helper. The argv and
    /// exit-code rules are still compiled and tested here.
    #[cfg(not(target_os = "linux"))]
    fn confirm(&self, _prompt: &shell_flags::ConfirmPrompt<'_>) -> Result<bool, PlatformError> {
        Err(PlatformError::UnsupportedField {
            reason: "platform_linux::confirm requires Linux (zenity/kdialog)".to_string(),
        })
    }

    /// 32 bytes from the Secret Service, created on first use. Fails closed with
    /// no key store, and never fabricates or writes a plaintext key — see
    /// [`memory_key`] and `keyring`.
    #[cfg(target_os = "linux")]
    fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError> {
        memory_key::MemoryKeyStore::secret_service().load_or_create_memory_key()
    }

    /// Off Linux there is no `org.freedesktop.secrets` to talk to. The
    /// load-or-create contract in [`memory_key`] is compiled and tested here.
    #[cfg(not(target_os = "linux"))]
    fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError> {
        Err(PlatformError::UnsupportedField {
            reason: "platform_linux::load_or_create_memory_key requires Linux (Secret Service \
                     over D-Bus)"
                .to_string(),
        })
    }
}

/// Linux ghost/correction overlay: an override-redirect X11 window
/// (ROADMAP Phase 2.5, `OverlayPlacement::OverrideRedirect`).
///
/// The X11 connection is opened **lazily**, on the first show, not in `new()`.
/// Two reasons: `new()` is infallible in the app's wiring, and the contract
/// already requires the host to reconcile a failed `show_ghost` (hide + retract
/// the shown stat) — so reporting "no display" at the first show is exactly the
/// shape callers handle, while a failing constructor would not be.
///
/// Wayland is Phase 3 (`OverlayPlacement::LayerShell`): this presenter needs an
/// X server, and reports a diagnosable error without one.
#[derive(Debug, Default)]
pub struct LinuxOverlayPresenter {
    #[cfg(target_os = "linux")]
    overlay: Option<x11_overlay::X11Overlay>,
}

impl LinuxOverlayPresenter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The live overlay, opening the X11 connection on first use.
    #[cfg(target_os = "linux")]
    fn overlay(&mut self) -> Result<&mut x11_overlay::X11Overlay, PlatformError> {
        if self.overlay.is_none() {
            self.overlay = Some(x11_overlay::X11Overlay::open()?);
        }
        // `is_none` was just resolved, so this cannot fail; expressed as an
        // `ok_or_else` rather than `unwrap` to keep the method panic-free.
        self.overlay
            .as_mut()
            .ok_or_else(|| PlatformError::CannotComplete {
                reason: "platform_linux x11 overlay: connection vanished".into(),
            })
    }

    /// The text window's X11 id once one exists — the seam the live tests use to
    /// interrogate the real window (override-redirect, map state, geometry, input
    /// shape, focus) from a second connection, and to prove `update_ghost` reuses
    /// it instead of creating one per keystroke.
    #[cfg(target_os = "linux")]
    pub fn text_window_id(&self) -> Option<u32> {
        self.overlay.as_ref().and_then(|o| o.text_window_id())
    }

    /// The correction underline window's X11 id, once a correction has shown.
    #[cfg(target_os = "linux")]
    pub fn underline_window_id(&self) -> Option<u32> {
        self.overlay.as_ref().and_then(|o| o.underline_window_id())
    }
}

impl platform::OverlayPresenter for LinuxOverlayPresenter {
    #[cfg(target_os = "linux")]
    fn show_ghost(&mut self, anchor: ScreenRect, text: &str) -> Result<(), PlatformError> {
        self.overlay()?.show_ghost(anchor, text)
    }

    /// Real impl: an override-redirect X11 window (see `x11_overlay`). Fail-closed
    /// off Linux, where there is no X server to talk to — this crate is compiled
    /// on the macOS and Windows lanes.
    #[cfg(not(target_os = "linux"))]
    fn show_ghost(&mut self, _anchor: ScreenRect, _text: &str) -> Result<(), PlatformError> {
        Err(LinuxAdapter::unsupported("show_ghost"))
    }

    #[cfg(target_os = "linux")]
    fn show_correction(&mut self, rect: ScreenRect, suggestion: &str) -> Result<(), PlatformError> {
        self.overlay()?.show_correction(rect, suggestion)
    }

    /// Real impl: an underline bar plus a suggestion banner, both override-redirect
    /// X11 windows (see `x11_overlay`).
    #[cfg(not(target_os = "linux"))]
    fn show_correction(
        &mut self,
        _rect: ScreenRect,
        _suggestion: &str,
    ) -> Result<(), PlatformError> {
        Err(LinuxAdapter::unsupported("show_correction"))
    }

    #[cfg(target_os = "linux")]
    fn update_ghost(&mut self, text: &str) -> Result<(), PlatformError> {
        // Deliberately does NOT open a connection: `update_ghost` requires a
        // ghost already showing, so a presenter that never showed one must fail
        // rather than connect and then discover it has no anchor.
        match self.overlay.as_mut() {
            Some(overlay) => overlay.update_ghost(text),
            None => Err(PlatformError::CannotComplete {
                reason: "platform_linux x11 overlay: cannot update a hidden ghost".into(),
            }),
        }
    }

    /// Real impl: re-render the existing window's text (see `x11_overlay`).
    #[cfg(not(target_os = "linux"))]
    fn update_ghost(&mut self, _text: &str) -> Result<(), PlatformError> {
        Err(LinuxAdapter::unsupported("update_ghost"))
    }

    /// Idempotent on every host: with no connection there is nothing showing, and
    /// unmapping an already-unmapped window is a no-op.
    fn hide(&mut self) -> Result<(), PlatformError> {
        #[cfg(target_os = "linux")]
        if let Some(overlay) = self.overlay.as_mut() {
            return overlay.hide();
        }
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
    fn url_launcher_reports_a_failure_detected_during_the_poll_window() {
        let status = std::process::Command::new("sh")
            .args(["-c", "exit 3"])
            .status()
            .unwrap();
        let mut pending = Some(status);
        let mut polls = 0;
        let detected = poll_for_immediate_exit_with(
            || {
                polls += 1;
                Ok(if polls == 2 { pending.take() } else { None })
            },
            |_| {},
        )
        .unwrap();

        let status = detected.expect("the second poll must observe the launcher failure");
        assert!(!status.success());
        assert_eq!(polls, 2);
    }

    #[test]
    fn scaffold_reports_linux_and_fails_closed() {
        let adapter = LinuxAdapter::new();
        // environment() is the one cheap, infallible method the scaffold answers.
        assert_eq!(adapter.environment().os, OperatingSystem::Linux);
        // The version probe is real (see the `linux_version` tests). Its value is
        // host-dependent — the macOS CI lane builds this crate too, and there
        // neither probe file exists — so only the contract is pinned here: never
        // empty, because `Environment::version` is displayed as-is.
        assert!(!adapter.environment().version.is_empty());
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
        use platform::shell::ShellHost;

        let h = LinuxShellHost::new();
        assert!(!h.secure_input_enabled());
        assert!(!h.screen_capture_permission());
        // open_permission_settings stays fail-closed on every host, deliberately:
        // Linux has no per-application accessibility pane to open (see the method
        // docs). The reason must say so, because "unsupported" with no explanation
        // reads like an unfinished stub.
        let Err(PlatformError::UnsupportedField { reason }) = h.open_permission_settings() else {
            panic!("open_permission_settings must fail closed");
        };
        assert!(
            reason.contains("platform_linux::open_permission_settings")
                && reason.contains("no TCC equivalent"),
            "reason should name the method and why it is unsupported: {reason:?}"
        );
        // set_launch_at_login is real (XDG autostart) and is covered by the
        // `apply_autostart` round-trip below, which writes into a temp directory
        // instead of the developer's real `~/.config`.
        //
        // load_or_create_memory_key / confirm / reveal_file are real now (Phase
        // 2.6) and are NOT called here: on Linux they would talk to the session
        // bus, spawn a blocking dialog on the developer's desktop, or open a file
        // manager. Their host-independent decision logic is unit-tested in
        // `memory_key`, `confirm`, and `reveal`; their live behavior — including
        // the fail-closed paths on a bare host — is in the `#[ignore]`d session
        // tests (`keyring_live_tests.rs`, `confirm_live_tests.rs`). Off
        // Linux they must still refuse, which is what the next test pins.
        let start = std::time::Instant::now();
        h.pump_events(std::time::Duration::from_millis(5));
        assert!(start.elapsed() >= std::time::Duration::from_millis(5));
    }

    /// The three session-dependent services must never *pretend* to work on a
    /// host that is not Linux: the macOS and Windows lanes build this crate, and a
    /// `LinuxShellHost` wired in there has no Secret Service, no zenity, and no
    /// FileManager1. (On Linux these same calls are live, so they are exercised by
    /// the session tests instead — see `shell_host_is_fail_closed`.)
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn session_services_refuse_to_run_off_linux() {
        use platform::shell::ShellHost;
        use shell_flags::ConfirmPrompt;

        let h = LinuxShellHost::new();
        for (method, result) in [
            (
                "load_or_create_memory_key",
                h.load_or_create_memory_key().map(|_| ()),
            ),
            (
                "confirm",
                h.confirm(&ConfirmPrompt {
                    title: "t",
                    message: "m",
                    confirm_label: "c",
                })
                .map(|_| ()),
            ),
            ("reveal_file", h.reveal_file(std::path::Path::new("/tmp/x"))),
        ] {
            let Err(PlatformError::UnsupportedField { reason }) = result else {
                panic!("{method} must fail closed off Linux, got {result:?}");
            };
            assert!(
                reason.contains(method) && reason.contains("Linux"),
                "{method} reason should name the method and the requirement: {reason:?}"
            );
        }
    }

    #[test]
    fn linux_version_prefers_pretty_name_and_names_the_kernel() {
        // Real NixOS/Ubuntu shapes: quoted values, comments, and keys the probe
        // must ignore. PRETTY_NAME wins over NAME/VERSION_ID.
        let os_release = "# a comment\n\
             NAME=NixOS\n\
             VERSION_ID=\"26.05\"\n\
             PRETTY_NAME=\"NixOS 26.05 (Warbler)\"\n\
             ANSI_COLOR=\"0;38;2;126;186;228\"\n";
        assert_eq!(
            linux_version(Some(os_release), Some("6.18.39\n")),
            "NixOS 26.05 (Warbler) (kernel 6.18.39)"
        );
    }

    #[test]
    fn linux_version_falls_back_to_name_and_version_id() {
        // A blank PRETTY_NAME must fall through rather than report an empty
        // version, and a single-quoted value unquotes the same as a double-quoted
        // one. VERSION_ID is optional.
        assert_eq!(
            linux_version(
                Some("PRETTY_NAME=\"\"\nNAME='Debian GNU/Linux'\nVERSION_ID=12\n"),
                None
            ),
            "Debian GNU/Linux 12"
        );
        assert_eq!(linux_version(Some("NAME=Slackware\n"), None), "Slackware");
    }

    #[test]
    fn linux_version_degrades_to_whatever_probe_survives() {
        // environment() is contractually infallible, so every missing/garbage
        // combination has to yield a usable string. The all-missing case is what
        // the macOS CI lane exercises, where neither probe file exists.
        assert_eq!(linux_version(None, Some("6.1.0")), "kernel 6.1.0");
        assert_eq!(linux_version(None, None), "unknown");
        // Whitespace-only kernel file and an os-release with no usable key both
        // count as absent, not as an empty component.
        assert_eq!(
            linux_version(Some("# only comments\n"), Some("  \n")),
            "unknown"
        );
        // A key that merely ends in NAME must not be mistaken for NAME itself.
        assert_eq!(linux_version(Some("CODENAME=trixie\n"), None), "unknown");
    }

    #[test]
    fn meminfo_total_converts_kibibytes_and_refuses_anything_else() {
        // The real first line of /proc/meminfo, plus the neighbours the parser
        // must skip past.
        let meminfo = "MemTotal:       65760812 kB\nMemFree:        52000000 kB\n";
        assert_eq!(meminfo_total_bytes(meminfo), Some(65_760_812 * 1024));
        // A MemTotal-less file (or a truncated read) reports absent, so the
        // caller falls back to 0 rather than a fabricated size.
        assert_eq!(meminfo_total_bytes("MemFree: 1 kB\n"), None);
        assert_eq!(meminfo_total_bytes(""), None);
        // Unknown unit and non-numeric value are refused rather than guessed at.
        assert_eq!(meminfo_total_bytes("MemTotal: 64 MB\n"), None);
        assert_eq!(meminfo_total_bytes("MemTotal: lots kB\n"), None);
        // A corrupt huge value must not wrap into a small (flattering) size.
        assert_eq!(
            meminfo_total_bytes("MemTotal: 18446744073709551615 kB\n"),
            None
        );
    }

    #[test]
    fn autostart_dir_prefers_an_absolute_xdg_config_home() {
        assert_eq!(
            autostart_dir(Some("/home/u/.cfg"), Some("/home/u")).unwrap(),
            PathBuf::from("/home/u/.cfg/autostart")
        );
        // Empty and relative XDG values fall back to $HOME/.config: a relative
        // base would write the entry under the process cwd. The absolute/relative
        // verdict is POSIX and must not depend on the build host — this same test
        // runs on the Windows CI lane, where `Path::is_absolute("/home/u")` is
        // false.
        assert_eq!(
            autostart_dir(Some(""), Some("/home/u")).unwrap(),
            PathBuf::from("/home/u/.config/autostart")
        );
        assert_eq!(
            autostart_dir(Some("relative/cfg"), Some("/home/u")).unwrap(),
            PathBuf::from("/home/u/.config/autostart")
        );
        // A Windows-shaped absolute path is NOT a POSIX absolute path: it falls
        // back rather than being trusted as an XDG base.
        assert_eq!(
            autostart_dir(Some(r"C:\Users\u"), Some("/home/u")).unwrap(),
            PathBuf::from("/home/u/.config/autostart")
        );
        // No usable base at all: the caller must report failure, not write
        // somewhere arbitrary.
        assert_eq!(autostart_dir(None, None), None);
        assert_eq!(autostart_dir(Some(""), Some("")), None);
    }

    #[test]
    fn autostart_exec_value_is_quoted_and_field_codes_are_escaped() {
        assert_eq!(
            desktop_exec_value(Path::new("/usr/bin/compme")),
            "\"/usr/bin/compme\""
        );
        // A space needs the quoting; `%` must become `%%` or the session reads it
        // as a field code and drops it; `"`/`\`/`` ` ``/`$` are backslash-escaped
        // per the desktop-entry spec.
        assert_eq!(
            desktop_exec_value(Path::new("/opt/my apps/100% \"comp$me\"")),
            "\"/opt/my apps/100%% \\\"comp\\$me\\\"\""
        );
        // The generated entry is a parseable desktop file carrying that value.
        let entry = autostart_desktop_entry(Path::new("/usr/bin/compme"));
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("\nExec=\"/usr/bin/compme\"\n"));
        assert!(entry.contains("\nType=Application\n"));
        assert!(entry.ends_with('\n'));
    }

    #[test]
    fn autostart_entry_round_trips_enable_and_disable() {
        let dir = std::env::temp_dir().join(format!(
            "compme-linux-autostart-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let exec = Path::new("/usr/bin/compme");
        let entry = dir.join(AUTOSTART_ENTRY);

        // Enabling creates the directory as well as the entry.
        apply_autostart(&dir, true, exec).unwrap();
        let written = std::fs::read_to_string(&entry).unwrap();
        assert_eq!(written, autostart_desktop_entry(exec));
        // Re-enabling is idempotent (the rename overwrites), and leaves no temp
        // file behind.
        apply_autostart(&dir, true, exec).unwrap();
        assert_eq!(std::fs::read_to_string(&entry).unwrap(), written);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|name| name != AUTOSTART_ENTRY)
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );

        // Disabling removes it, and disabling again succeeds: the requested state
        // already holds, so the settings toggle must not report a failure.
        apply_autostart(&dir, false, exec).unwrap();
        assert!(!entry.exists());
        apply_autostart(&dir, false, exec).unwrap();

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn autostart_reports_io_failure_instead_of_silently_not_enabling() {
        // A path whose parent is a file, not a directory: create_dir_all fails, so
        // the toggle must surface the error and restore its visible state rather
        // than persist a launch-at-login value the session will never honor.
        let file = std::env::temp_dir().join(format!(
            "compme-linux-autostart-blocker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&file, []).unwrap();
        let blocked = file.join("autostart");
        assert!(apply_autostart(&blocked, true, Path::new("/usr/bin/compme")).is_err());
        std::fs::remove_file(&file).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_probes_report_real_values_on_a_linux_host() {
        // The pure parsers above run everywhere; this pins that the wiring reads
        // the real files on a Linux host. It is the test that catches the
        // placeholders regressing — a hardcoded 0 rated every catalog model
        // `Exceeds`, so the Setup pane offered no model at all on Linux.
        use platform::shell::ShellHost;

        let version = LinuxAdapter::new().environment().version;
        assert_ne!(
            version, "unknown",
            "a Linux host must expose /etc/os-release or /proc/sys/kernel/osrelease"
        );
        let bytes = LinuxShellHost::new().physical_memory_bytes();
        assert!(
            bytes >= 64 * 1024 * 1024,
            "physical memory probe returned {bytes} bytes"
        );
    }

    /// UPDATED for Phase 2.5 (the overlay is real on Linux now). This test used
    /// to pin `UnsupportedField` from all three show methods on every host.
    /// Deliberately narrowed rather than deleted: off Linux there is no X server
    /// and the methods stay fail-closed exactly as before, which is what the
    /// macOS and Windows lanes still assert here. The Linux half moved to
    /// `atspi_live_tests.rs`, where a real X session exists to succeed against —
    /// and `overlay_without_a_display_fails_closed_on_linux` below keeps the
    /// no-display path pinned.
    #[cfg(not(target_os = "linux"))]
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

    /// The Linux no-display path: a headless box, a cron job, a container. It must
    /// report a diagnosable error naming the layer and the `DISPLAY` it tried,
    /// never panic — and `hide()` must still be the idempotent success the trait
    /// promises, because the host calls it to reconcile a failed show.
    #[cfg(target_os = "linux")]
    #[test]
    fn overlay_without_a_display_fails_closed_on_linux() {
        use platform::OverlayPresenter;

        let mut o = LinuxOverlayPresenter::new();
        let anchor = ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 2.0,
            h: 16.0,
        };
        // update_ghost never opens a connection, so it fails the same way with or
        // without a display: there is no ghost to update.
        let Err(PlatformError::CannotComplete { reason }) = o.update_ghost("g") else {
            panic!("update_ghost without a shown ghost must fail closed");
        };
        assert!(
            reason.contains("platform_linux x11 overlay") && reason.contains("hidden ghost"),
            "reason should name the layer and the cause: {reason:?}"
        );
        o.hide().expect("hide is contractually idempotent-success");
        o.hide().expect("second hide too");

        // The show paths need a real X server; only assert their shape when there
        // demonstrably is none. A developer running this on a desktop legitimately
        // has one, and the live suite covers the success path there.
        if std::env::var_os("DISPLAY").is_some() {
            return;
        }
        for (label, result) in [
            ("show_ghost", o.show_ghost(anchor, "g")),
            ("show_correction", o.show_correction(anchor, "c")),
        ] {
            let Err(PlatformError::CannotComplete { reason }) = result else {
                panic!("{label} without a display must fail closed");
            };
            assert!(
                reason.starts_with("platform_linux x11 overlay connect:")
                    && reason.contains("DISPLAY=<unset>"),
                "{label} reason should name the layer and the DISPLAY tried: {reason:?}"
            );
        }
        o.hide().expect("hide after a failed show is still Ok");
    }
}
