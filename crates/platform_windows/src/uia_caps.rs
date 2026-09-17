//! UIA facts → [`platform::Capabilities`] mapping — pure, compiled and tested
//! on every host (this crate also builds on macOS and Linux, which have no
//! UIA). The live adapter collects these facts over COM — pattern
//! availability, the password property, the framework id — and this module
//! turns them into the portable contract, so the interesting decisions are
//! unit-tested rather than only reachable through a desktop session.
//!
//! The same fail-closed rule as `platform_linux::atspi_caps`: a capability is
//! claimed only when **this adapter's mechanism for it exists**. Claiming a
//! capability the engine cannot deliver arms suggestions on fields where the
//! accept/insert path will refuse.

use platform::{
    Capabilities, InsertStrategy, KeyInterceptMode, OverlayPlacement, SecurityState, Toolkit,
};

/// What the live adapter observed about one focused element. Plain data — no
/// `windows` COM types — so this module compiles and tests everywhere.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UiaFieldFacts {
    /// `IUIAutomationTextPattern` available: text and the selection are
    /// readable.
    pub has_text_pattern: bool,
    /// `IUIAutomationValuePattern` available. Recorded so the insert slice can
    /// gate its `SetValue` write on the same probe that minted the capability
    /// — but see the pairing note under [`capabilities_from_uia`]: nothing is
    /// claimed on it yet.
    pub has_value_pattern: bool,
    /// UIA `IsPassword` — the only secure-field signal this slice uses.
    pub is_password: bool,
    /// UIA `FrameworkId`, e.g. `WPF`, `Win32`, `Chrome`, `Electron`.
    pub framework: String,
}

/// Map observed facts onto the portable capability contract.
///
/// - **Reads** are claimed only from `TextPattern` availability. UIA does not
///   gate text reads on editability the way AT-SPI's `EditableText` does, so
///   there is no separate "editable" half: a read-only document exposes
///   `TextPattern` and its text genuinely is readable.
/// - **Writes are deliberately not claimed yet.** The Windows insert path
///   (`ValuePattern::SetValue`, spec §1.4) is not built, so `writable` stays
///   `false` and `insert_strategy` stays [`InsertStrategy::None`] — flipped
///   **together** in the insert slice, because a `writable` field with no
///   strategy (or a strategy whose insert refuses) would let the engine arm
///   suggestions this adapter cannot deliver. `has_value_pattern` is recorded
///   on the facts so that slice gates its write on the same probe.
/// - `accept_intercept` stays [`KeyInterceptMode::None`] until the
///   `WH_KEYBOARD_LL` tap exists (spec §1.3), and `overlay_at_caret` stays
///   [`OverlayPlacement::None`] until the layered window exists (spec §1.5) —
///   the mechanism rule above, applied to the other two pillars.
/// - A password field maps to the same blocked set the macOS adapter reports
///   for secure fields: nothing readable, nothing writable, `SecureField`.
pub fn capabilities_from_uia(facts: &UiaFieldFacts) -> Capabilities {
    let secure = facts.is_password;
    let readable = facts.has_text_pattern && !secure;
    Capabilities {
        readable_text: readable,
        readable_caret: readable,
        writable: false,
        assistant_field: false,
        secure,
        security_state: if secure {
            SecurityState::SecureField
        } else {
            SecurityState::Normal
        },
        toolkit: Toolkit::Unknown(facts.framework.clone()),
        multiline: false,
        insert_strategy: InsertStrategy::None,
        accept_intercept: KeyInterceptMode::None,
        overlay_at_caret: OverlayPlacement::None,
        // UIA screen coordinates are physical, global to the virtual desktop —
        // the same statement macOS makes for Cocoa screen space.
        coords_global_screen: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(has_text_pattern: bool, has_value_pattern: bool, is_password: bool) -> UiaFieldFacts {
        UiaFieldFacts {
            has_text_pattern,
            has_value_pattern,
            is_password,
            framework: "Win32".into(),
        }
    }

    #[test]
    fn text_pattern_field_is_readable_not_writable() {
        let caps = capabilities_from_uia(&facts(true, false, false));
        assert!(caps.readable_text);
        assert!(caps.readable_caret);
        // The insert path does not exist yet — never claimed before its
        // mechanism (see the module docs).
        assert!(!caps.writable);
        assert_eq!(caps.insert_strategy, InsertStrategy::None);
    }

    #[test]
    fn value_pattern_alone_is_not_readable() {
        let caps = capabilities_from_uia(&facts(false, true, false));
        assert!(!caps.readable_text);
        assert!(!caps.readable_caret);
        assert!(!caps.writable);
        assert_eq!(caps.insert_strategy, InsertStrategy::None);
    }

    #[test]
    fn no_patterns_is_inert_but_well_formed() {
        let caps = capabilities_from_uia(&facts(false, false, false));
        assert!(!caps.readable_text);
        assert!(!caps.readable_caret);
        assert!(!caps.writable);
        assert!(!caps.secure);
        assert_eq!(caps.security_state, SecurityState::Normal);
        assert!(caps.coords_global_screen);
    }

    #[test]
    fn password_field_maps_to_the_blocked_set() {
        let caps = capabilities_from_uia(&facts(true, true, true));
        assert!(!caps.readable_text);
        assert!(!caps.readable_caret);
        assert!(!caps.writable);
        assert!(!caps.assistant_field);
        assert!(caps.secure);
        assert_eq!(caps.security_state, SecurityState::SecureField);
        assert_eq!(caps.insert_strategy, InsertStrategy::None);
        assert_eq!(caps.accept_intercept, KeyInterceptMode::None);
        assert_eq!(caps.overlay_at_caret, OverlayPlacement::None);
        assert!(caps.coords_global_screen);
    }

    #[test]
    fn framework_id_surfaces_as_the_toolkit() {
        let mut f = facts(true, false, false);
        f.framework = "Chrome".into();
        assert_eq!(
            capabilities_from_uia(&f).toolkit,
            Toolkit::Unknown("Chrome".into())
        );
    }

    #[test]
    fn no_mechanism_no_claim_invariant_holds_across_every_combo() {
        for text in [false, true] {
            for value in [false, true] {
                for password in [false, true] {
                    let caps = capabilities_from_uia(&facts(text, value, password));
                    // Until the insert slice lands, no combo may claim a write
                    // or an interception/placement mechanism.
                    assert!(!caps.writable);
                    assert_eq!(caps.insert_strategy, InsertStrategy::None);
                    assert_eq!(caps.accept_intercept, KeyInterceptMode::None);
                    assert_eq!(caps.overlay_at_caret, OverlayPlacement::None);
                    assert!(!caps.assistant_field);
                    // Reads are never claimed from a password field.
                    assert!(password || (caps.readable_text == text));
                    assert!(password || (caps.readable_caret == text));
                }
            }
        }
    }
}
