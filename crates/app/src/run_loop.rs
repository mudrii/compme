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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use emoji::{EmojiPrefs, Gender, SkinTone};
use engine::{CompletionRequest, Engine, RequestKind, TriggerPolicy};
use personalization::{PersonalizationProfile, SenderIdentity, Strength};
use platform::{
    env_flag_on,
    shell::{DisableArm, TrayFlags},
    AcceptAction, AcceptSubscription, Capabilities, CorrectionRange, FieldHandle, InsertStrategy,
    KeyInterceptMode, OverlayPlacement, PlatformAdapter, PlatformError, ScreenRect, SecurityState,
    ShortcutAction, Subscription, TapControl, TextContext, Toolkit,
};
use prefs::Prefs;
use zeroize::Zeroize;

use crate::adapter::SharedAdapter;
use crate::config::{self, parse_clamped};
use crate::inference::{InferenceHandle, PreviousInputs, ScreenContext, WorkerContext};
use crate::model_select::{load_model, resolve_prompt_mode, resolve_source, PromptMode};
use crate::screen_ocr::ScreenOcr;
use crate::status::{derive_status, AppStatus, BlockReason};
use crate::wiring::{FieldTracker, LatestRequest, Observation};

const DEFAULT_DEBOUNCE_MS: u64 = 120;
const DEFAULT_MAX_WORDS: usize = 8;
const DEFAULT_MIN_CONTEXT_CHARS: usize = 3;
const DEFAULT_MAX_TOKENS: usize = 24;
const DEFAULT_HEARTBEAT_MS: u64 = 12;
/// Candidate completions generated per request (1 = single, up to 5 for cycle).
const DEFAULT_CANDIDATES: usize = 1;
/// Bounded best-effort wait for exact-stamped screen OCR. Vision can be slower
/// than this; late OCR is dropped rather than making suggestion latency unbounded.
const SCREEN_CONTEXT_WAIT_MS: u64 = 250;
/// Per-source character bound when previous-input context is enabled truthily.
const DEFAULT_CONTEXT_MAX_CHARS: usize = 160;
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
const UPDATES_URL: &str = "https://github.com/mudrii/compme/releases/latest";

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
struct PendingMonitoredText {
    field: FieldHandle,
    inserted: String,
    oversized: bool,
    app_key: Option<String>,
    domain: Option<String>,
    terminal_ok: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MonitoredBuffer {
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
        queue.remove(drop_index);
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
    /// Standalone grammar/spell-fix trigger (`COMPME_GRAMMAR_FIX`, default off).
    grammar_fix: bool,
    /// British-English normalization (A2 §16, `COMPME_BRITISH_ENGLISH`, default
    /// off): offer the UK spelling when the trailing word is a known US-only form.
    british_english: bool,
    /// Inline thesaurus / synonym suggestions (A2 §16, `COMPME_THESAURUS`,
    /// default off): offer synonyms for the trailing word as the user types.
    thesaurus: bool,
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
            grammar_fix: lookup("COMPME_GRAMMAR_FIX")
                .is_some_and(|v| v == "1" || v == "true" || v == "on"),
            british_english: lookup("COMPME_BRITISH_ENGLISH")
                .is_some_and(|v| v == "1" || v == "true" || v == "on"),
            thesaurus: lookup("COMPME_THESAURUS")
                .is_some_and(|v| v == "1" || v == "true" || v == "on"),
        }
    }
}

fn emoji_config_enabled(lookup: &impl Fn(&str) -> Option<String>) -> bool {
    lookup("COMPME_EMOJI").is_some_and(|v| v == "1" || v == "true" || v == "on")
}

/// Parse emoji prefs (A2 §8/§16) independently of the enable gate so persisted
/// skin-tone/gender choices survive while Emoji completions are disabled.
fn build_emoji_prefs(lookup: &impl Fn(&str) -> Option<String>) -> EmojiPrefs {
    EmojiPrefs {
        skin_tone: parse_skin_tone(lookup("COMPME_EMOJI_SKIN_TONE")),
        gender: parse_gender(lookup("COMPME_EMOJI_GENDER")),
    }
}

fn parse_skin_tone(raw: Option<String>) -> SkinTone {
    match raw
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("light") => SkinTone::Light,
        Some("medium-light") | Some("medium_light") => SkinTone::MediumLight,
        Some("medium") => SkinTone::Medium,
        Some("medium-dark") | Some("medium_dark") => SkinTone::MediumDark,
        Some("dark") => SkinTone::Dark,
        _ => SkinTone::Default,
    }
}

const EMOJI_SKIN_TONE_VALUES: [(SkinTone, &str); 6] = [
    (SkinTone::Default, "default"),
    (SkinTone::Light, "light"),
    (SkinTone::MediumLight, "medium-light"),
    (SkinTone::Medium, "medium"),
    (SkinTone::MediumDark, "medium-dark"),
    (SkinTone::Dark, "dark"),
];

fn emoji_skin_tone_index(tone: SkinTone) -> usize {
    EMOJI_SKIN_TONE_VALUES
        .iter()
        .position(|(candidate, _)| *candidate == tone)
        .unwrap_or(0)
}

fn emoji_skin_tone_from_index(index: usize) -> SkinTone {
    EMOJI_SKIN_TONE_VALUES
        .get(index)
        .map(|(tone, _)| *tone)
        .unwrap_or_default()
}

fn emoji_skin_tone_value(tone: SkinTone) -> &'static str {
    EMOJI_SKIN_TONE_VALUES
        .iter()
        .find_map(|(candidate, value)| (*candidate == tone).then_some(*value))
        .unwrap_or("default")
}

fn parse_gender(raw: Option<String>) -> Gender {
    match raw
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("female") => Gender::Female,
        Some("male") => Gender::Male,
        _ => Gender::Neutral,
    }
}

/// Gender popup rows in menu order (index addresses this table); the second
/// element is the persisted `COMPME_EMOJI_GENDER` value `parse_gender` reads.
const EMOJI_GENDER_VALUES: [(Gender, &str); 3] = [
    (Gender::Neutral, "neutral"),
    (Gender::Female, "female"),
    (Gender::Male, "male"),
];

fn emoji_gender_index(gender: Gender) -> usize {
    EMOJI_GENDER_VALUES
        .iter()
        .position(|(candidate, _)| *candidate == gender)
        .unwrap_or(0)
}

fn emoji_gender_from_index(index: usize) -> Gender {
    EMOJI_GENDER_VALUES
        .get(index)
        .map(|(gender, _)| *gender)
        .unwrap_or_default()
}

fn emoji_gender_value(gender: Gender) -> &'static str {
    EMOJI_GENDER_VALUES
        .iter()
        .find_map(|(candidate, value)| (*candidate == gender).then_some(*value))
        .unwrap_or("neutral")
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

fn emoji_offer(left: &str, cfg: &Option<EmojiPrefs>) -> Option<(String, usize)> {
    let prefs = cfg.as_ref()?;
    let suggestion = emoji::suggest(left, prefs)?;
    Some((suggestion.glyph, suggestion.replace_chars))
}

/// The trailing run of alphabetic characters at the caret (the word being typed),
/// or `None` when the left context ends in a non-letter (boundary). Used to gate
/// the word-based replacement offers (typo fix, US→UK) on an exact whole-word
/// match — the same "token at the caret" model emoji uses for `:shortcode`.
fn trailing_word(left: &str) -> Option<&str> {
    let start = left
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphabetic())
        .last()
        .map(|(i, _)| i)?;
    let word = &left[start..];
    (!word.is_empty()).then_some(word)
}

/// A local *replacement* to offer for the typed left-context, or `None`. Tries the
/// enabled features in priority order: emoji (`:shortcode`, explicit intent), then
/// the word-based fixes on the trailing word — typo autocorrect, then US→UK
/// spelling. Returns `(replacement_text, chars_to_replace)`. Pure over its inputs
/// so the observe-path wiring stays testable.
fn replacement_offer(
    left: &str,
    config: &Config,
    autocorrect_enabled: bool,
    thesaurus_enabled: bool,
) -> Option<(Vec<String>, usize)> {
    if let Some((glyph, len)) = emoji_offer(left, &config.emoji) {
        return Some((vec![glyph], len));
    }
    let word = trailing_word(left)?;
    let word_len = word.chars().count();
    if autocorrect_enabled {
        if let Some(fix) = autocorrect::correct(word) {
            return Some((vec![fix], word_len));
        }
        // Grammar capitalization ("i" -> "I") rides the same autocorrect gate:
        // it is a correction, and like typo-fixing it must stay off in code
        // fields (where a bare `i` is a variable). Apostrophe-aware contractions
        // are intentionally out of this trailing-word helper; they would require
        // a caret-token model that includes apostrophes.
        if let Some(fix) = grammar::capitalize_pronoun(word) {
            return Some((vec![fix], word_len));
        }
    }
    if config.british_english {
        if let Some(uk) = localize::to_british(word) {
            return Some((vec![uk], word_len));
        }
    }
    if thesaurus_enabled {
        let syns = thesaurus::synonyms(word);
        if !syns.is_empty() {
            return Some((syns, word_len));
        }
    }
    None
}

/// The full observe-path decision for a local replacement: a `(text, replace_left)`
/// to offer, or `None`. Combines the suggestion gate (tray `enabled` + per-app
/// exclude / snooze / terminal-NL, the SAME policy as a model completion) with the
/// feature lookup, so a local offer never shows where a model one wouldn't. Pure
/// over its inputs so the gate+offer interaction is unit-testable (warm-up is
/// intentionally not gated — replacements are local and need no model).
fn replacement_decision(
    left: &str,
    config: &Config,
    prefs: &Prefs,
    app_key: Option<&str>,
    domain: Option<&str>,
    enabled: bool,
    now_ms: u64,
) -> Option<(Vec<String>, usize)> {
    // `prefs` is passed separately from `config` because the run loop mutates
    // its prefs at runtime (snooze); reading `config.prefs` here would split
    // the policy source and let a local offer show while the model is snoozed.
    if !enabled || !suggestion_gates_pass(app_key, left, domain, prefs, now_ms) {
        return None;
    }
    // Per-app autocorrect/thesaurus overrides (App Settings): prefs override,
    // else the global config default.
    let autocorrect = prefs.autocorrect_enabled(app_key, config.autocorrect);
    let thesaurus = prefs.thesaurus_enabled(app_key, config.thesaurus);
    replacement_offer(left, config, autocorrect, thesaurus)
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
        || !suggestion_gates_pass(
            gate.app_key,
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

/// The completion worker's context char bound. Clipboard/screen context need
/// a positive bound even when previous-input context is off — with
/// `context_max_chars == 0` the worker's block builder returns `""` and the
/// enabled auxiliary sources would be a silent no-op. An explicit positive
/// bound always wins.
fn context_bound_chars(clipboard: bool, screen_active: bool, max_chars: usize) -> usize {
    if (clipboard || screen_active) && max_chars == 0 {
        DEFAULT_CONTEXT_MAX_CHARS
    } else {
        max_chars
    }
}

fn settings_context_bound_chars(max_chars: usize) -> usize {
    // Settings can enable clipboard context after launch. Keep the inference
    // worker's bound positive enough for that later enable; with no cells
    // populated, the generated context block remains empty.
    context_bound_chars(true, false, max_chars)
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

/// Whether the destination model file is already present and complete — a
/// non-empty `.gguf`, from the file's length (`None` = missing). A missing
/// file or a 0-byte stub (an interrupted finalize) is NOT present, so the
/// picker re-downloads rather than treating the stub as done. Guards a repeat
/// "Download" click from re-fetching and clobbering a good file.
fn model_present(dest_len: Option<u64>) -> bool {
    matches!(dest_len, Some(len) if len > 0)
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

fn request_log_line(
    request: &CompletionRequest,
    app_key: Option<&str>,
    domain: Option<&str>,
    prefs: &Prefs,
    now_ms: u64,
    acceptance_prompt_marker: Option<&str>,
    blocked: bool,
) -> String {
    let app_allows = app_allows_suggestions(app_key);
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
    domain: Option<String>,
    prefs: Prefs,
    acceptance_prompt_marker: Option<String>,
}

impl RequestLogContext {
    fn line_for(&self, request: &CompletionRequest, now_ms: u64) -> String {
        request_log_line(
            request,
            self.app_key.as_deref(),
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
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let created = !parent.exists();
        let hardened = std::fs::create_dir_all(parent).and_then(|()| {
            if created {
                platform_windows::win_host::harden_owner_only(parent)
            } else {
                Ok(())
            }
        });
        if let Err(err) = hardened {
            eprintln!(
                "compme: failed to harden memory dir {}: {err} — memory disabled",
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
    // covers a pre-existing unhardened dir and a bare-filename path. Posture
    // like unix (log, don't disable): the dir gate above is the fail-closed
    // layer for the fresh-install case.
    #[cfg(windows)]
    if opened.is_ok() {
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let mut file = path.as_os_str().to_owned();
            file.push(suffix);
            let file = std::path::Path::new(&file);
            if file.exists() {
                if let Err(err) = platform_windows::win_host::harden_owner_only(file) {
                    eprintln!(
                        "compme: failed to tighten permissions on {}: {err}",
                        file.display()
                    );
                }
            }
        }
    }
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

/// Build the personalization profile from config (A2 §6). The global
/// instructions key steers every request; optional per-app/per-domain target
/// lists activate supplemental value keys without delimiter-parsing free text.
fn build_personalization(lookup: &impl Fn(&str) -> Option<String>) -> PersonalizationProfile {
    // Case-handling asymmetry is intentional: per-app keys are kept verbatim
    // (`|app| app.to_string()`) because bundle ids are case-stable identifiers
    // matched against the verbatim `request.field.app`, while per-domain keys are
    // lowercased to match the lowercased host from `domain_from_url`. Do NOT
    // "normalize per_app too" — that would break bundle-id keying.
    let mut profile = PersonalizationProfile {
        global_instructions: lookup("COMPME_INSTRUCTIONS").unwrap_or_default(),
        per_app: instruction_map_from_config(
            lookup,
            "COMPME_INSTRUCTIONS_APPS",
            "COMPME_INSTRUCTIONS_APP_",
            |app| app.to_string(),
        ),
        per_domain: instruction_map_from_config(
            lookup,
            "COMPME_INSTRUCTIONS_DOMAINS",
            "COMPME_INSTRUCTIONS_DOMAIN_",
            |domain| domain.to_ascii_lowercase(),
        ),
        sender: SenderIdentity {
            name: lookup("COMPME_SENDER_NAME").unwrap_or_default(),
            email: lookup("COMPME_SENDER_EMAIL").unwrap_or_default(),
        },
        ..Default::default()
    };
    if let Some(stop) = lookup("COMPME_STRENGTH").and_then(|raw| raw.parse::<u8>().ok()) {
        profile.strength = Strength::from_stop(stop);
    }
    profile
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

fn apply_clipboard_context_edge(on: bool, clipboard_cell: &Mutex<Option<String>>) {
    if !on {
        *clipboard_cell.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ScreenContextEdge {
    Disabled,
    Enabled,
    RevertedDenied,
    RevertedSpawnFailed,
}

struct ScreenContextToggleState<'a, T> {
    config_screen_context: &'a mut bool,
    ui_flag: &'a AtomicBool,
    screen_cell: &'a Mutex<Option<ScreenContext>>,
    screen_ocr: &'a mut Option<T>,
}

fn apply_screen_context_edge<T>(
    on: bool,
    state: ScreenContextToggleState<'_, T>,
    mut set_wait_ms: impl FnMut(u64),
    screen_recording_permission: impl FnOnce() -> bool,
    spawn_screen_ocr: impl FnOnce() -> Result<T, String>,
) -> ScreenContextEdge {
    if !on {
        *state.screen_cell.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *state.screen_ocr = None;
        set_wait_ms(0);
        return ScreenContextEdge::Disabled;
    }

    if !screen_recording_permission() {
        *state.config_screen_context = false;
        state.ui_flag.store(false, Ordering::Relaxed);
        *state.screen_ocr = None;
        set_wait_ms(0);
        return ScreenContextEdge::RevertedDenied;
    }

    match spawn_screen_ocr() {
        Ok(ocr) => {
            *state.screen_ocr = Some(ocr);
            set_wait_ms(SCREEN_CONTEXT_WAIT_MS);
            ScreenContextEdge::Enabled
        }
        Err(_) => {
            *state.config_screen_context = false;
            state.ui_flag.store(false, Ordering::Relaxed);
            *state.screen_ocr = None;
            set_wait_ms(0);
            ScreenContextEdge::RevertedSpawnFailed
        }
    }
}

fn instruction_map_from_config(
    lookup: &impl Fn(&str) -> Option<String>,
    list_key: &str,
    value_prefix: &str,
    normalize_target: impl Fn(&str) -> String,
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let targets = comma_list(lookup(list_key));
    let mut key_counts = HashMap::new();
    for target in &targets {
        let value_key = format!("{value_prefix}{}", config_target_key_suffix(target));
        *key_counts.entry(value_key).or_insert(0usize) += 1;
    }
    for target in targets {
        let value_key = format!("{value_prefix}{}", config_target_key_suffix(&target));
        if key_counts.get(&value_key) != Some(&1) {
            continue;
        }
        let Some(value) = lookup(&value_key) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        map.insert(normalize_target(&target), value.to_string());
    }
    map
}

fn config_target_key_suffix(target: &str) -> String {
    target
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
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
/// `Unsupported` hard-blocks. `SidebarOnly` is also blocked until A3 adds a real
/// editor-vs-sidebar detector; fail-closed is safer than suggesting in editor
/// panes the spec explicitly excludes. Unresolved app → allow (fail-open), since
/// the field's own capabilities still gate.
fn app_allows_suggestions(app_key: Option<&str>) -> bool {
    app_key.is_none_or(|app| {
        let tier = compat::compatibility_tier(app);
        tier.allows_suggestions() && !tier.sidebar_only()
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
fn suggestion_gates_pass(
    app_key: Option<&str>,
    text: &str,
    domain: Option<&str>,
    prefs: &Prefs,
    now_ms: u64,
) -> bool {
    let terminal_ok = app_key.is_none_or(|app| compat::terminal_prompt_activates(app, text));
    app_allows_suggestions(app_key) && terminal_ok && prefs.should_suggest(app_key, domain, now_ms)
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
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
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
struct DomainMissNotice {
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

fn request_passes_submit_gates(
    request: &CompletionRequest,
    app_key: Option<&str>,
    domain: Option<&str>,
    prefs: &Prefs,
    now_ms: u64,
) -> bool {
    browser_domain_fresh_enough_for_rules(app_key, domain, prefs)
        && suggestion_gates_pass(app_key, request_gate_text(request), domain, prefs, now_ms)
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
        && app_allows_suggestions(app_key)
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
fn record_full_accept(
    action: AcceptAction,
    field: &FieldHandle,
    text: &str,
    context_max_chars: usize,
    previous_inputs: &PreviousInputs,
    memory: Option<&memory::MemoryStore>,
    collection_allowed: bool,
) {
    // Per-app "Input Collection off" (tray submenu / Cotypist parity) gates
    // BOTH sinks below — previous-inputs context AND encrypted memory.
    if !collection_allowed || action != AcceptAction::Full || field.app.starts_with("pid:") {
        return;
    }
    if context_max_chars > 0 {
        previous_inputs.record(&field.app, redaction::redact(text));
    }
    if let Some(store) = memory {
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
    wall_ms: u64,
    context_max_chars: usize,
    previous_inputs: &'a PreviousInputs,
    memory: Option<&'a memory::MemoryStore>,
    prefs: &'a Prefs,
    tracker: &'a mut FieldTracker,
    usage: &'a mut stats::Stats,
}

fn apply_accept_side_effects(accepted: bool, side_effects: AcceptSideEffects<'_>) {
    if !accepted {
        return;
    }
    let Some((field, text, replace_left)) = side_effects.preview else {
        if side_effects.action == AcceptAction::Correction {
            if let Some((field, text, range)) = side_effects.correction_preview {
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
        }
        return;
    };

    // Record only after `on_accept` succeeds. A failed insert must not leak a
    // never-accepted completion into previous-input context or encrypted memory.
    record_full_accept(
        side_effects.action,
        field,
        text,
        side_effects.context_max_chars,
        side_effects.previous_inputs,
        side_effects.memory,
        side_effects.prefs.collection_allowed(Some(&field.app)),
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
struct LogSquelch {
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

fn comma_list(raw: Option<String>) -> Vec<String> {
    raw.map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
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
    let tmp = path.with_extension("env.tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    {
        use std::io::Write as _;
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(stats::render_stats_file(&merged).as_bytes())?;
        // fsync before the rename: the periodic flush writes every ≤5 dirty
        // minutes (vs once per run pre-c128), so the power-loss window where
        // an unsynced rename persists a truncated file is no longer
        // negligible (review-c128).
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
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
    available_ram_gb: u32,
) -> crate::shell::SettingsFlags {
    crate::shell::SettingsFlags {
        general_enabled: tray_enabled,
        labs_midline: Arc::new(AtomicBool::new(config.allow_mid_word)),
        general_autocorrect: Arc::new(AtomicBool::new(config.autocorrect)),
        general_trailing_space: Arc::new(AtomicBool::new(config.trailing_space)),
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
        setup_reveal_model: Arc::new(AtomicBool::new(false)),
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

/// The runtime-persisted keys: anything the running app writes back to
/// config.env (Settings switches, policy overrides, and one-shot acceptances).
/// Env-over-file layering means a set env var silently wins at relaunch —
/// `env_shadow_warnings` names the shadowed ones at startup.
///
/// KEEP IN SYNC with every `persist_setting` writer: a new runtime-persisted key
/// must be added here or its shadow goes unwarned (review-c111/c127; the
/// len-pinned test below backstops this). Deliberately conservative: a key
/// set to "" still warns — it parses falsy but still occupies the env layer.
const SWITCH_KEYS: [&str; 33] = [
    "COMPME_ENABLED",
    "COMPME_MIDLINE",
    "COMPME_AUTOCORRECT",
    "COMPME_GRAMMAR_FIX",
    "COMPME_TRAILING_SPACE",
    "COMPME_CLIPBOARD_CONTEXT",
    "COMPME_SCREEN_CONTEXT",
    "COMPME_INSTRUCTIONS",
    "COMPME_SENDER_NAME",
    "COMPME_SENDER_EMAIL",
    "COMPME_STRENGTH",
    "COMPME_EMOJI",
    "COMPME_EMOJI_SKIN_TONE",
    "COMPME_EMOJI_GENDER",
    "COMPME_NO_COLLECT_APPS",
    "COMPME_EXCLUDED_APPS",
    "COMPME_EXCLUDED_DOMAINS",
    "COMPME_ENABLED_APPS",
    "COMPME_DISABLED_APPS",
    "COMPME_MIDLINE_ON_APPS",
    "COMPME_MIDLINE_OFF_APPS",
    "COMPME_AUTOCORRECT_ON_APPS",
    "COMPME_AUTOCORRECT_OFF_APPS",
    "COMPME_GRAMMAR_FIX_ON_APPS",
    "COMPME_GRAMMAR_FIX_OFF_APPS",
    "COMPME_THESAURUS_ON_APPS",
    "COMPME_THESAURUS_OFF_APPS",
    "COMPME_TAB_DISABLED_APPS",
    // License acceptances persist on the prompt's Accept; an env shadow
    // resurrects the un-accepted state at relaunch → surprise re-prompt
    // (fail-closed, but confusing without the warning) (review-c127).
    "COMPME_LICENSE_ACCEPTED",
    // Accept-key rebinds persist after a successful live re-arm (recorder
    // 5b); an env shadow resurrects the OLD keys at relaunch while the
    // Shortcuts pane read the file — the exact desync the warning names.
    "COMPME_ACCEPT_WORD_KEY",
    "COMPME_ACCEPT_FULL_KEY",
    "COMPME_GRAMMAR_ACCEPT_KEY",
    "COMPME_GRAMMAR_CHECK_KEY",
];

/// One warning line per switch key currently set in the environment
/// (review-c109: a flipped switch persists to file, then the env var
/// resurrects the old value at relaunch — confusing without this notice).
fn env_shadow_warnings(is_env_set: impl Fn(&str) -> bool) -> Vec<String> {
    SWITCH_KEYS
        .iter()
        .filter(|key| is_env_set(key))
        .map(|key| {
            format!(
                "{key} is set in the environment \u{2014} Settings changes persist to \
                 config.env but the environment wins at relaunch"
            )
        })
        .collect()
}

fn startup_env_shadow_notice_lines(is_env_set: impl Fn(&str) -> bool) -> Vec<String> {
    env_shadow_warnings(is_env_set)
        .into_iter()
        .map(|warning| format!("compme: {warning}"))
        .collect()
}

/// Edge-detect a settings switch: if the UI atomic differs from the loop's
/// current value, update the current value and return the new state — the
/// caller applies + persists exactly once per edge (audit c121: three
/// watchers shared this shape verbatim).
fn switch_edge(flag: &AtomicBool, current: &mut bool) -> Option<bool> {
    let now = flag.load(Ordering::Relaxed);
    (now != *current).then(|| {
        *current = now;
        now
    })
}

fn apply_autocorrect_settings_edge(
    flag: &AtomicBool,
    current: &mut bool,
    persist: impl FnOnce(bool),
    dismiss_existing: impl FnOnce(bool),
) -> Option<bool> {
    let on = switch_edge(flag, current)?;
    persist(on);
    if !on {
        dismiss_existing(on);
    }
    Some(on)
}

fn apply_trailing_space_settings_edge(
    flag: &AtomicBool,
    current: &mut bool,
    set_trailing_space: impl FnOnce(bool),
    persist: impl FnOnce(bool),
) -> Option<bool> {
    let on = switch_edge(flag, current)?;
    set_trailing_space(on);
    persist(on);
    Some(on)
}

fn apply_midline_settings_edge(
    flag: &AtomicBool,
    global_mid_word: &mut bool,
    prefs: &Prefs,
    focused_app: Option<&str>,
    set_allow_mid_word: impl FnOnce(bool),
    persist: impl FnOnce(bool),
) -> Option<bool> {
    let on = switch_edge(flag, global_mid_word)?;
    set_allow_mid_word(prefs.mid_line_enabled(focused_app, on));
    persist(on);
    Some(on)
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

/// Consume the tray "Check for Updates…" flag: one click opens the release
/// page at most once (swap-consumed, the same one-shot contract as the snooze
/// request). The opener is injected — the same closure-injection seam the
/// submit path uses — so tests observe the URL instead of spawning `open(1)`.
fn handle_check_updates_flag(flag: &AtomicBool, open: impl FnOnce(&'static str)) {
    if flag.swap(false, Ordering::Relaxed) {
        open(UPDATES_URL);
    }
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

/// Parse a fail-safe boolean: only explicit falsy values disable; anything else
/// (incl. unrecognized strings) keeps the safe default so a typo never silently
/// turns the whole product off.
///
/// Shared by two distinct keys on purpose — `COMPME_ENABLED` (the global
/// tray-toggle state, persisted on toggle) and `COMPME_DEFAULT_ENABLED`
/// (the per-app suggestion-policy default in prefs). Both want the same
/// fail-safe-on parse; their SEMANTICS stay separate.
fn parse_enabled_default(raw: Option<String>) -> bool {
    match raw {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        None => true,
    }
}

/// Build suggestion-gating preferences from config (A2 §8). Comma-separated
/// lists carry app/domain hard excludes, explicit per-app enable/disable policy,
/// and per-app feature overrides.
fn build_prefs(lookup: &impl Fn(&str) -> Option<String>) -> Prefs {
    let excluded_apps = comma_list(lookup("COMPME_EXCLUDED_APPS"))
        .into_iter()
        .collect();
    let mut prefs = Prefs {
        default_enabled: parse_enabled_default(lookup("COMPME_DEFAULT_ENABLED")),
        excluded_apps,
        ..Default::default()
    };
    for domain in comma_list(lookup("COMPME_EXCLUDED_DOMAINS")) {
        prefs.excluded_domains.insert(domain.to_ascii_lowercase());
    }
    for app in comma_list(lookup("COMPME_ENABLED_APPS")) {
        prefs.per_app.entry(app).or_default().enabled = Some(true);
    }
    for app in comma_list(lookup("COMPME_DISABLED_APPS")) {
        prefs.per_app.entry(app).or_default().enabled = Some(false);
    }
    // Per-app typing-history opt-outs (tray "Input Collection in <app>"),
    // mirroring the COMPME_EXCLUDED_APPS comma-list format.
    if let Some(raw) = lookup("COMPME_NO_COLLECT_APPS") {
        for app in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            prefs
                .per_app
                .entry(app.to_string())
                .or_default()
                .collect_inputs = Some(false);
        }
    }
    // Per-app feature overrides (App Settings pane): _ON/_OFF comma lists per
    // feature; ON parses first so a conflicting OFF (parsed second) WINS.
    for app in comma_list(lookup("COMPME_MIDLINE_ON_APPS")) {
        prefs.per_app.entry(app).or_default().mid_line = Some(true);
    }
    for app in comma_list(lookup("COMPME_MIDLINE_OFF_APPS")) {
        prefs.per_app.entry(app).or_default().mid_line = Some(false);
    }
    for app in comma_list(lookup("COMPME_AUTOCORRECT_ON_APPS")) {
        prefs.per_app.entry(app).or_default().autocorrect = Some(true);
    }
    for app in comma_list(lookup("COMPME_AUTOCORRECT_OFF_APPS")) {
        prefs.per_app.entry(app).or_default().autocorrect = Some(false);
    }
    for app in comma_list(lookup("COMPME_GRAMMAR_FIX_ON_APPS")) {
        prefs.per_app.entry(app).or_default().grammar_fix = Some(true);
    }
    for app in comma_list(lookup("COMPME_GRAMMAR_FIX_OFF_APPS")) {
        prefs.per_app.entry(app).or_default().grammar_fix = Some(false);
    }
    for app in comma_list(lookup("COMPME_THESAURUS_ON_APPS")) {
        prefs.per_app.entry(app).or_default().thesaurus = Some(true);
    }
    for app in comma_list(lookup("COMPME_THESAURUS_OFF_APPS")) {
        prefs.per_app.entry(app).or_default().thesaurus = Some(false);
    }
    for app in comma_list(lookup("COMPME_TAB_DISABLED_APPS")) {
        prefs.per_app.entry(app).or_default().tab_disabled = true;
    }
    prefs
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

fn prepare_model_download_dest(dest: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            format!(
                "failed to create model directory {}: {err}",
                parent.display()
            )
        })?;
    }
    Ok(())
}

fn model_download_dest_len(dest: &std::path::Path) -> Option<u64> {
    std::fs::metadata(dest).ok().map(|m| m.len())
}

/// Validate a bring-your-own-model file: a readable, non-empty `.gguf` whose
/// header carries the GGUF magic. Checked at the trust boundary (the file
/// panel) so a bad pick fails at the click, not deep in the model loader after
/// a relaunch. Returns a human-readable reason on rejection.
fn validate_gguf_model(path: &std::path::Path) -> Result<(), String> {
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("gguf"))
        != Some(true)
    {
        return Err(format!("{} is not a .gguf file", path.display()));
    }
    let mut file = std::fs::File::open(path)
        .map_err(|err| format!("cannot open {}: {err}", path.display()))?;
    let mut magic = [0u8; 4];
    // read_exact on a <4-byte (e.g. empty/partial) file errors, so this also
    // rejects empty stubs.
    std::io::Read::read_exact(&mut file, &mut magic)
        .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    if &magic != b"GGUF" {
        return Err(format!(
            "{} is not a GGUF model (bad header)",
            path.display()
        ));
    }
    Ok(())
}

/// The app-support models directory (sibling of the config file): where
/// `Download Model` writes GGUFs. `None` when no config home resolves.
fn app_support_models_dir() -> Option<PathBuf> {
    config::config_file_path().map(|path| path.with_file_name("models"))
}

/// The most-recently-modified non-empty `*.gguf` in `dir`, if any. Downloads
/// land in this dir, but the loader otherwise only consults COMPME_MODEL_PATH
/// (env/config file) and the DEFAULT_MODEL fallback — a repo-relative dev path
/// absent from a shipped `.app`. So a model the user already downloaded (this
/// build or an older one) would sit unused and the Setup row stay ✗, with a
/// re-download click reporting only "already present". Newest wins so the
/// latest download is adopted; empty/partial stubs are skipped.
// ponytail: newest-by-mtime, not the picker's selection — the Download button
// persists the exact selected model; this is only the zero-click fallback.
fn discover_downloaded_model(dir: &std::path::Path) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("gguf") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || meta.len() == 0 {
            continue;
        }
        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        if newest
            .as_ref()
            .is_none_or(|(newest_mtime, _)| mtime > *newest_mtime)
        {
            newest = Some((mtime, path));
        }
    }
    newest.map(|(_, path)| path)
}

fn model_download_dest_present(
    dest: &std::path::Path,
    expected_sha256: Option<&str>,
) -> Result<bool, String> {
    if !model_present(model_download_dest_len(dest)) {
        return Ok(false);
    }
    let Some(expected) = expected_sha256 else {
        return Ok(true);
    };
    let file = std::fs::File::open(dest)
        .map_err(|err| format!("failed to read existing model {}: {err}", dest.display()))?;
    let actual = model_fetch::read_sha256_hex(std::io::BufReader::new(file))
        .map_err(|err| format!("failed to hash existing model {}: {err}", dest.display()))?;
    Ok(actual == expected.to_ascii_lowercase())
}

fn model_download_ram_block_message(
    entry: &model_catalog::ModelEntry,
    available_ram_gb: u32,
) -> Option<String> {
    (!model_catalog::offerable_by_ram(entry, available_ram_gb)).then(|| {
        format!(
            "download of {} blocked — requires at least {} GiB RAM (available: {} GiB)",
            entry.name, entry.min_ram_gb, available_ram_gb
        )
    })
}

/// Build the whole stack, run until a signal (or the run-ms deadline), then tear
/// down in order.
pub fn run() -> Result<(), String> {
    // Single-instance guard FIRST — before any AX observer, hotkey
    // registration, or Apple Events handler exists. Two instances double all
    // of those (live c92 finding: open(1) launches a second copy via Launch
    // Services when the registered handler isn't already running). flock is
    // launch-method-agnostic and kernel-released on any exit.
    let Some(_instance_lock) = instance_lock_startup_gate(
        config::instance_lock_path(),
        config::try_acquire_instance_lock,
        || {},
    )?
    else {
        return Ok(());
    };

    // Mutable: General-tab switches update globals live (autocorrect today;
    // enabled/trailing-space later) — field writes between heartbeats only.
    let mut config = Config::from_env();
    install_signal_handlers();
    let shell = crate::shell::make_shell();

    // Permissions: if Accessibility isn't granted, fire the system prompt once.
    // The app keeps running and reflects the Blocked state in the tray. Focus,
    // caret, and accept subscriptions are installed once at startup; if any of
    // them degrade to no-op while permission is missing, granting Accessibility
    // later still requires a relaunch to install real event streams.
    let mut trusted = shell.accessibility_trusted();
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

    let adapter = crate::shell::make_adapter(config.acceptance_pid)
        .map_err(|err| format!("adapter init: {err:?}"))?;
    let adapter = Arc::new(adapter);

    let overlay = crate::shell::make_overlay().map_err(|err| format!("overlay init: {err:?}"))?;

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
    // explicit, existing COMPME_MODEL_PATH always wins (exists() short-circuits
    // the scan). Persist the adoption so later launches skip the scan.
    if config.stub_completion.is_none() && !config.model_path.exists() {
        if let Some(found) = app_support_models_dir()
            .as_deref()
            .and_then(discover_downloaded_model)
        {
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
    if let Some(enabled) = config.launch_at_login {
        match shell.set_launch_at_login(enabled) {
            Ok(()) => eprintln!(
                "compme: launch at login {}",
                if enabled { "ON" } else { "OFF" }
            ),
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
    let mut screen_ocr = if screen_active {
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
    let worker_context = WorkerContext {
        previous_inputs: previous_inputs.clone(),
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
        collection_toggle: Arc::new(AtomicBool::new(false)),
        app_disable: Arc::new(Mutex::new(None)),
    };
    // Runtime-mutable policy (snooze); starts from the configured prefs. The
    // ONE prefs the loop reads — never read config.prefs after this point, or
    // the policy source splits.
    let mut prefs = config.prefs.clone();
    // A tray failure is non-fatal — the engine still runs headless.
    let tray = match crate::shell::make_tray(flags.clone()) {
        Ok(tray) => Some(tray),
        Err(err) => {
            eprintln!("compme: tray unavailable: {err:?}");
            None
        }
    };

    let heartbeat = Duration::from_millis(config.heartbeat_ms);
    let mut tracker = FieldTracker::new();
    // Local 30-day usage stats (§11/§16). Accepts/dismisses are recorded from the
    // host inputs; Shown/Superseded are drained from the engine each loop turn.
    // The menu-bar display + persistence are A3 surfaces.
    let mut usage = stats::Stats::new();
    // Submit timestamps keyed by request generation, used to derive
    // first-suggestion latency when the matching outcome returns (§11 p95 floor).
    let mut submit_times: HashMap<u64, u64> = HashMap::new();
    let mut latest = LatestRequest::new();
    let mut pending_monitored: Vec<PendingMonitoredText> = Vec::new();
    let mut monitored_buffers: HashMap<FieldHandle, MonitoredBuffer> = HashMap::new();
    let mut current_field: Option<FieldHandle> = None;
    let mut hinted_apps: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut prev_enabled = config.enabled;
    let mut secure = false;
    let mut prev_secure = false;
    let mut last_secure_poll_ms: Option<u64> = None;
    let mut last_render: Option<(crate::status::AppStatus, bool, bool)> = None;
    let mut last_stats_line: Option<String> = None;
    let mut read_err_squelch = LogSquelch::default();
    // S2 settings window (lazy NSWindow) + the activation-policy poll state.
    // Settings switches write flags; the watchers below persist and apply them.
    // `global_mid_word` is the live global default because per-app overrides
    // still derive from it. Emoji is stored as an Option<EmojiPrefs>, so track
    // its bool edge separately from the config payload.
    let mut global_mid_word = config.allow_mid_word;
    let mut emoji_enabled = config.emoji.is_some();
    let mut emoji_prefs = config.emoji_prefs;
    let mut emoji_skin_tone_index = emoji_skin_tone_index(emoji_prefs.skin_tone);
    let mut emoji_gender_index = emoji_gender_index(emoji_prefs.gender);
    let available_ram_gb = model_catalog::bytes_to_whole_gb(shell.physical_memory_bytes());
    let settings_flags =
        build_settings_flags(&config, Arc::clone(&flags.enabled), available_ram_gb);
    // The app ids behind the Apps rows as last rendered (index == row).
    let mut apps_ids: Vec<String> = Vec::new();
    // One downloader per process (model_fetch contract); lazy — spawned on
    // the first Download click. Status polled per heartbeat for logging.
    let mut model_downloader: Option<model_fetch::ModelDownloader> = None;
    let mut model_download_status: Option<std::sync::Arc<model_fetch::DownloadStatus>> = None;
    let mut model_download_logged: u8 = 0; // 0=idle 1=running 2=terminal
                                           // Visible-only Setup re-probe cadence (setup_poll_due).
    let mut last_setup_poll_ms: Option<u64> = None;
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
    let mut last_stats_flush_ms: Option<u64> = None;
    let mut last_flushed_session = stats::SessionTotals::default();
    let mut settings_window = crate::shell::SettingsWindow::new(settings_flags.clone());
    let mut settings_was_visible = false;
    // The most recent focused app key, so settings edges can re-apply per-app
    // gates without waiting for the next Focus event.
    let mut last_app_key: Option<String> = None;
    // Browser host for the focused page, cached per Focus: (app key the read
    // was taken under, extracted host). Populated by the Focus arm's AX read
    // (is_browser-gated, one round-trip per browser focus); host only, never
    // the full URL (privacy boundary). `cached_domain` guards consumption on
    // the app key so a request resolved to a different app never inherits it.
    let mut last_domain: Option<(String, String)> = None;
    // One-shot inert-rules notice: counts browser-focus detection misses
    // (c121 transparency, runtime-contingent since c131).
    let mut domain_miss_notice = DomainMissNotice::default();
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
            latest.clear();
        }
        for event in coalesce_caret_reads(drained.events) {
            if host_event_invalidates_pending_request(&event) {
                latest.clear();
            }
            match event {
                HostEvent::Focus(field) => {
                    let (field, app_key) =
                        canonicalize_field_app(field, |pid| shell.bundle_id_for_pid(pid));
                    eprintln!("compme: focus {}", field.element_id);
                    clear_monitored_state_for_policy_transition(
                        &mut pending_monitored,
                        &mut monitored_buffers,
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
                        prefs.mid_line_enabled(app_key.as_deref(), global_mid_word),
                    );
                    // Per-app Tab disable (§16): suppress the literal-Tab
                    // hotkey for this app's NEXT arm cycle (hotkeys are
                    // transient — armed per visible suggestion).
                    crate::shell::set_tab_hotkey_suppressed(prefs.tab_disabled(app_key.as_deref()));
                    last_app_key = app_key.clone();
                    // Browser-domain detection (c131, slices 2-3 of the c128
                    // design): ONE AX round-trip per browser focus; the
                    // is_browser pre-gate keeps non-browsers at zero AX
                    // traffic. Any miss/failure → None = fail-open. The full
                    // URL dies inside domain_cache_entry; only the host is
                    // kept, and only the host is ever logged (debug only).
                    last_domain = if app_key.as_deref().is_some_and(compat::is_browser) {
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
                        if let Some(msg) = domain_miss_notice
                            .observe(!prefs.excluded_domains.is_empty(), entry.is_some())
                        {
                            eprintln!("compme: {msg}");
                        }
                        entry
                    } else {
                        None
                    };
                    if let Some(app) = app_key {
                        if hinted_apps.insert(app.clone()) {
                            log_compat_guidance(&app);
                        }
                    }
                    current_field = Some(field.clone());
                    tracker.reset();
                    if monitored_memory_active {
                        if let Ok(ctx) = adapter.read_context(&field) {
                            let _ = tracker.observe_with_inserted_text(
                                &field,
                                &ctx,
                                TriggerPolicy::Automatic,
                                now_ms,
                            );
                        }
                    }
                    offer_all(&mut latest, log_err("on_focus", engine.on_focus(field)));
                }
                HostEvent::Caret(field, _rect) => {
                    let (field, app_key) =
                        canonicalize_field_app(field, |pid| shell.bundle_id_for_pid(pid));
                    match adapter.read_context(&field) {
                        // One selection-changed notification covers both typing and a
                        // bare cursor move. Typing schedules a completion; a cursor
                        // move only invalidates a showing ghost (no re-request).
                        Ok(ctx) => {
                            read_err_squelch.reset();
                            current_field = Some(field.clone());
                            if config.diag_coords {
                                if let Ok(rect) = adapter.caret_rect(&field) {
                                    eprintln!(
                                        "compme: diag caret rect={rect:?} scales={:?}",
                                        shell.display_scales()
                                    );
                                }
                            }
                            let observation = if monitored_memory_active {
                                tracker.observe_with_inserted_text(
                                    &field,
                                    &ctx,
                                    TriggerPolicy::Automatic,
                                    now_ms,
                                )
                            } else {
                                tracker.observe(&field, &ctx, TriggerPolicy::Automatic, now_ms)
                            };
                            match observation {
                                Observation::Typed(change) => {
                                    let observe_domain =
                                        domain_observation_enabled(&prefs, &config.personalization);
                                    let domain = enqueue_monitored_change_for_current_domain(
                                        &mut pending_monitored,
                                        &mut last_domain,
                                        &change,
                                        app_key.clone(),
                                        observe_domain,
                                        || adapter.focused_page_url(&field).ok().flatten(),
                                    );
                                    offer_all(
                                        &mut latest,
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
                                        replacement_decision(
                                            &ctx.left,
                                            &config,
                                            &prefs,
                                            app_key.as_deref(),
                                            domain.as_deref(),
                                            flags.enabled.load(Ordering::Relaxed),
                                            now_ms,
                                        )
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
                                        latest.clear();
                                        offer_all(
                                            &mut latest,
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
                                Observation::CaretMoved { field, caret } => offer_all(
                                    &mut latest,
                                    log_err("on_caret_moved", engine.on_caret_moved(field, caret)),
                                ),
                            }
                        }
                        Err(err) => {
                            // Squelched: identical failures repeat at heartbeat
                            // rate while focus sits on an unsupported element.
                            let message = format!("{err:?}");
                            if read_err_squelch.should_log(&message) {
                                eprintln!("compme: read_context: {message}");
                            }
                            // Setup-needed onboarding (A2 §16): a browser/Arc/Dia field
                            // that won't read may need Accessibility/Text-Metrics setup
                            // (the Google-Docs-in-Chrome case). Surface guidance once.
                            if let Some(app) =
                                resolve_app_key(field.pid, |pid| shell.bundle_id_for_pid(pid))
                            {
                                if compat::needs_accessibility_setup(&app, false)
                                    && hinted_apps.insert(format!("setup:{app}"))
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
                    match engine.on_accept(action) {
                        Ok(requests) => {
                            // Absorb the accept's own insertion echo (Word OR
                            // Full) into the diff baseline so the AX readback of
                            // the inserted text registers as a caret move, not new
                            // typing — otherwise the echo would arm a spurious
                            // post-accept completion request (engine-macos §4 step
                            // 9: the accept's own insert is not a new edit).
                            apply_accept_side_effects(
                                true,
                                AcceptSideEffects {
                                    action,
                                    preview: preview.as_ref(),
                                    correction_preview: correction_preview.as_ref(),
                                    wall_ms,
                                    context_max_chars: config.context_max_chars,
                                    previous_inputs: &previous_inputs,
                                    memory: memory.as_ref(),
                                    prefs: &prefs,
                                    tracker: &mut tracker,
                                    usage: &mut usage,
                                },
                            );
                            offer_all(&mut latest, requests);
                        }
                        Err(err) => {
                            // no side effects on failed accept (see apply_accept_side_effects)
                            eprintln!("compme: on_accept error: {err:?}");
                        }
                    }
                }
                HostEvent::Dismiss => {
                    eprintln!("compme: dismiss (Esc)");
                    usage.record(wall_ms, stats::Outcome::Dismissed);
                    offer_all(
                        &mut latest,
                        log_err("on_dismiss_suppress", engine.on_dismiss_suppress()),
                    );
                }
                HostEvent::Cycle => {
                    eprintln!("compme: cycle candidate");
                    offer_all(&mut latest, log_err("on_cycle", engine.on_cycle()));
                }
                HostEvent::Shortcut(action) => match action {
                    ShortcutAction::ForceActivate => {
                        // Settled semantics: re-show the CURRENT pending suggestion
                        // without kicking a fresh inference. `on_force_show`
                        // re-emits the held candidate verbatim (no rotation, no
                        // RequestCompletion); a no-op when nothing is held.
                        eprintln!("compme: shortcut force-activate (re-show pending)");
                        offer_all(
                            &mut latest,
                            log_err("on_force_show", engine.on_force_show()),
                        );
                    }
                    ShortcutAction::ToggleApp => {
                        // Flip per-app Enabled for the focused app, mirroring the
                        // tray/settings per-app toggle. The focused app key comes
                        // from the same resolver the app-disable path uses.
                        match current_field
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
                                    latest.clear();
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
                            &mut pending_monitored,
                            &mut monitored_buffers,
                        );
                        // Disabling must retract any visible suggestion (and disarm
                        // its accept key); the enabled gate is only re-checked at
                        // submission. Mirrors the snooze / tray global-disable paths.
                        if now {
                            latest.clear();
                            let _ = log_err("on_dismiss", engine.on_dismiss());
                        }
                        eprintln!("compme: shortcut toggle-global enabled {now} -> {}", !now);
                    }
                    ShortcutAction::GrammarCheck => {
                        debug_assert_eq!(
                            host_event_route(&HostEvent::Shortcut(ShortcutAction::GrammarCheck)),
                            HostEventRoute::ManualGrammarDetection
                        );
                        let Some(field) = current_field.clone() else {
                            eprintln!("compme: shortcut grammar-check: no focused field");
                            continue;
                        };
                        let outcome = handle_grammar_check_shortcut(GrammarCheckShortcutArgs {
                            current_field: Some(field),
                            config: &config,
                            prefs: &prefs,
                            enabled: flags.enabled.load(Ordering::Relaxed),
                            now_ms,
                            last_domain: &mut last_domain,
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
                            &mut latest,
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
        for outcome in inference.drain_outcomes() {
            if matches!(outcome.request.kind, RequestKind::GrammarFix { .. }) {
                if let Some(latency) =
                    latency_sample(&mut submit_times, outcome.request.generation, now_ms)
                {
                    usage.record_latency(wall_ms, latency);
                }
                match (outcome.correction, outcome.correction_range) {
                    (Some(correction), Some(correction_range)) => {
                        eprintln!(
                            "compme: grammar outcome gen={} correction_present=true",
                            outcome.request.generation
                        );
                        offer_all(
                            &mut latest,
                            log_err(
                                "on_correction",
                                engine.on_correction(
                                    &outcome.request,
                                    correction,
                                    correction_range,
                                ),
                            ),
                        );
                    }
                    _ => eprintln!(
                        "compme: grammar outcome gen={} correction_present=false",
                        outcome.request.generation
                    ),
                }
                continue;
            }
            eprintln!(
                "{}",
                completion_outcome_log_line(outcome.request.generation, &outcome.candidates)
            );
            // First-suggestion latency for this completed request (§11).
            if let Some(latency) =
                latency_sample(&mut submit_times, outcome.request.generation, now_ms)
            {
                usage.record_latency(wall_ms, latency);
            }
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

        // 3b. Drain engine-internal Shown/Superseded events into usage stats
        // (§11/§16): the engine surfaces these; Accepted/Dismissed are recorded
        // from the host inputs above.
        for event in engine.take_stat_events() {
            usage.record(wall_ms, stat_outcome(event));
        }

        // 4. Derive status (permission/secure/ready/enabled) and update the tray.
        // Re-poll secure input and trust on a wall-clock throttle so granting
        // permission or a password field appearing is reflected without a restart.
        if last_secure_poll_ms
            .is_none_or(|last| now_ms.saturating_sub(last) >= SECURE_POLL_INTERVAL_MS)
        {
            secure = shell.secure_input_enabled();
            trusted = shell.accessibility_trusted();
            last_secure_poll_ms = Some(now_ms);
        }
        // SIGUSR1 toggles enable/disable (headless equivalent of the tray item).
        if TOGGLE.swap(false, Ordering::Relaxed) {
            let now = flags.enabled.load(Ordering::Relaxed);
            flags.enabled.store(!now, Ordering::Relaxed);
            clear_monitored_state_for_policy_transition(
                &mut pending_monitored,
                &mut monitored_buffers,
            );
            // Disabling must retract any visible suggestion (and disarm its accept
            // key); the enabled gate is only re-checked at submission.
            if now {
                latest.clear();
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
                &mut pending_monitored,
                &mut monitored_buffers,
            );
            if apply_global_disable(arm, &mut prefs, now_ms) {
                flags.enabled.store(false, Ordering::Relaxed);
                eprintln!("compme: completions disabled (persistent)");
            } else {
                eprintln!("compme: completions snoozed globally ({arm:?})");
                latest.clear();
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
                &mut pending_monitored,
                &mut monitored_buffers,
            );
            // A snooze must retract an already-visible ghost, exactly like the
            // disable edge below: gating runs at request-submission, so without
            // this a ghost already on screen would survive the snooze — and its
            // armed accept key would still insert it (a2-parity review #2).
            latest.clear();
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
                    &usage,
                    wall_ms,
                    settings_flags.stat_range_index.load(Ordering::Relaxed),
                    settings_flags.stat_group_index.load(Ordering::Relaxed),
                );
                // Grow-only session totals, NOT window-derived counts: past
                // 30 days the window prunes and the row would regress — and
                // it must agree with what the periodic flush writes to disk.
                let totals = usage.session_totals();
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
                model_download_status.as_deref(),
            );
            last_setup_poll_ms = Some(now_ms);
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
                (*lines, apps_ids) = compose_apps_rows(memory.as_ref());
            }
            // Publish the per-row policy bits alongside apps_lines (same order/
            // cap) so the Apps-pane checkboxes open reflecting the saved per-app
            // override instead of a hard-seeded OFF.
            *settings_flags
                .apps_policy_bits
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = compose_apps_policy_bits(
                &prefs,
                &apps_ids,
                global_mid_word,
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
        if crate::shell::policy_restore_needed(settings_was_visible, settings_visible) {
            if let Err(err) = settings_window.restore_accessory_policy() {
                eprintln!("compme: activation policy restore failed: {err}");
            }
        }
        settings_was_visible = settings_visible;
        // Setup buttons (tray-flags pattern): consume edges, perform the
        // privileged calls here on the main thread.
        if settings_flags.setup_grant_ax.swap(false, Ordering::Relaxed) {
            shell.prompt_accessibility_trust();
        }
        if settings_flags
            .setup_request_screen
            .swap(false, Ordering::Relaxed)
        {
            if should_request_screen_recording(
                config.screen_context,
                shell.screen_capture_permission(),
            ) {
                shell.request_screen_capture_permission();
            } else {
                eprintln!("compme: screen recording request ignored; screen context is off or already granted");
            }
        }
        if settings_flags
            .setup_reveal_model
            .swap(false, Ordering::Relaxed)
        {
            // Absolutize: NSURL fileURLWithPath resolves relative paths
            // against the CWD, which is / for a bundle launch (review-c107;
            // same class as the banked D14 default-path item).
            let model_abs = if config.model_path.is_absolute() {
                config.model_path.clone()
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(&config.model_path))
                    .unwrap_or_else(|_| config.model_path.clone())
            };
            if let Err(err) = shell.reveal_file(&model_abs) {
                eprintln!("compme: reveal model failed: {err:?}");
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
                    let _ = std::fs::create_dir_all(&dir);
                    if let Err(err) = shell.open_url(&dir.to_string_lossy()) {
                        eprintln!("compme: open models folder failed: {err:?}");
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
        // Apps-row Delete: resolve the clicked row index against the ids
        // rendered with the SAME cap/order, delete, recompose, re-render.
        let clicked_row = settings_flags
            .apps_delete_row
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(row) = clicked_row {
            if let (Some(store), Some(app)) = (&memory, apps_ids.get(row)) {
                // Irreversible (secure_delete zeroes freed pages) — confirm
                // first, Cancel-default (review-c112; deep-link precedent).
                let confirmed = shell
                    .confirm(&platform::shell::ConfirmPrompt {
                        title: "Delete recorded inputs?",
                        message: &format!(
                            "All recorded inputs for {app} will be permanently erased."
                        ),
                        confirm_label: "Delete",
                    })
                    .unwrap_or(false);
                if !confirmed {
                    eprintln!("compme: delete for {app} cancelled");
                } else if let Some((lines, ids)) =
                    delete_app_row_and_recompose(store, &apps_ids, row)
                {
                    // Poison-recovery: skipping would leave the Apps pane
                    // showing the just-deleted row (refresh runs below).
                    *settings_flags
                        .apps_lines
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = lines;
                    apps_ids = ids;
                    // Rows shifted — republish the policy bits in the new order
                    // before refresh_apps_labels re-seeds the checkboxes from them.
                    *settings_flags
                        .apps_policy_bits
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = compose_apps_policy_bits(
                        &prefs,
                        &apps_ids,
                        global_mid_word,
                        config.autocorrect,
                        config.grammar_fix,
                    );
                    settings_window.refresh_apps_labels();
                }
            }
        }
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
            if let (Some(app), Some(field)) =
                (apps_ids.get(row), apps_policy_field_from_index(field_index))
            {
                prefs.set_app_policy_field(app, field, on);
                eprintln!("compme: app policy {field:?} for {app} set to {on}");
                if let Some(path) = config::config_file_path() {
                    persist_web_override_prefs(&path, &prefs);
                }
                // Disabling the FOCUSED app must retract any suggestion already on
                // screen (and disarm its accept key); the submit gate only blocks
                // future submits, not an already-dispatched render. Mirrors the
                // shortcut/snooze/global-disable edges. Gated on the edited app
                // being the focused one so editing another app's row never
                // dismisses the focused field's ghost.
                let focused_app = current_field
                    .as_ref()
                    .and_then(|f| effective_app_key(f, |pid| shell.bundle_id_for_pid(pid)));
                if apps_edit_dismisses_focused(field, on, focused_app.as_deref(), app) {
                    latest.clear();
                    let _ = log_err("on_dismiss", engine.on_dismiss());
                }
            }
        }
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
        // Setup "Download Model": fetch the model the picker has selected
        // (setup_model_index; defaults to the recommended entry) into the
        // app-support models dir. Progress is logged; on Done the log says
        // how to point COMPME_MODEL_PATH at it.
        if settings_flags
            .setup_download_model
            .swap(false, Ordering::Relaxed)
            && download_idle(model_download_status.as_deref())
        {
            if let Some(home) = std::env::var_os("HOME") {
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
                            .confirm(&platform::shell::ConfirmPrompt {
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
                                eprintln!(
                                    "compme: failed to persist COMPME_LICENSE_ACCEPTED: {err}"
                                );
                            }
                        }
                        eprintln!(
                            "compme: {} accepted for {}",
                            accepted.license_name, accepted.model
                        );
                    }
                    let dest = std::path::PathBuf::from(home)
                        .join("Library/Application Support/compme/models")
                        .join(format!("{}.gguf", entry.name));
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
                        downloader: &mut model_downloader,
                        model_download_status: &mut model_download_status,
                        model_download_logged: &mut model_download_logged,
                        prepare: prepare_model_download_dest,
                        existing_model: model_download_dest_present,
                        spawn: || {
                            model_fetch::ModelDownloader::spawn().map_err(|err| err.to_string())
                        },
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
                // The click was already consumed by the swap above; without HOME
                // there is no app-support dir to download into, so log the no-op
                // rather than dropping the press silently.
                eprintln!("compme: download-model click ignored \u{2014} HOME is not set");
            }
        }
        // Download progress/terminal-state logging (one line per transition).
        if let Some(status) = &model_download_status {
            let state = status.state.lock().unwrap_or_else(|e| e.into_inner());
            let (next_logged, line) = download_log_transition(&state, model_download_logged);
            // The Done edge (logged advances to 2 with a Done state) fires once
            // per download — start_model_download_edge resets logged to 0 on
            // each new queue — so a second download re-persists its own path.
            let done_edge = next_logged != model_download_logged
                && matches!(&*state, model_fetch::DownloadState::Done(_));
            model_download_logged = next_logged;
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
        // Periodic lifetime-stats flush (c102): baseline + grow-only session
        // totals, idempotent overwrite. The dirty check keeps idle ticks off
        // the disk; on a failed write the timestamp still advances (no
        // per-heartbeat hammering of a broken disk) but the dirty marker
        // does not, so the next interval retries.
        let session_totals = usage.session_totals();
        if stats_flush_due(last_stats_flush_ms, now_ms) && session_totals != last_flushed_session {
            last_stats_flush_ms = Some(now_ms);
            match persist_lifetime_stats(stats_path.as_deref(), &lifetime_base, session_totals) {
                Ok(()) => last_flushed_session = session_totals,
                Err(err) => eprintln!("compme: stats persist failed: {err}"),
            }
        }
        // Visible-only Setup re-probe: granting a permission while the
        // window stays open flips its row within ~480ms.
        if setup_poll_due(settings_visible, last_setup_poll_ms, now_ms) {
            last_setup_poll_ms = Some(now_ms);
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
                model_download_status.as_deref(),
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
                    latest.clear();
                    let _ = log_err("on_dismiss", engine.on_dismiss());
                }
            },
        );
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
            &mut global_mid_word,
            &prefs,
            last_app_key.as_deref(),
            |on| engine.set_allow_mid_word(on),
            |on| persist_and_log_switch("COMPME_MIDLINE", "mid-line completions", on),
        );
        // Context-pane watchers. Clipboard context applies live because submit
        // reads `config.clipboard_context` for each request. Screen OCR also
        // applies live: enabling starts the worker when Screen Recording is
        // granted, and disabling drops it plus clears the worker-side wait.
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
                model_download_status.as_deref(),
            );
            settings_window.refresh_setup_labels();
        }
        // Emoji-pane watcher: the replacement path reads config.emoji on each
        // observation, so changing the Option is the live apply. Keep the parsed
        // prefs payload across live off/on cycles; the skin-tone popup updates
        // it below, and gender remains config-backed until its control ships.
        let emoji_edge = handle_emoji_switch_edge(
            &settings_flags.emoji_enabled,
            &mut emoji_enabled,
            &mut config.emoji,
            &mut emoji_prefs,
            |on| persist_and_log_switch("COMPME_EMOJI", "emoji completions", on),
        );
        if emoji_edge == Some(false) {
            latest.clear();
            let _ = log_err("on_dismiss", engine.on_dismiss());
        }
        handle_emoji_skin_tone_change_with_invalidation(
            &settings_flags.emoji_skin_tone_index,
            &mut emoji_skin_tone_index,
            &mut config.emoji,
            &mut emoji_prefs,
            |value| persist_and_log_value("COMPME_EMOJI_SKIN_TONE", "emoji skin tone", value),
            || {
                latest.clear();
                let _ = log_err("on_dismiss", engine.on_dismiss());
            },
        );
        handle_emoji_gender_change_with_invalidation(
            &settings_flags.emoji_gender_index,
            &mut emoji_gender_index,
            &mut config.emoji,
            &mut emoji_prefs,
            |value| persist_and_log_value("COMPME_EMOJI_GENDER", "emoji gender", value),
            || {
                latest.clear();
                let _ = log_err("on_dismiss", engine.on_dismiss());
            },
        );
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
                    .confirm(&platform::shell::ConfirmPrompt {
                        title: "Allow configuration change?",
                        message: &format!(
                            "A compme:// link wants to apply {action} for:\n{scope}\n({trust})"
                        ),
                        confirm_label: "Allow",
                    })
                    .unwrap_or(false)
            };
            match handle_deep_link(&url, config.trusted_key.as_ref(), &mut prefs, confirm) {
                Ok(summary) => {
                    eprintln!("compme: deep link {summary}");
                    clear_monitored_state_for_policy_transition(
                        &mut pending_monitored,
                        &mut monitored_buffers,
                    );
                    if let Some(path) = config::config_file_path() {
                        persist_web_override_prefs(&path, &prefs);
                    }
                    latest.clear();
                    let _ = log_err("on_dismiss", engine.on_dismiss());
                }
                Err(err) => eprintln!("compme: deep link rejected: {err}"),
            }
        }
        // Tray "Toggle Input Collection in Current App": flip the frontmost
        // app's typing-history override and persist the no-collect list. No
        // dismiss edge — collection gates RECORDING, not suggestion display.
        if flags.collection_toggle.swap(false, Ordering::Relaxed) {
            clear_monitored_state_for_policy_transition(
                &mut pending_monitored,
                &mut monitored_buffers,
            );
            match current_field
                .as_ref()
                .and_then(|f| effective_app_key(f, |pid| shell.bundle_id_for_pid(pid)))
            {
                Some(app) => {
                    let allowed = toggle_app_collection(&mut prefs, &app);
                    eprintln!(
                        "compme: input collection in {app} now {}",
                        if allowed { "ENABLED" } else { "DISABLED" }
                    );
                    if let Some(path) = config::config_file_path() {
                        // Mirror persist_web_override_prefs: an emptied list is
                        // REMOVED, not written as a blank `KEY=` line (which would
                        // shadow the env-over-file layer). Re-enabling the last
                        // no-collect app clears the key entirely.
                        let value = no_collect_apps_value(&prefs);
                        if value.is_empty() {
                            remove_setting_or_log(
                                &path,
                                "COMPME_NO_COLLECT_APPS",
                                "no-collect apps",
                            );
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
                &mut pending_monitored,
                &mut monitored_buffers,
            );
            match current_field
                .as_ref()
                .and_then(|f| effective_app_key(f, |pid| shell.bundle_id_for_pid(pid)))
            {
                Some(app) => {
                    apply_app_disable(arm, &app, &mut prefs, now_ms);
                    eprintln!("compme: completions disabled in {app} ({arm:?})");
                    if arm == DisableArm::Always {
                        if let Some(path) = config::config_file_path() {
                            if let Err(err) = config::persist_setting(
                                &path,
                                "COMPME_EXCLUDED_APPS",
                                &excluded_apps_value(&prefs),
                            ) {
                                eprintln!("compme: could not persist excluded apps: {err}");
                            }
                        }
                    }
                    latest.clear();
                    let _ = log_err("on_dismiss", engine.on_dismiss());
                }
                None => eprintln!("compme: disable-in-app ignored — no focused app to resolve"),
            }
        }
        let effective_trusted = runtime_trusted(trusted, subscriptions_require_relaunch);
        let enabled = flags.enabled.load(Ordering::Relaxed);
        flush_monitored_changes_after_secure_recheck(
            &mut pending_monitored,
            &mut monitored_buffers,
            memory.as_ref(),
            &prefs,
            MonitoredFlushState {
                secure: &mut secure,
                last_secure_poll_ms: &mut last_secure_poll_ms,
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
            secure,
            model_available,
            inference.is_ready(),
            enabled,
        );
        // Secure input is a true engine-state transition, not only a UI state:
        // clear queued work and invalidate the machine so held requests cannot
        // submit after the secure block clears.
        match secure_edge(prev_secure, secure, effective_trusted) {
            SecureEdge::Enter => {
                clear_monitored_state_for_policy_transition(
                    &mut pending_monitored,
                    &mut monitored_buffers,
                );
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
            clear_monitored_state_for_policy_transition(
                &mut pending_monitored,
                &mut monitored_buffers,
            );
            latest.clear();
            let _ = log_err("on_dismiss", engine.on_dismiss());
        }
        if status_drops_pending_requests(status) {
            clear_monitored_state_for_policy_transition(
                &mut pending_monitored,
                &mut monitored_buffers,
            );
            latest.clear();
        }
        // Persist a user enable/disable toggle (tray or SIGUSR1) so the next
        // launch starts in the same state (A3 settings persistence). Skipped on
        // the first iteration (prev starts equal to the configured value) and
        // never fatal — a read-only disk only costs persistence, not operation.
        if prev_enabled != enabled {
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
        prev_enabled = enabled;
        prev_secure = secure;
        // Only touch AppKit when the rendered state actually changed. The
        // snoozed flag is part of the render state so the title/line flip both
        // when a snooze starts AND when it auto-expires mid-Ready.
        let snoozed = prefs.is_snoozed(now_ms);
        if last_render != Some((status, enabled, snoozed)) {
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
            last_render = Some((status, enabled, snoozed));
        }
        // Menu-bar 30-day usage line (§11). The string only changes when a
        // stat event landed or the window rolled, so the compare keeps AppKit
        // untouched on idle heartbeats.
        if let Some(tray) = &tray {
            let stats_line = usage.summary_line(wall_ms);
            if last_stats_line.as_deref() != Some(stats_line.as_str()) {
                if let Err(err) = tray.set_stats_line(&stats_line) {
                    eprintln!("compme: tray stats update failed: {err:?}");
                }
                last_stats_line = Some(stats_line);
            }
        }

        // 5. Submit the newest pending request only when suggestions are allowed
        // (Ready ⇒ trusted + not secure + warm + enabled).
        if host_event_backlog_remaining {
            latest.clear();
            if manual_grammar_request.take().is_some() {
                eprintln!("compme: shortcut grammar-check dropped — host event backlog");
            }
        } else if status.suggestions_allowed() {
            if let Some(request) = manual_grammar_request.take() {
                let app_key = effective_app_key(&request.field, |pid| shell.bundle_id_for_pid(pid));
                let domain = cached_domain(&last_domain, app_key.as_deref());
                if request_passes_submit_gates(&request, app_key.as_deref(), domain, &prefs, now_ms)
                {
                    let log_context = RequestLogContext {
                        app_key,
                        domain: domain.map(str::to_owned),
                        prefs: prefs.clone(),
                        acceptance_prompt_marker: config.acceptance_prompt_marker.clone(),
                    };
                    let submitted_line = submit_request_and_track(
                        &mut submit_times,
                        request,
                        now_ms,
                        log_context,
                        |request| inference.submit(request),
                    );
                    eprintln!("{submitted_line}");
                } else {
                    eprintln!(
                        "{}",
                        request_log_line(
                            &request,
                            app_key.as_deref(),
                            domain,
                            &prefs,
                            now_ms,
                            config.acceptance_prompt_marker.as_deref(),
                            true,
                        )
                    );
                }
                latest.clear();
            }
            if let Some(request) = latest.take() {
                // Per-app/domain gating + pause/snooze (A2 §8). The exclude list
                // is keyed on bundle ids. Prefer a fresh pid resolution, but keep
                // the already-canonical request field app as the stable fallback;
                // a transient lookup miss must not fail-open per-app privacy gates.
                // The domain comes from the Focus arm's cache, guarded on the same
                // app key (c131).
                let app_key = effective_app_key(&request.field, |pid| shell.bundle_id_for_pid(pid));
                if request_passes_submit_gates(
                    &request,
                    app_key.as_deref(),
                    cached_domain(&last_domain, app_key.as_deref()),
                    &prefs,
                    now_ms,
                ) {
                    let domain = cached_domain(&last_domain, app_key.as_deref()).map(str::to_owned);
                    let log_context = RequestLogContext {
                        app_key,
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
                            submit_times: &mut submit_times,
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
                        request_log_line(
                            &request,
                            app_key.as_deref(),
                            cached_domain(&last_domain, app_key.as_deref()),
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
        handle_check_updates_flag(&flags.check_updates, |url| {
            if let Err(err) = shell.open_url(url) {
                eprintln!("compme: open updates failed: {err}");
            }
        });
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
    let session_usage = session_usage_snapshot(&usage, final_wall_ms);
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
        usage.session_totals(),
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
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    struct ShortcutBindingsGuard(crate::shell::ShortcutBindings);

    impl ShortcutBindingsGuard {
        fn reset() -> Self {
            let previous = crate::shell::effective_shortcut_bindings();
            crate::shell::set_shortcut_bindings_from_config(None, None, None, None);
            Self(previous)
        }
    }

    impl Drop for ShortcutBindingsGuard {
        fn drop(&mut self) {
            crate::shell::set_shortcut_bindings(self.0);
        }
    }

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
            ("COMPME_INSTRUCTIONS", "Be terse."),
            ("COMPME_STRENGTH", "5"),
            ("COMPME_SENDER_NAME", "Ada"),
        ]));
        assert_eq!(profile.strength, Strength::Max);
        let preamble = profile.build_preamble(Some("com.apple.TextEdit"), None);
        assert!(preamble.contains("Be terse."));
        assert!(preamble.contains("Ada"));
    }

    #[test]
    fn sender_email_is_parsed_into_profile_and_templated_into_preamble() {
        // COMPME_SENDER_EMAIL flows into profile.sender.email and is templated
        // into the steering preamble (the sender line) so the model can address
        // the writer correctly.
        let profile = build_personalization(&lookup(&[
            ("COMPME_INSTRUCTIONS", "Be terse."),
            ("COMPME_SENDER_EMAIL", "ada@example.com"),
        ]));
        assert_eq!(profile.sender.email, "ada@example.com");
        let preamble = profile.build_preamble(Some("com.apple.TextEdit"), None);
        assert!(
            preamble.contains("ada@example.com"),
            "sender email must appear in the built preamble: {preamble:?}"
        );
    }

    #[test]
    fn personalization_edit_rejoins_each_knob_onto_the_profile_and_persist_pair() {
        // Each PersonalizationEdit variant the Settings pane records must land on
        // the right profile field AND return the matching (env_key, value) so the
        // run loop persists what it applied. Covers the three steering knobs.
        use crate::shell::PersonalizationEdit as E;
        let mut profile = PersonalizationProfile::default();

        let (key, val) =
            apply_personalization_edit(&mut profile, E::GlobalInstructions("Be terse.".into()));
        assert_eq!(profile.global_instructions, "Be terse.");
        assert_eq!((key, val.as_str()), ("COMPME_INSTRUCTIONS", "Be terse."));

        let (key, val) = apply_personalization_edit(&mut profile, E::SenderName("Ada".into()));
        assert_eq!(profile.sender.name, "Ada");
        assert_eq!((key, val.as_str()), ("COMPME_SENDER_NAME", "Ada"));

        let (key, val) =
            apply_personalization_edit(&mut profile, E::SenderEmail("ada@x.io".into()));
        assert_eq!(profile.sender.email, "ada@x.io");
        assert_eq!((key, val.as_str()), ("COMPME_SENDER_EMAIL", "ada@x.io"));

        // Strength stop addresses STOPS by index and round-trips through
        // from_stop; the persisted value is the stop number, matching how
        // build_personalization parses COMPME_STRENGTH.
        let (key, val) = apply_personalization_edit(&mut profile, E::StrengthStop(5));
        assert_eq!(profile.strength, Strength::from_stop(5));
        assert_eq!((key, val.as_str()), ("COMPME_STRENGTH", "5"));
        assert_eq!(personalization_strength_index(profile.strength), 5);

        // Out-of-range stop is total (clamped via from_stop), never panics.
        let (_key, val) = apply_personalization_edit(&mut profile, E::StrengthStop(9_999));
        assert_eq!(profile.strength, Strength::from_stop(255));
        assert_eq!(val, "255");

        // Titles cover every stop in order, so the popup index always addresses
        // a real Strength.
        assert_eq!(
            personalization_strength_titles().len(),
            Strength::STOPS.len()
        );
    }

    #[test]
    fn live_personalization_edit_updates_worker_profile_and_persists_value() {
        use crate::shell::PersonalizationEdit as E;
        let mut profile = PersonalizationProfile::default();
        let applied_profile = RefCell::new(None);
        let persisted = RefCell::new(None);

        let (key, value, persist_result) = apply_live_personalization_edit(
            &mut profile,
            E::GlobalInstructions("Use short direct completions.".into()),
            |profile| *applied_profile.borrow_mut() = Some(profile),
            |key, value| {
                *persisted.borrow_mut() = Some((key, value.to_string()));
                Ok(())
            },
        );

        assert!(persist_result.is_ok());
        assert_eq!(key, "COMPME_INSTRUCTIONS");
        assert_eq!(value, "Use short direct completions.");
        assert_eq!(profile.global_instructions, "Use short direct completions.");
        assert_eq!(
            applied_profile
                .borrow()
                .as_ref()
                .unwrap()
                .global_instructions,
            "Use short direct completions."
        );
        assert_eq!(
            persisted.into_inner(),
            Some((
                "COMPME_INSTRUCTIONS",
                "Use short direct completions.".to_string()
            ))
        );
    }

    fn test_screen_context() -> ScreenContext {
        ScreenContext {
            field: host_field("screen-field"),
            generation: 1,
            snapshot: 2,
            text: "screen text".into(),
        }
    }

    #[test]
    fn context_toggle_off_clears_clipboard_screen_worker_and_wait() {
        let clipboard_cell = Mutex::new(Some("clipboard text".to_string()));
        apply_clipboard_context_edge(false, &clipboard_cell);
        assert_eq!(*clipboard_cell.lock().unwrap(), None);

        let mut config_screen_context = false;
        let ui_flag = AtomicBool::new(false);
        let screen_cell = Mutex::new(Some(test_screen_context()));
        let mut screen_ocr = Some("worker");
        let wait_ms = RefCell::new(Vec::new());

        let edge = apply_screen_context_edge(
            false,
            ScreenContextToggleState {
                config_screen_context: &mut config_screen_context,
                ui_flag: &ui_flag,
                screen_cell: &screen_cell,
                screen_ocr: &mut screen_ocr,
            },
            |ms| wait_ms.borrow_mut().push(ms),
            || true,
            || Ok("new-worker"),
        );

        assert_eq!(edge, ScreenContextEdge::Disabled);
        assert_eq!(*screen_cell.lock().unwrap(), None);
        assert_eq!(screen_ocr, None);
        assert_eq!(wait_ms.into_inner(), vec![0]);
    }

    #[test]
    fn screen_context_enable_reverts_false_when_permission_denied_or_spawn_fails() {
        let denied_flag = AtomicBool::new(true);
        let denied_cell = Mutex::new(None);
        let mut denied_context = true;
        let mut denied_ocr: Option<&str> = Some("old-worker");
        let denied_wait = RefCell::new(Vec::new());

        let denied = apply_screen_context_edge(
            true,
            ScreenContextToggleState {
                config_screen_context: &mut denied_context,
                ui_flag: &denied_flag,
                screen_cell: &denied_cell,
                screen_ocr: &mut denied_ocr,
            },
            |ms| denied_wait.borrow_mut().push(ms),
            || false,
            || Ok("new-worker"),
        );
        let denied_persist_value = denied_context;

        assert_eq!(denied, ScreenContextEdge::RevertedDenied);
        assert!(!denied_context);
        assert!(!denied_persist_value);
        assert!(!denied_flag.load(Ordering::Relaxed));
        assert_eq!(denied_ocr, None);
        assert_eq!(denied_wait.into_inner(), vec![0]);

        let failed_flag = AtomicBool::new(true);
        let failed_cell = Mutex::new(None);
        let mut failed_context = true;
        let mut failed_ocr: Option<&str> = Some("old-worker");
        let failed_wait = RefCell::new(Vec::new());

        let failed = apply_screen_context_edge(
            true,
            ScreenContextToggleState {
                config_screen_context: &mut failed_context,
                ui_flag: &failed_flag,
                screen_cell: &failed_cell,
                screen_ocr: &mut failed_ocr,
            },
            |ms| failed_wait.borrow_mut().push(ms),
            || true,
            || Err("spawn failed".to_string()),
        );
        let failed_persist_value = failed_context;

        assert_eq!(failed, ScreenContextEdge::RevertedSpawnFailed);
        assert!(!failed_context);
        assert!(!failed_persist_value);
        assert!(!failed_flag.load(Ordering::Relaxed));
        assert_eq!(failed_ocr, None);
        assert_eq!(failed_wait.into_inner(), vec![0]);
    }

    #[test]
    fn screen_context_enable_starts_worker_and_sets_wait() {
        let ui_flag = AtomicBool::new(true);
        let screen_cell = Mutex::new(None);
        let mut config_screen_context = true;
        let mut screen_ocr: Option<&str> = None;
        let wait_ms = RefCell::new(Vec::new());

        let edge = apply_screen_context_edge(
            true,
            ScreenContextToggleState {
                config_screen_context: &mut config_screen_context,
                ui_flag: &ui_flag,
                screen_cell: &screen_cell,
                screen_ocr: &mut screen_ocr,
            },
            |ms| wait_ms.borrow_mut().push(ms),
            || true,
            || Ok("new-worker"),
        );

        assert_eq!(edge, ScreenContextEdge::Enabled);
        assert!(config_screen_context);
        assert!(ui_flag.load(Ordering::Relaxed));
        assert_eq!(*screen_cell.lock().unwrap(), None);
        assert_eq!(screen_ocr, Some("new-worker"));
        assert_eq!(wait_ms.into_inner(), vec![SCREEN_CONTEXT_WAIT_MS]);
    }

    #[test]
    fn strength_falls_back_to_default_when_compme_strength_is_unparseable() {
        // COMPME_STRENGTH is present but cannot parse as u8: a non-numeric value
        // and a numeric value that overflows u8 both leave the default stop in
        // place (parse fails => the `if let Some` branch is skipped).
        let default_strength = PersonalizationProfile::default().strength;

        let non_numeric = build_personalization(&lookup(&[("COMPME_STRENGTH", "abc")]));
        assert_eq!(non_numeric.strength, default_strength);

        let overflows_u8 = build_personalization(&lookup(&[("COMPME_STRENGTH", "999")]));
        assert_eq!(overflows_u8.strength, default_strength);
    }

    #[test]
    fn instruction_suffix_folds_non_ascii_to_underscore() {
        // Every non-ASCII-alphanumeric char (including a multi-byte unicode char)
        // folds to a single '_'; ASCII alphanumerics are uppercased. For "café"
        // the 'é' becomes '_', yielding "CAF_".
        assert_eq!(config_target_key_suffix("café"), "CAF_");
    }

    #[test]
    fn personalization_built_from_per_app_and_domain_config_keys() {
        let profile = build_personalization(&lookup(&[
            ("COMPME_INSTRUCTIONS", "Be terse."),
            (
                "COMPME_INSTRUCTIONS_APPS",
                "com.apple.TextEdit, com.apple.Notes, com.missing.App",
            ),
            (
                "COMPME_INSTRUCTIONS_APP_COM_APPLE_TEXTEDIT",
                "Use a plain-text tone.",
            ),
            (
                "COMPME_INSTRUCTIONS_APP_COM_APPLE_NOTES",
                "Prefer note bullets.",
            ),
            (
                "COMPME_INSTRUCTIONS_DOMAINS",
                "Docs.Google.com, mail.example",
            ),
            (
                "COMPME_INSTRUCTIONS_DOMAIN_DOCS_GOOGLE_COM",
                "Prefer document context.",
            ),
        ]));

        assert_eq!(
            profile.per_app.get("com.apple.TextEdit"),
            Some(&"Use a plain-text tone.".to_string())
        );
        assert_eq!(
            profile.per_app.get("com.apple.Notes"),
            Some(&"Prefer note bullets.".to_string())
        );
        assert!(
            !profile.per_app.contains_key("com.missing.App"),
            "listed apps without instruction values should not create empty entries"
        );
        assert_eq!(
            profile.per_domain.get("docs.google.com"),
            Some(&"Prefer document context.".to_string())
        );
        assert!(
            !profile.per_domain.contains_key("mail.example"),
            "listed domains without instruction values should not create empty entries"
        );

        let preamble = profile.build_preamble(Some("com.apple.TextEdit"), Some("docs.google.com"));
        assert!(preamble.contains("Be terse."));
        assert!(preamble.contains("Use a plain-text tone."));
        assert!(preamble.contains("Prefer document context."));
        assert!(!preamble.contains("Prefer note bullets."));
    }

    #[test]
    fn personalization_per_domain_steers_a_subdomain_through_the_assembled_profile() {
        // End-to-end app wiring of the round-1 subdomain matcher: a `google.com`
        // rule from config must steer `www.google.com` (the host is lowercased and
        // matched on a dot boundary), but never a look-alike `evilgoogle.com`.
        // This pins the app-level lowercasing + subdomain seam, not just the
        // personalization crate's resolve_instructions.
        let profile = build_personalization(&lookup(&[
            ("COMPME_STRENGTH", "5"),
            ("COMPME_INSTRUCTIONS_DOMAINS", "Google.com"),
            (
                "COMPME_INSTRUCTIONS_DOMAIN_GOOGLE_COM",
                "Prefer search-friendly phrasing.",
            ),
        ]));

        // Config domain was lowercased into the profile key.
        assert_eq!(
            profile.per_domain.get("google.com"),
            Some(&"Prefer search-friendly phrasing.".to_string())
        );

        // A subdomain of the rule is steered.
        let on_subdomain = profile.build_preamble(None, Some("www.google.com"));
        assert!(
            on_subdomain.contains("Prefer search-friendly phrasing."),
            "subdomain www.google.com should match the google.com rule: {on_subdomain:?}"
        );

        // A look-alike host on a non-dot boundary is NOT steered.
        let on_lookalike = profile.build_preamble(None, Some("evilgoogle.com"));
        assert!(
            !on_lookalike.contains("Prefer search-friendly phrasing."),
            "evilgoogle.com must not match the google.com rule: {on_lookalike:?}"
        );
    }

    #[test]
    fn personalization_skips_ambiguous_per_target_instruction_keys() {
        let profile = build_personalization(&lookup(&[
            (
                "COMPME_INSTRUCTIONS_APPS",
                "com.example.Editor, com-example-Editor",
            ),
            (
                "COMPME_INSTRUCTIONS_APP_COM_EXAMPLE_EDITOR",
                "Use editor-specific style.",
            ),
            (
                "COMPME_INSTRUCTIONS_DOMAINS",
                "docs.google.com, docs-google-com",
            ),
            (
                "COMPME_INSTRUCTIONS_DOMAIN_DOCS_GOOGLE_COM",
                "Use docs-specific style.",
            ),
        ]));

        assert!(
            profile.per_app.is_empty(),
            "colliding app suffixes must not apply one value to multiple apps"
        );
        assert!(
            profile.per_domain.is_empty(),
            "colliding domain suffixes must not apply one value to multiple domains"
        );
    }

    #[test]
    fn personalization_skips_blank_per_target_instruction_values() {
        // A listed target whose value KEY is present but blank/whitespace-only
        // is a present-but-empty instruction: `instruction_map_from_config`
        // trims it, sees it empty, and skips it — no empty entry is stored.
        // (`com.missing.App` confirms the absent-key path still skips too.)
        let profile = build_personalization(&lookup(&[
            (
                "COMPME_INSTRUCTIONS_APPS",
                "com.apple.TextEdit, com.apple.Notes, com.missing.App",
            ),
            // Present but whitespace-only → must be skipped.
            ("COMPME_INSTRUCTIONS_APP_COM_APPLE_TEXTEDIT", "   "),
            // A real value confirms the non-blank path still stores.
            (
                "COMPME_INSTRUCTIONS_APP_COM_APPLE_NOTES",
                "Prefer note bullets.",
            ),
        ]));

        assert!(
            !profile.per_app.contains_key("com.apple.TextEdit"),
            "a whitespace-only instruction value must not be stored as an empty instruction"
        );
        assert!(
            !profile.per_app.contains_key("com.missing.App"),
            "a listed target with no value key stays absent"
        );
        assert_eq!(
            profile.per_app.get("com.apple.Notes"),
            Some(&"Prefer note bullets.".to_string()),
            "a non-blank value is still stored alongside the skipped blank one"
        );
    }

    #[test]
    fn personalization_defaults_to_no_steer_when_keys_absent() {
        let profile = build_personalization(&lookup(&[]));
        assert_eq!(profile.build_preamble(Some("com.apple.TextEdit"), None), "");
    }

    #[test]
    fn request_log_does_not_emit_prompt_text() {
        let request = CompletionRequest {
            generation: 42,
            field: field_with_app("com.apple.TextEdit"),
            domain: None,
            snapshot: 42,
            prompt: "secret prompt with ada@example.com".into(),
            max_tokens: 24,
            kind: RequestKind::Completion,
        };
        let prefs = Prefs::default();
        let line = request_log_line(
            &request,
            Some("com.apple.TextEdit"),
            None,
            &prefs,
            1_000,
            Some("ada@example.com"),
            false,
        );
        assert!(
            line.contains("request gen=42 prompt_chars=34 app=com.apple.TextEdit"),
            "request logs should expose only prompt length and gate metadata: {line}"
        );
        assert!(line.contains("app_allows=true"));
        assert!(line.contains("terminal_ok=true"));
        assert!(line.contains("domain_ready=true"));
        assert!(line.contains("prefs_ok=true"));
        assert!(line.contains("prompt_marker=true"));
        assert!(!line.contains("secret"));
        assert!(!line.contains("ada@example.com"));
        assert!(!line.contains("prompt with"));
    }

    #[test]
    fn prefs_built_from_excluded_apps_list() {
        let prefs = build_prefs(&lookup(&[(
            "COMPME_EXCLUDED_APPS",
            "com.apple.Finder, com.tinyspeck.slackmacgap",
        )]));
        assert!(!prefs.should_suggest(Some("com.apple.Finder"), None, 0));
        assert!(!prefs.should_suggest(Some("com.tinyspeck.slackmacgap"), None, 0));
        assert!(prefs.should_suggest(Some("com.apple.TextEdit"), None, 0));
    }

    #[test]
    fn prefs_builds_web_override_policy_from_config_lists() {
        let prefs = build_prefs(&lookup(&[
            ("COMPME_EXCLUDED_DOMAINS", "Docs.Google.com, bank.example"),
            (
                "COMPME_ENABLED_APPS",
                "com.example.enabled, com.example.conflict",
            ),
            (
                "COMPME_DISABLED_APPS",
                "com.example.disabled, com.example.conflict",
            ),
        ]));

        assert!(prefs.excluded_domains.contains("docs.google.com"));
        assert!(prefs.excluded_domains.contains("bank.example"));
        assert_eq!(prefs.per_app["com.example.enabled"].enabled, Some(true));
        assert_eq!(prefs.per_app["com.example.disabled"].enabled, Some(false));
        assert_eq!(
            prefs.per_app["com.example.conflict"].enabled,
            Some(false),
            "disabled list is parsed after enabled list so off wins conflicts"
        );
    }

    #[test]
    fn web_override_persisted_keys_round_trip_through_build_prefs() {
        let mut prefs = Prefs::default();
        for url in [
            "compme://setOverride?domain=Docs.Google.com&excluded=true",
            "compme://setOverride?app=com.foo.disabled&enabled=false",
            "compme://setOverride?app=com.foo.enabled&enabled=true",
            "compme://setOverride?app=com.foo.excluded&excluded=true",
        ] {
            handle_deep_link(url, None, &mut prefs, |_| true).expect("valid deep link applies");
        }

        let dir = std::env::temp_dir().join(format!(
            "compme-web-override-persist-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.env");
        persist_web_override_prefs(&path, &prefs);

        let map = config::load_file_map(&path);
        assert_eq!(
            map.get("COMPME_EXCLUDED_DOMAINS"),
            Some(&"docs.google.com".to_string())
        );
        assert_eq!(
            map.get("COMPME_DISABLED_APPS"),
            Some(&"com.foo.disabled".to_string())
        );
        assert_eq!(
            map.get("COMPME_ENABLED_APPS"),
            Some(&"com.foo.enabled".to_string())
        );
        assert_eq!(
            map.get("COMPME_EXCLUDED_APPS"),
            Some(&"com.foo.excluded".to_string())
        );

        let reloaded = build_prefs(&|key| map.get(key).cloned());
        assert_eq!(reloaded.per_app["com.foo.disabled"].enabled, Some(false));
        assert_eq!(reloaded.per_app["com.foo.enabled"].enabled, Some(true));
        assert!(!reloaded.should_suggest(None, Some("docs.google.com"), 0));
        assert!(!reloaded.should_suggest(Some("com.foo.disabled"), None, 0));
        assert!(reloaded.should_suggest(Some("com.foo.enabled"), None, 0));
        assert!(!reloaded.should_suggest(Some("com.foo.excluded"), None, 0));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn web_override_persist_round_trips_all_five_overrides_on_one_app() {
        // A single app carrying ALL FIVE editable per-app overrides at once —
        // enabled + tab_disabled + mid_line + autocorrect + grammar_fix — must survive
        // persist_web_override_prefs -> build_prefs with every field intact and
        // INDEPENDENT. The existing feature-only round-trip pins mid_line/
        // autocorrect/grammar_fix/tab_disabled but deliberately keeps `enabled == None` to
        // prove independence; no test rounds the `enabled` key alongside the
        // four feature keys on the SAME app. Because each field serializes to a
        // *separate* comma-list key, a regression where the enabled write (or its
        // reload) clobbered or dropped a co-resident feature override — or vice
        // versa — would pass every existing round-trip yet corrupt an app that a
        // user configured fully in the Apps pane.
        use prefs::AppPolicyField::*;
        let app = "com.foo.allfive";
        let mut prefs = Prefs::default();
        prefs.set_app_policy_field(app, Enabled, false); // -> COMPME_DISABLED_APPS
        prefs.set_app_policy_field(app, TabDisabled, true); // -> COMPME_TAB_DISABLED_APPS
        prefs.set_app_policy_field(app, MidLine, true); // -> COMPME_MIDLINE_ON_APPS
        prefs.set_app_policy_field(app, Autocorrect, false); // -> COMPME_AUTOCORRECT_OFF_APPS
        prefs.set_app_policy_field(app, GrammarFix, true); // -> COMPME_GRAMMAR_FIX_ON_APPS

        let dir = std::env::temp_dir().join(format!(
            "compme-web-override-allfour-persist-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.env");
        persist_web_override_prefs(&path, &prefs);

        let map = config::load_file_map(&path);
        // Each field lands in its own key for this one app; the disabled/enabled
        // split is pinned so a polarity flip is caught.
        assert_eq!(map.get("COMPME_DISABLED_APPS"), Some(&app.to_string()));
        assert_eq!(map.get("COMPME_ENABLED_APPS"), None);
        assert_eq!(map.get("COMPME_TAB_DISABLED_APPS"), Some(&app.to_string()));
        assert_eq!(map.get("COMPME_MIDLINE_ON_APPS"), Some(&app.to_string()));
        assert_eq!(
            map.get("COMPME_AUTOCORRECT_OFF_APPS"),
            Some(&app.to_string())
        );
        assert_eq!(
            map.get("COMPME_GRAMMAR_FIX_ON_APPS"),
            Some(&app.to_string())
        );

        let reloaded = build_prefs(&|key| map.get(key).cloned());
        let p = &reloaded.per_app[app];
        assert_eq!(p.enabled, Some(false), "enabled override lost");
        assert!(p.tab_disabled, "tab_disabled override lost");
        assert_eq!(p.mid_line, Some(true), "mid_line override lost");
        assert_eq!(p.autocorrect, Some(false), "autocorrect override lost");
        assert_eq!(p.grammar_fix, Some(true), "grammar_fix override lost");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn web_override_persist_round_trips_midline_autocorrect_tab_disabled_per_app() {
        // r2 HIGH-2: the Apps-pane feature overrides (mid_line / autocorrect /
        // tab_disabled) must survive persist_web_override_prefs -> build_prefs
        // EXACTLY and INDEPENDENTLY of the enabled / excluded keys. These three
        // fields are set via set_app_policy_field (not deep links), so the
        // existing enabled/excluded round-trip test never exercised them — the
        // _ON/_OFF/_TAB_DISABLED comma-list serialization was untested end to end.
        let mut prefs = Prefs::default();
        // An app carrying ONLY feature overrides (no enabled/excluded override),
        // so we prove the feature keys round-trip on their own.
        prefs.set_app_policy_field("com.foo.feat", prefs::AppPolicyField::MidLine, true);
        prefs.set_app_policy_field("com.foo.feat", prefs::AppPolicyField::Autocorrect, false);
        prefs.set_app_policy_field("com.foo.feat", prefs::AppPolicyField::GrammarFix, true);
        prefs.set_app_policy_field("com.foo.feat", prefs::AppPolicyField::TabDisabled, true);
        // A second app exercising the opposite mid_line/autocorrect polarity so
        // the _ON vs _OFF list split is pinned, not just "non-default present".
        prefs.set_app_policy_field("com.bar.feat", prefs::AppPolicyField::MidLine, false);
        prefs.set_app_policy_field("com.bar.feat", prefs::AppPolicyField::Autocorrect, true);
        prefs.set_app_policy_field("com.bar.feat", prefs::AppPolicyField::GrammarFix, false);

        let dir = std::env::temp_dir().join(format!(
            "compme-web-override-feature-persist-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.env");
        persist_web_override_prefs(&path, &prefs);

        let map = config::load_file_map(&path);
        // The polarity-split comma lists are written verbatim.
        assert_eq!(
            map.get("COMPME_MIDLINE_ON_APPS"),
            Some(&"com.foo.feat".to_string())
        );
        assert_eq!(
            map.get("COMPME_MIDLINE_OFF_APPS"),
            Some(&"com.bar.feat".to_string())
        );
        assert_eq!(
            map.get("COMPME_AUTOCORRECT_ON_APPS"),
            Some(&"com.bar.feat".to_string())
        );
        assert_eq!(
            map.get("COMPME_AUTOCORRECT_OFF_APPS"),
            Some(&"com.foo.feat".to_string())
        );
        assert_eq!(
            map.get("COMPME_GRAMMAR_FIX_ON_APPS"),
            Some(&"com.foo.feat".to_string())
        );
        assert_eq!(
            map.get("COMPME_GRAMMAR_FIX_OFF_APPS"),
            Some(&"com.bar.feat".to_string())
        );
        assert_eq!(
            map.get("COMPME_TAB_DISABLED_APPS"),
            Some(&"com.foo.feat".to_string())
        );

        let reloaded = build_prefs(&|key| map.get(key).cloned());
        let foo = &reloaded.per_app["com.foo.feat"];
        assert_eq!(foo.mid_line, Some(true));
        assert_eq!(foo.autocorrect, Some(false));
        assert_eq!(foo.grammar_fix, Some(true));
        assert!(foo.tab_disabled);
        // Independence: the feature-only app never gained an enabled override.
        assert_eq!(foo.enabled, None);
        let bar = &reloaded.per_app["com.bar.feat"];
        assert_eq!(bar.mid_line, Some(false));
        assert_eq!(bar.autocorrect, Some(true));
        assert_eq!(bar.grammar_fix, Some(false));
        assert!(!bar.tab_disabled);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_squelch_logs_changes_and_resumes_after_reset() {
        let mut squelch = LogSquelch::default();
        // First occurrence logs; identical repeats are squelched.
        assert!(squelch.should_log("UnsupportedField"));
        assert!(!squelch.should_log("UnsupportedField"));
        assert!(!squelch.should_log("UnsupportedField"));
        // A DIFFERENT error logs (state changed).
        assert!(squelch.should_log("StaleField"));
        assert!(!squelch.should_log("StaleField"));
        // A successful read resets: the next error is a new episode.
        squelch.reset();
        assert!(squelch.should_log("StaleField"));
    }

    #[test]
    fn statistics_pane_composition_is_exactly_stats_rows_deep() {
        // The window builds STATS_ROWS labels and zips them with these
        // lines; a composition that stopped matching would silently leave a
        // stale label (review-c103). Pin against the REAL const — a literal
        // here goes stale silently when the pane grows a row.
        let mut lines = stats_pane_lines(&[stats::DayBucket::default()]);
        lines.push(lifetime_line(&stats::PersistedStats::default()));
        assert_eq!(lines.len(), crate::shell::STATS_ROWS);
    }

    #[test]
    fn stats_flush_due_boundaries() {
        assert!(stats_flush_due(None, 10_000), "never flushed: due now");
        let last = Some(100_000);
        assert!(
            !stats_flush_due(last, 100_000 + STATS_FLUSH_INTERVAL_MS - 1),
            "inside the interval"
        );
        assert!(
            stats_flush_due(last, 100_000 + STATS_FLUSH_INTERVAL_MS),
            "interval elapsed"
        );
        assert!(
            !stats_flush_due(last, 99_999),
            "clock-skew saturates, not due"
        );
    }

    fn flush_temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("cm-flush-{tag}-{}", std::process::id()))
    }

    #[test]
    fn lifetime_flush_is_idempotent() {
        // baseline + grow-only session totals, overwritten in place: the
        // SAME state must produce byte-identical files no matter how many
        // times it flushes (periodic + shutdown share this writer).
        let dir = flush_temp_path("idem");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("stats.env");
        let base = stats::PersistedStats {
            shown: 10,
            accepted: 4,
            dismissed: 2,
            superseded: 1,
            words: 9,
        };
        let mut usage = stats::Stats::new();
        usage.record(1_000, stats::Outcome::Shown);
        usage.record(1_000, stats::Outcome::Accepted { words: 2 });
        let session = usage.session_totals();

        persist_lifetime_stats(Some(&path), &base, session).expect("first flush");
        let first = std::fs::read(&path).expect("file written");
        persist_lifetime_stats(Some(&path), &base, session).expect("second flush");
        let second = std::fs::read(&path).expect("file rewritten");
        assert_eq!(first, second, "same state → identical bytes");
        assert_eq!(
            String::from_utf8(first).unwrap(),
            stats::render_stats_file(&base.merged(session.counts, session.words))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lifetime_flush_then_final_flush_never_double_counts() {
        // The shutdown flush after N periodic flushes must yield EXACTLY
        // base + final-session-totals — a re-read of the file (the old
        // shutdown shape) would re-add the session every time.
        let dir = flush_temp_path("nodouble");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("stats.env");
        let base = stats::PersistedStats {
            shown: 100,
            accepted: 50,
            dismissed: 10,
            superseded: 5,
            words: 200,
        };
        let mut usage = stats::Stats::new();
        usage.record(1_000, stats::Outcome::Accepted { words: 3 });
        persist_lifetime_stats(Some(&path), &base, usage.session_totals()).expect("periodic");
        // Session grows, then the final (shutdown) flush.
        usage.record(2_000, stats::Outcome::Accepted { words: 4 });
        persist_lifetime_stats(Some(&path), &base, usage.session_totals()).expect("final");

        let on_disk = stats::parse_stats_file(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(on_disk.accepted, 52, "base 50 + 2 accepts, counted once");
        assert_eq!(on_disk.words, 207, "base 200 + 7 words, counted once");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lifetime_flush_skips_without_path_and_errors_cleanly_on_unwritable_dest() {
        // No stats path (no HOME/COMPME_CONFIG) → quiet no-op success. This is
        // the only TRUE fail-soft case: there is nowhere to write, so success.
        assert!(persist_lifetime_stats(
            None,
            &stats::PersistedStats::default(),
            Default::default()
        )
        .is_ok());
        // Unwritable destination (parent is a regular FILE) → Err (NOT
        // soft-swallowed), no panic, and nothing is written at the target.
        let blocker = flush_temp_path("blocked");
        std::fs::write(&blocker, b"i am a file").unwrap();
        let path = blocker.join("stats.env");
        assert!(persist_lifetime_stats(
            Some(&path),
            &stats::PersistedStats::default(),
            Default::default()
        )
        .is_err());
        assert!(
            !path.exists(),
            "a failed flush must leave nothing at the destination"
        );
        let _ = std::fs::remove_file(&blocker);
    }

    #[test]
    fn lifetime_flush_creates_a_nested_missing_parent_and_leaves_no_temp() {
        // The stats home may be several levels deep and not yet exist (first run
        // before any dir is created). `create_dir_all(parent)` must build the
        // whole chain — `create_dir` alone would error on the missing
        // grandparent. After a clean flush the only file present is the target:
        // the `.env.tmp` scratch must have been renamed away, never left behind.
        let root = flush_temp_path("nested");
        let _ = std::fs::remove_dir_all(&root);
        let path = root.join("a").join("b").join("stats.env");
        assert!(
            !path.parent().unwrap().exists(),
            "parent chain absent up front"
        );

        persist_lifetime_stats(
            Some(&path),
            &stats::PersistedStats::default(),
            Default::default(),
        )
        .expect("flush into a missing nested parent");

        assert!(path.exists(), "the target file must exist after the flush");
        let leftovers: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .expect("dir readable")
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            leftovers,
            vec!["stats.env".to_string()],
            "no `.env.tmp` scratch may linger beside the renamed target"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn setup_poll_fires_only_while_visible_and_spaced() {
        // The Setup tab re-probes permissions on a 480ms cadence, but ONLY
        // while the window is visible — hidden windows must cost nothing.
        assert!(!setup_poll_due(false, None, 10_000), "hidden: never");
        assert!(setup_poll_due(true, None, 10_000), "first visible poll");
        assert!(
            !setup_poll_due(true, Some(10_000), 10_479),
            "inside the interval"
        );
        assert!(
            setup_poll_due(true, Some(10_000), 10_480),
            "interval elapsed"
        );
        assert!(
            !setup_poll_due(false, Some(10_000), 99_999),
            "hidden again: never, regardless of elapsed"
        );
    }

    #[test]
    fn settings_flags_share_the_tray_enabled_atomic() {
        // The Enabled switch and the tray toggle are TWO VIEWS of ONE
        // atomic — sharing the Arc is what keeps them in sync (banked
        // c115 design). Pin identity, not just equal values.
        let config = Config::from_lookup(lookup(&[
            ("COMPME_MIDLINE", "1"),
            ("COMPME_AUTOCORRECT", "1"),
            ("COMPME_TRAILING_SPACE", "1"),
            ("COMPME_CLIPBOARD_CONTEXT", "0"),
            ("COMPME_SCREEN_CONTEXT", "1"),
            ("COMPME_EMOJI", "1"),
            ("COMPME_EMOJI_SKIN_TONE", "dark"),
            ("COMPME_EMOJI_GENDER", "female"),
            ("COMPME_INSTRUCTIONS", "Keep completions terse."),
            ("COMPME_SENDER_NAME", "Ada"),
            ("COMPME_SENDER_EMAIL", "ada@example.com"),
            ("COMPME_STRENGTH", "4"),
        ]));
        let tray_enabled = Arc::new(AtomicBool::new(true));
        let flags = build_settings_flags(&config, Arc::clone(&tray_enabled), 16);
        assert!(Arc::ptr_eq(&flags.general_enabled, &tray_enabled));
        assert_eq!(
            flags.labs_midline.load(Ordering::Relaxed),
            config.allow_mid_word
        );
        assert!(flags.general_autocorrect.load(Ordering::Relaxed) == config.autocorrect);
        assert_eq!(
            flags.general_trailing_space.load(Ordering::Relaxed),
            config.trailing_space
        );
        assert!(flags.context_clipboard.load(Ordering::Relaxed) == config.clipboard_context);
        assert!(flags.context_screen.load(Ordering::Relaxed) == config.screen_context);
        assert_eq!(
            flags.emoji_enabled.load(Ordering::Relaxed),
            config.emoji.is_some()
        );
        assert_eq!(
            flags.emoji_skin_tone_index.load(Ordering::Relaxed),
            emoji_skin_tone_index(config.emoji_prefs.skin_tone)
        );
        assert_eq!(
            flags.emoji_gender_index.load(Ordering::Relaxed),
            emoji_gender_index(config.emoji_prefs.gender)
        );
        assert_eq!(
            flags.setup_model_index.load(Ordering::Relaxed),
            crate::model_picker::recommended_index()
        );
        let expected_titles = crate::model_picker::model_menu_titles(16);
        assert!(!flags.setup_model_menu_titles.is_empty());
        assert_eq!(flags.setup_model_menu_titles, expected_titles);
        assert_eq!(
            *flags.personalization_instructions.lock().unwrap(),
            config.personalization.global_instructions
        );
        assert_eq!(
            *flags.personalization_sender_name.lock().unwrap(),
            config.personalization.sender.name
        );
        assert_eq!(
            *flags.personalization_sender_email.lock().unwrap(),
            config.personalization.sender.email
        );
        assert_eq!(
            flags.personalization_strength_index.load(Ordering::Relaxed),
            personalization_strength_index(config.personalization.strength)
        );
        assert_eq!(
            flags.personalization_strength_titles,
            personalization_strength_titles()
        );
    }

    // macOS-only: key NAMES come from the macOS arm's keycode label table;
    // the scaffold arms render numerically until real adapters bring their
    // own key naming (ROADMAP 1.1).
    #[cfg(target_os = "macos")]
    #[test]
    fn shortcuts_text_names_known_keycodes_and_falls_back_numerically() {
        // Shortcuts tab (persist-only slice): current bindings by NAME for
        // the known codes, numeric fallback for exotic rebinds, fixed rows
        // for the non-rebindable keys, and the how-to-change note.
        let text = shortcuts_text((48, 0), (50, 0), None);
        assert!(text.contains("Accept word: Tab"));
        assert!(text.contains("Accept full: ` (backtick)"));
        assert!(text.contains("Grammar accept: Unbound"));
        assert!(text.contains("Dismiss: Esc"));
        assert!(text.contains("Cycle candidates: Down arrow"));
        assert!(text.contains("COMPME_ACCEPT_WORD_KEY"));
        assert!(text.contains("COMPME_GRAMMAR_ACCEPT_KEY"));
        assert!(text.contains("relaunch"));

        let custom = shortcuts_text((125, 0), (200, 0), Some((96, 0)));
        assert!(custom.contains("Accept word: Down arrow"));
        assert!(custom.contains("Accept full: key 200")); // unnamed code → generic
        assert!(custom.contains("Grammar accept: F5"));

        // Modifier masks render as glyph-prefixed labels (slice 1b label half):
        // 512 = Carbon shiftKey ⇧, 4096 = controlKey ⌃.
        let combo = shortcuts_text((48, 512), (50, 4096), Some((96, 512)));
        assert!(combo.contains("Accept word: \u{21e7}Tab"), "{combo}");
        assert!(
            combo.contains("Accept full: \u{2303}` (backtick)"),
            "{combo}"
        );
        assert!(combo.contains("Grammar accept: \u{21e7}F5"), "{combo}");
    }

    #[test]
    fn domain_from_url_extracts_lowercased_host_without_port() {
        // The per-domain extractor's pure half (audit c121: domain gating
        // was promised but dead — the gates now consume a domain and this
        // is the URL→domain step the AX extractor will feed).
        assert_eq!(
            domain_from_url("https://Sub.Example.COM:8443/path?q=1"),
            Some("sub.example.com".to_string())
        );
        assert_eq!(
            domain_from_url("http://docs.google.com/document/d/x"),
            Some("docs.google.com".to_string())
        );
        assert_eq!(domain_from_url("not a url"), None);
        assert_eq!(domain_from_url("file:///etc/hosts"), None);
        assert_eq!(domain_from_url("https://"), None);
        // Userinfo stripping is SECURITY-relevant: `user:pw@host` must resolve
        // to the real host (after the last `@`), never the userinfo. For
        // `evil.com@bank.example` the host that per-domain rules must gate is
        // bank.example — taking the userinfo (or .next() instead of
        // .next_back()) would defeat the exclusion.
        assert_eq!(
            domain_from_url("https://user:pw@Bank.Example/account"),
            Some("bank.example".to_string())
        );
        assert_eq!(
            domain_from_url("https://evil.com@bank.example/"),
            Some("bank.example".to_string())
        );
        // Host with no path still resolves.
        assert_eq!(
            domain_from_url("https://example.com"),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn comma_list_trims_and_drops_empties_and_sorted_join_orders() {
        // Pinned in isolation (previously only exercised via build_prefs
        // round-trips, which would not localize a "keep empties" regression):
        // surrounding whitespace is trimmed, empty/whitespace-only entries are
        // dropped (including a doubled `,,`), and None yields an empty list.
        assert_eq!(comma_list(Some(" a , ,b ,".into())), vec!["a", "b"]);
        assert_eq!(comma_list(Some("x,,y".into())), vec!["x", "y"]);
        assert_eq!(comma_list(Some("   ".into())), Vec::<String>::new());
        assert_eq!(comma_list(None), Vec::<String>::new());
        // sorted_join is stable-sorted for a deterministic file diff.
        assert_eq!(sorted_join(["c", "a", "b"].into_iter()), "a,b,c");
        assert_eq!(sorted_join(std::iter::empty()), "");
    }

    #[test]
    fn suggestion_gates_honor_an_excluded_domain_when_present() {
        // End-to-end through the shared gate (audit top-5 missing test):
        // the domain parameter must actually block — and None must not.
        let mut prefs = Prefs::default();
        prefs.excluded_domains.insert("blocked.example".to_string());
        assert!(suggestion_gates_pass(
            Some("com.apple.Safari"),
            "hello",
            Some("ok.example"),
            &prefs,
            0
        ));
        assert!(!suggestion_gates_pass(
            Some("com.apple.Safari"),
            "hello",
            Some("blocked.example"),
            &prefs,
            0
        ));
        assert!(suggestion_gates_pass(
            Some("com.apple.Safari"),
            "hello",
            None,
            &prefs,
            0
        ));
    }

    #[test]
    fn domain_miss_notice_fires_at_threshold_not_before() {
        let mut notice = DomainMissNotice::default();
        for _ in 0..DOMAIN_MISS_NOTICE_THRESHOLD - 1 {
            assert_eq!(notice.observe(true, false), None);
        }
        let msg = notice.observe(true, false).expect("fires at the threshold");
        assert!(msg.contains(&DOMAIN_MISS_NOTICE_THRESHOLD.to_string()));
        assert!(msg.contains("COMPME_DEBUG"));
    }

    #[test]
    fn domain_miss_notice_success_resets_the_streak() {
        let mut notice = DomainMissNotice::default();
        for _ in 0..DOMAIN_MISS_NOTICE_THRESHOLD - 1 {
            assert_eq!(notice.observe(true, false), None);
        }
        assert_eq!(notice.observe(true, true), None, "success resets");
        for _ in 0..DOMAIN_MISS_NOTICE_THRESHOLD - 1 {
            assert_eq!(notice.observe(true, false), None, "fresh streak");
        }
        assert!(notice.observe(true, false).is_some());
    }

    #[test]
    fn domain_miss_notice_is_one_shot_per_process() {
        let mut notice = DomainMissNotice::default();
        for _ in 0..DOMAIN_MISS_NOTICE_THRESHOLD {
            let _ = notice.observe(true, false);
        }
        // Keep missing, even across a reset + fresh streak: never re-fires.
        assert_eq!(notice.observe(true, true), None);
        for _ in 0..3 * DOMAIN_MISS_NOTICE_THRESHOLD {
            assert_eq!(notice.observe(true, false), None);
        }
    }

    #[test]
    fn domain_miss_notice_never_fires_without_rules() {
        let mut notice = DomainMissNotice::default();
        for _ in 0..3 * DOMAIN_MISS_NOTICE_THRESHOLD {
            assert_eq!(notice.observe(false, false), None);
        }
    }

    #[test]
    fn domain_miss_notice_rules_removed_mid_streak_suppress_then_restore_fires() {
        // The mirror of the mid-streak test (reachable via deep-link Domain
        // Enable removing the last rule): crossing the threshold while rules
        // are ABSENT stays silent; restoring rules fires on the next miss.
        let mut notice = DomainMissNotice::default();
        for _ in 0..DOMAIN_MISS_NOTICE_THRESHOLD - 1 {
            assert_eq!(notice.observe(true, false), None);
        }
        // Rules removed exactly at the would-fire miss: suppressed.
        assert_eq!(notice.observe(false, false), None);
        // Rules restored: the accumulated streak fires immediately.
        assert!(notice.observe(true, false).is_some());
    }

    #[test]
    fn domain_miss_notice_rules_added_mid_streak_fire_immediately() {
        // The streak counts even while rules are empty (detection genuinely
        // HAS been failing); the first miss after rules appear fires.
        let mut notice = DomainMissNotice::default();
        for _ in 0..DOMAIN_MISS_NOTICE_THRESHOLD + 5 {
            assert_eq!(notice.observe(false, false), None);
        }
        assert!(notice.observe(true, false).is_some());
    }

    #[test]
    fn domain_cache_entry_pairs_browser_app_with_extracted_host() {
        // The Focus-arm decision: a browser app + a real page URL caches
        // (app key, HOST) — the full URL is dropped at extraction (privacy
        // boundary; path/query never leave the expression).
        assert_eq!(
            domain_cache_entry(
                Some("com.apple.Safari"),
                Some("https://docs.google.com/document/d/abc?tab=1")
            ),
            Some((
                "com.apple.Safari".to_string(),
                "docs.google.com".to_string()
            ))
        );
        // Non-browser app: never caches, even with a URL-shaped value.
        assert_eq!(
            domain_cache_entry(Some("com.apple.TextEdit"), Some("https://x.example/")),
            None
        );
        // Browser but no URL resolved (AX miss): fail-open.
        assert_eq!(domain_cache_entry(Some("com.google.Chrome"), None), None);
        // Browser but a non-URL value (omnibox search text shape): the
        // extractor rejects it — no bogus host.
        assert_eq!(
            domain_cache_entry(Some("com.google.Chrome"), Some("how to cook rice")),
            None
        );
        // No app key: nothing to attribute the host to.
        assert_eq!(domain_cache_entry(None, Some("https://x.example/")), None);
    }

    #[test]
    fn apply_global_disable_maps_arms_to_snooze_or_persistent_off() {
        // Global submenu (a3 build item 1, the half the 06-10 annotation
        // overclaimed): Hour/UntilRelaunch ride the snooze machinery like
        // the per-app arms; Always asks the caller to flip the persistent
        // enabled flag (true return) — the existing edge persists it.
        let mut prefs = Prefs::default();
        assert!(!apply_global_disable(DisableArm::Hour, &mut prefs, 1_000));
        assert!(prefs.is_snoozed(1_000 + 59 * 60 * 1000));
        assert!(!prefs.is_snoozed(1_000 + 61 * 60 * 1000));

        let mut prefs = Prefs::default();
        assert!(!apply_global_disable(
            DisableArm::UntilRelaunch,
            &mut prefs,
            1_000
        ));
        assert!(prefs.is_snoozed(u64::MAX - 1), "holds for the process life");

        let mut prefs = Prefs::default();
        assert!(apply_global_disable(DisableArm::Always, &mut prefs, 1_000));
        assert!(!prefs.is_snoozed(2_000), "Always is not a snooze");
    }

    #[test]
    fn switch_edge_fires_once_per_change_and_tracks_current() {
        // The watcher contract (audit c121): apply+persist exactly once per
        // edge, never per heartbeat.
        let flag = AtomicBool::new(false);
        let mut current = false;
        assert_eq!(switch_edge(&flag, &mut current), None, "no change: quiet");
        flag.store(true, Ordering::Relaxed);
        assert_eq!(switch_edge(&flag, &mut current), Some(true), "edge fires");
        assert!(current, "current tracks the new state");
        assert_eq!(switch_edge(&flag, &mut current), None, "same state: quiet");
        flag.store(false, Ordering::Relaxed);
        assert_eq!(switch_edge(&flag, &mut current), Some(false));
    }

    #[test]
    fn general_autocorrect_settings_edge_applies_live_and_persists_once() {
        let flag = AtomicBool::new(true);
        let mut current = false;
        let persisted = RefCell::new(Vec::new());
        let dismissed = RefCell::new(Vec::new());

        assert_eq!(
            apply_autocorrect_settings_edge(
                &flag,
                &mut current,
                |on| persisted.borrow_mut().push(on),
                |on| dismissed.borrow_mut().push(on),
            ),
            Some(true)
        );
        assert!(current);
        assert_eq!(persisted.borrow().as_slice(), &[true]);
        assert_eq!(dismissed.borrow().as_slice(), &[] as &[bool]);
        assert_eq!(
            apply_autocorrect_settings_edge(
                &flag,
                &mut current,
                |on| persisted.borrow_mut().push(on),
                |on| dismissed.borrow_mut().push(on),
            ),
            None
        );
        assert_eq!(persisted.borrow().as_slice(), &[true]);
        assert_eq!(dismissed.borrow().as_slice(), &[] as &[bool]);

        flag.store(false, Ordering::Relaxed);
        assert_eq!(
            apply_autocorrect_settings_edge(
                &flag,
                &mut current,
                |on| persisted.borrow_mut().push(on),
                |on| dismissed.borrow_mut().push(on),
            ),
            Some(false)
        );
        assert!(!current);
        assert_eq!(persisted.borrow().as_slice(), &[true, false]);
        assert_eq!(dismissed.borrow().as_slice(), &[false]);
    }

    #[test]
    fn trailing_space_settings_edge_sets_engine_and_persists_once() {
        let flag = AtomicBool::new(false);
        let mut current = true;
        let live = RefCell::new(Vec::new());
        let persisted = RefCell::new(Vec::new());

        assert_eq!(
            apply_trailing_space_settings_edge(
                &flag,
                &mut current,
                |on| live.borrow_mut().push(on),
                |on| persisted.borrow_mut().push(on),
            ),
            Some(false)
        );
        assert!(!current);
        assert_eq!(live.borrow().as_slice(), &[false]);
        assert_eq!(persisted.borrow().as_slice(), &[false]);
    }

    #[test]
    fn midline_settings_edge_applies_effective_app_policy_and_persists_global_default() {
        let flag = AtomicBool::new(true);
        let mut global = false;
        let mut prefs = Prefs::default();
        prefs.set_app_policy_field("com.override", prefs::AppPolicyField::MidLine, false);
        let live = RefCell::new(Vec::new());
        let persisted = RefCell::new(Vec::new());

        assert_eq!(
            apply_midline_settings_edge(
                &flag,
                &mut global,
                &prefs,
                Some("com.override"),
                |on| live.borrow_mut().push(on),
                |on| persisted.borrow_mut().push(on),
            ),
            Some(true)
        );

        assert!(global);
        assert_eq!(live.borrow().as_slice(), &[false]);
        assert_eq!(persisted.borrow().as_slice(), &[true]);
    }

    #[test]
    fn delete_app_row_resolves_against_ids_and_recomposes_together() {
        // The irreversible path (audit c121, top missing test): row index →
        // app id resolution uses the SAME cap/order as the rendered lines,
        // out-of-range clicks no-op, and lines+ids recompose as one unit so
        // a follow-up click can't hit the wrong app.
        use memory::{MemoryStore, StaticKey, StorageMode};
        let store =
            MemoryStore::open_in_memory(&StaticKey([7u8; 32]), StorageMode::AcceptedOnly).unwrap();
        store.remember("com.a.alpha", "x").unwrap();
        store.remember("com.a.alpha", "y").unwrap();
        store.remember("com.b.beta", "z").unwrap();
        let (lines, ids) = compose_apps_rows(Some(&store));
        assert_eq!(
            ids,
            vec!["com.a.alpha".to_string(), "com.b.beta".to_string()]
        );
        assert_eq!(lines.len(), 2);

        // Out-of-range (stale) click: nothing deleted.
        assert!(delete_app_row_and_recompose(&store, &ids, 5).is_none());
        assert_eq!(store.count().unwrap(), 3);

        // Row 0 deletes alpha; recomposed pair stays aligned.
        let (lines2, ids2) = delete_app_row_and_recompose(&store, &ids, 0).unwrap();
        assert_eq!(ids2, vec!["com.b.beta".to_string()]);
        assert_eq!(lines2.len(), 1);
        assert!(lines2[0].contains("com.b.beta"));
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn apps_row_ids_align_with_the_rendered_lines() {
        // Delete buttons carry a row index; resolution back to an app id
        // must use the SAME cap and order as the rendered lines, or a click
        // deletes the wrong app's history.
        let many: Vec<(String, u64)> = (0..20).map(|i| (format!("app{i:02}"), 20 - i)).collect();
        let ids = apps_row_ids(&many);
        assert_eq!(ids.len(), crate::shell::APPS_ROWS);
        assert_eq!(ids[0], "app00");
        assert_eq!(ids.len(), apps_pane_lines(&many, true).len());
        // Status lines carry no deletable rows.
        assert!(apps_row_ids(&[]).is_empty());
    }

    #[test]
    fn apps_policy_field_index_maps_in_checkbox_order() {
        // The Apps-row checkbox tag packs (row, field); the field index must
        // map back to the SAME AppPolicyField order the AppKit layer renders
        // (APP_POLICY_FIELD_TITLES), or a toggle writes the wrong field. The
        // index count is pinned to crate::shell::APP_POLICY_FIELDS so a
        // drifting duplicate can't silently desync the two sides.
        use prefs::AppPolicyField::*;
        assert_eq!(apps_policy_field_from_index(0), Some(Enabled));
        assert_eq!(apps_policy_field_from_index(1), Some(TabDisabled));
        assert_eq!(apps_policy_field_from_index(2), Some(MidLine));
        assert_eq!(apps_policy_field_from_index(3), Some(Autocorrect));
        assert_eq!(apps_policy_field_from_index(4), Some(GrammarFix));
        // One past the last field is out of range (stale/garbled click no-ops).
        assert_eq!(
            apps_policy_field_from_index(crate::shell::APP_POLICY_FIELDS),
            None
        );
        // Every valid index resolves — the map covers all rendered checkboxes.
        for i in 0..crate::shell::APP_POLICY_FIELDS {
            assert!(apps_policy_field_from_index(i).is_some());
        }
    }

    #[test]
    fn apps_policy_bits_resolve_per_app_overrides_in_checkbox_order() {
        // The Apps-pane checkboxes seed from these bits; each row must reflect
        // the saved per-app override (not a hard-seeded OFF), in the SAME
        // [Enabled, TabDisabled, MidLine, Autocorrect, GrammarFix] order the checkboxes use.
        use prefs::AppPolicyField::*;
        let mut prefs = prefs::Prefs {
            default_enabled: false,
            ..Default::default()
        };
        // "explicit" overrides every field ON; "inherit" has no override and so
        // falls back to defaults (default_enabled=false, globals passed below).
        prefs.set_app_policy_field("com.explicit", Enabled, true);
        prefs.set_app_policy_field("com.explicit", TabDisabled, true);
        prefs.set_app_policy_field("com.explicit", MidLine, true);
        prefs.set_app_policy_field("com.explicit", Autocorrect, true);
        prefs.set_app_policy_field("com.explicit", GrammarFix, false);
        let ids = vec!["com.explicit".to_string(), "com.inherit".to_string()];

        let bits = compose_apps_policy_bits(&prefs, &ids, false, true, true);

        assert_eq!(bits.len(), ids.len(), "one entry per row, same order/cap");
        // Explicit overrides win on every field.
        assert_eq!(bits[0], [true, true, true, true, false]);
        // Inherit: Enabled falls to default_enabled (false), TabDisabled default
        // off, MidLine to the global (false), Autocorrect and GrammarFix to globals (true).
        assert_eq!(bits[1], [false, false, false, true, true]);
    }

    #[test]
    fn apps_pane_lines_render_counts_or_status() {
        // Apps tab: top apps by recorded-input count; honest status lines
        // when collection is off or nothing is recorded yet.
        assert_eq!(
            apps_pane_lines(&[], false),
            vec!["Input collection is off".to_string()]
        );
        assert_eq!(
            apps_pane_lines(&[], true),
            vec!["No recorded inputs yet".to_string()]
        );
        let counts = vec![
            ("com.apple.TextEdit".to_string(), 12),
            ("com.google.Chrome".to_string(), 3),
        ];
        assert_eq!(
            apps_pane_lines(&counts, true),
            vec![
                "com.apple.TextEdit \u{2014} 12".to_string(),
                "com.google.Chrome \u{2014} 3".to_string(),
            ]
        );
        // Capped at the window's row count (shared const, review-c108).
        let many: Vec<(String, u64)> = (0..20).map(|i| (format!("app{i:02}"), 20 - i)).collect();
        assert_eq!(apps_pane_lines(&many, true).len(), crate::shell::APPS_ROWS);
    }

    #[test]
    fn setup_pane_composition_respects_the_row_limit() {
        // The window builds SETUP_ROWS labels; zip-truncation would
        // silently hide overflow rows (review-c106, c103 precedent). Pin
        // against the REAL const, not a drifting literal.
        let rows = crate::setup_state::setup_rows(crate::setup_state::SetupChecks {
            ax_trusted: true,
            ax_relaunch_required: false,
            screen_context_enabled: true,
            screen_recording: true,
            model_ready: true,
        });
        assert!(rows.len() <= crate::shell::SETUP_ROWS);
    }

    #[test]
    fn setup_row_line_renders_readiness_glyphs() {
        // Setup tab rows: check mark when ready, cross when not.
        let ready = crate::setup_state::SetupRow {
            label: "Accessibility",
            ready: true,
            action: None,
        };
        let missing = crate::setup_state::SetupRow {
            label: "Model file",
            ready: false,
            action: None,
        };
        assert_eq!(setup_row_line(&ready), "\u{2713} Accessibility");
        assert_eq!(setup_row_line(&missing), "\u{2717} Model file");
    }

    #[test]
    fn setup_lines_from_checks_renders_relaunch_required_after_accessibility_grant() {
        let lines = setup_lines_from_checks(crate::setup_state::SetupChecks {
            ax_trusted: true,
            ax_relaunch_required: true,
            screen_context_enabled: false,
            screen_recording: false,
            model_ready: true,
        });
        assert_eq!(
            lines,
            vec![
                "\u{2717} Relaunch app".to_string(),
                "\u{2713} Model file".to_string()
            ]
        );
    }

    #[test]
    fn startup_key_bindings_apply_global_shortcuts_from_config() {
        let _guard = ShortcutBindingsGuard::reset();
        let config = Config::from_lookup(lookup(&[
            ("COMPME_FORCE_ACTIVATE_KEY", "cmd+96"),
            ("COMPME_TOGGLE_APP_KEY", "option+96"),
            ("COMPME_TOGGLE_GLOBAL_KEY", "shift+96"),
            ("COMPME_GRAMMAR_CHECK_KEY", "control+96"),
        ]));

        apply_startup_key_bindings(&config);

        let bindings = crate::shell::effective_shortcut_bindings();
        assert_eq!(bindings.force_activate, Some((96, 256)));
        assert_eq!(bindings.toggle_app, Some((96, 2048)));
        assert_eq!(bindings.toggle_global, Some((96, 512)));
        assert_eq!(bindings.grammar_check, Some((96, 4096)));
    }

    #[test]
    fn accept_subscription_observes_startup_shortcuts_before_installing() {
        let _guard = ShortcutBindingsGuard::reset();
        let config = Config::from_lookup(lookup(&[
            ("COMPME_FORCE_ACTIVATE_KEY", "cmd+96"),
            ("COMPME_TOGGLE_APP_KEY", "option+96"),
            ("COMPME_TOGGLE_GLOBAL_KEY", "shift+96"),
            ("COMPME_GRAMMAR_CHECK_KEY", "control+96"),
        ]));
        let observed = RefCell::new(None);

        let (sub, requires_relaunch) =
            subscribe_accept_after_startup_key_bindings(&config, true, || {
                *observed.borrow_mut() = Some(crate::shell::effective_shortcut_bindings());
                Ok(noop_accept_subscription())
            })
            .expect("subscription setup succeeds");

        assert!(!requires_relaunch);
        drop(sub);
        let observed = observed.into_inner().expect("subscribe closure ran");
        assert_eq!(observed.force_activate, Some((96, 256)));
        assert_eq!(observed.toggle_app, Some((96, 2048)));
        assert_eq!(observed.toggle_global, Some((96, 512)));
        assert_eq!(observed.grammar_check, Some((96, 4096)));
    }

    #[test]
    fn lifetime_line_formats_persisted_plus_session_totals() {
        // Statistics pane 4th row: lifetime totals (stats.env base merged
        // with the live session) — words and accepted only, no sparkline.
        let merged = stats::PersistedStats {
            shown: 100,
            accepted: 42,
            dismissed: 5,
            superseded: 3,
            words: 337,
        };
        assert_eq!(
            lifetime_line(&merged),
            "Lifetime 337 words \u{b7} 42 accepted"
        );
    }

    #[test]
    fn session_usage_snapshot_uses_the_stats_wall_clock_window() {
        // Usage events are recorded with epoch milliseconds. Shutdown must query
        // the same wall-clock domain; using process-elapsed milliseconds would
        // drop every current-session event from the 30-day stats window.
        let wall_ms = 1_800_000_000_000;
        let mut usage = stats::Stats::default();
        usage.record(wall_ms, stats::Outcome::Shown);
        usage.record(wall_ms, stats::Outcome::Accepted { words: 3 });
        usage.record_latency(wall_ms, 42);

        let snapshot = session_usage_snapshot(&usage, wall_ms + 1);
        assert_eq!(snapshot.counts.shown, 1);
        assert_eq!(snapshot.counts.accepted, 1);
        assert_eq!(snapshot.words, 3);
        assert_eq!(snapshot.latency_avg, Some(42));
        assert_eq!(snapshot.latency_p95, Some(42));

        let later_wall_ms = wall_ms + 1_000;
        let later_snapshot = session_usage_snapshot(&usage, later_wall_ms);
        assert_eq!(later_snapshot, snapshot);
    }

    #[test]
    fn stats_pane_lines_render_one_sparkline_row_per_metric() {
        // Statistics pane T2: three fixed rows (shown/accepted/words), each
        // label-padded with a per-day sparkline and the span total.
        let mk = |shown: usize, accepted: usize, words: usize| stats::DayBucket {
            counts: stats::Counts {
                shown,
                accepted,
                dismissed: 0,
                superseded: 0,
            },
            words,
        };
        let buckets = [mk(0, 0, 0), mk(2, 1, 2), mk(4, 1, 5)];
        assert_eq!(
            stats_pane_lines(&buckets),
            vec![
                "Shown    \u{2581}\u{2585}\u{2588}  6",
                "Accepted \u{2581}\u{2588}\u{2588}  2",
                "Words    \u{2581}\u{2584}\u{2588}  7",
            ]
        );
    }

    #[test]
    fn stats_range_group_indices_select_window_and_bucket_rows() {
        let now = 1_800_000_000_000;
        let mut usage = stats::Stats::default();
        usage.record(now - 13 * stats::DAY_MS, stats::Outcome::Shown);
        usage.record(
            now - 12 * stats::DAY_MS,
            stats::Outcome::Accepted { words: 2 },
        );
        usage.record(now - 2 * stats::DAY_MS, stats::Outcome::Shown);
        usage.record(now - stats::DAY_MS, stats::Outcome::Shown);
        usage.record(now, stats::Outcome::Accepted { words: 5 });

        assert_eq!(
            compose_stats_lines(&usage, now, 1, 1),
            vec![
                "Shown    \u{2585}\u{2588}  3",
                "Accepted \u{2588}\u{2588}  2",
                "Words    \u{2584}\u{2588}  7",
            ]
        );
    }

    #[test]
    fn env_shadow_warnings_name_only_set_switch_keys() {
        // A set env var silently overrides the file a Settings switch writes
        // (env-over-file layering) — warn at startup per shadowed key.
        let warnings = env_shadow_warnings(|key| key == "COMPME_AUTOCORRECT");
        assert_eq!(
            warnings,
            vec![
                "COMPME_AUTOCORRECT is set in the environment \u{2014} Settings changes \
                 persist to config.env but the environment wins at relaunch"
                    .to_string()
            ]
        );
        assert!(env_shadow_warnings(|_| false).is_empty());
        let every_warning = env_shadow_warnings(|_| true);
        for key in [
            "COMPME_ENABLED",
            "COMPME_MIDLINE",
            "COMPME_AUTOCORRECT",
            "COMPME_GRAMMAR_FIX",
            "COMPME_TRAILING_SPACE",
            "COMPME_CLIPBOARD_CONTEXT",
            "COMPME_SCREEN_CONTEXT",
            "COMPME_INSTRUCTIONS",
            "COMPME_SENDER_NAME",
            "COMPME_SENDER_EMAIL",
            "COMPME_STRENGTH",
            "COMPME_EMOJI",
            "COMPME_EMOJI_SKIN_TONE",
            "COMPME_EMOJI_GENDER",
            "COMPME_NO_COLLECT_APPS",
            "COMPME_EXCLUDED_APPS",
            "COMPME_EXCLUDED_DOMAINS",
            "COMPME_ENABLED_APPS",
            "COMPME_DISABLED_APPS",
            "COMPME_MIDLINE_ON_APPS",
            "COMPME_MIDLINE_OFF_APPS",
            "COMPME_AUTOCORRECT_ON_APPS",
            "COMPME_AUTOCORRECT_OFF_APPS",
            "COMPME_GRAMMAR_FIX_ON_APPS",
            "COMPME_GRAMMAR_FIX_OFF_APPS",
            "COMPME_THESAURUS_ON_APPS",
            "COMPME_THESAURUS_OFF_APPS",
            "COMPME_TAB_DISABLED_APPS",
            "COMPME_LICENSE_ACCEPTED",
            "COMPME_ACCEPT_WORD_KEY",
            "COMPME_ACCEPT_FULL_KEY",
            "COMPME_GRAMMAR_ACCEPT_KEY",
            "COMPME_GRAMMAR_CHECK_KEY",
        ] {
            assert!(
                every_warning.iter().any(|warning| warning.starts_with(key)),
                "{key} must warn when env shadows persisted config"
            );
        }
        assert_eq!(every_warning.len(), 33);
    }

    #[test]
    fn startup_env_shadow_notice_lines_keep_runtime_prefix_and_unset_keys_quiet() {
        let notices = startup_env_shadow_notice_lines(|key| key == "COMPME_ACCEPT_WORD_KEY");
        assert_eq!(
            notices,
            vec![
                "compme: COMPME_ACCEPT_WORD_KEY is set in the environment \u{2014} Settings \
                 changes persist to config.env but the environment wins at relaunch"
                    .to_string()
            ]
        );
        assert!(startup_env_shadow_notice_lines(|_| false).is_empty());
    }

    #[test]
    fn force_activate_parses_documented_key_and_legacy_alias() {
        assert_eq!(
            Config::from_lookup(lookup(&[("COMPME_FORCE_ACTIVATE_KEY", "ctrl+49")]))
                .force_activate_key
                .as_deref(),
            Some("ctrl+49")
        );
        assert_eq!(
            Config::from_lookup(lookup(&[("COMPME_FORCE_ACTIVATE", "shift+49")]))
                .force_activate_key
                .as_deref(),
            Some("shift+49")
        );
        let config = Config::from_lookup(lookup(&[
            ("COMPME_FORCE_ACTIVATE_KEY", "ctrl+49"),
            ("COMPME_FORCE_ACTIVATE", "shift+49"),
        ]));
        assert_eq!(
            config.force_activate_key.as_deref(),
            Some("ctrl+49"),
            "documented key spelling wins over the legacy alias"
        );
        let bindings = crate::shell::ShortcutBindings::from_config(
            config.force_activate_key.as_deref(),
            None,
            None,
            None,
        );
        assert_eq!(
            bindings.force_activate,
            crate::shell::parse_accept_key("ctrl+49")
        );
    }

    #[test]
    fn config_parses_grammar_check_and_grammar_accept_keys() {
        let config = Config::from_lookup(lookup(&[
            ("COMPME_GRAMMAR_CHECK_KEY", "cmd+shift+96"),
            ("COMPME_GRAMMAR_ACCEPT_KEY", "ctrl+96"),
        ]));
        assert_eq!(config.grammar_check_key.as_deref(), Some("cmd+shift+96"));
        assert_eq!(
            config.grammar_accept_key,
            crate::shell::parse_accept_key("ctrl+96")
        );
    }

    #[test]
    fn config_parses_toggle_shortcut_keys_and_maps_them_to_their_own_bindings() {
        let config = Config::from_lookup(lookup(&[
            ("COMPME_TOGGLE_APP_KEY", "ctrl+48"),
            ("COMPME_TOGGLE_GLOBAL_KEY", "shift+50"),
        ]));
        assert_eq!(config.toggle_app_key.as_deref(), Some("ctrl+48"));
        assert_eq!(config.toggle_global_key.as_deref(), Some("shift+50"));
        // Empty strings must fall through to None (the .filter guard), not
        // survive as bound-but-unparseable chords.
        let empty = Config::from_lookup(lookup(&[
            ("COMPME_TOGGLE_APP_KEY", ""),
            ("COMPME_TOGGLE_GLOBAL_KEY", ""),
        ]));
        assert!(empty.toggle_app_key.is_none());
        assert!(empty.toggle_global_key.is_none());
        // Thread the keys exactly as run() does (force_activate, toggle_app,
        // toggle_global, grammar_check): distinct chords so a positional swap
        // between the two toggle slots fails.
        let bindings = crate::shell::ShortcutBindings::from_config(
            None,
            config.toggle_app_key.as_deref(),
            config.toggle_global_key.as_deref(),
            None,
        );
        assert_eq!(
            bindings.toggle_app,
            crate::shell::parse_accept_key("ctrl+48")
        );
        assert_eq!(
            bindings.toggle_global,
            crate::shell::parse_accept_key("shift+50")
        );
    }

    #[test]
    fn env_shadow_warns_when_emoji_gender_env_shadows_persisted_setting() {
        let warnings = env_shadow_warnings(|key| key == "COMPME_EMOJI_GENDER");
        assert_eq!(
            warnings,
            vec![
                "COMPME_EMOJI_GENDER is set in the environment \u{2014} Settings changes \
                 persist to config.env but the environment wins at relaunch"
                    .to_string()
            ]
        );
    }

    #[test]
    fn trailing_space_persist_value_round_trips_through_the_parser() {
        assert!(
            Config::from_lookup(lookup(&[("COMPME_TRAILING_SPACE", switch_value(true))]))
                .trailing_space
        );
        assert!(
            !Config::from_lookup(lookup(&[("COMPME_TRAILING_SPACE", switch_value(false))]))
                .trailing_space
        );
    }

    #[test]
    fn autocorrect_persist_value_round_trips_through_the_parser() {
        // The General-tab Autocorrect switch persists switch_value(flag);
        // the launch parser must read it back to the same bool, both ways.
        assert!(
            Config::from_lookup(lookup(&[("COMPME_AUTOCORRECT", switch_value(true))])).autocorrect
        );
        assert!(
            !Config::from_lookup(lookup(&[("COMPME_AUTOCORRECT", switch_value(false))]))
                .autocorrect
        );
    }

    #[test]
    fn midline_persist_value_round_trips_through_the_parser() {
        // The Labs-pane watcher persists switch_value(flag); the launch-time
        // parser must read it back to the same bool, both ways.
        assert!(
            Config::from_lookup(lookup(&[("COMPME_MIDLINE", switch_value(true))])).allow_mid_word
        );
        assert!(
            !Config::from_lookup(lookup(&[("COMPME_MIDLINE", switch_value(false))])).allow_mid_word
        );
    }

    #[test]
    fn emoji_persist_value_round_trips_through_the_parser() {
        assert!(
            Config::from_lookup(lookup(&[("COMPME_EMOJI", switch_value(true))]))
                .emoji
                .is_some()
        );
        assert!(
            Config::from_lookup(lookup(&[("COMPME_EMOJI", switch_value(false))]))
                .emoji
                .is_none()
        );
        assert_eq!(
            Config::from_lookup(lookup(&[
                ("COMPME_EMOJI", "1"),
                ("COMPME_EMOJI_SKIN_TONE", "medium-light"),
            ]))
            .emoji
            .unwrap()
            .skin_tone,
            SkinTone::MediumLight
        );
    }

    #[test]
    fn emoji_toggle_preserves_custom_prefs_within_the_session() {
        let mut config_emoji = Some(EmojiPrefs {
            skin_tone: SkinTone::MediumDark,
            gender: Gender::Female,
        });
        let mut saved = config_emoji.unwrap();

        apply_emoji_enabled(&mut config_emoji, &mut saved, false);
        assert!(config_emoji.is_none());

        apply_emoji_enabled(&mut config_emoji, &mut saved, true);
        assert_eq!(
            config_emoji,
            Some(EmojiPrefs {
                skin_tone: SkinTone::MediumDark,
                gender: Gender::Female,
            })
        );
    }

    #[test]
    fn emoji_switch_edge_applies_config_and_persists_only_on_change() {
        let flag = AtomicBool::new(true);
        let mut current = true;
        let mut config_emoji = Some(EmojiPrefs {
            skin_tone: SkinTone::MediumDark,
            gender: Gender::Female,
        });
        let mut saved = config_emoji.unwrap();
        let mut persisted = Vec::new();

        assert_eq!(
            handle_emoji_switch_edge(&flag, &mut current, &mut config_emoji, &mut saved, |on| {
                persisted.push(on)
            },),
            None
        );
        assert_eq!(persisted, Vec::<bool>::new());

        flag.store(false, Ordering::Relaxed);
        assert_eq!(
            handle_emoji_switch_edge(&flag, &mut current, &mut config_emoji, &mut saved, |on| {
                persisted.push(on)
            },),
            Some(false)
        );
        assert!(config_emoji.is_none());
        assert_eq!(persisted, vec![false]);

        flag.store(true, Ordering::Relaxed);
        assert_eq!(
            handle_emoji_switch_edge(&flag, &mut current, &mut config_emoji, &mut saved, |on| {
                persisted.push(on)
            },),
            Some(true)
        );
        assert_eq!(
            config_emoji,
            Some(EmojiPrefs {
                skin_tone: SkinTone::MediumDark,
                gender: Gender::Female,
            })
        );
        assert_eq!(persisted, vec![false, true]);
    }

    #[test]
    fn disabled_emoji_preserves_persisted_skin_tone_for_later_enable() {
        let config = Config::from_lookup(lookup(&[
            ("COMPME_EMOJI", "0"),
            ("COMPME_EMOJI_SKIN_TONE", "dark"),
            ("COMPME_EMOJI_GENDER", "female"),
        ]));
        assert_eq!(config.emoji, None);
        assert_eq!(
            config.emoji_prefs,
            EmojiPrefs {
                skin_tone: SkinTone::Dark,
                gender: Gender::Female,
            }
        );

        let flags = build_settings_flags(&config, Arc::new(AtomicBool::new(true)), 16);
        assert_eq!(
            flags.emoji_skin_tone_index.load(Ordering::Relaxed),
            emoji_skin_tone_index(SkinTone::Dark)
        );

        let enabled_flag = AtomicBool::new(true);
        let mut enabled = false;
        let mut config_emoji = config.emoji;
        let mut saved = config.emoji_prefs;
        handle_emoji_switch_edge(
            &enabled_flag,
            &mut enabled,
            &mut config_emoji,
            &mut saved,
            |_| {},
        );
        assert_eq!(
            config_emoji,
            Some(EmojiPrefs {
                skin_tone: SkinTone::Dark,
                gender: Gender::Female,
            })
        );
    }

    #[test]
    fn emoji_skin_tone_edge_applies_config_and_persists_only_on_change() {
        let index = AtomicUsize::new(emoji_skin_tone_index(SkinTone::MediumDark));
        let mut current = emoji_skin_tone_index(SkinTone::MediumDark);
        let mut config_emoji = Some(EmojiPrefs {
            skin_tone: SkinTone::MediumDark,
            gender: Gender::Female,
        });
        let mut saved = config_emoji.unwrap();
        let mut persisted = Vec::new();

        assert_eq!(
            handle_emoji_skin_tone_change(
                &index,
                &mut current,
                &mut config_emoji,
                &mut saved,
                |value| persisted.push(value.to_string()),
            ),
            None
        );
        assert_eq!(persisted, Vec::<String>::new());

        index.store(emoji_skin_tone_index(SkinTone::Light), Ordering::Relaxed);
        assert_eq!(
            handle_emoji_skin_tone_change(
                &index,
                &mut current,
                &mut config_emoji,
                &mut saved,
                |value| persisted.push(value.to_string()),
            ),
            Some(SkinTone::Light)
        );
        assert_eq!(
            config_emoji,
            Some(EmojiPrefs {
                skin_tone: SkinTone::Light,
                gender: Gender::Female,
            })
        );
        assert_eq!(saved.skin_tone, SkinTone::Light);
        assert_eq!(persisted, vec!["light"]);
    }

    #[test]
    fn emoji_skin_tone_change_persists_saved_prefs_while_emoji_disabled() {
        // config_emoji=None (Emoji disabled). Every other emoji test passes Some,
        // so moving `saved_prefs.skin_tone = tone` inside the `if let Some(prefs)`
        // block would drop persistence here yet stay green. The saved prefs must
        // update so re-enabling Emoji restores the chosen tone.
        let index = AtomicUsize::new(emoji_skin_tone_index(SkinTone::Light));
        let mut current = emoji_skin_tone_index(SkinTone::MediumDark);
        let mut config_emoji: Option<EmojiPrefs> = None;
        let mut saved = EmojiPrefs {
            skin_tone: SkinTone::MediumDark,
            gender: Gender::Female,
        };
        let mut persisted = Vec::new();
        assert_eq!(
            handle_emoji_skin_tone_change(
                &index,
                &mut current,
                &mut config_emoji,
                &mut saved,
                |value| persisted.push(value.to_string()),
            ),
            Some(SkinTone::Light)
        );
        assert!(config_emoji.is_none(), "Emoji must stay disabled");
        assert_eq!(saved.skin_tone, SkinTone::Light);
        assert_eq!(persisted, vec!["light"]);
    }

    #[test]
    fn emoji_gender_change_persists_saved_prefs_while_emoji_disabled() {
        // Same disabled-state persistence contract for gender.
        let index = AtomicUsize::new(emoji_gender_index(Gender::Male));
        let mut current = emoji_gender_index(Gender::Female);
        let mut config_emoji: Option<EmojiPrefs> = None;
        let mut saved = EmojiPrefs {
            skin_tone: SkinTone::MediumDark,
            gender: Gender::Female,
        };
        let mut persisted = Vec::new();
        let out = handle_emoji_gender_change(
            &index,
            &mut current,
            &mut config_emoji,
            &mut saved,
            |value| persisted.push(value.to_string()),
        );
        assert_eq!(out, Some(Gender::Male));
        assert!(config_emoji.is_none(), "Emoji must stay disabled");
        assert_eq!(saved.gender, Gender::Male);
        assert_eq!(
            persisted,
            vec![emoji_gender_value(Gender::Male).to_string()]
        );
    }

    #[test]
    fn enqueue_deep_link_bounds_queue_fifo_and_rejects_oversize() {
        // No direct test drove enqueue_deep_link's cap/oversize branches (only the
        // handle_deep_link caller was tested). Pin FIFO evict-oldest at the cap and
        // the oversize reject.
        let mut q: Vec<String> = Vec::new();
        for i in 0..MAX_DEEP_LINK_QUEUE {
            assert!(enqueue_deep_link(&mut q, format!("compme://{i}")));
        }
        assert_eq!(q.len(), MAX_DEEP_LINK_QUEUE);
        // Full queue: accept the new url, evict the OLDEST (FIFO), stay at cap.
        assert!(enqueue_deep_link(&mut q, "compme://new".into()));
        assert_eq!(q.len(), MAX_DEEP_LINK_QUEUE);
        assert_eq!(q[0], "compme://1", "oldest (compme://0) must be evicted");
        assert_eq!(q[MAX_DEEP_LINK_QUEUE - 1], "compme://new");
        // Oversize url rejected; queue untouched.
        let big = "x".repeat(MAX_DEEP_LINK_URL_CHARS + 1);
        assert!(!enqueue_deep_link(&mut q, big));
        assert_eq!(q.len(), MAX_DEEP_LINK_QUEUE);
        assert_eq!(q[MAX_DEEP_LINK_QUEUE - 1], "compme://new");
    }

    #[test]
    fn emoji_gender_edge_applies_config_and_persists_only_on_change() {
        let index = AtomicUsize::new(emoji_gender_index(Gender::Female));
        let mut current = emoji_gender_index(Gender::Female);
        let mut config_emoji = Some(EmojiPrefs {
            skin_tone: SkinTone::Medium,
            gender: Gender::Female,
        });
        let mut saved = config_emoji.unwrap();
        let mut persisted = Vec::new();

        // No change → None, nothing persisted.
        assert_eq!(
            handle_emoji_gender_change(
                &index,
                &mut current,
                &mut config_emoji,
                &mut saved,
                |value| persisted.push(value.to_string()),
            ),
            None
        );
        assert_eq!(persisted, Vec::<String>::new());

        // Change to Male → applies to config + saved, persists "male".
        index.store(emoji_gender_index(Gender::Male), Ordering::Relaxed);
        assert_eq!(
            handle_emoji_gender_change(
                &index,
                &mut current,
                &mut config_emoji,
                &mut saved,
                |value| persisted.push(value.to_string()),
            ),
            Some(Gender::Male)
        );
        assert_eq!(
            config_emoji,
            Some(EmojiPrefs {
                skin_tone: SkinTone::Medium,
                gender: Gender::Male,
            })
        );
        assert_eq!(saved.gender, Gender::Male);
        assert_eq!(persisted, vec!["male"]);

        // Index↔value round-trip for every variant; OOB clamps to the default.
        for g in [Gender::Neutral, Gender::Female, Gender::Male] {
            assert_eq!(emoji_gender_from_index(emoji_gender_index(g)), g);
        }
        assert_eq!(emoji_gender_from_index(99), Gender::Neutral);
        assert_eq!(emoji_gender_value(Gender::Neutral), "neutral");
        assert_eq!(parse_gender(Some("male".into())), Gender::Male);
    }

    #[test]
    fn skin_tone_index_round_trips_all_variants_and_clamps_oob() {
        // The skin-tone popup addresses `EMOJI_SKIN_TONE_VALUES` by index, so
        // `emoji_skin_tone_from_index` must invert `emoji_skin_tone_index` for
        // every variant. An out-of-range index clamps to the documented default
        // (`SkinTone::default()` == `SkinTone::Default`), mirroring the gender
        // round-trip above.
        for tone in [
            SkinTone::Default,
            SkinTone::Light,
            SkinTone::MediumLight,
            SkinTone::Medium,
            SkinTone::MediumDark,
            SkinTone::Dark,
        ] {
            assert_eq!(
                emoji_skin_tone_from_index(emoji_skin_tone_index(tone)),
                tone,
                "index round-trip must be lossless for {tone:?}"
            );
        }
        assert_eq!(emoji_skin_tone_from_index(99), SkinTone::Default);
    }

    #[test]
    fn handle_emoji_skin_tone_change_clamps_out_of_range_atomic_to_last_tone() {
        // A bogus atomic index (e.g. 99) must clamp to the last addressable tone
        // via `.min(EMOJI_SKIN_TONE_VALUES.len() - 1)` — not panic or fall back to
        // the default. The last entry is `SkinTone::Dark` ("dark").
        let index = AtomicUsize::new(99);
        let mut current = emoji_skin_tone_index(SkinTone::Default);
        let mut config_emoji = Some(EmojiPrefs::default());
        let mut saved_prefs = EmojiPrefs::default();
        let mut persisted: Option<&'static str> = None;
        let result = handle_emoji_skin_tone_change(
            &index,
            &mut current,
            &mut config_emoji,
            &mut saved_prefs,
            |value| persisted = Some(value),
        );
        assert_eq!(result, Some(SkinTone::Dark));
        assert_eq!(current, emoji_skin_tone_index(SkinTone::Dark));
        assert_eq!(saved_prefs.skin_tone, SkinTone::Dark);
        assert_eq!(config_emoji.unwrap().skin_tone, SkinTone::Dark);
        assert_eq!(persisted, Some("dark"));
    }

    #[test]
    fn handle_emoji_gender_change_clamps_out_of_range_atomic_to_last_gender() {
        // Gender twin of the skin-tone clamp test: a bogus atomic index clamps to
        // the last gender via `.min(EMOJI_GENDER_VALUES.len() - 1)`. The last entry
        // is `Gender::Male` ("male").
        let index = AtomicUsize::new(99);
        let mut current = emoji_gender_index(Gender::Neutral);
        let mut config_emoji = Some(EmojiPrefs::default());
        let mut saved_prefs = EmojiPrefs::default();
        let mut persisted: Option<&'static str> = None;
        let result = handle_emoji_gender_change(
            &index,
            &mut current,
            &mut config_emoji,
            &mut saved_prefs,
            |value| persisted = Some(value),
        );
        assert_eq!(result, Some(Gender::Male));
        assert_eq!(current, emoji_gender_index(Gender::Male));
        assert_eq!(saved_prefs.gender, Gender::Male);
        assert_eq!(config_emoji.unwrap().gender, Gender::Male);
        assert_eq!(persisted, Some("male"));
    }

    #[test]
    fn emoji_gender_edge_invalidates_stale_visible_suggestion() {
        let index = AtomicUsize::new(emoji_gender_index(Gender::Male));
        let mut current = emoji_gender_index(Gender::Neutral);
        let mut config_emoji = Some(EmojiPrefs::default());
        let mut saved = EmojiPrefs::default();
        let mut persisted = Vec::new();
        let mut invalidations = 0;

        assert_eq!(
            handle_emoji_gender_change_with_invalidation(
                &index,
                &mut current,
                &mut config_emoji,
                &mut saved,
                |value| persisted.push(value.to_string()),
                || invalidations += 1,
            ),
            Some(Gender::Male)
        );
        assert_eq!(persisted, vec!["male"]);
        assert_eq!(invalidations, 1);
    }

    #[test]
    fn emoji_skin_tone_edge_invalidates_stale_visible_suggestion() {
        let index = AtomicUsize::new(emoji_skin_tone_index(SkinTone::Dark));
        let mut current = emoji_skin_tone_index(SkinTone::Default);
        let mut config_emoji = Some(EmojiPrefs::default());
        let mut saved = EmojiPrefs::default();
        let mut persisted = Vec::new();
        let mut invalidations = 0;

        assert_eq!(
            handle_emoji_skin_tone_change_with_invalidation(
                &index,
                &mut current,
                &mut config_emoji,
                &mut saved,
                |value| persisted.push(value.to_string()),
                || invalidations += 1,
            ),
            Some(SkinTone::Dark)
        );
        assert_eq!(persisted, vec!["dark"]);
        assert_eq!(invalidations, 1);
    }

    #[test]
    fn emoji_edges_with_unchanged_index_never_invalidate_the_visible_suggestion() {
        // A no-op popup edit (same index as `current`) must NOT clear the
        // showing suggestion: `handle_*_change` short-circuits to `None`, so the
        // `_with_invalidation` wrapper never runs `invalidate_visible_suggestion`.
        // A changed index proves the invalidation path is still wired. Mirrors
        // `emoji_{gender,skin_tone}_edge_invalidates_stale_visible_suggestion`.

        // --- Skin tone: same index → no invalidation, no persist, None. ---
        let index = AtomicUsize::new(emoji_skin_tone_index(SkinTone::Medium));
        let mut current = emoji_skin_tone_index(SkinTone::Medium);
        let mut config_emoji = Some(EmojiPrefs::default());
        let mut saved = EmojiPrefs::default();
        let mut persisted = Vec::new();
        let mut invalidations = 0;

        assert_eq!(
            handle_emoji_skin_tone_change_with_invalidation(
                &index,
                &mut current,
                &mut config_emoji,
                &mut saved,
                |value| persisted.push(value.to_string()),
                || invalidations += 1,
            ),
            None,
            "unchanged skin-tone index is a no-op edit"
        );
        assert_eq!(persisted, Vec::<String>::new());
        assert_eq!(
            invalidations, 0,
            "a no-op skin-tone edit must not clear the showing suggestion"
        );

        // A genuine change DOES invalidate, proving the path is still wired.
        index.store(emoji_skin_tone_index(SkinTone::Dark), Ordering::Relaxed);
        assert_eq!(
            handle_emoji_skin_tone_change_with_invalidation(
                &index,
                &mut current,
                &mut config_emoji,
                &mut saved,
                |value| persisted.push(value.to_string()),
                || invalidations += 1,
            ),
            Some(SkinTone::Dark)
        );
        assert_eq!(invalidations, 1);

        // --- Gender: same index → no invalidation, no persist, None. ---
        let index = AtomicUsize::new(emoji_gender_index(Gender::Female));
        let mut current = emoji_gender_index(Gender::Female);
        let mut config_emoji = Some(EmojiPrefs::default());
        let mut saved = EmojiPrefs::default();
        let mut persisted = Vec::new();
        let mut invalidations = 0;

        assert_eq!(
            handle_emoji_gender_change_with_invalidation(
                &index,
                &mut current,
                &mut config_emoji,
                &mut saved,
                |value| persisted.push(value.to_string()),
                || invalidations += 1,
            ),
            None,
            "unchanged gender index is a no-op edit"
        );
        assert_eq!(persisted, Vec::<String>::new());
        assert_eq!(
            invalidations, 0,
            "a no-op gender edit must not clear the showing suggestion"
        );

        // A genuine change DOES invalidate.
        index.store(emoji_gender_index(Gender::Male), Ordering::Relaxed);
        assert_eq!(
            handle_emoji_gender_change_with_invalidation(
                &index,
                &mut current,
                &mut config_emoji,
                &mut saved,
                |value| persisted.push(value.to_string()),
                || invalidations += 1,
            ),
            Some(Gender::Male)
        );
        assert_eq!(invalidations, 1);
    }

    #[test]
    fn snooze_duration_matches_the_rendered_wording() {
        // AppStatus::render_line says "Snoozed for up to 1 hour" (a &'static
        // str). If SNOOZE_MINUTES ever changes, that wording must follow.
        assert_eq!(
            SNOOZE_MINUTES, 60,
            "update AppStatus::render_line's 'up to 1 hour' wording (status.rs) \
             together with SNOOZE_MINUTES"
        );
        assert!(AppStatus::Ready.render_line(true).contains("1 hour"));
    }

    #[test]
    fn deep_links_apply_overrides_and_fail_closed() {
        let mut prefs = Prefs::default();
        // A valid unsigned exclude applies and names the action.
        let summary = handle_deep_link(
            "compme://setOverride?app=com.apple.TextEdit&excluded=true",
            None,
            &mut prefs,
            |_| true,
        )
        .expect("valid link applies");
        assert!(summary.contains("com.apple.TextEdit"), "{summary}");
        assert!(prefs.excluded_apps.contains("com.apple.TextEdit"));
        // Garbage fails with the parser's message, prefs untouched.
        let err = handle_deep_link("compme://setEverything?x=1", None, &mut prefs, |_| true)
            .expect_err("unknown command must fail");
        assert!(err.contains("unknown command"), "{err}");
        // A signed link without a configured trusted key fails closed.
        let err = handle_deep_link(
            &format!(
                "compme://setOverride?app=com.apple.TextEdit&enabled=true&sig={}",
                "ab".repeat(64)
            ),
            None,
            &mut prefs,
            |_| true,
        )
        .expect_err("signed link without a key must fail");
        assert!(err.contains("no trusted key"), "{err}");
    }

    #[test]
    fn only_a_verified_signed_deep_link_reaches_confirmation_and_mutates_prefs() {
        // RFC 8032-compatible deterministic fixture: private seed [7; 32].
        // Keeping the public key, payload, and signature as literals makes the
        // expected trust decision independent of the production signer/parser.
        let trusted = webconfig::TrustedKey::from_hex(
            "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c",
        )
        .expect("fixture public key");
        let signed = concat!(
            "compme://setOverride?app=com.apple.TextEdit&excluded=true",
            "&sig=721848ed25850b98440cdb91f5077077b8f1077446be885c3b8c6b3c3a2a986f",
            "8884b34489c675afdc344af112d58251f8df40098903d97a861605baa667a005",
        );
        let mut prefs = Prefs::default();
        let confirmations = RefCell::new(Vec::new());

        let summary = handle_deep_link(signed, Some(&trusted), &mut prefs, |decision| {
            confirmations.borrow_mut().push(decision.clone());
            true
        })
        .expect("a valid signed link should reach host confirmation and apply");

        assert_eq!(
            confirmations.into_inner(),
            vec![webconfig::PromptDecision {
                scope: "com.apple.TextEdit".to_string(),
                action: "Exclude".to_string(),
                trust: "signed link, verified".to_string(),
            }],
        );
        assert!(summary.contains("Signed link"), "{summary}");
        assert!(prefs.excluded_apps.contains("com.apple.TextEdit"));

        // Changing the signed scope without resigning must fail before the host
        // prompts or mutates the already-established policy.
        let before = prefs.clone();
        let tampered = signed.replace("com.apple.TextEdit", "com.apple.Mail");
        let prompted = Cell::new(false);
        let err = handle_deep_link(&tampered, Some(&trusted), &mut prefs, |_| {
            prompted.set(true);
            true
        })
        .expect_err("tampered payload must fail closed");
        assert_eq!(err, "signature verification failed");
        assert!(
            !prompted.get(),
            "unverified payload must not reach confirmation"
        );
        assert_eq!(prefs, before, "unverified payload must not mutate policy");
    }

    #[test]
    fn accept_key_config_parses_keycodes_and_rejects_junk() {
        // Raw macOS virtual keycodes (the future shortcuts-pane recorder
        // emits keycodes too); junk → None → default bindings.
        let config = Config::from_lookup(lookup(&[
            ("COMPME_ACCEPT_WORD_KEY", "122"),
            ("COMPME_ACCEPT_FULL_KEY", "120"),
        ]));
        assert_eq!(config.accept_word_key, Some((122, 0)));
        assert_eq!(config.accept_full_key, Some((120, 0)));
        let junk = Config::from_lookup(lookup(&[("COMPME_ACCEPT_WORD_KEY", "tab")]));
        assert_eq!(junk.accept_word_key, None);
        assert_eq!(Config::from_lookup(lookup(&[])).accept_word_key, None);
        // Modifier combos parse into (keycode, Carbon mask): shift=512, ctrl=4096
        // (slice 1b — Shift+Tab etc. configurable via the persisted string).
        let combo = Config::from_lookup(lookup(&[
            ("COMPME_ACCEPT_WORD_KEY", "shift+48"),
            ("COMPME_ACCEPT_FULL_KEY", "ctrl+shift+50"),
        ]));
        assert_eq!(combo.accept_word_key, Some((48, 512)));
        assert_eq!(combo.accept_full_key, Some((50, 512 | 4096)));
    }

    #[test]
    fn emoji_and_memory_config_synonyms_map_to_the_right_variant() {
        // The enum/synonym parse arms each fall to a SAFE default on no-match, so
        // a dropped/typo'd arm is silent — pin every documented synonym + the
        // trim/case handling + the default fallback.
        assert_eq!(parse_gender(Some("male".into())), Gender::Male);
        assert_eq!(parse_gender(Some(" Female ".into())), Gender::Female);
        assert_eq!(parse_gender(Some("nonbinary".into())), Gender::Neutral);

        assert_eq!(parse_skin_tone(Some("light".into())), SkinTone::Light);
        assert_eq!(parse_skin_tone(Some("medium".into())), SkinTone::Medium);
        assert_eq!(
            parse_skin_tone(Some("medium_light".into())),
            SkinTone::MediumLight
        );
        assert_eq!(
            parse_skin_tone(Some("medium_dark".into())),
            SkinTone::MediumDark
        );
        assert_eq!(parse_skin_tone(Some("dark".into())), SkinTone::Dark);
        assert_eq!(parse_skin_tone(Some("bogus".into())), SkinTone::Default);

        // The third storage-mode alias `all_monitored` (siblings `all`/`monitored`
        // are already pinned elsewhere).
        assert_eq!(
            parse_storage_mode(Some("all_monitored".into())),
            memory::StorageMode::AllMonitored
        );

        // Tri-state truthy/falsy synonyms + trim/case + unrecognized → None.
        for v in ["1", "true", "on", "yes", " YES "] {
            assert_eq!(parse_tri_state(Some(v.into())), Some(true), "{v}");
        }
        for v in ["0", "false", "off", "no", " No "] {
            assert_eq!(parse_tri_state(Some(v.into())), Some(false), "{v}");
        }
        assert_eq!(parse_tri_state(Some("maybe".into())), None);
        assert_eq!(parse_tri_state(None), None);
    }

    #[test]
    fn a_declined_prompt_rejects_the_link_and_leaves_prefs_untouched() {
        let mut prefs = Prefs::default();
        let err = handle_deep_link(
            "compme://setOverride?app=com.apple.TextEdit&excluded=true",
            None,
            &mut prefs,
            |_| false, // user clicks Cancel
        )
        .expect_err("declined prompt must reject");
        assert!(err.contains("declined"), "{err}");
        assert!(prefs.excluded_apps.is_empty(), "prefs must be untouched");
    }

    #[test]
    fn launch_at_login_applies_only_when_the_key_is_explicitly_set() {
        // Absent: leave the user's Login Items setting alone.
        assert_eq!(Config::from_lookup(lookup(&[])).launch_at_login, None);
        // Explicit true/false apply.
        assert_eq!(
            Config::from_lookup(lookup(&[("COMPME_LAUNCH_AT_LOGIN", "true")])).launch_at_login,
            Some(true)
        );
        assert_eq!(
            Config::from_lookup(lookup(&[("COMPME_LAUNCH_AT_LOGIN", "0")])).launch_at_login,
            Some(false)
        );
        // Junk fails safe to leave-alone, NOT to a register/unregister.
        assert_eq!(
            Config::from_lookup(lookup(&[("COMPME_LAUNCH_AT_LOGIN", "maybe")])).launch_at_login,
            None
        );
    }

    #[test]
    fn per_app_feature_lists_parse_with_off_winning_conflicts() {
        let prefs = build_prefs(&lookup(&[
            ("COMPME_MIDLINE_ON_APPS", "com.a.one, com.a.both"),
            ("COMPME_MIDLINE_OFF_APPS", "com.a.both"),
            ("COMPME_AUTOCORRECT_OFF_APPS", "com.a.two"),
        ]));
        assert!(prefs.mid_line_enabled(Some("com.a.one"), false), "ON list");
        assert!(
            !prefs.mid_line_enabled(Some("com.a.both"), true),
            "OFF wins the conflict"
        );
        assert!(!prefs.autocorrect_enabled(Some("com.a.two"), true));
        // (Write-back serializers land with the settings-pane watcher — their
        // first production caller; the c22 no-unused-fns rule.)
    }

    #[test]
    fn per_app_autocorrect_override_gates_the_replacement_offer() {
        // Global autocorrect ON, per-app OFF: the typo fix must not offer in
        // that app, while emoji (an unrelated feature) still does elsewhere.
        let config = Config::from_lookup(lookup(&[
            ("COMPME_AUTOCORRECT", "1"),
            ("COMPME_AUTOCORRECT_OFF_APPS", "com.quiet.app"),
        ]));
        // `teh` is a known typo; in the opted-out app no offer fires…
        assert_eq!(
            replacement_decision(
                "teh",
                &config,
                &config.prefs,
                Some("com.quiet.app"),
                None,
                true,
                0
            ),
            None
        );
        // …but the same input in another app offers the fix.
        assert!(replacement_decision(
            "teh",
            &config,
            &config.prefs,
            Some("com.other.app"),
            None,
            true,
            0
        )
        .is_some());
    }

    #[test]
    fn per_app_thesaurus_override_survives_persistence_and_gates_the_offer() {
        let configured = Config::from_lookup(lookup(&[
            ("COMPME_THESAURUS", "0"),
            (
                "COMPME_THESAURUS_ON_APPS",
                "com.example.writer,com.example.conflict",
            ),
            ("COMPME_THESAURUS_OFF_APPS", "com.example.conflict"),
        ]));
        let dir = std::env::temp_dir().join(format!(
            "compme-per-app-thesaurus-persist-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.env");

        // Any prefs persistence edge rewrites every per-app policy category.
        // The config-only thesaurus override must survive that rewrite too.
        persist_web_override_prefs(&path, &configured.prefs);
        let map = config::load_file_map(&path);
        let reloaded = Config::from_lookup(|key| {
            (key == "COMPME_THESAURUS")
                .then(|| "0".to_string())
                .or_else(|| map.get(key).cloned())
        });

        assert_eq!(
            replacement_decision(
                "happy",
                &reloaded,
                &reloaded.prefs,
                Some("com.example.writer"),
                None,
                true,
                0,
            ),
            Some((
                vec![
                    "glad".to_string(),
                    "joyful".to_string(),
                    "cheerful".to_string(),
                    "content".to_string(),
                    "pleased".to_string(),
                    "delighted".to_string(),
                ],
                5,
            )),
            "the opted-in app should get the observable synonym candidates",
        );
        assert_eq!(
            replacement_decision(
                "happy",
                &reloaded,
                &reloaded.prefs,
                Some("com.example.other"),
                None,
                true,
                0,
            ),
            None,
            "an unconfigured app should inherit the global off state",
        );
        assert_eq!(
            replacement_decision(
                "happy",
                &reloaded,
                &reloaded.prefs,
                Some("com.example.conflict"),
                None,
                true,
                0,
            ),
            None,
            "the explicit per-app off list should win a conflicting on entry",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_collect_apps_env_parses_into_per_app_collect_overrides() {
        let prefs = build_prefs(&lookup(&[(
            "COMPME_NO_COLLECT_APPS",
            "com.apple.TextEdit, com.googlecode.iterm2",
        )]));
        assert!(!prefs.collection_allowed(Some("com.apple.TextEdit")));
        assert!(!prefs.collection_allowed(Some("com.googlecode.iterm2")));
        assert!(prefs.collection_allowed(Some("com.apple.Safari")));
    }

    #[test]
    fn toggling_app_collection_flips_state_and_serializes_stably() {
        let mut prefs = Prefs::default();
        // First toggle: disable.
        assert!(!toggle_app_collection(&mut prefs, "com.apple.TextEdit"));
        assert!(!prefs.collection_allowed(Some("com.apple.TextEdit")));
        // Stable sorted persistence value.
        assert!(!toggle_app_collection(&mut prefs, "com.apple.Finder"));
        assert_eq!(
            no_collect_apps_value(&prefs),
            "com.apple.Finder,com.apple.TextEdit"
        );
        // Second toggle: re-enable; value shrinks.
        assert!(toggle_app_collection(&mut prefs, "com.apple.Finder"));
        assert_eq!(no_collect_apps_value(&prefs), "com.apple.TextEdit");
    }

    #[test]
    fn collection_disabled_skips_both_recording_sinks() {
        // Per-app "Input Collection off" must silence BOTH sinks: the
        // previous-inputs context and the encrypted memory store.
        let previous = PreviousInputs::default();
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([3u8; 32]),
            memory::StorageMode::AcceptedOnly,
        )
        .expect("store");
        let field = FieldHandle {
            app: "com.apple.TextEdit".into(),
            pid: Some(42),
            element_id: "ax:1".into(),
            generation: 1,
        };

        record_full_accept(
            AcceptAction::Full,
            &field,
            "hello world",
            100,
            &previous,
            Some(&store),
            false, // collection disabled for this app
        );
        assert_eq!(store.count().expect("count"), 0, "memory must not record");
        assert!(
            previous.recent("com.apple.TextEdit").is_empty(),
            "previous-inputs must not record"
        );

        // Sanity: allowed -> both record.
        record_full_accept(
            AcceptAction::Full,
            &field,
            "hello world",
            100,
            &previous,
            Some(&store),
            true,
        );
        assert_eq!(store.count().expect("count"), 1);
        assert!(!previous.recent("com.apple.TextEdit").is_empty());
    }

    #[test]
    fn failed_accept_records_no_context_memory_or_accept_stats() {
        let previous = PreviousInputs::default();
        let store = accepted_store();
        let mut tracker = FieldTracker::new();
        let mut usage = stats::Stats::new();
        let prefs = Prefs::default();
        let preview = (
            field_with_app("com.apple.TextEdit"),
            "never inserted secret".to_string(),
            0usize,
        );

        apply_accept_side_effects(
            false,
            AcceptSideEffects {
                action: AcceptAction::Full,
                preview: Some(&preview),
                correction_preview: None,
                wall_ms: 10_000,
                context_max_chars: 160,
                previous_inputs: &previous,
                memory: Some(&store),
                prefs: &prefs,
                tracker: &mut tracker,
                usage: &mut usage,
            },
        );

        assert_eq!(store.count().unwrap(), 0);
        assert!(previous.recent("com.apple.TextEdit").is_empty());
        let totals = usage.session_totals();
        assert_eq!(totals.counts.accepted, 0);
        assert_eq!(totals.words, 0);
    }

    #[test]
    fn successful_accept_records_context_memory_and_accept_stats() {
        let previous = PreviousInputs::default();
        let store = accepted_store();
        let mut tracker = FieldTracker::new();
        let mut usage = stats::Stats::new();
        let prefs = Prefs::default();
        let preview = (
            field_with_app("com.apple.TextEdit"),
            "accepted words".to_string(),
            0usize,
        );

        apply_accept_side_effects(
            true,
            AcceptSideEffects {
                action: AcceptAction::Full,
                preview: Some(&preview),
                correction_preview: None,
                wall_ms: 10_000,
                context_max_chars: 160,
                previous_inputs: &previous,
                memory: Some(&store),
                prefs: &prefs,
                tracker: &mut tracker,
                usage: &mut usage,
            },
        );

        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(previous.recent("com.apple.TextEdit").len(), 1);
        let totals = usage.session_totals();
        assert_eq!(totals.counts.accepted, 1);
        assert_eq!(totals.words, 2);
    }

    #[test]
    fn replacement_accept_absorbs_the_delete_then_insert_echo() {
        // A REPLACEMENT accept (`replace_left > 0`, e.g. an emoji `:smile`→😄
        // swap) routes to `apply_self_replace`, which deletes the typed token
        // before inserting. The tracker baseline must end delete-then-inserted so
        // the field's own AX readback is absorbed as a caret move — not mistaken
        // for fresh typing. The two existing accept tests use `replace_left == 0`
        // (append-only), leaving this branch unexercised.
        let previous = PreviousInputs::default();
        let store = accepted_store();
        let mut tracker = FieldTracker::new();
        let mut usage = stats::Stats::new();
        let prefs = Prefs::default();
        let field = field_with_app("com.apple.TextEdit");

        // Seed a baseline of "x:smile" (caret at 7) so the replace branch has a
        // baseline to delete-then-insert against.
        tracker.observe(
            &field,
            &text_context(&field, "x:smile"),
            TriggerPolicy::Automatic,
            0,
        );

        let preview = (field.clone(), "😄".to_string(), 6usize);
        apply_accept_side_effects(
            true,
            AcceptSideEffects {
                action: AcceptAction::Full,
                preview: Some(&preview),
                correction_preview: None,
                wall_ms: 10_000,
                context_max_chars: 160,
                previous_inputs: &previous,
                memory: Some(&store),
                prefs: &prefs,
                tracker: &mut tracker,
                usage: &mut usage,
            },
        );

        // The replace deleted ":smile" and inserted "😄": the baseline now reads
        // "x😄" (caret at 2). The field's matching readback must absorb as a pure
        // caret move with no spurious echo armed.
        let observed = tracker.observe(
            &field,
            &text_context(&field, "x😄"),
            TriggerPolicy::Automatic,
            1,
        );
        assert_eq!(
            observed,
            Observation::CaretMoved {
                field: field.clone(),
                caret: 2,
            },
            "replacement accept must leave the baseline delete-then-inserted"
        );
        // Sanity: the accept still recorded its stats and sinks.
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(usage.session_totals().counts.accepted, 1);
    }

    #[test]
    fn correction_accept_absorbs_exact_range_echo_and_records_stats() {
        let previous = PreviousInputs::default();
        let store = accepted_store();
        let mut tracker = FieldTracker::new();
        let mut usage = stats::Stats::new();
        let prefs = Prefs::default();
        let field = field_with_app("com.apple.TextEdit");
        tracker.observe(
            &field,
            &text_context(&field, "I saw teh"),
            TriggerPolicy::Automatic,
            0,
        );
        let correction = (
            field.clone(),
            "the".to_string(),
            CorrectionRange { start: 6, end: 9 },
        );

        apply_accept_side_effects(
            true,
            AcceptSideEffects {
                action: AcceptAction::Correction,
                preview: None,
                correction_preview: Some(&correction),
                wall_ms: 10_000,
                context_max_chars: 160,
                previous_inputs: &previous,
                memory: Some(&store),
                prefs: &prefs,
                tracker: &mut tracker,
                usage: &mut usage,
            },
        );

        let observed = tracker.observe(
            &field,
            &text_context(&field, "I saw the"),
            TriggerPolicy::Automatic,
            1,
        );
        assert_eq!(
            observed,
            Observation::CaretMoved {
                field: field.clone(),
                caret: 9,
            }
        );
        let totals = usage.session_totals();
        assert_eq!(totals.counts.accepted, 1);
        assert_eq!(totals.words, 1);
        assert_eq!(
            store.count().unwrap(),
            0,
            "corrections are not full accepts"
        );
        assert!(previous.recent("com.apple.TextEdit").is_empty());
    }

    #[test]
    fn correction_accept_absorbs_app_normalized_readback_as_caret_move() {
        // The app normalized the landed correction on write (here it
        // autocapitalized "the" to "The"), so the next AX readback differs from
        // the text the correction intended. It is still the accept's own echo —
        // not new typing — so it must absorb as a caret move. Before the
        // one-shot resync, the tracker seeded the INTENDED "the" and the
        // normalized "The" readback diffed into a synthetic same-length change,
        // arming a spurious request and routing "The" into monitored memory.
        let previous = PreviousInputs::default();
        let store = accepted_store();
        let mut tracker = FieldTracker::new();
        let mut usage = stats::Stats::new();
        let prefs = Prefs::default();
        let field = field_with_app("com.apple.TextEdit");
        tracker.observe(
            &field,
            &text_context(&field, "I saw teh"),
            TriggerPolicy::Automatic,
            0,
        );
        let correction = (
            field.clone(),
            "the".to_string(),
            CorrectionRange { start: 6, end: 9 },
        );

        apply_accept_side_effects(
            true,
            AcceptSideEffects {
                action: AcceptAction::Correction,
                preview: None,
                correction_preview: Some(&correction),
                wall_ms: 10_000,
                context_max_chars: 160,
                previous_inputs: &previous,
                memory: Some(&store),
                prefs: &prefs,
                tracker: &mut tracker,
                usage: &mut usage,
            },
        );

        // Readback is the app-normalized form ("The"), NOT the intended "the".
        let observed = tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, "I saw The"),
            TriggerPolicy::Automatic,
            1,
        );
        match observed {
            Observation::CaretMoved { caret, .. } => assert_eq!(caret, 9),
            Observation::Typed(change) => panic!(
                "normalized correction echo must absorb as a caret move, not \
                 synthesize typing (inserted_text={:?})",
                change.inserted_text
            ),
        }
    }

    #[test]
    fn completion_outcome_log_line_never_includes_candidate_text() {
        let line = completion_outcome_log_line(7, &["secret phrase".into(), "other".into()]);

        assert!(line.contains("gen=7"));
        assert!(line.contains("candidate_count=2"));
        assert!(line.contains("candidate_lengths=[13, 5]"));
        assert!(
            !line.contains("secret phrase"),
            "diagnostics must not emit raw completion text"
        );
    }

    #[test]
    fn replacement_debug_log_line_redacts_left_context() {
        let secret = "sk-abcdEFGH0123456789abcdEFGH0123";
        let line = replacement_debug_log_line(
            &format!("token {secret}"),
            true,
            false,
            false,
            true,
            "None",
        );

        assert!(line.contains("left="));
        assert!(!line.contains(secret));
        assert!(line.contains("[redacted-secret]"));
    }

    #[test]
    fn blocked_request_log_line_reports_gate_metadata_without_prompt_text() {
        let prefs = Prefs::default();
        let request = req_with_prompt("git status && print-secret");
        let line = request_log_line(
            &request,
            Some("com.apple.Terminal"),
            None,
            &prefs,
            1_000,
            Some("print-secret"),
            true,
        );

        assert!(line.contains("request blocked"));
        assert!(line.contains("prompt_chars=26"));
        assert!(line.contains("app=com.apple.Terminal"));
        assert!(line.contains("terminal_ok=false"));
        assert!(line.contains("prompt_marker=true"));
        assert!(!line.contains("git status"));
        assert!(!line.contains("print-secret"));
    }

    #[test]
    fn app_disable_arms_map_to_the_right_prefs_mutation() {
        let mut prefs = Prefs::default();
        // Hour: per-app snooze for 60 minutes, auto-resuming.
        apply_app_disable(DisableArm::Hour, "com.apple.TextEdit", &mut prefs, 1_000);
        assert!(prefs.is_app_snoozed("com.apple.TextEdit", 1_000 + 59 * 60_000));
        assert!(!prefs.is_app_snoozed("com.apple.TextEdit", 1_000 + 60 * 60_000));
        // UntilRelaunch: saturated deadline, session-only.
        apply_app_disable(
            DisableArm::UntilRelaunch,
            "com.apple.Safari",
            &mut prefs,
            1_000,
        );
        assert!(prefs.is_app_snoozed("com.apple.Safari", u64::MAX - 1));
        // Always: hard exclude (persisted by the caller).
        apply_app_disable(
            DisableArm::Always,
            "com.googlecode.iterm2",
            &mut prefs,
            1_000,
        );
        assert!(prefs.excluded_apps.contains("com.googlecode.iterm2"));
        // Excluded-apps persistence value: stable comma-joined sorted list,
        // round-trippable through the COMPME_EXCLUDED_APPS parser.
        prefs.excluded_apps.insert("com.apple.Finder".into());
        assert_eq!(
            excluded_apps_value(&prefs),
            "com.apple.Finder,com.googlecode.iterm2"
        );
    }

    #[test]
    fn snooze_request_snoozes_for_an_hour_and_is_consumed() {
        let mut prefs = Prefs::default();
        // Not requested → untouched.
        assert!(!apply_snooze_request(false, &mut prefs, 1_000));
        assert!(!prefs.is_snoozed(1_000));
        // Requested → snoozed for exactly SNOOZE_MINUTES from now.
        assert!(apply_snooze_request(true, &mut prefs, 1_000));
        assert!(prefs.is_snoozed(1_000));
        assert!(prefs.is_snoozed(1_000 + 59 * 60 * 1_000));
        assert!(!prefs.is_snoozed(1_000 + 60 * 60 * 1_000));
    }

    #[test]
    fn check_updates_flag_opens_the_updates_url_once() {
        // An armed "Check for Updates…" flag opens the release page exactly
        // once and is consumed by the tick that observed it.
        let flag = AtomicBool::new(true);
        let mut opened: Vec<&'static str> = Vec::new();
        handle_check_updates_flag(&flag, |url| opened.push(url));
        assert_eq!(opened, [UPDATES_URL]);
        assert!(!flag.load(Ordering::Relaxed));
        // Second tick: the consumed flag opens nothing.
        handle_check_updates_flag(&flag, |url| opened.push(url));
        assert_eq!(opened, [UPDATES_URL]);
    }

    #[test]
    fn config_enabled_reads_compme_enabled_and_defaults_on() {
        // The global tray-toggle state, persisted on toggle and read back at
        // launch. Distinct from COMPME_DEFAULT_ENABLED (the per-app
        // suggestion-policy default in prefs).
        assert!(Config::from_lookup(lookup(&[])).enabled);
        assert!(Config::from_lookup(lookup(&[("COMPME_ENABLED", "true")])).enabled);
        assert!(!Config::from_lookup(lookup(&[("COMPME_ENABLED", "false")])).enabled);
        assert!(!Config::from_lookup(lookup(&[("COMPME_ENABLED", "0")])).enabled);
    }

    #[test]
    fn prefs_default_enabled_fails_safe() {
        // Absent or unrecognized → enabled (a typo never silently kills the app);
        // only explicit falsy values disable.
        assert!(build_prefs(&lookup(&[])).default_enabled);
        assert!(build_prefs(&lookup(&[("COMPME_DEFAULT_ENABLED", "yes")])).default_enabled);
        assert!(build_prefs(&lookup(&[("COMPME_DEFAULT_ENABLED", "True")])).default_enabled);
        assert!(!build_prefs(&lookup(&[("COMPME_DEFAULT_ENABLED", "0")])).default_enabled);
        assert!(!build_prefs(&lookup(&[("COMPME_DEFAULT_ENABLED", "off")])).default_enabled);
    }

    #[test]
    fn clipboard_and_screen_context_flags_default_off() {
        let off = Config::from_lookup(lookup(&[]));
        assert!(!off.clipboard_context);
        assert!(!off.screen_context);
        assert!(!off.diag_context);
        assert_eq!(off.acceptance_prompt_marker, None);
        let on = Config::from_lookup(lookup(&[
            ("COMPME_CLIPBOARD_CONTEXT", "1"),
            ("COMPME_SCREEN_CONTEXT", "true"),
            ("COMPME_DIAG_CONTEXT", "true"),
            ("COMPME_ACCEPTANCE_PROMPT_MARKER", "run marker"),
        ]));
        assert!(on.clipboard_context);
        assert!(on.screen_context);
        assert!(on.diag_context);
        assert_eq!(on.acceptance_prompt_marker.as_deref(), Some("run marker"));
    }

    #[test]
    fn clipboard_diagnostic_reports_marker_without_raw_text() {
        let line = clipboard_diagnostic_line(
            Some("CLIPBOARD-CONTEXT-MARKER"),
            Some("CLIPBOARD-CONTEXT-MARKER"),
        );
        assert_eq!(line, "Some(chars=24 marker=true)");
        assert!(
            !line.contains("CLIPBOARD-CONTEXT-MARKER"),
            "diagnostic leaked marker text: {line:?}"
        );
        assert_eq!(
            clipboard_diagnostic_line(
                Some("other 24 character text"),
                Some("CLIPBOARD-CONTEXT-MARKER")
            ),
            "Some(chars=23 marker=false)"
        );
        assert_eq!(
            clipboard_diagnostic_line(None, Some("CLIPBOARD-CONTEXT-MARKER")),
            "None"
        );
    }

    #[test]
    fn unsupported_apps_are_gated_out() {
        assert!(!app_allows_suggestions(Some("com.mitchellh.ghostty")));
        assert!(app_allows_suggestions(Some("com.apple.TextEdit")));
        // Unresolved app → fail-open (field capabilities still gate).
        assert!(app_allows_suggestions(None));
    }

    #[test]
    fn sidebar_only_apps_are_blocked_until_field_detector_exists() {
        assert!(!app_allows_suggestions(Some("com.microsoft.VSCode")));
        assert!(!app_allows_suggestions(Some(
            "com.todesktop.230313mzl4w4u92"
        )));
        assert!(!app_allows_suggestions(Some("com.exafunction.windsurf")));
    }

    #[test]
    fn suggestion_gates_apply_to_local_replacements_too() {
        // The local replacement offer (emoji/typo/UK) shares this gate, so it is
        // suppressed exactly where a model completion would be.
        let prefs = Prefs::default();
        assert!(suggestion_gates_pass(
            Some("com.apple.TextEdit"),
            "color",
            None,
            &prefs,
            0
        ));
        // Sidebar-only app → blocked.
        assert!(!suggestion_gates_pass(
            Some("com.microsoft.VSCode"),
            "color",
            None,
            &prefs,
            0
        ));
        // Terminal with a shell-command line → blocked (not a natural-language prompt).
        assert!(!suggestion_gates_pass(
            Some("com.googlecode.iterm2"),
            "git status && ls -la",
            None,
            &prefs,
            0
        ));
    }

    #[test]
    fn live_keymap_apply_orders_set_rearm_persist_and_reverts_on_failure() {
        // Recorder 5b sequencing contract (banked c131 design): keymap
        // write FIRST (an old hotkey firing mid-swap reads the new map —
        // role-safe), re-arm SECOND, persist ONLY after the re-arm
        // succeeded. On re-arm failure the map REVERTS so
        // effective_accept_keys()/the Shortcuts pane keep telling the
        // registered truth (the c123 desync class).
        let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
        let l1 = std::rc::Rc::clone(&log);
        let l2 = std::rc::Rc::clone(&log);
        let l3 = std::rc::Rc::clone(&log);
        let ok = apply_live_accept_keymap(
            Some((35, 0)),
            Some((38, 0)),
            Some((96, 0)),
            |w, f, g| {
                l1.borrow_mut().push(format!("set:{w:?},{f:?},{g:?}"));
                Ok(())
            },
            || {
                l2.borrow_mut().push("rearm".into());
                Ok(())
            },
            |w, f, g| l3.borrow_mut().push(format!("persist:{w:?},{f:?},{g:?}")),
            || ((35, 0), (38, 0), Some((96, 0))),
        );
        assert!(ok.is_ok());
        assert_eq!(
            *log.borrow(),
            vec![
                "set:Some((35, 0)),Some((38, 0)),Some((96, 0))".to_string(),
                "rearm".to_string(),
                "persist:(35, 0),(38, 0),Some((96, 0))".to_string(),
            ]
        );

        // Failure path: set → rearm Err → REVERT set, no persist.
        let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
        let l1 = std::rc::Rc::clone(&log);
        let l2 = std::rc::Rc::clone(&log);
        let l3 = std::rc::Rc::clone(&log);
        let rearm_calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let calls = std::rc::Rc::clone(&rearm_calls);
        let err = apply_live_accept_keymap(
            Some((35, 0)),
            Some((38, 0)),
            Some((96, 0)),
            |w, f, g| {
                l1.borrow_mut().push(format!("set:{w:?},{f:?},{g:?}"));
                Ok(())
            },
            || {
                let call = calls.get() + 1;
                calls.set(call);
                l2.borrow_mut().push("rearm".into());
                if call == 1 {
                    Err(PlatformError::Timeout)
                } else {
                    Ok(())
                }
            },
            |w, f, g| l3.borrow_mut().push(format!("persist:{w:?},{f:?},{g:?}")),
            || ((48, 0), (50, 0), Some((96, 512))), // the pre-swap registered truth
        );
        assert!(err.is_err());
        assert_eq!(
            *log.borrow(),
            vec![
                "set:Some((35, 0)),Some((38, 0)),Some((96, 0))".to_string(),
                "rearm".to_string(),
                "set:Some((48, 0)),Some((50, 0)),Some((96, 512))".to_string(), // revert (masks intact)
                "rearm".to_string(), // restore the old consumer tap
            ],
            "restore the previous keymap/tap and do not persist after a failed re-arm"
        );

        // Failure path where the REVERT set_map ALSO fails: the function still
        // returns the re-arm error (never the revert error) and never persists.
        // The revert failure is logged rather than swallowed silently — the
        // keymap/registration desync would otherwise be invisible.
        let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
        let calls = std::cell::Cell::new(0u32);
        let l2 = std::rc::Rc::clone(&log);
        let l3 = std::rc::Rc::clone(&log);
        let revert_fails = apply_live_accept_keymap(
            Some((35, 0)),
            Some((38, 0)),
            Some((96, 0)),
            |w, f, _g| {
                // First call (the forward set) succeeds; the second (the
                // revert) fails.
                if calls.get() == 0 {
                    calls.set(1);
                    Ok(())
                } else {
                    Err(crate::shell::KeymapError::Collision(
                        w.or(f).map(|(k, _)| k).unwrap_or(0),
                    ))
                }
            },
            || {
                l2.borrow_mut().push("rearm".into());
                Err(PlatformError::Timeout)
            },
            |w, f, g| l3.borrow_mut().push(format!("persist:{w:?},{f:?},{g:?}")),
            || ((48, 0), (50, 0), None),
        );
        assert!(
            matches!(&revert_fails, Err(e) if e.starts_with("re-arm failed")),
            "the re-arm error is returned even when the revert also fails"
        );
        assert!(
            !log.borrow().iter().any(|l| l.starts_with("persist:")),
            "a failed re-arm never persists, even if the revert fails too"
        );

        // Partial rebind (word=None keeps the default): persist receives the
        // DEFAULTS-RESOLVED registered pair from effective(), not the raw
        // request args — pins the explicit-beats-absent persist choice.
        let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
        let l1 = std::rc::Rc::clone(&log);
        let l2 = std::rc::Rc::clone(&log);
        let l3 = std::rc::Rc::clone(&log);
        let partial = apply_live_accept_keymap(
            None,
            Some((38, 0)),
            None,
            |w, f, g| {
                l1.borrow_mut().push(format!("set:{w:?},{f:?},{g:?}"));
                Ok(())
            },
            || {
                l2.borrow_mut().push("rearm".into());
                Ok(())
            },
            |w, f, g| l3.borrow_mut().push(format!("persist:{w:?},{f:?},{g:?}")),
            || ((48, 0), (38, 0), None), // post-resolution: default word stays 48
        );
        assert!(partial.is_ok());
        assert_eq!(
            log.borrow().last().unwrap(),
            "persist:(48, 0),(38, 0),None",
            "persist writes the RESOLVED pair, not the raw request"
        );

        // Invalid map (collision) fails BEFORE any rearm/persist.
        let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
        let l2 = std::rc::Rc::clone(&log);
        let l3 = std::rc::Rc::clone(&log);
        let invalid = apply_live_accept_keymap(
            Some((53, 0)),
            None,
            None,
            |_, _, _| Err(crate::shell::KeymapError::Collision(53)),
            || {
                l2.borrow_mut().push("rearm".into());
                Ok(())
            },
            |w, f, g| l3.borrow_mut().push(format!("persist:{w:?},{f:?},{g:?}")),
            || ((48, 0), (50, 0), None),
        );
        assert!(invalid.is_err());
        assert!(
            log.borrow().is_empty(),
            "rejected map never rearms/persists"
        );
    }

    #[test]
    fn live_rebind_sets_and_persists_the_recorder_resolved_masks_verbatim() {
        // Slice 2: the recorder now supplies fully-resolved (keycode, mask)
        // pairs for BOTH roles, so apply_live_accept_keymap sets them as-is —
        // no mask reconstruction. word=Shift+48 (the unchanged role, carried
        // through by the recorder with its Shift mask intact — the audit-r2
        // preservation, now done upstream in recorder_outcome) and a freshly
        // captured bare full key (50) both reach set_map untouched, and persist
        // receives the same resolved pair (round-trips via format_accept_key).
        let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
        let l1 = std::rc::Rc::clone(&log);
        let l3 = std::rc::Rc::clone(&log);
        const SHIFT: u32 = 512; // Carbon shiftKey mask used by the macOS keymap facade.
                                // A stateful registered map so effective() reflects what set_map last
                                // wrote — this lets the PERSIST leg be asserted (persist reads the
                                // resolved registered pair via effective(), exactly as the real run
                                // loop does). Starts at the pre-rebind truth (word=Shift+48, full=60).
                                // Typed literals let inference name the type (no complex annotation).
        let registered = std::rc::Rc::new(std::cell::RefCell::new((
            (48_i64, SHIFT),
            (60_i64, 0_u32),
            Some((96_i64, SHIFT)),
        )));
        let r_set = std::rc::Rc::clone(&registered);
        let r_eff = std::rc::Rc::clone(&registered);
        let applied = apply_live_accept_keymap(
            Some((48, SHIFT)), // word: unchanged Shift+48, mask carried by the recorder
            Some((50, 0)),     // full: newly captured bare key
            Some((96, SHIFT)), // grammar accept: existing masked key preserved
            move |w, f, g| {
                // Mirror set_accept_keymap_from_config_with_mods: a None slot
                // default-fills (Tab/backtick); here both are explicit.
                *r_set.borrow_mut() = (w.unwrap_or((48, 0)), f.unwrap_or((50, 0)), g);
                l1.borrow_mut().push(format!("set:{w:?},{f:?},{g:?}"));
                Ok(())
            },
            || Ok(()),
            |w, f, g| l3.borrow_mut().push(format!("persist:{w:?},{f:?},{g:?}")),
            move || *r_eff.borrow(),
        );
        assert!(applied.is_ok());
        assert_eq!(
            log.borrow()[0],
            format!("set:Some((48, {SHIFT})),Some((50, 0)),Some((96, {SHIFT}))"),
            "the recorder-resolved masks reach set_map verbatim — Shift+48 kept, full bare"
        );
        assert_eq!(
            log.borrow().last().unwrap(),
            &format!("persist:(48, {SHIFT}),(50, 0),Some((96, {SHIFT}))"),
            "persist receives the resolved registered pair — the Shift mask survives to disk"
        );
    }

    #[test]
    fn live_rebind_failure_rearms_the_previous_keymap_after_revert() {
        let registered = std::rc::Rc::new(std::cell::RefCell::new((
            (48_i64, 512_u32),
            (50_i64, 0_u32),
            Some((96_i64, 512_u32)),
        )));
        let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
        let rearm_calls = std::rc::Rc::new(std::cell::Cell::new(0));

        let r_set = std::rc::Rc::clone(&registered);
        let l_set = std::rc::Rc::clone(&log);
        let l_rearm = std::rc::Rc::clone(&log);
        let calls = std::rc::Rc::clone(&rearm_calls);
        let r_eff = std::rc::Rc::clone(&registered);
        let l_persist = std::rc::Rc::clone(&log);
        let applied = apply_live_accept_keymap(
            Some((60, 0)),
            Some((61, 0)),
            Some((62, 0)),
            move |w, f, g| {
                *r_set.borrow_mut() = (w.unwrap_or((48, 0)), f.unwrap_or((50, 0)), g);
                l_set
                    .borrow_mut()
                    .push(format!("set:{:?}", *r_set.borrow()));
                Ok(())
            },
            move || {
                let call = calls.get() + 1;
                calls.set(call);
                l_rearm.borrow_mut().push(format!("rearm:{call}"));
                if call == 1 {
                    Err(PlatformError::Timeout)
                } else {
                    Ok(())
                }
            },
            move |w, f, g| {
                l_persist
                    .borrow_mut()
                    .push(format!("persist:{w:?},{f:?},{g:?}"))
            },
            move || *r_eff.borrow(),
        );

        assert!(applied.is_err());
        assert_eq!(
            log.borrow().as_slice(),
            [
                "set:((60, 0), (61, 0), Some((62, 0)))",
                "rearm:1",
                "set:((48, 512), (50, 0), Some((96, 512)))",
                "rearm:2",
            ],
            "failure restores the old map and re-arms against it without persisting"
        );
        assert_eq!(rearm_calls.get(), 2);
        assert_eq!(
            *registered.borrow(),
            ((48, 512), (50, 0), Some((96, 512))),
            "effective keymap reports the previous registered truth"
        );
    }

    #[test]
    fn grammar_accept_rebind_persists_compme_grammar_accept_key() {
        let persisted: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
        let sink = std::rc::Rc::clone(&persisted);
        let ok = apply_live_accept_keymap(
            Some((48, 0)),
            Some((50, 0)),
            Some((96, 512)),
            |_, _, _| Ok(()),
            || Ok(()),
            move |w, f, g| {
                for (key, value) in [
                    ("COMPME_ACCEPT_WORD_KEY", Some(w)),
                    ("COMPME_ACCEPT_FULL_KEY", Some(f)),
                    ("COMPME_GRAMMAR_ACCEPT_KEY", g),
                ] {
                    match value {
                        Some((code, mask)) => sink.borrow_mut().push(format!(
                            "{key}={}",
                            crate::shell::format_accept_key(code, mask)
                        )),
                        None => sink.borrow_mut().push(format!("remove:{key}")),
                    }
                }
            },
            || ((48, 0), (50, 0), Some((96, 512))),
        );
        assert!(ok.is_ok());
        assert!(persisted
            .borrow()
            .contains(&"COMPME_GRAMMAR_ACCEPT_KEY=shift+96".to_string()));
    }

    #[test]
    fn cached_domain_guards_on_the_app_it_was_read_under() {
        let cache = Some(("com.apple.Safari".to_string(), "docs.example".to_string()));
        // Same app → the cached host applies.
        assert_eq!(
            cached_domain(&cache, Some("com.apple.Safari")),
            Some("docs.example")
        );
        // The request resolved to a DIFFERENT app than the focus that
        // populated the cache → never cross-attribute a domain.
        assert_eq!(cached_domain(&cache, Some("com.google.Chrome")), None);
        assert_eq!(cached_domain(&cache, None), None);
        assert_eq!(cached_domain(&None, Some("com.apple.Safari")), None);
    }

    #[test]
    fn typing_domain_refreshes_browser_cache_when_a_domain_consumer_is_enabled() {
        let mut cache = Some((
            "com.apple.Safari".to_string(),
            "allowed.example".to_string(),
        ));
        assert_eq!(
            typing_domain(&mut cache, Some("com.apple.Safari"), false, None),
            Some("allowed.example".into())
        );

        assert_eq!(
            typing_domain(
                &mut cache,
                Some("com.apple.Safari"),
                true,
                Some("https://blocked.example/private")
            ),
            Some("blocked.example".into())
        );
        assert_eq!(
            typing_domain(&mut cache, Some("com.apple.Safari"), true, None),
            None
        );
    }

    #[test]
    fn domain_observation_enabled_fires_on_either_consumer() {
        // Domain reads are an OR of the two per-domain consumers: excluded-domain
        // rules and per-domain steering instructions. Pin each disjunct so neither
        // consumer silently stops requesting browser-domain detection.
        let empty_profile = PersonalizationProfile::default();
        let empty_prefs = Prefs::default();

        // Both empty → no consumer wants domains.
        assert!(!domain_observation_enabled(&empty_prefs, &empty_profile));

        // Only excluded_domains non-empty → enabled.
        let mut prefs_only = Prefs::default();
        prefs_only.excluded_domains.insert("bank.example".into());
        assert!(domain_observation_enabled(&prefs_only, &empty_profile));

        // Only per_domain steering non-empty → enabled.
        let mut profile_only = PersonalizationProfile::default();
        profile_only
            .per_domain
            .insert("docs.google.com".into(), "Match the doc tone.".into());
        assert!(domain_observation_enabled(&empty_prefs, &profile_only));
    }

    #[test]
    fn submit_gate_blocks_an_excluded_domain() {
        // The per-domain rules' submit-side consumer: with a domain present,
        // an excluded host blocks the request in an otherwise-allowed app.
        let mut prefs = Prefs::default();
        prefs.excluded_domains.insert("bank.example".into());
        assert!(!request_passes_submit_gates(
            &req_with_prompt("Dear team"),
            Some("com.apple.Safari"),
            Some("bank.example"),
            &prefs,
            0
        ));
        assert!(request_passes_submit_gates(
            &req_with_prompt("Dear team"),
            Some("com.apple.Safari"),
            Some("other.example"),
            &prefs,
            0
        ));
        // Browser domain rules configured but no fresh domain resolved:
        // fail closed on model submit so a missed URL read cannot bypass an
        // excluded-domain rule.
        assert!(!request_passes_submit_gates(
            &req_with_prompt("Dear team"),
            Some("com.apple.Safari"),
            None,
            &prefs,
            0
        ));
    }

    #[test]
    fn submit_gate_uses_grammar_left_context_not_empty_prompt() {
        let prefs = Prefs::default();
        let request = grammar_req_with_left_ctx("Dear team teh");
        assert_eq!(request.prompt, "");
        assert!(request_passes_submit_gates(
            &request,
            Some("com.apple.Terminal"),
            None,
            &prefs,
            0
        ));
    }

    #[test]
    fn submit_gate_blocks_a_subdomain_of_an_excluded_domain() {
        // Privacy-critical subdomain consumer: an excluded `bank.example` rule
        // must also block a request typed on the subdomain `login.bank.example`
        // (dot-boundary match), through both the model submit gate and the
        // local-replacement gate. A look-alike host on a non-dot boundary stays
        // allowed.
        let config = Config::from_lookup(lookup(&[("COMPME_EMOJI", "1")]));
        let mut prefs = Prefs::default();
        prefs.excluded_domains.insert("bank.example".into());
        let app = Some("com.apple.Safari");

        // Submit gate: subdomain blocked.
        assert!(!request_passes_submit_gates(
            &req_with_prompt("Dear team"),
            app,
            Some("login.bank.example"),
            &prefs,
            0
        ));
        // Submit gate: look-alike on a non-dot boundary is NOT blocked.
        assert!(request_passes_submit_gates(
            &req_with_prompt("Dear team"),
            app,
            Some("notbank.example"),
            &prefs,
            0
        ));

        // Replacement gate: the same subdomain rule blocks the local path too.
        assert!(replacement_decision(
            "hi :smile",
            &config,
            &prefs,
            app,
            Some("login.bank.example"),
            true,
            0
        )
        .is_none());
        assert!(replacement_decision(
            "hi :smile",
            &config,
            &prefs,
            app,
            Some("notbank.example"),
            true,
            0
        )
        .is_some());
    }

    #[test]
    fn web_override_persist_removes_emptied_keys_instead_of_blanking() {
        // Clearing the last entry in a category must REMOVE the key from
        // config.env — not leave the prior value stale (a naive skip) and not
        // write a blank `KEY=` (which occupies the env-over-file layer and
        // clutters the file). review-2026-06-13.
        let dir =
            std::env::temp_dir().join(format!("compme-weboverride-persist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.env");

        let mut prefs = Prefs::default();
        prefs.excluded_domains.insert("old.example.com".into());
        persist_web_override_prefs(&path, &prefs);
        let after_set = std::fs::read_to_string(&path).expect("read after set");
        assert!(
            after_set.contains("COMPME_EXCLUDED_DOMAINS=old.example.com"),
            "a populated category must persist its value: {after_set:?}"
        );

        // Clear every category and re-persist.
        persist_web_override_prefs(&path, &Prefs::default());
        let after_clear = std::fs::read_to_string(&path).expect("read after clear");
        assert!(
            !after_clear.contains("COMPME_EXCLUDED_DOMAINS"),
            "emptied key must be removed, not left stale or blanked: {after_clear:?}"
        );
        assert!(
            !after_clear.contains("COMPME_ENABLED_APPS="),
            "an empty category must never be written as a blank key: {after_clear:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replacement_decision_blocks_an_excluded_domain() {
        // The local-replacement path consumes the same domain gate.
        let config = Config::from_lookup(lookup(&[("COMPME_EMOJI", "1")]));
        let mut prefs = Prefs::default();
        prefs.excluded_domains.insert("bank.example".into());
        let app = Some("com.apple.Safari");
        assert!(replacement_decision(
            "hi :smile",
            &config,
            &prefs,
            app,
            Some("bank.example"),
            true,
            0
        )
        .is_none());
        assert!(replacement_decision(
            "hi :smile",
            &config,
            &prefs,
            app,
            Some("other.example"),
            true,
            0
        )
        .is_some());
    }

    #[test]
    fn submit_gate_combines_app_terminal_and_preference_policy() {
        let prefs = Prefs::default();
        assert!(request_passes_submit_gates(
            &req_with_prompt("Dear team"),
            Some("com.apple.TextEdit"),
            None,
            &prefs,
            0
        ));
        assert!(!request_passes_submit_gates(
            &req_with_prompt("Dear team"),
            Some("com.mitchellh.ghostty"),
            None,
            &prefs,
            0
        ));
        assert!(!request_passes_submit_gates(
            &req_with_prompt("Dear team"),
            Some("com.microsoft.VSCode"),
            None,
            &prefs,
            0
        ));
        assert!(!request_passes_submit_gates(
            &req_with_prompt("git status && ls -la"),
            Some("com.googlecode.iterm2"),
            None,
            &prefs,
            0
        ));
        assert!(request_passes_submit_gates(
            &req_with_prompt("please summarize the recent changes"),
            Some("com.googlecode.iterm2"),
            None,
            &prefs,
            0
        ));

        let excluded = build_prefs(&lookup(&[("COMPME_EXCLUDED_APPS", "com.apple.TextEdit")]));
        assert!(!request_passes_submit_gates(
            &req_with_prompt("Dear team"),
            Some("com.apple.TextEdit"),
            None,
            &excluded,
            0
        ));
    }

    #[test]
    fn submit_gate_blocks_while_snoozed_then_auto_resumes() {
        // Snooze must gate suggestions through the *integration* submit gate, not
        // only the standalone prefs unit — and auto-resume after the window
        // (A2 §16 pause/snooze).
        let mut prefs = Prefs::default();
        prefs.snooze(1_000, 5); // paused until t = 1_000 + 5*60_000 = 301_000 ms
        let req = req_with_prompt("Dear team");
        let app = Some("com.apple.TextEdit");
        // Blocked at the start of and midway through the window.
        assert!(!request_passes_submit_gates(&req, app, None, &prefs, 1_000));
        assert!(!request_passes_submit_gates(
            &req, app, None, &prefs, 61_000
        ));
        // Auto-resumes once the window elapses.
        assert!(request_passes_submit_gates(
            &req, app, None, &prefs, 301_001
        ));
    }

    #[test]
    fn submit_gate_uses_resolved_bundle_id_not_volatile_field_app() {
        let volatile = CompletionRequest {
            field: FieldHandle {
                app: "pid:42".into(),
                pid: Some(42),
                element_id: "f".into(),
                generation: 1,
            },
            ..req_with_prompt("Dear team")
        };

        let sidebar_key = resolve_app_key(volatile.field.pid, |pid| {
            (pid == 42).then(|| "com.microsoft.VSCode".to_string())
        });
        assert!(!request_passes_submit_gates(
            &volatile,
            sidebar_key.as_deref(),
            None,
            &Prefs::default(),
            0
        ));

        let textedit_key = resolve_app_key(volatile.field.pid, |pid| {
            (pid == 42).then(|| "com.apple.TextEdit".to_string())
        });
        let excluded = build_prefs(&lookup(&[("COMPME_EXCLUDED_APPS", "com.apple.TextEdit")]));
        assert!(!request_passes_submit_gates(
            &volatile,
            textedit_key.as_deref(),
            None,
            &excluded,
            0
        ));

        // Unresolved pid fails open and does not treat the volatile `pid:42`
        // field app as a preference key.
        let unresolved = resolve_app_key(volatile.field.pid, |_| None);
        assert!(request_passes_submit_gates(
            &volatile,
            unresolved.as_deref(),
            None,
            &build_prefs(&lookup(&[("COMPME_EXCLUDED_APPS", "pid:42")])),
            0
        ));
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
    fn context_max_chars_treats_falsy_words_and_blank_values_as_off() {
        // Explicit falsy words and an empty/whitespace-only value all mean off
        // (0); a plain number is taken verbatim; non-numeric junk falls back to
        // the default bound rather than disabling context.
        assert_eq!(parse_context_max_chars(Some("false".into())), 0);
        assert_eq!(parse_context_max_chars(Some("no".into())), 0);
        assert_eq!(parse_context_max_chars(Some("".into())), 0);
        assert_eq!(parse_context_max_chars(Some("   ".into())), 0);
        assert_eq!(parse_context_max_chars(Some("200".into())), 200);
        assert_eq!(
            parse_context_max_chars(Some("junk".into())),
            DEFAULT_CONTEXT_MAX_CHARS
        );
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
    fn resolve_app_key_returns_none_for_pid_above_i32_range() {
        // A u32 pid larger than i32::MAX can't be a real macOS pid; `i32::try_from`
        // fails so the resolver must never be called and gating fails open (None),
        // rather than panicking or wrapping to a negative pid.
        let resolver = |_pid: i32| -> Option<String> {
            panic!("resolver must not be called for an out-of-range pid");
        };
        let too_big = (i32::MAX as u32) + 1;
        assert_eq!(resolve_app_key(Some(too_big), resolver), None);
        assert_eq!(resolve_app_key(Some(u32::MAX), resolver), None);
    }

    #[test]
    fn effective_app_key_falls_back_to_canonical_field_app() {
        let stable = FieldHandle {
            app: "com.apple.TextEdit".into(),
            pid: Some(42),
            element_id: "f".into(),
            generation: 1,
        };
        assert_eq!(
            effective_app_key(&stable, |_| None),
            Some("com.apple.TextEdit".into()),
            "a transient pid lookup miss must keep the already-canonical app key"
        );

        let volatile = FieldHandle {
            app: "pid:42".into(),
            ..stable
        };
        assert_eq!(
            effective_app_key(&volatile, |_| None),
            None,
            "a volatile pid:N app still fails open when no resolver can identify it"
        );
    }

    #[test]
    fn effective_app_key_blocks_submit_with_canonical_fallback() {
        let field = FieldHandle {
            app: "com.apple.TextEdit".into(),
            pid: Some(42),
            element_id: "f".into(),
            generation: 1,
        };
        let request = CompletionRequest {
            field: field.clone(),
            ..req_with_prompt("Dear team")
        };
        let mut prefs = Prefs::default();
        prefs.excluded_apps.insert("com.apple.TextEdit".into());
        let app_key = effective_app_key(&field, |_| None);

        assert!(!request_passes_submit_gates(
            &request,
            app_key.as_deref(),
            None,
            &prefs,
            1_000
        ));
    }

    #[test]
    fn current_app_actions_use_canonical_fallback_when_pid_lookup_fails() {
        let field = FieldHandle {
            app: "com.apple.TextEdit".into(),
            pid: Some(42),
            element_id: "f".into(),
            generation: 1,
        };
        let app = effective_app_key(&field, |_| None).expect("canonical fallback");

        let mut prefs = Prefs::default();
        assert!(!toggle_app_collection(&mut prefs, &app));
        assert_eq!(no_collect_apps_value(&prefs), "com.apple.TextEdit");

        apply_app_disable(DisableArm::Always, &app, &mut prefs, 1_000);
        assert_eq!(excluded_apps_value(&prefs), "com.apple.TextEdit");
    }

    #[test]
    fn memory_storage_mode_defaults_off_and_parses_modes() {
        use memory::StorageMode;
        // Unset, falsy, and unknown all stay Off (opt-in §16 default).
        assert_eq!(parse_storage_mode(None), StorageMode::Off);
        assert_eq!(parse_storage_mode(Some("off".into())), StorageMode::Off);
        assert_eq!(
            parse_storage_mode(Some("nonsense".into())),
            StorageMode::Off
        );
        // Accepted-only synonyms.
        assert_eq!(
            parse_storage_mode(Some("accepted".into())),
            StorageMode::AcceptedOnly
        );
        assert_eq!(
            parse_storage_mode(Some("  TRUE ".into())),
            StorageMode::AcceptedOnly
        );
        // All-monitored synonyms.
        assert_eq!(
            parse_storage_mode(Some("all".into())),
            StorageMode::AllMonitored
        );
        assert_eq!(
            parse_storage_mode(Some("monitored".into())),
            StorageMode::AllMonitored
        );
    }

    #[test]
    fn hex_key_parses_64_chars_and_rejects_bad_input() {
        let hex = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        let key = parse_hex_key(hex).expect("valid 64-hex key");
        assert_eq!(key[0], 0x00);
        assert_eq!(key[1], 0x11);
        assert_eq!(key[31], 0xff);
        // Wrong length and non-hex digits fail closed (store stays disabled).
        assert!(parse_hex_key("deadbeef").is_none());
        assert!(parse_hex_key(&"z".repeat(64)).is_none());
    }

    #[test]
    fn memory_disabled_without_key_or_path_even_when_mode_set() {
        // Mode on but no key/path → no store (fail-closed, logged).
        let cfg = MemoryConfig {
            mode: memory::StorageMode::AcceptedOnly,
            path: None,
            key: None,
        };
        assert!(open_memory_store(&cfg, || None).is_none());
        // Off mode is always disabled regardless of key/path.
        let cfg_off = MemoryConfig {
            mode: memory::StorageMode::Off,
            path: Some(PathBuf::from("/tmp/should-not-open.db")),
            key: Some([7u8; 32]),
        };
        assert!(open_memory_store(&cfg_off, || None).is_none());
    }

    #[test]
    fn memory_opens_with_the_keychain_fallback_key_when_env_key_is_missing() {
        let path = std::env::temp_dir().join(format!(
            "compme-keychain-fallback-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let cfg = MemoryConfig {
            mode: memory::StorageMode::AcceptedOnly,
            path: Some(path.clone()),
            key: None,
        };

        let store = open_memory_store(&cfg, || Some([7u8; 32]));
        assert!(
            store.is_some(),
            "a keychain-provided key must open the store when the env key is absent"
        );
        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_explicit_env_key_takes_precedence_over_the_keychain() {
        // The keychain must not even be consulted: an explicit
        // COMPME_MEMORY_KEY is the operator's override (and the
        // fail-closed path when the keychain is unavailable).
        let path = std::env::temp_dir().join(format!(
            "compme-env-key-precedence-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let cfg = MemoryConfig {
            mode: memory::StorageMode::AcceptedOnly,
            path: Some(path.clone()),
            key: Some([7u8; 32]),
        };

        let store = open_memory_store(&cfg, || panic!("keychain consulted despite env key"));
        assert!(store.is_some());
        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn configured_all_monitored_store_persists_redacted_inserted_deltas_only() {
        let path = std::env::temp_dir().join(format!(
            "compme-all-monitored-configured-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let path_str = path.to_string_lossy().into_owned();
        let key = "1111111111111111111111111111111111111111111111111111111111111111";
        let cfg = build_memory_config(&|name| match name {
            "COMPME_MEMORY" => Some("all".into()),
            "COMPME_MEMORY_PATH" => Some(path_str.clone()),
            "COMPME_MEMORY_KEY" => Some(key.into()),
            _ => None,
        });
        let store = open_memory_store(&cfg, || panic!("keychain consulted despite env key"))
            .expect("configured all-monitored store opens");
        let field = field_with_app("com.apple.TextEdit");
        let change = typed_change_after_baseline(
            &field,
            "pre-existing alice@example.com",
            "pre-existing alice@example.com typed bob@example.com ",
        );
        let prefs = Prefs::default();
        queue_and_flush_monitored(&change, &store, &prefs, true, false);
        assert_eq!(
            store.recent("com.apple.TextEdit", 10).unwrap(),
            vec![" typed [redacted-email] "]
        );
        drop(store);

        let reopened = open_memory_store(&cfg, || panic!("keychain consulted despite env key"))
            .expect("configured all-monitored store reopens");
        assert_eq!(
            reopened.recent("com.apple.TextEdit", 10).unwrap(),
            vec![" typed [redacted-email] "]
        );
        drop(reopened);
        let raw = std::fs::read(&path).expect("memory db is readable");
        for needle in [
            b"bob@example.com".as_slice(),
            b"[redacted-email]".as_slice(),
        ] {
            assert!(
                !raw.windows(needle.len()).any(|window| window == needle),
                "monitored text must be encrypted on disk, including redacted form"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_keychain_is_not_consulted_when_memory_is_off_or_path_is_missing() {
        let cfg_off = MemoryConfig {
            mode: memory::StorageMode::Off,
            path: Some(PathBuf::from("/tmp/should-not-open.db")),
            key: None,
        };
        assert!(open_memory_store(&cfg_off, || panic!("keychain consulted while Off")).is_none());
        // No path → no store to encrypt; creating a keychain key would be a
        // side effect with no purpose.
        let cfg_no_path = MemoryConfig {
            mode: memory::StorageMode::AcceptedOnly,
            path: None,
            key: None,
        };
        assert!(
            open_memory_store(&cfg_no_path, || panic!("keychain consulted without a path"))
                .is_none()
        );
    }

    #[test]
    fn latency_sample_computes_elapsed_and_prunes_older_generations() {
        let mut submit = HashMap::new();
        submit.insert(1u64, 100u64);
        submit.insert(2u64, 150u64);
        submit.insert(3u64, 220u64);
        // Outcome for gen 2 at t=210 → latency 60ms; gen 1 (older) is pruned, gen
        // 3 (newer, still pending) is kept.
        assert_eq!(latency_sample(&mut submit, 2, 210), Some(60));
        assert!(!submit.contains_key(&1));
        assert!(!submit.contains_key(&2));
        assert!(submit.contains_key(&3));
    }

    #[test]
    fn latency_sample_is_none_for_an_untracked_generation() {
        let mut submit = HashMap::new();
        submit.insert(5u64, 100u64);
        // A coalesced-away or already-pruned generation has no submit time.
        assert_eq!(latency_sample(&mut submit, 9, 200), None);
        // The unrelated entry is untouched.
        assert!(submit.contains_key(&5));
        // Degenerate form of the same path: a fully empty map.
        let mut empty: HashMap<u64, u64> = HashMap::new();
        assert_eq!(latency_sample(&mut empty, 1, 100), None);
        assert!(empty.is_empty());
    }

    #[test]
    fn latency_sample_saturates_rather_than_overflowing() {
        let mut submit = HashMap::new();
        submit.insert(1u64, 0u64);
        // An implausibly large elapsed value clamps to u32::MAX, never panics.
        assert_eq!(latency_sample(&mut submit, 1, u64::MAX), Some(u32::MAX));
    }

    #[test]
    fn latency_sample_prunes_all_lower_generations_in_one_call() {
        let mut submit = HashMap::new();
        for (gen, at) in [(1u64, 100u64), (2, 110), (3, 120)] {
            submit.insert(gen, at);
        }
        // Outcome for the newest pending gen prunes every entry at or below it.
        assert_eq!(latency_sample(&mut submit, 3, 200), Some(80));
        assert!(submit.is_empty());
    }

    #[test]
    fn latency_sample_returns_zero_when_outcome_same_ms_as_submit() {
        // A completion returned within the same heartbeat reads as 0 ms — the
        // true measured value at run-loop resolution, not None.
        let mut submit = HashMap::new();
        submit.insert(1u64, 100u64);
        assert_eq!(latency_sample(&mut submit, 1, 100), Some(0));
    }

    #[test]
    fn latency_sample_supports_repeated_calls_against_a_shared_map() {
        // The runtime pattern: one persistent map, sampled per outcome.
        let mut submit = HashMap::new();
        submit.insert(2u64, 100u64);
        submit.insert(3u64, 130u64);
        assert_eq!(latency_sample(&mut submit, 2, 150), Some(50));
        assert_eq!(latency_sample(&mut submit, 3, 200), Some(70));
        assert!(submit.is_empty());
    }

    fn request_for_submit_tracking(generation: u64) -> CompletionRequest {
        CompletionRequest {
            generation,
            field: FieldHandle {
                app: "com.apple.TextEdit".into(),
                pid: Some(7),
                element_id: "ax:field".into(),
                generation: 1,
            },
            domain: None,
            snapshot: generation,
            prompt: "hello world".into(),
            max_tokens: 24,
            kind: RequestKind::Completion,
        }
    }

    fn request_log_context_for_submit_tracking() -> RequestLogContext {
        RequestLogContext {
            app_key: Some("com.apple.TextEdit".into()),
            domain: None,
            prefs: Prefs::default(),
            acceptance_prompt_marker: Some("hello".into()),
        }
    }

    #[test]
    fn submit_tracking_records_only_accepted_requests() {
        let mut submit_times = HashMap::new();
        let mut log_context = request_log_context_for_submit_tracking();
        log_context.domain = Some("docs.google.com".into());

        let log_line = submit_request_and_track(
            &mut submit_times,
            request_for_submit_tracking(7),
            123,
            log_context,
            |request| {
                assert_eq!(request.generation, 7);
                assert_eq!(request.prompt, "hello world");
                assert_eq!(request.domain.as_deref(), Some("docs.google.com"));
                true
            },
        );

        assert!(log_line.contains("request gen=7"));
        assert!(log_line.contains("app=com.apple.TextEdit"));
        assert!(!log_line.contains("inference submit failed"));
        assert_eq!(submit_times.get(&7), Some(&123));
    }

    #[test]
    fn submit_tracking_does_not_record_rejected_requests() {
        let mut submit_times = HashMap::new();

        let mut submitted_generation = None;
        let log_line = submit_request_and_track(
            &mut submit_times,
            request_for_submit_tracking(7),
            123,
            request_log_context_for_submit_tracking(),
            |request| {
                submitted_generation = Some(request.generation);
                false
            },
        );

        assert_eq!(log_line, "compme: inference submit failed gen=7");
        assert!(!log_line.contains("request gen="));
        assert_eq!(submitted_generation, Some(7));
        assert!(!submit_times.contains_key(&7));
        assert_eq!(
            latency_sample(&mut submit_times, 7, 200),
            None,
            "rejected worker submissions must not create phantom latency samples"
        );
    }

    #[test]
    fn submit_tracking_does_not_overwrite_a_domain_already_on_the_request() {
        // `submit_request_and_track` only fills `request.domain` from
        // `log_context.domain` when the request's own domain is None. A domain
        // already resolved onto the request (e.g. the active tab's host) must win
        // over the log context's — never get clobbered.
        let mut submit_times = HashMap::new();
        let request = CompletionRequest {
            domain: Some("a.com".into()),
            ..request_for_submit_tracking(7)
        };
        let mut log_context = request_log_context_for_submit_tracking();
        log_context.domain = Some("b.com".into());

        submit_request_and_track(&mut submit_times, request, 123, log_context, |request| {
            assert_eq!(
                request.domain.as_deref(),
                Some("a.com"),
                "the request's pre-existing domain must be preserved"
            );
            true
        });
    }

    #[test]
    fn auxiliary_context_is_prepared_before_submitting_the_request() {
        let clipboard_cell = Arc::new(Mutex::new(None));
        let order = RefCell::new(Vec::new());
        let mut submit_times = HashMap::new();
        let mut log_context = request_log_context_for_submit_tracking();
        log_context.domain = Some("docs.google.com".into());
        let request = CompletionRequest {
            generation: 17,
            snapshot: 42,
            ..request_for_submit_tracking(17)
        };

        let (clipboard_diag, submit_line) = submit_request_with_auxiliary_context(
            request,
            SubmitRequestContext {
                submit_times: &mut submit_times,
                now_ms: 321,
                log_context,
            },
            AuxiliarySubmitContext {
                clipboard_enabled: true,
                diag_context: true,
                diag_clipboard_marker: Some("copied marker"),
                clipboard_cell: &clipboard_cell,
                screen_enabled: true,
            },
            || {
                order.borrow_mut().push("clipboard");
                Some("copied marker".into())
            },
            |_| {
                order.borrow_mut().push("caret");
                rect(9.0)
            },
            |submission| {
                order.borrow_mut().push("screen");
                assert_eq!(submission.generation, 17);
                assert_eq!(submission.snapshot, 42);
                assert_eq!(submission.caret_rect.unwrap().x, 9.0);
            },
            |request| {
                order.borrow_mut().push("submit");
                assert_eq!(request.generation, 17);
                assert_eq!(request.domain.as_deref(), Some("docs.google.com"));
                true
            },
        );

        assert_eq!(
            *order.borrow(),
            vec!["clipboard", "caret", "screen", "submit"]
        );
        assert_eq!(
            *clipboard_cell.lock().unwrap(),
            Some("copied marker".to_string())
        );
        assert_eq!(
            clipboard_diag.as_deref(),
            Some("Some(chars=13 marker=true)")
        );
        assert!(submit_line.contains("request gen=17"));
        assert_eq!(submit_times.get(&17), Some(&321));
    }

    #[test]
    fn auxiliary_context_off_clears_stale_clipboard_and_skips_screen_submission() {
        let clipboard_cell = Arc::new(Mutex::new(Some("stale clipboard".into())));
        let mut submit_times = HashMap::new();

        let (clipboard_diag, submit_line) = submit_request_with_auxiliary_context(
            request_for_submit_tracking(18),
            SubmitRequestContext {
                submit_times: &mut submit_times,
                now_ms: 444,
                log_context: request_log_context_for_submit_tracking(),
            },
            AuxiliarySubmitContext {
                clipboard_enabled: false,
                diag_context: true,
                diag_clipboard_marker: Some("marker"),
                clipboard_cell: &clipboard_cell,
                screen_enabled: false,
            },
            || panic!("clipboard must not be read when context is disabled"),
            |_| panic!("caret must not be read when screen context is disabled"),
            |_| panic!("screen OCR must not be submitted when disabled"),
            |request| {
                assert_eq!(request.generation, 18);
                true
            },
        );

        assert_eq!(clipboard_diag, None);
        assert_eq!(*clipboard_cell.lock().unwrap(), None);
        assert!(submit_line.contains("request gen=18"));
        assert_eq!(submit_times.get(&18), Some(&444));
    }

    #[test]
    fn clipboard_enabled_but_empty_clears_the_stale_cell() {
        // Clipboard context is ENABLED but the OS read returns None (empty
        // clipboard). The cell must be CLEARED to None — never left holding a
        // prior value, which would leak a stale secret into the next prompt. The
        // `*_off_*` test above covers the disabled branch; this pins the
        // enabled-but-empty path.
        let clipboard_cell = Arc::new(Mutex::new(Some("old secret".into())));
        let mut submit_times = HashMap::new();

        let (clipboard_diag, submit_line) = submit_request_with_auxiliary_context(
            request_for_submit_tracking(19),
            SubmitRequestContext {
                submit_times: &mut submit_times,
                now_ms: 555,
                log_context: request_log_context_for_submit_tracking(),
            },
            AuxiliarySubmitContext {
                clipboard_enabled: true,
                diag_context: false,
                diag_clipboard_marker: None,
                clipboard_cell: &clipboard_cell,
                screen_enabled: false,
            },
            || None,
            |_| panic!("screen disabled"),
            |_| panic!("screen disabled"),
            |request| {
                assert_eq!(request.generation, 19);
                true
            },
        );

        assert_eq!(clipboard_diag, None);
        assert_eq!(
            *clipboard_cell.lock().unwrap(),
            None,
            "an empty clipboard read must clear the stale cell"
        );
        assert!(submit_line.contains("request gen=19"));
        assert_eq!(submit_times.get(&19), Some(&555));
    }

    #[test]
    fn submit_path_redacts_clipboard_before_storing_in_cell() {
        // The submit path (submit_request_with_auxiliary_context) is the ONLY
        // place clipboard text is redacted before it lands in the cell the
        // inference worker reads into the model prompt. The clipboard routinely
        // holds passwords/cards/emails, so a regression dropping redaction::redact
        // here would silently leak raw secrets into the prompt. Pin it: a
        // secret-bearing clipboard must be stored already redacted.
        let clipboard_cell = Arc::new(Mutex::new(None));
        let mut submit_times = HashMap::new();
        let raw_secret = "sk-abcdEFGH0123456789abcdEFGH0123";

        submit_request_with_auxiliary_context(
            request_for_submit_tracking(21),
            SubmitRequestContext {
                submit_times: &mut submit_times,
                now_ms: 500,
                log_context: request_log_context_for_submit_tracking(),
            },
            AuxiliarySubmitContext {
                clipboard_enabled: true,
                diag_context: false,
                diag_clipboard_marker: None,
                clipboard_cell: &clipboard_cell,
                screen_enabled: false,
            },
            || Some(format!("paste {raw_secret} now")),
            |_| panic!("screen disabled"),
            |_| panic!("screen disabled"),
            |_| true,
        );

        let stored = clipboard_cell.lock().unwrap().clone().expect("cell set");
        assert!(
            stored.contains("[redacted-secret]"),
            "clipboard stored redacted: {stored:?}"
        );
        assert!(
            !stored.contains(raw_secret),
            "raw secret must not reach the prompt cell: {stored:?}"
        );
    }

    #[test]
    fn screen_ocr_submission_preserves_request_stamp_and_caret_rect() {
        let request = CompletionRequest {
            generation: 17,
            snapshot: 42,
            ..request_for_submit_tracking(17)
        };
        let submission = ScreenOcrSubmission::from_request(&request, rect(9.0));

        assert_eq!(submission.field, request.field);
        assert_eq!(submission.generation, 17);
        assert_eq!(submission.snapshot, 42);
        assert_eq!(submission.caret_rect.unwrap().x, 9.0);
    }

    fn field_with_app(app: &str) -> FieldHandle {
        FieldHandle {
            app: app.into(),
            pid: Some(7),
            element_id: "ax:field".into(),
            generation: 1,
        }
    }

    #[test]
    fn buffered_monitored_text_drops_orphaned_prior_generation_buffer() {
        // A field's generation bumps (element replaced) without a Focus event
        // clearing the map. The old-generation Collecting buffer must not linger:
        // the fresh handle prunes its same-logical-field stale sibling so
        // monitored_buffers can't accumulate dead keys within one session.
        let mut buffers: HashMap<FieldHandle, MonitoredBuffer> = HashMap::new();
        let gen1 = field_with_app("com.apple.TextEdit");
        let mut gen2 = field_with_app("com.apple.TextEdit");
        gen2.generation = 2; // same app/pid/element_id, replaced element

        // Mid-word collection on gen1 leaves a Collecting buffer (no boundary).
        assert_eq!(buffered_monitored_text(&mut buffers, &gen1, "ab"), None);
        assert_eq!(buffers.len(), 1);

        // First keystroke on gen2 evicts the orphaned gen1 buffer.
        assert_eq!(buffered_monitored_text(&mut buffers, &gen2, "cd"), None);
        assert_eq!(buffers.len(), 1, "stale gen1 buffer pruned: {buffers:?}");
        assert!(buffers.contains_key(&gen2));
        assert!(!buffers.contains_key(&gen1));

        // An UNRELATED field is left untouched by the prune.
        let other = field_with_app("com.apple.Notes");
        assert_eq!(buffered_monitored_text(&mut buffers, &other, "ef"), None);
        assert_eq!(buffers.len(), 2);
        assert!(buffers.contains_key(&gen2));
        assert!(buffers.contains_key(&other));
    }

    fn text_context(field: &FieldHandle, left: &str) -> platform::TextContext {
        platform::TextContext {
            left: left.into(),
            right: String::new(),
            left_scalars: left.chars().count(),
            selection: None,
            caret: left.chars().count(),
            source: platform::ContextSource::Accessibility,
            field_id: field.clone(),
            offset_encoding: platform::OffsetEncoding::UnicodeScalars,
        }
    }

    fn text_context_with_right(
        field: &FieldHandle,
        left: &str,
        right: &str,
    ) -> platform::TextContext {
        platform::TextContext {
            left: left.into(),
            right: right.into(),
            left_scalars: left.chars().count(),
            selection: None,
            caret: left.chars().count(),
            source: platform::ContextSource::Accessibility,
            field_id: field.clone(),
            offset_encoding: platform::OffsetEncoding::Utf16CodeUnits,
        }
    }

    fn writable_axset_caps() -> Capabilities {
        Capabilities {
            readable_text: true,
            readable_caret: true,
            writable: true,
            secure: false,
            security_state: SecurityState::Normal,
            toolkit: Toolkit::AppKit,
            multiline: true,
            insert_strategy: InsertStrategy::AxSet,
            accept_intercept: KeyInterceptMode::CgEventTap,
            overlay_at_caret: OverlayPlacement::NativePanel,
            coords_global_screen: true,
        }
    }

    fn grammar_gate<'a>(
        config: &'a Config,
        prefs: &'a Prefs,
        app_key: Option<&'a str>,
        domain: Option<&'a str>,
        enabled: bool,
        caps: &'a Capabilities,
        now_ms: u64,
    ) -> GrammarRequestGate<'a> {
        GrammarRequestGate {
            config,
            prefs,
            app_key,
            domain,
            enabled,
            caps,
            now_ms,
        }
    }

    #[test]
    fn grammar_trigger_dispatches_word_at_caret_scalar_range() {
        let field = host_field("grammar");
        let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
        let request = grammar_fix_request(
            &field,
            &text_context_with_right(&field, "😀 teh", ""),
            grammar_gate(
                &config,
                &config.prefs,
                Some("TextEdit"),
                None,
                true,
                &writable_axset_caps(),
                0,
            ),
        )
        .expect("request");

        assert_eq!(request.generation, field.generation);
        assert_eq!(request.prompt, "");
        match request.kind {
            RequestKind::GrammarFix {
                word,
                left_ctx,
                correction_range,
            } => {
                assert_eq!(word, "teh");
                assert_eq!(left_ctx, "😀 teh");
                assert_eq!(correction_range, CorrectionRange { start: 2, end: 5 });
            }
            RequestKind::Completion => panic!("expected grammar request"),
        }
    }

    #[test]
    fn grammar_request_bounds_left_context_to_a_caret_adjacent_tail() {
        // The AX field value is unbounded input; the prompt context must be a
        // bounded tail while correction_range stays in full-field coordinates.
        let field = host_field("grammar-long");
        let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
        let long_left = format!("{} teh", "word ".repeat(400).trim_end());
        let request = grammar_fix_request(
            &field,
            &text_context_with_right(&field, &long_left, ""),
            grammar_gate(
                &config,
                &config.prefs,
                Some("TextEdit"),
                None,
                true,
                &writable_axset_caps(),
                0,
            ),
        )
        .expect("request");

        match request.kind {
            RequestKind::GrammarFix {
                word,
                left_ctx,
                correction_range,
            } => {
                assert_eq!(word, "teh");
                assert!(
                    left_ctx.chars().count() <= GRAMMAR_LEFT_CTX_CHARS,
                    "left_ctx not bounded: {} chars",
                    left_ctx.chars().count()
                );
                assert!(left_ctx.ends_with("teh"), "tail must stay caret-adjacent");
                // Range still addresses the full field value, not the tail.
                let start = long_left.chars().count() - 3;
                assert_eq!(
                    correction_range,
                    CorrectionRange {
                        start,
                        end: start + 3
                    }
                );
            }
            RequestKind::Completion => panic!("expected grammar request"),
        }
    }

    #[test]
    fn grammar_trigger_dispatches_midword_whole_word_range() {
        let field = host_field("grammar-mid");
        let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
        let request = grammar_fix_request(
            &field,
            &text_context_with_right(&field, "te", "h later"),
            grammar_gate(
                &config,
                &config.prefs,
                Some("TextEdit"),
                None,
                true,
                &writable_axset_caps(),
                0,
            ),
        )
        .expect("request");

        match request.kind {
            RequestKind::GrammarFix {
                word,
                correction_range,
                ..
            } => {
                assert_eq!(word, "teh");
                assert_eq!(correction_range, CorrectionRange { start: 0, end: 3 });
            }
            RequestKind::Completion => panic!("expected grammar request"),
        }
    }

    #[test]
    fn grammar_trigger_rejects_an_overlong_word_before_inference() {
        let field = host_field("grammar-overlong");
        let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
        let long_word = "x".repeat(GRAMMAR_WORD_MAX_CHARS + 1);

        assert!(
            grammar_fix_request(
                &field,
                &text_context_with_right(&field, &long_word, ""),
                grammar_gate(
                    &config,
                    &config.prefs,
                    Some("TextEdit"),
                    None,
                    true,
                    &writable_axset_caps(),
                    0,
                ),
            )
            .is_none(),
            "unbounded AX words must not become grammar-model prompts"
        );
    }

    #[test]
    fn grammar_detection_blocks_without_fresh_browser_domain_when_domain_rules_exist() {
        let field = field_with_app("com.google.Chrome");
        let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
        let mut prefs = config.prefs.clone();
        prefs.excluded_domains.insert("docs.example.com".into());

        assert!(grammar_fix_request(
            &field,
            &text_context_with_right(&field, "teh", ""),
            grammar_gate(
                &config,
                &prefs,
                Some("com.google.Chrome"),
                None,
                true,
                &writable_axset_caps(),
                0,
            ),
        )
        .is_none());
        assert!(grammar_fix_request(
            &field,
            &text_context_with_right(&field, "teh", ""),
            grammar_gate(
                &config,
                &prefs,
                Some("com.google.Chrome"),
                Some("docs.example.com"),
                true,
                &writable_axset_caps(),
                0,
            ),
        )
        .is_none());
        assert!(grammar_fix_request(
            &field,
            &text_context_with_right(&field, "teh", ""),
            grammar_gate(
                &config,
                &prefs,
                Some("com.google.Chrome"),
                Some("public.example.com"),
                true,
                &writable_axset_caps(),
                0,
            ),
        )
        .is_some());
    }

    #[test]
    fn grammar_detection_refresh_drops_stale_allowed_browser_domain() {
        let field = field_with_app("com.google.Chrome");
        let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
        let mut prefs = config.prefs.clone();
        prefs.excluded_domains.insert("blocked.example".into());
        let mut cache = Some((
            "com.google.Chrome".to_string(),
            "allowed.example".to_string(),
        ));

        let refreshed_domain = typing_domain(&mut cache, Some("com.google.Chrome"), true, None);

        assert_eq!(refreshed_domain, None);
        assert!(grammar_fix_request(
            &field,
            &text_context_with_right(&field, "teh", ""),
            grammar_gate(
                &config,
                &prefs,
                Some("com.google.Chrome"),
                refreshed_domain.as_deref(),
                true,
                &writable_axset_caps(),
                0,
            ),
        )
        .is_none());
    }

    #[test]
    fn grammar_detection_refresh_reads_current_browser_url_before_gating() {
        let field = field_with_app("com.google.Chrome");
        let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
        let mut prefs = config.prefs.clone();
        prefs.excluded_domains.insert("blocked.example".into());
        let calls = std::cell::Cell::new(0);
        let mut cache = Some((
            "com.google.Chrome".to_string(),
            "allowed.example".to_string(),
        ));

        let refreshed_domain =
            typing_domain_for_current_field(&mut cache, Some("com.google.Chrome"), true, || {
                calls.set(calls.get() + 1);
                Some("https://blocked.example/doc".to_string())
            });

        assert_eq!(calls.get(), 1);
        assert_eq!(refreshed_domain.as_deref(), Some("blocked.example"));
        assert!(grammar_fix_request(
            &field,
            &text_context_with_right(&field, "teh", ""),
            grammar_gate(
                &config,
                &prefs,
                Some("com.google.Chrome"),
                refreshed_domain.as_deref(),
                true,
                &writable_axset_caps(),
                0,
            ),
        )
        .is_none());
    }

    #[test]
    fn grammar_detection_refresh_allows_current_allowed_browser_url() {
        let field = field_with_app("com.google.Chrome");
        let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
        let mut prefs = config.prefs.clone();
        prefs.excluded_domains.insert("blocked.example".into());
        let calls = std::cell::Cell::new(0);
        let mut cache = Some((
            "com.google.Chrome".to_string(),
            "blocked.example".to_string(),
        ));

        let refreshed_domain =
            typing_domain_for_current_field(&mut cache, Some("com.google.Chrome"), true, || {
                calls.set(calls.get() + 1);
                Some("https://allowed.example/doc".to_string())
            });

        assert_eq!(calls.get(), 1);
        assert_eq!(refreshed_domain.as_deref(), Some("allowed.example"));
        assert!(grammar_fix_request(
            &field,
            &text_context_with_right(&field, "teh", ""),
            grammar_gate(
                &config,
                &prefs,
                Some("com.google.Chrome"),
                refreshed_domain.as_deref(),
                true,
                &writable_axset_caps(),
                0,
            ),
        )
        .is_some());
    }

    #[test]
    fn manual_grammar_request_uses_fresh_browser_url_before_gating() {
        let field = field_with_app("com.google.Chrome");
        let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
        let mut prefs = config.prefs.clone();
        prefs.excluded_domains.insert("blocked.example".into());
        let calls = std::cell::Cell::new(0);
        let mut cache = Some((
            "com.google.Chrome".to_string(),
            "allowed.example".to_string(),
        ));

        let request = manual_grammar_request_for_current_field(
            ManualGrammarRequestInputs {
                field: &field,
                ctx: &text_context_with_right(&field, "teh", ""),
                caps: &writable_axset_caps(),
                config: &config,
                prefs: &prefs,
                app_key: Some("com.google.Chrome"),
                enabled: true,
                now_ms: 0,
            },
            &mut cache,
            || {
                calls.set(calls.get() + 1);
                Some("https://blocked.example/doc".to_string())
            },
        );

        assert_eq!(calls.get(), 1);
        assert!(
            request.is_none(),
            "manual grammar shortcut must not arm from stale allowed cache after the current URL is excluded"
        );
    }

    #[test]
    fn grammar_detection_respects_enable_per_app_snooze_and_axset() {
        let field = host_field("grammar-gates");
        let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
        let ctx = text_context_with_right(&field, "teh", "");
        assert!(grammar_fix_request(
            &field,
            &ctx,
            grammar_gate(
                &config,
                &config.prefs,
                Some("TextEdit"),
                None,
                true,
                &writable_axset_caps(),
                0,
            ),
        )
        .is_some());

        assert!(grammar_fix_request(
            &field,
            &ctx,
            grammar_gate(
                &config,
                &config.prefs,
                Some("TextEdit"),
                None,
                false,
                &writable_axset_caps(),
                0,
            ),
        )
        .is_none());

        let mut prefs = config.prefs.clone();
        prefs.set_app_policy_field("TextEdit", prefs::AppPolicyField::GrammarFix, false);
        assert!(grammar_fix_request(
            &field,
            &ctx,
            grammar_gate(
                &config,
                &prefs,
                Some("TextEdit"),
                None,
                true,
                &writable_axset_caps(),
                0,
            ),
        )
        .is_none());

        let mut prefs = config.prefs.clone();
        prefs.snooze_app("TextEdit", 0, 60);
        assert!(grammar_fix_request(
            &field,
            &ctx,
            grammar_gate(
                &config,
                &prefs,
                Some("TextEdit"),
                None,
                true,
                &writable_axset_caps(),
                1,
            ),
        )
        .is_none());

        let mut caps = writable_axset_caps();
        caps.insert_strategy = InsertStrategy::SyntheticKeys;
        assert!(grammar_fix_request(
            &field,
            &ctx,
            grammar_gate(
                &config,
                &config.prefs,
                Some("TextEdit"),
                None,
                true,
                &caps,
                0,
            ),
        )
        .is_none());
    }

    #[test]
    fn grammar_detection_allows_per_app_on_override_when_global_default_is_off() {
        let field = host_field("grammar-app-override");
        let config = Config::from_lookup(lookup(&[]));
        let ctx = text_context_with_right(&field, "teh", "");

        assert!(
            grammar_fix_request(
                &field,
                &ctx,
                grammar_gate(
                    &config,
                    &config.prefs,
                    Some("TextEdit"),
                    None,
                    true,
                    &writable_axset_caps(),
                    0,
                ),
            )
            .is_none(),
            "global grammar off with no app override must block"
        );

        let mut prefs = config.prefs.clone();
        prefs.set_app_policy_field("TextEdit", prefs::AppPolicyField::GrammarFix, true);

        assert!(
            grammar_fix_request(
                &field,
                &ctx,
                grammar_gate(
                    &config,
                    &prefs,
                    Some("TextEdit"),
                    None,
                    true,
                    &writable_axset_caps(),
                    0,
                ),
            )
            .is_some(),
            "Apps-pane grammar override must enable the focused app even when the global default is off"
        );
    }

    #[test]
    fn grammar_pre_read_policy_blocks_disabled_paths_before_ax_text() {
        let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
        let app = Some("TextEdit");
        let mut cache = None;
        assert!(grammar_pre_read_policy_passes(
            &config,
            &config.prefs,
            app,
            true,
            0,
            &mut cache,
            || None,
        ));

        let mut cache = None;
        assert!(!grammar_pre_read_policy_passes(
            &config,
            &config.prefs,
            app,
            false,
            0,
            &mut cache,
            || None,
        ));

        let mut prefs = config.prefs.clone();
        prefs.set_app_policy_field("TextEdit", prefs::AppPolicyField::GrammarFix, false);
        let mut cache = None;
        assert!(!grammar_pre_read_policy_passes(
            &config,
            &prefs,
            app,
            true,
            0,
            &mut cache,
            || None,
        ));

        let mut prefs = config.prefs.clone();
        prefs.snooze_app("TextEdit", 0, 60);
        let mut cache = None;
        assert!(!grammar_pre_read_policy_passes(
            &config,
            &prefs,
            app,
            true,
            1,
            &mut cache,
            || None,
        ));

        let mut prefs = config.prefs.clone();
        prefs.excluded_domains.insert("blocked.example".into());
        let url_reads = std::cell::Cell::new(0);
        let mut cache = Some((
            "com.google.Chrome".to_string(),
            "allowed.example".to_string(),
        ));
        assert!(!grammar_pre_read_policy_passes(
            &config,
            &prefs,
            Some("com.google.Chrome"),
            true,
            0,
            &mut cache,
            || {
                url_reads.set(url_reads.get() + 1);
                Some("https://blocked.example/doc".to_string())
            },
        ));
        assert_eq!(url_reads.get(), 1);
    }

    fn grammar_shortcut_probe(
        config: &Config,
        prefs: &Prefs,
        enabled: bool,
        app: &str,
        now_ms: u64,
        cached_domain_entry: Option<(String, String)>,
        focused_url: Option<&str>,
    ) -> (GrammarCheckShortcutOutcome, usize, usize, usize) {
        let field = field_with_app(app);
        let mut cache = cached_domain_entry;
        let read_count = std::cell::Cell::new(0);
        let caps_count = std::cell::Cell::new(0);
        let url_count = std::cell::Cell::new(0);
        let outcome = handle_grammar_check_shortcut(GrammarCheckShortcutArgs {
            current_field: Some(field.clone()),
            config,
            prefs,
            enabled,
            now_ms,
            last_domain: &mut cache,
            resolve_app_key: |field: FieldHandle| Some(field.app.clone()),
            focused_page_url: |_: FieldHandle| {
                url_count.set(url_count.get() + 1);
                focused_url.map(str::to_string)
            },
            read_context: |field: FieldHandle| {
                read_count.set(read_count.get() + 1);
                Ok(text_context_with_right(&field, "teh", ""))
            },
            capabilities: |_: FieldHandle| {
                caps_count.set(caps_count.get() + 1);
                Ok(writable_axset_caps())
            },
            arm_manual_grammar_request: |_: FieldHandle| Some((77, 88)),
        });
        (outcome, read_count.get(), caps_count.get(), url_count.get())
    }

    #[test]
    fn grammar_check_shortcut_blocks_policy_before_read_context() {
        let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));

        let (_, reads, caps, urls) =
            grammar_shortcut_probe(&config, &config.prefs, false, "TextEdit", 0, None, None);
        assert_eq!((reads, caps, urls), (0, 0, 0));

        let mut prefs = config.prefs.clone();
        prefs.set_app_policy_field("TextEdit", prefs::AppPolicyField::GrammarFix, false);
        let (_, reads, caps, urls) =
            grammar_shortcut_probe(&config, &prefs, true, "TextEdit", 0, None, None);
        assert_eq!((reads, caps, urls), (0, 0, 0));

        let mut prefs = config.prefs.clone();
        prefs.snooze_app("TextEdit", 0, 60);
        let (_, reads, caps, urls) =
            grammar_shortcut_probe(&config, &prefs, true, "TextEdit", 1, None, None);
        assert_eq!((reads, caps, urls), (0, 0, 0));

        let mut prefs = config.prefs.clone();
        prefs.excluded_domains.insert("blocked.example".into());
        let (_, reads, caps, urls) = grammar_shortcut_probe(
            &config,
            &prefs,
            true,
            "com.google.Chrome",
            0,
            Some((
                "com.google.Chrome".to_string(),
                "allowed.example".to_string(),
            )),
            Some("https://blocked.example/doc"),
        );
        assert_eq!((reads, caps, urls), (0, 0, 1));

        let (outcome, reads, caps, urls) =
            grammar_shortcut_probe(&config, &config.prefs, true, "TextEdit", 0, None, None);
        assert_eq!((reads, caps, urls), (1, 1, 0));
        match outcome {
            GrammarCheckShortcutOutcome::Armed(request) => {
                assert_eq!(request.generation, 77);
                assert_eq!(request.snapshot, 88);
                assert!(matches!(request.kind, RequestKind::GrammarFix { .. }));
            }
            other => panic!("expected armed grammar request, got {other:?}"),
        }
    }

    #[test]
    fn grammar_check_shortcut_surfaces_read_context_error_without_capability_or_arm() {
        let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
        let field = field_with_app("TextEdit");
        let mut cache = None;
        let caps_count = std::cell::Cell::new(0);
        let arm_count = std::cell::Cell::new(0);

        let outcome = handle_grammar_check_shortcut(GrammarCheckShortcutArgs {
            current_field: Some(field),
            config: &config,
            prefs: &config.prefs,
            enabled: true,
            now_ms: 0,
            last_domain: &mut cache,
            resolve_app_key: |field: FieldHandle| Some(field.app.clone()),
            focused_page_url: |_: FieldHandle| None,
            read_context: |_| Err(PlatformError::Timeout),
            capabilities: |_| {
                caps_count.set(caps_count.get() + 1);
                Ok(writable_axset_caps())
            },
            arm_manual_grammar_request: |_| {
                arm_count.set(arm_count.get() + 1);
                Some((77, 88))
            },
        });

        assert!(matches!(
            outcome,
            GrammarCheckShortcutOutcome::ReadContextError(PlatformError::Timeout)
        ));
        assert_eq!(caps_count.get(), 0);
        assert_eq!(arm_count.get(), 0);
    }

    #[test]
    fn grammar_check_shortcut_surfaces_capabilities_error_without_arm() {
        let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
        let field = field_with_app("TextEdit");
        let mut cache = None;
        let read_count = std::cell::Cell::new(0);
        let arm_count = std::cell::Cell::new(0);

        let outcome = handle_grammar_check_shortcut(GrammarCheckShortcutArgs {
            current_field: Some(field.clone()),
            config: &config,
            prefs: &config.prefs,
            enabled: true,
            now_ms: 0,
            last_domain: &mut cache,
            resolve_app_key: |field: FieldHandle| Some(field.app.clone()),
            focused_page_url: |_: FieldHandle| None,
            read_context: |field: FieldHandle| {
                read_count.set(read_count.get() + 1);
                Ok(text_context_with_right(&field, "teh", ""))
            },
            capabilities: |_| Err(PlatformError::Timeout),
            arm_manual_grammar_request: |_| {
                arm_count.set(arm_count.get() + 1);
                Some((77, 88))
            },
        });

        assert!(matches!(
            outcome,
            GrammarCheckShortcutOutcome::CapabilitiesError(PlatformError::Timeout)
        ));
        assert_eq!(read_count.get(), 1);
        assert_eq!(arm_count.get(), 0);
    }

    #[test]
    fn grammar_detection_rejects_non_empty_selection() {
        let field = host_field("grammar-selection");
        let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
        let mut ctx = text_context_with_right(&field, "teh", "");
        ctx.selection = Some(platform::TextRange { start: 0, end: 1 });

        assert!(grammar_fix_request(
            &field,
            &ctx,
            grammar_gate(
                &config,
                &config.prefs,
                Some("TextEdit"),
                None,
                true,
                &writable_axset_caps(),
                0,
            ),
        )
        .is_none());
    }

    #[test]
    fn grammar_detection_allows_collapsed_selection() {
        // AX providers may report the caret as an empty selection range rather
        // than None: a collapsed range (start == end) is no selection and must
        // not block grammar fix. Pins the `start != range.end` conjunct.
        let field = host_field("grammar-collapsed-selection");
        let config = Config::from_lookup(lookup(&[("COMPME_GRAMMAR_FIX", "1")]));
        let mut ctx = text_context_with_right(&field, "teh", "");
        ctx.selection = Some(platform::TextRange { start: 3, end: 3 });

        assert!(grammar_fix_request(
            &field,
            &ctx,
            grammar_gate(
                &config,
                &config.prefs,
                Some("TextEdit"),
                None,
                true,
                &writable_axset_caps(),
                0,
            ),
        )
        .is_some());
    }

    fn typed_change_after_baseline(
        field: &FieldHandle,
        baseline: &str,
        next: &str,
    ) -> engine::TextChange {
        let mut tracker = FieldTracker::new();
        let _ = tracker.observe_with_inserted_text(
            field,
            &text_context(field, baseline),
            TriggerPolicy::Automatic,
            1,
        );
        match tracker.observe_with_inserted_text(
            field,
            &text_context(field, next),
            TriggerPolicy::Automatic,
            2,
        ) {
            Observation::Typed(change) => change,
            Observation::CaretMoved { .. } => panic!("expected typed change"),
        }
    }

    fn accepted_store() -> memory::MemoryStore {
        memory::MemoryStore::open_in_memory(
            &memory::StaticKey([3u8; 32]),
            memory::StorageMode::AcceptedOnly,
        )
        .expect("open in-memory store")
    }

    fn queue_and_flush_monitored(
        change: &engine::TextChange,
        store: &memory::MemoryStore,
        prefs: &Prefs,
        enabled: bool,
        secure: bool,
    ) {
        queue_and_flush_monitored_for_app(change, store, prefs, enabled, secure, None);
    }

    fn queue_and_flush_monitored_for_app(
        change: &engine::TextChange,
        store: &memory::MemoryStore,
        prefs: &Prefs,
        enabled: bool,
        secure: bool,
        domain: Option<&str>,
    ) {
        let mut pending = Vec::new();
        let mut buffers = HashMap::new();
        enqueue_monitored_change(
            &mut pending,
            change,
            Some(change.field.app.clone()),
            domain.map(str::to_owned),
        );
        flush_monitored_changes(
            &mut pending,
            &mut buffers,
            Some(store),
            prefs,
            monitored_policy(enabled, secure, true, 1_000),
        );
    }

    fn queue_and_flush_monitored_with_buffers(
        change: &engine::TextChange,
        buffers: &mut HashMap<FieldHandle, MonitoredBuffer>,
        store: &memory::MemoryStore,
        prefs: &Prefs,
        enabled: bool,
        secure: bool,
    ) {
        let mut pending = Vec::new();
        enqueue_monitored_change(&mut pending, change, Some(change.field.app.clone()), None);
        flush_monitored_changes(
            &mut pending,
            buffers,
            Some(store),
            prefs,
            monitored_policy(enabled, secure, true, 1_000),
        );
    }

    fn monitored_policy(
        enabled: bool,
        secure: bool,
        trusted: bool,
        now_ms: u64,
    ) -> MonitoredPolicy {
        MonitoredPolicy {
            enabled,
            secure,
            trusted,
            now_ms,
        }
    }

    fn assert_policy_transition_clears_buffered_monitored_text() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([13u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let prefs = Prefs::default();
        let mut tracker = FieldTracker::new();
        let _ = tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, ""),
            TriggerPolicy::Automatic,
            1,
        );
        let partial = match tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, "secret"),
            TriggerPolicy::Automatic,
            2,
        ) {
            Observation::Typed(change) => change,
            Observation::CaretMoved { .. } => panic!("expected typed change"),
        };
        let mut buffers = HashMap::new();
        queue_and_flush_monitored_with_buffers(&partial, &mut buffers, &store, &prefs, true, false);
        assert_eq!(store.count().unwrap(), 0);
        assert!(!buffers.is_empty());

        let mut pending = Vec::new();
        enqueue_monitored_change(
            &mut pending,
            &partial,
            Some("com.apple.TextEdit".into()),
            None,
        );
        assert!(!pending.is_empty());
        clear_monitored_state_for_policy_transition(&mut pending, &mut buffers);
        assert!(pending.is_empty());
        assert!(buffers.is_empty());
        let boundary = match tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, "secret "),
            TriggerPolicy::Automatic,
            3,
        ) {
            Observation::Typed(change) => change,
            Observation::CaretMoved { .. } => panic!("expected typed change"),
        };
        queue_and_flush_monitored_with_buffers(
            &boundary,
            &mut buffers,
            &store,
            &prefs,
            true,
            false,
        );
        assert_eq!(store.recent("com.apple.TextEdit", 10).unwrap(), vec![" "]);
    }

    #[test]
    fn full_accept_records_to_both_sinks_under_a_resolved_bundle_id() {
        let prev = PreviousInputs::default();
        let store = accepted_store();
        record_full_accept(
            AcceptAction::Full,
            &field_with_app("com.apple.TextEdit"),
            "the quick brown fox",
            160,
            &prev,
            Some(&store),
            true,
        );
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(prev.recent("com.apple.TextEdit").len(), 1);
    }

    #[test]
    fn word_accept_records_nothing() {
        let prev = PreviousInputs::default();
        let store = accepted_store();
        record_full_accept(
            AcceptAction::Word,
            &field_with_app("com.apple.TextEdit"),
            "fox",
            160,
            &prev,
            Some(&store),
            true,
        );
        assert_eq!(store.count().unwrap(), 0);
        assert!(prev.recent("com.apple.TextEdit").is_empty());
    }

    #[test]
    fn full_accept_under_a_volatile_pid_key_records_nothing() {
        let prev = PreviousInputs::default();
        let store = accepted_store();
        record_full_accept(
            AcceptAction::Full,
            &field_with_app("pid:42"),
            "ignored",
            160,
            &prev,
            Some(&store),
            true,
        );
        assert_eq!(store.count().unwrap(), 0);
        assert!(prev.recent("pid:42").is_empty());
    }

    #[test]
    fn full_accept_with_context_disabled_still_records_to_memory() {
        // context_max_chars == 0 disables the previous-input ring, but the
        // encrypted store (its own opt-in) still records.
        let prev = PreviousInputs::default();
        let store = accepted_store();
        record_full_accept(
            AcceptAction::Full,
            &field_with_app("com.apple.TextEdit"),
            "remembered",
            0,
            &prev,
            Some(&store),
            true,
        );
        assert_eq!(store.count().unwrap(), 1);
        assert!(prev.recent("com.apple.TextEdit").is_empty());
    }

    #[test]
    fn all_monitored_records_typed_field_text_after_established_baseline() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([4u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let mut tracker = FieldTracker::new();
        let first = match tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, "pre-existing draft"),
            TriggerPolicy::Automatic,
            1,
        ) {
            Observation::Typed(change) => change,
            Observation::CaretMoved { .. } => panic!("first non-empty snapshot is typed"),
        };
        let prefs = Prefs::default();
        queue_and_flush_monitored(&first, &store, &prefs, true, false);
        assert_eq!(
            store.count().unwrap(),
            0,
            "baseline snapshot is not user typing"
        );

        let second = match tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, "pre-existing draft! "),
            TriggerPolicy::Automatic,
            2,
        ) {
            Observation::Typed(change) => change,
            Observation::CaretMoved { .. } => panic!("second snapshot changed text"),
        };
        queue_and_flush_monitored(&second, &store, &prefs, true, false);
        assert_eq!(store.recent("com.apple.TextEdit", 10).unwrap(), vec!["! "]);
    }

    #[test]
    fn all_monitored_records_first_typed_text_after_empty_baseline() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([7u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let change = typed_change_after_baseline(&field, "", "h ");
        let prefs = Prefs::default();
        queue_and_flush_monitored(&change, &store, &prefs, true, false);
        assert_eq!(store.recent("com.apple.TextEdit", 10).unwrap(), vec!["h "]);
    }

    #[test]
    fn all_monitored_buffers_char_by_char_text_until_redactable_boundary() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([11u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let prefs = Prefs::default();
        let mut tracker = FieldTracker::new();
        let _ = tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, ""),
            TriggerPolicy::Automatic,
            1,
        );
        let mut buffers = HashMap::new();
        for (idx, value) in [
            "a",
            "ad",
            "ada",
            "ada@",
            "ada@e",
            "ada@ex",
            "ada@example",
            "ada@example.",
            "ada@example.com",
        ]
        .into_iter()
        .enumerate()
        {
            let change = match tracker.observe_with_inserted_text(
                &field,
                &text_context(&field, value),
                TriggerPolicy::Automatic,
                (idx + 2) as u64,
            ) {
                Observation::Typed(change) => change,
                Observation::CaretMoved { .. } => panic!("expected typed change"),
            };
            queue_and_flush_monitored_with_buffers(
                &change,
                &mut buffers,
                &store,
                &prefs,
                true,
                false,
            );
            assert_eq!(store.count().unwrap(), 0);
        }

        let change = match tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, "ada@example.com "),
            TriggerPolicy::Automatic,
            99,
        ) {
            Observation::Typed(change) => change,
            Observation::CaretMoved { .. } => panic!("expected typed change"),
        };
        queue_and_flush_monitored_with_buffers(&change, &mut buffers, &store, &prefs, true, false);
        assert_eq!(
            store.recent("com.apple.TextEdit", 10).unwrap(),
            vec!["[redacted-email] "]
        );
    }

    #[test]
    fn accepted_only_does_not_record_monitored_typing() {
        let store = accepted_store();
        let field = field_with_app("com.apple.TextEdit");
        let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
        let prefs = Prefs::default();
        queue_and_flush_monitored(&change, &store, &prefs, true, false);
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn all_monitored_browser_domains_use_fresh_cached_domain_rules() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([14u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.Safari");
        let mut prefs = Prefs::default();
        prefs.excluded_domains.insert("sensitive.example".into());

        let allowed = typed_change_after_baseline(&field, "", "allowed browser text ");
        queue_and_flush_monitored_for_app(
            &allowed,
            &store,
            &prefs,
            true,
            false,
            Some("other.example"),
        );
        assert_eq!(
            store.recent("com.apple.Safari", 10).unwrap(),
            vec!["allowed browser text "]
        );

        let blocked = typed_change_after_baseline(&field, "", "blocked browser text ");
        queue_and_flush_monitored_for_app(
            &blocked,
            &store,
            &prefs,
            true,
            false,
            Some("docs.sensitive.example"),
        );
        assert_eq!(
            store.recent("com.apple.Safari", 10).unwrap(),
            vec!["allowed browser text "]
        );
    }

    #[test]
    fn monitored_typing_honors_collection_privacy_gates() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([5u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
        let mut prefs = Prefs::default();
        prefs
            .per_app
            .entry("com.apple.TextEdit".into())
            .or_default()
            .collect_inputs = Some(false);
        queue_and_flush_monitored(&change, &store, &prefs, true, false);
        assert_eq!(store.count().unwrap(), 0);

        let volatile = field_with_app("pid:42");
        let change = typed_change_after_baseline(&volatile, "", "ordinary typed text ");
        let prefs = Prefs::default();
        queue_and_flush_monitored(&change, &store, &prefs, true, false);
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn monitored_typing_honors_disabled_and_excluded_app_blocks() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([18u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let change = typed_change_after_baseline(&field, "", "ordinary typed text ");

        let mut disabled = Prefs::default();
        disabled
            .per_app
            .entry("com.apple.TextEdit".into())
            .or_default()
            .enabled = Some(false);
        queue_and_flush_monitored(&change, &store, &disabled, true, false);
        assert_eq!(store.count().unwrap(), 0);

        let mut excluded = Prefs::default();
        excluded.excluded_apps.insert("com.apple.TextEdit".into());
        queue_and_flush_monitored(&change, &store, &excluded, true, false);
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn monitored_typing_uses_field_app_fallback_when_app_key_missing() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([28u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let change = typed_change_after_baseline(&field, "", "ordinary typed text ");

        let mut excluded = Prefs::default();
        excluded.excluded_apps.insert("com.apple.TextEdit".into());
        let mut pending = Vec::new();
        enqueue_monitored_change(&mut pending, &change, None, None);
        assert_eq!(
            pending[0].app_key.as_deref(),
            Some("com.apple.TextEdit"),
            "stable field app must be used when pid resolution missed"
        );
        flush_monitored_changes(
            &mut pending,
            &mut HashMap::new(),
            Some(&store),
            &excluded,
            monitored_policy(true, false, true, 1_000),
        );
        assert_eq!(store.count().unwrap(), 0);

        let mut snoozed = Prefs::default();
        snoozed.snooze_app("com.apple.TextEdit", 1_000, 60);
        let mut pending = Vec::new();
        enqueue_monitored_change(&mut pending, &change, None, None);
        flush_monitored_changes(
            &mut pending,
            &mut HashMap::new(),
            Some(&store),
            &snoozed,
            monitored_policy(true, false, true, 1_001),
        );
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn collection_off_drops_partial_monitored_buffer_before_reenable() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([12u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let mut tracker = FieldTracker::new();
        let _ = tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, ""),
            TriggerPolicy::Automatic,
            1,
        );
        let partial = match tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, "secret"),
            TriggerPolicy::Automatic,
            2,
        ) {
            Observation::Typed(change) => change,
            Observation::CaretMoved { .. } => panic!("expected typed change"),
        };

        let mut pending = Vec::new();
        let mut buffers = HashMap::new();
        enqueue_monitored_change(
            &mut pending,
            &partial,
            Some("com.apple.TextEdit".into()),
            None,
        );
        let mut prefs = Prefs::default();
        prefs
            .per_app
            .entry("com.apple.TextEdit".into())
            .or_default()
            .collect_inputs = Some(false);
        flush_monitored_changes(
            &mut pending,
            &mut buffers,
            Some(&store),
            &prefs,
            monitored_policy(true, false, true, 1_000),
        );
        assert!(buffers.is_empty());

        let boundary = match tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, "secret "),
            TriggerPolicy::Automatic,
            3,
        ) {
            Observation::Typed(change) => change,
            Observation::CaretMoved { .. } => panic!("expected typed change"),
        };
        let mut pending = Vec::new();
        enqueue_monitored_change(
            &mut pending,
            &boundary,
            Some("com.apple.TextEdit".into()),
            None,
        );
        flush_monitored_changes(
            &mut pending,
            &mut buffers,
            Some(&store),
            &Prefs::default(),
            monitored_policy(true, false, true, 1_001),
        );
        assert_eq!(store.recent("com.apple.TextEdit", 10).unwrap(), vec![" "]);
    }

    #[test]
    fn oversized_monitored_insert_persists_no_user_text() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([21u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let oversized = format!("{} ", "x".repeat(MAX_MONITORED_BUFFER_CHARS + 1));
        let change = typed_change_after_baseline(&field, "", &oversized);
        queue_and_flush_monitored(&change, &store, &Prefs::default(), true, false);

        assert_eq!(store.count().unwrap(), 0);
        assert_eq!(
            store.recent("com.apple.TextEdit", 10).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn oversized_monitored_insert_with_boundary_clears_partial_buffer() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([15u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let prefs = Prefs::default();
        let mut tracker = FieldTracker::new();
        let _ = tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, ""),
            TriggerPolicy::Automatic,
            1,
        );
        let partial = match tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, "secret"),
            TriggerPolicy::Automatic,
            2,
        ) {
            Observation::Typed(change) => change,
            Observation::CaretMoved { .. } => panic!("expected typed change"),
        };
        let mut buffers = HashMap::new();
        queue_and_flush_monitored_with_buffers(&partial, &mut buffers, &store, &prefs, true, false);
        assert!(!buffers.is_empty());

        let oversized = format!("secret{} ", "x".repeat(MAX_MONITORED_BUFFER_CHARS + 1));
        let change = match tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, &oversized),
            TriggerPolicy::Automatic,
            3,
        ) {
            Observation::Typed(change) => change,
            Observation::CaretMoved { .. } => panic!("expected typed change"),
        };
        queue_and_flush_monitored_with_buffers(&change, &mut buffers, &store, &prefs, true, false);
        assert!(buffers.is_empty());

        let boundary = format!("{oversized} ");
        let change = match tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, &boundary),
            TriggerPolicy::Automatic,
            4,
        ) {
            Observation::Typed(change) => change,
            Observation::CaretMoved { .. } => panic!("expected typed change"),
        };
        queue_and_flush_monitored_with_buffers(&change, &mut buffers, &store, &prefs, true, false);
        assert_eq!(store.recent("com.apple.TextEdit", 10).unwrap(), vec![" "]);
    }

    #[test]
    fn monitored_overflow_drops_until_next_boundary() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([14u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let prefs = Prefs::default();
        let mut tracker = FieldTracker::new();
        let _ = tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, ""),
            TriggerPolicy::Automatic,
            1,
        );
        let mut buffers = HashMap::new();
        let mut value = String::new();
        for idx in 0..=MAX_MONITORED_BUFFER_CHARS {
            value.push('x');
            let change = match tracker.observe_with_inserted_text(
                &field,
                &text_context(&field, &value),
                TriggerPolicy::Automatic,
                (idx + 2) as u64,
            ) {
                Observation::Typed(change) => change,
                Observation::CaretMoved { .. } => panic!("expected typed change"),
            };
            queue_and_flush_monitored_with_buffers(
                &change,
                &mut buffers,
                &store,
                &prefs,
                true,
                false,
            );
        }
        assert_eq!(
            buffers.get(&field),
            Some(&MonitoredBuffer::DroppedUntilBoundary)
        );

        value.push(' ');
        let change = match tracker.observe_with_inserted_text(
            &field,
            &text_context(&field, &value),
            TriggerPolicy::Automatic,
            999,
        ) {
            Observation::Typed(change) => change,
            Observation::CaretMoved { .. } => panic!("expected typed change"),
        };
        queue_and_flush_monitored_with_buffers(&change, &mut buffers, &store, &prefs, true, false);
        assert!(buffers.is_empty());
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn browser_domain_rule_without_fresh_domain_blocks_replacement_offer() {
        let config = Config::from_lookup(lookup(&[("COMPME_EMOJI", "1")]));
        let mut prefs = Prefs::default();
        prefs.excluded_domains.insert("bank.example".into());
        let app = Some("com.apple.Safari");
        let decision = if browser_domain_fresh_enough_for_rules(app, None, &prefs) {
            replacement_decision("hi :smile", &config, &prefs, app, None, true, 0)
        } else {
            None
        };
        assert!(decision.is_none());
    }

    #[test]
    fn policy_transition_drops_partial_monitored_buffer_before_reuse() {
        assert_policy_transition_clears_buffered_monitored_text();
    }

    #[test]
    fn monitored_typing_stops_when_context_collection_is_blocked() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([6u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
        let prefs = Prefs::default();
        queue_and_flush_monitored(&change, &store, &prefs, true, true);
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn monitored_typing_uses_fresh_browser_url_before_persisting() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([20u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.google.Chrome");
        let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
        let mut prefs = Prefs::default();
        prefs.excluded_domains.insert("blocked.example".into());
        let mut cache = Some((
            "com.google.Chrome".to_string(),
            "allowed.example".to_string(),
        ));
        let calls = std::cell::Cell::new(0);
        let mut pending = Vec::new();
        let mut buffers = HashMap::new();

        let domain = enqueue_monitored_change_for_current_domain(
            &mut pending,
            &mut cache,
            &change,
            Some("com.google.Chrome".to_string()),
            true,
            || {
                calls.set(calls.get() + 1);
                Some("https://blocked.example/doc".to_string())
            },
        );
        flush_monitored_changes(
            &mut pending,
            &mut buffers,
            Some(&store),
            &prefs,
            monitored_policy(true, false, true, 1_000),
        );

        assert_eq!(calls.get(), 1);
        assert_eq!(domain.as_deref(), Some("blocked.example"));
        assert_eq!(store.count().unwrap(), 0);
        assert!(buffers.is_empty());
    }

    #[test]
    fn secure_policy_clears_buffered_monitored_text_without_boundary() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([16u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let mut buffers = HashMap::from([(
            field.clone(),
            MonitoredBuffer::Collecting("partial secret".into()),
        )]);
        let mut pending = Vec::new();

        flush_monitored_changes(
            &mut pending,
            &mut buffers,
            Some(&store),
            &Prefs::default(),
            monitored_policy(true, true, true, 1_001),
        );

        assert!(pending.is_empty());
        assert!(buffers.is_empty());
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn monitored_flush_rechecks_secure_input_before_persisting_pending_text() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([17u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
        let mut pending = Vec::new();
        enqueue_monitored_change(
            &mut pending,
            &change,
            Some("com.apple.TextEdit".into()),
            None,
        );
        let mut buffers = HashMap::new();
        let mut secure = false;
        let mut last_secure_poll_ms = None;
        let mut probe_called = false;

        flush_monitored_changes_after_secure_recheck(
            &mut pending,
            &mut buffers,
            Some(&store),
            &Prefs::default(),
            MonitoredFlushState {
                secure: &mut secure,
                last_secure_poll_ms: &mut last_secure_poll_ms,
            },
            MonitoredFlushRuntime {
                monitored_memory_active: true,
                enabled: true,
                trusted: true,
                now_ms: 1_001,
            },
            || {
                probe_called = true;
                true
            },
        );

        assert!(probe_called);
        assert!(secure);
        assert_eq!(last_secure_poll_ms, Some(1_001));
        assert!(pending.is_empty());
        assert!(buffers.is_empty());
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn monitored_flush_persists_when_secure_recheck_clears() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([19u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
        let mut pending = Vec::new();
        enqueue_monitored_change(
            &mut pending,
            &change,
            Some("com.apple.TextEdit".into()),
            None,
        );
        let mut buffers = HashMap::new();
        let mut secure = true;
        let mut last_secure_poll_ms = None;
        let mut probe_called = false;

        flush_monitored_changes_after_secure_recheck(
            &mut pending,
            &mut buffers,
            Some(&store),
            &Prefs::default(),
            MonitoredFlushState {
                secure: &mut secure,
                last_secure_poll_ms: &mut last_secure_poll_ms,
            },
            MonitoredFlushRuntime {
                monitored_memory_active: true,
                enabled: true,
                trusted: true,
                now_ms: 1_002,
            },
            || {
                probe_called = true;
                false
            },
        );

        assert!(probe_called);
        assert!(!secure);
        assert_eq!(last_secure_poll_ms, Some(1_002));
        assert!(pending.is_empty());
        assert!(buffers.is_empty());
        assert_eq!(
            store.recent("com.apple.TextEdit", 10).unwrap(),
            vec!["ordinary typed text "]
        );
    }

    #[test]
    fn monitored_flush_blocks_relaunch_required_effective_untrusted_runtime() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([25u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
        let mut pending = Vec::new();
        enqueue_monitored_change(
            &mut pending,
            &change,
            Some("com.apple.TextEdit".into()),
            None,
        );
        let mut buffers = HashMap::new();
        let mut secure = true;
        let mut last_secure_poll_ms = None;
        let mut probe_called = false;

        flush_monitored_changes_after_secure_recheck(
            &mut pending,
            &mut buffers,
            Some(&store),
            &Prefs::default(),
            MonitoredFlushState {
                secure: &mut secure,
                last_secure_poll_ms: &mut last_secure_poll_ms,
            },
            MonitoredFlushRuntime {
                monitored_memory_active: true,
                enabled: true,
                trusted: runtime_trusted(true, true),
                now_ms: 1_004,
            },
            || {
                probe_called = true;
                false
            },
        );

        assert!(probe_called);
        assert!(!secure);
        assert_eq!(last_secure_poll_ms, Some(1_004));
        assert!(pending.is_empty());
        assert!(buffers.is_empty());
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn monitored_flush_skips_secure_recheck_without_pending_work() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([20u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let mut pending = Vec::new();
        let mut buffers = HashMap::new();
        let mut secure = false;
        let mut last_secure_poll_ms = None;

        flush_monitored_changes_after_secure_recheck(
            &mut pending,
            &mut buffers,
            Some(&store),
            &Prefs::default(),
            MonitoredFlushState {
                secure: &mut secure,
                last_secure_poll_ms: &mut last_secure_poll_ms,
            },
            MonitoredFlushRuntime {
                monitored_memory_active: true,
                enabled: true,
                trusted: true,
                now_ms: 1_003,
            },
            || panic!("secure probe must not run without pending monitored work"),
        );

        assert!(!secure);
        assert_eq!(last_secure_poll_ms, None);
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn monitored_flush_rechecks_secure_input_for_buffered_work() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([22u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let mut pending = Vec::new();
        let mut buffers = HashMap::from([(
            field.clone(),
            MonitoredBuffer::Collecting("partial secret".into()),
        )]);
        let mut secure = false;
        let mut last_secure_poll_ms = None;
        let mut probe_called = false;

        flush_monitored_changes_after_secure_recheck(
            &mut pending,
            &mut buffers,
            Some(&store),
            &Prefs::default(),
            MonitoredFlushState {
                secure: &mut secure,
                last_secure_poll_ms: &mut last_secure_poll_ms,
            },
            MonitoredFlushRuntime {
                monitored_memory_active: true,
                enabled: true,
                trusted: true,
                now_ms: 1_004,
            },
            || {
                probe_called = true;
                true
            },
        );

        assert!(probe_called);
        assert!(secure);
        assert_eq!(last_secure_poll_ms, Some(1_004));
        assert!(pending.is_empty());
        assert!(buffers.is_empty());
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn monitored_buffers_are_isolated_per_same_app_field() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([21u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field_a = field_with_app("com.apple.TextEdit");
        let mut field_b = field_with_app("com.apple.TextEdit");
        field_b.element_id = "ax:other-field".into();
        field_b.generation = 2;
        let prefs = Prefs::default();
        let mut tracker_a = FieldTracker::new();
        let mut tracker_b = FieldTracker::new();
        let _ = tracker_a.observe_with_inserted_text(
            &field_a,
            &text_context(&field_a, ""),
            TriggerPolicy::Automatic,
            1,
        );
        let _ = tracker_b.observe_with_inserted_text(
            &field_b,
            &text_context(&field_b, ""),
            TriggerPolicy::Automatic,
            1,
        );
        let partial_a = match tracker_a.observe_with_inserted_text(
            &field_a,
            &text_context(&field_a, "secret"),
            TriggerPolicy::Automatic,
            2,
        ) {
            Observation::Typed(change) => change,
            Observation::CaretMoved { .. } => panic!("expected typed change"),
        };
        let partial_b = match tracker_b.observe_with_inserted_text(
            &field_b,
            &text_context(&field_b, "note"),
            TriggerPolicy::Automatic,
            2,
        ) {
            Observation::Typed(change) => change,
            Observation::CaretMoved { .. } => panic!("expected typed change"),
        };
        let mut buffers = HashMap::new();
        queue_and_flush_monitored_with_buffers(
            &partial_a,
            &mut buffers,
            &store,
            &prefs,
            true,
            false,
        );
        queue_and_flush_monitored_with_buffers(
            &partial_b,
            &mut buffers,
            &store,
            &prefs,
            true,
            false,
        );
        assert_eq!(store.count().unwrap(), 0);
        assert!(buffers.contains_key(&field_a));
        assert!(buffers.contains_key(&field_b));

        let boundary_b = match tracker_b.observe_with_inserted_text(
            &field_b,
            &text_context(&field_b, "note "),
            TriggerPolicy::Automatic,
            3,
        ) {
            Observation::Typed(change) => change,
            Observation::CaretMoved { .. } => panic!("expected typed change"),
        };
        queue_and_flush_monitored_with_buffers(
            &boundary_b,
            &mut buffers,
            &store,
            &prefs,
            true,
            false,
        );

        assert_eq!(
            store.recent("com.apple.TextEdit", 10).unwrap(),
            vec!["note "]
        );
        assert!(buffers.contains_key(&field_a));
        assert!(!buffers.contains_key(&field_b));
    }

    #[test]
    fn monitored_write_failure_drains_boundary_without_replay() {
        let field = field_with_app("com.apple.TextEdit");
        let prefs = Prefs::default();
        let mut pending = Vec::new();
        let mut buffers = HashMap::new();
        let first = typed_change_after_baseline(&field, "", "first ");
        enqueue_monitored_change(
            &mut pending,
            &first,
            Some("com.apple.TextEdit".into()),
            None,
        );
        let mut attempts = Vec::new();

        flush_monitored_changes_with_monitor(
            &mut pending,
            &mut buffers,
            &prefs,
            monitored_policy(true, false, true, 1_001),
            |field, text| {
                attempts.push((field.app.clone(), text.to_string()));
                Err(memory::MemoryError::Db("forced failure".into()))
            },
        );

        assert_eq!(
            attempts,
            vec![("com.apple.TextEdit".into(), "first ".into())]
        );
        assert!(pending.is_empty());
        assert!(buffers.is_empty());

        let next = typed_change_after_baseline(&field, "first ", "first next ");
        enqueue_monitored_change(&mut pending, &next, Some("com.apple.TextEdit".into()), None);
        flush_monitored_changes_with_monitor(
            &mut pending,
            &mut buffers,
            &prefs,
            monitored_policy(true, false, true, 1_002),
            |field, text| {
                attempts.push((field.app.clone(), text.to_string()));
                Ok(())
            },
        );

        assert_eq!(
            attempts,
            vec![
                ("com.apple.TextEdit".into(), "first ".into()),
                ("com.apple.TextEdit".into(), "next ".into()),
            ]
        );
        assert!(pending.is_empty());
        assert!(buffers.is_empty());
    }

    #[test]
    fn queued_monitored_typing_uses_policy_after_queueing() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([8u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
        let mut pending = Vec::new();
        enqueue_monitored_change(
            &mut pending,
            &change,
            Some("com.apple.TextEdit".into()),
            None,
        );

        let mut prefs = Prefs::default();
        prefs.snooze(1_000, 5);
        let mut buffers = HashMap::new();
        flush_monitored_changes(
            &mut pending,
            &mut buffers,
            Some(&store),
            &prefs,
            monitored_policy(true, false, true, 1_001),
        );
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn queued_monitored_typing_uses_field_app_when_app_key_is_absent() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([9u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.apple.TextEdit");
        let change = typed_change_after_baseline(&field, "", "ordinary typed text ");
        let mut pending = Vec::new();
        enqueue_monitored_change(&mut pending, &change, None, None);

        let mut prefs = Prefs::default();
        prefs
            .per_app
            .entry("com.apple.TextEdit".into())
            .or_default()
            .collect_inputs = Some(false);
        let mut buffers = HashMap::new();
        flush_monitored_changes(
            &mut pending,
            &mut buffers,
            Some(&store),
            &prefs,
            monitored_policy(true, false, true, 1_001),
        );
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn secure_field_blocks_suggestion_offer() {
        // Privacy-critical: secure input (password field / global secure input)
        // must block the collection/suggestion gate BEFORE any other gate gets a
        // say — even when trusted + enabled + an allowed app + an unexcluded
        // domain would otherwise all pass. `!policy.secure` is the first
        // conjunct, so a single secure=true forces false. The edge-detector enum
        // is tested elsewhere; this pins the GATE behavior.
        let prefs = Prefs::default();
        // Everything else green: trusted, enabled, allowed app, no exclusions.
        assert!(
            monitored_collection_gates_pass(
                Some("com.apple.TextEdit"),
                None,
                &prefs,
                monitored_policy(true, false, true, 1_000),
                true,
            ),
            "baseline must pass so the only difference below is `secure`"
        );
        // Flip ONLY secure: the gate must now refuse, proving secure short-circuits
        // ahead of the other (still-green) gates.
        assert!(
            !monitored_collection_gates_pass(
                Some("com.apple.TextEdit"),
                None,
                &prefs,
                monitored_policy(true, true, true, 1_000),
                true,
            ),
            "secure input must block the suggestion/collection gate regardless of \
             every other gate being green"
        );
    }

    #[test]
    fn excluded_apps_gate_blocks_suggestions() {
        // The per-app exclude path (App Settings / tray "Disable in this app"):
        // an excluded app must fail `suggestion_gates_pass` while a sibling app
        // still passes. Today only the domain-exclusion path has a gate test;
        // this pins the APP exclusion through the same shared gate.
        let mut prefs = Prefs::default();
        prefs.excluded_apps.insert("com.apple.Finder".into());
        assert!(
            !suggestion_gates_pass(Some("com.apple.Finder"), "hello there", None, &prefs, 0),
            "an excluded app must be blocked by the suggestion gate"
        );
        assert!(
            suggestion_gates_pass(Some("com.apple.TextEdit"), "hello there", None, &prefs, 0),
            "a non-excluded app must still pass"
        );
    }

    #[test]
    fn tray_disabled_blocks_suggestions_regardless_of_prefs() {
        // The tray Enable toggle is a hard master switch: with `enabled=false`,
        // the suggestion gate stack must withhold offers even when prefs would
        // happily allow the app (default prefs: no exclusions, no snooze) AND a
        // real offer exists. We assert through BOTH seams the run loop uses:
        // - `suggestion_gates_pass` is prefs-only and STILL passes (proving the
        //   pref layer would allow it), so the block must come from `enabled`;
        // - `replacement_decision`, which carries the `enabled` flag, returns
        //   None once the tray is off and Some when it is on (same inputs).
        let config = Config::from_lookup(lookup(&[("COMPME_EMOJI", "1")]));
        let allowed = Some("com.apple.TextEdit");
        assert!(
            suggestion_gates_pass(allowed, "hi :smile", None, &config.prefs, 0),
            "default prefs allow this app — so the block below is the tray switch, not prefs"
        );
        assert!(
            replacement_decision("hi :smile", &config, &config.prefs, allowed, None, true, 0)
                .is_some(),
            "baseline: tray enabled + allowed app + a shortcode offers"
        );
        assert!(
            replacement_decision("hi :smile", &config, &config.prefs, allowed, None, false, 0)
                .is_none(),
            "the tray master switch (enabled=false) must block offers regardless of prefs"
        );
    }

    #[test]
    fn monitored_collection_gates_match_suggestion_privacy_blocks() {
        let mut prefs = Prefs::default();
        assert!(!monitored_collection_gates_pass(
            Some("com.apple.TextEdit"),
            None,
            &prefs,
            monitored_policy(false, false, true, 1_000),
            true,
        ));
        assert!(!monitored_collection_gates_pass(
            Some("com.apple.TextEdit"),
            None,
            &prefs,
            monitored_policy(true, true, true, 1_000),
            true,
        ));
        assert!(!monitored_collection_gates_pass(
            Some("com.apple.TextEdit"),
            None,
            &prefs,
            monitored_policy(true, false, false, 1_000),
            true,
        ));

        prefs.excluded_apps.insert("com.apple.TextEdit".into());
        assert!(!monitored_collection_gates_pass(
            Some("com.apple.TextEdit"),
            None,
            &prefs,
            monitored_policy(true, false, true, 1_000),
            true,
        ));
        prefs.excluded_apps.clear();

        prefs.excluded_domains.insert("sensitive.example".into());
        assert!(!monitored_collection_gates_pass(
            Some("com.apple.Safari"),
            Some("docs.sensitive.example"),
            &prefs,
            monitored_policy(true, false, true, 1_000),
            true,
        ));
        assert!(!monitored_collection_gates_pass(
            Some("com.apple.Safari"),
            None,
            &prefs,
            monitored_policy(true, false, true, 1_000),
            true,
        ));
        assert!(monitored_collection_gates_pass(
            Some("com.apple.Safari"),
            Some("other.example"),
            &prefs,
            monitored_policy(true, false, true, 1_000),
            true,
        ));
        // Every gate open EXCEPT terminal_ok: a shell-history-style field in a
        // terminal must block monitored collection on its own, pinning the
        // `&& terminal_ok` conjunct. Same inputs as the passing case above but
        // with terminal_ok=false.
        assert!(!monitored_collection_gates_pass(
            Some("com.apple.Safari"),
            Some("other.example"),
            &prefs,
            monitored_policy(true, false, true, 1_000),
            false,
        ));
        prefs.excluded_domains.clear();

        prefs.snooze(1_000, 5);
        assert!(!monitored_collection_gates_pass(
            Some("com.apple.TextEdit"),
            None,
            &prefs,
            monitored_policy(true, false, true, 1_001),
            true,
        ));
    }

    #[test]
    fn queued_monitored_typing_preserves_terminal_compatibility_without_prompt() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([10u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.googlecode.iterm2");
        let change = typed_change_after_baseline(&field, "", "git status && ls -la ");
        let mut pending = Vec::new();
        enqueue_monitored_change(
            &mut pending,
            &change,
            Some("com.googlecode.iterm2".into()),
            None,
        );
        assert!(!pending[0].terminal_ok);

        let prefs = Prefs::default();
        let mut buffers = HashMap::new();
        flush_monitored_changes(
            &mut pending,
            &mut buffers,
            Some(&store),
            &prefs,
            monitored_policy(true, false, true, 1_001),
        );
        assert_eq!(store.count().unwrap(), 0);

        let prompt = typed_change_after_baseline(&field, "", "please summarize the diff for ");
        let mut pending = Vec::new();
        enqueue_monitored_change(
            &mut pending,
            &prompt,
            Some("com.googlecode.iterm2".into()),
            None,
        );
        assert!(pending[0].terminal_ok);
        flush_monitored_changes(
            &mut pending,
            &mut buffers,
            Some(&store),
            &prefs,
            monitored_policy(true, false, true, 1_002),
        );
        assert_eq!(
            store.recent("com.googlecode.iterm2", 10).unwrap(),
            vec!["please summarize the diff for "]
        );
    }

    #[test]
    fn queued_monitored_typing_uses_field_app_for_terminal_policy_when_app_key_missing() {
        let store = memory::MemoryStore::open_in_memory(
            &memory::StaticKey([29u8; 32]),
            memory::StorageMode::AllMonitored,
        )
        .expect("open in-memory store");
        let field = field_with_app("com.googlecode.iterm2");
        let change = typed_change_after_baseline(&field, "", "git status && ls -la ");
        let mut pending = Vec::new();
        enqueue_monitored_change(&mut pending, &change, None, None);
        assert_eq!(pending[0].app_key.as_deref(), Some("com.googlecode.iterm2"));
        assert!(
            !pending[0].terminal_ok,
            "terminal command text must not fail open when pid resolution misses"
        );

        flush_monitored_changes(
            &mut pending,
            &mut HashMap::new(),
            Some(&store),
            &Prefs::default(),
            monitored_policy(true, false, true, 1_000),
        );
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn accept_word_count_is_at_least_one() {
        assert_eq!(accept_word_count("the quick brown fox"), 4);
        assert_eq!(accept_word_count("solo"), 1);
        assert_eq!(accept_word_count("   "), 1); // whitespace-only still counts as one
        assert_eq!(accept_word_count(""), 1);
    }

    #[test]
    fn mirror_mode_only_for_mirror_only_apps() {
        assert!(mirror_mode_for(Some("org.mozilla.firefox")));
        assert!(!mirror_mode_for(Some("com.apple.TextEdit")));
        assert!(!mirror_mode_for(None)); // unresolved app → inline
    }

    #[test]
    fn stat_outcome_maps_engine_events() {
        assert_eq!(
            stat_outcome(engine::StatEvent::Shown),
            stats::Outcome::Shown
        );
        assert_eq!(
            stat_outcome(engine::StatEvent::Superseded),
            stats::Outcome::Superseded
        );
    }

    #[test]
    fn canonicalize_field_app_replaces_volatile_pid_app_with_bundle_id() {
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: "ax:field".into(),
            generation: 7,
        };
        let (canonical, app_key) = canonicalize_field_app(field, |pid| {
            (pid == 42).then(|| "com.apple.TextEdit".into())
        });

        assert_eq!(app_key.as_deref(), Some("com.apple.TextEdit"));
        assert_eq!(canonical.app, "com.apple.TextEdit");
        assert_eq!(canonical.pid, Some(42));
        assert_eq!(canonical.element_id, "ax:field");
    }

    #[test]
    fn canonicalize_field_app_returns_stable_fallback_key_on_resolver_miss() {
        let field = FieldHandle {
            app: "com.apple.TextEdit".into(),
            pid: Some(42),
            element_id: "ax:field".into(),
            generation: 7,
        };
        let (canonical, app_key) = canonicalize_field_app(field, |_| None);

        assert_eq!(app_key.as_deref(), Some("com.apple.TextEdit"));
        assert_eq!(canonical.app, "com.apple.TextEdit");
    }

    #[test]
    fn previous_inputs_record_and_read_with_canonical_bundle_id() {
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: "ax:field".into(),
            generation: 7,
        };
        let (canonical, _) = canonicalize_field_app(field, |pid| {
            (pid == 42).then(|| "com.apple.TextEdit".into())
        });
        let previous_inputs = PreviousInputs::default();
        previous_inputs.record(&canonical.app, "accepted completion".into());
        let worker_context = WorkerContext {
            previous_inputs,
            max_chars: 200,
            ..WorkerContext::default()
        };
        let matching_request = engine::CompletionRequest {
            generation: 7,
            field: canonical.clone(),
            domain: None,
            snapshot: 7,
            prompt: "now".into(),
            max_tokens: 8,
            kind: RequestKind::Completion,
        };
        let volatile_request = engine::CompletionRequest {
            generation: 7,
            field: FieldHandle {
                app: "pid:42".into(),
                pid: Some(42),
                element_id: "ax:field".into(),
                generation: 7,
            },
            domain: None,
            snapshot: 7,
            prompt: "now".into(),
            max_tokens: 8,
            kind: RequestKind::Completion,
        };

        assert!(worker_context
            .block_for(&matching_request)
            .contains("accepted completion"));
        assert!(!worker_context
            .block_for(&volatile_request)
            .contains("accepted completion"));
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
        assert!(!config.grammar_fix);
        assert!(!config.diag_coords);
    }

    #[test]
    fn min_context_parses_and_clamps() {
        assert_eq!(
            Config::from_lookup(lookup(&[("COMPME_MIN_CONTEXT", "5")])).min_context_chars,
            5
        );
        // over max → clamps to 100
        assert_eq!(
            Config::from_lookup(lookup(&[("COMPME_MIN_CONTEXT", "999")])).min_context_chars,
            100
        );
        // unparseable → default
        assert_eq!(
            Config::from_lookup(lookup(&[("COMPME_MIN_CONTEXT", "lots")])).min_context_chars,
            DEFAULT_MIN_CONTEXT_CHARS
        );
    }

    #[test]
    fn midline_opt_in_by_one_or_true() {
        assert!(Config::from_lookup(lookup(&[("COMPME_MIDLINE", "1")])).allow_mid_word);
        assert!(Config::from_lookup(lookup(&[("COMPME_MIDLINE", "true")])).allow_mid_word);
        assert!(!Config::from_lookup(lookup(&[("COMPME_MIDLINE", "no")])).allow_mid_word);
    }

    #[test]
    fn trailing_space_opt_in_by_one_or_true_and_off_by_default() {
        assert!(Config::from_lookup(lookup(&[("COMPME_TRAILING_SPACE", "1")])).trailing_space);
        assert!(Config::from_lookup(lookup(&[("COMPME_TRAILING_SPACE", "true")])).trailing_space);
        assert!(!Config::from_lookup(lookup(&[("COMPME_TRAILING_SPACE", "no")])).trailing_space);
        // Off by default when the key is absent (byte-identical accept behavior).
        assert!(!Config::from_lookup(lookup(&[])).trailing_space);
    }

    #[test]
    fn emoji_config_off_by_default_and_parses_prefs_when_enabled() {
        // Absent / falsy → disabled (None).
        assert!(Config::from_lookup(lookup(&[])).emoji.is_none());
        assert!(Config::from_lookup(lookup(&[("COMPME_EMOJI", "no")]))
            .emoji
            .is_none());
        // Enabled → Some with default prefs.
        let on = Config::from_lookup(lookup(&[("COMPME_EMOJI", "1")]))
            .emoji
            .expect("enabled");
        assert_eq!(on, EmojiPrefs::default());
        // Skin tone + gender parsed.
        let custom = Config::from_lookup(lookup(&[
            ("COMPME_EMOJI", "on"),
            ("COMPME_EMOJI_SKIN_TONE", "medium-dark"),
            ("COMPME_EMOJI_GENDER", "female"),
        ]))
        .emoji
        .expect("enabled");
        assert_eq!(custom.skin_tone, SkinTone::MediumDark);
        assert_eq!(custom.gender, Gender::Female);
    }

    #[test]
    fn emoji_offer_gated_by_enable_and_shortcode() {
        let prefs = Some(EmojiPrefs::default());
        // Enabled + a trailing :shortcode → offers (glyph, chars-to-replace).
        let (glyph, replace_left) = emoji_offer("hi :smile", &prefs).expect("offer");
        assert!(!glyph.is_empty());
        assert_eq!(replace_left, 6); // ":smile"
                                     // Enabled but no shortcode → no offer.
        assert!(emoji_offer("hello world", &prefs).is_none());
        // Disabled (None) → never offers, even with a shortcode.
        assert!(emoji_offer("hi :smile", &None).is_none());
    }

    #[test]
    fn trailing_word_extracts_the_word_at_the_caret() {
        assert_eq!(trailing_word("I teh"), Some("teh"));
        assert_eq!(trailing_word("color"), Some("color"));
        assert_eq!(trailing_word("café"), Some("café")); // multibyte
        assert_eq!(trailing_word("x:smile"), Some("smile")); // ':' is a boundary
        assert_eq!(trailing_word("done "), None); // trailing space = boundary
        assert_eq!(trailing_word("a1b"), Some("b")); // digit is a boundary
        assert_eq!(trailing_word(""), None);
    }

    #[test]
    fn autocorrect_and_british_off_by_default() {
        let config = Config::from_lookup(lookup(&[]));
        assert!(!config.autocorrect);
        assert!(!config.grammar_fix);
        assert!(!config.british_english);
        assert!(!config.thesaurus);
        // Off → no word-based offer even on a known typo / americanism.
        assert!(replacement_offer("teh", &config, config.autocorrect, config.thesaurus).is_none());
        assert!(
            replacement_offer("color", &config, config.autocorrect, config.thesaurus).is_none()
        );
    }

    #[test]
    fn replacement_offer_fires_for_enabled_word_features() {
        let ac = Config::from_lookup(lookup(&[("COMPME_AUTOCORRECT", "1")]));
        assert_eq!(
            replacement_offer("I teh", &ac, ac.autocorrect, ac.thesaurus),
            Some((vec!["the".into()], 3))
        );
        // A correctly-spelled word never offers.
        assert!(replacement_offer("the", &ac, ac.autocorrect, ac.thesaurus).is_none());

        let uk = Config::from_lookup(lookup(&[("COMPME_BRITISH_ENGLISH", "on")]));
        assert_eq!(
            replacement_offer("color", &uk, uk.autocorrect, uk.thesaurus),
            Some((vec!["colour".into()], 5))
        );
        assert!(replacement_offer("colour", &uk, uk.autocorrect, uk.thesaurus).is_none());
    }

    #[test]
    fn replacement_offer_prioritizes_emoji_then_word_features() {
        // Emoji shortcode wins over the word-based features when all are enabled.
        let all = Config::from_lookup(lookup(&[
            ("COMPME_EMOJI", "1"),
            ("COMPME_AUTOCORRECT", "1"),
            ("COMPME_BRITISH_ENGLISH", "1"),
            ("COMPME_THESAURUS", "1"),
        ]));
        let (candidates, replace_left) =
            replacement_offer("teh :smile", &all, all.autocorrect, all.thesaurus)
                .expect("emoji wins");
        assert_eq!(candidates[0], "😄"); // emoji wins
        assert_eq!(replace_left, 6); // ":smile", not the word "teh"
    }

    #[test]
    fn replacement_offer_falls_through_autocorrect_to_localize() {
        // With BOTH word features on: a US spelling is not a typo, so it
        // must fall THROUGH autocorrect (None) to the UK fix; a typo takes
        // the autocorrect branch first.
        let both = Config::from_lookup(lookup(&[
            ("COMPME_AUTOCORRECT", "1"),
            ("COMPME_BRITISH_ENGLISH", "1"),
        ]));
        assert_eq!(
            replacement_offer("color", &both, both.autocorrect, both.thesaurus),
            Some((vec!["colour".into()], 5))
        );
        assert_eq!(
            replacement_offer("teh", &both, both.autocorrect, both.thesaurus),
            Some((vec!["the".into()], 3))
        );
    }

    #[test]
    fn grammar_capitalizes_standalone_i_under_the_autocorrect_gate() {
        // "i" -> "I" is a grammar fix wired into replacement_offer behind the
        // autocorrect toggle (so it stays off in code fields like typo-fixing).
        let on = Config::from_lookup(lookup(&[("COMPME_AUTOCORRECT", "1")]));
        // A lone lowercase pronoun is offered as capital "I", replacing 1 char.
        assert_eq!(
            replacement_offer("i", &on, on.autocorrect, on.thesaurus),
            Some((vec!["I".into()], 1))
        );
        // Words that merely start with "i" are untouched (no false fix).
        assert_eq!(
            replacement_offer("in", &on, on.autocorrect, on.thesaurus),
            None
        );
        assert_eq!(
            replacement_offer("idea", &on, on.autocorrect, on.thesaurus),
            None
        );
        // Contraction limitation pinned: `trailing_word` tokenizes on the
        // apostrophe (it takes only alphabetic chars), so "i'm" reaches the
        // pipeline as "m" and no grammar fix fires — even though
        // grammar::capitalize_pronoun("i'm") itself returns "I'm". Capitalizing
        // contractions would need the caret-token model to include apostrophes.
        assert_eq!(
            replacement_offer("i'm", &on, on.autocorrect, on.thesaurus),
            None
        );
        // Gated off: autocorrect disabled -> no grammar fix either.
        let off = Config::from_lookup(lookup(&[("COMPME_AUTOCORRECT", "0")]));
        assert_eq!(
            replacement_offer("i", &off, off.autocorrect, off.thesaurus),
            None
        );
    }

    #[test]
    fn thesaurus_offer_fires_for_enabled_feature() {
        let th = Config::from_lookup(lookup(&[("COMPME_THESAURUS", "1")]));
        let (syns, word_len) =
            replacement_offer("I am happy", &th, th.autocorrect, th.thesaurus).expect("offer");
        assert!(syns.contains(&"glad".to_string()));
        assert!(!syns.contains(&"happy".to_string()));
        assert_eq!(word_len, 5); // "happy"
    }

    #[test]
    fn replacement_decision_honors_snooze_and_auto_resumes() {
        // The fn's own contract: a local offer must not show while the model
        // is snoozed. Snooze lives in runtime-mutated prefs, passed
        // separately from config — this is the local path's own gate test.
        let config = Config::from_lookup(lookup(&[("COMPME_EMOJI", "1")]));
        let mut prefs = Prefs::default();
        prefs.snooze(1_000, 60);
        let app = Some("com.apple.TextEdit");
        assert!(
            replacement_decision("hi :smile", &config, &prefs, app, None, true, 2_000).is_none()
        );
        // 60 minutes later the snooze expired → offers again.
        let after = 1_000 + 60 * 60_000;
        assert!(
            replacement_decision("hi :smile", &config, &prefs, app, None, true, after).is_some()
        );
    }

    #[test]
    fn replacement_decision_uses_canonical_fallback_on_resolver_miss() {
        let config = Config::from_lookup(lookup(&[("COMPME_EMOJI", "1")]));
        let mut prefs = Prefs::default();
        prefs.excluded_apps.insert("com.apple.TextEdit".into());
        let field = FieldHandle {
            app: "com.apple.TextEdit".into(),
            pid: Some(42),
            element_id: "ax:field".into(),
            generation: 7,
        };
        let (_, app_key) = canonicalize_field_app(field, |_| None);

        assert_eq!(app_key.as_deref(), Some("com.apple.TextEdit"));
        assert!(
            replacement_decision(
                "hi :smile",
                &config,
                &prefs,
                app_key.as_deref(),
                None,
                true,
                0
            )
            .is_none(),
            "local replacements must not fail open when pid resolution misses"
        );
    }

    #[test]
    fn per_app_autocorrect_on_list_overrides_a_global_off() {
        // COMPME_AUTOCORRECT_ON_APPS: the positive override loop — a typo'd
        // key string in that parse would silently kill the feature.
        let prefs = build_prefs(&lookup(&[("COMPME_AUTOCORRECT_ON_APPS", "com.a.one")]));
        assert!(prefs.autocorrect_enabled(Some("com.a.one"), false));
        assert!(!prefs.autocorrect_enabled(Some("com.other"), false));
    }

    #[test]
    fn grammar_fix_config_and_per_app_lists_parse() {
        let config = Config::from_lookup(lookup(&[
            ("COMPME_GRAMMAR_FIX", "on"),
            ("COMPME_GRAMMAR_ACCEPT_KEY", "ctrl+96"),
            ("COMPME_GRAMMAR_CHECK_KEY", "shift+96"),
        ]));
        assert!(config.grammar_fix);
        assert_eq!(
            config.grammar_accept_key,
            crate::shell::parse_accept_key("ctrl+96")
        );
        assert_eq!(config.grammar_check_key.as_deref(), Some("shift+96"));

        let prefs = build_prefs(&lookup(&[
            ("COMPME_GRAMMAR_FIX_ON_APPS", "com.a.one"),
            ("COMPME_GRAMMAR_FIX_OFF_APPS", "com.a.two"),
        ]));
        assert!(prefs.grammar_fix_enabled(Some("com.a.one"), false));
        assert!(!prefs.grammar_fix_enabled(Some("com.a.two"), true));
        assert!(!prefs.grammar_fix_enabled(Some("com.other"), false));
    }

    #[test]
    fn trusted_key_parses_valid_hex_and_fails_closed_otherwise() {
        // COMPME_TRUSTED_KEY gates whether signed deep links can EVER apply;
        // the lookup→from_hex wiring is the app's security posture switch.
        // (from_hex validates a real Ed25519 point — this is the basepoint.)
        let valid = "5866666666666666666666666666666666666666666666666666666666666666";
        let with_key = Config::from_lookup(lookup(&[("COMPME_TRUSTED_KEY", valid)]));
        assert!(with_key.trusted_key.is_some());
        let junk = Config::from_lookup(lookup(&[("COMPME_TRUSTED_KEY", "not-hex")]));
        assert!(junk.trusted_key.is_none(), "malformed key fails closed");
        let absent = Config::from_lookup(lookup(&[]));
        assert!(
            absent.trusted_key.is_none(),
            "default: signed links rejected"
        );
    }

    #[test]
    fn context_bound_lifts_zero_only_when_an_auxiliary_source_is_active() {
        // clipboard/screen context with max_chars == 0 would be a silent
        // no-op (the worker's block builder returns "" at bound 0).
        assert_eq!(
            context_bound_chars(true, false, 0),
            DEFAULT_CONTEXT_MAX_CHARS
        );
        assert_eq!(
            context_bound_chars(false, true, 0),
            DEFAULT_CONTEXT_MAX_CHARS
        );
        assert_eq!(
            context_bound_chars(false, false, 0),
            0,
            "nothing enabled stays off"
        );
        assert_eq!(
            context_bound_chars(true, true, 50),
            50,
            "explicit bound wins"
        );
    }

    #[test]
    fn settings_context_bound_supports_late_clipboard_enable() {
        // Clipboard can now be enabled from Settings after launch; the inference
        // worker therefore needs a positive bound even when context env vars
        // were off at startup.
        assert_eq!(settings_context_bound_chars(0), DEFAULT_CONTEXT_MAX_CHARS);
        assert_eq!(settings_context_bound_chars(120), 120);
    }

    #[test]
    fn parse_license_accepted_round_trips_and_normalizes() {
        // None → empty set; messy hand-edited values trim and drop empties;
        // serialize (via record_license_acceptance) is sorted + deduped, so
        // parse(serialize(parse(x))) == parse(x).
        assert!(parse_license_accepted(None).is_empty());
        let parsed = parse_license_accepted(Some(" b , ,a ".into()));
        assert_eq!(
            parsed.iter().cloned().collect::<Vec<_>>(),
            vec!["a".to_string(), "b".to_string()]
        );
        let mut set = parsed.clone();
        let serialized = record_license_acceptance(&mut set, "a"); // duplicate
        assert_eq!(serialized, "a,b", "sorted, deduped, unchanged by re-accept");
        assert_eq!(parse_license_accepted(Some(serialized)), parsed);
    }

    #[test]
    fn record_license_acceptance_inserts_new_models() {
        let mut set = std::collections::BTreeSet::new();
        assert_eq!(
            record_license_acceptance(&mut set, "gemma-2-2b-q4_k_m"),
            "gemma-2-2b-q4_k_m"
        );
        assert_eq!(
            record_license_acceptance(&mut set, "llama-3.2-1b-q4_k_m"),
            "gemma-2-2b-q4_k_m,llama-3.2-1b-q4_k_m"
        );
        assert!(set.contains("gemma-2-2b-q4_k_m"));
    }

    #[test]
    fn config_parses_license_accepted_from_lookup() {
        let config = Config::from_lookup(lookup(&[("COMPME_LICENSE_ACCEPTED", "x-model,y-model")]));
        assert!(config.license_accepted.contains("x-model"));
        assert!(config.license_accepted.contains("y-model"));
        assert!(Config::from_lookup(lookup(&[])).license_accepted.is_empty());
    }

    #[test]
    fn catalog_download_request_threads_the_entry_hash_to_the_verifier() {
        // The consume edge previously hardcoded expected_sha256: None — a
        // pinned catalog hash would have been silently ignored. The request
        // builder must carry the entry's hash so verify-before-rename
        // engages the moment a hash lands in the catalog.
        let entry = model_catalog::ModelEntry {
            name: "test-model",
            url: "https://example.invalid/m.gguf",
            size_mb: 1,
            min_ram_gb: 1,
            license: model_catalog::License::Apache2,
            expected_sha256: Some(
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
        };
        let status = std::sync::Arc::new(model_fetch::DownloadStatus::default());
        let request = catalog_download_request(
            &entry,
            PathBuf::from("/tmp/m.gguf"),
            std::sync::Arc::clone(&status),
        );
        assert_eq!(
            request.expected_sha256.as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert_eq!(request.url, entry.url);
        assert_eq!(request.dest, PathBuf::from("/tmp/m.gguf"));
        assert_eq!(request.max_bytes, Some(1024 * 1024));
        // The SAME status block must ride along — a helper constructing a
        // fresh one would silently break progress polling.
        assert!(std::sync::Arc::ptr_eq(&request.status, &status));

        // Unpinned entry → no verification requested (downloader skips).
        let unpinned = model_catalog::ModelEntry {
            expected_sha256: None,
            ..entry
        };
        let status = std::sync::Arc::new(model_fetch::DownloadStatus::default());
        let request = catalog_download_request(&unpinned, PathBuf::from("/tmp/m.gguf"), status);
        assert_eq!(request.expected_sha256, None);
    }

    #[test]
    fn download_idle_blocks_only_an_in_flight_download() {
        use model_fetch::{DownloadState, DownloadStatus};
        // The latent one-shot bug (found by the picker design audit): the
        // old `model_download_status.is_none()` guard never reset, so after
        // ONE download — even a Failed one — every later request was
        // silently swallowed for the process lifetime. Idle/Running block
        // (in flight); Done/Failed re-allow (retry and re-download work).
        assert!(download_idle(None), "no download yet");
        let status = DownloadStatus::default(); // state: Idle (queued)
        assert!(!download_idle(Some(&status)), "queued blocks");
        *status.state.lock().unwrap() = DownloadState::Running;
        assert!(!download_idle(Some(&status)), "running blocks");
        *status.state.lock().unwrap() = DownloadState::Done("/tmp/m.gguf".into());
        assert!(download_idle(Some(&status)), "done re-allows");
        *status.state.lock().unwrap() = DownloadState::Failed("boom".into());
        assert!(download_idle(Some(&status)), "failed re-allows retry");
    }

    #[test]
    fn model_present_only_for_a_nonempty_existing_file() {
        // The dest-exists guard: a complete .gguf already on disk skips the
        // re-download (avoid clobber + wasted bandwidth on a repeat click).
        assert!(model_present(Some(1)), "a 1-byte+ file is present");
        assert!(model_present(Some(500_000_000)), "a real model is present");
        // A missing file OR a 0-byte stub (an interrupted finalize) is NOT
        // present — re-download rather than treat the stub as done.
        assert!(!model_present(None), "missing file → re-download");
        assert!(!model_present(Some(0)), "0-byte stub → re-download");
    }

    #[test]
    fn model_download_requeues_existing_file_when_hash_mismatches() {
        const EXPECTED_HASH: &str =
            "3aa927ba0345110f5880efe4a064beafcd9b37d4652c0293ca266654223ebf1f";
        let entry = model_catalog::ModelEntry {
            name: "test-model",
            url: "https://example.invalid/m.gguf",
            size_mb: 1,
            min_ram_gb: 1,
            license: model_catalog::License::Apache2,
            expected_sha256: Some(EXPECTED_HASH),
        };
        let dest = std::env::temp_dir().join(format!(
            "compme-existing-hash-mismatch-{}.gguf",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&dest);
        std::fs::write(&dest, b"wrong model bytes").unwrap();
        assert_eq!(
            model_download_dest_present(&dest, Some(EXPECTED_HASH)),
            Ok(false),
            "the helper must hash nonempty pinned files before trusting them"
        );
        let mut downloader = Some(());
        let mut status = None;
        let mut logged = 7;
        let requested_hash = std::cell::RefCell::new(None::<String>);

        let result = start_model_download_edge(ModelDownloadEdge {
            entry: &entry,
            dest: &dest,
            downloader: &mut downloader,
            model_download_status: &mut status,
            model_download_logged: &mut logged,
            prepare: |_: &std::path::Path| Ok(()),
            existing_model: model_download_dest_present,
            spawn: || Ok(()),
            request: |_: &(), request: model_fetch::DownloadRequest| {
                *requested_hash.borrow_mut() = request.expected_sha256;
                true
            },
        });
        let _ = std::fs::remove_file(&dest);

        assert_eq!(
            result,
            DownloadStartResult::Queued,
            "a nonempty file with the wrong hash must be re-downloaded"
        );
        assert_eq!(requested_hash.borrow().as_deref(), entry.expected_sha256);
        assert!(
            status.is_some(),
            "queued re-download must expose a fresh status block"
        );
        assert_eq!(logged, 0);
        std::fs::write(&dest, b"expected model bytes").unwrap();
        assert_eq!(
            model_download_dest_present(&dest, Some(EXPECTED_HASH)),
            Ok(true),
            "a matching pinned model may skip the download"
        );
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn model_download_skips_existing_file_when_hash_matches() {
        const EXPECTED_HASH: &str =
            "de516b3d3641c9011fbf3cea3198c39f339fd92066b124279b69949640b171a5";
        let entry = model_catalog::ModelEntry {
            name: "test-model",
            url: "https://example.invalid/m.gguf",
            size_mb: 1,
            min_ram_gb: 1,
            license: model_catalog::License::Apache2,
            expected_sha256: Some(EXPECTED_HASH),
        };
        let dest = std::env::temp_dir().join(format!(
            "compme-existing-hash-match-{}.gguf",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&dest);
        std::fs::write(&dest, b"matching model bytes").unwrap();
        let mut downloader = Some(());
        let original_status = std::sync::Arc::new(model_fetch::DownloadStatus::default());
        *original_status.state.lock().unwrap() =
            model_fetch::DownloadState::Done("/tmp/previous.gguf".into());
        let original_ptr = std::sync::Arc::as_ptr(&original_status);
        let mut status = Some(original_status);
        let mut logged = 2;
        let requested = std::cell::Cell::new(false);

        let result = start_model_download_edge(ModelDownloadEdge {
            entry: &entry,
            dest: &dest,
            downloader: &mut downloader,
            model_download_status: &mut status,
            model_download_logged: &mut logged,
            prepare: |_: &std::path::Path| Ok(()),
            existing_model: model_download_dest_present,
            spawn: || Ok(()),
            request: |_: &(), _request: model_fetch::DownloadRequest| {
                requested.set(true);
                true
            },
        });
        let _ = std::fs::remove_file(&dest);

        assert_eq!(result, DownloadStartResult::AlreadyPresent);
        assert!(!requested.get(), "matching existing model skips enqueue");
        assert_eq!(
            status.as_ref().map(std::sync::Arc::as_ptr),
            Some(original_ptr),
            "skip path must not replace the tracked status block"
        );
        assert_eq!(logged, 2);
    }

    #[test]
    fn model_download_busy_does_not_replace_tracked_status() {
        let entry = model_catalog::recommended().expect("catalog has a recommended model");
        let dest = std::path::PathBuf::from("/tmp/compme-model.gguf");
        let mut downloader = Some(());
        let original_status = std::sync::Arc::new(model_fetch::DownloadStatus::default());
        *original_status.state.lock().unwrap() =
            model_fetch::DownloadState::Failed("previous failure".into());
        let original_ptr = std::sync::Arc::as_ptr(&original_status);
        let mut status = Some(original_status);
        let mut logged = 2;

        let result = start_model_download_edge(ModelDownloadEdge {
            entry,
            dest: &dest,
            downloader: &mut downloader,
            model_download_status: &mut status,
            model_download_logged: &mut logged,
            prepare: |_: &std::path::Path| Ok(()),
            existing_model: |_: &std::path::Path, _: Option<&str>| Ok(false),
            spawn: || Ok(()),
            request: |_: &(), _request: model_fetch::DownloadRequest| false,
        });

        assert_eq!(result, DownloadStartResult::Busy);
        assert_eq!(
            status.as_ref().map(std::sync::Arc::as_ptr),
            Some(original_ptr),
            "dropped requests must not expose a fresh idle status"
        );
        assert_eq!(logged, 2);
    }

    #[test]
    fn model_download_ram_block_message_blocks_only_below_minimum() {
        let entry = model_catalog::recommended().expect("catalog has a recommended model");
        assert!(
            model_download_ram_block_message(entry, entry.min_ram_gb.saturating_sub(1))
                .expect("below minimum is blocked")
                .contains(entry.name)
        );
        assert_eq!(
            model_download_ram_block_message(entry, entry.min_ram_gb),
            None,
            "tight-at-minimum models are allowed with a picker warning"
        );
    }

    #[test]
    fn model_download_click_blocks_below_min_ram_before_prompt_or_enqueue() {
        let entry = model_catalog::recommended().expect("catalog has a recommended model");
        let mut accepted = std::collections::BTreeSet::new();
        let prompted = std::cell::Cell::new(false);

        let decision = model_download_click_decision(
            crate::model_picker::recommended_index(),
            entry.min_ram_gb.saturating_sub(1),
            &mut accepted,
            |_, _, _| {
                prompted.set(true);
                true
            },
        )
        .expect("catalog has an entry");

        match decision {
            ModelDownloadClickDecision::BlockedByRam(message) => {
                assert!(message.contains(entry.name));
            }
            other => panic!("expected RAM block, got {other:?}"),
        }
        assert!(
            !prompted.get(),
            "RAM block must happen before license prompt"
        );
        assert!(accepted.is_empty());
    }

    #[test]
    fn model_download_click_declines_license_without_recording_acceptance() {
        let encumbered_index = model_catalog::catalog()
            .iter()
            .position(|entry| entry.license.needs_acceptance())
            .expect("catalog has an encumbered entry");
        let encumbered = &model_catalog::catalog()[encumbered_index];
        let mut accepted = std::collections::BTreeSet::new();

        let decision = model_download_click_decision(
            encumbered_index,
            encumbered.min_ram_gb,
            &mut accepted,
            |model, license_name, terms_url| {
                assert_eq!(model, encumbered.name);
                assert_eq!(license_name, encumbered.license.display_name());
                assert_eq!(terms_url, encumbered.license.terms_url());
                false
            },
        )
        .expect("catalog has an entry");

        assert_eq!(
            decision,
            ModelDownloadClickDecision::LicenseDeclined {
                model: encumbered.name
            }
        );
        assert!(
            accepted.is_empty(),
            "declining a license must not persist acceptance"
        );
    }

    #[test]
    fn model_download_click_accepts_license_and_returns_persist_value() {
        let encumbered_index = model_catalog::catalog()
            .iter()
            .position(|entry| entry.license.needs_acceptance())
            .expect("catalog has an encumbered entry");
        let encumbered = &model_catalog::catalog()[encumbered_index];
        let mut accepted = std::collections::BTreeSet::new();

        let decision = model_download_click_decision(
            encumbered_index,
            encumbered.min_ram_gb,
            &mut accepted,
            |model, license_name, terms_url| {
                assert_eq!(model, encumbered.name);
                assert_eq!(license_name, encumbered.license.display_name());
                assert_eq!(terms_url, encumbered.license.terms_url());
                true
            },
        )
        .expect("catalog has an entry");

        match decision {
            ModelDownloadClickDecision::Ready {
                entry,
                accepted_license: Some(accepted_license),
            } => {
                assert_eq!(entry.name, encumbered.name);
                assert_eq!(accepted_license.model, encumbered.name);
                assert_eq!(
                    accepted_license.license_name,
                    encumbered.license.display_name()
                );
                assert_eq!(accepted_license.value, encumbered.name);
            }
            other => panic!("expected accepted license ready decision, got {other:?}"),
        }
        assert!(accepted.contains(encumbered.name));
    }

    #[test]
    fn model_download_click_skips_prompt_for_already_accepted_license() {
        let encumbered_index = model_catalog::catalog()
            .iter()
            .position(|entry| entry.license.needs_acceptance())
            .expect("catalog has an encumbered entry");
        let encumbered = &model_catalog::catalog()[encumbered_index];
        // Seed the accepted set so the download gate proceeds without prompting.
        let mut accepted = std::collections::BTreeSet::new();
        accepted.insert(encumbered.name.to_string());

        let decision = model_download_click_decision(
            encumbered_index,
            encumbered.min_ram_gb,
            &mut accepted,
            |_, _, _| panic!("already-accepted license must not re-prompt"),
        )
        .expect("catalog has an entry");

        match decision {
            ModelDownloadClickDecision::Ready {
                entry,
                accepted_license: None,
            } => assert_eq!(entry.name, encumbered.name),
            other => panic!("expected ready decision without new acceptance, got {other:?}"),
        }
        // Re-download of an already-licensed model leaves the set unchanged.
        assert!(accepted.contains(encumbered.name));
    }

    #[test]
    fn model_download_click_uses_selected_index_and_oob_falls_back_to_recommended() {
        let selected_index = model_catalog::catalog()
            .iter()
            .position(|entry| {
                entry.license == model_catalog::License::Apache2
                    && Some(entry.name) != model_catalog::recommended().map(|e| e.name)
            })
            .expect("catalog has a non-default unencumbered entry");
        let selected = &model_catalog::catalog()[selected_index];
        let mut accepted = std::collections::BTreeSet::new();

        let selected_decision = model_download_click_decision(
            selected_index,
            selected.min_ram_gb,
            &mut accepted,
            |_, _, _| panic!("unencumbered selected model must not prompt"),
        )
        .expect("catalog has an entry");
        match selected_decision {
            ModelDownloadClickDecision::Ready {
                entry,
                accepted_license: None,
            } => assert_eq!(entry.name, selected.name),
            other => panic!("expected selected entry to be ready, got {other:?}"),
        }

        let recommended = model_catalog::recommended().expect("catalog has a recommended model");
        let fallback_decision = model_download_click_decision(
            usize::MAX,
            recommended.min_ram_gb,
            &mut accepted,
            |_, _, _| panic!("recommended model must not prompt"),
        )
        .expect("fallback catalog entry");
        match fallback_decision {
            ModelDownloadClickDecision::Ready {
                entry,
                accepted_license: None,
            } => assert_eq!(entry.name, recommended.name),
            other => panic!("expected OOB fallback to recommended, got {other:?}"),
        }
    }

    #[test]
    fn validate_gguf_model_accepts_gguf_magic_and_rejects_the_rest() {
        let dir = std::env::temp_dir().join(format!("cm-byom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Wrong extension → rejected before any read.
        let txt = dir.join("model.bin");
        std::fs::write(&txt, b"GGUFxxxx").unwrap();
        assert!(
            validate_gguf_model(&txt).is_err(),
            "non-.gguf must be rejected"
        );

        // .gguf extension but wrong magic → rejected.
        let bad = dir.join("bad.gguf");
        std::fs::write(&bad, b"NOPEyyyy").unwrap();
        assert!(
            validate_gguf_model(&bad).is_err(),
            "bad magic must be rejected"
        );

        // Empty .gguf → rejected (read_exact of 4 bytes fails).
        let empty = dir.join("empty.gguf");
        std::fs::write(&empty, b"").unwrap();
        assert!(
            validate_gguf_model(&empty).is_err(),
            "empty must be rejected"
        );

        // Missing file → rejected.
        assert!(validate_gguf_model(&dir.join("nope.gguf")).is_err());

        // Real GGUF magic + uppercase extension → accepted.
        let good = dir.join("model.GGUF");
        std::fs::write(&good, b"GGUF\x03\x00\x00\x00rest").unwrap();
        assert!(
            validate_gguf_model(&good).is_ok(),
            "a GGUF-magic .gguf (case-insensitive ext) must be accepted"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_downloaded_model_picks_newest_nonempty_gguf() {
        use std::time::{Duration, SystemTime};
        let dir = std::env::temp_dir().join(format!("cm-discover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Empty dir → nothing to adopt.
        assert_eq!(discover_downloaded_model(&dir), None);

        // A non-gguf file and an empty gguf stub are both ignored.
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        std::fs::write(dir.join("partial.gguf"), b"").unwrap();
        assert_eq!(
            discover_downloaded_model(&dir),
            None,
            "non-gguf and empty stubs must be skipped"
        );

        // Two real models: the one with the newer mtime wins, regardless of name.
        let older = dir.join("qwen2.5-0.5b-q4_k_m.gguf");
        let newer = dir.join("gemma-2-2b-q4_k_m.gguf");
        std::fs::write(&older, b"aaaa").unwrap();
        std::fs::write(&newer, b"bbbb").unwrap();
        let base = SystemTime::now();
        set_mtime(&older, base - Duration::from_secs(60));
        set_mtime(&newer, base);
        assert_eq!(
            discover_downloaded_model(&dir).as_deref(),
            Some(newer.as_path()),
            "the most recently modified gguf must win"
        );

        // Touch the older one newer → it now wins (proves mtime, not name/order).
        set_mtime(&older, base + Duration::from_secs(60));
        assert_eq!(
            discover_downloaded_model(&dir).as_deref(),
            Some(older.as_path())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Set a file's mtime with stdlib only (File::set_modified, stable 1.75).
    fn set_mtime(path: &std::path::Path, when: std::time::SystemTime) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(when).unwrap();
    }

    #[test]
    fn model_download_status_line_surfaces_progress_and_outcome_but_stays_quiet_when_ready() {
        use model_fetch::{DownloadState, DownloadStatus};
        use std::sync::atomic::Ordering;

        let status = std::sync::Arc::new(DownloadStatus::default());
        let set = |state: DownloadState, done: u64, total: u64| {
            *status.state.lock().unwrap() = state;
            status.downloaded.store(done, Ordering::Relaxed);
            status.total.store(total, Ordering::Relaxed);
        };

        // Model already loaded → never nag, whatever the download state says.
        set(DownloadState::Failed("ignored".into()), 0, 0);
        assert_eq!(model_download_status_line(Some(&status), true), None);
        // No download and idle → nothing to show.
        assert_eq!(model_download_status_line(None, false), None);
        set(DownloadState::Idle, 0, 0);
        assert_eq!(model_download_status_line(Some(&status), false), None);

        // Running: percent when total known, byte count when unknown (0 total).
        set(
            DownloadState::Running,
            512 * 1024 * 1024,
            1024 * 1024 * 1024,
        );
        assert!(model_download_status_line(Some(&status), false)
            .unwrap()
            .contains("50%"));
        set(DownloadState::Running, 3 * 1024 * 1024, 0);
        assert!(model_download_status_line(Some(&status), false)
            .unwrap()
            .contains("3 MB"));

        // Terminal states are the whole point — the user must see them.
        set(DownloadState::Done("/tmp/m.gguf".into()), 0, 0);
        assert!(model_download_status_line(Some(&status), false)
            .unwrap()
            .contains("relaunch"));
        set(DownloadState::Failed("http error: status 404".into()), 0, 0);
        assert!(model_download_status_line(Some(&status), false)
            .unwrap()
            .contains("404"));
    }

    #[test]
    fn download_log_transitions_log_each_stage_exactly_once() {
        use model_fetch::DownloadState;
        // Running logs once, never repeats.
        let (logged, line) = download_log_transition(&DownloadState::Running, 0);
        assert_eq!(logged, 1);
        assert!(line.unwrap().contains("running"));
        assert_eq!(
            download_log_transition(&DownloadState::Running, 1),
            (1, None)
        );
        // Done logs where the model landed — the only user-visible signal —
        // even when it skipped Running.
        let done = DownloadState::Done("/tmp/m.gguf".into());
        let (logged, line) = download_log_transition(&done, 0);
        assert_eq!(logged, 2);
        assert!(line.unwrap().contains("/tmp/m.gguf"));
        assert_eq!(
            download_log_transition(&done, 2),
            (2, None),
            "terminal logs once"
        );
        // Failed is terminal too.
        let failed = DownloadState::Failed("boom".into());
        let (logged, line) = download_log_transition(&failed, 1);
        assert_eq!(logged, 2);
        assert!(line.unwrap().contains("boom"));
        assert_eq!(download_log_transition(&failed, 2), (2, None));
        // Idle never logs.
        assert_eq!(download_log_transition(&DownloadState::Idle, 0), (0, None));
    }

    #[test]
    fn download_log_transition_emits_path_hint_on_running_to_done() {
        // The normal sequence is Running (logged 0->1) then Done (logged 1->2).
        // The Done guard is `logged < 2`, so reaching Done with logged == 1 must
        // still emit the destination path — the only signal of where the model
        // landed. A mutant narrowing the guard to `logged == 0` would drop the
        // line on this real path, leaving the user with no destination.
        let done = model_fetch::DownloadState::Done("/tmp/m.gguf".into());
        let (logged, line) = download_log_transition(&done, 1);
        assert_eq!(logged, 2);
        assert!(line.unwrap().contains("/tmp/m.gguf"));
    }

    #[test]
    fn model_download_prepare_failure_does_not_spawn_or_enqueue() {
        let entry = model_catalog::recommended().expect("catalog has a recommended model");
        let dest = std::path::PathBuf::from("/tmp/compme-model.gguf");
        let mut downloader: Option<()> = None;
        let mut status = Some(std::sync::Arc::new(model_fetch::DownloadStatus::default()));
        let previous_status = status.as_ref().map(std::sync::Arc::as_ptr).unwrap();
        let mut logged = 7;
        let metadata_checked = std::cell::Cell::new(false);
        let spawned = std::cell::Cell::new(false);
        let requested = std::cell::Cell::new(false);

        let result = start_model_download_edge(ModelDownloadEdge {
            entry,
            dest: &dest,
            downloader: &mut downloader,
            model_download_status: &mut status,
            model_download_logged: &mut logged,
            prepare: |_: &std::path::Path| Err("no model directory".into()),
            existing_model: |_: &std::path::Path, _: Option<&str>| {
                metadata_checked.set(true);
                Ok(false)
            },
            spawn: || {
                spawned.set(true);
                Ok(())
            },
            request: |_: &(), _| {
                requested.set(true);
                true
            },
        });

        assert_eq!(
            result,
            DownloadStartResult::PreparedFailed("no model directory".into())
        );
        assert!(
            !metadata_checked.get(),
            "metadata must not run after prep fails"
        );
        assert!(!spawned.get(), "downloader must not spawn after prep fails");
        assert!(
            !requested.get(),
            "request must not enqueue after prep fails"
        );
        assert_eq!(
            status.as_ref().map(std::sync::Arc::as_ptr),
            Some(previous_status)
        );
        assert_eq!(logged, 7);
    }

    #[test]
    fn model_download_spawn_failure_does_not_enqueue_or_mark_running() {
        let entry = model_catalog::recommended().expect("catalog has a recommended model");
        let dest = std::path::PathBuf::from("/tmp/compme-model.gguf");
        let mut downloader: Option<()> = None;
        let mut status = None;
        let mut logged = 7;
        let requested = std::cell::Cell::new(false);

        let result = start_model_download_edge(ModelDownloadEdge {
            entry,
            dest: &dest,
            downloader: &mut downloader,
            model_download_status: &mut status,
            model_download_logged: &mut logged,
            prepare: |_: &std::path::Path| Ok(()),
            existing_model: |_: &std::path::Path, _: Option<&str>| Ok(false),
            spawn: || Err("thread unavailable".into()),
            request: |_: &(), _| {
                requested.set(true);
                true
            },
        });

        assert_eq!(
            result,
            DownloadStartResult::SpawnFailed("thread unavailable".into())
        );
        assert!(
            !requested.get(),
            "request must not enqueue without a downloader"
        );
        assert!(status.is_none(), "failed spawn must not set running status");
        assert_eq!(logged, 7);
    }

    #[test]
    fn replacement_decision_combines_gate_and_offer() {
        let config = Config::from_lookup(lookup(&[("COMPME_EMOJI", "1")]));
        let allowed = Some("com.apple.TextEdit");
        // Enabled (tray) + allowed app + a shortcode → offers.
        assert!(
            replacement_decision("hi :smile", &config, &config.prefs, allowed, None, true, 0)
                .is_some()
        );
        // Tray-disabled → no offer even with a match.
        assert!(
            replacement_decision("hi :smile", &config, &config.prefs, allowed, None, false, 0)
                .is_none()
        );
        // Sidebar-only / blocked app → no offer even when enabled.
        assert!(replacement_decision(
            "hi :smile",
            &config,
            &config.prefs,
            Some("com.microsoft.VSCode"),
            None,
            true,
            0
        )
        .is_none());
        // No matching token → no offer.
        assert!(replacement_decision(
            "hello world",
            &config,
            &config.prefs,
            allowed,
            None,
            true,
            0
        )
        .is_none());
    }

    #[test]
    fn numeric_knobs_parse_and_clamp() {
        let config = Config::from_lookup(lookup(&[
            ("COMPME_DEBOUNCE_MS", "60"),
            ("COMPME_MAX_WORDS", "999"),    // over max → clamps to 50
            ("COMPME_MAX_TOKENS", "0"),     // under min → clamps to 1
            ("COMPME_HEARTBEAT_MS", "500"), // over max → clamps to 100
        ]));
        assert_eq!(config.debounce_ms, 60);
        assert_eq!(config.max_words, 50);
        assert_eq!(config.max_tokens, 1);
        assert_eq!(config.heartbeat_ms, 100);
    }

    #[test]
    fn numeric_knobs_fall_back_to_defaults_when_unparseable() {
        let config = Config::from_lookup(lookup(&[
            ("COMPME_DEBOUNCE_MS", "fast"),
            ("COMPME_MAX_WORDS", "many"),
            ("COMPME_MAX_TOKENS", "lots"),
            ("COMPME_HEARTBEAT_MS", "soon"),
        ]));
        assert_eq!(config.debounce_ms, DEFAULT_DEBOUNCE_MS);
        assert_eq!(config.max_words, DEFAULT_MAX_WORDS);
        assert_eq!(config.max_tokens, DEFAULT_MAX_TOKENS);
        assert_eq!(config.heartbeat_ms, DEFAULT_HEARTBEAT_MS);
    }

    #[test]
    fn candidate_count_parses_and_clamps() {
        assert_eq!(
            Config::from_lookup(lookup(&[("COMPME_CANDIDATES", "3")])).candidates,
            3
        );
        assert_eq!(
            Config::from_lookup(lookup(&[("COMPME_CANDIDATES", "0")])).candidates,
            1
        );
        assert_eq!(
            Config::from_lookup(lookup(&[("COMPME_CANDIDATES", "99")])).candidates,
            5
        );
        assert_eq!(
            Config::from_lookup(lookup(&[("COMPME_CANDIDATES", "many")])).candidates,
            DEFAULT_CANDIDATES
        );
    }

    #[test]
    fn diag_coords_enabled_by_one_or_true() {
        assert!(Config::from_lookup(lookup(&[("COMPME_DIAG_COORDS", "1")])).diag_coords);
        assert!(Config::from_lookup(lookup(&[("COMPME_DIAG_COORDS", "true")])).diag_coords);
        assert!(!Config::from_lookup(lookup(&[("COMPME_DIAG_COORDS", "no")])).diag_coords);
    }

    #[test]
    fn valid_pid_and_run_ms_parse() {
        let config = Config::from_lookup(lookup(&[
            ("COMPME_ACCEPTANCE_PID", "8273"),
            ("COMPME_RUN_MS", "4000"),
        ]));
        assert_eq!(config.acceptance_pid, Some(8273));
        assert_eq!(config.run_ms, Some(4000));
    }

    #[test]
    fn unparseable_pid_and_run_ms_fall_back_to_none() {
        let config = Config::from_lookup(lookup(&[
            ("COMPME_ACCEPTANCE_PID", "not-a-number"),
            ("COMPME_RUN_MS", "later"),
        ]));
        assert_eq!(config.acceptance_pid, None);
        assert_eq!(config.run_ms, None);
    }

    #[test]
    fn empty_stub_completion_is_treated_as_unset() {
        let config = Config::from_lookup(lookup(&[("COMPME_STUB_COMPLETION", "")]));
        assert_eq!(config.stub_completion, None);
    }

    #[test]
    fn non_empty_stub_completion_is_kept() {
        let config = Config::from_lookup(lookup(&[("COMPME_STUB_COMPLETION", " jumps")]));
        assert_eq!(config.stub_completion.as_deref(), Some(" jumps"));
    }

    #[test]
    fn model_path_override_wins_over_default() {
        let config = Config::from_lookup(lookup(&[("COMPME_MODEL_PATH", "/models/x.gguf")]));
        assert_eq!(config.model_path, PathBuf::from("/models/x.gguf"));
    }

    #[test]
    fn prompt_mode_raw_is_parsed() {
        let config = Config::from_lookup(lookup(&[("COMPME_PROMPT_MODE", "raw")]));
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
        assert!(status_drops_pending_requests(AppStatus::Blocked(
            BlockReason::ModelUnavailable
        )));
    }

    #[test]
    fn manual_grammar_request_drops_under_loading_unlike_pending_completions() {
        // The one-shot GrammarCheck shortcut arms `manual_grammar_request`,
        // consumed only inside the `suggestions_allowed()` arm; every other
        // status takes the drop-with-log `else if` so the key press is never
        // silently discarded. The `suggestions_allowed` truth table alone is
        // owned by status.rs (`only_ready_allows_suggestions`) — asserting it
        // again here would be a duplicate. The behavior *this* branch encodes,
        // pinned nowhere else, is the DIVERGENCE at Loading: a manual grammar
        // request is dropped there (suggestions not allowed) even though a
        // pending *completion* request is preserved (`status_drops_pending_
        // requests` is false for Loading). If the two predicates were ever
        // realigned at Loading, the grammar drop branch would silently change
        // meaning; this guards that coupling.
        // Ready: grammar submitted, pending completions kept.
        assert!(AppStatus::Ready.suggestions_allowed());
        assert!(!status_drops_pending_requests(AppStatus::Ready));
        // Loading: grammar request dropped, pending completion preserved.
        assert!(!AppStatus::Loading.suggestions_allowed());
        assert!(!status_drops_pending_requests(AppStatus::Loading));
        // Hard-blocked / disabled: grammar request and pending completions both go.
        for status in [
            AppStatus::Disabled,
            AppStatus::Blocked(BlockReason::Permission),
            AppStatus::Blocked(BlockReason::RelaunchRequired),
            AppStatus::Blocked(BlockReason::SecureInput),
            AppStatus::Blocked(BlockReason::ModelUnavailable),
        ] {
            assert!(!status.suggestions_allowed(), "{status:?}");
            assert!(status_drops_pending_requests(status), "{status:?}");
        }
    }

    #[test]
    fn subscription_error_degrades_only_for_missing_accessibility_or_untrusted_startup() {
        assert_eq!(
            subscription_error_action(
                false,
                &PlatformError::CannotComplete {
                    reason: "AX down".into()
                }
            ),
            SubscriptionErrorAction::NoopUntilPermission
        );
        assert_eq!(
            subscription_error_action(
                true,
                &PlatformError::PermissionMissing {
                    permission: "Accessibility".into()
                }
            ),
            SubscriptionErrorAction::NoopUntilPermission
        );
        // Fatal must carry the underlying error context — run() interpolates
        // this message verbatim into the operator-facing startup failure, so a
        // blank/constant payload would silently strip the diagnostic.
        match subscription_error_action(
            true,
            &PlatformError::CannotComplete {
                reason: "AX down".into(),
            },
        ) {
            SubscriptionErrorAction::Fatal(m) => {
                assert!(m.contains("CannotComplete") && m.contains("AX down"), "{m}")
            }
            other => panic!("expected Fatal, got {other:?}"),
        }
        match subscription_error_action(true, &PlatformError::Timeout) {
            SubscriptionErrorAction::Fatal(m) => assert!(m.contains("Timeout"), "{m}"),
            other => panic!("expected Fatal, got {other:?}"),
        }
    }

    #[test]
    fn degraded_startup_subscriptions_keep_runtime_permission_blocked() {
        assert!(runtime_trusted(true, false));
        assert!(!runtime_trusted(false, false));
        assert!(!runtime_trusted(true, true));
        assert!(!runtime_trusted(false, true));
    }

    #[test]
    fn secure_input_subscription_error_is_fatal_when_trusted() {
        // A SecureInput error at subscription time is NOT the missing-permission
        // degrade path: when the app is already trusted it is a fatal startup
        // condition, and the Fatal payload must carry the variant so run()'s
        // operator-facing message names what failed. (Untrusted still degrades
        // to NoopUntilPermission, like every non-permission error.)
        let secure = PlatformError::SecureInput {
            state: SecurityState::SecureInputEnabled,
        };
        match subscription_error_action(true, &secure) {
            SubscriptionErrorAction::Fatal(m) => assert!(m.contains("SecureInput"), "{m}"),
            other => panic!("expected Fatal, got {other:?}"),
        }
        assert_eq!(
            subscription_error_action(false, &secure),
            SubscriptionErrorAction::NoopUntilPermission
        );
    }

    #[test]
    fn screen_recording_requested_only_when_context_on_and_permission_missing() {
        assert!(should_request_screen_recording(true, false));
        assert!(!should_request_screen_recording(true, true));
        assert!(!should_request_screen_recording(false, false));
        assert!(!should_request_screen_recording(false, true));
    }

    #[test]
    fn instance_lock_io_failure_fails_closed_before_startup_side_effects() {
        let side_effects = std::cell::Cell::new(0);
        let result: Result<Option<()>, String> = instance_lock_startup_gate(
            Some(std::path::PathBuf::from("/tmp/compme.lock")),
            |_| Err(config::InstanceLockError::Io("permission denied".into())),
            || side_effects.set(side_effects.get() + 1),
        );
        assert!(matches!(
            result,
            Err(message) if message.contains("permission denied")
        ));
        assert_eq!(
            side_effects.get(),
            0,
            "startup side effects must not run after lock IO failure"
        );
        assert!(matches!(
            instance_startup_decision(Some(config::InstanceLockError::Io("permission denied".into()))),
            InstanceStartupDecision::Fail(message) if message.contains("permission denied")
        ));
    }

    #[test]
    fn missing_instance_lock_path_fails_closed_before_startup_side_effects() {
        let side_effects = std::cell::Cell::new(0);
        let result: Result<Option<()>, String> = instance_lock_startup_gate(
            None::<std::path::PathBuf>,
            |_| Ok(()),
            || side_effects.set(side_effects.get() + 1),
        );
        assert!(matches!(
            result,
            Err(message) if message.contains("instance lock")
        ));
        assert_eq!(
            side_effects.get(),
            0,
            "startup side effects must not run without an instance lock path"
        );
        assert!(matches!(
            instance_startup_decision(None),
            InstanceStartupDecision::Fail(message) if message.contains("instance lock")
        ));
    }

    #[test]
    fn acquiring_the_instance_lock_runs_startup_side_effects_once_and_proceeds() {
        // The proceed path: a clean acquire returns Ok(Some(lock)) AND runs the
        // startup side effects exactly once (installing AX observers etc.). A
        // regression that swallowed a successful acquire, or ran the side effects
        // zero/twice, would slip past the fail-closed tests alone.
        let side_effects = std::cell::Cell::new(0);
        let result = instance_lock_startup_gate(
            Some(std::path::PathBuf::from("/tmp/compme.lock")),
            |_| Ok("held-lock"),
            || side_effects.set(side_effects.get() + 1),
        );
        assert!(matches!(result, Ok(Some("held-lock"))));
        assert_eq!(
            side_effects.get(),
            1,
            "startup side effects must run exactly once after a clean acquire"
        );
    }

    #[test]
    fn a_duplicate_instance_exits_gracefully_without_startup_side_effects() {
        // The graceful-duplicate path: `Held` maps to ExitOk, so the gate returns
        // Ok(None) (caller exits 0, not an error) and the startup side effects
        // never run — a second launch must not install observers or touch state.
        let side_effects = std::cell::Cell::new(0);
        let result: Result<Option<()>, String> = instance_lock_startup_gate(
            Some(std::path::PathBuf::from("/tmp/compme.lock")),
            |_| Err(config::InstanceLockError::Held),
            || side_effects.set(side_effects.get() + 1),
        );
        assert!(matches!(result, Ok(None)));
        assert_eq!(
            side_effects.get(),
            0,
            "a duplicate instance must not run startup side effects"
        );
        assert!(matches!(
            instance_startup_decision(Some(config::InstanceLockError::Held)),
            InstanceStartupDecision::ExitOk(message) if message.contains("already running")
        ));
    }

    #[test]
    fn model_download_dest_parent_failure_is_reported() {
        let file_parent = std::env::temp_dir().join(format!(
            "compme-download-parent-blocker-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&file_parent);
        std::fs::write(&file_parent, b"not a directory").unwrap();
        let dest = file_parent.join("model.gguf");
        let result = prepare_model_download_dest(&dest);
        let _ = std::fs::remove_file(&file_parent);
        assert!(
            result.is_err(),
            "download preparation must report parent creation failures"
        );
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
            domain: None,
            snapshot: generation,
            prompt: "p".into(),
            max_tokens: 8,
            kind: RequestKind::Completion,
        }
    }

    fn req_with_prompt(prompt: &str) -> CompletionRequest {
        CompletionRequest {
            prompt: prompt.into(),
            ..req(1)
        }
    }

    fn grammar_req_with_left_ctx(left_ctx: &str) -> CompletionRequest {
        CompletionRequest {
            prompt: String::new(),
            kind: RequestKind::GrammarFix {
                word: "teh".into(),
                left_ctx: left_ctx.into(),
                correction_range: CorrectionRange { start: 0, end: 3 },
            },
            ..req(1)
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
        // Newest-by-generation wins regardless of arrival order…
        assert_eq!(latest.take().unwrap().generation, 3);

        // …and offering an OLDER generation afterward must NOT re-populate the
        // slot (the `>=` guard in LatestRequest::offer): a late stale request
        // can never resurrect a request the loop already moved past.
        offer_all(&mut latest, vec![req(3)]);
        offer_all(&mut latest, vec![req(1)]);
        assert_eq!(
            latest.take().unwrap().generation,
            3,
            "an older generation must not overwrite the retained newest"
        );
    }

    #[test]
    fn grammar_check_shortcut_blocked_after_read_clears_pending_completion() {
        let mut latest = LatestRequest::new();
        latest.offer(req(9));
        let mut manual = Some(req(1));

        apply_grammar_shortcut_pending_effect(
            &mut latest,
            &mut manual,
            &GrammarCheckShortcutOutcome::BlockedAfterRead,
        );

        assert!(latest.take().is_none());
        assert!(manual.is_none());
    }

    #[test]
    fn grammar_check_shortcut_not_armed_clears_pending_completion() {
        let mut latest = LatestRequest::new();
        latest.offer(req(9));
        let mut manual = None;

        apply_grammar_shortcut_pending_effect(
            &mut latest,
            &mut manual,
            &GrammarCheckShortcutOutcome::NotArmed,
        );

        assert!(latest.take().is_none());
        assert!(manual.is_none());
    }

    #[test]
    fn grammar_check_shortcut_later_failed_press_drops_stale_manual_request() {
        let mut latest = LatestRequest::new();
        let mut manual = None;

        apply_grammar_shortcut_pending_effect(
            &mut latest,
            &mut manual,
            &GrammarCheckShortcutOutcome::Armed(req(4)),
        );
        apply_grammar_shortcut_pending_effect(
            &mut latest,
            &mut manual,
            &GrammarCheckShortcutOutcome::BlockedAfterRead,
        );

        assert!(latest.take().is_none());
        assert!(manual.is_none());

        apply_grammar_shortcut_pending_effect(
            &mut latest,
            &mut manual,
            &GrammarCheckShortcutOutcome::Armed(req(5)),
        );
        apply_grammar_shortcut_pending_effect(
            &mut latest,
            &mut manual,
            &GrammarCheckShortcutOutcome::NotArmed,
        );

        assert!(latest.take().is_none());
        assert!(manual.is_none());
    }

    #[test]
    fn grammar_check_shortcut_non_armed_error_drops_stale_manual_without_completion_clear() {
        let mut latest = LatestRequest::new();
        latest.offer(req(9));
        let mut manual = Some(req(4));

        apply_grammar_shortcut_pending_effect(
            &mut latest,
            &mut manual,
            &GrammarCheckShortcutOutcome::ReadContextError(PlatformError::Timeout),
        );

        assert_eq!(latest.take().unwrap().generation, 9);
        assert!(manual.is_none());
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
    fn toggle_app_dismisses_only_when_app_was_enabled() {
        // ToggleApp disables exactly when the app was enabled pre-toggle, so the
        // dismiss seam fires on that branch only. Both arms pinned so inverting
        // the production guard is caught (the per-app retraction has no global
        // edge for should_dismiss_on_disable to cover).
        assert!(toggle_app_dismisses(true)); // was enabled -> toggle disables -> dismiss
        assert!(!toggle_app_dismisses(false)); // was disabled -> toggle enables -> keep
    }

    #[test]
    fn app_enabled_baseline_reads_override_then_default() {
        // The value ToggleApp inverts: per-app `enabled` override wins; absent an
        // override it falls back to `default_enabled` (NOT should_suggest, so
        // snooze/exclude don't enter here).
        let mut prefs = Prefs {
            default_enabled: true,
            ..Default::default()
        };
        // No override -> default.
        assert!(app_enabled_baseline(&prefs, "com.none"));
        prefs.default_enabled = false;
        assert!(!app_enabled_baseline(&prefs, "com.none"));
        // Override beats default in both directions.
        prefs.per_app.entry("com.on".into()).or_default().enabled = Some(true);
        assert!(app_enabled_baseline(&prefs, "com.on")); // override true vs default false
        prefs.default_enabled = true;
        prefs.per_app.entry("com.off".into()).or_default().enabled = Some(false);
        assert!(!app_enabled_baseline(&prefs, "com.off")); // override false vs default true
    }

    #[test]
    fn toggle_app_inverts_override_and_converges_across_baselines() {
        // review finding E + r1/r2: ToggleApp inverts the PER-APP enabled override
        // (driven by app_enabled_baseline), writing the inverse as an explicit
        // override. It must CONVERGE — one toggle flips the live state, a second
        // restores it — from every starting baseline (None/default true, None/
        // default false, Some(true), Some(false)). The toggle dispatch reads the
        // baseline then writes `set_app_policy_field(Enabled, !baseline)`; this
        // mirrors that core through the same two public helpers.
        let app = "com.toggle.app";
        let one_toggle = |prefs: &mut Prefs| {
            let next = !app_enabled_baseline(prefs, app);
            prefs.set_app_policy_field(app, prefs::AppPolicyField::Enabled, next);
            next
        };
        for (default_enabled, seed) in [
            (true, None),
            (false, None),
            (true, Some(true)),
            (false, Some(false)),
            (true, Some(false)),
            (false, Some(true)),
        ] {
            let mut prefs = Prefs {
                default_enabled,
                ..Default::default()
            };
            if let Some(v) = seed {
                prefs.per_app.entry(app.into()).or_default().enabled = Some(v);
            }
            let start = app_enabled_baseline(&prefs, app);
            // One toggle flips the effective enabled state and pins an override.
            let after_first = one_toggle(&mut prefs);
            assert_eq!(
                after_first, !start,
                "first toggle must flip (seed {seed:?})"
            );
            assert_eq!(prefs.per_app[app].enabled, Some(!start));
            assert_eq!(app_enabled_baseline(&prefs, app), !start);
            // Second toggle converges back — no drift, regardless of baseline.
            let after_second = one_toggle(&mut prefs);
            assert_eq!(
                after_second, start,
                "second toggle must converge (seed {seed:?})"
            );
            assert_eq!(app_enabled_baseline(&prefs, app), start);
        }
    }

    #[test]
    fn apps_edit_dismisses_only_focused_app_on_enable_off_edge() {
        use prefs::AppPolicyField::*;
        // Gap 3: editing the FOCUSED app's Enabled->off dismisses; editing a
        // DIFFERENT app's row does not; and only the Enabled->off edge fires.
        // Focused app == "com.a".
        assert!(apps_edit_dismisses_focused(
            Enabled,
            false,
            Some("com.a"),
            "com.a"
        ));
        // Different app edited while focused on com.a -> no dismiss.
        assert!(!apps_edit_dismisses_focused(
            Enabled,
            false,
            Some("com.a"),
            "com.b"
        ));
        // Enabling (on=true) the focused app does not dismiss.
        assert!(!apps_edit_dismisses_focused(
            Enabled,
            true,
            Some("com.a"),
            "com.a"
        ));
        // Disabling GrammarFix for the focused app also dismisses, because an
        // already visible correction would otherwise remain acceptable.
        assert!(apps_edit_dismisses_focused(
            GrammarFix,
            false,
            Some("com.a"),
            "com.a"
        ));
        // Feature-off edges that can stale an existing visible suggestion also dismiss.
        assert!(!apps_edit_dismisses_focused(
            TabDisabled,
            false,
            Some("com.a"),
            "com.a"
        ));
        // Enabling Tab suppression for the focused app must also retract the
        // visible suggestion, otherwise the already armed bare-Tab binding can
        // still accept it until the next focus/show cycle.
        assert!(apps_edit_dismisses_focused(
            TabDisabled,
            true,
            Some("com.a"),
            "com.a"
        ));
        assert!(apps_edit_dismisses_focused(
            MidLine,
            false,
            Some("com.a"),
            "com.a"
        ));
        assert!(apps_edit_dismisses_focused(
            Autocorrect,
            false,
            Some("com.a"),
            "com.a"
        ));
        // No focused app at all -> nothing to dismiss.
        assert!(!apps_edit_dismisses_focused(Enabled, false, None, "com.a"));
    }

    #[test]
    fn toggle_app_dismisses_iff_focused_app_was_enabled_before_toggle() {
        // The ToggleApp shortcut flips a PER-APP override and must retract an
        // on-screen ghost ONLY when the toggle DISABLES the focused app. Unlike
        // ToggleGlobal/SIGUSR1, it never touches the global `enabled` atomic, so
        // the tick reconciliation (should_dismiss_on_disable over the global
        // edge) can NOT cover it — the production arm's
        // `if toggle_app_dismisses(current) { latest.clear(); on_dismiss() }`
        // seam is the only retraction, with `current =
        // app_enabled_baseline(&prefs, app)` read BEFORE the override write.
        // Round 1's convergence test pinned the override write but never this
        // dismiss guard; inverting the seam (leave a ghost on disable, dismiss
        // on enable) passes every round-1 test. This drives the dispatch core
        // through the same three production helpers.
        let app = "com.toggle.dismiss";
        let toggle_decides_dismiss = |prefs: &mut Prefs| -> bool {
            let current = app_enabled_baseline(prefs, app);
            prefs.set_app_policy_field(app, prefs::AppPolicyField::Enabled, !current);
            toggle_app_dismisses(current) // the run loop's dismiss guard
        };
        for (default_enabled, seed) in [
            (true, None),
            (false, None),
            (true, Some(true)),
            (false, Some(false)),
            (true, Some(false)),
            (false, Some(true)),
        ] {
            let mut prefs = Prefs {
                default_enabled,
                ..Default::default()
            };
            if let Some(v) = seed {
                prefs.per_app.entry(app.into()).or_default().enabled = Some(v);
            }
            let was_enabled = app_enabled_baseline(&prefs, app);
            // Dismiss fires iff the app was enabled before (the toggle disables
            // it); when it was already disabled, the toggle re-enables and there
            // is nothing on screen to retract.
            assert_eq!(
                toggle_decides_dismiss(&mut prefs),
                was_enabled,
                "dismiss decision must equal pre-toggle enabled (seed {seed:?})"
            );
            // And the toggle still flipped the live state (guards against a
            // mutation that returns the right dismiss bool but skips the write).
            assert_eq!(app_enabled_baseline(&prefs, app), !was_enabled);
        }
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
            HostEvent::Shortcut(ShortcutAction::GrammarCheck),
            HostEvent::Accept(AcceptAction::Correction),
        ];
        assert_eq!(coalesce_caret_reads(events.clone()), events);
    }

    #[test]
    fn focus_caret_accept_and_dismiss_clear_pending_requests() {
        assert!(host_event_invalidates_pending_request(&HostEvent::Focus(
            host_field("a")
        )));
        assert!(host_event_invalidates_pending_request(&HostEvent::Caret(
            host_field("a"),
            None
        )));
        assert!(host_event_invalidates_pending_request(&HostEvent::Accept(
            AcceptAction::Full
        )));
        assert!(host_event_invalidates_pending_request(&HostEvent::Accept(
            AcceptAction::Correction
        )));
        assert!(host_event_invalidates_pending_request(&HostEvent::Dismiss));
        assert!(!host_event_invalidates_pending_request(&HostEvent::Cycle));
        assert!(!host_event_invalidates_pending_request(
            &HostEvent::Shortcut(ShortcutAction::GrammarCheck)
        ));
    }

    #[test]
    fn host_event_queue_drops_old_caret_to_preserve_control_event() {
        let mut queue = VecDeque::new();
        for i in 0..MAX_HOST_EVENT_QUEUE {
            assert!(enqueue_host_event(
                &mut queue,
                HostEvent::Caret(host_field(&format!("field-{i}")), rect(i as f64))
            ));
        }

        assert!(enqueue_host_event(
            &mut queue,
            HostEvent::Accept(AcceptAction::Full)
        ));

        assert_eq!(queue.len(), MAX_HOST_EVENT_QUEUE);
        assert!(queue
            .iter()
            .any(|event| matches!(event, HostEvent::Accept(AcceptAction::Full))));
        assert!(!queue.iter().any(
            |event| matches!(event, HostEvent::Caret(field, _) if field.element_id == "field-0")
        ));
    }

    #[test]
    fn host_event_queue_drops_old_focus_to_preserve_control_event() {
        // Focus events are backpressure-droppable too (a superseded focus is as
        // stale as a superseded caret). A full queue of Focus events must yield
        // the oldest one to admit a control event, not refuse it.
        let mut queue = VecDeque::new();
        for i in 0..MAX_HOST_EVENT_QUEUE {
            assert!(enqueue_host_event(
                &mut queue,
                HostEvent::Focus(host_field(&format!("field-{i}")))
            ));
        }

        assert!(enqueue_host_event(
            &mut queue,
            HostEvent::Accept(AcceptAction::Full)
        ));

        assert_eq!(queue.len(), MAX_HOST_EVENT_QUEUE);
        assert!(queue
            .iter()
            .any(|event| matches!(event, HostEvent::Accept(AcceptAction::Full))));
        assert!(!queue.iter().any(
            |event| matches!(event, HostEvent::Focus(field) if field.element_id == "field-0")
        ));
    }

    #[test]
    fn host_event_queue_refuses_when_only_control_events_remain() {
        let mut queue = VecDeque::new();
        for _ in 0..MAX_HOST_EVENT_QUEUE {
            assert!(enqueue_host_event(
                &mut queue,
                HostEvent::Accept(AcceptAction::Full)
            ));
        }

        assert!(!enqueue_host_event(&mut queue, HostEvent::Dismiss));
        assert_eq!(queue.len(), MAX_HOST_EVENT_QUEUE);
    }

    #[test]
    fn host_event_drain_reports_backlog() {
        let queue = Mutex::new(VecDeque::new());
        for i in 0..(MAX_HOST_EVENTS_PER_TICK + 1) {
            assert!(push_host_event(
                &queue,
                HostEvent::Caret(host_field(&format!("field-{i}")), rect(i as f64))
            ));
        }

        let drained = drain_host_events(&queue);

        assert_eq!(drained.events.len(), MAX_HOST_EVENTS_PER_TICK);
        assert!(drained.backlog_remaining);
    }

    #[test]
    fn grammar_check_shortcut_routes_to_detection() {
        assert_eq!(
            host_event_route(&HostEvent::Shortcut(ShortcutAction::GrammarCheck)),
            HostEventRoute::ManualGrammarDetection
        );
        assert_eq!(
            host_event_route(&HostEvent::Shortcut(ShortcutAction::ForceActivate)),
            HostEventRoute::Normal
        );
    }

    #[test]
    fn grammar_accept_action_routes_to_accept_correction_not_full() {
        assert_eq!(
            host_event_route(&HostEvent::Accept(AcceptAction::Correction)),
            HostEventRoute::AcceptCorrection
        );
        assert_eq!(
            host_event_route(&HostEvent::Accept(AcceptAction::Full)),
            HostEventRoute::Normal
        );
        assert_eq!(
            host_event_route(&HostEvent::Accept(AcceptAction::Word)),
            HostEventRoute::Normal
        );
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
