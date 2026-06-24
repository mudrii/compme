//! Local usage statistics over a rolling 30-day window (design spec §11 success
//! metrics + §16 "local stats / menu-bar word count" gate).
//!
//! Cotypist surfaces 30-day completion stats (shown / accepted / dismissed /
//! superseded), a words-completed count for the menu bar, and latency. This is
//! the OS-agnostic, pure accumulator that computes them; persistence and the
//! menu-bar display are separate (A3) concerns.
//!
//! Time is **injected** — callers pass `now_ms` (epoch milliseconds) on every
//! record and query — so the window logic is deterministic and unit-testable
//! without a clock (the rest of the workspace follows the same rule). Counts and
//! latencies are filtered to the last 30 days on read, and pruned on write to
//! bound memory.

use std::collections::VecDeque;

/// The rolling window: 30 days in milliseconds.
pub const WINDOW_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// One day in milliseconds (the Statistics-pane chart bucket size).
pub const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// Lifetime outcome totals as persisted to `stats.env` on shutdown (T3).
/// Distinct from the rolling 30-day window: these only ever grow. Pure
/// string parse/render here; file IO stays in the app crate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PersistedStats {
    pub shown: u64,
    pub accepted: u64,
    pub dismissed: u64,
    pub superseded: u64,
    pub words: u64,
}

impl PersistedStats {
    /// These totals plus one session's counts and accepted words.
    pub fn merged(self, session: Counts, session_words: usize) -> Self {
        Self {
            shown: self.shown.saturating_add(session.shown as u64),
            accepted: self.accepted.saturating_add(session.accepted as u64),
            dismissed: self.dismissed.saturating_add(session.dismissed as u64),
            superseded: self.superseded.saturating_add(session.superseded as u64),
            words: self.words.saturating_add(session_words as u64),
        }
    }
}

/// Parse `stats.env` contents. Fail-soft: missing keys, malformed values,
/// and unknown lines all read as zero/ignored — a corrupt stats file must
/// never break startup or shutdown (worst case: lifetime counters reset).
pub fn parse_stats_file(contents: &str) -> PersistedStats {
    let mut out = PersistedStats::default();
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Ok(value) = value.trim().parse::<u64>() else {
            continue;
        };
        match key.trim() {
            "STATS_SHOWN" => out.shown = value,
            "STATS_ACCEPTED" => out.accepted = value,
            "STATS_DISMISSED" => out.dismissed = value,
            "STATS_SUPERSEDED" => out.superseded = value,
            "STATS_WORDS" => out.words = value,
            _ => {}
        }
    }
    out
}

/// Render totals in the dotenv-style format `parse_stats_file` reads.
pub fn render_stats_file(stats: &PersistedStats) -> String {
    format!(
        "# compme lifetime stats (written periodically and on shutdown)\n\
         STATS_SHOWN={}\nSTATS_ACCEPTED={}\nSTATS_DISMISSED={}\n\
         STATS_SUPERSEDED={}\nSTATS_WORDS={}\n",
        stats.shown, stats.accepted, stats.dismissed, stats.superseded, stats.words,
    )
}

/// Render a series as unicode block-bars (▁▂▃▄▅▆▇█), one glyph per value,
/// ceiling-scaled to the series maximum: the max is always full-height, zero
/// is always the baseline glyph, and any nonzero value rises above it (a
/// sparse day must stay visibly different from an idle one). An all-zero
/// series is a flat baseline; an empty series is an empty string.
pub fn sparkline(values: &[usize]) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = values.iter().copied().max().unwrap_or(0);
    values
        .iter()
        .map(|&v| {
            if max == 0 {
                BARS[0]
            } else {
                // Ceiling division onto 0..=7: v=0 → 0, v=max → 7.
                let scaled = ((v as u128) * ((BARS.len() - 1) as u128))
                    .div_ceil(max as u128)
                    .min((BARS.len() - 1) as u128) as usize;
                BARS[scaled]
            }
        })
        .collect()
}

/// One chart bar for the Statistics pane: outcome counts plus accepted words
/// over a single 24h slice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DayBucket {
    pub counts: Counts,
    pub words: usize,
}

/// Re-bucket trailing daily slices by the Statistics grouping picker: `Daily`
/// returns them unchanged; `Weekly` sums every 7 consecutive slices into one
/// bucket, oldest group first (a trailing partial week is summed as-is),
/// summing every outcome count and accepted-words total. Feeds `stats_pane_lines`.
pub fn group_buckets(buckets: &[DayBucket], grouping: StatGrouping) -> Vec<DayBucket> {
    match grouping {
        StatGrouping::Daily => buckets.to_vec(),
        StatGrouping::Weekly => buckets
            .chunks(7)
            .map(|week| {
                let mut agg = DayBucket::default();
                for b in week {
                    agg.counts.shown += b.counts.shown;
                    agg.counts.accepted += b.counts.accepted;
                    agg.counts.dismissed += b.counts.dismissed;
                    agg.counts.superseded += b.counts.superseded;
                    agg.words += b.words;
                }
                agg
            })
            .collect(),
    }
}

/// Selectable trailing span for the Statistics-pane chart (the "range" control).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatRange {
    Last7Days,
    Last14Days,
    Last30Days,
}

impl StatRange {
    /// Picker menu order (drives the range NSPopUpButton's item list).
    pub const ALL: [StatRange; 3] = [
        StatRange::Last7Days,
        StatRange::Last14Days,
        StatRange::Last30Days,
    ];

    /// The number of trailing 24h slices this range covers.
    pub fn days(self) -> usize {
        match self {
            StatRange::Last7Days => 7,
            StatRange::Last14Days => 14,
            StatRange::Last30Days => 30,
        }
    }

    /// Human-readable picker item title.
    pub fn label(self) -> &'static str {
        match self {
            StatRange::Last7Days => "Last 7 days",
            StatRange::Last14Days => "Last 14 days",
            StatRange::Last30Days => "Last 30 days",
        }
    }

    /// Decode a selected picker-row index; out-of-range clamps to the first
    /// item (mirrors the model picker's total-over-OOB selection).
    pub fn from_index(index: usize) -> Self {
        *Self::ALL.get(index).unwrap_or(&Self::ALL[0])
    }
}

/// How the chart groups its trailing daily slices (the "group" control).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatGrouping {
    /// One bar per 24h slice.
    Daily,
    /// One bar per 7 consecutive slices, oldest group first; a trailing
    /// partial group (e.g. the last 2 days of a 30-day range) is summed as-is.
    Weekly,
}

impl StatGrouping {
    /// Picker menu order (drives the grouping NSPopUpButton's item list).
    pub const ALL: [StatGrouping; 2] = [StatGrouping::Daily, StatGrouping::Weekly];

    /// Human-readable picker item title.
    pub fn label(self) -> &'static str {
        match self {
            StatGrouping::Daily => "Daily",
            StatGrouping::Weekly => "Weekly",
        }
    }

    /// Decode a selected picker-row index; out-of-range clamps to the first item.
    pub fn from_index(index: usize) -> Self {
        *Self::ALL.get(index).unwrap_or(&Self::ALL[0])
    }
}

/// Which metric series the chart plots (the "metric" control).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatMetric {
    Shown,
    Accepted,
    Dismissed,
    Superseded,
    Words,
}

impl StatMetric {
    /// Picker menu order (drives the metric NSPopUpButton's item list).
    pub const ALL: [StatMetric; 5] = [
        StatMetric::Shown,
        StatMetric::Accepted,
        StatMetric::Dismissed,
        StatMetric::Superseded,
        StatMetric::Words,
    ];

    /// Human-readable picker item title.
    pub fn label(self) -> &'static str {
        match self {
            StatMetric::Shown => "Shown",
            StatMetric::Accepted => "Accepted",
            StatMetric::Dismissed => "Dismissed",
            StatMetric::Superseded => "Superseded",
            StatMetric::Words => "Words",
        }
    }

    /// Decode a selected picker-row index; out-of-range clamps to the first item.
    pub fn from_index(index: usize) -> Self {
        *Self::ALL.get(index).unwrap_or(&Self::ALL[0])
    }

    /// Pull this metric's value out of a single day bucket.
    fn of(self, bucket: &DayBucket) -> usize {
        match self {
            StatMetric::Shown => bucket.counts.shown,
            StatMetric::Accepted => bucket.counts.accepted,
            StatMetric::Dismissed => bucket.counts.dismissed,
            StatMetric::Superseded => bucket.counts.superseded,
            StatMetric::Words => bucket.words,
        }
    }
}

/// A completion-lifecycle outcome worth counting (design spec §11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A ghost suggestion was shown to the user.
    Shown,
    /// The user accepted a completion; `words` is how many words it inserted
    /// (feeds the menu-bar "words completed" count).
    Accepted { words: usize },
    /// The user dismissed a shown suggestion (Esc / click away).
    Dismissed,
    /// A shown/pending suggestion was superseded by a newer request before the
    /// user acted on it.
    Superseded,
}

/// A snapshot of outcome counts over the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub shown: usize,
    pub accepted: usize,
    pub dismissed: usize,
    pub superseded: usize,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    at_ms: u64,
    outcome: Outcome,
}

/// Grow-only outcome totals for the current process session. Unlike the
/// 30-day window these are never pruned, so a >30-day session still
/// persists every outcome (review-c102 undercount fix) — the lifetime
/// persistence path writes baseline + THESE, never window-derived counts
/// (which regress once pruning starts).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionTotals {
    pub counts: Counts,
    /// Total words across all `Accepted` outcomes this session.
    pub words: usize,
}

/// Rolling 30-day usage accumulator. Cheap to clone-free `record`; queries are
/// `O(n)` over the retained window, which stays small at human interaction rates.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    entries: VecDeque<Entry>,
    latencies: VecDeque<(u64, u32)>,
    session: SessionTotals,
}

impl Stats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an outcome at `now_ms`, then prune anything older than the window.
    /// The grow-only session totals update here too (never pruned).
    pub fn record(&mut self, now_ms: u64, outcome: Outcome) {
        match outcome {
            Outcome::Shown => self.session.counts.shown += 1,
            Outcome::Accepted { words } => {
                self.session.counts.accepted += 1;
                self.session.words += words;
            }
            Outcome::Dismissed => self.session.counts.dismissed += 1,
            Outcome::Superseded => self.session.counts.superseded += 1,
        }
        self.entries.push_back(Entry {
            at_ms: now_ms,
            outcome,
        });
        self.prune(now_ms);
    }

    /// Grow-only totals for this process session (persistence input; the
    /// windowed queries stay on `counts`/`words_completed`).
    pub fn session_totals(&self) -> SessionTotals {
        self.session
    }

    /// Record a first-suggestion latency sample (milliseconds) at `now_ms`.
    pub fn record_latency(&mut self, now_ms: u64, latency_ms: u32) {
        self.latencies.push_back((now_ms, latency_ms));
        self.prune(now_ms);
    }

    /// Drop entries that fell out of the trailing window ending at `now_ms`.
    /// Entries are appended in time order, so expired ones are always at the
    /// front — pop until the front is back inside the window.
    fn prune(&mut self, now_ms: u64) {
        let cutoff = now_ms.saturating_sub(WINDOW_MS);
        while self.entries.front().is_some_and(|e| e.at_ms < cutoff) {
            self.entries.pop_front();
        }
        while self.latencies.front().is_some_and(|&(at, _)| at < cutoff) {
            self.latencies.pop_front();
        }
    }

    fn cutoff(now_ms: u64) -> u64 {
        now_ms.saturating_sub(WINDOW_MS)
    }

    /// Outcome counts over the 30 days ending at `now_ms`.
    pub fn counts(&self, now_ms: u64) -> Counts {
        let cutoff = Self::cutoff(now_ms);
        let mut c = Counts::default();
        for e in self
            .entries
            .iter()
            .filter(|e| e.at_ms >= cutoff && e.at_ms <= now_ms)
        {
            match e.outcome {
                Outcome::Shown => c.shown += 1,
                Outcome::Accepted { .. } => c.accepted += 1,
                Outcome::Dismissed => c.dismissed += 1,
                Outcome::Superseded => c.superseded += 1,
            }
        }
        c
    }

    /// Total words accepted over the window (the menu-bar "words completed"
    /// figure).
    pub fn words_completed(&self, now_ms: u64) -> usize {
        let cutoff = Self::cutoff(now_ms);
        self.entries
            .iter()
            .filter(|e| e.at_ms >= cutoff && e.at_ms <= now_ms)
            .filter_map(|e| match e.outcome {
                Outcome::Accepted { words } => Some(words),
                _ => None,
            })
            .sum()
    }

    /// Acceptance rate (accepted / shown) over the window, `None` when nothing
    /// was shown (avoids a divide-by-zero / meaningless 0%).
    pub fn acceptance_rate(&self, now_ms: u64) -> Option<f64> {
        let c = self.counts(now_ms);
        (c.shown > 0).then(|| c.accepted as f64 / c.shown as f64)
    }

    /// Mean first-suggestion latency (ms) over the window, `None` when no samples.
    pub fn latency_avg_ms(&self, now_ms: u64) -> Option<u32> {
        let cutoff = Self::cutoff(now_ms);
        // Single pass, no intermediate Vec: accumulate (sum, count) in u64 so a
        // window of large samples never overflows.
        let (sum, count) = self
            .latencies
            .iter()
            .filter(|&&(at, _)| at >= cutoff && at <= now_ms)
            .fold((0u64, 0u64), |(sum, count), &(_, ms)| {
                (sum + u64::from(ms), count + 1)
            });
        (count > 0).then(|| (sum / count) as u32)
    }

    /// 95th-percentile latency (ms, nearest-rank) over the window — the §11 hard
    /// floor metric (<500 ms p95). `None` when no samples.
    pub fn latency_p95_ms(&self, now_ms: u64) -> Option<u32> {
        self.latency_percentile_ms(now_ms, 95)
    }

    /// Nearest-rank percentile latency (ms) for `pct` in `1..=100`. Returns
    /// `None` when there are no samples or `pct == 0` (0 has no nearest rank);
    /// `pct > 100` clamps to the maximum sample.
    pub fn latency_percentile_ms(&self, now_ms: u64, pct: u8) -> Option<u32> {
        if pct == 0 {
            return None;
        }
        let cutoff = Self::cutoff(now_ms);
        let mut samples: Vec<u32> = self
            .latencies
            .iter()
            .filter(|&&(at, _)| at >= cutoff && at <= now_ms)
            .map(|&(_, ms)| ms)
            .collect();
        if samples.is_empty() {
            return None;
        }
        samples.sort_unstable();
        let n = samples.len();
        // Nearest-rank: rank = ceil(pct/100 * n), 1-based; clamp into range.
        // `saturating_mul` mirrors the `daily_buckets` cutoff guard — the product
        // is unreachably large here (window-pruned deque), but kept overflow-safe
        // for parity so neither percentile path can wrap.
        let rank = (pct as usize).saturating_mul(n).div_ceil(100);
        let idx = rank.clamp(1, n) - 1;
        Some(samples[idx])
    }

    /// Number of retained (un-pruned) entries. Pruning happens on write, so in a
    /// long idle period (no `record`/`record_latency` calls) this may include
    /// window-expired entries not yet dropped — it is a post-write memory bound,
    /// not a continuous time-based one. Queries always filter by window
    /// regardless, so expired entries never affect counts/latency results.
    pub fn retained_len(&self) -> usize {
        self.entries.len() + self.latencies.len()
    }

    /// Chart data for the Statistics pane: `days` trailing 24h slices ending
    /// at `now_ms`, oldest first. Slices are sliding (anchored to `now_ms`),
    /// not calendar days — timezone-free and deterministic like every other
    /// query here. An entry at exactly `now_ms` lands in the newest bucket.
    pub fn daily_buckets(&self, now_ms: u64, days: usize) -> Vec<DayBucket> {
        let mut buckets = vec![DayBucket::default(); days];
        if days == 0 {
            return buckets;
        }
        // saturating_mul: an absurd `days` must clamp the window, not wrap it
        // (review-c96 — wrap would land at days ≥ ~213k, theoretical only).
        let cutoff = now_ms.saturating_sub((days as u64).saturating_mul(DAY_MS));
        for e in self
            .entries
            .iter()
            .filter(|e| e.at_ms >= cutoff && e.at_ms <= now_ms)
        {
            let idx = (((e.at_ms - cutoff) / DAY_MS) as usize).min(days - 1);
            let b = &mut buckets[idx];
            match e.outcome {
                Outcome::Shown => b.counts.shown += 1,
                Outcome::Accepted { words } => {
                    b.counts.accepted += 1;
                    b.words += words;
                }
                Outcome::Dismissed => b.counts.dismissed += 1,
                Outcome::Superseded => b.counts.superseded += 1,
            }
        }
        buckets
    }

    /// Chart series for the Statistics pane's range/group/metric selector: the
    /// chosen `metric` over the `range`, grouped per `grouping`, oldest bar
    /// first. Daily yields one value per trailing 24h slice; Weekly sums every
    /// 7 slices (a trailing partial week is summed as-is). Feeds `sparkline`.
    pub fn metric_series(
        &self,
        now_ms: u64,
        range: StatRange,
        grouping: StatGrouping,
        metric: StatMetric,
    ) -> Vec<usize> {
        // Re-bucket by the grouping (the weekly chunk-of-7 rule lives once in
        // group_buckets), then project each bucket onto the chosen metric.
        group_buckets(&self.daily_buckets(now_ms, range.days()), grouping)
            .iter()
            .map(|b| metric.of(b))
            .collect()
    }

    /// One human-readable line for the menu bar (§11 "words completed" display):
    /// `"{words} words · {accepted} accepted ({rate}%)"` over the 30-day window.
    /// The rate is omitted when nothing was shown (no meaningless 0%), and a
    /// fully idle window reads as a friendly placeholder instead of zeros.
    pub fn summary_line(&self, now_ms: u64) -> String {
        let counts = self.counts(now_ms);
        let words = self.words_completed(now_ms);
        if counts == Counts::default() && words == 0 {
            return "No completions in the last 30 days".to_string();
        }
        let mut line = format!("{words} words · {} accepted", counts.accepted);
        if let Some(rate) = self.acceptance_rate(now_ms) {
            line.push_str(&format!(" ({:.0}%)", rate * 100.0));
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_000_000_000_000; // a fixed base timestamp

    #[test]
    fn session_totals_survive_window_pruning() {
        // The review-c102 undercount: window-derived counts REGRESS once a
        // >30-day session starts pruning, so a periodic persist fed from
        // counts(now) would write SMALLER totals than an earlier flush.
        // Session totals are grow-only — every outcome of the process
        // lifetime persists.
        let mut s = Stats::new();
        s.record(T0, Outcome::Shown);
        s.record(T0, Outcome::Accepted { words: 3 });
        let late = T0 + WINDOW_MS + 10_000;
        s.record(late, Outcome::Dismissed);

        // The window dropped the early entries...
        let windowed = s.counts(late);
        assert_eq!(windowed.shown, 0);
        assert_eq!(windowed.accepted, 0);
        assert_eq!(windowed.dismissed, 1);
        // ...but the session totals kept them.
        let totals = s.session_totals();
        assert_eq!(totals.counts.shown, 1);
        assert_eq!(totals.counts.accepted, 1);
        assert_eq!(totals.counts.dismissed, 1);
        assert_eq!(totals.words, 3);
    }

    #[test]
    fn session_totals_accumulate_counts_and_words_only_from_outcomes() {
        let mut s = Stats::new();
        assert_eq!(s.session_totals(), SessionTotals::default());
        s.record(T0, Outcome::Shown);
        s.record(T0, Outcome::Accepted { words: 2 });
        s.record(T0, Outcome::Accepted { words: 5 });
        s.record(T0, Outcome::Superseded);
        // Latencies are not persisted — they must not touch the totals.
        s.record_latency(T0, 42);

        let totals = s.session_totals();
        assert_eq!(totals.counts.shown, 1);
        assert_eq!(totals.counts.accepted, 2);
        assert_eq!(totals.counts.superseded, 1);
        assert_eq!(totals.counts.dismissed, 0);
        assert_eq!(totals.words, 7);
    }

    #[test]
    fn persisted_stats_round_trip_and_merge_session_counts() {
        // Shutdown persistence (T3): lifetime totals survive render→parse
        // unchanged; merging a session adds counts and words on top.
        let lifetime = PersistedStats {
            shown: 100,
            accepted: 40,
            dismissed: 10,
            superseded: 25,
            words: 320,
        };
        assert_eq!(parse_stats_file(&render_stats_file(&lifetime)), lifetime);

        let session = Counts {
            shown: 5,
            accepted: 2,
            dismissed: 1,
            superseded: 0,
        };
        let merged = lifetime.merged(session, 17);
        assert_eq!(merged.shown, 105);
        assert_eq!(merged.accepted, 42);
        assert_eq!(merged.dismissed, 11);
        assert_eq!(merged.superseded, 25);
        assert_eq!(merged.words, 337);

        // Missing file / malformed values fail soft to zero — never panic.
        assert_eq!(parse_stats_file(""), PersistedStats::default());
        assert_eq!(
            parse_stats_file("STATS_SHOWN=abc\nGARBAGE\nSTATS_WORDS=7"),
            PersistedStats {
                words: 7,
                ..PersistedStats::default()
            }
        );
    }

    #[test]
    fn parse_stats_tolerates_whitespace_around_key_and_value() {
        // parse_stats_file trims both the key (`key.trim()`) and the value
        // (`value.trim().parse()`), so a dotenv line with spaces around `=`
        // parses identically to the canonical no-space form. A hand-edited
        // stats.env must not silently reset a counter just because someone
        // added a space.
        let spaced = parse_stats_file("STATS_SHOWN = 5");
        assert_eq!(spaced.shown, 5);
        assert_eq!(spaced, parse_stats_file("STATS_SHOWN=5"));

        // Tolerance holds across every key, with assorted surrounding/tab/CR
        // whitespace, and the trailing carriage return `lines()` leaves on a
        // CRLF file.
        let messy = "  STATS_SHOWN =  5\n\
                     STATS_ACCEPTED\t=\t2 \n\
                     STATS_DISMISSED = 1\r\n\
                     STATS_SUPERSEDED =3\n\
                     STATS_WORDS= 9 \n";
        assert_eq!(
            parse_stats_file(messy),
            PersistedStats {
                shown: 5,
                accepted: 2,
                dismissed: 1,
                superseded: 3,
                words: 9,
            }
        );
    }

    #[test]
    fn persisted_stats_merge_saturates_huge_lifetime_counters() {
        let lifetime = PersistedStats {
            shown: u64::MAX,
            accepted: u64::MAX,
            dismissed: u64::MAX,
            superseded: u64::MAX,
            words: u64::MAX,
        };
        let session = Counts {
            shown: 1,
            accepted: 1,
            dismissed: 1,
            superseded: 1,
        };

        assert_eq!(
            lifetime.merged(session, 1),
            PersistedStats {
                shown: u64::MAX,
                accepted: u64::MAX,
                dismissed: u64::MAX,
                superseded: u64::MAX,
                words: u64::MAX,
            }
        );
    }

    #[test]
    fn sparkline_scales_bars_to_the_series_maximum() {
        // Statistics-pane chart row: one glyph per day, ceiling-scaled so the
        // max value always renders full-height, zero renders the baseline, and
        // any nonzero value visibly rises above it.
        assert_eq!(sparkline(&[0, 1, 4, 8]), "▁▂▅█");
        assert_eq!(sparkline(&[2, 2]), "██"); // every max is full-height
        assert_eq!(sparkline(&[0, 0]), "▁▁"); // all-zero series stays baseline
        assert_eq!(sparkline(&[]), "");
    }

    #[test]
    fn sparkline_keeps_a_small_nonzero_day_above_the_idle_baseline() {
        // Invariant guard (lib.rs §sparkline): any nonzero value must render at
        // least one bar ABOVE the idle baseline (bar index >= 1) even when the
        // series max dwarfs it — a sparse day stays visibly different from an
        // idle one. The ceiling division (`div_ceil`) is what enforces this; a
        // ceil→round regression would flatten v=1 against a max of 100/1000 back
        // down to the baseline glyph ▁. BARS = ['▁','▂',...,'█'], so index 0 is
        // ▁ (idle) and index 1 is ▂ (the smallest nonzero rendering).
        assert_eq!(sparkline(&[0, 1, 100]), "▁▂█");
        assert_eq!(sparkline(&[1, 1000]), "▂█");
    }

    #[test]
    fn sparkline_handles_usize_max_without_overflow() {
        assert_eq!(sparkline(&[0, usize::MAX]), "▁█");
        assert_eq!(sparkline(&[usize::MAX, usize::MAX]), "██");
    }

    #[test]
    fn daily_buckets_split_the_window_into_trailing_day_slices() {
        // Statistics-pane chart data: `days` trailing 24h slices ending at
        // `now_ms`, oldest first, each with outcome counts + accepted words.
        let mut s = Stats::new();
        let now = T0 + 10 * DAY_MS;
        s.record(now, Outcome::Accepted { words: 3 }); // most recent slice
        s.record(now - DAY_MS - 1, Outcome::Shown); // middle slice
        s.record(now - 3 * DAY_MS + 1, Outcome::Dismissed); // oldest slice
        let buckets = s.daily_buckets(now, 3);
        assert_eq!(buckets.len(), 3);
        assert_eq!(buckets[2].counts.accepted, 1);
        assert_eq!(buckets[2].words, 3);
        assert_eq!(buckets[1].counts.shown, 1);
        assert_eq!(buckets[1].words, 0);
        assert_eq!(buckets[0].counts.dismissed, 1);
        assert_eq!(buckets[0].counts.shown, 0);
    }

    #[test]
    fn daily_buckets_exclude_entries_older_than_the_requested_days() {
        // Retained 30-day entries older than the asked-for span must not
        // leak into the oldest bucket; an entry exactly at the cutoff is in.
        let mut s = Stats::new();
        let now = T0 + 10 * DAY_MS;
        s.record(now - 2 * DAY_MS - 1, Outcome::Accepted { words: 9 }); // outside 2-day span
        s.record(now - 2 * DAY_MS, Outcome::Shown); // exactly at the cutoff
        let buckets = s.daily_buckets(now, 2);
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].counts.shown, 1);
        assert_eq!(buckets[0].counts.accepted, 0);
        assert_eq!(buckets[0].words, 0);
        assert!(s.daily_buckets(now, 0).is_empty());
    }

    #[test]
    fn daily_buckets_day_boundary_is_exact_at_the_slice_edge() {
        // The slice index is `(at - cutoff) / DAY_MS`. Pin the boundary
        // precisely: the last millisecond of day 0 (cutoff + DAY_MS - 1) stays
        // in bucket 0, and the first millisecond of day 1 (cutoff + DAY_MS)
        // crosses into bucket 1. cutoff = now - days*DAY_MS.
        let mut s = Stats::new();
        let days = 3;
        let now = T0 + days as u64 * DAY_MS;
        let cutoff = now - days as u64 * DAY_MS; // == T0
        s.record(cutoff + DAY_MS - 1, Outcome::Shown); // last ms of bucket 0
        s.record(cutoff + DAY_MS, Outcome::Dismissed); // first ms of bucket 1
        let buckets = s.daily_buckets(now, days);
        assert_eq!(buckets.len(), days);
        assert_eq!(
            buckets[0].counts.shown, 1,
            "cutoff+DAY_MS-1 lands in bucket 0"
        );
        assert_eq!(buckets[0].counts.dismissed, 0);
        assert_eq!(
            buckets[1].counts.dismissed, 1,
            "cutoff+DAY_MS lands in bucket 1"
        );
        assert_eq!(buckets[1].counts.shown, 0);
    }

    #[test]
    fn daily_buckets_coalesce_same_window_events_into_one_slice() {
        // Two events inside the SAME 24h slice must land in the SAME bucket
        // index (slice = (at - cutoff) / DAY_MS), while a third event in a
        // different 24h window lands in a DIFFERENT index. cutoff = now -
        // days*DAY_MS == T0 here, so the newest slice is bucket `days-1`.
        let mut s = Stats::new();
        let days = 3;
        let now = T0 + days as u64 * DAY_MS;
        s.record(now, Outcome::Shown); // newest slice (bucket 2)
        s.record(now - 1, Outcome::Shown); // same 24h slice -> same bucket 2
        s.record(now - DAY_MS - 1, Outcome::Shown); // previous slice -> bucket 1
        let buckets = s.daily_buckets(now, days);
        assert_eq!(buckets.len(), days);
        assert_eq!(
            buckets[2].counts.shown, 2,
            "two same-window events coalesce into the newest slice"
        );
        assert_eq!(
            buckets[1].counts.shown, 1,
            "the third event falls in a distinct, older slice"
        );
        assert_eq!(buckets[0].counts.shown, 0);
    }

    #[test]
    fn cutoff_saturates_when_now_is_inside_the_first_window() {
        // When `now_ms < WINDOW_MS`, `now - WINDOW_MS` would underflow; the
        // saturating cutoff clamps to 0 so every recorded entry is retained
        // and queryable rather than wrapping to a huge cutoff that hides them.
        let mut s = Stats::new();
        let now = 5_000; // far below WINDOW_MS (~2.6 billion ms)
        s.record(0, Outcome::Shown); // at epoch 0
        s.record(1, Outcome::Accepted { words: 4 });
        s.record(now, Outcome::Shown);
        s.record_latency(2, 80);

        // No underflow: all in-window entries counted, none dropped.
        assert_eq!(
            s.counts(now),
            Counts {
                shown: 2,
                accepted: 1,
                dismissed: 0,
                superseded: 0,
            }
        );
        assert_eq!(s.words_completed(now), 4);
        assert_eq!(s.latency_avg_ms(now), Some(80));
        // daily_buckets shares the same saturating-cutoff guard.
        let buckets = s.daily_buckets(now, 30);
        let total_shown: usize = buckets.iter().map(|b| b.counts.shown).sum();
        assert_eq!(total_shown, 2, "no entry lost to an underflowed cutoff");
    }

    #[test]
    fn summary_line_formats_words_accepts_and_rate() {
        let mut s = Stats::new();
        s.record(T0, Outcome::Shown);
        s.record(T0, Outcome::Shown);
        s.record(T0, Outcome::Shown);
        s.record(T0, Outcome::Shown);
        s.record(T0, Outcome::Accepted { words: 3 });
        s.record(T0, Outcome::Accepted { words: 5 });
        // 8 words, 2 accepted of 4 shown = 50%.
        assert_eq!(s.summary_line(T0), "8 words · 2 accepted (50%)");
    }

    #[test]
    fn summary_line_shows_counts_when_shown_but_zero_words() {
        // The idle placeholder guard is `counts == Counts::default() && words == 0`
        // (AND, not OR). Two Shown events make counts non-default while words stays
        // 0, so the AND is false and the real formatted line renders — NOT the
        // "No completions" placeholder. A `||` mutant would wrongly short-circuit
        // to the placeholder here. acceptance_rate is 0/2 = Some(0.0) → "(0%)".
        let mut s = Stats::new();
        s.record(T0, Outcome::Shown);
        s.record(T0, Outcome::Shown);
        assert_eq!(s.summary_line(T0), "0 words · 0 accepted (0%)");
    }

    #[test]
    fn daily_buckets_exclude_strictly_future_entries() {
        // daily_buckets filters `e.at_ms >= cutoff && e.at_ms <= now_ms`. A
        // strictly-future entry (clock skew, at_ms > now_ms) must NOT land in the
        // newest bucket — the `<= now_ms` upper bound excludes it.
        let mut s = Stats::new();
        let now = T0 + 5 * DAY_MS;
        s.record(now + DAY_MS, Outcome::Shown); // strictly future
        let buckets = s.daily_buckets(now, 2);
        assert_eq!(buckets.len(), 2);
        assert_eq!(
            buckets[1].counts.shown, 0,
            "future entry must not land in the newest bucket"
        );
        assert_eq!(buckets[0].counts.shown, 0);
    }

    #[test]
    fn daily_buckets_place_a_now_aligned_entry_in_the_newest_bucket() {
        // An entry recorded at exactly `now_ms` sits at `(now - cutoff)/DAY_MS`
        // = `days*DAY_MS/DAY_MS` = `days`, which is one past the last index.
        // The `.min(days - 1)` guard clamps it into the newest bucket instead of
        // panicking on an out-of-bounds index. Pin both the length and placement.
        let mut s = Stats::new();
        let now = T0 + 30 * DAY_MS;
        s.record(now, Outcome::Accepted { words: 7 });
        let buckets = s.daily_buckets(now, 7);
        assert_eq!(buckets.len(), 7);
        assert_eq!(
            buckets[6].counts.accepted, 1,
            "now-aligned entry clamps into the newest (last) bucket"
        );
        assert_eq!(buckets[6].words, 7);
        // It must not double-count into any earlier bucket.
        let earlier_accepted: usize = buckets[..6].iter().map(|b| b.counts.accepted).sum();
        assert_eq!(earlier_accepted, 0);
    }

    #[test]
    fn summary_line_reads_as_placeholder_when_idle() {
        assert_eq!(
            Stats::new().summary_line(T0),
            "No completions in the last 30 days"
        );
        // Entries older than the window roll out → placeholder again.
        let mut s = Stats::new();
        s.record(T0, Outcome::Accepted { words: 4 });
        assert_eq!(
            s.summary_line(T0 + WINDOW_MS + 1),
            "No completions in the last 30 days"
        );
    }

    #[test]
    fn summary_line_omits_the_rate_when_nothing_was_shown() {
        // Accepts without shown events (e.g. local replacements accepted via
        // a path that never emitted Shown) must not render a bogus percent.
        let mut s = Stats::new();
        s.record(T0, Outcome::Accepted { words: 2 });
        assert_eq!(s.summary_line(T0), "2 words · 1 accepted");
    }

    #[test]
    fn summary_line_rounds_the_rate_to_a_whole_percent() {
        // The rate is formatted with {:.0} (rounds), not truncated: 3 of 8 shown
        // = 37.5% must render "38%" — truncation would wrongly show "37%". Pins
        // the menu-bar text against a switch to {:.1}/{:.2} or integer truncation.
        let mut s = Stats::new();
        for _ in 0..8 {
            s.record(T0, Outcome::Shown);
        }
        for _ in 0..3 {
            s.record(T0, Outcome::Accepted { words: 1 });
        }
        assert_eq!(s.summary_line(T0), "3 words · 3 accepted (38%)");
    }

    #[test]
    fn empty_stats_are_all_zero_and_none() {
        let s = Stats::new();
        assert_eq!(s.counts(T0), Counts::default());
        assert_eq!(s.words_completed(T0), 0);
        assert_eq!(s.acceptance_rate(T0), None);
        assert_eq!(s.latency_avg_ms(T0), None);
        assert_eq!(s.latency_p95_ms(T0), None);
    }

    #[test]
    fn counts_each_outcome_kind() {
        let mut s = Stats::new();
        s.record(T0, Outcome::Shown);
        s.record(T0, Outcome::Shown);
        s.record(T0, Outcome::Accepted { words: 3 });
        s.record(T0, Outcome::Dismissed);
        s.record(T0, Outcome::Superseded);
        assert_eq!(
            s.counts(T0),
            Counts {
                shown: 2,
                accepted: 1,
                dismissed: 1,
                superseded: 1,
            }
        );
    }

    #[test]
    fn words_completed_sums_accepted_word_counts() {
        let mut s = Stats::new();
        s.record(T0, Outcome::Accepted { words: 4 });
        s.record(T0, Outcome::Accepted { words: 1 });
        s.record(T0, Outcome::Dismissed); // not an accept → ignored
        assert_eq!(s.words_completed(T0), 5);
    }

    #[test]
    fn acceptance_rate_is_accepted_over_shown() {
        let mut s = Stats::new();
        for _ in 0..4 {
            s.record(T0, Outcome::Shown);
        }
        s.record(T0, Outcome::Accepted { words: 1 });
        assert_eq!(s.acceptance_rate(T0), Some(0.25));
    }

    #[test]
    fn entries_outside_the_window_are_excluded_from_queries() {
        let mut s = Stats::new();
        s.record(T0, Outcome::Shown);
        s.record(T0, Outcome::Accepted { words: 7 });
        // Query 31 days later: the old entries are outside the 30-day window.
        let later = T0 + WINDOW_MS + 1;
        assert_eq!(s.counts(later), Counts::default());
        assert_eq!(s.words_completed(later), 0);
    }

    #[test]
    fn future_entries_are_excluded_from_window_queries() {
        let mut s = Stats::new();
        s.record(T0, Outcome::Shown);
        s.record(T0, Outcome::Accepted { words: 2 });
        s.record(T0 + 1, Outcome::Accepted { words: 99 });
        s.record_latency(T0, 20);
        s.record_latency(T0 + 1, 200);

        assert_eq!(
            s.counts(T0),
            Counts {
                shown: 1,
                accepted: 1,
                dismissed: 0,
                superseded: 0,
            }
        );
        assert_eq!(s.words_completed(T0), 2);
        assert_eq!(s.acceptance_rate(T0), Some(1.0));
        assert_eq!(s.latency_avg_ms(T0), Some(20));
        assert_eq!(s.latency_p95_ms(T0), Some(20));
    }

    #[test]
    fn the_window_boundary_is_inclusive_at_exactly_30_days() {
        let mut s = Stats::new();
        s.record(T0, Outcome::Shown);
        // Exactly at the window edge (now - WINDOW_MS == T0) is still counted.
        let edge = T0 + WINDOW_MS;
        assert_eq!(s.counts(edge).shown, 1);
        // One ms past the edge drops it.
        assert_eq!(s.counts(edge + 1).shown, 0);
    }

    #[test]
    fn recording_past_the_window_prunes_old_entries_to_bound_memory() {
        let mut s = Stats::new();
        s.record(T0, Outcome::Shown);
        s.record(T0, Outcome::Shown);
        assert_eq!(s.retained_len(), 2);
        // A later record prunes the now-expired ones on write.
        s.record(T0 + WINDOW_MS + 1, Outcome::Shown);
        assert_eq!(s.retained_len(), 1);
    }

    #[test]
    fn latency_avg_is_the_mean_over_the_window() {
        let mut s = Stats::new();
        s.record_latency(T0, 10);
        s.record_latency(T0, 20);
        s.record_latency(T0, 30);
        assert_eq!(s.latency_avg_ms(T0), Some(20));
    }

    #[test]
    fn latency_p95_nearest_rank() {
        let mut s = Stats::new();
        // 1..=20 → p95 nearest-rank: ceil(0.95*20)=19 → 19th value (1-based) = 19.
        for ms in 1..=20u32 {
            s.record_latency(T0, ms);
        }
        assert_eq!(s.latency_p95_ms(T0), Some(19));
        // p100 is the max; p1 of this set is the min.
        assert_eq!(s.latency_percentile_ms(T0, 100), Some(20));
        assert_eq!(s.latency_percentile_ms(T0, 1), Some(1));
    }

    #[test]
    fn latency_samples_outside_the_window_are_excluded() {
        let mut s = Stats::new();
        s.record_latency(T0, 500);
        let later = T0 + WINDOW_MS + 1;
        assert_eq!(s.latency_avg_ms(later), None);
        assert_eq!(s.latency_p95_ms(later), None);
    }

    #[test]
    fn percentile_single_sample_is_that_sample_for_every_pct() {
        // n=1 is where the div_ceil/clamp index math is most fragile.
        let mut s = Stats::new();
        s.record_latency(T0, 42);
        for pct in [1u8, 50, 95, 100] {
            assert_eq!(s.latency_percentile_ms(T0, pct), Some(42));
        }
    }

    #[test]
    fn percentile_small_odd_n_nearest_rank() {
        // Unsorted [30,10,20]: p50 → rank ceil(0.5*3)=2 → 2nd value = 20.
        let mut s = Stats::new();
        s.record_latency(T0, 30);
        s.record_latency(T0, 10);
        s.record_latency(T0, 20);
        assert_eq!(s.latency_percentile_ms(T0, 50), Some(20));
        assert_eq!(s.latency_percentile_ms(T0, 100), Some(30));
        assert_eq!(s.latency_percentile_ms(T0, 1), Some(10));
    }

    #[test]
    fn percentile_even_n_two_nearest_rank() {
        // n=2 nearest-rank: sorted [a,b]. p50 → rank ceil(0.5*2)=1 → idx0=a;
        // p100 → rank ceil(1.0*2)=2 → idx1=b. Pins the even-n boundary between
        // the odd-n (n=3) and single-sample (n=1) cases.
        let mut s = Stats::new();
        s.record_latency(T0, 70); // out of order to prove sorting
        s.record_latency(T0, 30);
        assert_eq!(s.latency_percentile_ms(T0, 50), Some(30)); // idx 0
        assert_eq!(s.latency_percentile_ms(T0, 100), Some(70)); // idx 1
    }

    #[test]
    fn percentile_zero_is_none_and_above_100_clamps_to_max() {
        let mut s = Stats::new();
        for ms in [10u32, 20, 30] {
            s.record_latency(T0, ms);
        }
        assert_eq!(s.latency_percentile_ms(T0, 0), None); // 0 has no nearest rank
        assert_eq!(s.latency_percentile_ms(T0, 200), Some(30)); // clamps to max
    }

    #[test]
    fn acceptance_rate_is_none_when_nothing_shown_even_with_accepts() {
        // The guard's actual purpose: no divide-by-zero / Inf when accepts exist
        // but nothing was recorded as Shown.
        let mut s = Stats::new();
        s.record(T0, Outcome::Accepted { words: 2 });
        s.record(T0, Outcome::Accepted { words: 1 });
        assert_eq!(s.acceptance_rate(T0), None);
    }

    #[test]
    fn acceptance_rate_can_exceed_one_when_accepts_exceed_shown() {
        // Word-accepts can outnumber Shown events; the rate is not clamped.
        let mut s = Stats::new();
        s.record(T0, Outcome::Shown);
        s.record(T0, Outcome::Shown);
        for _ in 0..3 {
            s.record(T0, Outcome::Accepted { words: 1 });
        }
        assert_eq!(s.acceptance_rate(T0), Some(1.5));
    }

    #[test]
    fn latency_avg_floor_divides() {
        // [10,20,20] = 50/3 floors to 16; [10,20,21] = 51/3 = 17 divides
        // exactly — both directions of the floor-division contract.
        let mut s = Stats::new();
        for ms in [10u32, 20, 20] {
            s.record_latency(T0, ms);
        }
        assert_eq!(s.latency_avg_ms(T0), Some(16));

        let mut exact = Stats::new();
        for ms in [10u32, 20, 21] {
            exact.record_latency(T0, ms);
        }
        assert_eq!(exact.latency_avg_ms(T0), Some(17));
    }

    #[test]
    fn latency_partial_window_keeps_survivors() {
        let mut s = Stats::new();
        s.record_latency(T0, 100);
        s.record_latency(T0 + WINDOW_MS, 200);
        // At the edge both are in-window → mean 150.
        assert_eq!(s.latency_avg_ms(T0 + WINDOW_MS), Some(150));
        // One ms past drops only the T0 sample → just 200 survives.
        assert_eq!(s.latency_avg_ms(T0 + WINDOW_MS + 1), Some(200));
    }

    #[test]
    fn latency_avg_does_not_overflow_on_many_large_samples() {
        // latency_avg_ms accumulates the sample sum in a u64 (lib.rs L411), so
        // summing several near-u32::MAX samples never overflows. A naive u32 sum
        // would wrap: 5 × ~4.29e9 ≈ 2.1e10 ≫ u32::MAX. Feed five fixed large
        // samples whose true mean is exactly representable as a u32 and assert it.
        let mut s = Stats::new();
        let samples = [
            u32::MAX,
            u32::MAX - 4,
            u32::MAX - 6,
            u32::MAX - 10,
            u32::MAX - 5,
        ];
        for &ms in &samples {
            s.record_latency(T0, ms);
        }
        // sum = 5*u32::MAX - (0+4+6+10+5) = 5*4_294_967_295 - 25 = 21_474_836_450.
        // mean = 21_474_836_450 / 5 = 4_294_967_290 = u32::MAX - 5, in u32 range.
        let expected: u64 = samples.iter().map(|&ms| ms as u64).sum::<u64>() / samples.len() as u64;
        assert_eq!(expected, (u32::MAX - 5) as u64); // sanity-pin the arithmetic
        assert_eq!(s.latency_avg_ms(T0), Some(u32::MAX - 5));
    }

    #[test]
    fn latency_window_boundary_is_inclusive() {
        let mut s = Stats::new();
        s.record_latency(T0, 42);
        assert_eq!(s.latency_avg_ms(T0 + WINDOW_MS), Some(42));
        assert_eq!(s.latency_avg_ms(T0 + WINDOW_MS + 1), None);
    }

    #[test]
    fn prune_bounds_the_latency_deque_too() {
        let mut s = Stats::new();
        s.record_latency(T0, 1);
        s.record_latency(T0, 2);
        assert_eq!(s.retained_len(), 2);
        s.record_latency(T0 + WINDOW_MS + 1, 3); // prunes the two expired samples
        assert_eq!(s.retained_len(), 1);
    }

    #[test]
    fn counts_slide_as_the_query_window_advances() {
        // Interleaved records across time, queried with a moving window.
        let mut s = Stats::new();
        s.record(T0, Outcome::Shown);
        s.record(T0 + 15 * DAY_MS, Outcome::Shown);
        s.record(T0 + 29 * DAY_MS, Outcome::Shown);
        assert_eq!(s.counts(T0 + 29 * DAY_MS).shown, 3);
        // Advancing past T0+30d drops the oldest from the window.
        assert_eq!(s.counts(T0 + WINDOW_MS + 1).shown, 2);
    }

    #[test]
    fn metric_series_daily_selects_the_chosen_metric_per_day() {
        // The Statistics-pane range/group/metric control: a Daily grouping over
        // the Last7Days range yields one value per trailing 24h slice, oldest
        // first, for the chosen metric.
        let mut s = Stats::new();
        let now = T0 + 7 * DAY_MS;
        s.record(now, Outcome::Accepted { words: 5 }); // newest slice (idx 6)
        s.record(now - DAY_MS - 1, Outcome::Accepted { words: 2 }); // idx 5
        s.record(now - DAY_MS - 1, Outcome::Shown); // idx 5
        s.record(now - 2 * DAY_MS - 1, Outcome::Dismissed); // idx 4
        s.record(now - 2 * DAY_MS - 1, Outcome::Superseded); // idx 4

        assert_eq!(
            s.metric_series(
                now,
                StatRange::Last7Days,
                StatGrouping::Daily,
                StatMetric::Accepted
            ),
            vec![0, 0, 0, 0, 0, 1, 1],
        );
        assert_eq!(
            s.metric_series(
                now,
                StatRange::Last7Days,
                StatGrouping::Daily,
                StatMetric::Dismissed
            ),
            vec![0, 0, 0, 0, 1, 0, 0],
        );
        assert_eq!(
            s.metric_series(
                now,
                StatRange::Last7Days,
                StatGrouping::Daily,
                StatMetric::Superseded
            ),
            vec![0, 0, 0, 0, 1, 0, 0],
        );
        assert_eq!(
            s.metric_series(
                now,
                StatRange::Last7Days,
                StatGrouping::Daily,
                StatMetric::Words
            ),
            vec![0, 0, 0, 0, 0, 2, 5],
        );
        assert_eq!(
            s.metric_series(
                now,
                StatRange::Last7Days,
                StatGrouping::Daily,
                StatMetric::Shown
            ),
            vec![0, 0, 0, 0, 0, 1, 0],
        );
    }

    #[test]
    fn metric_series_weekly_sums_each_seven_day_group_oldest_first() {
        // Weekly grouping sums every 7 trailing daily slices into one bar,
        // oldest group first; a trailing partial group is summed as-is. Use
        // NON-uniform per-week counts plus events straddling the week boundary
        // (slice 6 = last of week 0, slice 7 = first of week 1) so a chunk
        // off-by-one would shift counts across bars and fail. cutoff = now -
        // 14*DAY_MS == T0; slice index = (at - cutoff) / DAY_MS.
        let mut s = Stats::new();
        let now = T0 + 14 * DAY_MS;
        // Week 0 = slices 0..=6 -> 3 accepts (distinct from week 1's count).
        s.record(now - 14 * DAY_MS, Outcome::Accepted { words: 1 }); // slice 0
        s.record(now - 10 * DAY_MS, Outcome::Accepted { words: 1 }); // slice 4
        s.record(now - 8 * DAY_MS, Outcome::Accepted { words: 1 }); // slice 6 (last of week 0)
                                                                    // Week 1 = slices 7..=13 -> 2 accepts; first lands exactly on slice 7.
        s.record(now - 7 * DAY_MS, Outcome::Accepted { words: 1 }); // slice 7 (first of week 1)
        s.record(now, Outcome::Accepted { words: 1 }); // slice 13

        assert_eq!(
            s.metric_series(
                now,
                StatRange::Last14Days,
                StatGrouping::Weekly,
                StatMetric::Accepted
            ),
            vec![3, 2],
        );
    }

    #[test]
    fn metric_series_weekly_30day_range_groups_into_five_buckets() {
        // 30 daily slices → chunks of 7 → 4 full weeks + a trailing 2-day group.
        let s = Stats::new();
        let series = s.metric_series(
            T0 + WINDOW_MS,
            StatRange::Last30Days,
            StatGrouping::Weekly,
            StatMetric::Dismissed,
        );
        assert_eq!(series.len(), 5);
        assert!(series.iter().all(|&v| v == 0));
    }

    #[test]
    fn metric_series_weekly_30day_range_sums_nonzero_data_into_the_right_weeks() {
        // The existing 30-day weekly test only pins the bucket COUNT with
        // all-zero data. This one places non-zero data in DISTINCT weeks and
        // asserts each weekly bucket sums the right values (the chunk-of-7
        // aggregation, not just its shape). now = T0 + 30 days → cutoff = T0,
        // so a day-index `d` slice is `[T0 + d*DAY_MS, T0 + (d+1)*DAY_MS)`.
        // Weeks: w0=idx0-6, w1=7-13, w2=14-20, w3=21-27, w4=28-29 (partial).
        let mut s = Stats::new();
        let now = T0 + 30 * DAY_MS;
        // Week 0: two accepts (idx 2 and idx 5) → accepted=2, words=3+4=7.
        s.record(T0 + 2 * DAY_MS + 1, Outcome::Accepted { words: 3 });
        s.record(T0 + 5 * DAY_MS + 1, Outcome::Accepted { words: 4 });
        // Week 2: one accept (idx 14) → accepted=1, words=10.
        s.record(T0 + 14 * DAY_MS + 1, Outcome::Accepted { words: 10 });
        // Week 4 (partial trailing group): one accept at the newest slice
        // (idx 29 == now) → accepted=1, words=6.
        s.record(now, Outcome::Accepted { words: 6 });

        // Accepted-count series sums per week; weeks 1 and 3 stay empty.
        assert_eq!(
            s.metric_series(
                now,
                StatRange::Last30Days,
                StatGrouping::Weekly,
                StatMetric::Accepted
            ),
            vec![2, 0, 1, 0, 1],
        );
        // Words series sums the per-accept word totals into the same weeks.
        assert_eq!(
            s.metric_series(
                now,
                StatRange::Last30Days,
                StatGrouping::Weekly,
                StatMetric::Words
            ),
            vec![7, 0, 10, 0, 6],
        );
    }

    #[test]
    fn stat_range_days_are_fixed_spans() {
        assert_eq!(StatRange::Last7Days.days(), 7);
        assert_eq!(StatRange::Last14Days.days(), 14);
        assert_eq!(StatRange::Last30Days.days(), 30);
    }

    #[test]
    fn stat_picker_enums_expose_menu_order_labels_and_index_decode() {
        // The Statistics-pane range/group/metric pickers (NSPopUpButtons) drive
        // these enums: ALL is the menu order, label() the item title, and
        // from_index decodes the selected row (OOB clamps to the first item,
        // like the model picker's total-over-OOB selection).
        assert_eq!(
            StatRange::ALL,
            [
                StatRange::Last7Days,
                StatRange::Last14Days,
                StatRange::Last30Days
            ]
        );
        assert_eq!(
            StatGrouping::ALL,
            [StatGrouping::Daily, StatGrouping::Weekly]
        );
        assert_eq!(
            StatMetric::ALL,
            [
                StatMetric::Shown,
                StatMetric::Accepted,
                StatMetric::Dismissed,
                StatMetric::Superseded,
                StatMetric::Words
            ]
        );

        // Labels are human-readable and, per metric, distinct (no two menu rows
        // can share a title).
        assert_eq!(StatRange::Last30Days.label(), "Last 30 days");
        assert_eq!(StatGrouping::Weekly.label(), "Weekly");
        assert_eq!(StatMetric::Accepted.label(), "Accepted");
        let labels: Vec<&str> = StatMetric::ALL.iter().map(|m| m.label()).collect();
        let unique: std::collections::HashSet<&str> = labels.iter().copied().collect();
        assert_eq!(unique.len(), labels.len());

        // from_index round-trips ALL and clamps out-of-range to the first item.
        for (i, &r) in StatRange::ALL.iter().enumerate() {
            assert_eq!(StatRange::from_index(i), r);
        }
        for (i, &g) in StatGrouping::ALL.iter().enumerate() {
            assert_eq!(StatGrouping::from_index(i), g);
        }
        for (i, &m) in StatMetric::ALL.iter().enumerate() {
            assert_eq!(StatMetric::from_index(i), m);
        }
        assert_eq!(StatRange::from_index(99), StatRange::Last7Days);
        assert_eq!(StatGrouping::from_index(99), StatGrouping::Daily);
        assert_eq!(StatMetric::from_index(99), StatMetric::Shown);
    }

    #[test]
    fn group_buckets_weekly_sums_each_seven_day_chunk_oldest_first() {
        // The Statistics grouping picker: Daily returns the buckets unchanged;
        // Weekly sums every 7 trailing slices (oldest group first, trailing
        // partial group summed as-is) into one bucket per week.
        let mk = |shown, accepted, words| DayBucket {
            counts: Counts {
                shown,
                accepted,
                dismissed: 0,
                superseded: 0,
            },
            words,
        };
        let mut daily = vec![DayBucket::default(); 9]; // week0 = 0..6, week1 = 7..8
        daily[0] = mk(1, 0, 2);
        daily[6] = mk(0, 1, 3);
        daily[7] = mk(2, 0, 0);
        daily[8] = mk(0, 1, 5);

        // Daily grouping is the identity.
        assert_eq!(group_buckets(&daily, StatGrouping::Daily), daily);

        // Weekly collapses to one bucket per 7-day chunk, summing every field.
        let weekly = group_buckets(&daily, StatGrouping::Weekly);
        assert_eq!(weekly.len(), 2);
        assert_eq!(weekly[0], mk(1, 1, 5)); // sum of idx 0..6
        assert_eq!(weekly[1], mk(2, 1, 5)); // sum of idx 7..8 (partial week)

        // Empty input → empty (no panic on chunks of 0).
        assert!(group_buckets(&[], StatGrouping::Weekly).is_empty());
    }

    #[test]
    fn weekly_grouping_also_sums_dismissed_and_superseded() {
        // The sibling group_buckets test pins shown/accepted/words but leaves
        // dismissed/superseded at zero. group_buckets sums EVERY outcome count
        // across a 7-day chunk (lib.rs L128-129), so place non-zero dismissed
        // and superseded values in distinct days of one week and assert both are
        // summed into the single weekly bucket — a regression that dropped either
        // field from the chunk loop would slip past the existing coverage.
        let mut daily = vec![DayBucket::default(); 7];
        daily[0].counts.dismissed = 2;
        daily[3].counts.dismissed = 1;
        daily[1].counts.superseded = 5;
        daily[6].counts.superseded = 4;

        let weekly = group_buckets(&daily, StatGrouping::Weekly);
        assert_eq!(weekly.len(), 1);
        assert_eq!(weekly[0].counts.dismissed, 3); // 2 + 1 across the chunk
        assert_eq!(weekly[0].counts.superseded, 9); // 5 + 4 across the chunk
    }
}
