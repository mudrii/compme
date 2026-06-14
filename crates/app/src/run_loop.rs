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

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};
use emoji::{EmojiPrefs, Gender, SkinTone};
use engine::{CompletionRequest, Engine, TriggerPolicy};
use personalization::{PersonalizationProfile, SenderIdentity, Strength};
use platform::{
    AcceptAction, Capabilities, FieldHandle, InsertStrategy, KeyInterceptMode, OverlayPlacement,
    PlatformAdapter, PlatformError, ScreenRect, SecurityState, TapControl, Toolkit,
};
use platform_macos::DisableArm;
use platform_macos::{
    accessibility_trusted, bundle_id_for_pid, display_scales, prompt_accessibility_trust,
    read_pasteboard_text, request_screen_recording_permission, screen_recording_permission,
    secure_input_enabled, MacosOverlayPresenter, MacosPlatformAdapter, MacosTray, TrayFlags,
};
use prefs::Prefs;

use crate::adapter::SharedAdapter;
use crate::config::{self, parse_clamped};
use crate::inference::{InferenceHandle, PreviousInputs, WorkerContext};
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
/// Per-source character bound when previous-input context is enabled truthily.
const DEFAULT_CONTEXT_MAX_CHARS: usize = 160;
const DEFAULT_MODEL: &str = "tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf";
/// Re-poll secure input + Accessibility trust at most this often (wall-clock ms).
const SECURE_POLL_INTERVAL_MS: u64 = 480;
/// Periodic lifetime-stats flush cadence (c102 follow-up): bounds crash loss
/// to ≤5 minutes of events; the file is ~120 bytes so the write is free.
const STATS_FLUSH_INTERVAL_MS: u64 = 5 * 60 * 1000;
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

fn host_event_invalidates_pending_request(event: &HostEvent) -> bool {
    matches!(
        event,
        HostEvent::Focus(_) | HostEvent::Caret(_, _) | HostEvent::Accept(_) | HostEvent::Dismiss
    )
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
    personalization: PersonalizationProfile,
    prefs: Prefs,
    memory: MemoryConfig,
    /// Emoji completion (A2 §8/§16). `Some` = enabled with the user's skin-tone/
    /// gender prefs; `None` = off (default). Drives the local `:shortcode`
    /// replacement offer in the observe path.
    emoji: Option<EmojiPrefs>,
    /// Inline typo autocorrect (A2 §8/§16, `COMPME_AUTOCORRECT`, default off):
    /// offer the correction when the trailing word is a known typo.
    autocorrect: bool,
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
    /// Rebound accept keys (raw macOS virtual keycodes,
    /// `COMPME_ACCEPT_WORD_KEY` / `COMPME_ACCEPT_FULL_KEY`). `None` →
    /// defaults (Tab 48 / grave 50). Collisions fail soft to defaults at
    /// startup with a logged error.
    accept_word_key: Option<i64>,
    accept_full_key: Option<i64>,
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
            // Global on/off (the tray-toggle state, persisted on toggle).
            // Distinct from COMPME_DEFAULT_ENABLED, the per-app
            // suggestion-policy default in prefs.
            enabled: parse_enabled_default(lookup("COMPME_ENABLED")),
            launch_at_login: parse_tri_state(lookup("COMPME_LAUNCH_AT_LOGIN")),
            trusted_key: lookup("COMPME_TRUSTED_KEY")
                .and_then(|raw| webconfig::TrustedKey::from_hex(&raw)),
            license_accepted: parse_license_accepted(lookup("COMPME_LICENSE_ACCEPTED")),
            accept_word_key: lookup("COMPME_ACCEPT_WORD_KEY")
                .and_then(|raw| raw.trim().parse::<i64>().ok()),
            accept_full_key: lookup("COMPME_ACCEPT_FULL_KEY")
                .and_then(|raw| raw.trim().parse::<i64>().ok()),
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
            personalization: build_personalization(&lookup),
            prefs: build_prefs(&lookup),
            memory: build_memory_config(&lookup),
            emoji: build_emoji_config(&lookup),
            autocorrect: lookup("COMPME_AUTOCORRECT")
                .is_some_and(|v| v == "1" || v == "true" || v == "on"),
            british_english: lookup("COMPME_BRITISH_ENGLISH")
                .is_some_and(|v| v == "1" || v == "true" || v == "on"),
            thesaurus: lookup("COMPME_THESAURUS")
                .is_some_and(|v| v == "1" || v == "true" || v == "on"),
        }
    }
}

/// Parse emoji-completion config (A2 §8/§16). `Some(prefs)` when
/// `COMPME_EMOJI` is on (opt-in, default off → `None` = disabled);
/// `COMPME_EMOJI_SKIN_TONE` (default/light/medium-light/medium/medium-dark/
/// dark) and `COMPME_EMOJI_GENDER` (neutral/female/male) select modifiers.
fn build_emoji_config(lookup: &impl Fn(&str) -> Option<String>) -> Option<EmojiPrefs> {
    let enabled = lookup("COMPME_EMOJI").is_some_and(|v| v == "1" || v == "true" || v == "on");
    if !enabled {
        return None;
    }
    Some(EmojiPrefs {
        skin_tone: parse_skin_tone(lookup("COMPME_EMOJI_SKIN_TONE")),
        gender: parse_gender(lookup("COMPME_EMOJI_GENDER")),
    })
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

/// A local emoji *replacement* for the typed left-context, when emoji completion
/// is enabled: `Some((glyph, replace_chars))` to offer, else `None`. Pure wrapper
/// over `emoji::suggest` behind the enable flag so the run-loop wiring is testable.
/// True when `COMPME_DEBUG` is set — gates verbose run-loop diagnostics
/// (replacement decision, etc.). Off by default → zero production output.
fn debug_enabled() -> bool {
    std::env::var_os("COMPME_DEBUG").is_some()
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

/// Whether completion inference has a usable model source: a stub completion
/// (acceptance harness) counts as ready — a stub-driven run must not show
/// "✗ Model file" in Setup. Single source for the startup diagnostic AND the
/// Setup pane (duplicated inline before, which invited divergence).
fn model_ready(config: &Config) -> bool {
    config.stub_completion.is_some() || config.model_path.exists()
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

/// Parse `COMPME_LICENSE_ACCEPTED` (comma-joined model names) into a set.
/// Trims and drops empties so hand-edited values normalize on the next
/// persist; BTreeSet keeps the serialized form deterministic.
fn parse_license_accepted(raw: Option<String>) -> std::collections::BTreeSet<String> {
    raw.map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
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
/// SHA-256 (when present) into model_fetch's verify-before-rename. The
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
        status,
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
fn apply_live_accept_keymap(
    word: Option<i64>,
    full: Option<i64>,
    set_map: impl Fn(Option<i64>, Option<i64>) -> Result<(), platform_macos::KeymapError>,
    rearm: impl Fn() -> Result<(), PlatformError>,
    persist: impl Fn(i64, i64),
    effective: impl Fn() -> (i64, i64),
) -> Result<(), String> {
    let previous = effective();
    set_map(word, full).map_err(|err| format!("rejected keymap: {err:?}"))?;
    if let Err(err) = rearm() {
        // Best-effort revert. The previous pair was validated when it
        // registered, so this set_map cannot fail in practice; if it ever
        // did, the map would claim the NEW keys while the OLD stay armed —
        // the c123 desync — hence revert-then-error, never error-then-leave.
        // The revert is the LAST line of defense against that desync, so a
        // failure here must not be SILENT: nothing else would surface that
        // the keymap and the registered hotkeys now disagree.
        if let Err(revert_err) = set_map(Some(previous.0), Some(previous.1)) {
            eprintln!(
                "compme: accept-keymap re-arm failed and revert to {previous:?} also failed: {revert_err:?}"
            );
        }
        return Err(format!("re-arm failed: {err:?}"));
    }
    let registered = effective();
    persist(registered.0, registered.1);
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
                "compme: model downloaded \u{2014} set COMPME_MODEL_PATH={} to use it",
                path.display()
            )),
        ),
        model_fetch::DownloadState::Failed(err) if logged < 2 => {
            (2, Some(format!("compme: model download failed: {err}")))
        }
        _ => (logged, None),
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
    let Some(key) = config.key.or_else(&keychain_key) else {
        eprintln!(
            "compme: COMPME_MEMORY set but no key available (no \
             COMPME_MEMORY_KEY and the keychain provided none) — memory disabled"
        );
        return None;
    };
    match MemoryStore::open(path, &StaticKey(key), config.mode) {
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

/// Build the personalization profile from config (A2 §6). Per-app/per-domain
/// instruction maps are an A3 settings concern; A2 wires the global instructions,
/// strength stop, and sender identity, which are enough to steer completions.
fn build_personalization(lookup: &impl Fn(&str) -> Option<String>) -> PersonalizationProfile {
    let mut profile = PersonalizationProfile {
        global_instructions: lookup("COMPME_INSTRUCTIONS").unwrap_or_default(),
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
/// submit gate and the local replacement-offer gate so both honor the same
/// per-app/snooze/terminal policy.
/// `domain` is the focused browser page's HOST when known (the Focus arm's
/// AX read via `domain_cache_entry`); `None` = no browser frontmost or no
/// URL resolved — fail-open.
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
    suggestion_gates_pass(app_key, &request.prompt, domain, prefs, now_ms)
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

fn canonicalize_field_app(
    mut field: FieldHandle,
    resolver: impl Fn(i32) -> Option<String>,
) -> (FieldHandle, Option<String>) {
    let app_key = resolve_app_key(field.pid, resolver);
    if let Some(app) = &app_key {
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
        DisableArm::Hour => prefs.snooze_app(app, now_ms, 60),
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
fn app_enabled_value(prefs: &Prefs, enabled: bool) -> String {
    sorted_join(
        prefs
            .per_app
            .iter()
            .filter(|(_, policy)| policy.enabled == Some(enabled))
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
            app_enabled_value(prefs, true),
            "enabled apps",
        ),
        (
            "COMPME_DISABLED_APPS",
            app_enabled_value(prefs, false),
            "disabled apps",
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
    std::fs::create_dir_all(path.parent().unwrap_or(path))?;
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
        .take(platform_macos::APPS_ROWS)
        .map(|(app, n)| format!("{app} \u{2014} {n}"))
        .collect()
}

use platform_macos::keycode_label;

/// The Shortcuts tab's text (persist-only slice): the EFFECTIVE bindings
/// (post-validation, from the platform's registered keymap — review-c114:
/// rendering raw config would lie when a colliding pair was rejected and
/// the runtime fell back to defaults), the fixed non-rebindable keys, and
/// how to change them. Static per process — bindings are read at launch
/// until the live-rebind refactor lands.
fn shortcuts_text(word_key: i64, full_key: i64) -> String {
    format!(
        "Accept word: {}\nAccept full: {}\nDismiss: Esc\nCycle candidates: Down arrow\n\n\
         To change: set COMPME_ACCEPT_WORD_KEY / COMPME_ACCEPT_FULL_KEY (macOS \
         keycodes) in config.env \u{2014} applies at relaunch (the in-app \
         recorder applies live).",
        keycode_label(word_key),
        keycode_label(full_key),
    )
}

/// The app ids behind the Apps-tab rows, in render order with the render
/// cap — index `i` here IS row `i` of `apps_pane_lines`, the contract the
/// per-row Delete buttons rely on.
fn apps_row_ids(counts: &[(String, u64)]) -> Vec<String> {
    counts
        .iter()
        .take(platform_macos::APPS_ROWS)
        .map(|(app, _)| app.clone())
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
) -> platform_macos::SettingsFlags {
    platform_macos::SettingsFlags {
        general_enabled: tray_enabled,
        labs_midline: Arc::new(AtomicBool::new(config.allow_mid_word)),
        general_autocorrect: Arc::new(AtomicBool::new(config.autocorrect)),
        general_trailing_space: Arc::new(AtomicBool::new(config.trailing_space)),
        stats_lines: Arc::new(Mutex::new(Vec::new())),
        about_text: crate::about::about_text(),
        setup_lines: Arc::new(Mutex::new(Vec::new())),
        setup_grant_ax: Arc::new(AtomicBool::new(false)),
        setup_request_screen: Arc::new(AtomicBool::new(false)),
        setup_reveal_model: Arc::new(AtomicBool::new(false)),
        setup_download_model: Arc::new(AtomicBool::new(false)),
        apps_lines: Arc::new(Mutex::new(Vec::new())),
        apps_delete_row: Arc::new(Mutex::new(None)),
        shortcuts_text: {
            let (word, full) = platform_macos::effective_accept_keys();
            Arc::new(Mutex::new(shortcuts_text(word, full)))
        },
        shortcuts_rebind_request: Arc::new(Mutex::new(None)),
    }
}

/// The Setup tab's current rows as display lines: probe permissions and the
/// model file NOW (cheap queries) and render through `setup_row_line`.
fn compose_setup_lines(config: &Config) -> Vec<String> {
    crate::setup_state::setup_rows(crate::setup_state::SetupChecks {
        // Probed fresh here (cheap), not the loop's 480ms-stale copy —
        // review-c107: rows must not flip at different cadences.
        ax_trusted: accessibility_trusted(),
        screen_context_enabled: config.screen_context,
        screen_recording: screen_recording_permission(),
        model_exists: model_ready(config),
    })
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
const SWITCH_KEYS: [&str; 12] = [
    "COMPME_ENABLED",
    "COMPME_MIDLINE",
    "COMPME_AUTOCORRECT",
    "COMPME_TRAILING_SPACE",
    "COMPME_NO_COLLECT_APPS",
    "COMPME_EXCLUDED_APPS",
    "COMPME_EXCLUDED_DOMAINS",
    "COMPME_ENABLED_APPS",
    "COMPME_DISABLED_APPS",
    // License acceptances persist on the prompt's Accept; an env shadow
    // resurrects the un-accepted state at relaunch → surprise re-prompt
    // (fail-closed, but confusing without the warning) (review-c127).
    "COMPME_LICENSE_ACCEPTED",
    // Accept-key rebinds persist after a successful live re-arm (recorder
    // 5b); an env shadow resurrects the OLD keys at relaunch while the
    // Shortcuts pane read the file — the exact desync the warning names.
    "COMPME_ACCEPT_WORD_KEY",
    "COMPME_ACCEPT_FULL_KEY",
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

/// How long a tray "Snooze" pauses suggestions. One fixed duration for now
/// (Cotypist-style pause); a duration submenu is a future settings surface.
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
    for app in comma_list(lookup("COMPME_THESAURUS_ON_APPS")) {
        prefs.per_app.entry(app).or_default().thesaurus = Some(true);
    }
    for app in comma_list(lookup("COMPME_THESAURUS_OFF_APPS")) {
        prefs.per_app.entry(app).or_default().thesaurus = Some(false);
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
    // Single-instance guard FIRST — before any AX observer, hotkey
    // registration, or Apple Events handler exists. Two instances double all
    // of those (live c92 finding: open(1) launches a second copy via Launch
    // Services when the registered handler isn't already running). flock is
    // launch-method-agnostic and kernel-released on any exit.
    let _instance_lock = match config::instance_lock_path() {
        Some(path) => match config::try_acquire_instance_lock(&path) {
            Ok(lock) => Some(lock),
            Err(config::InstanceLockError::Held) => {
                eprintln!("compme: another instance is already running — exiting");
                return Ok(());
            }
            Err(config::InstanceLockError::Io(err)) => {
                // An IO failure is NOT "another instance" — say so, and keep
                // running unguarded rather than refusing to start.
                eprintln!("compme: instance lock unavailable ({err}) — continuing unguarded");
                None
            }
        },
        None => {
            eprintln!("compme: no config dir for the instance lock — continuing unguarded");
            None
        }
    };

    // Mutable: General-tab switches update globals live (autocorrect today;
    // enabled/trailing-space later) — field writes between heartbeats only.
    let mut config = Config::from_env();
    install_signal_handlers();

    // Permissions: if Accessibility isn't granted, fire the system prompt once.
    // The app keeps running and reflects the Blocked state in the tray. Trust is
    // re-polled in the loop so granting it mid-session clears Blocked without a
    // restart.
    let mut trusted = accessibility_trusted();
    if !trusted {
        eprintln!("compme: Accessibility not granted — requesting permission");
        prompt_accessibility_trust();
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
    for warning in env_shadow_warnings(|key| env::var(key).is_ok()) {
        eprintln!("compme: {warning}");
    }

    // Setup status (the Setup pane's row model doubles as the startup
    // diagnostic): one line per not-ready item, so a log alone explains why
    // ghosts won't appear (missing permission, missing model file).
    for row in crate::setup_state::setup_rows(crate::setup_state::SetupChecks {
        ax_trusted: trusted,
        screen_context_enabled: config.screen_context,
        screen_recording: screen_recording_permission(),
        model_exists: model_ready(&config),
    }) {
        if !row.ready {
            eprintln!("compme: setup: {} not ready", row.label);
        }
    }

    if config.diag_coords {
        eprintln!("compme: diag display_scales={:?}", display_scales());
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
    .with_trigger_gates(config.min_context_chars, config.allow_mid_word)
    .with_trailing_space(config.trailing_space);

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
        eprintln!("compme: requesting Screen Recording permission (screen context)");
        request_screen_recording_permission();
        // The grant takes effect on the NEXT launch (macOS shows the prompt async
        // and re-reads TCC at startup), so screen context is inactive this run.
        eprintln!("compme: restart after granting Screen Recording to enable screen context");
    }

    // Rebound accept keys (cycle-13 residual): set the process-wide keymap
    // BEFORE the platform adapter exists, so the Carbon registration, the
    // decision logic, and the handler's id→keycode inverse all read one
    // source from the first arm. Collision/invalid → fail soft to defaults.
    if config.accept_word_key.is_some() || config.accept_full_key.is_some() {
        match platform_macos::set_accept_keymap_from_config(
            config.accept_word_key,
            config.accept_full_key,
        ) {
            Ok(()) => eprintln!(
                "compme: accept keys rebound (word={:?} full={:?})",
                config.accept_word_key, config.accept_full_key
            ),
            Err(err) => {
                eprintln!("compme: accept-key rebind invalid ({err:?}); using defaults")
            }
        }
    }

    // compme:// deep-link reception (web-driven config §8/§16): Launch
    // Services routes scheme opens as Apple Events; the handler enqueues the
    // raw URL and the heartbeat drains it through the strict fail-closed
    // parser. Install failure is non-fatal (deep links just stay inert).
    let deep_links: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let deep_links_in_handler = Arc::clone(&deep_links);
    let _url_handler = match platform_macos::install_url_event_handler(Arc::new(move |url| {
        deep_links_in_handler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(url);
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
        match platform_macos::set_launch_at_login(enabled) {
            Ok(()) => eprintln!(
                "compme: launch at login {}",
                if enabled { "ON" } else { "OFF" }
            ),
            Err(err) => eprintln!("compme: launch-at-login unavailable: {err}"),
        }
    }

    let previous_inputs = PreviousInputs::default();
    // Encrypted on-disk memory of accepted completions (A2 §6/§16). Off unless
    // COMPME_MEMORY + path are configured; the key comes from
    // COMPME_MEMORY_KEY or (default) the macOS Keychain, generated on
    // first use. Lives on this thread (the rusqlite handle is not Send) and is
    // only touched on Full-accept.
    let memory =
        open_memory_store(
            &config.memory,
            || match platform_macos::keychain::KeychainKeyStore::new().load_or_create_memory_key() {
                Ok(key) => Some(key),
                Err(err) => {
                    eprintln!("compme: keychain memory key unavailable: {err}");
                    None
                }
            },
        );
    let clipboard_cell: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let screen_cell: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    // Screen OCR only contributes when the grant is actually present this session.
    let screen_active = config.screen_context && screen_recording_permission();
    // Clipboard/screen context work independently of previous-input context, so
    // the worker needs a positive char bound when any of them is enabled.
    let context_bound = context_bound_chars(
        config.clipboard_context,
        screen_active,
        config.context_max_chars,
    );
    let worker_context = WorkerContext {
        previous_inputs: previous_inputs.clone(),
        clipboard: Arc::clone(&clipboard_cell),
        screen: Arc::clone(&screen_cell),
        max_chars: context_bound,
        diag_context: config.diag_context,
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
        enabled: Arc::new(AtomicBool::new(config.enabled)),
        quit: Arc::new(AtomicBool::new(false)),
        open_settings: Arc::new(AtomicBool::new(false)),
        snooze_requested: Arc::new(AtomicBool::new(false)),
        global_disable: Arc::new(Mutex::new(None)),
        open_settings_window: Arc::new(AtomicBool::new(false)),
        collection_toggle: Arc::new(AtomicBool::new(false)),
        app_disable: Arc::new(Mutex::new(None)),
    };
    // Runtime-mutable policy (snooze); starts from the configured prefs. The
    // ONE prefs the loop reads — never read config.prefs after this point, or
    // the policy source splits.
    let mut prefs = config.prefs.clone();
    // A tray failure is non-fatal — the engine still runs headless.
    let tray = match MacosTray::new(flags.clone()) {
        Ok(tray) => Some(tray),
        Err(err) => {
            eprintln!("compme: tray unavailable: {err:?}");
            None
        }
    };

    // Screen OCR (Vision, ~200–800 ms) runs on its own thread so it never
    // stalls this AppKit run loop (overlay repaint + Carbon accept callbacks).
    // It publishes redacted text into `screen_cell`, which the inference worker
    // reads; one-submit staleness is accepted (as for clipboard).
    let screen_ocr = if screen_active {
        match ScreenOcr::spawn(Arc::clone(&screen_cell), context_bound, config.diag_context) {
            Ok(ocr) => Some(ocr),
            Err(err) => {
                eprintln!("compme: screen OCR worker unavailable: {err}; screen context disabled");
                None
            }
        }
    } else {
        None
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
    // Labs pane: the NSSwitch writes this flag; the watcher below persists and
    // re-applies it. `global_mid_word` is the live global default (config is
    // only the launch-time snapshot once the pane can change it).
    let mut global_mid_word = config.allow_mid_word;
    let settings_flags = build_settings_flags(&config, Arc::clone(&flags.enabled));
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
    let mut settings_window = platform_macos::MacosSettingsWindow::new(settings_flags.clone());
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
        let now_ms = start.elapsed().as_millis() as u64;
        // Wall-clock stamp for usage stats only (its 30-day window needs an
        // absolute clock); `now_ms` stays monotonic for latency/debounce deltas.
        let wall_ms = wall_now_ms();

        // 1. Host events → engine. The caret callback is the typing driver: read
        // context (executes on the adapter's AX worker), diff into a TextChange.
        // Drain the queue first, then collapse bursts of same-field caret reads so
        // we issue at most one AX round-trip per field per heartbeat.
        let drained: Vec<HostEvent> = rx.try_iter().collect();
        for event in coalesce_caret_reads(drained) {
            if host_event_invalidates_pending_request(&event) {
                latest.clear();
            }
            match event {
                HostEvent::Focus(field) => {
                    let (field, app_key) = canonicalize_field_app(field, bundle_id_for_pid);
                    eprintln!("compme: focus {}", field.element_id);
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
                    platform_macos::set_tab_hotkey_suppressed(
                        prefs.tab_disabled(app_key.as_deref()),
                    );
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
                    offer_all(&mut latest, log_err("on_focus", engine.on_focus(field)));
                }
                HostEvent::Caret(field, _rect) => {
                    let (field, _app_key) = canonicalize_field_app(field, bundle_id_for_pid);
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
                                        display_scales()
                                    );
                                }
                            }
                            match tracker.observe(&field, &ctx, TriggerPolicy::Automatic, now_ms) {
                                Observation::Typed(change) => {
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
                                    let replace_app_key =
                                        resolve_app_key(field.pid, bundle_id_for_pid);
                                    let decision = replacement_decision(
                                        &ctx.left,
                                        &config,
                                        &prefs,
                                        replace_app_key.as_deref(),
                                        cached_domain(&last_domain, replace_app_key.as_deref()),
                                        flags.enabled.load(Ordering::Relaxed),
                                        now_ms,
                                    );
                                    if debug_enabled() {
                                        // Diagnose emoji/typo/spelling preempt vs the
                                        // model: the left context the decision saw, the
                                        // feature toggles, and what (if anything) it
                                        // offered. `decision == None` while a model
                                        // request fires for the same text = the local
                                        // offer is not matching/gating as expected.
                                        eprintln!(
                                            "compme: replace left={:?} emoji={} \
                                             autocorrect={} british={} thesaurus={} decision={decision:?}",
                                            ctx.left,
                                            config.emoji.is_some(),
                                            config.autocorrect,
                                            config.british_english,
                                            config.thesaurus,
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
                            if let Some(app) = resolve_app_key(field.pid, bundle_id_for_pid) {
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
                    eprintln!("compme: accept {action:?}");
                    // Preview the engine's accept payload once and reuse it for
                    // both the Word self-insert and the Full context record, so
                    // the two never read divergent engine snapshots.
                    let preview = engine.preview_accept_insert(action);
                    // Record only *full* accepts: a full completion is meaningful
                    // prior text, whereas a single word (the Word-accept payload)
                    // is low-signal. Routed to two opt-in sinks — the volatile
                    // previous-input ring and the encrypted on-disk memory store.
                    if let (Some(field), Some((_, text, _))) =
                        (current_field.as_ref(), preview.as_ref())
                    {
                        record_full_accept(
                            action,
                            field,
                            text,
                            config.context_max_chars,
                            &previous_inputs,
                            memory.as_ref(),
                            prefs.collection_allowed(Some(&field.app)),
                        );
                    }
                    match engine.on_accept(action) {
                        Ok(requests) => {
                            // Absorb the accept's own insertion echo (Word OR
                            // Full) into the diff baseline so the AX readback of
                            // the inserted text registers as a caret move, not new
                            // typing — otherwise the echo would arm a spurious
                            // post-accept completion request (engine-macos §4 step
                            // 9: the accept's own insert is not a new edit).
                            if let Some((field, text, replace_left)) = &preview {
                                // Absorb the accept's echo. A replacement
                                // (`replace_left > 0`, e.g. emoji) deletes the
                                // typed token before inserting, so the baseline
                                // must delete-then-insert to match the field; an
                                // ordinary completion is append-only.
                                if *replace_left > 0 {
                                    tracker.apply_self_replace(field, text, *replace_left);
                                } else {
                                    tracker.apply_self_insert(field, text);
                                }
                                // Local usage stats (§11/§16): count every accept
                                // (both Word and Full — unlike the full-only
                                // previous-inputs/memory block above) and the words
                                // it inserted (menu-bar word count). At least one
                                // word per accept.
                                usage.record(
                                    wall_ms,
                                    stats::Outcome::Accepted {
                                        words: accept_word_count(text),
                                    },
                                );
                            }
                            offer_all(&mut latest, requests);
                        }
                        Err(err) => eprintln!("compme: on_accept error: {err:?}"),
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
            }
        }

        // 2. Inference outcomes → engine (stale ones are discarded internally).
        for outcome in inference.drain_outcomes() {
            eprintln!(
                "compme: completion gen={} candidates={:?}",
                outcome.request.generation, outcome.candidates
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
            secure = secure_input_enabled();
            trusted = accessibility_trusted();
            last_secure_poll_ms = Some(now_ms);
        }
        // SIGUSR1 toggles enable/disable (headless equivalent of the tray item).
        if TOGGLE.swap(false, Ordering::Relaxed) {
            let now = flags.enabled.load(Ordering::Relaxed);
            flags.enabled.store(!now, Ordering::Relaxed);
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
            if let Ok(mut lines) = settings_flags.stats_lines.lock() {
                *lines = stats_pane_lines(&usage.daily_buckets(wall_ms, 7));
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
            if let Ok(mut lines) = settings_flags.setup_lines.lock() {
                *lines = compose_setup_lines(&config);
            }
            last_setup_poll_ms = Some(now_ms);
            // Apps tab: per-app counts straight from the store (plaintext
            // GROUP BY, no decryption). Unlike setup_lines these are
            // show-time snapshots, same stance as stats_lines (c99): cheap
            // probes refresh live, data aggregations refresh per open.
            if let Ok(mut lines) = settings_flags.apps_lines.lock() {
                (*lines, apps_ids) = compose_apps_rows(memory.as_ref());
            }
            if let Err(err) = settings_window.show() {
                eprintln!("compme: settings window unavailable: {err}");
            }
        }
        // Visibility poll: however the window closed (red button included),
        // demote the activation policy back to Accessory exactly once on the
        // visible→hidden edge so no Dock icon is left stranded.
        let settings_visible = settings_window.is_visible();
        if platform_macos::policy_restore_needed(settings_was_visible, settings_visible) {
            if let Err(err) = settings_window.restore_accessory_policy() {
                eprintln!("compme: activation policy restore failed: {err}");
            }
        }
        settings_was_visible = settings_visible;
        // Setup buttons (tray-flags pattern): consume edges, perform the
        // privileged calls here on the main thread.
        if settings_flags.setup_grant_ax.swap(false, Ordering::Relaxed) {
            prompt_accessibility_trust();
        }
        if settings_flags
            .setup_request_screen
            .swap(false, Ordering::Relaxed)
        {
            request_screen_recording_permission();
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
            if let Err(err) = platform_macos::reveal_file_in_finder(&model_abs) {
                eprintln!("compme: reveal model failed: {err:?}");
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
        if let Some((word, full)) = rebind_request {
            let outcome = apply_live_accept_keymap(
                word,
                full,
                platform_macos::set_accept_keymap_from_config,
                || engine.rearm_accept_keys(),
                |w, f| {
                    if let Some(path) = config::config_file_path() {
                        for (key, value) in
                            [("COMPME_ACCEPT_WORD_KEY", w), ("COMPME_ACCEPT_FULL_KEY", f)]
                        {
                            if let Err(err) =
                                config::persist_setting(&path, key, &value.to_string())
                            {
                                eprintln!("compme: failed to persist {key}: {err}");
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
                platform_macos::effective_accept_keys,
            );
            match outcome {
                Ok(()) => {
                    let (word_key, full_key) = platform_macos::effective_accept_keys();
                    // Recompose the Shortcuts text; show() re-reads it on the
                    // next open (refresh-on-show — the c121 forward trap).
                    if let Ok(mut text) = settings_flags.shortcuts_text.lock() {
                        *text = shortcuts_text(word_key, full_key);
                    }
                    // The slice-4 recorder lives INSIDE the window, so it is
                    // open at exactly this moment — refresh the live label
                    // (show() only covers the reopen edge) (review-c133).
                    settings_window.refresh_shortcuts_label();
                    eprintln!("compme: accept keys rebound (word={word_key} full={full_key})");
                }
                Err(err) => eprintln!("compme: accept-key rebind failed: {err}"),
            }
        }
        // Apps-row Delete: resolve the clicked row index against the ids
        // rendered with the SAME cap/order, delete, recompose, re-render.
        let clicked_row = settings_flags
            .apps_delete_row
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        if let Some(row) = clicked_row {
            if let (Some(store), Some(app)) = (&memory, apps_ids.get(row)) {
                // Irreversible (secure_delete zeroes freed pages) — confirm
                // first, Cancel-default (review-c112; deep-link precedent).
                let confirmed = platform_macos::confirm_delete_app_prompt(app).unwrap_or(false);
                if !confirmed {
                    eprintln!("compme: delete for {app} cancelled");
                } else if let Some((lines, ids)) =
                    delete_app_row_and_recompose(store, &apps_ids, row)
                {
                    if let Ok(mut shared) = settings_flags.apps_lines.lock() {
                        *shared = lines;
                    }
                    apps_ids = ids;
                    settings_window.refresh_apps_labels();
                }
            }
        }
        // Setup "Download Recommended Model": one-click fetch of the
        // smallest unencumbered catalog entry into the app-support models
        // dir (D14 wiring; picker UI is a later slice). Progress is logged;
        // on Done the log says how to point COMPME_MODEL_PATH at it.
        if settings_flags
            .setup_download_model
            .swap(false, Ordering::Relaxed)
            && download_idle(model_download_status.as_deref())
        {
            // Selected-or-recommended: identical to recommended() until the
            // picker popup (D14 3b.4 slice b) writes a different index.
            if let (Some(entry), Some(home)) = (
                crate::model_picker::selected_catalog_entry(
                    crate::model_picker::recommended_index(),
                ),
                std::env::var_os("HOME"),
            ) {
                // License click-through gate (D14, c95 "once per model"):
                // inert for today's unencumbered recommended() target; bites
                // when a future picker selects a GemmaTerms/LlamaCommunity
                // entry. EVERY download path must route through this gate —
                // a second path that skips it silently bypasses the terms.
                let allowed = match model_catalog::download_gate(entry, |name| {
                    config.license_accepted.contains(name)
                }) {
                    model_catalog::DownloadGate::Proceed => true,
                    model_catalog::DownloadGate::NeedsLicense {
                        model,
                        license_name,
                        terms_url,
                    } => {
                        // Prompt failure (typed but unreachable off-main) =
                        // decline: the gate fails closed.
                        let accepted =
                            platform_macos::confirm_license_prompt(model, license_name, terms_url)
                                .unwrap_or(false);
                        if accepted {
                            // In-memory FIRST (same-session re-prompt guard),
                            // then persist; a failed write only logs — the
                            // user DID accept, so the download proceeds.
                            let value =
                                record_license_acceptance(&mut config.license_accepted, model);
                            if let Some(path) = config::config_file_path() {
                                if let Err(err) = config::persist_setting(
                                    &path,
                                    "COMPME_LICENSE_ACCEPTED",
                                    &value,
                                ) {
                                    eprintln!(
                                        "compme: failed to persist COMPME_LICENSE_ACCEPTED: {err}"
                                    );
                                }
                            }
                            eprintln!("compme: {license_name} accepted for {model}");
                        } else {
                            eprintln!(
                                "compme: download of {model} cancelled (license not accepted)"
                            );
                        }
                        accepted
                    }
                };
                if allowed {
                    let dest = std::path::PathBuf::from(home)
                        .join("Library/Application Support/compme/models")
                        .join(format!("{}.gguf", entry.name));
                    let _ = std::fs::create_dir_all(dest.parent().unwrap_or(&dest));
                    if model_downloader.is_none() {
                        model_downloader = model_fetch::ModelDownloader::spawn().ok();
                    }
                    if let Some(downloader) = &model_downloader {
                        let status = std::sync::Arc::new(model_fetch::DownloadStatus::default());
                        // Track the status ONLY when the request was queued:
                        // a dropped request's status stays Idle forever and
                        // would wedge the download_idle gate (review-c130).
                        if downloader.request(catalog_download_request(
                            entry,
                            dest,
                            std::sync::Arc::clone(&status),
                        )) {
                            eprintln!(
                                "compme: downloading {} ({} MB) \u{2014} progress in this log",
                                entry.name, entry.size_mb
                            );
                            model_download_status = Some(status);
                            model_download_logged = 0;
                        } else {
                            eprintln!("compme: model download queue busy \u{2014} try again");
                        }
                    }
                }
            }
        }
        // Download progress/terminal-state logging (one line per transition).
        if let Some(status) = &model_download_status {
            let state = status.state.lock().unwrap_or_else(|e| e.into_inner());
            let (next_logged, line) = download_log_transition(&state, model_download_logged);
            model_download_logged = next_logged;
            if let Some(line) = line {
                eprintln!("{line}");
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
            if let Ok(mut lines) = settings_flags.setup_lines.lock() {
                *lines = compose_setup_lines(&config);
            }
            settings_window.refresh_setup_labels();
        }
        // General-tab Autocorrect watcher: persist + apply on the edge. The
        // decision path reads config.autocorrect per offer, so a field write
        // IS the live apply (per-app overrides still win).
        if let Some(on) = switch_edge(&settings_flags.general_autocorrect, &mut config.autocorrect)
        {
            // Live apply = the field write itself (read per offer).
            persist_and_log_switch("COMPME_AUTOCORRECT", "autocorrect", on);
        }
        // General-tab Trailing-space watcher: persist + live engine apply
        // (the flag is baked at build via with_trailing_space, so the c94
        // runtime-setter pattern applies — set_trailing_space).
        if let Some(on) = switch_edge(
            &settings_flags.general_trailing_space,
            &mut config.trailing_space,
        ) {
            engine.set_trailing_space(on);
            persist_and_log_switch("COMPME_TRAILING_SPACE", "trailing space", on);
        }
        // Labs-pane watcher: on a switch edge, persist COMPME_MIDLINE and
        // re-apply the engine gate for the current app immediately (per-app
        // overrides still win; the switch changes only the global default).
        // A persist failure is logged but not retried — the runtime global
        // wins until relaunch (deliberate graceful degradation, same stance
        // as the instance lock: an IO hiccup must not stall the app, at the
        // cost of config.env staying stale until the next successful write).
        if let Some(on) = switch_edge(&settings_flags.labs_midline, &mut global_mid_word) {
            engine.set_allow_mid_word(prefs.mid_line_enabled(last_app_key.as_deref(), on));
            persist_and_log_switch("COMPME_MIDLINE", "mid-line completions", on);
        }
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
                let webconfig::PromptDecision::PromptRequired {
                    scope,
                    action,
                    trust,
                } = decision
                else {
                    return true; // reserved silent class (unreachable today)
                };
                platform_macos::confirm_deep_link_prompt(scope, action, trust).unwrap_or(false)
            };
            match handle_deep_link(&url, config.trusted_key.as_ref(), &mut prefs, confirm) {
                Ok(summary) => {
                    eprintln!("compme: deep link {summary}");
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
            match current_field
                .as_ref()
                .and_then(|f| resolve_app_key(f.pid, bundle_id_for_pid))
            {
                Some(app) => {
                    let allowed = toggle_app_collection(&mut prefs, &app);
                    eprintln!(
                        "compme: input collection in {app} now {}",
                        if allowed { "ENABLED" } else { "DISABLED" }
                    );
                    if let Some(path) = config::config_file_path() {
                        if let Err(err) = config::persist_setting(
                            &path,
                            "COMPME_NO_COLLECT_APPS",
                            &no_collect_apps_value(&prefs),
                        ) {
                            eprintln!("compme: could not persist no-collect apps: {err}");
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
            match current_field
                .as_ref()
                .and_then(|f| resolve_app_key(f.pid, bundle_id_for_pid))
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
        if status.suggestions_allowed() {
            if let Some(request) = latest.take() {
                // Per-app/domain gating + pause/snooze (A2 §8). The exclude list
                // is keyed on bundle ids, so resolve the focused pid to a bundle
                // id (the field's own `app` is a volatile `pid:N`); fail-open if
                // it can't be resolved. The domain comes from the Focus arm's
                // cache, guarded on the same app key (c131).
                let app_key = resolve_app_key(request.field.pid, bundle_id_for_pid);
                if request_passes_submit_gates(
                    &request,
                    app_key.as_deref(),
                    cached_domain(&last_domain, app_key.as_deref()),
                    &prefs,
                    now_ms,
                ) {
                    // Refresh the clipboard context cell (redacted) just before a
                    // submit that will use it (A2 §16 clipboard context). Invariant:
                    // the cell is rewritten before *every* gated submit, so the
                    // worker (which reads the latest cell for the surviving
                    // coalesced request) never attaches a prior app's clipboard.
                    if config.clipboard_context {
                        let clip = read_pasteboard_text().map(|text| redaction::redact(&text));
                        if config.diag_context {
                            eprintln!("compme: clipboard_context={clip:?}");
                        }
                        *clipboard_cell.lock().unwrap_or_else(|e| e.into_inner()) = clip;
                    }
                    // Screen-aware context (A2 §16): hand the caret's display to
                    // the off-thread OCR worker (it captures, OCRs, redacts, and
                    // publishes into `screen_cell`). Fire-and-forget so the run
                    // loop never blocks on Vision. caret_rect read is the only
                    // on-loop AX touch and is cheap.
                    if let Some(ocr) = &screen_ocr {
                        let caret_rect = adapter.caret_rect(&request.field).ok().flatten();
                        ocr.request(caret_rect);
                    }
                    eprintln!(
                        "compme: request gen={} prompt={:?}",
                        request.generation, request.prompt
                    );
                    submit_times.insert(request.generation, now_ms);
                    inference.submit(request);
                }
            }
        }

        // 6. Tray actions (menu callbacks fire on this same main thread via the
        // run-loop pump, so Relaxed is sufficient for these flags).
        if flags.open_settings.swap(false, Ordering::Relaxed) {
            // spawn, not status(): waiting on open(1) would block the
            // heartbeat, and nothing reads the exit status anyway.
            if let Err(err) = Command::new("open").arg(ACCESSIBILITY_SETTINGS_URL).spawn() {
                eprintln!("compme: open settings failed: {err}");
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

        // 8. Drain queued AppKit/window-server events, then pump the main run
        // loop. The drain is what dispatches Carbon accept-hotkey presses to
        // their handler (a bare CFRunLoop pump never dequeues them — live
        // step-6 finding: hotkeys registered, handler never fired); the
        // CFRunLoop pump paces the loop and services the overlay.
        platform_macos::pump_app_events();
        // SAFETY: `kCFRunLoopDefaultMode` is a Core Foundation extern static.
        let mode = unsafe { kCFRunLoopDefaultMode };
        CFRunLoop::run_in_mode(mode, heartbeat, false);
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
    fn personalization_defaults_to_no_steer_when_keys_absent() {
        let profile = build_personalization(&lookup(&[]));
        assert_eq!(profile.build_preamble(Some("com.apple.TextEdit"), None), "");
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
        assert_eq!(lines.len(), platform_macos::STATS_ROWS);
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
        let config = Config::from_lookup(lookup(&[]));
        let tray_enabled = Arc::new(AtomicBool::new(true));
        let flags = build_settings_flags(&config, Arc::clone(&tray_enabled));
        assert!(Arc::ptr_eq(&flags.general_enabled, &tray_enabled));
        assert!(flags.general_autocorrect.load(Ordering::Relaxed) == config.autocorrect);
    }

    #[test]
    fn shortcuts_text_names_known_keycodes_and_falls_back_numerically() {
        // Shortcuts tab (persist-only slice): current bindings by NAME for
        // the known codes, numeric fallback for exotic rebinds, fixed rows
        // for the non-rebindable keys, and the how-to-change note.
        let text = shortcuts_text(48, 50);
        assert!(text.contains("Accept word: Tab"));
        assert!(text.contains("Accept full: ` (backtick)"));
        assert!(text.contains("Dismiss: Esc"));
        assert!(text.contains("Cycle candidates: Down arrow"));
        assert!(text.contains("COMPME_ACCEPT_WORD_KEY"));
        assert!(text.contains("relaunch"));

        let custom = shortcuts_text(125, 200);
        assert!(custom.contains("Accept word: Down arrow"));
        assert!(custom.contains("Accept full: key 200")); // unnamed code → generic
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
        assert_eq!(ids.len(), platform_macos::APPS_ROWS);
        assert_eq!(ids[0], "app00");
        assert_eq!(ids.len(), apps_pane_lines(&many, true).len());
        // Status lines carry no deletable rows.
        assert!(apps_row_ids(&[]).is_empty());
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
        assert_eq!(
            apps_pane_lines(&many, true).len(),
            platform_macos::APPS_ROWS
        );
    }

    #[test]
    fn setup_pane_composition_respects_the_row_limit() {
        // The window builds SETUP_ROWS labels; zip-truncation would
        // silently hide overflow rows (review-c106, c103 precedent). Pin
        // against the REAL const, not a drifting literal.
        let rows = crate::setup_state::setup_rows(crate::setup_state::SetupChecks {
            ax_trusted: true,
            screen_context_enabled: true,
            screen_recording: true,
            model_exists: true,
        });
        assert!(rows.len() <= platform_macos::SETUP_ROWS);
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
            "COMPME_TRAILING_SPACE",
            "COMPME_NO_COLLECT_APPS",
            "COMPME_EXCLUDED_APPS",
            "COMPME_EXCLUDED_DOMAINS",
            "COMPME_ENABLED_APPS",
            "COMPME_DISABLED_APPS",
            "COMPME_LICENSE_ACCEPTED",
            "COMPME_ACCEPT_WORD_KEY",
            "COMPME_ACCEPT_FULL_KEY",
        ] {
            assert!(
                every_warning.iter().any(|warning| warning.starts_with(key)),
                "{key} must warn when env shadows persisted config"
            );
        }
        assert_eq!(every_warning.len(), 12);
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
    fn accept_key_config_parses_keycodes_and_rejects_junk() {
        // Raw macOS virtual keycodes (the future shortcuts-pane recorder
        // emits keycodes too); junk → None → default bindings.
        let config = Config::from_lookup(lookup(&[
            ("COMPME_ACCEPT_WORD_KEY", "122"),
            ("COMPME_ACCEPT_FULL_KEY", "120"),
        ]));
        assert_eq!(config.accept_word_key, Some(122));
        assert_eq!(config.accept_full_key, Some(120));
        let junk = Config::from_lookup(lookup(&[("COMPME_ACCEPT_WORD_KEY", "tab")]));
        assert_eq!(junk.accept_word_key, None);
        assert_eq!(Config::from_lookup(lookup(&[])).accept_word_key, None);
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
        let on = Config::from_lookup(lookup(&[
            ("COMPME_CLIPBOARD_CONTEXT", "1"),
            ("COMPME_SCREEN_CONTEXT", "true"),
            ("COMPME_DIAG_CONTEXT", "true"),
        ]));
        assert!(on.clipboard_context);
        assert!(on.screen_context);
        assert!(on.diag_context);
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
            Some(35),
            Some(38),
            |w, f| {
                l1.borrow_mut().push(format!("set:{w:?},{f:?}"));
                Ok(())
            },
            || {
                l2.borrow_mut().push("rearm".into());
                Ok(())
            },
            |w, f| l3.borrow_mut().push(format!("persist:{w},{f}")),
            || (35, 38),
        );
        assert!(ok.is_ok());
        assert_eq!(
            *log.borrow(),
            vec![
                "set:Some(35),Some(38)".to_string(),
                "rearm".to_string(),
                "persist:35,38".to_string(),
            ]
        );

        // Failure path: set → rearm Err → REVERT set, no persist.
        let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
        let l1 = std::rc::Rc::clone(&log);
        let l2 = std::rc::Rc::clone(&log);
        let l3 = std::rc::Rc::clone(&log);
        let err = apply_live_accept_keymap(
            Some(35),
            Some(38),
            |w, f| {
                l1.borrow_mut().push(format!("set:{w:?},{f:?}"));
                Ok(())
            },
            || {
                l2.borrow_mut().push("rearm".into());
                Err(PlatformError::Timeout)
            },
            |w, f| l3.borrow_mut().push(format!("persist:{w},{f}")),
            || (48, 50), // the pre-swap registered truth
        );
        assert!(err.is_err());
        assert_eq!(
            *log.borrow(),
            vec![
                "set:Some(35),Some(38)".to_string(),
                "rearm".to_string(),
                "set:Some(48),Some(50)".to_string(), // revert
            ],
            "no persist after a failed re-arm"
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
            Some(35),
            Some(38),
            |w, f| {
                // First call (the forward set) succeeds; the second (the
                // revert) fails.
                if calls.get() == 0 {
                    calls.set(1);
                    Ok(())
                } else {
                    Err(platform_macos::KeymapError::Collision(w.or(f).unwrap_or(0)))
                }
            },
            || {
                l2.borrow_mut().push("rearm".into());
                Err(PlatformError::Timeout)
            },
            |w, f| l3.borrow_mut().push(format!("persist:{w},{f}")),
            || (48, 50),
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
            Some(38),
            |w, f| {
                l1.borrow_mut().push(format!("set:{w:?},{f:?}"));
                Ok(())
            },
            || {
                l2.borrow_mut().push("rearm".into());
                Ok(())
            },
            |w, f| l3.borrow_mut().push(format!("persist:{w},{f}")),
            || (48, 38), // post-resolution: default word stays 48
        );
        assert!(partial.is_ok());
        assert_eq!(
            log.borrow().last().unwrap(),
            "persist:48,38",
            "persist writes the RESOLVED pair, not the raw request"
        );

        // Invalid map (collision) fails BEFORE any rearm/persist.
        let log: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
        let l2 = std::rc::Rc::clone(&log);
        let l3 = std::rc::Rc::clone(&log);
        let invalid = apply_live_accept_keymap(
            Some(53),
            None,
            |_, _| Err(platform_macos::KeymapError::Collision(53)),
            || {
                l2.borrow_mut().push("rearm".into());
                Ok(())
            },
            |w, f| l3.borrow_mut().push(format!("persist:{w},{f}")),
            || (48, 50),
        );
        assert!(invalid.is_err());
        assert!(
            log.borrow().is_empty(),
            "rejected map never rearms/persists"
        );
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
        // No domain resolved → fail-open, exactly today's behavior.
        assert!(request_passes_submit_gates(
            &req_with_prompt("Dear team"),
            Some("com.apple.Safari"),
            None,
            &prefs,
            0
        ));
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

    fn field_with_app(app: &str) -> FieldHandle {
        FieldHandle {
            app: app.into(),
            pid: Some(7),
            element_id: "ax:field".into(),
            generation: 1,
        }
    }

    fn accepted_store() -> memory::MemoryStore {
        memory::MemoryStore::open_in_memory(
            &memory::StaticKey([3u8; 32]),
            memory::StorageMode::AcceptedOnly,
        )
        .expect("open in-memory store")
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

        assert!(worker_context
            .block_for("com.apple.TextEdit")
            .contains("accepted completion"));
        assert!(!worker_context
            .block_for("pid:42")
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
    fn per_app_autocorrect_on_list_overrides_a_global_off() {
        // COMPME_AUTOCORRECT_ON_APPS: the positive override loop — a typo'd
        // key string in that parse would silently kill the feature.
        let prefs = build_prefs(&lookup(&[("COMPME_AUTOCORRECT_ON_APPS", "com.a.one")]));
        assert!(prefs.autocorrect_enabled(Some("com.a.one"), false));
        assert!(!prefs.autocorrect_enabled(Some("com.other"), false));
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
    fn model_ready_counts_a_stub_as_a_model_source() {
        let stub = Config::from_lookup(lookup(&[
            ("COMPME_STUB_COMPLETION", "x"),
            ("COMPME_MODEL_PATH", "/no/such/file.gguf"),
        ]));
        assert!(model_ready(&stub), "stub-driven runs must not flag Setup");
        let missing = Config::from_lookup(lookup(&[("COMPME_MODEL_PATH", "/no/such/file.gguf")]));
        assert!(!model_ready(&missing));
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
        // Done logs the COMPME_MODEL_PATH hint — the only user-visible
        // signal of where the model landed — even when it skipped Running.
        let done = DownloadState::Done("/tmp/m.gguf".into());
        let (logged, line) = download_log_transition(&done, 0);
        assert_eq!(logged, 2);
        assert!(line.unwrap().contains("COMPME_MODEL_PATH=/tmp/m.gguf"));
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

    fn req_with_prompt(prompt: &str) -> CompletionRequest {
        CompletionRequest {
            prompt: prompt.into(),
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
        assert!(host_event_invalidates_pending_request(&HostEvent::Dismiss));
        assert!(!host_event_invalidates_pending_request(&HostEvent::Cycle));
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
