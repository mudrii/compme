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
        // The scaffold has no real version probe yet: it reports the fixed
        // "unknown" version. Pin it so the real version impl visibly replaces
        // the placeholder.
        assert_eq!(adapter.environment().version, "unknown");
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
        assert!(matches!(
            adapter.text_range_rect(&field, platform::CorrectionRange { start: 0, end: 1 }),
            Err(PlatformError::UnsupportedField { .. })
        ));
        assert!(matches!(
            adapter.insert_replacing_range(
                &field,
                "old",
                "x",
                platform::CorrectionRange { start: 0, end: 1 },
                InsertStrategy::AxSet,
            ),
            Err(PlatformError::UnsupportedField { .. })
        ));
        // The two methods the scaffold inherits as trait defaults (fail-OPEN by
        // design: "no anchor / no domain", which is safe) are pinned here so a
        // future change to the trait defaults can't silently alter stub behavior.
        assert!(matches!(adapter.popup_anchor(&field), Ok(None)));
        assert!(matches!(adapter.focused_page_url(&field), Ok(None)));
    }

    #[test]
    fn every_io_and_subscribe_method_fails_closed() {
        // Fail-closed is the scaffold's whole point: the prior test pinned only
        // subscribe_focus + insert_replacing. Pin the rest so any one method
        // regressing to Ok (e.g. an accidental stub returning empty caps) is a
        // test failure, not a silent live-fire of an unimplemented adapter.
        let adapter = WindowsAdapter::new();
        let field = FieldHandle {
            app: "test".to_string(),
            pid: None,
            element_id: "scaffold".to_string(),
            generation: 0,
        };

        let caret_cb: CaretCallback = Arc::new(|_field, _rect| {});
        assert!(matches!(
            adapter.subscribe_caret(caret_cb),
            Err(PlatformError::UnsupportedField { .. })
        ));
        let accept_cb: AcceptCallback = Arc::new(|_tap| {});
        assert!(matches!(
            adapter.subscribe_accept(accept_cb),
            Err(PlatformError::UnsupportedField { .. })
        ));
        assert!(matches!(
            adapter.capabilities(&field),
            Err(PlatformError::UnsupportedField { .. })
        ));
        assert!(matches!(
            adapter.read_context(&field),
            Err(PlatformError::UnsupportedField { .. })
        ));
        assert!(matches!(
            adapter.caret_rect(&field),
            Err(PlatformError::UnsupportedField { .. })
        ));
        assert!(matches!(
            adapter.insert(&field, "x", InsertStrategy::None),
            Err(PlatformError::UnsupportedField { .. })
        ));
    }

    #[test]
    fn insert_fails_closed_for_every_strategy_variant() {
        // The prior tests only exercised InsertStrategy::None. A real adapter that
        // half-landed (e.g. an AxSet/Clipboard branch returning Ok before the
        // others) must not slip past the scaffold gate, so pin BOTH insert and
        // insert_replacing as UnsupportedField across ALL strategies. If a variant
        // is added to InsertStrategy, this match goes non-exhaustive and forces an
        // update.
        let adapter = WindowsAdapter::new();
        let field = FieldHandle {
            app: "test".to_string(),
            pid: None,
            element_id: "scaffold".to_string(),
            generation: 0,
        };
        for strategy in [
            InsertStrategy::AxSet,
            InsertStrategy::SyntheticKeys,
            InsertStrategy::Clipboard,
            InsertStrategy::ImeCommit,
            InsertStrategy::None,
        ] {
            // Exhaustive, wildcard-free: a new InsertStrategy variant breaks
            // compilation here and forces the array above to be updated too.
            match strategy {
                InsertStrategy::AxSet
                | InsertStrategy::SyntheticKeys
                | InsertStrategy::Clipboard
                | InsertStrategy::ImeCommit
                | InsertStrategy::None => {}
            }
            assert!(
                matches!(
                    adapter.insert(&field, "x", strategy),
                    Err(PlatformError::UnsupportedField { .. })
                ),
                "insert {strategy:?}"
            );
            assert!(
                matches!(
                    adapter.insert_replacing(&field, "x", 1, strategy),
                    Err(PlatformError::UnsupportedField { .. })
                ),
                "insert_replacing {strategy:?}"
            );
            assert!(
                matches!(
                    adapter.insert_replacing_range(
                        &field,
                        "old",
                        "x",
                        platform::CorrectionRange { start: 0, end: 1 },
                        strategy,
                    ),
                    Err(PlatformError::UnsupportedField { .. })
                ),
                "insert_replacing_range {strategy:?}"
            );
        }
    }

    #[test]
    fn insert_replacing_zero_replace_left_also_fails_closed() {
        // The trait mandates that `replace_left == 0` behaves as a plain insert
        // (no backspaces). The prior matrix test only used replace_left == 1, so
        // pin that the scaffold still fails closed for the insert-like zero case
        // across every strategy — an adapter that special-cased replace_left == 0
        // to an Ok append must not slip past the gate.
        let adapter = WindowsAdapter::new();
        let field = FieldHandle {
            app: "test".to_string(),
            pid: None,
            element_id: "scaffold".to_string(),
            generation: 0,
        };
        for strategy in [
            InsertStrategy::AxSet,
            InsertStrategy::SyntheticKeys,
            InsertStrategy::Clipboard,
            InsertStrategy::ImeCommit,
            InsertStrategy::None,
        ] {
            assert!(
                matches!(
                    adapter.insert_replacing(&field, "x", 0, strategy),
                    Err(PlatformError::UnsupportedField { .. })
                ),
                "insert_replacing replace_left=0 {strategy:?}"
            );
        }
    }

    #[test]
    fn unsupported_reason_names_the_failing_method() {
        // Fail-closed isn't enough: when a stub rejects, its diagnostic must name
        // BOTH the crate and the exact method, so an operator reading a log can
        // tell *which* unimplemented call fired. Pin the real reason format
        // ("platform_windows::<method> not yet implemented (Tier 1.1 scaffold)")
        // across a representative spread — a subscribe, a capability probe, and an
        // insert — so a future refactor of `unsupported()` can't drop the method
        // name (or the crate prefix) without breaking a test.
        let adapter = WindowsAdapter::new();
        let field = FieldHandle {
            app: "test".to_string(),
            pid: None,
            element_id: "scaffold".to_string(),
            generation: 0,
        };

        let Err(PlatformError::UnsupportedField { reason }) = adapter.capabilities(&field) else {
            panic!("capabilities should fail closed with UnsupportedField");
        };
        assert!(
            reason.contains("platform_windows::"),
            "reason should carry the crate prefix: {reason:?}"
        );
        assert!(
            reason.contains("capabilities"),
            "reason should name the failing method `capabilities`: {reason:?}"
        );
        assert!(
            reason.contains("not yet implemented (Tier 1.1 scaffold)"),
            "reason should explain the stub is a scaffold: {reason:?}"
        );
        assert_eq!(
            reason, "platform_windows::capabilities not yet implemented (Tier 1.1 scaffold)",
            "full reason string format pinned"
        );

        let caret_cb: CaretCallback = Arc::new(|_field, _rect| {});
        let Err(PlatformError::UnsupportedField { reason }) = adapter.subscribe_caret(caret_cb)
        else {
            panic!("subscribe_caret should fail closed with UnsupportedField");
        };
        assert!(
            reason.contains("platform_windows::") && reason.contains("subscribe_caret"),
            "reason should name crate + `subscribe_caret`: {reason:?}"
        );

        let Err(PlatformError::UnsupportedField { reason }) =
            adapter.insert_replacing(&field, "x", 1, InsertStrategy::None)
        else {
            panic!("insert_replacing should fail closed with UnsupportedField");
        };
        assert!(
            reason.contains("platform_windows::") && reason.contains("insert_replacing"),
            "reason should name crate + `insert_replacing`: {reason:?}"
        );
    }
}
