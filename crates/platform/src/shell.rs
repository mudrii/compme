//! Host shell services beyond field text I/O -- the second half of the
//! cross-platform contract (ROADMAP 1.1). `PlatformAdapter` covers
//! focus/caret/read/insert; `ShellHost` covers the product shell around it:
//! event pumping, permissions, clipboard, OCR context, OS integration,
//! modal confirms, and the memory-key store. One impl per OS.
//!
//! Threading: `ShellHost` is `Send + Sync`; methods that present UI enforce
//! their platform's threading contract internally.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{PlatformError, ScreenRect};

pub type UrlCallback = dyn Fn(String) + Send + Sync + 'static;
pub type KeyWithMods = (i64, u32);
pub type EffectiveAcceptKeys = (KeyWithMods, KeyWithMods, Option<KeyWithMods>);
pub type AppsPolicyEdit = (usize, usize, bool);
pub type AppsPolicyEditSlot = Arc<Mutex<Option<AppsPolicyEdit>>>;
pub type RebindRequest = (
    Option<KeyWithMods>,
    Option<KeyWithMods>,
    Option<KeyWithMods>,
);
pub type CurrentAcceptKeys = (KeyWithMods, KeyWithMods, Option<KeyWithMods>);

pub const APPS_ROWS: usize = 8;
pub const APP_POLICY_FIELDS: usize = 5;
pub const APP_POLICY_FIELD_TITLES: [&str; APP_POLICY_FIELDS] = [
    "Enabled",
    "Tab key",
    "Mid-line",
    "Autocorrect",
    "Grammar fix",
];
pub const STATS_ROWS: usize = 4;
pub const SETUP_ROWS: usize = 3;

const ACCEPT_KEY_MODIFIERS: [(&str, u32); 4] = [
    ("cmd", 1 << 8),
    ("shift", 1 << 9),
    ("option", 1 << 11),
    ("control", 1 << 12),
];

/// Parse the persisted key chord grammar shared by accept keys and always-on
/// shortcut bindings.
pub fn parse_key_with_mods(raw: &str) -> Option<(i64, u32)> {
    let mut keycode = None;
    let mut mask = 0u32;
    for token in raw.split('+') {
        let token = token.trim();
        if token.is_empty() || keycode.is_some() {
            return None;
        }
        if let Ok(code) = token.parse::<i64>() {
            if code < 0 {
                return None;
            }
            keycode = Some(code);
        } else {
            mask |= match token.to_ascii_lowercase().as_str() {
                "cmd" | "command" | "super" | "meta" | "win" => 1 << 8,
                "shift" => 1 << 9,
                "opt" | "option" | "alt" => 1 << 11,
                "ctrl" | "control" => 1 << 12,
                _ => return None,
            };
        }
    }
    keycode.map(|code| (code, mask))
}

pub fn format_key_with_mods(keycode: i64, mask: u32) -> String {
    let mut out = String::new();
    for (word, bit) in ACCEPT_KEY_MODIFIERS {
        if mask & bit != 0 {
            out.push_str(word);
            out.push('+');
        }
    }
    out.push_str(&keycode.to_string());
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeymapError {
    Collision(i64),
    InvalidKeycode(i64),
}

impl std::fmt::Display for KeymapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeymapError::Collision(keycode) => {
                write!(
                    f,
                    "keymap collision: keycode {keycode} bound more than once"
                )
            }
            KeymapError::InvalidKeycode(keycode) => {
                write!(f, "invalid keycode: {keycode} (must be non-negative)")
            }
        }
    }
}

impl std::error::Error for KeymapError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShortcutBindings {
    pub force_activate: Option<(i64, u32)>,
    pub toggle_app: Option<(i64, u32)>,
    pub toggle_global: Option<(i64, u32)>,
    pub grammar_check: Option<(i64, u32)>,
}

impl ShortcutBindings {
    pub fn from_config(
        force_activate: Option<&str>,
        toggle_app: Option<&str>,
        toggle_global: Option<&str>,
        grammar_check: Option<&str>,
    ) -> Self {
        Self {
            force_activate: force_activate.and_then(parse_key_with_mods),
            toggle_app: toggle_app.and_then(parse_key_with_mods),
            toggle_global: toggle_global.and_then(parse_key_with_mods),
            grammar_check: grammar_check.and_then(parse_key_with_mods),
        }
    }

    pub fn has_internal_collision(&self) -> bool {
        let bound: Vec<(i64, u32)> = [
            self.force_activate,
            self.toggle_app,
            self.toggle_global,
            self.grammar_check,
        ]
        .into_iter()
        .flatten()
        .collect();
        for i in 0..bound.len() {
            for j in (i + 1)..bound.len() {
                if bound[i] == bound[j] {
                    return true;
                }
            }
        }
        false
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersonalizationEdit {
    GlobalInstructions(String),
    SenderName(String),
    SenderEmail(String),
    StrengthStop(usize),
}

#[derive(Clone)]
pub struct SettingsFlags {
    pub general_enabled: Arc<AtomicBool>,
    pub labs_midline: Arc<AtomicBool>,
    pub general_autocorrect: Arc<AtomicBool>,
    pub general_trailing_space: Arc<AtomicBool>,
    pub context_clipboard: Arc<AtomicBool>,
    pub context_screen: Arc<AtomicBool>,
    pub emoji_enabled: Arc<AtomicBool>,
    pub emoji_skin_tone_index: Arc<AtomicUsize>,
    pub emoji_gender_index: Arc<AtomicUsize>,
    pub stats_lines: Arc<Mutex<Vec<String>>>,
    pub stat_range_index: Arc<AtomicUsize>,
    pub stat_range_titles: Vec<String>,
    pub stat_group_index: Arc<AtomicUsize>,
    pub stat_group_titles: Vec<String>,
    pub about_text: String,
    pub setup_lines: Arc<Mutex<Vec<String>>>,
    pub setup_grant_ax: Arc<AtomicBool>,
    pub setup_request_screen: Arc<AtomicBool>,
    pub setup_reveal_model: Arc<AtomicBool>,
    /// "Show Models Folder" clicked — the run loop reveals the app-support
    /// models directory in Finder.
    pub setup_reveal_models_dir: Arc<AtomicBool>,
    /// Bring-your-own-model: a path the user picked via the file panel, for the
    /// run loop to validate and point COMPME_MODEL_PATH at. `None` until chosen.
    pub setup_choose_model: Arc<Mutex<Option<std::path::PathBuf>>>,
    pub setup_download_model: Arc<AtomicBool>,
    pub setup_model_index: Arc<AtomicUsize>,
    pub setup_model_menu_titles: Vec<String>,
    pub apps_lines: Arc<Mutex<Vec<String>>>,
    pub apps_policy_bits: Arc<Mutex<Vec<[bool; APP_POLICY_FIELDS]>>>,
    pub apps_delete_row: Arc<Mutex<Option<usize>>>,
    pub apps_edit: AppsPolicyEditSlot,
    pub shortcuts_text: Arc<Mutex<String>>,
    pub shortcuts_rebind_request: Arc<Mutex<Option<RebindRequest>>>,
    pub personalization_edit: Arc<Mutex<Vec<PersonalizationEdit>>>,
    pub personalization_instructions: Arc<Mutex<String>>,
    pub personalization_sender_name: Arc<Mutex<String>>,
    pub personalization_sender_email: Arc<Mutex<String>>,
    pub personalization_strength_index: Arc<AtomicUsize>,
    pub personalization_strength_titles: Vec<String>,
}

/// A blocking modal confirmation. The confirming button must not be the
/// default: Return/Enter declines, matching the existing macOS prompts.
pub struct ConfirmPrompt<'a> {
    pub title: &'a str,
    pub message: &'a str,
    /// Label of the confirming button, e.g. "Allow", "Delete".
    pub confirm_label: &'a str,
}

pub trait ShellHost: Send + Sync {
    /// Drain queued native UI events, then service the platform main loop for
    /// at most `heartbeat`.
    fn pump_events(&self, heartbeat: Duration);

    /// Whether the process holds the OS grant required to observe and inject
    /// text. Default `true`: Windows and Linux need no macOS-style
    /// Accessibility trust grant.
    fn accessibility_trusted(&self) -> bool {
        true
    }

    /// Fire the OS permission prompt for that grant, if one exists. Returns
    /// the possibly unchanged grant state.
    fn prompt_accessibility_trust(&self) -> bool {
        true
    }

    /// Whether the OS reports a global secure-input session. Default `false`;
    /// per-field secure detection stays `Capabilities::security_state`.
    fn secure_input_enabled(&self) -> bool {
        false
    }

    /// Screen-capture grant probe. Fail closed: unknown means absent, OCR off.
    fn screen_capture_permission(&self) -> bool {
        false
    }

    /// Screen-capture grant request. Fail closed: unknown means absent, OCR off.
    fn request_screen_capture_permission(&self) -> bool {
        false
    }

    fn physical_memory_bytes(&self) -> u64;

    fn bundle_id_for_pid(&self, _pid: i32) -> Option<String> {
        None
    }

    fn read_clipboard_text(&self) -> Option<String> {
        None
    }

    /// OCR'd text near the caret. `None` means unavailable.
    fn screen_context_text(
        &self,
        _caret_rect: Option<ScreenRect>,
        _max_chars: usize,
    ) -> Option<String> {
        None
    }

    fn display_scales(&self) -> Vec<(ScreenRect, f64)> {
        Vec::new()
    }

    /// Open `url` with the OS default handler. Non-blocking.
    fn open_url(&self, url: &str) -> Result<(), PlatformError>;

    /// Open the OS settings pane where the user grants text-access permission.
    fn open_permission_settings(&self) -> Result<(), PlatformError>;

    /// Reveal `path` in the OS file browser.
    fn reveal_file(&self, path: &Path) -> Result<(), PlatformError>;

    fn set_launch_at_login(&self, enabled: bool) -> Result<(), PlatformError>;

    /// Blocking modal confirm. `Ok(true)` only on an explicit confirm click.
    fn confirm(&self, prompt: &ConfirmPrompt<'_>) -> Result<bool, PlatformError>;

    /// 32-byte memory-store encryption key from the OS key store, created on
    /// first use.
    fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError>;
}

/// Status-area handle. Owned by the main loop; implementors may be UI-thread
/// only.
pub trait TrayHandle {
    fn set_status(
        &self,
        title: &str,
        status_line: &str,
        enabled: bool,
        needs_accessibility: bool,
    ) -> Result<(), PlatformError>;

    fn set_stats_line(&self, line: &str) -> Result<(), PlatformError>;
}

/// Shared toggles flipped by tray menu actions and observed by the run loop.
#[derive(Clone)]
pub struct TrayFlags {
    /// User enable/disable toggle (suggestions on/off).
    pub enabled: Arc<AtomicBool>,
    /// Set when the user picks Quit.
    pub quit: Arc<AtomicBool>,
    /// Set when the user picks Open Accessibility Settings.
    pub open_settings: Arc<AtomicBool>,
    /// Set when the user picks Snooze; the run loop consumes it (swap false)
    /// and applies the snooze to its prefs.
    pub snooze_requested: Arc<AtomicBool>,
    /// "Disable Completions Globally ▸" arm (run loop consumes; Always is
    /// routed to the persistent enabled flag there).
    pub global_disable: Arc<Mutex<Option<DisableArm>>>,
    /// Set when the user picks "Settings…"; the run loop shows the S2
    /// settings window (and handles the activation-policy dance).
    pub open_settings_window: Arc<AtomicBool>,
    /// Set when the user picks "Check for Updates…"; the run loop opens the
    /// GitHub Releases updater surface.
    pub check_updates: Arc<AtomicBool>,
    /// Set when the user picks "Toggle Input Collection in Current App"; the
    /// run loop consumes it (swap false) and flips the frontmost app's
    /// typing-history collection override.
    pub collection_toggle: Arc<AtomicBool>,
    /// Set when the user picks a "Disable Completions in Current App" arm;
    /// the run loop consumes it (take) and applies it to the FRONTMOST app's
    /// prefs (the tray never resolves app identity itself).
    pub app_disable: Arc<Mutex<Option<DisableArm>>>,
}

/// Which "Disable Completions in Current App" arm the user picked
/// (Cotypist-style tray submenu).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisableArm {
    /// Pause in this app for one hour (auto-resumes).
    Hour,
    /// Pause in this app until the app relaunches (session-only).
    UntilRelaunch,
    /// Permanently exclude this app (persisted).
    Always,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal impl providing only the required methods. This pins that every
    /// default is fail-closed/absent, so a not-yet-ported platform inherits
    /// safe behavior.
    struct BareHost;

    impl ShellHost for BareHost {
        fn pump_events(&self, _heartbeat: Duration) {}

        fn physical_memory_bytes(&self) -> u64 {
            1
        }

        fn open_url(&self, _url: &str) -> Result<(), PlatformError> {
            Ok(())
        }

        fn open_permission_settings(&self) -> Result<(), PlatformError> {
            Ok(())
        }

        fn reveal_file(&self, _path: &Path) -> Result<(), PlatformError> {
            Ok(())
        }

        fn set_launch_at_login(&self, _enabled: bool) -> Result<(), PlatformError> {
            Ok(())
        }

        fn confirm(&self, _prompt: &ConfirmPrompt<'_>) -> Result<bool, PlatformError> {
            Ok(false)
        }

        fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError> {
            Err(PlatformError::UnsupportedField {
                reason: "bare".into(),
            })
        }
    }

    #[test]
    fn shell_defaults_are_fail_closed() {
        let h = BareHost;

        assert!(h.accessibility_trusted());
        assert!(h.prompt_accessibility_trust());
        assert!(!h.secure_input_enabled());
        assert!(!h.screen_capture_permission());
        assert!(!h.request_screen_capture_permission());
        assert_eq!(h.bundle_id_for_pid(1), None);
        assert_eq!(h.read_clipboard_text(), None);
        assert_eq!(h.screen_context_text(None, 100), None);
        assert!(h.display_scales().is_empty());
    }

    #[test]
    fn shell_host_is_object_safe_and_send_sync() {
        fn takes(_: Arc<dyn ShellHost>) {}

        takes(Arc::new(BareHost));
    }

    #[test]
    fn key_with_mods_parser_accepts_shared_shortcut_grammar() {
        assert_eq!(parse_key_with_mods("cmd+96"), Some((96, 1 << 8)));
        assert_eq!(
            parse_key_with_mods("ctrl+shift+96"),
            Some((96, (1 << 12) | (1 << 9)))
        );
        assert_eq!(parse_key_with_mods("alt+49"), Some((49, 1 << 11)));
        assert_eq!(parse_key_with_mods("96"), Some((96, 0)));
    }

    #[test]
    fn key_with_mods_parser_rejects_malformed_chords() {
        assert_eq!(parse_key_with_mods("ctrl"), None);
        assert_eq!(parse_key_with_mods("ctrl+"), None);
        assert_eq!(parse_key_with_mods("96+ctrl"), None);
        assert_eq!(parse_key_with_mods("-1"), None);
        assert_eq!(parse_key_with_mods("wat+96"), None);
    }
}
