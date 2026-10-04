# G7 independent validation — 2026-09-29

Current status (verified 2026-10-03): **implementation committed in `7894243`,
test cleanup corrected in `261a1e6`; all five native CI lanes passed on
`261a1e6` ([run 36532353871](https://github.com/mudrii/compme/actions/runs/36532353871)),
including all 391 macOS adapter tests and the 2284-test inventory. Mac Full
Local Gate and physical before/after hotkey acceptance remain pending; not
release-ready.** The remainder records the original Linux checkpoint and
subsequent commit authorization; its pending-native statements are historical.

## Subsequent commit/push authorization

After the local validation report and explicit disclosure that no commit or
push had occurred, the owner requested **commit and push** on 2026-09-29.
This supersedes the local batch's earlier no-commit/no-push restriction, not
the outstanding native/physical/release evidence requirements. Before committing,
Codex verified every source hash against the final local snapshot and attempted
`tools/dev/check.sh`: fmt passed; command 2/57 stopped with
`error[E0455]: link kind framework is only supported on Apple targets` and
`objc2 only works on Apple platforms`. Full output:
`.gate/g7-commit-full-local-20260929.log`. Remaining Full Local Gate commands
did not run in that attempt; the separate Linux Host Gate evidence below still
applies. Native CI must validate the actual pushed commit. The baseline and
no-commit statements below describe the original local-validation checkpoint.

Implementation contract: [G7 Linux implementation plan](2026-09-29-g7-linux-implementation.md).
Baseline HEAD: `12d581555dcbcaa811654f9e4a98e998bf7383b9` plus the protected
pre-existing working diff recorded by that plan. Implementation is delegated
to Herdr `wA:tA` / `wA:pR`, pi `zai/glm-5.3-flash`; Codex owns this audit.

## First native CI execution and follow-up

Commit `789424312b88a3a9ff9af16c7b2563fa25044eb7` was pushed to `main`.
[Native CI](https://github.com/mudrii/compme/actions/runs/36531557679/job/109286151247)
ran the macOS suite: **390 passed, one failed**. The sole failure was
`g7_stale_duplicate_foreign_family_and_foreign_owner_teardown_are_no_ops`:
its final cleanup called `worker_fifo_barrier` after an earlier
`adapter.shutdown()` had already stopped and joined the worker. The expected
`CannotComplete { reason: "AX worker is not running" }` therefore failed the
test's barrier expectation. Failure excerpt:
`.gate/g7-native-ci-7894243-failure.log`.

The follow-up removes that invalid post-shutdown request and uses the earlier
completed join as the synchronization boundary, then replays every recorded
teardown and retains all slot/token/deferred-handler leak assertions. No
production behavior or test inventory changes. The existing failing native
test is the regression; final proof requires the follow-up commit's native CI.
This is a concrete limitation of the earlier static/cross-compilation review,
which did not identify the invalid cleanup call.

Follow-up local checks: Linux Host Gate again passed 44/44 commands with
zero skipped (`.gate/g7-ci-fix-linux-20260929.log`); Darwin all-targets check
and clippy passed (`.gate/g7-ci-fix-darwin-clippy-20260929.log`). The required
Full Local Gate attempt again stopped at command 2/57 on Apple-only dependencies
(`.gate/g7-ci-fix-full-local-20260929.log`). No native pass is inferred from
these Linux checks.

## Final verified evidence

- Portable tests include the actual production `carbon_registry.rs` source.
- Linux Host Gate passed **44/44 commands, zero skipped commands**. Its
  portable test lane passed 1250 tests; the serial app lane passed 649
  (647 main-binary plus two config-startup tests); one doctest passed:
  **1900 passed, zero failed**. Evidence:
  `.gate/g7-glm-20260929-linux.log`, SHA-256
  `1af2f5caf17b44889007e493d113fa1346cc14df4a582a0ab8cac1f6bcefc7f4`.
  This includes all 17 named portable Carbon registry tests. The existing
  vendored llama-cpp warning categories/counts match the protected Batch 0 log;
  no new project warning was introduced.
- Codex independently compiled final-source isolated copies with Rust 1.97.0 using
  `rustc --edition=2021 --test`: control 17/17; bypassing the main-thread
  guard fails both off-main tests; delaying slot clear until after replacement
  registration fails the corrected slot-order test and rollback slot test.
  Full output, mutation diffs and source hashes:
  `.gate/g7-codex-mutations-final-20260929.log`. Both mutants fail two tests;
  control passes 17/17. Earlier GLM isolated mutants also exercised skipped
  predecessor drain, family matching, rollback isolation and owner matching.
- Codex extracted the current TLS/RAII transaction helper verbatim into an
  isolated Linux harness with a stub callback type. All five probes passed:
  nested deferral, destructor reentry, per-thread isolation, destruction after
  caller-lock release, and panic-unwind cleanup. Evidence:
  `.gate/g7-codex-transaction-probe-20260929.log`. This is helper validation,
  not execution of Carbon or native integration tests. The extracted helper
  still matches the final production source verbatim.
- Codex independently ran final `cargo check --locked -p platform_macos
  --target aarch64-apple-darwin --all-targets`, the equivalent clippy command
  with `-- -D warnings`, and native-target `cargo doc --locked --no-deps`
  with `RUSTDOCFLAGS="-D warnings"`: all passed. Logs:
  `.gate/g7-codex-darwin-check-20260929.log`,
  `.gate/g7-codex-darwin-clippy-20260929.log`,
  `.gate/g7-codex-darwin-doc-20260929.log`.
- Final independent Standards and Spec reviews identified no remaining
  production or concrete test-path blocker after repairs. The last factual
  documentation corrections were verified in the final diff.
- Inventory delta is **42 tests: 17 portable plus 25 native regressions**;
  2242 becomes 2284. Current count surfaces are reconciled; dated historical
  counts remain unchanged. The native count is source-derived, not an executed
  macOS inventory claim.

## Findings returned to GLM

The implementation and test corrections below have been verified by independent
read-only Standards and Spec reviews. Runtime validation is distinguished below:

1. Reject retained installers after adapter shutdown before they can replace
   another adapter's registrations. Use per-adapter closed state.
2. Wire actual callback-slot clearing to production registry replacement and
   rollback; portable hooks alone do not clear the native slot.
3. Defer callback destruction outside slot locks and registry borrows, through
   the entire installation transaction, to avoid reentrant deadlock, panic or
   stale publication after a reentrant installation. Include the enclosing
   controller mutex boundary. Use thread-local queues and nested transaction
   scopes; extract callbacks before dropping them outside all RefCell borrows.
4. Correct teardown comments: dropping a caller-side worker resource queues
   removal; it does not synchronously unregister on the caller.
5. Preserve skipped-shortcut diagnostics and native handle thread ownership;
   resolve unused production/test-only helpers without broad warning suppression.
6. Native tests must use recording teardown executors, worker barriers and
   explicit replay on the simulated main thread. Do not assume immediate
   cleanup after cancellation or cleanup while the worker is parked.
7. Select queued teardown by identity/family after a barrier, not by racing
   `.first()`. Verify every predecessor unregister precedes the first new register.
8. Lost-reply regression must execute the installer closure and account for its
   returned resource, join the receiver, and prove stale cleanup is harmless.
9. Fake unregister must reject unknown tokens, double removal and wrong families.
10. Verify subscription cancellation supplies the active flag checked at delivery.
11. Exercise retained installers directly after shutdown; inactive controllers
    return early and cannot establish the installer's closed-state check.
12. Use the fake's current registration counter plus the desired failure offset;
    an absolute second-call failure cannot test replacement after the first arm.
13. Configure a real grammar binding for controller reentry and assert both
    reentrant operations succeed. Verify pending outer deferrals survive off-main
    rejection and nested adoption rollback until the outer transaction exits.
14. Inspect native-token leaks as well as callback slots and deferred handlers in
    test fixture cleanup. Release locks/RefCell borrows before dropping callbacks.
15. Distinguish an invalid keycode from a genuine native status `-1` using the
    actual keycode conversion condition.

Portable ordering assertions and failed-publication traces were repaired and
independently checked. Documentation must distinguish registration rejection
off-main from asynchronous teardown, and local implementation from validation.

## Remaining validation and release boundaries

Neither static review nor cross-compilation executes native tests. These gates
remain explicitly open:

- Serialized `platform_macos` tests/examples, including the 25 new regressions:
  require native macOS linking/runtime.
- Full Local Gate on Mac, including Swift icon generation, bundle smoke, live
  `check-model-gates.sh` and the macOS-shaped A1b runner self-test: unavailable
  in this Linux batch. Linux ran their supported self-tests where listed in its
  canonical gate, not their native/live counterparts.
- Exact-candidate native CI: there is no pushed commit for this working tree.
- Qfd §22.4a physical hotkey baseline and before/after acceptance, and the 22
  live macOS release gates: require a real Mac/GUI; no evidence was synthesized.
- Model-backed CPU/Metal release gates and the separate `tools/spike` gate:
  not part of this G7 Linux Host Gate run. The default portable run ignored
  nine model-backed tests plus 42 Linux live acceptance tests; the latter need
  their dedicated GUI/session harness. Two app subprocess helpers remained
  ignored in the default enumeration, as designed. This run does not claim
  new Linux/Windows live acceptance or model performance evidence.
- Release `post_verify`, signing/notarization and cask finalization need their
  real release workflow; none was attempted.

Main-thread registration is synchronous; off-main registration rejects before
effects. Main can wait for ID-token adoption by the worker, but the worker never
waits for main-thread teardown. Shutdown closes installers, deactivates delivery,
stops/joins the worker and drains that owner's registry on main. Off-main teardown
still requires a live main dispatch loop; cross-checks cannot establish its OS
behavior or physical keyboard correctness.

Protected baseline: HEAD remains `12d581555dcbcaa811654f9e4a98e998bf7383b9`;
the real index is untouched. G7-only diffs were reviewed against a temporary
index containing the protected starting patch, whose SHA-256 remains
`69cf86493210ddfbaaa7fa9c587198f2c23455b2bf07ed1933edc0c229138b7e`.
Existing consolidation/Batch 0 changes remain present. No branches, commits,
pushes, tags or release actions were performed.
