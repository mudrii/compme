//! Product-shell state vocabulary shared by the `app` crate and the
//! `platform_macos` backend: the settings-window and tray flag bags, the
//! persisted key-chord grammar, and the modal-confirm payload. These are
//! macOS-product-shaped types (a real Windows/Linux settings UI would define
//! its own), parked here instead of inside the portable `platform` contract
//! crate. Pure data and sync types (`Arc<Atomic…>`/`Arc<Mutex<…>>` state),
//! std-only, zero OS dependencies — so `platform` may depend on this crate
//! for `ShellHost::confirm`'s `ConfirmPrompt` without the dependency
//! direction ever cycling back.

use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex};

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
    /// Current launch-at-login state and the desired state written by the
    /// General-pane switch. The host loop restores this atomic when the OS
    /// rejects a change so the visible control cannot claim a false state.
    pub general_launch_at_login: Arc<AtomicBool>,
    pub labs_midline: Arc<AtomicBool>,
    pub general_autocorrect: Arc<AtomicBool>,
    pub general_full_autocorrect: Arc<AtomicBool>,
    pub general_thesaurus_selection: Arc<AtomicBool>,
    pub general_trailing_space: Arc<AtomicBool>,
    pub context_cross_app_previous_inputs: Arc<AtomicBool>,
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
    /// Set when the user picks "Visit Website".
    pub visit_website: Arc<AtomicBool>,
    /// Set when the user picks "Contact Support".
    pub contact_support: Arc<AtomicBool>,
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

    #[test]
    fn key_with_mods_formatter_orders_modifiers_and_round_trips() {
        // Inverse of the parser: canonical cmd/shift/option/control word order
        // regardless of mask bit order, bare keycode for an empty mask, and
        // parse(format(x)) is the identity for persisted chords.
        assert_eq!(
            format_key_with_mods(96, (1 << 12) | (1 << 8)),
            "cmd+control+96"
        );
        assert_eq!(format_key_with_mods(96, 0), "96");
        assert_eq!(
            parse_key_with_mods(&format_key_with_mods(49, 1 << 11)),
            Some((49, 1 << 11))
        );
    }

    #[test]
    fn keymap_error_display_renders_each_variant() {
        assert_eq!(
            KeymapError::Collision(53).to_string(),
            "keymap collision: keycode 53 bound more than once"
        );
        assert_eq!(
            KeymapError::InvalidKeycode(-1).to_string(),
            "invalid keycode: -1 (must be non-negative)"
        );
    }
}
