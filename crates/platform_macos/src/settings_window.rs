//! The S2 settings window shell (A3 settings plan, tick 1: skeleton).
//!
//! Same contract as the tray: render-only AppKit glue, no policy. The run
//! loop opens it via a tray flag and polls visibility each heartbeat so the
//! activation-policy dance stays correct however the window closes.
//!
//! LSUIElement apps (our Info.plist) run as `Accessory`: a window shown
//! without promoting the activation policy to `Regular` never becomes key.
//! `set_visible(true)` promotes; the visibility POLL (not a window delegate)
//! detects any close — red button included — and demotes back to
//! `Accessory`, so no Dock icon is left stranded. AppKit/FFI glue: build-
//! and live-verified, not unit-tested (tray convention); the policy-edge
//! decision is the unit-tested pure part.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use platform::PlatformError;

/// Whether a visibility transition requires demoting the activation policy
/// back to `Accessory` (pure: the run loop feeds it the polled states).
pub fn policy_restore_needed(was_visible: bool, visible_now: bool) -> bool {
    was_visible && !visible_now
}

pub struct MacosSettingsWindow {
    window: Option<Retained<NSWindow>>,
}

impl MacosSettingsWindow {
    pub fn new() -> Self {
        // Lazy: the NSWindow is created on first show (main thread).
        Self { window: None }
    }

    /// Show the window (creating it on first use) and promote the activation
    /// policy so it can become key. Main-thread only.
    pub fn show(&mut self) -> Result<(), PlatformError> {
        let mtm = main_thread()?;
        if self.window.is_none() {
            self.window = Some(build_window(mtm));
        }
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        if let Some(window) = &self.window {
            window.makeKeyAndOrderFront(None);
        }
        app.activate();
        Ok(())
    }

    /// Whether the window is visible to the app — TRUE while miniaturized
    /// (AppKit `isVisible` semantics). That is deliberate for the policy
    /// dance: a minimized window needs the Dock (its tile is the restore
    /// path), so the activation policy must stay `Regular` until the window
    /// actually closes. Main-thread only.
    pub fn is_visible(&self) -> bool {
        self.window.as_ref().is_some_and(|w| w.isVisible())
    }

    /// Demote the activation policy back to `Accessory` (after the window
    /// closed — however it closed). Main-thread only.
    pub fn restore_accessory_policy(&self) -> Result<(), PlatformError> {
        let mtm = main_thread()?;
        NSApplication::sharedApplication(mtm)
            .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        Ok(())
    }
}

impl Default for MacosSettingsWindow {
    fn default() -> Self {
        Self::new()
    }
}

fn main_thread() -> Result<MainThreadMarker, PlatformError> {
    MainThreadMarker::new().ok_or_else(|| PlatformError::CannotComplete {
        reason: "settings window requires the main thread".into(),
    })
}

fn build_window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let frame = NSRect::new(NSPoint::new(200.0, 200.0), NSSize::new(520.0, 420.0));
    let style =
        NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
    // SAFETY: standard NSWindow init; releasedWhenClosed defaults are managed
    // by the Retained wrapper (we keep ownership and hide instead of free).
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str("Compme Settings"));
    window.center();
    // Keep the instance alive across closes: AppKit's default releases a
    // window on close, which would dangle our Retained pointer.
    // SAFETY: documented NSWindow property setter.
    unsafe { window.setReleasedWhenClosed(false) };
    window
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_restores_only_on_the_visible_to_hidden_edge() {
        assert!(policy_restore_needed(true, false), "close edge demotes");
        assert!(
            !policy_restore_needed(true, true),
            "still open: keep Regular"
        );
        assert!(!policy_restore_needed(false, false), "never opened: no-op");
        assert!(
            !policy_restore_needed(false, true),
            "open edge is show()'s job"
        );
    }
}
