//! StatusNotifierItem tray for Linux.
//!
//! The protocol is served through `ksni`'s blocking service, which implements
//! both StatusNotifierItem and DBusMenu over the existing pure-Rust zbus stack.
//! Starting the service is deliberately strict: a missing watcher or host is an
//! error, allowing the app's existing optional-tray path to keep running
//! headless without claiming an icon exists.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use ksni::blocking::TrayMethods as _;
use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
use platform::shell::TrayHandle;
use platform::PlatformError;
use shell_flags::{DisableArm, TrayFlags};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    ToggleEnabled,
    OpenAccessibilitySettings,
    Quit,
    DisableGlobal(DisableArm),
    Snooze,
    OpenSettingsWindow,
    CheckUpdates,
    VisitWebsite,
    ContactSupport,
    ToggleCollection,
    DisableApp(DisableArm),
}

pub fn apply_tray_action(flags: &TrayFlags, action: TrayAction) {
    match action {
        TrayAction::ToggleEnabled => {
            flags.toggle_enabled();
        }
        TrayAction::OpenAccessibilitySettings => {
            flags.open_settings.store(true, Ordering::Relaxed);
        }
        TrayAction::Quit => flags.quit.store(true, Ordering::Relaxed),
        TrayAction::DisableGlobal(arm) => {
            *flags
                .global_disable
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(arm);
        }
        TrayAction::Snooze => flags.snooze_requested.store(true, Ordering::Relaxed),
        TrayAction::OpenSettingsWindow => flags.open_settings_window.store(true, Ordering::Relaxed),
        TrayAction::CheckUpdates => flags.check_updates.store(true, Ordering::Relaxed),
        TrayAction::VisitWebsite => flags.visit_website.store(true, Ordering::Relaxed),
        TrayAction::ContactSupport => flags.contact_support.store(true, Ordering::Relaxed),
        TrayAction::ToggleCollection => flags.collection_toggle.store(true, Ordering::Relaxed),
        TrayAction::DisableApp(arm) => {
            *flags
                .app_disable
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(arm);
        }
    }
}

#[derive(Clone)]
struct TrayRenderState {
    title: String,
    status_line: String,
    stats_line: String,
    enabled: bool,
    needs_accessibility: bool,
}

struct CompmeTray {
    flags: TrayFlags,
    render: TrayRenderState,
}

/// The worker-facing portion of ksni. Keeping this tiny seam lets the
/// concurrency contract be tested without a session bus.
trait TrayService: Send + 'static {
    fn update(&self, render: TrayRenderState) -> bool;
    fn shutdown(&self);
}

impl TrayService for ksni::blocking::Handle<CompmeTray> {
    fn update(&self, render: TrayRenderState) -> bool {
        self.update(|tray| tray.render = render).is_some()
    }

    fn shutdown(&self) {
        let _shutdown_requested = self.shutdown();
    }
}

fn standard(label: impl Into<String>, action: TrayAction) -> ksni::MenuItem<CompmeTray> {
    StandardItem {
        label: label.into(),
        activate: Box::new(move |tray: &mut CompmeTray| {
            apply_tray_action(&tray.flags, action);
        }),
        ..Default::default()
    }
    .into()
}

fn disable_menu(label: &str, action: fn(DisableArm) -> TrayAction) -> ksni::MenuItem<CompmeTray> {
    SubMenu {
        label: label.into(),
        submenu: vec![
            standard("For 1 Hour", action(DisableArm::Hour)),
            standard("Until Relaunch", action(DisableArm::UntilRelaunch)),
            standard("Always", action(DisableArm::Always)),
        ],
        ..Default::default()
    }
    .into()
}

impl ksni::Tray for CompmeTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "compme".into()
    }

    fn title(&self) -> String {
        self.render.title.clone()
    }

    fn icon_name(&self) -> String {
        if self.render.enabled {
            "input-keyboard".into()
        } else {
            "input-keyboard-symbolic".into()
        }
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.render.title.clone(),
            description: self.render.status_line.clone(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let status = StandardItem {
            label: self.render.status_line.clone(),
            enabled: false,
            ..Default::default()
        }
        .into();
        let stats = StandardItem {
            label: self.render.stats_line.clone(),
            enabled: false,
            visible: !self.render.stats_line.is_empty(),
            ..Default::default()
        }
        .into();
        let enabled = CheckmarkItem {
            label: "Enable Completions".into(),
            checked: self.render.enabled,
            activate: Box::new(|tray: &mut Self| {
                apply_tray_action(&tray.flags, TrayAction::ToggleEnabled);
            }),
            ..Default::default()
        }
        .into();
        let accessibility = StandardItem {
            label: "Open Accessibility Settings".into(),
            visible: self.render.needs_accessibility,
            activate: Box::new(|tray: &mut Self| {
                apply_tray_action(&tray.flags, TrayAction::OpenAccessibilitySettings);
            }),
            ..Default::default()
        }
        .into();

        vec![
            status,
            stats,
            ksni::MenuItem::Separator,
            enabled,
            standard("Snooze", TrayAction::Snooze),
            disable_menu("Disable Completions Globally", TrayAction::DisableGlobal),
            standard(
                "Toggle Input Collection in Current App",
                TrayAction::ToggleCollection,
            ),
            disable_menu("Disable Completions in Current App", TrayAction::DisableApp),
            ksni::MenuItem::Separator,
            standard("Settings…", TrayAction::OpenSettingsWindow),
            accessibility,
            standard("Check for Updates…", TrayAction::CheckUpdates),
            standard("Visit Website", TrayAction::VisitWebsite),
            standard("Contact Support", TrayAction::ContactSupport),
            ksni::MenuItem::Separator,
            standard("Quit", TrayAction::Quit),
        ]
    }
}

pub struct LinuxTray {
    pending: Arc<Mutex<TrayRenderState>>,
    refresh_tx: mpsc::SyncSender<()>,
    stopping: Arc<AtomicBool>,
    stopped: Mutex<mpsc::Receiver<()>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl LinuxTray {
    const START_TIMEOUT: Duration = Duration::from_secs(2);
    const STOP_TIMEOUT: Duration = Duration::from_secs(2);

    pub fn new(flags: TrayFlags) -> Result<Self, PlatformError> {
        let render = TrayRenderState {
            title: "compme".into(),
            status_line: "Starting…".into(),
            stats_line: String::new(),
            enabled: flags.enabled.load(Ordering::Relaxed),
            needs_accessibility: false,
        };
        let tray = CompmeTray {
            flags,
            render: render.clone(),
        };
        Self::start_worker(render, Self::START_TIMEOUT, move || {
            tray.spawn().map_err(|err| err.to_string())
        })
    }

    fn start_worker<S, F>(
        render: TrayRenderState,
        start_timeout: Duration,
        start: F,
    ) -> Result<Self, PlatformError>
    where
        S: TrayService,
        F: FnOnce() -> Result<S, String> + Send + 'static,
    {
        let pending = Arc::new(Mutex::new(render));
        let stopping = Arc::new(AtomicBool::new(false));
        let (refresh_tx, refresh_rx) = mpsc::sync_channel::<()>(1);
        let (started_tx, started_rx) = mpsc::sync_channel::<Result<(), String>>(1);
        let (stopped_tx, stopped) = mpsc::channel::<()>();
        let worker_pending = Arc::clone(&pending);
        let worker_stopping = Arc::clone(&stopping);
        let worker = std::thread::Builder::new()
            .name("compme-sni-tray".into())
            .spawn(move || {
                let _stopped = stopped_tx;
                let service = match start() {
                    Ok(service) => {
                        let _ = started_tx.send(Ok(()));
                        service
                    }
                    Err(err) => {
                        let _ = started_tx.send(Err(err.to_string()));
                        return;
                    }
                };
                while !worker_stopping.load(Ordering::Acquire) {
                    if refresh_rx.recv().is_err() {
                        break;
                    }
                    if worker_stopping.load(Ordering::Acquire) {
                        break;
                    }
                    let render = worker_pending
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone();
                    if !service.update(render) {
                        break;
                    }
                }
                // Sending is enough. Waiting here would let a broken bus keep
                // this worker alive after the app's bounded Drop returned.
                service.shutdown();
            })
            .map_err(|err| PlatformError::CannotComplete {
                reason: format!("platform_linux StatusNotifierItem worker: {err}"),
            })?;

        match started_rx.recv_timeout(start_timeout) {
            Ok(Ok(())) => Ok(Self {
                pending,
                refresh_tx,
                stopping,
                stopped: Mutex::new(stopped),
                worker: Mutex::new(Some(worker)),
            }),
            Ok(Err(err)) => {
                let all_exited = matches!(
                    stopped.recv_timeout(Self::STOP_TIMEOUT),
                    Err(mpsc::RecvTimeoutError::Disconnected)
                );
                if all_exited {
                    let _ = worker.join();
                }
                Err(PlatformError::CannotComplete {
                    reason: format!("platform_linux StatusNotifierItem unavailable: {err}"),
                })
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                stopping.store(true, Ordering::Release);
                let _ = refresh_tx.try_send(());
                // Do not join: the whole point of this branch is that startup
                // crossed its bound. The worker owns no grab or user data.
                drop(worker);
                Err(PlatformError::Timeout)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // The worker exited without publishing a startup result. It
                // owns no resources once both channel senders are gone.
                let all_exited = matches!(
                    stopped.recv_timeout(Self::STOP_TIMEOUT),
                    Err(mpsc::RecvTimeoutError::Disconnected)
                );
                if all_exited {
                    let _ = worker.join();
                }
                Err(PlatformError::CannotComplete {
                    reason: "platform_linux StatusNotifierItem worker exited during startup".into(),
                })
            }
        }
    }

    fn queue_refresh(&self) -> Result<(), PlatformError> {
        if self.stopping.load(Ordering::Acquire) {
            return Err(PlatformError::CannotComplete {
                reason: "platform_linux StatusNotifierItem service stopped".into(),
            });
        }
        match self.refresh_tx.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Full(())) => Ok(()),
            Err(mpsc::TrySendError::Disconnected(())) => Err(PlatformError::CannotComplete {
                reason: "platform_linux StatusNotifierItem service stopped".into(),
            }),
        }
    }
}

impl TrayHandle for LinuxTray {
    fn set_status(
        &self,
        title: &str,
        status_line: &str,
        enabled: bool,
        needs_accessibility: bool,
    ) -> Result<(), PlatformError> {
        {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            pending.title = title.to_string();
            pending.status_line = status_line.to_string();
            pending.enabled = enabled;
            pending.needs_accessibility = needs_accessibility;
        }
        self.queue_refresh()
    }

    fn set_stats_line(&self, line: &str) -> Result<(), PlatformError> {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stats_line = line.to_string();
        self.queue_refresh()
    }
}

impl Drop for LinuxTray {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let _ = self.refresh_tx.try_send(());
        let all_exited = matches!(
            self.stopped
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .recv_timeout(Self::STOP_TIMEOUT),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if all_exited {
            if let Some(worker) = worker {
                let _ = worker.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ksni::Tray as _;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    fn flags() -> TrayFlags {
        TrayFlags {
            enabled: Arc::new(AtomicBool::new(true)),
            quit: Arc::new(AtomicBool::new(false)),
            open_settings: Arc::new(AtomicBool::new(false)),
            snooze_requested: Arc::new(AtomicBool::new(false)),
            global_disable: Arc::new(Mutex::new(None)),
            open_settings_window: Arc::new(AtomicBool::new(false)),
            check_updates: Arc::new(AtomicBool::new(false)),
            visit_website: Arc::new(AtomicBool::new(false)),
            contact_support: Arc::new(AtomicBool::new(false)),
            collection_toggle: Arc::new(AtomicBool::new(false)),
            app_disable: Arc::new(Mutex::new(None)),
        }
    }

    #[test]
    fn actions_only_mutate_their_shared_flag() {
        let flags = flags();
        assert!(apply_and_read_enabled(&flags));
        apply_tray_action(&flags, TrayAction::ToggleEnabled);
        assert!(!flags.enabled.load(Ordering::Relaxed));
        apply_tray_action(&flags, TrayAction::DisableGlobal(DisableArm::Hour));
        assert_eq!(
            *flags.global_disable.lock().unwrap(),
            Some(DisableArm::Hour)
        );
        apply_tray_action(&flags, TrayAction::DisableApp(DisableArm::Always));
        assert_eq!(*flags.app_disable.lock().unwrap(), Some(DisableArm::Always));
        apply_tray_action(&flags, TrayAction::Quit);
        assert!(flags.quit.load(Ordering::Relaxed));
        for (action, flag) in [
            (TrayAction::Snooze, &flags.snooze_requested),
            (TrayAction::OpenSettingsWindow, &flags.open_settings_window),
            (TrayAction::CheckUpdates, &flags.check_updates),
            (TrayAction::VisitWebsite, &flags.visit_website),
            (TrayAction::ContactSupport, &flags.contact_support),
            (TrayAction::ToggleCollection, &flags.collection_toggle),
            (TrayAction::OpenAccessibilitySettings, &flags.open_settings),
        ] {
            apply_tray_action(&flags, action);
            assert!(flag.load(Ordering::Relaxed), "{action:?}");
        }
    }

    fn apply_and_read_enabled(flags: &TrayFlags) -> bool {
        flags.enabled.load(Ordering::Relaxed)
    }

    #[test]
    fn menu_keeps_dynamic_status_and_accessibility_visibility() {
        let tray = CompmeTray {
            flags: flags(),
            render: TrayRenderState {
                title: "Ready".into(),
                status_line: "Model loaded".into(),
                stats_line: "12 accepted".into(),
                enabled: true,
                needs_accessibility: false,
            },
        };
        let menu = tray.menu();
        assert!(menu.len() >= 10);
        assert_eq!(tray.title(), "Ready");
        assert_eq!(tray.tool_tip().description, "Model loaded");
        let ksni::MenuItem::Standard(accessibility) = &menu[10] else {
            panic!("accessibility row must be a standard item");
        };
        assert_eq!(accessibility.label, "Open Accessibility Settings");
        assert!(!accessibility.visible);
    }

    struct BlockingService {
        updates: mpsc::Sender<TrayRenderState>,
        releases: mpsc::Receiver<()>,
    }

    impl TrayService for BlockingService {
        fn update(&self, render: TrayRenderState) -> bool {
            self.updates.send(render).is_ok()
                && self.releases.recv_timeout(Duration::from_secs(1)).is_ok()
        }

        fn shutdown(&self) {}
    }

    fn initial_render() -> TrayRenderState {
        TrayRenderState {
            title: "compme".into(),
            status_line: "Starting…".into(),
            stats_line: String::new(),
            enabled: true,
            needs_accessibility: false,
        }
    }

    #[test]
    fn startup_timeout_is_bounded_when_the_service_stalls() {
        let (release_tx, release_rx) = mpsc::channel();
        let started = Instant::now();
        let result = LinuxTray::start_worker(
            initial_render(),
            Duration::from_millis(25),
            move || -> Result<BlockingService, String> {
                let _ = release_rx.recv();
                Err("released after timeout".into())
            },
        );
        assert!(matches!(result, Err(PlatformError::Timeout)));
        assert!(started.elapsed() < Duration::from_secs(1));
        release_tx.send(()).expect("release stalled fake service");
    }

    #[test]
    fn updates_are_nonblocking_and_coalesce_while_the_service_stalls() {
        let (updates_tx, updates_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let tray = LinuxTray::start_worker(initial_render(), Duration::from_secs(1), move || {
            Ok(BlockingService {
                updates: updates_tx,
                releases: release_rx,
            })
        })
        .expect("start fake tray worker");

        tray.set_stats_line("first").expect("queue first update");
        let first = updates_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker entered first update");
        assert_eq!(first.stats_line, "first");

        let started = Instant::now();
        for index in 0..100 {
            tray.set_stats_line(&format!("latest-{index}"))
                .expect("coalesce update");
        }
        assert!(started.elapsed() < Duration::from_millis(250));

        release_tx.send(()).expect("release first update");
        let coalesced = updates_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker entered coalesced update");
        assert_eq!(coalesced.stats_line, "latest-99");
        release_tx.send(()).expect("release coalesced update");
        drop(tray);
    }
}
