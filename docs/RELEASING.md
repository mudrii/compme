# Releasing

Compme ships as a macOS `.app` bundle published to GitHub Releases and installed
through a Homebrew cask after the first signed release. GitHub Actions runs the
root macOS checks and release build on Apple Silicon `macos-14` runners, and CI
also runs scoped Windows/Linux adapter jobs.

## Pipelines

| Workflow | Trigger | What it does |
|----------|---------|--------------|
| [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) | branch push / PR / tag `v*` | Root gates: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets -- --test-threads=1`, `cargo build --workspace --all-targets`, `cargo build -p platform_macos --examples`, plus acceptance/bundle/release script syntax, bundle metadata/version check + self-test (`tools/bundle/check-bundle-metadata.sh` and `tools/bundle/check-bundle-metadata.sh --self-test`), bundle assembler self-test (`tools/bundle/make-app.sh --self-test`), A1b/A2/E2E self-tests, missing-model startup self-test + product smoke (`tools/acceptance/missing-model-startup.sh --self-test` and `tools/acceptance/missing-model-startup.sh`), model-client feature policy + self-test (`tools/release/check-model-client-features.sh` and `tools/release/check-model-client-features.sh --self-test`), release model-gate policy, model-gate self-test (`tools/release/run-model-gates.sh --self-test`), cask-updater self-test (`tools/release/update-cask.sh --self-test`), cask-finalizer self-test (`tools/release/finalize-cask.sh --self-test`), notarization helper self-test (`tools/release/notarize-app.sh --self-test`), and update-manifest self-test (`tools/release/write-update-manifest.sh --self-test`). Spike gates: `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo build --bins` in `tools/spike`. |
| [`.github/workflows/release.yml`](../.github/workflows/release.yml) | tag `v*` | Runs release validation first: serialized root fmt/clippy/test/build, [`tools/release/run-model-gates.sh`](../tools/release/run-model-gates.sh), acceptance/bundle/release script syntax + self-tests, missing-model startup product smoke (`tools/acceptance/missing-model-startup.sh`), bundle metadata/version check + self-test (`tools/bundle/check-bundle-metadata.sh` and `tools/bundle/check-bundle-metadata.sh --self-test`), bundle assembler self-test (`tools/bundle/make-app.sh --self-test`), model-client feature policy + self-test (`tools/release/check-model-client-features.sh` and `tools/release/check-model-client-features.sh --self-test`), release model-gate policy, model-gate self-test (`tools/release/run-model-gates.sh --self-test`), cask-updater self-test (`tools/release/update-cask.sh --self-test`), cask-finalizer self-test (`tools/release/finalize-cask.sh --self-test`), notarization helper self-test (`tools/release/notarize-app.sh --self-test`), update-manifest self-test (`tools/release/write-update-manifest.sh --self-test`), spike fmt/clippy/test/build, and scoped Windows/Linux adapter fmt/clippy/test/build jobs. Only after all release validation jobs pass, imports the Developer-ID certificate, builds `Compme.app` via [`tools/bundle/make-app.sh`](../tools/bundle/make-app.sh) with hardened runtime, notarizes + staples it via [`tools/release/notarize-app.sh`](../tools/release/notarize-app.sh), zips it with `ditto`, computes the sha256, writes an update manifest, publishes a GitHub Release with the zip, `.sha256`, and manifest, then commits the finalized Homebrew cask checksum back to the default branch via [`tools/release/finalize-cask.sh`](../tools/release/finalize-cask.sh). |

Workflow permissions default to `contents: read`; only the publish job receives
`contents: write`. The publish job also checks out full history
(`fetch-depth: 0`) so the cask finalizer can prove the tag commit is on the
default branch before it writes the finalized checksum.

Model-inference tests (`crates/model_client/tests/latency.rs` and the spike model
integration test) are `#[ignore]`d because they need a local GGUF and a Metal GPU,
so branch/PR CI remains hermetic. The Release workflow runs
[`tools/release/run-model-gates.sh`](../tools/release/run-model-gates.sh) with
`COMPME_REQUIRE_MODEL_TESTS=1` and `COMPME_REQUIRE_MODEL_CONTEXT=1`; publishing a
tag therefore downloads and validates model presence/load/warm-up, and
hash-verifies the base Qwen2.5 GGUF before running the model-backed gates on the
macOS runner. Set
`COMPME_REQUIRE_LATENCY_BUDGET=1` when you also want backend-sensitive completion
determinism and the sub-500ms latency budget enforced on the current machine.

## Cutting a release

1. Ensure the repository has the release secrets:
   `COMPME_DEVELOPER_ID_P12_BASE64`,
   `COMPME_DEVELOPER_ID_P12_PASSWORD`, `COMPME_CODESIGN_IDENTITY`, plus one
   GitHub-runner notarization credential set accepted by
   [`tools/release/notarize-app.sh`](../tools/release/notarize-app.sh): either
   `COMPME_NOTARYTOOL_KEY_BASE64` + `COMPME_NOTARYTOOL_KEY_ID` +
   `COMPME_NOTARYTOOL_ISSUER`, or `COMPME_NOTARYTOOL_APPLE_ID` +
   `COMPME_NOTARYTOOL_PASSWORD` + `COMPME_NOTARYTOOL_TEAM_ID`. A
   `COMPME_NOTARYTOOL_KEYCHAIN_PROFILE` is supported by the helper for a
   preconfigured local keychain, but the GitHub-hosted workflow does not create
   that profile.
2. Bump the version in `crates/app/Cargo.toml`, `tools/bundle/Info.plist`
   (`CFBundleShortVersionString`), and `Casks/compme.rb` (`version`), then run
   `tools/bundle/check-bundle-metadata.sh`, commit, and push.
3. On a model-capable Mac with the local GGUF installed, run the ignored
   model-backed gates before tagging. The Release workflow runs
   `tools/release/run-model-gates.sh`, which downloads and hash-verifies the
   same model, runs the same gates again, and fails closed if the model cannot
   be fetched or verified:

   ```sh
   COMPME_REQUIRE_MODEL_TESTS=1 COMPME_REQUIRE_MODEL_CONTEXT=1 cargo test -p model_client --test latency -- --ignored --test-threads=1
   cd tools/spike
   COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored --test-threads=1
   cd ../..
   ```

4. Ensure the release commit is on the up-to-date default branch, then tag and
   push. The cask finalizer refuses to update `main` if the tag commit is not an
   ancestor of the default branch or if the default-branch cask version has
   already moved past the tag version:

   ```sh
   git checkout main
   git pull --ff-only origin main
   git tag v0.1.0
   git push origin v0.1.0
   ```

   The Release workflow builds, Developer-ID signs, notarizes, staples, packages,
   and publishes `compme-0.1.0-macos.zip`, `compme-0.1.0-macos.zip.sha256`, and
   `compme-0.1.0-update.json` to the `v0.1.0` GitHub Release.
5. Confirm the workflow's **Finalize Homebrew cask** step committed the cask
   sha256 back to the default branch. If branch protection blocks that bot push
   or you need to recover manually, run:

   ```sh
   tools/release/update-cask.sh v0.1.0
   git add Casks/compme.rb
   git commit -m "chore(release): cask v0.1.0"
   git push
   ```

   Release helpers are strict about arity; use the documented command forms
   exactly, because unexpected positional arguments exit with usage.

## Code signing / notarization

`make-app.sh` **ad-hoc** signs the bundle (`codesign -s -`) by default for local
source builds. Set `COMPME_CODESIGN_IDENTITY` to a Developer-ID Application
identity to produce a hardened-runtime, timestamped signature; optionally set
`COMPME_CODESIGN_ENTITLEMENTS` when a future release needs an entitlements file.

The tag workflow requires a Developer-ID `.p12` certificate and notarization
credentials in GitHub Secrets. It fails closed if those secrets are absent, then
submits the signed `.app` archive with `xcrun notarytool submit --wait`, staples
the ticket with `xcrun stapler staple`, validates the staple, and only then
packages the release zip.

## Updates

The release workflow uploads `compme-<version>-update.json` next to the zip and
checksum. The app's menu-bar **Check for Updates…** item opens the latest GitHub
Release, where that manifest, the notarized zip, checksum, and generated release
notes are available. A full in-app Sparkle/appcast client remains a later
upgrade; the current path is the GitHub-release-driven updater option from the
roadmap.

## Installing (for users)

Homebrew cask install is available only after the first signed `v*` release
publishes the artifact and finalizes the cask checksum. Until then, build from
source as described in the README.

```sh
brew tap mudrii/compme https://github.com/mudrii/compme
brew install --cask compme
```

See the [README](../README.md#install) for the post-install Accessibility grant.
