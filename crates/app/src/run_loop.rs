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
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};
use engine::{CompletionRequest, Engine, TriggerPolicy};
use platform::{AcceptAction, FieldHandle, PlatformAdapter, PlatformError, ScreenRect};
use platform_macos::{
    accessibility_trusted, display_scales, prompt_accessibility_trust, secure_input_enabled,
    MacosOverlayPresenter, MacosPlatformAdapter, MacosTray, TrayFlags,
};

use crate::adapter::SharedAdapter;
use crate::config::{self, parse_clamped};
use crate::inference::InferenceHandle;
use crate::model_select::{load_model, resolve_prompt_mode, resolve_source, PromptMode};
use crate::status::{derive_status, should_dismiss};
use crate::wiring::{FieldTracker, LatestRequest, Observation};

const DEFAULT_DEBOUNCE_MS: u64 = 120;
const DEFAULT_MAX_WORDS: usize = 8;
const DEFAULT_MAX_TOKENS: usize = 24;
const DEFAULT_HEARTBEAT_MS: u64 = 12;
const DEFAULT_MODEL: &str = "tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf";
/// Re-poll secure input + Accessibility trust at most this often (wall-clock ms).
const SECURE_POLL_INTERVAL_MS: u64 = 480;
const ACCESSIBILITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

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
    debounce_ms: u64,
    max_words: usize,
    max_tokens: usize,
    heartbeat_ms: u64,
    diag_coords: bool,
}

impl Config {
    /// Build config by layering the environment over the optional config file
    /// (env wins over file wins over default), all through `from_lookup`.
    fn from_env() -> Self {
        let file_map = config::config_file_path()
            .map(|path| config::load_file_map(&path))
            .unwrap_or_default();
        Self::from_lookup(move |key| env::var(key).ok().or_else(|| file_map.get(key).cloned()))
    }

    /// Pure config parsing from a key→value lookup, so the parsing rules
    /// (pid/run_ms parse, empty-stub filtering, default model path, prompt mode,
    /// clamped numeric knobs) are unit-testable without touching the environment.
    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            acceptance_pid: lookup("COMPLETE_ME_ACCEPTANCE_PID")
                .and_then(|raw| raw.parse::<i32>().ok()),
            stub_completion: lookup("COMPLETE_ME_STUB_COMPLETION").filter(|s| !s.is_empty()),
            model_path: lookup("COMPLETE_ME_MODEL_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_MODEL)),
            prompt_mode: resolve_prompt_mode(lookup("COMPLETE_ME_PROMPT_MODE")),
            run_ms: lookup("COMPLETE_ME_RUN_MS").and_then(|raw| raw.parse::<u64>().ok()),
            debounce_ms: parse_clamped(
                lookup("COMPLETE_ME_DEBOUNCE_MS"),
                DEFAULT_DEBOUNCE_MS,
                0,
                5000,
            ),
            max_words: parse_clamped(lookup("COMPLETE_ME_MAX_WORDS"), DEFAULT_MAX_WORDS, 1, 50),
            max_tokens: parse_clamped(lookup("COMPLETE_ME_MAX_TOKENS"), DEFAULT_MAX_TOKENS, 1, 200),
            heartbeat_ms: parse_clamped(
                lookup("COMPLETE_ME_HEARTBEAT_MS"),
                DEFAULT_HEARTBEAT_MS,
                1,
                100,
            ),
            diag_coords: lookup("COMPLETE_ME_DIAG_COORDS").is_some_and(|v| v == "1" || v == "true"),
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

    // Permissions: if Accessibility isn't granted, fire the system prompt once.
    // The app keeps running and reflects the Blocked state in the tray. Trust is
    // re-polled in the loop so granting it mid-session clears Blocked without a
    // restart.
    let mut trusted = accessibility_trusted();
    if !trusted {
        eprintln!("complete-me: Accessibility not granted — requesting permission");
        prompt_accessibility_trust();
    }

    if config.diag_coords {
        eprintln!("complete-me: diag display_scales={:?}", display_scales());
    }

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
        config.debounce_ms,
        config.max_words,
        config.max_tokens,
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

    // Shared state for the tray; flipped by menu actions, observed by this loop.
    let flags = TrayFlags {
        enabled: Arc::new(AtomicBool::new(true)),
        quit: Arc::new(AtomicBool::new(false)),
        open_settings: Arc::new(AtomicBool::new(false)),
    };
    // A tray failure is non-fatal — the engine still runs headless.
    let tray = match MacosTray::new(flags.clone()) {
        Ok(tray) => Some(tray),
        Err(err) => {
            eprintln!("complete-me: tray unavailable: {err:?}");
            None
        }
    };

    let heartbeat = Duration::from_millis(config.heartbeat_ms);
    let mut tracker = FieldTracker::new();
    let mut latest = LatestRequest::new();
    let mut prev_enabled = true;
    let mut secure = false;
    let mut prev_secure = false;
    let mut last_secure_poll_ms: Option<u64> = None;
    let mut last_render: Option<(crate::status::AppStatus, bool)> = None;
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
                        if config.diag_coords {
                            if let Ok(rect) = adapter.caret_rect(&field) {
                                eprintln!(
                                    "complete-me: diag caret rect={rect:?} scales={:?}",
                                    display_scales()
                                );
                            }
                        }
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

        // 4. Derive status (permission/secure/ready/enabled) and update the tray.
        // Re-poll secure input and trust on a wall-clock throttle so granting
        // permission or a password field appearing is reflected without a restart.
        if last_secure_poll_ms
            .is_none_or(|last| now_ms.saturating_sub(last) >= SECURE_POLL_INTERVAL_MS)
        {
            secure = secure_input_enabled();
            trusted = accessibility_trusted();
            last_secure_poll_ms = Some(now_ms);
        }
        let enabled = flags.enabled.load(Ordering::Relaxed);
        // Hide any showing ghost on the disable or secure-on edge (gating only
        // blocks *new* requests; an already-visible ghost needs explicit dismiss).
        if should_dismiss(prev_enabled, enabled, prev_secure, secure) {
            let _ = log_err("on_dismiss", engine.on_dismiss());
        }
        prev_enabled = enabled;
        prev_secure = secure;
        let status = derive_status(trusted, secure, inference.is_ready(), enabled);
        // Only touch AppKit when the rendered state actually changed.
        if last_render != Some((status, enabled)) {
            if let Some(tray) = &tray {
                if let Err(err) = tray.set_status(
                    status.menu_title(),
                    status.status_line(),
                    enabled,
                    status.needs_accessibility(),
                ) {
                    eprintln!("complete-me: tray update failed: {err:?}");
                }
            }
            last_render = Some((status, enabled));
        }

        // 5. Submit the newest pending request only when suggestions are allowed
        // (Ready ⇒ trusted + not secure + warm + enabled).
        if status.suggestions_allowed() {
            if let Some(request) = latest.take() {
                eprintln!(
                    "complete-me: request gen={} prompt={:?}",
                    request.generation, request.prompt
                );
                inference.submit(request);
            }
        }

        // 6. Tray actions (menu callbacks fire on this same main thread via the
        // run-loop pump, so Relaxed is sufficient for these flags).
        if flags.open_settings.swap(false, Ordering::Relaxed) {
            if let Err(err) = Command::new("open")
                .arg(ACCESSIBILITY_SETTINGS_URL)
                .status()
            {
                eprintln!("complete-me: open settings failed: {err}");
            }
        }
        if flags.quit.load(Ordering::Relaxed) {
            eprintln!("complete-me: quit requested");
            break;
        }

        // 7. Bounded run (gates pass COMPLETE_ME_RUN_MS).
        if let Some(run_ms) = config.run_ms {
            if now_ms >= run_ms {
                break;
            }
        }

        // 8. Pump the main run loop: paces the loop and services the overlay.
        // SAFETY: `kCFRunLoopDefaultMode` is a Core Foundation extern static.
        let mode = unsafe { kCFRunLoopDefaultMode };
        CFRunLoop::run_in_mode(mode, heartbeat, false);
    }

    eprintln!("complete-me: shutting down");
    drop(tray); // remove the status item before AppKit teardown
    drop(caret_sub);
    drop(focus_sub);
    inference.shutdown();
    drop(engine); // drops overlay + accept subscription + the engine's adapter handle
    drop(adapter); // last Arc ref → AX worker thread stops
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Build a lookup closure from a list of key/value pairs.
    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn empty_environment_uses_defaults() {
        let config = Config::from_lookup(lookup(&[]));
        assert_eq!(config.acceptance_pid, None);
        assert_eq!(config.stub_completion, None);
        assert_eq!(config.model_path, PathBuf::from(DEFAULT_MODEL));
        assert_eq!(config.prompt_mode, PromptMode::Terse);
        assert_eq!(config.run_ms, None);
        assert_eq!(config.debounce_ms, DEFAULT_DEBOUNCE_MS);
        assert_eq!(config.max_words, DEFAULT_MAX_WORDS);
        assert_eq!(config.max_tokens, DEFAULT_MAX_TOKENS);
        assert_eq!(config.heartbeat_ms, DEFAULT_HEARTBEAT_MS);
        assert!(!config.diag_coords);
    }

    #[test]
    fn numeric_knobs_parse_and_clamp() {
        let config = Config::from_lookup(lookup(&[
            ("COMPLETE_ME_DEBOUNCE_MS", "60"),
            ("COMPLETE_ME_MAX_WORDS", "999"), // over max → clamps to 50
            ("COMPLETE_ME_MAX_TOKENS", "0"),  // under min → clamps to 1
            ("COMPLETE_ME_HEARTBEAT_MS", "500"), // over max → clamps to 100
        ]));
        assert_eq!(config.debounce_ms, 60);
        assert_eq!(config.max_words, 50);
        assert_eq!(config.max_tokens, 1);
        assert_eq!(config.heartbeat_ms, 100);
    }

    #[test]
    fn diag_coords_enabled_by_one_or_true() {
        assert!(Config::from_lookup(lookup(&[("COMPLETE_ME_DIAG_COORDS", "1")])).diag_coords);
        assert!(Config::from_lookup(lookup(&[("COMPLETE_ME_DIAG_COORDS", "true")])).diag_coords);
        assert!(!Config::from_lookup(lookup(&[("COMPLETE_ME_DIAG_COORDS", "no")])).diag_coords);
    }

    #[test]
    fn valid_pid_and_run_ms_parse() {
        let config = Config::from_lookup(lookup(&[
            ("COMPLETE_ME_ACCEPTANCE_PID", "8273"),
            ("COMPLETE_ME_RUN_MS", "4000"),
        ]));
        assert_eq!(config.acceptance_pid, Some(8273));
        assert_eq!(config.run_ms, Some(4000));
    }

    #[test]
    fn unparseable_pid_and_run_ms_fall_back_to_none() {
        let config = Config::from_lookup(lookup(&[
            ("COMPLETE_ME_ACCEPTANCE_PID", "not-a-number"),
            ("COMPLETE_ME_RUN_MS", "later"),
        ]));
        assert_eq!(config.acceptance_pid, None);
        assert_eq!(config.run_ms, None);
    }

    #[test]
    fn empty_stub_completion_is_treated_as_unset() {
        let config = Config::from_lookup(lookup(&[("COMPLETE_ME_STUB_COMPLETION", "")]));
        assert_eq!(config.stub_completion, None);
    }

    #[test]
    fn non_empty_stub_completion_is_kept() {
        let config = Config::from_lookup(lookup(&[("COMPLETE_ME_STUB_COMPLETION", " jumps")]));
        assert_eq!(config.stub_completion.as_deref(), Some(" jumps"));
    }

    #[test]
    fn model_path_override_wins_over_default() {
        let config = Config::from_lookup(lookup(&[("COMPLETE_ME_MODEL_PATH", "/models/x.gguf")]));
        assert_eq!(config.model_path, PathBuf::from("/models/x.gguf"));
    }

    #[test]
    fn prompt_mode_raw_is_parsed() {
        let config = Config::from_lookup(lookup(&[("COMPLETE_ME_PROMPT_MODE", "raw")]));
        assert_eq!(config.prompt_mode, PromptMode::Raw);
    }
}
