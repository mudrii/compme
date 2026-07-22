# Quality gates + release last-mile implementation plan (Batches 3 & 4)

**Date:** 2026-07-18 · **Status:** delivered in the working tree (pending commit) — all six items (3.1–3.4, 4.1, 4.2) landed on 2026-07-18; the adversarial-audit remediation followed
**Prereqs:** clean `main` (builds, clippy clean, ≈1920 tests green); the
Batch 1+2 hardening diff (CI/docs/cask-window/dependabot/pre-push hook,
`protect-main` ruleset) committed or applied.
**Supersedes:** nothing — details the remaining items from the 2026-07-18
full-project analysis. ROADMAP stays the status ledger; this doc is the
execution guide for the six remaining hardening work items.

Evidence base: five-agent full-project analysis 2026-07-18 (plan/code/docs/
tests/CI sweeps), every claim grep-verified on the `0550f0f` tree plus the
Batch 1+2 working-tree changes. Anchors below cite that tree; line numbers
drift — re-locate by symbol before editing.

**Release boundary:** v0.1.5 (`14ae81e`) is the latest published artifact.
Nothing in this plan changes runtime product behavior; items 3.1, 3.2, 4.1,
4.2 touch CI/release tooling and tests only, and 3.3/3.4 touch test code and
one mechanical extraction in `crates/app`.

## Scope

| # | Work item | Class | Estimate |
|---|-----------|-------|----------|
| 3.1 | Model-quality regression gate | release tooling + corpus | 0.5–1 day |
| 3.2 | Hermetic inference smoke gate on branch CI | CI | 1–2 h |
| 3.3 | AX insertion-serialization tests | tests | 2–4 h |
| 3.4 | `run()` startup extraction | mechanical refactor + tests | 0.5–1 day |
| 4.1 | Post-publish verification job | release tooling | 0.5 day |
| 4.2 | Version-docs reconciliation check | release tooling | 1–2 h |

Out of scope (deliberately deferred; triggers recorded in the analysis): the
`platform_macos/src/lib.rs` split, the full `run()` → `RunState` breakup beyond
item 3.4, test-suite de-serialization, llvm-cov in CI, proptest, the 35-step
macOS job split, CONTRIBUTING/templates, and the six live-governance caveats
(owner decisions). Do not pull these into the batches.

## Cross-cutting rules (read before any item)

These are enforced, not advisory — the repo pins its own infrastructure:

1. **`tools/release/check-model-gates.sh` pins reality.** It pins exact CI /
   release / audit job topology, action identities + input sets, step
   name/command pairs, script content lines, doc gate lines
   (`require_development_gate_line`, `require_readme_gate_line` pointers,
   grammar-spec validation lines), and the env-cleanup contract of every
   self-test script (`check_self_test_env_file`). **Any** new or changed CI
   step, release script, or documented gate command requires a pin update in
   this file, and usually a self-test fixture. Run `bash
   tools/release/check-model-gates.sh` (live) and `--self-test` before and
   after every such change; both must exit 0.
2. **Every new bash script follows the house contract:** `#!/usr/bin/env
   bash`, `set -euo pipefail`, a hermetic `--self-test` mode (fixtures in
   `mktemp -d`, `trap` cleanup, inherited-env poisoning guards), wired into
   the ci.yml self-test battery and the `docs/DEVELOPMENT.md` Full Local Gate
   list (which is pinned — add the matching `require_development_gate_line`).
   Scripts must pass the CI `bash -n` traversal and `shellcheck
   --severity=error` (both are gates since Batch 2).
3. **Actions come only from the approved full-length-SHA list** in
   `approved_action_ref?` (checkout, dtolnay/rust-toolchain,
   Swatinem/rust-cache, upload/download-artifact, attest-build-provenance).
   New actions or new input sets need pin updates (`validate_action_inputs!`).
   Validate workflow edits locally with `go run
   github.com/rhysd/actionlint/cmd/actionlint@v1.7.12 -oneline
   .github/workflows/*.yml` — zero findings required.
4. **Toolchain version is single-sourced** in `rust-toolchain.toml`; never add
   a literal `toolchain:` input (the checker rejects it).
5. **Deterministic gates must stay green:** `cargo fmt --all -- --check`,
   `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo
   test --locked --workspace --all-targets -- --test-threads=1`, `cargo build
   --locked --workspace --all-targets`, `RUSTDOCFLAGS="-D warnings" cargo doc
   --no-deps --workspace`, `cargo audit`.
6. **Docs move with behavior.** README/DEVELOPMENT/ACCEPTANCE/RELEASING
   sections describing a changed gate must be updated in the same commit
   (several lines are pinned; the checker will name the stale one). AGENTS.md:
   update `docs/ROADMAP.md` when an item ships; add a `# Lessons` line if
   corrected.
7. **Work on `main`, one work item per commit series.** Items 3.1/3.2/4.1/4.2
   all touch `check-model-gates.sh` pins — each series carries its own pin +
   fixture updates; never batch pin updates across items.

---

## 3.1 — Model-quality regression gate

**Finding:** completion quality is the product's entire value, yet prompt and
model-catalog changes (e.g. `5126509`, `4c2f8d3`) shipped with no committed
eval corpus; the only quality evidence is a one-off "7/8 typo fixes, 0/4 false
fixes" GGUF probe (`docs/ROADMAP.md:503`). No gate fires on quality drift.

**Objective:** a deterministic corpus-based probe that fails the release
pipeline when the pinned model + current prompt stack drops below a fixed
pass threshold.

### Design

- **Corpus:** `tools/release/quality-corpus.jsonl` — one JSON object per line:
  `{"id", "path": "completion"|"grammar", "left", "right"?, "word"?,
  "expect": {"type": "contains"|"regex"|"not_contains"|"max_words"|
  "single_word_vetted", "value"?}, "note"?}`. Seed with ~20 cases:
  - the existing typo-fix probe set (the 8 typo cases + 4 false-fix controls
    from the `7/8` probe),
  - grammar-path vetting cases (case preservation `Cat→Dog`/`CAT→DOG`,
    multi-word rejection, non-ASCII rejection, edit-distance bound — these
    exercise `grammar::vet_correction` through the real model, the
    highest-value quality surface),
  - degeneration controls (no `the the the` loops, stop at sentence boundary,
    no suffix regurgitation of `right` text).
- **Harness:** new ignored test target `crates/model_client/tests/quality.rs`,
  following `tests/latency.rs` conventions: skips cleanly without a local GGUF,
  activates on `COMPME_REQUIRE_MODEL_TESTS=1`, reads the corpus path from
  `COMPME_QUALITY_CORPUS` (defaulting to the in-repo file), loads the model
  once, runs every case, reports per-case results, and fails below the pass
  threshold. Threshold: **≥80%** (16/20) — catches catastrophic drift, does
  not grade nuance.
- **Driver:** `tools/release/check-quality.sh` (house contract, rule 2). It
  mirrors `run-model-gates.sh`'s model acquisition: the same pinned triplet
  (`default_model="tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf"`,
  `default_url="https://huggingface.co/Brianpuz/Qwen2.5-0.5B-Q4_K_M-GGUF/resolve/2188f0ce52503bd130dee9abf56f36f610784c0e/qwen2.5-0.5b-q4_k_m.gguf"`,
  `default_expected="ca6f8885c1d6a14025e705295fe1b240ad5a30c4c696215a341d7e6610a26484"`),
  CPU-forced (`COMPME_MODEL_GPU_LAYERS=0`), bounded context
  (`COMPME_MODEL_CONTEXT_TOKENS=256`), then runs `cargo test --locked -p
  model_client --test quality -- --ignored --test-threads=1`.
- **Determinism:** verify the `model_client` sampler exposes a fixed seed or
  temperature 0 (sampler configuration exists — see the divergence-contract
  tests in `crates/model_client/src/lib.rs` ~:858-901). Use it. Prefer
  relative assertions (`contains`/`regex`/`not_contains`) over exact-match so
  minor backend variation does not flake; the threshold absorbs the rest.
- **Where it runs:** (a) release.yml `validate` job, new step after
  "Model-backed release gates", reusing the already-downloaded model via
  `COMPME_MODEL_GATE_PATH`; (b) documented pre-tag local command in
  `docs/RELEASING.md`. **Not** in per-push CI — zero day-to-day cost.

### Steps

1. Verify-first: read `run-model-gates.sh` download logic — confirm it skips
   re-download when the destination exists with matching sha256 (if not, add
   that guard; item 3.2 depends on it too). Confirm the sampler seed/temp-0
   knob in `model_client`. Record both findings in the commit message.
2. Add `tools/release/quality-corpus.jsonl` (~20 cases above).
3. Add `crates/model_client/tests/quality.rs` (skip-without-model,
   env-activated, threshold assert). Run locally with the dev GGUF: `cargo
   test --locked -p model_client --test quality -- --ignored
   --test-threads=1` — iterate the corpus until the pinned model passes
   comfortably above threshold (record the baseline rate).
4. Add `tools/release/check-quality.sh` + hermetic `--self-test` (fake corpus
   + fake `cargo` on `PATH` in a temp dir: threshold pass/fail, malformed
   corpus line, missing model behavior, inherited-env poisoning guard).
5. Wire the release.yml `validate` step; wire the script self-test into the
   ci.yml battery.
6. Pin updates (rule 1): release validate step pin; `require_line` pins for
   the script's model triplet (mirroring the `gate_script` triplet pins);
   `check_self_test_env_file` contract; `require_development_gate_line` +
   DEVELOPMENT.md gate-list line; RELEASING.md pre-tag command (check for
   newly-stale pinned lines after editing).
7. Docs: RELEASING.md (pre-tag quality gate), DEVELOPMENT.md (gate list).
8. Full cross-cutting gate (rule 5) + `check-model-gates.sh` live/self-test.

### Risks

- **Small-model nondeterminism** across backends → CPU-forced runs, relative
  assertions, 80% threshold. If a case proves unstable on the hosted runner,
  tighten the assertion or drop the case, never lower the threshold below
  80%.
- **Corpus rot** (a case becomes wrong as prompts legitimately improve) → the
  threshold tolerates 4 failures; a failing case demands an explicit
  investigate-and-update commit, not a silent edit.
- Scope creep into a full eval framework → resist; 20 cases, one threshold.

**Acceptance:** release `validate` fails when the corpus pass rate < 80% and
passes at the recorded baseline; `check-quality.sh --self-test` passes; the
checker is green; the corpus + threshold are documented in RELEASING.md.

---

## 3.2 — Hermetic inference smoke gate on branch CI

**Finding:** the llama load→complete→shutdown path runs only in 5 `#[ignore]`d
tests exercised at release time; a llama-worker breakage merged mid-cycle
ships green (`crates/model_client/tests/latency.rs:101,141,191,239,282`).

**Objective:** run the existing, already-hardened release model gates on
branch CI so real-inference breakage is caught per push.

### Design

Do **not** build a new path — reuse `run-model-gates.sh` exactly as the
release `validate` job does (`COMPME_REQUIRE_LATENCY_BUDGET=0`, hosted-runner
convention). Two changes make it cheap:

1. **Cache the model.** Add the model directory to the check job's existing
   `Swatinem/rust-cache` via `cache-directories` (the mechanism already used
   for `~/.cargo/advisory-db` on the Linux lane). Model path:
   `tools/spike/models` (the script's `default_model` location). First run
   downloads ~0.4 GB; subsequent runs hit the cache. Depends on the
   skip-on-sha-match guard verified in 3.1 step 1.
2. **Add the step** to the ci.yml `check` job after "Build": "Model-backed
   smoke gate" — `COMPME_REQUIRE_LATENCY_BUDGET=0 bash
   tools/release/run-model-gates.sh` (the latency budget is meaningless on a
   virtualized runner; functional load/complete/shutdown coverage is the
   point).

### Steps

1. ci.yml `check` job: extend the rust-cache `with:` block with
   `cache-directories: tools/spike/models` (keep existing keys).
2. ci.yml `check` job: add the smoke-gate step with the env var, mirroring
   the release step's form.
3. Pin updates (rule 1): the new rust-cache input set must be added to the
   approved `Swatinem/rust-cache` input list in `validate_action_inputs!`
   (currently `{}`, `{"workspaces" => "tools/spike"}`,
   `{"cache-directories" => "~/.cargo/advisory-db"}`, and the release validate
   combined set — add the new combined set verbatim as YAML parses it); new
   step pin in the CI check table plus an env assertion mirroring the release
   `COMPME_REQUIRE_LATENCY_BUDGET` pin (~line 3132-3133 pattern).
4. Watch the first two CI runs: confirm the second run restores the model
   from cache (log line) and the step adds < 5 min.
5. Docs: DEVELOPMENT.md CI paragraph (branch CI now exercises real
   inference); check for stale pinned lines.

### Risks

- **Wall-time growth on the 90-min budget** → the model is cached and the
  0.5B functional suite is fast; measure on the first run. If it threatens
  the budget, gate the step to `main`-branch pushes only (still pre-release,
  still per-day) rather than dropping it.
- **HF availability at test time** → the model URL is a third-party
  dependency (already accepted at release time); a download failure fails
  the step loudly, which is correct.

**Acceptance:** branch CI runs real load→complete→shutdown per push with a
cached model; a deliberately broken `model_client` (test on a scratch branch
or local revert) fails the step; checker green.

---

## 3.3 — AX insertion-serialization tests

**Finding:** the parity-critical write path — `insert_replacing` /
`insert_replacing_range` against a live AX element — is exercised only by
manual acceptance scripts, while classifiers/parsers are exhaustively tested
(`crates/platform_macos/src/lib.rs`; fabricated-refcon observer tests at
~:14101, :14157).

**Objective:** assert the exact `AXUIElementSetAttributeValue` call sequence
for range replacement against the crate's existing fake-element seams.

### Design

- **Step 1 is inventory:** read the test-injection seams —
  `#[cfg_attr(not(test), allow(dead_code))]` `Custom` installer variants
  (~:711, :717), `is_self_generated_event` (~:2828), and the fake-element
  pattern used by the observer tests. Determine how AX attribute-set calls
  can be captured in tests (recording fake element / injected setter).
- Add a recording fake AX element that captures (attribute, value) pairs in
  order, then table-drive these cases through `insert_replacing_range`:
  1. **Happy path:** exact sequence — value set with the replacement applied,
     then selected-range set to the post-insert caret; assert order and
     payloads byte-for-byte (Unicode content included — the crate's offset
     discipline is scalar-based).
  2. **Mid-field range** replacement (non-suffix caret).
  3. **Stale focus:** frontmost PID moved away from the field's PID before
     insertion → **no** AX calls, error surfaced (global-strategy rejection
     invariant).
  4. **Unsupported strategy:** capability without atomic range replace → no
     AX calls, `UnsupportedField` class error.
- Keep the tests serial-safe (suite runs `--test-threads=1` due to
  pasteboard/global state — no new shared globals).

### Steps

1. Inventory the seams; pick the capture mechanism that matches the existing
   pattern (do not introduce a new mocking style).
2. Write the four cases in the in-src `mod tests` of
   `crates/platform_macos/src/lib.rs` (the crate's convention — no new
   `tests/` target).
3. `cargo test --locked -p platform_macos insert -- --test-threads=1` green;
   then the full crate suite serial.
4. No checker pins (pure test code) — but run the full cross-cutting gate.

### Risks

- **Brittleness to refactors** — asserting exact sequences couples tests to
  implementation order; acceptable here because the sequence *is* the
  contract (wrong order = corrupted field). Comment each assertion with the
  invariant it protects.

**Acceptance:** the four cases pass; a manual mutation (swap the two AX
calls, or drop the stale-focus check) is caught by the tests.

---

## 3.4 — `run()` startup extraction

**Finding:** `pub fn run()` (`crates/app/src/run_loop.rs:3884`, 2,334 lines)
is the only untestable seam — the shell is bound at compile time
(`crate::shell::make_shell()`, ~:3901, cfg-selected in `shell/mod.rs`) — and
live bug c92 was exactly this class of startup-ordering bug. Wiring
regressions here compile green and pass ~1,900 tests.

**Objective:** extract the startup sequence (instance lock → config → signal
handlers → permission prompt → adapter/overlay/engine construction →
inference spawn) into an injectable, tested function. **The heartbeat loop
stays untouched** — the full `RunState` refactor is explicitly deferred.

### Design

- **Enable the seam on macOS test builds:** the stub shell
  (`crates/app/src/shell/stub.rs`, module-level `#![allow(dead_code)]`) is
  cfg'd out on macOS (`shell/mod.rs`). Widen it to `cfg(any(test,
  not(target_os = "macos")))` so tests on the dev Mac can inject it. This is
  the enabling change; it compiles the stub's warnings away via its existing
  allow.
- **Extract:** `fn startup(factories: &RunFactories) -> Result<RunContext,
  StartupError>` in `run_loop.rs`, where `RunFactories` carries the
  shell/adapter/overlay/tray constructors (production passes the real ones;
  tests pass recording fakes) and `RunContext` owns everything the heartbeat
  loop needs. `run()` becomes: `let ctx = startup(&real_factories())?;`
  then the existing loop, verbatim. Move code, do not rewrite it — behavior
  identical by construction.
- **Tests** (in the file's existing `mod tests`, its convention): a
  call-log fake records factory invocations and asserts —
  1. **Order:** instance lock before config before signals before
     permissions before adapter/engine construction (the c92 class).
  2. **Fail-closed config:** unreadable existing config aborts before any
     platform construction (complements the binary-level
     `app/tests/config_startup.rs`).
  3. **Degraded subscription:** the "requires relaunch" flow surfaces and
     stops startup cleanly.
  4. **Instance-lock collision:** second startup fails without touching the
     adapter (OS-level coverage already exists in bundle-smoke's
     `COMPME_ACCEPTANCE_PID=444` test; this is the fast unit version).
  5. **Permission denied:** AX-permission-negative path does not construct
     the engine.

### Steps

1. Widen the stub-shell cfg; `cargo check --workspace` + `cargo test -p app`
   green (no behavior change yet).
2. Introduce `RunFactories`/`RunContext` types and `startup()`; move the
   startup block verbatim; `run()` delegates. Gates green.
3. Add the five tests. Gates green.
4. Verify no regression at the product level: `tools/acceptance/
   e2e-complete-me.sh --self-test`, `tools/bundle/bundle-smoke.sh`, and a
   manual smoke (`COMPME_STUB_COMPLETION` bounded run) on the dev Mac.
5. No checker pins (no CI/script/doc-gate change) unless a doc mentions the
   structure — ARCHITECTURE.md's `app` section describes the run loop;
   update its module description if the extraction changes what it says.

### Risks

- **The extraction itself introduces an ordering change** → mechanical-move
  discipline, per-step gates, and the smoke battery in step 4. If anything
  smells, revert the series — do not debug forward.
- **Factory-type plumbing fights the concrete macOS types** → keep factories
  as plain `fn() -> …` closures returning the existing concrete/trait types;
  do not generalize beyond what the five tests need.

**Acceptance:** `run()` delegates to `startup()`; the five ordering/failure
tests pass; the full serial suite + smoke battery is green; the heartbeat
loop's diff is empty.

---

## 4.1 — Post-publish verification job

**Finding:** the pipeline's last mile — the published cask actually installs
and Gatekeeper passes on a clean machine — is a manual checklist item
(`docs/RELEASING.md` post-publish section); AGENTS.md lesson #1 exists
because this tail burned a release.

**Objective:** the pipeline verifies itself end-to-end after
`finalize_cask`.

### Design

New `post_verify` job in release.yml:

```yaml
post_verify:
  name: Post-publish install verification
  needs: finalize_cask
  runs-on: macos-14
  timeout-minutes: 30
  permissions:
    contents: read
  steps:
    - name: Download published assets and verify checksum
      env:
        GH_TOKEN: ${{ github.token }}
      run: |
        set -euo pipefail
        gh release download "$GITHUB_REF_NAME" --pattern "*.zip" --dir verify
        gh release download "$GITHUB_REF_NAME" --pattern "*.zip.sha256" --dir verify
        (cd verify && shasum -a 256 -c *.sha256)
    - name: Install the published cask
      run: |
        set -euo pipefail
        brew update
        brew install --cask compme
    - name: Assess installed app
      run: |
        set -euo pipefail
        codesign --verify --deep --strict /Applications/Compme.app
        xcrun stapler validate /Applications/Compme.app
        spctl --assess --type execute -vv /Applications/Compme.app
    - name: Bounded startup smoke
      run: |
        set -euo pipefail
        tmp="$(mktemp -d)"
        COMPME_CONFIG="$tmp/config.env" COMPME_RUN_MS=5000 \
          COMPME_STUB_COMPLETION=" smoke" \
          /Applications/Compme.app/Contents/MacOS/compme
```

Design notes:

- **No `environment: release`, no secrets beyond `github.token`** — the job
  verifies *published* state; pin `environment` absent and no secret
  references (mirror the prebuild pins).
- The cask install verifies the tap state end-to-end: `brew` downloads the
  release zip and checks it against the cask's sha256 — the exact user
  experience that Batch 2's cask-window fix protects.
- The startup smoke mirrors `bundle-smoke.sh`'s isolation (`COMPME_CONFIG`,
  `COMPME_RUN_MS`, `COMPME_STUB_COMPLETION`); it asserts bounded clean
  startup only — no AX-dependent behavior (runner has no grant).
- Adjust paths/patterns to the real bundle layout when implementing (verify
  the app's binary name and the cask's install target on the first run).

### Steps

1. Add the job; actionlint clean.
2. Pin updates (rule 1): `expected_action_topology` + `expected_timeouts`
   entries; needs-chain pins (`post_verify` depends only on
   `finalize_cask`); add the job to the `tag_job_guard` list pattern
   (~:3088-3092) if the pattern is kept; `contents: read` permissions pin;
   environment-absent + no-secret-reference pins; self-test fixture for the
   new topology.
3. RELEASING.md: shrink the manual post-publish checklist to "confirm
   `post_verify` green; spot-check the tracking issue log" (watch for pinned
   lines).
4. Validate on the next real release (or a pre-release dry run against the
   current published v0.1.5: the job's logic minus `needs:` can be exercised
   manually first).

### Risks

- **brew/network flake at the finish line** → the job is last and mutates
  nothing; a failure blocks nothing downstream, so a rerun is safe. Keep it
  required-green for calling the release done (RELEASING.md wording).

**Acceptance:** on a real release, `post_verify` installs the published cask
and passes signature/notarization/Gatekeeper assessment plus the bounded
smoke; a tampered cask (simulated in a dry run) fails the job.

---

## 4.2 — Version-docs reconciliation check

**Finding:** AGENTS.md lesson #2 — docs lagging the published version — is a
recurring manual catch (README status, SECURITY supported release, ROADMAP
anchors, release-boundary notes).

**Objective:** a grep-level gate that fails when the documented version lags
`Cargo.toml`.

### Design

- New `tools/release/check-version-docs.sh` (house contract, rule 2): read
  `version` from the root `Cargo.toml` `[workspace.package]` (single-sourced
  since `1a12b50`), then require it in:
  - `README.md` status line,
  - `SECURITY.md` supported-release table,
  - `docs/ROADMAP.md` header,
  - `docs/RELEASING.md` release-boundary note.
  Failure output names each stale file. (`Casks/compme.rb` and
  `tools/bundle/Info.plist` are already covered by
  `check-bundle-metadata.sh` — do not duplicate.)
- `--self-test`: fixture docs in a temp dir — all-current passes, one-stale
  fails naming the file, inherited-env poisoning guard.
- Wire into: ci.yml `check` job (catches doc lag at the release-prep commit,
  not at tag time) and release.yml `validate`; self-test into the ci.yml
  battery.

### Steps

1. Write the script + self-test; run both.
2. Wire the two workflow steps; actionlint.
3. Pin updates (rule 1): step pins for both workflows, script
   `require_line` pins, `check_self_test_env_file` contract,
   `require_development_gate_line` + DEVELOPMENT.md gate-list line.
4. Full cross-cutting gate.

**Acceptance:** a commit bumping `Cargo.toml` without touching one documented
surface fails the new step naming that file; checker green.

---

## Sequencing

```text
Phase A (independent, low risk — start here)
  3.2 CI smoke gate ─────────────┐
  3.3 AX insertion tests ────────┤ (any order, separate commit series)
Phase B                          ▼
  3.1 quality gate ──────── before next release; depends on 3.2's
                            verified download-cache behavior (shares
                            the model-pin triplet and the skip guard)
Phase C (batch together — one release.yml + checker-pin session)
  4.2 version-docs check ──┐
  4.1 post-verify job ─────┴── after B, before the release that
                               will exercise them
Phase D (own series, no release imminent)
  3.4 run() startup extraction ── independent of A–C; schedule for a
                                  quiet stretch, revert-friendly
```

Dependencies: 3.1 reuses 3.2's verified cache/skip behavior; 4.1/4.2 share
one release.yml + pin-update session; 3.3/3.4 touch no pins and can land any
time. Nothing here blocks the 22 manual live gates or cross-platform Phase 1
— those remain the product critical path on `docs/ROADMAP.md`.

## Global acceptance

Every item lands with: its own acceptance criteria met; the cross-cutting
gate (rule 5) green; `bash tools/release/check-model-gates.sh` and
`--self-test` green; actionlint clean on touched workflows; affected script
self-tests green; docs moved with behavior (rule 6); and a ROADMAP.md status
line per AGENTS.md. When all six items are done, re-run the analysis section
of the 2026-07-18 findings and confirm: quality has a gate, mid-cycle llama
breakage cannot ship green, the AX write path has a deterministic test, the
coordinator's startup is unit-tested, and the release pipeline verifies
itself end-to-end.
