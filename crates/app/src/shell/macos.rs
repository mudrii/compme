#![allow(dead_code, unused_imports)]

use std::sync::Arc;

use platform::shell::{ShellHost, TrayFlags, TrayHandle};
use platform::PlatformError;

pub type PlatformAdapterImpl = platform_macos::MacosPlatformAdapter;
pub type OverlayPresenterImpl = platform_macos::MacosOverlayPresenter;
pub type SettingsWindow = platform_macos::MacosSettingsWindow;
pub type UrlHandlerGuard = platform_macos::UrlEventHandler;

pub fn make_shell() -> Arc<dyn ShellHost> {
    Arc::new(platform_macos::MacosShellHost::new())
}

pub fn make_adapter(acceptance_pid: Option<i32>) -> Result<PlatformAdapterImpl, PlatformError> {
    match acceptance_pid {
        Some(pid) => {
            platform_macos::MacosPlatformAdapter::with_frontmost_pid_override_for_acceptance(pid)
        }
        None => platform_macos::MacosPlatformAdapter::new(),
    }
}

pub fn make_overlay() -> Result<OverlayPresenterImpl, PlatformError> {
    platform_macos::MacosOverlayPresenter::new()
}

pub fn make_tray(flags: TrayFlags) -> Result<Box<dyn TrayHandle>, PlatformError> {
    platform_macos::MacosTray::new(flags).map(|tray| Box::new(tray) as Box<dyn TrayHandle>)
}

pub use platform_macos::install_url_event_handler;
pub use platform_macos::{
    effective_accept_keys_with_mods_and_grammar, effective_shortcut_bindings, format_accept_key,
    keycode_label_with_mods, parse_accept_key, policy_restore_needed,
    set_accept_keymap_from_config_with_mods, set_shortcut_bindings,
    set_shortcut_bindings_from_config, set_tab_hotkey_suppressed, KeymapError, PersonalizationEdit,
    SettingsFlags, ShortcutBindings, APPS_ROWS, APP_POLICY_FIELDS, APP_POLICY_FIELD_TITLES,
    SETUP_ROWS, STATS_ROWS,
};
