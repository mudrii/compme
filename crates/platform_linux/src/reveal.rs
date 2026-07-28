//! Reveal a file in the Linux file browser (ROADMAP Phase 2.6).
//!
//! **Linux has no portable "select this file" call.** `xdg-open` only opens a
//! target with its default handler: given a file it launches the *application*
//! for that file type (opening a `.gguf` in a text editor), and given a directory
//! it opens the folder without selecting anything. The one interface that selects
//! an item is `org.freedesktop.FileManager1.ShowItems` — a D-Bus interface
//! implemented by Nautilus, Dolphin, Thunar, Nemo, PCManFM and others, and
//! D-Bus-activatable, so the file manager need not already be running.
//!
//! So: try `ShowItems` first, and fall back to `xdg-open` on the *containing
//! directory* — never on the file itself, since launching an editor is not what
//! "reveal" means and could execute a handler on a model file. The fallback goes
//! through the same launcher as `LinuxShellHost::open_url`, so an
//! immediate `xdg-open` failure is still reported instead of swallowed.
//!
//! Known limit, inherited from that launcher and identical to `open_url`'s: only a
//! failure inside the ~50 ms poll window is reported. On a host that *has*
//! `xdg-open` but no desktop (verified on the headless box: `Ok(())`), the launch
//! is accepted and the failure surfaces later in the reaped child. Reporting a
//! definitive answer would mean blocking the caller on a GUI process, which
//! `open_url` already decided against.
//!
//! The path arithmetic (file URI, containing directory) is pure and POSIX-encoded
//! on the string: this crate is also built and tested on Windows, where
//! `Path::is_absolute("/home/u")` is false and `Path::parent` follows the *build*
//! host's separators.

// Only the D-Bus/launcher half needs the error type; the pure path arithmetic
// below is infallible-or-`None`, so the import is Linux-only to keep the macOS
// and Windows lanes warning-free under `-D warnings`.
#[cfg(target_os = "linux")]
use platform::PlatformError;

/// The D-Bus interface, well-known name, and object path are all this string;
/// FileManager1's spec uses the same value for all three.
#[cfg(target_os = "linux")]
pub const FILE_MANAGER1_NAME: &str = "org.freedesktop.FileManager1";
#[cfg(target_os = "linux")]
pub const FILE_MANAGER1_PATH: &str = "/org/freedesktop/FileManager1";

/// Percent-encode `path` into a `file://` URI, or `None` when it is not a POSIX
/// absolute path — a relative path has no URI, and guessing a base would reveal
/// the wrong file.
///
/// Encodes every byte outside RFC 3986's unreserved set, leaving `/` as the path
/// separator. Bytes, not chars: a non-UTF-8 filename is still a valid Linux path,
/// and its URI is the percent-encoding of its bytes.
pub fn file_uri(path: &str) -> Option<String> {
    if !path.starts_with('/') {
        return None;
    }
    let mut uri = String::from("file://");
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(byte as char);
            }
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    Some(uri)
}

/// The directory containing `path`, for the `xdg-open` fallback. `None` when
/// `path` is not POSIX-absolute or is the root itself (there is nothing to
/// reveal `/` inside of). Trailing slashes are trimmed first, so `/a/b/` reveals
/// `/a`, matching how the shell reads it.
pub fn containing_dir(path: &str) -> Option<&str> {
    if !path.starts_with('/') {
        return None;
    }
    let trimmed = path.trim_end_matches('/');
    let (parent, _) = trimmed.rsplit_once('/')?;
    Some(if parent.is_empty() { "/" } else { parent })
}

/// Reveal `path`: `ShowItems` if a file manager answers, else `xdg-open` on the
/// containing directory. Both failures are reported together, because "nothing
/// happened" with no reason is the hardest version of this bug to diagnose.
#[cfg(target_os = "linux")]
pub fn reveal(path: &std::path::Path) -> Result<(), PlatformError> {
    let path = path.to_string_lossy();
    let uri = file_uri(&path).ok_or_else(|| PlatformError::CannotComplete {
        reason: format!("reveal: {path} is not an absolute path"),
    })?;
    let show_items_error = match show_items(&uri) {
        Ok(()) => return Ok(()),
        Err(err) => err,
    };
    let dir = containing_dir(&path).ok_or_else(|| PlatformError::CannotComplete {
        reason: format!("reveal: {path} has no containing directory ({show_items_error})"),
    })?;
    crate::xdg_open(dir).map_err(|err| PlatformError::CannotComplete {
        reason: format!("reveal {path}: {show_items_error}; {err}"),
    })
}

/// `org.freedesktop.FileManager1.ShowItems(URIs: as, StartupId: s)` — the only
/// portable call that *selects* the item. An empty startup id is what a caller
/// without a launch context passes.
#[cfg(target_os = "linux")]
fn show_items(uri: &str) -> Result<(), PlatformError> {
    let connection =
        zbus::blocking::Connection::session().map_err(|err| PlatformError::CannotComplete {
            reason: format!("reveal: no session bus: {err}"),
        })?;
    connection
        .call_method(
            Some(FILE_MANAGER1_NAME),
            FILE_MANAGER1_PATH,
            Some(FILE_MANAGER1_NAME),
            "ShowItems",
            &(vec![uri], ""),
        )
        .map(|_| ())
        .map_err(|err| PlatformError::CannotComplete {
            reason: format!("reveal: {FILE_MANAGER1_NAME} ShowItems: {err}"),
        })
}

/// Live FileManager1 test. In a sibling file (a `#[path]` module) rather than
/// inline, matching how `run_loop` and `platform_macos` keep their tests — see the
/// repo brief's "Where tests live".
#[cfg(all(test, target_os = "linux"))]
#[path = "reveal_live_tests.rs"]
mod live_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uri_percent_encodes_everything_a_path_can_hold() {
        assert_eq!(
            file_uri("/home/u/models/q4.gguf"),
            Some("file:///home/u/models/q4.gguf".to_string())
        );
        // Space, `#` (a URI fragment if left raw), `%` (double-encoding), `&`,
        // quote, and a newline — all legal in a Linux filename.
        assert_eq!(
            file_uri("/tmp/a b#c%d&e'f\ng"),
            Some("file:///tmp/a%20b%23c%25d%26e%27f%0Ag".to_string())
        );
        // Non-ASCII is encoded per UTF-8 byte, not per char.
        assert_eq!(
            file_uri("/tmp/café"),
            Some("file:///tmp/caf%C3%A9".to_string())
        );
        // Unreserved characters must NOT be encoded, or some file managers fail
        // to match the path.
        assert_eq!(
            file_uri("/tmp/A-z_0.9~x"),
            Some("file:///tmp/A-z_0.9~x".to_string())
        );
        // The POSIX absolute rule is encoded on the string: this same assertion
        // runs on the Windows lane, where `Path::is_absolute("/tmp/x")` is false
        // and `C:\` is absolute.
        assert_eq!(file_uri("models/q4.gguf"), None);
        assert_eq!(file_uri(r"C:\Users\u\q4.gguf"), None);
        assert_eq!(file_uri(""), None);
    }

    #[test]
    fn containing_dir_is_the_parent_and_never_the_file() {
        assert_eq!(
            containing_dir("/home/u/models/q4.gguf"),
            Some("/home/u/models")
        );
        assert_eq!(containing_dir("/home/u/models/"), Some("/home/u"));
        // A file directly under the root reveals the root.
        assert_eq!(containing_dir("/q4.gguf"), Some("/"));
        // Nothing sensible to open for the root itself, or for a relative path.
        assert_eq!(containing_dir("/"), None);
        assert_eq!(containing_dir("models/q4.gguf"), None);
        assert_eq!(containing_dir(r"C:\Users\u"), None);
    }
}
