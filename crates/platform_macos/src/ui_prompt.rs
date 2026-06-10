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

/// Show "Allow this link to <action> for <scope>?" and return whether the
/// user clicked Allow. Main-thread only. Cancel is the FIRST button (the
/// default/safe answer — Return key declines).
pub fn confirm_deep_link_prompt(
    scope: &str,
    action: &str,
    trust: &str,
) -> Result<bool, PlatformError> {
    let mtm = MainThreadMarker::new().ok_or_else(|| PlatformError::CannotComplete {
        reason: "deep-link prompt requires the main thread".into(),
    })?;
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str("Allow configuration change?"));
    alert.setInformativeText(&NSString::from_str(&format!(
        "A compme:// link wants to apply {action} for:\n{scope}\n({trust})"
    )));
    // First button = default (Return). Safe default = Cancel.
    let _ = alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    let _ = alert.addButtonWithTitle(&NSString::from_str("Allow"));
    let response = alert.runModal();
    if crate::debug_enabled() {
        eprintln!("compme: prompt response={response:?} (first={NSAlertFirstButtonReturn:?})");
    }
    // First button returns NSAlertFirstButtonReturn (Cancel); Allow is +1.
    Ok(response == NSAlertFirstButtonReturn + 1)
}

/// Confirm deleting one app's recorded-input history (Apps tab Delete —
/// irreversible: secure_delete zeroes the freed pages). Same shape as the
/// deep-link prompt: Cancel is the first/default button.
pub fn confirm_delete_app_prompt(app: &str) -> Result<bool, PlatformError> {
    let mtm = MainThreadMarker::new().ok_or_else(|| PlatformError::CannotComplete {
        reason: "delete prompt requires the main thread".into(),
    })?;
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str("Delete recorded inputs?"));
    alert.setInformativeText(&NSString::from_str(&format!(
        "All recorded inputs for {app} will be permanently erased."
    )));
    let _ = alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    let _ = alert.addButtonWithTitle(&NSString::from_str("Delete"));
    let response = alert.runModal();
    Ok(response == NSAlertFirstButtonReturn + 1)
}
