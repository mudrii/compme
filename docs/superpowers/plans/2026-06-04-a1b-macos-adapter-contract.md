# A1b - macOS Adapter Contract Plan

**Date:** 2026-06-04  
**Status:** Active A1b macOS adapter acceptance tracker; A1a contract-first implementation has started and must stay aligned with this file.  
**Purpose:** Make the macOS adapter risks concrete before the OS-agnostic engine hardens its `Event`, `Command`, `TextContext`, and `PlatformAdapter` shapes.

## Gate

A1a should not implement the older narrow `PlatformAdapter` snippets as the final contract. Ongoing A1a work must stay revised against this A1b contract.

## Required Platform Contract

`platform` should model the validated cross-platform contract, even if A1b is the first real implementation:

```rust
pub trait PlatformAdapter: Send + Sync {
    fn environment(&self) -> Environment;
    fn subscribe_focus(&self, cb: FocusCallback) -> Result<Subscription, PlatformError>;
    fn subscribe_caret(&self, cb: CaretCallback) -> Result<Subscription, PlatformError>;
    fn front_app(&self) -> Option<AppId>;
    fn capabilities(&self, field: &FieldHandle) -> Result<Capabilities, PlatformError>;
    fn read_context(&self, field: &FieldHandle) -> Result<TextContext, PlatformError>;
    fn caret_rect(&self, field: &FieldHandle) -> Result<Option<ScreenRect>, PlatformError>;
    fn insert(&self, field: &FieldHandle, text: &str, strategy: InsertStrategy)
        -> Result<Inserted, PlatformError>;
}
```

Minimum shared types:

- `FieldHandle`: opaque focused-field identity, including pid/element identity where available.
- `TextContext`: `left`, `right`, `selection`, `caret`, `source`, `field_id`, and offset encoding.
- `Capabilities`: include `secure`, `security_state`, `toolkit`, `insert_strategy`, `accept_intercept`, `overlay_at_caret`, and `coords_global_screen`.
- `PlatformError`: distinguish permission missing, secure input, cannot complete, unsupported field, timeout, stale field, and app exited.
- `Environment`: macOS version, display topology notes if known.

## A1a Changes Forced By A1b

- `Focus` events need a `FieldHandle` or focus token, not just capabilities.
- `TextChanged` needs edit metadata: insert/delete/paste/unknown, previous caret/value hash, and trigger-policy context.
- `Focus`, secure-state changes, app changes, and field identity changes must emit `Hide` if a suggestion is visible.
- Core must distinguish unsupported, blocked, popup, and inline modes for diagnostics.
- Core should request completions against a `TextContext` snapshot token, not raw `(value, caret)` only.

## macOS Adapter Tasks

Implementation status as of 2026-06-05:

- Task 1 production scaffold is in place: `AxWorker` runs one dedicated AX worker thread, runs timeout setup on that worker, calls `AXUIElementSetMessagingTimeout(systemWide, 0.05)` in the real constructor, releases the system-wide AX element under CoreFoundation's Create Rule, maps AX errors into `PlatformError`, and pumps the worker `CFRunLoop` on idle timeouts and after jobs.
- Task 2 focus/caret subscriptions are in place: `AXObserver` registration/removal, stable refcon plumbing, worker-owned observer resources, dynamic frontmost-pid rebind, stale old-pid callback suppression, focused-element caret observer registration with app fallback, safety polling, caret coalescing, stable field reuse, callback rect propagation, and off-worker callback delivery are implemented.
- Task 3 security and ownership are in place: owner pid/identifier/role/subrole resolution, secure-field blocking from `AXSecureTextField`, global Secure Input blocking through `IsSecureEventInputEnabled`, stale-field app-exit mapping, and global Secure Input diagnostic priority over secure-field subrole are implemented.
- Task 4 caret geometry is in place and live-validated: native `AXBoundsForRange` zero-length, previous-character fallback, and container rejection remain covered; Chromium/WebKit marker-first geometry uses local SDK-confirmed `AXSelectedTextMarkerRange` and `AXBoundsForTextMarkerRange` strings with fallback to the native ladder. `caret_marker_acceptance` now snapshots a passing diagnostic during the observer window so late browser `AXApplication` focus churn cannot overwrite valid text-field marker evidence. Safari textarea marker-path acceptance passed with `source=Marker` in `tools/acceptance/logs/a1b-live-20260605-102320/caret-marker-browser-marker.attempt-1.log`.
- Task 5 accept interception substrate is in place and live-validated: `PlatformAdapter::subscribe_accept` returns an `AcceptSubscription`, installs a permanent listen-only `CGEventTap`, creates the active consuming tap only while suggestions are visible, consumes Tab/keycode 48 only when a precomputed `AcceptAction` is armed, dispatches `AcceptAction::Full` or `AcceptAction::Word` off the tap callback, ignores tagged self-generated events, re-enables taps on tap-disabled events, and supports delayed consuming-tap teardown after synthetic insertion. `accept_tap_acceptance` passed inactive/full/word/delayed-hide gates in the default live runner.
- Task 6 insertion planning is in place and live-validated for TextEdit: `AxSet`, tagged `SyntheticKeys`, tagged `Clipboard`, stale-focus rejection before global event posting, item/type pasteboard snapshot/restore for eager contents, provider-backed pasteboard snapshot materialization, `changeCount`-guarded clipboard restore to avoid overwriting newer user/app clipboard changes, and `None` strategy planning are implemented. Synthetic and clipboard insertion now post to the target pid; clipboard paste uses an explicit Command-down/V-down/V-up/Command-up sequence. The default live runner passed TextEdit `SyntheticKeys`, `Clipboard`, `AxSet`, full accept insertion, and word accept insertion.
- Task 7 overlay bridge is in place and live-validated: `platform::OverlayPresenter` and `MacosOverlayPresenter` support `show_ghost`, `update_ghost`, and `hide` through a transparent click-through non-activating `NSPanel`; presenter construction and operations require the AppKit main thread. `overlay_presenter_acceptance` now asserts diagnostics for visible show/update, hidden after hide, click-through, non-activating panel style, `can_become_key_window=false`, and level `101`.
- Task 8 popup fallback is in place and live-validated: `popup_fallback_acceptance` launches a repo-local AppKit child fixture that exposes mutable AX value plus selected range but no parameterized caret bounds, then validates it externally through `MacosPlatformAdapter`. The fixture reports `RECT Ok(None)` and `CAPS ... readable_caret: false ... overlay_at_caret: None`, causing `ux=Popup`, then inserts through `InsertStrategy::AxSet` and verifies `READ_AFTER_INSERT` returns the mutated value. The adapter treats `kAXErrorParameterizedAttributeUnsupported` from bounds queries as no caret geometry, while still propagating stale/hard AX failures.
- Current automated evidence: `cargo fmt --check`, `cargo test -p platform_macos`, `cargo test --workspace --all-targets`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo build --workspace --all-targets` all pass after the popup fixture and runner updates. The `--all-targets` test command is required because the popup fallback regression coverage lives in example targets. The current default live runner passed with `Summary: pass=13 fail=0 skip=1 logs=/Users/mudrii/src/complete-me/tools/acceptance/logs/a1b-live-20260605-104813`. The current browser-marker split passed with `Summary: pass=7 fail=0 skip=7 logs=/Users/mudrii/src/complete-me/tools/acceptance/logs/a1b-live-20260605-104257`.
- Native macOS inline prediction suppression decision: do not attempt cross-app suppression in A1b. Current AppKit bindings expose `setAutomaticTextCompletionEnabled(false)` for owned `NSTextView`/`NSTextField` controls, but Complete Me is targeting other applications' text fields through Accessibility plus overlay rendering. Treat native prediction suppression as a future app-specific integration/settings item, not a blocker for A1b development start.

### Task 1: AX worker thread and timeout

- Create one dedicated AX worker thread.
- Call `AXUIElementSetMessagingTimeout(systemWide, 0.05)` during setup.
- Never perform AX reads from the AppKit main thread or CGEventTap callback.
- Convert AX errors into `PlatformError`.

Acceptance:

- Wedged/unsupported focused elements return timeout/cannot-complete errors without blocking the main thread.
- Focused TextEdit context reads still work.

### Task 2: Focus and caret subscriptions

- Use `AXObserver` for focused UI element changes and selected-text changes where available.
- Coalesce caret events.
- Add a low-rate safety poll for apps that under-report changes.

Acceptance:

- Typing and caret movement in TextEdit produce bounded event volume.
- Focus changes emit a new field token and cause engine `Hide`.

### Task 3: Field ownership and secure block

- Resolve owner pid from the AX element, not `NSWorkspace.frontmostApplication`.
- Detect `AXSecureTextField` subrole.
- Detect global Secure Input (`IsSecureEventInputEnabled`) and map to `SecurityState::SecureInputEnabled`.

Acceptance:

- Password fields and stuck Secure Input block reads/inserts.
- Diagnostics identify the block reason.

### Task 4: Caret ladder including web path

- Implement native tiers already proven in spike: zero-length `kAXBoundsForRangeParameterizedAttribute`, previous-character fallback, container rejection.
- Add Chromium/WebKit path: `AXSelectedTextMarkerRange` to `AXBoundsForTextMarkerRange`.
- Add coordinate normalization for Retina and multi-monitor cases.

Acceptance:

- TextEdit reports a usable rect.
- Chrome/Safari textarea either reports a web-marker rect or explicitly records fallback tier.
- No container-sized rect is treated as an inline caret.

### Task 5: Two-tap accept interception

- Permanent `ListenOnly` observer tap.
- Transient active `Default` consuming tap only while a suggestion is visible.
- Re-enable on tap-disabled events.
- Defer teardown briefly after synthetic insertion.
- Consume only accept shortcuts from precomputed engine state.

Acceptance:

- Tab passes normally when no suggestion is visible.
- Tab is swallowed when a suggestion is visible.
- Other apps do not exhibit perceptible input lag.

### Task 6: Synthetic event tagging and insertion planner

- Tag self-generated events and ignore them in taps.
- Implement at least `AxSet`, `SyntheticKeys`, and `Clipboard` strategy selection.
- Do not allow self-inserted `AcceptWord` text to invalidate the remaining suggestion.

Acceptance:

- Full accept inserts once.
- Word accept inserts the first word and keeps the remainder visible.
- No double-insert or immediate self-dismiss.

### Task 7: Overlay bridge

- Render ghost text through non-activating `NSPanel`.
- Use AppKit only on the main thread.
- Support `Hide`, `ShowGhost`, and `UpdateGhost`.
- Document native macOS inline prediction suppression as deferred for cross-app fields unless a future owned-control integration is added.

Acceptance:

- Overlay appears at/near caret, does not steal focus, and hides on focus/caret invalidation.

## A0/A1 Gate Evidence

These criteria are historical readiness gates that have been satisfied or carried forward; remaining A1b product blockers are listed above.

- P3/P4/P5/P5b/P6/P7 manual acceptance is recorded.
- Chromium/Web caret support is either proven or explicitly moved into A1b Task 4 as a blocker.
- A1a plan is updated to the contract above.
- Model choice is resolved in A0 findings: A1a development default is `tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf` with the terse continuation prompt, while model path/prompt strategy must remain configurable.
- Deprecated `llama-cpp-2` decode calls are removed from the real-code plan.
