# Development

This document describes the current development workflow for the Compme
Rust workspace and the separate spike package.

## App Bundle

`tools/bundle/make-app.sh` assembles an ad-hoc-signed `Compme.app` under
`target/bundle/` from the release binary: `LSUIElement` menu-bar app, bundle id
`com.compme.app`, and the `compme://` URL scheme declared (Launch Services
registration). The bundle is the unlock for URL-scheme reception,
`SMAppService` launch-at-login, and a stable TCC identity. Local source bundles
are ad-hoc signed by default; the stable `vX.Y.Z` tag workflow Developer-ID signs,
notarizes, and staples release artifacts. It compiles the release binary first
in a secretless prebuild job, then fails closed before bundling if signing
credentials are missing and before packaging or publication if notarization
credentials are missing. Both sides of the prebuild artifact boundary require a
thin arm64 binary; the signing job registers its cleanup path before importing
the Developer-ID certificate and fails if the temporary keychain cannot be
deleted and proven absent.

Smoke test: `COMPME_RUN_MS=1500 target/bundle/Compme.app/Contents/MacOS/compme`.

## Repository State

The current checkout develops on `main`; the latest published release is the
signed, notarized, and stapled v0.1.4. Earlier release notes preserve each
artifact's original signing status. Workspace behavior may be newer than the
latest tag, so use tag-specific release assets and notes when validating a
published version. Specifically, `v0.1.4` points to `18b8dc0`; current `main` adds
post-tag runtime/release hardening, moves A2 validation out of automation, and
removes the duplicate model-location control. Those changes are unreleased
until the next tag.

The root `Cargo.toml` is a Rust workspace with 25 members
([verified 2026-07-10] — keep in sync with `Cargo.toml`):

- `crates/platform` — cross-platform adapter contract
- `crates/context`, `crates/ranker`, `crates/engine_core`, `crates/engine` — suggestion pipeline
- `crates/personalization`, `crates/redaction`, `crates/prefs`, `crates/memory` — steering, privacy, prefs, encrypted history
- `crates/stats` — usage statistics + lifetime persistence
- `crates/webconfig` — confirmed signed/unsigned reversible `compme://` links
- `crates/emoji`, `crates/textcase`, `crates/thesaurus`, `crates/autocorrect`, `crates/grammar`, `crates/localize` — local replacement features
- `crates/compat` — per-app compatibility tiers
- `crates/model_catalog`, `crates/model_fetch`, `crates/model_client` — model catalog, downloads, llama.cpp client
- `crates/platform_macos` — the macOS adapter (AX, overlay, tray, settings window)
- `crates/platform_windows`, `crates/platform_linux` — fail-closed adapter scaffolds for Tier 1.1; Windows additionally has owner-only DACL hardening, a console control handler, and native URL opening, while Linux URL opening reaps its `xdg-open` child
- `crates/app` — the `compme` binary

`tools/spike` is excluded from the root workspace and must be checked
separately.

Every native/build.rs/FFI/crypto dependency carries an exact `=x.y.z` pin
(llama-cpp-2, rusqlite, aes-gcm, ed25519-dalek, the objc2/security-framework
stack, windows) so a routine `cargo update` cannot move ABI-sensitive code
without a review-forcing manifest diff; foundational (libc, getrandom) and
pure-Rust deps stay caret-ranged.

## Prerequisites

Required for root workspace development:

- Rust toolchain
- macOS when building or testing `platform_macos`
- Xcode Command Line Tools
- CMake for the bundled llama.cpp build (`brew install cmake`; not included
  with Xcode CLT)

Required for live macOS acceptance:

- unlocked macOS GUI session
- Accessibility permission for the terminal
- no global Secure Input owner, unless intentionally using `--force`
- TextEdit open with a focused editable document for TextEdit gates

Input Monitoring is not required for the production Carbon accept path or the
root live runner. It is only relevant for historical `tools/spike` CGEventTap
probes and the explicit revoked-permission spot-check in `docs/ACCEPTANCE.md`.

Required for model-backed tests:

- GGUF model files at the paths used by `model_client` and `tools/spike`

Current local model paths:

```text
tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf
tools/spike/models/qwen2.5-0.5b-instruct-q4_k_m.gguf
```

### Selecting the completion model

`compme` resolves the model path with env > `config.env` > built-in default
(`run_loop::DEFAULT_MODEL`, which is `tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf`):

```sh
# one-off override
COMPME_MODEL_PATH=/abs/path/to/model.gguf compme
```

```text
# persistent: $HOME/Library/Application Support/compme/config.env
# (or wherever COMPME_CONFIG points)
COMPME_MODEL_PATH=/abs/path/to/model.gguf
```

The MVP default is **Qwen2.5-0.5B Q4_K_M** — chosen for the warm sub-150 ms first
token the latency gate requires, not for output quality. The reference app
(Cotypist) ships a far larger default (~3 GB Gemma 4) behind a downloaded,
tiered catalog. The override supports compatible non-recurrent transformer
GGUFs accepted by the pinned `llama-cpp-2` backend. Recurrent/hybrid models are
currently unsupported because prefix-KV reuse would require full re-decode.

**In-app model picker (Setup tab).** The Setup tab now exposes a
"Model to download:" popup over the built-in `model_catalog` (four entries,
smallest first: `qwen2.5-0.5b-q4_k_m`, `llama-3.2-1b-q4_k_m`,
`qwen2.5-1.5b-q4_k_m`, `gemma-2-2b-q4_k_m`). The picker defaults to the
recommended entry (the smallest unencumbered model) and downloads the selected
catalog model on click into
`$HOME/Library/Application Support/compme/models/<name>.gguf`. Three behaviors
are wired (D14):

- **RAM-fit gate** — each popup row is suffixed with its `ram_verdict` for
  the machine's available memory (`fits` / `tight — may swap under load` /
  `exceeds available memory`). `Exceeds` blocks download before license prompts
  or fetch work; `Tight` remains allowed with a warning.
- **License click-through** — every download path routes through
  `model_catalog::download_gate`. Unencumbered (Apache-2.0) entries proceed
  silently; gated entries (Llama Community, Gemma Terms) prompt a terms
  click-through that fails closed and is remembered once-per-model in
  `COMPME_LICENSE_ACCEPTED`.
- **Dest-exists guard** — a present, non-empty `.gguf` at the destination is not
  re-downloaded (`model_present`), so a repeat "Download" click never clobbers a
  good file; an interrupted 0-byte stub is treated as absent and re-fetched.

On a completed download (or when a model is already present, or via
"Choose Model…") the app persists `COMPME_MODEL_PATH` to `config.env` so the
next launch loads it; if it is unset and the configured path is missing, startup
auto-adopts the newest `.gguf` in the models dir. Downloads use `model_fetch`
with catalog-pinned SHA-256 hashes, a catalog-derived byte ceiling, and
verify-before-rename semantics. The same Setup pane has exactly one
model-location action, **Show Models Folder**; there is no separate "Reveal
Model in Finder" control. See
`docs/superpowers/specs/2026-06-03-engine-macos-mvp-design.md` §15 D14.

## Root Workspace Commands

Format:

```sh
cargo fmt --all
cargo fmt --all -- --check
```

Lint:

```sh
cargo clippy --locked --workspace --all-targets -- -D warnings
```

Test:

```sh
cargo test --locked --workspace --all-targets -- --test-threads=1
```

Build:

```sh
cargo build --locked --workspace --all-targets
```

The suite is ~1851 tests. Use `--all-targets` for clippy, test, and build so
the macOS example regression targets are compiled and the `platform_macos`
example regression tests run.

## Spike Commands

Run from `tools/spike`:

```sh
cargo fmt
cargo fmt -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --bins
```

The spike package is intentionally separate from the root workspace. Root
workspace commands do not validate it.

## Full Local Gate

Run this before committing a change to main or treating local validation as complete:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets -- --test-threads=1
cargo build --locked --workspace --all-targets
cargo build --locked -p platform_macos --examples

find tools/acceptance tools/bundle tools/release -type f -name '*.sh' ! -path 'tools/acceptance/run-a2-compat-gates.sh' ! -path 'tools/release/check-a2-matrix-ledger.sh' -print0 | xargs -0 bash -n
tools/release/validate-version.sh --self-test
tools/bundle/check-bundle-metadata.sh
tools/bundle/check-bundle-metadata.sh --self-test
ruby -c Casks/compme.rb
tools/bundle/make-app.sh --self-test
tools/bundle/bundle-smoke.sh
tools/bundle/bundle-smoke.sh --self-test
tools/acceptance/e2e-complete-me.sh --self-test
tools/acceptance/missing-model-startup.sh --self-test
tools/acceptance/missing-model-startup.sh
tools/acceptance/run-ui-assisted-session.sh --self-test
tools/acceptance/run-a1b-live-gates.sh --self-test
tools/release/check-model-client-features.sh
tools/release/check-model-client-features.sh --self-test
tools/release/check-agent-briefs.sh
tools/release/check-agent-briefs.sh --self-test
tools/release/check-privacy-policy.sh
tools/release/check-privacy-policy.sh --self-test
bash tools/release/check-model-gates.sh
tools/release/run-model-gates.sh --self-test
tools/release/update-cask.sh --self-test
tools/release/finalize-cask.sh --self-test
tools/release/notarize-app.sh --self-test
tools/release/write-update-manifest.sh --self-test
bash tools/release/run-model-gates.sh

cd tools/spike
cargo fmt -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --bins
```

The root suite is ~1851 tests. The `tools/spike` workspace is separate from the
root workspace — root commands do not validate it, so it carries its own gate.
The full gate uses `cargo test --locked --workspace --all-targets -- --test-threads=1`
because the `platform_macos` example regression tests are part of the acceptance
surface and several macOS pasteboard checks share process-wide OS state.
Branch/PR CI also runs native Windows/Linux portability jobs: workspace fmt,
portable-workspace clippy/tests excluding `platform_macos`, and an app-binary
build through each fail-closed target facade. Tag release validation is
narrower: it runs fmt, clippy, test, and build for `platform_windows` on
`windows-latest` and for `platform_linux` on `ubuntu-latest`. Those are
CI/release-runner gates rather than local macOS commands.
For release-readiness audits with the local GGUF model installed, also run the
ignored model-backed gates from [ACCEPTANCE.md](ACCEPTANCE.md) and the A1b
manual checklist. The automated tag workflow self-tests the A1b checklist
structure; it does **not** claim that the 17 remaining LOOK/manual gates passed.
Record those live results separately and keep unresolved gates pending.

A2 validation is local/manual-only. CI, tag releases, and
`tools/release/check-model-gates.sh` neither execute nor syntax-check the A2
runner or ledger checker. For an explicit manual pre-release compatibility pass:

```sh
tools/acceptance/run-a2-compat-gates.sh --self-test
tools/release/check-a2-matrix-ledger.sh --self-test
```

Continue with [the A2 manual procedure](ACCEPTANCE.md#a2-compatibility-and-context-smoke-gates-localmanual-only),
which defines the required target mapping, evidence directory, `ledger`
assignment, matrix run, and local ledger validation.

For macOS adapter work, also run the live acceptance harness when the GUI state
is available:

```sh
tools/acceptance/run-a1b-live-gates.sh
```

## Test Strategy

The repository follows test-first discipline: the pure cores are written and
unit-tested before the glue that calls them, so config parsing, model selection,
catalog/picker resolution, and pipeline shaping are all provable without
touching the environment, the filesystem, or a real model. The
lookup-injection pattern (`Config::from_lookup`, `config_file_path_from`) exists
precisely so these rules stay unit-testable without mutating the process
environment.

The AppKit/FFI glue in `platform_macos` (and the AppKit slice of the model
picker) is build-and-LOOK-verified rather than unit-tested: it is compiled by
the `--all-targets` gate and exercised live through the acceptance harness, not
asserted in pure unit tests. The pure helpers it consumes (e.g. the picker's
`model_menu_titles` / `selected_catalog_entry`, the catalog's `ram_verdict` and
`download_gate`) are unit-tested in their owning crates.

Root workspace coverage includes:

- pure text helpers in `context`
- candidate shaping in `ranker`
- UX classification and subscription behavior in `platform`
- deterministic event/command behavior in `engine_core`
- local model trait and latency coverage in `model_client`
- pure model selection / picker / catalog logic in `app` and `model_catalog`
- macOS adapter unit tests and example regression tests in `platform_macos`

The macOS example tests are important because they verify behavior used by live
acceptance binaries. Compile them via the `--all-targets` gate; run them with
`cargo test --locked --workspace --all-targets -- --test-threads=1`.

**Known flake.** A small number of `platform_macos` tests share the process-wide
general `NSPasteboard`, so running them in parallel can intermittently fail when
two tests touch the clipboard at once. They pass when run isolated (single test
thread / a focused `cargo test`). This is a test-harness artifact, not a product
bug.

Spike coverage includes:

- pure seam behavior in `tools/spike/src/lib.rs`
- model integration timing in `tools/spike/tests/model_integration.rs`
- compile coverage for probe binaries

## Live Acceptance Development Loop

For `platform_macos` changes:

1. Run root format, clippy, tests, and build.
2. Build example binaries:

   ```sh
   cargo build --locked -p platform_macos --examples
   cargo build --locked -p app
   ```

3. Prepare macOS:

   - unlock the session
   - grant permissions
   - open TextEdit
   - focus an editable document
   - disable password fields and other Secure Input owners

4. Run:

   ```sh
   tools/acceptance/run-a1b-live-gates.sh --skip-build
   ```

5. Inspect logs under `tools/acceptance/logs/`.

Use `--dry-run` to inspect commands without executing them:

```sh
tools/acceptance/run-a1b-live-gates.sh --dry-run
```

Use `--skip-textedit --allow-incomplete` when intentionally validating only a
browser or external popup target that must remain focused. Omit
`--allow-incomplete` for release/readiness runs; mandatory TextEdit skips fail
by default.

Use `--allow-manual` only after executing and recording the MANUAL checklist
lines emitted by the runner. Omit it for unattended readiness runs; unresolved
manual gates fail by default.

```sh
tools/acceptance/run-a1b-live-gates.sh --skip-textedit --allow-incomplete --browser-pid <pid>
tools/acceptance/run-a1b-live-gates.sh --skip-textedit --allow-incomplete --popup-pid <pid>
```

## Model Development Notes

`model_client::LlamaModel` currently:

- loads a GGUF model through `llama-cpp-2`
- enables Metal offload on macOS through `with_n_gpu_layers(999)`; current
  non-macOS builds are CPU-only until the planned Vulkan/CUDA features and CI
  SDKs land
- **[Updated 2026-06-08 — G3 closed]** runs on a dedicated worker thread owning a **persistent** `LlamaContext` (no longer a fresh context per completion) and **reuses the KV cache** for the shared prompt prefix (`reusable_prefix_len` + `clear_kv_cache_seq`), re-decoding only the divergent suffix
- serializes `complete()` calls via a mutex held across the round-trip; the backend is a `'static` singleton
- supports `warm_up()` so launch can trigger the first Metal decode before serving suggestions
- supports ordered `shutdown()` so the model/backend are dropped before process teardown
- greedily samples up to the requested max token count
- clamps the prompt to context capacity and caps generation to the remaining
  token budget before decode
- decodes pieces with `token_to_piece` and a UTF-8 decoder

Known future production work:

- cancellation and timeout policy
- production multi-candidate ranking, quality thresholds, and model-client stop/cancellation policy beyond the current `engine_core`/`ranker` shaping

(Persistent model actor, serialized access, and prefix-cache reuse are now
implemented — see design spec §15 G3.)

## Documentation Updates

When changing behavior, update the relevant docs:

- `README.md`: entrypoint, high-level commands, status.
- `AGENTS.md`: canonical agent brief; keep `CLAUDE.md`, `GEMINI.md`, and
  `QWEN.md` symlinked to it.
- `docs/ARCHITECTURE.md`: crate responsibilities and runtime design.
- `docs/DEVELOPMENT.md`: commands, gates, workflow.
- `docs/ACCEPTANCE.md`: live macOS validation and harness behavior.
- `docs/RELEASING.md`: release workflow, model gates, cask finalization,
  signing, notarization, and update-manifest behavior.
- `docs/superpowers/*`: detailed plans, decisions, and evidence.

Do not replace detailed planning evidence with summaries. Keep summaries in the
root docs and preserve the detailed artifacts under `docs/superpowers/`.
