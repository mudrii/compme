//! The main-thread run loop: the place where every proven part meets.
//!
//! Threading model (see the P0 design spec):
//! - This loop runs on the AppKit **main thread**. It owns the `Engine` and the
//!   `OverlayPresenterImpl`; the engine applies overlay commands internally, and
//!   the overlay enforces the main thread at runtime.
//! - Platform focus/caret/accept callbacks fire on the adapter's **dispatcher
//!   thread**; they only enqueue a `HostEvent` (cheap, no AX work).
//! - Inference runs on its own thread (`InferenceHandle`).
//! - Each iteration drains queued host events and inference outcomes, ticks the
//!   engine, submits the newest pending request, then pumps the host event loop
//!   for one heartbeat (which paces the loop and services the overlay).

use std::collections::{HashMap, VecDeque};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use emoji::{EmojiPrefs, Gender, SkinTone};
use engine::{CompletionRequest, Engine, RequestKind, TriggerPolicy};
use personalization::{PersonalizationProfile, Strength};
use platform::{
    env_flag_on,
    shell::{ShellHost, TrayHandle},
    AcceptAction, AcceptSubscription, Capabilities, CorrectionRange, FieldHandle, InsertStrategy,
    KeyInterceptMode, OverlayPlacement, OverlayPresenter, PlatformAdapter, PlatformError,
    ScreenRect, SecurityState, ShortcutAction, Subscription, TapControl, TextContext, Toolkit,
};
use prefs::Prefs;
use shell_flags::{DisableArm, TrayFlags};
use zeroize::Zeroize;

use crate::adapter::SharedAdapter;
use crate::builders::{
    app_support_models_dir, build_emoji_prefs, build_personalization, build_prefs, comma_list,
    downloaded_model_to_adopt, emoji_config_enabled, emoji_gender_from_index, emoji_gender_index,
    emoji_gender_value, emoji_skin_tone_from_index, emoji_skin_tone_index, emoji_skin_tone_value,
    layered, model_download_dest_present, model_download_ram_block_message, parse_enabled_default,
    prepare_model_download_dest, show_models_folder_with, validate_gguf_model, EMOJI_GENDER_VALUES,
    EMOJI_SKIN_TONE_VALUES,
};
#[cfg(test)]
use crate::builders::{parse_gender, parse_skin_tone};
use crate::config::{self, parse_clamped};
#[cfg(test)]
use crate::context_policy::context_bound_chars;
use crate::context_policy::{
    apply_clipboard_context_edge, apply_screen_context_edge, settings_context_bound_chars,
    ScreenContextEdge, ScreenContextToggleState, DEFAULT_CONTEXT_MAX_CHARS, SCREEN_CONTEXT_WAIT_MS,
};
#[cfg(test)]
use crate::feature_policy;
use crate::feature_policy::{
    app_allows_suggestions as app_allows_suggestions_for_field,
    suggestion_gates_pass as suggestion_gates_pass_for_field, FeaturePolicy, FeatureSwitches,
    SuggestionTarget as SuggestionApp,
};
use crate::inference::{InferenceHandle, PreviousInputs, ScreenContext, WorkerContext};
use crate::loop_state::{
    DownloadState, FocusContext, MonitoredInput, PolicyState, SessionUi, SettingsState,
    SuggestionState, UsageStats,
};
use crate::model_select::{load_model, resolve_prompt_mode, resolve_source, PromptMode};
use crate::screen_ocr::ScreenOcr;
#[cfg(test)]
use crate::settings_runtime::env_shadow_warnings;
use crate::settings_runtime::{
    apply_autocorrect_settings_edge, apply_launch_at_login_settings_edge,
    apply_midline_settings_edge, apply_trailing_space_settings_edge,
    startup_env_shadow_notice_lines, switch_edge,
};
use crate::status::{derive_status, AppStatus, BlockReason};
use crate::url_actions::{take_url_actions, UrlActionFlags};
use crate::wiring::{FieldTracker, LatestRequest, Observation};

const DEFAULT_DEBOUNCE_MS: u64 = 120;
const DEFAULT_MAX_WORDS: usize = 8;
const DEFAULT_MIN_CONTEXT_CHARS: usize = 3;
const DEFAULT_MAX_TOKENS: usize = 24;
const DEFAULT_HEARTBEAT_MS: u64 = 12;
/// Candidate completions generated per request (1 = single, up to 5 for cycle).
const DEFAULT_CANDIDATES: usize = 1;
const MAX_MONITORED_BUFFER_CHARS: usize = 512;
const DEFAULT_MODEL: &str = "tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf";
const MAX_DEEP_LINK_URL_CHARS: usize = 4096;
const MAX_DEEP_LINK_QUEUE: usize = 8;
const MAX_HOST_EVENT_QUEUE: usize = 1024;
const MAX_HOST_EVENTS_PER_TICK: usize = 256;
/// Re-poll secure input + Accessibility trust at most this often (wall-clock ms).
const SECURE_POLL_INTERVAL_MS: u64 = 480;
/// Periodic lifetime-stats flush cadence (c102 follow-up): bounds crash loss
/// to ≤5 minutes of events; the file is ~120 bytes so the write is free.
const STATS_FLUSH_INTERVAL_MS: u64 = 5 * 60 * 1000;

/// Set by the SIGINT/SIGTERM handler; observed by the loop to begin shutdown.
static STOP: AtomicBool = AtomicBool::new(false);
/// Set by the SIGUSR1 handler; observed by the loop to toggle enable/disable
/// (a headless equivalent of the tray's Enable item, also handy for scripting).
static TOGGLE: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_signal(_sig: libc::c_int) {
    // Async-signal-safe: only a relaxed atomic store.
    STOP.store(true, Ordering::Relaxed);
}

#[cfg(unix)]
extern "C" fn on_toggle(_sig: libc::c_int) {
    TOGGLE.store(true, Ordering::Relaxed);
}

#[cfg(unix)]
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

#[cfg(windows)]
fn install_signal_handlers() {
    // Ctrl-C / console-close parity with SIGINT/SIGTERM. The headless toggle
    // (SIGUSR1 equivalent) lands with the real Windows adapter (named event).
    if let Err(err) = platform_windows::win_host::install_console_ctrl_handler(&STOP) {
        eprintln!("compme: console ctrl handler unavailable: {err}");
    }
}

#[cfg(not(any(unix, windows)))]
fn install_signal_handlers() {}

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
    /// An always-on (global) hotkey fired: re-show the pending suggestion or
    /// toggle suggestions for the focused app / globally. Acts even when no
    /// suggestion is showing, unlike the accept variants.
    Shortcut(ShortcutAction),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingMonitoredText {
    field: FieldHandle,
    inserted: String,
    oversized: bool,
    app_key: Option<String>,
    domain: Option<String>,
    terminal_ok: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MonitoredBuffer {
    Collecting(String),
    DroppedUntilBoundary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MonitoredPolicy {
    enabled: bool,
    secure: bool,
    trusted: bool,
    now_ms: u64,
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

struct HostEventDrain {
    events: Vec<HostEvent>,
    backlog_remaining: bool,
}

fn host_event_is_backpressure_droppable(event: &HostEvent) -> bool {
    matches!(event, HostEvent::Focus(_) | HostEvent::Caret(_, _))
}

fn enqueue_host_event(queue: &mut VecDeque<HostEvent>, event: HostEvent) -> bool {
    if queue.len() >= MAX_HOST_EVENT_QUEUE {
        let Some(drop_index) = queue.iter().position(host_event_is_backpressure_droppable) else {
            return false;
        };
        let dropped_focus = match queue.get(drop_index) {
            Some(HostEvent::Focus(field)) => Some(field.clone()),
            _ => None,
        };
        queue.remove(drop_index);
        if let Some(field) = dropped_focus {
            queue.retain(
                |event| !matches!(event, HostEvent::Caret(caret_field, _) if caret_field == &field),
            );
        }
    }
    queue.push_back(event);
    true
}

fn push_host_event(queue: &Mutex<VecDeque<HostEvent>>, event: HostEvent) -> bool {
    let mut queue = queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    enqueue_host_event(&mut queue, event)
}

fn drain_host_events(queue: &Mutex<VecDeque<HostEvent>>) -> HostEventDrain {
    let mut queue = queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut events = Vec::new();
    for _ in 0..MAX_HOST_EVENTS_PER_TICK {
        let Some(event) = queue.pop_front() else {
            break;
        };
        events.push(event);
    }
    HostEventDrain {
        events,
        backlog_remaining: !queue.is_empty(),
    }
}

fn enqueue_deep_link(queue: &mut Vec<String>, url: String) -> bool {
    if url.chars().count() > MAX_DEEP_LINK_URL_CHARS {
        return false;
    }
    if queue.len() >= MAX_DEEP_LINK_QUEUE {
        queue.remove(0);
    }
    queue.push(url);
    true
}

fn host_event_invalidates_pending_request(event: &HostEvent) -> bool {
    matches!(
        event,
        HostEvent::Focus(_) | HostEvent::Caret(_, _) | HostEvent::Accept(_) | HostEvent::Dismiss
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostEventRoute {
    Normal,
    ManualGrammarDetection,
    AcceptCorrection,
}

fn host_event_route(event: &HostEvent) -> HostEventRoute {
    match event {
        HostEvent::Shortcut(ShortcutAction::GrammarCheck) => HostEventRoute::ManualGrammarDetection,
        HostEvent::Accept(AcceptAction::Correction) => HostEventRoute::AcceptCorrection,
        _ => HostEventRoute::Normal,
    }
}

/// Runtime configuration, all from the environment (full config surface is P1).
struct Config {
    /// Global on/off at launch (`COMPME_ENABLED`, default on). The tray
    /// toggle flips the runtime flag and persists back to this key.
    enabled: bool,
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
    trailing_space: bool,
    diag_coords: bool,
    candidates: usize,
    context_max_chars: usize,
    cross_app_previous_inputs: bool,
    clipboard_context: bool,
    screen_context: bool,
    diag_context: bool,
    diag_clipboard_marker: Option<String>,
    acceptance_prompt_marker: Option<String>,
    personalization: PersonalizationProfile,
    prefs: Prefs,
    memory: MemoryConfig,
    /// Emoji completion (A2 §8/§16). `Some` = enabled with the user's skin-tone/
    /// gender prefs; `None` = off (default). Drives the local `:shortcode`
    /// replacement offer in the observe path.
    emoji: Option<EmojiPrefs>,
    /// The persisted emoji preference payload, retained even while Emoji
    /// completions are disabled so settings choices survive off/on cycles and
    /// relaunches.
    emoji_prefs: EmojiPrefs,
    /// Inline typo autocorrect (A2 §8/§16, `COMPME_AUTOCORRECT`, default off):
    /// offer the correction when the trailing word is a known typo.
    autocorrect: bool,
    /// OS-backed statistical autocorrect (`COMPME_FULL_AUTOCORRECT`, default
    /// off), distinct from the curated typo table and gated out of code fields.
    full_autocorrect: bool,
    /// Standalone grammar/spell-fix trigger (`COMPME_GRAMMAR_FIX`, default off).
    grammar_fix: bool,
    /// British-English normalization (A2 §16, `COMPME_BRITISH_ENGLISH`, default
    /// off): offer the UK spelling when the trailing word is a known US-only form.
    british_english: bool,
    /// Inline thesaurus / synonym suggestions (A2 §16, `COMPME_THESAURUS`,
    /// default off): offer synonyms for the trailing word as the user types.
    thesaurus: bool,
    /// Explicit selection-triggered thesaurus mode
    /// (`COMPME_THESAURUS_SELECTION`, default off).
    thesaurus_selection: bool,
    /// Launch-at-login (A3 D13, `COMPME_LAUNCH_AT_LOGIN`): `Some(true/false)`
    /// registers/unregisters the SMAppService login item at startup; `None`
    /// (absent or unrecognized) leaves the user's Login Items setting alone.
    launch_at_login: Option<bool>,
    /// Host-pinned Ed25519 key for SIGNED deep links (`COMPME_TRUSTED_KEY`,
    /// 64 hex). `None` (default, incl. malformed) = signed links rejected
    /// fail-closed; unsigned reversible links work either way.
    trusted_key: Option<webconfig::TrustedKey>,
    /// Model names whose click-through license terms the user has accepted
    /// (`COMPME_LICENSE_ACCEPTED`, comma-joined; persisted on Accept).
    /// BTreeSet so the serialized form is deterministic (sorted, deduped).
    license_accepted: std::collections::BTreeSet<String>,
    /// Rebound accept keys as `(macOS virtual keycode, Carbon modifier mask)`,
    /// parsed from `COMPME_ACCEPT_WORD_KEY` / `COMPME_ACCEPT_FULL_KEY` (e.g.
    /// `"48"` or `"shift+48"`). `None` → defaults (Tab 48 / grave 50). A mask
    /// of 0 is a bare key. Collisions/invalid input fail soft to defaults at
    /// startup with a logged error.
    accept_word_key: Option<(i64, u32)>,
    accept_full_key: Option<(i64, u32)>,
    grammar_accept_key: Option<(i64, u32)>,
    /// Always-on (global) shortcut chords, raw config strings parsed by
    /// `crate::shell::set_shortcut_bindings_from_config` (same grammar as the
    /// accept keys, e.g. `"96"` or `"ctrl+shift+50"`). `None` → that shortcut is
    /// unbound. A colliding set is dropped whole at registration with a log.
    force_activate_key: Option<String>,
    toggle_app_key: Option<String>,
    toggle_global_key: Option<String>,
    grammar_check_key: Option<String>,
}

/// Encrypted-memory settings (A2 §6/§16). Off by default. `mode` selects what is
/// recorded; `path` is the on-disk SQLite database; `key` is the optional
/// explicit 32-byte AES key from `COMPME_MEMORY_KEY` (64 hex chars) — when
/// absent, `open_memory_store` falls back to the Keychain-backed key (generated
/// on first use). Without a path the store stays disabled even if a mode is set.
struct MemoryConfig {
    mode: memory::StorageMode,
    path: Option<PathBuf>,
    key: Option<[u8; 32]>,
}

impl Drop for MemoryConfig {
    // The explicit AES key lives on the long-lived Config for the whole run;
    // scrub it on drop so it does not linger in process memory (matching the
    // `memory` crate's StaticKey/cipher zeroization). `open_memory_store`
    // separately scrubs the transient copy it hands to the store.
    fn drop(&mut self) {
        if let Some(key) = self.key.as_mut() {
            key.zeroize();
        }
    }
}

impl Config {
    /// Build config by layering the environment over the optional config file
    /// (env wins over file wins over default), all through `from_lookup`.
    fn from_env() -> Result<Self, String> {
        let file_map = if let Some(path) = config::config_file_path() {
            config::load_file_map(&path)
                .map_err(|err| format!("failed to read config {}: {err}", path.display()))?
        } else {
            HashMap::new()
        };
        Ok(Self::from_lookup(move |key| {
            layered(env::var(key).ok(), file_map.get(key).cloned())
        }))
    }

    /// Pure config parsing from a key→value lookup, so the parsing rules
    /// (pid/run_ms parse, empty-stub filtering, default model path, prompt mode,
    /// clamped numeric knobs) are unit-testable without touching the environment.
    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let emoji_prefs = build_emoji_prefs(&lookup);
        let emoji_enabled = emoji_config_enabled(&lookup);
        Self {
            // Global on/off (the tray-toggle state, persisted on toggle).
            // Distinct from COMPME_DEFAULT_ENABLED, the per-app
            // suggestion-policy default in prefs.
            enabled: parse_enabled_default(lookup("COMPME_ENABLED")),
            launch_at_login: parse_tri_state(lookup("COMPME_LAUNCH_AT_LOGIN")),
            trusted_key: lookup("COMPME_TRUSTED_KEY")
                .and_then(|raw| webconfig::TrustedKey::from_hex(&raw)),
            license_accepted: parse_license_accepted(lookup("COMPME_LICENSE_ACCEPTED")),
            accept_word_key: lookup("COMPME_ACCEPT_WORD_KEY")
                .and_then(|raw| crate::shell::parse_accept_key(&raw)),
            accept_full_key: lookup("COMPME_ACCEPT_FULL_KEY")
                .and_then(|raw| crate::shell::parse_accept_key(&raw)),
            grammar_accept_key: lookup("COMPME_GRAMMAR_ACCEPT_KEY")
                .and_then(|raw| crate::shell::parse_accept_key(&raw)),
            force_activate_key: lookup("COMPME_FORCE_ACTIVATE_KEY")
                .or_else(|| lookup("COMPME_FORCE_ACTIVATE"))
                .filter(|s| !s.is_empty()),
            toggle_app_key: lookup("COMPME_TOGGLE_APP_KEY").filter(|s| !s.is_empty()),
            toggle_global_key: lookup("COMPME_TOGGLE_GLOBAL_KEY").filter(|s| !s.is_empty()),
            grammar_check_key: lookup("COMPME_GRAMMAR_CHECK_KEY").filter(|s| !s.is_empty()),
            acceptance_pid: lookup("COMPME_ACCEPTANCE_PID").and_then(|raw| raw.parse::<i32>().ok()),
            stub_completion: lookup("COMPME_STUB_COMPLETION").filter(|s| !s.is_empty()),
            model_path: lookup("COMPME_MODEL_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_MODEL)),
            prompt_mode: resolve_prompt_mode(lookup("COMPME_PROMPT_MODE")),
            run_ms: lookup("COMPME_RUN_MS").and_then(|raw| raw.parse::<u64>().ok()),
            debounce_ms: parse_clamped(lookup("COMPME_DEBOUNCE_MS"), DEFAULT_DEBOUNCE_MS, 0, 5000),
            max_words: parse_clamped(lookup("COMPME_MAX_WORDS"), DEFAULT_MAX_WORDS, 1, 50),
            max_tokens: parse_clamped(lookup("COMPME_MAX_TOKENS"), DEFAULT_MAX_TOKENS, 1, 200),
            heartbeat_ms: parse_clamped(
                lookup("COMPME_HEARTBEAT_MS"),
                DEFAULT_HEARTBEAT_MS,
                1,
                100,
            ),
            min_context_chars: parse_clamped(
                lookup("COMPME_MIN_CONTEXT"),
                DEFAULT_MIN_CONTEXT_CHARS,
                0,
                100,
            ),
            // Conservative default: suppress mid-word completions (engine-macos
            // design §4 trigger gating + plan-review F5, "protect first-run").
            // `COMPME_MIDLINE=1` opts into them.
            allow_mid_word: lookup("COMPME_MIDLINE").is_some_and(|v| v == "1" || v == "true"),
            // Cotypist "Include trailing space after single-word completions".
            // Off by default → accept text is byte-identical to before the flag.
            trailing_space: lookup("COMPME_TRAILING_SPACE")
                .is_some_and(|v| v == "1" || v == "true"),
            diag_coords: lookup("COMPME_DIAG_COORDS").is_some_and(|v| v == "1" || v == "true"),
            candidates: parse_clamped(lookup("COMPME_CANDIDATES"), DEFAULT_CANDIDATES, 1, 5),
            context_max_chars: parse_context_max_chars(lookup("COMPME_PREVIOUS_INPUT_CONTEXT")),
            cross_app_previous_inputs: lookup("COMPME_CROSS_APP_PREVIOUS_INPUTS")
                .is_some_and(|v| v == "1" || v == "true" || v == "on"),
            clipboard_context: lookup("COMPME_CLIPBOARD_CONTEXT")
                .is_some_and(|v| v == "1" || v == "true"),
            screen_context: lookup("COMPME_SCREEN_CONTEXT")
                .is_some_and(|v| v == "1" || v == "true"),
            diag_context: lookup("COMPME_DIAG_CONTEXT").is_some_and(|v| v == "1" || v == "true"),
            diag_clipboard_marker: lookup("COMPME_DIAG_CLIPBOARD_MARKER").filter(|v| !v.is_empty()),
            acceptance_prompt_marker: lookup("COMPME_ACCEPTANCE_PROMPT_MARKER")
                .filter(|v| !v.is_empty()),
            personalization: build_personalization(&lookup),
            prefs: build_prefs(&lookup),
            memory: build_memory_config(&lookup),
            emoji_prefs,
            emoji: emoji_enabled.then_some(emoji_prefs),
            autocorrect: lookup("COMPME_AUTOCORRECT")
                .is_some_and(|v| v == "1" || v == "true" || v == "on"),
            full_autocorrect: lookup("COMPME_FULL_AUTOCORRECT")
                .is_some_and(|v| v == "1" || v == "true" || v == "on"),
            grammar_fix: lookup("COMPME_GRAMMAR_FIX")
                .is_some_and(|v| v == "1" || v == "true" || v == "on"),
            british_english: lookup("COMPME_BRITISH_ENGLISH")
                .is_some_and(|v| v == "1" || v == "true" || v == "on"),
            thesaurus: lookup("COMPME_THESAURUS")
                .is_some_and(|v| v == "1" || v == "true" || v == "on"),
            thesaurus_selection: lookup("COMPME_THESAURUS_SELECTION")
                .is_some_and(|v| v == "1" || v == "true" || v == "on"),
        }
    }
}

/// A local emoji *replacement* for the typed left-context, when emoji completion
/// is enabled: `Some((glyph, replace_chars))` to offer, else `None`. Pure wrapper
/// over `emoji::suggest` behind the enable flag so the run-loop wiring is testable.
/// True when `COMPME_DEBUG` is enabled — gates verbose run-loop diagnostics
/// (replacement decision, etc.). Off by default and when set to an explicit
/// off-value (`0`/`false`/`off`/`no`/empty), matching the project's other
/// boolean env vars — so `COMPME_DEBUG=0` silences it instead of enabling it.
fn debug_enabled() -> bool {
    env_flag_on(std::env::var_os("COMPME_DEBUG").as_deref())
}

fn feature_switches(config: &Config) -> FeatureSwitches<'_> {
    FeatureSwitches {
        emoji: config.emoji.as_ref(),
        autocorrect: config.autocorrect,
        full_autocorrect: config.full_autocorrect,
        british_english: config.british_english,
        thesaurus: config.thesaurus,
        thesaurus_selection: config.thesaurus_selection,
    }
}

fn feature_policy<'a>(
    config: &'a Config,
    prefs: &'a Prefs,
    app: SuggestionApp<'a>,
    domain: Option<&'a str>,
    enabled: bool,
    now_ms: u64,
) -> FeaturePolicy<'a> {
    FeaturePolicy::new(
        feature_switches(config),
        prefs,
        app,
        domain,
        enabled,
        now_ms,
    )
}

#[cfg(test)]
fn emoji_offer(left: &str, cfg: &Option<EmojiPrefs>) -> Option<(String, usize)> {
    feature_policy::emoji_offer(left, cfg.as_ref())
}

#[cfg(test)]
fn trailing_word(left: &str) -> Option<&str> {
    feature_policy::trailing_word(left)
}

#[cfg(test)]
fn replacement_offer(
    left: &str,
    config: &Config,
    autocorrect_enabled: bool,
    thesaurus_enabled: bool,
) -> Option<(Vec<String>, usize)> {
    feature_policy::replacement_offer(
        left,
        feature_switches(config),
        autocorrect_enabled,
        thesaurus_enabled,
    )
}

#[cfg(test)]
fn replacement_decision(
    left: &str,
    config: &Config,
    prefs: &Prefs,
    app_key: Option<&str>,
    domain: Option<&str>,
    enabled: bool,
    now_ms: u64,
) -> Option<(Vec<String>, usize)> {
    replacement_decision_for_field(
        left,
        config,
        prefs,
        SuggestionApp {
            app_key,
            assistant_field: false,
        },
        domain,
        enabled,
        now_ms,
    )
}

fn replacement_decision_for_field(
    left: &str,
    config: &Config,
    prefs: &Prefs,
    app: SuggestionApp<'_>,
    domain: Option<&str>,
    enabled: bool,
    now_ms: u64,
) -> Option<(Vec<String>, usize)> {
    feature_policy(config, prefs, app, domain, enabled, now_ms).local_replacement(left)
}

struct FullAutocorrectGate<'a> {
    app: SuggestionApp<'a>,
    domain: Option<&'a str>,
    enabled: bool,
    now_ms: u64,
}

fn full_autocorrect_decision(
    left: &str,
    config: &Config,
    prefs: &Prefs,
    gate: FullAutocorrectGate<'_>,
    spelling_correction: impl FnOnce(&str) -> Result<Option<String>, PlatformError>,
) -> Option<(Vec<String>, usize)> {
    feature_policy(
        config,
        prefs,
        gate.app,
        gate.domain,
        gate.enabled,
        gate.now_ms,
    )
    .full_autocorrect(left, spelling_correction)
}

struct SelectionThesaurusGate<'a> {
    config: &'a Config,
    prefs: &'a Prefs,
    app: SuggestionApp<'a>,
    domain: Option<&'a str>,
    enabled: bool,
    caps: &'a Capabilities,
    now_ms: u64,
}

fn selection_thesaurus_decision(
    ctx: &TextContext,
    gate: SelectionThesaurusGate<'_>,
) -> Option<(String, Vec<String>, CorrectionRange)> {
    feature_policy(
        gate.config,
        gate.prefs,
        gate.app,
        gate.domain,
        gate.enabled,
        gate.now_ms,
    )
    .selection_thesaurus(ctx, gate.caps)
}

struct GrammarRequestGate<'a> {
    config: &'a Config,
    prefs: &'a Prefs,
    app_key: Option<&'a str>,
    domain: Option<&'a str>,
    enabled: bool,
    caps: &'a Capabilities,
    now_ms: u64,
}

/// Cap on the left-context tail sent to the grammar-fix prompt. The vetted
/// correction is a single word, so a few hundred caret-adjacent chars carry
/// all the signal; the full AX field value can be arbitrarily large.
const GRAMMAR_LEFT_CTX_CHARS: usize = 400;
/// Maximum correction-token length accepted from an accessibility field.
/// Longer adjacent runs are not useful spelling targets and must not become
/// unbounded model prompts.
const GRAMMAR_WORD_MAX_CHARS: usize = 128;

fn grammar_fix_request(
    field: &FieldHandle,
    ctx: &TextContext,
    gate: GrammarRequestGate<'_>,
) -> Option<CompletionRequest> {
    if !gate.enabled
        || !gate
            .prefs
            .grammar_fix_enabled(gate.app_key, gate.config.grammar_fix)
        || !browser_domain_fresh_enough_for_rules(gate.app_key, gate.domain, gate.prefs)
        || !suggestion_gates_pass_for_field(
            SuggestionApp {
                app_key: gate.app_key,
                assistant_field: gate.caps.assistant_field,
            },
            &ctx.left,
            gate.domain,
            gate.prefs,
            gate.now_ms,
        )
        || !gate.caps.insert_strategy.supports_atomic_range_replace()
        || ctx.selection.is_some_and(|range| range.start != range.end)
    {
        return None;
    }

    let word = context::word_at_split_caret(
        &ctx.left,
        &ctx.right,
        ctx.left_scalars,
        GRAMMAR_WORD_MAX_CHARS,
    )?;
    Some(CompletionRequest {
        generation: field.generation,
        field: field.clone(),
        domain: gate.domain.map(str::to_string),
        snapshot: field.generation,
        prompt: String::new(),
        // Grammar output is one vetted word — the completion-tuned
        // DEFAULT_MAX_TOKENS/COMPME_MAX_TOKENS budget does not apply here.
        max_tokens: crate::inference::GRAMMAR_MAX_TOKENS,
        kind: RequestKind::GrammarFix {
            word: word.word,
            // Tail-bounded: the prompt needs the word plus nearby context, and
            // the AX-read field value is unbounded attacker/user-sized input.
            // correction_range stays in full-field scalar coordinates.
            left_ctx: context::tail_chars(&ctx.left, GRAMMAR_LEFT_CTX_CHARS).to_string(),
            correction_range: CorrectionRange {
                start: word.range.start,
                end: word.range.end,
            },
        },
    })
}

fn clipboard_diagnostic_line(text: Option<&str>, marker: Option<&str>) -> String {
    match text {
        Some(text) => {
            let marker_found = marker.is_some_and(|marker| text == marker);
            format!("Some(chars={} marker={marker_found})", text.chars().count())
        }
        None => "None".to_string(),
    }
}

/// Parse `COMPME_LICENSE_ACCEPTED` (comma-joined model names) into a set.
/// Trims and drops empties so hand-edited values normalize on the next
/// persist; BTreeSet keeps the serialized form deterministic.
fn parse_license_accepted(raw: Option<String>) -> std::collections::BTreeSet<String> {
    comma_list(raw).into_iter().collect()
}

/// Record one license acceptance in the in-memory set (so the same session
/// never re-prompts) and return the comma-joined value to persist under
/// `COMPME_LICENSE_ACCEPTED`. Sorted + deduped by the set; re-accepting is
/// a no-op.
fn record_license_acceptance(
    accepted: &mut std::collections::BTreeSet<String>,
    model: &str,
) -> String {
    accepted.insert(model.to_string());
    accepted.iter().cloned().collect::<Vec<_>>().join(",")
}

/// Build the worker request for a catalog entry, threading its pinned
/// SHA-256 and advertised-size ceiling into model_fetch's guarded stream. The
/// consume edge previously hardcoded `expected_sha256: None`, which would
/// have silently ignored a pinned catalog hash.
fn catalog_download_request(
    entry: &model_catalog::ModelEntry,
    dest: PathBuf,
    status: std::sync::Arc<model_fetch::DownloadStatus>,
) -> model_fetch::DownloadRequest {
    model_fetch::DownloadRequest {
        url: entry.url.to_string(),
        dest,
        expected_sha256: entry.expected_sha256.map(String::from),
        max_bytes: Some(u64::from(entry.size_mb) * 1024 * 1024),
        status,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum DownloadStartResult {
    PreparedFailed(String),
    AlreadyPresent,
    SpawnFailed(String),
    Queued,
    Busy,
}

#[derive(Debug, PartialEq, Eq)]
struct AcceptedLicenseDecision {
    model: &'static str,
    license_name: &'static str,
    value: String,
}

#[derive(Debug, PartialEq, Eq)]
enum ModelDownloadClickDecision {
    BlockedByRam(String),
    LicenseDeclined {
        model: &'static str,
    },
    Ready {
        entry: &'static model_catalog::ModelEntry,
        accepted_license: Option<AcceptedLicenseDecision>,
    },
}

fn model_download_click_decision(
    selected_index: usize,
    available_ram_gb: u32,
    accepted_licenses: &mut std::collections::BTreeSet<String>,
    mut confirm_license: impl FnMut(&str, &str, &str) -> bool,
) -> Option<ModelDownloadClickDecision> {
    let entry = crate::model_picker::selected_catalog_entry(selected_index)?;
    if let Some(message) = model_download_ram_block_message(entry, available_ram_gb) {
        return Some(ModelDownloadClickDecision::BlockedByRam(message));
    }
    match model_catalog::download_gate(entry, |name| accepted_licenses.contains(name)) {
        model_catalog::DownloadGate::Proceed => Some(ModelDownloadClickDecision::Ready {
            entry,
            accepted_license: None,
        }),
        model_catalog::DownloadGate::NeedsLicense {
            model,
            license_name,
            terms_url,
        } => {
            if confirm_license(model, license_name, terms_url) {
                let value = record_license_acceptance(accepted_licenses, model);
                Some(ModelDownloadClickDecision::Ready {
                    entry,
                    accepted_license: Some(AcceptedLicenseDecision {
                        model,
                        license_name,
                        value,
                    }),
                })
            } else {
                Some(ModelDownloadClickDecision::LicenseDeclined { model })
            }
        }
    }
}

struct ModelDownloadEdge<'a, D, Prepare, ExistingModel, Spawn, Request> {
    entry: &'a model_catalog::ModelEntry,
    dest: &'a std::path::Path,
    downloader: &'a mut Option<D>,
    model_download_status: &'a mut Option<std::sync::Arc<model_fetch::DownloadStatus>>,
    model_download_logged: &'a mut u8,
    prepare: Prepare,
    existing_model: ExistingModel,
    spawn: Spawn,
    request: Request,
}

fn start_model_download_edge<D, Prepare, ExistingModel, Spawn, Request>(
    edge: ModelDownloadEdge<'_, D, Prepare, ExistingModel, Spawn, Request>,
) -> DownloadStartResult
where
    Prepare: for<'p> FnOnce(&'p std::path::Path) -> Result<(), String>,
    ExistingModel: for<'p> FnOnce(&'p std::path::Path, Option<&str>) -> Result<bool, String>,
    Spawn: FnOnce() -> Result<D, String>,
    Request: for<'d> FnOnce(&'d D, model_fetch::DownloadRequest) -> bool,
{
    if let Err(err) = (edge.prepare)(edge.dest) {
        return DownloadStartResult::PreparedFailed(err);
    }
    let already_present = match (edge.existing_model)(edge.dest, edge.entry.expected_sha256) {
        Ok(already_present) => already_present,
        Err(err) => return DownloadStartResult::PreparedFailed(err),
    };
    if already_present {
        return DownloadStartResult::AlreadyPresent;
    }
    if edge.downloader.is_none() {
        match (edge.spawn)() {
            Ok(spawned) => *edge.downloader = Some(spawned),
            Err(err) => return DownloadStartResult::SpawnFailed(err),
        }
    }
    let Some(downloader) = edge.downloader.as_ref() else {
        return DownloadStartResult::SpawnFailed("model downloader unavailable".into());
    };
    let status = std::sync::Arc::new(model_fetch::DownloadStatus::default());
    if (edge.request)(
        downloader,
        catalog_download_request(
            edge.entry,
            edge.dest.to_path_buf(),
            std::sync::Arc::clone(&status),
        ),
    ) {
        *edge.model_download_status = Some(status);
        *edge.model_download_logged = 0;
        DownloadStartResult::Queued
    } else {
        DownloadStartResult::Busy
    }
}

/// Whether a new download may start: none ran yet, or the last one reached
/// a terminal state (Done/Failed — retry and re-download both work).
/// Idle/Running block (a request is queued or in flight). Replaces the
/// one-shot `is_none()` guard that silently swallowed every request after
/// the first download for the process lifetime.
fn download_idle(status: Option<&model_fetch::DownloadStatus>) -> bool {
    let Some(status) = status else { return true };
    let state = status.state.lock().unwrap_or_else(|e| e.into_inner());
    matches!(
        *state,
        model_fetch::DownloadState::Done(_) | model_fetch::DownloadState::Failed(_)
    )
}

/// Live accept-key rebind (recorder 5b): the PINNED sequencing contract.
/// Keymap write FIRST (an old hotkey firing mid-swap reads the new map —
/// role-safe: the id→keycode→binding round-trip stays within one map),
/// re-arm SECOND, persist ONLY after the re-arm succeeded. On re-arm
/// failure the map REVERTS to the previously registered pair so
/// `effective_accept_keys()` and the Shortcuts pane keep telling the
/// registered truth (the c123 desync class). Injected seams so the
/// ordering is unit-testable without touching the process-global keymap.
type KeyWithMods = (i64, u32);

fn apply_live_accept_keymap(
    word: Option<KeyWithMods>,
    full: Option<KeyWithMods>,
    grammar_accept: Option<KeyWithMods>,
    set_map: impl Fn(
        Option<KeyWithMods>,
        Option<KeyWithMods>,
        Option<KeyWithMods>,
    ) -> Result<(), crate::shell::KeymapError>,
    rearm: impl Fn() -> Result<(), PlatformError>,
    persist: impl Fn(KeyWithMods, KeyWithMods, Option<KeyWithMods>),
    effective: impl Fn() -> (KeyWithMods, KeyWithMods, Option<KeyWithMods>),
) -> Result<(), String> {
    let previous = effective();
    // Slice 2: the recorder now captures `(keycode, mask)` for BOTH roles (the
    // captured key's modifier mask via `event.modifierFlags()`, and the OTHER
    // role's CURRENT (keycode, mask) carried through verbatim for c134
    // clobber-avoidance). So the masks arrive already-resolved — set them as-is.
    // The audit-r2 mask-preservation that used to be reconstructed here now
    // lives at its source in `recorder_outcome`/`rebind_request_for`.
    set_map(word, full, grammar_accept).map_err(|err| format!("rejected keymap: {err:?}"))?;
    if let Err(err) = rearm() {
        // Best-effort revert. The previous pair was validated when it
        // registered, so this set_map cannot fail in practice. Reverting the
        // map alone is not enough: the failed rearm may have already dropped
        // the old consumer tap, so try one more rearm after restoring the old
        // map to put the consumer-tap registration back too.
        match set_map(Some(previous.0), Some(previous.1), previous.2) {
            Ok(()) => {
                if let Err(restore_err) = rearm() {
                    eprintln!(
                        "compme: accept-keymap re-arm failed and old keymap {previous:?} \
                         could not be re-armed: {restore_err:?}"
                    );
                }
            }
            Err(revert_err) => {
                eprintln!(
                    "compme: accept-keymap re-arm failed and revert to {previous:?} \
                     also failed: {revert_err:?}"
                );
            }
        }
        return Err(format!("re-arm failed: {err:?}"));
    }
    let registered = effective();
    persist(registered.0, registered.1, registered.2);
    Ok(())
}

/// One step of the model-download log state machine (`logged`: 0=idle,
/// 1=running-logged, 2=terminal-logged): the next state plus the line to
/// emit, if any. Done/Failed log exactly once — they are the only
/// user-visible signal of where the model landed — and an instant Done that
/// skipped the Running transition still logs.
fn download_log_transition(state: &model_fetch::DownloadState, logged: u8) -> (u8, Option<String>) {
    match state {
        model_fetch::DownloadState::Running if logged == 0 => {
            (1, Some("compme: model download running".into()))
        }
        model_fetch::DownloadState::Done(path) if logged < 2 => (
            2,
            Some(format!(
                "compme: model downloaded to {} \u{2014} COMPME_MODEL_PATH set, relaunch to use",
                path.display()
            )),
        ),
        model_fetch::DownloadState::Failed(err) if logged < 2 => {
            (2, Some(format!("compme: model download failed: {err}")))
        }
        _ => (logged, None),
    }
}

#[cfg(test)]
fn request_log_line(
    request: &CompletionRequest,
    app_key: Option<&str>,
    domain: Option<&str>,
    prefs: &Prefs,
    now_ms: u64,
    acceptance_prompt_marker: Option<&str>,
    blocked: bool,
) -> String {
    request_log_line_for_field(
        request,
        SuggestionApp {
            app_key,
            assistant_field: false,
        },
        domain,
        prefs,
        now_ms,
        acceptance_prompt_marker,
        blocked,
    )
}

fn request_log_line_for_field(
    request: &CompletionRequest,
    app: SuggestionApp<'_>,
    domain: Option<&str>,
    prefs: &Prefs,
    now_ms: u64,
    acceptance_prompt_marker: Option<&str>,
    blocked: bool,
) -> String {
    let app_key = app.app_key;
    let app_allows = app_allows_suggestions_for_field(app);
    let gate_text = request_gate_text(request);
    let terminal_ok = app_key.is_none_or(|app| compat::terminal_prompt_activates(app, gate_text));
    let domain_ready = browser_domain_fresh_enough_for_rules(app_key, domain, prefs);
    let prefs_ok = prefs.should_suggest(app_key, domain, now_ms);
    let prompt_marker = match acceptance_prompt_marker {
        Some(marker) => request.prompt.contains(marker),
        None => false,
    };
    format!(
        "compme: request{} gen={} prompt_chars={} app={} app_allows={} \
         terminal_ok={} domain_ready={} prefs_ok={} prompt_marker={}",
        if blocked { " blocked" } else { "" },
        request.generation,
        request.prompt.chars().count(),
        app_key.unwrap_or("unknown"),
        app_allows,
        terminal_ok,
        domain_ready,
        prefs_ok,
        prompt_marker,
    )
}

fn request_gate_text(request: &CompletionRequest) -> &str {
    match &request.kind {
        RequestKind::Completion => &request.prompt,
        RequestKind::GrammarFix { left_ctx, .. } => left_ctx,
    }
}

#[derive(Clone, Debug)]
struct RequestLogContext {
    app_key: Option<String>,
    assistant_field: bool,
    domain: Option<String>,
    prefs: Prefs,
    acceptance_prompt_marker: Option<String>,
}

impl RequestLogContext {
    fn line_for(&self, request: &CompletionRequest, now_ms: u64) -> String {
        request_log_line_for_field(
            request,
            SuggestionApp {
                app_key: self.app_key.as_deref(),
                assistant_field: self.assistant_field,
            },
            self.domain.as_deref(),
            &self.prefs,
            now_ms,
            self.acceptance_prompt_marker.as_deref(),
            false,
        )
    }
}

/// Parse the encrypted-memory config (A2 §6/§16). `COMPME_MEMORY` selects the
/// storage mode (off/accepted/all, default off); `COMPME_MEMORY_PATH` the db
/// file; `COMPME_MEMORY_KEY` a 64-hex-char (32-byte) AES key.
fn build_memory_config(lookup: &impl Fn(&str) -> Option<String>) -> MemoryConfig {
    MemoryConfig {
        mode: parse_storage_mode(lookup("COMPME_MEMORY")),
        path: lookup("COMPME_MEMORY_PATH").map(PathBuf::from),
        key: lookup("COMPME_MEMORY_KEY").and_then(|raw| parse_hex_key(&raw)),
    }
}

/// Map `COMPME_MEMORY` to a storage mode. Unset/unrecognized/falsy → `Off`
/// (opt-in, §16: default off). `accepted`/`1`/`true`/`on` → `AcceptedOnly`;
/// `all`/`monitored` → `AllMonitored`.
fn parse_storage_mode(raw: Option<String>) -> memory::StorageMode {
    use memory::StorageMode;
    match raw.as_deref().map(str::trim).map(str::to_ascii_lowercase) {
        Some(v) if v == "accepted" || v == "1" || v == "true" || v == "on" => {
            StorageMode::AcceptedOnly
        }
        Some(v) if v == "all" || v == "monitored" || v == "all_monitored" => {
            StorageMode::AllMonitored
        }
        _ => StorageMode::Off,
    }
}

/// Decode a 64-char hex string into a 32-byte key. Returns `None` on wrong length
/// or a non-hex digit (the store then stays disabled — fail-closed).
fn parse_hex_key(raw: &str) -> Option<[u8; 32]> {
    let raw = raw.trim();
    if raw.len() != 64 {
        return None;
    }
    let mut key = [0u8; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(raw.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(key)
}

/// Keep an opened memory store only when every existing owned SQLite file can
/// be made owner-only. On the first hardening error `store` is dropped by the
/// `Err` return, so callers cannot accidentally continue with a live store.
#[cfg(any(windows, test))]
fn retain_memory_store_if_hardened<T>(
    store: T,
    path: &std::path::Path,
    exists: impl Fn(&std::path::Path) -> std::io::Result<bool>,
    mut harden: impl FnMut(&std::path::Path) -> std::io::Result<()>,
) -> std::io::Result<T> {
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let mut candidate = path.as_os_str().to_owned();
        candidate.push(suffix);
        let candidate = PathBuf::from(candidate);
        if exists(&candidate)? {
            harden(&candidate).map_err(|err| {
                std::io::Error::new(
                    err.kind(),
                    format!("failed to harden {}: {err}", candidate.display()),
                )
            })?;
        }
    }
    Ok(store)
}

/// Ensure a Windows memory parent exists, then require proof that it already
/// has the exact protected owner-only inheritable DACL. The injected form pins
/// fail-closed behavior on non-Windows hosts without mutating a real directory.
#[cfg(any(windows, test))]
fn ensure_memory_parent_posture_with(
    parent: &std::path::Path,
    key: &mut [u8; 32],
    ensure: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
    verify: impl FnOnce(&std::path::Path) -> std::io::Result<bool>,
) -> std::io::Result<()> {
    let result = ensure(parent).and_then(|()| {
        verify(parent)?.then_some(()).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "{} is not an owner-only inherited directory",
                    parent.display()
                ),
            )
        })
    });
    if result.is_err() {
        key.zeroize();
    }
    result
}

/// Open the encrypted memory store when enabled and fully configured. Returns
/// `None` (disabled, logged) when the mode is `Off`, the path is missing, no key
/// is available, or the open fails — never fatal, mirroring the tray-unavailable
/// fallback.
///
/// Key precedence: an explicit `COMPME_MEMORY_KEY` wins (the operator
/// override, and the fail-closed path when the keychain is unavailable);
/// otherwise `keychain_key` supplies the OS-keystore key (§16 "key in OS
/// keystore"). The keychain is consulted only when the store would actually
/// open (mode on, path present) — never as a side effect.
fn open_memory_store(
    config: &MemoryConfig,
    keychain_key: impl Fn() -> Option<[u8; 32]>,
) -> Option<memory::MemoryStore> {
    use memory::{MemoryStore, StaticKey, StorageMode};
    if config.mode == StorageMode::Off {
        return None;
    }
    let Some(path) = config.path.as_ref() else {
        eprintln!(
            "compme: COMPME_MEMORY set but COMPME_MEMORY_PATH missing — \
             memory disabled"
        );
        return None;
    };
    let Some(mut key) = config.key.or_else(&keychain_key) else {
        eprintln!(
            "compme: COMPME_MEMORY set but no key available (no \
             COMPME_MEMORY_KEY and the keychain provided none) — memory disabled"
        );
        return None;
    };
    // Windows analog of the store's unix 0700 dir tightening: harden the db
    // directory ONLY when this launch creates it — a pre-existing (possibly
    // shared or user-chosen) parent must not have owner-only ACLs propagated
    // over its existing subtree. Fail closed — the store holds user text.
    #[cfg(windows)]
    {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        if let Err(err) = ensure_memory_parent_posture_with(
            parent,
            &mut key,
            config::create_owner_only_dir_if_missing,
            platform_windows::win_host::is_owner_only_inherited_dir,
        ) {
            eprintln!(
                "compme: insecure memory dir {}: {err} — memory disabled",
                parent.display()
            );
            return None;
        }
    }
    // StaticKey scrubs its own copy on drop; scrub this transient copy too so
    // no un-zeroized key byte is left on the stack after the store is opened.
    let opened = MemoryStore::open(path, &StaticKey(key), config.mode);
    // Windows analog of the store's unix per-file 0600 belt-and-suspenders:
    // owner-only DACL on the db and any sidecar, regardless of dir state —
    // covers a pre-existing unhardened dir and a bare-filename path. Any
    // per-file hardening error drops the opened store and disables memory.
    #[cfg(windows)]
    let opened = opened.and_then(|store| {
        retain_memory_store_if_hardened(
            store,
            path,
            |candidate| candidate.try_exists(),
            platform_windows::win_host::harden_owner_only,
        )
        .map_err(|err| memory::MemoryError::Io(err.to_string()))
    });
    key.zeroize();
    match opened {
        Ok(store) => {
            eprintln!("compme: encrypted memory enabled (mode={:?})", config.mode);
            Some(store)
        }
        Err(err) => {
            eprintln!("compme: memory store unavailable: {err} — memory disabled");
            None
        }
    }
}

/// Human label for each `Strength::STOPS` row, in stop order (0 = Off .. 5 =
/// Max). The Personalization popup renders these; the run loop maps the picked
/// index back via `Strength::from_stop`. Composed app-side because the
/// `Strength` directive text is private to the `personalization` crate and the
/// pane crate can't see the enum at all (the stat-range titles pattern).
fn personalization_strength_titles() -> Vec<String> {
    Strength::STOPS
        .iter()
        .map(|s| {
            match s {
                Strength::Off => "Off",
                Strength::Stop1 => "Very gentle",
                Strength::Stop2 => "Gentle",
                Strength::Stop3 => "Balanced",
                Strength::Stop4 => "Strong",
                Strength::Max => "Strict",
            }
            .to_string()
        })
        .collect()
}

/// The stop index of `strength` within `Strength::STOPS` (0 = Off). Used to
/// pre-select the popup row from the current profile. Total: every `Strength`
/// is in `STOPS`, so the search never fails; 0 is a safe fallback regardless.
fn personalization_strength_index(strength: Strength) -> usize {
    Strength::STOPS
        .iter()
        .position(|s| *s == strength)
        .unwrap_or(0)
}

/// Apply one Personalization-pane edit to the source `profile` in place and
/// return the `(env_key, value)` to persist so the edit survives restart. Pure:
/// no IO, no inference — the run loop drives `set_profile` and persistence
/// around it. The seam carries primitives; this is where they rejoin the typed
/// `PersonalizationProfile` (the `apps_edit` → `AppPolicyField` pattern).
fn apply_personalization_edit(
    profile: &mut PersonalizationProfile,
    edit: crate::shell::PersonalizationEdit,
) -> (&'static str, String) {
    use crate::shell::PersonalizationEdit as E;
    match edit {
        E::GlobalInstructions(text) => {
            profile.global_instructions = text.clone();
            ("COMPME_INSTRUCTIONS", text)
        }
        E::SenderName(name) => {
            profile.sender.name = name.clone();
            ("COMPME_SENDER_NAME", name)
        }
        E::SenderEmail(email) => {
            profile.sender.email = email.clone();
            ("COMPME_SENDER_EMAIL", email)
        }
        E::StrengthStop(stop) => {
            // The popup index addresses STOPS directly; clamp via from_stop so
            // an out-of-range value is total (mirrors build_personalization).
            let stop = stop.min(u8::MAX as usize) as u8;
            profile.strength = Strength::from_stop(stop);
            ("COMPME_STRENGTH", stop.to_string())
        }
    }
}

fn apply_live_personalization_edit(
    profile: &mut PersonalizationProfile,
    edit: crate::shell::PersonalizationEdit,
    set_profile: impl FnOnce(PersonalizationProfile),
    persist: impl FnOnce(&'static str, &str) -> std::io::Result<()>,
) -> (&'static str, String, std::io::Result<()>) {
    let (key, value) = apply_personalization_edit(profile, edit);
    set_profile(profile.clone());
    let persist_result = persist(key, &value);
    (key, value, persist_result)
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
            "compme: {app} needs setup for inline suggestions \
             (e.g. Google Docs Accessibility / Text Metrics)"
        ),
        CompatTier::MirrorOnly => {
            eprintln!("compme: {app} renders via a mirror window (inline overlay unsupported)")
        }
        CompatTier::Partial => eprintln!("compme: {app} has partial support"),
        CompatTier::SidebarOnly => {
            eprintln!("compme: {app} suggests in AI-chat/sidebar fields only, not the editor pane")
        }
        CompatTier::Unsupported => {
            eprintln!("compme: {app} is not supported — suggestions disabled")
        }
        CompatTier::Works | CompatTier::Unknown => {}
    }
}

/// Whether the focused app's compatibility tier permits suggestions (A2 §16).
/// `Unsupported` hard-blocks. `SidebarOnly` permits only fields the platform
/// positively identified as an AI-assistant/chat input; unknown/editor fields
/// remain fail-closed. Unresolved app → allow (fail-open), since the field's own
/// capabilities still gate.
fn app_allows_suggestions(app_key: Option<&str>) -> bool {
    app_allows_suggestions_for_field(SuggestionApp {
        app_key,
        assistant_field: false,
    })
}

/// Whether suggestions are allowed for `app_key` given `text` as the candidate
/// prompt/context: the app's compatibility tier allows inline (and isn't
/// sidebar-only), a terminal only when `text` reads as a natural-language prompt,
/// and per-app exclude / snooze (`should_suggest`) pass. Shared by the model
/// submit gate and the local replacement-offer gate for per-app/snooze/terminal
/// policy; submit adds a domain-freshness fail-closed guard before calling it.
/// `domain` is the focused browser page's HOST when known (the Focus arm's
/// AX read via `domain_cache_entry`). This helper treats `None` as fail-open;
/// callers that need browser-rule freshness must wrap it with
/// `browser_domain_fresh_enough_for_rules`.
#[cfg(test)]
fn suggestion_gates_pass(
    app_key: Option<&str>,
    text: &str,
    domain: Option<&str>,
    prefs: &Prefs,
    now_ms: u64,
) -> bool {
    suggestion_gates_pass_for_field(
        SuggestionApp {
            app_key,
            assistant_field: false,
        },
        text,
        domain,
        prefs,
        now_ms,
    )
}

/// Lowercased host of an http(s) URL, port stripped — the pure half of the
/// per-domain extractor (the AX/browser URL source is the pending half).
fn domain_from_url(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .split('@')
        .next_back()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    let host = webconfig::normalize_domain(host);
    (!host.is_empty()).then_some(host)
}

/// Consecutive browser-focus detection misses before the one-shot inert
/// notice fires. 5 absorbs the EXPECTED warm-up misses (Chromium builds its
/// a11y tree lazily — the first focus into each Chromium-family browser
/// predictably misses; threshold margin absorbs warm-up rather than extra
/// per-app state) while still firing within minutes of a genuinely broken
/// session (every focus misses, nothing resets).
const DOMAIN_MISS_NOTICE_THRESHOLD: u32 = 5;

/// One-shot transparency notice (c121 "transparency over silence", made
/// runtime-contingent after c131 shipped the AX domain source): per-domain
/// rules are configured but browser-focus detection has missed N times in a
/// row — the rules are likely inert and only debug logging would otherwise
/// show it. Counts ONLY browser focuses (call placement: the Focus arm's
/// is_browser branch); any successful detection resets the streak; fires at
/// most once per process. The streak counts even while no rules exist —
/// only the FIRE is gated on rules — so rules added mid-session inherit the
/// accumulated evidence and fire on the next miss.
#[derive(Default)]
pub(crate) struct DomainMissNotice {
    misses: u32,
    fired: bool,
}

impl DomainMissNotice {
    /// Record one browser-focus detection outcome; returns the notice line
    /// when it should fire. `rules_configured` is read live at each call
    /// (prefs mutate via deep links/settings — never snapshot it).
    fn observe(&mut self, rules_configured: bool, detected: bool) -> Option<String> {
        if detected {
            self.misses = 0;
            return None;
        }
        self.misses = self.misses.saturating_add(1);
        if self.fired || !rules_configured || self.misses < DOMAIN_MISS_NOTICE_THRESHOLD {
            return None;
        }
        self.fired = true;
        Some(format!(
            "domain rules are configured but no page URL was detected in the \
             last {DOMAIN_MISS_NOTICE_THRESHOLD} browser focuses \u{2014} domain \
             rules may not be applying; set COMPME_DEBUG=1 to log each focus's \
             domain read"
        ))
    }
}

/// The Focus-arm domain-cache decision: a browser app + a resolvable page
/// URL caches `(app key, HOST)`. The full URL is dropped here — only the
/// host crosses the privacy boundary (path/query/fragment never leave this
/// expression, never logged, never persisted). Non-browsers and
/// non-URL-shaped values (omnibox search text) yield `None` = fail-open.
fn domain_cache_entry(app_key: Option<&str>, url: Option<&str>) -> Option<(String, String)> {
    let app = app_key.filter(|a| compat::is_browser(a))?;
    let host = url.and_then(domain_from_url)?;
    Some((app.to_string(), host))
}

#[cfg(test)]
fn request_passes_submit_gates(
    request: &CompletionRequest,
    app_key: Option<&str>,
    domain: Option<&str>,
    prefs: &Prefs,
    now_ms: u64,
) -> bool {
    request_passes_submit_gates_for_field(
        request,
        SuggestionApp {
            app_key,
            assistant_field: false,
        },
        domain,
        prefs,
        now_ms,
    )
}

fn request_passes_submit_gates_for_field(
    request: &CompletionRequest,
    app: SuggestionApp<'_>,
    domain: Option<&str>,
    prefs: &Prefs,
    now_ms: u64,
) -> bool {
    browser_domain_fresh_enough_for_rules(app.app_key, domain, prefs)
        && suggestion_gates_pass_for_field(app, request_gate_text(request), domain, prefs, now_ms)
}

fn browser_domain_fresh_enough_for_rules(
    app_key: Option<&str>,
    domain: Option<&str>,
    prefs: &Prefs,
) -> bool {
    !(app_key.is_some_and(compat::is_browser)
        && !prefs.excluded_domains.is_empty()
        && domain.is_none())
}

fn monitored_collection_gates_pass(
    app_key: Option<&str>,
    domain: Option<&str>,
    prefs: &Prefs,
    policy: MonitoredPolicy,
    terminal_ok: bool,
) -> bool {
    !policy.secure
        && policy.trusted
        && policy.enabled
        && terminal_ok
        && browser_domain_fresh_enough_for_rules(app_key, domain, prefs)
        && app_allows_suggestions(app_key)
        && prefs.monitored_collection_allowed(app_key, domain, policy.now_ms)
}

/// The cached browser host for `app_key`, but ONLY when it is the app the
/// read was taken under — the request's app may differ from the focus that
/// populated the cache, and a domain must never cross-attribute. `None` =
/// fail-open (identical to no detection at all).
fn cached_domain<'a>(
    cache: &'a Option<(String, String)>,
    app_key: Option<&str>,
) -> Option<&'a str> {
    let (read_app, host) = cache.as_ref()?;
    (app_key == Some(read_app.as_str())).then_some(host.as_str())
}

fn domain_observation_enabled(prefs: &Prefs, profile: &PersonalizationProfile) -> bool {
    !prefs.excluded_domains.is_empty() || !profile.per_domain.is_empty()
}

fn typing_domain(
    cache: &mut Option<(String, String)>,
    app_key: Option<&str>,
    refresh_browser_domain: bool,
    fresh_url: Option<&str>,
) -> Option<String> {
    if app_key.is_some_and(compat::is_browser) && refresh_browser_domain {
        *cache = domain_cache_entry(app_key, fresh_url);
    }
    cached_domain(cache, app_key).map(str::to_owned)
}

fn typing_domain_for_current_field(
    cache: &mut Option<(String, String)>,
    app_key: Option<&str>,
    observe_domain: bool,
    mut focused_page_url: impl FnMut() -> Option<String>,
) -> Option<String> {
    let fresh_url = if app_key.is_some_and(compat::is_browser) && observe_domain {
        focused_page_url()
    } else {
        None
    };
    typing_domain(cache, app_key, observe_domain, fresh_url.as_deref())
}

struct ManualGrammarRequestInputs<'a> {
    field: &'a FieldHandle,
    ctx: &'a TextContext,
    caps: &'a Capabilities,
    config: &'a Config,
    prefs: &'a Prefs,
    app_key: Option<&'a str>,
    enabled: bool,
    now_ms: u64,
}

fn grammar_pre_read_policy_passes(
    config: &Config,
    prefs: &Prefs,
    app_key: Option<&str>,
    enabled: bool,
    now_ms: u64,
    last_domain: &mut Option<(String, String)>,
    focused_page_url: impl FnMut() -> Option<String>,
) -> bool {
    let observe_domain = domain_observation_enabled(prefs, &config.personalization);
    let domain =
        typing_domain_for_current_field(last_domain, app_key, observe_domain, focused_page_url);
    enabled
        && prefs.grammar_fix_enabled(app_key, config.grammar_fix)
        && browser_domain_fresh_enough_for_rules(app_key, domain.as_deref(), prefs)
        && app_key.is_none_or(|app| compat::compatibility_tier(app).allows_suggestions())
        && prefs.should_suggest(app_key, domain.as_deref(), now_ms)
}

fn manual_grammar_request_for_current_field(
    inputs: ManualGrammarRequestInputs<'_>,
    last_domain: &mut Option<(String, String)>,
    focused_page_url: impl FnMut() -> Option<String>,
) -> Option<CompletionRequest> {
    let observe_domain = domain_observation_enabled(inputs.prefs, &inputs.config.personalization);
    let domain = typing_domain_for_current_field(
        last_domain,
        inputs.app_key,
        observe_domain,
        focused_page_url,
    );
    grammar_fix_request(
        inputs.field,
        inputs.ctx,
        GrammarRequestGate {
            config: inputs.config,
            prefs: inputs.prefs,
            app_key: inputs.app_key,
            domain: domain.as_deref(),
            enabled: inputs.enabled,
            caps: inputs.caps,
            now_ms: inputs.now_ms,
        },
    )
}

#[derive(Debug)]
enum GrammarCheckShortcutOutcome {
    NoField,
    BlockedBeforeRead,
    ReadContextError(PlatformError),
    CapabilitiesError(PlatformError),
    BlockedAfterRead,
    NotArmed,
    Armed(CompletionRequest),
}

struct GrammarCheckShortcutArgs<
    'a,
    ResolveAppKey,
    FocusedPageUrl,
    ReadContext,
    ReadCapabilities,
    ArmGrammarRequest,
> {
    current_field: Option<FieldHandle>,
    config: &'a Config,
    prefs: &'a Prefs,
    enabled: bool,
    now_ms: u64,
    last_domain: &'a mut Option<(String, String)>,
    resolve_app_key: ResolveAppKey,
    focused_page_url: FocusedPageUrl,
    read_context: ReadContext,
    capabilities: ReadCapabilities,
    arm_manual_grammar_request: ArmGrammarRequest,
}

fn handle_grammar_check_shortcut<
    ResolveAppKey,
    FocusedPageUrl,
    ReadContext,
    ReadCapabilities,
    ArmGrammarRequest,
>(
    args: GrammarCheckShortcutArgs<
        '_,
        ResolveAppKey,
        FocusedPageUrl,
        ReadContext,
        ReadCapabilities,
        ArmGrammarRequest,
    >,
) -> GrammarCheckShortcutOutcome
where
    ResolveAppKey: FnMut(FieldHandle) -> Option<String>,
    FocusedPageUrl: FnMut(FieldHandle) -> Option<String>,
    ReadContext: FnOnce(FieldHandle) -> Result<TextContext, PlatformError>,
    ReadCapabilities: FnOnce(FieldHandle) -> Result<Capabilities, PlatformError>,
    ArmGrammarRequest: FnOnce(FieldHandle) -> Option<(u64, u64)>,
{
    let GrammarCheckShortcutArgs {
        current_field,
        config,
        prefs,
        enabled,
        now_ms,
        last_domain,
        mut resolve_app_key,
        mut focused_page_url,
        read_context,
        capabilities,
        arm_manual_grammar_request,
    } = args;
    let Some(field) = current_field else {
        return GrammarCheckShortcutOutcome::NoField;
    };
    let app_key = resolve_app_key(field.clone());
    if !grammar_pre_read_policy_passes(
        config,
        prefs,
        app_key.as_deref(),
        enabled,
        now_ms,
        last_domain,
        || focused_page_url(field.clone()),
    ) {
        return GrammarCheckShortcutOutcome::BlockedBeforeRead;
    }

    let ctx = match read_context(field.clone()) {
        Ok(ctx) => ctx,
        Err(err) => return GrammarCheckShortcutOutcome::ReadContextError(err),
    };
    let caps = match capabilities(field.clone()) {
        Ok(caps) => caps,
        Err(err) => return GrammarCheckShortcutOutcome::CapabilitiesError(err),
    };
    let Some(mut request) = manual_grammar_request_for_current_field(
        ManualGrammarRequestInputs {
            field: &field,
            ctx: &ctx,
            caps: &caps,
            config,
            prefs,
            app_key: app_key.as_deref(),
            enabled,
            now_ms,
        },
        last_domain,
        || focused_page_url(field.clone()),
    ) else {
        return GrammarCheckShortcutOutcome::BlockedAfterRead;
    };

    let Some((generation, snapshot)) = arm_manual_grammar_request(field) else {
        return GrammarCheckShortcutOutcome::NotArmed;
    };
    request.generation = generation;
    request.snapshot = snapshot;
    GrammarCheckShortcutOutcome::Armed(request)
}

fn enqueue_monitored_change_for_current_domain(
    pending: &mut Vec<PendingMonitoredText>,
    last_domain: &mut Option<(String, String)>,
    change: &engine::TextChange,
    app_key: Option<String>,
    observe_domain: bool,
    focused_page_url: impl FnMut() -> Option<String>,
) -> Option<String> {
    let domain = typing_domain_for_current_field(
        last_domain,
        app_key.as_deref(),
        observe_domain,
        focused_page_url,
    );
    enqueue_monitored_change(pending, change, app_key, domain.clone());
    domain
}

/// First-suggestion latency (ms) for a completed request's `generation`: the
/// elapsed time since it was submitted. Removes the matched submit timestamp and
/// prunes older ones (requests coalesced away in the inference channel never
/// produce an outcome), so the map stays bounded. Returns `None` when the
/// generation has no recorded submit (already pruned / never tracked).
///
/// Relies on the engine's `generation` being **globally monotonic** — it only
/// ever increases (`SuggestionMachine::advance_snapshot` does `generation += 1`
/// and never resets, including across field/focus changes) — so pruning every
/// entry `<= generation` can never drop a still-pending newer request. Latency is
/// measured at run-loop (heartbeat) resolution, so a completion returned within
/// the same tick reads as 0 ms; that is the true measured value at this
/// resolution, not an error.
fn latency_sample(
    submit_times: &mut HashMap<u64, u64>,
    generation: u64,
    now_ms: u64,
) -> Option<u32> {
    let submit_ms = submit_times.remove(&generation)?;
    // Generations are monotonic; anything at or below this one is done or stale.
    submit_times.retain(|&gen, _| gen > generation);
    Some(u32::try_from(now_ms.saturating_sub(submit_ms)).unwrap_or(u32::MAX))
}

fn submit_request_and_track(
    submit_times: &mut HashMap<u64, u64>,
    mut request: CompletionRequest,
    now_ms: u64,
    log_context: RequestLogContext,
    submit: impl FnOnce(CompletionRequest) -> bool,
) -> String {
    if request.domain.is_none() {
        request.domain = log_context.domain.clone();
    }
    let generation = request.generation;
    let submitted_line = log_context.line_for(&request, now_ms);
    if !submit(request) {
        return format!("compme: inference submit failed gen={generation}");
    }
    submit_times.insert(generation, now_ms);
    submitted_line
}

struct SubmitRequestContext<'a> {
    submit_times: &'a mut HashMap<u64, u64>,
    now_ms: u64,
    log_context: RequestLogContext,
}

struct AuxiliarySubmitContext<'a> {
    clipboard_enabled: bool,
    diag_context: bool,
    diag_clipboard_marker: Option<&'a str>,
    clipboard_cell: &'a Arc<Mutex<Option<String>>>,
    screen_enabled: bool,
}

fn submit_request_with_auxiliary_context(
    request: CompletionRequest,
    submit_context: SubmitRequestContext<'_>,
    aux_context: AuxiliarySubmitContext<'_>,
    read_clipboard: impl FnOnce() -> Option<String>,
    screen_caret_rect: impl FnOnce(&CompletionRequest) -> Option<ScreenRect>,
    submit_screen: impl FnOnce(ScreenOcrSubmission),
    submit: impl FnOnce(CompletionRequest) -> bool,
) -> (Option<String>, String) {
    let clipboard_diag = if aux_context.clipboard_enabled {
        let raw_clip = read_clipboard();
        let diag = aux_context.diag_context.then(|| {
            clipboard_diagnostic_line(raw_clip.as_deref(), aux_context.diag_clipboard_marker)
        });
        let clip = raw_clip.map(|text| redaction::redact(&text));
        *aux_context
            .clipboard_cell
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = clip;
        diag
    } else {
        *aux_context
            .clipboard_cell
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        None
    };

    if aux_context.screen_enabled {
        submit_screen(ScreenOcrSubmission::from_request(
            &request,
            screen_caret_rect(&request),
        ));
    }

    let submitted_line = submit_request_and_track(
        submit_context.submit_times,
        request,
        submit_context.now_ms,
        submit_context.log_context,
        submit,
    );
    (clipboard_diag, submitted_line)
}

#[derive(Clone, Debug, PartialEq)]
struct ScreenOcrSubmission {
    field: FieldHandle,
    generation: u64,
    snapshot: u64,
    caret_rect: Option<ScreenRect>,
}

impl ScreenOcrSubmission {
    fn from_request(request: &CompletionRequest, caret_rect: Option<ScreenRect>) -> Self {
        Self {
            field: request.field.clone(),
            generation: request.generation,
            snapshot: request.snapshot,
            caret_rect,
        }
    }

    fn send_to(self, ocr: &ScreenOcr) {
        ocr.request(self.field, self.generation, self.snapshot, self.caret_rect);
    }
}

fn completion_outcome_log_line(generation: u64, candidates: &[String]) -> String {
    let lengths = candidates
        .iter()
        .map(|candidate| candidate.len())
        .collect::<Vec<_>>();
    format!(
        "compme: completion gen={generation} candidate_count={} candidate_lengths={lengths:?}",
        candidates.len()
    )
}

fn replacement_debug_log_line(
    left: &str,
    emoji: bool,
    autocorrect: bool,
    british: bool,
    thesaurus: bool,
    decision: &str,
) -> String {
    let redacted_left = redaction::redact(left);
    format!(
        "compme: replace left={redacted_left:?} emoji={emoji} autocorrect={autocorrect} \
         british={british} thesaurus={thesaurus} decision={decision}"
    )
}

/// Route a *Full*-accept's text to the opt-in recording sinks (design spec
/// §6/§16): the volatile previous-input ring (when context is enabled) and the
/// encrypted memory store (when configured). Word-accepts (low-signal) and
/// volatile `pid:N` field keys (unresolved bundle id, would never match the
/// canonicalized lookup/personalization key) are skipped. Pure over its inputs so
/// the accept-routing logic is testable without the run loop.
struct AcceptRecording<'a> {
    context_max_chars: usize,
    cross_app_previous_inputs: bool,
    previous_inputs: &'a PreviousInputs,
    memory: Option<&'a memory::MemoryStore>,
    collection_allowed: bool,
}

fn record_full_accept(
    action: AcceptAction,
    field: &FieldHandle,
    text: &str,
    recording: AcceptRecording<'_>,
) {
    // Per-app "Input Collection off" (tray submenu / Cotypist parity) gates
    // BOTH sinks below — previous-inputs context AND encrypted memory.
    if !recording.collection_allowed
        || action != AcceptAction::Full
        || field.app.starts_with("pid:")
    {
        return;
    }
    if recording.context_max_chars > 0 {
        recording.previous_inputs.record_with_cross_app(
            &field.app,
            redaction::redact(text),
            recording.cross_app_previous_inputs,
        );
    }
    if let Some(store) = recording.memory {
        // The store redacts + encrypts before persisting; a no-op when its mode
        // is Off.
        if let Err(err) = store.remember(&field.app, text) {
            eprintln!("compme: memory remember failed: {err}");
        }
    }
}

type AcceptPreview = (FieldHandle, String, usize);
type CorrectionPreview = (FieldHandle, String, CorrectionRange);

struct AcceptSideEffects<'a> {
    action: AcceptAction,
    preview: Option<&'a AcceptPreview>,
    correction_preview: Option<&'a CorrectionPreview>,
    range_preview: Option<&'a CorrectionPreview>,
    wall_ms: u64,
    context_max_chars: usize,
    cross_app_previous_inputs: bool,
    previous_inputs: &'a PreviousInputs,
    memory: Option<&'a memory::MemoryStore>,
    prefs: &'a Prefs,
    tracker: &'a mut FieldTracker,
    usage: &'a mut stats::Stats,
}

fn accept_mutation_committed<T>(result: &Result<T, engine::AcceptError>) -> bool {
    match result {
        Ok(_) => true,
        Err(err) => err.committed,
    }
}

fn apply_accept_side_effects(accepted: bool, side_effects: AcceptSideEffects<'_>) {
    if !accepted {
        return;
    }
    let Some((field, text, replace_left)) = side_effects.preview else {
        let range_preview = side_effects.range_preview.or_else(|| {
            (side_effects.action == AcceptAction::Correction)
                .then_some(side_effects.correction_preview)
                .flatten()
        });
        if let Some((field, text, range)) = range_preview {
            side_effects
                .tracker
                .apply_self_replace_range(field, text, *range);
            side_effects.usage.record(
                side_effects.wall_ms,
                stats::Outcome::Accepted {
                    words: accept_word_count(text),
                },
            );
        }
        return;
    };

    // Record only after `on_accept` succeeds. A failed insert must not leak a
    // never-accepted completion into previous-input context or encrypted memory.
    record_full_accept(
        side_effects.action,
        field,
        text,
        AcceptRecording {
            context_max_chars: side_effects.context_max_chars,
            cross_app_previous_inputs: side_effects.cross_app_previous_inputs,
            previous_inputs: side_effects.previous_inputs,
            memory: side_effects.memory,
            collection_allowed: side_effects.prefs.collection_allowed(Some(&field.app)),
        },
    );
    // Absorb the accept's echo. A replacement (`replace_left > 0`, e.g. emoji)
    // deletes the typed token before inserting, so the baseline must
    // delete-then-insert to match the field; an ordinary completion is
    // append-only.
    if *replace_left > 0 {
        side_effects
            .tracker
            .apply_self_replace(field, text, *replace_left);
    } else {
        side_effects.tracker.apply_self_insert(field, text);
    }
    // Local usage stats (§11/§16): count every accept (both Word and Full —
    // unlike the full-only previous-inputs/memory block above) and the words it
    // inserted (menu-bar word count). At least one word per accept.
    side_effects.usage.record(
        side_effects.wall_ms,
        stats::Outcome::Accepted {
            words: accept_word_count(text),
        },
    );
}

/// Route ordinary monitored insertion deltas to the encrypted memory store.
/// `MemoryStore::monitor` is mode-aware: it persists only in `AllMonitored`
/// mode and no-ops in `AcceptedOnly`/`Off`, while this helper preserves the app
/// loop's privacy gates shared with accept recording.
fn record_monitored_text_with_monitor(
    field: &FieldHandle,
    text: &str,
    collection_allowed: bool,
    monitor: &mut impl FnMut(&FieldHandle, &str) -> std::result::Result<(), memory::MemoryError>,
) {
    if !collection_allowed || field.app.starts_with("pid:") || text.is_empty() {
        return;
    }
    if let Err(err) = monitor(field, text) {
        eprintln!("compme: memory monitor failed: {err}");
    }
}

/// Queue only established insertion deltas for monitored memory. Persistence is
/// delayed until after same-tick runtime policy changes are drained, so toggles
/// and snoozes apply before any durable write.
fn enqueue_monitored_change(
    pending: &mut Vec<PendingMonitoredText>,
    change: &engine::TextChange,
    app_key: Option<String>,
    domain: Option<String>,
) {
    let Some(inserted) = change.inserted_text.as_deref() else {
        return;
    };
    if inserted.is_empty() {
        return;
    }
    let app_key = app_key
        .or_else(|| (!change.field.app.starts_with("pid:")).then(|| change.field.app.clone()));
    let oversized = inserted.chars().count() > MAX_MONITORED_BUFFER_CHARS;
    pending.push(PendingMonitoredText {
        field: change.field.clone(),
        inserted: if oversized {
            if monitored_boundary(inserted) {
                " ".to_string()
            } else {
                String::new()
            }
        } else {
            inserted.to_string()
        },
        oversized,
        terminal_ok: app_key
            .as_deref()
            .is_none_or(|app| compat::terminal_prompt_activates(app, &change.value)),
        app_key,
        domain,
    });
}

fn monitored_boundary(text: &str) -> bool {
    text.chars().any(char::is_whitespace)
}

fn buffered_monitored_text(
    buffers: &mut HashMap<FieldHandle, MonitoredBuffer>,
    field: &FieldHandle,
    inserted: &str,
) -> Option<String> {
    if !buffers.contains_key(field) {
        // Fresh handle for this field: if the adapter bumped `generation` (the
        // element was replaced) without an intervening Focus event clearing the
        // map, the prior generation's Collecting buffer is orphaned — it never
        // receives another pending item, so it would linger until the next
        // Focus/policy clear. Drop those same-logical-field stale buffers here so
        // monitored_buffers can't accumulate dead keys within one session. Runs
        // only on a key-miss (first keystroke of a new field-generation), so it
        // stays off the per-keystroke hot path.
        buffers.retain(|k, _| {
            !(k.app == field.app && k.pid == field.pid && k.element_id == field.element_id)
        });
    }
    match buffers
        .entry(field.clone())
        .or_insert_with(|| MonitoredBuffer::Collecting(String::new()))
    {
        MonitoredBuffer::Collecting(buffer) => {
            buffer.push_str(inserted);
            if buffer.chars().count() > MAX_MONITORED_BUFFER_CHARS {
                if monitored_boundary(inserted) {
                    buffers.remove(field);
                } else {
                    buffers.insert(field.clone(), MonitoredBuffer::DroppedUntilBoundary);
                }
                return None;
            }
        }
        MonitoredBuffer::DroppedUntilBoundary => {
            if monitored_boundary(inserted) {
                buffers.remove(field);
            }
            return None;
        }
    }
    if !monitored_boundary(inserted) {
        return None;
    }
    match buffers.remove(field) {
        Some(MonitoredBuffer::Collecting(text)) => Some(text),
        Some(MonitoredBuffer::DroppedUntilBoundary) | None => None,
    }
}

fn clear_monitored_state_for_policy_transition(
    pending: &mut Vec<PendingMonitoredText>,
    buffers: &mut HashMap<FieldHandle, MonitoredBuffer>,
) {
    pending.clear();
    buffers.clear();
}

fn flush_monitored_changes(
    pending: &mut Vec<PendingMonitoredText>,
    buffers: &mut HashMap<FieldHandle, MonitoredBuffer>,
    memory: Option<&memory::MemoryStore>,
    prefs: &Prefs,
    policy: MonitoredPolicy,
) {
    flush_monitored_changes_with_monitor(pending, buffers, prefs, policy, |field, text| {
        if let Some(store) = memory {
            store.monitor(&field.app, text)?;
        }
        Ok(())
    });
}

fn flush_monitored_changes_with_monitor(
    pending: &mut Vec<PendingMonitoredText>,
    buffers: &mut HashMap<FieldHandle, MonitoredBuffer>,
    prefs: &Prefs,
    policy: MonitoredPolicy,
    mut monitor: impl FnMut(&FieldHandle, &str) -> std::result::Result<(), memory::MemoryError>,
) {
    if policy.secure {
        pending.clear();
        buffers.clear();
        return;
    }
    for item in pending.drain(..) {
        if !monitored_collection_gates_pass(
            item.app_key.as_deref(),
            item.domain.as_deref(),
            prefs,
            policy,
            item.terminal_ok,
        ) {
            buffers.remove(&item.field);
            continue;
        }
        let collection_allowed =
            prefs.collection_allowed(item.app_key.as_deref().or(Some(&item.field.app)));
        if !collection_allowed {
            buffers.remove(&item.field);
            continue;
        }
        if item.oversized {
            if monitored_boundary(&item.inserted) {
                buffers.remove(&item.field);
            } else {
                buffers.insert(item.field.clone(), MonitoredBuffer::DroppedUntilBoundary);
            }
            continue;
        }
        let Some(text) = buffered_monitored_text(buffers, &item.field, &item.inserted) else {
            continue;
        };
        record_monitored_text_with_monitor(&item.field, &text, collection_allowed, &mut monitor);
    }
}

struct MonitoredFlushRuntime {
    monitored_memory_active: bool,
    enabled: bool,
    trusted: bool,
    now_ms: u64,
}

struct MonitoredFlushState<'a> {
    secure: &'a mut bool,
    last_secure_poll_ms: &'a mut Option<u64>,
}

fn flush_monitored_changes_after_secure_recheck(
    pending: &mut Vec<PendingMonitoredText>,
    buffers: &mut HashMap<FieldHandle, MonitoredBuffer>,
    memory: Option<&memory::MemoryStore>,
    prefs: &Prefs,
    state: MonitoredFlushState<'_>,
    runtime: MonitoredFlushRuntime,
    secure_probe: impl FnOnce() -> bool,
) {
    if runtime.monitored_memory_active && (!pending.is_empty() || !buffers.is_empty()) {
        *state.secure = secure_probe();
        *state.last_secure_poll_ms = Some(runtime.now_ms);
    }
    flush_monitored_changes(
        pending,
        buffers,
        memory,
        prefs,
        MonitoredPolicy {
            enabled: runtime.enabled,
            secure: *state.secure,
            trusted: runtime.trusted,
            now_ms: runtime.now_ms,
        },
    );
}

/// Words inserted by an accept, for the menu-bar word count — at least one per
/// accept so an empty/whitespace payload still counts as one acceptance.
fn accept_word_count(text: &str) -> usize {
    text.split_whitespace().count().max(1)
}

/// Wall-clock epoch milliseconds, for `stats`'s rolling 30-day window (which
/// needs an absolute clock, unlike the loop's monotonic `now_ms` used for
/// latency/debounce deltas). Falls back to 0 if the system clock is before the
/// epoch (never, in practice).
fn wall_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Whether the focused app should render in the floating mirror window instead
/// of inline — true only for the `MirrorOnly` compat tier (Firefox/Zen). An
/// unresolved app (`None`) renders inline (A2 §16).
fn mirror_mode_for(app_key: Option<&str>) -> bool {
    app_key.is_some_and(|app| {
        matches!(
            compat::compatibility_tier(app),
            compat::CompatTier::MirrorOnly
        )
    })
}

/// Map an engine stat event to a usage-stats outcome.
fn stat_outcome(event: engine::StatEvent) -> stats::Outcome {
    match event {
        engine::StatEvent::Shown => stats::Outcome::Shown,
        engine::StatEvent::Superseded => stats::Outcome::Superseded,
    }
}

/// Resolve a focused field's pid to a stable bundle id for per-app preferences.
/// Pure over the resolver so the wiring is testable without AppKit; the runtime
/// passes `bundle_id_for_pid`. Returns `None` (fail-open) when there is no pid or
/// the bundle id can't be resolved.
fn resolve_app_key(pid: Option<u32>, resolver: impl Fn(i32) -> Option<String>) -> Option<String> {
    pid.and_then(|p| i32::try_from(p).ok()).and_then(resolver)
}

/// Prefer a fresh pid resolution but preserve an already-canonical field app when
/// the resolver transiently misses. Volatile `pid:N` fallback keys still fail
/// open because they are not stable preference keys.
fn effective_app_key(
    field: &FieldHandle,
    resolver: impl Fn(i32) -> Option<String>,
) -> Option<String> {
    resolve_app_key(field.pid, resolver)
        .or_else(|| (!field.app.starts_with("pid:")).then(|| field.app.clone()))
}

/// The per-app suggestion-enabled value the ToggleApp shortcut inverts: the
/// per-app `enabled` OVERRIDE if present, else the global `default_enabled`
/// baseline. Deliberately NOT `should_suggest`, which folds in snooze /
/// app-snooze / `excluded_apps` (all of which outrank `enabled`); inverting the
/// fully-gated value would write an override the gates still mask, so the toggle
/// would never converge. Pure so the toggle's invert + convergence are testable.
fn app_enabled_baseline(prefs: &Prefs, app: &str) -> bool {
    prefs
        .per_app
        .get(app)
        .and_then(|p| p.enabled)
        .unwrap_or(prefs.default_enabled)
}

fn canonicalize_field_app(
    mut field: FieldHandle,
    resolver: impl Fn(i32) -> Option<String>,
) -> (FieldHandle, Option<String>) {
    let resolved = resolve_app_key(field.pid, resolver);
    let app_key = resolved
        .clone()
        .or_else(|| (!field.app.starts_with("pid:")).then(|| field.app.clone()));
    if let Some(app) = &resolved {
        field.app = app.clone();
    }
    (field, app_key)
}

/// Squelch for repeating error logs: a failing `read_context` fires every
/// caret/typed event (heartbeat rate) while focus sits on an unsupported
/// element, flooding the log with identical lines (observed live: dozens of
/// `UnsupportedField` repeats per second). Log only when the message CHANGES;
/// a successful read resets it so the next failure is a new episode.
#[derive(Default)]
pub(crate) struct LogSquelch {
    last: Option<String>,
}

impl LogSquelch {
    fn should_log(&mut self, message: &str) -> bool {
        if self.last.as_deref() == Some(message) {
            return false;
        }
        self.last = Some(message.to_string());
        true
    }

    fn reset(&mut self) {
        self.last = None;
    }
}

/// Map a tray per-app disable arm onto the prefs store. `Always` is a hard
/// exclude — the caller persists it (COMPME_EXCLUDED_APPS); the timed arms are
/// session-only by design.
/// Apply a tray "Disable Completions Globally ▸" arm. Hour/UntilRelaunch
/// ride the global snooze (UntilRelaunch = u64::MAX minutes, the per-app
/// precedent); Always returns true so the caller flips the persistent
/// enabled flag — its existing edge handles persist + ghost dismiss.
fn apply_global_disable(arm: DisableArm, prefs: &mut Prefs, now_ms: u64) -> bool {
    match arm {
        DisableArm::Hour => {
            prefs.snooze(now_ms, SNOOZE_MINUTES);
            false
        }
        DisableArm::UntilRelaunch => {
            prefs.snooze(now_ms, u64::MAX);
            false
        }
        DisableArm::Always => true,
    }
}

fn apply_app_disable(arm: DisableArm, app: &str, prefs: &mut Prefs, now_ms: u64) {
    match arm {
        DisableArm::Hour => prefs.snooze_app(app, now_ms, SNOOZE_MINUTES),
        DisableArm::UntilRelaunch => prefs.snooze_app(app, now_ms, u64::MAX),
        DisableArm::Always => {
            prefs.excluded_apps.insert(app.to_string());
        }
    }
}

/// Apply one received `compme://` deep link (web-driven config, §8/§16):
/// strict fail-closed parse (signature-aware — a signed link needs the
/// host-pinned trusted key) then map the reversible command onto prefs.
/// Returns a user-visible summary or the failure reason; the caller logs
/// either way (the §16 "user-visible" requirement; a confirmation PROMPT is
/// the follow-up host work).
fn handle_deep_link(
    url: &str,
    trusted: Option<&webconfig::TrustedKey>,
    prefs: &mut Prefs,
    confirm: impl Fn(&webconfig::PromptDecision) -> bool,
) -> Result<String, String> {
    match webconfig::parse_deep_link_with_trust(url, trusted) {
        Ok((command, trust)) => {
            // §16 mandatory host confirmation: the pure decision says what to
            // ask; the injected closure renders it (NSAlert in production,
            // a constant in tests). Declined = rejected, prefs untouched.
            let decision = webconfig::prompt_decision_for_link(&command, trust);
            if !confirm(&decision) {
                return Err("declined by user".to_string());
            }
            prefs.apply_override(&command);
            Ok(format!(
                "applied {:?} override for {:?} ({trust:?} link)",
                command.action, command.scope
            ))
        }
        Err(err) => Err(err.to_string()),
    }
}

/// Flip per-app input collection for `app`; returns whether collection is now
/// allowed there. Re-enabling resets to inherit (None) rather than Some(true),
/// so the persisted no-collect list stays the single source.
fn toggle_app_collection(prefs: &mut Prefs, app: &str) -> bool {
    let policy = prefs.per_app.entry(app.to_string()).or_default();
    if policy.collect_inputs == Some(false) {
        policy.collect_inputs = None;
        true
    } else {
        policy.collect_inputs = Some(false);
        false
    }
}

fn sorted_join<'a>(values: impl Iterator<Item = &'a str>) -> String {
    let mut values: Vec<&str> = values.collect();
    values.sort_unstable();
    values.join(",")
}

/// The COMPME_NO_COLLECT_APPS persistence value: sorted comma-joined apps with
/// collection explicitly off, round-trippable through build_prefs.
fn no_collect_apps_value(prefs: &Prefs) -> String {
    sorted_join(
        prefs
            .per_app
            .iter()
            .filter(|(_, policy)| policy.collect_inputs == Some(false))
            .map(|(app, _)| app.as_str()),
    )
}

/// The COMPME_EXCLUDED_APPS persistence value: comma-joined, sorted for a
/// stable file diff, round-trippable through the build_prefs parser.
fn excluded_apps_value(prefs: &Prefs) -> String {
    sorted_join(prefs.excluded_apps.iter().map(String::as_str))
}

/// The COMPME_EXCLUDED_DOMAINS persistence value: normalized lowercase hosts,
/// sorted for stable diffs, round-trippable through the build_prefs parser.
fn excluded_domains_value(prefs: &Prefs) -> String {
    sorted_join(prefs.excluded_domains.iter().map(String::as_str))
}

/// The COMPME_ENABLED_APPS / COMPME_DISABLED_APPS persistence value: apps with
/// explicit web-config suggestion-policy overrides. Absent entries inherit.
fn app_override_value(
    prefs: &Prefs,
    pick: impl Fn(&prefs::AppPolicy) -> Option<bool>,
    on: bool,
) -> String {
    sorted_join(
        prefs
            .per_app
            .iter()
            .filter(|(_, policy)| pick(policy) == Some(on))
            .map(|(app, _)| app.as_str()),
    )
}

fn app_tab_disabled_value(prefs: &Prefs) -> String {
    sorted_join(
        prefs
            .per_app
            .iter()
            .filter(|(_, policy)| policy.tab_disabled)
            .map(|(app, _)| app.as_str()),
    )
}

fn persist_setting_or_log(path: &Path, key: &str, value: &str, label: &str) {
    if let Err(err) = config::persist_setting(path, key, value) {
        eprintln!("compme: could not persist {label}: {err}");
    }
}

fn remove_setting_or_log(path: &Path, key: &str, label: &str) {
    if let Err(err) = config::remove_setting(path, key) {
        eprintln!("compme: could not clear {label}: {err}");
    }
}

fn persist_web_override_prefs(path: &Path, prefs: &Prefs) {
    for (key, value, label) in [
        (
            "COMPME_EXCLUDED_APPS",
            excluded_apps_value(prefs),
            "excluded apps",
        ),
        (
            "COMPME_EXCLUDED_DOMAINS",
            excluded_domains_value(prefs),
            "excluded domains",
        ),
        (
            "COMPME_ENABLED_APPS",
            app_override_value(prefs, |p| p.enabled, true),
            "enabled apps",
        ),
        (
            "COMPME_DISABLED_APPS",
            app_override_value(prefs, |p| p.enabled, false),
            "disabled apps",
        ),
        // Per-app feature overrides edited in the Apps pane. Without these the
        // pane's MidLine/Autocorrect/Thesaurus/TabDisabled checkboxes applied
        // live but silently reverted on restart (build_prefs reads these keys).
        (
            "COMPME_MIDLINE_ON_APPS",
            app_override_value(prefs, |p| p.mid_line, true),
            "per-app mid-line on",
        ),
        (
            "COMPME_MIDLINE_OFF_APPS",
            app_override_value(prefs, |p| p.mid_line, false),
            "per-app mid-line off",
        ),
        (
            "COMPME_AUTOCORRECT_ON_APPS",
            app_override_value(prefs, |p| p.autocorrect, true),
            "per-app autocorrect on",
        ),
        (
            "COMPME_AUTOCORRECT_OFF_APPS",
            app_override_value(prefs, |p| p.autocorrect, false),
            "per-app autocorrect off",
        ),
        (
            "COMPME_GRAMMAR_FIX_ON_APPS",
            app_override_value(prefs, |p| p.grammar_fix, true),
            "per-app grammar fix on",
        ),
        (
            "COMPME_GRAMMAR_FIX_OFF_APPS",
            app_override_value(prefs, |p| p.grammar_fix, false),
            "per-app grammar fix off",
        ),
        (
            "COMPME_THESAURUS_ON_APPS",
            app_override_value(prefs, |p| p.thesaurus, true),
            "per-app thesaurus on",
        ),
        (
            "COMPME_THESAURUS_OFF_APPS",
            app_override_value(prefs, |p| p.thesaurus, false),
            "per-app thesaurus off",
        ),
        (
            "COMPME_TAB_DISABLED_APPS",
            app_tab_disabled_value(prefs),
            "per-app tab-disabled",
        ),
    ] {
        // An emptied category is REMOVED, not written as a blank `KEY=` line:
        // a blank still occupies the env-over-file layer (and clutters the
        // config), while skipping the write entirely would leave a stale value
        // when the user clears the last entry. Removal is the only correct
        // option — no stale value, no blank-key shadow (review-2026-06-13).
        if value.is_empty() {
            remove_setting_or_log(path, key, label);
        } else {
            persist_setting_or_log(path, key, &value, label);
        }
    }
}

/// Statistics-pane rows (T2): one fixed line per metric (shown/accepted/
/// words), each with a per-day sparkline over `buckets` and the span total.
/// Pure — the window only renders these strings.
fn stats_pane_lines(buckets: &[stats::DayBucket]) -> Vec<String> {
    let shown: Vec<usize> = buckets.iter().map(|b| b.counts.shown).collect();
    let accepted: Vec<usize> = buckets.iter().map(|b| b.counts.accepted).collect();
    let words: Vec<usize> = buckets.iter().map(|b| b.words).collect();
    [("Shown", shown), ("Accepted", accepted), ("Words", words)]
        .into_iter()
        .map(|(label, series)| {
            let total: usize = series.iter().sum();
            format!("{label:<9}{}  {total}", stats::sparkline(&series))
        })
        .collect()
}

fn compose_stats_lines(
    usage: &stats::Stats,
    wall_ms: u64,
    range_index: usize,
    group_index: usize,
) -> Vec<String> {
    let days = stats::StatRange::from_index(range_index).days();
    let grouping = stats::StatGrouping::from_index(group_index);
    let buckets = stats::group_buckets(&usage.daily_buckets(wall_ms, days), grouping);
    stats_pane_lines(&buckets)
}

/// Whether the Setup tab's permission re-probe is due: only while the
/// settings window is visible (hidden windows must cost nothing), at most
/// every `SECURE_POLL_INTERVAL_MS`.
fn setup_poll_due(visible: bool, last_poll_ms: Option<u64>, now_ms: u64) -> bool {
    visible
        && last_poll_ms.is_none_or(|last| now_ms.saturating_sub(last) >= SECURE_POLL_INTERVAL_MS)
}

/// True when the periodic lifetime-stats flush interval has elapsed (the
/// MONOTONIC clock — wall NTP jumps must not skew the cadence). `None`
/// (never flushed) is due immediately; the dirty check at the call site
/// keeps that from writing an untouched file at startup.
fn stats_flush_due(last_flush_ms: Option<u64>, now_ms: u64) -> bool {
    last_flush_ms.is_none_or(|last| now_ms.saturating_sub(last) >= STATS_FLUSH_INTERVAL_MS)
}

/// Write `base` + the session's grow-only totals to `path` (temp+rename).
/// Idempotent: the same state produces identical bytes, so the periodic
/// flush and the shutdown flush share this one writer. stats.env is
/// SINGLE-WRITER (this run loop) — every write overwrites from the
/// immutable startup baseline; re-reading the file here would re-add the
/// session each flush (double count). `None` path = no stats home, no-op.
fn persist_lifetime_stats(
    path: Option<&std::path::Path>,
    base: &stats::PersistedStats,
    session: stats::SessionTotals,
) -> std::io::Result<()> {
    let Some(path) = path else { return Ok(()) };
    let merged = base.merged(session.counts, session.words);
    // fsync before the rename: the periodic flush writes every ≤5 dirty
    // minutes (vs once per run pre-c128), so the power-loss window where
    // an unsynced rename persists a truncated file is no longer negligible.
    config::atomic_write_owner_only(path, stats::render_stats_file(&merged).as_bytes(), true)
}

/// The Apps tab's rows: top apps by recorded-input count (capped at the
/// window's label count), or an honest status line when collection is off /
/// nothing is recorded.
fn apps_pane_lines(counts: &[(String, u64)], collection_on: bool) -> Vec<String> {
    if !collection_on {
        return vec!["Input collection is off".to_string()];
    }
    if counts.is_empty() {
        return vec!["No recorded inputs yet".to_string()];
    }
    counts
        .iter()
        .take(crate::shell::APPS_ROWS)
        .map(|(app, n)| format!("{app} \u{2014} {n}"))
        .collect()
}

use crate::shell::keycode_label_with_mods;

/// The Shortcuts tab's text (persist-only slice): the EFFECTIVE bindings
/// (post-validation, from the platform's registered keymap — review-c114:
/// rendering raw config would lie when a colliding pair was rejected and
/// the runtime fell back to defaults), the fixed non-rebindable keys, and
/// how to change them. Static per process — bindings are read at launch
/// until the live-rebind refactor lands.
fn shortcuts_text(
    word: (i64, u32),
    full: (i64, u32),
    grammar_accept: Option<(i64, u32)>,
) -> String {
    let grammar_accept = grammar_accept
        .map(|(code, mask)| keycode_label_with_mods(code, mask))
        .unwrap_or_else(|| "Unbound".to_string());
    format!(
        "Accept word: {}\nAccept full: {}\nDismiss: Esc\nCycle candidates: Down arrow\n\
         Grammar check: config-only via COMPME_GRAMMAR_CHECK_KEY\n\
         Grammar accept: {}\n\n\
         To change: set COMPME_ACCEPT_WORD_KEY / COMPME_ACCEPT_FULL_KEY / \
         COMPME_GRAMMAR_ACCEPT_KEY (macOS keycodes, e.g. \"shift+48\") in \
         config.env \u{2014} applies at relaunch (the in-app recorder applies live).",
        keycode_label_with_mods(word.0, word.1),
        keycode_label_with_mods(full.0, full.1),
        grammar_accept,
    )
}

/// The app ids behind the Apps-tab rows, in render order with the render
/// cap — index `i` here IS row `i` of `apps_pane_lines`, the contract the
/// per-row Delete buttons rely on.
fn apps_row_ids(counts: &[(String, u64)]) -> Vec<String> {
    counts
        .iter()
        .take(crate::shell::APPS_ROWS)
        .map(|(app, _)| app.clone())
        .collect()
}

/// Whether an Apps-pane policy edit must retract the suggestion already on the
/// FOCUSED field. Disabling all suggestions or a feature that could have
/// produced the focused ghost qualifies; editing a different app's row or
/// enabling a policy leaves the focused ghost alone (the submit gate handles
/// future submits). Pure so the focused-vs-other gate is testable.
fn apps_edit_dismisses_focused(
    field: prefs::AppPolicyField,
    on: bool,
    focused_app: Option<&str>,
    edited_app: &str,
) -> bool {
    if focused_app != Some(edited_app) {
        return false;
    }
    match field {
        prefs::AppPolicyField::TabDisabled => on,
        prefs::AppPolicyField::Enabled
        | prefs::AppPolicyField::MidLine
        | prefs::AppPolicyField::Autocorrect
        | prefs::AppPolicyField::GrammarFix => !on,
    }
}

/// Map an Apps-row checkbox field index (the low part of the packed tag, see
/// `crate::shell::APP_POLICY_FIELDS`) to a `prefs::AppPolicyField`. Returns
/// `None` for an out-of-range index (a stale/garbled click no-ops, like an
/// out-of-range delete row). The order MUST match `APP_POLICY_FIELD_TITLES`.
fn apps_policy_field_from_index(index: usize) -> Option<prefs::AppPolicyField> {
    use prefs::AppPolicyField::*;
    match index {
        0 => Some(Enabled),
        1 => Some(TabDisabled),
        2 => Some(MidLine),
        3 => Some(Autocorrect),
        4 => Some(GrammarFix),
        _ => None,
    }
}

/// Resolve each Apps row's per-app policy into the `[Enabled, TabDisabled,
/// MidLine, Autocorrect, GrammarFix]` checkbox bits the settings window seeds from. One
/// entry per `app_ids` row, in the SAME order/cap as `apps_row_ids` (so the
/// window can zip it against `apps_lines` row-for-row). The bool order matches
/// `apps_policy_field_from_index` / `crate::shell::APP_POLICY_FIELD_TITLES`.
fn compose_apps_policy_bits(
    prefs: &prefs::Prefs,
    app_ids: &[String],
    global_mid_line: bool,
    global_autocorrect: bool,
    global_grammar_fix: bool,
) -> Vec<[bool; crate::shell::APP_POLICY_FIELDS]> {
    app_ids
        .iter()
        .map(|app| {
            [
                prefs
                    .per_app
                    .get(app)
                    .and_then(|p| p.enabled)
                    .unwrap_or(prefs.default_enabled),
                prefs.tab_disabled(Some(app)),
                prefs.mid_line_enabled(Some(app), global_mid_line),
                prefs.autocorrect_enabled(Some(app), global_autocorrect),
                prefs.grammar_fix_enabled(Some(app), global_grammar_fix),
            ]
        })
        .collect()
}

/// The settings window's shared state. `tray_enabled` is TrayFlags.enabled —
/// the Enabled switch and the tray checkmark are two views of that one
/// atomic (identity pinned in tests). Must run AFTER
/// set_accept_keymap_from_config so the Shortcuts text shows the
/// post-validation truth.
fn build_settings_flags(
    config: &Config,
    tray_enabled: Arc<AtomicBool>,
    launch_at_login_enabled: bool,
    available_ram_gb: u32,
) -> crate::shell::SettingsFlags {
    crate::shell::SettingsFlags {
        general_enabled: tray_enabled,
        general_launch_at_login: Arc::new(AtomicBool::new(launch_at_login_enabled)),
        labs_midline: Arc::new(AtomicBool::new(config.allow_mid_word)),
        general_autocorrect: Arc::new(AtomicBool::new(config.autocorrect)),
        general_full_autocorrect: Arc::new(AtomicBool::new(config.full_autocorrect)),
        general_thesaurus_selection: Arc::new(AtomicBool::new(config.thesaurus_selection)),
        general_trailing_space: Arc::new(AtomicBool::new(config.trailing_space)),
        context_cross_app_previous_inputs: Arc::new(AtomicBool::new(
            config.cross_app_previous_inputs,
        )),
        context_clipboard: Arc::new(AtomicBool::new(config.clipboard_context)),
        context_screen: Arc::new(AtomicBool::new(config.screen_context)),
        emoji_enabled: Arc::new(AtomicBool::new(config.emoji.is_some())),
        emoji_skin_tone_index: Arc::new(AtomicUsize::new(emoji_skin_tone_index(
            config.emoji_prefs.skin_tone,
        ))),
        emoji_gender_index: Arc::new(AtomicUsize::new(emoji_gender_index(
            config.emoji_prefs.gender,
        ))),
        stats_lines: Arc::new(Mutex::new(Vec::new())),
        about_text: crate::about::about_text(),
        setup_lines: Arc::new(Mutex::new(Vec::new())),
        setup_grant_ax: Arc::new(AtomicBool::new(false)),
        setup_request_screen: Arc::new(AtomicBool::new(false)),
        setup_reveal_models_dir: Arc::new(AtomicBool::new(false)),
        setup_choose_model: Arc::new(Mutex::new(None)),
        setup_download_model: Arc::new(AtomicBool::new(false)),
        // Picker download target: start at the recommended index so the
        // default download is byte-identical to before (the popup pre-selects
        // the same row). The names cross the crate boundary here because
        // the platform settings window can't see model_catalog (the about_text pattern).
        setup_model_index: Arc::new(AtomicUsize::new(crate::model_picker::recommended_index())),
        // Item titles carry a RAM-fit label ("name · fits/tight/exceeds")
        // computed against this machine's physical memory, read once here.
        setup_model_menu_titles: crate::model_picker::model_menu_titles(available_ram_gb),
        // Statistics range picker. Default index 0 = StatRange::ALL[0]
        // (Last 7 days), so the rendered span is byte-identical to the
        // pre-picker `daily_buckets(.., 7)`. Titles cross the seam here because
        // the platform settings window can't see the `stats` crate (the model-picker pattern).
        stat_range_index: Arc::new(AtomicUsize::new(0)),
        stat_range_titles: stats::StatRange::ALL
            .iter()
            .map(|r| r.label().to_string())
            .collect(),
        // Default index 0 = StatGrouping::ALL[0] (Daily) → group_buckets is the
        // identity, so the rendered rows are byte-identical to pre-picker.
        stat_group_index: Arc::new(AtomicUsize::new(0)),
        stat_group_titles: stats::StatGrouping::ALL
            .iter()
            .map(|g| g.label().to_string())
            .collect(),
        apps_lines: Arc::new(Mutex::new(Vec::new())),
        apps_policy_bits: Arc::new(Mutex::new(Vec::new())),
        apps_delete_row: Arc::new(Mutex::new(None)),
        apps_edit: Arc::new(Mutex::new(None)),
        shortcuts_text: {
            let (word, full, grammar_accept) =
                crate::shell::effective_accept_keys_with_mods_and_grammar();
            Arc::new(Mutex::new(shortcuts_text(word, full, grammar_accept)))
        },
        shortcuts_rebind_request: Arc::new(Mutex::new(None)),
        personalization_edit: Arc::new(Mutex::new(Vec::new())),
        // Seed the pane from the current source profile so its fields/popup
        // reflect config on open (the about_text / emoji-index pattern).
        personalization_instructions: Arc::new(Mutex::new(
            config.personalization.global_instructions.clone(),
        )),
        personalization_sender_name: Arc::new(Mutex::new(
            config.personalization.sender.name.clone(),
        )),
        personalization_sender_email: Arc::new(Mutex::new(
            config.personalization.sender.email.clone(),
        )),
        personalization_strength_index: Arc::new(AtomicUsize::new(personalization_strength_index(
            config.personalization.strength,
        ))),
        personalization_strength_titles: personalization_strength_titles(),
    }
}

/// The Setup tab's current rows as display lines: probe permissions and the
/// model file NOW (cheap queries) and render through `setup_row_line`.
fn compose_setup_lines(
    config: &Config,
    model_ready: bool,
    ax_relaunch_required: bool,
    ax_trusted: bool,
    screen_recording: bool,
    download_status: Option<&model_fetch::DownloadStatus>,
) -> Vec<String> {
    let mut lines = setup_lines_from_checks(crate::setup_state::SetupChecks {
        // Probed fresh here (cheap), not the loop's 480ms-stale copy —
        // review-c107: rows must not flip at different cadences.
        ax_trusted,
        ax_relaunch_required,
        screen_context_enabled: config.screen_context,
        screen_recording,
        model_ready,
    });
    // A download's progress/outcome lives only in the log otherwise, invisible
    // to a Finder-launched .app. Surface it as a Setup-pane suffix so the user
    // sees the click did something (and why it failed).
    if let Some(line) = model_download_status_line(download_status, model_ready) {
        lines.push(line);
    }
    lines
}

/// A one-line download-status suffix for the Setup pane, or `None` when there
/// is nothing to say: the model already loaded (`model_ready`, so the row is
/// already ✓), no download has run, or it is idle. Running shows a percent
/// when the total is known (0 total = unknown); Done points at the relaunch;
/// Failed surfaces the error the user would otherwise never see.
fn model_download_status_line(
    status: Option<&model_fetch::DownloadStatus>,
    model_ready: bool,
) -> Option<String> {
    if model_ready {
        return None;
    }
    let status = status?;
    let state = status.state.lock().unwrap_or_else(|e| e.into_inner());
    match &*state {
        model_fetch::DownloadState::Idle => None,
        model_fetch::DownloadState::Running => {
            let done = status.downloaded.load(Ordering::Relaxed);
            let total = status.total.load(Ordering::Relaxed);
            // checked_div is None for the 0 = unknown-total sentinel, so an
            // unknown total falls back to a byte count instead of a bogus %.
            Some(match done.saturating_mul(100).checked_div(total) {
                Some(pct) => format!("   downloading model\u{2026} {}%", pct.min(100)),
                None => format!("   downloading model\u{2026} {} MB", done / (1024 * 1024)),
            })
        }
        model_fetch::DownloadState::Done(_) => {
            Some("   model downloaded \u{2014} relaunch to use".into())
        }
        model_fetch::DownloadState::Failed(err) => Some(format!("   download failed: {err}")),
    }
}

fn setup_lines_from_checks(checks: crate::setup_state::SetupChecks) -> Vec<String> {
    crate::setup_state::setup_rows(checks)
        .iter()
        .map(setup_row_line)
        .collect()
}

/// One Setup-tab row: readiness glyph + label (the pane's display form of
/// `setup_state::SetupRow`).
fn setup_row_line(row: &crate::setup_state::SetupRow) -> String {
    format!(
        "{} {}",
        if row.ready { '\u{2713}' } else { '\u{2717}' },
        row.label
    )
}

/// The Statistics pane's lifetime row: persisted totals merged with the live
/// session. Words and accepted only — no per-day series exists across
/// restarts (stats.env stores grow-only counters), so no sparkline.
fn lifetime_line(merged: &stats::PersistedStats) -> String {
    format!(
        "Lifetime {} words \u{b7} {} accepted",
        merged.words, merged.accepted
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionUsageSnapshot {
    counts: stats::Counts,
    words: usize,
    latency_avg: Option<u32>,
    latency_p95: Option<u32>,
}

fn session_usage_snapshot(usage: &stats::Stats, wall_ms: u64) -> SessionUsageSnapshot {
    SessionUsageSnapshot {
        counts: usage.counts(wall_ms),
        words: usage.words_completed(wall_ms),
        latency_avg: usage.latency_avg_ms(wall_ms),
        latency_p95: usage.latency_p95_ms(wall_ms),
    }
}

fn apply_emoji_enabled(
    config_emoji: &mut Option<EmojiPrefs>,
    saved_prefs: &mut EmojiPrefs,
    enabled: bool,
) {
    if enabled {
        *config_emoji = Some(*saved_prefs);
    } else {
        if let Some(prefs) = config_emoji.take() {
            *saved_prefs = prefs;
        }
    }
}

fn apply_emoji_skin_tone(
    config_emoji: &mut Option<EmojiPrefs>,
    saved_prefs: &mut EmojiPrefs,
    tone: SkinTone,
) {
    saved_prefs.skin_tone = tone;
    if let Some(prefs) = config_emoji.as_mut() {
        prefs.skin_tone = tone;
    }
}

fn handle_emoji_switch_edge(
    flag: &AtomicBool,
    current: &mut bool,
    config_emoji: &mut Option<EmojiPrefs>,
    saved_prefs: &mut EmojiPrefs,
    mut persist: impl FnMut(bool),
) -> Option<bool> {
    let on = switch_edge(flag, current)?;
    apply_emoji_enabled(config_emoji, saved_prefs, on);
    persist(on);
    Some(on)
}

fn handle_emoji_skin_tone_change(
    flag: &AtomicUsize,
    current: &mut usize,
    config_emoji: &mut Option<EmojiPrefs>,
    saved_prefs: &mut EmojiPrefs,
    mut persist: impl FnMut(&'static str),
) -> Option<SkinTone> {
    let now = flag
        .load(Ordering::Relaxed)
        .min(EMOJI_SKIN_TONE_VALUES.len() - 1);
    if now == *current {
        return None;
    }
    *current = now;
    let tone = emoji_skin_tone_from_index(now);
    apply_emoji_skin_tone(config_emoji, saved_prefs, tone);
    persist(emoji_skin_tone_value(tone));
    Some(tone)
}

fn handle_emoji_skin_tone_change_with_invalidation(
    flag: &AtomicUsize,
    current: &mut usize,
    config_emoji: &mut Option<EmojiPrefs>,
    saved_prefs: &mut EmojiPrefs,
    persist: impl FnMut(&'static str),
    mut invalidate_visible_suggestion: impl FnMut(),
) -> Option<SkinTone> {
    let tone = handle_emoji_skin_tone_change(flag, current, config_emoji, saved_prefs, persist)?;
    invalidate_visible_suggestion();
    Some(tone)
}

fn apply_emoji_gender(
    config_emoji: &mut Option<EmojiPrefs>,
    saved_prefs: &mut EmojiPrefs,
    gender: Gender,
) {
    saved_prefs.gender = gender;
    if let Some(prefs) = config_emoji.as_mut() {
        prefs.gender = gender;
    }
}

fn handle_emoji_gender_change(
    flag: &AtomicUsize,
    current: &mut usize,
    config_emoji: &mut Option<EmojiPrefs>,
    saved_prefs: &mut EmojiPrefs,
    mut persist: impl FnMut(&'static str),
) -> Option<Gender> {
    let now = flag
        .load(Ordering::Relaxed)
        .min(EMOJI_GENDER_VALUES.len() - 1);
    if now == *current {
        return None;
    }
    *current = now;
    let gender = emoji_gender_from_index(now);
    apply_emoji_gender(config_emoji, saved_prefs, gender);
    persist(emoji_gender_value(gender));
    Some(gender)
}

fn handle_emoji_gender_change_with_invalidation(
    flag: &AtomicUsize,
    current: &mut usize,
    config_emoji: &mut Option<EmojiPrefs>,
    saved_prefs: &mut EmojiPrefs,
    persist: impl FnMut(&'static str),
    mut invalidate_visible_suggestion: impl FnMut(),
) -> Option<Gender> {
    let gender = handle_emoji_gender_change(flag, current, config_emoji, saved_prefs, persist)?;
    invalidate_visible_suggestion();
    Some(gender)
}

/// Persist one switch edge and log it. A persist failure is logged, not
/// retried — the runtime value wins until relaunch (deliberate graceful
/// degradation: an IO hiccup must not stall the app, at the cost of
/// config.env staying stale until the next successful write).
fn persist_and_log_switch(key: &str, label: &str, enabled: bool) {
    if let Some(path) = config::config_file_path() {
        if let Err(err) = config::persist_setting(&path, key, switch_value(enabled)) {
            eprintln!("compme: failed to persist {key}: {err}");
        }
    }
    eprintln!(
        "compme: {label} {}",
        if enabled { "enabled" } else { "disabled" }
    );
}

fn persist_and_log_value(key: &str, label: &str, value: &str) {
    if let Some(path) = config::config_file_path() {
        if let Err(err) = config::persist_setting(&path, key, value) {
            eprintln!("compme: failed to persist {key}: {err}");
        }
    }
    eprintln!("compme: {label} set to {value}");
}

/// Compose the Apps tab's rows + the parallel app-id list from the store.
/// ONE source for the show edge and the post-delete recompose (audit c121:
/// the match was duplicated verbatim).
fn compose_apps_rows(store: Option<&memory::MemoryStore>) -> (Vec<String>, Vec<String>) {
    match store {
        Some(store) => match store.count_by_app() {
            Ok(counts) => (apps_pane_lines(&counts, true), apps_row_ids(&counts)),
            Err(err) => (vec![format!("Store error: {err}")], Vec::new()),
        },
        None => (apps_pane_lines(&[], false), Vec::new()),
    }
}

/// Resolve a clicked Apps-row index against the ids rendered with the SAME
/// cap/order, delete that app's history, and return the recomposed rows.
/// `None` = out-of-range row (stale click) — nothing deleted. The confirm
/// prompt stays at the caller (FFI lives at the consume edge).
fn delete_app_row_and_recompose(
    store: &memory::MemoryStore,
    ids: &[String],
    row: usize,
) -> Option<(Vec<String>, Vec<String>)> {
    let app = ids.get(row)?;
    match store.delete_app(app) {
        Ok(n) => eprintln!("compme: deleted {n} records for {app}"),
        Err(err) => eprintln!("compme: delete for {app} failed: {err}"),
    }
    Some(compose_apps_rows(Some(store)))
}

/// The persistence value for a boolean settings switch (COMPME_MIDLINE,
/// COMPME_AUTOCORRECT), paired with the launch parsers (`"1"`/`"true"`
/// truthy; everything else off).
fn switch_value(enabled: bool) -> &'static str {
    if enabled {
        "1"
    } else {
        "0"
    }
}

/// How long the tray's fixed "Snooze for 1 hour" action pauses suggestions.
const SNOOZE_MINUTES: u64 = 60;

/// Apply a consumed tray snooze request: pause all suggestions for
/// [`SNOOZE_MINUTES`] from `now_ms` (the monotonic loop clock — a relaunch
/// deliberately clears a snooze). Returns whether a snooze was applied.
fn apply_snooze_request(requested: bool, prefs: &mut Prefs, now_ms: u64) -> bool {
    if requested {
        prefs.snooze(now_ms, SNOOZE_MINUTES);
    }
    requested
}

/// Strict tri-state boolean: explicit truthy → `Some(true)`, explicit falsy →
/// `Some(false)`, absent/unrecognized → `None` (callers treat `None` as
/// "leave the current state alone" — a typo must never flip a login item).
fn parse_tri_state(raw: Option<String>) -> Option<bool> {
    match raw.as_deref().map(str::trim).map(str::to_ascii_lowercase) {
        Some(v) if v == "1" || v == "true" || v == "on" || v == "yes" => Some(true),
        Some(v) if v == "0" || v == "false" || v == "off" || v == "no" => Some(false),
        _ => None,
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
            eprintln!("compme: {what} error: {err:?}");
            Vec::new()
        }
    }
}

fn offer_all(latest: &mut LatestRequest, requests: Vec<CompletionRequest>) {
    for request in requests {
        latest.offer(request);
    }
}

fn apply_grammar_shortcut_pending_effect(
    latest: &mut LatestRequest,
    manual_grammar_request: &mut Option<CompletionRequest>,
    outcome: &GrammarCheckShortcutOutcome,
) {
    match outcome {
        GrammarCheckShortcutOutcome::BlockedAfterRead | GrammarCheckShortcutOutcome::NotArmed => {
            latest.clear();
            *manual_grammar_request = None;
        }
        GrammarCheckShortcutOutcome::Armed(request) => {
            latest.clear();
            *manual_grammar_request = Some(request.clone());
        }
        GrammarCheckShortcutOutcome::NoField
        | GrammarCheckShortcutOutcome::BlockedBeforeRead
        | GrammarCheckShortcutOutcome::ReadContextError(_)
        | GrammarCheckShortcutOutcome::CapabilitiesError(_) => {
            *manual_grammar_request = None;
        }
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

/// Whether a ToggleApp shortcut must dismiss the on-screen suggestion. The
/// toggle inverts the focused app's per-app `enabled` baseline, so it disables
/// (and must retract any ghost) exactly when the app was enabled BEFORE the
/// toggle. Unlike ToggleGlobal/SIGUSR1 this never moves the global `enabled`
/// atomic, so the per-tick `should_dismiss_on_disable` reconciliation can not
/// cover it — this seam is the only retraction. Pure so the decision is
/// testable without driving the whole run loop.
fn toggle_app_dismisses(prev_enabled: bool) -> bool {
    prev_enabled
}

fn secure_input_caps() -> Capabilities {
    Capabilities {
        readable_text: false,
        readable_caret: false,
        writable: false,
        assistant_field: false,
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
            | AppStatus::Blocked(
                BlockReason::Permission
                    | BlockReason::RelaunchRequired
                    | BlockReason::SecureInput
                    | BlockReason::ModelUnavailable,
            )
    )
}

#[derive(Debug, PartialEq, Eq)]
enum SubscriptionErrorAction {
    NoopUntilPermission,
    Fatal(String),
}

fn subscription_error_action(trusted: bool, err: &PlatformError) -> SubscriptionErrorAction {
    match err {
        PlatformError::PermissionMissing { .. } => SubscriptionErrorAction::NoopUntilPermission,
        _ if !trusted => SubscriptionErrorAction::NoopUntilPermission,
        _ => SubscriptionErrorAction::Fatal(format!("{err:?}")),
    }
}

fn runtime_trusted(accessibility_trusted: bool, subscriptions_require_relaunch: bool) -> bool {
    accessibility_trusted && !subscriptions_require_relaunch
}

fn apply_startup_key_bindings(config: &Config) {
    // Rebound accept keys (cycle-13 residual): set the process-wide keymap
    // before suggestions can arm accept handling, so the Carbon registration,
    // the decision logic, and the handler's id->keycode inverse all read one
    // source. Collision/invalid -> fail soft to defaults.
    if config.accept_word_key.is_some()
        || config.accept_full_key.is_some()
        || config.grammar_accept_key.is_some()
    {
        match crate::shell::set_accept_keymap_from_config_with_mods(
            config.accept_word_key,
            config.accept_full_key,
            config.grammar_accept_key,
        ) {
            Ok(()) => eprintln!(
                "compme: accept keys rebound (word={:?} full={:?} grammar={:?})",
                config.accept_word_key, config.accept_full_key, config.grammar_accept_key
            ),
            Err(err) => {
                eprintln!("compme: accept-key rebind invalid ({err:?}); using defaults")
            }
        }
    }

    // Always-on (global) shortcuts must be configured before subscribe_accept():
    // that subscription installs their process-lifetime Carbon hotkeys once.
    // Setting this afterward logs plausible bindings but leaves no registered
    // shortcut until relaunch.
    if config.force_activate_key.is_some()
        || config.toggle_app_key.is_some()
        || config.toggle_global_key.is_some()
        || config.grammar_check_key.is_some()
    {
        let bindings = crate::shell::set_shortcut_bindings_from_config(
            config.force_activate_key.as_deref(),
            config.toggle_app_key.as_deref(),
            config.toggle_global_key.as_deref(),
            config.grammar_check_key.as_deref(),
        );
        eprintln!("compme: global shortcuts configured ({bindings:?})");
    }
}

fn subscribe_accept_after_startup_key_bindings(
    config: &Config,
    trusted: bool,
    subscribe: impl FnOnce() -> Result<AcceptSubscription, PlatformError>,
) -> Result<(AcceptSubscription, bool), String> {
    apply_startup_key_bindings(config);
    match subscribe() {
        Ok(sub) => Ok((sub, false)),
        Err(err) => match subscription_error_action(trusted, &err) {
            SubscriptionErrorAction::NoopUntilPermission => {
                eprintln!(
                    "compme: accept subscription unavailable until Accessibility is granted — grant it, then relaunch: {err:?}"
                );
                Ok((noop_accept_subscription(), true))
            }
            SubscriptionErrorAction::Fatal(message) => Err(format!("subscribe accept: {message}")),
        },
    }
}

fn should_request_screen_recording(screen_context: bool, already_granted: bool) -> bool {
    screen_context && !already_granted
}

fn noop_accept_subscription() -> AcceptSubscription {
    AcceptSubscription::new(Subscription::new(0), |_| Ok(()), |_| Ok(()), |_| Ok(()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum InstanceStartupDecision {
    ExitOk(String),
    Fail(String),
}

fn instance_startup_decision(error: Option<config::InstanceLockError>) -> InstanceStartupDecision {
    match error {
        None => InstanceStartupDecision::Fail(
            "compme: no config dir for the instance lock — refusing to start unguarded".into(),
        ),
        Some(config::InstanceLockError::Held) => InstanceStartupDecision::ExitOk(
            "compme: another instance is already running — exiting".into(),
        ),
        Some(config::InstanceLockError::Io(err)) => InstanceStartupDecision::Fail(format!(
            "compme: instance lock unavailable ({err}) — refusing to start unguarded"
        )),
    }
}

fn instance_lock_startup_gate<L>(
    path: Option<std::path::PathBuf>,
    acquire: impl FnOnce(&std::path::Path) -> Result<L, config::InstanceLockError>,
    after_lock_acquired: impl FnOnce(),
) -> Result<Option<L>, String> {
    let Some(path) = path else {
        return match instance_startup_decision(None) {
            InstanceStartupDecision::ExitOk(message) => {
                eprintln!("{message}");
                Ok(None)
            }
            InstanceStartupDecision::Fail(message) => Err(message),
        };
    };
    match acquire(&path) {
        Ok(lock) => {
            after_lock_acquired();
            Ok(Some(lock))
        }
        Err(err) => match instance_startup_decision(Some(err)) {
            InstanceStartupDecision::ExitOk(message) => {
                eprintln!("{message}");
                Ok(None)
            }
            InstanceStartupDecision::Fail(message) => Err(message),
        },
    }
}

/// Fatal startup condition, reported exactly as `run()` has always reported
/// it. An alias (not a newtype) so the moved startup block's `?` operators
/// and `format!` error arms compile unchanged.
type StartupError = String;

/// Factory-field aliases (clippy::type_complexity): the instance-lock
/// acquisition and tray-construction closures.
type InstanceLockAcquire =
    Box<dyn Fn(&Path) -> Result<config::InstanceLock, config::InstanceLockError>>;
type TrayFactory = Box<dyn Fn(TrayFlags) -> Result<Box<dyn TrayHandle>, PlatformError>>;

/// The constructors [`startup`] calls instead of hard-wired platform/shell
/// functions: production passes [`real_factories`]; tests pass recording
/// fakes. Generic over the adapter/overlay (like `SharedAdapter<A>`) so tests
/// can inject inert fakes without touching the real platform types.
struct RunFactories<A: PlatformAdapter, O: OverlayPresenter> {
    instance_lock_path: Box<dyn Fn() -> Option<PathBuf>>,
    try_acquire_instance_lock: InstanceLockAcquire,
    load_config: Box<dyn Fn() -> Result<Config, StartupError>>,
    install_signal_handlers: Box<dyn Fn()>,
    make_shell: Box<dyn Fn() -> Arc<dyn ShellHost>>,
    make_adapter: Box<dyn Fn(Option<i32>) -> Result<A, PlatformError>>,
    make_overlay: Box<dyn Fn() -> Result<O, PlatformError>>,
    make_tray: TrayFactory,
}

/// The production constructors — the same functions `run()` called directly
/// before the extraction.
fn real_factories(
) -> RunFactories<crate::shell::PlatformAdapterImpl, crate::shell::OverlayPresenterImpl> {
    RunFactories {
        instance_lock_path: Box::new(config::instance_lock_path),
        try_acquire_instance_lock: Box::new(config::try_acquire_instance_lock),
        load_config: Box::new(Config::from_env),
        install_signal_handlers: Box::new(install_signal_handlers),
        make_shell: Box::new(crate::shell::make_shell),
        make_adapter: Box::new(crate::shell::make_adapter),
        make_overlay: Box::new(crate::shell::make_overlay),
        make_tray: Box::new(crate::shell::make_tray),
    }
}

/// Everything the heartbeat loop (and the teardown after it) needs out of
/// startup, in the order the pieces were originally bound.
struct RunContext<A: PlatformAdapter, O: OverlayPresenter> {
    instance_lock: config::InstanceLock,
    config: Config,
    shell: Arc<dyn ShellHost>,
    trusted: bool,
    adapter: Arc<A>,
    engine: Engine<SharedAdapter<A>, O>,
    host_events: Arc<Mutex<VecDeque<HostEvent>>>,
    focus_sub: Subscription,
    caret_sub: Subscription,
    subscriptions_require_relaunch: bool,
    model_available: bool,
    deep_links: Arc<Mutex<Vec<String>>>,
    url_handler: Option<crate::shell::UrlHandlerGuard>,
    launch_at_login_enabled: bool,
    previous_inputs: PreviousInputs,
    memory: Option<memory::MemoryStore>,
    monitored_memory_active: bool,
    clipboard_cell: Arc<Mutex<Option<String>>>,
    screen_cell: Arc<Mutex<Option<ScreenContext>>>,
    context_bound: usize,
    screen_ocr: Option<ScreenOcr>,
    screen_wait_ms: Arc<AtomicU64>,
    cross_app_previous_inputs: Arc<AtomicBool>,
    inference: InferenceHandle,
    flags: TrayFlags,
    prefs: Prefs,
    tray: Option<Box<dyn TrayHandle>>,
}

/// Build the whole stack the heartbeat loop needs: instance lock → config →
/// signal handlers → permission prompt → adapter/overlay/engine construction
/// → inference spawn. `Ok(None)` is the clean second-instance exit (the
/// instance-lock gate's ExitOk arm — not an error): `run()` maps it to
/// `Ok(())` exactly as the inline code did.
fn startup<A: PlatformAdapter, O: OverlayPresenter>(
    factories: &RunFactories<A, O>,
) -> Result<Option<RunContext<A, O>>, StartupError> {
    // Single-instance guard FIRST — before any AX observer, hotkey
    // registration, or Apple Events handler exists. Two instances double all
    // of those (live c92 finding: open(1) launches a second copy via Launch
    // Services when the registered handler isn't already running). flock is
    // launch-method-agnostic and kernel-released on any exit.
    let Some(instance_lock) = instance_lock_startup_gate(
        (factories.instance_lock_path)(),
        |path| (factories.try_acquire_instance_lock)(path),
        || {},
    )?
    else {
        return Ok(None);
    };

    // Mutable: General-tab switches update globals live (autocorrect today;
    // enabled/trailing-space later) — field writes between heartbeats only.
    let mut config = (factories.load_config)()?;
    (factories.install_signal_handlers)();
    let shell = (factories.make_shell)();

    // Permissions: if Accessibility isn't granted, fire the system prompt once.
    // The app keeps running and reflects the Blocked state in the tray. Focus,
    // caret, and accept subscriptions are installed once at startup; if any of
    // them degrade to no-op while permission is missing, granting Accessibility
    // later still requires a relaunch to install real event streams.
    // (`mut` lives on the run-loop binding: the loop re-polls trust, startup
    // only reads it.)
    let trusted = shell.accessibility_trusted();
    if !trusted {
        eprintln!("compme: Accessibility not granted — requesting permission");
        shell.prompt_accessibility_trust();
    }

    // Domain-rule transparency (audit c121): a rule pasted as a full URL
    // would never match a bare-host domain — lint it. (The "rules are
    // inert" startup warning retired with c131: the AX detection source
    // ships; live validation is the remaining LOOK item.)
    for rule in &config.prefs.excluded_domains {
        if let Some(host) = domain_from_url(rule) {
            eprintln!(
                "compme: domain rule '{rule}' looks like a URL \u{2014} did you mean '{host}'?"
            );
        }
    }

    // Env-shadow notice (review-c109): switches whose env var will override
    // the persisted file at relaunch.
    for warning in startup_env_shadow_notice_lines(|key| env::var(key).is_ok()) {
        eprintln!("{warning}");
    }

    if config.diag_coords {
        eprintln!("compme: diag display_scales={:?}", shell.display_scales());
    }

    let adapter = (factories.make_adapter)(config.acceptance_pid)
        .map_err(|err| format!("adapter init: {err:?}"))?;
    let adapter = Arc::new(adapter);

    let overlay = (factories.make_overlay)().map_err(|err| format!("overlay init: {err:?}"))?;

    let mut engine = Engine::new(
        SharedAdapter::new(Arc::clone(&adapter)),
        overlay,
        config.debounce_ms,
        config.max_words,
        config.max_tokens,
    )
    .with_trigger_gates(config.min_context_chars, config.allow_mid_word)
    .with_trailing_space(config.trailing_space);

    // Callbacks fire on the dispatcher thread; keep enqueueing non-blocking and
    // bounded so a burst cannot grow memory without limit.
    let host_events: Arc<Mutex<VecDeque<HostEvent>>> = Arc::new(Mutex::new(VecDeque::new()));

    let focus_events = Arc::clone(&host_events);
    let (focus_sub, focus_subscription_requires_relaunch) = match adapter.subscribe_focus(Arc::new(
        move |field| {
            let _ = push_host_event(&focus_events, HostEvent::Focus(field));
        },
    )) {
        Ok(sub) => (sub, false),
        Err(err) => match subscription_error_action(trusted, &err) {
            SubscriptionErrorAction::NoopUntilPermission => {
                eprintln!(
                    "compme: focus subscription unavailable until Accessibility is granted — grant it, then relaunch: {err:?}"
                );
                (Subscription::new(0), true)
            }
            SubscriptionErrorAction::Fatal(message) => {
                return Err(format!("subscribe focus: {message}"));
            }
        },
    };

    let caret_events = Arc::clone(&host_events);
    let (caret_sub, caret_subscription_requires_relaunch) = match adapter.subscribe_caret(Arc::new(
        move |field, rect| {
            let _ = push_host_event(&caret_events, HostEvent::Caret(field, rect));
        },
    )) {
        Ok(sub) => (sub, false),
        Err(err) => match subscription_error_action(trusted, &err) {
            SubscriptionErrorAction::NoopUntilPermission => {
                eprintln!(
                    "compme: caret subscription unavailable until Accessibility is granted — grant it, then relaunch: {err:?}"
                );
                (Subscription::new(0), true)
            }
            SubscriptionErrorAction::Fatal(message) => {
                return Err(format!("subscribe caret: {message}"));
            }
        },
    };

    let accept_events = Arc::clone(&host_events);
    let (accept_sub, accept_subscription_requires_relaunch) =
        subscribe_accept_after_startup_key_bindings(&config, trusted, || {
            adapter.subscribe_accept(Arc::new(move |control| {
                let event = match control {
                    TapControl::Accept(action) => HostEvent::Accept(action),
                    TapControl::Dismiss => HostEvent::Dismiss,
                    TapControl::Cycle => HostEvent::Cycle,
                    TapControl::Shortcut(action) => HostEvent::Shortcut(action),
                };
                if !push_host_event(&accept_events, event) {
                    eprintln!("compme: host control event dropped: queue full");
                }
            }))
        })?;
    let subscriptions_require_relaunch = focus_subscription_requires_relaunch
        || caret_subscription_requires_relaunch
        || accept_subscription_requires_relaunch;
    engine.set_accept_subscription(accept_sub);

    // Auto-adopt an already-downloaded model when the configured path is
    // unusable (COMPME_MODEL_PATH unset → nonexistent DEFAULT_MODEL, or a
    // stale/deleted path). Downloads land in the app-support models dir but
    // the loader only reads env/file/default, so a model the user already
    // downloaded would never load and the Setup row would stay ✗ forever. An
    // explicit COMPME_MODEL_PATH wins only when it is a readable GGUF with the
    // expected magic. Persist fallback adoption so later launches skip the scan.
    if let Some(found) = downloaded_model_to_adopt(
        config.stub_completion.as_deref(),
        &config.model_path,
        app_support_models_dir().as_deref(),
    ) {
        eprintln!("compme: adopting downloaded model {}", found.display());
        if let Some(cfg) = config::config_file_path() {
            if let Err(err) =
                config::persist_setting(&cfg, "COMPME_MODEL_PATH", &found.to_string_lossy())
            {
                eprintln!("compme: failed to persist adopted COMPME_MODEL_PATH: {err}");
            }
        }
        config.model_path = found;
    }

    let model = match load_model(resolve_source(
        config.stub_completion.clone(),
        config.model_path.clone(),
    )) {
        Ok(model) => Some(model),
        Err(err) => {
            eprintln!("compme: model unavailable at startup: {err}");
            eprintln!("compme: setup remains available; download or select a model, then relaunch");
            None
        }
    };
    let model_available = model.is_some();
    // Setup status (the Setup pane's row model doubles as the startup
    // diagnostic): one line per not-ready item, so a log alone explains why
    // ghosts won't appear (missing permission, missing model file).
    for row in crate::setup_state::setup_rows(crate::setup_state::SetupChecks {
        ax_trusted: trusted,
        ax_relaunch_required: subscriptions_require_relaunch,
        screen_context_enabled: config.screen_context,
        screen_recording: shell.screen_capture_permission(),
        model_ready: model_available,
    }) {
        if !row.ready {
            eprintln!("compme: setup: {} not ready", row.label);
        }
    }
    // Screen-recording context (optional, A2 §16): request the permission once if
    // the user opted in. The app continues with field-only context if denied
    // (the "works without it" requirement); local OCR enrichment rides on this
    // grant.
    if config.screen_context && !shell.screen_capture_permission() {
        eprintln!("compme: requesting Screen Recording permission (screen context)");
        shell.request_screen_capture_permission();
        // The grant takes effect on the NEXT launch (macOS shows the prompt async
        // and re-reads TCC at startup), so screen context is inactive this run.
        eprintln!("compme: restart after granting Screen Recording to enable screen context");
    }

    // compme:// deep-link reception (web-driven config §8/§16): Launch
    // Services routes scheme opens as Apple Events; the handler enqueues the
    // raw URL and the heartbeat drains it through the strict fail-closed
    // parser. Install failure is non-fatal (deep links just stay inert).
    let deep_links: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let deep_links_in_handler = Arc::clone(&deep_links);
    let _url_handler = match crate::shell::install_url_event_handler(Arc::new(move |url| {
        let mut queue = deep_links_in_handler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !enqueue_deep_link(&mut queue, url) {
            eprintln!("compme: deep-link event dropped: URL too large");
        }
    })) {
        Ok(handler) => Some(handler),
        Err(err) => {
            eprintln!("compme: deep-link handler unavailable: {err}");
            None
        }
    };

    // Launch-at-login (A3 D13): apply only an EXPLICIT config choice; absent
    // leaves the user's Login Items alone. Non-fatal — a bare cargo binary
    // (no bundle) is expected to fail here, and the bundled app is the real
    // consumer.
    let mut launch_at_login_enabled = false;
    if let Some(enabled) = config.launch_at_login {
        match shell.set_launch_at_login(enabled) {
            Ok(()) => {
                launch_at_login_enabled = enabled;
                eprintln!(
                    "compme: launch at login {}",
                    if enabled { "ON" } else { "OFF" }
                )
            }
            Err(err) => eprintln!("compme: launch-at-login unavailable: {err}"),
        }
    }

    let previous_inputs = PreviousInputs::default();
    // Encrypted on-disk memory (A2 §6/§16). Off unless COMPME_MEMORY + path are
    // configured; the key comes from COMPME_MEMORY_KEY or (default) the macOS
    // Keychain, generated on first use. Lives on this thread (the rusqlite
    // handle is not Send). AcceptedOnly records Full accepts; AllMonitored also
    // records established non-secure insertion deltas.
    let memory = open_memory_store(&config.memory, || match shell.load_or_create_memory_key() {
        Ok(key) => Some(key),
        Err(err) => {
            eprintln!("compme: OS key store memory key unavailable: {err}");
            None
        }
    });
    let monitored_memory_active =
        config.memory.mode == memory::StorageMode::AllMonitored && memory.is_some();
    let clipboard_cell: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let screen_cell: Arc<Mutex<Option<ScreenContext>>> = Arc::new(Mutex::new(None));
    // Screen OCR only contributes when the grant is actually present this session.
    let screen_active = config.screen_context && shell.screen_capture_permission();
    // Clipboard/screen context work independently of previous-input context.
    // The Settings pane can enable clipboard context after launch, so keep the
    // worker bound positive enough for a later live enable.
    let context_bound = settings_context_bound_chars(config.context_max_chars);
    // Screen OCR (~200–800 ms) runs on its own thread so it never stalls
    // this host UI loop (overlay repaint + accept-hotkey callbacks). It
    // publishes redacted text into `screen_cell`, which the inference worker
    // waits for briefly off the UI loop and accepts only when stamped for
    // the submitted request.
    let screen_ocr = if screen_active {
        match ScreenOcr::spawn(
            Arc::clone(&shell),
            Arc::clone(&screen_cell),
            context_bound,
            config.diag_context,
        ) {
            Ok(ocr) => Some(ocr),
            Err(err) => {
                eprintln!("compme: screen OCR worker unavailable: {err}; screen context disabled");
                config.screen_context = false;
                persist_and_log_switch("COMPME_SCREEN_CONTEXT", "screen context", false);
                None
            }
        }
    } else {
        None
    };
    let screen_wait_ms = WorkerContext::screen_wait_cell(if screen_ocr.is_some() {
        Duration::from_millis(SCREEN_CONTEXT_WAIT_MS)
    } else {
        Duration::ZERO
    });
    let cross_app_previous_inputs = Arc::new(AtomicBool::new(config.cross_app_previous_inputs));
    let worker_context = WorkerContext {
        previous_inputs: previous_inputs.clone(),
        cross_app_previous_inputs: Arc::clone(&cross_app_previous_inputs),
        clipboard: Arc::clone(&clipboard_cell),
        screen: Arc::clone(&screen_cell),
        screen_wait_ms: Arc::clone(&screen_wait_ms),
        max_chars: context_bound,
        diag_context: config.diag_context,
    };
    let inference = match model {
        Some(model) => InferenceHandle::spawn(
            model,
            config.prompt_mode,
            config.personalization.clone(),
            config.candidates,
            worker_context,
        )?,
        None => InferenceHandle::unavailable(),
    };

    // Shared state for the tray; flipped by menu actions, observed by this loop.
    let flags = TrayFlags {
        enabled: Arc::new(AtomicBool::new(config.enabled)),
        quit: Arc::new(AtomicBool::new(false)),
        open_settings: Arc::new(AtomicBool::new(false)),
        snooze_requested: Arc::new(AtomicBool::new(false)),
        global_disable: Arc::new(Mutex::new(None)),
        open_settings_window: Arc::new(AtomicBool::new(false)),
        check_updates: Arc::new(AtomicBool::new(false)),
        visit_website: Arc::new(AtomicBool::new(false)),
        contact_support: Arc::new(AtomicBool::new(false)),
        collection_toggle: Arc::new(AtomicBool::new(false)),
        app_disable: Arc::new(Mutex::new(None)),
    };
    // Runtime-mutable policy (snooze); starts from the configured prefs. The
    // ONE prefs the loop reads — never read config.prefs after this point, or
    // the policy source splits. (`mut` lives on the run-loop binding.)
    let prefs = config.prefs.clone();
    // A tray failure is non-fatal — the engine still runs headless.
    let tray = match (factories.make_tray)(flags.clone()) {
        Ok(tray) => Some(tray),
        Err(err) => {
            eprintln!("compme: tray unavailable: {err:?}");
            None
        }
    };

    Ok(Some(RunContext {
        instance_lock,
        config,
        shell,
        trusted,
        adapter,
        engine,
        host_events,
        focus_sub,
        caret_sub,
        subscriptions_require_relaunch,
        model_available,
        deep_links,
        url_handler: _url_handler,
        launch_at_login_enabled,
        previous_inputs,
        memory,
        monitored_memory_active,
        clipboard_cell,
        screen_cell,
        context_bound,
        screen_ocr,
        screen_wait_ms,
        cross_app_previous_inputs,
        inference,
        flags,
        prefs,
        tray,
    }))
}

/// Build the whole stack, run until a signal (or the run-ms deadline), then tear
/// down in order.
/// Heartbeat phase: the Setup pane's Download Model click plus the
/// download-progress log/auto-wire edge. Split out of `run()` verbatim
/// (2026-07-25 audit, F16).
fn model_download_phase(
    settings_flags: &crate::shell::SettingsFlags,
    shell: &Arc<dyn ShellHost>,
    config: &mut Config,
    available_ram_gb: u32,
    model_downloader: &mut Option<model_fetch::ModelDownloader>,
    download: &mut DownloadState,
) {
    // Setup "Download Model": fetch the model the picker has selected
    // (setup_model_index; defaults to the recommended entry) into the
    // app-support models dir. Progress is logged; on Done the log says
    // how to point COMPME_MODEL_PATH at it.
    if settings_flags
        .setup_download_model
        .swap(false, Ordering::Relaxed)
        && download_idle(download.model_download_status.as_deref())
    {
        if let Some(models_dir) = app_support_models_dir() {
            // Selected-or-recommended, RAM hard block, and license
            // click-through live in a pure decision helper so this edge is
            // covered as a single app-level policy before download IO.
            let selected_index = settings_flags.setup_model_index.load(Ordering::Relaxed);
            let decision = model_download_click_decision(
                selected_index,
                available_ram_gb,
                &mut config.license_accepted,
                |model, license_name, terms_url| {
                    shell
                        .confirm(&shell_flags::ConfirmPrompt {
                            title: "Accept model license?",
                            message: &format!(
                                "{model} is distributed under the {license_name}.\n\
                                 Downloading requires accepting its terms:\n{terms_url}"
                            ),
                            confirm_label: "Accept",
                        })
                        .unwrap_or(false)
                },
            );
            let ready = match decision {
                Some(ModelDownloadClickDecision::Ready {
                    entry,
                    accepted_license,
                }) => Some((entry, accepted_license)),
                Some(ModelDownloadClickDecision::BlockedByRam(message)) => {
                    eprintln!("compme: {message}");
                    None
                }
                Some(ModelDownloadClickDecision::LicenseDeclined { model }) => {
                    eprintln!("compme: download of {model} cancelled (license not accepted)");
                    None
                }
                None => None,
            };
            // Only the Ready decision runs the download body; blocked/
            // declined/empty cases log above and fall through to the loop
            // tail (event-pump + host-loop pace) like every other heartbeat
            // branch. A `continue` here would skip that mandatory
            // accept-event drain for one tick.
            if let Some((entry, accepted_license)) = ready {
                if let Some(accepted) = accepted_license {
                    // In-memory FIRST (same-session re-prompt guard), then
                    // persist; a failed write only logs — the user DID accept,
                    // so the download proceeds.
                    if let Some(path) = config::config_file_path() {
                        if let Err(err) = config::persist_setting(
                            &path,
                            "COMPME_LICENSE_ACCEPTED",
                            &accepted.value,
                        ) {
                            eprintln!("compme: failed to persist COMPME_LICENSE_ACCEPTED: {err}");
                        }
                    }
                    eprintln!(
                        "compme: {} accepted for {}",
                        accepted.license_name, accepted.model
                    );
                }
                let dest = models_dir.join(format!("{}.gguf", entry.name));
                // Skip the fetch when the model is already on disk — a
                // repeat "Download" click on a present model would otherwise
                // re-fetch and clobber a good file. An interrupted 0-byte
                // stub is NOT present, so it still re-downloads. This check
                // sits AFTER the license gate on purpose: keeping every
                // download-triggering path behind the gate is the simpler
                // invariant, and accepted licenses are remembered, so a
                // normal re-click on a present encumbered model never
                // re-prompts (the prompt-then-skip is an unaccepted-yet
                // edge case, inert for today's unencumbered catalog).
                match start_model_download_edge(ModelDownloadEdge {
                    entry,
                    dest: &dest,
                    downloader: model_downloader,
                    model_download_status: &mut download.model_download_status,
                    model_download_logged: &mut download.model_download_logged,
                    prepare: prepare_model_download_dest,
                    existing_model: model_download_dest_present,
                    spawn: || model_fetch::ModelDownloader::spawn().map_err(|err| err.to_string()),
                    request: |downloader: &model_fetch::ModelDownloader, request| {
                        downloader.request(request)
                    },
                }) {
                    DownloadStartResult::PreparedFailed(err) => {
                        eprintln!("compme: {err}");
                    }
                    DownloadStartResult::AlreadyPresent => {
                        // The model is already on disk (this build or an
                        // older one). A download Done edge will never fire,
                        // so wire it here: persist the SELECTED model's path
                        // so a re-click on a present model adopts it instead
                        // of being an inert "already present" no-op.
                        if let Some(cfg) = config::config_file_path() {
                            if let Err(err) = config::persist_setting(
                                &cfg,
                                "COMPME_MODEL_PATH",
                                &dest.to_string_lossy(),
                            ) {
                                eprintln!("compme: failed to persist COMPME_MODEL_PATH: {err}");
                            }
                        }
                        eprintln!(
                            "compme: {} already downloaded at {} \u{2014} COMPME_MODEL_PATH set, relaunch to use",
                            entry.name,
                            dest.display()
                        )
                    }
                    DownloadStartResult::SpawnFailed(err) => {
                        eprintln!("compme: failed to start model downloader \u{2014} {err}");
                    }
                    DownloadStartResult::Queued => eprintln!(
                        "compme: downloading {} ({} MB) \u{2014} progress in this log",
                        entry.name, entry.size_mb
                    ),
                    DownloadStartResult::Busy => {
                        eprintln!("compme: model download queue busy \u{2014} try again");
                    }
                }
            }
        } else {
            // The click was already consumed by the swap above; without a
            // resolvable config home there is no app-support model directory.
            eprintln!("compme: download-model click ignored \u{2014} config home is unavailable");
        }
    }
    // Download progress/terminal-state logging (one line per transition).
    if let Some(status) = &download.model_download_status {
        let state = status.state.lock().unwrap_or_else(|e| e.into_inner());
        let (next_logged, line) = download_log_transition(&state, download.model_download_logged);
        // The Done edge (logged advances to 2 with a Done state) fires once
        // per download — start_model_download_edge resets logged to 0 on
        // each new queue — so a second download re-persists its own path.
        let done_edge = next_logged != download.model_download_logged
            && matches!(&*state, model_fetch::DownloadState::Done(_));
        download.model_download_logged = next_logged;
        if let Some(line) = line {
            eprintln!("{line}");
        }
        // Auto-wire the freshly downloaded model: persist COMPME_MODEL_PATH
        // so the next launch loads it (env > file > default). Without this a
        // completed download is unusable — the Setup "Model file" row stays
        // ✗ forever and a Finder-launched .app has no way to point at the
        // file (env vars aren't set for GUI launches). Persist failure only
        // logs; the file is still on disk for a manual override.
        if done_edge {
            if let model_fetch::DownloadState::Done(path) = &*state {
                if let Some(cfg) = config::config_file_path() {
                    match config::persist_setting(
                        &cfg,
                        "COMPME_MODEL_PATH",
                        &path.to_string_lossy(),
                    ) {
                        Ok(()) => eprintln!(
                            "compme: COMPME_MODEL_PATH set to {} \u{2014} relaunch to use it",
                            path.display()
                        ),
                        Err(err) => {
                            eprintln!("compme: failed to persist COMPME_MODEL_PATH: {err}")
                        }
                    }
                }
            }
        }
    }
}

/// Heartbeat phase: drain and apply queued `compme://` deep links.
/// Split out of `run()` verbatim (2026-07-25 audit, F16); every branch,
/// log line, and ordering is unchanged.
fn drain_deep_links_phase<A: PlatformAdapter, O: OverlayPresenter>(
    deep_links: &Mutex<Vec<String>>,
    shell: &Arc<dyn ShellHost>,
    config: &Config,
    prefs: &mut Prefs,
    monitored: &mut MonitoredInput,
    suggestion: &mut SuggestionState,
    engine: &mut Engine<SharedAdapter<A>, O>,
) {
    // Drain received compme:// deep links (strict fail-closed parse →
    // reversible override). Every outcome is logged (the §16 user-visible
    // requirement; a confirmation prompt is the follow-up). An applied
    // override changes suggestion policy, so fire the dismiss edge
    // (a2-parity review #2) and persist every round-trippable web-config
    // policy field.
    let pending_links: Vec<String> = {
        let mut lock = deep_links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *lock)
    };
    for url in pending_links {
        let confirm = |decision: &webconfig::PromptDecision| -> bool {
            let webconfig::PromptDecision {
                scope,
                action,
                trust,
            } = decision;
            shell
                .confirm(&shell_flags::ConfirmPrompt {
                    title: "Allow configuration change?",
                    message: &format!(
                        "A compme:// link wants to apply {action} for:\n{scope}\n({trust})"
                    ),
                    confirm_label: "Allow",
                })
                .unwrap_or(false)
        };
        match handle_deep_link(&url, config.trusted_key.as_ref(), prefs, confirm) {
            Ok(summary) => {
                eprintln!("compme: deep link {summary}");
                clear_monitored_state_for_policy_transition(
                    &mut monitored.pending_monitored,
                    &mut monitored.monitored_buffers,
                );
                if let Some(path) = config::config_file_path() {
                    persist_web_override_prefs(&path, prefs);
                }
                suggestion.latest.clear();
                let _ = log_err("on_dismiss", engine.on_dismiss());
            }
            Err(err) => eprintln!("compme: deep link rejected: {err}"),
        }
    }
}

/// Heartbeat phase: the Setup pane's privileged button edges — grant
/// Accessibility, request Screen Recording, Show Models Folder, and the
/// bring-your-own-model path. Split out of `run()` verbatim (F16).
fn setup_pane_actions_phase(
    settings_flags: &crate::shell::SettingsFlags,
    shell: &Arc<dyn ShellHost>,
    config: &Config,
) {
    // Setup buttons (tray-flags pattern): consume edges, perform the
    // privileged calls here on the main thread.
    if settings_flags.setup_grant_ax.swap(false, Ordering::Relaxed) {
        shell.prompt_accessibility_trust();
    }
    if settings_flags
        .setup_request_screen
        .swap(false, Ordering::Relaxed)
    {
        if should_request_screen_recording(config.screen_context, shell.screen_capture_permission())
        {
            shell.request_screen_capture_permission();
        } else {
            eprintln!("compme: screen recording request ignored; screen context is off or already granted");
        }
    }
    // "Show Models Folder": open the app-support models dir in Finder
    // (created first so it opens even before the first download).
    if settings_flags
        .setup_reveal_models_dir
        .swap(false, Ordering::Relaxed)
    {
        match app_support_models_dir() {
            Some(dir) => {
                if let Err(err) = show_models_folder_with(
                    &dir,
                    |path| std::fs::create_dir_all(path),
                    |path| {
                        shell
                            .reveal_file(path)
                            .map_err(|err| std::io::Error::other(format!("{err:?}")))
                    },
                ) {
                    eprintln!("compme: open models folder failed: {err}");
                }
            }
            None => eprintln!("compme: cannot resolve models folder (no config home)"),
        }
    }
    // Bring-your-own-model: a path picked via the file panel. Validate it is
    // a readable GGUF, then point COMPME_MODEL_PATH at it in place (no copy);
    // the model loads on the next launch (same as adopt/download).
    let chosen_model = settings_flags
        .setup_choose_model
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(path) = chosen_model {
        match validate_gguf_model(&path) {
            Ok(()) => {
                if let Some(cfg) = config::config_file_path() {
                    match config::persist_setting(
                        &cfg,
                        "COMPME_MODEL_PATH",
                        &path.to_string_lossy(),
                    ) {
                        Ok(()) => eprintln!(
                            "compme: using model {} \u{2014} relaunch to load",
                            path.display()
                        ),
                        Err(err) => {
                            eprintln!("compme: failed to persist COMPME_MODEL_PATH: {err}")
                        }
                    }
                }
            }
            Err(reason) => eprintln!("compme: chosen model rejected \u{2014} {reason}"),
        }
    }
}

/// Heartbeat phase: the Apps pane's Delete-row edge (confirm, secure
/// delete, recompose, re-render). Split out of `run()` verbatim (F16).
fn apps_row_delete_phase(
    settings_flags: &crate::shell::SettingsFlags,
    shell: &Arc<dyn ShellHost>,
    memory: &Option<memory::MemoryStore>,
    settings: &mut SettingsState,
    prefs: &Prefs,
    config: &Config,
    settings_window: &crate::shell::SettingsWindow,
) {
    // Apps-row Delete: resolve the clicked row index against the ids
    // rendered with the SAME cap/order, delete, recompose, re-render.
    let clicked_row = settings_flags
        .apps_delete_row
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(row) = clicked_row {
        if let (Some(store), Some(app)) = (&memory, settings.apps_ids.get(row)) {
            // Irreversible (secure_delete zeroes freed pages) — confirm
            // first, Cancel-default (review-c112; deep-link precedent).
            let confirmed = shell
                .confirm(&shell_flags::ConfirmPrompt {
                    title: "Delete recorded inputs?",
                    message: &format!("All recorded inputs for {app} will be permanently erased."),
                    confirm_label: "Delete",
                })
                .unwrap_or(false);
            if !confirmed {
                eprintln!("compme: delete for {app} cancelled");
            } else if let Some((lines, ids)) =
                delete_app_row_and_recompose(store, &settings.apps_ids, row)
            {
                // Poison-recovery: skipping would leave the Apps pane
                // showing the just-deleted row (refresh runs below).
                *settings_flags
                    .apps_lines
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = lines;
                settings.apps_ids = ids;
                // Rows shifted — republish the policy bits in the new order
                // before refresh_apps_labels re-seeds the checkboxes from them.
                *settings_flags
                    .apps_policy_bits
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = compose_apps_policy_bits(
                    prefs,
                    &settings.apps_ids,
                    settings.global_mid_word,
                    config.autocorrect,
                    config.grammar_fix,
                );
                settings_window.refresh_apps_labels();
            }
        }
    }
}

/// Heartbeat phase: the Apps pane's per-app policy checkbox edge, including
/// the focused-app dismiss. Split out of `run()` verbatim (F16).
fn apps_row_policy_edit_phase<A: PlatformAdapter, O: OverlayPresenter>(
    settings_flags: &crate::shell::SettingsFlags,
    settings: &SettingsState,
    prefs: &mut Prefs,
    focus: &FocusContext,
    shell: &Arc<dyn ShellHost>,
    suggestion: &mut SuggestionState,
    engine: &mut Engine<SharedAdapter<A>, O>,
) {
    // Apps-row policy checkbox: resolve the clicked row against the SAME
    // ids/cap/order as Delete, map the field index to an AppPolicyField,
    // write the per-app override into the live prefs, and persist (the
    // web-override persist path serializes every per_app field). No
    // apps_lines recompose — the edit changes policy, not recorded-input
    // counts.
    let edit = settings_flags
        .apps_edit
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some((row, field_index, on)) = edit {
        if let (Some(app), Some(field)) = (
            settings.apps_ids.get(row),
            apps_policy_field_from_index(field_index),
        ) {
            prefs.set_app_policy_field(app, field, on);
            eprintln!("compme: app policy {field:?} for {app} set to {on}");
            if let Some(path) = config::config_file_path() {
                persist_web_override_prefs(&path, prefs);
            }
            // Disabling the FOCUSED app must retract any suggestion already on
            // screen (and disarm its accept key); the submit gate only blocks
            // future submits, not an already-dispatched render. Mirrors the
            // shortcut/snooze/global-disable edges. Gated on the edited app
            // being the focused one so editing another app's row never
            // dismisses the focused field's ghost.
            let focused_app = focus
                .current_field
                .as_ref()
                .and_then(|f| effective_app_key(f, |pid| shell.bundle_id_for_pid(pid)));
            if apps_edit_dismisses_focused(field, on, focused_app.as_deref(), app) {
                suggestion.latest.clear();
                let _ = log_err("on_dismiss", engine.on_dismiss());
            }
        }
    }
}

/// Heartbeat phase: Personalization-pane knob edits — live `set_profile`,
/// persist, and the flag mirrors. Split out of `run()` verbatim (F16).
fn personalization_edits_phase(
    settings_window: &crate::shell::SettingsWindow,
    settings_flags: &crate::shell::SettingsFlags,
    config: &mut Config,
    inference: &InferenceHandle,
) {
    // Personalization pane edit: apply the recorded knob change to the
    // source profile (so it survives restart via persist) AND push it to the
    // running worker LIVE via set_profile — no respawn, takes effect on the
    // next request. The seam carried a primitive; apply_personalization_edit
    // rejoins it to the typed profile and returns the (key, value) to persist.
    //
    // NOTE: the three knobs here (global instructions, sender, strength)
    // govern PROMPT STEERING only. The MemoryStore open/close lifecycle is
    // NOT part of PersonalizationProfile — it is governed by the separate
    // `config.memory.mode` (memory::StorageMode), opened once at startup
    // above (`open_memory_store`). So there is no MemoryStore call to make
    // from a profile edit.
    // TODO(LOOK): if a future "remember my edits" mode is added to
    // PersonalizationProfile that should gate the MemoryStore, wire it to
    // open_memory_store / store.close() here; today the profile has no such
    // knob, so the steering edit must not touch `memory`.
    settings_window.flush_personalization_edits();
    let pers_edits = settings_flags
        .personalization_edit
        .lock()
        .map(|mut slot| std::mem::take(&mut *slot))
        .unwrap_or_else(|poisoned| std::mem::take(&mut *poisoned.into_inner()));
    for edit in pers_edits {
        let edit_for_flags = edit.clone();
        let (key, _value, persist_result) = apply_live_personalization_edit(
            &mut config.personalization,
            edit,
            |profile| inference.set_profile(profile),
            |key, value| {
                if let Some(path) = config::config_file_path() {
                    config::persist_setting(&path, key, value)
                } else {
                    Ok(())
                }
            },
        );
        eprintln!("compme: personalization {key} updated");
        if let Err(err) = persist_result {
            eprintln!("compme: failed to persist {key}: {err}");
        }
        use crate::shell::PersonalizationEdit as E;
        match edit_for_flags {
            E::GlobalInstructions(_) => {
                *settings_flags
                    .personalization_instructions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) =
                    config.personalization.global_instructions.clone();
            }
            E::SenderName(_) => {
                *settings_flags
                    .personalization_sender_name
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) =
                    config.personalization.sender.name.clone();
            }
            E::SenderEmail(_) => {
                *settings_flags
                    .personalization_sender_email
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) =
                    config.personalization.sender.email.clone();
            }
            E::StrengthStop(_) => {
                settings_flags.personalization_strength_index.store(
                    personalization_strength_index(config.personalization.strength),
                    Ordering::Relaxed,
                );
            }
        }
    }
}

/// Heartbeat phase: the tray's per-app input-collection toggle. Split out
/// of `run()` verbatim (F16).
fn tray_collection_toggle_phase(
    flags: &TrayFlags,
    shell: &Arc<dyn ShellHost>,
    focus: &FocusContext,
    prefs: &mut Prefs,
    monitored: &mut MonitoredInput,
) {
    // Tray "Toggle Input Collection in Current App": flip the frontmost
    // app's typing-history override and persist the no-collect list. No
    // dismiss edge — collection gates RECORDING, not suggestion display.
    if flags.collection_toggle.swap(false, Ordering::Relaxed) {
        clear_monitored_state_for_policy_transition(
            &mut monitored.pending_monitored,
            &mut monitored.monitored_buffers,
        );
        match focus
            .current_field
            .as_ref()
            .and_then(|f| effective_app_key(f, |pid| shell.bundle_id_for_pid(pid)))
        {
            Some(app) => {
                let allowed = toggle_app_collection(prefs, &app);
                eprintln!(
                    "compme: input collection in {app} now {}",
                    if allowed { "ENABLED" } else { "DISABLED" }
                );
                if let Some(path) = config::config_file_path() {
                    // Mirror persist_web_override_prefs: an emptied list is
                    // REMOVED, not written as a blank `KEY=` line (which would
                    // shadow the env-over-file layer). Re-enabling the last
                    // no-collect app clears the key entirely.
                    let value = no_collect_apps_value(prefs);
                    if value.is_empty() {
                        remove_setting_or_log(&path, "COMPME_NO_COLLECT_APPS", "no-collect apps");
                    } else {
                        persist_setting_or_log(
                            &path,
                            "COMPME_NO_COLLECT_APPS",
                            &value,
                            "no-collect apps",
                        );
                    }
                }
            }
            None => {
                eprintln!("compme: collection toggle ignored — no focused app to resolve")
            }
        }
    }
}

/// Heartbeat phase: the tray's per-app disable arm (resolve the frontmost
/// app at consumption time, apply, persist Always, dismiss). Split out of
/// `run()` verbatim (F16).
#[allow(clippy::too_many_arguments)]
fn tray_app_disable_phase<A: PlatformAdapter, O: OverlayPresenter>(
    flags: &TrayFlags,
    shell: &Arc<dyn ShellHost>,
    focus: &FocusContext,
    prefs: &mut Prefs,
    monitored: &mut MonitoredInput,
    suggestion: &mut SuggestionState,
    engine: &mut Engine<SharedAdapter<A>, O>,
    now_ms: u64,
) {
    // Tray "Disable Completions in Current App" ▸ arm: resolve the CURRENT
    // frontmost app at consumption time (the tray never knows app identity)
    // and apply. Same dismiss edge as snooze/disable — the pref change must
    // retract a visible ghost (a2-parity review #2, pre-documented for
    // exactly this surface).
    if let Some(arm) = flags
        .app_disable
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        clear_monitored_state_for_policy_transition(
            &mut monitored.pending_monitored,
            &mut monitored.monitored_buffers,
        );
        match focus
            .current_field
            .as_ref()
            .and_then(|f| effective_app_key(f, |pid| shell.bundle_id_for_pid(pid)))
        {
            Some(app) => {
                apply_app_disable(arm, &app, prefs, now_ms);
                eprintln!("compme: completions disabled in {app} ({arm:?})");
                if arm == DisableArm::Always {
                    if let Some(path) = config::config_file_path() {
                        if let Err(err) = config::persist_setting(
                            &path,
                            "COMPME_EXCLUDED_APPS",
                            &excluded_apps_value(prefs),
                        ) {
                            eprintln!("compme: could not persist excluded apps: {err}");
                        }
                    }
                }
                suggestion.latest.clear();
                let _ = log_err("on_dismiss", engine.on_dismiss());
            }
            None => eprintln!("compme: disable-in-app ignored — no focused app to resolve"),
        }
    }
}

pub fn run() -> Result<(), String> {
    let Some(ctx) = startup(&real_factories())? else {
        return Ok(());
    };
    // Rebind the context under the exact names (and mutability) the loop below
    // was written against, so the heartbeat loop and teardown stay verbatim.
    let RunContext {
        instance_lock: _instance_lock,
        mut config,
        shell,
        mut trusted,
        adapter,
        mut engine,
        host_events,
        focus_sub,
        caret_sub,
        subscriptions_require_relaunch,
        model_available,
        deep_links,
        url_handler: _url_handler,
        launch_at_login_enabled,
        previous_inputs,
        memory,
        monitored_memory_active,
        clipboard_cell,
        screen_cell,
        context_bound,
        mut screen_ocr,
        screen_wait_ms,
        cross_app_previous_inputs,
        inference,
        flags,
        mut prefs,
        tray,
    } = ctx;

    let heartbeat = Duration::from_millis(config.heartbeat_ms);
    // Loop state lives in cohesive structs (crate::loop_state), one per
    // responsibility; the bindings moved verbatim and references became field
    // paths. Every field is pure data (no Drop impls anywhere in the moved
    // types), so the regrouping cannot change teardown behavior — see the
    // loop_state module doc for the teardown-order contract. The two
    // drop-observable handles (`model_downloader`, `settings_window`) stay
    // locals at their original declaration sites below.
    let mut focus = FocusContext::new();
    let mut usage_stats = UsageStats::default();
    let mut suggestion = SuggestionState::new();
    let mut monitored = MonitoredInput::default();
    let mut session_ui = SessionUi::default();
    let mut policy = PolicyState::new(config.enabled);
    // S2 settings window (lazy NSWindow) mirror state + the activation-policy
    // poll state. Settings switches write flags; the watchers below persist
    // and apply them.
    let mut settings = SettingsState::new(
        config.allow_mid_word,
        config.emoji.is_some(),
        config.emoji_prefs,
        emoji_skin_tone_index(config.emoji_prefs.skin_tone),
        emoji_gender_index(config.emoji_prefs.gender),
        launch_at_login_enabled,
    );
    let available_ram_gb = model_catalog::bytes_to_whole_gb(shell.physical_memory_bytes());
    let settings_flags = build_settings_flags(
        &config,
        Arc::clone(&flags.enabled),
        launch_at_login_enabled,
        available_ram_gb,
    );
    // One downloader per process (model_fetch contract); lazy — spawned on
    // the first Download click. Status polled per heartbeat for logging.
    let mut model_downloader: Option<model_fetch::ModelDownloader> = None;
    let mut download = DownloadState::default();
    // Lifetime stats baseline, read once.
    // stats.env is SINGLE-WRITER (this loop): every write is the immutable
    // startup baseline + grow-only session totals, so this read stays the
    // baseline for the whole run (the periodic flush and the shutdown flush
    // share it — re-reading the file would double-count the session).
    let stats_path = config::stats_file_path();
    let lifetime_base = stats::parse_stats_file(
        &stats_path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default(),
    );
    let mut settings_window = crate::shell::SettingsWindow::new(settings_flags.clone());
    let start = Instant::now();

    eprintln!(
        "compme: running (acceptance_pid={:?} stub={} run_ms={:?})",
        config.acceptance_pid,
        config.stub_completion.is_some(),
        config.run_ms
    );

    while !STOP.load(Ordering::Relaxed) {
        let now_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        // Wall-clock stamp for usage stats only (its 30-day window needs an
        // absolute clock); `now_ms` stays monotonic for latency/debounce deltas.
        let wall_ms = wall_now_ms();
        let mut manual_grammar_request: Option<CompletionRequest> = None;

        // 1. Host events → engine. The caret callback is the typing driver: read
        // context (executes on the adapter's AX worker), diff into a TextChange.
        // Drain the queue first, then collapse bursts of same-field caret reads so
        // we issue at most one AX round-trip per field per heartbeat.
        let drained = drain_host_events(&host_events);
        let host_event_backlog_remaining = drained.backlog_remaining;
        if host_event_backlog_remaining {
            suggestion.latest.clear();
        }
        for event in coalesce_caret_reads(drained.events) {
            if host_event_invalidates_pending_request(&event) {
                suggestion.latest.clear();
            }
            match event {
                HostEvent::Focus(field) => {
                    let (field, app_key) =
                        canonicalize_field_app(field, |pid| shell.bundle_id_for_pid(pid));
                    eprintln!("compme: focus {}", field.element_id);
                    clear_monitored_state_for_policy_transition(
                        &mut monitored.pending_monitored,
                        &mut monitored.monitored_buffers,
                    );
                    // Compatibility onboarding (A2 §16): surface tier-specific
                    // guidance once per app (mirror-window apps, setup-needed
                    // browsers like Google Docs/Arc).
                    // MirrorOnly apps (Firefox/Zen) render the ghost in the
                    // floating mirror window, not inline (A2 §16).
                    engine.set_mirror_mode(mirror_mode_for(app_key.as_deref()));
                    // Per-app mid-line override (App Settings): re-apply the
                    // engine's trigger gate for the newly focused app — the
                    // f8ebf33 model's deferred merge, now live.
                    engine.set_allow_mid_word(
                        prefs.mid_line_enabled(app_key.as_deref(), settings.global_mid_word),
                    );
                    // Per-app Tab disable (§16): suppress the literal-Tab
                    // hotkey for this app's NEXT arm cycle (hotkeys are
                    // transient — armed per visible suggestion).
                    crate::shell::set_tab_hotkey_suppressed(prefs.tab_disabled(app_key.as_deref()));
                    focus.last_app_key = app_key.clone();
                    // Browser-domain detection (c131, slices 2-3 of the c128
                    // design): ONE AX round-trip per browser focus; the
                    // is_browser pre-gate keeps non-browsers at zero AX
                    // traffic. Any miss/failure → None = fail-open. The full
                    // URL dies inside domain_cache_entry; only the host is
                    // kept, and only the host is ever logged (debug only).
                    focus.last_domain = if app_key.as_deref().is_some_and(compat::is_browser) {
                        let url = adapter.focused_page_url(&field).ok().flatten();
                        let entry = domain_cache_entry(app_key.as_deref(), url.as_deref());
                        if debug_enabled() {
                            match &entry {
                                Some((app, host)) => eprintln!("compme: domain={host} ({app})"),
                                None => eprintln!("compme: domain=none (browser, no URL)"),
                            }
                        }
                        // Rules read LIVE (deep links/settings mutate prefs);
                        // observe before the move into last_domain.
                        if let Some(msg) = focus
                            .domain_miss_notice
                            .observe(!prefs.excluded_domains.is_empty(), entry.is_some())
                        {
                            eprintln!("compme: {msg}");
                        }
                        entry
                    } else {
                        None
                    };
                    if let Some(app) = app_key {
                        if session_ui.hinted_apps.insert(app.clone()) {
                            log_compat_guidance(&app);
                        }
                    }
                    focus.current_field = Some(field.clone());
                    focus.tracker.reset();
                    if monitored_memory_active {
                        if let Ok(ctx) = adapter.read_context(&field) {
                            let _ = focus.tracker.observe_with_inserted_text(
                                &field,
                                &ctx,
                                TriggerPolicy::Automatic,
                                now_ms,
                            );
                        }
                    }
                    let focus_requests = log_err("on_focus", engine.on_focus(field));
                    focus.current_assistant_field = engine.assistant_field();
                    offer_all(&mut suggestion.latest, focus_requests);
                }
                HostEvent::Caret(field, _rect) => {
                    let (field, app_key) =
                        canonicalize_field_app(field, |pid| shell.bundle_id_for_pid(pid));
                    match adapter.read_context(&field) {
                        // One selection-changed notification covers both typing and a
                        // bare cursor move. Typing schedules a completion; a cursor
                        // move only invalidates a showing ghost (no re-request).
                        Ok(ctx) => {
                            session_ui.read_err_squelch.reset();
                            focus.current_field = Some(field.clone());
                            if config.diag_coords {
                                if let Ok(rect) = adapter.caret_rect(&field) {
                                    eprintln!(
                                        "compme: diag caret rect={rect:?} scales={:?}",
                                        shell.display_scales()
                                    );
                                }
                            }
                            let observation = if monitored_memory_active {
                                focus.tracker.observe_with_inserted_text(
                                    &field,
                                    &ctx,
                                    TriggerPolicy::Automatic,
                                    now_ms,
                                )
                            } else {
                                focus.tracker.observe(
                                    &field,
                                    &ctx,
                                    TriggerPolicy::Automatic,
                                    now_ms,
                                )
                            };
                            let caps = engine.current_capabilities();
                            match observation {
                                Observation::Typed(change) => {
                                    let observe_domain =
                                        domain_observation_enabled(&prefs, &config.personalization);
                                    let domain = enqueue_monitored_change_for_current_domain(
                                        &mut monitored.pending_monitored,
                                        &mut focus.last_domain,
                                        &change,
                                        app_key.clone(),
                                        observe_domain,
                                        || adapter.focused_page_url(&field).ok().flatten(),
                                    );
                                    offer_all(
                                        &mut suggestion.latest,
                                        log_err("on_text_changed", engine.on_text_changed(change)),
                                    );
                                    // Local replacement (A2 §8/§16): a typed
                                    // `:shortcode` (emoji), typo (autocorrect), or
                                    // US-only spelling (British English) offers a
                                    // replacement ghost and PREEMPTS the model
                                    // completion for this turn (Cotypist behavior —
                                    // local offers are instant + high-confidence).
                                    // `ctx.left` is the left-of-caret text. Each
                                    // feature is off by default. Honor the SAME
                                    // gating as a model completion — tray-enabled +
                                    // per-app exclude / snooze / terminal — so a
                                    // local offer never shows where a model one
                                    // wouldn't (warm-up is intentionally not required:
                                    // replacements are local and need no model).
                                    let decision = if browser_domain_fresh_enough_for_rules(
                                        app_key.as_deref(),
                                        domain.as_deref(),
                                        &prefs,
                                    ) {
                                        let app = SuggestionApp {
                                            app_key: app_key.as_deref(),
                                            assistant_field: focus.current_assistant_field,
                                        };
                                        let enabled = flags.enabled.load(Ordering::Relaxed);
                                        replacement_decision_for_field(
                                            &ctx.left,
                                            &config,
                                            &prefs,
                                            app,
                                            domain.as_deref(),
                                            enabled,
                                            now_ms,
                                        )
                                        .or_else(|| {
                                            full_autocorrect_decision(
                                                &ctx.left,
                                                &config,
                                                &prefs,
                                                FullAutocorrectGate {
                                                    app,
                                                    domain: domain.as_deref(),
                                                    enabled,
                                                    now_ms,
                                                },
                                                |word| shell.spelling_correction(word),
                                            )
                                        })
                                    } else {
                                        None
                                    };
                                    if debug_enabled() {
                                        // Diagnose emoji/typo/spelling preempt vs the
                                        // model: the left context the decision saw, the
                                        // feature toggles, and what (if anything) it
                                        // offered. `decision == None` while a model
                                        // request fires for the same text = the local
                                        // offer is not matching/gating as expected.
                                        eprintln!(
                                            "{}",
                                            replacement_debug_log_line(
                                                &ctx.left,
                                                config.emoji.is_some(),
                                                config.autocorrect,
                                                config.british_english,
                                                config.thesaurus,
                                                &format!("{decision:?}"),
                                            )
                                        );
                                    }
                                    if let Some((candidates, replace_left)) = decision {
                                        // Drop the just-queued model request so it
                                        // can't supersede the emoji ghost.
                                        suggestion.latest.clear();
                                        offer_all(
                                            &mut suggestion.latest,
                                            log_err(
                                                "on_replacement",
                                                engine.on_replacement(
                                                    &field,
                                                    candidates,
                                                    replace_left,
                                                ),
                                            ),
                                        );
                                    }
                                }
                                Observation::CaretMoved { field, caret } => {
                                    offer_all(
                                        &mut suggestion.latest,
                                        log_err(
                                            "on_caret_moved",
                                            engine.on_caret_moved(field.clone(), caret),
                                        ),
                                    );
                                }
                            }
                            let domain = cached_domain(&focus.last_domain, app_key.as_deref());
                            if let Some((original, candidates, range)) =
                                selection_thesaurus_decision(
                                    &ctx,
                                    SelectionThesaurusGate {
                                        config: &config,
                                        prefs: &prefs,
                                        app: SuggestionApp {
                                            app_key: app_key.as_deref(),
                                            assistant_field: focus.current_assistant_field,
                                        },
                                        domain,
                                        enabled: flags.enabled.load(Ordering::Relaxed),
                                        caps: &caps,
                                        now_ms,
                                    },
                                )
                            {
                                suggestion.latest.clear();
                                offer_all(
                                    &mut suggestion.latest,
                                    log_err(
                                        "on_selection_replacement",
                                        engine.on_selection_replacement(
                                            &field, original, candidates, range,
                                        ),
                                    ),
                                );
                            } else {
                                offer_all(
                                    &mut suggestion.latest,
                                    log_err(
                                        "on_selection_unavailable",
                                        engine.on_selection_unavailable(),
                                    ),
                                );
                            }
                        }
                        Err(err) => {
                            let _ =
                                log_err("on_context_unavailable", engine.on_context_unavailable());
                            // Squelched: identical failures repeat at heartbeat
                            // rate while focus sits on an unsupported element.
                            let message = format!("{err:?}");
                            if session_ui.read_err_squelch.should_log(&message) {
                                eprintln!("compme: read_context: {message}");
                            }
                            // Setup-needed onboarding (A2 §16): a browser/Arc/Dia field
                            // that won't read may need Accessibility/Text-Metrics setup
                            // (the Google-Docs-in-Chrome case). Surface guidance once.
                            if let Some(app) =
                                resolve_app_key(field.pid, |pid| shell.bundle_id_for_pid(pid))
                            {
                                if compat::needs_accessibility_setup(&app, false)
                                    && session_ui.hinted_apps.insert(format!("setup:{app}"))
                                {
                                    eprintln!(
                                        "compme: {app} field not readable — may need \
                                     Accessibility/Text-Metrics setup (e.g. Google Docs)"
                                    );
                                }
                            }
                        }
                    }
                }
                HostEvent::Accept(action) => {
                    debug_assert_eq!(
                        host_event_route(&HostEvent::Accept(action)),
                        if matches!(action, AcceptAction::Correction) {
                            HostEventRoute::AcceptCorrection
                        } else {
                            HostEventRoute::Normal
                        }
                    );
                    eprintln!("compme: accept {action:?}");
                    // Preview the engine's accept payload once and reuse it for
                    // both the Word self-insert and the Full context record, so
                    // the two never read divergent engine snapshots.
                    let preview = engine.preview_accept_insert(action);
                    let correction_preview = engine.preview_accept_correction();
                    let range_preview = engine.preview_accept_range(action);
                    let accept_result = engine.on_accept(action);
                    // The platform field mutation precedes overlay/tap teardown.
                    // A teardown error is still surfaced, but an already-committed
                    // mutation must absorb its AX echo and update acceptance sinks
                    // exactly once. A pre-insert adapter error remains unaccepted.
                    let committed = accept_mutation_committed(&accept_result);
                    apply_accept_side_effects(
                        committed,
                        AcceptSideEffects {
                            action,
                            preview: preview.as_ref(),
                            correction_preview: correction_preview.as_ref(),
                            range_preview: range_preview.as_ref(),
                            wall_ms,
                            context_max_chars: if config.context_max_chars > 0
                                || config.cross_app_previous_inputs
                            {
                                context_bound
                            } else {
                                0
                            },
                            cross_app_previous_inputs: config.cross_app_previous_inputs,
                            previous_inputs: &previous_inputs,
                            memory: memory.as_ref(),
                            prefs: &prefs,
                            tracker: &mut focus.tracker,
                            usage: &mut usage_stats.usage,
                        },
                    );
                    match accept_result {
                        Ok(requests) => offer_all(&mut suggestion.latest, requests),
                        Err(err) => {
                            eprintln!(
                                "compme: on_accept error after committed={}: {:?}",
                                err.committed, err.error
                            );
                        }
                    }
                }
                HostEvent::Dismiss => {
                    eprintln!("compme: dismiss (Esc)");
                    usage_stats.usage.record(wall_ms, stats::Outcome::Dismissed);
                    offer_all(
                        &mut suggestion.latest,
                        log_err("on_dismiss_suppress", engine.on_dismiss_suppress()),
                    );
                }
                HostEvent::Cycle => {
                    eprintln!("compme: cycle candidate");
                    offer_all(
                        &mut suggestion.latest,
                        log_err("on_cycle", engine.on_cycle()),
                    );
                }
                HostEvent::Shortcut(action) => match action {
                    ShortcutAction::ForceActivate => {
                        // Settled semantics: re-show the CURRENT pending suggestion
                        // without kicking a fresh inference. `on_force_show`
                        // re-emits the held candidate verbatim (no rotation, no
                        // RequestCompletion); a no-op when nothing is held.
                        eprintln!("compme: shortcut force-activate (re-show pending)");
                        offer_all(
                            &mut suggestion.latest,
                            log_err("on_force_show", engine.on_force_show()),
                        );
                    }
                    ShortcutAction::ToggleApp => {
                        // Flip per-app Enabled for the focused app, mirroring the
                        // tray/settings per-app toggle. The focused app key comes
                        // from the same resolver the app-disable path uses.
                        match focus
                            .current_field
                            .as_ref()
                            .and_then(|f| effective_app_key(f, |pid| shell.bundle_id_for_pid(pid)))
                        {
                            Some(app) => {
                                // Invert the per-app `enabled` baseline (override if
                                // present, else `default_enabled`) — NOT
                                // `should_suggest`, which folds in snooze / app-snooze
                                // / `excluded_apps` that outrank `enabled`. See
                                // `app_enabled_baseline` for why inverting the gated
                                // value would never converge.
                                let current = app_enabled_baseline(&prefs, &app);
                                prefs.set_app_policy_field(
                                    &app,
                                    prefs::AppPolicyField::Enabled,
                                    !current,
                                );
                                eprintln!(
                                    "compme: shortcut toggle-app {app} enabled {current} -> {}",
                                    !current
                                );
                                if let Some(path) = config::config_file_path() {
                                    persist_web_override_prefs(&path, &prefs);
                                }
                                // Disabling must retract any suggestion already on
                                // screen (and disarm its accept key); the gate is only
                                // re-checked at submission, so a visible ghost would
                                // otherwise still insert. Mirrors the snooze /
                                // tray-disable paths below.
                                if toggle_app_dismisses(current) {
                                    suggestion.latest.clear();
                                    let _ = log_err("on_dismiss", engine.on_dismiss());
                                }
                            }
                            // No resolvable focused app (no field / unknown bundle):
                            // nothing to toggle.
                            None => eprintln!("compme: shortcut toggle-app: no focused app"),
                        }
                    }
                    ShortcutAction::ToggleGlobal => {
                        // Invert the runtime global-enabled flag, mirroring the
                        // SIGUSR1 / tray enable-disable below, including the
                        // monitored-state reset on the policy transition.
                        let now = flags.enabled.load(Ordering::Relaxed);
                        flags.enabled.store(!now, Ordering::Relaxed);
                        clear_monitored_state_for_policy_transition(
                            &mut monitored.pending_monitored,
                            &mut monitored.monitored_buffers,
                        );
                        // Disabling must retract any visible suggestion (and disarm
                        // its accept key); the enabled gate is only re-checked at
                        // submission. Mirrors the snooze / tray global-disable paths.
                        if now {
                            suggestion.latest.clear();
                            let _ = log_err("on_dismiss", engine.on_dismiss());
                        }
                        eprintln!("compme: shortcut toggle-global enabled {now} -> {}", !now);
                    }
                    ShortcutAction::GrammarCheck => {
                        debug_assert_eq!(
                            host_event_route(&HostEvent::Shortcut(ShortcutAction::GrammarCheck)),
                            HostEventRoute::ManualGrammarDetection
                        );
                        let Some(field) = focus.current_field.clone() else {
                            eprintln!("compme: shortcut grammar-check: no focused field");
                            continue;
                        };
                        let outcome = handle_grammar_check_shortcut(GrammarCheckShortcutArgs {
                            current_field: Some(field),
                            config: &config,
                            prefs: &prefs,
                            enabled: flags.enabled.load(Ordering::Relaxed),
                            now_ms,
                            last_domain: &mut focus.last_domain,
                            resolve_app_key: |field| {
                                effective_app_key(&field, |pid| shell.bundle_id_for_pid(pid))
                            },
                            focused_page_url: |field| {
                                adapter.focused_page_url(&field).ok().flatten()
                            },
                            read_context: |field| adapter.read_context(&field),
                            capabilities: |field| adapter.capabilities(&field),
                            arm_manual_grammar_request: |field| {
                                engine.arm_manual_grammar_request(&field)
                            },
                        });
                        apply_grammar_shortcut_pending_effect(
                            &mut suggestion.latest,
                            &mut manual_grammar_request,
                            &outcome,
                        );
                        match outcome {
                            GrammarCheckShortcutOutcome::NoField => {
                                eprintln!("compme: shortcut grammar-check: no focused field");
                            }
                            GrammarCheckShortcutOutcome::BlockedBeforeRead => {
                                eprintln!(
                                    "compme: shortcut grammar-check blocked before text read"
                                );
                            }
                            GrammarCheckShortcutOutcome::ReadContextError(err) => {
                                eprintln!("compme: grammar-check read_context error: {err:?}");
                            }
                            GrammarCheckShortcutOutcome::CapabilitiesError(err) => {
                                eprintln!("compme: grammar-check capabilities error: {err:?}");
                            }
                            GrammarCheckShortcutOutcome::BlockedAfterRead => {
                                eprintln!("compme: shortcut grammar-check blocked");
                            }
                            GrammarCheckShortcutOutcome::NotArmed => {
                                eprintln!("compme: shortcut grammar-check not armed");
                            }
                            GrammarCheckShortcutOutcome::Armed(request) => {
                                debug_assert!(matches!(
                                    manual_grammar_request.as_ref(),
                                    Some(armed) if armed.generation == request.generation
                                ));
                            }
                        }
                    }
                },
            }
        }

        // 2. Inference outcomes → engine (stale ones are discarded internally).
        if !host_event_backlog_remaining {
            for outcome in inference.drain_outcomes() {
                if matches!(outcome.request.kind, RequestKind::GrammarFix { .. }) {
                    if let Some(latency) = latency_sample(
                        &mut suggestion.submit_times,
                        outcome.request.generation,
                        now_ms,
                    ) {
                        usage_stats.usage.record_latency(wall_ms, latency);
                    }
                    match (outcome.correction, outcome.correction_range) {
                        (Some(correction), Some(correction_range)) => {
                            offer_all(
                                &mut suggestion.latest,
                                log_err(
                                    "on_correction",
                                    engine.on_correction(
                                        &outcome.request,
                                        correction,
                                        correction_range,
                                    ),
                                ),
                            );
                            eprintln!(
                                "compme: grammar outcome gen={} correction_present=true",
                                outcome.request.generation
                            );
                        }
                        _ => {
                            offer_all(
                                &mut suggestion.latest,
                                log_err(
                                    "on_correction_absent",
                                    engine.on_correction_absent(&outcome.request),
                                ),
                            );
                            eprintln!(
                                "compme: grammar outcome gen={} correction_present=false",
                                outcome.request.generation
                            );
                        }
                    }
                    continue;
                }
                eprintln!(
                    "{}",
                    completion_outcome_log_line(outcome.request.generation, &outcome.candidates)
                );
                // First-suggestion latency for this completed request (§11).
                if let Some(latency) = latency_sample(
                    &mut suggestion.submit_times,
                    outcome.request.generation,
                    now_ms,
                ) {
                    usage_stats.usage.record_latency(wall_ms, latency);
                }
                offer_all(
                    &mut suggestion.latest,
                    log_err(
                        "on_completion",
                        engine.on_completion_multi(&outcome.request, outcome.candidates),
                    ),
                );
            }
        }

        // 3. Debounce tick.
        offer_all(
            &mut suggestion.latest,
            log_err("on_tick", engine.on_tick(now_ms)),
        );

        // 3b. Drain engine-internal Shown/Superseded events into usage stats
        // (§11/§16): the engine surfaces these; Accepted/Dismissed are recorded
        // from the host inputs above.
        for event in engine.take_stat_events() {
            usage_stats.usage.record(wall_ms, stat_outcome(event));
        }

        // 4. Derive status (permission/secure/ready/enabled) and update the tray.
        // Re-poll secure input and trust on a wall-clock throttle so granting
        // permission or a password field appearing is reflected without a restart.
        if policy
            .last_secure_poll_ms
            .is_none_or(|last| now_ms.saturating_sub(last) >= SECURE_POLL_INTERVAL_MS)
        {
            policy.secure = shell.secure_input_enabled();
            trusted = shell.accessibility_trusted();
            policy.last_secure_poll_ms = Some(now_ms);
        }
        // SIGUSR1 toggles enable/disable (headless equivalent of the tray item).
        if TOGGLE.swap(false, Ordering::Relaxed) {
            let now = flags.enabled.load(Ordering::Relaxed);
            flags.enabled.store(!now, Ordering::Relaxed);
            clear_monitored_state_for_policy_transition(
                &mut monitored.pending_monitored,
                &mut monitored.monitored_buffers,
            );
            // Disabling must retract any visible suggestion (and disarm its accept
            // key); the enabled gate is only re-checked at submission.
            if now {
                suggestion.latest.clear();
                let _ = log_err("on_dismiss", engine.on_dismiss());
            }
        }
        // Tray "Disable Completions Globally ▸": Hour/UntilRelaunch snooze
        // globally (UntilRelaunch holds for the process life); Always flips
        // the shared enabled atomic — its edge persists + dismisses.
        let global_arm = flags
            .global_disable
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(arm) = global_arm {
            clear_monitored_state_for_policy_transition(
                &mut monitored.pending_monitored,
                &mut monitored.monitored_buffers,
            );
            if apply_global_disable(arm, &mut prefs, now_ms) {
                flags.enabled.store(false, Ordering::Relaxed);
                eprintln!("compme: completions disabled (persistent)");
            } else {
                eprintln!("compme: completions snoozed globally ({arm:?})");
                suggestion.latest.clear();
                let _ = log_err("on_dismiss", engine.on_dismiss());
            }
        }
        // Tray "Snooze for 1 hour": pause suggestions on the monotonic clock
        // (a relaunch deliberately clears it). Consumed with swap so one click
        // is one snooze.
        if apply_snooze_request(
            flags.snooze_requested.swap(false, Ordering::Relaxed),
            &mut prefs,
            now_ms,
        ) {
            eprintln!("compme: suggestions snoozed for {SNOOZE_MINUTES} minutes");
            clear_monitored_state_for_policy_transition(
                &mut monitored.pending_monitored,
                &mut monitored.monitored_buffers,
            );
            // A snooze must retract an already-visible ghost, exactly like the
            // disable edge below: gating runs at request-submission, so without
            // this a ghost already on screen would survive the snooze — and its
            // armed accept key would still insert it (a2-parity review #2).
            suggestion.latest.clear();
            let _ = log_err("on_dismiss", engine.on_dismiss());
        }
        // Tray "Settings…": show the S2 window (promotes activation policy so
        // a menu-bar app's window can become key).
        if flags.open_settings_window.swap(false, Ordering::Relaxed) {
            // Compose the Statistics rows right before showing — the window
            // renders strings only; data stays on this side of the seam.
            {
                // Poison-recovery: silently skipping would leave the Statistics
                // pane stale (subsystem disabled), diverging from the recovery
                // policy used elsewhere; recover the buffer instead.
                let mut lines = settings_flags
                    .stats_lines
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                // Span + bucketing chosen by the Statistics range/group pickers
                // (defaults: 7 days, Daily → identity bucketing).
                *lines = compose_stats_lines(
                    &usage_stats.usage,
                    wall_ms,
                    settings_flags.stat_range_index.load(Ordering::Relaxed),
                    settings_flags.stat_group_index.load(Ordering::Relaxed),
                );
                // Grow-only session totals, NOT window-derived counts: past
                // 30 days the window prunes and the row would regress — and
                // it must agree with what the periodic flush writes to disk.
                let totals = usage_stats.usage.session_totals();
                lines.push(lifetime_line(
                    &lifetime_base.merged(totals.counts, totals.words),
                ));
            }
            // Setup tab: re-probe permissions/model at every open (cheap
            // queries; the visible-only poll below covers stays-open).
            // Poison-recovery so a poisoned lock cannot silently disable the
            // Setup pane refresh (uniform with the recovery policy elsewhere).
            *settings_flags
                .setup_lines
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = compose_setup_lines(
                &config,
                model_available,
                subscriptions_require_relaunch,
                shell.accessibility_trusted(),
                shell.screen_capture_permission(),
                download.model_download_status.as_deref(),
            );
            settings.last_setup_poll_ms = Some(now_ms);
            // Apps tab: per-app counts straight from the store (plaintext
            // GROUP BY, no decryption). Unlike setup_lines these are
            // show-time snapshots, same stance as stats_lines (c99): cheap
            // probes refresh live, data aggregations refresh per open.
            {
                // Poison-recovery: skipping would leave the Apps pane stale.
                let mut lines = settings_flags
                    .apps_lines
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                (*lines, settings.apps_ids) = compose_apps_rows(memory.as_ref());
            }
            // Publish the per-row policy bits alongside apps_lines (same order/
            // cap) so the Apps-pane checkboxes open reflecting the saved per-app
            // override instead of a hard-seeded OFF.
            *settings_flags
                .apps_policy_bits
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = compose_apps_policy_bits(
                &prefs,
                &settings.apps_ids,
                settings.global_mid_word,
                config.autocorrect,
                config.grammar_fix,
            );
            if let Err(err) = settings_window.show() {
                eprintln!("compme: settings window unavailable: {err}");
            }
        }
        // Visibility poll: however the window closed (red button included),
        // demote the activation policy back to Accessory exactly once on the
        // visible→hidden edge so no Dock icon is left stranded.
        let settings_visible = settings_window.is_visible();
        if crate::shell::policy_restore_needed(settings.settings_was_visible, settings_visible) {
            if let Err(err) = settings_window.restore_accessory_policy() {
                eprintln!("compme: activation policy restore failed: {err}");
            }
        }
        settings.settings_was_visible = settings_visible;
        setup_pane_actions_phase(&settings_flags, &shell, &config);
        // Live accept-key rebind (recorder 5b slice 3): the recorder UI (or
        // a debug trigger — slice 4 supplies the producer) parks the request;
        // consume the edge here. Sequencing inside apply_live_accept_keymap:
        // keymap write FIRST, re-arm SECOND, persist ONLY after success.
        let rebind_request = settings_flags
            .shortcuts_rebind_request
            .lock()
            .map(|mut slot| slot.take())
            .unwrap_or_else(|poisoned| poisoned.into_inner().take());
        if let Some((word, full, grammar_accept)) = rebind_request {
            let outcome = apply_live_accept_keymap(
                word,
                full,
                grammar_accept,
                |word, full, grammar_accept| {
                    crate::shell::set_accept_keymap_from_config_with_mods(
                        word,
                        full,
                        grammar_accept,
                    )
                },
                || engine.rearm_accept_keys(),
                |w: (i64, u32), f: (i64, u32), g: Option<(i64, u32)>| {
                    if let Some(path) = config::config_file_path() {
                        // Persist with format_accept_key so a configured mask
                        // round-trips ("shift+48") through parse_accept_key at
                        // relaunch instead of being written back as a bare code.
                        for (key, value) in
                            [("COMPME_ACCEPT_WORD_KEY", w), ("COMPME_ACCEPT_FULL_KEY", f)]
                        {
                            let serialized = crate::shell::format_accept_key(value.0, value.1);
                            if let Err(err) = config::persist_setting(&path, key, &serialized) {
                                eprintln!("compme: failed to persist {key}: {err}");
                            }
                        }
                        match g {
                            Some(value) => {
                                let serialized = crate::shell::format_accept_key(value.0, value.1);
                                if let Err(err) = config::persist_setting(
                                    &path,
                                    "COMPME_GRAMMAR_ACCEPT_KEY",
                                    &serialized,
                                ) {
                                    eprintln!(
                                        "compme: failed to persist COMPME_GRAMMAR_ACCEPT_KEY: {err}"
                                    );
                                }
                            }
                            None => {
                                remove_setting_or_log(
                                    &path,
                                    "COMPME_GRAMMAR_ACCEPT_KEY",
                                    "grammar accept key",
                                );
                            }
                        }
                    } else {
                        // The rebind is LIVE but evaporates at relaunch — say
                        // so instead of letting the success log imply it
                        // persisted (review-c133).
                        eprintln!(
                            "compme: no config dir \u{2014} rebound keys apply this session only"
                        );
                    }
                },
                crate::shell::effective_accept_keys_with_mods_and_grammar,
            );
            match outcome {
                Ok(()) => {
                    let (word, full, grammar_accept) =
                        crate::shell::effective_accept_keys_with_mods_and_grammar();
                    // Recompose the Shortcuts text; show() re-reads it on the
                    // next open (refresh-on-show — the c121 forward trap).
                    if let Ok(mut text) = settings_flags.shortcuts_text.lock() {
                        *text = shortcuts_text(word, full, grammar_accept);
                    }
                    // The slice-4 recorder lives INSIDE the window, so it is
                    // open at exactly this moment — refresh the live label
                    // (show() only covers the reopen edge) (review-c133).
                    settings_window.refresh_shortcuts_label();
                    eprintln!(
                        "compme: accept keys rebound (word={word:?} full={full:?} grammar_accept={grammar_accept:?})"
                    );
                }
                Err(err) => eprintln!("compme: accept-key rebind failed: {err}"),
            }
        }
        apps_row_delete_phase(
            &settings_flags,
            &shell,
            &memory,
            &mut settings,
            &prefs,
            &config,
            &settings_window,
        );
        apps_row_policy_edit_phase(
            &settings_flags,
            &settings,
            &mut prefs,
            &focus,
            &shell,
            &mut suggestion,
            &mut engine,
        );
        personalization_edits_phase(&settings_window, &settings_flags, &mut config, &inference);
        model_download_phase(
            &settings_flags,
            &shell,
            &mut config,
            available_ram_gb,
            &mut model_downloader,
            &mut download,
        );
        // Periodic lifetime-stats flush (c102): baseline + grow-only session
        // totals, idempotent overwrite. The dirty check keeps idle ticks off
        // the disk; on a failed write the timestamp still advances (no
        // per-heartbeat hammering of a broken disk) but the dirty marker
        // does not, so the next interval retries.
        let session_totals = usage_stats.usage.session_totals();
        if stats_flush_due(usage_stats.last_stats_flush_ms, now_ms)
            && session_totals != usage_stats.last_flushed_session
        {
            usage_stats.last_stats_flush_ms = Some(now_ms);
            match persist_lifetime_stats(stats_path.as_deref(), &lifetime_base, session_totals) {
                Ok(()) => usage_stats.last_flushed_session = session_totals,
                Err(err) => eprintln!("compme: stats persist failed: {err}"),
            }
        }
        // Visible-only Setup re-probe: granting a permission while the
        // window stays open flips its row within ~480ms.
        if setup_poll_due(settings_visible, settings.last_setup_poll_ms, now_ms) {
            settings.last_setup_poll_ms = Some(now_ms);
            // Poison-recovery so a poisoned lock cannot silently disable the
            // visible Setup re-probe (uniform with the recovery policy).
            *settings_flags
                .setup_lines
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = compose_setup_lines(
                &config,
                model_available,
                subscriptions_require_relaunch,
                shell.accessibility_trusted(),
                shell.screen_capture_permission(),
                download.model_download_status.as_deref(),
            );
            settings_window.refresh_setup_labels();
        }
        // General-tab Autocorrect watcher: persist + apply on the edge. The
        // decision path reads config.autocorrect per offer, so a field write
        // IS the live apply (per-app overrides still win).
        let _ = apply_autocorrect_settings_edge(
            &settings_flags.general_autocorrect,
            &mut config.autocorrect,
            |on| persist_and_log_switch("COMPME_AUTOCORRECT", "autocorrect", on),
            |on| {
                if !on {
                    suggestion.latest.clear();
                    let _ = log_err("on_dismiss", engine.on_dismiss());
                }
            },
        );
        // Full autocorrect is a separate OS-backed spelling feature. It shares
        // the per-app Autocorrect override but has its own global switch.
        let _ = apply_autocorrect_settings_edge(
            &settings_flags.general_full_autocorrect,
            &mut config.full_autocorrect,
            |on| persist_and_log_switch("COMPME_FULL_AUTOCORRECT", "full autocorrect", on),
            |on| {
                if !on {
                    suggestion.latest.clear();
                    let _ = log_err("on_dismiss", engine.on_dismiss());
                }
            },
        );
        let _ = apply_autocorrect_settings_edge(
            &settings_flags.general_thesaurus_selection,
            &mut config.thesaurus_selection,
            |on| {
                persist_and_log_switch(
                    "COMPME_THESAURUS_SELECTION",
                    "selection-triggered thesaurus",
                    on,
                )
            },
            |on| {
                if !on {
                    suggestion.latest.clear();
                    let _ = log_err("on_dismiss", engine.on_dismiss());
                }
            },
        );
        // General-tab Launch at Login watcher: mutate the OS first. Only a
        // successful registration/unregistration is persisted; rejection
        // restores the shared atomic and immediately redraws the visible switch.
        if let Err(err) = apply_launch_at_login_settings_edge(
            &settings_flags.general_launch_at_login,
            &mut settings.current_launch_at_login,
            shell.as_ref(),
            |on| persist_and_log_switch("COMPME_LAUNCH_AT_LOGIN", "launch at login", on),
        ) {
            eprintln!("compme: launch-at-login change rejected: {err}");
            settings_window.refresh_switches();
        }
        // General-tab Trailing-space watcher: persist + live engine apply
        // (the flag is baked at build via with_trailing_space, so the c94
        // runtime-setter pattern applies — set_trailing_space).
        apply_trailing_space_settings_edge(
            &settings_flags.general_trailing_space,
            &mut config.trailing_space,
            |on| engine.set_trailing_space(on),
            |on| persist_and_log_switch("COMPME_TRAILING_SPACE", "trailing space", on),
        );
        // Labs-pane watcher: on a switch edge, persist COMPME_MIDLINE and
        // re-apply the engine gate for the current app immediately (per-app
        // overrides still win; the switch changes only the global default).
        // A persist failure is logged but not retried — the runtime global
        // wins until relaunch (deliberate graceful degradation, same stance
        // as the instance lock: an IO hiccup must not stall the app, at the
        // cost of config.env staying stale until the next successful write).
        apply_midline_settings_edge(
            &settings_flags.labs_midline,
            &mut settings.global_mid_word,
            &prefs,
            focus.last_app_key.as_deref(),
            |on| engine.set_allow_mid_word(on),
            |on| persist_and_log_switch("COMPME_MIDLINE", "mid-line completions", on),
        );
        // Context-pane watchers. Clipboard context applies live because submit
        // reads `config.clipboard_context` for each request. Screen OCR also
        // applies live: enabling starts the worker when Screen Recording is
        // granted, and disabling drops it plus clears the worker-side wait.
        if let Some(on) = switch_edge(
            &settings_flags.context_cross_app_previous_inputs,
            &mut config.cross_app_previous_inputs,
        ) {
            cross_app_previous_inputs.store(on, Ordering::Relaxed);
            if !on {
                previous_inputs.clear_cross_app();
            }
            persist_and_log_switch(
                "COMPME_CROSS_APP_PREVIOUS_INPUTS",
                "cross-app previous-input context",
                on,
            );
        }
        if let Some(on) = switch_edge(
            &settings_flags.context_clipboard,
            &mut config.clipboard_context,
        ) {
            apply_clipboard_context_edge(on, &clipboard_cell);
            persist_and_log_switch("COMPME_CLIPBOARD_CONTEXT", "clipboard context", on);
        }
        if let Some(on) = switch_edge(&settings_flags.context_screen, &mut config.screen_context) {
            let context_edge = apply_screen_context_edge(
                on,
                ScreenContextToggleState {
                    config_screen_context: &mut config.screen_context,
                    ui_flag: &settings_flags.context_screen,
                    screen_cell: &screen_cell,
                    screen_ocr: &mut screen_ocr,
                },
                |ms| screen_wait_ms.store(ms, Ordering::Relaxed),
                || shell.screen_capture_permission(),
                || {
                    ScreenOcr::spawn(
                        Arc::clone(&shell),
                        Arc::clone(&screen_cell),
                        context_bound,
                        config.diag_context,
                    )
                    .map_err(|err| err.to_string())
                },
            );
            if context_edge == ScreenContextEdge::RevertedSpawnFailed {
                eprintln!("compme: screen OCR worker unavailable; screen context disabled");
            }
            persist_and_log_switch(
                "COMPME_SCREEN_CONTEXT",
                "screen context",
                config.screen_context,
            );
            settings_window.refresh_switches();
            // Poison-recovery: skipping would leave the Setup pane stale after
            // the screen-context toggle (refresh runs below).
            *settings_flags
                .setup_lines
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = compose_setup_lines(
                &config,
                model_available,
                subscriptions_require_relaunch,
                shell.accessibility_trusted(),
                shell.screen_capture_permission(),
                download.model_download_status.as_deref(),
            );
            settings_window.refresh_setup_labels();
        }
        // Emoji-pane watcher: the replacement path reads config.emoji on each
        // observation, so changing the Option is the live apply. Keep the parsed
        // prefs payload across live off/on cycles; the skin-tone popup updates
        // it below, and gender remains config-backed until its control ships.
        let emoji_edge = handle_emoji_switch_edge(
            &settings_flags.emoji_enabled,
            &mut settings.emoji_enabled,
            &mut config.emoji,
            &mut settings.emoji_prefs,
            |on| persist_and_log_switch("COMPME_EMOJI", "emoji completions", on),
        );
        if emoji_edge == Some(false) {
            suggestion.latest.clear();
            let _ = log_err("on_dismiss", engine.on_dismiss());
        }
        handle_emoji_skin_tone_change_with_invalidation(
            &settings_flags.emoji_skin_tone_index,
            &mut settings.emoji_skin_tone_index,
            &mut config.emoji,
            &mut settings.emoji_prefs,
            |value| persist_and_log_value("COMPME_EMOJI_SKIN_TONE", "emoji skin tone", value),
            || {
                suggestion.latest.clear();
                let _ = log_err("on_dismiss", engine.on_dismiss());
            },
        );
        handle_emoji_gender_change_with_invalidation(
            &settings_flags.emoji_gender_index,
            &mut settings.emoji_gender_index,
            &mut config.emoji,
            &mut settings.emoji_prefs,
            |value| persist_and_log_value("COMPME_EMOJI_GENDER", "emoji gender", value),
            || {
                suggestion.latest.clear();
                let _ = log_err("on_dismiss", engine.on_dismiss());
            },
        );
        drain_deep_links_phase(
            &deep_links,
            &shell,
            &config,
            &mut prefs,
            &mut monitored,
            &mut suggestion,
            &mut engine,
        );
        tray_collection_toggle_phase(&flags, &shell, &focus, &mut prefs, &mut monitored);
        tray_app_disable_phase(
            &flags,
            &shell,
            &focus,
            &mut prefs,
            &mut monitored,
            &mut suggestion,
            &mut engine,
            now_ms,
        );
        let effective_trusted = runtime_trusted(trusted, subscriptions_require_relaunch);
        let enabled = flags.enabled.load(Ordering::Relaxed);
        flush_monitored_changes_after_secure_recheck(
            &mut monitored.pending_monitored,
            &mut monitored.monitored_buffers,
            memory.as_ref(),
            &prefs,
            MonitoredFlushState {
                secure: &mut policy.secure,
                last_secure_poll_ms: &mut policy.last_secure_poll_ms,
            },
            MonitoredFlushRuntime {
                monitored_memory_active,
                enabled,
                trusted: effective_trusted,
                now_ms,
            },
            || shell.secure_input_enabled(),
        );
        let status = derive_status(
            trusted,
            subscriptions_require_relaunch,
            policy.secure,
            model_available,
            inference.is_ready(),
            enabled,
        );
        // Secure input is a true engine-state transition, not only a UI state:
        // clear queued work and invalidate the machine so held requests cannot
        // submit after the secure block clears.
        match secure_edge(policy.prev_secure, policy.secure, effective_trusted) {
            SecureEdge::Enter => {
                clear_monitored_state_for_policy_transition(
                    &mut monitored.pending_monitored,
                    &mut monitored.monitored_buffers,
                );
                suggestion.latest.clear();
                offer_all(
                    &mut suggestion.latest,
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
                if let Some(field) = focus.current_field.clone() {
                    focus.tracker.reset();
                    offer_all(
                        &mut suggestion.latest,
                        log_err("on_focus", engine.on_focus(field)),
                    );
                }
            }
            SecureEdge::None => {}
        }
        // Disabling is user policy: dismiss visible UI and drop queued requests.
        if should_dismiss_on_disable(policy.prev_enabled, enabled) {
            clear_monitored_state_for_policy_transition(
                &mut monitored.pending_monitored,
                &mut monitored.monitored_buffers,
            );
            suggestion.latest.clear();
            let _ = log_err("on_dismiss", engine.on_dismiss());
        }
        if status_drops_pending_requests(status) {
            clear_monitored_state_for_policy_transition(
                &mut monitored.pending_monitored,
                &mut monitored.monitored_buffers,
            );
            suggestion.latest.clear();
        }
        // Persist a user enable/disable toggle (tray or SIGUSR1) so the next
        // launch starts in the same state (A3 settings persistence). Skipped on
        // the first iteration (prev starts equal to the configured value) and
        // never fatal — a read-only disk only costs persistence, not operation.
        if policy.prev_enabled != enabled {
            if let Some(path) = config::config_file_path() {
                match config::persist_setting(
                    &path,
                    "COMPME_ENABLED",
                    if enabled { "true" } else { "false" },
                ) {
                    Ok(()) => {
                        eprintln!("compme: persisted enabled={enabled} to {}", path.display())
                    }
                    Err(err) => eprintln!("compme: could not persist enabled state: {err}"),
                }
            }
        }
        policy.prev_enabled = enabled;
        policy.prev_secure = policy.secure;
        // Only touch AppKit when the rendered state actually changed. The
        // snoozed flag is part of the render state so the title/line flip both
        // when a snooze starts AND when it auto-expires mid-Ready.
        let snoozed = prefs.is_snoozed(now_ms);
        if session_ui.last_render != Some((status, enabled, snoozed)) {
            eprintln!("compme: status={status:?} enabled={enabled} snoozed={snoozed}");
            if let Some(tray) = &tray {
                if let Err(err) = tray.set_status(
                    status.render_title(snoozed),
                    status.render_line(snoozed),
                    enabled,
                    status.needs_accessibility(),
                ) {
                    eprintln!("compme: tray update failed: {err:?}");
                }
            }
            session_ui.last_render = Some((status, enabled, snoozed));
        }
        // Menu-bar 30-day usage line (§11). The string only changes when a
        // stat event landed or the window rolled, so the compare keeps AppKit
        // untouched on idle heartbeats.
        if let Some(tray) = &tray {
            let stats_line = usage_stats.usage.summary_line(wall_ms);
            if session_ui.last_stats_line.as_deref() != Some(stats_line.as_str()) {
                if let Err(err) = tray.set_stats_line(&stats_line) {
                    eprintln!("compme: tray stats update failed: {err:?}");
                }
                session_ui.last_stats_line = Some(stats_line);
            }
        }

        // 5. Submit the newest pending request only when suggestions are allowed
        // (Ready ⇒ trusted + not secure + warm + enabled).
        if host_event_backlog_remaining {
            suggestion.latest.clear();
            if manual_grammar_request.take().is_some() {
                eprintln!("compme: shortcut grammar-check dropped — host event backlog");
            }
        } else if status.suggestions_allowed() {
            if let Some(request) = manual_grammar_request.take() {
                let app_key = effective_app_key(&request.field, |pid| shell.bundle_id_for_pid(pid));
                let domain = cached_domain(&focus.last_domain, app_key.as_deref());
                if request_passes_submit_gates_for_field(
                    &request,
                    SuggestionApp {
                        app_key: app_key.as_deref(),
                        assistant_field: focus.current_assistant_field,
                    },
                    domain,
                    &prefs,
                    now_ms,
                ) {
                    let log_context = RequestLogContext {
                        app_key,
                        assistant_field: focus.current_assistant_field,
                        domain: domain.map(str::to_owned),
                        prefs: prefs.clone(),
                        acceptance_prompt_marker: config.acceptance_prompt_marker.clone(),
                    };
                    let submitted_line = submit_request_and_track(
                        &mut suggestion.submit_times,
                        request,
                        now_ms,
                        log_context,
                        |request| inference.submit(request),
                    );
                    eprintln!("{submitted_line}");
                } else {
                    eprintln!(
                        "{}",
                        request_log_line_for_field(
                            &request,
                            SuggestionApp {
                                app_key: app_key.as_deref(),
                                assistant_field: focus.current_assistant_field,
                            },
                            domain,
                            &prefs,
                            now_ms,
                            config.acceptance_prompt_marker.as_deref(),
                            true,
                        )
                    );
                }
                suggestion.latest.clear();
            }
            if let Some(request) = suggestion.latest.take() {
                // Per-app/domain gating + pause/snooze (A2 §8). The exclude list
                // is keyed on bundle ids. Prefer a fresh pid resolution, but keep
                // the already-canonical request field app as the stable fallback;
                // a transient lookup miss must not fail-open per-app privacy gates.
                // The domain comes from the Focus arm's cache, guarded on the same
                // app key (c131).
                let app_key = effective_app_key(&request.field, |pid| shell.bundle_id_for_pid(pid));
                if request_passes_submit_gates_for_field(
                    &request,
                    SuggestionApp {
                        app_key: app_key.as_deref(),
                        assistant_field: focus.current_assistant_field,
                    },
                    cached_domain(&focus.last_domain, app_key.as_deref()),
                    &prefs,
                    now_ms,
                ) {
                    let domain =
                        cached_domain(&focus.last_domain, app_key.as_deref()).map(str::to_owned);
                    let log_context = RequestLogContext {
                        app_key,
                        assistant_field: focus.current_assistant_field,
                        domain,
                        prefs: prefs.clone(),
                        acceptance_prompt_marker: config.acceptance_prompt_marker.clone(),
                    };
                    // Refresh clipboard and dispatch screen OCR immediately before
                    // submitting this exact request. The worker reads auxiliary
                    // cells after coalescing, so this order prevents stale
                    // clipboard/screen context from a prior gated request.
                    let screen_enabled = config.screen_context && screen_ocr.is_some();
                    let (clipboard_diag, submitted_line) = submit_request_with_auxiliary_context(
                        request,
                        SubmitRequestContext {
                            submit_times: &mut suggestion.submit_times,
                            now_ms,
                            log_context,
                        },
                        AuxiliarySubmitContext {
                            clipboard_enabled: config.clipboard_context,
                            diag_context: config.diag_context,
                            diag_clipboard_marker: config.diag_clipboard_marker.as_deref(),
                            clipboard_cell: &clipboard_cell,
                            screen_enabled,
                        },
                        || shell.read_clipboard_text(),
                        // A fresh AX caret_rect read on the AppKit thread. Bounded:
                        // submits are debounced (not per-keystroke) and the heavy
                        // OCR is offloaded to ScreenOcr's own thread — only this
                        // rect read is inline. If a sluggish AX server ever makes it
                        // stall the heartbeat, reuse the rect from the Caret host
                        // event instead of reading afresh here.
                        |request| adapter.caret_rect(&request.field).ok().flatten(),
                        |submission| {
                            if let Some(ocr) = &screen_ocr {
                                submission.send_to(ocr);
                            }
                        },
                        |request| inference.submit(request),
                    );
                    if let Some(line) = clipboard_diag {
                        eprintln!("compme: clipboard_context={line}");
                    }
                    eprintln!("{submitted_line}");
                } else {
                    eprintln!(
                        "{}",
                        request_log_line_for_field(
                            &request,
                            SuggestionApp {
                                app_key: app_key.as_deref(),
                                assistant_field: focus.current_assistant_field,
                            },
                            cached_domain(&focus.last_domain, app_key.as_deref()),
                            &prefs,
                            now_ms,
                            config.acceptance_prompt_marker.as_deref(),
                            true,
                        )
                    );
                }
            }
        } else if manual_grammar_request.take().is_some() {
            // A one-shot GrammarCheck shortcut arms `manual_grammar_request`,
            // which resets every tick. When this tick is not Ready (Loading,
            // Blocked, or Disabled) the request can never be consumed, so log
            // the drop instead of silently discarding the user's key press —
            // matching the outcome line every sibling shortcut action emits.
            eprintln!(
                "compme: shortcut grammar-check dropped \u{2014} status={status:?} not ready"
            );
        }

        // 6. Tray actions (menu callbacks fire on this same main thread via the
        // run-loop pump, so Relaxed is sufficient for these flags).
        if flags.open_settings.swap(false, Ordering::Relaxed) {
            if let Err(err) = shell.open_permission_settings() {
                eprintln!("compme: open settings failed: {err}");
            }
        }
        for action in take_url_actions(UrlActionFlags {
            check_updates: &flags.check_updates,
            visit_website: &flags.visit_website,
            contact_support: &flags.contact_support,
        }) {
            if let Err(err) = shell.open_url(action.url()) {
                eprintln!("compme: open {} failed: {err}", action.label());
            }
        }
        if flags.quit.load(Ordering::Relaxed) {
            eprintln!("compme: quit requested");
            break;
        }

        // 7. Bounded run (gates pass COMPME_RUN_MS).
        if let Some(run_ms) = config.run_ms {
            if now_ms >= run_ms {
                break;
            }
        }

        // 8. Drain queued window-system events, then pump the host run loop.
        // On macOS the drain is what dispatches Carbon accept-hotkey presses
        // to their handler (a bare CFRunLoop pump never dequeues them — live
        // step-6 finding: hotkeys registered, handler never fired); the pump
        // paces the loop and services the overlay.
        shell.pump_events(heartbeat);
    }

    eprintln!("compme: shutting down");
    // Session usage summary (§11/§16). Window-derived (30d) — past 30 days
    // of uptime it reports LESS than the persisted lifetime totals.
    // Intentional: this is a diagnostic line and latency avg/p95 are
    // inherently windowed; the persist path uses grow-only session totals.
    let final_wall_ms = wall_now_ms();
    let session_usage = session_usage_snapshot(&usage_stats.usage, final_wall_ms);
    eprintln!(
        "compme: usage shown={} accepted={} dismissed={} superseded={} words={} \
         latency_avg={:?} latency_p95={:?}",
        session_usage.counts.shown,
        session_usage.counts.accepted,
        session_usage.counts.dismissed,
        session_usage.counts.superseded,
        session_usage.words,
        session_usage.latency_avg,
        session_usage.latency_p95,
    );
    // Lifetime stats: final flush — the SAME idempotent baseline+session
    // write as the periodic flush. Re-reading the file here (the pre-c128
    // shape) would double-count the session: the file already holds
    // baseline + session from the last periodic flush. Fail-soft — a stats
    // hiccup must not block shutdown.
    if let Err(err) = persist_lifetime_stats(
        stats_path.as_deref(),
        &lifetime_base,
        usage_stats.usage.session_totals(),
    ) {
        eprintln!("compme: stats persist failed: {err}");
    }
    drop(tray); // remove the status item before AppKit teardown
    drop(caret_sub);
    drop(focus_sub);
    inference.shutdown();
    drop(engine); // drops overlay + accept subscription + the engine's adapter handle
    drop(adapter); // last Arc ref → AX worker thread stops
    Ok(())
}

#[cfg(test)]
#[path = "run_loop_tests.rs"]
mod tests;
