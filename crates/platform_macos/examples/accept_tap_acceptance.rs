use std::env;
use std::process;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use core_graphics::event::{CGEvent, CGEventTapLocation, KeyCode};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use platform::{AcceptAction, PlatformAdapter, TapControl};
use platform_macos::MacosPlatformAdapter;

/// Grave/backtick (key above Tab). Must match the engine's accept binding:
/// Tab accepts the next word, grave accepts the full completion.
const KEYCODE_GRAVE: u16 = 50;

fn main() {
    let duration = env::args()
        .nth(1)
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(4));
    let requirement = env::args().nth(2).unwrap_or_else(|| "full".into());

    let adapter = match MacosPlatformAdapter::new() {
        Ok(adapter) => adapter,
        Err(err) => {
            eprintln!("failed to create macOS adapter: {err:?}");
            process::exit(2);
        }
    };

    println!("front_app={:?}", adapter.front_app());

    let actions = Arc::new(Mutex::new(Vec::new()));
    let actions_for_callback = Arc::clone(&actions);
    let subscription = match adapter.subscribe_accept(Arc::new(move |control| {
        println!("ACCEPT_ACTION {control:?}");
        if let TapControl::Accept(action) = control {
            actions_for_callback.lock().expect("actions").push(action);
        }
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
        "full" => {
            subscription
                .set_suggestion_visible(true)
                .expect("show full suggestion");
            subscription
                .set_accept_action(Some(AcceptAction::Full))
                .expect("arm full accept");
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
            let delay = env::var("COMPLETE_ME_ACCEPTANCE_HIDE_AFTER_MS")
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
                "unknown requirement {other:?}; expected inactive, full, word, or delayed-hide"
            );
            process::exit(2);
        }
    }

    // The posted key must match the requirement: grave accepts the full
    // completion, Tab accepts the next word. Posting Tab for "full" would now
    // resolve to a word accept under the keycode-driven binding.
    let (accept_keycode, accept_key_label): (u16, &str) = if requirement == "full" {
        (KEYCODE_GRAVE, "GRAVE")
    } else {
        (KeyCode::TAB, "TAB")
    };
    if let Some(delay) = env::var("COMPLETE_ME_ACCEPTANCE_POST_TAB_AFTER_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
    {
        thread::spawn(move || {
            thread::sleep(delay);
            match post_accept_key(accept_keycode) {
                Ok(()) => println!("POSTED_{accept_key_label}"),
                Err(err) => eprintln!("POST_KEY_ERROR {err}"),
            }
        });
    }

    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }

    let actions = actions.lock().expect("actions").clone();
    println!("SUMMARY actions={actions:?}");

    let accepted = match requirement.as_str() {
        "inactive" => actions.is_empty(),
        "full" => actions.contains(&AcceptAction::Full),
        "word" => actions.contains(&AcceptAction::Word),
        "delayed-hide" => actions.is_empty(),
        _ => false,
    };
    if !accepted {
        process::exit(1);
    }
}

fn post_accept_key(keycode: u16) -> Result<(), String> {
    let source = CGEventSource::new(CGEventSourceStateID::Private)
        .map_err(|_| "failed to create CGEventSource".to_string())?;
    let key_down = CGEvent::new_keyboard_event(source.clone(), keycode, true)
        .map_err(|_| "failed to create key-down event".to_string())?;
    let key_up = CGEvent::new_keyboard_event(source, keycode, false)
        .map_err(|_| "failed to create key-up event".to_string())?;
    key_down.post(CGEventTapLocation::HID);
    key_up.post(CGEventTapLocation::HID);
    Ok(())
}
