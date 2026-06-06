//! The main-thread run loop: the place where every proven part meets.
//!
//! Threading model (see the P0 design spec):
//! - This loop runs on the AppKit **main thread**. It owns the `Engine` and the
//!   `MacosOverlayPresenter`; the engine applies overlay commands internally, and
//!   the overlay enforces the main thread at runtime.
//! - Platform focus/caret/accept callbacks fire on the adapter's **dispatcher
//!   thread**; they only enqueue a `HostEvent` (cheap, no AX work).
//! - Inference runs on its own thread (`InferenceHandle`).
//! - Each iteration drains queued host events and inference outcomes, ticks the
//!   engine, submits the newest pending request, then pumps the CFRunLoop for one
//!   heartbeat (which paces the loop and services the overlay).

use std::env;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};
use engine::{CompletionRequest, Engine, TriggerPolicy};
use platform::{AcceptAction, FieldHandle, PlatformAdapter, PlatformError, ScreenRect};
use platform_macos::{MacosOverlayPresenter, MacosPlatformAdapter};

use crate::adapter::SharedAdapter;
use crate::inference::InferenceHandle;
use crate::model_select::{load_model, resolve_prompt_mode, resolve_source, PromptMode};
use crate::wiring::{FieldTracker, LatestRequest, Observation};

const DEBOUNCE_MS: u64 = 120;
const MAX_WORDS: usize = 8;
const MAX_TOKENS: usize = 24;
const HEARTBEAT: Duration = Duration::from_millis(12);
const DEFAULT_MODEL: &str = "tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf";

/// Set by the SIGINT/SIGTERM handler; observed by the loop to begin shutdown.
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: libc::c_int) {
    // Async-signal-safe: only a relaxed atomic store.
    STOP.store(true, Ordering::Relaxed);
}

fn install_signal_handlers() {
    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: installing a handler that only sets an atomic flag is safe.
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

/// What a platform callback enqueues for the main loop to process.
enum HostEvent {
    Focus(FieldHandle),
    Caret(FieldHandle, Option<ScreenRect>),
    Accept(AcceptAction),
}

/// Runtime configuration, all from the environment (full config surface is P1).
struct Config {
    acceptance_pid: Option<i32>,
    stub_completion: Option<String>,
    model_path: PathBuf,
    prompt_mode: PromptMode,
    run_ms: Option<u64>,
}

impl Config {
    fn from_env() -> Self {
        Self {
            acceptance_pid: env::var("COMPLETE_ME_ACCEPTANCE_PID")
                .ok()
                .and_then(|raw| raw.parse::<i32>().ok()),
            stub_completion: env::var("COMPLETE_ME_STUB_COMPLETION")
                .ok()
                .filter(|s| !s.is_empty()),
            model_path: env::var("COMPLETE_ME_MODEL_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(DEFAULT_MODEL)),
            prompt_mode: resolve_prompt_mode(env::var("COMPLETE_ME_PROMPT_MODE").ok()),
            run_ms: env::var("COMPLETE_ME_RUN_MS")
                .ok()
                .and_then(|raw| raw.parse::<u64>().ok()),
        }
    }
}

/// Log a platform error and fall back to no requests, so one failed effect never
/// kills the loop.
fn log_err(
    what: &str,
    result: Result<Vec<CompletionRequest>, PlatformError>,
) -> Vec<CompletionRequest> {
    match result {
        Ok(requests) => requests,
        Err(err) => {
            eprintln!("complete-me: {what} error: {err:?}");
            Vec::new()
        }
    }
}

fn offer_all(latest: &mut LatestRequest, requests: Vec<CompletionRequest>) {
    for request in requests {
        latest.offer(request);
    }
}

/// Build the whole stack, run until a signal (or the run-ms deadline), then tear
/// down in order.
pub fn run() -> Result<(), String> {
    let config = Config::from_env();
    install_signal_handlers();

    let adapter = match config.acceptance_pid {
        Some(pid) => MacosPlatformAdapter::with_frontmost_pid_override_for_acceptance(pid),
        None => MacosPlatformAdapter::new(),
    }
    .map_err(|err| format!("adapter init: {err:?}"))?;
    let adapter = Arc::new(adapter);

    let overlay = MacosOverlayPresenter::new().map_err(|err| format!("overlay init: {err:?}"))?;

    let mut engine = Engine::new(
        SharedAdapter::new(Arc::clone(&adapter)),
        overlay,
        DEBOUNCE_MS,
        MAX_WORDS,
        MAX_TOKENS,
    );

    // Callbacks fire on the dispatcher thread; mpsc::Sender is !Sync, so share it
    // through a Mutex (the callbacks must be Send + Sync).
    let (tx, rx) = channel::<HostEvent>();
    let tx: Arc<Mutex<Sender<HostEvent>>> = Arc::new(Mutex::new(tx));

    let focus_tx = Arc::clone(&tx);
    let focus_sub = adapter
        .subscribe_focus(Arc::new(move |field| {
            if let Ok(tx) = focus_tx.lock() {
                let _ = tx.send(HostEvent::Focus(field));
            }
        }))
        .map_err(|err| format!("subscribe focus: {err:?}"))?;

    let caret_tx = Arc::clone(&tx);
    let caret_sub = adapter
        .subscribe_caret(Arc::new(move |field, rect| {
            if let Ok(tx) = caret_tx.lock() {
                let _ = tx.send(HostEvent::Caret(field, rect));
            }
        }))
        .map_err(|err| format!("subscribe caret: {err:?}"))?;

    let accept_tx = Arc::clone(&tx);
    let accept_sub = adapter
        .subscribe_accept(Arc::new(move |action| {
            if let Ok(tx) = accept_tx.lock() {
                let _ = tx.send(HostEvent::Accept(action));
            }
        }))
        .map_err(|err| format!("subscribe accept: {err:?}"))?;
    engine.set_accept_subscription(accept_sub);

    let model = load_model(resolve_source(
        config.stub_completion.clone(),
        config.model_path.clone(),
    ))?;
    let inference = InferenceHandle::spawn(model, config.prompt_mode)?;

    let mut tracker = FieldTracker::new();
    let mut latest = LatestRequest::new();
    let start = Instant::now();

    eprintln!(
        "complete-me: running (acceptance_pid={:?} stub={} run_ms={:?})",
        config.acceptance_pid,
        config.stub_completion.is_some(),
        config.run_ms
    );

    while !STOP.load(Ordering::Relaxed) {
        let now_ms = start.elapsed().as_millis() as u64;

        // 1. Host events → engine. The caret callback is the typing driver: read
        // context (executes on the adapter's AX worker), diff into a TextChange.
        for event in rx.try_iter() {
            match event {
                HostEvent::Focus(field) => {
                    eprintln!("complete-me: focus {}", field.element_id);
                    tracker.reset();
                    offer_all(&mut latest, log_err("on_focus", engine.on_focus(field)));
                }
                HostEvent::Caret(field, _rect) => match adapter.read_context(&field) {
                    // One selection-changed notification covers both typing and a
                    // bare cursor move. Typing schedules a completion; a cursor
                    // move only invalidates a showing ghost (no re-request).
                    Ok(ctx) => {
                        match tracker.observe(&field, &ctx, TriggerPolicy::Automatic, now_ms) {
                            Observation::Typed(change) => offer_all(
                                &mut latest,
                                log_err("on_text_changed", engine.on_text_changed(change)),
                            ),
                            Observation::CaretMoved { field, caret } => offer_all(
                                &mut latest,
                                log_err("on_caret_moved", engine.on_caret_moved(field, caret)),
                            ),
                        }
                    }
                    Err(err) => eprintln!("complete-me: read_context: {err:?}"),
                },
                HostEvent::Accept(action) => {
                    eprintln!("complete-me: accept {action:?}");
                    offer_all(&mut latest, log_err("on_accept", engine.on_accept(action)));
                }
            }
        }

        // 2. Inference outcomes → engine (stale ones are discarded internally).
        for outcome in inference.drain_outcomes() {
            eprintln!(
                "complete-me: completion gen={} text={:?}",
                outcome.request.generation, outcome.text
            );
            offer_all(
                &mut latest,
                log_err(
                    "on_completion",
                    engine.on_completion(&outcome.request, outcome.text),
                ),
            );
        }

        // 3. Debounce tick.
        offer_all(&mut latest, log_err("on_tick", engine.on_tick(now_ms)));

        // 4. Submit the newest pending request — withheld until the model is warm
        // (the "loading" state; no suggestions appear before readiness).
        if inference.is_ready() {
            if let Some(request) = latest.take() {
                eprintln!(
                    "complete-me: request gen={} prompt={:?}",
                    request.generation, request.prompt
                );
                inference.submit(request);
            }
        }

        // 5. Bounded run (gates pass COMPLETE_ME_RUN_MS).
        if let Some(run_ms) = config.run_ms {
            if now_ms >= run_ms {
                break;
            }
        }

        // 6. Pump the main run loop: paces the loop and services the overlay.
        // SAFETY: `kCFRunLoopDefaultMode` is a Core Foundation extern static.
        let mode = unsafe { kCFRunLoopDefaultMode };
        CFRunLoop::run_in_mode(mode, HEARTBEAT, false);
    }

    eprintln!("complete-me: shutting down");
    drop(caret_sub);
    drop(focus_sub);
    inference.shutdown();
    drop(engine); // drops overlay + accept subscription + the engine's adapter handle
    drop(adapter); // last Arc ref → AX worker thread stops
    Ok(())
}
