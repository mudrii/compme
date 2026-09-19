//! UIA element-identity codec — pure, compiled and tested on every host
//! (this crate also builds on macOS and Linux, which have no UIA).
//!
//! UI Automation identifies elements by a provider-assigned *runtime id*: an
//! `i32` array that is unique within the owning process and stable while the
//! element lives. The adapter encodes the pair (owning pid, runtime id) into a
//! `FieldHandle::element_id` string so the engine can carry the identity the
//! same way it carries the macOS `ax:` and Linux `atspi:` ids.
//!
//! The decoder fails closed on anything this module did not encode — the same
//! rule as `platform_linux::atspi_ids`. A "repaired" or foreign id must never
//! resolve, because a mis-decoded id would address a *different* element and
//! the engine would act on text it never read.

/// Every id this module mints starts with this prefix.
pub const UIA_ID_PREFIX: &str = "uia:";

/// Encode `(pid, runtime_id)` into a `FieldHandle::element_id`.
///
/// Runtime-id components are joined with `.` — never `-`, because runtime-id
/// components are signed `i32` and a `-` separator would make the encoding
/// ambiguous to parse. An empty runtime id encodes as an empty `rid`, which
/// the decoder refuses: no real UIA element has one.
pub fn encode_element_id(pid: u32, runtime_id: &[i32]) -> String {
    let mut id = format!("{UIA_ID_PREFIX}pid={pid}:rid=");
    for (index, component) in runtime_id.iter().enumerate() {
        if index > 0 {
            id.push('.');
        }
        id.push_str(&component.to_string());
    }
    id
}

/// Decode an id this module minted. `None` for anything else — foreign
/// prefixes, missing/malformed pid or rid, empty components — never a guess.
pub fn decode_element_id(id: &str) -> Option<(u32, Vec<i32>)> {
    let rest = id.strip_prefix(UIA_ID_PREFIX)?;
    let (pid_part, rid_part) = rest.split_once(":rid=")?;
    let pid = pid_part.strip_prefix("pid=")?.parse::<u32>().ok()?;
    if rid_part.is_empty() {
        return None;
    }
    let runtime_id = rid_part
        .split('.')
        .map(|component| component.parse::<i32>().ok())
        .collect::<Option<Vec<_>>>()?;
    Some((pid, runtime_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_pid_and_multi_component_runtime_id() {
        let id = encode_element_id(4242, &[42, 1_000_000, -7]);
        assert_eq!(id, "uia:pid=4242:rid=42.1000000.-7");
        assert_eq!(
            decode_element_id(&id),
            Some((4242, vec![42, 1_000_000, -7]))
        );
    }

    #[test]
    fn single_component_round_trips() {
        let id = encode_element_id(1, &[0x2a]);
        assert_eq!(decode_element_id(&id), Some((1, vec![0x2a])));
    }

    #[test]
    fn refuses_foreign_prefixes_from_other_adapters() {
        assert_eq!(decode_element_id("ax:pid=1:rid=2"), None);
        assert_eq!(decode_element_id("atspi:name=:path=/x"), None);
        assert_eq!(decode_element_id(""), None);
    }

    #[test]
    fn refuses_missing_or_empty_rid() {
        assert_eq!(decode_element_id("uia:pid=1"), None);
        assert_eq!(decode_element_id("uia:pid=1:rid="), None);
    }

    #[test]
    fn refuses_malformed_pid_and_components() {
        assert_eq!(decode_element_id("uia:pid=:rid=1"), None);
        assert_eq!(decode_element_id("uia:pid=x:rid=1"), None);
        assert_eq!(decode_element_id("uia:pid=-1:rid=1"), None);
        assert_eq!(decode_element_id("uia:pid=1:rid=1..2"), None);
        assert_eq!(decode_element_id("uia:pid=1:rid=.1"), None);
        assert_eq!(decode_element_id("uia:pid=1:rid=1."), None);
        assert_eq!(decode_element_id("uia:pid=1:rid=1;2"), None);
    }

    #[test]
    fn refuses_arbitrary_pid_labels_and_malformed_unicode_without_panicking() {
        assert_eq!(decode_element_id("uia:xxx=1:rid=2"), None);
        assert_eq!(decode_element_id("uia:abcé:rid=2"), None);
        assert_eq!(decode_element_id("uia:😀:rid=2"), None);
    }

    #[test]
    fn extreme_component_values_round_trip() {
        let id = encode_element_id(u32::MAX, &[i32::MAX, i32::MIN, 0]);
        assert_eq!(
            decode_element_id(&id),
            Some((u32::MAX, vec![i32::MAX, i32::MIN, 0]))
        );
    }
}
