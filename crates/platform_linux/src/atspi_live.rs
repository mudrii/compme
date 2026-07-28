//! Live AT-SPI2 read path (ROADMAP Phase 2.1/2.2) — Linux only.
//!
//! Talks to the accessibility bus over D-Bus with `atspi`'s blocking proxies. The
//! decisions worth knowing:
//!
//! **Why D-Bus and not libatspi.** Linking the C library would make the compme
//! binary refuse to *start* on a host without it — a hard failure where this
//! project requires fail-closed degradation. A D-Bus connection is something we
//! attempt and report on, so a headless server or a session with accessibility
//! switched off simply has no adapter, not a broken executable.
//!
//! **Why blocking proxies.** [`platform::PlatformAdapter`] is synchronous, and the
//! macOS adapter already establishes the pattern of one owner thread serializing
//! platform calls. Blocking proxies fit that directly; an async runtime would add
//! a second concurrency idiom for no gain (see the plan's cross-cutting rules).
//!
//! **Offsets are Unicode scalars.** AT-SPI counts characters, not UTF-16 code
//! units, so this is the first adapter to report
//! [`platform::OffsetEncoding::UnicodeScalars`] — the unit the `context` crate
//! actually wants. Slicing therefore goes through scalar-aware helpers, never
//! byte indexing, and the integration tests deliberately include astral-plane
//! text because a UTF-16 assumption survives every ASCII test.

use crate::atspi_caps::{capabilities_from, FieldFacts};
use crate::atspi_ids::ElementId;
use atspi::proxy::accessible::AccessibleProxyBlocking;
use atspi::proxy::application::ApplicationProxyBlocking;
use atspi::proxy::text::TextProxyBlocking;
use atspi::zbus::blocking::Connection;
use atspi::{CoordType, Interface, State};
use platform::{
    Capabilities, ContextSource, FieldHandle, OffsetEncoding, PlatformError, ScreenRect,
    TextContext, TextRange,
};

/// The accessibility registry's root object — the "desktop" whose children are
/// the applications currently exposing accessibility.
const REGISTRY_BUS: &str = "org.a11y.atspi.Registry";
const REGISTRY_ROOT: &str = "/org/a11y/atspi/accessible/root";

/// Depth bound for tree walks. The accessibility tree is a live UI hierarchy
/// owned by other processes: it can be deep, and a malformed one can cycle. A
/// bound turns "compme hangs" into "compme reports no field".
const MAX_DEPTH: usize = 16;

/// Cap on the text pulled from one field. `GetText(0, -1)` is unbounded input
/// from another process — a multi-megabyte document would otherwise be copied
/// through D-Bus on every keystroke.
const MAX_FIELD_SCALARS: usize = 200_000;

fn cannot_complete(what: &str, err: impl std::fmt::Display) -> PlatformError {
    PlatformError::CannotComplete {
        reason: format!("platform_linux atspi {what}: {err}"),
    }
}

fn unsupported(reason: String) -> PlatformError {
    PlatformError::UnsupportedField { reason }
}

/// A connection to the accessibility bus. One per adapter; the owning thread
/// serializes all use, mirroring the macOS AX worker.
pub struct AtspiSession {
    connection: Connection,
}

impl std::fmt::Debug for AtspiSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // zbus::blocking::Connection is not Debug; the address is the only part
        // worth logging and it is not secret.
        f.debug_struct("AtspiSession").finish_non_exhaustive()
    }
}

impl AtspiSession {
    /// Open the accessibility bus: ask `org.a11y.Bus` on the session bus for its
    /// address, then connect to it.
    ///
    /// Every failure here is expected on some legitimate host — no session bus on
    /// a headless server, no `org.a11y.Bus` when accessibility is not running — so
    /// each is reported rather than retried or panicked on. The caller treats an
    /// error as "this host has no accessibility support".
    pub fn open() -> Result<Self, PlatformError> {
        let session = Connection::session().map_err(|err| cannot_complete("session bus", err))?;
        let address: String = session
            .call_method(
                Some("org.a11y.Bus"),
                "/org/a11y/bus",
                Some("org.a11y.Bus"),
                "GetAddress",
                &(),
            )
            .map_err(|err| cannot_complete("org.a11y.Bus GetAddress", err))?
            .body()
            .deserialize()
            .map_err(|err| cannot_complete("a11y bus address", err))?;
        let address = address
            .parse::<atspi::zbus::Address>()
            .map_err(|err| cannot_complete("a11y bus address parse", err))?;
        let connection = atspi::zbus::blocking::connection::Builder::address(address)
            .map_err(|err| cannot_complete("a11y bus builder", err))?
            .build()
            .map_err(|err| cannot_complete("a11y bus connect", err))?;
        Ok(Self { connection })
    }

    fn accessible(&self, id: &ElementId) -> Result<AccessibleProxyBlocking<'_>, PlatformError> {
        AccessibleProxyBlocking::builder(&self.connection)
            .destination(id.bus_name.clone())
            .map_err(|err| cannot_complete("accessible destination", err))?
            .path(id.path.clone())
            .map_err(|err| cannot_complete("accessible path", err))?
            .build()
            .map_err(|err| cannot_complete("accessible proxy", err))
    }

    fn text(&self, id: &ElementId) -> Result<TextProxyBlocking<'_>, PlatformError> {
        TextProxyBlocking::builder(&self.connection)
            .destination(id.bus_name.clone())
            .map_err(|err| cannot_complete("text destination", err))?
            .path(id.path.clone())
            .map_err(|err| cannot_complete("text path", err))?
            .build()
            .map_err(|err| cannot_complete("text proxy", err))
    }

    /// The focused editable field, if any: a depth-bounded walk from the registry
    /// root looking for `STATE_FOCUSED`.
    ///
    /// Applications that expose no accessibility are simply absent from this
    /// tree, and one unreachable application must not hide the rest, so a
    /// per-application error is skipped rather than propagated.
    pub fn focused_field(&self) -> Result<Option<ElementId>, PlatformError> {
        let root = ElementId::new(REGISTRY_BUS, REGISTRY_ROOT);
        let desktop = self.accessible(&root)?;
        let apps = desktop
            .get_children()
            .map_err(|err| cannot_complete("desktop children", err))?;
        for app in apps {
            let Some(bus_name) = app.name_as_str() else {
                continue;
            };
            let app_id = ElementId::new(bus_name, app.path_as_str());
            if let Ok(Some(found)) = self.find_focused(&app_id, 0) {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    fn find_focused(
        &self,
        id: &ElementId,
        depth: usize,
    ) -> Result<Option<ElementId>, PlatformError> {
        if depth > MAX_DEPTH {
            return Ok(None);
        }
        let node = self.accessible(id)?;
        if let Ok(states) = node.get_state() {
            if states.contains(State::Focused) {
                return Ok(Some(id.clone()));
            }
        }
        let children = match node.get_children() {
            Ok(children) => children,
            // A child list we cannot read is not an error for the whole walk:
            // the application may have exited mid-traversal.
            Err(_) => return Ok(None),
        };
        for child in children {
            let Some(bus_name) = child.name_as_str() else {
                continue;
            };
            let child_id = ElementId::new(bus_name, child.path_as_str());
            if let Ok(Some(found)) = self.find_focused(&child_id, depth + 1) {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// Collect the facts [`capabilities_from`] needs. A missing toolkit name is
    /// not fatal — it only softens the `Toolkit` hint.
    pub fn field_facts(&self, id: &ElementId) -> Result<FieldFacts, PlatformError> {
        let node = self.accessible(id)?;
        let interfaces = node
            .get_interfaces()
            .map_err(|err| cannot_complete("get_interfaces", err))?;
        let states = node
            .get_state()
            .map_err(|err| cannot_complete("get_state", err))?;
        let role = node
            .get_role_name()
            .map_err(|err| cannot_complete("get_role_name", err))?;
        let toolkit_name = self.toolkit_name(&node).unwrap_or_default();
        Ok(FieldFacts {
            has_text: interfaces.contains(Interface::Text),
            has_editable_text: interfaces.contains(Interface::EditableText),
            editable: states.contains(State::Editable),
            sensitive: states.contains(State::Sensitive),
            multiline: states.contains(State::MultiLine),
            role,
            toolkit_name,
        })
    }

    fn toolkit_name(&self, node: &AccessibleProxyBlocking<'_>) -> Option<String> {
        let app = node.get_application().ok()?;
        let app_id = ElementId::new(app.name_as_str()?, app.path_as_str());
        ApplicationProxyBlocking::builder(&self.connection)
            .destination(app_id.bus_name)
            .ok()?
            .path(app_id.path)
            .ok()?
            .build()
            .ok()?
            .toolkit_name()
            .ok()
    }

    pub fn capabilities(&self, id: &ElementId) -> Result<Capabilities, PlatformError> {
        Ok(capabilities_from(&self.field_facts(id)?))
    }

    /// Read the field's text split at the caret, plus any selection.
    ///
    /// The caret offset and every returned range are Unicode scalars, matching
    /// AT-SPI's own unit; nothing here indexes bytes.
    pub fn read_context(&self, field: &FieldHandle) -> Result<TextContext, PlatformError> {
        let id = ElementId::decode(&field.element_id)
            .ok_or_else(|| unsupported(format!("malformed element id: {}", field.element_id)))?;
        let text = self.text(&id)?;
        let value = text
            .get_text(0, -1)
            .map_err(|err| cannot_complete("get_text", err))?;
        let scalars: Vec<char> = value.chars().take(MAX_FIELD_SCALARS).collect();
        let caret = text
            .caret_offset()
            .map_err(|err| cannot_complete("caret_offset", err))?;
        // A negative or past-the-end caret is a toolkit bug, not something to
        // propagate into scalar arithmetic: clamp into the text we actually read.
        let caret = usize::try_from(caret).unwrap_or(0).min(scalars.len());
        let left: String = scalars[..caret].iter().collect();
        let right: String = scalars[caret..].iter().collect();
        let (selection, selected_text) = self.selection(&text, &scalars);
        Ok(TextContext {
            left,
            right,
            left_scalars: caret,
            selection,
            selected_text,
            caret,
            source: ContextSource::Accessibility,
            field_id: field.clone(),
            offset_encoding: OffsetEncoding::UnicodeScalars,
        })
    }

    /// The first non-empty selection, as a scalar range plus its exact text.
    ///
    /// Selection is best-effort: a toolkit that refuses the query still has a
    /// readable field, and reporting no selection is the safe answer (it only
    /// disables selection-scoped features like the thesaurus).
    fn selection(
        &self,
        text: &TextProxyBlocking<'_>,
        scalars: &[char],
    ) -> (Option<TextRange>, Option<String>) {
        if text.get_n_selections().unwrap_or(0) < 1 {
            return (None, None);
        }
        let Ok((start, end)) = text.get_selection(0) else {
            return (None, None);
        };
        let start = usize::try_from(start).unwrap_or(0).min(scalars.len());
        let end = usize::try_from(end).unwrap_or(0).min(scalars.len());
        if start >= end {
            return (None, None);
        }
        let selected: String = scalars[start..end].iter().collect();
        (Some(TextRange { start, end }), Some(selected))
    }

    /// Screen rectangle of the character at the caret.
    ///
    /// `Ok(None)` means "no usable geometry" — either the toolkit reported a
    /// degenerate rect or the caret sits past the last character. Overlay
    /// placement must degrade rather than draw at a bogus point.
    pub fn caret_rect(&self, field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
        let id = ElementId::decode(&field.element_id)
            .ok_or_else(|| unsupported(format!("malformed element id: {}", field.element_id)))?;
        let text = self.text(&id)?;
        let caret = text
            .caret_offset()
            .map_err(|err| cannot_complete("caret_offset", err))?;
        let count = text.character_count().unwrap_or(0);
        // GetCharacterExtents is defined per existing character, so a caret at the
        // end has none; fall back to the last character's box, which is where the
        // next glyph will appear.
        let offset = caret.min(count.saturating_sub(1)).max(0);
        if count <= 0 {
            return Ok(None);
        }
        let (x, y, width, height) = text
            .get_character_extents(offset, CoordType::Screen)
            .map_err(|err| cannot_complete("get_character_extents", err))?;
        if width <= 0 || height <= 0 {
            return Ok(None);
        }
        Ok(Some(ScreenRect {
            x: f64::from(x),
            y: f64::from(y),
            w: f64::from(width),
            h: f64::from(height),
        }))
    }

    /// An accessible's `Name` property. Used for diagnostics and to confirm which
    /// field a walk landed on; `None` when the object is gone or refuses it.
    pub fn element_name(&self, id: &ElementId) -> Option<String> {
        self.accessible(id).ok()?.name().ok()
    }

    /// The application owning the focused field, for `front_app`.
    pub fn focused_app_name(&self) -> Option<String> {
        let focused = self.focused_field().ok().flatten()?;
        let node = self.accessible(&focused).ok()?;
        let app = node.get_application().ok()?;
        let app_id = ElementId::new(app.name_as_str()?, app.path_as_str());
        self.accessible(&app_id).ok()?.name().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `open()` must report a diagnosable error rather than panic when no
    /// accessibility bus exists. That is the normal state on a build machine and
    /// on any headless server, so it is the path most likely to run in anger.
    #[test]
    fn opening_without_an_accessibility_bus_fails_closed() {
        // Only assert the shape when there is demonstrably no session bus; a
        // developer desktop running this test may legitimately have one.
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
            return;
        }
        match AtspiSession::open() {
            Err(PlatformError::CannotComplete { reason }) => {
                assert!(
                    reason.starts_with("platform_linux atspi "),
                    "reason should name the crate and layer: {reason:?}"
                );
            }
            Err(other) => panic!("expected CannotComplete, got {other:?}"),
            Ok(_) => panic!("no session bus, yet a connection succeeded"),
        }
    }
}
