# Design — Sub-project A: Engine + macOS MVP

**Date:** 2026-06-03
**Status:** Approved, then revised with online validation + Cotypist reverse-engineering
**Scope:** First sub-project of an **open-source, multi-platform** predictive-writing product that re-implements all Cotypist functionality except payment/licensing (see Project Scope note below). Covers **only** the OS-agnostic engine + the macOS adapter + the app shell. **Windows/Linux adapters are committed sub-projects (B/C)** behind the interfaces defined here, not optional add-ons.

**Revision note (v2):** This document was re-validated against current crates/docs (Feb–Jun 2026) and against the **installed** Cotypist binary (`/Applications/Cotypist.app`, v2026.1 **build 73**) via static analysis + its live `UserDefaults`. (The current official DMG is **2026.1.1 build 74** — see §15 D12; treat all evidence here as installed-b73, not the latest shipping build.) Material corrections are marked **[CORR]** and the evidence is in §12–§13.

**2026-06-05 A1b reconciliation:** Native inline prediction suppression remains a product concern, but is deferred out of A1b for cross-app text fields. Current AppKit bindings expose automatic text-completion controls for owned `NSTextView`/`NSTextField` instances, while this app reads and writes other applications through Accessibility plus native overlay rendering. The active A1b source of truth is `docs/superpowers/plans/2026-06-04-a1b-macos-adapter-contract.md`.

**2026-06-07 decompile audit (corrections):** Full static re-decompile (`codesign`/`otool`/`strings`) + live `UserDefaults` read produced fixes marked **[CORR 06-07]**: (1) `RepliesSDK.framework` is feedback/screen-capture tooling, **not** the completion engine — completion = `CompletionManagerActor` + llama.cpp directly (§1, §13); (2) default accept is **Tab = next word**, `~`/key-above-Tab = full — previously under-specified (§1, §4, §13); (3) **[SUPERSEDED 2026-06-08 — see §15 D2]** personalization strength was read as a 3-stop control from binary flag names (`Gentle/Balanced/Strong`); the **`/pricing` page is authoritative and shows a 6-stop slider** (Off↔Max, tier caps 2/4/6); the `/help/personalization` page still uses simplified "Off to Strong" wording, so only `/pricing` confirms six-stop. Gentle/Balanced/Strong are tier *thresholds* (`Pro.PersonalizationBeyondBalanced` / `Plus.PersonalizationBeyondGentle`), not the stop count (§6, §15 D2); (4) `maxCompletionLength=4` is **live-confirmed**; **[CORR 06-08 — see §15 D3]** `selectedModel=gemma-4-E4B-UD-Q5_K_XL` is **NOT a static fact**: build-73 binary contains **no `gemma-4-*` download id** (static catalog = `gemma-3-*` + Qwen3 + Llama-3.2 + Phi-4-mini); "Gemma 4" is server-driven catalog (`google.protobuf.FeatureSet` `/features`) + marketing copy. Does not affect our Qwen2.5-0.5B choice; (5) Cotypist's ~100–200 ms latency is secondary-sourced and "Qwen 2.5 1.5B" is stale pre-launch reporting (§5); (6) **[CORR 06-08 — see §15 D1/F1]** full-accept uses Carbon/`MASShortcut` (no Input Monitoring). **Re-decompile found NO `CGEventTapCreate` anywhere in the bundle** — Cotypist uses AX (`AXUIElementSetAttributeValue`/`AXObserver`) + CGEvent synthesis (`CGEventPost`/`CGEventPostToPid`) + Carbon hotkeys; the earlier "only the Tab swallow needs a CGEventTap" was our inference, not Cotypist's mechanism. **[CORR 06-09]** Compme production accept keys now use transient Carbon hotkeys too (§15 F1).

**2026-06-07 architecture pivot (Tauri → native):** The app shell was planned as a Tauri v2 tray app, but the shipped implementation is a **native Rust binary** (`crates/app`) with an AppKit `NSStatusItem` tray built via `objc2` (`crates/platform_macos/src/tray.rs`) — **there is no Tauri dependency in the workspace**. Config is a dotenv-style `config.env` file (not a hidden Tauri webview). Mentions of "Tauri v2 tray app", `apps/app`, `WebviewWindowBuilder`, `run_on_main_thread`, and the Tauri `updater` plugin below are **superseded**: read them as the native AppKit run loop, `crates/app`, the objc2 tray/menu, and an A3 updater decision still open (no Tauri). The cross-platform `PlatformAdapter` seam is unchanged.

**2026-06-07 Cotypist parity audit:** P0/P1 are MVP layers, not Cotypist parity; individual P0/P1 docs record which checks are implemented, live-validated, or still pending after the accept-key flip. The installed Cotypist app and current website add requirements beyond the active MVP: optional Screen Recording / screen-aware context, encrypted local personalization, per-app and per-domain controls, Google Docs onboarding, browser compatibility workarounds, terminal AI-agent prompt support, mirror-window fallback, configurable shortcut matrix, emoji/typo features, anonymous telemetry controls, and a signed/updating app distribution story. These are now tracked explicitly in §8, §9, §12, and §13; do not mark the product "Cotypist-aligned" until those A2/A3+ items have implementation and validation evidence.

**2026-06-08 audit update:** Current public Cotypist pages add sharper parity requirements than the older static decompile notes: tier-capped feature behavior, a **confirmed six-stop** personalization slider (Off↔Max, tier caps 2/4/6 — §15 D2), and more granular app compatibility tiers. Treat website claims and live installed preferences as separate evidence streams; user-customized local preferences do not prove factory defaults. **Consolidated audit status (resolved + still-open findings F1/F2/G3 + new parallel-agent findings D1–D5 + live gates) is tracked in §15.**

**2026-06-09 PROJECT SCOPE (authoritative — supersedes any "tier/feature-gate decision" left open below):** Compme is an **open-source, multi-platform** re-implementation of **all Cotypist functionality EXCEPT payment, licensing, subscription tiers, and multi-device seats** (the only things deliberately not cloned). Consequences, locked:
- **No pricing/feature gates.** Every capability is available to every user. The personalization slider has **full reach for all** (no Free=2/Plus=4/Pro=6 caps); the **entire model catalog** is available (the only gating is *hardware capability* — don't offer a model the machine can't run, §15 D14 — which is not a pricing gate); **unlimited completions**; global **and** per-app instructions; clipboard awareness; Labs/experimental features — all on by default or freely toggleable. `featureUnlimitedCompletions`/`featureMidSize|LargeModels`/`featureMultiDeviceSeats`/subscription flags are **dropped**, not gated.
- **Multi-platform is a committed deliverable, not "maybe later".** macOS (Sub-project A) ships first; **Windows (B, UIA) and Linux (C, AT-SPI)** are in-scope goals built behind the `PlatformAdapter` contract (§3, §14). Every engine/feature decision must stay portable — no macOS-only assumption leaks into `engine_core`/`platform`.
- **Open-source posture.** **Licensed Apache-2.0** (chosen 2026-06-09 — patent grant; `LICENSE` file + `workspace.package.license` in `Cargo.toml`). **No proprietary telemetry** (Sentry/BigQuery are Cotypist's, not ours): default to **no network analytics**; any future diagnostics are local-only and opt-in. OSS-only dependency stack (e.g. `rusqlite`, not GRDB; `llama-cpp-2`; `objc2`). **Model-license passthrough**: the download flow must surface each model's license (Gemma terms, Qwen Apache-2.0, Llama community license) and never bundle restricted weights — download with acceptance (ties to §15 D14, the `GemmaTermsNotice` surprise).

**2026-06-08 parallel-agent re-audit (decompile + website + plan/code):** Three new ground-truth checks landed. (a) **No CGEventTap in Cotypist** — confirms F1 is a real architectural delta, not a guess (D1). (b) **Six-stop slider** confirmed on `cotypist.app/pricing` (authoritative), not 3-stop; `/help/personalization` uses simplified "Off to Strong" wording so it does not independently confirm six-stop (D2). (c) **`gemma-4-E4B` is not a static binary fact** in build 73; catalog is server-driven (D3). (d) Plan model catalog is **missing Phi-4-mini** and the site dropped Llama (D4). (e) The **trial-length mismatch is narrowed, not withdrawn** — website pages (`/pricing` + landing) are consistent at **30-day Pro trial**, but the official Sparkle **appcast says "free 7-day trial"**, so website-vs-appcast is a real first-party inconsistency (D5; cosmetic, Cotypist's, not ours). Details in §15.

---

## 1. Context & motivation

Local, privacy-first predictive writing: read editable text around the caret in other apps, predict next word/phrase with a **local** model, show inline grey ghost text, accept incrementally with a configurable shortcut.

Validated against the installed Cotypist binary (v2026.1 build 73; current shipping DMG is 2026.1.1 b74 — §15 D12):

| Aspect | Cotypist (binary + libs + live prefs) | Decision for this product |
|---|---|---|
| Inference | `libllama` + `libggml-metal` (ggml 0.12.0), llama.cpp, Metal | llama.cpp via `llama-cpp-2` (feature `metal`), Metal backend |
| Models | **Selectable catalog**, Instruct: Gemma 3/4, Qwen 3 (e.g. `gemma-4-E4B-UD-Q5_K_XL`); downloaded first-run | Selectable catalog; start small (see §5); download first-run |
| Storage | `GRDB.framework` (SQLite); training data "encrypted, stored locally"; local DB appears passphrase-protected and key-managed | `rusqlite` + `bundled` (FTS5 included) plus A2 encryption/key-management plan before claiming personalization parity |
| App shape | Menu-bar agent, `LSUIElement=true`, status item, no dock | **[CORR 06-07]** Native Rust binary (`crates/app`), AppKit `NSStatusItem` tray via `objc2`, `ActivationPolicy::Accessory`; config via `config.env` (no Tauri, no webview) |
| Engine | **[CORR 06-07]** Swift `CompletionManagerActor` + llama.cpp directly (prompt build, sampling). `RepliesSDK.framework` is a screen-capture/feedback SDK, **not** the completion engine | Our `engine_core` + `ranker` + `model_client` |
| Personalization | **Prompt-based**: `userPrompt` custom instructions + strength control + sender name/email + optional training-data collector. Static binary evidence shows Gentle/Balanced/Strong symbols; current public pricing describes tier-capped slider stops. | Same mechanism: prompt-based primary; **full-reach strength + full model catalog for all users — no tier caps** (Project Scope); only hardware gates which models are offered |
| Context source | AX **+ pasteboard fallback** + previous-input / cross-app history | AX primary; pasteboard + previous-input augmentation (latter deferred) |
| Screen-aware context | Optional Screen Recording + on-device OCR/context extraction; still works without it | A2+ optional permission; local-only ScreenCaptureKit/Vision-style context source with clear opt-in/off path |
| Models CDN | Self-hosted `models.cotypist.app` (zstd), sourced from HF | HF direct or self-host (TBD) |
| Native inline prediction | Disabled while active (`InlinePredictionDisableController`) | Same product goal; cross-app suppression deferred out of A1b unless an owned-control integration is added |
| Accept | **[CORR 06-07] Configurable, two-tier**: factory default evidence points to **Tab = next word** (partial), **`~`/key-above-Tab = full**; `maxCompletionLength` in words (live-confirmed default 4); Tab disable per-app. Installed user profiles may differ because shortcuts are configurable. | 2 configurable shortcuts, word-capped — **default Tab→word to match Cotypist muscle memory**, but do not hardcode factory defaults as the only valid layout (§4) |
| Update | Sparkle (`SUFeedURL` cotypist.app/updates) | **[CORR 06-08]** A3 decision, open. Tauri's updater is off the table (no Tauri); **Sparkle is now the leading candidate** — it is the standard Developer-ID/non-MAS macOS updater and is what Cotypist itself ships (§13). The earlier "drop Sparkle for Tauri" rationale is void. |
| Analytics | Sentry + Google BigQuery, opt-out | **None by default (OSS)** — no proprietary telemetry; any diagnostics local-only + opt-in |
| Entitlements | `com.apple.security.automation.apple-events`; not sandboxed | Same; hardened runtime + notarize |
| Language | Swift native | **Rust** (chosen for cross-platform reuse) |

### Non-goals (this spec)
- Windows (UI Automation) / Linux (AT-SPI) adapters — behind `PlatformAdapter`.
- Swappable cloud providers (Ollama/OpenAI) — behind `LocalModel` trait.
- Browser extension / IDE plugins / remote compat registry.
- On-device fine-tuning (personalization is prompt + optional retrieval, never weight training).
- Full Cotypist feature parity in P0/P1. P0/P1 prove the core local completion loop; parity features land in A2/A3+ only when they have their own acceptance gates.

---

## 2. Architecture

Single process, **native Rust** (`crates/app`). **[CORR 06-07]** The shell is not Tauri: an AppKit `NSStatusItem` tray/menu (objc2) provides lifecycle + enable/disable; there is no settings webview (config is `config.env`). **Three run-loop contexts** (validated, §12):

- **Main thread / AppKit run loop** — the process's own AppKit loop; all NSPanel/overlay calls run on the main thread.
- **AX worker run loop** — owns AX observer resources and transient Carbon accept-hotkey resources; callbacks answer from **pre-computed state** and never perform synchronous AX reads.
- **AX/inference worker** — background thread/queue; AX IPC (with short messaging timeout) + llama.cpp decode.

```
┌─ Native Rust binary (one process, ActivationPolicy::Accessory) ────────┐
│  AppKit NSStatusItem tray/menu (objc2) · config.env · lifecycle        │
│                                                                         │
│  ┌── engine_core ────────┐   suggestion state machine, debounce,        │
│  │ generation tokens,    │   accept logic (full/partial), app policy     │
│  │ invalidation, cancel  │                                               │
│  └───────┬───────────────┘                                              │
│   ┌──────┴────────┬────────────┬───────────────┬──────────────┐         │
│  context     ranker     model_client    (personalization/prefs: A2/A3)   │
│  (AX read +    (score,     (LocalModel:    (custom-instr     (UserDefaults│
│   pasteboard    trim,       llama-cpp-2,    prompt, strength,  -equivalent,│
│   fallback,     boundary)   Metal, warm,    sender identity,   per-app    │
│   caret rect)               prefix-cache)   opt. memory)       overrides) │
│                                                                         │
│  ┌── platform (trait PlatformAdapter) ───────────────────┐              │
│  │  platform_macos: AX (accessibility-sys/objc2),         │              │
│  │  Carbon hotkeys, NSPanel overlay, NSWorkspace front-app│              │
│  └─────────────────────────────────────────────────────── ┘             │
└─────────────────────────────────────────────────────────────────────────┘
```

### Current Workspace And Planned Crate Map

Current root workspace crates:

```
crates/engine_core      # state machine, generation tokens, invalidation, cancel, accept logic, policy (renamed from `core`)
crates/context          # TextContext, Selection, Caret + AX+pasteboard capture model
crates/ranker           # candidate trim/boundary/repetition/score
crates/model_client     # LocalModel trait + llama.cpp impl (warm-up, prefix cache, N-sample)
crates/platform         # PlatformAdapter trait + shared types (cross-platform contract)
crates/platform_macos   # AX read, Carbon accept hotkeys, NSPanel overlay, front-app, NSStatusItem tray (objc2)
crates/engine           # wires SuggestionMachine → adapter + overlay + accept subscription
crates/app              # [CORR 06-07] native compme binary: run loop, inference thread, config.env, tray
tools/spike             # throwaway A0 PoC (excluded from workspace; retained under tools/spike)
```

Planned A2/A3 crates or modules, not current workspace members:

- `crates/personalization`: custom-instructions prompt builder, sender identity, optional encrypted local memory.
- `crates/prefs`: settings store, per-app/per-domain overrides, and migration from MVP `config.env`.

**Crate strategy** (verdicts in `2026-06-03-prior-art-review.md` §3): build the AX/tap/inject layer natively via `objc2` + `objc2-app-kit` + `accessibility-sys`/`axuielement` + `core-graphics`; inference via `llama-cpp-2` (C-API surface, `metal`). **Do NOT depend on `rdev`/`rdevin` for the capture path** (stale / grab-disabled on Linux) — KeyType, Cotabby, and Espanso all hand-rolled native capture. `enigo` only as an inject shortcut later.

---

## 3. The cross-platform contract **[CORR — expanded after Win/Linux validation]**

Capability-first so `engine_core` never special-cases apps; capabilities drive UX mode. This is the **validated** shape (see `2026-06-03-cross-platform-review.md` §4): Windows and Linux independently forced strategy enums + extra flags. macOS fills the rich values now so B/C slot in without reshaping the contract.

```rust
pub trait PlatformAdapter: Send + Sync {
    fn environment(&self) -> Environment;          // OS + display_server + compositor (Linux); macOS = {macOS, n/a}
    fn subscribe_focus(&self, cb: FocusCallback);  // focus events (cheap)
    fn subscribe_caret(&self, cb: CaretCallback);  // caret events — SEPARATE (expensive on Win UIA / Linux D-Bus)
    fn front_app(&self) -> Option<AppId>;          // bundle id; often None on Wayland
    fn capabilities(&self, f: &FieldHandle) -> Capabilities;
    fn read_context(&self, f: &FieldHandle) -> Result<TextContext>; // left/right/selection; pasteboard fallback
    fn caret_rect(&self, f: &FieldHandle) -> Option<ScreenRect>;    // collapsed-range workaround required (§12)
    fn insert(&self, f: &FieldHandle, text: &str, s: InsertStrategy) -> Result<Inserted>;
}

pub struct Capabilities {
    pub readable_text: bool,
    pub readable_caret: bool,
    pub writable: bool,
    pub secure: bool,                       // AXSecureTextField subrole OR Secure Input mode → HARD block (§12)
    pub multiline: bool,
    pub toolkit: Toolkit,                   // generalizes is_electron: Cocoa/Win32/WPF/Qt/Gtk*/Electron/Java/Vte/Unknown
    pub insert_strategy: InsertStrategy,    // macOS: AxSet | SyntheticKeys | Clipboard  (Linux adds EditableTextApi/ImeCommit)
    pub accept_intercept: KeyInterceptMode, // macOS: CarbonHotkey  (Win: LowLevelHook; X11: XGrabKey; Wayland: HotkeyOnly/ImeOwnsKey/None)
    pub overlay_at_caret: OverlayPlacement, // macOS: NativePanel  (≠ readable_caret — GNOME/Wayland can read caret but not place)
    pub coords_global_screen: bool,         // can caret rect be used for absolute positioning?
}
```

Rationale per field is in the review (§4). The macOS adapter implements `accept_intercept = CarbonHotkey`, `overlay_at_caret = NativePanel`, `insert_strategy ∈ {AxSet, SyntheticKeys, Clipboard}` (probe writable → fall back), `toolkit` detected via bundle id / framework. `subscribe_caret` is split from focus because on Windows/Linux caret events are the expensive ones — macOS keeps the split for contract uniformity even though AX is cheaper.

### UX mode derivation (in `platform::ux_mode`, consumed by `engine_core`)
| readable_text | readable_caret | writable | secure | → Mode |
|---|---|---|---|---|
| ✓ | ✓ | ✓ | ✗ | **Inline ghost text** |
| ✓ | ✗ | ✓ | ✗ | **Near-caret popup** (use front-app frame / cursor) |
| ✓ | – | ✓ | ✗ | **Hotkey completion** (continuous unsafe) |
| – | – | – | – | **Unsupported** (tray status + diagnostic) |
| any | any | any | ✓ | **Blocked** (always; also when `IsSecureEventInputEnabled`) |

---

## 4. Event flow & suggestion lifecycle **[CORR — biggest gap in v1]**

A suggestion is a contract over a specific context snapshot. Define it precisely or Tab inserts stale text.

1. Focus/caret change → adapter callback. Read `front_app`, apply per-app override (excluded? strength? enabled?).
2. **Debounce** ~120–300 ms of keystroke quiescence (**P1 ships a 120 ms default**, configurable via `COMPME_DEBOUNCE_MS`, clamp 0..=5000); gate: not mid-word unless configured, not on backspace, min context length.
3. Snapshot context → compute **generation token** = hash of `{element id, text length, caret offset, left-context tail}`.
4. `model_client` runs inference (warm model, cached prefix). **Cancellation token** checked between decode steps; superseded request → drop-all-but-latest.
5. On return, **discard unless current generation token still matches** (stale-race guard).
6. `ranker` trims to word boundary, caps at `maxCompletionLength` words, applies repetition/sensitive penalties.
7. Overlay renders top candidate at `caret_rect` (Retina/multi-monitor coordinate conversion, §12) or popup fallback. Render over a **backdrop** (solid/blurred/glass, configurable) for legibility on arbitrary app backgrounds. Avoid double ghost text where the target integration allows native inline prediction suppression; for cross-app Accessibility fields this is deferred out of A1b. Multi-candidate shows as an inline list (row + badge views).
8. **Accept**: full-completion shortcut inserts all; partial shortcut inserts next word (+ trailing space if available). **[CORR 06-07] Match Cotypist defaults: Tab = accept next word (partial), `~`/key-above-Tab = accept full** (`acceptNextWordOnly_includeTrailingSpaceIfAvailable=1`, live-confirmed); Tab disengageable per-app where Tab has native function. Down cycles candidates; Esc dismisses and suppresses completions in the current field until refocus/edit; Option+Tab passes a literal Tab because only bare Tab is registered as a Carbon hotkey. **[CORR 06-09 — D11 fixed in code]** Deterministic unit coverage exists for Esc, Option+Tab, and cycle semantics; Carbon consume remains a manual physical-key live gate because synthetic key posts do not fire `RegisterEventHotKey`. Per-app/global enable toggles stay A3 (§8).
9. **Invalidation** (any → drop suggestion): non-accept keystroke, caret/selection move, focus/app change, mouse click, text no longer matches prefix.
10. `personalization`/stats record outcome locally (redacted).

**Implementation reality (from prior-art code — `2026-06-03-prior-art-review.md` §2):**
- **Carbon accept hotkeys, not a consuming CGEventTap.** Cotypist parity avoids the Input Monitoring prompt: register bare Tab, grave, Esc, and Down via Carbon only while a suggestion is armed, and unregister immediately on hide/disarm. The earlier two-tap CGEventTap design remains useful historical prior art for event-tap probes, but it is no longer the production accept-key mechanism.
- **Tag synthetic events** (`CGEventSource.userData`) and skip them in the tap — else your own insert re-enters the tap → dismiss/double-insert.
- **`AXUIElementSetMessagingTimeout(systemWide, 0.05)`** — default 6 s; a wedged app beachballs typing. Most important AX reliability knob.
- **All AX calls on one dedicated background thread** (never main — NSOpenPanel deadlock; AXSwift was abandoned over thread bugs).
- **Resolve field owner from the AX element's pid**, not `NSWorkspace.frontmostApplication` (Raycast/Spotlight/Alfred keep the previous app as frontmost).
- **AX value-changed lags keystrokes** → front-run dismissal from the key tap (`hasPrefix` check); redraw shrinking remainder eagerly on accept.
- **Suspend triggering during non-ASCII IME composition.** Wake lazy Chromium/Electron a11y via `AXManualAccessibility` on the browser-process element.

---

## 5. Inference **[CORR]**

- Backend: llama.cpp via `llama-cpp-2` (latest v0.1.146; **`metal` is NOT default** — must enable; add `sampler`). Vendored build via git submodule + cmake; needs clang/cmake; pin exact versions. `mistral.rs` (pure Rust, Metal) is the plan-B if the C++ build hurts distribution.
- **Warm-up mandatory**: first decode triggers Metal shader compile (seconds). Pre-load model + dummy decode at launch; show "loading" tray state.
- **Prefill dominates TTFT**: keep left-context short (few hundred tokens); reuse KV/prefix cache across keystrokes. Don't re-prefill long context per keystroke.
- Live mode: `n_predict` 8–24 tokens, capped to `maxCompletionLength` words; aggressive stop sequences (newline/sentence boundary) — **boundary/stop handling is the hidden quality lever**.
- Candidates (2–5): **N independent samples** (temp/seed variation; llama.cpp dropped beam search). Decode shared prompt once, branch N sequences → ~N× the *generation* cost, not N× whole request.
- Latency: 0.5–1.5B Q4 on M-series ≈ 30–80 tok/s. Sub-150 ms first suggestion feasible **only** warm + short prompt. **[CORR 06-07]** Cotypist's ~100–200 ms target is **secondary-sourced** (press reporting, not first-party — site only claims "real-time"); the "Qwen 2.5 1.5B" figure is stale pre-launch reporting. Cotypist trades latency budget for a much larger model than our MVP's 0.5B. **(Model-id caveat 06-08 [CORR — see §15 D3]:** the live `selectedModel` pref reportedly read `gemma-4-E4B-UD-Q5_K_XL` and the pricing page lists a "Gemma 4 E-series" catalog, **but a full static string dump of build 73 contains no `gemma-4-*` download id at all** — the on-disk downloadable catalog is `gemma-3-*` (1b/4b/270m) + Qwen3 + Llama-3.2 + Phi-4-mini. "Gemma 4" is therefore a **server-driven catalog** (`google.protobuf.FeatureSet` `/features`) + marketing label, not a static binary fact. The exact shipped GGUF id is unsettled; it does not affect our own model choice.)
- **Model: selectable tiered catalog** (mirrors Cotypist). Cotypist self-hosts GGUFs at `models.cotypist.app` (zstd-compressed), sourced from HF (unsloth `UD-Q*_K_XL` dynamic quants, `mradermacher *-i1-GGUF`). Static catalog observed in build 73 (download ids): Gemma 3 1b/4b-it-UD-Q4, Gemma 3 270m, Llama-3.2-1B/3B-Instruct-UD, Qwen3-0.6B/1.7B/4B/8B/30B-A3B-Base-i1, **Phi-4-mini-instruct** (`unsloth/Phi-4-mini-instruct-GGUF`). **[CORR 06-08]** No `gemma-4-*` download id is in the binary; "Gemma 4 E2B/E4B/26B-A4B" appears only in the **website catalog + marketing** (server-driven, §15 D3/D4). The site also drops Llama from its advertised list — site catalog ≠ static binary catalog. We can either self-host similarly or pull from HF directly. Catalog organized by *size class* (not pricing): "always fast" **Qwen3-0.6B / Qwen2.5-0.5B / gemma-3-1b**, Q4_K_M (~350–490 MB); "quality" ~1.5–1.7B; "large" classes — **all available to every user**, gated only by **hardware capability** (RAM/compute), never by a paid tier (Project Scope, §15 D14/D15).
  - **Base vs Instruct:** Cotypist ships **both** (`-Base-i1` and `-it`/Instruct). Base = cleaner continuation; Instruct works with hard constraints (word cap, custom-instruction prompt, stop sequences). Decision: **benchmark both in A0**; offer both in catalog; default per-model.
  - **Mid-line completion** (`featureMidLineCompletion`): insert within a line, not only at end. Achievable with left-context + stop-at-existing-text without full FIM; revisit FIM only for code fields.
  - **FIM / right-context: dropped for v1** — no good small *prose* FIM checkpoint; code-FIM models hurt prose. Left-context continuation only. Revisit (Qwen2.5-Coder FIM) only if targeting code fields.
- `LocalModel` stays a trait so cloud providers are a later additive spec.

**Inference gotchas (from KeyType/Cotabby ADRs — `2026-06-03-prior-art-review.md` §2):**
- **KV-cache reuse unsafe on hybrid/recurrent models** (Qwen3.5 SSM/GatedDeltaNet layers): `seq_cp` aborts, `seq_rm` rollback fails, `llama_model_is_recurrent` returns false despite recurrent buffers. **Only pure-append reuse is safe**; any divergence → `llama_memory_clear` + full re-decode. Snapshot/restore via `llama_state_seq_get/set_data` for branches. (Prefer non-recurrent small models to keep prefix-cache simple.)
- **Token healing for mid-word completions** (worst case): back up to last whitespace, force typed bytes as a required prefix via byte-mask **over the full vocab** (not post-top-k), strip the re-emitted stem.
- **Suffix-overlap guard for mid-line** — small models regurgitate text after caret; compare on alphanumerics, truncate at overlap.
- **Trim trailing whitespace from the prefix** before prompting (the just-typed space makes small models wander/double-space).
- **ggml-Metal aborts on exit** unless model/context freed via explicit `shutdown()` before teardown (guard double-free).
- Serialize all llama calls behind an actor/mutex (`llama_context` not thread-safe). Optional: disk-cached per-model constrained-decode token profile.

---

## 6. Personalization **[CORR — redesigned to match Cotypist]**

Prompt-based, not ML. Simpler, ships, and is what Cotypist actually does.

- **Primary: custom-instructions prompt.** User-editable free-text style profile (`userPrompt`: name, role, languages, tone rules) templated into the completion prompt. **Global + per-app** instructions (`featureCustomInstructionsGlobal` / `PerApp`) — per-app supplements global. Auto-seed a starter from the Mac on first run; "a few hundred words" guidance.
- **Strength control [CORR 06-08/06-09 — resolved to 6-stop, `/pricing` authoritative]**: `cotypist.app/pricing` is the source of record — the slider has **6 stops**, only endpoints labeled (**Off** ↔ **Max**), with **tier caps: Free→tick 2, Plus→tick 4 (default), Pro→tick 6/Max**. (Note: the `/help/personalization` page still uses simplified "Off to Strong" wording — do not treat it as confirming six-stop; §15 D2.) The static binary symbols `featurePersonalization{Gentle,Balanced,Strong}` + `Pro.PersonalizationBeyondBalanced` / `Plus.PersonalizationBeyondGentle` are Cotypist's tier-gating *thresholds*, not the stop count — they reconcile with the 6-stop slider. **A2 target (scope-locked 2026-06-09): a 6-stop slider with FULL reach for every user — no tier caps** (Cotypist's 2/4/6 caps are paywall artifacts we don't clone). Controls how hard instructions + memory steer.
- **Sender identity**: name + email (`io_replies_sender_*`) for signature/contact awareness.
- **Custom model override** (`featureCustomModelOverride`): user may point at their own GGUF. Behind `LocalModel`; defer UI to A3.
- **Context augmentation (deferred to A2/later)**: previous-input context — recent text the user typed (same app, and cross-app `featureCrossAppPreviousInputs`) — fed as extra context. Privacy-sensitive: opt-in, redacted, bounded retention.
- **Optional local memory (deferred within A2)**: encrypted `rusqlite`/SQLite-compatible store with FTS5-style retrieval of accepted completions for retrieval-augmented prompting + ranker similarity score. Opt-in (`TrainingDataCollector` — encrypted, local, record count + "disable and erase"), inspectable. Encryption key must live in Keychain or an equivalent macOS-protected key store; deletion UX must support all data plus per-app/per-domain data. Plain unencrypted `rusqlite` is not Cotypist parity.
- **No fine-tuning, ever.** Memory/inputs feed the prompt/ranker, never weights.
- **Redaction before any persistence**: emails, card-like numbers (Luhn), tokens/secrets (regex; `pii-vault`/`redact`). Diagnostics text-redacted by default.

---

## 7. Privacy & safety (first-class)

- Never read/store secure fields: block on `AXSecureTextField` subrole **and** `IsSecureEventInputEnabled` (§12).
- All inference local by default (only backend this spec). No raw-text logging by default.
- Optional Screen Recording / screen-aware context must be local-only, off-revocable, and non-blocking: the app continues with field-only context when denied.
- Visible **pause/snooze** ("disable for N minutes", as Cotypist) + per-app exclude list (default-exclude Finder-like) + per-window incognito.
- Custom-instructions & memory are user-visible/editable; clear retention + "forget learned data".
- Telemetry decision is explicit: P0/P1 ship no network telemetry. If A3 adds crash/usage telemetry, the plan must specify provider, region, payload schema, default state, opt-out/opt-in semantics, restart requirements, and a hard rule that typed text, clipboard text, OCR text, and prompts are never included.

**Distribution & permission lifecycle (prior-art §2 — category's #1 support burden):**
- **App Sandbox OFF**; hardened runtime needs `com.apple.security.cs.disable-library-validation` to load the dynamic llama framework → **Mac App Store impossible**. Ship Developer-ID DMG + a native updater (no Tauri; A3 decision). Entitlement `com.apple.security.automation.apple-events`.
- A3 updater requirements: generate updater artifacts, generate/store signing key material safely, define the update endpoint/manifest format, choose static vs dynamic manifests, verify signature failure behavior, and validate update rollback/failure recovery before release.
- **Stable signing identity** — TCC keys on cert+bundle-id; a cert change under the same bundle id causes an infinite "grant Accessibility" loop. Provide a `tccutil reset` recovery path + re-grant detection after OS updates.
- Detect when **Secure Input** is stuck (background password managers) — it kills all injection globally; surface it in diagnostics.
- Onboard **Accessibility** for AX read/write and Carbon accept hotkeys; add optional Screen Recording onboarding in A2+ for screen-aware context. Re-check after grant (may need relaunch). **[CORR 06-09]** The production accept path no longer uses a consuming CGEventTap, so Input Monitoring is not part of the accept-key onboarding.

---

## 8. Settings / config surface (mirrors Cotypist's panes — §13)

| Pane | Options |
|---|---|
| General | Completions enabled by default · `maxCompletionLength` (words, `featureConfigurableCompletionLength`) · typo/suggested-fix indicator separate from full autocorrect (`featureFullAutocorrect`) because public copy is inconsistent · mid-line completion (`featureMidLineCompletion`) · menu-bar word-count |
| Personalization | Global custom instructions · per-app and per-domain custom instructions · full-reach strength control (no tier caps) · sender name/email · training-data collection (enable / disable+erase / record count) · encrypted local database status |
| Model | Selectable catalog (tiered) · download manager · custom model override (own GGUF) |
| Shortcuts | Accept next word · accept full completion · dismiss · force-activate current field · temporary current-app toggle · global toggle · per-app Tab disable; all configurable where feasible. Factory defaults are separate from user-customized installed preferences. |
| App Overrides | Per-app enable/disable/exclude · per-app strength · per-app: **Tab-key behavior, Smart Quotes, Text Mirroring, Size Thresholds, Display/backdrop+font** · per-app instructions. Domain/website overrides are required for browser personalization/data controls; app-only knobs remain app-only. |
| Compatibility | Google Docs Accessibility setup · Arc/Dia Text Metrics guidance · Firefox/Zen mirror-window fallback · Terminal/iTerm AI-agent prompt mode · Slack partial handling · code-editor sidebar/chat-only activation for VS Code/Cursor/Windsurf · TheBrain support check · explicit unsupported list for Pages/Scrivener/Thunderbird/OneNote/BBEdit/Sublime/Ghostty/cmux/Warp-style cases until proven |
| Context | Pasteboard-context toggle · previous-input context · cross-app previous inputs · optional Screen Recording/OCR surrounding-context toggle |
| Display | Backdrop style (solid / blurred / glass) · suggestion color/symbol · font style (`featurePerAppFontStyleOverrides`) · mirror-window fallback for fields without usable inline metrics |
| Permissions | Accessibility status · optional Screen Recording status · pasteboard permission |
| Emoji | Emoji completion · skin tone · gender · vanilla-variant toggle (`includeVanillaVariants` — modeled later; not in `EmojiPrefs` yet) |
| Labs | Experimental flags (`featureCotypistLabsAccess`); thesaurus auto/selection mode (also has a first-class enable toggle, not Labs-only) · autocorrect/typo-fix |
| About / Update | Version · auto-update (native updater, A3 — no Tauri) |

Stored in a `prefs` crate keyed like Cotypist (`CompletionManager_*`, `ModelRepository_*`, `feature*`, per-app override list). Cotypist also supports **web-driven config** (`cotypist.app/setPreference`, `/launchCotypist/setOverride` deep links via URL scheme) for pushing compatibility fixes — optional later.

**Planned `COMPME_*` config keys for the new pure-feature toggles** (the crates exist + are tested; these keys are the wiring contract, not yet read by `app`): `COMPME_EMOJI` (enable) + `COMPME_EMOJI_SKIN_TONE` + `COMPME_EMOJI_GENDER` (`crates/emoji`); `COMPME_THESAURUS` (enable + auto/selection mode, `crates/thesaurus`); `COMPME_AUTOCORRECT` (typo-fix enable, `crates/autocorrect`); `COMPME_ACCEPT_WORD_KEY` + `COMPME_ACCEPT_FULL_KEY` (keycodes, `platform_macos::AcceptKeymap`). These join the ~28 keys already parsed in `app/run_loop.rs::Config::from_lookup`.

Tier semantics: Cotypist gates completion quotas, model catalog size, clipboard awareness, global/per-app instructions, full autocorrect, Labs, and device count by **paid tier**. **[DECISION LOCKED 2026-06-09 — see Project Scope at top]** Compme ships **none of these gates**: payment, licensing, subscription tiers, and multi-device seats are the *only* Cotypist functionality not cloned. Every feature above is available to every user, unconditionally. The single remaining gate is **hardware capability** (don't offer a model the device can't run — §15 D14), which is not a pricing gate. There is no "feature-gate decision" left to make.

---

## 9. Phasing (Sub-project A)

| Phase | Weeks | Deliverable | Exit criterion |
|---|---|---|---|
| **A0 spike** (throwaway) | 1–2 | (1) caret **ladder** read in a native app (TextEdit) AND a Chromium app (AXTextMarker path); (2) **two-tap CGEventTap** that swallows a test key without stalling other apps, behind Input Monitoring; (3) NSPanel overlay (Retina-correct); (4) warm llama.cpp round-trip + latency table + KV-reuse rules for the chosen model; bench base-vs-instruct | All four work in real apps; two-tap proven stall-free; sub-150 ms warm latency confirmed or model retiered |
| **A1 core loop** | 3–4 | `PlatformAdapter` + macOS adapter + suggestion lifecycle (§4) + configurable accept + ghost overlay (backdrop + native-inline-prediction suppression only where supported) + **secure block (subrole + secure-input)** | Type in Notes/Mail → inline suggestion → accept; passwords & secure-input blocked; no stale inserts; no double ghost text **where native inline-prediction suppression applies (owned/supported fields); cross-app Accessibility-field suppression is deferred to A2+** (see §4 step 7, A1b) |
| **A2 parity features** | 3–4 | Prompt-based personalization (global/per-app/per-domain custom instructions + re-verified strength semantics + sender) + encrypted local memory/data controls + pasteboard context + optional Screen Recording/OCR context + multi-candidate/cycle + Google Docs setup + browser compatibility/mirror fallback + Terminal/iTerm AI-agent prompt activation + current compatibility matrix | Suggestions are steered by custom instructions; encrypted local data can be inspected/deleted; Google Docs and browser workarounds are guided; terminal suggestions only activate in intended prompt contexts; unsupported/partial app claims are explicit |
| **A3 settings + ship** | 2–3 | Native settings UI (all §8 panes; no Tauri) + per-app/domain overrides + **model catalog/download (incl. download-failure recovery, manual model placement, and hardware gating for large models — §15 D14)** + diagnostics + pause/snooze + **launch-at-login (`SMAppService`/login-item, default-off, toggleable — §15 D13)** + updater details (native, Sparkle-leading/TBD — no Tauri) + codesign/notarize (hardened runtime + entitlements) + **OSS license (Apache-2.0, `LICENSE` added 2026-06-09) + no-proprietary-telemetry (local-only/opt-in) + model-license passthrough** | Installable signed/notarized `.app`; configurable; self-diagnosing; Accessibility onboarding plus optional Screen Recording onboarding; updater artifacts validated; **no pricing/feature gates (all features open — Project Scope); Apache-2.0 LICENSE present; no network analytics by default; model downloads surface their license** |

~9–13 weeks solo to a shippable macOS app.

---

## 10. Risks (updated with validation)

| Risk | Sev | Mitigation |
|---|---|---|
| **Tab/accept interception** must consume bare Tab without stealing it outside active suggestions. **[CORR 06-09 — §15 F1]** Carbon `RegisterEventHotKey` is the production path, matching Cotypist's Accessibility-only mechanism. | High | Register Carbon hotkeys only while a suggestion is armed; unregister immediately on hide/disarm; keep manual live consume + Input-Monitoring-revoked confirmation as the GUI-bound residual. |
| Historical single-CGEventTap design can stall OTHER apps' input (real bug: Cotabby #328) | High | Production no longer uses a consuming CGEventTap for accept keys; keep the old two-tap spike only as historical evidence. |
| Carbon hotkey collision or OS registration failure | Med | Register only transiently while armed; surface `CannotComplete` with Carbon status; keep acceptance probes for bare Tab/grave/Esc/Down. |
| **Reading AX perturbs target apps** (Calendar/System Settings glitches) | Med | Non-invasive caret strategy for native single-line; full resolver only for web/multiline; text-eligibility gate |
| **Hybrid-model KV-cache corruption / ggml exit-abort** | Med | Pure-append reuse only or full re-decode; prefer non-recurrent small model; explicit `shutdown()` |
| **TCC re-grant loop on cert change; permission silent-stop after OS update** | Med | Stable signing cert; `tccutil reset` recovery UX; re-grant detection |
| `caret_rect` collapsed-range returns `kAXErrorNoValue` in most apps | High | "Bounds of adjacent char" workaround + element-frame fallback (designed-in) |
| Electron/Chromium apps expose poor AX tree | High | Detect Electron → keystroke/clipboard insert + pasteboard context + popup positioning |
| **Secure Input mode** blocks AX/taps in password fields | Med | Detect `IsSecureEventInputEnabled`; suppress entirely |
| llama.cpp vendored C++ build (clang/cmake, slow) | Med | Pin versions; prebuilt artifacts in CI; mistral.rs fallback evaluated in A0 |
| TCC permissions (Accessibility, optional Screen Recording), revocable, post-grant relaunch | Med | Onboarding sequences required vs optional permissions; runtime detect each; guide to correct Settings pane |
| AX synchronous IPC can block (6 s default timeout) | Med | Off-main worker; `AXUIElementSetMessagingTimeout` short; handle `kAXErrorCannotComplete` retry |
| Single process: settings-UI panic stalls predictions | Low | Prediction on dedicated thread; `catch_unwind` around UI |

---

## 11. Success metrics
First-suggestion perceived latency <100–150 ms (warm); **<500 ms p95 is the hard floor** — slower "feels laggy and reduces acceptance" (industry threshold). Insertion failure <1% in supported apps · <5% laggy sessions · clear tier for top ~20 macOS apps · local stats: shown/accepted/dismissed/superseded, latency, words (30-day, mirrors Cotypist stats).

**Acceptance is trust-compounding** (66k-interaction study: prior per-user acceptance dominates future acceptance) → **protect first-run**; conservative triggering (fire near word/sentence boundaries, not every keystroke) beats always-on. Narrow scope deliberately: main code editor panes stay disabled unless a later code-specific plan exists, but terminal and code-editor sidebar **AI prompt fields** are compatibility targets because Cotypist supports those natural-language workflows.

---

## 12. Online validation results (Feb–Jun 2026) — evidence

- **objc2 v0.6.4** (maintained) + **accessibility-sys/accessibility v0.2.0** (thin, 1 maintainer) provide AXUIElement FFI. Prefer `accessibility-sys` + own wrappers; Carbon accept hotkeys are hand-written FFI via the Carbon framework.
- **Caret rect = a 5-tier ladder** (confirmed by KeyType `AXCaretGeometryResolver`, prior-art §2), not one workaround: (1) `kAXBoundsForRangeParameterizedAttribute` zero-length range — *works in many native apps, try first*; reject empty/container-sized rects; (2) **web path** — Chromium/WebKit need `AXSelectedTextMarkerRange`→`AXBoundsForTextMarkerRange` (opaque markers, NOT NSRange); (3) previous-char `NSRange(loc-1,1)` → `maxX`; (4) `AXStaticText` child-run interpolation; (5) font-metric estimate. Plus **Retina pixel-vs-point**: validate against `AXFrame` anchor, divide by per-display `backingScaleFactor` if mismatched.
- **Focus events** = `AXObserver` + `kAXFocusedUIElementChangedNotification` (+ caret via `kAXSelectedTextChangedNotification`); deliver on a CFRunLoop thread.
- **Secure field** = **subrole** `AXSecureTextField` (role stays `AXTextField`); also honor `IsSecureEventInputEnabled`.
- **Accept-key interception (current design)** = **Carbon `RegisterEventHotKey`** registered only while a suggestion is armed; Tab accepts next word, grave accepts full, Esc dismisses+suppress, Down cycles. **[CORR 06-09 — §15 F1]** This avoids Input Monitoring and matches Cotypist's no-CGEventTap architecture.
- **Overlay** = `NSPanel` `.nonactivatingPanel`, `.floating`, `canJoinAllSpaces|fullScreenAuxiliary`, clear/`ignoresMouseEvents`; never `activate(ignoringOtherApps:)`. `tauri-nspanel` plugin exists.
- **AX IPC** synchronous, 6 s default timeout → off-main + lower timeout.
- **Tauri v2** (evaluated, **not adopted — see 2026-06-07 pivot note**): would have given `ActivationPolicy::Accessory`, `TrayIconBuilder`, hidden webview, official `updater`, and `tauri build` codesign/notarize. The shipped app instead sets `ActivationPolicy::Accessory` and the `NSStatusItem` tray directly via `objc2`/AppKit; the A3 updater + codesign/notarize approach is open (no Tauri tooling).
- **Cotypist 2026.1 website/app delta**: optional Screen Recording improves context through local screen text recognition; clipboard context is optional/off by default; personalization is opt-in and encrypted locally; anonymous crash/usage telemetry exists and is user-controllable. Compme must model each of those as explicit A2/A3 decisions, not inferred behavior.
- **Inference**: `llama-cpp-2` v0.1.146, `metal` feature, vendored cmake build; warm-up + prefix cache critical; N-sample (no beam search); 30–80 tok/s for 0.5–1.5B Q4.
- **Models**: Qwen2.5-0.5B/1.5B-Instruct GGUF Q4_K_M exist (~491 MB / ~1.12 GB); base cleaner for completion but Instruct works with constraints (Cotypist ships Instruct). FIM = code-only → drop for v1.
- **Storage**: `rusqlite` `bundled` includes FTS5 (no separate flag); `directories::ProjectDirs` for paths (`cache_dir()` for the model); regex+Luhn redaction (`pii-vault`/`redact`).

---

## 13. Cotypist reverse-engineering — how it operates

**Evidence provenance [CORR 06-08 — D12]:** all decompile/`strings`/live-prefs evidence below is from the **installed build, `2026.1` build 73**. The current official DMG payload is **`2026.1.1` build 74**; the appcast advertises `shortVersionString=2026.1.1` but `sparkle:version=73`, so trust the **DMG `Info.plist`**, not the appcast. A b74 re-decompile is a follow-up before any "current Cotypist" claim — design decisions here are unaffected.

**Binary**: arm64 (thin, Apple-Silicon-only) Swift + AppKit/SwiftUI, `LSUIElement=true`, min macOS 14, built vs macOS 26.4 SDK, Developer ID (Accelerated Thought GmbH, `MRLF43FW3G`), Hardened Runtime ON, entitlement `com.apple.security.automation.apple-events` only, not sandboxed. Also links `ServiceManagement` (`SMAppService` / `shouldLaunchAtLogin` — launch-at-login, §15 D13). Libs: `libllama`/`libggml*` (Metal), `GRDB` (SQLite), `Sparkle` (update, `SUFeedURL=cotypist.app/updates`), `Sentry`, `ScreenCaptureKit`, `Vision`, NaturalLanguage. **[CORR 06-07]** `RepliesSDK.framework` is a screen-capture/annotation/feedback SDK — **not** the completion engine (completion runs through `CompletionManagerActor` + llama.cpp directly).

**Operation (from class names + live prefs):**
- `CompletionAccessibilityMonitor` watches focus/text via AX; `TextFieldContextCapture` reads field context **with optional pasteboard augmentation**.
- `CompletionManagerActor` (Swift actor → serialized concurrency) builds a `CompletionRequest` (prompt = custom instructions + context), runs local inference, returns `CompletionResult`.
- `CompletionOverlayManager`/`CompletionBackdropManager` render ghost text; `CompletionInserter` inserts on accept.
- `ShortcutListener` + key monitor handle **configurable** accept-full / accept-partial / force-enable shortcuts. **[CORR 06-07] Observed defaults** (live prefs + binary strings "(Tab) key to complete", "Disable Completions with the Tab Key", "try the key above [Tab]"): **Tab = accept-next-word** (partial), **`~`/key-above-Tab = accept-full**; `acceptNextWordOnly_includeTrailingSpaceIfAvailable=1`. The full-accept shortcut is registered via **`MASShortcut`/Carbon** (`RegisterEventHotKey`, no Input Monitoring). **[CORR 06-08 — see §15 D1/F1]** A full re-decompile (main binary + every bundled framework) found **no `CGEventTapCreate`/`CGEventTapEnable` anywhere** — Cotypist does **not** use a CGEventTap. Its input layer is AX (`AXUIElementSetAttributeValue`/`AXUIElementPerformAction`/`AXObserverAddNotification`) for read/write plus CGEvent **synthesis** (`CGEventCreateKeyboardEvent`/`CGEventKeyboardSetUnicodeString`/`CGEventPost`/`CGEventPostToPid`) for injection, and Carbon hotkeys for shortcuts. The Tab swallow is therefore **not** tap-based in Cotypist (most likely a Carbon `RegisterEventHotKey` Tab registration, which can consume). Our MVP's consuming CGEventTap (which forces Input Monitoring) is a deliberate divergence we must revisit — see §15 F1.
- `ModelRepository` manages a **tiered selectable model catalog**; `DownloadAndRenameTask` downloads the chosen GGUF first-run. **[CORR 06-08]** The catalog is **server-driven** (protobuf `/features`, `fixed_features`/`overridable_features`); the build-73 static download ids are `gemma-3-*`/Qwen3/Llama-3.2/Phi-4-mini (no `gemma-4-*` id — §15 D3/D4). `maxCompletionLength` live-confirmed **4** words; our MVP default is **8** (`DEFAULT_MAX_WORDS`, configurable) — a deliberate divergence (§15 D9).
- Pause/snooze ("Completions disabled for N minutes"); per-app exclusion (`excludedApplications`, e.g. Finder); 30-day completion stats; emoji completion; "suggested fixes" (spelling/grammar via NSSpellChecker).
- Compatibility surface observed from the current site: Google Docs requires Accessibility mode; Arc/Dia need Text Metrics or an accessibility launch flag for inline suggestions; Firefox/Zen use mirror-window fallback; Terminal.app and iTerm activate for AI-agent prompts; Ghostty is publicly listed as not supported even though the binary has preparatory Ghostty customizer symbols.

**Config surface (live `UserDefaults` keys observed):**
`CompletionManager_{acceptFullCompletionShortcut, acceptPartialCompletionShortcut, acceptNextWordOnly_includeTrailingSpaceIfAvailable, excludedApplications, maxCompletionLength=4, userPrompt}` · `ModelRepository_{selectedModel, statusItemVisible, shouldShowCompletedWordCountInMenuBar}` · `PersonalizationStrengthSlider` · `TextFieldContextCapture_pasteboardContextEnabled` · `TrainingDataCollector_enabled` · `EmojiCompletion_{preferredGender, preferredSkinTone, includeVanillaVariants}` · `io_replies_sender_{name,email}` · `ShortcutListener_forceEnableShortcut` · Sparkle `SU*`. Settings panes enumerated in §8. **[Note 06-08]** These namespaced forms are reconstructed from owning class + property; the concatenated keys (e.g. `CompletionManager_acceptFullCompletionShortcut`) are **not literal strings in the binary** — they are built at runtime from class name + property, so treat the names as semantic, not as grep-able literals.

**Overlay internals**: `InlineSuggestionsOverlayWindow` + `OverlayViewController` host `InlineSuggestionsListView` (row + badge + border views) over a `CompletionBackdropManager` backdrop (`SolidBackdropView`/`BlurredBackdropView`/glass effect) for legibility. `InlinePredictionDisableController` is a future owned-control integration point for supported native inline prediction suppression.

**Network/endpoints**: model CDN `models.cotypist.app` (zstd GGUFs); `cotypist.app/{setPreference,launchCotypist/setOverride}` web-driven config via URL scheme; `cotypist.app/{compatibility,appHelp/textMetrics,help/privacy,pricing}`; telemetry = Sentry (crash/perf, Frankfurt) + Google BigQuery (anonymous usage counts, Frankfurt), both default-on + user-disablable, **content never sent**. **[CORR 06-07, confirmed 06-08]** No network completion backend exists — the question is closed: `swift-protobuf` is bundled only to serve `RepliesSDK` feedback/screen-capture tooling, **not** a `replies.io` completion path. Completion is **fully local** (`CompletionManagerActor` + llama.cpp). There is no cloud/remote inference endpoint. Bundled deps of note: `MASShortcut` (configurable shortcuts), `LetsMove`, `CwlUtils`, `zstd`, `Sentry`.

**Feature-flag catalog (full product surface, observed):**
`featureConfigurableCompletionLength` · `featureMidLineCompletion` · `featureFullAutocorrect` · `featureEmojiCompletion` · `featureThesaurus{AutoMode,SelectionMode}` · `featureCustomInstructions{Global,PerApp}` · `featurePersonalization{Gentle,Balanced,Strong}` · `featurePasteboardContext` · `featurePreviousInputContext` · `featureCrossAppPreviousInputs` · `featureCustomModelOverride` · `feature{MidSize,Large}Models` · `featureUnlimitedCompletions` · `featurePerAppFontStyleOverrides` · `featureMultiDeviceSeats` · `featureCotypistLabsAccess`. **[Scope 2026-06-09]** In Cotypist these are paid-tier gates. **We clone the features but not the gates:** every `feature*` above is **always-on/available** (Labs included); `featureUnlimitedCompletions`, `feature{MidSize,Large}Models`, and `featureMultiDeviceSeats` are **dropped** (no quota, no seat licensing). Only *hardware capability* limits which models are offered (§15 D14).

**Thresholds/quality**: `deepMatchThreshold`, `reuseThreshold` (completion caching/reuse), `meetsQualityThresholds`, field-`Size Thresholds` (don't suggest in tiny fields), `wordCountAboveLengthThreshold` (stats).

**What we adopt:** prompt-based personalization (global+per-app/per-domain, **6-stop strength slider Off↔Max, full reach for all users — no tier caps**; §15 D2 + Project Scope), configurable shortcut matrix, word-capped length, pasteboard + previous-input context, optional screen-aware context, selectable model catalog (base+instruct), backdrop/mirror overlay, disable-native-inline-prediction where possible, pause/snooze, per-app overrides (incl. tab-key/smart-quotes/size-threshold/display), encrypted local stats/training data, compatibility guidance, quality/reuse thresholds.
**What we change:** **[CORR 06-07]** native Rust shell (`crates/app` + objc2/AppKit tray), **not Tauri**; updater + codesign/notarize is an open A3 decision (Tauri dropped — **Sparkle is the leading candidate, as Cotypist ships it**); Rust instead of Swift; `engine_core`/`model_client` built by hand (Cotypist's completion is Swift `CompletionManagerActor` + llama.cpp; `RepliesSDK` is unrelated feedback tooling). **[CORR 06-09]** Our input layer now matches Cotypist's no-CGEventTap accept-key architecture: AX + CGEvent synthesis + transient Carbon hotkeys; model fetch from HF or self-host TBD; telemetry disabled unless explicitly designed later.
**Deferred features (sequenced later, still in scope):** emoji completion, thesaurus, full autocorrect, cross-app previous inputs, web-driven config. Domain/website overrides are no longer optional for personalization/privacy parity; they are A2/A3 requirements for browser use. **Dropped (out of scope — no monetization):** subscription, paid tiers, multi-device seat licensing, completion quotas. (Cotypist's `cotypist://subscription` route and seat flags have no analogue here.)

---

## 14. Multi-platform sub-projects (committed deliverables, sequenced after A)

**[Scope 2026-06-09]** Windows and Linux are **in-scope goals**, not "maybe later" — multi-platform is a core project pillar. They are sequenced after the macOS MVP and built behind the same `PlatformAdapter` contract (§3), so `engine_core`/`platform`/`ranker`/`model_client` carry **zero macOS-only assumptions**. Validated in `2026-06-03-cross-platform-review.md`. Ordering reflects capability loss, not just porting effort: each step down loses a pillar of the macOS interaction model.

- **B. Windows** — `platform_windows`: UIA on a dedicated MTA worker thread + `WH_KEYBOARD_LL` accept + layered overlay (PMv2 DPI). Inference: Vulkan+CPU default, CUDA optional download. Strong tier = WPF/WinForms/Win32/native Qt; Electron/Chromium degrade to popup/hotkey.
- **C1. Linux X11 + Wayland(KDE/wlroots)** — `platform_linux`: `atspi` adapter + XTEST/wtype insert + override-redirect/layer-shell overlay + **dedicated-hotkey** accept (plain Tab can't be grabbed globally). AppImage distribution.
- **C2. Linux GNOME/Wayland + cross-platform IME path** — **separate architecture**: IBus **input-method-engine** backend with IME-native suggestion UI. GNOME/Wayland defeats overlay + key-intercept + front-app simultaneously, so the macOS model is *absent*, not degraded. Biggest single piece of Linux work.
- **D.** Cloud provider abstraction (behind `LocalModel`), browser extension, IDE plugins, remote compat registry, web-driven config, domain/website overrides.

**Cross-cutting (from review):** **[CORR 06-08]** the shell is native AppKit (objc2), not Tauri; render overlays with **native** windows per OS (a webview can't host click-through overlays — the original reason the design avoided one). Engine/inference crate stays OS-agnostic — only the llama.cpp build feature (`metal`/`vulkan`/`cuda`) + shipped runtime differ; build with `dynamic-backends` for one-binary GPU/CPU adaptation.

---

## 15. Audit status (Cotypist-parity audit, 2026-06-08)

Single source of truth for the parallel-agent audit (decompile of `/Applications/Cotypist.app` v2026.1 b73 + website + plan/code cross-check). Net: plan is strongly aligned with the real app; the items below are the remaining deltas. IDs are stable so later passes can report "fixed / pending" against them.

### Resolved this cycle (doc reconciliations)

| ID | Finding | Resolution |
|---|---|---|
| F3 | RepliesSDK / `replies.io` completion backend "unconfirmed" | Closed — confirmed **no network completion path**, fully local (§13). |
| F4 | Cotypist default model "Gemma 4" vs decompiled `gemma-3-*` | **Re-opened/reconciled 2026-06-08 → see D3.** The b73 **static binary has no `gemma-4-*` download id** (only `gemma-3-*` 1b/4b/270m + Qwen3 + Llama-3.2 + Phi-4-mini). "Gemma 4 E2B/E4B/26B-A4B" is **website/marketing + server-driven catalog** (`/features` protobuf), not a static fact. The earlier "both families ship, live-verified" claim overstated static evidence; the gemma-4 GGUF id is server-delivered, not in the shipped binary. Does not affect our Qwen2.5 choice. |
| I3 | Stale Tauri text presented as current | CPR §6 + this spec's pivot note + the online-validation "Sources" block now all marked historical. |
| I6 | A1 exit "no double ghost text" vs deferred cross-app suppression | Criterion scoped to owned/supported fields; cross-app suppression deferred A2+ (§9, §4 step 7). |
| I10 | Debounce spec (150–300 ms) vs shipped default (120 ms) | EMD §4 reconciled to "~120–300 ms, P1 ships 120 ms default, configurable". |
| G3 | No prefix/KV-cache reuse / long-lived llama context | **Closed (implemented + validated 2026-06-08).** `model_client::LlamaModel` now runs on a dedicated worker thread owning a **persistent** `LlamaContext`; `complete()` reuses the KV cache for the shared prompt prefix (`reusable_prefix_len` + `clear_kv_cache_seq`, re-decoding only the divergent suffix) and serializes calls via a mutex held across the round-trip. Backend is a `'static` singleton (fixes `BackendAlreadyInitialized` across instances). Proven by an ignored real-GGUF test (`prefix_reuse_matches_fresh_context_output`: reuse output == fresh-context output) and a live real-model run in the product binary. |

### Open — architectural deltas from Cotypist (decisions, not bugs)

| ID | Finding | Status / decision needed |
|---|---|---|
| **F1** | **Accept-key interception previously required Input Monitoring.** The old consuming `CGEventTap` path imposed a TCC prompt Cotypist avoids. **[CORR 06-08 — confidence raised to HIGH, see D1]** Full re-decompile of the bundle (main + every framework) found **zero `CGEventTapCreate`/`CGEventTapEnable`** — Cotypist ships **no event tap at all**. Its input layer is AX + CGEvent synthesis + Carbon hotkeys; the Tab swallow is a Carbon `RegisterEventHotKey` Tab registration. | **CLOSED in production code (2026-06-09):** `platform_macos` now installs transient Carbon accept hotkeys for bare Tab, grave, Esc, and Down only while a suggestion is armed, advertises `accept_intercept = CarbonHotkey`, and removes the accept-flow `PermissionMissing{"Input Monitoring"}` path. Spike `tools/spike/.../p8_carbon_hotkey.rs` ran (M4 Max): bare Tab (48) and grave (50) both registered with status 0. **Residual:** manual live cross-app consume + Input-Monitoring-revoked confirmation in the product binary remains GUI-bound; the code path is build/unit-validated and aligned with Cotypist's Accessibility-only mechanism. |
| **F2** | **Insertion default order is AxSet-first**; Cotypist's primary path is **pasteboard paste** (smart-insert + backspace + pasteboard restore), char/chunk fallback. We have all strategies but a different default. | Decide whether to flip to paste-first for Electron/web/terminal robustness, or keep AX-first with paste fallback. Currently AX-first (`platform_macos::insertion_strategy`). Low severity. |

_(G3 moved to **Resolved this cycle** above — implemented + validated 2026-06-08.)_

### Open — live-desktop validation gates (status after the 2026-06-08 live run)

Live run on Apple M4 Max, macOS 25.5, Accessibility + Input Monitoring granted. Initial gates ran single-display; G7 was later re-run with two displays (built-in 2× Retina + external 1×). The 2026-06-08 accept-key live evidence used the old consuming `CGEventTap` path; after the 2026-06-09 Carbon migration it remains historical evidence for bindings/insertion, not closure of product Carbon consumption.

| ID | Gate | Status |
|---|---|---|
| **G6** | live grave→Full desktop accept | **OLD TAP PATH CLOSED; CARBON PRODUCT PENDING (2026-06-09).** `tools/acceptance/e2e-compme.sh` closed the former CGEventTap path against TextEdit on 2026-06-08. Current production uses transient Carbon hotkeys, so closure now requires a physical-key product run: visible suggestion -> grave/key-above-Tab -> `accept Full` log -> inserted field contents. |
| **I11** | P0 E2E under current bindings | **OLD TAP PATH CLOSED; CARBON PRODUCT PENDING (2026-06-09).** The 2026-06-08 full/word runs proved the former bindings and insertion path, including real `LlamaModel` inference. Re-close for current bindings with physical Carbon input for full, word, Esc, Down, and Option+Tab. |
| **G5** | Chrome/Electron caret (zero-width collapsed caret) | **FIXED + live-validated (2026-06-08).** Root cause: `usable_caret_rect` required `w > 0`, so Chrome's **zero-width collapsed-caret** rect was rejected → `resolve_caret_rect` returned `None` → popup fallback. Fix: accept `w >= 0` (a collapsed caret is a zero-width bar; a zero-width rect can never be a container, which always has positive width) while still rejecting negative/oversized widths and non-positive heights. Live re-probe (Chrome, autofocus `<textarea>`): Chrome's caret now **resolves inline** — `resolved=Some(x:609,y:264,w:0,h:21)` where it previously fell to popup. Note: on this Chrome build the `AXSelectedTextMarkerRange` query returns a *null* rect (so the resolved `source` is `NativeFallback` via the zero-length-range tier, not `Marker`); an earlier run saw a non-null marker (`w:0,h:54`) which now classifies as `Marker`. The `source=Marker` *label* is Chrome-build-dependent, but the actual defect (zero-width caret unusable on Chromium) is fixed. Safari remains a proven `source=Marker` profile. |
| **G7** | Retina 2× / multi-monitor caret offset | **Measurement-closed; live 2× re-confirm pending (2026-06-08, two displays).** Built-in **Liquid Retina XDR 3024×1964 (true 2×, logical 1512×982)** + external **3840×1600 (1×, origin x=1512)**. TextEdit caret rect measured on each: built-in window {150,120,820,560} → `RECT x=328.4 y=220 w=1 h=14` (inside window, tight caret, **no 2× doubling**); external window {1700,200,2400,640} → `RECT x=1885.1 y=300 w=1 h=14` (correct **global** cross-display coords, offset by the display origin, no mismapping). `coords_global=true`, `overlay=NativePanel`. AX returns **points** on a genuine 2× Retina panel and the multi-monitor offset maps correctly. **Caveat — FIXED (2026-06-08).** `active_display_scales` now derives the backing scale from the current `CGDisplayMode`'s native `pixel_width()` over its point `width()` (pure helper `backing_scale`, unit-tested: 3024/1512→2.0, 3840/3840→1.0, 0→1.0), instead of `CGDisplayPixelsWide` which returns logical width for scaled Retina modes (always ~1.0). The pixel-correction branch in `normalize_ax_screen_rect` now has a correct scale to work with, so a pixel-reporting app on a Retina display would be corrected. Behaviour is unchanged for normal point coordinates (they land on a display → pass-through; only off-display coords that divide cleanly onto a display are corrected). Live single-display re-check reports the ultra-wide at 1.0 correctly; the 2× value is unit-proven (live 2.0 re-confirmation needs the built-in panel reconnected). |

**Remaining highest-leverage actions (now all scheduled, not open):** F1/G6/I11 manual physical-key product live consume confirmation under Carbon; D12 → b74 re-decompile task; G7 live-2× re-confirm (hardware-bound). G7's only residual is the latent true-backing-scale detection caveat above (revisit only if a pixel-reporting app surfaces).

### 2026-06-08 parallel-agent re-audit (D-series)

Three agents ran in parallel: (1) static+dynamic decompile of `/Applications/Cotypist.app` b73, (2) `cotypist.app` website research, (3) plan/code cross-check. New findings, IDs stable:

| ID | Finding | Severity | Resolution / action |
|---|---|---|---|
| **D1** | **Cotypist ships no CGEventTap.** Re-decompile found zero `CGEventTapCreate` in the bundle; input = AX + CGEvent synthesis + Carbon hotkeys. The old Compme consuming tap forced Input Monitoring; Cotypist avoids it. | High | Folded into **F1** (raised to high confidence). **Closed in production code 2026-06-09:** Compme now uses Carbon accept hotkeys and no accept-flow Input Monitoring prompt. |
| **D2** | **Personalization slider is 6-stop, not 3 — but only `/pricing` is authoritative for it.** `cotypist.app/pricing` shows **6 ticks** (Off↔Max), tier caps Free=2 / Plus=4 / Pro=6. **[CORR 06-09]** The `/help/personalization` page uses **older/simplified "It runs from Off to Strong" wording** (not six-stop), so do **not** claim both pages confirm six-stop — `/pricing` is the source of record. Gentle/Balanced/Strong (binary flags) are tier *thresholds*, not stops. | Medium (A2) | **Resolved in §6 + §1 table:** Cotypist's slider is 6-stop with tier caps (`/pricing` authoritative); **our product ships the 6-stop slider with FULL reach for all users — no caps** (Project Scope / §15 D15). Doc fixed. |
| **D3** | **`gemma-4-E4B-UD-Q5_K_XL` is not a static binary fact** in b73 (no `gemma-4-*` download id); "Gemma 4" is server-driven catalog + marketing. | Low | F4 reconciled; §1/§5/§13 corrected. No impact on our Qwen2.5-0.5B choice. |
| **D4** | **Plan model catalog incomplete:** missing **Phi-4-mini** (in binary); site advertises Gemma 4 + drops Llama. Static catalog ≠ site catalog (server-driven). | Low | §5 catalog updated (Phi-4-mini added; server-driven noted). |
| **D5** | **Trial-length mismatch — narrowed, not withdrawn (re-opened 2026-06-09).** Website pages (`/pricing` + landing) are internally consistent at **30-day Pro trial**, BUT the official **Sparkle appcast `cotypist.app/updates/cotypist.xml` says "a free 7-day trial is one click away"** — so **website (30d) vs appcast (7d) is a real first-party inconsistency**. The earlier "not reproducible" was too broad (it only checked website pages). | Low | **Open (cosmetic, Cotypist's inconsistency, not ours).** Use the website 30-day as the headline; note the appcast 7-day discrepancy exists. Does not affect our product (we ship no trial). Re-confirm on b74 appcast. |
| **D6** | Stale root docs: `ARCHITECTURE.md`/`DEVELOPMENT.md` still say "fresh llama context per completion / prefix-reuse not implemented" — contradicts closed **G3** + actual code. | Medium (trust) | **Fixed** in both root docs (point to G3 closure). |
| **D7** | `core` → `engine_core` crate rename not propagated to `README.md`, `ARCHITECTURE.md`, `DEVELOPMENT.md`, design §2/§9; `crates/core/` does not exist. | Medium (trust) | **Fixed** across docs. |
| **D8** | **P0 grave→Full live-gate self-contradiction:** earlier docs mixed old CGEventTap live evidence with the current Carbon product gate. | High (trust) | **Reconciled 2026-06-09:** §15 G6/I11 now distinguish **old tap path closed** from **current Carbon product physical-key evidence pending**; `ACCEPTANCE.md` and `run-a1b-live-gates.sh` report Carbon gates as explicit `MANUAL` gates rather than ordinary skips. |
| **D9** | `maxCompletionLength`: Cotypist live default **4**, our `DEFAULT_MAX_WORDS=8` — deliberate divergence, previously undocumented. | Trivial | **Documented** in §13. |
| **D10** | G7 true-2× Retina label vs `p1`/`ACCEPTANCE` "scale>1 unverified". 2× is unit-proven, not live-proven. | Low | **Resolved 2026-06-09:** G7 row label softened to **"Measurement-closed; live 2× re-confirm pending"** so it no longer overstates closure; matches `ACCEPTANCE.md` ("measurement-closed but not yet live-re-confirmed"). |

### 2026-06-09 peer-review additions (D11–D13)

Second-reviewer pass (ran `cargo test -p model_client --test latency -- --ignored` → both real-GGUF tests pass, independently confirming **G3**). New deltas:

| ID | Finding | Severity | Resolution / action |
|---|---|---|---|
| **D11** | **Cotypist control behavior is under-modeled.** Public docs (`/help/tips`, `/help/shortcuts`): **Esc dismisses + suppresses the current field**, **Option+Tab sends a literal Tab** (per-app Tab bypass), temporary per-app toggle, global toggle. | Medium | **CLOSED in code (2026-06-09):** Esc maps to dismiss+suppress, Option+Tab passes through because only bare Tab is a Carbon hotkey, and Down cycles candidates. Unit coverage exists in `engine_core` and `platform_macos`; `run-a1b-live-gates.sh` records Carbon accept-key gates as manual physical-key gates because macOS synthetic key posts do not fire `RegisterEventHotKey`. Per-app/global toggle stays A3 settings (§8). |
| **D12** | **Audit target is installed b73, not the current shipping build.** Installed `/Applications/Cotypist.app` = **2026.1 build 73**; the official DMG payload is **2026.1.1 build 74**. The appcast advertises `shortVersionString=2026.1.1` but `sparkle:version=73`, so trust the **DMG `Info.plist`**, not the appcast. | Medium | **Provenance fixed + tracked task:** §13 states evidence is from **installed b73**. **Scheduled action:** re-decompile the official **2026.1.1 b74** DMG and diff against the b73 findings here (entitlements, frameworks, model catalog, feature flags, control strings) before any "current Cotypist" claim; record deltas as D-series updates. Owner: next audit cycle. Does not change current design decisions. |
| **D13** | **Launch-at-login missing from parity planning.** Cotypist links `ServiceManagement.framework`, imports `SMAppService`, ships `shouldLaunchAtLogin` strings. Plan §9 A3 covers menu-bar/updater/signing but not launch-at-login. | Medium | **Added to A3 app-lifecycle scope** (§9): native launch-at-login via `SMAppService` (or login-item equivalent), default-off, user-toggleable. |
| **D15** | **Scope clarified + locked (2026-06-09):** project is **open-source, multi-platform, full Cotypist parity EXCEPT payment/licensing/tiers/seats.** Plan previously left a "tier/feature-gate decision" open (§8/§9/§13) and never stated OSS or committed multi-platform. | High (scope) | **Resolved:** Project Scope note added at top; tier gates removed (all features open, hardware-gating only); slider un-capped (§6/§13); subscription/seats **dropped** not deferred (§13); §9 A3 → OSS license + no-proprietary-telemetry + model-license passthrough; §14 + §5 mark Windows/Linux as committed deliverables. **[2026-06-09] `LICENSE` (Apache-2.0) added + `workspace.package.license` in `Cargo.toml` + README License section — the OSS claim is now backed by an actual license file, not aspirational.** |
| **D14** | **Model download UX under-specified.** Cotypist's public troubleshooting (`/help/troubleshooting`) adds requirements not captured: **direct-download recovery** when the CDN download fails, **manual model placement** (drop a GGUF into the models dir), and **hardware gating** for large Gemma-4-class models (don't offer models the machine can't run). | Medium (A3 backlog) | **Added to A3 model catalog/download acceptance** (§9): the download flow must handle failure recovery, allow manual GGUF placement, and gate large models by available RAM/hardware. **[2026-06-10]** model_catalog (4 entries, license gates, RAM advisory) + model_fetch pure core (streaming sha256, resume planning) shipped 2026-06-10; download loop + picker UI pending. |

| **D16** | **In-scope parity features + per-platform parity lacked explicit acceptance gates.** Deferred-but-in-scope features (emoji, thesaurus, full autocorrect, cross-app previous inputs, web-driven config) and personalization/compatibility requirements were listed without exit criteria; multi-platform was committed without a feature×platform parity matrix. | Medium | **Added §16 (parity acceptance gates)** — one concrete gate per in-scope feature incl. personalization storage modes + compatibility tiers — and the **feature×platform parity matrix** (`cross-platform-review.md` §7b). A feature/platform is "parity" only when its gate passes. |

**Binary surprises not yet modeled in the plan** (capture for A2/A3, not MVP blockers): server-driven feature config (protobuf `/features`, `fixed_features`/`overridable_features`); `cotypist://subscription` URL route; Sentry feedback+screenshot capture; `GemmaTermsNotice` licensing UX; `AppOverrides` GRDB table + per-domain overrides; Phi-4-mini in catalog; **`SMAppService` launch-at-login (D13)**; **Esc-suppress + Option+Tab bypass control semantics (D11)**.

---

## 16. Parity acceptance gates (per in-scope feature) **[added 2026-06-09 — D16]**

A feature is not "Cotypist parity" until its gate below passes (automated where possible, else manual QA recorded). Payment/licensing/tiers/seats are out of scope and have no gate. Per-platform status lives in `cross-platform-review.md` §7b.

### Control / shortcut parity (A1b/A2)
| Feature | Phase | Acceptance gate |
|---|---|---|
| Tab→Word, key-above-Tab→Full | A1b | Deterministic mapping + old tap insertion path closed; current Carbon product consume requires physical-key live re-close (§15 G6/I11). |
| Esc dismiss + suppress current field | A1b T5b | Esc hides ghost AND no new suggestion in that field until refocus/edit; unit test for `suppressed` set/clear; live TextEdit run |
| Option+Tab literal-Tab bypass | A1b T5b | Option+Tab inserts a literal Tab (no Word-accept, no swallow); `accept_tap_decision` Option+Tab→`None` unit test |
| Per-app Tab disable + per-app/domain overrides | A2/A3 | Override store gates suggestion/accept per app + per domain; round-trip test + live two-app check |
| Accept-key reconfiguration | A2/A3 | User can rebind both accept keys; persisted; takes effect without restart. **Model implemented**: `platform_macos::AcceptKeymap` (pub) — keycode→binding map, `from_accept_keys(word, full)` rebinds the two accept keys with collision + negative-keycode validation; `accept_tap_decision`, Carbon registration (`carbon_bindings`), AND the handler's id→keycode inverse (`keycode_for_hotkey_id`) are all now **data-driven from one keymap** (default preserves exact Cotypist bindings; 8 tests). Residual: thread a *configured* (non-default) keymap from `COMPME_ACCEPT_WORD_KEY`/`_FULL_KEY` through the live tap/registration + persistence — FFI wiring. |

### Personalization / privacy parity (A2) — sharpened per `/help/personalization`
| Feature | Acceptance gate |
|---|---|
| Custom instructions (global + per-app + per-domain) | Instruction text measurably steers completions; per-app supplements global; persisted; live before/after diff |
| 6-stop strength slider, full reach (no caps) | All 6 stops selectable by every user; higher stop = stronger steer (observable); no tier gating present |
| Encrypted local storage | DB encrypted at rest; key in OS keystore (Keychain/DPAPI/Secret Service); plaintext never on disk (inspect file to confirm) |
| Storage mode: accepted-only vs all-monitored | Both modes selectable; default **off** (opt-in); mode honored (verify only accepted completions stored in accepted-only mode) |
| Inspect + delete | Record count shown; delete-all works; per-app and per-domain deletion works; disable+erase removes the store |
| Sender identity | Name/email feed signature/contact awareness in prompt; editable |

**2026-06-09 test-audit status:** current code has deterministic coverage for
prompt construction, per-request app-scoped personalization, redaction, and
memory core behavior. §16 acceptance stays partial until settings persistence,
live keychain validation (the Keychain-backed `KeyProvider` is code-complete:
`platform_macos::keychain`, generate-on-first-use, env key as operator
override), and live before/after completion diffs are recorded.

### Context-source parity (A2)
| Feature | Acceptance gate |
|---|---|
| Pasteboard context | Opt-in; clipboard text augments prompt when enabled; off by default |
| Previous-input context | Recent same-app input augments prompt; bounded retention; redacted |
| Cross-app previous inputs | Opt-in; cross-app history augments prompt; privacy-bounded; **degrades on Wayland/GNOME (front-app limits — §7b)** |
| Screen-recording / OCR context | Opt-in behind Screen Recording permission; local OCR only; works without it; clear off path |

### Compatibility parity (A2) — executable/manual gates per `cotypist.app/compatibility`
| Tier (from compatibility page) | Acceptance gate |
|---|---|
| **Works** (Safari/Chrome/Mail/Word/TextEdit/Notes/Notion/Obsidian/Messages/Terminal/iTerm…) | Inline suggestion + accept verified live in a representative app per family; record in acceptance logs |
| **Setup needed** (Google Docs; Arc/Dia) | Onboarding detects missing Accessibility/Text-Metrics and guides the user; verified Docs round-trip after setup |
| **Mirror window only** (Firefox/Zen) | Mirror-window fallback renders + accepts; documented UX |
| **Partial** (Slack) | Documented partial behavior; no crash/lag |
| **Sidebar chats only** (VS Code/Cursor/Windsurf) | Suggests in AI-chat panels only, not the editor pane; verified |
| **Not supported** (Thunderbird/Pages/Scrivener/Ghostty/Warp…) | Explicitly disabled/listed; no misfire |
| Terminal/iTerm AI-agent prompt | Activates only in intended natural-language prompt contexts, not arbitrary shell input |

**2026-06-09 test-audit status:** `tools/acceptance/run-a2-compat-gates.sh`
is request-path smoke evidence for selected compatibility/context paths. It is
not a full replacement for the per-family live matrix above, especially setup
needed browsers, mirror-only apps, sidebar-only AI panels, insertion behavior,
and onboarding copy.

### Other in-scope features — now with gates (close D16's "loosely deferred")
| Feature | Phase | Acceptance gate |
|---|---|---|
| Emoji completion (skin-tone/gender prefs) | A2/A3 | Emoji suggested from text; preference honored; toggleable. **Suggester implemented**: `crates/emoji::suggest` detects a trailing `:shortcode` (start/whitespace-anchored, ≥2-char prefix or exact, alias table) and renders the glyph honoring `EmojiPrefs` skin-tone (Fitzpatrick, orthogonal to gender for the neutral variant) + gender (neutral/female/male ZWJ); returns `replace_chars` (typed length) for the host (22 tests). **WIRED (cycle 26):** run_loop offers the emoji ghost on a typed `:shortcode` (`replacement_offer`/`replacement_decision`), accept emits `Command::Replace` → AxSet range-replace; `COMPME_EMOJI` (+`_SKIN_TONE`/`_GENDER`) enable toggle, default off; gated + race-free (AxSet-only). **LIVE §16 GATE PASSED (2026-06-10):** physical-key Tab accept in TextEdit deleted the typed `:smile` and inserted `😄` on the caret line (ACCEPTANCE.md, A2 Local-Replacement Live Gate). **Backspace-synthesis DONE + live-validated 2026-06-10** (poster seam + AxSet readback fallback; iTerm2 silent-write case proven by scripted accept). Accept paths also verified in Safari's address bar and a Chrome textarea; Chromium/iTerm2 caret rects normalized for placement. |
| Thesaurus (auto + selection mode) | A2/A3 | Synonym suggestion on word selection / auto mode; toggleable. **Lookup implemented**: `crates/thesaurus::synonyms(word)` returns curated synonym-group alternatives (case-insensitive, query-case reapplied lower/Title/UPPER, multi-group merge + dedup, word excluded) + `has_synonyms` for auto-mode gating (15 tests). Residual: wire into the host (selection/auto trigger + `COMPME_THESAURUS_*` toggle + replacement insertion) — engine/host integration, like emoji. |
| Full autocorrect vs typo/suggested-fix | A2/A3 | Typo fix distinct from full autocorrect (separate toggles, per `/help`); no false-correct in code fields. **Typo-fix half implemented**: `crates/autocorrect::correct(word)` — high-precision curated common-typo table (each key is NOT a valid English word, so real words are never altered — false-correct contract tested), query-case reapplied via shared `crates/textcase`, multi-word (`alot`→`a lot`); `is_typo` for gating. **WIRED (cycle 27):** run_loop offers the correction on a trailing-word typo (`replacement_offer`), accept emits `Command::Replace`; `COMPME_AUTOCORRECT` enable toggle, default off. **LIVE §16 GATE PASSED (2026-06-10):** physical-key Tab accept replaced a typed `teh` with `the` in TextEdit (ACCEPTANCE.md). Residual: full statistical autocorrect (NSSpellChecker-equiv, platform), separate per-toggle UI, and the host code-field gate. |
| British English normalization (Cotypist 0.22 Labs) | A2/A3 | US→UK spelling normalization (e.g. `color`→`colour`, `organize`→`organise`); high-precision (curated US→UK table, no false positives on words that are valid in both), query-case reapplied via shared `crates/textcase` (`CasePattern`), toggleable. **Pure crate `localize` (mirrors `autocorrect`/`thesaurus`):** curated US→UK map keyed only on US-only forms so shared spellings are never altered; case-pattern reapplication preserves lower/Title/UPPER; gated by a `COMPME_BRITISH_ENGLISH` host toggle (default **off**). **WIRED (cycle 27):** run_loop offers the UK spelling on a trailing-word US-only form (`replacement_offer`), accept emits `Command::Replace`. **LIVE §16 GATE PASSED (2026-06-10):** `color`→`colour` ghost offered + placed on the caret line live (Esc-dismiss also verified); the accept is the byte-identical shared path live-verified by the emoji/autocorrect accepts (ACCEPTANCE.md). |
| Web-driven config (`setPreference`/`setOverride` deep links) | A3 | URL-scheme deep link applies a per-app/domain override; signed/validated; user-visible. **Parser + application implemented**: `crates/webconfig::parse_deep_link` strictly validates `compme://setOverride?...` (scheme/command/param allow-list, app XOR domain, strict `true`/`false`, charset+length-bounded scope, fail-closed on anything unknown — 18 tests); `prefs::apply_override` maps the reversible command onto the policy store (App enable = full allow that also clears exclude). Restricted to a **reversible, user-visible** subset deliberately. **Signing implemented**: `parse_deep_link_with_trust` verifies a trailing `&sig=` Ed25519 signature (128 hex, `verify_strict`, byte-prefix payload — no canonicalization, sig must be final param) against a host-pinned `TrustedKey`; no key configured → signed links rejected fail-closed (10 tests, crate total 28). Any future non-reversible command must be gated on `LinkTrust::Signed`. **Reception SHIPPED + validated live 2026-06-10**: the bundle declares the scheme (CFBundleURLTypes, c80) and `platform_macos::install_url_event_handler` (NSAppleEventManager kAEGetURL) feeds the run loop, which parses fail-closed, applies the override, persists the exclude list, fires the dismiss edge, and logs every outcome — a scripted `open compme://…` round-trip applied an Exclude (persisted), rejected a garbage command, and restored via Include. **Signed links validated live 2026-06-10**: a link signed by the `sign_link` dev example verified against `COMPME_TRUSTED_KEY` and applied (`(Signed link)` logged); a tampered payload was rejected (`signature verification failed`). **Confirmation prompt SHIPPED + blocking-verified live 2026-06-10**: every link routes through the pure `prompt_decision_for_link` and a blocking NSAlert (Cancel is the default button; declined = rejected, prefs untouched — test-pinned); scripted runs proved the modal HOLDS the link until answered and an Allow click applied+persisted. Residual: the trusted-key distribution decision (ship-time choice, not code) and a polished Allow/Cancel LOOK pass. |
| Multi-candidate / cycle | A2 | N candidates generated; cycle shortcut switches; accept inserts the shown one |
| Trailing space after single-word completions (Cotypist "Shortcuts" toggle) | A2/A3 | A config toggle (`COMPME_TRAILING_SPACE`) that, when enabled, appends a single trailing space when the accepted completion is a single word; default off. Pure core implementable in the `engine_core` accept-insert path + the config key; live echo-path validation (the inserted space round-trips through the host text field) is the FFI residual. |
| Pause / snooze | A2/A3 | "disable for N minutes" gates suggestions; auto-resumes; per-app exclude list |
| Native inline-prediction suppression | A2+ | Suppressed in owned/supported fields; cross-app explicitly deferred (no double ghost) |
| Configurable completion length (`featureConfigurableCompletionLength`) | A2/A3 | User sets word cap; ranker honors the cap; persisted; takes effect without restart |
| Mid-line completion (`featureMidLineCompletion`) | A2/A3 | Inserts within a line without duplicating right-context text (suffix-overlap guard); toggleable |
| Custom model override (`featureCustomModelOverride`) | A3 | User points at own GGUF; loads behind `LocalModel`; surfaces the model's license (ties to D14 manual-placement) |
| Per-app display overrides (`featurePerAppFontStyleOverrides`, smart-quotes, text-mirroring, size-thresholds) | A3 | Each override persists, applies per app, and has an observable effect; size-threshold suppresses suggestions in tiny fields |
| Labs / experimental (`featureCotypistLabsAccess`) | A3 | Labs flags are user-toggleable and surfaced; no tier gating present (all open per Project Scope) |
| Local stats / menu-bar word count (`shouldShowCompletedWordCountInMenuBar`) | A3 | 30-day shown/accepted/dismissed/superseded + latency + words computed and displayable; menu-bar word-count toggle works. **Compute half implemented**: `crates/stats` rolling-30-day accumulator (counts, words_completed, acceptance_rate, latency avg/p95 nearest-rank; time-injected, 20 tests) wired in `app` run loop. All four outcomes now recorded live: Accepted/Dismissed from host inputs; **Shown/Superseded surfaced by `engine_core` (`StatEvent` + `take_stat_events`, with failed-placement `Shown` retraction and completion-replace supersede) and drained each loop turn**. **Latency recorded too**: the run loop times submit→outcome per request generation (`latency_sample`, monotonic-generation pruned, heartbeat-resolution) → `usage.record_latency`; shutdown summary prints all counts + words + latency avg/p95. **Menu-bar display SHIPPED**: `stats::summary_line` (words · accepted (rate%); rate omitted when nothing shown; idle placeholder) rendered as a non-interactive `MacosTray::set_stats_line` menu row, diffed per heartbeat on the wall clock. Residual (A3): live LOOK validation, display toggle, and persistence across launches. |
| Quality / reuse thresholds (`deepMatchThreshold`, `reuseThreshold`, `meetsQualityThresholds`) | A2/A3 | Internal completion-quality tuning; either surfaced in a Labs/General control or explicitly marked non-user-facing |

**Multi-platform rule:** each gate above is written platform-agnostically; per-platform achievability (✓/◑/⌨/✗) is in `cross-platform-review.md` §7b. A platform claims a feature only when its gate passes there with that platform's mechanism.
