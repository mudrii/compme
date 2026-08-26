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
> placement is Phase 3 by design. Manual testing splits into two lanes:
>
> - **Lane N (native, your real session):** everything display-server-agnostic
>   — AT-SPI plumbing, clean degradation without X, zenity confirm, Secret
>   Service keyring, reveal, shutdown.
> - **Lane S (sandbox):** the visible UX — ghost placement, accept, dismiss,
>   chord handling — inside the repo's AT-SPI session harness (Xvfb + private
>   D-Bus + accessibility bus), viewed through a **Wayland-native** VNC client
>   so no X socket or `DISPLAY` ever reaches the desktop session.

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

## Gate ledger

Mark ✅/❌ with date + one-line evidence (log line or observation). Failures
become fix loops; do not mark a gate from a headless run.

### Lane S — sandbox UX gates

- [ ] `lx1-ghost-at-caret` — Type a word in gedit; the ghost " world" renders
      at the caret position in the DejaVu face, tracks the caret as you
      continue typing, and never detaches, flickers, or paints over the wrong
      window.
- [ ] `lx2-tab-accepts` — With the ghost visible, Tab inserts the completion
      into the gedit buffer exactly once (readback matches, no doubled text),
      the ghost hides, and the log records the accept.
- [ ] `lx3-tab-passes-through-without-ghost` — With no ghost visible, Tab
      reaches gedit as a normal Tab (indent/insert) and is never swallowed.
      This is the key-eating polarity the tap's grab-exactly-while-armed
      contract exists to protect.
- [ ] `lx4-dismiss-and-supersede` — Escape dismisses a visible ghost
      (`usage … dismissed` increments); continuing to type replaces a stale
      ghost with the new suggestion (superseded, not stacked).
- [ ] `lx5-modified-chords-pass-through` — With the ghost visible, Ctrl+Tab
      and Alt+Tab are not intercepted (exact-modifier grabs): they reach
      gedit/the window manager untouched.
- [ ] `lx6-rebound-accept-chord` — Persist a rebound accept chord in the
      session's `config.env`, restart compme in the sandbox, and confirm the
      new chord accepts while plain Tab (if unbound) passes through; an
      untranslatable chord must fail soft without killing the live set.
- [ ] `lx7-clean-sandbox-shutdown` — `touch /tmp/compme-manual/stop`: compme
      exits within the bounded-teardown window, no keyboard grab survives
      (typing in gedit stays normal after exit), and the log shows orderly
      shutdown.

### Lane N — native Wayland gates

- [ ] `ln1-clean-degradation-without-x` — On the real niri session (no
      `DISPLAY`), compme starts, reports tray/deep-link scaffolds and the
      unavailable tap/overlay as non-fatal, keeps the AT-SPI focus/caret/read
      path live, and does not crash, busy-loop, or spam the log.
- [ ] `ln2-zenity-confirm` — A destructive action prompts through zenity
      (native Wayland GTK): the confirming button is not the default, cancel
      and timeout both decline, and a missing display would have been
      pre-flighted rather than silently declining.
- [ ] `ln3-keyring-secret-service` — With the session's gnome-keyring
      unlocked, the encrypted-store key is created/read through
      `org.freedesktop.secrets`; locking the collection degrades to the
      documented fail-closed behavior, not a crash.
- [ ] `ln4-reveal-file-manager` — Reveal routes through
      `org.freedesktop.FileManager1` if a file manager is present; with none
      on the session, it fails with a typed error naming the gap. Non-UTF-8
      paths survive byte-exact (spot-check optional; the unit suite pins it).
- [ ] `ln5-clean-native-shutdown` — Ctrl+C: bounded shutdown, exit within the
      2 s teardown windows, no orphaned worker threads (process gone, no
      zombie).

## Evidence

| Gate | Result | Date | Evidence |
|---|---|---|---|
| _(fill per run)_ | | | |

Wayland-native overlay/accept remain **Phase 3 by design** — nothing in this
checklist claims them. When that phase lands, this document grows a Lane W.
