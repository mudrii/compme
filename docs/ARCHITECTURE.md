# Architecture

Compme is split into a pure completion core, a platform contract, platform
adapters, a local model seam, and a ring of small pure feature crates (text
features, gating, personalization, privacy, catalog/download). The current
implementation focuses on macOS because the hard integration points are
Accessibility, event taps, AppKit overlays, Secure Input, and pasteboard
behavior.

The workspace now holds 24 crates. The shape is deliberate: almost everything
outside the model/download seams, platform adapters, and host is pure (text in →
decision out, time and keys injected, no I/O), so it is unit-testable without a
clock, a network, or AppKit. The impurity is fenced into `model_client`
(llama.cpp), `model_fetch` (network), the `platform_*` adapter crates, and
`app`.

## System Overview

```text
Focused app text field
        │
        ▼
platform_macos::MacosPlatformAdapter
        │  AX worker: focus/caret subscriptions, text reads, capabilities,
        │  inserts, accept interception (Carbon), overlay, tray, settings window
        ▼
platform contract types
        │  FieldHandle, TextContext, Capabilities, InsertStrategy
        ▼
app run loop ──────────────────────────────────────────────┐
        │  marshals platform callbacks onto the AppKit main  │ local-replacement
        │  thread; owns policy (prefs, compat, personalize)  │ fast path:
        ▼                                                    │ emoji / autocorrect
engine::Engine ── engine_core::SuggestionMachine             │ / localize /
        │  deterministic event -> command state machine      │ thesaurus
        │  (shapes candidates through `ranker`)              │ (no model)
        ▼                                                    │
model_client::LocalModel                                     │
        │  llama.cpp-backed completion (worker thread)        │
        ▼                                                    │
engine_core::SuggestionMachine                               │
        │  validates generation + field snapshot              │
        ▼                                                    ▼
platform_macos
        │  overlay, accept interception, insertion
        ▼
Focused app text field

side stores (all opt-in / off by default):
  memory  — encrypted accepted-completion log (redaction → AES-256-GCM)
  stats   — rolling 30-day acceptance counters + sparkline (menu bar)
  prefs / compat / webconfig — per-app + per-domain gating and overrides
```

Two suggestion paths share the gate. The **model path** runs left-context
through the engine/state-machine and llama.cpp. The **local-replacement path**
short-circuits in the observe path for the four deterministic text features
(emoji shortcode, typo fix, US→UK, thesaurus) — no model, no latency. Both
honor the same per-app/per-domain prefs gate.

## Workspace Crates

The 24 crates fall into six groups: the **contract + core** (`platform`,
`engine_core`, `engine`, `context`, `ranker`), the **model seam**
(`model_client`, `model_catalog`, `model_fetch`), **pure text features**
(`autocorrect`, `localize`, `thesaurus`, `emoji`, `textcase`), **policy &
privacy** (`prefs`, `compat`, `personalization`, `redaction`, `memory`,
`stats`, `webconfig`), **platform adapters** (`platform_macos`,
`platform_windows`, `platform_linux`), and the **host binary** (`app`). The
non-platform feature crates are pure and OS-agnostic, with time and keys
injected, so the host owns all I/O, clocks, and toggles.

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

### `autocorrect`

`autocorrect` is the **typo-fix / suggested-fix** half of the §16 gate: a
curated, high-precision/low-recall table that maps an unambiguous misspelling
to its correction, reapplying the query's capitalization (via `textcase`). A
real word is never "corrected", so there are no false positives on valid input;
ambiguous strings that are also real words (`cant`, `wont`, `weve`) are
deliberately excluded. Full statistical autocorrect is a separate host concern.
The host wires it into the local-replacement path (`replacement_offer`) and
gates it on `COMPME_AUTOCORRECT`; it must not run in code editors.

### `localize`

`localize` is British-English normalization (§16): a curated US→UK spelling
table that maps an American-only form to its British equivalent, reapplying the
typed capitalization. Like `autocorrect` it is high-precision/low-recall —
every key is American-only, so an already-British or shared spelling is never
altered, and genuinely ambiguous forms (`meter`, `tire`, `check`, `license`,
`practice`, `program`, `draft`) are excluded. Whole-word only; the host decides
*when* via the `COMPME_BRITISH_ENGLISH` toggle (off by default) and feeds it
through the local-replacement path.

### `thesaurus`

`thesaurus` looks a word up in a curated synonym table and returns the
alternatives, applying the queried word's case pattern (`textcase`) so a host
can drop a replacement straight in. The host gates it on the "thesaurus" toggle
and currently offers synonyms for the trailing word at the caret (*auto* mode).
Selection-trigger UX (highlight a word → offer synonyms) remains future A2/A3
work; the same lookup table is intended to serve both modes once that host
surface exists. Mirrors Cotypist's `featureThesaurusAutoMode` today and keeps
`featureThesaurusSelectionMode` as tracked parity work.

### `emoji`

`emoji` suggests an emoji when the user types a `:shortcode`, honoring
skin-tone (Fitzpatrick U+1F3FB..U+1F3FF) and gender preferences. Pure:
detection + table lookup + modifier application. The host reads
`COMPME_EMOJI` / `_SKIN_TONE` / `_GENDER`, offers the emoji ghost through the
local-replacement path, and performs the actual replacement. Combining skin
tone *and* gender (full ZWJ assembly) is deferred — a gendered match keeps the
default skin tone.

### `textcase`

`textcase` detects a capitalization pattern and re-applies it to a replacement
word or phrase, shared by `autocorrect`, `localize`, and `thesaurus` so a
substituted word carries the same case the user typed. Pure and OS-agnostic.

### `prefs`

`prefs` is the suggestion-gating policy core (§8 / §16): per-app and per-domain
enable/exclude, per-app Tab-key disable, and a global pause/snooze. Pure — a
policy struct plus deterministic resolution against a caller-supplied clock
(`now_ms`), so every transition is unit-testable. The run loop resolves
`suggestion_gates_pass(app_key, left, domain, now_ms)` before either suggestion
path produces output. Persistence and the settings UI live in the host.

### `compat`

`compat` classifies a macOS bundle id into a compatibility tier and the policy
that tier implies — the deterministic core behind the §16 compatibility-parity
table (mirroring `cotypist.app/compatibility`). It encodes per-app UX quirks
(e.g. apps whose caret rect collapses to a line, omnibox/address-bar detection,
mirror-window and setup-needed apps) so the host can pick the right insertion
and overlay behavior. Live per-app verification is environment-bound; this
crate is the pure classifier that drives gating.

### `personalization`

`personalization` templates prompt-based steering (§6) into a preamble that the
host prepends to the completion prompt: custom instructions (global + per-app +
per-domain instruction maps), a 6-stop strength slider, and sender identity.
The app config parser fills the maps from target-list keys plus sanitized
per-target instruction keys; request-time app and domain steering are live, with
browser domains copied onto completion requests before inference builds the
preamble. The remaining personalization work is the settings editor surface for
mode/instructions/strength, not runtime steering. Pure and dependency-free —
no ML, no I/O. The 6 strength stops have full reach for every user; Cotypist's
Free/Plus/Pro caps are paywall artifacts deliberately not cloned.

### `redaction`

`redaction` scrubs sensitive text before any persistence or diagnostics (§6/§7)
— emails, Luhn-valid 13–19 digit card-like runs, and high-entropy tokens/
secrets. Pure: text in, redacted text out, run email → secret → card so a long
email local-part is redacted whole. It is best-effort and deliberately
over-redacts (privacy over fidelity): a false positive loses a little stored
context, a false negative would leak a secret. `memory` runs every record
through it before encryption.

### `memory`

`memory` is the encrypted local log for accepted completions or all monitored
typing (§6 / §16). Text is **redacted** (`redaction`) then **encrypted**
(AES-256-GCM, a random nonce per record); only text ciphertext reaches the
SQLite database — text plaintext never touches disk. The app identifier remains
plaintext metadata for per-app counts/delete and is also bound into the AEAD as
AAD, so rows cannot be relabeled and decrypted under another app. The 32-byte
key comes from a `KeyProvider`: production reads it from the macOS Keychain (A3
live integration), tests use a fixed key. Storage is opt-in —
`StorageMode::Off` is the default and records nothing; `AcceptedOnly` stores
accepted completions, `AllMonitored` is the broader opt-in. Records are
inspectable (`count` / `recent`) and deletable (`delete_all` / `delete_app`).

### `stats`

`stats` is a pure accumulator over a rolling 30-day window (§11 / §16): shown /
accepted / dismissed / superseded counts, a words-completed count for the menu
bar, and latency. Time is injected — callers pass `now_ms` on every record and
query — so the window logic is deterministic; counts are filtered to the last
30 days on read and pruned on write. The host renders the Statistics pane and
menu-bar surface; `stats::sparkline` produces the per-day bar series shown in
the settings window.

### `webconfig`

`webconfig` parses `compme://setOverride` deep links — the
safe, reversible, user-visible subset of Cotypist's URL-scheme config pushes.
The parser is strict and fail-closed: it accepts only the `compme` scheme and
`setOverride` command, exactly one scope (`app` XOR `domain`) and one action
(`enabled` XOR `excluded`), and rejects unknown commands/params, malformed
scopes, and any percent-encoding. Anything non-reversible (custom instructions,
model paths, security settings) requires `LinkTrust::Signed`:
`parse_deep_link_with_trust` verifies a trailing `&sig=<128 hex>` **Ed25519**
signature over the exact URL byte-prefix against a host-pinned `TrustedKey`,
with no canonicalization and fail-closed when no key is configured. The §16
web-config flow is wired end-to-end: `platform_macos::url_events` installs the
Apple-Events `compme://` URL-scheme handler, and the run loop drains each link
through a host confirmation prompt before `handle_deep_link` applies it.

### `engine_core`

`engine_core` (renamed from `core`) owns the deterministic suggestion workflow.
`SuggestionMachine` consumes events and emits commands. It does not call macOS
APIs or model APIs directly.

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

**[Updated 2026-06-08 — G3 closed]** `LlamaModel` now runs on a dedicated worker
thread owning a **persistent** `LlamaContext`. `complete()` reuses the KV cache
for the shared prompt prefix (`reusable_prefix_len` + `clear_kv_cache_seq`,
re-decoding only the divergent suffix) and serializes calls via a mutex held
across the round-trip; the backend is a `'static` singleton. (Earlier drafts of
this doc said a fresh context is created per completion — that is no longer true;
see design spec §15 G3.)

### `model_catalog`

`model_catalog` is the pure catalog (§15 D14) of which local GGUF models the
host offers: display name, download URL, byte size, license, and a
`RamVerdict`. `bytes_to_whole_gb` and `ram_verdict` turn a model size plus the
machine's RAM (probed via `sysctl` in the host, not here) into a fit verdict:
`Fits` and `Tight` are offerable labels, while `Exceeds` is a hard download
gate answered by `offerable_by_ram`. The catalog is static Rust data, not a TOML file: the same
in-repo content, no parser dependency, and invalid entries become compile
errors. Everything here is pure; the RAM probe and IO are later slices.

### `model_fetch`

`model_fetch` is the model downloader (§15 D14), two halves in one crate: a
pure core (SHA-256 integrity, resume planning — unit-testable with no IO) and a
blocking network half (`download_url` over `ureq` with resume/restart/verify,
plus a `ModelDownloader` worker thread). The download protocol is
`.part` → verify SHA-256 → atomic rename, so a partial download never
masquerades as complete. The seam stays inside the crate so protocol tests can
drive the real network code against a loopback mini-server; nothing here
touches AppKit or the engine.

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

`app` owns the `compme` binary and the runtime wiring. Its `run_loop` is the
single place where the pure crates meet AppKit: it loads config, owns all
policy (prefs, compat, personalization), marshals platform callbacks onto the
main thread, and dispatches both suggestion paths. It is the only root crate
that combines config loading, AppKit run-loop pumping, the menu-bar status
surface, the settings window, model selection/download, the inference worker,
signal handling, and ordered shutdown.

Major responsibilities:

- load dotenv-style config plus environment overrides
- choose `StubModel` or `LlamaModel`; warm the model before serving
- resolve the prefs/compat gate, then drive either the model path (engine →
  llama.cpp) or the local-replacement path (`replacement_offer`: emoji,
  autocorrect, localize, thesaurus) in the observe path
- compute the browser page domain from the focused element's AX URL and feed it
  into the per-domain gate
- apply per-app mid-line override live on focus via `Engine::set_allow_mid_word`
- marshal platform callbacks onto the AppKit main-thread engine host
- keep only the latest pending completion request
- compose the settings window panes (Setup checklist, General switches, Apps
  recorded-input counts, Context/Emoji controls, Shortcuts bindings, Statistics
  sparklines, About) and apply tray/window flags each heartbeat
- pick the download target from `model_catalog` with a RAM-fit verdict,
  enforce the click-through license gate, and spawn the `model_fetch` worker
- apply parked accept-key rebinds in the PINNED order (set keymap → re-arm →
  persist only on success), reverting on failure
- derive loading/ready/disabled/blocked status for tray gating
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
- Transient Carbon `RegisterEventHotKey` accept interception, armed only while
  a suggestion is shown, with rebindable keys + modifier masks (`AcceptKeymap`).
- AppKit `NSPanel` overlay presenter that is transparent, click-through, and
  non-activating.
- `NSStatusItem` tray with a template menu-bar icon and status menu.
- An 8-tab settings `NSWindow` shell (render-only; the run loop owns policy),
  including the `KeyRecorderField` accept-key recorder.

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

Accept keys are intercepted with a **transient Carbon `RegisterEventHotKey`**,
registered only while a suggestion is shown and torn down when it hides — the
key reaches the focused app normally whenever no ghost is visible. The default
binding mirrors Cotypist: **Tab accepts the next word**, **grave/backtick (the
key above Tab) accepts the whole completion**, with fixed bare keys for
dismiss/cycle.

The binding is swappable at runtime through `AcceptKeymap`, which now carries
**Carbon modifier masks** alongside the keycodes (`word_mods` / `full_mods`).
The collision identity is therefore `(keycode, mask)`, and `register_hotkey`
forwards the mask, so the two accept keys can share a keycode with different
modifiers, or carry combos like ⌃⌥⇧⌘. A mask of `0` reproduces the bare-key
behavior exactly, so the pre-modifier config format still reads.

Bindings can be rebound live from an in-app **`KeyRecorderField`** — an `NSView`
overlay (not an `NSTextField`) that captures `keycode + modifierFlags` from a
live keystroke, maps the NSEvent flags onto the same Carbon mask bits, and
**parks a rebind request** for the run loop to apply. The run loop applies it in
a PINNED order — set keymap first, re-arm the registered hotkeys second, and
persist only after the re-arm succeeds, reverting on failure. The Shortcuts
settings pane renders the current binding with ⌃⌥⇧⌘ glyph labels. Self-
generated synthetic insertion events are tagged and ignored so the app never
swallows its own inserts.

### Overlay Presenter

`MacosOverlayPresenter` is AppKit-main-thread-only. It renders ghost text in a
borderless non-activating `NSPanel`, with click-through enabled. The panel is
shown at global screen coordinates derived from AX caret geometry.

### Tray

The menu-bar surface is an `NSStatusItem` carrying a **template menu-bar icon**
— a caret + double-chevron PNG embedded via `include_bytes!` and marked
`setTemplate(true)` so macOS tints it for light/dark menu bars. (This replaced
the earlier "CM…" text title.) The status menu exposes enable/disable and the
settings window; the run loop drives loading/ready/disabled/blocked state into
it on each heartbeat.

### Settings Window

The settings window is an 8-tab AppKit `NSWindow` — **Setup, General, Apps,
Context, Emoji, Shortcuts, Statistics, About** (an `NSTabView`). It is
render-only: the run loop
owns all policy and pushes pane contents and reads back UI intents through a
flags struct each heartbeat (the tray-flags pattern). Because the app is an
`LSUIElement` accessory, showing the window promotes the activation policy to
`Regular`; a visibility *poll* (not a window delegate) detects any close —
including the red button — and demotes back to `Accessory`, so no Dock icon is
stranded.

The Setup tab carries a **model picker** (`NSPopUpButton`) that selects the
download target from `model_catalog`, shown with a RAM-fit label; `Exceeds`
models are blocked before the click-through **license gate**, and a
dest-exists guard avoids redundant downloads. The Statistics tab renders the
`stats` sparkline rows; the Apps tab lists per-app recorded-input counts with
per-row delete; General carries feature toggles (autocorrect, trailing-space,
etc.); the Shortcuts tab shows the current accept binding and hosts the
recorder.

### Model Catalog, Fetch, and Picker

`model_catalog` supplies the offered models and the pure RAM-fit verdict; the
host probes machine RAM via `sysctl`, renders the label, and blocks `Exceeds`
downloads before the license/fetch edge. When the user picks an offerable model
and accepts its license, the run loop spawns the `model_fetch`
`ModelDownloader` worker, which downloads to a `.part` file, verifies the
SHA-256, and atomically renames it into place. The chosen model then feeds
`LlamaModel` on the inference worker thread.

### Local-Replacement Path

Independent of the model path, the run loop offers **local replacements** in the
observe path: `replacement_offer` tries, in order, an emoji `:shortcode`, a
typo fix (`autocorrect`), a US→UK normalization (`localize`), and a thesaurus
synonym, each gated by its toggle and reapplying the typed case via `textcase`.
These need no model and add no latency. They pass through the same
`suggestion_gates_pass` per-app + per-domain gate as model completions; the
domain is the focused browser page's host, read from the AX URL attribute when
a browser is frontmost (`None` otherwise).

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
- `p8_carbon_hotkey`: transient Carbon `RegisterEventHotKey` accept proof.

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
