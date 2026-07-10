# Cross-Platform Review — Windows + Linux + Cross-Platform Inference

**Date:** 2026-06-03
**Status:** Validation review (feeds future Sub-projects B/C; revises the `PlatformAdapter` trait in the macOS spec)
**Companion:** `2026-06-03-engine-macos-mvp-design.md` (Sub-project A)
**Method:** 3 parallel deep-research agents against current docs/crates (Feb–Jun 2026): Windows UIA stack, Linux AT-SPI/X11/Wayland stack, cross-platform llama.cpp + Tauri shell. Sources inline.

> **Historical research status.** This review established the portability risks
> and strategy; it does not describe current product support. macOS is the only
> released usable adapter. Windows/Linux currently compile as fail-closed
> scaffolds, off-mac inference is CPU-only at the pinned runtime, and Vulkan,
> CUDA, real adapters, Wayland, packaging, and acceptance remain planned. The
> current execution source is the 2026-07-08 cross-platform implementation plan;
> the current status ledger is `docs/ROADMAP.md`.

**Current-design correction (2026-07-05):** The macOS accept path now uses the
`CarbonHotkey` capability (`RegisterEventHotKey`) for transient accept
interception. `CgEventTap` remains relevant to historical A0 probes and some
synthetic input examples, but it is not the current production accept-key
requirement.

---

## 0. Executive summary

Three load-bearing conclusions:

1. **Inference is cleanly cross-platform.** The engine crate stays OS-agnostic; only the llama.cpp build feature + shipped GPU runtime differ. macOS→Metal, Windows+Linux→**Vulkan default + CPU fallback** (one artifact, all GPU vendors), **CUDA optional separate download** (~373 MB runtime). CPU-only is viable for 0.5–1.5B Q4 (~20–50 tok/s). No redesign needed. (§3)

2. **The text-integration layer does NOT generalize from the macOS shape.** The current trait assumes (a) you can read caret geometry, (b) you can place a window there, (c) you can intercept the accept key, (d) you can inject text — as if uniform. On Windows these are reliable only for native toolkits; on Linux they fracture by display server **and** compositor. The trait must grow capability flags and per-method strategy enums, and callers must treat "readable but not placeable / not interceptable" as a **common** state. (§4)

3. **There are two fundamentally different integration architectures, not one with degraded modes.** The "**Accessibility + inject + native overlay**" path (the macOS design) works on macOS, Windows, Linux/X11, Linux/KDE+wlroots. But **GNOME/Wayland defeats overlay + key-interception + front-app + the Wayland IME protocol simultaneously** — there the only honest path is an **Input-Method Engine (IBus) backend** with IME-native suggestion UI. The product must plan for an IME backend as a first-class alternative, especially on the single most common modern Linux desktop. (§5, §6)

**Strategic ordering (unchanged, reinforced):** macOS → Windows → Linux/X11 → Linux/Wayland(KDE,wlroots) → Linux/GNOME-Wayland(IME). Each step down adds capability loss, not just porting effort.

---

## 1. The portability verdict per concern

| Concern | macOS | Windows | Linux X11 | Linux Wayland (KDE / wlroots) | Linux Wayland (GNOME) |
|---|---|---|---|---|---|
| Read text + context | AX | UIA TextPattern *(toolkit)* | AT-SPI *(app)* | AT-SPI *(app)* | AT-SPI *(app)* |
| Caret rect | AXBoundsForRange* | TextPattern2.GetCaretRange* | AT-SPI char-extents | AT-SPI char-extents | AT-SPI char-extents |
| **Place overlay at caret** | NSPanel ✓ | layered window ✓ | override-redirect ✓ | layer-shell ✓ | **✗ no layer-shell** |
| **Intercept/swallow accept key** | CarbonHotkey ✓ | WH_KEYBOARD_LL ✓† | XGrabKey (hotkey only)‡ | **✗ client can't** | **✗ client can't** |
| Inject/insert text | AX-set / CGEvent | SendInput / clipboard / Value | XTEST ✓ / EditableText | wtype/ydotool / EditableText | ydotool / EditableText |
| Front app id | NSWorkspace ✓ | GetForegroundWindow ✓ | _NET_ACTIVE_WINDOW ✓ | compositor IPC (best-effort) | **extension-only** |
| Disable native autocomplete | InlinePrediction off | no global API (detect TSF/IME, back off) | n/a | n/a | n/a |

\* caret often degenerate/`NoValue` → expand-to-character workaround. † blocked for elevated windows (UIPI) unless `uiAccess`. ‡ plain Tab cannot be grabbed globally without breaking the desktop; use a dedicated hotkey.

**The killer cell:** GNOME/Wayland is ✗ on overlay **and** key-interception **and** front-app — three pillars of the macOS interaction model gone at once.

---

## 2. Windows adapter — findings (UIA + Win32)

- **UIA via `windows` crate (~0.62)**, COM. **Must run all UIA calls on a dedicated MTA thread that owns no windows** — calling on the UI thread can deadlock. Adapter owns a UIA worker thread; trait methods do a channel round-trip (tens of ms, cross-process COM). Consider the `uiautomation` ergonomic wrapper crate over hand-rolled vtables.
- **Caret:** `TextPattern2.GetCaretRange` (check `isActive` BOOL!) → `GetBoundingRectangles`; degenerate selection range returns empty array → `ExpandToEnclosingUnit(Character)`; legacy `GetCaretPos` needs `AttachThreadInput`. **Electron/Chromium + Windows Terminal/console expose no usable caret.**
- **Focus/caret events** (`AddFocusChangedEventHandler`, `Text_TextSelectionChanged`) fire cross-process but are **slow and can freeze the whole desktop** with broad `TreeScope` (documented NVDA/PowerToys multi-second hangs). Narrow scope to focused element; subscribe caret separately from focus.
- **Secure field:** `UIA_IsPasswordPropertyId`. Also UAC **secure desktop** + **elevated windows** invisible to a normal-integrity client (UIPI) → ship `uiAccess="true"` (signed, Program Files) or mark elevated unsupported.
- **Insert:** `ValuePattern.SetValue` replaces whole value (single-line only) → unusable for inline. **SendInput** = most universal but **must release held modifiers + the swallowed Tab first** (#1 cause of "works in Notepad, breaks in VS Code/Office"). Clipboard+Ctrl+V for big text (settle delay 150–500 ms, longer for Chromium).
- **Key interception:** `SetWindowsHookEx(WH_KEYBOARD_LL)`, return non-zero to swallow Tab. No admin, not injected. **Callback must do ~zero work — Win10 1709+ silently unhooks if >1000 ms** (never run inference in the hook). Can't swallow keys for elevated windows unless `uiAccess`. Crate: hand-roll via `windows`, or `rdev`/`rdevin` (grab API "unstable").
- **Overlay:** `WS_EX_LAYERED|WS_EX_TRANSPARENT|WS_EX_NOACTIVATE|WS_EX_TOOLWINDOW` topmost. **PMv2 DPI awareness is load-bearing** — UIA caret rects are physical pixels; without PMv2 the overlay misaligns on scaled/multi-monitor.
- **Native autocomplete/IME:** no global off switch. Win11 inline suggestions ride TSF; during IME composition read/insert is unsafe → **detect and back off**.

**Windows tiers:** Full inline = Win32 edit/RichEdit, WinForms, WPF, native Qt/WinUI (a11y on). Popup-only = degenerate caret / Electron *with forced a11y*. Hotkey/blind = Electron/Chromium default (VS Code, Slack, Discord, Teams, browsers — **huge share**), Terminal/console. Unsupported = password/elevated/secure-desktop.

Sources: learn.microsoft.com UIA threading, GetCaretRange, ValuePattern, LowLevelKeyboardProc, extended-window-styles, hidpi; microsoft.github.io/windows-docs-rs; chromium accessibility overview; nvaccess/nvda#18239.

---

## 3. Cross-platform inference — findings (llama.cpp + `llama-cpp-2`)

- **Backends:** Metal=macOS only; **Vulkan = all 3 OSes, all GPU vendors** (NVIDIA/AMD/Intel) — best single cross-vendor backend for Win+Linux. CUDA/ROCm/SYCL build on **both** Win+Linux. **No DirectML** in llama.cpp (don't plan for it). On NVIDIA, Vulkan trails CUDA on *prompt processing* but is ~on par for *token generation* — and our hot path is short-prompt generation, so the gap barely hurts.
- **`llama-cpp-2` v0.1.146** features: `metal`, `vulkan`, `cuda`, `rocm`, `dynamic-backends`, `dynamic-link`, `sampler`, … (defaults don't include GPU). Vendored cmake build maps `vulkan→GGML_VULKAN=ON` etc. Build deps: CUDA Toolkit / Vulkan SDK (`glslc`).
- **Auto-detect + CPU fallback is built in** (`ggml_backend_load_all` + scoring; CPU universal fallback). Must call `LlamaBackend::init()` once or get "no backends loaded".
- **`dynamic-backends` feature = the distribution win:** `GGML_BACKEND_DL` + `GGML_CPU_ALL_VARIANTS` → one binary loads best GPU backend AND best CPU microarch (AVX2/AVX512/Zen4) at runtime. Avoids shipping CPU forks.
- **CUDA runtime ≈ 373 MB + driver coupling** vs macOS arm64 ~9 MB → **CUDA must be an optional separate download**, never the default.
- **CPU-only viable:** 1.5B Q4 ≈ 20–50 tok/s on a typical laptop; 0.5B faster. Fine for short ghost-text. Keep model warm, debounce, `Q4_0` for ARM speed.
- **Engine OS-agnostic confirmed:** identical ggml/llama API across backends; one `LlamaModel`+context behind a mutex/actor, predict on a bg thread — same pattern on all 3 OSes. **Only the cargo feature + shipped runtime differ.**

**Ship rule:** macOS→Metal · Windows+Linux default→Vulkan+CPU fallback (build with `dynamic-backends`) · NVIDIA→optional CUDA download.

Sources: ggml-org/llama.cpp build.md + releases; docs.rs llama-cpp-2 / llama-cpp-sys-2 build.rs; knightli.com benchmarks.

---

## 4. Trait impact — the validated `PlatformAdapter`

Both Windows and Linux agents independently demanded the same changes. Capabilities are **first-class and per-focus**, not edge cases.

```rust
trait PlatformAdapter: Send + Sync {
    fn environment(&self) -> Environment;          // OS + display_server + compositor (Linux) + session caveats
    fn subscribe_focus(&self, cb: FocusCallback);  // focus events (cheap)
    fn subscribe_caret(&self, cb: CaretCallback);  // caret events — SEPARATE (expensive on Win UIA / Linux D-Bus)
    fn front_app(&self) -> Option<AppId>;          // often None on Wayland
    fn capabilities(&self, f: &FieldHandle) -> Capabilities;
    fn read_context(&self, f: &FieldHandle) -> Result<TextContext>;
    fn caret_rect(&self, f: &FieldHandle) -> Option<ScreenRect>;
    fn insert(&self, f: &FieldHandle, text: &str, strat: InsertStrategy) -> Result<Inserted>;
}

struct Capabilities {
    readable_text: bool,
    readable_caret: bool,
    writable: bool,
    secure: bool,
    multiline: bool,
    toolkit: Toolkit,                  // generalizes is_electron: Cocoa/Win32/WPF/Qt/Gtk3/Gtk4/Electron/Java/Vte/Unknown
    insert_strategy: InsertStrategy,   // EditableTextApi | ValueSet | SyntheticKeys | Clipboard | ImeCommit | None
    accept_intercept: KeyInterceptMode,// CarbonHotkey | CgEventTap | LowLevelHook | XGrabKey | FocusScopedInhibit | ImeOwnsKey | HotkeyOnly | None
    overlay_at_caret: OverlayPlacement,// NativePanel | LayeredWindow | OverrideRedirect | LayerShell | ImeCandidate | None
    coords_global_screen: bool,        // can the caret rect be used for absolute positioning?
}
```

**Why each change (evidence):**

- **`subscribe_caret` split from `subscribe_focus`** — Windows `Text_TextSelectionChanged` is the desktop-freezing one; must be scoped/throttled independently. Linux caret events are D-Bus round-trips needing coalescing.
- **`insert_strategy` enum** — there is no uniform insert primitive. Windows: SendInput vs clipboard vs Value. Linux: EditableText (often absent) vs XTEST (X11 only) vs wtype/ydotool. Caller must know if insert is lossless or best-effort, and key-up of the accept key must be coordinated with SendInput (so key-interception is **not** cleanly separable from insert).
- **`accept_intercept` mode** — "press Tab to accept" is **not portable**. Possible: macOS/Windows/X11(dedicated hotkey). Impossible for a normal client: **Wayland**. Where impossible → `HotkeyOnly` or `ImeOwnsKey`.
- **`overlay_at_caret` ≠ `readable_caret`** — GNOME/Wayland can give the caret rect via AT-SPI but **cannot place a window there** (no layer-shell). Overlay placement is a separate capability.
- **`environment()` with display_server + compositor** — there is no single `LinuxAdapter`; it must detect `XDG_SESSION_TYPE` and the Wayland compositor (Mutter/KWin/wlroots/COSMIC) and advertise very different capabilities.
- **Threading is implicit but real** — Windows mandates a UIA MTA worker thread; Linux AT-SPI is async D-Bus (zbus/tokio); macOS AX off-main. Each adapter owns its own runtime; trait methods may block (document it) or become `async`.

This expanded trait should be adopted in the **macOS spec now** (macOS implements
the rich enum values: `CarbonHotkey`, `NativePanel`,
`EditableTextApi`/`SyntheticKeys`) so B/C slot in without reshaping the
contract.

---

## 5. The second architecture: Input-Method Engine (IME) backend

Where Accessibility+inject+overlay can't work (notably GNOME/Wayland, and as a Windows TSF alternative), the **sanctioned** channel that legitimately sees keystrokes, reads surrounding text, and commits text is the OS input-method framework.

| Platform | IME framework | Context in | Commit | Suggestion UI | Caveat |
|---|---|---|---|---|---|
| Linux/Wayland (KDE, wlroots) | Fcitx5 / IBus + `text-input-v3` | `set_surrounding_text` (≤4 KB, cursor+anchor) + `completion` content-hint | `zwp_input_method_v2` commit | `set_cursor_rectangle` popup | version mismatches (Chromium v1, Qt v3 only 6.7+) |
| **Linux/Wayland (GNOME)** | **IBus** (GNOME integrates IBus in the shell) | via IBus engine context | IBus commit | IBus candidate/preedit | GNOME does **not** implement the Wayland IM protocol Fcitx5 uses → **IBus is the only robust path** |
| Linux/X11 | IBus / XIM | engine context | commit | candidate window | works, but X11 path can also use accessibility+XTEST |
| Windows | TSF (Text Services Framework) text-input processor | TSF document mgr | TSF | TSF candidate UI | heavier than WH_KEYBOARD_LL; alternative, not required |

**Trade-offs of the IME path:** you live inside IME UX (preedit/candidate window, not free-floating ghost text); context capped (~4 KB, app must support surrounding-text); you inherit IME activation/switching UX. **Upside:** sidesteps both the "can't swallow Tab" and "can't inject/overlay" walls entirely, and is *less alien on Linux* where many users already route through an IME.

**Decision:** the IME backend is its own sub-project (or a major phase within Linux), distinct from the accessibility backend. Model it as an alternative `PlatformAdapter`-family backend selected by `environment()`.

Sources: fcitx-im.org Wayland; wayland.app text-input-unstable-v3 / keyboard-shortcuts-inhibit / xwayland-keyboard-grab; gitlab.gnome.org mutter#973 (layer-shell refusal); wlr-layer-shell protocol; odilia `atspi` crate.

---

## 6. Historical Tauri shell research — cross-platform caveats

**Status note (2026-06-08):** this section is historical cross-platform research. The active macOS plan has pivoted to a native Rust/AppKit shell with no Tauri dependency. Reuse these notes only if a later Windows/Linux settings/tray shell deliberately reintroduces Tauri; do not apply them to Sub-project A macOS execution.

- **Tray:** Win/macOS fine. Linux needs `libayatana-appindicator`; **GNOME needs the AppIndicator shell extension**; tray **breaks inside Flatpak**; works best from **AppImage** (embeds the lib). Linux icon must be PNG, Windows `.ico`.
- **Overlay = native, not Tauri webview.** Per-pixel click-through is unsolved in Tauri; `set_ignore_cursor_events` is buggy even on Windows; Linux transparency needs a compositor. Use Tauri for **tray + settings UI only**; render ghost text with native windows per OS (NSPanel / layered window / layer-shell / override-redirect) via `raw-window-handle`.
- **No-dock:** `set_activation_policy(Accessory)` macOS-only; Win/Linux just create no main window / `skip_taskbar(true)`.
- **Updater** (plugin ≥ v2.10.0): supports deb/rpm/AppImage/NSIS/MSI/.app. **Flatpak/snap update externally** (Flathub).
- **Packaging** via `tauri build`: msi/nsis (Win), app/dmg (mac), deb/rpm/appimage (Linux). **Flatpak = separate `flatpak-builder`**.
- **Global-shortcut plugin: X11 only; disabled on Wayland** (would segfault). Use **XWayland (`GDK_BACKEND=x11`)** fallback or the `org.freedesktop.portal.GlobalShortcuts` portal (not yet in Tauri). Reinforces: on Wayland the accept gesture goes through IME, not a global hotkey.

Sources: v2.tauri.app system-tray / updater / global-shortcut / distribute; tauri-apps/tauri#14234 (GNOME tray), #13070 (per-pixel click-through), #11461 (ignore-cursor bug), #3578 (Wayland global shortcut).

---

## 7. Realistic support matrix (the deliverable)

Tier = best achievable interaction. "Accept" = how the user commits a suggestion. "Overlay" = how suggestion is shown.

| Platform / env | Tier | Accept mechanism | Overlay | Notes |
|---|---|---|---|---|
| **macOS 14+** | **Full inline** | transient Carbon `RegisterEventHotKey` while a suggestion is armed | NSPanel | Reference platform (Sub-project A). **[CORR 06-12 — F1/D1 closed]** Production accept no longer uses a consuming CGEventTap or requires Input Monitoring; Accessibility remains required for AX read/write and CGEvent insertion. |
| **Windows — WPF/WinForms/Win32/native Qt** | **Full inline** | WH_KEYBOARD_LL | layered window (PMv2) | The strong Windows tier |
| **Windows — Electron/Chromium (forced a11y)** | **Popup** | WH_KEYBOARD_LL | layered window | Caret often whole-line; VS Code/Slack/Teams/browsers |
| **Windows — Electron default / Terminal / elevated** | **Hotkey / Unsupported** | dedicated hotkey | popup panel | a11y off or UIPI-blocked |
| **Linux X11 (any DE)** | **Full inline** | XGrabKey (dedicated hotkey, not plain Tab) | override-redirect + XShape | macOS-parity-ish; ship first on Linux |
| **Linux Wayland — KDE / wlroots** | **Inline (altered accept)** | focus-scoped inhibit / hotkey / IME | layer-shell | Overlay works; no global Tab swallow |
| **Linux Wayland — GNOME** | **IME-only / reduced** | IBus engine owns the key | IBus candidate UI | No overlay, no key-intercept, no front-app → IME backend |
| **Any — password / secure / elevated** | **Blocked** | — | — | Never read/insert |

Per-platform inference (orthogonal, all tiers): macOS Metal; Windows/Linux Vulkan+CPU; CUDA optional.

---

## 7b. Cotypist feature × platform parity matrix **[added 2026-06-09 — D16]**

Every cloned Cotypist feature (rows) against each target platform (cols). Legend: **✓** native parity · **◑** degraded/altered (works, reduced) · **⌨** hotkey/IME-only · **✗** not achievable on that platform · **n/a** not applicable. Payment/licensing/tiers/seats are out of scope everywhere (Project Scope). This matrix is the **source of truth for per-platform exit criteria** — a platform is "parity" only when every non-✗ cell has an acceptance gate passed.

| Cotypist feature | macOS | Windows (strong) | Win Electron | Linux X11 | Linux Wayland KDE/wlroots | Linux Wayland GNOME |
|---|---|---|---|---|---|---|
| Read text + caret context | ✓ AX | ✓ UIA | ◑ forced-a11y | ✓ AT-SPI | ✓ AT-SPI | ✓ AT-SPI |
| Inline ghost overlay at caret | ✓ NSPanel | ✓ layered | ◑ whole-line caret | ✓ override-redirect | ✓ layer-shell | ✗ (IME candidate UI only) |
| Accept next-word (Tab) | ✓ | ✓ WH_KEYBOARD_LL | ✓ | ⌨ dedicated hotkey¹ | ◑ inhibit/hotkey | ⌨ IBus owns key |
| Accept full (key-above-Tab) | ✓ | ✓ | ✓ | ⌨ hotkey | ◑ | ⌨ IME |
| Esc dismiss + suppress (D11) | ✓ Carbon hotkey, A1b/self-test validated | ✓ | ✓ | ✓ | ✓ | ◑ IME |
| Option/Alt+Tab literal-Tab bypass (D11) | ✓ Option+Tab bypass, A1b/self-test validated | ✓ | ✓ | ✓ | ✓ | ◑ |
| Per-app Tab disable + per-app/domain overrides | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Secure-field / password block | ✓ SecureInput | ✓ UIPI/secure | ✓ | ✓ | ✓ | ✓ |
| Prompt-based personalization (global+per-app+per-domain, 6-stop full-reach) | ✓ portable | ✓ | ✓ | ✓ | ✓ | ✓ |
| Encrypted local memory (rusqlite + OS keystore) | ✓ Keychain | ✓ DPAPI/Credential | ✓ | ◑ Secret Service | ◑ Secret Service | ◑ Secret Service |
| Pasteboard / previous-input context | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Cross-app previous inputs | ✓ | ✓ | ✓ | ✓ | ◑ front-app best-effort | ✗ front-app extension-only |
| Screen-recording / OCR context (opt-in) | ✓ ScreenCaptureKit+Vision | ✓ Graphics Capture+OCR | ✓ | ◑ X11 capture+OCR | ◑ portal capture | ◑ portal capture |
| Native inline-prediction suppression | ◑ owned-only | ◑ detect/back-off | n/a | n/a | n/a | n/a |
| Model catalog + download (hardware-gated, no price gate) | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Pause/snooze, stats, launch-at-login | ✓ SMAppService | ✓ Run-key/Task | ✓ | ✓ autostart | ✓ autostart | ✓ autostart |
| Local inference (llama.cpp) | ✓ Metal | ✓ Vulkan/CUDA | ✓ | ✓ Vulkan/CPU | ✓ | ✓ |

¹ Plain Tab cannot be grabbed globally on X11/Wayland without breaking the desktop — Linux accept defaults to a dedicated hotkey, not bare Tab (documented divergence, not a bug). **GNOME/Wayland is the hard floor** (overlay ✗, key-intercept ✗, front-app ✗ → IME-backend reduced mode). Each ◑/⌨ cell needs an explicit, documented UX so users know what they get per platform.

---

## 8. Recommendations & sub-project shape

- **Adopt the expanded trait now** (§4) in the macOS spec — macOS fills the rich enums; B/C don't reshape the contract.
- **Sub-project B (Windows):** UIA MTA-worker adapter + WH_KEYBOARD_LL + layered overlay (PMv2). Default Vulkan+CPU build, optional CUDA. Strong tier = native toolkits; accept Electron-as-popup/hotkey.
- **Sub-project C1 (Linux/X11 + KDE/wlroots Wayland):** `atspi` adapter + XTEST/wtype + override-redirect/layer-shell overlay + dedicated-hotkey accept. AppImage distribution.
- **Sub-project C2 (Linux/GNOME-Wayland, and cross-platform IME path):** **IBus input-method engine backend** with IME-native suggestion UI. Distinct architecture; biggest single piece of Linux work.
- **Shell:** macOS Sub-project A uses the native AppKit shell. For future Windows/Linux, Tauri remains only a candidate for tray/settings UI; **native overlays** stay mandatory everywhere. Document GNOME tray extension + XWayland fallback if Tauri is reintroduced.
- **Engine/inference:** no change — OS-agnostic crate, per-OS build feature, `dynamic-backends` for one-binary GPU/CPU adaptation.

**One-sentence strategy:** build the Accessibility+inject+native-overlay path down through macOS → Windows → Linux/X11 → Linux/KDE+wlroots, and treat GNOME/Wayland as an IME-engine sub-project — because there the macOS interaction model is not degraded, it is absent.
