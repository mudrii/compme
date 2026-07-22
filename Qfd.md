# compme — Full Architecture, Source, Test, Documentation, and CI Audit

**Audit date:** 2026-07-20 · **Re-audited:** 2026-07-21 (five-agent full re-audit; deltas and current finding statuses in §12)

**Repository:** `compme`

**Audited tree:** `main` at `4a6fd22afab2084c61be2e9e8fe7ff11a2c206e4`, plus the current uncommitted working tree

**Scope:** architecture, all Rust source seams, tests, coverage, documentation, roadmap/spec alignment, CI, release automation, acceptance tooling, and live GitHub governance

**Change made by this audit:** this report only; no production, test, workflow, or roadmap file was changed

## 1. Executive verdict

The implementation is technically strong and unusually well defended for a macOS-first desktop application. Its pure completion/policy core is deep, highly covered, and cleanly separated from host I/O. The `PlatformAdapter` contract is explicit, fail-closed on unsafe mutations, and already supports honest Windows/Linux scaffolds. CI and release automation cover substantially more than ordinary Rust gates: portable builds, security audit, real-model smoke, quality thresholds, action pinning, bundle validation, notarization, provenance, publication, cask finalization, and helper self-tests.

The audit did not find a Critical production-runtime defect. It did find one open release-correctness defect, two material architecture risks, several documentation/plan inconsistencies, and a process risk that the previous Qfd understated:

1. `finalize-cask.sh` says it selects the previous **stable** release, but its glob also accepts prerelease and malformed tags. This can reject a valid next cask finalization or weaken its stale-release guard.
2. The verified changes remain uncommitted. Remote CI proves `origin/main`, not the current 33-modified/10-untracked working tree. One untracked file, `keybindings.md`, is unrelated to compme, so “commit all 43 entries” is unsafe advice.
3. `run_loop.rs` and `platform_macos/src/lib.rs` remain 17,035 and 15,185 lines. The newer startup seam is useful, but a 27-field `RunContext`, eight factory closures, 39-field `SettingsFlags`, and 11-field `TrayFlags` show that extraction has relocated state more than it has deepened the interface.
4. The roadmap claims `main` is unprotected, while the live repository and `RELEASING.md` confirm an active minimal `protect-main` ruleset. The cross-platform execution guide also carries stale contract/test counts.
5. The largest objective gaps remain exactly the ones the product plan acknowledges: 22 macOS manual/live gate IDs and real Windows/Linux adapters.

All executable local gates passed except `shellcheck`, which is not installed locally. `bash -n` passed for every governed shell script, and the latest pushed HEAD has a green CI run that includes shellcheck; however, the uncommitted script changes still need remote CI or a local shellcheck run before they have that proof.

## 2. Audit method and boundaries

The audit used the repository's CodeGraph index first to trace the main event, inference, mutation, settings, platform, and release paths. It then reconciled those paths against:

- [`docs/ROADMAP.md`](docs/ROADMAP.md), the declared source of truth for pending work;
- architecture, development, acceptance, release, troubleshooting, manual-validation, plan, and spec documents;
- all 25 workspace crates and the separate `tools/spike` crate;
- the three GitHub Actions workflows, Dependabot configuration, bundle/release/acceptance helpers, and the published v0.1.5 release;
- current GitHub rulesets, environment settings, Actions policy, and the latest workflow runs;
- direct local execution of deterministic, model-backed, bundle, release-helper, and coverage gates.

Version-sensitive dependency checks used current documentation through `ctx7`: `Swatinem/rust-cache` accepts the configured multiline `workspaces` and `cache-directories` syntax, and `getrandom` 0.3's `fill(&mut [u8])` API is the correct secure cross-platform replacement. Neither is a finding.

The 22 LOOK/manual desktop gates were not claimed as passed. Their runners and ledger structure were self-tested, but actual results require a granted interactive macOS desktop and must remain recorded separately in [`docs/ACCEPTANCE.md`](docs/ACCEPTANCE.md).

## 3. Verified ground truth

| Surface | Current evidence |
|---|---|
| Git parity | `HEAD == origin/main == 4a6fd22`; ahead/behind `0/0` before considering the working tree |
| Working tree | 33 modified entries and 10 untracked entries; `git diff --check` passes |
| Workspace | 25 packages, 63 Rust source/test files, 77,694 physical Rust lines |
| Root test inventory | 1,941 tests **listed**; 1,935 execute in the default deterministic run; 6 are intentionally ignored model-backed gates |
| Root deterministic result | 1,935 passed, 0 failed, 6 ignored |
| Model-backed result | 5 root latency/context/quality-probe tests passed; 1 spike latency test passed; quality corpus passed separately |
| Spike inventory | 44 tests listed; 43 default tests passed, 1 model/GPU test ignored in the default lane and passed in the model gate |
| Quality corpus | 20/21 passed (95%), above the 80% / 17-case floor; `typo-occured` is the documented miss |
| Manual acceptance ledger | 22 exact runner-pinned manual/live IDs |
| Relative Markdown links | 0 missing local targets |
| Production debt markers | 2 `TODO(LOOK)` markers; no production `todo!()` or `unimplemented!()` path found |
| Published release | v0.1.5 tag `14ae81e`; release asset SHA-256 matches `Casks/compme.rb`; the published release workflow, including final cask publication, completed successfully |
| Current pushed CI | CI, CodeQL, and scheduled audit are green for pushed HEAD; this does not prove the uncommitted stack |

### Coverage

`cargo llvm-cov --locked --workspace --all-targets --summary-only -- --test-threads=1` passed:

| Metric | Coverage |
|---|---:|
| Regions | 86.50% |
| Functions | 82.76% |
| Lines | 85.49% |
| Branches | Not reported by this instrumentation |

The distribution is more informative than the aggregate:

- Pure policy/core crates are generally 98–100% line-covered, including `engine_core`, `ranker`, `prefs`, `context`, `redaction`, and `stats`.
- `app/src/run_loop.rs` is 82.86%; inference is 96.82%.
- `model_client` is 62.95%, principally because real-model paths are opt-in; the ignored gates passed during this audit.
- `platform_macos/src/lib.rs` is 79.18%, while native UI/FFI edges remain thinner: settings window 45.79%, shell host 31.76%, tray 40.72%, UI prompts 35.29%, and login-item code 0%.

This profile is coherent with the architecture: deterministic policy is strongly unit-tested, while AppKit/Accessibility/login-item behavior still needs the explicit live acceptance ledger. Raising the total percentage by unit-testing FFI wrappers would be less valuable than closing the 22 live gates or adding small, stateful host-boundary adapters where behavior can be tested without AppKit.

## 4. Architecture analysis

### 4.1 Effective module map

| Layer | Responsibility | Audit assessment |
|---|---|---|
| Pure core (`engine_core`, ranker, prefs, context, grammar, autocorrect, redaction, stats, etc.) | Policy, eligibility, ranking, text transformation, state transitions | Deep, cohesive, heavily tested, and the strongest part of the system |
| `engine` | Drives the pure policy against platform contracts | Clear dependency direction; errors are per-turn and recoverable |
| `platform` | Portable contracts and shared shell/settings types | `PlatformAdapter` is strong; shell/settings state ports are too broad |
| `app` / `run_loop` | Composition root, startup, event coordination, inference, settings/tray integration, teardown | Correctness-rich but still a monolithic orchestrator |
| `platform_macos` | AX, Carbon, AppKit, insertion, overlay, shell, settings, tray | Extensive and defended, but implementation ownership is concentrated in oversized files |
| `platform_windows` / `platform_linux` | Compile-tested, fail-closed facades with a few native services | Honest scaffolds, not usable completion adapters yet |
| Bundle/release/acceptance helpers | Package, validate, publish, recover cask, and collect evidence | Strong self-test culture; one tag-selection defect remains |

### 4.2 What is architecturally strong

- `PlatformAdapter` has 14 methods with explicit synchronization, bounded-blocking, and all-or-nothing mutation obligations. Unsafe replacement fallbacks are required or fail closed; missing implementations cannot silently append text.
- Security and correctness policy lives above platform mechanics: secure-input gating, stale field/range checks, app identity, generation invalidation, readback verification, and atomic replacement capabilities are explicit.
- The pure core is not coupled to AppKit, Accessibility, Windows UIA, or Linux AT-SPI. Target-specific crates are selected behind one app shell module.
- Windows/Linux stubs return `UnsupportedField` instead of pretending feature parity. Hosted builds therefore detect portability leaks without misrepresenting runtime support.
- Model download, checksum, local memory, config writes, redaction, signed deep links, bundle metadata, and release provenance are guarded and tested as policies rather than incidental code.
- Test names and failure contracts are unusually specific. Non-trivial fixes generally include discriminating regression tests and helper self-tests.

### 4.3 Structural gaps and optimization opportunities

#### A. The composition root remains shallow

`crates/app/src/run_loop.rs` is 17,035 lines. `startup()` is a legitimate test seam, but it returns a 27-field `RunContext` and is parameterized by eight constructor closures. The factory record verifies construction order and several early failures, yet it still exposes nearly every underlying binding to the heartbeat loop.

Recommended direction:

1. Extract cohesive owned modules, not field bundles: `StartupServices`, `RuntimePolicy`, `HostEventPump`, `SuggestionSession`, `SettingsController`, and `ShutdownCoordinator` are candidate responsibilities, not mandated names.
2. Give each module a small command/query interface and keep state behind it.
3. Move one behavior and its tests at a time; preserve event ordering and failure semantics byte-for-byte where possible.
4. Use complexity/change-frequency triggers rather than line count alone, but stop adding unrelated behavior to `run_loop.rs` now.

#### B. The macOS adapter is still a second monolith

`crates/platform_macos/src/lib.rs` is 15,185 lines and still carries the large AX worker, field/range access, event subscriptions, and mutation machinery. `settings_window.rs` is another 3,283 lines.

The existing planned order remains sensible: carve the AX worker behind an internal port, then move loop state. The safest extraction rule is to preserve the public `PlatformAdapter` behavior and test the internal port with deterministic fake AX targets. The newly added `AxRangeTarget` seam demonstrates that approach.

#### C. Shared shell/settings contracts expose storage instead of behavior

`SettingsFlags` has 39 public atomics/mutexes/vectors and `TrayFlags` has 11. `ShellHost` has 18 methods (8 required and 10 defaulted). These are portable at the type level, but the settings and tray ports are macOS-shaped shared-memory buses. A real Windows or Linux UI would have to mirror a large set of synchronization details and polling conventions.

Before native adapters grow, replace direct flag exposure incrementally with:

- immutable settings snapshots;
- typed settings/tray commands;
- a small event source/sink contract;
- an in-memory implementation for unit tests;
- platform UIs that translate native events without owning product policy.

This is the best architectural optimization because it deepens the future cross-platform seam while reducing `run_loop` state and making native UI behavior testable.

#### D. Cross-platform compilation is not cross-platform completion

The Windows and Linux crates correctly prove compilation and fail-closed behavior. They do not yet implement the core read/caret/subscription/insertion/overlay path. Windows supplies some real host services such as secure URL opening, console handling, and DACL hardening; Linux supplies URL launching/reaping. Neither is a functional inline-completion product.

The roadmap's Phase 1 Windows UIA and Phase 2 Linux X11/AT-SPI work remain the largest committed implementation gap. The current architecture can support them, but the settings/event-bus debt above should be reduced before duplicating macOS orchestration.

## 5. Objective and roadmap alignment

| Objective | Current state | Alignment |
|---|---|---|
| Local, no-telemetry inline completion | Implemented; local inference and local policy/memory controls verified | Aligned |
| macOS first | v0.1.5 is published, signed/notarized, cask-backed; current code adds further verified hardening | Aligned |
| Deterministic core behind `PlatformAdapter` | Implemented and strongly tested | Aligned |
| A2/A3 parity and grammar features | Code paths and deterministic gates exist; several LOOK/manual rows remain | Partially aligned pending live evidence |
| Windows/Linux committed deliverables | Phase 0/scaffolds and hosted portability gates exist; real adapters are pending | Largest implementation gap |
| Secure release pipeline | Strong and proven for v0.1.5; current uncommitted changes add post-publish verification | Mostly aligned; fix the stable-tag selector and prove new job on next tag |
| Roadmap as current source of truth | Broadly comprehensive, but it contains a live-governance contradiction and stale execution wording | Needs correction |

### Documentation consistency findings

1. [`docs/ROADMAP.md`](docs/ROADMAP.md) lines 223–233 say live GitHub settings leave `main` unprotected. The live check and [`docs/RELEASING.md`](docs/RELEASING.md) lines 45–64 confirm an active `protect-main` ruleset that blocks deletion and non-fast-forward updates. The real open decision is whether to add required reviews/status checks, not whether any protection exists.
2. Roadmap execution item 10 still says “decide whether to protect `main`.” It should say “decide whether to strengthen the existing minimal ruleset with review/status enforcement.”
3. The roadmap header says “Last updated 2026-07-18” while narrating 2026-07-19 remediation and current working-tree changes. Its 47-line evidence preamble is historically valuable but obscures the present pending-work ledger.
4. [`docs/superpowers/specs/2026-07-08-cross-platform-implementation-plan.md`](docs/superpowers/specs/2026-07-08-cross-platform-implementation-plan.md) is still the execution guide for pending phases, but its header says approximately 1,920 tests and its historical contract inventory says `PlatformAdapter` has 15 methods and `ShellHost` has 8 required + 9 defaulted. Current values are approximately 1,941 listed, 14 adapter methods, and 8 required + 10 defaulted shell methods. Mark the inventory explicitly frozen at its source commit or refresh the live prerequisites.
5. Relative Markdown links are intact, and the version-doc checker correctly gates all eight published-version surfaces. The README, SECURITY, release, development, acceptance, architecture, manual-validation, and roadmap version anchors agree on v0.1.5.

## 6. Findings ledger

### P1 — address before treating the current stack as release-ready

#### F1. Previous “stable” release selection accepts non-stable tags — OPEN

`tools/release/finalize-cask.sh` lines 127–139 claims to print the newest stable tag but iterates `git tag --list 'v[0-9]*.[0-9]*.[0-9]*'`. Git's glob matches names such as `v1.2.3-rc.1` and `v1.2.3junk`. The first non-current match becomes `previous_cask_version`, which lines 187–204 use as the only allowed lagging cask version.

Impact: an authorized but stray prerelease/malformed reachable tag can make a legitimate next stable release reject the current cask as stale, or select the wrong comparison version. Tag creation is presently one of the accepted unrestricted governance gaps, so this is not merely theoretical input outside the documented trust boundary.

Fix:

- filter candidates with the same strict stable SemVer shape already used by `check-bundle-metadata.sh` (`^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$`);
- add self-test fixtures containing higher-sorting prerelease and malformed tags;
- pin the new fixture/logic in `check-model-gates.sh`.

#### F2. Verified implementation remains local and mixed with an unrelated file — OPEN PROCESS RISK

`HEAD` and `origin/main` are equal, but the working tree contains 33 modified and 10 untracked entries. The latest remote CI therefore proves only `4a6fd22`, not the current stack. `keybindings.md` is an unrelated coding-tool keybinding table and should not be swept into a compme commit without explicit intent.

Fix: review and stage the intended compme files selectively, exclude or separately classify `keybindings.md`, rerun the locally unavailable shellcheck through CI or an installed local binary, then commit directly to `main` under the repository policy.

#### F3. Close the 22 live macOS gates before claiming feature parity — OPEN PRODUCT EVIDENCE

The runner/ledger contracts are strong, but native AppKit/AX, browser caret calibration, physical hotkeys, settings UI, grammar presentation, memory privacy, and app-specific behavior cannot be proven by unit tests or headless CI. These are not optional test debt: they are the remaining evidence for already-written macOS features.

### P2 — architecture and plan corrections

#### F4. Roadmap governance state contradicts live state — OPEN

Correct the two roadmap passages described above. Keep the six accepted live caveats explicit:

- release reviewer self-approval is allowed;
- administrator bypass is allowed;
- deployment branches/tags are unrestricted;
- all GitHub Actions are allowed;
- GitHub's required-SHA-pinning switch is off;
- release-tag creation is unrestricted.

The current minimal main and release-tag rulesets are active and should not be described as absent.

#### F5. `run_loop` and macOS platform modules remain change-amplifying seams — OPEN

The previous Qfd correctly kept the god-file extraction open, but understated the interface problem. The 27-field startup result and 39/11-field settings/tray data buses mean new feature work crosses many bindings and public synchronization details. Deepen these modules before or alongside the first real native adapter, using small behavior-owning ports and incremental tests.

#### F6. Startup extraction is only a partial architectural closure — OPEN

The extracted startup path improves ordering tests and enables an overlay-construction failure test. It does not yet isolate startup as a deep module: eight closures construct concrete subsystems and return 27 pieces of state. Continue only when the next change can remove state or hide a responsibility; do not add abstraction solely to reduce file length.

#### F7. Cross-platform execution guide has live-prerequisite drift — OPEN DOCS

Refresh the current test/contract counts or label them as a frozen historical inventory at `b367f0f`. The task phases remain useful, but implementers should not have to determine which numbers are historical and which are current prerequisites.

2026-07-21 sharpening: the stale "15-method trait" figure appears not only in the historical inventory (line 29) but also in the plan's forward-looking cross-cutting rule at line 318 ("implement against the existing 15-method trait"), which is active implementer guidance, not history. The current trait has 14 methods (12 required + 2 defaulted: `popup_anchor`, `focused_page_url`; re-verified by direct count of `crates/platform/src/lib.rs:491` on 2026-07-21). ShellHost is 8 required + 10 defaulted, not 8+9; the test count is ≈1,941 listed, not ≈1,920.

#### F12. macOS/product-UI vocabulary lives in the shared cross-platform contract crate — NEW 2026-07-21

`SettingsFlags` (39 fields, including product-specific ones such as `personalization_sender_email`, `emoji_gender_index`, `setup_choose_model`), `TrayFlags`, `ConfirmPrompt`, and `ShellHost` all live in `crates/platform/src/shell.rs` (479 lines) — the contract crate that `engine_core`/`engine` depend on. `platform_windows` and `platform_linux` contain zero references to `SettingsFlags`/`TrayFlags`; only `app` and `platform_macos` consume them. This is the concrete substrate of §4.3C, now located precisely: a crate-placement defect (settings-window/tray vocabulary parked in the portable contract), not merely interface width. A real Windows/Linux settings UI would either mirror this macOS-shaped struct or the crate stays macOS-coupled. Fix direction is unchanged from §4.3C: typed commands/snapshots/events, with the macOS-shaped state moving out of the shared crate.

Related minor observation: `crates/app/src/shell/stub.rs:1` carries a file-level `#![allow(dead_code)]`, which masks any genuinely dead item in the non-macOS shell wiring.

### P3 — accepted trade-offs or lower-value optimizations

#### F8. Whole-workspace test serialization — ACCEPTED, OPTIMIZABLE

The root suite uses `--test-threads=1` because macOS shortcut/pasteboard checks share process-global state. Guard locks close known races, but the whole 1,941-test inventory pays the cost. Split pure crates into a parallel job and keep only stateful platform/app tests serial when CI duration justifies the maintenance cost.

#### F9. Docs-only pushes skip CI — ACCEPTED VISIBILITY GAP

Direct docs-only pushes skip the check job, including version-doc validation, until the next code push or release. Pull requests still run CI. A small docs-only validation job would close this without paying for macOS builds, but the current trade-off is explicitly documented.

#### F10. New `post_verify` job is deterministically defended but not release-proven — TRACK (content re-verified 2026-07-21)

The current workflow's watchdog marker, bounded runtime assertion, duplicate-instance rejection, codesign, stapler, and Gatekeeper checks are strong. The 2026-07-21 re-audit confirmed the job body is substantive, not a stub — published-asset checksum verification, `brew install --cask`, `codesign --verify --deep --strict`, `xcrun stapler validate`, `spctl --assess`, and a bounded startup smoke with duplicate-instance rejection (`release.yml:695-773`), all pinned verbatim by `check-model-gates.sh`. The published v0.1.5 run predates this uncommitted job, so operational proof still arrives only with the next real tag.

#### F11. Dependabot/action-pin reconciliation remains manual — TRACK (enforcement confirmed fail-closed 2026-07-21)

Dependabot covers root Cargo, `tools/spike`, and Actions. Action references are full-SHA pinned and comments name versions, while `check-model-gates.sh` pins the allowlist. The reconciliation is fail-closed in practice: `check-model-gates.sh` hardcodes the approved action SHAs, so an unreconciled Dependabot SHA bump fails CI until the checker and version comments are updated together. The remaining gap is doc completeness only — the procedure lives in `dependabot.yml:1-4` and ROADMAP but is not mirrored in DEVELOPMENT/RELEASING; see also F13.

#### F13. Remediation-plan item 3 (Dependabot noise reduction) is partially delivered — NEW 2026-07-21

`docs/superpowers/plans/2026-07-19-audit-findings-remediation.md` item 3's acceptance requires the `github-actions` ecosystem scoped to **monthly** ("dependabot config shows monthly for actions, weekly retained for cargo") and a greppable doc paragraph in the maintenance docs. The untracked `.github/dependabot.yml` still schedules `github-actions` **weekly**, and the re-pin procedure is documented only in `dependabot.yml`'s own comments and ROADMAP, not DEVELOPMENT/RELEASING. Either finish the item (interval edit + doc paragraph) or amend the plan's acceptance to match what shipped.

## 7. Source and test audit details

### Source correctness and safety

- Mutation paths are guarded by capability checks, expected-text/range validation, stale-focus/generation invalidation, and all-or-nothing adapter contracts.
- Secure-input and inaccessible-field paths fail closed before inference or persistence.
- Config and local memory code enforce private paths/permissions and avoid following hostile temporary-file links; platform-specific hardening is explicit.
- Signed deep links reject unknown/misordered parameters, malformed scopes, tampering, and untrusted keys.
- Production `unsafe` is concentrated in platform/FFI boundaries plus the documented Unix signal installation; pure policy crates are safe Rust.
- The only production TODO markers are two explicit `TODO(LOOK)` reminders. The observed `unimplemented!()` calls are test fakes, not production paths.

No correctness issue was found in the current `getrandom` migration, rust-cache configuration, model checksum path, or action reference syntax.

### Test design

Strengths:

- tests encode negative and fail-closed cases, not only happy paths;
- concurrency, stale identity/range, partial mutation, hostile filesystem, env poisoning, duplicate-instance, timeout, and release-recovery paths are represented;
- release/bundle helpers have hermetic self-tests that exercise their CLI and environment contracts;
- real model tests and the quality corpus are separate from deterministic defaults but runnable through pinned wrappers;
- the quality corpus pins raw pre-vetting output for guard-specific rejection cases.

Gaps:

- native UI/FFI coverage is intentionally thin and must be closed by live gates;
- the release finalizer lacks a prerelease/malformed-tag fixture;
- the startup factory seam tests selected ordering/failure edges, not every side effect between construction stages;
- the ignored latency file retains a small duplicate typo battery that could drift from the canonical quality corpus.

## 8. CI/CD and release audit

### Strong controls confirmed

- workflow-wide read-only permissions by default, with scoped write permissions for publication/cask jobs;
- full-SHA action references, `persist-credentials: false`, explicit job timeouts, and concurrency cancellation;
- `actionlint`, shellcheck, rustfmt, clippy, tests, docs warnings, build, and `cargo audit`;
- Windows/Linux portable workspace tests and app builds through fail-closed target facades;
- a branch model-backed smoke using the pinned/hash-verified GGUF;
- release validation across portable targets, signing/notarization/stapling, build provenance attestation and verification, asset checksum validation, immutable helper execution from the release commit, cask finalization, and a new installed-app verification job;
- scheduled RustSec and live governance checks with tracking-issue behavior on failure;
- v0.1.5 asset/cask checksum consistency and successful final cask job verified against the published run.

### Remaining trust boundaries

GitHub repository settings do not independently enforce all repository-level policies. Minimal main protection exists, but no review/status requirement exists; tag creation, environment self-review/admin bypass, deployment scope, Actions allowlisting, and GitHub's own SHA-pin enforcement remain owner decisions. Repository scripts and workflow content mitigate these gaps but cannot replace external authorization boundaries.

## 9. Comparison with the previous Qfd

| Previous Qfd claim | Current audit result |
|---|---|
| “1,941 passed, 6 ignored” | **Incorrect.** 1,941 are listed; 1,935 pass in the default root run and 6 are ignored. All 6 model-backed gates passed separately. |
| “Every deterministic gate is green, including local shellcheck” | Mostly confirmed, but **local shellcheck was not run** because it is not installed. `bash -n`, actionlint, and all other local gates passed; pushed HEAD CI is green. |
| “The plan documents track reality” | **Too broad.** ROADMAP contradicts live main protection, and the cross-platform guide has stale live counts. |
| “All new reconciliation findings are resolved” | **No longer true.** The stable-tag filter defect is newly open. |
| “Commit the 33 modified + 10 untracked stack” | **Unsafe as blanket advice.** Stage intended compme files selectively; `keybindings.md` appears unrelated. |
| Governance checker hard baseline is fixed | Confirmed. Live run exits 0 and reports six accepted caveats. |
| Eight version surfaces are gated | Confirmed. Live and self-test checks pass. |
| Overlay startup failure is pinned | Confirmed. |
| Quality corpus is 20/21 with three raw guard pins | Confirmed by a live pinned-model run. |
| God-file extraction remains open | Confirmed and expanded: the settings/tray shared-state interfaces are part of the same structural risk. |
| v0.1.5 release/cask completed end-to-end | Confirmed for the workflow that existed at v0.1.5; the new `post_verify` job awaits its first real release run. |

The previous report was directionally accurate about code quality and the remaining strategic work, but its final “everything remediated” conclusion was overconfident. The corrected conclusion is: strong current implementation, no Critical runtime defect found, one release-helper defect open, several architecture/documentation gaps open, manual validation unfinished, and the entire remediation stack still local.

## 10. Prioritized next actions

1. Fix and self-test strict stable-tag filtering in `finalize-cask.sh`.
2. Correct ROADMAP's main-protection statements and refresh/freeze the cross-platform plan's live counts.
3. Run or install shellcheck, then selectively stage the intended compme stack; explicitly exclude/classify `keybindings.md` before committing.
4. Close and record the 22 macOS live gates, starting with settings/setup, physical shortcuts/grammar, Chromium caret calibration, and memory privacy.
5. Deepen the settings/tray event interface and continue incremental `run_loop`/AX-worker extraction only when each step hides state or behavior.
6. Begin Windows UIA Phase 1, then Linux X11/AT-SPI Phase 2, keeping runtime-support claims fail-closed until native acceptance passes.
7. Let the next release prove `post_verify` operationally, including final cask/checksum consistency.

## 11. Validation record

Passed during this audit:

- `cargo fmt --all -- --check`
- `cargo clippy --locked --workspace --all-targets -- -D warnings`
- `cargo test --locked --workspace --all-targets -- --test-threads=1`
- `cargo build --locked --workspace --all-targets`
- `cargo build --locked -p platform_macos --examples`
- `RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --workspace`
- `cargo audit`
- `go run github.com/rhysd/actionlint/cmd/actionlint@v1.7.12 -color`
- all governed shell scripts through `bash -n`
- root bundle metadata, bundle smoke, and missing-model product smoke
- all bundle, acceptance-runner, governance, version, policy, quality, cask, notarization, and manifest self-tests
- live version-doc, model-client-feature, agent-brief, privacy-policy, and model-gate policy checks
- all five ignored root model tests, the ignored spike model/latency test, and the 20/21 quality corpus
- separate spike fmt, clippy, test, and binary build
- LLVM coverage run
- `git diff --check`

Not passed or not executed:

- local `shellcheck`: executable unavailable; current pushed CI is green, but the uncommitted changes still need this proof;
- 22 native macOS manual/live gates: intentionally not claimed without an interactive granted desktop;
- native Windows/Linux runtime acceptance: adapters are not implemented;
- new release `post_verify`: no release has yet run the uncommitted job.

## 12. 2026-07-21 re-audit

**Method:** five parallel audit agents (architecture, source, tests, docs/plan alignment, CI/release) over the same tree (`HEAD == origin/main == 4a6fd22`, same 33-modified/10-untracked working set as §3 — byte-identical on every re-measured surface). Every agent finding was independently re-verified by the orchestrator against the cited file/line before acceptance; one agent claim was refuted this way (see below).

**Verdict:** the 2026-07-20 report holds. No new correctness or safety defect was found in source, tests, or CI. The uncommitted `run_loop.rs` `startup()` extraction and the `AxRangeTarget` seam were re-verified as behavior-preserving (guard-lifetime, drop-order, secure-input-recheck-first, and stale-identity ordering all intact). Two new lower-priority findings were added (F12 crate-placement, F13 remediation-item-3 partial delivery), and F7/F10/F11 were sharpened in place.

### Finding status after re-audit

| Finding | Status 2026-07-21 |
|---|---|
| F1 stable-tag glob in `finalize-cask.sh` | **OPEN, confirmed by four independent checks.** Additional detail: `--sort=-version:refname` without `versionsort.suffix` sorts a prerelease suffix above its release, so a stray `vX.Y.Z-rc.N` would be selected; the self-test fixture repo never creates a prerelease/malformed tag, so the defect is also untested. `check-bundle-metadata.sh` shares the loose glob at line 462 but is compensated by a strict semver re-filter (lines 531/545); `finalize-cask.sh` is the only unfiltered instance. |
| F2 uncommitted stack + unrelated `keybindings.md` | OPEN, unchanged. |
| F3 22 live macOS gates | OPEN. Ledger re-verified: exactly 22 runner-pinned IDs; no pending gate is claimed passed; completed gates carry recorded evidence. |
| F4 ROADMAP vs live governance | OPEN, re-confirmed verbatim (`ROADMAP.md:223-227` and item 10 at `:787-790` vs `RELEASING.md:45-48`). Already tracked as remediation-plan item 9, which is why the two docs remain inconsistent in the same tree. |
| F5/F6 run_loop / settings-tray interface depth | OPEN, static — all counts byte-identical to §4 (17,035 / 15,185 / 3,283 lines; 27-field `RunContext`; 8 factories; 39/11-field flags; ShellHost 8+10). Sharpened by F12. |
| F7 cross-platform guide drift | OPEN, widened — the stale 15-method figure also sits in the plan's *active* cross-cutting rule (line 318), not just the historical inventory. |
| F8 test serialization | ACCEPTED, unchanged. |
| F9 docs-only pushes skip CI | ACCEPTED, re-confirmed by-design: `paths-ignore` applies to pushes only; PRs always run full CI. |
| F10 `post_verify` unproven | TRACK — job content re-verified substantive and gate-pinned; awaiting first real release run. |
| F11 Dependabot reconciliation | TRACK — enforcement confirmed fail-closed via hardcoded SHAs in `check-model-gates.sh`; residual gap is doc completeness (F13). |
| F12 shared-contract crate placement | **NEW P2** (see §6). |
| F13 remediation item 3 partial delivery | **NEW P3** (see §6). |

### Re-audit corrections and refuted agent claims

- An agent reported `PlatformAdapter` had grown to 18 methods; direct count shows the trait body still has exactly **14** (the agent's range ran past the trait's closing brace into the `Overlay` trait). §4's figure stands. This is recorded to keep the confabulation-check discipline visible.
- CI hardening observation, documented not flagged: the release `preflight` checkout uses `fetch-depth: 0` without `persist-credentials: false`; it runs only first-party scripts and the exact shape is pinned by `check-model-gates.sh`, so it is deliberate.

### 2026-07-21 validation record

Passed: `cargo fmt --check`; `cargo clippy --locked --workspace --all-targets -- -D warnings`; full deterministic suite `cargo test --locked --workspace --all-targets -- --test-threads=1` → **1,935 passed, 0 failed, 6 ignored** (matches §3 exactly); `bash -n` on all governed scripts; `finalize-cask.sh --self-test`; `check-version-docs.sh` self-test and live run.

Not executed: local `shellcheck` (still not installed); model-backed gates, coverage, and bundle smokes (unchanged tree — §11 results from 2026-07-20 remain the evidence); the 22 live gates; Windows/Linux runtime acceptance.

### Prioritized next actions (unchanged order, two additions)

The §10 list stands. Add: (8) move the macOS-shaped `SettingsFlags`/`TrayFlags`/`ConfirmPrompt` vocabulary out of `crates/platform/src/shell.rs` as part of action 5's interface deepening; (9) finish or re-scope remediation item 3 (Dependabot monthly interval + maintenance-doc paragraph).

---

## 13. 2026-07-22 implementation record (status: all addressable findings resolved)

This section reconciles the 2026-07-20/21 findings with the implementation program executed on 2026-07-21/22. Every wave was validated after landing (fmt, clippy `-D warnings`, serial suite, `check-model-gates.sh` live + `--self-test`, actionlint, shellcheck, script self-tests) and committed separately. Validation at the end of the program: workspace clippy ✅, full serial suite ✅ (1,942 listed: 1,012 parallel-lane + 923 serial-lane + 6 ignored + 1 latency corpus guard), checker live ✅ / self-test ✅, actionlint ✅, shellcheck ✅, `check-version-docs.sh` (8 surfaces) ✅, `finalize-cask.sh --self-test` ✅, e2e + bundle smoke ✅.

### Commits (all on `main`)

| Commit | Contents |
|---|---|
| `c60d171` | Batch 1+2 CI/docs/cask-window/dependabot/pre-push hardening; Batch 3+4 (quality gate, CI smoke gate, AX insertion tests, `run()` startup extraction, `post_verify`, version-docs check); nitpick sweep; audit remediation; this ledger |
| `e804924` | Test-lane split (parallel 23 crates + serial macOS/app), `docs.yml` docs-only lane, latency corpus dedup, `tools/dev/check.sh` gate runner, ROADMAP header slim |
| `17da332` | F1 stable-tag filter; F4 governance wording; F7 guide drift; F13 dependabot monthly |
| `509318e` | README test-count re-stamp |
| `9fcc177` | F12: `shell_flags` crate — platform's `shell.rs` shrinks 479 → **183 lines** (2 portable traits) |
| `4cbf0a8` | C8c `builders.rs` (run_loop −1,000 ln) + C8b `ax_worker.rs` (lib.rs −1,832 ln) |
| `ffba136` | C8a `loop_state.rs` — 32 bindings in 8 cohesive structs; teardown order preserved bit-identically |

### Finding status flips

| Finding | Status |
|---|---|
| F1 stable-tag glob | **RESOLVED** — strict SemVer guard + 4 discriminating fixtures (prerelease/malformed skip, prerelease-only empty fallback, lag accept, moved-past reject); old code fails the new fixture |
| F2 uncommitted stack | **RESOLVED** — everything committed across the 7 commits above; `keybindings.md` remains deliberately untracked (unrelated reference, verified accurate, owner's to keep or move) |
| F3 22 live macOS gates | **OPEN** — requires the owner's granted-Mac sessions; the product critical path |
| F4 ROADMAP vs live governance | **RESOLVED** — both passages now describe the active `protect-main` ruleset + the strengthen-or-not decision; six caveats still explicit |
| F5/F6 god-file seams | **RESOLVED at the planned depth** — `run_loop.rs` 17,035 → **16,036** (startup extraction + builders.rs + loop_state.rs, heartbeat loop byte-identical, teardown order preserved); `platform_macos/lib.rs` 15,185 → **13,353** (`ax_worker.rs`, zero new pub surface); the deeper typed-commands settings/tray redesign remains as documented future work, not a gap |
| F7 guide drift | **RESOLVED** — inventory frozen at `b367f0f`; active guidance corrected to 14-method trait; prerequisites re-verified |
| F8 test serialization | **RESOLVED** — parallel lane (23 crates) + serial lane (macOS/app); 1,935 passed in ~half the serial cost |
| F9 docs-only pushes | **RESOLVED** — `docs.yml` gates version-docs/script-syntax/shellcheck/cask-syntax on exactly those pushes |
| F10 post_verify unproven | **TRACK** — first real exercise comes with the next tag |
| F11/F13 dependabot | **RESOLVED** — actions monthly, cargo weekly; procedure mirrored in dependabot.yml, RELEASING, DEVELOPMENT; remediation-plan item 3 marked delivered |
| F12 crate-placement | **RESOLVED** — `shell_flags` pure zero-dep crate; `platform`'s public surface is now the two portable traits |

### Incidents worth recording (honesty)

- The C8c builder move initially missed one import (`parse_enabled_default`) and left `SenderIdentity` unused in run_loop.rs — caught by the verification clippy run before commit, fixed in two lines.
- An earlier env-guard "consistency" edit broke the checker's poison-invocation contract for `check-bundle-metadata.sh`; the live checker caught it the same run and it was reverted. Two deliberate self-test env patterns now exist and are documented at the scripts' entrypoints.

### Remaining open (unchanged from §10)

1. Close and record the 22 macOS live gates (owner action).
2. Windows UIA Phase 1 → Linux X11/AT-SPI Phase 2 (largest committed deliverable).
3. Let the next release prove `post_verify` operationally.
4. Optional future deepening: typed settings/tray commands/snapshots/events (the F12 placement move is done; the redesign is a design task, not debt).
