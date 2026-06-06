# P0 MVP Integration — Design

**Date:** 2026-06-06
**Status:** Draft — awaiting user spec review
**Scope:** The 5 P0 (blocks-MVP-ship) items: integration run-loop binary, Engine↔inference wiring, end-to-end live acceptance gate, model warm-up on launch, graceful shutdown on exit.

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
                             timer               applies overlay         │
                          (~12ms heartbeat)      cmds (main-thread OK)    │
                             │                    ▲                       │
                       [result queue] ◀───────────┴────on_completion──────┘ (mpsc)
```

- **Main thread** owns `Engine` + `MacosOverlayPresenter`; runs `NSApplication.run()` under `NSApplicationActivationPolicy::Accessory`.
- **Heartbeat**: a single repeating main-thread timer (~12ms). Each fire:
  1. drain inbound **event queue** → `engine.on_focus` / `engine.on_text_changed` / `engine.on_caret_moved` / `engine.on_accept`,
  2. drain inbound **result queue** → `engine.on_completion`,
  3. call `engine.on_tick(now_ms)`,
  4. push any collected `CompletionRequest` to the inference **request slot** (latest-wins).
- **Inference thread**: `warm_up()` once → loop `recv → complete → send {request, text}` back.
- The adapter's own AX work happens on its `AxWorker`; the Engine touching the adapter from main only briefly blocks main.

### Decision a (defaulted, confirm at review): signals via `libc`
SIGINT/SIGTERM handled with a raw `libc::sigaction` handler that only sets an `AtomicBool` (async-signal-safe). No new `signal-hook` dependency. `libc` is already in the dependency graph.

### Decision b (defaulted, confirm at review): heartbeat via `CFRunLoopTimer`
Use a `CFRunLoopTimer` added to the main run loop rather than `NSTimer` — no Objective-C target object / selector plumbing required. `core-foundation` is already a transitive dependency via the macOS crates.

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

- A `libc::sigaction` handler for SIGINT/SIGTERM sets `AtomicBool should_stop`.
- The heartbeat checks `should_stop`; when set it stops the run loop (`NSApplication::stop` / `CFRunLoopStop`).
- After `run()` returns, teardown runs in order:
  1. drop accept, caret, focus subscriptions (in that order),
  2. signal the inference thread to stop and `join()` it,
  3. `Box::new(model).shutdown()` — frees model before backend, guarding the ggml-Metal exit-abort.

## Config (P0 = constants + env; full config surface is P1)

- `debounce_ms`, `max_words`, `max_tokens`: constants in `main.rs`.
- `COMPLETE_ME_MODEL_PATH`: model GGUF path; defaults to the spike base model (`tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf`).
- `COMPLETE_ME_STUB_COMPLETION="<text>"`: when set, use a deterministic `StubModel` returning `<text>` instead of `LlamaModel`.
- `COMPLETE_ME_ACCEPTANCE_PID=<pid>`: use `MacosPlatformAdapter::with_frontmost_pid_override_for_acceptance` (mirrors the existing examples); absent → `MacosPlatformAdapter::new()`.
- `COMPLETE_ME_RUN_MS=<n>`: auto-stop after `n` ms (bounded gate runs); absent → run until signal.

## End-to-end live gate (P0 item 3)

Driven by `tools/acceptance/run-a1b-live-gates.sh` (extended with one new gate):

1. `osascript`: focus TextEdit and set a known prefix.
2. Launch `complete-me` with `COMPLETE_ME_STUB_COMPLETION="<known>"`, `COMPLETE_ME_ACCEPTANCE_PID=<textedit pid>`, `COMPLETE_ME_RUN_MS=<n>`.
3. The binary logs each pipeline stage: `focus → request → completion → show-ghost → accept → insert`.
4. `osascript` sends **Tab** → accept tap fires → `engine.on_accept(Full)` → insert.
5. The gate reads the TextEdit value and asserts it contains `<known>`, and asserts the log shows each stage. Deterministic because the stub completion is fixed.
6. A separate **manual** invocation uses the real `LlamaModel` and asserts that *an* insert occurred (output text not pinned, since it is nondeterministic).

The product binary is the thing under test; the gate drives the real product path with a deterministic model.

## Testing strategy

- **Pure logic in `wiring.rs`** (EditKind/previous-hash derivation, latest-wins coalescing, ready-gate state) — unit-tested, no AppKit. TDD.
- **`StubModel`** — unit-tests the inference-thread protocol (request in → result out, warm-up sets ready).
- **AppKit/main-thread glue** (`run_loop.rs` timer, `NSApplication.run`, signal handler) is intentionally thin and is covered by the live E2E gate; it cannot be unit-tested without a UI session.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets` stay green throughout.

## Out of scope (P1+, per the pending list)

Tray/menu-bar UI, Accessibility/Input-Monitoring permission first-run UX, full settings/config surface, Retina multi-monitor offset measurement, prefix/KV-cache reuse, long-lived model actor, N-sample multi-candidate generation, sentence/punctuation stop-boundary, and all P2–P4 items.

## Open items to confirm at spec review

- **Decision a**: `libc` raw `sigaction` for signals (vs adding `signal-hook`).
- **Decision b**: `CFRunLoopTimer` heartbeat (vs `NSTimer`).
- Heartbeat interval (~12ms proposed) and whether `on_caret_moved` should always also trigger a `read_context` (cost vs. correctness).
