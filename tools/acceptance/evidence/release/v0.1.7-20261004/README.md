# v0.1.7 preparation evidence — 2026-10-04

These checks ran on the local Apple M4 Max Mac, macOS 27, using Rust 1.99.0
(Homebrew). Native CI separately uses the repository's pinned Rust 1.97.0.
The base commit and built candidate binary hash are in `local-validation.json`.

- `tools/dev/check.sh`: 57 commands run, zero skipped; 2276 root tests passed,
  11 ignored. The strict CPU/Metal/spike model gates and quality gate passed.
- `tools/acceptance/run-a1b-live-gates.sh --skip-build --log-dir /tmp/compme-0.1.7-a1b-final`:
  21 scripted checks passed; one browser-marker target was unselected; 22
  manual rows remain open. The runner exited 1, correctly refusing overall
  acceptance. See `a1b-summary.log` for each unexecuted manual row.
- `launch-env-regression-{before,after}.log`: actual child-process environment
  tests for gate-setting forwarding and exclusion of inherited completion data.
- `trailing-space-{before,after}.log`: live TextEdit word-only readback failed
  before the harness fix and passed afterward with the exact trailing space.

The candidate is locally ad-hoc signed. It is not a published/notarized 0.1.7
artifact. No physical keyboard or revoked Input Monitoring result is inferred
from scripted key posts. Follow `docs/ACCEPTANCE.md` and `docs/RELEASING.md` for
the remaining pre-tag and post-tag evidence.
