//! Pure event→contract mapping for the AT-SPI2 event path.
//!
//! The two decisions the event workers make on every signal live here rather than
//! in the D-Bus code, so both are tested on every host this crate builds on — the
//! macOS development machines and the Windows CI lane included, where there is no
//! accessibility bus to receive a signal from.
//!
//! - **Handle minting.** [`FieldHandle::generation`] must change when the element
//!   behind a handle is replaced, so a write against an old handle fails instead of
//!   landing in whatever now occupies that address. AT-SPI identifies an accessible
//!   by its `(bus name, object path)` pair, and that pair *is* stable for the life
//!   of the element, so the generation advances exactly when the pair changes.
//! - **Coalescing.** A caret event fires per keystroke and resolving its geometry
//!   costs D-Bus round trips, so [`latest`] collapses a burst to its newest event —
//!   the only one whose caret position is still true.

use platform::FieldHandle;
use std::sync::mpsc;

/// Mints one [`FieldHandle`] per element, reusing it while the element is unchanged.
///
/// `describe` supplies the `app`/`pid` pair, which costs D-Bus round trips on the
/// live path; it is called **only** when the element actually changed, so a burst of
/// caret events inside one field costs none of them.
#[derive(Debug, Default)]
pub struct FieldMinter {
    current: Option<FieldHandle>,
    minted: u64,
}

impl FieldMinter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The handle for `element_id` (an `atspi_ids::ElementId::encode()` string).
    ///
    /// Returning to a previously focused element mints a *fresh* generation rather
    /// than reviving the old handle: the accessible may have been destroyed and
    /// recreated at the same object path in between, and reusing the old generation
    /// would tell the host "same live field" about a field it can no longer trust.
    pub fn handle(
        &mut self,
        element_id: &str,
        describe: impl FnOnce() -> (String, Option<u32>),
    ) -> FieldHandle {
        if let Some(current) = self
            .current
            .as_ref()
            .filter(|current| current.element_id == element_id)
        {
            return current.clone();
        }
        let (app, pid) = describe();
        self.minted += 1;
        let handle = FieldHandle {
            app,
            pid,
            element_id: element_id.to_string(),
            generation: self.minted,
        };
        self.current = Some(handle.clone());
        handle
    }
}

/// The newest value available, given one already received: everything queued behind
/// it is dropped.
///
/// This is the whole coalescing rule. It is deliberately *not* a debounce: the
/// caller has an event in hand and will act on one, so the only question is which,
/// and a superseded caret position is worthless.
pub fn latest<T>(received: T, queued: &mpsc::Receiver<T>) -> T {
    let mut newest = received;
    while let Ok(next) = queued.try_recv() {
        newest = next;
    }
    newest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn describing(app: &str) -> impl FnOnce() -> (String, Option<u32>) + '_ {
        move || (app.to_string(), Some(4242))
    }

    #[test]
    fn field_minter_reuses_a_handle_while_the_element_is_unchanged() {
        let mut minter = FieldMinter::new();
        let mut lookups = 0;

        let first = minter.handle(":1.42|/org/a11y/atspi/accessible/17", || {
            lookups += 1;
            ("gedit".to_string(), Some(991))
        });
        let again = minter.handle(":1.42|/org/a11y/atspi/accessible/17", || {
            lookups += 1;
            ("must not be consulted".to_string(), None)
        });

        assert_eq!(first, again, "the same element must yield the same handle");
        assert_eq!(first.app, "gedit");
        assert_eq!(first.pid, Some(991));
        assert_eq!(
            lookups, 1,
            "the app/pid lookup costs D-Bus round trips, so it must not repeat per event"
        );
    }

    #[test]
    fn field_minter_advances_the_generation_for_every_element_change() {
        // The contract's stale-handle guarantee: a handle for a replaced element must
        // not compare equal to the new one. Returning to an element already seen is
        // the case that matters — reviving its old generation would claim the field
        // is the same live element when the toolkit may have rebuilt it.
        let mut minter = FieldMinter::new();

        let entry = minter.handle(":1.42|/entry", describing("fixture"));
        let view = minter.handle(":1.42|/view", describing("fixture"));
        let entry_again = minter.handle(":1.42|/entry", describing("fixture"));

        assert_eq!(
            (entry.generation, view.generation, entry_again.generation),
            (1, 2, 3)
        );
        assert_ne!(
            entry, entry_again,
            "a revisited element must not reuse its old handle"
        );
    }

    #[test]
    fn latest_collapses_a_burst_to_its_newest_event() {
        let (tx, rx) = mpsc::channel();
        for position in 2..=5 {
            tx.send(position).unwrap();
        }

        // 1 stands for the event the dispatcher already took off the queue.
        assert_eq!(latest(1, &rx), 5);
        // Nothing queued: the received value is the newest by definition.
        assert_eq!(latest(6, &rx), 6);
        // A dropped sender is not an error — the worker is shutting down and the
        // value in hand is still the newest one.
        drop(tx);
        assert_eq!(latest(7, &rx), 7);
    }
}
