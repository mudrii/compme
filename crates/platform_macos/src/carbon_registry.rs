//! Host-neutral Carbon hotkey arm registry (G7, Qfd §22.2/§22.4).
//!
//! This module owns the REAL replace/rollback/teardown ALGORITHM for the two
//! main-thread-only hotkey registries (consumer accept keys, always-on
//! process shortcuts). Every native Carbon operation is injected behind
//! [`NativeOps`], so the same production logic runs on macOS (raw
//! `EventHotKeyRef` tokens) and in the portable Linux regression tests
//! (`crates/platform/tests/carbon_registry_tests.rs` includes this file
//! verbatim via `#[path]`). std-only by construction: no platform imports,
//! no `unsafe`; the only `cfg` usage is `#[cfg(test)]` on test-only
//! accessors (active in both portable and native test compilations).
//!
//! Contract (refines Qfd §22.2 per the 2026-09-29 G7 handoff):
//!
//! - Every mutating entry point is **main-thread-only and synchronous**. The
//!   main-thread identity is injected (`NativeOps::is_main_thread`); an
//!   off-main call returns [`RegistryError::NotOnMainThread`] with ZERO side
//!   effects — no handler install, no drain, no register/unregister, no slot
//!   event.
//! - Main success means the native operations applied. A failed consumer arm
//!   rolls back EXACTLY the refs it registered in that attempt (never the
//!   drained predecessor's, never the other family's) and leaves the family
//!   slot empty — never armed over zero live keys. The failure is retryable:
//!   the next install re-invokes `install_handler` (plain retry, no Once) and
//!   rebuilds the full plan.
//! - Replacement drains ALL old refs of the family — unregisters issued and
//!   completed BEFORE the FIRST new register — including shared Esc/Down
//!   chords, so re-registering a chord can never hit a duplicate-registration
//!   error from our own stale arm.
//! - Teardown is ID-scoped: [`CarbonRegistry::retire`] matches
//!   owner + arm + family exactly; stale, duplicate, foreign-family, and
//!   old-owner requests are no-ops and can never clear newer work.
//!   [`CarbonRegistry::shutdown_owner`] drains only the named owner's entries
//!   across both families.
//! - IDs are process-globally monotonic across adapter lifetimes:
//!   [`allocate_owner_id`] per adapter instance, [`allocate_arm_id`] per arm,
//!   so a late id from a previous adapter can never collide with (or match) a
//!   newer arm.
//! - Slot publish/clear is surfaced as [`SlotEvent`]s through
//!   [`SlotHooks`], emitted at the exact algorithmic points: `Cleared` for a
//!   drained predecessor fires BEFORE the first new register; `Published`
//!   fires only after the new arm's entry is fully recorded. Hooks must not
//!   reenter the registry (the native side only arms/disarms its static
//!   handler slot, which never calls back here).
//!
//! Zero-key plans are legal and still install the handler and arm the slot:
//! a live `CarbonRegistry::armed_arm` entry with zero tokens is an armed
//! slot, distinguishable in tests from "no operation ran".

use std::sync::atomic::{AtomicU64, Ordering};

/// Which main-thread-only registry an arm belongs to: the per-suggestion
/// consumer accept keys (ids 1–4 and the grammar arm id 9 in production) or
/// the always-on process shortcuts (ids 5–8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    /// Per-suggestion accept keys (`CARBON_HANDLER_SLOT` in production).
    Consumer,
    /// Always-on process shortcuts (`SHORTCUT_HANDLER_SLOT` in production).
    Shortcut,
}

impl Family {
    /// Deterministic iteration order for teardown sweeps (consumer first,
    /// mirroring production's registry ordering).
    pub const ALL: [Family; 2] = [Family::Consumer, Family::Shortcut];
}

/// One `(hotkey-id, keycode, modifier-mask)` triple, hoisted to the caller by
/// the plan computation (`arm_bindings_for_action` / `shortcut_registration_plan`).
/// `Copy + Send`-shaped: only plain data crosses the registry boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyBinding {
    /// Carbon hotkey id routed to the handler (consumer 1–4/9, shortcut 5–8).
    pub id: u32,
    /// Carbon virtual keycode.
    pub keycode: i64,
    /// Carbon modifier mask (0 for a bare key).
    pub mask: u32,
}

/// The full set of bindings one arm installs, computed by the caller before
/// any main-thread work (Qfd §22.2 "hoist the pure plan").
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArmPlan {
    /// Bindings in registration order.
    pub bindings: Vec<KeyBinding>,
}

impl ArmPlan {
    /// Convenience constructor for explicit fixtures and callers.
    pub fn from_bindings(bindings: impl IntoIterator<Item = KeyBinding>) -> Self {
        ArmPlan {
            bindings: bindings.into_iter().collect(),
        }
    }
}

/// Adapter-instance identity. One per adapter lifetime, allocated from a
/// process-global monotonic counter so a dropped old adapter's teardown ids
/// can never match a newer adapter's arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OwnerId(pub u64);

/// Arm identity, process-globally monotonic across adapter lifetimes (the
/// production `CARBON_ARM_ID` discipline). Every install attempt allocates a
/// fresh id, so a stale queued teardown can never alias a newer arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArmId(pub u64);

static NEXT_OWNER_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_ARM_ID: AtomicU64 = AtomicU64::new(1);

/// Allocate the next process-globally monotonic [`OwnerId`].
pub fn allocate_owner_id() -> OwnerId {
    OwnerId(NEXT_OWNER_ID.fetch_add(1, Ordering::Relaxed))
}

/// Allocate the next process-globally monotonic [`ArmId`].
pub fn allocate_arm_id() -> ArmId {
    ArmId(NEXT_ARM_ID.fetch_add(1, Ordering::Relaxed))
}

/// Typed registry failures. Maps to `PlatformError::CannotComplete` at the
/// native `lib.rs` boundary; [`RegistryError::NotOnMainThread`] is the
/// side-effect-free typed rejection for off-main calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// Off-main call rejected BEFORE any effect: no Carbon call, no slot
    /// mutation, no drain, no register/unregister, no slot event.
    NotOnMainThread,
    /// `InstallEventHandler` failed. Inherently retryable: the flag stays
    /// false, nothing was drained or registered, and the next arm retries.
    HandlerInstall { status: i32 },
    /// `RegisterEventHotKey` failed for one binding. The arm is rolled back
    /// (consumer) or the key logged-and-skipped (shortcut) before this error
    /// surfaces.
    Register {
        family: Family,
        binding: KeyBinding,
        status: i32,
    },
}

/// What one successful install did: the arm id to hand to
/// [`CarbonRegistry::retire`], how many refs registered, and (shortcuts only)
/// which keys were logged-and-skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmOutcome {
    /// The new arm's id (freshly allocated, process-globally monotonic).
    pub arm: ArmId,
    /// Number of refs successfully registered for this arm.
    pub registered: usize,
    /// Shortcut keys skipped by the per-key log-and-skip policy; empty for
    /// consumer arms (consumer is all-or-error).
    pub skipped: Vec<KeyBinding>,
}

/// Slot publish/clear events emitted at exact algorithmic points. The native
/// side maps `Published` to arming its static handler slot and `Cleared` to
/// the id-guarded disarm (duplicate/stale disarms stay harmless there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotEvent {
    /// The named arm now owns the family slot. Emitted only after the arm's
    /// entry (possibly zero-token) is fully recorded — never for a failed arm.
    Published { family: Family, arm: ArmId },
    /// The named arm stopped owning the family slot: drained for replacement
    /// (BEFORE the first new register), retired, failed after its predecessor
    /// was drained, or swept by [`CarbonRegistry::shutdown_owner`].
    Cleared { family: Family, arm: ArmId },
}

/// Slot publish/clear hooks. Kept OUTSIDE the registry's entry state and
/// called without any borrow reentry: implementations must only touch their
/// own native slot storage, never call back into the registry.
pub trait SlotHooks {
    /// Called for every [`SlotEvent`].
    fn slot_event(&mut self, event: SlotEvent);
}

/// The injected native boundary. Production implements this over the real
/// Carbon FFI (main-thread-checked); the portable tests implement it over a
/// recording fake. `Token` stays native-side (`EventHotKeyRef` in
/// production); only plain data crosses this trait.
pub trait NativeOps {
    /// Raw native registration handle (production: `EventHotKeyRef`). `Copy`
    /// exists only so handles can be stored and passed to `unregister` by
    /// value; a raw token NEVER crosses threads — it stays main-local next to
    /// the registry. `Copy` does not imply `Send`: only the plain-data
    /// `OwnerId`/`ArmId`/`Family` values cross threads.
    type Token: Copy;

    /// Injected main-thread identity. Production: a real platform check.
    /// Tests: a plain field, so branching is validated deterministically on a
    /// serial test thread.
    fn is_main_thread(&self) -> bool;

    /// Install the shared Carbon event handler, once per process lifetime.
    /// Must be a plain retryable operation (no `Once`): a failed install
    /// leaves the flag false so the NEXT arm retries. Called on every install
    /// attempt BEFORE any drain or register.
    fn install_handler(&mut self) -> Result<(), RegistryError>;

    /// Register one hotkey binding on the main thread, returning the native
    /// token. Errors are [`RegistryError::Register`] (family, binding,
    /// Carbon status).
    fn register(
        &mut self,
        family: Family,
        binding: &KeyBinding,
    ) -> Result<Self::Token, RegistryError>;

    /// Unregister one previously returned token. Exactly-once discipline: the
    /// registry calls this exactly once per successfully registered token.
    fn unregister(&mut self, family: Family, token: Self::Token);
}

/// One family's live arm: who owns it, which arm id guards it, and the native
/// tokens registered for it. Entry presence IS slot ownership: a family has
/// either zero or one live arm, and a zero-token entry is a live, armed slot.
struct FamilyEntry<O: NativeOps> {
    owner: OwnerId,
    arm: ArmId,
    tokens: Vec<O::Token>,
}

/// The two main-thread-only arm registries (consumer + shortcut) with the
/// replacement, rollback, and teardown algorithm. All mutating methods are
/// main-thread-only via the injected predicate; all teardown is ID-scoped so
/// stale, duplicate, foreign, and old-owner requests are no-ops.
pub struct CarbonRegistry<O: NativeOps> {
    ops: O,
    hooks: Option<Box<dyn SlotHooks>>,
    consumer: Option<FamilyEntry<O>>,
    shortcut: Option<FamilyEntry<O>>,
}

/// Register outcome for [`CarbonRegistry::register_all`]: the successfully
/// issued tokens, or the error paired with the tokens already issued (which
/// the caller rolls back).
type RegisterOutcome<O> =
    Result<Vec<<O as NativeOps>::Token>, (RegistryError, Vec<<O as NativeOps>::Token>)>;

impl<O: NativeOps> CarbonRegistry<O> {
    /// Build a registry over the native operations with no slot hooks.
    /// Production uses [`Self::with_hooks`]; the portable test crate (which
    /// clippy in this crate cannot see) builds bare registries through this
    /// constructor.
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn new(ops: O) -> Self {
        CarbonRegistry {
            ops,
            hooks: None,
            consumer: None,
            shortcut: None,
        }
    }

    /// Build a registry that emits [`SlotEvent`]s to `hooks` at the exact
    /// algorithmic publish/clear points. Hooks must not reenter the registry.
    pub fn with_hooks(ops: O, hooks: Box<dyn SlotHooks>) -> Self {
        CarbonRegistry {
            ops,
            hooks: Some(hooks),
            consumer: None,
            shortcut: None,
        }
    }

    /// The injected main-thread identity (same predicate the mutating
    /// entry points consult).
    pub fn is_main_thread(&self) -> bool {
        self.ops.is_main_thread()
    }

    /// Read-only access to the injected native operations (op-log inspection,
    /// native-side state queries). Does not run Carbon work by itself.
    /// Test-only: production reads native state through the slot hooks.
    // Also exercised by the portable test target (a different crate clippy
    // cannot see); retained here for the native test lane.
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn ops(&self) -> &O {
        &self.ops
    }

    /// Mutable access to the injected native operations, for callers that
    /// drive native-side state between registry calls (e.g. failure-injection
    /// setup in the portable tests). Main-thread discipline is the caller's:
    /// do not call native operations off-main through this accessor.
    // Also exercised by the portable test target (a different crate clippy
    // cannot see); retained here for the native test lane.
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn ops_mut(&mut self) -> &mut O {
        &mut self.ops
    }

    /// Consume the registry, returning the native operations. The registry
    /// holds no native tokens after teardown drained them; dropping a registry
    /// with live entries leaks those registrations by design — teardown is
    /// explicit (retire/shutdown_owner), never implicit in Drop.
    // Also exercised by the portable test target (a different crate clippy
    // cannot see); retained here for the native test lane.
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn into_ops(self) -> O {
        self.ops
    }

    /// The live arm owning `family`'s slot, if any (owner + arm id). A live
    /// entry with zero registered tokens is still an armed slot. Production
    /// clears slots via the hooks; test targets assert through this reader.
    // Also exercised by the portable test target (a different crate clippy
    // cannot see); retained here for the native test lane.
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn armed_arm(&self, family: Family) -> Option<(OwnerId, ArmId)> {
        match family {
            Family::Consumer => self.consumer.as_ref().map(|e| (e.owner, e.arm)),
            Family::Shortcut => self.shortcut.as_ref().map(|e| (e.owner, e.arm)),
        }
    }

    /// Install (or replace) the CONSUMER arm for `owner` with `plan`.
    ///
    /// Order of operations, all inline on the main thread:
    /// 1. main-thread check — off-main returns [`RegistryError::NotOnMainThread`]
    ///    with zero effects;
    /// 2. `install_handler` (retryable) — on failure nothing has been drained
    ///    or registered and the predecessor stays live;
    /// 3. the predecessor family arm (any owner) is FULLY drained — every old
    ///    ref unregistered, `SlotEvent::Cleared` emitted — BEFORE the first
    ///    new register, so shared Esc/Down chords can re-register cleanly;
    /// 4. each binding registers in plan order; on the first failure the arm
    ///    rolls back EXACTLY the refs this attempt registered (never foreign
    ///    tokens), leaving the family slot empty (never armed over zero live
    ///    keys) and returns the [`RegistryError::Register`];
    /// 5. on success the entry is recorded (even for a zero-key plan — the
    ///    slot arms with zero registrations) and `SlotEvent::Published`
    ///    fires.
    pub fn install_consumer_arm(
        &mut self,
        owner: OwnerId,
        plan: ArmPlan,
    ) -> Result<ArmOutcome, RegistryError> {
        self.check_main_thread()?;
        self.ops.install_handler()?;
        // Replacement ordering guarantee: ALL old refs of this family are
        // unregistered before the FIRST new register below.
        self.drain_family(Family::Consumer);
        let arm = allocate_arm_id();
        match self.register_all(Family::Consumer, &plan.bindings) {
            Ok(tokens) => {
                let registered = tokens.len();
                self.consumer = Some(FamilyEntry { owner, arm, tokens });
                self.emit(SlotEvent::Published {
                    family: Family::Consumer,
                    arm,
                });
                Ok(ArmOutcome {
                    arm,
                    registered,
                    skipped: Vec::new(),
                })
            }
            Err((err, partial)) => {
                // All-or-error: unregister EXACTLY the tokens this arm
                // registered. The predecessor was already drained (its Cleared
                // fired in step 3) and the other family is untouched, so the
                // only live tokens are this attempt's own.
                for token in &partial {
                    self.ops.unregister(Family::Consumer, *token);
                }
                Err(err)
            }
        }
    }

    /// Install (or replace) the SHORTCUT arm for `owner` with `plan`.
    ///
    /// Same main-thread guard, retryable handler install, and
    /// drain-predecessor-first ordering as [`Self::install_consumer_arm`],
    /// but registration follows the per-key log-and-skip policy: a failed
    /// key is recorded in `ArmOutcome::skipped` and the remaining keys still
    /// register; the successful keys are kept. Only the handler-install
    /// failure fails the call.
    pub fn install_shortcut_arm(
        &mut self,
        owner: OwnerId,
        plan: ArmPlan,
    ) -> Result<ArmOutcome, RegistryError> {
        self.check_main_thread()?;
        self.ops.install_handler()?;
        self.drain_family(Family::Shortcut);
        let arm = allocate_arm_id();
        let mut tokens = Vec::new();
        let mut skipped = Vec::new();
        for binding in &plan.bindings {
            match self.ops.register(Family::Shortcut, binding) {
                Ok(token) => tokens.push(token),
                // Per-key log-and-skip: one bad shortcut binding must never
                // abort the install or lose the already-registered keys.
                Err(RegistryError::Register { binding, .. }) => skipped.push(binding),
                Err(err) => {
                    for token in &tokens {
                        self.ops.unregister(Family::Shortcut, *token);
                    }
                    return Err(err);
                }
            }
        }
        let registered = tokens.len();
        self.shortcut = Some(FamilyEntry { owner, arm, tokens });
        self.emit(SlotEvent::Published {
            family: Family::Shortcut,
            arm,
        });
        Ok(ArmOutcome {
            arm,
            registered,
            skipped,
        })
    }

    /// Retire the arm matching EXACTLY (`owner`, `arm`, `family`): every owned
    /// ref unregisters exactly once and `SlotEvent::Cleared` fires. A stale
    /// (already replaced/retired), duplicate, or foreign request — wrong
    /// owner, wrong arm id, or wrong family — is a NO-OP returning `Ok(false)`
    /// so a late teardown can never clear newer work. Main-thread-only:
    /// off-main callers post an ID-scoped request instead of calling this.
    pub fn retire(
        &mut self,
        owner: OwnerId,
        arm: ArmId,
        family: Family,
    ) -> Result<bool, RegistryError> {
        self.check_main_thread()?;
        let matches = match family {
            Family::Consumer => self
                .consumer
                .as_ref()
                .is_some_and(|e| e.owner == owner && e.arm == arm),
            Family::Shortcut => self
                .shortcut
                .as_ref()
                .is_some_and(|e| e.owner == owner && e.arm == arm),
        };
        if !matches {
            return Ok(false);
        }
        let entry = self.take_family(family).expect("checked live above");
        for token in &entry.tokens {
            self.ops.unregister(family, *token);
        }
        self.emit(SlotEvent::Cleared {
            family,
            arm: entry.arm,
        });
        Ok(true)
    }

    /// Explicit owner-scoped shutdown cleanup: drain EVERY arm owned by
    /// `owner` across BOTH families inline on the main thread — each owned ref
    /// unregisters exactly once, each owned slot emits `SlotEvent::Cleared` —
    /// and never touch any other owner's entries. Returns the number of arms
    /// drained. Dropping an old adapter can therefore never clear newer
    /// registrations owned elsewhere.
    pub fn shutdown_owner(&mut self, owner: OwnerId) -> Result<usize, RegistryError> {
        self.check_main_thread()?;
        let mut drained = 0;
        for family in Family::ALL {
            let owned = match family {
                Family::Consumer => self.consumer.as_ref().is_some_and(|e| e.owner == owner),
                Family::Shortcut => self.shortcut.as_ref().is_some_and(|e| e.owner == owner),
            };
            if owned {
                let entry = self.take_family(family).expect("checked live above");
                for token in &entry.tokens {
                    self.ops.unregister(family, *token);
                }
                self.emit(SlotEvent::Cleared {
                    family,
                    arm: entry.arm,
                });
                drained += 1;
            }
        }
        Ok(drained)
    }

    fn check_main_thread(&self) -> Result<(), RegistryError> {
        if self.ops.is_main_thread() {
            Ok(())
        } else {
            Err(RegistryError::NotOnMainThread)
        }
    }

    fn take_family(&mut self, family: Family) -> Option<FamilyEntry<O>> {
        match family {
            Family::Consumer => self.consumer.take(),
            Family::Shortcut => self.shortcut.take(),
        }
    }

    /// Drain a family's live arm (any owner): every ref unregisters exactly
    /// once, the entry is removed, and `Cleared` fires. Used by replacement
    /// (before the first new register), by the failure path (via the caller),
    /// and by retire/shutdown sweeps.
    fn drain_family(&mut self, family: Family) -> Option<FamilyEntry<O>> {
        let entry = self.take_family(family)?;
        for token in &entry.tokens {
            self.ops.unregister(family, *token);
        }
        self.emit(SlotEvent::Cleared {
            family,
            arm: entry.arm,
        });
        Some(entry)
    }

    /// Register outcome for [`Self::register_all`]: the successfully issued
    /// tokens, or the error paired with the tokens already issued (which the
    /// caller rolls back).
    fn register_all(&mut self, family: Family, bindings: &[KeyBinding]) -> RegisterOutcome<O> {
        let mut tokens = Vec::new();
        for binding in bindings {
            match self.ops.register(family, binding) {
                Ok(token) => tokens.push(token),
                Err(err) => return Err((err, tokens)),
            }
        }
        Ok(tokens)
    }

    fn emit(&mut self, event: SlotEvent) {
        if let Some(hooks) = self.hooks.as_mut() {
            hooks.slot_event(event);
        }
    }
}
