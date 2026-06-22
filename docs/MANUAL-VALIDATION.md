# compme — Manual UX Validation Checklist

> Live, human-at-the-Mac validation record for the Tier-3 settings UI. The
> Statistics range/grouping and Emoji gender LOOK gates below were completed on
> 2026-06-17 and are summarized in [`ACCEPTANCE.md`](ACCEPTANCE.md). Future
> AppKit LOOK items can still use this file as the working checklist before
> their evidence is promoted into the acceptance record.
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

## Caret-rect calibration — Chromium forks (pending live evidence)

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

<!-- Future Tier-3 FFI items (group/metric pickers, Personalization controls,
Apps editing rows, the 3.4 hotkey recorder rows + Carbon registration) append
their LOOK gates below as they land. -->
