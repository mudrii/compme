# Compme v0.1.1 — model setup + app icon fixes

> Historical release record for protected tag `v0.1.1` (`401e07b`). It
> describes that published artifact, not current `main` or the latest stable
> release.

A small follow-up to v0.1.0 fixing two problems people hit on the first
Homebrew install: a downloaded model never becoming usable, and a missing app
icon. All inference stays local; no telemetry (CI-enforced).

## Fixes

- **Downloaded models are now wired up automatically.** Previously a completed
  download only logged a hint to set `COMPME_MODEL_PATH` — impossible for a
  Finder-launched `.app` — so the Setup "Model file" row stayed unchecked and
  the model was never loaded. The app now persists the downloaded model path to
  its config; relaunch and it loads.
- **Setup pane shows download progress and errors.** Download status (percent,
  "downloaded — relaunch to use", or the failure reason) was previously
  stderr-only and invisible in a GUI launch; it now appears in the Setup tab.
- **App icon.** The bundle now ships an icon (`CFBundleIconFile`), so Finder and
  the Dock no longer show the generic placeholder.

## Upgrade

```sh
brew upgrade --cask compme
```

Then reopen Compme. If you downloaded a model in v0.1.0, click **Download
Model** once more (or relaunch after it finishes) so the model path is recorded.

**Still ad-hoc signed** (no Apple Developer ID yet): Gatekeeper blocks the first
launch — approve under System Settings → Privacy & Security ("Open Anyway"), or
install the cask with `--no-quarantine`. Requirements are unchanged from v0.1.0
(macOS 14+ Apple silicon, Accessibility permission, ~1 GB for the default
model).
