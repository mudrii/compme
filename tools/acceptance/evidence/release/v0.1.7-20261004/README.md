# v0.1.7 preparation evidence — 2026-10-04

These checks ran on the local Apple M4 Max Mac, macOS 27, using Rust 1.99.0
(Homebrew). Native CI separately uses the repository's pinned Rust 1.97.0.
The base commit and built candidate binary hash are in `local-validation.json`.

- `tools/dev/check.sh`: 57 commands run, zero skipped; 2276 root tests passed,
  11 ignored. The strict CPU/Metal/spike model gates and quality gate passed.
  The final evidence update passed the same full gate again; both log hashes
  and selected command counts are in `local-validation.json`.
- All five native CI lanes passed preparation commit `aa199ea` in
  [run 37191288847](https://github.com/mudrii/compme/actions/runs/37191288847).
  Docs and CodeQL also passed that commit. These links identify the tested
  preparation commit; any later commit must pass its applicable checks.
- `tools/acceptance/run-a1b-live-gates.sh --skip-build --log-dir /tmp/compme-0.1.7-a1b-final`:
  21 scripted checks passed; one browser-marker target was unselected; 22
  manual rows remain open. The runner exited 1, correctly refusing overall
  acceptance. See `a1b-summary.log` for each unexecuted manual row.
- `launch-env-regression-{before,after}.log`: actual child-process environment
  tests for gate-setting forwarding and exclusion of inherited completion data.
- `trailing-space-{before,after}.log`: live TextEdit word-only readback failed
  before the harness fix and passed afterward with the exact trailing space.
- `secure-input-memory.log`: the bounded AllMonitored product loop reached
  Ready in a locked session, blocked Secure Input reads, issued no completion
  request and left zero encrypted-memory rows. This does not close the broader
  live memory/mode/erase gate.
- `gui-resume.json` and `native-chrome-caret.log`: resumed assisted observations
  after unlocking, the exact limits of the Chrome marker/fallback probes, and
  the source/binary provenance of the prepared pre-G7 physical baseline.
  No complete manual gate is claimed by these partial observations.

The candidate is locally ad-hoc signed. It is not a published/notarized 0.1.7
artifact. No physical keyboard or revoked Input Monitoring result is inferred
from scripted key posts. Follow `docs/ACCEPTANCE.md` and `docs/RELEASING.md` for
the remaining pre-tag and post-tag evidence.
