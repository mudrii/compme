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

/// Generic ShellHost confirmation. Main-thread only; Cancel is the
/// FIRST/default button (Return declines), matching the named prompts below.
pub fn confirm_prompt(
    title: &str,
    message: &str,
    confirm_label: &str,
) -> Result<bool, PlatformError> {
    run_confirm(
        "confirm prompt requires the main thread",
        title,
        message,
        confirm_label,
    )
}
