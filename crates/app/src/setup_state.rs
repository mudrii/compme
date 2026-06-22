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
    /// `COMPME_SCREEN_CONTEXT` is on — only then is the Screen Recording
    /// permission a setup requirement at all (default-off feature must not
    /// nag for a permission it will never use).
    pub screen_context_enabled: bool,
    /// `CGPreflightScreenCaptureAccess()` — Screen Recording permission.
    pub screen_recording: bool,
    /// The resolved model source loaded successfully, or the acceptance stub is
    /// configured.
    pub model_ready: bool,
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
/// offers Reveal-in-Finder only while usable (nothing useful to reveal
/// otherwise).
pub fn setup_rows(checks: SetupChecks) -> Vec<SetupRow> {
    let mut rows = vec![SetupRow {
        label: "Accessibility",
        ready: checks.ax_trusted,
        action: (!checks.ax_trusted).then_some(SetupAction::GrantAccessibility),
    }];
    if checks.screen_context_enabled {
        rows.push(SetupRow {
            label: "Screen Recording",
            ready: checks.screen_recording,
            action: (!checks.screen_recording).then_some(SetupAction::RequestScreenRecording),
        });
    }
    rows.push(SetupRow {
        label: "Model file",
        ready: checks.model_ready,
        action: checks.model_ready.then_some(SetupAction::RevealModel),
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_recording_row_is_omitted_when_screen_context_is_off() {
        // COMPME_SCREEN_CONTEXT defaults off; the permission is only needed
        // when the feature is on, so a default-config run must not nag.
        let rows = setup_rows(SetupChecks {
            ax_trusted: true,
            screen_context_enabled: false,
            screen_recording: false,
            model_ready: true,
        });
        assert!(rows.iter().all(|r| r.label != "Screen Recording"));
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn setup_rows_offer_actions_only_where_they_make_sense() {
        // All good: no permission prompts, model still revealable.
        let ready = setup_rows(SetupChecks {
            ax_trusted: true,
            screen_context_enabled: true,
            screen_recording: true,
            model_ready: true,
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
            screen_context_enabled: true,
            screen_recording: false,
            model_ready: false,
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
