# G7 call/ownership analysis — current code (2026-09-29)

> **Later 2026-09-29 update:** “current code” below means the pre-G7 snapshot
> analyzed by Worker A. The later owner-authorized
> [implementation plan](2026-09-29-g7-linux-implementation.md) is implemented
> locally; see the [independent audit](2026-09-29-g7-codex-validation.md) for
> the new ownership contract and remaining native validation.

**Author:** Worker A (implementation analysis), dispatched by the glm
coordinator session. **Scope & method.** Read-only analysis of the G7 surface
named in `Qfd.md` §22 (design of record, 2026-09-16, not landed). Per repo
policy, `codegraph explore` was run first on all G7 symbol groups; grep/read
were used only to follow up and pin exact lines. No file was modified; no cargo
or state-changing git command was run. All line numbers below are current
on-disk lines verified by direct reads. The uncommitted protected diff was not
touched.

**Coordinator integration (2026-09-29), applying three independent-review
corrections:**

1. **Historical-growth narrative retracted (P1).** The worker's original
   headline claimed the second Carbon resource family
   (`WorkerShortcutResource`, `install_process_shortcut_hotkeys`, a second
   handler slot) "now exists that §22 never mentions", that `register_hotkey`
   "split" into two methods, that the defect surface "grew", and implied
   adjacent tests appeared after §22. Git history refutes all of this: the
   §22-era commit `ab565a0` (2026-09-16) already contained
   `WorkerShortcutResource` (`:3340`), `install_process_shortcut_hotkeys`
   (`:3410`), both `register_hotkey` methods (`:3360`/`:3675`),
   `CorrectionConsumer`, grammar id 9, and a `#[cfg(test)]`
   `install_resource` (`:336`); the shortcut resource itself dates to `d8f0e98`
   (2026-06-30); the stale-disarm slot test predates September. The
   §22-era source contains these symbols; Qfd §22 itself specifies two
   registries (§22.2) and two `Drop` impls (§22.3) without explicitly naming
   the shortcut symbols (the explicit symbol list comes from the 2026-09-29
   dispatch handoff). The
   unproven eight-versus-six function-surface comparison is removed. Claims
   about a "signature changed" date are likewise dropped; current signatures
   are stated without change-date assertions.
2. **`unsafe impl Send` reframed (P2).** Qfd §22.2's "exactly one
   `unsafe impl Send` is needed" is treated as a historical assumption to
   reevaluate at implementation time, not a prescribed requirement: a token
   carrying only a `u64` is automatically `Send`, and adding a `Drop` impl does
   not change auto-trait status — so the correct expectation is likely **zero**
   new `unsafe impl`s if the token stays ID-only. Any eventual `unsafe impl`
   would need its own justification.
3. **Coverage conclusion bounded (P2).** The original "zero automated coverage
   CONFIRMED" rested on grep name counts, which cannot prove absence of runtime
   coverage: the A1b acceptance runner's accept-tap gates exercise the real
   native registration path live on macOS, and worker-queue machinery has its
   own `#[cfg(test)]` tests with fake resources. The bounded conclusion below
   separates inspected fake paths from missing current-candidate live evidence.

Independently verified as accurate and retained: the main→worker synchronous
wait edge and the async teardown (three-initiator) description, the symbols
table, the wait graph, and the defect-surface location list.

## 1. Symbols table

Threads: **M** = main/AppKit thread (loop documented as main-thread at
`crates/app/src/run_loop.rs:4-6`, entered via `run()` at `run_loop.rs:5703`
from `crates/app/src/main.rs:38-39`); **W** = the `compme-ax-worker` thread
(spawned `crates/platform_macos/src/ax_worker.rs:286-291`); **S** = detached
sleeper thread; **D** = callback-dispatcher thread (`ax_worker.rs:1116-1125`);
**E** = any engine-side caller thread (main loop in production).

| Symbol | Current file:line | Thread | Waits on | Owns |
|---|---|---|---|---|
| `MacosPlatformAdapter::accept_tap_installer` | `crates/platform_macos/src/lib.rs:1516-1528` | M (called from controller on E) | Nothing itself; the returned closure waits on W | The `AdapterAcceptTapInstaller` enum (`lib.rs:253`); builds closure capturing `AxWorkerHandle` |
| `AdapterAcceptTapInstaller::{Worker,Custom}` | `lib.rs:595-598` | — | — | — |
| `AcceptTapInstallerFn` (type) | `lib.rs:170-171` | — | — | — |
| `AcceptTapResource` (caller-side wrapper) | `lib.rs:343-353` | dropped on E/S | Nothing (its inner `Drop` is async) | `Box<dyn Any + Send>` wrapping `AxWorkerResource` |
| `AxWorkerHandle::install_resource` | `ax_worker.rs:378-400` | caller (M/E/S) | **Blocks on `reply_rx.recv()`** (`ax_worker.rs:393-397`) | Mints resource id (`ax_worker.rs:382`); returns `AxWorkerResource` |
| `AxWorkerHandle` (struct) | `ax_worker.rs:182-186` | — | — | `tx`, shared `next_resource_id` |
| `AxWorkerResource` (struct + `Drop`) | `ax_worker.rs:188-192`; `Drop` 511-520 | dropped on E/S | Nothing — **async**: posts `Message::RemoveResource { reply: None }` (`ax_worker.rs:516-519`) | resource id + `tx` clone |
| `WorkerResource` (type alias) | `ax_worker.rs:54` | lives only on W | — | `Box<dyn Any + 'static>` — **deliberately not `Send`** (no `Send` bound; never leaves W's `resources` map, `ax_worker.rs:878`) |
| `ResourceInstaller` (type alias) | `ax_worker.rs:55-56` | closure built on M, runs on W | — | `Box<dyn FnOnce() -> Result<WorkerResource, PlatformError> + Send>` (closure is `Send`; its result is not) |
| `Message::{InstallResource,RemoveResource}` | `ax_worker.rs:81-85`, `86-88` | queued from M/E/S | `RemoveResource` carries `Option<Sender<bool>>` (used only by test `close`, `ax_worker.rs:505-507`; production posts `reply: None`) | — |
| Worker loop arms (execute installers / drop resources) | `ax_worker.rs:933-949` (install), `951-966` (remove), `1092` (Stop) | W | Never blocks on M | `resources: HashMap<u64, WorkerResource>` (`ax_worker.rs:878`) |
| `install_worker_accept_tap_resource` | `lib.rs:3312-3325` | W (inside `InstallResource` arm) | Runs inline; calls blocking-free Carbon | Dispatches by `AcceptTapKind` |
| `install_carbon_accept_hotkeys` | `lib.rs:3328-3359` | W | Calls `ensure_carbon_handler_installed` (`3336`) and `RegisterEventHotKey` (`3354`) | Builds `WorkerAcceptTapResource` |
| `install_process_shortcut_hotkeys` | `lib.rs:3459-3507` | W | Calls `ensure_carbon_handler_installed` (`3466`), `RegisterEventHotKey` via log-and-skip (`3495`) | Builds `WorkerShortcutResource` |
| `ensure_carbon_handler_installed` | `lib.rs:3510-3566` | W | Holds `CARBON_HANDLER_INSTALLED` mutex across `InstallEventHandler` FFI (`~3538-3546`) | One-time process-lifetime `EventHandlerRef` (intentionally never removed) |
| `carbon_accept_hotkey_handler` | `lib.rs:3782-3872` | **M** (Carbon delivers on the app event target) | Never blocks; sends `CallbackMessage::Accept` async via mpsc | Nothing (reads slots) |
| `WorkerAcceptTapResource` (struct) | `lib.rs:3284-3288` | owned by W's map | — | `Vec<EventHotKeyRef>`, `arm_id: u64` |
| `impl WorkerAcceptTapResource::register_hotkey` | `lib.rs:3734-3780` | W | Calls `RegisterEventHotKey` FFI (`~3752-3763`) | Pushes ref into `self.hotkeys` only on status 0 (`3779`) |
| `Drop for WorkerAcceptTapResource` | `lib.rs:3290-3310` | W (on remove/Stop/worker exit) | Nothing | `UnregisterEventHotKey` per ref (`3305`); `CARBON_HANDLER_SLOT.disarm(arm_id)` (`3308`) |
| `WorkerShortcutResource` (struct) | `lib.rs:3389-3392` | owned by W's map | — | `Vec<EventHotKeyRef>`, `arm_id: u64` |
| `impl WorkerShortcutResource::register_hotkey` | `lib.rs:3408-3448` | W | `RegisterEventHotKey` FFI (`~3426-3437`) | Same push-on-success discipline |
| `Drop for WorkerShortcutResource` | `lib.rs:3394-3406` | W | Nothing | `UnregisterEventHotKey` per ref (`3401`); `SHORTCUT_HANDLER_SLOT.disarm(arm_id)` (`3404`) |
| `CarbonHandlerSlot` (+ `arm`/`disarm`/`current`) | `lib.rs:3235-3273` | any (mutex-guarded); `current` runs on M inside the Carbon callback | — | `Mutex<Option<(u64, Arc<AcceptTapHandler>)>>` |
| `CARBON_HANDLER_SLOT` / `CARBON_ARM_ID` / `CARBON_HANDLER_INSTALLED` | `lib.rs:3277`, `3279`, `3282` | shared statics | — | consumer-arm slot; u64 arm counter; install flag |
| `SHORTCUT_HANDLER_SLOT` / `SHORTCUT_ARM_ID` | `lib.rs:3365`, `3367` | shared statics | — | shortcut-arm slot; u64 arm counter |
| `AcceptTapController` (struct) | `lib.rs:355-363` | E/S callers | — | `consumer_tap: Mutex<Option<AcceptTapResource>>`, `accept_action`, `teardown_generation` |
| `AcceptTapController::set_accept_action` | `lib.rs:416-469` | E (main, via engine) | **Holds `consumer_tap` guard across the installer call that blocks on W** (invariant doc `lib.rs:426-430`; install call `446-449`; tear-down drop `451-458`) | Arbitrates arm/disarm |
| `AcceptTapController::rearm_consumer_tap` | `lib.rs:485-518` (doc 473-484) | E (main; doc `481-483` forbids worker callers) | Drop (`497`) is async; install (`513-516`) **blocks on W**; FIFO assumption doc `480-486` | Same guard-across-installer invariant (`486-487`) |
| `AcceptTapController::hide_suggestion_after` | `lib.rs:533-558` | E (main) to schedule; **S** to fire | Detached sleeper sleeps `delay` (`553-556`), then `deactivate_if_generation` (`560-584`; drop at `581-583`) | One thread per non-zero-delay hide (`546-552`) |
| `AcceptTapController::set_suggestion_visible` | `lib.rs:404-414` | E (main) | Delegates to `set_accept_action` (`413`) | — |
| `subscribe_accept` (shortcut install site) | `lib.rs:1713-1745` | M at subscription setup | `installer(AcceptTapKind::Shortcut, …)` at `1725-1733` **blocks on W once** | Holds `shortcut_tap` for subscription lifetime (`SubscriptionEntry::Accept`) |
| `AxWorker::run` (generic job path) | `ax_worker.rs:331-356` | caller (M in production: `lib.rs:1167`, `lib.rs:1874`) | Blocks on reply (`ax_worker.rs:344-348`) | — |
| `run_callback_dispatcher` (`CallbackMessage::Accept` arm) | `ax_worker.rs:1148-1163` | D | Never blocks on M/W | — |
| Engine bridge: `Engine::set_tap_visible` / `hide_tap_after` / `on_tick` | `crates/engine/src/lib.rs:205-217`, `222-228`, `302-309` | M | `set_accept_action`/`set_suggestion_visible` block on W; `hide_tap_after` returns immediately (spawns S) | — |
| Platform bridge: `AcceptSubscription::{set_accept_action,hide_suggestion_after,rearm_accept_tap}` | `crates/platform/src/lib.rs:511-513`, `505-507`, `489-497` | E | As above; rearm documented host-loop-only (`492-494`) | — |
| Host wiring: `run()` tick → `engine.on_tick` | `crates/app/src/run_loop.rs:6271`; module doc `run_loop.rs:4-6` | M | — | — |
| `AcceptCallback` (host side) | `crates/app/src/run_loop.rs:4565-4577` | D | Pushes to a host-event queue (`push_host_event`); non-blocking | — |
| `AdapterTestHooks` / `with_worker_test_hooks` (injection seam) | `lib.rs:365-378`, `1291`, `1320` | test only | — | `#[cfg(test)]` |

## 2. Complete current wait graph

**Registration (arm) — the synchronous main → worker edge.**

```
[M] run_loop::run (run_loop.rs:5703, main thread per run_loop.rs:4-6)
 └→ engine.on_tick (run_loop.rs:6271) → Engine::dispatch
     └→ Engine::set_tap_visible (engine/src/lib.rs:205-217)
         └→ AcceptSubscription::set_accept_action (platform/src/lib.rs:511-513)
             └→ closure (platform_macos lib.rs:1769)
                 └→ AcceptTapController::set_accept_action (lib.rs:416)
                     └→ (self.installer)(kind, handler) (lib.rs:446-449)
                         └→ accept_tap_installer Worker-arm closure (lib.rs:1520-1524)
                             └→ AxWorkerHandle::install_resource (ax_worker.rs:378)
                                 └→ ★ SYNC EDGE ★ reply_rx.recv() (ax_worker.rs:393-397)
[W] Message::InstallResource arm (ax_worker.rs:933-949)
     └→ install_worker_accept_tap_resource (lib.rs:3312)
         ├→ Consumer/CorrectionConsumer → install_carbon_accept_hotkeys (lib.rs:3328)
         │   ├→ GetApplicationEventTarget (3335)          [Carbon FFI on W]
         │   ├→ ensure_carbon_handler_installed (3336 → 3510; InstallEventHandler ~3538-3546)
         │   ├→ CARBON_ARM_ID / CARBON_HANDLER_SLOT.arm (3338-3339)
         │   ├→ plan = accept_keymap().arm_bindings_for_action(...) (3351-3353; fn 3037)
         │   └→ WorkerAcceptTapResource::register_hotkey (3354 → 3735; RegisterEventHotKey ~3752-3763)
         └→ Shortcut → install_process_shortcut_hotkeys (lib.rs:3459)
             ├→ GetApplicationEventTarget (3464-3465)     [Carbon FFI on W]
             ├→ ensure_carbon_handler_installed (3466)
             ├→ SHORTCUT_ARM_ID / SHORTCUT_HANDLER_SLOT.arm (3468-3469)
             ├→ plan = shortcut_registration_plan + collision filter (3474-3489; fns 2919, 3375)
             └→ WorkerShortcutResource::register_hotkey, log-and-skip (3495 → 3409; FFI ~3426-3437)
     └→ reply.send(Ok(())) → M unblocks. New keys live when set_accept_action returns (§22 invariant, still true).
```

The same synchronous edge runs once per subscription for shortcuts from
`subscribe_accept` (`lib.rs:1725-1733`), and on every rearm install
(`lib.rs:513-516`).

**Teardown (disarm) — three initiators, all async into W.**

```
Initiator 1 [M]: set_accept_action(None) (lib.rs:416) → (false,true) arm (451-458)
                 → *consumer_tap = None → AcceptTapResource::drop (343-353)
Initiator 2 [S]: hide_suggestion_after (533) spawns detached sleeper (553-556)
                 → deactivate_if_generation (560) → clear action (573) → drop (581-583)
Initiator 3 [M]: rearm_consumer_tap (485): *consumer_tap = None (497) — DROP-BEFORE-INSTALL,
                 documented FIFO assumption at 480-486 — then install (513-516, ★ sync edge again)
        ↓ each drops AcceptTapResource → AxWorkerResource::drop (ax_worker.rs:511-520)
          → async Message::RemoveResource{id, reply:None} (516-519) — caller does NOT wait
[W] RemoveResource arm (ax_worker.rs:951-966): resources.remove(&id)
    → WorkerAcceptTapResource::drop (3298-3310): UnregisterEventHotKey ×N (3305) + CARBON_SLOT.disarm (3308)
    → WorkerShortcutResource::drop (3396-3406): UnregisterEventHotKey ×N (3401) + SHORTCUT_SLOT.disarm (3404)
[Exit] Message::Stop (ax_worker.rs:1092) → loop ends → `resources` map drops at fn end → same Drops on W.
```

**Dispatch (steady state) — main only, async fan-out.**
`[M]` Carbon delivers the hotkey event on the app event target →
`carbon_accept_hotkey_handler` (`lib.rs:3782`, `catch_unwind`-shielded) → picks
slot by id (`3846-3852`: shortcut ids → `SHORTCUT_HANDLER_SLOT`, accept ids →
`CARBON_HANDLER_SLOT`) → `slot.current()` clone (`3855`) → handler →
`callback_tx.send(CallbackMessage::Accept)` (async mpsc) → `[D]`
`run_callback_dispatcher` Accept arm (`ax_worker.rs:1154-1158`) → host
`AcceptCallback` pushes a `HostEvent` into a queue (`run_loop.rs:4565-4577`)
drained later by `[M]`. **No synchronous edge anywhere in dispatch.**

**Confirmation: no worker → main synchronous edge exists today.** Every W-loop
arm (`ax_worker.rs:917-1103`) either sends a reply to a requester that is
already waiting (main/engine/sleeper → W), sends observer/callback messages
into async channels, or drops resources locally. W never calls a blocking
primitive whose counterpart runs on M. The only synchronous cross-thread waits
are (a) **main/engine/sleeper → worker** via `install_resource`'s reply
(`ax_worker.rs:393-397`) and `AxWorker::run`'s reply (`ax_worker.rs:344-348`;
production callers `lib.rs:1167` insert-apply and `lib.rs:1874`
`page_url_for_pid`), and (b) async fire-and-forget `RemoveResource`. This
matches §22.1's claim and is the property a future G7 design leans on.

## 3. Carbon-registration/unregistration executed on the worker thread (G7 defect surface, current)

All of the following execute inside the W loop (`ax_worker.rs:933-966`,
`1092`), i.e., on a non-main thread, against APIs Apple documents as
main-thread-only:

1. `InstallEventHandler` — `lib.rs:3538-3546` in `ensure_carbon_handler_installed` (reached from W at `3336` accept-arm and `3466` shortcut-arm).
2. `RegisterEventHotKey` (accept keys, ids 1–4 + grammar) — `WorkerAcceptTapResource::register_hotkey`, `lib.rs:~3752-3763`, driven per binding from `3354` inside `install_carbon_accept_hotkeys` on W.
3. `RegisterEventHotKey` (always-on shortcuts, ids 5–8) — `WorkerShortcutResource::register_hotkey`, `lib.rs:~3426-3437`, driven per binding from `3495` inside `install_process_shortcut_hotkeys` on W.
4. `GetApplicationEventTarget` on W — `lib.rs:3335` and `3464-3465` (Carbon event-target API called off-main).
5. `UnregisterEventHotKey` (consumer arms) — `Drop for WorkerAcceptTapResource`, `lib.rs:3305`, running on W whenever the resource is removed (`RemoveResource` arm `ax_worker.rs:951-966`), on worker Stop (`1092`), or worker exit.
6. `UnregisterEventHotKey` (shortcut arms) — `Drop for WorkerShortcutResource`, `lib.rs:3401`, same W sites.
7. The enclosing closures themselves — `install_worker_accept_tap_resource` (`lib.rs:3312`), `install_carbon_accept_hotkeys` (`3328`), `install_process_shortcut_hotkeys` (`3459`) — are queued from M (`lib.rs:1520-1524`, `1725-1733`, `513-516`) and executed on W.

The only Carbon callback, `carbon_accept_hotkey_handler` (`lib.rs:3782`), is
already on M, exactly as §22.1 recorded.

## 4. Seam analysis for a future implementation (analysis only — nothing proposed)

- **Pure key plan (already extracted).** The accept plan is computed as plain
  `Vec<(u32, i64, u32)>` data before any FFI:
  `accept_keymap().arm_bindings_for_action(action, TAB_HOTKEY_SUPPRESSED.load(...))`
  at `lib.rs:3351-3353` (fn at `3037`, `pub` on `AcceptKeymap`), plus
  `carbon_bindings()` (`3059`). The shortcut plan is likewise pure:
  `shortcut_bindings()` (`3646`) → `has_internal_collision` guard (`3475-3477`)
  → `shortcut_registration_plan` (`2919`) →
  `shortcut_plan_minus_accept_collisions` (`3375-3386`). The thread-bound code
  is only the FFI loops consuming these plans (`3354`, `3495`) — i.e., the
  plan/FFI boundary already exists at exactly the point a hoist would cut.
- **Main-thread executor injection point.** The existing seam is the
  `AdapterAcceptTapInstaller::{Worker, Custom}` enum (`lib.rs:595-598`)
  selected in `accept_tap_installer()` (`1516-1528`);
  `Custom(Arc<AcceptTapInstallerFn>)` is currently populated only by
  `#[cfg(test)]` wiring (`with_worker_test_hooks` `lib.rs:1291`,
  `AdapterTestHooks::accept_tap_installer` `lib.rs:375`, assigned at `1320`).
  A main-thread executor would replace/complement the `Worker` arm's blocking
  closure at `1520-1524`. `dispatch2 =0.3.1` is already a dependency
  (`crates/platform_macos/Cargo.toml:12`) and `DispatchQueue::main().after()`
  is live-proven in `schedule_pasteboard_restore` (`lib.rs:2394-2404`), as
  §22.2 stated.
- **Register/unregister sink attachment.** Today the "sink" is implicit:
  register = `resource.register_hotkey` pushing `EventHotKeyRef`s into the
  resource (`3779`, `3445-3447` region) while the resource lives in W's
  `resources` map (`ax_worker.rs:878`); unregister = the `Drop` impls draining
  the vecs (`3298-3310`, `3396-3406`) triggered by the async `RemoveResource`
  chain (`AxWorkerResource::drop` `ax_worker.rs:511-520` → loop arm
  `951-966`). A register/unregister sink would attach where the install
  functions currently box the resource (`lib.rs:3359`, `3507`) and where the
  Drops currently drain — the same two points §22.2 identified.
- **What crosses threads today (types and bounds).**
  - `Job`: `Box<dyn FnOnce() -> Box<dyn Any + Send> + Send>` (`ax_worker.rs:53`) — `Send` closure, `Send` result.
  - `ResourceInstaller`: `Box<dyn FnOnce() -> Result<WorkerResource, PlatformError> + Send>` (`ax_worker.rs:55-56`) — `Send` closure whose **`WorkerResource = Box<dyn Any + 'static>` result is deliberately NOT `Send`** (`ax_worker.rs:54`) and never leaves W (stored at `878`); only `Result<(), PlatformError>` crosses back (`ax_worker.rs:383-397`).
  - `Message::RemoveResource { id: u64, reply: Option<mpsc::Sender<bool>> }` (`ax_worker.rs:86-88`) — **a `u64` id is already the only teardown payload** crossing M→W.
  - `AxWorkerResource { id, tx: Sender<Message>, closed }` (`ax_worker.rs:188-192`) — `Send`; lives caller-side inside `AcceptTapResource = Box<dyn Any + Send>` (`lib.rs:343-345`).
  - `Arc<AcceptTapHandler>` where `AcceptTapHandler = dyn Fn(AcceptTapEvent) -> AcceptTapDecision + Send + Sync` (`lib.rs:169-171`) — crosses M→W (arm) and is read on M (Carbon handler `3855`).
  - `arm_id: u64` (from `CARBON_ARM_ID` `lib.rs:3279` / `SHORTCUT_ARM_ID` `lib.rs:3367`), stored in both resources (`3287`, `3391`) — the ownership token the id-guard disarm consumes (`3308`, `3404`).
  - `CallbackMessage` over mpsc (`ax_worker.rs:64-74`).
- **Cross-thread token for a future implementation — auto-trait note.** A token
  carrying only a `u64` (or only `Send` fields) is automatically `Send`;
  adding a `Drop` impl does not change `Send`/`Sync` auto-trait status. Qfd
  §22.2's "exactly one `unsafe impl Send` is needed" is therefore a historical
  assumption to reevaluate, not a prescription: the correct expectation is
  **zero** new `unsafe impl`s if the token stays ID-only, and any `unsafe impl`
  that does appear must be individually justified. Location observation (not an
  implementation proposal): a token's natural home is beside the two resource
  structs (`lib.rs:3284`, `3389`) / the arm-id statics (`3279`, `3367`), an
  ID-only wrapper over the already-`u64` arm/resource ids (ids minted at
  `lib.rs:3338`/`3468` and `ax_worker.rs:382`), whose `Drop`/use posts the
  unregister that the current `Drop` impls perform inline on W. There is **no**
  `unsafe impl Send` in the G7 surface today — the crate's only `unsafe impl`
  is `objc2::encode::RefEncode for CGImageOpaque` (`lib.rs:2188`), unrelated.

## 5. Drift: Qfd §22.1 (2026-09-16) vs current code

**Verified drift is line-number drift only.** The structural inventory on disk
matches what §22 already names and what git history shows in the §22-era
commit `ab565a0` (`WorkerShortcutResource` `:3340`,
`install_process_shortcut_hotkeys` `:3410`, both `register_hotkey` methods
`:3360`/`:3675`, `CorrectionConsumer`, grammar id 9, `#[cfg(test)]`
`install_resource` `:336`; shortcut resource since `d8f0e98`, 2026-06-30).
Nothing §22 names has been added, split, or removed since; earlier
"growth"/"split" claims are retracted.

| §22.1 citation | Current | Delta |
|---|---|---|
| `accept_tap_installer` (`lib.rs:1486`) | `lib.rs:1516` | moved +30 |
| `install_resource` (`ax_worker.rs:350`) | `AxWorkerHandle::install_resource` `ax_worker.rs:378` | moved +28; a same-named `#[cfg(test)]` `AxWorker::install_resource` exists at `ax_worker.rs:364` (present at `ab565a0` `:336`; not new) |
| `install_worker_accept_tap_resource` (`lib.rs:3265`) | `lib.rs:3312` | moved +47; current signature takes `AcceptTapKind` and dispatches three ways (`3316-3324`) |
| `ensure_carbon_handler_installed` (`:3430`) | `lib.rs:3510` | moved +80 |
| `register_hotkey` (`:3358`, `:3674`) | two methods, one per resource type: `WorkerShortcutResource::register_hotkey` (`3409`), `WorkerAcceptTapResource::register_hotkey` (`3735`); call sites `3354` and `3495` | line drift only; both methods already present at `ab565a0` (`:3360`/`:3675`) |
| `carbon_accept_hotkey_handler` (`:3714`) | `lib.rs:3782` | moved +68 |
| §6 ledger row line pin (`Qfd.md:842`: `1486,3265,3279,3358,3430,3674`) | all six stale | per the "dated record" lesson, these are citations, not current values |

§22 claims re-verified as still true: `WorkerResource` deliberately not `Send`
(`ax_worker.rs:54`); main synchronously blocked on the worker during Carbon FFI
(`ax_worker.rs:393-397` behind `lib.rs:446-449`); unregister via async
`RemoveResource` with no reply on the production path (`ax_worker.rs:516-519`);
only the dispatch handler on main (`lib.rs:3782`); arm-ID ownership scheme
(`lib.rs:3250-3265`); `rearm_consumer_tap`'s DROP-BEFORE-INSTALL FIFO doc
(`lib.rs:480-486`); `hide_suggestion_after`'s detached second teardown
initiator (`lib.rs:553-556`).

## 6. Automated coverage of the G7 surface — bounded conclusion

**Bounded conclusion: no identified direct deterministic unit coverage of the
native registration/Drop boundary.** The G7-flagged thread-affinity functions —
`ensure_carbon_handler_installed`, both `register_hotkey` methods,
`install_carbon_accept_hotkeys`, `install_process_shortcut_hotkeys`,
`install_worker_accept_tap_resource`, `carbon_accept_hotkey_handler`, and both
resource `Drop` impls — have no identified direct deterministic unit test. This
is a bounded claim: grep/name counts over `lib_tests.rs` (all zero except one
comment at `lib_tests.rs:5209`) cannot prove absence of runtime coverage.

**Inspected coverage that DOES exist around the surface** (none of it
exercises Carbon FFI):

- **`CarbonHandlerSlot` unit tests — 3**: `carbon_slot_serves_the_armed_handler_and_clears_on_matching_disarm` (`lib_tests.rs:3494`), `carbon_slot_stale_disarm_never_clears_a_newer_arm` (`3512`, the out-of-order guard; predates September), `carbon_slot_handler_cloned_out_survives_a_concurrent_disarm` (`3540`). Pure slot logic.
- **Pure plan/keymap tests**: `shortcut_plan_minus_accept_collisions` (`lib_tests.rs:4969-5023`), `shortcut_registration_plan` (`5066`, `5074`), `accept_keymap` rebind/modifier tests (`5259-5319`, `3490`).
- **Controller/subscription orchestration with fake installers**: `set_accept_action`/`hide_suggestion_after` behaviors at `lib_tests.rs:3587`, `3595`, `3628`, `5690`, `5699`, `5795`, `5981`, `6016` — including the delayed-hide drop ordering probe (`3595`) and generation cancel (`6014`). Every test reaches the controller through the `Custom` fake installer (`AdapterTestHooks`, fake closures at `lib_tests.rs:486-498`, `552-560`), never the `Worker` arm.
- **Worker-queue machinery** (`ax_worker.rs` `mod tests`, `ax_worker.rs:1685`): `ax_worker_installs_and_drops_resources_on_worker_thread` (`2406-2434`) and `ax_worker_failed_resource_install_does_not_store_resource` (`2436-2448`) prove `install_resource`/`RemoveResource` thread affinity, panic containment, and bookkeeping — with **fake** resources only.

**Missing current-candidate live evidence (distinct from unit coverage):** the
A1b acceptance runner's accept-tap gates do exercise the real native
registration/unregistration path, but only live on macOS with a GUI session,
and every corresponding ledger row in `docs/ACCEPTANCE.md` remains "never
recorded" — the §22.4a physical-key baseline prerequisite stands. Cross-
compilation and this analysis do not substitute for it.
