# Releasing

Compme ships as a macOS `.app` bundle published to GitHub Releases and installed
through a Homebrew cask. Continuous integration and the release build both run
on GitHub Actions (Apple Silicon `macos-14` runners).

## Pipelines

| Workflow | Trigger | What it does |
|----------|---------|--------------|
| [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) | push / PR | `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets`. |
| [`.github/workflows/release.yml`](../.github/workflows/release.yml) | tag `v*` | Builds `Compme.app` via [`tools/bundle/make-app.sh`](../tools/bundle/make-app.sh), zips it with `ditto`, computes the sha256, and publishes a GitHub Release with the zip + `.sha256`. |

Model-inference tests (`crates/model_client/tests/latency.rs`) are `#[ignore]`d —
they need a local GGUF and a Metal GPU — so CI is hermetic. A few `platform_macos`
tests share the process-wide `NSPasteboard` and can flake under parallelism; just
re-run the job if that single test fails.

## Cutting a release

1. Bump the version in `crates/app/Cargo.toml` and `Casks/compme.rb` (`version`),
   commit, and push.
2. Tag and push:

   ```sh
   git tag v0.1.0
   git push origin v0.1.0
   ```

   The Release workflow builds, packages, and publishes
   `compme-0.1.0-macos.zip` (+ `.sha256`) to the `v0.1.0` GitHub Release.
3. Finalize the cask sha256 from the published artifact (the workflow prints the
   exact `version`/`sha256` to its run summary):

   ```sh
   tools/release/update-cask.sh v0.1.0
   git add Casks/compme.rb
   git commit -m "chore(release): cask v0.1.0"
   git push
   ```

## Code signing / notarization (human-gated)

`make-app.sh` **ad-hoc** signs the bundle (`codesign -s -`). That is enough to
run locally but Gatekeeper will quarantine a downloaded copy, so the cask tells
users to clear the quarantine flag. Proper Developer-ID codesigning +
notarization (and dropping that caveat) requires an Apple Developer account and
secrets in the repo — a deliberate, human-gated follow-up, not wired into CI.

## Installing (for users)

```sh
brew tap mudrii/compme https://github.com/mudrii/compme
brew install --cask compme
```

See the [README](../README.md#install) for the post-install Accessibility grant.
