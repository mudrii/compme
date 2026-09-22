# compme — Agent Brief

Inline text-completion engine (Rust). macOS ships first; Windows and Linux are
committed deliverables behind the shared `PlatformAdapter` contract. YAGNI:
minimal diffs, stdlib first; non-trivial logic ships with a test.

## Orientation

- **docs/ROADMAP.md is the single source of truth** for pending work and
  status: read it before non-trivial work, update it when you ship.
- `docs/DEVELOPMENT.md` — commands and the canonical gate list.
- `docs/ARCHITECTURE.md` — crates and runtime design.
- `docs/ACCEPTANCE.md` — live macOS gate ledger and where evidence goes.
- `docs/RELEASING.md` — tag → sign → notarize → publish → cask runbook; read
  it before any release step.
- `Qfd.md` — audit findings and their remediation record.
- CodeGraph-indexed: `codegraph explore "<symbols or question>"` before
  grep/find.

## Workflow

- Commit directly to `main` — no branches, no PRs.
- Gate every commit with `tools/dev/check.sh` (runs the "Full Local Gate"
  fence in `docs/DEVELOPMENT.md`). `fmt` + `clippy` + `test` alone miss the
  policy, doc/version, and script self-test checkers.
- Report with evidence: paste failing output, name each skipped gate and why,
  and `git diff` your edits before reporting them.
- A lane you can't run isn't green: a change touching `#[cfg(target_os)]` code
  or its tests is done only when the pushed CI run is green on the lane that
  compiles it.

## Tripwires

Each of these has broken a real run here.

- **Machine-pinned docs.** `check-model-gates.sh` pins test/crate counts,
  action SHAs, workflow step shapes, and named tests to exact doc lines —
  re-stamp the doc or checker in the same commit.
- **Version anchors.** `check-version-docs.sh` matches eight version surfaces
  on exact anchor phrases; a reword fails by design — fix the anchor in the
  checker, same commit.
- **Test lanes.** `platform_macos` and `app` share process-global state: run
  them with `-- --test-threads=1`.
- **Test location.** `run_loop` and `platform_macos/lib.rs` unit tests live in
  sibling `run_loop_tests.rs` / `lib_tests.rs` (`#[path]` modules).
- **`tools/spike`** is outside the workspace: own `Cargo.lock`, own gates, own
  `[patch.crates-io]` (a patch applies only to the workspace declaring it).
- **One brief.** `CLAUDE.md`, `GEMINI.md`, `QWEN.md` are symlinks to this file;
  `check-agent-briefs.sh` rejects any other harness rule file.
- **Platform truth.** `platform_windows` is the fail-closed platform-I/O
  scaffold; `platform_linux` is wired for AT-SPI2 and X11 with explicit
  unsupported surfaces — document each boundary exactly.
- **Host-neutral platform crates.** `platform_linux`/`platform_windows` build
  on all three hosts: encode POSIX rules on the string (`starts_with('/')`),
  never build-host `std` semantics (`Path::is_absolute`).
- **macOS CI bash 3.2.** A heredoc inside `$(...)` dies there; use
  `ruby -e '<single-quoted script>' args`.
- **Evidence you cannot synthesize.** The 22 live macOS gates, Windows/Linux
  acceptance, and release `post_verify` need a GUI session, that hardware, or a
  real tag; record only real results in `docs/ACCEPTANCE.md`.
- **ABI-pinned deps.** `llama-cpp-2` is exact-pinned twice in
  `crates/model_client` and once in `tools/spike`, both patched to
  `vendor/llama-cpp-2`: bump all three pins, rebase the vendored copy,
  regenerate both lockfiles, run `tools/release/run-model-gates.sh`.
- **Previous-input gate.** New previous-input readers gate on
  `previous_input_context_chars`, not `context_bound` (floored nonzero even
  with the feature off).
- **User-data erase.** Every delete path also clears the live copies seeded
  from the store (`PreviousInputs` rings) in the same edge.
- **Memory key.** Read/delete-only store opens use `load_existing_memory_key`;
  `load_or_create_memory_key` mints an OS key-store entry.

## Lessons

When the user corrects you or catches a mistake, add the lesson here as a
one-line rule before continuing.

- Confirm provenance before calling test duplication a defect or hardening
  scope drift — both can be intentional or separately commissioned.
- A release is done when cask finalization has run and the published
  cask/checksum verify end-to-end; reconcile every doc naming the version in
  the same flow as the bump.
- Measure the production surface before calling a file large or well covered:
  inline tests are 56–63% of some files.
- Prove a "verbatim" refactor with a normalized token diff of old vs new plus
  extracted callees.
- Verify CLI flags with `<tool> --help` (model IDs: `pi --list-models`) before
  quoting a command.
- A number in a dated record section is a citation: check it against its
  source document, not today's value.
