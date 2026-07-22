# Releasing

Compme ships as a Developer-ID signed and notarized macOS `.app` bundle published
to GitHub Releases and installed through a Homebrew cask. GitHub Actions runs the
root macOS checks and release build on Apple Silicon `macos-14` runners. Branch
CI also tests the portable workspace and app binary on Windows and Linux; tag
validation runs the same portable workspace and app-binary gates on those
platforms.

> **Release boundary (2026-07-13):** this page describes the pipeline on current
> `main` for the next tag. The latest published artifact is `v0.1.5` at
> `14ae81e`; its arm64 app is Developer-ID signed, hardened-runtime enabled,
> notarized, and stapled, and its zip carries a build-provenance attestation.
> The fail-closed signing policy and local/manual-only A2 policy described below
> are part of the `v0.1.5` workflow. (The earlier `v0.1.4` workflow still
> contained the optional unsigned fallback and automated A2 release checks,
> although that published artifact also used the signed/notarized path.)

## Pipelines

| Workflow | Trigger | What it does |
|----------|---------|--------------|
| [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) | push to `main` / `spike/**`, PR, or `workflow_dispatch` (`paths-ignore` skips docs-only pushes) | Root gates: `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace --exclude platform_macos --exclude app --all-targets` (parallel) and `cargo test --locked -p platform_macos -p app --all-targets -- --test-threads=1` (serial), `cargo build --locked --workspace --all-targets`, `cargo build --locked -p platform_macos --examples`, `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`, a Model-backed smoke gate (`COMPME_REQUIRE_LATENCY_BUDGET=0 bash tools/release/run-model-gates.sh` against the rust-cache-cached pinned GGUF), plus acceptance/bundle/release script syntax and error-severity shellcheck over `tools/**/*.sh`, bundle metadata/version check + self-test (`tools/bundle/check-bundle-metadata.sh` and `tools/bundle/check-bundle-metadata.sh --self-test`), version-docs check + self-test (`tools/release/check-version-docs.sh` and `tools/release/check-version-docs.sh --self-test`), release-version validator self-test, Homebrew cask Ruby syntax (`ruby -c Casks/compme.rb`), bundle assembler and icon-generator self-tests (`tools/bundle/make-app.sh --self-test` and `tools/bundle/make-icon.sh --self-test`), bundle smoke + self-test (`tools/bundle/bundle-smoke.sh` and `tools/bundle/bundle-smoke.sh --self-test`), UI-assisted session self-test (`tools/acceptance/run-ui-assisted-session.sh --self-test`), A1b/E2E self-tests, agent-brief alignment + self-test (`tools/release/check-agent-briefs.sh` and `tools/release/check-agent-briefs.sh --self-test`), privacy policy + self-test (`tools/release/check-privacy-policy.sh` and `tools/release/check-privacy-policy.sh --self-test`), read-only GitHub-governance checker self-test (`tools/release/check-github-governance.sh --self-test`), missing-model startup self-test + product smoke (`tools/acceptance/missing-model-startup.sh --self-test` and `tools/acceptance/missing-model-startup.sh`), model-client feature policy + self-test (`tools/release/check-model-client-features.sh` and `tools/release/check-model-client-features.sh --self-test`), release model-gate policy, model-gate self-test (`tools/release/run-model-gates.sh --self-test`), quality-gate self-test (`tools/release/check-quality.sh --self-test`), cask-updater self-test (`tools/release/update-cask.sh --self-test`), cask-finalizer self-test (`tools/release/finalize-cask.sh --self-test`), notarization helper self-test (`tools/release/notarize-app.sh --self-test`), and update-manifest self-test (`tools/release/write-update-manifest.sh --self-test`). Spike gates: `cargo fmt -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`, `cargo build --locked --bins` in `tools/spike`. Windows/Linux portability jobs clippy/test the workspace excluding `platform_macos` and build the app binary through the target facade (workspace fmt runs once on the macOS lane; rustfmt output is platform-independent); the Linux job also runs the pinned `cargo-audit`. A separate Ubuntu job runs `actionlint` (with shellcheck over inline `run:` blocks) across all workflows. |
| [`.github/workflows/audit.yml`](../.github/workflows/audit.yml) | Mondays at 06:17 UTC or `workflow_dispatch` | Two isolated Ubuntu jobs. The audit job installs `cargo-audit` 0.22.2 with `--locked` and audits `Cargo.lock`; the governance job runs `tools/release/check-github-governance.sh` live (read-only) against the repository settings, degrading to a warning when an endpoint requires the Administration permission that `GITHUB_TOKEN` cannot hold. Both have read-only contents permission (plus `issues: write` for failure notification), explicit timeouts, and do not cancel in-progress runs. On failure either job opens or updates a tracking issue so scheduled breakage is not lost to email. |
| [`.github/workflows/release.yml`](../.github/workflows/release.yml) | protected stable tag `vX.Y.Z` | Preflight validates the stable version, exact default-branch tip, and bundle metadata. Validation installs pinned `cargo-audit` 0.22.2 and runs root/spike gates and release self-tests plus the model-backed gates — the release model-gate wrapper and the corpus-based Model-quality gate (`tools/release/check-quality.sh`) — while separate Windows/Linux jobs run the portable-workspace clippy/test plus app-binary gates (fmt runs once on the macOS lane). Secretless prebuild produces an exact-arm64 binary. Protected signing downloads and re-verifies it, signs, notarizes, staples, deletes the signing keychain, packages the zip, then expands the final zip and requires exactly one top-level `Compme.app` that passes strict `codesign`, staple, and Gatekeeper assessment, and finally attests build provenance for the packaged zip. Publication verifies the downloaded zip's checksum and provenance attestation, writes the update manifest from that checksum, and creates a draft after an exact-tip check; it then re-fetches tag/default branch immediately before undraft, deleting the stale draft and failing on drift. The cask finalizer re-verifies checksum and attestation, then verifies its local zip against the published checksum asset before branch mutation. A closing `post_verify` job (`needs: finalize_cask`) downloads the published assets, verifies the zip against its checksum asset, installs the published cask with `brew install --cask compme`, assesses the installed app with strict codesign, staple, and Gatekeeper checks, and runs a bounded startup smoke. All jobs have explicit timeouts: the outer signing job allows 360 minutes, while the notary submission defaults to 60 minutes and can be raised with `COMPME_NOTARYTOOL_TIMEOUT` below the outer ceiling. |

CI (the Linux portability job) and tag validation install `cargo-audit` 0.22.2
with `--locked` and run `cargo audit` under `contents: read`. The weekly
dependency-audit workflow runs the same pinned command and additionally holds
`issues: write`, used only to open or update a tracking issue when a scheduled
run fails. No audit job needs `checks: write`. Every CI and release job has an
explicit timeout. The publish job verifies the downloaded zip checksum and its
build-provenance attestation before writing the update manifest and creating
the draft.

Dependabot opens weekly grouped PRs (one per ecosystem; see
[`.github/dependabot.yml`](../.github/dependabot.yml)) that bump pinned action
SHAs and Cargo deps. Each actions PR needs two manual follow-ups in the same
PR before the gates go green: re-pin the action-SHA allowlists in
`tools/release/check-model-gates.sh`, and hand-update the `# vX.Y.Z` trailing
comments on the `uses:` lines (dependabot rewrites only the SHA). Until both
land, `check-model-gates.sh` fails red — that fail-closed behavior is
intended, and the dependabot.yml header comment pins the same contract.

Workflow permissions default to `contents: read`; only the publication and cask
finalization jobs receive `contents: write`, and both are gated by the protected
`release` environment. Both check out full history (`fetch-depth: 0`) so their
late tag/default-branch checks and the cask finalizer's ancestry proof use the
complete repository history.
The workflow never overwrites an existing asset, but GitHub release assets are
still inside the trust boundary of privileged contents writers unless the
repository separately enables GitHub Immutable Releases.

**Current live-governance caveat (verified 2026-07-18):** `main` is covered by
the active `protect-main` ruleset, which blocks force-pushes and deletion but
requires no reviews or status checks, preserving the direct-to-`main`
workflow. The remaining gaps are accepted owner decisions: live settings allow
reviewer self-approval, administrator bypass, and unrestricted deployment
branches; repository Actions allow every action, required SHA pinning is off,
and the release-tag ruleset does not restrict tag creation. These controls
therefore do not provide an independent approval boundary. Run
`tools/release/check-github-governance.sh` for a read-only check of the live
settings; `--self-test` validates the checker without network access and runs
in CI/release validation. The live check runs weekly in the `governance` job
of `audit.yml`: endpoint reads that require the Administration permission are
skipped with warnings, since `GITHUB_TOKEN` cannot hold it, while the ruleset
and environment checks are always evaluated and fail — opening a tracking
issue — on regressions from the documented baseline (main ruleset intact,
reviewer requirement present, release-tag ruleset complete); the accepted gaps
above print as warnings. The remaining
hardening decision — protected-branch review versus the documented
direct-to-`main` workflow — stays with the owner and is tracked in
[ROADMAP.md](ROADMAP.md).

A2 validation is local/manual-only. The automated workflows never execute
`tools/acceptance/run-a2-compat-gates.sh` or
`tools/release/check-a2-matrix-ledger.sh` (the generic `bash -n` script-syntax
traversal only parses them); `check-model-gates.sh` rejects their automated
execution.

Model-inference tests (`crates/model_client/tests/latency.rs` and the spike model
integration test) are `#[ignore]`d because they need a local GGUF, so the default
`cargo test` lane on branch/PR CI stays hermetic; a separate Model-backed smoke
gate step runs the release wrapper per push with
`COMPME_REQUIRE_LATENCY_BUDGET=0` (functional load/complete/shutdown coverage on
the cached, pinned GGUF). The Release workflow CPU-forces the root `model_client`
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
runner and the runner-pinned manual gates listed in
[ACCEPTANCE.md](ACCEPTANCE.md). It does not execute a granted GUI session or
convert those pending checks into release passes; current live status is tracked
in [ACCEPTANCE.md](ACCEPTANCE.md)'s Manual/Live Gate Ledger.

## Cutting a release

1. Ensure the repository has the release secrets and variables:
   `COMPME_DEVELOPER_ID_P12_BASE64`,
   `COMPME_DEVELOPER_ID_P12_PASSWORD`, `COMPME_CODESIGN_IDENTITY`, plus one
   GitHub-runner notarization credential set:
   `COMPME_NOTARYTOOL_KEY_BASE64` + `COMPME_NOTARYTOOL_KEY_ID` +
   `COMPME_NOTARYTOOL_ISSUER` (the App Store Connect API key set — the only
   set the hosted workflow passes to the helper).
   [`tools/release/notarize-app.sh`](../tools/release/notarize-app.sh) itself
   also accepts `COMPME_NOTARYTOOL_APPLE_ID` + `COMPME_NOTARYTOOL_PASSWORD` +
   `COMPME_NOTARYTOOL_TEAM_ID` or a `COMPME_NOTARYTOOL_KEYCHAIN_PROFILE` for
   manual/local runs, but the hosted workflow does not read those secrets.

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
     declare `environment: release`. Signing/notarization secrets may live at
     repo or environment scope;
     environment scope keeps them away from non-release workflows. A cask-only
     retry re-enters the environment approval step, but approval independence
     depends on the live self-review, admin-bypass, and deployment-branch
     settings described above.

   Tag releases fail closed if any Developer-ID or notarization secret is
   missing; there is no unsigned publication fallback.
2. Bump the version in the root `Cargo.toml` (`[workspace.package]` — every
   crate, including `app`, inherits it via `version.workspace = true`) and
   `tools/bundle/Info.plist`
   (both `CFBundleShortVersionString` and `CFBundleVersion` to the same value —
   `check-bundle-metadata.sh` enforces equality). Do NOT bump `Casks/compme.rb`
   here: the published cask keeps serving the previous release (its `version`
   and `sha256` stay a consistent pair) until the cask-finalization job rewrites
   both lines from the published artifact, so `brew install --cask compme` keeps
   working throughout the release. Refresh `Cargo.lock` so its package entries record the same
   version (for example, run `cargo check --workspace` once without `--locked`), then
   validate the version and bundle metadata before committing:

   ```sh
   version="X.Y.Z"
   tools/release/validate-version.sh "$version"
   tools/bundle/check-bundle-metadata.sh
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

   Run the corpus-based model-quality gate alongside it. The gate reuses the
   same pinned, hash-verified GGUF, runs every case in
   `tools/release/quality-corpus.jsonl` through the real model, and fails when
   the corpus pass rate drops below the 80% pass threshold:

   ```sh
   bash tools/release/check-quality.sh
   ```

   The release `validate` job runs the same command after the model-backed
   release gates, so quality drift fails the pipeline even when skipped
   locally.

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
   before creating the draft, then fetches the default branch and tag again
   immediately before undraft. Drift deletes the stale draft and fails closed.
   The cask finalizer refuses to update `main`
   if the tag commit is not an ancestor of the default branch or if the
   default-branch cask version is neither the tag version nor the previous
   release tag's version (a stale or out-of-order cask):

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
   then Developer-ID signs, notarizes, staples, and packages it. The final zip
   is expanded, required to contain exactly one top-level `Compme.app`, and
   reassessed with strict codesign, staple, and Gatekeeper checks. The **Publish
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

   It repeats the exact-tip/tag check and then undrafts the release.
   `gh release create` fails if the tag already has
   a release, and the workflow never uses an asset-upload overwrite/clobber path;
   existing release assets are therefore never replaced in place. In short:
   same-name release assets are never overwritten; a collision fails closed
   because create-only publication refuses the existing release for the tag.

   The release body is auto-generated from commits (`--generate-notes`), and
   those GitHub-generated notes are the policy going forward — v0.1.3 and later
   shipped on them alone, with no hand-written file. The curated
   [`docs/RELEASE-NOTES-v0.1.0.md`](RELEASE-NOTES-v0.1.0.md),
   `RELEASE-NOTES-v0.1.1.md`, and `RELEASE-NOTES-v0.1.2.md` files are historical
   pre-0.1.3 artifacts, kept for the record and no longer produced. Also refresh
   the README **Status** section if this is the first release.
5. After publication, the separate **Finalize Homebrew cask** job downloads the
   artifact again and commits the cask version and sha256 back to the default
   branch — until then the cask intentionally still names the previous release.
   The finalizer independently downloads the published release's
   `<zip>.sha256` with `gh`, requires its exact lowercase-SHA-256/filename format,
   and refuses a local artifact whose bytes do not match it. Before it checks out
   or pulls the mutable default branch, it also verifies the release tag commit
   and default-branch ancestry, then materializes `update-cask.sh` and
   `validate-version.sh` from that exact commit with `git show`; dirty
   working-tree copies are never executed. It invokes those frozen helpers with
   explicit cask and artifact paths. Before committing, it verifies Ruby syntax
   plus the exact arm64 stanza, version, release URL, and artifact sha256.

   If branch protection blocks that bot push or you need to recover manually,
   use the guarded finalizer path from a checkout at the release tag commit so
   tag/version, default-branch ancestry, frozen-helper, and stale-version checks
   still run:

   ```sh
   version="X.Y.Z" # replace with the published version
   tag="v$version"
   git fetch origin main "refs/tags/$tag:refs/tags/$tag"
   git checkout --detach "$tag"
   mkdir -p release-artifacts
   gh release download "$tag" \
     --repo mudrii/compme \
     --pattern "compme-$version-macos.zip" \
     --dir release-artifacts
   artifact_path="$PWD/release-artifacts/compme-$version-macos.zip"
   GITHUB_SHA="$(git rev-parse "$tag^{commit}")" \
     tools/release/finalize-cask.sh \
       "$tag" \
       "$artifact_path" \
       "$version" \
       main
   ```

   `gh` must already be authenticated, or `GH_TOKEN` must contain a token that
   can read the release. Do not supply a renamed or independently rebuilt zip:
   the finalizer requires the canonical filename and compares its bytes with the
   checksum asset downloaded directly from the published tag. The finalizer also
   rejects draft and prerelease state; publish/undraft the stable release only
   after verifying all three original assets, then run cask finalization.

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
     artifact without creating or uploading another release. There is no
     broken-cask window: until the job succeeds, the cask on `main`
     intentionally lags the release, still naming the previous version with its
     matching `sha256`, so `brew install --cask compme` keeps installing the
     previous, consistent release. The bundle-metadata check enforces exactly
     this contract — the cask version must equal the app version or the latest
     release tag's version.
   - If the Actions rerun window or workflow artifacts have expired, do not
     rebuild and upload new bytes under the existing protected tag. If a draft
     already contains all three original assets, download them, verify the zip
     against its published checksum, inspect the update manifest, and only then
     undraft it. If any original asset is absent or invalid, delete the draft and
     cut the next patch release from a new commit/tag. For a published release
     whose cask alone still needs recovery, download its zip as shown above and
     run the guarded finalizer; its checksum comes from the published release
     asset, not from the local file.

   Post-publish checklist:
   - Confirm the `post_verify` job (Post-publish install verification) is
     green: it downloads the published assets and verifies the zip against its
     checksum asset, installs the published cask with `brew tap mudrii/compme
     https://github.com/mudrii/compme && brew install --cask compme`, assesses
     `/Applications/Compme.app` with strict codesign, staple, and Gatekeeper
     checks, and runs a bounded startup smoke — the automated replacement for
     the old clean-machine install check.
   - One-line manual spot-check: `gh release view "vX.Y.Z"` (with the
     published tag substituted) shows all three assets (zip, `.sha256`,
     `-update.json`) and the release is not a draft.

   Recovering from a published bad release: do NOT retag — the protected tag
   cannot move, and the cask finalizer's stale-version guard refuses to re-bump
   `main` to a version it has already reached. For an urgent withdrawal, first
   mark the GitHub release as a draft (`gh release edit "$tag" --draft`) and
   remove or disable that version's cask on `main` through a reviewed commit so
   new installs stop. Preserve the tag and evidence for incident analysis. Then
   bump to the next patch version (e.g. `v0.1.1`) and run the complete release
   flow; never replace assets on the withdrawn tag.

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
then packages the release zip. It expands those final bytes and reruns
`codesign --verify --deep --strict`, `xcrun stapler validate`, and
`spctl --assess --type execute`, then attests the packaged zip's build
provenance (`actions/attest-build-provenance`) before artifact upload.

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
