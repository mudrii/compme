#![allow(dead_code)]

use std::sync::{Arc, LazyLock, RwLock};

use platform::shell::{ShellHost, TrayFlags, TrayHandle};
use platform::PlatformError;

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
#[cfg(any(windows, target_os = "linux"))]
pub fn make_adapter(_acceptance_pid: Option<i32>) -> Result<PlatformAdapterImpl, PlatformError> {
    Ok(PlatformAdapterImpl::new())
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

pub struct UrlHandlerGuard;

pub fn install_url_event_handler(
    _on_url: Arc<platform::shell::UrlCallback>,
) -> Result<UrlHandlerGuard, PlatformError> {
    Err(PlatformError::UnsupportedField {
        reason: "deep links not yet implemented (Tier 1.1 scaffold)".into(),
    })
}

pub use platform::shell::{
    AppsPolicyEdit, AppsPolicyEditSlot, CurrentAcceptKeys, EffectiveAcceptKeys, KeyWithMods,
    KeymapError, PersonalizationEdit, RebindRequest, SettingsFlags, ShortcutBindings, APPS_ROWS,
    APP_POLICY_FIELDS, APP_POLICY_FIELD_TITLES, SETUP_ROWS, STATS_ROWS,
};

static SHORTCUT_BINDINGS: LazyLock<RwLock<ShortcutBindings>> =
    LazyLock::new(|| RwLock::new(ShortcutBindings::default()));

pub fn parse_accept_key(raw: &str) -> Option<(i64, u32)> {
    platform::shell::parse_key_with_mods(raw)
}

pub fn format_accept_key(keycode: i64, mask: u32) -> String {
    platform::shell::format_key_with_mods(keycode, mask)
}

pub fn keycode_label_with_mods(code: i64, mask: u32) -> String {
    format_accept_key(code, mask)
}

pub fn set_tab_hotkey_suppressed(_suppressed: bool) {}

pub fn set_accept_keymap_from_config_with_mods(
    _word: Option<(i64, u32)>,
    _full: Option<(i64, u32)>,
    _grammar_accept: Option<(i64, u32)>,
) -> Result<(), KeymapError> {
    Ok(())
}

pub fn effective_accept_keys_with_mods_and_grammar() -> EffectiveAcceptKeys {
    ((48, 0), (50, 0), None)
}

pub fn effective_shortcut_bindings() -> ShortcutBindings {
    *SHORTCUT_BINDINGS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn set_shortcut_bindings(bindings: ShortcutBindings) {
    *SHORTCUT_BINDINGS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = bindings;
}

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

pub fn policy_restore_needed(was_visible: bool, visible_now: bool) -> bool {
    was_visible && !visible_now
}

pub struct SettingsWindow;

impl SettingsWindow {
    pub fn new(_flags: SettingsFlags) -> Self {
        Self
    }

    pub fn show(&mut self) -> Result<(), PlatformError> {
        Ok(())
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
