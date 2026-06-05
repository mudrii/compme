//! P5b: two-tap CGEventTap probe.
//!
//! This proves the split between a passive listen-only observer and an active
//! consuming tap. The observer toggles a precomputed `suggestion_visible` state
//! with F8. The consuming tap drops Tab only while that state is true.
//!
//! Production still needs A1b lifecycle work: create/enable the consuming tap
//! only while a real suggestion is visible, re-enable after disabled events,
//! and defer teardown after synthetic insertion. This probe intentionally keeps
//! both taps installed so the pinned `core-graphics` safe wrapper is enough.
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::event::{
    CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CallbackResult, EventField,
};

use spike::keys::should_swallow;

const KEYCODE_F8: i64 = 100;

fn add_to_runloop(tap: &CGEventTap<'static>) {
    let source = tap
        .mach_port()
        .create_runloop_source(0)
        .expect("runloop source");
    CFRunLoop::get_current().add_source(&source, unsafe { kCFRunLoopCommonModes });
    tap.enable();
}

fn main() {
    let visible = Arc::new(AtomicBool::new(false));

    let observer_state = Arc::clone(&visible);
    let observer = CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::ListenOnly,
        vec![CGEventType::KeyDown],
        move |_proxy, etype, event| {
            if matches!(
                etype,
                CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
            ) {
                eprintln!("observer tap disabled: {etype:?}");
                return CallbackResult::Keep;
            }

            let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
            if keycode == KEYCODE_F8 {
                let now_visible = !observer_state.load(Ordering::Relaxed);
                observer_state.store(now_visible, Ordering::Relaxed);
                println!("listen-only: F8 toggled suggestion_visible={now_visible}");
            } else {
                println!("listen-only: observed keycode {keycode}");
            }
            let _ = io::stdout().flush();
            CallbackResult::Keep
        },
    )
    .expect("listen-only tap create failed; grant Input Monitoring + Accessibility, relaunch");

    let consumer_state = Arc::clone(&visible);
    let consumer = CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        vec![CGEventType::KeyDown],
        move |_proxy, etype, event| {
            if matches!(
                etype,
                CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
            ) {
                eprintln!("consumer tap disabled: {etype:?}");
                return CallbackResult::Keep;
            }

            let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
            let suggestion_visible = consumer_state.load(Ordering::Relaxed);
            if should_swallow(keycode, suggestion_visible) {
                println!("consumer: Tab swallowed while suggestion_visible=true");
                let _ = io::stdout().flush();
                CallbackResult::Drop
            } else {
                println!(
                    "consumer: keycode {keycode} passed (suggestion_visible={suggestion_visible})"
                );
                let _ = io::stdout().flush();
                CallbackResult::Keep
            }
        },
    )
    .expect("consumer tap create failed; grant Input Monitoring + Accessibility, relaunch");

    add_to_runloop(&observer);
    add_to_runloop(&consumer);

    println!(
        "Two-tap probe active. F8 toggles simulated suggestion visibility. \
         Tab should pass when false and be swallowed when true. Ctrl-C to stop."
    );
    CFRunLoop::run_current();
}
