use std::collections::BTreeSet;
use std::env;
use std::process;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use platform::{
    ux_mode, Capabilities, FieldHandle, InsertStrategy, Inserted, PlatformAdapter, PlatformError,
    TextContext, UxMode,
};
use platform_macos::MacosPlatformAdapter;

#[derive(Default)]
struct EventCounts {
    focus: usize,
    caret: usize,
    last_field: Option<FieldHandle>,
    apps: BTreeSet<String>,
}

fn main() {
    let duration = env::args()
        .nth(1)
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(6));
    let required = env::args().nth(2).unwrap_or_else(|| "both".into());

    let adapter = if let Ok(raw_sequence) = env::var("COMPME_ACCEPTANCE_PID_SEQUENCE") {
        let sequence = raw_sequence
            .split(',')
            .filter_map(|raw| raw.trim().parse::<i32>().ok())
            .collect::<Vec<_>>();
        if sequence.is_empty() {
            eprintln!("COMPME_ACCEPTANCE_PID_SEQUENCE did not contain any valid pids");
            process::exit(2);
        }

        let interval = env::var("COMPME_ACCEPTANCE_PID_SEQUENCE_INTERVAL_MS")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or_else(|| Duration::from_millis(900));
        let current = Arc::new(Mutex::new(sequence[0]));
        let current_for_thread = Arc::clone(&current);
        thread::spawn(move || {
            for pid in sequence.into_iter().skip(1) {
                thread::sleep(interval);
                *current_for_thread.lock().expect("pid sequence") = pid;
            }
        });

        MacosPlatformAdapter::with_frontmost_pid_provider_for_acceptance(move || {
            Some(*current.lock().expect("current pid"))
        })
    } else {
        match env::var("COMPME_ACCEPTANCE_PID")
            .ok()
            .and_then(|raw| raw.parse::<i32>().ok())
        {
            Some(pid) => MacosPlatformAdapter::with_frontmost_pid_override_for_acceptance(pid),
            None => MacosPlatformAdapter::new(),
        }
    };
    let adapter = match adapter {
        Ok(adapter) => adapter,
        Err(err) => {
            eprintln!("failed to create macOS adapter: {err:?}");
            process::exit(2);
        }
    };

    println!("front_app={:?}", adapter.front_app());

    let counts = Arc::new(Mutex::new(EventCounts::default()));
    let focus_counts = Arc::clone(&counts);
    let caret_counts = Arc::clone(&counts);

    let focus = match adapter.subscribe_focus(Arc::new(move |field| {
        let mut counts = focus_counts.lock().expect("focus counts");
        counts.focus += 1;
        counts.last_field = Some(field.clone());
        counts.apps.insert(field.app.clone());
        println!(
            "FOCUS app={} pid={:?} generation={} element={}",
            field.app, field.pid, field.generation, field.element_id
        );
    })) {
        Ok(subscription) => subscription,
        Err(err) => {
            eprintln!("failed to subscribe focus: {err:?}");
            process::exit(2);
        }
    };

    let caret = match adapter.subscribe_caret(Arc::new(move |field, rect| {
        let mut counts = caret_counts.lock().expect("caret counts");
        counts.caret += 1;
        counts.last_field = Some(field.clone());
        counts.apps.insert(field.app.clone());
        println!(
            "CARET app={} pid={:?} generation={} rect={:?} element={}",
            field.app, field.pid, field.generation, rect, field.element_id
        );
    })) {
        Ok(subscription) => subscription,
        Err(err) => {
            eprintln!("failed to subscribe caret: {err:?}");
            drop(focus);
            process::exit(2);
        }
    };

    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }

    drop(caret);
    drop(focus);

    let (focus_count, caret_count, last_field, apps) = {
        let counts = counts.lock().expect("counts");
        (
            counts.focus,
            counts.caret,
            counts.last_field.clone(),
            counts.apps.clone(),
        )
    };

    let read_context = last_field.as_ref().map(|field| adapter.read_context(field));
    if let Some(result) = &read_context {
        match result {
            Ok(context) => println!(
                "READ caret={} selection={:?} left={:?} right={:?} source={:?} encoding={:?}",
                context.caret,
                context.selection,
                context.left,
                context.right,
                context.source,
                context.offset_encoding
            ),
            Err(err) => println!("READ_ERROR {err:?}"),
        }
    }
    let caret_rect = last_field.as_ref().map(|field| adapter.caret_rect(field));
    if let Some(result) = &caret_rect {
        match result {
            Ok(rect) => println!("RECT {rect:?}"),
            Err(err) => println!("RECT_ERROR {err:?}"),
        }
    }
    let capabilities = last_field.as_ref().map(|field| adapter.capabilities(field));
    if let Some(result) = &capabilities {
        match result {
            Ok(caps) => println!(
                "CAPS readable_text={} readable_caret={} writable={} secure={} state={:?} toolkit={:?} multiline={} insert={:?} intercept={:?} overlay={:?} coords_global={} ux={:?}",
                caps.readable_text,
                caps.readable_caret,
                caps.writable,
                caps.secure,
                caps.security_state,
                caps.toolkit,
                caps.multiline,
                caps.insert_strategy,
                caps.accept_intercept,
                caps.overlay_at_caret,
                caps.coords_global_screen,
                ux_mode(caps)
            ),
            Err(err) => println!("CAPS_ERROR {err:?}"),
        }
    }
    let insert_text = env::var("COMPME_ACCEPTANCE_INSERT_TEXT")
        .unwrap_or_else(|_| format!(" cm-insert-{}", process::id()));
    let requested_insert_strategy = match required.as_str() {
        "insert" | "popup" => Some(InsertStrategy::AxSet),
        "synthetic" => Some(InsertStrategy::SyntheticKeys),
        "clipboard" => Some(InsertStrategy::Clipboard),
        _ => None,
    };
    if requested_insert_strategy.is_some() && insert_text.is_empty() {
        eprintln!("insertion acceptance requires non-empty COMPME_ACCEPTANCE_INSERT_TEXT");
        process::exit(2);
    }
    let pre_insert_read = if requested_insert_strategy.is_some() {
        last_field.as_ref().map(|field| adapter.read_context(field))
    } else {
        None
    };
    if let Some(result) = &pre_insert_read {
        match result {
            Ok(context) => println!(
                "PRE_INSERT_READ caret={} selection={:?} left={:?} right={:?}",
                context.caret, context.selection, context.left, context.right
            ),
            Err(err) => println!("PRE_INSERT_READ_ERROR {err:?}"),
        }
    }
    let insert_result = requested_insert_strategy.and_then(|strategy| {
        last_field
            .as_ref()
            .map(|field| adapter.insert(field, &insert_text, strategy))
    });
    if let Some(result) = &insert_result {
        match result {
            Ok(inserted) => println!(
                "INSERT bytes={} chars={} strategy={:?} text={:?}",
                inserted.bytes, inserted.chars, inserted.strategy, insert_text
            ),
            Err(err) => println!("INSERT_ERROR {err:?}"),
        }
    }
    let post_insert_read = if matches!(insert_result, Some(Ok(_))) {
        last_field.as_ref().and_then(|field| {
            poll_post_insert_read(
                &adapter,
                field,
                pre_insert_read
                    .as_ref()
                    .and_then(|result| result.as_ref().ok()),
                &insert_text,
                Duration::from_secs(2),
            )
        })
    } else {
        None
    };
    if let Some(result) = &post_insert_read {
        match result {
            Ok(context) => println!(
                "POST_INSERT_READ caret={} selection={:?} left={:?} right={:?}",
                context.caret, context.selection, context.left, context.right
            ),
            Err(err) => println!("POST_INSERT_READ_ERROR {err:?}"),
        }
    }

    println!(
        "SUMMARY focus={} caret={} apps={:?}",
        focus_count, caret_count, apps
    );
    let accepted = match required.as_str() {
        "focus" => focus_count > 0,
        "caret" => caret_count > 0,
        "both" => focus_count > 0 && caret_count > 0,
        "switch" => apps.len() > 1,
        "read" => matches!(read_context, Some(Ok(_))),
        "rect" => matches!(caret_rect, Some(Ok(Some(_)))),
        "caps" => {
            matches!(
                capabilities,
                Some(Ok(ref caps))
                    if caps.readable_text
                        && caps.readable_caret
                        && caps.writable
                        && !caps.secure
                        && caps.insert_strategy == InsertStrategy::AxSet
            )
        }
        "popup" => popup_acceptance_matches(
            &capabilities,
            &insert_result,
            &pre_insert_read,
            &post_insert_read,
            &insert_text,
        ),
        "insert" => {
            matches!(insert_result, Some(Ok(_)))
                && matches!(
                    (&pre_insert_read, &post_insert_read),
                    (Some(Ok(before)), Some(Ok(after))) if inserted_delta_matches(before, after, &insert_text)
                )
        }
        "synthetic" => {
            matches!(insert_result, Some(Ok(ref inserted)) if inserted.strategy == InsertStrategy::SyntheticKeys)
                && matches!(
                    (&pre_insert_read, &post_insert_read),
                    (Some(Ok(before)), Some(Ok(after))) if inserted_delta_matches(before, after, &insert_text)
                )
        }
        "clipboard" => {
            matches!(insert_result, Some(Ok(ref inserted)) if inserted.strategy == InsertStrategy::Clipboard)
                && matches!(
                    (&pre_insert_read, &post_insert_read),
                    (Some(Ok(before)), Some(Ok(after))) if inserted_delta_matches(before, after, &insert_text)
                )
        }
        other => {
            eprintln!(
                "unknown requirement {other:?}; expected focus, caret, switch, read, rect, caps, popup, insert, synthetic, clipboard, or both"
            );
            false
        }
    };
    if !accepted {
        process::exit(1);
    }
}

fn popup_acceptance_matches(
    capabilities: &Option<Result<Capabilities, PlatformError>>,
    insert_result: &Option<Result<Inserted, PlatformError>>,
    pre_insert_read: &Option<Result<TextContext, PlatformError>>,
    post_insert_read: &Option<Result<TextContext, PlatformError>>,
    insert_text: &str,
) -> bool {
    matches!(
        capabilities,
        Some(Ok(caps))
            if caps.readable_text
                && caps.writable
                && !caps.secure
                && ux_mode(caps) == UxMode::Popup
    ) && matches!(insert_result, Some(Ok(inserted)) if inserted.strategy == InsertStrategy::AxSet)
        && matches!(
            (pre_insert_read, post_insert_read),
            (Some(Ok(before)), Some(Ok(after))) if inserted_delta_matches(before, after, insert_text)
        )
}

fn poll_post_insert_read(
    adapter: &MacosPlatformAdapter,
    field: &FieldHandle,
    before: Option<&TextContext>,
    insert_text: &str,
    timeout: Duration,
) -> Option<Result<TextContext, platform::PlatformError>> {
    let deadline = Instant::now() + timeout;
    let mut last = None;
    while Instant::now() < deadline {
        let result = adapter.read_context(field);
        if let (Some(before), Ok(after)) = (before, &result) {
            if inserted_delta_matches(before, after, insert_text) {
                return Some(result);
            }
        }
        last = Some(result);
        thread::sleep(Duration::from_millis(50));
    }
    last
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

#[cfg(test)]
mod tests {
    use super::*;
    use platform::{
        ContextSource, KeyInterceptMode, OffsetEncoding, OverlayPlacement, SecurityState, Toolkit,
    };

    fn popup_caps() -> Option<Result<Capabilities, PlatformError>> {
        Some(Ok(Capabilities {
            readable_text: true,
            readable_caret: false,
            writable: true,
            assistant_field: false,
            secure: false,
            security_state: SecurityState::Normal,
            toolkit: Toolkit::AppKit,
            multiline: false,
            insert_strategy: InsertStrategy::AxSet,
            accept_intercept: KeyInterceptMode::CarbonHotkey,
            overlay_at_caret: OverlayPlacement::None,
            coords_global_screen: true,
        }))
    }

    fn inserted(strategy: InsertStrategy) -> Option<Result<Inserted, PlatformError>> {
        Some(Ok(Inserted {
            bytes: " inserted".len(),
            chars: " inserted".chars().count(),
            strategy,
        }))
    }

    fn context(left: &str) -> Result<TextContext, PlatformError> {
        Ok(TextContext {
            left: left.into(),
            right: String::new(),
            left_scalars: left.chars().count(),
            selection: None,
            selected_text: None,
            caret: left.encode_utf16().count(),
            source: ContextSource::Accessibility,
            field_id: FieldHandle {
                app: "pid:1".into(),
                pid: Some(1),
                element_id: "field".into(),
                generation: 1,
            },
            offset_encoding: OffsetEncoding::Utf16CodeUnits,
        })
    }

    #[test]
    fn popup_acceptance_requires_popup_caps_ax_insert_and_readback_delta() {
        assert!(popup_acceptance_matches(
            &popup_caps(),
            &inserted(InsertStrategy::AxSet),
            &Some(context("value")),
            &Some(context("value inserted")),
            " inserted",
        ));
    }

    #[test]
    fn popup_acceptance_rejects_insert_without_readback_delta() {
        assert!(!popup_acceptance_matches(
            &popup_caps(),
            &inserted(InsertStrategy::AxSet),
            &Some(context("value")),
            &Some(context("value")),
            " inserted",
        ));
    }

    #[test]
    fn popup_acceptance_rejects_inline_mode_or_non_ax_insert() {
        let mut inline_caps = match popup_caps() {
            Some(Ok(caps)) => caps,
            _ => panic!("expected test caps"),
        };
        inline_caps.readable_caret = true;
        inline_caps.overlay_at_caret = OverlayPlacement::NativePanel;

        assert!(!popup_acceptance_matches(
            &Some(Ok(inline_caps)),
            &inserted(InsertStrategy::AxSet),
            &Some(context("value")),
            &Some(context("value inserted")),
            " inserted",
        ));
        assert!(!popup_acceptance_matches(
            &popup_caps(),
            &inserted(InsertStrategy::Clipboard),
            &Some(context("value")),
            &Some(context("value inserted")),
            " inserted",
        ));
    }
}
