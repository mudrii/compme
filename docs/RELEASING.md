# Releasing

Compme ships as a macOS `.app` bundle published to GitHub Releases and installed
through a Homebrew cask. Continuous integration and the release build both run
on GitHub Actions (Apple Silicon `macos-14` runners).

## Pipelines

| Workflow | Trigger | What it does |
|----------|---------|--------------|
| [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) | branch push / PR / tag `v*` | Root gates: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets -- --test-threads=1`, `cargo build --workspace --all-targets`, plus acceptance script syntax and A1b/A2/E2E self-tests. Spike gates: `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo build --bins` in `tools/spike`. |
| [`.github/workflows/release.yml`](../.github/workflows/release.yml) | tag `v*` | Runs release validation first: serialized root fmt/clippy/test/build, acceptance script syntax + self-tests, and spike fmt/clippy/test/build. Only after validation passes, builds `Compme.app` via [`tools/bundle/make-app.sh`](../tools/bundle/make-app.sh), zips it with `ditto`, computes the sha256, and publishes a GitHub Release with the zip + `.sha256`. |

Model-inference tests (`crates/model_client/tests/latency.rs` and the spike model
integration test) are `#[ignore]`d because they need a local GGUF and a Metal GPU,
so GitHub-hosted CI remains hermetic. A release candidate must run the
`COMPME_REQUIRE_MODEL_TESTS=1` commands in [ACCEPTANCE.md](ACCEPTANCE.md) on a
model-capable Mac before tagging.

## Cutting a release

1. Bump the version in `crates/app/Cargo.toml` and `Casks/compme.rb` (`version`),
   commit, and push.
2. On a model-capable Mac with the local GGUF installed, run the ignored
   model-backed gates:

   ```sh
   COMPME_REQUIRE_MODEL_TESTS=1 cargo test -p model_client --test latency -- --ignored
   cd tools/spike
   COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored
   cd ../..
   ```

3. Tag and push:

   ```sh
   git tag v0.1.0
   git push origin v0.1.0
   ```

   The Release workflow builds, packages, and publishes
   `compme-0.1.0-macos.zip` (+ `.sha256`) to the `v0.1.0` GitHub Release.
4. Finalize the cask sha256 from the published artifact (the workflow prints the
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
