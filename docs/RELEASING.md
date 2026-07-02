# Releasing

Compme ships as a macOS `.app` bundle published to GitHub Releases and installed
through a Homebrew cask. Continuous integration and the release build both run
on GitHub Actions (Apple Silicon `macos-14` runners).

## Pipelines

| Workflow | Trigger | What it does |
|----------|---------|--------------|
| [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) | branch push / PR / tag `v*` | Root gates: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets -- --test-threads=1`, `cargo build --workspace --all-targets`, plus acceptance/bundle/release script syntax, bundle metadata/version check, bundle assembler self-test (`tools/bundle/make-app.sh --self-test`), A1b/A2/E2E self-tests, missing-model startup self-test + product smoke (`tools/acceptance/missing-model-startup.sh --self-test` and `tools/acceptance/missing-model-startup.sh`), model-client feature policy (`tools/release/check-model-client-features.sh`), release model-gate policy, cask-updater self-test (`tools/release/update-cask.sh --self-test`), notarization helper self-test, and update-manifest self-test. Spike gates: `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo build --bins` in `tools/spike`. |
| [`.github/workflows/release.yml`](../.github/workflows/release.yml) | tag `v*` | Runs release validation first: serialized root fmt/clippy/test/build, [`tools/release/run-model-gates.sh`](../tools/release/run-model-gates.sh), acceptance/bundle/release script syntax + self-tests, missing-model startup product smoke (`tools/acceptance/missing-model-startup.sh`), bundle metadata/version check (`tools/bundle/check-bundle-metadata.sh`), bundle assembler self-test (`tools/bundle/make-app.sh --self-test`), model-client feature policy (`tools/release/check-model-client-features.sh`), release model-gate policy, cask-updater self-test, notarization helper self-test, update-manifest self-test, and spike fmt/clippy/test/build. Only after validation passes, imports the Developer-ID certificate, builds `Compme.app` via [`tools/bundle/make-app.sh`](../tools/bundle/make-app.sh) with hardened runtime, notarizes + staples it via [`tools/release/notarize-app.sh`](../tools/release/notarize-app.sh), zips it with `ditto`, computes the sha256, writes an update manifest, and publishes a GitHub Release with the zip, `.sha256`, and manifest. |

Model-inference tests (`crates/model_client/tests/latency.rs` and the spike model
integration test) are `#[ignore]`d because they need a local GGUF and a Metal GPU,
so branch/PR CI remains hermetic. The Release workflow runs
[`tools/release/run-model-gates.sh`](../tools/release/run-model-gates.sh) with
`COMPME_REQUIRE_MODEL_TESTS=1`; publishing a tag therefore downloads and
validates model presence/load/warm-up, and hash-verifies the base Qwen2.5 GGUF
before running the model-backed gates on the macOS runner. Set
`COMPME_REQUIRE_LATENCY_BUDGET=1` when you also want backend-sensitive completion
determinism and the sub-500ms latency budget enforced on the current machine.

## Cutting a release

1. Ensure the repository has the release secrets:
   `COMPME_DEVELOPER_ID_P12_BASE64`,
   `COMPME_DEVELOPER_ID_P12_PASSWORD`, `COMPME_CODESIGN_IDENTITY`, plus one
   notarization credential set accepted by
   [`tools/release/notarize-app.sh`](../tools/release/notarize-app.sh)
   (App Store Connect API key, keychain profile, or Apple-ID app-specific
   password).
2. Bump the version in `crates/app/Cargo.toml`, `tools/bundle/Info.plist`
   (`CFBundleShortVersionString`), and `Casks/compme.rb` (`version`), then run
   `tools/bundle/check-bundle-metadata.sh`, commit, and push.
3. On a model-capable Mac with the local GGUF installed, run the ignored
   model-backed gates before tagging. The Release workflow runs
   `tools/release/run-model-gates.sh`, which downloads and hash-verifies the
   same model, runs the same gates again, and fails closed if the model cannot
   be fetched or verified:

   ```sh
   COMPME_REQUIRE_MODEL_TESTS=1 cargo test -p model_client --test latency -- --ignored --test-threads=1
   cd tools/spike
   COMPME_REQUIRE_MODEL_TESTS=1 cargo test --test model_integration -- --ignored --test-threads=1
   cd ../..
   ```

4. Tag and push:

   ```sh
   git tag v0.1.0
   git push origin v0.1.0
   ```

   The Release workflow builds, Developer-ID signs, notarizes, staples, packages,
   and publishes `compme-0.1.0-macos.zip`, `compme-0.1.0-macos.zip.sha256`, and
   `compme-0.1.0-update.json` to the `v0.1.0` GitHub Release.
5. Finalize the cask sha256 from the published artifact (the workflow prints the
   exact `version`/`sha256` to its run summary):

   ```sh
   tools/release/update-cask.sh v0.1.0
   git add Casks/compme.rb
   git commit -m "chore(release): cask v0.1.0"
   git push
   ```

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

```sh
brew tap mudrii/compme https://github.com/mudrii/compme
brew install --cask compme
```

See the [README](../README.md#install) for the post-install Accessibility grant.
