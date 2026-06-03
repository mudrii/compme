# A0 Manual Acceptance (human-run; needs Accessibility + Input Monitoring granted to the terminal)

> These probes block on run loops (`CFRunLoop::run_current`, `app.run()`) or a 1s polling loop,
> and need TCC grants + a GUI, so they cannot be auto-verified by a subagent — they are
> compile-gated only (all five compile under `cargo build --release`). Behaviour is verified
> here by a human. Grant **System Settings → Privacy & Security → Accessibility** AND
> **Input Monitoring** to the terminal that launches the probes, then relaunch it.
> Run each from `tools/spike/` (the model + `models/` paths are relative).

- [ ] **P3 AX read** — `cargo run --release --bin p3_axread`; focus TextEdit, type "hello world".
      PASS: prints `caret=11 left_tail="hello world"`. Try Safari address bar + a Chrome textarea; record which read.
- [ ] **P4 caret** — `cargo run --release --bin p4_caret`; focus TextEdit, type/move caret.
      PASS: prints `tier=Exact|Derived rect=(x,y wxh)` that tracks the on-screen caret. Record tier per app + any Retina offset.
- [ ] **P5 tap** — `cargo run --release --bin p5_tap`; press Tab (the accept key wired via `spike::keys::should_swallow`, keycode 48) vs other keys; type in ANOTHER app.
      PASS: Tab swallowed, others pass, NO input lag in the other app.
- [ ] **P6 overlay** — `cargo run --release --bin p6_overlay`.
      PASS: grey ghost text floats, does NOT steal focus, click-through.
- [ ] **P7 smoke** — `cargo run --release --bin p7_smoke`; within 3s focus a TextEdit doc with text.
      PASS: prints left context + completion AND grey ghost text appears at/near the caret.

> Note on P5 accept key: the original probe draft used F8 (keycode 100). The TDD rewrite routes the
> swallow decision through the tested `spike::keys::should_swallow` seam, whose accept key is
> `KEYCODE_TAB` (48). So the probe swallows **Tab**, not F8 — press Tab to see `SWALLOWED`.
