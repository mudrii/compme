//! Pure event→contract mapping for the AT-SPI2 event path.
//!
//! The two decisions the event workers make on every signal live here rather than
//! in the D-Bus code, so both are tested on every host this crate builds on — the
//! macOS development machines and the Windows CI lane included, where there is no
//! accessibility bus to receive a signal from.
//!
//! - **Field identity.** [`LinuxFieldRegistry`] is the single focus-owned source
//!   of [`FieldHandle`] generations for event delivery and adapter I/O.
//! - **Coalescing.** A caret event fires per keystroke and resolving its geometry
//!   costs D-Bus round trips, so [`latest`] collapses a burst to its newest event —
//!   the only one whose caret position is still true.

use crate::atspi_ids::ElementId;
use platform::{FieldHandle, PlatformError};
use std::sync::mpsc;

/// The adapter-owned identity authority for Linux AT-SPI fields.
///
/// Focus events are the only events allowed to mint a handle. Caret events may
/// reuse the current handle, but a bus-wide event for any other accessible is
/// rejected. Every I/O path validates against the same current identity and
/// generation before addressing the accessible.
#[derive(Debug, Default)]
pub struct LinuxFieldRegistry {
    current: Option<FieldHandle>,
    minted: u64,
}

impl LinuxFieldRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint or reuse the current focused field.
    pub fn focus(
        &mut self,
        element: &ElementId,
        describe: impl FnOnce() -> (String, Option<u32>),
    ) -> FieldHandle {
        let element_id = element.encode();
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
            element_id,
            generation: self.minted,
        };
        self.current = Some(handle.clone());
        handle
    }

    /// The current handle only when `element` is the focused accessible.
    pub fn current_for(&self, element: &ElementId) -> Option<FieldHandle> {
        let encoded = element.encode();
        self.current
            .as_ref()
            .filter(|current| current.element_id == encoded)
            .cloned()
    }

    /// The application owning the current focused field, if one is registered.
    pub fn current_app(&self) -> Option<String> {
        self.current.as_ref().map(|current| current.app.clone())
    }

    /// Validate both native element identity and focus generation.
    pub fn validate(&self, field: &FieldHandle) -> Result<ElementId, PlatformError> {
        let current = self.current.as_ref().ok_or(PlatformError::StaleField)?;
        if current.element_id != field.element_id || current.generation != field.generation {
            return Err(PlatformError::StaleField);
        }
        ElementId::decode(&field.element_id).ok_or(PlatformError::StaleField)
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
    fn linux_field_registry_reuses_focus_identity_for_caret_events() {
        let mut registry = LinuxFieldRegistry::new();
        let element = ElementId::new(":1.42", "/entry");
        let mut descriptions = 0;

        let focused = registry.focus(&element, || {
            descriptions += 1;
            ("fixture".to_string(), Some(4242))
        });
        let duplicate_focus = registry.focus(&element, || {
            descriptions += 1;
            ("must not be consulted".to_string(), None)
        });

        assert_eq!(registry.current_for(&element), Some(focused));
        assert_eq!(duplicate_focus.generation, 1);
        assert_eq!(
            descriptions, 1,
            "duplicate focus must not repeat metadata I/O"
        );
    }

    #[test]
    fn linux_field_registry_drops_foreign_caret_without_advancing_generation() {
        let mut registry = LinuxFieldRegistry::new();
        let entry = ElementId::new(":1.42", "/entry");
        let foreign = ElementId::new(":1.99", "/foreign");
        let next = ElementId::new(":1.42", "/next");

        let focused = registry.focus(&entry, describing("fixture"));
        assert_eq!(registry.current_for(&foreign), None);
        let next = registry.focus(&next, describing("fixture"));

        assert_eq!((focused.generation, next.generation), (1, 2));
    }

    #[test]
    fn linux_field_registry_revisit_invalidates_the_first_handle() {
        let mut registry = LinuxFieldRegistry::new();
        let entry = ElementId::new(":1.42", "/entry");
        let view = ElementId::new(":1.42", "/view");

        let first_entry = registry.focus(&entry, describing("fixture"));
        let view = registry.focus(&view, describing("fixture"));
        let current_entry = registry.focus(&entry, describing("fixture"));

        assert_eq!(
            (
                first_entry.generation,
                view.generation,
                current_entry.generation
            ),
            (1, 2, 3)
        );
        assert_eq!(
            registry.validate(&first_entry),
            Err(PlatformError::StaleField)
        );
        assert_eq!(registry.validate(&view), Err(PlatformError::StaleField));
        assert_eq!(registry.validate(&current_entry), Ok(entry));
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
