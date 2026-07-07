# Compme

[![CI](https://github.com/mudrii/compme/actions/workflows/ci.yml/badge.svg)](https://github.com/mudrii/compme/actions/workflows/ci.yml)

Compme is an **open-source, multi-platform** inline text-completion engine
**inspired by** [Cotypist](https://cotypist.app). It is **not** a proprietary
clone, but the plan is to cover Cotypist's non-payment user-facing writing
workflow while shaping the implementation around open local-first goals.
Compme deliberately leaves out payment, licensing, subscription tiers, and
multi-device seats. Every feature that ships is available to every user with no
pricing gates; the only constraint on which local models are offered is hardware
capability. macOS ships first; **Windows and Linux are
committed deliverables** built behind a shared cross-platform `PlatformAdapter`
contract. All inference is local (llama.cpp), with no proprietary telemetry.

The current validated workspace has the deterministic macOS MVP and A2/A3 core
surfaces implemented; the remaining live-validation backlog is tracked in
[docs/ROADMAP.md](docs/ROADMAP.md). The repository is a Rust workspace of 25
crates: a pure completion core, a set of OS-agnostic text features
(autocorrect, British-English, grammar, emoji, thesaurus, redaction, stats,
personalization, ranking, compatibility tiers, model catalog), macOS/Windows/Linux
platform adapter crates, a llama.cpp-backed local model seam with an async
downloader, and the `compme` binary that wires them together. A separate spike
prototype under `tools/spike` validates low-level macOS behavior before it is
promoted into the workspace.

The macOS run loop is functional: it reads caret/text context through
Accessibility, generates short local completions, classifies field UX (inline /
popup / blocked / hotkey-only / unsupported), shows a non-activating AppKit
ghost-text overlay, intercepts accept keys through transient Carbon hotkeys, and
inserts accepted text through Accessibility, synthetic keys, or a clipboard-paste
fallback. Tagged release artifacts are built through the GitHub Release workflow
as Developer-ID signed and notarized bundles when the repository release secrets
are configured.

## Install

### Homebrew (macOS)

Homebrew cask install is not available until the first signed `v*` release
publishes the artifact and finalizes the cask checksum. Until then, build from
source.

Once the first release is published, install with:

```sh
brew tap mudrii/compme https://github.com/mudrii/compme
brew install --cask compme
```

Release artifacts are Developer-ID signed and notarized by the tag workflow once
release secrets are configured (see [docs/RELEASING.md](docs/RELEASING.md)).
Enable Compme under System Settings → Privacy & Security → Accessibility. All
inference is local; nothing leaves the machine.

### From source

```sh
git clone https://github.com/mudrii/compme && cd compme
tools/bundle/make-app.sh        # → target/bundle/Compme.app (ad-hoc signed by default)
open target/bundle/Compme.app
```

After either install path, enable Compme under System Settings → Privacy &
Security → Accessibility. Source builds should use the bundle for `compme://`
deep links, Login Items, and a stable TCC identity. For core development, an
unbundled `cargo run -p app` is still fine.

## Features

- **Inline model completions** — short local GGUF continuations through `llama-cpp-2`
  (Metal), debounced and gated per field.
- **Modifier-combo accept keys** — Tab = next word, grave/`` ` `` = full accept by
  default, now rebindable to any key plus Shift/Ctrl/Option/Command modifiers. An
  in-app key recorder and a Shortcuts settings pane edit them; the persisted form is a
  `modifier+keycode` string such as `shift+48` or `ctrl+shift+50`.
- **Inline autocorrect** — high-precision trailing-word typo→correction replacement,
  with no false-correct on real words.
- **Standalone grammar/spell fix** — a separate trigger/accept flow for the word
  at the caret: underline + correction banner, strict single-word model vetting,
  and exact range replacement.
- **British-English normalization** — opt-in US→UK spelling for unambiguous American-only forms.
- **Emoji completion** — `:shortcode`→emoji with skin-tone and gender preferences.
- **Thesaurus / synonyms** — curated synonym suggestions for the trailing word.
- **Per-app and per-domain control** — per-app enable, Tab-key disable, input-collection
  opt-out, and mid-line / autocorrect / grammar-fix / thesaurus overrides, plus per-app and
  per-browser-domain exclusion.
- **Browser-domain detection** — the focused browser page's host is read from the
  Accessibility URL and matched against domain exclusions. Model submit fails closed
  when browser-domain rules are configured and no fresh URL resolves.
- **Model picker with RAM-fit advisory** — the Setup tab lets you choose which built-in
  catalog model to download, each row carrying a `fits` / `tight` / `exceeds` verdict
  for this machine, with a dest-exists guard that skips a model already on disk.
- **Encrypted typing memory** — opt-in AES-256-GCM store for accepted completions
  or all monitored typing, redacted before encryption, keyed by a
  Keychain-managed key.
- **Usage statistics** — a rolling 30-day accumulator (shown / accepted / dismissed /
  superseded, words completed, latency) surfaced in the Statistics pane.
- **Nine-tab settings window** — Setup, General, Personalization, Apps, Context,
  Emoji, Shortcuts, Statistics, About.
- **Menu-bar icon** — a caret + double-chevron template image (it recently replaced the
  old "CM…" text title; that title remains only as a fallback if the image fails to load).
- **Release updater surface** — the tray's **Check for Updates…** item opens the
  latest GitHub Release; tagged releases also upload a machine-readable update
  manifest next to the notarized zip and checksum.
- **Signed deep-link config** — a fail-closed `compme://setOverride` URL scheme for
  reversible per-app/per-domain overrides; non-reversible settings require an Ed25519
  signature.

## Repository Layout

```text
.
├── Cargo.toml                         # Root Rust workspace
├── crates/
│   ├── platform/                      # Cross-platform adapter + UX contract
│   ├── platform_macos/                # macOS Accessibility/AppKit/Carbon adapter
│   ├── platform_windows/              # Windows adapter scaffold (fail-closed)
│   ├── platform_linux/                # Linux adapter scaffold (fail-closed)
│   ├── context/                       # Pure caret/text-context helpers
│   ├── engine_core/                   # Deterministic suggestion state machine
│   ├── engine/                        # Runtime host: engine_core ↔ platform ↔ overlay
│   ├── model_client/                  # LocalModel trait + llama.cpp backend
│   ├── model_catalog/                 # Built-in GGUF catalog + RAM-fit verdict
│   ├── model_fetch/                   # Async model downloader (verify → atomic rename)
│   ├── ranker/                        # Candidate shaping helpers
│   ├── prefs/                         # Suggestion-gating preferences
│   ├── compat/                        # Per-app compatibility tiers/quirks
│   ├── personalization/              # Instructions / strength / sender identity
│   ├── autocorrect/                   # Trailing-word typo correction
│   ├── grammar/                       # Pronoun capitalization and grammar-fix post-filter
│   ├── localize/                      # US↔British English normalization
│   ├── emoji/                         # :shortcode → emoji completion
│   ├── thesaurus/                     # Synonym suggestions
│   ├── textcase/                      # Case-pattern detection/application
│   ├── redaction/                     # Secret/high-entropy redaction
│   ├── memory/                        # Encrypted typing-history store
│   ├── stats/                         # Acceptance statistics + sparkline
│   ├── webconfig/                     # Signed compme:// deep-link config
│   └── app/                           # compme binary and run loop
├── tools/
│   ├── acceptance/                    # A1b live, A2 compat, E2E, and startup gates
│   ├── bundle/                        # macOS bundle assets (URL scheme, icon)
│   ├── release/                       # Release gates, cask, notarization, update manifest
│   └── spike/                         # Separate A0 prototype workspace
└── docs/
    ├── ARCHITECTURE.md
    ├── DEVELOPMENT.md
    ├── ACCEPTANCE.md
    ├── RELEASING.md
    └── superpowers/                   # Detailed planning and validation notes
```

`tools/spike` is intentionally excluded from the root workspace. Run its checks
from `tools/spike/`.

## Crates

| Crate | Purpose |
|-------|---------|
| `platform` | Cross-platform contract shared by the pure engine and platform adapters: field handles, capabilities, insertion strategies, subscriptions, overlay presenter, and UX-mode classification. |
| `platform_macos` | macOS implementation of the adapter and overlay presenter using Accessibility, CoreGraphics, AppKit/Carbon, and pasteboard APIs; ghost overlay, tray, key recorder, and settings window. |
| `platform_windows` | Windows adapter scaffold that reports Windows and fails closed for platform I/O/subscription methods until a real adapter is implemented. |
| `platform_linux` | Linux adapter scaffold that reports Linux and fails closed for platform I/O/subscription methods until a real adapter is implemented. |
| `context` | Pure text-context helpers around a caret (left/right context, left-tail extraction, prompt-prefix trimming). |
| `engine_core` | Deterministic `SuggestionMachine` that turns focus/text/caret/model events into commands. |
| `engine` | Impure-but-deterministic wiring between the pure machine and the platform adapter + overlay; surfaces `RequestCompletion` as a `CompletionRequest` for the host to fulfil, so inference never blocks the machine. |
| `model_client` | `LocalModel` trait plus a `LlamaModel` implementation using `llama-cpp-2`; macOS enables Metal, while non-macOS targets use dynamic/Vulkan-capable backends. |
| `model_catalog` | Pure, static catalog of which local models the General/Setup pane offers, their download sources, and a `fits` / `tight` / `exceeds` RAM-fit verdict for the host. |
| `model_fetch` | Pure SHA-256 integrity + resume planning, plus the blocking network downloader (`.part` → verify → atomic rename) and a `ModelDownloader` worker thread. |
| `ranker` | Candidate shaping helpers: word capping, first-word extraction, and repetition penalty. |
| `prefs` | Suggestion-gating preferences: per-app and per-domain enable/exclude, per-app Tab-key disable, and a global pause/snooze, resolved against an injected clock. |
| `compat` | Pure classifier from a macOS bundle id to a compatibility tier, plus the gating policy each tier implies (mirrors the Cotypist compatibility table). |
| `personalization` | Prompt-based personalization: global + per-app + per-domain instruction maps (request-time app and domain steering are wired, and a Personalization pane edits global instructions, strength, and sender identity — the per-app/per-domain instruction editor remains a follow-up), a 6-stop strength slider (no tier caps), and sender identity, templated into a steering preamble. |
| `autocorrect` | Pure, high-precision trailing-word typo→correction table with the query's capitalization reapplied; never "corrects" a real word. |
| `grammar` | Pure grammar helpers: current inline pronoun capitalization plus the LLM-backed standalone grammar/spell-fix post-filter. |
| `localize` | Pure, high-precision US→British spelling normalization for American-only forms; deliberately skips ambiguous words. |
| `emoji` | Pure `:shortcode`→emoji completion honoring skin-tone (Fitzpatrick) and gender preferences. |
| `thesaurus` | Pure synonym lookup with the queried word's case pattern applied; supports selection and auto modes. |
| `textcase` | Pure capitalization-pattern detection and application, shared by the text-suggestion crates. |
| `redaction` | Pure best-effort scrubbing of emails, Luhn-valid card numbers, and high-entropy tokens before any persistence or diagnostics; biased to over-redact. |
| `memory` | Encrypted local memory for accepted completions or all monitored typing: text is redacted then AES-256-GCM encrypted to SQLite, opt-in storage modes, Keychain-managed key, plaintext app metadata for per-app counts/delete. |
| `stats` | Pure rolling 30-day accumulator for shown/accepted/dismissed/superseded counts, words completed, and latency, with injected time. |
| `webconfig` | Strict, fail-closed parser for `compme://setOverride` deep links; reversible per-app/per-domain overrides only, signed (Ed25519) links required for anything non-reversible. |
| `app` | `compme` binary: config loading, run loop, tray menu + icon, settings window wiring, inference worker, and shutdown ordering. |

## Requirements

- macOS for the macOS adapter and live acceptance harnesses.
- Rust toolchain compatible with the workspace.
- Xcode Command Line Tools for native macOS frameworks.
- Accessibility permission for the terminal running live probes.
- Input Monitoring is **not** required by the production accept path (transient
  Carbon hotkeys); it is only relevant to the historical A0 CGEventTap spike
  probes under `tools/spike`.
- Local GGUF model files for model latency tests and spike inference probes.

The checked-in local model paths used by current tests and probes are:

```text
tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf
tools/spike/models/qwen2.5-0.5b-instruct-q4_k_m.gguf
```

## Configuration keys

Settings layer as `env > config.env file > default`; keys with Settings
switches persist to the file, and an env var overrides the file at relaunch.
Many keys with a per-app split also accept `_ON_APPS` / `_OFF_APPS` lists of
comma-separated bundle ids.

| Key | Meaning |
|-----|---------|
| `COMPME_ENABLED` | Master suggestion on/off (also the tray toggle; persisted on toggle). |
| `COMPME_DEFAULT_ENABLED` | Per-app suggestion-policy default in `prefs` (distinct from the master `COMPME_ENABLED`). |
| `COMPME_MIDLINE` | Allow mid-line completions (also a Settings switch). `_ON_APPS` / `_OFF_APPS` override per app. |
| `COMPME_AUTOCORRECT` | Inline typo autocorrect (default off; also a Settings switch). `_ON_APPS` / `_OFF_APPS` override per app. |
| `COMPME_GRAMMAR_FIX` | Standalone grammar/spell-fix trigger flow (default off). `_ON_APPS` / `_OFF_APPS` override per app. |
| `COMPME_GRAMMAR_CHECK_KEY` | Always-on grammar trigger shortcut as a `modifier+keycode` string; runs detection for the word at the caret. |
| `COMPME_GRAMMAR_ACCEPT_KEY` | Correction-only accept key as a `modifier+keycode` string; replaces the vetted word only while a correction is showing. |
| `COMPME_BRITISH_ENGLISH` | British-English normalization (default off). |
| `COMPME_THESAURUS` | Inline thesaurus / synonym suggestions (default off). `_ON_APPS` / `_OFF_APPS` override per app. |
| `COMPME_TRAILING_SPACE` | Append a trailing space on single-word accept (default off; also a Settings switch). |
| `COMPME_EMOJI` | Emoji completion on/off. |
| `COMPME_EMOJI_SKIN_TONE` | Preferred skin tone (Fitzpatrick) for emoji completion. |
| `COMPME_EMOJI_GENDER` | Preferred gender (neutral / female / male) for emoji completion. |
| `COMPME_ACCEPT_WORD_KEY` | Word-accept key as a `modifier+keycode` string (e.g. `48` or `shift+48`); default Tab (48). Applies at relaunch. |
| `COMPME_ACCEPT_FULL_KEY` | Full-accept key as a `modifier+keycode` string (e.g. `50` or `ctrl+shift+50`); default grave/backtick (50). |
| `COMPME_FORCE_ACTIVATE_KEY` | Always-on shortcut re-showing the currently held suggestion (alias: `COMPME_FORCE_ACTIVATE`); no fresh inference, no-op when nothing is pending. |
| `COMPME_TOGGLE_APP_KEY` | Always-on shortcut toggling suggestions for the focused app. |
| `COMPME_TOGGLE_GLOBAL_KEY` | Always-on shortcut toggling the global suggestion switch. |
| `COMPME_EXCLUDED_APPS` | Comma-separated bundle ids excluded from completion (persisted by tray/deep-link/config today; Apps editing UI pending). |
| `COMPME_EXCLUDED_DOMAINS` | Comma-separated browser hosts excluded from completion. |
| `COMPME_ENABLED_APPS` / `COMPME_DISABLED_APPS` | Per-app suggestion enable / disable overrides. |
| `COMPME_TAB_DISABLED_APPS` | Comma-separated bundle ids where the Tab word-accept key is disabled (Tab types normally there). |
| `COMPME_NO_COLLECT_APPS` | Apps for which input is never collected into typing memory. |
| `COMPME_CLIPBOARD_CONTEXT` | Opt-in: include clipboard text in the context block. |
| `COMPME_SCREEN_CONTEXT` | Opt-in: include screen text in the context block. |
| `COMPME_PREVIOUS_INPUT_CONTEXT` | Cap (characters) of previous-input context to include; off when unset. |
| `COMPME_INSTRUCTIONS` | Custom steering instructions prepended to the prompt. |
| `COMPME_INSTRUCTIONS_APPS` / `COMPME_INSTRUCTIONS_APP_<BUNDLE>` | Comma-separated bundle ids and sanitized per-app instruction values, e.g. `COMPME_INSTRUCTIONS_APP_COM_APPLE_TEXTEDIT`. |
| `COMPME_INSTRUCTIONS_DOMAINS` / `COMPME_INSTRUCTIONS_DOMAIN_<HOST>` | Comma-separated hosts and sanitized per-domain instruction values, e.g. `COMPME_INSTRUCTIONS_DOMAIN_DOCS_GOOGLE_COM`. |
| `COMPME_STRENGTH` | Personalization strength (6 stops). |
| `COMPME_SENDER_NAME` / `COMPME_SENDER_EMAIL` | Sender identity templated into the steering preamble. |
| `COMPME_MEMORY` | Typing-memory collection mode: `off` / `accepted` / `all` (default `off`). |
| `COMPME_MEMORY_PATH` | Override path for the encrypted memory store (store stays off without a path). |
| `COMPME_MEMORY_KEY` | 64-hex AES key for memory (default: Keychain-managed). |
| `COMPME_LAUNCH_AT_LOGIN` | `true`/`false` registers/unregisters the login item; absent leaves Login Items alone. |
| `COMPME_MODEL_PATH` | Path to the GGUF model file to load (defaults to the checked-in spike model). |
| `COMPME_TRUSTED_KEY` | 64-hex Ed25519 public key for SIGNED `compme://` links (fail-closed when unset). |
| `COMPME_LICENSE_ACCEPTED` | Comma-separated model names whose license terms were accepted (written by the app after the click-through prompt). |

Advanced tuning knobs (clamped, sensible defaults) also exist for prompt and
debounce behavior — `COMPME_PROMPT_MODE`, `COMPME_DEBOUNCE_MS`,
`COMPME_MAX_WORDS`, `COMPME_MAX_TOKENS`, `COMPME_HEARTBEAT_MS`,
`COMPME_MIN_CONTEXT`, `COMPME_CANDIDATES`. Harness/diagnostic keys
(`COMPME_ACCEPTANCE_*`, `COMPME_DIAG_*`, `COMPME_DEBUG`, `COMPME_RUN_MS`,
`COMPME_STUB_COMPLETION`, `COMPME_CONFIG`) are for tests and debugging only.
Model/release gate overrides such as `COMPME_MODEL_GPU_LAYERS`,
`COMPME_MODEL_CONTEXT_TOKENS`, `COMPME_REQUIRE_MODEL_TESTS`,
`COMPME_REQUIRE_MODEL_CONTEXT`, `COMPME_REQUIRE_LATENCY_BUDGET`,
`COMPME_MODEL_GATE_PATH`, `COMPME_MODEL_GATE_URL`, and
`COMPME_MODEL_GATE_SHA256` are documented in
[docs/RELEASING.md](docs/RELEASING.md).

## Quick Start

Build the root workspace:

```sh
cargo build --locked --workspace --all-targets
```

Run the root test suite, including example-target regression tests:

```sh
cargo test --locked --workspace --all-targets -- --test-threads=1
```

Run the root lint gate:

```sh
cargo clippy --locked --workspace --all-targets -- -D warnings
```

Run the spike workspace checks:

```sh
cd tools/spike
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --bins
```

Run the macOS live acceptance harness:

```sh
tools/acceptance/run-a1b-live-gates.sh
```

Before running live gates, unlock the session, disable global Secure Input, grant
Accessibility to the launching terminal, open TextEdit, and focus an editable
document. (Input Monitoring is only needed for the historical CGEventTap spike
probes under `tools/spike`, not the Carbon-hotkey production accept path.)

## Current Validation Gates

Use these gates before treating the workspace as development-ready. The root
suite is roughly 1,747 tests:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets -- --test-threads=1
cargo build --locked --workspace --all-targets
cargo build --locked -p platform_macos --examples

bash -n tools/acceptance/*.sh tools/bundle/*.sh tools/release/*.sh
tools/bundle/check-bundle-metadata.sh
tools/bundle/check-bundle-metadata.sh --self-test
tools/bundle/make-app.sh --self-test
tools/bundle/bundle-smoke.sh
tools/bundle/bundle-smoke.sh --self-test
tools/acceptance/e2e-complete-me.sh --self-test
tools/acceptance/missing-model-startup.sh --self-test
tools/acceptance/missing-model-startup.sh
tools/acceptance/run-ui-assisted-session.sh --self-test
tools/acceptance/run-a1b-live-gates.sh --self-test
tools/acceptance/run-a2-compat-gates.sh --self-test
tools/release/check-a2-matrix-ledger.sh --self-test
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

CI and the tag release workflow also run scoped Windows/Linux adapter gates on
native runners: fmt, clippy, test, and build for `platform_windows` on
`windows-latest`, and the same four commands for `platform_linux` on
`ubuntu-latest`. They are CI/release-runner gates, not part of the local macOS
gate above.

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for the full development workflow
and [docs/ACCEPTANCE.md](docs/ACCEPTANCE.md) for live macOS validation.

## Architecture

At a high level:

1. `platform_macos` observes focus and caret changes from the frontmost app and,
   for browsers, reads the focused page's host from the Accessibility URL.
2. The platform adapter emits stable `FieldHandle` values and reads
   `TextContext` from Accessibility.
3. `engine_core::SuggestionMachine` debounces text changes and emits a
   `RequestCompletion` command for the current field snapshot; the host fulfils it
   off the machine thread so inference never blocks.
4. `model_client::LocalModel` generates a short continuation, or a local feature
   (autocorrect / British / emoji / thesaurus) proposes a replacement, gated by
   per-app and per-domain preferences.
5. `engine_core` validates the returned generation/snapshot and emits `ShowGhost`,
   `UpdateGhost`, `Insert`, or `Hide`.
6. `platform_macos` presents ghost text through an AppKit `NSPanel`, intercepts
   accept actions through transient Carbon hotkeys (`RegisterEventHotKey`, armed
   only while a suggestion is shown, supporting modifier+key combos rebound from
   the Shortcuts pane), and inserts accepted text through the safest available
   strategy. A menu-bar tray icon and a nine-tab settings window (Setup /
   General / Personalization / Apps / Context / Emoji / Shortcuts / Statistics /
   About) drive configuration and the model picker.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for details.

## Documentation Index

- [Roadmap & pending work](docs/ROADMAP.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Development](docs/DEVELOPMENT.md)
- [Acceptance](docs/ACCEPTANCE.md)
- [Releasing](docs/RELEASING.md)
- [Engine/macOS MVP design](docs/superpowers/specs/2026-06-03-engine-macos-mvp-design.md)
- [A1b macOS adapter contract](docs/superpowers/plans/2026-06-04-a1b-macos-adapter-contract.md)
- [Current work handoff](docs/superpowers/plans/2026-06-05-current-work-handoff.md)

## License

Licensed under the [Apache License, Version 2.0](LICENSE). The Apache-2.0 patent
grant is the deliberate choice for an inline-completion tool. Local model weights
are downloaded under their own licenses (e.g. Gemma terms, Qwen Apache-2.0, Llama
community license) and are never bundled — the download flow surfaces each
model's license for acceptance.

## Status

This repository is on an active development branch and has no release tags in
the current checkout. Treat documented behavior as current workspace behavior,
not a published product release.
