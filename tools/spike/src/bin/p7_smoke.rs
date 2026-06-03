//! P7: integration smoke — read -> infer -> overlay at the caret, ALL via the lib.
//!
//! TDD wiring: this bin is FFI glue only. A single `AxField` (focused `AXUIElementRef`)
//! implements BOTH tested seams — `spike::context::FieldSource` (value + caret) and
//! `spike::caret::BoundsSource` (range bounds). The flow uses:
//!   * `spike::completion::suggest(value, caret, &LlamaCompleter, 4)` for the text
//!     (the `LlamaCompleter` is the same real `spike::completion::Completer` as p2_infer),
//!   * `spike::caret::resolve_caret(&ax_field, caret)` for the caret rect (ladder owned by lib),
//!   * `spike::geometry::ax_to_cocoa_origin(screen_h, rect)` for panel placement (coord flip owned by lib).
//! No inline ladder / pipeline / coordinate math lives here.
//!
//! After 3s (focus a TextEdit doc WITH text), it reads, completes, and shows grey
//! ghost text at the caret. One-shot, then blocks on `app.run()` (Ctrl-C to quit).
//! The autonomous gate is: it compiles. End-to-end behaviour is human-verified per
//! MANUAL-ACCEPTANCE.md.
use std::num::NonZeroU32;
use std::os::raw::c_void;
use std::{thread, time::Duration};

use accessibility_sys::{
    kAXBoundsForRangeParameterizedAttribute, kAXErrorSuccess, kAXFocusedUIElementAttribute,
    kAXSelectedTextRangeAttribute, kAXValueAttribute, kAXValueTypeCFRange, kAXValueTypeCGRect,
    AXIsProcessTrusted, AXUIElementCopyAttributeValue, AXUIElementCopyParameterizedAttributeValue,
    AXUIElementCreateSystemWide, AXUIElementRef, AXValueCreate, AXValueGetValue, AXValueRef,
};
use core_foundation::base::{CFRange, CFType, CFTypeRef, TCFType};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::geometry::{CGPoint, CGRect, CGSize};

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel, Special};
use llama_cpp_2::sampling::LlamaSampler;

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSPanel, NSScreen,
    NSTextField, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use spike::caret::{resolve_caret, BoundsSource};
use spike::completion::{suggest, Completer};
use spike::context::FieldSource;
use spike::geometry::{ax_to_cocoa_origin, ScreenRect};

const MODEL: &str = "models/qwen2.5-0.5b-instruct-q4_k_m.gguf";
const N_TOKENS: usize = 8;

// ---- AX glue -------------------------------------------------------------

unsafe fn copy_attr(el: AXUIElementRef, attr: &str) -> Option<CFType> {
    let cf = CFString::new(attr);
    let mut out: CFTypeRef = std::ptr::null();
    if AXUIElementCopyAttributeValue(el, cf.as_concrete_TypeRef(), &mut out) == kAXErrorSuccess
        && !out.is_null()
    {
        Some(CFType::wrap_under_create_rule(out))
    } else {
        None
    }
}

unsafe fn bounds_for_range(el: AXUIElementRef, location: isize, length: isize) -> Option<CGRect> {
    let range = CFRange { location, length };
    let axval = AXValueCreate(kAXValueTypeCFRange, &range as *const _ as *const c_void);
    if axval.is_null() {
        return None;
    }
    let _hold = CFType::wrap_under_create_rule(axval as CFTypeRef);
    let pname = CFString::new(kAXBoundsForRangeParameterizedAttribute);
    let mut out: CFTypeRef = std::ptr::null();
    if AXUIElementCopyParameterizedAttributeValue(
        el,
        pname.as_concrete_TypeRef(),
        axval as CFTypeRef,
        &mut out,
    ) == kAXErrorSuccess
        && !out.is_null()
    {
        let held = CFType::wrap_under_create_rule(out);
        let mut r = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize { width: 0.0, height: 0.0 },
        };
        // AXValueGetValue returns bool in accessibility-sys 0.2.0.
        if AXValueGetValue(
            held.as_CFTypeRef() as AXValueRef,
            kAXValueTypeCGRect,
            &mut r as *mut _ as *mut c_void,
        ) {
            return Some(r);
        }
    }
    None
}

/// Focused AX element implementing BOTH lib seams; the only logic is shape conversion.
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

// ---- Model glue (same real Completer pattern as p2_infer) ----------------

struct LlamaCompleter {
    backend: LlamaBackend,
    model: LlamaModel,
    ctx_params: LlamaContextParams,
}

impl LlamaCompleter {
    fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let backend = LlamaBackend::init()?;
        let model = LlamaModel::load_from_file(
            &backend,
            MODEL,
            &LlamaModelParams::default().with_n_gpu_layers(999),
        )?;
        let ctx_params =
            LlamaContextParams::default().with_n_ctx(Some(NonZeroU32::new(2048).unwrap()));
        Ok(Self { backend, model, ctx_params })
    }
}

impl Completer for LlamaCompleter {
    fn complete(&self, prompt: &str) -> String {
        let mut ctx = self
            .model
            .new_context(&self.backend, self.ctx_params.clone())
            .unwrap();
        let toks = self.model.str_to_token(prompt, AddBos::Always).unwrap();
        let mut b = LlamaBatch::new(512, 1);
        let last = toks.len() - 1;
        for (i, t) in toks.iter().enumerate() {
            b.add(*t, i as i32, &[0], i == last).unwrap();
        }
        ctx.decode(&mut b).unwrap();
        let mut s = LlamaSampler::greedy();
        let mut cur = b.n_tokens();
        let mut out = String::new();
        for _ in 0..N_TOKENS {
            let tok = s.sample(&ctx, b.n_tokens() - 1);
            if self.model.is_eog_token(tok) {
                break;
            }
            if let Ok(piece) = self.model.token_to_str(tok, Special::Tokenize) {
                out.push_str(&piece);
            }
            b.clear();
            b.add(tok, cur, &[0], true).unwrap();
            cur += 1;
            ctx.decode(&mut b).unwrap();
        }
        out
    }
}

// ---- Integration flow ----------------------------------------------------

fn main() {
    unsafe {
        // AXIsProcessTrusted returns bool in accessibility-sys 0.2.0.
        if !AXIsProcessTrusted() {
            eprintln!("grant Accessibility + Input Monitoring, relaunch.");
            return;
        }
    }
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    println!("Focus a TextEdit doc with some text. Reading in 3s...");
    thread::sleep(Duration::from_secs(3));

    // Read the focused field once, then drive ALL logic through the lib seams.
    let (value, caret, rect) = unsafe {
        let sys = AXUIElementCreateSystemWide();
        match copy_attr(sys, kAXFocusedUIElementAttribute) {
            Some(f) => {
                let field = AxField { el: f.as_CFTypeRef() as AXUIElementRef };
                let value = field.value().unwrap_or_default();
                let caret = field.caret();
                // Caret rect: lib ladder owns tier selection + container rejection.
                let (_tier, rect) = resolve_caret(&field, caret as isize);
                (value, caret, rect)
            }
            None => (String::new(), 0usize, None),
        }
    };

    if value.trim().is_empty() {
        eprintln!("no left context — focus a text field WITH text and retry.");
        return;
    }

    // Completion via the tested pipeline (left_context -> trim -> complete -> cap).
    let completer = match LlamaCompleter::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("model load failed: {e}");
            return;
        }
    };
    let completion = suggest(&value, caret, &completer, 4);
    println!("caret: {caret}");
    println!("completion: {:?}", completion.replace('\n', "\\n"));

    // Panel placement: lib owns the AX(top-left) -> Cocoa(bottom-left) coordinate flip.
    let (px, py, ph) = match rect {
        Some(r) => {
            let screen_h = NSScreen::mainScreen(mtm)
                .map(|s| s.frame().size.height)
                .unwrap_or(900.0);
            let (x, y) = ax_to_cocoa_origin(screen_h, r);
            (x, y, r.h.max(18.0))
        }
        None => {
            println!("(no caret rect — fixed point)");
            (400.0, 400.0, 22.0)
        }
    };

    // Overlay (NSPanel init + setters are safe fns in objc2-app-kit 0.3.2).
    let frame = NSRect::new(NSPoint::new(px, py), NSSize::new(260.0, ph + 6.0));
    let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
    let panel: Retained<NSPanel> = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        style,
        NSBackingStoreType::Buffered,
        false,
    );
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setLevel(101);
    panel.setIgnoresMouseEvents(true);
    panel.setHidesOnDeactivate(false);
    let label = NSTextField::labelWithString(&NSString::from_str(&completion), mtm);
    label.setFrame(NSRect::new(NSPoint::new(2.0, 2.0), NSSize::new(256.0, ph)));
    label.setTextColor(Some(&NSColor::colorWithWhite_alpha(0.5, 0.9)));
    label.setDrawsBackground(false);
    label.setBezeled(false);
    label.setEditable(false);
    if let Some(c) = panel.contentView() {
        c.addSubview(&label);
    }
    panel.orderFrontRegardless();

    println!("Ghost completion shown at caret. Ctrl-C to quit.");
    app.run();
}
