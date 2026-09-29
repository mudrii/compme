# macOS release validation handoff — runbook and blocked-state (2026-09-29)

> **Later 2026-09-29 update:** the owner authorized local G7 implementation
> before the physical baseline. Implementation and Linux validation are now
> complete; native validation remains pending. See the
> [current plan](2026-09-29-g7-linux-implementation.md) and
> [independent audit](2026-09-29-g7-codex-validation.md).
> The blocked-state report below preserves the earlier snapshot; its statements
> that production G7 routing is unchanged are historical, not current status.

**Author:** Worker C (acceptance and release preparation), dispatched by the glm
coordinator session. **Host:** NixOS, no Mac access. **Method:** read-only — no
repository file was modified, no cargo invoked, no state-changing git command
run, no gate/acceptance script executed. CodeGraph was queried first per the
AGENTS.md rule (it surfaced Rust parse symbols; all shell argument parsing
cited below was read directly from the script sources). Every flag quoted below
was verified in the cited script's parsing code on working tree HEAD
`12d581555dcbcaa811654f9e4a98e998bf7383b9` (the tree includes the protected
15-file consolidation diff; the fence text in `docs/DEVELOPMENT.md` quoted here
is the working-tree version whose only delta is the 2252→2241 test-count
restamp).

**Coordinator integration (2026-09-29), first pass:** every `manual_gate` line
citation and the runner summary/pin citations were re-verified against
`tools/acceptance/run-a1b-live-gates.sh` and corrected where the worker's
citations had drifted (the `manual_gate` block is `:1114–1134`; the
`input-monitoring-revoked-carbon-accept` invocation is `:1135` with its
self-test branch at `:681` and its `321 full`/`321 word` pins at `:684/:686`;
the summary log line is `:1138`). The `sidebar-only-editor-assistant-look` row
(`:1129`) was restored. The worker's G20 status claim was independently
confirmed: G20 is **closed** (`Qfd.md:855`, closed 2026-09-17), so Qfd §22.3
item 4's "G20 is still open" is stale history, and the remaining §22.3 grounds
for holding G7 stand.

**Coordinator integration (2026-09-29), second pass — four independent-audit
fixes applied:**

1. **Step 1 pipeline no longer masks failure:** `check.sh | tee` without
   `pipefail` returns tee's exit status, so a gate failure would look green.
   The runbook now sets `pipefail`, creates the log directory first, and
   requires stopping on any nonzero status.
2. **Ledger-row accounting corrected:** Deliverable 1a now lists exactly the
   22 real `docs/ACCEPTANCE.md` ledger row IDs (`:802–823`) in ledger order,
   including `sidebar-only-editor-assistant-look` (row 16). The G7 baseline
   set and the scripted browser/popup companions are explicitly marked as
   cross-cutting groupings, **not** ledger rows.
3. **A2 committed-evidence sequence corrected:** `check-a2-matrix-ledger.sh`
   rejects an untracked ledger (`git ls-files --error-unmatch`,
   `:174–176`) and one whose working-tree content differs from HEAD
   (`git diff --quiet HEAD`, `:186–192`). Generating the TSV and running the
   checker alone is therefore wrong: the TSV and its referenced logs must be
   committed under the mandatory gate workflow before committed-ledger
   verification, or the step is recorded as blocked.
4. **Screen-context OCR Tier-4 row completed:** per `docs/ROADMAP.md:1299` the
   remaining work is "OCR quality/perf on a granted desktop + multi-display
   caret confirm"; the scripted `screen` submit-context smoke alone does not
   cover the row.

## Deliverable 1 — Coverage-to-gate map

### 1a. The 22 runner-pinned ledger rows (`docs/ACCEPTANCE.md` "Manual/Live Gate Ledger", `:802–823`, added 2026-06-10)

Ledger state as of this reading: **19 rows "never recorded"**, `encrypted-memory-all-monitored-live` **partial (2026-06-17)**, `grammar-fix-textedit-look` **scripted pass / physical residual (2026-07-07)**, `input-monitoring-revoked-carbon-accept` **pending/conditional**. Prerequisites common to all rows: unlocked macOS GUI session, Accessibility granted to the terminal app, no global Secure Input owner, current-candidate `compme` binary (`target/debug/compme`) built on that same tree. Priority marks (★) follow the handoff's priority order; row numbers are the ledger's own 1–22 order.

| Row | Gate row | (a) Prerequisites | (b) Command / manual action (flags verified in source) | (c) Expected observable on success | (d) Evidence destination |
|---|---|---|---|---|---|
| 1 ★ | `apps-policy-toggle-look` (`:1114`) | ≥2 app rows in Settings > Apps | Runner-emitted `MANUAL`: On/Tab/Mid/AC/GF columns don't overlap names; toggle Enabled and Grammar fix for the focused app | Suggestions/corrections visibly dismiss; `COMPME_*_APPS` config persists | Ledger row |
| 2 ★ | `personalization-pane-look` (`:1115`) | Settings > Personalization | Runner-emitted `MANUAL`: edit global instructions, sender identity, strength | Multi-line field commits (Return; Option-Return = newline), persists, and the next request uses updated steering without relaunch | Ledger row |
| 3 | `menu-bar-icon-look` (`:1116`) | Tray visible | Runner-emitted `MANUAL`: caret + double-chevron template image renders in light **and** dark menu bars; fallback title readable on decode failure | Correct tint in both appearances; no `CM…` text except decode failure | Ledger row |
| 4 | `shortcuts-recorder-look` (`:1117`) | Settings > Shortcuts | Runner-emitted `MANUAL`: click recorder, press e.g. ⇧F5 | Label `⇧F5`; log lines `recorder keyDown role=… keycode=… mask=…` and `carbon hotkey registered id=…`; live accept in TextEdit; Esc cancels / Down rejected; collision shows "In use — press another"; pane re-syncs on reopen | Ledger row |
| 5 ★ | `always-on-hotkeys-physical-look` (`:1118`) | Current build; shortcuts configured; TextEdit focused with a suggestion showing; **physical keyboard** | Emitted as `MANUAL` by the runner; action: press force-activate (e.g. ⇧F5), per-app toggle, global toggle, grammar-check **as physical presses, exactly once each**; this row is the physical half of the **G7 baseline** required by Qfd §22.4a before any Carbon marshal change | Each press dispatches (log `compme: carbon hotkey registered id=…` at arm, then the action's log line, e.g. force re-present / toggle state flip) without reopening Settings; bindings persist across relaunch | Per-gate log under runner `LOG_DIR` + product log excerpt; update the row in the `docs/ACCEPTANCE.md` ledger table (result/date/binary/commit/evidence) |
| 6 ★ | `setup-model-picker-look` (Setup single-location-control invariant) (`:1119`) | Settings → Setup | Emitted as `MANUAL`: verify **exactly one Show Models Folder** control and **no Reveal Model in Finder**; picker rows carry RAM-fit suffixes; Download uses the selected row; encumbered models prompt license click-through before fetch; dest-exists re-click verifies without re-fetch | Exactly one location control; `downloading <model> (<MB> MB)…` / `model downloaded to <path>…` log lines; blocked-download message under minimum RAM | Ledger row; ACCEPTANCE.md "Setup tab model picker LOOK gate" checklist |
| 7 ★ | `nine-tab-settings-walkthrough` (`:1120`) | Current build; tray → Settings open | Emitted as `MANUAL`: walk Setup, General, Personalization, Apps, Context, Emoji, Shortcuts, Statistics, About — controls fit, reflect state, live-apply where documented | Each pane matches the ACCEPTANCE.md "Nine-tab Settings walkthrough" checklist; no overlap/panic in `COMPME_DEBUG=1` log | Ledger row; `docs/MANUAL-VALIDATION.md` walkthroughs hold the detailed steps |
| 8 | `full-autocorrect-prose-code-look` (`:1121`) | `COMPME_FULL_AUTOCORRECT=1`; TextEdit + a code editor | Runner-emitted `MANUAL`: whole-word typo in TextEdit → accept macOS spelling offer → exact readback; repeat in code-editor main pane | Atomic replacement in prose; **no** correction offered in the code pane | Ledger row |
| 9 | `cross-app-previous-inputs-look` (`:1122`) | `COMPME_CROSS_APP_PREVIOUS_INPUTS=1`, `COMPME_DIAG_CONTEXT=1`; two supported apps | Runner-emitted `MANUAL`: Full-accept in app A, submit in app B | Diagnostic reports `sources=recent` without content; disabling the Context switch clears the ring so the old entry does not return on re-enable; same-app history stays isolated | Ledger row |
| 10 | `selection-thesaurus-look` (`:1123`) | `COMPME_THESAURUS_SELECTION=1`; TextEdit | Runner-emitted `MANUAL`: select exactly one word → banner; Down cycles; accept replaces exactly the selected range; then collapse/move selection | Stale offer cannot apply | Ledger row |
| 11 | `tray-external-links-look` (`:1124`) | Tray menu | Runner-emitted `MANUAL`: click Visit Website and Contact Support once each | Browser opens exactly the repository and `/issues/new` URLs, once, no duplicate launches | Ledger row |
| 12 ★ | `caret-marker-chromium-forks-calibration` (`:1125`) | Brave/Edge/Vivaldi; `COMPME_DEBUG=1` for caret diagnostics | Runner-emitted `MANUAL` checklist: type in each fork, compare ghost placement | Calibration decision: whether Brave/Edge/Vivaldi ghosts land one line low, recorded **before** touching `RECT_IS_LINE_BUNDLE_PREFIXES` | Ledger row + screenshots/log excerpts |
| 13 ★ | `caret-marker-chrome-marker` (`:1126`) | Google Chrome; `COMPME_DEBUG=1` | Runner-emitted `MANUAL` checklist: focus a Chrome textarea/content-editable, observe ghost | Ghost lands on the caret line with `MacosCaretRectSource::Marker` in diagnostics | Ledger row + `LOG_DIR` excerpt |
| 14 ★ | `caret-marker-chromium-marker` (`:1127`) | A Chromium build; `COMPME_DEBUG=1` | Runner-emitted `MANUAL` checklist: same as row 13 on Chromium | Ghost lands on the caret line with marker source in diagnostics | Ledger row |
| 15 ★ | `caret-marker-electron-marker` (`:1128`) | An Electron app (VS Code); `COMPME_DEBUG=1` | Runner-emitted `MANUAL` checklist: same as row 13 on Electron | Ghost lands on the caret line with marker source in diagnostics | Ledger row |
| 16 | `sidebar-only-editor-assistant-look` (`:1129`) | `COMPME_DEBUG=1`; VS Code, Cursor, and Windsurf open | Runner-emitted `MANUAL`: in each editor, the main editor pane must produce **no request**, while a positively labelled assistant/chat field must submit a request with `app_allows=true` | Zero requests from main editor panes across all three; assistant/chat fields submit normally | Ledger row + `COMPME_DEBUG=1` log excerpt |
| 17 ★ | `encrypted-memory-all-monitored-live` (residual) (`:1130`) | Disposable `COMPME_CONFIG` + `COMPME_MEMORY_PATH`; harmless marker text; supported editor + two supported browsers with detectable domains; **leave `COMPME_MEMORY` unset at launch so it cannot shadow the persisted picker** | Runner-emitted `MANUAL`: remaining legs are secure-input, snoozed policy transition, and volatile `pid:N` — confirm each adds **no** store rows. The erasure legs are the Apps memory-control procedure (see 1b) | `rows=0` after each block case (product log `decision=None` while snoozed; no rows under `pid:N` target) | Ledger row (already "partial"); record the three legs in the ACCEPTANCE.md prose entry ("Encrypted memory AllMonitored live gate") |
| 18 ★ | `grammar-fix-textedit-look` (`:1131`) | `COMPME_GRAMMAR_FIX=1`, `COMPME_GRAMMAR_CHECK_KEY=<trigger>`, `COMPME_GRAMMAR_ACCEPT_KEY=<accept>`; model loaded; TextEdit | Runner-emitted `MANUAL`: type `teh`, place caret in/after word, press trigger; verify underline + banner **without** focus change or swallowed accept keys; press grammar-accept → in-place exact replacement; move caret/edit → stale correction cannot apply | Thin underline + banner render; original word replaced in place with no duplicate suffix/left-fragment; stale refusal after caret move | Product log + observation; ledger row + ACCEPTANCE.md "Standalone Grammar-Fix LOOK Gate" section |
| 19 | `mirror-window-firefox-zen-look` (`:1132`) | Firefox and Zen; mirror-window mode enabled | Runner-emitted `MANUAL` | Ghost appears in the mirror window aligned to the focused field, never in the source window | Ledger row |
| 20 | `setup-needed-docs-arc-onboarding` (`:1133`) | Google Docs focused in Arc; Accessibility missing/unsupported element state | Runner-emitted `MANUAL` | Setup-needed onboarding appears naming the missing setup action; **no** completion request submitted | Ledger row |
| 21 | `multi-candidate-cycle-physical-look` (`:1134`) | `COMPME_CANDIDATES>1`; multi-candidate suggestion visible | Runner-emitted `MANUAL`: physical Down-arrow | Candidates cycle in order, wrap predictably; accept inserts the selected candidate | Ledger row |
| 22 | `input-monitoring-revoked-carbon-accept` (invoked `:1135`; helper `:418`) | Conditional: the runner scripts it **only** when read-only preflight shows Input Monitoring already revoked (`CGPreflightListenEventAccess()`; branch `:681`); otherwise a manual checklist item — revoke Input Monitoring while keeping Accessibility | Scripted branch runs full+word accept taps (self-test pins `321 full` / `321 word` at `:684/:686`); manual branch: physical accept with permission revoked | Accept behavior unchanged; recorded as a permission-state confirmation, not a production-path requirement | Ledger row ("pending / conditional") |

**Not ledger rows — cross-cutting groupings (do not count toward the 22):**

- **G7 baseline set (Qfd §22.4a)** — the before-picture grouping for the Carbon
  marshal change, composed of ledger row 5 (`always-on-hotkeys-physical-look`,
  physical) plus the runner's **scripted** accept/dismiss/cycle/rearm gates:
  `accept-tap-full` (`:1101`), `accept-tap-word` (`:1102`),
  `accept-tap-escape` (`:1103`, dismiss), `accept-tap-cycle`,
  `accept-tap-delayed-hide` (rearm/hide), `accept-insert-full`,
  `accept-insert-word`, `accept-insert-option-tab` (gate list `:1074–1107`),
  then physical UX confirmation of the same five keys per ACCEPTANCE.md
  "Manual Physical Carbon Gates". Prerequisites: row 5's plus hands off the
  keyboard during scripted runs (exact-match control checks retry up to
  `--retries`). Success: `SUMMARY controls=[Accept(Full)/Accept(Word)/Dismiss/Cycle]`
  with exit 0 per harness; physical runs show the exact control set and field
  contents (word-accept leaves remainder ghost; Esc suppresses until
  refocus/edit). Evidence: `LOG_DIR/accept-tap-*.log` (+`.attempt-N.log`
  retries) and a ledger note under row 5; this set is re-recorded after G7
  (Qfd §22.4c). Scripted-gate results live in the run log, not as new ledger
  rows.
- **Scripted browser/popup companions** — optional scripted gates, not ledger
  rows: Chrome/Safari focused
  (`tools/acceptance/run-a1b-live-gates.sh --skip-textedit --allow-incomplete
  --browser-pid <pid>`, parse `:130–133`) runs `caret-marker-browser-marker`;
  a writable no-rect target focused (`--popup-pid <pid>`, parse `:124–129`)
  runs `popup-fallback` (requires capability + `AxSet` insertion + readback
  proving the field changed). Results go to `LOG_DIR` per-gate logs.

### 1b. Additional manually recorded Tier-4 rows (`docs/ROADMAP.md` "Tier 4 — Live validation" table, plus cross-referenced residuals)

| Tier-4 row | Prerequisites | Command / action | Expected observable | Evidence destination |
|---|---|---|---|---|
| Compatibility matrix (13 rows) | PIDs for all 13 documented rows (TextEdit, Notes, Mail, Word, Safari, Chrome, Brave, browser-exclude, terminal-cmd, terminal-nlp, unsupported, clipboard, screen); Screen Recording permission for `screen` | `tools/acceptance/run-a2-compat-gates.sh --self-test` then the matrix procedure in ACCEPTANCE.md: `COMPME_A2_BROWSER_EXCLUDED_DOMAIN=<focused-host> COMPME_A2_LOG_DIR=<evidence_dir> COMPME_A2_MATRIX_TARGETS="row_id=pid,…" tools/acceptance/run-a2-compat-gates.sh matrix` (kind dispatch: `run-a2-compat-gates.sh:40`), then — **only after committing** — `tools/release/check-a2-matrix-ledger.sh <ledger>` (see Step 5 for the commit requirement) | TSV ledger with **no** skip/fail rows (evidence runs must not set `COMPME_A2_MATRIX_ALLOW_SKIP=1`); checker passes freshness/content/app-evidence assertions | `tools/acceptance/evidence/a2/<tag>-<timestamp>/a2-compat-matrix-*.tsv` + row logs, committed |
| Browser-domain extraction (Chrome/Brave legs) | Chrome and Brave focused; `browser-domain-exclude` additionally requires `COMPME_A2_BROWSER_EXCLUDED_DOMAIN=<focused-host>` | `tools/acceptance/run-a2-compat-gates.sh browser-domain-allow` / `browser-domain-exclude` with the probe PID | Host-only domain metadata accepted; exclusion proves `prefs_ok=false` blocks the focused domain | A2 evidence dir under `tools/acceptance/evidence/a2/` |
| A2 local-replacement physical confirmation (emoji / autocorrect / British English) | AxSet field (TextEdit); `COMPME_EMOJI=1` / `COMPME_AUTOCORRECT=1` / `COMPME_BRITISH_ENGLISH=1`; physical keyboard | Type `:smile` / `teh` / `color`, accept; plus the four **unexercised** suppression spot-checks (excluded app / snoozed / terminal shell-command line / tray disabled) | Token deleted (no `:smile😄`), replacement inserted atomically; suppression cases produce zero ghosts/requests | ACCEPTANCE.md "A2 Local-Replacement Live Gate" section record |
| B3 live macOS evidence (ROADMAP "Still open after this batch", 2026-09-23) | A focused element resolvable at focus time but not at insert time | Exercise a global insert against such a field from the A2 app matrix at a Mac | Global insert **refuses** (fail closed, `StaleField`), nothing posted | ACCEPTANCE.md A2 matrix prose / Qfd note |
| Apps memory-control acceptance procedure (native Apps-pane mode/erase LOOK; legs 1–6) | Disposable config + `COMPME_MEMORY_PATH`; unique harmless markers; two supported browsers; `COMPME_MEMORY` unset at launch | Manual legs inside Settings > Apps per ACCEPTANCE.md "Apps memory-control acceptance procedure": mode transitions incl. Off-restore, cancel-vs-confirm app delete, domain delete across two browsers, Erase All, erase-while-Off, buffered-text non-restoration | Counts move only when expected; cancelled ops change nothing; erased app/domain/global records and live previous-input contributions clear and cannot be restored by buffered pre-erase text | Recorded in the ACCEPTANCE.md ledger prose (the procedure's step 6 names this ledger); commit + versions + before/after counts |
| Terminal/iTerm AI-prompt tuning | Real agent prompts in Terminal/iTerm | `run-a2-compat-gates.sh terminal-cmd` / `terminal-nlp` with probe PID | Command-line blocked; natural-language allowed | A2 evidence |
| Screen-context OCR — quality/perf + multi-display caret confirm (ROADMAP `:1299`: "OCR quality/perf on a granted desktop + multi-display caret confirm") | Screen Recording permission; visible text; **a multi-display desktop** for the caret leg | Two parts: (1) `run-a2-compat-gates.sh screen` with probe PID (`COMPME_DIAG_CONTEXT=1` enabled by the gate) — a submit-context smoke only; (2) human assessment of OCR **quality and performance** on the granted desktop, plus caret/ghost confirmation **across multiple displays** (ghost lands on the display/field that holds the caret). Part 1 alone does **not** discharge this row | Non-empty OCR context reaches the submit path; quality/perf judged usable; caret confirmation holds on every display arrangement tested | A2 evidence + ACCEPTANCE.md prose recording both parts |
| Strength slider (6 stops) | Model loaded | Live before/after steering at multiple stops | Output steering visibly changes per stop | ACCEPTANCE.md prose |
| Trailing-space UX confirmation | TextEdit | Covered by scripted `e2e-compme-trailing-space` (exact `word-only` readback); optional manual leg inside the nine-tab walkthrough | Exact single-word trailing-space readback | Runner log + ledger note |

## Deliverable 2 — Executable Mac validation sequence

Run everything from the repo root at the **same commit/working tree** for the whole session (the handoff's same-candidate evidence rule). Paste order:

### Step 0 — Environment checks (manual, before anything)

1. Real Mac (Metal-capable), unlocked GUI session; Accessibility granted to the terminal app you will use. No app holding global Secure Input (no password fields focused). The runner's preflight enforces this (`check_preflight` at `run-a1b-live-gates.sh:499`; locked screen or a Secure Input PID is a `BLOCKER`, exit 2, unless `--force`).
2. Toolchain/tools on PATH: `cargo`, `go` (actionlint is a required tool for `check.sh` — a missing one fails the gate up front), `ruby`, `shellcheck` and `cargo-audit` (the only two whose absence *skips* lines with a note; see the `check.sh` header, `tools/dev/check.sh:1–11`).
3. Model artifact: `tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf` present, or network access for the sha-pinned download (`curl -L --fail --retry 3 --retry-delay 5` + shasum check, `run-model-gates.sh:248–251`).
4. TextEdit open with a **plain** editable document focused (rich TextEdit may list-indent a passed-through Option+Tab — both are valid non-consumption evidence).
5. Optional target apps installed for the scripted/manual rows: Chrome, a Chromium build, VS Code (Electron), Brave/Edge/Vivaldi, Firefox, Zen, Arc.

### Step 1 — Full Local Gate — **runner-executed**

```sh
bash -euo pipefail -c '
  mkdir -p .gate
  tools/dev/check.sh 2>&1 | tee ".gate/mac-full-$(date +%Y%m%d-%H%M%S).log"
' && echo "Full Local Gate: PASS" || {
  echo "Full Local Gate: FAIL — stop; do not continue to later steps" >&2
  exit 1
}
```

`bash -euo pipefail -c` makes the pipeline's status the gate's (`pipefail`; a bare `check.sh | tee` would report tee's status and mask a failure), `-e` turns that status into an immediate nonzero exit of the inner bash, and the `||` branch converts it into a real stop — `exit 1` ends the invoking shell, so a failed gate cannot be followed by later steps. `check.sh` itself extracts the ` ```sh ` fence under `## Full Local Gate` in `docs/DEVELOPMENT.md` and runs each line in order from the repo root in one shell (`check.sh:1–11`). On the Mac this includes: fmt/clippy, the parallel portable-crate tests, **the serial lane** `cargo test --locked -p platform_macos -p app --all-targets -- --test-threads=1` (`platform_macos` swaps process-global hotkey state and `app` mutates `COMPME_CONFIG`; never parallelize these), example builds, bundle smoke, all script self-tests including `tools/acceptance/run-a1b-live-gates.sh --self-test`, and **the model + quality gates** (`bash tools/release/run-model-gates.sh` and `bash tools/release/check-quality.sh` are fence lines). On Darwin, `run-model-gates.sh:266–275` enforces the strict latency budget by default (`COMPME_REQUIRE_LATENCY_BUDGET:-1` — do **not** set it to 0 here; that relaxation is only for hosted CI) and runs the Metal in-flight-cancellation test that only a real Mac may record as A32 evidence. First failure stops the gate (`check.sh` header). `.gate/` is the existing untracked evidence location. Exit 0 prints `check.sh: gate complete: N run, M skipped of T commands`.

**Dedup rule (handoff constraint):** if Step 1 is green on this candidate, the model/quality gates in Step 6 are **already covered** — do not rerun them. Rerun them standalone only if the tree changed after Step 1 or Step 1 had to be resumed after a fix.

### Step 2 — A1b scripted default gates — **runner-executed**

```sh
# TextEdit session (open TextEdit first):
tools/acceptance/run-a1b-live-gates.sh
```

Builds examples + `compme`, preflights lock/Secure Input, then runs the scripted default gate list (TextEdit read/inserts, caret marker, accept-insert-*, e2e-compme-*, accept-tap-*, overlay-presenter/-correction) and emits the 22 `MANUAL` checklist lines. Flags verified in the parse block (`run-a1b-live-gates.sh` usage `:33–69`, parsing `:108–177`): `--dry-run --force --skip-build --skip-textedit --skip-e2e --allow-incomplete --allow-manual --self-test --textedit-pid PID --popup-pid PID --browser-pid PID --timeout-ms MS (default 3000) --short-timeout-ms MS (default 1500) --retries N (default 3) --gate-pause-ms MS (default 1000) --log-dir DIR`. **Keep hands off the keyboard** — the accept-tap gates assert an exact control set. **Logs:** `tools/acceptance/logs/a1b-live-YYYYMMDD-HHMMSS/` (default, `:21`; override `A1B_LOG_DIR`/`--log-dir`) — `preflight.log` (`:500`) plus one log per gate, retryable gates as `.attempt-N.log`; the summary prints `logs=$LOG_DIR` (`:1138`). **Exit codes:** 0 clean; 1 = unresolved mandatory skips (`--allow-incomplete` only for intentional partial runs) or unresolved MANUAL rows (`--allow-manual` only *after* executing and recording them) (`:1143–1151`); 2 = usage/preflight. A product quit during first-decode warm-up exits 70 — per ACCEPTANCE.md "Expected exit codes" that is recorded as a **pass with the code noted**, not a failure, unless the quit came after the first completion.

### Step 3 — Scripted target-app parameters — **runner-executed**, per focused target

```sh
# Operator-required variable (replace before running; quoted so the shell
# never parses the placeholder as redirection):
TARGET_PID='<pid>'   # PID of the focused Chrome/Safari or popup target app

# Chrome or Safari focused on a text field:
tools/acceptance/run-a1b-live-gates.sh --skip-textedit --allow-incomplete --browser-pid "$TARGET_PID"
# A writable no-rect popup target focused:
tools/acceptance/run-a1b-live-gates.sh --skip-textedit --allow-incomplete --popup-pid "$TARGET_PID"
```

(the `--skip-textedit --allow-incomplete` shape is the documented optional-gate form in ACCEPTANCE.md).

### Step 4 — The 22 manual physical rows — **manual**, in the priority order of Deliverable 1

Work the ★ rows first: row 5 `always-on-hotkeys-physical-look` + the accept/dismiss/cycle/rearm set (G7 §22.4a baseline), the four caret-marker calibration rows (12–15), memory privacy residuals + Apps memory-control procedure, row 18 `grammar-fix-textedit-look`, row 7 nine-tab walkthrough, row 6 Setup single-location invariant, rows 1–2 Apps/Personalization. Use `COMPME_DEBUG=1` launches for log evidence (`COMPME_EMOJI=1 COMPME_DEBUG=1 cargo run -p app 2>&1 | tee /tmp/cm.log` per ACCEPTANCE.md "Live UI LOOK Gates"). Press each physical key **exactly once**. After each row is executed and recorded, a final unattended readiness run may pass `--allow-manual` to prove the checklist is dischargeable — omit it otherwise.

### Step 5 — A2 compatibility matrix + domain/terminal/screen probes — **runner-executed, then commit, then checker**

The ledger checker verifies **committed** evidence: it rejects an untracked TSV (`git ls-files --error-unmatch`, `check-a2-matrix-ledger.sh:174–176`) and one whose working-tree content differs from HEAD (`git diff --quiet HEAD`, `:186–192`). Generating the matrix and running the checker in the same breath against fresh uncommitted output **fails by design**. The correct sequence on the Mac:

```sh
# Operator-required variables (export before running; unset variables abort
# with a clear message instead of parsing placeholders as redirection):
export A2_TAG='<tag>'                  # evidence run tag (must replace)
export A2_EXCLUDED_HOST='<focused-host>'   # host of the focused browser (must replace)
export A2_MATRIX_TARGETS='row_id=pid,…'    # all 13 row_id=pid pairs (must replace)
export A2_LEDGER_PATH_FILE="/tmp/a2-ledger-path.$(id -u)"   # shared by both phases below

bash -euo pipefail -c '
  tools/acceptance/run-a2-compat-gates.sh --self-test
  tools/release/check-a2-matrix-ledger.sh --self-test

  # 1. Generate (never with COMPME_A2_MATRIX_ALLOW_SKIP=1):
  evidence_dir="tools/acceptance/evidence/a2/${A2_TAG}-$(date +%Y%m%d-%H%M%S)"
  COMPME_A2_BROWSER_EXCLUDED_DOMAIN="$A2_EXCLUDED_HOST" \
    COMPME_A2_LOG_DIR="$evidence_dir" \
    COMPME_A2_MATRIX_TARGETS="$A2_MATRIX_TARGETS" \
    tools/acceptance/run-a2-compat-gates.sh matrix

  # 2. Select exactly the one TSV the run wrote (POSIX-safe: set --/$#/$1 work
  #    in sh, bash, and zsh; a quoted glob would never expand and the checker
  #    takes a literal path, usage :5). Explicit if + exit: a bare
  #    "[ a ] && [ b ]" list evades errexit when the first test is false.
  set -- "$evidence_dir"/a2-compat-matrix-*.tsv
  if [ "$#" -ne 1 ] || [ ! -f "$1" ]; then
    echo "expected exactly one generated TSV under $evidence_dir, got $#" >&2
    exit 1
  fi
  printf "%s\n" "$1" > "$A2_LEDGER_PATH_FILE"
  echo "TSV ready for review+commit: $1"
'
```

**Step 3 — separate operator action (no script above performs it):** review
the TSV and the row logs it references (no skip/fail rows; content/app
evidence present), then `git add` and `git commit` the TSV **and its
referenced logs** under the mandatory gate workflow (AGENTS: a commit carries
its required gates; include any doc/ledger re-stamps in the same commit).

**Step 4 — only after that commit** (literal path from the generated file,
Bash again for identical behavior across login shells):

```sh
A2_LEDGER_PATH_FILE="/tmp/a2-ledger-path.$(id -u)" bash -euo pipefail -c '
  ledger="$(cat "$A2_LEDGER_PATH_FILE")"
  tools/release/check-a2-matrix-ledger.sh "$ledger"
'
```

If a commit is not authorized at that point (e.g. other gates still open on the same tree), record the A2 verification as **blocked-for-verification** with the evidence paths — do not report the checker as passed from uncommitted state.

### Step 6 — Model/quality gates standalone — **runner-executed**, only when not already covered by Step 1

```sh
bash tools/release/run-model-gates.sh
bash tools/release/check-quality.sh
```

Both accept only `--self-test` (any other argument → usage, exit 2: `run-model-gates.sh:229–231`, `check-quality.sh:272–275`). They verify/download the pinned GGUF (sha-checked, `:248–251`), run the CPU-forced latency suite, the Darwin-only Metal cancellation test, and the `tools/spike` integration suite (`:266–275`); `check-quality.sh:312` runs the quality corpus test against the 17-of-21 threshold baseline (`:11–13`). **Logs:** stdout; the cached model lands at `tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf`.

### Step 7 — Record

Update one row per gate in the `docs/ACCEPTANCE.md` Manual/Live Gate Ledger (result, date, binary/commit, evidence path), plus the prose sections for A2/memory/grammar residuals, and re-stamp any count/doc surface a fix touched. Record only observations actually made on the Mac; anything not executed stays pending.

## Deliverable 3 — Truthful blocked-state report (2026-09-29, this host)

**Baseline observed (read-only):** HEAD `12d581555dcbcaa811654f9e4a98e998bf7383b9` on `main`; 15 modified tracked files, +43/−153 (the authorized test-consolidation diff, uncommitted and protected — not touched by this worker); untracked `.gate/` evidence logs and `docs/TEST-CONSOLIDATION-2026-09-26.md` preserved.

**Locally runnable on NixOS (Linux Host Gate):** `tools/dev/check.sh --fence "Linux Host Gate"` — the portable workspace clippy/test/doc lanes with `app` serial, the `platform_macos` **cross-check only** (type-checks, never links or executes macOS code), and the host-agnostic script checks. **It was not run for this report** (worker was read-only, no cargo); its last recorded result — 44 commands, 1881 tests + one doctest, macOS all-targets cross-check pass — is the historical 2026-09-26 record in ROADMAP, and it validates that tree, not new work.

**Pending on this host / owed to a Mac or CI:** the Full Local Gate is blocked on Linux by the Apple-only `objc2` dependency, so the macOS-only fence lines stay owed: `platform_macos` tests and examples, the Swift icon generator and bundle smoke, the live `check-model-gates.sh`, the A1b runner self-test (macOS-shaped fixtures), the model-backed gates (including the Metal cancellation evidence only a real Mac may record), and the `tools/spike` gate. **Zero of the 22 ledger rows and zero Tier-4 live rows may be claimed from any NixOS run** — every one needs the granted GUI session, and the model rows additionally need real model artifacts; the A1b runner is structurally unusable here (its preflight exits 2 on `ioreg` being unavailable, `:504–507`). Unavailable means pending, never pass.

**G7 (Carbon main-thread marshal):** design of record is Qfd §22, deliberately **not landed** (§22.3: the validating gate has never run; the replaced code has no identified direct deterministic unit coverage of the native registration/Drop boundary; it swaps two load-bearing invariants on the core interaction; ~200–250 lines not compilable on this host). The §22.3 G20 item is **stale** — G20 closed 2026-09-17 (`Qfd.md:855`) — but the remaining grounds stand. The **exact missing prerequisite** (§22.4a) is a recorded physical baseline against the **current build**: `always-on-hotkeys-physical-look` plus accept/dismiss/cycle/rearm (Deliverable 1a row 5 + the G7 baseline-set grouping). Until that before-picture exists at a Mac, a post-change failure would be unattributable, so **production G7 routing remains unchanged** (ROADMAP authorized-queue item: "Awaiting recorded physical-key baseline"). Worker A's Batch 1 is correctly gated on this; cross-compilation alone is partial validation, not closure.

**Uncommitted consolidation diff:** 15 files awaiting the Full Local Gate (blocked here by `objc2`) and a green **native** macOS CI run verifying the inventory restamp 2252 → 2241 and executing the consolidated macOS tests. Until then it stays uncommitted and protected.

**Release authorization:** none is granted or claimed. Ready-to-tag requires recorded closure of all 22 runner-pinned macOS gates (Qfd F3), a green Full Local Gate, and the RELEASING.md pre-tag runbook; version bump, tagging, Developer-ID signing, notarization, publication, and cask finalization are a later release operation **explicitly not authorized by this development handoff**. No Mac or Windows live acceptance exists, none may be inferred from Linux runs, and nothing in this report records a result that was not observed.
