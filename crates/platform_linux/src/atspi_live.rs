//! Live AT-SPI2 read path (ROADMAP Phase 2.1/2.2) — Linux only.
//!
//! Talks to the accessibility bus over D-Bus with `atspi`'s blocking proxies. The
//! decisions worth knowing:
//!
//! **Why D-Bus and not libatspi.** Linking the C library would make the compme
//! binary refuse to *start* on a host without it — a hard failure where this
//! project requires fail-closed degradation. A D-Bus connection is something we
//! attempt and report on, so a headless server or a session with accessibility
//! switched off simply has no adapter, not a broken executable.
//!
//! **Why blocking proxies.** [`platform::PlatformAdapter`] is synchronous, and the
//! macOS adapter already establishes the pattern of one owner thread serializing
//! platform calls. Blocking proxies fit that directly; an async runtime would add
//! a second concurrency idiom for no gain (see the plan's cross-cutting rules).
//!
//! **Offsets are Unicode scalars.** AT-SPI counts characters, not UTF-16 code
//! units, so this is the first adapter to report
//! [`platform::OffsetEncoding::UnicodeScalars`] — the unit the `context` crate
//! actually wants. Slicing therefore goes through scalar-aware helpers, never
//! byte indexing, and the integration tests deliberately include astral-plane
//! text because a UTF-16 assumption survives every ASCII test.

use crate::atspi_caps::{capabilities_from, FieldFacts};
use crate::atspi_ids::ElementId;
use atspi::proxy::accessible::AccessibleProxyBlocking;
use atspi::proxy::application::ApplicationProxyBlocking;
use atspi::proxy::component::ComponentProxyBlocking;
use atspi::proxy::editable_text::EditableTextProxyBlocking;
use atspi::proxy::text::TextProxyBlocking;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use atspi::zbus::blocking::Connection;
use atspi::{CoordType, Interface, State};
use platform::{
    Capabilities, ContextSource, FieldHandle, InsertStrategy, Inserted, OffsetEncoding,
    PlatformError, ScreenRect, TextContext, TextRange,
};

/// The accessibility registry's root object — the "desktop" whose children are
/// the applications currently exposing accessibility.
const REGISTRY_BUS: &str = "org.a11y.atspi.Registry";
const REGISTRY_ROOT: &str = "/org/a11y/atspi/accessible/root";

/// Depth bound for tree walks. The accessibility tree is a live UI hierarchy
/// owned by other processes: it can be deep, and a malformed one can cycle. A
/// bound turns "compme hangs" into "compme reports no field".
const MAX_DEPTH: usize = 16;

/// Cap on the text pulled from one field. `GetText(0, -1)` is unbounded input
/// from another process — a multi-megabyte document would otherwise be copied
/// through D-Bus on every keystroke.
const MAX_FIELD_SCALARS: usize = 200_000;

/// Validate AT-SPI's signed character count before any unbounded text transfer.
///
/// A negative count is a broken peer response. A count beyond the local safety
/// cap cannot be represented by a whole-field snapshot without silently losing
/// the suffix, so both cases fail closed.
fn checked_field_scalar_count(count: i32) -> Result<usize, PlatformError> {
    let count = usize::try_from(count)
        .map_err(|_| unsupported(format!("invalid negative field scalar count: {count}")))?;
    if count > MAX_FIELD_SCALARS {
        return Err(field_over_cap_error());
    }
    Ok(count)
}

/// Validate a host-supplied half-open scalar range against the provider's
/// current text length, then convert it to AT-SPI's signed scalar offsets.
/// Validation happens before `GetRangeExtents`, so a toolkit cannot silently
/// clamp an invalid correction range into plausible geometry.
fn checked_range_offsets(
    range: platform::CorrectionRange,
    character_count: i32,
) -> Result<(i32, i32), PlatformError> {
    if range.start > range.end {
        return Err(unsupported(format!(
            "platform_linux: inverted geometry range {}..{}",
            range.start, range.end
        )));
    }
    let character_count = usize::try_from(character_count).map_err(|_| {
        unsupported(format!(
            "invalid negative field scalar count: {character_count}"
        ))
    })?;
    if range.end > character_count {
        return Err(unsupported(format!(
            "platform_linux: geometry range {}..{} past the field length {character_count}",
            range.start, range.end
        )));
    }
    let start = i32::try_from(range.start)
        .map_err(|_| unsupported("platform_linux: geometry range start exceeds i32".into()))?;
    let end = i32::try_from(range.end)
        .map_err(|_| unsupported("platform_linux: geometry range end exceeds i32".into()))?;
    Ok((start, end))
}

/// Validate the scalar length a whole-field swap would write: `field_len`
/// scalars with `replaced` of them exchanged for `inserted`.
///
/// Writing an over-cap rebuilt value would mutate the field and then fail the
/// capped readback — reporting the accept as failed after it already landed —
/// so the swap is refused up front instead.
fn checked_rebuilt_len(
    field_len: usize,
    replaced: usize,
    inserted: usize,
) -> Result<usize, PlatformError> {
    let rebuilt = field_len - replaced + inserted;
    if rebuilt > MAX_FIELD_SCALARS {
        return Err(field_over_cap_error());
    }
    Ok(rebuilt)
}

/// Every pre-write check a whole-field range replacement owes, followed by the
/// value it would write. One function so the checks cannot drift apart from the
/// write they guard: the caller cannot reach `SetTextContents` without the
/// `updated` string this returns, so no check here can be dropped and still
/// compile — which the checks-as-separate-statements shape did allow.
///
/// The inverted-range guard stays at the call site on purpose: it must run
/// before the field is read over D-Bus, and this function takes the scalars that
/// read produced.
fn checked_replacement(
    scalars: &[char],
    expected_text: &str,
    text: &str,
    range: platform::CorrectionRange,
) -> Result<String, PlatformError> {
    if range.end > scalars.len() {
        return Err(unsupported(format!(
            "platform_linux: range {}..{} past the field length {}",
            range.start,
            range.end,
            scalars.len()
        )));
    }
    let current: String = scalars[range.start..range.end].iter().collect();
    if current != expected_text {
        return Err(unsupported(
            "platform_linux: field changed under the replacement".into(),
        ));
    }
    checked_rebuilt_len(scalars.len(), range.end - range.start, text.chars().count())?;
    let mut updated: String = scalars[..range.start].iter().collect();
    updated.push_str(text);
    updated.extend(scalars[range.end..].iter());
    Ok(updated)
}

fn replacement_snapshot_is_current(
    snapshot: &[char],
    current: &[char],
) -> Result<(), PlatformError> {
    if current != snapshot {
        return Err(unsupported(
            "platform_linux: field changed while preparing the replacement".into(),
        ));
    }
    Ok(())
}

fn dispatch_prepared_replacement<E>(
    snapshot: &[char],
    updated: String,
    attempt: &MutationAttempt,
    prepare: impl FnOnce() -> Result<E, PlatformError>,
    read_current: impl FnOnce() -> Result<Vec<char>, PlatformError>,
    write: impl FnOnce(E, &str) -> Result<bool, PlatformError>,
) -> Result<(bool, String), PlatformError> {
    let editable = prepare()?;
    let current = read_current()?;
    replacement_snapshot_is_current(snapshot, &current)?;
    let replaced = attempt.dispatch("set_text_contents", || write(editable, &updated))?;
    Ok((replaced, updated))
}

fn field_over_cap_error() -> PlatformError {
    unsupported(format!(
        "field exceeds {MAX_FIELD_SCALARS} scalars; refusing lossy read/replace"
    ))
}

/// Hard bound for one adapter-level accessibility-bus operation (G8): a
/// wedged or lost a11y bus must surface [`PlatformError::Timeout`] — the
/// adapter contract's anti-hang clause — never park the run loop.
/// Generous on purpose: a healthy bus answers in single-digit milliseconds,
/// so only a wedged server approaches this. Shared by the keyring and
/// reveal session-bus connections for their raw call sites.
pub(crate) const BUS_CALL_DEADLINE: Duration = Duration::from_secs(10);

/// Run one adapter-level bus operation on a helper thread under a deadline
/// (G8). zbus's per-connection `method_timeout` (set in
/// [`AtspiSession::open`]) covers only raw `Connection::call_method`; the
/// generated `*ProxyBlocking` calls — proxy construction included — have no
/// timeout of their own, so a wedged accessibility bus would otherwise hang
/// this adapter forever. The closure owns a clone of the connection (cheap
/// Arc clone; the proxies it builds borrow that clone and never leave the
/// helper thread). On deadline the caller gets [`PlatformError::Timeout`];
/// the helper thread is deliberately left to finish on its own. If a late call
/// returns a connection, proxy, or iterator after the receiver is gone, the
/// failed send drops that resource on the helper thread.
pub(crate) fn bounded_bus_call<T, F>(
    label: &str,
    deadline: Duration,
    call: F,
) -> Result<T, PlatformError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, PlatformError> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let spawn = std::thread::Builder::new()
        .name(format!("compme-bus-{label}"))
        .spawn(move || {
            let _ = tx.send(call());
        });
    if let Err(err) = spawn {
        return Err(cannot_complete("bus helper thread", err));
    }
    match rx.recv_timeout(deadline) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(PlatformError::Timeout),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(cannot_complete(
            "bus helper thread",
            "the helper exited without an answer",
        )),
    }
}

const MUTATION_PREPARING: u8 = 0;
const MUTATION_DISPATCHED: u8 = 1;
const MUTATION_EXPIRED: u8 = 2;

/// Which state an unknown write outcome makes untrustworthy. An AT-SPI method
/// is addressed to one accessible, so only that field is poisoned. XTEST
/// keystrokes go to whichever window holds X focus, so an uncertain synthetic
/// insert may have landed in any field and poisons the whole session.
#[derive(Clone, Debug, PartialEq, Eq)]
enum MutationScope {
    Field(String),
    SyntheticInput(String),
}

impl MutationScope {
    /// The encoded element id the mutation addresses.
    fn field(&self) -> &str {
        match self {
            Self::Field(field) | Self::SyntheticInput(field) => field,
        }
    }
}

/// Serializes AT-SPI writes and permanently refuses later writes and trusted
/// reads of a field once one dispatched request has an unknowable outcome for
/// it ([`MutationScope`] decides whether that is one field or the session). A
/// fresh `AtspiSession` is the recovery boundary: reusing this connection could
/// otherwise turn an engine retry or a later accept into a second write while
/// the provider is still resolving the first one.
#[derive(Default)]
struct MutationCoordinator {
    serial: Mutex<()>,
    quarantined_session: AtomicBool,
    quarantined_fields: Mutex<HashSet<String>>,
}

impl MutationCoordinator {
    fn is_quarantined(&self, field: &str) -> bool {
        self.quarantined_session.load(Ordering::Acquire)
            || self
                .quarantined_fields
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(field)
    }

    fn outcome_unknown(&self, scope: &MutationScope, reason: impl Into<String>) -> PlatformError {
        match scope {
            MutationScope::Field(field) => {
                self.quarantined_fields
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(field.clone());
            }
            MutationScope::SyntheticInput(_) => {
                self.quarantined_session.store(true, Ordering::Release);
            }
        }
        PlatformError::MutationOutcomeUnknown {
            reason: reason.into(),
        }
    }

    fn quarantine_error(&self) -> PlatformError {
        PlatformError::MutationOutcomeUnknown {
            reason: "platform_linux AT-SPI mutations are quarantined because an earlier D-Bus write outcome is unknown; establish a fresh accessibility session before writing again".to_string(),
        }
    }

    fn require_trusted_text_state(&self, field: &str) -> Result<(), PlatformError> {
        if self.is_quarantined(field) {
            Err(self.quarantine_error())
        } else {
            Ok(())
        }
    }
}

/// One mutation's linearization point. The caller's timeout and the helper's
/// write compete on the same atomic transition: expiry wins before dispatch, or
/// dispatch wins and the caller receives an honest unknown-outcome error. A
/// dropped reply receiver is never treated as D-Bus cancellation.
struct MutationAttempt {
    phase: AtomicU8,
    coordinator: Arc<MutationCoordinator>,
    scope: MutationScope,
}

impl MutationAttempt {
    fn new(coordinator: Arc<MutationCoordinator>, scope: MutationScope) -> Self {
        Self {
            phase: AtomicU8::new(MUTATION_PREPARING),
            coordinator,
            scope,
        }
    }

    fn expired(&self) -> bool {
        self.phase.load(Ordering::Acquire) == MUTATION_EXPIRED
    }

    fn was_dispatched(&self) -> bool {
        self.phase.load(Ordering::Acquire) == MUTATION_DISPATCHED
    }

    /// Mark the remote call dispatched immediately before invoking it. Any
    /// transport error after this boundary is conservatively unknown: D-Bus may
    /// have delivered the method and lost only the reply.
    fn dispatch<T>(
        &self,
        operation: &str,
        write: impl FnOnce() -> Result<T, PlatformError>,
    ) -> Result<T, PlatformError> {
        match self.phase.compare_exchange(
            MUTATION_PREPARING,
            MUTATION_DISPATCHED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {}
            Err(MUTATION_EXPIRED) => return Err(PlatformError::Timeout),
            Err(_) => {
                return Err(cannot_complete(
                    "mutation coordinator",
                    "attempted more than one native write in one mutation",
                ));
            }
        }
        write().map_err(|err| {
            self.coordinator.outcome_unknown(&self.scope, format!(
                "platform_linux AT-SPI {operation} was dispatched but its D-Bus reply failed ({err}); the provider may have applied the write"
            ))
        })
    }

    /// Returns true only when expiry prevented dispatch. False means the write
    /// crossed the native boundary and its eventual result cannot be reported to
    /// this caller safely.
    fn expire_before_dispatch(&self) -> bool {
        self.phase
            .compare_exchange(
                MUTATION_PREPARING,
                MUTATION_EXPIRED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn outcome_unknown(&self, reason: impl Into<String>) -> PlatformError {
        self.coordinator.outcome_unknown(&self.scope, reason)
    }
}

/// Mutation-specific counterpart to [`bounded_bus_call`]. Preparation may time
/// out normally because the atomic expiry prevents a later write. Once the
/// write is dispatched, timeout is reported as `MutationOutcomeUnknown` and the
/// attempt's [`MutationScope`] is quarantined: D-Bus provides no safe
/// cancellation or exactly-once retry primitive for these methods.
fn bounded_mutation_call<T, F>(
    coordinator: Arc<MutationCoordinator>,
    scope: MutationScope,
    label: &str,
    deadline: Duration,
    call: F,
) -> Result<T, PlatformError>
where
    T: Send + 'static,
    F: FnOnce(&MutationAttempt) -> Result<T, PlatformError> + Send + 'static,
{
    if coordinator.is_quarantined(scope.field()) {
        return Err(coordinator.quarantine_error());
    }

    let attempt = Arc::new(MutationAttempt::new(Arc::clone(&coordinator), scope));
    let worker_attempt = Arc::clone(&attempt);
    let worker_coordinator = Arc::clone(&coordinator);
    let worker_label = label.to_string();
    // Rendezvous, not a buffered channel: the helper keeps `serial` until the
    // caller receives the result or drops `rx` on timeout. Therefore a queued
    // read/mutation cannot enter between an unobserved result and quarantine.
    let (tx, rx) = mpsc::sync_channel(0);
    let spawn = std::thread::Builder::new()
        .name(format!("compme-bus-{label}"))
        .spawn(move || {
            let _serial = worker_coordinator
                .serial
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if worker_coordinator.is_quarantined(worker_attempt.scope.field()) {
                    Err(worker_coordinator.quarantine_error())
                } else if worker_attempt.expired() {
                    Err(PlatformError::Timeout)
                } else {
                    call(&worker_attempt)
                }
            }))
            .unwrap_or_else(|_| {
                if worker_attempt.was_dispatched() {
                    Err(worker_attempt.outcome_unknown(format!(
                        "platform_linux AT-SPI {worker_label} helper panicked after dispatch; the provider may have applied the write"
                    )))
                } else {
                    Err(cannot_complete(
                        "mutation helper thread",
                        "the helper panicked before dispatch",
                    ))
                }
            });
            if tx.send(result).is_err() && worker_attempt.was_dispatched() {
                // The caller's receive deadline won. `sync_channel(0)` keeps us
                // under `serial` until it drops the receiver; quarantine before
                // this guard releases so no queued operation can enter the gap.
                let _ = worker_attempt.outcome_unknown(format!(
                    "platform_linux AT-SPI {worker_label} result was not received after dispatch; the provider may have applied the write"
                ));
            }
        });
    if let Err(err) = spawn {
        return Err(cannot_complete("mutation helper thread", err));
    }

    match rx.recv_timeout(deadline) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) if attempt.expire_before_dispatch() => {
            drop(rx);
            Err(PlatformError::Timeout)
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let error = attempt.outcome_unknown(format!(
                "platform_linux AT-SPI {label} was dispatched before its deadline, but no reply arrived; the provider may have applied the write"
            ));
            drop(rx);
            Err(error)
        }
        Err(mpsc::RecvTimeoutError::Disconnected) if attempt.expire_before_dispatch() => Err(
            cannot_complete("mutation helper thread", "the helper exited without an answer"),
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(attempt.outcome_unknown(format!(
            "platform_linux AT-SPI {label} was dispatched, but its helper exited without an answer; the provider may have applied the write"
        ))),
    }
}

/// Serialize a text/capability snapshot with mutations, then re-check the
/// `field` quarantine under that same gate. This closes the race where a late
/// provider echo starts a read just before the mutation caller marks its
/// dispatched write unknown: the read waits for the mutation helper, observes
/// quarantine, and never exposes the uncertain field value to the host.
fn bounded_trusted_read_call<T, F>(
    coordinator: Arc<MutationCoordinator>,
    field: String,
    label: &str,
    deadline: Duration,
    call: F,
) -> Result<T, PlatformError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, PlatformError> + Send + 'static,
{
    bounded_bus_call(label, deadline, move || {
        let _serial = coordinator
            .serial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        coordinator.require_trusted_text_state(&field)?;
        call()
    })
}

fn cannot_complete(what: &str, err: impl std::fmt::Display) -> PlatformError {
    PlatformError::CannotComplete {
        reason: format!("platform_linux atspi {what}: {err}"),
    }
}

fn unsupported(reason: String) -> PlatformError {
    PlatformError::UnsupportedField { reason }
}

fn screen_rect_from_extents((x, y, width, height): (i32, i32, i32, i32)) -> Option<ScreenRect> {
    (width > 0 && height > 0).then_some(ScreenRect {
        x: f64::from(x),
        y: f64::from(y),
        w: f64::from(width),
        h: f64::from(height),
    })
}

/// A connection to the accessibility bus. One per adapter; the owning thread
/// serializes all use, mirroring the macOS AX worker.
pub struct AtspiSession {
    connection: Connection,
    mutations: Arc<MutationCoordinator>,
}

impl std::fmt::Debug for AtspiSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // zbus::blocking::Connection is not Debug; the address is the only part
        // worth logging and it is not secret.
        f.debug_struct("AtspiSession").finish_non_exhaustive()
    }
}

impl AtspiSession {
    /// Open the accessibility bus: ask `org.a11y.Bus` on the session bus for its
    /// address, then connect to it.
    ///
    /// Every failure here is expected on some legitimate host — no session bus on
    /// a headless server, no `org.a11y.Bus` when accessibility is not running — so
    /// each is reported rather than retried or panicked on. The caller treats an
    /// error as "this host has no accessibility support".
    pub fn open() -> Result<Self, PlatformError> {
        let session_address =
            atspi::zbus::Address::session().map_err(|err| cannot_complete("session bus", err))?;
        Self::open_from_session_address(session_address, BUS_CALL_DEADLINE)
    }

    fn open_from_session_address(
        session_address: atspi::zbus::Address,
        deadline: Duration,
    ) -> Result<Self, PlatformError> {
        // `method_timeout` applies only after a connection exists; put the
        // session and accessibility-bus authentication handshakes under one
        // outer deadline as well.
        bounded_bus_call("accessibility session setup", deadline, move || {
            Self::open_unbounded(session_address, deadline)
        })
    }

    fn open_unbounded(
        session_address: atspi::zbus::Address,
        deadline: Duration,
    ) -> Result<Self, PlatformError> {
        // The session bus connection carries a method timeout so the raw
        // GetAddress round trip below is bounded (zbus applies
        // `method_timeout` inside Connection::call_method).
        let session = atspi::zbus::blocking::connection::Builder::address(session_address)
            .map_err(|err| cannot_complete("session bus", err))?
            .method_timeout(deadline)
            .build()
            .map_err(|err| cannot_complete("session bus", err))?;
        let address: String = session
            .call_method(
                Some("org.a11y.Bus"),
                "/org/a11y/bus",
                Some("org.a11y.Bus"),
                "GetAddress",
                &(),
            )
            .map_err(|err| cannot_complete("org.a11y.Bus GetAddress", err))?
            .body()
            .deserialize()
            .map_err(|err| cannot_complete("a11y bus address", err))?;
        let address = address
            .parse::<atspi::zbus::Address>()
            .map_err(|err| cannot_complete("a11y bus address parse", err))?;
        let connection = atspi::zbus::blocking::connection::Builder::address(address)
            .map_err(|err| cannot_complete("a11y bus builder", err))?
            // Bounds the raw `Connection::call_method` sites on this
            // connection. The generated *ProxyBlocking calls are NOT covered
            // (zbus 5.19 applies this only inside call_method) — those are
            // bounded by [`bounded_bus_call`] at the adapter boundary.
            .method_timeout(deadline)
            .build()
            .map_err(|err| cannot_complete("a11y bus connect", err))?;
        Ok(Self {
            connection,
            mutations: Arc::new(MutationCoordinator::default()),
        })
    }

    /// The underlying bus connection. Exposed for the event workers
    /// (`atspi_events`), which clone it so cancelling a subscription can close it —
    /// the only way to interrupt a thread parked in the blocking message iterator.
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Walk the desktop tree to the focused accessible. Bounded (G8): a
    /// wedged bus surfaces `Timeout` instead of hanging the walk.
    pub fn focused_field(&self) -> Result<Option<ElementId>, PlatformError> {
        let connection = self.connection.clone();
        bounded_bus_call("focused_field", BUS_CALL_DEADLINE, move || {
            focused_field_on(&connection)
        })
    }

    /// Collect the facts [`capabilities_from`] needs. A missing toolkit name
    /// is not fatal — it only softens the `Toolkit` hint. Bounded (G8): a
    /// wedged bus surfaces `Timeout`.
    pub fn field_facts(&self, id: &ElementId) -> Result<FieldFacts, PlatformError> {
        // Capabilities enable text reads and writes. After an uncertain write,
        // neither may be advertised for that field: a delayed provider echo
        // must not be mistaken for fresh user input and recorded by the host.
        let id = id.clone();
        let connection = self.connection.clone();
        bounded_trusted_read_call(
            Arc::clone(&self.mutations),
            id.encode(),
            "field_facts",
            BUS_CALL_DEADLINE,
            move || field_facts_on(&connection, &id),
        )
    }

    pub fn capabilities(&self, id: &ElementId) -> Result<Capabilities, PlatformError> {
        Ok(capabilities_from(&self.field_facts(id)?))
    }

    /// Read the field's text split at the caret, plus any selection.
    /// Bounded (G8): a wedged bus surfaces `Timeout`.
    pub fn read_context(&self, field: &FieldHandle) -> Result<TextContext, PlatformError> {
        // Keep the quarantine persistent even after the late D-Bus reply arrives.
        // Without this gate, its eventual text-changed signal could be consumed as
        // ordinary typing and enter context memory despite an uncommitted accept.
        let field = field.clone();
        let connection = self.connection.clone();
        bounded_trusted_read_call(
            Arc::clone(&self.mutations),
            field.element_id.clone(),
            "read_context",
            BUS_CALL_DEADLINE,
            move || read_context_on(&connection, &field),
        )
    }

    /// Screen rectangle of the character at the caret; `Ok(None)` means "no
    /// usable geometry". Bounded (G8): a wedged bus surfaces `Timeout`.
    pub fn caret_rect(&self, field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
        let field = field.clone();
        let connection = self.connection.clone();
        bounded_bus_call("caret_rect", BUS_CALL_DEADLINE, move || {
            caret_rect_on(&connection, &field)
        })
    }

    /// Screen rectangle enclosing a valid half-open scalar range. AT-SPI uses
    /// Unicode-scalar offsets already, so only bounds validation and the signed
    /// wire conversion are needed. Bounded and serialized with text mutations:
    /// geometry from a quarantined field is no longer trustworthy.
    pub fn text_range_rect(
        &self,
        field: &FieldHandle,
        range: platform::CorrectionRange,
    ) -> Result<Option<ScreenRect>, PlatformError> {
        let field = field.clone();
        let connection = self.connection.clone();
        bounded_trusted_read_call(
            Arc::clone(&self.mutations),
            field.element_id.clone(),
            "text_range_rect",
            BUS_CALL_DEADLINE,
            move || text_range_rect_on(&connection, &field, range),
        )
    }

    /// Screen bounds of the focused component, used only when per-character
    /// caret geometry is unavailable. Bounded (G8): a wedged bus surfaces
    /// `Timeout`.
    pub fn popup_anchor(&self, field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError> {
        let field = field.clone();
        let connection = self.connection.clone();
        bounded_bus_call("popup_anchor", BUS_CALL_DEADLINE, move || {
            popup_anchor_on(&connection, &field)
        })
    }

    /// An accessible's `Name` property, for diagnostics and walk
    /// confirmation; `None` when the object is gone or refuses it. Bounded
    /// (G8): a wedged bus surfaces `Timeout`.
    pub fn element_name(&self, id: &ElementId) -> Option<String> {
        let id = id.clone();
        let connection = self.connection.clone();
        bounded_bus_call("element_name", BUS_CALL_DEADLINE, move || {
            Ok(accessible_on(&connection, &id)
                .ok()
                .and_then(|node| node.name().ok()))
        })
        .ok()
        .flatten()
    }

    /// Insert `text` at the caret through `EditableText.InsertText`.
    /// Bounded (G8): a wedged bus surfaces `Timeout`.
    pub fn insert(&self, field: &FieldHandle, text: &str) -> Result<Inserted, PlatformError> {
        let field = field.clone();
        let text = text.to_string();
        let connection = self.connection.clone();
        bounded_mutation_call(
            Arc::clone(&self.mutations),
            MutationScope::Field(field.element_id.clone()),
            "insert",
            BUS_CALL_DEADLINE,
            move |attempt| insert_on(&connection, &field, &text, attempt),
        )
    }

    /// Insert plain text through XTEST when the focused field exposes readable
    /// AT-SPI Text state but no EditableText mutation interface. The X11 module
    /// preflights the complete key sequence; this coordinator supplies the same
    /// bounded dispatch, unknown-outcome quarantine, and trusted readback used
    /// by native AT-SPI writes.
    pub fn insert_synthetic(
        &self,
        field: &FieldHandle,
        text: &str,
    ) -> Result<Inserted, PlatformError> {
        let field = field.clone();
        let text = text.to_string();
        let connection = self.connection.clone();
        bounded_mutation_call(
            Arc::clone(&self.mutations),
            MutationScope::SyntheticInput(field.element_id.clone()),
            "xtest_insert",
            BUS_CALL_DEADLINE,
            move |attempt| insert_synthetic_on(&connection, &field, &text, attempt),
        )
    }

    /// Replace exactly `range` with `text` while the field still holds
    /// `expected_text` there (atomicity and cap reasoning documented on the
    /// bus-side half). Bounded (G8): a wedged bus surfaces `Timeout`.
    pub fn insert_replacing_range(
        &self,
        field: &FieldHandle,
        expected_text: &str,
        text: &str,
        range: platform::CorrectionRange,
        strategy: InsertStrategy,
    ) -> Result<Inserted, PlatformError> {
        let field = field.clone();
        let expected_text = expected_text.to_string();
        let text = text.to_string();
        let connection = self.connection.clone();
        bounded_mutation_call(
            Arc::clone(&self.mutations),
            MutationScope::Field(field.element_id.clone()),
            "insert_replacing_range",
            BUS_CALL_DEADLINE,
            move |attempt| {
                insert_replacing_range_on(
                    &connection,
                    &field,
                    &expected_text,
                    &text,
                    range,
                    strategy,
                    attempt,
                )
            },
        )
    }

    /// The application owning the focused field, for `front_app`. Bounded
    /// (G8) — one deadline covers the walk and the name; a wedged bus
    /// surfaces `Timeout`.
    pub fn focused_app_name(&self) -> Option<String> {
        let connection = self.connection.clone();
        bounded_bus_call("focused_app_name", BUS_CALL_DEADLINE, move || {
            let focused = focused_field_on(&connection).ok().flatten();
            Ok(focused.and_then(|focused| application_name_on(&connection, &focused)))
        })
        .ok()
        .flatten()
    }

    /// The `app` and `pid` for a [`FieldHandle`] built from an event. Bounded
    /// (G8): a wedged bus surfaces `Timeout` and the owning bus name stands
    /// in.
    pub(crate) fn element_owner(&self, id: &ElementId) -> (String, Option<u32>) {
        let id = id.clone();
        let connection = self.connection.clone();
        let fallback = id.bus_name.clone();
        bounded_bus_call("element_owner", BUS_CALL_DEADLINE, move || {
            let app = application_name_on(&connection, &id).unwrap_or_else(|| id.bus_name.clone());
            let pid = owner_pid_on(&connection, &id);
            Ok((app, pid))
        })
        .unwrap_or((fallback, None))
    }
}

fn accessible_on<'a>(
    connection: &'a Connection,
    id: &ElementId,
) -> Result<AccessibleProxyBlocking<'a>, PlatformError> {
    AccessibleProxyBlocking::builder(connection)
        .destination(id.bus_name.clone())
        .map_err(|err| cannot_complete("accessible destination", err))?
        .path(id.path.clone())
        .map_err(|err| cannot_complete("accessible path", err))?
        .build()
        .map_err(|err| cannot_complete("accessible proxy", err))
}

fn text_on<'a>(
    connection: &'a Connection,
    id: &ElementId,
) -> Result<TextProxyBlocking<'a>, PlatformError> {
    TextProxyBlocking::builder(connection)
        .destination(id.bus_name.clone())
        .map_err(|err| cannot_complete("text destination", err))?
        .path(id.path.clone())
        .map_err(|err| cannot_complete("text path", err))?
        .build()
        .map_err(|err| cannot_complete("text proxy", err))
}

fn component_on<'a>(
    connection: &'a Connection,
    id: &ElementId,
) -> Result<ComponentProxyBlocking<'a>, PlatformError> {
    ComponentProxyBlocking::builder(connection)
        .destination(id.bus_name.clone())
        .map_err(|err| cannot_complete("component destination", err))?
        .path(id.path.clone())
        .map_err(|err| cannot_complete("component path", err))?
        .build()
        .map_err(|err| cannot_complete("component proxy", err))
}

fn editable_text_on<'a>(
    connection: &'a Connection,
    id: &ElementId,
) -> Result<EditableTextProxyBlocking<'a>, PlatformError> {
    EditableTextProxyBlocking::builder(connection)
        .destination(id.bus_name.clone())
        .map_err(|err| cannot_complete("editable destination", err))?
        .path(id.path.clone())
        .map_err(|err| cannot_complete("editable path", err))?
        .build()
        .map_err(|err| cannot_complete("editable proxy", err))
}

fn toolkit_name_on(connection: &Connection, node: &AccessibleProxyBlocking<'_>) -> Option<String> {
    let app = node.get_application().ok()?;
    let app_id = ElementId::new(app.name_as_str()?, app.path_as_str());
    ApplicationProxyBlocking::builder(connection)
        .destination(app_id.bus_name)
        .ok()?
        .path(app_id.path)
        .ok()?
        .build()
        .ok()?
        .toolkit_name()
        .ok()
}

fn find_focused_on(
    connection: &Connection,
    id: &ElementId,
    depth: usize,
) -> Result<Option<ElementId>, PlatformError> {
    if depth > MAX_DEPTH {
        return Ok(None);
    }
    let node = accessible_on(connection, id)?;
    if let Ok(states) = node.get_state() {
        if states.contains(State::Focused) {
            return Ok(Some(id.clone()));
        }
    }
    let children = match node.get_children() {
        Ok(children) => children,
        // A child list we cannot read is not an error for the whole walk:
        // the application may have exited mid-traversal.
        Err(_) => return Ok(None),
    };
    for child in children {
        let Some(bus_name) = child.name_as_str() else {
            continue;
        };
        let child_id = ElementId::new(bus_name, child.path_as_str());
        if let Ok(Some(found)) = find_focused_on(connection, &child_id, depth + 1) {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// The focused editable field, if any: a depth-bounded walk from the registry
/// root looking for `STATE_FOCUSED`.
///
/// Applications that expose no accessibility are simply absent from this
/// tree, and one unreachable application must not hide the rest, so a
/// per-application error is skipped rather than propagated.
pub fn focused_field_on(connection: &Connection) -> Result<Option<ElementId>, PlatformError> {
    let root = ElementId::new(REGISTRY_BUS, REGISTRY_ROOT);
    let desktop = accessible_on(connection, &root)?;
    let apps = desktop
        .get_children()
        .map_err(|err| cannot_complete("desktop children", err))?;
    for app in apps {
        let Some(bus_name) = app.name_as_str() else {
            continue;
        };
        let app_id = ElementId::new(bus_name, app.path_as_str());
        if let Ok(Some(found)) = find_focused_on(connection, &app_id, 0) {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// Collect the facts [`capabilities_from`] needs. A missing toolkit name is
/// not fatal — it only softens the `Toolkit` hint.
fn field_facts_on(connection: &Connection, id: &ElementId) -> Result<FieldFacts, PlatformError> {
    let node = accessible_on(connection, id)?;
    let interfaces = node
        .get_interfaces()
        .map_err(|err| cannot_complete("get_interfaces", err))?;
    let states = node
        .get_state()
        .map_err(|err| cannot_complete("get_state", err))?;
    let role = node
        .get_role_name()
        .map_err(|err| cannot_complete("get_role_name", err))?;
    let toolkit_name = toolkit_name_on(connection, &node).unwrap_or_default();
    Ok(FieldFacts {
        has_text: interfaces.contains(Interface::Text),
        has_editable_text: interfaces.contains(Interface::EditableText),
        editable: states.contains(State::Editable),
        sensitive: states.contains(State::Sensitive),
        multiline: states.contains(State::MultiLine),
        role,
        toolkit_name,
    })
}

/// Read the field's text split at the caret, plus any selection.
///
/// The caret offset and every returned range are Unicode scalars, matching
/// AT-SPI's own unit; nothing here indexes bytes.
fn read_context_on(
    connection: &Connection,
    field: &FieldHandle,
) -> Result<TextContext, PlatformError> {
    let id = ElementId::decode(&field.element_id)
        .ok_or_else(|| unsupported(format!("malformed element id: {}", field.element_id)))?;
    let text = text_on(connection, &id)?;
    let scalars = field_scalars_from_text_on(&text)?;
    let caret = text
        .caret_offset()
        .map_err(|err| cannot_complete("caret_offset", err))?;
    // A negative or past-the-end caret is a toolkit bug, not something to
    // propagate into scalar arithmetic: clamp into the text we actually read.
    let caret = usize::try_from(caret).unwrap_or(0).min(scalars.len());
    let (selection, selected_text) = selection_on(&text, &scalars);
    let (left_end, right_start, caret) = selection.map_or((caret, caret, caret), |range| {
        (range.start, range.end, range.start)
    });
    let left: String = scalars[..left_end].iter().collect();
    let right: String = scalars[right_start..].iter().collect();
    Ok(TextContext {
        left,
        right,
        left_scalars: left_end,
        selection,
        selected_text,
        caret,
        source: ContextSource::Accessibility,
        field_id: field.clone(),
        offset_encoding: OffsetEncoding::UnicodeScalars,
    })
}

/// The first non-empty selection, as a scalar range plus its exact text.
///
/// Selection is best-effort: a toolkit that refuses the query still has a
/// readable field, and reporting no selection is the safe answer (it only
/// disables selection-scoped features like the thesaurus).
fn selection_on(
    text: &TextProxyBlocking<'_>,
    scalars: &[char],
) -> (Option<TextRange>, Option<String>) {
    if text.get_n_selections().unwrap_or(0) < 1 {
        return (None, None);
    }
    let Ok((start, end)) = text.get_selection(0) else {
        return (None, None);
    };
    let start = usize::try_from(start).unwrap_or(0).min(scalars.len());
    let end = usize::try_from(end).unwrap_or(0).min(scalars.len());
    if start >= end {
        return (None, None);
    }
    let selected: String = scalars[start..end].iter().collect();
    (Some(TextRange { start, end }), Some(selected))
}

/// Screen rectangle of the character at the caret.
///
/// `Ok(None)` means "no usable geometry" — either the toolkit reported a
/// degenerate rect or the caret sits past the last character. Overlay
/// placement must degrade rather than draw at a bogus point.
fn caret_rect_on(
    connection: &Connection,
    field: &FieldHandle,
) -> Result<Option<ScreenRect>, PlatformError> {
    let id = ElementId::decode(&field.element_id)
        .ok_or_else(|| unsupported(format!("malformed element id: {}", field.element_id)))?;
    let text = text_on(connection, &id)?;
    let caret = text
        .caret_offset()
        .map_err(|err| cannot_complete("caret_offset", err))?;
    let count = text.character_count().unwrap_or(0);
    // GetCharacterExtents is defined per existing character, so a caret at the
    // end has none; fall back to the last character's box, which is where the
    // next glyph will appear.
    let offset = caret.min(count.saturating_sub(1)).max(0);
    if count <= 0 {
        return Ok(None);
    }
    let (x, y, width, height) = text
        .get_character_extents(offset, CoordType::Screen)
        .map_err(|err| cannot_complete("get_character_extents", err))?;
    Ok(screen_rect_from_extents((x, y, width, height)))
}

/// Screen rectangle enclosing `range`, whose offsets are Unicode scalars in
/// both the shared contract and AT-SPI's Text interface.
fn text_range_rect_on(
    connection: &Connection,
    field: &FieldHandle,
    range: platform::CorrectionRange,
) -> Result<Option<ScreenRect>, PlatformError> {
    let id = element(field)?;
    let text = text_on(connection, &id)?;
    let count = text
        .character_count()
        .map_err(|err| cannot_complete("character_count", err))?;
    let (start, end) = checked_range_offsets(range, count)?;
    if start == end {
        return Ok(None);
    }
    let extents = text
        .get_range_extents(start, end, CoordType::Screen)
        .map_err(|err| cannot_complete("get_range_extents", err))?;
    Ok(screen_rect_from_extents(extents))
}

/// Screen bounds of the focused component, used only when per-character
/// caret geometry is unavailable (notably an empty text field).
fn popup_anchor_on(
    connection: &Connection,
    field: &FieldHandle,
) -> Result<Option<ScreenRect>, PlatformError> {
    let id = ElementId::decode(&field.element_id)
        .ok_or_else(|| unsupported(format!("malformed element id: {}", field.element_id)))?;
    let extents = component_on(connection, &id)?
        .get_extents(CoordType::Screen)
        .map_err(|err| cannot_complete("component get_extents", err))?;
    Ok(screen_rect_from_extents(extents))
}

/// `EditableText.InsertText`'s `length` argument: the UTF-8 **byte** length of
/// `text` (Atspi-2.0/Atk-1.0 GIR; GTK forwards it to `gtk_editable_insert_text`
/// and `gtk_text_buffer_insert`), unlike the scalar-counted `position`.
fn insert_text_length(text: &str) -> Result<i32, PlatformError> {
    i32::try_from(text.len()).map_err(|_| {
        unsupported(format!(
            "platform_linux: {} insert bytes exceed the AT-SPI length range",
            text.len()
        ))
    })
}

/// Insert `text` at the caret through `EditableText.InsertText`.
///
/// `position` is a scalar offset and `length` a byte count
/// ([`insert_text_length`]); the returned `chars` counts scalars so caret math
/// on the host side stays consistent. The inserted span is read back, so a
/// toolkit that truncates or rejects part of the text reports an unknown
/// outcome instead of a success it never delivered.
fn insert_on(
    connection: &Connection,
    field: &FieldHandle,
    text: &str,
    attempt: &MutationAttempt,
) -> Result<Inserted, PlatformError> {
    let id = element(field)?;
    let length = insert_text_length(text)?;
    let scalars = text.chars().count();
    let caret = caret_offset_on(connection, &id)?;
    let end = i32::try_from(scalars)
        .ok()
        .and_then(|scalars| caret.checked_add(scalars))
        .ok_or_else(|| {
            unsupported(format!(
                "platform_linux: insert of {scalars} scalars at {caret} overflows AT-SPI offsets"
            ))
        })?;
    let editable = editable_text_on(connection, &id)?;
    let inserted = attempt.dispatch("insert_text", || {
        editable
            .insert_text(caret, text, length)
            .map_err(|err| cannot_complete("insert_text", err))
    })?;
    if !inserted {
        return Err(cannot_complete(
            "insert_text",
            "the toolkit refused the insert",
        ));
    }
    let landed = text_on(connection, &id)
        .and_then(|proxy| {
            proxy
                .get_text(caret, end)
                .map_err(|err| cannot_complete("get_text", err))
        })
        .map_err(|err| {
            attempt.outcome_unknown(format!(
                "platform_linux AT-SPI insert_text succeeded, but its readback failed ({err}); the inserted text cannot be verified"
            ))
        })?;
    if landed != text {
        return Err(attempt.outcome_unknown(
            "platform_linux AT-SPI insert_text succeeded, but readback does not match the inserted text",
        ));
    }
    Ok(Inserted {
        bytes: text.len(),
        chars: scalars,
        strategy: InsertStrategy::NativeRangeSet,
    })
}

/// Insert a complete, preflighted XTEST sequence and prove the resulting field
/// value through AT-SPI. Preparation failures leave the field unchanged. Once
/// the first X event is dispatched, every transport/readback failure is an
/// unknown outcome and permanently quarantines this accessibility session.
fn insert_synthetic_on(
    connection: &Connection,
    field: &FieldHandle,
    text: &str,
    attempt: &MutationAttempt,
) -> Result<Inserted, PlatformError> {
    let prepared = crate::x11_insert::prepare(text)?;
    let inserted = Inserted {
        bytes: text.len(),
        chars: text.chars().count(),
        strategy: InsertStrategy::SyntheticKeys,
    };
    if prepared.is_empty() {
        return Ok(inserted);
    }

    let id = element(field)?;
    if focused_field_on(connection)?.as_ref() != Some(&id) {
        return Err(PlatformError::StaleField);
    }
    require_no_selection_on(connection, &id)?;
    let before = read_context_on(connection, field)?;
    let expected = checked_synthetic_value(&before.left, &before.right, text)?;
    prepared.wait_for_clear_keyboard()?;
    // Keep the last identity check adjacent to the global X input boundary.
    // The earlier check avoids reading the wrong field; this one closes the
    // avoidable window spent on selection, text, and cap validation.
    if focused_field_on(connection)?.as_ref() != Some(&id) {
        return Err(PlatformError::StaleField);
    }
    prepared.require_plan_still_safe_now()?;
    prepared.require_clear_keyboard_now()?;

    attempt.dispatch("XTEST synthetic insert", || prepared.dispatch())?;

    const READBACK_ATTEMPTS: usize = 4;
    const READBACK_DELAY: Duration = Duration::from_millis(20);
    let mut last_observed = None;
    for index in 0..READBACK_ATTEMPTS {
        match field_scalars_on(connection, &id) {
            Ok(scalars) => {
                let observed: String = scalars.into_iter().collect();
                if observed == expected {
                    return Ok(inserted);
                }
                last_observed = Some(observed);
            }
            Err(err) => {
                return Err(attempt.outcome_unknown(format!(
                    "platform_linux XTEST input was dispatched, but AT-SPI readback failed ({err}); the field may contain a partial insert"
                )));
            }
        }
        if index + 1 < READBACK_ATTEMPTS {
            std::thread::sleep(READBACK_DELAY);
        }
    }
    Err(attempt.outcome_unknown(synthetic_readback_mismatch_reason(last_observed.as_deref())))
}

fn checked_synthetic_value(
    left: &str,
    right: &str,
    inserted: &str,
) -> Result<String, PlatformError> {
    let field_len = left.chars().count() + right.chars().count();
    checked_rebuilt_len(field_len, 0, inserted.chars().count())?;
    let mut expected = String::with_capacity(left.len() + inserted.len() + right.len());
    expected.push_str(left);
    expected.push_str(inserted);
    expected.push_str(right);
    Ok(expected)
}

fn synthetic_readback_mismatch_reason(observed: Option<&str>) -> String {
    let observed_scalars = observed.map(|value| value.chars().count());
    format!(
        "platform_linux XTEST input was dispatched, but readback did not match the expected field value (observed scalar count {observed_scalars:?}); the field may contain a partial insert"
    )
}

/// Unlike context display, mutation preparation cannot treat a failed
/// selection query as "no selection": typing would replace selected text. Ask
/// every reported selection authoritatively and refuse before XTEST dispatch.
fn require_no_selection_on(connection: &Connection, id: &ElementId) -> Result<(), PlatformError> {
    let text = text_on(connection, id)?;
    let count = text
        .get_n_selections()
        .map_err(|err| cannot_complete("get_n_selections", err))?;
    if count < 0 {
        return Err(unsupported(format!(
            "platform_linux: invalid negative selection count {count}"
        )));
    }
    for index in 0..count {
        let (start, end) = text
            .get_selection(index)
            .map_err(|err| cannot_complete("get_selection", err))?;
        if start != end {
            return Err(unsupported(
                "platform_linux: XTEST plain insert refuses an active selection".to_string(),
            ));
        }
    }
    Ok(())
}

/// The field's complete text as scalars, after rejecting an over-cap field.
fn field_scalars_on(connection: &Connection, id: &ElementId) -> Result<Vec<char>, PlatformError> {
    let text = text_on(connection, id)?;
    field_scalars_from_text_on(&text)
}

/// Query the authoritative size before asking another process to transfer
/// its whole value. The post-fetch check closes the race where the field
/// grows between `CharacterCount` and `GetText`.
fn field_scalars_from_text_on(text: &TextProxyBlocking<'_>) -> Result<Vec<char>, PlatformError> {
    let announced_count = text
        .character_count()
        .map_err(|err| cannot_complete("character_count", err))?;
    checked_field_scalar_count(announced_count)?;
    let value = text
        .get_text(0, -1)
        .map_err(|err| cannot_complete("get_text", err))?;
    let scalars: Vec<char> = value.chars().collect();
    if scalars.len() > MAX_FIELD_SCALARS {
        return Err(field_over_cap_error());
    }
    Ok(scalars)
}

fn caret_offset_on(connection: &Connection, id: &ElementId) -> Result<i32, PlatformError> {
    text_on(connection, id)?
        .caret_offset()
        .map_err(|err| cannot_complete("caret_offset", err))
}

/// Decode a handle's element id, failing closed on anything malformed.
fn element(field: &FieldHandle) -> Result<ElementId, PlatformError> {
    ElementId::decode(&field.element_id)
        .ok_or_else(|| unsupported(format!("malformed element id: {}", field.element_id)))
}

/// The name of the application owning `id`.
fn application_name_on(connection: &Connection, id: &ElementId) -> Option<String> {
    let node = accessible_on(connection, id).ok()?;
    let app = node.get_application().ok()?;
    let app_id = ElementId::new(app.name_as_str()?, app.path_as_str());
    accessible_on(connection, &app_id).ok()?.name().ok()
}

/// The unix process id behind `id`'s bus name, asked of the accessibility bus
/// itself. `None` whenever the bus declines — a peer that has already exited is
/// the ordinary case, not an error worth propagating into a focus event.
fn owner_pid_on(connection: &Connection, id: &ElementId) -> Option<u32> {
    let bus_name = atspi::zbus::names::BusName::try_from(id.bus_name.as_str()).ok()?;
    atspi::zbus::blocking::fdo::DBusProxy::new(connection)
        .ok()?
        .get_connection_unix_process_id(bus_name)
        .ok()
}

/// Replace exactly `range` with `text`, but only while the field still holds
/// `expected_text` there.
///
/// **Why a whole-value swap and not DeleteText+InsertText.** The contract for
/// an atomic strategy is all-or-nothing. `DeleteText` followed by `InsertText`
/// is two D-Bus round trips: a failure between them leaves the user's field
/// truncated, which is worse than refusing the replacement. `SetTextContents`
/// is one call, so the field either changes completely or not at all — the
/// same reasoning that makes macOS use an `AXValue` set.
/// Before taking that whole-field snapshot, the adapter checks AT-SPI's
/// character count and refuses fields above `MAX_FIELD_SCALARS`. It never
/// reconstructs a value from a capped prefix, and it refuses a rebuilt
/// value that would itself exceed the cap — otherwise a replacement longer
/// than its range could mutate an at-cap field and then fail the capped
/// readback, reporting an accept as failed after it landed.
///
/// The expected-text guard is re-checked immediately before the swap, so a
/// keystroke that landed between the suggestion and the accept invalidates the
/// replacement instead of overwriting what the user just typed.
fn insert_replacing_range_on(
    connection: &Connection,
    field: &FieldHandle,
    expected_text: &str,
    text: &str,
    range: platform::CorrectionRange,
    strategy: InsertStrategy,
    attempt: &MutationAttempt,
) -> Result<Inserted, PlatformError> {
    if !strategy.supports_atomic_range_replace() {
        return Err(unsupported(format!(
            "platform_linux: {strategy:?} cannot range-replace atomically"
        )));
    }
    if range.start > range.end {
        return Err(unsupported(format!(
            "platform_linux: inverted range {}..{}",
            range.start, range.end
        )));
    }
    let id = element(field)?;
    let scalars = field_scalars_on(connection, &id)?;
    let updated = checked_replacement(&scalars, expected_text, text, range)?;
    let (replaced, updated) = dispatch_prepared_replacement(
        &scalars,
        updated,
        attempt,
        || editable_text_on(connection, &id),
        || field_scalars_on(connection, &id),
        |editable, updated| {
            editable
                .set_text_contents(updated)
                .map_err(|err| cannot_complete("set_text_contents", err))
        },
    )?;
    if !replaced {
        return Err(cannot_complete(
            "set_text_contents",
            "the toolkit refused the replacement",
        ));
    }
    // Verified readback: a toolkit that accepts the call and stores something
    // else would otherwise leave the engine believing text it never wrote.
    let after: String = field_scalars_on(connection, &id)
        .map_err(|err| {
            attempt.outcome_unknown(format!(
                "platform_linux AT-SPI set_text_contents succeeded, but its readback failed ({err}); the written field value cannot be verified"
            ))
        })?
        .iter()
        .collect();
    if after != updated {
        return Err(attempt.outcome_unknown(
            "platform_linux AT-SPI set_text_contents succeeded, but readback does not match the written value",
        ));
    }
    // SetTextContents resets the toolkit caret (GTK moves it to 0). Put it
    // just after the replacement, in scalars like every AT-SPI Text offset.
    // The verified value already landed, so a refused caret move degrades
    // editing comfort only and is not an unknown write outcome.
    let chars = text.chars().count();
    let caret = i32::try_from(range.start + chars).unwrap_or(i32::MAX);
    match text_on(connection, &id).and_then(|proxy| {
        proxy
            .set_caret_offset(caret)
            .map_err(|err| cannot_complete("set_caret_offset", err))
    }) {
        Ok(true) => {}
        Ok(false) => eprintln!(
            "compme: Linux range replace landed, but the toolkit refused the caret move to {caret}"
        ),
        Err(err) => {
            eprintln!("compme: Linux range replace landed, but restoring the caret failed: {err}")
        }
    }
    Ok(Inserted {
        bytes: text.len(),
        chars,
        strategy: InsertStrategy::NativeRangeSet,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_authentication_is_bounded_by_the_setup_deadline() {
        use std::os::unix::net::UnixListener;

        let socket = std::env::temp_dir().join(format!(
            "compme-atspi-auth-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        let listener = UnixListener::bind(&socket).expect("bind fake D-Bus listener");
        let address: atspi::zbus::Address = format!("unix:path={}", socket.display())
            .parse()
            .expect("local Unix address");
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept client");
            accepted_tx.send(()).expect("test still listening");
            let _ = release_rx.recv();
            drop(stream);
        });
        let (done_tx, done_rx) = mpsc::channel();
        let client = std::thread::spawn(move || {
            let result =
                AtspiSession::open_from_session_address(address, Duration::from_millis(50));
            let _ = done_tx.send(result);
        });

        accepted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("client reached fake listener");
        let result = done_rx.recv_timeout(Duration::from_secs(2));
        release_tx.send(()).expect("release fake listener");
        server.join().expect("fake server exits");
        client.join().expect("client helper exits");
        let _ = std::fs::remove_file(socket);

        assert!(
            matches!(result, Ok(Err(PlatformError::Timeout))),
            "authentication must return Timeout before the fake server is released"
        );
    }

    #[test]
    fn bounded_bus_call_maps_a_wedged_call_to_platform_timeout() {
        // G8: a wedged accessibility bus must surface the contract's
        // PlatformError::Timeout, never park the caller. The closure stands
        // in for a *ProxyBlocking call that never returns.
        // The call stays wedged until the test releases it after the caller
        // has returned, so "gave up at the deadline" does not race a sleep. A
        // caller that waited instead would sit out the 30 s release fallback.
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let started = std::time::Instant::now();
        let result = bounded_bus_call("wedged", std::time::Duration::from_millis(10), move || {
            let _ = release_rx.recv_timeout(std::time::Duration::from_secs(30));
            Ok(7)
        });
        let elapsed = started.elapsed();
        let _ = release_tx.send(());
        assert_eq!(result, Err(PlatformError::Timeout));
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "the caller must give up at the deadline, not wait the closure out: {elapsed:?}"
        );
    }

    #[test]
    fn bounded_bus_call_passes_fast_results_and_errors_through() {
        // The bound is a wrapper, not a filter: a fast call's value and a
        // fast call's mapped error both arrive unchanged.
        assert_eq!(
            bounded_bus_call("fast-ok", BUS_CALL_DEADLINE, || Ok(vec![1, 2])),
            Ok(vec![1, 2])
        );
        let expected = unsupported("fast failure".to_string());
        assert_eq!(
            bounded_bus_call("fast-err", BUS_CALL_DEADLINE, || {
                Err::<u8, _>(unsupported("fast failure".to_string()))
            }),
            Err(expected)
        );
    }

    const TEST_FIELD: &str = ":1.7|/test/field";

    fn test_scope() -> MutationScope {
        MutationScope::Field(TEST_FIELD.to_string())
    }

    struct BarrierMutationTransport {
        prepare_gate: Mutex<Option<mpsc::Receiver<()>>>,
        write_gate: Mutex<Option<mpsc::Receiver<()>>>,
        write_started: Mutex<Option<mpsc::Sender<()>>>,
        writes: Arc<std::sync::atomic::AtomicUsize>,
        accepts: bool,
    }

    impl BarrierMutationTransport {
        fn execute(&self, attempt: &MutationAttempt) -> Result<(), PlatformError> {
            if let Some(gate) = self
                .prepare_gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                gate.recv().expect("release preparation");
            }
            let accepted = attempt.dispatch("test_write", || {
                self.writes.fetch_add(1, Ordering::SeqCst);
                if let Some(started) = self
                    .write_started
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    started.send(()).expect("report dispatched test write");
                }
                if let Some(gate) = self
                    .write_gate
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    gate.recv().expect("release write reply");
                }
                Ok(self.accepts)
            })?;
            if accepted {
                Ok(())
            } else {
                Err(cannot_complete("test_write", "provider refused the write"))
            }
        }
    }

    fn immediate_transport(
        writes: Arc<std::sync::atomic::AtomicUsize>,
        accepts: bool,
    ) -> Arc<BarrierMutationTransport> {
        Arc::new(BarrierMutationTransport {
            prepare_gate: Mutex::new(None),
            write_gate: Mutex::new(None),
            write_started: Mutex::new(None),
            writes,
            accepts,
        })
    }

    #[test]
    fn expired_mutation_preparation_cannot_write_after_timeout() {
        let coordinator = Arc::new(MutationCoordinator::default());
        let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let transport = Arc::new(BarrierMutationTransport {
            prepare_gate: Mutex::new(Some(release_rx)),
            write_gate: Mutex::new(None),
            write_started: Mutex::new(None),
            writes: Arc::clone(&writes),
            accepts: true,
        });

        let result = bounded_mutation_call(
            Arc::clone(&coordinator),
            test_scope(),
            "late-preparation",
            Duration::from_millis(10),
            move |attempt| {
                let result = transport.execute(attempt);
                done_tx.send(()).expect("report helper completion");
                result
            },
        );
        assert_eq!(result, Err(PlatformError::Timeout));

        // The helper may observe the expiry before it runs the closure and
        // drop it unrun, which closes both channels; that is a finish too.
        let _ = release_tx.send(());
        match done_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("expired helper must finish"),
        }
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        assert!(
            !coordinator.is_quarantined(TEST_FIELD),
            "expiry before dispatch leaves the session safe for later writes"
        );
    }

    #[test]
    fn late_write_reply_serializes_then_quarantines_every_later_mutation() {
        let coordinator = Arc::new(MutationCoordinator::default());
        let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (release_write_tx, release_write_rx) = mpsc::channel();
        let (write_started_tx, write_started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let transport = Arc::new(BarrierMutationTransport {
            prepare_gate: Mutex::new(None),
            write_gate: Mutex::new(Some(release_write_rx)),
            write_started: Mutex::new(Some(write_started_tx)),
            writes: Arc::clone(&writes),
            accepts: true,
        });
        let first_coordinator = Arc::clone(&coordinator);
        let first = std::thread::spawn(move || {
            bounded_mutation_call(
                first_coordinator,
                test_scope(),
                "late-write-reply",
                Duration::from_millis(80),
                move |attempt| {
                    let result = transport.execute(attempt);
                    done_tx.send(()).expect("report first helper completion");
                    result
                },
            )
        });
        write_started_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("first write must dispatch");

        let overlapping = immediate_transport(Arc::clone(&writes), true);
        assert_eq!(
            bounded_mutation_call(
                Arc::clone(&coordinator),
                test_scope(),
                "overlap",
                Duration::from_millis(10),
                move |attempt| overlapping.execute(attempt),
            ),
            Err(PlatformError::Timeout),
            "an overlapping attempt must expire while the dispatched write owns the serial gate"
        );
        let first_result = first.join().expect("join first caller");
        assert!(matches!(
            first_result,
            Err(PlatformError::MutationOutcomeUnknown { reason })
                if reason.contains("late-write-reply") && reason.contains("may have applied")
        ));

        let quarantined = immediate_transport(Arc::clone(&writes), true);
        assert!(matches!(
            bounded_mutation_call(
                Arc::clone(&coordinator),
                test_scope(),
                "after-unknown",
                Duration::from_secs(1),
                move |attempt| quarantined.execute(attempt),
            ),
            Err(PlatformError::MutationOutcomeUnknown { reason })
                if reason.contains("quarantined")
        ));
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        assert!(matches!(
            coordinator.require_trusted_text_state(TEST_FIELD),
            Err(PlatformError::MutationOutcomeUnknown { .. })
        ));

        // Reproduce the publication race independently of the timeout caller's
        // store: clear its quarantine contribution while the dispatched helper
        // still owns `serial`, then queue a trusted read. The helper's failed
        // rendezvous send must restore quarantine before it unlocks; otherwise
        // this read executes against the uncertain late write.
        coordinator
            .quarantined_fields
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        let trusted_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let read_count = Arc::clone(&trusted_reads);
        let queued_read_coordinator = Arc::clone(&coordinator);
        let (queued_tx, queued_rx) = mpsc::channel();
        let queued_read = std::thread::spawn(move || {
            queued_tx.send(()).expect("report queued read caller");
            bounded_trusted_read_call(
                queued_read_coordinator,
                TEST_FIELD.to_string(),
                "after-late-echo",
                Duration::from_secs(1),
                move || {
                    read_count.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
        });
        queued_rx.recv().expect("trusted read must be queued");
        release_write_tx.send(()).expect("release late reply");
        done_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("late write helper must finish");
        assert!(matches!(
            queued_read.join().expect("join queued read"),
            Err(PlatformError::MutationOutcomeUnknown { .. })
        ));
        assert_eq!(
            trusted_reads.load(Ordering::SeqCst),
            0,
            "an uncertain mutation's late echo must not expose a text snapshot"
        );
        let still_quarantined = immediate_transport(Arc::clone(&writes), true);
        assert!(matches!(
            bounded_mutation_call(
                coordinator,
                test_scope(),
                "after-late-reply",
                Duration::from_secs(1),
                move |attempt| still_quarantined.execute(attempt),
            ),
            Err(PlatformError::MutationOutcomeUnknown { .. })
        ));
        assert_eq!(writes.load(Ordering::SeqCst), 1);
    }

    /// Runs one fake write that lands and then fails its readback, the shape
    /// `insert_on` and `insert_replacing_range_on` map to an unknown outcome.
    fn mismatched_readback_write(
        coordinator: &Arc<MutationCoordinator>,
        scope: MutationScope,
        writes: &Arc<std::sync::atomic::AtomicUsize>,
    ) -> Result<(), PlatformError> {
        let transport = immediate_transport(Arc::clone(writes), true);
        bounded_mutation_call(
            Arc::clone(coordinator),
            scope,
            "mismatched-readback",
            Duration::from_secs(1),
            move |attempt| {
                transport.execute(attempt)?;
                Err(attempt.outcome_unknown("readback does not match the written value"))
            },
        )
    }

    fn fake_write(
        coordinator: &Arc<MutationCoordinator>,
        field: &str,
        writes: &Arc<std::sync::atomic::AtomicUsize>,
    ) -> Result<(), PlatformError> {
        let transport = immediate_transport(Arc::clone(writes), true);
        bounded_mutation_call(
            Arc::clone(coordinator),
            MutationScope::Field(field.to_string()),
            "fake-write",
            Duration::from_secs(1),
            move |attempt| transport.execute(attempt),
        )
    }

    fn fake_trusted_read(
        coordinator: &Arc<MutationCoordinator>,
        field: &str,
    ) -> Result<(), PlatformError> {
        bounded_trusted_read_call(
            Arc::clone(coordinator),
            field.to_string(),
            "fake-read",
            Duration::from_secs(1),
            || Ok(()),
        )
    }

    #[test]
    fn readback_mismatch_quarantines_only_that_field() {
        const OTHER_FIELD: &str = ":1.7|/test/other";
        let coordinator = Arc::new(MutationCoordinator::default());
        let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        assert!(matches!(
            mismatched_readback_write(&coordinator, test_scope(), &writes),
            Err(PlatformError::MutationOutcomeUnknown { reason })
                if reason.contains("readback does not match")
        ));
        assert_eq!(writes.load(Ordering::SeqCst), 1);

        assert!(matches!(
            fake_write(&coordinator, TEST_FIELD, &writes),
            Err(PlatformError::MutationOutcomeUnknown { reason }) if reason.contains("quarantined")
        ));
        assert!(matches!(
            fake_trusted_read(&coordinator, TEST_FIELD),
            Err(PlatformError::MutationOutcomeUnknown { .. })
        ));
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "the uncertain field must never be written again"
        );

        assert_eq!(fake_write(&coordinator, OTHER_FIELD, &writes), Ok(()));
        assert_eq!(fake_trusted_read(&coordinator, OTHER_FIELD), Ok(()));
        assert_eq!(writes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn uncertain_synthetic_input_quarantines_every_field() {
        const OTHER_FIELD: &str = ":1.7|/test/other";
        let coordinator = Arc::new(MutationCoordinator::default());
        let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        assert!(matches!(
            mismatched_readback_write(
                &coordinator,
                MutationScope::SyntheticInput(TEST_FIELD.to_string()),
                &writes,
            ),
            Err(PlatformError::MutationOutcomeUnknown { .. })
        ));
        // XTEST keystrokes follow X focus, not an accessible, so an uncertain
        // one may have landed in any field.
        assert!(matches!(
            fake_write(&coordinator, OTHER_FIELD, &writes),
            Err(PlatformError::MutationOutcomeUnknown { reason }) if reason.contains("quarantined")
        ));
        assert!(matches!(
            fake_trusted_read(&coordinator, OTHER_FIELD),
            Err(PlatformError::MutationOutcomeUnknown { .. })
        ));
        assert_eq!(writes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn successful_and_refused_writes_keep_the_session_usable() {
        let coordinator = Arc::new(MutationCoordinator::default());
        let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let refused = immediate_transport(Arc::clone(&writes), false);
        assert!(matches!(
            bounded_mutation_call(
                Arc::clone(&coordinator),
                test_scope(),
                "refused",
                Duration::from_secs(1),
                move |attempt| refused.execute(attempt),
            ),
            Err(PlatformError::CannotComplete { reason })
                if reason.contains("provider refused")
        ));
        assert!(!coordinator.is_quarantined(TEST_FIELD));

        let successful = immediate_transport(Arc::clone(&writes), true);
        assert_eq!(
            bounded_mutation_call(
                Arc::clone(&coordinator),
                test_scope(),
                "successful",
                Duration::from_secs(1),
                move |attempt| successful.execute(attempt),
            ),
            Ok(())
        );
        assert!(!coordinator.is_quarantined(TEST_FIELD));
        assert_eq!(writes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn screen_rect_from_extents_accepts_component_bounds_and_rejects_degenerate_geometry() {
        assert_eq!(
            screen_rect_from_extents((10, 20, 300, 24)),
            Some(ScreenRect {
                x: 10.0,
                y: 20.0,
                w: 300.0,
                h: 24.0,
            })
        );
        assert_eq!(screen_rect_from_extents((10, 20, 0, 24)), None);
        assert_eq!(screen_rect_from_extents((10, 20, 300, -1)), None);
    }

    #[test]
    fn range_offsets_validate_half_open_scalar_bounds_before_provider_io() {
        assert_eq!(
            checked_range_offsets(platform::CorrectionRange { start: 1, end: 2 }, 3)
                .expect("one astral scalar occupies one AT-SPI offset"),
            (1, 2)
        );
        assert_eq!(
            checked_range_offsets(platform::CorrectionRange { start: 3, end: 3 }, 3)
                .expect("empty range at the field end is valid"),
            (3, 3)
        );
        assert!(matches!(
            checked_range_offsets(platform::CorrectionRange { start: 2, end: 1 }, 3),
            Err(PlatformError::UnsupportedField { reason })
                if reason.contains("inverted geometry range 2..1")
        ));
        assert!(matches!(
            checked_range_offsets(platform::CorrectionRange { start: 0, end: 4 }, 3),
            Err(PlatformError::UnsupportedField { reason })
                if reason.contains("geometry range 0..4 past the field length 3")
        ));
        assert!(matches!(
            checked_range_offsets(platform::CorrectionRange { start: 0, end: 0 }, -1),
            Err(PlatformError::UnsupportedField { reason })
                if reason == "invalid negative field scalar count: -1"
        ));
    }

    #[test]
    fn insert_text_length_is_the_utf8_byte_count_not_the_scalar_count() {
        assert_eq!(insert_text_length("XY").expect("ascii"), 2);
        assert_eq!(insert_text_length("é😀ß").expect("non-ascii"), 8);
        assert_eq!(insert_text_length("").expect("empty"), 0);
    }

    #[test]
    fn synthetic_preflight_caps_the_result_and_mismatch_diagnostics_hide_field_text() {
        let at_cap = "p".repeat(MAX_FIELD_SCALARS);
        assert!(matches!(
            checked_synthetic_value(&at_cap, "", "x"),
            Err(PlatformError::UnsupportedField { reason })
                if reason == "field exceeds 200000 scalars; refusing lossy read/replace"
        ));

        let private = "private existing field contents";
        let reason = synthetic_readback_mismatch_reason(Some(private));
        assert!(!reason.contains(private));
        assert!(reason.contains("observed scalar count Some(31)"));
    }

    #[test]
    fn field_scalar_count_rejects_negative_and_over_cap_values() {
        assert_eq!(checked_field_scalar_count(0).expect("empty field"), 0);
        assert_eq!(
            checked_field_scalar_count(MAX_FIELD_SCALARS as i32).expect("field at cap"),
            MAX_FIELD_SCALARS
        );

        assert!(matches!(
            checked_field_scalar_count(-1),
            Err(PlatformError::UnsupportedField { reason })
                if reason == "invalid negative field scalar count: -1"
        ));
        assert!(matches!(
            checked_field_scalar_count(MAX_FIELD_SCALARS as i32 + 1),
            Err(PlatformError::UnsupportedField { reason })
                if reason == "field exceeds 200000 scalars; refusing lossy read/replace"
        ));
    }

    /// A replacement longer than the range it replaces must not push an at-cap
    /// field over the limit: the write would land and then the capped readback
    /// would report failure — the mutate-then-error shape A61 exists to forbid.
    #[test]
    fn rebuilt_field_at_cap_is_allowed_and_one_scalar_over_is_refused() {
        assert_eq!(
            checked_rebuilt_len(MAX_FIELD_SCALARS, 3, 3).expect("same-length swap at cap"),
            MAX_FIELD_SCALARS
        );
        assert_eq!(
            checked_rebuilt_len(MAX_FIELD_SCALARS, 3, 2).expect("shrinking swap at cap"),
            MAX_FIELD_SCALARS - 1
        );
        assert!(matches!(
            checked_rebuilt_len(MAX_FIELD_SCALARS, 3, 4),
            Err(PlatformError::UnsupportedField { reason })
                if reason == "field exceeds 200000 scalars; refusing lossy read/replace"
        ));
    }

    /// The same cap, asserted where `insert_replacing_range` actually applies it:
    /// on the whole pre-write sequence, over a real at-cap field. Pinning only
    /// [`checked_rebuilt_len`]'s verdict left the call site free — the check
    /// could be deleted from the replacement path with every test still green.
    #[test]
    fn at_cap_field_refuses_a_growing_replacement_and_still_builds_a_same_size_one() {
        let scalars: Vec<char> = std::iter::repeat_n('a', MAX_FIELD_SCALARS).collect();
        let range = platform::CorrectionRange { start: 0, end: 1 };
        let expected = "a";

        // One scalar out, two in: the rebuilt value is one over the cap, so the
        // replacement must be refused *before* anything is written.
        assert!(matches!(
            checked_replacement(&scalars, expected, "bc", range),
            Err(PlatformError::UnsupportedField { reason })
                if reason == "field exceeds 200000 scalars; refusing lossy read/replace"
        ));

        // Same size and shrinking stay allowed at the cap: the guard is about
        // growth past the cap, not about touching a full field at all.
        let same = checked_replacement(&scalars, expected, "b", range).expect("same-size swap");
        assert_eq!(same.chars().count(), MAX_FIELD_SCALARS);
        assert!(same.starts_with('b'));
        let shrunk = checked_replacement(&scalars, expected, "", range).expect("shrinking swap");
        assert_eq!(shrunk.chars().count(), MAX_FIELD_SCALARS - 1);
    }

    /// The other two pre-write guards, over the same sequence: a range past the
    /// field and text that changed under the replacement must both refuse
    /// without producing a value to write.
    #[test]
    fn replacement_refuses_a_range_past_the_field_and_text_that_changed_underneath() {
        let scalars: Vec<char> = "hello".chars().collect();
        assert!(matches!(
            checked_replacement(
                &scalars,
                "hello",
                "x",
                platform::CorrectionRange { start: 0, end: 9 },
            ),
            Err(PlatformError::UnsupportedField { reason })
                if reason == "platform_linux: range 0..9 past the field length 5"
        ));
        let current_private = "current private field text";
        let expected_private = "expected private field text";
        let private_scalars: Vec<char> = current_private.chars().collect();
        let mismatch = checked_replacement(
            &private_scalars,
            expected_private,
            "x",
            platform::CorrectionRange {
                start: 0,
                end: private_scalars.len(),
            },
        );
        assert!(matches!(
            mismatch,
            Err(PlatformError::UnsupportedField { reason })
                if reason == "platform_linux: field changed under the replacement"
                    && !reason.contains(current_private)
                    && !reason.contains(expected_private)
        ));
        assert_eq!(
            checked_replacement(
                &scalars,
                "ell",
                "i",
                platform::CorrectionRange { start: 1, end: 4 },
            )
            .expect("in-range swap"),
            "hio"
        );
    }

    #[test]
    fn replacement_revalidation_refuses_edits_inside_or_outside_the_range() {
        fn assert_refused(mutated: &str) {
            let field = std::cell::RefCell::new("teh quick brown".to_string());
            let snapshot: Vec<char> = field.borrow().chars().collect();
            let writes = std::cell::Cell::new(0usize);
            let attempt =
                MutationAttempt::new(Arc::new(MutationCoordinator::default()), test_scope());
            let updated = checked_replacement(
                &snapshot,
                "teh",
                "the",
                platform::CorrectionRange { start: 0, end: 3 },
            )
            .expect("initial snapshot matches");

            let result = dispatch_prepared_replacement(
                &snapshot,
                updated,
                &attempt,
                || {
                    *field.borrow_mut() = mutated.to_string();
                    Ok(())
                },
                || Ok(field.borrow().chars().collect()),
                |(), updated| {
                    writes.set(writes.get() + 1);
                    *field.borrow_mut() = updated.to_string();
                    Ok(true)
                },
            );

            assert!(matches!(
                result,
                Err(PlatformError::UnsupportedField { reason })
                    if reason == "platform_linux: field changed while preparing the replacement"
            ));
            assert_eq!(writes.get(), 0, "a changed snapshot must never be written");
            assert_eq!(
                field.into_inner(),
                mutated,
                "the user's intervening edit must survive"
            );
        }

        assert_refused("ten quick brown");
        assert_refused("teh quick brown!");

        let field = std::cell::RefCell::new("teh quick brown".to_string());
        let snapshot: Vec<char> = field.borrow().chars().collect();
        let writes = std::cell::Cell::new(0usize);
        let attempt = MutationAttempt::new(Arc::new(MutationCoordinator::default()), test_scope());
        let updated = checked_replacement(
            &snapshot,
            "teh",
            "the",
            platform::CorrectionRange { start: 0, end: 3 },
        )
        .expect("initial snapshot matches");
        let (replaced, updated) = dispatch_prepared_replacement(
            &snapshot,
            updated,
            &attempt,
            || Ok(()),
            || Ok(field.borrow().chars().collect()),
            |(), updated| {
                writes.set(writes.get() + 1);
                *field.borrow_mut() = updated.to_string();
                Ok(true)
            },
        )
        .expect("an unchanged prepared snapshot is written");
        assert!(replaced);
        assert_eq!(updated, "the quick brown");
        assert_eq!(writes.get(), 1);
        assert_eq!(field.into_inner(), "the quick brown");
    }

    /// `open()` must report a diagnosable error rather than panic when no
    /// accessibility bus exists. That is the normal state on a build machine and
    /// on any headless server, so it is the path most likely to run in anger.
    #[test]
    fn opening_without_an_accessibility_bus_fails_closed() {
        // Only assert the shape when there is demonstrably no session bus; a
        // developer desktop running this test may legitimately have one.
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
            return;
        }
        match AtspiSession::open() {
            Err(PlatformError::CannotComplete { reason }) => {
                assert!(
                    reason.starts_with("platform_linux atspi "),
                    "reason should name the crate and layer: {reason:?}"
                );
            }
            Err(other) => panic!("expected CannotComplete, got {other:?}"),
            Ok(_) => panic!("no session bus, yet a connection succeeded"),
        }
    }
}
