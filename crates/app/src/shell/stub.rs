//! Portable shell stub.
//!
//! On Windows/Linux this module is glob-re-exported as the `shell` surface
//! (`shell/mod.rs`: `pub use stub::*`), so every public item below is live.
//! On macOS the module is compiled only in test builds, and solely to supply
//! `stub::make_tray` for run-loop unit tests; the real shell comes from
//! `macos::*` and the glob is disabled, so every other item here is unused on
//! that one build. Each such item therefore carries `#[allow(dead_code)]` (the
//! cfg-shaped "live on Windows/Linux, dead on the macOS test build" fact the
//! code can't otherwise express). Items left un-annotated stay dead-code checked.

use std::sync::{Arc, LazyLock, RwLock};

// `ShellHost` is cfg-shaped: `make_shell` uses it on Windows/Linux, while the
// macOS test build compiles this file without that consumer.
#[allow(unused_imports)]
use platform::shell::{ShellHost, TrayHandle};
use platform::PlatformError;
use shell_flags::TrayFlags;

#[cfg(windows)]
pub type PlatformAdapterImpl = platform_windows::WindowsAdapter;
#[cfg(windows)]
pub type OverlayPresenterImpl = platform_windows::WindowsOverlayPresenter;
#[cfg(target_os = "linux")]
pub type PlatformAdapterImpl = platform_linux::LinuxAdapter;
#[cfg(target_os = "linux")]
pub type OverlayPresenterImpl = platform_linux::LinuxOverlayPresenter;

#[cfg(windows)]
pub fn make_shell() -> Arc<dyn ShellHost> {
    Arc::new(platform_windows::WindowsShellHost::new())
}

#[cfg(target_os = "linux")]
pub fn make_shell() -> Arc<dyn ShellHost> {
    Arc::new(platform_linux::LinuxShellHost::new())
}

// Adapter/overlay constructors exist only where a real platform adapter backs
// the stub (Windows/Linux); macOS test builds compile this module for its
// inert pieces (tray, URL handler, settings window) and inject their own
// recording fakes for adapter/overlay/shell.
#[cfg(windows)]
pub fn make_adapter(_acceptance_pid: Option<i32>) -> Result<PlatformAdapterImpl, PlatformError> {
    Ok(PlatformAdapterImpl::new())
}

/// `with_accessibility()`, not `new()`.
///
/// `LinuxAdapter::new()` is deliberately inert — it opens no accessibility bus,
/// so every field read, insert, and subscription fails closed. That is the right
/// default for a constructor unit tests call dozens of times (`org.a11y.Bus` is
/// D-Bus-activatable, and a host without the service waits out a 25-second
/// method timeout), but it is the wrong thing for the product: with `new()` here,
/// every live Linux surface that Phase 2 built and gate-tested was unreachable
/// from the binary. Opening the bus once at startup is exactly where that
/// one-time cost belongs.
///
/// A host with no accessibility session still degrades rather than fails: the
/// constructor reports no session and the adapter keeps its fail-closed answers.
#[cfg(target_os = "linux")]
pub fn make_adapter(_acceptance_pid: Option<i32>) -> Result<PlatformAdapterImpl, PlatformError> {
    Ok(platform_linux::LinuxAdapter::with_accessibility())
}

#[cfg(any(windows, target_os = "linux"))]
pub fn make_overlay() -> Result<OverlayPresenterImpl, PlatformError> {
    Ok(OverlayPresenterImpl::new())
}

pub fn make_tray(_flags: TrayFlags) -> Result<Box<dyn TrayHandle>, PlatformError> {
    Err(PlatformError::UnsupportedField {
        reason: "tray not yet implemented (Tier 1.1 scaffold)".into(),
    })
}

#[allow(dead_code)]
pub struct UrlHandlerGuard;

#[allow(dead_code)]
pub fn install_url_event_handler(
    _on_url: Arc<shell_flags::UrlCallback>,
) -> Result<UrlHandlerGuard, PlatformError> {
    Err(PlatformError::UnsupportedField {
        reason: "deep links not yet implemented (Tier 1.1 scaffold)".into(),
    })
}

#[allow(unused_imports)]
// The facade surface is intentionally broader than each host's call set.
pub use shell_flags::{
    AppsPolicyEdit, AppsPolicyEditSlot, CurrentAcceptKeys, EffectiveAcceptKeys, KeyWithMods,
    KeymapError, PersonalizationEdit, RebindRequest, SettingsFlags, ShortcutBindings, APPS_ROWS,
    APP_POLICY_FIELDS, APP_POLICY_FIELD_TITLES, SETUP_ROWS, STATS_ROWS,
};

#[allow(dead_code)]
static SHORTCUT_BINDINGS: LazyLock<RwLock<ShortcutBindings>> =
    LazyLock::new(|| RwLock::new(ShortcutBindings::default()));

#[allow(dead_code)]
pub fn parse_accept_key(raw: &str) -> Option<(i64, u32)> {
    shell_flags::parse_key_with_mods(raw)
}

#[allow(dead_code)]
pub fn format_accept_key(keycode: i64, mask: u32) -> String {
    shell_flags::format_key_with_mods(keycode, mask)
}

#[allow(dead_code)]
pub fn keycode_label_with_mods(code: i64, mask: u32) -> String {
    format_accept_key(code, mask)
}

#[allow(dead_code)]
pub fn set_tab_hotkey_suppressed(_suppressed: bool) {}

/// G5 chord translation: persisted macOS chords land in `platform_linux`'s
/// process-wide store, which the Linux adapter reads both for its startup probe
/// and for the accept subscription's transactional live-rearm hook.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub fn set_accept_keymap_from_config_with_mods(
    word: Option<(i64, u32)>,
    full: Option<(i64, u32)>,
    grammar_accept: Option<(i64, u32)>,
) -> Result<(), KeymapError> {
    platform_linux::x11_keys::set_accept_chords_with_mods(word, full, grammar_accept)
}

/// No rebinding mechanism exists off Linux yet (Windows is a fail-closed
/// scaffold), so the persisted chords are refused rather than silently
/// accepted: the run loop then logs "using defaults" instead of "accept keys
/// rebound" for a rebind that never happened.
#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
pub fn set_accept_keymap_from_config_with_mods(
    _word: Option<(i64, u32)>,
    _full: Option<(i64, u32)>,
    _grammar_accept: Option<(i64, u32)>,
) -> Result<(), KeymapError> {
    Err(KeymapError::Unsupported)
}

#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub fn effective_accept_keys_with_mods_and_grammar() -> EffectiveAcceptKeys {
    platform_linux::x11_keys::effective_accept_chords_with_mods()
}

/// The defaults, because [`set_accept_keymap_from_config_with_mods`] refuses
/// every rebind on these hosts: what is reported is exactly what is in force.
#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
pub fn effective_accept_keys_with_mods_and_grammar() -> EffectiveAcceptKeys {
    ((48, 0), (50, 0), None)
}

#[allow(dead_code)]
pub fn effective_shortcut_bindings() -> ShortcutBindings {
    *SHORTCUT_BINDINGS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[allow(dead_code)]
pub fn set_shortcut_bindings(bindings: ShortcutBindings) {
    *SHORTCUT_BINDINGS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = bindings;
}

#[allow(dead_code)]
pub fn set_shortcut_bindings_from_config(
    force_activate: Option<&str>,
    toggle_app: Option<&str>,
    toggle_global: Option<&str>,
    grammar_check: Option<&str>,
) -> ShortcutBindings {
    let bindings =
        ShortcutBindings::from_config(force_activate, toggle_app, toggle_global, grammar_check);
    let effective = if bindings.has_internal_collision() {
        ShortcutBindings::default()
    } else {
        bindings
    };
    set_shortcut_bindings(effective);
    effective
}

#[allow(dead_code)]
pub fn policy_restore_needed(was_visible: bool, visible_now: bool) -> bool {
    was_visible && !visible_now
}

/// No settings window exists off macOS: Windows is a fail-closed scaffold and
/// Linux is config-file-only by decision (2026-07-29). `show` therefore
/// reports that instead of pretending a window opened, so the run loop logs
/// "settings window unavailable"; the refresh hooks stay no-ops because there
/// is nothing to refresh, and `is_visible` is honestly `false`.
#[allow(dead_code)]
pub struct SettingsWindow;

#[allow(dead_code)]
impl SettingsWindow {
    pub fn new(_flags: SettingsFlags) -> Self {
        Self
    }

    pub fn show(&mut self) -> Result<(), PlatformError> {
        Err(PlatformError::UnsupportedField {
            reason: "settings window not implemented on this platform (Windows scaffold; Linux is config-file-only)".into(),
        })
    }

    pub fn flush_personalization_edits(&self) {}

    pub fn refresh_switches(&self) {}

    pub fn refresh_setup_labels(&self) {}

    pub fn refresh_shortcuts_label(&self) {}

    pub fn refresh_apps_labels(&self) {}

    pub fn is_visible(&self) -> bool {
        false
    }

    pub fn restore_accessory_policy(&self) -> Result<(), PlatformError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_window_stub_reports_unavailable_instead_of_silent_success() {
        let mut window = SettingsWindow;
        let Err(PlatformError::UnsupportedField { reason }) = window.show() else {
            panic!("the stub has no window to show; Ok would make the run loop log a success");
        };
        assert!(reason.contains("settings window"), "{reason}");
        assert!(!window.is_visible());
        // Nothing was shown, so there is no activation policy to restore.
        assert!(window.restore_accessory_policy().is_ok());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn accept_keymap_rebind_is_refused_where_no_mechanism_exists() {
        assert_eq!(
            set_accept_keymap_from_config_with_mods(Some((48, 0)), None, None),
            Err(KeymapError::Unsupported)
        );
        // The reported effective keys are the defaults the refusal leaves in force.
        assert_eq!(
            effective_accept_keys_with_mods_and_grammar(),
            ((48, 0), (50, 0), None)
        );
    }
}
