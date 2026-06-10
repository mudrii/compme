//! Setup-pane row model (A3 settings, c91 design / c97 verification).
//!
//! Pure core: permission/model probe results go in, renderable row states come
//! out. The FFI side (`accessibility_trusted`, `screen_recording_permission`,
//! `Path::exists`, the prompt-triggering variants) stays in the run loop and
//! the window; this module never touches AppKit, so every row rule is
//! unit-testable.

/// Probe results the run loop gathers before showing the Setup pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetupChecks {
    /// `AXIsProcessTrusted()` — Accessibility permission.
    pub ax_trusted: bool,
    /// `CGPreflightScreenCaptureAccess()` — Screen Recording permission.
    pub screen_recording: bool,
    /// The resolved model file exists on disk.
    pub model_exists: bool,
}

/// The button a row offers, if any. The window maps these to the
/// prompt-triggering FFI calls (`prompt_accessibility_trust`,
/// `request_screen_recording_permission`, reveal-in-Finder).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupAction {
    GrantAccessibility,
    RequestScreenRecording,
    RevealModel,
}

/// One renderable Setup row: fixed label, ready state, optional action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetupRow {
    pub label: &'static str,
    pub ready: bool,
    pub action: Option<SetupAction>,
}

/// The Setup pane's rows for a set of probe results, top to bottom.
/// Permission rows offer their prompt only while missing; the model row
/// offers Reveal-in-Finder only while present (nothing to reveal otherwise).
pub fn setup_rows(checks: SetupChecks) -> Vec<SetupRow> {
    vec![
        SetupRow {
            label: "Accessibility",
            ready: checks.ax_trusted,
            action: (!checks.ax_trusted).then_some(SetupAction::GrantAccessibility),
        },
        SetupRow {
            label: "Screen Recording",
            ready: checks.screen_recording,
            action: (!checks.screen_recording).then_some(SetupAction::RequestScreenRecording),
        },
        SetupRow {
            label: "Model file",
            ready: checks.model_exists,
            action: checks.model_exists.then_some(SetupAction::RevealModel),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_rows_offer_actions_only_where_they_make_sense() {
        // All good: no permission prompts, model still revealable.
        let ready = setup_rows(SetupChecks {
            ax_trusted: true,
            screen_recording: true,
            model_exists: true,
        });
        assert_eq!(
            ready,
            vec![
                SetupRow {
                    label: "Accessibility",
                    ready: true,
                    action: None,
                },
                SetupRow {
                    label: "Screen Recording",
                    ready: true,
                    action: None,
                },
                SetupRow {
                    label: "Model file",
                    ready: true,
                    action: Some(SetupAction::RevealModel),
                },
            ]
        );

        // Nothing granted, model missing: prompts offered, nothing to reveal.
        let missing = setup_rows(SetupChecks {
            ax_trusted: false,
            screen_recording: false,
            model_exists: false,
        });
        assert_eq!(
            missing,
            vec![
                SetupRow {
                    label: "Accessibility",
                    ready: false,
                    action: Some(SetupAction::GrantAccessibility),
                },
                SetupRow {
                    label: "Screen Recording",
                    ready: false,
                    action: Some(SetupAction::RequestScreenRecording),
                },
                SetupRow {
                    label: "Model file",
                    ready: false,
                    action: None,
                },
            ]
        );
    }
}
