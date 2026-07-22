//! Host shell services beyond field text I/O -- the second half of the
//! cross-platform contract (ROADMAP 1.1). `PlatformAdapter` covers
//! focus/caret/read/insert; `ShellHost` covers the product shell around it:
//! event pumping, permissions, clipboard, OCR context, OS integration,
//! modal confirms, and the memory-key store. One impl per OS.
//!
//! The macOS-product-shaped state vocabulary that used to live here
//! (settings/tray flag bags, the key-chord grammar, `ConfirmPrompt`) lives in
//! the pure `shell_flags` crate; this module keeps only the portable traits.
//!
//! Threading: `ShellHost` is `Send + Sync`; methods that present UI enforce
//! their platform's threading contract internally.

use std::path::Path;
use std::time::Duration;

use shell_flags::ConfirmPrompt;

use crate::{PlatformError, ScreenRect};

pub trait ShellHost: Send + Sync {
    /// Drain queued native UI events, then service the platform main loop for
    /// at most `heartbeat`.
    fn pump_events(&self, heartbeat: Duration);

    /// Whether the process holds the OS grant required to observe and inject
    /// text. Default `true`: Windows and Linux need no macOS-style
    /// Accessibility trust grant.
    fn accessibility_trusted(&self) -> bool {
        true
    }

    /// Fire the OS permission prompt for that grant, if one exists. Returns
    /// the possibly unchanged grant state.
    fn prompt_accessibility_trust(&self) -> bool {
        true
    }

    /// Whether the OS reports a global secure-input session. Default `false`;
    /// per-field secure detection stays `Capabilities::security_state`.
    fn secure_input_enabled(&self) -> bool {
        false
    }

    /// Screen-capture grant probe. Fail closed: unknown means absent, OCR off.
    fn screen_capture_permission(&self) -> bool {
        false
    }

    /// Screen-capture grant request. Fail closed: unknown means absent, OCR off.
    fn request_screen_capture_permission(&self) -> bool {
        false
    }

    fn physical_memory_bytes(&self) -> u64;

    fn bundle_id_for_pid(&self, _pid: i32) -> Option<String> {
        None
    }

    fn read_clipboard_text(&self) -> Option<String> {
        None
    }

    /// Return the OS spell checker's preferred correction when `word` is a
    /// wholly misspelled token. `Ok(None)` means correct, unknown, or
    /// unsupported. The app owns prose/code-field and feature-policy gating.
    fn spelling_correction(&self, _word: &str) -> Result<Option<String>, PlatformError> {
        Ok(None)
    }

    /// OCR'd text near the caret. `None` means unavailable.
    fn screen_context_text(
        &self,
        _caret_rect: Option<ScreenRect>,
        _max_chars: usize,
    ) -> Option<String> {
        None
    }

    fn display_scales(&self) -> Vec<(ScreenRect, f64)> {
        Vec::new()
    }

    /// Open `url` with the OS default handler. Non-blocking.
    fn open_url(&self, url: &str) -> Result<(), PlatformError>;

    /// Open the OS settings pane where the user grants text-access permission.
    fn open_permission_settings(&self) -> Result<(), PlatformError>;

    /// Reveal `path` in the OS file browser.
    fn reveal_file(&self, path: &Path) -> Result<(), PlatformError>;

    fn set_launch_at_login(&self, enabled: bool) -> Result<(), PlatformError>;

    /// Blocking modal confirm. `Ok(true)` only on an explicit confirm click.
    fn confirm(&self, prompt: &ConfirmPrompt<'_>) -> Result<bool, PlatformError>;

    /// 32-byte memory-store encryption key from the OS key store, created on
    /// first use.
    fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError>;
}

/// Status-area handle. Owned by the main loop; implementors may be UI-thread
/// only.
pub trait TrayHandle {
    fn set_status(
        &self,
        title: &str,
        status_line: &str,
        enabled: bool,
        needs_accessibility: bool,
    ) -> Result<(), PlatformError>;

    fn set_stats_line(&self, line: &str) -> Result<(), PlatformError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Minimal impl providing only the required methods. This pins that every
    /// default is fail-closed/absent, so a not-yet-ported platform inherits
    /// safe behavior.
    struct BareHost;

    impl ShellHost for BareHost {
        fn pump_events(&self, _heartbeat: Duration) {}

        fn physical_memory_bytes(&self) -> u64 {
            1
        }

        fn open_url(&self, _url: &str) -> Result<(), PlatformError> {
            Ok(())
        }

        fn open_permission_settings(&self) -> Result<(), PlatformError> {
            Ok(())
        }

        fn reveal_file(&self, _path: &Path) -> Result<(), PlatformError> {
            Ok(())
        }

        fn set_launch_at_login(&self, _enabled: bool) -> Result<(), PlatformError> {
            Ok(())
        }

        fn confirm(&self, _prompt: &ConfirmPrompt<'_>) -> Result<bool, PlatformError> {
            Ok(false)
        }

        fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError> {
            Err(PlatformError::UnsupportedField {
                reason: "bare".into(),
            })
        }
    }

    #[test]
    fn shell_defaults_are_fail_closed() {
        let h = BareHost;

        assert!(h.accessibility_trusted());
        assert!(h.prompt_accessibility_trust());
        assert!(!h.secure_input_enabled());
        assert!(!h.screen_capture_permission());
        assert!(!h.request_screen_capture_permission());
        assert_eq!(h.bundle_id_for_pid(1), None);
        assert_eq!(h.read_clipboard_text(), None);
        assert_eq!(h.screen_context_text(None, 100), None);
        assert!(h.display_scales().is_empty());
    }

    #[test]
    fn shell_host_is_object_safe_and_send_sync() {
        fn takes(_: Arc<dyn ShellHost>) {}

        takes(Arc::new(BareHost));
    }
}
