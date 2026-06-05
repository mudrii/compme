//! P6: transparent, non-activating NSPanel showing grey "ghost text" at a fixed
//! screen point. Pure overlay probe (no lib logic) — proves the AppKit ghost-text
//! mechanism: borderless, non-activating, click-through, floats above other windows.
//!
//! NOTE: blocks on `app.run()` forever (Ctrl-C to quit). The autonomous gate is: it
//! compiles. The visual/focus behaviour is human-verified per MANUAL-ACCEPTANCE.md.
use objc2::rc::Retained;
// objc2 0.6.4: `alloc(mtm)` is a `MainThreadOnly` trait method — the trait must be in scope.
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSPanel,
    NSTextField, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let frame = NSRect::new(NSPoint::new(400.0, 400.0), NSSize::new(240.0, 30.0));
    let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
    // objc2-app-kit 0.3.2: NSPanel's init + setters are SAFE fns (NSWindow's are
    // `unsafe`, but NSPanel overrides init as safe) — no `unsafe` block needed.
    let panel: Retained<NSPanel> = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        style,
        NSBackingStoreType::Buffered,
        false,
    );
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setLevel(101); // above normal windows (NSPopUpMenuWindowLevel)
    panel.setIgnoresMouseEvents(true);
    panel.setHidesOnDeactivate(false);

    let label = NSTextField::labelWithString(&NSString::from_str("ghost completion text"), mtm);
    label.setFrame(NSRect::new(
        NSPoint::new(8.0, 4.0),
        NSSize::new(224.0, 22.0),
    ));
    label.setTextColor(Some(&NSColor::colorWithWhite_alpha(0.5, 0.9)));
    label.setDrawsBackground(false);
    label.setBezeled(false);
    label.setEditable(false);
    if let Some(content) = panel.contentView() {
        content.addSubview(&label);
    }
    panel.orderFrontRegardless();
    println!(
        "Grey ghost text should be visible near the lower-left of the main screen. Ctrl-C to quit."
    );
    app.run();
}
