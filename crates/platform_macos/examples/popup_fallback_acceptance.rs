use std::cell::RefCell;
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::process::{self, Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use accessibility_sys::{
    kAXFocusedUIElementAttribute, kAXIdentifierAttribute, kAXRoleAttribute,
    kAXSelectedTextRangeAttribute, kAXValueAttribute,
};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSTextField, NSView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize, NSString, NSValue,
};
use platform::{
    ux_mode, FieldHandle, InsertStrategy, Inserted, PlatformAdapter, PlatformError, ScreenRect,
    TextContext, UxMode,
};
use platform_macos::MacosPlatformAdapter;

const FIXTURE_IDENTIFIER: &str = "compme-a1b-popup-fixture";
const FIXTURE_VALUE: &str = "popup fixture value";
const INSERT_TEXT: &str = " inserted";

#[derive(Debug)]
struct PopupFixtureIvars {
    value: RefCell<String>,
    selected_range: RefCell<NSRange>,
}

define_class!(
    // SAFETY: This acceptance-only NSView subclass exposes a deliberately
    // small AX text surface: readable and writable value/range, no bounds.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = PopupFixtureIvars]
    struct PopupFixtureView;

    unsafe impl NSObjectProtocol for PopupFixtureView {}

    impl PopupFixtureView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(isAccessibilityElement))]
        fn is_accessibility_element(&self) -> bool {
            true
        }

        #[unsafe(method(accessibilityIsIgnored))]
        fn accessibility_is_ignored(&self) -> bool {
            false
        }

        #[unsafe(method(accessibilityFocusedUIElement))]
        fn accessibility_focused_ui_element(&self) -> *mut AnyObject {
            Retained::autorelease_ptr(self.retain().into())
        }

        #[unsafe(method(accessibilityIdentifier))]
        fn accessibility_identifier(&self) -> *mut NSString {
            Retained::autorelease_ptr(NSString::from_str(FIXTURE_IDENTIFIER))
        }

        #[unsafe(method(accessibilityRole))]
        fn accessibility_role(&self) -> *mut NSString {
            Retained::autorelease_ptr(NSString::from_str("AXTextField"))
        }

        #[unsafe(method(accessibilityValue))]
        fn accessibility_value(&self) -> *mut NSString {
            Retained::autorelease_ptr(NSString::from_str(&self.ivars().value.borrow()))
        }

        #[unsafe(method(setAccessibilityValue:))]
        fn set_accessibility_value(&self, value: Option<&AnyObject>) {
            let Some(value) = value else {
                return;
            };
            let string_value: Option<Retained<NSString>> = unsafe { msg_send![value, description] };
            if let Some(string_value) = string_value {
                let string_value = string_value.to_string();
                *self.ivars().value.borrow_mut() = string_value.clone();
                println!("FIXTURE_SET_VALUE value={string_value:?}");
            }
        }

        #[unsafe(method(accessibilitySelectedTextRange))]
        fn accessibility_selected_text_range(&self) -> NSRange {
            *self.ivars().selected_range.borrow()
        }

        #[unsafe(method(setAccessibilitySelectedTextRange:))]
        fn set_accessibility_selected_text_range(&self, range: NSRange) {
            *self.ivars().selected_range.borrow_mut() = range;
            println!(
                "FIXTURE_SET_RANGE location={} length={}",
                range.location, range.length
            );
        }

        #[unsafe(method(accessibilityAttributeNames))]
        fn accessibility_attribute_names(&self) -> *mut NSArray<NSString> {
            let names = [
                NSString::from_str(kAXIdentifierAttribute),
                NSString::from_str(kAXRoleAttribute),
                NSString::from_str(kAXValueAttribute),
                NSString::from_str(kAXSelectedTextRangeAttribute),
                NSString::from_str(kAXFocusedUIElementAttribute),
            ];
            Retained::autorelease_ptr(NSArray::from_retained_slice(&names))
        }

        #[unsafe(method(accessibilityAttributeValue:))]
        fn accessibility_attribute_value(&self, attribute: &NSString) -> *mut AnyObject {
            let attribute = attribute.to_string();
            let value: Option<Retained<AnyObject>> = if attribute == kAXIdentifierAttribute {
                Some(NSString::from_str(FIXTURE_IDENTIFIER).into())
            } else if attribute == kAXRoleAttribute {
                Some(NSString::from_str("AXTextField").into())
            } else if attribute == kAXValueAttribute {
                Some(NSString::from_str(&self.ivars().value.borrow()).into())
            } else if attribute == kAXSelectedTextRangeAttribute {
                let range = *self.ivars().selected_range.borrow();
                Some(unsafe { NSValue::valueWithRange(range) }.into())
            } else if attribute == kAXFocusedUIElementAttribute {
                Some(self.retain().into())
            } else {
                None
            };
            value.map_or(std::ptr::null_mut(), Retained::autorelease_ptr)
        }

        #[unsafe(method(accessibilityIsAttributeSettable:))]
        fn accessibility_is_attribute_settable(&self, attribute: &NSString) -> bool {
            let attribute = attribute.to_string();
            attribute == kAXValueAttribute || attribute == kAXSelectedTextRangeAttribute
        }

        #[unsafe(method(accessibilitySetValue:forAttribute:))]
        fn accessibility_set_value_for_attribute(
            &self,
            value: Option<&AnyObject>,
            attribute: &NSString,
        ) {
            let attribute = attribute.to_string();
            if attribute == kAXValueAttribute {
                let string_value: Option<Retained<NSString>> =
                    value.and_then(|value| unsafe { msg_send![value, description] });
                if let Some(string_value) = string_value {
                    let string_value = string_value.to_string();
                    *self.ivars().value.borrow_mut() = string_value.clone();
                    println!("FIXTURE_SET_VALUE value={string_value:?}");
                }
            } else if attribute == kAXSelectedTextRangeAttribute {
                if let Some(value) = value {
                    let range: NSRange = unsafe { msg_send![value, rangeValue] };
                    *self.ivars().selected_range.borrow_mut() = range;
                    println!(
                        "FIXTURE_SET_RANGE location={} length={}",
                        range.location, range.length
                    );
                }
            }
        }

        #[unsafe(method(accessibilityParameterizedAttributeNames))]
        fn accessibility_parameterized_attribute_names(&self) -> *mut NSArray<NSString> {
            Retained::autorelease_ptr(NSArray::from_slice(&[]))
        }
    }
);

impl PopupFixtureView {
    fn new(frame: NSRect, value: &str, mtm: MainThreadMarker) -> Retained<Self> {
        let caret = NSRange::new(value.encode_utf16().count(), 0);
        let this = Self::alloc(mtm).set_ivars(PopupFixtureIvars {
            value: RefCell::new(value.to_string()),
            selected_range: RefCell::new(caret),
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

fn main() {
    let mut args = env::args().skip(1);
    if matches!(args.next().as_deref(), Some("--serve")) {
        serve_fixture();
        return;
    }

    let duration = env::args()
        .nth(1)
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(3));

    let mut child = spawn_fixture();
    let pid = child.id();
    let accepted = validate_popup(pid, duration);
    shutdown_fixture(&mut child);

    println!("SUMMARY popup={accepted} fixture_pid={pid}");
    if !accepted {
        process::exit(1);
    }
}

fn serve_fixture() {
    let mtm = MainThreadMarker::new().expect("must run on AppKit main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    let window_frame = NSRect::new(NSPoint::new(360.0, 360.0), NSSize::new(520.0, 160.0));
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            window_frame,
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str("A1b Popup Fallback Target"));

    let view_frame = NSRect::new(NSPoint::new(24.0, 52.0), NSSize::new(472.0, 48.0));
    let view = PopupFixtureView::new(view_frame, FIXTURE_VALUE, mtm);

    let label = NSTextField::labelWithString(
        &NSString::from_str("Writable AX text without caret geometry"),
        mtm,
    );
    label.setFrame(NSRect::new(
        NSPoint::new(24.0, 108.0),
        NSSize::new(472.0, 24.0),
    ));

    let content = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(520.0, 160.0)),
    );
    content.addSubview(&label);
    content.addSubview(&view);
    window.setContentView(Some(&content));
    window.makeKeyAndOrderFront(None);
    let _ = window.makeFirstResponder(Some(&view));
    app.activate();

    println!("FIXTURE pid={} window_ready=true", process::id());
    std::io::stdout().flush().expect("flush fixture ready line");
    app.run();
}

fn spawn_fixture() -> Child {
    let exe = env::current_exe().expect("current executable path");
    let mut child = Command::new(exe)
        .arg("--serve")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn popup fixture");

    let stdout = child.stdout.take().expect("fixture stdout pipe");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("read fixture ready line");
    if !line.starts_with("FIXTURE ") {
        shutdown_fixture(&mut child);
        panic!("fixture did not report readiness: {line:?}");
    }
    print!("{line}");
    child.stdout = Some(reader.into_inner());
    child
}

fn validate_popup(pid: u32, duration: Duration) -> bool {
    let adapter = match MacosPlatformAdapter::with_frontmost_pid_override_for_acceptance(pid as i32)
    {
        Ok(adapter) => adapter,
        Err(err) => {
            eprintln!("ADAPTER_ERROR {err:?}");
            return false;
        }
    };
    let field = FieldHandle {
        app: format!("pid:{pid}"),
        pid: Some(pid),
        element_id: format!("ax:pid={pid}|id={FIXTURE_IDENTIFIER}|role=AXTextField"),
        generation: 1,
    };

    let deadline = Instant::now() + duration;
    let mut accepted = false;
    while Instant::now() < deadline {
        let read = adapter.read_context(&field);
        let rect = adapter.caret_rect(&field);
        let caps = adapter.capabilities(&field);
        let anchor = adapter.popup_anchor(&field);
        println!("READ {read:?}");
        println!("RECT {rect:?}");
        println!("CAPS {caps:?}");
        println!("ANCHOR {anchor:?}");
        if read.is_ok()
            && matches!(rect, Ok(None))
            && matches!(caps, Ok(ref caps) if ux_mode(caps) == UxMode::Popup)
            && popup_anchor_accepted(&anchor)
        {
            let inserted = adapter.insert(&field, INSERT_TEXT, InsertStrategy::AxSet);
            println!("INSERT {inserted:?}");
            let after_read = adapter.read_context(&field);
            println!("READ_AFTER_INSERT {after_read:?}");
            let expected_left = format!("{FIXTURE_VALUE}{INSERT_TEXT}");
            let insert_accepted =
                popup_insert_readback_accepted(&inserted, &after_read, &expected_left);
            if !insert_accepted {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            accepted = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    accepted
}

fn popup_anchor_accepted(anchor: &Result<Option<ScreenRect>, PlatformError>) -> bool {
    matches!(anchor, Ok(Some(rect)) if rect.w > 0.0 && rect.h > 0.0)
}

fn popup_insert_readback_accepted(
    inserted: &Result<Inserted, PlatformError>,
    after_read: &Result<TextContext, PlatformError>,
    expected_left: &str,
) -> bool {
    matches!(
        inserted,
        Ok(inserted)
            if inserted.strategy == InsertStrategy::AxSet
                && inserted.bytes == INSERT_TEXT.len()
                && inserted.chars == INSERT_TEXT.chars().count()
    ) && matches!(
        after_read,
        Ok(context)
            if context.left == expected_left
                && context.right.is_empty()
                && context.selection.is_none()
                && context.caret == expected_left.encode_utf16().count()
    )
}

fn shutdown_fixture(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::{ContextSource, OffsetEncoding};

    fn inserted(strategy: InsertStrategy) -> Result<Inserted, PlatformError> {
        Ok(Inserted {
            bytes: INSERT_TEXT.len(),
            chars: INSERT_TEXT.chars().count(),
            strategy,
        })
    }

    fn text_context(left: &str, right: &str) -> Result<TextContext, PlatformError> {
        Ok(TextContext {
            left: left.into(),
            right: right.into(),
            left_scalars: left.chars().count(),
            selection: None,
            selected_text: None,
            caret: left.encode_utf16().count(),
            source: ContextSource::Accessibility,
            field_id: FieldHandle {
                app: "pid:1".into(),
                pid: Some(1),
                element_id: "fixture".into(),
                generation: 1,
            },
            offset_encoding: OffsetEncoding::Utf16CodeUnits,
        })
    }

    #[test]
    fn popup_acceptance_requires_ax_insert_and_mutated_readback() {
        assert!(popup_insert_readback_accepted(
            &inserted(InsertStrategy::AxSet),
            &text_context("popup fixture value inserted", ""),
            "popup fixture value inserted"
        ));
    }

    #[test]
    fn popup_acceptance_requires_positive_popup_anchor() {
        assert!(popup_anchor_accepted(&Ok(Some(ScreenRect {
            x: 10.0,
            y: 20.0,
            w: 300.0,
            h: 80.0,
        }))));
        assert!(!popup_anchor_accepted(&Ok(None)));
        assert!(!popup_anchor_accepted(&Err(PlatformError::Timeout)));
        assert!(!popup_anchor_accepted(&Ok(Some(ScreenRect {
            x: 10.0,
            y: 20.0,
            w: 0.0,
            h: 80.0,
        }))));
        assert!(!popup_anchor_accepted(&Ok(Some(ScreenRect {
            x: 10.0,
            y: 20.0,
            w: 300.0,
            h: 0.0,
        }))));
    }

    #[test]
    fn popup_acceptance_rejects_success_without_mutated_readback() {
        assert!(!popup_insert_readback_accepted(
            &inserted(InsertStrategy::AxSet),
            &text_context("popup fixture value", ""),
            "popup fixture value inserted"
        ));
    }

    #[test]
    fn popup_acceptance_rejects_non_ax_insert_strategy() {
        assert!(!popup_insert_readback_accepted(
            &inserted(InsertStrategy::Clipboard),
            &text_context("popup fixture value inserted", ""),
            "popup fixture value inserted"
        ));
    }

    #[test]
    fn popup_acceptance_rejects_failed_insert_or_readback() {
        assert!(!popup_insert_readback_accepted(
            &Err(PlatformError::CannotComplete {
                reason: "insert failed".into(),
            }),
            &text_context("popup fixture value inserted", ""),
            "popup fixture value inserted"
        ));
        assert!(!popup_insert_readback_accepted(
            &inserted(InsertStrategy::AxSet),
            &Err(PlatformError::Timeout),
            "popup fixture value inserted"
        ));
    }

    #[test]
    fn popup_acceptance_rejects_mutated_text_with_stale_selection_or_caret() {
        let mut selected = text_context("popup fixture value inserted", "").unwrap();
        selected.selection = Some(platform::TextRange { start: 0, end: 1 });
        assert!(!popup_insert_readback_accepted(
            &inserted(InsertStrategy::AxSet),
            &Ok(selected),
            "popup fixture value inserted"
        ));

        let mut stale_caret = text_context("popup fixture value inserted", "").unwrap();
        stale_caret.caret = "popup fixture value".encode_utf16().count();
        assert!(!popup_insert_readback_accepted(
            &inserted(InsertStrategy::AxSet),
            &Ok(stale_caret),
            "popup fixture value inserted"
        ));
    }

    #[test]
    fn popup_acceptance_rejects_wrong_insert_receipt_counts() {
        let wrong_bytes = Ok(Inserted {
            bytes: INSERT_TEXT.len() - 1,
            chars: INSERT_TEXT.chars().count(),
            strategy: InsertStrategy::AxSet,
        });
        assert!(!popup_insert_readback_accepted(
            &wrong_bytes,
            &text_context("popup fixture value inserted", ""),
            "popup fixture value inserted"
        ));

        let wrong_chars = Ok(Inserted {
            bytes: INSERT_TEXT.len(),
            chars: INSERT_TEXT.chars().count() - 1,
            strategy: InsertStrategy::AxSet,
        });
        assert!(!popup_insert_readback_accepted(
            &wrong_chars,
            &text_context("popup fixture value inserted", ""),
            "popup fixture value inserted"
        ));
    }
}
