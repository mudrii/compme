# Complete Me

Complete Me is an experimental macOS inline-completion engine. The repository
currently contains a Rust workspace for the core completion contract, a macOS
platform adapter, a llama.cpp-backed local model seam, and a separate spike
prototype used to validate low-level macOS behavior before it is promoted into
the workspace.

The project is not packaged as an end-user app yet. The current codebase is a
contract-first implementation and validation harness for:

- reading text context and caret state from focused macOS text fields through
  Accessibility
- generating short local completions with a GGUF model through `llama-cpp-2`
- deciding whether a field can support inline, popup, blocked, hotkey-only, or
  unsupported UX
- showing a non-activating AppKit ghost-text overlay
- intercepting Tab acceptance through a split observer/consumer `CGEventTap`
- inserting accepted text through Accessibility, synthetic keys, or clipboard
  paste fallback

## Repository Layout

```text
.
├── Cargo.toml                         # Root Rust workspace
├── crates/
│   ├── platform/                      # Cross-platform adapter and UX contract
│   ├── context/                       # Pure caret/text-context helpers
│   ├── ranker/                        # Candidate shaping helpers
│   ├── core/                          # Deterministic suggestion state machine
│   ├── model_client/                  # Local model trait and llama.cpp backend
│   └── platform_macos/                # macOS Accessibility/AppKit adapter
├── tools/
│   ├── acceptance/                    # A1b macOS live acceptance runner
│   └── spike/                         # Separate A0 prototype workspace
└── docs/
    ├── ARCHITECTURE.md
    ├── DEVELOPMENT.md
    ├── ACCEPTANCE.md
    └── superpowers/                   # Detailed planning and validation notes
```

`tools/spike` is intentionally excluded from the root workspace. Run its checks
from `tools/spike/`.

## Crates

| Crate | Purpose |
|-------|---------|
| `platform` | Public platform abstraction: field handles, capabilities, insertion strategies, subscriptions, overlay presenter, and UX mode classification. |
| `context` | Pure helpers for left/right context, left tail extraction, and prompt-prefix trimming. |
| `ranker` | Candidate shaping helpers such as word capping, first-word extraction, and repetition penalty. |
| `core` | Deterministic `SuggestionMachine` that turns focus/text/caret/model events into commands. |
| `model_client` | `LocalModel` trait plus a `LlamaModel` implementation using `llama-cpp-2` with Metal. |
| `platform_macos` | macOS implementation of `PlatformAdapter` and `OverlayPresenter` using Accessibility, CoreGraphics, AppKit, and pasteboard APIs. |

## Requirements

- macOS for the macOS adapter and live acceptance harnesses.
- Rust toolchain compatible with the workspace.
- Xcode Command Line Tools for native macOS frameworks.
- Accessibility permission for the terminal running live probes.
- Input Monitoring permission for acceptance-tap live probes.
- Local GGUF model files for model latency tests and spike inference probes.

The checked-in local model paths used by current tests and probes are:

```text
tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf
tools/spike/models/qwen2.5-0.5b-instruct-q4_k_m.gguf
```

## Quick Start

Build the root workspace:

```sh
cargo build --workspace --all-targets
```

Run the root test suite, including example-target regression tests:

```sh
cargo test --workspace --all-targets
```

Run the root lint gate:

```sh
cargo clippy --workspace --all-targets -- -D warnings
```

Run the spike workspace checks:

```sh
cd tools/spike
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --bins
```

Run the macOS live acceptance harness:

```sh
tools/acceptance/run-a1b-live-gates.sh
```

Before running live gates, unlock the session, disable global Secure Input, grant
Accessibility/Input Monitoring to the launching terminal, open TextEdit, and
focus an editable document.

## Current Validation Gates

Use these gates before treating the workspace as development-ready:

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

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for the full development workflow
and [docs/ACCEPTANCE.md](docs/ACCEPTANCE.md) for live macOS validation.

## Architecture

At a high level:

1. `platform_macos` observes focus and caret changes from the frontmost app.
2. The platform adapter emits stable `FieldHandle` values and reads
   `TextContext` from Accessibility.
3. `core::SuggestionMachine` debounces text changes and emits a
   `RequestCompletion` command for the current field snapshot.
4. `model_client::LocalModel` generates a short continuation from the prompt.
5. `core` validates the returned generation/snapshot and emits `ShowGhost`,
   `UpdateGhost`, `Insert`, or `Hide`.
6. `platform_macos` presents ghost text through an AppKit `NSPanel`, intercepts
   accept actions through `CGEventTap`, and inserts accepted text through the
   safest available strategy.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for details.

## Documentation Index

- [Architecture](docs/ARCHITECTURE.md)
- [Development](docs/DEVELOPMENT.md)
- [Acceptance](docs/ACCEPTANCE.md)
- [Engine/macOS MVP design](docs/superpowers/specs/2026-06-03-engine-macos-mvp-design.md)
- [A1b macOS adapter contract](docs/superpowers/plans/2026-06-04-a1b-macos-adapter-contract.md)
- [Current work handoff](docs/superpowers/plans/2026-06-05-current-work-handoff.md)

## Status

This repository is on an active development branch and has no release tags in
the current checkout. Treat documented behavior as current workspace behavior,
not a published product release.
