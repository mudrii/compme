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

use atspi::proxy::accessible::AccessibleProxyBlocking;

use atspi::proxy::component::ComponentProxyBlocking;

use atspi::proxy::editable_text::EditableTextProxyBlocking;

use platform::{InsertStrategy, OffsetEncoding, PlatformAdapter, SecurityState};

use std::sync::{mpsc, Arc};

use std::time::{Duration, Instant};

/// The fixture's single-line entry, by accessible name.
const FIXTURE_ENTRY: &str = "compme-fixture-entry";

/// The fixture's multi-line text view, by accessible name. The event tests move
/// focus onto it and back, which is the only way to make a focus change happen
/// without synthesizing input.
const FIXTURE_TEXTVIEW: &str = "compme-fixture-textview";

/// What linux-atspi-fixture.c seeds the entry with.
const FIXTURE_TEXT: &str = "teh quick brown";

/// What linux-atspi-fixture.c seeds the multi-line text view with.
const FIXTURE_VIEW_TEXT: &str = "Hello from the compme AT-SPI2 fixture.\nSecond line.";

/// Ceiling on waiting for an event that a correct implementation delivers in
/// milliseconds. Generous because it is only paid when something is broken.
const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

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

fn handle(adapter: &LinuxAdapter, id: &ElementId) -> FieldHandle {
    adapter.register_test_field(id)
}

/// A test-owned connection to the accessibility bus, independent of the adapter's.
/// Every proxy helper below builds on this one so the bring-up lives in one place.
fn a11y_bus() -> zbus::blocking::Connection {
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
    zbus::blocking::connection::Builder::address(
        address.parse::<zbus::Address>().expect("parse address"),
    )
    .expect("builder")
    .build()
    .expect("a11y bus")
}

fn editable(session: &AtspiSession, id: &ElementId) -> EditableTextProxyBlocking<'static> {
    // Test-side write access, so the read path can be checked against text this
    // test chose. The adapter's own insert path is Phase 2.4.
    let _ = session;
    let a11y = a11y_bus();
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
    let adapter = LinuxAdapter::with_accessibility();
    let context = adapter
        .read_context(&handle(&adapter, &id))
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

    let context = adapter
        .read_context(&handle(&adapter, &id))
        .expect("read_context");
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

    let context = adapter
        .read_context(&handle(&adapter, &id))
        .expect("read_context");
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
    let adapter = LinuxAdapter::with_accessibility();
    let caps = adapter
        .capabilities(&handle(&adapter, &id))
        .expect("capabilities");

    assert!(caps.readable_text && caps.readable_caret && caps.writable);
    assert!(!caps.multiline, "a GtkEntry is single-line");
    assert!(!caps.secure);
    assert_eq!(caps.security_state, SecurityState::Normal);
    assert_eq!(caps.insert_strategy, InsertStrategy::NativeRangeSet);
    assert!(caps.coords_global_screen);
    // GTK must not be misfolded into one of the named toolkits — that is the part
    // the contract cares about, since Toolkit drives compatibility quirks.
    //
    // The name itself is *not* asserted non-empty: at-spi2 2.60 on NixOS reports
    // one, while the 2.5x stack on Ubuntu reports an empty ToolkitName for the
    // same GTK3 app. `Toolkit` is documented as a hint, never a correctness gate,
    // so an empty name is a legitimate "unknown toolkit" rather than evidence of
    // an adapter bug — and pinning it would make this suite fail per-distribution.
    assert!(
        matches!(&caps.toolkit, platform::Toolkit::Unknown(_)),
        "GTK must map to Unknown, got {:?}",
        caps.toolkit
    );
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_caret_rect_is_real_screen_geometry() {
    let session = session();
    let id = fixture_entry(&session);
    let adapter = LinuxAdapter::with_accessibility();
    let rect = adapter
        .caret_rect(&handle(&adapter, &id))
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
fn live_empty_entry_uses_component_bounds_as_popup_anchor() {
    let session = session();
    let id = fixture_entry(&session);
    let adapter = LinuxAdapter::with_accessibility();
    editable(&session, &id)
        .set_text_contents("")
        .expect("empty fixture entry");
    let field = handle(&adapter, &id);

    assert_eq!(adapter.caret_rect(&field).expect("caret_rect"), None);
    let anchor = adapter
        .popup_anchor(&field)
        .expect("popup_anchor")
        .expect("the mapped entry has component bounds");
    assert!(
        anchor.w > 0.0 && anchor.h > 0.0,
        "degenerate anchor: {anchor:?}"
    );
    assert!(
        anchor.x >= 0.0 && anchor.y >= 0.0 && anchor.x < 1280.0 && anchor.y < 1024.0,
        "popup anchor off-screen: {anchor:?}"
    );
    let entry = editable(&session, &id);
    entry
        .set_text_contents(FIXTURE_TEXT)
        .expect("restore fixture text");
    zbus_text(&id)
        .set_caret_offset(i32::try_from(FIXTURE_TEXT.chars().count()).unwrap())
        .expect("restore fixture caret");
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
    let mut bad = handle(&adapter, &ElementId::new(":1.0", "/"));
    bad.element_id = "not-an-element-id".to_string();
    assert!(matches!(
        adapter.read_context(&bad),
        Err(PlatformError::StaleField)
    ));
    assert!(matches!(
        adapter.capabilities(&bad),
        Err(PlatformError::StaleField)
    ));

    // A well-formed id for a bus name nobody owns: the D-Bus call must surface as
    // an error, never a panic or a fabricated empty context.
    let gone = handle(
        &adapter,
        &ElementId::new(":1.99999", "/org/a11y/atspi/accessible/1"),
    );
    assert!(adapter.read_context(&gone).is_err());
    assert!(adapter.capabilities(&gone).is_err());
    assert!(adapter.caret_rect(&gone).is_err());
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_every_atspi_io_path_rejects_a_stale_generation_before_io() {
    let session = session();
    let entry = fixture_entry(&session);
    let textview = fixture_sibling(&entry, FIXTURE_TEXTVIEW);
    let adapter = LinuxAdapter::with_accessibility();
    let stale = handle(&adapter, &entry);
    let _current = handle(&adapter, &textview);
    let range = platform::CorrectionRange { start: 0, end: 1 };

    assert_eq!(adapter.capabilities(&stale), Err(PlatformError::StaleField));
    assert_eq!(adapter.read_context(&stale), Err(PlatformError::StaleField));
    assert_eq!(adapter.caret_rect(&stale), Err(PlatformError::StaleField));
    assert_eq!(
        adapter.insert(&stale, "x", InsertStrategy::NativeRangeSet),
        Err(PlatformError::StaleField)
    );
    assert_eq!(
        adapter.insert_replacing_range(&stale, "t", "x", range, InsertStrategy::NativeRangeSet,),
        Err(PlatformError::StaleField)
    );
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_insert_puts_text_at_the_caret() {
    let session = session();
    let id = fixture_entry(&session);
    let adapter = LinuxAdapter::with_accessibility();
    let entry = editable(&session, &id);
    entry.set_text_contents("abc").expect("seed");
    zbus_text(&id).set_caret_offset(3).expect("caret to end");

    let inserted = adapter
        .insert(&handle(&adapter, &id), "XY", InsertStrategy::NativeRangeSet)
        .expect("insert");
    assert_eq!(inserted.chars, 2);
    assert_eq!(inserted.bytes, 2);
    assert_eq!(inserted.strategy, InsertStrategy::NativeRangeSet);

    let context = adapter
        .read_context(&handle(&adapter, &id))
        .expect("read back");
    assert_eq!(format!("{}{}", context.left, context.right), "abcXY");

    entry.set_text_contents(FIXTURE_TEXT).expect("restore");
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_range_replace_swaps_exactly_the_range() {
    // The grammar-fix shape: correct "teh" to "the" without touching the rest.
    let session = session();
    let id = fixture_entry(&session);
    let adapter = LinuxAdapter::with_accessibility();
    let entry = editable(&session, &id);
    entry.set_text_contents(FIXTURE_TEXT).expect("seed");

    let inserted = adapter
        .insert_replacing_range(
            &handle(&adapter, &id),
            "teh",
            "the",
            platform::CorrectionRange { start: 0, end: 3 },
            InsertStrategy::NativeRangeSet,
        )
        .expect("range replace");
    assert_eq!(inserted.chars, 3);

    let context = adapter
        .read_context(&handle(&adapter, &id))
        .expect("read back");
    assert_eq!(
        format!("{}{}", context.left, context.right),
        "the quick brown"
    );

    entry.set_text_contents(FIXTURE_TEXT).expect("restore");
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_range_replace_refuses_a_stale_or_impossible_range_and_leaves_the_field_alone() {
    // The safety property that matters most: if the field moved under the
    // suggestion, the replacement must be refused rather than overwrite whatever
    // the user typed in the meantime. Each rejection is checked to leave the field
    // byte-identical, because a partial write is worse than no write.
    let session = session();
    let id = fixture_entry(&session);
    let adapter = LinuxAdapter::with_accessibility();
    let entry = editable(&session, &id);
    entry.set_text_contents(FIXTURE_TEXT).expect("seed");
    let range = platform::CorrectionRange { start: 0, end: 3 };

    for (label, expected, replace_range) in [
        ("expected text no longer present", "zzz", range),
        (
            "range past the end",
            "teh",
            platform::CorrectionRange { start: 0, end: 999 },
        ),
        (
            "inverted range",
            "teh",
            platform::CorrectionRange { start: 5, end: 2 },
        ),
    ] {
        let result = adapter.insert_replacing_range(
            &handle(&adapter, &id),
            expected,
            "REPLACED",
            replace_range,
            InsertStrategy::NativeRangeSet,
        );
        assert!(
            matches!(result, Err(PlatformError::UnsupportedField { .. })),
            "{label} must fail closed, got {result:?}"
        );
        let context = adapter
            .read_context(&handle(&adapter, &id))
            .expect("read back");
        assert_eq!(
            format!("{}{}", context.left, context.right),
            FIXTURE_TEXT,
            "{label} must leave the field untouched"
        );
    }

    // A non-atomic strategy is refused before anything is read or written.
    for strategy in [
        InsertStrategy::SyntheticKeys,
        InsertStrategy::Clipboard,
        InsertStrategy::ImeCommit,
        InsertStrategy::None,
    ] {
        assert!(
            adapter
                .insert_replacing_range(&handle(&adapter, &id), "teh", "the", range, strategy,)
                .is_err(),
            "{strategy:?} must not range-replace"
        );
    }
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_over_cap_field_refuses_read_and_replace_without_mutation() {
    // The old implementation took only the first 200,000 scalars, rebuilt that
    // capped snapshot, and wrote it back. Replacing one scalar with one scalar
    // therefore returned success while silently deleting the final character.
    let session = session();
    let entry = fixture_entry(&session);
    let textview = fixture_sibling(&entry, FIXTURE_TEXTVIEW);
    let adapter = LinuxAdapter::with_accessibility();
    let editable = editable(&session, &textview);
    let text = zbus_text(&textview);
    let oversized = "x".repeat(200_001);
    editable
        .set_text_contents(&oversized)
        .expect("seed cap-crossing text view");
    let before_count = text.character_count().expect("count seeded text");

    let read = adapter.read_context(&handle(&adapter, &textview));
    let replace = adapter.insert_replacing_range(
        &handle(&adapter, &textview),
        "x",
        "y",
        platform::CorrectionRange { start: 0, end: 1 },
        InsertStrategy::NativeRangeSet,
    );
    let after_count = text.character_count().expect("count after refusal");

    // Restore before asserting, so a failed regression does not poison every
    // later live test with a huge fixture value.
    editable
        .set_text_contents(FIXTURE_VIEW_TEXT)
        .expect("restore text view");

    assert!(
        matches!(read, Err(PlatformError::UnsupportedField { .. })),
        "an over-cap field must not produce a lossy context, got {read:?}"
    );
    assert!(
        matches!(replace, Err(PlatformError::UnsupportedField { .. })),
        "an over-cap field must not be replaced, got {replace:?}"
    );
    assert_eq!(before_count, 200_001, "fixture must cross the exact cap");
    assert_eq!(
        after_count, before_count,
        "a refused replacement must not truncate the field"
    );
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_insert_replacing_left_stays_fail_closed() {
    // Deliberate: `replace_left` would need DeleteText + InsertText, two round
    // trips, so a failure between them truncates the user's field. The engine has
    // an atomic route (insert_replacing_range) and must use it.
    let session = session();
    let id = fixture_entry(&session);
    let adapter = LinuxAdapter::with_accessibility();
    let before = adapter.read_context(&handle(&adapter, &id)).expect("read");

    let result = adapter.insert_replacing(
        &handle(&adapter, &id),
        "the",
        3,
        InsertStrategy::NativeRangeSet,
    );
    assert!(matches!(
        result,
        Err(PlatformError::UnsupportedField { .. })
    ));
    let after = adapter.read_context(&handle(&adapter, &id)).expect("read");
    assert_eq!(
        format!("{}{}", before.left, before.right),
        format!("{}{}", after.left, after.right)
    );
}

// ---------------------------------------------------------------------------
// Phase 2.5: the override-redirect X11 overlay.
//
// The properties asserted here are the ones that are *observable* through the X11
// protocol from a second connection, which is stronger evidence than the
// presenter reporting on itself: the window exists, is override-redirect, is
// mapped, sits at the caret, holds no input focus, has an empty input region
// (click-through), is reused by `update_ghost`, and disappears on `hide()`. Plus
// the one real proof that text rendered: the root framebuffer changes where the
// glyphs go.
//
// What these cannot prove headlessly: that the glyphs are *legible*, correctly
// coloured, correctly anti-aliased, or vertically aligned with the field's own
// text. Those need a live LOOK on a real desktop (see docs/ROADMAP.md §1.1).
// ---------------------------------------------------------------------------

use crate::overlay_geometry;

use platform::OverlayPresenter;

use x11rb::connection::Connection as _;

use x11rb::protocol::shape::{self, ConnectionExt as _};

use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, MapState};

/// A second X11 connection, so every assertion below reads the server's own view
/// of the overlay rather than the presenter's bookkeeping.
fn x11() -> (x11rb::rust_connection::RustConnection, u32, (u16, u16)) {
    let (conn, screen_num) = x11rb::connect(None).expect("the harness must provide a display");
    let screen = &conn.setup().roots[screen_num];
    let facts = (
        screen.root,
        (screen.width_in_pixels, screen.height_in_pixels),
    );
    (conn, facts.0, facts.1)
}

/// The fixture's caret rect, which is where the ghost must appear.
fn fixture_caret_rect() -> ScreenRect {
    let session = session();
    let id = fixture_entry(&session);
    let adapter = LinuxAdapter::with_accessibility();
    adapter
        .caret_rect(&handle(&adapter, &id))
        .expect("caret_rect")
        .expect("a mapped entry has character geometry")
}

/// Pixels of the root window over `rect` — what is actually on screen there.
fn root_pixels(
    conn: &x11rb::rust_connection::RustConnection,
    root: u32,
    x: i16,
    y: i16,
    w: u16,
    h: u16,
) -> Vec<u8> {
    conn.get_image(ImageFormat::Z_PIXMAP, root, x, y, w, h, !0)
        .expect("get_image")
        .reply()
        .expect("get_image reply")
        .data
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_ghost_overlay_is_an_override_redirect_clickthrough_window_at_the_caret() {
    let caret = fixture_caret_rect();
    let (conn, _root, screen) = x11();
    let focus_before = conn
        .get_input_focus()
        .expect("get_input_focus")
        .reply()
        .expect("focus reply")
        .focus;

    let mut overlay = LinuxOverlayPresenter::new();
    overlay
        .show_ghost(caret, "quick brown fox")
        .expect("show_ghost must succeed in a real X session");
    let window = overlay
        .text_window_id()
        .expect("a successful show_ghost must have created a window");

    let attrs = conn
        .get_window_attributes(window)
        .expect("get_window_attributes")
        .reply()
        .expect("attributes reply");
    assert!(
        attrs.override_redirect,
        "the overlay must be unmanaged: a managed window gets decorations, is \
         reparented, and can be handed the focus"
    );
    assert_eq!(
        attrs.map_state,
        MapState::VIEWABLE,
        "a shown ghost must be mapped and viewable"
    );

    let geometry = conn
        .get_geometry(window)
        .expect("get_geometry")
        .reply()
        .expect("geometry reply");
    assert_eq!(
        f64::from(geometry.x),
        caret.x,
        "the ghost starts at the caret's left edge"
    );
    // No Y-flip: X11 root coordinates and AT-SPI screen coordinates share a
    // top-left origin. A ported Cocoa flip would land this near
    // `screen_height - caret.y`, which this assertion is what catches.
    let expected_h = overlay_geometry::ghost_box_height(caret);
    assert_eq!(f64::from(geometry.height), expected_h);
    assert_eq!(
        f64::from(geometry.y),
        caret.y - 2.0,
        "the box hugs the caret line, lifted by its 2px pad"
    );
    assert!(geometry.width > 0, "a ghost with text needs a width");
    assert!(
        i32::from(geometry.x) + i32::from(geometry.width) <= i32::from(screen.0)
            && i32::from(geometry.y) + i32::from(geometry.height) <= i32::from(screen.1),
        "the overlay must be clamped inside the root window: {geometry:?} vs {screen:?}"
    );

    // Click-through. Not cosmetic: without an empty input region the ghost would
    // swallow the user's clicks on their own text field.
    let input_shape = conn
        .shape_get_rectangles(window, shape::SK::INPUT)
        .expect("shape_get_rectangles")
        .reply()
        .expect("input shape reply");
    assert!(
        input_shape.rectangles.is_empty(),
        "the input region must be empty, got {:?}",
        input_shape.rectangles
    );

    // Never takes focus. Override-redirect plus never calling SetInputFocus, so
    // the fixture keeps the keyboard.
    let focus_after = conn
        .get_input_focus()
        .expect("get_input_focus")
        .reply()
        .expect("focus reply")
        .focus;
    assert_ne!(focus_after, window, "the overlay must never hold the focus");
    assert_eq!(
        focus_after, focus_before,
        "showing a ghost must not move the input focus at all"
    );

    overlay.hide().expect("hide");
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_ghost_overlay_update_reuses_the_window_and_hide_is_idempotent() {
    let caret = fixture_caret_rect();
    let (conn, _root, _screen) = x11();
    let mut overlay = LinuxOverlayPresenter::new();

    overlay.show_ghost(caret, "one").expect("show_ghost");
    let window = overlay.text_window_id().expect("window");
    let first_width = conn
        .get_geometry(window)
        .expect("get_geometry")
        .reply()
        .expect("reply")
        .width;

    // A ghost is re-rendered on every keystroke. Creating a window per update
    // would leak X resources for the whole session, so this pins reuse.
    overlay
        .update_ghost("one two three four")
        .expect("update_ghost");
    assert_eq!(
        overlay.text_window_id(),
        Some(window),
        "update_ghost must re-render the same window, not create a new one"
    );
    let second_width = conn
        .get_geometry(window)
        .expect("get_geometry")
        .reply()
        .expect("reply")
        .width;
    assert!(
        second_width > first_width,
        "the window must resize to the longer text: {first_width} -> {second_width}"
    );

    // hide() withdraws it and is safe to repeat — the host calls it to reconcile.
    overlay.hide().expect("hide");
    assert_eq!(
        conn.get_window_attributes(window)
            .expect("attrs")
            .reply()
            .expect("reply")
            .map_state,
        MapState::UNMAPPED,
        "hide must unmap the window"
    );
    overlay.hide().expect("hide is idempotent");
    overlay.hide().expect("and again");

    // update_ghost after hide has no ghost to update, and must say so rather than
    // silently re-showing something the engine believes is gone.
    assert!(matches!(
        overlay.update_ghost("nope"),
        Err(PlatformError::CannotComplete { .. })
    ));

    // Re-showing maps the same window again.
    overlay.show_ghost(caret, "again").expect("re-show");
    assert_eq!(overlay.text_window_id(), Some(window));
    assert_eq!(
        conn.get_window_attributes(window)
            .expect("attrs")
            .reply()
            .expect("reply")
            .map_state,
        MapState::VIEWABLE
    );
    overlay.hide().expect("hide");
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_ghost_overlay_actually_draws_pixels_where_the_text_goes() {
    // The only headless proof that anything *rendered*: compare the root
    // framebuffer over a fixed region before and after the show. A window that
    // maps but draws nothing — no font, an empty bounding shape, a byte order that
    // wrote into the alpha byte — leaves the screen untouched and fails here.
    let caret = fixture_caret_rect();
    let (conn, root, screen) = x11();
    let mut overlay = LinuxOverlayPresenter::new();

    // Anchor over bare root, clear of the fixture's 480x240 window. Two reasons:
    // the baseline is a static colour that nobody has to repaint, and the `before`
    // grab can therefore be taken FIRST — the earlier version of this test grabbed
    // `before` after `hide()` and compared against a region GTK had not yet
    // redrawn, so a correctly drawn ghost still read as "no change".
    let anchor = ScreenRect {
        x: f64::from(screen.0) - 400.0,
        y: f64::from(screen.1) - 200.0,
        ..caret
    };
    // A generous fixed region around the anchor, so the compared pixels do not
    // depend on the window box the renderer happens to choose.
    let (region_x, region_y, region_w, region_h) =
        (anchor.x as i16 - 4, anchor.y as i16 - 8, 240_u16, 40_u16);
    let before = root_pixels(&conn, root, region_x, region_y, region_w, region_h);

    overlay
        .show_ghost(anchor, "Hlxy quick")
        .expect("show_ghost");
    let window = overlay.text_window_id().expect("window");
    let after = root_pixels(&conn, root, region_x, region_y, region_w, region_h);

    assert_eq!(after.len(), before.len(), "same region, same buffer size");
    let changed = after
        .iter()
        .zip(before.iter())
        .filter(|(a, b)| a != b)
        .count();
    // Ten glyphs at a 15px size cover well over ten pixels, so a handful of
    // changed bytes would mean something other than text was drawn.
    assert!(
        changed >= 30,
        "the ghost drew (almost) nothing: only {changed} of {} bytes over \
         {region_w}x{region_h} at {region_x},{region_y} changed (font: check \
         COMPME_FONT / XDG_DATA_DIRS)",
        after.len()
    );

    // ...and the change must be *inside* the window the presenter reported, not
    // somewhere else on screen.
    let geometry = conn
        .get_geometry(window)
        .expect("get_geometry")
        .reply()
        .expect("reply");
    assert!(
        i32::from(geometry.x) >= i32::from(region_x)
            && i32::from(geometry.y) >= i32::from(region_y)
            && i32::from(geometry.x) + i32::from(geometry.width)
                <= i32::from(region_x) + i32::from(region_w)
            && i32::from(geometry.y) + i32::from(geometry.height)
                <= i32::from(region_y) + i32::from(region_h),
        "the changed pixels must be attributable to the overlay window: \
         {geometry:?} is not inside {region_x},{region_y} {region_w}x{region_h}"
    );

    overlay.hide().expect("hide");
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_correction_overlay_underlines_the_word_and_banners_the_suggestion() {
    // The grammar-fix affordance: a thin underline under the word plus a banner
    // above it carrying the suggestion (docs/superpowers/specs/
    // 2026-07-01-grammar-fix-design.md §G4).
    let caret = fixture_caret_rect();
    let word = ScreenRect {
        x: caret.x,
        y: caret.y + 200.0,
        w: 40.0,
        h: caret.h,
    };
    let (conn, _root, _screen) = x11();
    let mut overlay = LinuxOverlayPresenter::new();
    overlay
        .show_correction(word, "the")
        .expect("show_correction must succeed in a real X session");

    let banner = overlay.text_window_id().expect("banner window");
    let underline = overlay.underline_window_id().expect("underline window");
    assert_ne!(
        banner, underline,
        "the banner and the underline are separate windows, so the transparent \
         gap between them does not have to be composited"
    );

    let banner_geometry = conn
        .get_geometry(banner)
        .expect("banner geometry")
        .reply()
        .expect("reply");
    let underline_geometry = conn
        .get_geometry(underline)
        .expect("underline geometry")
        .reply()
        .expect("reply");

    assert_eq!(
        f64::from(underline_geometry.y),
        word.y + word.h,
        "the underline sits flush under the word box"
    );
    assert_eq!(underline_geometry.height, overlay_geometry::UNDERLINE_H);
    assert_eq!(f64::from(underline_geometry.width), word.w);
    assert!(
        f64::from(banner_geometry.y) + f64::from(banner_geometry.height) <= word.y,
        "the banner must sit entirely above the word it describes: {banner_geometry:?} vs {word:?}"
    );
    assert!(
        f64::from(banner_geometry.width) >= word.w,
        "the banner must be at least as wide as the word"
    );

    for (label, window) in [("banner", banner), ("underline", underline)] {
        let attrs = conn
            .get_window_attributes(window)
            .expect("attrs")
            .reply()
            .expect("reply");
        assert!(attrs.override_redirect, "{label} must be override-redirect");
        assert_eq!(
            attrs.map_state,
            MapState::VIEWABLE,
            "{label} must be mapped"
        );
        assert!(
            conn.shape_get_rectangles(window, shape::SK::INPUT)
                .expect("shape")
                .reply()
                .expect("reply")
                .rectangles
                .is_empty(),
            "{label} must be click-through"
        );
    }

    // Showing a ghost afterwards must withdraw the underline: a stale underline
    // under a word the engine is no longer correcting is a lie about state.
    overlay.show_ghost(caret, "ghost").expect("show_ghost");
    assert_eq!(
        conn.get_window_attributes(underline)
            .expect("attrs")
            .reply()
            .expect("reply")
            .map_state,
        MapState::UNMAPPED,
        "show_ghost must withdraw the correction underline"
    );

    overlay.hide().expect("hide");
    for (label, window) in [("banner", banner), ("underline", underline)] {
        assert_eq!(
            conn.get_window_attributes(window)
                .expect("attrs")
                .reply()
                .expect("reply")
                .map_state,
            MapState::UNMAPPED,
            "hide must withdraw the {label}"
        );
    }
}

/// A Text proxy for the fixture entry, for tests that need to drive selection.
fn zbus_text(id: &ElementId) -> atspi::proxy::text::TextProxyBlocking<'static> {
    let a11y = a11y_bus();
    atspi::proxy::text::TextProxyBlocking::builder(&a11y)
        .destination(id.bus_name.clone())
        .expect("destination")
        .path(id.path.clone())
        .expect("path")
        .build()
        .expect("Text proxy")
}

/// The fixture's other field, found by walking the focused entry's parent.
///
/// The event tests need a *second* focusable field: AT-SPI only emits
/// `state-changed:focused` on a focus *change*, and the fixture starts with the entry
/// already focused, so there is nothing to observe until focus moves elsewhere.
fn fixture_sibling(entry: &ElementId, name: &str) -> ElementId {
    let a11y = a11y_bus();
    let accessible = |id: &ElementId| {
        AccessibleProxyBlocking::builder(&a11y)
            .destination(id.bus_name.clone())
            .expect("destination")
            .path(id.path.clone())
            .expect("path")
            .build()
            .expect("Accessible proxy")
    };
    let parent = accessible(entry).parent().expect("the entry has a parent");
    let parent = ElementId::new(
        parent.name_as_str().expect("parent bus name"),
        parent.path_as_str(),
    );
    accessible(&parent)
        .get_children()
        .expect("parent children")
        .into_iter()
        .find_map(|child| {
            let id = ElementId::new(child.name_as_str()?, child.path_as_str());
            (accessible(&id).name().ok()? == name).then_some(id)
        })
        .unwrap_or_else(|| panic!("the fixture must expose a sibling named {name}"))
}

/// Move the keyboard focus onto `id` through AT-SPI's own `Component.GrabFocus`.
///
/// Deliberately not synthetic X input: XTEST would drag in a second mechanism (and a
/// link dependency) to test the event path, while `GrabFocus` is a real toolkit focus
/// change — GTK runs the same code path a user's Tab key would.
fn grab_focus(id: &ElementId) {
    let a11y = a11y_bus();
    let grabbed = ComponentProxyBlocking::builder(&a11y)
        .destination(id.bus_name.clone())
        .expect("destination")
        .path(id.path.clone())
        .expect("path")
        .build()
        .expect("Component proxy")
        .grab_focus()
        .expect("GrabFocus");
    assert!(grabbed, "the toolkit refused to focus {}", id.encode());
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_focus_events_deliver_a_readable_field_and_stop_when_dropped() {
    let session = session();
    let entry = fixture_entry(&session);
    let textview = fixture_sibling(&entry, FIXTURE_TEXTVIEW);
    let adapter = LinuxAdapter::with_accessibility();

    let (tx, rx) = mpsc::channel();
    let cb: FocusCallback = Arc::new(move |field| {
        let _ = tx.send(field);
    });
    // A47: the event path and the accept path share one process-wide id
    // counter. Bracketing the real call proves *this* consumer draws from it —
    // a second counter reintroduced in `atspi_events::into_subscription` would
    // hand out an id outside this window.
    let before_id = crate::next_subscription_id();
    let subscription = adapter
        .subscribe_focus(Arc::clone(&cb))
        .expect("subscribe_focus");
    let after_id = crate::next_subscription_id();
    assert!(
        before_id < subscription.id() && subscription.id() < after_id,
        "subscribe_focus must mint its id from the shared subscription counter: \
         {before_id} < {} < {after_id}",
        subscription.id()
    );

    // Focus the text view, then the entry again: two real focus changes, ending on
    // the fixture's documented baseline so the rest of the suite is unaffected.
    grab_focus(&textview);
    let moved = rx.recv_timeout(EVENT_TIMEOUT).expect("focus event");
    assert_eq!(moved.element_id, textview.encode());
    assert_eq!(
        moved.app, "compme-fixture",
        "the handle must name the owning application"
    );
    assert!(
        moved.pid.is_some_and(|pid| pid > 0),
        "the bus knows the owner's pid: {:?}",
        moved.pid
    );

    // Exactly one delivery per focus change. GTK emits the underlying
    // `state-changed:focused` signal twice, and letting the duplicate through would
    // make the host re-probe capabilities and re-read the field for no news — this
    // pins the suppression, and would fail loudly if the toolkit stopped doubling.
    assert!(
        rx.recv_timeout(Duration::from_millis(500)).is_err(),
        "a consecutive duplicate focus event must be suppressed"
    );

    grab_focus(&entry);
    let back = rx.recv_timeout(EVENT_TIMEOUT).expect("second focus event");
    assert_eq!(back.element_id, entry.encode());
    assert_ne!(
        back.generation, moved.generation,
        "a different element must not reuse a live handle's generation"
    );
    // The point of the whole event half: the handle it delivers is directly usable by
    // the read path, with no id translation in between.
    let context = adapter.read_context(&back).expect("read_context");
    assert_eq!(
        format!("{}{}", context.left, context.right),
        FIXTURE_TEXT,
        "the focus handle must address the entry the read path reads"
    );
    assert!(adapter.capabilities(&back).expect("capabilities").writable);

    // Dropping must stop delivery *and* retire the worker threads. The Arc count is
    // the proof of the second half: the dispatcher thread holds the only other clone
    // of the callback, so it can only fall back to 1 once that thread has exited.
    drop(subscription);
    assert_eq!(
        Arc::strong_count(&cb),
        1,
        "the worker threads must be joined before the Subscription drop returns"
    );
    while rx.try_recv().is_ok() {} // events already in flight when we cancelled
    grab_focus(&textview);
    grab_focus(&entry);
    assert!(
        rx.recv_timeout(Duration::from_millis(500)).is_err(),
        "a dropped subscription must not deliver another focus event"
    );
}

#[test]
#[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
fn live_caret_events_deliver_on_screen_geometry_and_stop_when_dropped() {
    let session = session();
    let entry = fixture_entry(&session);
    let adapter = LinuxAdapter::with_accessibility();
    let focused = handle(&adapter, &entry);
    let text = zbus_text(&entry);

    let (tx, rx) = mpsc::channel();
    let cb: CaretCallback = Arc::new(move |field, rect| {
        let _ = tx.send((field, rect));
    });
    let subscription = adapter
        .subscribe_caret(Arc::clone(&cb))
        .expect("subscribe_caret");

    text.set_caret_offset(0).expect("caret to the start");
    let (field, rect) = rx.recv_timeout(EVENT_TIMEOUT).expect("caret event");
    assert_eq!(field, focused, "caret must reuse the current focus handle");
    let rect = rect.expect("a mapped entry has character geometry");
    assert!(rect.w > 0.0 && rect.h > 0.0, "degenerate rect: {rect:?}");
    // Same bound as the caret_rect test: the fixture lives inside a 1280x1024 Xvfb
    // screen, so an off-screen rect means these are not global screen coordinates.
    assert!(
        rect.x >= 0.0 && rect.y >= 0.0 && rect.x < 1280.0 && rect.y < 1024.0,
        "caret rect off-screen: {rect:?}"
    );

    // Coalescing must not swallow the *final* position of a burst: whatever else it
    // drops, the last event it delivers has to be the caret's resting place. That is
    // the property a naive throttle gets wrong — it drops the trailing event and
    // leaves the overlay one keystroke behind wherever the user stopped typing.
    for offset in 1..=FIXTURE_TEXT.chars().count() {
        text.set_caret_offset(i32::try_from(offset).unwrap())
            .expect("caret move");
    }
    let resting = adapter.caret_rect(&field).expect("caret_rect");
    let mut last = None;
    let deadline = Instant::now() + EVENT_TIMEOUT;
    // Drain until the bus goes quiet for far longer than the coalescing interval.
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok((_, rect)) => last = Some(rect),
            Err(_) => break,
        }
    }
    assert_eq!(
        last.expect("the caret burst must deliver at least one event"),
        resting,
        "the last coalesced event must report the caret's resting position"
    );

    drop(subscription);
    assert_eq!(
        Arc::strong_count(&cb),
        1,
        "the worker threads must be joined before the Subscription drop returns"
    );
    while rx.try_recv().is_ok() {}
    text.set_caret_offset(0).expect("caret to the start");
    assert!(
        rx.recv_timeout(Duration::from_millis(500)).is_err(),
        "a dropped subscription must not deliver another caret event"
    );
    // Restore the fixture's documented baseline (caret at the end, no selection).
    text.set_caret_offset(i32::try_from(FIXTURE_TEXT.chars().count()).unwrap())
        .expect("caret back to the end");
}

/// Live X11 accept-tap tests (ROADMAP Phase 2.3).
///
/// These mirror `tools/acceptance/linux-keytap-spike.c`'s four observations
/// against the same GTK fixture, which logs every key it receives:
///
/// | leg | what it proves |
/// |-----|----------------|
/// | baseline (unarmed) | the rig can deliver a synthetic key to the app |
/// | consume (armed) | `AsyncKeyboard` keeps the key from the app |
/// | pass-through | `ReplayKeyboard` hands the key to the app untouched |
/// | post-teardown | the grab is really gone, in every application |
///
/// The baseline and post-teardown legs are not ceremony: without them a rig where
/// no key ever arrives scores identically to a perfect consume.
///
/// Keys are synthesized with XTEST, which the X server cannot distinguish from
/// hardware input, so these exercise the passive grab exactly as a real keypress
/// would.
mod x11_accept_tap {
    use super::*;
    use crate::x11_keys::keycode_for_keysym;
    use platform::{AcceptAction, AcceptCallback, AcceptSubscription, TapControl};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use x11rb::connection::Connection;
    // `ConnectionExt` is not re-imported here: the parent module already brings it
    // in, and `use super::*` carries it into this one (a merge artifact from the
    // overlay and accept-tap work landing in the same file).
    use x11rb::protocol::xproto::{Window, KEY_PRESS_EVENT, KEY_RELEASE_EVENT};
    use x11rb::protocol::xtest::ConnectionExt as _;
    use x11rb::rust_connection::RustConnection;

    const KEYSYM_TAB: u32 = 0xff09;
    const KEYSYM_GRAVE: u32 = 0x0060;
    const KEYSYM_ESCAPE: u32 = 0xff1b;
    const KEYSYM_DOWN: u32 = 0xff54;
    const KEYSYM_CONTROL_L: u32 = 0xffe3;
    const KEYSYM_RETURN: u32 = 0xff0d;
    /// Generous, like the spike's: a miss costs the full wait, while a false "not
    /// received" would invert the verdict.
    const KEY_WAIT: Duration = Duration::from_millis(2000);
    /// How long a key gets to *fail* to arrive before "not delivered" is asserted.
    const LEAK_WAIT: Duration = Duration::from_millis(300);

    /// The fixture's log, where every received key appears as `KEY <name>`.
    fn fixture_log() -> std::path::PathBuf {
        std::path::PathBuf::from(
            std::env::var_os("COMPME_ATSPI_FIXTURE_LOG")
                .expect("the harness exports COMPME_ATSPI_FIXTURE_LOG"),
        )
    }

    fn key_count(name: &str) -> usize {
        std::fs::read_to_string(fixture_log())
            .unwrap_or_default()
            .lines()
            .filter(|line| line.trim_end() == format!("KEY {name}"))
            .count()
    }

    /// Poll until the fixture has logged `target` occurrences, or the budget runs
    /// out; returns what was actually seen either way so a failure reports it.
    fn wait_for_key_count(name: &str, target: usize) -> usize {
        let deadline = Instant::now() + KEY_WAIT;
        let mut seen = key_count(name);
        while seen < target && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            seen = key_count(name);
        }
        seen
    }

    /// A second X connection, used only to synthesize keys — the adapter owns its
    /// own, exactly as it would in production.
    fn xtest() -> (RustConnection, Window) {
        let (conn, screen) = x11rb::connect(None).expect("the harness provides a DISPLAY");
        conn.xtest_get_version(2, 2)
            .expect("XTEST request")
            .reply()
            .expect("the harness's Xvfb must have XTEST");
        let root = conn.setup().roots[screen].root;
        (conn, root)
    }

    fn keycode(conn: &RustConnection, keysym: u32) -> u8 {
        let setup = conn.setup();
        let min = setup.min_keycode;
        let count = setup.max_keycode - min + 1;
        let mapping = conn
            .get_keyboard_mapping(min, count)
            .expect("mapping request")
            .reply()
            .expect("mapping reply");
        keycode_for_keysym(min, mapping.keysyms_per_keycode, &mapping.keysyms, keysym)
            .unwrap_or_else(|| panic!("this layout has no keycode for keysym {keysym:#x}"))
    }

    /// Restores a whole-server keyboard-map mutation even when a live assertion
    /// panics. Leaving Xvfb's map altered would make every later test misleading.
    struct KeyboardMappingGuard<'a> {
        conn: &'a RustConnection,
        min: u8,
        count: u8,
        keysyms_per_keycode: u8,
        keysyms: Vec<u32>,
        restored: bool,
    }

    impl KeyboardMappingGuard<'_> {
        fn restore(&mut self) {
            if self.restored {
                return;
            }
            self.conn
                .change_keyboard_mapping(
                    self.count,
                    self.min,
                    self.keysyms_per_keycode,
                    &self.keysyms,
                )
                .expect("restore mapping")
                .check()
                .expect("restore mapping reply");
            self.conn.flush().expect("flush restored mapping");
            self.restored = true;
        }
    }

    impl Drop for KeyboardMappingGuard<'_> {
        fn drop(&mut self) {
            if self.restored {
                return;
            }
            if let Ok(cookie) = self.conn.change_keyboard_mapping(
                self.count,
                self.min,
                self.keysyms_per_keycode,
                &self.keysyms,
            ) {
                cookie.ignore_error();
                let _ = self.conn.flush();
            }
        }
    }

    fn tap_key(conn: &RustConnection, root: Window, keysym: u32) {
        let code = keycode(conn, keysym);
        for press in [KEY_PRESS_EVENT, KEY_RELEASE_EVENT] {
            conn.xtest_fake_input(press, code, 0, root, 0, 0, 0)
                .expect("XTEST fake key")
                .ignore_error();
        }
        conn.flush().expect("flush");
    }

    /// Press `modifier`, tap `keysym`, release `modifier` — a real modified chord,
    /// with the modifier physically held so the KeyPress carries its bit.
    fn tap_key_with_modifier(conn: &RustConnection, root: Window, modifier: u32, keysym: u32) {
        let modifier_code = keycode(conn, modifier);
        conn.xtest_fake_input(KEY_PRESS_EVENT, modifier_code, 0, root, 0, 0, 0)
            .expect("XTEST modifier press")
            .ignore_error();
        tap_key(conn, root, keysym);
        conn.xtest_fake_input(KEY_RELEASE_EVENT, modifier_code, 0, root, 0, 0, 0)
            .expect("XTEST modifier release")
            .ignore_error();
        conn.flush().expect("flush");
    }

    /// A connection to the accessibility bus, for the focus restore below.
    fn a11y_bus() -> zbus::blocking::Connection {
        let session = zbus::blocking::Connection::session().expect("session bus");
        let address: String = session
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
        zbus::blocking::connection::Builder::address(
            address.parse::<zbus::Address>().expect("parse address"),
        )
        .expect("builder")
        .build()
        .expect("a11y bus")
    }

    /// Depth-bounded search of the accessibility tree for an accessible with this
    /// name. By *name*, deliberately: the adapter's own walk finds the *focused*
    /// field, and the focus is exactly what has moved away by the time this runs.
    fn find_by_name(
        conn: &zbus::blocking::Connection,
        id: &ElementId,
        want: &str,
        depth: usize,
    ) -> Option<ElementId> {
        if depth > 16 {
            return None;
        }
        let node = atspi::proxy::accessible::AccessibleProxyBlocking::builder(conn)
            .destination(id.bus_name.clone())
            .ok()?
            .path(id.path.clone())
            .ok()?
            .build()
            .ok()?;
        if node.name().ok().as_deref() == Some(want) {
            return Some(id.clone());
        }
        for child in node.get_children().ok()? {
            let child_id = ElementId::new(child.name_as_str()?, child.path_as_str());
            if let Some(found) = find_by_name(conn, &child_id, want, depth + 1) {
                return Some(found);
            }
        }
        None
    }

    /// Every key these tests let through is a *real* key, and Tab moves GTK's
    /// focus — so a pass-through leg leaves the fixture focused on its text view,
    /// where Tab inserts a tab character instead of cycling back. Put the entry
    /// back through `Component.GrabFocus`, so this module cannot perturb the
    /// AT-SPI tests no matter what order they run in.
    fn restore_entry_focus() {
        let conn = a11y_bus();
        let root = ElementId::new("org.a11y.atspi.Registry", "/org/a11y/atspi/accessible/root");
        let entry = find_by_name(&conn, &root, FIXTURE_ENTRY, 0)
            .unwrap_or_else(|| panic!("no accessible named {FIXTURE_ENTRY}"));
        let component = atspi::proxy::component::ComponentProxyBlocking::builder(&conn)
            .destination(entry.bus_name.clone())
            .expect("destination")
            .path(entry.path.clone())
            .expect("path")
            .build()
            .expect("Component proxy");
        assert!(
            component.grab_focus().expect("GrabFocus"),
            "the fixture entry refused focus"
        );
        let session = crate::atspi_live::AtspiSession::open().expect("a11y bus");
        for _ in 0..20 {
            if let Ok(Some(id)) = session.focused_field() {
                if session.element_name(&id).as_deref() == Some(FIXTURE_ENTRY) {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("focus did not return to {FIXTURE_ENTRY}");
    }

    type Recorded = Arc<Mutex<Vec<TapControl>>>;

    fn install_tap() -> (LinuxAdapter, AcceptSubscription, Recorded) {
        let adapter = LinuxAdapter::with_accessibility();
        let recorded: Recorded = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&recorded);
        let callback: AcceptCallback = Arc::new(move |control| {
            sink.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(control);
        });
        // A47's other consumer, pinned at its real call site: the accept
        // subscription's id must fall inside a window drawn from the same
        // process-wide counter the event subscriptions use.
        let before_id = crate::next_subscription_id();
        let subscription = adapter
            .subscribe_accept(callback)
            .expect("the harness's X session has the accept keys free, so the tap must install");
        let after_id = crate::next_subscription_id();
        assert!(
            before_id < subscription.id() && subscription.id() < after_id,
            "subscribe_accept must mint its id from the shared subscription counter: \
             {before_id} < {} < {after_id}",
            subscription.id()
        );
        (adapter, subscription, recorded)
    }

    /// Wait briefly for the dispatcher thread to deliver, then report what it did.
    fn delivered(recorded: &Recorded, expected: usize) -> Vec<TapControl> {
        let deadline = Instant::now() + KEY_WAIT;
        loop {
            let controls = recorded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if controls.len() >= expected || Instant::now() >= deadline {
                return controls;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
    fn live_accept_tap_consumes_only_while_a_suggestion_is_visible() {
        let (conn, root) = xtest();
        let (_adapter, subscription, recorded) = install_tap();

        // A. Baseline. The tap is installed but unarmed, so nothing is grabbed and
        // the synthetic Tab must reach the application — this is what makes the
        // consume leg below meaningful.
        let mut expected = key_count("Tab") + 1;
        tap_key(&conn, root, KEYSYM_TAB);
        assert_eq!(
            wait_for_key_count("Tab", expected),
            expected,
            "an unarmed tap must not intercept anything"
        );

        // B. Consume. Armed, so the grab is installed and AsyncKeyboard swallows
        // the key: the application must not see it, and the engine must.
        subscription
            .set_suggestion_visible(true)
            .expect("arming must succeed with the accept keys free");
        tap_key(&conn, root, KEYSYM_TAB);
        assert_eq!(
            delivered(&recorded, 1),
            vec![TapControl::Accept(AcceptAction::Word)],
            "Tab is the word-accept key, and the armed tap must report it"
        );
        std::thread::sleep(LEAK_WAIT); // give a leaked key time to land
        assert_eq!(
            key_count("Tab"),
            expected,
            "the consumed Tab must not reach the application"
        );

        // C. Pass-through *while grabbed* — the spike's leg C, and the reason a
        // passive grab is not too invasive. A correction offer binds only the
        // grammar key, so Tab is still grabbed but is resolved with
        // ReplayKeyboard: the application receives it, with no synthetic re-send.
        subscription
            .set_accept_action(Some(AcceptAction::Correction))
            .expect("switch the armed action to a correction");
        expected += 1;
        tap_key(&conn, root, KEYSYM_TAB);
        assert_eq!(
            wait_for_key_count("Tab", expected),
            expected,
            "a grabbed-but-unbound key must be replayed to the application"
        );

        // D. Disarmed: the grab is dropped entirely.
        subscription
            .set_suggestion_visible(false)
            .expect("disarming must succeed");
        expected += 1;
        tap_key(&conn, root, KEYSYM_TAB);
        assert_eq!(
            wait_for_key_count("Tab", expected),
            expected,
            "a disarmed tap must pass Tab through"
        );

        // E. Teardown. Dropping the subscription must leave nothing behind.
        drop(subscription);
        expected += 1;
        tap_key(&conn, root, KEYSYM_TAB);
        assert_eq!(
            wait_for_key_count("Tab", expected),
            expected,
            "plain delivery must be restored after teardown"
        );
        assert_eq!(
            delivered(&recorded, 2).len(),
            1,
            "only the armed keystroke may be reported"
        );
        restore_entry_focus();
    }

    #[test]
    #[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
    fn live_accept_tap_never_leaves_the_keyboard_grabbed_after_teardown() {
        // The dangerous teardown: drop the subscription while the grab is ARMED.
        // A leaked grab is invisible until a user presses Tab in another
        // application, so it is proven two independent ways.
        let (conn, root) = xtest();
        let (_adapter, subscription, _recorded) = install_tap();
        subscription.set_suggestion_visible(true).expect("arm");
        // Teardown must also be BOUNDED. It used to join all three workers with
        // no timeout, so a worker that missed its wake — a dropped `SendEvent`,
        // an X server that stopped answering — hung the run loop forever, on the
        // very thread that drives the product. Drop now waits `STOP_TIMEOUT` and
        // then detaches, so this must return promptly even though nothing here
        // is wedged.
        let started = std::time::Instant::now();
        drop(subscription);
        let teardown = started.elapsed();
        assert!(
            teardown < std::time::Duration::from_secs(5),
            "teardown must be bounded, took {teardown:?}"
        );

        let expected = key_count("Tab") + 1;
        tap_key(&conn, root, KEYSYM_TAB);
        assert_eq!(
            wait_for_key_count("Tab", expected),
            expected,
            "teardown while armed must release the grab"
        );

        // And the X server agrees: a fresh grab of the same key would fail with
        // BadAccess if the previous one were still held by anyone.
        let (_adapter, second, _recorded) = install_tap();
        second
            .set_suggestion_visible(true)
            .expect("the accept keys must be grabbable again after teardown");
        drop(second);
        restore_entry_focus();
    }

    #[test]
    #[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
    fn live_accept_tap_replays_a_modified_chord_to_the_application() {
        // Ctrl+Tab (switch browser tab) is not in the exact-modifier grab plan,
        // so the server never routes it to the tap: it must reach the app
        // untouched even while a suggestion is showing. (Replay of a *grabbed*
        // chord is covered by the consumes-only-while-visible test's leg C.)
        let (conn, root) = xtest();
        let (_adapter, subscription, recorded) = install_tap();
        subscription.set_suggestion_visible(true).expect("arm");

        let expected = key_count("Tab") + 1;
        tap_key_with_modifier(&conn, root, KEYSYM_CONTROL_L, KEYSYM_TAB);
        assert_eq!(
            wait_for_key_count("Tab", expected),
            expected,
            "Ctrl+Tab must be replayed to the application even while armed"
        );
        assert!(
            delivered(&recorded, 1).is_empty(),
            "a modified chord must not be reported as an accept"
        );
        drop(subscription);
        restore_entry_focus();
    }

    #[test]
    #[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
    fn live_accept_tap_ignores_an_unrelated_alt_tab_owner() {
        let (owner, root) = xtest();
        let tab = keycode(&owner, KEYSYM_TAB);
        owner
            .grab_key(
                false,
                root,
                x11rb::protocol::xproto::ModMask::M1,
                tab,
                x11rb::protocol::xproto::GrabMode::ASYNC,
                x11rb::protocol::xproto::GrabMode::ASYNC,
            )
            .expect("Alt+Tab grab request")
            .check()
            .expect("the isolated Xvfb has Alt+Tab free");

        let (_adapter, subscription, recorded) = install_tap();
        subscription
            .set_suggestion_visible(true)
            .expect("bare accept chords remain installable");
        tap_key(&owner, root, KEYSYM_TAB);
        assert_eq!(
            delivered(&recorded, 1),
            vec![TapControl::Accept(AcceptAction::Word)],
            "another client's Alt+Tab must not block compme's bare Tab"
        );
        drop(subscription);
        restore_entry_focus();
    }

    #[test]
    #[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
    fn live_accept_tap_rebinds_while_armed() {
        crate::x11_keys::set_accept_chords_with_mods(None, None, None).unwrap();
        let (conn, root) = xtest();
        let (_adapter, subscription, recorded) = install_tap();
        subscription.set_suggestion_visible(true).expect("arm");

        crate::x11_keys::set_accept_chords_with_mods(Some((36, 0)), None, None)
            .expect("Return word rebind");
        subscription
            .rearm_accept_tap()
            .expect("transactional live rebind");

        tap_key(&conn, root, KEYSYM_RETURN);
        assert_eq!(
            delivered(&recorded, 1),
            vec![TapControl::Accept(AcceptAction::Word)],
            "the new Return chord must accept immediately"
        );
        let tabs = key_count("Tab") + 1;
        tap_key(&conn, root, KEYSYM_TAB);
        assert_eq!(
            wait_for_key_count("Tab", tabs),
            tabs,
            "the old Tab chord must be released after rebind"
        );
        assert_eq!(delivered(&recorded, 2).len(), 1);

        drop(subscription);
        crate::x11_keys::set_accept_chords_with_mods(None, None, None).unwrap();
        restore_entry_focus();
    }

    #[test]
    #[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
    fn live_accept_tap_failed_rebind_restores_the_armed_plan() {
        crate::x11_keys::set_accept_chords_with_mods(None, None, None).unwrap();
        let (owner, root) = xtest();
        let return_key = keycode(&owner, KEYSYM_RETURN);
        owner
            .grab_key(
                false,
                root,
                x11rb::protocol::xproto::ModMask::from(0u16),
                return_key,
                x11rb::protocol::xproto::GrabMode::ASYNC,
                x11rb::protocol::xproto::GrabMode::ASYNC,
            )
            .expect("bare Return grab request")
            .check()
            .expect("the isolated Xvfb has bare Return free");

        let (_adapter, subscription, recorded) = install_tap();
        subscription.set_suggestion_visible(true).expect("arm");
        crate::x11_keys::set_accept_chords_with_mods(Some((36, 0)), None, None)
            .expect("Return word rebind");
        assert!(
            subscription.rearm_accept_tap().is_err(),
            "another client's bare Return grab must reject the new plan"
        );

        tap_key(&owner, root, KEYSYM_TAB);
        assert_eq!(
            delivered(&recorded, 1),
            vec![TapControl::Accept(AcceptAction::Word)],
            "a failed rebind must restore the previously armed Tab plan"
        );

        drop(subscription);
        crate::x11_keys::set_accept_chords_with_mods(None, None, None).unwrap();
        restore_entry_focus();
    }

    #[test]
    #[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
    fn live_accept_tap_failed_arm_leaves_no_state_behind() {
        // A58: arm state (the armed action, the arm timestamp, the failsafe hide
        // deadline) is written on the *success* arm. The failure arm must leave
        // none of it, or a later successful arm inherits a deadline that already
        // expired and the watchdog disarms it out from under the user. Asserted
        // through behavior only: a keystroke either accepts or it does not.
        crate::x11_keys::set_accept_chords_with_mods(None, None, None).unwrap();
        let (owner, root) = xtest();
        // Installed BEFORE the conflicting grab: `with_accessibility` trial-grabs
        // the accept keys to decide `accept_intercept`, so grabbing Tab first
        // would fail the probe and there would be no tap to arm at all.
        let (_adapter, subscription, recorded) = install_tap();
        let tab = keycode(&owner, KEYSYM_TAB);
        let grab_tab = |owner: &RustConnection| {
            owner
                .grab_key(
                    false,
                    root,
                    x11rb::protocol::xproto::ModMask::from(0u16),
                    tab,
                    x11rb::protocol::xproto::GrabMode::ASYNC,
                    x11rb::protocol::xproto::GrabMode::ASYNC,
                )
                .expect("bare Tab grab request")
                .check()
                .expect("the isolated Xvfb has bare Tab free");
        };
        let ungrab_tab = |owner: &RustConnection| {
            owner
                .ungrab_key(tab, root, x11rb::protocol::xproto::ModMask::from(0u16))
                .expect("release owned Tab")
                .check()
                .expect("release owned Tab reply");
        };

        // Leg 1: a pending failsafe hide must not survive the failed arm.
        subscription
            .hide_suggestion_after(Duration::from_millis(150))
            .expect("schedule a failsafe hide before the arm fails");
        grab_tab(&owner);
        assert!(
            matches!(
                subscription.set_accept_action(Some(AcceptAction::Correction)),
                Err(PlatformError::UnsupportedField { .. })
            ),
            "another client's bare Tab grab must make the arm fail with BadAccess"
        );
        // Well past the scheduled hide: a deadline that survived has now fired.
        std::thread::sleep(Duration::from_millis(400));
        ungrab_tab(&owner);
        // Deliberately NOT set_suggestion_visible, which clears the deadline on
        // every visibility transition and would hide the leak this leg is for.
        subscription
            .set_accept_action(Some(AcceptAction::Word))
            .expect("the accept keys are free again");
        std::thread::sleep(Duration::from_millis(150)); // several watchdog ticks
        tap_key(&owner, root, KEYSYM_TAB);
        assert_eq!(
            delivered(&recorded, 1),
            vec![TapControl::Accept(AcceptAction::Word)],
            "a hide deadline from before the failed arm must not disarm the later arm"
        );

        // Leg 2: the failed arm's action must not survive either — the next
        // `set_suggestion_visible(true)` defaults to a full accept only if it
        // finds no armed action, so a leaked Correction would silently replace
        // the arm's meaning and stop Tab from accepting.
        subscription.set_suggestion_visible(false).expect("disarm");
        grab_tab(&owner);
        assert!(
            subscription
                .set_accept_action(Some(AcceptAction::Correction))
                .is_err(),
            "the second arm must fail the same way"
        );
        ungrab_tab(&owner);
        subscription
            .set_suggestion_visible(true)
            .expect("arm again, as from cold");
        tap_key(&owner, root, KEYSYM_TAB);
        assert_eq!(
            delivered(&recorded, 2),
            vec![
                TapControl::Accept(AcceptAction::Word),
                TapControl::Accept(AcceptAction::Word)
            ],
            "arming after a failed arm must behave as from cold, not inherit its action"
        );

        drop(subscription);
        restore_entry_focus();
    }

    #[test]
    #[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
    fn live_accept_tap_rebuilds_its_plan_after_keyboard_mapping_notify() {
        let (conn, root) = xtest();
        let setup = conn.setup();
        let min = setup.min_keycode;
        let count = setup.max_keycode - min + 1;
        let original = conn
            .get_keyboard_mapping(min, count)
            .expect("mapping request")
            .reply()
            .expect("mapping reply");
        let per = usize::from(original.keysyms_per_keycode);
        let old_tab = keycode_for_keysym(
            min,
            original.keysyms_per_keycode,
            &original.keysyms,
            KEYSYM_TAB,
        )
        .expect("Tab keycode");
        let replacement = (min..=setup.max_keycode)
            .find(|candidate| {
                *candidate != old_tab && original.keysyms[usize::from(*candidate - min) * per] != 0
            })
            .expect("a second mapped keycode");
        let first = old_tab.min(replacement);
        let last = old_tab.max(replacement);
        let start = usize::from(first - min) * per;
        let end = (usize::from(last - min) + 1) * per;
        let mut changed = original.keysyms[start..end].to_vec();
        let old_index = usize::from(old_tab - first) * per;
        let new_index = usize::from(replacement - first) * per;
        changed.swap(old_index, new_index);

        let (_adapter, subscription, recorded) = install_tap();
        subscription.set_suggestion_visible(true).expect("arm");
        conn.change_keyboard_mapping(
            usize::from(last - first + 1) as u8,
            first,
            original.keysyms_per_keycode,
            &changed,
        )
        .expect("change mapping")
        .check()
        .expect("change mapping reply");
        conn.flush().expect("flush mapping change");

        for press in [KEY_PRESS_EVENT, KEY_RELEASE_EVENT] {
            conn.xtest_fake_input(press, replacement, 0, root, 0, 0, 0)
                .expect("XTEST rebound Tab")
                .ignore_error();
        }
        conn.flush().expect("flush rebound Tab");
        assert_eq!(
            delivered(&recorded, 1),
            vec![TapControl::Accept(AcceptAction::Word)],
            "MappingNotify must move the live grab to Tab's new keycode"
        );

        drop(subscription);
        conn.change_keyboard_mapping(count, min, original.keysyms_per_keycode, &original.keysyms)
            .expect("restore mapping")
            .check()
            .expect("restore mapping reply");
        conn.flush().expect("flush restored mapping");
        restore_entry_focus();
    }

    #[test]
    #[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
    fn live_accept_tap_plan_build_failure_drops_the_stale_armed_plan() {
        crate::x11_keys::set_accept_chords_with_mods(None, None, None).unwrap();
        let (owner, root) = xtest();
        let setup = owner.setup();
        let min = setup.min_keycode;
        let count = setup.max_keycode - min + 1;
        let original = owner
            .get_keyboard_mapping(min, count)
            .expect("mapping request")
            .reply()
            .expect("mapping reply");
        let old_tab = keycode_for_keysym(
            min,
            original.keysyms_per_keycode,
            &original.keysyms,
            KEYSYM_TAB,
        )
        .expect("Tab keycode");
        let mut changed = original.keysyms.clone();
        for keysym in &mut changed {
            if [KEYSYM_TAB, KEYSYM_GRAVE, KEYSYM_ESCAPE, KEYSYM_DOWN].contains(keysym) {
                *keysym = 0;
            }
        }
        let mut mapping = KeyboardMappingGuard {
            conn: &owner,
            min,
            count,
            keysyms_per_keycode: original.keysyms_per_keycode,
            keysyms: original.keysyms,
            restored: false,
        };

        let (_adapter, subscription, recorded) = install_tap();
        subscription.set_suggestion_visible(true).expect("arm");
        subscription
            .hide_suggestion_after(Duration::from_millis(300))
            .expect("schedule hide before the failed rebuild");
        owner
            .change_keyboard_mapping(count, min, mapping.keysyms_per_keycode, &changed)
            .expect("remove accept keysyms")
            .check()
            .expect("remove accept keysyms reply");
        owner.flush().expect("flush changed mapping");

        // Acquiring this grab is also the synchronization point proving that
        // the tap consumed MappingNotify and released its old plan. Do not race
        // the event thread with a fixed sleep.
        let deadline = Instant::now() + KEY_WAIT;
        loop {
            let result = owner
                .grab_key(
                    false,
                    root,
                    x11rb::protocol::xproto::ModMask::from(0u16),
                    old_tab,
                    x11rb::protocol::xproto::GrabMode::ASYNC,
                    x11rb::protocol::xproto::GrabMode::ASYNC,
                )
                .expect("old Tab grab request")
                .check();
            if result.is_ok() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the failed rebuild must release the stale armed Tab grab: {result:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            subscription.rearm_accept_tap().is_err(),
            "a layout carrying none of the accept keys must reject the rebuilt plan"
        );
        subscription
            .set_accept_action(Some(AcceptAction::Word))
            .expect("arming an empty plan must not retry stale keycodes");

        std::thread::sleep(Duration::from_millis(400));
        owner
            .ungrab_key(old_tab, root, x11rb::protocol::xproto::ModMask::from(0u16))
            .expect("release owned old Tab")
            .check()
            .expect("release owned old Tab reply");
        mapping.restore();
        subscription
            .rearm_accept_tap()
            .expect("a later valid map must rebuild and arm normally");

        tap_key(&owner, root, KEYSYM_TAB);
        assert_eq!(
            delivered(&recorded, 1),
            vec![TapControl::Accept(AcceptAction::Word)],
            "the build failure must clear the old hide deadline and allow valid recovery"
        );

        drop(subscription);
        crate::x11_keys::set_accept_chords_with_mods(None, None, None).unwrap();
        restore_entry_focus();
    }

    #[test]
    #[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
    fn live_accept_tap_reports_dismiss_and_cycle_with_their_controls() {
        let (conn, root) = xtest();
        let (_adapter, subscription, recorded) = install_tap();
        subscription.set_suggestion_visible(true).expect("arm");

        let escapes = key_count("Escape");
        let downs = key_count("Down");
        tap_key(&conn, root, KEYSYM_ESCAPE);
        tap_key(&conn, root, KEYSYM_DOWN);

        let controls = delivered(&recorded, 2);
        assert!(
            controls.contains(&TapControl::Dismiss) && controls.contains(&TapControl::Cycle),
            "Esc must dismiss and Down must cycle, got {controls:?}"
        );
        std::thread::sleep(LEAK_WAIT);
        assert_eq!(
            (key_count("Escape"), key_count("Down")),
            (escapes, downs),
            "both consumed keys must be kept from the application"
        );
    }

    #[test]
    #[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
    fn live_accept_tap_watchdog_disarms_a_missed_hide() {
        // hide_suggestion_after is the engine's failsafe for a hide it never sends.
        // The watchdog owns that deadline, so this proves the grab really goes away
        // without any further engine call — the property that stops a lost hide
        // from eating Tab forever.
        let (conn, root) = xtest();
        let (_adapter, subscription, recorded) = install_tap();
        subscription.set_suggestion_visible(true).expect("arm");
        subscription
            .hide_suggestion_after(Duration::from_millis(50))
            .expect("schedule the failsafe hide");
        std::thread::sleep(Duration::from_millis(250));

        let expected = key_count("Tab") + 1;
        tap_key(&conn, root, KEYSYM_TAB);
        assert_eq!(
            wait_for_key_count("Tab", expected),
            expected,
            "the watchdog must have dropped the grab after the scheduled hide"
        );
        assert!(
            delivered(&recorded, 1).is_empty(),
            "nothing may be accepted after the failsafe hide"
        );
        drop(subscription);
        restore_entry_focus();
    }

    #[test]
    #[ignore = "needs the AT-SPI session harness: run-linux-atspi-session.sh --run-in-session"]
    fn live_capabilities_report_the_x_grab_key_tap() {
        // The capability flip this phase earns: with a real X session and the
        // accept keys free, the adapter may finally claim XGrabKey. The pure
        // AT-SPI mapping still reports None (it cannot know), so this also pins
        // that `capabilities` fills the session fact in.
        restore_entry_focus();
        let session = session();
        let id = fixture_entry(&session);
        let adapter = LinuxAdapter::with_accessibility();
        let caps = adapter
            .capabilities(&handle(&adapter, &id))
            .expect("capabilities");
        assert_eq!(caps.accept_intercept, platform::KeyInterceptMode::XGrabKey);
        // An adapter that never probed must stay fail-closed on both counts.
        let inert = LinuxAdapter::new();
        assert!(matches!(
            inert.subscribe_accept(Arc::new(|_| {})),
            Err(PlatformError::UnsupportedField { .. })
        ));
    }
}
