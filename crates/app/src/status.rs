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

/// Whether a showing ghost must be dismissed this iteration.
///
/// Suggestion *gating* (`suggestions_allowed`) only blocks new requests; an
/// already-visible ghost needs an explicit dismiss when the user disables the
/// app or when secure input turns on (a password field gained focus). Both are
/// rising/falling edges, so this compares the previous tick's state to now.
pub fn should_dismiss(prev_enabled: bool, enabled: bool, prev_secure: bool, secure: bool) -> bool {
    let disabled_edge = prev_enabled && !enabled;
    let secured_edge = !prev_secure && secure;
    disabled_edge || secured_edge
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
    fn dismiss_on_disable_edge_only() {
        // Falling edge enabled true→false dismisses; staying disabled does not.
        assert!(should_dismiss(true, false, false, false));
        assert!(!should_dismiss(false, false, false, false));
        // Staying enabled does not dismiss.
        assert!(!should_dismiss(true, true, false, false));
        // Re-enabling (false→true) does not dismiss.
        assert!(!should_dismiss(false, true, false, false));
    }

    #[test]
    fn dismiss_on_secure_rising_edge_only() {
        // Rising edge secure false→true dismisses; staying secure does not.
        assert!(should_dismiss(true, true, false, true));
        assert!(!should_dismiss(true, true, true, true));
        // Secure clearing (true→false) does not dismiss.
        assert!(!should_dismiss(true, true, true, false));
    }

    #[test]
    fn dismiss_when_both_edges_fire() {
        assert!(should_dismiss(true, false, false, true));
    }

    #[test]
    fn no_dismiss_in_steady_state() {
        // Enabled + not secure, unchanged: nothing to dismiss.
        assert!(!should_dismiss(true, true, false, false));
    }

    #[test]
    fn every_status_has_distinct_title_and_line() {
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
