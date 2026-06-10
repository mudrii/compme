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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSControlStateValueOn,
    NSFont, NSSwitch, NSTextField, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};
use platform::PlatformError;

/// Settings-pane toggles, flipped by controls on the main thread and observed
/// by the run loop (the tray-flags pattern: render-only UI, policy outside).
#[derive(Clone)]
pub struct SettingsFlags {
    /// Labs: global mid-line completions (`COMPME_MIDLINE`). The run loop
    /// watches edges, persists, and re-applies the engine gate live.
    pub labs_midline: Arc<AtomicBool>,
    /// Statistics rows, composed by the run loop (`stats_pane_lines`) right
    /// before each show; the window only renders them (one label per line).
    pub stats_lines: Arc<Mutex<Vec<String>>>,
    /// About text (version/license/no-telemetry/repo/credits), composed once
    /// at startup — static for the process lifetime, rendered verbatim.
    pub about_text: String,
}

struct SettingsTargetIvars {
    flags: SettingsFlags,
}

define_class!(
    // SAFETY: a plain NSObject subclass used only as a control action target;
    // its methods read control state and flip atomics.
    #[unsafe(super = objc2_foundation::NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = SettingsTargetIvars]
    struct SettingsTarget;

    unsafe impl NSObjectProtocol for SettingsTarget {}

    impl SettingsTarget {
        #[unsafe(method(toggleMidline:))]
        fn toggle_midline(&self, sender: Option<&NSSwitch>) {
            if let Some(switch) = sender {
                let on = switch.state() == NSControlStateValueOn;
                self.ivars().flags.labs_midline.store(on, Ordering::Relaxed);
            }
        }
    }
);

impl SettingsTarget {
    fn new(flags: SettingsFlags, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SettingsTargetIvars { flags });
        // SAFETY: NSObject's init signature is correct for this subclass.
        unsafe { objc2::msg_send![super(this), init] }
    }
}

/// Whether a visibility transition requires demoting the activation policy
/// back to `Accessory` (pure: the run loop feeds it the polled states).
pub fn policy_restore_needed(was_visible: bool, visible_now: bool) -> bool {
    was_visible && !visible_now
}

pub struct MacosSettingsWindow {
    window: Option<Retained<NSWindow>>,
    flags: SettingsFlags,
    // Keep the action target alive for the window's lifetime.
    target: Option<Retained<SettingsTarget>>,
    // Statistics row labels, refreshed from `flags.stats_lines` on every show
    // (the window is built once; data rows must not go stale on reopen).
    stats_labels: Vec<Retained<NSTextField>>,
}

impl MacosSettingsWindow {
    pub fn new(flags: SettingsFlags) -> Self {
        // Lazy: the NSWindow is created on first show (main thread).
        Self {
            window: None,
            flags,
            target: None,
            stats_labels: Vec::new(),
        }
    }

    /// Show the window (creating it on first use) and promote the activation
    /// policy so it can become key. Main-thread only.
    pub fn show(&mut self) -> Result<(), PlatformError> {
        let mtm = main_thread()?;
        if self.window.is_none() {
            let target = SettingsTarget::new(self.flags.clone(), mtm);
            let (window, stats_labels) = build_window(mtm, &target, &self.flags);
            self.window = Some(window);
            self.stats_labels = stats_labels;
            self.target = Some(target);
        }
        // Refresh data rows on EVERY show — the lazily built window is reused
        // across opens, so stale strings would otherwise survive a reopen.
        if let Ok(lines) = self.flags.stats_lines.lock() {
            for (label, line) in self.stats_labels.iter().zip(lines.iter()) {
                label.setStringValue(&NSString::from_str(line));
            }
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

fn main_thread() -> Result<MainThreadMarker, PlatformError> {
    MainThreadMarker::new().ok_or_else(|| PlatformError::CannotComplete {
        reason: "settings window requires the main thread".into(),
    })
}

fn build_window(
    mtm: MainThreadMarker,
    target: &Retained<SettingsTarget>,
    flags: &SettingsFlags,
) -> (Retained<NSWindow>, Vec<Retained<NSTextField>>) {
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
    let mut stats_labels: Vec<Retained<NSTextField>> = Vec::new();

    // Labs pane (the first functional pane): one labeled NSSwitch for the
    // global mid-line toggle, initialized from the CURRENT config state.
    if let Some(content) = window.contentView() {
        let header = NSTextField::labelWithString(&NSString::from_str("Labs"), mtm);
        header.setFrame(NSRect::new(
            NSPoint::new(20.0, 370.0),
            NSSize::new(200.0, 24.0),
        ));
        content.addSubview(&header);

        let label = NSTextField::labelWithString(
            &NSString::from_str("Mid-line completions (show even with text after the cursor)"),
            mtm,
        );
        label.setFrame(NSRect::new(
            NSPoint::new(20.0, 340.0),
            NSSize::new(400.0, 20.0),
        ));
        content.addSubview(&label);

        let switch = NSSwitch::new(mtm);
        switch.setFrame(NSRect::new(
            NSPoint::new(430.0, 336.0),
            NSSize::new(60.0, 26.0),
        ));
        switch.setState(if flags.labs_midline.load(Ordering::Relaxed) {
            objc2_app_kit::NSControlStateValueOn
        } else {
            objc2_app_kit::NSControlStateValueOff
        });
        // SAFETY: target outlives the window (held by MacosSettingsWindow).
        unsafe {
            switch.setTarget(Some({
                let any: &AnyObject = target.as_ref();
                any
            }));
            switch.setAction(Some(sel!(toggleMidline:)));
        }
        content.addSubview(&switch);

        // Statistics section: header + three data rows (shown/accepted/words).
        // Row strings come from the run loop via flags.stats_lines; show()
        // refreshes them on every open. Monospaced font keeps the fixed-width
        // labels and sparkline glyphs column-aligned.
        let stats_header = NSTextField::labelWithString(
            &NSString::from_str("Statistics \u{2014} this session"),
            mtm,
        );
        stats_header.setFrame(NSRect::new(
            NSPoint::new(20.0, 290.0),
            NSSize::new(200.0, 24.0),
        ));
        content.addSubview(&stats_header);

        let initial: Vec<String> = flags
            .stats_lines
            .lock()
            .map(|l| l.clone())
            .unwrap_or_default();
        // SAFETY: NSFontWeightRegular is a constant extern static.
        let mono = NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe {
            objc2_app_kit::NSFontWeightRegular
        });
        for row in 0..STATS_ROWS {
            let text = initial.get(row).map(String::as_str).unwrap_or("");
            let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
            label.setFont(Some(&mono));
            label.setFrame(NSRect::new(
                NSPoint::new(20.0, 262.0 - row as f64 * 24.0),
                NSSize::new(420.0, 20.0),
            ));
            content.addSubview(&label);
            stats_labels.push(label);
        }

        // About section: static for the process lifetime, so build-once is
        // fine here (unlike the Statistics rows above).
        let about_header = NSTextField::labelWithString(&NSString::from_str("About"), mtm);
        about_header.setFrame(NSRect::new(
            NSPoint::new(20.0, 168.0),
            NSSize::new(200.0, 24.0),
        ));
        content.addSubview(&about_header);
        let about =
            NSTextField::wrappingLabelWithString(&NSString::from_str(&flags.about_text), mtm);
        about.setFrame(NSRect::new(
            NSPoint::new(20.0, 28.0),
            NSSize::new(480.0, 136.0),
        ));
        // Display-only: wrappingLabelWithString is selectable by default,
        // which is fine (lets the user copy the repo URL), but it must not
        // be editable. Small font: the credits line wraps to ~3 lines; at
        // the default size the block runs flush against its 136px box
        // (review-c101), at 11pt it fits with headroom.
        about.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        about.setEditable(false);
        content.addSubview(&about);
    }
    // Keep the instance alive across closes: AppKit's default releases a
    // window on close, which would dangle our Retained pointer.
    // SAFETY: documented NSWindow property setter.
    unsafe { window.setReleasedWhenClosed(false) };
    (window, stats_labels)
}

/// Fixed Statistics row count (shown / accepted / words / lifetime).
const STATS_ROWS: usize = 4;

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
