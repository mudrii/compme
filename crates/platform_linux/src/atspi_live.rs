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
use atspi::proxy::component::ComponentProxyBlocking;
use atspi::proxy::editable_text::EditableTextProxyBlocking;
use atspi::proxy::text::TextProxyBlocking;
use atspi::zbus::blocking::Connection;
use atspi::{CoordType, Interface, State};
use platform::{
    Capabilities, ContextSource, FieldHandle, InsertStrategy, Inserted, OffsetEncoding,
    PlatformError, ScreenRect, TextContext, TextRange,
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

/// Validate AT-SPI's signed character count before any unbounded text transfer.
///
/// A negative count is a broken peer response. A count beyond the local safety
/// cap cannot be represented by a whole-field snapshot without silently losing
/// the suffix, so both cases fail closed.
fn checked_field_scalar_count(count: i32) -> Result<usize, PlatformError> {
    let count = usize::try_from(count)
        .map_err(|_| unsupported(format!("invalid negative field scalar count: {count}")))?;
    if count > MAX_FIELD_SCALARS {
        return Err(field_over_cap_error());
    }
    Ok(count)
}

/// Validate the scalar length a whole-field swap would write: `field_len`
/// scalars with `replaced` of them exchanged for `inserted`.
///
/// Writing an over-cap rebuilt value would mutate the field and then fail the
/// capped readback — reporting the accept as failed after it already landed —
/// so the swap is refused up front instead.
fn checked_rebuilt_len(
    field_len: usize,
    replaced: usize,
    inserted: usize,
) -> Result<usize, PlatformError> {
    let rebuilt = field_len - replaced + inserted;
    if rebuilt > MAX_FIELD_SCALARS {
        return Err(field_over_cap_error());
    }
    Ok(rebuilt)
}

/// Every pre-write check a whole-field range replacement owes, followed by the
/// value it would write. One function so the checks cannot drift apart from the
/// write they guard: the caller cannot reach `SetTextContents` without the
/// `updated` string this returns, so no check here can be dropped and still
/// compile — which the checks-as-separate-statements shape did allow.
///
/// The inverted-range guard stays at the call site on purpose: it must run
/// before the field is read over D-Bus, and this function takes the scalars that
/// read produced.
fn checked_replacement(
    scalars: &[char],
    expected_text: &str,
    text: &str,
    range: platform::CorrectionRange,
) -> Result<String, PlatformError> {
    if range.end > scalars.len() {
        return Err(unsupported(format!(
            "platform_linux: range {}..{} past the field length {}",
            range.start,
            range.end,
            scalars.len()
        )));
    }
    let current: String = scalars[range.start..range.end].iter().collect();
    if current != expected_text {
        return Err(unsupported(format!(
            "platform_linux: field changed under the replacement (found {current:?})"
        )));
    }
    checked_rebuilt_len(scalars.len(), range.end - range.start, text.chars().count())?;
    let mut updated: String = scalars[..range.start].iter().collect();
    updated.push_str(text);
    updated.extend(scalars[range.end..].iter());
    Ok(updated)
}

fn field_over_cap_error() -> PlatformError {
    unsupported(format!(
        "field exceeds {MAX_FIELD_SCALARS} scalars; refusing lossy read/replace"
    ))
}

fn cannot_complete(what: &str, err: impl std::fmt::Display) -> PlatformError {
    PlatformError::CannotComplete {
        reason: format!("platform_linux atspi {what}: {err}"),
    }
}

fn unsupported(reason: String) -> PlatformError {
    PlatformError::UnsupportedField { reason }
}

fn screen_rect_from_extents((x, y, width, height): (i32, i32, i32, i32)) -> Option<ScreenRect> {
    (width > 0 && height > 0).then_some(ScreenRect {
        x: f64::from(x),
        y: f64::from(y),
        w: f64::from(width),
        h: f64::from(height),
    })
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

    /// The underlying bus connection. Exposed for the event workers
    /// (`atspi_events`), which clone it so cancelling a subscription can close it —
    /// the only way to interrupt a thread parked in the blocking message iterator.
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
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

    fn component(&self, id: &ElementId) -> Result<ComponentProxyBlocking<'_>, PlatformError> {
        ComponentProxyBlocking::builder(&self.connection)
            .destination(id.bus_name.clone())
            .map_err(|err| cannot_complete("component destination", err))?
            .path(id.path.clone())
            .map_err(|err| cannot_complete("component path", err))?
            .build()
            .map_err(|err| cannot_complete("component proxy", err))
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
        let scalars = self.field_scalars_from_text(&text)?;
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
        Ok(screen_rect_from_extents((x, y, width, height)))
    }

    /// Screen bounds of the focused component, used only when per-character
    /// caret geometry is unavailable (notably an empty text field).
    pub fn popup_anchor(&self, field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
        let id = ElementId::decode(&field.element_id)
            .ok_or_else(|| unsupported(format!("malformed element id: {}", field.element_id)))?;
        let extents = self
            .component(&id)?
            .get_extents(CoordType::Screen)
            .map_err(|err| cannot_complete("component get_extents", err))?;
        Ok(screen_rect_from_extents(extents))
    }

    /// An accessible's `Name` property. Used for diagnostics and to confirm which
    /// field a walk landed on; `None` when the object is gone or refuses it.
    pub fn element_name(&self, id: &ElementId) -> Option<String> {
        self.accessible(id).ok()?.name().ok()
    }

    /// Insert `text` at the caret through `EditableText.InsertText`.
    ///
    /// `length` is in scalars, matching AT-SPI's own unit; the returned `chars`
    /// counts the same way so caret math on the host side stays consistent.
    pub fn insert(&self, field: &FieldHandle, text: &str) -> Result<Inserted, PlatformError> {
        let id = self.element(field)?;
        let caret = self.caret_offset(&id)?;
        let editable = self.editable_text(&id)?;
        let scalars = text.chars().count();
        let inserted = editable
            .insert_text(caret, text, i32::try_from(scalars).unwrap_or(i32::MAX))
            .map_err(|err| cannot_complete("insert_text", err))?;
        if !inserted {
            return Err(cannot_complete(
                "insert_text",
                "the toolkit refused the insert",
            ));
        }
        Ok(Inserted {
            bytes: text.len(),
            chars: scalars,
            strategy: InsertStrategy::NativeRangeSet,
        })
    }

    /// Replace exactly `range` with `text`, but only while the field still holds
    /// `expected_text` there.
    ///
    /// **Why a whole-value swap and not DeleteText+InsertText.** The contract for
    /// an atomic strategy is all-or-nothing. `DeleteText` followed by `InsertText`
    /// is two D-Bus round trips: a failure between them leaves the user's field
    /// truncated, which is worse than refusing the replacement. `SetTextContents`
    /// is one call, so the field either changes completely or not at all — the
    /// same reasoning that makes macOS use an `AXValue` set.
    /// Before taking that whole-field snapshot, the adapter checks AT-SPI's
    /// character count and refuses fields above `MAX_FIELD_SCALARS`. It never
    /// reconstructs a value from a capped prefix, and it refuses a rebuilt
    /// value that would itself exceed the cap — otherwise a replacement longer
    /// than its range could mutate an at-cap field and then fail the capped
    /// readback, reporting an accept as failed after it landed.
    ///
    /// The expected-text guard is re-checked immediately before the swap, so a
    /// keystroke that landed between the suggestion and the accept invalidates the
    /// replacement instead of overwriting what the user just typed.
    pub fn insert_replacing_range(
        &self,
        field: &FieldHandle,
        expected_text: &str,
        text: &str,
        range: platform::CorrectionRange,
        strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        if !strategy.supports_atomic_range_replace() {
            return Err(unsupported(format!(
                "platform_linux: {strategy:?} cannot range-replace atomically"
            )));
        }
        if range.start > range.end {
            return Err(unsupported(format!(
                "platform_linux: inverted range {}..{}",
                range.start, range.end
            )));
        }
        let id = self.element(field)?;
        let scalars = self.field_scalars(&id)?;
        let updated = checked_replacement(&scalars, expected_text, text, range)?;

        let editable = self.editable_text(&id)?;
        if !editable
            .set_text_contents(&updated)
            .map_err(|err| cannot_complete("set_text_contents", err))?
        {
            return Err(cannot_complete(
                "set_text_contents",
                "the toolkit refused the replacement",
            ));
        }
        // Verified readback: a toolkit that accepts the call and stores something
        // else would otherwise leave the engine believing text it never wrote.
        let after: String = self.field_scalars(&id)?.iter().collect();
        if after != updated {
            return Err(cannot_complete(
                "set_text_contents",
                "readback does not match the written value",
            ));
        }
        Ok(Inserted {
            bytes: text.len(),
            chars: text.chars().count(),
            strategy: InsertStrategy::NativeRangeSet,
        })
    }

    /// The field's complete text as scalars, after rejecting an over-cap field.
    fn field_scalars(&self, id: &ElementId) -> Result<Vec<char>, PlatformError> {
        let text = self.text(id)?;
        self.field_scalars_from_text(&text)
    }

    /// Query the authoritative size before asking another process to transfer
    /// its whole value. The post-fetch check closes the race where the field
    /// grows between `CharacterCount` and `GetText`.
    fn field_scalars_from_text(
        &self,
        text: &TextProxyBlocking<'_>,
    ) -> Result<Vec<char>, PlatformError> {
        let announced_count = text
            .character_count()
            .map_err(|err| cannot_complete("character_count", err))?;
        checked_field_scalar_count(announced_count)?;
        let value = text
            .get_text(0, -1)
            .map_err(|err| cannot_complete("get_text", err))?;
        let scalars: Vec<char> = value.chars().collect();
        if scalars.len() > MAX_FIELD_SCALARS {
            return Err(field_over_cap_error());
        }
        Ok(scalars)
    }

    fn caret_offset(&self, id: &ElementId) -> Result<i32, PlatformError> {
        self.text(id)?
            .caret_offset()
            .map_err(|err| cannot_complete("caret_offset", err))
    }

    fn editable_text(
        &self,
        id: &ElementId,
    ) -> Result<EditableTextProxyBlocking<'_>, PlatformError> {
        EditableTextProxyBlocking::builder(&self.connection)
            .destination(id.bus_name.clone())
            .map_err(|err| cannot_complete("editable destination", err))?
            .path(id.path.clone())
            .map_err(|err| cannot_complete("editable path", err))?
            .build()
            .map_err(|err| cannot_complete("editable proxy", err))
    }

    /// Decode a handle's element id, failing closed on anything malformed.
    fn element(&self, field: &FieldHandle) -> Result<ElementId, PlatformError> {
        ElementId::decode(&field.element_id)
            .ok_or_else(|| unsupported(format!("malformed element id: {}", field.element_id)))
    }

    /// The application owning the focused field, for `front_app`.
    pub fn focused_app_name(&self) -> Option<String> {
        self.application_name(&self.focused_field().ok().flatten()?)
    }

    /// The `app` and `pid` for a [`FieldHandle`] built from an event, as far as the
    /// accessibility bus can answer.
    ///
    /// Best effort by construction, and cheap only relative to how rarely it runs:
    /// AT-SPI models the application as another accessible (so its name is two more
    /// round trips) and exposes no process id at all, so the pid comes from the bus's
    /// own `GetConnectionUnixProcessID`. When the name cannot be read the owning bus
    /// name stands in, because `FieldHandle::app` is logged and compared — an empty
    /// string there reads as a bug in the adapter rather than a quiet toolkit.
    pub(crate) fn element_owner(&self, id: &ElementId) -> (String, Option<u32>) {
        (
            self.application_name(id)
                .unwrap_or_else(|| id.bus_name.clone()),
            self.owner_pid(id),
        )
    }

    /// The name of the application owning `id`.
    fn application_name(&self, id: &ElementId) -> Option<String> {
        let node = self.accessible(id).ok()?;
        let app = node.get_application().ok()?;
        let app_id = ElementId::new(app.name_as_str()?, app.path_as_str());
        self.accessible(&app_id).ok()?.name().ok()
    }

    /// The unix process id behind `id`'s bus name, asked of the accessibility bus
    /// itself. `None` whenever the bus declines — a peer that has already exited is
    /// the ordinary case, not an error worth propagating into a focus event.
    fn owner_pid(&self, id: &ElementId) -> Option<u32> {
        let bus_name = atspi::zbus::names::BusName::try_from(id.bus_name.as_str()).ok()?;
        atspi::zbus::blocking::fdo::DBusProxy::new(&self.connection)
            .ok()?
            .get_connection_unix_process_id(bus_name)
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_rect_from_extents_accepts_component_bounds_and_rejects_degenerate_geometry() {
        assert_eq!(
            screen_rect_from_extents((10, 20, 300, 24)),
            Some(ScreenRect {
                x: 10.0,
                y: 20.0,
                w: 300.0,
                h: 24.0,
            })
        );
        assert_eq!(screen_rect_from_extents((10, 20, 0, 24)), None);
        assert_eq!(screen_rect_from_extents((10, 20, 300, -1)), None);
    }

    #[test]
    fn field_scalar_count_rejects_negative_and_over_cap_values() {
        assert_eq!(checked_field_scalar_count(0).expect("empty field"), 0);
        assert_eq!(
            checked_field_scalar_count(MAX_FIELD_SCALARS as i32).expect("field at cap"),
            MAX_FIELD_SCALARS
        );

        assert!(matches!(
            checked_field_scalar_count(-1),
            Err(PlatformError::UnsupportedField { reason })
                if reason == "invalid negative field scalar count: -1"
        ));
        assert!(matches!(
            checked_field_scalar_count(MAX_FIELD_SCALARS as i32 + 1),
            Err(PlatformError::UnsupportedField { reason })
                if reason == "field exceeds 200000 scalars; refusing lossy read/replace"
        ));
    }

    /// A replacement longer than the range it replaces must not push an at-cap
    /// field over the limit: the write would land and then the capped readback
    /// would report failure — the mutate-then-error shape A61 exists to forbid.
    #[test]
    fn rebuilt_field_at_cap_is_allowed_and_one_scalar_over_is_refused() {
        assert_eq!(
            checked_rebuilt_len(MAX_FIELD_SCALARS, 3, 3).expect("same-length swap at cap"),
            MAX_FIELD_SCALARS
        );
        assert_eq!(
            checked_rebuilt_len(MAX_FIELD_SCALARS, 3, 2).expect("shrinking swap at cap"),
            MAX_FIELD_SCALARS - 1
        );
        assert!(matches!(
            checked_rebuilt_len(MAX_FIELD_SCALARS, 3, 4),
            Err(PlatformError::UnsupportedField { reason })
                if reason == "field exceeds 200000 scalars; refusing lossy read/replace"
        ));
    }

    /// The same cap, asserted where `insert_replacing_range` actually applies it:
    /// on the whole pre-write sequence, over a real at-cap field. Pinning only
    /// [`checked_rebuilt_len`]'s verdict left the call site free — the check
    /// could be deleted from the replacement path with every test still green.
    #[test]
    fn at_cap_field_refuses_a_growing_replacement_and_still_builds_a_same_size_one() {
        let scalars: Vec<char> = std::iter::repeat_n('a', MAX_FIELD_SCALARS).collect();
        let range = platform::CorrectionRange { start: 0, end: 1 };
        let expected = "a";

        // One scalar out, two in: the rebuilt value is one over the cap, so the
        // replacement must be refused *before* anything is written.
        assert!(matches!(
            checked_replacement(&scalars, expected, "bc", range),
            Err(PlatformError::UnsupportedField { reason })
                if reason == "field exceeds 200000 scalars; refusing lossy read/replace"
        ));

        // Same size and shrinking stay allowed at the cap: the guard is about
        // growth past the cap, not about touching a full field at all.
        let same = checked_replacement(&scalars, expected, "b", range).expect("same-size swap");
        assert_eq!(same.chars().count(), MAX_FIELD_SCALARS);
        assert!(same.starts_with('b'));
        let shrunk = checked_replacement(&scalars, expected, "", range).expect("shrinking swap");
        assert_eq!(shrunk.chars().count(), MAX_FIELD_SCALARS - 1);
    }

    /// The other two pre-write guards, over the same sequence: a range past the
    /// field and text that changed under the replacement must both refuse
    /// without producing a value to write.
    #[test]
    fn replacement_refuses_a_range_past_the_field_and_text_that_changed_underneath() {
        let scalars: Vec<char> = "hello".chars().collect();
        assert!(matches!(
            checked_replacement(
                &scalars,
                "hello",
                "x",
                platform::CorrectionRange { start: 0, end: 9 },
            ),
            Err(PlatformError::UnsupportedField { reason })
                if reason == "platform_linux: range 0..9 past the field length 5"
        ));
        assert!(matches!(
            checked_replacement(
                &scalars,
                "help",
                "x",
                platform::CorrectionRange { start: 0, end: 4 },
            ),
            Err(PlatformError::UnsupportedField { reason })
                if reason == "platform_linux: field changed under the replacement (found \"hell\")"
        ));
        assert_eq!(
            checked_replacement(
                &scalars,
                "ell",
                "i",
                platform::CorrectionRange { start: 1, end: 4 },
            )
            .expect("in-range swap"),
            "hio"
        );
    }

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
