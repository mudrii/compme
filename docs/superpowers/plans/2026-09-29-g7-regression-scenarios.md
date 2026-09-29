# G7 regression scenario design + Linux-feasible test improvements (2026-09-29)

> **Later 2026-09-29 update:** this is the earlier design snapshot. The
> [implementation plan](2026-09-29-g7-linux-implementation.md) refines its
> registration contract; G7 is now implemented locally and Linux-validated.
> See the [audit record](2026-09-29-g7-codex-validation.md) for current evidence
> and outstanding native gates. Statements below about future-only code refer
> to the original snapshot.

**Worker B (regression-test design), read-only session; integrated by the
coordinator.** All `file:line` references are to the on-disk tree at HEAD
`12d5815…` plus the protected consolidation diff.

**Scope split (binding for implementers):** Deliverable 1 (S1–S9, I1) is a
design specification for the **future G7 batch** — none of it exists in current
code, and nothing here changes production routing. Deliverable 2 (D2-1) is
**implemented locally this batch, Linux Host Gate passed, uncommitted with
native gates pending** (see §D2-1 for exact evidence).
D2-2 is a recommended follow-up requiring an ownership decision; D2-3/D2-4 are
rejected.

**Review corrections integrated (final, per the independent B audit):**

1. Coverage claims bounded to "no identified direct deterministic unit coverage
   of the native registration/Drop boundary"; live A1b accept-tap scripts do
   exercise the native path on macOS but are not deterministic unit tests and
   are unrecorded (previously overstated as "zero automated coverage").
2. "Drop cannot carry context" retracted: `Drop::drop(&mut self)` can read
   resource fields, so executor/sink handles can be **stored in the resource**
   instead of process globals. Storage choice (stored handles vs. globals) is
   an ownership decision for the G7 batch, not forced by a false rationale.
3. Main-thread routing tests need an **injected main-thread predicate/identity**
   or a native main-thread harness: Rust test bodies run on test threads even
   with `--test-threads=1`, so "call on main" must be simulated by the seam,
   not asserted of the test process.
4. I1 corrected: `test_adapter_with_hooks` (`lib_tests.rs:486–511`) injects the
   **`Custom`** fake installer; it does **not** exercise the `Worker` arm. The
   integration assertion needs a new explicit native-path harness mode.
5. S1 corrected: no premature off-main `Ok` contract (completion contract is
   explicitly unresolved — handoff Batch 1 item 6).
6. S6 strengthened: sink-recorded failure alone is insufficient; caller/
   controller state must reconcile and failure must be observable under a
   defined nonblocking contract.
7. S8 split: keeping the replacement (B) live proves only stale-A teardown;
   real worker/subscription shutdown with final empty registries is a separate
   required assertion; replacement-under-queued-ops stays with S2/S3.
8. S9 corrected: defaults are **empty** for shortcuts and grammar —
   `ShortcutBindings` derives `Default` with all-`Option`-`None` fields
   (`crates/shell_flags/src/lib.rs:118–124`); `AcceptKeymap::default()` has
   `grammar_accept: None` (`crates/platform_macos/src/lib.rs:2992–2998`).
   Shortcut/grammar id expectations require explicit configuration; empty
   defaults are pinned separately.
9. D2-1 kept with an explicit positive control (existing four-role acceptance
   test) so the collision result is attributable; `cmd+96` is Linux-supported
   (F5/Mod4 mapping), so unsupported-chord fallback cannot explain the result.

## 0. Basis

- **The blocker is real and respected.** `always-on-hotkeys-physical-look` is
  "never recorded" (`docs/ACCEPTANCE.md:806`); ROADMAP row 19 holds G7 until
  the physical baseline exists. Nothing here changes production routing.
- **Design of record:** Qfd.md §22, especially §22.2 (main-thread marshal,
  id-guard replaces FIFO, u64-only crossing) and §22.4 (the Carbon-free
  testable seam). The handoff Batch 1 "Regression requirements" list is the
  checklist this catalog implements.
- **Qfd's line numbers are stale**; current anchors are as recorded in the
  integrated call/ownership analysis
  (`docs/superpowers/plans/2026-09-29-g7-call-ownership-analysis.md` §1).

## 1. The seam the scenarios attach to (must exist before they compile)

Two injection points, arranged like the existing fake style
(`AdapterTestHooks` `lib.rs:366–377`, wiring `lib.rs:1291–1324`;
`AdapterAcceptTapInstaller::Custom` `lib.rs:595–598`):

1. **MainThreadExecutor** — production default: run inline when an injected
   main-thread predicate says "on main", else post to `DispatchQueue::main()`
   (precedent `schedule_pasteboard_restore`, `lib.rs:2394–2404`). Test fakes:
   `InlineExecutor` (executes immediately, records the **executor's** main
   identity) and `QueuedExecutor` (records ops; applies them only on explicit
   `flush()`/`flush_in_order(...)` so worst-case reorderings are constructed,
   not raced). The main-thread **predicate/identity is itself injected** so
   tests can simulate both sides deterministically (correction 3); a native
   main-thread harness (dispatch onto the real main thread) supplements this
   on macOS where needed.
2. **CarbonOpSink** — register/unregister/install-handler leaf
   (`RegisterEventHotKey`/`UnregisterEventHotKey`/`InstallEventHandler`).
   Production default: the real FFI. Test fake: `RecordingSink` (fake tokens,
   ordered `(op, id, keycode, mask, thread)` log, failure injection: fail the
   Nth register or the handler install with a Carbon-shaped nonzero status).
   **Storage:** executor/sink handles may live in resource fields — `Drop`
   receives `&mut self` and can use them — or in process-level access points;
   the G7 batch chooses by ownership analysis (correction 2). Production
   functions remain the code under test; only the FFI leaf is fake.
3. **CarbonMarshalGuard** — like `TabHotkeySuppressedGuard`
   (`lib_tests.rs:415`)/`KeyBindingsGuard` (`run_loop_tests.rs:71–105`):
   serialization mutex plus reset of `CARBON_ARM_ID`/`SHORTCUT_ARM_ID` and both
   slots (process-global statics, `lib.rs:3277–3282`, `lib.rs:3365–3367`).

**Red/green policy.** S1, S2, S5, S6 have genuine red-before states (behavior
variant implemented wrong → test fails). S3's and S8's worst-case reorderings
are **structurally unreachable pre-G7** (worker FIFO, `ax_worker.rs:951–964`);
their red state is expressed against the new mechanism (a no-id-guard variant
fails), and their slot-level twins are already green today
(`carbon_slot_stale_disarm_never_clears_a_newer_arm`, `lib_tests.rs:3511`) and
must stay green. Naming follows the repo's full-sentence snake_case convention
in `crates/platform_macos/src/lib_tests.rs` (`#[path]` sibling module).

## 2. Deliverable 1 — scenario catalog (future G7 batch)

### S1. `registers_inline_when_already_on_main_and_via_executor_otherwise`
Production `install_carbon_accept_hotkeys` routed through executor + sink,
driven twice: once with the injected predicate reporting main (inline path),
once reporting off-main (`QueuedExecutor`). Assertions: (a) main-path register
ops are in the sink before install returns (the synchronous-live invariant,
`lib.rs:496–505`, stays on the main path); (b) off-main: no sink op exists
before the executor flush, and recorded thread tags carry the executor's main
identity, never the caller's; (c) after flush the applied plan equals
`accept_keymap().arm_bindings_for_action(...)` computed on the caller
(`lib.rs:3351–3353`). **Completion contract: not asserted here beyond what the
G7 batch defines** (correction 5 — S6 pins whatever contract lands).

### S2. `unregisters_the_previous_arm_before_registering_its_replacement`
One `RecordingSink`, two installs with overlapping chords (Esc/Down exist in
every keymap), `QueuedExecutor` ordering. Assert: every unregister of arm A's
tokens strictly precedes arm B's first register; B registers its full set.
Red = "assign over old" variant. Carbon-op-level twin of the existing
controller-level pin `rearm_while_armed_reinstalls_the_consumer_and_keeps_the_armed_value`
(`lib_tests.rs:5880`).

### S3. `stale_queued_unregister_leaves_the_newer_arm_armed_and_registered`
Arm A → arm B (id guard unregisters A first) → drop A's resource posting a
stale `unregister(A)` → flush in the worst async order (B's registers first,
stale unregister last). Assert: no unregister of any B token; slot still
resolves to B; firing B's keycode invokes B. Red = no-id-guard variant (the
reordering is impossible pre-G7 by FIFO; it becomes the live hazard under
async main dispatch — Qfd §22.2).

### S4. `drop_teardown_unregisters_exactly_once_for_its_owned_ids`
Drop a consumer resource; assert unregister count == register count for its
tokens, token set exactly owned, zero ops on the shortcut resource's tokens,
slot disarmed only because this arm owned it.

### S5. `partial_registration_failure_cleans_up_only_its_owned_resources`
Sink fails the 3rd of 4 default accepts. Assert typed error naming
keycode/status (shape `lib.rs:3766–3771`); sink shows 2 ok + 1 failed register
then exactly 2 owned unregisters and nothing foreign; slot disarmed; state
retryable. Pins the current fail-closed semantic (`lib.rs:3344–3351` →
`lib.rs:3290`) so the G7 rewrite cannot lose it.

### S6. `registration_failure_propagates_and_a_later_retry_succeeds`
Sink fails `install_handler` on attempt 1, succeeds on attempt 2. Assert:
(a) the typed error surfaces — **and**, because off-main queuing is not proof
of anything, failure must be **observable at the defined completion point and
reconciled caller/controller-side** (correction 6): controller state ends
disarmed/unwedged (`set_accept_action` error path `lib.rs:449–453`), no slot
left armed over zero live keys; this test is where the batch's nonblocking
completion contract is pinned in writing; (b) attempt 2 re-invokes
install_handler (plain-flag retry, `lib.rs:3507–3509` — a `Once` regression
is caught); (c) retry registers the full plan and returns `Ok`.

### S7. `consumer_teardown_leaves_process_shortcuts_registered_and_dispatching`
Full hook-built adapter with executor+sink swapped; subscribe → show → hide.
Assert unregisters only for consumer tokens; `SHORTCUT_HANDLER_SLOT` still
armed and dispatching (`AcceptTapDecision::Shortcut(...)`); reverse leg on
subscription drop. Two-registry separation preserved (`lib.rs:3277` vs
`lib.rs:3365`, slot pick `lib.rs:3849–3853`).

### S8. `worker_stop_and_subscription_drop_drain_every_registry` (split per correction 7)
Real shutdown, not replacement ordering: after arming A and B and queueing
ops, drive the **actual** worker `Stop` (`ax_worker.rs:1109–1116`) **and**
drop the subscription; flush everything. Assert: final registries **empty**
(no tokens registered in the sink, both slots disarmed, worker resource map
drained), no late callbacks fire after stop, and no post-teardown re-post can
re-arm an obsolete arm (late duplicate A ops are dropped/rejected). The
"replacement stays live under queued ops" property remains S2/S3's
responsibility — S8 owns terminal state only.

### S9. Unchanged-behavior bundle (existing pins that must stay green) — corrected defaults (correction 8)
- **Key IDs:** `carbon_hotkey_ids_map_to_accept_keys` (`lib_tests.rs:4663`),
  `default_keymap_matches_the_cotypist_bindings` (`lib_tests.rs:4701`),
  constants `lib.rs:125–137`.
- **Action mapping / Tab suppression / collision:** existing pins as listed by
  the worker (`lib_tests.rs:4599`, `4569`, `5361`, `5506`, `5189`;
  `5448`, `5484`; `4953`, `4982`, `5540`, `4757`, `5080`).
- **Portable acceptance half (green on Linux today):** engine_core
  `:1405/:1427/:4884–4927`; engine `:431–436` +
  `arms_accept_tap_on_show_and_disarms_on_hide` `:1918`; run_loop
  `:4569–4572` pinned at `run_loop_tests.rs:10623–10635`.
- **New op-level deltas (each a few lines in the S1/S2 harness):**
  1. `register_plan_ids_are_stable_across_the_main_thread_marshal` — with an
     **explicitly configured noncolliding four-shortcut set and a grammar
     binding**, the sink plan uses ids {1,2,3,4} consumer, {5,6,7,8} shortcut,
     {9} grammar arm. Defaults are **not** the subject here (see 3).
  2. `suppressed_tab_plan_is_snapshotted_before_the_off_main_post` — with
     `TAB_HOTKEY_SUPPRESSED=true` (`lib_tests.rs:5484–5504`), the queued plan
     contains no `(48, 0)` chord.
  3. `default_configuration_registers_only_the_consumer_arm` — **empty
     defaults pinned separately**: with no configured shortcuts/grammar
     binding, the sink shows exactly the {1,2,3,4} consumer plan and zero
     shortcut/grammar **hotkey registrations** (empty plans may still install
     the handler and arm the slot — that distinction is the point of the pin;
     `ShortcutBindings::default()` all-`None`,
     `shell_flags/src/lib.rs:118–124`; `AcceptKeymap::default()`
     `grammar_accept: None`, `lib.rs:2992–2998`).

### I1. Integration assertion — production reaches the tested seam (rewritten per correction 4)
`production_worker_arm_routes_through_the_injected_carbon_seam`. The existing
`test_adapter_with_hooks` harness injects `Custom` (`lib_tests.rs:486–511`) and
so cannot serve. Required: a **new explicit native-path harness mode** that
keeps the **real `AdapterAcceptTapInstaller::Worker` arm** (`lib.rs:1518–1525`)
and the real `install_worker_accept_tap_resource` /
`install_carbon_accept_hotkeys` / `install_process_shortcut_hotkeys`, swapping
**only the leaf FFI operations** for `RecordingSink`/`QueuedExecutor`. Reachability is proven two ways: (a) the sink op log is produced by those
production functions (Shortcut ids first per `lib.rs:3317–3320`, then the exact
`arm_bindings_for_action` plan); (b) a **negative control** — same test with a
panic-on-use sink stub — fails, demonstrating the path genuinely flows through
the injected seam. Runs only on macOS native (cross-check compiles it on
Linux, `docs/DEVELOPMENT.md:501`).

**Determinism and serialization:** no sleeps; worst-case orders via
`QueuedExecutor::flush_in_order`; real-async completion via the existing
`wait_until` poller (`lib_tests.rs:584–598`); delivery via `recv_timeout`
channels. All scenarios touch process-global statics, so macOS native runs
stay `--test-threads=1` and take `CarbonMarshalGuard`.

## 3. Deliverable 2 — Linux-now test work

### D2-1 — implemented this batch (uncommitted, pending native gates): host-side collision drop-whole pin
`startup_shortcut_config_drops_a_colliding_set_whole_on_this_host` in
`crates/app/src/run_loop_tests.rs`, next to the positive control
`startup_key_bindings_apply_global_shortcuts_from_config`
(`run_loop_tests.rs:2223–2241`), which already proves all four roles of a
**supported, noncolliding** config are accepted (`cmd+96`-family is
Linux-supported — F5/Mod4 mapping — so unsupported-chord fallback cannot
explain results; correction 9). The new test feeds two roles one chord
(`cmd+96` twice; the other two roles configured noncolliding) and asserts
`crate::shell::effective_shortcut_bindings()` returns **all defaults**
(drop-whole, `crates/app/src/shell/stub.rs:206–213`). Evidence status: named test passes in the serial app lane and the full Linux
Host Gate on this host — 2026-09-29, 44 commands 0 skipped, app lane 647+2 and
config_startup 2 (649 total); log `.gate/glm-batch0-20260929-linux.log`
(colliding-set test and positive control both `... ok` in the app serial
section); like the protected consolidation diff it
stays **uncommitted** until the Full Local Gate and native macOS CI run — no
landed/committed claim is made.

### D2-2 — deferred (ownership decision required)
Focus-path per-app Tab-suppression propagation (producer side of
`set_tab_hotkey_suppressed`, `crates/app/src/run_loop.rs:5842–5845`) has no
test anywhere and the Linux stub is a bare no-op (`stub.rs:125`). Closing it
requires **some** production-side affordance — a recorded state store in
`stub.rs` is one unselected proposal, not a proven necessity; a test hook or a
shared production helper might serve instead. The choice belongs to the owner
in a dedicated change, since every candidate touches live production code on
Linux/Windows (`shell/mod.rs:18–26`). Deferred unimplemented.

### D2-3 — REJECTED (forbidden category) / D2-4 — REJECTED (duplication)
A portable clone of the arm registry would be the "disconnected test-only state
model" the handoff forbids; the slot semantics are already tested where they
live. New portable correction/acceptance/rearm tests would duplicate protected,
recently-consolidated coverage.

## 4. Coordinator note for the future G7 batch
Adding scenarios to `lib_tests.rs` changes the native test inventory; the
checker derives expectations dynamically but every doc naming a count must be
reconciled in the same commit (machine-pinned-docs tripwire), and the macOS
lane owes the native run.
