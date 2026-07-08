# Compme v0.1.0 — first public release

Compme is an open-source inline text-completion engine for macOS: local GGUF
completions rendered as ghost text in the field you are typing in, accepted
word-by-word or in full from the keyboard. All inference is local
(llama.cpp/Metal); there is no telemetry of any kind — enforced by a CI policy
gate, not just policy.

## Highlights

- **Inline completions** — short local-model continuations, debounced and
  gated per field, with candidate cycling and a force-show hotkey.
- **Rebindable accept keys** — Tab = next word, `` ` `` = full accept by
  default; any key plus Shift/Ctrl/Option/Command via the in-app key recorder.
- **Writing aids** — high-precision trailing-word autocorrect, standalone
  grammar/spell fix (underline + banner + exact range replacement), `:shortcode`
  emoji with skin-tone/gender preferences, curated thesaurus, opt-in US→UK
  spelling normalization.
- **Per-app and per-domain control** — enable/disable, Tab-key and collection
  opt-outs, and feature overrides per app; browser-domain exclusions fail
  closed when no fresh URL resolves.
- **Privacy by construction** — prompts pass a redaction layer
  (secrets/emails/cards) before any model call; optional typing memory is
  AES-256-GCM encrypted with a Keychain-managed key and redacted before
  encryption; screen-context OCR is redacted at the sole capture seam.
- **Model picker** — built-in catalog with per-machine RAM-fit verdicts and
  verified, resumable downloads (SHA-256 checked before use).
- **Settings** — nine-tab settings window, menu-bar tray, launch-at-login,
  signed `compme://` deep-link overrides (Ed25519, fail-closed).

## Requirements

- macOS 14+ on Apple silicon (Metal inference).
- Accessibility permission (prompted on first run). Input Monitoring is not
  required.
- ~1 GB disk for the default small model (downloaded on first setup).

## Install

Homebrew (available once this release is published):

```sh
brew tap mudrii/compme https://github.com/mudrii/compme
brew install --cask compme
```

Or download `compme-0.1.0-macos.zip` below; verify with the `.sha256` file.
`compme-0.1.0-update.json` is a machine-readable manifest for tooling (nothing
in-app consumes it yet).

**This release is ad-hoc signed** (no Apple Developer ID yet): Gatekeeper
blocks the first launch. After installing, approve the app under System
Settings → Privacy & Security ("Open Anyway"), or install the cask with
`--no-quarantine` if you accept the trade-off. A future release will be
Developer-ID signed and notarized.

## Known limitations

- macOS only. Windows and Linux adapters are committed deliverables: the
  shared `PlatformAdapter` contract, fail-closed scaffolds, Windows host
  services (file hardening, console handling), and 3-OS CI already ship; the
  real UIA/AT-SPI adapters are the next roadmap tier.
- Inference is Metal-only in this build (CPU fallback builds exist for other
  targets but are not packaged).
- Secure fields and secure-input sessions are always blocked (by design).
