# Releasing

Compme ships as a macOS `.app` bundle published to GitHub Releases and installed
through a Homebrew cask after the first signed release. GitHub Actions runs the
root macOS checks and release build on Apple Silicon `macos-14` runners, and CI
also runs scoped Windows/Linux adapter jobs.

## Pipelines

| Workflow | Trigger | What it does |
|----------|---------|--------------|
| [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) | branch push / PR | Root gates: `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace --all-targets -- --test-threads=1`, `cargo build --locked --workspace --all-targets`, `cargo build --locked -p platform_macos --examples`, plus acceptance/bundle/release script syntax, bundle metadata/version check + self-test (`tools/bundle/check-bundle-metadata.sh` and `tools/bundle/check-bundle-metadata.sh --self-test`), bundle assembler self-test (`tools/bundle/make-app.sh --self-test`), A1b/A2/E2E self-tests, A2 matrix ledger self-test (`tools/release/check-a2-matrix-ledger.sh --self-test`), agent-brief alignment + self-test (`tools/release/check-agent-briefs.sh` and `tools/release/check-agent-briefs.sh --self-test`), privacy policy + self-test (`tools/release/check-privacy-policy.sh` and `tools/release/check-privacy-policy.sh --self-test`), missing-model startup self-test + product smoke (`tools/acceptance/missing-model-startup.sh --self-test` and `tools/acceptance/missing-model-startup.sh`), model-client feature policy + self-test (`tools/release/check-model-client-features.sh` and `tools/release/check-model-client-features.sh --self-test`), release model-gate policy, model-gate self-test (`tools/release/run-model-gates.sh --self-test`), cask-updater self-test (`tools/release/update-cask.sh --self-test`), cask-finalizer self-test (`tools/release/finalize-cask.sh --self-test`), notarization helper self-test (`tools/release/notarize-app.sh --self-test`), and update-manifest self-test (`tools/release/write-update-manifest.sh --self-test`). Spike gates: `cargo fmt -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`, `cargo build --locked --bins` in `tools/spike`. |
| [`.github/workflows/release.yml`](../.github/workflows/release.yml) | protected tag `v*` | Runs release validation first: serialized root fmt/clippy/test/build, [`tools/release/run-model-gates.sh`](../tools/release/run-model-gates.sh), acceptance/bundle/release script syntax + self-tests, A2 matrix ledger self-test plus live proof via `COMPME_A2_MATRIX_LEDGER` under `tools/acceptance/evidence/a2/`, agent-brief alignment + self-test, privacy policy + self-test, missing-model startup product smoke (`tools/acceptance/missing-model-startup.sh`), bundle metadata/version check + self-test (`tools/bundle/check-bundle-metadata.sh` and `tools/bundle/check-bundle-metadata.sh --self-test`), bundle assembler self-test (`tools/bundle/make-app.sh --self-test`), model-client feature policy + self-test (`tools/release/check-model-client-features.sh` and `tools/release/check-model-client-features.sh --self-test`), release model-gate policy, model-gate self-test (`tools/release/run-model-gates.sh --self-test`), cask-updater self-test (`tools/release/update-cask.sh --self-test`), cask-finalizer self-test (`tools/release/finalize-cask.sh --self-test`), notarization helper self-test (`tools/release/notarize-app.sh --self-test`), update-manifest self-test (`tools/release/write-update-manifest.sh --self-test`), spike fmt/clippy/test/build, and scoped Windows/Linux adapter fmt/clippy/test/build jobs. Only after all release validation jobs pass, the protected `release` environment gates signing/publishing, scrubs the checkout-persisted git credentials right after the tag-ancestry check, prebuilds the release binary before any signing identity exists (so third-party build scripts never run with the keychain available), imports the Developer-ID certificate (deleting the decoded `.p12` immediately after import), assembles `Compme.app` via [`tools/bundle/make-app.sh`](../tools/bundle/make-app.sh) from the prebuilt binary with `COMPME_BUNDLE_SKIP_BUILD=1`, notarizes + staples it via [`tools/release/notarize-app.sh`](../tools/release/notarize-app.sh), deletes the signing keychain before packaging/upload, zips it with `ditto` and computes the sha256, writes an update manifest, and uploads those artifacts. A separate publish job then downloads the artifacts, verifies the zip against its `.sha256`, creates a draft GitHub Release with the zip, `.sha256`, and manifest, commits the finalized Homebrew cask checksum from the verified downloaded zip back to the default branch via [`tools/release/finalize-cask.sh`](../tools/release/finalize-cask.sh), then undrafts the release after the cask update succeeds. |

Workflow permissions default to `contents: read`; only the publish job receives
`contents: write`. The publish job also checks out full history
(`fetch-depth: 0`) so the cask finalizer can prove the tag commit is on the
default branch before it writes the finalized checksum.

Model-inference tests (`crates/model_client/tests/latency.rs` and the spike model
integration test) are `#[ignore]`d because they need a local GGUF and a Metal GPU,
so branch/PR CI remains hermetic. The Release workflow runs
[`tools/release/run-model-gates.sh`](../tools/release/run-model-gates.sh) with
`COMPME_REQUIRE_MODEL_TESTS=1`, `COMPME_REQUIRE_MODEL_CONTEXT=1`, and
`COMPME_REQUIRE_LATENCY_BUDGET=1`; publishing a tag therefore downloads and
validates model presence/load/warm-up, enforces backend-sensitive completion
determinism plus the sub-500ms latency budget, and hash-verifies the base
Qwen2.5 GGUF before running the model-backed gates on the macOS runner. The
wrapper passes that verified model path into the spike integration test through
`COMPME_SPIKE_MODEL_PATH`, so the spike gate uses the same GGUF as the root
model-client gate.

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
   (both `CFBundleShortVersionString` and `CFBundleVersion` to the same value —
   `check-bundle-metadata.sh` enforces equality), and `Casks/compme.rb`
   (`version`), then run `tools/bundle/check-bundle-metadata.sh`, commit, and
   push.
3. On a model-capable Mac, run the release model-gate wrapper before tagging.
   It downloads and hash-verifies the GGUF when needed, runs the ignored
   model-backed gates, and fails closed if the model cannot be fetched or
   verified. Override the default model with `COMPME_MODEL_GATE_PATH`,
   `COMPME_MODEL_GATE_URL`, and `COMPME_MODEL_GATE_SHA256` when testing a
   different release model:

   ```sh
   bash tools/release/run-model-gates.sh
   ```

   For debugging individual failures, the wrapper runs the root latency test
   with `COMPME_MODEL_GPU_LAYERS=0`, `COMPME_MODEL_CONTEXT_TOKENS=256`,
   `COMPME_REQUIRE_MODEL_TESTS=1`, `COMPME_REQUIRE_MODEL_CONTEXT=1`, and
   `COMPME_REQUIRE_LATENCY_BUDGET=1`, then runs the spike model integration test
   with `COMPME_SPIKE_MODEL_PATH` pointing at the same verified GGUF,
   `COMPME_REQUIRE_MODEL_TESTS=1`, and `COMPME_REQUIRE_LATENCY_BUDGET=1`.

   In the same live macOS session, run the A2 compatibility matrix against the
   required target apps into a committed evidence directory, then validate the
   produced ledger has every row passing before tagging:

   ```sh
   evidence_dir="tools/acceptance/evidence/a2/v0.1.0-$(date +%Y%m%d-%H%M%S)"
   mkdir -p "$evidence_dir"
   COMPME_A2_BROWSER_EXCLUDED_DOMAIN="example.test" \
   COMPME_A2_LOG_DIR="$evidence_dir" \
   COMPME_A2_MATRIX_TARGETS="textedit=123 notes=124 mail=125 word=126 safari=127 chrome=128 brave=129 browser-exclude=130 terminal-cmd=131 terminal-nlp=132 unsupported=133 clipboard=134 screen=135" \
     tools/acceptance/run-a2-compat-gates.sh matrix
   ledger="$(ls -t "$evidence_dir"/a2-compat-matrix-*.tsv | head -n 1)"
   git add "$evidence_dir"
   tools/release/check-a2-matrix-ledger.sh "$ledger"
   ```

   Commit the TSV plus its per-row log files. The tag release workflow fails
   closed unless `COMPME_A2_MATRIX_LEDGER` is set to that repo-relative TSV path
   under `tools/acceptance/evidence/a2/`, then validates it with the same
   checker. The committed row logs matter: the checker rejects ledgers whose
   `log_path` entries are missing on the GitHub runner, logs that do not prove
   the expected app/domain/context behavior, stale ledgers older than
   `COMPME_A2_LEDGER_MAX_AGE_SECONDS` (default `86400`), and future-dated
   ledgers beyond `COMPME_A2_LEDGER_MAX_FUTURE_SKEW_SECONDS` (default `300`).

   Set `COMPME_A2_BROWSER_EXCLUDED_DOMAIN` to the host focused in the
   browser-exclude row. The `screen` row requires Screen Recording permission and
   visible text on the focused display so OCR can produce non-empty context.

4. Ensure the release commit is on the up-to-date default branch, then tag and
   push. The repository must have a tag ruleset that protects `v*` tags; the
   preflight job exits before validation if GitHub reports the tag ref is not
   protected. The cask finalizer refuses to update `main` if the tag commit is
   not an ancestor of the default branch or if the default-branch cask version
   has already moved past the tag version:

   ```sh
   git checkout main
   git pull --ff-only origin main
   git tag v0.1.0
   git push origin v0.1.0
   ```

   The Release workflow builds, Developer-ID signs, notarizes, staples, packages,
   creates a draft `v0.1.0` GitHub Release with `compme-0.1.0-macos.zip`,
   `compme-0.1.0-macos.zip.sha256`, and `compme-0.1.0-update.json`, finalizes
   the cask checksum on the default branch, then undrafts the release.
5. Confirm the workflow's **Finalize Homebrew cask** step committed the cask
   sha256 back to the default branch. If branch protection blocks that bot push
   or you need to recover manually, use the guarded finalizer path from a
   checkout at the release tag commit so tag/version, default-branch ancestry,
   and stale-version checks still run:

   ```sh
   git fetch origin main
   GITHUB_SHA="$(git rev-parse v0.1.0)" \
     tools/release/finalize-cask.sh \
       v0.1.0 \
       "$PWD/release-artifacts/compme-0.1.0-macos.zip" \
       0.1.0 \
       main
   ```

   Release helpers are strict about arity; use the documented command forms
   exactly, because unexpected positional arguments exit with usage.

   Re-run recovery: if the publish job fails after the draft release was already
   created (for example the cask push raced a concurrent `main` commit),
   re-running it can leave an orphaned draft for the tag. Before re-running,
   check `gh release list` and delete any duplicate draft for that tag with
   `gh release delete <tag> ...` (target the draft, not a published release),
   then re-run. The release only goes public — the undraft step — after the cask
   finalize succeeds, so a failed run always leaves the release still drafted.

## Code signing / notarization

`make-app.sh` **ad-hoc** signs the bundle (`codesign -s -`) by default for local
source builds. Set `COMPME_CODESIGN_IDENTITY` to a Developer-ID Application
identity to produce a hardened-runtime, timestamped signature; optionally set
`COMPME_CODESIGN_ENTITLEMENTS` when a future release needs an entitlements file.

The tag workflow requires a Developer-ID `.p12` certificate and notarization
credentials in GitHub Secrets. It prebuilds `target/release/compme` before the
signing identity is imported; after import, `make-app.sh` runs with
`COMPME_BUNDLE_SKIP_BUILD=1` so no `cargo` build scripts execute while the
Developer-ID keychain is available. It fails closed if signing/notarization
secrets are absent, then submits the signed `.app` archive with
`xcrun notarytool submit --wait`, staples the ticket with `xcrun stapler
staple`, validates the staple, deletes the signing keychain and clears the
signing env values, and only then packages the release zip.

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
