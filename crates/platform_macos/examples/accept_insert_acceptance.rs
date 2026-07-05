use std::env;
use std::process;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, KeyCode};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use platform::{
    AcceptAction, FieldHandle, InsertStrategy, PlatformAdapter, PlatformError, TapControl,
    TextContext,
};
use platform_macos::MacosPlatformAdapter;

/// Grave/backtick (key above Tab). Must match the engine's accept binding:
/// Tab accepts the next word, grave accepts the full completion.
const KEYCODE_GRAVE: u16 = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
enum RequirementPlan {
    Accept {
        action: AcceptAction,
        expected_text: String,
        keycode: u16,
        key_label: &'static str,
    },
    Passthrough {
        armed_action: AcceptAction,
        expected_text: String,
        keycode: u16,
        key_label: &'static str,
        option_down: bool,
    },
}

impl RequirementPlan {
    fn action_to_arm(&self) -> AcceptAction {
        match self {
            RequirementPlan::Accept { action, .. } => *action,
            RequirementPlan::Passthrough { armed_action, .. } => *armed_action,
        }
    }

    fn expected_text(&self) -> &str {
        match self {
            RequirementPlan::Accept { expected_text, .. }
            | RequirementPlan::Passthrough { expected_text, .. } => expected_text,
        }
    }

    fn post_key(&self) -> (u16, &'static str, bool) {
        match self {
            RequirementPlan::Accept {
                keycode, key_label, ..
            } => (*keycode, key_label, false),
            RequirementPlan::Passthrough {
                keycode,
                key_label,
                option_down,
                ..
            } => (*keycode, key_label, *option_down),
        }
    }
}

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

    let adapter = match env::var("COMPME_ACCEPTANCE_PID")
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
        env::var("COMPME_ACCEPTANCE_FULL_TEXT").unwrap_or_else(|_| " accepted-full".into());
    let word_text = env::var("COMPME_ACCEPTANCE_WORD_TEXT").unwrap_or_else(|_| " accepted".into());
    let plan = match accept_plan_for_requirement(&requirement, full_text, word_text) {
        Some(plan) => plan,
        None => {
            eprintln!("unknown requirement {requirement:?}; expected full, word, or option-tab");
            drop(caret);
            drop(focus);
            process::exit(2);
        }
    };
    let action = plan.action_to_arm();
    let expected_text = plan.expected_text().to_string();

    let adapter_for_accept = Arc::clone(&adapter);
    let accept_state = Arc::clone(&state);
    let expected_for_accept = expected_text.clone();
    let accept = match adapter.subscribe_accept(Arc::new(move |control| {
        let action = match control {
            TapControl::Accept(action) => action,
            // This accept-insert harness only exercises Tab/grave; ignore
            // Esc/cycle and the always-on shortcuts.
            TapControl::Dismiss | TapControl::Cycle | TapControl::Shortcut(_) => return,
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

    // Grave accepts the full completion, Tab accepts the next word, and
    // Option+Tab must pass through to the target app. Plain-text targets insert
    // a literal tab; rich TextEdit can handle the same key as list indentation.
    let (accept_keycode, accept_key_label, option_down) = plan.post_key();
    let post_after = env::var("COMPME_ACCEPTANCE_POST_TAB_AFTER_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(300));
    thread::spawn(move || {
        thread::sleep(post_after);
        match post_accept_key(accept_keycode, option_down) {
            Ok(()) => println!("POSTED_{accept_key_label}"),
            Err(err) => eprintln!("POST_KEY_ERROR {err}"),
        }
    });

    // Carbon dispatches RegisterEventHotKey presses during application event
    // DEQUEUE, not via CFRunLoop sources — without this pump the hotkeys
    // register but never fire (the c41 root cause; the product binary pumps
    // each heartbeat for the same reason).
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        platform_macos::pump_app_events();
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

    let accepted = match &plan {
        RequirementPlan::Accept { action, .. } => {
            let text_matches = matches!(
                (pre_read.as_ref(), post_read.as_ref()),
                (Some(before), Some(Ok(after))) if inserted_delta_matches(before, after, &expected_text)
            );
            actions == vec![*action]
                && matches!(insert_results.as_slice(), [Ok(())])
                && text_matches
        }
        RequirementPlan::Passthrough { .. } => {
            let passthrough_seen = matches!(
                (pre_read.as_ref(), post_read.as_ref()),
                (Some(before), Some(Ok(after))) if native_passthrough_matches(before, after, &expected_text)
            );
            actions.is_empty() && insert_results.is_empty() && passthrough_seen
        }
    };
    if !accepted {
        process::exit(1);
    }
}

fn accept_plan_for_requirement(
    requirement: &str,
    full_text: String,
    word_text: String,
) -> Option<RequirementPlan> {
    match requirement {
        "full" => Some(RequirementPlan::Accept {
            action: AcceptAction::Full,
            expected_text: full_text,
            keycode: KEYCODE_GRAVE,
            key_label: "GRAVE",
        }),
        "word" => Some(RequirementPlan::Accept {
            action: AcceptAction::Word,
            expected_text: word_text,
            keycode: KeyCode::TAB,
            key_label: "TAB",
        }),
        "option-tab" => Some(RequirementPlan::Passthrough {
            armed_action: AcceptAction::Word,
            expected_text: "\t".into(),
            keycode: KeyCode::TAB,
            key_label: "OPTION_TAB",
            option_down: true,
        }),
        _ => None,
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

fn native_passthrough_matches(
    before: &TextContext,
    after: &TextContext,
    insert_text: &str,
) -> bool {
    inserted_delta_matches(before, after, insert_text)
        || after.left != before.left
        || after.right != before.right
        || after.selection != before.selection
        || after.caret != before.caret
}

fn looks_like_text_field(field: &FieldHandle) -> bool {
    field.element_id.contains("role=AXTextArea") || field.element_id.contains("role=AXTextField")
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
    use platform::{ContextSource, OffsetEncoding, TextRange};

    fn context(left: &str, right: &str, caret: usize) -> TextContext {
        TextContext {
            left: left.into(),
            right: right.into(),
            caret,
            selection: None,
            source: ContextSource::Accessibility,
            field_id: FieldHandle {
                app: "pid:1".into(),
                pid: Some(1),
                element_id: "ax:ptr=1|role=AXTextArea".into(),
                generation: 1,
            },
            offset_encoding: OffsetEncoding::Utf16CodeUnits,
        }
    }

    #[test]
    fn accept_plan_matches_full_and_word_contract() {
        assert_eq!(
            accept_plan_for_requirement("full", " all".into(), " one".into()),
            Some(RequirementPlan::Accept {
                action: AcceptAction::Full,
                expected_text: " all".into(),
                keycode: KEYCODE_GRAVE,
                key_label: "GRAVE",
            })
        );
        assert_eq!(
            accept_plan_for_requirement("word", " all".into(), " one".into()),
            Some(RequirementPlan::Accept {
                action: AcceptAction::Word,
                expected_text: " one".into(),
                keycode: KeyCode::TAB,
                key_label: "TAB",
            })
        );
        assert_eq!(
            accept_plan_for_requirement("escape", " all".into(), " one".into()),
            None
        );
    }

    #[test]
    fn option_tab_plan_passes_native_tab_without_accepting() {
        let plan = accept_plan_for_requirement("option-tab", " all".into(), " one".into())
            .expect("option-tab plan");

        assert_eq!(plan.action_to_arm(), AcceptAction::Word);
        assert_eq!(plan.expected_text(), "\t");
        assert_eq!(plan.post_key(), (KeyCode::TAB, "OPTION_TAB", true));
    }

    #[test]
    fn native_passthrough_accepts_literal_tab_or_textedit_rich_text_transform() {
        let before = context("probe", "", 5);
        let literal = context("probe\t", "", 6);
        let rich_text_list_indent = context("\t\u{2043}\tprobe", "\n", 8);

        assert!(native_passthrough_matches(&before, &literal, "\t"));
        assert!(native_passthrough_matches(
            &before,
            &rich_text_list_indent,
            "\t"
        ));
    }

    #[test]
    fn native_passthrough_rejects_no_target_side_change() {
        let before = context("probe", "", 5);
        let unchanged = context("probe", "", 5);

        assert!(!native_passthrough_matches(&before, &unchanged, "\t"));
    }

    #[test]
    fn inserted_delta_matches_utf16_caret_growth() {
        let before = context("Hi ", "!", 3);
        let after = context("Hi 😀", "!", 5);

        assert!(inserted_delta_matches(&before, &after, "😀"));
    }

    #[test]
    fn inserted_delta_rejects_empty_or_wrong_readback() {
        let before = context("Hi", "!", 2);
        let valid_after = context("Hi there", "!", 8);
        assert!(!inserted_delta_matches(&before, &valid_after, ""));

        let wrong_right = context("Hi there", "?", 8);
        assert!(!inserted_delta_matches(&before, &wrong_right, " there"));

        let mut selected = context("Hi there", "!", 8);
        selected.selection = Some(TextRange { start: 2, end: 4 });
        assert!(!inserted_delta_matches(&before, &selected, " there"));

        let wrong_caret = context("Hi there", "!", 7);
        assert!(!inserted_delta_matches(&before, &wrong_caret, " there"));
    }

    #[test]
    fn looks_like_text_field_uses_role_metadata() {
        assert!(looks_like_text_field(&FieldHandle {
            app: "pid:1".into(),
            pid: Some(1),
            element_id: "ax:ptr=1|role=AXTextArea".into(),
            generation: 1,
        }));
        assert!(looks_like_text_field(&FieldHandle {
            app: "pid:1".into(),
            pid: Some(1),
            element_id: "ax:ptr=1|role=AXTextField".into(),
            generation: 1,
        }));
        assert!(!looks_like_text_field(&FieldHandle {
            app: "pid:1".into(),
            pid: Some(1),
            element_id: "ax:ptr=1|role=AXButton".into(),
            generation: 1,
        }));
    }
}
