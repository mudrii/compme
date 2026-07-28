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

use crate::atspi_event_map::{latest, FieldMinter};
use crate::atspi_ids::ElementId;
use crate::atspi_live::AtspiSession;
use atspi::events::object::{StateChangedEvent, TextCaretMovedEvent};
use atspi::events::{DBusMatchRule, RegistryEventString};
use atspi::proxy::registry::RegistryProxyBlocking;
use atspi::zbus::blocking::{Connection, MessageIterator};
use atspi::zbus::Message;
use atspi::{ObjectRefOwned, State};
use platform::{CaretCallback, FieldHandle, FocusCallback, PlatformError, Subscription};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
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

/// Subscription ids are only required to be distinct; the counter is process-wide
/// because subscriptions are not owned by any one adapter instance.
static NEXT_SUBSCRIPTION_ID: AtomicU64 = AtomicU64::new(1);

fn cannot_complete(what: &str, err: impl std::fmt::Display) -> PlatformError {
    PlatformError::CannotComplete {
        reason: format!("platform_linux atspi events {what}: {err}"),
    }
}

/// Register for `object:state-changed:focused` and report each element that takes
/// the focus.
pub fn subscribe_focus(cb: FocusCallback) -> Result<Subscription, PlatformError> {
    let mut delivered = None;
    let workers = start(
        FOCUS_REGISTRY_EVENT,
        StateChangedEvent::MATCH_RULE_STRING,
        decode_focus,
        // Focus changes are rare and each one is a distinct field the host must
        // re-probe, so there is no time window to coalesce over — but consecutive
        // *duplicates* are dropped below, which is a different thing.
        None,
        move |session, minter, element| {
            let field = mint(session, minter, &element);
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
pub fn subscribe_caret(cb: CaretCallback) -> Result<Subscription, PlatformError> {
    let workers = start(
        TextCaretMovedEvent::REGISTRY_EVENT_STRING,
        TextCaretMovedEvent::MATCH_RULE_STRING,
        decode_caret,
        Some(CARET_MIN_INTERVAL),
        move |session, minter, element| {
            let field = mint(session, minter, &element);
            // Geometry is best effort by contract: `None` means "no usable rect",
            // which the host already handles by falling back to popup placement. A
            // toolkit that refuses extents must still produce a caret event.
            let rect = session.caret_rect(&field).unwrap_or(None);
            cb(field, rect);
        },
    )?;
    Ok(into_subscription(workers))
}

/// The handle for an event's element, with the `app`/`pid` lookup behind the
/// minter's change check so it costs nothing on repeat events.
fn mint(session: &AtspiSession, minter: &mut FieldMinter, element: &ElementId) -> FieldHandle {
    minter.handle(&element.encode(), || session.element_owner(element))
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
    /// Closed before the connection is, so no callback can fire after
    /// `Subscription::drop` returns even if a worker thread outlives the timeout.
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
        self.active.store(false, Ordering::Release);
        let _ = self.connection.close();
        if self.stopped.recv_timeout(STOP_TIMEOUT).is_err() {
            // Detach rather than park the run loop on a bus that stopped answering.
            // Delivery is already off (the gate above closed first) and the threads
            // own nothing but their own connection clone and the callback, so the
            // cost of a detached one is bounded and invisible to the host.
            return;
        }
        for thread in self.threads {
            let _ = thread.join();
        }
    }
}

fn into_subscription(workers: EventWorkers) -> Subscription {
    let id = NEXT_SUBSCRIPTION_ID.fetch_add(1, Ordering::Relaxed);
    Subscription::with_cancel(id, move || workers.stop())
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
    F: FnMut(&AtspiSession, &mut FieldMinter, ElementId) + Send + 'static,
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
        let mut minter = FieldMinter::new();
        while let Ok(element) = event_rx.recv() {
            if !active_for_dispatch.load(Ordering::Acquire) {
                break;
            }
            let element = match coalesce {
                Some(_) => latest(element, &event_rx),
                None => element,
            };
            // A panicking subscriber must not take the subscription with it: the
            // contract lets callbacks run on an adapter-internal thread, and
            // unwinding out of one would silently end delivery for every later event.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                deliver(&session, &mut minter, element);
            }));
            if let Some(interval) = coalesce {
                thread::sleep(interval);
            }
        }
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
        let first = NEXT_SUBSCRIPTION_ID.fetch_add(1, Ordering::Relaxed);
        let second = NEXT_SUBSCRIPTION_ID.fetch_add(1, Ordering::Relaxed);
        assert!(second > first);
    }
}
