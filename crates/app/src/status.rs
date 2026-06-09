//! Application status — the single derived state that drives both the tray UI
//! and suggestion gating.
//!
//! Kept pure (no AppKit, no atomics) so the policy is unit-tested and the tray
//! only ever renders precomputed strings.

/// Why suggestions are blocked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockReason {
    /// Accessibility permission not granted.
    Permission,
    /// Secure input is active (password field / global secure input).
    SecureInput,
}

/// The app's current state, in priority order of severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppStatus {
    Loading,
    Ready,
    Disabled,
    Blocked(BlockReason),
}

/// Derive the status from the four inputs, most-severe first.
///
/// A missing permission outranks everything (nothing works without it); secure
/// input outranks readiness (we must not suggest into a password field); an
/// unwarmed model outranks the user toggle (there is nothing to offer yet); a
/// disabled toggle outranks `Ready`.
pub fn derive_status(trusted: bool, secure: bool, ready: bool, enabled: bool) -> AppStatus {
    if !trusted {
        AppStatus::Blocked(BlockReason::Permission)
    } else if secure {
        AppStatus::Blocked(BlockReason::SecureInput)
    } else if !ready {
        AppStatus::Loading
    } else if !enabled {
        AppStatus::Disabled
    } else {
        AppStatus::Ready
    }
}

impl AppStatus {
    /// Only `Ready` permits inference requests to be submitted.
    pub fn suggestions_allowed(self) -> bool {
        matches!(self, AppStatus::Ready)
    }

    /// True when the "Open Accessibility Settings" affordance should be offered.
    pub fn needs_accessibility(self) -> bool {
        matches!(self, AppStatus::Blocked(BlockReason::Permission))
    }

    /// Short menu-bar button label.
    pub fn menu_title(self) -> &'static str {
        match self {
            AppStatus::Loading => "CM…",
            AppStatus::Ready => "CM",
            AppStatus::Disabled => "CM⏸",
            AppStatus::Blocked(_) => "CM⚠",
        }
    }

    /// One-line human description shown as the disabled status row in the menu.
    pub fn status_line(self) -> &'static str {
        match self {
            AppStatus::Loading => "Loading model…",
            AppStatus::Ready => "Ready",
            AppStatus::Disabled => "Disabled",
            AppStatus::Blocked(BlockReason::Permission) => "Blocked: grant Accessibility",
            AppStatus::Blocked(BlockReason::SecureInput) => "Paused: secure input active",
        }
    }

    /// [`Self::menu_title`] with the snooze overlay: snooze is a PREFS gate
    /// (not an `AppStatus` — `suggestions_allowed` is untouched and the submit
    /// gate keeps doing the blocking), so it renders only over `Ready` and
    /// never masks a more severe state.
    pub fn render_title(self, snoozed: bool) -> &'static str {
        match self {
            AppStatus::Ready if snoozed => "CM💤",
            other => other.menu_title(),
        }
    }

    /// [`Self::status_line`] with the snooze overlay; see [`Self::render_title`].
    pub fn render_line(self, snoozed: bool) -> &'static str {
        match self {
            AppStatus::Ready if snoozed => "Snoozed for up to 1 hour",
            other => other.status_line(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_permission_outranks_everything() {
        // Even ready + enabled + not-secure: no trust → Blocked(Permission).
        assert_eq!(
            derive_status(false, false, true, true),
            AppStatus::Blocked(BlockReason::Permission)
        );
        // Trust missing wins over secure too.
        assert_eq!(
            derive_status(false, true, false, false),
            AppStatus::Blocked(BlockReason::Permission)
        );
    }

    #[test]
    fn secure_input_outranks_readiness_and_toggle() {
        assert_eq!(
            derive_status(true, true, true, true),
            AppStatus::Blocked(BlockReason::SecureInput)
        );
    }

    #[test]
    fn secure_input_outranks_an_unwarmed_model() {
        // Trusted but secure input is active while the model is still warming
        // (ready=false). The docstring promises "secure input outranks
        // readiness", so this must report Blocked(SecureInput), NOT Loading —
        // otherwise a branch reorder (check readiness before secure) would
        // silently surface "Loading model…" while focus sits in a password
        // field, hiding the secure-input pause. The toggle is irrelevant here.
        assert_eq!(
            derive_status(true, true, false, true),
            AppStatus::Blocked(BlockReason::SecureInput)
        );
        assert_eq!(
            derive_status(true, true, false, false),
            AppStatus::Blocked(BlockReason::SecureInput)
        );
    }

    #[test]
    fn not_ready_is_loading_when_trusted_and_unsecured() {
        assert_eq!(derive_status(true, false, false, true), AppStatus::Loading);
    }

    #[test]
    fn ready_but_disabled_is_disabled() {
        assert_eq!(derive_status(true, false, true, false), AppStatus::Disabled);
    }

    #[test]
    fn all_clear_is_ready() {
        assert_eq!(derive_status(true, false, true, true), AppStatus::Ready);
    }

    #[test]
    fn only_ready_allows_suggestions() {
        assert!(AppStatus::Ready.suggestions_allowed());
        for s in [
            AppStatus::Loading,
            AppStatus::Disabled,
            AppStatus::Blocked(BlockReason::Permission),
            AppStatus::Blocked(BlockReason::SecureInput),
        ] {
            assert!(!s.suggestions_allowed(), "{s:?} must not allow suggestions");
        }
    }

    #[test]
    fn only_permission_block_needs_accessibility_affordance() {
        assert!(AppStatus::Blocked(BlockReason::Permission).needs_accessibility());
        assert!(!AppStatus::Blocked(BlockReason::SecureInput).needs_accessibility());
        assert!(!AppStatus::Ready.needs_accessibility());
    }

    #[test]
    fn snoozed_renders_only_over_ready() {
        // Display-only: snooze is a prefs gate, not an AppStatus — but the
        // user needs visible feedback. It must never mask a more severe state.
        assert_eq!(AppStatus::Ready.render_title(true), "CM💤");
        assert_eq!(
            AppStatus::Ready.render_line(true),
            "Snoozed for up to 1 hour"
        );
        // Not snoozed → plain Ready strings.
        assert_eq!(AppStatus::Ready.render_title(false), "CM");
        assert_eq!(AppStatus::Ready.render_line(false), "Ready");
        // Severer states win even while snoozed.
        for s in [
            AppStatus::Loading,
            AppStatus::Disabled,
            AppStatus::Blocked(BlockReason::Permission),
            AppStatus::Blocked(BlockReason::SecureInput),
        ] {
            assert_eq!(s.render_title(true), s.menu_title(), "{s:?}");
            assert_eq!(s.render_line(true), s.status_line(), "{s:?}");
        }
    }

    #[test]
    fn every_status_has_renderable_title_and_line() {
        let statuses = [
            AppStatus::Loading,
            AppStatus::Ready,
            AppStatus::Disabled,
            AppStatus::Blocked(BlockReason::Permission),
            AppStatus::Blocked(BlockReason::SecureInput),
        ];
        for s in statuses {
            assert!(!s.menu_title().is_empty());
            assert!(!s.status_line().is_empty());
        }
    }
}
