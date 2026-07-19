# Audit-findings remediation plan (2026-07-19 full audit)

**Date:** 2026-07-19 · **Status:** pending — no implementation started
**Evidence base:** three-round audit 2026-07-19 (five finder agents →
adversarial verify → fresh independent review → final independent
re-derivation). All ten consolidated findings CONFIRMED; live execution
evidence: full deterministic gate battery exit 0 on the working tree, and
`tools/release/check-quality.sh` passed against the real pinned GGUF (10.6s).
**Prereq:** the uncommitted Batch 3+4 delta (2026-07-18 plan, all six items
implemented and gate-green) — Phase 0 below commits it first; every item here
builds on those pins and scripts.
**Supersedes:** nothing. The 2026-07-18 plan's deliberate deferrals
(platform_macos split trigger, full `RunState` breakup, llvm-cov, proptest,
CONTRIBUTING) stay deferred unless named below.

Anchors cite the 2026-07-19 working tree; line numbers drift — re-locate by
symbol before editing. Cross-cutting rules 1–7 of
[`2026-07-18-quality-and-release-gates.md`](2026-07-18-quality-and-release-gates.md)
apply verbatim to every item (checker pins, house script contract, approved
action SHAs, single-sourced toolchain, deterministic gates, docs move with
behavior, one work item per commit series).

## Scope

| # | Work item | Finding | Class | Estimate |
|---|-----------|---------|-------|----------|
| 0 | Commit the Batch 3+4 delta | audit top action | commit series | 1–2 h |
| 1 | Version-docs checker: gate DEVELOPMENT/ACCEPTANCE boundaries | C1 | release tooling | 1–2 h |
| 2 | Governance check: guard endpoint reads + settle teeth-vs-warn | C2 | CI + owner decision | 1–2 h + decision |
| 3 | Dependabot ↔ SHA-allowlist reconciliation procedure | C3 | docs/process | 1 h |
| 4 | Test-suite parallelization split | C4 | tests + CI | 0.5–1 day |
| 5 | Quality-gate polish (comment, corpus contingency, probe cross-ref) | C5, C9 | tests/docs | 1 h |
| 6 | post_verify smoke fail-fast timeout | C6 | release tooling | 1 h |
| 7 | Docs-only pushes: run the docs gates | C7 | CI | 1–2 h |
| 8 | God-file extractions (LoopState, platform_macos carve, config movers) | C8 | refactor series | 3–5 days, phased |
| 9 | Docs hygiene batch (ROADMAP header, wording, links, gate wrapper) | docs audit | docs | 0.5 day |
| 10 | Watch items (rust-cache eviction, post_verify first live run) | Q1, C6 | monitoring | 0 |

Out of scope (unchanged owner-gated set): branch protection, release-tag
creation restriction, environment self-review, Actions allowlist enforcement
on GitHub's side — item 2 records the one decision this plan needs.

---

## 0 — Commit the Batch 3+4 delta (prerequisite)

The working tree holds the fully implemented, triple-verified 2026-07-18 plan
(quality gate, CI smoke gate, AX insertion tests, `startup()` extraction,
`post_verify`, version-docs check) plus dependabot/TROUBLESHOOTING/tools-dev.
Nothing below lands cleanly before it.

Steps: split into the plan's own commit-series boundaries (rule 7): one
series per work item where separable; the shared release.yml/pin session
(4.1+4.2) may land as one commit. Before the first push: full deterministic
gate + `check-model-gates.sh` live and `--self-test` (already verified green
on this exact tree). Refresh the ≈1937 test-count prose (README:357,
DEVELOPMENT:176,249, ROADMAP:3) to the post-commit `cargo test --list` count
in the same series (the count gate names the figure).

Acceptance: clean `git status` (tracked files), CI green on `main`, the first
post-commit CI run downloads the model once and the second restores it from
cache (validates the rotated rust-cache key captured `tools/spike/models`).

## 1 — Gate the remaining version-boundary docs (C1)

**Finding:** `tools/release/check-version-docs.sh:143-146` gates
README/SECURITY/ROADMAP/RELEASING only; `docs/DEVELOPMENT.md:27,30,33` and
`docs/ACCEPTANCE.md:7` hard-code `v0.1.5`/`14ae81e` ungated (confirmed: no
check-model-gates.sh pin covers the boundary text either).

**Design:** extend `require_doc_version` coverage with two entries:
- `docs/DEVELOPMENT.md` — anchor `points to` line (version + commit form).
- `docs/ACCEPTANCE.md` — anchor the release-boundary blockquote line.
Both docs state version AND commit; check the version token (the commit hash
changes per release and is reconciled by the same edit — do not double-gate).
Extend `--self-test` fixtures: all-current passes; stale DEVELOPMENT fails
naming the file; stale ACCEPTANCE fails naming the file.

**Steps:** script edit → self-test fixtures → run live + self-test →
pin updates (rule 1: the script's `require_line` pins in
check-model-gates.sh cover its doc list — add the two new lines) → gates.

**Acceptance:** editing `docs/DEVELOPMENT.md`'s boundary to `v0.1.4` makes
the live check fail naming DEVELOPMENT.md; same for ACCEPTANCE.md; checker
and battery green.

## 2 — Governance check: guard reads, settle warn-vs-fail (C2)

**Finding (both halves confirmed):** (a) the weekly `governance` job's
hard-fail arm (audit.yml:102) is unreachable — `GITHUB_TOKEN` cannot hold
Administration, the `actions/permissions` probe 403s, and the workflow
downgrades to `::warning` + exit 0; the job can never catch a regression.
(b) `check-github-governance.sh:298-300` reads `environments`, and
`rulesets` unguarded under `set -euo pipefail` — a split-permission token
would abort the script and the workflow would misfile a false-positive
tracking issue.

**Design:**
- **Mechanical fix (do now):** wrap the three `gh api` reads in the same
  guarded pattern the branch-protection read already uses (capture stderr,
  distinguish permission-refused from real failure); a refused read joins the
  `admin_readable=0` path instead of aborting. Extend `--self-test` with a
  refused-environments fixture.
- **Decision (owner):** pick one, record in ROADMAP §governance:
  1. *Give it teeth (recommended):* fine-grained PAT, this repo only,
     read-only Administration, stored as `GOVERNANCE_READ_TOKEN`; job uses it
     when present, falls back to warn-only without it. The hard-fail arm
     becomes reachable.
  2. *Accept warn-only:* delete the unreachable hard-fail arm and the
     tracking-issue step's regression framing; re-document the job as a
     weekly reminder. Honest, zero-secret.
  Do not leave the current state: a check that reads as enforcement but can
  only warn is false assurance.

**Steps:** script guards + fixtures → workflow edit per decision →
actionlint → pins (audit.yml step/permissions pins; a new secret reference
needs its own pin treatment) → docs (RELEASING governance note).

**Acceptance:** self-test covers the refused-endpoint path; with option 1 a
deliberately mis-set fixture fails the job in a dry run; with option 2 the
workflow no longer contains an unreachable arm.

## 3 — Dependabot ↔ SHA-allowlist procedure (C3)

**Finding:** weekly grouped `github-actions` bumps rewrite SHAs that
`approved_action_ref?` and ~33 fixture occurrences hard-code; every bump PR
fails `check-model-gates.sh` until hand-reconciled; nothing documents this.

**Design:** keep the allowlist (it is the stronger control; dependabot PRs
failing closed is correct). Two changes:
- **Document the reconciliation:** DEVELOPMENT.md (or RELEASING.md
  maintenance section): "action bump = update `approved_action_ref?` + every
  fixture occurrence + the version comment; grep the old SHA to zero; run
  checker live + self-test." Add the matching gate-line pin if the wording
  lands in a pinned section.
- **Reduce noise:** scope the dependabot `github-actions` ecosystem to
  monthly (security advisories still arrive immediately) — weekly grouped
  bumps of pinned actions are churn the checker will reject anyway.

**Steps:** dependabot.yml interval edit → doc paragraph → pins if pinned
section touched → gates.

**Acceptance:** the procedure is greppable in docs; dependabot config shows
monthly for actions, weekly retained for cargo.

## 4 — Test-suite parallelization split (C4)

**Finding:** workspace-wide `--test-threads=1` (ci.yml:80, release.yml:94)
is required only by: `platform_macos` (real AX/pasteboard/keychain/
main-thread) and `app` (`SHORTCUT_BINDINGS` process-global at
`crates/app/src/shell/stub.rs:63`, mutated via `ShortcutBindingsGuard`
(run_loop.rs:6375-6389) which save/restores without holding a lock across
the test body). The remaining ~22 crates are verified hazard-free (no
env mutation, pid/hex-keyed temp dirs, ephemeral ports, init-once
OnceLocks).

**Design (two independent halves):**
- **4a — de-hazard `app`:** give `ShortcutBindingsGuard` a
  `static GUARD_LOCK: Mutex<()>` held for the guard's lifetime (acquire in
  `reset()`, release on Drop; use `PoisonError::into_inner`). Tests using the
  guard then serialize among themselves only. Add a two-thread regression
  test that fails without the lock (spawn both, assert no cross-clobber).
- **4b — split the CI invocation:** replace the single serial workspace run
  with: `cargo test --locked --workspace --exclude platform_macos
  --all-targets` (parallel default) followed by `cargo test --locked -p
  platform_macos --all-targets -- --test-threads=1`. Apply identically in
  ci.yml check lane and release validate; DEVELOPMENT.md Full Local Gate
  line changes with it (pinned — update `require_development_gate_line`).
  If 4a is deferred, add `-p app -- --test-threads=1` to the serial leg
  instead — do not parallelize `app` before 4a lands.

**Steps:** 4a guard + regression test → full serial suite still green → 4b
workflow/docs/pin edits → three consecutive green CI runs (flake watch) →
record the wall-time delta in the commit message.

**Risks:** an unfound serialization dependency in a "clean" crate surfaces
as a flake — the three-run watch exists for this; if one appears, pin that
crate to the serial leg and file the fix separately. Do not chase timing.

**Acceptance:** CI wall-time measurably down (expect minutes on the macOS
lane); no new flakes across three runs; the regression test proves the guard
lock.

## 5 — Quality-gate polish (C5, C9)

- **C5:** fix `crates/model_client/tests/quality.rs:29` comment ("80% of 20
  cases = 16") to the real arithmetic (21 cases → 17 must pass); align the
  `check-quality.sh:11-14` framing. Comment-only; the integer math is
  already correct and slightly stricter than prose claimed.
- **C9 (contingency, not an edit now):** `completion-dear-team` ("let you
  know") and `completion-thanks` ("be in touch") are the fragile multi-word
  `contains` cases (baseline 20/21, margin 3). Predeclared remedy from the
  parent plan stands: on hosted-runner instability, weaken the substring or
  drop the case — never lower the 80% threshold. Add a one-line comment in
  the corpus header (or the driver) naming that remedy so a future failure
  is handled by policy, not debate.
- **Probe cross-ref:** `crates/model_client/tests/latency.rs:298-306`
  duplicates the 8-typo battery now canonical in the corpus — add a
  cross-reference comment pointing at `quality-corpus.jsonl` so edits track.
- **Mirror test:** add the `make_overlay`-failure startup test (twin of
  `startup_adapter_permission_failure_stops_before_engine`; recording
  factories return `Err` from `make_overlay`, assert abort before engine and
  the `overlay init:` error form).

**Acceptance:** comments accurate; mirror test red-green verified by
temporarily inverting the factory result; gates green.

## 6 — post_verify smoke fail-fast (C6)

**Finding:** the bounded-startup smoke relies on `COMPME_RUN_MS`, enforced
only inside the heartbeat loop (run_loop.rs:6316). Known startup steps are
non-blocking (flock, config, `AXIsProcessTrustedWithOptions` returns
immediately), so no demonstrated hang — but the only backstop for an unknown
pre-loop block is the job's 30-minute timeout.

**Design:** wrap the smoke invocation in a coarse shell timeout so a hang
fails in seconds, not 30 minutes. macOS runners lack GNU `timeout`; use the
portable form the repo can pin:

```bash
"/Applications/Compme.app/Contents/MacOS/compme" & pid=$!
for _ in $(seq 1 30); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
if kill -0 "$pid" 2>/dev/null; then kill -9 "$pid"; echo "smoke hang" >&2; exit 1; fi
wait "$pid"
```

(env assignments as in the current step; 30s ≈ 6× the 5s run-ms budget).
Keep `timeout-minutes: 30` as the outer bound.

**Steps:** step edit → actionlint → pin update (step command pin) → validate
on the next release run (this job is tag-gated; a scratch validation can
exercise the wrapper against `sleep 600` locally in the self-test style).

**Acceptance:** a simulated hang (binary replaced by `sleep 600` in a local
dry run of the step script) exits 1 in ~30s; the real release run stays
green; first-release validation of the whole job (watch item 10) still
applies.

## 7 — Docs-only pushes: run the docs gates (C7)

**Finding:** ci.yml `paths-ignore` (docs/**, *.md, LICENSE) applies to
`push` only, so a docs-only push to `main` runs no CI at all — including
`check-version-docs.sh`, whose whole purpose is catching doc drift.

**Design:** keep the expensive lanes skipped; add a cheap `docs` job that is
the push lane's complement: triggered on push with `paths: [docs/**, *.md,
LICENSE]`, ubuntu, minutes: runs `check-version-docs.sh`,
`check-model-gates.sh --self-test` is overkill — scope to the doc-facing
checks: version-docs live + the markdown-referencing self-tests the battery
already isolates (minimum: version-docs live; add `check-bundle-metadata.sh
--self-test` only if cheap). Two path filters that partition pushes mean
every push runs exactly one lane.

**Steps:** new job → actionlint → pins (new job topology/timeout/permissions
in the CI table) → docs (DEVELOPMENT CI paragraph, pinned) → verify with a
docs-only scratch push.

**Acceptance:** a docs-only push runs the docs job (and only it); a stale
version surface in that push fails; compiled pushes unchanged.

## 8 — God-file extraction series (C8)

Behavior-preserving refactor series; each sub-item its own commit series in
a quiet stretch, mechanical-move discipline (the `startup()` extraction is
the template: move verbatim, rebind names, per-step gates, revert on smell).

- **8a — `run()` heartbeat loop:** extract `LoopState` (the ~34 `mut`
  preamble bindings) and split the `HostEvent` match arms into
  `fn on_focus/on_caret/on_text_changed/…(&mut LoopState, …)` plus
  `poll_downloads/poll_stats/poll_settings` helpers. `run()` becomes
  startup → state init → dispatch loop. Target: no function over ~300 lines
  in the file's coordinator section. Ordering tests from item 5's mirror
  pattern cover the seams.
- **8b — `platform_macos/src/lib.rs` carve (15,185 lines):** four
  independent mechanical moves, one commit each: `ax_worker.rs` (actor +
  handle + resources + observer backend), `overlay.rs`
  (MacosOverlayPresenter), `clipboard.rs` (ClipboardRestoreCoordinator),
  `accept_keymap.rs` (Carbon hotkeys + AcceptKeymap). `lib.rs` keeps the
  adapter impl + wiring + FFI helpers. Tests move with their subjects
  (in-src `mod tests` convention preserved per module).
- **8c — config builders out of `run_loop.rs`:** move the pure emoji-prefs,
  personalization, memory-config, and model-download-edge builders (~1k
  lines) into `config.rs` / a `downloads` module. Pure moves; their tests
  travel along.

Sequencing within 8: 8c (lowest risk) → 8b (independent of app) → 8a
(largest; after 4a so the guard-lock refactor doesn't collide).

**Acceptance per sub-item:** `git diff --stat` shows moves not rewrites
(reviewer spot-check), full serial suite + clippy green, no pin changes
except doc lines that name file structure (ARCHITECTURE.md module
descriptions updated in the same commit).

## 9 — Docs hygiene batch

One commit series, working-tree docs only:

- **ROADMAP header:** move the ~50-line commit-hash delta narration
  (ROADMAP.md:3-51) into `docs/CHANGELOG.md` (or point at the
  GitHub-generated release notes); header shrinks to boundary + count +
  branch + link. Fix the wording at ROADMAP.md:27: "tag `v0.1.5` (commit
  `14ae81e`)" — the SHAs are commits, not tags.
- **TROUBLESHOOTING.md links:** add from DEVELOPMENT.md and the README
  support/issue path (currently README-only).
- **Full Local Gate wrapper:** `tools/dev/check.sh` (house contract,
  `--self-test`) running the deterministic gate + script battery in order;
  DEVELOPMENT.md names it as the canonical entry, keeps the itemized list
  for detail. Wire its self-test into the CI battery + pins
  (`require_development_gate_line`).
- **Gate-list grouping:** fix the interleaved live/self-test ordering
  artifact (DEVELOPMENT.md:211-214).

**Acceptance:** version-docs + model-gates checkers green after the ROADMAP
restructure (header anchor pin from item 1 must still match); wrapper
self-test green; links resolve.

## 10 — Watch items (no code now)

- **rust-cache eviction window (Q1, confirmed upstream):** on an exact
  primary-key hit the post-job save never runs, and the key doesn't derive
  from `tools/spike/models` — after a GitHub cache eviction the model
  re-downloads every run until the next Cargo.lock/toolchain rotation. Watch
  the first runs after Phase 0; if eviction churn shows up, switch the model
  dir to a dedicated `actions/cache` step keyed on the model sha256 (new
  action ⇒ allowlist + pin work — only if the churn is real).
- **post_verify first live run (C6):** the job's end-to-end behavior on a
  real runner is validated at the next release; item 6's fail-fast makes a
  surprise cheap.

## Sequencing

```text
Phase 0  commit Batch 3+4 (prereq for everything)
Phase A  independent quick fixes, any order, own series:
           1 version-docs anchors · 2 governance guards (+decision) ·
           3 dependabot procedure · 5 quality polish · 6 post_verify timeout
Phase B  7 docs lane (one ci.yml + pin session; can share with 4b)
Phase C  4a app guard-lock → 4b parallel split (measure, 3-run watch)
Phase D  9 docs hygiene batch (after 1, so the new anchors are exercised)
Phase E  8c → 8b → 8a extraction series (quiet stretch, revert-friendly)
Watch    10 (first post-commit CI runs + next release)
```

## Global acceptance

Every item: its acceptance met; deterministic gate green (rule 5);
`check-model-gates.sh` live + `--self-test` green; actionlint clean on
touched workflows; affected script self-tests green; docs moved with
behavior; ROADMAP status line per AGENTS.md. Plan complete when: both
version-boundary docs are gated, the governance job either has teeth or
honestly warns, dependabot bumps have a documented procedure, CI runs
parallel where safe and serial where required, docs-only pushes run the doc
gates, the two god-files are decomposed, and the audit memory's finding list
is fully closed.
