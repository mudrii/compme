//! Constrained XTEST plain-text insertion for X11.
//!
//! XTEST emits keycodes, not text. This module therefore uses only keysyms
//! already present in the active keyboard map and preflights the complete
//! string before emitting its first event. It never rewrites the process-global
//! keyboard map and never enters toolkit-specific Unicode compose sequences:
//! either choice could expose a partial insert or disturb another application.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use platform::PlatformError;
use x11rb::connection::Connection as _;
use x11rb::protocol::xkb::{ConnectionExt as _, ID};
use x11rb::protocol::xproto::{ConnectionExt as _, KEY_PRESS_EVENT, KEY_RELEASE_EVENT};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

const XK_SHIFT_L: u32 = 0xffe1;
const MODIFIER_RELEASE_BUDGET: Duration = Duration::from_millis(250);
const MODIFIER_POLL_INTERVAL: Duration = Duration::from_millis(5);
const PROBE_BUDGET: Duration = Duration::from_millis(250);

// Never infer the meaning of Mod1..Mod5 from their conventional slot. A custom
// layout may bind Level3 or a group switch to any one of them, including Mod2.
const ANY_CORE_MODIFIER: u16 = 0xff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeyStroke {
    keycode: u8,
    shift: bool,
}

/// A fully preflighted insertion. Construction performs every fallible query
/// that can be completed before the mutation boundary; `dispatch` only emits
/// the already-resolved key events.
pub(crate) struct PreparedInsert {
    connection: RustConnection,
    root: u32,
    min_keycode: u8,
    keycode_count: u8,
    keysyms_per_keycode: u8,
    keysyms: Vec<u32>,
    shift_keycode: Option<u8>,
    strokes: Vec<KeyStroke>,
}

pub(crate) fn probe() -> bool {
    bounded_probe(PROBE_BUDGET, || open().is_ok())
}

fn bounded_probe(deadline: Duration, probe: impl FnOnce() -> bool + Send + 'static) -> bool {
    let (tx, rx) = mpsc::sync_channel(1);
    if std::thread::Builder::new()
        .name("compme-x11-insert-probe".into())
        .spawn(move || {
            let _ = tx.send(probe());
        })
        .is_err()
    {
        return false;
    }
    rx.recv_timeout(deadline).unwrap_or(false)
}

pub(crate) fn prepare(text: &str) -> Result<PreparedInsert, PlatformError> {
    let (connection, root) = open()?;
    let setup = connection.setup();
    let min_keycode = setup.min_keycode;
    let count = setup
        .max_keycode
        .saturating_sub(min_keycode)
        .saturating_add(1);
    let mapping = connection
        .get_keyboard_mapping(min_keycode, count)
        .map_err(|err| cannot_complete("get_keyboard_mapping", err))?
        .reply()
        .map_err(|err| cannot_complete("keyboard mapping reply", err))?;
    let strokes = plan_for_mapping(
        text,
        min_keycode,
        mapping.keysyms_per_keycode,
        &mapping.keysyms,
    )?;
    let needs_shift = strokes.iter().any(|stroke| stroke.shift);
    let shift_keycode = needs_shift
        .then(|| {
            keycode_for_level(
                min_keycode,
                mapping.keysyms_per_keycode,
                &mapping.keysyms,
                XK_SHIFT_L,
            )
            .map(|stroke| stroke.keycode)
            .ok_or_else(|| unsupported("the active X11 layout has no Shift key"))
        })
        .transpose()?;
    refuse_compme_grab_collisions(
        &strokes,
        min_keycode,
        mapping.keysyms_per_keycode,
        &mapping.keysyms,
        &crate::x11_keys::configured_bindings(),
        &crate::x11_shortcuts::ShortcutPlan::from_bindings(
            crate::x11_shortcuts::configured_bindings(),
        ),
    )?;

    Ok(PreparedInsert {
        connection,
        root,
        min_keycode,
        keycode_count: count,
        keysyms_per_keycode: mapping.keysyms_per_keycode,
        keysyms: mapping.keysyms,
        shift_keycode,
        strokes,
    })
}

impl PreparedInsert {
    pub(crate) fn is_empty(&self) -> bool {
        self.strokes.is_empty()
    }

    /// Wait briefly for the accept chord's physical keys to be released.
    /// An active passive grab remains in force until its triggering key comes
    /// up, and any other held key or modifier can change where XTEST events go
    /// or what they mean.
    pub(crate) fn wait_for_clear_keyboard(&self) -> Result<(), PlatformError> {
        let deadline = Instant::now() + MODIFIER_RELEASE_BUDGET;
        loop {
            if self.keyboard_state_is_clear()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(unsupported(
                    "XTEST insert refused while a key, modifier, or non-primary XKB layout group is active",
                ));
            }
            std::thread::sleep(MODIFIER_POLL_INTERVAL);
        }
    }

    /// One-shot counterpart used directly beside the XTEST dispatch boundary.
    /// The earlier wait handles an accept chord being released normally; this
    /// catches a modifier or group switch that appears during AT-SPI prep.
    pub(crate) fn require_clear_keyboard_now(&self) -> Result<(), PlatformError> {
        if self.keyboard_state_is_clear()? {
            Ok(())
        } else {
            Err(unsupported(
                "XTEST insert refused because keyboard state changed before dispatch",
            ))
        }
    }

    /// Recheck the two process-global inputs to the plan after AT-SPI prep:
    /// keyboard mapping and compme's configured passive grabs. A layout or
    /// shortcut rebind must invalidate the prepared strokes before dispatch.
    pub(crate) fn require_plan_still_safe_now(&self) -> Result<(), PlatformError> {
        let mapping = self
            .connection
            .get_keyboard_mapping(self.min_keycode, self.keycode_count)
            .map_err(|err| cannot_complete("final keyboard mapping", err))?
            .reply()
            .map_err(|err| cannot_complete("final keyboard mapping reply", err))?;
        if mapping.keysyms_per_keycode != self.keysyms_per_keycode
            || mapping.keysyms != self.keysyms
        {
            return Err(unsupported(
                "the X11 keyboard mapping changed while preparing the insert",
            ));
        }
        refuse_compme_grab_collisions(
            &self.strokes,
            self.min_keycode,
            self.keysyms_per_keycode,
            &self.keysyms,
            &crate::x11_keys::configured_bindings(),
            &crate::x11_shortcuts::ShortcutPlan::from_bindings(
                crate::x11_shortcuts::configured_bindings(),
            ),
        )
    }

    fn keyboard_state_is_clear(&self) -> Result<bool, PlatformError> {
        let pointer = self
            .connection
            .query_pointer(self.root)
            .map_err(|err| cannot_complete("query_pointer", err))?
            .reply()
            .map_err(|err| cannot_complete("query_pointer reply", err))?;
        let xkb = self
            .connection
            .xkb_get_state(u16::from(ID::USE_CORE_KBD))
            .map_err(|err| cannot_complete("XKB get state", err))?
            .reply()
            .map_err(|err| cannot_complete("XKB get state reply", err))?;
        let keymap = self
            .connection
            .query_keymap()
            .map_err(|err| cannot_complete("query_keymap", err))?
            .reply()
            .map_err(|err| cannot_complete("query_keymap reply", err))?;
        Ok(no_physical_keys_pressed(&keymap.keys)
            && keyboard_state_is_primary_and_unmodified(
                u16::from(pointer.mask),
                u16::from(xkb.mods),
                u16::from(xkb.base_mods),
                u16::from(xkb.latched_mods),
                u16::from(xkb.locked_mods),
                u8::from(xkb.group),
                u8::from(xkb.locked_group),
                xkb.base_group,
                xkb.latched_group,
            ))
    }

    /// Emit the preflighted sequence. The caller establishes its mutation
    /// boundary before entering this method and maps every error here to an
    /// unknown outcome. A release guard makes a best effort to lift any key
    /// whose press reached the X server before a later request failed.
    pub(crate) fn dispatch(&self) -> Result<(), PlatformError> {
        let mut pressed = PressedKeys::new(&self.connection, self.root);
        for stroke in &self.strokes {
            if stroke.shift {
                pressed.press(self.shift_keycode.expect("shift was preflighted"))?;
            }
            pressed.press(stroke.keycode)?;
            pressed.release(stroke.keycode)?;
            if stroke.shift {
                pressed.release(self.shift_keycode.expect("shift was preflighted"))?;
            }
        }
        self.connection
            .flush()
            .map_err(|err| cannot_complete("flush", err))?;
        Ok(())
    }
}

fn no_physical_keys_pressed(keys: &[u8; 32]) -> bool {
    keys.iter().all(|byte| *byte == 0)
}

fn refuse_compme_grab_collisions(
    strokes: &[KeyStroke],
    min_keycode: u8,
    keysyms_per_keycode: u8,
    keysyms: &[u32],
    accept: &crate::x11_keys::AcceptBindings,
    shortcuts: &crate::x11_shortcuts::ShortcutPlan,
) -> Result<(), PlatformError> {
    let collides = |keysym: u32, modifiers: u16| {
        let Some(keycode) =
            crate::x11_keys::keycode_for_keysym(min_keycode, keysyms_per_keycode, keysyms, keysym)
        else {
            return false;
        };
        strokes.iter().any(|stroke| {
            let generated_modifiers = if stroke.shift {
                crate::x11_keys::X11_SHIFT
            } else {
                0
            };
            stroke.keycode == keycode && generated_modifiers == modifiers
        })
    };
    if accept
        .iter()
        .any(|binding| collides(binding.keysym, binding.modifiers))
        || shortcuts
            .iter()
            .any(|binding| collides(binding.keysym, binding.modifiers))
    {
        return Err(unsupported(
            "planned keystrokes collide with a configured compme passive grab",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn keyboard_state_is_primary_and_unmodified(
    pointer_modifiers: u16,
    effective_modifiers: u16,
    base_modifiers: u16,
    latched_modifiers: u16,
    locked_modifiers: u16,
    effective_group: u8,
    locked_group: u8,
    base_group: i16,
    latched_group: i16,
) -> bool {
    [
        pointer_modifiers,
        effective_modifiers,
        base_modifiers,
        latched_modifiers,
        locked_modifiers,
    ]
    .into_iter()
    .all(|modifiers| modifiers & ANY_CORE_MODIFIER == 0)
        && effective_group == 0
        && locked_group == 0
        && base_group == 0
        && latched_group == 0
}

fn open() -> Result<(RustConnection, u32), PlatformError> {
    let (connection, screen) =
        x11rb::connect(None).map_err(|err| cannot_complete("connect to DISPLAY", err))?;
    connection
        .xtest_get_version(2, 2)
        .map_err(|err| cannot_complete("XTEST version request", err))?
        .reply()
        .map_err(|err| cannot_complete("XTEST version reply", err))?;
    let xkb = connection
        .xkb_use_extension(1, 0)
        .map_err(|err| cannot_complete("XKB version request", err))?
        .reply()
        .map_err(|err| cannot_complete("XKB version reply", err))?;
    if !xkb.supported {
        return Err(unsupported(
            "the X server does not support XKB state queries",
        ));
    }
    let root = connection.setup().roots[screen].root;
    Ok((connection, root))
}

fn plan_for_mapping(
    text: &str,
    min_keycode: u8,
    keysyms_per_keycode: u8,
    keysyms: &[u32],
) -> Result<Vec<KeyStroke>, PlatformError> {
    text.chars()
        .map(|character| {
            let keysym = keysym_for_character(character).ok_or_else(|| {
                unsupported(format!(
                    "XTEST insert cannot represent control character U+{:04X}",
                    u32::from(character)
                ))
            })?;
            keycode_for_level(min_keycode, keysyms_per_keycode, keysyms, keysym).ok_or_else(|| {
                unsupported(format!(
                    "XTEST insert cannot represent {character:?} in the active X11 keyboard layout"
                ))
            })
        })
        .collect()
}

fn keysym_for_character(character: char) -> Option<u32> {
    if character.is_control() {
        return None;
    }
    let scalar = u32::from(character);
    Some(if scalar <= 0xff {
        scalar
    } else {
        0x0100_0000 | scalar
    })
}

/// Resolve only group 1's unshifted and shifted levels. Higher levels need a
/// layout-switch modifier, which this safety-constrained path refuses.
fn keycode_for_level(
    min_keycode: u8,
    keysyms_per_keycode: u8,
    keysyms: &[u32],
    target: u32,
) -> Option<KeyStroke> {
    let per = usize::from(keysyms_per_keycode);
    if per == 0 {
        return None;
    }
    keysyms
        .chunks_exact(per)
        .enumerate()
        .find_map(|(index, row)| {
            let keycode = min_keycode.checked_add(u8::try_from(index).ok()?)?;
            if row.first().copied() == Some(target) {
                Some(KeyStroke {
                    keycode,
                    shift: false,
                })
            } else if row.get(1).copied() == Some(target) {
                Some(KeyStroke {
                    keycode,
                    shift: true,
                })
            } else {
                None
            }
        })
}

struct PressedKeys<'a> {
    connection: &'a RustConnection,
    root: u32,
    pressed: Vec<u8>,
}

impl<'a> PressedKeys<'a> {
    fn new(connection: &'a RustConnection, root: u32) -> Self {
        Self {
            connection,
            root,
            pressed: Vec::new(),
        }
    }

    fn press(&mut self, keycode: u8) -> Result<(), PlatformError> {
        // Track before checking: an error reply does not prove the server
        // ignored the request, so Drop still owes a best-effort release.
        self.pressed.push(keycode);
        self.send(KEY_PRESS_EVENT, keycode)
    }

    fn release(&mut self, keycode: u8) -> Result<(), PlatformError> {
        self.send(KEY_RELEASE_EVENT, keycode)?;
        if let Some(position) = self.pressed.iter().rposition(|pressed| *pressed == keycode) {
            self.pressed.remove(position);
        }
        Ok(())
    }

    fn send(&self, event_type: u8, keycode: u8) -> Result<(), PlatformError> {
        self.connection
            .xtest_fake_input(event_type, keycode, 0, self.root, 0, 0, 0)
            .map_err(|err| cannot_complete("XTEST fake input", err))?
            .check()
            .map_err(|err| cannot_complete("XTEST fake input reply", err))
    }
}

impl Drop for PressedKeys<'_> {
    fn drop(&mut self) {
        for keycode in self.pressed.iter().rev().copied() {
            if let Ok(cookie) =
                self.connection
                    .xtest_fake_input(KEY_RELEASE_EVENT, keycode, 0, self.root, 0, 0, 0)
            {
                cookie.ignore_error();
            }
        }
        let _ = self.connection.flush();
    }
}

fn unsupported(reason: impl Into<String>) -> PlatformError {
    PlatformError::UnsupportedField {
        reason: format!("platform_linux x11 insert: {}", reason.into()),
    }
}

fn cannot_complete(what: &str, err: impl std::fmt::Display) -> PlatformError {
    PlatformError::CannotComplete {
        reason: format!("platform_linux x11 insert {what}: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping() -> Vec<u32> {
        vec![
            u32::from('a'),
            u32::from('A'),
            u32::from('1'),
            u32::from('!'),
            XK_SHIFT_L,
            0,
            u32::from('`'),
            u32::from('~'),
            u32::from(' '),
            0,
        ]
    }

    #[test]
    fn complete_string_is_preflighted_into_unshifted_and_shifted_strokes() {
        assert_eq!(
            plan_for_mapping("aA1!", 8, 2, &mapping()).expect("mapped text"),
            vec![
                KeyStroke {
                    keycode: 8,
                    shift: false,
                },
                KeyStroke {
                    keycode: 8,
                    shift: true,
                },
                KeyStroke {
                    keycode: 9,
                    shift: false,
                },
                KeyStroke {
                    keycode: 9,
                    shift: true,
                },
            ]
        );

        let accept = crate::x11_keys::AcceptBindings::defaults();
        let no_shortcuts = crate::x11_shortcuts::ShortcutPlan::default();
        let grave = plan_for_mapping("`", 8, 2, &mapping()).expect("mapped grave");
        assert!(matches!(
            refuse_compme_grab_collisions(&grave, 8, 2, &mapping(), &accept, &no_shortcuts),
            Err(PlatformError::UnsupportedField { .. })
        ));

        // Collision is about the generated physical chord, not the printable
        // output keysym: Shift+grave produces '~' but still activates a
        // configured Shift+grave passive grab.
        let shifted_grave =
            crate::x11_shortcuts::ShortcutPlan::from_bindings(shell_flags::ShortcutBindings {
                force_activate: Some((50, 1 << 9)),
                ..shell_flags::ShortcutBindings::default()
            });
        let tilde = plan_for_mapping("~", 8, 2, &mapping()).expect("mapped tilde");
        assert!(matches!(
            refuse_compme_grab_collisions(
                &tilde,
                8,
                2,
                &mapping(),
                &crate::x11_keys::AcceptBindings::default(),
                &shifted_grave
            ),
            Err(PlatformError::UnsupportedField { .. })
        ));
    }

    #[test]
    fn unrepresentable_or_control_character_refuses_the_whole_plan() {
        for text in ["a😀", "a\n"] {
            assert!(matches!(
                plan_for_mapping(text, 8, 2, &mapping()),
                Err(PlatformError::UnsupportedField { .. })
            ));
        }
    }

    #[test]
    fn unicode_keysym_already_present_in_layout_is_supported_without_remapping() {
        let mapping = [0x0100_03bb, 0];
        assert_eq!(
            plan_for_mapping("λ", 24, 2, &mapping).expect("mapped Unicode scalar"),
            vec![KeyStroke {
                keycode: 24,
                shift: false,
            }]
        );
    }

    #[test]
    fn any_modifier_or_nonprimary_xkb_group_refuses_dispatch() {
        assert!(keyboard_state_is_primary_and_unmodified(
            0, 0, 0, 0, 0, 0, 0, 0, 0
        ));
        // Mod2 is not assumed to be NumLock: a custom layout can bind it to
        // Level3 or a group switch.
        assert!(!keyboard_state_is_primary_and_unmodified(
            1 << 4,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0
        ));
        assert!(!keyboard_state_is_primary_and_unmodified(
            0, 0, 0, 0, 0, 1, 1, 0, 0
        ));
        assert!(!keyboard_state_is_primary_and_unmodified(
            0, 0, 0, 0, 0, 0, 0, 0, 1
        ));
        assert!(no_physical_keys_pressed(&[0; 32]));
        let mut held = [0; 32];
        held[6] = 1 << 2;
        assert!(!no_physical_keys_pressed(&held));
    }

    #[test]
    fn startup_probe_is_bounded_when_the_x_server_never_replies() {
        let (release_tx, release_rx) = mpsc::channel();
        assert!(!bounded_probe(Duration::from_millis(10), move || {
            let _ = release_rx.recv();
            true
        }));
        release_tx.send(()).expect("release detached probe");
        assert!(bounded_probe(Duration::from_secs(1), || true));
    }
}
