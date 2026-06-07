# Architecture

Complete Me is split into a pure completion core, a platform contract, a macOS
adapter, and a local model seam. The current implementation focuses on macOS
because the hard integration points are Accessibility, event taps, AppKit
overlays, Secure Input, and pasteboard behavior.

## System Overview

```text
Focused app text field
        │
        ▼
platform_macos::MacosPlatformAdapter
        │  focus/caret subscriptions, text reads, capabilities, inserts
        ▼
platform contract types
        │  FieldHandle, TextContext, Capabilities, InsertStrategy
        ▼
core::SuggestionMachine
        │  deterministic event -> command state machine
        ▼
model_client::LocalModel
        │  llama.cpp-backed completion
        ▼
core::SuggestionMachine
        │  validates generation + field snapshot
        ▼
platform_macos
        │  overlay, accept tap, insertion
        ▼
Focused app text field
```

## Workspace Crates

### `platform`

`platform` defines the cross-platform boundary. It intentionally contains data
types and traits rather than macOS-specific behavior.

Key concepts:

- `FieldHandle`: stable field identity used to tie completions and inserts to a
  focused field.
- `TextContext`: text to the left and right of the caret, selection metadata,
  source, field identity, and offset encoding.
- `Capabilities`: what the focused field supports: readable text, readable
  caret, write support, secure-state information, toolkit, insertion strategy,
  accept interception, and overlay placement.
- `InsertStrategy`: `AxSet`, `SyntheticKeys`, `Clipboard`, `ImeCommit`, or
  `None`.
- `PlatformAdapter`: focus/caret/accept subscriptions, app discovery,
  capabilities, context reads, caret geometry, and insertion.
- `OverlayPresenter`: `show_ghost`, `update_ghost`, and `hide`.
- `ux_mode`: classifies capabilities as `Inline`, `Popup`, `Hotkey`,
  `Unsupported`, or `Blocked`.

### `context`

`context` is pure text handling around caret indexes:

- `left_context`
- `right_context`
- `left_tail`
- `trim_prefix`

These helpers avoid platform dependencies and are tested with Unicode-safe
cases.

### `ranker`

`ranker` contains lightweight candidate shaping:

- `trim_to_stop_boundary`: cut a raw completion at the first line break before
  any word capping, so inline ghost text stays a single visual line.
- `truncate_at_sentence_end`: cut at the first sentence terminator (`.`/`!`/`?`
  followed by whitespace or end-of-text), so a completion stops at one sentence;
  decimals like `3.14` are preserved.
- `strip_suffix_overlap`: drop a trailing run of the candidate that already
  exists to the right of the caret (small models regurgitate post-caret text);
  comparison is case- and punctuation-insensitive on whole words.
- `is_degenerate_repetition`: report a single word or phrase repeated three or
  more times (`the the the`) so the caller can suppress the loop.
- `cap_words`
- `next_word`
- `repetition_penalty`: returns a sub-floor factor when the candidate repeats a
  contiguous run of recent words verbatim.

The implementation stays small; per-app scoring and learned ranking remain
future work.

### `core`

`core` owns the deterministic suggestion workflow. `SuggestionMachine` consumes
events and emits commands. It does not call macOS APIs or model APIs directly.

Important events:

- `Focus`
- `TextChanged`
- `CaretMoved`
- `Tick`
- `CompletionReady`
- `SecureStateChanged`
- `AcceptFull`
- `AcceptWord`
- `Dismiss`

Important commands:

- `RequestCompletion`
- `ShowGhost`
- `UpdateGhost`
- `Hide`
- `Insert`

The machine tracks:

- generation numbers
- snapshot IDs
- the active field
- pending debounce state
- the requested completion
- the currently shown suggestion

This prevents stale model output from being shown or inserted after focus,
caret, secure-state, or text changes.

Before a completion is shown, the machine shapes it through `ranker` in order:
`trim_to_stop_boundary` cuts at the first line break, `truncate_at_sentence_end`
cuts at the first sentence end, `strip_suffix_overlap` removes any tail the user
already has to the right of the caret, and `cap_words` enforces the word cap.
The shaped candidate is then suppressed entirely when a `repetition_penalty`
below `REPETITION_PENALTY_FLOOR` shows it repeats nearby text, or when
`is_degenerate_repetition` flags a repeated-word loop.

### `model_client`

`model_client` defines the model boundary:

- `LocalModel`: synchronous local completion trait.
- `LocalModelError`: structured failure stage plus message.
- `LlamaModel`: `llama-cpp-2` implementation using Metal via
  `with_n_gpu_layers(999)`. Overrides `warm_up` (a throwaway decode that
  triggers Metal shader compile up front) and `shutdown` (drops the model
  before the backend, in order, to avoid the ggml exit-abort).
- `terse_continuation_prompt`: the current development prompt shape.

The current `LlamaModel` creates a fresh llama context per completion.
Long-lived actor lifecycle, prefix-cache reuse, and serialized production model
access are not implemented in this crate yet.

### `engine`

`engine` is the runtime host that wires `SuggestionMachine` with a
`PlatformAdapter` and an `OverlayPresenter`. It drives the suggestion loop:
subscribing to platform events, feeding them into the machine, and dispatching
the resulting commands back to the platform and overlay layers.

Beyond translating platform callbacks into core `Event`s, the host exposes two
adapter-driven entry points required by the A1b macOS contract:

- `on_secure_state`: forwards a fresh `Capabilities` reading into the machine as
  a `SecureStateChanged` event when Secure Input or secure-field status flips.
- `set_accept_subscription`: hands the adapter's accept-tap subscription to the
  host so accept events reach the machine while a suggestion is armed.

### `app`

`app` owns the `complete-me` binary and the runtime wiring that P0/P1 validated.
It is the only root crate that combines config loading, AppKit run-loop pumping,
the menu-bar status surface, model selection, the inference worker, signal
handling, and ordered shutdown.

Major responsibilities:

- load dotenv-style config plus environment overrides
- choose `StubModel` or `LlamaModel`
- warm the model before serving suggestions
- marshal platform callbacks onto the AppKit main-thread engine host
- keep only the latest pending completion request
- derive loading/ready/disabled/blocked status for tray gating
- dismiss suggestions when the app is disabled
- shut down inference before dropping engine/overlay/platform resources

### `platform_macos`

`platform_macos` implements the platform contract for macOS.

Major responsibilities:

- Dedicated AX worker thread for Accessibility calls.
- Focus and caret `AXObserver` registration.
- Dynamic rebind when the frontmost PID changes.
- Focused-element safety polling.
- Secure Input and secure-field blocking.
- Stable field identity from AX owner PID, identifier, role, subrole, and raw
  pointer fallback.
- Text reads through AX value and selected range.
- Caret geometry through native range bounds and Chromium/WebKit marker
  attributes.
- Capability classification for inline and popup UX.
- Insert planning across `AxSet`, `SyntheticKeys`, and `Clipboard`.
- Stale-focus rejection before global synthetic or paste insertion.
- Pasteboard snapshot/restore with `changeCount` guard.
- Split accept interception using a permanent observer tap and transient
  consumer tap.
- AppKit `NSPanel` overlay presenter that is transparent, click-through, and
  non-activating.

## macOS Runtime Model

### AX Worker

Accessibility operations are routed through a dedicated worker thread. This
keeps AX calls serialized and gives the adapter one place to own observer
resources and run-loop sources.

The worker handles:

- synchronous jobs
- resource installation/removal
- observer events
- focused-element polling
- run-loop pumping during idle intervals

### Focus and Caret Observation

Focus subscription:

- observes app-level focus changes
- suppresses duplicate semantic field identities
- creates new `FieldHandle` generations as focus changes

Caret subscription:

- prefers focused-element observer registration with app fallback
- emits stable fields for caret events
- coalesces duplicate caret events within a short interval
- forwards optional caret rectangles from observer events

Both subscriptions maintain current binding state and ignore callbacks from
stale PIDs after a frontmost app change.

### Capability and UX Classification

The adapter reads field characteristics and maps them into `Capabilities`.
`platform::ux_mode` then decides:

- `Blocked`: secure field or global Secure Input.
- `Unsupported`: unreadable or unwritable fields, or no insertion strategy.
- `Hotkey`: fields requiring hotkey-only interception.
- `Inline`: readable caret plus usable overlay placement.
- `Popup`: text can be read/written, but there is no usable caret geometry for
  inline overlay.

Popup fallback is important because some writable AX fields expose value and
selection but not usable parameterized caret bounds.

### Insertion Strategies

The macOS adapter supports:

- `AxSet`: write text by setting AX value and selected range.
- `SyntheticKeys`: post tagged synthetic key events to the target PID.
- `Clipboard`: snapshot pasteboard contents, write the insert text, paste, then
  restore only if the pasteboard change count is still safe.

Global strategies are rejected if the frontmost PID has moved away from the
field's PID before insertion.

### Accept Interception

Accept interception uses two event-tap roles:

- Observer tap: permanent, listen-only, used to keep tap infrastructure active.
- Consumer tap: installed only while a suggestion is visible and armed with an
  `AcceptAction`.

The consumer tap is keycode-driven while armed (a completion is visible):
**Tab (keycode 48) accepts the next word**, **grave/backtick (keycode 50, the
key above Tab) accepts the whole completion** — matching Cotypist's default
binding. The armed `AcceptAction` is only a visibility gate; the keycode picks
the action. Tagged self-generated events are ignored to avoid swallowing the
app's own synthetic insertion events.

### Overlay Presenter

`MacosOverlayPresenter` is AppKit-main-thread-only. It renders ghost text in a
borderless non-activating `NSPanel`, with click-through enabled. The panel is
shown at global screen coordinates derived from AX caret geometry.

## Spike Workspace

`tools/spike` is a separate Rust package used to prove risky implementation
paths before they move into root crates.

Spike binaries:

- `p1_build`: native build and Metal setup proof.
- `p2_infer`: real llama completion proof.
- `p2_model_compare`: base-vs-instruct prompt comparison.
- `p3_axread`: AX text read proof.
- `p4_caret`: caret geometry proof.
- `p5_tap`: single accept tap proof.
- `p5_twotap`: observer/consumer tap split proof.
- `p6_overlay`: AppKit overlay proof.
- `p7_smoke`: read -> infer -> overlay smoke path.

The spike should not be treated as production architecture. It is retained as
evidence and a reproducible harness for low-level behavior.

## Documentation Sources

The high-detail design and validation records live under:

- `docs/superpowers/specs/`
- `docs/superpowers/plans/`
- `tools/spike/FINDINGS.md`
- `tools/spike/MANUAL-ACCEPTANCE.md`

The root docs summarize current repository behavior and point back to those
records for detailed evidence.
