//! Cross-platform contract shared by the pure engine and platform adapters.

use std::sync::Arc;
use std::time::Duration;

pub type AppId = String;
pub type FocusCallback = Arc<dyn Fn(FieldHandle) + Send + Sync + 'static>;
pub type CaretCallback = Arc<dyn Fn(FieldHandle, Option<ScreenRect>) + Send + Sync + 'static>;
pub type AcceptCallback = Arc<dyn Fn(AcceptAction) + Send + Sync + 'static>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FieldHandle {
    pub app: AppId,
    pub pid: Option<u32>,
    pub element_id: String,
    pub generation: u64,
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextSource {
    Accessibility,
    Clipboard,
    Synthetic,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OffsetEncoding {
    Utf8Bytes,
    Utf16CodeUnits,
    UnicodeScalars,
    GraphemeClusters,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecurityState {
    Normal,
    SecureField,
    SecureInputEnabled,
    Unknown,
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertStrategy {
    AxSet,
    SyntheticKeys,
    Clipboard,
    ImeCommit,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyInterceptMode {
    CgEventTap,
    LowLevelHook,
    XGrabKey,
    FocusScopedInhibit,
    ImeOwnsKey,
    HotkeyOnly,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptAction {
    Full,
    Word,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayPlacement {
    NativePanel,
    LayeredWindow,
    OverrideRedirect,
    LayerShell,
    ImeCandidate,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UxMode {
    Inline,
    Popup,
    Hotkey,
    Unsupported,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Environment {
    pub os: OperatingSystem,
    pub version: String,
    pub display_topology: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperatingSystem {
    Macos,
    Windows,
    Linux,
    Unknown(String),
}

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

pub struct Subscription {
    id: u64,
    cancel: Option<Box<dyn FnOnce() + Send + 'static>>,
}

pub struct AcceptSubscription {
    subscription: Subscription,
    set_suggestion_visible: Arc<dyn Fn(bool) -> Result<(), PlatformError> + Send + Sync + 'static>,
    hide_suggestion_after:
        Arc<dyn Fn(Duration) -> Result<(), PlatformError> + Send + Sync + 'static>,
    set_accept_action:
        Arc<dyn Fn(Option<AcceptAction>) -> Result<(), PlatformError> + Send + Sync + 'static>,
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
        }
    }

    pub fn id(&self) -> u64 {
        self.subscription.id()
    }

    pub fn set_suggestion_visible(&self, visible: bool) -> Result<(), PlatformError> {
        (self.set_suggestion_visible)(visible)
    }

    pub fn hide_suggestion_after(&self, delay: Duration) -> Result<(), PlatformError> {
        (self.hide_suggestion_after)(delay)
    }

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inserted {
    pub bytes: usize,
    pub chars: usize,
    pub strategy: InsertStrategy,
}

pub trait PlatformAdapter: Send + Sync {
    fn environment(&self) -> Environment;
    fn subscribe_focus(&self, cb: FocusCallback) -> Result<Subscription, PlatformError>;
    fn subscribe_caret(&self, cb: CaretCallback) -> Result<Subscription, PlatformError>;
    fn subscribe_accept(&self, cb: AcceptCallback) -> Result<AcceptSubscription, PlatformError>;
    fn front_app(&self) -> Option<AppId>;
    fn capabilities(&self, field: &FieldHandle) -> Result<Capabilities, PlatformError>;
    fn read_context(&self, field: &FieldHandle) -> Result<TextContext, PlatformError>;
    fn caret_rect(&self, field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError>;
    /// A fallback anchor rect for popup-mode placement when no caret geometry is
    /// available (e.g. the focused field exposes no caret bounds). Typically the
    /// focused window frame. Defaults to `None` for adapters that cannot supply
    /// one.
    fn popup_anchor(&self, _field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
        Ok(None)
    }
    fn insert(
        &self,
        field: &FieldHandle,
        text: &str,
        strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError>;
}

pub trait OverlayPresenter {
    fn show_ghost(&mut self, rect: ScreenRect, text: &str) -> Result<(), PlatformError>;
    fn update_ghost(&mut self, text: &str) -> Result<(), PlatformError>;
    fn hide(&mut self) -> Result<(), PlatformError>;
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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
    fn hotkey_only_intercept_is_hotkey_mode() {
        let mut c = caps();
        c.accept_intercept = KeyInterceptMode::HotkeyOnly;

        assert_eq!(ux_mode(&c), UxMode::Hotkey);
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
}
