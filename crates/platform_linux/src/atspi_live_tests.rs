//! Live AT-SPI2 tests (ROADMAP Phase 2.1/2.2). Linux only, and `#[ignore]`d:
//! they need a real accessibility session with the GTK fixture running.
//!
//! Run them through the harness, which owns the bring-up:
//!
//! ```sh
//! tools/acceptance/run-linux-atspi-session.sh \
//!   --run-in-session cargo test -p platform_linux -- --ignored
//! ```
//!
//! They are `#[ignore]`d rather than env-sniffed on purpose: a test that decides
//! for itself whether to run reports success when the session is broken, which is
//! exactly the failure this suite exists to catch. Ignored means a plain
//! `cargo test` on any host says "ignored" out loud, and an explicit `--ignored`
//! run fails loudly if the session is not there.

use super::*;
use crate::atspi_ids::ElementId;
use crate::atspi_live::AtspiSession;
use atspi::proxy::editable_text::EditableTextProxyBlocking;
use platform::{InsertStrategy, OffsetEncoding, PlatformAdapter, SecurityState};

/// The fixture's single-line entry, by accessible name.
const FIXTURE_ENTRY: &str = "compme-fixture-entry";
/// What linux-atspi-fixture.c seeds the entry with.
const FIXTURE_TEXT: &str = "teh quick brown";

fn session() -> AtspiSession {
    AtspiSession::open().expect("the harness must provide an accessibility bus")
}

/// The fixture's focused entry. The fixture calls `gtk_widget_grab_focus` on it,
/// so the focused-field walk must land there; asserting the name catches a walk
/// that found some other application's field, which would make every following
/// assertion meaningless.
fn fixture_entry(session: &AtspiSession) -> ElementId {
    let id = session
        .focused_field()
        .expect("focused-field walk must not error")
        .expect("the fixture entry has focus, so a focused field must exist");
    let name = session.element_name(&id).unwrap_or_default();
    assert_eq!(
        name, FIXTURE_ENTRY,
        "focused-field walk landed on {name:?}, not the fixture entry"
    );
    id
}

fn handle(id: &ElementId) -> FieldHandle {
    FieldHandle {
        app: "compme-fixture".to_string(),
        pid: None,
        element_id: id.encode(),
        generation: 0,
    }
}

fn editable(session: &AtspiSession, id: &ElementId) -> EditableTextProxyBlocking<'static> {
    // Test-side write access, so the read path can be checked against text this
    // test chose. The adapter's own insert path is Phase 2.4.
    let _ = session;
    let connection = zbus::blocking::Connection::session().expect("session bus");
    let address: String = connection
        .call_method(
            Some("org.a11y.Bus"),
            "/org/a11y/bus",
            Some("org.a11y.Bus"),
            "GetAddress",
            &(),
        )
        .expect("GetAddress")
        .body()
        .deserialize()
        .expect("address");
    let a11y = zbus::blocking::connection::Builder::address(
        address.parse::<zbus::Address>().expect("parse address"),
    )
    .expect("builder")
    .build()
    .expect("a11y bus");
    EditableTextProxyBlocking::builder(&a11y)
        .destination(id.bus_name.clone())
        .expect("destination")
        .path(id.path.clone())
        .expect("path")
        .build()
        .expect("EditableText proxy")
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_focused_field_is_the_fixture_entry_and_reads_its_text() {
    let session = session();
    let id = fixture_entry(&session);
    let context = LinuxAdapter::with_accessibility()
        .read_context(&handle(&id))
        .expect("read_context");

    assert_eq!(
        format!("{}{}", context.left, context.right),
        FIXTURE_TEXT,
        "the whole field value must round-trip through left+right"
    );
    // The fixture leaves the caret at the end of the seeded text.
    assert_eq!(context.caret, FIXTURE_TEXT.chars().count());
    assert_eq!(context.left_scalars, context.left.chars().count());
    assert_eq!(context.offset_encoding, OffsetEncoding::UnicodeScalars);
    assert_eq!(context.source, platform::ContextSource::Accessibility);
    assert_eq!(context.field_id.element_id, id.encode());
    // The fixture's documented baseline: caret at the end, nothing selected. A
    // failure here should say what was selected instead of just "false".
    assert!(
        context.selection.is_none() && context.selected_text.is_none(),
        "unexpected baseline selection {:?} = {:?}",
        context.selection,
        context.selected_text
    );
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_offsets_count_scalars_not_utf16_units() {
    // The bug this test exists for: AT-SPI counts characters while AppKit and
    // Chromium count UTF-16 code units, so an adapter that assumes UTF-16 is
    // wrong by one per astral-plane scalar — and every ASCII test still passes.
    // "a😀b" is 3 scalars but 4 UTF-16 units.
    let session = session();
    let id = fixture_entry(&session);
    let adapter = LinuxAdapter::with_accessibility();
    let entry = editable(&session, &id);
    entry.set_text_contents("a😀b").expect("seed astral text");

    let context = adapter.read_context(&handle(&id)).expect("read_context");
    assert_eq!(format!("{}{}", context.left, context.right), "a😀b");
    let scalars = context.left.chars().count() + context.right.chars().count();
    assert_eq!(scalars, 3, "the field holds 3 scalars");
    assert!(
        context.caret <= 3,
        "caret {} exceeds the scalar length: offsets are being read as UTF-16 units",
        context.caret
    );
    // left/right must split on a scalar boundary — a UTF-16 split would panic or
    // produce a replacement character.
    assert!(!context.left.contains('\u{FFFD}') && !context.right.contains('\u{FFFD}'));

    entry.set_text_contents(FIXTURE_TEXT).expect("restore");
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_selection_is_reported_as_a_scalar_range_with_its_text() {
    let session = session();
    let id = fixture_entry(&session);
    let adapter = LinuxAdapter::with_accessibility();
    let text = zbus_text(&id);
    // Select "quick" in "teh quick brown" (scalars 4..9).
    text.add_selection(4, 9).expect("add_selection");

    let context = adapter.read_context(&handle(&id)).expect("read_context");
    let range = context.selection.expect("a selection was set");
    assert_eq!((range.start, range.end), (4, 9));
    assert_eq!(context.selected_text.as_deref(), Some("quick"));

    text.remove_selection(0).ok();
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_capabilities_describe_an_editable_single_line_entry() {
    let session = session();
    let id = fixture_entry(&session);
    let caps = LinuxAdapter::with_accessibility()
        .capabilities(&handle(&id))
        .expect("capabilities");

    assert!(caps.readable_text && caps.readable_caret && caps.writable);
    assert!(!caps.multiline, "a GtkEntry is single-line");
    assert!(!caps.secure);
    assert_eq!(caps.security_state, SecurityState::Normal);
    assert_eq!(caps.insert_strategy, InsertStrategy::NativeRangeSet);
    assert!(caps.coords_global_screen);
    // GTK reports its toolkit name; the exact string is the toolkit's business,
    // but it must not be empty or the Toolkit hint carries nothing.
    assert!(
        matches!(&caps.toolkit, platform::Toolkit::Unknown(name) if !name.is_empty()),
        "unexpected toolkit: {:?}",
        caps.toolkit
    );
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_caret_rect_is_real_screen_geometry() {
    let session = session();
    let id = fixture_entry(&session);
    let rect = LinuxAdapter::with_accessibility()
        .caret_rect(&handle(&id))
        .expect("caret_rect")
        .expect("a mapped entry has character geometry");

    assert!(rect.w > 0.0 && rect.h > 0.0, "degenerate rect: {rect:?}");
    // The fixture window is 480x240 inside a 1280x1024 Xvfb screen, so a caret
    // outside that box means the coordinates are not screen-global.
    assert!(
        rect.x >= 0.0 && rect.y >= 0.0 && rect.x < 1280.0 && rect.y < 1024.0,
        "caret rect off-screen: {rect:?}"
    );
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_front_app_names_the_fixture() {
    assert_eq!(
        LinuxAdapter::with_accessibility().front_app().as_deref(),
        Some("compme-fixture"),
        "front_app must name the application owning the focused field"
    );
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_malformed_and_stale_ids_fail_closed_without_panicking() {
    let adapter = LinuxAdapter::with_accessibility();
    let mut bad = handle(&ElementId::new(":1.0", "/"));
    bad.element_id = "not-an-element-id".to_string();
    assert!(matches!(
        adapter.read_context(&bad),
        Err(PlatformError::UnsupportedField { .. })
    ));
    assert!(matches!(
        adapter.capabilities(&bad),
        Err(PlatformError::UnsupportedField { .. })
    ));

    // A well-formed id for a bus name nobody owns: the D-Bus call must surface as
    // an error, never a panic or a fabricated empty context.
    let gone = handle(&ElementId::new(":1.99999", "/org/a11y/atspi/accessible/1"));
    assert!(adapter.read_context(&gone).is_err());
    assert!(adapter.capabilities(&gone).is_err());
    assert!(adapter.caret_rect(&gone).is_err());
}

/// A Text proxy for the fixture entry, for tests that need to drive selection.
fn zbus_text(id: &ElementId) -> atspi::proxy::text::TextProxyBlocking<'static> {
    let connection = zbus::blocking::Connection::session().expect("session bus");
    let address: String = connection
        .call_method(
            Some("org.a11y.Bus"),
            "/org/a11y/bus",
            Some("org.a11y.Bus"),
            "GetAddress",
            &(),
        )
        .expect("GetAddress")
        .body()
        .deserialize()
        .expect("address");
    let a11y = zbus::blocking::connection::Builder::address(
        address.parse::<zbus::Address>().expect("parse address"),
    )
    .expect("builder")
    .build()
    .expect("a11y bus");
    atspi::proxy::text::TextProxyBlocking::builder(&a11y)
        .destination(id.bus_name.clone())
        .expect("destination")
        .path(id.path.clone())
        .expect("path")
        .build()
        .expect("Text proxy")
}
