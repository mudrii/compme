# Development

This document describes the current development workflow for the Compme
Rust workspace and the separate spike package.

## App Bundle

`tools/bundle/make-app.sh` assembles an ad-hoc-signed `Compme.app` under
`target/bundle/` from the release binary: `LSUIElement` menu-bar app, bundle id
`com.compme.app`, and the `compme://` URL scheme declared (Launch Services
registration). The bundle is the unlock for URL-scheme reception,
`SMAppService` launch-at-login, and a stable TCC identity. Real
codesign/notarization (Developer ID) is the A3 ship item.

Smoke test: `COMPME_RUN_MS=1500 target/bundle/Compme.app/Contents/MacOS/compme`.

## Repository State

The current checkout has no git tags. Treat the code as unreleased workspace
behavior unless a future release process adds tags, release notes, or packaged
artifacts.

The root `Cargo.toml` is a Rust workspace with these members
([updated 2026-06-11] — keep in sync with `Cargo.toml`):

- `crates/platform` — cross-platform adapter contract
- `crates/context`, `crates/ranker`, `crates/engine_core`, `crates/engine` — suggestion pipeline
- `crates/personalization`, `crates/redaction`, `crates/prefs`, `crates/memory` — steering, privacy, prefs, encrypted history
- `crates/stats` — usage statistics + lifetime persistence
- `crates/webconfig` — signed `compme://` deep links
- `crates/emoji`, `crates/textcase`, `crates/thesaurus`, `crates/autocorrect`, `crates/localize` — local replacement features
- `crates/compat` — per-app compatibility tiers
- `crates/model_catalog`, `crates/model_fetch`, `crates/model_client` — model catalog, downloads, llama.cpp client
- `crates/platform_macos` — the macOS adapter (AX, overlay, tray, settings window)
- `crates/app` — the `compme` binary

`tools/spike` is excluded from the root workspace and must be checked
separately.

## Prerequisites

Required for root workspace development:

- Rust toolchain
- macOS when building or testing `platform_macos`
- Xcode Command Line Tools

Required for live macOS acceptance:

- unlocked macOS GUI session
- Accessibility permission for the terminal
- Input Monitoring permission for event-tap probes
- no global Secure Input owner, unless intentionally using `--force`
- TextEdit open with a focused editable document for TextEdit gates

Required for model-backed tests:

- GGUF model files at the paths used by `model_client` and `tools/spike`

Current local model paths:

```text
tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf
tools/spike/models/qwen2.5-0.5b-instruct-q4_k_m.gguf
```

### Selecting the completion model

`compme` resolves the model path with env > `config.env` > built-in default
(`run_loop::DEFAULT_MODEL`):

```sh
# one-off override
COMPME_MODEL_PATH=/abs/path/to/model.gguf compme
```

```text
# persistent: $HOME/Library/Application Support/compme/config.env
COMPME_MODEL_PATH=/abs/path/to/model.gguf
```

The MVP default is **Qwen2.5-0.5B Q4_K_M** — chosen for the warm sub-150 ms first
token the latency gate requires, not for output quality. The reference app
(Cotypist) ships a far larger default (~3 GB Gemma 4) behind a downloaded,
tiered catalog. Any GGUF that `llama-cpp-2` can load works via the override
above, so tiering up is a config change, not a code change. A selectable
download manager and per-tier catalog remain A2/A3 scope (see
`docs/superpowers/specs/2026-06-03-engine-macos-mvp-design.md` §5, §8).

## Root Workspace Commands

Format:

```sh
cargo fmt --all
cargo fmt --all -- --check
```

Lint:

```sh
cargo clippy --workspace --all-targets -- -D warnings
```

Test:

```sh
cargo test --workspace --all-targets
```

Build:

```sh
cargo build --workspace --all-targets
```

Use `--all-targets` for tests and clippy. The macOS acceptance regression
coverage includes example targets, and plain `cargo test --workspace` will not
run those tests.

## Spike Commands

Run from `tools/spike`:

```sh
cargo fmt
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --bins
```

The spike package is intentionally separate from the root workspace. Root
workspace commands do not validate it.

## Full Local Gate

Run this before considering a change ready for review:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --workspace --all-targets

cd tools/spike
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --bins
```

For macOS adapter work, also run the live acceptance harness when the GUI state
is available:

```sh
tools/acceptance/run-a1b-live-gates.sh
```

## Test Strategy

The repository uses focused Rust unit tests and example-target tests.

Root workspace coverage includes:

- pure text helpers in `context`
- candidate shaping in `ranker`
- UX classification and subscription behavior in `platform`
- deterministic event/command behavior in `engine_core`
- local model trait and latency coverage in `model_client`
- macOS adapter unit tests and example regression tests in `platform_macos`

The macOS example tests are important because they verify behavior used by live
acceptance binaries. Keep them in the `--all-targets` gate.

Spike coverage includes:

- pure seam behavior in `tools/spike/src/lib.rs`
- model integration timing in `tools/spike/tests/model_integration.rs`
- compile coverage for probe binaries

## Live Acceptance Development Loop

For `platform_macos` changes:

1. Run root format, clippy, tests, and build.
2. Build example binaries:

   ```sh
   cargo build -p platform_macos --examples
   ```

3. Prepare macOS:

   - unlock the session
   - grant permissions
   - open TextEdit
   - focus an editable document
   - disable password fields and other Secure Input owners

4. Run:

   ```sh
   tools/acceptance/run-a1b-live-gates.sh
   ```

5. Inspect logs under `tools/acceptance/logs/`.

Use `--dry-run` to inspect commands without executing them:

```sh
tools/acceptance/run-a1b-live-gates.sh --dry-run
```

Use `--skip-textedit` when validating browser or external popup targets that
must remain focused:

```sh
tools/acceptance/run-a1b-live-gates.sh --skip-textedit --browser-pid <pid>
tools/acceptance/run-a1b-live-gates.sh --skip-textedit --popup-pid <pid>
```

## Model Development Notes

`model_client::LlamaModel` currently:

- loads a GGUF model through `llama-cpp-2`
- enables Metal offload through `with_n_gpu_layers(999)`
- **[Updated 2026-06-08 — G3 closed]** runs on a dedicated worker thread owning a **persistent** `LlamaContext` (no longer a fresh context per completion) and **reuses the KV cache** for the shared prompt prefix (`reusable_prefix_len` + `clear_kv_cache_seq`), re-decoding only the divergent suffix
- serializes `complete()` calls via a mutex held across the round-trip; the backend is a `'static` singleton
- supports `warm_up()` so launch can trigger the first Metal decode before serving suggestions
- supports ordered `shutdown()` so the model/backend are dropped before process teardown
- greedily samples up to the requested max token count
- decodes pieces with `token_to_piece` and a UTF-8 decoder

Known future production work:

- cancellation and timeout policy
- production multi-candidate ranking, quality thresholds, and model-client stop/cancellation policy beyond the current `engine_core`/`ranker` shaping

(Persistent model actor, serialized access, and prefix-cache reuse are now
implemented — see design spec §15 G3.)

## Documentation Updates

When changing behavior, update the relevant docs:

- `README.md`: entrypoint, high-level commands, status.
- `docs/ARCHITECTURE.md`: crate responsibilities and runtime design.
- `docs/DEVELOPMENT.md`: commands, gates, workflow.
- `docs/ACCEPTANCE.md`: live macOS validation and harness behavior.
- `docs/superpowers/*`: detailed plans, decisions, and evidence.

Do not replace detailed planning evidence with summaries. Keep summaries in the
root docs and preserve the detailed artifacts under `docs/superpowers/`.
