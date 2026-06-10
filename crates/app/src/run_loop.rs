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
use std::path::PathBuf;
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
    /// Launch-at-login (A3 D13, `COMPME_LAUNCH_AT_LOGIN`): `Some(true/false)`
    /// registers/unregisters the SMAppService login item at startup; `None`
    /// (absent or unrecognized) leaves the user's Login Items setting alone.
    launch_at_login: Option<bool>,
    /// Host-pinned Ed25519 key for SIGNED deep links (`COMPME_TRUSTED_KEY`,
    /// 64 hex). `None` (default, incl. malformed) = signed links rejected
    /// fail-closed; unsigned reversible links work either way.
    trusted_key: Option<webconfig::TrustedKey>,
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
) -> Option<(String, usize)> {
    if let Some(offer) = emoji_offer(left, &config.emoji) {
        return Some(offer);
    }
    let word = trailing_word(left)?;
    let word_len = word.chars().count();
    if autocorrect_enabled {
        if let Some(fix) = autocorrect::correct(word) {
            return Some((fix, word_len));
        }
    }
    if config.british_english {
        if let Some(uk) = localize::to_british(word) {
            return Some((uk, word_len));
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
    enabled: bool,
    now_ms: u64,
) -> Option<(String, usize)> {
    // `prefs` is passed separately from `config` because the run loop mutates
    // its prefs at runtime (snooze); reading `config.prefs` here would split
    // the policy source and let a local offer show while the model is snoozed.
    if !enabled || !suggestion_gates_pass(app_key, left, prefs, now_ms) {
        return None;
    }
    // Per-app autocorrect override (App Settings): prefs override, else the
    // global config default. (mid_line's merge is pending an engine API
    // change — its gate is baked at engine build time.)
    let autocorrect = prefs.autocorrect_enabled(app_key, config.autocorrect);
    replacement_offer(left, config, autocorrect)
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
fn suggestion_gates_pass(app_key: Option<&str>, text: &str, prefs: &Prefs, now_ms: u64) -> bool {
    let terminal_ok = app_key.is_none_or(|app| compat::terminal_prompt_activates(app, text));
    app_allows_suggestions(app_key) && terminal_ok && prefs.should_suggest(app_key, None, now_ms)
}

fn request_passes_submit_gates(
    request: &CompletionRequest,
    app_key: Option<&str>,
    prefs: &Prefs,
    now_ms: u64,
) -> bool {
    suggestion_gates_pass(app_key, &request.prompt, prefs, now_ms)
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

/// The COMPME_NO_COLLECT_APPS persistence value: sorted comma-joined apps with
/// collection explicitly off, round-trippable through build_prefs.
fn no_collect_apps_value(prefs: &Prefs) -> String {
    let mut apps: Vec<&str> = prefs
        .per_app
        .iter()
        .filter(|(_, policy)| policy.collect_inputs == Some(false))
        .map(|(app, _)| app.as_str())
        .collect();
    apps.sort_unstable();
    apps.join(",")
}

/// The COMPME_EXCLUDED_APPS persistence value: comma-joined, sorted for a
/// stable file diff, round-trippable through the build_prefs parser.
fn excluded_apps_value(prefs: &Prefs) -> String {
    let mut apps: Vec<&str> = prefs.excluded_apps.iter().map(String::as_str).collect();
    apps.sort_unstable();
    apps.join(",")
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

/// Build suggestion-gating preferences from config (A2 §8). A comma-separated
/// app-exclude list and a default-enabled toggle; finer per-app/domain overrides
/// are an A3 settings concern.
fn build_prefs(lookup: &impl Fn(&str) -> Option<String>) -> Prefs {
    let excluded_apps = lookup("COMPME_EXCLUDED_APPS")
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mut prefs = Prefs {
        default_enabled: parse_enabled_default(lookup("COMPME_DEFAULT_ENABLED")),
        excluded_apps,
        ..Default::default()
    };
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
    let list = |raw: Option<String>| -> Vec<String> {
        raw.map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
    };
    for app in list(lookup("COMPME_MIDLINE_ON_APPS")) {
        prefs.per_app.entry(app).or_default().mid_line = Some(true);
    }
    for app in list(lookup("COMPME_MIDLINE_OFF_APPS")) {
        prefs.per_app.entry(app).or_default().mid_line = Some(false);
    }
    for app in list(lookup("COMPME_AUTOCORRECT_ON_APPS")) {
        prefs.per_app.entry(app).or_default().autocorrect = Some(true);
    }
    for app in list(lookup("COMPME_AUTOCORRECT_OFF_APPS")) {
        prefs.per_app.entry(app).or_default().autocorrect = Some(false);
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
    let config = Config::from_env();
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
        enabled: Arc::new(AtomicBool::new(config.enabled)),
        quit: Arc::new(AtomicBool::new(false)),
        open_settings: Arc::new(AtomicBool::new(false)),
        snooze_requested: Arc::new(AtomicBool::new(false)),
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
    let mut settings_window = platform_macos::MacosSettingsWindow::new();
    let mut settings_was_visible = false;
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
                                             autocorrect={} british={} decision={decision:?}",
                                            ctx.left,
                                            config.emoji.is_some(),
                                            config.autocorrect,
                                            config.british_english,
                                        );
                                    }
                                    if let Some((glyph, replace_left)) = decision {
                                        // Drop the just-queued model request so it
                                        // can't supersede the emoji ghost.
                                        latest.clear();
                                        offer_all(
                                            &mut latest,
                                            log_err(
                                                "offer_replacement",
                                                engine.offer_replacement(
                                                    &field,
                                                    glyph,
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
        // Drain received compme:// deep links (strict fail-closed parse →
        // reversible override). Every outcome is logged (the §16 user-visible
        // requirement; a confirmation prompt is the follow-up). An applied
        // override changes suggestion policy, so fire the dismiss edge
        // (a2-parity review #2) and persist the exclude list.
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
                        if let Err(err) = config::persist_setting(
                            &path,
                            "COMPME_EXCLUDED_APPS",
                            &excluded_apps_value(&prefs),
                        ) {
                            eprintln!("compme: could not persist excluded apps: {err}");
                        }
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
                // it can't be resolved. Domain is None until browser-domain
                // extraction lands.
                let app_key = resolve_app_key(request.field.pid, bundle_id_for_pid);
                if request_passes_submit_gates(&request, app_key.as_deref(), &prefs, now_ms) {
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
            if let Err(err) = Command::new("open")
                .arg(ACCESSIBILITY_SETTINGS_URL)
                .status()
            {
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
    // Session usage summary (§11/§16).
    let final_ms = start.elapsed().as_millis() as u64;
    let counts = usage.counts(final_ms);
    eprintln!(
        "compme: usage shown={} accepted={} dismissed={} superseded={} words={} \
         latency_avg={:?} latency_p95={:?}",
        counts.shown,
        counts.accepted,
        counts.dismissed,
        counts.superseded,
        usage.words_completed(final_ms),
        usage.latency_avg_ms(final_ms),
        usage.latency_p95_ms(final_ms),
    );
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
        let mut prefs = config.prefs.clone();
        let _ = &mut prefs;
        // `teh` is a known typo; in the opted-out app no offer fires…
        assert_eq!(
            replacement_decision(
                "teh",
                &config,
                &config.prefs,
                Some("com.quiet.app"),
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
    fn config_enabled_reads_complete_me_enabled_and_defaults_on() {
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
            &prefs,
            0
        ));
        // Sidebar-only app → blocked.
        assert!(!suggestion_gates_pass(
            Some("com.microsoft.VSCode"),
            "color",
            &prefs,
            0
        ));
        // Terminal with a shell-command line → blocked (not a natural-language prompt).
        assert!(!suggestion_gates_pass(
            Some("com.googlecode.iterm2"),
            "git status && ls -la",
            &prefs,
            0
        ));
    }

    #[test]
    fn submit_gate_combines_app_terminal_and_preference_policy() {
        let prefs = Prefs::default();
        assert!(request_passes_submit_gates(
            &req_with_prompt("Dear team"),
            Some("com.apple.TextEdit"),
            &prefs,
            0
        ));
        assert!(!request_passes_submit_gates(
            &req_with_prompt("Dear team"),
            Some("com.mitchellh.ghostty"),
            &prefs,
            0
        ));
        assert!(!request_passes_submit_gates(
            &req_with_prompt("Dear team"),
            Some("com.microsoft.VSCode"),
            &prefs,
            0
        ));
        assert!(!request_passes_submit_gates(
            &req_with_prompt("git status && ls -la"),
            Some("com.googlecode.iterm2"),
            &prefs,
            0
        ));
        assert!(request_passes_submit_gates(
            &req_with_prompt("please summarize the recent changes"),
            Some("com.googlecode.iterm2"),
            &prefs,
            0
        ));

        let excluded = build_prefs(&lookup(&[("COMPME_EXCLUDED_APPS", "com.apple.TextEdit")]));
        assert!(!request_passes_submit_gates(
            &req_with_prompt("Dear team"),
            Some("com.apple.TextEdit"),
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
        assert!(!request_passes_submit_gates(&req, app, &prefs, 1_000));
        assert!(!request_passes_submit_gates(&req, app, &prefs, 61_000));
        // Auto-resumes once the window elapses.
        assert!(request_passes_submit_gates(&req, app, &prefs, 301_001));
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
            &excluded,
            0
        ));

        // Unresolved pid fails open and does not treat the volatile `pid:42`
        // field app as a preference key.
        let unresolved = resolve_app_key(volatile.field.pid, |_| None);
        assert!(request_passes_submit_gates(
            &volatile,
            unresolved.as_deref(),
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
    fn latency_sample_empty_map_is_none() {
        let mut submit: HashMap<u64, u64> = HashMap::new();
        assert_eq!(latency_sample(&mut submit, 1, 100), None);
        assert!(submit.is_empty());
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
        // Off → no word-based offer even on a known typo / americanism.
        assert!(replacement_offer("teh", &config, config.autocorrect).is_none());
        assert!(replacement_offer("color", &config, config.autocorrect).is_none());
    }

    #[test]
    fn replacement_offer_fires_for_enabled_word_features() {
        let ac = Config::from_lookup(lookup(&[("COMPME_AUTOCORRECT", "1")]));
        assert_eq!(
            replacement_offer("I teh", &ac, ac.autocorrect),
            Some(("the".into(), 3))
        );
        // A correctly-spelled word never offers.
        assert!(replacement_offer("the", &ac, ac.autocorrect).is_none());

        let uk = Config::from_lookup(lookup(&[("COMPME_BRITISH_ENGLISH", "on")]));
        assert_eq!(
            replacement_offer("color", &uk, uk.autocorrect),
            Some(("colour".into(), 5))
        );
        assert!(replacement_offer("colour", &uk, uk.autocorrect).is_none());
    }

    #[test]
    fn replacement_offer_prioritizes_emoji_then_word_features() {
        // Emoji shortcode wins over the word-based features when all are enabled.
        let all = Config::from_lookup(lookup(&[
            ("COMPME_EMOJI", "1"),
            ("COMPME_AUTOCORRECT", "1"),
            ("COMPME_BRITISH_ENGLISH", "1"),
        ]));
        let (glyph, replace_left) =
            replacement_offer("teh :smile", &all, all.autocorrect).expect("emoji wins");
        assert!(!glyph.is_empty());
        assert_eq!(replace_left, 6); // ":smile", not the word "teh"
    }

    #[test]
    fn replacement_decision_combines_gate_and_offer() {
        let config = Config::from_lookup(lookup(&[("COMPME_EMOJI", "1")]));
        let allowed = Some("com.apple.TextEdit");
        // Enabled (tray) + allowed app + a shortcode → offers.
        assert!(
            replacement_decision("hi :smile", &config, &config.prefs, allowed, true, 0).is_some()
        );
        // Tray-disabled → no offer even with a match.
        assert!(
            replacement_decision("hi :smile", &config, &config.prefs, allowed, false, 0).is_none()
        );
        // Sidebar-only / blocked app → no offer even when enabled.
        assert!(replacement_decision(
            "hi :smile",
            &config,
            &config.prefs,
            Some("com.microsoft.VSCode"),
            true,
            0
        )
        .is_none());
        // No matching token → no offer.
        assert!(
            replacement_decision("hello world", &config, &config.prefs, allowed, true, 0).is_none()
        );
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
