# Handoff — 2026-09-22 audit fix batch (resume from here)

> **Closed 2026-09-23.** Batches B and D and the §6 leftovers landed with a
> second six-dimension audit; status and the items still open are in
> [ROADMAP](../../ROADMAP.md#full-codebase-review-fixes--2026-092223) and
> `Qfd.md` §25. This file is kept as the record of the 2026-09-22 run.

**Date:** 2026-09-22 · **Main HEAD when written:** `b1b38ba` · **Updated:** same
day at `a357b94` — Batches A and C are complete and **pushed**; CI run
35737431131 is green on every lane with every mac-lane step (model gates,
count-pin policy, C2's four serialised tests) individually confirmed.
**Brief being executed:** [`2026-09-22-audit-fix-brief.md`](2026-09-22-audit-fix-brief.md)
(all findings, fix directions, gate commands, tripwires).
Development resumed on the pi agent (GLM) after Qwen's quota ran out; the
first push (`a861d96`) failed only the mac model gate on A1's timing-based
regression, fixed in `a357b94` (see §4).

This file is written so any model or person can resume without the previous
session's context. Read `AGENTS.md` first, then the brief, then this file.

## 1. Host and tooling (this NixOS machine)

- `cargo` is not on PATH. Everything runs inside:

  ```sh
  XDG_CACHE_HOME=/tmp/nixcache nix-shell -p gcc pkg-config gtk3 at-spi2-core glib dbus cmake ninja clang ruby shellcheck go \
    --run 'source target/revalidate/env.sh && <command>'
  ```

  `target/revalidate/env.sh` hard-codes `/nix/store` paths for libclang, cmake,
  gcc-lib and glibc-dev that rot after a nix GC. Test each with `[ -e path ]`;
  replacements: `ls -d /nix/store/*clang-21*-lib/lib`,
  `/nix/store/*cmake-*/bin/cmake`, `/nix/store/*gcc-*-lib/lib/libstdc++.so.6`,
  `/nix/store/*glibc-*-dev/include`; sed them in, then
  `rm -rf target/debug/build/llama-cpp-sys-2-*`. Repaired 2026-09-22.
- The documented Full Local Gate stops at `cargo clippy --workspace` here
  (objc2 is Apple-only). The gate that passes here is the ci.yml Linux lane:

  ```sh
  cargo fmt --all -- --check
  cargo clippy --locked --workspace --exclude platform_macos --all-targets -- -D warnings
  cargo test --locked --workspace --exclude platform_macos --exclude app --all-targets
  cargo test --locked -p app --all-targets -- --test-threads=1
  cargo test --locked --doc --workspace --exclude platform_macos
  RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --exclude platform_macos
  tools/release/check-version-docs.sh
  bash tools/release/check-model-gates.sh --self-test
  ```

  plus every script self-test in the fence except `make-icon.sh` (swift) and
  `bundle-smoke.sh` (plutil). `tools/release/run-model-gates.sh` exits 101 at
  its final `tools/spike` step on Linux; run its first `cargo test -p
  model_client --test latency -- --ignored` line by hand instead.
  `tools/acceptance/missing-model-startup.sh` fails here with libstdc++ status
  127 because it launches under `env -i` (host artifact).
- Cross-target compile of the mac crate works here and checks its test module:
  `cargo check --locked -p platform_macos --target aarch64-apple-darwin --all-targets`
  (Darwin and Windows std are installed in `target/.rustup`). It is a compile
  proof only; the mac lane is the execution authority.
- The GGUF at `tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf` is untracked; a
  fresh worktree needs `ln -s <repo>/tools/spike/models tools/spike/models`.
- Qwen ran in a herdr tab (`herdr agent list`, pane `wA:pM`); `herdr agent
  prompt <pane> "<text>"` steers, `herdr pane send-text` + `herdr agent
  send-keys <pane> ctrl+q` queues (queued text is read only when the turn ends;
  `esc` cancels the turn and delivers the queue). `.qwen/` is in
  `.git/info/exclude` locally.

## 2. Process rules the owner set (apply to every remaining item)

1. Strict TDD per item: add the behaviour-named regression test first, run it
   and capture the RED output, then the minimal production change, then GREEN.
2. One commit per item immediately after its gate passes. Conventional subject
   (`fix(crate): …`, `test(crate): …`, `docs: …`), body with what/why, the
   reproduced probe, test names, red-then-green evidence, the gate commands and
   the `test result:` line, and the trailer on its own line:
   `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
3. Stage by explicit path; never `git add -A` / `git add .`.
4. Tripwire: any change to the number of tests must re-stamp the five pinned
   count lines **in the same commit**: `README.md:367`,
   `docs/DEVELOPMENT.md:217` and `:326`, `docs/ROADMAP.md:3`,
   `docs/superpowers/specs/2026-07-01-grammar-fix-design.md:5`, with a
   "Re-stamps the five pinned workspace test-count lines (N -> M)" body line.
   **Current value: 2226** (mac-lane inventory; only the mac lane verifies it).
   `check-model-gates.sh` also pins named test symbols (`grep
   require_test_symbol`); re-stamp the checker in the same commit if one moves.
5. Numbers inside dated record sections are citations, not live claims
   (AGENTS.md lesson); do not "correct" them.
6. Every landed commit is independently validated by the coordinator: crate
   tests, fmt, `check-version-docs.sh`, `check-model-gates.sh --self-test`, and
   a red proof (parent's production file or a neutralised line with the new
   test kept) in an isolated `git worktree` with
   `CARGO_TARGET_DIR=target/validate`. Landed cherry-picks are compared to the
   validated worktree commit by `git patch-id` excluding the five pin files.

## 3. What is landed and validated on `main` (`070197e..b1b38ba`)

| Item | Commit | Validation evidence |
|---|---|---|
| brief | `070197e` | docs |
| A1 KV prefix reuse on `complete_n` | `cd901ba` | real model: fixed 16.7 s → 0.74 s (22.6×); parent code 1.0× |
| A2 `_`-compound credential keys | `0c63073` | probe red at parent, green at commit; `code` key split kept `error_code=500` |
| A3 vendor-prefix left boundary | `da2e1f6` | red/green; `sk-`/`ghp_` still redact after space, `(`, `«`, `?` |
| A4 fullwidth-digit PANs | `7d172e9` | red/green; Arabic-Indic digits still pass through (open, see §5) |
| A5 `COMPME_MODEL_PATH` in `SWITCH_KEYS` (38) | `c4ee2b9` | two tests red at parent |
| A6 warm-up failure → `WorkerHealth::Degraded` → `Blocked(ModelUnavailable)` | `c64bb6e` | red by neutralising the store; owner chose option 1 (keep latch) |
| A6 doc: latch is sticky until relaunch | `c785082` | rustdoc -D warnings clean |
| A7 dotenv unquoting (+ encoder escapes quote-wrapped values) | `71ba8bf` | confirmed at runtime; red by neutralising `surrounding_quotes` |
| `SWITCH_KEYS` completeness test mirrors the array | `14fd90a` | red both directions (list drift, array drift) |
| AGENTS.md dated-citation lesson | `6885ba0` | check-agent-briefs OK |
| C1 `#[cfg(unix)]` on the two `sh` tests | `caaf919` | 141 platform_linux tests, live count 41 |
| C7 drop dead `download_url`, fix `FetchError` doc | `ad8d765` | workspace check, rustdoc clean |
| C7 doc follow-up (entry points that exist) | `815ed9d` | checkers |
| C8 three doc comments corrected (emoji, platform, ranker) | `e0bb339` | claims verified against code, rustdoc clean |
| C3 drop `word_at_caret` seam + 6 duplicate tests, checker pins re-stamped | `48855c6` | 40 context tests; pins 2239→2233 |
| C4 drop `offer_replacement` wrapper + 7 twins (hardened assertions ported) | `542945b` | 170 engine_core tests; pins 2233→2226 |
| C6 shortcuts-text lock poison recovery | `fa60f4f` | clippy clean, no `if let Ok(..lock())` left in run_loop.rs |
| docs: drop references to deleted seams | `83ae8a0` | checkers |
| C2 shared `HOTKEY_GLOBALS_TEST_LOCK` for four mac tests + ci.yml comment | `e20912d` | Darwin cross-check compiles incl. tests, actionlint OK; **execution unproven until the mac lane runs** |
| docs: pasteboard attribution (DEVELOPMENT.md, pre-push) | `b1b38ba` | shellcheck, bash -n, pinned phrases intact |

## 4. Landed after the pause (all validated the same way)

| Item | Commit | Notes |
|---|---|---|
| C5 routing tests driven through production paths, `HostEventRoute` seam deleted | `a861d96` | no `#[test]` delta (pins stay 2226); mutation proofs: dropping the Correction-only gating fails the accept test, neutralising the Armed arm fails the grammar-check test |
| A1 regression made deterministic | `a357b94` | `LlamaModel::last_prompt_tokens_decoded()` telemetry; test asserts 223 prompt tokens decoded cold, exactly 1 on the identical repeat, on a natural paragraph prompt; red with the pre-A1 `clear()` restored (`left: 223, right: 1`) |

Why `a357b94` was needed (recorded so nobody re-adds a timing assertion): on
the paravirtual mac runner a generated token costs about as much as a prompt
batch, so cold/warm timing ratios are ~1.2x even with reuse working (22x on
Linux); and on the degenerate `"the quick brown fox jumps " x60` prompt the
greedy next token is a near-tie whose argmax flips deterministically between a
batched full-prompt decode and a single-token reuse decode (backend numerics,
not a cache-trim bug: on natural prose fresh, `complete` and `complete_n`
outputs are byte-identical).

## 5. CI state

`gh run view 35737431131` (CI) and `35737429746` (CodeQL) for `a357b94`: all
jobs green; on the mac lane the steps `Test (serial, macOS state)`,
`Model-backed smoke gate`, `Model-quality gate`, `Version docs check`, `Agent
brief alignment` and `Release model gate policy` each report `success`. The
`ci-${{ github.ref }}` concurrency group keeps only one pending run, so push
once per batch and read per-step conclusions, not the job verdict.

## 6. Still open from the review (not started)

- **Batch B** (adapter correctness; each needs the mac or Linux live lane; see
  the brief for file:line and fix direction): B1 AX worker `catch_unwind` on
  the observer/poll arms + lossy CFString read; B2 `insert_for_field` must use
  the injected secure-input provider; B3 macOS global-insert stale-focus check
  is pid-only and `FieldHandle.generation` is never read (owner decision:
  wire it or delete the contract sentence); B4 Linux `insert_replacing_range`
  never restores the caret + session-wide quarantine on readback mismatch;
  B5 `x11_tap.rs` `set_action` writes `action` outside the `grabbed` lock, and
  `MAX_ARMED_MS` disarms silently; B6 Linux `subscribe_accept` install failure
  is fatal at startup.
- **Batch D** (owner decisions): D1 CI concurrency group drops queued main
  runs (10 cancelled on 2026-09-20) and `protect-main` has no required checks;
  D2 three different macOS test counts in docs — **closed 2026-09-23**: every
  live count pin (README, DEVELOPMENT ×2, ROADMAP:3, grammar spec:5) reads
  2226; ROADMAP row 34's "2,222" (tied to `baa89c9`) and
  IMPLEMENTATION-EVIDENCE's "2,226" are dated citations, not live counts;
  D3 document the Linux-host
  portable fence and make `check.sh` non-zero when cargo lines are skipped;
  D4 `check-model-gates.sh` (5,010 lines) has no EXIT trap; D5 consolidate the
  dated 2026-09-19/20 docs and root `2FIX.md`/`FIXED.md`.
- Found during the batch, not fixed: `download_url_bounded` also has no caller
  outside `model_fetch`; two macOS readers of `shortcuts_text`
  (`settings_window.rs:1157`, `:1276`) still use `if let Ok(..lock())`;
  A4's digit mapper covers ASCII and fullwidth only (Arabic-Indic PANs pass);
  a value grabbed from `(token=abc)` swallows the `)` (pre-existing).

## 7. Local artifacts you may delete

- `target/validate/` (coordinator's isolated build dir) and any
  `target/validate-wt-*` worktrees (`git worktree list`; remove with
  `git worktree remove --force`).
- `.qwen/worktrees/c5` (C5 is committed; the worktree is stale).
- `.gate/*.log` predate this batch and are unrelated.
