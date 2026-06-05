//! P3: read focused field value + left-of-caret context via Accessibility.
//!
//! TDD wiring: the FFI here is thin glue. `AxField` (a focused `AXUIElementRef`)
//! implements the tested `spike::context::FieldSource` seam, and the printed tail
//! is produced by the lib's `spike::context::left_tail(&value, caret, 40)` — this
//! bin owns NO context logic of its own.
//!
//! Run, then focus a text field and type — it reads every 1s.
//! For automation, set `SPIKE_AX_PID=<pid>` to target one app's focused element.
//! NOTE: blocks on a 1s polling loop forever (Ctrl-C to stop); behaviour is
//! human-verified per MANUAL-ACCEPTANCE.md. The autonomous gate is: it compiles.
use std::env;
use std::io::{self, Write};
use std::os::raw::c_void;
use std::{thread, time::Duration};

use accessibility_sys::{
    kAXErrorSuccess, kAXFocusedUIElementAttribute, kAXSelectedTextRangeAttribute,
    kAXValueAttribute, kAXValueTypeCFRange, AXIsProcessTrusted, AXUIElementCopyAttributeValue,
    AXUIElementCreateApplication, AXUIElementCreateSystemWide, AXUIElementRef, AXValueGetValue,
    AXValueRef,
};
use core_foundation::base::{CFRange, CFType, CFTypeRef, TCFType};
use core_foundation::string::{CFString, CFStringRef};
use objc2_app_kit::NSWorkspace;

use spike::context::{left_tail, FieldSource};

/// Copy an AX attribute as a managed CFType (released on drop). None on error/missing.
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

/// Real `FieldSource` over a focused AX element. Reads the field's value
/// (`kAXValueAttribute`) and caret (`kAXSelectedTextRangeAttribute`) on demand.
struct AxField {
    el: AXUIElementRef,
}

impl FieldSource for AxField {
    fn value(&self) -> Option<String> {
        unsafe {
            copy_attr(self.el, kAXValueAttribute)
                .map(|v| CFString::wrap_under_get_rule(v.as_CFTypeRef() as CFStringRef).to_string())
        }
    }

    fn caret(&self) -> usize {
        unsafe {
            if let Some(r) = copy_attr(self.el, kAXSelectedTextRangeAttribute) {
                let mut range = CFRange {
                    location: 0,
                    length: 0,
                };
                // AXValueGetValue returns bool in accessibility-sys 0.2.0.
                if AXValueGetValue(
                    r.as_CFTypeRef() as AXValueRef,
                    kAXValueTypeCFRange,
                    &mut range as *mut _ as *mut c_void,
                ) {
                    return range.location.max(0) as usize;
                }
            }
            0
        }
    }
}

fn main() {
    unsafe {
        // AXIsProcessTrusted returns bool in accessibility-sys 0.2.0.
        if !AXIsProcessTrusted() {
            eprintln!("NOT trusted: grant Accessibility to this terminal, then relaunch it.");
            return;
        }
        let sys = AXUIElementCreateSystemWide();
        println!(
            "Focus a text field and type; reading every 1s. \
             Optional: SPIKE_AX_PID targets one app. Ctrl-C to stop."
        );
        loop {
            let focused = match focused_element(sys) {
                Ok(focused) => focused,
                Err(err) => {
                    println!("(no focused element; err={err:?})");
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }
            };
            let field = AxField {
                el: focused.as_CFTypeRef() as AXUIElementRef,
            };

            match field.value() {
                Some(value) => {
                    let caret = field.caret();
                    // Printed tail comes from the tested lib seam — NOT inline logic.
                    let tail = left_tail(&value, caret, 40);
                    println!(
                        "caret={} left_tail=\"{}\"",
                        caret,
                        tail.replace('\n', "\\n")
                    );
                }
                None => println!("(focused element exposes no AXValue — unsupported field)"),
            }
            let _ = io::stdout().flush();
            thread::sleep(Duration::from_secs(1));
        }
    }
}
