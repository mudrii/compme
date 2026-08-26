//! Live AT-SPI2 focus and caret events (ROADMAP Phase 2.1, event half) — Linux only.
//!
//! One subscription owns two threads and its own accessibility-bus connection:
//!
//! - a **reader** parked in the blocking message iterator, which does nothing but
//!   decode a signal into the element it concerns and forward it, and
//! - a **dispatcher** which owns every blocking call — the `app`/`pid` lookups, the
//!   caret geometry, and the subscriber callback.
//!
//! That split is the macOS worker/`CallbackDispatcher` shape ported over, and it is
//! load-bearing here for the same reason: a subscriber that takes a millisecond must
//! not stall the bus reader, and a subscriber that panics must not kill the
//! subscription.
//!
//! **Why a connection per subscription.** Stopping a subscription has to interrupt a
//! thread parked in `MessageIterator::next()`, and the blocking zbus API has no
//! interruptible receive — nor can the iterator be dropped from another thread while
//! the parked one borrows it. Closing the connection shuts its socket down both ways,
//! so the pending read fails, the stream terminates and the reader unwinds. That is
//! only safe if the connection belongs to the subscription alone, which is why this
//! opens its own rather than sharing the adapter's read-path session.
//!
//! **Registration is two steps, both required.** `org.a11y.atspi.Registry` decides
//! which events applications emit *at all* (an unregistered event never reaches the
//! bus), while the bus's match rule decides which of them are routed to us. Missing
//! either one yields a subscription that silently never fires.

use crate::atspi_event_map::{latest, LinuxFieldRegistry};
use crate::atspi_ids::ElementId;
use crate::atspi_live::AtspiSession;
use atspi::events::object::{StateChangedEvent, TextCaretMovedEvent};
use atspi::events::{DBusMatchRule, RegistryEventString};
use atspi::proxy::registry::RegistryProxyBlocking;
use atspi::zbus::blocking::{Connection, MessageIterator};
use atspi::zbus::Message;
use atspi::{ObjectRefOwned, State};
use platform::{CaretCallback, FocusCallback, PlatformError, Subscription};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Registry event string for "this object took the focus". Deliberately narrower
/// than `StateChangedEvent::REGISTRY_EVENT_STRING` (`object:state-changed`): the
/// registry uses this string to tell applications which state changes to emit, so
/// naming the detail keeps every other state transition off the bus entirely.
const FOCUS_REGISTRY_EVENT: &str = "object:state-changed:focused";

/// Depth of the per-subscription message queue. The connection's socket reader
/// broadcasts into it, so it has to absorb a keystroke burst while the dispatcher is
/// mid-round-trip; 64 is zbus's own default for a stream.
const EVENT_QUEUE_DEPTH: usize = 64;

/// Floor on the spacing between caret-geometry round trips. Every caret event costs
/// three D-Bus calls to resolve a rect, and AT-SPI emits one per keystroke, so an
/// unthrottled dispatcher would spend a typist's whole latency budget on the bus.
/// Throttling loses intermediate positions, never the final one: the dispatcher
/// always delivers the newest queued event (see [`latest`]).
///
/// 25ms matches the macOS adapter's `CARET_COALESCE_INTERVAL_MS`, so both platforms
/// present the host with the same worst-case caret event rate.
const CARET_MIN_INTERVAL: Duration = Duration::from_millis(25);

/// How long cancellation waits for the workers to acknowledge shutdown before
/// detaching them. Unsubscribing runs on the engine's run loop, which must not be
/// parked indefinitely by an accessibility bus that has stopped answering.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

fn cannot_complete(what: &str, err: impl std::fmt::Display) -> PlatformError {
    PlatformError::CannotComplete {
        reason: format!("platform_linux atspi events {what}: {err}"),
    }
}

/// Register for `object:state-changed:focused` and report each element that takes
/// the focus.
pub fn subscribe_focus(
    fields: Arc<Mutex<LinuxFieldRegistry>>,
    cb: FocusCallback,
) -> Result<Subscription, PlatformError> {
    let mut delivered = None;
    let workers = start(
        FOCUS_REGISTRY_EVENT,
        StateChangedEvent::MATCH_RULE_STRING,
        decode_focus,
        // Focus changes are rare and each one is a distinct field the host must
        // re-probe, so there is no time window to coalesce over — but consecutive
        // *duplicates* are dropped below, which is a different thing.
        None,
        move |session, element| {
            let field = fields
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .focus(&element, || session.element_owner(&element));
            // GTK emits `state-changed:focused` **twice** for one focus move
            // (measured on GTK3/at-spi2 2.60 with the harness fixture), and each
            // focus event costs the host a capability probe plus a field read. So
            // suppress a consecutive repeat of the same field, matching what the
            // macOS adapter does with its `current_identity_key`. The limitation is
            // the same too: an element destroyed and rebuilt at the same object path
            // looks like a duplicate, so the host learns about it from the next
            // caret event or the next focus change instead.
            if delivered.as_ref() == Some(&field) {
                return;
            }
            delivered = Some(field.clone());
            cb(field);
        },
    )?;
    Ok(into_subscription(workers))
}

/// Register for `object:text-caret-moved` and report the caret's screen geometry.
pub fn subscribe_caret(
    fields: Arc<Mutex<LinuxFieldRegistry>>,
    cb: CaretCallback,
) -> Result<Subscription, PlatformError> {
    let workers = start(
        TextCaretMovedEvent::REGISTRY_EVENT_STRING,
        TextCaretMovedEvent::MATCH_RULE_STRING,
        decode_caret,
        Some(CARET_MIN_INTERVAL),
        move |session, element| {
            let field = fields
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .current_for(&element);
            let Some(field) = field else {
                if debug_enabled() {
                    eprintln!(
                        "compme: dropped foreign AT-SPI caret event for {}",
                        element.encode()
                    );
                }
                return;
            };
            // Geometry is best effort by contract: `None` means "no usable rect",
            // which the host already handles by falling back to popup placement. A
            // toolkit that refuses extents must still produce a caret event.
            let rect = session.caret_rect(&field).unwrap_or(None);
            // Focus and caret dispatchers are independent threads. If focus moved
            // during the geometry round trip, suppress this now-stale callback so
            // it cannot arrive after the new focus and cancel that field's debounce.
            if fields
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .validate(&field)
                .is_err()
            {
                if debug_enabled() {
                    eprintln!(
                        "compme: dropped AT-SPI caret callback superseded during geometry for {}",
                        element.encode()
                    );
                }
                return;
            }
            cb(field, rect);
        },
    )?;
    Ok(into_subscription(workers))
}

pub(crate) fn debug_enabled() -> bool {
    debug_flag_on(std::env::var_os("COMPME_DEBUG").as_deref())
}

fn debug_flag_on(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| {
        let value = value.to_string_lossy();
        !value.is_empty()
            && !matches!(
                value.to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
    })
}

/// `object:state-changed:focused` with `enabled` set — the element that just took
/// the focus.
///
/// A `focused = false` event is dropped: [`platform::FocusCallback`] carries a
/// field, so a field *losing* focus has nothing to report through it, and inventing
/// a handle for it would make the host act on a field the user has left.
fn decode_focus(message: &Message) -> Option<ElementId> {
    let event = StateChangedEvent::try_from(message).ok()?;
    if event.state != State::Focused || !event.enabled {
        return None;
    }
    element_id(&event.item)
}

fn decode_caret(message: &Message) -> Option<ElementId> {
    element_id(&TextCaretMovedEvent::try_from(message).ok()?.item)
}

/// The event's subject as an [`ElementId`]. `None` for AT-SPI's null object
/// reference, which names no addressable accessible — a toolkit sends it for "the
/// object is gone", and encoding it would produce a handle every later call fails on.
fn element_id(item: &ObjectRefOwned) -> Option<ElementId> {
    Some(ElementId::new(item.name_as_str()?, item.path_as_str()))
}

/// The reader/dispatcher pair behind one subscription, and everything needed to stop
/// them.
struct EventWorkers {
    /// Closed before the connection is, so nothing new passes the delivery gate
    /// after stop. A callback already past the gate may complete after
    /// `Subscription::drop` returns if a worker outlives the timeout.
    active: Arc<AtomicBool>,
    /// A clone of the subscription's own accessibility-bus connection. Closing it is
    /// what wakes the reader out of its blocking receive (see the module docs).
    connection: Connection,
    /// Signalled by the dispatcher as its last act. The dispatcher can only reach
    /// that point after the reader has exited and dropped the event channel, so this
    /// one acknowledgement covers both threads.
    stopped: mpsc::Receiver<()>,
    threads: Vec<JoinHandle<()>>,
}

impl EventWorkers {
    fn stop(self) {
        // `close` is the only part of this that needs a bus, so the sequence
        // itself lives in [`stop_gated_workers`] where a test can drive it.
        let EventWorkers {
            active,
            connection,
            stopped,
            threads,
        } = self;
        stop_gated_workers(
            &active,
            || {
                let _ = connection.close();
            },
            &stopped,
            threads,
        );
    }
}

/// Close the delivery gate, wake the workers, and wait a bounded time for their
/// acknowledgement before detaching them.
///
/// Order is load-bearing: the gate closes *first*, so anything the `wake` step
/// shakes loose is already refused delivery, and the wait can then time out
/// without leaving a callback able to fire.
fn stop_gated_workers(
    active: &AtomicBool,
    wake: impl FnOnce(),
    stopped: &mpsc::Receiver<()>,
    threads: Vec<JoinHandle<()>>,
) {
    active.store(false, Ordering::Release);
    wake();
    if stopped.recv_timeout(STOP_TIMEOUT).is_err() {
        // Detach rather than park the run loop on a bus that stopped answering.
        // Nothing new can pass the gate above. A callback already past it may
        // finish; returning here keeps subscription drop bounded even though
        // that worker cannot be joined safely within the timeout.
        return;
    }
    for thread in threads {
        let _ = thread.join();
    }
}

/// The dispatcher thread's loop: take the next event, refuse it if the
/// subscription has stopped, coalesce, and hand it to the subscriber.
///
/// The gate is re-read per event rather than once, because an event queued
/// before `stop` is still waiting in the channel when `stop` runs — delivering
/// it would be a callback after the subscription was cancelled.
fn dispatch_gated_events<F>(
    active: &AtomicBool,
    events: &mpsc::Receiver<ElementId>,
    coalesce: Option<Duration>,
    mut deliver: F,
) where
    F: FnMut(ElementId),
{
    while let Ok(element) = events.recv() {
        if !active.load(Ordering::Acquire) {
            break;
        }
        let element = match coalesce {
            Some(_) => latest(element, events),
            None => element,
        };
        // A panicking subscriber must not take the subscription with it: the
        // contract lets callbacks run on an adapter-internal thread, and
        // unwinding out of one would silently end delivery for every later event.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            deliver(element);
        }));
        if let Some(interval) = coalesce {
            thread::sleep(interval);
        }
    }
}

fn into_subscription(workers: EventWorkers) -> Subscription {
    crate::new_cancelling_subscription(move || workers.stop())
}

/// Start the reader and dispatcher for one event kind.
///
/// `decode` runs on the reader thread and must stay cheap — it turns a bus message
/// into the element it concerns, or `None` for anything not of interest. `deliver`
/// runs on the dispatcher thread and owns all the blocking work.
fn start<F>(
    registry_event: &str,
    match_rule: &'static str,
    decode: fn(&Message) -> Option<ElementId>,
    coalesce: Option<Duration>,
    mut deliver: F,
) -> Result<EventWorkers, PlatformError>
where
    F: FnMut(&AtspiSession, ElementId) + Send + 'static,
{
    let session = AtspiSession::open()?;
    let connection = session.connection().clone();
    RegistryProxyBlocking::new(&connection)
        .map_err(|err| cannot_complete("registry proxy", err))?
        .register_event(registry_event)
        .map_err(|err| cannot_complete("Registry.RegisterEvent", err))?;
    // `for_match_rule` both registers the rule with the bus and filters what the
    // iterator yields, so the reader never wakes for another client's traffic.
    let messages =
        MessageIterator::for_match_rule(match_rule, &connection, Some(EVENT_QUEUE_DEPTH))
            .map_err(|err| cannot_complete("event match rule", err))?;

    let active = Arc::new(AtomicBool::new(true));
    let (event_tx, event_rx) = mpsc::channel();
    let (stopped_tx, stopped_rx) = mpsc::channel();

    // The dispatcher starts first: if the reader then fails to spawn, dropping its
    // never-started closure drops `event_tx`, and the dispatcher retires on its own.
    let active_for_dispatch = Arc::clone(&active);
    let dispatcher = spawn("compme-atspi-dispatch", move || {
        dispatch_gated_events(&active_for_dispatch, &event_rx, coalesce, |element| {
            deliver(&session, element);
        });
        let _ = stopped_tx.send(());
    })?;

    let reader = spawn("compme-atspi-events", move || {
        for message in messages {
            // An `Err` is the closed connection or a bus that died; either way there
            // is nothing left to read.
            let Ok(message) = message else {
                break;
            };
            if let Some(element) = decode(&message) {
                if event_tx.send(element).is_err() {
                    break;
                }
            }
        }
    })?;

    Ok(EventWorkers {
        active,
        connection,
        stopped: stopped_rx,
        threads: vec![reader, dispatcher],
    })
}

fn spawn(
    name: &str,
    body: impl FnOnce() + Send + 'static,
) -> Result<JoinHandle<()>, PlatformError> {
    thread::Builder::new()
        .name(name.to_string())
        .spawn(body)
        .map_err(|err| cannot_complete(name, err))
}

#[cfg(test)]
mod tests {
    use super::*;
    use atspi::zbus::names::UniqueName;
    use atspi::zbus::zvariant::ObjectPath;
    use atspi::ObjectRef;

    const BUS_NAME: &str = ":1.42";
    const PATH: &str = "/org/a11y/atspi/accessible/17";

    fn item() -> ObjectRefOwned {
        ObjectRef::new_owned(
            UniqueName::from_static_str_unchecked(BUS_NAME),
            ObjectPath::from_static_str_unchecked(PATH),
        )
    }

    /// Round-trip a real signal message, so the decoders are checked against the
    /// wire format rather than against a hand-built struct. No bus needed.
    fn message(event: impl TryInto<Message, Error = atspi::AtspiError>) -> Message {
        event.try_into().expect("event to signal message")
    }

    #[test]
    fn focus_is_decoded_only_when_an_element_takes_the_focus() {
        let focused = message(StateChangedEvent {
            item: item(),
            state: State::Focused,
            enabled: true,
        });
        assert_eq!(
            decode_focus(&focused),
            Some(ElementId::new(BUS_NAME, PATH)),
            "a focused=true state change is the event this subscription exists for"
        );

        // Losing focus reports no field: the callback carries one, so there is
        // nothing honest to hand the host.
        let unfocused = message(StateChangedEvent {
            item: item(),
            state: State::Focused,
            enabled: false,
        });
        assert_eq!(decode_focus(&unfocused), None);

        // Registering the narrow `:focused` detail does not stop a toolkit from
        // emitting other state changes, so the state itself is still checked.
        let busy = message(StateChangedEvent {
            item: item(),
            state: State::Busy,
            enabled: true,
        });
        assert_eq!(decode_focus(&busy), None);
    }

    #[test]
    fn caret_moves_decode_to_the_element_that_moved() {
        let moved = message(TextCaretMovedEvent {
            item: item(),
            position: 7,
        });
        assert_eq!(decode_caret(&moved), Some(ElementId::new(BUS_NAME, PATH)));
        // A caret event is not a focus event and vice versa: the match rules keep
        // them apart on the bus, and the decoders must agree.
        assert_eq!(decode_focus(&moved), None);
    }

    #[test]
    fn a_null_object_reference_yields_no_element_id() {
        // AT-SPI sends the null reference for "no such object". Encoding it would
        // mint a handle addressing `/org/a11y/atspi/null`, which every later call
        // fails on — with a diagnostic blaming the field instead of the event.
        assert_eq!(element_id(&ObjectRefOwned::new(ObjectRef::Null)), None);
    }

    #[test]
    fn subscription_ids_are_distinct_per_subscription() {
        // Two subscriptions must be separable by id; a shared id would make the
        // host's bookkeeping alias them.
        let first = crate::next_subscription_id();
        let second = crate::next_subscription_id();
        assert!(second > first);
    }

    /// Both kinds of subscription this adapter mints — the accept tap's
    /// (`new_subscription`, used by `subscribe_accept`) and the event
    /// subscriptions' (`new_cancelling_subscription`, used by
    /// [`into_subscription`]) — must draw from ONE counter. Interleaving them
    /// and demanding a strictly increasing sequence is what a second counter in
    /// either constructor would fail: it would restart at 1 and hand the host
    /// an id the other kind had already used.
    #[test]
    fn both_subscription_constructors_draw_from_the_same_counter() {
        let accept = crate::new_subscription();
        let event = crate::new_cancelling_subscription(|| {});
        let accept_again = crate::new_subscription();
        let event_again = crate::new_cancelling_subscription(|| {});

        let ids = [accept.id(), event.id(), accept_again.id(), event_again.id()];
        assert!(
            ids.windows(2).all(|pair| pair[1] > pair[0]),
            "subscription ids must come from one process-wide sequence: {ids:?}"
        );
    }

    /// A44's drop contract, headless: a subscriber still running when the
    /// subscription stops must not park the caller, and the event that was
    /// already queued behind it must never reach the callback.
    ///
    /// Both halves are one test because they are one ordering rule — the gate
    /// closes before the wait starts, so timing out is safe.
    #[test]
    fn stopping_is_bounded_when_a_subscriber_blocks_and_nothing_is_delivered_after_it() {
        let active = Arc::new(AtomicBool::new(true));
        let (event_tx, event_rx) = mpsc::channel();
        let (stopped_tx, stopped_rx) = mpsc::channel();
        // Held by the "subscriber" until the test releases it, standing in for a
        // callback that outlives the stop timeout without sleeping for one.
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let delivered = Arc::new(Mutex::new(Vec::new()));

        let active_for_worker = Arc::clone(&active);
        let sink = Arc::clone(&delivered);
        let worker = thread::spawn(move || {
            dispatch_gated_events(&active_for_worker, &event_rx, None, |element| {
                sink.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(element);
                entered_tx.send(()).expect("the test outlives the worker");
                // Blocks past the stop below, exactly like a subscriber waiting
                // on something slow.
                let _ = release_rx.recv();
            });
            let _ = stopped_tx.send(());
        });

        let first = ElementId::new(BUS_NAME, PATH);
        event_tx.send(first.clone()).expect("queue the first event");
        entered_rx.recv().expect("the subscriber must be entered");
        // Queued while the subscriber is blocked, so it is waiting in the channel
        // when the stop below closes the gate.
        event_tx
            .send(ElementId::new(BUS_NAME, "/org/a11y/atspi/accessible/18"))
            .expect("queue an event behind the blocked subscriber");

        let started = std::time::Instant::now();
        stop_gated_workers(&active, || drop(event_tx), &stopped_rx, vec![worker]);
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "stop must be bounded by STOP_TIMEOUT, took {elapsed:?}"
        );
        assert!(
            elapsed >= STOP_TIMEOUT,
            "this case must be the timeout path, not a clean acknowledgement: {elapsed:?}"
        );

        release_tx.send(()).expect("release the blocked subscriber");
        // The detached worker now drains: the queued event must hit the closed
        // gate rather than the callback.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            *delivered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![first],
            "no event may be delivered after the subscription stopped"
        );
    }

    /// The other half of the same sequence: when the workers do acknowledge,
    /// stop joins them instead of detaching, so the threads are gone by return.
    #[test]
    fn stopping_joins_workers_that_acknowledge_within_the_bound() {
        let active = Arc::new(AtomicBool::new(true));
        let (event_tx, event_rx) = mpsc::channel();
        let (stopped_tx, stopped_rx) = mpsc::channel();
        let finished = Arc::new(AtomicBool::new(false));

        let active_for_worker = Arc::clone(&active);
        let finished_for_worker = Arc::clone(&finished);
        let worker = thread::spawn(move || {
            dispatch_gated_events(&active_for_worker, &event_rx, None, |_| {});
            finished_for_worker.store(true, Ordering::Release);
            let _ = stopped_tx.send(());
        });

        let started = std::time::Instant::now();
        stop_gated_workers(&active, || drop(event_tx), &stopped_rx, vec![worker]);
        assert!(
            started.elapsed() < STOP_TIMEOUT,
            "an acknowledged stop must not wait out the timeout"
        );
        assert!(
            finished.load(Ordering::Acquire),
            "stop must join the worker it waited for"
        );
    }

    #[test]
    fn debug_logging_is_opt_in_and_understands_explicit_off_values() {
        for value in ["", "0", "false", "off", "no"] {
            assert!(
                !debug_flag_on(Some(std::ffi::OsStr::new(value))),
                "{value:?} must keep diagnostics off"
            );
        }
        assert!(debug_flag_on(Some(std::ffi::OsStr::new("1"))));
        assert!(!debug_flag_on(None));
    }
}
