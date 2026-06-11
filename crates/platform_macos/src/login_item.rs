//! Launch-at-login via `SMAppService` (A3 §9 D13: default-off, toggleable).
//!
//! Only meaningful when the process runs from an app BUNDLE
//! (`SMAppService.mainApp` registers the containing bundle); a bare `cargo
//! run` binary gets an error from the framework, which callers treat as
//! non-fatal. AppKit/FFI glue: build- and live-verified, not unit-tested
//! (the tray convention); the decide-to-call policy is the unit-tested part
//! (`Config.launch_at_login` tri-state in the app crate).

use objc2_service_management::SMAppService;
use platform::PlatformError;

/// Register (`true`) or unregister (`false`) this app as a login item.
pub fn set_launch_at_login(enabled: bool) -> Result<(), PlatformError> {
    // SAFETY: SMAppService class methods are plain ObjC calls with no
    // pointer arguments; mainAppService returns a retained service object
    // and register/unregister only read it. Bundle-context requirements are
    // a runtime error (returned via *AndReturnError), not UB.
    let service = unsafe { SMAppService::mainAppService() };
    let result = if enabled {
        unsafe { service.registerAndReturnError() }
    } else {
        unsafe { service.unregisterAndReturnError() }
    };
    result.map_err(|err| PlatformError::CannotComplete {
        reason: format!(
            "launch-at-login {} failed: {err}",
            if enabled { "register" } else { "unregister" }
        ),
    })
}
