# 2FIX implementation record

This file records what actually changed for each active item in `2FIX.md`.
It is evidence, not a second plan: unresolved scope and ordering remain in
`2FIX.md`; `docs/ROADMAP.md` remains the project status source of truth.

Status meanings:

- **Pending** — no implementation evidence recorded yet.
- **In progress** — code is being changed or focused verification is incomplete.
- **Code complete** — deterministic implementation and focused checks pass;
  named live/CI evidence may still be outstanding.
- **Verified** — every item-specific deterministic and live/CI obligation that
  can close the item has passed.
- **Posture recorded** — the finding intentionally closes with a documented,
  tested policy rather than a behavior change.

## Status index

| WP | Items and status |
|---|---|
| WP1 | A41 **Verified**; A50 **Code complete** |
| WP2 | A61 **Verified** |
| WP3 | A1 **Code complete** |
| WP4 | A11, A17, A51 **Code complete** |
| WP5 | A3, A6, A12, A22, A28, A33, A34, A40 **Code complete** |
| WP6 | A7 **Verified**; A10, A47 **Code complete** |
| WP7 | A8, A9, A42, A43, A58, A60, A63, A68 **Verified on Linux** |
| WP8 | A25, A30 **Code complete** |
| WP9 | A18, A31, A62 **Code complete** |
| WP10 | A2, A4, A5, A20, A21, A29 **Code complete** |
| WP11 | A15, A73, A74 **Code complete** |
| WP12 | A14, A23, A24, A35, A36, A37, A49, A54, A55, A56 **Code complete**; A53 **CI shape complete, real-tag proof pending** |
| WP13 | A32, A38, A48 **Code complete**; A44 **Posture recorded** |
| WP14 | A19, A26, A52, A57, A64, A65, A67, A71 **Code complete**; A66 **Posture recorded** |
| WP15 | A13, A39, A59, A69, A70 **Code complete** |

## Final aggregate validation — 2026-08-25 through 2026-08-26

- The finding inventory is exact: `2FIX.md` and this ledger each contain 69
  unique active IDs, with no missing or duplicate ID. All 69 plan items are
  checked.
- The final TDD/BDD gap pass closed five missing negative-path scenarios
  without adding findings: comment-only/no-op Linux workflow commands, a
  comment-only/no-op docs privacy step, credential lookup failure, failed X11
  live-rebind rollback, and release-helper argument validation. The workflow
  policy mutation first failed red when the comment-only command was accepted,
  then passed after the checker required the exact executable command.
- The checker-pinned macOS workspace count is restamped from 2,027 to 2,060:
  +16 portable-crate, +12 app, +3 host-portable `platform_linux`, and +2
  `platform_macos` tests. Linux-only additions are excluded from that total.
- Portable workspace clippy, tests, build, and strict rustdoc passed. The app
  lane enumerated 558 unit tests: 556 passed and two subprocess helpers remained
  intentionally ignored. Its isolated startup integration test passed from an
  RPATH-linked target that preserves its intentional clean environment. The
  macOS adapter passed cross-compilation check and clippy for
  `aarch64-apple-darwin`.
- Linux passed 106 package tests plus all 34 ignored live tests under the live
  harness. The C2 probe observed two overlay shows with one superseded request,
  confirming that stale completion rather than a duplicate field identity was
  the reproduced cause.
- The root CPU real-model suite passed with the documented hosted-runner latency
  opt-out, the quality corpus passed, and the exact A32 CPU case returned typed
  cancellation within 250 ms after llama.cpp polled the abort callback. The
  separate strict 500 ms warm-latency
  budget measured 1,871 ms on this CPU-only host and remains a real-Mac pre-tag
  obligation. The Darwin Metal cancellation command is checker-pinned and
  self-tested, but still needs execution on a real Mac. Workflow-policy mutation
  tests, actionlint, privacy and brief checks, release helper self-tests, shell
  syntax, ShellCheck at error severity, and the pinned `cargo-audit` 0.22.2
  check passed; only the documented allowed RUSTSEC-2026-0192 warning remains.
- The literal `tools/dev/check.sh` cannot complete on this Linux host: its
  workspace clippy/build/doc commands compile Apple frameworks, the bundle/icon
  checks require the macOS Swift toolchain, and the standalone spike is macOS
  specific. Those are recorded host boundaries, not claimed passes. A53 still
  requires a real tag, and macOS/Windows GUI or hardware evidence remains
  external where each item says so.

## Per-item record

### A41 — Linux startup without an AT-SPI bus

- **Status:** Verified on the Linux host.
- **Changed:** added `PlatformError::AccessibilityUnavailable { reason }`;
  Linux session-backed methods now distinguish a missing AT-SPI service from
  a genuinely unsupported operation. Startup maps only that typed error to an
  distinct `AccessibilitySubscriptions::Unavailable` state and
  `BlockReason::AccessibilityUnavailable`, logging the original reason and
  rendering no false macOS permission/relaunch instruction. Trusted
  `UnsupportedField`, timeout,
  secure-input, and other subscription failures remain fatal.
- **Files:** `crates/platform/src/lib.rs`, `crates/platform_linux/src/lib.rs`,
  `crates/app/src/run_loop.rs`, `crates/app/src/run_loop_tests.rs`.
- **Red evidence:** the new classifier regression initially failed to compile
  because neither `AccessibilityUnavailable` nor the `Unavailable` action
  existed (`E0599` for both variants).
- **Green evidence:**
  `cargo test -p app accessibility_service -- --test-threads=1` passed both
  `unavailable_accessibility_service_degrades_without_masquerading_as_permission`
  and
  `startup_survives_missing_linux_accessibility_service_without_permission_prompt`
  (2 passed, 0 failed). The command ran in the repository's Nix native-build
  environment with the staged Rust 1.97 toolchain.
  `cargo test -p platform_linux -- --test-threads=1` then passed 100 tests
  with 27 live tests ignored. A real bounded Linux run under Xvfb with an
  intentionally invalid D-Bus address printed the typed focus/caret
  unavailable diagnostics, reached `compme: running`, and exited cleanly after
  `COMPME_RUN_MS=1000`; the harness reported PASS.

### A50 — trailing shell operator panic

- **Status:** Code complete; portable aggregate gates and the test-count
  restamp passed.
- **Changed:** `compat::is_go_command` now obtains the first token with
  `tokens.first()` and returns `false` for an empty slice. No caller behavior
  or strong-operator parsing order changed.
- **Files:** `crates/compat/src/lib.rs`.
- **Red evidence:**
  `terminal_handles_lines_ending_in_shell_operators_without_panicking`
  reproduced `index out of bounds: the len is 0 but the index is 0` at the old
  `tokens[0]` access.
- **Green evidence:** `cargo test -p compat` passed 47 tests, 0 failed;
  `cargo fmt -p compat -- --check` and `git diff --check` passed.
- **Tests added:** public terminal-policy regression covering trailing `|`,
  `||`, `;`, `&&`, `<`, `2>`, and `2>>`; direct empty-token guard regression.
- **Count impact:** +2 portable tests; recompute and re-stamp the aggregate
  workspace anchors after the current parallel batch.

### A61 — over-cap Linux field data-loss guard

- **Status:** Verified, including the live GTK regression.
- **Changed:** `read_context` and `insert_replacing_range` now share a checked
  whole-field read. It queries AT-SPI `CharacterCount` before `GetText`, maps
  query failure, negative count, and values above 200,000 to a fail-closed
  error, and rechecks the fetched scalar length to close a growth race. No
  capped prefix can reach a whole-field replacement.
- **Files:** `crates/platform_linux/src/atspi_live.rs`,
  `crates/platform_linux/src/atspi_live_tests.rs`.
- **Red evidence:** the headless boundary regression initially failed to
  compile with `E0425` because `checked_field_scalar_count` did not exist.
- **Green evidence:** focused boundary test, `cargo check -p platform_linux`,
  all-target clippy with warnings denied, and the targeted AT-SPI live harness
  all passed. The combined package run passed 100 tests with 27 ignored.
- **Live proof:** the GTK fixture seeded 200,001 scalars; both read and range
  replacement refused it, and the post-operation character count remained
  200,001.
- **Count impact:** +1 portable test and +1 ignored Linux live test; the live
  count changes from 26 to 27 and must be pinned by A1.

### A25 — forbid HTTPS downgrade

- **Status:** Code complete; portable aggregate gates passed.
- **Changed:** the production ureq agent is built with `.https_only(true)`.
  Plaintext loopback fixtures use a private injected test agent; the production
  downloader remains HTTPS-only.
- **Files:** `crates/model_fetch/src/lib.rs`.
- **Red evidence:** `production_agent_rejects_plaintext_requests_and_redirects`
  observed `https_only() == false` before the change.
- **Green evidence:** included in the 50-test `model_fetch` all-target run;
  clippy with warnings denied and crate formatting passed.

### A30 — terminal download cleanup and held-handle verification

- **Status:** Code complete; portable aggregate gates passed.
- **Changed:** terminal hash mismatch and every size-exceeded exit remove the
  `.part`; resumable transport failures retain it. An exact-cap part with an
  expected hash is verified and promoted without a network retry. Download
  parts open read/write with the existing no-follow/reparse protections, are
  synced and rewound, and are hashed through that held handle before rename.
  The public error/caller API is unchanged.
- **Files:** `crates/model_fetch/src/lib.rs`.
- **Red evidence:** regressions proved hash mismatch and streamed cap failure
  left their part files, while an exact-cap complete part attempted a network
  request and failed connection.
- **Green evidence:** `cargo test --locked -p model_fetch --all-targets`
  passed 50 tests; crate doc tests, clippy with warnings denied, formatting,
  and diff checks passed. Tests cover retry-after-mismatch, all size-terminal
  cleanup paths, exact-cap promotion, and Unix pathname swap rejection.
- **Count impact:** A25/A30 together add 3 host tests; aggregate workspace
  count anchors were re-stamped and the policy checker self-test passed.

### A11 — pinned docs and privacy checks reach a CI lane

- **Status:** Code complete; push/macOS checker evidence remains pending.
- **Changed:** CI now ignores only `docs/superpowers/plans/**`, so pinned specs
  trigger the full lane. The mirrored docs lane uses the same narrow path and
  runs `check-privacy-policy.sh`. The release checker pins both invariants and
  mutates widened paths and a removed privacy step in self-test fixtures.
- **Files:** `.github/workflows/ci.yml`, `.github/workflows/docs.yml`,
  `tools/release/check-model-gates.sh`.
- **Red evidence:** the checker-first run rejected the old workflow with
  `missing release gate: CI push trigger skips only unpinned prose`.
- **Green evidence:** `check-model-gates.sh --self-test` passed again under a
  Ruby-enabled Nix shell; the assigned package also passed actionlint 1.7.12,
  privacy checker plus self-test, Bash syntax, and diff checks. A follow-up
  mutation proves a comment-only/no-op privacy step is rejected.

### A17 — complete 22-ID manual-gate pin

- **Status:** Code complete; aggregate policy/self-tests passed, with hosted
  push execution still external.
- **Changed:** one checker helper derives the runner self-test ID set, requires
  exactly 22 unique IDs, exact-diffs it with ACCEPTANCE, and requires every ID
  in MANUAL-VALIDATION. Mutation fixtures independently remove a formerly
  omitted ID from each surface.
- **Files:** `tools/release/check-model-gates.sh`.
- **Green evidence:** the extended model-gate self-test passed and the live
  source sets were accepted only at 22/22/22.

### A51 — job permissions and action provenance are pinned

- **Status:** Code complete; push/CI evidence pending.
- **Changed:** all five CI jobs must inherit workflow read-only permissions;
  audit's job permissions are exact; docs must inherit; action provenance is
  validated for CI, release, audit, and docs. Per-job permission mutations and
  audit/docs mutations prove the checks bite.
- **Files:** `tools/release/check-model-gates.sh`.
- **Red evidence:** an initial mutation was accepted and failed the new test as
  `unnecessary CI spike permission was accepted`.
- **Green evidence:** model-gate self-test and actionlint passed.

### A7 — foreign Linux caret events cancelled the debounce

- **Status:** Verified on the Linux host, including the C2 application probe.
- **Changed:** caret delivery asks the shared focus registry for the current
  element before metadata, geometry, minting, or callback. A focus race during
  geometry is revalidated before callback. Dropped-event diagnostics exist only
  with `COMPME_DEBUG`; temporary pending/tick probe accessors were removed after
  the session.
- **Files:** `crates/platform_linux/src/atspi_events.rs` and the shared registry
  files listed under A10.
- **Live proof:** a private AT-SPI/Xvfb GTK session focused the fixture, typed
  ` hello ` with xdotool, and ran a deterministic stub. Logs showed the debounce
  survive the same-field caret echo, two `request gen=` submissions, two
  completion outcomes, and final usage `shown=2 accepted=0 dismissed=0
  superseded=1`. This closes the prior ROADMAP `shown=0` diagnostic.

### A10 — one Linux field identity authority

- **Status:** Code complete; macOS/Windows build lanes remain pending.
- **Changed:** `LinuxFieldRegistry` is owned by the adapter and shared by focus,
  caret, and all AT-SPI I/O. Focus alone mints; same-element caret reuses;
  foreign caret drops; A→B→A invalidates the first A generation. Capabilities,
  read, caret, insert, and exact-range replacement validate identity plus
  generation before I/O and return `StaleField` on mismatch.
- **Files:** `crates/platform_linux/src/atspi_event_map.rs`,
  `crates/platform_linux/src/atspi_events.rs`,
  `crates/platform_linux/src/lib.rs`,
  `crates/platform_linux/src/atspi_live_tests.rs`.
- **Red evidence:** the first registry regression failed with `E0433` because
  `LinuxFieldRegistry`/`ElementId` did not exist at the seam.
- **Green evidence:** `platform_linux` passed 104 portable tests with 28 live
  cases ignored in the ordinary run; all 28 live cases then passed in the
  Xvfb/AT-SPI harness. All-target clippy with warnings denied and formatting
  passed.
- **Count impact:** +2 portable tests and +1 ignored Linux live test.

### A47 — one Linux subscription ID source

- **Status:** Code complete; cross-host CI lanes pending.
- **Changed:** accessibility event subscriptions and the X11 accept tap now use
  the single crate-level process-wide counter.
- **Files:** `crates/platform_linux/src/atspi_events.rs`,
  `crates/platform_linux/src/lib.rs`.
- **Green evidence:** the distinct-ID regression is included in the 104-test
  package run and the live suite remained 28/28 green.

### A18 — worker panic containment and health

- **Status:** Code complete; Linux-testable app/model boundaries pass and the
  macOS code cross-checks, while the AX regression still requires the macOS
  serial lane.
- **Changed:** the app inference boundary catches panics, records terminal
  `Failed(reason)` health, clears readiness, rejects later submissions, and
  makes the run loop derive `ModelUnavailable`. The llama worker uses the same
  terminal-health pattern and rejects later jobs. AX jobs are independently
  caught and return `CannotComplete { reason: "AX job panicked" }` so the next
  job can run. Each crate uses a non-panicking stderr writer for worker logs.
- **Files:** `crates/app/src/main.rs`, `crates/app/src/inference.rs`,
  `crates/app/src/run_loop.rs`, `crates/app/src/run_loop_tests.rs`,
  `crates/model_client/src/lib.rs`, `crates/platform_macos/src/lib.rs`,
  `crates/platform_macos/src/ax_worker.rs`.
- **Red evidence:** app regressions first failed with missing
  `failure_reason`/`effective_model_available` symbols. The platform test pins
  the pre-fix failure mode but cannot link Apple frameworks on this Linux host.
- **Green evidence:** both app regressions and the model-client injected-panic
  regression pass. `cargo check --locked -p platform_macos --all-targets
  --target aarch64-apple-darwin` passed with Rust 1.97 after installing that
  standard-library target. The AX test is present but remains unexecuted here.

### A31 — observer-rebind failure visibility

- **Status:** Code complete; the Apple-target cross-check passes, but the
  regression still needs the macOS serial lane.
- **Changed:** failed observer installs log through the non-panicking writer at
  most once per pid per 30 seconds. Expired pid entries are pruned on a new
  failure, bounding short-lived-process growth.
- **Files:** `crates/platform_macos/src/ax_worker.rs`.
- **Tests added:** `observer_rebind_failure_logging_is_rate_limited_per_pid_and_pruned`.
- **Green evidence:** the full `platform_macos` crate and all targets passed an
  `aarch64-apple-darwin` cross-check; runtime test execution remains macOS-only.

### A62 — callbacks run outside macOS identity/coalescer locks

- **Status:** Code complete; implementation cross-checks and the named
  regression awaits macOS execution.
- **Changed:** focus identity/tracker guards and caret tracker/coalescer guards
  are scoped to state computation and dropped before subscriber invocation.
- **Files:** `crates/platform_macos/src/lib.rs`,
  `crates/platform_macos/src/lib_tests.rs`.
- **Tests added:**
  `panicking_subscribers_do_not_poison_later_focus_or_caret_delivery`.
- **Green evidence:** the full `platform_macos` crate and all targets passed an
  `aarch64-apple-darwin` cross-check; runtime test execution remains macOS-only.

### A39 — encrypted-store schema migration posture

- **Status:** Code complete; posture recorded without speculative migration code.
- **Changed:** the memory module documents the 0.x schema as immutable; the
  first schema change must introduce `PRAGMA user_version` and a transactional
  migration before changing table definitions.
- **Files:** `crates/memory/src/lib.rs`, `docs/ROADMAP.md`.

### A59 — three small accuracy corrections

- **Status:** Code complete; portable aggregate gates passed.
- **Changed:** memory pagination stops on the actual fetched count versus the
  effective SQL page, the target-specific llama exact-pin comment now describes
  Metal versus off-mac CPU-only configuration accurately, and stats pruning
  documents its monotonic-time assumption and safe out-of-order behavior.
- **Files:** `crates/memory/src/lib.rs`, `crates/model_client/Cargo.toml`,
  `crates/stats/src/lib.rs`.
- **Green evidence:** memory passed 49 tests, stats 57, strict rustdoc passed for
  both, and affected-crate clippy passed with warnings denied.

### A69 — preserve non-UTF-8 reveal paths

- **Status:** Code complete; portable aggregate gates passed.
- **Changed:** internal `file_uri_bytes` and byte-parent routing preserve native
  Unix path bytes; `xdg_open` accepts `OsStr`, with lossy conversion restricted
  to diagnostics.
- **Files:** `crates/platform_linux/src/reveal.rs`,
  `crates/platform_linux/src/lib.rs`.
- **Red evidence:** the helper-first regression failed with `E0425` for missing
  byte-preserving helpers.
- **Green evidence:**
  `non_utf8_path_bytes_survive_uri_and_parent_routing` passed; the existing live
  reveal regression remained green.

### A70 — deterministic Linux font search order

- **Status:** Code complete; portable aggregate gates passed.
- **Changed:** candidates sort by font rank, caller-provided search-directory
  index, then path, so equal-rank user fonts beat later system directories.
- **Files:** `crates/platform_linux/src/overlay_font.rs`.
- **Red evidence:** the regression selected the lexically earlier system path
  instead of the earlier search directory.
- **Green evidence:**
  `equal_rank_fonts_follow_search_directory_order_before_path` passed.
- **Count impact:** A69/A70 add +2 tests on Unix (+1 on Windows).

### A32 — bounded inference shutdown

- **Status:** Code complete; CPU real-model and portable fault-injection gates
  pass. The Darwin gate is wired to execute the same cancellation test with
  Metal; execution evidence requires a macOS host.
- **Changed:** added cloneable terminal model cancellation plus a typed,
  prompt-free `ShutdownRequested` error. `LlamaModel` checks it around every
  candidate/decode/success path. A minimal vendored patch to exact-pinned
  `llama-cpp-2` safely installs llama.cpp's abort callback: callback state owns
  the cancellation atomic and monotonic poll counter and remains retained by
  `LlamaContext` until after native context free.
- **Outer lifecycle:** inference separately closes submissions and sets a stop
  flag, so queued screen-wait work cannot start. Model ownership remains outside
  panic containment; explicit `model.shutdown()` precedes the stopped ack.
  Shutdown waits for both ack and `JoinHandle::is_finished()` under one 250 ms
  deadline, then joins without an unbounded post-ack race.
- **Forced policy:** timeout produces a must-use result. The run loop explicitly
  drops tray/subscriptions/engine/adapter, arms a last-drop termination guard
  declared before startup, and starts a watchdog. When watchdog spawn succeeds,
  normal scope drops—including AES key zeroization, memory, clipboard, OCR,
  downloader, settings, and instance lock—run first; then Unix `_exit(70)` or
  Windows `TerminateProcess(..., 70)` bypasses C `atexit` and abort/core-dump
  behavior. The watchdog covers a stuck later destructor; spawn failure exits
  immediately. The app never continues with a detached llama owner.
- **Files:** root `Cargo.toml`/`Cargo.lock`, `vendor/llama-cpp-2`,
  `crates/model_client/src/lib.rs`, `crates/model_client/tests/latency.rs`,
  `crates/app/{Cargo.toml,src/inference.rs,src/run_loop.rs,src/run_loop_tests.rs}`,
  model gate/checker, and coordinated docs.
- **Red evidence:** the first real CPU GGUF test timed out after 250 ms while a
  single prompt decode was still inside native code, proving token-boundary
  polling alone insufficient. Before the checker hardening, the model wrapper
  had no safe callback API.
- **Green evidence:** model-client unit tests pass 55/55; all 60 inference tests
  pass; the real pinned CPU GGUF waits until llama.cpp polls the native abort
  callback and returns typed cancellation within 250 ms; blocked-call and
  blocked-model-shutdown fakes select timeout without leaking test threads;
  subprocesses prove both the final guard and watchdog terminate with code 70.
  `run-model-gates.sh --self-test` covers the CPU and Darwin Metal command shapes
  plus a non-Darwin negative assertion; the checker pins that scope and the exact
  acceptance command.
- **Count impact:** +2 model-client unit, +1 ignored real-model, +3 inference,
  and +4 hard-exit subprocess tests.

### A38 — Insert/Replace then Hide ordering contract

- **Status:** Code complete; portable aggregate gates and the count restamp
  passed.
- **Changed:** a table-driven producer test pins immediate mutation→Hide
  adjacency for seven terminal accept paths. The engine consumer comment links
  that producer contract and accurately excludes partial word accept, which is
  Insert→UpdateGhost and intentionally keeps the tap armed.
- **Files:** `crates/engine_core/src/lib.rs`, `crates/engine/src/lib.rs`.
- **Green evidence:** engine_core passed 174 tests and engine passed 93; the new
  contract test was green immediately because it formalizes existing behavior.
- **Count impact:** +1 portable workspace test.

### A44 — Linux subscription drop guarantee wording

- **Status:** Posture recorded; no stronger synchronization added.
- **Changed:** documentation now states the real guarantee: the active gate
  blocks new deliveries after stop, while a callback already past the gate may
  finish after `Subscription::drop`; Drop waits up to two seconds, then returns
  without joining. This is the intended bounded-shutdown tradeoff.
- **Files:** `crates/platform_linux/src/atspi_events.rs`; matching ROADMAP echo
  is part of the coordinated accuracy pass.

### A48 — teardown and memory-hardening documentation

- **Status:** Code complete; documentation and portable aggregate gates passed.
- **Changed:** model-client shutdown docs now say context then model are freed
  while the process-static backend intentionally remains. Memory docs record
  that all 13 permission/symlink hardening implementation/regression cfg sites
  are Unix-only and Windows still needs owner-only ACL/DACL coverage.
- **Files:** `crates/model_client/src/lib.rs`, `crates/memory/src/lib.rs`;
  matching ROADMAP milestone text is part of the accuracy pass.
- **Green evidence:** memory, engine, engine_core, and platform_linux affected
  tests/clippy/rustdoc passed; the model-client change is documentation-only and
  its native crate is covered by the separate model-client build run.

### A13 — Linux terminal process identities

- **Status:** Code complete after the A7/C2 ghost probe closed; Linux live and
  portable aggregate gates passed.
- **Changed:** terminal policy now recognizes the executable basenames emitted
  by Linux `/proc/<pid>/exe`→argv0→comm normalization, including GNOME Terminal,
  Konsole, foot, Alacritty, kitty, WezTerm, XTerm, and other explicit terminal
  processes. `code` remains non-terminal because process identity cannot prove
  that the focused field is VS Code's integrated terminal.
- **Files:** `crates/compat/src/lib.rs`.
- **Red evidence:** the Linux identity regression failed first with
  `gnome-terminal-server must be recognized`.
- **Green evidence:** `cargo test -p compat` passed 49 tests. Regressions verify
  shell-command suppression and natural-language activation for Linux terminal
  identities, plus the explicit `code` exclusion.
- **Count impact:** +2 portable tests.

### A2 — hermetic Full Local Gate runner self-test

- **Status:** Code complete; a macOS self-test run remains pending.
- **Changed:** the self-test now builds an explicit runtime-tools directory
  containing only the host utilities required by the runner and fixtures, then
  uses fixture-bin plus that directory. It no longer inherits `/usr/bin`,
  `/bin`, or the caller's PATH, so missing optional-tool cases cannot pass by
  accidentally finding a host `shellcheck` or `cargo-audit`.
- **Files:** `tools/dev/check.sh`.
- **Red evidence:** the first hermetic run exited 127 because the fixture PATH
  did not provide `bash`, proving the previous host-path dependency.
- **Green evidence:** `tools/dev/check.sh --self-test` passed on NixOS; shell
  syntax validation passed.

### A4 — host requirement for the live model-gate checker

- **Status:** Code complete; documentation-only.
- **Changed:** development prerequisites now distinguish the macOS-only live
  checker, which enumerates macOS-cfg test targets, from its host-agnostic
  `--self-test`.
- **Files:** `docs/DEVELOPMENT.md`.

### A5 — workspace minimum supported Rust version

- **Status:** Code complete; aggregate cross-host CI remains pending.
- **Changed:** the workspace declares Rust 1.97, matching
  `rust-toolchain.toml`, and all 26 member manifests inherit that value. The
  development guide identifies 1.97 as both the MSRV and pinned supported
  toolchain without claiming an untested lower version.
- **Files:** root `Cargo.toml`, all 26 workspace crate manifests,
  `docs/DEVELOPMENT.md`.
- **Green evidence:** `cargo metadata` reported `rust_version = 1.97` for all
  26 packages; strict portable workspace rustdoc passed with the staged 1.97
  toolchain.

### A20 — strict portable rustdoc coverage

- **Status:** Code complete; CI execution remains pending.
- **Changed:** the broken private-constant rustdoc link is plain code text. The
  Linux CI job now runs `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
  --workspace --exclude platform_macos`, and the release policy checker pins
  that exact step with a removal mutation.
- **Files:** `crates/platform_linux/src/x11_tap.rs`,
  `.github/workflows/ci.yml`, `tools/release/check-model-gates.sh`.
- **Red evidence:** strict platform-linux rustdoc rejected the unresolved
  `WATCHDOG_TICK` link.
- **Green evidence:** strict package and portable-workspace rustdoc passed;
  the policy-checker self-test passed.

### A21 — portable file-mode inspection

- **Status:** Code complete; real BSD-host execution and the full macOS icon
  self-test remain pending.
- **Changed:** icon and notarization scripts share the same local strategy:
  try GNU `stat -c '%a'` first and fall back to BSD `stat -f '%Lp'`. Hermetic
  fake-stat cases cover both dialects and specifically reject GNU `stat -f`'s
  successful filesystem-dump output as a mode.
- **Files:** `tools/bundle/make-icon.sh`,
  `tools/release/notarize-app.sh`.
- **Red evidence:** the pre-fix BSD-first sequence either failed under GNU stat
  or accepted the wrong successful output.
- **Green evidence:** the notarization self-test and all shell syntax checks
  passed on NixOS. A traced icon self-test passed both hermetic GNU/BSD `stat`
  cases, atomic-generation cases, and cleanup checks, then stopped only when it
  reached the pre-existing real-Swift helper check because this Linux host has
  no `swift` executable.

### A29 — nested Full Local Gate prerequisites

- **Status:** Code complete; aggregate gate pending.
- **Changed:** development prerequisites explicitly list Ruby and Go and
  explain actionlint's `go run` invocation. The bundle metadata checker now
  fails early with the exact missing-Ruby diagnostic, backed by a PATH-hermetic
  regression that prevents the host interpreter from leaking into the case.
- **Files:** `docs/DEVELOPMENT.md`,
  `tools/bundle/check-bundle-metadata.sh`.
- **Red evidence:** without Ruby, the old path emitted `ruby: command not
  found` and then misreported invalid Ruby syntax.
- **Green evidence:** the bundle metadata self-test and live metadata check
  passed; the root re-ran the self-test independently after review.

### A15 — rust-toolchain pin provenance comments

- **Status:** Code complete; CI execution remains pending.
- **Changed:** all nine `dtolnay/rust-toolchain` SHA pins carry the accurate
  `# stable` provenance label: four in CI, four in release, and one in audit.
  The pinned commit is from the stable branch; calling it `v1` would falsely
  imply the repository's only release tag points to that SHA. The policy
  checker enforces the exact 4/4/1 distribution and mutates one comment.
- **Files:** `.github/workflows/ci.yml`, `.github/workflows/release.yml`,
  `.github/workflows/audit.yml`, `tools/release/check-model-gates.sh`.
- **Red evidence:** the checker first rejected the workflows with
  `CI rust-toolchain pins retain the stable version comment`.
- **Green evidence:** policy self-test, actionlint, YAML parsing, and diff
  checks passed.

### A73 — runner-image-keyed native caches

- **Status:** Code complete; CI execution remains pending.
- **Changed:** Windows and Linux jobs in branch and tag workflows read a
  safely-defaulted runner image identity through a bash output step and pass it
  as the rust-cache key. This isolates native llama.cpp artifacts across runner
  image revisions while leaving the already-fixed macOS cache key unchanged.
  The checker pins all four step shapes/orderings and has CI-Windows and
  release-Linux mutations.
- **Files:** `.github/workflows/ci.yml`, `.github/workflows/release.yml`,
  `tools/release/check-model-gates.sh`.
- **Red evidence:** after the checker expectation landed, it rejected CI with
  `windows exact action and input topology` until the workflow steps and keys
  were added.
- **Green evidence:** policy self-test, actionlint 1.7.12, shellcheck, and YAML
  parsing passed.

### A74 — acceptance harness error-accumulation contract

- **Status:** Code complete; documentation and existing self-tests agree.
- **Changed:** each of the three acceptance harness headers explains why it
  deliberately omits `set -e`: gate runners continue to collect failures via
  counters, while setup errors use `fail()` or explicit exit 2. No execution
  semantics changed.
- **Files:** `tools/acceptance/e2e-complete-me.sh`,
  `tools/acceptance/run-a1b-live-gates.sh`,
  `tools/acceptance/run-a2-compat-gates.sh`.
- **Green evidence:** all three harness self-tests, bash syntax checks, and
  shellcheck passed where host-portable. The A2 live/self-test path still has
  its pre-existing macOS-only `/usr/bin/osascript` requirement on Linux; no
  macOS execution is claimed.

### A19 — test-only API and architecture-surface cleanup

- **Status:** Code complete; portable compilation and clippy pass.
- **Changed:** kept the real library contracts (`engine::on_completion` and
  the already-test-only `engine_core::offer_replacement`), made
  `compat::app_policy` private behind its production wrappers, and made or
  gated the same-module-only context, catalog, and stats helpers as test-only.
  The architecture helper list now names the bounded production seam and
  labels the full-value helper as test-only.
- **Files:** `crates/compat/src/lib.rs`, `crates/context/src/lib.rs`,
  `crates/model_catalog/src/lib.rs`, `crates/stats/src/lib.rs`,
  `docs/ARCHITECTURE.md`.
- **Green evidence:** affected portable crates passed all-target clippy with
  warnings denied; `stats` passed 57 tests.

### A26 — scoped shell-facade lint allowances

- **Status:** Code complete; portable app clippy passes, macOS serial lane
  remains pending.
- **Changed:** removed both file-wide lint allowances. The only remaining
  `unused_imports` allowances sit directly on the two intentionally broad
  platform-facade re-exports; masked dead-code warnings were resolved rather
  than globally suppressed.
- **Files:** `crates/app/src/shell/mod.rs`,
  `crates/app/src/shell/macos.rs`, `crates/app/src/shell/stub.rs`.
- **Red evidence:** after removing the blanket allowance, strict app clippy
  identified the exact stub facade re-export that still needed a scoped
  allowance.
- **Green evidence:** `cargo clippy --locked -p app --all-targets -- -D
  warnings` passed in the Nix native-build environment.

### A52 — deliberate browser-domain AX read cost

- **Status:** Code complete; documented YAGNI posture.
- **Changed:** the monitored-edit path now states why it performs one fresh AX
  URL read per browser edit and defers a per-field invalidated cache until
  profiling shows a material cost.
- **Files:** `crates/app/src/run_loop.rs`.

### A57 — run-loop correctness and observability cleanup

- **Status:** Code complete; focused app tests and strict clippy pass.
- **Changed:** monotonic clocks are named accurately; caret events establish
  the current field even when context reading fails; tautological route asserts
  and their production-only scaffolding were removed/gated; and a dismiss is
  counted only while the engine actually has a visible suggestion.
- **Files:** `crates/app/src/loop_state.rs`, `crates/app/src/run_loop.rs`,
  `crates/engine/src/lib.rs`, `crates/engine_core/src/lib.rs`.
- **Green evidence:** the focused app regressions passed serially, strict app
  all-target clippy passed, and the existing engine-core ghost lifecycle test
  now pins the false/true/false visibility sequence. The aggregate engine run
  is deferred until the concurrent Linux replacement work stabilizes.

### A64 — AX value/selection pre-write validation

- **Status:** Code complete; macOS serial execution remains pending.
- **Changed:** `AxSet` insertion immediately re-reads both the full value and
  selected range before setting the computed value and fails with
  `CannotComplete` if either snapshot moved. The call-site comment explicitly
  preserves the residual final read-to-set race and readback limitation.
- **Files:** `crates/platform_macos/src/lib.rs`,
  `crates/platform_macos/src/lib_tests.rs`.
- **Red evidence:** the new snapshot regression initially failed to compile
  because `ensure_ax_insert_snapshot_unchanged` did not exist.
- **Green evidence:** the platform crate passed all-target check and clippy
  with warnings denied for `aarch64-apple-darwin`; the macOS-only unit test is
  compiled by that lane but cannot be linked or executed on this Linux host.

### A65 — fail-closed accept disarm window

- **Status:** Code complete; bounded posture is documented and unit-tested,
  with macOS execution pending.
- **Changed:** the controller documents that disarm clears the action before
  dropping the Carbon hotkey resource, including the generation-guarded
  zero-delay/delayed-hide teardown path. During queued unregister a key may be
  swallowed, but the handler cannot insert because its action is already
  `None`; no synchronous-close abstraction was added without live evidence.
- **Files:** `crates/platform_macos/src/lib.rs`,
  `crates/platform_macos/src/lib_tests.rs`.
- **Tests added:** drop probes cover both direct disarm and zero-delay
  `hide_suggestion_after`, observing that the shared action is already cleared
  when the hotkey resource destructor runs.

### A66 — accepted same-process caret-observer latency

- **Status:** Posture recorded.
- **Changed:** the subscription call site and ROADMAP record accept the safety
  poll's at-most-250-ms same-pid focus latency. They identify the missing
  callback-to-rebind ownership channel and require measured user-visible
  evidence before adding it.
- **Files:** `crates/platform_macos/src/lib.rs`, `docs/ROADMAP.md`.

### A67 — guarded clipboard restoration during teardown

- **Status:** Code complete; macOS serial execution remains pending.
- **Changed:** the adapter now retains the shared clipboard restore
  coordinator and, on teardown, attempts the pending restore only when the
  pasteboard change-count guard still matches. The insert path documents that
  the fixed one-second timer cannot prove a slow target finished pasting and
  that full paste-completion detection would require a separate event tap.
- **Files:** `crates/platform_macos/src/lib.rs`,
  `crates/platform_macos/src/lib_tests.rs`.
- **Test added:** a unique pasteboard verifies that adapter/coordinator teardown
  restores a still-pending snapshot without overwriting an external change.

### A71 — dead synthetic-backspace and observer-tap removal

- **Status:** Code complete; cross-compilation and strict clippy pass.
- **Changed:** removed the unreachable synthetic-backspace poster, adapter
  seam, helper, and tests. Removed the no-op observer tap kind, handler,
  install, subscription resource, and its tests. Non-atomic synthetic and
  clipboard replacements still fail closed before any write.
- **Files:** `crates/platform_macos/src/lib.rs`,
  `crates/platform_macos/src/lib_tests.rs`.
- **Green evidence:** `cargo check --locked -p platform_macos --all-targets
  --target aarch64-apple-darwin` and the matching clippy command with warnings
  denied both passed. Native execution remains a macOS-only obligation.

### A1 — executable Linux live-test count pin

- **Status:** Code complete; branch/tag CI execution remains pending.
- **Changed:** ROADMAP records the emitted current total, 34 live tests: 31
  AT-SPI/X11 plus one each for confirm, keyring, and reveal. A dedicated Linux
  checker obtains the ignored tests from Cargo rather than source attributes,
  validates that decomposition, and has a host-portable fixture self-test. Both
  branch and tag Linux lanes run it, and the release policy checker pins those
  step shapes and mutation coverage. The policy self-test rejects deleted,
  comment-only, and no-op live/harness commands in both workflows.
- **Files:** `docs/ROADMAP.md`,
  `tools/release/check-linux-live-test-count.sh`, `.github/workflows/ci.yml`,
  `.github/workflows/release.yml`, `tools/release/check-model-gates.sh`.
- **Red evidence:** the checker-first policy run rejected the old release lane
  because it did not verify the documented Linux live count. The follow-up
  mutation then proved that comment-only live commands were still accepted
  before the command checks became exact.
- **Green evidence:** normal Linux mode reported `34 (31 AT-SPI/X11 adapter
  tests + 1 each for confirm, keyring, reveal)`; all 34 passed in the live
  harness, and its self-test plus the model-policy self-test passed. The next
  remote CI run is still required before calling the workflow execution
  verified.

### A3 — accurate dependency pinning claim

- **Status:** Code complete; documentation verified.
- **Changed:** DEVELOPMENT now says native/ABI-sensitive dependencies are exact
  pinned, preserving the intentionally ranged pure-Rust dependencies.
- **Files:** `docs/DEVELOPMENT.md`.

### A6 — readable release gate inventory

- **Status:** Code complete; version anchors verified.
- **Changed:** the oversized CI table cell became a compact row plus structured
  step list without changing the eight version-checker anchor phrases.
- **Files:** `docs/RELEASING.md`.
- **Green evidence:** the version-doc checker and its self-test passed.

### A12 — current cross-platform implementation status

- **Status:** Code complete; independently re-reviewed.
- **Changed:** ROADMAP, architecture, acceptance/development guidance, agent
  brief, both relevant specs, and the dated Qfd revalidation now distinguish
  wired experimental Linux from the Windows scaffold. Completed Linux phases
  are no longer sequenced as pending, remaining work is spelled out in ROADMAP
  without delegating its SSOT role to `2FIX.md`, and the historical Qfd snapshot
  remains explicitly historical.
- **Files:** `AGENTS.md`, `Qfd.md`, `docs/ACCEPTANCE.md`,
  `docs/ARCHITECTURE.md`, `docs/DEVELOPMENT.md`, `docs/ROADMAP.md`, and the two
  cross-platform/grammar specs named by the finding.
- **Green evidence:** an independent read-only review found and then confirmed
  closure of the SSOT and historical-snapshot issues; agent-brief, version-doc,
  and focused policy gates passed.

### A22 — portable-crate lane count

- **Status:** Code complete; checker self-test passes.
- **Changed:** the CI comment now states 24 parallel crates: 26 workspace
  members minus `platform_macos` and `app`.
- **Files:** `.github/workflows/ci.yml`.

### A28 — Qfd resolution-table consistency

- **Status:** Code complete.
- **Changed:** the later reconciliation no longer calls F7/F11/F13 unchanged;
  it agrees with their resolved records and accurately separates the remaining
  F3 live-gate obligation.
- **Files:** `Qfd.md`.

### A33 — v0.1.6 milestone and Windows forcing function

- **Status:** Code complete; no tag or release was created.
- **Changed:** ROADMAP plans v0.1.6 as macOS-first audit remediation without
  promoting Linux. It distinguishes ready-to-tag requirements from full
  release completion through `post_verify`; records that the Linux C.2 trigger
  fired; and makes Windows UIA Phase 1 the next adapter milestone after the
  current Linux correctness cluster.
- **Files:** `docs/ROADMAP.md`.
- **Review correction:** the first draft circularly required completion of the
  entire release runbook before readiness. Independent review caught it and
  the final wording now separates pre-tag readiness from post-tag completion.

### A34 — workspace inventory and model-path wording

- **Status:** Code complete; documentation verified.
- **Changed:** README and DEVELOPMENT include `shell_flags` in the 26-member
  workspace inventory, and README calls the spike GGUF paths gitignored/fetched
  rather than checked in.
- **Files:** `README.md`, `docs/DEVELOPMENT.md`.

### A40 — personalization dependency boundary

- **Status:** Code complete; crate documentation matches the manifest.
- **Changed:** the crate header now says its pure implementation uses std plus
  `webconfig`, removing the false dependency-free claim.
- **Files:** `crates/personalization/src/lib.rs`.

### A8 — guarded NativeRangeSet local replacement accepts

- **Status:** Verified on Linux; cross-host lanes remain pending.
- **Changed:** local replacements offered on `NativeRangeSet` fields capture the
  exact Unicode-scalar range and original slice. Full and Word accept emit
  `ReplaceRange` with that guard; `AxSet` retains its established `Replace`
  command, non-atomic strategies remain excluded, and a delete count past the
  caret is rejected before anything is shown.
- **Files:** `crates/engine_core/src/lib.rs`, `crates/engine/src/lib.rs`.
- **Red evidence:** the Unicode regression first returned left-delete
  `Replace { replace_left: 4 }` instead of a guarded range for `😀teh`.
- **Green evidence:** focused command and expected-text mismatch regressions
  passed; complete `engine_core` and `engine` runs passed 177 and 94 tests.

### A9 — empty-field Linux popup anchor

- **Status:** Verified in the Linux live harness.
- **Changed:** when character-level caret extents are absent, Linux resolves
  AT-SPI Component screen extents as the popup anchor after validating current
  field identity. Degenerate component bounds remain `None`, and the fallback
  diagnostic is emitted only when a usable anchor exists.
- **Files:** `crates/platform_linux/src/atspi_live.rs`,
  `crates/platform_linux/src/lib.rs`,
  `crates/platform_linux/src/atspi_live_tests.rs`.
- **Green evidence:** pure geometry tests passed and the ignored GTK empty-entry
  regression obtained a real anchor; the current full 34-test live lane stayed
  green.

### A42 — keyboard-map-aware X11 grab plan

- **Status:** Verified in the Linux live harness.
- **Changed:** keyboard/modifier `MappingNotify` rebuilds the single X11
  `GrabPlan` and transactionally swaps exact grabs, restoring the old plan on
  failure. Pointer-only mapping notifications are ignored.
- **Files:** `crates/platform_linux/src/x11_tap.rs`,
  `crates/platform_linux/src/atspi_live_tests.rs`.
- **Live proof:** changing the keyboard map while armed rebuilt the plan and the
  rebound key still accepted in the Xvfb lane.

### A43 — configured chords precede the Linux installability probe

- **Status:** Code complete and ordering-tested; cross-host CI remains pending.
- **Changed:** startup applies persisted accept/shortcut bindings immediately
  after config load and before adapter construction. The subscription helper is
  explicitly preconfigured and no longer applies the map a second time.
- **Files:** `crates/app/src/run_loop.rs`,
  `crates/app/src/run_loop_tests.rs`, `crates/app/src/shell/stub.rs`.
- **Red evidence:** the new factory-order regression observed default chords at
  adapter construction instead of the persisted Return/Tab configuration.
- **Green evidence:** the regression passed; the current app lane passed 556
  tests with two intentional ignores, and the config-startup integration passed
  from the isolated RPATH-linked target.

### A58 — X11 tap accuracy cleanups

- **Status:** Verified with the WP7 Linux lanes.
- **Changed:** removed the unreachable default-chord translation arm; corrected
  watchdog/lock and keysym-dedup comments to match the one-plan mechanism; and
  records `armed_since_ms` only after a successful grab.
- **Files:** `crates/platform_linux/src/x11_keys.rs`,
  `crates/platform_linux/src/x11_tap.rs`.

### A60 — exact-modifier passive grabs

- **Status:** Verified in the Linux live harness.
- **Changed:** the X11 plan discovers the server's NumLock modifier, expands
  each configured chord across CapsLock/NumLock permutations, and deduplicates
  exact `(keycode, modifier)` grabs. Initial trial, install, rollback, ungrab,
  mapping refresh, and live rearm all consume that same plan; `AnyModifier` is
  no longer used.
- **Files:** `crates/platform_linux/src/x11_tap.rs`,
  `crates/platform_linux/src/atspi_live_tests.rs`.
- **Live proof:** a second client owning Alt+Tab no longer prevented compme
  acquiring and accepting bare Tab.

### A63 — transactional Linux live rearm

- **Status:** Verified in the Linux live harness.
- **Changed:** the Linux `AcceptSubscription` rearm closure reads configured
  bindings and invokes the same transactional plan swap used for mapping
  changes. The new plan is published only after its grabs succeed; failure
  restores the previous plan or disarms if restoration itself fails.
- **Files:** `crates/platform_linux/src/lib.rs`,
  `crates/platform_linux/src/x11_tap.rs`,
  `crates/platform_linux/src/atspi_live_tests.rs`.
- **Live proof:** while armed, the new chord accepted after rebind and the old
  chord no longer did. A second two-client case forced the new chord to fail
  with `BadAccess` and proved the previously armed Tab plan was restored.

### A68 — partial X11 worker-spawn teardown

- **Status:** Verified in the Linux live harness.
- **Changed:** a local spawn guard owns each worker until all three exist. A
  later spawn error marks inactive/stopping, thaws and ungrabs, drops senders,
  wakes the event thread, and bounded-joins workers that acknowledge exit before
  returning the error. A worker wedged past the two-second bound is detached
  only after thaw, ungrab, stop, sender-drop, and wake have all run.
- **Files:** `crates/platform_linux/src/x11_tap.rs`.
- **Green evidence:** deterministic failures injected at the dispatcher and
  watchdog spawns on a healthy X session proved every already-started worker was
  gone before install returned. Platform Linux passed 106 portable tests with
  34 ignored; all 34 ignored tests passed in the live harness, strict clippy and
  rustdoc passed.

### A14 — tag-validation parity controls

- **Status:** Code complete; hosted tag lane remains pending.
- **Changed:** release validation adds the CI-shaped all-tools Shellcheck,
  strict workspace rustdoc, model-policy self-test, and Full Local Gate runner
  self-test. Shared checker expectations pin both branch and tag shapes.
- **Files:** `.github/workflows/release.yml`,
  `tools/release/check-model-gates.sh`.
- **Green evidence:** policy self-test, actionlint, shellcheck, Bash syntax, and
  YAML parsing passed.

### A23 — retry-safe draft release preparation

- **Status:** Code complete; script branches are fixture-tested.
- **Changed:** a pre-create helper treats a genuinely absent release as clear,
  deletes only an existing draft without `--cleanup-tag`, rejects a published
  release, and fails loud on lookup errors.
- **Files:** `tools/release/prepare-draft-release.sh`,
  `.github/workflows/release.yml`, `tools/release/check-model-gates.sh`,
  `docs/DEVELOPMENT.md`.
- **Red evidence:** the first helper fixture failed because the function was not
  exposed to the fixture execution path.
- **Green evidence:** absent, draft, published, lookup-failure, and argument
  self-tests passed; the workflow policy mutation passed.

### A24 — non-macOS release publication runners

- **Status:** Code complete; hosted workflow execution remains pending.
- **Changed:** `publish_release` and `finalize_cask` run on Ubuntu while keeping
  release environments, permissions, guards, and artifacts intact;
  `post_verify` remains on macOS for its real cask install.
- **Files:** `.github/workflows/release.yml`,
  `tools/release/check-model-gates.sh`.

### A35 — macOS crate doc-test lane

- **Status:** CI shape complete; requires macOS execution.
- **Changed:** the macOS check job separately runs locked doc tests for
  `platform_macos` and `app`, with no unnecessary serial test flag.
- **Files:** `.github/workflows/ci.yml`,
  `tools/release/check-model-gates.sh`.

### A36 — absence-safe, failure-loud credential scrubbing

- **Status:** Code complete; helper and workflow ordering are tested.
- **Changed:** a dedicated helper treats a missing local GitHub extraheader as
  success, removes every present value, and propagates lookup/unset failures.
  Preflight and publish call it directly after their last fetch; prebuild calls
  it before any third-party build code and no longer masks failure with
  `|| true`.
- **Files:** `tools/release/scrub-git-credentials.sh`,
  `.github/workflows/release.yml`, `tools/release/check-model-gates.sh`,
  `docs/DEVELOPMENT.md`.
- **Review correction:** a first draft scrubbed preflight only after the HEAD
  comparison, so a mismatch could exit first. Final ordering is
  fetch → scrub → compare and is exact-pinned.
- **Green evidence:** no-key, multiple-value, and injected-unset-failure
  self-tests passed; an independent injected lookup failure is also rejected;
  WP12 policy mutations include missing/fail-open scrubs.

### A37 — preserve main CI runs

- **Status:** Code complete; workflow shape is mutation-tested.
- **Changed:** CI cancels superseded work only when the ref is not
  `refs/heads/main`.
- **Files:** `.github/workflows/ci.yml`,
  `tools/release/check-model-gates.sh`.
- **Red evidence:** the checker first rejected the old unconditional
  cancellation policy.

### A49 — signer-workflow-scoped attestations

- **Status:** Code complete; hosted release execution remains pending.
- **Changed:** both publication/finalization attestation checks require the
  pinned `mudrii/compme/.github/workflows/release.yml` signer workflow in
  addition to repository scope. Revalidation added checker invariants and
  reorder mutations requiring download < checksum < attestation < cask
  finalization, so attestation cannot silently move after artifact mutation.
- **Files:** `.github/workflows/release.yml`,
  `tools/release/check-model-gates.sh`.

### A53 — published artifact provenance verification

- **Status:** CI shape complete; real-tag evidence is intentionally pending.
- **Changed:** `post_verify` verifies the downloaded macOS zip attestation after
  checksum verification and before cask installation, with both repository and
  signer-workflow constraints. The checker now mutates this order and fails if
  attestation moves after artifact consumption.
- **Files:** `.github/workflows/release.yml`,
  `tools/release/check-model-gates.sh`.
- **Green evidence:** workflow parsing, actionlint, policy shape checks, and
  mutation tests pass. A real `gh attestation verify` result cannot exist until
  the next tag publishes an artifact and attestation; none is claimed.

### A54 — repository-parameterized cask finalization

- **Status:** Code complete; finalizer self-test passes.
- **Changed:** the finalizer accepts the repository as its fifth argument and
  threads it into release view/download. The workflow supplies
  `GITHUB_REPOSITORY`; the fake-gh test rejects a missing or wrong repo.
- **Files:** `tools/release/finalize-cask.sh`,
  `.github/workflows/release.yml`, `tools/release/check-model-gates.sh`.

### A55 — all-target portable tests

- **Status:** Code complete; hosted Windows/Linux execution remains pending.
- **Changed:** branch and tag Windows/Linux portable test commands include
  `--all-targets`, with exact checker pins and removal mutations.
- **Files:** `.github/workflows/ci.yml`, `.github/workflows/release.yml`,
  `tools/release/check-model-gates.sh`.

### A56 — branch model-quality gate

- **Status:** Code complete; live branch execution remains pending.
- **Changed:** branch CI runs the live quality gate immediately after the model
  smoke gate, reusing the already-downloaded cached GGUF; no duplicate download
  step was introduced, and the existing 90-minute job timeout remains pinned.
- **Files:** `.github/workflows/ci.yml`,
  `tools/release/check-model-gates.sh`.
- **Green evidence:** quality self-test and policy mutation passed. The live
  corpus command was not claimed from this workflow review; it remains a hosted
  model-lane obligation.
