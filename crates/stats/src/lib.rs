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

/// One chart bar for the Statistics pane: outcome counts plus accepted words
/// over a single 24h slice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DayBucket {
    pub counts: Counts,
    pub words: usize,
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

/// Rolling 30-day usage accumulator. Cheap to clone-free `record`; queries are
/// `O(n)` over the retained window, which stays small at human interaction rates.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    entries: VecDeque<Entry>,
    latencies: VecDeque<(u64, u32)>,
}

impl Stats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an outcome at `now_ms`, then prune anything older than the window.
    pub fn record(&mut self, now_ms: u64, outcome: Outcome) {
        self.entries.push_back(Entry {
            at_ms: now_ms,
            outcome,
        });
        self.prune(now_ms);
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
        for e in self.entries.iter().filter(|e| e.at_ms >= cutoff) {
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
            .filter(|e| e.at_ms >= cutoff)
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
        let samples: Vec<u32> = self
            .latencies
            .iter()
            .filter(|&&(at, _)| at >= cutoff)
            .map(|&(_, ms)| ms)
            .collect();
        if samples.is_empty() {
            return None;
        }
        let sum: u64 = samples.iter().map(|&ms| ms as u64).sum();
        Some((sum / samples.len() as u64) as u32)
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
            .filter(|&&(at, _)| at >= cutoff)
            .map(|&(_, ms)| ms)
            .collect();
        if samples.is_empty() {
            return None;
        }
        samples.sort_unstable();
        let n = samples.len();
        // Nearest-rank: rank = ceil(pct/100 * n), 1-based; clamp into range.
        let rank = (pct as usize * n).div_ceil(100);
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
        let cutoff = now_ms.saturating_sub(days as u64 * DAY_MS);
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

    const DAY_MS_T: u64 = 24 * 60 * 60 * 1000;

    #[test]
    fn daily_buckets_split_the_window_into_trailing_day_slices() {
        // Statistics-pane chart data: `days` trailing 24h slices ending at
        // `now_ms`, oldest first, each with outcome counts + accepted words.
        let mut s = Stats::new();
        let now = T0 + 10 * DAY_MS_T;
        s.record(now, Outcome::Accepted { words: 3 }); // most recent slice
        s.record(now - DAY_MS_T - 1, Outcome::Shown); // middle slice
        s.record(now - 3 * DAY_MS_T + 1, Outcome::Dismissed); // oldest slice
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
        let now = T0 + 10 * DAY_MS_T;
        s.record(now - 2 * DAY_MS_T - 1, Outcome::Accepted { words: 9 }); // outside 2-day span
        s.record(now - 2 * DAY_MS_T, Outcome::Shown); // exactly at the cutoff
        let buckets = s.daily_buckets(now, 2);
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].counts.shown, 1);
        assert_eq!(buckets[0].counts.accepted, 0);
        assert_eq!(buckets[0].words, 0);
        assert!(s.daily_buckets(now, 0).is_empty());
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
        // [10,20,21] mean = 51/3 = 17 exactly; [10,20,20]=50/3 floors to 16.
        let mut s = Stats::new();
        for ms in [10u32, 20, 20] {
            s.record_latency(T0, ms);
        }
        assert_eq!(s.latency_avg_ms(T0), Some(16));
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

    const DAY_MS: u64 = 24 * 60 * 60 * 1000;
}
