//! P8 (A1b Task 5c): Carbon `RegisterEventHotKey` accept-key probe.
//!
//! Question (design spec §15 F1 / D1): Cotypist ships **no CGEventTap** — it
//! swallows accept keys with Carbon `RegisterEventHotKey` and needs only
//! Accessibility. Our MVP uses a consuming `CGEventTap`, which forces an extra
//! **Input Monitoring** TCC prompt. Can a Carbon hotkey **consume** the accept
//! keys (the known-awkward case is bare Tab) so we can drop the tap + Input
//! Monitoring?
//!
//! This probe registers Carbon hotkeys on **Tab (keycode 48)** and
//! **grave/backtick (keycode 50)** with **no modifiers**, installs a Carbon
//! event handler, and runs the event loop. It prints:
//!   - `REGISTER tab_status=<OSStatus> grave_status=<OSStatus>` — `0` = noErr,
//!     i.e. the key was accepted for a global hotkey (the bare-Tab question).
//!   - `HOTKEY_FIRED key=...` each time a registered key is pressed in ANY app.
//!
//! Live measurement (manual, record in FINDINGS.md): with this probe running,
//! focus another app (TextEdit) and press Tab / grave. If `HOTKEY_FIRED` prints
//! AND the key does NOT reach the other app (no tab inserted, no backtick
//! typed), Carbon consumes it globally → we can drop the CGEventTap + Input
//! Monitoring. Re-run with Input Monitoring **revoked** to confirm Carbon needs
//! only Accessibility. Set `RUN_MS` to bound the run (default 8000).

use std::ffi::c_void;
use std::io::{self, Write};
use std::ptr;
use std::time::{Duration, Instant};

use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};

type OSStatus = i32;
type OSType = u32;
type EventTargetRef = *mut c_void;
type EventHotKeyRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventRef = *mut c_void;
type EventHandlerUPP = extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus;

#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyID {
    signature: OSType,
    id: u32,
}

#[repr(C)]
struct EventTypeSpec {
    event_class: OSType,
    event_kind: u32,
}

// FourCharCodes.
const K_EVENT_CLASS_KEYBOARD: OSType = u32::from_be_bytes(*b"keyb");
const K_EVENT_HOTKEY_PRESSED: u32 = 5;
const K_EVENT_PARAM_DIRECT_OBJECT: OSType = u32::from_be_bytes(*b"----");
const TYPE_EVENT_HOTKEY_ID: OSType = u32::from_be_bytes(*b"hkid");
const HOTKEY_SIGNATURE: OSType = u32::from_be_bytes(*b"cmHK");

const KEYCODE_TAB: u32 = 48;
const KEYCODE_GRAVE: u32 = 50;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn RegisterEventHotKey(
        in_hot_key_code: u32,
        in_hot_key_modifiers: u32,
        in_hot_key_id: EventHotKeyID,
        in_target: EventTargetRef,
        in_options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OSStatus;
    fn InstallEventHandler(
        in_target: EventTargetRef,
        in_handler: EventHandlerUPP,
        in_num_types: u32,
        in_list: *const EventTypeSpec,
        in_user_data: *mut c_void,
        out_ref: *mut EventHandlerRef,
    ) -> OSStatus;
    fn GetEventParameter(
        in_event: EventRef,
        in_name: OSType,
        in_desired_type: OSType,
        out_actual_type: *mut OSType,
        in_buffer_size: usize,
        out_actual_size: *mut usize,
        out_data: *mut c_void,
    ) -> OSStatus;
}

extern "C" fn hotkey_handler(
    _call: EventHandlerCallRef,
    event: EventRef,
    _user: *mut c_void,
) -> OSStatus {
    let mut hkid = EventHotKeyID {
        signature: 0,
        id: 0,
    };
    unsafe {
        GetEventParameter(
            event,
            K_EVENT_PARAM_DIRECT_OBJECT,
            TYPE_EVENT_HOTKEY_ID,
            ptr::null_mut(),
            std::mem::size_of::<EventHotKeyID>(),
            ptr::null_mut(),
            &mut hkid as *mut _ as *mut c_void,
        );
    }
    let key = match hkid.id {
        1 => "Tab(48)",
        2 => "grave(50)",
        _ => "unknown",
    };
    println!("HOTKEY_FIRED id={} key={key}", hkid.id);
    let _ = io::stdout().flush();
    0
}

fn main() {
    let run_ms: u64 = std::env::var("RUN_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8000);

    unsafe {
        let target = GetApplicationEventTarget();

        let spec = EventTypeSpec {
            event_class: K_EVENT_CLASS_KEYBOARD,
            event_kind: K_EVENT_HOTKEY_PRESSED,
        };
        let mut handler_ref: EventHandlerRef = ptr::null_mut();
        let handler_status = InstallEventHandler(
            target,
            hotkey_handler,
            1,
            &spec,
            ptr::null_mut(),
            &mut handler_ref,
        );

        let mut tab_ref: EventHotKeyRef = ptr::null_mut();
        let tab_status = RegisterEventHotKey(
            KEYCODE_TAB,
            0,
            EventHotKeyID {
                signature: HOTKEY_SIGNATURE,
                id: 1,
            },
            target,
            0,
            &mut tab_ref,
        );

        let mut grave_ref: EventHotKeyRef = ptr::null_mut();
        let grave_status = RegisterEventHotKey(
            KEYCODE_GRAVE,
            0,
            EventHotKeyID {
                signature: HOTKEY_SIGNATURE,
                id: 2,
            },
            target,
            0,
            &mut grave_ref,
        );

        println!(
            "REGISTER tab_status={tab_status} grave_status={grave_status} \
             handler_status={handler_status} (0 = noErr / accepted)"
        );
        println!(
            "Carbon hotkeys live for {run_ms}ms. Focus ANOTHER app (TextEdit) and \
             press Tab / grave. HOTKEY_FIRED + the key NOT reaching the app = Carbon \
             consumes globally (drop the CGEventTap + Input Monitoring)."
        );
        let _ = io::stdout().flush();
    }

    let start = Instant::now();
    while (start.elapsed().as_millis() as u64) < run_ms {
        CFRunLoop::run_in_mode(
            unsafe { kCFRunLoopDefaultMode },
            Duration::from_millis(250),
            false,
        );
    }
    println!("DONE");
}
