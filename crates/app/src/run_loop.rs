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
use personalization::{PersonalizationProfile, SenderIdentity, Strength};
use platform::{
    AcceptAction, Capabilities, FieldHandle, InsertStrategy, KeyInterceptMode, OverlayPlacement,
    PlatformAdapter, PlatformError, ScreenRect, SecurityState, TapControl, Toolkit,
};
use platform_macos::{
    accessibility_trusted, bundle_id_for_pid, display_scales, prompt_accessibility_trust,
    read_pasteboard_text, request_screen_recording_permission, screen_context_text,
    screen_recording_permission, secure_input_enabled, MacosOverlayPresenter, MacosPlatformAdapter,
    MacosTray, TrayFlags,
};
use prefs::Prefs;

use crate::adapter::SharedAdapter;
use crate::config::{self, parse_clamped};
use crate::inference::{InferenceHandle, PreviousInputs, WorkerContext};
use crate::model_select::{load_model, resolve_prompt_mode, resolve_source, PromptMode};
use crate::status::{derive_status, AppStatus, BlockReason};
use crate::wiring::{FieldTracker, LatestRequest, Observation};

const DEFAULT_DEBOUNCE_MS: u64 = 120;
const DEFAULT_MAX_WORDS: usize = 8;
const DEFAULT_MIN_CONTEXT_CHARS: usize = 3;
const DEFAULT_MAX_TOKENS: usize = 24;
const DEFAULT_HEARTBEAT_MS: u64 = 12;
/// Candidate completions generated per request (1 = single, up to 5 for cycle).
const DEFAULT_CANDIDATES: usize = 1;
/// Per-source character bound when previous-input context is enabled truthily.
const DEFAULT_CONTEXT_MAX_CHARS: usize = 160;
const DEFAULT_MODEL: &str = "tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf";
/// Re-poll secure input + Accessibility trust at most this often (wall-clock ms).
const SECURE_POLL_INTERVAL_MS: u64 = 480;
const ACCESSIBILITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

/// Set by the SIGINT/SIGTERM handler; observed by the loop to begin shutdown.
static STOP: AtomicBool = AtomicBool::new(false);
/// Set by the SIGUSR1 handler; observed by the loop to toggle enable/disable
/// (a headless equivalent of the tray's Enable item, also handy for scripting).
static TOGGLE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: libc::c_int) {
    // Async-signal-safe: only a relaxed atomic store.
    STOP.store(true, Ordering::Relaxed);
}

extern "C" fn on_toggle(_sig: libc::c_int) {
    TOGGLE.store(true, Ordering::Relaxed);
}

fn install_signal_handlers() {
    let stop = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    let toggle = on_toggle as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: installing handlers that only set atomic flags is safe.
    unsafe {
        libc::signal(libc::SIGINT, stop);
        libc::signal(libc::SIGTERM, stop);
        libc::signal(libc::SIGUSR1, toggle);
    }
}

/// What a platform callback enqueues for the main loop to process.
#[derive(Clone, Debug, PartialEq)]
enum HostEvent {
    Focus(FieldHandle),
    Caret(FieldHandle, Option<ScreenRect>),
    Accept(AcceptAction),
    /// Esc: dismiss the ghost and suppress completions in the current field.
    Dismiss,
    /// Down arrow: rotate to the next candidate (multi-candidate cycle).
    Cycle,
}

/// Collapse a burst of consecutive same-field `Caret` events into just the last
/// one. Each `Caret` triggers an AX `read_context` round-trip; when several land
/// in one heartbeat drain for the same field, only the newest read matters — the
/// earlier reads would be immediately superseded. Dropping them removes redundant
/// AX traffic with zero added latency (the surviving event carries the latest
/// rect). A run is only collapsed across *adjacent* same-field carets, so an
/// intervening `Focus`/`Accept` (which changes engine state) always breaks it.
fn coalesce_caret_reads(events: Vec<HostEvent>) -> Vec<HostEvent> {
    let mut out: Vec<HostEvent> = Vec::with_capacity(events.len());
    let mut iter = events.into_iter().peekable();
    while let Some(event) = iter.next() {
        if let HostEvent::Caret(field, _) = &event {
            if let Some(HostEvent::Caret(next_field, _)) = iter.peek() {
                if next_field == field {
                    // Superseded by the next same-field caret read; drop this one.
                    continue;
                }
            }
        }
        out.push(event);
    }
    out
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
    min_context_chars: usize,
    allow_mid_word: bool,
    diag_coords: bool,
    candidates: usize,
    context_max_chars: usize,
    clipboard_context: bool,
    screen_context: bool,
    personalization: PersonalizationProfile,
    prefs: Prefs,
}

impl Config {
    /// Build config by layering the environment over the optional config file
    /// (env wins over file wins over default), all through `from_lookup`.
    fn from_env() -> Self {
        let file_map = config::config_file_path()
            .map(|path| config::load_file_map(&path))
            .unwrap_or_default();
        Self::from_lookup(move |key| layered(env::var(key).ok(), file_map.get(key).cloned()))
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
            min_context_chars: parse_clamped(
                lookup("COMPLETE_ME_MIN_CONTEXT"),
                DEFAULT_MIN_CONTEXT_CHARS,
                0,
                100,
            ),
            // Conservative default: suppress mid-word completions (engine-macos
            // design §4 trigger gating + plan-review F5, "protect first-run").
            // `COMPLETE_ME_MIDLINE=1` opts into them.
            allow_mid_word: lookup("COMPLETE_ME_MIDLINE").is_some_and(|v| v == "1" || v == "true"),
            diag_coords: lookup("COMPLETE_ME_DIAG_COORDS").is_some_and(|v| v == "1" || v == "true"),
            candidates: parse_clamped(lookup("COMPLETE_ME_CANDIDATES"), DEFAULT_CANDIDATES, 1, 5),
            context_max_chars: parse_context_max_chars(lookup(
                "COMPLETE_ME_PREVIOUS_INPUT_CONTEXT",
            )),
            clipboard_context: lookup("COMPLETE_ME_CLIPBOARD_CONTEXT")
                .is_some_and(|v| v == "1" || v == "true"),
            screen_context: lookup("COMPLETE_ME_SCREEN_CONTEXT")
                .is_some_and(|v| v == "1" || v == "true"),
            personalization: build_personalization(&lookup),
            prefs: build_prefs(&lookup),
        }
    }
}

/// Build the personalization profile from config (A2 §6). Per-app/per-domain
/// instruction maps are an A3 settings concern; A2 wires the global instructions,
/// strength stop, and sender identity, which are enough to steer completions.
fn build_personalization(lookup: &impl Fn(&str) -> Option<String>) -> PersonalizationProfile {
    let mut profile = PersonalizationProfile {
        global_instructions: lookup("COMPLETE_ME_INSTRUCTIONS").unwrap_or_default(),
        sender: SenderIdentity {
            name: lookup("COMPLETE_ME_SENDER_NAME").unwrap_or_default(),
            email: lookup("COMPLETE_ME_SENDER_EMAIL").unwrap_or_default(),
        },
        ..Default::default()
    };
    if let Some(stop) = lookup("COMPLETE_ME_STRENGTH").and_then(|raw| raw.parse::<u8>().ok()) {
        profile.strength = Strength::from_stop(stop);
    }
    profile
}

/// Parse the previous-input context setting (A2 §16): off by default; an explicit
/// falsy value is off; a positive number is the per-source char bound; any other
/// truthy value uses the default bound.
fn parse_context_max_chars(raw: Option<String>) -> usize {
    match raw {
        None => 0,
        Some(v) => {
            let v = v.trim();
            if matches!(
                v.to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no" | ""
            ) {
                0
            } else {
                v.parse::<usize>()
                    .map(|n| n.min(2000))
                    .unwrap_or(DEFAULT_CONTEXT_MAX_CHARS)
            }
        }
    }
}

/// Log one-time compatibility guidance for an app by its tier (A2 §16
/// onboarding): setup-needed browsers (Google Docs/Arc), mirror-window apps,
/// partial/sidebar-only apps, and unsupported apps.
fn log_compat_guidance(app: &str) {
    use compat::CompatTier;
    match compat::compatibility_tier(app) {
        CompatTier::SetupNeeded => eprintln!(
            "complete-me: {app} needs setup for inline suggestions \
             (e.g. Google Docs Accessibility / Text Metrics)"
        ),
        CompatTier::MirrorOnly => {
            eprintln!("complete-me: {app} renders via a mirror window (inline overlay unsupported)")
        }
        CompatTier::Partial => eprintln!("complete-me: {app} has partial support"),
        CompatTier::SidebarOnly => eprintln!(
            "complete-me: {app} suggests in AI-chat/sidebar fields only, not the editor pane"
        ),
        CompatTier::Unsupported => {
            eprintln!("complete-me: {app} is not supported — suggestions disabled")
        }
        CompatTier::Works | CompatTier::Unknown => {}
    }
}

/// Whether the focused app's compatibility tier permits suggestions (A2 §16):
/// the `Unsupported` tier hard-blocks. Unresolved app → allow (fail-open), since
/// the field's own capabilities still gate.
fn app_allows_suggestions(app_key: Option<&str>) -> bool {
    app_key.is_none_or(|app| compat::compatibility_tier(app).allows_suggestions())
}

/// Resolve a focused field's pid to a stable bundle id for per-app preferences.
/// Pure over the resolver so the wiring is testable without AppKit; the runtime
/// passes `bundle_id_for_pid`. Returns `None` (fail-open) when there is no pid or
/// the bundle id can't be resolved.
fn resolve_app_key(pid: Option<u32>, resolver: impl Fn(i32) -> Option<String>) -> Option<String> {
    pid.and_then(|p| i32::try_from(p).ok()).and_then(resolver)
}

/// Parse a fail-safe boolean: only explicit falsy values disable; anything else
/// (incl. unrecognized strings) keeps the safe default so a typo never silently
/// turns the whole product off.
fn parse_enabled_default(raw: Option<String>) -> bool {
    match raw {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        None => true,
    }
}

/// Build suggestion-gating preferences from config (A2 §8). A comma-separated
/// app-exclude list and a default-enabled toggle; finer per-app/domain overrides
/// are an A3 settings concern.
fn build_prefs(lookup: &impl Fn(&str) -> Option<String>) -> Prefs {
    let excluded_apps = lookup("COMPLETE_ME_EXCLUDED_APPS")
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Prefs {
        default_enabled: parse_enabled_default(lookup("COMPLETE_ME_DEFAULT_ENABLED")),
        excluded_apps,
        ..Default::default()
    }
}

/// Resolve one config key with env-over-file precedence: the environment value
/// wins, falling back to the file value, else `None` (so `from_lookup` applies
/// the default). Extracted so the precedence direction is unit-testable without
/// mutating the process environment.
fn layered(env_value: Option<String>, file_value: Option<String>) -> Option<String> {
    env_value.or(file_value)
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

/// The engine-state transition implied by a change in global Secure Input,
/// derived purely so the run loop's edge handling is unit-testable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SecureEdge {
    /// Secure Input just turned on — block the engine and drop queued work.
    Enter,
    /// Secure Input just cleared (and Accessibility is trusted) — rehydrate the
    /// focused field's capabilities so the machine unblocks without a new focus.
    ClearRehydrate,
    /// No secure transition this tick.
    None,
}

fn secure_edge(prev_secure: bool, secure: bool, trusted: bool) -> SecureEdge {
    match (prev_secure, secure) {
        (false, true) => SecureEdge::Enter,
        (true, false) if trusted => SecureEdge::ClearRehydrate,
        // Cleared-but-untrusted stays blocked by Permission until trust returns.
        _ => SecureEdge::None,
    }
}

/// Whether disabling (enabled true→false) should dismiss the suggestion and drop
/// queued requests. Pure so the run loop's enable-edge handling is testable.
fn should_dismiss_on_disable(prev_enabled: bool, enabled: bool) -> bool {
    prev_enabled && !enabled
}

fn secure_input_caps() -> Capabilities {
    Capabilities {
        readable_text: false,
        readable_caret: false,
        writable: false,
        secure: true,
        security_state: SecurityState::SecureInputEnabled,
        toolkit: Toolkit::Unknown("secure input".into()),
        multiline: false,
        insert_strategy: InsertStrategy::None,
        accept_intercept: KeyInterceptMode::None,
        overlay_at_caret: OverlayPlacement::None,
        coords_global_screen: false,
    }
}

fn status_drops_pending_requests(status: AppStatus) -> bool {
    matches!(
        status,
        AppStatus::Disabled
            | AppStatus::Blocked(BlockReason::Permission | BlockReason::SecureInput)
    )
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
    )
    .with_trigger_gates(config.min_context_chars, config.allow_mid_word);

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
        .subscribe_accept(Arc::new(move |control| {
            let event = match control {
                TapControl::Accept(action) => HostEvent::Accept(action),
                TapControl::Dismiss => HostEvent::Dismiss,
                TapControl::Cycle => HostEvent::Cycle,
            };
            if let Ok(tx) = accept_tx.lock() {
                let _ = tx.send(event);
            }
        }))
        .map_err(|err| format!("subscribe accept: {err:?}"))?;
    engine.set_accept_subscription(accept_sub);

    let model = load_model(resolve_source(
        config.stub_completion.clone(),
        config.model_path.clone(),
    ))?;
    // Screen-recording context (optional, A2 §16): request the permission once if
    // the user opted in. The app continues with field-only context if denied
    // (the "works without it" requirement); local OCR enrichment rides on this
    // grant.
    if config.screen_context && !screen_recording_permission() {
        eprintln!("complete-me: requesting Screen Recording permission (screen context)");
        request_screen_recording_permission();
        // The grant takes effect on the NEXT launch (macOS shows the prompt async
        // and re-reads TCC at startup), so screen context is inactive this run.
        eprintln!("complete-me: restart after granting Screen Recording to enable screen context");
    }

    let previous_inputs = PreviousInputs::default();
    let clipboard_cell: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let screen_cell: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    // Screen OCR only contributes when the grant is actually present this session.
    let screen_active = config.screen_context && screen_recording_permission();
    // Clipboard/screen context work independently of previous-input context, so
    // the worker needs a positive char bound when any of them is enabled.
    let context_bound =
        if (config.clipboard_context || screen_active) && config.context_max_chars == 0 {
            DEFAULT_CONTEXT_MAX_CHARS
        } else {
            config.context_max_chars
        };
    let worker_context = WorkerContext {
        previous_inputs: previous_inputs.clone(),
        clipboard: Arc::clone(&clipboard_cell),
        screen: Arc::clone(&screen_cell),
        max_chars: context_bound,
    };
    let inference = InferenceHandle::spawn(
        model,
        config.prompt_mode,
        config.personalization.clone(),
        config.candidates,
        worker_context,
    )?;

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
    let mut current_field: Option<FieldHandle> = None;
    let mut hinted_apps: std::collections::HashSet<String> = std::collections::HashSet::new();
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
        // Drain the queue first, then collapse bursts of same-field caret reads so
        // we issue at most one AX round-trip per field per heartbeat.
        let drained: Vec<HostEvent> = rx.try_iter().collect();
        for event in coalesce_caret_reads(drained) {
            match event {
                HostEvent::Focus(field) => {
                    eprintln!("complete-me: focus {}", field.element_id);
                    // Compatibility onboarding (A2 §16): surface tier-specific
                    // guidance once per app (mirror-window apps, setup-needed
                    // browsers like Google Docs/Arc).
                    if let Some(app) = resolve_app_key(field.pid, bundle_id_for_pid) {
                        // MirrorOnly apps (Firefox/Zen) render the ghost in the
                        // floating mirror window, not inline (A2 §16).
                        engine.set_mirror_mode(matches!(
                            compat::compatibility_tier(&app),
                            compat::CompatTier::MirrorOnly
                        ));
                        if hinted_apps.insert(app.clone()) {
                            log_compat_guidance(&app);
                        }
                    } else {
                        engine.set_mirror_mode(false);
                    }
                    current_field = Some(field.clone());
                    tracker.reset();
                    offer_all(&mut latest, log_err("on_focus", engine.on_focus(field)));
                }
                HostEvent::Caret(field, _rect) => match adapter.read_context(&field) {
                    // One selection-changed notification covers both typing and a
                    // bare cursor move. Typing schedules a completion; a cursor
                    // move only invalidates a showing ghost (no re-request).
                    Ok(ctx) => {
                        current_field = Some(field.clone());
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
                    Err(err) => {
                        eprintln!("complete-me: read_context: {err:?}");
                        // Setup-needed onboarding (A2 §16): a browser/Arc/Dia field
                        // that won't read may need Accessibility/Text-Metrics setup
                        // (the Google-Docs-in-Chrome case). Surface guidance once.
                        if let Some(app) = resolve_app_key(field.pid, bundle_id_for_pid) {
                            if compat::needs_accessibility_setup(&app, false)
                                && hinted_apps.insert(format!("setup:{app}"))
                            {
                                eprintln!(
                                    "complete-me: {app} field not readable — may need \
                                     Accessibility/Text-Metrics setup (e.g. Google Docs)"
                                );
                            }
                        }
                    }
                },
                HostEvent::Accept(action) => {
                    eprintln!("complete-me: accept {action:?}");
                    let self_insert = (action == AcceptAction::Word)
                        .then(|| engine.preview_accept_insert(action))
                        .flatten();
                    // Record only *full* accepts as previous-input context: a full
                    // completion is meaningful prior text, whereas a single word
                    // (the Word-accept payload) is low-signal. Redacted + per-app.
                    if config.context_max_chars > 0 && action == AcceptAction::Full {
                        if let (Some(field), Some((_, text))) =
                            (current_field.as_ref(), engine.preview_accept_insert(action))
                        {
                            previous_inputs.record(&field.app, redaction::redact(&text));
                        }
                    }
                    match engine.on_accept(action) {
                        Ok(requests) => {
                            if let Some((field, text)) = self_insert {
                                tracker.apply_self_insert(&field, &text);
                            }
                            offer_all(&mut latest, requests);
                        }
                        Err(err) => eprintln!("complete-me: on_accept error: {err:?}"),
                    }
                }
                HostEvent::Dismiss => {
                    eprintln!("complete-me: dismiss (Esc)");
                    offer_all(
                        &mut latest,
                        log_err("on_dismiss_suppress", engine.on_dismiss_suppress()),
                    );
                }
                HostEvent::Cycle => {
                    eprintln!("complete-me: cycle candidate");
                    offer_all(&mut latest, log_err("on_cycle", engine.on_cycle()));
                }
            }
        }

        // 2. Inference outcomes → engine (stale ones are discarded internally).
        for outcome in inference.drain_outcomes() {
            eprintln!(
                "complete-me: completion gen={} candidates={:?}",
                outcome.request.generation, outcome.candidates
            );
            offer_all(
                &mut latest,
                log_err(
                    "on_completion",
                    engine.on_completion_multi(&outcome.request, outcome.candidates),
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
        // SIGUSR1 toggles enable/disable (headless equivalent of the tray item).
        if TOGGLE.swap(false, Ordering::Relaxed) {
            let now = flags.enabled.load(Ordering::Relaxed);
            flags.enabled.store(!now, Ordering::Relaxed);
        }
        let enabled = flags.enabled.load(Ordering::Relaxed);
        let status = derive_status(trusted, secure, inference.is_ready(), enabled);
        // Secure input is a true engine-state transition, not only a UI state:
        // clear queued work and invalidate the machine so held requests cannot
        // submit after the secure block clears.
        match secure_edge(prev_secure, secure, trusted) {
            SecureEdge::Enter => {
                latest.clear();
                offer_all(
                    &mut latest,
                    log_err(
                        "on_secure_state",
                        engine.on_secure_state(secure_input_caps()),
                    ),
                );
            }
            SecureEdge::ClearRehydrate => {
                // Rehydrate capabilities for the current field after the secure
                // global block clears; otherwise the machine would stay blocked
                // until a fresh focus event arrives.
                if let Some(field) = current_field.clone() {
                    tracker.reset();
                    offer_all(&mut latest, log_err("on_focus", engine.on_focus(field)));
                }
            }
            SecureEdge::None => {}
        }
        // Disabling is user policy: dismiss visible UI and drop queued requests.
        if should_dismiss_on_disable(prev_enabled, enabled) {
            latest.clear();
            let _ = log_err("on_dismiss", engine.on_dismiss());
        }
        if status_drops_pending_requests(status) {
            latest.clear();
        }
        prev_enabled = enabled;
        prev_secure = secure;
        // Only touch AppKit when the rendered state actually changed.
        if last_render != Some((status, enabled)) {
            eprintln!("complete-me: status={status:?} enabled={enabled}");
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
                // Per-app/domain gating + pause/snooze (A2 §8). The exclude list
                // is keyed on bundle ids, so resolve the focused pid to a bundle
                // id (the field's own `app` is a volatile `pid:N`); fail-open if
                // it can't be resolved. Domain is None until browser-domain
                // extraction lands.
                let app_key = resolve_app_key(request.field.pid, bundle_id_for_pid);
                // Terminal apps only suggest for natural-language agent prompts,
                // not shell commands (A2 §16).
                let terminal_ok = app_key
                    .as_deref()
                    .is_none_or(|app| compat::terminal_prompt_activates(app, &request.prompt));
                if app_allows_suggestions(app_key.as_deref())
                    && terminal_ok
                    && config
                        .prefs
                        .should_suggest(app_key.as_deref(), None, now_ms)
                {
                    // Refresh the clipboard context cell (redacted) just before a
                    // submit that will use it (A2 §16 clipboard context). Invariant:
                    // the cell is rewritten before *every* gated submit, so the
                    // worker (which reads the latest cell for the surviving
                    // coalesced request) never attaches a prior app's clipboard.
                    if config.clipboard_context {
                        let clip = read_pasteboard_text().map(|text| redaction::redact(&text));
                        *clipboard_cell.lock().unwrap_or_else(|e| e.into_inner()) = clip;
                    }
                    // Screen-aware context (A2 §16): local OCR of the display,
                    // redacted, refreshed before a submit that will use it.
                    if screen_active {
                        let screen = screen_context_text(DEFAULT_CONTEXT_MAX_CHARS)
                            .map(|text| redaction::redact(&text));
                        *screen_cell.lock().unwrap_or_else(|e| e.into_inner()) = screen;
                    }
                    eprintln!(
                        "complete-me: request gen={} prompt={:?}",
                        request.generation, request.prompt
                    );
                    inference.submit(request);
                }
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
    fn personalization_built_from_config_keys() {
        let profile = build_personalization(&lookup(&[
            ("COMPLETE_ME_INSTRUCTIONS", "Be terse."),
            ("COMPLETE_ME_STRENGTH", "5"),
            ("COMPLETE_ME_SENDER_NAME", "Ada"),
        ]));
        assert_eq!(profile.strength, Strength::Max);
        let preamble = profile.build_preamble(Some("com.apple.TextEdit"), None);
        assert!(preamble.contains("Be terse."));
        assert!(preamble.contains("Ada"));
    }

    #[test]
    fn personalization_defaults_to_no_steer_when_keys_absent() {
        let profile = build_personalization(&lookup(&[]));
        assert_eq!(profile.build_preamble(Some("com.apple.TextEdit"), None), "");
    }

    #[test]
    fn prefs_built_from_excluded_apps_list() {
        let prefs = build_prefs(&lookup(&[(
            "COMPLETE_ME_EXCLUDED_APPS",
            "com.apple.Finder, com.tinyspeck.slackmacgap",
        )]));
        assert!(!prefs.should_suggest(Some("com.apple.Finder"), None, 0));
        assert!(!prefs.should_suggest(Some("com.tinyspeck.slackmacgap"), None, 0));
        assert!(prefs.should_suggest(Some("com.apple.TextEdit"), None, 0));
    }

    #[test]
    fn prefs_default_enabled_fails_safe() {
        // Absent or unrecognized → enabled (a typo never silently kills the app);
        // only explicit falsy values disable.
        assert!(build_prefs(&lookup(&[])).default_enabled);
        assert!(build_prefs(&lookup(&[("COMPLETE_ME_DEFAULT_ENABLED", "yes")])).default_enabled);
        assert!(build_prefs(&lookup(&[("COMPLETE_ME_DEFAULT_ENABLED", "True")])).default_enabled);
        assert!(!build_prefs(&lookup(&[("COMPLETE_ME_DEFAULT_ENABLED", "0")])).default_enabled);
        assert!(!build_prefs(&lookup(&[("COMPLETE_ME_DEFAULT_ENABLED", "off")])).default_enabled);
    }

    #[test]
    fn clipboard_and_screen_context_flags_default_off() {
        let off = Config::from_lookup(lookup(&[]));
        assert!(!off.clipboard_context);
        assert!(!off.screen_context);
        let on = Config::from_lookup(lookup(&[
            ("COMPLETE_ME_CLIPBOARD_CONTEXT", "1"),
            ("COMPLETE_ME_SCREEN_CONTEXT", "true"),
        ]));
        assert!(on.clipboard_context);
        assert!(on.screen_context);
    }

    #[test]
    fn unsupported_apps_are_gated_out() {
        assert!(!app_allows_suggestions(Some("com.mitchellh.ghostty")));
        assert!(app_allows_suggestions(Some("com.apple.TextEdit")));
        // Unresolved app → fail-open (field capabilities still gate).
        assert!(app_allows_suggestions(None));
    }

    #[test]
    fn context_max_chars_parsing_is_off_by_default_and_fail_safe() {
        assert_eq!(parse_context_max_chars(None), 0);
        assert_eq!(parse_context_max_chars(Some("off".into())), 0);
        assert_eq!(parse_context_max_chars(Some("0".into())), 0);
        assert_eq!(parse_context_max_chars(Some("150".into())), 150);
        assert_eq!(
            parse_context_max_chars(Some("true".into())),
            DEFAULT_CONTEXT_MAX_CHARS
        );
        assert_eq!(parse_context_max_chars(Some("99999".into())), 2000); // clamped
    }

    #[test]
    fn resolve_app_key_maps_pid_to_bundle_id() {
        let resolver = |pid: i32| (pid == 42).then(|| "com.apple.TextEdit".to_string());
        assert_eq!(
            resolve_app_key(Some(42), resolver),
            Some("com.apple.TextEdit".into())
        );
        // Unresolvable pid or absent pid → None (fail-open gating).
        assert_eq!(resolve_app_key(Some(99), resolver), None);
        assert_eq!(resolve_app_key(None, resolver), None);
    }

    #[test]
    fn layered_lookup_prefers_env_then_file_then_none() {
        // env wins over file (the P1 "env > file > default" precedence).
        assert_eq!(
            layered(Some("env".into()), Some("file".into())),
            Some("env".into())
        );
        // file is the fallback when env is absent.
        assert_eq!(layered(None, Some("file".into())), Some("file".into()));
        // neither present → None, so `from_lookup` applies the built-in default.
        assert_eq!(layered(None, None), None);
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
        assert_eq!(config.min_context_chars, DEFAULT_MIN_CONTEXT_CHARS);
        assert!(!config.allow_mid_word); // conservative default: mid-word suppressed
        assert!(!config.diag_coords);
    }

    #[test]
    fn min_context_parses_and_clamps() {
        assert_eq!(
            Config::from_lookup(lookup(&[("COMPLETE_ME_MIN_CONTEXT", "5")])).min_context_chars,
            5
        );
        // over max → clamps to 100
        assert_eq!(
            Config::from_lookup(lookup(&[("COMPLETE_ME_MIN_CONTEXT", "999")])).min_context_chars,
            100
        );
        // unparseable → default
        assert_eq!(
            Config::from_lookup(lookup(&[("COMPLETE_ME_MIN_CONTEXT", "lots")])).min_context_chars,
            DEFAULT_MIN_CONTEXT_CHARS
        );
    }

    #[test]
    fn midline_opt_in_by_one_or_true() {
        assert!(Config::from_lookup(lookup(&[("COMPLETE_ME_MIDLINE", "1")])).allow_mid_word);
        assert!(Config::from_lookup(lookup(&[("COMPLETE_ME_MIDLINE", "true")])).allow_mid_word);
        assert!(!Config::from_lookup(lookup(&[("COMPLETE_ME_MIDLINE", "no")])).allow_mid_word);
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
    fn numeric_knobs_fall_back_to_defaults_when_unparseable() {
        let config = Config::from_lookup(lookup(&[
            ("COMPLETE_ME_DEBOUNCE_MS", "fast"),
            ("COMPLETE_ME_MAX_WORDS", "many"),
            ("COMPLETE_ME_MAX_TOKENS", "lots"),
            ("COMPLETE_ME_HEARTBEAT_MS", "soon"),
        ]));
        assert_eq!(config.debounce_ms, DEFAULT_DEBOUNCE_MS);
        assert_eq!(config.max_words, DEFAULT_MAX_WORDS);
        assert_eq!(config.max_tokens, DEFAULT_MAX_TOKENS);
        assert_eq!(config.heartbeat_ms, DEFAULT_HEARTBEAT_MS);
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

    #[test]
    fn only_unavailable_statuses_drop_pending_requests() {
        assert!(!status_drops_pending_requests(AppStatus::Loading));
        assert!(!status_drops_pending_requests(AppStatus::Ready));
        assert!(status_drops_pending_requests(AppStatus::Disabled));
        assert!(status_drops_pending_requests(AppStatus::Blocked(
            BlockReason::Permission
        )));
        assert!(status_drops_pending_requests(AppStatus::Blocked(
            BlockReason::SecureInput
        )));
    }

    #[test]
    fn secure_input_caps_are_non_interactive_and_secure() {
        let caps = secure_input_caps();
        assert!(!caps.readable_text);
        assert!(!caps.readable_caret);
        assert!(!caps.writable);
        assert!(caps.secure);
        assert_eq!(caps.security_state, SecurityState::SecureInputEnabled);
        assert_eq!(caps.insert_strategy, InsertStrategy::None);
        assert_eq!(caps.accept_intercept, KeyInterceptMode::None);
        assert_eq!(caps.overlay_at_caret, OverlayPlacement::None);
    }

    fn host_field(id: &str) -> FieldHandle {
        FieldHandle {
            app: "TextEdit".into(),
            pid: Some(7),
            element_id: id.into(),
            generation: 1,
        }
    }

    fn rect(x: f64) -> Option<ScreenRect> {
        Some(ScreenRect {
            x,
            y: 0.0,
            w: 1.0,
            h: 14.0,
        })
    }

    fn req(generation: u64) -> CompletionRequest {
        CompletionRequest {
            generation,
            field: host_field("f"),
            snapshot: generation,
            prompt: "p".into(),
            max_tokens: 8,
        }
    }

    #[test]
    fn log_err_passes_through_ok_requests() {
        let out = log_err("x", Ok(vec![req(1), req(2)]));
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn log_err_swallows_errors_into_empty_vec() {
        // The "one failed effect never kills the loop" guarantee: an Err becomes
        // an empty request list (logged), not a propagated failure.
        let out = log_err("x", Err(PlatformError::Timeout));
        assert!(out.is_empty());
    }

    #[test]
    fn offer_all_keeps_newest_request() {
        let mut latest = LatestRequest::new();
        offer_all(&mut latest, vec![req(1), req(3), req(2)]);
        assert_eq!(latest.take().unwrap().generation, 3);
    }

    #[test]
    fn secure_edge_detects_enter() {
        assert_eq!(secure_edge(false, true, true), SecureEdge::Enter);
        assert_eq!(secure_edge(false, true, false), SecureEdge::Enter);
    }

    #[test]
    fn secure_edge_clears_only_when_trusted() {
        assert_eq!(secure_edge(true, false, true), SecureEdge::ClearRehydrate);
        // Cleared but Accessibility not (yet) trusted → stay blocked, no rehydrate.
        assert_eq!(secure_edge(true, false, false), SecureEdge::None);
    }

    #[test]
    fn secure_edge_none_when_unchanged() {
        assert_eq!(secure_edge(false, false, true), SecureEdge::None);
        assert_eq!(secure_edge(true, true, true), SecureEdge::None);
    }

    #[test]
    fn dismiss_only_on_enabled_to_disabled_edge() {
        assert!(should_dismiss_on_disable(true, false));
        assert!(!should_dismiss_on_disable(false, false)); // already disabled
        assert!(!should_dismiss_on_disable(false, true)); // re-enabling
        assert!(!should_dismiss_on_disable(true, true)); // still enabled
    }

    #[test]
    fn coalesce_empty_drain_is_empty() {
        assert_eq!(coalesce_caret_reads(vec![]), vec![]);
    }

    #[test]
    fn coalesce_keeps_a_lone_caret() {
        let events = vec![HostEvent::Caret(host_field("a"), rect(1.0))];
        assert_eq!(coalesce_caret_reads(events.clone()), events);
    }

    #[test]
    fn coalesce_collapses_adjacent_same_field_carets_to_the_last() {
        let events = vec![
            HostEvent::Caret(host_field("a"), rect(1.0)),
            HostEvent::Caret(host_field("a"), rect(2.0)),
            HostEvent::Caret(host_field("a"), rect(3.0)),
        ];
        // Only the newest read survives, carrying the latest rect.
        assert_eq!(
            coalesce_caret_reads(events),
            vec![HostEvent::Caret(host_field("a"), rect(3.0))]
        );
    }

    #[test]
    fn coalesce_keeps_carets_for_different_fields() {
        let events = vec![
            HostEvent::Caret(host_field("a"), rect(1.0)),
            HostEvent::Caret(host_field("b"), rect(2.0)),
        ];
        assert_eq!(coalesce_caret_reads(events.clone()), events);
    }

    #[test]
    fn coalesce_does_not_cross_a_focus_event() {
        // Focus changes engine state, so the caret before it must still be read.
        let events = vec![
            HostEvent::Caret(host_field("a"), rect(1.0)),
            HostEvent::Focus(host_field("a")),
            HostEvent::Caret(host_field("a"), rect(2.0)),
        ];
        assert_eq!(coalesce_caret_reads(events.clone()), events);
    }

    #[test]
    fn coalesce_does_not_cross_an_accept_event() {
        let events = vec![
            HostEvent::Caret(host_field("a"), rect(1.0)),
            HostEvent::Accept(AcceptAction::Full),
            HostEvent::Caret(host_field("a"), rect(2.0)),
        ];
        assert_eq!(coalesce_caret_reads(events.clone()), events);
    }

    #[test]
    fn coalesce_passes_non_caret_events_through() {
        let events = vec![
            HostEvent::Focus(host_field("a")),
            HostEvent::Accept(AcceptAction::Word),
        ];
        assert_eq!(coalesce_caret_reads(events.clone()), events);
    }

    #[test]
    fn coalesce_collapses_only_within_runs() {
        // a,a -> last a ; then b ; then a,a -> last a. Two runs collapse
        // independently around the intervening different-field caret.
        let events = vec![
            HostEvent::Caret(host_field("a"), rect(1.0)),
            HostEvent::Caret(host_field("a"), rect(2.0)),
            HostEvent::Caret(host_field("b"), rect(3.0)),
            HostEvent::Caret(host_field("a"), rect(4.0)),
            HostEvent::Caret(host_field("a"), rect(5.0)),
        ];
        assert_eq!(
            coalesce_caret_reads(events),
            vec![
                HostEvent::Caret(host_field("a"), rect(2.0)),
                HostEvent::Caret(host_field("b"), rect(3.0)),
                HostEvent::Caret(host_field("a"), rect(5.0)),
            ]
        );
    }
}
