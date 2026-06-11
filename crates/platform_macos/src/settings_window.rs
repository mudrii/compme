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
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSButton,
    NSControlStateValueOn, NSFont, NSSwitch, NSTabView, NSTabViewItem, NSTextField, NSView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};
use platform::PlatformError;

/// Settings-pane toggles, flipped by controls on the main thread and observed
/// by the run loop (the tray-flags pattern: render-only UI, policy outside).
#[derive(Clone)]
pub struct SettingsFlags {
    /// Master enabled flag — THE SAME Arc as TrayFlags.enabled (one atomic,
    /// two views: tray checkmark + this switch). The run loop's existing
    /// enabled-edge handles persist + ghost dismiss; tray and SIGUSR1 also
    /// write it, which is why switches refresh on every show.
    pub general_enabled: Arc<AtomicBool>,
    /// Labs: global mid-line completions (`COMPME_MIDLINE`). The run loop
    /// watches edges, persists, and re-applies the engine gate live.
    pub labs_midline: Arc<AtomicBool>,
    /// General: global typo autocorrect (`COMPME_AUTOCORRECT`). Same watcher
    /// pattern: the run loop persists and applies on the edge.
    pub general_autocorrect: Arc<AtomicBool>,
    /// General: trailing space after single-word accepts
    /// (`COMPME_TRAILING_SPACE`). Same watcher pattern.
    pub general_trailing_space: Arc<AtomicBool>,
    /// Statistics rows, composed by the run loop (`stats_pane_lines`) right
    /// before each show; the window only renders them (one label per line).
    pub stats_lines: Arc<Mutex<Vec<String>>>,
    /// About text (version/license/no-telemetry/repo/credits), composed once
    /// at startup — static for the process lifetime, rendered verbatim.
    pub about_text: String,
    /// Setup rows (permission/model readiness), composed by the run loop
    /// right before each show; one label per line, refreshed like stats.
    pub setup_lines: Arc<Mutex<Vec<String>>>,
    /// Setup buttons (tray-flags pattern): the button stores true, the run
    /// loop consumes the edge and performs the privileged call on its side.
    pub setup_grant_ax: Arc<AtomicBool>,
    pub setup_request_screen: Arc<AtomicBool>,
    pub setup_reveal_model: Arc<AtomicBool>,
    /// Setup "Download Recommended Model" — the run loop spawns the worker
    /// and logs progress (picker UI is a later slice).
    pub setup_download_model: Arc<AtomicBool>,
    /// Apps rows (per-app recorded-input counts), composed by the run loop
    /// right before each show; refreshed like stats.
    pub apps_lines: Arc<Mutex<Vec<String>>>,
    /// A clicked Apps-row Delete button: the ROW INDEX (the run loop resolves
    /// it to an app id via apps_row_ids and performs the delete).
    pub apps_delete_row: Arc<Mutex<Option<usize>>>,
    /// Shortcuts text (current bindings + how to change them), composed once
    /// at startup — static for the process lifetime like `about_text`
    /// (rebinding applies at relaunch until the live-rebind refactor).
    pub shortcuts_text: String,
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
        #[unsafe(method(grantAccessibility:))]
        fn grant_accessibility(&self, _sender: Option<&NSButton>) {
            self.ivars().flags.setup_grant_ax.store(true, Ordering::Relaxed);
        }

        #[unsafe(method(requestScreenRecording:))]
        fn request_screen_recording(&self, _sender: Option<&NSButton>) {
            self.ivars()
                .flags
                .setup_request_screen
                .store(true, Ordering::Relaxed);
        }

        #[unsafe(method(revealModel:))]
        fn reveal_model(&self, _sender: Option<&NSButton>) {
            self.ivars()
                .flags
                .setup_reveal_model
                .store(true, Ordering::Relaxed);
        }

        #[unsafe(method(downloadModel:))]
        fn download_model(&self, _sender: Option<&NSButton>) {
            self.ivars()
                .flags
                .setup_download_model
                .store(true, Ordering::Relaxed);
        }

        #[unsafe(method(deleteAppRow:))]
        fn delete_app_row(&self, sender: Option<&NSButton>) {
            if let Some(button) = sender {
                let row = button.tag().max(0) as usize;
                if let Ok(mut slot) = self.ivars().flags.apps_delete_row.lock() {
                    *slot = Some(row);
                }
            }
        }

        #[unsafe(method(toggleEnabled:))]
        fn toggle_enabled(&self, sender: Option<&NSSwitch>) {
            if let Some(switch) = sender {
                let on = switch.state() == NSControlStateValueOn;
                self.ivars().flags.general_enabled.store(on, Ordering::Relaxed);
            }
        }

        #[unsafe(method(toggleTrailingSpace:))]
        fn toggle_trailing_space(&self, sender: Option<&NSSwitch>) {
            if let Some(switch) = sender {
                let on = switch.state() == NSControlStateValueOn;
                self.ivars()
                    .flags
                    .general_trailing_space
                    .store(on, Ordering::Relaxed);
            }
        }

        #[unsafe(method(toggleAutocorrect:))]
        fn toggle_autocorrect(&self, sender: Option<&NSSwitch>) {
            if let Some(switch) = sender {
                let on = switch.state() == NSControlStateValueOn;
                self.ivars()
                    .flags
                    .general_autocorrect
                    .store(on, Ordering::Relaxed);
            }
        }

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
    // Setup row labels, refreshed from `flags.setup_lines` the same way.
    setup_labels: Vec<Retained<NSTextField>>,
    // Apps row labels, refreshed from `flags.apps_lines` the same way.
    apps_labels: Vec<Retained<NSTextField>>,
    // General-tab switches, refreshed from their atomics on every show:
    // enabled has EXTERNAL writers (tray, SIGUSR1), so its rendered state
    // can go stale while the window is closed (c95 staleness class). The
    // others refresh too — harmless and uniform.
    switches: Vec<(Retained<NSSwitch>, Arc<AtomicBool>)>,
}

impl MacosSettingsWindow {
    pub fn new(flags: SettingsFlags) -> Self {
        // Lazy: the NSWindow is created on first show (main thread).
        Self {
            window: None,
            flags,
            target: None,
            stats_labels: Vec::new(),
            setup_labels: Vec::new(),
            apps_labels: Vec::new(),
            switches: Vec::new(),
        }
    }

    /// Show the window (creating it on first use) and promote the activation
    /// policy so it can become key. Main-thread only.
    pub fn show(&mut self) -> Result<(), PlatformError> {
        let mtm = main_thread()?;
        if self.window.is_none() {
            let target = SettingsTarget::new(self.flags.clone(), mtm);
            let built = build_window(mtm, &target, &self.flags);
            self.window = Some(built.window);
            self.stats_labels = built.stats_labels;
            self.setup_labels = built.setup_labels;
            self.apps_labels = built.apps_labels;
            self.switches = built.switches;
            self.target = Some(target);
        }
        // Refresh data rows on EVERY show — the lazily built window is reused
        // across opens, so stale strings would otherwise survive a reopen.
        if let Ok(lines) = self.flags.stats_lines.lock() {
            for (label, line) in self.stats_labels.iter().zip(lines.iter()) {
                label.setStringValue(&NSString::from_str(line));
            }
        }
        if let Ok(lines) = self.flags.setup_lines.lock() {
            for (label, line) in self.setup_labels.iter().zip(lines.iter()) {
                label.setStringValue(&NSString::from_str(line));
            }
        }
        if let Ok(lines) = self.flags.apps_lines.lock() {
            for (label, line) in self.apps_labels.iter().zip(lines.iter()) {
                label.setStringValue(&NSString::from_str(line));
            }
        }
        // Switches re-sync from their atomics — enabled can be flipped by
        // the tray or SIGUSR1 while this window is closed.
        for (switch, atomic) in &self.switches {
            switch.setState(if atomic.load(Ordering::Relaxed) {
                NSControlStateValueOn
            } else {
                objc2_app_kit::NSControlStateValueOff
            });
        }
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        if let Some(window) = &self.window {
            window.makeKeyAndOrderFront(None);
        }
        app.activate();
        Ok(())
    }

    /// Re-render the Setup rows from `flags.setup_lines` while the window
    /// stays open (the visible-only poll edge; show() covers the open edge).
    pub fn refresh_setup_labels(&self) {
        if let Ok(lines) = self.flags.setup_lines.lock() {
            for (label, line) in self.setup_labels.iter().zip(lines.iter()) {
                label.setStringValue(&NSString::from_str(line));
            }
        }
    }

    /// Re-render the Apps rows from `flags.apps_lines` after a delete (the
    /// run loop recomposes, then calls this; show() covers the open edge).
    pub fn refresh_apps_labels(&self) {
        if let Ok(lines) = self.flags.apps_lines.lock() {
            for (label, line) in self.apps_labels.iter().zip(lines.iter()) {
                label.setStringValue(&NSString::from_str(line));
            }
        }
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
) -> BuiltWindow {
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
    let mut setup_labels: Vec<Retained<NSTextField>> = Vec::new();
    let mut apps_labels: Vec<Retained<NSTextField>> = Vec::new();
    let mut switches: Vec<(Retained<NSSwitch>, Arc<AtomicBool>)> = Vec::new();

    // Tab layout (c105): one NSTabView as the content view, one tab per
    // pane_titles() entry. Tab content is ~500x350; per-pane coordinates are
    // local to their tab view, so panes never collide with each other again
    // (the c103/c104 overlap class is structurally gone).
    let tabs = NSTabView::new(mtm);
    let pane_views: Vec<Retained<NSView>> = pane_titles()
        .iter()
        .map(|title| {
            let view = NSView::new(mtm);
            let item = NSTabViewItem::new();
            item.setLabel(&NSString::from_str(title));
            item.setView(Some(&view));
            tabs.addTabViewItem(&item);
            view
        })
        .collect();

    // Setup tab: readiness rows (permissions, model file). Strings come
    // from the run loop via flags.setup_lines; show() refreshes them on
    // every open. Grant/Request/Reveal buttons are the next slice.
    {
        let setup = &pane_views[0];
        let header = NSTextField::labelWithString(&NSString::from_str("Setup checklist"), mtm);
        header.setFrame(NSRect::new(
            NSPoint::new(20.0, 300.0),
            NSSize::new(300.0, 24.0),
        ));
        setup.addSubview(&header);
        let initial: Vec<String> = flags
            .setup_lines
            .lock()
            .map(|l| l.clone())
            .unwrap_or_default();
        for row in 0..SETUP_ROWS {
            let text = initial.get(row).map(String::as_str).unwrap_or("");
            let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
            label.setFrame(NSRect::new(
                NSPoint::new(20.0, 270.0 - row as f64 * 26.0),
                NSSize::new(440.0, 20.0),
            ));
            setup.addSubview(&label);
            setup_labels.push(label);
        }

        // Action buttons (always present; each is a harmless no-op when its
        // item is already ready). The privileged calls happen in the run
        // loop — buttons only set flags.
        let buttons: [(&str, objc2::runtime::Sel); 4] = [
            ("Grant Accessibility\u{2026}", sel!(grantAccessibility:)),
            (
                "Request Screen Recording\u{2026}",
                sel!(requestScreenRecording:),
            ),
            ("Reveal Model in Finder", sel!(revealModel:)),
            ("Download Recommended Model", sel!(downloadModel:)),
        ];
        for (i, (title, action)) in buttons.into_iter().enumerate() {
            // SAFETY: target outlives the window (held by MacosSettingsWindow).
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str(title),
                    Some({
                        let any: &AnyObject = target.as_ref();
                        any
                    }),
                    Some(action),
                    mtm,
                )
            };
            button.setFrame(NSRect::new(
                NSPoint::new(20.0, 150.0 - i as f64 * 36.0),
                NSSize::new(230.0, 28.0),
            ));
            setup.addSubview(&button);
        }
    }

    // General tab: the Labs switch (global mid-line toggle), initialized
    // from the CURRENT config state.
    {
        let general = &pane_views[1];

        // Enabled row (top): two views of one atomic — see SettingsFlags.
        let en_label = NSTextField::labelWithString(&NSString::from_str("Enable completions"), mtm);
        en_label.setFrame(NSRect::new(
            NSPoint::new(20.0, 340.0),
            NSSize::new(400.0, 20.0),
        ));
        general.addSubview(&en_label);
        let en_switch = NSSwitch::new(mtm);
        en_switch.setFrame(NSRect::new(
            NSPoint::new(420.0, 336.0),
            NSSize::new(60.0, 26.0),
        ));
        en_switch.setState(if flags.general_enabled.load(Ordering::Relaxed) {
            objc2_app_kit::NSControlStateValueOn
        } else {
            objc2_app_kit::NSControlStateValueOff
        });
        // SAFETY: target outlives the window (held by MacosSettingsWindow).
        unsafe {
            en_switch.setTarget(Some({
                let any: &AnyObject = target.as_ref();
                any
            }));
            en_switch.setAction(Some(sel!(toggleEnabled:)));
        }
        general.addSubview(&en_switch);
        switches.push((en_switch, Arc::clone(&flags.general_enabled)));

        let label = NSTextField::labelWithString(
            &NSString::from_str("Mid-line completions (show even with text after the cursor)"),
            mtm,
        );
        label.setFrame(NSRect::new(
            NSPoint::new(20.0, 300.0),
            NSSize::new(400.0, 20.0),
        ));
        general.addSubview(&label);

        let switch = NSSwitch::new(mtm);
        switch.setFrame(NSRect::new(
            NSPoint::new(420.0, 296.0),
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
        general.addSubview(&switch);
        switches.push((switch, Arc::clone(&flags.labs_midline)));

        // Autocorrect row, same switch pattern one row below.
        let ac_label = NSTextField::labelWithString(
            &NSString::from_str("Autocorrect typos (offer the fix as you type)"),
            mtm,
        );
        ac_label.setFrame(NSRect::new(
            NSPoint::new(20.0, 260.0),
            NSSize::new(400.0, 20.0),
        ));
        general.addSubview(&ac_label);
        let ac_switch = NSSwitch::new(mtm);
        ac_switch.setFrame(NSRect::new(
            NSPoint::new(420.0, 256.0),
            NSSize::new(60.0, 26.0),
        ));
        ac_switch.setState(if flags.general_autocorrect.load(Ordering::Relaxed) {
            objc2_app_kit::NSControlStateValueOn
        } else {
            objc2_app_kit::NSControlStateValueOff
        });
        // SAFETY: target outlives the window (held by MacosSettingsWindow).
        unsafe {
            ac_switch.setTarget(Some({
                let any: &AnyObject = target.as_ref();
                any
            }));
            ac_switch.setAction(Some(sel!(toggleAutocorrect:)));
        }
        general.addSubview(&ac_switch);
        switches.push((ac_switch, Arc::clone(&flags.general_autocorrect)));

        // Trailing-space row, third in the stack.
        let ts_label = NSTextField::labelWithString(
            &NSString::from_str("Trailing space after single-word completions"),
            mtm,
        );
        ts_label.setFrame(NSRect::new(
            NSPoint::new(20.0, 220.0),
            NSSize::new(400.0, 20.0),
        ));
        general.addSubview(&ts_label);
        let ts_switch = NSSwitch::new(mtm);
        ts_switch.setFrame(NSRect::new(
            NSPoint::new(420.0, 216.0),
            NSSize::new(60.0, 26.0),
        ));
        ts_switch.setState(if flags.general_trailing_space.load(Ordering::Relaxed) {
            objc2_app_kit::NSControlStateValueOn
        } else {
            objc2_app_kit::NSControlStateValueOff
        });
        // SAFETY: target outlives the window (held by MacosSettingsWindow).
        unsafe {
            ts_switch.setTarget(Some({
                let any: &AnyObject = target.as_ref();
                any
            }));
            ts_switch.setAction(Some(sel!(toggleTrailingSpace:)));
        }
        general.addSubview(&ts_switch);
        switches.push((ts_switch, Arc::clone(&flags.general_trailing_space)));
    }

    // Apps tab: per-app recorded-input counts (encrypted memory store).
    // Strings come from the run loop via flags.apps_lines; refreshed on
    // every show like the other data tabs.
    {
        let apps = &pane_views[2];
        let header =
            NSTextField::labelWithString(&NSString::from_str("Recorded inputs by app"), mtm);
        header.setFrame(NSRect::new(
            NSPoint::new(20.0, 300.0),
            NSSize::new(300.0, 24.0),
        ));
        apps.addSubview(&header);
        let initial: Vec<String> = flags
            .apps_lines
            .lock()
            .map(|l| l.clone())
            .unwrap_or_default();
        for row in 0..APPS_ROWS {
            let text = initial.get(row).map(String::as_str).unwrap_or("");
            let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
            label.setFrame(NSRect::new(
                NSPoint::new(20.0, 270.0 - row as f64 * 26.0),
                NSSize::new(440.0, 20.0),
            ));
            apps.addSubview(&label);
            apps_labels.push(label);

            // Per-row Delete: the tag carries the row index; the run loop
            // resolves it against apps_row_ids (same cap/order as the
            // lines) and deletes that app's history.
            // SAFETY: target outlives the window (held by MacosSettingsWindow).
            let delete = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str("Delete"),
                    Some({
                        let any: &AnyObject = target.as_ref();
                        any
                    }),
                    Some(sel!(deleteAppRow:)),
                    mtm,
                )
            };
            delete.setTag(row as isize);
            delete.setFrame(NSRect::new(
                NSPoint::new(410.0, 266.0 - row as f64 * 26.0),
                NSSize::new(80.0, 24.0),
            ));
            apps.addSubview(&delete);
        }
    }

    // Shortcuts tab: static for the process lifetime (bindings are read at
    // launch; the live-rebind refactor is banked), so build-once like About.
    {
        let shortcuts_view = &pane_views[3];
        let text =
            NSTextField::wrappingLabelWithString(&NSString::from_str(&flags.shortcuts_text), mtm);
        text.setFrame(NSRect::new(
            NSPoint::new(20.0, 160.0),
            NSSize::new(460.0, 170.0),
        ));
        text.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        text.setEditable(false);
        shortcuts_view.addSubview(&text);
    }

    // Statistics tab: header + STATS_ROWS data rows. Row strings come from
    // the run loop via flags.stats_lines; show() refreshes them on every
    // open. Monospaced font keeps sparkline glyphs column-aligned.
    {
        let stats = &pane_views[4];
        let stats_header =
            NSTextField::labelWithString(&NSString::from_str("This session + lifetime"), mtm);
        stats_header.setFrame(NSRect::new(
            NSPoint::new(20.0, 300.0),
            NSSize::new(300.0, 24.0),
        ));
        stats.addSubview(&stats_header);

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
                NSPoint::new(20.0, 270.0 - row as f64 * 26.0),
                NSSize::new(440.0, 20.0),
            ));
            stats.addSubview(&label);
            stats_labels.push(label);
        }
    }

    // About tab: static for the process lifetime, so build-once is fine
    // here (unlike the Statistics rows above).
    {
        let about_view = &pane_views[5];
        let about =
            NSTextField::wrappingLabelWithString(&NSString::from_str(&flags.about_text), mtm);
        about.setFrame(NSRect::new(
            NSPoint::new(20.0, 160.0),
            NSSize::new(460.0, 170.0),
        ));
        // Display-only: selectable (lets the user copy the repo URL), not
        // editable. 11pt so the wrapped credits block keeps headroom
        // (review-c101).
        about.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        about.setEditable(false);
        about_view.addSubview(&about);
    }
    window.setContentView(Some(&tabs));
    let _ = &pane_views; // pane views are retained by their tab items
                         // Keep the instance alive across closes: AppKit's default releases a
                         // window on close, which would dangle our Retained pointer.
                         // SAFETY: documented NSWindow property setter.
    unsafe { window.setReleasedWhenClosed(false) };
    BuiltWindow {
        window,
        stats_labels,
        setup_labels,
        apps_labels,
        switches,
    }
}

/// Everything `build_window` hands back: the window plus the data-row label
/// handles each tab needs refreshed on show.
struct BuiltWindow {
    window: Retained<NSWindow>,
    stats_labels: Vec<Retained<NSTextField>>,
    setup_labels: Vec<Retained<NSTextField>>,
    apps_labels: Vec<Retained<NSTextField>>,
    switches: Vec<(Retained<NSSwitch>, Arc<AtomicBool>)>,
}

/// Max Setup row count (accessibility / screen recording / model file).
const SETUP_ROWS: usize = 3;

/// Max Apps rows (top apps by recorded-input count, plus status lines).
/// Public: the run loop's line composer caps to this same number
/// (review-c108 — a drifting duplicate would silently waste label slots).
pub const APPS_ROWS: usize = 8;

/// Fixed Statistics row count (shown / accepted / words / lifetime).
const STATS_ROWS: usize = 4;

/// Number of settings tabs.
pub const PANE_COUNT: usize = 6;

/// Tab titles in display order (Cotypist order) — Setup first, About last;
/// new panes insert between, never around.
pub fn pane_titles() -> [&'static str; PANE_COUNT] {
    [
        "Setup",
        "General",
        "Apps",
        "Shortcuts",
        "Statistics",
        "About",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_titles_are_fixed_and_ordered() {
        // Tab order is part of the settings UX contract (Cotypist order):
        // Setup first, About last. New panes insert between, never around.
        assert_eq!(
            pane_titles(),
            [
                "Setup",
                "General",
                "Apps",
                "Shortcuts",
                "Statistics",
                "About"
            ]
        );
        assert_eq!(pane_titles().len(), PANE_COUNT);
    }

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
