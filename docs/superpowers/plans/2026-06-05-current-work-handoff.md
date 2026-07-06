# Current Work Handoff - 2026-06-05

> **⚠️ SUPERSEDED — historical snapshot (2026-06-05).** Point-in-time handoff; do not treat as current. Since written: the **post-flip grave→Full live GUI gates are CLOSED** (design spec §15 G6/I11, M4 Max 2026-06-08) — references below to "live grave-accept validation pending" / "fresh desktop rerun needed" are stale. The crate **`core` was renamed `engine_core`** (`crates/core` no longer exists). G3 (KV-cache reuse / persistent model actor) is **implemented and closed**. For current status see `docs/superpowers/specs/2026-06-03-engine-macos-mvp-design.md` §15 and the Project Scope note at its top (open-source, multi-platform, parity-minus-payment).

## Scope

This handoff covers the plan-review and spike-validation work completed so far in `/Users/mudrii/src/compme`, focused on A0 readiness, P3-P7 manual acceptance, the P6 overlay redo, the P2 model decision, plan corrections, and current go/no-go.

## Current Status

**A1a/A1b implementation is underway. Automated validation and the current live macOS TextEdit/browser/popup acceptance profiles passed for the pre-accept-key-flip baseline. The macOS adapter contract no longer has an unresolved architecture gate blocking development, but the post-flip grave->full live GUI gates and Chrome marker-path recheck remain explicit validation follow-ups.**

The platform-risk probes are no longer compile-only. `tools/spike/FINDINGS.md` records PASS evidence for P1-P7, and `tools/spike/MANUAL-ACCEPTANCE.md` records manual PASS evidence for P3, P4, P5, P5b, P6, and P7.

The remaining implementation guardrail is contract alignment: A1a must follow `docs/superpowers/plans/2026-06-04-a1b-macos-adapter-contract.md` instead of the older narrow platform trait snippets.

## A0 / Manual Acceptance Status

- **P1 build:** PASS, covered in `tools/spike/FINDINGS.md`.
- **P2 inference/model decision:** PASS. Warm 12-token instruct completion was previously documented at 35ms, and repeated validation runs on 2026-06-05 produced 34-36ms in `tests/model_integration.rs`. `p2_model_compare` then benchmarked instruct vs base across raw, FIM, and terse continuation prompts. A1a development default is `tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf` with the terse continuation prompt; empty-suffix FIM is rejected for now.
- **P3 AX read:** PASS. TextEdit, Safari address/search field, and Chrome textarea printed `caret=11 left_tail="hello world"` in manual acceptance.
- **P4 caret:** PASS. TextEdit, Safari, and Chrome textarea returned usable derived caret rects; TextEdit movement tracked correctly.
- **P5 tap:** PASS for the original tap substrate; binding-specific revalidation is pending. The active tap swallowed Tab when a suggestion was visible and passed other keys. The real accept key is Tab/keycode 48, not the original draft F8. **[CORR 06-07 → implemented 2026-06-07]** Decompile audit found Cotypist maps **Tab → next-word** and **grave/`~` → full**. Now implemented keycode-driven in `accept_tap_decision` (Tab/48→Word, grave/50→Full); the old Tab→Full + Option-modifier path was removed. Unit tests were green at the time; the live grave-accept validation note is historical and was later closed by the rebuilt Carbon/live acceptance gates. A best-effort `AXManualAccessibility` wake was also added at app-element creation so the Chromium/Electron web caret-marker path has markers to read (live `source=Marker` on Chrome pending).
- **P5b two-tap:** PASS. Listen-only observer plus consuming tap split was proven with simulated visibility toggled by F8 and Tab swallowed only while visible.
- **P6 overlay:** PASS after the 2026-06-05 redo. Details below.
- **P7 smoke:** PASS. With `SPIKE_AX_PID=<TextEdit pid>`, the flow read TextEdit prefix `Dear team, I wanted to `, caret `23`, generated completion `"ask about the \"C\""`, and CoreGraphics listed the onscreen layer-101 overlay near the caret.

## P6 Overlay Redo Evidence

P6 was redone on 2026-06-05 and is now accepted:

- Ran `tools/spike/target/release/p6_overlay` from the repo root.
- Screenshot `/tmp/compme-p6-redo-before.png` visually showed grey `ghost completion text` over a Chrome click target.
- CoreGraphics listed owner `p6_overlay`, onscreen, layer `101`, bounds `240x30`, alpha `1`, before and after the click.
- Frontmost-app checks did not switch focus to the overlay, proving the panel did not activate itself.
- Click-through was verified by placing a Chrome button under the overlay and clicking screen coordinate `{590,1185}` inside both overlay and button. Chrome title changed from `clicked-0` to `clicked-1`.
- Follow-up validation confirmed no `p6_overlay` process was left running.

Source-of-truth records:

- `tools/spike/MANUAL-ACCEPTANCE.md`
- `tools/spike/FINDINGS.md`
- `docs/superpowers/plans/2026-06-04-plan-review-online-validation.md`

## Files Changed So Far

New plan/docs files:

- `docs/superpowers/plans/2026-06-04-a1b-macos-adapter-contract.md`
- `docs/superpowers/plans/2026-06-04-plan-review-online-validation.md`
- `docs/superpowers/plans/2026-06-05-current-work-handoff.md`

New implementation crates:

- `crates/engine/`

Modified plan/docs files:

- `docs/superpowers/plans/2026-06-03-a0-spike.md`
- `docs/superpowers/plans/2026-06-03-a0-spike-tdd.md`
- `docs/superpowers/plans/2026-06-03-a1a-engine.md`

Modified spike files:

- `tools/spike/Cargo.lock`
- `tools/spike/Cargo.toml`
- `tools/spike/FINDINGS.md`
- `tools/spike/MANUAL-ACCEPTANCE.md`
- `tools/spike/src/bin/p2_infer.rs`
- `tools/spike/src/bin/p3_axread.rs`
- `tools/spike/src/bin/p4_caret.rs`
- `tools/spike/src/bin/p5_tap.rs`
- `tools/spike/src/bin/p7_smoke.rs`
- `tools/spike/tests/model_integration.rs`

New spike probe:

- `tools/spike/src/bin/p5_twotap.rs`

## Online Validation Already Captured

The plan review was validated against current documentation and records these documentation checks:

- `npx ctx7@latest library tauri "..."`
- `npx ctx7@latest docs /websites/v2_tauri_app "..."`
- Apple docs for `AXUIElementCreateSystemWide`, `AXUIElementCopyAttributeValue`, `AXValue`, `CGEventTapCreate`, and `NSApplicationActivationPolicy`.
- docs.rs references for `llama-cpp-2`, `accessibility-sys`, `core-graphics`, and `objc2-app-kit`.

Key result: the A1a engine plan was corrected to stay contract-first and avoid treating spike seam types or old API signatures as final product interfaces.

## Fresh Validation Run - 2026-06-05

Commands run from `/Users/mudrii/src/compme`:

```sh
cargo build --release --manifest-path tools/spike/Cargo.toml --bin p6_overlay
```

Outcome: PASS. Release `p6_overlay` binary built successfully.

```sh
cargo test --manifest-path tools/spike/Cargo.toml -- --nocapture
```

Outcome: PASS. Unit tests: 28 passed, 0 failed. Model integration: 1 passed, warm 12-token completion 34ms in the latest rerun. Doc tests: 0 tests, pass.

## Model Artifacts

Current local model files:

- `tools/spike/models/qwen2.5-0.5b-instruct-q4_k_m.gguf` - 469M, present since 2026-06-03.
- `tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf` - 379M, downloaded on 2026-06-05.

New base model source:

```sh
curl -L --fail --continue-at - \
  --output tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf \
  "https://huggingface.co/Brianpuz/Qwen2.5-0.5B-Q4_K_M-GGUF/resolve/main/qwen2.5-0.5b-q4_k_m.gguf?download=true"
```

Online verification notes: `Qwen/Qwen2.5-0.5B-GGUF` returned 401 unauthenticated during verification; `Brianpuz/Qwen2.5-0.5B-Q4_K_M-GGUF` is public, not gated, tagged as quantized from `Qwen/Qwen2.5-0.5B`, Apache-2.0, and served the 397,807,520-byte GGUF. Local SHA-256 begins `ca6f8885c1d6a14025e705295fe1b240ad5a30c4c696215a341d7e6610a26484`.

Benchmark result: both models were fast under the 500ms floor. The corrected repo-root `p2_model_compare` run labels the backend as `llama-cpp-2-metal`; base+terse totals were 51-52ms with `quality_flags=ok` for all 6 cases. Base+terse gave the best development-default tradeoff; instruct remains a fallback, but not the default. Empty-suffix FIM produced empty, corrupt Unicode, repeated-prefix, or off-topic output and is not selected.

## Exact Next Step

A1a implementation has started test-first against the reconciled A1b contract.

Completed A1a slices:

- Root Cargo workspace, currently listing only crates that exist so the workspace stays buildable.
- `crates/platform`: A1b-aligned platform contract scaffolding and `ux_mode` policy.
- `crates/context`: Unicode-safe left/right context, left-tail, and prompt-trim helpers.
- `crates/ranker`: candidate word capping, first-word split for partial accept, and repetition penalty.
- `crates/core`: deterministic `SuggestionMachine` for debounced requests, stale completion discard, hide invalidation, full accept, and word accept; events/commands now carry `FieldHandle` and snapshot IDs so completions and inserts are tied to the focused field.
- `crates/model_client`: fallible `LocalModel` seam, configurable terse continuation prompt, and `LlamaModel` backed by `llama-cpp-2` 0.1.146 with the selected base GGUF. Runtime llama failures now surface as `LocalModelError` instead of panicking during completion.
- `crates/platform_macos`: A1b macOS adapter implementation is unit-tested and live-validated for the default TextEdit profile, Safari marker profile, and repo-local popup fallback fixture, with one current caveat: the recorded live full-accept passes predate the Tab->word / grave->full binding correction and need a fresh desktop rerun. Implemented slices include the AX worker/run-loop resource model, focus/caret observer subscriptions with rebind and safety polling, secure-field and global Secure Input blocking, field ownership resolution, AX text context reads, native plus Chromium/WebKit marker-first caret geometry, `kAXErrorParameterizedAttributeUnsupported` handling as absent caret geometry for bounds queries, AxSet/SyntheticKeys/Clipboard insertion planning, stale-focus rejection before global event posting, eager pasteboard item/type snapshot restore, provider-backed pasteboard snapshot materialization, `changeCount`-guarded clipboard restore, capability-level popup fallback when no caret rect is available, the two-tap accept interception substrate with a permanent listen-only tap plus transient consuming tap for Tab/keycode 48, explicit precomputed `AcceptAction` gating, delayed teardown after synthetic insertion, and the AppKit-main-thread `MacosOverlayPresenter` for `show_ghost`/`update_ghost`/`hide` through a transparent click-through non-activating `NSPanel`. Production live harnesses exist for TextEdit focus/caret/read/insert, repo-local popup fallback fixture plus optional external `--popup-pid`, accept tap interception, marker-vs-fallback caret diagnostics, full/word accept insertion, and overlay presenter show/update/hide diagnostics. Live SyntheticKeys, Clipboard, AxSet, pre-flip accept tap inactive/full/word/delayed-hide, pre-flip full/word accept insertion, Safari Chromium/WebKit marker geometry, popup fallback, and overlay diagnostics passed. Native macOS inline prediction suppression is explicitly deferred out of A1b because AppKit only exposes `setAutomaticTextCompletionEnabled(false)` for owned text controls, while this app targets other applications through Accessibility and overlay rendering.

Note on `edit_kind` / delete suppression: the `edit_kind` field exists on `TextChanged` but `EditKind::Delete` suppression was not yet wired in `SuggestionMachine` at time of handoff — added as part of review fixes.

Fresh A1a validation:

```sh
cargo test --workspace --all-targets
```

Outcome: PASS. Workspace unit/integration/example tests passed with `--all-targets`, including the popup fallback example regression tests. `model_client` integration loaded `tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf` on Metal and remained under the 500ms latency floor. `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo build --workspace --all-targets` also pass.

Fresh live TextEdit acceptance:

- `COMPME_ACCEPTANCE_PID=82733 ./target/debug/examples/textedit_observer_acceptance 4000 focus`: PASS with `SUMMARY focus=1 caret=15`; first focus field was TextEdit `First Text View`.
- `COMPME_ACCEPTANCE_PID=82733 ./target/debug/examples/textedit_observer_acceptance 3000 caret`: PASS with `SUMMARY focus=1 caret=12`; caret events stayed on generation 1 for TextEdit `First Text View`.
- `COMPME_ACCEPTANCE_PID=82733 ./target/debug/examples/textedit_observer_acceptance 3000 read`: PASS with `READ caret=33 selection=None left=" targeted-focus read-context-live" right="" source=Accessibility encoding=Utf16CodeUnits` and `SUMMARY focus=1 caret=11`.
- `COMPME_ACCEPTANCE_PID=82733 ./target/debug/examples/textedit_observer_acceptance 3000 rect`: PASS with `RECT Some(ScreenRect { x: 822.083984375, y: 360.0, w: 1.0, h: 14.0 })` and `SUMMARY focus=1 caret=11`.
- `COMPME_ACCEPTANCE_PID=82733 ./target/debug/examples/textedit_observer_acceptance 3000 caps`: PASS with `CAPS readable_text=true readable_caret=true writable=true secure=false state=Normal toolkit=AppKit multiline=true insert=AxSet intercept=CgEventTap overlay=NativePanel coords_global=true`.
- `COMPME_ACCEPTANCE_PID=82733 ./target/debug/examples/textedit_observer_acceptance 3000 insert`: PASS with `INSERT bytes=16 chars=16 strategy=AxSet text=" cm-insert-41394"` and `POST_INSERT_READ caret=49 selection=None left=" targeted-focus read-context-live cm-insert-41394" right=""`.
- `COMPME_ACCEPTANCE_PID=82733 ./target/debug/examples/textedit_observer_acceptance 4000 caret`: PASS after focusing a TextEdit document and inserting ` callback-rect-live`, with repeated callback `rect=Some(ScreenRect { x: 597.689453125, y: 215.0, w: 1.0, h: 14.0 })`, direct `RECT Some(...)`, and `CAPS ... overlay=NativePanel`.
- `COMPME_ACCEPTANCE_PID_SEQUENCE=82733,681,82733 COMPME_ACCEPTANCE_PID_SEQUENCE_INTERVAL_MS=900 ./target/debug/examples/textedit_observer_acceptance 4200 switch`: PASS with `SUMMARY focus=3 caret=16 apps={"pid:681", "pid:82733"}`.

Fresh A1b live runner acceptance:

- `bash -n tools/acceptance/run-a1b-live-gates.sh`: PASS.
- `tools/acceptance/run-a1b-live-gates.sh --skip-build --textedit-pid 82733 --timeout-ms 4000 --short-timeout-ms 2000`: PASS with `Summary: pass=13 fail=0 skip=1`; log directory `tools/acceptance/logs/a1b-live-20260605-104813`. This covers TextEdit read, SyntheticKeys insertion, Clipboard insertion, AxSet insertion, TextEdit caret fallback, full accept insertion, word accept insertion, popup fallback fixture, accept tap inactive/full/word/delayed-hide, and overlay show/update/hide diagnostics for the pre-flip accept binding. The grave->full rerun note is historical pre-flip evidence; the current path was later closed by the rebuilt Carbon/live acceptance gates.
- `tools/acceptance/run-a1b-live-gates.sh --skip-build --skip-textedit --browser-pid 5956 --timeout-ms 3000 --short-timeout-ms 2000`: PASS with `Summary: pass=7 fail=0 skip=7`; log directory `tools/acceptance/logs/a1b-live-20260605-104257`. This covers Safari textarea marker geometry with `DIAG source=Marker`, popup fallback fixture, plus the same accept-tap and overlay diagnostics in the focused browser profile.
- `env COMPME_ACCEPTANCE_PID=5956 target/debug/examples/caret_marker_acceptance 3000 marker`: direct Safari marker check PASS after focusing the page text entry area; the harness now snapshots a passing marker diagnostic before late browser focus churn can stale the final focused element.
- `target/debug/examples/popup_fallback_acceptance 5000`: direct popup fixture check PASS with `RECT Ok(None)`, popup-mode capabilities, `INSERT Ok(Inserted { bytes: 9, chars: 9, strategy: AxSet })`, `READ_AFTER_INSERT Ok(TextContext { left: "popup fixture value inserted", ... caret: 28, ... })`, and `SUMMARY popup=true`. Normal TextEdit and Safari text areas remain inline-mode targets because they correctly expose caret geometry; the repo-local fixture is the popup-mode proof.

Native inline prediction decision:

- `npx ctx7@latest docs /websites/rs_objc2-app-kit "NSTextView inline predictive text automatic text completion disable isAutomaticTextCompletionEnabled macOS AppKit"` confirms `isAutomaticTextCompletionEnabled`, `setAutomaticTextCompletionEnabled`, and `toggleAutomaticTextCompletion` for AppKit-owned text controls. A1b will not attempt cross-app suppression for other applications' text fields; keep this as a future app-specific integration/settings item.

Next implementation step:

Begin the next development slice against the A1b-aligned platform contract. Keep `tools/acceptance/run-a1b-live-gates.sh` as the macOS live regression gate; use the default TextEdit profile for full local acceptance and the `--skip-textedit --browser-pid <pid>` profile when validating browser marker geometry.

Important implementation risk:

- `AXObserver` callbacks are delivered by the run loop where the observer source is registered. `AxWorker` now pumps that run loop on idle timeouts and after jobs, worker-owned resources keep observer registrations and retained AX elements on the worker thread, and public subscriptions now install observers through that path. The C callback queues retained elements back onto `AxWorker` before identity resolution, and callback invocation then moves to the callback dispatcher, so AX reads and user callbacks stay out of the C callback path. Live TextEdit acceptance has passed for pid-targeted focus, caret, read-context, rect, and AxSet insert paths.
- Observer subscriptions now rebind when the frontmost pid changes and suppress stale old-pid callbacks. App disappearance and reappearance are unit-tested, stale field operations for non-running pids map to `PlatformError::AppExited`, and live pid-sequence acceptance passed across TextEdit and Finder.

Acceptance for the next step:

- Keep all AX reads on `AxWorker`; do not perform AX reads on AppKit main thread or CGEventTap callbacks.
- Resolve field identity from callback or focused AX elements with owner pid/role data; live TextEdit acceptance is proven for pid-targeted focus, caret, read-context, rect, AxSet, SyntheticKeys, Clipboard, full accept insertion, and word accept insertion paths. Safari textarea marker-path acceptance is proven with `source=Marker`. Global Secure Input detection is covered by unit tests and was validated against the local macOS SDK header; a runtime secure-input block remains a diagnostic path, not a development-start blocker.
- Keep model path and prompt strategy configurable; the selected base GGUF remains the development default, not a product-quality guarantee.
- Keep A1a bound to `docs/superpowers/plans/2026-06-04-a1b-macos-adapter-contract.md`.

## Suggested Skills

- `gsd-plan-phase` for turning the model-decision work into a small, verifiable phase.
- `gsd-execute-phase` for implementing the benchmark and docs update once planned.
- `test` for focused validation after the model benchmark changes.
