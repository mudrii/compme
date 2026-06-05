//! P5: consuming CGEventTap that SWALLOWS the accept key, passes all other keys.
//!
//! TDD wiring: the swallow decision is made by the tested `spike::keys::should_swallow`
//! seam — NOT an inline keycode check. The probe hardcodes `suggestion_visible = true`
//! (a running app would track real suggestion state); the lib then swallows only the
//! accept key (`spike::keys::KEYCODE_TAB`) and passes everything else.
//!
//! Proves global interception + that an instant callback doesn't stall other apps.
//! NOTE: blocks on `CFRunLoop::run_current()` forever (Ctrl-C to stop). The autonomous
//! gate is: it compiles. Stall-free behaviour is human-verified per MANUAL-ACCEPTANCE.md.
use std::io::{self, Write};

use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::event::{
    CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CallbackResult, EventField,
};

use spike::keys::{should_swallow, KEYCODE_TAB};

fn main() {
    // core-graphics 0.25: the callback returns `CallbackResult` (Keep/Drop/Replace),
    // not `Option<CGEvent>`. `CGEventTap::new` requires a `Send + 'static` callback.
    let tap = CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default, // active/consuming
        vec![CGEventType::KeyDown],
        |_proxy, _etype, event| {
            // Callback MUST stay non-blocking — slow callbacks get the tap auto-disabled
            // and stall global input. Decide from pre-computed state only.
            // core-graphics 0.25: the keycode field key is `EventField::KEYBOARD_EVENT_KEYCODE`
            // (a const on the `EventField` struct); `CGEventField` is just `type = u32`.
            let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
            // Probe hardcodes suggestion_visible = true; the lib owns the predicate.
            if should_swallow(keycode, true) {
                println!("keycode {keycode} (accept key) -> SWALLOWED");
                let _ = io::stdout().flush();
                CallbackResult::Drop
            } else {
                println!("key {keycode} -> passed");
                let _ = io::stdout().flush();
                CallbackResult::Keep
            }
        },
    )
    .expect("tap create failed — grant Input Monitoring + Accessibility, relaunch terminal");

    let current = CFRunLoop::get_current();
    // core-graphics 0.25: `mach_port` is now a private field exposed via `mach_port()`.
    let source = tap
        .mach_port()
        .create_runloop_source(0)
        .expect("runloop source");
    // core-foundation 0.10: `add_source` is safe; `kCFRunLoopCommonModes` is an extern
    // static so reading it is the only `unsafe` here.
    current.add_source(&source, unsafe { kCFRunLoopCommonModes });
    tap.enable();
    println!(
        "Tap active. Tab (keycode {KEYCODE_TAB}) = swallowed, others = passed. \
         Type in ANOTHER app to confirm no stall. Ctrl-C to stop."
    );
    CFRunLoop::run_current();
}
