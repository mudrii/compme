# GLM Batch 0 independent review — 2026-09-29

Codex monitored Herdr tab `wA:tA` (`glm`), pane `wA:pR`, through three
independent pi worker reports, coordinator integration, and a fix/review loop.
The coordinator used the requested `zai/glm-5.3-flash` model. Independent
Standards and Spec reviewers checked the reports and integrated artifacts.

## Outcome

The Linux-feasible Batch 0 work is reviewed. It adds one production-connected
startup shortcut collision test and three preparation documents:

- [Ownership analysis](2026-09-29-g7-call-ownership-analysis.md)
- [Regression specification](2026-09-29-g7-regression-scenarios.md)
- [Mac validation runbook](2026-09-29-mac-release-validation-handoff.md)

The G7 runtime implementation is not part of the completed work. Its physical
Mac baseline prerequisite remains unfulfilled. No commit, push, tag, or release
was made; HEAD remains `12d581555dcbcaa811654f9e4a98e998bf7383b9`.

## Findings returned and verified

The coordinator corrected unsupported historical-growth and coverage claims,
unnecessary unsafe-Send guidance, the proposed harness's bypass of production,
premature async success, incomplete shutdown assertions, incorrect default
bindings, and main-thread test assumptions. The runbook now includes all 22
ledger rows, the remaining OCR/multiple-display scope, and committed-evidence
ordering. Its logging, ledger selection, shell portability, and missing-variable
errors were corrected through repeated review.

The final source test exercises `Config` through `apply_startup_key_bindings`
and the host shell's effective bindings. Two colliding roles plus two distinct
roles prove whole-set rejection; the existing successful four-role test supplies
the positive control. `KeyBindingsGuard` restores shared state.

No material finding remains in the reviewed local changes. This is not native
validation or release approval.

## Verification evidence

- Linux Host Gate: **44 commands run, 0 skipped**, including the new test and
  its positive control, serial app tests, fmt/clippy, rustdoc, script checks,
  and the `platform_macos` all-targets cross-check.
- Log: `.gate/glm-batch0-20260929-linux.log`; SHA-256
  `a1549fb8544482d941165bb6fd3afb9bc3655615205fbafe166aa79f9eabe448`.
  Codex inspected the named passing tests and gate completion and independently
  verified this hash. There are 649 passing app tests across its targets;
  two app helper tests are ignored. A separate doctest passes.
- Executable runbook blocks pass Bash syntax validation. Coordinator fixture
  runs exercised missing variables, multiple ledger files, generation failure,
  successful selection, and checker rejection. These are shell-template checks,
  not live Mac acceptance results.
- `git diff --check` passes. A temporary Git index reconstructed the protected
  starting diff for comparison; the existing consolidation edits are preserved.
  New tracked work is the collision test, current inventory restamps, and the
  ROADMAP batch record. Historical consolidation counts remain historical.

Full Local Gate on macOS, native CI for the local candidate, physical acceptance,
model/quality execution on Mac, and release verification remain outstanding.
Linux cross-compilation cannot discharge those requirements.
