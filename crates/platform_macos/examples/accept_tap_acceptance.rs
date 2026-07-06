use std::env;
use std::process;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, KeyCode};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use platform::{AcceptAction, PlatformAdapter, TapControl};
use platform_macos::MacosPlatformAdapter;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
}

/// Grave/backtick (key above Tab). Must match the engine's accept binding:
/// Tab accepts the next word, grave accepts the full completion.
const KEYCODE_GRAVE: u16 = 50;
const KEYCODE_ESCAPE: u16 = 53;
const KEYCODE_GRAMMAR_ACCEPT: u16 = 96;
const KEYCODE_DOWN: u16 = 125;

fn main() {
    let duration = env::args()
        .nth(1)
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(4));
    let requirement = env::args().nth(2).unwrap_or_else(|| "full".into());

    if env_truthy("COMPME_ACCEPTANCE_REQUIRE_INPUT_MONITORING_REVOKED") {
        let granted = unsafe { CGPreflightListenEventAccess() };
        println!("INPUT_MONITORING granted={granted}");
        if granted {
            eprintln!("Input Monitoring is granted; revoke it before running this gate");
            process::exit(1);
        }
    }

    let adapter = match MacosPlatformAdapter::new() {
        Ok(adapter) => adapter,
        Err(err) => {
            eprintln!("failed to create macOS adapter: {err:?}");
            process::exit(2);
        }
    };

    println!("front_app={:?}", adapter.front_app());

    let controls = Arc::new(Mutex::new(Vec::new()));
    let controls_for_callback = Arc::clone(&controls);
    let subscription = match adapter.subscribe_accept(Arc::new(move |control| {
        println!("ACCEPT_ACTION {control:?}");
        controls_for_callback
            .lock()
            .expect("controls")
            .push(control);
    })) {
        Ok(subscription) => subscription,
        Err(err) => {
            eprintln!("failed to subscribe accept: {err:?}");
            process::exit(2);
        }
    };

    match requirement.as_str() {
        "inactive" => subscription
            .set_suggestion_visible(false)
            .expect("set inactive"),
        "full" | "escape" | "option-tab" | "cycle" => {
            subscription
                .set_suggestion_visible(true)
                .expect("show suggestion");
            subscription
                .set_accept_action(Some(AcceptAction::Full))
                .expect("arm full accept");
        }
        "correction" | "correction-tab" | "correction-full" | "correction-escape"
        | "correction-cycle" => {
            platform_macos::set_accept_keymap_from_config_with_mods(
                None,
                None,
                Some((KEYCODE_GRAMMAR_ACCEPT.into(), 0)),
            )
            .expect("configure grammar accept key");
            subscription
                .set_suggestion_visible(true)
                .expect("show correction");
            subscription
                .set_accept_action(Some(AcceptAction::Correction))
                .expect("arm correction accept");
        }
        "word" => {
            subscription
                .set_suggestion_visible(true)
                .expect("show word suggestion");
            subscription
                .set_accept_action(Some(AcceptAction::Word))
                .expect("arm word accept");
        }
        "delayed-hide" => {
            subscription
                .set_suggestion_visible(true)
                .expect("show suggestion");
            let delay = env::var("COMPME_ACCEPTANCE_HIDE_AFTER_MS")
                .ok()
                .and_then(|raw| raw.parse::<u64>().ok())
                .map(Duration::from_millis)
                .unwrap_or_else(|| Duration::from_millis(100));
            subscription
                .hide_suggestion_after(delay)
                .expect("schedule delayed hide");
        }
        other => {
            eprintln!(
                "unknown requirement {other:?}; expected inactive, full, word, correction, correction-tab, correction-full, correction-escape, correction-cycle, delayed-hide, escape, option-tab, or cycle"
            );
            process::exit(2);
        }
    }

    // The posted key must match the requirement: grave accepts the full
    // completion, Tab accepts the next word. Posting Tab for "full" would now
    // resolve to a word accept under the keycode-driven binding.
    let (accept_keycode, accept_key_label, option_down) = key_to_post_for_requirement(&requirement);
    if let Some(delay) = env::var("COMPME_ACCEPTANCE_POST_TAB_AFTER_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
    {
        thread::spawn(move || {
            thread::sleep(delay);
            match post_accept_key(accept_keycode, option_down) {
                Ok(()) => println!("POSTED_{accept_key_label}"),
                Err(err) => eprintln!("POST_KEY_ERROR {err}"),
            }
        });
    }

    // Carbon dispatches RegisterEventHotKey presses during application event
    // DEQUEUE, not via CFRunLoop sources — without this pump the hotkeys
    // register but never fire (the c41 root cause; the product binary pumps
    // each heartbeat for the same reason).
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        platform_macos::pump_app_events();
        thread::sleep(Duration::from_millis(50));
    }

    let controls = controls.lock().expect("controls").clone();
    println!("SUMMARY controls={controls:?}");

    let accepted = controls_satisfy_requirement(&requirement, &controls);
    if !accepted {
        process::exit(1);
    }
}

fn env_truthy(key: &str) -> bool {
    env::var(key).ok().is_some_and(|raw| truthy_value(&raw))
}

fn truthy_value(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn key_to_post_for_requirement(requirement: &str) -> (u16, &'static str, bool) {
    match requirement {
        "full" => (KEYCODE_GRAVE, "GRAVE", false),
        "correction" => (KEYCODE_GRAMMAR_ACCEPT, "GRAMMAR_ACCEPT", false),
        "correction-full" => (KEYCODE_GRAVE, "GRAVE", false),
        "correction-escape" => (KEYCODE_ESCAPE, "ESCAPE", false),
        "correction-cycle" => (KEYCODE_DOWN, "DOWN", false),
        "escape" => (KEYCODE_ESCAPE, "ESCAPE", false),
        "option-tab" => (KeyCode::TAB, "OPTION_TAB", true),
        "cycle" => (KEYCODE_DOWN, "DOWN", false),
        _ => (KeyCode::TAB, "TAB", false),
    }
}

fn controls_satisfy_requirement(requirement: &str, controls: &[TapControl]) -> bool {
    match requirement {
        "inactive" | "delayed-hide" | "option-tab" => controls.is_empty(),
        "correction-tab" | "correction-full" | "correction-escape" | "correction-cycle" => {
            controls.is_empty()
        }
        "full" => controls == [TapControl::Accept(AcceptAction::Full)],
        "word" => controls == [TapControl::Accept(AcceptAction::Word)],
        "correction" => controls == [TapControl::Accept(AcceptAction::Correction)],
        "escape" => controls == [TapControl::Dismiss],
        "cycle" => controls == [TapControl::Cycle],
        _ => false,
    }
}

fn post_accept_key(keycode: u16, option_down: bool) -> Result<(), String> {
    let source = CGEventSource::new(CGEventSourceStateID::Private)
        .map_err(|_| "failed to create CGEventSource".to_string())?;
    let key_down = CGEvent::new_keyboard_event(source.clone(), keycode, true)
        .map_err(|_| "failed to create key-down event".to_string())?;
    let key_up = CGEvent::new_keyboard_event(source, keycode, false)
        .map_err(|_| "failed to create key-up event".to_string())?;
    if option_down {
        key_down.set_flags(CGEventFlags::CGEventFlagAlternate);
        key_up.set_flags(CGEventFlags::CGEventFlagAlternate);
    }
    key_down.post(CGEventTapLocation::HID);
    key_up.post(CGEventTapLocation::HID);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_to_post_matches_accept_contract() {
        assert_eq!(
            key_to_post_for_requirement("full"),
            (KEYCODE_GRAVE, "GRAVE", false)
        );
        assert_eq!(
            key_to_post_for_requirement("word"),
            (KeyCode::TAB, "TAB", false)
        );
        assert_eq!(
            key_to_post_for_requirement("correction"),
            (KEYCODE_GRAMMAR_ACCEPT, "GRAMMAR_ACCEPT", false)
        );
        assert_eq!(
            key_to_post_for_requirement("correction-tab"),
            (KeyCode::TAB, "TAB", false)
        );
        assert_eq!(
            key_to_post_for_requirement("correction-full"),
            (KEYCODE_GRAVE, "GRAVE", false)
        );
        assert_eq!(
            key_to_post_for_requirement("correction-escape"),
            (KEYCODE_ESCAPE, "ESCAPE", false)
        );
        assert_eq!(
            key_to_post_for_requirement("correction-cycle"),
            (KEYCODE_DOWN, "DOWN", false)
        );
        assert_eq!(
            key_to_post_for_requirement("option-tab"),
            (KeyCode::TAB, "OPTION_TAB", true)
        );
        assert_eq!(
            key_to_post_for_requirement("escape"),
            (KEYCODE_ESCAPE, "ESCAPE", false)
        );
        assert_eq!(
            key_to_post_for_requirement("cycle"),
            (KEYCODE_DOWN, "DOWN", false)
        );
        assert_eq!(
            key_to_post_for_requirement("inactive"),
            (KeyCode::TAB, "TAB", false)
        );
    }

    #[test]
    fn env_truthy_accepts_only_explicit_truthy_values() {
        assert!(truthy_value("true"));
        assert!(truthy_value(" YES "));
        assert!(truthy_value("on"));
        assert!(!truthy_value("0"));
        assert!(!truthy_value(""));
    }

    #[test]
    fn controls_satisfy_only_the_requested_behavior() {
        assert!(controls_satisfy_requirement(
            "full",
            &[TapControl::Accept(AcceptAction::Full)]
        ));
        assert!(!controls_satisfy_requirement(
            "full",
            &[
                TapControl::Accept(AcceptAction::Word),
                TapControl::Accept(AcceptAction::Full),
            ]
        ));
        assert!(!controls_satisfy_requirement(
            "full",
            &[
                TapControl::Accept(AcceptAction::Full),
                TapControl::Accept(AcceptAction::Word),
            ]
        ));
        assert!(!controls_satisfy_requirement(
            "full",
            &[TapControl::Accept(AcceptAction::Word)]
        ));
        // Duplicate identical fires (double registration / a future key-up
        // handler) must fail too — exact length, not just membership.
        assert!(!controls_satisfy_requirement(
            "full",
            &[
                TapControl::Accept(AcceptAction::Full),
                TapControl::Accept(AcceptAction::Full),
            ]
        ));
        assert!(controls_satisfy_requirement(
            "word",
            &[TapControl::Accept(AcceptAction::Word)]
        ));
        assert!(controls_satisfy_requirement(
            "correction",
            &[TapControl::Accept(AcceptAction::Correction)]
        ));
        assert!(!controls_satisfy_requirement(
            "correction",
            &[TapControl::Accept(AcceptAction::Full)]
        ));
        assert!(controls_satisfy_requirement("correction-tab", &[]));
        assert!(controls_satisfy_requirement("correction-full", &[]));
        assert!(controls_satisfy_requirement("correction-escape", &[]));
        assert!(controls_satisfy_requirement("correction-cycle", &[]));
        assert!(!controls_satisfy_requirement(
            "correction-tab",
            &[TapControl::Accept(AcceptAction::Word)]
        ));
        assert!(!controls_satisfy_requirement(
            "word",
            &[TapControl::Accept(AcceptAction::Word), TapControl::Dismiss]
        ));
        assert!(controls_satisfy_requirement(
            "escape",
            &[TapControl::Dismiss]
        ));
        assert!(!controls_satisfy_requirement(
            "escape",
            &[TapControl::Dismiss, TapControl::Cycle]
        ));
        assert!(controls_satisfy_requirement("cycle", &[TapControl::Cycle]));
        assert!(!controls_satisfy_requirement(
            "cycle",
            &[TapControl::Cycle, TapControl::Dismiss]
        ));
        assert!(controls_satisfy_requirement("option-tab", &[]));
        assert!(!controls_satisfy_requirement(
            "option-tab",
            &[TapControl::Accept(AcceptAction::Word)]
        ));
        assert!(!controls_satisfy_requirement("unknown", &[]));
    }
}
