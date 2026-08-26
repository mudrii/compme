# 2FIX — Implementation Plan (plan · code · CI)

> **Basis:** the eight-pass review ledger, consolidated 2026-08-25 against
> `main` @ `6b490be`. Every one of the **69 implementation findings** is scheduled
> below in one of **15 work packages (WP1–WP15)**, ordered by risk and by
> shared files. Each item keeps its original ID and its key evidence cites
> so the ledger's substance survives the consolidation; Appendix B records
> the refuted/merged/deferred IDs, Appendix C the known-open items tracked
> elsewhere, and Appendix D the validation evidence summary.
>
> **How to use this file:** work top-down by WP. One WP = one coherent
> change-set (usually one commit, sometimes two where a tripwire forces a
> split). Tick items off by editing their `[ ]` boxes; when a WP lands,
> note the commit hash on its header line.

## Implementation audit — 2026-08-25

- **Implemented:** all 69 active findings are checked below. The detailed,
  one-record-per-finding change and verification evidence lives only in
  `FIXED.md`; this plan deliberately does not repeat it.
- **A32 policy selected:** shutdown allows 250 ms for cooperative cancellation
  plus acknowledged ordered model teardown. A safe vendored extension wires
  llama.cpp's CPU/Metal abort callback to the terminal cancellation flag. If a
  native call still never returns, the run loop arms a last-drop hard-exit guard
  plus watchdog; ordinary Rust scope cleanup runs first when the watchdog can be
  spawned, then `_exit` on Unix or `TerminateProcess` on Windows prevents an
  unsafe detached continuation. Watchdog-spawn failure exits immediately.
- **External evidence still pending:** A53 needs the next real tag's complete
  post-verify/finalization run. A2's full macOS gate, A21's real BSD/Swift
  execution, macOS GUI acceptance and 22 manual LOOK gates, A32's real-Mac Metal
  cancellation, and Windows hardware acceptance remain evidence obligations;
  they are not synthesized from this Linux review.
- **Current host validation:** portable Rust, Linux unit/live suites, the root
  model-client real-model suite with the hosted-runner latency opt-out, the exact
  250 ms CPU cancellation case, workflow policy self-tests, actionlint, and
  portable release-script self-tests pass. The
  separate 500 ms warm-latency budget measured 1,871 ms on this CPU-only Linux
  host and remains a real-Mac pre-tag obligation. The literal full local gate
  remains macOS-only because its workspace commands compile Apple frameworks
  and its icon self-test invokes the real Swift toolchain.

## Ground rules (apply to every WP)

1. **Gate discipline.** Every commit lands gate-green:
   `tools/dev/check.sh` runs the full documented fence. The full gate needs
   a macOS host; on Linux, run the CI-lane equivalents and let the macOS CI
   `check` job be the authority (see Appendix D for what this host can and
   cannot run).
2. **Test-count anchors.** Any commit that changes the macOS-lane
   `cargo test --workspace --all-targets -- --list` count must re-stamp all
   four documented surfaces **in the same commit**: `README.md`, both count
   anchors in `docs/DEVELOPMENT.md`, `docs/ROADMAP.md`, and
   `docs/superpowers/specs/2026-07-01-grammar-fix-design.md`. Tests
   inside `#[cfg(all(test, target_os = "linux"))]` modules do **not**
   count (they never exist on the macOS lane); ordinary `#[cfg(test)]`
   tests in portable crates do.
3. **Workflow/checker tripwire.** Any edit to `.github/workflows/*` or to
   pinned doc lines must re-stamp `tools/release/check-model-gates.sh` in
   the same commit. Run `check-model-gates.sh --self-test` locally (works
   on any host); the live checker validates on the macOS CI lane.
4. **Version anchors.** Doc reformats must preserve the exact anchor
   phrases `tools/release/check-version-docs.sh` matches; run it after any
   doc edit that touches a version surface.
5. **Test placement.** `run_loop` tests go in `run_loop_tests.rs`,
   `platform_macos/lib.rs` tests in `lib_tests.rs` (`#[path]` siblings).
   `platform_macos` and `app` test lanes are serial
   (`-- --test-threads=1`).
6. **Host-portable platform crates.** `platform_linux`/`platform_windows`
   compile and test on all three hosts: encode POSIX rules on strings, no
   build-host `std` path semantics, and run a non-mac lane before claiming
   a platform-crate change green.
7. **Live evidence.** Never mark a live gate or a live behavior fixed from
   a headless run; the Linux live suite runs under Xvfb in CI, the macOS
   live gates need a granted GUI session.
8. **Style.** Commit directly to `main`, minimal diffs, stdlib first,
   YAGNI; non-trivial logic ships with a test.

## Execution order and dependencies

| WP | Theme | Items | Depends on | Host needed |
|---|---|---|---|---|
| WP1 | P1 hotfixes | A50, A41 | — | any (A41 live-verify: Linux) |
| WP2 | Linux data-loss guard | A61 | — | any (headless test) |
| WP3 | Live-test count restamp | A1 | WP2 | Linux + any-host self-test |
| WP4 | Policy-checker hardening | A11, A17, A51 | — | any (self-test) + macOS CI |
| WP5 | Plan & doc accuracy pass | A3, A6, A12, A22, A28, A33, A34, A40 | WP3 (same files) | any |
| WP6 | C.2 diagnosis enablers | A7, A10, A47 | WP1 (A41 unblocks live probe) | Linux + live session |
| WP7 | Linux accept/overlay cluster | A8, A9, A42, A43, A58, A60, A63, A68 | WP2, WP6 | Linux + Xvfb harness |
| WP8 | Download lifecycle | A25, A30 | — | any |
| WP9 | Panic containment & observability | A18, A31, A62 | — | macOS for live verify |
| WP10 | Local-gate portability | A2, A4, A5, A20, A21, A29 | — | Linux + macOS re-run |
| WP11 | CI cache hardening | A15, A73, A74 | WP4 (checker churn) | any + CI |
| WP12 | CI parity & release ops | A14, A23, A24, A35, A36, A37, A49, A53, A54, A55, A56 | WP11 (same files) | any + CI; A53 needs next tag |
| WP13 | Teardown & cross-crate contracts | A32, A38, A44, A48 | — | any |
| WP14 | macOS adapter posture & dead code | A19, A26, A52, A57, A64, A65, A66, A67, A71 | WP9 (same files) | macOS |
| WP15 | Small accuracy & hygiene | A13, A39, A59, A69, A70 | — | any |

WP1, WP2, and WP4 are independent (WP1 first — both items are P1).
WP3 follows WP2 so A61's new ignored test is included in the one live-count
restamp. WP4 → WP11 → WP12 must land in that sequence:
all three edit `check-model-gates.sh`/workflows and each re-stamp builds
on the previous shape. WP7 folds several same-file items; do not split
A42/A43/A58/A60/A63 across commits that each force an X11-tap re-review.

### Implementation contract used

- **First batch:** WP1/A50, WP1/A41, WP2, WP3, then WP4. For each item,
  write the named regression first, observe it fail for the intended
  reason, implement only the listed seam, run the focused crate/self-test,
  inspect `git diff`, then run the host-available Full Local Gate.
- **Linux behavioral batch:** WP6's registry landed before its live probe;
  WP7 followed after the probe identified stale completion as the reproduced
  C.2 cause. Static/unit success alone did not close C.2.
- **Workflow batch:** WP11 follows WP4; WP12 follows WP11. Every workflow
  shape change and its checker expectation/mutation fixture land together.
- **Explicit stop gates:** A32 closed only after shutdown fault injection
  proved the unresolved native-call boundary and the safe native abort callback,
  bounded join, and forced-exit fallback were implemented and tested. A13 closed
  after the C.2 probe.
  A53's workflow implementation is checked, but its real-tag proof still waits
  for a real release. macOS live evidence and the 22 LOOK gates require a
  granted macOS GUI and remain external evidence obligations.
- **No release in this plan:** v0.1.6 is the selected milestone, not an
  authorization to tag, publish, or finalize it.

---

## WP1 — P1 hotfixes (A50, A41)

**Goal:** remove the two live-proven P1s: a per-keystroke panic on the
primary macOS path and a fatal exit that keeps the Linux binary from
starting at all. **Commit shape:** two commits (independent crates).

### [x] A50 — trailing shell operator panics the run loop (P1, macOS)

- **Problem:** `compat::is_go_command` indexes `tokens[0]` unguarded
  (`crates/compat/src/lib.rs:488`). `right = skip_env_assignments(&tokens[index + 1..])`
  (`:359`) is empty when the operator is the line's last token, and
  `right.first().is_some_and(...) || is_go_command(right)` (`:378`)
  evaluates `is_go_command(&[])` before the strong-operator early return
  (`:379-381`) → index-out-of-bounds panic. Triggers: trailing `|`, `||`,
  `;`, `&&`, `<`, `2>`, `2>>` in Terminal/iTerm2. Live-panicked twice via
  `terminal_prompt_activates("com.apple.terminal", "foo bar |")`. Callers
  are unguarded per-keystroke gates (`run_loop.rs:1029`, `:2126`,
  `feature_policy.rs:173-175`); zero `catch_unwind` in `crates/app`.
- **Change:** add an empty-slice guard as the first line of
  `is_go_command`:
  `let Some(first) = tokens.first() else { return false; };` and use
  `first` in place of `tokens[0]`. With an empty `right`, `:378` is then
  false and control falls to the `:379-381` strong-operator return — the
  originally intended semantics. Do not touch the guarded call site at
  `:289`.
- **Tests:** extend the `terminal_prompt_activates` table (inline test
  module, `compat/src/lib.rs:839-1317`) with trailing-operator rows for
  each trigger (`"foo bar |"`, `"explain this ||"`, `"x ;"`, `"a &&"`,
  `"b <"`, `"c 2>"`, `"d 2>>"`) asserting the non-panicking expected
  activation result, plus one direct `is_go_command(&[])` unit test.
- **Re-stamps:** new portable tests change the macOS-lane count →
  re-stamp the four test-count anchors (Ground rule 2) in the same commit.
- **Verify:** `cargo test -p compat`; the previously panicking repro line
  as a test; full gate.

### [x] A41 — Linux exits fatally when accessibility is absent (P1, Linux)

- **Problem:** `LinuxAdapter::with_accessibility()` never fails (no bus ⇒
  `session: None`, documented "a supported configuration, not an error",
  `platform_linux/src/lib.rs:154-161`), but `subscribe_focus` with no
  session returns `UnsupportedField` (`:187-200`);
  `accessibility_trusted()` trait default is `true`
  (`platform/src/shell.rs:29-31`, no Linux override), so
  `subscription_error_action` maps the error to `Fatal`
  (`run_loop.rs:3337-3343`) and `main.rs:30-33` exits 1. Live-proven twice
  plus re-observed in the pass-8 gate run. The reason string also
  mislabels the no-session state as "not yet implemented (Tier 1.1
  scaffold)" (`:196-200`).
- **Change (three parts, one commit):**
  1. **Represent and classify the state.** Add the typed
     `PlatformError::AccessibilityUnavailable { reason }` variant, then
     extend `SubscriptionErrorAction` with non-fatal
     `Unavailable(String)`. Map only that new variant to `Unavailable`;
     keep `PermissionMissing` and an untrusted macOS runtime on
     `NoopUntilPermission`, and keep `UnsupportedField`, `CannotComplete`,
     `Timeout`, and `SecureInput` fatal when trusted. Focus/caret/accept
     install the existing inert subscriptions for `Unavailable`, but log
     the original platform reason without telling Linux users to grant
     macOS Accessibility or relaunch.
  2. **Make the Linux reason accurate.** Keep
     `platform_linux::unsupported()` for genuine scaffold surfaces and
     return `AccessibilityUnavailable` from `session_unavailable(operation)`
     on the `session: None` path, with reason
     "AT-SPI accessibility session unavailable (no bus)". Do not expose
     `has_accessibility_session` or couple `ShellHost` to the adapter:
     `make_shell()` is created and queried before `make_adapter()`, so the
     earlier shell-override prescription had no valid ownership path.
  3. **Do not weaken macOS:** the macOS-shaped fatal tests
     (`run_loop_tests.rs:8603, 8649-8677`) must keep passing unchanged.
- **Tests:** in `run_loop_tests.rs` (sibling file — Ground rule 5): a
  Linux-shaped `AccessibilityUnavailable` subscription becomes `Unavailable`
  and startup completes; permission-missing and trusted fatal matrices
  remain unchanged. In `platform_linux`: unit-pin both reason strings
  (implemented-but-unavailable vs unimplemented).
- **Re-stamps:** test-count anchors (new portable tests).
- **Verify:** rebuild the binary on this host and run without an a11y bus
  — must print a degrade notice and stay alive (re-run of the E15 repro,
  inverted expectation). This unblocks WP6's live C.2 probe.

---

## WP2 — Linux data-loss guard (A61)

**Goal:** no lossy write can ever leave the adapter. **Commit shape:** one
commit.

### [x] A61 — `insert_replacing_range` silently truncates >200k-scalar fields (P2)

- **Problem:** `MAX_FIELD_SCALARS = 200_000`
  (`platform_linux/src/atspi_live.rs:50`); `field_scalars` caps every read
  via `.take(...)` (`:437-443`); `insert_replacing_range` (`:371-434`)
  rebuilds the whole field from the capped snapshot (`:406-408`) and
  writes it back (`:411-413`) — everything past the cap is destroyed, and
  the equally-capped readback (`:422-428`) returns `Ok` when the
  replacement is not longer. `read_context` (`:245-253`) fabricates
  caret/context through the same cap. No guard exists anywhere
  (`lib.rs:400-409` forwards directly).
- **Change:** make over-cap fields fail closed before fetching or writing:
  1. Add one `field_scalars_checked` path that queries AT-SPI Text's
     `character_count()` first, validates/converts the count, and returns
     `Err(UnsupportedField { reason: "field exceeds 200000 scalars; refusing lossy read/replace" })`
     when the cap is exceeded. A failed/negative count is an error, not
     `0`; only then call `get_text(0, -1)`. Do not fetch `MAX+1`: the
     interface already exposes the authoritative size and fetching an
     unbounded huge value defeats the guard.
  2. Route **both** `insert_replacing_range` and `read_context` through
     the checked path. Before writing, validate the rebuilt scalar length with
     a checked helper so an at-cap field may stay the same size or shrink, but
     growth above the cap is rejected before mutation. The engine already
     handles `UnsupportedField` gracefully (fail-closed contract), so an
     over-cap field simply gets no suggestions/replacements rather than
     corruption.
  3. Update the module doc at `:361-370` so the stated swap-safety
     invariant matches the code again.
  4. If per-field UX for huge documents matters later, a windowed
     `read_context` is a **separate, follow-up** decision — do not build
     it now (YAGNI).
- **Tests:** factor the cap decisions into pure helpers (reported count and
  rebuilt length → verdict) and unit-test them headlessly, including same-size,
  shrink, and one-scalar-over rebuilt values at the cap (portable tests — count
  into anchors); add
  an `#[ignore]`d live-suite case in `atspi_live_tests.rs` that builds a
  cap-crossing field and asserts `insert_replacing_range` refuses without
  mutating (Linux-cfg'd — does not touch the anchors, **does** touch A1's
  live-test count: coordinate with WP3 so the restamped number is written
  once, correctly).
- **Re-stamps:** test-count anchors (pure helper test); ROADMAP live-test
  count if the live case lands in the same push (see WP3).
- **Verify:** `cargo test -p platform_linux`; live suite in CI (Xvfb lane
  runs the `--ignored` set).

---

## WP3 — Live-test count restamp (A1)

**Goal:** the SSOT stops lying about the live-test count, and the number
can no longer drift silently. **Commit shape:** one commit after WP2
(doc + checker + dedicated count check), so the number is written once
(currently 26; expected 27 after A61's live regression).

### [x] A1 — ROADMAP says 31 live tests; the tree has 26 (P2)

- **Problem:** `docs/ROADMAP.md:407` claims "31 live tests (25 AT-SPI + 2
  each …)"; the true executable ignored set is **26** (23 AT-SPI/X11 + 1 each
  confirm/keyring/reveal); `check-model-gates.sh:215`'s comment repeats
  "(31 currently)". Born wrong in docs-only commit `6abe2b6`; second
  occurrence of this drift class.
- **Change:**
  1. `docs/ROADMAP.md:407` → "26 live tests (23 AT-SPI/X11 + 1 each for
     confirm, keyring, reveal)" — or the post-WP2 number if that lands
     first; count from the emitter
     (`cargo test -p platform_linux -- --list --ignored`), never from a
     raw grep (doc comments inflate it).
  2. `tools/release/check-model-gates.sh:215` comment → the same number.
  3. **Pin it:** add `tools/release/check-linux-live-test-count.sh`. On
     Linux it runs `cargo test --locked -p platform_linux -- --list
     --ignored`, counts emitted tests (not source attributes), and matches
     the ROADMAP line. Give it a fixture-driven `--self-test` usable on
     any host. Run the live check in the Linux CI and release lanes; pin
     both step shapes in `check-model-gates.sh`. Pin the live-suite and
     harness-self-test commands exactly, with mutations for deleted and
     comment-only/no-op bodies. This avoids raw-grep comment inflation,
     prevents a named-but-inert gate from passing policy, and avoids
     pretending the macOS-only live checker can enumerate Linux-cfg'd tests.
- **Re-stamps:** the checker edit itself is the re-stamp; run
  `check-model-gates.sh --self-test`.
- **Verify:** dedicated fixture self-test and model-checker self-test green;
  Linux CI enumerates the live set and macOS CI validates the pinned
  workflow/checker topology.

---

## WP4 — Policy-checker hardening (A11, A17, A51)

**Goal:** close the three holes in the pinning net. All three edit
`check-model-gates.sh`; A11 also edits `ci.yml`/`docs.yml`. **Commit
shape:** one commit (single re-stamp).

### [x] A11 — pinned grammar spec is under `paths-ignore`; privacy checker never runs on the docs lane (P2)

- **Problem:** `ci.yml:16-17` ignores `docs/superpowers/**`, but the
  checker pins `docs/superpowers/specs/2026-07-01-grammar-fix-design.md`
  (`check-model-gates.sh:33`, `:3750-3774`) and `docs.yml` runs no pinning
  checker (`docs.yml:41-57`) — a spec-only direct push to `main` lands
  green. Second hole, same class: `check-privacy-policy.sh` scans `docs/`
  (`:37/:52/:138`) and runs on neither lane.
- **Change:**
  1. Narrow the ignore glob so the pinned spec always triggers full CI:
     replace `docs/superpowers/**` with
     `docs/superpowers/plans/**`. Specs remain on full CI by default;
     do not create an allowlist of today's unpinned specs that can become
     pinned later.
  2. Add a `check-privacy-policy.sh` step to `docs.yml` (ruby-only —
     runs fine on the docs lane).
  3. Re-stamp the checker's expected `paths-ignore` list
     (`check-model-gates.sh:182`) and add the new docs.yml step to
     `check_docs_integrity_controls`, same commit. Require the privacy
     command exactly and mutation-test a comment-only/no-op body, not only
     deletion of the named step.

### [x] A17 — only 17 of the 22 runner-pinned gate IDs are pinned (P2)

- **Problem:** the checker's `for gate in \` loop
  (`check-model-gates.sh:3924-3946`) omits the five 2026-07-17 parity IDs
  (`full-autocorrect-prose-code-look`, `cross-app-previous-inputs-look`,
  `selection-thesaurus-look`, `tray-external-links-look`,
  `sidebar-only-editor-assistant-look`); deleting one from the ledger
  would not fail the release checker.
- **Change:** add the five IDs to the loop; confirm the list `diff`s clean
  against ACCEPTANCE.md's "Exact runner-emitted manual gate IDs" (22) and
  the runner's `--self-test` loop (`run-a1b-live-gates.sh:740-761`).

### [x] A51 — job-level `permissions:` are outside the pinning net (P3)

- **Problem:** the checker pins audit.yml's workflow-level and
  `governance`-job permissions (`:238-239`, `:263-264`) but not the
  `audit` job's (`audit.yml:20-23`); in ci.yml only the `check` job is
  pinned permissions-less (`:204`, `:3171`) — `actionlint`/`spike`/
  `windows`/`linux` can silently gain write permissions; the mutation
  fixture (`:2322`) covers only workflow-level; `validate_actions!` skips
  audit.yml/docs.yml (`:3154`, `:3308`).
- **Change:** pin the audit job's permissions block; assert
  permissions-absence on the four unpinned ci.yml jobs; extend the
  mutation fixtures to job-level permissions; run `validate_actions!` on
  all four workflows.

**WP4 verification:** `check-model-gates.sh --self-test` green locally;
push → macOS checker lane green; deliberately verify the new pins bite by
running the extended self-test (its mutation fixtures must fail the
mutated copies).

---

## WP5 — Plan & doc accuracy pass (A3, A6, A12, A22, A28, A33, A34, A40)

**Goal:** one docs sweep that makes every planning surface match the tree,
so the next agent does not restart Phase 2 or trust a wrong count.
**Commit shape:** one commit. The A22 comment stays in WP5; run the checker
self-test after the workflow comment and update an expectation only if the
checker proves that comment is part of a pinned shape.

### [x] A12 — ROADMAP / spec / AGENTS still sequence Linux 2.1–2.5 as remaining (P3)

- **Change (restamp every listed surface):**
  - `ROADMAP.md:116` title ("real impls pending" → wired-adapter wording);
    `:146` fail-closed claim; `:161-164` overlay owed (shipped —
    XTEST/`wtype` still owed); `:286` 2.7 runner packages (landed, see
    `:312`); execution order `:1318-1320` (drop the five DONE phases;
    remaining Linux = A7–A10/A41/A42/A43/A47/A58/A60/A61/A63/A68,
    tray, shortcuts, `text_range_rect`, Wayland, then Windows UIA);
    header date `:3`. Use the WP table as the ID source so the ROADMAP
    does not omit half of the Linux cluster again.
  - Spec `2026-07-08-cross-platform-implementation-plan.md:3` ("phases 1–6
    pending") and `:352` (README "scaffold-only" instruction — obsolete).
  - `AGENTS.md:65-66` — "both platforms are fail-closed scaffolds" is
    false for Linux; reword to Linux-wired / Windows-scaffold.
  - Grammar spec `:1193-1196` — only `text_range_rect` remains the trait
    default (`platform/src/lib.rs:537-544`).
  - `DEVELOPMENT.md:47` lead-in; `ACCEPTANCE.md:26-27` (mention the live
    AT-SPI step); `Qfd.md:136-140` scaffold wording;
    `ARCHITECTURE.md:4-8` intro.
  - Delivery log `ROADMAP.md:53-79`: append the 07-29→08-18 batch
    (currently narrated only in Qfd §15–19). While in there, move the
    long-form narrative into the existing `<details>` blocks so the
    pending-work ledger is scannable (Qfd §14's structural ask).

### [x] A33 — no next-release milestone; Windows commitment has no forcing function (P4)

- **Decision for implementation:** plan the next patch release as
  **v0.1.6**, macOS-first, containing verified fixes and CI/doc repairs;
  Linux remains experimental and is not promoted by this release. Do not
  cut a release as part of these WPs. Add a short ROADMAP "Next release"
  section with that scope, the 22 live-gate closure criterion (Qfd F3),
  and the RELEASING checklist pointer. Keep Windows committed but mark
  Phase 1 🔒 deferred until the Linux C.2 ghost milestone closes; that is
  the explicit trigger, not an undated promise.

### [x] A34 — README/DEVELOPMENT enumerate 25 crates while claiming 26; "checked-in" GGUFs (P4)

- **Change:** add `shell_flags` to README's layout tree and
  `DEVELOPMENT.md:35-36`'s member list; reword `README.md:239` to "the
  local model paths (gitignored, fetched to `tools/spike/models/`)".

### [x] A3 — DEVELOPMENT.md over-claims the exact-pin set (P3)

- **Change:** reword `docs/DEVELOPMENT.md:53` to "every
  native/ABI-sensitive dependency carries an exact `=x.y.z` pin"
  (`sha2 = "0.11"` and `zeroize = "1"` are deliberately ranged; do not
  tighten them just to save the sentence).

### [x] A6 — RELEASING.md 3,609-character table cell (P5)

- **Change:** break the `ci.yml` row (`docs/RELEASING.md:23`) into a step
  list under the table. Preserve the version-anchor phrases; run
  `check-version-docs.sh` after.

### [x] A22 — `ci.yml:106-107` comment says "23 crates" (P5)

- **Change:** 23 → 24 (26 − platform_macos − app). Comment-only, but it
  lives inside a pinned workflow: run `check-model-gates.sh --self-test`;
  if the line is inside a pinned step shape, land it with WP4.

### [x] A28 — Qfd §18 contradicts §13 on F7/F11/F13 (P5)

- **Change:** reword `Qfd.md:751-752` ("Still open and unchanged") to
  match §13's RESOLVED rows (`:413`, `:417`).

### [x] A40 — personalization header claims dependency-free (P5)

- **Change:** `crates/personalization/src/lib.rs:4` → "pure, std +
  `webconfig` only" (dep at `Cargo.toml:8`, used at `lib.rs:177`).

**WP5 verification:** `check-version-docs.sh`, `check-agent-briefs.sh`,
`check-model-gates.sh --self-test` all green; read the diff once end-to-end
against the tree state (this pass exists because docs drifted — do not
introduce fresh drift).

---

## WP6 — C.2 diagnosis enablers (A7, A10, A47)

**Goal:** remove the two mechanisms that can silently cancel the 120 ms
debounce, then run the live probe until `shown>0` is explained. Requires
WP1 (A41) so the binary runs on the dev host. **Commit shape:** one code
commit + a probe session with recorded evidence.

### [x] A10 — two `FieldMinter`s; I/O ignores `generation` (P3)

- **Problem:** `subscribe_focus` and `subscribe_caret` each `start()`
  their own dispatcher with its own `FieldMinter::new()`
  (`atspi_events.rs:83`, `:113`, `:234`); the same live field gets
  different generations per lane, so `FieldHandle` equality fails on every
  focus→caret transition — hiding the ghost and clearing `pending_since`
  (`engine_core.rs:617-629`). Separately, `atspi_live` decodes
  `element_id` only, so writes to a stale handle mutate whatever occupies
  the path; the contract requires `StaleField`
  (`platform/src/lib.rs:24-27`).
- **Change:** replace the subscription-local minters with one
  adapter-owned internal `LinuxFieldRegistry`, held behind
  `Arc<Mutex<_>>` and shared by focus subscription, caret subscription,
  and every AT-SPI I/O method:
  1. `focus(element, describe)` mints/reuses the current handle and records
     the focused `ElementId`; moving to a different element invalidates the
     old generation. Returning later to a prior `(bus, path)` mints a new
     generation.
  2. `current_for(element)` returns a handle only when that element is the
     current focus. Caret dispatch uses it before any app/pid lookup,
     geometry D-Bus call, mint, or callback.
  3. `validate(field)` compares both element identity and generation;
     capabilities/read/caret/insert/range-replace all call it before I/O
     and return `PlatformError::StaleField` on mismatch.
  This is one deep ownership seam. Merely sharing `FieldMinter`s would
  still let bus-wide foreign caret events replace the current generation.
- **Tests:** registry unit tests: focus then caret for the same element
  yields the same handle; foreign caret returns `None` without advancing a
  generation; focus A→B→A invalidates the first A handle; each adapter I/O
  method rejects the stale generation. Existing live tests that construct
  generation-0 handles must use a `cfg(test)` registration helper or drive
  registration through focus; do not weaken production validation.

### [x] A7 — unscoped `TextCaretMoved` cancels the debounce (P2, probe)

- **Problem:** the caret subscription matches bus-wide
  `member='TextCaretMoved'` (`atspi_events.rs:114-115`); `decode_caret`
  filters nothing (`:150-152`); a failed foreign read →
  `on_context_unavailable` → `pending_since = None`
  (`run_loop.rs:4914-4916`, `engine_core.rs:645-647`); a successful
  foreign read overwrites `focus.current_field` (`run_loop.rs:4740`).
- **Change:** use A10's registry `current_for(element)` as the filter.
  Drop a foreign caret before enrichment or callback, so it cannot mint a
  handle, make D-Bus calls, overwrite `focus.current_field`, or invoke
  `on_context_unavailable`. Instrument `pending_since` set/clear, Tick,
  and `compme: request gen=` only behind `COMPME_DEBUG` (or a narrower
  temporary probe flag); remove the temporary lines after the C.2 session
  unless they are deliberately converted into WP9's durable diagnostics.
- **Probe protocol (the actual C.2 work):** with WP1+WP6 landed, run a
  live Linux session and watch the instrumentation until either `shown>0`
  or the next blocker is identified with evidence. Record the outcome in
  ROADMAP 1.1 (and close or re-scope C.2). Do **not** mark C.2 fixed on
  code inspection alone.
- **Tests:** pure-half test for the filter decision (event field vs
  focused field ⇒ drop/deliver); live-suite addition if the harness can
  synthesize a foreign caret event.

### [x] A47 — two process-wide subscription-id counters (P5)

- **Change (same files, fold in):** one shared counter (or prefix ids by
  subsystem) for `platform_linux/src/lib.rs:134` and
  `atspi_events.rs:71`.

**WP6 verification:** `cargo test -p platform_linux` on Linux **and** the
macOS/windows lanes (Ground rule 6); Xvfb live suite; the recorded probe
session.

---

## WP7 — Linux accept/overlay cluster (A8, A9, A42, A43, A58, A60, A63, A68)

**Goal:** the first `shown>0` session can accept, place a ghost, coexist
with desktop shortcuts, and survive layout/binding changes. All items
share `x11_tap.rs`/`x11_keys.rs`/`atspi_live.rs`/run-loop wiring — land as
one reviewed change-set (2–3 commits max: routing, tap, cleanups).
Depends on WP2 (A61's guard shapes A8's routing) and WP6.

### [x] A8 — emoji/typo accepts call fail-closed `insert_replacing` (P2)

- **Problem:** `AcceptFull`/`AcceptWord` with `replace_left > 0` emit
  `Command::Replace` (`engine_core.rs:683-688`) → dispatch calls
  `insert_replacing` (`engine.rs:656-668`) → Linux `UnsupportedField`. The
  adapter comment (`platform_linux/src/lib.rs:377-394`) claiming the
  engine routes through `insert_replacing_range` is false.
- **Change:** fix this in `engine_core`, where the value, scalar caret,
  and `InsertStrategy` are all known; the dispatch layer has none of the
  original text needed for a safe conversion. When
  `offer_replacement_multi` accepts a local replacement, derive and store
  `CorrectionRange { start: caret - replace_left, end: caret }` plus the
  exact original scalar slice. On accept:
  - `NativeRangeSet` emits `Command::ReplaceRange` with that range and
    original text;
  - `AxSet` keeps the existing `Command::Replace` path until macOS parity
    is separately proven;
  - non-atomic strategies remain gated out.
  Guard `replace_left <= caret` and slice by Unicode scalar index. Fix the
  false Linux adapter comment; do not invent a PlatformAdapter capability
  method that does not exist.
- **Tests:** core command-shape tests for a Unicode original, exact scalar
  range, `NativeRangeSet` → `ReplaceRange`, `AxSet` unchanged, invalid
  `replace_left` rejected, and expected-text mismatch propagated by the
  engine adapter test.

### [x] A9 — empty fields cannot place a ghost (P3)

- **Problem:** `count <= 0` and degenerate extents return
  `caret_rect = Ok(None)` (`atspi_live.rs:310-312`); Linux does not
  override `popup_anchor` (default `Ok(None)`,
  `platform/src/lib.rs:521-522`); both `None` → `show_failed`
  (`engine.rs:581-587`) — first character in an empty GTK entry never
  paints.
- **Change:** implement `popup_anchor` for `LinuxAdapter`: when caret
  extents are unavailable, fall back to the focused component's bounds
  (AT-SPI Component extents); log the no-geometry reconcile path.
- **Tests:** pure-half geometry-choice test; live-suite case (empty entry
  ⇒ anchor present).

### [x] A43 — installability probe measures default chords (P3)

- **Problem:** the probe trial-grabs `configured_bindings()` during
  `with_accessibility()` (`run_loop.rs:3606` → `platform_linux/src/lib.rs:166`
  → `x11_tap.rs:191-207`), which falls back to defaults while the store is
  empty (`x11_keys.rs:404-414`); `apply_startup_key_bindings` fills the
  store later (`run_loop.rs:3397`/`:3668`). `install()` then grabs the
  *configured* chords (`x11_tap.rs:384-394`). Live-reproduced (E15).
- **Change:** immediately after config load, call
  `apply_startup_key_bindings(&config)` before `make_adapter`; remove the
  same call from `subscribe_accept_after_startup_key_bindings` (and rename
  that helper to state its precondition) so startup applies bindings
  exactly once. The process-global store then contains persisted chords
  before Linux adapter construction performs its installability probe.
  Correct the now-true comment at `shell/stub.rs:116-119`.
- **Tests:** run-loop test pinning the ordering (bindings applied before
  adapter construction); re-run the E15 harness (rebind Tab→Return; the
  same adapter must probe/install the configured set).

### [x] A60 — `AnyModifier` grabs fail wholesale on unrelated chords (P2)

- **Problem:** every accept key is grabbed with `ModMask::ANY`
  (`x11_tap.rs:268-289`, `:283`; ungrab `:318`); X11 fails the whole grab
  with `BadAccess` if any modifier combination is owned elsewhere.
  Xvfb-reproduced twice (client owning Alt+Tab breaks an `AnyModifier`
  Tab grab while exact bare-Tab succeeds).
- **Change:** introduce one internal `GrabPlan { bindings, keys, grabs }`.
  `build_grab_plan(connection, configured_bindings)` resolves keycodes,
  discovers the NumLock modifier from X's modifier mapping (never assume
  Mod2), expands each configured mask across Caps/NumLock permutations,
  and deduplicates exact `(keycode, mask)` grabs. Trial-grab, install,
  rollback, ungrab, MappingNotify, and live rearm all consume that exact
  plan. Keep the current plan behind the tap's synchronization primitive;
  never maintain separate immutable binding/key snapshots.
- **Tests:** two-client Xvfb regression in the live suite: client A owns
  Alt+Tab, compme still acquires bare Tab (this is the E17 repro,
  productized).

### [x] A42 — no `MappingNotify` handling; grabs go stale on layout change (P3)

- **Problem:** keycode↔keysym resolution happens once
  (`x11_tap.rs:229-250`); `run_event_loop` swallows everything but
  KeyPress/KeyRelease (`:628-650`); after `setxkbmap`, grabs sit on old
  keycodes and `keysym_for_keycode` (`:140-146`) misattributes presses.
- **Change:** for keyboard/modifier `Event::MappingNotify`, build a new
  `GrabPlan` from the configured bindings and transactionally swap it. Once a
  new plan exists, ungrab old, try new, and restore old if installing the new
  grabs fails. If plan construction itself fails because the changed layout
  carries no accept keys, the old keycodes are unsafe: release them, disarm,
  clear watchdog state, and publish an empty fail-open plan until a later valid
  rebuild. Ignore pointer-only mappings. This same operation is the only
  re-grab implementation used by A63.
- **Tests:** live-harness cases: change the keyboard map mid-grab, then assert
  the rebound key still accepts; force a conflicting live rebind, then assert
  it errors and the previously armed plan still accepts; and remove every
  accept keysym so plan construction fails, then prove the stale grab is
  released, a later arm does not retry stale keycodes, the old hide deadline is
  cleared, and a valid restored map recovers (need a real X connection —
  `#[ignore]`d).

### [x] A63 — live-rebind path updates config but never re-grabs (P3)

- **Problem:** the tap snapshots bindings once into immutable
  `TapState::bindings`/`keys` (`x11_tap.rs:386-395`); Linux builds its
  `AcceptSubscription` **without** `with_rearm`
  (`platform_linux/src/lib.rs:261-279`), so `engine::rearm_accept_keys`
  is the platform-default no-op (`platform/src/lib.rs:396`, `:418-419`)
  while `apply_live_accept_keymap` (`run_loop.rs:919-967`, `:5389-5406`)
  updates the store and persists config. Latent (only producer is
  macOS-only, `settings_window.rs:2211-2246`), no tripwire.
- **Change:** wire Linux `AcceptSubscription::with_rearm` to A42's single
  transactional plan swap: re-read `configured_bindings()`, build the new
  plan, ungrab old, install new, and restore old if installation fails. A
  construction failure uses A42's empty-plan fail-open state instead. The
  producer is currently macOS-only, so this is latent
  infrastructure; keep it because it is a thin call into machinery A42
  already requires, not a second subsystem.
- **Tests:** live-harness: rebind while armed; new chord accepts, old one
  does not.

### [x] A68 — partial-spawn failure leaks a parked thread + X connection (P5)

- **Change (same file, fold in):** spawn sequentially under a local
  teardown guard. If dispatcher/watchdog spawn fails after the event
  thread exists, set `stopping`, wake the event thread, drop senders, and
  perform the existing bounded joins before returning `Err`. Merely
  dropping a Rust `JoinHandle` detaches it, so setting the flag alone is
  not cleanup.

### [x] A58 — three small X11-tap items (P5)

- **Change (fold into the same pass):** delete the unreachable
  "untranslatable DEFAULT chord" arm (`x11_keys.rs:386-389`); fix the
  watchdog-lock doc overstatement (`x11_keys.rs:333-335`,
  `x11_tap.rs:97-98`) to match the mechanism; set `armed_since_ms` only
  after a successful grab (`x11_tap.rs:451-453`).

**WP7 verification:** full `platform_linux` lanes on all three hosts; the
Xvfb `--ignored` suite locally and in CI; the E15/E17 harnesses re-run
with inverted expectations; test-count anchors restamped for any portable
tests added; ROADMAP live-test count (A1's pin) restamped for the new
live cases.

---

## WP8 — Download lifecycle (A25, A30)

**Goal:** a bad download can always be recovered from, and no hop can
downgrade to plaintext. **Commit shape:** one commit in `model_fetch` (+
one line in `app` if the caller-reclaim variant is chosen).

### [x] A30 — terminal failures strand `.part`; verification reopens it unsafely (P2)

- **Problem:** `HashMismatch` keeps the part "for the caller"
  (`model_fetch/src/lib.rs:392-405`) but the caller only logs
  (`run_loop.rs:986`, `:2965`); retries resume off the corrupt part
  (`:241-242`, `:338`) and re-fail forever with a multi-GB part pinned.
  Cap-boundary: mid-stream stop leaves `written <= max_bytes` (`:351-358`)
  but reclaim is strict `existing > max_bytes` (`:205`) — a part exactly
  at the ceiling is rejected forever. Hashing reopens the part with plain
  `File::open` (`:396`), a symlink-following TOCTOU seam.
- **Change (inside `download_with_agent`, so every caller is safe):**
  1. Delete `.part` before returning terminal `HashMismatch` or
     `SizeExceeded`; retain it only for resumable transport/timeout
     failures. Do this at every size terminal (oversized header,
     mid-stream cap, and pre-existing `> max_bytes`). Do **not** reject
     `existing == max_bytes` up front: it may be a complete crash-resumed
     file and should proceed to verification.
  2. Open the part once with the existing no-follow protections plus read
     access, flush + `sync_all`, seek to offset 0, and hash through that
     held handle (`BufReader<&mut File>` or an equivalent handle clone).
     Never close and reopen by pathname between download and verification.
  3. Keep `HashMismatch`'s public shape unchanged unless UX proves the
     filename is needed; cleanup makes the retry self-healing and avoids
     an unnecessary API change.
- **Tests:** `model_fetch` has a strong suite — add: mismatch deletes the
  part and a retry starts fresh; each size-terminal deletes the part;
  an exact-cap complete part reaches verification; hashing uses
  the held handle (symlinked part cannot be swapped in). Anchors restamp.

### [x] A25 — ureq default redirect policy allows https→http (P5)

- **Change:** `.https_only(true)` on `production_agent()`
  (`model_fetch/src/lib.rs:175-183`) + a pin test asserting the config.
  (ureq-3.4.0 defaults: `max_redirects = 10`, `https_only = false`;
  redirect-following itself is deliberate — HF→CDN hops, `:116`.)

---

## WP9 — Panic containment & observability (A18, A31, A62)

**Goal:** no single panic silently ends a subsystem for the session, and
degradation is visible. **Commit shape:** one commit per crate (app,
model_client, platform_macos).

### [x] A18 — three workers have no panic containment or health signal (P3)

- **Problem:** zero `catch_unwind` in `app/src/inference.rs` (loop ~:181,
  spawn :489-491); llama decode worker is a plain spawn
  (`model_client/src/lib.rs:201`); the AX worker's job loop runs jobs
  unshielded (`let _ = reply.send(job());`, `ax_worker.rs:739`) while five
  sibling boundaries are shielded. Release builds keep `panic=unwind` (no
  `[profile]` section), so one panic in the ~20 unsafe AX helpers kills
  the worker and every later AX call fails "AX worker is not running" —
  silently. In-tree trigger: worker jobs call `eprintln!` (`lib.rs:4394`,
  `:4623`), which panics on a failed stderr write.
- **Change:** containment differs by ownership:
  1. AX jobs are independent: wrap each job in `catch_unwind`, reply
     `CannotComplete { reason: "AX job panicked" }`, and continue so the
     next job can run.
  2. The app inference worker and llama decode worker own mutable model
     state that may be corrupted by a panic. Catch only at their worker
     boundary, record a visible terminal `Failed(reason)` health state,
     reject/close later submissions, and exit the worker. Do **not** keep
     decoding with the same context after a panic. Translate the app
     health state to `ModelUnavailable`, rather than leaving the UI in
     perpetual `Loading`.
  3. Log worker start/exit/death (rate-unlimited — these are once-ever
     events).
  4. Replace worker-side `eprintln!` with a non-panicking helper
     (`let _ = writeln!(io::stderr().lock(), …)`) — one small
     `pub(crate) fn` per crate, used at the cited sites.
- **Tests:** serial macOS AX test: a panicking job returns
  `CannotComplete` and the next job succeeds. Inference/model-client
  tests: injected panic sets `Failed`, later submit is rejected, and the
  run loop surfaces model unavailable.

### [x] A62 — focus/caret callbacks invoked under held mutexes (P3)

- **Problem:** focus dispatch holds `current_identity_key` (`lib.rs:1502`)
  and `field_tracker` (`:1510`) guards across `cb_for_dispatch(field)`
  (`:1514`); caret dispatch holds tracker (`:1572`) + coalescer (`:1577`)
  across `:1581`. A panicking subscriber poisons them; every later focus
  **and** caret event hits `let Ok(..) = ..lock() else { return; }` and is
  dropped forever (both closures share `self.field_tracker`, `:234`). The
  crate recovers with `PoisonError::into_inner` elsewhere (`:1409`,
  `:2384`, `:3181`) but not at these four sites.
- **Change:** the `field` is already cloned — restructure both dispatch
  closures to compute-under-lock, then `drop(guard)` explicitly **before**
  invoking `cb_for_dispatch`. (Guard-drop beats `into_inner` here: it
  also removes the latent lock-across-user-code ordering hazard.)
- **Tests:** serial-lane test — a subscriber that panics once does not
  stop subsequent focus/caret deliveries.

### [x] A31 — observer-rebind failures swallowed; no logging channel (P3)

- **Problem:** `install_observer_binding(pid, &config).ok()`
  (`ax_worker.rs:617`) drops the error every poll; no
  `log`/`tracing` dependency exists anywhere, so degradation is invisible.
- **Change (now):** rate-limited stderr log per failing pid using WP9's
  non-panicking writer. If a `HashMap<pid, Instant>` is used, prune entries
  older than the rate window on insertion so arbitrary short-lived pids
  cannot grow it without bound.
- **Change (decision, not code):** record in ROADMAP's seam-work section
  that a minimal logging facility is a prerequisite for the Win/Linux UI
  adapters (C.5) — decide `log` + env-filtered stderr vs a house
  micro-logger before that work starts. Do not add the dependency now.

---

## WP10 — Local-gate portability (A2, A4, A5, A20, A21, A29)

**Goal:** the documented Full Local Gate runs (or degrades loudly) on the
committed Linux deliverable and on non-FHS hosts. **Commit shape:** one
tooling commit + one docs commit (or combined).

### [x] A20 — `WATCHDOG_TICK` doc link breaks `cargo doc -D warnings` on Linux (P3)

- **Change:** fix `x11_tap.rs:34` (plain backticks, or link a public
  item); add `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
  --exclude platform_macos` to the **Linux CI lane** so the class cannot
  recur. Land the workflow step and checker re-stamp in WP10; do not defer
  it into later workflow packages.
- **Verify:** the doc command exits 0 on this host (it exits 101 today).

### [x] A21 — two BSD-only `stat` usages break self-tests on GNU/Linux (P3)

- **Change:** in `tools/bundle/make-icon.sh:190` and
  `tools/release/notarize-app.sh:139`, probe GNU first:
  `mode=$(stat -c '%a' "$f" 2>/dev/null || stat -f '%Lp' "$f")` (GNU
  `stat -f`'s dump goes to stdout — the current fallback order can never
  work there).
- **Verify:** both `--self-test`s green on Linux **and** macOS.

### [x] A2 — `check.sh --self-test` hardcodes `/usr/bin:/bin` (P3)

- **Change:** build a private `$tmp/runtime-bin` containing symlinks to
  the exact host utilities the runner/self-test needs (`env`, `bash`,
  `awk`, `find`, `xargs`, `grep`, `sed`, `diff`, `cat`, `mv`, and the
  remaining commands found by an audit of `check.sh`). Use
  `PATH="$bin_full:$runtime_bin"` / `bin_min` in all fixture runs. Do not
  append the inherited PATH: a real `shellcheck` or `cargo-audit` there
  would make the "missing tool" fixture silently test the wrong case.
- **Verify:** self-test green on NixOS (fails with exit 127 today) and
  macOS.

### [x] A29 — nested tool deps unprobed; ruby absence misdiagnosed (P4)

- **Change:** document Ruby and a Go toolchain (`go run` downloads and
  executes actionlint; no preinstalled `actionlint` binary is required)
  as Full Local Gate prerequisites in `DEVELOPMENT.md` (the cheap, honest fix — extending
  `probe_missing_tool` to parse script interiors is over-engineering);
  add a `command -v ruby` guard to
  `tools/bundle/check-bundle-metadata.sh:466-468` so a missing ruby
  reports "ruby not found (required for cask syntax check)" instead of
  "invalid Ruby syntax".
- **Verify:** on a ruby-less shell the gate now fails with the real
  reason at the right step.

### [x] A4 — live `check-model-gates.sh` is macOS-only, undocumented (P4)

- **Change:** one sentence in `DEVELOPMENT.md` (live checker requires
  macOS; `--self-test` is host-agnostic). Do not build a Linux enumeration
  mode (YAGNI; the macOS CI lane is the authority).

### [x] A5 — no MSRV recorded (P4)

- **Decision for implementation:** set workspace `rust-version = "1.97"`
  to match the repository's pinned supported toolchain. A Linux-only
  portable build on 1.95 does not prove a workspace MSRV for macOS target
  dependencies. If a lower MSRV becomes a product requirement, add
  Linux+macOS MSRV lanes and establish it deliberately; do not publish an
  unproven 1.95 promise now.
- **Verify:** metadata/manifests inherit 1.97 where intended; pinned 1.97
  full gate and CI remain green.

---

## WP11 — CI cache hardening (A15, A73, A74)

**Goal:** immunize the non-mac caches against the known poisoning class
and make current pin intent explicit.
All workflow edits — land **after WP4** so there is exactly one checker
re-stamp sequence. **Commit shape:** one commit (workflows + checker).

### [x] A73 — image-key the Linux/Windows rust-caches (P4)

- **Change:** `ci.yml:290` (windows) has no `with:` at all; `ci.yml:328`
  (linux), `release.yml:240`/`:268` are un-keyed. Add a `shell: bash`
  "read runner image" step per job that writes a safely-defaulted output
  (`${ImageOS:-${RUNNER_OS:-unknown}}-${ImageVersion:-unknown}`), then
  pass `key: ${{ steps.img.outputs.image }}` to rust-cache — both
  lanes build llama.cpp natively (`model_client` non-mac
  `llama-cpp-2 =0.1.146`) and are exposed to the exact class `da90bf5`
  fixed for macOS. Leave the already-correct macOS `key: macos-15`
  literal unchanged in this minimal diff.
- **Re-stamps:** checker step shapes.

### [x] A15 — `dtolnay/rust-toolchain` pins lack provenance comments (P5)

- **Revalidated change:** add the accurate `# stable` provenance comment to all
  9 pin sites (ci 4, release 4, audit 1). This action is pinned from its stable
  branch; inventing a `vX.Y.Z` label would misstate the upstream ref.

### [x] A74 — three acceptance harnesses lack `set -e`, undocumented (P5)

- **Change:** header comment in `tools/acceptance/e2e-complete-me.sh`,
  `run-a1b-live-gates.sh`, `run-a2-compat-gates.sh` declaring the
  deliberate `-e` omission (gate runners must accumulate results past
  failures) and the error-handling contract (`run_gate` counters /
  `fail()` / explicit `exit 2` on setup errors). Do **not** add `-e` — it
  would abort suites on the first failing gate.
- **Verify:** all three `--self-test`s still green.

---

## WP12 — CI parity & release ops (A14, A23, A24, A35, A36, A37, A49, A53, A54, A55, A56)

**Goal:** a tag validates no less than a push; small release-pipeline
gaps closed. Land **after WP11** (same files, one more checker re-stamp).
**Commit shape:** one workflows commit + one scripts/docs commit. All
formerly-open choices below now have a selected implementation.

### [x] A14 — tag `validate` runs strictly less than push CI (P4)

- **Change:** add to `release.yml`: the Linux shellcheck step
  (`ci.yml:317-320`'s shape), the rustdoc step, and the two missing
  self-tests (`check-model-gates.sh --self-test`,
  `tools/dev/check.sh --self-test`). Extend the shared step pins so the
  two lanes cannot drift apart again.

### [x] A56 — live corpus gate runs only at tag time (P4)

- **Change:** run the live `bash tools/release/check-quality.sh` after the
  branch model smoke gate. Reuse the GGUF already downloaded/cached by
  `run-model-gates.sh`; do not add a second download step. Pin the step
  and timeout in the checker.

### [x] A35 — doc tests never run for the mac-only crates (P5)

- **Change:** add `cargo test --locked --doc -p platform_macos -p app` to the macOS
  check job. Doc tests are separate harnesses and need no serial flag;
  both packages have library targets.

### [x] A55 — portable test lanes omit `--all-targets` (P5)

- **Change:** add `--all-targets` to `ci.yml:300` and
  `release.yml:247/:275` for local/CI parity; re-pin exact commands.

### [x] A36 — persisted credentials in preflight/publish; scrub tolerates failure (P5)

- **Change:** unset the extraheader immediately after the last
  authenticated git op in `preflight` and `publish_release`
  (`finalize_cask` keeps it for the push); drop the `|| true` from the
  prebuild scrub (`release.yml:348`) so a failed scrub fails loud.
  Re-pin topology. The helper self-test must distinguish an absent key from a
  lookup failure and independently reject a failed `--unset-all`.

### [x] A37 — `cancel-in-progress` also cancels main runs (P5)

- **Change:**
  `cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}` on the ci
  concurrency group.

### [x] A49 — attestation verification is repo-scoped (P4)

- **Change:** add
  `--signer-workflow mudrii/compme/.github/workflows/release.yml` to both
  `gh attestation verify` calls (`release.yml:609`, `:713`). The flag
  spelling is confirmed (`--signer-workflow`, not `--workflow` — E19).

### [x] A53 — `post_verify` checks only the self-consistent sha256 pair (P5)

- **Change:** add `gh attestation verify` of the downloaded zip in
  `post_verify` (`release.yml:745-752`), with A49's signer-workflow pin.
  **Execution evidence arrives only at the next real tag** — note that in
  the commit message; do not claim it verified before then.

### [x] A23 — failed publish leaves a stale draft that blocks rerun (P5)

- **Change:** before `gh release create --draft` (`release.yml:642-644`),
  query `gh release view "$tag" --json isDraft`. If no release exists,
  continue; if it is a draft, delete only that release with
  `gh release delete "$tag" --yes` (do not clean up the tag); if it is
  published, fail loudly. Then create the draft. Add scriptable branch
  tests/fixtures for absent, draft, published, lookup-failure, and CLI argument
  validation states.

### [x] A24 — publish/finalize burn macOS runners for gh/git work (P5)

- **Change:** move `publish_release` and `finalize_cask` to
  `ubuntu-latest` (`release.yml:577`, `:684`); keep `environment: release`
  and every guard. `post_verify` stays on macOS (genuinely needs it).

### [x] A54 — `finalize-cask.sh` hardcodes the repo slug (P5)

- **Change:** thread `"$GITHUB_REPOSITORY"` through
  `tools/release/finalize-cask.sh:48`/`:64` like the sibling scripts;
  update its `--self-test`.

**WP12 verification:** `check-model-gates.sh --self-test` after every
workflow edit; actionlint; full CI green; A53's live proof deferred to the
next tag (record in C.4's slot).

---

## WP13 — Teardown & cross-crate contracts (A32, A38, A44, A48)

**Goal:** make shutdown's safety tradeoff explicit and tested, and make
comment-only contracts checked or honest. **Commit shape:** contract/doc
commit plus a separate A32 implementation only after its fault-injection
gate selects a safe policy.

### [x] A32 — unbounded shutdown joins (P3)

- **Implemented:** `LocalModel` exposes a cloneable terminal cancellation
  handle and typed `ShutdownRequested` error. `LlamaModel` polls it between
  candidates/tokens and binds it through a lifetime-owning vendored
  `llama-cpp-2` context extension to llama.cpp's native abort callback. The
  callback state owns the cancellation atomic and a monotonic poll counter and
  is retained by `LlamaContext` until after `llama_free`, so no raw callback
  lifetime escapes the safe wrapper.
- **Bounded outer lifecycle:** `InferenceHandle` separately closes submissions
  and sets an outer stop flag, preventing the screen-wait disconnect path from
  starting queued work. The worker retains model ownership outside
  `catch_unwind`, always calls `model.shutdown()`, and acknowledges stopped only
  after teardown returns. Shutdown requires both that acknowledgement and
  `JoinHandle::is_finished()` within one 250 ms deadline before joining.
- **Selected forced policy:** a timeout is a must-use outcome. Production first
  drops tray/subscriptions/engine/adapter, then arms a last-drop hard-exit guard
  declared before startup plus a 250 ms watchdog. When watchdog spawn succeeds,
  later locals—including config key material and memory/clipboard/OCR guards—get
  ordinary Rust drops; the guard then uses `_exit(70)` or
  `TerminateProcess(..., 70)`, bypassing C `atexit` and abort/core-dump behavior.
  The watchdog covers stuck cleanup; spawn failure exits immediately.
- **Tests/evidence:** cooperative cancellation, skipped queued work, post-model-
  shutdown acknowledgement, permanently blocked native call timeout, terminal
  token/error semantics, and subprocess proofs that both the final guard and
  watchdog exit with code 70 pass. The real pinned GGUF CPU cancellation test
  waits until llama.cpp itself polls the abort callback, then returns typed
  cancellation within 250 ms. `run-model-gates.sh` repeats that exact test with
  production Metal offload on Darwin, and its self-test proves a non-Darwin run
  cannot invoke that lane. Live Metal execution remains a macOS-host evidence
  obligation rather than a Linux claim.

### [x] A44 — drop-guarantee text overstates (P4)

- **Change:** reword the `atspi_events` guarantee (and the ROADMAP echo)
  to what the code does: "nothing *new* passes the gate after stop; a
  callback already past the gate may complete" (`atspi_events.rs:178-191`,
  `:236-247`). Do not build a deliver-completion handshake (YAGNI —
  callbacks only push into Arc queues).

### [x] A38 — Insert→Hide adjacency is comment-enforced (P5)

- **Change:** add one table-driven `engine_core` sequence test asserting
  every accept commit path emits Insert/Replace/ReplaceRange immediately
  followed by Hide. Do not add state to `engine` merely for a runtime
  `debug_assert!`; the producer contract belongs at the producer and the
  consumer comment can link to the named test.

### [x] A48 — stale shutdown doc; unix-only hardening note (P5)

- **Change:** reword `model_client/src/lib.rs:551-552` (backend is
  `'static` by design, never freed — `:110-113`); add one comment in
  `memory` noting the 13 hardening sites are `#[cfg(unix)]` and a line in
  the ROADMAP Windows Phase 1 scope (WP5 already touches it) so the
  Windows adapter picks up ACL hardening deliberately.

---

## WP14 — macOS adapter posture & dead code (A19, A26, A52, A57, A64, A65, A66, A67, A71)

**Goal:** decided postures for the four known race/latency windows, and a
smaller true surface. Needs a macOS host for the serial test lane.
**Commit shape:** 2–3 commits (dead code; postures; hygiene).

### [x] A71 — unreachable synthetic-backspace path; dead observer handler (P4)

- **Change:** delete `post_synthetic_backspaces` (`lib.rs:2263-2292`), the
  `backspace_poster` seam (`:1186`), and `delete_left_via_backspaces`'
  now-dead arms (`:1368-1377`) — both call sites are preceded by
  `refuse_non_atomic_replacement` (`:1138`, `:1152`), so `replace_left > 0`
  can never reach them. Delete `accept_observer_tap_handler`
  (`:2685-2692`) and the corresponding observer install/resource; its
  production installer returns a no-op `Box::new(())` at `:3242`, so
  retaining the resource cannot observe anything. Remove the `lib_tests.rs` tests
  that exercised them.
- **Re-stamps:** test-count anchors (count decreases).

### [x] A19 — seven pub APIs kept alive by tests only; ARCHITECTURE drift (P4)

- **Change:** audit each symbol instead of applying a blanket `cfg(test)`:
  `engine_core::offer_replacement` is already test-only and needs no
  change; `compat::app_policy` backs production wrappers and should become
  private/`pub(crate)`, not disappear. Make same-module-only helpers
  (`context::word_at_caret`, `model_catalog::is_wellformed_sha256`, and
  `catalog_provenance`) private where rustdoc links permit; gate
  `stats::retained_len` only if production has no caller. Keep or narrow
  `engine::on_completion` based on its public library contract rather than
  its current call count. Fix the stale helper listing at
  `ARCHITECTURE.md:149`; add compile tests only where visibility is part of
  the intended external API.

### [x] A64 — AxSet non-atomic read-modify-write (P4)

- **Change:** add a cheap pre-write sanity check in `insert_for_field`
  (`lib.rs:4406-4460`): immediately before the set, re-read both AX value
  and selected range and require them to equal the snapshots used to
  compute `new_value`; abort with `CannotComplete` if either moved. Also document the
  accepted race at the call site. The check converts silent corruption
  into a clean retry. Note the classifier
  (`:4360-4366`) still cannot see a clobber; the check shrinks the window
  rather than closing it — say so in the comment.

### [x] A65 — disarm clears the action before the hotkey unregisters (P4)

- **Decision for implementation:** document and test the existing tiny
  fail-closed race: disarm clears the action synchronously, while queued
  resource drop may briefly leave Carbon consuming the old key; a key in
  that interval cannot insert a completion because action is `None`, but
  may be swallowed. Do not redesign `AcceptTapResource`/`Box<dyn Any>`
  into a synchronous close protocol for this bounded teardown window.
  Re-open only with live evidence that the swallowed-key window is user
  visible.

### [x] A66 — intra-app focus never rebinds the caret observer (P4)

- **Decision for implementation:** record the accepted maximum 250 ms
  same-pid focus latency. The safety poll asks the AX worker to poll and
  dispatch; it does not return focused identity to the rebind poller, so
  the earlier "trigger the same rebind" prescription had no channel to
  implement. A correct fix needs a callback-to-rebind command path and new
  ownership synchronization. Do not add it without a live failing app or
  measured user-visible delay (YAGNI).

### [x] A67 — clipboard restore on a fixed 1 s timer (P5) *(decide posture)*

- **Change:** store the existing `Arc<ClipboardRestoreCoordinator>` on the
  adapter (not only inside the insert closure), add a
  `restore_pending_if_unchanged` operation, and invoke it during adapter
  teardown. Preserve the change-count guard so user clipboard writes are
  never overwritten. Also document the remaining risk at
  `post_clipboard_text` (`lib.rs:2294-2348`) — the `changeCount` guard
  (`:2412-2429`) protects against other writers, not slow pasters — and
  add a best-effort restore on adapter teardown so a normal quit cannot
  leak the completion. Full paste-completion detection is out of scope
  (no reliable signal without an event tap).

### [x] A52 — per-keystroke AX round-trip for the domain read (P4)

- **Change:** document the deliberate cost at `run_loop.rs:4769-4776`
  now; cache-per-field-with-invalidation only if profiling ever shows it
  matters (YAGNI).

### [x] A57 — four small run_loop items (P5)

- **Change:** delete or make meaningful the tautological asserts
  (`run_loop.rs:4942-4949`, `:5087-5090`; keep `:5141-5145`); fix the
  "wall-clock" comments (`:91-92`, `loop_state.rs:129`) to "monotonic";
  gate the `Outcome::Dismissed` record (`:4996-5002`) on a visible ghost;
  update `focus.current_field` in the caret `Err` arm (`:4914-4939`).

### [x] A26 — file-wide allows in the shell façade (P5)

- **Change:** scope or drop `#![allow(unused_imports)]`
  (`shell/mod.rs:8`) and `#![allow(dead_code, unused_imports)]`
  (`shell/macos.rs:1`); fix whatever they were masking.

---

## WP15 — Small accuracy & hygiene (A13, A39, A59, A69, A70)

**Goal:** the remaining small accuracy fixes. **Commit shape:** one hygiene
commit excluding A13; A13 is a later Linux-policy commit after C.2.

### [x] A69 — `to_string_lossy` defeats `file_uri`'s byte fidelity (P5)

- **Change:** keep the portable public `file_uri(&str)` wrapper, but add an
  internal `file_uri_bytes(&[u8])` and byte-aware parent helper. On Linux,
  feed `path.as_os_str().as_bytes()` through them. Change the `xdg-open`
  path launcher to accept `&OsStr` (URL callers adapt from `&str`) so the
  containing-directory fallback also avoids `to_string_lossy`. A byte-safe
  URI helper alone is insufficient if the fallback API converts back to
  UTF-8.
- **Tests:** unit case with a non-UTF-8 `OsStr` (bytes round-trip through
  the percent-encoding).

### [x] A70 — font tie-break contradicts the search-order promise (P5)

- **Change:** carry the directory index into `find_font_file`'s tie-break
  (`overlay_font.rs:129-138`, `:167`): rank, then dir index, then path;
  update the wrong inline comment (`:133-135`). Unit test: equal-rank
  faces in `XDG_DATA_HOME=/var/...` vs `/usr/share/fonts` — user dir wins.

### [x] A59 — three one-line mismatches (P5)

- **Change:** reword `model_client/Cargo.toml:12-13` (no dynamic backends
  — `:18-23` explains why); guard or comment `stats::prune`'s
  monotonicity assumption (`stats/src/lib.rs:304-311`); use
  `fetched < page` in `memory/src/lib.rs:382`.

### [x] A39 — no schema-migration story for the encrypted store (P5)

- **Change (decision, not code):** record in `memory`'s module doc and
  ROADMAP: the SQLite schema is immutable for 0.x; any first schema change
  must land `PRAGMA user_version` + a migration helper **first**. No code
  now.

### [x] A13 — `compat` terminal policy keys on macOS bundle ids (P4, deferred)

- **Change (after C.2 produces ghosts):** map identities proven to be
  terminal processes (`gnome-terminal-server`, `konsole`, `foot`,
  `alacritty`, `kitty`, etc.) into `is_terminal`, with tests for the exact
  `/proc`/desktop-id normalization source. Do **not** classify `code`
  globally: a VS Code process identity does not prove that the focused
  field is its integrated terminal. That case remains fail-closed until
  Linux field/window metadata can distinguish terminal roles.

---

## Coverage matrix (all 69 implementation findings → work package)

| WP | Items |
|---|---|
| WP1 | A41, A50 |
| WP2 | A61 |
| WP3 | A1 |
| WP4 | A11, A17, A51 |
| WP5 | A3, A6, A12, A22, A28, A33, A34, A40 |
| WP6 | A7, A10, A47 |
| WP7 | A8, A9, A42, A43, A58, A60, A63, A68 |
| WP8 | A25, A30 |
| WP9 | A18, A31, A62 |
| WP10 | A2, A4, A5, A20, A21, A29 |
| WP11 | A15, A73, A74 |
| WP12 | A14, A23, A24, A35, A36, A37, A49, A53, A54, A55, A56 |
| WP13 | A32, A38, A44, A48 |
| WP14 | A19, A26, A52, A57, A64, A65, A66, A67, A71 |
| WP15 | A13, A39, A59, A69, A70 |

Count check: 2+1+1+3+8+3+8+2+3+6+3+11+4+9+5 = **69**. Every active ID
appears exactly once; the retired IDs are in Appendix B and must not be
re-planned.

---

## Appendix A — verification obligations that outlive the WPs

- **The 22 live macOS gates** (Qfd F3): owner/hardware-bound; unchanged by
  this plan. Ledger verified exact (22 == 22 via runner dry-run).
- **C.2 (`shown=0`)**: closed only by WP6's recorded live probe reaching
  `shown>0` (or naming the next blocker with evidence) — never by code
  inspection.
- **A53 / post_verify**: execution evidence arrives at the **next real
  tag**; follow the run through every environment approval and verify the
  published cask/checksum end-to-end before reporting success.
- **Windows Phase 1**: per WP5/A33's recorded decision. The scaffold is
  honest (`UnsupportedField` everywhere; `physical_memory_bytes`
  hardcoded 0 at `platform_windows/src/lib.rs:142-144` is the known
  RAM-fit trap to fix when the phase starts). Do not start before C.2
  produces a ghost.
- **Governance** (required reviewers, tag-creation scope, release-env
  self-approval): owner decisions, compensated meanwhile by the weekly
  read-only governance check. Revisit when a second maintainer exists.

## Appendix B — retired IDs (do not re-plan)

- **A16 · REFUTED** — "test-count anchors stale / macOS checker red":
  HEAD itself re-stamped all four anchors ("Count pins 2026 -> 2027") and
  the live GitHub check-runs for `6b490be` are all green including the
  macOS checker lane; the original arithmetic double-counted
  Linux-cfg'd live tests and used an estimated macOS count.
- **A46 · REFUTED** — "Slack webhook URLs survive redaction": the real
  redactor returned `https:[redacted-secret]` in two executable runs; the
  static pass had isolated only the trailing secret.
- **A27 → merged into A12** (delivery-log coverage). **A45 → merged into
  A30** (cap-boundary + hash-through-handle).
- **A72 → deferred policy decision** — adding cargo-deny and choosing a
  license/source allowlist is a new supply-chain policy, not remediation of
  a demonstrated defect. Re-open only with an owner-approved policy.
  *2026-08-26:* the redistribution-compliance half is closed independently —
  canonical MIT and Apache-2.0 license texts now ship in
  `vendor/llama-cpp-2/` (the vendored copy previously carried neither,
  which both licenses require of redistributed copies). The cargo-deny
  decision itself remains with the owner.
- **A75 → deferred optional tooling** — no repeated re-pin failure was
  demonstrated; a repo-rewriting helper is speculative automation under
  YAGNI. The current documented manual ritual remains authoritative.
- Sixth-pass refuted candidate (recorded so it is not re-reported):
  "local offers bypass the secure-input gate" — `read_context` samples
  secure input fresh per call (`platform_macos/src/lib.rs:1712-1716`) with
  a TOCTOU re-check in the AX worker (`:4102-4106`); fails closed.

## Appendix C — verified-accurate claims (no action; next review starts here)

26 workspace crates; `tools/spike` and the exact-source
`vendor/llama-cpp-2` patch are outside the workspace. Large-file line counts
are intentionally not treated as stable architecture facts; production-only
surface, not inline/sibling test volume, is the useful measure.
`llama-cpp-2 =0.1.146` remains pinned identically in the two model-client target
entries and spike, with both lockfiles agreeing; 22 runner-pinned live
gates exact vs ACCEPTANCE; production `unwrap`/`expect` sites re-reviewed
and justified; zero `unsafe` in `platform_linux` and in all six
security/policy crates; memory/webconfig/redaction crypto+redaction paths
re-verified line-by-line (AES-256-GCM per-record nonce + AAD + zeroize;
Ed25519 `verify_strict`; email→secret→card order); `model_fetch`
verify-before-rename atomicity and byte ceilings; every workflow `uses:`
entry SHA-pinned, with secretless prebuild, keychain
proof-of-cleanup, provenance attested and re-verified at publish and
finalize, tag-drift rechecks, and an exactly-mirrored `paths-ignore` ↔
docs.yml pair (modulo A11); `release.yml` implements RELEASING.md's flow
exactly, honestly annotating the undraft→finalize window. The pre-change HEAD
baseline was green across all four hosted workflows with no flaky-lane pattern;
the uncommitted workflow changes still require hosted CI execution. Eight version
anchors reconciled; agent-brief symlinks intact.

## Appendix D — validation evidence summary (2026-08-25 through 2026-08-26, this host)

Current focused revalidation uses staged rustup 1.97.0. The app unit lane lists
**560** tests: **558 passed** and two subprocess helpers are intentionally
ignored. Model-client unit tests pass **55/55**; the new real pinned-GGUF CPU
cancellation case observes llama.cpp polling the abort callback and returns
typed shutdown within 250 ms.
The macOS workspace-count restamp is **2,064**, derived from the checker-pinned
HEAD baseline of 2,027 plus 37 target-visible additions: 17 portable-crate, 14
app, three host-portable `platform_linux`, and three `platform_macos` tests. The
Linux-only additions are deliberately excluded from that macOS count.
(Superseded 2026-08-26 by the findings-fix round in `FIXED.md`: one portable
`memory` schema-snapshot test takes the macOS restamp to **2,065**, and the
Linux package lane to **112** tests with 36 ignored, **36/36** live.)
At that round's HEAD the Linux package lane passed **107** tests with 35
ignored, and all **35/35** ignored live AT-SPI/X11/session-service cases passed
in the provisioned Xvfb harness, including the A42 plan-construction failure
regression — historical snapshot; the superseding current figures are the
112/36 and 36/36 above. The app's
isolated `config_startup` executable test also passes from the required
RPATH-linked target while preserving its intentional clean environment.

Formatting, strict app/model clippy/check, macOS adapter cross-check/clippy, the
root real-model suite with its hosted-runner latency opt-out, the exact A32 CPU
cancellation gate, quality corpus, `cargo audit` (one allowed RUSTSEC-2026-0192
warning), shellcheck/actionlint/`bash -n`, and release-policy/helper self-tests
are the aggregate gate set. Revalidation also closed two prior-claim gaps: A41
no longer
maps unavailable AT-SPI to relaunch-required status, and A65's generation-
guarded hide path clears its action before dropping the hotkey resource. The
attestation checker now rejects moving verification after cask mutation/install.
The strict 500 ms warm-model latency gate failed honestly at 1,871 ms on this
CPU-only host; only the required real-Mac pre-tag run can close that evidence.

Not runnable from this Linux host: execution of `platform_macos` tests, the real
Swift icon check, the 22 macOS live/LOOK gates, real-Mac Metal cancellation, the
Windows hardware lane, live governance, and next-tag `post_verify`. The literal
full local fence still compiles Apple frameworks and therefore remains a macOS
execution obligation; none of those results is synthesized here.
