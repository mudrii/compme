//! Cross-platform contract shared by the pure engine and platform adapters.

use std::sync::Arc;
use std::time::Duration;

/// Application identifier (e.g. a macOS bundle id). Stable for the lifetime of
/// a running app; not guaranteed unique across simultaneous instances — pair
/// with `pid` where instance identity matters.
pub type AppId = String;
/// Focus-change callback. Adapters may invoke it from an internal event thread:
/// it must be cheap, must not block, and must not call back into the adapter
/// (re-entrancy is not part of the contract).
pub type FocusCallback = Arc<dyn Fn(FieldHandle) + Send + Sync + 'static>;
/// Caret-geometry callback (`None` rect = geometry unavailable). Same threading
/// constraints as [`FocusCallback`].
pub type CaretCallback = Arc<dyn Fn(FieldHandle, Option<ScreenRect>) + Send + Sync + 'static>;
/// Accept-tap callback delivering [`TapControl`] signals. Same threading
/// constraints as [`FocusCallback`]; it runs on the key-event path, so any
/// delay here is visible typing latency.
pub type AcceptCallback = Arc<dyn Fn(TapControl) + Send + Sync + 'static>;

/// Identity of one editable field. Two handles refer to the same live field
/// only when *all* fields compare equal — adapters must bump `generation` when
/// the underlying element is replaced, so operations against an old handle fail
/// with [`PlatformError::StaleField`] instead of writing into the wrong element.
/// Holders must treat a stale handle as dead and wait for the next focus event.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FieldHandle {
    pub app: AppId,
    pub pid: Option<u32>,
    pub element_id: String,
    pub generation: u64,
}

/// Point-in-time snapshot of a field's text around the caret. `caret` and
/// `selection` are offsets in the units named by `offset_encoding` — consumers
/// must convert (the `context` crate's helpers take Unicode-scalar offsets)
/// before any indexing; mixing units silently corrupts caret math on non-ASCII
/// text. The snapshot is not live: it may be stale by the time it is read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextContext {
    pub left: String,
    pub right: String,
    pub selection: Option<TextRange>,
    pub caret: usize,
    pub source: ContextSource,
    pub field_id: FieldHandle,
    pub offset_encoding: OffsetEncoding,
}

/// Half-open `[start, end)` selection range, in the same `offset_encoding`
/// units as the [`TextContext`] that carries it. Producers must keep
/// `start <= end` (a caret-only selection is `start == end`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextRange {
    pub start: usize,
    pub end: usize,
}

/// How a [`TextContext`] was obtained — a fidelity hint, not a behavior switch.
/// Only `Accessibility` reads are authoritative; `Clipboard`/`Synthetic`
/// snapshots may diverge from the field and `Unknown` carries no guarantee.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextSource {
    Accessibility,
    Clipboard,
    Synthetic,
    Unknown,
}

/// The unit in which [`TextContext`] offsets (`caret`, `selection`) count.
/// Consumers must convert to their own unit before indexing — the variants
/// disagree on any non-ASCII text, so an unconverted offset is a latent
/// corruption bug, not an error the type system catches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OffsetEncoding {
    /// Offsets count UTF-8 bytes. Valid offsets always lie on `char`
    /// boundaries; a multibyte scalar advances the offset by its byte length.
    Utf8Bytes,
    /// Offsets count UTF-16 code units (AppKit/Chromium native). Astral-plane
    /// scalars (emoji) count as **two** units — the usual off-by-one source.
    Utf16CodeUnits,
    /// Offsets count Unicode scalar values (Rust `char`s). This is the unit
    /// the `context` crate's caret helpers require.
    UnicodeScalars,
    /// Offsets count user-perceived characters (grapheme clusters). One
    /// cluster may span many scalars (combining marks, ZWJ emoji), so this is
    /// the coarsest unit — never index a Rust string with it directly.
    GraphemeClusters,
}

/// Rectangle in screen points. Whether the origin is the global screen (vs a
/// window/display-local space) is reported per field via
/// `Capabilities::coords_global_screen`; callers must check before placing
/// overlays on multi-display topologies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Half-open Unicode-scalar range for a word correction in `left + right`
/// context text. Platform adapters convert this to their native offset units at
/// the trait boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CorrectionRange {
    pub start: usize,
    pub end: usize,
}

/// What an adapter can do with one specific field, probed at focus time. This
/// is the sole input to [`ux_mode`]; callers must re-probe on every focus and
/// secure-state change rather than cache across fields — `secure`/
/// `security_state` gate a privacy invariant, not an optimization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub readable_text: bool,
    pub readable_caret: bool,
    pub writable: bool,
    pub secure: bool,
    pub security_state: SecurityState,
    pub toolkit: Toolkit,
    pub multiline: bool,
    pub insert_strategy: InsertStrategy,
    pub accept_intercept: KeyInterceptMode,
    pub overlay_at_caret: OverlayPlacement,
    pub coords_global_screen: bool,
}

/// Secure-input status. `SecureField` (the focused field is a password field)
/// and `SecureInputEnabled` (system-wide secure input) both force
/// `UxMode::Blocked`: no text from such a field may ever reach a prompt or log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecurityState {
    Normal,
    SecureField,
    SecureInputEnabled,
    Unknown,
}

/// UI toolkit detected behind the focused field — a strategy hint (which
/// insert/overlay paths tend to work), never a correctness gate on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Toolkit {
    AppKit,
    UIKit,
    Chromium,
    WebKit,
    Electron,
    Terminal,
    Unknown(String),
}

/// How accepted text enters the field. Only `AxSet` can range-replace (delete
/// left of the caret atomically) — replacement suggestions are gated on it.
/// `None` means the field cannot be written at all (`UxMode::Unsupported`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertStrategy {
    AxSet,
    SyntheticKeys,
    Clipboard,
    ImeCommit,
    None,
}

/// How the accept/dismiss keys are intercepted for a field. `HotkeyOnly`
/// demotes the UX to `UxMode::Hotkey` (no transparent Tab interception);
/// `None` leaves accept keys entirely to the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyInterceptMode {
    CgEventTap,
    CarbonHotkey,
    LowLevelHook,
    XGrabKey,
    FocusScopedInhibit,
    ImeOwnsKey,
    HotkeyOnly,
    None,
}

/// What one accept keystroke commits: the whole suggestion (`Full`) or only
/// its next word (`Word`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptAction {
    Full,
    Word,
    Correction,
}

/// A control signal delivered by the accept key-interception tap. Either an
/// accept (Tab/grave), a dismiss+suppress (Esc), a candidate cycle, or one of
/// the always-on (global) shortcut actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapControl {
    Accept(AcceptAction),
    Dismiss,
    /// Rotate to the next candidate (multi-candidate cycle key).
    Cycle,
    /// An always-on (global) hotkey fired — re-show the pending suggestion or
    /// toggle suggestions for the focused app / globally. Distinct from the
    /// accept variants because it acts even when no suggestion is showing.
    Shortcut(ShortcutAction),
}

/// The three always-on (global) shortcut actions an adapter can deliver through
/// [`TapControl::Shortcut`]. `ForceActivate` re-shows the current pending
/// suggestion (no fresh inference); `ToggleApp` flips suggestions for the
/// focused app; `ToggleGlobal` flips the global enabled default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutAction {
    ForceActivate,
    ToggleApp,
    ToggleGlobal,
    GrammarCheck,
}

/// How a ghost overlay can be anchored for a field. `None` means no
/// caret-anchored placement exists, which (with a readable caret or not)
/// pushes [`ux_mode`] to `Popup` rather than `Inline`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayPlacement {
    NativePanel,
    LayeredWindow,
    OverrideRedirect,
    LayerShell,
    ImeCandidate,
    None,
}

/// The suggestion UX a field supports, derived by [`ux_mode`]. `Blocked` is a
/// hard privacy gate (secure field/input) that callers must honor before any
/// other consideration — never request, read, or show anything in a `Blocked`
/// field. `Unsupported` means the field cannot be read or written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UxMode {
    Inline,
    Popup,
    Hotkey,
    Unsupported,
    Blocked,
}

/// Static description of the host (OS, version) — diagnostics and strategy
/// selection only; per-field decisions belong in [`Capabilities`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Environment {
    pub os: OperatingSystem,
    pub version: String,
}

/// Host operating system; `Unknown` carries the raw name so adapters never
/// have to lie about an unrecognized platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperatingSystem {
    Macos,
    Windows,
    Linux,
    Unknown(String),
}

/// Per-call adapter failure. All variants are non-fatal and per-operation: the
/// engine treats any error as "no suggestion this turn", so adapters must leave
/// the field unmodified when returning one. `StaleField` means the
/// [`FieldHandle`] is dead — retrying with the same handle will keep failing;
/// callers must wait for a fresh focus event instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlatformError {
    PermissionMissing { permission: String },
    SecureInput { state: SecurityState },
    CannotComplete { reason: String },
    UnsupportedField { reason: String },
    Timeout,
    StaleField,
    AppExited { app: AppId },
}

impl std::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlatformError::PermissionMissing { permission } => {
                write!(f, "required permission missing: {permission}")
            }
            PlatformError::SecureInput { state } => {
                write!(f, "secure input active: {state:?}")
            }
            PlatformError::CannotComplete { reason } => {
                write!(f, "cannot complete: {reason}")
            }
            PlatformError::UnsupportedField { reason } => {
                write!(f, "unsupported field: {reason}")
            }
            PlatformError::Timeout => write!(f, "platform operation timed out"),
            PlatformError::StaleField => write!(f, "field is stale"),
            PlatformError::AppExited { app } => write!(f, "app exited: {app}"),
        }
    }
}

impl std::error::Error for PlatformError {}

/// RAII handle for an adapter event subscription: dropping it runs the
/// registered cancel exactly once. Adapters must supply a cancel that is safe
/// to run from any thread and that stops further callback delivery (callbacks
/// already in flight may still complete).
pub struct Subscription {
    id: u64,
    cancel: Option<Box<dyn FnOnce() + Send + 'static>>,
}

/// [`Subscription`] for the accept-key tap, bundling the control hooks the
/// engine must call to keep the tap honest: the tap may swallow accept/dismiss
/// keys only while a suggestion is visible, so callers must keep
/// `set_suggestion_visible` strictly in sync with the overlay — a stale `true`
/// eats the user's Tab keystrokes.
pub struct AcceptSubscription {
    subscription: Subscription,
    set_suggestion_visible: Arc<dyn Fn(bool) -> Result<(), PlatformError> + Send + Sync + 'static>,
    hide_suggestion_after:
        Arc<dyn Fn(Duration) -> Result<(), PlatformError> + Send + Sync + 'static>,
    set_accept_action:
        Arc<dyn Fn(Option<AcceptAction>) -> Result<(), PlatformError> + Send + Sync + 'static>,
    /// Recorder 5b: drop + re-register the platform's accept tap against the
    /// current keymap. Builder-attached (`with_rearm`) so the constructor's
    /// signature — load-bearing for 7 test sites — stays unchanged; the
    /// default is a successful no-op (platforms without live rebind).
    rearm: Arc<dyn Fn() -> Result<(), PlatformError> + Send + Sync + 'static>,
}

impl Subscription {
    pub fn new(id: u64) -> Self {
        Self { id, cancel: None }
    }

    pub fn with_cancel(id: u64, cancel: impl FnOnce() + Send + 'static) -> Self {
        Self {
            id,
            cancel: Some(Box::new(cancel)),
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }
}

impl AcceptSubscription {
    pub fn new(
        subscription: Subscription,
        set_suggestion_visible: impl Fn(bool) -> Result<(), PlatformError> + Send + Sync + 'static,
        hide_suggestion_after: impl Fn(Duration) -> Result<(), PlatformError> + Send + Sync + 'static,
        set_accept_action: impl Fn(Option<AcceptAction>) -> Result<(), PlatformError>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            subscription,
            set_suggestion_visible: Arc::new(set_suggestion_visible),
            hide_suggestion_after: Arc::new(hide_suggestion_after),
            set_accept_action: Arc::new(set_accept_action),
            rearm: Arc::new(|| Ok(())),
        }
    }

    /// Attach the platform's live re-arm hook (recorder 5b). The hook must
    /// drop the armed accept tap and re-register it against the CURRENT
    /// keymap; it is called from the host loop only (never the platform's
    /// own worker thread — the macOS impl would deadlock on its own queue).
    pub fn with_rearm(
        mut self,
        rearm: impl Fn() -> Result<(), PlatformError> + Send + Sync + 'static,
    ) -> Self {
        self.rearm = Arc::new(rearm);
        self
    }

    /// Drop + re-register the accept tap against the current keymap (live
    /// rebind). No-op `Ok(())` on platforms without the hook. Call from the
    /// host loop only — never from the platform's own worker thread (the
    /// macOS impl blocks on its own queue and would deadlock). Callers must
    /// NOT persist a rebind whose re-arm returned `Err` — the registered
    /// keys and the persisted config would desync.
    pub fn rearm_accept_tap(&self) -> Result<(), PlatformError> {
        (self.rearm)()
    }

    pub fn id(&self) -> u64 {
        self.subscription.id()
    }

    /// Tell the tap whether a suggestion is on screen. Must be called on every
    /// show/hide transition; the tap passes keys through while `false`.
    pub fn set_suggestion_visible(&self, visible: bool) -> Result<(), PlatformError> {
        (self.set_suggestion_visible)(visible)
    }

    /// Schedule the tap to treat the suggestion as hidden after `delay` — a
    /// failsafe so a missed hide cannot swallow keys indefinitely.
    pub fn hide_suggestion_after(&self, delay: Duration) -> Result<(), PlatformError> {
        (self.hide_suggestion_after)(delay)
    }

    /// Set which [`AcceptAction`] the accept key reports (`None` clears the
    /// override). Takes effect for subsequent keystrokes, not ones in flight.
    pub fn set_accept_action(&self, action: Option<AcceptAction>) -> Result<(), PlatformError> {
        (self.set_accept_action)(action)
    }
}

impl std::fmt::Debug for AcceptSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcceptSubscription")
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("id", &self.id)
            .field("has_cancel", &self.cancel.is_some())
            .finish()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel();
        }
    }
}

/// Receipt for a successful insert: the byte/char counts actually written and
/// the strategy that succeeded (which may differ from the requested one if the
/// adapter fell back). `chars` counts Unicode scalars — hosts use it to advance
/// caret math, so it must match the inserted text exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inserted {
    pub bytes: usize,
    pub chars: usize,
    pub strategy: InsertStrategy,
}

/// The platform seam the pure engine drives. Obligations on implementors:
///
/// - **Threading.** `Send + Sync` is load-bearing: methods are called from the
///   engine's run loop while subscription callbacks fire from adapter-internal
///   threads, so all internal state needs its own synchronization.
/// - **Blocking.** Methods are synchronous but must not block unboundedly —
///   return [`PlatformError::Timeout`] instead of hanging the run loop.
/// - **Error semantics.** Every error is per-call and recoverable (the engine
///   skips the suggestion and moves on); a failed mutation must leave the
///   field unmodified — no partial inserts.
pub trait PlatformAdapter: Send + Sync {
    /// Static host description. Must be cheap and infallible.
    fn environment(&self) -> Environment;
    /// Register for focus changes. The callback may fire from an internal
    /// thread (see [`FocusCallback`]); dropping the returned [`Subscription`]
    /// must stop delivery.
    fn subscribe_focus(&self, cb: FocusCallback) -> Result<Subscription, PlatformError>;
    /// Register for caret geometry updates. Same threading/cancel contract as
    /// [`subscribe_focus`](Self::subscribe_focus).
    fn subscribe_caret(&self, cb: CaretCallback) -> Result<Subscription, PlatformError>;
    /// Install the accept/dismiss key tap. Implementors must swallow keys only
    /// while the engine has reported a visible suggestion through the returned
    /// handle's `set_suggestion_visible` — anything else eats normal typing.
    fn subscribe_accept(&self, cb: AcceptCallback) -> Result<AcceptSubscription, PlatformError>;
    /// The frontmost app, if determinable. Must not block.
    fn front_app(&self) -> Option<AppId>;
    /// Probe what is possible for `field`. Callers must re-probe per focus and
    /// on secure-state changes — capabilities are per-field, not per-app.
    fn capabilities(&self, field: &FieldHandle) -> Result<Capabilities, PlatformError>;
    /// Snapshot the field's text around the caret. Offsets in the result are
    /// in its `offset_encoding` units — callers convert before scalar-based
    /// use. An `Err` means "no context this turn", never a fatal condition.
    fn read_context(&self, field: &FieldHandle) -> Result<TextContext, PlatformError>;
    /// Caret bounds in screen coordinates. `Ok(None)` means the field is valid
    /// but exposes no caret geometry (popup-mode placement applies).
    fn caret_rect(&self, field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError>;
    /// A fallback anchor rect for popup-mode placement when no caret geometry is
    /// available (e.g. the focused field exposes no caret bounds). Typically the
    /// focused window frame. Defaults to `None` for adapters that cannot supply
    /// one.
    fn popup_anchor(&self, _field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
        Ok(None)
    }
    /// Best-effort URL of the focused window's web page, for per-domain
    /// gating. `Ok(None)` when the platform/app exposes none — the common
    /// case, and fail-OPEN by design (no domain = no domain gating), which
    /// is why a default here is safe where `insert_replacing`'s was not
    /// (a missing insert override silently corrupted output; a missing URL
    /// source merely skips an optional gate).
    fn focused_page_url(&self, _field: &FieldHandle) -> Result<Option<String>, PlatformError> {
        Ok(None)
    }
    /// Bounds for a correction range. `Ok(None)` means the field is live and the
    /// range is valid, but the platform cannot expose range geometry; hosts may
    /// fall back to caret/popup anchors. An invalid/unimplemented range seam must
    /// fail closed with `Err`, not degrade into a caret-anchored correction.
    fn text_range_rect(
        &self,
        _field: &FieldHandle,
        _range: CorrectionRange,
    ) -> Result<Option<ScreenRect>, PlatformError> {
        Err(PlatformError::UnsupportedField {
            reason: "correction range geometry unsupported".into(),
        })
    }
    /// Insert `text` at the caret using `strategy`. All-or-nothing: on `Err`
    /// the field must be unchanged. The caller guarantees the field was
    /// validated via [`capabilities`](Self::capabilities) and is writable.
    fn insert(
        &self,
        field: &FieldHandle,
        text: &str,
        strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError>;
    /// Insert `text` after deleting `replace_left` characters immediately to the
    /// left of the caret — a *replacement* (emoji/typo/US→UK spelling). Adapters
    /// that can range-replace (AxSet) honor the deletion; ones that cannot
    /// (yet) must still implement this explicitly — typically delegating to
    /// [`insert`](Self::insert) — so the degradation is a stated decision.
    ///
    /// `replace_left == 0` MUST behave exactly as [`insert`](Self::insert): a
    /// pure append at the caret with no deletion. Pinning the zero case here
    /// keeps the (future) platform adapters from each inventing their own answer.
    ///
    /// REQUIRED (no default) on purpose: this used to default to an append-only
    /// `insert` delegate, and a forwarding wrapper (`SharedAdapter`) silently
    /// inherited it — live result was `:smile😄` (emoji appended, typed token
    /// never deleted). A missing implementation must be a compile error, not a
    /// silent behavior downgrade.
    fn insert_replacing(
        &self,
        field: &FieldHandle,
        text: &str,
        replace_left: usize,
        strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError>;
    /// Replace exactly `range` with `text`, but only if the current field text at
    /// `range` still equals `expected_text`. Adapters that do not support
    /// arbitrary range replacement must fail closed rather than approximate with
    /// a left-of-caret deletion.
    fn insert_replacing_range(
        &self,
        _field: &FieldHandle,
        _expected_text: &str,
        _text: &str,
        _range: CorrectionRange,
        _strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        Err(PlatformError::UnsupportedField {
            reason: "range replacement unsupported".into(),
        })
    }
}

/// Renders the ghost-text overlay. Deliberately not `Send`/`Sync`:
/// implementations wrap platform UI objects and must be driven from the UI
/// thread only. A failed `show_ghost` must be reconciled by the host (hide +
/// retract the shown stat) — the engine assumes an emitted ghost is on screen.
pub trait OverlayPresenter {
    /// Show (or re-anchor) the ghost at `rect` with `text`, replacing any
    /// previous ghost. `rect` is in the adapter's screen coordinate space.
    fn show_ghost(&mut self, rect: ScreenRect, text: &str) -> Result<(), PlatformError>;
    /// Show a correction underline/banner at `rect`. Platforms without a
    /// correction presenter must fail closed rather than silently rendering a
    /// ghost-like correction that cannot be accepted safely.
    fn show_correction(
        &mut self,
        _rect: ScreenRect,
        _suggestion: &str,
    ) -> Result<(), PlatformError> {
        Err(PlatformError::UnsupportedField {
            reason: "correction presentation unsupported".into(),
        })
    }
    /// Change the text of the currently shown ghost without re-anchoring.
    /// Callers must have a ghost showing (a successful `show_ghost` first).
    fn update_ghost(&mut self, text: &str) -> Result<(), PlatformError>;
    /// Remove the ghost. Must be idempotent — safe when nothing is showing.
    fn hide(&mut self) -> Result<(), PlatformError>;
}

/// Derive the suggestion UX for a field from its probed capabilities. The
/// precedence is a contract callers rely on: secure ⇒ `Blocked` always wins
/// (no completion may ever be requested), then unreadable/unwritable/no insert
/// strategy ⇒ `Unsupported`, then `HotkeyOnly` interception ⇒ `Hotkey`, then
/// caret + overlay placement pick `Inline` vs `Popup`. Pure — cheap enough to
/// re-derive on every event rather than cache.
pub fn ux_mode(capabilities: &Capabilities) -> UxMode {
    if capabilities.secure
        || matches!(
            capabilities.security_state,
            SecurityState::SecureField | SecurityState::SecureInputEnabled
        )
    {
        return UxMode::Blocked;
    }

    if !capabilities.readable_text
        || !capabilities.writable
        || capabilities.insert_strategy == InsertStrategy::None
    {
        return UxMode::Unsupported;
    }

    if capabilities.accept_intercept == KeyInterceptMode::HotkeyOnly {
        return UxMode::Hotkey;
    }

    if capabilities.readable_caret && capabilities.overlay_at_caret != OverlayPlacement::None {
        UxMode::Inline
    } else {
        UxMode::Popup
    }
}

/// A boolean env var is ON when present and not an explicit off-value
/// (`0`/`false`/`off`/`no`/empty, case-insensitive). A present non-UTF-8 value
/// counts as on. The project's fail-safe-on convention: `COMPME_DEBUG=0`
/// silences instead of enabling. (Feature-flag vars use an ON-allow-list — a
/// separate convention.)
pub fn env_flag_on(value: Option<&std::ffi::OsStr>) -> bool {
    match value {
        None => false,
        Some(v) => match v.to_str() {
            None => true,
            Some(s) => !matches!(
                s.trim().to_ascii_lowercase().as_str(),
                "" | "0" | "false" | "off" | "no"
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn env_flag_on_treats_off_values_as_disabled() {
        use std::ffi::OsStr;
        // Unset → off. Explicit off-values (case-insensitive, trimmed) → off, so
        // COMPME_DEBUG=0 silences instead of enabling.
        assert!(!env_flag_on(None));
        for off in ["0", "false", "FALSE", "off", "no", "", " no "] {
            assert!(!env_flag_on(Some(OsStr::new(off))), "{off:?} should be off");
        }
        // Any other present value → on.
        for on in ["1", "true", "yes", "verbose"] {
            assert!(env_flag_on(Some(OsStr::new(on))), "{on:?} should be on");
        }
        // A non-UTF-8 value is present and not an off-token, so it reads as on.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let non_utf8 = OsStr::from_bytes(&[0xff]);
            assert!(env_flag_on(Some(non_utf8)), "non-UTF-8 value should be on");
        }
    }

    fn caps() -> Capabilities {
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

    #[test]
    fn secure_is_always_blocked() {
        let mut c = caps();
        c.secure = true;
        c.security_state = SecurityState::SecureField;

        assert_eq!(ux_mode(&c), UxMode::Blocked);
    }

    #[test]
    fn global_secure_input_is_blocked() {
        let mut c = caps();
        c.security_state = SecurityState::SecureInputEnabled;

        assert_eq!(ux_mode(&c), UxMode::Blocked);
    }

    #[test]
    fn secure_blocks_even_when_otherwise_unsupported() {
        // The secure gate is checked BEFORE the unsupported gate, so a field
        // that is both secure AND otherwise unsupported (no insert strategy,
        // not readable) must resolve to Blocked — never Unsupported.
        let mut c = caps();
        c.secure = true;
        c.security_state = SecurityState::SecureField;
        c.insert_strategy = InsertStrategy::None;
        c.readable_text = false;

        assert_eq!(ux_mode(&c), UxMode::Blocked);
    }

    #[test]
    fn secure_input_enabled_blocks_even_when_otherwise_unsupported() {
        // Same precedence via the global SecureInputEnabled signal: the secure
        // gate wins over the unsupported gate.
        let mut c = caps();
        c.security_state = SecurityState::SecureInputEnabled;
        c.insert_strategy = InsertStrategy::None;
        c.readable_text = false;

        assert_eq!(ux_mode(&c), UxMode::Blocked);
    }

    #[test]
    fn full_caps_is_inline() {
        assert_eq!(ux_mode(&caps()), UxMode::Inline);
    }

    #[test]
    fn no_caret_but_writable_is_popup() {
        let mut c = caps();
        c.readable_caret = false;
        c.overlay_at_caret = OverlayPlacement::None;

        assert_eq!(ux_mode(&c), UxMode::Popup);
    }

    #[test]
    fn caret_readable_but_no_overlay_placement_is_popup() {
        // Isolates the overlay_at_caret half of the inline gate: the caret is
        // readable but there is no caret-anchored overlay placement, so we fall
        // through to Popup rather than Inline. Everything else stays inline-capable.
        let mut c = caps();
        c.readable_caret = true;
        c.overlay_at_caret = OverlayPlacement::None;

        assert_eq!(ux_mode(&c), UxMode::Popup);
    }

    #[test]
    fn ux_mode_overlay_without_readable_caret_is_popup() {
        // Isolates the readable_caret half of the inline AND-gate: a
        // caret-anchored overlay placement IS present, but the caret is not
        // readable, so we fall through to Popup rather than Inline. Pins the
        // complement of `caret_readable_but_no_overlay_placement_is_popup`; a
        // mutation that flipped the inline `&&` to `||` (or dropped the
        // readable_caret term) would wrongly return Inline here and fail.
        let mut c = caps();
        c.readable_caret = false;
        c.overlay_at_caret = OverlayPlacement::NativePanel;
        c.secure = false;
        c.security_state = SecurityState::Normal;

        assert_eq!(ux_mode(&c), UxMode::Popup);
    }

    #[test]
    fn secure_flag_alone_with_normal_state_is_blocked() {
        // Isolates the left side of the secure `||`: the secure flag is set even
        // though the security_state is Normal, so the field is Blocked.
        let mut c = caps();
        c.secure = true;
        c.security_state = SecurityState::Normal;

        assert_eq!(ux_mode(&c), UxMode::Blocked);
    }

    #[test]
    fn unknown_security_state_alone_is_not_blocked() {
        // Pins the Unknown→non-blocking boundary: the secure guard only fires for
        // SecureField | SecureInputEnabled, so with secure=false and an Unknown
        // security_state the field stays fully usable (Inline, like Normal). A
        // regression that started treating Unknown as Blocked would fail here.
        let mut c = caps();
        c.secure = false;
        c.security_state = SecurityState::Unknown;

        assert_eq!(ux_mode(&c), UxMode::Inline);
    }

    #[test]
    fn hotkey_only_intercept_is_hotkey_mode() {
        let mut c = caps();
        c.accept_intercept = KeyInterceptMode::HotkeyOnly;

        assert_eq!(ux_mode(&c), UxMode::Hotkey);
    }

    #[test]
    fn non_hotkey_intercept_variants_do_not_resolve_to_hotkey_mode() {
        // The Hotkey gate is an equality check against HotkeyOnly, so EVERY other
        // KeyInterceptMode variant must fall through it. With otherwise-full
        // (inline-capable) caps, each non-HotkeyOnly variant resolves to Inline —
        // a mutation that widened the gate (e.g. `!=` flipped, or a matches! with
        // extra variants) would surface Hotkey here and fail.
        for intercept in [
            KeyInterceptMode::CgEventTap,
            KeyInterceptMode::CarbonHotkey,
            KeyInterceptMode::LowLevelHook,
            KeyInterceptMode::XGrabKey,
            KeyInterceptMode::FocusScopedInhibit,
            KeyInterceptMode::ImeOwnsKey,
            KeyInterceptMode::None,
        ] {
            // Exhaustive, wildcard-free: a new KeyInterceptMode variant breaks
            // compilation here and forces the array above to be updated too.
            // (HotkeyOnly is intentionally excluded from the array — it is the
            // one variant that DOES resolve to Hotkey, pinned by
            // `hotkey_only_intercept_is_hotkey_mode`.)
            match intercept {
                KeyInterceptMode::CgEventTap
                | KeyInterceptMode::CarbonHotkey
                | KeyInterceptMode::LowLevelHook
                | KeyInterceptMode::XGrabKey
                | KeyInterceptMode::FocusScopedInhibit
                | KeyInterceptMode::ImeOwnsKey
                | KeyInterceptMode::HotkeyOnly
                | KeyInterceptMode::None => {}
            }
            let mut c = caps();
            c.accept_intercept = intercept;

            assert_eq!(
                ux_mode(&c),
                UxMode::Inline,
                "non-HotkeyOnly intercept {intercept:?} must not demote to Hotkey"
            );
        }
    }

    #[test]
    fn secure_field_blocks_even_with_hotkey_only_intercept() {
        // The secure guard runs before the intercept check, so a secure field
        // is Blocked regardless of a HotkeyOnly intercept that would otherwise
        // classify as Hotkey.
        let mut c = caps();
        c.secure = true;
        c.security_state = SecurityState::SecureField;
        c.accept_intercept = KeyInterceptMode::HotkeyOnly;

        assert_eq!(ux_mode(&c), UxMode::Blocked);
    }

    #[test]
    fn unsupported_wins_over_hotkey_only_intercept() {
        // The Unsupported gate (no insert strategy) runs before the intercept
        // check, so a field that is both unwritable-by-strategy AND HotkeyOnly
        // classifies as Unsupported, not Hotkey. Mirrors the secure/hotkey
        // ordering test one precedence level down.
        let mut c = caps();
        c.insert_strategy = InsertStrategy::None;
        c.accept_intercept = KeyInterceptMode::HotkeyOnly;

        assert_eq!(ux_mode(&c), UxMode::Unsupported);
    }

    #[test]
    fn not_readable_is_unsupported() {
        let mut c = caps();
        c.readable_text = false;
        c.readable_caret = false;
        c.writable = false;

        assert_eq!(ux_mode(&c), UxMode::Unsupported);
    }

    #[test]
    fn readable_not_writable_is_unsupported() {
        let mut c = caps();
        c.writable = false;

        assert_eq!(ux_mode(&c), UxMode::Unsupported);
    }

    #[test]
    fn no_insert_strategy_is_unsupported() {
        let mut c = caps();
        c.insert_strategy = InsertStrategy::None;

        assert_eq!(ux_mode(&c), UxMode::Unsupported);
    }

    #[test]
    fn subscription_drop_runs_cancel_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_in_cancel = Arc::clone(&calls);
        let subscription = Subscription::with_cancel(7, move || {
            calls_in_cancel.fetch_add(1, Ordering::Relaxed);
        });

        assert_eq!(subscription.id(), 7);
        drop(subscription);

        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn subscription_without_cancel_drops_safely() {
        let subscription = Subscription::new(3);
        assert_eq!(subscription.id(), 3);
        drop(subscription);
    }

    #[test]
    fn secure_field_state_alone_is_blocked() {
        let mut c = caps();
        c.secure = false;
        c.security_state = SecurityState::SecureField;

        assert_eq!(ux_mode(&c), UxMode::Blocked);
    }

    #[test]
    fn accept_subscription_forwards_each_callback_and_id() {
        let visible = Arc::new(AtomicUsize::new(0));
        let hide = Arc::new(AtomicUsize::new(0));
        let action = Arc::new(AtomicUsize::new(0));
        let v = Arc::clone(&visible);
        let h = Arc::clone(&hide);
        let a = Arc::clone(&action);

        let subscription = AcceptSubscription::new(
            Subscription::new(9),
            move |_visible| {
                v.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
            move |_delay| {
                h.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
            move |_action| {
                a.fetch_add(1, Ordering::Relaxed);
                Err(PlatformError::Timeout)
            },
        );

        assert_eq!(subscription.id(), 9);
        assert!(subscription.set_suggestion_visible(true).is_ok());
        assert!(subscription
            .hide_suggestion_after(std::time::Duration::from_millis(5))
            .is_ok());
        // The set_accept_action closure returns Err — forwarding must surface it.
        assert!(subscription
            .set_accept_action(Some(AcceptAction::Word))
            .is_err());

        assert_eq!(visible.load(Ordering::Relaxed), 1);
        assert_eq!(hide.load(Ordering::Relaxed), 1);
        assert_eq!(action.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn accept_subscription_forwards_set_suggestion_visible_err() {
        // Only set_accept_action's err path was proven; set_suggestion_visible
        // must surface its closure's Err too — a swallowed error here would let
        // the host believe the tap state was updated when it was not.
        let subscription = AcceptSubscription::new(
            Subscription::new(11),
            |_visible| Err(PlatformError::Timeout),
            |_delay| Ok(()),
            |_action| Ok(()),
        );

        assert!(subscription.set_suggestion_visible(true).is_err());
    }

    #[test]
    fn accept_subscription_forwards_hide_suggestion_after_err() {
        // Companion to the set_suggestion_visible err test: the hide failsafe's
        // Err must surface so a missed-hide schedule failure is observable.
        let subscription = AcceptSubscription::new(
            Subscription::new(12),
            |_visible| Ok(()),
            |_delay| Err(PlatformError::Timeout),
            |_action| Ok(()),
        );

        assert!(subscription
            .hide_suggestion_after(std::time::Duration::from_millis(5))
            .is_err());
    }

    #[test]
    fn rearm_defaults_to_noop_and_with_rearm_forwards_including_err() {
        // Recorder 5b: the rearm hook is builder-attached so the 7 existing
        // AcceptSubscription::new construction sites stay untouched — a
        // subscription without the hook must rearm as a successful no-op.
        let plain =
            AcceptSubscription::new(Subscription::new(1), |_| Ok(()), |_| Ok(()), |_| Ok(()));
        assert!(plain.rearm_accept_tap().is_ok());

        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        let hooked =
            AcceptSubscription::new(Subscription::new(2), |_| Ok(()), |_| Ok(()), |_| Ok(()))
                .with_rearm(move || {
                    c.fetch_add(1, Ordering::Relaxed);
                    Err(PlatformError::Timeout)
                });
        // Forwarding surfaces the platform's error (the caller must NOT
        // persist a keymap the re-arm failed to register).
        assert!(hooked.rearm_accept_tap().is_err());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    struct UnsupportedCorrectionOverlay;

    impl OverlayPresenter for UnsupportedCorrectionOverlay {
        fn show_ghost(&mut self, _rect: ScreenRect, _text: &str) -> Result<(), PlatformError> {
            Ok(())
        }

        fn update_ghost(&mut self, _text: &str) -> Result<(), PlatformError> {
            Ok(())
        }

        fn hide(&mut self) -> Result<(), PlatformError> {
            Ok(())
        }
    }

    #[test]
    fn default_correction_presentation_fails_closed_instead_of_ghost_fallback() {
        let mut overlay = UnsupportedCorrectionOverlay;

        assert!(matches!(
            overlay.show_correction(
                ScreenRect {
                    x: 1.0,
                    y: 2.0,
                    w: 3.0,
                    h: 4.0,
                },
                "the",
            ),
            Err(PlatformError::UnsupportedField { .. })
        ));
    }

    /// Minimal adapter that implements only the REQUIRED trait methods, so the
    /// range-seam defaults (`text_range_rect`, `insert_replacing_range`) are the
    /// trait-provided ones — exactly what a not-yet-ported platform inherits.
    struct DefaultRangeSeamAdapter;

    impl PlatformAdapter for DefaultRangeSeamAdapter {
        fn environment(&self) -> Environment {
            Environment {
                os: OperatingSystem::Unknown("test".to_string()),
                version: "0".to_string(),
            }
        }

        fn subscribe_focus(&self, _cb: FocusCallback) -> Result<Subscription, PlatformError> {
            Ok(Subscription::new(1))
        }

        fn subscribe_caret(&self, _cb: CaretCallback) -> Result<Subscription, PlatformError> {
            Ok(Subscription::new(2))
        }

        fn subscribe_accept(
            &self,
            _cb: AcceptCallback,
        ) -> Result<AcceptSubscription, PlatformError> {
            Ok(AcceptSubscription::new(
                Subscription::new(3),
                |_| Ok(()),
                |_| Ok(()),
                |_| Ok(()),
            ))
        }

        fn front_app(&self) -> Option<AppId> {
            None
        }

        fn capabilities(&self, _field: &FieldHandle) -> Result<Capabilities, PlatformError> {
            Err(PlatformError::Timeout)
        }

        fn read_context(&self, _field: &FieldHandle) -> Result<TextContext, PlatformError> {
            Err(PlatformError::Timeout)
        }

        fn caret_rect(&self, _field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
            Ok(None)
        }

        fn insert(
            &self,
            _field: &FieldHandle,
            _text: &str,
            _strategy: InsertStrategy,
        ) -> Result<Inserted, PlatformError> {
            Err(PlatformError::Timeout)
        }

        fn insert_replacing(
            &self,
            _field: &FieldHandle,
            _text: &str,
            _replace_left: usize,
            _strategy: InsertStrategy,
        ) -> Result<Inserted, PlatformError> {
            Err(PlatformError::Timeout)
        }
    }

    #[test]
    fn default_range_seam_errors_name_the_unsupported_capability() {
        // The trait defaults for the correction-range seam must fail closed AND
        // name the missing capability, so an operator log distinguishes "range
        // geometry missing" from "range replacement missing". Pin the exact
        // reason strings — a reworded or swapped default would break diagnosis.
        let adapter = DefaultRangeSeamAdapter;
        let field = FieldHandle {
            app: "test".to_string(),
            pid: None,
            element_id: "range-seam".to_string(),
            generation: 0,
        };
        let range = CorrectionRange { start: 0, end: 3 };

        match adapter.text_range_rect(&field, range) {
            Err(PlatformError::UnsupportedField { reason }) => {
                assert_eq!(reason, "correction range geometry unsupported");
            }
            other => panic!("text_range_rect default must fail closed, got {other:?}"),
        }

        match adapter.insert_replacing_range(&field, "old", "new", range, InsertStrategy::AxSet) {
            Err(PlatformError::UnsupportedField { reason }) => {
                assert_eq!(reason, "range replacement unsupported");
            }
            other => panic!("insert_replacing_range default must fail closed, got {other:?}"),
        }
    }

    #[test]
    fn platform_error_display_renders_each_variant() {
        // Pin the user-facing Display string per variant so a swapped text,
        // dropped placeholder, or wrong interpolation is caught. Field-carrying
        // variants must surface their payload; unit variants their fixed text.
        assert_eq!(
            PlatformError::PermissionMissing {
                permission: "accessibility".to_string(),
            }
            .to_string(),
            "required permission missing: accessibility"
        );
        assert_eq!(
            PlatformError::SecureInput {
                state: SecurityState::SecureField,
            }
            .to_string(),
            "secure input active: SecureField"
        );
        assert_eq!(
            PlatformError::CannotComplete {
                reason: "no caret".to_string(),
            }
            .to_string(),
            "cannot complete: no caret"
        );
        assert_eq!(
            PlatformError::UnsupportedField {
                reason: "read-only".to_string(),
            }
            .to_string(),
            "unsupported field: read-only"
        );
        assert_eq!(
            PlatformError::AppExited {
                app: "com.apple.Mail".to_string(),
            }
            .to_string(),
            "app exited: com.apple.Mail"
        );
        assert_eq!(
            PlatformError::Timeout.to_string(),
            "platform operation timed out"
        );
        assert_eq!(PlatformError::StaleField.to_string(), "field is stale");
    }
}
