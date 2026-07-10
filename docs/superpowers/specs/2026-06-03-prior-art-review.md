# Prior-Art Review — OSS Clones, Espanso, and the Landscape

**Date:** 2026-06-03
**Status:** Validation review (real-code evidence; revises macOS spec design + risks)
**Companions:** `2026-06-03-engine-macos-mvp-design.md`, `2026-06-03-cross-platform-review.md`
**Method:** 3 parallel agents read real production code (cloned + GitHub) and the product landscape: (1) macOS clones KeyType/Cotabby/GhostType, (2) Espanso + Rust input/AX crate maturity, (3) commercial+OSS landscape and failure modes.

> **Historical research status.** This evidence informed the implementation but
> is not a current support or pending-work statement. Production accept uses
> transient Carbon hotkeys, macOS has shipped through v0.1.4, and the remaining
> work is tracked only in `docs/ROADMAP.md` and `docs/ACCEPTANCE.md`.

**Current-design correction (2026-07-05):** CGEventTap/Input Monitoring notes
below are historical A0/prior-art evidence, not the current Compme accept-key
requirement. Production accept uses transient Carbon hotkeys; Accessibility is
still required, while Input Monitoring is only covered by historical spike probes
and the revoked-permission acceptance spot-check.

---

## 0. Headline

**All 7 macOS technical assumptions are confirmed by shipping code** — but real code reveals ~17 gotchas docs never mention, and several **change the design**: the two-tap CGEventTap, the 5-tier caret ladder + separate web path, the 50 ms AX timeout, hybrid-model KV-cache hazards, token healing, and App-Sandbox-off (no Mac App Store). The landscape confirms **accessibility-driven all-apps is the proven strategy** (Cotypist, KeyType, Lightkey, Grammarly) but only with **graceful per-app degradation**, **sub-500 ms latency**, and a **permission-lifecycle recovery UX** as first-class work. Espanso proves the cross-platform trait shape is right — and proves "read context everywhere" is a separate, 3-stack problem the best expander never even attempted.

---

## 1. Reference implementations (read these before coding)

| Repo | Lang | License | Status | Value |
|---|---|---|---|---|
| **KeyType** (johnbean393) | Swift, 9 SwiftPM pkgs | **MIT** | very active, ADRs through 2026-06 | **Gold standard.** Its `docs/05-decisions.md` ADR log is a postmortem of every gotcha. Read freely (MIT). |
| **Cotabby** (FuJacob) | Swift + in-proc C++ | **AGPL-3.0** | production, external contributors | Robustness reference (two-tap tap, 50 ms AX timeout, safety gates). **Copyleft — read/learn, don't copy code.** |
| **GhostType** (mk668a) | Swift, ~14 files | PolyForm Noncommercial | minimal v0.1 | Simplest viable version; OpenAI-compatible server backend (no bundled model). |
| **Espanso** | Rust, ~15 crates | GPL-3.0 | 13.9k★, v2.3.0 (Oct 2025) | Cross-platform input layer in Rust. `trait Source` + `trait Injector` per-OS — validates `PlatformAdapter`. **Read/learn (GPL).** |
| **odilia** | Rust | — | Beta ("not production ready") | Proof the `atspi` read path works in Rust; not yet a foundation. |

Most valuable single artifact: **KeyType `AXCaretGeometryResolver.swift`** (the caret ladder) + its ADR log.

---

## 2. The gotcha catalog (real code, not docs)

Grouped by subsystem. Each is a concrete design requirement.

### Caret geometry — it's a 5-tier ladder with quality tiers
1. **`exact`** — `kAXBoundsForRangeParameterizedAttribute` with a **zero-length** range at the caret. *Contradicts the "always fails" claim*: works in many native apps — **try it first**. Guard with `!rect.isEmpty` AND reject container-sized rects (Electron returns the whole composer rect).
2. **`exact` (web)** — Chromium/WebKit: `kAXSelectedTextRangeAttribute` is unreliable → use `AXSelectedTextMarkerRange` → `AXBoundsForTextMarkerRange` (opaque markers passed back to AX). **A separate code path, not NSRange.**
3. **`derived`** — previous-character fallback: query `NSRange(location-1, 1)`, use `cocoaRect.maxX`. (This is "the workaround" — it's tier 3, not the whole story.)
4. **`derived`** — child `AXStaticText` run interpolation (Google-Docs-style).
5. **`estimated`** — measure line-prefix width with `NSFont` metrics, simulate soft-wrap. Last resort.

Plus: **Retina pixel-vs-point is a second independent bug** — AX text rects sometimes come back in physical pixels; validate against the element's `AXFrame` anchor, divide by `backingScaleFactor` per-display if mismatched. Negative-origin external monitors break naïve division.

### CGEventTap — the two-tap design (correctness, not optimization)
- A single always-on **active** `.defaultTap` whose callback hops to a slow actor **stalls global keystrokes in unrelated apps** (Cotabby #328: DaVinci spacebar froze).
- **Fix:** permanent **`.listenOnly`** observer tap (can't stall input) + a narrow **`.defaultTap`** consuming tap **installed only while a suggestion is visible**, torn down otherwise.
- Re-enable on `.tapDisabledByTimeout` / `.tapDisabledByUserInput` inside the callback. Defer mach-port invalidation ~50 ms or the **last word's synthetic insert is lost** (Cotabby PR #385).
- **Tag your own synthetic events** (`CGEventSource.userData = marker`) and skip them, or your paste/keystrokes re-enter your tap → dismiss/double-insert.

### Accessibility reads
- **Set `AXUIElementSetMessagingTimeout(systemWide, 0.05)`** — default cross-process AX timeout is ~6 s; a wedged target app beachballs the user's typing. *Single most important AX reliability knob.*
- **Reading AX itself perturbs some apps** (KeyType #20: Calendar glitches; System Settings scroll-into-view). Use a **non-invasive** strategy (AXValue + AXSelectedTextRange + AXFrame estimate) for native single-line fields; full resolver only for web + native multiline. Gate reads behind a text-eligibility check (never touch sidebar rows/buttons).
- **Confine ALL macOS AX calls to one dedicated background thread** (production screen-reader lesson; AXSwift was ripped out for stability). Off the main thread (NSOpenPanel deadlocks if AX reads fire on main while a panel takes focus).
- **Accessory-app focus attribution**: Raycast/Spotlight/Alfred show non-activating panels but keep the *previous* app as `frontmostApplication` — resolve the field owner from the AX element's **pid**, not `NSWorkspace.frontmostApplication`.
- **Chromium/Electron AX is lazy** — set `AXManualAccessibility` on the **browser process** element (not renderers) to wake the web tree; prefer it over `AXEnhancedUserInterface` (latter glitches window managers).
- **AX value-changed notifications lag keystrokes** tens-to-hundreds of ms → front-run dismissal from the key tap (`hasPrefix` check), redraw shrinking remainder eagerly on accept.

### Insertion
- Per-app strategy planner is unavoidable: `pasteboardPaste` / `pasteAndMatchStyle` / `characterInjection` / `chunkedStringInjection` / `firstWordOnly`, with flags `useNonBreakingSpaceWorkaround`, `backspaceAfterPaste`, `restorePasteboard`. (Google Docs needs paste-match-style + backspace; WeChat needs chunked Unicode inject; clipboard restore needs 120–500 ms delay.)
- Inject **Unicode by codepoint** (`CGEventKeyboardSetUnicodeString`), not keycodes, for text — but it **truncates ~20 chars** (Espanso), so chunk long strings. Keycodes only for shortcut chords.

### Model / inference (KeyType ADRs — the deepest findings)
- **KV-cache reuse is unsafe on hybrid/recurrent models** (Qwen3.5 = attention + GatedDeltaNet/SSM): `llama_memory_seq_rm` rollback fails, `llama_memory_seq_cp` **aborts**, and `llama_model_is_recurrent` returns **false** despite recurrent buffers. **Only pure-append KV reuse is safe**; any divergence → `llama_memory_clear` + full re-decode. Use `llama_state_seq_get_data`/`set_data` snapshot/restore for branches.
- **ggml-Metal aborts on process exit** unless you free model/context via an explicit `shutdown()` before teardown (guard against double-free).
- **Token healing for mid-word completions** (the worst case): back up to the last whitespace, force the typed bytes as a required prefix (byte-admissibility mask **over the full vocab**, not post-top-k), strip the re-emitted stem.
- **Suffix-overlap guard for mid-line FIM** — small models regurgitate `afterCursor`; compare on alphanumerics, truncate at overlap.
- **Trim trailing whitespace from the prefix** before prompting — the just-typed space makes small base models wander + double-space.
- **Constrained generation + disk-cached per-model token profile** (scan vocab once; FNV-1a fingerprint of path+size+vocab). Serialize all llama calls (context not thread-safe) behind an actor/lock.

### IME / input source
- Suspend auto-trigger while a non-ASCII IME (JP/CN/KR) is composing (watch `kTISNotifySelectedKeyboardInputSourceChanged`); only trigger for ASCII-capable layouts.

### Distribution / permissions (category's #1 support burden)
- **Historical prior-art risk, not current Compme packaging:** apps that load an
  external dynamic llama framework may need App Sandbox off plus
  `com.apple.security.cs.disable-library-validation`. Compme instead statically
  links llama into its single binary; v0.1.4 shipped Developer-ID signed/notarized
  with hardened runtime and no entitlements file. Its cross-app Accessibility
  design still does not fit a conventional Mac App Store sandbox. The current
  release publishes the `.app` zip/manifest, undrafts it, then finalizes the
  Homebrew cask.
- **TCC keys on cert+bundle-id** — a new signing cert under the same bundle id causes an **infinite "grant Accessibility" loop**. Need a **stable signing identity** + a `tccutil reset` recovery path + re-grant detection after OS updates.
- Historical CGEventTap probes needed **both** Accessibility + Input Monitoring.
  Current production accept does not require Input Monitoring; **Secure Input**
  (triggered by background password managers) can still get stuck globally and
  suppresses completion/accept behavior.

---

## 3. Crate maturity — what to depend on

| Crate | Latest | Use | Verdict |
|---|---|---|---|
| `llama-cpp-2` | 0.1.146 | inference (C API surface only) | **Adopt.** Build llama.cpp with `metal`/`vulkan`. Serialize calls. |
| `objc2` + `objc2-app-kit` + `objc2-application-services` | current | NSPanel, AppKit, AX | **Adopt.** Maintained, exact APIs. |
| `accessibility-sys` / `accessibility` | 0.2.0 (2025-03) | raw AX C API | **Adopt** (thin; wrap yourself). |
| `axuielement` | 0.6.x | safe macOS AX read (caret bounds, AXStringForRange, AXTextMarker) | **Evaluate/adopt** for the read path. |
| `core-graphics` | current | CGEventTap, CGEvent | **Adopt** for the two-tap + injection. |
| `atspi` (odilia) | 0.30.0 | Linux AT-SPI read | **Adopt** (Linux). 8.4M downloads, healthy. |
| `enigo` | 0.6.1 | cross-platform inject (3 OS) | OK for inject; Wayland experimental. |
| `rdev` | 0.5.3 (**2023**) | global capture | **Avoid for capture path** — stale, "pet project." |
| `rdevin` | 0.1.0 | rdev fork | **Avoid** — v0.1, grab disabled on Linux. |
| `global-hotkey` (tauri) | 0.8.0 | registered hotkeys | Hotkeys only; **X11-only on Linux**, Wayland unmerged. |

**Signal:** Espanso, KeyType, Cotabby all **wrote their own native capture/inject layer** rather than depend on rdev. Do the same via `objc2`/`core-graphics`/`windows`/native FFI. Use `enigo` only as an inject shortcut on the easy platforms.

---

## 4. Assumption verdicts

| # | Assumption | Verdict |
|---|---|---|
| 1 | Caret via `kAXBoundsForRange` + collapsed-range workaround | **CONFIRMS + major nuance** — 5-tier ladder; collapsed *works* in native; web needs AXTextMarker; Retina scaling; reject container rects |
| 2 | Focus/text via AXObserver | **CONFIRMS** — + 2 Hz safety poll for under-reporting apps; ~20 ms debounce |
| 3 | Historical Tab accept via CGEventTap + Input Monitoring | **CONFIRMS + critical nuance for the A0 probe** — must be two-tap (listen-only + on-demand consuming); current production accept moved to transient Carbon hotkeys |
| 4 | Non-activating NSPanel overlay | **CONFIRMS exactly** — + need capsule-below-caret mode for mid-line; caret-height font; defensive color |
| 5 | AX-set / CGEvent / clipboard insertion | **CONFIRMS all three** — per-app planner; tag synthetic events; Unicode codepoint inject |
| 6 | llama.cpp + Metal, small GGUF first-run | **CONFIRMS exactly** — + hybrid-model KV hazards, exit-abort, token healing |
| 7 | Secure-field block via AXSecureTextField subrole | **CONFIRMS + broaden** — match role+roleDescription+title+placeholder (CVV/OTP/etc.), over-suppress |
| — | (Espanso) trait-per-platform shape | **CONFIRMS** `PlatformAdapter` |
| — | (Espanso) hooks swallow keys | **CONTRADICTS** — Espanso is listen-only + backspace-correct; swallowing is a deliberate harder choice (we need it for Tab) |
| — | "read context everywhere" is easy | **CONTRADICTS** — Espanso never reads caret/text; it's a separate 3-stack problem (macOS axuielement / Linux atspi / Win UIA), no unified crate |

---

## 5. Landscape & strategy

**Who ships "everywhere" and how:** accessibility-API + overlay is the proven path — Cotypist, KeyType, **Lightkey** (Windows, pre-LLM, years-long existence proof), Grammarly. Grammarly's architecture lesson: **thin platform-specific text-access/window layer, shared logic**; overlay is click-through with event-proxying; falls back to **polling the field ~1 s** because no universal change signal. Nobody gets pixel-perfect alignment always.

**Failed strategies:** DOM injection (editors *actively block* Grammarly — corrupts text); pure keystroke injection (dies in Electron/terminals, reads as keylogger); standalone desktop IME (SwiftKey never shipped one — OS vendors absorb prediction).

**Top failure modes / uninstall drivers:**
- **Permission-reset dance** — silent stop after every OS point-release / reinstall / cert change (category's #1 complaint).
- **Electron/TCC ambiguity** — "which sub-process gets the permission? no one knows." Grammarly drops Electron ≤11 + Mac App Store apps.
- **Latency** — autocomplete must respond **<500 ms p95** or "feels laggy, reduces acceptance." Our <150 ms target is well inside.
- **Suggestions-as-noise** — acceptance is **trust-compounding** (66k-interaction study: prior per-user acceptance dominates); a bad first impression is sticky → conservative triggering (fire after sentence-end/newline) beats always-on.
- **Custom-render apps can't be read** — inherent accessibility-tree gap, not a bug.
- **Sherlock risk** — Apple Intelligence Writing Tools is the existential threat.

**Strategic lessons (adopt):**
- Accessibility-driven all-apps is right — but **design graceful per-app degradation + a self-diagnosing compatibility list from day one**.
- **Narrow deliberately**: cede code/terminal to Copilot (Cotypist does). Own non-code writing. Blunts Sherlock.
- **On-device/private** is the clearest trust differentiator — lead with it.
- **Augment-don't-replace + partial accept (Tab = next word)** fights the noise failure mode.
- **Per-app style + train-on-my-writing** is the top *unmet* user ask → credible paid tier (matches Cotypist's per-app custom instructions).
- **Protect first-run** acceptance; build permission-recovery UX.

---

## 6. Net effect on the plan

- **No assumption was wrong.** The architecture stands. The corrections are depth, not direction.
- **Design-changing items folded into the macOS spec:** historical two-tap
  CGEventTap evidence (later superseded for production accept by transient Carbon
  hotkeys), 5-tier caret ladder + AXTextMarker web path + Retina conversion, 50
  ms AX timeout, synthetic-event tagging, AX-on-one-thread, accessory-app pid
  attribution, hybrid-model KV-cache rules + ggml shutdown + token healing +
  trailing-whitespace trim, App-Sandbox-off / no-MAS / stable-cert + TCC-reset
  UX, Secure-Input handling, IME-composition suspend.
- **A0 spike must now also prove:** the two-tap stall-free design, the caret ladder in a native app + a Chromium app, and warm inference with the chosen model's KV-reuse rules.
- **Read these before coding:** KeyType `AXCaretGeometryResolver.swift` + ADR log (MIT); Cotabby `AXHelper.swift` + `InputMonitor.swift` (AGPL — learn, don't copy); Espanso `espanso-detect`/`espanso-inject` (GPL — learn).
