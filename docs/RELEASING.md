# Releasing

Compme ships as a macOS `.app` bundle published to GitHub Releases and installed
through a Homebrew cask after the first signed release. GitHub Actions runs the
root macOS checks and release build on Apple Silicon `macos-14` runners, and CI
also runs scoped Windows/Linux adapter jobs.

## Pipelines

| Workflow | Trigger | What it does |
|----------|---------|--------------|
| [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) | push to `main` / `spike/**`, PR, or `workflow_dispatch` | Root gates: `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace --all-targets -- --test-threads=1`, `cargo build --locked --workspace --all-targets`, `cargo build --locked -p platform_macos --examples`, plus acceptance/bundle/release script syntax, bundle metadata/version check + self-test (`tools/bundle/check-bundle-metadata.sh` and `tools/bundle/check-bundle-metadata.sh --self-test`), bundle assembler self-test (`tools/bundle/make-app.sh --self-test`), bundle smoke + self-test (`tools/bundle/bundle-smoke.sh` and `tools/bundle/bundle-smoke.sh --self-test`), UI-assisted session self-test (`tools/acceptance/run-ui-assisted-session.sh --self-test`), A1b/A2/E2E self-tests, A2 matrix ledger self-test (`tools/release/check-a2-matrix-ledger.sh --self-test`), agent-brief alignment + self-test (`tools/release/check-agent-briefs.sh` and `tools/release/check-agent-briefs.sh --self-test`), privacy policy + self-test (`tools/release/check-privacy-policy.sh` and `tools/release/check-privacy-policy.sh --self-test`), missing-model startup self-test + product smoke (`tools/acceptance/missing-model-startup.sh --self-test` and `tools/acceptance/missing-model-startup.sh`), model-client feature policy + self-test (`tools/release/check-model-client-features.sh` and `tools/release/check-model-client-features.sh --self-test`), release model-gate policy, model-gate self-test (`tools/release/run-model-gates.sh --self-test`), cask-updater self-test (`tools/release/update-cask.sh --self-test`), cask-finalizer self-test (`tools/release/finalize-cask.sh --self-test`), notarization helper self-test (`tools/release/notarize-app.sh --self-test`), and update-manifest self-test (`tools/release/write-update-manifest.sh --self-test`). Spike gates: `cargo fmt -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`, `cargo build --locked --bins` in `tools/spike`. |
| [`.github/workflows/release.yml`](../.github/workflows/release.yml) | protected tag `v*` | Runs release validation first: serialized root fmt/clippy/test/build, [`tools/release/run-model-gates.sh`](../tools/release/run-model-gates.sh), acceptance/bundle/release script syntax + self-tests, A2 matrix ledger self-test plus live proof via `COMPME_A2_MATRIX_LEDGER` under `tools/acceptance/evidence/a2/`, agent-brief alignment + self-test, privacy policy + self-test, missing-model startup product smoke (`tools/acceptance/missing-model-startup.sh`), bundle metadata/version check + self-test (`tools/bundle/check-bundle-metadata.sh` and `tools/bundle/check-bundle-metadata.sh --self-test`), bundle assembler self-test (`tools/bundle/make-app.sh --self-test`), bundle smoke + self-test (`tools/bundle/bundle-smoke.sh` and `tools/bundle/bundle-smoke.sh --self-test`), model-client feature policy + self-test (`tools/release/check-model-client-features.sh` and `tools/release/check-model-client-features.sh --self-test`), release model-gate policy, model-gate self-test (`tools/release/run-model-gates.sh --self-test`), cask-updater self-test (`tools/release/update-cask.sh --self-test`), cask-finalizer self-test (`tools/release/finalize-cask.sh --self-test`), notarization helper self-test (`tools/release/notarize-app.sh --self-test`), update-manifest self-test (`tools/release/write-update-manifest.sh --self-test`), spike fmt/clippy/test/build, and scoped Windows/Linux adapter fmt/clippy/test/build jobs. Only after all release validation jobs pass, a secretless prebuild job (read-only permissions, no `release` environment, no secrets) re-verifies tag ancestry, scrubs the checkout-persisted git credentials right after the tag-ancestry check, compiles the release binary cold (no build cache; third-party `build.rs`/proc-macro code runs only in this job, whose runner never holds signing credentials — a job boundary, since GitHub Actions does not kill background processes between steps of the same job), and uploads it as a workflow artifact. The protected `release` environment then gates the signing job, which runs no `cargo` and installs no Rust toolchain at all: it downloads the prebuilt binary (restoring the executable bit that `actions/download-artifact` drops), imports the Developer-ID certificate (deleting the decoded `.p12` immediately after import), assembles `Compme.app` via [`tools/bundle/make-app.sh`](../tools/bundle/make-app.sh) from the prebuilt binary with `COMPME_BUNDLE_SKIP_BUILD=1`, notarizes + staples it via [`tools/release/notarize-app.sh`](../tools/release/notarize-app.sh), deletes the signing keychain before packaging/upload, zips it with `ditto` and computes the sha256, writes an update manifest, and uploads those artifacts. A separate publish job then downloads the artifacts, verifies the zip against its `.sha256`, creates a draft GitHub Release with the zip, `.sha256`, and manifest, undrafts the release, then commits the finalized Homebrew cask checksum from the verified downloaded zip back to the default branch via [`tools/release/finalize-cask.sh`](../tools/release/finalize-cask.sh) — undraft runs first so a cask push never points at a draft-only URL. A hyphenated tag (`v1.2.3-rc.1`) is published as a GitHub *prerelease* (excluded from `/releases/latest`, the URL the in-app updater opens) and skips cask finalization entirely — Homebrew has no prerelease notion and would otherwise ship the rc to every `brew upgrade`. |

Workflow permissions default to `contents: read`; only the publish job receives
`contents: write`. The publish job also checks out full history
(`fetch-depth: 0`) so the cask finalizer can prove the tag commit is on the
default branch before it writes the finalized checksum.

Model-inference tests (`crates/model_client/tests/latency.rs` and the spike model
integration test) are `#[ignore]`d because they need a local GGUF, so branch/PR
CI remains hermetic. The Release workflow CPU-forces the root `model_client`
latency suite with `COMPME_MODEL_GPU_LAYERS=0`; the separate spike integration
test remains Metal/GPU-oriented. The Release workflow runs
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

1. Ensure the repository has the release secrets and variables:
   `COMPME_DEVELOPER_ID_P12_BASE64`,
   `COMPME_DEVELOPER_ID_P12_PASSWORD`, `COMPME_CODESIGN_IDENTITY`, plus one
   GitHub-runner notarization credential set accepted by
   [`tools/release/notarize-app.sh`](../tools/release/notarize-app.sh): either
   `COMPME_NOTARYTOOL_KEY_BASE64` + `COMPME_NOTARYTOOL_KEY_ID` +
   `COMPME_NOTARYTOOL_ISSUER`, or `COMPME_NOTARYTOOL_APPLE_ID` +
   `COMPME_NOTARYTOOL_PASSWORD` + `COMPME_NOTARYTOOL_TEAM_ID`. A
   `COMPME_NOTARYTOOL_KEYCHAIN_PROFILE` is supported by the helper for a
   preconfigured local keychain, but the GitHub-hosted workflow does not create
   that profile. The release repository variable `COMPME_A2_MATRIX_LEDGER` must
   point at the committed repo-relative TSV under `tools/acceptance/evidence/a2/`.

   **Producing the secrets** (first-time setup):
   - Developer-ID `.p12`: export the "Developer ID Application" certificate +
     private key from Keychain Access (or `security export`), then
     `base64 -i cert.p12 | pbcopy` → `COMPME_DEVELOPER_ID_P12_BASE64`; the
     export passphrase is `COMPME_DEVELOPER_ID_P12_PASSWORD`.
   - `COMPME_CODESIGN_IDENTITY`: the full identity string from
     `security find-identity -v -p codesigning`, e.g.
     `Developer ID Application: Your Name (TEAMID)`.
   - Notarytool API key (preferred set): create an App Store Connect API key
     (Users and Access → Integrations → App Store Connect API, role
     Developer+), download `AuthKey_<KEY_ID>.p8` once, then
     `base64 -i AuthKey_<KEY_ID>.p8` → `COMPME_NOTARYTOOL_KEY_BASE64`; the
     Key ID and Issuer ID shown on that page are `COMPME_NOTARYTOOL_KEY_ID`
     and `COMPME_NOTARYTOOL_ISSUER`. (Alternative Apple-ID set: an
     app-specific password from appleid.apple.com plus your Team ID.)

   **One-time repository setup** (the preflight fails closed without it):
   - Protected tag ruleset: Settings → Rules → Rulesets → new tag ruleset
     targeting pattern `v*` (this is what makes `github.ref_protected` true;
     an unprotected tag is rejected by the preflight).
   - `release` environment: Settings → Environments → create `release` with
     required reviewers — both the signing and publish jobs declare
     `environment: release`, so this is the human gate before any signing.
     Signing/notarization secrets may live at repo or environment scope;
     environment scope keeps them away from non-release workflows.

   **Interim unsigned mode:** with NO Developer-ID secret configured, the
   signing and notarization steps are skipped (gated on
   `COMPME_DEVELOPER_ID_P12_BASE64`; pinned by `check-model-gates.sh`) and the
   release ships an ad-hoc-signed bundle — Gatekeeper requires the user's
   explicit approval on first launch, and the cask caveats say so. A partial
   secret set still fails loud. Adding the full secret set later flips
   releases back to signed+notarized with no workflow change.
2. Bump the version in `crates/app/Cargo.toml`, `tools/bundle/Info.plist`
   (both `CFBundleShortVersionString` and `CFBundleVersion` to the same value —
   `check-bundle-metadata.sh` enforces equality), and `Casks/compme.rb`
   (`version`), then run `tools/bundle/check-bundle-metadata.sh`, commit, and
   push. Use a SemVer release version only: `X.Y.Z`, optionally with a
   prerelease suffix (build metadata `+…` tags are rejected by the preflight).
   The pushed tag must be `v<version>` and must match
   bundle metadata; the release preflight runs
   `COMPME_EXPECTED_VERSION="${GITHUB_REF_NAME#v}" tools/bundle/check-bundle-metadata.sh`.
3. On a model-capable Mac, run the release model-gate wrapper before tagging.
   It downloads and hash-verifies the GGUF when needed, runs the ignored
   model-backed gates, and fails closed if the model cannot be fetched or
   verified. Override the default model locally with `COMPME_MODEL_GATE_PATH`,
   `COMPME_MODEL_GATE_URL`, and `COMPME_MODEL_GATE_SHA256` when testing a
   different release model. In a GitHub tag-release context, those overrides are
   rejected unless `COMPME_ALLOW_MODEL_GATE_OVERRIDE=1` is also set for an
   intentional recovery run:

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
   release_tag="vX.Y.Z" # match the protected tag you will push
   run_id="$release_tag-$(date +%Y%m%d-%H%M%S)"
   evidence_dir="tools/acceptance/evidence/a2/$run_id"
   mkdir -p "$evidence_dir"
   COMPME_A2_BROWSER_EXCLUDED_DOMAIN="example.test" \
   COMPME_A2_LOG_DIR="$evidence_dir" \
   COMPME_A2_MATRIX_TARGETS="textedit=123 notes=124 mail=125 word=126 safari=127 chrome=128 brave=129 browser-exclude=130 terminal-cmd=131 terminal-nlp=132 unsupported=133 clipboard=134 screen=135" \
     tools/acceptance/run-a2-compat-gates.sh matrix
   ledger="$(ls -t "$evidence_dir"/a2-compat-matrix-*.tsv | head -n 1)"
   git add "$evidence_dir"
   git commit -m "test: A2 compatibility matrix evidence for $release_tag"
   tools/release/check-a2-matrix-ledger.sh "$ledger"
   ```

   Commit the TSV plus its per-row log files *before* running the checker: it
   rejects staged-but-uncommitted evidence (the working tree must match HEAD
   for the ledger and its logs). The tag release workflow fails
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
   push. Use a protected SemVer `v*` tag from the current default branch. The
   preflight fails before release validation if the tag is unprotected, not
   SemVer, not on `origin/<default-branch>`, or does not match the bundle
   metadata. The cask finalizer refuses to update `main` if the tag commit is
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
   `compme-0.1.0-macos.zip.sha256`, and `compme-0.1.0-update.json`, undrafts
   the release, then finalizes the cask checksum on the default branch. A
   hyphenated prerelease tag (`vX.Y.Z-rc.N`) is instead marked a GitHub
   prerelease and skips cask finalization, so `brew upgrade` and the in-app
   updater (`/releases/latest`) never pick it up.

   The release body is auto-generated from commits; prepend the curated notes
   (e.g. [`docs/RELEASE-NOTES-v0.1.0.md`](RELEASE-NOTES-v0.1.0.md)) after
   publish with `gh release edit v0.1.0 --notes-file docs/RELEASE-NOTES-v0.1.0.md`
   (or paste them above the generated list in the web UI). Also refresh the
   README **Status** section if this is the first release.
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

   Re-run recovery: a publish job that fails before the undraft step leaves
   the release drafted, but re-running can orphan a duplicate draft — check
   `gh release list` and delete any extra draft for the tag before re-running.

   Post-publish checklist:
   - `gh release view v0.1.0` shows all three assets (zip, `.sha256`,
     `-update.json`) and the release is not a draft.
   - `https://github.com/mudrii/compme/releases/latest` resolves to the tag.
   - On a clean machine: `brew tap mudrii/compme https://github.com/mudrii/compme
     && brew install --cask compme` installs the signed, notarized app.

   Recovering from a published bad release: do NOT retag — the protected tag
   cannot move, and the cask finalizer's stale-version guard refuses to
   re-bump `main` to a version it has already reached. Bump to the next patch
   version (e.g. `v0.1.1`) and run the release flow again.

## Code signing / notarization

`make-app.sh` **ad-hoc** signs the bundle (`codesign -s -`) by default for local
source builds. Set `COMPME_CODESIGN_IDENTITY` to a Developer-ID Application
identity to produce a hardened-runtime, timestamped signature; optionally set
`COMPME_CODESIGN_ENTITLEMENTS` when a future release needs an entitlements file.

The tag workflow requires a Developer-ID `.p12` certificate and notarization
credentials in GitHub Secrets. `target/release/compme` is compiled in a
separate secretless prebuild job and handed to the signing job as a workflow
artifact; the signing job runs no `cargo` at all and `make-app.sh` runs with
`COMPME_BUNDLE_SKIP_BUILD=1`, so no third-party build code ever executes on
the runner that holds the Developer-ID keychain or notarization credentials.
It fails closed if signing/notarization
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
