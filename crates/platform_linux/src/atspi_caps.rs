//! AT-SPI2 → [`platform::Capabilities`] mapping, kept pure so it is testable on
//! every host (this crate also builds on macOS and Windows, where no
//! accessibility bus exists).
//!
//! The live adapter collects these facts over D-Bus — the interface list, the
//! state set, the role name, and the application's toolkit — and this module
//! turns them into the portable contract. Splitting it this way means the
//! interesting decisions (what counts as writable, what counts as secure) are
//! unit-tested rather than only reachable through a running desktop session.

use platform::{
    Capabilities, InsertStrategy, KeyInterceptMode, OverlayPlacement, SecurityState, Toolkit,
};

/// What the adapter observed about one accessible. Plain data — no `atspi`
/// types — so this module compiles and tests everywhere.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldFacts {
    /// `org.a11y.atspi.Text` present: text and caret offset are readable.
    pub has_text: bool,
    /// `org.a11y.atspi.EditableText` present: the value can be changed.
    pub has_editable_text: bool,
    /// AT-SPI `STATE_EDITABLE`. Present-but-not-editable happens on read-only
    /// entries and rendered documents.
    pub editable: bool,
    /// AT-SPI `STATE_SENSITIVE`: a disabled widget is not writable even when it
    /// exposes EditableText.
    pub sensitive: bool,
    /// AT-SPI `STATE_MULTI_LINE`.
    pub multiline: bool,
    /// AT-SPI role name, e.g. `entry`, `text`, `password text`, `terminal`.
    pub role: String,
    /// The owning application's `ToolkitName` property, e.g. `GTK`, `Qt`.
    pub toolkit_name: String,
}

/// The AT-SPI role for a password entry. This is the *only* signal AT-SPI gives
/// for secure input — there is no separate protected state — so it is matched
/// exactly rather than by substring, and anything unrecognized stays insecure
/// only because a role we do not know is not evidence of a password field.
const ROLE_PASSWORD: &str = "password text";

/// Map observed facts onto the portable capability contract.
///
/// `accept_intercept` reports **what this adapter can actually do today**, not
/// what X11 permits: the accept tap (Phase 2.3, resolved to `XGrabKey` +
/// `XAllowEvents`) is not built yet, so claiming it here would have the engine arm
/// a mechanism that immediately fails. It flips as that phase lands. The
/// read/write bits, by contrast, describe the field itself and are true now.
///
/// `overlay_at_caret` reports `OverrideRedirect` since Phase 2.5: the presenter
/// (`x11_overlay`) really does place a click-through override-redirect window at
/// the caret. Like `coords_global_screen` below, that is an **X11** statement —
/// under Wayland this adapter's screen geometry is wrong anyway, and the overlay
/// there is `LayerShell` in Phase 3. A session with no X server is not silently
/// mis-served: `show_ghost` fails closed with the `DISPLAY` it tried, and the
/// contract requires the host to reconcile a failed show.
pub fn capabilities_from(facts: &FieldFacts) -> Capabilities {
    let secure = facts.role == ROLE_PASSWORD;
    Capabilities {
        readable_text: facts.has_text,
        // The caret offset is part of the Text interface, as are per-character
        // extents, so one interface answers both.
        readable_caret: facts.has_text,
        writable: facts.has_editable_text && facts.editable && facts.sensitive,
        // AT-SPI exposes no assistant/chat-input marker. Reporting false keeps
        // SidebarOnly applications fail-closed, exactly as the Windows scaffold
        // and the pre-classifier macOS build did.
        assistant_field: false,
        secure,
        security_state: if secure {
            SecurityState::SecureField
        } else {
            SecurityState::Normal
        },
        toolkit: toolkit_from(&facts.toolkit_name),
        multiline: facts.multiline,
        insert_strategy: insert_strategy_from(facts),
        accept_intercept: KeyInterceptMode::None,
        overlay_at_caret: OverlayPlacement::OverrideRedirect,
        // Both Text::GetCharacterExtents and Component::GetExtents are queried
        // with ATSPI_COORD_TYPE_SCREEN, so geometry is already global.
        coords_global_screen: true,
    }
}

/// `NativeRangeSet` only when the value can really be replaced atomically.
///
/// EditableText's `InsertText`/`DeleteText` pair is *not* atomic together, so the
/// atomic contract is met by `SetTextContents` (whole-value swap) guarded by an
/// expected-text snapshot — the same shape as the macOS `AxSet` path. Anything
/// else reports `None`: the synthetic-key fallback (XTEST) is not built yet, and
/// claiming a strategy the adapter cannot perform makes the engine offer
/// replacements it will fail to apply.
fn insert_strategy_from(facts: &FieldFacts) -> InsertStrategy {
    if facts.has_editable_text && facts.editable && facts.sensitive {
        InsertStrategy::NativeRangeSet
    } else {
        InsertStrategy::None
    }
}

/// AT-SPI reports a free-form toolkit string. Only the values the portable
/// `Toolkit` enum names are folded in; everything else is preserved verbatim
/// rather than guessed at, because `Toolkit` is a compatibility hint and a wrong
/// fold is worse than an honest `Unknown`.
fn toolkit_from(toolkit_name: &str) -> Toolkit {
    match toolkit_name.trim() {
        // Chromium and Electron both report "Chromium" over AT-SPI; the
        // distinction is not observable here, so report the accurate half.
        name if name.eq_ignore_ascii_case("chromium") => Toolkit::Chromium,
        name if name.eq_ignore_ascii_case("webkitgtk") || name.eq_ignore_ascii_case("webkit") => {
            Toolkit::WebKit
        }
        name => Toolkit::Unknown(name.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editable_entry() -> FieldFacts {
        FieldFacts {
            has_text: true,
            has_editable_text: true,
            editable: true,
            sensitive: true,
            multiline: false,
            role: "entry".to_string(),
            toolkit_name: "GTK".to_string(),
        }
    }

    #[test]
    fn editable_entry_is_readable_writable_and_atomically_replaceable() {
        let caps = capabilities_from(&editable_entry());
        assert!(caps.readable_text && caps.readable_caret && caps.writable);
        assert_eq!(caps.insert_strategy, InsertStrategy::NativeRangeSet);
        assert!(caps.insert_strategy.supports_atomic_range_replace());
        assert!(!caps.secure);
        assert_eq!(caps.security_state, SecurityState::Normal);
        assert_eq!(caps.toolkit, Toolkit::Unknown("GTK".to_string()));
        assert!(caps.coords_global_screen);
    }

    #[test]
    fn password_role_forces_blocked_regardless_of_everything_else() {
        // The privacy gate that must never be reachable by accident: a password
        // field is Blocked even though it is a perfectly writable text entry.
        let mut facts = editable_entry();
        facts.role = "password text".to_string();
        let caps = capabilities_from(&facts);
        assert!(caps.secure);
        assert_eq!(caps.security_state, SecurityState::SecureField);
        assert_eq!(platform::ux_mode(&caps), platform::UxMode::Blocked);
    }

    #[test]
    fn a_role_we_do_not_recognize_is_not_treated_as_secure() {
        // Fail-closed cuts both ways: an unknown role is not evidence of a
        // password field, and marking it secure would silently disable compme in
        // every field a future toolkit names differently.
        for role in [
            "entry",
            "text",
            "terminal",
            "document text",
            "paragraph",
            "",
        ] {
            let mut facts = editable_entry();
            facts.role = role.to_string();
            assert!(!capabilities_from(&facts).secure, "role {role:?}");
        }
        // ...but a near-miss must not slip through the exact match either way:
        // it is neither secure nor silently mapped to something else.
        let mut facts = editable_entry();
        facts.role = "password".to_string();
        assert!(!capabilities_from(&facts).secure);
    }

    #[test]
    fn unwritable_fields_report_no_insert_strategy() {
        // Each of these is a real AT-SPI shape: a read-only entry, a disabled
        // widget, and a rendered document with no EditableText. None may claim an
        // insert strategy, because ux_mode turns that claim into a suggestion the
        // adapter cannot apply.
        for (label, mutate) in [
            (
                "not editable",
                Box::new(|f: &mut FieldFacts| f.editable = false) as Box<dyn Fn(&mut FieldFacts)>,
            ),
            (
                "not sensitive",
                Box::new(|f: &mut FieldFacts| f.sensitive = false),
            ),
            (
                "no EditableText",
                Box::new(|f: &mut FieldFacts| f.has_editable_text = false),
            ),
        ] {
            let mut facts = editable_entry();
            mutate(&mut facts);
            let caps = capabilities_from(&facts);
            assert!(!caps.writable, "{label} must not be writable");
            assert_eq!(caps.insert_strategy, InsertStrategy::None, "{label}");
            assert_eq!(
                platform::ux_mode(&caps),
                platform::UxMode::Unsupported,
                "{label}"
            );
        }
    }

    #[test]
    fn a_field_with_no_text_interface_is_unsupported_not_readable() {
        let mut facts = editable_entry();
        facts.has_text = false;
        let caps = capabilities_from(&facts);
        assert!(!caps.readable_text && !caps.readable_caret);
        assert_eq!(platform::ux_mode(&caps), platform::UxMode::Unsupported);
    }

    #[test]
    fn multiline_state_is_carried_through() {
        let mut facts = editable_entry();
        facts.multiline = true;
        assert!(capabilities_from(&facts).multiline);
        facts.multiline = false;
        assert!(!capabilities_from(&facts).multiline);
    }

    #[test]
    fn toolkit_folds_only_the_names_the_enum_knows() {
        assert_eq!(toolkit_from("Chromium"), Toolkit::Chromium);
        assert_eq!(toolkit_from("chromium"), Toolkit::Chromium);
        assert_eq!(toolkit_from("WebKitGTK"), Toolkit::WebKit);
        // Preserved verbatim rather than guessed at.
        assert_eq!(toolkit_from("GTK"), Toolkit::Unknown("GTK".to_string()));
        // An absent ToolkitName is a real observation, not a defect: at-spi2 2.60
        // reports one for a GTK3 app while the 2.5x stack on Ubuntu reports "".
        // It must stay an honest empty Unknown rather than being invented.
        assert_eq!(toolkit_from(""), Toolkit::Unknown(String::new()));
        assert_eq!(toolkit_from("Qt"), Toolkit::Unknown("Qt".to_string()));
        assert_eq!(toolkit_from("  GTK  "), Toolkit::Unknown("GTK".to_string()));
    }

    #[test]
    fn unbuilt_mechanisms_are_reported_as_absent() {
        // The tap (Phase 2.3, design resolved) is not implemented. Reporting it
        // would have the engine arm a mechanism that fails on first use, so it
        // stays None until that lands — and this test is what makes flipping it a
        // deliberate act.
        let caps = capabilities_from(&editable_entry());
        assert_eq!(caps.accept_intercept, KeyInterceptMode::None);
    }

    #[test]
    fn the_override_redirect_overlay_is_reported_now_that_it_exists() {
        // DELIBERATE FLIP (Phase 2.5): this assertion was
        // `overlay_at_caret == OverlayPlacement::None` inside the test above,
        // because no overlay existed. `x11_overlay::X11Overlay` now places a
        // click-through override-redirect window at the caret, proven live in the
        // Xvfb session suite (`live_ghost_overlay_*`), so reporting `None` would
        // now understate the adapter and hold every Linux field at `Popup`.
        //
        // The X11 scope is the same statement `coords_global_screen` already
        // makes; Wayland is Phase 3 (`LayerShell`), and a session with no X
        // server gets a fail-closed `show_ghost` naming the `DISPLAY` it tried
        // rather than a silent no-op.
        let caps = capabilities_from(&editable_entry());
        assert_eq!(caps.overlay_at_caret, OverlayPlacement::OverrideRedirect);
        // This is the whole point of the flip: a readable caret plus a real
        // caret-anchored placement is what `ux_mode` turns into inline ghost text.
        assert!(caps.readable_caret);
        assert_eq!(platform::ux_mode(&caps), platform::UxMode::Inline);
    }
}
