//! Pure accept-key translation and decision logic for the X11 accept tap
//! (ROADMAP Phase 2.3), kept free of `x11rb` types so it compiles and is tested
//! on every host — this crate also builds on the macOS and Windows lanes, where
//! no X server exists.
//!
//! Three decisions live here, and they are the ones with interesting bugs:
//!
//! **Chord translation (plan gap G5).** compme persists key chords in the macOS
//! form: a macOS *virtual keycode* plus a Carbon-style modifier bit mask
//! (`shell_flags::parse_key_with_mods`). X11 addresses keys by keysym and
//! modifiers by a different bit layout, so every chord has to be translated. An
//! untranslatable keycode is *dropped* rather than guessed at: a key we do not
//! grab simply reaches the application, which is the fail-open direction for a
//! keystroke.
//!
//! **Which keystroke means what.** Mirrors the macOS `accept_tap_decision`: no
//! armed action means every key passes through, an armed ghost binds
//! word/full/dismiss/cycle, and an armed correction consumes *only* the
//! grammar-accept key. Modifiers are matched exactly, so `Ctrl+Tab` (browser tab
//! switch) and the `Option+Tab` bypass fall through to the application even while
//! a bare-`Tab` binding is armed.
//!
//! **When to unfreeze the keyboard.** A `GrabModeSync` grab freezes keyboard
//! processing system-wide until `XAllowEvents`, so the watchdog's budget
//! arithmetic is pure and pinned by tests instead of being observable only on a
//! frozen desktop.

use platform::{AcceptAction, TapControl};

/// X11 keysyms (`X11/keysymdef.h`) for the keys compme's accept chords can name.
/// Written as literals rather than pulled from a keysym crate: this is a fixed,
/// tiny table, and a dependency that only exists on Linux would leave the macOS
/// and Windows lanes unable to test the translation.
const KEYSYM_SPACE: u32 = 0x0020;
const KEYSYM_GRAVE: u32 = 0x0060;
const KEYSYM_RETURN: u32 = 0xff0d;
const KEYSYM_TAB: u32 = 0xff09;
const KEYSYM_ESCAPE: u32 = 0xff1b;
const KEYSYM_LEFT: u32 = 0xff51;
const KEYSYM_UP: u32 = 0xff52;
const KEYSYM_RIGHT: u32 = 0xff53;
const KEYSYM_DOWN: u32 = 0xff54;
/// `XK_F1`..`XK_F12` are contiguous from 0xffbe.
const KEYSYM_F1: u32 = 0xffbe;

/// macOS virtual keycodes (`Carbon/Events.h` `kVK_*`) → X11 keysyms. The four
/// compme binds by default (Tab, grave, Escape, Down) plus the keys a rebind can
/// plausibly name; anything else translates to `None`.
const MAC_KEYCODE_TO_KEYSYM: [(i64, u32); 21] = [
    (36, KEYSYM_RETURN),
    (48, KEYSYM_TAB),
    (49, KEYSYM_SPACE),
    (50, KEYSYM_GRAVE),
    (53, KEYSYM_ESCAPE),
    (123, KEYSYM_LEFT),
    (124, KEYSYM_RIGHT),
    (125, KEYSYM_DOWN),
    (126, KEYSYM_UP),
    // F1-F12 are not contiguous in the macOS keycode space.
    (122, KEYSYM_F1),
    (120, KEYSYM_F1 + 1),
    (99, KEYSYM_F1 + 2),
    (118, KEYSYM_F1 + 3),
    (96, KEYSYM_F1 + 4),
    (97, KEYSYM_F1 + 5),
    (98, KEYSYM_F1 + 6),
    (100, KEYSYM_F1 + 7),
    (101, KEYSYM_F1 + 8),
    (109, KEYSYM_F1 + 9),
    (103, KEYSYM_F1 + 10),
    (111, KEYSYM_F1 + 11),
];

/// The persisted mask bits, as `shell_flags::parse_key_with_mods` produces them
/// (Carbon `RegisterEventHotKey` layout).
const MAC_CMD: u32 = 1 << 8;
const MAC_SHIFT: u32 = 1 << 9;
const MAC_OPTION: u32 = 1 << 11;
const MAC_CONTROL: u32 = 1 << 12;

/// X11 `KeyButMask` modifier bits (`X.h`).
pub const X11_SHIFT: u16 = 1 << 0;
pub const X11_CONTROL: u16 = 1 << 2;
/// `Mod1` is Alt on every layout compme targets — the macOS Option analogue.
pub const X11_MOD1: u16 = 1 << 3;
/// `Mod4` is Super/Windows — the macOS Command analogue.
pub const X11_MOD4: u16 = 1 << 6;

/// The only modifier bits a chord may care about. `Lock` (CapsLock), `Mod2`
/// (usually NumLock), `Mod3` and `Mod5` are deliberately excluded: they are
/// latched state, not intent, and treating them as significant would make Tab
/// stop being intercepted the moment a user leaves NumLock on.
pub const SIGNIFICANT_MODIFIERS: u16 = X11_SHIFT | X11_CONTROL | X11_MOD1 | X11_MOD4;

/// Sentinel for "no deadline / not frozen" in the tap's atomics, so the watchdog
/// arithmetic below stays a pure function over plain integers.
pub const UNSET_MS: u64 = u64::MAX;
/// How long a frozen keyboard may stay frozen before the watchdog forces it
/// open. The resolve normally happens in the same loop iteration as the event, in
/// microseconds; anything approaching this budget means something is wrong and
/// the user's keyboard matters more than the accept.
pub const FREEZE_BUDGET_MS: u64 = 100;
/// Hard cap on how long the grab may stay armed without a visibility
/// transition. The engine is supposed to disarm on every hide; this is the
/// failsafe for a missed one, so a lost hide cannot intercept Tab forever.
pub const MAX_ARMED_MS: u64 = 30_000;

/// What one accept chord does when it fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptRole {
    Word,
    Full,
    Dismiss,
    Cycle,
    GrammarAccept,
}

/// One translated chord: an X11 keysym plus the exact significant modifiers it
/// requires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcceptBinding {
    pub keysym: u32,
    pub modifiers: u16,
    pub role: AcceptRole,
}

/// The default chords, in the persisted macOS form the config layer reads, so
/// the translation is exercised by the default path rather than only by a future
/// rebind. Matches `platform_macos`'s `ACCEPT_KEYMAP` default (Cotypist parity):
/// Tab accepts the next word, the grave key above it accepts the whole
/// completion, Escape dismisses, Down cycles candidates.
const DEFAULT_MAC_CHORDS: [(i64, u32, AcceptRole); 4] = [
    (48, 0, AcceptRole::Word),
    (50, 0, AcceptRole::Full),
    (53, 0, AcceptRole::Dismiss),
    (125, 0, AcceptRole::Cycle),
];

/// The chords the tap grabs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AcceptBindings {
    bindings: Vec<AcceptBinding>,
}

impl AcceptBindings {
    /// Translate persisted `(macOS keycode, macOS modifier mask, role)` chords
    /// into X11 form. A keycode with no keysym is skipped: not grabbing a key
    /// means the application receives it, which is the safe direction.
    pub fn from_mac_chords(chords: &[(i64, u32, AcceptRole)]) -> Self {
        Self {
            bindings: chords
                .iter()
                .filter_map(|&(keycode, mask, role)| {
                    Some(AcceptBinding {
                        keysym: keysym_for_mac_keycode(keycode)?,
                        modifiers: x11_modifiers_for_mac_mask(mask),
                        role,
                    })
                })
                .collect(),
        }
    }

    /// The compme defaults.
    pub fn defaults() -> Self {
        Self::from_mac_chords(&DEFAULT_MAC_CHORDS)
    }

    /// The keysyms to grab, each once. Two roles may share a keysym (`Tab` and
    /// `shift+Tab`), and grabbing the same key twice is an `Access` error, so the
    /// grab list is deduplicated while the decision list is not.
    pub fn distinct_keysyms(&self) -> Vec<u32> {
        let mut keysyms: Vec<u32> = Vec::with_capacity(self.bindings.len());
        for binding in &self.bindings {
            if !keysyms.contains(&binding.keysym) {
                keysyms.push(binding.keysym);
            }
        }
        keysyms
    }

    /// The role bound to this keysym with exactly these modifiers held.
    ///
    /// Exact matching is what keeps the tap out of the way: with a bare `Tab`
    /// binding armed, `Ctrl+Tab` and the `Option+Tab` per-app bypass match
    /// nothing and are replayed to the application untouched.
    pub fn role_for(&self, keysym: u32, state: u16) -> Option<AcceptRole> {
        let held = state & SIGNIFICANT_MODIFIERS;
        self.bindings
            .iter()
            .find(|binding| binding.keysym == keysym && binding.modifiers == held)
            .map(|binding| binding.role)
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

/// macOS virtual keycode → X11 keysym, or `None` when this adapter has no
/// translation for it.
pub fn keysym_for_mac_keycode(keycode: i64) -> Option<u32> {
    MAC_KEYCODE_TO_KEYSYM
        .iter()
        .find(|(mac, _)| *mac == keycode)
        .map(|(_, keysym)| *keysym)
}

/// Persisted macOS modifier mask → X11 modifier mask. Unknown bits are ignored
/// rather than refused: the mask is advisory metadata, and a chord that lost an
/// unrecognized modifier still matches exactly (the extra bit would be
/// unmatchable on X11 anyway).
pub fn x11_modifiers_for_mac_mask(mask: u32) -> u16 {
    let mut out = 0;
    if mask & MAC_SHIFT != 0 {
        out |= X11_SHIFT;
    }
    if mask & MAC_CONTROL != 0 {
        out |= X11_CONTROL;
    }
    if mask & MAC_OPTION != 0 {
        out |= X11_MOD1;
    }
    if mask & MAC_CMD != 0 {
        out |= X11_MOD4;
    }
    out
}

/// The X11 keycode carrying `keysym`, from a `GetKeyboardMapping` reply.
///
/// The reply is a flat table of `keysyms_per_keycode` entries per keycode
/// starting at `min_keycode`. A keycode whose *level 0* (unshifted) keysym
/// matches wins, because that is the physical key a user means by "Tab"; a match
/// at a shifted level is only a fallback, since e.g. `ISO_Left_Tab` lives at
/// level 1 of the Tab key on most layouts.
pub fn keycode_for_keysym(
    min_keycode: u8,
    keysyms_per_keycode: u8,
    keysyms: &[u32],
    keysym: u32,
) -> Option<u8> {
    if keysyms_per_keycode == 0 {
        return None;
    }
    let per = usize::from(keysyms_per_keycode);
    let mut fallback = None;
    for (index, candidate) in keysyms.iter().enumerate() {
        if *candidate != keysym {
            continue;
        }
        let keycode = u8::try_from(usize::from(min_keycode) + index / per).ok()?;
        if index % per == 0 {
            return Some(keycode);
        }
        fallback = fallback.or(Some(keycode));
    }
    fallback
}

/// What to do with one grabbed keystroke. `Consume` is the accept path
/// (`XAllowEvents(AsyncKeyboard)`, the key dies with us); `PassThrough` is
/// `XAllowEvents(ReplayKeyboard)`, which delivers it to the focused application
/// as if no grab existed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyDecision {
    Consume(TapControl),
    PassThrough,
}

/// Resolve one grabbed keystroke. Mirrors `platform_macos::accept_tap_decision`:
/// with no armed action nothing is consumed, an armed ghost binds
/// word/full/dismiss/cycle, and an armed correction consumes only the
/// grammar-accept key so ordinary Tab/Esc keep working while a correction shows.
pub fn key_decision(
    bindings: &AcceptBindings,
    keysym: u32,
    state: u16,
    action: Option<AcceptAction>,
) -> KeyDecision {
    let Some(role) = bindings.role_for(keysym, state) else {
        return KeyDecision::PassThrough;
    };
    match (action, role) {
        (Some(AcceptAction::Correction), AcceptRole::GrammarAccept) => {
            KeyDecision::Consume(TapControl::Accept(AcceptAction::Correction))
        }
        (Some(AcceptAction::Correction), _) => KeyDecision::PassThrough,
        (Some(AcceptAction::Full | AcceptAction::Word), role) => match role {
            AcceptRole::Word => KeyDecision::Consume(TapControl::Accept(AcceptAction::Word)),
            AcceptRole::Full => KeyDecision::Consume(TapControl::Accept(AcceptAction::Full)),
            AcceptRole::Dismiss => KeyDecision::Consume(TapControl::Dismiss),
            AcceptRole::Cycle => KeyDecision::Consume(TapControl::Cycle),
            // The grammar key only acts while a correction is armed.
            AcceptRole::GrammarAccept => KeyDecision::PassThrough,
        },
        (None, _) => KeyDecision::PassThrough,
    }
}

/// Whether the grab must be installed, dropped, or left alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrabTransition {
    Grab,
    Ungrab,
    Unchanged,
}

/// The tap's whole arm/disarm state machine: **the grab exists exactly while an
/// accept action is armed**. That equivalence is the contract's key-eating
/// guard, so it is one pure function rather than a condition repeated at each
/// call site.
pub fn arm_transition(action: Option<AcceptAction>, grabbed: bool) -> GrabTransition {
    match (action.is_some(), grabbed) {
        (true, false) => GrabTransition::Grab,
        (false, true) => GrabTransition::Ungrab,
        _ => GrabTransition::Unchanged,
    }
}

/// What the watchdog must do this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchdogAction {
    Nothing,
    /// A deadline passed: drop the grab so nothing else can be swallowed.
    Disarm,
    /// The keyboard has been frozen too long: replay the frozen event to the
    /// application (fail open — the user's keystroke is worth more than the
    /// accept) and drop the grab.
    ThawAndDisarm,
}

/// Deadline arithmetic for the watchdog thread. All times are milliseconds on
/// one monotonic clock, `UNSET_MS` meaning "not set", so the caller can keep the
/// state in atomics and the watchdog never waits on a lock the worker holds.
pub fn watchdog_action(
    now_ms: u64,
    frozen_since_ms: u64,
    armed_since_ms: u64,
    hide_deadline_ms: u64,
) -> WatchdogAction {
    if frozen_since_ms != UNSET_MS && now_ms.saturating_sub(frozen_since_ms) >= FREEZE_BUDGET_MS {
        return WatchdogAction::ThawAndDisarm;
    }
    if armed_since_ms != UNSET_MS && now_ms.saturating_sub(armed_since_ms) >= MAX_ARMED_MS {
        return WatchdogAction::Disarm;
    }
    if hide_deadline_ms != UNSET_MS && now_ms >= hide_deadline_ms {
        return WatchdogAction::Disarm;
    }
    WatchdogAction::Nothing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_chords_translate_to_the_four_x11_keysyms() {
        let bindings = AcceptBindings::defaults();
        // Bare keys: the default chords carry no modifiers, so a plain press
        // matches and any significant modifier does not.
        assert_eq!(bindings.role_for(KEYSYM_TAB, 0), Some(AcceptRole::Word));
        assert_eq!(bindings.role_for(KEYSYM_GRAVE, 0), Some(AcceptRole::Full));
        assert_eq!(
            bindings.role_for(KEYSYM_ESCAPE, 0),
            Some(AcceptRole::Dismiss)
        );
        assert_eq!(bindings.role_for(KEYSYM_DOWN, 0), Some(AcceptRole::Cycle));
        assert_eq!(bindings.distinct_keysyms().len(), 4);
        assert!(!bindings.is_empty());
    }

    #[test]
    fn modifiers_are_matched_exactly_so_ctrl_tab_and_option_tab_pass_through() {
        // The bug this prevents: an AnyModifier grab hands us Ctrl+Tab (browser
        // tab switch) and Option+Tab (compme's per-app Tab bypass) too. Both must
        // fall through to the application even while a bare Tab is armed.
        let bindings = AcceptBindings::defaults();
        assert_eq!(bindings.role_for(KEYSYM_TAB, X11_CONTROL), None);
        assert_eq!(bindings.role_for(KEYSYM_TAB, X11_MOD1), None);
        assert_eq!(bindings.role_for(KEYSYM_TAB, X11_SHIFT), None);
        assert_eq!(bindings.role_for(KEYSYM_TAB, X11_MOD4), None);
        assert_eq!(
            key_decision(&bindings, KEYSYM_TAB, X11_MOD1, Some(AcceptAction::Word)),
            KeyDecision::PassThrough
        );
    }

    #[test]
    fn latched_lock_modifiers_do_not_break_a_bare_binding() {
        // CapsLock (Lock), NumLock (Mod2) and Mod3/Mod5 are latched state, not
        // intent: leaving NumLock on must not stop Tab from being intercepted.
        let bindings = AcceptBindings::defaults();
        for latched in [1 << 1, 1 << 4, 1 << 5, 1 << 7] {
            assert_eq!(
                bindings.role_for(KEYSYM_TAB, latched),
                Some(AcceptRole::Word),
                "latched modifier {latched:#b} must be ignored"
            );
        }
    }

    #[test]
    fn a_rebound_chord_translates_keycode_and_modifier_mask_together() {
        // The G5 translation: the persisted form is a macOS keycode plus a Carbon
        // mask, so both halves must be converted. "shift+48" (shift+Tab) and
        // "cmd+96" (cmd+F5) are the shapes the config grammar produces.
        let bindings = AcceptBindings::from_mac_chords(&[
            (48, 1 << 9, AcceptRole::Word),
            (96, 1 << 8, AcceptRole::Full),
            (50, (1 << 11) | (1 << 12), AcceptRole::GrammarAccept),
        ]);
        assert_eq!(
            bindings.role_for(KEYSYM_TAB, X11_SHIFT),
            Some(AcceptRole::Word)
        );
        assert_eq!(bindings.role_for(KEYSYM_TAB, 0), None);
        assert_eq!(
            bindings.role_for(KEYSYM_F1 + 4, X11_MOD4),
            Some(AcceptRole::Full)
        );
        assert_eq!(
            bindings.role_for(KEYSYM_GRAVE, X11_MOD1 | X11_CONTROL),
            Some(AcceptRole::GrammarAccept)
        );
    }

    #[test]
    fn every_mapped_mac_keycode_is_distinct_and_unmapped_ones_are_dropped() {
        // A duplicated keysym in the table would make two physical keys collide
        // silently; an unknown keycode must drop the chord rather than be guessed
        // at, because grabbing the wrong key would eat a key compme never bound.
        let mut keysyms: Vec<u32> = MAC_KEYCODE_TO_KEYSYM.iter().map(|(_, k)| *k).collect();
        keysyms.sort_unstable();
        let before = keysyms.len();
        keysyms.dedup();
        assert_eq!(before, keysyms.len(), "duplicate keysym in the table");

        assert_eq!(keysym_for_mac_keycode(48), Some(KEYSYM_TAB));
        assert_eq!(keysym_for_mac_keycode(111), Some(KEYSYM_F1 + 11));
        // 0 is kVK_ANSI_A: a real keycode with no entry here.
        assert_eq!(keysym_for_mac_keycode(0), None);
        assert_eq!(keysym_for_mac_keycode(-1), None);
        assert_eq!(keysym_for_mac_keycode(9999), None);
        let dropped = AcceptBindings::from_mac_chords(&[(0, 0, AcceptRole::Word)]);
        assert!(dropped.is_empty());
        assert!(dropped.distinct_keysyms().is_empty());
        assert_eq!(dropped.role_for(KEYSYM_TAB, 0), None);
    }

    #[test]
    fn mac_modifier_masks_map_onto_the_x11_bit_layout() {
        assert_eq!(x11_modifiers_for_mac_mask(0), 0);
        assert_eq!(x11_modifiers_for_mac_mask(1 << 9), X11_SHIFT);
        assert_eq!(x11_modifiers_for_mac_mask(1 << 12), X11_CONTROL);
        assert_eq!(x11_modifiers_for_mac_mask(1 << 11), X11_MOD1);
        assert_eq!(x11_modifiers_for_mac_mask(1 << 8), X11_MOD4);
        assert_eq!(
            x11_modifiers_for_mac_mask((1 << 8) | (1 << 9) | (1 << 11) | (1 << 12)),
            SIGNIFICANT_MODIFIERS
        );
        // An unrecognized bit is ignored, not folded onto an arbitrary X11 bit.
        assert_eq!(x11_modifiers_for_mac_mask(1 << 3), 0);
    }

    #[test]
    fn distinct_keysyms_deduplicates_two_roles_sharing_one_key() {
        // Grabbing the same keycode twice is an Access error, so the grab list
        // must collapse Tab/shift+Tab into one entry while both stay decidable.
        let bindings = AcceptBindings::from_mac_chords(&[
            (48, 0, AcceptRole::Word),
            (48, 1 << 9, AcceptRole::Full),
        ]);
        assert_eq!(bindings.distinct_keysyms(), vec![KEYSYM_TAB]);
        assert_eq!(bindings.role_for(KEYSYM_TAB, 0), Some(AcceptRole::Word));
        assert_eq!(
            bindings.role_for(KEYSYM_TAB, X11_SHIFT),
            Some(AcceptRole::Full)
        );
    }

    #[test]
    fn nothing_is_consumed_while_no_action_is_armed() {
        // The contract's headline rule: keys may be swallowed only while the
        // engine has reported a visible suggestion. Unarmed, every bound key must
        // be replayed to the application.
        let bindings = AcceptBindings::defaults();
        for keysym in bindings.distinct_keysyms() {
            assert_eq!(
                key_decision(&bindings, keysym, 0, None),
                KeyDecision::PassThrough,
                "keysym {keysym:#x} must pass through while unarmed"
            );
        }
    }

    #[test]
    fn an_armed_ghost_binds_word_full_dismiss_and_cycle() {
        let bindings = AcceptBindings::defaults();
        for armed in [AcceptAction::Word, AcceptAction::Full] {
            assert_eq!(
                key_decision(&bindings, KEYSYM_TAB, 0, Some(armed)),
                KeyDecision::Consume(TapControl::Accept(AcceptAction::Word))
            );
            assert_eq!(
                key_decision(&bindings, KEYSYM_GRAVE, 0, Some(armed)),
                KeyDecision::Consume(TapControl::Accept(AcceptAction::Full))
            );
            assert_eq!(
                key_decision(&bindings, KEYSYM_ESCAPE, 0, Some(armed)),
                KeyDecision::Consume(TapControl::Dismiss)
            );
            assert_eq!(
                key_decision(&bindings, KEYSYM_DOWN, 0, Some(armed)),
                KeyDecision::Consume(TapControl::Cycle)
            );
        }
        // A key nobody bound is never consumed, armed or not.
        assert_eq!(
            key_decision(&bindings, KEYSYM_SPACE, 0, Some(AcceptAction::Word)),
            KeyDecision::PassThrough
        );
    }

    #[test]
    fn an_armed_correction_consumes_only_the_grammar_key() {
        // macOS parity: while a correction offer shows, Tab/Esc/Down must keep
        // their normal meaning — swallowing them would eat editing keystrokes for
        // a suggestion the user is not answering.
        let bindings = AcceptBindings::from_mac_chords(&[
            (48, 0, AcceptRole::Word),
            (50, 0, AcceptRole::Full),
            (53, 0, AcceptRole::Dismiss),
            (125, 0, AcceptRole::Cycle),
            (36, 0, AcceptRole::GrammarAccept),
        ]);
        assert_eq!(
            key_decision(&bindings, KEYSYM_RETURN, 0, Some(AcceptAction::Correction)),
            KeyDecision::Consume(TapControl::Accept(AcceptAction::Correction))
        );
        for keysym in [KEYSYM_TAB, KEYSYM_GRAVE, KEYSYM_ESCAPE, KEYSYM_DOWN] {
            assert_eq!(
                key_decision(&bindings, keysym, 0, Some(AcceptAction::Correction)),
                KeyDecision::PassThrough,
                "keysym {keysym:#x} must pass through under a correction"
            );
        }
        // And the grammar key does nothing while a ghost (not a correction) shows.
        assert_eq!(
            key_decision(&bindings, KEYSYM_RETURN, 0, Some(AcceptAction::Word)),
            KeyDecision::PassThrough
        );
    }

    #[test]
    fn the_grab_exists_exactly_while_an_action_is_armed() {
        assert_eq!(
            arm_transition(Some(AcceptAction::Word), false),
            GrabTransition::Grab
        );
        assert_eq!(
            arm_transition(Some(AcceptAction::Word), true),
            GrabTransition::Unchanged
        );
        assert_eq!(arm_transition(None, true), GrabTransition::Ungrab);
        assert_eq!(arm_transition(None, false), GrabTransition::Unchanged);
    }

    #[test]
    fn keycode_lookup_prefers_the_unshifted_level_and_reports_absence() {
        // A real fragment of a GetKeyboardMapping reply: 2 keysyms per keycode
        // from min_keycode 8. Keycode 9 is Escape, 23 is Tab with ISO_Left_Tab at
        // the shifted level.
        let per = 2u8;
        let min = 8u8;
        let mut keysyms = vec![0u32; 16 * usize::from(per)];
        let slot = |keycode: u8| (usize::from(keycode) - usize::from(min)) * usize::from(per);
        keysyms[slot(9)] = KEYSYM_ESCAPE;
        keysyms[slot(23)] = KEYSYM_TAB;
        keysyms[slot(23) + 1] = 0xfe20; // ISO_Left_Tab
        assert_eq!(
            keycode_for_keysym(min, per, &keysyms, KEYSYM_ESCAPE),
            Some(9)
        );
        assert_eq!(keycode_for_keysym(min, per, &keysyms, KEYSYM_TAB), Some(23));
        assert_eq!(keycode_for_keysym(min, per, &keysyms, 0xfe20), Some(23));
        // A keysym this layout does not carry must be reported absent, so the tap
        // degrades instead of grabbing keycode 0.
        assert_eq!(keycode_for_keysym(min, per, &keysyms, KEYSYM_GRAVE), None);
        // Degenerate replies must not divide by zero or panic.
        assert_eq!(keycode_for_keysym(min, 0, &keysyms, KEYSYM_TAB), None);
        assert_eq!(keycode_for_keysym(min, per, &[], KEYSYM_TAB), None);
    }

    #[test]
    fn keycode_lookup_falls_back_to_a_shifted_level_when_that_is_the_only_match() {
        // Some layouts only reach grave through a shifted level; the tap should
        // still find the physical key rather than report no keycode.
        let keysyms = [0u32, KEYSYM_GRAVE];
        assert_eq!(keycode_for_keysym(8, 2, &keysyms, KEYSYM_GRAVE), Some(8));
    }

    #[test]
    fn the_watchdog_thaws_a_frozen_keyboard_before_anything_else() {
        // The hazard this whole watchdog exists for: a GrabModeSync grab freezes
        // every application's keyboard until XAllowEvents. Past the budget it must
        // fail OPEN (replay the keystroke to the app) and drop the grab.
        assert_eq!(
            watchdog_action(FREEZE_BUDGET_MS, 0, 0, UNSET_MS),
            WatchdogAction::ThawAndDisarm
        );
        // One millisecond inside the budget is still the normal path: the resolve
        // happens in the same loop iteration, in microseconds.
        assert_eq!(
            watchdog_action(FREEZE_BUDGET_MS - 1, 0, 0, UNSET_MS),
            WatchdogAction::Nothing
        );
        // A frozen keyboard outranks both other deadlines.
        assert_eq!(
            watchdog_action(MAX_ARMED_MS, 0, 0, 0),
            WatchdogAction::ThawAndDisarm
        );
    }

    #[test]
    fn the_watchdog_disarms_a_stuck_arm_and_an_elapsed_scheduled_hide() {
        // A missed hide must not leave Tab intercepted forever.
        assert_eq!(
            watchdog_action(MAX_ARMED_MS, UNSET_MS, 0, UNSET_MS),
            WatchdogAction::Disarm
        );
        assert_eq!(
            watchdog_action(MAX_ARMED_MS - 1, UNSET_MS, 0, UNSET_MS),
            WatchdogAction::Nothing
        );
        // The engine's delayed-hide failsafe (hide_suggestion_after).
        assert_eq!(
            watchdog_action(500, UNSET_MS, 400, 500),
            WatchdogAction::Disarm
        );
        assert_eq!(
            watchdog_action(499, UNSET_MS, 400, 500),
            WatchdogAction::Nothing
        );
        // Nothing set at all: idle.
        assert_eq!(
            watchdog_action(u64::MAX - 1, UNSET_MS, UNSET_MS, UNSET_MS),
            WatchdogAction::Nothing
        );
    }

    #[test]
    fn watchdog_arithmetic_cannot_wrap_on_a_clock_that_went_backwards() {
        // now < since should read as "no time has passed", never as a huge
        // elapsed value that force-disarms every tick.
        assert_eq!(
            watchdog_action(0, 10, 10, UNSET_MS),
            WatchdogAction::Nothing
        );
    }
}
