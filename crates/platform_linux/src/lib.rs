//! Linux platform adapter — SCAFFOLD (ROADMAP Tier 1.1).
//!
//! Implements the [`platform::PlatformAdapter`] contract so the cross-platform
//! structure exists and CI can gate it, but the real Linux API integration is
//! **not yet built** — it requires a Linux build+test environment (this scaffold
//! was authored on a macOS-only host). Every method is a fail-closed stub
//! returning [`PlatformError::UnsupportedField`] (IO/subscribe) or a safe empty
//! value, so wiring this adapter in is inert, never a crash. Each method's doc
//! names the Linux API its real implementation will use.

use platform::{
    AcceptCallback, AcceptSubscription, AppId, Capabilities, CaretCallback, Environment,
    FieldHandle, FocusCallback, InsertStrategy, Inserted, OperatingSystem, PlatformAdapter,
    PlatformError, ScreenRect, Subscription, TextContext,
};

/// Linux implementation of [`PlatformAdapter`] — scaffold (see module docs).
/// Implementation map for the real adapter (built on a Linux host):
/// - focus / caret events → AT-SPI2 (`atspi`, accessibility over D-Bus)
/// - capabilities / read_context / caret_rect → AT-SPI2 Text/EditableText interfaces
/// - subscribe_accept → AT-SPI2 device/key listeners (X11), or a compositor path on Wayland
/// - insert / insert_replacing → AT-SPI2 EditableText, else XTEST / `wtype` synthetic keys
///   (Wayland restricts synthetic injection — IBus IME commit is the fallback)
/// - overlay → an override-redirect X11 window, or a layer-shell surface on Wayland
#[derive(Debug, Default)]
pub struct LinuxAdapter;

impl LinuxAdapter {
    pub fn new() -> Self {
        Self
    }

    /// The error every not-yet-implemented method returns. Fail-closed: the host
    /// treats any error as "no suggestion this turn" and leaves the field
    /// untouched, so an unwired Linux adapter is inert, never harmful.
    fn unsupported(method: &str) -> PlatformError {
        PlatformError::UnsupportedField {
            reason: format!("platform_linux::{method} not yet implemented (Tier 1.1 scaffold)"),
        }
    }
}

impl PlatformAdapter for LinuxAdapter {
    /// Real impl: `/etc/os-release` + `uname`. Cheap + infallible per the contract.
    fn environment(&self) -> Environment {
        Environment {
            os: OperatingSystem::Linux,
            version: "unknown".to_string(),
            display_topology: None,
        }
    }

    /// Real impl: AT-SPI2 focus-changed event subscription (D-Bus).
    fn subscribe_focus(&self, _cb: FocusCallback) -> Result<Subscription, PlatformError> {
        Err(Self::unsupported("subscribe_focus"))
    }

    /// Real impl: AT-SPI2 text-caret-moved / bounds-changed events.
    fn subscribe_caret(&self, _cb: CaretCallback) -> Result<Subscription, PlatformError> {
        Err(Self::unsupported("subscribe_caret"))
    }

    /// Real impl: AT-SPI2 device/key listener (X11); a compositor shortcut on Wayland.
    fn subscribe_accept(&self, _cb: AcceptCallback) -> Result<AcceptSubscription, PlatformError> {
        Err(Self::unsupported("subscribe_accept"))
    }

    /// Real impl: AT-SPI2 active-window application name.
    fn front_app(&self) -> Option<AppId> {
        None
    }

    /// Real impl: AT-SPI2 Text/EditableText interface probe + role/state checks.
    fn capabilities(&self, _field: &FieldHandle) -> Result<Capabilities, PlatformError> {
        Err(Self::unsupported("capabilities"))
    }

    /// Real impl: AT-SPI2 Text interface range around the caret.
    fn read_context(&self, _field: &FieldHandle) -> Result<TextContext, PlatformError> {
        Err(Self::unsupported("read_context"))
    }

    /// Real impl: AT-SPI2 character-extents bounding rectangle of the caret.
    fn caret_rect(&self, _field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
        Err(Self::unsupported("caret_rect"))
    }

    /// Real impl: AT-SPI2 EditableText insert, else XTEST / `wtype` synthetic typing.
    fn insert(
        &self,
        _field: &FieldHandle,
        _text: &str,
        _strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        Err(Self::unsupported("insert"))
    }

    /// Real impl: AT-SPI2 range-replace, else backspace×N + XTEST/`wtype` typing.
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
    fn scaffold_reports_linux_and_fails_closed() {
        let adapter = LinuxAdapter::new();
        // environment() is the one cheap, infallible method the scaffold answers.
        assert_eq!(adapter.environment().os, OperatingSystem::Linux);
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

    #[test]
    fn every_io_and_subscribe_method_fails_closed() {
        // Fail-closed is the scaffold's whole point: the prior test pinned only
        // subscribe_focus + insert_replacing. Pin the rest so any one method
        // regressing to Ok (e.g. an accidental stub returning empty caps) is a
        // test failure, not a silent live-fire of an unimplemented adapter.
        let adapter = LinuxAdapter::new();
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
        // half-landed (e.g. an EditableText/XTEST branch returning Ok before the
        // others) must not slip past the scaffold gate, so pin BOTH insert and
        // insert_replacing as UnsupportedField across ALL strategies. If a variant
        // is added to InsertStrategy, this match goes non-exhaustive and forces an
        // update.
        let adapter = LinuxAdapter::new();
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
        }
    }
}
