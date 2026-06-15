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

### 3.3 Statistics — range picker (commit `feat(stats)…` range picker)
- [ ] Settings → **Statistics** tab shows a **Range:** dropdown at the top-right
      of the pane, not overlapping the "This session + lifetime" header.
- [ ] The dropdown lists exactly **Last 7 days / Last 14 days / Last 30 days**
      in that order, with **Last 7 days** preselected.
- [ ] Selecting **Last 14 days** / **Last 30 days**, then reopening Settings
      (the rows recompose on show), re-renders the sparkline rows over the
      chosen span (more glyphs / different shape for a wider window).
- [ ] Default (Last 7 days) renders identically to before the picker existed.

_Backed by pure, unit-tested logic: `stats::StatRange::{ALL,days,label,from_index}`
+ `metric_series` (range/group/metric series) + `from_index` OOB-clamp. Only the
NSPopUpButton wiring is unverified here._

<!-- Future Tier-3 FFI items (group/metric pickers, Personalization/Emoji
controls, Apps editing rows, the 3.4 hotkey recorder rows + Carbon registration)
append their LOOK gates below as they land. -->
