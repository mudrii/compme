//! Windows platform adapter — SCAFFOLD (ROADMAP Tier 1.1).
//!
//! Implements the [`platform::PlatformAdapter`] contract so the cross-platform
//! structure exists and CI can gate it, but the real Windows API integration is
//! **not yet built** — it requires a Windows build+test environment (this
//! scaffold was authored on a macOS-only host). Every method is a fail-closed
//! stub returning [`PlatformError::UnsupportedField`] (IO/subscribe) or a safe
//! empty value, so wiring this adapter in is inert, never a crash. Each method's
//! doc names the Win32 API its real implementation will use.

use platform::{
    AcceptCallback, AcceptSubscription, AppId, Capabilities, CaretCallback, Environment,
    FieldHandle, FocusCallback, InsertStrategy, Inserted, OperatingSystem, PlatformAdapter,
    PlatformError, ScreenRect, Subscription, TextContext,
};

/// Windows implementation of [`PlatformAdapter`] — scaffold (see module docs).
/// Implementation map for the real adapter (built on a Windows host):
/// - focus / caret events → UI Automation (`IUIAutomation` + event handlers)
/// - capabilities / read_context / caret_rect → UIA TextPattern + bounding rects
/// - subscribe_accept → low-level keyboard hook (`WH_KEYBOARD_LL`)
/// - insert / insert_replacing → UIA ValuePattern, else `SendInput` synthetic keys
/// - overlay → a layered, click-through, topmost window (separate `OverlayPresenter`)
#[derive(Debug, Default)]
pub struct WindowsAdapter;

impl WindowsAdapter {
    pub fn new() -> Self {
        Self
    }

    /// The error every not-yet-implemented method returns. Fail-closed: the host
    /// treats any error as "no suggestion this turn" and leaves the field
    /// untouched, so an unwired Windows adapter is inert, never harmful.
    fn unsupported(method: &str) -> PlatformError {
        PlatformError::UnsupportedField {
            reason: format!("platform_windows::{method} not yet implemented (Tier 1.1 scaffold)"),
        }
    }
}

impl PlatformAdapter for WindowsAdapter {
    /// Real impl: `RtlGetVersion`. Cheap + infallible per the contract.
    fn environment(&self) -> Environment {
        Environment {
            os: OperatingSystem::Windows,
            version: "unknown".to_string(),
            display_topology: None,
        }
    }

    /// Real impl: UI Automation focus-changed event handler.
    fn subscribe_focus(&self, _cb: FocusCallback) -> Result<Subscription, PlatformError> {
        Err(Self::unsupported("subscribe_focus"))
    }

    /// Real impl: UIA TextPattern caret + structure-changed events.
    fn subscribe_caret(&self, _cb: CaretCallback) -> Result<Subscription, PlatformError> {
        Err(Self::unsupported("subscribe_caret"))
    }

    /// Real impl: a `WH_KEYBOARD_LL` low-level hook gating the accept/dismiss keys.
    fn subscribe_accept(&self, _cb: AcceptCallback) -> Result<AcceptSubscription, PlatformError> {
        Err(Self::unsupported("subscribe_accept"))
    }

    /// Real impl: `GetForegroundWindow` → `GetWindowThreadProcessId` → module name.
    fn front_app(&self) -> Option<AppId> {
        None
    }

    /// Real impl: UIA control/value/text patterns + secure-desktop probe.
    fn capabilities(&self, _field: &FieldHandle) -> Result<Capabilities, PlatformError> {
        Err(Self::unsupported("capabilities"))
    }

    /// Real impl: UIA TextPattern range around the caret.
    fn read_context(&self, _field: &FieldHandle) -> Result<TextContext, PlatformError> {
        Err(Self::unsupported("read_context"))
    }

    /// Real impl: UIA TextPattern bounding rectangle of the caret/selection.
    fn caret_rect(&self, _field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
        Err(Self::unsupported("caret_rect"))
    }

    /// Real impl: UIA ValuePattern set, else `SendInput` synthetic typing.
    fn insert(
        &self,
        _field: &FieldHandle,
        _text: &str,
        _strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        Err(Self::unsupported("insert"))
    }

    /// Real impl: UIA range-replace, else backspace×N + `SendInput` typing.
    fn insert_replacing(
        &self,
        _field: &FieldHandle,
        _text: &str,
        _replace_left: usize,
        _strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        Err(Self::unsupported("insert_replacing"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn scaffold_reports_windows_and_fails_closed() {
        let adapter = WindowsAdapter::new();
        // environment() is the one cheap, infallible method the scaffold answers.
        assert_eq!(adapter.environment().os, OperatingSystem::Windows);
        // No frontmost app until the real impl lands.
        assert_eq!(adapter.front_app(), None);
        // Subscribe/IO methods fail closed (UnsupportedField), never panic — the
        // host treats this as "no suggestion this turn" and leaves fields alone.
        let cb: FocusCallback = Arc::new(|_field| {});
        assert!(matches!(
            adapter.subscribe_focus(cb),
            Err(PlatformError::UnsupportedField { .. })
        ));
        // insert_replacing is the method whose missing/wrong impl caused the
        // historical `:smile😄` append-only bug, so pin that the scaffold returns
        // an error rather than silently delegating to an append-only insert.
        let field = FieldHandle {
            app: "test".to_string(),
            pid: None,
            element_id: "scaffold".to_string(),
            generation: 0,
        };
        assert!(matches!(
            adapter.insert_replacing(&field, "x", 1, InsertStrategy::None),
            Err(PlatformError::UnsupportedField { .. })
        ));
        // The two methods the scaffold inherits as trait defaults (fail-OPEN by
        // design: "no anchor / no domain", which is safe) are pinned here so a
        // future change to the trait defaults can't silently alter stub behavior.
        assert!(matches!(adapter.popup_anchor(&field), Ok(None)));
        assert!(matches!(adapter.focused_page_url(&field), Ok(None)));
    }
}
