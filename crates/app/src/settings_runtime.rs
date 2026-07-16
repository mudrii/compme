//! Settings edge detection and live-application policy.
//!
//! Callers provide persistence and engine callbacks. This module guarantees
//! that each setting is applied once per edge and that OS-backed changes are
//! committed before their persisted value.

use std::sync::atomic::{AtomicBool, Ordering};

use platform::{shell::ShellHost, PlatformError};
use prefs::Prefs;

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
