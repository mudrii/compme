//! Element identity for the AT-SPI2 adapter — pure, so it is testable on every
//! host (this crate also builds on macOS and Windows, where no a11y bus exists).
//!
//! AT-SPI2 addresses an accessible by a D-Bus *pair*: the owning application's
//! unique bus name (`:1.42`) and the object path
//! (`/org/a11y/atspi/accessible/17`). [`platform::FieldHandle`] carries a single
//! `element_id: String`, so the pair is encoded into it and parsed back out.
//!
//! Why a string join rather than a side table: the handle crosses the portable
//! engine, is compared for identity, and outlives any adapter-side cache. A
//! self-describing id keeps "same field?" answerable without shared mutable
//! state — the same reason the macOS adapter puts `hash=`/`pid:` segments in its
//! own key.

/// Separator between bus name and object path. Neither may contain it: D-Bus
/// bus names are `[A-Za-z0-9_-]` segments after a `:` or dot, and object paths
/// are `/`-separated `[A-Za-z0-9_]`.
const SEP: char = '|';

/// A D-Bus accessible address: unique bus name plus object path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElementId {
    pub bus_name: String,
    pub path: String,
}

impl ElementId {
    pub fn new(bus_name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            bus_name: bus_name.into(),
            path: path.into(),
        }
    }

    /// The `element_id` string carried in a `FieldHandle`.
    pub fn encode(&self) -> String {
        format!("{}{SEP}{}", self.bus_name, self.path)
    }

    /// Parse an `element_id` produced by [`encode`](Self::encode). Returns
    /// `None` for anything else — a malformed id must fail closed at the adapter
    /// boundary rather than be "repaired" into an address that resolves to some
    /// other accessible.
    pub fn decode(encoded: &str) -> Option<Self> {
        let (bus_name, path) = encoded.split_once(SEP)?;
        if bus_name.is_empty() || !path.starts_with('/') {
            return None;
        }
        // A second separator means the input was not produced by encode(); the
        // path would be truncated, silently addressing an ancestor.
        if path.contains(SEP) {
            return None;
        }
        Some(Self::new(bus_name, path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_id_round_trips_a_dbus_address() {
        let id = ElementId::new(":1.42", "/org/a11y/atspi/accessible/17");
        let encoded = id.encode();
        assert_eq!(encoded, ":1.42|/org/a11y/atspi/accessible/17");
        assert_eq!(ElementId::decode(&encoded), Some(id));
    }

    #[test]
    fn element_id_decode_fails_closed_on_anything_it_did_not_encode() {
        // Each of these could otherwise be "repaired" into an address that
        // resolves to a *different* accessible — the field the user is not
        // typing in — so every one must be rejected.
        for bad in [
            "",                                   // empty
            ":1.42",                              // no separator
            "|/org/a11y/atspi/accessible/17",     // empty bus name
            ":1.42|",                             // empty path
            ":1.42|org/a11y/atspi/accessible/17", // path not absolute
            ":1.42|/org/a11y|/accessible/17",     // ambiguous: two separators
            "/org/a11y/atspi/accessible/17",      // path only
        ] {
            assert_eq!(ElementId::decode(bad), None, "must reject {bad:?}");
        }
    }

    #[test]
    fn element_id_keeps_the_root_path_usable() {
        // The application root is a legitimate target (front_app reads its
        // name), and `/` is the shortest valid path.
        let id = ElementId::new(":1.7", "/");
        assert_eq!(ElementId::decode(&id.encode()), Some(id));
    }
}
