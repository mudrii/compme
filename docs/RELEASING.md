# Releasing

Compme ships as a Developer-ID signed and notarized macOS `.app` bundle published
to GitHub Releases and installed through a Homebrew cask. GitHub Actions runs the
root macOS checks and release build on Apple Silicon `macos-14` runners. Branch
CI also tests the portable workspace and app binary on Windows and Linux; tag
validation runs scoped adapter-crate jobs on those platforms.

> **Release boundary (2026-07-10):** this page describes the pipeline on current
> `main` for the next tag. The latest published artifact is `v0.1.4` at
> `18b8dc0`; its arm64 app is Developer-ID signed, hardened-runtime enabled,
> notarized, and stapled. The fail-closed signing policy and local/manual-only A2
> policy described below landed after that tag. The `v0.1.4` workflow still
> contained the earlier optional unsigned fallback and automated A2 release
> checks, although the published artifact itself used the signed/notarized path.

## Pipelines

| Workflow | Trigger | What it does |
|----------|---------|--------------|
| [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) | push to `main` / `spike/**`, PR, or `workflow_dispatch` | Root gates: `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace --all-targets -- --test-threads=1`, `cargo build --locked --workspace --all-targets`, `cargo build --locked -p platform_macos --examples`, plus non-A2 acceptance/bundle/release script syntax, bundle metadata/version check + self-test (`tools/bundle/check-bundle-metadata.sh` and `tools/bundle/check-bundle-metadata.sh --self-test`), release-version validator self-test, Homebrew cask Ruby syntax (`ruby -c Casks/compme.rb`), bundle assembler self-test (`tools/bundle/make-app.sh --self-test`), bundle smoke + self-test (`tools/bundle/bundle-smoke.sh` and `tools/bundle/bundle-smoke.sh --self-test`), UI-assisted session self-test (`tools/acceptance/run-ui-assisted-session.sh --self-test`), A1b/E2E self-tests, agent-brief alignment + self-test (`tools/release/check-agent-briefs.sh` and `tools/release/check-agent-briefs.sh --self-test`), privacy policy + self-test (`tools/release/check-privacy-policy.sh` and `tools/release/check-privacy-policy.sh --self-test`), missing-model startup self-test + product smoke (`tools/acceptance/missing-model-startup.sh --self-test` and `tools/acceptance/missing-model-startup.sh`), model-client feature policy + self-test (`tools/release/check-model-client-features.sh` and `tools/release/check-model-client-features.sh --self-test`), release model-gate policy, model-gate self-test (`tools/release/run-model-gates.sh --self-test`), cask-updater self-test (`tools/release/update-cask.sh --self-test`), cask-finalizer self-test (`tools/release/finalize-cask.sh --self-test`), notarization helper self-test (`tools/release/notarize-app.sh --self-test`), and update-manifest self-test (`tools/release/write-update-manifest.sh --self-test`). Spike gates: `cargo fmt -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`, `cargo build --locked --bins` in `tools/spike`. Windows/Linux portability jobs fmt the workspace, clippy/test the workspace excluding `platform_macos`, and build the app binary through the target facade. |
| [`.github/workflows/release.yml`](../.github/workflows/release.yml) | protected stable tag `vX.Y.Z` | Preflight validates the stable-only `X.Y.Z` version with `validate-version.sh`, requires the tag commit to equal the current default-branch HEAD, and checks bundle metadata before starting expensive jobs. Release validation then runs serialized root fmt/clippy/test, [`tools/release/run-model-gates.sh`](../tools/release/run-model-gates.sh), root build, non-A2 acceptance/bundle/release script syntax + self-tests including the release-version validator, Homebrew cask Ruby syntax, spike fmt/clippy/test/build, and scoped Windows/Linux adapter fmt/clippy/test/build jobs. Only after all validation passes, a secretless prebuild job (read-only permissions, no `release` environment, no secrets) rechecks that the tag still equals default-branch HEAD, scrubs the checkout-persisted git credential, compiles cold without a build cache, verifies the binary contains exactly `arm64`, and uploads it. The protected `release` environment then gates the signing job, which runs no `cargo` and installs no Rust toolchain: it downloads the binary, restores its executable bit, re-verifies exact `arm64` before exposing signing secrets, registers a deterministic cleanup path, imports the Developer-ID certificate, assembles `Compme.app` with `COMPME_BUNDLE_SKIP_BUILD=1`, notarizes + staples it, and strictly deletes and verifies absence of the signing keychain before packaging. It then zips with `ditto`, computes sha256, writes the update manifest, and uploads the three artifacts. Publication and Homebrew finalization are separate protected-environment jobs so a cask failure can be retried without rebuilding or republishing. The publish job verifies the downloaded zip checksum, performs a late exact-default-tip check, refuses an existing release for the tag, creates a draft with `gh release create "$GITHUB_REF_NAME" --verify-tag --draft --generate-notes` plus exactly the three release files, and undrafts it. The dependent finalizer downloads and verifies the artifact again, freezes the tag-reviewed cask updater and version validator before switching to the default branch, and validates the resulting cask before committing it. |

Workflow permissions default to `contents: read`; only the publication and cask
finalization jobs receive `contents: write`, and both are gated by the protected
`release` environment. Both check out full history (`fetch-depth: 0`) so their
late tag/default-branch checks and the cask finalizer's ancestry proof use the
complete repository history.

A2 validation is local/manual-only. The automated workflows exclude
`tools/acceptance/run-a2-compat-gates.sh` and
`tools/release/check-a2-matrix-ledger.sh` from both execution and the generic
shell-syntax pass; `check-model-gates.sh` rejects their reintroduction.

Model-inference tests (`crates/model_client/tests/latency.rs` and the spike model
integration test) are `#[ignore]`d because they need a local GGUF, so branch/PR
CI remains hermetic. The Release workflow CPU-forces the root `model_client`
latency suite with `COMPME_MODEL_GPU_LAYERS=0`; the separate spike integration
test remains Metal/GPU-oriented. Hosted tag validation runs
[`tools/release/run-model-gates.sh`](../tools/release/run-model-gates.sh) with
`COMPME_REQUIRE_MODEL_TESTS=1`, `COMPME_REQUIRE_MODEL_CONTEXT=1`, and
`COMPME_REQUIRE_LATENCY_BUDGET=0`; it downloads and hash-verifies the base
Qwen2.5 GGUF and exercises functional model/context behavior without claiming a
meaningful performance measurement on a virtualized runner. The strict sub-500
ms budget remains a mandatory pre-tag command on a real, model-capable Mac with
the wrapper's default `COMPME_REQUIRE_LATENCY_BUDGET=1`. The
wrapper passes that verified model path into the spike integration test through
`COMPME_SPIKE_MODEL_PATH`, so the spike gate uses the same GGUF as the root
model-client gate.

The release workflow runs `run-a1b-live-gates.sh --self-test` only to pin the
runner and its 17 LOOK/manual checklist IDs. It does not execute a granted GUI
session or convert those pending checks into release passes; current live status
is tracked in [ACCEPTANCE.md](ACCEPTANCE.md)'s Manual/Live Gate Ledger.

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
   that profile.

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
     required reviewers — the signing, publication, and cask-finalization jobs
     declare `environment: release`. A cask-only retry therefore requires a new
     approval by design, preserving the human gate around each write-capable job.
     Signing/notarization secrets may live at repo or environment scope;
     environment scope keeps them away from non-release workflows.

   Tag releases fail closed if any Developer-ID or notarization secret is
   missing; there is no unsigned publication fallback.
2. Bump the version in `crates/app/Cargo.toml`, `tools/bundle/Info.plist`
   (both `CFBundleShortVersionString` and `CFBundleVersion` to the same value —
   `check-bundle-metadata.sh` enforces equality), and `Casks/compme.rb`
   (`version`). Refresh `Cargo.lock` so its `app` package entry records the same
   version (for example, run `cargo check -p app` once without `--locked`), then
   validate the version and both distribution metadata surfaces before committing:

   ```sh
   version="X.Y.Z"
   tools/release/validate-version.sh "$version"
   tools/bundle/check-bundle-metadata.sh
   ruby -c Casks/compme.rb
   ```

   Releases use one stable-only version contract: `X.Y.Z` in bundle, Cargo, and
   cask metadata, and `vX.Y.Z` for the tag. Hyphenated prereleases, build
   metadata (`+…`), leading-zero components, and additional components are
   rejected by the shared validator and release preflight. Apple bundle version
   metadata requires numeric components, so there is no prerelease-tag path.
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

   A2 is not an automated tag gate. For an explicit local/manual pre-release
   compatibility pass, run its matrix against the target apps and validate the
   produced ledger locally:

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

   Commit the TSV plus its per-row log files *before* running the local checker:
   it rejects staged-but-uncommitted evidence (the working tree must match HEAD
   for the ledger and its logs). The committed row logs matter: the checker
   rejects ledgers whose
   `log_path` entries are missing from the committed evidence checkout, logs that
   do not prove the expected app/domain/context behavior, stale ledgers older than
   `COMPME_A2_LEDGER_MAX_AGE_SECONDS` (default `86400`), and future-dated
   ledgers beyond `COMPME_A2_LEDGER_MAX_FUTURE_SKEW_SECONDS` (default `300`).

   Set `COMPME_A2_BROWSER_EXCLUDED_DOMAIN` to the host focused in the
   browser-exclude row. The `screen` row requires Screen Recording permission and
   visible text on the focused display so OCR can produce non-empty context.

4. Ensure the release commit is on the up-to-date default branch, then tag and
   push. Use a protected stable `vX.Y.Z` tag from the
   current default branch. The preflight fails before release validation if the
   tag is unprotected, outside that version subset, does not equal the current
   `origin/<default-branch>` HEAD, or does not match the bundle metadata. The
   secretless prebuild repeats the exact-HEAD check after validation so a
   default-branch advance cancels the release rather than signing an older
   commit. The publication job performs the same exact-tip check immediately
   before creating the draft, closing the later build/signing window.
   The cask finalizer refuses to update `main`
   if the tag commit is not an ancestor of the default branch or if the
   default-branch cask version has already moved past the tag version:

   ```sh
   git checkout main
   git pull --ff-only origin main
   version="X.Y.Z" # replace with the version from step 2
   tag="v$version"
   git tag "$tag"
   git push origin "$tag"
   ```

   The Release workflow builds and verifies an exact-arm64 binary before upload,
   re-verifies exact arm64 after download and before signing secrets are exposed,
   then Developer-ID signs, notarizes, staples, and packages it. The **Publish
   release** job creates a draft `vX.Y.Z` GitHub Release by running
   the equivalent of:

   ```sh
   gh release create "$tag" \
     --verify-tag \
     --draft \
     --generate-notes \
     "release-artifacts/compme-$version-macos.zip" \
     "release-artifacts/compme-$version-macos.zip.sha256" \
     "release-artifacts/compme-$version-update.json"
   ```

   It then undrafts the release. `gh release create` fails if the tag already has
   a release, and the workflow never uses an asset-upload overwrite/clobber path;
   existing release assets are therefore never replaced in place. In short:
   same-name release assets are never overwritten; a collision fails closed
   because create-only publication refuses the existing release for the tag.

   The release body is auto-generated from commits. When curated notes exist
   (e.g. [`docs/RELEASE-NOTES-v0.1.0.md`](RELEASE-NOTES-v0.1.0.md)), paste them
   above the generated list in the web UI or combine both sections into one file
   before using `gh release edit "$tag" --notes-file <combined-file>`; that option
   replaces the whole body rather than prepending text. Also refresh the README
   **Status** section if this is the first release.
5. After publication, the separate **Finalize Homebrew cask** job downloads the
   artifact again, verifies its checksum, and commits the cask sha256 back to the
   default branch. Before it checks out or pulls that mutable branch, the
   finalizer freezes `update-cask.sh` and `validate-version.sh` from the reviewed
   release tag; it then invokes those frozen helpers with explicit cask and
   artifact paths. Before committing, it verifies Ruby syntax plus the exact
   arm64 stanza, version, release URL, and artifact sha256.

   If branch protection blocks that bot push or you need to recover manually,
   use the guarded finalizer path from a checkout at the release tag commit so
   tag/version, default-branch ancestry, frozen-helper, and stale-version checks
   still run:

   ```sh
   version="X.Y.Z" # replace with the published version
   tag="v$version"
   git fetch origin main "refs/tags/$tag:refs/tags/$tag"
   git checkout --detach "$tag"
   artifact_path="$PWD/release-artifacts/compme-$version-macos.zip"
   GITHUB_SHA="$(git rev-parse "$tag^{commit}")" \
     tools/release/finalize-cask.sh \
       "$tag" \
       "$artifact_path" \
       "$version" \
       main
   ```

   Release helpers are strict about arity; use the documented command forms
   exactly, because unexpected positional arguments exit with usage.

   Retry boundaries are intentional:

   - If publication fails after draft creation but before undrafting, inspect
     `gh release view "$tag"` and `gh release list`, then delete the incomplete
     draft before rerunning **Publish release**. The create-only publication path
     refuses the existing release and never overwrites its assets in place.
   - If the release is already published and only cask finalization fails, rerun
     only the failed **Finalize Homebrew cask** job. Approve its protected
     `release` environment again; it re-downloads and re-verifies the published
     artifact without creating or uploading another release.

   Post-publish checklist:
   - `gh release view "vX.Y.Z"` (with the published tag substituted) shows all
     three assets (zip, `.sha256`,
     `-update.json`) and the release is not a draft.
   - `https://github.com/mudrii/compme/releases/latest` resolves to the tag.
   - `ruby -c Casks/compme.rb` reports `Syntax OK` on the finalized default branch.
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
artifact; both sides verify that it contains exactly the `arm64` architecture,
and the signing job runs no `cargo` at all. `make-app.sh` runs with
`COMPME_BUNDLE_SKIP_BUILD=1`, so no third-party build code ever executes on
the runner that holds the Developer-ID keychain or notarization credentials.
It fails closed if signing/notarization secrets are absent. Before import it
records a deterministic keychain path for the always-run cleanup; cleanup uses
that fixed fallback even after an earlier-step failure, treats deletion failure
as fatal, verifies the keychain file is absent, and only then clears the signing
environment values. The workflow submits the signed `.app` archive with
`xcrun notarytool submit --wait`, staples the ticket with `xcrun stapler
staple`, validates the staple, performs that strict keychain cleanup, and only
then packages the release zip.

## Updates

The release workflow uploads `compme-<version>-update.json` next to the zip and
checksum. The app's menu-bar **Check for Updates…** item opens the latest GitHub
Release, where that manifest, the notarized zip, checksum, and generated release
notes are available. A full in-app Sparkle/appcast client remains a later
upgrade; the current path is the GitHub-release-driven updater option from the
roadmap.

## Installing (for users)

Homebrew cask installation uses the published signed/notarized release artifact
and the checksum finalized by the tag workflow.

```sh
brew tap mudrii/compme https://github.com/mudrii/compme
brew install --cask compme
```

See the [README](../README.md#install) for the post-install Accessibility grant.
