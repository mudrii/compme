//! `compme://` deep-link reception via `NSAppleEventManager` (`kAEGetURL`).
//!
//! Launch Services routes URL opens for the scheme declared in the bundle's
//! `CFBundleURLTypes` (tools/bundle) as Apple Events; this installs the
//! process-side handler. The handler only EXTRACTS the URL string and hands
//! it to the injected callback (which enqueues for the run loop) — parsing,
//! trust, and policy live in the pure `webconfig`/app layers. AppKit/FFI
//! glue: build- and live-verified, not unit-tested (the tray convention).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSObjectProtocol};
pub use platform::shell::UrlCallback;
use platform::PlatformError;

/// `kInternetEventClass` == `kAEGetURL` == 'GURL'.
const GURL: u32 = 0x4755_524C;
/// `keyDirectObject` == '----'.
const KEY_DIRECT_OBJECT: u32 = 0x2D2D_2D2D;

/// Decode and dispatch one `kAEGetURL` event.
///
/// Returns `true` when the event carried a string direct object. Callback
/// panics are contained so they cannot unwind through the Objective-C event
/// manager boundary.
pub fn dispatch_gurl_event(event: &NSAppleEventDescriptor, on_url: &UrlCallback) -> bool {
    let Some(url) = event
        .paramDescriptorForKeyword(KEY_DIRECT_OBJECT)
        .and_then(|direct| direct.stringValue())
    else {
        return false;
    };
    let url = url.to_string();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        on_url(url);
    }));
    true
}

struct UrlTargetIvars {
    on_url: Arc<UrlCallback>,
}

/// Tracks which guard owns the process-wide GURL/GURL registration.
///
/// Apple Event handler installation is main-thread-only, but the atomic slot
/// keeps the ownership rule independently testable: replacing a registration
/// makes older guards stale, and only the current guard may unregister it.
struct UrlHandlerSlot {
    current_owner: AtomicU64,
}

impl UrlHandlerSlot {
    const fn new() -> Self {
        Self {
            current_owner: AtomicU64::new(0),
        }
    }

    fn arm(&self, owner: u64) {
        debug_assert_ne!(owner, 0);
        self.current_owner.store(owner, Ordering::Release);
    }

    fn disarm_if_current(&self, owner: u64, unregister: impl FnOnce()) -> bool {
        if self
            .current_owner
            .compare_exchange(owner, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        unregister();
        true
    }
}

static URL_HANDLER_SLOT: UrlHandlerSlot = UrlHandlerSlot::new();
static NEXT_URL_HANDLER_OWNER: AtomicU64 = AtomicU64::new(1);

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
            dispatch_gurl_event(event, self.ivars().on_url.as_ref());
        }
    }
);

/// Owns one process-wide GURL/GURL registration and keeps its target alive.
///
/// The target is main-thread-only, so the guard must also be dropped on the
/// main thread. Dropping the current guard unregisters the Apple Event handler;
/// dropping a stale guard from an older installation leaves the newer handler
/// untouched.
pub struct UrlEventHandler {
    owner: u64,
    _target: Retained<UrlTarget>,
}

impl Drop for UrlEventHandler {
    fn drop(&mut self) {
        let on_main_thread = MainThreadMarker::new().is_some();
        debug_assert!(
            on_main_thread,
            "URL event handler must be dropped on the main thread"
        );
        if !on_main_thread {
            return;
        }
        URL_HANDLER_SLOT.disarm_if_current(self.owner, || {
            NSAppleEventManager::sharedAppleEventManager()
                .removeEventHandlerForEventClass_andEventID(GURL, GURL);
        });
    }
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
    let owner = NEXT_URL_HANDLER_OWNER.fetch_add(1, Ordering::Relaxed);
    URL_HANDLER_SLOT.arm(owner);
    Ok(UrlEventHandler {
        owner,
        _target: target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::NSString;
    use std::sync::Mutex;

    fn event_with_url(url: Option<&str>) -> Retained<NSAppleEventDescriptor> {
        let event = NSAppleEventDescriptor::recordDescriptor();
        if let Some(url) = url {
            let direct = NSAppleEventDescriptor::descriptorWithString(&NSString::from_str(url));
            event.setParamDescriptor_forKeyword(&direct, KEY_DIRECT_OBJECT);
        }
        event
    }

    #[test]
    fn gurl_event_delivers_the_exact_unicode_url() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let callback_received = Arc::clone(&received);
        let callback = move |url| callback_received.lock().unwrap().push(url);
        let event = event_with_url(Some("compme://setOverride?app=文本&enabled=true"));

        assert!(dispatch_gurl_event(&event, &callback));
        assert_eq!(
            *received.lock().unwrap(),
            ["compme://setOverride?app=文本&enabled=true"]
        );
    }

    #[test]
    fn gurl_event_without_a_string_direct_object_is_ignored() {
        let called = Arc::new(Mutex::new(false));
        let callback_called = Arc::clone(&called);
        let callback = move |_| *callback_called.lock().unwrap() = true;
        let event = event_with_url(None);

        assert!(!dispatch_gurl_event(&event, &callback));
        assert!(!*called.lock().unwrap());
    }

    #[test]
    fn gurl_callback_panic_is_contained_at_the_event_boundary() {
        let event = event_with_url(Some("compme://setOverride?enabled=true"));

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            dispatch_gurl_event(&event, &|_| panic!("callback panic"))
        }));

        assert!(matches!(outcome, Ok(true)));
    }

    #[test]
    fn current_url_handler_owner_disarms_exactly_once() {
        let slot = UrlHandlerSlot::new();
        slot.arm(41);
        let mut unregisters = 0;

        assert!(slot.disarm_if_current(41, || unregisters += 1));
        assert!(!slot.disarm_if_current(41, || unregisters += 1));
        assert_eq!(unregisters, 1);
    }

    #[test]
    fn stale_url_handler_owner_cannot_disarm_newer_registration() {
        let slot = UrlHandlerSlot::new();
        slot.arm(41);
        slot.arm(42);
        let mut unregisters = 0;

        assert!(!slot.disarm_if_current(41, || unregisters += 1));
        assert!(slot.disarm_if_current(42, || unregisters += 1));
        assert_eq!(unregisters, 1);
    }
}
