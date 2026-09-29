//! Portable deterministic regression suite for the host-neutral Carbon arm
//! registry core (G7). The REAL production module
//! `crates/platform_macos/src/carbon_registry.rs` is included verbatim via
//! `#[path]` — no copied test-only state machine. Native operations are the
//! injected `RecordingOps` fake (ordered op log, failure injection,
//! process-global chord-collision emulation). Sequencing is fully explicit:
//! registry calls are synchronous and teardown is driven by direct
//! retire/shutdown calls, so there are no sleeps and no threads except the
//! one spawned in
//! `off_main_consumer_install_is_rejected_with_zero_native_or_slot_effects` —
//! a LOGIC test of the injected-predicate guard, not an OS-affinity proof (a
//! serial Rust test thread is never the OS main thread, so main identity must
//! be simulated by the seam per the G7 regression-scenario corrections).

#[allow(dead_code)]
#[path = "../../platform_macos/src/carbon_registry.rs"]
mod carbon_registry;

use carbon_registry::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Carbon-shaped nonzero status for injected handler-install failure.
const HANDLER_FAIL_STATUS: i32 = -50;
/// Carbon-shaped nonzero status for injected register failure.
const REGISTER_FAIL_STATUS: i32 = -42;
/// The real Carbon `eventHotKeyExistsErr`: registering an already-registered
/// (keycode, mask) chord fails regardless of hotkey id — chords are
/// process-global, which is exactly why replacement must drain old refs
/// before the first new register.
const DUPLICATE_CHORD_STATUS: i32 = -9878;

/// Injected main-thread identity, flippable mid-test for the teardown
/// rejection case. A plain predicate: these tests validate the guard's
/// branching, not Carbon's real OS thread affinity.
#[derive(Clone, Debug)]
struct MainFlag(Arc<AtomicBool>);

impl MainFlag {
    fn on() -> Self {
        MainFlag(Arc::new(AtomicBool::new(true)))
    }

    fn set(&self, on: bool) {
        self.0.store(on, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    InstallHandler,
    Register { family: Family, binding: KeyBinding },
    Unregister { family: Family, binding: KeyBinding },
}

/// Unified ordered timeline: native ops AND slot events in one log, so an
/// assertion can prove interleaving (e.g. `Cleared(old)` strictly before the
/// first new register) instead of comparing two independent logs.
#[derive(Debug, Clone)]
enum TimelineEvent {
    Op(Op),
    Slot(SlotEvent),
}

type Timeline = std::sync::Arc<std::sync::Mutex<Vec<TimelineEvent>>>;

/// The injected native leaf: records every operation in order, emulates
/// process-global chord uniqueness (any live (keycode, mask) chord collides
/// with a new register no matter the id or family), and fails selected
/// 1-based call attempts.
#[derive(Debug)]
struct RecordingOps {
    main: MainFlag,
    log: Vec<Op>,
    timeline: Timeline,
    live: Vec<(Family, KeyBinding)>,
    chords: Vec<(i64, u32)>,
    tokens: Vec<(u64, Family, KeyBinding)>,
    next_token: u64,
    handler_attempts: usize,
    register_attempts: usize,
    fail_handler_attempts: Vec<usize>,
    fail_register_attempts: Vec<usize>,
}

impl RecordingOps {
    fn on_main() -> Self {
        RecordingOps::on_main_with(std::sync::Arc::new(std::sync::Mutex::new(Vec::new())))
    }

    fn on_main_with(timeline: Timeline) -> Self {
        RecordingOps {
            main: MainFlag::on(),
            log: Vec::new(),
            timeline,
            live: Vec::new(),
            chords: Vec::new(),
            tokens: Vec::new(),
            next_token: 0,
            handler_attempts: 0,
            register_attempts: 0,
            fail_handler_attempts: Vec::new(),
            fail_register_attempts: Vec::new(),
        }
    }

    fn record_timeline(&self, event: TimelineEvent) {
        self.timeline.lock().expect("timeline lock").push(event);
    }

    fn register_indexes(&self) -> Vec<usize> {
        self.log
            .iter()
            .enumerate()
            .filter(|(_, op)| matches!(op, Op::Register { .. }))
            .map(|(i, _)| i)
            .collect()
    }

    fn unregister_indexes(&self) -> Vec<usize> {
        self.log
            .iter()
            .enumerate()
            .filter(|(_, op)| matches!(op, Op::Unregister { .. }))
            .map(|(i, _)| i)
            .collect()
    }

    fn unregisters(&self) -> Vec<(Family, KeyBinding)> {
        self.log
            .iter()
            .filter_map(|op| match op {
                Op::Unregister { family, binding } => Some((*family, *binding)),
                _ => None,
            })
            .collect()
    }

    fn registers(&self) -> Vec<(Family, KeyBinding)> {
        self.log
            .iter()
            .filter_map(|op| match op {
                Op::Register { family, binding } => Some((*family, *binding)),
                _ => None,
            })
            .collect()
    }

    fn live_of(&self, family: Family) -> Vec<KeyBinding> {
        self.live
            .iter()
            .filter(|(f, _)| *f == family)
            .map(|(_, b)| *b)
            .collect()
    }

    fn handler_install_count(&self) -> usize {
        self.log
            .iter()
            .filter(|op| matches!(op, Op::InstallHandler))
            .count()
    }

    /// Fail the Nth register call AFTER this point (1-based global attempt
    /// number counted from now).
    fn fail_register_ordinal_from_now(&mut self, ordinal: usize) {
        self.fail_register_attempts
            .push(self.register_attempts + ordinal);
    }

    /// Fail the NEXT handler-install call.
    fn fail_next_handler_install(&mut self) {
        self.fail_handler_attempts.push(self.handler_attempts + 1);
    }
}

impl NativeOps for RecordingOps {
    type Token = u64;

    fn is_main_thread(&self) -> bool {
        self.main.0.load(Ordering::SeqCst)
    }

    fn install_handler(&mut self) -> Result<(), RegistryError> {
        self.handler_attempts += 1;
        self.log.push(Op::InstallHandler);
        self.record_timeline(TimelineEvent::Op(Op::InstallHandler));
        if self.fail_handler_attempts.contains(&self.handler_attempts) {
            Err(RegistryError::HandlerInstall {
                status: HANDLER_FAIL_STATUS,
            })
        } else {
            Ok(())
        }
    }

    fn register(
        &mut self,
        family: Family,
        binding: &KeyBinding,
    ) -> Result<Self::Token, RegistryError> {
        self.register_attempts += 1;
        // Carbon reality first: a live (keycode, mask) chord collides no
        // matter which hotkey id or family re-registers it.
        if self.chords.contains(&(binding.keycode, binding.mask)) {
            return Err(RegistryError::Register {
                family,
                binding: *binding,
                status: DUPLICATE_CHORD_STATUS,
            });
        }
        self.log.push(Op::Register {
            family,
            binding: *binding,
        });
        self.record_timeline(TimelineEvent::Op(Op::Register {
            family,
            binding: *binding,
        }));
        if self
            .fail_register_attempts
            .contains(&self.register_attempts)
        {
            return Err(RegistryError::Register {
                family,
                binding: *binding,
                status: REGISTER_FAIL_STATUS,
            });
        }
        self.next_token += 1;
        let token = self.next_token;
        self.chords.push((binding.keycode, binding.mask));
        self.live.push((family, *binding));
        self.tokens.push((token, family, *binding));
        Ok(token)
    }

    fn unregister(&mut self, family: Family, token: Self::Token) {
        let pos = self
            .tokens
            .iter()
            .position(|(t, f, _)| *t == token && *f == family)
            .expect("unregister of a token this ops never issued");
        let (_, family, binding) = self.tokens.remove(pos);
        self.log.push(Op::Unregister { family, binding });
        self.record_timeline(TimelineEvent::Op(Op::Unregister { family, binding }));
        self.live.retain(|(f, b)| !(*f == family && *b == binding));
        self.chords
            .retain(|c| *c != (binding.keycode, binding.mask));
    }
}

/// Shared ordered timeline handle: hooks and native ops both append here, so
/// tests can assert exact interleaving across slot and native events.
#[derive(Clone, Default)]
struct EventLog(Timeline);

impl EventLog {
    fn slot_snapshot(&self) -> Vec<SlotEvent> {
        self.0
            .lock()
            .expect("timeline lock")
            .iter()
            .filter_map(|e| match e {
                TimelineEvent::Slot(s) => Some(*s),
                _ => None,
            })
            .collect()
    }
}

struct RecordingHooks {
    log: EventLog,
}

impl SlotHooks for RecordingHooks {
    fn slot_event(&mut self, event: SlotEvent) {
        self.log
            .0
            .lock()
            .expect("timeline lock")
            .push(TimelineEvent::Slot(event));
    }
}

fn registry_with_event_log() -> (CarbonRegistry<RecordingOps>, EventLog) {
    let log = EventLog::default();
    let registry = CarbonRegistry::with_hooks(
        RecordingOps::on_main_with(log.0.clone()),
        Box::new(RecordingHooks { log: log.clone() }),
    );
    (registry, log)
}

/// The four consumer accept keys (ids 1-4), mirroring the production default
/// chords: Tab(48), Grave(50), Escape(53), Down(125).
fn consumer_plan() -> ArmPlan {
    ArmPlan::from_bindings([b(1, 48, 0), b(2, 50, 0), b(3, 53, 0), b(4, 125, 0)])
}

/// The always-on process shortcuts (ids 5-8) as an explicit configured set.
fn shortcut_plan() -> ArmPlan {
    ArmPlan::from_bindings([b(5, 96, 256), b(6, 97, 256), b(7, 98, 256), b(8, 99, 256)])
}

/// The grammar/correction accept arm (id 9) — a CONSUMER-family arm in
/// production (`AcceptAction::Correction`).
fn grammar_plan() -> ArmPlan {
    ArmPlan::from_bindings([b(9, 13, 0)])
}

fn b(id: u32, keycode: i64, mask: u32) -> KeyBinding {
    KeyBinding { id, keycode, mask }
}

fn plan_of(bindings: &[KeyBinding]) -> ArmPlan {
    ArmPlan::from_bindings(bindings.iter().copied())
}

fn as_register(op: &Op) -> Option<(Family, KeyBinding)> {
    match op {
        Op::Register { family, binding } => Some((*family, *binding)),
        _ => None,
    }
}

// --- 1. Main registration is inline and complete -------------------------

#[test]
fn consumer_arm_registers_inline_on_main_with_the_full_plan_in_order() {
    let mut registry = CarbonRegistry::new(RecordingOps::on_main());
    let owner = allocate_owner_id();
    let outcome = registry
        .install_consumer_arm(owner, consumer_plan())
        .expect("inline main-thread install succeeds");
    assert_eq!(outcome.registered, 4);
    assert!(outcome.skipped.is_empty());
    assert_eq!(
        registry.armed_arm(Family::Consumer),
        Some((owner, outcome.arm))
    );
    assert_eq!(registry.armed_arm(Family::Shortcut), None);
    let ops = registry.ops();
    // Inline and complete: one handler install, the full plan in plan order,
    // no teardown, exactly the plan's refs live.
    assert_eq!(ops.handler_install_count(), 1);
    let expected: Vec<(Family, KeyBinding)> = consumer_plan()
        .bindings
        .iter()
        .map(|b| (Family::Consumer, *b))
        .collect();
    assert_eq!(ops.registers(), expected);
    assert!(ops.unregisters().is_empty());
    assert_eq!(ops.live_of(Family::Consumer), consumer_plan().bindings);
}

// --- 2. Off-main typed rejection with zero effects ------------------------

#[test]
fn off_main_consumer_install_is_rejected_with_zero_native_or_slot_effects() {
    // LOGIC test of the guard only: a plain spawned thread whose injected
    // predicate returns false exercises the off-main branch deterministically.
    // This is NOT an OS-affinity proof — Carbon's real main-thread check is a
    // separate native-integration concern on macOS.
    let ops = RecordingOps::on_main();
    ops.main.set(false);
    let handle = std::thread::spawn(move || {
        let mut registry = CarbonRegistry::new(ops);
        let consumer = registry.install_consumer_arm(allocate_owner_id(), consumer_plan());
        let shortcut = registry.install_shortcut_arm(allocate_owner_id(), shortcut_plan());
        let consumer_armed = registry.armed_arm(Family::Consumer);
        let shortcut_armed = registry.armed_arm(Family::Shortcut);
        let ops = registry.into_ops();
        (consumer, shortcut, consumer_armed, shortcut_armed, ops)
    });
    let (consumer, shortcut, consumer_armed, shortcut_armed, ops) = handle.join().expect("join");
    assert_eq!(consumer, Err(RegistryError::NotOnMainThread));
    assert_eq!(shortcut, Err(RegistryError::NotOnMainThread));
    assert_eq!(consumer_armed, None);
    assert_eq!(shortcut_armed, None);
    // Zero side effects: not even one native operation was attempted — no
    // handler install, no drain, no register, no slot event.
    assert!(
        ops.log.is_empty(),
        "off-main rejection leaked ops: {:?}",
        ops.log
    );
    assert!(ops.live.is_empty());
    assert!(ops.chords.is_empty());
}

// --- 3. Replacement ordering: all old unregisters precede the first new register

#[test]
fn consumer_replacement_unregisters_every_old_ref_before_the_first_new_register() {
    let (mut registry, log) = registry_with_event_log();
    let owner = allocate_owner_id();
    let first = registry
        .install_consumer_arm(owner, consumer_plan())
        .expect("first arm");
    // Capture the timeline boundary BEFORE the replacement: the first new
    // register is then located independently of where unregisters land, so
    // the ordering assertion below is not tautological.
    let boundary = log.0.lock().expect("timeline lock").len();
    // Rearm with a plan sharing the Esc(53)/Down(125) chords: the replacement
    // must drain ALL old refs first, or the shared-chord re-registers would
    // fail with the duplicate-chord status (-9878) below.
    let second = registry
        .install_consumer_arm(owner, consumer_plan())
        .expect("replacement arm succeeds");
    assert!(second.arm.0 > first.arm.0, "arm ids are monotonic");
    let timeline = log.0.lock().expect("timeline lock");
    let first_new_register = timeline[boundary..]
        .iter()
        .position(|e| matches!(e, TimelineEvent::Op(Op::Register { .. })))
        .expect("replacement registers exist")
        + boundary;
    let post_boundary_unregisters: Vec<usize> = (boundary..timeline.len())
        .filter(|i| matches!(timeline[*i], TimelineEvent::Op(Op::Unregister { .. })))
        .collect();
    assert_eq!(
        post_boundary_unregisters.len(),
        4,
        "exactly the four old refs drained"
    );
    // EVERY old unregister strictly precedes the FIRST new register, where
    // "first new register" comes from the pre-captured boundary, not from the
    // last unregister.
    assert!(
        post_boundary_unregisters
            .iter()
            .all(|i| *i < first_new_register),
        "an old unregister happened after the first new register"
    );
    drop(timeline);
    let ops = registry.ops();
    let unregister_idx = ops.unregister_indexes();
    assert_eq!(unregister_idx.len(), 4, "exactly the four old refs drained");
    // The drained refs are exactly the old plan's, each once.
    assert_eq!(ops.unregisters().len(), 4);
    for binding in consumer_plan().bindings {
        assert_eq!(
            ops.unregisters()
                .iter()
                .filter(|(_, b)| *b == binding)
                .count(),
            1,
            "old ref for {binding:?} unregistered exactly once"
        );
    }
    // The replacement registered its FULL set (shared chords included), i.e.
    // every register from the pre-captured boundary onward.
    let timeline = log.0.lock().expect("timeline lock");
    let new_registers: Vec<(Family, KeyBinding)> = timeline[boundary..]
        .iter()
        .filter_map(|e| match e {
            TimelineEvent::Op(Op::Register { family, binding }) => Some((*family, *binding)),
            _ => None,
        })
        .collect();
    drop(timeline);
    let expected: Vec<(Family, KeyBinding)> = consumer_plan()
        .bindings
        .iter()
        .map(|b| (Family::Consumer, *b))
        .collect();
    assert_eq!(new_registers, expected);
    assert_eq!(ops.live_of(Family::Consumer), consumer_plan().bindings);
}

// --- 4. Consumer replacement never touches the shortcut family ------------

#[test]
fn consumer_replacement_leaves_the_shortcut_family_untouched() {
    let mut registry = CarbonRegistry::new(RecordingOps::on_main());
    let shortcut_owner = allocate_owner_id();
    let shortcut_arm = registry
        .install_shortcut_arm(shortcut_owner, shortcut_plan())
        .expect("shortcut arm");
    let consumer_owner = allocate_owner_id();
    registry
        .install_consumer_arm(consumer_owner, consumer_plan())
        .expect("first consumer arm");
    registry
        .install_consumer_arm(consumer_owner, plan_of(&[b(11, 70, 0), b(12, 71, 0)]))
        .expect("replacement consumer arm");
    let ops = registry.ops();
    // Not one unregister belongs to the shortcut family.
    assert!(
        ops.unregisters()
            .iter()
            .all(|(f, _)| *f == Family::Consumer),
        "shortcut refs were touched by a consumer replacement: {:?}",
        ops.unregisters()
    );
    assert_eq!(ops.live_of(Family::Shortcut), shortcut_plan().bindings);
    assert_eq!(
        registry.armed_arm(Family::Shortcut),
        Some((shortcut_owner, shortcut_arm.arm))
    );
}

// --- 5. Consumer partial failure: rollback exactly the owned successes ----

#[test]
fn consumer_nth_register_failure_rolls_back_exactly_the_owned_successes_and_stays_retryable() {
    let (mut registry, log) = registry_with_event_log();
    let shortcut_owner = allocate_owner_id();
    let shortcut_outcome = registry
        .install_shortcut_arm(shortcut_owner, plan_of(&[b(5, 96, 256), b(6, 97, 256)]))
        .expect("shortcut arm");
    let first_owner = allocate_owner_id();
    let first_arm = registry
        .install_consumer_arm(first_owner, plan_of(&[b(1, 60, 0)]))
        .expect("first consumer arm")
        .arm;
    // Fail the 3rd register of the NEXT consumer install (its plan has 4).
    registry.ops_mut().fail_register_ordinal_from_now(3);

    let second_owner = allocate_owner_id();
    let second_plan = plan_of(&[b(11, 70, 0), b(12, 71, 0), b(13, 72, 0), b(14, 73, 0)]);
    let err = registry
        .install_consumer_arm(second_owner, second_plan.clone())
        .expect_err("3rd register of the new arm fails");
    assert_eq!(
        err,
        RegistryError::Register {
            family: Family::Consumer,
            binding: b(13, 72, 0),
            status: REGISTER_FAIL_STATUS,
        }
    );
    let ops = registry.ops();
    // Exactly three unregisters: the drained predecessor's single ref (before
    // the first new register) plus ONLY this arm's two owned successes.
    let expected_unregisters = vec![
        (Family::Consumer, b(1, 60, 0)),
        (Family::Consumer, b(11, 70, 0)),
        (Family::Consumer, b(12, 71, 0)),
    ];
    assert_eq!(ops.unregisters(), expected_unregisters);
    let unregister_idx = ops.unregister_indexes();
    let register_idx = ops.register_indexes();
    // Predecessor drain preceded the new registers; rollback followed the
    // owned successes; nothing foreign (shortcut refs, already-drained refs)
    // was unregistered.
    assert!(unregister_idx[0] < register_idx[3]);
    assert!(unregister_idx[1] > register_idx[4]);
    assert!(unregister_idx[2] > unregister_idx[1]);
    assert_eq!(unregister_idx.len(), 3);
    // No active consumer slot; the shortcut arm is completely untouched.
    assert_eq!(registry.armed_arm(Family::Consumer), None);
    assert_eq!(ops.live_of(Family::Consumer), Vec::<KeyBinding>::new());
    assert_eq!(
        ops.live_of(Family::Shortcut),
        vec![b(5, 96, 256), b(6, 97, 256)]
    );
    assert_eq!(
        registry.armed_arm(Family::Shortcut),
        Some((shortcut_owner, shortcut_outcome.arm))
    );
    // A failed replacement never publishes its slot: exactly the shortcut
    // arm's publish, the first consumer arm's publish, and the drain's clear
    // of that first arm appear — the failed arm itself emits no slot event,
    // leaving the consumer slot empty ("never armed over zero live keys").
    assert_eq!(
        log.slot_snapshot(),
        vec![
            SlotEvent::Published {
                family: Family::Shortcut,
                arm: shortcut_outcome.arm,
            },
            SlotEvent::Published {
                family: Family::Consumer,
                arm: first_arm,
            },
            SlotEvent::Cleared {
                family: Family::Consumer,
                arm: first_arm,
            },
        ]
    );

    // Retryable: the next install re-registers the full plan and succeeds.
    let retry = registry
        .install_consumer_arm(second_owner, second_plan.clone())
        .expect("retry after rollback succeeds");
    assert_eq!(retry.registered, 4);
    assert!(
        retry.arm.0 > first_arm.0,
        "monotonic arm ids across installs"
    );
    let ops = registry.ops();
    assert_eq!(ops.live_of(Family::Consumer), second_plan.bindings);
    assert_eq!(
        registry.armed_arm(Family::Consumer),
        Some((second_owner, retry.arm))
    );
}

// --- 6. Handler-install failure propagates; retry installs ----------------

#[test]
fn handler_install_failure_propagates_without_effects_and_a_later_retry_installs_and_replaces() {
    let mut registry = CarbonRegistry::new(RecordingOps::on_main());
    let owner = allocate_owner_id();
    let first = registry
        .install_consumer_arm(owner, plan_of(&[b(1, 60, 0)]))
        .expect("first arm");
    registry.ops_mut().fail_next_handler_install();
    let ops_after_first = registry.ops().log.len();

    let err = registry
        .install_consumer_arm(owner, consumer_plan())
        .expect_err("handler install failure propagates");
    assert_eq!(
        err,
        RegistryError::HandlerInstall {
            status: HANDLER_FAIL_STATUS,
        }
    );
    let ops = registry.ops();
    // The failed attempt appended exactly ONE op (the install_handler call)
    // and nothing else: no drain, no register, no slot change.
    assert_eq!(ops.log.len(), ops_after_first + 1);
    assert!(matches!(ops.log.last(), Some(Op::InstallHandler)));
    // The predecessor survives untouched (handler install precedes drain).
    assert_eq!(ops.live_of(Family::Consumer), vec![b(1, 60, 0)]);
    assert_eq!(
        registry.armed_arm(Family::Consumer),
        Some((owner, first.arm))
    );

    // Retryable: the later attempt re-invokes install_handler (no Once),
    // drains the predecessor first, and registers the full plan.
    let second = registry
        .install_consumer_arm(owner, consumer_plan())
        .expect("retry succeeds");
    let ops = registry.ops();
    assert_eq!(ops.handler_install_count(), 3, "install_handler re-invoked");
    let unregister_idx = ops.unregister_indexes();
    assert_eq!(unregister_idx.len(), 1, "only the drained predecessor");
    let register_idx = ops.register_indexes();
    let first_new = register_idx[1]; // second install's first register
    assert!(
        unregister_idx[0] < first_new,
        "drain precedes new registers"
    );
    assert_eq!(ops.live_of(Family::Consumer), consumer_plan().bindings);
    assert_eq!(
        registry.armed_arm(Family::Consumer),
        Some((owner, second.arm))
    );
}

// --- 7. Shortcut per-key log-and-skip -------------------------------------

#[test]
fn shortcut_arm_keeps_successful_keys_and_skips_only_the_failed_binding() {
    let mut registry = CarbonRegistry::new(RecordingOps::on_main());
    let owner = allocate_owner_id();
    registry.ops_mut().fail_register_ordinal_from_now(2);
    let outcome = registry
        .install_shortcut_arm(owner, shortcut_plan())
        .expect("per-key skip never fails the shortcut install");
    assert_eq!(outcome.registered, 3);
    assert_eq!(outcome.skipped, vec![b(6, 97, 256)]);
    let ops = registry.ops();
    // The op log records ATTEMPTS (the failed id-6 register included); the
    // live set records only the three survivors.
    let attempts: Vec<KeyBinding> = ops.registers().into_iter().map(|(_, b)| b).collect();
    assert_eq!(
        attempts,
        vec![b(5, 96, 256), b(6, 97, 256), b(7, 98, 256), b(8, 99, 256)]
    );
    assert!(ops.unregisters().is_empty());
    assert_eq!(
        ops.live_of(Family::Shortcut),
        vec![b(5, 96, 256), b(7, 98, 256), b(8, 99, 256)]
    );
    assert_eq!(
        registry.armed_arm(Family::Shortcut),
        Some((owner, outcome.arm))
    );
}

// --- 8. Consumer teardown: exactly-once unregister, shortcuts preserved ---

#[test]
fn consumer_teardown_unregisters_each_owned_ref_exactly_once_and_preserves_shortcuts() {
    let (mut registry, _log) = registry_with_event_log();
    let owner = allocate_owner_id();
    let consumer = registry
        .install_consumer_arm(owner, consumer_plan())
        .expect("consumer arm");
    let shortcut = registry
        .install_shortcut_arm(owner, shortcut_plan())
        .expect("shortcut arm");

    let retired = registry
        .retire(owner, consumer.arm, Family::Consumer)
        .expect("main-thread retire");
    assert!(retired);
    let ops = registry.ops();
    let expected: Vec<(Family, KeyBinding)> = consumer_plan()
        .bindings
        .iter()
        .map(|b| (Family::Consumer, *b))
        .collect();
    assert_eq!(ops.unregisters(), expected, "each owned ref exactly once");
    assert_eq!(ops.live_of(Family::Shortcut), shortcut_plan().bindings);
    assert_eq!(
        registry.armed_arm(Family::Consumer),
        None,
        "consumer slot disarmed"
    );
    assert_eq!(
        registry.armed_arm(Family::Shortcut),
        Some((owner, shortcut.arm)),
        "shortcut slot preserved"
    );

    // Reverse leg: shortcut teardown unregisters exactly its own refs.
    let retired = registry
        .retire(owner, shortcut.arm, Family::Shortcut)
        .expect("main-thread retire");
    assert!(retired);
    let expected: Vec<(Family, KeyBinding)> = shortcut_plan()
        .bindings
        .iter()
        .map(|b| (Family::Shortcut, *b))
        .collect();
    let ops = registry.ops();
    let mut all = consumer_unregisters(ops);
    all.extend(expected);
    assert_eq!(ops.unregisters(), all);
    assert!(ops.live.is_empty());
    assert_eq!(registry.armed_arm(Family::Shortcut), None);
}

fn consumer_unregisters(ops: &RecordingOps) -> Vec<(Family, KeyBinding)> {
    ops.unregisters()
        .into_iter()
        .filter(|(f, _)| *f == Family::Consumer)
        .collect()
}

// --- 9. Retire matches owner+arm+family; everything else is a no-op -------

#[test]
fn retire_matches_owner_arm_and_family_and_ignores_stale_duplicate_and_foreign_requests() {
    let mut registry = CarbonRegistry::new(RecordingOps::on_main());
    let owner = allocate_owner_id();
    let consumer = registry
        .install_consumer_arm(owner, consumer_plan())
        .expect("consumer arm");
    registry
        .install_shortcut_arm(owner, shortcut_plan())
        .expect("shortcut arm");
    let ops_after_install = registry.ops().log.len();

    // Foreign FAMILY: consumer arm retired via the shortcut family is a no-op.
    assert_eq!(
        registry.retire(owner, consumer.arm, Family::Shortcut),
        Ok(false)
    );
    assert_eq!(registry.ops().log.len(), ops_after_install);
    // Foreign OWNER: another owner's id cannot retire this arm.
    let foreign_owner = allocate_owner_id();
    assert_eq!(
        registry.retire(foreign_owner, consumer.arm, Family::Consumer),
        Ok(false)
    );
    assert_eq!(registry.ops().log.len(), ops_after_install);
    // Wrong ARM id: a stale/unknown id is a no-op.
    assert_eq!(
        registry.retire(owner, ArmId(consumer.arm.0 + 1_000), Family::Consumer),
        Ok(false)
    );
    assert_eq!(registry.ops().log.len(), ops_after_install);

    // The exact (owner, arm, family) match retires and cleans up.
    assert_eq!(
        registry.retire(owner, consumer.arm, Family::Consumer),
        Ok(true)
    );
    assert_eq!(registry.ops().unregisters().len(), 4);
    assert_eq!(registry.armed_arm(Family::Consumer), None);

    // Duplicate request after a successful retire: no-op, zero new ops.
    let ops_len = registry.ops().log.len();
    assert_eq!(
        registry.retire(owner, consumer.arm, Family::Consumer),
        Ok(false)
    );
    assert_eq!(registry.ops().log.len(), ops_len);
}

// --- 10. Old-owner/old-arm teardown cannot clear the newer arm ------------

#[test]
fn retire_of_a_replaced_arm_is_a_stale_no_op_that_cannot_clear_the_newer_arm() {
    let (mut registry, log) = registry_with_event_log();
    let owner = allocate_owner_id();
    let first = registry
        .install_consumer_arm(owner, consumer_plan())
        .expect("first arm");
    let second = registry
        .install_consumer_arm(owner, plan_of(&[b(21, 80, 0), b(22, 81, 0)]))
        .expect("replacement arm");
    let ops_len_at_replacement = registry.ops().log.len();

    // A late teardown of the REPLACED arm id is a stale no-op.
    assert_eq!(
        registry.retire(owner, first.arm, Family::Consumer),
        Ok(false)
    );
    let ops = registry.ops();
    assert_eq!(ops.log.len(), ops_len_at_replacement, "zero new ops");
    // Only the replacement's drain unregisters exist (the four old refs).
    assert_eq!(ops.unregisters().len(), 4);
    assert_eq!(
        registry.armed_arm(Family::Consumer),
        Some((owner, second.arm)),
        "the newer arm stays armed and live"
    );
    assert_eq!(
        ops.live_of(Family::Consumer),
        vec![b(21, 80, 0), b(22, 81, 0)]
    );
    // No Cleared was emitted for the newer arm by the stale teardown.
    let events = log.slot_snapshot();
    assert!(!events.contains(&SlotEvent::Cleared {
        family: Family::Consumer,
        arm: second.arm,
    }));
    assert!(events.contains(&SlotEvent::Cleared {
        family: Family::Consumer,
        arm: first.arm,
    }));
}

// --- 11. Zero-key plans still arm a slot -----------------------------------

#[test]
fn zero_key_plans_still_arm_their_slot_with_zero_registrations() {
    let (mut registry, log) = registry_with_event_log();
    let owner = allocate_owner_id();

    let consumer = registry
        .install_consumer_arm(owner, ArmPlan::default())
        .expect("zero-key consumer arm");
    let shortcut = registry
        .install_shortcut_arm(owner, ArmPlan::default())
        .expect("zero-key shortcut arm");
    assert_eq!(consumer.registered, 0);
    assert_eq!(shortcut.registered, 0);
    let ops = registry.ops();
    assert_eq!(
        ops.registers().len(),
        0,
        "zero-key means zero registrations"
    );
    assert!(ops.unregisters().is_empty());
    assert_eq!(
        ops.handler_install_count(),
        2,
        "handler still installed per arm attempt"
    );
    // Both slots ARE armed — a live zero-token entry is an armed slot, which
    // is what distinguishes an armed registration from "no operation ran".
    assert_eq!(
        registry.armed_arm(Family::Consumer),
        Some((owner, consumer.arm))
    );
    assert_eq!(
        registry.armed_arm(Family::Shortcut),
        Some((owner, shortcut.arm))
    );

    // Retiring a zero-key arm cleans its slot with zero native unregisters.
    assert_eq!(
        registry.retire(owner, consumer.arm, Family::Consumer),
        Ok(true)
    );
    assert_eq!(registry.ops().unregisters().len(), 0);
    assert_eq!(registry.armed_arm(Family::Consumer), None);
    let events = log.slot_snapshot();
    assert_eq!(
        events,
        vec![
            SlotEvent::Published {
                family: Family::Consumer,
                arm: consumer.arm,
            },
            SlotEvent::Published {
                family: Family::Shortcut,
                arm: shortcut.arm,
            },
            SlotEvent::Cleared {
                family: Family::Consumer,
                arm: consumer.arm,
            },
        ]
    );
}

// --- 12. shutdown_owner drains only its owner ------------------------------

#[test]
fn shutdown_owner_drains_only_that_owners_entries_across_both_families() {
    let (mut registry, log) = registry_with_event_log();
    let owner_a = allocate_owner_id();
    let owner_b = allocate_owner_id();
    // Owner A holds both families; owner B replaces A's CONSUMER arm (family
    // slots hold one live arm, so B's install drains A's consumer), leaving
    // A's shortcut and B's consumer live simultaneously — two owners.
    let a_shortcut = registry
        .install_shortcut_arm(owner_a, plan_of(&[b(5, 96, 256), b(6, 97, 256)]))
        .expect("A shortcut");
    let b_consumer = registry
        .install_consumer_arm(owner_b, plan_of(&[b(21, 80, 0), b(22, 81, 0)]))
        .expect("B consumer replaces A's consumer");
    let ops_len_at_setup = registry.ops().log.len();

    let drained = registry
        .shutdown_owner(owner_a)
        .expect("main-thread shutdown");
    assert_eq!(drained, 1, "only A's surviving shortcut arm");
    let ops = registry.ops();
    // Exactly A's two shortcut refs, each once; not one of B's bindings
    // touched — dropping an old owner can never clear newer registrations.
    let expected = vec![
        (Family::Shortcut, b(5, 96, 256)),
        (Family::Shortcut, b(6, 97, 256)),
    ];
    assert_eq!(ops.unregisters(), expected);
    assert_eq!(ops.live_of(Family::Shortcut), Vec::<KeyBinding>::new());
    assert_eq!(
        ops.live_of(Family::Consumer),
        vec![b(21, 80, 0), b(22, 81, 0)]
    );
    assert_eq!(
        registry.armed_arm(Family::Consumer),
        Some((owner_b, b_consumer.arm)),
        "B's consumer slot survives A's shutdown"
    );
    assert_eq!(registry.armed_arm(Family::Shortcut), None);
    let events = log.slot_snapshot();
    assert!(events.contains(&SlotEvent::Cleared {
        family: Family::Shortcut,
        arm: a_shortcut.arm,
    }));
    assert!(!events.contains(&SlotEvent::Cleared {
        family: Family::Consumer,
        arm: b_consumer.arm,
    }));
    assert_eq!(ops.log.len(), ops_len_at_setup + 2);

    // A second sweep of the drained owner is a harmless no-op (its consumer
    // arm was already replaced by B's — a stale owner-sweep touches nothing).
    assert_eq!(registry.shutdown_owner(owner_a), Ok(0));
    assert_eq!(registry.ops().log.len(), ops_len_at_setup + 2);

    // The surviving owner still drains fully.
    assert_eq!(registry.shutdown_owner(owner_b), Ok(1));
    let ops = registry.ops();
    assert!(ops.live.is_empty());
    assert_eq!(registry.armed_arm(Family::Consumer), None);
    assert_eq!(registry.armed_arm(Family::Shortcut), None);
}

// --- 13. Off-main teardown is typed-rejected without effects ----------------

#[test]
fn off_main_teardown_requests_are_rejected_without_native_effects() {
    let mut registry = CarbonRegistry::new(RecordingOps::on_main());
    let main_flag = registry.ops().main.clone();
    let owner = allocate_owner_id();
    let consumer = registry
        .install_consumer_arm(owner, consumer_plan())
        .expect("arm");
    let ops_len = registry.ops().log.len();
    main_flag.set(false);
    // The core's teardown is inline-on-main; the native layer posts the
    // ID-scoped async request for off-main drops — the core only ever runs
    // retire/shutdown inline, so off-main calls here are typed rejections.
    assert_eq!(
        registry.retire(owner, consumer.arm, Family::Consumer),
        Err(RegistryError::NotOnMainThread)
    );
    assert_eq!(
        registry.shutdown_owner(owner),
        Err(RegistryError::NotOnMainThread)
    );
    let ops = registry.ops();
    assert_eq!(ops.log.len(), ops_len, "no native op ran");
    assert_eq!(
        registry.armed_arm(Family::Consumer),
        Some((owner, consumer.arm))
    );
    assert_eq!(ops.live_of(Family::Consumer), consumer_plan().bindings);
}

// --- 14. Owner and arm ids are globally monotonic ---------------------------

#[test]
fn owner_and_arm_ids_are_globally_monotonic_across_adapter_lifetimes() {
    let o1 = allocate_owner_id();
    let o2 = allocate_owner_id();
    assert!(o2 > o1, "owner ids are monotonic");
    let pre = allocate_arm_id();

    let mut registry_a = CarbonRegistry::new(RecordingOps::on_main());
    let mut registry_b = CarbonRegistry::new(RecordingOps::on_main());
    let arm_a1 = registry_a
        .install_consumer_arm(o1, plan_of(&[b(1, 60, 0)]))
        .expect("arm in registry A")
        .arm;
    let arm_b1 = registry_b
        .install_consumer_arm(o2, plan_of(&[b(2, 61, 0)]))
        .expect("arm in registry B")
        .arm;
    assert!(arm_a1.0 > pre.0);
    assert!(arm_b1.0 > arm_a1.0, "arm ids are global across registries");

    // Teardown never recycles ids: a fresh arm after a retire is strictly
    // newer (a late id from a dead adapter can never alias it).
    assert_eq!(
        registry_a.retire(o1, arm_a1, Family::Consumer),
        Ok(true),
        "teardown"
    );
    let arm_a2 = registry_a
        .install_consumer_arm(o1, plan_of(&[b(3, 62, 0)]))
        .expect("fresh arm")
        .arm;
    assert!(arm_a2.0 > arm_b1.0, "no arm id reuse after teardown");
}

// --- 15. Explicit configured fixture: ids 1-4, 5-8, grammar 9 ---------------

#[test]
fn explicit_fixture_registers_consumer_ids_one_to_four_shortcut_ids_five_to_eight_and_grammar_id_nine(
) {
    let mut registry = CarbonRegistry::new(RecordingOps::on_main());
    let owner = allocate_owner_id();
    registry
        .install_consumer_arm(owner, consumer_plan())
        .expect("consumer arm ids 1-4");
    registry
        .install_shortcut_arm(owner, shortcut_plan())
        .expect("shortcut arm ids 5-8");
    // The grammar arm is a CONSUMER-family arm (AcceptAction::Correction), so
    // it replaces the ids 1-4 arm: its drain must precede id 9's register.
    registry
        .install_consumer_arm(owner, grammar_plan())
        .expect("grammar arm id 9");
    let ops = registry.ops();
    let expected_registers: Vec<(Family, KeyBinding)> = consumer_plan()
        .bindings
        .iter()
        .map(|b| (Family::Consumer, *b))
        .chain(
            shortcut_plan()
                .bindings
                .iter()
                .map(|b| (Family::Shortcut, *b)),
        )
        .chain(
            grammar_plan()
                .bindings
                .iter()
                .map(|b| (Family::Consumer, *b)),
        )
        .collect();
    assert_eq!(
        ops.registers(),
        expected_registers,
        "exact (id, keycode, mask) tuples per family, in install order"
    );
    // Grammar replacement: ids 1-4 drained before id 9 registered.
    let grammar_register_idx = ops
        .log
        .iter()
        .position(|op| as_register(op) == Some((Family::Consumer, b(9, 13, 0))))
        .expect("grammar register in log");
    let consumer_unregister_idx = ops.unregister_indexes();
    assert_eq!(consumer_unregister_idx.len(), 4);
    assert!(
        consumer_unregister_idx
            .iter()
            .all(|i| *i < grammar_register_idx),
        "grammar arm registered before the ids 1-4 drain completed"
    );
    // Final state: consumer family carries exactly the grammar arm; shortcuts
    // stay live through the consumer replacement.
    assert_eq!(ops.live_of(Family::Consumer), vec![b(9, 13, 0)]);
    assert_eq!(ops.live_of(Family::Shortcut), shortcut_plan().bindings);
}

// --- 16. Default empty plans: handler + armed empty slots, zero registrations

#[test]
fn default_empty_plans_install_the_handler_arm_empty_slots_and_register_nothing() {
    let mut registry = CarbonRegistry::new(RecordingOps::on_main());
    let owner = allocate_owner_id();
    let consumer = registry
        .install_consumer_arm(owner, ArmPlan::default())
        .expect("empty consumer plan arms a slot");
    let shortcut = registry
        .install_shortcut_arm(owner, ArmPlan::default())
        .expect("empty shortcut plan arms a slot");
    let ops = registry.ops();
    assert_eq!(
        ops.registers().len(),
        0,
        "defaults configure no hotkey registrations"
    );
    assert!(ops.unregisters().is_empty());
    assert_eq!(
        registry.armed_arm(Family::Consumer),
        Some((owner, consumer.arm)),
        "empty consumer plan still arms its slot"
    );
    assert_eq!(
        registry.armed_arm(Family::Shortcut),
        Some((owner, shortcut.arm)),
        "empty shortcut plan still arms its slot"
    );
    // Cleanup of empty slots: retire works, zero native unregisters.
    assert_eq!(
        registry.retire(owner, consumer.arm, Family::Consumer),
        Ok(true)
    );
    assert_eq!(
        registry.retire(owner, shortcut.arm, Family::Shortcut),
        Ok(true)
    );
    assert_eq!(registry.ops().unregisters().len(), 0);
    assert_eq!(registry.armed_arm(Family::Consumer), None);
    assert_eq!(registry.armed_arm(Family::Shortcut), None);
}

// --- 17. Slot hook ordering contract ----------------------------------------

#[test]
fn slot_hooks_observe_cleared_before_the_replacement_publish() {
    let (mut registry, log) = registry_with_event_log();
    let owner = allocate_owner_id();
    let first = registry
        .install_consumer_arm(owner, consumer_plan())
        .expect("first arm");
    // Capture the boundary BEFORE the replacement so the first replacement
    // register is located independently of where the Cleared event lands.
    let boundary = log.0.lock().expect("timeline lock").len();
    let second = registry
        .install_consumer_arm(owner, plan_of(&[b(21, 80, 0), b(22, 81, 0)]))
        .expect("replacement arm");
    registry
        .retire(owner, second.arm, Family::Consumer)
        .expect("retire");
    // Exact publish/clear sequence: Published(old) … Cleared(old) BEFORE
    // Published(new); retire clears only the arm that owned the slot.
    assert_eq!(
        log.slot_snapshot(),
        vec![
            SlotEvent::Published {
                family: Family::Consumer,
                arm: first.arm,
            },
            SlotEvent::Cleared {
                family: Family::Consumer,
                arm: first.arm,
            },
            SlotEvent::Published {
                family: Family::Consumer,
                arm: second.arm,
            },
            SlotEvent::Cleared {
                family: Family::Consumer,
                arm: second.arm,
            },
        ]
    );
    // Unified interleaving, non-tautological: the first register AFTER the
    // pre-captured replacement boundary (independent of Cleared's position)
    // must come strictly after Cleared(old).
    let cleared_old_index;
    let first_new_register_index;
    {
        let timeline = log.0.lock().expect("timeline lock");
        cleared_old_index = timeline
            .iter()
            .position(|e| {
                matches!(e, TimelineEvent::Slot(SlotEvent::Cleared { arm, .. }) if *arm == first.arm)
            })
            .expect("old arm cleared");
        first_new_register_index = timeline[boundary..]
            .iter()
            .position(|e| matches!(e, TimelineEvent::Op(Op::Register { .. })))
            .expect("replacement registers exist")
            + boundary;
    }
    assert!(
        cleared_old_index < first_new_register_index,
        "Cleared(old) must precede the first replacement register"
    );
}
