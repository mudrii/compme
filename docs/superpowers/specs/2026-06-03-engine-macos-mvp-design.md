# Design — Sub-project A: Engine + macOS MVP

**Date:** 2026-06-03
**Status:** Approved (brainstorming → ready for implementation plan)
**Scope:** First sub-project of a long-term cross-platform predictive-writing product (Cotypist-like). This spec covers **only** the OS-agnostic engine + the macOS adapter + the app shell. Windows/Linux adapters and ecosystem integrations are separate, later specs that plug behind interfaces defined here.

---

## 1. Context & motivation

The product is a local, privacy-first predictive-writing assistant: it reads the editable text around the caret in other apps, predicts the next word/phrase with a local model, shows it as inline grey "ghost text", and lets the user accept incrementally with Tab.

This was validated against the real shipping Cotypist (`/Applications/Cotypist.app`):

| Aspect | Real Cotypist (binary dissection) | Decision for this product |
|---|---|---|
| Inference | `libllama` + `libggml-metal` (llama.cpp, Metal) | Same: llama.cpp via `llama-cpp-2`, Metal backend |
| Storage | `GRDB.framework` (Swift SQLite) | `rusqlite` + FTS5 |
| App shape | Menu-bar agent, `LSUIElement = true`, no dock | Tauri v2 tray app, hidden settings window |
| Model | Not bundled; downloaded first-run | Same: download on first run |
| Text access | macOS Accessibility (AX), min macOS 14 | `objc2` / `accessibility-sys` AX bindings |
| Engine | Custom `RepliesSDK` (prompt/sampling/acceptance) | Our `core` + `ranker` crates |
| Plumbing | Sparkle (update), Sentry (crash) | Sparkle for update; crash reporting optional |
| Language | Swift native | **Rust** (chosen for cross-platform reuse) |

The original research-style plan that motivated this work proposed a 3-platform / 8-crate / 5-person / 4-phase effort with fabricated citations. It had the right **tech bets** but ~10× the right **scale** and understated the Rust FFI cost. This spec re-scopes to one solo-buildable sub-project.

### Non-goals (this spec)
- Windows (UI Automation) and Linux (AT-SPI) adapters — later specs, behind `PlatformAdapter`.
- Swappable model providers (Ollama/OpenAI) — later spec, behind the `LocalModel` trait seam.
- Browser extension / IDE plugins / remote compat registry.
- On-device fine-tuning (personalization is memory+ranking only, never fine-tune).

---

## 2. Architecture

Single process. Tauri v2 supplies the tray menu, lifecycle, and a hidden settings webview. The Rust core (AX taps, overlay, inference, learning) runs in Tauri's backend, with prediction work on a dedicated thread so a settings-UI panic cannot stall predictions.

```
┌─ Tauri v2 tray app (one process, LSUIElement) ───────────┐
│  tray menu · hidden settings webview · lifecycle         │
│                                                           │
│  ┌── core ──────────┐   suggestion state machine,         │
│  │ debounce, sched, │   acceptance (word/phrase), policy  │
│  │ accept logic     │                                     │
│  └───────┬──────────┘                                     │
│   ┌──────┴───────┬───────────┬───────────────┐            │
│  context       ranker     model_client    personalization │
│  (TextCtx,    (score,     (llama-cpp-2,    (rusqlite+FTS5, │
│   Caret,       rep.pen,    GGUF, Metal)     phrase memory, │
│   Selection)   pers.score)                  redaction)     │
│                                                           │
│  ┌── platform (trait PlatformAdapter) ──┐                 │
│  │  platform_macos: objc2 / AX /        │                 │
│  │  CGEvent / NSPanel overlay           │                 │
│  └──────────────────────────────────────┘                │
└───────────────────────────────────────────────────────────┘
```

### Workspace layout
```
crates/core             # state machine, scheduling, debounce, acceptance, app policy
crates/context          # TextContext, Selection, Caret — shared data models
crates/ranker           # candidate scoring, repetition penalty, personalization score
crates/model_client     # LocalModel trait + llama.cpp (llama-cpp-2) impl
crates/personalization  # rusqlite + FTS5 store, phrase memory, redaction
crates/platform         # PlatformAdapter trait + shared types (the cross-platform contract)
crates/platform_macos   # AX read, CGEvent insert, NSPanel overlay (objc2 / accessibility-sys)
apps/app                # Tauri v2 tray app wiring it all together + settings webview
tools/spike             # throwaway A0 proof-of-concept (deleted after A0)
```

---

## 3. The cross-platform contract

`PlatformAdapter` is the central bet — get it right with one real adapter before B/C commit to it. Capability-first so `core` never special-cases apps; capabilities drive UX mode selection.

```rust
pub trait PlatformAdapter: Send + Sync {
    /// Register for focus-change events on editable fields.
    fn subscribe_focus(&self, cb: FocusCallback);

    /// What can we do with this field right now?
    fn capabilities(&self, f: &FieldHandle) -> Capabilities;

    /// Text to the left and right of caret, plus current selection.
    fn read_context(&self, f: &FieldHandle) -> Result<TextContext>;

    /// Caret rectangle in screen coordinates. None => no inline mode.
    fn caret_rect(&self, f: &FieldHandle) -> Option<ScreenRect>;

    /// Insert/accept text. Strategy chosen by InsertMode + capabilities.
    fn insert(&self, f: &FieldHandle, text: &str, mode: InsertMode) -> Result<()>;
}

pub struct Capabilities {
    pub readable_text: bool,
    pub readable_caret: bool,
    pub writable: bool,
    pub secure: bool,       // password / secure field — HARD block
    pub multiline: bool,
}
```

### UX mode derivation (in `core`, from `Capabilities`)
| readable_text | readable_caret | writable | secure | → Mode |
|---|---|---|---|---|
| ✓ | ✓ | ✓ | ✗ | **Inline ghost text** (premium) |
| ✓ | ✗ | ✓ | ✗ | **Near-caret popup** |
| ✓ | – | ✓ | ✗ | **Hotkey completion** (if continuous unsafe) |
| – | – | – | – | **Unsupported** (diagnostic tooltip) |
| any | any | any | ✓ | **Blocked** (always) |

---

## 4. Event flow

1. Focus changes to an editable field → `subscribe_focus` callback fires.
2. Adapter gathers `capabilities` + `read_context` + `caret_rect` + app id.
3. `core` checks policy (per-app toggle, secure block) and selects UX mode.
4. After debounce, `core` schedules a completion request.
5. `model_client` returns 2–5 short candidates (3–12 tokens, live mode) from local GGUF.
6. `ranker` scores candidates: LM score + repetition penalty + personalization similarity + sensitive-context penalty.
7. Overlay renders top candidate as ghost text at `caret_rect` (or popup fallback).
8. **Tab** accepts one word/chunk; **Shift-Tab** cycles alternatives; **Esc** dismisses.
9. `personalization` records accepted/rejected outcomes locally (after redaction).

---

## 5. Inference

- Backend: llama.cpp via `llama-cpp-2`, Metal enabled. Bundled build, pinned versions.
- Model: start **Qwen2.5-0.5B-Instruct** GGUF, Q4_K_M (try 1.5B if latency budget allows). Downloaded first-run, cached in app support dir. Benchmarked in A0.
- Live mode: 2–5 candidates, 3–12 tokens each. Stream internally; render only past a min-confidence threshold.
- Prompt features: left-context window, optional right-context, app-category hint (email/chat/doc/terminal), user style hints from `personalization`.
- `LocalModel` is a trait so future providers are an additive spec, not a refactor.

---

## 6. Personalization (memory + ranking, never fine-tune)

- `rusqlite` + FTS5. Stores: accepted completions, rejected completions, accepted-phrase memory, per-app/per-domain style profiles, recurring entities/contacts/project terms.
- **Opt-in and inspectable.** "Forget learned data" control. Per-app deny list + per-window incognito.
- **Redaction before any persistence:** emails, card-like numbers, passwords, tokens/secrets stripped.
- Feeds `ranker` as a personalization-similarity score; no model weights touched.

---

## 7. Privacy & safety (first-class)

- Never read or store secure fields (hard block at capability layer).
- No raw-text logging by default; diagnostics are text-redacted unless user explicitly exports verbose logs.
- All inference local by default (only backend in this spec).
- Visible "pause everywhere" toggle; per-app deny list; incognito per window.
- Clear retention policy + "forget learned data".

---

## 8. Phasing (Sub-project A)

| Phase | Weeks | Deliverable | Exit criterion |
|---|---|---|---|
| **A0 spike** (`tools/spike`, throwaway) | 1–2 | AX caret-rect read + transparent overlay + llama round-trip in TextEdit | Caret geometry works in ≥1 real app; round-trip latency measured |
| **A1 core loop** | 3–4 | `PlatformAdapter` trait + macOS adapter + single completion + ghost overlay + Tab-accept + secure-field block | Type in Notes/Mail → inline suggestion → Tab inserts, no lag; passwords blocked |
| **A2 fat features** | 3–4 | personalization (SQLite+FTS5+redaction), multi-candidate gen, ranker, Shift-Tab cycle | Suggestions adapt to accepted phrases; cycling works |
| **A3 settings + ship** | 2–3 | Tauri settings window, per-app toggles, diagnostics page, packaging + notarization + Sparkle | Installable signed `.app`, configurable, self-diagnosing |

Estimated ~9–13 weeks solo to a shippable macOS app.

---

## 9. Risks

| Risk | Severity | Mitigation |
|---|---|---|
| Rust AX FFI friction (`objc2` / `accessibility-sys`) | High | A0 spike de-risks before committing to A1 |
| `caret_rect` unavailable in many apps | High | Capability tier → automatic popup fallback (designed-in) |
| llama.cpp + Tauri build/link complexity | Medium | `llama-cpp-2` bundled build; pin versions; verify in A0 |
| Single-process: settings-UI panic stalls predictions | Medium | Prediction work on dedicated thread; `catch_unwind` around UI |
| Notarization / AX-permission onboarding UX | Medium | A3 handles; detect permission on launch, guide to System Settings |

---

## 10. Success metrics

- First-suggestion perceived latency < 100–150 ms on Apple Silicon.
- Insertion failure < 1% in supported apps.
- < 5% sessions with noticeable typing lag.
- Clear support tier for top ~20 macOS apps.
- Telemetry (local): acceptance rate, dismiss rate, wrong-insertion incidents, overlay misalignment, CPU/RAM, token throughput.

---

## 11. Future sub-projects (out of scope here, behind interfaces)

- **B.** `platform_windows` (UI Automation) behind `PlatformAdapter`.
- **C.** `platform_linux` (AT-SPI, X11/Wayland split) + popup fallback.
- **D.** Provider abstraction (Ollama/OpenAI), browser extension, IDE plugins, remote compat registry.
