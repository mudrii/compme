# compme — Manual UX Validation Checklist

> Live, human-at-the-Mac validation gates **deferred** by the autonomous Tier-3
> loop. Each item below is **code-complete and gate-green** (`fmt` + `clippy -D
> warnings` + `cargo test --workspace` all pass) but its on-screen behavior was
> not visually confirmed — AppKit/objc2 construction is build-verified only.
>
> Run the app and work down the list:
> ```sh
> cd ~/src/compme && COMPME_DEBUG=1 cargo run -p app 2>&1 | tee /tmp/cm.log
> ```
> Open the tray → Settings… and exercise each item. Mark ✅/❌ and note anything
> off; failures become a follow-up fix loop.

## Tier 3 settings UI

### 3.3 Statistics — range + grouping pickers (commits `feat(stats)…` range / grouping picker)
- [ ] Settings → **Statistics** tab shows **two** bare dropdowns side-by-side on
      the header row (no text labels — the items are self-describing): a range
      popup then a grouping popup, both right of the "This session + lifetime"
      header with no overlap and no clipping at the pane's right edge (the group
      popup ends ~22px from the usable edge — confirm it isn't cut off).
- [ ] No orphaned/ghost "Range:" label remains (it was removed when the second
      picker landed).
- [ ] Range popup lists **Last 7 days / Last 14 days / Last 30 days** (Last 7
      preselected); grouping popup lists **Daily / Weekly** (Daily preselected).
- [ ] Range default (7 days) + grouping default (Daily) render identically to
      before the pickers existed.
- [ ] Switching grouping to **Weekly** with a ≥14-day range, then reopening
      Settings (rows recompose on show, not instantly — same as the range
      picker), collapses the rows to one bar per week, oldest week first, with
      the trailing partial week summed (not dropped).

_Backed by pure, unit-tested logic: `stats::StatRange::{ALL,days,label,from_index}`
+ `metric_series` (range/group/metric series) + `from_index` OOB-clamp. Only the
NSPopUpButton wiring is unverified here._

### 3.2 Emoji — gender picker (commit `feat(emoji): add gender picker`)
- [ ] Settings → **Emoji** tab shows a **Gender** dropdown directly below the
      **Skin tone** dropdown, with no visual overlap.
- [ ] The dropdown lists **Neutral / Female / Male** and reflects the persisted
      `COMPME_EMOJI_GENDER` on open (Neutral by default).
- [ ] Changing it persists `COMPME_EMOJI_GENDER` to `config.env` and (if a ghost
      suggestion is visible) dismisses it, mirroring the skin-tone picker.

_Backed by unit-tested pure helpers (`emoji_gender_{index,from_index,value}` +
`handle_emoji_gender_change[_with_invalidation]`); only the NSPopUpButton wiring
is unverified here._

<!-- Future Tier-3 FFI items (group/metric pickers, Personalization controls,
Apps editing rows, the 3.4 hotkey recorder rows + Carbon registration) append
their LOOK gates below as they land. -->
