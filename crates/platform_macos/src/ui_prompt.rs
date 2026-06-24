//! Blocking confirmation prompt for deep links (§16 mandatory host
//! confirmation). `NSAlert::runModal` runs a NESTED run-loop mode, so the
//! main thread keeps pumping (platform callbacks still enqueue; further
//! deep links queue FIFO and prompt after this one). AppKit/FFI glue:
//! build- and live-verified, not unit-tested (tray convention) — the
//! prompt-or-not DECISION is the pure, tested part (`webconfig`).

use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn};
use objc2_foundation::NSString;
use platform::PlatformError;

/// Shared NSAlert confirmation. Cancel is the FIRST/default button (Return
/// declines); `confirm_label` is the second. Returns whether the user chose
/// the second button. Main-thread only; `runModal` runs a nested run loop.
fn run_confirm(
    main_thread_reason: &str,
    message: &str,
    informative: &str,
    confirm_label: &str,
) -> Result<bool, PlatformError> {
    let mtm = MainThreadMarker::new().ok_or_else(|| PlatformError::CannotComplete {
        reason: main_thread_reason.into(),
    })?;
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(message));
    alert.setInformativeText(&NSString::from_str(informative));
    // First button = default (Return). Safe default = Cancel.
    let _ = alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    let _ = alert.addButtonWithTitle(&NSString::from_str(confirm_label));
    let response = alert.runModal();
    if crate::debug_enabled() {
        eprintln!("compme: prompt response={response:?} (first={NSAlertFirstButtonReturn:?})");
    }
    // First button returns NSAlertFirstButtonReturn (Cancel); confirm is +1.
    Ok(response == NSAlertFirstButtonReturn + 1)
}

/// Show "Allow this link to <action> for <scope>?" and return whether the
/// user clicked Allow. Main-thread only. Cancel is the FIRST button (the
/// default/safe answer — Return key declines).
pub fn confirm_deep_link_prompt(
    scope: &str,
    action: &str,
    trust: &str,
) -> Result<bool, PlatformError> {
    run_confirm(
        "deep-link prompt requires the main thread",
        "Allow configuration change?",
        &format!("A compme:// link wants to apply {action} for:\n{scope}\n({trust})"),
        "Allow",
    )
}

/// Click-through license gate before a model download (D14, c95 "once per
/// model"). Same shape as the other prompts: main-thread only, Cancel is
/// the FIRST/default button (Return declines), nested run loop while modal.
pub fn confirm_license_prompt(
    model: &str,
    license_name: &str,
    terms_url: &str,
) -> Result<bool, PlatformError> {
    run_confirm(
        "license prompt requires the main thread",
        "Accept model license?",
        &format!(
            "{model} is distributed under the {license_name}.\n\
             Downloading requires accepting its terms:\n{terms_url}"
        ),
        "Accept",
    )
}

/// Confirm deleting one app's recorded-input history (Apps tab Delete —
/// irreversible: secure_delete zeroes the freed pages). Same shape as the
/// deep-link prompt: Cancel is the first/default button.
pub fn confirm_delete_app_prompt(app: &str) -> Result<bool, PlatformError> {
    run_confirm(
        "delete prompt requires the main thread",
        "Delete recorded inputs?",
        &format!("All recorded inputs for {app} will be permanently erased."),
        "Delete",
    )
}
