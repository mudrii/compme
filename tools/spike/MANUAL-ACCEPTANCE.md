# A0 Manual Acceptance (human-run; needs Accessibility + Input Monitoring granted to the terminal)

> These probes block on run loops (`CFRunLoop::run_current`, `app.run()`) or a 1s polling loop,
> and need TCC grants + a GUI, so they cannot be auto-verified by a subagent — they are
> compile-gated only (all five compile under `cargo build --release`). Behaviour is verified
> here by a human. Grant **System Settings → Privacy & Security → Accessibility** AND
> **Input Monitoring** to the terminal that launches the probes, then relaunch it.
> Run each from `tools/spike/` (the model + `models/` paths are relative).

- [x] **P3 AX read** — `cargo run --release --bin p3_axread`; focus TextEdit, type "hello world".
      PASS: prints `caret=11 left_tail="hello world"`. Verified TextEdit, Safari address/search field, and Chrome textarea on 2026-06-04. For automation, `SPIKE_AX_PID=<pid>` targets one app's focused element.
- [x] **P4 caret** — `cargo run --release --bin p4_caret`; focus TextEdit, type/move caret.
      PASS: prints `tier=Exact|Derived rect=(x,y wxh)` that tracks the on-screen caret. Verified 2026-06-04: TextEdit, Safari, and Chrome each returned `Derived` caret rects; TextEdit movement changed from `caret=11 rect=(562,215 1x14)` to `caret=10 rect=(556,215 1x14)`. No obvious Retina offset observed in the checked windows.
- [x] **P5 tap** — `cargo run --release --bin p5_tap`; press Tab (the accept key wired via `spike::keys::should_swallow`, keycode 48) vs other keys; type in ANOTHER app.
      PASS: Tab swallowed, others pass, NO input lag in the other app. Verified 2026-06-04 with macOS `System Events`: TextEdit value stayed unchanged after Tab while the tap logged `keycode 48 (accept key) -> SWALLOWED`; keycode 11 passed and inserted `b`.
- [x] **P5b two-tap** — `cargo run --release --bin p5_twotap`; press Tab before and after toggling simulated suggestion visibility with F8.
      PASS: listen-only tap observes keys; F8 toggles `suggestion_visible`; Tab passes when false and is swallowed when true; other apps do not lag. Verified 2026-06-04 with macOS `System Events`: TextEdit changed from `twotap` to `twotap\t` after the false-state Tab, F8 toggled true, and the second Tab was swallowed. NOTE: this proves split observer/consumer behavior, but production still needs A1b create/enable/teardown lifecycle around a real suggestion.
- [x] **P6 overlay** — `cargo run --release --bin p6_overlay`.
      PASS 2026-06-05: screenshot `/tmp/complete-me-p6-redo-before.png` visually showed grey `ghost completion text` over a Chrome click target; CoreGraphics listed onscreen `p6_overlay` at layer `101`, bounds `240x30`, alpha `1` before and after the click; `System Events`/frontmost checks did not switch focus to the overlay; click-through was verified by clicking screen coordinate `{590,1185}` inside both overlay and Chrome button, changing Chrome title from `clicked-0` to `clicked-1`.
- [x] **P7 smoke** — `cargo run --release --bin p7_smoke`; within 3s focus a TextEdit doc with text.
      PASS: prints left context + completion AND grey ghost text appears at/near the caret. Verified 2026-06-04 with `SPIKE_AX_PID=<TextEdit pid>`: printed `left_tail: "Dear team, I wanted to "`, `caret: 23`, completion `"ask about the \"C\""`, and CoreGraphics listed an onscreen `p7_smoke` layer-101 overlay window at bounds `261x24` near the TextEdit caret. NOTE: this is historical instruct-model smoke evidence for read→infer→overlay wiring; the model-quality/default decision is now recorded in `tools/spike/FINDINGS.md` P2/P2b.

> Note on P5 accept key: the original probe draft used F8 (keycode 100). The TDD rewrite routes the
> swallow decision through the tested `spike::keys::should_swallow` seam, whose accept key is
> `KEYCODE_TAB` (48). So the probe swallows **Tab**, not F8 — press Tab to see `SWALLOWED`.
> In P5b, F8 is only a local probe toggle for simulated visibility; Tab remains the accept key.
