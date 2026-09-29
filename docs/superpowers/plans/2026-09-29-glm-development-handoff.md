# GLM development handoff — 2026-09-29

> **Later 2026-09-29 update:** G7 was subsequently authorized for local
> implementation, delivered to the same GLM session, implemented and audited.
> The [G7 plan](2026-09-29-g7-linux-implementation.md) and
> [final local validation record](2026-09-29-g7-codex-validation.md) supersede
> this earlier handoff's G7 hold. Native macOS/release gates remain open.

## Objective and delivery status

Prepare the next macOS release candidate through coordinated multi-agent
development, followed by independent Codex verification and a fix/review loop.
The designated implementer is the existing `pi` session in Herdr tab `glm`,
using the user's selected `glm-5.3-flash` model. Preserve that session/model.

**Handoff status: delivered and acknowledged.** Socket access was restored after the
session permissions changed. The destination has been verified as tab `wA:tA`
(`glm`), pane `wA:pR`, running pi v0.87.1 with `(zai) glm-5.3-flash`, high
thinking, in `/home/mudrii/src/compme`. Codex monitors from pane `wA:pQ`.
The original baseline diff hash and HEAD remained unchanged at dispatch.
Herdr reported the destination working; its output acknowledged the authorized
scope and baseline before reading this brief. The prompt's 10-second wait timed
out while the turn continued; the observed acknowledgement verifies delivery.
Batch 0 integration and independent review are complete; see the
[independent review record](2026-09-29-glm-independent-review.md).
The Linux Host Gate passed all 44 commands. G7 runtime work, native Mac gates,
and release validation remain pending; the changes are uncommitted.

Default scope is the macOS release-preparation milestone discussed with the
owner. Windows and Linux follow-on implementation is described below as a
separate scope option; it is not automatically included in this batch.

## Starting point and protected work

- Repository: `/home/mudrii/src/compme`, branch `main`.
- Starting HEAD: `12d581555dcbcaa811654f9e4a98e998bf7383b9`.
- Existing tracked diff: 15 files, +43/-153 lines, from the authorized test
  consolidation. Preserve these edits and all existing `.gate/` evidence.
- Existing untracked report: `docs/TEST-CONSOLIDATION-2026-09-26.md`.
- Local snapshot: `/tmp/compme-glm-20260929-baseline.patch`, SHA-256
  `cd158c0d6eb386e1e2aa6fc3213f50604a98f96032846b98ced40ddc24edb956`.
  This temporary snapshot excludes untracked files; refresh/preserve those
  separately before editing. Do not apply the snapshot over the current tree.
- The last recorded Linux Host Gate passed 44 commands, 1881 tests and one
  doctest, with a macOS all-targets cross-check. Full Local Gate stopped on
  Apple's `objc2` dependency on Linux. These are historical results, not a pass
  for new work.
- Remote Git/CI could not be refreshed on September 29. Recheck exact remote
  HEAD and CI when access is available; earlier green CI excludes local edits.

Read `AGENTS.md`, `docs/ROADMAP.md`, `docs/DEVELOPMENT.md`,
`docs/ARCHITECTURE.md`, `docs/ACCEPTANCE.md`, and `Qfd.md` §22 before changes.
Use CodeGraph before searching/reading code to locate implementation seams.
The repository brief governs workflow; this document is the task specification.

## Multi-agent assignments

The GLM coordinator must actually dispatch separate workers, report their
assignments, and integrate their results. Inspect the installed pi delegation
capabilities rather than assuming a subagent command exists. If delegation is
unavailable, report that limitation; do not describe serial work as multi-agent.

| Agent | Ownership | Deliverable |
|---|---|---|
| Coordinator (existing glm session) | Integration, shared docs, gate runs | Baseline preservation, agreed interfaces, combined diff, exact test evidence |
| A: Carbon implementation | `platform_macos` hotkey/worker code | Call/ownership analysis; G7 implementation only after the physical baseline prerequisite |
| B: Regression tests | Hotkey regression scenarios | Adversarial tests against the actual implementation, coordinated with A |
| C: Acceptance and release preparation | Existing acceptance tooling/docs | Coverage-to-gate mapping, executable Mac validation sequence, truthful blocked-state report |
| Codex reviewer | Read-only review, then fix requests to glm | Independent standards and specification reviews; rerun relevant checks |

A and B share potential test files. Agree ownership before editing: B may
prepare scenarios while A edits production files; integrate test-file changes
serially. Only the coordinator edits ROADMAP/count surfaces or runs Git writes.
Use the current checkout; create no branches/PRs. Serialize builds using the
same target directory. Do not let independent agents overwrite one another.

## Batch 0 — establish an auditable baseline

1. Capture HEAD, current diff, untracked-file inventory, host, toolchain, and
   current agent assignments. Verify all pre-existing cleanup edits survive.
2. Review the consolidation report and native-test inventory changes. Run the
   applicable documented gate; fix regressions introduced by this batch.
3. Inspect actual physical-baseline evidence for the current macOS build.
   `always-on-hotkeys-physical-look` and accept/dismiss/cycle/rearm are required
   before G7 changes production registration behavior (Qfd §22.4).
4. If no granted Mac/baseline is available, complete the call graph, test
   scenario design, existing-test improvements where independently useful, and
   the Mac execution handoff. Keep G7 production routing unchanged and identify
   the exact missing evidence. Do not add a disconnected test-only state model
   and call it G7 coverage.

Completion: a preserved baseline, an explicit Mac availability/baseline result,
and a reviewed ownership/test plan. A blocked native prerequisite does not
prevent independent documentation or existing-test work.

## Batch 1 — G7 Carbon main-thread registration

Prerequisite: record the physical baseline above on the current build. This
plan does not waive it. Use the design of record in Qfd §22; line numbers there
are historical, so locate current symbols:

- `MacosAdapter::accept_tap_installer`
- `AxWorkerHandle::install_resource` and `WorkerResource`
- `install_worker_accept_tap_resource`
- `install_carbon_accept_hotkeys`, `install_process_shortcut_hotkeys`
- `WorkerAcceptTapResource`, `WorkerShortcutResource`
- `ensure_carbon_handler_installed`, the two handler slots, and arm IDs

### Behavior and ownership

1. Compute the pure key-registration plan outside the worker closure.
2. Perform Carbon handler installation and hotkey registration/unregistration
   on main. Apply inline when already on main; schedule asynchronously from
   other threads using the existing dispatch dependency.
3. Keep raw Carbon references in main-thread-owned registries, separated for
   per-suggestion accepts and process-lifetime shortcuts. Keep worker resource
   ownership via an ID-only token consistent with MVP §2.
4. Prove the wait graph has no worker-to-main synchronous wait. Main already
   waits on the worker: adding main `exec_sync` from that worker deadlocks.
5. Replace FIFO assumptions with arm-ID ownership: replacing an arm unregisters
   the older arm before registering the replacement; delayed destruction of an
   older token cannot disarm the newer arm.
6. Preserve error semantics: consumer registration failure must not report a
   working accept tap; shortcuts retain their documented collision/skip rules.
   Specify off-main completion/error delivery explicitly. Queuing is not proof
   that registration succeeded. Resolve this contract before implementation.
7. Handle partial registration rollback, empty plans, handler-install failure,
   repeated teardown, delayed callbacks, and shutdown without leaks or stale
   handler delivery. Preserve independent shortcut lifetime across suggestions.
8. Justify every new unsafe boundary and Send/Sync assertion; prefer ID-only
   cross-thread messages. Do not transport raw Carbon handles to workers.

### Regression requirements

Tests must invoke the same ownership/dispatch logic that production uses, with
injected executors and Carbon operation sinks. Include:

- Main-thread calls apply inline; off-main calls use the executor.
- `unregister(old)` occurs before `register(new)`.
- A queued stale unregister leaves a newer handler and arm intact.
- Drop requests teardown exactly once for its owned ID.
- Failure after partial registration cleans up only resources it owns.
- Registration failure propagates correctly; a later retry works.
- Suggestion accept teardown leaves process shortcuts live.
- Shutdown with queued operations cannot reactivate an obsolete arm.
- Existing key IDs, action mapping, Tab suppression, collision behavior,
  and correction/full/word acceptance remain unchanged.

Use deterministic scheduling instead of sleep-based race assumptions. Show
red-before/green-after evidence for non-trivial behavior. Run native macOS
tests serialized. Add an integration assertion that proves production reaches
the tested seam; testing a parallel fake implementation is insufficient.

### Documentation and acceptance

Amend the MVP thread-ownership statement in the same change as actual routing;
do not declare the new behavior before implementation. Update Qfd/ROADMAP and
test inventories based on measured native counts. Re-run the physical baseline
gate set after G7, recording build/commit, date, result and evidence paths.

Completion: implementation and regression gates pass, exact-commit macOS CI
passes, and before/after physical evidence exists. Cross-compilation alone is
recorded as partial validation.

## Batch 2 — macOS release-candidate validation

Use existing tooling rather than a new acceptance framework. Agent C maps each
of the 22 ledger rows and the additional manually recorded Tier-4 rows from
ROADMAP to prerequisites, commands/manual actions, expected observable behavior
and evidence destination. Prioritize physical hotkeys, browser caret calibration,
memory erasure/privacy, grammar replacement, the nine-tab Settings walkthrough,
Setup's single-location-control invariant, and Apps/Personalization. Preserve
unsupported and non-atomic replacement boundaries until actual compatibility
evidence justifies a scoped change.

On a model-capable Mac with a granted GUI session:

```sh
tools/dev/check.sh
tools/acceptance/run-a1b-live-gates.sh
bash tools/release/run-model-gates.sh
bash tools/release/check-quality.sh
```

Consult the runner's help and ACCEPTANCE preconditions before execution; supply
the documented target-app parameters for applicable gates. The first command
already includes model/quality gates; do not rerun expensive steps when valid
same-candidate evidence exists. Physical manual rows are separate from runner
self-tests. The strict latency gate retains its default required budget.

Record only actual observations. Unavailable GUI/hardware/model artifacts mean
pending, never pass. Fix observed failures, then rerun affected and required
gates. Version bump, tagging, signing/publication and cask finalization are a
later release operation, not authorized by this development handoff.

## Follow-on scope options — separate batches

If the owner selects Windows development, read
`docs/superpowers/specs/2026-07-08-cross-platform-implementation-plan.md` and
Qfd §23 before implementation. The current foundation has read-only UIA; retain
its window-less MTA worker and bounded call contract.

1. Implement focused-field caret/range geometry with Unicode-correct offsets,
   explicit unsupported cases, and stale identity refusal.
2. Add focus/text/caret subscriptions with bounded teardown, cancellation and
   late-callback suppression. Native provider tests are required.
3. Add safe insertion/replacement, checking identity, selection, secure state,
   mutation outcome and readback. Never blindly retry an uncertain mutation.
4. Add accept-key hooks, then the suggestion/correction overlay, then integrate
   startup/teardown and capability reporting. Keep unsupported surfaces honest.
5. Validate on Windows hardware before packaging or usability claims.

Split workers by UIA geometry/events, insertion safety, and hooks/overlay; agree
shared trait/event contracts first and integrate sequentially. Preserve
host-neutral portable tests and obtain native Windows CI for Windows changes.

Linux follow-on work is real-desktop tray/shortcut/AppImage acceptance and fixes
found there. Compare GNOME/KDE/sway Wayland capabilities in actual sessions
before selecting an implementation; Xvfb results are not Wayland evidence.
Do not batch all platform features into the macOS G7 change.

## Integration, monitoring and independent audit loop

The coordinator reports phase transitions, worker assignments, touched files,
gate commands/results, blockers and final changed-symbol summary in the glm
session. A request being submitted does not prove a turn started or completed.

Codex monitors only the discovered pane belonging to tab `glm`, using Herdr
agent identity/read/wait commands. Inspect output after timeouts or unknown
states; avoid duplicate prompts. When implementation settles, capture the
current diff and distinguish new changes from the saved pre-existing cleanup.

Run two independent read-only reviews:

1. **Standards:** AGENTS rules, ownership/lifetime safety, minimal interfaces,
   failure handling, tests, doc/count consistency and native validation claims.
2. **Specification:** every requirement in the active batch, real production
   integration, stale-arm races, partial failure, error delivery and acceptance
   evidence. Include functional/security findings, not just style.

For each finding give severity, file/line, reproducible scenario or failing
test, and required behavior. Send the numbered actionable findings back to glm;
glm fixes them and supplies regression evidence. Review the resulting delta,
rerun relevant tests, and repeat until no actionable findings remain or an
external prerequisite blocks progress. Report blocked checks explicitly.

The final report separates: implemented, locally verified, native-CI verified,
live accepted, and still pending. Commit only under AGENTS' required gate rules;
if a mandatory gate is unavailable, preserve the reviewable working tree.
Passing an audit does not itself authorize tagging or publication.
