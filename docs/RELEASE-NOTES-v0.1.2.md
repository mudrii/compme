# Compme v0.1.2 — model loading fixed + model management

> Historical release record for protected tag `v0.1.2` (`4a6c1e2`). It
> describes that published artifact, not current `main` or the latest stable
> release.

Fixes the main v0.1.1 problem — a downloaded model never actually loaded — and
adds two ways to manage models. All inference stays local; no telemetry
(CI-enforced).

## Fixes

- **Downloaded models now load.** v0.1.1 only wired a model on a *fresh*
  download's completion, so models already on disk (downloaded under an older
  build) were never loaded — the Setup "Model file" row stayed ✗ and re-clicking
  Download reported "already present" without wiring anything. On launch Compme
  now auto-adopts the newest `.gguf` in its models folder and persists
  `COMPME_MODEL_PATH`; the Download button wires an already-present model too.

## New

- **Show Models Folder** — a Setup-tab button that opens
  `~/Library/Application Support/compme/models` in Finder.
- **Choose Model… (bring your own model)** — pick any local `.gguf` from a file
  panel; it's validated (GGUF header) and used in place, no copy or re-download.
- **New app icon** — an inline-completion motif (typed text · caret · ghost
  suggestion).

## Upgrade

```sh
brew upgrade --cask compme
```

Reopen Compme. If a model was downloaded but never loaded, it's adopted
automatically on launch (one relaunch may be needed the first time).

**Still ad-hoc signed** (no Apple Developer ID yet): Gatekeeper blocks the first
launch — approve under System Settings → Privacy & Security ("Open Anyway"), or
install the cask with `--no-quarantine`. Requirements unchanged (macOS 14+
Apple silicon, Accessibility permission).
