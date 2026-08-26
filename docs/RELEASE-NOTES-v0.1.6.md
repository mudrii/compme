# Compme v0.1.6 — audit remediation and release hardening

> Historical release record for protected tag `v0.1.6` (stamp the tag commit
> here when it is pushed). It describes that published artifact, not current
> `main` or a later stable release.

A correctness and hardening patch: a full three-pass audit (plan, code, CI/CD)
produced 69 verified findings, and every one is closed, test-pinned, and
independently re-verified in this release. No new macOS features; the payoff is
that the paths you already use are safer, and the release pipeline that ships
them proves more about itself. All inference stays local; no telemetry
(CI-enforced).

## Fixes (macOS product)

- **Terminal lines ending in a shell operator no longer crash the run loop**
  (`foo bar |` panicked the classifier).
- **Accepts can no longer race the field.** The AxSet write path re-reads the
  field value and selection immediately before writing and refuses when either
  moved — including selection-length-only movement — instead of overwriting
  text the app changed underneath.
- **Disarm ordering fixed:** the accept hotkey resource is released only after
  the armed action clears, so a keystroke in the teardown window can no longer
  fire a stale accept.
- **Clipboard restoration is teardown-owned:** an interrupted accept restores
  your clipboard on exit through a change-count-guarded coordinator instead of
  racing a fixed timer.
- **Worker panics are contained.** The AX, inference, and event workers wrap
  each job; a panic logs, answers the caller with a typed error, and the next
  job runs — a single bad AX call no longer silently kills the session.
- **Shutdown is bounded.** Quit tears down within a 250 ms inference bound
  backed by a watchdog (exit code 70) instead of hanging on a wedged native
  call.
- **Model downloads are single-origin and self-cleaning:** HTTPS-only
  (redirect downgrades refused), verification reads the exact bytes it
  downloaded through a held handle, and a failed download deletes its
  multi-gigabyte `.part` so retries start fresh and the typed error
  (hash mismatch, size cap) survives cleanup.

## Reliability and supply chain

- **Every workflow step that matters is now machine-pinned** — job
  permissions, action SHAs, credential scrubbing, attestation scoping to the
  release workflow, runner choices, doc-test and portable-test lanes — and a
  mutation self-test proves each pin bites before every commit.
- **Tag validation now runs at least everything a push to `main` runs**,
  including the live Linux suite and the portable doc tests; the published
  artifact is re-verified end-to-end by a closing `post_verify` job (download,
  checksum, provenance attestation, `brew install`, Gatekeeper, startup
  smoke).
- **`llama-cpp-2` is vendored** with a minimal abort-lifetime patch (upstream
  license texts included) and both the workspace and the spike validation
  lane build the same vendored copy.
- Dependency refresh: ureq 3, aes-gcm 0.11, rusqlite 0.40.2, zbus 5.19;
  macOS lanes moved to the `macos-15` image with image-keyed caches.

## Experimental (Linux — not a supported product in this release)

The Linux adapter is now wired into the binary and live-verified end to end on
X11: AT-SPI2 field read/insert with fail-closed guards, focus/caret events with
a single field-identity authority, a caret-tracking ghost overlay, an
exact-modifier X11 accept tap that survives keyboard-layout changes and honors
persisted rebinds, zenity confirmation, Secret Service keyring, and
FileManager1 reveal — 36 live harness tests plus a first human manual-testing
round. Wayland rendering/interception remains future work by design.

## Upgrade

```sh
brew update && brew upgrade --cask compme
```

Settings, statistics, and personalization data are unchanged; no migration.
