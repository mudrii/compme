//! `compme://` deep-link reception via `NSAppleEventManager` (`kAEGetURL`).
//!
//! Launch Services routes URL opens for the scheme declared in the bundle's
//! `CFBundleURLTypes` (tools/bundle) as Apple Events; this installs the
//! process-side handler. The handler only EXTRACTS the URL string and hands
//! it to the injected callback (which enqueues for the run loop) — parsing,
//! trust, and policy live in the pure `webconfig`/app layers. AppKit/FFI
//! glue: build- and live-verified, not unit-tested (the tray convention).

use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSObjectProtocol};
use platform::PlatformError;

/// `kInternetEventClass` == `kAEGetURL` == 'GURL'.
const GURL: u32 = 0x4755_524C;
/// `keyDirectObject` == '----'.
const KEY_DIRECT_OBJECT: u32 = 0x2D2D_2D2D;

type UrlCallback = dyn Fn(String) + Send + Sync + 'static;

struct UrlTargetIvars {
    on_url: Arc<UrlCallback>,
}

define_class!(
    // SAFETY: a plain NSObject subclass used only as an Apple Events handler
    // target; the method extracts a string and calls a Rust closure.
    #[unsafe(super = objc2_foundation::NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = UrlTargetIvars]
    struct UrlTarget;

    unsafe impl NSObjectProtocol for UrlTarget {}

    impl UrlTarget {
        #[unsafe(method(handleGetURL:withReplyEvent:))]
        fn handle_get_url(
            &self,
            event: &NSAppleEventDescriptor,
            _reply: Option<&NSAppleEventDescriptor>,
        ) {
            if let Some(url) = event
                .paramDescriptorForKeyword(KEY_DIRECT_OBJECT)
                .and_then(|direct| direct.stringValue())
            {
                // objc2→Rust FFI boundary: a panic unwinding into the objc
                // `handleGetURL:withReplyEvent:` dispatch is UB-adjacent
                // (objc2 0.6.4 turns it into abort()). Shield the injected
                // callback, matching the catch_unwind convention every other
                // FFI entry in lib.rs uses.
                let on_url = Arc::clone(&self.ivars().on_url);
                let url = url.to_string();
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    on_url(url);
                }));
            }
        }
    }
);

/// Keeps the handler target alive; dropping it leaves a dangling Apple Events
/// registration, so hold it for the process lifetime (run loop owns it).
pub struct UrlEventHandler {
    _target: Retained<UrlTarget>,
}

/// Install the `kAEGetURL` handler. Main-thread only (Apple Events dispatch
/// on the main run loop, which the heartbeat pumps).
pub fn install_url_event_handler(
    on_url: Arc<UrlCallback>,
) -> Result<UrlEventHandler, PlatformError> {
    let mtm = MainThreadMarker::new().ok_or_else(|| PlatformError::CannotComplete {
        reason: "url handler must be installed on the main thread".into(),
    })?;
    let target = UrlTarget::alloc(mtm).set_ivars(UrlTargetIvars { on_url });
    // SAFETY: NSObject's init signature is correct for this subclass.
    let target: Retained<UrlTarget> = unsafe { msg_send![super(target), init] };
    // SAFETY: the selector exists on UrlTarget (defined above) with the
    // handler signature Apple Events expects; the target is kept alive by the
    // returned guard for the registration's lifetime.
    unsafe {
        NSAppleEventManager::sharedAppleEventManager()
            .setEventHandler_andSelector_forEventClass_andEventID(
                {
                    let any: &AnyObject = target.as_ref();
                    any
                },
                sel!(handleGetURL:withReplyEvent:),
                GURL,
                GURL,
            );
    }
    Ok(UrlEventHandler { _target: target })
}
