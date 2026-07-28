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
    use x11rb::protocol::xproto::{ConnectionExt as _, Window, KEY_PRESS_EVENT, KEY_RELEASE_EVENT};
    use x11rb::protocol::xtest::ConnectionExt as _;
    use x11rb::rust_connection::RustConnection;

    const KEYSYM_TAB: u32 = 0xff09;
    const KEYSYM_ESCAPE: u32 = 0xff1b;
    const KEYSYM_DOWN: u32 = 0xff54;
    const KEYSYM_CONTROL_L: u32 = 0xffe3;
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
        let subscription = adapter
            .subscribe_accept(callback)
            .expect("the harness's X session has the accept keys free, so the tap must install");
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
        drop(subscription);

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
        // Ctrl+Tab (switch browser tab) reaches us because the grab is
        // AnyModifier, and must be replayed untouched: the decision matches
        // modifiers exactly. Eating this would break every app's Ctrl+Tab whenever
        // a suggestion happened to be showing.
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
        let caps = LinuxAdapter::with_accessibility()
            .capabilities(&handle(&id))
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
