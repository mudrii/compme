use std::env;
use std::process;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use core_graphics::event::{CGEvent, CGEventTapLocation, KeyCode};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use platform::{
    AcceptAction, FieldHandle, InsertStrategy, PlatformAdapter, PlatformError, TapControl,
    TextContext,
};
use platform_macos::MacosPlatformAdapter;

/// Grave/backtick (key above Tab). Must match the engine's accept binding:
/// Tab accepts the next word, grave accepts the full completion.
const KEYCODE_GRAVE: u16 = 50;

#[derive(Default)]
struct HarnessState {
    field: Option<FieldHandle>,
    pre_read: Option<TextContext>,
    actions: Vec<AcceptAction>,
    insert_results: Vec<Result<(), PlatformError>>,
}

fn main() {
    let duration = env::args()
        .nth(1)
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(4));
    let requirement = env::args().nth(2).unwrap_or_else(|| "full".into());

    let adapter = match env::var("COMPLETE_ME_ACCEPTANCE_PID")
        .ok()
        .and_then(|raw| raw.parse::<i32>().ok())
    {
        Some(pid) => MacosPlatformAdapter::with_frontmost_pid_override_for_acceptance(pid),
        None => MacosPlatformAdapter::new(),
    };
    let adapter = match adapter {
        Ok(adapter) => Arc::new(adapter),
        Err(err) => {
            eprintln!("failed to create macOS adapter: {err:?}");
            process::exit(2);
        }
    };

    println!("front_app={:?}", adapter.front_app());

    let state = Arc::new(Mutex::new(HarnessState::default()));
    let focus_state = Arc::clone(&state);
    let focus = match adapter.subscribe_focus(Arc::new(move |field| {
        println!(
            "FOCUS app={} pid={:?} generation={} element={}",
            field.app, field.pid, field.generation, field.element_id
        );
        if looks_like_text_field(&field) {
            focus_state.lock().expect("focus state").field = Some(field);
        }
    })) {
        Ok(subscription) => subscription,
        Err(err) => {
            eprintln!("failed to subscribe focus: {err:?}");
            process::exit(2);
        }
    };

    let caret_state = Arc::clone(&state);
    let caret = match adapter.subscribe_caret(Arc::new(move |field, rect| {
        println!(
            "CARET app={} pid={:?} generation={} rect={:?} element={}",
            field.app, field.pid, field.generation, rect, field.element_id
        );
        if rect.is_some() || looks_like_text_field(&field) {
            caret_state.lock().expect("caret state").field = Some(field);
        }
    })) {
        Ok(subscription) => subscription,
        Err(err) => {
            eprintln!("failed to subscribe caret: {err:?}");
            drop(focus);
            process::exit(2);
        }
    };

    let field = match wait_for_field(&adapter, &state, duration.min(Duration::from_secs(2))) {
        Some(field) => field,
        None => {
            eprintln!("no text field observed");
            drop(caret);
            drop(focus);
            process::exit(1);
        }
    };
    let pre_read = match adapter.read_context(&field) {
        Ok(context) => context,
        Err(err) => {
            eprintln!("PRE_READ_ERROR {err:?}");
            drop(caret);
            drop(focus);
            process::exit(1);
        }
    };
    println!(
        "PRE_READ caret={} selection={:?} left={:?} right={:?}",
        pre_read.caret, pre_read.selection, pre_read.left, pre_read.right
    );
    state.lock().expect("state").pre_read = Some(pre_read);

    let full_text =
        env::var("COMPLETE_ME_ACCEPTANCE_FULL_TEXT").unwrap_or_else(|_| " accepted-full".into());
    let word_text =
        env::var("COMPLETE_ME_ACCEPTANCE_WORD_TEXT").unwrap_or_else(|_| " accepted".into());
    let (action, expected_text) = match requirement.as_str() {
        "full" => (AcceptAction::Full, full_text),
        "word" => (AcceptAction::Word, word_text),
        other => {
            eprintln!("unknown requirement {other:?}; expected full or word");
            drop(caret);
            drop(focus);
            process::exit(2);
        }
    };

    let adapter_for_accept = Arc::clone(&adapter);
    let accept_state = Arc::clone(&state);
    let expected_for_accept = expected_text.clone();
    let accept = match adapter.subscribe_accept(Arc::new(move |control| {
        let action = match control {
            TapControl::Accept(action) => action,
            // This accept-insert harness only exercises Tab/grave; ignore Esc.
            TapControl::Dismiss => return,
        };
        println!("ACCEPT_ACTION {action:?}");
        let field = accept_state.lock().expect("state").field.clone();
        let result = match field {
            Some(field) => adapter_for_accept
                .insert(&field, &expected_for_accept, InsertStrategy::AxSet)
                .map(|_| ()),
            None => Err(PlatformError::CannotComplete {
                reason: "no field available for accept insert".into(),
            }),
        };
        println!("ACCEPT_INSERT_RESULT {result:?}");
        let mut state = accept_state.lock().expect("state");
        state.actions.push(action);
        state.insert_results.push(result);
    })) {
        Ok(subscription) => subscription,
        Err(err) => {
            eprintln!("failed to subscribe accept: {err:?}");
            drop(caret);
            drop(focus);
            process::exit(2);
        }
    };

    accept
        .set_suggestion_visible(true)
        .expect("show suggestion");
    accept
        .set_accept_action(Some(action))
        .expect("arm accept action");

    // grave accepts the full completion, Tab accepts the next word — post the key
    // that matches the requirement so the gate exercises the real accept path.
    let (accept_keycode, accept_key_label): (u16, &str) = match requirement.as_str() {
        "full" => (KEYCODE_GRAVE, "GRAVE"),
        _ => (KeyCode::TAB, "TAB"),
    };
    let post_after = env::var("COMPLETE_ME_ACCEPTANCE_POST_TAB_AFTER_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(300));
    thread::spawn(move || {
        thread::sleep(post_after);
        match post_accept_key(accept_keycode) {
            Ok(()) => println!("POSTED_{accept_key_label}"),
            Err(err) => eprintln!("POST_KEY_ERROR {err}"),
        }
    });

    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }

    drop(accept);
    drop(caret);
    drop(focus);

    let (field, pre_read, actions, insert_results) = {
        let state = state.lock().expect("state");
        (
            state.field.clone(),
            state.pre_read.clone(),
            state.actions.clone(),
            state.insert_results.clone(),
        )
    };
    let post_read = field.as_ref().map(|field| adapter.read_context(field));
    if let Some(result) = &post_read {
        match result {
            Ok(context) => println!(
                "POST_READ caret={} selection={:?} left={:?} right={:?}",
                context.caret, context.selection, context.left, context.right
            ),
            Err(err) => println!("POST_READ_ERROR {err:?}"),
        }
    }
    println!("SUMMARY actions={actions:?} insert_results={insert_results:?}");

    let accepted = actions == vec![action]
        && matches!(insert_results.as_slice(), [Ok(())])
        && matches!(
            (pre_read.as_ref(), post_read.as_ref()),
            (Some(before), Some(Ok(after))) if inserted_delta_matches(before, after, &expected_text)
        );
    if !accepted {
        process::exit(1);
    }
}

fn wait_for_field(
    adapter: &MacosPlatformAdapter,
    state: &Arc<Mutex<HarnessState>>,
    timeout: Duration,
) -> Option<FieldHandle> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(field) = state.lock().expect("state").field.clone() {
            if adapter.read_context(&field).is_ok() {
                return Some(field);
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    None
}

fn inserted_delta_matches(before: &TextContext, after: &TextContext, insert_text: &str) -> bool {
    if insert_text.is_empty() {
        return false;
    }

    let expected_left = format!("{}{}", before.left, insert_text);
    after.left == expected_left
        && after.right == before.right
        && after.selection.is_none()
        && after.caret
            == before
                .caret
                .saturating_add(insert_text.encode_utf16().count())
}

fn looks_like_text_field(field: &FieldHandle) -> bool {
    field.element_id.contains("role=AXTextArea") || field.element_id.contains("role=AXTextField")
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
