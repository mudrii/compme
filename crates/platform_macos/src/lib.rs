//! macOS platform adapter scaffolding.

use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::ffi::{c_uchar, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, Once};
use std::thread::{self, ThreadId};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use accessibility_sys::{
    kAXBoundsForRangeParameterizedAttribute, kAXErrorAPIDisabled, kAXErrorAttributeUnsupported,
    kAXErrorCannotComplete, kAXErrorFailure, kAXErrorIllegalArgument, kAXErrorInvalidUIElement,
    kAXErrorNoValue, kAXErrorParameterizedAttributeUnsupported, kAXErrorSuccess,
    kAXFocusedUIElementAttribute, kAXIdentifierAttribute, kAXRoleAttribute,
    kAXSecureTextFieldSubrole, kAXSelectedTextRangeAttribute, kAXSubroleAttribute,
    kAXTrustedCheckOptionPrompt, kAXValueAttribute, kAXValueTypeCFRange, kAXValueTypeCGRect,
    AXError, AXIsProcessTrusted, AXIsProcessTrustedWithOptions, AXUIElementCopyAttributeValue,
    AXUIElementCopyParameterizedAttributeValue, AXUIElementCreateApplication, AXUIElementGetPid,
    AXUIElementIsAttributeSettable, AXUIElementRef, AXUIElementSetAttributeValue, AXValueCreate,
    AXValueGetValue, AXValueRef,
};
use core_foundation::array::CFArray;
use core_foundation::base::{CFRange, CFRelease, CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_graphics::display::CGDisplay;
use core_graphics::event::{CGEvent, CGEventFlags, CGEventType, EventField, KeyCode};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use dispatch2::{DispatchQueue, DispatchTime};
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

use ax_worker::{
    start_dynamic_observer_binding, CallbackMessage, DynamicObserverBinding,
    DynamicObserverBindingConfig, ObserverBindingConfig, ObserverDispatch, ObserverEvent,
    WorkerResource,
};

mod ax_worker;
pub mod keychain;
mod login_item;
mod settings_window;
mod shell_host;
mod tray;
mod ui_prompt;
mod url_events;
// Crate-internal: the AX worker is an implementation detail of
// MacosPlatformAdapter. No consumer outside this crate (workspace or the
// acceptance examples) names these types, so they stay off the public API.
pub(crate) use ax_worker::{AxWorker, CallbackDispatcher, ObserverNotification};
pub use login_item::set_launch_at_login;
pub use settings_window::{
    keycode_label, keycode_label_with_mods, policy_restore_needed, rebind_request_for,
    record_decision, MacosSettingsWindow, RecordDecision, RecorderRole,
};
pub use shell_flags::{
    AppsPolicyEdit, CurrentAcceptKeys, EffectiveAcceptKeys, KeyWithMods, KeymapError,
    PersonalizationEdit, RebindRequest, SettingsFlags, ShortcutBindings, APPS_ROWS,
    APP_POLICY_FIELDS, APP_POLICY_FIELD_TITLES, SETUP_ROWS, STATS_ROWS,
};
pub use shell_host::MacosShellHost;
pub use tray::{apply_tray_action, DisableArm, MacosTray, TrayAction, TrayFlags};
pub use ui_prompt::{
    confirm_prompt, confirmation_button_titles, confirmation_response_is_explicit,
};
pub use url_events::{dispatch_gurl_event, install_url_event_handler, UrlEventHandler};

const CARET_COALESCE_INTERVAL_MS: u64 = 25;
const FIELD_IDENTITY_REGISTRY_CAPACITY: usize = 64;
const APP_REBIND_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_USABLE_CARET_RECT_WIDTH: f64 = 2000.0;
const MAX_USABLE_CARET_RECT_HEIGHT: f64 = 200.0;
const AX_SELECTED_TEXT_MARKER_RANGE_ATTRIBUTE: &str = "AXSelectedTextMarkerRange";
const AX_BOUNDS_FOR_TEXT_MARKER_RANGE_PARAMETERIZED_ATTRIBUTE: &str = "AXBoundsForTextMarkerRange";
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

pub(crate) type AdapterObserverInstallerFn = dyn Fn(
        i32,
        ObserverInstallTarget,
        Vec<ObserverNotification>,
        ObserverDispatch,
    ) -> Result<ObserverResource, PlatformError>
    + Send
    + Sync
    + 'static;
pub(crate) type FrontmostPidProvider = dyn Fn() -> Option<i32> + Send + Sync + 'static;
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
    fn CFHash(cf: CFTypeRef) -> usize;
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

pub struct MacosPlatformAdapter {
    worker: AxWorker,
    callback_dispatcher: CallbackDispatcher,
    next_subscription_id: AtomicU64,
    subscriptions: Arc<Mutex<HashMap<u64, SubscriptionEntry>>>,
    field_tracker: Arc<Mutex<CaretFieldTracker>>,
    frontmost_pid: Arc<FrontmostPidProvider>,
    now_ms: Arc<NowMsProvider>,
    secure_input_enabled: Arc<SecureInputProvider>,
    process_exists: Arc<ProcessExistsProvider>,
    synthetic_key_poster: Arc<SyntheticKeyPoster>,
    pasteboard_poster: Arc<PasteboardPoster>,
    backspace_poster: Arc<BackspacePoster>,
    observer_installer: AdapterObserverInstaller,
    accept_tap_installer: AdapterAcceptTapInstaller,
    ax_range_target: Arc<dyn AxRangeTarget + Send + Sync>,
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
    pub has_underline_panel: bool,
    pub underline_visible: bool,
    pub underline_frame: Option<ScreenRect>,
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
        /// Process-lifetime always-on shortcut registration (ids 5/6/7/8), held
        /// for the subscription's lifetime so toggles fire with no suggestion
        /// visible (finding C). Dropped on unsubscribe → hotkeys unregistered.
        _shortcut_tap: AcceptTapResource,
        _controller: Arc<AcceptTapController>,
    },
}

pub(crate) struct ObserverResource {
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
    ax_range_target: Arc<dyn AxRangeTarget + Send + Sync>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObserverInstallTarget {
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
    /// ToggleApp / ToggleGlobal, ids 5/6/7/8). Installed ONCE per subscription
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
                has_underline_panel: false,
                underline_visible: false,
                underline_frame: None,
            };
        };
        let behavior = panel.collectionBehavior();
        let frame = panel.frame();
        let underline_frame = self.underline_panel.as_ref().map(|panel| {
            let frame = panel.frame();
            ScreenRect {
                x: frame.origin.x,
                y: frame.origin.y,
                w: frame.size.width,
                h: frame.size.height,
            }
        });

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
            has_underline_panel: self.underline_panel.is_some(),
            underline_visible: self
                .underline_panel
                .as_ref()
                .is_some_and(|panel| panel.isVisible()),
            underline_frame,
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
    /// before inserting — a replacement) is honored atomically by `AxSet`.
    /// `SyntheticKeys`/`Clipboard` cannot safely read-modify-write a range, so
    /// non-zero replacement requests using those strategies fail closed before
    /// posting any text.
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

    pub(crate) fn with_worker(worker: AxWorker) -> Result<Self, PlatformError> {
        let clipboard_restore = Arc::new(ClipboardRestoreCoordinator::default());
        Ok(Self {
            worker,
            callback_dispatcher: CallbackDispatcher::new()?,
            next_subscription_id: AtomicU64::new(1),
            subscriptions: Arc::new(Mutex::new(HashMap::new())),
            field_tracker: Arc::new(Mutex::new(CaretFieldTracker::new())),
            frontmost_pid: Arc::new(frontmost_app_pid),
            now_ms: Arc::new(wall_clock_now_ms),
            secure_input_enabled: Arc::new(macos_secure_input_enabled),
            process_exists: Arc::new(process_exists),
            synthetic_key_poster: Arc::new(post_synthetic_text),
            pasteboard_poster: Arc::new(move |pid, text| {
                post_clipboard_text(pid, text, Arc::clone(&clipboard_restore))
            }),
            backspace_poster: Arc::new(post_synthetic_backspaces),
            observer_installer: AdapterObserverInstaller::Worker,
            accept_tap_installer: AdapterAcceptTapInstaller::Worker,
            ax_range_target: Arc::new(RawAxRangeTarget),
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
        let secure_input_enabled = Arc::clone(&self.secure_input_enabled);
        let pid = field
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .or_else(|| (self.frontmost_pid)())
            .ok_or_else(|| PlatformError::CannotComplete {
                reason: "no pid available for caret diagnostics".into(),
            })?;

        let result = self
            .worker
            .run(move || caret_diagnostics_for_field(pid, field, secure_input_enabled))?;
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
            ax_range_target,
        } = hooks;

        Self {
            worker,
            callback_dispatcher,
            next_subscription_id: AtomicU64::new(1),
            subscriptions: Arc::new(Mutex::new(HashMap::new())),
            field_tracker: Arc::new(Mutex::new(CaretFieldTracker::new())),
            frontmost_pid,
            now_ms,
            secure_input_enabled,
            process_exists,
            synthetic_key_poster,
            pasteboard_poster,
            backspace_poster,
            observer_installer: AdapterObserverInstaller::Custom(observer_installer),
            accept_tap_installer: AdapterAcceptTapInstaller::Custom(accept_tap_installer),
            ax_range_target,
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
        let field_tracker = Arc::clone(&self.field_tracker);
        let current_identity_key = Arc::new(Mutex::new(None));
        let binding_state = Arc::new(Mutex::new(None));
        let active = Arc::new(AtomicBool::new(true));
        let active_for_dispatch = Arc::clone(&active);
        let cb_for_dispatch = Arc::clone(&cb);
        let current_identity_key_for_dispatch = Arc::clone(&current_identity_key);
        let frontmost_pid_for_dispatch = Arc::clone(&self.frontmost_pid);
        let dispatch: ObserverDispatch = Arc::new(move |event: ObserverEvent| {
            if event.notification != ObserverNotification::FocusChanged {
                return;
            }
            if !active_for_dispatch.load(Ordering::Acquire) {
                return;
            }
            // Installing a new observer necessarily happens before the rebind
            // poller can publish it into `binding_state`. Use the desired
            // frontmost pid so the new observer cannot lose its first event in
            // that window, while stale observers stop dispatching immediately.
            if (frontmost_pid_for_dispatch)() != Some(event.pid) {
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

            let Ok(mut field_tracker) = field_tracker.lock() else {
                return;
            };
            let field = field_tracker.field_for_event(event.pid, &event.identity);
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
        let tracker = Arc::clone(&self.field_tracker);
        let coalescer = Arc::new(Mutex::new(CaretCoalescer::new(CARET_COALESCE_INTERVAL_MS)));
        let now_ms = Arc::clone(&self.now_ms);
        let binding_state = Arc::new(Mutex::new(None));
        let active = Arc::new(AtomicBool::new(true));
        let active_for_dispatch = Arc::clone(&active);
        let cb_for_dispatch = Arc::clone(&cb);
        let frontmost_pid_for_dispatch = Arc::clone(&self.frontmost_pid);
        let dispatch: ObserverDispatch = Arc::new(move |event: ObserverEvent| {
            if event.notification != ObserverNotification::CaretChanged {
                return;
            }
            if !active_for_dispatch.load(Ordering::Acquire) {
                return;
            }
            // See `subscribe_focus`: observer callbacks may arrive after
            // install succeeds but before the poller swaps `binding_state`.
            if (frontmost_pid_for_dispatch)() != Some(event.pid) {
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
        // Always-on shortcuts (ids 5/6/7/8) install ONCE here, for the
        // subscription lifetime — NOT armed/dropped with each visible suggestion
        // like the consumer tap (finding C). The delivery handler is the same
        // consumer handler: `active` is still the subscription-lifetime guard,
        // but shortcut events work with no suggestion showing because they carry
        // their action and do not require an armed `accept_action`.
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
        let secure_input_enabled = Arc::clone(&self.secure_input_enabled);
        let pid = field
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .or_else(|| (self.frontmost_pid)())
            .ok_or_else(|| PlatformError::CannotComplete {
                reason: "no pid available for capabilities".into(),
            })?;

        let result = self
            .worker
            .run(move || capabilities_for_field(pid, field, secure_input_enabled))?;
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
        let secure_input_enabled = Arc::clone(&self.secure_input_enabled);
        let pid = field
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .or_else(|| (self.frontmost_pid)())
            .ok_or_else(|| PlatformError::CannotComplete {
                reason: "no pid available for read_context".into(),
            })?;

        let result = self
            .worker
            .run(move || read_context_for_field(pid, field, secure_input_enabled))?;
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
        let secure_input_enabled = Arc::clone(&self.secure_input_enabled);
        let pid = field
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .or_else(|| (self.frontmost_pid)())
            .ok_or_else(|| PlatformError::CannotComplete {
                reason: "no pid available for caret_rect".into(),
            })?;

        let result = self
            .worker
            .run(move || caret_rect_for_field(pid, field, secure_input_enabled))?;
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
        let secure_input_enabled = Arc::clone(&self.secure_input_enabled);
        let pid = field
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .or_else(|| (self.frontmost_pid)())
            .ok_or_else(|| PlatformError::CannotComplete {
                reason: "no pid available for popup_anchor".into(),
            })?;

        let result = self
            .worker
            .run(move || popup_anchor_for_field(pid, field, secure_input_enabled))?;
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
        let secure_input_enabled = Arc::clone(&self.secure_input_enabled);
        let pid = field
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .or_else(|| (self.frontmost_pid)())
            .ok_or_else(|| PlatformError::CannotComplete {
                reason: "no pid available for text_range_rect".into(),
            })?;

        let result = self
            .worker
            .run(move || text_range_rect_for_field(pid, field, range, secure_input_enabled))?;
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
        expected_text: &str,
        text: &str,
        range: CorrectionRange,
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
        if strategy != InsertStrategy::AxSet {
            return Err(PlatformError::UnsupportedField {
                reason: "range replacement requires AxSet".into(),
            });
        }

        let field = field.clone();
        let app = field.app.clone();
        let secure_input_enabled = Arc::clone(&self.secure_input_enabled);
        let ax_range_target = Arc::clone(&self.ax_range_target);
        let expected_text = expected_text.to_string();
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
            .run(move || {
                insert_range_for_field(
                    pid,
                    field,
                    expected_text,
                    text,
                    range,
                    strategy,
                    secure_input_enabled,
                    ax_range_target.as_ref(),
                )
            })?
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
/// Opaque `CGImage` so `msg_send!` encodes the Vision argument as
/// `^{CGImage=}`. Passing the ref as `*mut c_void` encodes `^v`, which
/// objc2's debug-build signature verification rejects with a panic on the
/// OCR thread (live 2026-07-07: screen context died in every debug build).
#[repr(C)]
struct CGImageOpaque([u8; 0]);
// SAFETY: matches the Objective-C type encoding of `CGImageRef`'s pointee.
unsafe impl objc2::encode::RefEncode for CGImageOpaque {
    const ENCODING_REF: objc2::encode::Encoding =
        objc2::encode::Encoding::Pointer(&objc2::encode::Encoding::Struct("CGImage", &[]));
}

unsafe fn screen_ocr_with_image(image_ref: *mut c_void, max_chars: usize) -> Option<String> {
    // VNRequestTextRecognitionLevelFast — fast recognition keeps this off-the-critical
    // path call cheap; accurate-level full-display OCR would stall the run loop.
    const RECOGNITION_LEVEL_FAST: isize = 1;
    // Drain the autoreleased Vision/Foundation objects this pipeline creates; the
    // run loop is a manual poll loop with no per-iteration pool, so without this
    // they would accumulate for the process lifetime. The owned `String` result
    // is copied out before the pool drains.
    objc2::rc::autoreleasepool(|_| unsafe {
        let image: *const CGImageOpaque = image_ref.cast();
        let handler_alloc: *mut AnyObject = msg_send![class!(VNImageRequestHandler), alloc];
        let options: *mut AnyObject = msg_send![class!(NSDictionary), dictionary];
        let handler: *mut AnyObject =
            msg_send![handler_alloc, initWithCGImage: image, options: options];
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

fn post_clipboard_text(
    pid: i32,
    text: &str,
    coordinator: Arc<ClipboardRestoreCoordinator>,
) -> Result<(), PlatformError> {
    let pasteboard = NSPasteboard::generalPasteboard();
    let string_type = pasteboard_string_type();
    let previous_snapshot = coordinator.snapshot_for_insert(snapshot_pasteboard(&pasteboard)?);

    pasteboard.clearContents();
    if !pasteboard.setString_forType(&NSString::from_str(text), string_type) {
        let _ = restore_pasteboard(&pasteboard, &previous_snapshot);
        return Err(PlatformError::CannotComplete {
            reason: "failed to write completion text to pasteboard".into(),
        });
    }
    let completion_change_count = pasteboard.changeCount();
    let restore_epoch =
        coordinator.record_insert(previous_snapshot.clone(), completion_change_count);

    if let Err(error) = schedule_pasteboard_restore(Arc::clone(&coordinator), restore_epoch) {
        let _ =
            restore_coordinated_pasteboard_if_unchanged(&pasteboard, &coordinator, restore_epoch);
        return Err(error);
    }
    let post_result = post_command_v(pid);
    if post_result.is_err() {
        let _ =
            restore_coordinated_pasteboard_if_unchanged(&pasteboard, &coordinator, restore_epoch);
    }
    post_result
}

fn schedule_pasteboard_restore(
    coordinator: Arc<ClipboardRestoreCoordinator>,
    restore_epoch: u64,
) -> Result<(), PlatformError> {
    let when = DispatchTime::try_from(CLIPBOARD_RESTORE_DELAY).map_err(|()| {
        PlatformError::CannotComplete {
            reason: "clipboard restore deadline overflowed".into(),
        }
    })?;
    DispatchQueue::main()
        .after(when, move || {
            let pasteboard = NSPasteboard::generalPasteboard();
            let _ = restore_coordinated_pasteboard_if_unchanged(
                &pasteboard,
                &coordinator,
                restore_epoch,
            );
        })
        .map_err(|error| PlatformError::CannotComplete {
            reason: format!("failed to schedule clipboard restore: {error:?}"),
        })
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

#[derive(Debug)]
struct PendingClipboardRestore {
    snapshot: PasteboardSnapshot,
    expected_change_count: isize,
    epoch: u64,
}

#[derive(Debug, Default)]
struct ClipboardRestoreCoordinator {
    pending: Mutex<Option<PendingClipboardRestore>>,
    next_epoch: AtomicU64,
}

impl ClipboardRestoreCoordinator {
    fn snapshot_for_insert(&self, captured: PasteboardSnapshot) -> PasteboardSnapshot {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|pending| pending.snapshot.clone())
            .unwrap_or(captured)
    }

    fn record_insert(&self, snapshot: PasteboardSnapshot, expected_change_count: isize) -> u64 {
        let epoch = self.next_epoch.fetch_add(1, Ordering::Relaxed) + 1;
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match pending.as_mut() {
            Some(pending) => {
                pending.expected_change_count = expected_change_count;
                pending.epoch = epoch;
            }
            None => {
                *pending = Some(PendingClipboardRestore {
                    snapshot,
                    expected_change_count,
                    epoch,
                });
            }
        }
        epoch
    }

    fn take_if_current_epoch_and_change_count(
        &self,
        epoch: u64,
        actual: isize,
    ) -> Option<PasteboardSnapshot> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if pending
            .as_ref()
            .is_none_or(|pending| pending.epoch != epoch)
        {
            return None;
        }
        let pending = pending.take()?;
        (pending.expected_change_count == actual).then_some(pending.snapshot)
    }
}

fn restore_coordinated_pasteboard_if_unchanged(
    pasteboard: &NSPasteboard,
    coordinator: &ClipboardRestoreCoordinator,
    restore_epoch: u64,
) -> PasteboardRestoreOutcome {
    let Some(snapshot) =
        coordinator.take_if_current_epoch_and_change_count(restore_epoch, pasteboard.changeCount())
    else {
        return PasteboardRestoreOutcome::SkippedChanged;
    };
    restore_pasteboard(pasteboard, &snapshot)
}

fn snapshot_pasteboard(pasteboard: &NSPasteboard) -> Result<PasteboardSnapshot, PlatformError> {
    let fallback_string = pasteboard
        .stringForType(pasteboard_string_type())
        .map(|value| value.to_string());
    let items = pasteboard
        .pasteboardItems()
        .map(|items| snapshot_pasteboard_items(&items))
        .transpose()?
        .unwrap_or_default();

    Ok(PasteboardSnapshot {
        items,
        fallback_string,
    })
}

fn snapshot_pasteboard_items(
    items: &NSArray<NSPasteboardItem>,
) -> Result<Vec<PasteboardItemSnapshot>, PlatformError> {
    let mut snapshots = Vec::with_capacity(items.len());
    for item in items {
        let advertised_types = item.types();
        if advertised_types.is_empty() {
            return Err(PlatformError::CannotComplete {
                reason: "clipboard item advertised no restorable types".into(),
            });
        }
        let mut types = Vec::with_capacity(advertised_types.len());
        for pasteboard_type in advertised_types {
            let type_name = pasteboard_type.to_string();
            let data = item.dataForType(&pasteboard_type).ok_or_else(|| {
                PlatformError::CannotComplete {
                    reason: format!(
                        "clipboard type {type_name:?} could not be materialized safely"
                    ),
                }
            })?;
            types.push(PasteboardTypeSnapshot {
                type_name,
                data: data.to_vec(),
            });
        }
        snapshots.push(PasteboardItemSnapshot { types });
    }
    Ok(snapshots)
}

fn restore_pasteboard(
    pasteboard: &NSPasteboard,
    snapshot: &PasteboardSnapshot,
) -> PasteboardRestoreOutcome {
    restore_pasteboard_with_writer(pasteboard, snapshot, write_pasteboard_items)
}

fn restore_pasteboard_with_writer(
    pasteboard: &NSPasteboard,
    snapshot: &PasteboardSnapshot,
    mut writer: impl FnMut(&NSPasteboard, Vec<Retained<NSPasteboardItem>>) -> bool,
) -> PasteboardRestoreOutcome {
    if snapshot.items.is_empty() {
        restore_pasteboard_string(pasteboard, snapshot.fallback_string.as_deref());
        return PasteboardRestoreOutcome::Restored;
    }

    let (Some(items), Some(retry_items)) = (
        materialize_pasteboard_items(&snapshot.items),
        materialize_pasteboard_items(&snapshot.items),
    ) else {
        return PasteboardRestoreOutcome::FailedPreserved;
    };
    let current_string = pasteboard
        .stringForType(pasteboard_string_type())
        .map(|value| value.to_string());
    pasteboard.clearContents();
    if writer(pasteboard, items) {
        return PasteboardRestoreOutcome::Restored;
    }

    // `writeObjects:` can fail after the pasteboard has already been cleared.
    // Retry from a separately materialized copy of the complete multi-format
    // snapshot before falling back to the safest string content still known.
    pasteboard.clearContents();
    if writer(pasteboard, retry_items) {
        return PasteboardRestoreOutcome::Restored;
    }

    restore_pasteboard_string(
        pasteboard,
        snapshot
            .fallback_string
            .as_deref()
            .or(current_string.as_deref()),
    );
    PasteboardRestoreOutcome::FailedPreserved
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PasteboardRestoreOutcome {
    Restored,
    SkippedChanged,
    FailedPreserved,
}

#[cfg(test)]
fn restore_pasteboard_if_unchanged(
    pasteboard: &NSPasteboard,
    snapshot: &PasteboardSnapshot,
    expected_change_count: isize,
) -> PasteboardRestoreOutcome {
    if pasteboard.changeCount() != expected_change_count {
        return PasteboardRestoreOutcome::SkippedChanged;
    }

    restore_pasteboard(pasteboard, snapshot)
}

#[cfg(test)]
fn restore_pasteboard_items(
    pasteboard: &NSPasteboard,
    item_snapshots: &[PasteboardItemSnapshot],
) -> bool {
    let Some(items) = materialize_pasteboard_items(item_snapshots) else {
        return false;
    };
    write_pasteboard_items(pasteboard, items)
}

fn materialize_pasteboard_items(
    item_snapshots: &[PasteboardItemSnapshot],
) -> Option<Vec<Retained<NSPasteboardItem>>> {
    let mut items = Vec::with_capacity(item_snapshots.len());
    for item_snapshot in item_snapshots {
        let item = NSPasteboardItem::new();
        if !populate_pasteboard_item(&item, item_snapshot) {
            return None;
        }
        items.push(item);
    }
    Some(items)
}

fn write_pasteboard_items(
    pasteboard: &NSPasteboard,
    items: Vec<Retained<NSPasteboardItem>>,
) -> bool {
    let items = items
        .into_iter()
        .map(ProtocolObject::<dyn NSPasteboardWriting>::from_retained)
        .collect::<Vec<_>>();
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
        if !active.load(Ordering::Acquire) {
            return AcceptTapDecision::Keep;
        }

        // Always-on shortcuts fire even when accept interception is inactive
        // (no suggestion showing), but only while the subscription still owns
        // the installed shortcut resource.
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

fn shortcut_registration_plan(bindings: ShortcutBindings) -> Vec<(u32, i64, u32)> {
    [
        (CARBON_HOTKEY_FORCE_ACTIVATE, bindings.force_activate),
        (CARBON_HOTKEY_TOGGLE_APP, bindings.toggle_app),
        (CARBON_HOTKEY_TOGGLE_GLOBAL, bindings.toggle_global),
        (CARBON_HOTKEY_GRAMMAR_CHECK, bindings.grammar_check),
    ]
    .into_iter()
    .filter_map(|(id, binding)| binding.map(|(keycode, mask)| (id, keycode, mask)))
    .collect()
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
    /// keycode is outside Carbon's `u32` width, or if any two bindings collide.
    pub fn from_accept_keys(word: Option<i64>, full: Option<i64>) -> Result<Self, KeymapError> {
        Self::from_accept_keys_with_mods(word, full, 0, 0)
    }

    /// Like [`AcceptKeymap::from_accept_keys`] but the two rebindable keys carry a
    /// Carbon modifier mask (modifier-combo support, slice 1). A binding is
    /// identified by `(keycode, mask)`, so two keys collide only when BOTH match —
    /// Tab (word) and Shift+Tab (full) are distinct and may coexist. `word_mods`/
    /// `full_mods` of 0 reproduce the bare-key behavior exactly. Fails if a keycode
    /// is outside Carbon's `u32` width, or if any two of the four bindings share a
    /// keycode AND mask.
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
        if let Some(bad) = keys
            .into_iter()
            .flatten()
            .find(|&keycode| u32::try_from(keycode).is_err())
        {
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
        // Always-on shortcuts (ids 5/6/7/8) install ONCE per subscription on their
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
    // shortcuts (ids 5/6/7/8) are registered once per subscription on the
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
/// (ids 5/6/7/8), kept in its OWN slot so always-on shortcuts dispatch even when
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
/// registrations (ids 5/6/7/8) and the [`SHORTCUT_HANDLER_SLOT`] arm. Mirrors
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
        if debug_enabled() {
            eprintln!(
                "compme: carbon shortcut registered id={id} keycode={keycode} modifiers={modifiers}"
            );
        }
        self.hotkeys.push(hotkey_ref);
        Ok(())
    }
}

/// Install the always-on shortcut hotkeys (ids 5/6/7/8) ONCE for the
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
        shortcut_plan_minus_accept_collisions(shortcut_registration_plan(shortcuts), &accept_chords)
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

/// Snapshot the active always-on shortcut bindings.
pub fn effective_shortcut_bindings() -> ShortcutBindings {
    shortcut_bindings()
}

/// Restore a previously captured shortcut binding snapshot.
pub fn set_shortcut_bindings(bindings: ShortcutBindings) {
    *SHORTCUT_BINDINGS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = bindings;
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
        // Shortcut ids (5/6/7/8) read the PROCESS-LIFETIME shortcut slot so they
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
        assistant_field: false,
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

pub(crate) fn create_app_ax_element(pid: i32) -> Result<(AXUIElementRef, CFType), PlatformError> {
    let element = unsafe { AXUIElementCreateApplication(pid) };
    if element.is_null() {
        return Err(PlatformError::CannotComplete {
            reason: "AXUIElementCreateApplication returned null".into(),
        });
    }

    let owner = unsafe { CFType::wrap_under_create_rule(element as CFTypeRef) };
    Ok((element, owner))
}

pub(crate) unsafe fn copy_focused_ui_element(
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

pub(crate) fn choose_caret_observer_element(
    app_element: AXUIElementRef,
    focused_element: Option<AXUIElementRef>,
) -> AXUIElementRef {
    focused_element.unwrap_or(app_element)
}

fn capabilities_for_field(
    pid: i32,
    field: FieldHandle,
    secure_input_enabled: Arc<SecureInputProvider>,
) -> Result<Capabilities, PlatformError> {
    // TOCTOU re-check before any worker-thread AX read. The public
    // `capabilities` entry point checks secure input on the caller thread, but
    // secure input can turn on before this worker reaches AX.
    recheck_global_secure_input(&secure_input_enabled)?;

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

    let mut capabilities = editable_capabilities(
        &identity,
        value_settable,
        selected_range_settable,
        has_caret_rect,
        true,
    );
    let metadata = unsafe { read_sidebar_field_metadata(element, &identity) }?;
    capabilities.assistant_field = assistant_field_evidence(&metadata).is_some();
    Ok(capabilities)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SidebarFieldMetadata {
    identifier: Option<String>,
    description: Option<String>,
    title: Option<String>,
    placeholder: Option<String>,
    help: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AssistantMetadataSource {
    Identifier,
    Description,
    Title,
    Placeholder,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AssistantFieldEvidence {
    source: AssistantMetadataSource,
    marker: &'static str,
}

fn assistant_field_evidence(metadata: &SidebarFieldMetadata) -> Option<AssistantFieldEvidence> {
    const IDENTIFIER_MARKERS: [&str; 8] = [
        "chat",
        "copilot",
        "assistant",
        "composer",
        "interactive-input",
        "prompt-input",
        "cascade",
        "aichat",
    ];
    const LABEL_MARKERS: [&str; 10] = [
        "ask copilot",
        "ask ai",
        "ask anything",
        "chat input",
        "chat message",
        "ai assistant",
        "agent mode",
        "composer",
        "cascade",
        "send a message",
    ];

    if let Some(identifier) = metadata.identifier.as_deref() {
        let identifier = identifier.to_ascii_lowercase();
        if let Some(marker) = IDENTIFIER_MARKERS
            .iter()
            .find(|marker| identifier.contains(**marker))
        {
            return Some(AssistantFieldEvidence {
                source: AssistantMetadataSource::Identifier,
                marker,
            });
        }
    }

    for (source, value) in [
        (
            AssistantMetadataSource::Description,
            metadata.description.as_deref(),
        ),
        (AssistantMetadataSource::Title, metadata.title.as_deref()),
        (
            AssistantMetadataSource::Placeholder,
            metadata.placeholder.as_deref(),
        ),
        (AssistantMetadataSource::Help, metadata.help.as_deref()),
    ] {
        let Some(value) = value else {
            continue;
        };
        let value = value.to_ascii_lowercase();
        if let Some(marker) = LABEL_MARKERS.iter().find(|marker| value.contains(**marker)) {
            return Some(AssistantFieldEvidence { source, marker });
        }
    }
    None
}

unsafe fn read_sidebar_field_metadata(
    element: AXUIElementRef,
    identity: &AxElementIdentity,
) -> Result<SidebarFieldMetadata, PlatformError> {
    Ok(SidebarFieldMetadata {
        identifier: identity.identifier.clone(),
        description: sidebar_metadata_attribute(read_optional_ax_string_attribute(
            element,
            "AXDescription",
        ))?,
        title: sidebar_metadata_attribute(read_optional_ax_string_attribute(element, "AXTitle"))?,
        placeholder: sidebar_metadata_attribute(read_optional_ax_string_attribute(
            element,
            "AXPlaceholderValue",
        ))?,
        help: sidebar_metadata_attribute(read_optional_ax_string_attribute(element, "AXHelp"))?,
    })
}

fn sidebar_metadata_attribute(
    result: Result<Option<String>, PlatformError>,
) -> Result<Option<String>, PlatformError> {
    match result {
        // These labels are optional classification hints. A toolkit that
        // transiently rejects one must degrade to an unknown/non-assistant
        // field instead of making an otherwise usable field unsupported.
        Err(PlatformError::CannotComplete { .. }) | Err(PlatformError::UnsupportedField { .. }) => {
            Ok(None)
        }
        other => other,
    }
}

fn secure_input_recheck_result(enabled: bool) -> Result<(), PlatformError> {
    if enabled {
        Err(PlatformError::SecureInput {
            state: SecurityState::SecureInputEnabled,
        })
    } else {
        Ok(())
    }
}

fn recheck_global_secure_input(
    secure_input_enabled: &Arc<SecureInputProvider>,
) -> Result<(), PlatformError> {
    secure_input_recheck_result(secure_input_enabled.as_ref()())
}

fn read_context_for_field(
    pid: i32,
    field: FieldHandle,
    secure_input_enabled: Arc<SecureInputProvider>,
) -> Result<TextContext, PlatformError> {
    // TOCTOU re-check, mirroring the insert path's `recheck_secure_input`. The
    // dispatch-site guard in `read_context` samples global secure input once on
    // the calling thread; secure input can turn on before this worker reaches
    // AX. Re-checking here keeps the window as narrow as possible.
    recheck_global_secure_input(&secure_input_enabled)?;

    let (element, _owners) = copy_focused_or_app_element(pid)?;
    let identity = unsafe { resolve_ax_element_identity(element) }?;
    if !field_matches_identity(&field, &identity) {
        return Err(PlatformError::StaleField);
    }

    let value = unsafe { read_required_ax_string_attribute(element, kAXValueAttribute) }?;
    let selected_range = unsafe { read_required_ax_range_attribute(element) }?;
    Ok(text_context_from_value(field, value, selected_range))
}

fn caret_rect_for_field(
    pid: i32,
    field: FieldHandle,
    secure_input_enabled: Arc<SecureInputProvider>,
) -> Result<Option<ScreenRect>, PlatformError> {
    // TOCTOU re-check on the worker thread, mirroring `read_context_for_field`
    // and the insert path's `recheck_secure_input`.
    recheck_global_secure_input(&secure_input_enabled)?;

    let (element, _owners) = copy_focused_or_app_element(pid)?;
    let identity = unsafe { resolve_ax_element_identity(element) }?;
    if !field_matches_identity(&field, &identity) {
        return Err(PlatformError::StaleField);
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
/// caret geometry is available. Best effort — returns `None` if neither the
/// focused element nor its application exposes a focused window frame.
fn popup_anchor_for_field(
    pid: i32,
    field: FieldHandle,
    secure_input_enabled: Arc<SecureInputProvider>,
) -> Result<Option<ScreenRect>, PlatformError> {
    // TOCTOU re-check on the worker thread, uniform with the other four
    // `_for_field` workers. Lowest sensitivity of the set (window-chrome
    // `AXFrame` geometry, never field text/caret), but re-checked anyway so all
    // five `_for_field` workers share the fail-closed posture with no straggler.
    recheck_global_secure_input(&secure_input_enabled)?;

    let (element, _owners) = copy_focused_or_app_element(pid)?;
    let identity = unsafe { resolve_ax_element_identity(element) }?;
    if !field_matches_identity(&field, &identity) {
        return Err(PlatformError::StaleField);
    }

    unsafe {
        let element_window = copy_ax_element_attribute(element, AX_WINDOW_ATTRIBUTE)?;
        let (app_element, _app_owner) = create_app_ax_element(pid)?;
        let app_window = copy_ax_element_attribute(app_element, "AXFocusedWindow")?;
        let element_window_ref = element_window.as_ref().map(|(window, _owner)| *window);
        let app_window_ref = app_window.as_ref().map(|(window, _owner)| *window);
        first_readable_popup_anchor_rect(
            popup_anchor_window_sources(element_window_ref, app_window_ref),
            |window| read_ax_cgrect_attribute(window, AX_FRAME_ATTRIBUTE),
        )
    }
}

fn popup_anchor_window_sources(
    element_window: Option<AXUIElementRef>,
    app_focused_window: Option<AXUIElementRef>,
) -> impl Iterator<Item = AXUIElementRef> {
    element_window.into_iter().chain(app_focused_window)
}

fn first_readable_popup_anchor_rect<I, F>(
    windows: I,
    mut read_frame: F,
) -> Result<Option<ScreenRect>, PlatformError>
where
    I: IntoIterator<Item = AXUIElementRef>,
    F: FnMut(AXUIElementRef) -> Result<Option<ScreenRect>, PlatformError>,
{
    for window in windows {
        if let Some(rect) = read_frame(window)? {
            return Ok(Some(rect));
        }
    }
    Ok(None)
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
    secure_input_enabled: Arc<SecureInputProvider>,
) -> Result<MacosCaretDiagnostics, PlatformError> {
    // TOCTOU re-check on the worker thread, mirroring `caret_rect_for_field`.
    // Geometry-only (no plaintext), but kept uniform across all `_for_field`
    // worker fns so they share the fail-closed posture before any AX read.
    recheck_global_secure_input(&secure_input_enabled)?;

    let (element, _owners) = copy_focused_or_app_element(pid)?;
    let identity = unsafe { resolve_ax_element_identity(element) }?;
    if !field_matches_identity(&field, &identity) {
        return Err(PlatformError::StaleField);
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
    secure_input_enabled: Arc<SecureInputProvider>,
) -> Result<Option<ScreenRect>, PlatformError> {
    // TOCTOU re-check before AX identity/text reads, matching the other
    // `_for_field` workers. Grammar correction geometry must fail closed if
    // global Secure Input flips on after dispatch.
    recheck_global_secure_input(&secure_input_enabled)?;

    let (element, _owners) = copy_focused_or_app_element(pid)?;
    let identity = unsafe { resolve_ax_element_identity(element) }?;
    if !field_matches_identity(&field, &identity) {
        return Err(PlatformError::StaleField);
    }

    let value = unsafe { read_required_ax_string_attribute(element, kAXValueAttribute) }?;
    let selected_range = unsafe { read_required_ax_range_attribute(element) }?;
    let ctx = text_context_from_value(field, value, selected_range);
    let Some(range) = scalar_correction_range_to_utf16_range(
        &ctx.left,
        ctx.selected_text.as_deref(),
        &ctx.right,
        selected_range,
        range,
    ) else {
        return Err(PlatformError::UnsupportedField {
            reason: "correction range is not contiguous in the field".into(),
        });
    };
    unsafe { read_ax_bounds_for_range(element, range.location, range.length) }
}

/// Element-level AX access for the range-replacement write path, abstracted so
/// tests can drive `insert_replacing_range` without a live AX element — the
/// same role `ObserverBackend` plays for the observer path. Production uses
/// [`RawAxRangeTarget`]'s forwarders to the raw FFI helpers; tests inject a
/// recording fake and assert the exact attribute-set sequence.
trait AxRangeTarget {
    fn copy_focused_or_app_element(
        &self,
        pid: i32,
    ) -> Result<(AXUIElementRef, Vec<CFType>), PlatformError>;
    unsafe fn resolve_identity(
        &self,
        element: AXUIElementRef,
    ) -> Result<AxElementIdentity, PlatformError>;
    unsafe fn read_value(&self, element: AXUIElementRef) -> Result<String, PlatformError>;
    unsafe fn read_selected_range(&self, element: AXUIElementRef)
        -> Result<CFRange, PlatformError>;
    unsafe fn set_value(
        &self,
        element: AXUIElementRef,
        new_value: &str,
    ) -> Result<(), PlatformError>;
    /// Advisory caret set after a landed value write; failures stay non-fatal
    /// exactly as in [`set_caret_after_value_write`].
    unsafe fn set_caret_after_value_write(&self, element: AXUIElementRef, new_caret: usize);
}

struct RawAxRangeTarget;

impl AxRangeTarget for RawAxRangeTarget {
    fn copy_focused_or_app_element(
        &self,
        pid: i32,
    ) -> Result<(AXUIElementRef, Vec<CFType>), PlatformError> {
        copy_focused_or_app_element(pid)
    }

    unsafe fn resolve_identity(
        &self,
        element: AXUIElementRef,
    ) -> Result<AxElementIdentity, PlatformError> {
        resolve_ax_element_identity(element)
    }

    unsafe fn read_value(&self, element: AXUIElementRef) -> Result<String, PlatformError> {
        read_required_ax_string_attribute(element, kAXValueAttribute)
    }

    unsafe fn read_selected_range(
        &self,
        element: AXUIElementRef,
    ) -> Result<CFRange, PlatformError> {
        read_required_ax_range_attribute(element)
    }

    unsafe fn set_value(
        &self,
        element: AXUIElementRef,
        new_value: &str,
    ) -> Result<(), PlatformError> {
        set_required_ax_string_attribute(element, kAXValueAttribute, new_value)
    }

    unsafe fn set_caret_after_value_write(&self, element: AXUIElementRef, new_caret: usize) {
        set_caret_after_value_write(element, new_caret);
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_range_for_field(
    pid: i32,
    field: FieldHandle,
    expected_text: String,
    text: String,
    range: CorrectionRange,
    strategy: InsertStrategy,
    secure_input_enabled: Arc<SecureInputProvider>,
    target: &dyn AxRangeTarget,
) -> Result<AxSetApply, PlatformError> {
    // TOCTOU re-check before AX identity/text reads, matching insert/read/caret
    // workers. Grammar range replacement must not touch the focused AX element
    // after global Secure Input turns on.
    recheck_global_secure_input(&secure_input_enabled)?;

    let (element, _owners) = target.copy_focused_or_app_element(pid)?;
    let identity = unsafe { target.resolve_identity(element) }?;
    if !field_matches_identity(&field, &identity) {
        return Err(PlatformError::StaleField);
    }

    let value = unsafe { target.read_value(element) }?;
    let selected_range = unsafe { target.read_selected_range(element) }?;
    let ctx = text_context_from_value(field, value.clone(), selected_range);
    let Some(range) = scalar_correction_range_to_utf16_range(
        &ctx.left,
        ctx.selected_text.as_deref(),
        &ctx.right,
        selected_range,
        range,
    ) else {
        return Err(PlatformError::UnsupportedField {
            reason: "correction range is not contiguous in the field".into(),
        });
    };
    if !utf16_range_matches_expected(&value, range, &expected_text) {
        return Err(PlatformError::StaleField);
    }
    let (new_value, new_caret) = splice_text_at_utf16_range(&value, range, &text);
    unsafe {
        target.set_value(element, &new_value)?;
        target.set_caret_after_value_write(element, new_caret);
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
    let readback = unsafe { target.read_value(element) }.unwrap_or_else(|_| new_value.clone());
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

fn utf16_range_matches_expected(value: &str, selected_range: CFRange, expected: &str) -> bool {
    let utf16_len = value.encode_utf16().count();
    let start = selected_range.location.max(0) as usize;
    let length = selected_range.length.max(0) as usize;
    let Some(end) = start.checked_add(length) else {
        return false;
    };
    if end > utf16_len {
        return false;
    }

    let left_end = byte_index_for_utf16_units(value, start);
    let right_start = byte_index_for_utf16_units(value, end);
    value
        .get(left_end..right_start)
        .is_some_and(|current| current == expected)
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
    selected_text: Option<&str>,
    right: &str,
    selected_range: CFRange,
    range: CorrectionRange,
) -> Option<CFRange> {
    if range.start > range.end {
        return None;
    }
    let selected_text = selected_text.unwrap_or_default();
    let selection_start = selected_range.location.max(0) as usize;
    let selection_len = selected_range.length.max(0) as usize;
    if selection_start != left.encode_utf16().count()
        || selection_len != selected_text.encode_utf16().count()
    {
        return None;
    }

    let value = format!("{left}{selected_text}{right}");
    if range.end > value.chars().count() {
        return None;
    }
    let start_utf16 = utf16_units_for_scalar_prefix(&value, range.start)?;
    let end_utf16 = utf16_units_for_scalar_prefix(&value, range.end)?;

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
        assistant_field: false,
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
    let (left_end, left_scalars) = byte_index_and_scalar_count_for_utf16_units(&value, start);
    let right_start = byte_index_for_utf16_units(&value, end);

    TextContext {
        left: value[..left_end].to_string(),
        right: value[right_start..].to_string(),
        left_scalars,
        selection: (end > start).then_some(TextRange { start, end }),
        selected_text: (end > start).then(|| value[left_end..right_start].to_string()),
        caret: start,
        source: ContextSource::Accessibility,
        field_id: field,
        offset_encoding: OffsetEncoding::Utf16CodeUnits,
    }
}

fn byte_index_for_utf16_units(value: &str, target_units: usize) -> usize {
    byte_index_and_scalar_count_for_utf16_units(value, target_units).0
}

fn byte_index_and_scalar_count_for_utf16_units(value: &str, target_units: usize) -> (usize, usize) {
    if target_units == 0 {
        return (0, 0);
    }

    let mut units = 0usize;
    let mut scalars = 0usize;
    for (byte_index, ch) in value.char_indices() {
        units = units.saturating_add(ch.len_utf16());
        scalars += 1;
        if units >= target_units {
            return (byte_index + ch.len_utf8(), scalars);
        }
    }

    (value.len(), scalars)
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
    fields: HashMap<String, FieldHandle>,
    recency: VecDeque<String>,
}

impl CaretFieldTracker {
    fn new() -> Self {
        Self {
            factory: FocusTokenFactory::new(),
            fields: HashMap::new(),
            recency: VecDeque::new(),
        }
    }

    fn field_for_event(&mut self, fallback_pid: i32, identity: &AxElementIdentity) -> FieldHandle {
        let app = identity.app_id(fallback_pid);
        let pid = identity.pid(fallback_pid);
        let element_id = identity.field_element_id();
        let pid = pid.or_else(|| u32::try_from(fallback_pid).ok());
        let identity_key = identity
            .stable_field_key()
            .unwrap_or_else(|| format!("pid={}:element={element_id}", pid.unwrap_or_default()));
        if let Some(field) = self.fields.get(&identity_key).cloned() {
            self.recency.retain(|key| key != &identity_key);
            self.recency.push_back(identity_key);
            return field;
        }

        let field = self.factory.focused_field(app, pid, element_id);
        if self.fields.len() >= FIELD_IDENTITY_REGISTRY_CAPACITY {
            if let Some(oldest) = self.recency.pop_front() {
                self.fields.remove(&oldest);
            }
        }
        self.fields.insert(identity_key.clone(), field.clone());
        self.recency.push_back(identity_key);
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AxElementIdentity {
    pointer_id: String,
    owner_pid: Option<u32>,
    identifier: Option<String>,
    role: Option<String>,
    subrole: Option<String>,
    /// `CFHash` of the AX element: stable for the same underlying node across
    /// element refs. Chromium delivers a fresh ref per focus notification for
    /// identifier-less web fields, so pointer identity churns (live
    /// 2026-07-07: every post-focus read StaleFielded and the ghost never
    /// rendered); the hash is the stable substitute.
    element_hash: Option<u64>,
}

impl AxElementIdentity {
    pub(crate) fn pointer_only(pointer_id: impl Into<String>) -> Self {
        Self {
            pointer_id: pointer_id.into(),
            owner_pid: None,
            identifier: None,
            role: None,
            subrole: None,
            element_hash: None,
        }
    }

    fn with_element_hash(mut self, element_hash: Option<u64>) -> Self {
        self.element_hash = element_hash;
        self
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
            element_hash: None,
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
        if let Some(hash) = self.element_hash {
            parts.push(format!("hash={hash}"));
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

        let mut parts = vec![format!("pid={owner_pid}")];
        match (&self.identifier, self.element_hash) {
            // An explicit AXIdentifier is the strongest identity; the hash is
            // the fallback for identifier-less (e.g. Chromium web) fields.
            (Some(identifier), _) => {
                parts.push(format!("id={}", escape_identity_component(identifier)));
                if let Some(role) = &self.role {
                    parts.push(format!("role={}", escape_identity_component(role)));
                }
                if let Some(subrole) = &self.subrole {
                    parts.push(format!("subrole={}", escape_identity_component(subrole)));
                }
            }
            (None, Some(hash)) => {
                parts.push(format!("hash={hash}"));
                if let Some(role) = &self.role {
                    parts.push(format!("role={}", escape_identity_component(role)));
                }
                if let Some(subrole) = &self.subrole {
                    parts.push(format!("subrole={}", escape_identity_component(subrole)));
                }
            }
            (None, None) => return None,
        }

        Some(format!("ax:{}", parts.join("|")))
    }
}

fn escape_identity_component(value: &str) -> String {
    value.replace('\\', "\\\\").replace('|', "\\|")
}

pub(crate) unsafe fn resolve_ax_element_identity(
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
    // CFHash equality tracks the underlying AX node, not the ref pointer —
    // the stable identity for identifier-less fields (Chromium churns refs).
    let element_hash = Some(CFHash(element as CFTypeRef) as u64);

    Ok(
        AxElementIdentity::new(pointer_id, owner_pid, identifier, role, subrole)
            .with_element_hash(element_hash),
    )
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

    let (app_element, _app_owner) = create_app_ax_element(pid)?;
    unsafe {
        let Some((window, window_owner)) =
            copy_ax_element_attribute(app_element, "AXFocusedWindow")?
        else {
            return Ok(None);
        };
        let window = AxUrlNode {
            element: window,
            _owner: window_owner,
        };
        page_url_from_window_tree(
            window,
            PageUrlWalkLimits {
                max_depth: MAX_DEPTH,
                max_children: MAX_CHILDREN,
                max_nodes: MAX_NODES,
                max_walk: MAX_WALK,
            },
            |node| read_optional_ax_url_attribute(node.element, "AXDocument"),
            |node| read_optional_ax_string_attribute(node.element, "AXRole"),
            |node| read_optional_ax_url_attribute(node.element, "AXURL"),
            |node, cap| {
                copy_ax_children(node.element, cap).map(|children| {
                    children
                        .into_iter()
                        .map(|(element, owner)| AxUrlNode {
                            element,
                            _owner: owner,
                        })
                        .collect()
                })
            },
        )
    }
}

struct AxUrlNode {
    element: AXUIElementRef,
    _owner: CFType,
}

#[derive(Clone, Copy)]
struct PageUrlWalkLimits {
    max_depth: usize,
    max_children: usize,
    max_nodes: usize,
    max_walk: std::time::Duration,
}

fn page_url_from_window_tree<N>(
    window: N,
    limits: PageUrlWalkLimits,
    mut read_document: impl FnMut(&N) -> Result<Option<String>, PlatformError>,
    mut read_role: impl FnMut(&N) -> Result<Option<String>, PlatformError>,
    mut read_url: impl FnMut(&N) -> Result<Option<String>, PlatformError>,
    mut copy_children: impl FnMut(&N, usize) -> Result<Vec<N>, PlatformError>,
) -> Result<Option<String>, PlatformError> {
    let started = std::time::Instant::now();
    if let Ok(Some(doc)) = read_document(&window) {
        return Ok(Some(doc));
    }

    let mut queue = std::collections::VecDeque::new();
    queue.push_back((window, 0usize));
    let mut visited = 0usize;
    while let Some((node, depth)) = queue.pop_front() {
        visited += 1;
        if visited > limits.max_nodes || started.elapsed() > limits.max_walk {
            break;
        }
        if let Ok(Some(role)) = read_role(&node) {
            if role == "AXWebArea" {
                if let Ok(Some(url)) = read_url(&node) {
                    return Ok(Some(url));
                }
                // A web area without AXURL: keep walking (frames nest).
            }
        }
        if depth >= limits.max_depth {
            continue;
        }
        if let Ok(children) = copy_children(&node, limits.max_children) {
            for child in children {
                queue.push_back((child, depth + 1));
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

pub(crate) fn observer_caret_rect(
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

pub(crate) fn ax_element_id(element: AXUIElementRef) -> String {
    if element.is_null() {
        "ax:null".into()
    } else {
        format!("ax:0x{:x}", element as usize)
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
