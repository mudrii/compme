//! P4: caret-rect ladder over real Accessibility, wired to the tested lib.
//!
//! TDD wiring: `AxField` (a focused `AXUIElementRef`) implements the tested
//! `spike::caret::BoundsSource` seam — its `bounds(loc, len)` calls the AX
//! `bounds_for_range` helper (`AXUIElementCopyParameterizedAttributeValue` with
//! `kAXBoundsForRangeParameterizedAttribute`) and returns a `spike::geometry::ScreenRect`.
//! The tier (`exact`/`derived`/`none`) is decided by `spike::caret::resolve_caret`,
//! which owns the whole ladder (zero-length-at-caret -> prev-char right edge ->
//! container/empty rejection). This bin re-implements NONE of that.
//!
//! Run, focus TextEdit, type; prints the caret screen rect + which tier produced it.
//! For automation, set `SPIKE_AX_PID=<pid>` to target one app's focused element.
//! NOTE: blocks on a 1s polling loop forever (Ctrl-C to stop); behaviour is
//! human-verified per MANUAL-ACCEPTANCE.md. The autonomous gate is: it compiles.
use std::env;
use std::io::{self, Write};
use std::os::raw::c_void;
use std::{thread, time::Duration};

use accessibility_sys::{
    kAXBoundsForRangeParameterizedAttribute, kAXErrorSuccess, kAXFocusedUIElementAttribute,
    kAXSelectedTextRangeAttribute, kAXValueTypeCFRange, kAXValueTypeCGRect, AXIsProcessTrusted,
    AXUIElementCopyAttributeValue, AXUIElementCopyParameterizedAttributeValue,
    AXUIElementCreateApplication, AXUIElementCreateSystemWide, AXUIElementRef, AXValueCreate,
    AXValueGetValue, AXValueRef,
};
use core_foundation::base::{CFRange, CFType, CFTypeRef, TCFType};
use core_foundation::string::CFString;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use objc2_app_kit::NSWorkspace;

use spike::caret::{resolve_caret, BoundsSource};
use spike::geometry::ScreenRect;

unsafe fn copy_attr(el: AXUIElementRef, attr: &str) -> Option<CFType> {
    copy_attr_result(el, attr).ok()
}

unsafe fn copy_attr_result(
    el: AXUIElementRef,
    attr: &str,
) -> Result<CFType, accessibility_sys::AXError> {
    let cf = CFString::new(attr);
    let mut out: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(el, cf.as_concrete_TypeRef(), &mut out);
    if err == kAXErrorSuccess && !out.is_null() {
        Ok(CFType::wrap_under_create_rule(out))
    } else {
        Err(err)
    }
}

unsafe fn focused_element(sys: AXUIElementRef) -> Result<CFType, accessibility_sys::AXError> {
    if let Some(pid) = env::var("SPIKE_AX_PID")
        .ok()
        .and_then(|raw| raw.parse::<i32>().ok())
    {
        let app = AXUIElementCreateApplication(pid);
        if !app.is_null() {
            let _app = CFType::wrap_under_create_rule(app as CFTypeRef);
            if let Ok(focused) = copy_attr_result(app, kAXFocusedUIElementAttribute) {
                return Ok(focused);
            }
        }
    }

    match copy_attr_result(sys, kAXFocusedUIElementAttribute) {
        Ok(focused) => Ok(focused),
        Err(system_err) => {
            let Some(frontmost) = NSWorkspace::sharedWorkspace().frontmostApplication() else {
                return Err(system_err);
            };
            let pid = frontmost.processIdentifier();
            if pid < 0 {
                return Err(system_err);
            }

            let app = AXUIElementCreateApplication(pid);
            if app.is_null() {
                return Err(system_err);
            }
            let _app = CFType::wrap_under_create_rule(app as CFTypeRef);
            copy_attr_result(app, kAXFocusedUIElementAttribute).map_err(|_| system_err)
        }
    }
}

/// AX `bounds_for_range`: screen rect of the text range [location, location+length).
/// Returns the raw `CGRect` (top-left origin); the lib decides usability/tier.
unsafe fn bounds_for_range(el: AXUIElementRef, location: isize, length: isize) -> Option<CGRect> {
    let range = CFRange { location, length };
    let axval = AXValueCreate(kAXValueTypeCFRange, &range as *const _ as *const c_void);
    if axval.is_null() {
        return None;
    }
    let _hold = CFType::wrap_under_create_rule(axval as CFTypeRef); // release on drop
    let pname = CFString::new(kAXBoundsForRangeParameterizedAttribute);
    let mut out: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyParameterizedAttributeValue(
        el,
        pname.as_concrete_TypeRef(),
        axval as CFTypeRef,
        &mut out,
    );
    if err != kAXErrorSuccess || out.is_null() {
        return None;
    }
    let held = CFType::wrap_under_create_rule(out);
    let mut rect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: 0.0,
            height: 0.0,
        },
    };
    // AXValueGetValue returns bool in accessibility-sys 0.2.0.
    if AXValueGetValue(
        held.as_CFTypeRef() as AXValueRef,
        kAXValueTypeCGRect,
        &mut rect as *mut _ as *mut c_void,
    ) {
        Some(rect)
    } else {
        None
    }
}

/// Real `BoundsSource` over a focused AX element. The only logic here is the
/// CGRect -> ScreenRect shape conversion; the ladder lives in the lib.
struct AxField {
    el: AXUIElementRef,
}

impl BoundsSource for AxField {
    fn bounds(&self, location: isize, length: isize) -> Option<ScreenRect> {
        unsafe {
            bounds_for_range(self.el, location, length).map(|r| ScreenRect {
                x: r.origin.x,
                y: r.origin.y,
                w: r.size.width,
                h: r.size.height,
            })
        }
    }
}

fn main() {
    unsafe {
        // AXIsProcessTrusted returns bool in accessibility-sys 0.2.0.
        if !AXIsProcessTrusted() {
            eprintln!("grant Accessibility, relaunch.");
            return;
        }
        let sys = AXUIElementCreateSystemWide();
        println!(
            "Focus a text field; printing caret rect every 1s. \
             Optional: SPIKE_AX_PID targets one app. Ctrl-C to stop."
        );
        loop {
            let focused = match focused_element(sys) {
                Ok(focused) => focused,
                Err(err) => {
                    println!("(no focus; err={err:?})");
                    let _ = io::stdout().flush();
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }
            };
            let el = focused.as_CFTypeRef() as AXUIElementRef;

            // Read the caret (selected-range location).
            let mut caret: isize = 0;
            if let Some(r) = copy_attr(el, kAXSelectedTextRangeAttribute) {
                let mut range = CFRange {
                    location: 0,
                    length: 0,
                };
                if AXValueGetValue(
                    r.as_CFTypeRef() as AXValueRef,
                    kAXValueTypeCFRange,
                    &mut range as *mut _ as *mut c_void,
                ) {
                    caret = range.location;
                }
            }

            // Tier + rect decided entirely by the tested lib ladder.
            let ax_field = AxField { el };
            let (tier, rect) = resolve_caret(&ax_field, caret);
            match rect {
                Some(r) => println!(
                    "caret={} tier={:?} rect=({:.0},{:.0} {:.0}x{:.0})",
                    caret, tier, r.x, r.y, r.w, r.h
                ),
                None => println!(
                    "caret={} tier={:?} (no usable caret rect -> popup fallback)",
                    caret, tier
                ),
            }
            let _ = io::stdout().flush();
            thread::sleep(Duration::from_secs(1));
        }
    }
}
