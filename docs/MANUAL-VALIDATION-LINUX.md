# compme — Linux Manual Validation Checklist

> Live, human-at-the-keyboard checklist for the experimental Linux surfaces
> (`docs/ROADMAP.md` Tier 1, Linux Phase 2). Evidence recorded here is Linux
> pre-release confidence for the wired adapter surfaces; it does not gate the
> macOS release ledger in [`ACCEPTANCE.md`](ACCEPTANCE.md).
>
> **Host reality this checklist is written for:** the development desktop runs
> Wayland (niri + Noctalia) and its owner forbids XWayland and
> xwayland-satellite. The X11 ghost overlay and accept tap therefore cannot run
> in the live desktop session — by policy, not by defect — and Wayland overlay
> placement is Phase 3 by design. Manual testing splits into three lanes:
>
> - **Lane N (native, your real session):** the display-server-agnostic and
>   session-bus surfaces — AT-SPI plumbing, clean degradation without X,
>   StatusNotifierItem tray, zenity confirm, Secret Service keyring, reveal,
>   shutdown.
> - **Lane S (sandbox):** the visible UX — ghost placement, accept, dismiss,
>   chord handling, and always-on shortcuts — inside the repo's AT-SPI session
>   harness (Xvfb + private D-Bus + accessibility bus), viewed through a
>   **Wayland-native** VNC client so no X socket or `DISPLAY` ever reaches the
>   desktop session.
> - **Lane P (package):** the experimental AppImage built on the oldest
>   supported distribution, then copied to a separate clean X11 desktop for
>   artifact and end-to-end acceptance. This lane is required before Linux
>   distribution; the assembler self-test is not a substitute.

## Bring-up

Build once: `cargo build --locked -p app` (binary lands at
`target/debug/compme`).

**Lane S** (two terminals):

```sh
# 1. The sandbox (leave running; logs in /tmp/compme-manual):
nix-shell -p gcc pkg-config gtk3 at-spi2-core glib xorg.xvfb dbus zenity \
  dejavu_fonts x11vnc gedit --run \
  'tools/acceptance/run-linux-atspi-session.sh --run-in-session \
     tools/acceptance/run-linux-manual-session.sh'

# 2. The viewer, from the Wayland desktop:
nix shell nixpkgs#wlvncc -c wlvncc localhost 5900
```

Inside the viewer: gedit is the target app; compme runs with `COMPME_DEBUG=1`,
an isolated config, and the deterministic stub completion `" world"`. Watch
`tail -f /tmp/compme-manual/compme.log` while testing. End the session with
`touch /tmp/compme-manual/stop`.

**Lane N** runs the same binary directly in the desktop session (no `DISPLAY`
is expected to exist):

```sh
COMPME_DEBUG=1 COMPME_STUB_COMPLETION=" world" \
  COMPME_CONFIG="$(mktemp -d)/config.env" ./target/debug/compme 2>&1 | tee /tmp/cm-native.log
```

**Lane P** uses the assembler documented in [`DEVELOPMENT.md`](DEVELOPMENT.md)
on the oldest supported distribution. Record the source commit, build-host OS
and architecture, reviewed AppImage runtime source and checksum, packaging-tool
versions, output checksum, and the clean test-host OS. Copy the finished
artifact to that clean host rather than reusing the build environment. Do not
publish it from this checklist; Linux publication remains a separate release
decision and runbook change.

## Gate ledger

Mark ✅/❌ with date + one-line evidence (log line or observation). Failures
become fix loops; do not mark a gate from a headless run.

### Lane S — sandbox UX gates

- [x] `lx1-ghost-at-caret` — Type a word in gedit; the ghost " world" renders
      at the caret position in the DejaVu face, tracks the caret as you
      continue typing, and never detaches, flickers, or paints over the wrong
      window.
- [x] `lx2-tab-accepts` — With the ghost visible, Tab inserts the completion
      into the gedit buffer exactly once (readback matches, no doubled text),
      the ghost hides, and the log records the accept.
- [x] `lx3-tab-passes-through-without-ghost` — With no ghost visible, Tab
      reaches gedit as a normal Tab (indent/insert) and is never swallowed.
      This is the key-eating polarity the tap's grab-exactly-while-armed
      contract exists to protect.
- [x] `lx4-dismiss-and-supersede` — Escape dismisses a visible ghost
      (`usage … dismissed` increments); continuing to type replaces a stale
      ghost with the new suggestion (superseded, not stacked).
- [ ] `lx5-modified-chords-pass-through` — With the ghost visible, Ctrl+Tab
      and Alt+Tab are not intercepted (exact-modifier grabs): they reach
      gedit/the window manager untouched.
- [ ] `lx6-rebound-accept-chord` — Supply a rebound accept chord through
      `COMPME_ACCEPT_WORD_KEY` in the Lane S command's environment, and confirm the
      new chord accepts while plain Tab (if unbound) passes through; an
      untranslatable chord must fail soft without killing the live set.
- [ ] `lx7-clean-sandbox-shutdown` — `touch /tmp/compme-manual/stop`: compme
      exits within the bounded-teardown window, no keyboard grab survives
      (typing in gedit stays normal after exit), and the log shows orderly
      shutdown.
- [ ] `lx8-always-on-x11-shortcuts` — Supply distinct, non-reserved chords
      through `COMPME_FORCE_ACTIVATE_KEY`, `COMPME_TOGGLE_APP_KEY`,
      `COMPME_TOGGLE_GLOBAL_KEY`, and `COMPME_GRAMMAR_CHECK_KEY` in the
      environment of the Lane S bring-up command, then press each physically.
      Use the persisted chord grammar in [`README.md`](../README.md); these
      values survive the manual runner's fresh empty config on every launch.
      Keep them distinct from the suggestion accept chords. Restore app and
      global enablement after each toggle before checking the next action.
      The matching action fires once and the chord does not leak into gedit;
      the shortcuts work with no ghost visible, while force-activate re-shows
      only a suggestion the engine still holds. In a separate run, reserve one
      configured chord in another client on the same sandbox `DISPLAY` before
      compme starts, and confirm shortcut setup degrades without disabling a
      working suggestion-scoped accept tap.

### Lane N — native Wayland gates

- [ ] `ln1-clean-degradation-without-x` — On the real niri session (no
      `DISPLAY`), compme starts, reports the actual tray outcome and the
      unavailable deep-link/tap/overlay surfaces as non-fatal, keeps the AT-SPI
      focus/caret/read path live, and does not crash, busy-loop, or spam the
      log. _Code prerequisite closed 2026-09-08 (`b8d3626`): before it, the missing tap was
      reported as `UnsupportedField` and the run loop exited at startup._
- [ ] `ln2-zenity-confirm` — A destructive action prompts through zenity
      (native Wayland GTK): the confirming button is not the default, cancel
      and timeout both decline, and a missing display would have been
      pre-flighted rather than silently declining.
- [ ] `ln3-keyring-secret-service` — With the session's gnome-keyring
      unlocked, the encrypted-store key is created/read through
      `org.freedesktop.secrets`; locking the collection degrades to the
      documented fail-closed behavior, not a crash.
- [ ] `ln4-reveal-file-manager` — Reveal routes through
      `org.freedesktop.FileManager1` if a file manager is present. With that
      service absent, it opens the containing directory through `xdg-open`;
      failure of both routes during the launcher's approximately 50 ms startup
      check returns a typed error naming both attempts. Observe the directory
      actually opening: a later `xdg-open` failure can follow an initial success.
      Non-UTF-8 paths survive byte-exact (spot-check optional; the unit suite
      pins it).
- [ ] `ln5-clean-native-shutdown` — Ctrl+C: bounded shutdown, exit within the
      2 s teardown windows, no orphaned worker threads (process gone, no
      zombie).
- [ ] `ln6-status-notifier-tray` — With a StatusNotifierWatcher and tray host
      active, the Compme icon appears, its title/status and enabled checkmark
      update, and Enable Completions plus one disable/snooze action change the
      running app exactly once. Quit exits cleanly and removes the item. Repeat
      without a watcher (or after stopping it): startup reports the unavailable
      tray within its bound while the AT-SPI adapter keeps running.

### Lane P — clean-machine AppImage gates

- [ ] `lp1-appimage-assembly-provenance` — Assemble from the native release
      binary with reviewed architecture-matched runtime/tool inputs on the
      recorded build host. The output is a nonempty executable at a previously
      unused path; record its SHA-256. Record successful `desktop-file-validate`
      and `appstreamcli validate --no-net` results for the packaged metadata.
      Confirm `readelf` reports the intended architecture/interpreter and no
      `/nix/store` loader or library path.
- [ ] `lp2-appimage-clean-launch` — On the recorded clean host, launch the
      copied AppImage directly (not an extracted build tree). It starts without
      undeclared host-library errors, exposes the packaged desktop metadata and
      icon when integrated by the desktop, and shuts down without leftover
      processes. Record stdout/stderr and the artifact checksum observed on the
      test host.
- [ ] `lp3-appimage-real-desktop-flow` — In that host's real X11 desktop with
      AT-SPI enabled, exercise focus/read, caret and range geometry, ghost
      placement, one accept, one always-on shortcut, and the StatusNotifierItem
      tray from the packaged binary. The text mutation occurs exactly once and
      every X11 grab is released on exit. This is package acceptance only; it
      does not make Linux supported or published.
- [ ] `lp4-appimage-shortcut-collision` — On the real X11 test desktop, reserve
      one configured always-on shortcut in the window manager or a second X11
      client before launching the AppImage. Compme reports the unavailable
      shortcut set without disabling its suggestion-scoped accept tap; accepting
      a visible ghost still works once, and exit releases every grab Compme did
      acquire.

## Evidence

| Gate | Result | Date | Evidence |
|---|---|---|---|
| lx1 | ✅ | 2026-08-26 | Ghost rendered at the gedit caret and tracked typing; 54 stub requests logged, stale generations superseded (gen skips) |
| lx2 | ✅ | 2026-08-26 | Single insertion confirmed at the keyboard; `accept Word` in compme.log, ghost hid |
| lx3 | ✅ | 2026-08-26 | Repeated Tab with no ghost always reached gedit; no spurious accept logged |
| lx4 | ✅ | 2026-08-26 | Two `dismiss (Esc)` log lines; typing superseded rather than stacked |
| lx8 | | | |
| ln6 | | | |
| lp1 | | | |
| lp2 | | | |
| lp3 | | | |
| lp4 | | | |
| _(fill per run)_ | | | |

Wayland-native overlay/accept remain **Phase 3 by design** — nothing in this
checklist claims them. Since the G16 fix, a Wayland-only session
(`WAYLAND_DISPLAY` set, `DISPLAY` unset) reports `overlay_at_caret: None` in
`capabilities`, so the engine runs with no inline ghost instead of arming an
X11 placement that cannot map. When Phase 3 lands, this document grows a
Lane W.

No unchecked Lane N, S, or P row is release evidence. In particular, the
AppImage assembler and its self-test do not imply a published Linux artifact.
