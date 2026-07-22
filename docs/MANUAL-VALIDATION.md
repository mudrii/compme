# compme — Manual UX Validation Checklist

> Live, human-at-the-Mac working checklist for Settings, grammar correction,
> caret calibration, and runtime LOOK gates. The Statistics range/grouping and
> Emoji gender gates below were completed on 2026-06-17 and are summarized in
> [`ACCEPTANCE.md`](ACCEPTANCE.md). New evidence is promoted into that document's
> authoritative Manual/Live Gate Ledger.
>
> **Release boundary (2026-07-13):** this checklist tracks current `main`.
> Validate the latest published `v0.1.5` binary from tag `v0.1.5` (commit
> `14ae81e`) and its release assets; current `main` contains post-release
> build, release-tooling, cask, and documentation changes. The
> local/manual-only A2 policy and the single **Show Models Folder** invariant
> are included in v0.1.5.
>
> Run the app and work down the list:
> ```sh
> cd ~/src/compme && COMPME_DEBUG=1 cargo run -p app 2>&1 | tee /tmp/cm.log
> ```
> Open the tray → Settings… and exercise each item. Mark ✅/❌ and note anything
> off; failures become a follow-up fix loop.

## Tier 3 settings UI

### 3.3 Statistics — range + grouping pickers (completed 2026-06-17)
- [x] Settings → **Statistics** tab shows **two** bare dropdowns side-by-side on
      the header row (no text labels — the items are self-describing): a range
      popup then a grouping popup, both right of the "This session + lifetime"
      header with no overlap and no clipping at the pane's right edge (the group
      popup ends ~22px from the usable edge — confirm it isn't cut off).
- [x] No orphaned/ghost "Range:" label remains (it was removed when the second
      picker landed).
- [x] Range popup lists **Last 7 days / Last 14 days / Last 30 days** (Last 7
      preselected); grouping popup lists **Daily / Weekly** (Daily preselected).
- [x] Range default (7 days) + grouping default (Daily) render identically to
      before the pickers existed.
- [x] Switching grouping to **Weekly** with a ≥14-day range, then reopening
      Settings (rows recompose on show, not instantly — same as the range
      picker), collapses the rows to one bar per week, oldest week first, with
      the trailing partial week summed (not dropped).

_Live evidence: Settings preserved Last 14 days + Weekly across reopen and
rendered weekly two-bar sparklines with the Lifetime row still visible._

### 3.2 Emoji — gender picker (completed 2026-06-17)
- [x] Settings → **Emoji** tab shows a **Gender** dropdown directly below the
      **Skin tone** dropdown, with no visual overlap.
- [x] The dropdown lists **Neutral / Female / Male** and reflects the persisted
      `COMPME_EMOJI_GENDER` on open (Neutral by default).
- [x] Changing it persists `COMPME_EMOJI_GENDER` to `config.env` and (if a ghost
      suggestion is visible) dismisses it, mirroring the skin-tone picker.

_Live evidence: the dropdown exposed Neutral/Female/Male, persisted
`COMPME_EMOJI_GENDER=female`, and reopened with Female selected. Stale-ghost
invalidation remains unit-covered by `emoji_gender_edge_invalidates_stale_visible_suggestion`._

## Tier 5 grammar/spell-fix LOOK gate (scripted-live pass 2026-07-07; physical LOOK pending)

Runner ID: `grammar-fix-textedit-look`.

A scripted assisted-session live pass on 2026-07-07
recorded in the historical matrix and summarized in
[`ACCEPTANCE.md`](ACCEPTANCE.md) (after commit 5126509's few-shot grammar prompt)
rendered the underline and banner
with a real model, accepted in place, and refused a stale correction. The
checkboxes below remain unchecked because they record the formal human/physical
pass.

> Deterministic coverage is green for grammar request routing, correction
> vetting, scalar-range tracking, correction-only accept handling, and fail-closed
> platform stubs. This checklist records the remaining on-device visual pass
> before it is promoted into [`ACCEPTANCE.md`](ACCEPTANCE.md).

- [ ] Launch `compme` with `COMPME_GRAMMAR_FIX=1`,
      `COMPME_GRAMMAR_CHECK_KEY=<trigger>`, and
      `COMPME_GRAMMAR_ACCEPT_KEY=<accept>`.
- [ ] In TextEdit, type a single-word typo such as `teh`, place the caret in or
      just after the word, and press the grammar trigger.
- [ ] Confirm a thin underline appears under the word and a correction banner
      appears above it without stealing focus or swallowing normal completion
      accept keys.
- [ ] Press the grammar accept key and confirm the original word is replaced in
      place with the vetted correction, with no duplicate suffix or leftover
      left fragment.
- [ ] Move the caret or edit the field before accepting and confirm the stale
      correction no longer applies.

## Caret-rect calibration — Chromium forks (pending live evidence)

Runner ID: `caret-marker-chromium-forks-calibration`.

> The `RECT_IS_LINE_BUNDLE_PREFIXES` list (platform_macos `normalize_caret_rect`)
> is **evidence-only** ("extend per app on evidence, never by guess") — Chrome,
> Chromium, iTerm2 and Safari's WebKit search fields were each added from live
> screenshots. Brave/Edge/Vivaldi use the same Blink engine as Chrome, so the
> ghost likely lands one line low for them too, but no live evidence exists yet
> and the Safari-omnibox exception shows within-engine surprises are real — so
> they were deliberately NOT added by inference.

- [ ] On a granted desktop, type in **Brave** (`com.brave.Browser`), **Edge**
      (`com.microsoft.edgemac`) and **Vivaldi** (`com.vivaldi.Vivaldi`); confirm
      whether the ghost lands one line low (as Chrome did pre-calibration).
- [ ] If confirmed, add the three bundle prefixes to `RECT_IS_LINE_BUNDLE_PREFIXES`
      and extend the `normalize_caret_rect` test — promoting them from guess to
      evidence, exactly as Chrome/Safari were.

## Caret-rect source — live-Chrome AXTextMarker path (`source=Marker`)

Runner IDs: `caret-marker-chrome-marker`, `caret-marker-chromium-marker`,
`caret-marker-electron-marker`.

> The web caret path (`AXSelectedTextMarkerRange` → `AXBoundsForTextMarkerRange`,
> in `read_ax_bounds_for_selected_text_marker_range`) is first-class and
> preferred over the `NSRange` fallback by `resolve_caret_rect_with_marker_first`,
> which reports `MacosCaretRectSource::Marker`. The Chromium/WebKit zero-width
> marker case is pinned by the unit test
> `resolve_caret_rect_with_marker_first_prefers_zero_width_chromium_marker`. This
> checklist item is the live confirmation the plan-review doc
> ([`2026-06-04-plan-review-online-validation.md`](superpowers/plans/2026-06-04-plan-review-online-validation.md),
> Finding 3) delegates here before declaring broad Chromium/Electron support.

- [ ] On a granted desktop, focus a **Google Chrome** (`com.google.Chrome`)
      textarea or content-editable and type; confirm the ghost lands on the caret
      line (the marker path resolved) rather than one line low or at the field
      origin (the `NSRange` fallback), with `MacosCaretRectSource::Marker` in the
      `COMPME_DEBUG` caret diagnostics.
- [ ] Repeat in a Chromium build (`org.chromium.*`) and an Electron app (e.g. VS
      Code) to confirm the zero-width marker case resolves via the marker path in
      the live app, matching the unit test, before extending Chromium/Electron
      support claims.

## Settings & runtime LOOK gates (runner-emitted)

The authoritative pass/fail ledger for these is
[`ACCEPTANCE.md`](ACCEPTANCE.md)'s Manual/Live Gate Ledger; the runner
(`tools/acceptance/run-a1b-live-gates.sh`) emits each ID and its self-test pins
the set. Walkthrough detail lives in the ledger entries.

- [ ] `apps-policy-toggle-look` — Apps pane On/Tab/Mid/AC/GF columns render
      without overlap; toggling Enabled + Grammar fix dismisses and persists.
- [ ] `personalization-pane-look` — instructions/sender/strength edits commit
      (multiline field) and visibly re-steer output without relaunch.
- [ ] `shortcuts-recorder-look` — physical modifier-combo capture, live rebind,
      persistence across reopen.
- [ ] `always-on-hotkeys-physical-look` — configured force-activate, per-app
      toggle, global toggle, and grammar-check hotkeys dispatch from physical
      keypresses without reopening Settings and persist across relaunch.
- [ ] `nine-tab-settings-walkthrough` — all nine panes fit, hold state, and
      live-apply.
- [ ] `full-autocorrect-prose-code-look` — TextEdit receives an exact
      whole-word macOS spelling offer while a code-editor main pane remains
      fail-closed.
- [ ] `cross-app-previous-inputs-look` — a Full-accept in one supported app
      contributes privacy-safe `sources=recent` diagnostics in another only
      while the Context opt-in is enabled; disabling clears the global ring.
- [ ] `selection-thesaurus-look` — an exact selected word shows a correction
      banner, Down cycles, Full accepts the active synonym into that range, and
      collapsing/moving the selection stales it.
- [ ] `tray-external-links-look` — Visit Website and Contact Support each open
      the exact allowlisted browser destination once.
- [ ] `menu-bar-icon-look` — caret + double-chevron template icon renders
      (text title only as fallback).
- [ ] `setup-model-picker-look` — exactly one **Show Models Folder** control is
      visible, **Reveal Model in Finder** is absent, picker rows carry RAM
      verdicts, and the license gate and dest-exists skip behave.
- [ ] `encrypted-memory-all-monitored-live` — residual legs: secure-input
      suppression, snoozed policy transition, volatile `pid:N` adds no rows.
- [ ] `mirror-window-firefox-zen-look` — Firefox/Zen mirror-window placement
      keeps the ghost out of mirrored monitor captures.
- [ ] `setup-needed-docs-arc-onboarding` — Google Docs in Arc shows setup-needed
      onboarding instead of silently suppressing suggestions.
- [ ] `sidebar-only-editor-assistant-look` — VS Code, Cursor, and Windsurf main
      editors submit no request while positively labelled assistant/chat fields
      submit with `app_allows=true`.
- [ ] `multi-candidate-cycle-physical-look` — physical Down-arrow cycles visible
      candidates in order, wraps predictably, and accept inserts the selection.
- [ ] `input-monitoring-revoked-carbon-accept` — accept keys still fire with
      Input Monitoring revoked (Carbon path).
