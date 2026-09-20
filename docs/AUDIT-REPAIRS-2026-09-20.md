# Full-codebase audit repairs — 2026-09-20

The audit reviewed `2e25460dbfd388cd545cbf85f8c7cabaa212d5a7` across source,
tests, documentation, and roadmap alignment. The owner authorized its seven
code defects and six documentation findings. GPT-5.6 Sol implemented the
repairs and Astra reviewed them independently. Current pending work remains
in [ROADMAP](ROADMAP.md#full-codebase-audit-repairs--2026-09-20).

## Code and regression evidence

| Finding | Repair and behavioral regression |
|---|---|
| C1: private field text in Linux replacement errors | Generic refusal diagnostics; the existing regression now asserts that neither current nor expected private text appears in the error. |
| C2: Linux selection snapshots duplicate selected text | Left/right context excludes the selection and offsets use its start; live tests reconstruct forward, backward, and astral-Unicode selections exactly. |
| C3: edits lost during replacement preparation | Re-read the complete field after preparing the editable proxy; a shared production orchestration regression injects edits inside/outside the target range and proves zero writes and preserved user input. |
| C4: native abort for prompts larger than a decode batch | Decode chunks respect the actual native batch capacity, preserving positions, final logits, cancellation, and cache handling. A subprocess regression uses the same 3,000-repeat prompt that aborted the old implementation, then exercises prefix reuse and the original small-context comparisons. |
| C5: retention failure leaves a newly inserted memory row | Insert and retention trimming share one SQLite transaction; a real failing delete trigger proves rollback through public count/retrieval behavior. |
| C6: unbounded accessibility connection/subscription setup | Bound session/accessibility authentication and registry/match setup. A stalled Unix authentication socket exercises production setup; a delayed registration result proves that timeout prevents worker startup and drops late resources. |
| C7: queued X11 callbacks survive cancellation | Gate delivery on the tap's cancellation state. The existing live teardown test blocks one callback, queues another, drops the real tap, and proves the queued callback never runs after cancellation returns. |

C3 closes the avoidable preparation window. AT-SPI's full-value write has no
compare-and-set operation, so this does not establish atomicity against an
edit between the final read and native write.

## Documentation corrections

- D1: label the old coverage percentages as historical measurements that
  include inline tests; do not present them as current production-only coverage.
- D2: require relaunch after granting Accessibility when startup could not
  install subscriptions.
- D3: add native memory-control acceptance steps for live mode changes, Off,
  existing/missing stores and keys, app/global erasure, and domain erasure.
  Keep the existing 22 macOS gate IDs and require actual native evidence.
- D4: mark buffered memory erasure implemented while retaining its pending
  native Apps-pane acceptance.
- D5: separate sandbox teardown from real-desktop post-exit typing checks.
- D6: tie structural extractions to concrete testing or second-shell needs.

## Validation boundaries

The integrated portable run passed **1,865 tests**, with **50 ignored**;
the provisioned Linux X11/AT-SPI harness separately passed **41/41** live
tests. Formatting, portable all-target clippy with warnings denied, builds,
doctests, and rustdoc with warnings denied passed. Shell syntax, shellcheck,
actionlint, cask syntax, documentation/policy checks, and script self-tests
passed where runnable. The icon self-test requires macOS/Swift. Dependency
auditing passed with the previously documented allowed `ttf-parser`
unmaintained warning. [Native CI](https://github.com/mudrii/compme/actions/workflows/ci.yml?query=branch%3Amain)
records the macOS, Windows, Linux, and separate spike gates for each pushed
commit; check the run's revision when using its result as evidence.

The final C4 long-prompt regression passed on Linux CPU (**240.90 s**) and
Vulkan (**23.30 s**). These are whole-test runtimes, including setup and
multiple completions, not product latency measurements or latency acceptance.

This record does not claim full path coverage, native GUI acceptance,
release readiness, or a new release.
The canonical local gate on Linux stops at step 2 because Apple framework
dependencies cannot compile for the Linux host. Portable checks and native
host CI are recorded separately; GUI and physical-input observations still
belong in [ACCEPTANCE](ACCEPTANCE.md).
