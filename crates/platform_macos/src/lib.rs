//! macOS platform adapter scaffolding.

use std::any::Any;
use std::collections::HashMap;
use std::ffi::{c_uchar, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, Once};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use accessibility_sys::{
    kAXBoundsForRangeParameterizedAttribute, kAXErrorAPIDisabled, kAXErrorAttributeUnsupported,
    kAXErrorCannotComplete, kAXErrorFailure, kAXErrorIllegalArgument, kAXErrorInvalidUIElement,
    kAXErrorNoValue, kAXErrorParameterizedAttributeUnsupported, kAXErrorSuccess,
    kAXFocusedUIElementAttribute, kAXFocusedUIElementChangedNotification, kAXIdentifierAttribute,
    kAXRoleAttribute, kAXSecureTextFieldSubrole, kAXSelectedTextChangedNotification,
    kAXSelectedTextRangeAttribute, kAXSubroleAttribute, kAXTrustedCheckOptionPrompt,
    kAXValueAttribute, kAXValueTypeCFRange, kAXValueTypeCGRect, AXError, AXIsProcessTrusted,
    AXIsProcessTrustedWithOptions, AXObserverAddNotification, AXObserverCreate,
    AXObserverGetRunLoopSource, AXObserverRef, AXObserverRemoveNotification,
    AXUIElementCopyAttributeValue, AXUIElementCopyParameterizedAttributeValue,
    AXUIElementCreateApplication, AXUIElementCreateSystemWide, AXUIElementGetPid,
    AXUIElementIsAttributeSettable, AXUIElementRef, AXUIElementSetAttributeValue,
    AXUIElementSetMessagingTimeout, AXValueCreate, AXValueGetValue, AXValueRef,
};
use core_foundation::array::CFArray;
use core_foundation::base::{CFRange, CFRelease, CFRetain, CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::runloop::{
    kCFRunLoopCommonModes, kCFRunLoopDefaultMode, CFRunLoop, CFRunLoopSource,
};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::display::CGDisplay;
use core_graphics::event::{CGEvent, CGEventFlags, CGEventType, EventField, KeyCode};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSEventMask, NSFont,
    NSPanel, NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting,
    NSRunningApplication, NSScreen, NSTextField, NSWindowCollectionBehavior, NSWindowStyleMask,
    NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSData, NSDate, NSDefaultRunLoopMode, NSPoint, NSProcessInfo, NSRect, NSSize, NSString,
};
use platform::{
    env_flag_on, AcceptAction, AcceptCallback, AcceptSubscription, AppId, Capabilities,
    CaretCallback, ContextSource, CorrectionRange, Environment, FieldHandle, FocusCallback,
    InsertStrategy, Inserted, KeyInterceptMode, OffsetEncoding, OperatingSystem, OverlayPlacement,
    OverlayPresenter, PlatformAdapter, PlatformError, ScreenRect, SecurityState, ShortcutAction,
    Subscription, TapControl, TextContext, TextRange, Toolkit,
};

pub mod keychain;
mod login_item;
mod settings_window;
mod tray;
mod ui_prompt;
mod url_events;
pub use login_item::set_launch_at_login;
pub use settings_window::{
    keycode_label, keycode_label_with_mods, policy_restore_needed, rebind_request_for,
    record_decision, MacosSettingsWindow, PersonalizationEdit, RebindRequest, RecordDecision,
    RecorderRole, SettingsFlags, APPS_ROWS, APP_POLICY_FIELDS, SETUP_ROWS, STATS_ROWS,
};
pub use tray::{DisableArm, MacosTray, TrayFlags};
pub use ui_prompt::{confirm_deep_link_prompt, confirm_delete_app_prompt, confirm_license_prompt};
pub use url_events::{install_url_event_handler, UrlEventHandler};

const AX_MESSAGING_TIMEOUT_SECONDS: f32 = 0.05;
const AX_WORKER_PUMP_INTERVAL: Duration = Duration::from_millis(5);
const AX_WORKER_RUN_LOOP_SLICE: Duration = Duration::from_millis(1);
const CARET_COALESCE_INTERVAL_MS: u64 = 25;
const CARET_SAFETY_POLL_INTERVAL: Duration = Duration::from_millis(250);
const APP_REBIND_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_USABLE_CARET_RECT_WIDTH: f64 = 2000.0;
const MAX_USABLE_CARET_RECT_HEIGHT: f64 = 200.0;
const AX_SELECTED_TEXT_MARKER_RANGE_ATTRIBUTE: &str = "AXSelectedTextMarkerRange";
const AX_BOUNDS_FOR_TEXT_MARKER_RANGE_PARAMETERIZED_ATTRIBUTE: &str = "AXBoundsForTextMarkerRange";
/// Setting this attribute to true asks a Chromium/Electron application to build
/// its accessibility tree on demand, which is what exposes the
/// `AXSelectedTextMarkerRange` markers the web caret path depends on. WebKit and
/// AppKit ignore it; see `enable_manual_accessibility`.
const AX_MANUAL_ACCESSIBILITY_ATTRIBUTE: &str = "AXManualAccessibility";
const AX_WINDOW_ATTRIBUTE: &str = "AXWindow";
const AX_FRAME_ATTRIBUTE: &str = "AXFrame";
const ESRCH: i32 = 3;
/// Default accept keys, matching Cotypist: Tab accepts the next word
/// (partial), the grave/backtick key above Tab accepts the whole completion.
const KEYCODE_TAB: i64 = 48;
const KEYCODE_GRAVE: i64 = 50;
/// Escape: dismisses the showing ghost and suppresses completions in the field
/// until refocus/edit (Cotypist parity).
const KEYCODE_ESCAPE: i64 = 53;
/// Down arrow: rotate to the next candidate while a suggestion is visible
/// (multi-candidate cycle).
const KEYCODE_DOWN: i64 = 125;
pub type KeyWithMods = (i64, u32);
pub type EffectiveAcceptKeys = (KeyWithMods, KeyWithMods, Option<KeyWithMods>);
const SYNTHETIC_EVENT_TAG: i64 = 0x636d706c746d65;
const CLIPBOARD_RESTORE_DELAY: Duration = Duration::from_millis(1000);
const K_EVENT_CLASS_KEYBOARD: OSType = u32::from_be_bytes(*b"keyb");
const K_EVENT_HOTKEY_PRESSED: u32 = 5;
const K_EVENT_PARAM_DIRECT_OBJECT: OSType = u32::from_be_bytes(*b"----");
const TYPE_EVENT_HOTKEY_ID: OSType = u32::from_be_bytes(*b"hkid");
const HOTKEY_SIGNATURE: OSType = u32::from_be_bytes(*b"cmAK");
const CARBON_HOTKEY_TAB: u32 = 1;
const CARBON_HOTKEY_GRAVE: u32 = 2;
const CARBON_HOTKEY_ESCAPE: u32 = 3;
const CARBON_HOTKEY_DOWN: u32 = 4;
// Always-on (global) shortcut ids. Disjoint from the accept-key ids above so the
// single Carbon hotkey handler can route every fired id unambiguously — an
// accept-key id resolves via `binding_for_hotkey_id`, a shortcut id via
// `shortcut_action_for_hotkey_id`.
const CARBON_HOTKEY_FORCE_ACTIVATE: u32 = 5;
const CARBON_HOTKEY_TOGGLE_APP: u32 = 6;
const CARBON_HOTKEY_TOGGLE_GLOBAL: u32 = 7;
const CARBON_HOTKEY_GRAMMAR_CHECK: u32 = 8;
const CARBON_HOTKEY_GRAMMAR_ACCEPT: u32 = 9;

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

type Job = Box<dyn FnOnce() -> Box<dyn Any + Send> + Send + 'static>;
type WorkerResource = Box<dyn Any + 'static>;
type ResourceInstaller =
    Box<dyn FnOnce() -> Result<WorkerResource, PlatformError> + Send + 'static>;
type ObserverDispatch = Arc<dyn Fn(ObserverEvent) + Send + Sync + 'static>;
type AdapterObserverInstallerFn = dyn Fn(
        i32,
        ObserverInstallTarget,
        Vec<ObserverNotification>,
        ObserverDispatch,
    ) -> Result<ObserverResource, PlatformError>
    + Send
    + Sync
    + 'static;
type FrontmostPidProvider = dyn Fn() -> Option<i32> + Send + Sync + 'static;
type NowMsProvider = dyn Fn() -> u64 + Send + Sync + 'static;
type SecureInputProvider = dyn Fn() -> bool + Send + Sync + 'static;
type ProcessExistsProvider = dyn Fn(i32) -> bool + Send + Sync + 'static;
type SyntheticKeyPoster = dyn Fn(i32, &str) -> Result<(), PlatformError> + Send + Sync + 'static;
type PasteboardPoster = dyn Fn(i32, &str) -> Result<(), PlatformError> + Send + Sync + 'static;
type BackspacePoster = dyn Fn(i32, usize) -> Result<(), PlatformError> + Send + Sync + 'static;
type AcceptTapHandler = dyn Fn(AcceptTapEvent) -> AcceptTapDecision + Send + Sync + 'static;
type AcceptTapInstallerFn = dyn Fn(AcceptTapKind, Arc<AcceptTapHandler>) -> Result<AcceptTapResource, PlatformError>
    + Send
    + Sync
    + 'static;
type OSStatus = i32;
type OSType = u32;
type EventTargetRef = *mut c_void;
type EventHotKeyRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventRef = *mut c_void;
type EventHandlerUPP = extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus;

static SECURE_INPUT_QUERY_LOCK: Mutex<()> = Mutex::new(());

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn IsSecureEventInputEnabled() -> c_uchar;
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn RegisterEventHotKey(
        in_hot_key_code: u32,
        in_hot_key_modifiers: u32,
        in_hot_key_id: EventHotKeyID,
        in_target: EventTargetRef,
        in_options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OSStatus;
    fn UnregisterEventHotKey(in_hot_key: EventHotKeyRef) -> OSStatus;
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

// Linked so the Vision OCR classes (VNImageRequestHandler / VNRecognizeTextRequest)
// resolve at runtime; the calls go through objc2 `msg_send!`.
#[link(name = "Vision", kind = "framework")]
extern "C" {}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    /// Whether this process already has Screen Recording permission (no prompt).
    fn CGPreflightScreenCaptureAccess() -> bool;
    /// Request Screen Recording permission, firing the system prompt if needed.
    fn CGRequestScreenCaptureAccess() -> bool;
    fn CGMainDisplayID() -> u32;
    /// Snapshot the display as a `CGImageRef` (+1; release with `CFRelease`).
    fn CGDisplayCreateImage(display: u32) -> *mut c_void;
}

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
    fn __error() -> *mut i32;
}

enum CallbackMessage {
    Dispatch {
        dispatch: ObserverDispatch,
        event: ObserverEvent,
    },
    Accept {
        callback: AcceptCallback,
        control: TapControl,
    },
    Stop,
}

enum Message {
    Run {
        job: Job,
        reply: mpsc::Sender<Box<dyn Any + Send>>,
    },
    InstallResource {
        id: u64,
        install: ResourceInstaller,
        reply: mpsc::Sender<Result<(), PlatformError>>,
    },
    RemoveResource {
        id: u64,
        reply: Option<mpsc::Sender<bool>>,
    },
    ObserverEvent {
        pid: i32,
        notification: ObserverNotification,
        retained_element: Option<usize>,
        fallback_element_id: String,
        dispatch: ObserverDispatch,
        callback_tx: mpsc::Sender<CallbackMessage>,
    },
    PollFocusedElement {
        pid: i32,
        notification: ObserverNotification,
        dispatch: ObserverDispatch,
        callback_tx: mpsc::Sender<CallbackMessage>,
    },
    #[cfg(test)]
    ResourceCount {
        reply: mpsc::Sender<usize>,
    },
    Stop,
}

trait AxWorkerLoop: Send + 'static {
    fn recv(&mut self) -> Result<Message, mpsc::RecvTimeoutError>;
    fn pump_run_loop(&mut self);
}

struct ChannelAxWorkerLoop {
    rx: mpsc::Receiver<Message>,
    pump_interval: Duration,
}

impl ChannelAxWorkerLoop {
    fn new(rx: mpsc::Receiver<Message>) -> Self {
        Self {
            rx,
            pump_interval: AX_WORKER_PUMP_INTERVAL,
        }
    }
}

impl AxWorkerLoop for ChannelAxWorkerLoop {
    fn recv(&mut self) -> Result<Message, mpsc::RecvTimeoutError> {
        self.rx.recv_timeout(self.pump_interval)
    }

    fn pump_run_loop(&mut self) {
        let _ = CFRunLoop::run_in_mode(
            unsafe { kCFRunLoopDefaultMode },
            AX_WORKER_RUN_LOOP_SLICE,
            true,
        );
    }
}

pub struct AxWorker {
    tx: mpsc::Sender<Message>,
    thread_id: ThreadId,
    handle: Option<JoinHandle<()>>,
    next_resource_id: Arc<AtomicU64>,
}

#[derive(Clone)]
struct AxWorkerHandle {
    tx: mpsc::Sender<Message>,
    next_resource_id: Arc<AtomicU64>,
}

#[derive(Debug)]
pub struct AxWorkerResource {
    id: u64,
    tx: mpsc::Sender<Message>,
    closed: bool,
}

#[derive(Debug)]
pub struct CallbackDispatcher {
    tx: mpsc::Sender<CallbackMessage>,
    handle: Option<JoinHandle<()>>,
}

pub struct MacosPlatformAdapter {
    worker: AxWorker,
    callback_dispatcher: CallbackDispatcher,
    next_subscription_id: AtomicU64,
    subscriptions: Arc<Mutex<HashMap<u64, SubscriptionEntry>>>,
    frontmost_pid: Arc<FrontmostPidProvider>,
    now_ms: Arc<NowMsProvider>,
    secure_input_enabled: Arc<SecureInputProvider>,
    process_exists: Arc<ProcessExistsProvider>,
    synthetic_key_poster: Arc<SyntheticKeyPoster>,
    pasteboard_poster: Arc<PasteboardPoster>,
    backspace_poster: Arc<BackspacePoster>,
    observer_installer: AdapterObserverInstaller,
    accept_tap_installer: AdapterAcceptTapInstaller,
}

pub struct MacosOverlayPresenter {
    panel: Option<Retained<NSPanel>>,
    label: Option<Retained<NSTextField>>,
    underline_panel: Option<Retained<NSPanel>>,
    last_rect: Option<ScreenRect>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MacosOverlayDiagnostics {
    pub has_panel: bool,
    pub visible: bool,
    pub ignores_mouse_events: bool,
    pub nonactivating_panel: bool,
    pub can_become_key_window: bool,
    pub level: isize,
    /// §12 collection behavior: the ghost must join all Spaces and act as a
    /// full-screen auxiliary so it survives Space switches / full-screen apps.
    pub joins_all_spaces: bool,
    pub fullscreen_auxiliary: bool,
    /// Cocoa-space panel frame last exposed to AppKit; acceptance diagnostics
    /// use this to pin that the ghost is anchored near the caret, not just
    /// that an NSPanel exists.
    pub panel_frame: Option<ScreenRect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacosCaretRectSource {
    Marker,
    NativeFallback,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MacosCaretDiagnostics {
    pub marker_rect: Option<ScreenRect>,
    pub native_rect: Option<ScreenRect>,
    pub resolved_rect: Option<ScreenRect>,
    pub source: MacosCaretRectSource,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct OverlayFrame {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

enum SubscriptionEntry {
    Focus {
        _callback: FocusCallback,
        _binding: DynamicObserverBinding,
    },
    Caret {
        _callback: CaretCallback,
        _binding: DynamicObserverBinding,
    },
    Accept {
        _callback: AcceptCallback,
        _observer_tap: AcceptTapResource,
        /// Process-lifetime always-on shortcut registration (ids 5/6/7), held
        /// for the subscription's lifetime so toggles fire with no suggestion
        /// visible (finding C). Dropped on unsubscribe → hotkeys unregistered.
        _shortcut_tap: AcceptTapResource,
        _controller: Arc<AcceptTapController>,
    },
}

struct ObserverResource {
    _inner: Box<dyn Any + Send + 'static>,
}

struct AcceptTapResource {
    _inner: Box<dyn Any + Send + 'static>,
}

impl AcceptTapResource {
    fn new(inner: impl Any + Send + 'static) -> Self {
        Self {
            _inner: Box::new(inner),
        }
    }
}

struct AcceptTapController {
    installer: Arc<AcceptTapInstallerFn>,
    callback_tx: mpsc::Sender<CallbackMessage>,
    callback: AcceptCallback,
    active: Arc<AtomicBool>,
    consumer_tap: Mutex<Option<AcceptTapResource>>,
    accept_action: Arc<Mutex<Option<AcceptAction>>>,
    teardown_generation: AtomicU64,
}

struct SafetyPoller {
    stop_tx: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

struct ObserverBinding {
    pid: i32,
    _observer: ObserverResource,
    _poller: SafetyPoller,
}

struct DynamicObserverBinding {
    _rebinder: RebindPoller,
    _current: Arc<Mutex<Option<ObserverBinding>>>,
}

#[derive(Clone)]
struct ObserverBindingConfig {
    installer: Arc<AdapterObserverInstallerFn>,
    worker_tx: mpsc::Sender<Message>,
    target: ObserverInstallTarget,
    notifications: Vec<ObserverNotification>,
    poll_notification: ObserverNotification,
    dispatch: ObserverDispatch,
    callback_tx: mpsc::Sender<CallbackMessage>,
}

struct DynamicObserverBindingConfig {
    initial_pid: i32,
    frontmost_pid: Arc<FrontmostPidProvider>,
    current: Arc<Mutex<Option<ObserverBinding>>>,
    binding: ObserverBindingConfig,
    rebind_interval: Duration,
}

#[cfg(test)]
struct AdapterTestHooks {
    callback_dispatcher: CallbackDispatcher,
    frontmost_pid: Arc<FrontmostPidProvider>,
    now_ms: Arc<NowMsProvider>,
    secure_input_enabled: Arc<SecureInputProvider>,
    process_exists: Arc<ProcessExistsProvider>,
    synthetic_key_poster: Arc<SyntheticKeyPoster>,
    pasteboard_poster: Arc<PasteboardPoster>,
    backspace_poster: Arc<BackspacePoster>,
    observer_installer: Arc<AdapterObserverInstallerFn>,
    accept_tap_installer: Arc<AcceptTapInstallerFn>,
}

struct RebindPoller {
    stop_tx: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObserverInstallTarget {
    App,
    FocusedElementWithAppFallback,
}

impl ObserverResource {
    fn new(inner: impl Any + Send + 'static) -> Self {
        Self {
            _inner: Box::new(inner),
        }
    }
}

impl AcceptTapController {
    fn set_suggestion_visible(&self, visible: bool) -> Result<(), PlatformError> {
        if !self.active.load(Ordering::Acquire) {
            return Ok(());
        }
        self.teardown_generation.fetch_add(1, Ordering::AcqRel);
        let action = if visible {
            Some(
                *self
                    .accept_action
                    .lock()
                    .map_err(|_| PlatformError::CannotComplete {
                        reason: "accept action lock poisoned".into(),
                    })?
                    .get_or_insert(AcceptAction::Full),
            )
        } else {
            None
        };
        self.set_accept_action(action)
    }

    fn set_accept_action(&self, action: Option<AcceptAction>) -> Result<(), PlatformError> {
        {
            let mut accept_action =
                self.accept_action
                    .lock()
                    .map_err(|_| PlatformError::CannotComplete {
                        reason: "accept action lock poisoned".into(),
                    })?;
            *accept_action = action;
        }
        // INVARIANT (audit c121+): this guard is held across the installer
        // call below, which blocks into the AX worker and performs Carbon
        // FFI. Safe because nothing on the worker (or in any callback) ever
        // touches `consumer_tap` — the lock only serializes arm/disarm from
        // the engine side. Do not add worker-side consumer_tap access.
        let mut consumer_tap =
            self.consumer_tap
                .lock()
                .map_err(|_| PlatformError::CannotComplete {
                    reason: "accept tap controller lock poisoned".into(),
                })?;

        match (action.is_some(), consumer_tap.is_some()) {
            (true, false) => {
                let handler = accept_consumer_tap_handler(
                    Arc::clone(&self.active),
                    self.callback_tx.clone(),
                    Arc::clone(&self.callback),
                    Arc::clone(&self.accept_action),
                );
                *consumer_tap = Some((self.installer)(
                    accept_consumer_kind_for_action(action),
                    handler,
                )?);
            }
            (false, true) => {
                *consumer_tap = None;
            }
            _ => {}
        }

        Ok(())
    }

    /// Recorder 5b: live accept-key re-arm. Drops the armed consumer tap
    /// (the proven WorkerAcceptTapResource teardown — UnregisterEventHotKey
    /// per ref + slot disarm, on the AX worker thread) and re-installs it,
    /// so the Carbon registrations re-read the swapped ACCEPT_KEYMAP.
    /// No-op while unarmed: the next arm cycle reads the new map anyway.
    ///
    /// DROP-BEFORE-INSTALL is load-bearing: Esc/Down exist in every keymap,
    /// so installing first would double-register them. The worker queue is
    /// FIFO (RemoveResource lands before InstallResource) and the install
    /// blocks for its reply, so old and new registrations never overlap and
    /// the new keys are live when this returns. The unarmed window between
    /// the two is fail-open: an accept key pressed inside it passes through
    /// to the app as a literal keystroke (single miss, never key-eating).
    ///
    /// Engine-side threads ONLY (the same rule as set_accept_action):
    /// calling from the AX worker would deadlock on its own queue. Does NOT
    /// touch teardown_generation — rearm is not a visibility transition,
    /// and a pending delayed-hide failsafe must stay able to fire.
    fn rearm_consumer_tap(&self) -> Result<(), PlatformError> {
        // Same guard-across-installer invariant as set_accept_action above:
        // nothing on the worker ever touches consumer_tap.
        let mut consumer_tap =
            self.consumer_tap
                .lock()
                .map_err(|_| PlatformError::CannotComplete {
                    reason: "accept tap controller lock poisoned".into(),
                })?;
        if consumer_tap.is_none() {
            return Ok(());
        }
        *consumer_tap = None; // FIFO #1: old hotkeys unregister on the worker
        let handler = accept_consumer_tap_handler(
            Arc::clone(&self.active),
            self.callback_tx.clone(),
            Arc::clone(&self.callback),
            Arc::clone(&self.accept_action),
        );
        // FIFO #2, blocks until live. On Err the tap stays disarmed —
        // fail-open to the user's typing — and self-heals on the next
        // visibility transition (set_accept_action sees (Some, None)).
        let action = *self
            .accept_action
            .lock()
            .map_err(|_| PlatformError::CannotComplete {
                reason: "accept action lock poisoned".into(),
            })?;
        *consumer_tap = Some((self.installer)(
            accept_consumer_kind_for_action(action),
            handler,
        )?);
        Ok(())
    }

    fn clear_accept_action_if_generation(&self, generation: u64) -> Result<(), PlatformError> {
        let mut accept_action =
            self.accept_action
                .lock()
                .map_err(|_| PlatformError::CannotComplete {
                    reason: "accept action lock poisoned".into(),
                })?;
        if self.teardown_generation.load(Ordering::Acquire) == generation {
            *accept_action = None;
        }
        Ok(())
    }

    fn hide_suggestion_after(controller: Arc<Self>, delay: Duration) -> Result<(), PlatformError> {
        if !controller.active.load(Ordering::Acquire) {
            return Ok(());
        }

        let generation = controller
            .teardown_generation
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        if delay.is_zero() {
            return controller.deactivate_if_generation(generation);
        }

        // ponytail: one detached sleeper thread per non-zero-delay hide. This is
        // the failsafe teardown after an accept (a terminal action — not a
        // per-keystroke path), so the spawn rate is low and each thread exits
        // after `delay`; superseded ones no-op via the generation guard. If a
        // future caller ever invokes this per keystroke, replace the spawn with a
        // single reusable timer thread / CFRunLoop timer keyed on
        // teardown_generation.
        thread::spawn(move || {
            thread::sleep(delay);
            let _ = controller.deactivate_if_generation(generation);
        });
        Ok(())
    }

    fn deactivate_if_generation(&self, generation: u64) -> Result<(), PlatformError> {
        if !self.active.load(Ordering::Acquire) {
            return Ok(());
        }
        if self.teardown_generation.load(Ordering::Acquire) != generation {
            return Ok(());
        }

        {
            let mut consumer_tap =
                self.consumer_tap
                    .lock()
                    .map_err(|_| PlatformError::CannotComplete {
                        reason: "accept tap controller lock poisoned".into(),
                    })?;
            if self.teardown_generation.load(Ordering::Acquire) == generation {
                *consumer_tap = None;
            }
        }
        self.clear_accept_action_if_generation(generation)?;
        Ok(())
    }
}

enum AdapterObserverInstaller {
    Worker,
    #[cfg_attr(not(test), allow(dead_code))]
    Custom(Arc<AdapterObserverInstallerFn>),
}

enum AdapterAcceptTapInstaller {
    Worker,
    #[cfg_attr(not(test), allow(dead_code))]
    Custom(Arc<AcceptTapInstallerFn>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AcceptTapKind {
    Observer,
    Consumer,
    CorrectionConsumer,
    /// Process-lifetime always-on shortcut registration (ForceActivate /
    /// ToggleApp / ToggleGlobal, ids 5/6/7). Installed ONCE per subscription
    /// — unlike `Consumer`, it is NOT armed/dropped with each visible
    /// suggestion, so a toggle can fire in its primary no-suggestion state.
    Shortcut,
}

fn accept_consumer_kind_for_action(action: Option<AcceptAction>) -> AcceptTapKind {
    if action == Some(AcceptAction::Correction) {
        AcceptTapKind::CorrectionConsumer
    } else {
        AcceptTapKind::Consumer
    }
}

#[derive(Clone, Copy, Debug)]
struct AcceptTapEvent {
    event_type: CGEventType,
    keycode: i64,
    source_user_data: i64,
    /// Whether the Option (Alternate) modifier is held — Option+Tab is a
    /// literal-Tab bypass.
    option_down: bool,
    /// The accept role resolved from the fired Carbon hotkey *id*, when the
    /// producer knows it. The id identifies the role unambiguously even when two
    /// roles share a keycode (Tab vs Shift+Tab), so the decision prefers this
    /// over re-deriving the role by keycode. `None` → fall back to the keycode
    /// map (the keycode-based decision tests, and any non-id producer).
    binding: Option<AcceptBinding>,
    /// Set when the fired Carbon id is an always-on (global) shortcut, not an
    /// accept key. `Some(action)` short-circuits the decision straight to
    /// [`AcceptTapDecision::Shortcut`]; `None` is the accept-key path.
    shortcut: Option<ShortcutAction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AcceptTapDecision {
    Keep,
    Drop(AcceptAction),
    /// Consume the key and route a dismiss+suppress to the engine (Esc).
    DropDismiss,
    /// Consume the key and route a candidate-cycle to the engine (Down arrow).
    DropCycle,
    ReenableAndKeep,
    /// An always-on (global) shortcut fired — deliver the action to the app
    /// (re-show pending suggestion / toggle). Acts regardless of accept state.
    Shortcut(ShortcutAction),
}

impl std::fmt::Debug for MacosPlatformAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacosPlatformAdapter")
            .field("worker", &self.worker)
            .finish_non_exhaustive()
    }
}

/// One-time `finishLaunching` guard for [`pump_app_events`].
static APP_FINISH_LAUNCHING: Once = Once::new();

/// Drain pending AppKit/window-server events without blocking.
///
/// The product binary paces its own heartbeat loop with `CFRunLoopRunInMode`
/// instead of `[NSApp run]` — and a plain CFRunLoop pump services run-loop
/// sources but never DEQUEUES window-server events from the application event
/// queue. Carbon dispatches `RegisterEventHotKey` presses to the installed
/// handler during event dequeue, so the accept hotkeys registered fine but the
/// handler never fired on a physical key (observed live in step-6: four
/// registrations per arm cycle, zero fires). Draining here each heartbeat makes
/// hotkey presses — and any other queued AppKit events — actually dispatch.
/// No-op when called off the main thread.
pub fn pump_app_events() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    APP_FINISH_LAUNCHING.call_once(|| app.finishLaunching());
    let distant_past = NSDate::distantPast();
    loop {
        let event = app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any,
            Some(&distant_past),
            unsafe { NSDefaultRunLoopMode },
            true,
        );
        let Some(event) = event else { break };
        app.sendEvent(&event);
    }
}

impl MacosOverlayPresenter {
    pub fn new() -> Result<Self, PlatformError> {
        let mtm = overlay_main_thread_marker()?;
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        Ok(Self {
            panel: None,
            label: None,
            underline_panel: None,
            last_rect: None,
        })
    }

    fn ensure_panel(
        &mut self,
        mtm: MainThreadMarker,
        frame: OverlayFrame,
        text: &str,
    ) -> Result<(), PlatformError> {
        if self.panel.is_some() && self.label.is_some() {
            return Ok(());
        }

        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let panel: Retained<NSPanel> = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            ns_rect(frame),
            style,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setLevel(101);
        panel.setIgnoresMouseEvents(true);
        panel.setHidesOnDeactivate(false);
        // §12: the ghost overlay must follow the user across Spaces and render
        // over full-screen apps. A high window level only controls z-order
        // within the current Space, so CanJoinAllSpaces|FullScreenAuxiliary is
        // required — without it the ghost vanishes on a Space switch and never
        // shows over a full-screen Space.
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );

        let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
        configure_overlay_label(&label, frame, text);
        if let Some(content) = panel.contentView() {
            content.addSubview(&label);
        } else {
            return Err(PlatformError::CannotComplete {
                reason: "overlay panel had no content view".into(),
            });
        }

        self.panel = Some(panel);
        self.label = Some(label);
        Ok(())
    }

    fn ensure_underline_panel(
        &mut self,
        mtm: MainThreadMarker,
        frame: OverlayFrame,
    ) -> Result<(), PlatformError> {
        if self.underline_panel.is_some() {
            return Ok(());
        }

        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let panel: Retained<NSPanel> = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            ns_rect(frame),
            style,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::colorWithWhite_alpha(0.45, 0.9)));
        panel.setLevel(101);
        panel.setIgnoresMouseEvents(true);
        panel.setHidesOnDeactivate(false);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );

        self.underline_panel = Some(panel);
        Ok(())
    }

    pub fn diagnostics_for_acceptance(&self) -> MacosOverlayDiagnostics {
        let Some(panel) = &self.panel else {
            return MacosOverlayDiagnostics {
                has_panel: false,
                visible: false,
                ignores_mouse_events: false,
                nonactivating_panel: false,
                can_become_key_window: false,
                level: 0,
                joins_all_spaces: false,
                fullscreen_auxiliary: false,
                panel_frame: None,
            };
        };
        let behavior = panel.collectionBehavior();
        let frame = panel.frame();

        MacosOverlayDiagnostics {
            has_panel: true,
            visible: panel.isVisible(),
            ignores_mouse_events: panel.ignoresMouseEvents(),
            nonactivating_panel: panel
                .styleMask()
                .contains(NSWindowStyleMask::NonactivatingPanel),
            can_become_key_window: panel.canBecomeKeyWindow(),
            level: panel.level(),
            joins_all_spaces: behavior.contains(NSWindowCollectionBehavior::CanJoinAllSpaces),
            fullscreen_auxiliary: behavior
                .contains(NSWindowCollectionBehavior::FullScreenAuxiliary),
            panel_frame: Some(ScreenRect {
                x: frame.origin.x,
                y: frame.origin.y,
                w: frame.size.width,
                h: frame.size.height,
            }),
        }
    }
}

impl OverlayPresenter for MacosOverlayPresenter {
    fn show_ghost(&mut self, rect: ScreenRect, text: &str) -> Result<(), PlatformError> {
        let mtm = overlay_main_thread_marker()?;
        let primary_height = primary_screen_height(mtm);
        let frame = overlay_frame_for_text(rect, text, primary_height);
        if debug_enabled() {
            // Diagnostic for live overlay-placement bugs (ghost vertical
            // alignment): dump the AX caret rect (top-left/Y-down), the primary
            // screen height used for the Y-flip, and the resulting Cocoa
            // (bottom-left/Y-up) window frame. Gated by COMPME_DEBUG.
            eprintln!(
                "compme: ghost text_len={} caret_rect=(x{:.1} y{:.1} w{:.1} h{:.1}) \
                 primary_h={:.1} overlay_frame=(x{:.1} y{:.1} w{:.1} h{:.1})",
                text.len(),
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                primary_height,
                frame.x,
                frame.y,
                frame.w,
                frame.h
            );
        }
        // Only record last_rect once the panel exists: on an ensure_panel error
        // a stale Some(rect) would claim the overlay is shown when it isn't.
        self.ensure_panel(mtm, frame, text)?;
        self.last_rect = Some(rect);
        if let Some(panel) = &self.panel {
            panel.setFrame_display(ns_rect(frame), true);
            panel.orderFrontRegardless();
        }
        if let Some(label) = &self.label {
            configure_overlay_label(label, frame, text);
        }
        if let Some(panel) = &self.underline_panel {
            panel.orderOut(None);
        }
        Ok(())
    }

    fn show_correction(&mut self, rect: ScreenRect, suggestion: &str) -> Result<(), PlatformError> {
        let mtm = overlay_main_thread_marker()?;
        let primary_height = primary_screen_height(mtm);
        let banner = correction_banner_frame_for_word(rect, suggestion, primary_height);
        let underline = correction_underline_frame_for_word(rect, primary_height);
        if debug_enabled() {
            eprintln!(
                "compme: correction suggestion_len={} word_rect=(x{:.1} y{:.1} w{:.1} h{:.1}) \
                 primary_h={:.1} banner=(x{:.1} y{:.1} w{:.1} h{:.1}) \
                 underline=(x{:.1} y{:.1} w{:.1} h{:.1})",
                suggestion.len(),
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                primary_height,
                banner.x,
                banner.y,
                banner.w,
                banner.h,
                underline.x,
                underline.y,
                underline.w,
                underline.h
            );
        }

        self.ensure_panel(mtm, banner, suggestion)?;
        self.ensure_underline_panel(mtm, underline)?;
        self.last_rect = Some(rect);
        if let Some(panel) = &self.panel {
            panel.setFrame_display(ns_rect(banner), true);
            panel.orderFrontRegardless();
        }
        if let Some(label) = &self.label {
            configure_overlay_label(label, banner, suggestion);
        }
        if let Some(panel) = &self.underline_panel {
            panel.setFrame_display(ns_rect(underline), true);
            panel.orderFrontRegardless();
        }
        Ok(())
    }

    fn update_ghost(&mut self, text: &str) -> Result<(), PlatformError> {
        let mtm = overlay_main_thread_marker()?;
        let Some(rect) = self.last_rect else {
            return Err(PlatformError::CannotComplete {
                reason: "cannot update hidden overlay".into(),
            });
        };
        let frame = overlay_frame_for_text(rect, text, primary_screen_height(mtm));
        // Bind panel and label together before mutating either: the panel and
        // label are created as a pair in `ensure_panel`, so a half-present state
        // is unreachable on the live path — but resizing the panel and *then*
        // erroring on a missing label would leave the panel resized while still
        // showing stale text. Check both up front so the update is all-or-nothing.
        let (Some(panel), Some(label)) = (&self.panel, &self.label) else {
            return Err(PlatformError::CannotComplete {
                reason: "cannot update hidden overlay".into(),
            });
        };
        panel.setFrame_display(ns_rect(frame), true);
        configure_overlay_label(label, frame, text);
        Ok(())
    }

    fn hide(&mut self) -> Result<(), PlatformError> {
        let _mtm = overlay_main_thread_marker()?;
        if let Some(panel) = &self.panel {
            panel.orderOut(None);
        }
        if let Some(panel) = &self.underline_panel {
            panel.orderOut(None);
        }
        Ok(())
    }
}

/// True when `COMPME_DEBUG` is enabled — gates verbose live diagnostics
/// (overlay placement, Carbon hotkey registration/fires). Off by default and
/// when set to an explicit off-value (`0`/`false`/`off`/`no`/empty), matching
/// the project's other boolean env vars — so `COMPME_DEBUG=0` silences it.
fn debug_enabled() -> bool {
    env_flag_on(std::env::var_os("COMPME_DEBUG").as_deref())
}

fn overlay_main_thread_marker() -> Result<MainThreadMarker, PlatformError> {
    MainThreadMarker::new().ok_or_else(|| PlatformError::CannotComplete {
        reason: "macOS overlay must be used on the AppKit main thread".into(),
    })
}

/// macOS version as `major.minor.patch` (thread-safe; no main thread needed).
fn macos_version_string() -> String {
    let v = NSProcessInfo::processInfo().operatingSystemVersion();
    format!("{}.{}.{}", v.majorVersion, v.minorVersion, v.patchVersion)
}

/// Height of the primary (menu-bar) screen — the shared origin both the AX
/// (top-left) and Cocoa (bottom-left) global coordinate systems are measured
/// from. Used to flip the caret rect into Cocoa window coordinates.
fn primary_screen_height(mtm: MainThreadMarker) -> f64 {
    NSScreen::screens(mtm)
        .firstObject()
        .map(|screen| screen.frame().size.height)
        .unwrap_or(0.0)
}

/// A real caret rect is a sliver: at most a few px wide, one text line tall.
/// Anything bigger is the app answering the caret query with ELEMENT BOUNDS
/// (live Chrome AXTextField finding, 2026-06-10: rect = 1799×1225 → the
/// line-midpoint flip placed the ghost at y = -429.5, offscreen).
const CARET_MAX_W: f64 = 4.0;
/// Generous: display-size fonts produce tall caret lines (an 80pt line is a
/// real heading — the box cap handles it), while element bounds run to
/// hundreds or thousands of px.
const CARET_MAX_H: f64 = 160.0;
/// Fallback box height when the rect is bounds, not a caret (a default 14pt
/// line hugged: 14 + 4).
const DEGENERATE_BOX_H: f64 = 18.0;

fn overlay_frame_for_text(rect: ScreenRect, text: &str, primary_height: f64) -> OverlayFrame {
    let text_width = (text.chars().count() as f64 * 7.0) + 24.0;
    let w = text_width.clamp(240.0, 720.0);

    let (h, y) = if rect.w > CARET_MAX_W || rect.h > CARET_MAX_H {
        // Degenerate: treat the rect as the focused element's bounds and hug
        // its inside top-left (where the field's text starts) with a default
        // line box. Re-calibrate from a debug log if a real app's text sits
        // elsewhere — same playbook as the step-6 caret calibration.
        let h = DEGENERATE_BOX_H;
        (h, primary_height - rect.y - h)
    } else {
        // HUG the caret line: 2pt pad above and below it. A box noticeably
        // taller than the line (the old 30pt floor over a typical 14pt line)
        // floats the label text off the typed line no matter how the box is
        // anchored, because the label's cell top-aligns its text inside the
        // box (live step-6 finding, two rounds: top-anchored AND line-centered
        // 30pt boxes both looked misaligned).
        let h = (rect.h + 4.0).clamp(16.0, 48.0);
        // AX gives a top-left-origin (Y-down) global rect; Cocoa windows use a
        // bottom-left-origin (Y-up) global space sharing the primary screen's
        // corner. Flip against the primary height so the overlay lands at the
        // caret on any display, centering the box on the caret line's vertical
        // midpoint. LIVE-CALIBRATED (step-6 screenshot + debug log): the AX
        // caret rect's bottom edge (rect.y + rect.h) is the caret line's TOP —
        // treating rect.y as the line top rendered the ghost exactly one line
        // high on every line — so the line's midpoint is rect.y + 1.5*rect.h.
        (h, primary_height - rect.y - 1.5 * rect.h - h / 2.0)
    };

    // NO blanket onscreen clamp: in Cocoa's global space a display BELOW the
    // primary has legitimately negative y, so clamping would break
    // multi-display placement (the existing secondary-display test pins this).
    // The degenerate branch above is what keeps the known bad case onscreen:
    // an element-bounds position is inside a visible element.
    OverlayFrame { x: rect.x, y, w, h }
}

fn correction_banner_frame_for_word(
    rect: ScreenRect,
    text: &str,
    primary_height: f64,
) -> OverlayFrame {
    let text_width = (text.chars().count() as f64 * 7.0) + 24.0;
    let w = text_width.clamp(96.0, 480.0).max(rect.w);
    let h = (rect.h + 8.0).clamp(20.0, 52.0);
    OverlayFrame {
        x: rect.x,
        y: primary_height - rect.y + 4.0,
        w,
        h,
    }
}

fn correction_underline_frame_for_word(rect: ScreenRect, primary_height: f64) -> OverlayFrame {
    OverlayFrame {
        x: rect.x,
        y: primary_height - rect.y - rect.h - 2.0,
        w: rect.w.max(8.0),
        h: 2.0,
    }
}

fn overlay_label_frame(frame: OverlayFrame) -> OverlayFrame {
    // 2pt insets all around: the box starts at the caret x and hugs the line,
    // so the label hugs the box — the old 8pt horizontal inset showed as a
    // visible gap between the typed word and the ghost (live step-6 finding).
    OverlayFrame {
        x: 2.0,
        y: 2.0,
        w: (frame.w - 4.0).max(1.0),
        h: (frame.h - 4.0).max(1.0),
    }
}

/// Ghost label font size for a given overlay box height: the box hugs the
/// caret line (`line height + 4`), so `box height - 6` tracks the field's
/// visual text size (a 14pt TextEdit line → 18pt box → 12pt font, TextEdit's
/// default body size) instead of the fixed 13pt label default. Clamped to a
/// legible floor and a sane cap for tall (clamped-48) boxes.
fn overlay_font_size(frame_h: f64) -> f64 {
    (frame_h - 6.0).clamp(9.0, 28.0)
}

fn ns_rect(frame: OverlayFrame) -> NSRect {
    NSRect::new(
        NSPoint::new(frame.x, frame.y),
        NSSize::new(frame.w, frame.h),
    )
}

fn configure_overlay_label(label: &NSTextField, frame: OverlayFrame, text: &str) {
    label.setFrame(ns_rect(overlay_label_frame(frame)));
    label.setStringValue(&NSString::from_str(text));
    label.setFont(Some(&NSFont::systemFontOfSize(overlay_font_size(frame.h))));
    label.setTextColor(Some(&NSColor::colorWithWhite_alpha(0.5, 0.9)));
    label.setDrawsBackground(false);
    label.setBezeled(false);
    label.setEditable(false);
}

impl MacosPlatformAdapter {
    pub fn new() -> Result<Self, PlatformError> {
        Self::with_worker(AxWorker::new()?)
    }

    /// Shared insert path. `replace_left` (characters to delete left of the caret
    /// before inserting — a replacement) is honored by every strategy: `AxSet`
    /// range-replaces atomically; `SyntheticKeys`/`Clipboard` cannot
    /// read-modify-write a range, so they synthesize `replace_left` backspace
    /// key presses BEFORE posting the text (a failed backspace post aborts the
    /// insert — never insert without deleting first).
    /// `replace_left == 0` is byte-identical to the prior append-only behavior
    /// (the backspace poster is never invoked). The empty-text early return
    /// precedes deletion: nothing is deleted when there is nothing to insert.
    fn insert_impl(
        &self,
        field: &FieldHandle,
        text: &str,
        replace_left: usize,
        strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        if (self.secure_input_enabled)() {
            return Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            });
        }
        if field_has_secure_text_subrole(field) {
            return Err(PlatformError::SecureInput {
                state: SecurityState::SecureField,
            });
        }
        if text.is_empty() {
            return Ok(Inserted {
                bytes: 0,
                chars: 0,
                strategy,
            });
        }

        let field = field.clone();
        let app = field.app.clone();
        let text = text.to_string();
        let pid = field
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .or_else(|| (self.frontmost_pid)())
            .ok_or_else(|| PlatformError::CannotComplete {
                reason: "no pid available for insert".into(),
            })?;

        match strategy {
            InsertStrategy::AxSet => {
                let text_for_worker = text.clone();
                let apply = self.worker.run(move || {
                    insert_for_field(pid, field, text_for_worker, replace_left, strategy)
                })?;
                let result = apply
                    .and_then(|apply| self.finish_axset_insert(pid, apply, &text, replace_left));
                self.map_app_exited(pid, app, result)
            }
            InsertStrategy::SyntheticKeys => {
                self.ensure_global_insert_target(pid)?;
                Self::refuse_non_atomic_replacement(replace_left, strategy)?;
                let result = self
                    .recheck_secure_input()
                    .and_then(|()| self.delete_left_via_backspaces(pid, replace_left))
                    .and_then(|()| (self.synthetic_key_poster)(pid, &text))
                    .map(|()| Inserted {
                        bytes: text.len(),
                        chars: text.chars().count(),
                        strategy,
                    });
                self.map_app_exited(pid, app, result)
            }
            InsertStrategy::Clipboard => {
                self.ensure_global_insert_target(pid)?;
                Self::refuse_non_atomic_replacement(replace_left, strategy)?;
                let result = self
                    .recheck_secure_input()
                    .and_then(|()| self.delete_left_via_backspaces(pid, replace_left))
                    .and_then(|()| (self.pasteboard_poster)(pid, &text))
                    .map(|()| Inserted {
                        bytes: text.len(),
                        chars: text.chars().count(),
                        strategy,
                    });
                self.map_app_exited(pid, app, result)
            }
            other => Err(PlatformError::UnsupportedField {
                reason: format!("macOS insert strategy {other:?} not implemented yet"),
            }),
        }
    }

    pub fn with_worker(worker: AxWorker) -> Result<Self, PlatformError> {
        Ok(Self {
            worker,
            callback_dispatcher: CallbackDispatcher::new()?,
            next_subscription_id: AtomicU64::new(1),
            subscriptions: Arc::new(Mutex::new(HashMap::new())),
            frontmost_pid: Arc::new(frontmost_app_pid),
            now_ms: Arc::new(wall_clock_now_ms),
            secure_input_enabled: Arc::new(macos_secure_input_enabled),
            process_exists: Arc::new(process_exists),
            synthetic_key_poster: Arc::new(post_synthetic_text),
            pasteboard_poster: Arc::new(post_clipboard_text),
            backspace_poster: Arc::new(post_synthetic_backspaces),
            observer_installer: AdapterObserverInstaller::Worker,
            accept_tap_installer: AdapterAcceptTapInstaller::Worker,
        })
    }

    #[doc(hidden)]
    pub fn with_frontmost_pid_override_for_acceptance(pid: i32) -> Result<Self, PlatformError> {
        Self::with_frontmost_pid_provider_for_acceptance(move || Some(pid))
    }

    #[doc(hidden)]
    pub fn with_frontmost_pid_provider_for_acceptance<F>(
        frontmost_pid: F,
    ) -> Result<Self, PlatformError>
    where
        F: Fn() -> Option<i32> + Send + Sync + 'static,
    {
        let mut adapter = Self::new()?;
        adapter.frontmost_pid = Arc::new(frontmost_pid);
        Ok(adapter)
    }

    #[doc(hidden)]
    pub fn caret_diagnostics(
        &self,
        field: &FieldHandle,
    ) -> Result<MacosCaretDiagnostics, PlatformError> {
        if (self.secure_input_enabled)() {
            return Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            });
        }
        if field_has_secure_text_subrole(field) {
            return Err(PlatformError::SecureInput {
                state: SecurityState::SecureField,
            });
        }

        let field = field.clone();
        let app = field.app.clone();
        let pid = field
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .or_else(|| (self.frontmost_pid)())
            .ok_or_else(|| PlatformError::CannotComplete {
                reason: "no pid available for caret diagnostics".into(),
            })?;

        let result = self
            .worker
            .run(move || caret_diagnostics_for_field(pid, field))?;
        self.map_app_exited(pid, app, result)
    }

    #[cfg(test)]
    fn with_worker_test_hooks(worker: AxWorker, hooks: AdapterTestHooks) -> Self {
        let AdapterTestHooks {
            callback_dispatcher,
            frontmost_pid,
            now_ms,
            secure_input_enabled,
            process_exists,
            synthetic_key_poster,
            pasteboard_poster,
            backspace_poster,
            observer_installer,
            accept_tap_installer,
        } = hooks;

        Self {
            worker,
            callback_dispatcher,
            next_subscription_id: AtomicU64::new(1),
            subscriptions: Arc::new(Mutex::new(HashMap::new())),
            frontmost_pid,
            now_ms,
            secure_input_enabled,
            process_exists,
            synthetic_key_poster,
            pasteboard_poster,
            backspace_poster,
            observer_installer: AdapterObserverInstaller::Custom(observer_installer),
            accept_tap_installer: AdapterAcceptTapInstaller::Custom(accept_tap_installer),
        }
    }

    pub fn ax_worker_thread_id(&self) -> ThreadId {
        self.worker.thread_id()
    }

    fn next_subscription(&self) -> u64 {
        self.next_subscription_id.fetch_add(1, Ordering::Relaxed)
    }

    #[cfg(test)]
    fn subscription_count(&self) -> Result<usize, PlatformError> {
        Ok(self
            .subscriptions
            .lock()
            .map_err(|_| PlatformError::CannotComplete {
                reason: "subscription registry lock poisoned".into(),
            })?
            .len())
    }

    /// Like `subscription_count` but recovers a poisoned registry lock instead of
    /// reporting it. Lets a test observe whether the cancel path actually removed
    /// an entry even when the registry mutex is poisoned (the production drop
    /// closure recovers with `into_inner`, so the live count must still be exact).
    #[cfg(test)]
    fn subscription_count_recovering_poison(&self) -> usize {
        self.subscriptions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    fn frontmost_pid(&self) -> Result<i32, PlatformError> {
        (self.frontmost_pid)().ok_or_else(|| PlatformError::CannotComplete {
            reason: "no frontmost application pid".into(),
        })
    }

    /// Complete an AxSet insert from its readback-classified outcome. A
    /// silently ignored plain insert (live: iTerm2 — settable AXValue,
    /// successful set, content untouched) can fall back to synthetic input. A
    /// replacement cannot: without the original token, deleting first on a
    /// global input channel is not all-or-nothing.
    /// Re-query secure-input state immediately before a synthetic key/clipboard
    /// post. The entry guard in `insert_impl` is sampled once; secure input can
    /// turn on in the window between that check and the actual post (a password
    /// prompt focuses mid-insert). Re-checking at the post site keeps the TOCTOU
    /// window as narrow as possible, matching the crate's fail-closed posture.
    fn recheck_secure_input(&self) -> Result<(), PlatformError> {
        if (self.secure_input_enabled)() {
            return Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            });
        }
        Ok(())
    }

    fn finish_axset_insert(
        &self,
        pid: i32,
        apply: AxSetApply,
        text: &str,
        replace_left: usize,
    ) -> Result<Inserted, PlatformError> {
        match apply {
            AxSetApply::Applied(inserted) => Ok(inserted),
            AxSetApply::SilentlyIgnored => {
                if replace_left > 0 {
                    return Err(PlatformError::CannotComplete {
                        reason: "AxSet replacement was ignored; non-atomic fallback refused".into(),
                    });
                }
                if debug_enabled() {
                    eprintln!(
                        "compme: AxSet write silently ignored — falling back to synthetic input"
                    );
                }
                self.recheck_secure_input()?;
                self.ensure_global_insert_target(pid)?;
                (self.synthetic_key_poster)(pid, text).map(|()| Inserted {
                    bytes: text.len(),
                    chars: text.chars().count(),
                    strategy: InsertStrategy::SyntheticKeys,
                })
            }
        }
    }

    /// Deletes `replace_left` characters left of the caret on the global insert
    /// channels by synthesizing backspace presses. No-op (poster never invoked)
    /// when `replace_left == 0`, keeping plain inserts byte-identical.
    fn delete_left_via_backspaces(
        &self,
        pid: i32,
        replace_left: usize,
    ) -> Result<(), PlatformError> {
        if replace_left == 0 {
            return Ok(());
        }
        (self.backspace_poster)(pid, replace_left)
    }

    fn refuse_non_atomic_replacement(
        replace_left: usize,
        strategy: InsertStrategy,
    ) -> Result<(), PlatformError> {
        if replace_left == 0 {
            Ok(())
        } else {
            Err(PlatformError::CannotComplete {
                reason: format!("macOS {strategy:?} replacement is not atomic"),
            })
        }
    }

    fn ensure_global_insert_target(&self, pid: i32) -> Result<(), PlatformError> {
        match (self.frontmost_pid)() {
            Some(frontmost_pid) if frontmost_pid == pid => Ok(()),
            Some(_) => Err(PlatformError::StaleField),
            None => Err(PlatformError::CannotComplete {
                reason: "no frontmost application pid for global insert".into(),
            }),
        }
    }

    fn subscription_handle(&self, id: u64, active: Arc<AtomicBool>) -> Subscription {
        let subscriptions = Arc::downgrade(&self.subscriptions);
        Subscription::with_cancel(id, move || {
            active.store(false, Ordering::Release);
            let removed = subscriptions.upgrade().and_then(|subscriptions| {
                subscriptions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id)
            });
            drop(removed);
        })
    }

    fn observer_installer(&self) -> Arc<AdapterObserverInstallerFn> {
        match &self.observer_installer {
            AdapterObserverInstaller::Worker => {
                let worker = self.worker.handle();
                let callback_tx = self.callback_dispatcher.sender();
                Arc::new(move |pid, target, notifications, dispatch| match target {
                    ObserverInstallTarget::App => worker
                        .install_app_observer(pid, notifications, dispatch, callback_tx.clone())
                        .map(ObserverResource::new),
                    ObserverInstallTarget::FocusedElementWithAppFallback => worker
                        .install_focused_caret_observer(pid, dispatch, callback_tx.clone())
                        .map(ObserverResource::new),
                })
            }
            AdapterObserverInstaller::Custom(install) => Arc::clone(install),
        }
    }

    fn accept_tap_installer(&self) -> Arc<AcceptTapInstallerFn> {
        match &self.accept_tap_installer {
            AdapterAcceptTapInstaller::Worker => {
                let worker = self.worker.handle();
                Arc::new(move |kind, handler| {
                    worker
                        .install_resource(move || install_worker_accept_tap_resource(kind, handler))
                        .map(AcceptTapResource::new)
                })
            }
            AdapterAcceptTapInstaller::Custom(install) => Arc::clone(install),
        }
    }

    fn map_app_exited<T>(
        &self,
        pid: i32,
        app: AppId,
        result: Result<T, PlatformError>,
    ) -> Result<T, PlatformError> {
        match result {
            Err(PlatformError::StaleField) | Err(PlatformError::CannotComplete { .. })
                if !(self.process_exists)(pid) =>
            {
                Err(PlatformError::AppExited { app })
            }
            other => other,
        }
    }
}

impl PlatformAdapter for MacosPlatformAdapter {
    fn environment(&self) -> Environment {
        Environment {
            os: OperatingSystem::Macos,
            version: macos_version_string(),
        }
    }

    fn subscribe_focus(&self, cb: FocusCallback) -> Result<Subscription, PlatformError> {
        let pid = self.frontmost_pid()?;
        let id = self.next_subscription();
        let factory = Arc::new(Mutex::new(FocusTokenFactory::new()));
        let current_identity_key = Arc::new(Mutex::new(None));
        let binding_state = Arc::new(Mutex::new(None));
        let active = Arc::new(AtomicBool::new(true));
        let active_for_dispatch = Arc::clone(&active);
        let cb_for_dispatch = Arc::clone(&cb);
        let current_identity_key_for_dispatch = Arc::clone(&current_identity_key);
        let binding_state_for_dispatch = Arc::clone(&binding_state);
        let dispatch: ObserverDispatch = Arc::new(move |event: ObserverEvent| {
            if event.notification != ObserverNotification::FocusChanged {
                return;
            }
            if !active_for_dispatch.load(Ordering::Acquire) {
                return;
            }
            if current_binding_pid(&binding_state_for_dispatch) != Some(event.pid) {
                return;
            }

            let identity_key = event.identity.stable_field_key().unwrap_or_else(|| {
                format!("pid={}:{}", event.pid, event.identity.field_element_id())
            });
            let Ok(mut current_identity_key) = current_identity_key_for_dispatch.lock() else {
                return;
            };
            if current_identity_key.as_ref() == Some(&identity_key) {
                return;
            }
            *current_identity_key = Some(identity_key);

            let Ok(mut factory) = factory.lock() else {
                return;
            };
            let field = factory.focused_field(
                event.identity.app_id(event.pid),
                event.identity.pid(event.pid),
                event.identity.field_element_id(),
            );
            cb_for_dispatch(field);
        });
        let binding = start_dynamic_observer_binding(DynamicObserverBindingConfig {
            initial_pid: pid,
            frontmost_pid: Arc::clone(&self.frontmost_pid),
            current: Arc::clone(&binding_state),
            binding: ObserverBindingConfig {
                installer: self.observer_installer(),
                worker_tx: self.worker.handle().tx,
                target: ObserverInstallTarget::App,
                notifications: vec![ObserverNotification::FocusChanged],
                poll_notification: ObserverNotification::FocusChanged,
                dispatch: Arc::clone(&dispatch),
                callback_tx: self.callback_dispatcher.sender(),
            },
            rebind_interval: APP_REBIND_POLL_INTERVAL,
        })?;

        self.subscriptions
            .lock()
            .map_err(|_| PlatformError::CannotComplete {
                reason: "subscription registry lock poisoned".into(),
            })?
            .insert(
                id,
                SubscriptionEntry::Focus {
                    _callback: cb,
                    _binding: binding,
                },
            );

        Ok(self.subscription_handle(id, active))
    }

    fn subscribe_caret(&self, cb: CaretCallback) -> Result<Subscription, PlatformError> {
        let pid = self.frontmost_pid()?;
        let id = self.next_subscription();
        let tracker = Arc::new(Mutex::new(CaretFieldTracker::new()));
        let coalescer = Arc::new(Mutex::new(CaretCoalescer::new(CARET_COALESCE_INTERVAL_MS)));
        let now_ms = Arc::clone(&self.now_ms);
        let binding_state = Arc::new(Mutex::new(None));
        let active = Arc::new(AtomicBool::new(true));
        let active_for_dispatch = Arc::clone(&active);
        let cb_for_dispatch = Arc::clone(&cb);
        let binding_state_for_dispatch = Arc::clone(&binding_state);
        let dispatch: ObserverDispatch = Arc::new(move |event: ObserverEvent| {
            if event.notification != ObserverNotification::CaretChanged {
                return;
            }
            if !active_for_dispatch.load(Ordering::Acquire) {
                return;
            }
            if current_binding_pid(&binding_state_for_dispatch) != Some(event.pid) {
                return;
            }

            let Ok(mut tracker) = tracker.lock() else {
                return;
            };
            let field = tracker.field_for_event(event.pid, &event.identity);
            let rect = event.rect;
            let Ok(mut coalescer) = coalescer.lock() else {
                return;
            };
            if let Some((field, rect)) = coalescer.observe((now_ms)(), field, rect) {
                cb_for_dispatch(field, rect);
            }
        });
        let binding = start_dynamic_observer_binding(DynamicObserverBindingConfig {
            initial_pid: pid,
            frontmost_pid: Arc::clone(&self.frontmost_pid),
            current: Arc::clone(&binding_state),
            binding: ObserverBindingConfig {
                installer: self.observer_installer(),
                worker_tx: self.worker.handle().tx,
                target: ObserverInstallTarget::FocusedElementWithAppFallback,
                notifications: vec![ObserverNotification::CaretChanged],
                poll_notification: ObserverNotification::CaretChanged,
                dispatch: Arc::clone(&dispatch),
                callback_tx: self.callback_dispatcher.sender(),
            },
            rebind_interval: APP_REBIND_POLL_INTERVAL,
        })?;

        self.subscriptions
            .lock()
            .map_err(|_| PlatformError::CannotComplete {
                reason: "subscription registry lock poisoned".into(),
            })?
            .insert(
                id,
                SubscriptionEntry::Caret {
                    _callback: cb,
                    _binding: binding,
                },
            );

        Ok(self.subscription_handle(id, active))
    }

    fn subscribe_accept(&self, cb: AcceptCallback) -> Result<AcceptSubscription, PlatformError> {
        let id = self.next_subscription();
        let active = Arc::new(AtomicBool::new(true));
        let installer = self.accept_tap_installer();
        let callback_tx = self.callback_dispatcher.sender();
        let observer_tap = installer(
            AcceptTapKind::Observer,
            accept_observer_tap_handler(Arc::clone(&active)),
        )?;
        let accept_action = Arc::new(Mutex::new(None));
        // Always-on shortcuts (ids 5/6/7) install ONCE here, for the
        // subscription lifetime — NOT armed/dropped with each visible suggestion
        // like the consumer tap (finding C). The delivery handler is the same
        // consumer handler: it already fires shortcuts before its `active`
        // early-return, so a toggle works with no suggestion showing. Accept
        // decisions read `accept_action`, but shortcut events carry their action
        // and bypass it, so sharing the controller's slot is sound.
        let shortcut_tap = installer(
            AcceptTapKind::Shortcut,
            accept_consumer_tap_handler(
                Arc::clone(&active),
                callback_tx.clone(),
                Arc::clone(&cb),
                Arc::clone(&accept_action),
            ),
        )?;
        let controller = Arc::new(AcceptTapController {
            installer,
            callback_tx,
            callback: Arc::clone(&cb),
            active: Arc::clone(&active),
            consumer_tap: Mutex::new(None),
            accept_action,
            teardown_generation: AtomicU64::new(0),
        });

        self.subscriptions
            .lock()
            .map_err(|_| PlatformError::CannotComplete {
                reason: "subscription registry lock poisoned".into(),
            })?
            .insert(
                id,
                SubscriptionEntry::Accept {
                    _callback: cb,
                    _observer_tap: observer_tap,
                    _shortcut_tap: shortcut_tap,
                    _controller: Arc::clone(&controller),
                },
            );

        let subscription = self.subscription_handle(id, active);
        let controller_for_visible = Arc::clone(&controller);
        let controller_for_hide = Arc::clone(&controller);
        let controller_for_action = Arc::clone(&controller);
        let controller_for_rearm = Arc::clone(&controller);
        Ok(AcceptSubscription::new(
            subscription,
            move |visible| controller_for_visible.set_suggestion_visible(visible),
            move |delay| {
                AcceptTapController::hide_suggestion_after(Arc::clone(&controller_for_hide), delay)
            },
            move |action| controller_for_action.set_accept_action(action),
        )
        .with_rearm(move || controller_for_rearm.rearm_consumer_tap()))
    }

    fn front_app(&self) -> Option<AppId> {
        (self.frontmost_pid)().map(|pid| format!("pid:{pid}"))
    }

    fn capabilities(&self, field: &FieldHandle) -> Result<Capabilities, PlatformError> {
        if (self.secure_input_enabled)() {
            return Ok(global_secure_input_capabilities());
        }
        if field_has_secure_text_subrole(field) {
            return Ok(secure_field_capabilities());
        }

        let field = field.clone();
        let app = field.app.clone();
        let pid = field
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .or_else(|| (self.frontmost_pid)())
            .ok_or_else(|| PlatformError::CannotComplete {
                reason: "no pid available for capabilities".into(),
            })?;

        let result = self
            .worker
            .run(move || capabilities_for_field(pid, field))?;
        self.map_app_exited(pid, app, result)
    }

    fn read_context(&self, field: &FieldHandle) -> Result<TextContext, PlatformError> {
        if (self.secure_input_enabled)() {
            return Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            });
        }
        if field_has_secure_text_subrole(field) {
            return Err(PlatformError::SecureInput {
                state: SecurityState::SecureField,
            });
        }

        let field = field.clone();
        let app = field.app.clone();
        let pid = field
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .or_else(|| (self.frontmost_pid)())
            .ok_or_else(|| PlatformError::CannotComplete {
                reason: "no pid available for read_context".into(),
            })?;

        let result = self
            .worker
            .run(move || read_context_for_field(pid, field))?;
        self.map_app_exited(pid, app, result)
    }

    fn caret_rect(&self, field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
        if (self.secure_input_enabled)() {
            return Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            });
        }
        if field_has_secure_text_subrole(field) {
            return Err(PlatformError::SecureInput {
                state: SecurityState::SecureField,
            });
        }

        let field = field.clone();
        let app = field.app.clone();
        let pid = field
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .or_else(|| (self.frontmost_pid)())
            .ok_or_else(|| PlatformError::CannotComplete {
                reason: "no pid available for caret_rect".into(),
            })?;

        let result = self.worker.run(move || caret_rect_for_field(pid, field))?;
        self.map_app_exited(pid, app, result)
    }

    fn focused_page_url(&self, field: &FieldHandle) -> Result<Option<String>, PlatformError> {
        // No secure-input refusal here (unlike read_context): this reads
        // window/web-area METADATA, never field text — and under secure
        // input suggestions are blocked upstream anyway, so the result is
        // moot rather than sensitive.
        let app = field.app.clone();
        let pid = field
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .or_else(|| (self.frontmost_pid)())
            .ok_or_else(|| PlatformError::CannotComplete {
                reason: "no pid available for focused_page_url".into(),
            })?;
        let result = self.worker.run(move || page_url_for_pid(pid))?;
        self.map_app_exited(pid, app, result)
    }

    fn popup_anchor(&self, field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
        if (self.secure_input_enabled)() {
            return Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            });
        }
        if field_has_secure_text_subrole(field) {
            return Err(PlatformError::SecureInput {
                state: SecurityState::SecureField,
            });
        }

        let field = field.clone();
        let app = field.app.clone();
        let pid = field
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .or_else(|| (self.frontmost_pid)())
            .ok_or_else(|| PlatformError::CannotComplete {
                reason: "no pid available for popup_anchor".into(),
            })?;

        let result = self
            .worker
            .run(move || popup_anchor_for_field(pid, field))?;
        self.map_app_exited(pid, app, result)
    }

    fn text_range_rect(
        &self,
        field: &FieldHandle,
        range: CorrectionRange,
    ) -> Result<Option<ScreenRect>, PlatformError> {
        if (self.secure_input_enabled)() {
            return Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            });
        }
        if field_has_secure_text_subrole(field) {
            return Err(PlatformError::SecureInput {
                state: SecurityState::SecureField,
            });
        }

        let field = field.clone();
        let app = field.app.clone();
        let pid = field
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .or_else(|| (self.frontmost_pid)())
            .ok_or_else(|| PlatformError::CannotComplete {
                reason: "no pid available for text_range_rect".into(),
            })?;

        let result = self
            .worker
            .run(move || text_range_rect_for_field(pid, field, range))?;
        self.map_app_exited(pid, app, result)
    }

    fn insert(
        &self,
        field: &FieldHandle,
        text: &str,
        strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        self.insert_impl(field, text, 0, strategy)
    }

    fn insert_replacing(
        &self,
        field: &FieldHandle,
        text: &str,
        replace_left: usize,
        strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        self.insert_impl(field, text, replace_left, strategy)
    }

    fn insert_replacing_range(
        &self,
        field: &FieldHandle,
        text: &str,
        range: CorrectionRange,
        strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        if strategy != InsertStrategy::AxSet {
            return Err(PlatformError::UnsupportedField {
                reason: "range replacement requires AxSet".into(),
            });
        }
        if (self.secure_input_enabled)() {
            return Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            });
        }
        if field_has_secure_text_subrole(field) {
            return Err(PlatformError::SecureInput {
                state: SecurityState::SecureField,
            });
        }

        let field = field.clone();
        let app = field.app.clone();
        let text = text.to_string();
        let pid = field
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .or_else(|| (self.frontmost_pid)())
            .ok_or_else(|| PlatformError::CannotComplete {
                reason: "no pid available for insert_replacing_range".into(),
            })?;

        let result = self
            .worker
            .run(move || insert_range_for_field(pid, field, text, range, strategy))?
            .and_then(|apply| match apply {
                AxSetApply::Applied(inserted) => Ok(inserted),
                AxSetApply::SilentlyIgnored => Err(PlatformError::CannotComplete {
                    reason: "AX range replacement was ignored".into(),
                }),
            });
        self.map_app_exited(pid, app, result)
    }
}

fn frontmost_app_pid() -> Option<i32> {
    let frontmost = NSWorkspace::sharedWorkspace().frontmostApplication()?;
    let pid = frontmost.processIdentifier();
    if pid < 0 {
        None
    } else {
        Some(pid)
    }
}

/// Resolve the bundle identifier (e.g. `com.apple.TextEdit`) for a process id,
/// or `None` if the process is gone or has no bundle id. Used by the app layer
/// to key per-app preferences/personalization on a stable bundle id rather than
/// the volatile `pid:N` field id (A2 §8). `NSRunningApplication` lookups are
/// callable off the main thread.
pub fn bundle_id_for_pid(pid: i32) -> Option<String> {
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
    app.bundleIdentifier().map(|id| id.to_string())
}

fn wall_clock_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn macos_secure_input_enabled() -> bool {
    let _guard = SECURE_INPUT_QUERY_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    unsafe { IsSecureEventInputEnabled() != 0 }
}

/// Whether this process holds the macOS Accessibility (AX) permission.
///
/// Process-global, so it is a free function rather than an adapter method.
pub fn accessibility_trusted() -> bool {
    // SAFETY: `AXIsProcessTrusted` takes no arguments and is always safe to call.
    unsafe { AXIsProcessTrusted() }
}

/// Like [`accessibility_trusted`], but if the permission is missing this fires
/// the system "grant Accessibility" prompt. Returns the current trust state.
pub fn prompt_accessibility_trust() -> bool {
    // SAFETY: `kAXTrustedCheckOptionPrompt` is a Core Foundation extern static
    // CFString; wrapping it under the get rule borrows without taking ownership.
    let key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
    let options =
        CFDictionary::from_CFType_pairs(&[(key.as_CFType(), CFBoolean::true_value().as_CFType())]);
    // SAFETY: passing a valid CFDictionaryRef to the AX trust API.
    unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) }
}

/// Whether macOS global secure input is currently enabled (e.g. a password
/// field has the keyboard). Public wrapper over the Carbon query.
pub fn secure_input_enabled() -> bool {
    macos_secure_input_enabled()
}

/// The general pasteboard's plain-text contents, for opt-in clipboard context
/// (A2 §16). Call on the main thread. `None` when the clipboard holds no string.
pub fn read_pasteboard_text() -> Option<String> {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard
        .stringForType(pasteboard_string_type())
        .map(|value| value.to_string())
}

/// Whether this process has Screen Recording permission (for optional
/// screen-aware/OCR context, A2 §16). No prompt; pure query.
pub fn screen_recording_permission() -> bool {
    // SAFETY: the CG screen-capture access query takes no arguments.
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Request Screen Recording permission, firing the system prompt if it is not
/// already granted. Returns the resulting grant state.
pub fn request_screen_recording_permission() -> bool {
    // SAFETY: the CG screen-capture access request takes no arguments.
    unsafe { CGRequestScreenCaptureAccess() }
}

/// Physical RAM in bytes (`NSProcessInfo.physicalMemory`), for the model
/// picker's RAM-fit label/gate. Thread-safe (no main-thread requirement); the
/// caller floors it to whole GiB via `model_catalog::bytes_to_whole_gb`.
pub fn physical_memory_bytes() -> u64 {
    objc2_foundation::NSProcessInfo::processInfo().physicalMemory()
}

/// Reveal `path` in Finder (the Setup pane's model row). Main-thread only.
pub fn reveal_file_in_finder(path: &std::path::Path) -> Result<(), PlatformError> {
    use objc2_foundation::{NSArray, NSURL};
    if MainThreadMarker::new().is_none() {
        return Err(PlatformError::CannotComplete {
            reason: "reveal requires the main thread".into(),
        });
    }
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let urls = NSArray::from_retained_slice(&[url]);
    NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&urls);
    Ok(())
}

/// Screen-aware context (A2 §16): capture the display containing the focused
/// caret when available (falling back to the main display) and OCR it locally
/// with Vision (`VNRecognizeTextRequest`), returning up to `max_chars` of
/// recognized on-screen text. Returns `None` when Screen Recording is not
/// granted, capture fails, or nothing is recognized — so the caller degrades to
/// field-only context ("works without it"). Local-only; no network, no storage.
pub fn screen_context_text(caret_rect: Option<ScreenRect>, max_chars: usize) -> Option<String> {
    if max_chars == 0 || !screen_recording_permission() {
        return None;
    }
    // SAFETY: standard Vision OCR pipeline via objc2 message sends. Each selector
    // matches its documented signature; `performRequests:error:` is synchronous
    // (no completion handler), and the autoreleased results are read before this
    // scope returns. The handler/request are owned (+1 from alloc/init / new); the
    // captured CGImage is +1 from `CGDisplayCreateImage` and released below.
    unsafe {
        let display_id = caret_rect
            .and_then(display_id_containing_rect)
            .unwrap_or_else(|| CGMainDisplayID());
        let image_ref = CGDisplayCreateImage(display_id);
        if image_ref.is_null() {
            return None;
        }
        let result = screen_ocr_with_image(image_ref, max_chars);
        CFRelease(image_ref as CFTypeRef);
        result
    }
}

fn display_id_containing_rect(rect: ScreenRect) -> Option<u32> {
    let ids = CGDisplay::active_displays().ok()?;
    ids.into_iter().find(|id| {
        let bounds = CGDisplay::new(*id).bounds();
        rect_center_is_inside_bounds(rect, bounds)
    })
}

fn rect_center_is_inside_bounds(rect: ScreenRect, bounds: CGRect) -> bool {
    let center_x = rect.x + rect.w / 2.0;
    let center_y = rect.y + rect.h / 2.0;
    center_x >= bounds.origin.x
        && center_x <= bounds.origin.x + bounds.size.width
        && center_y >= bounds.origin.y
        && center_y <= bounds.origin.y + bounds.size.height
}

/// Run Vision text recognition over a captured `CGImageRef`. Split out so the
/// caller owns the image's lifetime (release after this returns).
///
/// # Safety
/// `image_ref` must be a valid `CGImageRef`.
unsafe fn screen_ocr_with_image(image_ref: *mut c_void, max_chars: usize) -> Option<String> {
    // VNRequestTextRecognitionLevelFast — fast recognition keeps this off-the-critical
    // path call cheap; accurate-level full-display OCR would stall the run loop.
    const RECOGNITION_LEVEL_FAST: isize = 1;
    // Drain the autoreleased Vision/Foundation objects this pipeline creates; the
    // run loop is a manual poll loop with no per-iteration pool, so without this
    // they would accumulate for the process lifetime. The owned `String` result
    // is copied out before the pool drains.
    objc2::rc::autoreleasepool(|_| unsafe {
        let handler_alloc: *mut AnyObject = msg_send![class!(VNImageRequestHandler), alloc];
        let options: *mut AnyObject = msg_send![class!(NSDictionary), dictionary];
        let handler: *mut AnyObject =
            msg_send![handler_alloc, initWithCGImage: image_ref, options: options];
        let handler = Retained::from_raw(handler)?;

        let request: *mut AnyObject = msg_send![class!(VNRecognizeTextRequest), new];
        let request = Retained::from_raw(request)?;
        let _: () = msg_send![&*request, setRecognitionLevel: RECOGNITION_LEVEL_FAST];
        let _: () = msg_send![&*request, setUsesLanguageCorrection: false];

        let requests: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: &*request];
        let mut error: *mut AnyObject = ptr::null_mut();
        let ok: bool = msg_send![&*handler, performRequests: requests, error: &mut error];
        if !ok {
            // Hard Vision failure → treat as no screen context (caller degrades).
            return None;
        }

        let results: *mut AnyObject = msg_send![&*request, results];
        if results.is_null() {
            return None;
        }
        let count: usize = msg_send![results, count];

        // Collect the top candidate string per observation into owned `String`s,
        // then delegate the join/skip/truncate to the pure (testable) helper.
        let mut lines: Vec<String> = Vec::new();
        for index in 0..count {
            let observation: *mut AnyObject = msg_send![results, objectAtIndex: index];
            let candidates: *mut AnyObject = msg_send![observation, topCandidates: 1usize];
            let candidate_count: usize = msg_send![candidates, count];
            if candidate_count == 0 {
                continue;
            }
            let candidate: *mut AnyObject = msg_send![candidates, objectAtIndex: 0usize];
            let string: *mut NSString = msg_send![candidate, string];
            if string.is_null() {
                continue;
            }
            lines.push((*string).to_string());
        }
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        join_and_truncate_lines(&refs, max_chars)
    })
}

/// Join OCR candidate lines into a single space-separated string, skipping
/// blank/whitespace-only lines, and truncate to at most `max_chars` Unicode
/// scalar values. Returns `None` when no non-blank text remains.
///
/// Pure split-out of the join/skip/truncate logic that used to live inlined in
/// the `unsafe` [`screen_ocr_with_image`] loop, so it can be unit-tested without
/// the Vision FFI. Behaviour-preserving: lines are trimmed before joining, the
/// accumulation stops early once `>= max_chars` scalars have accrued, and the
/// final result is truncated on a codepoint boundary (never splitting a scalar).
fn join_and_truncate_lines(lines: &[&str], max_chars: usize) -> Option<String> {
    let mut text = String::new();
    for line in lines {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(trimmed);
        }
        if text.chars().count() >= max_chars {
            break;
        }
    }
    if text.is_empty() {
        None
    } else {
        Some(text.chars().take(max_chars).collect())
    }
}

/// Active displays as `(bounds, backing scale)` pairs, for the Retina/multi-
/// monitor coordinate diagnostic.
pub fn display_scales() -> Vec<(ScreenRect, f64)> {
    display_scale_pairs(&active_display_scales())
}

/// Pure mapping of `DisplayScale`s to `(bounds, scale)` pairs, split out so the
/// field projection is unit-testable without the FFI display query.
fn display_scale_pairs(scales: &[DisplayScale]) -> Vec<(ScreenRect, f64)> {
    scales
        .iter()
        .map(|d| {
            (
                ScreenRect {
                    x: d.bounds.origin.x,
                    y: d.bounds.origin.y,
                    w: d.bounds.size.width,
                    h: d.bounds.size.height,
                },
                d.scale,
            )
        })
        .collect()
}

fn process_exists(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }

    if unsafe { kill(pid, 0) } == 0 {
        return true;
    }

    // SAFETY: __error() returns the thread-local errno pointer (always
    // valid for the calling thread); reading it immediately after kill(2)
    // observes that call's errno.
    unsafe { *__error() != ESRCH }
}

fn post_synthetic_text(pid: i32, text: &str) -> Result<(), PlatformError> {
    let source = CGEventSource::new(CGEventSourceStateID::Private).map_err(|_| {
        PlatformError::CannotComplete {
            reason: "failed to create CGEventSource for synthetic insertion".into(),
        }
    })?;
    let key_down =
        CGEvent::new_keyboard_event(source.clone(), KeyCode::SPACE, true).map_err(|_| {
            PlatformError::CannotComplete {
                reason: "failed to create synthetic key-down event".into(),
            }
        })?;
    key_down.set_string(text);
    let key_up = CGEvent::new_keyboard_event(source, KeyCode::SPACE, false).map_err(|_| {
        PlatformError::CannotComplete {
            reason: "failed to create synthetic key-up event".into(),
        }
    })?;

    tag_synthetic_event(&key_down);
    tag_synthetic_event(&key_up);
    key_down.post_to_pid(pid);
    key_up.post_to_pid(pid);
    Ok(())
}

/// Synthesizes `count` Delete (backspace, keycode 0x33) key presses to `pid`.
/// This is the only way the write-only `SyntheticKeys`/`Clipboard` insert
/// channels can remove the typed token before a replacement insert — they
/// cannot range-replace like `AxSet`.
///
/// `count` is a number of backspace PRESSES: the app deletes one grapheme
/// cluster per press. Callers pass the typed token's char count, which equals
/// the press count for the ASCII shortcodes/words replacements use today; a
/// future ZWJ-sequence token would need a grapheme-aware count.
///
/// All 2N events are created BEFORE any is posted, so a creation failure
/// leaves the field untouched (no partial deletion).
fn post_synthetic_backspaces(pid: i32, count: usize) -> Result<(), PlatformError> {
    let source = CGEventSource::new(CGEventSourceStateID::Private).map_err(|_| {
        PlatformError::CannotComplete {
            reason: "failed to create CGEventSource for synthetic backspaces".into(),
        }
    })?;
    let mut events = Vec::with_capacity(count * 2);
    for _ in 0..count {
        let key_down =
            CGEvent::new_keyboard_event(source.clone(), KeyCode::DELETE, true).map_err(|_| {
                PlatformError::CannotComplete {
                    reason: "failed to create synthetic backspace key-down event".into(),
                }
            })?;
        let key_up =
            CGEvent::new_keyboard_event(source.clone(), KeyCode::DELETE, false).map_err(|_| {
                PlatformError::CannotComplete {
                    reason: "failed to create synthetic backspace key-up event".into(),
                }
            })?;
        tag_synthetic_event(&key_down);
        tag_synthetic_event(&key_up);
        events.push(key_down);
        events.push(key_up);
    }
    for event in events {
        event.post_to_pid(pid);
    }
    Ok(())
}

fn post_clipboard_text(pid: i32, text: &str) -> Result<(), PlatformError> {
    let pasteboard = NSPasteboard::generalPasteboard();
    let string_type = pasteboard_string_type();
    let previous_snapshot = snapshot_pasteboard(&pasteboard);

    pasteboard.clearContents();
    if !pasteboard.setString_forType(&NSString::from_str(text), string_type) {
        restore_pasteboard(&pasteboard, &previous_snapshot);
        return Err(PlatformError::CannotComplete {
            reason: "failed to write completion text to pasteboard".into(),
        });
    }
    let completion_change_count = pasteboard.changeCount();

    let post_result = post_command_v(pid);
    // The Clipboard strategy runs on the AppKit main thread (NSPasteboard is
    // main-thread-only), so this blocking sleep stalls the heartbeat run loop for
    // CLIPBOARD_RESTORE_DELAY. It is the price of a synchronous "paste then
    // restore the user's clipboard" on the only insert strategy left for apps
    // with no settable AXValue/range. ponytail: accepted limitation for that
    // fallback path — upgrade to a deferred run-loop/timer restore (keeping the
    // heartbeat responsive) if a real app makes the 1s stall on accept felt.
    thread::sleep(CLIPBOARD_RESTORE_DELAY);
    restore_pasteboard_if_unchanged(&pasteboard, &previous_snapshot, completion_change_count);
    post_result
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PasteboardSnapshot {
    items: Vec<PasteboardItemSnapshot>,
    fallback_string: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PasteboardItemSnapshot {
    types: Vec<PasteboardTypeSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PasteboardTypeSnapshot {
    type_name: String,
    data: Vec<u8>,
}

fn snapshot_pasteboard(pasteboard: &NSPasteboard) -> PasteboardSnapshot {
    let fallback_string = pasteboard
        .stringForType(pasteboard_string_type())
        .map(|value| value.to_string());
    let items = pasteboard
        .pasteboardItems()
        .map(|items| snapshot_pasteboard_items(&items))
        .unwrap_or_default();

    PasteboardSnapshot {
        items,
        fallback_string,
    }
}

fn snapshot_pasteboard_items(items: &NSArray<NSPasteboardItem>) -> Vec<PasteboardItemSnapshot> {
    items
        .iter()
        .filter_map(|item| {
            let types = item
                .types()
                .iter()
                .filter_map(|pasteboard_type| {
                    item.dataForType(&pasteboard_type)
                        .map(|data| PasteboardTypeSnapshot {
                            type_name: pasteboard_type.to_string(),
                            data: data.to_vec(),
                        })
                })
                .collect::<Vec<_>>();

            (!types.is_empty()).then_some(PasteboardItemSnapshot { types })
        })
        .collect()
}

fn restore_pasteboard(pasteboard: &NSPasteboard, snapshot: &PasteboardSnapshot) {
    pasteboard.clearContents();
    if !snapshot.items.is_empty() && restore_pasteboard_items(pasteboard, &snapshot.items) {
        return;
    }

    restore_pasteboard_string(pasteboard, snapshot.fallback_string.as_deref());
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PasteboardRestoreOutcome {
    Restored,
    SkippedChanged,
}

fn restore_pasteboard_if_unchanged(
    pasteboard: &NSPasteboard,
    snapshot: &PasteboardSnapshot,
    expected_change_count: isize,
) -> PasteboardRestoreOutcome {
    if pasteboard.changeCount() != expected_change_count {
        return PasteboardRestoreOutcome::SkippedChanged;
    }

    restore_pasteboard(pasteboard, snapshot);
    PasteboardRestoreOutcome::Restored
}

fn restore_pasteboard_items(
    pasteboard: &NSPasteboard,
    item_snapshots: &[PasteboardItemSnapshot],
) -> bool {
    let mut items = Vec::with_capacity(item_snapshots.len());
    for item_snapshot in item_snapshots {
        let item = NSPasteboardItem::new();
        if !populate_pasteboard_item(&item, item_snapshot) {
            return false;
        }
        items.push(ProtocolObject::<dyn NSPasteboardWriting>::from_retained(
            item,
        ));
    }

    let item_refs = NSArray::from_retained_slice(&items);
    pasteboard.writeObjects(&item_refs)
}

fn populate_pasteboard_item(
    item: &NSPasteboardItem,
    item_snapshot: &PasteboardItemSnapshot,
) -> bool {
    for type_snapshot in &item_snapshot.types {
        let data = NSData::with_bytes(&type_snapshot.data);
        let pasteboard_type = NSString::from_str(&type_snapshot.type_name);
        if !item.setData_forType(&data, &pasteboard_type) {
            return false;
        }
    }

    true
}

fn restore_pasteboard_string(pasteboard: &NSPasteboard, previous_string: Option<&str>) {
    pasteboard.clearContents();
    if let Some(previous_string) = previous_string {
        pasteboard.setString_forType(
            &NSString::from_str(previous_string),
            pasteboard_string_type(),
        );
    }
}

fn pasteboard_string_type() -> &'static objc2_app_kit::NSPasteboardType {
    // SAFETY: AppKit provides this process-lifetime global pasteboard type constant.
    unsafe { NSPasteboardTypeString }
}

fn post_command_v(pid: i32) -> Result<(), PlatformError> {
    let source = CGEventSource::new(CGEventSourceStateID::Private).map_err(|_| {
        PlatformError::CannotComplete {
            reason: "failed to create CGEventSource for clipboard insertion".into(),
        }
    })?;
    let command_down = CGEvent::new_keyboard_event(source.clone(), KeyCode::COMMAND, true)
        .map_err(|_| PlatformError::CannotComplete {
            reason: "failed to create command key-down event".into(),
        })?;
    let key_down =
        CGEvent::new_keyboard_event(source.clone(), KeyCode::ANSI_V, true).map_err(|_| {
            PlatformError::CannotComplete {
                reason: "failed to create command-v key down event".into(),
            }
        })?;
    let key_up =
        CGEvent::new_keyboard_event(source.clone(), KeyCode::ANSI_V, false).map_err(|_| {
            PlatformError::CannotComplete {
                reason: "failed to create command-v key up event".into(),
            }
        })?;
    let command_up =
        CGEvent::new_keyboard_event(source, KeyCode::COMMAND, false).map_err(|_| {
            PlatformError::CannotComplete {
                reason: "failed to create command key-up event".into(),
            }
        })?;

    command_down.set_flags(CGEventFlags::CGEventFlagCommand);
    key_down.set_flags(CGEventFlags::CGEventFlagCommand);
    key_up.set_flags(CGEventFlags::CGEventFlagCommand);
    command_up.set_flags(CGEventFlags::CGEventFlagNull);
    tag_synthetic_event(&command_down);
    tag_synthetic_event(&key_down);
    tag_synthetic_event(&key_up);
    tag_synthetic_event(&command_up);
    command_down.post_to_pid(pid);
    key_down.post_to_pid(pid);
    key_up.post_to_pid(pid);
    command_up.post_to_pid(pid);
    Ok(())
}

fn tag_synthetic_event(event: &CGEvent) {
    event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, SYNTHETIC_EVENT_TAG);
}

fn should_ignore_event_for_tap(event_source_user_data: i64) -> bool {
    event_source_user_data == SYNTHETIC_EVENT_TAG
}

#[cfg_attr(not(test), allow(dead_code))]
fn is_self_generated_event(event: &CGEvent) -> bool {
    should_ignore_event_for_tap(event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA))
}

fn accept_observer_tap_handler(active: Arc<AtomicBool>) -> Arc<AcceptTapHandler> {
    Arc::new(move |event| {
        if !active.load(Ordering::Acquire) {
            return AcceptTapDecision::Keep;
        }
        accept_tap_decision(&accept_keymap(), AcceptTapKind::Observer, event, None)
    })
}

fn accept_consumer_tap_handler(
    active: Arc<AtomicBool>,
    callback_tx: mpsc::Sender<CallbackMessage>,
    callback: AcceptCallback,
    accept_action: Arc<Mutex<Option<AcceptAction>>>,
) -> Arc<AcceptTapHandler> {
    Arc::new(move |event| {
        // Always-on shortcuts fire even when accept interception is inactive
        // (no suggestion showing) — gate them BEFORE the `active` early-return.
        if event.shortcut.is_none() && !active.load(Ordering::Acquire) {
            return AcceptTapDecision::Keep;
        }

        let action = *accept_action
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let decision =
            accept_tap_decision(&accept_keymap(), AcceptTapKind::Consumer, event, action);
        let control = match decision {
            AcceptTapDecision::Drop(action) => Some(TapControl::Accept(action)),
            AcceptTapDecision::DropDismiss => Some(TapControl::Dismiss),
            AcceptTapDecision::DropCycle => Some(TapControl::Cycle),
            AcceptTapDecision::Shortcut(shortcut) => Some(TapControl::Shortcut(shortcut)),
            _ => None,
        };
        if let Some(control) = control {
            let _ = callback_tx.send(CallbackMessage::Accept {
                callback: Arc::clone(&callback),
                control,
            });
        }
        decision
    })
}

/// The accept binding a physical key maps to (design spec §16 accept-key
/// reconfiguration).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptBinding {
    Word,
    Full,
    GrammarAccept,
    Dismiss,
    Cycle,
}

/// Configurable map from macOS keycode → accept binding. The default matches
/// Cotypist (Tab→next-word, grave/key-above-Tab→full, Esc→dismiss, Down→cycle);
/// users may rebind the two accept keys (word/full). Pure + validated; the
/// `accept_tap_decision` and Carbon registration both consult it, so a rebind is
/// honored everywhere from one source of truth.
///
/// Public so the app's config layer can build a rebound map from
/// `COMPME_ACCEPT_WORD_KEY`/`_FULL_KEY` and thread it in at startup via
/// [`set_accept_keymap_from_config`]. The decision and Carbon registration
/// both read the swappable `ACCEPT_KEYMAP` global (recorder tick 5a); the
/// remaining live-rebind gap is re-ARMING already-registered hotkeys after a
/// swap (see the warning on [`set_accept_keymap`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcceptKeymap {
    word: i64,
    full: i64,
    dismiss: i64,
    cycle: i64,
    grammar_accept: Option<i64>,
    /// Carbon modifier masks for the two rebindable accept keys (modifier-combo
    /// support, slice 1). 0 = bare key (today's behavior). Dismiss/cycle are
    /// fixed bare keys, so they carry no mask.
    word_mods: u32,
    full_mods: u32,
    grammar_accept_mods: u32,
}

/// Carbon event modifier masks — the `modifiers` argument of
/// `RegisterEventHotKey`. Standard Carbon constants; [`parse_accept_key`] maps
/// modifier words onto them and [`format_accept_key`] back, and slice 2's
/// recorder maps NSEvent flags onto the same bits.
const CARBON_CMD_KEY: u32 = 256;
const CARBON_SHIFT_KEY: u32 = 512;
const CARBON_OPTION_KEY: u32 = 2048;
const CARBON_CONTROL_KEY: u32 = 4096;

/// The four Carbon modifier words accepted by [`parse_accept_key`] and emitted
/// by [`format_accept_key`], in ascending bit order (cmd, shift, option,
/// control) so `format` is deterministic. Each canonical word plus its aliases
/// maps to one mask bit.
const ACCEPT_KEY_MODIFIERS: [(&str, u32); 4] = [
    ("cmd", CARBON_CMD_KEY),
    ("shift", CARBON_SHIFT_KEY),
    ("option", CARBON_OPTION_KEY),
    ("control", CARBON_CONTROL_KEY),
];

/// Map one lower-cased modifier word (canonical or alias) to its Carbon bit.
fn accept_key_modifier_bit(word: &str) -> Option<u32> {
    match word {
        "cmd" | "command" | "super" | "meta" | "win" => Some(CARBON_CMD_KEY),
        "shift" => Some(CARBON_SHIFT_KEY),
        "opt" | "option" | "alt" => Some(CARBON_OPTION_KEY),
        "ctrl" | "control" => Some(CARBON_CONTROL_KEY),
        _ => None,
    }
}

/// Parse a persisted accept-key string into `(keycode, Carbon modifier mask)`.
/// Grammar: `+`-separated, case-insensitive — zero or more modifier words
/// (`shift`/`ctrl`/`control`/`opt`/`option`/`alt`/`cmd`/`command`/…) followed by
/// a single non-negative integer keycode. A bare `"96"` yields `(96, 0)` so the
/// pre-modifier config format still reads. Any malformed input (non-numeric or
/// negative keycode, unknown modifier, missing keycode) returns `None`, letting
/// the caller fall soft to the defaults.
pub fn parse_accept_key(raw: &str) -> Option<(i64, u32)> {
    let mut keycode = None;
    let mut mask = 0u32;
    for token in raw.split('+') {
        let token = token.trim();
        if token.is_empty() {
            return None;
        }
        if keycode.is_some() {
            // A token after the keycode (the integer must be last).
            return None;
        }
        if let Ok(code) = token.parse::<i64>() {
            if code < 0 {
                return None;
            }
            keycode = Some(code);
        } else {
            mask |= accept_key_modifier_bit(&token.to_ascii_lowercase())?;
        }
    }
    keycode.map(|code| (code, mask))
}

/// Format a `(keycode, Carbon modifier mask)` pair into the persisted string
/// form — the inverse of [`parse_accept_key`]. Modifiers are emitted in a fixed
/// ascending-bit order so the output is deterministic and round-trips; a zero
/// mask emits the bare keycode (back-compat output).
pub fn format_accept_key(keycode: i64, mask: u32) -> String {
    let mut out = String::new();
    for (word, bit) in ACCEPT_KEY_MODIFIERS {
        if mask & bit != 0 {
            out.push_str(word);
            out.push('+');
        }
    }
    out.push_str(&keycode.to_string());
    out
}

/// The three optional global shortcut hotkeys configurable beyond the accept
/// keys (A3 Shortcuts pane, ROADMAP Tier 3.4): force a completion now, toggle
/// completions for the focused app, and toggle completions globally. Each is
/// `None` until the user binds it — default-off, so no extra global hotkey is
/// registered. Parsed from the persisted `COMPME_FORCE_ACTIVATE_KEY` /
/// `COMPME_TOGGLE_APP_KEY` / `COMPME_TOGGLE_GLOBAL_KEY` config strings via
/// [`parse_accept_key`] (same grammar as the accept keys, so modifier combos
/// like `ctrl+shift+50` work). The Carbon registration of these always-on
/// hotkeys and the recorder rows that edit them are a later FFI tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShortcutBindings {
    pub force_activate: Option<(i64, u32)>,
    pub toggle_app: Option<(i64, u32)>,
    pub toggle_global: Option<(i64, u32)>,
    pub grammar_check: Option<(i64, u32)>,
}

impl ShortcutBindings {
    /// Parse the three optional config strings into `(keycode, mask)` bindings.
    /// A `None`, empty, or malformed string leaves that binding unset, matching
    /// the accept keys' fail-soft-to-default behavior. Collisions are NOT
    /// rejected here — call [`Self::has_internal_collision`] before registering.
    pub fn from_config(
        force_activate: Option<&str>,
        toggle_app: Option<&str>,
        toggle_global: Option<&str>,
        grammar_check: Option<&str>,
    ) -> Self {
        Self {
            force_activate: force_activate.and_then(parse_accept_key),
            toggle_app: toggle_app.and_then(parse_accept_key),
            toggle_global: toggle_global.and_then(parse_accept_key),
            grammar_check: grammar_check.and_then(parse_accept_key),
        }
    }

    /// `true` if any two bound shortcuts share the same `(keycode, mask)` chord.
    /// A single chord can't register two distinct global hotkeys, so the caller
    /// must reject/ignore a colliding set before installing them. Same keycode
    /// with different modifiers is a distinct chord (not a collision).
    pub fn has_internal_collision(&self) -> bool {
        let bound: Vec<(i64, u32)> = [
            self.force_activate,
            self.toggle_app,
            self.toggle_global,
            self.grammar_check,
        ]
        .into_iter()
        .flatten()
        .collect();
        for i in 0..bound.len() {
            for j in (i + 1)..bound.len() {
                if bound[i] == bound[j] {
                    return true;
                }
            }
        }
        false
    }

    /// The bound shortcuts as `(carbon_hotkey_id, keycode, modifiers)` triples for
    /// the Carbon registration loop — unset shortcuts are skipped. Each id is the
    /// action's `CARBON_HOTKEY_*` constant, so the fired hotkey round-trips through
    /// [`shortcut_action_for_hotkey_id`] in the handler. Call
    /// [`Self::has_internal_collision`] first; a colliding set must not be registered.
    pub fn registration_plan(&self) -> Vec<(u32, i64, u32)> {
        [
            (CARBON_HOTKEY_FORCE_ACTIVATE, self.force_activate),
            (CARBON_HOTKEY_TOGGLE_APP, self.toggle_app),
            (CARBON_HOTKEY_TOGGLE_GLOBAL, self.toggle_global),
            (CARBON_HOTKEY_GRAMMAR_CHECK, self.grammar_check),
        ]
        .into_iter()
        .filter_map(|(id, binding)| binding.map(|(keycode, mask)| (id, keycode, mask)))
        .collect()
    }
}

/// Decode a fired Carbon hotkey id into its always-on shortcut action (the
/// shared boundary [`platform::ShortcutAction`] — `ForceActivate` re-shows the
/// current pending suggestion with no fresh inference, `ToggleApp`/`ToggleGlobal`
/// flip suggestions for the focused app / everywhere). Returns `None` for
/// accept-key ids (handled by `binding_for_hotkey_id`) and unknown ids — the
/// shared handler tries both decoders. Mirrors `binding_for_hotkey_id`.
fn shortcut_action_for_hotkey_id(id: u32) -> Option<ShortcutAction> {
    match id {
        CARBON_HOTKEY_FORCE_ACTIVATE => Some(ShortcutAction::ForceActivate),
        CARBON_HOTKEY_TOGGLE_APP => Some(ShortcutAction::ToggleApp),
        CARBON_HOTKEY_TOGGLE_GLOBAL => Some(ShortcutAction::ToggleGlobal),
        CARBON_HOTKEY_GRAMMAR_CHECK => Some(ShortcutAction::GrammarCheck),
        _ => None,
    }
}

/// Map a Carbon modifier mask to its macOS glyph prefix (⌃⌥⇧⌘) for the
/// Shortcuts-pane display label, in the conventional HIG order. Empty for a
/// bare key. Distinct from [`format_accept_key`], which emits persisted words.
fn accept_key_modifier_glyphs(mask: u32) -> String {
    let mut out = String::new();
    for (glyph, bit) in [
        ("\u{2303}", CARBON_CONTROL_KEY), // ⌃ Control
        ("\u{2325}", CARBON_OPTION_KEY),  // ⌥ Option
        ("\u{21e7}", CARBON_SHIFT_KEY),   // ⇧ Shift
        ("\u{2318}", CARBON_CMD_KEY),     // ⌘ Command
    ] {
        if mask & bit != 0 {
            out.push_str(glyph);
        }
    }
    out
}

/// Map an `NSEvent.modifierFlags()` bitmask to the Carbon modifier mask the
/// accept-key stack registers (slice 2 recorder). AppKit reports modifiers in
/// the device-independent HIGH bits; Carbon's `RegisterEventHotKey` wants the
/// LOW bits — this is the translator. Only the four registerable modifiers are
/// kept; every other NS flag (Caps Lock, Fn, numeric pad, device-dependent
/// left/right bits) is ignored so it can never leak a stray Carbon bit.
fn ns_modifier_flags_to_carbon_mask(ns_flags: u64) -> u32 {
    // objc2-app-kit `NSEventModifierFlags` device-independent bit positions.
    const NS_SHIFT: u64 = 1 << 17;
    const NS_CONTROL: u64 = 1 << 18;
    const NS_OPTION: u64 = 1 << 19;
    const NS_COMMAND: u64 = 1 << 20;
    let mut mask = 0u32;
    for (ns_bit, carbon_bit) in [
        (NS_SHIFT, CARBON_SHIFT_KEY),
        (NS_CONTROL, CARBON_CONTROL_KEY),
        (NS_OPTION, CARBON_OPTION_KEY),
        (NS_COMMAND, CARBON_CMD_KEY),
    ] {
        if ns_flags & ns_bit != 0 {
            mask |= carbon_bit;
        }
    }
    mask
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeymapError {
    /// Two bindings would share the same keycode.
    Collision(i64),
    /// A keycode was negative (macOS virtual keycodes are non-negative).
    InvalidKeycode(i64),
}

impl std::fmt::Display for KeymapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeymapError::Collision(keycode) => {
                write!(
                    f,
                    "keymap collision: keycode {keycode} bound more than once"
                )
            }
            KeymapError::InvalidKeycode(keycode) => {
                write!(f, "invalid keycode: {keycode} (must be non-negative)")
            }
        }
    }
}

impl std::error::Error for KeymapError {}

impl Default for AcceptKeymap {
    fn default() -> Self {
        Self {
            word: KEYCODE_TAB,
            full: KEYCODE_GRAVE,
            dismiss: KEYCODE_ESCAPE,
            cycle: KEYCODE_DOWN,
            grammar_accept: None,
            word_mods: 0,
            full_mods: 0,
            grammar_accept_mods: 0,
        }
    }
}

impl AcceptKeymap {
    /// The binding for a keycode, or `None` if the key is unbound.
    pub fn binding_for(&self, keycode: i64) -> Option<AcceptBinding> {
        if keycode == self.word {
            Some(AcceptBinding::Word)
        } else if keycode == self.full {
            Some(AcceptBinding::Full)
        } else if self.grammar_accept == Some(keycode) {
            Some(AcceptBinding::GrammarAccept)
        } else if keycode == self.dismiss {
            Some(AcceptBinding::Dismiss)
        } else if keycode == self.cycle {
            Some(AcceptBinding::Cycle)
        } else {
            None
        }
    }

    /// The Carbon `(hotkey-id, keycode, modifier-mask)` triples to register for
    /// this keymap.
    /// The bindings to REGISTER for one arm cycle: all four, minus any
    /// binding on the bare Tab key when the focused app has per-app
    /// Tab disable (§16) — an unregistered hotkey lets Tab reach the app
    /// untouched, which is the entire point. Pure (no global reads).
    pub fn arm_bindings(&self, suppress_tab: bool) -> Vec<(u32, i64, u32)> {
        self.carbon_bindings()
            .into_iter()
            .filter(|&(_, code, mods)| !(suppress_tab && code == KEYCODE_TAB && mods == 0))
            .collect()
    }

    pub fn arm_bindings_for_action(
        &self,
        action: AcceptAction,
        suppress_tab: bool,
    ) -> Vec<(u32, i64, u32)> {
        match action {
            AcceptAction::Correction => self
                .carbon_bindings()
                .into_iter()
                .filter(|(id, _, _)| *id == CARBON_HOTKEY_GRAMMAR_ACCEPT)
                .collect(),
            AcceptAction::Full | AcceptAction::Word => self
                .arm_bindings(suppress_tab)
                .into_iter()
                .filter(|(id, _, _)| *id != CARBON_HOTKEY_GRAMMAR_ACCEPT)
                .collect(),
        }
    }

    /// The Carbon `(hotkey-id, keycode, modifier-mask)` triples for this keymap.
    /// The mask is 0 for a bare key (the default for all four bindings); the two
    /// rebindable keys can carry a non-zero Carbon modifier mask (slice 1).
    pub fn carbon_bindings(&self) -> Vec<(u32, i64, u32)> {
        let mut bindings = vec![
            (CARBON_HOTKEY_TAB, self.word, self.word_mods),
            (CARBON_HOTKEY_GRAVE, self.full, self.full_mods),
            (CARBON_HOTKEY_ESCAPE, self.dismiss, 0),
            (CARBON_HOTKEY_DOWN, self.cycle, 0),
        ];
        if let Some(grammar_accept) = self.grammar_accept {
            bindings.push((
                CARBON_HOTKEY_GRAMMAR_ACCEPT,
                grammar_accept,
                self.grammar_accept_mods,
            ));
        }
        bindings
    }

    /// The keycode a registered Carbon hotkey id resolves to under this keymap —
    /// the inverse of [`AcceptKeymap::carbon_bindings`], used to translate a fired
    /// hotkey back into the keycode the decision logic expects.
    pub fn keycode_for_hotkey_id(&self, id: u32) -> Option<i64> {
        self.carbon_bindings()
            .iter()
            .find(|(hid, _, _)| *hid == id)
            .map(|&(_, keycode, _)| keycode)
    }

    /// Rebind the two accept keys (word/full) by keycode; `None` keeps the
    /// default for that key. Dismiss (Esc) and cycle (Down) are fixed. Fails if a
    /// keycode is negative, or if any two of the four bindings would collide.
    pub fn from_accept_keys(word: Option<i64>, full: Option<i64>) -> Result<Self, KeymapError> {
        Self::from_accept_keys_with_mods(word, full, 0, 0)
    }

    /// Like [`AcceptKeymap::from_accept_keys`] but the two rebindable keys carry a
    /// Carbon modifier mask (modifier-combo support, slice 1). A binding is
    /// identified by `(keycode, mask)`, so two keys collide only when BOTH match —
    /// Tab (word) and Shift+Tab (full) are distinct and may coexist. `word_mods`/
    /// `full_mods` of 0 reproduce the bare-key behavior exactly. Fails if a keycode
    /// is negative, or if any two of the four bindings share a keycode AND mask.
    pub fn from_accept_keys_with_mods(
        word: Option<i64>,
        full: Option<i64>,
        word_mods: u32,
        full_mods: u32,
    ) -> Result<Self, KeymapError> {
        Self::from_accept_keys_with_mods_and_grammar(word, full, None, word_mods, full_mods, 0)
    }

    pub fn from_accept_keys_with_mods_and_grammar(
        word: Option<i64>,
        full: Option<i64>,
        grammar_accept: Option<i64>,
        word_mods: u32,
        full_mods: u32,
        grammar_accept_mods: u32,
    ) -> Result<Self, KeymapError> {
        let map = Self {
            word: word.unwrap_or(KEYCODE_TAB),
            full: full.unwrap_or(KEYCODE_GRAVE),
            grammar_accept,
            word_mods,
            full_mods,
            grammar_accept_mods,
            ..Self::default()
        };
        let keys = [
            Some(map.word),
            Some(map.full),
            Some(map.dismiss),
            Some(map.cycle),
            map.grammar_accept,
        ];
        if let Some(bad) = keys.into_iter().flatten().find(|&k| k < 0) {
            return Err(KeymapError::InvalidKeycode(bad));
        }
        // Collision is on the full binding identity (keycode, mask), not keycode
        // alone — dismiss/cycle are fixed bare keys so their mask is 0.
        let mut bindings = vec![
            (map.word, map.word_mods),
            (map.full, map.full_mods),
            (map.dismiss, 0u32),
            (map.cycle, 0u32),
        ];
        if let Some(grammar_accept) = map.grammar_accept {
            bindings.push((grammar_accept, map.grammar_accept_mods));
        }
        for i in 0..bindings.len() {
            for j in (i + 1)..bindings.len() {
                if bindings[i] == bindings[j] {
                    return Err(KeymapError::Collision(bindings[i].0));
                }
            }
        }
        Ok(map)
    }
}

fn accept_tap_decision(
    keymap: &AcceptKeymap,
    kind: AcceptTapKind,
    event: AcceptTapEvent,
    action: Option<AcceptAction>,
) -> AcceptTapDecision {
    // Always-on (global) shortcut: the fired Carbon id resolved to a
    // ShortcutAction, so deliver it straight through — these act regardless of
    // accept state (`action`) or the per-app suppression that gates accept keys.
    if let Some(shortcut) = event.shortcut {
        return AcceptTapDecision::Shortcut(shortcut);
    }
    // RESERVED / currently unreachable in production. A `CGEventTap` can be
    // disabled by the OS on timeout or user-input backlog, and the owner is
    // expected to re-enable it. This crate's accept path is Carbon-hotkey
    // based, NOT a `CGEventTap`, so these event types are never delivered here
    // and the production consumer (`accept_consumer_tap_handler`) folds this
    // decision into its `_ => None` arm. The branch + variant are kept for a
    // future real `CGEventTap` integration and are exercised by unit tests
    // (`accept_tap_decision_reenables_*`) so the re-enable contract is pinned.
    if matches!(
        event.event_type,
        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
    ) {
        return AcceptTapDecision::ReenableAndKeep;
    }
    if should_ignore_event_for_tap(event.source_user_data) {
        return AcceptTapDecision::Keep;
    }
    if matches!(
        kind,
        AcceptTapKind::Consumer | AcceptTapKind::CorrectionConsumer
    ) && matches!(event.event_type, CGEventType::KeyDown)
    {
        // Cotypist binding: while a ghost is armed, the keycode picks the action.
        // While a correction is armed, only the dedicated grammar-accept key is
        // consumed; normal Word/Full/Esc/Down keys pass through to the app.
        // Prefer the role resolved from the fired hotkey id (mask-correct when
        // two roles share a keycode); fall back to the keycode map otherwise.
        let binding = event.binding.or_else(|| keymap.binding_for(event.keycode));
        match action {
            Some(AcceptAction::Correction) => {
                if binding == Some(AcceptBinding::GrammarAccept) {
                    return AcceptTapDecision::Drop(AcceptAction::Correction);
                }
            }
            Some(AcceptAction::Full | AcceptAction::Word) => match binding {
                // Option+<word key> is the per-app Tab bypass: pass it through
                // literally (no Word accept, no swallow).
                Some(AcceptBinding::Word) if event.option_down => return AcceptTapDecision::Keep,
                Some(AcceptBinding::Word) => return AcceptTapDecision::Drop(AcceptAction::Word),
                Some(AcceptBinding::Full) => return AcceptTapDecision::Drop(AcceptAction::Full),
                Some(AcceptBinding::Dismiss) => return AcceptTapDecision::DropDismiss,
                Some(AcceptBinding::Cycle) => return AcceptTapDecision::DropCycle,
                Some(AcceptBinding::GrammarAccept) | None => {}
            },
            None => {}
        }
    }

    AcceptTapDecision::Keep
}

/// The swappable target of the process-lifetime Carbon hotkey handler (R2-5
/// structural fix). The Carbon `InstallEventHandler` callback reads THIS slot
/// on every fire instead of a per-arm heap context, so there is no freed
/// memory for a late keypress to dereference: the slot is a `static`, and the
/// `Arc` cloned out of it keeps the engine handler alive for the duration of
/// the call even if a disarm lands concurrently.
///
/// Arms are tagged with a unique id; `disarm` only clears a slot still owned
/// by that id, so an out-of-order `drop` of a previous resource can never
/// silently disarm a newer one.
struct CarbonHandlerSlot {
    slot: Mutex<Option<(u64, Arc<AcceptTapHandler>)>>,
}

impl CarbonHandlerSlot {
    const fn new() -> Self {
        Self {
            slot: Mutex::new(None),
        }
    }

    // All three methods recover a poisoned lock (`into_inner`): `current` runs
    // inside an extern "C" Carbon callback where a panic would unwind across
    // FFI (abort/UB), and the slot state (a plain Option) cannot be left
    // logically inconsistent by whatever panic poisoned it.
    fn arm(&self, id: u64, handler: Arc<AcceptTapHandler>) {
        *self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((id, handler));
    }

    fn disarm(&self, id: u64) {
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.as_ref().is_some_and(|(owner, _)| *owner == id) {
            *slot = None;
        }
    }

    fn current(&self) -> Option<Arc<AcceptTapHandler>> {
        self.slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|(_, handler)| Arc::clone(handler))
    }
}

/// The single process-lifetime slot the Carbon handler reads (R2-5).
static CARBON_HANDLER_SLOT: CarbonHandlerSlot = CarbonHandlerSlot::new();
/// Unique arm ids for [`CARBON_HANDLER_SLOT`] ownership checks.
static CARBON_ARM_ID: AtomicU64 = AtomicU64::new(1);
/// Whether the process-lifetime Carbon handler is installed. A plain flag
/// (not `Once`) so a failed install can be retried on the next arm.
static CARBON_HANDLER_INSTALLED: Mutex<bool> = Mutex::new(false);

struct WorkerAcceptTapResource {
    hotkeys: Vec<EventHotKeyRef>,
    /// This resource's arm id; `drop` disarms only a slot it still owns.
    arm_id: u64,
}

impl Drop for WorkerAcceptTapResource {
    // R2-5 RESOLVED structurally: the Carbon handler is installed once for the
    // process lifetime and reads the static CARBON_HANDLER_SLOT, so teardown
    // only unregisters the hotkey registrations and disarms the slot. A press
    // racing this drop either sees the slot already empty (no-op) or clones
    // the Arc out first and completes against a still-alive handler — there
    // is no freed context to dereference anymore. (Live hotkey re-validation
    // after this restructure is the remaining human step.)
    fn drop(&mut self) {
        for hotkey in self.hotkeys.drain(..) {
            unsafe {
                let _ = UnregisterEventHotKey(hotkey);
            }
        }
        CARBON_HANDLER_SLOT.disarm(self.arm_id);
    }
}

fn install_worker_accept_tap_resource(
    kind: AcceptTapKind,
    handler: Arc<AcceptTapHandler>,
) -> Result<WorkerResource, PlatformError> {
    match kind {
        // The observer tap is a CGEventTap installed elsewhere; the worker-side
        // resource is a no-op placeholder so the subscription owns *something*.
        AcceptTapKind::Observer => Ok(Box::new(()) as WorkerResource),
        // Always-on shortcuts (ids 5/6/7) install ONCE per subscription on their
        // own process-lifetime resource — independent of the per-suggestion
        // consumer arm — so a toggle fires before any suggestion appears.
        AcceptTapKind::Shortcut => install_process_shortcut_hotkeys(handler),
        AcceptTapKind::Consumer => install_carbon_accept_hotkeys(handler, AcceptAction::Full),
        AcceptTapKind::CorrectionConsumer => {
            install_carbon_accept_hotkeys(handler, AcceptAction::Correction)
        }
    }
}

fn install_carbon_accept_hotkeys(
    handler: Arc<AcceptTapHandler>,
    action: AcceptAction,
) -> Result<WorkerResource, PlatformError> {
    let target = unsafe { GetApplicationEventTarget() };
    ensure_carbon_handler_installed(target)?;

    let arm_id = CARBON_ARM_ID.fetch_add(1, Ordering::Relaxed);
    CARBON_HANDLER_SLOT.arm(arm_id, handler);

    let mut resource = WorkerAcceptTapResource {
        hotkeys: Vec::new(),
        arm_id,
    };
    // Accept keys (ids 1-4) ONLY: they matter solely while a suggestion is
    // visible, so they stay tied to this per-arm consumer resource. Always-on
    // shortcuts (ids 5/6/7) are registered once per subscription on the
    // process-lifetime shortcut resource (`install_process_shortcut_hotkeys`),
    // NOT here — registering them per arm cycle left them unregistered in the
    // no-suggestion state, their primary moment (review finding C).
    for (id, keycode, mask) in accept_keymap()
        .arm_bindings_for_action(action, TAB_HOTKEY_SUPPRESSED.load(Ordering::Relaxed))
    {
        resource.register_hotkey(target, id, keycode, mask)?;
    }

    Ok(Box::new(resource) as WorkerResource)
}

/// The swappable delivery handler for the process-lifetime SHORTCUT hotkeys
/// (ids 5/6/7), kept in its OWN slot so always-on shortcuts dispatch even when
/// the accept consumer slot ([`CARBON_HANDLER_SLOT`]) is empty (no suggestion
/// visible). Mirrors [`CarbonHandlerSlot`]'s id-ownership + poison-recovery
/// discipline so an out-of-order teardown can never disarm a newer arm.
static SHORTCUT_HANDLER_SLOT: CarbonHandlerSlot = CarbonHandlerSlot::new();
/// Unique arm ids for [`SHORTCUT_HANDLER_SLOT`] ownership checks.
static SHORTCUT_ARM_ID: AtomicU64 = AtomicU64::new(1);

/// Drop the shortcut chords that collide with a currently-registered accept-key
/// chord (review finding F). Accept keys (ids 1-4) and shortcuts now register on
/// different lifecycles, so a shortcut bound to an accept chord — e.g. Tab(48)
/// or Esc(53) — would hit `eventHotKeyExistsErr` at `RegisterEventHotKey`. We
/// drop the colliding shortcut rather than let one bad binding abort the whole
/// install. Pure (testable): takes the shortcut plan and the accept chords.
fn shortcut_plan_minus_accept_collisions(
    plan: Vec<(u32, i64, u32)>,
    accept_chords: &[(i64, u32)],
) -> Vec<(u32, i64, u32)> {
    plan.into_iter()
        .filter(|&(_, keycode, mask)| !accept_chords.contains(&(keycode, mask)))
        .collect()
}

/// A process-lifetime resource owning the always-on SHORTCUT hotkey
/// registrations (ids 5/6/7) and the [`SHORTCUT_HANDLER_SLOT`] arm. Mirrors
/// [`WorkerAcceptTapResource`]: it owns its [`EventHotKeyRef`]s and, on drop,
/// unregisters each one and disarms only the slot it still owns — so there is
/// no leak and no double-register across subscriptions.
struct WorkerShortcutResource {
    hotkeys: Vec<EventHotKeyRef>,
    arm_id: u64,
}

impl Drop for WorkerShortcutResource {
    fn drop(&mut self) {
        for hotkey in self.hotkeys.drain(..) {
            // SAFETY: each ref came from a successful `RegisterEventHotKey` in
            // `install_process_shortcut_hotkeys` and is unregistered exactly
            // once (drained), mirroring `WorkerAcceptTapResource::drop`.
            unsafe {
                let _ = UnregisterEventHotKey(hotkey);
            }
        }
        SHORTCUT_HANDLER_SLOT.disarm(self.arm_id);
    }
}

impl WorkerShortcutResource {
    fn register_hotkey(
        &mut self,
        target: EventTargetRef,
        id: u32,
        keycode: i64,
        modifiers: u32,
    ) -> Result<(), PlatformError> {
        let keycode = u32::try_from(keycode).map_err(|_| PlatformError::CannotComplete {
            reason: format!("invalid Carbon shortcut keycode: {keycode}"),
        })?;
        let mut hotkey_ref: EventHotKeyRef = ptr::null_mut();
        // SAFETY: Carbon FFI; `hotkey_ref` is written by RegisterEventHotKey on
        // success (status 0) and pushed for the matching UnregisterEventHotKey
        // in `drop`. Same call shape as `WorkerAcceptTapResource::register_hotkey`.
        let status = unsafe {
            RegisterEventHotKey(
                keycode,
                modifiers,
                EventHotKeyID {
                    signature: HOTKEY_SIGNATURE,
                    id,
                },
                target,
                0,
                &mut hotkey_ref,
            )
        };
        if status != 0 {
            return Err(PlatformError::CannotComplete {
                reason: format!("failed to register Carbon shortcut {keycode}: status {status}"),
            });
        }
        self.hotkeys.push(hotkey_ref);
        Ok(())
    }
}

/// Install the always-on shortcut hotkeys (ids 5/6/7) ONCE for the
/// subscription's lifetime (review finding C). Reuses the shared Carbon handler
/// (`ensure_carbon_handler_installed`) — which routes shortcut ids to
/// [`SHORTCUT_HANDLER_SLOT`] — and arms that slot with the supplied delivery
/// handler. Robust against a shortcut chord colliding with an accept-key chord:
/// such a shortcut is dropped up front (finding F) and any residual register
/// error is logged-and-skipped, never `?`-aborting (a bad shortcut binding must
/// not break accept-key interception, which lives on a different resource now).
fn install_process_shortcut_hotkeys(
    handler: Arc<AcceptTapHandler>,
) -> Result<WorkerResource, PlatformError> {
    let target = unsafe { GetApplicationEventTarget() };
    ensure_carbon_handler_installed(target)?;

    let arm_id = SHORTCUT_ARM_ID.fetch_add(1, Ordering::Relaxed);
    SHORTCUT_HANDLER_SLOT.arm(arm_id, handler);

    let mut resource = WorkerShortcutResource {
        hotkeys: Vec::new(),
        arm_id,
    };

    let shortcuts = shortcut_bindings();
    let plan = if shortcuts.has_internal_collision() {
        if debug_enabled() {
            eprintln!("compme: shortcut bindings collide ({shortcuts:?}); skipping registration");
        }
        Vec::new()
    } else {
        let accept_chords: Vec<(i64, u32)> = accept_keymap()
            .carbon_bindings()
            .into_iter()
            .map(|(_, keycode, mask)| (keycode, mask))
            .collect();
        shortcut_plan_minus_accept_collisions(shortcuts.registration_plan(), &accept_chords)
    };

    for (id, keycode, mask) in plan {
        // Log-and-skip on error: a single colliding/invalid shortcut binding
        // must never abort the install (finding F). The cross-check above drops
        // the known accept-key collisions; this guards the residual cases.
        if let Err(err) = resource.register_hotkey(target, id, keycode, mask) {
            if debug_enabled() {
                eprintln!("compme: skipping shortcut hotkey id={id}: {err}");
            }
        }
    }

    Ok(Box::new(resource) as WorkerResource)
}

/// Install the Carbon hotkey handler ONCE for the process lifetime (R2-5).
/// The handler reads [`CARBON_HANDLER_SLOT`] — no per-arm context pointer —
/// and the `EventHandlerRef` is intentionally never removed (it must outlive
/// every possible late keypress). A failed install leaves the flag false so
/// the next arm retries.
fn ensure_carbon_handler_installed(target: EventTargetRef) -> Result<(), PlatformError> {
    // Held across the InstallEventHandler FFI call below — safe because the
    // Carbon callback never touches THIS lock (it reads CARBON_HANDLER_SLOT).
    // Do not add CARBON_HANDLER_SLOT operations inside this critical section.
    let mut installed = CARBON_HANDLER_INSTALLED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if *installed {
        return Ok(());
    }
    let spec = EventTypeSpec {
        event_class: K_EVENT_CLASS_KEYBOARD,
        event_kind: K_EVENT_HOTKEY_PRESSED,
    };
    let mut handler_ref: EventHandlerRef = ptr::null_mut();
    let handler_status = unsafe {
        InstallEventHandler(
            target,
            carbon_accept_hotkey_handler,
            1,
            &spec,
            ptr::null_mut(),
            &mut handler_ref,
        )
    };
    if handler_status != 0 {
        return Err(PlatformError::CannotComplete {
            reason: format!("failed to install Carbon accept-key handler: status {handler_status}"),
        });
    }
    *installed = true;
    Ok(())
}

/// The process-wide accept keymap (cycle-13 design: ONE source so the
/// decision logic, Carbon registration, and the handler's id→keycode inverse
/// can never diverge). RwLock (was OnceLock until c121): the Shortcuts
/// recorder rebinds at runtime — concurrent readers (decision path, Carbon
/// handler's inverse lookup) stay parallel, the rare write is one struct
/// copy. Never-set reads as the default bindings.
static ACCEPT_KEYMAP: std::sync::RwLock<AcceptKeymap> = std::sync::RwLock::new(AcceptKeymap {
    word: KEYCODE_TAB,
    full: KEYCODE_GRAVE,
    dismiss: KEYCODE_ESCAPE,
    cycle: KEYCODE_DOWN,
    grammar_accept: None,
    word_mods: 0,
    full_mods: 0,
    grammar_accept_mods: 0,
});

/// Per-app Tab suppression (§16 tab_disabled): the run loop sets this on
/// every focus change from prefs; the NEXT arm cycle (hotkeys are transient,
/// armed per visible suggestion) registers without the literal-Tab binding.
static TAB_HOTKEY_SUPPRESSED: AtomicBool = AtomicBool::new(false);

/// Set by the run loop on focus changes; read at hotkey arm time.
pub fn set_tab_hotkey_suppressed(suppressed: bool) {
    TAB_HOTKEY_SUPPRESSED.store(suppressed, Ordering::Relaxed);
}

/// Swap the active keymap (live rebind). Write FIRST, re-register hotkeys
/// SECOND — an old hotkey firing between the two reads the new map, which
/// is consistent (the id→keycode→binding round-trip stays within one map,
/// so the fired id resolves to its original ROLE — no mis-action).
///
/// Live swaps (recorder 5b slice 1, 2026-06-12) must pair this with
/// `AcceptSubscription::rearm_accept_tap()` — keymap write first, rearm
/// second — or, while hotkeys are ARMED, the old physical keys stay
/// registered and the new ones do nothing until the next arm cycle (the
/// 2026-06-11 external finding). Startup-before-arm callers need no rearm.
/// Persist a rebind ONLY after the rearm succeeded (config/runtime desync
/// otherwise).
pub fn set_accept_keymap(map: AcceptKeymap) {
    *ACCEPT_KEYMAP
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = map;
}

/// Install the configured keymap. Must run BEFORE the platform adapter arms
/// any accept tap (the run loop does this right after config parse). Returns
/// the validation error on collision/invalid keycodes — callers fail soft to
/// the defaults and log.
pub fn set_accept_keymap_from_config(
    word: Option<i64>,
    full: Option<i64>,
) -> Result<(), KeymapError> {
    // Bare-keycode entry (the live-rebind fn-pointer path still calls this):
    // delegate to the modifier-aware form with a zero mask, so both paths
    // share one validate-then-swap.
    set_accept_keymap_from_config_with_mods(word.map(|k| (k, 0)), full.map(|k| (k, 0)), None)
}

/// Like [`set_accept_keymap_from_config`] but each key carries a Carbon modifier
/// mask (modifier-combo support, slice 1b). Startup config reads
/// `COMPME_ACCEPT_WORD_KEY="shift+48"` into `(keycode, mask)` and lands it here.
/// Validates before swapping (same fail-soft contract): a collision or invalid
/// keycode errors WITHOUT touching the live map.
pub fn set_accept_keymap_from_config_with_mods(
    word: Option<(i64, u32)>,
    full: Option<(i64, u32)>,
    grammar_accept: Option<(i64, u32)>,
) -> Result<(), KeymapError> {
    let map = AcceptKeymap::from_accept_keys_with_mods_and_grammar(
        word.map(|(k, _)| k),
        full.map(|(k, _)| k),
        grammar_accept.map(|(k, _)| k),
        word.map(|(_, m)| m).unwrap_or(0),
        full.map(|(_, m)| m).unwrap_or(0),
        grammar_accept.map(|(_, m)| m).unwrap_or(0),
    )?;
    set_accept_keymap(map);
    Ok(())
}

/// The active always-on (global) shortcut bindings. Mirrors [`ACCEPT_KEYMAP`]:
/// a process-wide `RwLock` so the registration loop reads the latest set, the
/// recorder (a later tick) can rebind, and a never-set read is "no shortcuts".
/// Read at hotkey arm time — the next arm cycle picks up a config/runtime swap.
static SHORTCUT_BINDINGS: std::sync::RwLock<ShortcutBindings> =
    std::sync::RwLock::new(ShortcutBindings {
        force_activate: None,
        toggle_app: None,
        toggle_global: None,
        grammar_check: None,
    });

/// Snapshot the active shortcut bindings (the registration loop's single read).
fn shortcut_bindings() -> ShortcutBindings {
    *SHORTCUT_BINDINGS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Install the configured always-on shortcuts. Mirrors
/// [`set_accept_keymap_from_config_with_mods`]: the run loop parses the three
/// `COMPME_*` shortcut config strings, then lands them here BEFORE the adapter
/// arms its accept tap (the same arm cycle registers these). A colliding set is
/// dropped whole (`has_internal_collision`) — a single chord can't drive two
/// distinct hotkeys, so registering a partial set would be surprising. Returns
/// the parsed bindings for the caller to log/inspect.
pub fn set_shortcut_bindings_from_config(
    force_activate: Option<&str>,
    toggle_app: Option<&str>,
    toggle_global: Option<&str>,
    grammar_check: Option<&str>,
) -> ShortcutBindings {
    let bindings =
        ShortcutBindings::from_config(force_activate, toggle_app, toggle_global, grammar_check);
    let effective = if bindings.has_internal_collision() {
        if debug_enabled() {
            eprintln!(
                "compme: shortcut bindings collide ({bindings:?}); ignoring all three until distinct"
            );
        }
        ShortcutBindings::default()
    } else {
        bindings
    };
    *SHORTCUT_BINDINGS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = effective;
    effective
}

/// The active accept keymap. Single indirection so the three call sites
/// (decision, registration, inverse) always agree.
fn accept_keymap() -> AcceptKeymap {
    *ACCEPT_KEYMAP
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The EFFECTIVE accept keys (word, full) after validation fallback — what
/// the runtime actually registered. The Shortcuts pane renders these, never
/// raw config: a rejected collision falls back to defaults here exactly as
/// it did at registration (review-c114 collision-masquerade fix), and the
/// defaults live in one place (drift fix).
pub fn effective_accept_keys() -> (i64, i64) {
    let map = accept_keymap();
    (map.word, map.full)
}

/// Like [`effective_accept_keys`] but each key carries its Carbon modifier mask
/// (slice 1b label half) — the Shortcuts pane renders the ⌃⌥⇧⌘ glyph prefix
/// from these. Same single source as the registration and decision paths.
pub fn effective_accept_keys_with_mods() -> (KeyWithMods, KeyWithMods) {
    let map = accept_keymap();
    ((map.word, map.word_mods), (map.full, map.full_mods))
}

/// Like [`effective_accept_keys_with_mods`] but also returns the optional
/// grammar-accept binding. The grammar accept key is intentionally optional:
/// an absent binding means the correction accept hotkey is unbound.
pub fn effective_accept_keys_with_mods_and_grammar() -> EffectiveAcceptKeys {
    let map = accept_keymap();
    (
        (map.word, map.word_mods),
        (map.full, map.full_mods),
        map.grammar_accept
            .map(|keycode| (keycode, map.grammar_accept_mods)),
    )
}

impl WorkerAcceptTapResource {
    fn register_hotkey(
        &mut self,
        target: EventTargetRef,
        id: u32,
        keycode: i64,
        modifiers: u32,
    ) -> Result<(), PlatformError> {
        let keycode = u32::try_from(keycode).map_err(|_| PlatformError::CannotComplete {
            reason: format!("invalid Carbon accept-key keycode: {keycode}"),
        })?;
        let mut hotkey_ref: EventHotKeyRef = ptr::null_mut();
        let status = unsafe {
            RegisterEventHotKey(
                keycode,
                modifiers,
                EventHotKeyID {
                    signature: HOTKEY_SIGNATURE,
                    id,
                },
                target,
                0,
                &mut hotkey_ref,
            )
        };
        if status != 0 {
            return Err(PlatformError::CannotComplete {
                reason: format!("failed to register Carbon accept-key {keycode}: status {status}"),
            });
        }
        if debug_enabled() {
            // Live diagnostic: proves which accept keys were actually
            // registered (and on which arm cycle) when a physical press
            // appears to do nothing.
            eprintln!(
                "compme: carbon hotkey registered id={id} keycode={keycode} modifiers={modifiers}"
            );
        }
        self.hotkeys.push(hotkey_ref);
        Ok(())
    }
}

extern "C" fn carbon_accept_hotkey_handler(
    _call: EventHandlerCallRef,
    event: EventRef,
    _user: *mut c_void,
) -> OSStatus {
    // C→Rust FFI boundary: a panic unwinding into Carbon is UB. Shield the whole
    // body (matching the crate's dispatcher convention) and fall back to noErr
    // (0) on panic so Carbon can try other handlers.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut hotkey_id = EventHotKeyID {
            signature: 0,
            id: 0,
        };
        let status = unsafe {
            GetEventParameter(
                event,
                K_EVENT_PARAM_DIRECT_OBJECT,
                TYPE_EVENT_HOTKEY_ID,
                ptr::null_mut(),
                std::mem::size_of::<EventHotKeyID>(),
                ptr::null_mut(),
                (&mut hotkey_id as *mut EventHotKeyID).cast::<c_void>(),
            )
        };
        // noErr == 0. On any failure `hotkey_id` may be uninitialized garbage, so
        // bail rather than act on a bogus signature/id (returning noErr lets Carbon
        // try other handlers).
        if status != 0 {
            return 0;
        }
        if debug_enabled() {
            // Live diagnostic: fires on ANY hotkey event Carbon delivers to us,
            // before the signature/id filters — distinguishes "handler never runs"
            // (registration/dispatch problem) from "handler runs but filters out".
            eprintln!(
                "compme: carbon hotkey fired signature=0x{:x} id={} (ours=0x{:x})",
                hotkey_id.signature, hotkey_id.id, HOTKEY_SIGNATURE
            );
        }
        if hotkey_id.signature != HOTKEY_SIGNATURE {
            return 0;
        }
        // An always-on (global) shortcut id (5..=7) resolves here; it has no
        // accept keycode, so decode it FIRST and deliver a shortcut event. The
        // keycode is irrelevant for shortcuts (the action carries the meaning),
        // so a placeholder 0 is fine — the decision keys off `shortcut`.
        let shortcut = shortcut_action_for_hotkey_id(hotkey_id.id);
        let keycode = match shortcut {
            Some(_) => 0,
            None => {
                let Some(keycode) = carbon_hotkey_keycode(hotkey_id.id) else {
                    return 0;
                };
                keycode
            }
        };
        // Shortcut ids (5/6/7) read the PROCESS-LIFETIME shortcut slot so they
        // dispatch even when no suggestion is visible (the accept slot is empty
        // in that state — finding C). Accept ids read the per-arm accept slot.
        // Either way the cloned Arc keeps the handler alive through this call
        // even if a disarm lands concurrently; an empty slot drops the event.
        let slot = if shortcut.is_some() {
            &SHORTCUT_HANDLER_SLOT
        } else {
            &CARBON_HANDLER_SLOT
        };
        let Some(handler) = slot.current() else {
            return 0;
        };
        let _ = handler(AcceptTapEvent {
            event_type: CGEventType::KeyDown,
            keycode,
            source_user_data: 0,
            option_down: false,
            // The id is the authoritative role source — pass it through so a
            // masked role (e.g. Shift+Tab as Full) resolves to its own action
            // instead of collapsing onto the keycode's first match. `None` for
            // shortcut ids (they carry `shortcut` instead).
            binding: binding_for_hotkey_id(hotkey_id.id),
            shortcut,
        });
        0
    }))
    .unwrap_or(0)
}

fn carbon_hotkey_keycode(id: u32) -> Option<i64> {
    // Derive from the same keymap that drives registration, so the handler's
    // id→keycode translation can never diverge from what was registered.
    accept_keymap().keycode_for_hotkey_id(id)
}

/// The accept role a fired Carbon hotkey *id* maps to — the authoritative,
/// keymap-independent inverse of the registration slots in
/// [`AcceptKeymap::carbon_bindings`]. The id identifies the role directly, so
/// two roles sharing a keycode (Tab vs Shift+Tab) stay distinct at decision
/// time, where a keycode-only lookup would collapse them onto the first match.
fn binding_for_hotkey_id(id: u32) -> Option<AcceptBinding> {
    match id {
        CARBON_HOTKEY_TAB => Some(AcceptBinding::Word),
        CARBON_HOTKEY_GRAVE => Some(AcceptBinding::Full),
        CARBON_HOTKEY_GRAMMAR_ACCEPT => Some(AcceptBinding::GrammarAccept),
        CARBON_HOTKEY_ESCAPE => Some(AcceptBinding::Dismiss),
        CARBON_HOTKEY_DOWN => Some(AcceptBinding::Cycle),
        _ => None,
    }
}

fn field_has_secure_text_subrole(field: &FieldHandle) -> bool {
    field
        .element_id
        .contains(&format!("subrole={kAXSecureTextFieldSubrole}"))
}

fn global_secure_input_capabilities() -> Capabilities {
    blocked_capabilities(SecurityState::SecureInputEnabled)
}

fn secure_field_capabilities() -> Capabilities {
    blocked_capabilities(SecurityState::SecureField)
}

fn blocked_capabilities(security_state: SecurityState) -> Capabilities {
    Capabilities {
        readable_text: false,
        readable_caret: false,
        writable: false,
        secure: true,
        security_state,
        toolkit: Toolkit::Unknown("macOS Accessibility".into()),
        multiline: false,
        insert_strategy: InsertStrategy::None,
        accept_intercept: KeyInterceptMode::None,
        overlay_at_caret: OverlayPlacement::None,
        coords_global_screen: true,
    }
}

impl std::fmt::Debug for AxWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AxWorker")
            .field("thread_id", &self.thread_id)
            .finish_non_exhaustive()
    }
}

impl AxWorker {
    pub fn new() -> Result<Self, PlatformError> {
        Self::start_with_setup(set_ax_messaging_timeout)
    }

    pub fn start_with_setup<F>(setup: F) -> Result<Self, PlatformError>
    where
        F: FnOnce(f32) -> Result<(), PlatformError> + Send + 'static,
    {
        let (tx, rx) = mpsc::channel::<Message>();
        let (started_tx, started_rx) = mpsc::channel::<Result<ThreadId, PlatformError>>();

        let handle = thread::Builder::new()
            .name("compme-ax-worker".into())
            .spawn(move || {
                run_ax_worker_loop(
                    ChannelAxWorkerLoop::new(rx),
                    started_tx,
                    setup,
                    AX_MESSAGING_TIMEOUT_SECONDS,
                );
            })
            .map_err(|err| PlatformError::CannotComplete {
                reason: format!("failed to start AX worker thread: {err}"),
            })?;

        let thread_id = match started_rx
            .recv()
            .map_err(|err| PlatformError::CannotComplete {
                reason: format!("AX worker failed during startup: {err}"),
            })? {
            Ok(thread_id) => thread_id,
            Err(err) => {
                let _ = handle.join();
                return Err(err);
            }
        };

        Ok(Self {
            tx,
            thread_id,
            handle: Some(handle),
            next_resource_id: Arc::new(AtomicU64::new(1)),
        })
    }

    pub fn thread_id(&self) -> ThreadId {
        self.thread_id
    }

    fn handle(&self) -> AxWorkerHandle {
        AxWorkerHandle {
            tx: self.tx.clone(),
            next_resource_id: Arc::clone(&self.next_resource_id),
        }
    }

    pub fn run<F, R>(&self, job: F) -> Result<R, PlatformError>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(Message::Run {
                job: Box::new(move || Box::new(job()) as Box<dyn Any + Send>),
                reply: reply_tx,
            })
            .map_err(|_| PlatformError::CannotComplete {
                reason: "AX worker is not running".into(),
            })?;

        let value = reply_rx.recv().map_err(|_| PlatformError::CannotComplete {
            reason: "AX worker dropped job result".into(),
        })?;

        value
            .downcast::<R>()
            .map(|boxed| *boxed)
            .map_err(|_| PlatformError::CannotComplete {
                reason: "AX worker returned unexpected job result type".into(),
            })
    }

    pub fn install_resource<F>(&self, install: F) -> Result<AxWorkerResource, PlatformError>
    where
        F: FnOnce() -> Result<WorkerResource, PlatformError> + Send + 'static,
    {
        self.handle().install_resource(install)
    }

    #[cfg(test)]
    fn resource_count(&self) -> Result<usize, PlatformError> {
        self.handle().resource_count()
    }
}

impl AxWorkerHandle {
    pub fn install_resource<F>(&self, install: F) -> Result<AxWorkerResource, PlatformError>
    where
        F: FnOnce() -> Result<WorkerResource, PlatformError> + Send + 'static,
    {
        let id = self.next_resource_id.fetch_add(1, Ordering::Relaxed);
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(Message::InstallResource {
                id,
                install: Box::new(install),
                reply: reply_tx,
            })
            .map_err(|_| PlatformError::CannotComplete {
                reason: "AX worker is not running".into(),
            })?;

        reply_rx
            .recv()
            .map_err(|_| PlatformError::CannotComplete {
                reason: "AX worker dropped resource install result".into(),
            })??;

        Ok(AxWorkerResource {
            id,
            tx: self.tx.clone(),
            closed: false,
        })
    }

    #[cfg(test)]
    fn resource_count(&self) -> Result<usize, PlatformError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(Message::ResourceCount { reply: reply_tx })
            .map_err(|_| PlatformError::CannotComplete {
                reason: "AX worker is not running".into(),
            })?;

        reply_rx.recv().map_err(|_| PlatformError::CannotComplete {
            reason: "AX worker dropped resource count result".into(),
        })
    }

    fn install_app_observer(
        &self,
        pid: i32,
        notifications: Vec<ObserverNotification>,
        dispatch: ObserverDispatch,
        callback_tx: mpsc::Sender<CallbackMessage>,
    ) -> Result<AxWorkerResource, PlatformError> {
        let tx = self.tx.clone();
        self.install_resource(move || {
            let (element, element_owner) = create_app_ax_element(pid)?;
            // Wake Chromium/Electron a11y once per focus bind, not per read.
            unsafe { enable_manual_accessibility(element) };
            install_worker_observer_resource(
                tx,
                callback_tx,
                dispatch,
                pid,
                element,
                notifications,
                vec![element_owner],
            )
        })
    }

    fn install_focused_caret_observer(
        &self,
        pid: i32,
        dispatch: ObserverDispatch,
        callback_tx: mpsc::Sender<CallbackMessage>,
    ) -> Result<AxWorkerResource, PlatformError> {
        let tx = self.tx.clone();
        self.install_resource(move || {
            let (app_element, app_owner) = create_app_ax_element(pid)?;
            // Wake Chromium/Electron a11y once per focus bind, not per read.
            unsafe { enable_manual_accessibility(app_element) };
            let focused_owner = unsafe { copy_focused_ui_element(app_element) }?;
            let focused_element = focused_owner
                .as_ref()
                .map(|focused_owner| focused_owner.as_CFTypeRef() as AXUIElementRef);
            let target_element = choose_caret_observer_element(app_element, focused_element);
            let element_owners = if let Some(focused_owner) = focused_owner {
                vec![app_owner, focused_owner]
            } else {
                vec![app_owner]
            };

            install_worker_observer_resource(
                tx,
                callback_tx,
                dispatch,
                pid,
                target_element,
                vec![ObserverNotification::CaretChanged],
                element_owners,
            )
        })
    }
}

impl AxWorkerResource {
    pub fn close(mut self) -> Result<bool, PlatformError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(Message::RemoveResource {
                id: self.id,
                reply: Some(reply_tx),
            })
            .map_err(|_| PlatformError::CannotComplete {
                reason: "AX worker is not running".into(),
            })?;

        self.closed = true;
        reply_rx.recv().map_err(|_| PlatformError::CannotComplete {
            reason: "AX worker dropped resource removal result".into(),
        })
    }
}

impl Drop for AxWorkerResource {
    fn drop(&mut self) {
        if self.closed {
            return;
        }

        let _ = self.tx.send(Message::RemoveResource {
            id: self.id,
            reply: None,
        });
    }
}

impl Drop for SafetyPoller {
    fn drop(&mut self) {
        let _ = self.stop_tx.take().map(|stop_tx| stop_tx.send(()));
        if let Some(handle) = self.handle.take() {
            join_unless_self(handle);
        }
    }
}

impl Drop for RebindPoller {
    fn drop(&mut self) {
        let _ = self.stop_tx.take().map(|stop_tx| stop_tx.send(()));
        if let Some(handle) = self.handle.take() {
            join_unless_self(handle);
        }
    }
}

/// Join a poll thread, unless we are *on* that thread — a self-join makes
/// `pthread_join` return `EDEADLK` ("Resource deadlock avoided") and
/// `JoinHandle::join` panic. A reference cycle can land the last owner of a
/// poller on its own poll thread at teardown: the `ObserverBinding` owns the
/// `SafetyPoller`, whose poll-thread closure holds an `Arc` back into the binding
/// through the observer `dispatch`, so when that `Arc` is the final one, dropping
/// the binding runs `SafetyPoller::drop` on the caret-poll thread itself. Detach
/// in that case — the thread is already exiting (stop signal sent / channels
/// dropping), so its captured state is released either way.
fn join_unless_self(handle: JoinHandle<()>) {
    if handle.thread().id() == thread::current().id() {
        return; // self-join would deadlock; detach instead.
    }
    let _ = handle.join();
}

struct WorkerObserverResource {
    registration: Option<RawAxObserverRegistration>,
    _callback_state: Box<ObserverCallbackState>,
    _element_owners: Vec<CFType>,
}

impl Drop for WorkerObserverResource {
    fn drop(&mut self) {
        let _ = self.registration.take();
    }
}

fn create_app_ax_element(pid: i32) -> Result<(AXUIElementRef, CFType), PlatformError> {
    let element = unsafe { AXUIElementCreateApplication(pid) };
    if element.is_null() {
        return Err(PlatformError::CannotComplete {
            reason: "AXUIElementCreateApplication returned null".into(),
        });
    }

    let owner = unsafe { CFType::wrap_under_create_rule(element as CFTypeRef) };
    Ok((element, owner))
}

/// Ask an application to expose its accessibility tree by setting
/// `AXManualAccessibility = true` on its application element.
///
/// Chromium- and Electron-based apps (Chrome, Brave, Edge, Arc, Dia, Slack, VS
/// Code, …) build their AX tree lazily and only surface
/// `AXSelectedTextMarkerRange` markers once a client requests it this way. WebKit
/// (Safari) and native AppKit apps already expose markers and return
/// `AttributeUnsupported` here, so posting unconditionally needs no per-app
/// bundle-id detection. This is advisory: every failure is ignored. It is called
/// at observer install (once per focus bind to a pid), not on the per-caret read
/// path, so it adds no per-keystroke AX round-trip. Live caret behaviour is
/// covered by the browser caret-marker acceptance runner.
///
/// # Safety
/// `app_element` must be a valid application `AXUIElementRef`.
unsafe fn enable_manual_accessibility(app_element: AXUIElementRef) {
    let attribute = CFString::new(AX_MANUAL_ACCESSIBILITY_ATTRIBUTE);
    let value = CFBoolean::true_value();
    let _ = AXUIElementSetAttributeValue(
        app_element,
        attribute.as_concrete_TypeRef(),
        value.as_CFTypeRef(),
    );
}

fn install_worker_observer_resource(
    tx: mpsc::Sender<Message>,
    callback_tx: mpsc::Sender<CallbackMessage>,
    dispatch: ObserverDispatch,
    pid: i32,
    element: AXUIElementRef,
    notifications: Vec<ObserverNotification>,
    element_owners: Vec<CFType>,
) -> Result<WorkerResource, PlatformError> {
    let mut callback_state = Box::new(ObserverCallbackState {
        pid,
        tx,
        callback_tx,
        dispatch,
    });
    let refcon = callback_state.as_mut() as *mut ObserverCallbackState as *mut c_void;
    let registration =
        unsafe { register_raw_ax_observer_with_refcon(pid, element, &notifications, refcon) }?;

    Ok(Box::new(WorkerObserverResource {
        registration: Some(registration),
        _callback_state: callback_state,
        _element_owners: element_owners,
    }) as WorkerResource)
}

unsafe fn copy_focused_ui_element(
    app_element: AXUIElementRef,
) -> Result<Option<CFType>, PlatformError> {
    let attribute = CFString::new(kAXFocusedUIElementAttribute);
    let mut value: CFTypeRef = ptr::null_mut();
    let err =
        AXUIElementCopyAttributeValue(app_element, attribute.as_concrete_TypeRef(), &mut value);

    if focused_element_lookup_allows_app_fallback(err) {
        return Ok(None);
    }
    if err != kAXErrorSuccess {
        return Err(map_ax_error(err));
    }
    if value.is_null() {
        return Ok(None);
    }

    Ok(Some(CFType::wrap_under_create_rule(value)))
}

/// The attribute is simply absent on this element: read it as `None`/unsupported
/// rather than a hard error. Used for plain `AXUIElementCopyAttributeValue` reads.
fn ax_attribute_absent(error: AXError) -> bool {
    error == kAXErrorAttributeUnsupported || error == kAXErrorNoValue
}

/// As [`ax_attribute_absent`], plus `IllegalArgument` — for *settable* checks and
/// attribute writes, where some toolkits reject the attribute with that code.
fn ax_settable_absent(error: AXError) -> bool {
    ax_attribute_absent(error) || error == kAXErrorIllegalArgument
}

/// As [`ax_settable_absent`], plus `ParameterizedAttributeUnsupported` — for
/// parameterized range/marker queries (`AXBoundsForRange`,
/// `AXBoundsForTextMarkerRange`), whose absence shows up as any of these codes.
fn ax_parameterized_absent(error: AXError) -> bool {
    ax_settable_absent(error) || error == kAXErrorParameterizedAttributeUnsupported
}

/// The outcome of an AX bounds/marker copy, classified from the returned
/// `AXError` before the value pointer is touched.
#[derive(Debug, PartialEq, Eq)]
enum AxBoundsRead {
    /// The attribute is simply absent/unsupported on this element. Fail CLOSED:
    /// the caller degrades to `Ok(None)` (no rect) and falls back to
    /// caret/popup anchoring rather than surfacing an error.
    Absent,
    /// A real AX failure (e.g. `CannotComplete`): surface it as a `PlatformError`.
    Failed,
    /// The copy succeeded: read the rect from the returned value.
    Present,
}

/// Classify an AX bounds/marker copy result. This is the pure fail-closed seam
/// behind `text_range_rect`: an absent parameterized attribute degrades to a
/// missing rect (`Absent` → `Ok(None)`), never an error, while any other
/// non-success code is a genuine failure to surface (`Failed` → `Err`).
fn classify_ax_bounds_read(error: AXError) -> AxBoundsRead {
    if ax_parameterized_absent(error) {
        AxBoundsRead::Absent
    } else if error != kAXErrorSuccess {
        AxBoundsRead::Failed
    } else {
        AxBoundsRead::Present
    }
}

fn focused_element_lookup_allows_app_fallback(error: AXError) -> bool {
    ax_attribute_absent(error)
}

fn choose_caret_observer_element(
    app_element: AXUIElementRef,
    focused_element: Option<AXUIElementRef>,
) -> AXUIElementRef {
    focused_element.unwrap_or(app_element)
}

fn start_dynamic_observer_binding(
    config: DynamicObserverBindingConfig,
) -> Result<DynamicObserverBinding, PlatformError> {
    let initial = install_observer_binding(config.initial_pid, &config.binding)?;
    *config
        .current
        .lock()
        .map_err(|_| PlatformError::CannotComplete {
            reason: "observer binding lock poisoned".into(),
        })? = Some(initial);

    let rebinder = start_observer_rebind_poller(
        config.frontmost_pid,
        Arc::clone(&config.current),
        config.binding,
        config.rebind_interval,
    )?;

    Ok(DynamicObserverBinding {
        _rebinder: rebinder,
        _current: config.current,
    })
}

fn install_observer_binding(
    pid: i32,
    config: &ObserverBindingConfig,
) -> Result<ObserverBinding, PlatformError> {
    let observer = (config.installer)(
        pid,
        config.target,
        config.notifications.clone(),
        Arc::clone(&config.dispatch),
    )?;
    let poller = start_focused_element_safety_poll(
        config.worker_tx.clone(),
        pid,
        config.poll_notification,
        Arc::clone(&config.dispatch),
        config.callback_tx.clone(),
        CARET_SAFETY_POLL_INTERVAL,
    )?;

    Ok(ObserverBinding {
        pid,
        _observer: observer,
        _poller: poller,
    })
}

fn start_observer_rebind_poller(
    frontmost_pid: Arc<FrontmostPidProvider>,
    current: Arc<Mutex<Option<ObserverBinding>>>,
    config: ObserverBindingConfig,
    interval: Duration,
) -> Result<RebindPoller, PlatformError> {
    let (stop_tx, stop_rx) = mpsc::channel();
    let handle = thread::Builder::new()
        .name("compme-app-rebind".into())
        .spawn(move || loop {
            match stop_rx.recv_timeout(interval) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let desired_pid = frontmost_pid();
                    let current_pid = current_binding_pid(&current);
                    if desired_pid == current_pid {
                        continue;
                    }

                    let next_binding =
                        desired_pid.and_then(|pid| install_observer_binding(pid, &config).ok());

                    let Ok(mut current) = current.lock() else {
                        break;
                    };
                    if current.as_ref().map(|binding| binding.pid) == current_pid {
                        *current = next_binding;
                    }
                }
            }
        })
        .map_err(|_| PlatformError::CannotComplete {
            reason: "failed to start app rebind poll thread".into(),
        })?;

    Ok(RebindPoller {
        stop_tx: Some(stop_tx),
        handle: Some(handle),
    })
}

fn current_binding_pid(current: &Arc<Mutex<Option<ObserverBinding>>>) -> Option<i32> {
    current
        .lock()
        .ok()
        .and_then(|current| current.as_ref().map(|binding| binding.pid))
}

fn start_focused_element_safety_poll(
    tx: mpsc::Sender<Message>,
    pid: i32,
    notification: ObserverNotification,
    dispatch: ObserverDispatch,
    callback_tx: mpsc::Sender<CallbackMessage>,
    interval: Duration,
) -> Result<SafetyPoller, PlatformError> {
    let (stop_tx, stop_rx) = mpsc::channel();
    let handle = thread::Builder::new()
        .name("compme-caret-poll".into())
        .spawn(move || loop {
            match stop_rx.recv_timeout(interval) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if tx
                        .send(Message::PollFocusedElement {
                            pid,
                            notification,
                            dispatch: Arc::clone(&dispatch),
                            callback_tx: callback_tx.clone(),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }
        })
        .map_err(|_| PlatformError::CannotComplete {
            reason: "failed to start caret safety poll thread".into(),
        })?;

    Ok(SafetyPoller {
        stop_tx: Some(stop_tx),
        handle: Some(handle),
    })
}

fn dispatch_focused_element_poll(
    pid: i32,
    notification: ObserverNotification,
    dispatch: ObserverDispatch,
    callback_tx: mpsc::Sender<CallbackMessage>,
) {
    let Ok(event) = resolve_focused_or_app_event(pid, notification) else {
        return;
    };

    let _ = callback_tx.send(CallbackMessage::Dispatch { dispatch, event });
}

fn resolve_focused_or_app_event(
    pid: i32,
    notification: ObserverNotification,
) -> Result<ObserverEvent, PlatformError> {
    let (app_element, _app_owner) = create_app_ax_element(pid)?;
    let focused_owner = unsafe { copy_focused_ui_element(app_element) }?;
    let focused_element = focused_owner
        .as_ref()
        .map(|focused_owner| focused_owner.as_CFTypeRef() as AXUIElementRef);
    let target_element = choose_caret_observer_element(app_element, focused_element);

    Ok(ObserverEvent {
        pid,
        notification,
        identity: unsafe { resolve_ax_element_identity(target_element) }?,
        rect: observer_caret_rect(notification, target_element),
    })
}

fn capabilities_for_field(pid: i32, field: FieldHandle) -> Result<Capabilities, PlatformError> {
    let (element, _owners) = copy_focused_or_app_element(pid)?;
    let identity = unsafe { resolve_ax_element_identity(element) }?;
    if !field_matches_identity(&field, &identity) {
        return Err(PlatformError::StaleField);
    }

    let _value = unsafe { read_required_ax_string_attribute(element, kAXValueAttribute) }?;
    let selected_range = unsafe { read_required_ax_range_attribute(element) }?;
    let value_settable = unsafe { ax_attribute_is_settable(element, kAXValueAttribute) }?;
    let selected_range_settable =
        unsafe { ax_attribute_is_settable(element, kAXSelectedTextRangeAttribute) }
            .unwrap_or(false);
    let caret = selected_range.location.max(0);
    let has_caret_rect = match resolve_caret_rect_with_marker_first(
        caret,
        || unsafe { read_ax_bounds_for_selected_text_marker_range(element) },
        |location, length| unsafe { read_ax_bounds_for_range(element, location, length) },
    ) {
        Ok(Some(_)) => true,
        Ok(None) | Err(PlatformError::UnsupportedField { .. }) => false,
        Err(err) => return Err(err),
    };

    Ok(editable_capabilities(
        &identity,
        value_settable,
        selected_range_settable,
        has_caret_rect,
        true,
    ))
}

fn read_context_for_field(pid: i32, field: FieldHandle) -> Result<TextContext, PlatformError> {
    let (element, _owners) = copy_focused_or_app_element(pid)?;
    let identity = unsafe { resolve_ax_element_identity(element) }?;
    if !field_matches_identity(&field, &identity) {
        return Err(PlatformError::StaleField);
    }

    // TOCTOU re-check, mirroring the insert path's `recheck_secure_input`. The
    // dispatch-site guard in `read_context` samples global secure input once on
    // the calling thread; secure input can turn on in the window before this
    // worker thread reads `kAXValueAttribute` (e.g. an out-of-process password
    // prompt arms secure input mid-read). The `StaleField` guard above only
    // catches focus moving to a DIFFERENT element, not the same focused element
    // while global secure input flips on. Re-checking here keeps the window as
    // narrow as possible. `IsSecureEventInputEnabled()` is a global C call safe
    // from any thread.
    if macos_secure_input_enabled() {
        return Err(PlatformError::SecureInput {
            state: SecurityState::SecureInputEnabled,
        });
    }

    let value = unsafe { read_required_ax_string_attribute(element, kAXValueAttribute) }?;
    let selected_range = unsafe { read_required_ax_range_attribute(element) }?;
    Ok(text_context_from_value(field, value, selected_range))
}

fn caret_rect_for_field(pid: i32, field: FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
    let (element, _owners) = copy_focused_or_app_element(pid)?;
    let identity = unsafe { resolve_ax_element_identity(element) }?;
    if !field_matches_identity(&field, &identity) {
        return Err(PlatformError::StaleField);
    }

    // TOCTOU re-check on the worker thread, mirroring `read_context_for_field`
    // and the insert path's `recheck_secure_input`. Lower sensitivity than the
    // value read (geometry, not plaintext), but fixed for consistency so both
    // field-read workers share the fail-closed posture.
    if macos_secure_input_enabled() {
        return Err(PlatformError::SecureInput {
            state: SecurityState::SecureInputEnabled,
        });
    }

    let selected_range = unsafe { read_required_ax_range_attribute(element) }?;
    let caret = selected_range.location.max(0);
    let rect = resolve_caret_rect_with_marker_first(
        caret,
        || unsafe { read_ax_bounds_for_selected_text_marker_range(element) },
        |location, length| unsafe { read_ax_bounds_for_range(element, location, length) },
    )?;
    Ok(rect.map(|rect| {
        normalize_caret_rect(
            rect,
            bundle_id_for_pid(pid).as_deref(),
            is_browser_omnibox(&field.element_id),
        )
    }))
}

/// Bundles whose AX caret rect IS the caret line (`[y, y+h]`), unlike the
/// TextEdit-calibrated default where the line sits one rect below (`[y+h,
/// y+2h]`, cycle-44 live finding). Evidence-only list (2026-06-10 live
/// screenshots: ghost one line low in Chrome/iTerm2; 2026-06-14: same in
/// Safari's WebKit search fields — google.com/duckduckgo.com); extend per app
/// on evidence, never by guess. Safari's NATIVE address bar is field-aware
/// excluded (see `normalize_caret_rect`'s `is_omnibox`) — it is TextEdit-like,
/// not rect-is-line (2026-06-14: omnibox ghost landed too high under a blanket
/// shift).
const RECT_IS_LINE_BUNDLE_PREFIXES: [&str; 4] = [
    "com.google.Chrome",
    "org.chromium.",
    "com.googlecode.iterm2",
    "com.apple.Safari",
];

/// Whether `element_id` is a browser address/search bar (AXIdentifier
/// `WEB_BROWSER_ADDRESS_AND_SEARCH_FIELD`). Safari's is a NATIVE field whose
/// caret-rect semantics differ from its WebKit web content — see
/// [`normalize_caret_rect`].
fn is_browser_omnibox(element_id: &str) -> bool {
    element_id.contains("WEB_BROWSER_ADDRESS_AND_SEARCH_FIELD")
}

/// Normalize an app-specific caret rect to the calibrated default semantics
/// by shifting rect-is-line apps up one line. Degenerate rects (element
/// bounds, not carets) pass through untouched — the overlay's bounds fallback
/// owns those.
///
/// `is_omnibox` carves out Safari's NATIVE address/search bar, which is
/// TextEdit-like (the line sits one rect below the caret rect) UNLIKE Safari's
/// WebKit web content — so it must NOT get the rect-is-line shift, or the ghost
/// lands one line too HIGH (2026-06-14 live finding). The carve-out is
/// Safari-specific: Chrome/iTerm2 show no native-omnibox exception.
fn normalize_caret_rect(rect: ScreenRect, bundle_id: Option<&str>, is_omnibox: bool) -> ScreenRect {
    let plausible_caret = rect.w <= CARET_MAX_W && rect.h <= CARET_MAX_H;
    let rect_is_line = bundle_id.is_some_and(|id| {
        RECT_IS_LINE_BUNDLE_PREFIXES
            .iter()
            .any(|prefix| id.starts_with(prefix))
    });
    let safari_omnibox =
        is_omnibox && bundle_id.is_some_and(|id| id.starts_with("com.apple.Safari"));
    if plausible_caret && rect_is_line && !safari_omnibox {
        ScreenRect {
            y: rect.y - rect.h,
            ..rect
        }
    } else {
        rect
    }
}

/// Popup-mode fallback anchor: the focused field's window frame, used when no
/// caret geometry is available. Best effort — returns `None` if the element
/// exposes no `AXWindow`/`AXFrame`.
fn popup_anchor_for_field(
    pid: i32,
    field: FieldHandle,
) -> Result<Option<ScreenRect>, PlatformError> {
    let (element, _owners) = copy_focused_or_app_element(pid)?;
    let identity = unsafe { resolve_ax_element_identity(element) }?;
    if !field_matches_identity(&field, &identity) {
        return Err(PlatformError::StaleField);
    }

    // TOCTOU re-check on the worker thread, uniform with the other four
    // `_for_field` workers. Lowest sensitivity of the set (window-chrome
    // `AXFrame` geometry, never field text/caret), but re-checked anyway so all
    // five `_for_field` workers share the fail-closed posture with no straggler.
    if macos_secure_input_enabled() {
        return Err(PlatformError::SecureInput {
            state: SecurityState::SecureInputEnabled,
        });
    }

    unsafe {
        let Some((window, _window_owner)) =
            copy_ax_element_attribute(element, AX_WINDOW_ATTRIBUTE)?
        else {
            return Ok(None);
        };
        read_ax_cgrect_attribute(window, AX_FRAME_ATTRIBUTE)
    }
}

/// Copy an AX element-valued attribute (e.g. `AXWindow`). Returns the raw ref
/// together with its owning `CFType` so the caller keeps it alive.
unsafe fn copy_ax_element_attribute(
    element: AXUIElementRef,
    attribute: &str,
) -> Result<Option<(AXUIElementRef, CFType)>, PlatformError> {
    let attribute = CFString::new(attribute);
    let mut value: CFTypeRef = ptr::null_mut();
    let err = AXUIElementCopyAttributeValue(element, attribute.as_concrete_TypeRef(), &mut value);
    if ax_attribute_absent(err) {
        return Ok(None);
    }
    if err != kAXErrorSuccess {
        return Err(map_ax_error(err));
    }
    if value.is_null() {
        return Ok(None);
    }
    let owner = CFType::wrap_under_create_rule(value);
    Ok(Some((value as AXUIElementRef, owner)))
}

/// Read a `CGRect`-valued AX attribute (e.g. `AXFrame`) as a global screen rect.
unsafe fn read_ax_cgrect_attribute(
    element: AXUIElementRef,
    attribute: &str,
) -> Result<Option<ScreenRect>, PlatformError> {
    let attribute = CFString::new(attribute);
    let mut value: CFTypeRef = ptr::null_mut();
    let err = AXUIElementCopyAttributeValue(element, attribute.as_concrete_TypeRef(), &mut value);
    if ax_attribute_absent(err) {
        return Ok(None);
    }
    if err != kAXErrorSuccess {
        return Err(map_ax_error(err));
    }
    screen_rect_from_ax_value(value)
}

fn caret_diagnostics_for_field(
    pid: i32,
    field: FieldHandle,
) -> Result<MacosCaretDiagnostics, PlatformError> {
    let (element, _owners) = copy_focused_or_app_element(pid)?;
    let identity = unsafe { resolve_ax_element_identity(element) }?;
    if !field_matches_identity(&field, &identity) {
        return Err(PlatformError::StaleField);
    }

    // TOCTOU re-check on the worker thread, mirroring `caret_rect_for_field`.
    // Geometry-only (no plaintext), but kept uniform across all `_for_field`
    // worker fns so they share the fail-closed posture: the dispatch-site guard
    // samples global secure input once on the calling thread, and the
    // `StaleField` guard above only catches focus moving to a DIFFERENT element,
    // not the same focused element while global secure input flips on.
    if macos_secure_input_enabled() {
        return Err(PlatformError::SecureInput {
            state: SecurityState::SecureInputEnabled,
        });
    }

    let selected_range = unsafe { read_required_ax_range_attribute(element) }?;
    let caret = selected_range.location.max(0);
    let marker_rect = unsafe { read_ax_bounds_for_selected_text_marker_range(element) }?;
    let native_rect = resolve_caret_rect(caret, |location, length| unsafe {
        read_ax_bounds_for_range(element, location, length)
    })?;
    Ok(caret_diagnostics_from_rects(marker_rect, native_rect))
}

fn caret_diagnostics_from_rects(
    marker_rect: Option<ScreenRect>,
    native_rect: Option<ScreenRect>,
) -> MacosCaretDiagnostics {
    let (resolved_rect, source) = if marker_rect.is_some_and(usable_caret_rect) {
        (marker_rect, MacosCaretRectSource::Marker)
    } else if native_rect.is_some() {
        (native_rect, MacosCaretRectSource::NativeFallback)
    } else {
        (None, MacosCaretRectSource::None)
    };

    MacosCaretDiagnostics {
        marker_rect,
        native_rect,
        resolved_rect,
        source,
    }
}

/// Outcome of an AxSet value write, classified by readback (the iTerm2
/// finding, 2026-06-10: `AXUIElementSetAttributeValue` can return success
/// while the terminal's content stays untouched — a SILENT no-op that made
/// accepts count without inserting anything).
#[derive(Clone, Debug, PartialEq, Eq)]
enum AxSetApply {
    Applied(Inserted),
    /// The readback equals the ORIGINAL value: the write silently did
    /// nothing. (A readback that differs from both original and expected —
    /// e.g. app-side normalization — counts as Applied: falling back there
    /// would double-insert.)
    SilentlyIgnored,
}

/// Classify an AxSet write by comparing the post-write readback against the
/// original and expected values. Conservative: only a byte-identical-to-
/// original readback is a silent failure.
fn axset_readback_outcome(original: &str, readback: &str, inserted: Inserted) -> AxSetApply {
    if readback == original {
        AxSetApply::SilentlyIgnored
    } else {
        AxSetApply::Applied(inserted)
    }
}

/// Whether a post-write readback is worth logging as a divergence. A readback
/// equal to `new_value` is a clean apply, and one equal to `original` is the
/// silent-no-op quirk already classified by [`axset_readback_outcome`]; both
/// stay silent. Only a readback matching NEITHER is the diagnostic signal —
/// usually app-side normalization, but also the sole observable symptom of a
/// wrong-range or partially-applied splice — and must be surfaced. The `&&`
/// (not `||`) is load-bearing: either clause alone would fire on every normal
/// apply or every silent no-op.
fn range_readback_diverged(original: &str, new_value: &str, readback: &str) -> bool {
    readback != new_value && readback != original
}

/// Set the caret after a value write, treating any failure as non-fatal.
///
/// The value write already landed by the time this runs; a caret-set failure
/// must NOT override that completed write. `set_required_ax_selected_range` can
/// return `StaleField`/`CannotComplete` when the preceding value-set rebuilt the
/// AX tree (the same quirk class as the iTerm2 silent-no-op finding). Letting
/// such an error propagate via `?` would strand a COMPLETED write before the
/// caller's readback classification, turning a landed insert into a bare `Err`.
/// The readback is the source of truth for whether the text applied, so the
/// caret result is advisory: `UnsupportedField` is expected on some fields and
/// stays silent; any other error is logged and swallowed.
unsafe fn set_caret_after_value_write(element: AXUIElementRef, new_caret: usize) {
    if let Err(err) = set_required_ax_selected_range(element, new_caret) {
        if caret_set_failure_is_worth_logging(&err) {
            eprintln!("compme: caret set after AX value write failed (non-fatal): {err:?}");
        }
    }
}

/// Whether a caret-set failure (after a landed value write) is worth logging.
/// `UnsupportedField` is expected on fields that expose no settable selected
/// range, so it stays silent; every other error is non-fatal but surfaced.
fn caret_set_failure_is_worth_logging(err: &PlatformError) -> bool {
    !matches!(err, PlatformError::UnsupportedField { .. })
}

fn insert_for_field(
    pid: i32,
    field: FieldHandle,
    text: String,
    replace_left: usize,
    strategy: InsertStrategy,
) -> Result<AxSetApply, PlatformError> {
    let (element, _owners) = copy_focused_or_app_element(pid)?;
    let identity = unsafe { resolve_ax_element_identity(element) }?;
    if !field_matches_identity(&field, &identity) {
        return Err(PlatformError::StaleField);
    }

    // TOCTOU re-check on the worker thread, mirroring `read_context_for_field`
    // and `caret_rect_for_field`. The dispatch-site guard samples global secure
    // input once on the calling thread; secure input can flip on in the window
    // before this worker reads `kAXValueAttribute` (the full field plaintext)
    // below. The `StaleField` guard above only catches focus moving to a
    // DIFFERENT element, not the same focused element while global secure input
    // arms. Re-checking here keeps the window as narrow as possible.
    if macos_secure_input_enabled() {
        return Err(PlatformError::SecureInput {
            state: SecurityState::SecureInputEnabled,
        });
    }

    let value = unsafe { read_required_ax_string_attribute(element, kAXValueAttribute) }?;
    let selected_range = unsafe { read_required_ax_range_attribute(element) }?;
    // For a replacement, extend the splice range left to cover the typed token
    // (`replace_left` characters) so it is deleted before the new text is inserted.
    let selected_range = extend_range_left(&value, selected_range, replace_left);
    let (new_value, new_caret) = splice_text_at_utf16_range(&value, selected_range, &text);

    unsafe {
        set_required_ax_string_attribute(element, kAXValueAttribute, &new_value)?;
        set_caret_after_value_write(element, new_caret);
    }

    // Read the value back: some apps (live: iTerm2) report a settable
    // AXValue, return success from the set, and change NOTHING. A readback
    // still equal to the original is that silent no-op; the adapter then
    // falls back to synthetic input. Readback failure is treated as Applied
    // (fail open — the set reported success and we cannot prove otherwise).
    let readback = unsafe { read_required_ax_string_attribute(element, kAXValueAttribute) }
        .unwrap_or_else(|_| new_value.clone());
    Ok(axset_readback_outcome(
        &value,
        &readback,
        Inserted {
            bytes: text.len(),
            chars: text.chars().count(),
            strategy,
        },
    ))
}

fn text_range_rect_for_field(
    pid: i32,
    field: FieldHandle,
    range: CorrectionRange,
) -> Result<Option<ScreenRect>, PlatformError> {
    let (element, _owners) = copy_focused_or_app_element(pid)?;
    let identity = unsafe { resolve_ax_element_identity(element) }?;
    if !field_matches_identity(&field, &identity) {
        return Err(PlatformError::StaleField);
    }
    if macos_secure_input_enabled() {
        return Err(PlatformError::SecureInput {
            state: SecurityState::SecureInputEnabled,
        });
    }

    let value = unsafe { read_required_ax_string_attribute(element, kAXValueAttribute) }?;
    let selected_range = unsafe { read_required_ax_range_attribute(element) }?;
    let ctx = text_context_from_value(field, value, selected_range);
    let Some(range) =
        scalar_correction_range_to_utf16_range(&ctx.left, &ctx.right, selected_range, range)
    else {
        return Err(PlatformError::UnsupportedField {
            reason: "correction range is not contiguous in the field".into(),
        });
    };
    unsafe { read_ax_bounds_for_range(element, range.location, range.length) }
}

fn insert_range_for_field(
    pid: i32,
    field: FieldHandle,
    text: String,
    range: CorrectionRange,
    strategy: InsertStrategy,
) -> Result<AxSetApply, PlatformError> {
    let (element, _owners) = copy_focused_or_app_element(pid)?;
    let identity = unsafe { resolve_ax_element_identity(element) }?;
    if !field_matches_identity(&field, &identity) {
        return Err(PlatformError::StaleField);
    }
    if macos_secure_input_enabled() {
        return Err(PlatformError::SecureInput {
            state: SecurityState::SecureInputEnabled,
        });
    }

    let value = unsafe { read_required_ax_string_attribute(element, kAXValueAttribute) }?;
    let selected_range = unsafe { read_required_ax_range_attribute(element) }?;
    let ctx = text_context_from_value(field, value.clone(), selected_range);
    let Some(range) =
        scalar_correction_range_to_utf16_range(&ctx.left, &ctx.right, selected_range, range)
    else {
        return Err(PlatformError::UnsupportedField {
            reason: "correction range is not contiguous in the field".into(),
        });
    };
    let (new_value, new_caret) = splice_text_at_utf16_range(&value, range, &text);
    unsafe {
        set_required_ax_string_attribute(element, kAXValueAttribute, &new_value)?;
        set_caret_after_value_write(element, new_caret);
    }

    // Classify by readback exactly like `insert_for_field`: fail OPEN on a
    // readback read error (the set reported success and we cannot prove
    // otherwise), and treat any value that differs from the ORIGINAL as
    // Applied. A readback that differs from both original and `new_value` (e.g.
    // app-side normalization of smart quotes / trailing whitespace) is a
    // COMPLETED replacement, not a silent no-op; classifying it `SilentlyIgnored`
    // would claim nothing happened after the field was already mutated (see the
    // `AxSetApply` doc). Only a readback byte-identical to the original is the
    // silent-write quirk.
    let readback = unsafe { read_required_ax_string_attribute(element, kAXValueAttribute) }
        .unwrap_or_else(|_| new_value.clone());
    // Log a divergent readback so a wrong-range/partial-splice failure stays
    // diagnosable while still reporting Applied. Lengths only: the field text
    // may be sensitive.
    if range_readback_diverged(&value, &new_value, &readback) {
        eprintln!(
            "compme: range replacement readback diverged from expected value \
             (expected {} utf16 units, read back {})",
            new_value.encode_utf16().count(),
            readback.encode_utf16().count()
        );
    }
    Ok(axset_readback_outcome(
        &value,
        &readback,
        Inserted {
            bytes: text.len(),
            chars: text.chars().count(),
            strategy,
        },
    ))
}

fn copy_focused_or_app_element(pid: i32) -> Result<(AXUIElementRef, Vec<CFType>), PlatformError> {
    let (app_element, app_owner) = create_app_ax_element(pid)?;
    let focused_owner = unsafe { copy_focused_ui_element(app_element) }?;
    let focused_element = focused_owner
        .as_ref()
        .map(|focused_owner| focused_owner.as_CFTypeRef() as AXUIElementRef);
    let target_element = choose_caret_observer_element(app_element, focused_element);
    let owners = if let Some(focused_owner) = focused_owner {
        vec![app_owner, focused_owner]
    } else {
        vec![app_owner]
    };

    Ok((target_element, owners))
}

fn field_matches_identity(field: &FieldHandle, identity: &AxElementIdentity) -> bool {
    if field.element_id == identity.field_element_id() {
        return true;
    }

    identity.stable_field_key().is_some_and(|stable_key| {
        let stable_key = stable_key.strip_prefix("ax:").unwrap_or(&stable_key);
        // Segment equality, NOT substring containment: "id=name" must not
        // match a field carrying "id=name2", and "pid=4" must not match
        // "pid=42". The split honors the component escaping ("\|" stays
        // inside its segment) — a naive split('|') would let an identifier
        // containing a literal '|' forge segments (e.g. a Chromium
        // web-content id of "x|role=AXTextArea") and resurrect the
        // wrong-field-guard bypass this match exists to prevent.
        let field_id = field
            .element_id
            .strip_prefix("ax:")
            .unwrap_or(&field.element_id);
        let field_parts = split_identity_segments(field_id);
        split_identity_segments(stable_key)
            .iter()
            .all(|part| field_parts.contains(part))
    })
}

/// Split an identity key into its `|`-separated segments, honoring the
/// [`escape_identity_component`] scheme: `\|` is a literal pipe inside a
/// segment, `\\` a literal backslash, so neither terminates a segment.
fn split_identity_segments(value: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut escaped = false;
    for (i, c) in value.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '|' {
            segments.push(&value[start..i]);
            start = i + 1;
        }
    }
    segments.push(&value[start..]);
    segments
}

unsafe fn read_required_ax_string_attribute(
    element: AXUIElementRef,
    attribute: &str,
) -> Result<String, PlatformError> {
    let attribute = CFString::new(attribute);
    let mut value: CFTypeRef = ptr::null_mut();
    let err = AXUIElementCopyAttributeValue(element, attribute.as_concrete_TypeRef(), &mut value);
    if ax_attribute_absent(err) {
        return Err(PlatformError::UnsupportedField {
            reason: "AX text value unavailable".into(),
        });
    }
    if err != kAXErrorSuccess {
        return Err(map_ax_error(err));
    }
    if value.is_null() {
        return Err(PlatformError::UnsupportedField {
            reason: "AX text value was null".into(),
        });
    }

    let value = CFType::wrap_under_create_rule(value);
    value
        .downcast::<CFString>()
        .map(|value| value.to_string())
        .ok_or_else(|| PlatformError::UnsupportedField {
            reason: "AX text value was not a string".into(),
        })
}

unsafe fn read_required_ax_range_attribute(
    element: AXUIElementRef,
) -> Result<CFRange, PlatformError> {
    let attribute = CFString::new(kAXSelectedTextRangeAttribute);
    let mut value: CFTypeRef = ptr::null_mut();
    let err = AXUIElementCopyAttributeValue(element, attribute.as_concrete_TypeRef(), &mut value);
    if ax_attribute_absent(err) {
        return Err(PlatformError::UnsupportedField {
            reason: "AX selected text range unavailable".into(),
        });
    }
    if err != kAXErrorSuccess {
        return Err(map_ax_error(err));
    }
    if value.is_null() {
        return Err(PlatformError::UnsupportedField {
            reason: "AX selected text range was null".into(),
        });
    }

    let value = CFType::wrap_under_create_rule(value);
    let mut range = CFRange {
        location: 0,
        length: 0,
    };
    if AXValueGetValue(
        value.as_CFTypeRef() as AXValueRef,
        kAXValueTypeCFRange,
        &mut range as *mut _ as *mut c_void,
    ) {
        Ok(range)
    } else {
        Err(PlatformError::UnsupportedField {
            reason: "AX selected text range was not a CFRange".into(),
        })
    }
}

unsafe fn read_ax_bounds_for_selected_text_marker_range(
    element: AXUIElementRef,
) -> Result<Option<ScreenRect>, PlatformError> {
    let marker_attribute = CFString::new(AX_SELECTED_TEXT_MARKER_RANGE_ATTRIBUTE);
    let mut marker_range: CFTypeRef = ptr::null_mut();
    let err = AXUIElementCopyAttributeValue(
        element,
        marker_attribute.as_concrete_TypeRef(),
        &mut marker_range,
    );
    match classify_ax_bounds_read(err) {
        AxBoundsRead::Absent => return Ok(None),
        AxBoundsRead::Failed => return Err(map_ax_error(err)),
        AxBoundsRead::Present => {}
    }
    if marker_range.is_null() {
        return Ok(None);
    }
    let marker_range_owner = CFType::wrap_under_create_rule(marker_range);

    let bounds_attribute = CFString::new(AX_BOUNDS_FOR_TEXT_MARKER_RANGE_PARAMETERIZED_ATTRIBUTE);
    let mut value: CFTypeRef = ptr::null_mut();
    let err = AXUIElementCopyParameterizedAttributeValue(
        element,
        bounds_attribute.as_concrete_TypeRef(),
        marker_range_owner.as_CFTypeRef(),
        &mut value,
    );
    match classify_ax_bounds_read(err) {
        AxBoundsRead::Absent => return Ok(None),
        AxBoundsRead::Failed => return Err(map_ax_error(err)),
        AxBoundsRead::Present => {}
    }

    screen_rect_from_ax_value(value)
}

unsafe fn read_ax_bounds_for_range(
    element: AXUIElementRef,
    location: isize,
    length: isize,
) -> Result<Option<ScreenRect>, PlatformError> {
    let range = CFRange { location, length };
    let parameter = AXValueCreate(kAXValueTypeCFRange, &range as *const _ as *const c_void);
    if parameter.is_null() {
        return Err(PlatformError::CannotComplete {
            reason: "AXValueCreate failed for CFRange".into(),
        });
    }
    let _parameter_owner = CFType::wrap_under_create_rule(parameter as CFTypeRef);

    let attribute = CFString::new(kAXBoundsForRangeParameterizedAttribute);
    let mut value: CFTypeRef = ptr::null_mut();
    let err = AXUIElementCopyParameterizedAttributeValue(
        element,
        attribute.as_concrete_TypeRef(),
        parameter as CFTypeRef,
        &mut value,
    );
    match classify_ax_bounds_read(err) {
        AxBoundsRead::Absent => return Ok(None),
        AxBoundsRead::Failed => return Err(map_ax_error(err)),
        AxBoundsRead::Present => {}
    }
    if value.is_null() {
        return Ok(None);
    }

    screen_rect_from_ax_value(value)
}

unsafe fn screen_rect_from_ax_value(value: CFTypeRef) -> Result<Option<ScreenRect>, PlatformError> {
    if value.is_null() {
        return Ok(None);
    }

    let value = CFType::wrap_under_create_rule(value);
    let mut rect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: 0.0,
            height: 0.0,
        },
    };
    if AXValueGetValue(
        value.as_CFTypeRef() as AXValueRef,
        kAXValueTypeCGRect,
        &mut rect as *mut _ as *mut c_void,
    ) {
        Ok(Some(normalize_ax_screen_rect(
            rect,
            &active_display_scales(),
        )))
    } else {
        Ok(None)
    }
}

/// A display's point-space bounds plus its backing scale factor. Used to detect
/// whether an AX rect was reported in pixels instead of points.
#[derive(Clone, Copy, Debug)]
struct DisplayScale {
    bounds: CGRect,
    scale: f64,
}

/// The true backing scale factor for a display: native (mode) pixel width over
/// the mode's point width. Use the current `CGDisplayMode`, not
/// `CGDisplayPixelsWide`, which returns the *logical* (point) width for scaled
/// Retina modes and so always yields ~1.0 (the G7 caveat).
fn backing_scale(pixel_width: u64, point_width: u64) -> f64 {
    if point_width == 0 {
        return 1.0;
    }
    pixel_width as f64 / point_width as f64
}

/// Active displays with their point-space bounds and backing scale factor,
/// read via thread-safe CoreGraphics (not NSScreen, which needs the main
/// thread — caret rects are read off the AX worker thread).
fn active_display_scales() -> Vec<DisplayScale> {
    let Ok(ids) = CGDisplay::active_displays() else {
        return Vec::new();
    };
    ids.iter()
        .map(|id| {
            let display = CGDisplay::new(*id);
            let bounds = display.bounds();
            // True backing scale from the current display mode's native pixel
            // width vs its point width (CGDisplayPixelsWide reports points for
            // scaled Retina modes, so it can't tell 2x apart from 1x).
            let scale = display
                .display_mode()
                .map(|mode| backing_scale(mode.pixel_width(), mode.width()))
                .filter(|scale| *scale > 0.0)
                .unwrap_or(1.0);
            DisplayScale { bounds, scale }
        })
        .collect()
}

fn point_within(point: CGPoint, bounds: CGRect) -> bool {
    point.x >= bounds.origin.x
        && point.x <= bounds.origin.x + bounds.size.width
        && point.y >= bounds.origin.y
        && point.y <= bounds.origin.y + bounds.size.height
}

/// Normalize an AX caret/bounds rect into global screen points.
///
/// AX is documented to return global screen *points*, and on every display we
/// have measured it does — so the common path is a pass-through that preserves
/// fractional and negative origins for Retina and non-primary layouts. But the
/// MVP spec (§"Retina pixel-vs-point": "divide by per-display
/// `backingScaleFactor` if mismatched") requires guarding the case where a
/// misbehaving app reports *pixels*: if the raw origin lands on no display yet
/// dividing by some display's scale lands it inside that display's point
/// bounds, the rect was in pixels — divide the whole rect by that scale.
fn normalize_ax_screen_rect(rect: CGRect, displays: &[DisplayScale]) -> ScreenRect {
    let origin = rect.origin;
    let on_a_display = displays.iter().any(|d| point_within(origin, d.bounds));
    if !on_a_display {
        if let Some(scale) = displays.iter().find_map(|d| {
            let scaled = CGPoint::new(origin.x / d.scale, origin.y / d.scale);
            (d.scale > 1.0 && point_within(scaled, d.bounds)).then_some(d.scale)
        }) {
            return ScreenRect {
                x: rect.origin.x / scale,
                y: rect.origin.y / scale,
                w: rect.size.width / scale,
                h: rect.size.height / scale,
            };
        }
    }
    ScreenRect {
        x: rect.origin.x,
        y: rect.origin.y,
        w: rect.size.width,
        h: rect.size.height,
    }
}

unsafe fn ax_attribute_is_settable(
    element: AXUIElementRef,
    attribute: &str,
) -> Result<bool, PlatformError> {
    let attribute = CFString::new(attribute);
    let mut settable: c_uchar = 0;
    let err =
        AXUIElementIsAttributeSettable(element, attribute.as_concrete_TypeRef(), &mut settable);
    if ax_settable_absent(err) {
        return Ok(false);
    }
    if err == kAXErrorSuccess {
        Ok(settable != 0)
    } else {
        Err(map_ax_error(err))
    }
}

unsafe fn set_required_ax_string_attribute(
    element: AXUIElementRef,
    attribute: &str,
    value: &str,
) -> Result<(), PlatformError> {
    let attribute = CFString::new(attribute);
    let value = CFString::new(value);
    let err = AXUIElementSetAttributeValue(
        element,
        attribute.as_concrete_TypeRef(),
        value.as_CFTypeRef(),
    );
    if ax_settable_absent(err) {
        return Err(PlatformError::UnsupportedField {
            reason: "AX text value is not settable".into(),
        });
    }
    if err == kAXErrorSuccess {
        Ok(())
    } else {
        Err(map_ax_error(err))
    }
}

unsafe fn set_required_ax_selected_range(
    element: AXUIElementRef,
    caret: usize,
) -> Result<(), PlatformError> {
    let location = isize::try_from(caret).map_err(|_| PlatformError::CannotComplete {
        reason: "insert caret offset overflowed CFRange".into(),
    })?;
    let range = CFRange {
        location,
        length: 0,
    };
    let value = AXValueCreate(kAXValueTypeCFRange, &range as *const _ as *const c_void);
    if value.is_null() {
        return Err(PlatformError::CannotComplete {
            reason: "AXValueCreate failed for selected text range".into(),
        });
    }
    let value = CFType::wrap_under_create_rule(value as CFTypeRef);
    let attribute = CFString::new(kAXSelectedTextRangeAttribute);
    let err = AXUIElementSetAttributeValue(
        element,
        attribute.as_concrete_TypeRef(),
        value.as_CFTypeRef(),
    );
    if ax_settable_absent(err) {
        return Err(PlatformError::UnsupportedField {
            reason: "AX selected text range is not settable".into(),
        });
    }
    if err == kAXErrorSuccess {
        Ok(())
    } else {
        Err(map_ax_error(err))
    }
}

fn resolve_caret_rect(
    caret: isize,
    mut bounds: impl FnMut(isize, isize) -> Result<Option<ScreenRect>, PlatformError>,
) -> Result<Option<ScreenRect>, PlatformError> {
    if let Some(rect) = bounds(caret, 0)? {
        if usable_caret_rect(rect) {
            return Ok(Some(rect));
        }
    }

    if caret > 0 {
        if let Some(previous) = bounds(caret - 1, 1)? {
            if usable_caret_rect(previous) {
                return Ok(Some(ScreenRect {
                    x: previous.x + previous.w,
                    y: previous.y,
                    w: 1.0,
                    h: previous.h,
                }));
            }
        }
    }

    Ok(None)
}

fn resolve_caret_rect_with_marker_first(
    caret: isize,
    mut marker_bounds: impl FnMut() -> Result<Option<ScreenRect>, PlatformError>,
    range_bounds: impl FnMut(isize, isize) -> Result<Option<ScreenRect>, PlatformError>,
) -> Result<Option<ScreenRect>, PlatformError> {
    if let Some(rect) = marker_bounds()? {
        if usable_caret_rect(rect) {
            return Ok(Some(rect));
        }
    }

    resolve_caret_rect(caret, range_bounds)
}

fn usable_caret_rect(rect: ScreenRect) -> bool {
    // A collapsed caret is a thin vertical bar — zero width is valid (Chrome/
    // WebKit return zero-width marker rects, G5). Reject only negative or
    // container-sized widths; a zero-width rect can never be a container, which
    // always has positive width. Height must be a positive, caret-sized value.
    rect.w >= 0.0
        && rect.w < MAX_USABLE_CARET_RECT_WIDTH
        && rect.h > 0.0
        && rect.h < MAX_USABLE_CARET_RECT_HEIGHT
}

/// Extend a caret/selection range left by `replace_left` characters so a
/// subsequent splice deletes the typed token before inserting (a replacement,
/// e.g. emoji `:smile`→😄). `replace_left` is in **characters**; the AX range is
/// in **UTF-16 units**, so this walks char boundaries to convert. Clamped to the
/// text available left of the caret; `replace_left == 0` returns the range
/// unchanged (so ordinary inserts are byte-identical).
fn extend_range_left(value: &str, range: CFRange, replace_left: usize) -> CFRange {
    if replace_left == 0 {
        return range;
    }
    let utf16_len = value.encode_utf16().count();
    let caret = (range.location.max(0) as usize).min(utf16_len);
    // UTF-16 offset at each char boundary from the start up to the caret.
    let mut boundaries = vec![0usize];
    let mut offset = 0usize;
    for ch in value.chars() {
        if offset >= caret {
            break;
        }
        offset += ch.len_utf16();
        boundaries.push(offset);
    }
    let chars_before_caret = boundaries.len().saturating_sub(1);
    let start_char = chars_before_caret.saturating_sub(replace_left);
    let start = boundaries[start_char];
    let delta = caret.saturating_sub(start);
    // Cover exactly the `replace_left`-char prefix ending at the caret. We do NOT
    // add `range.length`: if the field has a live selection, sweeping it into the
    // splice would delete the user's selected text along with the typed token.
    // For a collapsed caret (range.length == 0, the usual case) this is unchanged.
    CFRange {
        location: start as isize,
        length: delta as isize,
    }
}

fn splice_text_at_utf16_range(
    value: &str,
    selected_range: CFRange,
    insert: &str,
) -> (String, usize) {
    let utf16_len = value.encode_utf16().count();
    let start = (selected_range.location.max(0) as usize).min(utf16_len);
    let length = selected_range.length.max(0) as usize;
    let end = start.saturating_add(length).min(utf16_len);
    let left_end = byte_index_for_utf16_units(value, start);
    let right_start = byte_index_for_utf16_units(value, end);

    let mut new_value = String::with_capacity(
        value
            .len()
            .saturating_add(insert.len())
            .saturating_sub(right_start.saturating_sub(left_end)),
    );
    new_value.push_str(&value[..left_end]);
    new_value.push_str(insert);
    new_value.push_str(&value[right_start..]);

    (
        new_value,
        start.saturating_add(insert.encode_utf16().count()),
    )
}

fn utf16_units_for_scalar_prefix(value: &str, scalar_count: usize) -> Option<usize> {
    let total = value.chars().count();
    if scalar_count > total {
        return None;
    }
    Some(value.chars().take(scalar_count).map(char::len_utf16).sum())
}

fn scalar_correction_range_to_utf16_range(
    left: &str,
    right: &str,
    selected_range: CFRange,
    range: CorrectionRange,
) -> Option<CFRange> {
    if range.start > range.end {
        return None;
    }
    let left_scalars = left.chars().count();
    let right_scalars = right.chars().count();
    if range.end > left_scalars.saturating_add(right_scalars) {
        return None;
    }

    let selection_len = selected_range.length.max(0) as usize;
    let caret_utf16 = (selected_range.location.max(0) as usize).saturating_add(selection_len);
    let selection_start = selected_range.location.max(0) as usize;

    let (start_utf16, end_utf16) = if range.end <= left_scalars {
        let left_start = utf16_units_for_scalar_prefix(left, range.start)?;
        let left_end = utf16_units_for_scalar_prefix(left, range.end)?;
        (left_start, left_end)
    } else if range.start >= left_scalars {
        let right_start =
            utf16_units_for_scalar_prefix(right, range.start.saturating_sub(left_scalars))?;
        let right_end =
            utf16_units_for_scalar_prefix(right, range.end.saturating_sub(left_scalars))?;
        (
            caret_utf16.saturating_add(right_start),
            caret_utf16.saturating_add(right_end),
        )
    } else if selection_len == 0 {
        let left_start = utf16_units_for_scalar_prefix(left, range.start)?;
        let right_end =
            utf16_units_for_scalar_prefix(right, range.end.saturating_sub(left_scalars))?;
        (left_start, caret_utf16.saturating_add(right_end))
    } else {
        return None;
    };

    if start_utf16 > end_utf16
        || (selection_len > 0 && start_utf16 < selection_start && end_utf16 > selection_start)
    {
        return None;
    }
    Some(CFRange {
        location: start_utf16 as isize,
        length: end_utf16.saturating_sub(start_utf16) as isize,
    })
}

fn editable_capabilities(
    identity: &AxElementIdentity,
    value_settable: bool,
    selected_range_settable: bool,
    has_caret_rect: bool,
    global_insert_allowed: bool,
) -> Capabilities {
    let insert_strategy = insertion_strategy(
        value_settable,
        selected_range_settable,
        has_caret_rect,
        global_insert_allowed,
    );

    Capabilities {
        readable_text: true,
        readable_caret: selected_range_settable && has_caret_rect,
        writable: insert_strategy != InsertStrategy::None,
        secure: false,
        security_state: SecurityState::Normal,
        toolkit: toolkit_for_identity(identity),
        multiline: identity
            .role
            .as_deref()
            .is_some_and(|role| role == "AXTextArea"),
        insert_strategy,
        accept_intercept: KeyInterceptMode::CarbonHotkey,
        overlay_at_caret: if selected_range_settable && has_caret_rect {
            OverlayPlacement::NativePanel
        } else {
            OverlayPlacement::None
        },
        coords_global_screen: true,
    }
}

fn insertion_strategy(
    value_settable: bool,
    selected_range_settable: bool,
    has_caret_rect: bool,
    global_insert_allowed: bool,
) -> InsertStrategy {
    if value_settable {
        InsertStrategy::AxSet
    } else if global_insert_allowed && selected_range_settable {
        InsertStrategy::SyntheticKeys
    } else if global_insert_allowed && has_caret_rect {
        InsertStrategy::Clipboard
    } else {
        InsertStrategy::None
    }
}

fn toolkit_for_identity(identity: &AxElementIdentity) -> Toolkit {
    match identity.role.as_deref() {
        Some("AXTextArea" | "AXTextField") => Toolkit::AppKit,
        Some(role) => Toolkit::Unknown(format!("macOS Accessibility {role}")),
        None => Toolkit::Unknown("macOS Accessibility".into()),
    }
}

fn text_context_from_value(
    field: FieldHandle,
    value: String,
    selected_range: CFRange,
) -> TextContext {
    let utf16_len = value.encode_utf16().count();
    let start = (selected_range.location.max(0) as usize).min(utf16_len);
    let length = selected_range.length.max(0) as usize;
    let end = start.saturating_add(length).min(utf16_len);
    let left_end = byte_index_for_utf16_units(&value, start);
    let right_start = byte_index_for_utf16_units(&value, end);

    TextContext {
        left: value[..left_end].to_string(),
        right: value[right_start..].to_string(),
        selection: (end > start).then_some(TextRange { start, end }),
        caret: start,
        source: ContextSource::Accessibility,
        field_id: field,
        offset_encoding: OffsetEncoding::Utf16CodeUnits,
    }
}

fn byte_index_for_utf16_units(value: &str, target_units: usize) -> usize {
    if target_units == 0 {
        return 0;
    }

    let mut units = 0usize;
    for (byte_index, ch) in value.char_indices() {
        if units >= target_units {
            return byte_index;
        }
        units = units.saturating_add(ch.len_utf16());
        if units >= target_units {
            return byte_index + ch.len_utf8();
        }
    }

    value.len()
}

fn run_ax_worker_loop<L, F>(
    mut worker_loop: L,
    started_tx: mpsc::Sender<Result<ThreadId, PlatformError>>,
    setup: F,
    timeout_seconds: f32,
) where
    L: AxWorkerLoop,
    F: FnOnce(f32) -> Result<(), PlatformError>,
{
    let mut resources: HashMap<u64, WorkerResource> = HashMap::new();
    let thread_id = thread::current().id();
    if let Err(err) = setup(timeout_seconds) {
        let _ = started_tx.send(Err(err));
        return;
    }

    if started_tx.send(Ok(thread_id)).is_err() {
        return;
    }

    loop {
        match worker_loop.recv() {
            Ok(Message::Run { job, reply }) => {
                let _ = reply.send(job());
                worker_loop.pump_run_loop();
            }
            Ok(Message::InstallResource { id, install, reply }) => {
                let result = install().map(|resource| {
                    resources.insert(id, resource);
                });
                let _ = reply.send(result);
                worker_loop.pump_run_loop();
            }
            Ok(Message::RemoveResource { id, reply }) => {
                let removed = resources.remove(&id).is_some();
                if let Some(reply) = reply {
                    let _ = reply.send(removed);
                }
                worker_loop.pump_run_loop();
            }
            Ok(Message::ObserverEvent {
                pid,
                notification,
                retained_element,
                fallback_element_id,
                dispatch,
                callback_tx,
            }) => {
                let event = resolve_retained_observer_event(
                    pid,
                    notification,
                    retained_element,
                    &fallback_element_id,
                );
                let _ = callback_tx.send(CallbackMessage::Dispatch { dispatch, event });
                worker_loop.pump_run_loop();
            }
            Ok(Message::PollFocusedElement {
                pid,
                notification,
                dispatch,
                callback_tx,
            }) => {
                dispatch_focused_element_poll(pid, notification, dispatch, callback_tx);
                worker_loop.pump_run_loop();
            }
            #[cfg(test)]
            Ok(Message::ResourceCount { reply }) => {
                let _ = reply.send(resources.len());
            }
            Ok(Message::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => worker_loop.pump_run_loop(),
        }
    }
}

impl Drop for AxWorker {
    fn drop(&mut self) {
        let _ = self.tx.send(Message::Stop);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl CallbackDispatcher {
    fn new() -> Result<Self, PlatformError> {
        let (tx, rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("compme-callbacks".into())
            .spawn(move || run_callback_dispatcher(rx))
            .map_err(|_| PlatformError::CannotComplete {
                reason: "failed to start callback dispatcher thread".into(),
            })?;

        Ok(Self {
            tx,
            handle: Some(handle),
        })
    }

    fn sender(&self) -> mpsc::Sender<CallbackMessage> {
        self.tx.clone()
    }
}

impl Drop for CallbackDispatcher {
    fn drop(&mut self) {
        let _ = self.tx.send(CallbackMessage::Stop);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run_callback_dispatcher(rx: mpsc::Receiver<CallbackMessage>) {
    while let Ok(message) = rx.recv() {
        match message {
            CallbackMessage::Dispatch { dispatch, event } => {
                dispatch_observer_event(dispatch, event);
            }
            CallbackMessage::Accept { callback, control } => {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    callback(control);
                }));
            }
            CallbackMessage::Stop => break,
        }
    }
}

fn set_ax_messaging_timeout(timeout_seconds: f32) -> Result<(), PlatformError> {
    unsafe {
        let system_wide = AXUIElementCreateSystemWide();
        if system_wide.is_null() {
            return Err(PlatformError::CannotComplete {
                reason: "AXUIElementCreateSystemWide returned null".into(),
            });
        }
        let _system_wide_owner = CFType::wrap_under_create_rule(system_wide as CFTypeRef);

        let err = AXUIElementSetMessagingTimeout(system_wide, timeout_seconds);
        if err == kAXErrorSuccess {
            Ok(())
        } else {
            Err(map_ax_error(err))
        }
    }
}

pub fn map_ax_error(error: AXError) -> PlatformError {
    if error == kAXErrorAPIDisabled {
        PlatformError::PermissionMissing {
            permission: "Accessibility".into(),
        }
    } else if error == kAXErrorCannotComplete {
        PlatformError::CannotComplete {
            reason: "AX cannot complete request".into(),
        }
    } else if error == kAXErrorAttributeUnsupported {
        PlatformError::UnsupportedField {
            reason: "AX attribute unsupported".into(),
        }
    } else if error == kAXErrorInvalidUIElement {
        PlatformError::StaleField
    } else if error == kAXErrorIllegalArgument {
        PlatformError::CannotComplete {
            reason: "AX illegal argument".into(),
        }
    } else if error == kAXErrorFailure {
        PlatformError::CannotComplete {
            reason: "AX request failed".into(),
        }
    } else {
        PlatformError::CannotComplete {
            reason: format!("AX error {error}"),
        }
    }
}

#[derive(Debug, Default)]
pub struct FocusTokenFactory {
    next_generation: u64,
}

impl FocusTokenFactory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn focused_field(
        &mut self,
        app: impl Into<String>,
        pid: Option<u32>,
        element_id: impl Into<String>,
    ) -> FieldHandle {
        self.next_generation += 1;
        FieldHandle {
            app: app.into(),
            pid,
            element_id: element_id.into(),
            generation: self.next_generation,
        }
    }
}

#[derive(Debug)]
pub struct CaretCoalescer {
    min_interval_ms: u64,
    last: Option<LastCaretEvent>,
}

#[derive(Debug)]
struct CaretFieldTracker {
    factory: FocusTokenFactory,
    current: Option<FieldHandle>,
    current_identity_key: Option<String>,
}

impl CaretFieldTracker {
    fn new() -> Self {
        Self {
            factory: FocusTokenFactory::new(),
            current: None,
            current_identity_key: None,
        }
    }

    fn field_for_event(&mut self, fallback_pid: i32, identity: &AxElementIdentity) -> FieldHandle {
        let app = identity.app_id(fallback_pid);
        let pid = identity.pid(fallback_pid);
        let element_id = identity.field_element_id();
        let identity_key = identity.stable_field_key();
        let pid = pid.or_else(|| u32::try_from(fallback_pid).ok());
        if let Some(current) = &self.current {
            if current.pid == pid
                && (current.element_id == element_id
                    || (self.current_identity_key.is_some()
                        && self.current_identity_key == identity_key))
            {
                return current.clone();
            }
        }

        let field = self.factory.focused_field(app, pid, element_id);
        self.current_identity_key = identity_key;
        self.current = Some(field.clone());
        field
    }
}

#[derive(Clone, Debug, PartialEq)]
struct LastCaretEvent {
    emitted_at_ms: u64,
    field: FieldHandle,
    rect: Option<ScreenRect>,
}

impl CaretCoalescer {
    pub fn new(min_interval_ms: u64) -> Self {
        Self {
            min_interval_ms,
            last: None,
        }
    }

    pub fn observe(
        &mut self,
        now_ms: u64,
        field: FieldHandle,
        rect: Option<ScreenRect>,
    ) -> Option<(FieldHandle, Option<ScreenRect>)> {
        let should_emit = self.last.as_ref().is_none_or(|last| {
            last.field != field
                || last.rect != rect
                || now_ms.saturating_sub(last.emitted_at_ms) >= self.min_interval_ms
        });

        if should_emit {
            self.last = Some(LastCaretEvent {
                emitted_at_ms: now_ms,
                field: field.clone(),
                rect,
            });
            Some((field, rect))
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObserverNotification {
    FocusChanged,
    CaretChanged,
}

#[derive(Clone, Debug, PartialEq)]
struct ObserverEvent {
    pid: i32,
    notification: ObserverNotification,
    identity: AxElementIdentity,
    rect: Option<ScreenRect>,
}

impl ObserverNotification {
    pub fn name(self) -> &'static str {
        match self {
            Self::FocusChanged => kAXFocusedUIElementChangedNotification,
            Self::CaretChanged => kAXSelectedTextChangedNotification,
        }
    }
}

trait ObserverBackend {
    type Observer;
    type Element;
    type Source;

    fn create_observer(&mut self, pid: i32) -> Result<Self::Observer, PlatformError>;
    fn run_loop_source(&mut self, observer: &Self::Observer)
        -> Result<Self::Source, PlatformError>;
    fn add_run_loop_source(&mut self, source: &Self::Source) -> Result<(), PlatformError>;
    fn remove_run_loop_source(&mut self, source: &Self::Source);
    fn add_notification(
        &mut self,
        observer: &Self::Observer,
        element: &Self::Element,
        notification: ObserverNotification,
        refcon: *mut c_void,
    ) -> Result<(), PlatformError>;
    fn remove_notification(
        &mut self,
        observer: &Self::Observer,
        element: &Self::Element,
        notification: ObserverNotification,
    );
}

struct AxObserverRegistration<B: ObserverBackend> {
    backend: B,
    observer: B::Observer,
    element: B::Element,
    source: B::Source,
    notifications: Vec<ObserverNotification>,
    #[cfg(test)]
    refcon: *mut c_void,
}

impl<B: ObserverBackend> AxObserverRegistration<B> {
    #[cfg(test)]
    fn register(
        backend: B,
        pid: i32,
        element: B::Element,
        notifications: &[ObserverNotification],
    ) -> Result<Self, PlatformError> {
        Self::register_with_refcon(backend, pid, element, notifications, ptr::null_mut())
    }

    fn register_with_refcon(
        mut backend: B,
        pid: i32,
        element: B::Element,
        notifications: &[ObserverNotification],
        refcon: *mut c_void,
    ) -> Result<Self, PlatformError> {
        let observer = backend.create_observer(pid)?;
        let source = backend.run_loop_source(&observer)?;
        backend.add_run_loop_source(&source)?;

        let mut registered = Vec::new();
        for notification in notifications {
            if let Err(err) = backend.add_notification(&observer, &element, *notification, refcon) {
                for registered_notification in &registered {
                    backend.remove_notification(&observer, &element, *registered_notification);
                }
                backend.remove_run_loop_source(&source);
                return Err(err);
            }
            registered.push(*notification);
        }

        Ok(Self {
            backend,
            observer,
            element,
            source,
            notifications: registered,
            #[cfg(test)]
            refcon,
        })
    }

    #[cfg(test)]
    fn refcon(&self) -> *mut c_void {
        self.refcon
    }
}

impl<B: ObserverBackend> Drop for AxObserverRegistration<B> {
    fn drop(&mut self) {
        for notification in &self.notifications {
            self.backend
                .remove_notification(&self.observer, &self.element, *notification);
        }
        self.backend.remove_run_loop_source(&self.source);
    }
}

struct RawAxObserverBackend {
    run_loop: CFRunLoop,
}

impl RawAxObserverBackend {
    pub fn current_run_loop() -> Self {
        Self {
            run_loop: CFRunLoop::get_current(),
        }
    }
}

struct RawAxObserver {
    observer: CFType,
}

impl RawAxObserver {
    fn as_ref(&self) -> AXObserverRef {
        self.observer.as_CFTypeRef() as AXObserverRef
    }
}

#[derive(Clone, Copy)]
struct RawAxElement {
    element: AXUIElementRef,
}

impl RawAxElement {
    /// The caller must keep the underlying AX element valid for the observer registration.
    unsafe fn borrowed(element: AXUIElementRef) -> Self {
        Self { element }
    }
}

type RawAxObserverRegistration = AxObserverRegistration<RawAxObserverBackend>;

unsafe fn register_raw_ax_observer_with_refcon(
    pid: i32,
    element: AXUIElementRef,
    notifications: &[ObserverNotification],
    refcon: *mut c_void,
) -> Result<RawAxObserverRegistration, PlatformError> {
    AxObserverRegistration::register_with_refcon(
        RawAxObserverBackend::current_run_loop(),
        pid,
        RawAxElement::borrowed(element),
        notifications,
        refcon,
    )
}

impl ObserverBackend for RawAxObserverBackend {
    type Observer = RawAxObserver;
    type Element = RawAxElement;
    type Source = CFRunLoopSource;

    fn create_observer(&mut self, pid: i32) -> Result<Self::Observer, PlatformError> {
        unsafe {
            let mut observer: AXObserverRef = ptr::null_mut();
            let err = AXObserverCreate(pid, ax_observer_callback, &mut observer);
            if err != kAXErrorSuccess {
                return Err(map_ax_error(err));
            }
            if observer.is_null() {
                return Err(PlatformError::CannotComplete {
                    reason: "AXObserverCreate returned null".into(),
                });
            }

            Ok(RawAxObserver {
                observer: CFType::wrap_under_create_rule(observer as CFTypeRef),
            })
        }
    }

    fn run_loop_source(
        &mut self,
        observer: &Self::Observer,
    ) -> Result<Self::Source, PlatformError> {
        unsafe {
            let source = AXObserverGetRunLoopSource(observer.as_ref());
            if source.is_null() {
                return Err(PlatformError::CannotComplete {
                    reason: "AXObserverGetRunLoopSource returned null".into(),
                });
            }

            Ok(CFRunLoopSource::wrap_under_get_rule(source))
        }
    }

    fn add_run_loop_source(&mut self, source: &Self::Source) -> Result<(), PlatformError> {
        unsafe {
            self.run_loop.add_source(source, kCFRunLoopCommonModes);
        }
        Ok(())
    }

    fn remove_run_loop_source(&mut self, source: &Self::Source) {
        unsafe {
            self.run_loop.remove_source(source, kCFRunLoopCommonModes);
        }
    }

    fn add_notification(
        &mut self,
        observer: &Self::Observer,
        element: &Self::Element,
        notification: ObserverNotification,
        refcon: *mut c_void,
    ) -> Result<(), PlatformError> {
        let notification = CFString::new(notification.name());
        unsafe {
            let err = AXObserverAddNotification(
                observer.as_ref(),
                element.element,
                notification.as_concrete_TypeRef(),
                refcon,
            );
            if err == kAXErrorSuccess {
                Ok(())
            } else {
                Err(map_ax_error(err))
            }
        }
    }

    fn remove_notification(
        &mut self,
        observer: &Self::Observer,
        element: &Self::Element,
        notification: ObserverNotification,
    ) {
        let notification = CFString::new(notification.name());
        unsafe {
            let _ = AXObserverRemoveNotification(
                observer.as_ref(),
                element.element,
                notification.as_concrete_TypeRef(),
            );
        }
    }
}

struct ObserverCallbackState {
    pid: i32,
    tx: mpsc::Sender<Message>,
    callback_tx: mpsc::Sender<CallbackMessage>,
    dispatch: ObserverDispatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AxElementIdentity {
    pointer_id: String,
    owner_pid: Option<u32>,
    identifier: Option<String>,
    role: Option<String>,
    subrole: Option<String>,
}

impl AxElementIdentity {
    fn pointer_only(pointer_id: impl Into<String>) -> Self {
        Self {
            pointer_id: pointer_id.into(),
            owner_pid: None,
            identifier: None,
            role: None,
            subrole: None,
        }
    }

    fn new(
        pointer_id: impl Into<String>,
        owner_pid: Option<u32>,
        identifier: Option<String>,
        role: Option<String>,
        subrole: Option<String>,
    ) -> Self {
        Self {
            pointer_id: pointer_id.into(),
            owner_pid,
            identifier,
            role,
            subrole,
        }
    }

    fn app_id(&self, fallback_pid: i32) -> AppId {
        self.owner_pid
            .map(|pid| format!("pid:{pid}"))
            .unwrap_or_else(|| format!("pid:{fallback_pid}"))
    }

    fn pid(&self, fallback_pid: i32) -> Option<u32> {
        self.owner_pid.or_else(|| u32::try_from(fallback_pid).ok())
    }

    fn field_element_id(&self) -> String {
        let mut parts = vec![format!(
            "ptr={}",
            escape_identity_component(&self.pointer_id)
        )];

        if let Some(pid) = self.owner_pid {
            parts.push(format!("pid={pid}"));
        }
        if let Some(identifier) = &self.identifier {
            parts.push(format!("id={}", escape_identity_component(identifier)));
        }
        if let Some(role) = &self.role {
            parts.push(format!("role={}", escape_identity_component(role)));
        }
        if let Some(subrole) = &self.subrole {
            parts.push(format!("subrole={}", escape_identity_component(subrole)));
        }

        format!("ax:{}", parts.join("|"))
    }

    fn stable_field_key(&self) -> Option<String> {
        let owner_pid = self.owner_pid?;
        if self.identifier.is_none() && self.role.is_none() && self.subrole.is_none() {
            return None;
        }

        let mut parts = vec![format!("pid={owner_pid}")];
        if let Some(identifier) = &self.identifier {
            parts.push(format!("id={}", escape_identity_component(identifier)));
        }
        if let Some(role) = &self.role {
            parts.push(format!("role={}", escape_identity_component(role)));
        }
        if let Some(subrole) = &self.subrole {
            parts.push(format!("subrole={}", escape_identity_component(subrole)));
        }

        Some(format!("ax:{}", parts.join("|")))
    }
}

fn escape_identity_component(value: &str) -> String {
    value.replace('\\', "\\\\").replace('|', "\\|")
}

unsafe fn resolve_ax_element_identity(
    element: AXUIElementRef,
) -> Result<AxElementIdentity, PlatformError> {
    let pointer_id = ax_element_id(element);
    if element.is_null() {
        return Ok(AxElementIdentity::pointer_only(pointer_id));
    }

    let owner_pid = read_ax_element_pid(element)?;
    let identifier = read_optional_ax_string_attribute(element, kAXIdentifierAttribute)?;
    let role = read_optional_ax_string_attribute(element, kAXRoleAttribute)?;
    let subrole = read_optional_ax_string_attribute(element, kAXSubroleAttribute)?;

    Ok(AxElementIdentity::new(
        pointer_id, owner_pid, identifier, role, subrole,
    ))
}

unsafe fn read_ax_element_pid(element: AXUIElementRef) -> Result<Option<u32>, PlatformError> {
    let mut pid = 0;
    let err = AXUIElementGetPid(element, &mut pid);
    if err != kAXErrorSuccess {
        return Err(map_ax_error(err));
    }

    Ok(u32::try_from(pid).ok())
}

/// Read a URL-valued AX attribute (`AXURL` is CFURL-typed; some
/// implementations return a CFString instead — accept both). Absent
/// attribute or unexpected type → `Ok(None)`, never an error: the domain
/// gate is fail-open by design.
unsafe fn read_optional_ax_url_attribute(
    element: AXUIElementRef,
    attribute: &str,
) -> Result<Option<String>, PlatformError> {
    let attribute = CFString::new(attribute);
    let mut value: CFTypeRef = ptr::null_mut();
    let err = AXUIElementCopyAttributeValue(element, attribute.as_concrete_TypeRef(), &mut value);
    if ax_attribute_absent(err) {
        return Ok(None);
    }
    if err != kAXErrorSuccess {
        return Err(map_ax_error(err));
    }
    if value.is_null() {
        return Ok(None);
    }
    let value = CFType::wrap_under_create_rule(value);
    if let Some(url) = value.downcast::<core_foundation::url::CFURL>() {
        // absolute() first: CFURLGetString returns the ORIGINAL string,
        // which for a base-relative CFURL is the relative half — a host can
        // only be extracted from the absolute form (review-c131).
        return Ok(Some(url.absolute().get_string().to_string()));
    }
    Ok(value.downcast::<CFString>().map(|s| s.to_string()))
}

/// The element's AX children, capped at `cap` (hang insurance on
/// pathological trees). Each child rides with a retained owner so the refs
/// stay valid while the caller holds the pair.
unsafe fn copy_ax_children(
    element: AXUIElementRef,
    cap: usize,
) -> Result<Vec<(AXUIElementRef, CFType)>, PlatformError> {
    let attribute = CFString::new("AXChildren");
    let mut value: CFTypeRef = ptr::null_mut();
    let err = AXUIElementCopyAttributeValue(element, attribute.as_concrete_TypeRef(), &mut value);
    if ax_attribute_absent(err) {
        return Ok(Vec::new());
    }
    if err != kAXErrorSuccess {
        return Err(map_ax_error(err));
    }
    if value.is_null() {
        return Ok(Vec::new());
    }
    let array_owner = CFType::wrap_under_create_rule(value);
    // Untyped CFArray (generic CFArray<CFType> has no runtime type check);
    // each item gets its own retain so the refs outlive the array owner.
    let Some(array) = array_owner.downcast::<CFArray>() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for item in array.iter().take(cap) {
        let item_ref = *item as CFTypeRef;
        if item_ref.is_null() {
            continue;
        }
        let owner = CFType::wrap_under_get_rule(item_ref);
        out.push((item_ref as AXUIElementRef, owner));
    }
    Ok(out)
}

/// Worker-side page-URL probe for the focused window of `pid` (browser-only
/// callers — the host pre-gates on `compat::is_browser`). Strategy per the
/// c128 design: the window's `AXDocument` string first (one cheap read),
/// else a bounded BFS for an `AXWebArea` exposing `AXURL` (WebKit, Chromium,
/// and Gecko all implement it on the web area). Per-node read errors skip
/// the node rather than aborting — a weird subtree must not kill the walk.
/// Runs ONLY on the AX worker thread (messaging timeout bounds hangs).
fn page_url_for_pid(pid: i32) -> Result<Option<String>, PlatformError> {
    /// BFS caps: hang insurance, not a search budget — the web area sits
    /// shallow under the window in all three engines. The node/child caps
    /// alone bound the walk at ~768 AX calls, which against a slow-but-alive
    /// renderer (each call near the 50ms messaging timeout) could stall the
    /// Focus arm for tens of seconds — the WALL-CLOCK budget below is the
    /// real bound (review-c131 Important).
    const MAX_DEPTH: usize = 8;
    const MAX_CHILDREN: usize = 64;
    const MAX_NODES: usize = 256;
    const MAX_WALK: std::time::Duration = std::time::Duration::from_millis(250);

    let started = std::time::Instant::now();
    let (app_element, _app_owner) = create_app_ax_element(pid)?;
    unsafe {
        let Some((window, window_owner)) =
            copy_ax_element_attribute(app_element, "AXFocusedWindow")?
        else {
            return Ok(None);
        };
        if let Ok(Some(doc)) = read_optional_ax_url_attribute(window, "AXDocument") {
            return Ok(Some(doc));
        }

        let mut queue: std::collections::VecDeque<(AXUIElementRef, CFType, usize)> =
            std::collections::VecDeque::new();
        queue.push_back((window, window_owner, 0));
        let mut visited = 0usize;
        while let Some((element, _owner, depth)) = queue.pop_front() {
            visited += 1;
            if visited > MAX_NODES || started.elapsed() > MAX_WALK {
                break;
            }
            if let Ok(Some(role)) = read_optional_ax_string_attribute(element, "AXRole") {
                if role == "AXWebArea" {
                    if let Ok(Some(url)) = read_optional_ax_url_attribute(element, "AXURL") {
                        return Ok(Some(url));
                    }
                    // A web area without AXURL: keep walking (frames nest).
                }
            }
            if depth >= MAX_DEPTH {
                continue;
            }
            if let Ok(children) = copy_ax_children(element, MAX_CHILDREN) {
                for (child, child_owner) in children {
                    queue.push_back((child, child_owner, depth + 1));
                }
            }
        }
    }
    Ok(None)
}

unsafe fn read_optional_ax_string_attribute(
    element: AXUIElementRef,
    attribute: &str,
) -> Result<Option<String>, PlatformError> {
    let attribute = CFString::new(attribute);
    let mut value: CFTypeRef = ptr::null_mut();
    let err = AXUIElementCopyAttributeValue(element, attribute.as_concrete_TypeRef(), &mut value);

    if ax_attribute_absent(err) {
        return Ok(None);
    }
    if err != kAXErrorSuccess {
        return Err(map_ax_error(err));
    }
    if value.is_null() {
        return Ok(None);
    }

    let value = CFType::wrap_under_create_rule(value);
    Ok(value.downcast::<CFString>().map(|value| value.to_string()))
}

fn resolve_retained_observer_event(
    pid: i32,
    notification: ObserverNotification,
    retained_element: Option<usize>,
    fallback_element_id: &str,
) -> ObserverEvent {
    if retained_element.is_none() {
        return ObserverEvent {
            pid,
            notification,
            identity: AxElementIdentity::pointer_only(fallback_element_id),
            rect: None,
        };
    }

    match resolve_retained_observer_element(notification, retained_element) {
        Ok((identity, rect)) => ObserverEvent {
            pid,
            notification,
            identity,
            rect,
        },
        Err(_) => ObserverEvent {
            pid,
            notification,
            identity: AxElementIdentity::pointer_only(fallback_element_id),
            rect: None,
        },
    }
}

fn resolve_retained_observer_element(
    notification: ObserverNotification,
    retained_element: Option<usize>,
) -> Result<(AxElementIdentity, Option<ScreenRect>), PlatformError> {
    let Some(retained_element) = retained_element else {
        return Err(PlatformError::UnsupportedField {
            reason: "observer callback did not include an AX element".into(),
        });
    };

    let element = retained_element as AXUIElementRef;
    let _owner = unsafe { CFType::wrap_under_create_rule(retained_element as CFTypeRef) };
    let identity = unsafe { resolve_ax_element_identity(element) }?;
    Ok((identity, observer_caret_rect(notification, element)))
}

fn observer_caret_rect(
    notification: ObserverNotification,
    element: AXUIElementRef,
) -> Option<ScreenRect> {
    if notification != ObserverNotification::CaretChanged {
        return None;
    }

    let selected_range = unsafe { read_required_ax_range_attribute(element) }.ok()?;
    let caret = selected_range.location.max(0);
    resolve_caret_rect_with_marker_first(
        caret,
        || unsafe { read_ax_bounds_for_selected_text_marker_range(element) },
        |location, length| unsafe { read_ax_bounds_for_range(element, location, length) },
    )
    .ok()
    .flatten()
}

fn dispatch_observer_event(dispatch: ObserverDispatch, event: ObserverEvent) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        dispatch(event);
    }));
}

unsafe fn decode_observer_notification(notification: CFStringRef) -> Option<ObserverNotification> {
    if notification.is_null() {
        return None;
    }

    let notification = CFString::wrap_under_get_rule(notification);
    let name = notification.to_string();
    if name == ObserverNotification::FocusChanged.name() {
        Some(ObserverNotification::FocusChanged)
    } else if name == ObserverNotification::CaretChanged.name() {
        Some(ObserverNotification::CaretChanged)
    } else {
        None
    }
}

unsafe extern "C" fn ax_observer_callback(
    _observer: AXObserverRef,
    element: AXUIElementRef,
    notification: CFStringRef,
    refcon: *mut c_void,
) {
    // C→Rust FFI boundary: a panic unwinding into the AX run loop is UB. Shield
    // the whole body (matching the crate's dispatcher convention); the callback
    // returns (), so a caught panic is simply swallowed.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if refcon.is_null() {
            return;
        }

        let Some(notification) = decode_observer_notification(notification) else {
            return;
        };

        let state = unsafe { &*(refcon as *const ObserverCallbackState) };
        let fallback_element_id = ax_element_id(element);
        let retained_element = retain_observer_element(element);
        let message = Message::ObserverEvent {
            pid: state.pid,
            notification,
            retained_element,
            fallback_element_id,
            dispatch: Arc::clone(&state.dispatch),
            callback_tx: state.callback_tx.clone(),
        };

        // Ownership of the CFRetain in `retained_element` is balanced manually: the
        // worker releases it via `resolve_retained_observer_element` (create rule)
        // when it processes the message, and the send-failure path below releases it
        // here. One bounded gap remains and is accepted: if the worker has already
        // stopped, a still-queued ObserverEvent is dropped without processing, so its
        // CFRetain leaks. This is shutdown-only (the worker stops exactly once) and
        // touches at most the messages in flight at that instant — negligible.
        // ponytail: bounded shutdown leak; upgrade to a Send Drop-guard around the
        // retained element (releasing on drop, `forget` on the create-rule handoff)
        // if the worker's drain/shutdown logic ever changes to risk per-event leaks.
        if state.tx.send(message).is_err() {
            release_retained_observer_element(retained_element);
        }
    }));
}

fn ax_element_id(element: AXUIElementRef) -> String {
    if element.is_null() {
        "ax:null".into()
    } else {
        format!("ax:0x{:x}", element as usize)
    }
}

fn retain_observer_element(element: AXUIElementRef) -> Option<usize> {
    if element.is_null() {
        return None;
    }

    let retained = unsafe { CFRetain(element as CFTypeRef) };
    if retained.is_null() {
        None
    } else {
        Some(retained as usize)
    }
}

fn release_retained_observer_element(element: Option<usize>) {
    if let Some(element) = element {
        unsafe {
            CFRelease(element as CFTypeRef);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::{define_class, msg_send, AnyThread, DefinedClass};
    use objc2_app_kit::NSPasteboardItemDataProvider;
    use objc2_foundation::{NSObject, NSObjectProtocol};

    #[test]
    fn screen_context_text_with_zero_max_chars_returns_none_before_any_ffi() {
        // The max_chars==0 guard short-circuits before any permission check or
        // screen-capture FFI, so it is safe to call without a screen-recording
        // entitlement and must yield None for both caret-rect shapes.
        assert!(screen_context_text(None, 0).is_none());
        let rect = ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 20.0,
        };
        assert!(screen_context_text(Some(rect), 0).is_none());
    }
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;
    use std::thread;

    #[derive(Debug)]
    struct TestPasteboardProviderIvars {
        provided_count: Arc<AtomicUsize>,
        value: String,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements relevant to this
        // test-only data provider.
        #[unsafe(super = NSObject)]
        #[thread_kind = AnyThread]
        #[ivars = TestPasteboardProviderIvars]
        struct TestPasteboardProvider;

        // SAFETY: NSObjectProtocol has no additional safety requirements.
        unsafe impl NSObjectProtocol for TestPasteboardProvider {}

        // SAFETY: The method signature matches NSPasteboardItemDataProvider.
        unsafe impl NSPasteboardItemDataProvider for TestPasteboardProvider {
            #[allow(non_snake_case)]
            #[unsafe(method(pasteboard:item:provideDataForType:))]
            fn pasteboard_item_provideDataForType(
                &self,
                _pasteboard: Option<&NSPasteboard>,
                item: &NSPasteboardItem,
                pasteboard_type: &objc2_app_kit::NSPasteboardType,
            ) {
                self.ivars().provided_count.fetch_add(1, Ordering::SeqCst);
                item.setString_forType(&NSString::from_str(&self.ivars().value), pasteboard_type);
            }
        }
    );

    impl TestPasteboardProvider {
        fn new(value: &str, provided_count: Arc<AtomicUsize>) -> Retained<Self> {
            let this = Self::alloc().set_ivars(TestPasteboardProviderIvars {
                provided_count,
                value: value.to_string(),
            });
            // SAFETY: The signature of NSObject's init method is correct.
            unsafe { msg_send![super(this), init] }
        }
    }

    #[derive(Debug)]
    struct TestNoopPasteboardProviderIvars {
        provided_count: Arc<AtomicUsize>,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements relevant to this
        // test-only data provider.
        #[unsafe(super = NSObject)]
        #[thread_kind = AnyThread]
        #[ivars = TestNoopPasteboardProviderIvars]
        struct TestNoopPasteboardProvider;

        // SAFETY: NSObjectProtocol has no additional safety requirements.
        unsafe impl NSObjectProtocol for TestNoopPasteboardProvider {}

        // SAFETY: The method signature matches NSPasteboardItemDataProvider.
        unsafe impl NSPasteboardItemDataProvider for TestNoopPasteboardProvider {
            #[allow(non_snake_case)]
            #[unsafe(method(pasteboard:item:provideDataForType:))]
            fn pasteboard_item_provideDataForType(
                &self,
                _pasteboard: Option<&NSPasteboard>,
                _item: &NSPasteboardItem,
                _pasteboard_type: &objc2_app_kit::NSPasteboardType,
            ) {
                // Deliberately set no data: the item advertises the type but the
                // provider yields nothing for it.
                self.ivars().provided_count.fetch_add(1, Ordering::SeqCst);
            }
        }
    );

    impl TestNoopPasteboardProvider {
        fn new(provided_count: Arc<AtomicUsize>) -> Retained<Self> {
            let this = Self::alloc().set_ivars(TestNoopPasteboardProviderIvars { provided_count });
            // SAFETY: The signature of NSObject's init method is correct.
            unsafe { msg_send![super(this), init] }
        }
    }

    struct FakeObserverBackend {
        log: Arc<Mutex<Vec<String>>>,
        fail_on: Option<ObserverNotification>,
    }

    impl FakeObserverBackend {
        fn new(log: Arc<Mutex<Vec<String>>>) -> Self {
            Self { log, fail_on: None }
        }

        fn failing_on(log: Arc<Mutex<Vec<String>>>, notification: ObserverNotification) -> Self {
            Self {
                log,
                fail_on: Some(notification),
            }
        }

        fn push(&self, event: impl Into<String>) {
            self.log.lock().unwrap().push(event.into());
        }
    }

    impl ObserverBackend for FakeObserverBackend {
        type Observer = String;
        type Element = String;
        type Source = String;

        fn create_observer(&mut self, pid: i32) -> Result<Self::Observer, PlatformError> {
            self.push(format!("create_observer:{pid}"));
            Ok(format!("observer-{pid}"))
        }

        fn run_loop_source(
            &mut self,
            observer: &Self::Observer,
        ) -> Result<Self::Source, PlatformError> {
            self.push(format!("source:{observer}"));
            Ok(format!("source-{observer}"))
        }

        fn add_run_loop_source(&mut self, source: &Self::Source) -> Result<(), PlatformError> {
            self.push(format!("add_source:{source}"));
            Ok(())
        }

        fn remove_run_loop_source(&mut self, source: &Self::Source) {
            self.push(format!("remove_source:{source}"));
        }

        fn add_notification(
            &mut self,
            observer: &Self::Observer,
            element: &Self::Element,
            notification: ObserverNotification,
            refcon: *mut c_void,
        ) -> Result<(), PlatformError> {
            if self.fail_on == Some(notification) {
                self.push(format!(
                    "fail_add:{observer}:{element}:{}",
                    notification.name()
                ));
                return Err(PlatformError::Timeout);
            }

            self.push(format!(
                "add:{observer}:{element}:{}:{}",
                notification.name(),
                if refcon.is_null() { "null" } else { "refcon" }
            ));
            Ok(())
        }

        fn remove_notification(
            &mut self,
            observer: &Self::Observer,
            element: &Self::Element,
            notification: ObserverNotification,
        ) {
            self.push(format!(
                "remove:{observer}:{element}:{}",
                notification.name()
            ));
        }
    }

    struct FakeAxWorkerLoop {
        events: Arc<Mutex<Vec<String>>>,
        messages: VecDeque<Result<Message, mpsc::RecvTimeoutError>>,
    }

    impl FakeAxWorkerLoop {
        fn new(messages: impl Into<VecDeque<Result<Message, mpsc::RecvTimeoutError>>>) -> Self {
            Self {
                events: Arc::new(Mutex::new(Vec::new())),
                messages: messages.into(),
            }
        }

        fn events(&self) -> Arc<Mutex<Vec<String>>> {
            Arc::clone(&self.events)
        }
    }

    impl AxWorkerLoop for FakeAxWorkerLoop {
        fn recv(&mut self) -> Result<Message, mpsc::RecvTimeoutError> {
            self.events.lock().unwrap().push("recv".into());
            self.messages
                .pop_front()
                .unwrap_or(Err(mpsc::RecvTimeoutError::Disconnected))
        }

        fn pump_run_loop(&mut self) {
            self.events.lock().unwrap().push("pump".into());
        }
    }

    fn stop_message() -> Result<Message, mpsc::RecvTimeoutError> {
        Ok(Message::Stop)
    }

    fn timeout_message() -> Result<Message, mpsc::RecvTimeoutError> {
        Err(mpsc::RecvTimeoutError::Timeout)
    }

    fn run_message(label: &'static str) -> Result<Message, mpsc::RecvTimeoutError> {
        let (reply, _rx) = mpsc::channel();
        Ok(Message::Run {
            job: Box::new(move || Box::new(label) as Box<dyn Any + Send>),
            reply,
        })
    }

    fn observer_event(
        notification: ObserverNotification,
        identity: AxElementIdentity,
    ) -> ObserverEvent {
        observer_event_for_pid(42, notification, identity, None)
    }

    fn observer_event_with_rect(
        notification: ObserverNotification,
        identity: AxElementIdentity,
        rect: Option<ScreenRect>,
    ) -> ObserverEvent {
        observer_event_for_pid(42, notification, identity, rect)
    }

    fn observer_event_for_pid(
        pid: i32,
        notification: ObserverNotification,
        identity: AxElementIdentity,
        rect: Option<ScreenRect>,
    ) -> ObserverEvent {
        ObserverEvent {
            pid,
            notification,
            identity,
            rect,
        }
    }

    fn pointer_identity(element_id: &str) -> AxElementIdentity {
        AxElementIdentity::pointer_only(element_id)
    }

    fn resolved_identity(
        pointer_id: &str,
        owner_pid: u32,
        identifier: Option<&str>,
    ) -> AxElementIdentity {
        AxElementIdentity::new(
            pointer_id,
            Some(owner_pid),
            identifier.map(str::to_string),
            Some("AXTextArea".into()),
            None,
        )
    }

    fn observer_message(
        dispatch: ObserverDispatch,
        callback_tx: mpsc::Sender<CallbackMessage>,
    ) -> Result<Message, mpsc::RecvTimeoutError> {
        Ok(Message::ObserverEvent {
            pid: 42,
            notification: ObserverNotification::FocusChanged,
            retained_element: None,
            fallback_element_id: "ax:null".into(),
            dispatch,
            callback_tx,
        })
    }

    struct DropTrackedResource {
        expected_thread: ThreadId,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl Drop for DropTrackedResource {
        fn drop(&mut self) {
            self.log.lock().unwrap().push(format!(
                "drop_on_worker:{}",
                thread::current().id() == self.expected_thread
            ));
        }
    }

    #[derive(Clone)]
    struct FakeObserverInstall {
        pid: i32,
        target: ObserverInstallTarget,
        notifications: Vec<ObserverNotification>,
        dispatch: ObserverDispatch,
    }

    /// Boxed inside the fake observer's `ObserverResource` so a test can observe
    /// teardown deterministically instead of sleeping. When the rebind poller
    /// replaces a binding (e.g. frontmost → None), the old `ObserverResource`
    /// drops, dropping this and recording the torn-down pid.
    struct TeardownSignal {
        pid: i32,
        log: Arc<Mutex<Vec<i32>>>,
    }

    impl Drop for TeardownSignal {
        fn drop(&mut self) {
            if let Ok(mut log) = self.log.lock() {
                log.push(self.pid);
            }
        }
    }

    #[derive(Clone)]
    struct FakeAcceptTapInstall {
        kind: AcceptTapKind,
        handler: Arc<AcceptTapHandler>,
    }

    struct TestAdapterConfig {
        frontmost_pid: Option<i32>,
        installs: Arc<Mutex<Vec<FakeObserverInstall>>>,
        install_error: Option<PlatformError>,
        now_ms: Arc<NowMsProvider>,
        secure_input_enabled: Arc<SecureInputProvider>,
        process_exists: Arc<ProcessExistsProvider>,
        synthetic_key_poster: Arc<SyntheticKeyPoster>,
        pasteboard_poster: Arc<PasteboardPoster>,
        backspace_poster: Arc<BackspacePoster>,
        accept_tap_installs: Arc<Mutex<Vec<FakeAcceptTapInstall>>>,
        /// Flat install/drop event log for ORDER assertions (the rearm
        /// drop-before-install pin): "install:<Kind>" per installer call,
        /// "drop" per fake tap-resource drop.
        accept_tap_events: Arc<Mutex<Vec<String>>>,
    }

    impl TestAdapterConfig {
        fn new(
            frontmost_pid: Option<i32>,
            installs: Arc<Mutex<Vec<FakeObserverInstall>>>,
            install_error: Option<PlatformError>,
        ) -> Self {
            Self {
                frontmost_pid,
                installs,
                install_error,
                now_ms: Arc::new(|| 1000),
                secure_input_enabled: Arc::new(|| false),
                process_exists: Arc::new(|_| true),
                synthetic_key_poster: Arc::new(|_, _| Ok(())),
                pasteboard_poster: Arc::new(|_, _| Ok(())),
                backspace_poster: Arc::new(|_, _| Ok(())),
                accept_tap_installs: Arc::new(Mutex::new(Vec::new())),
                accept_tap_events: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    fn test_adapter(
        frontmost_pid: Option<i32>,
        installs: Arc<Mutex<Vec<FakeObserverInstall>>>,
        install_error: Option<PlatformError>,
    ) -> MacosPlatformAdapter {
        test_adapter_with_hooks(TestAdapterConfig::new(
            frontmost_pid,
            installs,
            install_error,
        ))
    }

    fn test_adapter_with_secure_input(secure_input_enabled: bool) -> MacosPlatformAdapter {
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        config.secure_input_enabled = Arc::new(move || secure_input_enabled);
        test_adapter_with_hooks(config)
    }

    fn test_adapter_with_hooks(config: TestAdapterConfig) -> MacosPlatformAdapter {
        let TestAdapterConfig {
            frontmost_pid,
            installs,
            install_error,
            now_ms,
            secure_input_enabled,
            process_exists,
            synthetic_key_poster,
            pasteboard_poster,
            backspace_poster,
            accept_tap_installs,
            accept_tap_events,
        } = config;
        let worker = AxWorker::start_with_setup(|_| Ok(())).expect("worker");
        let frontmost_pid = Arc::new(move || frontmost_pid);
        let observer_installer = Arc::new(move |pid, target, notifications, dispatch| {
            if let Some(err) = install_error.clone() {
                return Err(err);
            }

            installs.lock().unwrap().push(FakeObserverInstall {
                pid,
                target,
                notifications,
                dispatch,
            });
            Ok(ObserverResource::new("observer"))
        });
        struct TapDropLogger {
            events: Arc<Mutex<Vec<String>>>,
        }
        impl Drop for TapDropLogger {
            fn drop(&mut self) {
                if let Ok(mut events) = self.events.lock() {
                    events.push("drop".into());
                }
            }
        }
        let accept_tap_installer = Arc::new(move |kind, handler: Arc<AcceptTapHandler>| {
            accept_tap_events
                .lock()
                .unwrap()
                .push(format!("install:{kind:?}"));
            accept_tap_installs
                .lock()
                .unwrap()
                .push(FakeAcceptTapInstall { kind, handler });
            Ok(AcceptTapResource::new(TapDropLogger {
                events: Arc::clone(&accept_tap_events),
            }))
        });

        MacosPlatformAdapter::with_worker_test_hooks(
            worker,
            AdapterTestHooks {
                callback_dispatcher: CallbackDispatcher::new().expect("CallbackDispatcher"),
                frontmost_pid,
                now_ms,
                secure_input_enabled,
                process_exists,
                synthetic_key_poster,
                pasteboard_poster,
                backspace_poster,
                observer_installer,
                accept_tap_installer,
            },
        )
    }

    fn test_adapter_with_dynamic_frontmost(
        frontmost_pid: Arc<Mutex<Option<i32>>>,
        installs: Arc<Mutex<Vec<FakeObserverInstall>>>,
        teardowns: Arc<Mutex<Vec<i32>>>,
    ) -> MacosPlatformAdapter {
        let worker = AxWorker::start_with_setup(|_| Ok(())).expect("worker");
        let frontmost_pid = Arc::new(move || *frontmost_pid.lock().unwrap());
        let observer_installer = Arc::new(move |pid, target, notifications, dispatch| {
            installs.lock().unwrap().push(FakeObserverInstall {
                pid,
                target,
                notifications,
                dispatch,
            });
            Ok(ObserverResource::new(TeardownSignal {
                pid,
                log: Arc::clone(&teardowns),
            }))
        });
        let accept_tap_installer = Arc::new(|kind, handler: Arc<AcceptTapHandler>| {
            let _ = (kind, handler);
            Ok(AcceptTapResource::new("accept-tap"))
        });

        MacosPlatformAdapter::with_worker_test_hooks(
            worker,
            AdapterTestHooks {
                callback_dispatcher: CallbackDispatcher::new().expect("CallbackDispatcher"),
                frontmost_pid,
                now_ms: Arc::new(|| 1000),
                secure_input_enabled: Arc::new(|| false),
                process_exists: Arc::new(|_| true),
                synthetic_key_poster: Arc::new(|_, _| Ok(())),
                pasteboard_poster: Arc::new(|_, _| Ok(())),
                backspace_poster: Arc::new(|_, _| Ok(())),
                observer_installer,
                accept_tap_installer,
            },
        )
    }

    /// Upper bound for the test polling waits below. Generous on purpose: the
    /// full `cargo test --workspace --all-targets` run launches many test
    /// binaries in parallel, oversubscribing the cores, so the 250 ms
    /// (`APP_REBIND_POLL_INTERVAL`) rebind-poll thread can be scheduled slowly.
    /// Each waiter returns the instant the count is reached, so a large ceiling
    /// costs nothing on green and only bounds genuine hangs. (The historical
    /// `focus_subscription_rebinds_*` flake was a synchronization race on the
    /// binding swap, fixed by waiting on the teardown signal — not a deadline
    /// timeout; this ceiling is defensive insurance against load, not that fix.)
    const WAIT_DEADLINE: Duration = Duration::from_secs(10);

    fn wait_for_install_count(installs: &Arc<Mutex<Vec<FakeObserverInstall>>>, expected: usize) {
        let deadline = SystemTime::now() + WAIT_DEADLINE;
        while SystemTime::now() < deadline {
            if installs.lock().unwrap().len() >= expected {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }

        assert_eq!(installs.lock().unwrap().len(), expected);
    }

    fn wait_for_accept_tap_count(
        installs: &Arc<Mutex<Vec<FakeAcceptTapInstall>>>,
        expected: usize,
    ) {
        let deadline = SystemTime::now() + WAIT_DEADLINE;
        while SystemTime::now() < deadline {
            if installs.lock().unwrap().len() >= expected {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }

        assert_eq!(installs.lock().unwrap().len(), expected);
    }

    fn wait_for_vec_count<T>(items: &Arc<Mutex<Vec<T>>>, expected: usize) {
        let deadline = SystemTime::now() + WAIT_DEADLINE;
        while SystemTime::now() < deadline {
            if items.lock().unwrap().len() >= expected {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }

        assert_eq!(items.lock().unwrap().len(), expected);
    }

    fn write_test_pasteboard_items(
        pasteboard: &NSPasteboard,
        items: Vec<Retained<NSPasteboardItem>>,
    ) -> bool {
        let writing_items = items
            .into_iter()
            .map(ProtocolObject::<dyn NSPasteboardWriting>::from_retained)
            .collect::<Vec<_>>();
        let writing_array = NSArray::from_retained_slice(&writing_items);
        pasteboard.writeObjects(&writing_array)
    }

    #[test]
    fn ax_worker_runs_jobs_on_dedicated_non_calling_thread() {
        let worker = AxWorker::start_with_setup(|_| Ok(())).expect("worker");
        let caller = thread::current().id();

        let worker_thread = worker.run(|| thread::current().id()).expect("run");

        assert_ne!(worker_thread, caller);
        assert_eq!(worker_thread, worker.thread_id());
    }

    #[test]
    fn ax_worker_serializes_jobs_on_same_thread() {
        let worker = AxWorker::start_with_setup(|_| Ok(())).expect("worker");

        let first = worker.run(|| thread::current().id()).expect("first");
        let second = worker.run(|| thread::current().id()).expect("second");

        assert_eq!(first, second);
    }

    #[test]
    fn ax_worker_setup_runs_on_worker_with_timeout() {
        let seen = Arc::new(Mutex::new(None));
        let seen_in_setup = Arc::clone(&seen);

        let worker = AxWorker::start_with_setup(move |timeout| {
            *seen_in_setup.lock().unwrap() = Some((thread::current().id(), timeout));
            Ok(())
        })
        .expect("worker");

        assert_eq!(*seen.lock().unwrap(), Some((worker.thread_id(), 0.05)));
    }

    #[test]
    fn ax_worker_reports_setup_error() {
        let err = AxWorker::start_with_setup(|_| Err(PlatformError::Timeout)).unwrap_err();

        assert_eq!(err, PlatformError::Timeout);
    }

    #[test]
    fn ax_worker_loop_pumps_run_loop_on_idle_timeout() {
        let worker_loop = FakeAxWorkerLoop::new(VecDeque::from([
            timeout_message(),
            timeout_message(),
            stop_message(),
        ]));
        let events = worker_loop.events();
        let (started_tx, started_rx) = mpsc::channel();

        run_ax_worker_loop(worker_loop, started_tx, |_| Ok(()), 0.05);

        assert_eq!(
            started_rx.recv().expect("started").unwrap(),
            thread::current().id()
        );
        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["recv", "pump", "recv", "pump", "recv"]
        );
    }

    #[test]
    fn ax_worker_loop_pumps_after_job_to_avoid_run_loop_starvation() {
        let worker_loop =
            FakeAxWorkerLoop::new(VecDeque::from([run_message("job"), stop_message()]));
        let events = worker_loop.events();
        let (started_tx, started_rx) = mpsc::channel();

        run_ax_worker_loop(worker_loop, started_tx, |_| Ok(()), 0.05);

        assert_eq!(
            started_rx.recv().expect("started").unwrap(),
            thread::current().id()
        );
        assert_eq!(events.lock().unwrap().as_slice(), ["recv", "pump", "recv"]);
    }

    #[test]
    fn ax_worker_loop_delivers_observer_callbacks_off_worker_thread() {
        let callback_dispatcher = CallbackDispatcher::new().expect("CallbackDispatcher");
        let (callback_thread_tx, callback_thread_rx) = mpsc::channel();
        let dispatch = Arc::new(move |_| {
            callback_thread_tx
                .send(thread::current().id())
                .expect("callback thread id");
        });
        let worker_loop = FakeAxWorkerLoop::new(VecDeque::from([
            observer_message(dispatch, callback_dispatcher.sender()),
            stop_message(),
        ]));
        let (started_tx, started_rx) = mpsc::channel();

        run_ax_worker_loop(worker_loop, started_tx, |_| Ok(()), 0.05);

        let ax_worker_thread = started_rx.recv().expect("started").unwrap();
        let callback_thread = callback_thread_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("callback delivered");
        assert_ne!(callback_thread, ax_worker_thread);
    }

    #[test]
    fn callback_dispatcher_contains_panics_and_keeps_running() {
        let callback_dispatcher = CallbackDispatcher::new().expect("CallbackDispatcher");
        let (delivered_tx, delivered_rx) = mpsc::channel();

        callback_dispatcher
            .sender()
            .send(CallbackMessage::Dispatch {
                dispatch: Arc::new(|_| panic!("callback panic is contained")),
                event: observer_event(
                    ObserverNotification::FocusChanged,
                    pointer_identity("ax:panic"),
                ),
            })
            .expect("send panicking callback");
        callback_dispatcher
            .sender()
            .send(CallbackMessage::Dispatch {
                dispatch: Arc::new(move |_| {
                    delivered_tx.send(()).expect("delivered");
                }),
                event: observer_event(
                    ObserverNotification::FocusChanged,
                    pointer_identity("ax:after-panic"),
                ),
            })
            .expect("send follow-up callback");

        delivered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("dispatcher continued after panic");
    }

    #[test]
    fn focused_element_safety_poll_sends_worker_poll_messages_until_dropped() {
        let (tx, rx) = mpsc::channel();
        let (callback_tx, _callback_rx) = mpsc::channel();
        let poller = start_focused_element_safety_poll(
            tx,
            42,
            ObserverNotification::CaretChanged,
            Arc::new(|_| {}),
            callback_tx,
            Duration::from_millis(5),
        )
        .expect("failed to start caret safety poll thread");

        let message = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("poll message");
        let Message::PollFocusedElement {
            pid, notification, ..
        } = message
        else {
            panic!("expected focused element poll message");
        };
        assert_eq!(pid, 42);
        assert_eq!(notification, ObserverNotification::CaretChanged);

        drop(poller);
    }

    #[test]
    fn ax_worker_installs_and_drops_resources_on_worker_thread() {
        let worker = AxWorker::start_with_setup(|_| Ok(())).expect("worker");
        let worker_thread = worker.thread_id();
        let log = Arc::new(Mutex::new(Vec::new()));
        let log_in_resource = Arc::clone(&log);

        let handle = worker
            .install_resource(move || {
                log_in_resource.lock().unwrap().push(format!(
                    "install_on_worker:{}",
                    thread::current().id() == worker_thread
                ));
                Ok(Box::new(DropTrackedResource {
                    expected_thread: worker_thread,
                    log: Arc::clone(&log_in_resource),
                }) as WorkerResource)
            })
            .expect("install resource");

        assert_eq!(worker.resource_count().expect("resource count"), 1);
        assert!(handle.close().expect("close resource"));
        assert_eq!(worker.resource_count().expect("resource count"), 0);
        assert_eq!(
            log.lock().unwrap().as_slice(),
            ["install_on_worker:true", "drop_on_worker:true"]
        );
    }

    #[test]
    fn ax_worker_failed_resource_install_does_not_store_resource() {
        let worker = AxWorker::start_with_setup(|_| Ok(())).expect("worker");

        let err = worker
            .install_resource(|| Err(PlatformError::Timeout))
            .unwrap_err();

        assert_eq!(err, PlatformError::Timeout);
        assert_eq!(worker.resource_count().expect("resource count"), 0);
    }

    #[test]
    fn ax_error_mapping_distinguishes_contract_errors() {
        assert_eq!(
            map_ax_error(accessibility_sys::kAXErrorAPIDisabled),
            PlatformError::PermissionMissing {
                permission: "Accessibility".into(),
            }
        );
        assert_eq!(
            map_ax_error(accessibility_sys::kAXErrorCannotComplete),
            PlatformError::CannotComplete {
                reason: "AX cannot complete request".into(),
            }
        );
        assert_eq!(
            map_ax_error(accessibility_sys::kAXErrorAttributeUnsupported),
            PlatformError::UnsupportedField {
                reason: "AX attribute unsupported".into(),
            }
        );
        assert_eq!(
            map_ax_error(accessibility_sys::kAXErrorInvalidUIElement),
            PlatformError::StaleField
        );
    }

    #[test]
    fn map_ax_error_covers_illegal_argument_failure_and_unknown() {
        assert_eq!(
            map_ax_error(accessibility_sys::kAXErrorIllegalArgument),
            PlatformError::CannotComplete {
                reason: "AX illegal argument".into(),
            }
        );
        assert_eq!(
            map_ax_error(accessibility_sys::kAXErrorFailure),
            PlatformError::CannotComplete {
                reason: "AX request failed".into(),
            }
        );

        // Any AX code not explicitly matched falls through to the generic
        // CannotComplete reason that embeds the raw error code.
        let unknown: AXError = -25299;
        assert_eq!(
            map_ax_error(unknown),
            PlatformError::CannotComplete {
                reason: format!("AX error {unknown}"),
            }
        );
    }

    #[test]
    fn focus_token_factory_assigns_new_generation_for_each_focus_event() {
        let mut factory = FocusTokenFactory::new();

        let first = factory.focused_field("TextEdit", Some(42), "element");
        let second = factory.focused_field("TextEdit", Some(42), "element");

        assert_eq!(first.generation, 1);
        assert_eq!(second.generation, 2);
        assert_eq!(second.element_id, "element");
    }

    #[test]
    fn ax_element_identity_prefers_owner_pid_for_field_metadata() {
        let identity = AxElementIdentity::new(
            "ax:0x123",
            Some(42),
            Some("editor".into()),
            Some("AXTextArea".into()),
            Some("AXSecureTextField".into()),
        );

        assert_eq!(identity.app_id(7), "pid:42");
        assert_eq!(identity.pid(7), Some(42));
        assert_eq!(
            identity.field_element_id(),
            "ax:ptr=ax:0x123|pid=42|id=editor|role=AXTextArea|subrole=AXSecureTextField"
        );
    }

    #[test]
    fn ax_element_identity_falls_back_to_frontmost_pid_until_resolved() {
        let identity = AxElementIdentity::pointer_only("ax:0x123");

        assert_eq!(identity.app_id(7), "pid:7");
        assert_eq!(identity.pid(7), Some(7));
        assert_eq!(identity.field_element_id(), "ax:ptr=ax:0x123");
    }

    #[test]
    fn ax_element_identity_escapes_separator_characters() {
        let identity = AxElementIdentity::new(
            r"ax:\0x123",
            Some(42),
            Some(r"editor|main".into()),
            Some(r"AX\TextArea".into()),
            None,
        );

        assert_eq!(
            identity.field_element_id(),
            r"ax:ptr=ax:\\0x123|pid=42|id=editor\|main|role=AX\\TextArea"
        );
    }

    #[test]
    fn ax_absent_predicates_classify_error_sets() {
        // Plain attribute reads: absent on Unsupported/NoValue only.
        assert!(ax_attribute_absent(kAXErrorAttributeUnsupported));
        assert!(ax_attribute_absent(kAXErrorNoValue));
        assert!(!ax_attribute_absent(kAXErrorIllegalArgument));
        assert!(!ax_attribute_absent(
            kAXErrorParameterizedAttributeUnsupported
        ));

        // Settable checks/writes: also IllegalArgument.
        assert!(ax_settable_absent(kAXErrorAttributeUnsupported));
        assert!(ax_settable_absent(kAXErrorNoValue));
        assert!(ax_settable_absent(kAXErrorIllegalArgument));
        assert!(!ax_settable_absent(
            kAXErrorParameterizedAttributeUnsupported
        ));

        // Parameterized range/marker queries: also ParameterizedAttributeUnsupported.
        assert!(ax_parameterized_absent(kAXErrorAttributeUnsupported));
        assert!(ax_parameterized_absent(kAXErrorNoValue));
        assert!(ax_parameterized_absent(kAXErrorIllegalArgument));
        assert!(ax_parameterized_absent(
            kAXErrorParameterizedAttributeUnsupported
        ));

        // None classify a real failure or success as "absent".
        for err in [kAXErrorSuccess, kAXErrorCannotComplete, kAXErrorFailure] {
            assert!(!ax_attribute_absent(err));
            assert!(!ax_settable_absent(err));
            assert!(!ax_parameterized_absent(err));
        }
    }

    #[test]
    fn text_range_rect_bounds_read_fails_closed_when_bounds_absent() {
        // The fail-closed seam behind `text_range_rect`: an absent/unsupported
        // parameterized bounds attribute must classify as `Absent`, which the
        // FFI reads degrade to `Ok(None)` (no rect, caret/popup fallback) — never
        // an error. Any other non-success code is a genuine `Failed` to surface.
        for absent in [
            kAXErrorAttributeUnsupported,
            kAXErrorNoValue,
            kAXErrorIllegalArgument,
            kAXErrorParameterizedAttributeUnsupported,
        ] {
            assert_eq!(classify_ax_bounds_read(absent), AxBoundsRead::Absent);
        }
        for failed in [kAXErrorCannotComplete, kAXErrorFailure] {
            assert_eq!(classify_ax_bounds_read(failed), AxBoundsRead::Failed);
        }
        assert_eq!(
            classify_ax_bounds_read(kAXErrorSuccess),
            AxBoundsRead::Present
        );
    }

    #[test]
    fn caret_field_tracker_reuses_field_on_identical_element_id() {
        // Same pid + same element_id (same pointer) takes the element-id fast path
        // and returns the cached field without minting a new one.
        let mut tracker = CaretFieldTracker::new();
        let id = AxElementIdentity::new(
            "ax:0x111",
            Some(42),
            Some("First Text View".into()),
            Some("AXTextArea".into()),
            None,
        );
        let first = tracker.field_for_event(42, &id);
        let again = tracker.field_for_event(42, &id);
        assert_eq!(again, first);
    }

    #[test]
    fn caret_field_tracker_mints_new_field_when_identity_diverges() {
        // Different pointer AND different semantic identity → a genuinely new
        // field (not the cached one).
        let mut tracker = CaretFieldTracker::new();
        let first_id = AxElementIdentity::new(
            "ax:0x111",
            Some(42),
            Some("First Text View".into()),
            Some("AXTextArea".into()),
            None,
        );
        let other_id = AxElementIdentity::new(
            "ax:0x999",
            Some(42),
            Some("Search Field".into()),
            Some("AXTextField".into()),
            None,
        );
        let first = tracker.field_for_event(42, &first_id);
        let other = tracker.field_for_event(42, &other_id);
        assert_ne!(other, first);
    }

    #[test]
    fn caret_field_tracker_uses_fallback_pid_when_identity_has_none() {
        // An identity with no owner pid falls back to the event's pid.
        let mut tracker = CaretFieldTracker::new();
        let id = AxElementIdentity::new(
            "ax:0x111",
            None,
            Some("First Text View".into()),
            Some("AXTextArea".into()),
            None,
        );
        let field = tracker.field_for_event(7, &id);
        assert_eq!(field.pid, Some(7));
    }

    #[test]
    fn caret_field_tracker_reuses_semantic_identity_when_pointer_changes() {
        let mut tracker = CaretFieldTracker::new();
        let first = AxElementIdentity::new(
            "ax:0x111",
            Some(42),
            Some("First Text View".into()),
            Some("AXTextArea".into()),
            None,
        );
        let second = AxElementIdentity::new(
            "ax:0x222",
            Some(42),
            Some("First Text View".into()),
            Some("AXTextArea".into()),
            None,
        );

        let first_field = tracker.field_for_event(42, &first);
        let second_field = tracker.field_for_event(42, &second);

        assert_eq!(second_field, first_field);
    }

    #[test]
    fn stable_field_key_is_none_without_owner_pid() {
        let identity = AxElementIdentity::new(
            "ax:0x123",
            None,
            Some("editor".into()),
            Some("AXTextArea".into()),
            Some("AXSecureTextField".into()),
        );

        assert_eq!(identity.stable_field_key(), None);
    }

    #[test]
    fn stable_field_key_is_none_when_no_semantic_attributes_present() {
        let identity = AxElementIdentity::new("ax:0x123", Some(42), None, None, None);

        assert_eq!(identity.stable_field_key(), None);
    }

    #[test]
    fn stable_field_key_builds_key_when_any_attribute_present() {
        let identity = AxElementIdentity::new(
            "ax:0x123",
            Some(42),
            Some("editor".into()),
            Some("AXTextArea".into()),
            Some("AXSecureTextField".into()),
        );

        assert_eq!(
            identity.stable_field_key(),
            Some("ax:pid=42|id=editor|role=AXTextArea|subrole=AXSecureTextField".into())
        );

        let role_only =
            AxElementIdentity::new("ax:0x123", Some(42), None, Some("AXTextArea".into()), None);

        assert_eq!(
            role_only.stable_field_key(),
            Some("ax:pid=42|role=AXTextArea".into())
        );
    }

    #[test]
    fn field_matches_identity_accepts_exact_field_element_id() {
        let identity = AxElementIdentity::new(
            "ax:0x123",
            Some(42),
            Some("editor".into()),
            Some("AXTextArea".into()),
            None,
        );
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: identity.field_element_id(),
            generation: 1,
        };

        assert!(field_matches_identity(&field, &identity));
    }

    #[test]
    fn field_matches_identity_accepts_when_all_stable_key_parts_present() {
        let identity = AxElementIdentity::new(
            "ax:0x999",
            Some(42),
            Some("editor".into()),
            Some("AXTextArea".into()),
            None,
        );
        // The stable key is "ax:pid=42|id=editor|role=AXTextArea". After
        // stripping the "ax:" prefix and splitting on '|', every part
        // (pid=42, id=editor, role=AXTextArea) is contained in element_id even
        // though the pointer differs from the original field_element_id.
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: "ax:ptr=ax:0xDIFFERENT|pid=42|id=editor|role=AXTextArea".into(),
            generation: 1,
        };

        assert!(field_matches_identity(&field, &identity));
    }

    #[test]
    fn field_matches_identity_rejects_when_a_stable_key_part_is_missing() {
        let identity = AxElementIdentity::new(
            "ax:0x999",
            Some(42),
            Some("editor".into()),
            Some("AXTextArea".into()),
            None,
        );
        // Missing the "role=AXTextArea" part, so not all parts are contained.
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: "ax:ptr=ax:0xDIFFERENT|pid=42|id=editor".into(),
            generation: 1,
        };

        assert!(!field_matches_identity(&field, &identity));
    }

    #[test]
    fn field_matches_identity_rejects_substring_prefix_parts() {
        let identity = AxElementIdentity::new(
            "ax:0x999",
            Some(42),
            Some("name".into()),
            Some("AXTextArea".into()),
            None,
        );
        // A DIFFERENT field in the same app whose identifier merely starts
        // with the stable key's identifier ("name" vs "name2") must NOT pass
        // the wrong-field guard — segment equality, not substring containment.
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: "ax:ptr=ax:0xDIFFERENT|pid=42|id=name2|role=AXTextArea".into(),
            generation: 1,
        };

        assert!(!field_matches_identity(&field, &identity));
    }

    #[test]
    fn field_matches_identity_rejects_pid_prefix_overlap() {
        let identity = AxElementIdentity::new(
            "ax:0x999",
            Some(4),
            Some("editor".into()),
            Some("AXTextArea".into()),
            None,
        );
        // "pid=4" is a substring of "pid=42" — a cross-process identity must
        // never match another pid's field.
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: "ax:ptr=ax:0xDIFFERENT|pid=42|id=editor|role=AXTextArea".into(),
            generation: 1,
        };

        assert!(!field_matches_identity(&field, &identity));
    }

    #[test]
    fn field_matches_identity_rejects_segment_injection_via_escaped_pipe_identifier() {
        let identity =
            AxElementIdentity::new("ax:0x999", Some(42), None, Some("AXTextArea".into()), None);
        // A field whose AXIdentifier contains a literal '|' (Chromium derives
        // identifiers from web-content ids) escapes it as "\|" — a naive
        // split('|') would fragment that component into "id=x\" plus a forged
        // "role=AXTextArea" segment, matching an identity whose role the
        // field does NOT have (its real role is AXButton).
        let other = AxElementIdentity::new(
            "ax:0xAAA",
            Some(42),
            Some("x|role=AXTextArea".into()),
            Some("AXButton".into()),
            None,
        );
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: other.field_element_id(),
            generation: 1,
        };

        assert!(!field_matches_identity(&field, &identity));
    }

    #[test]
    fn split_identity_segments_handles_backslash_escape_boundaries() {
        // A single '\' escapes the following '|' so it does NOT terminate a
        // segment (the injection-defense path exercised via field_matches_*).
        assert_eq!(split_identity_segments(r"id=a\|b"), vec![r"id=a\|b"]);

        // A doubled '\\' is a LITERAL backslash: the escape is consumed by the
        // second '\', so a following '|' DOES terminate. If the escape state
        // leaked past the pair, the whole string would collapse to one segment
        // and this assertion would fail. This '\\' branch has no other coverage.
        assert_eq!(split_identity_segments(r"a\\|b"), vec![r"a\\", "b"]);

        // Backslash + escaped pipe + a real terminator: the escaped '|' stays
        // in the first segment, the unescaped '|' splits.
        assert_eq!(split_identity_segments(r"x\|y|z"), vec![r"x\|y", "z"]);

        // Plain multi-segment and single-segment baselines (no escapes).
        assert_eq!(
            split_identity_segments("ptr=1|pid=42|role=AXTextArea"),
            vec!["ptr=1", "pid=42", "role=AXTextArea"]
        );
        assert_eq!(split_identity_segments("solo"), vec!["solo"]);

        // A trailing lone backslash must not panic or drop the final segment
        // (byte-index arithmetic on the last char).
        assert_eq!(split_identity_segments(r"a\"), vec![r"a\"]);
    }

    #[test]
    fn field_matches_identity_accepts_pipe_bearing_identifier_via_stable_key() {
        // The positive direction of the same escaping: an identity whose
        // identifier legitimately contains '|' must still match its own
        // re-resolved handle (different pointer forces the stable-key path).
        let identity = AxElementIdentity::new(
            "ax:0x999",
            Some(42),
            Some("editor|main".into()),
            Some("AXTextArea".into()),
            None,
        );
        let re_resolved = AxElementIdentity::new(
            "ax:0xDIFFERENT",
            Some(42),
            Some("editor|main".into()),
            Some("AXTextArea".into()),
            None,
        );
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: re_resolved.field_element_id(),
            generation: 1,
        };

        assert!(field_matches_identity(&field, &identity));
    }

    #[test]
    fn field_matches_identity_rejects_when_identity_has_no_stable_key() {
        // Pointer-only identity has no owner_pid, so stable_field_key() is None
        // and only an exact field_element_id match could succeed.
        let identity = AxElementIdentity::pointer_only("ax:0x123");
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: "ax:ptr=ax:0xOTHER".into(),
            generation: 1,
        };

        assert!(!field_matches_identity(&field, &identity));
    }

    #[test]
    fn toolkit_for_identity_maps_missing_role_to_generic_unknown() {
        let identity =
            AxElementIdentity::new("ax:0x123", Some(42), Some("editor".into()), None, None);

        assert_eq!(
            toolkit_for_identity(&identity),
            Toolkit::Unknown("macOS Accessibility".into())
        );
    }

    #[test]
    fn display_scale_pairs_projects_bounds_and_scale() {
        let scales = vec![
            DisplayScale {
                bounds: CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(1920.0, 1080.0)),
                scale: 1.0,
            },
            DisplayScale {
                bounds: CGRect::new(&CGPoint::new(1920.0, -200.0), &CGSize::new(1440.0, 900.0)),
                scale: 2.0,
            },
        ];

        let pairs = display_scale_pairs(&scales);

        assert_eq!(
            pairs,
            vec![
                (
                    ScreenRect {
                        x: 0.0,
                        y: 0.0,
                        w: 1920.0,
                        h: 1080.0
                    },
                    1.0
                ),
                (
                    ScreenRect {
                        x: 1920.0,
                        y: -200.0,
                        w: 1440.0,
                        h: 900.0
                    },
                    2.0
                ),
            ]
        );
    }

    #[test]
    fn display_scale_pairs_empty_is_empty() {
        assert!(display_scale_pairs(&[]).is_empty());
    }

    #[test]
    fn rect_center_inside_bounds_drives_screen_capture_display_choice() {
        let bounds = CGRect::new(&CGPoint::new(100.0, -50.0), &CGSize::new(800.0, 600.0));

        assert!(rect_center_is_inside_bounds(
            ScreenRect {
                x: 120.0,
                y: 10.0,
                w: 10.0,
                h: 20.0
            },
            bounds
        ));
        assert!(!rect_center_is_inside_bounds(
            ScreenRect {
                x: 20.0,
                y: 10.0,
                w: 10.0,
                h: 20.0
            },
            bounds
        ));
    }

    #[test]
    fn rect_center_on_the_bound_edges_is_inclusive() {
        // Pins the >=/<= inclusivity at both extremes (existing test only does
        // clearly-inside / clearly-outside). bounds covers x∈[100,900], y∈[-50,550].
        let bounds = CGRect::new(&CGPoint::new(100.0, -50.0), &CGSize::new(800.0, 600.0));
        // A zero-size rect places its center exactly at (x, y).
        let center_at = |x: f64, y: f64| ScreenRect {
            x,
            y,
            w: 0.0,
            h: 0.0,
        };
        // Center exactly on the origin corner is inside (>= / >=).
        assert!(rect_center_is_inside_bounds(
            center_at(100.0, -50.0),
            bounds
        ));
        // Center exactly on the far corner (origin + size) is inside (<= / <=).
        assert!(rect_center_is_inside_bounds(
            center_at(900.0, 550.0),
            bounds
        ));
        // A hair past either edge is outside — proving the comparison is at the
        // edge, not merely lenient.
        assert!(!rect_center_is_inside_bounds(
            center_at(99.999, -50.0),
            bounds
        ));
        assert!(!rect_center_is_inside_bounds(
            center_at(900.001, 550.0),
            bounds
        ));
        assert!(!rect_center_is_inside_bounds(
            center_at(100.0, -50.001),
            bounds
        ));
        assert!(!rect_center_is_inside_bounds(
            center_at(900.0, 550.001),
            bounds
        ));
    }

    #[test]
    fn resolve_retained_observer_event_without_element_is_pointer_only() {
        // No retained AX element → pointer-only identity, no rect, no FFI deref.
        let event = resolve_retained_observer_event(
            42,
            ObserverNotification::FocusChanged,
            None,
            "ax:null",
        );

        assert_eq!(
            event,
            ObserverEvent {
                pid: 42,
                notification: ObserverNotification::FocusChanged,
                identity: AxElementIdentity::pointer_only("ax:null"),
                rect: None,
            }
        );
    }

    #[test]
    fn capabilities_blocks_secure_text_field_handles() {
        let adapter = test_adapter(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: AxElementIdentity::new(
                "ax:0x123",
                Some(42),
                Some("password".into()),
                Some("AXTextField".into()),
                Some(kAXSecureTextFieldSubrole.into()),
            )
            .field_element_id(),
            generation: 1,
        };

        let caps = adapter.capabilities(&field).expect("secure capabilities");

        assert!(caps.secure);
        assert_eq!(caps.security_state, SecurityState::SecureField);
        assert!(!caps.readable_text);
        assert!(!caps.writable);
        assert_eq!(caps.insert_strategy, InsertStrategy::None);
        assert_eq!(caps.overlay_at_caret, OverlayPlacement::None);
    }

    #[test]
    fn capabilities_blocks_when_global_secure_input_is_enabled() {
        let adapter = test_adapter_with_secure_input(true);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        let caps = adapter
            .capabilities(&field)
            .expect("secure input capabilities");

        assert!(caps.secure);
        assert_eq!(caps.security_state, SecurityState::SecureInputEnabled);
        assert!(!caps.readable_text);
        assert!(!caps.writable);
        assert_eq!(caps.insert_strategy, InsertStrategy::None);
        assert_eq!(caps.accept_intercept, KeyInterceptMode::None);
    }

    #[test]
    fn capabilities_prefers_global_secure_input_over_secure_field() {
        let adapter = test_adapter_with_secure_input(true);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: AxElementIdentity::new(
                "ax:0x123",
                Some(42),
                Some("password".into()),
                Some("AXTextField".into()),
                Some(kAXSecureTextFieldSubrole.into()),
            )
            .field_element_id(),
            generation: 1,
        };

        let caps = adapter
            .capabilities(&field)
            .expect("secure input capabilities");

        assert_eq!(caps.security_state, SecurityState::SecureInputEnabled);
    }

    #[test]
    fn capabilities_requires_pid_for_non_secure_fields() {
        let adapter = test_adapter(None, Arc::new(Mutex::new(Vec::new())), None);
        let field = FieldHandle {
            app: "unknown".into(),
            pid: None,
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.capabilities(&field),
            Err(PlatformError::CannotComplete {
                reason: "no pid available for capabilities".into(),
            })
        );
    }

    #[test]
    fn editable_capabilities_advertise_inline_axset_when_rect_is_available() {
        let identity = AxElementIdentity::new(
            "ax:0x123",
            Some(42),
            Some("First Text View".into()),
            Some("AXTextArea".into()),
            None,
        );

        let caps = editable_capabilities(&identity, true, true, true, true);

        assert!(caps.readable_text);
        assert!(caps.readable_caret);
        assert!(caps.writable);
        assert!(!caps.secure);
        assert_eq!(caps.security_state, SecurityState::Normal);
        assert_eq!(caps.toolkit, Toolkit::AppKit);
        assert!(caps.multiline);
        assert_eq!(caps.insert_strategy, InsertStrategy::AxSet);
        assert_eq!(caps.accept_intercept, KeyInterceptMode::CarbonHotkey);
        assert_eq!(caps.overlay_at_caret, OverlayPlacement::NativePanel);
        assert!(caps.coords_global_screen);
        assert_eq!(platform::ux_mode(&caps), platform::UxMode::Inline);
    }

    #[test]
    fn editable_capabilities_mark_ax_text_field_single_line() {
        let identity = AxElementIdentity::new(
            "ax:0x123",
            Some(42),
            Some("Field".into()),
            Some("AXTextField".into()),
            None,
        );

        let caps = editable_capabilities(&identity, true, true, true, true);

        assert_eq!(caps.toolkit, Toolkit::AppKit);
        assert!(!caps.multiline);
        assert_eq!(platform::ux_mode(&caps), platform::UxMode::Inline);
    }

    #[test]
    fn editable_capabilities_fall_back_to_popup_without_rect() {
        let identity = AxElementIdentity::new(
            "ax:0x123",
            Some(42),
            Some("Field".into()),
            Some("AXTextField".into()),
            None,
        );

        let caps = editable_capabilities(&identity, true, true, false, true);

        assert!(caps.readable_text);
        assert!(!caps.readable_caret);
        assert!(caps.writable);
        assert!(!caps.multiline);
        assert_eq!(caps.insert_strategy, InsertStrategy::AxSet);
        assert_eq!(caps.overlay_at_caret, OverlayPlacement::None);
        assert_eq!(platform::ux_mode(&caps), platform::UxMode::Popup);
    }

    #[test]
    fn editable_capabilities_disable_caret_when_selected_range_is_not_settable() {
        let identity = AxElementIdentity::new(
            "ax:0x123",
            Some(42),
            Some("Field".into()),
            Some("AXTextArea".into()),
            None,
        );

        let caps = editable_capabilities(&identity, true, false, true, true);

        assert!(caps.readable_text);
        assert!(!caps.readable_caret);
        assert!(caps.writable);
        assert_eq!(caps.overlay_at_caret, OverlayPlacement::None);
        assert_eq!(platform::ux_mode(&caps), platform::UxMode::Popup);
    }

    #[test]
    fn editable_capabilities_plan_synthetic_when_ax_value_is_not_settable() {
        let identity = AxElementIdentity::new(
            "ax:0x123",
            Some(42),
            Some("Keyboard Injectable".into()),
            Some("AXTextArea".into()),
            None,
        );

        let caps = editable_capabilities(&identity, false, true, true, true);

        assert!(caps.readable_text);
        assert!(caps.writable);
        assert_eq!(caps.insert_strategy, InsertStrategy::SyntheticKeys);
        assert_eq!(platform::ux_mode(&caps), platform::UxMode::Inline);
    }

    #[test]
    fn editable_capabilities_plan_clipboard_when_only_caret_rect_is_available() {
        let identity = AxElementIdentity::new(
            "ax:0x123",
            Some(42),
            Some("Clipboard Injectable".into()),
            Some("AXTextArea".into()),
            None,
        );

        let caps = editable_capabilities(&identity, false, false, true, true);

        assert!(caps.readable_text);
        assert!(!caps.readable_caret);
        assert!(caps.writable);
        assert_eq!(caps.insert_strategy, InsertStrategy::Clipboard);
        assert_eq!(platform::ux_mode(&caps), platform::UxMode::Popup);
    }

    #[test]
    fn editable_capabilities_are_unsupported_when_no_insert_strategy_is_available() {
        let identity = AxElementIdentity::new(
            "ax:0x123",
            Some(42),
            Some("Read Only".into()),
            Some("AXTextArea".into()),
            None,
        );

        let caps = editable_capabilities(&identity, false, false, false, false);

        assert!(caps.readable_text);
        assert!(!caps.writable);
        assert_eq!(caps.insert_strategy, InsertStrategy::None);
        assert_eq!(platform::ux_mode(&caps), platform::UxMode::Unsupported);
    }

    #[test]
    fn editable_capabilities_preserve_unknown_role_in_toolkit() {
        let identity = AxElementIdentity::new(
            "ax:0x123",
            Some(42),
            Some("Custom".into()),
            Some("AXCustomEditor".into()),
            None,
        );

        let caps = editable_capabilities(&identity, true, true, true, true);

        assert_eq!(
            caps.toolkit,
            Toolkit::Unknown("macOS Accessibility AXCustomEditor".into())
        );
        assert!(!caps.multiline);
    }

    #[test]
    fn read_context_blocks_when_global_secure_input_is_enabled() {
        let adapter = test_adapter_with_secure_input(true);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.read_context(&field),
            Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            })
        );
    }

    #[test]
    fn read_context_blocks_secure_text_field_handles() {
        let adapter = test_adapter_with_secure_input(false);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: AxElementIdentity::new(
                "ax:0x123",
                Some(42),
                Some("password".into()),
                Some("AXTextField".into()),
                Some(kAXSecureTextFieldSubrole.into()),
            )
            .field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.read_context(&field),
            Err(PlatformError::SecureInput {
                state: SecurityState::SecureField,
            })
        );
    }

    #[test]
    fn caret_rect_blocks_when_global_secure_input_is_enabled() {
        let adapter = test_adapter_with_secure_input(true);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.caret_rect(&field),
            Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            })
        );
    }

    #[test]
    fn caret_rect_blocks_secure_text_field_handles() {
        let adapter = test_adapter_with_secure_input(false);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: AxElementIdentity::new(
                "ax:0x123",
                Some(42),
                Some("password".into()),
                Some("AXTextField".into()),
                Some(kAXSecureTextFieldSubrole.into()),
            )
            .field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.caret_rect(&field),
            Err(PlatformError::SecureInput {
                state: SecurityState::SecureField,
            })
        );
    }

    #[test]
    fn popup_anchor_blocks_when_global_secure_input_is_enabled() {
        let adapter = test_adapter_with_secure_input(true);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.popup_anchor(&field),
            Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            })
        );
    }

    #[test]
    fn popup_anchor_blocks_secure_text_field_handles() {
        let adapter = test_adapter_with_secure_input(false);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: AxElementIdentity::new(
                "ax:0x123",
                Some(42),
                Some("password".into()),
                Some("AXTextField".into()),
                Some(kAXSecureTextFieldSubrole.into()),
            )
            .field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.popup_anchor(&field),
            Err(PlatformError::SecureInput {
                state: SecurityState::SecureField,
            })
        );
    }

    #[test]
    fn caret_diagnostics_blocks_when_global_secure_input_is_enabled() {
        let adapter = test_adapter_with_secure_input(true);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.caret_diagnostics(&field),
            Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            })
        );
    }

    #[test]
    fn caret_diagnostics_blocks_secure_text_field_handles() {
        let adapter = test_adapter_with_secure_input(false);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: AxElementIdentity::new(
                "ax:0x123",
                Some(42),
                Some("password".into()),
                Some("AXTextField".into()),
                Some(kAXSecureTextFieldSubrole.into()),
            )
            .field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.caret_diagnostics(&field),
            Err(PlatformError::SecureInput {
                state: SecurityState::SecureField,
            })
        );
    }

    #[test]
    fn insert_blocks_when_global_secure_input_is_enabled() {
        let adapter = test_adapter_with_secure_input(true);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.insert(&field, "x", InsertStrategy::AxSet),
            Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            })
        );
    }

    #[test]
    fn insert_blocks_secure_text_field_handles() {
        let adapter = test_adapter_with_secure_input(false);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: AxElementIdentity::new(
                "ax:0x123",
                Some(42),
                Some("password".into()),
                Some("AXTextField".into()),
                Some(kAXSecureTextFieldSubrole.into()),
            )
            .field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.insert(&field, "x", InsertStrategy::AxSet),
            Err(PlatformError::SecureInput {
                state: SecurityState::SecureField,
            })
        );
    }

    #[test]
    fn insert_clipboard_posts_text_to_target_pid() {
        let posted = Arc::new(Mutex::new(Vec::new()));
        let posted_in_hook = Arc::clone(&posted);
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        config.pasteboard_poster = Arc::new(move |pid, text| {
            posted_in_hook.lock().unwrap().push((pid, text.to_string()));
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.insert(&field, "x", InsertStrategy::Clipboard),
            Ok(Inserted {
                bytes: 1,
                chars: 1,
                strategy: InsertStrategy::Clipboard,
            })
        );
        assert_eq!(*posted.lock().unwrap(), vec![(42, "x".into())]);
    }

    #[test]
    fn insert_synthetic_keys_posts_text_when_frontmost_pid_matches_field() {
        let posted = Arc::new(Mutex::new(Vec::new()));
        let posted_in_hook = Arc::clone(&posted);
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        config.synthetic_key_poster = Arc::new(move |pid, text| {
            posted_in_hook.lock().unwrap().push((pid, text.to_string()));
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.insert(&field, "hé", InsertStrategy::SyntheticKeys),
            Ok(Inserted {
                bytes: "hé".len(),
                chars: 2,
                strategy: InsertStrategy::SyntheticKeys,
            })
        );
        assert_eq!(*posted.lock().unwrap(), vec![(42, "hé".into())]);
    }

    #[test]
    fn insert_global_strategy_rejects_when_frontmost_pid_moved_to_another_app() {
        let posted = Arc::new(Mutex::new(Vec::new()));
        let posted_in_hook = Arc::clone(&posted);
        let mut config = TestAdapterConfig::new(Some(99), Arc::new(Mutex::new(Vec::new())), None);
        config.synthetic_key_poster = Arc::new(move |pid, text| {
            posted_in_hook.lock().unwrap().push((pid, text.to_string()));
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.insert(&field, "x", InsertStrategy::SyntheticKeys),
            Err(PlatformError::StaleField)
        );
        assert!(posted.lock().unwrap().is_empty());
    }

    #[test]
    fn insert_synthetic_keys_errors_when_no_app_is_frontmost() {
        // No frontmost app at all (the desktop has focus): a global synthetic
        // insert cannot target a window, so it must fail honestly rather than
        // post keys into the void. The field still carries a pid (so the pid
        // resolution succeeds), but `ensure_global_insert_target` sees a `None`
        // frontmost and refuses with CannotComplete.
        let posted = Arc::new(Mutex::new(Vec::new()));
        let posted_in_hook = Arc::clone(&posted);
        let mut config = TestAdapterConfig::new(None, Arc::new(Mutex::new(Vec::new())), None);
        config.synthetic_key_poster = Arc::new(move |pid, text| {
            posted_in_hook.lock().unwrap().push((pid, text.to_string()));
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.insert(&field, "x", InsertStrategy::SyntheticKeys),
            Err(PlatformError::CannotComplete {
                reason: "no frontmost application pid for global insert".into(),
            })
        );
        assert!(posted.lock().unwrap().is_empty());
    }

    #[test]
    fn insert_synthetic_keys_rechecks_secure_input_before_posting() {
        // TOCTOU (review finding): secure input is OFF at the entry guard but
        // turns ON before the synthetic post (a password prompt focuses
        // mid-insert). The re-check must refuse the post so no synthetic keys
        // reach the now-secure field.
        use std::sync::atomic::AtomicUsize;
        let posted = Arc::new(Mutex::new(Vec::new()));
        let posted_in_hook = Arc::clone(&posted);
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_in_hook = Arc::clone(&calls);
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        // false on the first call (entry guard), true on every later re-check.
        config.secure_input_enabled =
            Arc::new(move || calls_in_hook.fetch_add(1, Ordering::Relaxed) > 0);
        config.synthetic_key_poster = Arc::new(move |pid, text| {
            posted_in_hook.lock().unwrap().push((pid, text.to_string()));
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.insert(&field, "x", InsertStrategy::SyntheticKeys),
            Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            })
        );
        assert!(
            posted.lock().unwrap().is_empty(),
            "no synthetic post into a field that became secure mid-insert"
        );
    }

    #[test]
    fn insert_clipboard_rechecks_secure_input_before_posting() {
        // TOCTOU on the Clipboard strategy (sibling of the SyntheticKeys recheck
        // test): secure input is OFF at the entry guard but ON before the paste.
        // The recheck must refuse so no Cmd+V lands in a now-secure field.
        use std::sync::atomic::AtomicUsize;
        let posted = Arc::new(Mutex::new(Vec::new()));
        let posted_in_hook = Arc::clone(&posted);
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_in_hook = Arc::clone(&calls);
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        config.secure_input_enabled =
            Arc::new(move || calls_in_hook.fetch_add(1, Ordering::Relaxed) > 0);
        config.pasteboard_poster = Arc::new(move |pid, text| {
            posted_in_hook.lock().unwrap().push((pid, text.to_string()));
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.insert(&field, "x", InsertStrategy::Clipboard),
            Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            })
        );
        assert!(
            posted.lock().unwrap().is_empty(),
            "no clipboard paste into a field that became secure mid-insert"
        );
    }

    #[test]
    fn finish_axset_insert_silent_fallback_refuses_synthetic_post_when_secure_input_on() {
        // TOCTOU on the AxSet silent-fallback path: the entry guard passed, the
        // AX write was silently ignored, and secure input turned on before the
        // synthetic fallback post. The recheck inside finish_axset_insert must
        // refuse — the third recheck site, sibling to the SyntheticKeys/Clipboard
        // ones. Both other finish_axset_insert tests use the default (secure=false)
        // config, so this branch was otherwise unexercised.
        let touched = Arc::new(Mutex::new(Vec::new()));
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        config.secure_input_enabled = Arc::new(|| true);
        let t1 = Arc::clone(&touched);
        config.synthetic_key_poster = Arc::new(move |_, _| {
            t1.lock().unwrap().push("text");
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);

        assert_eq!(
            adapter.finish_axset_insert(42, AxSetApply::SilentlyIgnored, "x", 0),
            Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            }),
            "synthetic fallback must not post into a field that became secure"
        );
        assert!(touched.lock().unwrap().is_empty());
    }

    #[test]
    fn insert_replacing_with_zero_replace_left_is_pure_append_like_insert() {
        // Contract: insert_replacing(.., replace_left=0, ..) == insert (pure
        // append, NO deletion). Pins that the backspace poster is never invoked
        // on the zero path, so a regression that always deleted would fail.
        let posted = Arc::new(Mutex::new(Vec::new()));
        let backspaced = Arc::new(Mutex::new(Vec::new()));
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        let p = Arc::clone(&posted);
        config.synthetic_key_poster = Arc::new(move |_, text| {
            p.lock().unwrap().push(text.to_string());
            Ok(())
        });
        let b = Arc::clone(&backspaced);
        config.backspace_poster = Arc::new(move |_, _| {
            b.lock().unwrap().push("bs");
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.insert_replacing(&field, "hi", 0, InsertStrategy::SyntheticKeys),
            Ok(Inserted {
                bytes: 2,
                chars: 2,
                strategy: InsertStrategy::SyntheticKeys,
            })
        );
        assert_eq!(*posted.lock().unwrap(), vec!["hi".to_string()]);
        assert!(
            backspaced.lock().unwrap().is_empty(),
            "replace_left==0 must delete nothing"
        );
    }

    #[test]
    fn insert_replacing_range_refuses_non_axset_without_posting_text() {
        let posted = Arc::new(Mutex::new(Vec::new()));
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        let p = Arc::clone(&posted);
        config.synthetic_key_poster = Arc::new(move |_, text| {
            p.lock().unwrap().push(text.to_string());
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.insert_replacing_range(
                &field,
                "the",
                CorrectionRange { start: 0, end: 3 },
                InsertStrategy::SyntheticKeys,
            ),
            Err(PlatformError::UnsupportedField {
                reason: "range replacement requires AxSet".into(),
            })
        );
        assert!(posted.lock().unwrap().is_empty());
    }

    fn keep_handler(log: Arc<Mutex<Vec<i64>>>) -> Arc<AcceptTapHandler> {
        Arc::new(move |event: AcceptTapEvent| {
            log.lock().unwrap().push(event.keycode);
            AcceptTapDecision::Keep
        })
    }

    fn tap_event(keycode: i64) -> AcceptTapEvent {
        AcceptTapEvent {
            event_type: CGEventType::KeyDown,
            keycode,
            source_user_data: 0,
            option_down: false,
            binding: None,
            shortcut: None,
        }
    }

    #[test]
    fn axset_readback_classifies_only_an_unchanged_value_as_silent_failure() {
        fn inserted() -> Inserted {
            Inserted {
                bytes: 4,
                chars: 1,
                strategy: InsertStrategy::AxSet,
            }
        }
        // Readback == original → the write silently did nothing (iTerm2).
        assert_eq!(
            axset_readback_outcome(":smile", ":smile", inserted()),
            AxSetApply::SilentlyIgnored
        );
        // Readback == expected → applied.
        assert_eq!(
            axset_readback_outcome(":smile", "😄", inserted()),
            AxSetApply::Applied(inserted())
        );
        // Readback differs from BOTH (app normalization) → applied — a
        // fallback here would double-insert.
        assert_eq!(
            axset_readback_outcome(":smile", "\u{1f604} ", inserted()),
            AxSetApply::Applied(inserted())
        );
    }

    #[test]
    fn silently_ignored_axset_replacement_refuses_non_atomic_fallback() {
        let touched = Arc::new(Mutex::new(Vec::new()));
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        let t1 = Arc::clone(&touched);
        config.synthetic_key_poster = Arc::new(move |_, _| {
            t1.lock().unwrap().push("text");
            Ok(())
        });
        let t2 = Arc::clone(&touched);
        config.backspace_poster = Arc::new(move |_, _| {
            t2.lock().unwrap().push("backspace");
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);

        assert_eq!(
            adapter.finish_axset_insert(42, AxSetApply::SilentlyIgnored, "😄", 6),
            Err(PlatformError::CannotComplete {
                reason: "AxSet replacement was ignored; non-atomic fallback refused".into(),
            }),
            "replacement fallback cannot safely restore the deleted token"
        );
        assert!(touched.lock().unwrap().is_empty());
    }

    #[test]
    fn applied_axset_touches_no_synthetic_posters() {
        let touched = Arc::new(Mutex::new(Vec::new()));
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        let t1 = Arc::clone(&touched);
        config.synthetic_key_poster = Arc::new(move |_, _| {
            t1.lock().unwrap().push("text");
            Ok(())
        });
        let t2 = Arc::clone(&touched);
        config.backspace_poster = Arc::new(move |_, _| {
            t2.lock().unwrap().push("backspace");
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);

        let inserted = Inserted {
            bytes: 4,
            chars: 1,
            strategy: InsertStrategy::AxSet,
        };
        assert_eq!(
            adapter.finish_axset_insert(42, AxSetApply::Applied(inserted), "😄", 6),
            Ok(Inserted {
                bytes: 4,
                chars: 1,
                strategy: InsertStrategy::AxSet,
            })
        );
        assert!(touched.lock().unwrap().is_empty());
    }

    #[test]
    fn silently_ignored_axset_fails_honestly_when_the_app_is_not_frontmost() {
        let touched = Arc::new(Mutex::new(Vec::new()));
        let mut config = TestAdapterConfig::new(Some(99), Arc::new(Mutex::new(Vec::new())), None);
        let t1 = Arc::clone(&touched);
        config.synthetic_key_poster = Arc::new(move |_, _| {
            t1.lock().unwrap().push("text");
            Ok(())
        });
        let t2 = Arc::clone(&touched);
        config.backspace_poster = Arc::new(move |_, _| {
            t2.lock().unwrap().push("backspace");
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);

        assert_eq!(
            adapter.finish_axset_insert(42, AxSetApply::SilentlyIgnored, "x", 0),
            Err(PlatformError::StaleField),
            "synthetic input must never reach an app the user switched away from"
        );
        assert!(touched.lock().unwrap().is_empty());
    }

    #[test]
    fn a_rebound_keymap_keeps_decision_registration_and_inverse_consistent() {
        // The cycle-13 one-source contract, checked on a NON-default map so a
        // future regression in any of the three call sites' shared source
        // shows up as a divergence here (the swappable ACCEPT_KEYMAP global
        // stays untouched — the swap test owns it; this test works on a
        // local map only).
        let map = AcceptKeymap::from_accept_keys(Some(122), Some(120)).expect("valid rebind");
        for (id, keycode, _mask) in map.carbon_bindings() {
            // registration → inverse agrees
            assert_eq!(map.keycode_for_hotkey_id(id), Some(keycode), "id {id}");
            // registration → decision agrees (every registered key maps to a
            // binding; the armed-gate semantics live elsewhere)
            assert!(map.binding_for(keycode).is_some(), "keycode {keycode}");
        }
        // The rebound word/full keys actually moved.
        assert_eq!(map.binding_for(122), Some(AcceptBinding::Word));
        assert_eq!(map.binding_for(120), Some(AcceptBinding::Full));
        assert_eq!(map.binding_for(48), None, "old Tab unbound");
    }

    #[test]
    fn carbon_slot_serves_the_armed_handler_and_clears_on_matching_disarm() {
        let slot = CarbonHandlerSlot::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        assert!(slot.current().is_none(), "starts disarmed");

        slot.arm(1, keep_handler(Arc::clone(&log)));
        let handler = slot.current().expect("armed");
        let _ = handler(tap_event(48));
        assert_eq!(*log.lock().unwrap(), vec![48]);

        slot.disarm(1);
        assert!(slot.current().is_none(), "matching disarm clears");
    }

    #[test]
    fn carbon_slot_stale_disarm_never_clears_a_newer_arm() {
        // The R2-5 out-of-order guard: resource A armed (id 1), resource B
        // arms (id 2) before A's drop runs — A's disarm must be a no-op.
        let slot = CarbonHandlerSlot::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        slot.arm(1, keep_handler(Arc::clone(&log)));
        slot.arm(2, keep_handler(Arc::clone(&log)));

        slot.disarm(1);
        assert!(
            slot.current().is_some(),
            "a stale disarm must not clear the newer arm"
        );
        slot.disarm(2);
        assert!(slot.current().is_none());
    }

    #[test]
    fn carbon_slot_handler_cloned_out_survives_a_concurrent_disarm() {
        // The race R2-5 fixes: a fire that read the slot just before a disarm
        // must complete safely — the cloned Arc keeps the handler alive.
        let slot = CarbonHandlerSlot::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        slot.arm(7, keep_handler(Arc::clone(&log)));
        let in_flight = slot.current().expect("armed");
        slot.disarm(7);
        let _ = in_flight(tap_event(50));
        assert_eq!(
            *log.lock().unwrap(),
            vec![50],
            "the in-flight handler must still be callable after disarm"
        );
    }

    #[test]
    fn insert_replacing_synthetic_keys_refuses_non_atomic_replacement() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        let log_in_keys = Arc::clone(&log);
        config.synthetic_key_poster = Arc::new(move |pid, text| {
            log_in_keys
                .lock()
                .unwrap()
                .push(format!("text:{pid}:{text}"));
            Ok(())
        });
        let log_in_backspaces = Arc::clone(&log);
        config.backspace_poster = Arc::new(move |pid, count| {
            log_in_backspaces
                .lock()
                .unwrap()
                .push(format!("backspace:{pid}x{count}"));
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.insert_replacing(&field, "the", 3, InsertStrategy::SyntheticKeys),
            Err(PlatformError::CannotComplete {
                reason: "macOS SyntheticKeys replacement is not atomic".into(),
            })
        );
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn insert_replacing_blocks_when_global_secure_input_is_enabled() {
        let adapter = test_adapter_with_secure_input(true);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.insert_replacing(&field, "the", 3, InsertStrategy::SyntheticKeys),
            Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            })
        );
    }

    #[test]
    fn insert_replacing_with_empty_text_is_noop_and_never_invokes_backspace_poster() {
        let backspace_calls = Arc::new(Mutex::new(Vec::new()));
        let calls_in_hook = Arc::clone(&backspace_calls);
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        config.backspace_poster = Arc::new(move |pid, count| {
            calls_in_hook.lock().unwrap().push((pid, count));
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.insert_replacing(&field, "", 5, InsertStrategy::SyntheticKeys),
            Ok(Inserted {
                bytes: 0,
                chars: 0,
                strategy: InsertStrategy::SyntheticKeys,
            })
        );
        assert!(
            backspace_calls.lock().unwrap().is_empty(),
            "the empty-text early return precedes deletion: nothing is deleted when there is nothing to insert"
        );
    }

    #[test]
    fn insert_replacing_clipboard_refuses_non_atomic_replacement() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        let log_in_paste = Arc::clone(&log);
        config.pasteboard_poster = Arc::new(move |pid, text| {
            log_in_paste
                .lock()
                .unwrap()
                .push(format!("paste:{pid}:{text}"));
            Ok(())
        });
        let log_in_backspaces = Arc::clone(&log);
        config.backspace_poster = Arc::new(move |pid, count| {
            log_in_backspaces
                .lock()
                .unwrap()
                .push(format!("backspace:{pid}x{count}"));
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.insert_replacing(&field, "😄", 6, InsertStrategy::Clipboard),
            Err(PlatformError::CannotComplete {
                reason: "macOS Clipboard replacement is not atomic".into(),
            })
        );
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn insert_with_zero_replace_left_never_invokes_the_backspace_poster() {
        let backspace_calls = Arc::new(Mutex::new(Vec::new()));
        let calls_in_hook = Arc::clone(&backspace_calls);
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        config.backspace_poster = Arc::new(move |pid, count| {
            calls_in_hook.lock().unwrap().push((pid, count));
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert!(adapter
            .insert(&field, "x", InsertStrategy::SyntheticKeys)
            .is_ok());
        assert!(adapter
            .insert(&field, "x", InsertStrategy::Clipboard)
            .is_ok());
        assert!(
            backspace_calls.lock().unwrap().is_empty(),
            "plain inserts must stay byte-identical: no backspace synthesis"
        );
    }

    #[test]
    fn insert_replacing_axset_never_invokes_the_backspace_poster() {
        let backspace_calls = Arc::new(Mutex::new(Vec::new()));
        let calls_in_hook = Arc::clone(&backspace_calls);
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        config.backspace_poster = Arc::new(move |pid, count| {
            calls_in_hook.lock().unwrap().push((pid, count));
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        // AxSet range-replaces in-process on the AX worker; the result here is
        // irrelevant (no live AX element) — only the non-invocation matters.
        let _ = adapter.insert_replacing(&field, "the", 3, InsertStrategy::AxSet);
        assert!(
            backspace_calls.lock().unwrap().is_empty(),
            "AxSet deletes via range-replace, never via synthetic backspaces"
        );
    }

    #[test]
    fn insert_replacing_posts_no_backspaces_when_frontmost_pid_moved() {
        let backspace_calls = Arc::new(Mutex::new(Vec::new()));
        let calls_in_hook = Arc::clone(&backspace_calls);
        let mut config = TestAdapterConfig::new(Some(99), Arc::new(Mutex::new(Vec::new())), None);
        config.backspace_poster = Arc::new(move |pid, count| {
            calls_in_hook.lock().unwrap().push((pid, count));
            Ok(())
        });
        let adapter = test_adapter_with_hooks(config);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.insert_replacing(&field, "the", 3, InsertStrategy::SyntheticKeys),
            Err(PlatformError::StaleField)
        );
        assert!(
            backspace_calls.lock().unwrap().is_empty(),
            "backspaces must never reach an app the user already switched away from"
        );
    }

    #[test]
    fn pasteboard_snapshot_restores_multiple_items_and_types() {
        let pasteboard = NSPasteboard::pasteboardWithUniqueName();
        let custom_type = NSString::from_str("com.compme.test.bytes");
        pasteboard.clearContents();

        let first = NSPasteboardItem::new();
        assert!(first.setString_forType(&NSString::from_str("first"), pasteboard_string_type()));
        assert!(first.setData_forType(&NSData::with_bytes(&[1, 2, 3]), &custom_type));
        let second = NSPasteboardItem::new();
        assert!(second.setString_forType(&NSString::from_str("second"), pasteboard_string_type()));
        assert!(write_test_pasteboard_items(
            &pasteboard,
            vec![first, second]
        ));

        let snapshot = snapshot_pasteboard(&pasteboard);
        pasteboard.clearContents();
        assert!(pasteboard
            .setString_forType(&NSString::from_str("replacement"), pasteboard_string_type(),));

        restore_pasteboard(&pasteboard, &snapshot);

        let restored_items = pasteboard.pasteboardItems().expect("restored items");
        assert_eq!(restored_items.len(), 2);
        let restored_first = restored_items.objectAtIndex(0);
        let restored_second = restored_items.objectAtIndex(1);
        assert_eq!(
            restored_first
                .stringForType(pasteboard_string_type())
                .map(|value| value.to_string()),
            Some("first".into())
        );
        assert_eq!(
            restored_first
                .dataForType(&custom_type)
                .map(|data| data.to_vec()),
            Some(vec![1, 2, 3])
        );
        assert_eq!(
            restored_second
                .stringForType(pasteboard_string_type())
                .map(|value| value.to_string()),
            Some("second".into())
        );
    }

    #[test]
    fn snapshot_drops_item_that_advertises_a_type_but_yields_no_data() {
        // A pasteboard item can advertise a type via a lazy data provider yet
        // produce no data when asked (the provider sets nothing). The
        // `(!types.is_empty())` guard in `snapshot_pasteboard_items` keys off
        // the materialized type/data pairs, not the advertised types, so such
        // an item is dropped from the snapshot rather than captured empty.
        let provider_type = NSString::from_str("com.compme.test.empty");
        let provided_count = Arc::new(AtomicUsize::new(0));
        let provider = TestNoopPasteboardProvider::new(Arc::clone(&provided_count));

        let item = NSPasteboardItem::new();
        let provider_ref: &ProtocolObject<dyn NSPasteboardItemDataProvider> =
            ProtocolObject::from_ref(&*provider);
        let types = NSArray::from_slice(&[&*provider_type]);
        assert!(item.setDataProvider_forTypes(provider_ref, &types));
        // The item DOES advertise a type — so the drop is driven by the
        // missing data, not by an absent type.
        assert_eq!(item.types().len(), 1);

        let snapshot = snapshot_pasteboard_items(&NSArray::from_slice(&[&*item]));

        assert!(
            snapshot.is_empty(),
            "an item that advertises a type but yields no data must be dropped"
        );
        assert!(
            provided_count.load(Ordering::SeqCst) >= 1,
            "the data provider must have been asked for its (absent) data"
        );
    }

    #[test]
    fn restore_falls_back_to_clear_when_items_fail_to_write() {
        // `restore_pasteboard` clears, attempts the item restore, and on
        // failure falls through to `restore_pasteboard_string`. With no
        // fallback string the net effect is a cleared pasteboard. We force the
        // item restore to fail deterministically with an empty type name, which
        // makes `populate_pasteboard_item`'s `setData:forType:` reject the item
        // (so `restore_pasteboard_items` returns false) — no flaky FFI needed.
        let pasteboard = NSPasteboard::pasteboardWithUniqueName();
        pasteboard.clearContents();
        assert!(
            pasteboard.setString_forType(&NSString::from_str("stale"), pasteboard_string_type(),)
        );

        // Sanity: the item snapshot really does fail to write on its own.
        let failing_items = vec![PasteboardItemSnapshot {
            types: vec![PasteboardTypeSnapshot {
                type_name: String::new(),
                data: vec![1, 2, 3],
            }],
        }];
        assert!(
            !restore_pasteboard_items(&pasteboard, &failing_items),
            "an empty type name must make the item restore fail"
        );

        let snapshot = PasteboardSnapshot {
            items: failing_items,
            fallback_string: None,
        };
        restore_pasteboard(&pasteboard, &snapshot);

        // The failed restore fell back to clearing: no string survives and the
        // pasteboard holds nothing from the snapshot.
        assert_eq!(
            pasteboard
                .stringForType(pasteboard_string_type())
                .map(|value| value.to_string()),
            None,
            "a failed item restore with no fallback string must leave the pasteboard cleared"
        );
        assert!(
            pasteboard
                .pasteboardItems()
                .map(|items| items.len())
                .unwrap_or(0)
                == 0,
            "no snapshot items should have been written after the restore failure"
        );
    }

    #[test]
    fn pasteboard_snapshot_materializes_provider_items_before_restore() {
        let pasteboard = NSPasteboard::pasteboardWithUniqueName();
        let provider_type = NSString::from_str("com.compme.test.provider");
        let provided_count = Arc::new(AtomicUsize::new(0));
        let provider = TestPasteboardProvider::new("provided", Arc::clone(&provided_count));
        pasteboard.clearContents();

        let item = NSPasteboardItem::new();
        let provider_ref: &ProtocolObject<dyn NSPasteboardItemDataProvider> =
            ProtocolObject::from_ref(&*provider);
        let types = NSArray::from_slice(&[&*provider_type]);
        assert!(item.setDataProvider_forTypes(provider_ref, &types));
        assert_eq!(provided_count.load(Ordering::SeqCst), 0);

        let snapshot = PasteboardSnapshot {
            items: snapshot_pasteboard_items(&NSArray::from_slice(&[&*item])),
            fallback_string: None,
        };
        assert_eq!(provided_count.load(Ordering::SeqCst), 1);

        pasteboard.clearContents();
        restore_pasteboard(&pasteboard, &snapshot);

        let restored_items = pasteboard.pasteboardItems().expect("restored items");
        assert_eq!(restored_items.len(), 1);
        assert_eq!(
            restored_items
                .objectAtIndex(0)
                .stringForType(&provider_type)
                .map(|value| value.to_string()),
            Some("provided".into())
        );
        assert_eq!(provided_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn pasteboard_restore_falls_back_to_string_when_items_are_empty() {
        let pasteboard = NSPasteboard::pasteboardWithUniqueName();
        pasteboard.clearContents();
        assert!(pasteboard
            .setString_forType(&NSString::from_str("replacement"), pasteboard_string_type(),));
        let snapshot = PasteboardSnapshot {
            items: Vec::new(),
            fallback_string: Some("previous".into()),
        };

        restore_pasteboard(&pasteboard, &snapshot);

        assert_eq!(
            pasteboard
                .stringForType(pasteboard_string_type())
                .map(|value| value.to_string()),
            Some("previous".into())
        );
    }

    #[test]
    fn pasteboard_restore_if_unchanged_restores_snapshot() {
        let pasteboard = NSPasteboard::pasteboardWithUniqueName();
        pasteboard.clearContents();
        assert!(pasteboard
            .setString_forType(&NSString::from_str("previous"), pasteboard_string_type(),));
        let snapshot = snapshot_pasteboard(&pasteboard);

        pasteboard.clearContents();
        assert!(pasteboard
            .setString_forType(&NSString::from_str("completion"), pasteboard_string_type(),));
        let completion_change_count = pasteboard.changeCount();

        assert_eq!(
            restore_pasteboard_if_unchanged(&pasteboard, &snapshot, completion_change_count),
            PasteboardRestoreOutcome::Restored
        );
        assert_eq!(
            pasteboard
                .stringForType(pasteboard_string_type())
                .map(|value| value.to_string()),
            Some("previous".into())
        );
    }

    #[test]
    fn pasteboard_restore_if_unchanged_preserves_external_clipboard_change() {
        let pasteboard = NSPasteboard::pasteboardWithUniqueName();
        pasteboard.clearContents();
        assert!(pasteboard
            .setString_forType(&NSString::from_str("previous"), pasteboard_string_type(),));
        let snapshot = snapshot_pasteboard(&pasteboard);

        pasteboard.clearContents();
        assert!(pasteboard
            .setString_forType(&NSString::from_str("completion"), pasteboard_string_type(),));
        let completion_change_count = pasteboard.changeCount();
        pasteboard.clearContents();
        assert!(pasteboard
            .setString_forType(&NSString::from_str("external"), pasteboard_string_type(),));

        assert_eq!(
            restore_pasteboard_if_unchanged(&pasteboard, &snapshot, completion_change_count),
            PasteboardRestoreOutcome::SkippedChanged
        );
        assert_eq!(
            pasteboard
                .stringForType(pasteboard_string_type())
                .map(|value| value.to_string()),
            Some("external".into())
        );
    }

    #[test]
    fn chromium_caret_rects_are_normalized_to_textedit_semantics() {
        // Live screenshots (2026-06-10, g5.html textarea + google.com search):
        // the emoji ghost rendered exactly ONE LINE BELOW the typed text in
        // Chrome. Chrome's caret rect IS the caret line ([y, y+h]); the
        // TextEdit-calibrated formula assumes the line is one rect BELOW
        // ([y+h, y+2h], cycle-44 finding). Shifting Chrome rects up by h makes
        // the downstream math correct unchanged.
        let chrome_rect = ScreenRect {
            x: 911.0,
            y: 353.0,
            w: 0.0,
            h: 21.0,
        };
        let normalized = normalize_caret_rect(chrome_rect, Some("com.google.Chrome"), false);
        assert_eq!(normalized.y, 332.0, "shift up by one line height");
        assert_eq!(
            (normalized.x, normalized.w, normalized.h),
            (911.0, 0.0, 21.0)
        );

        // Chromium-family prefix matches too.
        assert_eq!(
            normalize_caret_rect(chrome_rect, Some("org.chromium.Chromium"), false).y,
            332.0
        );
        // iTerm2 exhibits the same semantics (live screenshots 2026-06-10:
        // ghost one line low in iTerm2, twice — user run + scripted self-test).
        assert_eq!(
            normalize_caret_rect(chrome_rect, Some("com.googlecode.iterm2"), false).y,
            332.0
        );
    }

    #[test]
    fn safari_web_field_caret_rects_are_normalized_to_textedit_semantics() {
        // Live finding 2026-06-14: the emoji ghost rendered exactly ONE LINE
        // BELOW the text in Safari's google.com / duckduckgo.com search boxes.
        // Safari's WebKit web-content caret rect IS the caret line (like
        // Chromium), so it joins the rect-is-line family and shifts up by h.
        let safari_rect = ScreenRect {
            x: 1741.0,
            y: 103.0,
            w: 0.0,
            h: 16.0,
        };
        // Web content (not the omnibox) → shifted onto the line.
        let normalized = normalize_caret_rect(safari_rect, Some("com.apple.Safari"), false);
        assert_eq!(normalized.y, 87.0, "shift up by one line height");
        assert_eq!(
            (normalized.x, normalized.w, normalized.h),
            (1741.0, 0.0, 16.0)
        );
        // Safari's NATIVE address bar (omnibox) is TextEdit-like — NOT shifted
        // (2026-06-14 live finding: the blanket shift put it one line too high).
        assert_eq!(
            normalize_caret_rect(safari_rect, Some("com.apple.Safari"), true).y,
            103.0,
            "the Safari omnibox keeps its raw caret y"
        );
        // The carve-out is Safari-specific: a Chrome omnibox still shifts.
        assert_eq!(
            normalize_caret_rect(safari_rect, Some("com.google.Chrome"), true).y,
            87.0,
            "non-Safari omnibox keeps the rect-is-line shift"
        );
    }

    #[test]
    fn is_browser_omnibox_detects_the_address_search_field() {
        // The carve-out hinges on this AXIdentifier (live: Safari's address bar
        // reports id=WEB_BROWSER_ADDRESS_AND_SEARCH_FIELD); a web-content field
        // (AXTextArea etc.) or an empty id is NOT the omnibox.
        assert!(is_browser_omnibox("WEB_BROWSER_ADDRESS_AND_SEARCH_FIELD"));
        assert!(!is_browser_omnibox("AXTextArea"));
        assert!(!is_browser_omnibox(""));
    }

    #[test]
    fn caret_normalization_leaves_other_apps_and_degenerate_rects_alone() {
        let rect = ScreenRect {
            x: 120.0,
            y: 240.0,
            w: 1.0,
            h: 14.0,
        };
        // TextEdit semantics are the calibrated default — untouched.
        assert_eq!(
            normalize_caret_rect(rect, Some("com.apple.TextEdit"), false).y,
            240.0
        );
        // Unknown app — untouched (no-false-positive discipline: only
        // evidence-backed bundles shift).
        assert_eq!(normalize_caret_rect(rect, None, false).y, 240.0);
        // A Chrome ELEMENT-BOUNDS rect (the degenerate case) must NOT shift —
        // the overlay's bounds fallback owns that path, and shifting y by a
        // 1225px "height" would garble it.
        let bounds = ScreenRect {
            x: 835.0,
            y: 168.0,
            w: 1799.0,
            h: 1225.0,
        };
        assert_eq!(
            normalize_caret_rect(bounds, Some("com.google.Chrome"), false).y,
            168.0
        );
    }

    #[test]
    fn overlay_frame_treats_an_element_bounds_rect_as_degenerate_and_stays_onscreen() {
        // Live Chrome finding (2026-06-10 log): an AXTextField answered the
        // caret query with its ELEMENT BOUNDS — rect=(835, 168, 1799, 1225) —
        // and the line-midpoint flip placed the ghost at y = -429.5, fully
        // offscreen. A real caret rect is a sliver (w ≤ a few px, h = one
        // line); anything wider/taller is bounds, so fall back to a default
        // line box hugging the element's inside top-left:
        // y = 1600 - 168 - 18 = 1414.
        let frame = overlay_frame_for_text(
            ScreenRect {
                x: 835.0,
                y: 168.0,
                w: 1799.0,
                h: 1225.0,
            },
            "😄",
            1600.0,
        );

        assert_eq!(frame.x, 835.0);
        assert_eq!(frame.h, 18.0);
        assert_eq!(frame.y, 1414.0);
        assert!(
            frame.y >= 0.0 && frame.y + frame.h <= 1600.0,
            "the ghost must land onscreen"
        );
    }

    #[test]
    fn overlay_frame_hugs_the_caret_line_height_and_centers_on_it() {
        // Live step-6 calibration (screenshot + debug log, cycle 44): the AX
        // caret rect's BOTTOM edge (rect.y + rect.h) is the caret line's TOP —
        // treating rect.y as the line top rendered the ghost exactly one line
        // above the typed text, on every line of the TextEdit gate doc. The
        // caret line therefore spans [y+h, y+2h] in AX (Y-down) coords. Box
        // hugs the line (h = 14 + 4 = 18) centered on the line's midpoint:
        // line center = 240 + 1.5*14 = 261 → Cocoa center = 1000 - 261 = 739
        // → box bottom = 739 - 18/2 = 730.
        let frame = overlay_frame_for_text(
            ScreenRect {
                x: 120.0,
                y: 240.0,
                w: 1.0,
                h: 14.0,
            },
            "short",
            1000.0,
        );

        assert_eq!(frame.h, 18.0);
        assert_eq!(frame.y, 730.0);
    }

    #[test]
    fn correction_frames_place_banner_above_and_underline_below_word_rect() {
        let rect = ScreenRect {
            x: 120.0,
            y: 240.0,
            w: 48.0,
            h: 14.0,
        };

        let banner = correction_banner_frame_for_word(rect, "the", 1000.0);
        let underline = correction_underline_frame_for_word(rect, 1000.0);

        assert_eq!(banner.x, 120.0);
        assert_eq!(banner.y, 764.0);
        assert_eq!(banner.w, 96.0);
        assert_eq!(banner.h, 22.0);
        assert_eq!(
            underline,
            OverlayFrame {
                x: 120.0,
                y: 744.0,
                w: 48.0,
                h: 2.0,
            }
        );
        assert!(
            underline.y < 1000.0 - rect.y - rect.h,
            "underline sits below the word rect in Cocoa coordinates"
        );
        assert!(
            banner.y > 1000.0 - rect.y,
            "banner sits above the word rect in Cocoa coordinates"
        );
    }

    #[test]
    fn correction_frames_preserve_secondary_display_negative_y() {
        let rect = ScreenRect {
            x: 120.0,
            y: 1100.0,
            w: 48.0,
            h: 14.0,
        };

        let banner = correction_banner_frame_for_word(rect, "correction", 1000.0);
        let underline = correction_underline_frame_for_word(rect, 1000.0);

        assert!(banner.y < 0.0);
        assert!(underline.y < 0.0);
    }

    #[test]
    fn overlay_frame_treats_narrow_but_tall_rect_as_degenerate() {
        // The degenerate guard ORs both dimensions
        // (rect.w > CARET_MAX_W || rect.h > CARET_MAX_H), so a narrow-but-tall
        // rect (w=2 ≤ 4, but h=200 > 160) is element bounds, not a caret. It
        // takes the degenerate branch: default 18pt box hugging the rect's top
        // (y = primary_height - rect.y - h = 1600 - 168 - 18 = 1414), staying
        // onscreen instead of flipping off-screen.
        assert_eq!(
            overlay_frame_for_text(
                ScreenRect {
                    x: 835.0,
                    y: 168.0,
                    w: 2.0,
                    h: 200.0,
                },
                "x",
                1600.0
            ),
            OverlayFrame {
                x: 835.0,
                y: 1414.0,
                w: 240.0,
                h: 18.0,
            }
        );
    }

    #[test]
    fn normalize_caret_rect_does_not_shift_narrow_but_tall_rect() {
        // The plausible-caret check ANDs both dimensions
        // (rect.w <= CARET_MAX_W && rect.h <= CARET_MAX_H), so a narrow-but-tall
        // rect (w=2 ≤ 4, but h=200 > 160) is NOT a plausible caret. Even for a
        // rect-is-line bundle (Chrome), it is returned unshifted — y stays 300.
        assert_eq!(
            normalize_caret_rect(
                ScreenRect {
                    x: 100.0,
                    y: 300.0,
                    w: 2.0,
                    h: 200.0,
                },
                Some("com.google.Chrome"),
                false
            )
            .y,
            300.0
        );
    }

    #[test]
    fn overlay_font_size_tracks_the_box_height() {
        // A 14pt line → 18pt box → 12pt font (TextEdit's default body size),
        // so the ghost glyphs match the typed text scale instead of the fixed
        // 13pt label default.
        assert_eq!(overlay_font_size(18.0), 12.0);
        // Tiny boxes never go below a legible floor…
        assert_eq!(overlay_font_size(10.0), 9.0);
        // …and tall boxes (clamped 48) cap so the glyphs stay sane.
        assert_eq!(overlay_font_size(48.0), 28.0);
    }

    #[test]
    fn overlay_frame_uses_caret_origin_and_minimum_size() {
        // Primary screen 1000pt tall: a caret rect at AX y=240 (its bottom edge
        // 254 = the caret line's top), line height 14 → box hugs the line
        // (14 + 4 = 18), centered on it: 1000 - 240 - 1.5*14 - 18/2 = 730.
        let frame = overlay_frame_for_text(
            ScreenRect {
                x: 120.0,
                y: 240.0,
                w: 1.0,
                h: 14.0,
            },
            "short",
            1000.0,
        );

        assert_eq!(
            frame,
            OverlayFrame {
                x: 120.0,
                y: 730.0,
                w: 240.0,
                h: 18.0,
            }
        );
    }

    #[test]
    fn overlay_frame_flips_against_primary_height_for_secondary_displays() {
        // A caret on a taller secondary display (AX y beyond the primary height)
        // produces a negative Cocoa y, which is correct in Cocoa global space.
        let frame = overlay_frame_for_text(
            ScreenRect {
                x: 50.0,
                y: 1200.0,
                w: 1.0,
                h: 14.0,
            },
            "short",
            1000.0,
        );

        assert_eq!(frame.y, 1000.0 - 1200.0 - 21.0 - 9.0);
    }

    #[test]
    fn overlay_frame_caps_very_long_text_width() {
        let frame = overlay_frame_for_text(
            ScreenRect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 80.0,
            },
            &"x".repeat(200),
            1000.0,
        );

        assert_eq!(frame.w, 720.0);
        assert_eq!(frame.h, 48.0);
    }

    #[test]
    fn overlay_label_frame_keeps_fixed_inset() {
        // 2pt insets all around: the box hugs the caret line and starts at the
        // caret x, so the label must hug the box for the ghost text to sit on
        // the line AND directly after the typed text (live finding: the old
        // 8pt horizontal inset read as a visible gap after the typed word).
        let label = overlay_label_frame(OverlayFrame {
            x: 120.0,
            y: 240.0,
            w: 240.0,
            h: 18.0,
        });

        assert_eq!(
            label,
            OverlayFrame {
                x: 2.0,
                y: 2.0,
                w: 236.0,
                h: 14.0,
            }
        );
    }

    fn accept_tap_event(
        event_type: CGEventType,
        keycode: i64,
        source_user_data: i64,
    ) -> AcceptTapEvent {
        AcceptTapEvent {
            event_type,
            keycode,
            source_user_data,
            option_down: false,
            binding: None,
            shortcut: None,
        }
    }

    fn accept_tap_event_with_option(event_type: CGEventType, keycode: i64) -> AcceptTapEvent {
        AcceptTapEvent {
            event_type,
            keycode,
            source_user_data: 0,
            option_down: true,
            binding: None,
            shortcut: None,
        }
    }

    #[test]
    fn option_tab_passes_through_as_literal_tab() {
        // Option+Tab is Cotypist's per-app Tab bypass: a real Tab reaches the
        // field (no Word accept, no swallow), even while armed.
        let opt_tab = accept_tap_event_with_option(CGEventType::KeyDown, KEYCODE_TAB);

        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Consumer,
                opt_tab,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::Keep
        );
    }

    #[test]
    fn option_word_bypass_applies_to_resolved_binding() {
        // The Option+Tab bypass must trigger off the resolved/fired Word role,
        // not only the keycode-fallback path: when the producer hands us the
        // Word binding directly (id-resolved) and Option is held, the key still
        // passes through literally (Keep) rather than accepting the word.
        let opt_word = AcceptTapEvent {
            event_type: CGEventType::KeyDown,
            keycode: KEYCODE_TAB,
            source_user_data: 0,
            option_down: true,
            binding: Some(AcceptBinding::Word),
            shortcut: None,
        };

        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Consumer,
                opt_word,
                Some(AcceptAction::Word)
            ),
            AcceptTapDecision::Keep
        );
    }

    #[test]
    fn escape_while_armed_dismisses_and_suppresses() {
        let esc = accept_tap_event(CGEventType::KeyDown, KEYCODE_ESCAPE, 0);

        // Armed consumer tap: Esc is consumed and routed as a dismiss+suppress.
        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Consumer,
                esc,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::DropDismiss
        );
        // Unarmed (no suggestion visible): Esc passes through to the app.
        assert_eq!(
            accept_tap_decision(&AcceptKeymap::default(), AcceptTapKind::Consumer, esc, None),
            AcceptTapDecision::Keep
        );
        // Observer (listen-only) tap never consumes Esc.
        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Observer,
                esc,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::Keep
        );
    }

    #[test]
    fn accept_tap_decision_tab_drops_to_word_only_on_armed_consumer_tap() {
        let tab = accept_tap_event(CGEventType::KeyDown, KEYCODE_TAB, 0);

        // Observer (listen-only) tap never consumes.
        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Observer,
                tab,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::Keep
        );
        // Consumer tap only consumes while armed.
        assert_eq!(
            accept_tap_decision(&AcceptKeymap::default(), AcceptTapKind::Consumer, tab, None),
            AcceptTapDecision::Keep
        );
        // Tab always accepts the next word once armed, regardless of armed value.
        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Consumer,
                tab,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::Drop(AcceptAction::Word)
        );
        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Consumer,
                tab,
                Some(AcceptAction::Word)
            ),
            AcceptTapDecision::Drop(AcceptAction::Word)
        );
    }

    #[test]
    fn tab_accepts_word_and_grave_accepts_full() {
        // Cotypist default binding: Tab = accept next word (partial),
        // grave/backtick (key above Tab) = accept the whole completion.
        // The armed value is only a gate — the keycode picks the action.
        let tab = accept_tap_event(CGEventType::KeyDown, KEYCODE_TAB, 0);
        let grave = accept_tap_event(CGEventType::KeyDown, KEYCODE_GRAVE, 0);

        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Consumer,
                tab,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::Drop(AcceptAction::Word),
            "Tab must accept the next word regardless of armed value"
        );
        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Consumer,
                grave,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::Drop(AcceptAction::Full),
            "grave must accept the full completion"
        );
        // Grave is only consumed while armed.
        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Consumer,
                grave,
                None
            ),
            AcceptTapDecision::Keep
        );
        // Grave on the observer (listen-only) tap is never consumed.
        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Observer,
                grave,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::Keep
        );
    }

    #[test]
    fn down_arrow_while_armed_cycles_candidates() {
        let down = accept_tap_event(CGEventType::KeyDown, KEYCODE_DOWN, 0);
        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Consumer,
                down,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::DropCycle
        );
        // Unarmed (no suggestion): Down passes through for normal navigation.
        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Consumer,
                down,
                None
            ),
            AcceptTapDecision::Keep
        );
        // Observer tap never consumes.
        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Observer,
                down,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::Keep
        );
    }

    #[test]
    fn carbon_hotkey_ids_map_to_accept_keys() {
        assert_eq!(carbon_hotkey_keycode(CARBON_HOTKEY_TAB), Some(KEYCODE_TAB));
        assert_eq!(
            carbon_hotkey_keycode(CARBON_HOTKEY_GRAVE),
            Some(KEYCODE_GRAVE)
        );
        assert_eq!(
            carbon_hotkey_keycode(CARBON_HOTKEY_ESCAPE),
            Some(KEYCODE_ESCAPE)
        );
        assert_eq!(
            carbon_hotkey_keycode(CARBON_HOTKEY_DOWN),
            Some(KEYCODE_DOWN)
        );
        assert_eq!(carbon_hotkey_keycode(99), None);
    }

    #[test]
    fn carbon_hotkey_installer_registers_every_accept_binding() {
        // Default keymap passed explicitly — the global belongs to the
        // swap-owning test; arm_bindings(false) is what the installer arms.
        let bindings = AcceptKeymap::default().arm_bindings(false);

        assert_eq!(
            bindings,
            [
                (CARBON_HOTKEY_TAB, KEYCODE_TAB, 0),
                (CARBON_HOTKEY_GRAVE, KEYCODE_GRAVE, 0),
                (CARBON_HOTKEY_ESCAPE, KEYCODE_ESCAPE, 0),
                (CARBON_HOTKEY_DOWN, KEYCODE_DOWN, 0),
            ]
        );
        for (id, keycode, _mask) in bindings {
            assert_eq!(carbon_hotkey_keycode(id), Some(keycode));
        }
    }

    #[test]
    fn default_keymap_matches_the_cotypist_bindings() {
        let map = AcceptKeymap::default();
        assert_eq!(map.binding_for(KEYCODE_TAB), Some(AcceptBinding::Word));
        assert_eq!(map.binding_for(KEYCODE_GRAVE), Some(AcceptBinding::Full));
        assert_eq!(
            map.binding_for(KEYCODE_ESCAPE),
            Some(AcceptBinding::Dismiss)
        );
        assert_eq!(map.binding_for(KEYCODE_DOWN), Some(AcceptBinding::Cycle));
        assert_eq!(map.binding_for(999), None);
        // Default Carbon registration content (explicit, not a self-comparison).
        assert_eq!(
            map.carbon_bindings(),
            [
                (CARBON_HOTKEY_TAB, KEYCODE_TAB, 0),
                (CARBON_HOTKEY_GRAVE, KEYCODE_GRAVE, 0),
                (CARBON_HOTKEY_ESCAPE, KEYCODE_ESCAPE, 0),
                (CARBON_HOTKEY_DOWN, KEYCODE_DOWN, 0),
            ]
        );
        // The id→keycode inverse used by the Carbon handler agrees with it.
        assert_eq!(
            map.keycode_for_hotkey_id(CARBON_HOTKEY_TAB),
            Some(KEYCODE_TAB)
        );
        assert_eq!(
            map.keycode_for_hotkey_id(CARBON_HOTKEY_DOWN),
            Some(KEYCODE_DOWN)
        );
        assert_eq!(map.keycode_for_hotkey_id(999), None);
    }

    #[test]
    fn rebinding_accept_keys_changes_the_mapping() {
        // Rebind word→F1 (122) and full→F2 (120); Esc/Down stay fixed.
        let map = AcceptKeymap::from_accept_keys(Some(122), Some(120)).expect("valid rebind");
        assert_eq!(map.binding_for(122), Some(AcceptBinding::Word));
        assert_eq!(map.binding_for(120), Some(AcceptBinding::Full));
        assert_eq!(map.binding_for(KEYCODE_TAB), None); // old word key no longer bound
        assert_eq!(
            map.binding_for(KEYCODE_ESCAPE),
            Some(AcceptBinding::Dismiss)
        );
        // Carbon registration reflects the rebind.
        assert_eq!(
            map.carbon_bindings(),
            [
                (CARBON_HOTKEY_TAB, 122, 0),
                (CARBON_HOTKEY_GRAVE, 120, 0),
                (CARBON_HOTKEY_ESCAPE, KEYCODE_ESCAPE, 0),
                (CARBON_HOTKEY_DOWN, KEYCODE_DOWN, 0),
            ]
        );
    }

    #[test]
    fn modifier_combo_collision_compares_keycode_and_modifiers() {
        // Slice 1 headline: a binding is identified by (keycode, modifier mask),
        // not keycode alone. So the SAME keycode under DIFFERENT modifiers is two
        // distinct accept keys — Tab for word, Shift+Tab for full must coexist.
        let map = AcceptKeymap::from_accept_keys_with_mods(
            Some(KEYCODE_TAB),
            Some(KEYCODE_TAB),
            0,
            CARBON_SHIFT_KEY,
        )
        .expect("Tab and Shift+Tab are different bindings — no collision");
        assert_eq!(
            map.carbon_bindings(),
            [
                (CARBON_HOTKEY_TAB, KEYCODE_TAB, 0),
                (CARBON_HOTKEY_GRAVE, KEYCODE_TAB, CARBON_SHIFT_KEY),
                (CARBON_HOTKEY_ESCAPE, KEYCODE_ESCAPE, 0),
                (CARBON_HOTKEY_DOWN, KEYCODE_DOWN, 0),
            ]
        );

        // Same keycode AND same mask collides exactly as before.
        assert_eq!(
            AcceptKeymap::from_accept_keys_with_mods(Some(KEYCODE_TAB), Some(KEYCODE_TAB), 0, 0),
            Err(KeymapError::Collision(KEYCODE_TAB))
        );

        // Same keycode AND the same NON-ZERO mask still collides — guards the
        // (keycode, mask) tuple identity in the non-zero branch (a regression to
        // a keycode-only or mask-dropping compare would register a duplicate).
        assert_eq!(
            AcceptKeymap::from_accept_keys_with_mods(
                Some(KEYCODE_TAB),
                Some(KEYCODE_TAB),
                CARBON_SHIFT_KEY,
                CARBON_SHIFT_KEY
            ),
            Err(KeymapError::Collision(KEYCODE_TAB))
        );

        // A modified word key does NOT collide with a fixed bare key of the same
        // keycode: Shift+Esc as word is distinct from bare Esc (dismiss).
        assert!(
            AcceptKeymap::from_accept_keys_with_mods(
                Some(KEYCODE_ESCAPE),
                None,
                CARBON_SHIFT_KEY,
                0
            )
            .is_ok(),
            "Shift+Esc (word) is distinct from bare Esc (dismiss)"
        );

        // The plain constructor is exactly the zero-modifier case.
        assert_eq!(
            AcceptKeymap::from_accept_keys(Some(122), Some(120)),
            AcceptKeymap::from_accept_keys_with_mods(Some(122), Some(120), 0, 0),
        );
    }

    #[test]
    fn shortcut_bindings_parse_from_config_and_detect_internal_collisions() {
        // The three optional global shortcuts (A3 Shortcuts pane) each parse via
        // parse_accept_key; None/empty/malformed leaves that binding unset.
        let b = ShortcutBindings::from_config(Some("shift+96"), None, Some("garbage"), None);
        assert_eq!(b.force_activate, Some((96, CARBON_SHIFT_KEY)));
        assert_eq!(b.toggle_app, None);
        assert_eq!(b.toggle_global, None); // malformed → unset (fail-soft)
        assert_eq!(b.grammar_check, None);
        assert!(!b.has_internal_collision());

        // All-unset is the default (no global hotkey registered).
        assert_eq!(
            ShortcutBindings::default(),
            ShortcutBindings::from_config(None, None, None, None)
        );

        // Two bindings on the SAME (keycode, mask) chord collide — the caller
        // must reject the set before registering (one chord can't fire two).
        let clash = ShortcutBindings::from_config(Some("ctrl+50"), Some("ctrl+50"), None, None);
        assert!(clash.has_internal_collision());

        // Same keycode, DIFFERENT modifiers is NOT a collision (distinct chords).
        let distinct = ShortcutBindings::from_config(
            Some("50"),
            Some("shift+50"),
            Some("ctrl+50"),
            Some("option+50"),
        );
        assert!(!distinct.has_internal_collision());
    }

    #[test]
    fn grammar_accept_key_maps_to_correction_action_only_in_correction_mode() {
        let keymap = AcceptKeymap::from_accept_keys_with_mods_and_grammar(
            None,
            None,
            Some(96),
            0,
            0,
            CARBON_CONTROL_KEY,
        )
        .unwrap();

        assert_eq!(keymap.binding_for(96), Some(AcceptBinding::GrammarAccept));
        assert!(keymap.carbon_bindings().contains(&(
            CARBON_HOTKEY_GRAMMAR_ACCEPT,
            96,
            CARBON_CONTROL_KEY
        )));
        assert_eq!(
            binding_for_hotkey_id(CARBON_HOTKEY_GRAMMAR_ACCEPT),
            Some(AcceptBinding::GrammarAccept)
        );
        let grammar_press = AcceptTapEvent {
            event_type: CGEventType::KeyDown,
            keycode: 96,
            source_user_data: 0,
            option_down: false,
            binding: Some(AcceptBinding::GrammarAccept),
            shortcut: None,
        };

        // Ghost mode preserves the existing Word/Full accept keys and lets the
        // grammar-accept key reach the app.
        assert_eq!(
            accept_tap_decision(
                &keymap,
                AcceptTapKind::Consumer,
                grammar_press,
                Some(AcceptAction::Full),
            ),
            AcceptTapDecision::Keep
        );

        // Correction mode consumes only the dedicated grammar-accept binding.
        assert_eq!(
            accept_tap_decision(
                &keymap,
                AcceptTapKind::Consumer,
                grammar_press,
                Some(AcceptAction::Correction),
            ),
            AcceptTapDecision::Drop(AcceptAction::Correction)
        );
    }

    #[test]
    fn correction_arm_passes_word_full_dismiss_and_cycle_keys_through() {
        let keymap =
            AcceptKeymap::from_accept_keys_with_mods_and_grammar(None, None, Some(96), 0, 0, 0)
                .unwrap();
        for (keycode, binding) in [
            (KEYCODE_TAB, Some(AcceptBinding::Word)),
            (KEYCODE_GRAVE, Some(AcceptBinding::Full)),
            (KEYCODE_ESCAPE, Some(AcceptBinding::Dismiss)),
            (KEYCODE_DOWN, Some(AcceptBinding::Cycle)),
        ] {
            assert_eq!(
                accept_tap_decision(
                    &keymap,
                    AcceptTapKind::Consumer,
                    AcceptTapEvent {
                        event_type: CGEventType::KeyDown,
                        keycode,
                        source_user_data: 0,
                        option_down: false,
                        binding,
                        shortcut: None,
                    },
                    Some(AcceptAction::Correction),
                ),
                AcceptTapDecision::Keep,
                "correction arm must pass through {binding:?}"
            );
        }
    }

    #[test]
    fn grammar_accept_key_collides_on_same_chord_only() {
        assert!(AcceptKeymap::from_accept_keys_with_mods_and_grammar(
            None,
            Some(96),
            Some(96),
            0,
            CARBON_CONTROL_KEY,
            CARBON_CONTROL_KEY,
        )
        .is_err());
        assert!(AcceptKeymap::from_accept_keys_with_mods_and_grammar(
            None,
            Some(96),
            Some(96),
            0,
            CARBON_SHIFT_KEY,
            CARBON_CONTROL_KEY,
        )
        .is_ok());
    }

    #[test]
    fn shortcut_plan_drops_chords_colliding_with_accept_keys() {
        // Finding F: accept keys (ids 1-4) and shortcuts (5/6/7) now register on
        // separate lifecycles, so a shortcut bound to an accept chord would hit
        // eventHotKeyExistsErr. The cross-check drops the colliding shortcut(s)
        // instead of aborting the whole install.
        let accept_chords = [
            (KEYCODE_TAB, 0u32),    // word
            (KEYCODE_GRAVE, 0u32),  // full
            (KEYCODE_ESCAPE, 0u32), // dismiss
            (KEYCODE_DOWN, 0u32),   // cycle
        ];
        // ForceActivate collides with Tab(48); ToggleApp on a free chord survives.
        let plan = vec![
            (CARBON_HOTKEY_FORCE_ACTIVATE, KEYCODE_TAB, 0),
            (CARBON_HOTKEY_TOGGLE_APP, 96, CARBON_SHIFT_KEY),
        ];
        let kept = shortcut_plan_minus_accept_collisions(plan, &accept_chords);
        assert_eq!(kept, vec![(CARBON_HOTKEY_TOGGLE_APP, 96, CARBON_SHIFT_KEY)]);

        // Same keycode but a DIFFERENT modifier is a distinct chord — not dropped.
        let plan = vec![(CARBON_HOTKEY_TOGGLE_GLOBAL, KEYCODE_TAB, CARBON_CONTROL_KEY)];
        let kept = shortcut_plan_minus_accept_collisions(plan, &accept_chords);
        assert_eq!(
            kept,
            vec![(CARBON_HOTKEY_TOGGLE_GLOBAL, KEYCODE_TAB, CARBON_CONTROL_KEY)]
        );
    }

    #[test]
    fn shortcut_plan_collision_filter_handles_the_boundary_sets() {
        let accept_chords = [
            (KEYCODE_TAB, 0u32),
            (KEYCODE_GRAVE, 0u32),
            (KEYCODE_ESCAPE, 0u32),
            (KEYCODE_DOWN, 0u32),
        ];

        // Empty plan stays empty regardless of the accept chords (the install
        // loop then registers nothing — no shortcuts were bound).
        assert!(shortcut_plan_minus_accept_collisions(Vec::new(), &accept_chords).is_empty());

        // Every planned chord collides with an accept key → the whole plan is
        // dropped to empty (each shortcut would have hit eventHotKeyExistsErr).
        let all_collide = vec![
            (CARBON_HOTKEY_FORCE_ACTIVATE, KEYCODE_TAB, 0),
            (CARBON_HOTKEY_TOGGLE_APP, KEYCODE_GRAVE, 0),
            (CARBON_HOTKEY_TOGGLE_GLOBAL, KEYCODE_DOWN, 0),
        ];
        assert!(shortcut_plan_minus_accept_collisions(all_collide, &accept_chords).is_empty());

        // No chord collides → the plan survives verbatim, in its original order
        // (a filter that reordered or dropped a free chord would change this).
        let none_collide = vec![
            (CARBON_HOTKEY_FORCE_ACTIVATE, 96, 0),
            (CARBON_HOTKEY_TOGGLE_APP, 50, CARBON_SHIFT_KEY),
            (CARBON_HOTKEY_TOGGLE_GLOBAL, KEYCODE_TAB, CARBON_CONTROL_KEY),
        ];
        assert_eq!(
            shortcut_plan_minus_accept_collisions(none_collide.clone(), &accept_chords),
            none_collide
        );

        // An empty accept-chord set can never collide → identity on any plan.
        assert_eq!(
            shortcut_plan_minus_accept_collisions(none_collide.clone(), &[]),
            none_collide
        );
    }

    #[test]
    fn shortcut_action_for_hotkey_id_maps_each_always_on_slot() {
        assert_eq!(
            shortcut_action_for_hotkey_id(CARBON_HOTKEY_FORCE_ACTIVATE),
            Some(ShortcutAction::ForceActivate)
        );
        assert_eq!(
            shortcut_action_for_hotkey_id(CARBON_HOTKEY_TOGGLE_APP),
            Some(ShortcutAction::ToggleApp)
        );
        assert_eq!(
            shortcut_action_for_hotkey_id(CARBON_HOTKEY_TOGGLE_GLOBAL),
            Some(ShortcutAction::ToggleGlobal)
        );
        assert_eq!(
            shortcut_action_for_hotkey_id(CARBON_HOTKEY_GRAMMAR_CHECK),
            Some(ShortcutAction::GrammarCheck)
        );
        assert_eq!(shortcut_action_for_hotkey_id(9999), None);
        // Disjoint from the accept-key ids so one shared Carbon handler routes by
        // id unambiguously: an accept id decodes to a binding, never an action.
        for accept_id in [
            CARBON_HOTKEY_TAB,
            CARBON_HOTKEY_GRAVE,
            CARBON_HOTKEY_ESCAPE,
            CARBON_HOTKEY_DOWN,
        ] {
            assert_eq!(shortcut_action_for_hotkey_id(accept_id), None);
            assert!(binding_for_hotkey_id(accept_id).is_some());
        }
    }

    #[test]
    fn registration_plan_lists_only_bound_shortcuts_under_their_action_ids() {
        let b = ShortcutBindings::from_config(Some("96"), None, Some("shift+96"), Some("ctrl+96"));
        // Only the two bound shortcuts appear, each under its action's hotkey id;
        // the unset toggle_app is skipped.
        assert_eq!(
            b.registration_plan(),
            vec![
                (CARBON_HOTKEY_FORCE_ACTIVATE, 96, 0),
                (CARBON_HOTKEY_TOGGLE_GLOBAL, 96, CARBON_SHIFT_KEY),
                (CARBON_HOTKEY_GRAMMAR_CHECK, 96, CARBON_CONTROL_KEY),
            ]
        );
        // Every planned id round-trips back to an action.
        for (id, _, _) in b.registration_plan() {
            assert!(shortcut_action_for_hotkey_id(id).is_some());
        }
    }

    #[test]
    fn set_shortcut_bindings_from_config_drops_a_colliding_set_whole() {
        // A distinct set is stored verbatim and returned for the caller to log.
        let ok =
            set_shortcut_bindings_from_config(Some("96"), None, Some("shift+96"), Some("ctrl+96"));
        assert_eq!(ok.force_activate, Some((96, 0)));
        assert_eq!(ok.toggle_global, Some((96, CARBON_SHIFT_KEY)));
        assert_eq!(ok.grammar_check, Some((96, CARBON_CONTROL_KEY)));
        assert_eq!(shortcut_bindings(), ok);

        // A set where two shortcuts share one chord is unregisterable, so the
        // whole set is dropped to the default (no partial registration).
        let dropped =
            set_shortcut_bindings_from_config(Some("ctrl+50"), Some("ctrl+50"), None, None);
        assert_eq!(dropped, ShortcutBindings::default());
        assert_eq!(shortcut_bindings(), ShortcutBindings::default());
    }

    #[test]
    fn accept_key_strings_parse_and_format_with_modifier_prefixes() {
        // Bare keycode (back-compat with the pre-modifier config format).
        assert_eq!(parse_accept_key("96"), Some((96, 0)));
        assert_eq!(parse_accept_key("  96 "), Some((96, 0)));
        // Single + multiple modifiers, case-insensitive, with aliases.
        assert_eq!(parse_accept_key("shift+96"), Some((96, CARBON_SHIFT_KEY)));
        assert_eq!(
            parse_accept_key("Ctrl+Shift+96"),
            Some((96, CARBON_SHIFT_KEY | CARBON_CONTROL_KEY))
        );
        assert_eq!(
            parse_accept_key("cmd+opt+0"),
            Some((0, CARBON_CMD_KEY | CARBON_OPTION_KEY))
        );
        assert_eq!(
            parse_accept_key("control+option+command+12"),
            Some((12, CARBON_CONTROL_KEY | CARBON_OPTION_KEY | CARBON_CMD_KEY))
        );
        // Every documented modifier alias maps to its Carbon bit (a dropped
        // alias arm would silently break a documented config form).
        assert_eq!(parse_accept_key("super+18"), Some((18, CARBON_CMD_KEY)));
        assert_eq!(parse_accept_key("meta+18"), Some((18, CARBON_CMD_KEY)));
        assert_eq!(parse_accept_key("win+18"), Some((18, CARBON_CMD_KEY)));
        assert_eq!(parse_accept_key("command+18"), Some((18, CARBON_CMD_KEY)));
        assert_eq!(parse_accept_key("alt+18"), Some((18, CARBON_OPTION_KEY)));
        // Junk → None (the caller falls soft to defaults).
        assert_eq!(parse_accept_key(""), None);
        assert_eq!(parse_accept_key("tab"), None); // non-numeric keycode
        assert_eq!(parse_accept_key("hyper+96"), None); // unknown modifier
        assert_eq!(parse_accept_key("shift+"), None); // missing keycode
        assert_eq!(parse_accept_key("-3"), None); // negative keycode
        assert_eq!(parse_accept_key("shift+ctrl"), None); // no numeric tail
                                                          // The integer keycode must be terminal: any token AFTER it is rejected,
                                                          // whether a modifier word or a second integer.
        assert_eq!(parse_accept_key("96+shift"), None); // modifier after keycode
        assert_eq!(parse_accept_key("96+97"), None); // second keycode after keycode

        // format → parse round-trips the (keycode, mask) pair exactly.
        for (keycode, mask) in [
            (96i64, 0u32),
            (96, CARBON_SHIFT_KEY),
            (
                12,
                CARBON_CONTROL_KEY | CARBON_OPTION_KEY | CARBON_SHIFT_KEY | CARBON_CMD_KEY,
            ),
            (0, CARBON_CMD_KEY),
        ] {
            let s = format_accept_key(keycode, mask);
            assert_eq!(
                parse_accept_key(&s),
                Some((keycode, mask)),
                "round-trip {s}"
            );
        }
        // A bare key formats with no prefix (back-compat output).
        assert_eq!(format_accept_key(96, 0), "96");
        // Each single modifier emits its canonical word (pins the word↔bit
        // pairing in ACCEPT_KEY_MODIFIERS; round-trip alone wouldn't catch a
        // mispairing since parse is order/word-tolerant), and a combo emits in
        // ascending-bit order regardless of how the mask was composed.
        assert_eq!(format_accept_key(96, CARBON_CMD_KEY), "cmd+96");
        assert_eq!(format_accept_key(96, CARBON_SHIFT_KEY), "shift+96");
        assert_eq!(format_accept_key(96, CARBON_OPTION_KEY), "option+96");
        assert_eq!(format_accept_key(96, CARBON_CONTROL_KEY), "control+96");
        assert_eq!(
            format_accept_key(96, CARBON_OPTION_KEY | CARBON_CMD_KEY),
            "cmd+option+96"
        );
    }

    #[test]
    fn ns_event_modifier_flags_map_to_carbon_bits() {
        // Slice 2 recorder: NSEvent reports modifiers in the HIGH bits
        // (device-independent flags) while Carbon's RegisterEventHotKey wants
        // the LOW bits. This is the translator between the two layouts. The NS
        // bit positions are objc2-app-kit's NSEventModifierFlags (Shift 1<<17,
        // Control 1<<18, Option 1<<19, Command 1<<20).
        const NS_SHIFT: u64 = 1 << 17;
        const NS_CONTROL: u64 = 1 << 18;
        const NS_OPTION: u64 = 1 << 19;
        const NS_COMMAND: u64 = 1 << 20;

        assert_eq!(ns_modifier_flags_to_carbon_mask(0), 0);
        assert_eq!(ns_modifier_flags_to_carbon_mask(NS_SHIFT), CARBON_SHIFT_KEY);
        assert_eq!(
            ns_modifier_flags_to_carbon_mask(NS_CONTROL),
            CARBON_CONTROL_KEY
        );
        assert_eq!(
            ns_modifier_flags_to_carbon_mask(NS_OPTION),
            CARBON_OPTION_KEY
        );
        assert_eq!(ns_modifier_flags_to_carbon_mask(NS_COMMAND), CARBON_CMD_KEY);
        // A combo maps every set bit, independent of the others.
        assert_eq!(
            ns_modifier_flags_to_carbon_mask(NS_SHIFT | NS_COMMAND),
            CARBON_SHIFT_KEY | CARBON_CMD_KEY
        );
        assert_eq!(
            ns_modifier_flags_to_carbon_mask(NS_CONTROL | NS_OPTION | NS_SHIFT | NS_COMMAND),
            CARBON_CONTROL_KEY | CARBON_OPTION_KEY | CARBON_SHIFT_KEY | CARBON_CMD_KEY
        );
        // Unrelated NS flags (CapsLock 1<<16, Fn 1<<23, numeric pad 1<<21) are
        // NOT registerable accept modifiers — they must be ignored, not leak
        // stray Carbon bits. CapsLock alongside Shift keeps only the Shift bit.
        const NS_CAPSLOCK: u64 = 1 << 16;
        const NS_NUMPAD: u64 = 1 << 21;
        const NS_FN: u64 = 1 << 23;
        assert_eq!(
            ns_modifier_flags_to_carbon_mask(NS_CAPSLOCK | NS_NUMPAD | NS_FN),
            0
        );
        assert_eq!(
            ns_modifier_flags_to_carbon_mask(NS_CAPSLOCK | NS_SHIFT),
            CARBON_SHIFT_KEY
        );
    }

    #[test]
    fn effective_accept_keys_default_then_follow_runtime_swaps() {
        // ONE test owns the global keymap (parallel tests would race it):
        // unset → defaults; set_accept_keymap → effective follows at runtime
        // (the live-rebind core, recorder tick 5a); restored afterward.
        // (accept_tap_decision takes the keymap as a parameter, so the
        // decision tests no longer read this global during the swap window.)
        assert_eq!(effective_accept_keys(), (48, 50));
        set_accept_keymap(AcceptKeymap::from_accept_keys(Some(35), Some(38)).unwrap());
        assert_eq!(effective_accept_keys(), (35, 38));
        set_accept_keymap(AcceptKeymap::default());
        assert_eq!(effective_accept_keys(), (48, 50));

        // Fail-soft contract: a rejected config (collision with a fixed key)
        // must error WITHOUT touching the live map — validation runs before
        // the swap, so the runtime never registers a colliding keymap.
        assert_eq!(
            set_accept_keymap_from_config(Some(KEYCODE_ESCAPE), None),
            Err(KeymapError::Collision(KEYCODE_ESCAPE))
        );
        assert_eq!(
            effective_accept_keys(),
            (48, 50),
            "global unchanged after a rejected config"
        );

        // Modifier masks flow from config through to the registered bindings
        // (slice 1b): set_accept_keymap_from_config_with_mods lands a non-zero
        // Carbon mask in carbon_bindings (word) while the unset key stays bare;
        // restored to the default afterward so the global is clean for others.
        set_accept_keymap_from_config_with_mods(Some((35, CARBON_SHIFT_KEY)), None, None).unwrap();
        let armed = accept_keymap().carbon_bindings();
        assert_eq!(armed[0], (CARBON_HOTKEY_TAB, 35, CARBON_SHIFT_KEY));
        assert_eq!(armed[1], (CARBON_HOTKEY_GRAVE, KEYCODE_GRAVE, 0));
        // effective_accept_keys_with_mods surfaces the live masks for the label
        // half (slice 1b): word carries its mask, the unset full key is bare.
        assert_eq!(
            effective_accept_keys_with_mods(),
            ((35, CARBON_SHIFT_KEY), (KEYCODE_GRAVE, 0))
        );
        set_accept_keymap(AcceptKeymap::default());
        assert_eq!(effective_accept_keys(), (48, 50));
    }

    #[test]
    fn binding_for_hotkey_id_maps_each_carbon_slot_to_its_role() {
        // The Carbon hotkey id is the authoritative role source: each registered
        // slot maps to one accept binding regardless of the keycode/mask bound to
        // it. This is what lets two roles share a keycode (Tab vs Shift+Tab).
        assert_eq!(
            binding_for_hotkey_id(CARBON_HOTKEY_TAB),
            Some(AcceptBinding::Word)
        );
        assert_eq!(
            binding_for_hotkey_id(CARBON_HOTKEY_GRAVE),
            Some(AcceptBinding::Full)
        );
        assert_eq!(
            binding_for_hotkey_id(CARBON_HOTKEY_ESCAPE),
            Some(AcceptBinding::Dismiss)
        );
        assert_eq!(
            binding_for_hotkey_id(CARBON_HOTKEY_DOWN),
            Some(AcceptBinding::Cycle)
        );
        assert_eq!(binding_for_hotkey_id(9999), None);
    }

    #[test]
    fn accept_tap_decision_uses_resolved_binding_over_keycode_for_masked_roles() {
        // Regression: word=Tab(48,0) and full=Shift+Tab(48,SHIFT) share a keycode
        // (the modifier feature permits this). The fired hotkey's id resolves the
        // ROLE; the decision must honor that binding, not re-derive by keycode —
        // which is keycode-ordered (word first) and would make Shift+Tab wrongly
        // perform a Word accept, leaving Full unreachable.
        let map = AcceptKeymap::from_accept_keys_with_mods(Some(48), Some(48), 0, CARBON_SHIFT_KEY)
            .expect("Tab + Shift+Tab coexist");
        let full_fire = AcceptTapEvent {
            event_type: CGEventType::KeyDown,
            keycode: 48,
            source_user_data: 0,
            option_down: false,
            binding: Some(AcceptBinding::Full),
            shortcut: None,
        };
        assert_eq!(
            accept_tap_decision(
                &map,
                AcceptTapKind::Consumer,
                full_fire,
                Some(AcceptAction::Word)
            ),
            AcceptTapDecision::Drop(AcceptAction::Full),
            "Shift+Tab (Full hotkey id) must accept the FULL completion, not Word"
        );
        let word_fire = AcceptTapEvent {
            binding: Some(AcceptBinding::Word),
            ..full_fire
        };
        assert_eq!(
            accept_tap_decision(
                &map,
                AcceptTapKind::Consumer,
                word_fire,
                Some(AcceptAction::Word)
            ),
            AcceptTapDecision::Drop(AcceptAction::Word)
        );
        // No binding supplied (legacy keycode path) → falls back to the keycode
        // map: unchanged behavior for the common distinct-keycode bindings.
        let no_binding = AcceptTapEvent {
            binding: None,
            ..full_fire
        };
        assert_eq!(
            accept_tap_decision(
                &map,
                AcceptTapKind::Consumer,
                no_binding,
                Some(AcceptAction::Word)
            ),
            AcceptTapDecision::Drop(AcceptAction::Word),
            "fallback resolves keycode 48 to Word (the first match)"
        );
    }

    #[test]
    fn accept_tap_decision_honors_a_rebound_keymap() {
        // The rebind→decision contract through the decision fn itself: the
        // rebound word key drops to Word, and the OLD default Tab is unbound
        // (passes through) — previously only pinned at the binding_for level.
        let map = AcceptKeymap::from_accept_keys(Some(122), None).unwrap();
        let rebound = accept_tap_event(CGEventType::KeyDown, 122, 0);
        let old_tab = accept_tap_event(CGEventType::KeyDown, KEYCODE_TAB, 0);

        assert_eq!(
            accept_tap_decision(
                &map,
                AcceptTapKind::Consumer,
                rebound,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::Drop(AcceptAction::Word)
        );
        assert_eq!(
            accept_tap_decision(
                &map,
                AcceptTapKind::Consumer,
                old_tab,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::Keep
        );
    }

    #[test]
    fn arm_bindings_skip_literal_tab_when_suppressed() {
        // Per-app Tab disable (§16): in apps where the user disabled Tab
        // (terminals etc.), the Word hotkey must not be registered AT ALL —
        // a consumed-but-ignored Tab would be worse than either behavior.
        // Pure: takes the map, no global reads (the keymap-global test owns
        // the static).
        let map = AcceptKeymap::default();
        assert_eq!(map.arm_bindings(false).len(), 4);
        let armed = map.arm_bindings(true);
        assert_eq!(armed.len(), 3);
        assert!(armed
            .iter()
            .all(|&(_, code, mods)| !(code == KEYCODE_TAB && mods == 0)));

        // Suppression targets bare Tab, not every binding on Tab. Modifier+Tab
        // remains a deliberate accept shortcut and is distinct from literal Tab.
        let modified_tab = AcceptKeymap::from_accept_keys_with_mods(
            Some(KEYCODE_TAB),
            Some(KEYCODE_TAB),
            0,
            CARBON_SHIFT_KEY,
        )
        .unwrap();
        let modified_armed = modified_tab.arm_bindings(true);
        assert_eq!(modified_armed.len(), 3);
        assert!(modified_armed
            .iter()
            .any(|&(_, code, mods)| code == KEYCODE_TAB && mods == CARBON_SHIFT_KEY));

        // Suppression targets the bare Tab binding, not the word role:
        // a word key rebound elsewhere keeps all four bindings.
        let rebound = AcceptKeymap::from_accept_keys(Some(35), None).unwrap();
        assert_eq!(rebound.arm_bindings(true).len(), 4);
    }

    #[test]
    fn arm_bindings_for_action_are_mode_specific() {
        let map =
            AcceptKeymap::from_accept_keys_with_mods_and_grammar(None, None, Some(96), 0, 0, 0)
                .unwrap();

        let ghost = map.arm_bindings_for_action(AcceptAction::Full, false);
        assert!(ghost.iter().any(|(id, _, _)| *id == CARBON_HOTKEY_TAB));
        assert!(ghost.iter().any(|(id, _, _)| *id == CARBON_HOTKEY_GRAVE));
        assert!(ghost.iter().any(|(id, _, _)| *id == CARBON_HOTKEY_ESCAPE));
        assert!(ghost.iter().any(|(id, _, _)| *id == CARBON_HOTKEY_DOWN));
        assert!(!ghost
            .iter()
            .any(|(id, _, _)| *id == CARBON_HOTKEY_GRAMMAR_ACCEPT));

        assert_eq!(
            map.arm_bindings_for_action(AcceptAction::Correction, false),
            vec![(CARBON_HOTKEY_GRAMMAR_ACCEPT, 96, 0)]
        );
    }

    #[test]
    fn from_accept_keys_defaults_unset_keys() {
        let map = AcceptKeymap::from_accept_keys(None, None).unwrap();
        assert_eq!(map, AcceptKeymap::default());
        // Setting only the full key keeps the default word key.
        let only_full = AcceptKeymap::from_accept_keys(None, Some(122)).unwrap();
        assert_eq!(only_full.word, KEYCODE_TAB);
        assert_eq!(only_full.full, 122);
        // Setting only the word key keeps the default full key.
        let only_word = AcceptKeymap::from_accept_keys(Some(122), None).unwrap();
        assert_eq!(only_word.word, 122);
        assert_eq!(only_word.full, KEYCODE_GRAVE);
    }

    #[test]
    fn from_accept_keys_rejects_every_colliding_pair() {
        // word == full.
        assert_eq!(
            AcceptKeymap::from_accept_keys(Some(122), Some(122)),
            Err(KeymapError::Collision(122))
        );
        // word collides with the fixed Esc (dismiss) and Down (cycle) bindings.
        assert_eq!(
            AcceptKeymap::from_accept_keys(Some(KEYCODE_ESCAPE), None),
            Err(KeymapError::Collision(KEYCODE_ESCAPE))
        );
        assert_eq!(
            AcceptKeymap::from_accept_keys(Some(KEYCODE_DOWN), None),
            Err(KeymapError::Collision(KEYCODE_DOWN))
        );
        // full collides with the fixed Esc (dismiss) and Down (cycle) bindings.
        assert_eq!(
            AcceptKeymap::from_accept_keys(None, Some(KEYCODE_ESCAPE)),
            Err(KeymapError::Collision(KEYCODE_ESCAPE))
        );
        assert_eq!(
            AcceptKeymap::from_accept_keys(None, Some(KEYCODE_DOWN)),
            Err(KeymapError::Collision(KEYCODE_DOWN))
        );
    }

    #[test]
    fn identity_rebind_is_ok_but_same_key_collides() {
        // Explicitly rebinding to the current defaults is a valid no-op.
        assert_eq!(
            AcceptKeymap::from_accept_keys(Some(KEYCODE_TAB), Some(KEYCODE_GRAVE)),
            Ok(AcceptKeymap::default())
        );
        // Binding both accept keys to the same physical key (even the legacy Tab)
        // collides.
        assert_eq!(
            AcceptKeymap::from_accept_keys(Some(KEYCODE_TAB), Some(KEYCODE_TAB)),
            Err(KeymapError::Collision(KEYCODE_TAB))
        );
    }

    #[test]
    fn from_accept_keys_rejects_negative_keycodes() {
        assert_eq!(
            AcceptKeymap::from_accept_keys(Some(-1), None),
            Err(KeymapError::InvalidKeycode(-1))
        );
        assert_eq!(
            AcceptKeymap::from_accept_keys(None, Some(-99)),
            Err(KeymapError::InvalidKeycode(-99))
        );
        // Zero is a valid macOS keycode (the 'a' key), so it is accepted.
        assert!(AcceptKeymap::from_accept_keys(Some(0), None).is_ok());
    }

    #[test]
    fn accept_tap_decision_ignores_self_generated_tab() {
        let event = accept_tap_event(CGEventType::KeyDown, KEYCODE_TAB, SYNTHETIC_EVENT_TAG);

        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Consumer,
                event,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::Keep
        );
    }

    /// Build a bare `AcceptTapController` for the epoch-guard tests. The
    /// installer/callback are no-op fakes (the guard never invokes them); only
    /// `teardown_generation`, `active`, `consumer_tap`, and `accept_action`
    /// matter to the teardown-race logic under test.
    fn test_accept_controller(
        generation: u64,
        action: Option<AcceptAction>,
        active: bool,
        consumer_armed: bool,
    ) -> AcceptTapController {
        let (callback_tx, _rx) = mpsc::channel::<CallbackMessage>();
        let installer: Arc<AcceptTapInstallerFn> =
            Arc::new(|_kind, _handler| Ok(AcceptTapResource::new("test-tap")));
        let callback: AcceptCallback = Arc::new(|_| {});
        AcceptTapController {
            installer,
            callback_tx,
            callback,
            active: Arc::new(AtomicBool::new(active)),
            consumer_tap: Mutex::new(consumer_armed.then(|| AcceptTapResource::new("test-tap"))),
            accept_action: Arc::new(Mutex::new(action)),
            teardown_generation: AtomicU64::new(generation),
        }
    }

    #[test]
    fn clear_accept_action_only_clears_when_generation_matches() {
        // The epoch guard protects against a stale delayed-teardown clearing an
        // accept action that was re-armed under a newer generation.
        let controller = test_accept_controller(5, Some(AcceptAction::Word), true, false);

        // Stale generation → must NOT clear (a newer arm superseded it).
        controller.clear_accept_action_if_generation(3).unwrap();
        assert_eq!(
            *controller.accept_action.lock().unwrap(),
            Some(AcceptAction::Word)
        );

        // Matching generation → clears.
        controller.clear_accept_action_if_generation(5).unwrap();
        assert_eq!(*controller.accept_action.lock().unwrap(), None);
    }

    #[test]
    fn deactivate_if_generation_respects_epoch_and_active_flag() {
        // Stale generation: nothing torn down.
        let stale = test_accept_controller(5, Some(AcceptAction::Full), true, true);
        stale.deactivate_if_generation(3).unwrap();
        assert!(stale.consumer_tap.lock().unwrap().is_some());
        assert_eq!(
            *stale.accept_action.lock().unwrap(),
            Some(AcceptAction::Full)
        );

        // Matching generation: consumer tap dropped AND accept action cleared.
        let matched = test_accept_controller(5, Some(AcceptAction::Full), true, true);
        matched.deactivate_if_generation(5).unwrap();
        assert!(matched.consumer_tap.lock().unwrap().is_none());
        assert_eq!(*matched.accept_action.lock().unwrap(), None);

        // Inactive controller: early return, no teardown even on a matching gen.
        let inactive = test_accept_controller(5, Some(AcceptAction::Full), false, true);
        inactive.deactivate_if_generation(5).unwrap();
        assert!(inactive.consumer_tap.lock().unwrap().is_some());
    }

    #[test]
    fn hide_suggestion_after_zero_delay_deactivates_synchronously_and_bumps_generation() {
        // A zero delay runs the teardown inline (no spawned thread): it advances
        // the epoch and deactivates at that new generation.
        let controller = Arc::new(test_accept_controller(
            0,
            Some(AcceptAction::Word),
            true,
            true,
        ));
        AcceptTapController::hide_suggestion_after(Arc::clone(&controller), Duration::ZERO)
            .unwrap();

        assert_eq!(controller.teardown_generation.load(Ordering::Acquire), 1);
        assert!(controller.consumer_tap.lock().unwrap().is_none());
        assert_eq!(*controller.accept_action.lock().unwrap(), None);
    }

    #[test]
    fn accept_tap_decision_reenables_disabled_taps() {
        let event = accept_tap_event(CGEventType::TapDisabledByTimeout, KEYCODE_TAB, 0);

        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Consumer,
                event,
                None
            ),
            AcceptTapDecision::ReenableAndKeep
        );
    }

    #[test]
    fn accept_tap_decision_reenables_a_user_input_disabled_tap() {
        // A tap can be disabled by the OS for *user input* as well as timeout
        // (e.g. the run loop fell behind). The decision must re-enable in BOTH
        // cases or the accept tap silently goes dead after the first stall.
        let event = accept_tap_event(CGEventType::TapDisabledByUserInput, KEYCODE_TAB, 0);

        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Consumer,
                event,
                None
            ),
            AcceptTapDecision::ReenableAndKeep
        );
    }

    #[test]
    fn subscribe_accept_installs_observer_and_transient_consumer_tap() {
        let accept_tap_installs = Arc::new(Mutex::new(Vec::new()));
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        config.accept_tap_installs = Arc::clone(&accept_tap_installs);
        let adapter = test_adapter_with_hooks(config);
        let (action_tx, action_rx) = mpsc::channel();

        let subscription = adapter
            .subscribe_accept(Arc::new(move |action| {
                action_tx.send(action).expect("action send");
            }))
            .expect("subscribe accept");
        // Per subscription, two process-lifetime resources install up front:
        // the Observer tap and the always-on Shortcut registration (finding C).
        wait_for_accept_tap_count(&accept_tap_installs, 2);
        assert_eq!(
            accept_tap_installs.lock().unwrap()[0].kind,
            AcceptTapKind::Observer
        );
        assert_eq!(
            accept_tap_installs.lock().unwrap()[1].kind,
            AcceptTapKind::Shortcut
        );

        subscription
            .set_suggestion_visible(true)
            .expect("activate consumer");
        wait_for_accept_tap_count(&accept_tap_installs, 3);
        assert_eq!(
            accept_tap_installs.lock().unwrap()[2].kind,
            AcceptTapKind::Consumer
        );

        subscription
            .set_suggestion_visible(true)
            .expect("activation is idempotent");
        assert_eq!(accept_tap_installs.lock().unwrap().len(), 3);

        let consumer_handler = Arc::clone(&accept_tap_installs.lock().unwrap()[2].handler);
        // While armed: Tab accepts the next word, grave accepts the full completion.
        assert_eq!(
            consumer_handler(accept_tap_event(CGEventType::KeyDown, KEYCODE_TAB, 0)),
            AcceptTapDecision::Drop(AcceptAction::Word)
        );
        assert_eq!(
            action_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("word accept action"),
            TapControl::Accept(AcceptAction::Word)
        );
        assert_eq!(
            consumer_handler(accept_tap_event(CGEventType::KeyDown, KEYCODE_GRAVE, 0)),
            AcceptTapDecision::Drop(AcceptAction::Full)
        );
        assert_eq!(
            action_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("full accept action"),
            TapControl::Accept(AcceptAction::Full)
        );
        subscription.set_accept_action(None).expect("disarm accept");
        assert_eq!(
            consumer_handler(accept_tap_event(CGEventType::KeyDown, KEYCODE_TAB, 0)),
            AcceptTapDecision::Keep
        );

        subscription
            .set_suggestion_visible(false)
            .expect("deactivate consumer");
        subscription
            .set_suggestion_visible(true)
            .expect("reactivate consumer");
        wait_for_accept_tap_count(&accept_tap_installs, 4);
        assert_eq!(
            accept_tap_installs.lock().unwrap()[3].kind,
            AcceptTapKind::Consumer
        );
    }

    #[test]
    fn rearm_while_armed_reinstalls_the_consumer_and_keeps_the_armed_value() {
        // Recorder 5b slice 1: rearm drops the armed consumer tap and
        // re-installs it. The armed accept value must SURVIVE the rearm:
        // it is visibility state, not registration state.
        let accept_tap_installs = Arc::new(Mutex::new(Vec::new()));
        let accept_tap_events = Arc::new(Mutex::new(Vec::new()));
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        config.accept_tap_installs = Arc::clone(&accept_tap_installs);
        config.accept_tap_events = Arc::clone(&accept_tap_events);
        let adapter = test_adapter_with_hooks(config);
        let (action_tx, action_rx) = mpsc::channel();

        let subscription = adapter
            .subscribe_accept(Arc::new(move |action| {
                action_tx.send(action).expect("action send");
            }))
            .expect("subscribe accept");
        subscription
            .set_suggestion_visible(true)
            .expect("activate consumer");
        // [Observer, Shortcut, Consumer] — the two process-lifetime resources
        // install before the first consumer arm (finding C).
        wait_for_accept_tap_count(&accept_tap_installs, 3);

        subscription.rearm_accept_tap().expect("rearm");
        wait_for_accept_tap_count(&accept_tap_installs, 4);
        assert_eq!(
            accept_tap_installs.lock().unwrap()[3].kind,
            AcceptTapKind::Consumer
        );
        // DROP-BEFORE-INSTALL is load-bearing (Esc/Down exist in every
        // keymap — install-first would double-register them): the old tap's
        // drop must land strictly before the rearm's install. A refactor to
        // "build new, assign over old" would pass the count asserts above
        // but flip this sequence (review-c132).
        //
        // Assert the rearm's own SUFFIX (drop→install), not the whole log
        // from adapter birth: pinning the full [Observer, Consumer, …]
        // construction prefix is brittle — an unrelated change to the initial
        // install order would break this test without touching rearm. The
        // discriminating invariant is purely the trailing pair.
        let events = accept_tap_events.lock().unwrap().clone();
        assert_eq!(
            &events[events.len().saturating_sub(2)..],
            &["drop".to_string(), "install:Consumer".to_string()],
            "rearm must drop the old consumer tap strictly before installing the new one"
        );
        // The NEW handler still consumes with the armed value intact.
        let handler = Arc::clone(&accept_tap_installs.lock().unwrap()[3].handler);
        assert_eq!(
            handler(accept_tap_event(CGEventType::KeyDown, KEYCODE_TAB, 0)),
            AcceptTapDecision::Drop(AcceptAction::Word)
        );
        assert_eq!(
            action_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("accept action after rearm"),
            TapControl::Accept(AcceptAction::Word)
        );
    }

    #[test]
    fn rearm_while_unarmed_is_a_successful_noop() {
        // No ghost visible = no consumer tap registered = nothing to
        // re-register; the next arm cycle reads the (new) keymap anyway.
        let accept_tap_installs = Arc::new(Mutex::new(Vec::new()));
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        config.accept_tap_installs = Arc::clone(&accept_tap_installs);
        let adapter = test_adapter_with_hooks(config);

        let subscription = adapter
            .subscribe_accept(Arc::new(|_| {}))
            .expect("subscribe accept");
        wait_for_accept_tap_count(&accept_tap_installs, 2); // observer + shortcut

        subscription
            .rearm_accept_tap()
            .expect("unarmed rearm is Ok");
        // Still just the process-lifetime installs (observer + shortcut) — no
        // phantom consumer.
        assert_eq!(accept_tap_installs.lock().unwrap().len(), 2);
    }

    #[test]
    fn accept_subscription_delayed_hide_tears_down_consumer_tap() {
        let accept_tap_installs = Arc::new(Mutex::new(Vec::new()));
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        config.accept_tap_installs = Arc::clone(&accept_tap_installs);
        let adapter = test_adapter_with_hooks(config);

        let subscription = adapter
            .subscribe_accept(Arc::new(|_| {}))
            .expect("subscribe accept");
        subscription
            .set_suggestion_visible(true)
            .expect("activate consumer");
        wait_for_accept_tap_count(&accept_tap_installs, 3);

        subscription
            .hide_suggestion_after(Duration::from_millis(10))
            .expect("schedule delayed hide");
        thread::sleep(Duration::from_millis(50));
        subscription
            .set_suggestion_visible(true)
            .expect("reactivate after delayed hide");

        wait_for_accept_tap_count(&accept_tap_installs, 4);
        assert_eq!(
            accept_tap_installs.lock().unwrap()[3].kind,
            AcceptTapKind::Consumer
        );
    }

    #[test]
    fn accept_subscription_visible_update_cancels_delayed_hide() {
        let accept_tap_installs = Arc::new(Mutex::new(Vec::new()));
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        config.accept_tap_installs = Arc::clone(&accept_tap_installs);
        let adapter = test_adapter_with_hooks(config);

        let subscription = adapter
            .subscribe_accept(Arc::new(|_| {}))
            .expect("subscribe accept");
        subscription
            .set_suggestion_visible(true)
            .expect("activate consumer");
        wait_for_accept_tap_count(&accept_tap_installs, 3);

        subscription
            .hide_suggestion_after(Duration::from_millis(30))
            .expect("schedule delayed hide");
        subscription
            .set_suggestion_visible(true)
            .expect("cancel delayed hide");
        thread::sleep(Duration::from_millis(70));
        subscription
            .set_suggestion_visible(true)
            .expect("still active after canceled hide");

        assert_eq!(accept_tap_installs.lock().unwrap().len(), 3);
    }

    #[test]
    fn tap_ignore_decision_ignores_exact_self_generated_tag() {
        assert!(should_ignore_event_for_tap(SYNTHETIC_EVENT_TAG));
    }

    #[test]
    fn tap_ignore_decision_passes_untagged_events() {
        assert!(!should_ignore_event_for_tap(0));
    }

    #[test]
    fn tap_ignore_decision_requires_exact_tag_match() {
        assert!(!should_ignore_event_for_tap(SYNTHETIC_EVENT_TAG - 1));
        assert!(!should_ignore_event_for_tap(SYNTHETIC_EVENT_TAG + 1));
    }

    #[test]
    fn synthetic_event_tag_can_be_detected_by_future_taps() {
        let source = CGEventSource::new(CGEventSourceStateID::Private).expect("source");
        let event =
            CGEvent::new_keyboard_event(source, KeyCode::SPACE, true).expect("keyboard event");

        assert!(!is_self_generated_event(&event));
        tag_synthetic_event(&event);
        assert!(is_self_generated_event(&event));
    }

    #[test]
    fn insert_empty_text_is_noop_for_axset() {
        let adapter = test_adapter_with_secure_input(false);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: pointer_identity("ax:0x123").field_element_id(),
            generation: 1,
        };

        assert_eq!(
            adapter.insert(&field, "", InsertStrategy::AxSet),
            Ok(Inserted {
                bytes: 0,
                chars: 0,
                strategy: InsertStrategy::AxSet,
            })
        );
    }

    #[test]
    fn text_context_uses_utf16_offsets_and_splits_on_caret() {
        let field = FocusTokenFactory::new().focused_field("TextEdit", Some(42), "element");

        let context = text_context_from_value(
            field.clone(),
            "Hi 😀 there".into(),
            CFRange {
                location: 5,
                length: 0,
            },
        );

        assert_eq!(context.left, "Hi 😀");
        assert_eq!(context.right, " there");
        assert_eq!(context.selection, None);
        assert_eq!(context.caret, 5);
        assert_eq!(context.field_id, field);
        assert_eq!(context.source, ContextSource::Accessibility);
        assert_eq!(context.offset_encoding, OffsetEncoding::Utf16CodeUnits);
    }

    #[test]
    fn text_context_omits_selected_text_from_left_and_right() {
        let field = FocusTokenFactory::new().focused_field("TextEdit", Some(42), "element");

        let context = text_context_from_value(
            field,
            "Hello world".into(),
            CFRange {
                location: 6,
                length: 5,
            },
        );

        assert_eq!(context.left, "Hello ");
        assert_eq!(context.right, "");
        assert_eq!(context.selection, Some(TextRange { start: 6, end: 11 }));
        assert_eq!(context.caret, 6);
    }

    #[test]
    fn text_context_clamps_out_of_range_utf16_offsets() {
        let field = FocusTokenFactory::new().focused_field("TextEdit", Some(42), "element");

        let context = text_context_from_value(
            field,
            "abc".into(),
            CFRange {
                location: 99,
                length: 99,
            },
        );

        assert_eq!(context.left, "abc");
        assert_eq!(context.right, "");
        assert_eq!(context.selection, None);
        assert_eq!(context.caret, 3);
    }

    #[test]
    fn splice_text_inserts_at_utf16_caret() {
        let (value, caret) = splice_text_at_utf16_range(
            "Hi 😀 there",
            CFRange {
                location: 5,
                length: 0,
            },
            "!",
        );

        assert_eq!(value, "Hi 😀! there");
        assert_eq!(caret, 6);
    }

    #[test]
    fn extend_range_left_clamps_a_negative_ax_location() {
        // AX apps do return junk ranges; a negative location clamps to the
        // start (nothing left of it to extend over). A regression to a direct
        // `as usize` cast would wrap to a huge offset.
        let range = extend_range_left(
            "hello",
            CFRange {
                location: -3,
                length: 0,
            },
            2,
        );
        assert_eq!((range.location, range.length), (0, 0));
    }

    #[test]
    fn extend_range_left_covers_typed_token_then_splice_replaces_it() {
        // ":smile" typed after "x"; caret at UTF-16 7. A replacement deletes those
        // 6 chars and inserts the glyph → "x😄".
        let range = extend_range_left(
            "x:smile",
            CFRange {
                location: 7,
                length: 0,
            },
            6,
        );
        assert_eq!(range.location, 1);
        assert_eq!(range.length, 6);
        let (value, caret) = splice_text_at_utf16_range("x:smile", range, "😄");
        assert_eq!(value, "x😄");
        assert_eq!(caret, 3); // "x" (1) + 😄 (2 UTF-16 units)
    }

    #[test]
    fn correction_range_splice_replaces_midword_without_left_fragment_leak() {
        let selected_range = CFRange {
            location: 2,
            length: 0,
        };
        let range = scalar_correction_range_to_utf16_range(
            "te",
            "h later",
            selected_range,
            CorrectionRange { start: 0, end: 3 },
        )
        .expect("midword correction range");
        let (value, caret) = splice_text_at_utf16_range("teh later", range, "the");

        assert_eq!(value, "the later");
        assert_eq!(caret, 3);
    }

    #[test]
    fn range_readback_treats_normalized_replacement_as_applied_not_silent() {
        // `insert_range_for_field` classifies a grammar-fix range replacement
        // with `axset_readback_outcome` (original == the pre-write field text),
        // mirroring `insert_for_field`. The original field is `"teh later"`, the
        // intended replacement `"the later"`.
        let inserted = Inserted {
            bytes: 3,
            chars: 3,
            strategy: InsertStrategy::AxSet,
        };
        // Readback == the exact replacement → applied.
        assert_eq!(
            axset_readback_outcome("teh later", "the later", inserted.clone()),
            AxSetApply::Applied(inserted.clone())
        );
        // Readback still byte-identical to the ORIGINAL → the write silently did
        // nothing (the iTerm2 quirk / wrong-range no-op) → refuse.
        assert_eq!(
            axset_readback_outcome("teh later", "teh later", inserted.clone()),
            AxSetApply::SilentlyIgnored
        );
        // App-side normalization: the field applied the replacement but also
        // trimmed/rewrote it, so the readback differs from BOTH the original and
        // the intended `new_value`. This is a COMPLETED replacement, not a silent
        // no-op — classify Applied. The former exact-match helper misclassified
        // this as SilentlyIgnored, producing a hard error after the field was
        // already mutated (and a retrying caller would double-apply).
        assert_eq!(
            axset_readback_outcome("teh later", "“the” later", inserted),
            AxSetApply::Applied(Inserted {
                bytes: 3,
                chars: 3,
                strategy: InsertStrategy::AxSet,
            })
        );
    }

    #[test]
    fn caret_set_failure_after_value_write_is_non_fatal_and_selectively_logged() {
        // After a landed AX value write, a caret-set failure must not turn the
        // completed write into an error (see `set_caret_after_value_write`). The
        // only observable is logging: `UnsupportedField` is expected and silent,
        // any other error is surfaced.
        assert!(!caret_set_failure_is_worth_logging(
            &PlatformError::UnsupportedField {
                reason: "no settable selected range".into(),
            }
        ));
        assert!(caret_set_failure_is_worth_logging(
            &PlatformError::StaleField
        ));
        assert!(caret_set_failure_is_worth_logging(
            &PlatformError::CannotComplete {
                reason: "AX tree rebuilt".into(),
            }
        ));
    }

    #[test]
    fn range_readback_divergence_logs_only_when_readback_matches_neither() {
        // The range-replacement divergence log fires only when the readback
        // equals NEITHER the value we wrote nor the original field text. A clean
        // apply (readback == new_value) and the silent no-op (readback ==
        // original) both stay quiet; app-side normalization to a third string is
        // the diagnostic case worth logging.
        let original = "teh cat";
        let new_value = "the cat";
        assert!(!range_readback_diverged(original, new_value, new_value));
        assert!(!range_readback_diverged(original, new_value, original));
        assert!(range_readback_diverged(original, new_value, "the  cat"));
    }

    #[test]
    fn extend_range_left_is_utf16_aware_for_astral_prefix() {
        // "🎉:1" — 🎉 is 2 UTF-16 units; caret at 4 (after "1"). Delete ":1" (2 chars).
        let range = extend_range_left(
            "🎉:1",
            CFRange {
                location: 4,
                length: 0,
            },
            2,
        );
        assert_eq!(range.location, 2); // immediately after 🎉
        assert_eq!(range.length, 2); // ":1" spans 2 UTF-16 units
    }

    #[test]
    fn scalar_correction_range_to_utf16_range_handles_ascii_and_zero_length() {
        let collapsed = CFRange {
            location: 5,
            length: 0,
        };
        let range = scalar_correction_range_to_utf16_range(
            "I saw",
            " teh",
            collapsed,
            CorrectionRange { start: 6, end: 9 },
        )
        .expect("range");
        assert_eq!(range.location, 6);
        assert_eq!(range.length, 3);

        let empty = scalar_correction_range_to_utf16_range(
            "hello",
            "",
            collapsed,
            CorrectionRange { start: 2, end: 2 },
        )
        .expect("empty range");
        assert_eq!(empty.location, 2);
        assert_eq!(empty.length, 0);
    }

    #[test]
    fn scalar_correction_range_to_utf16_range_accounts_for_astral_scalars() {
        let before = scalar_correction_range_to_utf16_range(
            "😀 t",
            "eh",
            CFRange {
                location: 4,
                length: 0,
            },
            CorrectionRange { start: 2, end: 5 },
        )
        .expect("range after astral prefix");
        assert_eq!(before.location, 3);
        assert_eq!(before.length, 3);

        let inside = scalar_correction_range_to_utf16_range(
            "a",
            "😀b",
            CFRange {
                location: 1,
                length: 0,
            },
            CorrectionRange { start: 1, end: 3 },
        )
        .expect("range containing astral scalar");
        assert_eq!(inside.location, 1);
        assert_eq!(inside.length, 3);
    }

    #[test]
    fn scalar_correction_range_to_utf16_range_fails_closed_across_selection_gap() {
        // TextContext left/right omit the live selection. A correction entirely
        // before or after the omitted gap is still contiguous in the raw field;
        // a correction spanning the gap is not safely representable.
        let selected = CFRange {
            location: 3,
            length: 2,
        };
        let before = scalar_correction_range_to_utf16_range(
            "abc",
            "def",
            selected,
            CorrectionRange { start: 0, end: 3 },
        )
        .expect("before selection");
        assert_eq!(before.location, 0);
        assert_eq!(before.length, 3);

        let after = scalar_correction_range_to_utf16_range(
            "abc",
            "def",
            selected,
            CorrectionRange { start: 3, end: 6 },
        )
        .expect("after selection");
        assert_eq!(after.location, 5);
        assert_eq!(after.length, 3);

        assert!(scalar_correction_range_to_utf16_range(
            "abc",
            "def",
            selected,
            CorrectionRange { start: 2, end: 4 },
        )
        .is_none());
    }

    #[test]
    fn scalar_correction_range_splice_allows_empty_text_to_delete_range() {
        let range = scalar_correction_range_to_utf16_range(
            "I saw ",
            "teh",
            CFRange {
                location: 6,
                length: 0,
            },
            CorrectionRange { start: 6, end: 9 },
        )
        .expect("range");
        let (value, caret) = splice_text_at_utf16_range("I saw teh", range, "");
        assert_eq!(value, "I saw ");
        assert_eq!(caret, 6);
    }

    #[test]
    fn extend_range_left_zero_replace_is_unchanged() {
        let range = extend_range_left(
            "abc",
            CFRange {
                location: 2,
                length: 0,
            },
            0,
        );
        assert_eq!(range.location, 2);
        assert_eq!(range.length, 0);
    }

    #[test]
    fn extend_range_left_clamps_to_available_chars() {
        // replace_left larger than chars-before-caret deletes only what exists.
        let range = extend_range_left(
            ":1",
            CFRange {
                location: 2,
                length: 0,
            },
            99,
        );
        assert_eq!(range.location, 0);
        assert_eq!(range.length, 2);
    }

    #[test]
    fn extend_range_left_does_not_sweep_in_an_existing_selection() {
        // Caret-anchored replacements use a collapsed range. If the field instead
        // has a live selection, the helper must delete ONLY the `replace_left`-char
        // typed prefix ending at the selection start — never the user's selected
        // text. "abcde", select "de" (loc 3, len 2), replace_left 2 → covers
        // utf16 [1,3) = "bc" (the prefix), leaving "de" intact.
        let range = extend_range_left(
            "abcde",
            CFRange {
                location: 3,
                length: 2,
            },
            2,
        );
        assert_eq!(range.location, 1);
        assert_eq!(range.length, 2); // only the 2-char prefix, selection untouched
    }

    #[test]
    fn extend_range_left_preserves_astral_selection() {
        // Multibyte variant of the selection-preservation fix: "😀bc" with 😀 a
        // 2-UTF-16-unit astral char. Caret/selection start at UTF-16 3 (after
        // "😀b"), selecting "c" (loc 3, len 1). replace_left 1 must cover exactly
        // the one-char prefix "b" → utf16 [2,3), leaving "c" selected. The length
        // (1) must NOT be swept into the returned range, or the splice would
        // delete the user's selected "c" along with the typed token.
        let range = extend_range_left(
            "😀bc",
            CFRange {
                location: 3,
                length: 1,
            },
            1,
        );
        assert_eq!(range.location, 2); // immediately after 😀 (2 UTF-16 units)
        assert_eq!(range.length, 1); // only "b"; "c" selection untouched
    }

    #[test]
    fn splice_text_replaces_selected_utf16_range() {
        let (value, caret) = splice_text_at_utf16_range(
            "Hello world",
            CFRange {
                location: 6,
                length: 5,
            },
            "there",
        );

        assert_eq!(value, "Hello there");
        assert_eq!(caret, 11);
    }

    #[test]
    fn splice_text_clamps_out_of_range_selection() {
        let (value, caret) = splice_text_at_utf16_range(
            "abc",
            CFRange {
                location: 99,
                length: 99,
            },
            "!",
        );

        assert_eq!(value, "abc!");
        assert_eq!(caret, 4);
    }

    #[test]
    fn resolve_caret_rect_uses_zero_length_rect_when_usable() {
        let exact = ScreenRect {
            x: 10.0,
            y: 20.0,
            w: 2.0,
            h: 18.0,
        };
        let mut calls = Vec::new();

        let rect = resolve_caret_rect(5, |location, length| {
            calls.push((location, length));
            Ok(Some(exact))
        })
        .expect("resolve caret");

        assert_eq!(rect, Some(exact));
        assert_eq!(calls, [(5, 0)]);
    }

    #[test]
    fn resolve_caret_rect_derives_from_previous_character_right_edge() {
        let previous = ScreenRect {
            x: 10.0,
            y: 20.0,
            w: 8.0,
            h: 18.0,
        };
        let mut calls = Vec::new();

        let rect = resolve_caret_rect(5, |location, length| {
            calls.push((location, length));
            Ok(if length == 0 { None } else { Some(previous) })
        })
        .expect("resolve caret");

        assert_eq!(
            rect,
            Some(ScreenRect {
                x: 18.0,
                y: 20.0,
                w: 1.0,
                h: 18.0,
            })
        );
        assert_eq!(calls, [(5, 0), (4, 1)]);
    }

    #[test]
    fn resolve_caret_rect_rejects_container_zero_length_before_fallback() {
        let container = ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 2500.0,
            h: 18.0,
        };
        let previous = ScreenRect {
            x: 10.0,
            y: 20.0,
            w: 8.0,
            h: 18.0,
        };

        let rect = resolve_caret_rect(5, |_, length| {
            Ok(Some(if length == 0 { container } else { previous }))
        })
        .expect("resolve caret");

        assert_eq!(
            rect,
            Some(ScreenRect {
                x: 18.0,
                y: 20.0,
                w: 1.0,
                h: 18.0,
            })
        );
    }

    #[test]
    fn resolve_caret_rect_does_not_request_previous_character_at_zero() {
        let mut calls = Vec::new();

        let rect = resolve_caret_rect(0, |location, length| {
            calls.push((location, length));
            Ok(None)
        })
        .expect("resolve caret");

        assert_eq!(rect, None);
        assert_eq!(calls, [(0, 0)]);
    }

    #[test]
    fn normalize_ax_screen_rect_preserves_global_point_coordinates() {
        let rect = normalize_ax_screen_rect(
            CGRect {
                origin: CGPoint {
                    x: -127.5,
                    y: 42.25,
                },
                size: CGSize {
                    width: 1.5,
                    height: 18.75,
                },
            },
            &[],
        );

        assert_eq!(
            rect,
            ScreenRect {
                x: -127.5,
                y: 42.25,
                w: 1.5,
                h: 18.75,
            }
        );
    }

    fn retina_display() -> DisplayScale {
        DisplayScale {
            bounds: CGRect {
                origin: CGPoint::new(0.0, 0.0),
                size: CGSize::new(1440.0, 900.0),
            },
            scale: 2.0,
        }
    }

    #[test]
    fn normalize_ax_screen_rect_passes_through_points_on_a_display() {
        let rect = normalize_ax_screen_rect(
            CGRect {
                origin: CGPoint::new(720.0, 450.0),
                size: CGSize::new(2.0, 18.0),
            },
            &[retina_display()],
        );
        assert_eq!(
            rect,
            ScreenRect {
                x: 720.0,
                y: 450.0,
                w: 2.0,
                h: 18.0
            }
        );
    }

    #[test]
    fn normalize_ax_screen_rect_divides_pixel_space_rect_by_backing_scale() {
        // Origin (1500, 880) lands on no display in points (the Retina display
        // is 1440x900 points), but /2 lands inside it — so it was reported in
        // pixels and must be divided by the backing scale factor.
        let rect = normalize_ax_screen_rect(
            CGRect {
                origin: CGPoint::new(1500.0, 880.0),
                size: CGSize::new(4.0, 36.0),
            },
            &[retina_display()],
        );
        assert_eq!(
            rect,
            ScreenRect {
                x: 750.0,
                y: 440.0,
                w: 2.0,
                h: 18.0
            }
        );
    }

    #[test]
    fn normalize_ax_screen_rect_preserves_when_scale_cannot_explain_offset() {
        // Off every display even after scaling — ambiguous, so preserve the
        // raw rect rather than guess.
        let rect = normalize_ax_screen_rect(
            CGRect {
                origin: CGPoint::new(9000.0, 9000.0),
                size: CGSize::new(2.0, 18.0),
            },
            &[retina_display()],
        );
        assert_eq!(
            rect,
            ScreenRect {
                x: 9000.0,
                y: 9000.0,
                w: 2.0,
                h: 18.0
            }
        );
    }

    fn primary_display() -> DisplayScale {
        DisplayScale {
            bounds: CGRect {
                origin: CGPoint::new(0.0, 0.0),
                size: CGSize::new(1440.0, 900.0),
            },
            scale: 1.0,
        }
    }

    fn secondary_retina_display() -> DisplayScale {
        DisplayScale {
            bounds: CGRect {
                origin: CGPoint::new(1440.0, 0.0),
                size: CGSize::new(1280.0, 800.0),
            },
            scale: 2.0,
        }
    }

    #[test]
    fn normalize_ax_screen_rect_passes_through_points_on_a_non_primary_display() {
        // Origin (1500, 100) is already inside the secondary display's point
        // bounds, so it must pass through untouched — not be mistaken for
        // pixels and divided by the primary's scale.
        let rect = normalize_ax_screen_rect(
            CGRect {
                origin: CGPoint::new(1500.0, 100.0),
                size: CGSize::new(2.0, 18.0),
            },
            &[primary_display(), secondary_retina_display()],
        );
        assert_eq!(
            rect,
            ScreenRect {
                x: 1500.0,
                y: 100.0,
                w: 2.0,
                h: 18.0
            }
        );
    }

    #[test]
    fn normalize_ax_screen_rect_divides_by_the_matching_display_scale_not_a_unit_display() {
        // Origin (5000, 100) lands on neither display in points. /1.0 still
        // lands on neither, but /2.0 lands inside the Retina secondary — so the
        // Retina scale is the one that explains it.
        let rect = normalize_ax_screen_rect(
            CGRect {
                origin: CGPoint::new(5000.0, 100.0),
                size: CGSize::new(4.0, 36.0),
            },
            &[primary_display(), secondary_retina_display()],
        );
        assert_eq!(
            rect,
            ScreenRect {
                x: 2500.0,
                y: 50.0,
                w: 2.0,
                h: 18.0
            }
        );
    }

    #[test]
    fn normalize_ax_screen_rect_empty_display_list_preserves_off_screen_rect() {
        // With no displays known, there is nothing to validate against — the
        // rect must pass through without panicking.
        let rect = normalize_ax_screen_rect(
            CGRect {
                origin: CGPoint::new(9000.0, 9000.0),
                size: CGSize::new(2.0, 18.0),
            },
            &[],
        );
        assert_eq!(
            rect,
            ScreenRect {
                x: 9000.0,
                y: 9000.0,
                w: 2.0,
                h: 18.0
            }
        );
    }

    #[test]
    fn resolve_caret_rect_returns_none_when_no_tier_is_usable() {
        let rect = resolve_caret_rect(5, |_, _| {
            Ok(Some(ScreenRect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            }))
        })
        .expect("resolve caret");

        assert_eq!(rect, None);
    }

    #[test]
    fn resolve_caret_rect_propagates_hard_bounds_errors() {
        let rect = resolve_caret_rect(5, |_, _| Err(PlatformError::StaleField));

        assert_eq!(rect, Err(PlatformError::StaleField));
    }

    #[test]
    fn resolve_caret_rect_with_marker_first_prefers_marker_rect() {
        let marker = ScreenRect {
            x: 30.0,
            y: 40.0,
            w: 1.0,
            h: 18.0,
        };
        let mut range_called = false;

        let rect = resolve_caret_rect_with_marker_first(
            5,
            || Ok(Some(marker)),
            |_, _| {
                range_called = true;
                Ok(None)
            },
        )
        .expect("resolve caret");

        assert_eq!(rect, Some(marker));
        assert!(!range_called);
    }

    #[test]
    fn resolve_caret_rect_with_marker_first_prefers_zero_width_chromium_marker() {
        // Finding-3 guardrail (2026-07-01): the AXTextMarker path must be
        // first-class for Chromium/WebKit, which return ZERO-WIDTH marker rects
        // (G5). A collapsed caret (w == 0.0) is a valid thin bar and must be
        // preferred over the range fallback — never treated as degenerate. This
        // pins the end-to-end decision (usable_caret_rect + marker-first) so a
        // regression that rejected zero-width markers, silently breaking Chrome
        // caret geometry, fails here (the four other resolver tests use w > 0 or
        // a container marker and would not catch it).
        let chromium_marker = ScreenRect {
            x: 120.0,
            y: 240.0,
            w: 0.0,
            h: 16.0,
        };
        let mut range_called = false;

        let rect = resolve_caret_rect_with_marker_first(
            7,
            || Ok(Some(chromium_marker)),
            |_, _| {
                range_called = true;
                Ok(None)
            },
        )
        .expect("resolve caret");

        assert_eq!(rect, Some(chromium_marker));
        assert!(
            !range_called,
            "range fallback must not run for a usable marker"
        );
    }

    #[test]
    fn resolve_caret_rect_with_marker_first_falls_back_when_marker_missing() {
        let native = ScreenRect {
            x: 10.0,
            y: 20.0,
            w: 1.0,
            h: 18.0,
        };
        let mut range_calls = Vec::new();

        let rect = resolve_caret_rect_with_marker_first(
            5,
            || Ok(None),
            |location, length| {
                range_calls.push((location, length));
                Ok(Some(native))
            },
        )
        .expect("resolve caret");

        assert_eq!(rect, Some(native));
        assert_eq!(range_calls, [(5, 0)]);
    }

    #[test]
    fn resolve_caret_rect_with_marker_first_falls_back_from_container_marker() {
        let container = ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 2500.0,
            h: 18.0,
        };
        let native = ScreenRect {
            x: 10.0,
            y: 20.0,
            w: 1.0,
            h: 18.0,
        };

        let rect = resolve_caret_rect_with_marker_first(
            5,
            || Ok(Some(container)),
            |_, _| Ok(Some(native)),
        )
        .expect("resolve caret");

        assert_eq!(rect, Some(native));
    }

    #[test]
    fn resolve_caret_rect_with_marker_first_propagates_marker_errors() {
        let rect = resolve_caret_rect_with_marker_first(
            5,
            || Err(PlatformError::StaleField),
            |_, _| Ok(None),
        );

        assert_eq!(rect, Err(PlatformError::StaleField));
    }

    #[test]
    fn caret_diagnostics_prefers_usable_marker_rect() {
        let marker = ScreenRect {
            x: 10.0,
            y: 20.0,
            w: 1.0,
            h: 18.0,
        };
        let native = ScreenRect {
            x: 30.0,
            y: 20.0,
            w: 1.0,
            h: 18.0,
        };

        let diagnostics = caret_diagnostics_from_rects(Some(marker), Some(native));

        assert_eq!(diagnostics.source, MacosCaretRectSource::Marker);
        assert_eq!(diagnostics.resolved_rect, Some(marker));
    }

    #[test]
    fn caret_diagnostics_falls_back_from_unusable_marker_rect() {
        let marker = ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 2500.0,
            h: 18.0,
        };
        let native = ScreenRect {
            x: 30.0,
            y: 20.0,
            w: 1.0,
            h: 18.0,
        };

        let diagnostics = caret_diagnostics_from_rects(Some(marker), Some(native));

        assert_eq!(diagnostics.source, MacosCaretRectSource::NativeFallback);
        assert_eq!(diagnostics.marker_rect, Some(marker));
        assert_eq!(diagnostics.resolved_rect, Some(native));
    }

    #[test]
    fn caret_diagnostics_records_none_without_any_rect() {
        let diagnostics = caret_diagnostics_from_rects(None, None);

        assert_eq!(diagnostics.source, MacosCaretRectSource::None);
        assert_eq!(diagnostics.resolved_rect, None);
    }

    #[test]
    fn non_accept_key_keeps_event() {
        // A key that is neither Tab nor grave must not be consumed.
        let event = accept_tap_event(CGEventType::KeyDown, 11, 0);
        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Consumer,
                event,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::Keep
        );
    }

    #[test]
    fn accept_tap_decision_keeps_keyup_tab() {
        // Only KeyDown is consumed; the matching KeyUp passes through.
        let event = accept_tap_event(CGEventType::KeyUp, KEYCODE_TAB, 0);
        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Consumer,
                event,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::Keep
        );
    }

    #[test]
    fn accept_tap_decision_keeps_keyup_grave() {
        let event = accept_tap_event(CGEventType::KeyUp, KEYCODE_GRAVE, 0);
        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Consumer,
                event,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::Keep
        );
    }

    #[test]
    fn observer_tap_keeps_tab() {
        let event = accept_tap_event(CGEventType::KeyDown, KEYCODE_TAB, 0);
        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Observer,
                event,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::Keep
        );
    }

    #[test]
    fn accept_tap_decision_ignores_self_generated_grave() {
        // Our own synthetic grave insertion must never re-enter as an accept.
        let event = accept_tap_event(CGEventType::KeyDown, KEYCODE_GRAVE, SYNTHETIC_EVENT_TAG);
        assert_eq!(
            accept_tap_decision(
                &AcceptKeymap::default(),
                AcceptTapKind::Consumer,
                event,
                Some(AcceptAction::Full)
            ),
            AcceptTapDecision::Keep
        );
    }

    #[test]
    fn overlay_frame_with_zero_primary_height_does_not_panic() {
        let frame = overlay_frame_for_text(
            ScreenRect {
                x: 10.0,
                y: 50.0,
                w: 1.0,
                h: 14.0,
            },
            "x",
            0.0,
        );
        // 0 - 50 - 1.5*14 - 18/2
        assert_eq!(frame.y, -80.0);
        assert!(frame.y.is_finite());
    }

    #[test]
    fn overlay_frame_at_exact_primary_height() {
        let frame = overlay_frame_for_text(
            ScreenRect {
                x: 0.0,
                y: 1000.0,
                w: 1.0,
                h: 14.0,
            },
            "x",
            1000.0,
        );
        assert_eq!(frame.y, 1000.0 - 1000.0 - 21.0 - 9.0);
    }

    #[test]
    fn overlay_frame_small_caret_height_clamps_and_flips() {
        // h clamps up to the 16 floor; centering uses the LINE height (2) for
        // the line midpoint and the clamped BOX height for the box midpoint.
        let frame = overlay_frame_for_text(
            ScreenRect {
                x: 0.0,
                y: 100.0,
                w: 1.0,
                h: 2.0,
            },
            "x",
            1000.0,
        );
        assert_eq!(frame.h, 16.0);
        assert_eq!(frame.y, 1000.0 - 100.0 - 3.0 - 8.0);
    }

    #[test]
    fn backing_scale_is_pixel_over_point_width() {
        // 2x Retina: 3024 native px over 1512 points = 2.0 (the case
        // CGDisplayPixelsWide could not detect).
        assert_eq!(backing_scale(3024, 1512), 2.0);
        // 1x display: native px == points = 1.0.
        assert_eq!(backing_scale(3840, 3840), 1.0);
        // Degenerate point width falls back to 1.0 (never divide by zero).
        assert_eq!(backing_scale(3024, 0), 1.0);
        // Zero native pixels yields 0.0; `active_display_scales` filters that out
        // (`scale > 0.0`) and falls back to 1.0, so a bogus mode never reaches
        // `normalize_ax_screen_rect`.
        assert_eq!(backing_scale(0, 1512), 0.0);
    }

    #[test]
    fn usable_caret_rect_accepts_normal_and_rejects_boundaries() {
        assert!(usable_caret_rect(ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 14.0,
        }));
        // A collapsed caret is legitimately zero-width (a thin vertical bar);
        // it must be accepted. Chrome/WebKit return such marker rects (G5).
        assert!(usable_caret_rect(ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 14.0,
        }));
        // Zero height is still rejected (a null/degenerate rect, not a caret).
        assert!(!usable_caret_rect(ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 0.0,
        }));
        // Negative width is rejected (malformed).
        assert!(!usable_caret_rect(ScreenRect {
            x: 0.0,
            y: 0.0,
            w: -1.0,
            h: 14.0,
        }));
        // Negative height is rejected.
        assert!(!usable_caret_rect(ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: -1.0,
        }));
        // Exact-max bounds are rejected (the cutoff is strict `<`).
        assert!(!usable_caret_rect(ScreenRect {
            x: 0.0,
            y: 0.0,
            w: MAX_USABLE_CARET_RECT_WIDTH,
            h: 14.0,
        }));
        assert!(!usable_caret_rect(ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: MAX_USABLE_CARET_RECT_HEIGHT,
        }));
        // over-max rejected (container-sized rects)
        assert!(!usable_caret_rect(ScreenRect {
            x: 0.0,
            y: 0.0,
            w: MAX_USABLE_CARET_RECT_WIDTH + 1.0,
            h: 14.0,
        }));
        assert!(!usable_caret_rect(ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: MAX_USABLE_CARET_RECT_HEIGHT + 1.0,
        }));
    }

    #[test]
    fn caret_diagnostics_uses_native_when_marker_absent() {
        let native = Some(ScreenRect {
            x: 1.0,
            y: 2.0,
            w: 1.0,
            h: 12.0,
        });
        let diag = caret_diagnostics_from_rects(None, native);
        assert_eq!(diag.source, MacosCaretRectSource::NativeFallback);
        assert_eq!(diag.resolved_rect, native);
    }

    #[test]
    fn caret_diagnostics_falls_back_when_marker_unusable() {
        let unusable_marker = Some(ScreenRect {
            x: 0.0,
            y: 0.0,
            w: MAX_USABLE_CARET_RECT_WIDTH + 10.0,
            h: 12.0,
        });
        let native = Some(ScreenRect {
            x: 5.0,
            y: 6.0,
            w: 1.0,
            h: 12.0,
        });
        let diag = caret_diagnostics_from_rects(unusable_marker, native);
        assert_eq!(diag.source, MacosCaretRectSource::NativeFallback);
        assert_eq!(diag.resolved_rect, native);
    }

    #[test]
    fn field_has_secure_text_subrole_matches_substring() {
        let secure = FieldHandle {
            app: "App".into(),
            pid: Some(1),
            element_id: format!("role=AXTextField|subrole={kAXSecureTextFieldSubrole}"),
            generation: 1,
        };
        let normal = FieldHandle {
            app: "App".into(),
            pid: Some(1),
            element_id: "role=AXTextField".into(),
            generation: 1,
        };
        assert!(field_has_secure_text_subrole(&secure));
        assert!(!field_has_secure_text_subrole(&normal));
    }

    #[test]
    fn insertion_strategy_covers_all_branches() {
        assert_eq!(
            insertion_strategy(true, false, false, false),
            InsertStrategy::AxSet
        );
        assert_eq!(
            insertion_strategy(false, true, false, true),
            InsertStrategy::SyntheticKeys
        );
        assert_eq!(
            insertion_strategy(false, false, true, true),
            InsertStrategy::Clipboard
        );
        assert_eq!(
            insertion_strategy(false, true, true, false),
            InsertStrategy::None
        );
        assert_eq!(
            insertion_strategy(false, false, false, true),
            InsertStrategy::None
        );
    }

    #[test]
    fn splice_text_into_empty_value() {
        let (value, caret) = splice_text_at_utf16_range(
            "",
            CFRange {
                location: 0,
                length: 0,
            },
            "hi",
        );
        assert_eq!(value, "hi");
        assert_eq!(caret, 2);
    }

    #[test]
    fn splice_text_at_surrogate_boundary() {
        // "a😀b": UTF-16 units a@0, 😀@1..3, b@3. Inserting at unit 1 (before the
        // emoji) must keep the emoji intact.
        let (value, caret) = splice_text_at_utf16_range(
            "a😀b",
            CFRange {
                location: 1,
                length: 0,
            },
            "X",
        );
        assert_eq!(value, "aX😀b");
        assert_eq!(caret, 2);
    }

    #[test]
    fn splice_text_replaces_an_astral_char_by_utf16_range() {
        // Delete the emoji in "a😀b" (UTF-16 units 1..3, the surrogate pair) and
        // insert "X". The range spans an astral char; byte math must not split it.
        let (value, caret) = splice_text_at_utf16_range(
            "a😀b",
            CFRange {
                location: 1,
                length: 2,
            },
            "X",
        );
        assert_eq!(value, "aXb");
        assert_eq!(caret, 2);
    }

    #[test]
    fn byte_index_for_utf16_units_maps_units_to_byte_boundaries() {
        // "a😀b": a=1 byte/1 unit, 😀=4 bytes/2 units, b=1 byte/1 unit.
        assert_eq!(byte_index_for_utf16_units("a😀b", 0), 0);
        assert_eq!(byte_index_for_utf16_units("a😀b", 1), 1); // before 😀
                                                              // A target that bisects the surrogate pair rounds up to the char's end.
        assert_eq!(byte_index_for_utf16_units("a😀b", 2), 5); // mid-😀 → after 😀
        assert_eq!(byte_index_for_utf16_units("a😀b", 3), 5); // after 😀
        assert_eq!(byte_index_for_utf16_units("a😀b", 4), 6); // after b
        assert_eq!(byte_index_for_utf16_units("a😀b", 99), 6); // past end → len
    }

    #[test]
    fn process_exists_is_false_for_non_positive_pids() {
        assert!(!process_exists(0));
        assert!(!process_exists(-1));
    }

    #[test]
    fn process_exists_true_for_current_process() {
        // Our own pid is always live: kill(pid, 0) returns 0, so this exercises
        // the kill==0 success branch (the prior test only hit the pid<=0 guard).
        assert!(process_exists(std::process::id() as i32));
    }

    #[test]
    fn process_exists_false_for_unused_high_pid() {
        // i32::MAX is far above any plausible live pid, so kill(pid, 0) sets
        // errno to ESRCH ("no such process") and we report not-alive. This hits
        // the post-kill ESRCH branch.
        assert!(!process_exists(i32::MAX));
    }

    #[test]
    fn join_and_truncate_lines_returns_none_for_no_lines() {
        assert_eq!(join_and_truncate_lines(&[], 100), None);
    }

    #[test]
    fn join_and_truncate_lines_joins_with_a_single_space() {
        assert_eq!(
            join_and_truncate_lines(&["foo", "bar"], 100),
            Some("foo bar".to_string())
        );
    }

    #[test]
    fn join_and_truncate_lines_skips_blank_and_whitespace_lines() {
        // Blank / whitespace-only lines are dropped, leaving no double or leading
        // space in the joined result.
        assert_eq!(
            join_and_truncate_lines(&["foo", "  ", "", "bar"], 100),
            Some("foo bar".to_string())
        );
    }

    #[test]
    fn join_and_truncate_lines_truncates_to_max_chars() {
        assert_eq!(
            join_and_truncate_lines(&["hello world"], 5),
            Some("hello".to_string())
        );
    }

    #[test]
    fn join_and_truncate_lines_truncates_on_codepoint_boundaries() {
        // Truncation counts Unicode scalar values, never splitting a multi-byte
        // codepoint: 2 scalars of "a😀b😀c" is "a😀".
        assert_eq!(
            join_and_truncate_lines(&["a😀b😀c"], 2),
            Some("a😀".to_string())
        );
    }

    #[test]
    fn normalize_ax_screen_rect_preserves_negative_origin() {
        let rect = normalize_ax_screen_rect(
            CGRect {
                origin: CGPoint::new(-50.0, -10.0),
                size: CGSize::new(3.0, 14.0),
            },
            &[],
        );
        assert_eq!(
            rect,
            ScreenRect {
                x: -50.0,
                y: -10.0,
                w: 3.0,
                h: 14.0,
            }
        );
    }

    #[test]
    fn caret_coalescer_drops_duplicate_events_inside_window() {
        let field = FocusTokenFactory::new().focused_field("TextEdit", Some(42), "element");
        let mut coalescer = CaretCoalescer::new(25);
        let rect = Some(platform::ScreenRect {
            x: 1.0,
            y: 2.0,
            w: 1.0,
            h: 12.0,
        });

        assert_eq!(
            coalescer.observe(100, field.clone(), rect),
            Some((field.clone(), rect))
        );
        assert_eq!(coalescer.observe(110, field.clone(), rect), None);
        assert_eq!(
            coalescer.observe(126, field.clone(), rect),
            Some((field, rect))
        );
    }

    #[test]
    fn caret_coalescer_emits_field_or_position_changes_immediately() {
        let mut factory = FocusTokenFactory::new();
        let field_a = factory.focused_field("TextEdit", Some(42), "a");
        let field_b = factory.focused_field("TextEdit", Some(42), "b");
        let mut coalescer = CaretCoalescer::new(100);
        let rect_a = Some(platform::ScreenRect {
            x: 1.0,
            y: 2.0,
            w: 1.0,
            h: 12.0,
        });
        let rect_b = Some(platform::ScreenRect {
            x: 5.0,
            y: 2.0,
            w: 1.0,
            h: 12.0,
        });

        assert_eq!(
            coalescer.observe(100, field_a.clone(), rect_a),
            Some((field_a.clone(), rect_a))
        );
        assert_eq!(
            coalescer.observe(101, field_a.clone(), rect_b),
            Some((field_a, rect_b))
        );
        assert_eq!(
            coalescer.observe(102, field_b.clone(), rect_b),
            Some((field_b, rect_b))
        );
    }

    #[test]
    fn focused_element_lookup_falls_back_only_for_missing_attribute() {
        assert!(focused_element_lookup_allows_app_fallback(
            kAXErrorAttributeUnsupported
        ));
        assert!(focused_element_lookup_allows_app_fallback(kAXErrorNoValue));
        assert!(!focused_element_lookup_allows_app_fallback(
            kAXErrorCannotComplete
        ));
        assert!(!focused_element_lookup_allows_app_fallback(
            kAXErrorAPIDisabled
        ));
    }

    #[test]
    fn caret_observer_element_prefers_focused_element_when_available() {
        let app_element = 0x01usize as AXUIElementRef;
        let focused_element = 0x02usize as AXUIElementRef;

        assert_eq!(
            choose_caret_observer_element(app_element, Some(focused_element)),
            focused_element
        );
        assert_eq!(
            choose_caret_observer_element(app_element, None),
            app_element
        );
    }

    #[test]
    fn observer_registration_adds_source_and_notifications() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeObserverBackend::new(Arc::clone(&log));

        let _registration = AxObserverRegistration::register(
            backend,
            42,
            "element-a".to_string(),
            &[
                ObserverNotification::FocusChanged,
                ObserverNotification::CaretChanged,
            ],
        )
        .expect("registration");

        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "create_observer:42",
                "source:observer-42",
                "add_source:source-observer-42",
                "add:observer-42:element-a:AXFocusedUIElementChanged:null",
                "add:observer-42:element-a:AXSelectedTextChanged:null",
            ]
        );
    }

    #[test]
    fn observer_registration_passes_refcon_to_notifications() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeObserverBackend::new(Arc::clone(&log));
        let (tx, _rx) = mpsc::channel();
        let (callback_tx, _callback_rx) = mpsc::channel();
        let mut state = ObserverCallbackState {
            pid: 42,
            tx,
            callback_tx,
            dispatch: Arc::new(|_| {}),
        };
        let refcon = &mut state as *mut ObserverCallbackState as *mut c_void;

        let registration = AxObserverRegistration::register_with_refcon(
            backend,
            42,
            "element-a".to_string(),
            &[ObserverNotification::FocusChanged],
            refcon,
        )
        .expect("registration");

        assert_eq!(registration.refcon(), refcon);
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "create_observer:42",
                "source:observer-42",
                "add_source:source-observer-42",
                "add:observer-42:element-a:AXFocusedUIElementChanged:refcon",
            ]
        );
    }

    #[test]
    fn observer_registration_cleans_up_partial_registration_on_add_failure() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let backend =
            FakeObserverBackend::failing_on(Arc::clone(&log), ObserverNotification::CaretChanged);

        let err = match AxObserverRegistration::register(
            backend,
            42,
            "element-a".to_string(),
            &[
                ObserverNotification::FocusChanged,
                ObserverNotification::CaretChanged,
            ],
        ) {
            Ok(_) => panic!("expected registration failure"),
            Err(err) => err,
        };

        assert_eq!(err, PlatformError::Timeout);
        assert_eq!(
            log.lock().unwrap().as_slice(),
            [
                "create_observer:42",
                "source:observer-42",
                "add_source:source-observer-42",
                "add:observer-42:element-a:AXFocusedUIElementChanged:null",
                "fail_add:observer-42:element-a:AXSelectedTextChanged",
                "remove:observer-42:element-a:AXFocusedUIElementChanged",
                "remove_source:source-observer-42",
            ]
        );
    }

    #[test]
    fn observer_registration_removes_notifications_and_source_on_drop() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeObserverBackend::new(Arc::clone(&log));

        {
            let _registration = AxObserverRegistration::register(
                backend,
                42,
                "element-a".to_string(),
                &[
                    ObserverNotification::FocusChanged,
                    ObserverNotification::CaretChanged,
                ],
            )
            .expect("registration");
        }

        assert_eq!(
            log.lock().unwrap().as_slice(),
            [
                "create_observer:42",
                "source:observer-42",
                "add_source:source-observer-42",
                "add:observer-42:element-a:AXFocusedUIElementChanged:null",
                "add:observer-42:element-a:AXSelectedTextChanged:null",
                "remove:observer-42:element-a:AXFocusedUIElementChanged",
                "remove:observer-42:element-a:AXSelectedTextChanged",
                "remove_source:source-observer-42",
            ]
        );
    }

    #[test]
    fn ax_observer_callback_decodes_focus_and_caret_notifications_from_refcon() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_in_dispatch = Arc::clone(&events);
        let (tx, rx) = mpsc::channel();
        let (callback_tx, _callback_rx) = mpsc::channel();
        let mut state = ObserverCallbackState {
            pid: 42,
            tx,
            callback_tx,
            dispatch: Arc::new(move |event| {
                events_in_dispatch.lock().unwrap().push(event);
            }),
        };
        let refcon = &mut state as *mut ObserverCallbackState as *mut c_void;
        let focus = CFString::new(ObserverNotification::FocusChanged.name());
        let caret = CFString::new(ObserverNotification::CaretChanged.name());

        unsafe {
            ax_observer_callback(
                ptr::null_mut(),
                ptr::null_mut(),
                focus.as_concrete_TypeRef(),
                refcon,
            );
            ax_observer_callback(
                ptr::null_mut(),
                ptr::null_mut(),
                caret.as_concrete_TypeRef(),
                refcon,
            );
        }

        let first = rx.recv().expect("focus message");
        let second = rx.recv().expect("caret message");
        for (message, expected_notification) in [
            (first, ObserverNotification::FocusChanged),
            (second, ObserverNotification::CaretChanged),
        ] {
            let Message::ObserverEvent {
                notification,
                retained_element,
                fallback_element_id,
                ..
            } = message
            else {
                panic!("expected observer event message");
            };

            assert_eq!(notification, expected_notification);
            assert_eq!(retained_element, None);
            assert_eq!(fallback_element_id, "ax:null");
        }
        assert!(events.lock().unwrap().is_empty());
    }

    #[test]
    fn ax_observer_callback_ignores_null_refcon_and_unknown_notification() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_in_dispatch = Arc::clone(&events);
        let (tx, rx) = mpsc::channel();
        let (callback_tx, _callback_rx) = mpsc::channel();
        let mut state = ObserverCallbackState {
            pid: 42,
            tx,
            callback_tx,
            dispatch: Arc::new(move |event| {
                events_in_dispatch.lock().unwrap().push(event);
            }),
        };
        let refcon = &mut state as *mut ObserverCallbackState as *mut c_void;
        let focus = CFString::new(ObserverNotification::FocusChanged.name());
        let unknown = CFString::new("AXOtherNotification");

        unsafe {
            ax_observer_callback(
                ptr::null_mut(),
                ptr::null_mut(),
                focus.as_concrete_TypeRef(),
                ptr::null_mut(),
            );
            ax_observer_callback(
                ptr::null_mut(),
                ptr::null_mut(),
                unknown.as_concrete_TypeRef(),
                refcon,
            );
        }

        assert!(events.lock().unwrap().is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn macos_platform_adapter_allocates_distinct_subscription_ids() {
        let installs = Arc::new(Mutex::new(Vec::new()));
        let adapter = test_adapter(Some(42), Arc::clone(&installs), None);

        let focus = adapter
            .subscribe_focus(Arc::new(|_| {}))
            .expect("focus subscription");
        let caret = adapter
            .subscribe_caret(Arc::new(|_, _| {}))
            .expect("caret subscription");

        assert_ne!(focus.id(), caret.id());
        assert_eq!(focus.id(), 1);
        assert_eq!(caret.id(), 2);
        assert!(adapter.ax_worker_thread_id() != thread::current().id());
        assert_eq!(adapter.subscription_count().expect("count"), 2);

        let installs = installs.lock().unwrap();
        assert_eq!(installs.len(), 2);
        assert_eq!(installs[0].pid, 42);
        assert_eq!(installs[0].target, ObserverInstallTarget::App);
        assert_eq!(
            installs[0].notifications,
            vec![ObserverNotification::FocusChanged]
        );
        assert_eq!(installs[1].pid, 42);
        assert_eq!(
            installs[1].target,
            ObserverInstallTarget::FocusedElementWithAppFallback
        );
        assert_eq!(
            installs[1].notifications,
            vec![ObserverNotification::CaretChanged]
        );
    }

    #[test]
    fn subscribe_caret_prefers_focused_element_observer_with_app_fallback() {
        let installs = Arc::new(Mutex::new(Vec::new()));
        let adapter = test_adapter(Some(42), Arc::clone(&installs), None);

        let _caret = adapter
            .subscribe_caret(Arc::new(|_, _| {}))
            .expect("caret subscription");

        let installs = installs.lock().unwrap();
        assert_eq!(installs.len(), 1);
        assert_eq!(
            installs[0].target,
            ObserverInstallTarget::FocusedElementWithAppFallback
        );
        assert_eq!(
            installs[0].notifications,
            vec![ObserverNotification::CaretChanged]
        );
    }

    #[test]
    fn macos_platform_adapter_does_not_store_subscription_when_observer_install_fails() {
        let installs = Arc::new(Mutex::new(Vec::new()));
        let adapter = test_adapter(
            Some(42),
            Arc::clone(&installs),
            Some(PlatformError::Timeout),
        );

        let err = adapter.subscribe_focus(Arc::new(|_| {})).unwrap_err();

        assert_eq!(err, PlatformError::Timeout);
        assert!(installs.lock().unwrap().is_empty());
        assert_eq!(adapter.subscription_count().expect("count"), 0);
    }

    #[test]
    fn dropping_focus_subscription_removes_observer_and_suppresses_late_dispatch() {
        let installs = Arc::new(Mutex::new(Vec::new()));
        let adapter = test_adapter(Some(42), Arc::clone(&installs), None);
        let focused = Arc::new(Mutex::new(Vec::new()));
        let focused_in_cb = Arc::clone(&focused);

        let focus = adapter
            .subscribe_focus(Arc::new(move |field| {
                focused_in_cb.lock().unwrap().push(field);
            }))
            .expect("focus subscription");
        let dispatch = installs.lock().unwrap()[0].dispatch.clone();

        assert_eq!(adapter.subscription_count().expect("count"), 1);
        drop(focus);

        assert_eq!(adapter.subscription_count().expect("count"), 0);
        dispatch(observer_event(
            ObserverNotification::FocusChanged,
            pointer_identity("ax:late-focus"),
        ));
        assert!(focused.lock().unwrap().is_empty());
    }

    #[test]
    fn dropping_caret_subscription_removes_observer_and_suppresses_late_dispatch() {
        let installs = Arc::new(Mutex::new(Vec::new()));
        let adapter = test_adapter(Some(42), Arc::clone(&installs), None);
        let carets = Arc::new(Mutex::new(Vec::new()));
        let carets_in_cb = Arc::clone(&carets);

        let caret = adapter
            .subscribe_caret(Arc::new(move |field, rect| {
                carets_in_cb.lock().unwrap().push((field, rect));
            }))
            .expect("caret subscription");
        let dispatch = installs.lock().unwrap()[0].dispatch.clone();

        assert_eq!(adapter.subscription_count().expect("count"), 1);
        drop(caret);

        assert_eq!(adapter.subscription_count().expect("count"), 0);
        dispatch(observer_event(
            ObserverNotification::CaretChanged,
            pointer_identity("ax:late-caret"),
        ));
        assert!(carets.lock().unwrap().is_empty());
    }

    #[test]
    fn dropping_subscription_with_poisoned_registry_still_removes_only_that_entry() {
        // The cancel closure recovers a poisoned registry lock with `into_inner`
        // (a panic on another thread that held the lock must not leak the
        // subscription forever). Pin that the recovered path removes exactly the
        // dropped id and leaves the sibling registered — observed through the
        // poison-recovering count, since the fail-closed `subscription_count`
        // would itself error on the poisoned lock.
        let installs = Arc::new(Mutex::new(Vec::new()));
        let adapter = test_adapter(Some(42), Arc::clone(&installs), None);

        let focus = adapter
            .subscribe_focus(Arc::new(|_| {}))
            .expect("focus subscription");
        let _caret = adapter
            .subscribe_caret(Arc::new(|_, _| {}))
            .expect("caret subscription");
        assert_eq!(adapter.subscription_count().expect("count"), 2);

        // Poison the registry mutex: panic while holding the lock.
        let subscriptions = Arc::clone(&adapter.subscriptions);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = subscriptions.lock().unwrap();
            panic!("poison the subscription registry");
        }));
        assert!(
            adapter.subscriptions.lock().is_err(),
            "registry is poisoned"
        );

        // The fail-closed accessor refuses a poisoned lock...
        assert!(adapter.subscription_count().is_err());
        // ...but the cancel path recovers it and still removes exactly the
        // dropped subscription, leaving the caret entry intact.
        drop(focus);
        assert_eq!(adapter.subscription_count_recovering_poison(), 1);
    }

    #[test]
    fn macos_platform_adapter_requires_frontmost_pid_before_subscription() {
        let installs = Arc::new(Mutex::new(Vec::new()));
        let adapter = test_adapter(None, Arc::clone(&installs), None);

        let err = adapter.subscribe_focus(Arc::new(|_| {})).unwrap_err();

        assert_eq!(
            err,
            PlatformError::CannotComplete {
                reason: "no frontmost application pid".into(),
            }
        );
        assert!(installs.lock().unwrap().is_empty());
        assert_eq!(adapter.subscription_count().expect("count"), 0);
    }

    #[test]
    fn stale_field_operation_for_exited_pid_reports_app_exited() {
        let installs = Arc::new(Mutex::new(Vec::new()));
        let mut config = TestAdapterConfig::new(Some(42), installs, None);
        config.process_exists = Arc::new(|_| false);
        let adapter = test_adapter_with_hooks(config);

        let err = adapter
            .map_app_exited::<()>(42, "pid:42".into(), Err(PlatformError::StaleField))
            .unwrap_err();

        assert_eq!(
            err,
            PlatformError::AppExited {
                app: "pid:42".into(),
            }
        );
    }

    #[test]
    fn stale_field_operation_for_running_pid_stays_stale() {
        let installs = Arc::new(Mutex::new(Vec::new()));
        let mut config = TestAdapterConfig::new(Some(42), installs, None);
        config.process_exists = Arc::new(|_| true);
        let adapter = test_adapter_with_hooks(config);

        let err = adapter
            .map_app_exited::<()>(42, "pid:42".into(), Err(PlatformError::StaleField))
            .unwrap_err();

        assert_eq!(err, PlatformError::StaleField);
    }

    #[test]
    fn macos_platform_adapter_dispatches_focus_and_caret_callbacks_from_observer_notifications() {
        let installs = Arc::new(Mutex::new(Vec::new()));
        let adapter = test_adapter(Some(42), Arc::clone(&installs), None);
        let focused = Arc::new(Mutex::new(Vec::new()));
        let carets = Arc::new(Mutex::new(Vec::new()));
        let focused_in_cb = Arc::clone(&focused);
        let carets_in_cb = Arc::clone(&carets);

        let _focus = adapter
            .subscribe_focus(Arc::new(move |field| {
                focused_in_cb.lock().unwrap().push(field);
            }))
            .expect("focus subscription");
        let _caret = adapter
            .subscribe_caret(Arc::new(move |field, rect| {
                carets_in_cb.lock().unwrap().push((field, rect));
            }))
            .expect("caret subscription");

        let installs = installs.lock().unwrap();
        (installs[0].dispatch)(observer_event(
            ObserverNotification::FocusChanged,
            resolved_identity("ax:0x111", 99, Some("editor-main")),
        ));
        (installs[0].dispatch)(observer_event(
            ObserverNotification::CaretChanged,
            pointer_identity("ax:0x222"),
        ));
        (installs[1].dispatch)(observer_event(
            ObserverNotification::CaretChanged,
            pointer_identity("ax:0x333"),
        ));
        (installs[1].dispatch)(observer_event(
            ObserverNotification::CaretChanged,
            pointer_identity("ax:0x333"),
        ));
        (installs[1].dispatch)(observer_event(
            ObserverNotification::CaretChanged,
            pointer_identity("ax:0x555"),
        ));
        (installs[1].dispatch)(observer_event(
            ObserverNotification::FocusChanged,
            pointer_identity("ax:0x444"),
        ));
        drop(installs);

        let focused = focused.lock().unwrap();
        assert_eq!(focused.len(), 1);
        assert_eq!(focused[0].app, "pid:99");
        assert_eq!(focused[0].pid, Some(99));
        assert_eq!(
            focused[0].element_id,
            "ax:ptr=ax:0x111|pid=99|id=editor-main|role=AXTextArea"
        );

        let carets = carets.lock().unwrap();
        assert_eq!(carets.len(), 2);
        assert_eq!(carets[0].0.app, "pid:42");
        assert_eq!(carets[0].0.pid, Some(42));
        assert_eq!(carets[0].0.element_id, "ax:ptr=ax:0x333");
        assert_eq!(carets[0].1, None);
        assert_eq!(carets[1].0.element_id, "ax:ptr=ax:0x555");
        assert_ne!(carets[1].0.generation, carets[0].0.generation);
    }

    #[test]
    fn focus_subscription_rebinds_to_new_frontmost_pid_and_ignores_old_events() {
        let frontmost_pid = Arc::new(Mutex::new(Some(42)));
        let installs = Arc::new(Mutex::new(Vec::new()));
        let teardowns = Arc::new(Mutex::new(Vec::new()));
        let adapter = test_adapter_with_dynamic_frontmost(
            Arc::clone(&frontmost_pid),
            Arc::clone(&installs),
            Arc::clone(&teardowns),
        );
        let focused = Arc::new(Mutex::new(Vec::new()));
        let focused_in_cb = Arc::clone(&focused);

        let _focus = adapter
            .subscribe_focus(Arc::new(move |field| {
                focused_in_cb.lock().unwrap().push(field);
            }))
            .expect("focus subscription");
        wait_for_install_count(&installs, 1);

        *frontmost_pid.lock().unwrap() = Some(99);
        wait_for_install_count(&installs, 2);
        // The poller pushes install #1 *before* it swaps the live binding to
        // pid 99 (and drops the old pid-42 binding). Waiting only on the install
        // count races that swap: a dispatch could still filter against pid 42.
        // The pid-42 teardown fires during the swap, so it is the correct
        // happens-after signal that the live binding is now pid 99.
        wait_for_vec_count(&teardowns, 1);
        assert_eq!(teardowns.lock().unwrap().as_slice(), [42]);
        let installs_snapshot = installs.lock().unwrap().clone();
        assert_eq!(installs_snapshot[0].pid, 42);
        assert_eq!(installs_snapshot[1].pid, 99);
        assert_eq!(installs_snapshot[1].target, ObserverInstallTarget::App);

        (installs_snapshot[0].dispatch)(observer_event_for_pid(
            42,
            ObserverNotification::FocusChanged,
            pointer_identity("ax:old"),
            None,
        ));
        (installs_snapshot[1].dispatch)(observer_event_for_pid(
            99,
            ObserverNotification::FocusChanged,
            pointer_identity("ax:new"),
            None,
        ));

        let focused = focused.lock().unwrap();
        assert_eq!(focused.len(), 1);
        assert_eq!(focused[0].app, "pid:99");
        assert_eq!(focused[0].pid, Some(99));
        assert_eq!(focused[0].element_id, "ax:ptr=ax:new");
    }

    #[test]
    fn caret_subscription_rebinds_and_does_not_reuse_same_pointer_across_pids() {
        let frontmost_pid = Arc::new(Mutex::new(Some(42)));
        let installs = Arc::new(Mutex::new(Vec::new()));
        let teardowns = Arc::new(Mutex::new(Vec::new()));
        let adapter = test_adapter_with_dynamic_frontmost(
            Arc::clone(&frontmost_pid),
            Arc::clone(&installs),
            Arc::clone(&teardowns),
        );
        let carets = Arc::new(Mutex::new(Vec::new()));
        let carets_in_cb = Arc::clone(&carets);

        let _caret = adapter
            .subscribe_caret(Arc::new(move |field, rect| {
                carets_in_cb.lock().unwrap().push((field, rect));
            }))
            .expect("caret subscription");
        wait_for_install_count(&installs, 1);
        let first_dispatch = installs.lock().unwrap()[0].dispatch.clone();
        first_dispatch(observer_event_for_pid(
            42,
            ObserverNotification::CaretChanged,
            pointer_identity("ax:same"),
            None,
        ));

        *frontmost_pid.lock().unwrap() = Some(99);
        wait_for_install_count(&installs, 2);
        // Wait for the pid-42 teardown so the live binding swap to pid 99 has
        // completed before dispatching (see the focus rebind test for why the
        // install count alone races the swap).
        wait_for_vec_count(&teardowns, 1);
        assert_eq!(teardowns.lock().unwrap().as_slice(), [42]);
        let installs_snapshot = installs.lock().unwrap().clone();
        assert_eq!(installs_snapshot[1].pid, 99);
        assert_eq!(
            installs_snapshot[1].target,
            ObserverInstallTarget::FocusedElementWithAppFallback
        );

        (installs_snapshot[0].dispatch)(observer_event_for_pid(
            42,
            ObserverNotification::CaretChanged,
            pointer_identity("ax:old"),
            None,
        ));
        (installs_snapshot[1].dispatch)(observer_event_for_pid(
            99,
            ObserverNotification::CaretChanged,
            pointer_identity("ax:same"),
            None,
        ));

        let carets = carets.lock().unwrap();
        assert_eq!(carets.len(), 2);
        assert_eq!(carets[0].0.app, "pid:42");
        assert_eq!(carets[0].0.pid, Some(42));
        assert_eq!(carets[1].0.app, "pid:99");
        assert_eq!(carets[1].0.pid, Some(99));
        assert_ne!(carets[1].0.generation, carets[0].0.generation);
    }

    #[test]
    fn focus_subscription_clears_binding_when_no_app_is_frontmost_then_rebinds() {
        let frontmost_pid = Arc::new(Mutex::new(Some(42)));
        let installs = Arc::new(Mutex::new(Vec::new()));
        let teardowns = Arc::new(Mutex::new(Vec::new()));
        let adapter = test_adapter_with_dynamic_frontmost(
            Arc::clone(&frontmost_pid),
            Arc::clone(&installs),
            Arc::clone(&teardowns),
        );
        let focused = Arc::new(Mutex::new(Vec::new()));
        let focused_in_cb = Arc::clone(&focused);

        let _focus = adapter
            .subscribe_focus(Arc::new(move |field| {
                focused_in_cb.lock().unwrap().push(field);
            }))
            .expect("focus subscription");
        wait_for_install_count(&installs, 1);
        let first_dispatch = installs.lock().unwrap()[0].dispatch.clone();

        *frontmost_pid.lock().unwrap() = None;
        // Wait until the rebind poller has actually torn down the pid-42 binding
        // (deterministic), rather than sleeping a fixed interval and hoping the
        // poll thread ran — that fixed sleep flaked under heavy parallel load.
        wait_for_vec_count(&teardowns, 1);
        assert_eq!(teardowns.lock().unwrap().as_slice(), [42]);
        first_dispatch(observer_event_for_pid(
            42,
            ObserverNotification::FocusChanged,
            pointer_identity("ax:old-after-exit"),
            None,
        ));
        assert!(focused.lock().unwrap().is_empty());

        *frontmost_pid.lock().unwrap() = Some(77);
        wait_for_install_count(&installs, 2);
        let second_dispatch = installs.lock().unwrap()[1].dispatch.clone();
        second_dispatch(observer_event_for_pid(
            77,
            ObserverNotification::FocusChanged,
            pointer_identity("ax:reborn"),
            None,
        ));

        wait_for_vec_count(&focused, 1);
        let focused = focused.lock().unwrap();
        assert_eq!(focused.len(), 1);
        assert_eq!(focused[0].app, "pid:77");
        assert_eq!(focused[0].pid, Some(77));
    }

    #[test]
    fn caret_subscription_forwards_observer_rect_to_callback() {
        let installs = Arc::new(Mutex::new(Vec::new()));
        let adapter = test_adapter(Some(42), Arc::clone(&installs), None);
        let carets = Arc::new(Mutex::new(Vec::new()));
        let carets_in_cb = Arc::clone(&carets);
        let rect = Some(platform::ScreenRect {
            x: 10.0,
            y: 20.0,
            w: 1.0,
            h: 14.0,
        });

        let _caret = adapter
            .subscribe_caret(Arc::new(move |field, rect| {
                carets_in_cb.lock().unwrap().push((field, rect));
            }))
            .expect("caret subscription");

        let installs = installs.lock().unwrap();
        (installs[0].dispatch)(observer_event_with_rect(
            ObserverNotification::CaretChanged,
            pointer_identity("ax:0x333"),
            rect,
        ));
        drop(installs);

        let carets = carets.lock().unwrap();
        assert_eq!(carets.len(), 1);
        assert_eq!(carets[0].0.element_id, "ax:ptr=ax:0x333");
        assert_eq!(carets[0].1, rect);
    }

    #[test]
    fn overlay_diagnostics_report_all_false_when_no_panel_present() {
        // A presenter that has never shown a ghost has no live NSPanel, so the
        // diagnostics must report the deterministic all-absent baseline rather
        // than reading a panel. Built via struct literal to bypass `new()`'s
        // MainThreadMarker requirement (this branch never touches AppKit).
        let presenter = MacosOverlayPresenter {
            panel: None,
            label: None,
            underline_panel: None,
            last_rect: None,
        };

        let diagnostics = presenter.diagnostics_for_acceptance();

        assert_eq!(
            diagnostics,
            MacosOverlayDiagnostics {
                has_panel: false,
                visible: false,
                ignores_mouse_events: false,
                nonactivating_panel: false,
                can_become_key_window: false,
                level: 0,
                joins_all_spaces: false,
                fullscreen_auxiliary: false,
                panel_frame: None,
            }
        );
    }
}
