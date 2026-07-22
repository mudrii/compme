//! Dedicated AX worker thread and its observer machinery.
//!
//! Every blocking Accessibility (AX) call runs on one dedicated worker thread
//! ([`AxWorker`]): jobs are dispatched synchronously over a channel, observer
//! resources are installed and removed on that thread, and the thread's run
//! loop is pumped between messages so `AXObserver` callbacks keep firing.
//! Observer notifications and the focused-element safety poll are resolved on
//! the worker, then handed to the [`CallbackDispatcher`] thread so subscriber
//! callbacks (and any panic in them) never touch the worker.

use std::any::Any;
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::Duration;

use accessibility_sys::{
    kAXErrorSuccess, kAXFocusedUIElementChangedNotification, kAXSelectedTextChangedNotification,
    AXObserverAddNotification, AXObserverCreate, AXObserverGetRunLoopSource, AXObserverRef,
    AXObserverRemoveNotification, AXUIElementCreateSystemWide, AXUIElementRef,
    AXUIElementSetAttributeValue, AXUIElementSetMessagingTimeout,
};
use core_foundation::base::{CFRelease, CFRetain, CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::runloop::{
    kCFRunLoopCommonModes, kCFRunLoopDefaultMode, CFRunLoop, CFRunLoopSource,
};
use core_foundation::string::{CFString, CFStringRef};
use platform::{AcceptCallback, PlatformError, ScreenRect, TapControl};

use crate::{
    ax_element_id, choose_caret_observer_element, copy_focused_ui_element, create_app_ax_element,
    map_ax_error, observer_caret_rect, resolve_ax_element_identity, AdapterObserverInstallerFn,
    AxElementIdentity, FrontmostPidProvider, ObserverInstallTarget, ObserverResource,
};

const AX_MESSAGING_TIMEOUT_SECONDS: f32 = 0.05;
const AX_WORKER_PUMP_INTERVAL: Duration = Duration::from_millis(5);
const AX_WORKER_RUN_LOOP_SLICE: Duration = Duration::from_millis(1);

const CARET_SAFETY_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Setting this attribute to true asks a Chromium/Electron application to build
/// its accessibility tree on demand, which is what exposes the
/// `AXSelectedTextMarkerRange` markers the web caret path depends on. WebKit and
/// AppKit ignore it; see `enable_manual_accessibility`.
const AX_MANUAL_ACCESSIBILITY_ATTRIBUTE: &str = "AXManualAccessibility";

type Job = Box<dyn FnOnce() -> Box<dyn Any + Send> + Send + 'static>;
pub(crate) type WorkerResource = Box<dyn Any + 'static>;
type ResourceInstaller =
    Box<dyn FnOnce() -> Result<WorkerResource, PlatformError> + Send + 'static>;
pub(crate) type ObserverDispatch = Arc<dyn Fn(ObserverEvent) + Send + Sync + 'static>;

pub(crate) enum CallbackMessage {
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

pub(crate) enum Message {
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
pub(crate) struct AxWorkerHandle {
    pub(crate) tx: mpsc::Sender<Message>,
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

struct SafetyPoller {
    stop_tx: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

pub(crate) struct ObserverBinding {
    pid: i32,
    _observer: ObserverResource,
    _poller: SafetyPoller,
}

pub(crate) struct DynamicObserverBinding {
    _rebinder: RebindPoller,
    _current: Arc<Mutex<Option<ObserverBinding>>>,
}

#[derive(Clone)]
pub(crate) struct ObserverBindingConfig {
    pub(crate) installer: Arc<AdapterObserverInstallerFn>,
    pub(crate) worker_tx: mpsc::Sender<Message>,
    pub(crate) target: ObserverInstallTarget,
    pub(crate) notifications: Vec<ObserverNotification>,
    pub(crate) poll_notification: ObserverNotification,
    pub(crate) dispatch: ObserverDispatch,
    pub(crate) callback_tx: mpsc::Sender<CallbackMessage>,
}

pub(crate) struct DynamicObserverBindingConfig {
    pub(crate) initial_pid: i32,
    pub(crate) frontmost_pid: Arc<FrontmostPidProvider>,
    pub(crate) current: Arc<Mutex<Option<ObserverBinding>>>,
    pub(crate) binding: ObserverBindingConfig,
    pub(crate) rebind_interval: Duration,
}

struct RebindPoller {
    stop_tx: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
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

    pub(crate) fn handle(&self) -> AxWorkerHandle {
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

    pub(crate) fn install_app_observer(
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

    pub(crate) fn install_focused_caret_observer(
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

pub(crate) fn start_dynamic_observer_binding(
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
    pub(crate) fn new() -> Result<Self, PlatformError> {
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

    pub(crate) fn sender(&self) -> mpsc::Sender<CallbackMessage> {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObserverNotification {
    FocusChanged,
    CaretChanged,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ObserverEvent {
    pub(crate) pid: i32,
    pub(crate) notification: ObserverNotification,
    pub(crate) identity: AxElementIdentity,
    pub(crate) rect: Option<ScreenRect>,
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
    use std::collections::VecDeque;

    use crate::tests::{observer_event, pointer_identity};

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
}
