# compme — Agent Brief

Inline text-completion engine (Rust). macOS ships first; Windows and Linux are
committed deliverables behind the shared `PlatformAdapter` contract.

Follow YAGNI principles.

## Orientation

- `docs/ROADMAP.md` — **docs/ROADMAP.md is the single source of truth** for
  pending work and status. Read it before non-trivial work; update it when you
  ship.
- `docs/DEVELOPMENT.md` — prerequisites, commands, and the canonical gate list.
- `docs/ARCHITECTURE.md` — crate responsibilities and runtime design.
- `docs/ACCEPTANCE.md` — the live macOS gate ledger (22 runner-pinned IDs) and
  where evidence is recorded.
- `docs/RELEASING.md` — tag → sign → notarize → publish → cask runbook.
- `Qfd.md` — audit findings and their remediation record.

This repo is CodeGraph-indexed (`.codegraph/`): `codegraph explore "<symbols or
question>"` returns the relevant source plus callers and blast radius in one
call. Reach for it before grep/find when locating or understanding code.

## Workflow

- Work from `main`; commit directly to `main` — no branches, no PRs.
- Minimal diffs, stdlib first, no speculative abstraction. Non-trivial logic
  ships with a test.
- Every gate green before you commit. The gate set is the `sh` fence under
  "Full Local Gate" in `docs/DEVELOPMENT.md`; run it in order with:

  ```sh
  tools/dev/check.sh      # runs that fence, skipping tools missing from PATH
  ```

  `fmt` + `clippy` + `test` alone are **not** the gate: the policy checker
  (`tools/release/check-model-gates.sh`), the doc/version checkers, and ~20
  script self-tests catch what the compiler cannot see.
- Report honestly: paste failing output, name what you skipped and why, and
  never claim a gate you did not run. Confirm your own edits landed (`git diff`)
  before reporting them.

## Tripwires

Each of these has broken a real run here.

- **Machine-pinned docs.** `check-model-gates.sh` pins workspace test counts,
  crate counts, action SHAs, workflow step shapes, and named test symbols to
  exact doc lines. Change a count, rename a pinned test, or move a test file →
  re-stamp the doc or checker **in the same commit**.
- **Version anchors.** `check-version-docs.sh` gates eight documented version
  surfaces against the root `Cargo.toml`, matched on exact anchor phrases. A
  reword fails loudly by design; fix the anchor in the checker, same commit.
- **Test lanes.** `platform_macos` and `app` share process-global state — run
  them with `-- --test-threads=1`. The other crates run in parallel.
- **Where tests live.** `run_loop`'s and `platform_macos/lib.rs`'s unit tests
  are in sibling `run_loop_tests.rs` / `lib_tests.rs` (`#[path]` modules), not
  inline. Add tests there.
- **`tools/spike` is outside the workspace** — separate `Cargo.lock`, separate
  gates; workspace commands do not cover it.
- **One brief only.** `AGENTS.md` is canonical; `CLAUDE.md`, `GEMINI.md`, and
  `QWEN.md` are symlinks to it. Adding `CURSOR.md`, `.cursorrules`,
  `.github/copilot-instructions.md`, `.cursor/rules/*`, etc. fails
  `check-agent-briefs.sh` — put shared guidance here instead.
- **Fail-closed platforms.** `platform_windows`/`platform_linux` are honest
  scaffolds returning `UnsupportedField`. Never make them look implemented.
- **Evidence you cannot synthesize.** The 22 live macOS gates need a granted
  GUI session, Windows/Linux acceptance needs that hardware, and the release
  `post_verify` job needs a real tag. Never mark one passed from a headless
  run — record real results in `docs/ACCEPTANCE.md`.
- **ABI-pinned deps.** `llama-cpp-2` is exact-pinned twice in
  `crates/model_client` (one entry per target) and again in `tools/spike`. Bump
  all three together, then run `tools/release/run-model-gates.sh`; it is
  excluded from Dependabot for exactly this reason.

# Self-Learning

When the user corrects you or catches a mistake, add the lesson as a one-line
rule under `# Lessons` before continuing, so it cannot happen twice.

# Lessons

- A release is not done until the workflow's final job (cask finalization) has run: after pushing a tag, follow the run through every environment approval and verify the published cask/checksum consistency end-to-end before reporting success.
- Cutting a release includes reconciling every doc that names the published version (README status/boundary, SECURITY supported release, ROADMAP anchors, release-boundary notes) in the same flow as the version bump — not as a follow-up when someone notices.
- Do not quote a metric that inline test code inflates: file line counts and per-file coverage here were 56-63% test code, so measure the production surface (or split the tests out) before calling a file large or well covered.
- Prove a "verbatim" refactor instead of asserting it: a normalized token-sequence diff of the old function against the new function plus its extracted callees catches a dropped branch that green tests and clippy will not.
- `platform_linux`/`platform_windows` are compiled and tested on all three hosts, so their logic must not use `std` APIs whose semantics follow the *build* host: `Path::is_absolute("/home/u")` is false on the Windows lane. Encode POSIX rules on the string (`starts_with('/')`), and run a non-mac lane before claiming a platform-crate change is green.
