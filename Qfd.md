# compme — Full Architecture, Source, Test, Documentation, and CI Audit

**Audit date:** 2026-07-20 · **Re-audited:** 2026-07-21 (five-agent full re-audit; deltas and current finding statuses in §12) · **Re-audited:** 2026-07-25 (post-implementation audit of the committed tree `67a74b2`; verification of every §13 flip, five new findings F14–F18, and corrections in §14) · **Remediated:** 2026-07-26 (F14–F18 closed in four commits; §15) · **Deep dive:** 2026-07-26/27 (§16 — refactor proven token-exact, coverage figures corrected, one live gate defect F20 fixed; read §14 → §15 → §16)

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

---

## 14. 2026-07-25 re-audit (post-implementation, committed tree)

**Method:** single-orchestrator full audit (architecture, source, tests, docs/plan alignment, CI/release, live GitHub state) over `main` at `67a74b2`, the first audit of the *committed* program rather than a working tree. Every §13 status flip was re-verified against the cited file/line or a live run; no claim was accepted from the record. Metrics were re-measured, not copied.

**Audited tree:** `HEAD == origin/main == 67a74b25824c2fd405c772526762ef6843b9fed2`, ahead/behind `0/0`, working tree clean except the deliberately untracked `keybindings.md`.

**Verdict:** every §13 "RESOLVED" flip is real. The implementation program did what it claimed, and the deterministic surface is green end-to-end on the committed tree (this is the proof §13's F2 was still waiting for — CI ran green on `67a74b2` at 2026-07-22T09:39Z, CodeQL re-ran green 2026-07-25). No correctness, safety, or security defect was found in source, tests, or CI. Five new findings are process/documentation/architecture-framing issues (F14–F18), one of which — a dependabot PR that cannot merge by construction — is blocking three open PRs today. One §12 figure is corrected.

### Re-measured ground truth

| Surface | 2026-07-25 evidence | vs §3 / §13 |
|---|---|---|
| Git parity | `HEAD == origin/main == 67a74b2`, `0/0` | F2 closed for real |
| Working tree | clean; only `keybindings.md` untracked | as recorded |
| Workspace | **26** packages, 79 tracked `.rs` files, 80,435 tracked Rust lines | was 25 / 63 / 77,694 (pre-`shell_flags`) |
| Deterministic suite | `cargo test --locked --workspace --all-targets -- --test-threads=1` → **1,936 passed, 0 failed, 6 ignored** (1,942 listed) | matches the ≈1,942 doc pins |
| fmt / clippy | `cargo fmt --check` ✅; `clippy --locked --workspace --all-targets -D warnings` ✅ | green |
| Doctests | none exist (`--doc` runs 0 tests) → the `--all-targets` lanes lose no coverage | new check, no gap |
| `run_loop.rs` / `platform_macos/lib.rs` / `settings_window.rs` | 16,036 / 13,353 / 3,283 lines | matches F5 flip |
| `platform/src/shell.rs` | 183 lines, 2 portable traits; `shell_flags` 318 lines, 4 tests | matches F12 flip |
| `SettingsFlags` / `TrayFlags` | still 39 / 11 fields — *relocated*, not narrowed (as F12 said) | unchanged by design |
| `PlatformAdapter` / `ShellHost` | 14 methods (**10 required + 4 defaulted**); ShellHost 8 required + 10 defaulted | see F7 correction below |
| Manual ledger | exactly 22 runner-pinned IDs in `ACCEPTANCE.md:686-709` | unchanged |
| `unsafe` | 1 site in `app` (signal install, `run_loop.rs:119`), 103 in `platform_macos/lib.rs`, 26 in `ax_worker.rs`, 8 in `platform_windows`, 0 in `platform_linux`, 0 in pure crates | confinement intact |
| Debt markers | 2 `TODO(LOOK)`; no production `todo!()`/`unimplemented!()` (the 4 hits are `engine` test fakes) | unchanged |
| Assertion-free tests | 0 (a 3-hit scan was a raw-string brace-counting artifact; all three assert) | test suite clean |
| Relative Markdown links | 0 missing local targets across every tracked `.md` | unchanged |
| Live governance | `check-github-governance.sh --repo mudrii/compme` exits 0, reports the same six accepted caveats | unchanged |
| Helper gates | `check-version-docs.sh` (8 surfaces, v0.1.5) ✅, `check-model-gates.sh` live ✅, `finalize-cask.sh --self-test` ✅ | unchanged |

### Verification of each §13 flip

| Finding | §13 status | 2026-07-25 verification |
|---|---|---|
| F1 stable-tag glob | RESOLVED | **Confirmed.** `finalize-cask.sh:136-143` re-filters every candidate through the strict stable-SemVer anchor (the same no-leading-zero `vX.Y.Z` regex `check-bundle-metadata.sh` uses) inside the sorted loop, with the rationale comment naming the `-version:refname` prerelease-sort trap; `--self-test` passes. |
| F2 uncommitted stack | RESOLVED | **Confirmed and now CI-proven** — the whole program is on `origin/main` with a green push run; `keybindings.md` correctly stayed out. |
| F3 22 live gates | OPEN | **Still open.** Ledger re-counted at 22; no gate claims unearned evidence. Remains the product critical path and the only owner-blocked item. |
| F4 governance wording | RESOLVED | **Confirmed.** `ROADMAP.md:231-245` and execution item 10 (`:799-807`) both describe the active `protect-main` ruleset and the strengthen-or-not decision; the six caveats are still enumerated and match the live checker's output verbatim. |
| F5/F6 god-file seams | RESOLVED at planned depth | **Confirmed** on line counts; **re-framed** by F16 below — the metric everyone has been quoting is dominated by inline test modules, and the real concentration is `run()` itself. |
| F7 guide drift | RESOLVED | **Confirmed.** The plan's inventory is explicitly frozen at `b367f0f` (`:28`) and the active cross-cutting rule at `:319` now says "14-method". Only §12's own parenthetical was wrong — corrected below. |
| F8 test serialization | RESOLVED | **Confirmed.** `ci.yml` runs 24 crates in the parallel lane (`--exclude platform_macos --exclude app`) and only those two serially, with the shared-global rationale in-comment. |
| F9 docs-only pushes | RESOLVED | **Confirmed.** `docs.yml` gates version-docs, `bash -n`, shellcheck, and cask syntax on exactly the `paths-ignore` set that `ci.yml` skips. Observed working live on a real docs push. See F18 for one cheap tightening. |
| F10 `post_verify` | TRACK | **Still TRACK.** Job present at `release.yml:698-773`, `needs: finalize_cask`, tag-gated; no release has run it (latest published artifact is still v0.1.5). |
| F11/F13 dependabot | RESOLVED | **Confirmed** as a config/doc change: actions monthly, cargo weekly ×2, procedure mirrored in `dependabot.yml:1-4`, `RELEASING.md:37-45`, `DEVELOPMENT.md:297-304`. But the *operational* cost is now visible — see F15. |
| F12 crate placement | RESOLVED | **Confirmed.** `shell_flags` is pure/zero-dep; `platform` depends on it only for `ConfirmPrompt`, dependency direction non-cyclic and documented at the crate head. The related nit is closed too: `app/src/shell/stub.rs`'s file-level `#![allow(dead_code)]` is gone, replaced by 16 per-item allows plus a module-doc rationale (`67a74b2`). |

### New findings

#### F14. `ARCHITECTURE.md` contradicts itself on workspace size — NEW P2 DOCS

`docs/ARCHITECTURE.md:28` says "The workspace now holds 25 crates"; `:84` says "The 26 crates fall into six groups" and then lists 26 names (6+3+6+7+3+1) including `shell_flags`. The actual count is **26** (`cargo metadata`). The `shell_flags` split updated the second mention and missed the first.

Root cause worth fixing beyond the typo: **no checker pins the crate count**, unlike the eight version surfaces that `check-version-docs.sh` gates and the test count that the doc pins carry. Fix: one-word edit at `:28`, plus a `cargo metadata`-derived crate-count assertion in `check-version-docs.sh` (or `check-agent-briefs.sh`) so the next crate split cannot silently desync a doc again.

#### F15. The grouped cargo dependabot PR cannot merge by construction, and it blocks 9 unrelated bumps — NEW P1 PROCESS

Three dependabot PRs have been open and unattended since 2026-07-22:

- **#3 "bump the cargo group with 10 updates" — fails all three build lanes in under a minute.** Root cause: `model_client` declares `llama-cpp-2` **twice**, once per target (`cfg(target_os = "macos")` with `metal`, `cfg(not(target_os = "macos"))` without), both exact-pinned `=0.1.146`. Dependabot rewrote only the macOS entry to `=0.1.152` and left the other at `=0.1.146`, producing two unsatisfiable exact requirements: `error: failed to select a version for llama-cpp-2` on macOS, Windows, and Linux. Because the group pattern is `*`, this one un-bumpable crate takes the other nine legitimate updates down with it.
  Fix: `ignore` `llama-cpp-2` in the root `cargo` group (the maintainer already owns ABI-pin bumps by hand — the manifest comment says exactly that), or exclude it from the group pattern so the remaining nine can merge on their own.
- **#1 "bump the github-actions group with 3 updates" — fails the macOS `check` job.** This is F11's fail-closed design working as intended: `check-model-gates.sh` hardcodes the approved action SHAs, so an unreconciled bump must fail. It is not a defect; it *is* the manual reconciliation cost, now measured — and it is what leaves a PR sitting.
- **#2 "bump llama-cpp-2 0.1.146 → 0.1.152 in /tools/spike" — fully green.** Caveat before merging it alone: it would leave the spike tree at 0.1.152 while the root workspace stays pinned at 0.1.146, splitting the ABI pin the two trees deliberately duplicate. Bump root (both target entries) and spike in one change, then re-run the model gates.

There is also one failing "Dependabot Updates" run on `67a74b2` (one of the three ecosystems failed to compute an update); harmless but worth a glance next time the queue is touched.

#### F16. The god-file metric is misleading; the real concentration is `run()` — NEW P2 ARCHITECTURE (re-frames F5/F6)

Both `Qfd.md` and `ROADMAP.md` quote raw file lengths (16,036 / 13,353). Measured against the last top-level `#[cfg(test)]` boundary:

| File | Total | Production | Inline tests |
|---|---:|---:|---:|
| `crates/app/src/run_loop.rs` | 16,036 | **5,956** | 10,080 (63%) |
| `crates/platform_macos/src/lib.rs` | 13,353 | **5,831** | 7,522 (56%) |

So the "god files" are roughly 6k-line production modules with 8–10k-line test modules bolted on. Two consequences:

1. **Stop quoting total lines as the architecture metric** — it overstates the debt by ~2.7× and makes each extraction look smaller than it is. The honest metric is production lines plus function-level complexity.
2. **The remaining concentration is one function.** `run()` (`run_loop.rs:3918-5956`) is **2,039 lines**: one heartbeat `while`, 119 `if`s, 23 `match`es, ~221 distinct helper calls, and a maximum nesting depth of **13** (deepest at `run_loop.rs:4194`, inside the autocorrect/full-autocorrect decision chain). `loop_state.rs` correctly grouped the *state*; the next honest step is extracting heartbeat *phases* as functions over those structs (poll policy edges, pump settings watchers, drain engine outcomes, flush monitored input, render tray) — each taking `&mut PolicyState` / `&mut SettingsState` / etc., which is now possible precisely because the state is grouped. That reduces nesting and makes each phase independently testable; further file-splitting does not.
3. Separately, a single 10,080-line inline `mod tests` is its own navigation cost. Splitting it into `#[path]`-included test modules by concern is behavior-free and low-risk, and it would make the production/test ratio visible in `wc -l` instead of hiding it.

#### F17. The `ax_worker` carve preserved over-broad visibility — NEW P3 OPTIMIZATION

`platform_macos/src/lib.rs:68` re-exports `AxWorker`, `AxWorkerResource`, `CallbackDispatcher`, and `ObserverNotification`. Grep across the workspace and the seven `platform_macos` examples finds **zero** consumers outside the crate; the only thing keeping `AxWorker` reachable is `pub fn with_worker`, itself called only from `MacosPlatformAdapter::new` (`:1076`) and the in-crate test-hook variant. The commit's "zero new pub surface" claim is accurate — these four were already `pub` before the move — but the carve was the natural moment to narrow them. Making the four types and `with_worker` `pub(crate)` shrinks the crate's public API and rustdoc surface with no behavior change and no test change.

#### F18. `docs.yml` has no branch filter — NEW P3 CI

`ci.yml` scopes pushes to `branches: [main, "spike/**"]`; `docs.yml` has `on: push` with paths only, so it also runs on every dependabot and feature branch (observed live: the Docs workflow ran on `dependabot/github_actions/github-actions-03481714fc`). PRs already get the same checks through `ci.yml`'s `check` job, so those runs are pure runner spend. Adding `branches: [main, "spike/**"]` gives parity with `ci.yml` at zero coverage cost.

### Corrections to the earlier record

- **§12 F7 sharpening was wrong in its split.** It said `PlatformAdapter` has "14 methods (12 required + 2 defaulted: `popup_anchor`, `focused_page_url`)". A signature-by-signature read of `crates/platform/src/lib.rs:491-593` gives 14 methods = **10 required** (`environment`, `subscribe_focus`, `subscribe_caret`, `subscribe_accept`, `front_app`, `capabilities`, `read_context`, `caret_rect`, `insert`, `insert_replacing`) + **4 defaulted** (`popup_anchor`, `focused_page_url`, `text_range_rect`, `insert_replacing_range`). The two missed defaults are exactly the Tier-5 grammar range surfaces that `ROADMAP.md` documents as fail-closed trait defaults, so the omission mattered: it hid the fact that a third of the contract is default-inherited by the Windows/Linux adapters. The total of 14 — the figure the docs actually carry — was and remains correct, so no documentation change follows from this; only this record needed the fix.
- **§13's validation line "1,942 listed: 1,012 parallel-lane + 923 serial-lane + 6 ignored + 1 latency corpus guard"** does not add up (1,012 + 923 + 6 = 1,941) and mixes lane inventories with ignore counts. The clean statement is: **1,942 listed = 1,936 executed + 6 ignored** in the single serial workspace run; the CI lane split partitions the same inventory (24 crates parallel, `platform_macos` + `app` serial).

### Alignment with objectives and plan

| Objective | State on `67a74b2` | Alignment |
|---|---|---|
| Local, no-telemetry inline completion | Implemented; privacy boundaries enforced as policy and tested | Aligned |
| macOS first | v0.1.5 signed/notarized/stapled and cask-backed; `main` adds hardening + the refactor program | Aligned |
| Deterministic core behind `PlatformAdapter` | 14-method contract, 4 fail-closed defaults, pure core 98–100% covered | Aligned |
| A2/A3 parity + grammar | Code complete; 22 live gates outstanding | Partially aligned pending live evidence (F3) |
| Windows/Linux committed deliverables | Phase 0 + fail-closed facades + hosted CI parity; no real adapters | Largest implementation gap, unchanged |
| Secure release pipeline | Strict tag guard, provenance, post-publish verification all in place | Aligned; `post_verify` still unproven operationally (F10) |
| ROADMAP as source of truth | Governance wording, counts, and the refactor record are all current | Aligned, with one stale count in ARCHITECTURE (F14) |

`ROADMAP.md`'s header is now accurate but is a single ~900-word sentence carrying the entire 2026-07-21/22 program. It reads as a changelog, not a header; moving the delivery narrative into the existing `<details>` log and leaving the header as date + branch + test count would make the pending-work ledger findable again. Cosmetic, not a defect.

### 2026-07-25 validation record

Passed: `cargo fmt --all -- --check`; `cargo clippy --locked --workspace --all-targets -- -D warnings`; `cargo test --locked --workspace --all-targets -- --test-threads=1` (**1,936 passed / 0 failed / 6 ignored**); `cargo test --locked --workspace --doc` (0 tests, no doctests exist); `tools/release/check-version-docs.sh`; `bash tools/release/check-model-gates.sh`; `tools/release/finalize-cask.sh --self-test`; `tools/release/check-github-governance.sh --repo mudrii/compme` (live, exit 0, six accepted caveats); full-repo relative-link check (0 missing); live GitHub run/PR inventory.

Not executed this round: `shellcheck` (still not installed locally — CI's Linux and Docs lanes cover it, green on HEAD); `cargo llvm-cov` (unchanged production surface since §3's 86.50%/82.76%/85.49% measurement plus the verbatim moves; not re-measured); model-backed gates and bundle smokes (unchanged since §13's program validation); the 22 live macOS gates; Windows/Linux runtime acceptance; `post_verify` (needs a real tag).

### Prioritized next actions (supersedes §13's list)

1. **Unblock the dependabot queue (F15).** `ignore` `llama-cpp-2` in the root cargo group, then bump root (both target entries) + spike together in one commit and re-run the model gates; reconcile #1's action SHAs through `check-model-gates.sh`; re-run #3.
2. **Fix `ARCHITECTURE.md:28` (25 → 26) and pin the crate count in a checker (F14).**
3. **Close and record the 22 macOS live gates (F3)** — owner action, still the product critical path.
4. **Extract heartbeat phases out of `run()` (F16)**, one behavior at a time over the `loop_state` structs; stop quoting total file lines as the architecture metric.
5. **Windows UIA Phase 1 → Linux X11/AT-SPI Phase 2** — largest committed deliverable, unchanged.
6. **Let the next release prove `post_verify` (F10)**, including cask/checksum consistency.
7. Low-cost cleanups when convenient: `pub(crate)` the four `ax_worker` re-exports (F17), branch-filter `docs.yml` (F18), split the 10k-line inline test module (F16.3), slim the ROADMAP header sentence.
8. Optional future deepening: typed settings/tray commands/snapshots/events — still a design task, not debt.

---

## 15. 2026-07-26 remediation of the §14 findings

Every §14 finding that is fixable in-repo was implemented the day after the audit, in four commits on `main`, each validated before the next started.

| Commit | Finding | What landed |
|---|---|---|
| `8aa70f7` | F14, F15 (config), F17, F18 | crate-count fix + checker pin; dependabot `ignore` for `llama-cpp-2`; `pub(crate)` AX-worker surface; `docs.yml` branch scope |
| `8267518` | F16.3 | inline test modules split into `run_loop_tests.rs` / `lib_tests.rs`, nine checker pins repointed |
| `1090aa5` | F16.2 | eight heartbeat phases extracted from `run()` |
| this commit | F16.1, record | ROADMAP header restructure with corrected metrics; this section |

### Finding status after remediation

| Finding | Status |
|---|---|
| F14 crate-count contradiction | **RESOLVED** — `ARCHITECTURE.md:28` says 26, and `check-model-gates.sh` now pins *both* crate-count sentences against `cargo metadata`'s member count, alongside the existing README/DEVELOPMENT pins. A future crate split fails CI instead of drifting. |
| F15 dependabot | **RESOLVED for the defect, verified live; the dependency review is the owner's.** Dependabot re-ran within minutes of the push: the spike llama PR (#2) auto-closed as ignored, and #3 was replaced by #4, which now *resolves* and compiles instead of dying at `failed to select a version for llama-cpp-2`. #4's remaining red is the genuine `ureq` 2→3 API migration (`AgentBuilder`, `Error::Status`, `Response::header`, `into_reader`) — an honest, actionable failure rather than a structural one. |
| F15 detail | **RESOLVED for the defect; the dependency review is the owner's.** `llama-cpp-2` is ignored by both cargo ecosystems, so the next grouped PR is resolvable; the manual bump procedure (both target entries + `tools/spike`, then `run-model-gates.sh`) is documented in `dependabot.yml`, DEVELOPMENT, and RELEASING. Deliberately **not** done here: adopting PR #3's remaining content, which is six *major* migrations — `getrandom` 0.3→0.4, `aes-gcm` 0.10→0.11, `rusqlite` 0.32→0.40, `sha2` 0.10→0.11, `ureq` 2→3, `ed25519-dalek` 2.2→3.0. Those touch the crypto, database, and HTTP seams, each needs code changes, and three of them are exact-pinned precisely because the maintainer reviews them case-by-case. Same for PR #1's `upload-artifact` v4→v7 / `download-artifact` v4→v8 majors: nothing in branch CI exercises artifact round-trips, so they would stay unproven until a real tag. Both are decisions, not defects. |
| F16 god-file metric + `run()` | **RESOLVED.** Metric: the inline test modules moved to sibling files, so `wc -l` now reports the production surface directly (`run_loop.rs` 6,092, `platform_macos/lib.rs` 5,837), and ROADMAP/Qfd quote those numbers. Function: `run()` is **1,546 lines, down from 2,039**, with eight phases extracted — `setup_pane_actions_phase`, `apps_row_delete_phase`, `apps_row_policy_edit_phase`, `personalization_edits_phase`, `model_download_phase`, `drain_deep_links_phase`, `tray_collection_toggle_phase`, `tray_app_disable_phase`. Every move is verbatim; the only edits are `&mut x` → `x` where a binding became a reference parameter. |
| F17 over-broad `pub` | **RESOLVED, and it paid for itself.** The four AX-worker types and `with_worker` are `pub(crate)`. Narrowing immediately exposed two methods the `pub` had been masking from dead-code analysis — `AxWorker::install_resource` (production goes through `AxWorkerHandle`) and `AxWorkerResource::close` (production relies on `Drop`) — both are test-only and are now `#[cfg(test)]`, out of the shipped binary. |
| F18 `docs.yml` scope | **RESOLVED** — `branches: [main, "spike/**"]`, matching `ci.yml`. |
| F7 split correction, §13 arithmetic | **RECORDED** in §14; no doc changed, because the docs only ever carried the correct total. |
| F3 / F10 / Windows-Linux adapters | **OPEN, unchanged** — owner action, next tag, and the largest committed deliverable respectively. |

### What was deliberately left inline (F16, honest scope)

The settings-watcher run (autocorrect, full-autocorrect, thesaurus, launch-at-login, trailing space, midline, context, emoji) and the host-event arm were **not** extracted. Each needs 15–20 bindings, so a function would have taken a wide context struct — the exact "relocated state, not a deeper interface" failure that F5/F6 warns about, and that the 27-field `RunContext` already demonstrated. They need a real seam (typed settings commands; a host-event context type), which is a design task, not a move. `run()`'s maximum nesting depth is still 13, inside the host-event arm, for the same reason.

### 2026-07-26 validation record

After each commit and again at the end: `cargo fmt --all -- --check`; `cargo clippy --locked --workspace --all-targets -- -D warnings`; `cargo test --locked --workspace --all-targets -- --test-threads=1` → **1,936 passed, 0 failed, 6 ignored** (identical to the pre-remediation run — app 546 and platform_macos 346 unchanged through the test-module split); `check-model-gates.sh` live; `check-version-docs.sh` (8 surfaces); `actionlint`; `e2e-complete-me.sh --self-test`; `bundle-smoke.sh`.

Not executed: local `shellcheck` (unavailable; CI's Linux and Docs lanes cover it), model-backed gates and coverage (unchanged model/policy surface — the refactors are verbatim moves), the 22 live macOS gates, Windows/Linux runtime acceptance.

---

## 16. 2026-07-26/27 deep dive — what the previous rounds missed

A dedicated pass asking "what could this audit method not see?", run after the §15 remediation. It found one live defect in a release gate, two gate-coverage holes, and a measurement error that had been repeated in every previous audit.

### First: hard verification of the §15 refactor

The §15 commits claimed "verbatim moves". That claim was asserted, not proven, so it was proven here: the old `run()` body was compared against the new `run()` **with all eight phase functions inlined back at their call sites**, as a normalized token sequence (comments and whitespace stripped).

- Tokens present in the old body but missing from the new: **0**. Nothing was dropped.
- Insertions: **0**.
- 13 differing hunks, every one accounted for: 11 borrow adaptations (`&`/`&mut` removed where a binding became a reference parameter) and 2 rustfmt collapses (a trailing comma, and `|| { expr }` → `|| expr` now that the closure fits on one line four indents shallower).

Same check on the test-module split: `run_loop_tests.rs` differs from the old inline module by 14 trailing commas, `lib_tests.rs` by 4 commas plus the `mod tests {` closing brace. Both splits are token-exact.

This is now the standard for claiming a refactor is behavior-preserving here (recorded as an `AGENTS.md` lesson): green tests and clippy do not prove it, because the extracted code has no direct test coverage.

#### F19. Per-file coverage was inflated by inline tests — the same error as the line counts, missed in §14

§3 reported `app/src/run_loop.rs` at 82.86% and `platform_macos/src/lib.rs` at 79.18%, and a workspace total of 86.50% / 82.76% / 85.49% (regions/functions/lines). Those numbers counted each file's ~10,000 and ~7,500 lines of **inline test code**, which is ~100% covered by construction. With the tests in sibling files (`cargo-llvm-cov` excludes `tests/`, `examples/`, and `*_tests.rs`), the honest production numbers are:

| Surface | Reported §3 | Actual 2026-07-26 |
|---|---:|---:|
| Workspace regions / functions / lines | 86.50% / 82.76% / 85.49% | **82.68% / 81.14% / 80.99%** |
| `app/src/run_loop.rs` regions | 82.86% | **52.28%** |
| `platform_macos/src/lib.rs` regions | 79.18% | **54.39%** |

So `run_loop.rs`'s production code is roughly **half** covered, not four-fifths, and the aggregate is ~4.5 points lower than claimed. F16 caught this distortion in the *size* metric and stopped there; the *coverage* metric had the identical defect and went unexamined. The two biggest uncovered blocks are the same two the audit already refuses to extract without a real seam (`run_loop.rs` 2,377 missed regions, `platform_macos/lib.rs` 2,370), and the eight newly extracted phases are now individually unit-testable — that is the concrete path to raising it.

Secondary discovery: coverage could not run at all. `rust-toolchain.toml` lists only `clippy`/`rustfmt` (profile `minimal`), so there is no `llvm-tools-preview`; on this machine the toolchain is Homebrew Rust, which ignores `rust-toolchain.toml` entirely and has no rustup components. `cargo llvm-cov` failed with `failed to find llvm-tools-preview` and no documentation existed anywhere in the repo. It now works via `LLVM_COV`/`LLVM_PROFDATA`, and `DEVELOPMENT.md` documents both paths plus the exclusion semantics and the new baseline.

#### F20. A stray coverage `.profraw` crashes the privacy release gate — live defect, now fixed

Running coverage leaves `default_*.profraw` files in each test process's CWD, i.e. inside crate directories. `check-privacy-policy.sh` walks `crates`, `tools`, `.github`, `Casks`, `README.md`, and `docs`, skipping only known-binary *extensions*, and read each file with `File.read(path, invalid: :replace, undef: :replace)`. Those options are **inert without a transcoding pair**, so the invalid bytes survived and `String#scan` aborted the whole gate with a raw Ruby backtrace:

```
-:144:in `scan': invalid byte sequence in UTF-8 (ArgumentError)
```

Impact: any non-UTF-8 byte anywhere in the scanned trees takes down a release gate with an error that names neither the file nor the cause — it took several steps to trace even while holding the diff that caused it. Worse, it fails *closed but opaque*: the gate never reports whether a denied host exists.

Fixed by reading as binary and transcoding with replacement, plus a discriminating self-test fixture (a binary stray next to a file containing a denied telemetry host, asserting the scan survives **and** still catches the host). Mutation-verified: restoring the old `File.read` line kills the new fixture. `.gitignore` now covers `*.profraw`/`*.profdata` so the stray cannot reappear as untracked noise.

#### F21. Two self-tests were never gated anywhere — the gate-of-gates gap

Of 24 self-test-capable scripts, CI ran 20. Two of the four omissions are deliberate (the A2 runner and its ledger checker are local/manual-only, and the policy checker actively rejects their presence in CI). The other two were oversights:

- **`check-model-gates.sh --self-test`** — CI ran the checker live but never its own fixture machinery. A broken `require_line`/`reject_line`/test-symbol helper could go dead-green while the live run still passed, which is precisely the failure mode that makes a gate-of-gates worthless.
- **`tools/dev/check.sh --self-test`** — nothing anywhere ran the developer gate runner or verified its DEVELOPMENT-fence parser.

Both pass today and are now CI steps. Related: the documented "Full Local Gate" omitted `actionlint`, which CI runs — so workflow edits (there were four this week) passed the local gate and were only linted remotely. The fence now includes `actionlint` and the two self-tests, which is also the fix for the deeper asymmetry: previous audits verified that CI covers the repo, never that the local gate covers CI.

#### F22. Ignoring a dependency in Dependabot also suppresses its security PRs — documented, compensated

The F15 fix (`ignore: llama-cpp-2`) has a side effect worth stating: Dependabot filters ignored dependencies before opening either kind of PR, so an ignored crate gets no security PR either. GitHub's own docs do not spell this out on the pages checked, so the safe reading is assumed. The compensating control already exists and is independent of Dependabot: `cargo audit` scans `Cargo.lock` against the RustSec advisory DB in CI's Linux lane and again weekly, and fails closed. `cargo audit` is clean on the current tree (220 dependencies, 0 advisories). This is now a comment in `dependabot.yml` so nobody has to re-derive it.

### Also checked, nothing found

Spike workspace (43 passed / 1 ignored, matching the documented 44), `cargo audit` clean, the 6 root ignored tests are all model-backed and are executed by `run-model-gates.sh` with `--ignored`, all 22 manual gate IDs present, zero assertion-free tests across 1,940 `#[test]` items (an earlier 3-hit scan was a raw-string brace-counting artifact), no dead-green `grep` guards in the checkers (`require_line`/`reject_line`/`require_test_symbol` all fail loudly on a missing target), and repo-wide relative Markdown links intact.

### Prioritized next actions (delta on §14's list)

1. **F3 / F10 / Windows-Linux adapters** — unchanged: owner action, next tag, largest deliverable.
2. **Raise `run_loop.rs` production coverage from ~52%** by unit-testing the eight extracted phases with fakes; they are reachable now and each has a ≤8-argument signature.
3. When the settings/tray typed-command seam lands, re-measure coverage — the watcher block is a large share of the uncovered half.
4. Optional: add a coverage job to CI. Deliberately not done — it needs `llvm-tools-preview` on the macOS lane and there is no threshold anyone has agreed to; the documented local command is enough for now.
