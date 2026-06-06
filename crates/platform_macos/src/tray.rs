//! Menu-bar (`NSStatusItem`) tray UI.
//!
//! The tray renders precomputed strings + booleans handed to [`MacosTray::set_status`]
//! — it holds no app policy (that lives in the pure `status` module of the app
//! crate). Menu actions only flip shared [`TrayFlags`] atomics; the run loop
//! observes them. AppKit/objc2 glue: build- and live-verified, not unit-tested.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::MainThreadMarker;
use objc2::{define_class, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSControlStateValueOff, NSControlStateValueOn, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{NSObjectProtocol, NSString};
use platform::PlatformError;

/// Shared toggles flipped by tray menu actions and observed by the run loop.
#[derive(Clone)]
pub struct TrayFlags {
    /// User enable/disable toggle (suggestions on/off).
    pub enabled: Arc<AtomicBool>,
    /// Set when the user picks Quit.
    pub quit: Arc<AtomicBool>,
    /// Set when the user picks Open Accessibility Settings.
    pub open_settings: Arc<AtomicBool>,
}

#[derive(Clone)]
struct TrayTargetIvars {
    flags: TrayFlags,
}

define_class!(
    // SAFETY: a plain NSObject subclass used only as a menu action target; its
    // methods just flip atomics and never touch Objective-C state unsafely.
    #[unsafe(super = objc2_foundation::NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = TrayTargetIvars]
    struct TrayTarget;

    unsafe impl NSObjectProtocol for TrayTarget {}

    impl TrayTarget {
        // These actions fire on the main thread (run-loop pump), the same thread
        // that reads the flags, so Relaxed ordering is sufficient.
        #[unsafe(method(toggleEnabled:))]
        fn toggle_enabled(&self, _sender: Option<&AnyObject>) {
            let enabled = &self.ivars().flags.enabled;
            let now = enabled.load(Ordering::Relaxed);
            enabled.store(!now, Ordering::Relaxed);
        }

        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            self.ivars().flags.open_settings.store(true, Ordering::Relaxed);
        }

        #[unsafe(method(requestQuit:))]
        fn request_quit(&self, _sender: Option<&AnyObject>) {
            self.ivars().flags.quit.store(true, Ordering::Relaxed);
        }
    }
);

impl TrayTarget {
    fn new(flags: TrayFlags, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TrayTargetIvars { flags });
        // SAFETY: NSObject's init signature is correct for this subclass.
        unsafe { objc2::msg_send![super(this), init] }
    }
}

/// The menu-bar status item and its menu.
pub struct MacosTray {
    status_item: Retained<NSStatusItem>,
    status_line_item: Retained<NSMenuItem>,
    enabled_item: Retained<NSMenuItem>,
    settings_item: Retained<NSMenuItem>,
    // The menu item's `target` is a weak reference; keep the target alive here.
    _target: Retained<TrayTarget>,
    _menu: Retained<NSMenu>,
}

impl MacosTray {
    /// Create the status item and menu on the main thread.
    pub fn new(flags: TrayFlags) -> Result<Self, PlatformError> {
        let mtm = MainThreadMarker::new().ok_or_else(|| PlatformError::CannotComplete {
            reason: "tray must be created on the main thread".into(),
        })?;

        let target = TrayTarget::new(flags, mtm);
        let bar = NSStatusBar::systemStatusBar();
        let status_item = bar.statusItemWithLength(NSVariableStatusItemLength);
        let menu = NSMenu::new(mtm);

        // Status line (non-interactive).
        let status_line_item = NSMenuItem::new(mtm);
        status_line_item.setTitle(&NSString::from_str("Ready"));
        status_line_item.setEnabled(false);
        menu.addItem(&status_line_item);
        menu.addItem(&NSMenuItem::separatorItem(mtm));

        // Enable/disable toggle.
        let enabled_item = NSMenuItem::new(mtm);
        enabled_item.setTitle(&NSString::from_str("Enabled"));
        // SAFETY: target outlives the menu (held in `_target`); selector exists.
        unsafe {
            enabled_item.setTarget(Some(target_as_any(&target)));
            enabled_item.setAction(Some(sel!(toggleEnabled:)));
        }
        menu.addItem(&enabled_item);

        // Open Accessibility Settings (hidden unless blocked on permission).
        let settings_item = NSMenuItem::new(mtm);
        settings_item.setTitle(&NSString::from_str("Open Accessibility Settings…"));
        settings_item.setHidden(true);
        // SAFETY: as above.
        unsafe {
            settings_item.setTarget(Some(target_as_any(&target)));
            settings_item.setAction(Some(sel!(openSettings:)));
        }
        menu.addItem(&settings_item);

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        // Quit (routes through the run loop's ordered teardown via the flag).
        let quit_item = NSMenuItem::new(mtm);
        quit_item.setTitle(&NSString::from_str("Quit"));
        // SAFETY: as above.
        unsafe {
            quit_item.setTarget(Some(target_as_any(&target)));
            quit_item.setAction(Some(sel!(requestQuit:)));
        }
        menu.addItem(&quit_item);

        status_item.setMenu(Some(&menu));
        if let Some(button) = status_item.button(mtm) {
            button.setTitle(&NSString::from_str("CM…"));
        }

        Ok(Self {
            status_item,
            status_line_item,
            enabled_item,
            settings_item,
            _target: target,
            _menu: menu,
        })
    }

    /// Render the current status: short button title, the menu status line, the
    /// enable checkmark, and whether the settings affordance is shown.
    pub fn set_status(
        &self,
        title: &str,
        status_line: &str,
        enabled: bool,
        needs_accessibility: bool,
    ) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if let Some(button) = self.status_item.button(mtm) {
            button.setTitle(&NSString::from_str(title));
        }
        self.status_line_item
            .setTitle(&NSString::from_str(status_line));
        self.enabled_item.setState(if enabled {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        self.settings_item.setHidden(!needs_accessibility);
    }
}

/// Borrow a `TrayTarget` as a plain `&AnyObject` for `setTarget:`.
fn target_as_any(target: &TrayTarget) -> &AnyObject {
    target.as_ref()
}
