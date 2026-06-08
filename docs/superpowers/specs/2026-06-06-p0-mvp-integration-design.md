# P0 MVP Integration — Design

**Date:** 2026-06-06 (reconciled with implementation 2026-06-07)
**Status:** Implemented in `crates/app` and reviewed. The original 2026-06-07 macOS live run validated the P0 loop before the Cotypist accept-key correction. **[CORR 06-08 — D8 reconciled]** The post-flip binding-specific live GUI rerun has since been **completed and is recorded as CLOSED in the design spec §15 (gates G6 + I11, Apple M4 Max, 2026-06-08)**: `e2e-complete-me.sh` ran both full (grave→`accept Full`) and word (Tab→`accept Word`, grave→`accept Full` remainder) modes against TextEdit with a real `LlamaModel`. Design spec §15 is the authoritative live-gate record; the "rerun pending" / "live-validation gap" language elsewhere in this doc predates that run and is retained only as historical context.
**Scope:** The 5 P0 (blocks-MVP-ship) items: integration run-loop binary, Engine↔inference wiring, end-to-end live acceptance gate, model warm-up on launch, graceful shutdown on exit.

**Cotypist parity note (2026-06-07 audit):** P0 proves the local completion loop and deterministic TextEdit acceptance path. It does not claim parity with Cotypist's full app surface: screen-aware context, encrypted personalization, app/domain overrides, Google Docs setup, browser mirror/text-metrics workarounds, terminal AI-agent prompt activation, full shortcut customization, updater/signing, telemetry policy, emoji, and typo correction are A2/A3+ scope.

## Live validation — 2026-06-07 (Apple M4 Max, macOS 25.5)

Run on a real GUI session (console user logged in, Accessibility already granted to the terminal). Evidence:

**Non-intrusive smoke** (`COMPLETE_ME_STUB_COMPLETION=… COMPLETE_ME_RUN_MS=2500`, no keystrokes): adapter + overlay init OK, `state=loading → state=ready`, focus event marshalled dispatcher→main, non-text frontmost (`AXGroup`) → `UnsupportedField` logged and loop continued, clean exit 0. Validates items 1, 4, 5 + the threading model.

**Stub E2E gate** (`tools/acceptance/e2e-complete-me.sh` against TextEdit pid): **historical PASS with stale binding evidence**. Seeded `"The quick brown fox "`, binary logged `focus` (AXTextArea) → `request gen=2 prompt="The quick brown fox"` → `completion " jumps-NNNNNN"` → `accept Full`, and the document became `"The quick brown fox jumps-NNNNNN"`. All four logged stages present; document assertion passed. Validated items 2, 3 for the original Tab=full binding. **[CORR 06-08]** This PASS predates the Cotypist-parity accept-key flip. The fresh post-flip desktop live run **has since been done** — design spec §15 G6/I11 record `e2e-complete-me.sh` passing both grave→full and Tab→word against TextEdit (M4 Max, 2026-06-08), including a real-`LlamaModel` end-to-end run. P0 is revalidated under the current bindings per §15.

**Real `LlamaModel`** (no stub, GGUF on Metal): warm-up `loading→ready` (embedded Metal lib ~7s first load), then `request gen=3 prompt="Dear team, I wanted to"` → real completion `" let you know that I have been working on a new project for the past few weeks…"`. Validates the real model path: load → warm → terse prompt → genuine inference. (Insert not exercised here — no Tab sent; insert proven by the stub E2E.)

Acceptance docs left open in TextEdit are throwaway test content (discard without saving).

## Problem

All parts of the macOS autocomplete stack are proven in isolation (A0 spike P1–P7, A1a engine, A1b macOS adapter, model_client). Nothing wires the full stack into one running process:

- No `main()` / `[[bin]]` exists anywhere; the workspace is library-only with acceptance examples.
- `Engine` only ever runs in unit tests; `LlamaModel` only runs in `model_client/tests/latency.rs`. They are decoupled.
- `warm_up()` and `shutdown()` overrides exist on `LlamaModel` but nothing calls them from an application.

This design wires those proven parts into a single `complete-me` binary and adds a deterministic end-to-end live gate.

## Key constraints (from the seam map)

1. **Engine runs on the AppKit main thread.** `Engine<P, O>` owns the `OverlayPresenter` and applies overlay commands (`ShowGhost`/`UpdateGhost`/`Hide`) **internally** inside its `on_*` methods. `MacosOverlayPresenter` enforces `MainThreadMarker::new()` at runtime. Therefore every `engine.on_*` call must happen on the AppKit main thread.
2. **Platform callbacks fire off-main.** `subscribe_focus/caret/accept` deliver `Arc<dyn Fn .. Send + Sync>` callbacks from the `CallbackDispatcher` thread. They cannot touch the Engine directly.
3. **Inference blocks ~50ms.** `LlamaModel::complete()` must not run on the main thread (stalls overlay/event drain) nor on the dispatcher thread (stalls events).
4. **Adapter methods self-marshal.** `read_context`, `caret_rect`, `capabilities`, `insert` dispatch to `AxWorker` internally and block the caller. They are safe to call from the main thread (brief block).
5. **No text-change subscription exists.** `PlatformAdapter` exposes only focus/caret/accept. Typing is observed through the **caret/selection-changed callback**: read context on each caret event and synthesize a `TextChange`.

## Architecture

```
 dispatcher thread          MAIN THREAD (NSApplication.run)            inference thread
 ----------------          --------------------------------          ----------------
 focus cb  ──┐                                                
 caret cb  ──┼──push──▶  [evt queue] ─drain─▶ Engine.on_*  ─reqs─▶ [req slot]─▶ complete()
 accept cb ──┘  (mpsc)        ▲                   │ (latest-wins)         │
                          pump (run_in_mode)    applies overlay         │
                          (~12ms heartbeat)      cmds (main-thread OK)    │
                             │                    ▲                       │
                       [result queue] ◀───────────┴────on_completion──────┘ (mpsc)
```

- **Main thread** owns `Engine` + `MacosOverlayPresenter`; runs `NSApplication.run()` under `NSApplicationActivationPolicy::Accessory`.
- **Heartbeat**: each iteration the main thread pumps the run loop once via `CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, ~12ms, false)` (paces the loop and services the overlay), then on the next pass:
  1. drain inbound **event queue** → `engine.on_focus` / `engine.on_text_changed` / `engine.on_caret_moved` / `engine.on_accept`,
  2. drain inbound **result queue** → `engine.on_completion`,
  3. call `engine.on_tick(now_ms)`,
  4. push any collected `CompletionRequest` to the inference **request slot** (latest-wins).
- **Inference thread**: `warm_up()` once → loop `recv → complete → send {request, text}` back.
- The adapter's own AX work happens on its `AxWorker`; the Engine touching the adapter from main only briefly blocks main.

### Decision a: signals via `libc`
SIGINT/SIGTERM handled with a `libc::signal` handler that only sets an `AtomicBool` (async-signal-safe `Relaxed` store). No `signal-hook` dependency.

### Decision b: heartbeat via run-loop pumping
Pump the main run loop with `CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, ~12ms, false)` once per iteration rather than installing a `CFRunLoopTimer`/`NSTimer` callback — no extern-C context plumbing, and one call both paces the loop and services the overlay's AppKit needs. `core-foundation` is already a transitive dependency via the macOS crates.

## Crate structure

New binary crate, added to workspace `members`:

```
crates/app/
  Cargo.toml          # [[bin]] name = "complete-me"; deps: engine, platform, platform_macos, model_client, libc, core-foundation
  src/main.rs         # parse env/config, build stack, run, ordered teardown
  src/run_loop.rs     # main-thread heartbeat: drain queues + on_tick + stop-flag check
  src/wiring.rs       # callback→event marshalling; TextChange/EditKind derivation; latest-wins coalescing
  src/inference.rs    # inference thread: warm_up + complete loop; ready flag
  src/model_select.rs # choose Box<dyn LocalModel>: LlamaModel vs StubModel (env)
```

Lib crates stay pure; only `crates/app` depends on AppKit entry glue.

## Event wiring

- **focus cb** → enqueue `Focus(field)` → `engine.on_focus(field)`.
- **caret cb** (typing driver) → enqueue `Caret(field, rect)`. Main handler:
  - `adapter.read_context(field)` → derive a `TextChange` → `engine.on_text_changed(change)`,
  - `engine.on_caret_moved(field, caret)` for hide-on-jump invalidation.
- **EditKind derivation**: `wiring.rs` keeps a per-field last-seen `(value, caret, value_hash)`. On each caret/read it computes:
  - `edit: EditKind` (insert vs delete vs other) by comparing new vs previous value length/content,
  - `previous_caret`, `previous_value_hash` fields on `TextChange`.
  - Deletes are suppressed by the Engine/SuggestionMachine (already wired).
- **accept cb** → enqueue `Accept(action)` → `engine.on_accept(action)`. `engine.set_accept_subscription(accept_sub)` is called at startup so suggestion-visibility and delayed teardown drive the two-tap interception.

## Inference + ready gate

- Model is `Box<dyn LocalModel>`, chosen at startup (see Config).
- **Request slot is latest-wins**: main keeps only the newest `CompletionRequest`; older superseded requests are dropped before send. Any stale result that still arrives is discarded by `engine.on_completion` via generation/snapshot checks (already wired) — belt and suspenders.
- **Warm-up (P0 item 4)**: the inference thread calls `model.warm_up()` before entering its loop, then sets `AtomicBool ready = true`. State is logged to stderr (`state=loading` → `state=ready`). No ghost appears until the first real completion lands. No tray UI in P0 (tray is P1); "loading state" surfaces as a log line + absence of suggestions.

## Shutdown (P0 item 5)

- A `libc::signal` handler for SIGINT/SIGTERM sets a `static AtomicBool STOP` (the only async-signal-safe work it does — a `Relaxed` store).
- The loop checks `STOP` each iteration; when set it exits the pump loop.
- After the loop exits, teardown runs in this order:
  1. drop the caret and focus subscriptions (stop new focus/caret events),
  2. `inference.shutdown()` — closes the request channel, the worker exits its loop and **frees the model on the inference thread** (`model.shutdown()` drops the model before the backend), then `join()`. This is the ggml-Metal exit-abort guard, and it completes before the engine/overlay are dropped.
  3. `drop(engine)` — releases the overlay and the accept subscription it owns; then `drop(adapter)` releases the last `Arc`, stopping the AX worker.

Note on the accept subscription: it is owned by the `Engine` (via `set_accept_subscription`) and so is released at step 3, not step 1. This is safe: the pump loop has already exited, so any late accept callback only enqueues an event that is never drained. The model is freed at step 2 — before the engine/overlay drop — so the Metal-exit guard holds regardless of accept-sub drop order.

## Config (P0 = constants + env; full config surface is P1)

- `debounce_ms`, `max_words`, `max_tokens`: constants in `main.rs`.
- `COMPLETE_ME_MODEL_PATH`: model GGUF path; defaults to the spike base model (`tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf`).
- `COMPLETE_ME_STUB_COMPLETION="<text>"`: when set, use a deterministic `StubModel` returning `<text>` instead of `LlamaModel`.
- `COMPLETE_ME_ACCEPTANCE_PID=<pid>`: use `MacosPlatformAdapter::with_frontmost_pid_override_for_acceptance` (mirrors the existing examples); absent → `MacosPlatformAdapter::new()`.
- `COMPLETE_ME_PROMPT_MODE=terse|raw`: prompt strategy applied to the engine's raw left-context prefix before inference. Default `terse` (wraps with `terse_continuation_prompt`, the A1a development default); `raw` passes the prefix through. Keeping this configurable satisfies the contract requirement that prompt strategy not be hardcoded.
- `COMPLETE_ME_RUN_MS=<n>`: auto-stop after `n` ms (bounded gate runs); absent → run until signal.

Prompt shaping lives in the inference thread (`shape_prompt`), not in the engine: the engine emits a raw left-context prefix in `CompletionRequest.prompt`, and the worker wraps it per `PromptMode` immediately before `model.complete()`. The `StubModel` ignores the prompt, so the gate stays deterministic regardless of mode.

## End-to-end live gate (P0 item 3)

Driven by `tools/acceptance/run-a1b-live-gates.sh` (extended with one new gate):

1. `osascript`: focus TextEdit and set a known prefix.
2. Launch `complete-me` with `COMPLETE_ME_STUB_COMPLETION="<known>"`, `COMPLETE_ME_ACCEPTANCE_PID=<textedit pid>`, `COMPLETE_ME_RUN_MS=<n>`.
3. `osascript` moves the caret to end-of-line (fires a selection-changed read), waits for the ghost, then sends **grave** (key code 50) → accept tap fires → `engine.on_accept(Full)` → insert. **[CORR 06-08]** (Was Tab; after the Cotypist-parity flip Tab accepts the next *word* and grave accepts the *full* completion.)
4. The gate asserts two things:
   - **Document content** contains `<known>`. This transitively proves the whole chain: `Engine::on_accept(Full)` only emits an `Insert` command when a ghost is currently showing (`SuggestionMachine` holds `self.showing`), so a successful insert of the stub text proves focus → read → infer → **show-ghost** → accept → insert all fired. The overlay-show step is applied *inside* the engine and emits no log line of its own, so it is verified by its observable effect (the insert), not by a log string.
   - **Logged stages** `focus`, `request gen=`, `completion gen=`, `accept Full` are each present in the binary's stderr. (These are the stages the run loop logs directly.)
5. Deterministic because the stub completion is fixed.
6. A separate **manual** invocation uses the real `LlamaModel` and asserts that *an* insert occurred (output text not pinned, since it is nondeterministic).

The gate verifies both accept paths via `COMPLETE_ME_E2E_ACCEPT` (**[CORR 06-08]** keys updated for the Tab=word / grave=full binding):
- **Full** (default): one **grave** (key code 50) → `engine.on_accept(Full)` → whole-suggestion insert; asserts the `accept Full` stage.
- **Word** (`COMPLETE_ME_E2E_ACCEPT=word`): **Tab** (key code 48 → first word) then **grave** (key code 50 → remaining ghost); asserts both `accept Word` and `accept Full` stages and that the contiguous stub text (e.g. `" jumps over"`) lands in the document. The standalone `accept-insert-word` gate still covers the tap-layer word path in isolation.

The product binary is the thing under test; the gate drives the real product path with a deterministic model.

## Testing strategy

- **Pure logic in `wiring.rs`** (EditKind/previous-hash derivation, latest-wins coalescing, ready-gate state) — unit-tested, no AppKit. TDD.
- **`StubModel`** — unit-tests the inference-thread protocol (request in → result out, warm-up sets ready).
- **AppKit/main-thread glue** (`run_loop.rs` run-loop pump, overlay, signal handler) is intentionally thin and is covered by the live E2E gate; it cannot be unit-tested without a UI session.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets` stay green throughout.

## Out of scope (P1+, per the pending list)

Historical P0 exclusions, some of which were closed by P1: tray/menu-bar UI, Accessibility/Input-Monitoring permission first-run UX, full settings/config surface, Retina multi-monitor offset measurement, prefix/KV-cache reuse, long-lived model actor, N-sample multi-candidate generation, sentence/punctuation stop-boundary, and all P2–P4 items. See `2026-06-07-p1-mvp-quality-design.md` for the items P1 has since implemented or measured; true-2x/multi-monitor geometry and literal tray-menu mouse-click validation remain environment/manual checks there.

Additional Cotypist-alignment items are also out of P0: optional Screen Recording / OCR context, Google Docs Accessibility onboarding, browser compatibility guidance and mirror fallback, Terminal/iTerm AI-agent prompt heuristics, per-app/per-domain controls, encrypted local personalization storage, emoji/typo features, full shortcut settings, signed/updating app packaging, and telemetry policy.

## Resolved decisions (as implemented)

- **Decision a**: signals via `libc::signal` (a handler that only sets `AtomicBool STOP`); no `signal-hook` dependency.
- **Decision b**: heartbeat via **pumping** the main run loop with `CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, ~12ms, false)` each iteration — chosen over a `CFRunLoopTimer`/`NSTimer` callback because it needs no extern-C context plumbing and both paces the loop and services the overlay in one call.
- **Caret vs typing**: the caret handler reads context, then `FieldTracker` classifies it: identical reconstructed value → `Observation::CaretMoved` → `engine.on_caret_moved` (hide-on-jump only); changed value → `Observation::Typed` → `engine.on_text_changed` (schedules a completion). This avoids re-requesting a completion on every bare cursor move.

## Known P1 follow-ups (acknowledged, not addressed here)

- A `read_context` AX round-trip runs per delivered caret event; no read-level debounce yet (the adapter already coalesces caret notifications).
- Heartbeat interval (~12ms) is a fixed constant; not yet tuned or configurable.
