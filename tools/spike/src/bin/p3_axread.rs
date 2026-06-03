//! P3: read focused field value + left-of-caret context via Accessibility.
//!
//! TDD wiring: the FFI here is thin glue. `AxField` (a focused `AXUIElementRef`)
//! implements the tested `spike::context::FieldSource` seam, and the printed tail
//! is produced by the lib's `spike::context::left_tail(&value, caret, 40)` — this
//! bin owns NO context logic of its own.
//!
//! Run, then focus a TextEdit document and type — it reads every 1s.
//! NOTE: blocks on a 1s polling loop forever (Ctrl-C to stop); behaviour is
//! human-verified per MANUAL-ACCEPTANCE.md. The autonomous gate is: it compiles.
use std::os::raw::c_void;
use std::{thread, time::Duration};

use accessibility_sys::{
    kAXErrorSuccess, kAXFocusedUIElementAttribute, kAXSelectedTextRangeAttribute,
    kAXValueAttribute, kAXValueTypeCFRange, AXIsProcessTrusted, AXUIElementCopyAttributeValue,
    AXUIElementCreateSystemWide, AXUIElementRef, AXValueGetValue, AXValueRef,
};
use core_foundation::base::{CFRange, CFType, CFTypeRef, TCFType};
use core_foundation::string::{CFString, CFStringRef};

use spike::context::{left_tail, FieldSource};

/// Copy an AX attribute as a managed CFType (released on drop). None on error/missing.
unsafe fn copy_attr(el: AXUIElementRef, attr: &str) -> Option<CFType> {
    let cf = CFString::new(attr);
    let mut out: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(el, cf.as_concrete_TypeRef(), &mut out);
    if err == kAXErrorSuccess && !out.is_null() {
        Some(CFType::wrap_under_create_rule(out))
    } else {
        None
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
                let mut range = CFRange { location: 0, length: 0 };
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
        println!("Focus a TextEdit doc and type; reading every 1s. Ctrl-C to stop.");
        loop {
            let Some(focused) = copy_attr(sys, kAXFocusedUIElementAttribute) else {
                println!("(no focused element)");
                thread::sleep(Duration::from_secs(1));
                continue;
            };
            let field = AxField { el: focused.as_CFTypeRef() as AXUIElementRef };

            match field.value() {
                Some(value) => {
                    let caret = field.caret();
                    // Printed tail comes from the tested lib seam — NOT inline logic.
                    let tail = left_tail(&value, caret, 40);
                    println!("caret={} left_tail=\"{}\"", caret, tail.replace('\n', "\\n"));
                }
                None => println!("(focused element exposes no AXValue — unsupported field)"),
            }
            thread::sleep(Duration::from_secs(1));
        }
    }
}
