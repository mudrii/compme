# Design — Sub-project A: Engine + macOS MVP

**Date:** 2026-06-03
**Status:** Approved, then revised with online validation + Cotypist reverse-engineering
**Scope:** First sub-project of a long-term cross-platform predictive-writing product (Cotypist-like). Covers **only** the OS-agnostic engine + the macOS adapter + the app shell. Windows/Linux adapters and ecosystem are later specs behind interfaces defined here.

**Revision note (v2):** This document was re-validated against current crates/docs (Feb–Jun 2026) and against the shipping Cotypist binary (`/Applications/Cotypist.app`, v2026.1 build 73) via static analysis + its live `UserDefaults`. Material corrections are marked **[CORR]** and the evidence is in §12–§13.

---

## 1. Context & motivation

Local, privacy-first predictive writing: read editable text around the caret in other apps, predict next word/phrase with a **local** model, show inline grey ghost text, accept incrementally with a configurable shortcut.

Validated against the shipping Cotypist binary:

| Aspect | Cotypist (binary + libs + live prefs) | Decision for this product |
|---|---|---|
| Inference | `libllama` + `libggml-metal` (ggml 0.12.0), llama.cpp, Metal | llama.cpp via `llama-cpp-2` (feature `metal`), Metal backend |
| Models | **Selectable catalog**, Instruct: Gemma 3/4, Qwen 3 (e.g. `gemma-4-E4B-UD-Q5_K_XL`); downloaded first-run | Selectable catalog; start small (see §5); download first-run |
| Storage | `GRDB.framework` (SQLite); training data "encrypted, stored locally" | `rusqlite` + `bundled` (FTS5 included) |
| App shape | Menu-bar agent, `LSUIElement=true`, status item, no dock | Tauri v2 tray app, `ActivationPolicy::Accessory`, hidden settings window |
| Engine | Custom `RepliesSDK.framework` (prompt build, sampling, sender identity) | Our `core` + `ranker` + `model_client` |
| Personalization | **Prompt-based**: `userPrompt` custom instructions + strength slider + sender name/email; optional training-data collector | Same: prompt-based primary; optional local memory later |
| Context source | AX **+ pasteboard fallback** + previous-input / cross-app history | AX primary; pasteboard + previous-input augmentation (latter deferred) |
| Models CDN | Self-hosted `models.cotypist.app` (zstd), sourced from HF | HF direct or self-host (TBD) |
| Native inline prediction | Disabled while active (`InlinePredictionDisableController`) | Same — must suppress macOS 14+ inline prediction |
| Accept | **Configurable, two-tier**: full + partial(next-word) shortcuts; `maxCompletionLength` in words (default 4) | Same model: 2 configurable shortcuts, word-capped |
| Update | Sparkle (`SUFeedURL` cotypist.app/updates) | **[CORR]** Tauri `updater` plugin (drop Sparkle — §12) |
| Analytics | Sentry, opt-out per app | Optional; local-only by default |
| Entitlements | `com.apple.security.automation.apple-events`; not sandboxed | Same; hardened runtime + notarize |
| Language | Swift native | **Rust** (chosen for cross-platform reuse) |

### Non-goals (this spec)
- Windows (UI Automation) / Linux (AT-SPI) adapters — behind `PlatformAdapter`.
- Swappable cloud providers (Ollama/OpenAI) — behind `LocalModel` trait.
- Browser extension / IDE plugins / remote compat registry.
- On-device fine-tuning (personalization is prompt + optional retrieval, never weight training).

---

## 2. Architecture

Single process. Tauri v2 = tray, lifecycle, hidden settings webview. Rust core (AX, overlay, inference, prefs) in Tauri's backend. **Three run-loop contexts** (validated, §12):

- **Main thread / AppKit run loop** — Tauri's loop; all NSPanel/overlay calls hop here via `app_handle.run_on_main_thread`.
- **CGEventTap thread** — own thread + `CFRunLoopRun`; key interception (accept shortcut). Callback must be non-blocking and answer from **pre-computed state** (never a synchronous AX call).
- **AX/inference worker** — background thread/queue; AX IPC (with short messaging timeout) + llama.cpp decode.

```
┌─ Tauri v2 tray app (one process, ActivationPolicy::Accessory) ─────────┐
│  tray menu/status item · hidden settings webview · lifecycle           │
│                                                                         │
│  ┌── core ───────────────┐   suggestion state machine, debounce,        │
│  │ generation tokens,    │   accept logic (full/partial), app policy     │
│  │ invalidation, cancel  │                                               │
│  └───────┬───────────────┘                                              │
│   ┌──────┴────────┬────────────┬───────────────┬──────────────┐         │
│  context        ranker      model_client    personalization  prefs       │
│  (AX read +    (score,     (LocalModel:    (custom-instr     (UserDefaults│
│   pasteboard    trim,       llama-cpp-2,    prompt, strength,  -equivalent,│
│   fallback,     boundary)   Metal, warm,    sender identity,   per-app    │
│   caret rect)               prefix-cache)   opt. memory)       overrides) │
│                                                                         │
│  ┌── platform (trait PlatformAdapter) ───────────────────┐              │
│  │  platform_macos: AX (accessibility-sys/objc2),         │              │
│  │  CGEventTap, NSPanel overlay, NSWorkspace front-app    │              │
│  └─────────────────────────────────────────────────────── ┘             │
└─────────────────────────────────────────────────────────────────────────┘
```

### Workspace
```
crates/core             # state machine, generation tokens, invalidation, cancel, accept logic, policy
crates/context          # TextContext, Selection, Caret + AX+pasteboard capture model
crates/ranker           # candidate trim/boundary/repetition/score
crates/model_client     # LocalModel trait + llama.cpp impl (warm-up, prefix cache, N-sample)
crates/personalization  # custom-instructions prompt builder + sender identity + optional rusqlite memory
crates/prefs            # settings store + per-app overrides (mirrors Cotypist's pane model)
crates/platform         # PlatformAdapter trait + shared types (cross-platform contract)
crates/platform_macos   # AX read, CGEventTap, NSPanel overlay, front-app detection
apps/app                # Tauri v2 tray + settings webview, wiring
tools/spike             # throwaway A0 PoC (deleted after A0)
```

**Crate strategy** (verdicts in `2026-06-03-prior-art-review.md` §3): build the AX/tap/inject layer natively via `objc2` + `objc2-app-kit` + `accessibility-sys`/`axuielement` + `core-graphics`; inference via `llama-cpp-2` (C-API surface, `metal`). **Do NOT depend on `rdev`/`rdevin` for the capture path** (stale / grab-disabled on Linux) — KeyType, Cotabby, and Espanso all hand-rolled native capture. `enigo` only as an inject shortcut later.

---

## 3. The cross-platform contract **[CORR — expanded after Win/Linux validation]**

Capability-first so `core` never special-cases apps; capabilities drive UX mode. This is the **validated** shape (see `2026-06-03-cross-platform-review.md` §4): Windows and Linux independently forced strategy enums + extra flags. macOS fills the rich values now so B/C slot in without reshaping the contract.

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
    pub accept_intercept: KeyInterceptMode, // macOS: CgEventTap  (Win: LowLevelHook; X11: XGrabKey; Wayland: HotkeyOnly/ImeOwnsKey/None)
    pub overlay_at_caret: OverlayPlacement, // macOS: NativePanel  (≠ readable_caret — GNOME/Wayland can read caret but not place)
    pub coords_global_screen: bool,         // can caret rect be used for absolute positioning?
}
```

Rationale per field is in the review (§4). The macOS adapter implements `accept_intercept = CgEventTap`, `overlay_at_caret = NativePanel`, `insert_strategy ∈ {AxSet, SyntheticKeys, Clipboard}` (probe writable → fall back), `toolkit` detected via bundle id / framework. `subscribe_caret` is split from focus because on Windows/Linux caret events are the expensive ones — macOS keeps the split for contract uniformity even though AX is cheaper.

### UX mode derivation (in `core`)
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
2. **Debounce** ~150–300 ms of keystroke quiescence; gate: not mid-word unless configured, not on backspace, min context length.
3. Snapshot context → compute **generation token** = hash of `{element id, text length, caret offset, left-context tail}`.
4. `model_client` runs inference (warm model, cached prefix). **Cancellation token** checked between decode steps; superseded request → drop-all-but-latest.
5. On return, **discard unless current generation token still matches** (stale-race guard).
6. `ranker` trims to word boundary, caps at `maxCompletionLength` words, applies repetition/sensitive penalties.
7. Overlay renders top candidate at `caret_rect` (Retina/multi-monitor coordinate conversion, §12) or popup fallback. Render over a **backdrop** (solid/blurred/glass, configurable) for legibility on arbitrary app backgrounds. **Disable macOS native inline prediction** while active (else double ghost text). Multi-candidate shows as an inline list (row + badge views).
8. **Accept**: full-completion shortcut inserts all; partial shortcut inserts next word (+ trailing space if available). Shift-equivalent cycles candidates; Esc dismisses.
9. **Invalidation** (any → drop suggestion): non-accept keystroke, caret/selection move, focus/app change, mouse click, text no longer matches prefix.
10. `personalization`/stats record outcome locally (redacted).

**Implementation reality (from prior-art code — `2026-06-03-prior-art-review.md` §2):**
- **Two-tap CGEventTap, not one.** A single always-on active `.defaultTap` stalls keystrokes in *other* apps (Cotabby DaVinci freeze). Use a permanent `.listenOnly` observer tap + a transient `.defaultTap` consuming tap installed **only while a suggestion is visible**. Re-enable on `tapDisabledByTimeout/UserInput`; defer mach-port teardown ~50 ms (else last accepted word lost).
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
- Latency: 0.5–1.5B Q4 on M-series ≈ 30–80 tok/s. Sub-150 ms first suggestion feasible **only** warm + short prompt. Cotypist targets ~100–200 ms and shipped on Qwen 2.5 1.5B before expanding the catalog.
- **Model: selectable tiered catalog** (mirrors Cotypist). Cotypist self-hosts GGUFs at `models.cotypist.app` (zstd-compressed), sourced from HF (unsloth `UD-Q*_K_XL` dynamic quants, `mradermacher *-i1-GGUF`). Catalog observed: Gemma 3 1b/4b-it-UD-Q4, Gemma 3 270m, Llama-3.2-1B/3B-Instruct-UD, Qwen3-0.6B/1.7B/30B-A3B-Base-i1, Gemma 4 E2B/E4B. We can either self-host similarly or pull from HF directly. Start tier "always fast": **Qwen3-0.6B / Qwen2.5-0.5B / gemma-3-1b**, Q4_K_M (~350–490 MB). Quality tier: ~1.5–1.7B (`featureMidSizeModels`); large tier behind capability gate.
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
- **Strength = 3 discrete levels** (`featurePersonalization{Gentle,Balanced,Strong}`), not a continuous slider. Controls how hard instructions + memory steer.
- **Sender identity**: name + email (`io_replies_sender_*`) for signature/contact awareness.
- **Custom model override** (`featureCustomModelOverride`): user may point at their own GGUF. Behind `LocalModel`; defer UI to A3.
- **Context augmentation (deferred to A2/later)**: previous-input context — recent text the user typed (same app, and cross-app `featureCrossAppPreviousInputs`) — fed as extra context. Privacy-sensitive: opt-in, redacted, bounded retention.
- **Optional local memory (deferred within A2)**: `rusqlite` + FTS5 store of accepted completions for retrieval-augmented prompting + ranker similarity score. Opt-in (`TrainingDataCollector` — encrypted, local, record count + "disable and erase"), inspectable.
- **No fine-tuning, ever.** Memory/inputs feed the prompt/ranker, never weights.
- **Redaction before any persistence**: emails, card-like numbers (Luhn), tokens/secrets (regex; `pii-vault`/`redact`). Diagnostics text-redacted by default.

---

## 7. Privacy & safety (first-class)

- Never read/store secure fields: block on `AXSecureTextField` subrole **and** `IsSecureEventInputEnabled` (§12).
- All inference local by default (only backend this spec). No raw-text logging by default.
- Visible **pause/snooze** ("disable for N minutes", as Cotypist) + per-app exclude list (default-exclude Finder-like) + per-window incognito.
- Custom-instructions & memory are user-visible/editable; clear retention + "forget learned data".

**Distribution & permission lifecycle (prior-art §2 — category's #1 support burden):**
- **App Sandbox OFF**; hardened runtime needs `com.apple.security.cs.disable-library-validation` to load the dynamic llama framework → **Mac App Store impossible**. Ship Developer-ID DMG + Tauri updater. Entitlement `com.apple.security.automation.apple-events`.
- **Stable signing identity** — TCC keys on cert+bundle-id; a cert change under the same bundle id causes an infinite "grant Accessibility" loop. Provide a `tccutil reset` recovery path + re-grant detection after OS updates.
- Detect when **Secure Input** is stuck (background password managers) — it kills all injection globally; surface it in diagnostics.
- Onboard **both** Accessibility + Input Monitoring; re-check after grant (may need relaunch).

---

## 8. Settings / config surface (mirrors Cotypist's panes — §13)

| Pane | Options |
|---|---|
| General | Completions enabled by default · `maxCompletionLength` (words, `featureConfigurableCompletionLength`) · show suggested fixes / autocorrect (`featureFullAutocorrect`) · mid-line completion (`featureMidLineCompletion`) · menu-bar word-count |
| Personalization | Global custom instructions · per-app custom instructions · strength (Gentle/Balanced/Strong) · sender name/email · training-data collection (enable / disable+erase / record count) |
| Model | Selectable catalog (tiered) · download manager · custom model override (own GGUF) |
| Shortcuts | Accept-full · accept-partial(next-word) · force-enable (all configurable; via MASShortcut-equivalent) |
| App Overrides | Per-app enable/disable/exclude · per-app strength · per-app: **Tab-key behavior, Smart Quotes, Text Mirroring, Size Thresholds, Display/backdrop+font** · per-app instructions. (Domain/website overrides later; app-only knobs excluded from domain overrides.) |
| Context | Pasteboard-context toggle · previous-input context · cross-app previous inputs |
| Display | Backdrop style (solid / blurred / glass) · suggestion color/symbol · font style (`featurePerAppFontStyleOverrides`) |
| Permissions | Accessibility status · Input Monitoring status · pasteboard permission |
| Emoji | Emoji completion · skin tone · gender |
| Labs | Experimental flags (`featureCotypistLabsAccess`); e.g. thesaurus auto/selection mode |
| About / Update | Version · auto-update (Tauri updater) |

Stored in a `prefs` crate keyed like Cotypist (`CompletionManager_*`, `ModelRepository_*`, `feature*`, per-app override list). Cotypist also supports **web-driven config** (`cotypist.app/setPreference`, `/launchCotypist/setOverride` deep links via URL scheme) for pushing compatibility fixes — optional later.

---

## 9. Phasing (Sub-project A)

| Phase | Weeks | Deliverable | Exit criterion |
|---|---|---|---|
| **A0 spike** (throwaway) | 1–2 | (1) caret **ladder** read in a native app (TextEdit) AND a Chromium app (AXTextMarker path); (2) **two-tap CGEventTap** that swallows a test key without stalling other apps, behind Input Monitoring; (3) NSPanel overlay (Retina-correct); (4) warm llama.cpp round-trip + latency table + KV-reuse rules for the chosen model; bench base-vs-instruct | All four work in real apps; two-tap proven stall-free; sub-150 ms warm latency confirmed or model retiered |
| **A1 core loop** | 3–4 | `PlatformAdapter` + macOS adapter + suggestion lifecycle (§4) + configurable accept + ghost overlay (backdrop + **disable native inline prediction**) + **secure block (subrole + secure-input)** | Type in Notes/Mail → inline suggestion → accept; passwords & secure-input blocked; no stale inserts; no double ghost text |
| **A2 features** | 3–4 | Prompt-based personalization (custom instructions + strength + sender) + pasteboard context fallback + multi-candidate + cycle; optional rusqlite memory if time | Suggestions steered by custom instructions; cycling works; Electron apps get keystroke/clipboard insertion |
| **A3 settings + ship** | 2–3 | Tauri settings (all §8 panes) + per-app overrides + model catalog/download + diagnostics + pause/snooze + Tauri updater + codesign/notarize (hardened runtime + entitlements) | Installable signed/notarized `.app`; configurable; self-diagnosing; two-permission onboarding |

~9–13 weeks solo to a shippable macOS app.

---

## 10. Risks (updated with validation)

| Risk | Sev | Mitigation |
|---|---|---|
| **Tab/accept interception** needs CGEventTap + **Input Monitoring** (global-shortcut CANNOT swallow keys) | High | A0 proves it; surface missing-permission state; gate consumption by app-focus/context |
| **Single CGEventTap stalls OTHER apps' input** (real bug: Cotabby #328) | High | **Two-tap design** (listen-only observer + on-demand consuming tap); never block the callback |
| CGEventTap fragile at runtime (`tapDisabledByTimeout/UserInput`, sleep/wake) | High | Re-enable on disable events, re-create on wake, keep callback non-blocking, periodic self-test |
| **Reading AX perturbs target apps** (Calendar/System Settings glitches) | Med | Non-invasive caret strategy for native single-line; full resolver only for web/multiline; text-eligibility gate |
| **Hybrid-model KV-cache corruption / ggml exit-abort** | Med | Pure-append reuse only or full re-decode; prefer non-recurrent small model; explicit `shutdown()` |
| **TCC re-grant loop on cert change; permission silent-stop after OS update** | Med | Stable signing cert; `tccutil reset` recovery UX; re-grant detection |
| `caret_rect` collapsed-range returns `kAXErrorNoValue` in most apps | High | "Bounds of adjacent char" workaround + element-frame fallback (designed-in) |
| Electron/Chromium apps expose poor AX tree | High | Detect Electron → keystroke/clipboard insert + pasteboard context + popup positioning |
| **Secure Input mode** blocks AX/taps in password fields | Med | Detect `IsSecureEventInputEnabled`; suppress entirely |
| llama.cpp + Tauri vendored C++ build (clang/cmake, slow) | Med | Pin versions; prebuilt artifacts in CI; mistral.rs fallback evaluated in A0 |
| Two TCC permissions (Accessibility + Input Monitoring), revocable, post-grant relaunch | Med | Onboarding sequences both; runtime detect each; guide to correct Settings pane |
| AX synchronous IPC can block (6 s default timeout) | Med | Off-main worker; `AXUIElementSetMessagingTimeout` short; handle `kAXErrorCannotComplete` retry |
| Single process: settings-UI panic stalls predictions | Low | Prediction on dedicated thread; `catch_unwind` around UI |

---

## 11. Success metrics
First-suggestion perceived latency <100–150 ms (warm); **<500 ms p95 is the hard floor** — slower "feels laggy and reduces acceptance" (industry threshold). Insertion failure <1% in supported apps · <5% laggy sessions · clear tier for top ~20 macOS apps · local stats: shown/accepted/dismissed/superseded, latency, words (30-day, mirrors Cotypist stats).

**Acceptance is trust-compounding** (66k-interaction study: prior per-user acceptance dominates future acceptance) → **protect first-run**; conservative triggering (fire near word/sentence boundaries, not every keystroke) beats always-on. Narrow scope deliberately — cede code/terminal to Copilot (as Cotypist does); own non-code writing.

---

## 12. Online validation results (Feb–Jun 2026) — evidence

- **objc2 v0.6.4** (maintained) + **accessibility-sys/accessibility v0.2.0** (thin, 1 maintainer) provide AXUIElement FFI. Prefer `accessibility-sys` + own wrappers; CGEventTap suppression is hand-written FFI via `core-graphics`/`objc2`.
- **Caret rect = a 5-tier ladder** (confirmed by KeyType `AXCaretGeometryResolver`, prior-art §2), not one workaround: (1) `kAXBoundsForRangeParameterizedAttribute` zero-length range — *works in many native apps, try first*; reject empty/container-sized rects; (2) **web path** — Chromium/WebKit need `AXSelectedTextMarkerRange`→`AXBoundsForTextMarkerRange` (opaque markers, NOT NSRange); (3) previous-char `NSRange(loc-1,1)` → `maxX`; (4) `AXStaticText` child-run interpolation; (5) font-metric estimate. Plus **Retina pixel-vs-point**: validate against `AXFrame` anchor, divide by per-display `backingScaleFactor` if mismatched.
- **Focus events** = `AXObserver` + `kAXFocusedUIElementChangedNotification` (+ caret via `kAXSelectedTextChangedNotification`); deliver on a CFRunLoop thread.
- **Secure field** = **subrole** `AXSecureTextField` (role stays `AXTextField`); also honor `IsSecureEventInputEnabled`.
- **Accept-key interception** = **CGEventTap** (`.cgSessionEventTap`, `.defaultTap`, return nil to swallow), needs **Input Monitoring**; Carbon hotkeys / `NSEvent` global monitors are passive and **cannot consume** keys. ← single most important correction.
- **CGEventTap fragility**: handle `kCGEventTapDisabledByTimeout/UserInput`, re-create on wake; non-blocking callback.
- **Overlay** = `NSPanel` `.nonactivatingPanel`, `.floating`, `canJoinAllSpaces|fullScreenAuxiliary`, clear/`ignoresMouseEvents`; never `activate(ignoringOtherApps:)`. `tauri-nspanel` plugin exists.
- **AX IPC** synchronous, 6 s default timeout → off-main + lower timeout.
- **Tauri v2**: `ActivationPolicy::Accessory` (=LSUIElement), `TrayIconBuilder`, hidden `WebviewWindowBuilder::visible(false)`; run loop is AppKit's; AppKit calls via `run_on_main_thread`. **Official `updater` plugin → use it, drop Sparkle** (redundant/conflicting). `tauri build` does codesign + notarize.
- **Inference**: `llama-cpp-2` v0.1.146, `metal` feature, vendored cmake build; warm-up + prefix cache critical; N-sample (no beam search); 30–80 tok/s for 0.5–1.5B Q4.
- **Models**: Qwen2.5-0.5B/1.5B-Instruct GGUF Q4_K_M exist (~491 MB / ~1.12 GB); base cleaner for completion but Instruct works with constraints (Cotypist ships Instruct). FIM = code-only → drop for v1.
- **Storage**: `rusqlite` `bundled` includes FTS5 (no separate flag); `directories::ProjectDirs` for paths (`cache_dir()` for the model); regex+Luhn redaction (`pii-vault`/`redact`).

---

## 13. Cotypist reverse-engineering — how it operates

**Binary**: arm64 Swift, `LSUIElement=true`, min macOS 14, entitlement `com.apple.security.automation.apple-events`, not sandboxed. Libs: `libllama`/`libggml*` (Metal), `GRDB` (SQLite), `RepliesSDK` (own engine), `Sparkle` (update, `SUFeedURL=cotypist.app/updates`), `Sentry`.

**Operation (from class names + live prefs):**
- `CompletionAccessibilityMonitor` watches focus/text via AX; `TextFieldContextCapture` reads field context **with optional pasteboard augmentation**.
- `CompletionManagerActor` (Swift actor → serialized concurrency) builds a `CompletionRequest` (prompt = custom instructions + context), runs local inference, returns `CompletionResult`.
- `CompletionOverlayManager`/`CompletionBackdropManager` render ghost text; `CompletionInserter` inserts on accept.
- `ShortcutListener` + key monitor handle **configurable** accept-full / accept-partial / force-enable shortcuts.
- `ModelRepository` manages a **tiered selectable model catalog**; `DownloadAndRenameTask` downloads the chosen GGUF first-run (current: `gemma-4-E4B-UD-Q5_K_XL`).
- Pause/snooze ("Completions disabled for N minutes"); per-app exclusion (`excludedApplications`, e.g. Finder); 30-day completion stats; emoji completion; "suggested fixes" (spelling/grammar via NSSpellChecker).

**Config surface (live `UserDefaults` keys observed):**
`CompletionManager_{acceptFullCompletionShortcut, acceptPartialCompletionShortcut, acceptNextWordOnly_includeTrailingSpaceIfAvailable, excludedApplications, maxCompletionLength=4, userPrompt}` · `ModelRepository_{selectedModel, statusItemVisible, shouldShowCompletedWordCountInMenuBar}` · `PersonalizationStrengthSlider` · `TextFieldContextCapture_pasteboardContextEnabled` · `TrainingDataCollector_enabled` · `EmojiCompletion_{preferredGender, preferredSkinTone, includeVanillaVariants}` · `io_replies_sender_{name,email}` · `ShortcutListener_forceEnableShortcut` · Sparkle `SU*`. Settings panes enumerated in §8.

**Overlay internals**: `InlineSuggestionsOverlayWindow` + `OverlayViewController` host `InlineSuggestionsListView` (row + badge + border views) over a `CompletionBackdropManager` backdrop (`SolidBackdropView`/`BlurredBackdropView`/glass effect) for legibility. `InlinePredictionDisableController` turns off macOS's own inline prediction while active.

**Network/endpoints**: model CDN `models.cotypist.app` (zstd GGUFs); `cotypist.app/{setPreference,launchCotypist/setOverride}` web-driven config via URL scheme; `cotypist.app/{compatibility,appHelp/textMetrics,help/privacy,pricing}`; RepliesSDK backend `replies.io` (protobuf — bundles `swift-protobuf`). Bundled deps of note: `MASShortcut` (configurable shortcuts), `LetsMove`, `CwlUtils`, `zstd`, `Sentry`.

**Feature-flag catalog (full product surface, observed):**
`featureConfigurableCompletionLength` · `featureMidLineCompletion` · `featureFullAutocorrect` · `featureEmojiCompletion` · `featureThesaurus{AutoMode,SelectionMode}` · `featureCustomInstructions{Global,PerApp}` · `featurePersonalization{Gentle,Balanced,Strong}` · `featurePasteboardContext` · `featurePreviousInputContext` · `featureCrossAppPreviousInputs` · `featureCustomModelOverride` · `feature{MidSize,Large}Models` · `featureUnlimitedCompletions` · `featurePerAppFontStyleOverrides` · `featureMultiDeviceSeats` · `featureCotypistLabsAccess`. (Subscription tiers gate model size + unlimited completions; we are not monetizing but the tiering informs the catalog structure.)

**Thresholds/quality**: `deepMatchThreshold`, `reuseThreshold` (completion caching/reuse), `meetsQualityThresholds`, field-`Size Thresholds` (don't suggest in tiny fields), `wordCountAboveLengthThreshold` (stats).

**What we adopt:** prompt-based personalization (global+per-app, 3 strength levels), two-tier configurable accept, word-capped length, pasteboard + previous-input context, selectable model catalog (base+instruct), backdrop overlay, disable-native-inline-prediction, pause/snooze, per-app overrides (incl. tab-key/smart-quotes/size-threshold/display), local encrypted stats/training data, quality/reuse thresholds.
**What we change:** Tauri updater instead of Sparkle; Rust instead of Swift; CGEventTap built by hand (no RepliesSDK); model fetch from HF or self-host TBD.
**Deferred features:** emoji completion, thesaurus, full autocorrect, cross-app previous inputs, web-driven config, domain/website overrides, subscription/seats.

---

## 14. Future sub-projects (out of scope, behind interfaces)

Validated in `2026-06-03-cross-platform-review.md`. Ordering reflects capability loss, not just porting effort: each step down loses a pillar of the macOS interaction model.

- **B. Windows** — `platform_windows`: UIA on a dedicated MTA worker thread + `WH_KEYBOARD_LL` accept + layered overlay (PMv2 DPI). Inference: Vulkan+CPU default, CUDA optional download. Strong tier = WPF/WinForms/Win32/native Qt; Electron/Chromium degrade to popup/hotkey.
- **C1. Linux X11 + Wayland(KDE/wlroots)** — `platform_linux`: `atspi` adapter + XTEST/wtype insert + override-redirect/layer-shell overlay + **dedicated-hotkey** accept (plain Tab can't be grabbed globally). AppImage distribution.
- **C2. Linux GNOME/Wayland + cross-platform IME path** — **separate architecture**: IBus **input-method-engine** backend with IME-native suggestion UI. GNOME/Wayland defeats overlay + key-intercept + front-app simultaneously, so the macOS model is *absent*, not degraded. Biggest single piece of Linux work.
- **D.** Cloud provider abstraction (behind `LocalModel`), browser extension, IDE plugins, remote compat registry, web-driven config, domain/website overrides.

**Cross-cutting (from review):** Tauri = tray + settings only; render overlays with **native** windows per OS (Tauri webview click-through is unsolved). Engine/inference crate stays OS-agnostic — only the llama.cpp build feature (`metal`/`vulkan`/`cuda`) + shipped runtime differ; build with `dynamic-backends` for one-binary GPU/CPU adaptation.
