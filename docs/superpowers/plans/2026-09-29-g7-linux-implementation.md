# G7 implementation on Linux — technical handoff

> **Delivery status:** handed to the existing GLM session, implemented with
> multiple workers, independently reviewed and repaired, and validated through
> the Linux Host Gate and Darwin cross-checks. Native execution and release
> acceptance remain pending. See the
> [final local audit](2026-09-29-g7-codex-validation.md) for evidence and limits.

> **Subsequent authorization:** after reviewing that result, the owner requested
> commit and push. This supersedes the original local-only no-commit/no-push
> scope below; native CI and physical/release acceptance remain required.

## Authorization, scope, and baseline

The owner explicitly requested G7 implementation on Linux on 2026-09-29 after
being informed that the physical Mac baseline is missing. This authorizes local
implementation before that baseline, superseding the earlier hold for this
working batch. It does not supply native evidence or authorize release.
Keep the prior baseline decision as a historical record; add a dated amendment
to Qfd §22 and ROADMAP describing the authorized sequencing change.

Implement through existing Herdr tab `wA:tA` (`glm`), pane `wA:pR`, pi with
`zai/glm-5.3-flash`. Codex independently reviews and returns fixes. Use actual
multi-agent work, with the coordinator as the sole integrator of overlapping
macOS files. Preserve the existing session and model.

Starting HEAD: `12d581555dcbcaa811654f9e4a98e998bf7383b9`, branch `main`.
The protected tracked diff is `/tmp/compme-g7-start-20260929.patch`, SHA-256
`69cf86493210ddfbaaa7fa9c587198f2c23455b2bf07ed1933edc0c229138b7e`.
It includes the prior consolidation and Batch 0 test/docs. Preserve untracked
plans, audit records, and `.gate/` evidence as well. No branches, commits,
pushes, tags, or publishing in this Linux-only batch: native gates are pending.

Read AGENTS, current ROADMAP, Qfd §22, the Batch 0 ownership analysis and
regression scenarios. Use CodeGraph before locating code. Their dated line
numbers are navigation hints, not current source truth. This document refines
the unresolved completion contract in the earlier scenario catalog.

## Required behavior and selected completion contract

All Carbon handler installation, hotkey registration, and unregistration must
execute on the macOS main thread. Preserve key IDs, plans, action mapping,
Tab suppression, collision filtering, and independent process-shortcut lifetime.

**Registration is main-thread-only and synchronous.** Check main-thread identity
before Carbon calls, handler-slot mutation, worker adoption, or queueing work.
An off-main registration attempt returns `PlatformError::CannotComplete` with
a clear reason and no side effects. Do not enqueue speculative registration.
Current production arm/rearm calls originate from the host loop; verify every
call path, including examples, before changing this contract. If a real supported
off-main caller is found, report it to Codex and resolve the contract before
changing that caller. Custom test installers retain their existing semantics.

Main registration returns success only after the native operation and worker
ownership adoption succeed. Preserve consumer all-or-error rollback, and the
existing shortcut per-key log-and-skip policy; handler-install failure remains
an error for both. This deliberately refines Qfd's asynchronous off-main install
proposal: a queued request cannot satisfy the current synchronous Result API.

**Teardown is inline on main, asynchronous otherwise.** Worker/sleeper drops
post an ID-scoped request and return. No worker-to-main synchronous dispatch,
reply wait, bounded wait, or new lock cycle is allowed. Main may retain the
existing main-to-worker adoption wait because adoption invokes no Carbon work.

## Implementation structure

1. Extract a small dependency-free ownership/registration module used directly
   by macOS production. It owns the actual replace/rollback/unregister algorithm,
   parameterized only at native operation boundaries. Use existing dependencies
   and straightforward types; avoid a general task framework or new crate unless
   a demonstrated build constraint requires it.
2. Store raw Carbon refs in main-thread-only storage, preferably a TLS RefCell
   with a real main-thread guard at every entry. Keep separate consumer and
   shortcut entries. Never access worker TLS as if it were main TLS. Queued
   closures carry only Send-safe IDs/family metadata, never raw native handles.
   A u64-based token is auto-Send; no unsafe Send/Sync assertion is needed.
3. Allocate monotonically unique arm IDs across adapter lifetimes. Replacement
   drains the previous family entry before registering the new plan, including
   shared Esc/Down chords. Late removal of an old ID cannot remove a replacement
   or the other family. Account for adapter ownership during final cleanup so
   dropping an old adapter cannot clear newer registrations owned elsewhere.
4. Hoist consumer and shortcut plan computation to the caller. Preserve default
   empty shortcut/grammar plans, configured collision handling, and correction
   mode. Successful zero-key plans can still install the shared handler and arm
   a slot; distinguish registrations from other operations in tests.
5. Apply native registration on main, then adopt an ID-only Drop token using
   `AxWorkerHandle::install_resource`. A main-side rollback guard remains armed
   until adoption succeeds. A send failure, installer failure/panic, or lost reply
   cleans up that ID inline on main. Duplicate queued cleanup is harmless.
   Preserve the worker's panic containment and non-Send resource storage.
6. Consumer partial failure unregisters exactly the successfully owned refs and
   leaves no active consumer slot. Handler installation failure remains retryable.
   Shortcut partial failure preserves successful refs and logs skipped keys as
   before. Publish and clear handler slots consistently with the selected error
   policy. Avoid holding registry borrows/locks across reentrant callbacks.
7. On teardown, retire only the matching ID and clear only its slot. Current
   subscription active/action guards must continue preventing late insertion or
   callback delivery. Test worker shutdown and subscriber cancellation, not only
   replacement while another arm survives.
8. Add explicit adapter-owned main-thread cleanup on shutdown before the host
   loop ceases servicing the dispatch queue. Worker shutdown may enqueue cleanup;
   waiting for the worker cannot require the main queue to progress. Drain the
   remaining owned entries inline after worker shutdown or at an equivalent
   demonstrably safe lifecycle edge. Off-main drop remains nonblocking; document
   its requirement that the main loop stay alive to finish queued native cleanup.
   Avoid permanent global shutdown flags that break later adapter instances.
   Rust runs `Drop::drop` before automatic field drops: an adapter Drop body
   cannot claim post-worker cleanup while relying on the worker field's later
   destructor. Use explicit shutdown/join or an equivalent ordered owner.
9. Update stale worker/FIFO comments and the MVP threading statement alongside
   actual code. Keep Carbon event callback panic containment. Document thread,
   lifetime, and reference validity at every changed unsafe call.

## Tests: production-connected and deterministic

Run the same dependency-free production module on Linux through a small
host-neutral harness (e.g. an existing Cargo integration target includes the
actual source file, or a checked-in rustc test runner). No copied test-only state
machine. Wire that test command into the appropriate documented gates if it is
not already reached by Cargo. Keep native wrapper tests in the existing sibling
`lib_tests.rs`; a new native-path harness must keep the production installer,
   because current `test_adapter_with_hooks` replaces it with Custom.

The portable harness must have a permanent gate entry and prove a nonzero set
of named tests ran. Injected thread identity validates branching, not Carbon's
real OS thread affinity; keep the native integration check separately pending.

Required assertions:

- Main registration is inline; off-main rejects without native/slot/worker effects.
- Replacing an arm unregisters all old refs before the first new registration.
- Stale, duplicate, foreign-family, and old-adapter teardown cannot clear newer work.
- Each owned reference unregisters once; token destruction requests one teardown.
- Consumer Nth-register failure rolls back only owned successes and remains retryable.
- Handler-install failure propagates and a later attempt retries installation.
- Shortcut per-key failure skips only failed bindings; consumer teardown preserves shortcuts.
- Worker adoption send failure and consumed-closure/lost-reply failure roll back.
- Actual subscription/worker shutdown with queued operations ends with no owned
  registrations and no late callbacks/reactivation; a later adapter can initialize.
- Explicit configured fixtures cover shortcut IDs 5–8 and grammar ID 9; default
  fixtures cover empty plans. Existing keymap/suppression/collision tests stay green.
- A native integration test proves the real installer reaches the shared module
  and worker token adoption, swapping only main identity/executor/native leaves.

Inject main-thread identity in deterministic tests: a serial Rust test thread
is not the OS main thread. Use explicit queue draining and completion channels,
not sleeps for race assertions. Demonstrate meaningful red/green or targeted
mutation sensitivity for ordering, stale-ID, rollback, and affinity behavior.
Run temporary mutants in isolated copies so concurrent work cannot be corrupted.

## Multi-agent execution and review points

Coordinator: verify baseline; assign disjoint work; integrate; execute gates;
return concise changed-file and evidence report. Poll workers at most 30 seconds
at a time. Preserve their output for review rather than producing long repeated
reasoning reports.

- Worker A: native ownership, registration/adoption, teardown/shutdown design and
  implementation patch. Own native integration files; coordinate exact boundaries.
- Worker B: implement the real portable core and deterministic regression tests
  against the agreed interface, plus native integration test proposals. Agree on
  interface with A before editing shared files; coordinator integrates collisions.
- Worker C: independent call-path/error/shutdown review and validation/doc inventory.
  Read-only during implementation, then return actionable issues to coordinator.

Completion checkpoints: (1) agreed lifecycle/interface and call-path audit;
(2) code plus meaningful tests; (3) Linux/cross-target gates; (4) Codex independent
Standards and Spec audits; (5) fixes and affected rechecks. Continue until all
Linux-actionable findings are resolved; native execution remains a separate status.

## Validation and evidence

Use the proven Nix environment: `nix-shell -p llvmPackages.libclang clang cmake
pkg-config openssl ruby go shellcheck python3`, toolchain 1.97.0, and exported
LIBCLANG_PATH/BINDGEN_EXTRA_CLANG_ARGS/LD_LIBRARY_PATH resolved through nix-build
as in the previous batch. Export variables before invoking Cargo. Capture full
logs with pipefail; verify named tests actually ran (app tests are binary targets).

Run focused tests first, then `tools/dev/check.sh --fence "Linux Host Gate"`.
Run/check the new portable test target, fmt/clippy, script self-tests if changed,
and macOS `--all-targets` cross-compilation. Cross-check app integration too if
changed. Reconcile current inventory docs from source-derived test deltas; keep
dated historical counts intact and label native count execution pending.

Inspect `git diff` and whitespace, compare against the protected starting diff,
and list every validation that cannot execute here: Full Local Gate on Mac,
serialized native macOS tests, exact-candidate native CI, physical before/after
hotkeys, and remaining release acceptance. Do not fabricate a baseline or mark
G7 released/fully validated. Native CI is not current-tree proof without its
corresponding pushed commit; do not push to manufacture that proof in this batch.

Deliver working-tree implementation and a compact evidence report with test
counts, log paths, resolved findings, thread/wait graph, contract refinement,
remaining native risks, and explicit uncommitted status. Update ROADMAP and Qfd
as local implementation awaiting native validation; update older handoff status
only with a pointer to this newer authorized plan, preserving dated reports.
