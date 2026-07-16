//! `ShellHost` for macOS.
//!
//! Every method is a thin wrapper over the free functions this crate already
//! ships. This adds the cross-platform seam without changing macOS behavior.

use std::path::Path;
use std::time::Duration;

use objc2_app_kit::{NSSpellChecker, NSWorkspace};
use objc2_foundation::{NSString, NSURL};
use platform::shell::{ConfirmPrompt, ShellHost, TrayHandle};
use platform::{PlatformError, ScreenRect};

#[derive(Debug, Default)]
pub struct MacosShellHost;

impl MacosShellHost {
    pub fn new() -> Self {
        Self
    }
}

impl ShellHost for MacosShellHost {
    fn pump_events(&self, heartbeat: Duration) {
        crate::pump_app_events();
        let mode = unsafe { core_foundation::runloop::kCFRunLoopDefaultMode };
        core_foundation::runloop::CFRunLoop::run_in_mode(mode, heartbeat, false);
    }

    fn accessibility_trusted(&self) -> bool {
        crate::accessibility_trusted()
    }

    fn prompt_accessibility_trust(&self) -> bool {
        crate::prompt_accessibility_trust()
    }

    fn secure_input_enabled(&self) -> bool {
        crate::secure_input_enabled()
    }

    fn screen_capture_permission(&self) -> bool {
        crate::screen_recording_permission()
    }

    fn request_screen_capture_permission(&self) -> bool {
        crate::request_screen_recording_permission()
    }

    fn physical_memory_bytes(&self) -> u64 {
        crate::physical_memory_bytes()
    }

    fn bundle_id_for_pid(&self, pid: i32) -> Option<String> {
        crate::bundle_id_for_pid(pid)
    }

    fn read_clipboard_text(&self) -> Option<String> {
        crate::read_pasteboard_text()
    }

    fn spelling_correction(&self, word: &str) -> Result<Option<String>, PlatformError> {
        if word.is_empty() || word.chars().count() > 128 {
            return Ok(None);
        }

        let checker = NSSpellChecker::sharedSpellChecker();
        let string = NSString::from_str(word);
        let misspelled = unsafe {
            checker.checkSpellingOfString_startingAt_language_wrap_inSpellDocumentWithTag_wordCount(
                &string,
                0,
                None,
                false,
                0,
                std::ptr::null_mut(),
            )
        };
        if misspelled.location != 0 || misspelled.length != string.len_utf16() {
            return Ok(None);
        }

        let language = checker.language();
        Ok(checker
            .correctionForWordRange_inString_language_inSpellDocumentWithTag(
                misspelled, &string, &language, 0,
            )
            .map(|correction| correction.to_string())
            .filter(|correction| !correction.is_empty() && correction != word))
    }

    fn screen_context_text(
        &self,
        caret_rect: Option<ScreenRect>,
        max_chars: usize,
    ) -> Option<String> {
        crate::screen_context_text(caret_rect, max_chars)
    }

    fn display_scales(&self) -> Vec<(ScreenRect, f64)> {
        crate::display_scales()
    }

    fn open_url(&self, url: &str) -> Result<(), PlatformError> {
        let url = NSURL::URLWithString(&NSString::from_str(url)).ok_or_else(|| {
            PlatformError::CannotComplete {
                reason: format!("invalid URL {url:?}"),
            }
        })?;
        if NSWorkspace::sharedWorkspace().openURL(&url) {
            Ok(())
        } else {
            Err(PlatformError::CannotComplete {
                reason: format!("failed to open {url:?}"),
            })
        }
    }

    fn open_permission_settings(&self) -> Result<(), PlatformError> {
        self.open_url(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        )
    }

    fn reveal_file(&self, path: &Path) -> Result<(), PlatformError> {
        crate::reveal_file_in_finder(path)
    }

    fn set_launch_at_login(&self, enabled: bool) -> Result<(), PlatformError> {
        crate::set_launch_at_login(enabled)
    }

    fn confirm(&self, prompt: &ConfirmPrompt<'_>) -> Result<bool, PlatformError> {
        crate::ui_prompt::confirm_prompt(prompt.title, prompt.message, prompt.confirm_label)
    }

    fn load_or_create_memory_key(&self) -> Result<[u8; 32], PlatformError> {
        crate::keychain::KeychainKeyStore::new().load_or_create_memory_key()
    }
}

impl TrayHandle for crate::MacosTray {
    fn set_status(
        &self,
        title: &str,
        status_line: &str,
        enabled: bool,
        needs_accessibility: bool,
    ) -> Result<(), PlatformError> {
        crate::MacosTray::set_status(self, title, status_line, enabled, needs_accessibility)
    }

    fn set_stats_line(&self, line: &str) -> Result<(), PlatformError> {
        crate::MacosTray::set_stats_line(self, line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::shell::ShellHost;

    #[test]
    fn physical_memory_is_nonzero() {
        assert!(MacosShellHost::new().physical_memory_bytes() > 0);
    }

    #[test]
    fn pump_events_returns_within_heartbeat_scale() {
        let start = std::time::Instant::now();
        MacosShellHost::new().pump_events(Duration::from_millis(10));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
