# Next development steps after the 2026-09-08 full audit

**Date:** 2026-09-08 · **Reviewed:** 2026-09-09 against `ecf5f7c` ·
**Status:** items 0, 1 (code half), 3, item 2e (G5), the lock/`front_app`
half of 5, and item 9's `model_fetch` redirect tests delivered 2026-09-08
(`f0bec03`, `b8d3626`, `5bf36fc`, `df34a62`; CI green at `857f1a4`);
items 2a–2c delivered 2026-09-10 (`e3c8d37`, `74e428e`, `b4ae361`, plus
`7462f33` fixing a mac-lane clippy `unused_mut` in the new fake builder),
and item 2d (G2) the same day (`b26caca` + `c93b508`); items 4a
(settings-command seam) and 4b (host-event context seam) delivered
2026-09-10 (`bbf3724`, `4584b35`) — item 4 is closed. CI green on every
lane (runs 34432782232, 34439157981, 34455569795, and 34488750297, macOS
included; platform_macos 357 tests, app 606) — item 2 is closed except for
the Chromium-family live recording attached to the caret-marker gates.
Still open: item 1's live `ln1` record (owner's niri host), 6 (G6
verification of the red `9b91b35` run first, then G7/G20), 7, 8, the rest
of 9, and 10 · item 5 closed 2026-09-10/11 (`3e061ba`/`6ed8289`
Wayland capability, `bf5893b` D-Bus timeouts, `02eaaaf` overlay collapse;
the 37-test Xvfb lane runs on the dev host) ·
**Tree audited:** `2d18c34` (v0.1.6 + same-day post-release commits); seven
commits since, all from this plan (`b865790`…`ecf5f7c`)
**Evidence base:** `Qfd.md` §20 — five parallel finder passes, every ledger row
re-read by the coordinating reviewer; local portable gate green (see Evidence).
**Supersedes:** the ordering of items 7–12 in `docs/ROADMAP.md` "Current
execution order". Items 3, 5, 8, 9, 10, 12 there are unchanged; this plan
inserts three clusters ahead of Windows Phase 1 and replaces the seam-work
design with a smaller cut.

Anchors cite `2d18c34`; line numbers drift, re-locate by symbol. The
cross-cutting rules of
[`2026-07-18-quality-and-release-gates.md`](2026-07-18-quality-and-release-gates.md)
apply (checker pins move with the doc, one work item per commit series, every
gate green before commit, non-mac lanes prove nothing about `cfg(target_os)`
branches).

## Why this order

1. **Text integrity on the shipping platform first.** G2/G3/G4 sit on the
   macOS insert path. Every other item is recoverable; a double insert or a
   truncated field rewrite is user-visible data damage.
2. **Linux is fatal on the desktop it was validated on.** G1 means the product
   exits on the owner's Wayland/niri host and on any X11 host whose window
   manager already grabs Tab. It is a three-line fix with a unit test.
3. **The roadmap's own prerequisite.** ROADMAP says the `run()` seams land
   before a second native shell. They need a safety net that does not exist:
   the eight heartbeat phases have zero tests.
4. **Windows Phase 1 then starts from honest ground** (G9 fixed, phase tests
   in place, seams typed) instead of mirroring a mac-shaped flag bus.

## Scope

| # | Work item | Findings | Lane that proves it | Est. |
|---|-----------|----------|---------------------|------|
| 0 | Doc and ledger restamp | G12, G18, G11 (record part) | Linux + mac doc pins | 2 h |
| 1 | Off-mac correctness fixes | G1, G9, G10, G13, G17 | Linux/Windows CI | 3–4 h |
| 2 | macOS insert-path hardening | G3, G4, G2, G5 | mac lane + gates | 1–2 d |
| 3 | Heartbeat-phase tests | §20.3 | Linux | 1 d |
| 4 | Settings-watcher and host-event seams | ROADMAP seam work | Linux (+ mac compile) | 2–3 d |
| 5 | Linux blocking and lock hygiene | G8, G16 | Xvfb lane + `ln1` | 1 d |
| 6 | AX worker throughput | G6, G7, G20 | mac lane + live | 2 d |
| 7 | Windows Phase 1, slice 1.1 | ROADMAP 1.1 | `windows-latest` | 2 d |
| 8 | Windows UIA read-only slice | ROADMAP 1.1 | `windows-latest` + notepad smoke | 3–5 d |
| 9 | Pre-emptive hardening | G14, G15, G19, vendor drift | Linux | 1 d |
| 10 | Owner decisions | G11 policy, governance, release notes | — | decision |

## Item 0 — Doc and ledger restamp (S)

- `README.md:35-39` and `docs/ROADMAP.md:5,93-109`: release boundary to
  `v0.1.6` / `6c0bea5`. Add "Release boundary" anchors for both to
  `tools/release/check-version-docs.sh` (same exact-phrase style as the
  existing eight) so the next release cannot leave them behind.
- Flip the satisfied rows: Qfd §10 item 7, §13 item 3, §14 item 6, §19
  "first live proof is still the next tag"; `FIXED.md:33` A53. Correct Qfd
  §14 `:460,552` (doctests now run in CI). Record `ce39c50`, `ec3247e`,
  `fcbc8f6` in the ROADMAP delivery log.
- ROADMAP counts: `run()` 1,557 lines (measured `pub fn run` to its closing
  brace, unchanged since `b8d3626`), `SettingsFlags` 39 fields, `TrayFlags`
  11. The original "1,559 / 42" figures were off; corrected 2026-09-09 in
  `docs/ROADMAP.md` and here.
- Release-notes policy: either delete `docs/RELEASE-NOTES-v0.1.6.md` or amend
  `docs/RELEASING.md:349-354` and `README.md:183-185`. Recommended: amend the
  policy to "hand-written notes optional for patch releases"; deleting evidence
  is worse than a policy footnote.
- Add a per-ID evidence table to `docs/ACCEPTANCE.md` under the 22-gate list
  (columns: gate, last result, date, binary/commit, tester) modelled on
  `docs/MANUAL-VALIDATION-LINUX.md:110-115`, initially populated with the three
  partial rows and "never recorded" for the rest. This is the recording half
  of G11; the policy half is item 10.
- Verify every touched doc against the mac-only pins before pushing (harness
  technique in the memory note `mac-live-mode-doc-pins`).

## Item 1 — Off-mac correctness fixes (S), one commit each

1. **G1** `crates/platform_linux/src/lib.rs:330-332`: return
   `Self::accessibility_unavailable("subscribe_accept")` when
   `accept_tap_installable` is false, matching focus/caret. Unit test:
   `LinuxAdapter::new().subscribe_accept(..)` maps through
   `subscription_error_action(true, ..)` to `Unavailable`, not `Fatal`. Then
   run `ln1-clean-degradation-without-x` on the niri host and record it.
   *Status 2026-09-09:* code and unit test shipped in `b8d3626`; the `ln1`
   row in `docs/MANUAL-VALIDATION-LINUX.md` is still unchecked — it needs
   the owner's Wayland session and stays open under this item.
2. **G9** `crates/app/src/shell/stub.rs:133-152,194-196`: the three
   non-Linux stubs return `Err(KeymapError::..)` / `Err(UnsupportedField)` so
   the loop takes its existing "using defaults" and "settings window
   unavailable" branches (`run_loop.rs:3471-3473,5512-5514`). Pin with a
   `cfg(not(target_os = "linux"))` test; the Windows CI lane compiles it.
3. **G10** `fetch_xor(true, Ordering::Relaxed)` at `run_loop.rs:5216,5387`
   and `platform_macos/src/tray.rs:70`. Test: two toggles from two threads
   leave the flag unchanged.
4. **G13** decide and pin. Recommended: in `looks_high_entropy`, treat `/`
   as a base64 signal only when the token also has a digit or mixed case, so
   `https://example.com/some/long/path` survives while `AbC1/…` still trips.
   Add the three probe strings from Qfd §20.5 as tests whichever way the
   decision goes. Note the memory store and diagnostics both flow through
   this function, so the change is user-visible in stored context.
5. **G17** `run_loop.rs:280-297`: prune carets only between `drop_index` and
   the next `Focus` for the same identity; extend the existing backpressure
   tests with a Focus/Caret/Focus/Caret sequence.

## Item 2 — macOS insert-path hardening (S each, mac lane only)

Order matters: the seam (2b) makes 2c and 2d testable.

- **2a G3** ✅ delivered `e3c8d37` (2026-09-10):
  `read_required_ax_string_attribute` refuses any CFString whose content
  cannot round-trip to a Rust string — the plan's `char_len()` vs
  `encode_utf16().count()` predicate gates a conversion built from the
  exact UTF-16 units (`CFStringGetCharacters` + strict `String::from_utf16`),
  returning `UnsupportedField { reason: "AX value not round-trippable" }`.
  Deviation from the letter of the plan: `converted` cannot come from
  `CFString::to_string()` — the pinned core-foundation 0.10.1 `Display`
  path asserts full UTF-8 convertibility and panics on the lone-surrogate
  input this guard exists for, so the exact-copy conversion is used
  instead. `AxRangeTarget::read_value` forwards here (range path covered).
  Unit-tested on a lone-surrogate CFString.
- **2b** ✅ delivered `74e428e` (2026-09-10): `insert_for_field` takes the
  same `&dyn AxRangeTarget` seam (injected through the adapter's
  `ax_range_target`), every FFI call a verbatim forward; the
  recheck→set→caret→readback order is pinned with the recording fake
  (`FakeAxRangeTarget` now logs reads as well as sets). `extend_range_left`
  and its five tests unchanged.
- **2c G4** ✅ delivered `b4ae361` (2026-09-10):
  `ensure_ax_insert_snapshot_unchanged` runs in `insert_range_for_field`
  before the set; the fake serves scripted second value/selected-range
  reads, and both the refusal and the applied read order are pinned.
- **2d G2** ✅ delivered `b26caca` + `c93b508` (2026-09-10): bounded
  readback re-poll (three reads, 20 ms apart) before classifying
  `SilentlyIgnored` in both insert paths, and the synthetic-key fallback
  gated on a bundle allowlist seeded with iTerm2 (the only live evidence)
  with staleness/secure-input checks still first. MVP §15 F2, the
  integration spec, and ARCHITECTURE amended in the same commit. Remaining:
  record the Chromium-family behaviour in `docs/ACCEPTANCE.md` when the
  caret-marker gates run (GUI-bound).
- **2e G5** ✅ delivered early in `b8d3626`: `catch_unwind` around
  `InstallResource` and the `RemoveResource` drop in `ax_worker.rs`
  (`run_ax_worker_loop`), so 2a–2d are what remain here.
- Ship behind a green mac CI lane; local Linux gates prove nothing here.

## Item 3 — Heartbeat-phase tests (M)

Drivers for the eight `_phase` functions at `run_loop.rs:4056-4677` using the
recording fakes already in `run_loop_tests.rs` (the `startup()` tests at
`:8845-8911` are the template). Add a test module for `feature_policy.rs`
and pin `engine`'s unreadable-capabilities degradation
(`engine/src/lib.rs:242-251`). This is the safety net item 4 needs; do not
reorder them.

## Item 4 — Seams, the smaller cut (M)

Replaces the ROADMAP "typed commands + immutable snapshots" design for now.

- **4a** ✅ delivered `bbf3724` (2026-09-10):
  `drain_settings_edges(&SettingsFlags, &mut Config, &mut SettingsState,
  &Prefs, focused_app) -> Vec<SettingsCommand>` (pure edge detection) plus
  `apply_settings_commands(commands, SettingsApplyCtx)` carrying the
  engine/shell/persist effects, including the two OS-backed edges
  (launch-at-login, screen context) with their revert paths — the drain
  emits them without consuming, apply resolves them in the same heartbeat.
  run() 1,557 → 1,432 lines. Eight tests (drain strict red→green on Linux,
  apply test-after); the pre-4a watcher helpers stay as `#[cfg(test)]`
  fixtures. The "89 SettingsFlags references" figure was 82 per the spec
  analysis; the macOS-side flag references are untouched.
- **4b** ✅ delivered `4584b35` (2026-09-10): `HostEventCtx<'a, A, O>`
  borrows the `loop_state` structs + shared services, and
  `handle_control_event` routes Dismiss/Cycle/Shortcut (the arm map had
  drifted: Focus 5024 / Caret 5097 / Accept 5318 / Shortcut 5384 on the
  pre-4a tree, not 5090-5291). The Shortcut arm moved verbatim (normalized
  token diff; the loop-local `continue` became `return`). Focus/Caret/Accept
  stay inline with the reasons recorded on the ctx — the spec-analysis
  requirement to "cover Accept/Dismiss/Cycle or say why not" is satisfied
  by routing Dismiss/Cycle and documenting Accept's why-not. Routing pinned
  with the item-3 fakes. run() 1,432 → 1,310 lines.
- Defer the snapshot bus until a non-mac shell actually produces flags (today
  `stub::make_tray` is `Err`, so there is no second producer).
- Re-stamp the ROADMAP "Remaining architecture seam work" section and the
  `run()` line count in the same commit.

## Item 5 — Linux blocking and lock hygiene (M)

- G8: compute `(app, pid)` before taking the registry lock in
  `atspi_events.rs:91-94`; answer `front_app` from
  `LinuxFieldRegistry::current.app`; bound D-Bus calls with zbus's per-call
  timeout so the adapter returns `PlatformError::Timeout` as the contract
  asks; collapse the overlay's per-request `.check()`s to one sync per
  `present()`.
- G16: ✅ `b8d3626` dropped `NoDisplay=true` from the autostart entry,
  zeroized the keyring write copy, and fixed the kdialog comments. Still
  open: make `atspi_caps` report `Popup`/unsupported overlay when
  `WAYLAND_DISPLAY` is set and `DISPLAY` is not (today
  `capabilities_for_field` always reports `OverrideRedirect`).
- Prove with the 36-test Xvfb lane (`--test-threads=1`) and `ln1`.

## Item 6 — AX worker throughput (M, mac lane)

- G6: ⚠️ **shipped `9b91b35`, UNVERIFIED — the macOS lane is red** (run
  34578291953, `Test (serial, macOS state)`; format/clippy/parallel steps
  green). The dev host's gh token expired before the logs could be
  fetched, so the failing test is unidentified — static review of every
  fake-loop queue simulates clean. The implementation drains contiguous
  same-`(pid, notification)` events to the newest (explicit CFRetain
  balance; first non-observer message deferred one iteration, never
  dropped or reordered) and skips unchanged-`(identity, rect)` polls, the
  A66/250 ms posture unchanged. **Fix-forward: re-auth gh, pull the failing
  step's log, fix the test/impl or revert `9b91b35`, verify the lane
  green before continuing item 6.**
- G7: marshal Carbon register/unregister to the main thread via
  `DispatchQueue::main().exec_sync`, resource ownership stays on the worker.
- G20: annotate the 80 bare `unsafe` blocks in the same pass (mechanical;
  do it while the code is open).
- Live evidence: `always-on-hotkeys-physical-look` and the caret-marker
  gates, recorded in the item-0 evidence table.

## Item 7 — Windows Phase 1, slice 1.1 (M, hosted runner only)

Smallest slice unit-testable on `windows-latest` without a desktop:
`RtlGetVersion` for `environment()` (drops `version: "unknown"`),
`GlobalMemoryStatusEx` for `physical_memory_bytes` (fixes the hard-coded 0 at
`platform_windows/src/lib.rs:142-144`, the same "every model rated Exceeds"
defect Linux already fixed), `PeekMessage` pump. Enable the needed
`windows` features in `platform_windows/Cargo.toml`. Carry the memory crate's
Unix-only hardening (`memory/src/lib.rs:142-214`, tests `:1313-1590`) to
DACL equivalents via the existing `win_host::harden_owner_only` in the same
series.

## Item 8 — Windows UIA read-only slice (L)

`IUIAutomation` on an STA thread: `GetFocusedElement` + `TextPattern` feeding
`capabilities`/`read_context`; best-effort notepad smoke per the
cross-platform plan §1.7. Insertion, the keyboard hook, and the layered
overlay follow only after this reads real fields.

## Item 9 — Pre-emptive hardening (S/M)

- G14: `&exp=<unix>` inside the signed deep-link prefix, rejected past
  expiry, with a test; do it while every command is still reversible.
- G15: decide between a longer forced-exit deadline while the first decode
  is in flight and documenting exit code 70 during warm-up as expected in
  `docs/ACCEPTANCE.md`.
- G19: `persist-credentials: false` on `release.yml:25,383,635` (`:750`
  needs push); extend the checker allowlist form to `release.yml`; add doc
  tests and `cargo audit` to the Windows lane.
- Vendor drift checker in `tools/release/` (`diff -r` against the registry
  tarball, allowlisting the three patched files); open the upstream
  `llama-cpp-2` PR so `[patch.crates-io]` can be retired; `PRAGMA
  user_version` in `memory` before its first schema change. ✅ The loopback
  redirect tests in `model_fetch` (https→http refusal, same-host 302 keeping
  `Range`, redirect cap) landed with item 3 in `5bf36fc`.

## Item 10 — Owner decisions (record either way)

- **G11 policy**: either "patch releases may ship with the 22 gates open"
  written into `docs/ROADMAP.md:133-134` and `docs/RELEASING.md`, or the
  gates become a real pre-tag ledger. Today the doc says one thing and
  v0.1.6 did the other.
- **Governance** (ROADMAP item 10): unchanged; the read-only checker's three
  pending mismatches are the inventory.
- **Release-notes policy** (item 0).

## Deliberately not in this plan

- Full `SettingsFlags`/`TrayFlags` snapshot redesign (deferred, see item 4).
- Linux StatusNotifierItem tray, always-on shortcut registration, XTEST
  fallback, `text_range_rect`, Wayland placement: unchanged Phase 2/3
  residuals in ROADMAP 1.1, sequenced after Windows slice 1.1 because Linux
  remains experimental by decision.
- Sparkle/appcast updater (ROADMAP 1.2, optional).
- Any change to the accepted A66 latency posture beyond removing its
  duplicate-event cost (item 6).

## Evidence (2026-09-08, this Linux host, staged toolchain 1.97.0)

Run through `nix-shell -p gcc pkg-config gtk3 at-spi2-core glib dbus cmake
ninja clang ruby shellcheck go` with the staged `target/revalidate/env.sh`
(its libclang/cmake store paths had been garbage-collected and were repaired
first):

| Step | Result |
|---|---|
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --locked --workspace --exclude platform_macos --all-targets -- -D warnings` | pass |
| `cargo test --locked --workspace --exclude platform_macos --exclude app --all-targets` | all pass (36 Linux live tests ignored as designed) |
| `cargo test --locked -p app --all-targets -- --test-threads=1` | 558 + 1 pass, 2 ignored |
| `cargo test --workspace --exclude platform_macos -- --list` | 1,741 tests |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --exclude platform_macos` | pass |
| `cargo build --locked -p app` | pass |
| `shellcheck --severity=error` over `tools/**/*.sh`; `bash -n` | pass |
| `validate-version.sh`, `check-version-docs.sh` (live + self-test), `check-bundle-metadata.sh`, `check-agent-briefs.sh`, `check-linux-live-test-count.sh` (36), `check-model-gates.sh --self-test`, `check-github-governance.sh --self-test`, `check.sh --self-test` | pass |
| `cargo audit` | **not run** (cargo-audit not installed on this host) |
| `platform_macos` tests, `check-model-gates.sh` live mode, `tools/spike` | **not run** (mac-only; spike's `objc2` refuses to build on Linux) |
| G13 reproduction | `redact("see https://example.com/some/long/path/segment/that/keeps/going")` → `"see https:[redacted-secret]"`; `redact("open /Users/alice/Documents/projects/compme/crates/redaction/src/lib.rs")` → `"open [redacted-secret]"` |

**CI evidence (2026-09-08):** pushed `main` at `857f1a4` (items 0, 1, 3, half of
5, and the phase-test serialization) is green on every lane — macOS
(`check-model-gates.sh` live mode at the 2115 count), Windows (portable
workspace + app binary), Linux (portable workspace + the 36-test Xvfb lane),
and Docs. The intermediate run at `df34a62` failed twice and both causes are
closed in `857f1a4`: the mac count pin was one too high (the new `front_app`
test is Linux-only), and the Windows lane runs `app` tests in parallel, which
the process-env-scoped phase tests now serialize behind a mutex.

## Spec analysis of the open items (2026-09-09, tree `ccf90a2`)

Each open item was read against the governing spec, the adapter contract, and
the code. Corrections the items above must carry; nothing here reorders them.

- **Item 2d (G2) conflicts with MVP spec §15 F2** (`2026-06-03-engine-macos-mvp-design.md:433`:
  keep the fixed `AxSet → SyntheticKeys → Clipboard → None` order, revisit only
  on live proof an app needs a *different* strategy). That order is a
  capability-probe preference; the only runtime fallback is the single
  `SilentlyIgnored` → `SyntheticKeys` retry in `finish_axset_insert`, and a
  bundle allowlist narrows that retry to iTerm2 on absence of proof. Ship it only with F2
  amended in the same commit, and update the integration spec `:13-14,110-112`
  and ARCHITECTURE `:825-826` that describe the unqualified fallback. The
  readback re-poll itself fills a gap no spec fixes.
- **Item 2a also fixes the range path**: `AxRangeTarget::read_value` forwards
  to `read_required_ax_string_attribute`. **2b** must keep `extend_range_left`
  (integration spec `:96-99`) and the two pinned `lib_tests.rs` symbols
  (`check-model-gates.sh:4534-4535`) unchanged. **2c** needs the fake to
  return the second value/selected-range read.
- **Item 6 (G7) vs MVP §2 `:75`** ("AX worker run loop owns … transient Carbon
  accept-hotkey resources"): ownership stays, so add "registered on the main
  thread" to that line. `exec_sync` from the worker deadlocks if main is
  waiting on the worker (`install_resource` is synchronous): use a bounded
  wait or an async post, and re-record `always-on-hotkeys-physical-look`.
  G6 is consistent; no spec mentions the 4 Hz safety poll (gap, record it).
- **Item 4 is spec-consistent.** No spec makes the snapshot bus a
  prerequisite; the Windows spec `:199` plans to map `TrayFlags`/`SettingsFlags`
  directly. Two settings edges are *not* pure: launch-at-login
  (`settings_runtime.rs:135-152` calls the shell inside detection and reverts
  the atomic) and screen context (`context_policy.rs:58-96` probes permission,
  spawns OCR, reverts the flag). Both need `SettingsCommand` variants with a
  revert path. Host-event arms are `Focus` 4807, `Caret` 4880, `Accept` 5101,
  `Shortcut` 5167 (not 5090-5291); `HostEventCtx` must cover
  `Accept`/`Dismiss`/`Cycle` or say why not. The "89 references" figure is 82
  (8 type + 74 field). Re-stamp ROADMAP `:49,1022,1026-1055`, ARCHITECTURE
  `:527-532`, Qfd `:21,214,371,587` when it ships; none are checker-pinned.
- **Item 5 timeouts:** zbus 5.19.0 has only connection-level
  `Builder::method_timeout`, applied inside `Connection::call_method` and
  *not* on generated `*ProxyBlocking` calls — so it bounds the 10 raw sites
  (keyring, reveal, `GetAddress`) but none of the 14 AT-SPI proxy sites on
  the hot path. Set it anyway, and wrap AT-SPI (and x11rb, which has no
  request timeout) calls in the helper-thread + `recv_timeout` pattern
  `x11_tap.rs:718,912` already uses, mapped to `PlatformError::Timeout`
  (today zero production sites emit it). Qfd's "zbus default 25 s" is
  libdbus's figure; zbus defaults to unbounded.
- **Item 5 overlay:** the collapse has not started; the one `.check()` token
  is inside `checked()`, called at 15 sites (10–12 round trips on first show,
  8–10 steady). X errors are per-request, so one trailing check does not
  preserve the fail-closed guarantee at `x11_overlay.rs:85-92`: send
  unchecked, then sync and drain `poll_for_event` for `Error` before `Ok`,
  with a live test that injects a bad request.
- **Item 5 Wayland:** consistent with the review spec `:124` and the contract
  `platform/src/lib.rs:252-254`; patch `overlay_at_caret` to `None` in
  `LinuxAdapter::capabilities` (`lib.rs:404-413`, where `accept_intercept` is
  already patched) — the symbols are `atspi_caps::capabilities_from` and
  `LinuxAdapter::capabilities`, not `capabilities_for_field`. Stale after:
  ROADMAP `:525-526`, ARCHITECTURE `:715`, MANUAL-VALIDATION-LINUX `:120`.
- **Item 7 mis-phases the DACL work.** Memory hardening on Windows is spec
  Phase 0.2 (`2026-07-08-cross-platform-implementation-plan.md:87-103`,
  shipped): `run_loop.rs:1318-1330` already hardens the db and sidecars via
  `win_host::harden_owner_only`. Of the five `cfg(unix)` sites only the ACL
  half has a Windows analogue; symlink/reparse rejection is not ACL work.
  Drop that bullet; fix the stale `memory/src/lib.rs:22-24` comment and
  ROADMAP `:1400-1401` instead. Spec 1.1 also requires the
  `MsgWaitForMultipleObjectsEx` heartbeat wait, not a bare `PeekMessage`
  loop, and its acceptance (boot/idle/quit) is desktop-manual. Enable the
  `windows =0.62.2` features for `RtlGetVersion` and
  `GlobalMemoryStatusEx` (verify names). Pins: windows job step shapes
  `check-model-gates.sh:3907-3910,3959-3962`, test-count pins, the
  `version == "unknown"` test and the "Tier 1.1 scaffold" reason string.
- **Item 8:** §1.7 reference is correct. Spec 1.2 also wants
  `AddFocusChangedEventHandler`, `caret_rect`, `subscribe_caret`. The specs
  disagree on threading: the implementation plan `:140` says STA, the
  cross-platform review `:58,126,219` says MTA on a window-less thread —
  decide and record before coding.
- **Prerequisites the plan sequences only implicitly:** the ROADMAP-stated
  logging seam A31 (`ROADMAP.md:1043-1045`, "before Windows/Linux UI adapter
  work C.5") is absent from this plan. Add it to item 4 or record the
  deferral.
- **Item 9 G14:** `parse_deep_link` rejects unknown params
  (`webconfig/src/lib.rs:189`), so `exp` joins the allow-list, is rejected on
  unsigned links, and the test needs an injectable clock. **G15:** "exit code
  70" is documented only in `RELEASE-NOTES-v0.1.6.md:32`; `shutdown_with_timeout`
  already takes a `Duration`, so the warm-up deadline option is cheap.
  **G19:** `:750` needs push (`finalize-cask.sh:227`); the other three already
  scrub credentials after their last fetch. Adding the flag needs the
  approved-input sets at `check-model-gates.sh:3754` and the topology at
  `:479,483,486` updated in the same commit; the Windows doc-test/audit step
  is a second topology edit (`:481`). **Vendor drift:** allowlist the
  vendored `Cargo.lock` too; the checker must be `--self-test`-able and join
  the pinned DEVELOPMENT gate list. **`user_version`:** the DDL pin
  `the_0x_schema_is_exactly_this_ddl_until_a_migration_lands` asserts
  `user_version == 0`; defer until a schema change is actually scheduled.
- **Item 10 G11:** the sentence is `ROADMAP.md:160-163` (not `:133-134`);
  RELEASING `:157-161` and the runbook have no 22-gate step. Neither option
  is checker-pinned. **Governance:** the checker emits six pending decisions
  (self-approval, admin bypass, deployment branches, actions allowlist,
  SHA-pin requirement, tag creation), not three.
