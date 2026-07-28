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

fn handle(id: &ElementId) -> FieldHandle {
    FieldHandle {
        app: "compme-fixture".to_string(),
        pid: None,
        element_id: id.encode(),
        generation: 0,
    }
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
        .insert(&handle(&id), "XY", InsertStrategy::NativeRangeSet)
        .expect("insert");
    assert_eq!(inserted.chars, 2);
    assert_eq!(inserted.bytes, 2);
    assert_eq!(inserted.strategy, InsertStrategy::NativeRangeSet);

    let context = adapter.read_context(&handle(&id)).expect("read back");
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
            &handle(&id),
            "teh",
            "the",
            platform::CorrectionRange { start: 0, end: 3 },
            InsertStrategy::NativeRangeSet,
        )
        .expect("range replace");
    assert_eq!(inserted.chars, 3);

    let context = adapter.read_context(&handle(&id)).expect("read back");
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
            &handle(&id),
            expected,
            "REPLACED",
            replace_range,
            InsertStrategy::NativeRangeSet,
        );
        assert!(
            matches!(result, Err(PlatformError::UnsupportedField { .. })),
            "{label} must fail closed, got {result:?}"
        );
        let context = adapter.read_context(&handle(&id)).expect("read back");
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
                .insert_replacing_range(&handle(&id), "teh", "the", range, strategy)
                .is_err(),
            "{strategy:?} must not range-replace"
        );
    }
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
    let before = adapter.read_context(&handle(&id)).expect("read");

    let result = adapter.insert_replacing(&handle(&id), "the", 3, InsertStrategy::NativeRangeSet);
    assert!(matches!(
        result,
        Err(PlatformError::UnsupportedField { .. })
    ));
    let after = adapter.read_context(&handle(&id)).expect("read");
    assert_eq!(
        format!("{}{}", before.left, before.right),
        format!("{}{}", after.left, after.right)
    );
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
    let subscription = adapter
        .subscribe_focus(Arc::clone(&cb))
        .expect("subscribe_focus");

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
    assert_eq!(field.element_id, entry.encode());
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
