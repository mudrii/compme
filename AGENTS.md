# compme — Agent Brief

Inline text-completion engine (Rust). macOS ships first; Windows/Linux are
committed deliverables behind the shared PlatformAdapter contract.

## Objective & roadmap
- docs/ROADMAP.md is the single source of truth for pending work and status.
  Read it before starting non-trivial work; update it when you ship.
- Work from main, commit directly to main.
- All deterministic gates green before commit: cargo fmt, clippy, test.

## Conventions
- Minimal diffs, stdlib first, no speculative abstraction.
- Non-trivial logic ships with a test.

# Self-Learning

When I correct you or catch you making a mistake, before continuing add the lesson as a one-line rule under `# Lessons` so it never happens again.

# Lessons

- A release is not done until the workflow's final job (cask finalization) has run: after pushing a tag, follow the run through every environment approval and verify the published cask/checksum consistency end-to-end before reporting success.
- Cutting a release includes reconciling every doc that names the published version (README status/boundary, SECURITY supported release, ROADMAP anchors, release-boundary notes) in the same flow as the version bump — not as a follow-up when someone notices.
