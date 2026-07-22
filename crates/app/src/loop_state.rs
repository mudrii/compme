//! Heartbeat-loop state for `run_loop::run()`, grouped by responsibility.
//!
//! The loop used to bind ~34 `let mut` locals at the top of `run()` and let
//! them sprawl across the ~1,875-line heartbeat. They now live in the structs
//! below — one per responsibility (suggestion lifecycle, focus/context,
//! monitored input, policy edges, settings mirrors, download progress, usage
//! stats, session UI) — and the loop touches them through field paths. The
//! conversion is behavior-identical: bindings moved verbatim, references
//! became field paths, no logic changed.
//!
//! Teardown-order contract. Rust drops locals in reverse declaration order
//! but struct fields in declaration order, so regrouping locals into structs
//! would reorder drops — that only matters for bindings with observable Drop
//! effects. A workspace-wide `impl Drop` scan shows that among the converted
//! bindings NONE has a Drop impl anywhere in its type tree (std containers,
//! `Option`s, `bool`s, and the plain-data structs listed below); their drop
//! order is unobservable in every execution path. The two `let mut` loop
//! bindings that DO have drop-time effects on the outside world stay `run()`
//! locals at their exact original declaration sites:
//!
//! - `model_downloader: Option<model_fetch::ModelDownloader>` — its Drop
//!   closes the request channel so the download worker exits after its
//!   current item (crates/model_fetch/src/lib.rs).
//! - `settings_window: crate::shell::SettingsWindow` — on macOS it holds
//!   `Retained<NSWindow>`/AppKit objects released at drop.
//!
//! Both remain declared after the `RunContext` destructure (as do these
//! structs), so on scope exit — normal return AND panic unwind — every piece
//! here still drops before the instance lock, the focus/caret/accept
//! subscriptions, the engine, and the adapter handle, exactly as before; and
//! `settings_window` still drops before `model_downloader`. The explicit
//! ordered teardown at the end of `run()` (`drop(tray)`, `drop(caret_sub)`,
//! `drop(focus_sub)`, `inference.shutdown()`, `drop(engine)`, `drop(adapter)`)
//! is untouched.
//!
//! No query/mutation helpers live here on purpose: every operation the loop
//! performs on this state already goes through an existing free function
//! taking the individual pieces (`offer_all`, `clear_monitored_state_for_
//! policy_transition`, `latency_sample`, `stats_flush_due`, …). Wrapping those
//! in methods would change their signatures (and their tests) without hiding
//! any new complexity — constructors are the only impls.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use emoji::EmojiPrefs;
use platform::FieldHandle;

use crate::run_loop::{DomainMissNotice, LogSquelch, MonitoredBuffer, PendingMonitoredText};
use crate::status::AppStatus;
use crate::wiring::{FieldTracker, LatestRequest};

/// Suggestion lifecycle: the armed/pending completion slot plus the
/// submit-time bookkeeping that feeds first-suggestion latency (§11 p95 floor).
#[derive(Default)]
pub struct SuggestionState {
    /// Newest pending completion request; superseded by newer offers,
    /// consumed (take) or dropped (clear) at submission/gating time.
    pub latest: LatestRequest,
    /// Submit timestamps keyed by request generation, used to derive
    /// first-suggestion latency when the matching outcome returns.
    pub submit_times: HashMap<u64, u64>,
}

impl SuggestionState {
    pub fn new() -> Self {
        Self {
            latest: LatestRequest::new(),
            submit_times: HashMap::new(),
        }
    }
}

/// Focus/context: the focused field, its caret-diff tracker, and the app /
/// browser-domain context derived from Focus events.
#[derive(Default)]
pub struct FocusContext {
    /// Last-seen value/caret per focused field; successive context reads are
    /// diffed into `Observation`s (reset on focus change).
    pub tracker: FieldTracker,
    /// The currently focused field, if any.
    pub current_field: Option<FieldHandle>,
    /// Whether the engine classified the focused field as an assistant surface.
    pub current_assistant_field: bool,
    /// The most recent focused app key, so settings edges can re-apply per-app
    /// gates without waiting for the next Focus event.
    pub last_app_key: Option<String>,
    /// Browser host for the focused page, cached per Focus: (app key the read
    /// was taken under, extracted host). Populated by the Focus arm's AX read
    /// (is_browser-gated, one round-trip per browser focus); host only, never
    /// the full URL (privacy boundary). `cached_domain` guards consumption on
    /// the app key so a request resolved to a different app never inherits it.
    pub last_domain: Option<(String, String)>,
    /// One-shot inert-rules notice: counts browser-focus detection misses
    /// (c121 transparency, runtime-contingent since c131).
    pub domain_miss_notice: DomainMissNotice,
}

impl FocusContext {
    pub fn new() -> Self {
        Self {
            tracker: FieldTracker::new(),
            current_field: None,
            current_assistant_field: false,
            last_app_key: None,
            last_domain: None,
            domain_miss_notice: DomainMissNotice::default(),
        }
    }
}

/// Monitored typing history (memory collection): the pending-flush queue and
/// the per-field capture buffers. Cleared together on every policy transition
/// (enable toggle, snooze, secure edge, focus change).
#[derive(Default)]
pub struct MonitoredInput {
    /// Captured text awaiting a policy-safe flush into the memory store.
    pub pending_monitored: Vec<PendingMonitoredText>,
    /// Per-field in-flight capture buffers (bounded, boundary-aware).
    pub monitored_buffers: HashMap<FieldHandle, MonitoredBuffer>,
}

/// Policy edge state: the enable/secure snapshots the heartbeat diffs against
/// to fire transition edges (dismiss, persist, secure enter/clear).
pub struct PolicyState {
    /// `flags.enabled` as of the previous heartbeat — the edge detector for
    /// tray/SIGUSR1 enable-disable (persist on change, dismiss on disable).
    pub prev_enabled: bool,
    /// Live secure-input state, re-polled on a wall-clock throttle.
    pub secure: bool,
    /// `secure` as of the previous heartbeat — drives SecureEdge transitions.
    pub prev_secure: bool,
    /// Last secure-input/trust re-poll (monotonic ms); throttles the poll.
    pub last_secure_poll_ms: Option<u64>,
}

impl PolicyState {
    /// `prev_enabled` starts at the configured value so the first heartbeat
    /// does not detect a spurious enable edge (and re-persist it).
    pub fn new(config_enabled: bool) -> Self {
        Self {
            prev_enabled: config_enabled,
            secure: false,
            prev_secure: false,
            last_secure_poll_ms: None,
        }
    }
}

/// Settings-window mirror state: the live values the switch/pane watchers
/// diff against (each watcher persists + applies on the edge), plus the
/// window visibility poll.
pub struct SettingsState {
    /// Live global mid-line default; per-app overrides still derive from it.
    pub global_mid_word: bool,
    /// Emoji on/off edge, tracked separately from the config payload (emoji
    /// is stored as an Option<EmojiPrefs>).
    pub emoji_enabled: bool,
    /// Parsed emoji prefs payload, kept across live off/on cycles.
    pub emoji_prefs: EmojiPrefs,
    /// Last-rendered skin-tone popup index.
    pub emoji_skin_tone_index: usize,
    /// Last-rendered gender popup index.
    pub emoji_gender_index: usize,
    /// Launch-at-login as last applied; a rejected OS mutation restores it.
    pub current_launch_at_login: bool,
    /// The app ids behind the Apps rows as last rendered (index == row).
    pub apps_ids: Vec<String>,
    /// Window visibility as of the previous poll — the visible→hidden edge
    /// demotes the activation policy back to Accessory exactly once.
    pub settings_was_visible: bool,
    /// Visible-only Setup re-probe cadence (setup_poll_due).
    pub last_setup_poll_ms: Option<u64>,
}

impl SettingsState {
    /// Seed the mirrors from the loaded config (verbatim the values the
    /// pre-struct locals started with); pane state starts empty/hidden.
    pub fn new(
        global_mid_word: bool,
        emoji_enabled: bool,
        emoji_prefs: EmojiPrefs,
        emoji_skin_tone_index: usize,
        emoji_gender_index: usize,
        current_launch_at_login: bool,
    ) -> Self {
        Self {
            global_mid_word,
            emoji_enabled,
            emoji_prefs,
            emoji_skin_tone_index,
            emoji_gender_index,
            current_launch_at_login,
            apps_ids: Vec::new(),
            settings_was_visible: false,
            last_setup_poll_ms: None,
        }
    }
}

/// Model-download progress. The downloader itself stays a `run()` local (its
/// Drop closes the worker channel — see the module doc); this is the polled
/// status + the one-line-per-transition log cursor.
#[derive(Default)]
pub struct DownloadState {
    /// Shared status of the in-flight (or last) download; polled per heartbeat.
    pub model_download_status: Option<Arc<model_fetch::DownloadStatus>>,
    /// Log cursor: 0=idle 1=running 2=terminal.
    pub model_download_logged: u8,
}

/// Local usage stats (§11/§16) plus the periodic lifetime-flush bookkeeping.
/// Accepts/dismisses are recorded from the host inputs; Shown/Superseded are
/// drained from the engine each loop turn. The menu-bar display + persistence
/// are A3 surfaces.
#[derive(Default)]
pub struct UsageStats {
    /// The 30-day window + grow-only session totals.
    pub usage: stats::Stats,
    /// Last periodic flush (monotonic ms); cadence STATS_FLUSH_INTERVAL_MS.
    pub last_stats_flush_ms: Option<u64>,
    /// Session totals as of the last successful flush — the dirty check.
    pub last_flushed_session: stats::SessionTotals,
}

/// Session UI housekeeping: one-shot onboarding hints, the read-error log
/// squelch, and tray-render dedup (only touch AppKit on real change).
#[derive(Default)]
pub struct SessionUi {
    /// Apps that already got their one-shot compat/setup guidance.
    pub hinted_apps: HashSet<String>,
    /// Squelch for repeating read_context failures (log only on change).
    pub read_err_squelch: LogSquelch,
    /// Last rendered (status, enabled, snoozed) triple — tray redraw dedup.
    pub last_render: Option<(AppStatus, bool, bool)>,
    /// Last rendered 30-day usage line — tray stats-line dedup.
    pub last_stats_line: Option<String>,
}
