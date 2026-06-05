use std::collections::BTreeSet;
use std::env;
use std::process;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use platform::{FieldHandle, PlatformAdapter, PlatformError};
use platform_macos::{MacosCaretRectSource, MacosPlatformAdapter};

#[derive(Default)]
struct EventState {
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
        .unwrap_or_else(|| Duration::from_secs(4));
    let requirement = env::args().nth(2).unwrap_or_else(|| "any".into());

    let adapter = match env::var("COMPLETE_ME_ACCEPTANCE_PID")
        .ok()
        .and_then(|raw| raw.parse::<i32>().ok())
    {
        Some(pid) => MacosPlatformAdapter::with_frontmost_pid_override_for_acceptance(pid),
        None => MacosPlatformAdapter::new(),
    };
    let adapter = match adapter {
        Ok(adapter) => adapter,
        Err(err) => {
            eprintln!("failed to create macOS adapter: {err:?}");
            process::exit(2);
        }
    };

    println!("front_app={:?}", adapter.front_app());

    let state = Arc::new(Mutex::new(EventState::default()));
    let focus_state = Arc::clone(&state);
    let caret_state = Arc::clone(&state);
    let focus = match adapter.subscribe_focus(Arc::new(move |field| {
        let mut state = focus_state.lock().expect("focus state");
        state.focus += 1;
        state.apps.insert(field.app.clone());
        if should_keep_diagnostic_field(&state.last_field, &field) {
            state.last_field = Some(field.clone());
        }
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
        let mut state = caret_state.lock().expect("caret state");
        state.caret += 1;
        state.apps.insert(field.app.clone());
        if rect.is_some() || should_keep_diagnostic_field(&state.last_field, &field) {
            state.last_field = Some(field.clone());
        }
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
    let mut accepted_diagnostics: Option<
        Result<platform_macos::MacosCaretDiagnostics, PlatformError>,
    > = None;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
        let last_field = {
            let state = state.lock().expect("state");
            state.last_field.clone()
        };
        if let Some(field) = last_field.as_ref() {
            let diagnostics = adapter.caret_diagnostics(field);
            if diagnostics_accepts(&diagnostics, &requirement) {
                accepted_diagnostics = Some(diagnostics);
                break;
            }
        }
    }

    drop(caret);
    drop(focus);

    let (focus_count, caret_count, last_field, apps) = {
        let state = state.lock().expect("state");
        (
            state.focus,
            state.caret,
            state.last_field.clone(),
            state.apps.clone(),
        )
    };
    let diagnostics = accepted_diagnostics.or_else(|| {
        last_field
            .as_ref()
            .map(|field| adapter.caret_diagnostics(field))
    });
    match &diagnostics {
        Some(Ok(diagnostics)) => println!(
            "DIAG source={:?} marker={:?} native={:?} resolved={:?}",
            diagnostics.source,
            diagnostics.marker_rect,
            diagnostics.native_rect,
            diagnostics.resolved_rect
        ),
        Some(Err(err)) => println!("DIAG_ERROR {err:?}"),
        None => println!("DIAG_ERROR no field observed"),
    }
    println!(
        "SUMMARY focus={} caret={} apps={:?}",
        focus_count, caret_count, apps
    );

    let accepted = diagnostics_accepts_option(&diagnostics, &requirement);
    if !accepted {
        process::exit(1);
    }
}

fn diagnostics_accepts_option(
    diagnostics: &Option<Result<platform_macos::MacosCaretDiagnostics, PlatformError>>,
    requirement: &str,
) -> bool {
    diagnostics
        .as_ref()
        .is_some_and(|diagnostics| diagnostics_accepts(diagnostics, requirement))
}

fn diagnostics_accepts(
    diagnostics: &Result<platform_macos::MacosCaretDiagnostics, PlatformError>,
    requirement: &str,
) -> bool {
    match requirement {
        "marker" => matches!(
            diagnostics,
            Ok(diagnostics) if diagnostics.source == MacosCaretRectSource::Marker
        ),
        "fallback" => matches!(
            diagnostics,
            Ok(diagnostics) if diagnostics.source == MacosCaretRectSource::NativeFallback
        ),
        "none" => matches!(
            diagnostics,
            Ok(diagnostics) if diagnostics.source == MacosCaretRectSource::None
        ),
        "any" => matches!(diagnostics, Ok(diagnostics) if diagnostics.resolved_rect.is_some()),
        other => {
            eprintln!("unknown requirement {other:?}; expected marker, fallback, none, or any");
            false
        }
    }
}

fn should_keep_diagnostic_field(current: &Option<FieldHandle>, candidate: &FieldHandle) -> bool {
    current
        .as_ref()
        .is_none_or(|current| !looks_like_text_field(current) || looks_like_text_field(candidate))
}

fn looks_like_text_field(field: &FieldHandle) -> bool {
    field.element_id.contains("role=AXTextArea") || field.element_id.contains("role=AXTextField")
}
