//! Settings edge detection and live-application policy.
//!
//! Callers provide persistence and engine callbacks. This module guarantees
//! that each setting is applied once per edge and that OS-backed changes are
//! committed before their persisted value.

use std::sync::atomic::{AtomicBool, Ordering};

use platform::{shell::ShellHost, PlatformError};
use prefs::Prefs;

use crate::loop_state::SettingsState;
use crate::run_loop::{emoji_gender_edge, emoji_skin_tone_edge, emoji_switch_edge, Config};
use crate::shell::SettingsFlags;

/// Every runtime-persisted config key that can be shadowed by the process
/// environment. OS-backed launch-at-login state is intentionally absent.
///
/// Deliberately conservative: a key set to `""` still warns because it still
/// occupies the environment layer.
pub(crate) const SWITCH_KEYS: [&str; 36] = [
    "COMPME_ENABLED",
    "COMPME_MIDLINE",
    "COMPME_AUTOCORRECT",
    "COMPME_FULL_AUTOCORRECT",
    "COMPME_THESAURUS_SELECTION",
    "COMPME_GRAMMAR_FIX",
    "COMPME_TRAILING_SPACE",
    "COMPME_CROSS_APP_PREVIOUS_INPUTS",
    "COMPME_CLIPBOARD_CONTEXT",
    "COMPME_SCREEN_CONTEXT",
    "COMPME_INSTRUCTIONS",
    "COMPME_SENDER_NAME",
    "COMPME_SENDER_EMAIL",
    "COMPME_STRENGTH",
    "COMPME_EMOJI",
    "COMPME_EMOJI_SKIN_TONE",
    "COMPME_EMOJI_GENDER",
    "COMPME_NO_COLLECT_APPS",
    "COMPME_EXCLUDED_APPS",
    "COMPME_EXCLUDED_DOMAINS",
    "COMPME_ENABLED_APPS",
    "COMPME_DISABLED_APPS",
    "COMPME_MIDLINE_ON_APPS",
    "COMPME_MIDLINE_OFF_APPS",
    "COMPME_AUTOCORRECT_ON_APPS",
    "COMPME_AUTOCORRECT_OFF_APPS",
    "COMPME_GRAMMAR_FIX_ON_APPS",
    "COMPME_GRAMMAR_FIX_OFF_APPS",
    "COMPME_THESAURUS_ON_APPS",
    "COMPME_THESAURUS_OFF_APPS",
    "COMPME_TAB_DISABLED_APPS",
    // License acceptances persist on the prompt's Accept; an env shadow
    // resurrects the un-accepted state at relaunch and causes a surprise
    // fail-closed re-prompt.
    "COMPME_LICENSE_ACCEPTED",
    // Accept-key rebinds persist after a successful live re-arm; an env shadow
    // resurrects the old keys at relaunch while Settings reads the file.
    "COMPME_ACCEPT_WORD_KEY",
    "COMPME_ACCEPT_FULL_KEY",
    "COMPME_GRAMMAR_ACCEPT_KEY",
    "COMPME_GRAMMAR_CHECK_KEY",
];

/// One warning line per switch key currently set in the environment.
pub(crate) fn env_shadow_warnings(is_env_set: impl Fn(&str) -> bool) -> Vec<String> {
    SWITCH_KEYS
        .iter()
        .filter(|key| is_env_set(key))
        .map(|key| {
            format!(
                "{key} is set in the environment \u{2014} Settings changes persist to \
                 config.env but the environment wins at relaunch"
            )
        })
        .collect()
}

pub(crate) fn startup_env_shadow_notice_lines(is_env_set: impl Fn(&str) -> bool) -> Vec<String> {
    env_shadow_warnings(is_env_set)
        .into_iter()
        .map(|warning| format!("compme: {warning}"))
        .collect()
}

/// Edge-detect a settings switch. The caller applies and persists the returned
/// state exactly once per change.
pub(crate) fn switch_edge(flag: &AtomicBool, current: &mut bool) -> Option<bool> {
    let now = flag.load(Ordering::Relaxed);
    (now != *current).then(|| {
        *current = now;
        now
    })
}

/// Pre-4a watcher API; the production path is now
/// [`drain_settings_edges`] + `apply_settings_commands`. Kept as a direct
/// unit fixture for the per-edge contracts its tests pin.
#[cfg(test)]
pub(crate) fn apply_autocorrect_settings_edge(
    flag: &AtomicBool,
    current: &mut bool,
    persist: impl FnOnce(bool),
    dismiss_existing: impl FnOnce(bool),
) -> Option<bool> {
    let on = switch_edge(flag, current)?;
    persist(on);
    if !on {
        dismiss_existing(on);
    }
    Some(on)
}

/// Pre-4a watcher API; production path is [`drain_settings_edges`] +
/// `apply_settings_commands`. Kept as a direct unit fixture.
#[cfg(test)]
pub(crate) fn apply_trailing_space_settings_edge(
    flag: &AtomicBool,
    current: &mut bool,
    set_trailing_space: impl FnOnce(bool),
    persist: impl FnOnce(bool),
) -> Option<bool> {
    let on = switch_edge(flag, current)?;
    set_trailing_space(on);
    persist(on);
    Some(on)
}

/// Pre-4a watcher API; production path is [`drain_settings_edges`] +
/// `apply_settings_commands`. Kept as a direct unit fixture.
#[cfg(test)]
pub(crate) fn apply_midline_settings_edge(
    flag: &AtomicBool,
    global_mid_word: &mut bool,
    prefs: &Prefs,
    focused_app: Option<&str>,
    set_allow_mid_word: impl FnOnce(bool),
    persist: impl FnOnce(bool),
) -> Option<bool> {
    let on = switch_edge(flag, global_mid_word)?;
    set_allow_mid_word(prefs.mid_line_enabled(focused_app, on));
    persist(on);
    Some(on)
}

/// One drained Settings-watcher edge (plan item 4a). The drain half
/// ([`drain_settings_edges`]) owns every pure mirror and emits commands in
/// the watcher order the loop historically applied them, so the apply half
/// replays exactly that sequence.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SettingsCommand {
    /// Pure switch: the `Config` mirror already flipped; apply persists and,
    /// on an off edge, dismisses the visible suggestion.
    Autocorrect {
        on: bool,
    },
    FullAutocorrect {
        on: bool,
    },
    ThesaurusSelection {
        on: bool,
    },
    /// Engine setter + persist.
    TrailingSpace {
        on: bool,
    },
    /// `allow` is the per-app-resolved gate (prefs override + focused app),
    /// not the raw global default.
    Midline {
        on: bool,
        allow: bool,
    },
    CrossAppPreviousInputs {
        on: bool,
    },
    ClipboardContext {
        on: bool,
    },
    /// Emoji on/off; the parsed payload survives off/on cycles in
    /// [`SettingsState::emoji_prefs`].
    EmojiSwitch {
        on: bool,
    },
    /// Persisted `COMPME_EMOJI_SKIN_TONE` value for a popup change.
    EmojiSkinTone {
        value: &'static str,
    },
    /// Persisted `COMPME_EMOJI_GENDER` value for a popup change.
    EmojiGender {
        value: &'static str,
    },
    /// OS-backed edge whose mirror may only move on a SUCCESSFUL apply: the
    /// drain emits without consuming, and apply must either update
    /// `SettingsState::current_launch_at_login` and persist, or restore the
    /// UI atomic and redraw (see [`apply_launch_at_login_settings_edge`]).
    LaunchAtLogin {
        desired: bool,
    },
    /// OS-backed edge with the same contract: apply probes the Screen
    /// Recording grant and spawns the OCR worker, reverting the UI atomic
    /// AND the `Config` mirror on denial or spawn failure.
    ScreenContext {
        desired: bool,
    },
}

/// Pure edge detection over the Settings flag bus (plan item 4a): updates
/// every pure mirror in `config`/`settings` and returns the commands the
/// effect half must apply, in watcher order. No engine, shell, persist, or
/// IO happens here. The two OS-backed edges ([`SettingsCommand::LaunchAtLogin`],
/// [`SettingsCommand::ScreenContext`]) are emitted WITHOUT consuming their
/// edge — their mirrors move only on a successful apply — so callers must
/// always run the apply half in the same heartbeat.
pub(crate) fn drain_settings_edges(
    flags: &SettingsFlags,
    config: &mut Config,
    settings: &mut SettingsState,
    prefs: &Prefs,
    focused_app: Option<&str>,
) -> Vec<SettingsCommand> {
    let mut commands = Vec::new();
    if let Some(on) = switch_edge(&flags.general_autocorrect, &mut config.autocorrect) {
        commands.push(SettingsCommand::Autocorrect { on });
    }
    if let Some(on) = switch_edge(
        &flags.general_full_autocorrect,
        &mut config.full_autocorrect,
    ) {
        commands.push(SettingsCommand::FullAutocorrect { on });
    }
    if let Some(on) = switch_edge(
        &flags.general_thesaurus_selection,
        &mut config.thesaurus_selection,
    ) {
        commands.push(SettingsCommand::ThesaurusSelection { on });
    }
    if let Some(on) = switch_edge(&flags.general_trailing_space, &mut config.trailing_space) {
        commands.push(SettingsCommand::TrailingSpace { on });
    }
    if let Some(on) = switch_edge(&flags.labs_midline, &mut settings.global_mid_word) {
        commands.push(SettingsCommand::Midline {
            on,
            allow: prefs.mid_line_enabled(focused_app, on),
        });
    }
    if let Some(on) = switch_edge(
        &flags.context_cross_app_previous_inputs,
        &mut config.cross_app_previous_inputs,
    ) {
        commands.push(SettingsCommand::CrossAppPreviousInputs { on });
    }
    if let Some(on) = switch_edge(&flags.context_clipboard, &mut config.clipboard_context) {
        commands.push(SettingsCommand::ClipboardContext { on });
    }
    if let Some(on) = emoji_switch_edge(
        &flags.emoji_enabled,
        &mut settings.emoji_enabled,
        &mut config.emoji,
        &mut settings.emoji_prefs,
    ) {
        commands.push(SettingsCommand::EmojiSwitch { on });
    }
    if let Some(tone) = emoji_skin_tone_edge(
        &flags.emoji_skin_tone_index,
        &mut settings.emoji_skin_tone_index,
        &mut config.emoji,
        &mut settings.emoji_prefs,
    ) {
        commands.push(SettingsCommand::EmojiSkinTone {
            value: crate::builders::emoji_skin_tone_value(tone),
        });
    }
    if let Some(gender) = emoji_gender_edge(
        &flags.emoji_gender_index,
        &mut settings.emoji_gender_index,
        &mut config.emoji,
        &mut settings.emoji_prefs,
    ) {
        commands.push(SettingsCommand::EmojiGender {
            value: crate::builders::emoji_gender_value(gender),
        });
    }
    let desired_launch = flags.general_launch_at_login.load(Ordering::Relaxed);
    if desired_launch != settings.current_launch_at_login {
        commands.push(SettingsCommand::LaunchAtLogin {
            desired: desired_launch,
        });
    }
    let desired_screen = flags.context_screen.load(Ordering::Relaxed);
    if desired_screen != config.screen_context {
        commands.push(SettingsCommand::ScreenContext {
            desired: desired_screen,
        });
    }
    commands
}

/// Apply a user launch-at-login change through the OS boundary before
/// persisting it. A rejected OS mutation restores both the loop's truth and
/// the shared UI atomic.
pub(crate) fn apply_launch_at_login_settings_edge(
    flag: &AtomicBool,
    current: &mut bool,
    shell: &dyn ShellHost,
    persist: impl FnOnce(bool),
) -> Result<Option<bool>, PlatformError> {
    let desired = flag.load(Ordering::Relaxed);
    if desired == *current {
        return Ok(None);
    }
    if let Err(err) = shell.set_launch_at_login(desired) {
        flag.store(*current, Ordering::Relaxed);
        return Err(err);
    }
    *current = desired;
    persist(desired);
    Ok(Some(desired))
}
