//! Unit tests for `lib`, split out of the module file
//! (2026-07-25 audit, F16) so the production surface is visible in `wc -l`.
//! Same module path as before (`mod tests` inside the parent module), so
//! `use super::*` and every test name are unchanged.

use super::*;
use objc2::{define_class, msg_send, AnyThread, DefinedClass};
use objc2_app_kit::NSPasteboardItemDataProvider;
use objc2_foundation::{NSObject, NSObjectProtocol};

#[test]
fn sidebar_field_classifier_requires_positive_assistant_metadata() {
    for (metadata, expected) in [
        (
            SidebarFieldMetadata {
                identifier: Some("workbench.panel.chat.view.input".into()),
                ..SidebarFieldMetadata::default()
            },
            AssistantFieldEvidence {
                source: AssistantMetadataSource::Identifier,
                marker: "chat",
            },
        ),
        (
            SidebarFieldMetadata {
                placeholder: Some("Ask Copilot anything".into()),
                ..SidebarFieldMetadata::default()
            },
            AssistantFieldEvidence {
                source: AssistantMetadataSource::Placeholder,
                marker: "ask copilot",
            },
        ),
        (
            SidebarFieldMetadata {
                description: Some("Cascade chat input".into()),
                ..SidebarFieldMetadata::default()
            },
            AssistantFieldEvidence {
                source: AssistantMetadataSource::Description,
                marker: "chat input",
            },
        ),
    ] {
        assert_eq!(assistant_field_evidence(&metadata), Some(expected));
    }
}

#[test]
fn sidebar_field_classifier_keeps_editor_and_unknown_fields_closed() {
    for metadata in [
        SidebarFieldMetadata::default(),
        SidebarFieldMetadata {
            identifier: Some("workbench.editors.textResourceEditor".into()),
            description: Some("Editor content".into()),
            ..SidebarFieldMetadata::default()
        },
        SidebarFieldMetadata {
            placeholder: Some("Search files by name".into()),
            ..SidebarFieldMetadata::default()
        },
    ] {
        assert!(
            assistant_field_evidence(&metadata).is_none(),
            "{metadata:?}"
        );
    }
}

#[test]
fn optional_sidebar_metadata_failure_does_not_block_an_editable_field() {
    assert_eq!(
        sidebar_metadata_attribute(Err(PlatformError::CannotComplete {
            reason: "optional AXHelp read failed".into(),
        })),
        Ok(None)
    );
    assert_eq!(
        sidebar_metadata_attribute(Err(PlatformError::StaleField)),
        Err(PlatformError::StaleField),
        "a genuinely stale field must still fail closed"
    );
}

#[test]
fn cg_image_opaque_encodes_as_vision_expects() {
    // Pin the Vision argument encoding: objc2's debug-build verification
    // panics the OCR thread when initWithCGImage: receives '^v' (a bare
    // void pointer) instead of '^{CGImage=}' (live 2026-07-07).
    assert_eq!(
        <*const CGImageOpaque as objc2::encode::Encode>::ENCODING.to_string(),
        "^{CGImage=}"
    );
}

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

pub(crate) fn observer_event(
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

pub(crate) fn pointer_identity(element_id: &str) -> AxElementIdentity {
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
    ax_range_target: Arc<dyn AxRangeTarget + Send + Sync>,
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
            ax_range_target: Arc::new(RawAxRangeTarget),
        }
    }
}

static SHORTCUT_BINDINGS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct ShortcutBindingsGuard {
    previous: ShortcutBindings,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl ShortcutBindingsGuard {
    fn set(
        force_activate: Option<&str>,
        toggle_app: Option<&str>,
        toggle_global: Option<&str>,
        grammar_check: Option<&str>,
    ) -> Self {
        let lock = SHORTCUT_BINDINGS_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = shortcut_bindings();
        set_shortcut_bindings_from_config(force_activate, toggle_app, toggle_global, grammar_check);
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for ShortcutBindingsGuard {
    fn drop(&mut self) {
        *SHORTCUT_BINDINGS
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = self.previous;
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

fn test_adapter_with_secure_input_flipping_on_worker() -> (MacosPlatformAdapter, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider_calls = Arc::clone(&calls);
    let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
    config.secure_input_enabled =
        Arc::new(move || provider_calls.fetch_add(1, Ordering::SeqCst) > 0);
    (test_adapter_with_hooks(config), calls)
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
        ax_range_target,
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
            ax_range_target,
        },
    )
}

fn test_adapter_with_dynamic_frontmost(
    frontmost_pid: Arc<Mutex<Option<i32>>>,
    installs: Arc<Mutex<Vec<FakeObserverInstall>>>,
    teardowns: Arc<Mutex<Vec<i32>>>,
) -> MacosPlatformAdapter {
    test_adapter_with_dynamic_frontmost_and_install_hook(
        frontmost_pid,
        installs,
        teardowns,
        Arc::new(|_| {}),
    )
}

fn test_adapter_with_dynamic_frontmost_and_install_hook(
    frontmost_pid: Arc<Mutex<Option<i32>>>,
    installs: Arc<Mutex<Vec<FakeObserverInstall>>>,
    teardowns: Arc<Mutex<Vec<i32>>>,
    after_install: Arc<dyn Fn(i32) + Send + Sync>,
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
        after_install(pid);
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
            ax_range_target: Arc::new(RawAxRangeTarget),
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

fn wait_for_accept_tap_count(installs: &Arc<Mutex<Vec<FakeAcceptTapInstall>>>, expected: usize) {
    let deadline = SystemTime::now() + WAIT_DEADLINE;
    while SystemTime::now() < deadline {
        if installs.lock().unwrap().len() >= expected {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(installs.lock().unwrap().len(), expected);
}

fn count_drop_events(events: &Arc<Mutex<Vec<String>>>) -> usize {
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e.as_str() == "drop")
        .count()
}

fn wait_for_drop_events(events: &Arc<Mutex<Vec<String>>>, expected: usize) {
    let deadline = SystemTime::now() + WAIT_DEADLINE;
    while SystemTime::now() < deadline {
        if count_drop_events(events) >= expected {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(count_drop_events(events), expected);
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
fn field_identity_registry_evicts_the_oldest_entry_at_its_bound() {
    let mut tracker = CaretFieldTracker::new();
    let first = resolved_identity("ax:first", 42, Some("editor-first"));
    let original = tracker.field_for_event(42, &first);

    for index in 0..FIELD_IDENTITY_REGISTRY_CAPACITY {
        let identity =
            resolved_identity(&format!("ax:{index}"), 42, Some(&format!("editor-{index}")));
        tracker.field_for_event(42, &identity);
    }

    let reminted = tracker.field_for_event(42, &first);
    assert_ne!(reminted.generation, original.generation);
    assert!(tracker.fields.len() <= FIELD_IDENTITY_REGISTRY_CAPACITY);
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
fn caret_field_tracker_mints_new_field_for_anonymous_same_role_pointer_change() {
    // Same pid + same role is not a stable field identity. Anonymous AX
    // fields in one app commonly share role/subrole, so a pointer change
    // must mint a new generation rather than reusing a pending insert target.
    let mut tracker = CaretFieldTracker::new();
    let first = AxElementIdentity::new("ax:0x111", Some(42), None, Some("AXTextArea".into()), None);
    let second =
        AxElementIdentity::new("ax:0x222", Some(42), None, Some("AXTextArea".into()), None);

    let first_field = tracker.field_for_event(42, &first);
    let second_field = tracker.field_for_event(42, &second);

    assert_ne!(second_field, first_field);
    assert_ne!(second_field.generation, first_field.generation);
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
fn stable_field_key_builds_key_when_identifier_present() {
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

    assert_eq!(role_only.stable_field_key(), None);
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
fn stable_field_key_uses_element_hash_for_anonymous_fields() {
    // Chromium web fields carry no AXIdentifier and deliver a fresh
    // element ref per focus notification (live 2026-07-07: every read
    // StaleFielded and the ghost never rendered). CFHash of the element is
    // stable for the same underlying AX node, so it substitutes for the
    // missing identifier.
    let identity =
        AxElementIdentity::new("ax:0x111", Some(42), None, Some("AXTextArea".into()), None)
            .with_element_hash(Some(777));

    assert_eq!(
        identity.stable_field_key(),
        Some("ax:pid=42|hash=777|role=AXTextArea".into())
    );
    // An identifier still wins over the hash when both exist.
    let named = AxElementIdentity::new("ax:0x111", Some(42), Some("editor".into()), None, None)
        .with_element_hash(Some(777));
    assert_eq!(named.stable_field_key(), Some("ax:pid=42|id=editor".into()));
}

#[test]
fn field_matches_identity_accepts_same_element_hash_across_pointer_churn() {
    let old_identity =
        AxElementIdentity::new("ax:0x111", Some(42), None, Some("AXTextArea".into()), None)
            .with_element_hash(Some(777));
    let new_identity =
        AxElementIdentity::new("ax:0x222", Some(42), None, Some("AXTextArea".into()), None)
            .with_element_hash(Some(777));
    let old_field = FieldHandle {
        app: "pid:42".into(),
        pid: Some(42),
        element_id: old_identity.field_element_id(),
        generation: 1,
    };

    assert!(field_matches_identity(&old_field, &new_identity));

    // A DIFFERENT hash is a different field — the anonymous wrong-field
    // guard stays intact.
    let other_identity =
        AxElementIdentity::new("ax:0x333", Some(42), None, Some("AXTextArea".into()), None)
            .with_element_hash(Some(888));
    assert!(!field_matches_identity(&old_field, &other_identity));
}

#[test]
fn caret_field_tracker_reuses_field_across_pointer_churn_with_same_hash() {
    let mut tracker = CaretFieldTracker::new();
    let first = AxElementIdentity::new("ax:0x111", Some(42), None, Some("AXTextArea".into()), None)
        .with_element_hash(Some(777));
    let second =
        AxElementIdentity::new("ax:0x222", Some(42), None, Some("AXTextArea".into()), None)
            .with_element_hash(Some(777));

    let first_field = tracker.field_for_event(42, &first);
    let second_field = tracker.field_for_event(42, &second);

    assert_eq!(second_field, first_field);
    assert_eq!(second_field.generation, first_field.generation);
}

#[test]
fn field_matches_identity_rejects_anonymous_same_role_pointer_change() {
    let old_identity =
        AxElementIdentity::new("ax:0x111", Some(42), None, Some("AXTextArea".into()), None);
    let new_identity =
        AxElementIdentity::new("ax:0x222", Some(42), None, Some("AXTextArea".into()), None);
    let old_field = FieldHandle {
        app: "pid:42".into(),
        pid: Some(42),
        element_id: old_identity.field_element_id(),
        generation: 1,
    };

    assert!(!field_matches_identity(&old_field, &new_identity));
}

#[derive(Default)]
struct TestAxUrlNode {
    document: Option<String>,
    role: Option<String>,
    url: Option<String>,
    children: Vec<usize>,
    role_error: bool,
    url_error: bool,
    children_error: bool,
}

fn page_url_from_test_nodes(
    nodes: &[TestAxUrlNode],
    root: usize,
) -> Result<Option<String>, PlatformError> {
    page_url_from_window_tree(
        root,
        PageUrlWalkLimits {
            max_depth: 8,
            max_children: 64,
            max_nodes: 256,
            max_walk: std::time::Duration::from_secs(1),
        },
        |idx| Ok(nodes[*idx].document.clone()),
        |idx| {
            if nodes[*idx].role_error {
                Err(PlatformError::Timeout)
            } else {
                Ok(nodes[*idx].role.clone())
            }
        },
        |idx| {
            if nodes[*idx].url_error {
                Err(PlatformError::Timeout)
            } else {
                Ok(nodes[*idx].url.clone())
            }
        },
        |idx, cap| {
            if nodes[*idx].children_error {
                Err(PlatformError::Timeout)
            } else {
                Ok(nodes[*idx].children.iter().copied().take(cap).collect())
            }
        },
    )
}

#[test]
fn page_url_walk_prefers_focused_window_document() {
    let nodes = vec![
        TestAxUrlNode {
            document: Some("https://docs.example.test/path".into()),
            children: vec![1],
            ..Default::default()
        },
        TestAxUrlNode {
            role: Some("AXWebArea".into()),
            url: Some("https://webarea.example.test/".into()),
            ..Default::default()
        },
    ];

    assert_eq!(
        page_url_from_test_nodes(&nodes, 0).unwrap().as_deref(),
        Some("https://docs.example.test/path")
    );
}

#[test]
fn page_url_walk_finds_nested_web_area_url() {
    let nodes = vec![
        TestAxUrlNode {
            children: vec![1],
            ..Default::default()
        },
        TestAxUrlNode {
            role: Some("AXGroup".into()),
            children: vec![2],
            ..Default::default()
        },
        TestAxUrlNode {
            role: Some("AXWebArea".into()),
            url: Some("https://nested.example.test/".into()),
            ..Default::default()
        },
    ];

    assert_eq!(
        page_url_from_test_nodes(&nodes, 0).unwrap().as_deref(),
        Some("https://nested.example.test/")
    );
}

#[test]
fn page_url_walk_skips_broken_nodes_and_keeps_searching() {
    let nodes = vec![
        TestAxUrlNode {
            children: vec![1, 2, 3],
            ..Default::default()
        },
        TestAxUrlNode {
            role: Some("AXWebArea".into()),
            url_error: true,
            ..Default::default()
        },
        TestAxUrlNode {
            role_error: true,
            children_error: true,
            ..Default::default()
        },
        TestAxUrlNode {
            role: Some("AXWebArea".into()),
            url: Some("https://healthy.example.test/".into()),
            ..Default::default()
        },
    ];

    assert_eq!(
        page_url_from_test_nodes(&nodes, 0).unwrap().as_deref(),
        Some("https://healthy.example.test/")
    );
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
    let identity = AxElementIdentity::new("ax:0x123", Some(42), Some("editor".into()), None, None);

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
fn capabilities_worker_secure_input_recheck_is_fail_closed() {
    assert_eq!(
        secure_input_recheck_result(true),
        Err(PlatformError::SecureInput {
            state: SecurityState::SecureInputEnabled,
        })
    );
    assert_eq!(secure_input_recheck_result(false), Ok(()));
}

#[test]
fn field_workers_fail_closed_when_secure_input_flips_before_ax() {
    let field = FieldHandle {
        app: "pid:42".into(),
        pid: Some(42),
        element_id: pointer_identity("ax:0x123").field_element_id(),
        generation: 1,
    };

    type FieldWorkerProbe = fn(&MacosPlatformAdapter, &FieldHandle) -> Result<(), PlatformError>;
    let probes: [(&str, FieldWorkerProbe); 7] = [
        ("capabilities", |adapter, field| {
            adapter.capabilities(field).map(|_| ())
        }),
        ("read_context", |adapter, field| {
            adapter.read_context(field).map(|_| ())
        }),
        ("caret_rect", |adapter, field| {
            adapter.caret_rect(field).map(|_| ())
        }),
        ("popup_anchor", |adapter, field| {
            adapter.popup_anchor(field).map(|_| ())
        }),
        ("caret_diagnostics", |adapter, field| {
            adapter.caret_diagnostics(field).map(|_| ())
        }),
        ("text_range_rect", |adapter, field| {
            adapter
                .text_range_rect(field, CorrectionRange { start: 0, end: 1 })
                .map(|_| ())
        }),
        ("insert_replacing_range", |adapter, field| {
            adapter
                .insert_replacing_range(
                    field,
                    "a",
                    "b",
                    CorrectionRange { start: 0, end: 1 },
                    InsertStrategy::AxSet,
                )
                .map(|_| ())
        }),
    ];

    for (name, probe) in probes {
        let (adapter, secure_input_calls) = test_adapter_with_secure_input_flipping_on_worker();

        assert_eq!(
            probe(&adapter, &field),
            Err(PlatformError::SecureInput {
                state: SecurityState::SecureInputEnabled,
            }),
            "{name} must fail closed when secure input flips on after dispatch",
        );
        assert_eq!(
            secure_input_calls.load(Ordering::SeqCst),
            2,
            "{name} should check once at dispatch and once on the worker"
        );
    }
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
fn popup_anchor_window_sources_prefer_element_then_app_focused_window() {
    let element = 0x11usize as AXUIElementRef;
    let app = 0x22usize as AXUIElementRef;

    assert_eq!(
        popup_anchor_window_sources(Some(element), Some(app)).collect::<Vec<_>>(),
        vec![element, app]
    );
    assert_eq!(
        popup_anchor_window_sources(None, Some(app)).collect::<Vec<_>>(),
        vec![app]
    );
    assert_eq!(
        popup_anchor_window_sources(Some(element), None).collect::<Vec<_>>(),
        vec![element]
    );
    assert!(popup_anchor_window_sources(None, None)
        .collect::<Vec<_>>()
        .is_empty());
}

#[test]
fn popup_anchor_rect_falls_back_from_frameless_element_window_to_app_window() {
    let element = 0x11usize as AXUIElementRef;
    let app = 0x22usize as AXUIElementRef;
    let app_rect = ScreenRect {
        x: 10.0,
        y: 20.0,
        w: 300.0,
        h: 40.0,
    };

    let rect = first_readable_popup_anchor_rect([element, app], |window| {
        if window == element {
            Ok(None)
        } else {
            Ok(Some(app_rect))
        }
    })
    .expect("fallback succeeds");
    assert_eq!(rect, Some(app_rect));

    assert_eq!(
        first_readable_popup_anchor_rect([element, app], |_| Ok(None)).unwrap(),
        None
    );

    let err = first_readable_popup_anchor_rect([element], |_| Err(PlatformError::Timeout));
    assert_eq!(err, Err(PlatformError::Timeout));
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
            "teh",
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

#[test]
fn insert_replacing_range_secure_input_wins_even_for_non_axset_strategy() {
    let adapter = test_adapter_with_secure_input(true);
    let field = FieldHandle {
        app: "pid:42".into(),
        pid: Some(42),
        element_id: pointer_identity("ax:0x123").field_element_id(),
        generation: 1,
    };

    assert_eq!(
        adapter.insert_replacing_range(
            &field,
            "teh",
            "the",
            CorrectionRange { start: 0, end: 3 },
            InsertStrategy::SyntheticKeys,
        ),
        Err(PlatformError::SecureInput {
            state: SecurityState::SecureInputEnabled,
        })
    );
}

/// Recording fake for the AX range-replacement target seam: models one
/// focused text element (value + selected range + resolved identity) and
/// logs every attribute SET in order as "attribute=payload" strings — the
/// `FakeObserverBackend` log style — so a test asserts the exact
/// `AXUIElementSetAttributeValue` sequence byte-for-byte, Unicode payload
/// included. Reads are not logged (the mutation sequence is the contract
/// under test); `focused_element_copies` counts how often a dispatch
/// actually reached for an AX element, so reject-before-any-AX-call cases
/// can assert zero AX traffic.
struct FakeAxRangeTarget {
    identity: AxElementIdentity,
    value: Arc<Mutex<String>>,
    selected_range: CFRange,
    set_log: Arc<Mutex<Vec<String>>>,
    focused_element_copies: Arc<AtomicUsize>,
}

impl FakeAxRangeTarget {
    fn new(
        identity: AxElementIdentity,
        value: &str,
        selected_range: CFRange,
        set_log: Arc<Mutex<Vec<String>>>,
        focused_element_copies: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            identity,
            value: Arc::new(Mutex::new(value.to_string())),
            selected_range,
            set_log,
            focused_element_copies,
        }
    }
}

impl AxRangeTarget for FakeAxRangeTarget {
    fn copy_focused_or_app_element(
        &self,
        _pid: i32,
    ) -> Result<(AXUIElementRef, Vec<CFType>), PlatformError> {
        self.focused_element_copies.fetch_add(1, Ordering::SeqCst);
        // The element ref must be backed by a real CF object: the returned
        // owner releases it on drop, mirroring `create_app_ax_element`'s
        // create-rule wrap. A retained CFString serves as the opaque token;
        // the fake never dereferences it.
        let token = CFString::new("fake-ax-element");
        let element = token.as_concrete_TypeRef() as AXUIElementRef;
        Ok((element, vec![token.as_CFType()]))
    }

    unsafe fn resolve_identity(
        &self,
        _element: AXUIElementRef,
    ) -> Result<AxElementIdentity, PlatformError> {
        Ok(self.identity.clone())
    }

    unsafe fn read_value(&self, _element: AXUIElementRef) -> Result<String, PlatformError> {
        Ok(self.value.lock().unwrap().clone())
    }

    unsafe fn read_selected_range(
        &self,
        _element: AXUIElementRef,
    ) -> Result<CFRange, PlatformError> {
        Ok(self.selected_range)
    }

    unsafe fn set_value(
        &self,
        _element: AXUIElementRef,
        new_value: &str,
    ) -> Result<(), PlatformError> {
        self.set_log
            .lock()
            .unwrap()
            .push(format!("set:AXValue={new_value}"));
        // Model the landed write so the post-write readback classifies
        // Applied (a stale readback would read as the iTerm2 silent no-op).
        *self.value.lock().unwrap() = new_value.to_string();
        Ok(())
    }

    unsafe fn set_caret_after_value_write(&self, _element: AXUIElementRef, new_caret: usize) {
        // The production path wraps `new_caret` as CFRange{location,0};
        // logging the length component too keeps a non-collapsed-range
        // regression visible.
        self.set_log
            .lock()
            .unwrap()
            .push(format!("set:AXSelectedTextRange={new_caret},0"));
    }
}

#[test]
fn insert_replacing_range_applies_value_then_caret_in_order() {
    struct AppliedCase {
        name: &'static str,
        field_value: &'static str,
        caret_utf16: isize,
        range: CorrectionRange,
        expected_text: &'static str,
        insert_text: &'static str,
        want_value: &'static str,
        want_caret: isize,
    }

    // Both rows pin the same contract at different caret positions: ONE
    // AXValue write carrying the fully-spliced field text, then ONE
    // AXSelectedTextRange write placing the caret after the inserted text.
    let cases = [
        AppliedCase {
            // Happy path: suffix caret, and the payload mixes a non-ASCII
            // BMP scalar with an astral emoji — the scalar-based
            // CorrectionRange must land on the right UTF-16 span ("teh" is
            // units 8..11, not scalars 7..10) and the byte-for-byte value
            // write must carry the Unicode through untouched.
            name: "suffix caret with unicode payload",
            field_value: "cofé 😺 teh",
            caret_utf16: 11, // c o f é ' ' 😺(2 units) ' ' t e h
            range: CorrectionRange { start: 7, end: 10 }, // scalar span of "teh"
            expected_text: "teh",
            insert_text: "the",
            want_value: "cofé 😺 the",
            want_caret: 11,
        },
        AppliedCase {
            // Mid-field range replacement: the caret sits PAST the replaced
            // span, so the post-insert caret lands mid-field (9, after
            // "the quick") — it must NOT jump to the end of the field.
            name: "mid-field range with caret past the span",
            field_value: "the quik brown fox jumps",
            caret_utf16: 15, // caret after "the quik brown "
            range: CorrectionRange { start: 4, end: 8 }, // scalar span of "quik"
            expected_text: "quik",
            insert_text: "quick",
            want_value: "the quick brown fox jumps",
            want_caret: 9,
        },
    ];

    for case in cases {
        let set_log = Arc::new(Mutex::new(Vec::new()));
        let copies = Arc::new(AtomicUsize::new(0));
        let identity = resolved_identity("ax:0x123", 42, Some("note"));
        let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
        config.ax_range_target = Arc::new(FakeAxRangeTarget::new(
            identity.clone(),
            case.field_value,
            CFRange {
                location: case.caret_utf16,
                length: 0,
            },
            Arc::clone(&set_log),
            copies,
        ));
        let adapter = test_adapter_with_hooks(config);
        let field = FieldHandle {
            app: "pid:42".into(),
            pid: Some(42),
            element_id: identity.field_element_id(),
            generation: 1,
        };

        let result = adapter.insert_replacing_range(
            &field,
            case.expected_text,
            case.insert_text,
            case.range,
            InsertStrategy::AxSet,
        );

        // Invariant: an applied replacement reports the INSERTED TEXT's
        // extent with the AxSet strategy — the accept path counts on this
        // to stay honest about what landed.
        assert_eq!(
            result,
            Ok(Inserted {
                bytes: case.insert_text.len(),
                chars: case.insert_text.chars().count(),
                strategy: InsertStrategy::AxSet,
            }),
            "{}: applied replacement must report the inserted text extent",
            case.name,
        );
        // Invariant: the AXUIElementSetAttributeValue sequence IS the
        // contract — the value write (full field text with the replacement
        // spliced in, byte-for-byte including Unicode) must land BEFORE
        // the selected-range write, and the caret payload must be the
        // post-insert caret in UTF-16 units (splice start + inserted
        // units). Swapping the two writes or miscomputing the offset
        // corrupts the field/caret on live apps.
        assert_eq!(
            set_log.lock().unwrap().as_slice(),
            [
                format!("set:AXValue={}", case.want_value),
                format!("set:AXSelectedTextRange={},0", case.want_caret),
            ]
            .as_slice(),
            "{}: value write then caret write, exact payloads in order",
            case.name,
        );
    }
}

#[test]
fn insert_replacing_range_refuses_stale_field_without_any_ax_write() {
    let set_log = Arc::new(Mutex::new(Vec::new()));
    let copies = Arc::new(AtomicUsize::new(0));
    let mut config = TestAdapterConfig::new(Some(99), Arc::new(Mutex::new(Vec::new())), None);
    config.ax_range_target = Arc::new(FakeAxRangeTarget::new(
        // Focus moved after the field handle was captured: frontmost is now
        // pid 99 and the focused element resolves a pid-99 identity, while
        // the handle still names the pid-42 field. With no pid on the
        // handle, the write would target the NEW frontmost app via the
        // fallback — exactly the moved-frontmost scenario.
        resolved_identity("ax:0x999", 99, Some("other")),
        "teh",
        CFRange {
            location: 3,
            length: 0,
        },
        Arc::clone(&set_log),
        copies,
    ));
    let adapter = test_adapter_with_hooks(config);
    let field = FieldHandle {
        app: "pid:42".into(),
        pid: None,
        element_id: resolved_identity("ax:0x123", 42, Some("note")).field_element_id(),
        generation: 1,
    };

    // Invariant: the element-targeted (AxSet) stale guard — identity match,
    // the analog of the global strategies' frontmost-pid check — must
    // surface an error instead of writing into whatever app now holds
    // focus.
    assert_eq!(
        adapter.insert_replacing_range(
            &field,
            "teh",
            "the",
            CorrectionRange { start: 0, end: 3 },
            InsertStrategy::AxSet,
        ),
        Err(PlatformError::StaleField),
    );
    // Invariant: the stale check precedes EVERY attribute-set call; the
    // mutation log must stay empty even though the identity read ran. A
    // single AXValue write slipping through corrupts the focused field of
    // an unrelated app.
    assert!(set_log.lock().unwrap().is_empty());
}

#[test]
fn insert_replacing_range_rejects_non_atomic_strategy_before_any_ax_call() {
    let set_log = Arc::new(Mutex::new(Vec::new()));
    let copies = Arc::new(AtomicUsize::new(0));
    let identity = resolved_identity("ax:0x123", 42, Some("note"));
    let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
    config.ax_range_target = Arc::new(FakeAxRangeTarget::new(
        identity.clone(),
        "teh",
        CFRange {
            location: 3,
            length: 0,
        },
        Arc::clone(&set_log),
        Arc::clone(&copies),
    ));
    let adapter = test_adapter_with_hooks(config);
    let field = FieldHandle {
        app: "pid:42".into(),
        pid: Some(42),
        element_id: identity.field_element_id(),
        generation: 1,
    };

    // Invariant: a field whose negotiated capability lacks atomic range
    // replace (any strategy but AxSet — here Clipboard) is refused with an
    // UnsupportedField-class error. (The SyntheticKeys twin is
    // insert_replacing_range_refuses_non_axset_without_posting_text.)
    assert_eq!(
        adapter.insert_replacing_range(
            &field,
            "teh",
            "the",
            CorrectionRange { start: 0, end: 3 },
            InsertStrategy::Clipboard,
        ),
        Err(PlatformError::UnsupportedField {
            reason: "range replacement requires AxSet".into(),
        }),
    );
    // Invariant: the rejection fires BEFORE the worker touches any AX
    // element — the range write is only safe as one atomic value swap, so
    // a partial path (copy the element, read it, then fail) must never
    // start.
    assert_eq!(
        copies.load(Ordering::SeqCst),
        0,
        "no AX element may be touched for a non-atomic strategy",
    );
    assert!(set_log.lock().unwrap().is_empty());
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

    let snapshot = snapshot_pasteboard(&pasteboard).unwrap();
    pasteboard.clearContents();
    assert!(
        pasteboard.setString_forType(&NSString::from_str("replacement"), pasteboard_string_type(),)
    );

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
fn snapshot_rejects_item_that_advertises_a_type_but_yields_no_data() {
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
        matches!(snapshot, Err(PlatformError::CannotComplete { .. })),
        "an incomplete snapshot must abort before the pasteboard is changed"
    );
    assert!(
        provided_count.load(Ordering::SeqCst) >= 1,
        "the data provider must have been asked for its (absent) data"
    );
}

#[test]
fn snapshot_rejects_partially_materialized_multi_format_item() {
    let provider_type = NSString::from_str("com.compme.test.empty");
    let provided_count = Arc::new(AtomicUsize::new(0));
    let provider = TestNoopPasteboardProvider::new(Arc::clone(&provided_count));
    let item = NSPasteboardItem::new();
    assert!(item.setString_forType(
        &NSString::from_str("materialized text"),
        pasteboard_string_type(),
    ));
    let provider_ref: &ProtocolObject<dyn NSPasteboardItemDataProvider> =
        ProtocolObject::from_ref(&*provider);
    assert!(item.setDataProvider_forTypes(provider_ref, &NSArray::from_slice(&[&*provider_type]),));

    let snapshot = snapshot_pasteboard_items(&NSArray::from_slice(&[&*item]));

    assert!(matches!(
        snapshot,
        Err(PlatformError::CannotComplete { .. })
    ));
    assert!(provided_count.load(Ordering::SeqCst) >= 1);
}

#[test]
fn restore_failure_preserves_the_current_pasteboard() {
    // Force item materialization to fail before the pasteboard is cleared.
    // The user's safest current content must survive intact.
    let pasteboard = NSPasteboard::pasteboardWithUniqueName();
    pasteboard.clearContents();
    assert!(pasteboard.setString_forType(&NSString::from_str("stale"), pasteboard_string_type(),));

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

    assert_eq!(
        pasteboard
            .stringForType(pasteboard_string_type())
            .map(|value| value.to_string()),
        Some("stale".into()),
        "an invalid snapshot must not clear the safest current content"
    );
}

#[test]
fn post_clear_write_failure_retries_the_complete_multiformat_snapshot() {
    let pasteboard = NSPasteboard::pasteboardWithUniqueName();
    let custom_type = NSString::from_str("com.compme.test.bytes");
    pasteboard.clearContents();
    let external = NSPasteboardItem::new();
    assert!(external.setString_forType(&NSString::from_str("external"), pasteboard_string_type(),));
    assert!(external.setData_forType(&NSData::with_bytes(&[1, 2, 3]), &custom_type));
    let external = ProtocolObject::<dyn NSPasteboardWriting>::from_retained(external);
    assert!(pasteboard.writeObjects(&NSArray::from_slice(&[&*external])));
    let snapshot = snapshot_pasteboard(&pasteboard).unwrap();

    pasteboard.clearContents();
    assert!(
        pasteboard.setString_forType(&NSString::from_str("completion"), pasteboard_string_type(),)
    );
    let attempts = AtomicUsize::new(0);

    let outcome = restore_pasteboard_with_writer(&pasteboard, &snapshot, |pasteboard, items| {
        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            assert!(pasteboard
                .pasteboardItems()
                .map(|items| items.is_empty())
                .unwrap_or(true));
            return false;
        }
        write_pasteboard_items(pasteboard, items)
    });

    assert_eq!(outcome, PasteboardRestoreOutcome::Restored);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    let item = pasteboard.pasteboardItems().unwrap().objectAtIndex(0);
    assert_eq!(
        item.stringForType(pasteboard_string_type())
            .map(|value| value.to_string()),
        Some("external".into())
    );
    assert_eq!(
        item.dataForType(&custom_type).map(|data| data.to_vec()),
        Some(vec![1, 2, 3])
    );
}

#[test]
fn repeated_restore_failure_preserves_the_original_snapshot_text() {
    let pasteboard = NSPasteboard::pasteboardWithUniqueName();
    pasteboard.clearContents();
    assert!(
        pasteboard.setString_forType(&NSString::from_str("completion"), pasteboard_string_type(),)
    );
    let snapshot = PasteboardSnapshot {
        items: vec![PasteboardItemSnapshot {
            types: vec![PasteboardTypeSnapshot {
                type_name: pasteboard_string_type().to_string(),
                data: vec![1, 2, 3],
            }],
        }],
        fallback_string: Some("original user text".into()),
    };
    let attempts = AtomicUsize::new(0);

    let outcome = restore_pasteboard_with_writer(&pasteboard, &snapshot, |_, _| {
        attempts.fetch_add(1, Ordering::SeqCst);
        false
    });

    assert_eq!(outcome, PasteboardRestoreOutcome::FailedPreserved);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(
        pasteboard
            .stringForType(pasteboard_string_type())
            .map(|value| value.to_string()),
        Some("original user text".into())
    );
}

#[test]
fn clipboard_restore_coordinator_keeps_earliest_snapshot_for_back_to_back_inserts() {
    let coordinator = ClipboardRestoreCoordinator::default();
    let external = PasteboardSnapshot {
        items: Vec::new(),
        fallback_string: Some("external".into()),
    };
    let first_completion = PasteboardSnapshot {
        items: Vec::new(),
        fallback_string: Some("first completion".into()),
    };

    let first = coordinator.snapshot_for_insert(external.clone());
    let first_epoch = coordinator.record_insert(first, 11);
    let second = coordinator.snapshot_for_insert(first_completion);
    let second_epoch = coordinator.record_insert(second, 12);

    assert_eq!(
        coordinator.take_if_current_epoch_and_change_count(first_epoch, 12),
        None,
        "the first insert's stale timer must leave the newer deadline pending"
    );
    let pending = coordinator
        .take_if_current_epoch_and_change_count(second_epoch, 12)
        .unwrap();
    assert_eq!(pending, external);
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
        items: snapshot_pasteboard_items(&NSArray::from_slice(&[&*item])).unwrap(),
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
    assert!(
        pasteboard.setString_forType(&NSString::from_str("replacement"), pasteboard_string_type(),)
    );
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
    assert!(
        pasteboard.setString_forType(&NSString::from_str("previous"), pasteboard_string_type(),)
    );
    let snapshot = snapshot_pasteboard(&pasteboard).unwrap();

    pasteboard.clearContents();
    assert!(
        pasteboard.setString_forType(&NSString::from_str("completion"), pasteboard_string_type(),)
    );
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
    assert!(
        pasteboard.setString_forType(&NSString::from_str("previous"), pasteboard_string_type(),)
    );
    let snapshot = snapshot_pasteboard(&pasteboard).unwrap();

    pasteboard.clearContents();
    assert!(
        pasteboard.setString_forType(&NSString::from_str("completion"), pasteboard_string_type(),)
    );
    let completion_change_count = pasteboard.changeCount();
    pasteboard.clearContents();
    assert!(
        pasteboard.setString_forType(&NSString::from_str("external"), pasteboard_string_type(),)
    );

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
fn overlay_label_frame_floors_a_tiny_box_at_one_point() {
    // A degenerate box smaller than the 4pt insets must floor at 1pt, not
    // go negative (a negative NSRect width/height is undefined AppKit
    // territory).
    let label = overlay_label_frame(OverlayFrame {
        x: 0.0,
        y: 0.0,
        w: 2.0,
        h: 2.0,
    });
    assert_eq!(
        label,
        OverlayFrame {
            x: 2.0,
            y: 2.0,
            w: 1.0,
            h: 1.0,
        }
    );
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

fn shortcut_tap_event(action: ShortcutAction) -> AcceptTapEvent {
    AcceptTapEvent {
        event_type: CGEventType::KeyDown,
        keycode: 0,
        source_user_data: 0,
        option_down: false,
        binding: None,
        shortcut: Some(action),
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
        AcceptKeymap::from_accept_keys_with_mods(Some(KEYCODE_ESCAPE), None, CARBON_SHIFT_KEY, 0)
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
    // Finding F: accept keys (ids 1-4) and shortcuts (5/6/7/8) now register on
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
        (CARBON_HOTKEY_GRAMMAR_CHECK, KEYCODE_ESCAPE, 0),
    ];
    assert!(shortcut_plan_minus_accept_collisions(all_collide, &accept_chords).is_empty());

    // No chord collides → the plan survives verbatim, in its original order
    // (a filter that reordered or dropped a free chord would change this).
    let none_collide = vec![
        (CARBON_HOTKEY_FORCE_ACTIVATE, 96, 0),
        (CARBON_HOTKEY_TOGGLE_APP, 50, CARBON_SHIFT_KEY),
        (CARBON_HOTKEY_TOGGLE_GLOBAL, KEYCODE_TAB, CARBON_CONTROL_KEY),
        (
            CARBON_HOTKEY_GRAMMAR_CHECK,
            KEYCODE_ESCAPE,
            CARBON_CONTROL_KEY,
        ),
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
        shortcut_registration_plan(b),
        vec![
            (CARBON_HOTKEY_FORCE_ACTIVATE, 96, 0),
            (CARBON_HOTKEY_TOGGLE_GLOBAL, 96, CARBON_SHIFT_KEY),
            (CARBON_HOTKEY_GRAMMAR_CHECK, 96, CARBON_CONTROL_KEY),
        ]
    );
    // Every planned id round-trips back to an action.
    for (id, _, _) in shortcut_registration_plan(b) {
        assert!(shortcut_action_for_hotkey_id(id).is_some());
    }
}

#[test]
fn set_shortcut_bindings_from_config_drops_a_colliding_set_whole() {
    let _guard = ShortcutBindingsGuard::set(None, None, None, None);
    // A distinct set is stored verbatim and returned for the caller to log.
    let ok = set_shortcut_bindings_from_config(Some("96"), None, Some("shift+96"), Some("ctrl+96"));
    assert_eq!(ok.force_activate, Some((96, 0)));
    assert_eq!(ok.toggle_global, Some((96, CARBON_SHIFT_KEY)));
    assert_eq!(ok.grammar_check, Some((96, CARBON_CONTROL_KEY)));
    assert_eq!(shortcut_bindings(), ok);

    // A set where two shortcuts share one chord is unregisterable, so the
    // whole set is dropped to the default (no partial registration).
    let dropped = set_shortcut_bindings_from_config(Some("ctrl+50"), Some("ctrl+50"), None, None);
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
    // Duplicated modifier is idempotent (|= not XOR/+=): the bit is set
    // once, not toggled off or overflowed.
    assert_eq!(
        parse_accept_key("shift+shift+96"),
        Some((96, CARBON_SHIFT_KEY))
    );
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
fn accept_key_modifier_glyphs_emit_in_fixed_hig_order() {
    // Shortcuts-pane label twin of format_accept_key: ⌃⌥⇧⌘ in HIG order
    // regardless of mask bit order, one glyph per set bit, empty for a
    // bare key. A reordered or dropped glyph would ship a wrong settings
    // label undetected by the word-form tests above.
    let all = CARBON_CMD_KEY | CARBON_SHIFT_KEY | CARBON_OPTION_KEY | CARBON_CONTROL_KEY;
    assert_eq!(
        accept_key_modifier_glyphs(all),
        "\u{2303}\u{2325}\u{21e7}\u{2318}"
    );
    assert_eq!(accept_key_modifier_glyphs(CARBON_CMD_KEY), "\u{2318}");
    assert_eq!(accept_key_modifier_glyphs(0), "");
}

#[test]
fn accept_consumer_kind_routes_correction_to_its_own_tap() {
    // The accept-tap arming path picks the tap flavor from the pending
    // action: only Correction gets the CorrectionConsumer tap; every other
    // action (and no action) arms the plain Consumer.
    assert_eq!(
        accept_consumer_kind_for_action(Some(AcceptAction::Correction)),
        AcceptTapKind::CorrectionConsumer
    );
    for action in [Some(AcceptAction::Full), Some(AcceptAction::Word), None] {
        assert_eq!(
            accept_consumer_kind_for_action(action),
            AcceptTapKind::Consumer,
            "{action:?}"
        );
    }
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

    set_accept_keymap_from_config_with_mods(
        Some((35, CARBON_SHIFT_KEY)),
        None,
        Some((96, CARBON_OPTION_KEY)),
    )
    .unwrap();
    assert_eq!(
        effective_accept_keys_with_mods_and_grammar(),
        (
            (35, CARBON_SHIFT_KEY),
            (KEYCODE_GRAVE, 0),
            Some((96, CARBON_OPTION_KEY))
        )
    );
    set_accept_keymap(AcceptKeymap::default());
    assert_eq!(effective_accept_keys(), (48, 50));
}

#[test]
fn effective_accept_keys_with_mods_and_grammar_includes_configured_grammar_accept() {
    set_accept_keymap_from_config_with_mods(
        Some((35, CARBON_SHIFT_KEY)),
        None,
        Some((96, CARBON_OPTION_KEY)),
    )
    .unwrap();

    assert_eq!(
        effective_accept_keys_with_mods_and_grammar(),
        (
            (35, CARBON_SHIFT_KEY),
            (KEYCODE_GRAVE, 0),
            Some((96, CARBON_OPTION_KEY))
        )
    );

    set_accept_keymap(AcceptKeymap::default());
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
fn set_tab_hotkey_suppressed_removes_literal_tab_from_worker_registration() {
    set_tab_hotkey_suppressed(false);
    let unsuppressed = accept_keymap().arm_bindings_for_action(
        AcceptAction::Full,
        TAB_HOTKEY_SUPPRESSED.load(Ordering::Relaxed),
    );
    assert!(unsuppressed
        .iter()
        .any(|&(_, code, mods)| code == KEYCODE_TAB && mods == 0));

    set_tab_hotkey_suppressed(true);
    let suppressed = accept_keymap().arm_bindings_for_action(
        AcceptAction::Full,
        TAB_HOTKEY_SUPPRESSED.load(Ordering::Relaxed),
    );
    assert!(suppressed
        .iter()
        .all(|&(_, code, mods)| !(code == KEYCODE_TAB && mods == 0)));

    set_tab_hotkey_suppressed(false);
}

#[test]
fn arm_bindings_for_action_are_mode_specific() {
    let map = AcceptKeymap::from_accept_keys_with_mods_and_grammar(None, None, Some(96), 0, 0, 0)
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
fn accept_keymap_rejects_keycode_above_carbon_width_without_mutating_live_map() {
    struct Restore(AcceptKeymap);
    impl Drop for Restore {
        fn drop(&mut self) {
            set_accept_keymap(self.0);
        }
    }

    let previous = accept_keymap();
    let _restore = Restore(previous);
    let too_large = i64::from(u32::MAX) + 1;

    assert_eq!(
        set_accept_keymap_from_config_with_mods(Some((too_large, 0)), None, None),
        Err(KeymapError::InvalidKeycode(too_large))
    );
    assert_eq!(accept_keymap(), previous);
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
    AcceptTapController::hide_suggestion_after(Arc::clone(&controller), Duration::ZERO).unwrap();

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
fn subscribe_accept_shortcut_resource_dispatches_configured_grammar_check() {
    let accept_tap_installs = Arc::new(Mutex::new(Vec::new()));
    let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
    config.accept_tap_installs = Arc::clone(&accept_tap_installs);
    let adapter = test_adapter_with_hooks(config);
    let _guard = ShortcutBindingsGuard::set(None, None, None, Some("ctrl+96"));
    assert_eq!(
        shortcut_bindings().grammar_check,
        Some((96, CARBON_CONTROL_KEY))
    );
    let (action_tx, action_rx) = mpsc::channel();

    let _subscription = adapter
        .subscribe_accept(Arc::new(move |action| {
            action_tx.send(action).expect("action send");
        }))
        .expect("subscribe accept");
    wait_for_accept_tap_count(&accept_tap_installs, 2);
    assert_eq!(
        accept_tap_installs.lock().unwrap()[1].kind,
        AcceptTapKind::Shortcut
    );

    let shortcut_handler = Arc::clone(&accept_tap_installs.lock().unwrap()[1].handler);
    assert_eq!(
        shortcut_handler(shortcut_tap_event(ShortcutAction::GrammarCheck)),
        AcceptTapDecision::Shortcut(ShortcutAction::GrammarCheck)
    );
    assert_eq!(
        action_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("grammar shortcut action"),
        TapControl::Shortcut(ShortcutAction::GrammarCheck)
    );
}

#[test]
fn subscribe_accept_shortcut_handler_stops_after_subscription_drop() {
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
    wait_for_accept_tap_count(&accept_tap_installs, 2);
    let shortcut_handler = Arc::clone(&accept_tap_installs.lock().unwrap()[1].handler);

    drop(subscription);

    assert_eq!(
        shortcut_handler(shortcut_tap_event(ShortcutAction::GrammarCheck)),
        AcceptTapDecision::Keep
    );
    assert!(
        action_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "dropped subscriptions must not dispatch shortcut callbacks"
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
    let accept_tap_events = Arc::new(Mutex::new(Vec::new()));
    let mut config = TestAdapterConfig::new(Some(42), Arc::new(Mutex::new(Vec::new())), None);
    config.accept_tap_installs = Arc::clone(&accept_tap_installs);
    config.accept_tap_events = Arc::clone(&accept_tap_events);
    let adapter = test_adapter_with_hooks(config);

    let subscription = adapter
        .subscribe_accept(Arc::new(|_| {}))
        .expect("subscribe accept");
    subscription
        .set_suggestion_visible(true)
        .expect("activate consumer");
    wait_for_accept_tap_count(&accept_tap_installs, 3);

    let drops_before = count_drop_events(&accept_tap_events);
    subscription
        .hide_suggestion_after(Duration::from_millis(10))
        .expect("schedule delayed hide");
    // Wait for the hide to actually fire (the consumer-tap drop) instead
    // of a fixed sleep: on an oversubscribed CI runner the 10 ms sleeper
    // thread can be scheduled later than any fixed interval, making the
    // reactivate below a visible->visible no-op and stranding the install
    // count at 3 (live CI flake 2026-07-08).
    wait_for_drop_events(&accept_tap_events, drops_before + 1);
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
    let event = CGEvent::new_keyboard_event(source, KeyCode::SPACE, true).expect("keyboard event");

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
    assert_eq!(context.left_scalars, 4);
    assert_eq!(context.right, " there");
    assert_eq!(context.selection, None);
    assert_eq!(context.selected_text, None);
    assert_eq!(context.caret, 5);
    assert_eq!(context.field_id, field);
    assert_eq!(context.source, ContextSource::Accessibility);
    assert_eq!(context.offset_encoding, OffsetEncoding::Utf16CodeUnits);
}

#[test]
fn text_context_carries_selected_text_separately_from_left_and_right() {
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
    assert_eq!(context.left_scalars, 6);
    assert_eq!(context.right, "");
    assert_eq!(context.selection, Some(TextRange { start: 6, end: 11 }));
    assert_eq!(context.selected_text.as_deref(), Some("world"));
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
    assert_eq!(context.left_scalars, 3);
    assert_eq!(context.right, "");
    assert_eq!(context.selection, None);
    assert_eq!(context.selected_text, None);
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
        None,
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
fn correction_range_expected_text_guard_rejects_changed_live_text() {
    let range = CFRange {
        location: 2,
        length: 3,
    };

    assert!(
        utf16_range_matches_expected("😀teh later", range, "teh"),
        "the live field substring at the mutation range still matches the original typo"
    );
    assert!(
        !utf16_range_matches_expected("😀cat later", range, "teh"),
        "the same numeric range in changed text must fail closed before mutation"
    );
    assert!(
        !utf16_range_matches_expected(
            "😀teh later",
            CFRange {
                location: 2,
                length: 99
            },
            "teh"
        ),
        "out-of-range coordinates must not be treated as a match"
    );
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
        None,
        " teh",
        collapsed,
        CorrectionRange { start: 6, end: 9 },
    )
    .expect("range");
    assert_eq!(range.location, 6);
    assert_eq!(range.length, 3);

    let empty = scalar_correction_range_to_utf16_range(
        "hello",
        None,
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
        None,
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
        None,
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
fn scalar_correction_range_to_utf16_range_restores_the_exact_selection_gap() {
    let selected = CFRange {
        location: 3,
        length: 2,
    };
    let before = scalar_correction_range_to_utf16_range(
        "abc",
        Some("XY"),
        "def",
        selected,
        CorrectionRange { start: 0, end: 3 },
    )
    .expect("before selection");
    assert_eq!(before.location, 0);
    assert_eq!(before.length, 3);

    let after = scalar_correction_range_to_utf16_range(
        "abc",
        Some("XY"),
        "def",
        selected,
        CorrectionRange { start: 5, end: 8 },
    )
    .expect("after selection");
    assert_eq!(after.location, 5);
    assert_eq!(after.length, 3);

    let exact_selection = scalar_correction_range_to_utf16_range(
        "abc",
        Some("XY"),
        "def",
        selected,
        CorrectionRange { start: 3, end: 5 },
    )
    .expect("selected text");
    assert_eq!(exact_selection.location, 3);
    assert_eq!(exact_selection.length, 2);

    assert!(
        scalar_correction_range_to_utf16_range(
            "abc",
            None,
            "def",
            selected,
            CorrectionRange { start: 3, end: 5 },
        )
        .is_none(),
        "a live selection without its exact text must fail closed"
    );
}

#[test]
fn scalar_correction_range_splice_allows_empty_text_to_delete_range() {
    let range = scalar_correction_range_to_utf16_range(
        "I saw ",
        None,
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

    let rect =
        resolve_caret_rect_with_marker_first(5, || Ok(Some(container)), |_, _| Ok(Some(native)))
            .expect("resolve caret");

    assert_eq!(rect, Some(native));
}

#[test]
fn resolve_caret_rect_with_marker_first_propagates_marker_errors() {
    let rect =
        resolve_caret_rect_with_marker_first(5, || Err(PlatformError::StaleField), |_, _| Ok(None));

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
fn focus_and_caret_callbacks_share_one_field_identity() {
    let installs = Arc::new(Mutex::new(Vec::new()));
    let mut config = TestAdapterConfig::new(Some(42), Arc::clone(&installs), None);
    let now = Arc::new(AtomicU64::new(0));
    let now_for_hook = Arc::clone(&now);
    config.now_ms = Arc::new(move || now_for_hook.fetch_add(30, Ordering::SeqCst));
    let adapter = test_adapter_with_hooks(config);
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
        .subscribe_caret(Arc::new(move |field, _| {
            carets_in_cb.lock().unwrap().push(field);
        }))
        .expect("caret subscription");

    let installs = installs.lock().unwrap();
    (installs[1].dispatch)(observer_event(
        ObserverNotification::CaretChanged,
        resolved_identity("ax:0x222", 42, Some("editor-b")),
    ));
    (installs[1].dispatch)(observer_event(
        ObserverNotification::CaretChanged,
        resolved_identity("ax:0x333", 42, Some("editor-c")),
    ));
    (installs[0].dispatch)(observer_event(
        ObserverNotification::FocusChanged,
        resolved_identity("ax:0x222", 42, Some("editor-b")),
    ));
    (installs[0].dispatch)(observer_event(
        ObserverNotification::FocusChanged,
        resolved_identity("ax:0x444", 42, Some("editor-d")),
    ));
    (installs[0].dispatch)(observer_event(
        ObserverNotification::FocusChanged,
        resolved_identity("ax:0x555", 42, Some("editor-e")),
    ));
    (installs[1].dispatch)(observer_event(
        ObserverNotification::CaretChanged,
        resolved_identity("ax:0x444", 42, Some("editor-d")),
    ));
    drop(installs);

    let focused = focused.lock().unwrap();
    let carets = carets.lock().unwrap();
    assert_eq!(focused.len(), 3);
    assert_eq!(carets.len(), 3);
    assert_eq!(focused[0], carets[0]);
    assert_eq!(focused[1], carets[2]);
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
    // The poller records install #1 before it drops the old pid-42 binding.
    // The teardown is the deterministic happens-after signal that the
    // binding lifecycle completed before the post-rebind assertions.
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
    let (install_started_tx, install_started_rx) = mpsc::channel();
    let (release_install_tx, release_install_rx) = mpsc::channel();
    let release_install_rx = Arc::new(Mutex::new(release_install_rx));
    let after_install = Arc::new(move |pid| {
        if pid == 99 {
            install_started_tx.send(()).expect("signal install");
            release_install_rx
                .lock()
                .unwrap()
                .recv_timeout(WAIT_DEADLINE)
                .expect("release install");
        }
    });
    let adapter = test_adapter_with_dynamic_frontmost_and_install_hook(
        Arc::clone(&frontmost_pid),
        Arc::clone(&installs),
        Arc::clone(&teardowns),
        after_install,
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
    install_started_rx
        .recv_timeout(WAIT_DEADLINE)
        .expect("second install started");
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
    let delivered_before_binding_swap = carets.lock().unwrap().len();
    release_install_tx.send(()).expect("release second install");
    wait_for_vec_count(&teardowns, 1);
    assert_eq!(teardowns.lock().unwrap().as_slice(), [42]);

    let carets = carets.lock().unwrap();
    assert_eq!(delivered_before_binding_swap, 2);
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
    let (install_started_tx, install_started_rx) = mpsc::channel();
    let (release_install_tx, release_install_rx) = mpsc::channel();
    let release_install_rx = Arc::new(Mutex::new(release_install_rx));
    let after_install = Arc::new(move |pid| {
        if pid == 77 {
            install_started_tx.send(()).expect("signal install");
            release_install_rx
                .lock()
                .unwrap()
                .recv_timeout(WAIT_DEADLINE)
                .expect("release install");
        }
    });
    let adapter = test_adapter_with_dynamic_frontmost_and_install_hook(
        Arc::clone(&frontmost_pid),
        Arc::clone(&installs),
        Arc::clone(&teardowns),
        after_install,
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
    install_started_rx
        .recv_timeout(WAIT_DEADLINE)
        .expect("second install started");
    let second_dispatch = installs.lock().unwrap()[1].dispatch.clone();
    second_dispatch(observer_event_for_pid(
        77,
        ObserverNotification::FocusChanged,
        pointer_identity("ax:reborn"),
        None,
    ));
    let delivered_before_binding_swap = focused.lock().unwrap().len();
    release_install_tx.send(()).expect("release second install");

    assert_eq!(delivered_before_binding_swap, 1);
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
            has_underline_panel: false,
            underline_visible: false,
            underline_frame: None,
        }
    );
}
