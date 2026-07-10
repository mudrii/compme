//! Menu-bar (`NSStatusItem`) tray UI.
//!
//! The tray renders precomputed strings + booleans handed to [`MacosTray::set_status`]
//! — it holds no app policy (that lives in the pure `status` module of the app
//! crate). Menu actions only flip shared [`TrayFlags`] atomics; the run loop
//! observes them. AppKit/objc2 glue: build- and live-verified, not unit-tested.

use std::sync::atomic::Ordering;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::MainThreadMarker;
use objc2::{define_class, sel, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSControlStateValueOff, NSControlStateValueOn, NSImage, NSMenu, NSMenuItem, NSStatusBar,
    NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSData, NSObjectProtocol, NSSize, NSString};
pub use platform::shell::{DisableArm, TrayFlags};
use platform::PlatformError;

/// Public tray actions that cross the AppKit target/action seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    ToggleEnabled,
    OpenAccessibilitySettings,
    Quit,
    DisableGlobal(DisableArm),
    Snooze,
    OpenSettingsWindow,
    CheckUpdates,
    ToggleCollection,
    DisableApp(DisableArm),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrayMenuActionSpec {
    pub title: &'static str,
    pub action: TrayAction,
}

pub fn tray_menu_action_specs() -> &'static [TrayMenuActionSpec] {
    &[TrayMenuActionSpec {
        title: "Check for Updates…",
        action: TrayAction::CheckUpdates,
    }]
}

/// Apply a user-selected tray action to the state shared with the run loop.
///
/// Keeping this mapping declarative makes the target/action shell thin: AppKit
/// selectors only identify an action, while this function owns its observable
/// meaning.
pub fn apply_tray_action(flags: &TrayFlags, action: TrayAction) {
    match action {
        TrayAction::ToggleEnabled => {
            let enabled = &flags.enabled;
            enabled.store(!enabled.load(Ordering::Relaxed), Ordering::Relaxed);
        }
        TrayAction::OpenAccessibilitySettings => {
            flags.open_settings.store(true, Ordering::Relaxed);
        }
        TrayAction::Quit => flags.quit.store(true, Ordering::Relaxed),
        TrayAction::DisableGlobal(arm) => {
            *flags
                .global_disable
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(arm);
        }
        TrayAction::Snooze => flags.snooze_requested.store(true, Ordering::Relaxed),
        TrayAction::OpenSettingsWindow => flags.open_settings_window.store(true, Ordering::Relaxed),
        TrayAction::CheckUpdates => flags.check_updates.store(true, Ordering::Relaxed),
        TrayAction::ToggleCollection => flags.collection_toggle.store(true, Ordering::Relaxed),
        TrayAction::DisableApp(arm) => {
            *flags
                .app_disable
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(arm);
        }
    }
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
            apply_tray_action(&self.ivars().flags, TrayAction::ToggleEnabled);
        }

        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            apply_tray_action(
                &self.ivars().flags,
                TrayAction::OpenAccessibilitySettings,
            );
        }

        #[unsafe(method(requestQuit:))]
        fn request_quit(&self, _sender: Option<&AnyObject>) {
            apply_tray_action(&self.ivars().flags, TrayAction::Quit);
        }

        #[unsafe(method(disableGlobalHour:))]
        fn disable_global_hour(&self, _sender: Option<&AnyObject>) {
            apply_tray_action(
                &self.ivars().flags,
                TrayAction::DisableGlobal(DisableArm::Hour),
            );
        }

        #[unsafe(method(disableGlobalRelaunch:))]
        fn disable_global_relaunch(&self, _sender: Option<&AnyObject>) {
            apply_tray_action(
                &self.ivars().flags,
                TrayAction::DisableGlobal(DisableArm::UntilRelaunch),
            );
        }

        #[unsafe(method(disableGlobalAlways:))]
        fn disable_global_always(&self, _sender: Option<&AnyObject>) {
            apply_tray_action(
                &self.ivars().flags,
                TrayAction::DisableGlobal(DisableArm::Always),
            );
        }

        #[unsafe(method(requestSnooze:))]
        fn request_snooze(&self, _sender: Option<&AnyObject>) {
            apply_tray_action(&self.ivars().flags, TrayAction::Snooze);
        }

        #[unsafe(method(openSettingsWindow:))]
        fn open_settings_window(&self, _sender: Option<&AnyObject>) {
            apply_tray_action(&self.ivars().flags, TrayAction::OpenSettingsWindow);
        }

        #[unsafe(method(checkUpdates:))]
        fn check_updates(&self, _sender: Option<&AnyObject>) {
            apply_tray_action(&self.ivars().flags, TrayAction::CheckUpdates);
        }

        #[unsafe(method(toggleCollection:))]
        fn toggle_collection(&self, _sender: Option<&AnyObject>) {
            apply_tray_action(&self.ivars().flags, TrayAction::ToggleCollection);
        }

        #[unsafe(method(disableAppHour:))]
        fn disable_app_hour(&self, _sender: Option<&AnyObject>) {
            apply_tray_action(
                &self.ivars().flags,
                TrayAction::DisableApp(DisableArm::Hour),
            );
        }

        #[unsafe(method(disableAppRelaunch:))]
        fn disable_app_relaunch(&self, _sender: Option<&AnyObject>) {
            apply_tray_action(
                &self.ivars().flags,
                TrayAction::DisableApp(DisableArm::UntilRelaunch),
            );
        }

        #[unsafe(method(disableAppAlways:))]
        fn disable_app_always(&self, _sender: Option<&AnyObject>) {
            apply_tray_action(
                &self.ivars().flags,
                TrayAction::DisableApp(DisableArm::Always),
            );
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
    stats_line_item: Retained<NSMenuItem>,
    enabled_item: Retained<NSMenuItem>,
    settings_item: Retained<NSMenuItem>,
    button_title_fallback: bool,
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

        // 30-day usage stats line (§11 "words completed"; non-interactive).
        let stats_line_item = NSMenuItem::new(mtm);
        stats_line_item.setTitle(&NSString::from_str("No completions in the last 30 days"));
        stats_line_item.setEnabled(false);
        menu.addItem(&stats_line_item);

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

        // Disable Completions in Current App ▸ (Cotypist-style). Static
        // "Current App" wording for now — the run loop resolves the actual
        // frontmost app on consumption; a dynamic app-name title needs an
        // NSMenuDelegate menuNeedsUpdate hook (future polish).
        let disable_app_item = NSMenuItem::new(mtm);
        disable_app_item.setTitle(&NSString::from_str("Disable Completions in Current App"));
        let disable_menu = NSMenu::new(mtm);
        for (title, sel) in [
            ("For 1 Hour", sel!(disableAppHour:)),
            ("Until Relaunch", sel!(disableAppRelaunch:)),
            ("Always", sel!(disableAppAlways:)),
        ] {
            let item = NSMenuItem::new(mtm);
            item.setTitle(&NSString::from_str(title));
            // SAFETY: as above — target outlives the menu via `_target`.
            unsafe {
                item.setTarget(Some(target_as_any(&target)));
                item.setAction(Some(sel));
            }
            disable_menu.addItem(&item);
        }
        disable_app_item.setSubmenu(Some(&disable_menu));
        menu.addItem(&disable_app_item);

        // Toggle Input Collection in Current App (Cotypist's per-app data-
        // collection control; single toggle item — their stateful submenu is
        // future polish alongside the dynamic app-name titles).
        let collection_item = NSMenuItem::new(mtm);
        collection_item.setTitle(&NSString::from_str(
            "Toggle Input Collection in Current App",
        ));
        // SAFETY: as above.
        unsafe {
            collection_item.setTarget(Some(target_as_any(&target)));
            collection_item.setAction(Some(sel!(toggleCollection:)));
        }
        menu.addItem(&collection_item);

        // Disable Completions Globally ▸ (the global mirror of the per-app
        // submenu; a3 build item 1's missing half, built 2026-06-11).
        let disable_global_item = NSMenuItem::new(mtm);
        disable_global_item.setTitle(&NSString::from_str("Disable Completions Globally"));
        let global_menu = NSMenu::new(mtm);
        for (title, sel) in [
            ("For 1 Hour", sel!(disableGlobalHour:)),
            ("Until Relaunch", sel!(disableGlobalRelaunch:)),
            ("Always", sel!(disableGlobalAlways:)),
        ] {
            let item = NSMenuItem::new(mtm);
            item.setTitle(&NSString::from_str(title));
            // SAFETY: as above — target outlives the menu via `_target`.
            unsafe {
                item.setTarget(Some(target_as_any(&target)));
                item.setAction(Some(sel));
            }
            global_menu.addItem(&item);
        }
        disable_global_item.setSubmenu(Some(&global_menu));
        menu.addItem(&disable_global_item);

        // Snooze (pause suggestions for a fixed hour; run loop applies it).
        let snooze_item = NSMenuItem::new(mtm);
        snooze_item.setTitle(&NSString::from_str("Snooze for 1 hour"));
        // SAFETY: as above.
        unsafe {
            snooze_item.setTarget(Some(target_as_any(&target)));
            snooze_item.setAction(Some(sel!(requestSnooze:)));
        }
        menu.addItem(&snooze_item);

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

        // Settings… (the S2 window; ⌘, equivalent once a key-equivalent is set).
        let settings_window_item = NSMenuItem::new(mtm);
        settings_window_item.setTitle(&NSString::from_str("Settings…"));
        // SAFETY: as above.
        unsafe {
            settings_window_item.setTarget(Some(target_as_any(&target)));
            settings_window_item.setAction(Some(sel!(openSettingsWindow:)));
        }
        // Standard macOS Settings shortcut (⌘, — Command is the default
        // modifier for key equivalents).
        settings_window_item.setKeyEquivalent(&NSString::from_str(","));
        menu.addItem(&settings_window_item);

        // GitHub-release updater surface. The release workflow publishes a
        // machine-readable manifest next to the zip; opening the latest release
        // is the native menu affordance until a Sparkle/appcast client lands.
        let check_updates_spec = tray_menu_action_specs()
            .iter()
            .find(|spec| spec.action == TrayAction::CheckUpdates)
            .expect("check updates tray action is present");
        let check_updates_item = NSMenuItem::new(mtm);
        check_updates_item.setTitle(&NSString::from_str(check_updates_spec.title));
        unsafe {
            check_updates_item.setTarget(Some(target_as_any(&target)));
            check_updates_item.setAction(Some(sel!(checkUpdates:)));
        }
        menu.addItem(&check_updates_item);

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
        let mut button_title_fallback = true;
        if let Some(button) = status_item.button(mtm) {
            // Menu-bar mark: a caret + double chevron ("auto-complete forward").
            // Embedded so it ships with the unbundled binary; a template image
            // so macOS tints it for light/dark menu bars. Falls back to the
            // text title if the PNG ever fails to decode.
            let data = NSData::with_bytes(include_bytes!("../assets/tray-icon.png"));
            match NSImage::initWithData(NSImage::alloc(), &data) {
                Some(image) => {
                    image.setTemplate(true);
                    // 36px bitmap shown at 18pt → crisp 2x on Retina menu bars.
                    image.setSize(NSSize::new(18.0, 18.0));
                    button.setImage(Some(&image));
                    button.setTitle(&NSString::from_str(""));
                    button_title_fallback = false;
                }
                None => button.setTitle(&NSString::from_str("CM\u{2026}")),
            }
        }

        Ok(Self {
            status_item,
            status_line_item,
            stats_line_item,
            enabled_item,
            settings_item,
            button_title_fallback,
            _target: target,
            _menu: menu,
        })
    }

    /// Render the current status: short button title, the menu status line, the
    /// enable checkmark, and whether the settings affordance is shown.
    ///
    /// Returns `Err` if called off the main thread (an AppKit contract violation)
    /// rather than silently no-op'ing, so a future threading regression surfaces.
    pub fn set_status(
        &self,
        title: &str,
        status_line: &str,
        enabled: bool,
        needs_accessibility: bool,
    ) -> Result<(), PlatformError> {
        let mtm = MainThreadMarker::new().ok_or_else(|| PlatformError::CannotComplete {
            reason: "tray set_status must run on the main thread".into(),
        })?;
        if let Some(button) = self.status_item.button(mtm) {
            button.setTitle(&NSString::from_str(tray_button_title(
                self.button_title_fallback,
                title,
            )));
        }
        self.status_line_item
            .setTitle(&NSString::from_str(status_line));
        self.enabled_item.setState(if enabled {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        self.settings_item.setHidden(!needs_accessibility);
        Ok(())
    }

    /// Render the 30-day usage line (a precomputed string — the math lives in
    /// the pure `stats` crate). Same main-thread contract as [`Self::set_status`].
    pub fn set_stats_line(&self, line: &str) -> Result<(), PlatformError> {
        MainThreadMarker::new().ok_or_else(|| PlatformError::CannotComplete {
            reason: "tray set_stats_line must run on the main thread".into(),
        })?;
        self.stats_line_item.setTitle(&NSString::from_str(line));
        Ok(())
    }
}

/// Borrow a `TrayTarget` as a plain `&AnyObject` for `setTarget:`.
fn target_as_any(target: &TrayTarget) -> &AnyObject {
    target.as_ref()
}

fn tray_button_title(button_title_fallback: bool, status_title: &str) -> &str {
    if button_title_fallback {
        status_title
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    fn tray_flags() -> TrayFlags {
        TrayFlags {
            enabled: Arc::new(AtomicBool::new(false)),
            quit: Arc::new(AtomicBool::new(false)),
            open_settings: Arc::new(AtomicBool::new(false)),
            snooze_requested: Arc::new(AtomicBool::new(false)),
            global_disable: Arc::new(Mutex::new(None)),
            open_settings_window: Arc::new(AtomicBool::new(false)),
            check_updates: Arc::new(AtomicBool::new(false)),
            collection_toggle: Arc::new(AtomicBool::new(false)),
            app_disable: Arc::new(Mutex::new(None)),
        }
    }

    #[derive(Debug, Default, PartialEq, Eq)]
    struct TrayState {
        enabled: bool,
        quit: bool,
        open_settings: bool,
        snooze_requested: bool,
        global_disable: Option<DisableArm>,
        open_settings_window: bool,
        check_updates: bool,
        collection_toggle: bool,
        app_disable: Option<DisableArm>,
    }

    fn state(flags: &TrayFlags) -> TrayState {
        TrayState {
            enabled: flags.enabled.load(Ordering::Relaxed),
            quit: flags.quit.load(Ordering::Relaxed),
            open_settings: flags.open_settings.load(Ordering::Relaxed),
            snooze_requested: flags.snooze_requested.load(Ordering::Relaxed),
            global_disable: *flags
                .global_disable
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            open_settings_window: flags.open_settings_window.load(Ordering::Relaxed),
            check_updates: flags.check_updates.load(Ordering::Relaxed),
            collection_toggle: flags.collection_toggle.load(Ordering::Relaxed),
            app_disable: *flags
                .app_disable
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        }
    }

    #[test]
    fn every_tray_action_changes_only_its_public_flag() {
        macro_rules! assert_action {
            ($action:expr, $field:ident = $value:expr) => {{
                let action = $action;
                let flags = tray_flags();
                apply_tray_action(&flags, action);
                assert_eq!(
                    state(&flags),
                    TrayState {
                        $field: $value,
                        ..TrayState::default()
                    },
                    "{action:?}"
                );
            }};
        }

        assert_action!(TrayAction::ToggleEnabled, enabled = true);
        assert_action!(TrayAction::OpenAccessibilitySettings, open_settings = true);
        assert_action!(TrayAction::Quit, quit = true);
        assert_action!(
            TrayAction::DisableGlobal(DisableArm::Hour),
            global_disable = Some(DisableArm::Hour)
        );
        assert_action!(
            TrayAction::DisableGlobal(DisableArm::UntilRelaunch),
            global_disable = Some(DisableArm::UntilRelaunch)
        );
        assert_action!(
            TrayAction::DisableGlobal(DisableArm::Always),
            global_disable = Some(DisableArm::Always)
        );
        assert_action!(TrayAction::Snooze, snooze_requested = true);
        assert_action!(TrayAction::OpenSettingsWindow, open_settings_window = true);
        assert_action!(TrayAction::CheckUpdates, check_updates = true);
        assert_action!(TrayAction::ToggleCollection, collection_toggle = true);
        assert_action!(
            TrayAction::DisableApp(DisableArm::Hour),
            app_disable = Some(DisableArm::Hour)
        );
        assert_action!(
            TrayAction::DisableApp(DisableArm::UntilRelaunch),
            app_disable = Some(DisableArm::UntilRelaunch)
        );
        assert_action!(
            TrayAction::DisableApp(DisableArm::Always),
            app_disable = Some(DisableArm::Always)
        );
    }

    #[test]
    fn tray_button_title_uses_status_text_only_when_icon_is_unavailable() {
        assert_eq!(tray_button_title(true, "CM\u{26a0}"), "CM\u{26a0}");
        assert_eq!(tray_button_title(false, "CM\u{26a0}"), "");
    }

    #[test]
    fn tray_action_specs_include_check_for_updates() {
        assert!(tray_menu_action_specs().contains(&TrayMenuActionSpec {
            title: "Check for Updates…",
            action: TrayAction::CheckUpdates,
        }));
    }
}
