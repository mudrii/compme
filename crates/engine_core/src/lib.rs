//! Deterministic suggestion state machine.

use context::{left_context, right_context, trim_trailing};
use platform::{
    ux_mode, AcceptAction, Capabilities, CorrectionRange, FieldHandle, InsertStrategy, UxMode,
};
use ranker::{
    cap_words, is_degenerate_repetition, next_word, repetition_penalty, strip_suffix_overlap,
    trim_to_stop_boundary, truncate_at_sentence_end,
};

/// Monotonic id for one (value, caret) state of the focused field. Completion
/// requests are stamped with the snapshot they were issued for; a completion
/// whose stamp no longer matches the machine's current snapshot is stale and
/// silently dropped — hosts must echo the stamp back unchanged, never invent
/// or reuse one.
pub type SnapshotId = u64;

/// Completions whose repetition penalty falls below this floor (i.e. they echo
/// text already to the left of the caret) are dropped rather than shown.
const REPETITION_PENALTY_FLOOR: f64 = 0.5;

/// Hard cap on the buffered stat-event queue. The host drains it every loop turn
/// (`take_stat_events`), so normal use never approaches this — the cap only bounds
/// memory if a host stops draining (e.g. a stats sink errors and the loop keeps
/// running). Stats are advisory, so dropping events beyond the cap is benign.
const STAT_EVENTS_CAP: usize = 4096;

/// What kind of edit produced a `TextChanged`. Only the Delete/non-Delete
/// split is load-bearing: `Delete` never arms a completion request, everything
/// else (including `Unknown`) does — so hosts unsure of the edit kind must
/// report `Unknown`, not `Delete`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    Insert,
    Delete,
    Unknown,
}

/// Whether an edit may auto-arm the completion debounce. `Manual` edits update
/// state (value/caret/snapshot) but never schedule a request — the host owns
/// any explicit-trigger path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerPolicy {
    Automatic,
    Manual,
}

/// Inputs to [`SuggestionMachine::on_event`]. Ordering is the host's
/// obligation: events must arrive in the order they happened on one serialized
/// loop (caret offsets are Unicode scalars, per the `context` crate contract).
/// Staleness is the machine's: `CompletionReady`/`CompletionReadyMulti` carry
/// the `generation`/`snapshot` they were requested with, and any mismatch with
/// the current boundary makes them no-ops — so late completions are idempotent
/// to deliver, never reorder around the event that staled them. The dismiss
/// variants differ deliberately: `Dismiss` is snapshot-neutral and idempotent;
/// `DismissDiscard`/`DismissSuppress` advance the snapshot (staling in-flight
/// requests) and must not be substituted for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Focus {
        field: FieldHandle,
        caps: Capabilities,
    },
    TextChanged {
        field: FieldHandle,
        value: String,
        caret: usize,
        edit: EditKind,
        trigger: TriggerPolicy,
        now_ms: u64,
    },
    CaretMoved {
        field: FieldHandle,
        caret: usize,
    },
    Tick {
        now_ms: u64,
    },
    CompletionReady {
        generation: u64,
        field: FieldHandle,
        snapshot: SnapshotId,
        text: String,
    },
    /// Multiple candidate continuations for one request (multi-candidate, A2
    /// §16). The first is the primary; `Cycle` rotates through the rest.
    CompletionReadyMulti {
        generation: u64,
        field: FieldHandle,
        snapshot: SnapshotId,
        candidates: Vec<String>,
    },
    CorrectionReady {
        generation: u64,
        field: FieldHandle,
        snapshot: SnapshotId,
        original: String,
        suggestion: String,
        correction_range: CorrectionRange,
    },
    /// Rotate to the next candidate while a suggestion is showing.
    Cycle,
    /// Re-assert the suggestion the engine currently holds (Item 4 always-on
    /// "force-show" hotkey): re-emit the on-screen candidate verbatim — same
    /// candidate, no rotation (unlike `Cycle`) and no fresh inference. A no-op
    /// when nothing is currently held.
    ForceShow,
    SecureStateChanged {
        caps: Capabilities,
    },
    AcceptFull,
    AcceptWord,
    AcceptCorrection,
    /// Snapshot-neutral hide: drop the showing ghost without invalidating any
    /// in-flight request. Used for idempotent reconciliation (e.g. an overlay
    /// placement that failed) where a freshly-requested completion must still be
    /// allowed to arrive.
    Dismiss,
    /// Hide AND stale any in-flight request (advances the snapshot) without
    /// suppressing the field. Used for the tray Disable path: dropping only the
    /// queued requests would let one already submitted to the inference worker
    /// pop a ghost back up after the user disabled the app.
    DismissDiscard,
    /// Esc: hide any showing ghost AND suppress new completions in the current
    /// field until the user refocuses or makes another edit (Cotypist parity).
    DismissSuppress,
}

/// Side effects the host must perform, in the order returned from
/// [`SuggestionMachine::on_event`]. The machine assumes every command is
/// executed: skipping one silently desyncs its model of the screen. The one
/// sanctioned failure path is a `ShowGhost` whose placement fails — the host
/// must reconcile via `Event::Dismiss` + [`SuggestionMachine::cancel_last_shown`].
/// `RequestCompletion` must be answered (if at all) with a completion event
/// carrying the same `generation`/`snapshot`/`field` it was issued with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    RequestCompletion {
        generation: u64,
        field: FieldHandle,
        snapshot: SnapshotId,
        prompt: String,
    },
    ShowGhost {
        field: FieldHandle,
        snapshot: SnapshotId,
        text: String,
    },
    ShowCorrection {
        field: FieldHandle,
        snapshot: SnapshotId,
        suggestion: String,
        correction_range: CorrectionRange,
    },
    UpdateGhost {
        field: FieldHandle,
        snapshot: SnapshotId,
        text: String,
    },
    Hide,
    Insert {
        field: FieldHandle,
        text: String,
    },
    /// Like `Insert`, but first delete `replace_left` characters immediately to
    /// the left of the caret — a *replacement* (e.g. emoji `:smile`→😄, typo fix,
    /// US→UK spelling). Emitted only for a `Showing` whose `replace_left > 0`
    /// (produced by `offer_replacement`). The host honors the deletion at the
    /// insertion boundary (AxSet range-extend; SyntheticKeys/Clipboard backspaces
    /// are the live-validated residual — see the integration-phase design note).
    Replace {
        field: FieldHandle,
        text: String,
        replace_left: usize,
    },
    ReplaceRange {
        field: FieldHandle,
        expected_text: String,
        text: String,
        correction_range: CorrectionRange,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presentation {
    Ghost,
    Correction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Showing {
    field: FieldHandle,
    snapshot: SnapshotId,
    /// Shaped candidate continuations; `index` selects the one on screen.
    candidates: Vec<String>,
    index: usize,
    caret: usize,
    /// Characters to delete left of the caret on accept (a replacement, set by
    /// `offer_replacement`). `0` for ordinary model completions (append-only).
    replace_left: usize,
    presentation: Presentation,
    correction_range: Option<CorrectionRange>,
    correction_original: Option<String>,
}

struct CorrectionOffer {
    original: String,
    suggestion: String,
    range: CorrectionRange,
}

impl Showing {
    fn current(&self) -> &str {
        &self.candidates[self.index]
    }
}

/// Suggestion-lifecycle events worth counting for local usage stats (design spec
/// §11). `Accepted`/`Dismissed` are observed by the host from the accept/dismiss
/// inputs; these two are machine-internal and surfaced via `take_stat_events`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatEvent {
    /// A ghost suggestion was presented (a `ShowGhost` command was emitted).
    Shown,
    /// A presented ghost was discarded by a non-user event (new typing, caret
    /// move, focus change, secure-state change) before the user acted on it.
    Superseded,
}

/// The deterministic suggestion core: consumes [`Event`]s, returns
/// [`Command`]s, performs no I/O and reads no clock — time only enters as the
/// `now_ms` fields on events, which the host must source from one monotonic,
/// non-decreasing clock (debounce math depends on it). Not internally
/// synchronized (`&mut self`): the host serializes all events onto a single
/// loop. Core invariant: every state-invalidating event (focus, edit, caret
/// move, secure change, discard/suppress dismiss) advances the
/// generation/snapshot boundary, so an in-flight completion can never render
/// against newer text.
pub struct SuggestionMachine {
    caps: Capabilities,
    debounce_ms: u64,
    max_words: usize,
    min_context_chars: usize,
    allow_mid_word: bool,
    generation: u64,
    snapshot: SnapshotId,
    field: Option<FieldHandle>,
    value: String,
    caret: usize,
    pending_since: Option<u64>,
    requested: Option<RequestedCompletion>,
    showing: Option<Showing>,
    /// Set by `DismissSuppress` (Esc); blocks completions in the current field
    /// until cleared by a refocus or the next edit.
    suppressed: bool,
    /// Cotypist's "Include trailing space after single-word completions"
    /// (`COMPME_TRAILING_SPACE`). When set, accepting a single-word
    /// completion inserts one trailing space. Default off → accept text is
    /// byte-identical to before this flag existed.
    trailing_space_single_word: bool,
    /// Buffered Shown/Superseded events drained by the host into usage stats
    /// every loop turn. Appended only via `record_stat`, which caps the queue at
    /// [`STAT_EVENTS_CAP`] so a non-draining host can't grow it without bound.
    stat_events: Vec<StatEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RequestedCompletion {
    generation: u64,
    field: FieldHandle,
    snapshot: SnapshotId,
    kind: RequestedKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestedKind {
    Completion,
    GrammarFix,
}

impl SuggestionMachine {
    pub fn new(caps: Capabilities, debounce_ms: u64, max_words: usize) -> Self {
        Self {
            caps,
            debounce_ms,
            max_words,
            // Permissive defaults: no minimum context, mid-word allowed. Callers
            // opt into conservative triggering via `with_trigger_gates`.
            min_context_chars: 0,
            allow_mid_word: true,
            generation: 0,
            snapshot: 0,
            field: None,
            value: String::new(),
            caret: 0,
            pending_since: None,
            requested: None,
            showing: None,
            suppressed: false,
            trailing_space_single_word: false,
            stat_events: Vec::new(),
        }
    }

    /// Drain the buffered Shown/Superseded stat events (design spec §11). The
    /// host calls this each loop turn and records them into local usage stats.
    pub fn take_stat_events(&mut self) -> Vec<StatEvent> {
        std::mem::take(&mut self.stat_events)
    }

    /// Buffer a Shown/Superseded stat event, bounded by [`STAT_EVENTS_CAP`] so a
    /// host that stops draining can't grow the queue without limit. Stats are
    /// advisory, so silently dropping past the cap is the right trade.
    fn record_stat(&mut self, event: StatEvent) {
        if self.stat_events.len() < STAT_EVENTS_CAP {
            self.stat_events.push(event);
        }
    }

    /// Configure conservative trigger gating (spec §4, "protect first-run"):
    /// require at least `min_context_chars` of trimmed left context before
    /// requesting, and (unless `allow_mid_word`) suppress requests when the caret
    /// splits a word. Defaults are permissive so existing callers are unaffected.
    /// Runtime flip of the mid-word gate (the per-app App-Settings override —
    /// the run loop re-applies it on every focus change; `with_trigger_gates`
    /// stays the builder for the global default).
    pub fn set_allow_mid_word(&mut self, allow_mid_word: bool) {
        self.allow_mid_word = allow_mid_word;
    }

    pub fn with_trigger_gates(mut self, min_context_chars: usize, allow_mid_word: bool) -> Self {
        self.min_context_chars = min_context_chars;
        self.allow_mid_word = allow_mid_word;
        self
    }

    /// Enable Cotypist's "Include trailing space after single-word completions".
    /// Default off so existing callers are unaffected.
    /// Runtime flip of the single-word trailing space (General-tab switch,
    /// c110) — the flag is read per accept, so this applies immediately.
    pub fn set_trailing_space(&mut self, enabled: bool) {
        self.trailing_space_single_word = enabled;
    }

    pub fn with_trailing_space(mut self, enabled: bool) -> Self {
        self.trailing_space_single_word = enabled;
        self
    }

    /// Apply the single-word trailing-space policy to accept-inserted text.
    /// Self-gating: only single-word text not already ending in whitespace is
    /// affected, so it is safe to call on every accept/preview path (multi-word
    /// completions and word-by-word fragments — which `next_word` returns with
    /// their own trailing space — pass through unchanged).
    fn finalize_accept_text(&self, text: &str) -> String {
        append_single_word_space(text, self.trailing_space_single_word)
    }

    fn enabled(&self) -> bool {
        matches!(ux_mode(&self.caps), UxMode::Inline | UxMode::Popup)
    }

    /// Whether the current value/caret passes the conservative trigger gates:
    /// enough left context, and not mid-word unless configured otherwise.
    fn passes_trigger_gates(&self) -> bool {
        let left = left_context(&self.value, self.caret);
        // Minimum context: count only substantive characters — leading and
        // trailing whitespace must not satisfy the minimum.
        if left.trim().chars().count() < self.min_context_chars {
            return false;
        }
        // Mid-word: the caret splits a word only when the characters on *both*
        // sides are word characters. A caret at a word boundary (after a space,
        // at the start of a word, or at end-of-text) is not mid-word.
        if !self.allow_mid_word {
            let is_word = |c: char| c.is_alphanumeric() || c == '_';
            let left_is_word = left.chars().next_back().is_some_and(is_word);
            let right = right_context(&self.value, self.caret);
            let right_is_word = right.chars().next().is_some_and(is_word);
            if left_is_word && right_is_word {
                return false;
            }
        }
        true
    }

    fn hide_if_showing(&mut self, out: &mut Vec<Command>) {
        if self.showing.take().is_some() {
            out.push(Command::Hide);
        }
    }

    fn advance_snapshot(&mut self) {
        self.generation += 1;
        self.snapshot += 1;
        self.requested = None;
    }

    pub fn on_event(&mut self, event: Event) -> Vec<Command> {
        let mut out = Vec::new();

        // A ghost showing before a non-user event (typing, caret move, focus or
        // secure-state change) that ends with it hidden was *superseded* — shown
        // but replaced before the user accepted or dismissed it (design spec §11).
        let was_showing = self.showing.is_some();
        let non_user_event = matches!(
            event,
            Event::Focus { .. }
                | Event::TextChanged { .. }
                | Event::CaretMoved { .. }
                | Event::SecureStateChanged { .. }
        );

        match event {
            Event::Focus { field, caps } => {
                self.hide_if_showing(&mut out);
                self.caps = caps;
                self.field = Some(field);
                self.value.clear();
                self.caret = 0;
                self.pending_since = None;
                self.suppressed = false;
                self.advance_snapshot();
            }
            Event::TextChanged {
                field,
                value,
                caret,
                edit,
                trigger,
                now_ms,
            } => {
                self.hide_if_showing(&mut out);
                self.field = Some(field);
                self.value = value;
                self.caret = caret;
                self.advance_snapshot();
                // An edit clears Esc-suppression and resumes triggering for that
                // edit; the suppression is only for the dismissed suggestion in
                // the current field until the user changes/refocuses it.
                self.suppressed = false;
                self.pending_since = if edit != EditKind::Delete
                    && self.enabled()
                    && trigger == TriggerPolicy::Automatic
                    && self.passes_trigger_gates()
                {
                    Some(now_ms)
                } else {
                    None
                };
            }
            Event::Tick { now_ms } => {
                if let (Some(since), Some(field)) = (self.pending_since, self.field.clone()) {
                    if self.enabled() && now_ms.saturating_sub(since) >= self.debounce_ms {
                        let generation = self.generation;
                        let snapshot = self.snapshot;
                        let prompt =
                            trim_trailing(&left_context(&self.value, self.caret)).to_string();
                        self.requested = Some(RequestedCompletion {
                            generation,
                            field: field.clone(),
                            snapshot,
                            kind: RequestedKind::Completion,
                        });
                        self.pending_since = None;
                        out.push(Command::RequestCompletion {
                            generation,
                            field,
                            snapshot,
                            prompt,
                        });
                    }
                }
            }
            Event::CompletionReady {
                generation,
                field,
                snapshot,
                text,
            } => {
                self.on_completion_ready(generation, &field, snapshot, vec![text], &mut out);
            }
            Event::CompletionReadyMulti {
                generation,
                field,
                snapshot,
                candidates,
            } => {
                self.on_completion_ready(generation, &field, snapshot, candidates, &mut out);
            }
            Event::CorrectionReady {
                generation,
                field,
                snapshot,
                original,
                suggestion,
                correction_range,
            } => {
                self.on_correction_ready(
                    generation,
                    &field,
                    snapshot,
                    CorrectionOffer {
                        original,
                        suggestion,
                        range: correction_range,
                    },
                    &mut out,
                );
            }
            Event::Cycle => {
                if let Some(showing) = self.showing.as_mut() {
                    if showing.candidates.len() > 1 {
                        showing.index = (showing.index + 1) % showing.candidates.len();
                        out.push(Command::UpdateGhost {
                            field: showing.field.clone(),
                            snapshot: showing.snapshot,
                            text: showing.current().to_string(),
                        });
                    }
                }
            }
            Event::ForceShow => {
                // Re-assert the held candidate verbatim (no rotation, no fresh
                // inference). Reuses the same `showing` state `Cycle` relies on,
                // so it can only re-present a suggestion the engine still holds;
                // a no-op otherwise. Re-emitting `ShowGhost` (not `UpdateGhost`)
                // re-presents from scratch, covering the case where the prior
                // placement failed and no ghost is actually on screen.
                //
                // TODO(LOOK): this re-shows only while `showing` is still set —
                // every hide path (`Dismiss*`, focus/edit/caret/secure change)
                // takes `self.showing`, so a *dismissed* suggestion leaves no
                // retained candidate to re-show. Retaining a dismissed candidate
                // across snapshot-invalidating events (and re-validating its
                // generation/snapshot on re-show) is a larger change deferred
                // until the physical-hotkey UX is validated.
                if let Some(showing) = self.showing.as_ref() {
                    // A held grammar correction must re-present as a correction:
                    // ShowGhost would anchor at the caret instead of the
                    // correction span and arm the tap as AcceptFull, which
                    // deliberately no-ops on Presentation::Correction.
                    match (showing.presentation, showing.correction_range) {
                        (Presentation::Correction, Some(correction_range)) => {
                            out.push(Command::ShowCorrection {
                                field: showing.field.clone(),
                                snapshot: showing.snapshot,
                                suggestion: showing.current().to_string(),
                                correction_range,
                            });
                        }
                        _ => out.push(Command::ShowGhost {
                            field: showing.field.clone(),
                            snapshot: showing.snapshot,
                            text: showing.current().to_string(),
                        }),
                    }
                }
            }
            Event::CaretMoved { field, caret } => {
                let moved = self.field.as_ref() != Some(&field) || self.caret != caret;
                if moved {
                    self.hide_if_showing(&mut out);
                    self.advance_snapshot();
                    // Cancel any armed-but-unfired debounce: the trigger gates
                    // (mid-word/min-context) were evaluated at the pre-move
                    // caret, and Tick does not re-check them. Without this, a
                    // caret move into a gate-rejecting position (e.g. mid-word)
                    // would still fire a RequestCompletion the gates would have
                    // blocked. Every other context-invalidating event (Focus,
                    // SecureStateChanged, Dismiss*) clears it the same way.
                    self.pending_since = None;
                }
                self.field = Some(field);
                self.caret = caret;
            }
            Event::SecureStateChanged { caps } => {
                self.hide_if_showing(&mut out);
                self.caps = caps;
                self.pending_since = None;
                self.advance_snapshot();
            }
            Event::Dismiss => {
                // Snapshot-neutral: hide only, leaving any in-flight request
                // valid (the show-failed reconciliation path relies on this).
                self.hide_if_showing(&mut out);
            }
            Event::DismissDiscard => {
                self.hide_if_showing(&mut out);
                self.pending_since = None;
                // Stale any in-flight request so its completion cannot pop a
                // ghost back up after the user disabled the app (the tray Disable
                // path only drops *queued* requests; one already submitted to the
                // inference worker would otherwise re-show on return).
                self.advance_snapshot();
            }
            Event::DismissSuppress => {
                self.hide_if_showing(&mut out);
                self.suppressed = true;
                self.pending_since = None;
                // Stale any in-flight request so its completion cannot pop a
                // ghost back up after the dismiss.
                self.advance_snapshot();
            }
            Event::AcceptFull => {
                if let Some(showing) = self.showing.take() {
                    if showing.presentation == Presentation::Correction {
                        self.showing = Some(showing);
                        return out;
                    }
                    // A replacement (`replace_left > 0`) inserts its exact rendered
                    // text (emoji glyph / synonym) — the trailing-space-after-
                    // single-word policy applies only to append-only completions.
                    let raw = &showing.candidates[showing.index];
                    let text = if showing.replace_left > 0 {
                        raw.clone()
                    } else {
                        self.finalize_accept_text(raw)
                    };
                    // Append-only completion → Insert; replacement (emoji/typo/
                    // spelling, replace_left > 0) → Replace that first deletes
                    // replace_left chars. Same shape the AcceptWord arm inlines.
                    out.push(if showing.replace_left > 0 {
                        Command::Replace {
                            field: showing.field,
                            text,
                            replace_left: showing.replace_left,
                        }
                    } else {
                        Command::Insert {
                            field: showing.field,
                            text,
                        }
                    });
                    out.push(Command::Hide);
                    self.advance_snapshot();
                }
            }
            Event::AcceptWord => {
                if let Some(mut showing) = self.showing.take() {
                    if showing.presentation == Presentation::Correction {
                        self.showing = Some(showing);
                        return out;
                    }
                    // A replacement (`replace_left > 0`, e.g. emoji/synonym) is
                    // atomic — there is no "next word" of a glyph to partially
                    // accept, and a multi-word synonym ("big deal") must not be
                    // split (which would drop the deletion). Word-accept of a
                    // replacement therefore commits the whole token like Full.
                    if showing.replace_left > 0 {
                        out.push(Command::Replace {
                            field: showing.field,
                            text: showing.candidates[showing.index].clone(),
                            replace_left: showing.replace_left,
                        });
                        out.push(Command::Hide);
                        self.advance_snapshot();
                        return out;
                    }
                    let (word, rest) = next_word(showing.current());
                    // Single-word trailing-space applies only when this accept
                    // completes the suggestion (no rest); `finalize_accept_text`
                    // self-gates, but `word` already carries its own trailing
                    // space when `rest` is non-empty, so it is a no-op there.
                    let text = self.finalize_accept_text(&word);
                    out.push(Command::Insert {
                        field: showing.field.clone(),
                        text,
                    });
                    if rest.is_empty() {
                        out.push(Command::Hide);
                        self.advance_snapshot();
                    } else {
                        // Advance the caret to the position it will occupy AFTER
                        // the host inserts `word`. This intentionally moves past
                        // the current `self.value` length — `self.value` still
                        // holds the pre-insert text and only grows when the host
                        // echoes the next `TextChanged`, so clamping to it here
                        // would wrongly pin the caret behind the accepted word and
                        // hide a ghost that should stay visible. Downstream context
                        // helpers clamp defensively, so a transiently-unsynced caret
                        // can never panic; it self-corrects on the next edit.
                        showing.caret += word.chars().count();
                        self.caret = showing.caret;
                        // Collapse to the active candidate: the siblings still
                        // begin with the just-accepted word and would re-offer it
                        // on cycle, so once the user commits word-by-word the
                        // alternatives are dropped.
                        showing.candidates = vec![rest.clone()];
                        showing.index = 0;
                        out.push(Command::UpdateGhost {
                            field: showing.field.clone(),
                            snapshot: showing.snapshot,
                            text: rest,
                        });
                        self.showing = Some(showing);
                    }
                }
            }
            Event::AcceptCorrection => {
                if let Some(showing) = self.showing.take() {
                    if showing.presentation != Presentation::Correction {
                        self.showing = Some(showing);
                        return out;
                    }
                    let Some(correction_range) = showing.correction_range else {
                        self.showing = Some(showing);
                        return out;
                    };
                    let Some(expected_text) = showing.correction_original.clone() else {
                        self.showing = Some(showing);
                        return out;
                    };
                    let text = showing.candidates[showing.index].clone();
                    out.push(Command::ReplaceRange {
                        field: showing.field,
                        expected_text,
                        text,
                        correction_range,
                    });
                    out.push(Command::Hide);
                    self.advance_snapshot();
                }
            }
        }

        if was_showing && non_user_event && self.showing.is_none() {
            self.record_stat(StatEvent::Superseded);
        }

        out
    }

    /// Shape raw candidates into inline offerings and, if any survive, show the
    /// first. Shared by the single (`CompletionReady`) and multi
    /// (`CompletionReadyMulti`) paths. Shaping: cut at the first line break, then
    /// the first sentence end, drop any tail that re-states text after the caret,
    /// cap the word count; drop empty/echoing/degenerate candidates and exact
    /// duplicates.
    fn on_completion_ready(
        &mut self,
        generation: u64,
        field: &FieldHandle,
        snapshot: SnapshotId,
        raw_candidates: Vec<String>,
        out: &mut Vec<Command>,
    ) {
        // No explicit `suppressed` check is needed here: `DismissSuppress`
        // advances the snapshot (staling any in-flight request) and clears
        // `requested`, and a suppressed field cannot arm a fresh request
        // (`TextChanged` clears suppression before arming), so no matching
        // completion can arrive while suppressed.
        let matches_request = self.requested.as_ref().is_some_and(|requested| {
            requested.generation == generation
                && requested.snapshot == snapshot
                && requested.field == *field
                && requested.kind == RequestedKind::Completion
                && generation == self.generation
                && snapshot == self.snapshot
        });
        if !matches_request {
            return;
        }

        let right = right_context(&self.value, self.caret);
        let recent = left_context(&self.value, self.caret);
        let mut shaped: Vec<String> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for raw in raw_candidates {
            let line = trim_to_stop_boundary(&raw);
            let sentence = truncate_at_sentence_end(line);
            let de_overlapped = strip_suffix_overlap(sentence, &right);
            let capped = cap_words(&de_overlapped, self.max_words);
            // Degeneracy is checked on the PRE-cap text: `is_degenerate_repetition`
            // needs >=3 words, but `cap_words` may have truncated a degenerate loop
            // below that floor (e.g. max_words=2), letting it slip through if checked
            // on `capped`. The repetition penalty stays on the shown (`capped`) text.
            let fresh = repetition_penalty(&capped, &recent) >= REPETITION_PENALTY_FLOOR
                && !is_degenerate_repetition(&de_overlapped)
                && !is_degenerate_repetition(&capped);
            // Dedup on a normalized key (trim + case-fold) so near-duplicates
            // (trailing space, case) don't show as separate cycle options.
            let key = capped.trim().to_lowercase();
            if !capped.is_empty() && fresh && !seen.contains(&key) {
                seen.push(key);
                shaped.push(capped);
            }
        }

        if let Some(first) = shaped.first().cloned() {
            // A fresh inference result replacing a still-showing ghost supersedes
            // it (the user never acted on the old one). The central on_event guard
            // does not see this — CompletionReady isn't a "non-user" hide event —
            // so account for it explicitly at the replacement site.
            if self.showing.is_some() {
                self.record_stat(StatEvent::Superseded);
            }
            self.showing = Some(Showing {
                field: field.clone(),
                snapshot,
                candidates: shaped,
                index: 0,
                caret: self.caret,
                replace_left: 0,
                presentation: Presentation::Ghost,
                correction_range: None,
                correction_original: None,
            });
            out.push(Command::ShowGhost {
                field: field.clone(),
                snapshot,
                text: first,
            });
            self.record_stat(StatEvent::Shown);
        }
        self.requested = None;
    }

    fn on_correction_ready(
        &mut self,
        generation: u64,
        field: &FieldHandle,
        snapshot: SnapshotId,
        offer: CorrectionOffer,
        out: &mut Vec<Command>,
    ) {
        let matches_request = self.requested.as_ref().is_some_and(|requested| {
            requested.generation == generation
                && requested.snapshot == snapshot
                && requested.field == *field
                && requested.kind == RequestedKind::GrammarFix
                && generation == self.generation
                && snapshot == self.snapshot
        });
        if !matches_request
            || !self.enabled()
            || self.suppressed
            || offer.suggestion.is_empty()
            || self.caps.insert_strategy != InsertStrategy::AxSet
        {
            return;
        }
        if self.showing.is_some() {
            self.record_stat(StatEvent::Superseded);
        }
        self.showing = Some(Showing {
            field: field.clone(),
            snapshot,
            candidates: vec![offer.suggestion.clone()],
            index: 0,
            caret: self.caret,
            replace_left: 0,
            presentation: Presentation::Correction,
            correction_range: Some(offer.range),
            correction_original: Some(offer.original),
        });
        out.push(Command::ShowCorrection {
            field: field.clone(),
            snapshot,
            suggestion: offer.suggestion,
            correction_range: offer.range,
        });
        self.record_stat(StatEvent::Shown);
        self.requested = None;
    }

    pub fn arm_manual_grammar_request(&mut self, field: &FieldHandle) -> Option<(u64, SnapshotId)> {
        if !self.enabled() || self.suppressed || self.field.as_ref() != Some(field) {
            return None;
        }
        let generation = self.generation;
        let snapshot = self.snapshot;
        self.pending_since = None;
        self.requested = Some(RequestedCompletion {
            generation,
            field: field.clone(),
            snapshot,
            kind: RequestedKind::GrammarFix,
        });
        Some((generation, snapshot))
    }

    /// Retract the most recent `Shown` stat event — used by the host when an
    /// overlay placement failed, so a ghost that was emitted but never actually
    /// presented to the user is not counted as shown (design spec §11).
    pub fn cancel_last_shown(&mut self) {
        if let Some(pos) = self
            .stat_events
            .iter()
            .rposition(|e| *e == StatEvent::Shown)
        {
            self.stat_events.remove(pos);
        }
    }

    /// The exact `(field, text, replace_left)` the next accept would insert, so
    /// the host can absorb its own self-insert echo (and record/replace) without
    /// reading a divergent engine snapshot. Matches `on_event`'s accept paths: a
    /// replacement (`replace_left > 0`) is atomic + unfinalized; an ordinary
    /// completion is word/full + trailing-space-finalized with `replace_left == 0`.
    /// Returns `None` when there is nothing to insert (empty text) — a guard the
    /// live accept path doesn't need because a shown candidate is never empty.
    pub fn preview_accept_insert(
        &self,
        action: AcceptAction,
    ) -> Option<(FieldHandle, String, usize)> {
        let showing = self.showing.as_ref()?;
        if showing.presentation == Presentation::Correction {
            return None;
        }
        if showing.replace_left > 0 {
            let text = showing.current().to_string();
            return (!text.is_empty()).then(|| (showing.field.clone(), text, showing.replace_left));
        }
        let raw = match action {
            AcceptAction::Full => showing.current().to_string(),
            AcceptAction::Word => next_word(showing.current()).0,
            AcceptAction::Correction => return None,
        };
        let text = self.finalize_accept_text(&raw);
        (!raw.is_empty()).then(|| (showing.field.clone(), text, 0))
    }

    pub fn preview_accept_correction(&self) -> Option<(FieldHandle, String, CorrectionRange)> {
        let showing = self.showing.as_ref()?;
        if showing.presentation != Presentation::Correction {
            return None;
        }
        let range = showing.correction_range?;
        let text = showing.candidates.get(showing.index)?.clone();
        (!text.is_empty()).then(|| (showing.field.clone(), text, range))
    }

    /// Offer a local *replacement* suggestion: show `text` as the ghost, and on
    /// accept delete `replace_left` characters to the left of the caret before
    /// inserting (emoji `:smile`→😄, typo fix, US→UK spelling). Host-driven — the
    /// host detects the opportunity (e.g. `emoji::suggest`) and supplies the
    /// rendered `text` + `replace_left`; `engine_core` takes no dependency on those
    /// crates. Gated like a model completion: no offer when the field can't show
    /// inline (`enabled`), the field is suppressed (post-Esc), or `text` is empty.
    /// The offer rides the current snapshot and **disarms the model path** for it
    /// (clears `pending_since` + `requested`), so neither a prior in-flight request
    /// (stale by snapshot) nor a freshly-armed one (the debounce tick that
    /// `on_text_changed` would fire) can issue a completion that supersedes this
    /// ghost — the replacement genuinely preempts the model. Emits `ShowGhost`
    /// (+ a `Shown` stat).
    ///
    /// Only offered on an `AxSet` field: the accept must *delete* `replace_left`
    /// chars, which only the AX range-replace path honors. SyntheticKeys/Clipboard
    /// cannot do that atomically, so offering there would both leave the typed
    /// token (`:smile😄`) and desync the host's diff baseline — so we don't.
    ///
    /// Test-only: production code calls [`Self::offer_replacement_multi`]
    /// directly. This single-candidate wrapper survives only for test call sites.
    #[cfg(test)]
    pub fn offer_replacement(
        &mut self,
        field: &FieldHandle,
        text: String,
        replace_left: usize,
    ) -> Vec<Command> {
        // Single-candidate convenience: identical to the multi path with one
        // candidate (multi's empty-filter + exact-dedup is a no-op on one
        // non-empty element, and its empty-after-filter guard matches this
        // path's `text.is_empty()` gate). Delegating keeps the offer logic —
        // the gating, supersede accounting, model-path disarm, and ShowGhost —
        // in exactly one place (`offer_replacement_multi`) rather than two.
        self.offer_replacement_multi(field, vec![text], replace_left)
    }

    /// Offer a local replacement (emoji/thesaurus/typo) with multiple candidates.
    /// Works exactly like `offer_replacement`, but populates a candidate list
    /// for cycling (Down arrow).
    pub fn offer_replacement_multi(
        &mut self,
        field: &FieldHandle,
        candidates: Vec<String>,
        replace_left: usize,
    ) -> Vec<Command> {
        let mut out = Vec::new();
        if !self.enabled()
            || self.suppressed
            || candidates.is_empty()
            || replace_left == 0
            || self.caps.insert_strategy != InsertStrategy::AxSet
        {
            return out;
        }
        if self.field.as_ref() != Some(field) {
            return out;
        }
        // Filter empties and dedup in order before seeding: the single-candidate
        // path rejects empty text and the completion path dedups (on a normalized
        // key); this multi seed dedups on EXACT match — a tighter equivalence,
        // sufficient so Cycle never shows a blank or an exact-duplicate candidate.
        // (Defense-in-depth: today's producers emit non-empty, unique candidates.)
        let mut seen = std::collections::HashSet::new();
        let candidates: Vec<String> = candidates
            .into_iter()
            .filter(|c| !c.is_empty() && seen.insert(c.clone()))
            .collect();
        if candidates.is_empty() {
            return out;
        }
        if self.showing.is_some() {
            self.record_stat(StatEvent::Superseded);
        }
        let text = candidates[0].clone();
        self.showing = Some(Showing {
            field: field.clone(),
            snapshot: self.snapshot,
            candidates,
            index: 0,
            caret: self.caret,
            replace_left,
            presentation: Presentation::Ghost,
            correction_range: None,
            correction_original: None,
        });
        self.pending_since = None;
        self.requested = None;
        out.push(Command::ShowGhost {
            field: field.clone(),
            snapshot: self.snapshot,
            text,
        });
        self.record_stat(StatEvent::Shown);
        out
    }
}

/// Cotypist's "Include trailing space after single-word completions": when
/// `enabled`, append one trailing space to a completion that is a single word
/// (no interior whitespace) and does not already end in whitespace. With
/// `enabled == false` the text is returned unchanged, so default accept
/// behavior is byte-identical to before this flag existed. Multi-word text and
/// text already ending in whitespace pass through regardless of `enabled`.
fn append_single_word_space(text: &str, enabled: bool) -> String {
    let is_single_word = !text.is_empty()
        && !text.chars().next_back().is_some_and(char::is_whitespace)
        && text.split_whitespace().count() == 1;
    if enabled && is_single_word {
        let mut out = String::with_capacity(text.len() + 1);
        out.push_str(text);
        out.push(' ');
        out
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::{InsertStrategy, KeyInterceptMode, OverlayPlacement, SecurityState, Toolkit};

    fn field(id: &str) -> FieldHandle {
        FieldHandle {
            app: "TextEdit".into(),
            pid: Some(42),
            element_id: id.into(),
            generation: 1,
        }
    }

    fn inline_caps() -> Capabilities {
        Capabilities {
            readable_text: true,
            readable_caret: true,
            writable: true,
            secure: false,
            security_state: SecurityState::Normal,
            toolkit: Toolkit::AppKit,
            multiline: true,
            insert_strategy: InsertStrategy::AxSet,
            accept_intercept: KeyInterceptMode::CgEventTap,
            overlay_at_caret: OverlayPlacement::NativePanel,
            coords_global_screen: true,
        }
    }

    fn secure_caps() -> Capabilities {
        let mut caps = inline_caps();
        caps.secure = true;
        caps.security_state = SecurityState::SecureField;
        caps
    }

    fn popup_caps() -> Capabilities {
        // No caret geometry → ux_mode derives Popup (not Inline). enabled()
        // accepts both, so this exercises the Popup arm of the predicate.
        let mut caps = inline_caps();
        caps.readable_caret = false;
        caps.overlay_at_caret = OverlayPlacement::None;
        caps
    }

    fn machine() -> SuggestionMachine {
        SuggestionMachine::new(inline_caps(), 200, 4)
    }

    fn text_changed(value: &str, caret: usize, now_ms: u64) -> Event {
        Event::TextChanged {
            field: field("field-a"),
            value: value.into(),
            caret,
            edit: EditKind::Insert,
            trigger: TriggerPolicy::Automatic,
            now_ms,
        }
    }

    /// Seed a showing correction through the production path: arm a manual
    /// grammar request, then deliver the matching `CorrectionReady`.
    fn show_correction(machine: &mut SuggestionMachine, suggestion: &str, range: CorrectionRange) {
        let f = field("field-a");
        let (generation, snapshot) = machine
            .arm_manual_grammar_request(&f)
            .expect("focused, enabled, unsuppressed field arms a grammar request");
        machine.on_event(Event::CorrectionReady {
            generation,
            field: f,
            snapshot,
            original: "teh".into(),
            suggestion: suggestion.into(),
            correction_range: range,
        });
        assert!(machine.showing.is_some(), "correction is showing");
    }

    #[test]
    fn shown_correction_is_invalidated_by_text_changed() {
        let mut machine = machine();
        machine.on_event(text_changed("teh", 3, 0));
        show_correction(&mut machine, "the", CorrectionRange { start: 0, end: 3 });

        assert_eq!(
            machine.on_event(text_changed("the", 3, 10)),
            vec![Command::Hide]
        );
        assert!(machine.showing.is_none());
    }

    #[test]
    fn force_show_reasserts_a_correction_as_correction_not_ghost() {
        let mut machine = machine();
        machine.on_event(text_changed("teh", 3, 0));
        let range = CorrectionRange { start: 0, end: 3 };
        show_correction(&mut machine, "the", range);

        // ForceShow must re-present a held correction AS a correction: a
        // ShowGhost here would anchor at the caret (not the correction span)
        // and arm the tap as AcceptFull, which silently no-ops on Correction.
        let out = machine.on_event(Event::ForceShow);
        match out.as_slice() {
            [Command::ShowCorrection {
                suggestion,
                correction_range,
                ..
            }] => {
                assert_eq!(suggestion, "the");
                assert_eq!(*correction_range, range);
            }
            other => panic!("expected a single ShowCorrection, got {other:?}"),
        }
    }

    #[test]
    fn accept_full_and_word_do_not_commit_correction_presentation() {
        let mut machine = machine();
        machine.on_event(text_changed("teh", 3, 0));
        show_correction(&mut machine, "the", CorrectionRange { start: 0, end: 3 });

        assert_eq!(machine.on_event(Event::AcceptFull), vec![]);
        assert!(machine.showing.is_some());
        assert_eq!(machine.on_event(Event::AcceptWord), vec![]);
        assert!(machine.showing.is_some());
    }

    #[test]
    fn no_request_when_context_below_min() {
        // min_context_chars=3; "hi " trims to "hi" (2 chars) < 3 → never arms.
        let mut machine = machine().with_trigger_gates(3, false);
        machine.on_event(text_changed("hi ", 3, 1000));
        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn backwards_clock_tick_does_not_fire_or_panic() {
        // The debounce arms at the TextChanged now_ms and Tick compares against
        // it with `now_ms.saturating_sub(since)`. A monotonic clock that goes
        // backwards (or is equal) — `since=1000`, then a `Tick{now_ms:500}` —
        // would underflow a plain subtraction; the saturating math floors at 0,
        // which is `< debounce_ms`, so the request must NOT fire early and the
        // machine must not panic.
        let mut machine = machine();
        machine.on_event(text_changed("hello", 5, 1000));
        let out = machine.on_event(Event::Tick { now_ms: 500 });
        assert_eq!(
            out,
            vec![],
            "a backwards-clock tick must not fire a completion early (or panic): {out:?}"
        );
        assert!(
            !out.iter()
                .any(|c| matches!(c, Command::RequestCompletion { .. })),
            "no RequestCompletion may be emitted when now_ms <= the arming timestamp"
        );
    }

    #[test]
    fn requests_when_context_meets_min() {
        // "hey " trims to "hey" (3 chars) == min → arms and fires.
        let mut machine = machine().with_trigger_gates(3, false);
        machine.on_event(text_changed("hey ", 4, 1000));
        assert!(machine
            .on_event(Event::Tick { now_ms: 1300 })
            .iter()
            .any(|c| matches!(c, Command::RequestCompletion { .. })));
    }

    #[test]
    fn set_allow_mid_word_flips_the_gate_at_runtime() {
        // The per-app override (App Settings) re-applies the gate on focus
        // change — the SAME machine must flip suppression on and off live.
        let mut machine = machine().with_trigger_gates(0, false);
        machine.on_event(text_changed("hello world", 3, 1000));
        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 2000 }),
            vec![],
            "suppressed while disallowed"
        );
        machine.set_allow_mid_word(true);
        machine.on_event(text_changed("hello worlds", 3, 3000));
        assert!(
            machine
                .on_event(Event::Tick { now_ms: 4000 })
                .iter()
                .any(|c| matches!(c, Command::RequestCompletion { .. })),
            "allowed after the runtime flip"
        );
        machine.set_allow_mid_word(false);
        machine.on_event(text_changed("hello worldly", 3, 5000));
        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 6000 }),
            vec![],
            "suppressed again after flipping back"
        );
    }

    #[test]
    fn no_request_mid_word() {
        // Caret at 3 inside "hello" → right context "lo world" starts with an
        // alphanumeric char → mid-word → suppressed when allow_mid_word=false.
        let mut machine = machine().with_trigger_gates(0, false);
        machine.on_event(text_changed("hello world", 3, 1000));
        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn requests_at_word_boundary() {
        // Caret at 5 (after "hello", before the space) → right " world" starts
        // with a non-word char → not mid-word → arms.
        let mut machine = machine().with_trigger_gates(0, false);
        machine.on_event(text_changed("hello world", 5, 1000));
        // The prompt is the left context (everything before the caret) with
        // trailing whitespace trimmed: caret 5 sits right after "hello", so the
        // request must carry exactly "hello" — pins the caret-split point.
        let commands = machine.on_event(Event::Tick { now_ms: 1300 });
        assert!(commands.iter().any(|c| matches!(
            c,
            Command::RequestCompletion { prompt, .. } if prompt == "hello"
        )));
    }

    #[test]
    fn requests_at_end_of_text() {
        // Caret at end → right context empty → not mid-word → arms.
        let mut machine = machine().with_trigger_gates(0, false);
        machine.on_event(text_changed("hello", 5, 1000));
        // Caret at end → left context is the whole value → prompt is exactly
        // "hello" (no trailing space to trim).
        let commands = machine.on_event(Event::Tick { now_ms: 1300 });
        assert!(commands.iter().any(|c| matches!(
            c,
            Command::RequestCompletion { prompt, .. } if prompt == "hello"
        )));
    }

    #[test]
    fn caret_at_word_start_is_not_mid_word() {
        // Caret at 4 in "foo bar": left "foo " ends in a space, right "bar"
        // starts a word. The caret is at a word *boundary* (start of "bar"), not
        // splitting a word, so it must arm even with mid-word suppression on.
        let mut machine = machine().with_trigger_gates(0, false);
        machine.on_event(text_changed("foo bar", 4, 1000));
        // Left context "foo " is trimmed to "foo": the request prompt is exactly
        // "foo", confirming the caret split fell at the start of "bar" (not
        // mid-word) AND that trailing-space trimming applies.
        let commands = machine.on_event(Event::Tick { now_ms: 1300 });
        assert!(commands.iter().any(|c| matches!(
            c,
            Command::RequestCompletion { prompt, .. } if prompt == "foo"
        )));
    }

    #[test]
    fn leading_whitespace_does_not_count_toward_min_context() {
        // "  ab" has 4 left-context chars but only 2 of substance. min=3 must
        // suppress (leading whitespace must not satisfy the minimum).
        let mut machine = machine().with_trigger_gates(3, false);
        machine.on_event(text_changed("  ab", 4, 1000));
        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn trailing_whitespace_does_not_count_toward_min_context() {
        // "ab  " has 4 left-context chars but trims to "ab" (2) < 3 → suppress.
        let mut machine = machine().with_trigger_gates(3, false);
        machine.on_event(text_changed("ab  ", 4, 1000));
        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn underscore_counts_as_a_word_char_for_mid_word() {
        // Caret at 4 in "foo_bar": left ends in '_', right starts with 'b' —
        // both are word chars, so the caret splits an identifier → suppressed.
        let mut machine = machine().with_trigger_gates(0, false);
        machine.on_event(text_changed("foo_bar", 4, 1000));
        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn non_ascii_letters_count_as_word_chars_for_mid_word() {
        // CJK ideographs are alphanumeric: caret inside "日本語" splits a word.
        let mut machine = machine().with_trigger_gates(0, false);
        machine.on_event(text_changed("日本語", 1, 1000));
        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn mid_word_allowed_when_configured() {
        // Same mid-word caret, but allow_mid_word=true → arms anyway.
        let mut machine = machine().with_trigger_gates(0, true);
        machine.on_event(text_changed("hello world", 3, 1000));
        assert!(machine
            .on_event(Event::Tick { now_ms: 1300 })
            .iter()
            .any(|c| matches!(c, Command::RequestCompletion { .. })));
    }

    #[test]
    fn default_machine_has_no_trigger_gates() {
        // new() leaves gates permissive (min 0, mid-word allowed) so existing
        // callers are unaffected; a 1-char mid-word context still arms.
        let mut machine = machine();
        machine.on_event(text_changed("ab", 1, 1000));
        assert!(machine
            .on_event(Event::Tick { now_ms: 1300 })
            .iter()
            .any(|c| matches!(c, Command::RequestCompletion { .. })));
    }

    #[test]
    fn popup_mode_arms_and_shows_like_inline() {
        // enabled() accepts UxMode::Popup as well as Inline; a popup-capable
        // field must still arm a request and show a ghost.
        let mut machine = SuggestionMachine::new(popup_caps(), 200, 4);
        machine.on_event(text_changed("hello ", 6, 1000));
        let armed = machine.on_event(Event::Tick { now_ms: 1200 });
        assert!(armed
            .iter()
            .any(|c| matches!(c, Command::RequestCompletion { .. })));

        let shown = machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            text: "world".into(),
        });
        assert!(shown.iter().any(|c| matches!(c, Command::ShowGhost { .. })));
    }

    #[test]
    fn focus_cancels_a_request_armed_before_it() {
        // A request armed by typing must be cancelled when focus moves away
        // before the debounce fires (Focus clears value/caret/pending_since).
        let mut machine = machine();
        machine.on_event(text_changed("hello ", 6, 1000)); // arms pending_since
        machine.on_event(Event::Focus {
            field: field("field-b"),
            caps: inline_caps(),
        });
        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn requests_completion_after_debounce() {
        let mut machine = machine();

        assert_eq!(machine.on_event(text_changed("hello ", 6, 1000)), vec![]);
        assert_eq!(machine.on_event(Event::Tick { now_ms: 1100 }), vec![]);
        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 1200 }),
            vec![Command::RequestCompletion {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                prompt: "hello".into(),
            }]
        );
    }

    #[test]
    fn delete_edit_does_not_trigger_request() {
        // After a Delete edit, even past the debounce window, no RequestCompletion
        // command should be emitted.
        let mut machine = machine();

        machine.on_event(Event::TextChanged {
            field: field("field-a"),
            value: "hell".into(),
            caret: 4,
            edit: EditKind::Delete,
            trigger: TriggerPolicy::Automatic,
            now_ms: 1000,
        });

        // Tick well past the debounce window — must not emit RequestCompletion.
        let cmds = machine.on_event(Event::Tick { now_ms: 2000 });
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, Command::RequestCompletion { .. })),
            "expected no RequestCompletion after a Delete edit, got: {cmds:?}"
        );
    }

    #[test]
    fn unknown_edit_triggers_request() {
        // The trigger gate keys off "not a Delete", so an Unknown edit still
        // arms a request — this pins the gate against regressing to `== Insert`.
        let mut machine = machine();

        machine.on_event(Event::TextChanged {
            field: field("field-a"),
            value: "typed ".into(),
            caret: 6,
            edit: EditKind::Unknown,
            trigger: TriggerPolicy::Automatic,
            now_ms: 1000,
        });

        let cmds = machine.on_event(Event::Tick { now_ms: 1200 });
        assert!(
            cmds.iter()
                .any(|c| matches!(c, Command::RequestCompletion { .. })),
            "expected RequestCompletion after an Unknown edit, got: {cmds:?}"
        );
    }

    #[test]
    fn secure_state_change_clears_pending_request() {
        // A secure-state flip arriving before debounce must cancel the pending
        // request. Caps stay enabled here so this isolates pending-clearing
        // from the separate "secure field disables requests" path.
        let mut machine = machine();
        machine.on_event(text_changed("hello ", 6, 1000));

        machine.on_event(Event::SecureStateChanged {
            caps: inline_caps(),
        });

        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn manual_trigger_does_not_auto_request() {
        let mut machine = machine();

        machine.on_event(Event::TextChanged {
            field: field("field-a"),
            value: "hello ".into(),
            caret: 6,
            edit: EditKind::Insert,
            trigger: TriggerPolicy::Manual,
            now_ms: 1000,
        });

        assert_eq!(machine.on_event(Event::Tick { now_ms: 2000 }), vec![]);
    }

    #[test]
    fn shows_ghost_on_matching_completion() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "a b c d e".into(),
            }),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "a b c d".into(),
            }]
        );
    }

    #[test]
    fn shows_only_first_line_of_multiline_completion() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "inline tail\n- bullet\n- bullet".into(),
            }),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "inline tail".into(),
            }]
        );
    }

    #[test]
    fn suppresses_completion_that_repeats_recent_text() {
        let mut machine = machine();
        machine.on_event(text_changed("please repeat me ", 16, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "repeat me".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn truncates_completion_at_sentence_end() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "first done. second thing".into(),
            }),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "first done.".into(),
            }]
        );
    }

    #[test]
    fn strips_completion_overlap_with_text_after_caret() {
        // Caret sits after "the quick" (9 chars) in "the quick fox"; the model
        // regurgitates the trailing " fox", which must be stripped before showing.
        let mut machine = machine();
        machine.on_event(text_changed("the quick fox", 9, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "quick brown fox".into(),
            }),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "quick brown".into(),
            }]
        );
    }

    #[test]
    fn suppresses_completion_fully_overlapping_text_after_caret() {
        // Caret after "the quick" (9 chars); the model echoes exactly the text
        // already to the right (" fox"). Stripping the overlap empties the
        // candidate, so nothing is shown.
        let mut machine = machine();
        machine.on_event(text_changed("the quick fox", 9, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "fox".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn applies_sentence_stop_and_overlap_strip_together() {
        // One completion needs both shapers: cut at the sentence end ("done."),
        // then strip the trailing " fox" already after the caret.
        let mut machine = machine();
        machine.on_event(text_changed("the quick fox", 9, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "brown fox. extra sentence".into(),
            }),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "brown".into(),
            }]
        );
    }

    #[test]
    fn suppresses_degenerate_repetition_created_by_word_cap() {
        // "na na na na ma" is NOT degenerate as-is (5 words), but capping to the
        // 4-word max yields "na na na na" — a loop. The degenerate check runs
        // AFTER cap_words, so the capped loop is still suppressed.
        let mut machine = machine();
        machine.on_event(text_changed("z", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "na na na na ma".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn suppresses_degenerate_repetition_masked_below_the_word_cap() {
        // Opposite direction to the test above: here the cap MASKS the loop
        // instead of creating it. "ha ha ha ha" capped to max_words=2 is
        // "ha ha" — below the 3-word degeneracy floor (ranker), so the POST-cap
        // check passes it. Only the PRE-cap is_degenerate_repetition(&de_overlapped)
        // gate suppresses the visible "ha ha" ghost; deleting that gate leaves
        // every other engine_core test green.
        let mut machine = SuggestionMachine::new(inline_caps(), 200, 2);
        machine.on_event(text_changed("z", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "ha ha ha ha".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn suppresses_degenerate_repetition_completion() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "ha ha ha".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn discards_stale_completion() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(text_changed("xy", 2, 600));

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "stale".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn discards_completion_for_wrong_field() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-b"),
                snapshot: 1,
                text: "wrong field".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn discards_completion_after_secure_state_advances_boundary() {
        // A request is in flight (gen/snap = 1). A secure-state change advances
        // the boundary; the completion tagged with the now-stale gen/snap must be
        // discarded — distinct stale-race site from text/focus changes.
        let mut machine = machine();
        machine.on_event(text_changed("hello ", 6, 1000));
        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 1200 }),
            vec![Command::RequestCompletion {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                prompt: "hello".into(),
            }]
        );

        machine.on_event(Event::SecureStateChanged {
            caps: inline_caps(),
        });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "world".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn discards_completion_after_caret_move_advances_boundary() {
        // A request is in flight (gen/snap = 1). A bare caret move, before any
        // ghost is showing, must still stale that request so old prompt text cannot
        // render at the new caret.
        let mut machine = machine();
        machine.on_event(text_changed("hello world", 11, 1000));
        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 1200 }),
            vec![Command::RequestCompletion {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                prompt: "hello world".into(),
            }]
        );

        machine.on_event(Event::CaretMoved {
            field: field("field-a"),
            caret: 5,
        });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "old tail".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn caret_move_cancels_an_armed_but_unfired_debounce() {
        // Mid-word gating DISABLED at arm time, so the gate is load-bearing: type
        // at a word boundary (passes the gate, arms a debounce), then move the
        // caret mid-word before the debounce elapses. The trigger gates were
        // evaluated at the TYPED (boundary) caret and are NOT re-checked at Tick,
        // so the only thing stopping a request at the now-gate-rejecting mid-word
        // caret is the move's cancel of the armed debounce. Tick must fire nothing.
        let mut machine = machine().with_trigger_gates(0, false);
        // Caret at 5 (after "hello", before the space) is a word boundary → arms.
        machine.on_event(text_changed("hello world", 5, 1000));
        // Move to caret 3, inside "hello" → a genuine mid-word position the gate
        // would reject, reached before the debounce window elapses.
        machine.on_event(Event::CaretMoved {
            field: field("field-a"),
            caret: 3,
        });
        // Debounce elapsed — but the armed request was cancelled by the move into
        // the gate-rejecting spot, so nothing fires.
        assert_eq!(machine.on_event(Event::Tick { now_ms: 1200 }), vec![]);
    }

    #[test]
    fn noop_caret_move_keeps_an_armed_debounce() {
        // A CaretMoved that reports the SAME field+caret as the machine already
        // tracks is a no-op (`moved == false`): it must NOT clear an armed
        // pending_since. Guards the `moved` guard added in the round-3 fix — a
        // spurious caret echo at the current position must not cancel a pending
        // request. Arm at the end of "hello " (caret 6, a word boundary), echo a
        // CaretMoved at the same spot, then Tick past the debounce → still fires.
        let mut machine = machine();
        machine.on_event(text_changed("hello ", 6, 1000));
        machine.on_event(Event::CaretMoved {
            field: field("field-a"),
            caret: 6,
        });
        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 1300 }),
            vec![Command::RequestCompletion {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                prompt: "hello".into(),
            }]
        );
    }

    #[test]
    fn text_change_rearms_while_a_request_is_in_flight() {
        // A second edit while a request is still in flight re-arms a fresh
        // debounce and advances the snapshot/generation boundary. The newer
        // request fires; the older request's completion, tagged with the now-stale
        // generation/snapshot, is dropped (no commands).
        let mut machine = machine();

        // First edit → request gen 1 / snap 1.
        machine.on_event(text_changed("hello ", 6, 1000));
        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 1200 }),
            vec![Command::RequestCompletion {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                prompt: "hello".into(),
            }]
        );

        // Second edit while gen 1 is in flight → re-arms, advancing to gen 2 /
        // snap 2; the next Tick fires the newer request.
        machine.on_event(text_changed("hello there ", 12, 2000));
        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 2200 }),
            vec![Command::RequestCompletion {
                generation: 2,
                field: field("field-a"),
                snapshot: 2,
                prompt: "hello there".into(),
            }]
        );

        // The stale gen-1/snap-1 completion arriving late is discarded — nothing
        // is shown, the in-flight gen-2 request is untouched.
        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "world".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn transition_to_secure_blocks_subsequent_requests() {
        // Field starts Normal, then a secure-state change flips caps to secure.
        // Typing afterwards must never arm a request (privacy invariant, §7).
        let mut machine = machine();
        machine.on_event(Event::SecureStateChanged {
            caps: secure_caps(),
        });
        machine.on_event(text_changed("password", 8, 1000));

        assert_eq!(machine.on_event(Event::Tick { now_ms: 9999 }), vec![]);

        // Now leave secure mode (caps flip back to inline) and retype: a request
        // must fire on the next tick past debounce. This proves the secure caps
        // were the *sole* suppressor above — without it, the empty-vec assertion
        // could pass for an unrelated reason (e.g. a stale debounce or no caret).
        machine.on_event(Event::SecureStateChanged {
            caps: inline_caps(),
        });
        machine.on_event(text_changed("password", 8, 10_000));
        let commands = machine.on_event(Event::Tick { now_ms: 10_300 });
        assert!(commands.iter().any(|c| matches!(
            c,
            Command::RequestCompletion { prompt, .. } if prompt == "password"
        )));
    }

    #[test]
    fn secure_field_never_requests() {
        let mut machine = SuggestionMachine::new(secure_caps(), 200, 4);
        machine.on_event(text_changed("pw", 2, 0));

        assert_eq!(machine.on_event(Event::Tick { now_ms: 9999 }), vec![]);
    }

    fn showing_machine() -> SuggestionMachine {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            text: "a b".into(),
        });
        machine
    }

    #[test]
    fn text_change_while_showing_hides() {
        let mut machine = showing_machine();

        assert_eq!(
            machine.on_event(text_changed("xy", 2, 600)),
            vec![Command::Hide]
        );
    }

    #[test]
    fn caret_move_while_showing_hides() {
        let mut machine = showing_machine();

        assert_eq!(
            machine.on_event(Event::CaretMoved {
                field: field("field-a"),
                caret: 9,
            }),
            vec![Command::Hide]
        );
    }

    #[test]
    fn caret_move_to_different_field_hides() {
        let mut machine = showing_machine();

        assert_eq!(
            machine.on_event(Event::CaretMoved {
                field: field("field-b"),
                caret: 1,
            }),
            vec![Command::Hide]
        );
    }

    #[test]
    fn caret_move_same_position_keeps_showing() {
        let mut machine = showing_machine();

        assert_eq!(
            machine.on_event(Event::CaretMoved {
                field: field("field-a"),
                caret: 1,
            }),
            vec![]
        );
    }

    #[test]
    fn focus_change_while_showing_hides() {
        let mut machine = showing_machine();

        assert_eq!(
            machine.on_event(Event::Focus {
                field: field("field-b"),
                caps: inline_caps(),
            }),
            vec![Command::Hide]
        );
    }

    #[test]
    fn secure_state_change_while_showing_hides() {
        let mut machine = showing_machine();

        assert_eq!(
            machine.on_event(Event::SecureStateChanged {
                caps: secure_caps(),
            }),
            vec![Command::Hide]
        );
    }

    #[test]
    fn dismiss_hides() {
        let mut machine = showing_machine();

        assert_eq!(machine.on_event(Event::Dismiss), vec![Command::Hide]);
    }

    fn showing_candidates(texts: &[&str]) -> SuggestionMachine {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReadyMulti {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            candidates: texts.iter().map(|s| s.to_string()).collect(),
        });
        machine
    }

    #[test]
    fn multi_candidate_shows_the_first() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        assert_eq!(
            machine.on_event(Event::CompletionReadyMulti {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                candidates: vec!["alpha".into(), "beta".into()],
            }),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "alpha".into(),
            }]
        );
    }

    #[test]
    fn trailing_space_applies_to_the_exhausting_word_accept_of_a_multi_word() {
        // The trailing-space policy fires "when this accept completes the
        // suggestion (no rest)": mid-completion word accepts carry their
        // native separator space, and the FINAL word accept of a multi-word
        // completion gets the policy space appended — not bare "beta".
        let mut machine = SuggestionMachine::new(inline_caps(), 200, 4).with_trailing_space(true);
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            text: "alpha beta".into(),
        });

        // Mid-completion accept: native separator space, no policy double.
        assert_eq!(
            machine.on_event(Event::AcceptWord),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "alpha ".into(),
                },
                Command::UpdateGhost {
                    field: field("field-a"),
                    snapshot: 1,
                    text: "beta".into(),
                },
            ]
        );
        // Exhausting accept: the policy space lands on the final word.
        assert_eq!(
            machine.on_event(Event::AcceptWord),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "beta ".into(),
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn multi_candidate_promotes_a_survivor_when_the_first_is_filtered() {
        // The per-candidate shaping loop must filter each candidate
        // independently: a degenerate first candidate ("ha ha ha" trips
        // is_degenerate_repetition) drops out and the surviving second
        // candidate becomes the primary ghost — a regression to
        // "validate only the first" would show garbage or nothing.
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        assert_eq!(
            machine.on_event(Event::CompletionReadyMulti {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                candidates: vec!["ha ha ha".into(), "fresh text".into()],
            }),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "fresh text".into(),
            }]
        );
    }

    #[test]
    fn multi_candidate_mixed_failures_show_only_the_survivor() {
        // Each candidate fails for a DIFFERENT reason and one survives: with the
        // caret after "the quick" (right context " fox"), "fox" empties under the
        // overlap strip, "ha ha ha" trips degenerate repetition, and "brown text"
        // survives both shapers — only the survivor is shown. Exercises the
        // overlap-empties branch in the multi path (the existing survivor test
        // only covers the degenerate-drop branch).
        let mut machine = machine();
        machine.on_event(text_changed("the quick fox", 9, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReadyMulti {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                candidates: vec!["fox".into(), "ha ha ha".into(), "brown text".into()],
            }),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "brown text".into(),
            }]
        );
        // Only one candidate survived → a Cycle has nothing else to rotate to.
        assert_eq!(machine.on_event(Event::Cycle), vec![]);
    }

    #[test]
    fn multi_candidate_all_filtered_shows_nothing() {
        // When EVERY candidate fails shaping (both degenerate repetition), the
        // multi path shows nothing and leaves no stale Showing behind — the
        // "all garbage from a small model" outcome. The survivor test covers
        // first-dropped-second-survives; this covers none-survive.
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        assert_eq!(
            machine.on_event(Event::CompletionReadyMulti {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                candidates: vec!["ha ha ha".into(), "the the the".into()],
            }),
            vec![],
            "no candidate survives shaping → nothing shown"
        );
        // Nothing is showing, so a Cycle is a no-op (no stale Showing to rotate).
        assert_eq!(machine.on_event(Event::Cycle), vec![]);
    }

    #[test]
    fn duplicate_completion_ready_delivery_is_a_noop() {
        // The Event docs promise late completions are idempotent to deliver:
        // a successful show clears `requested`, so re-delivering the same
        // (generation, field, snapshot, text) must produce no commands and
        // no second Shown stat event.
        let mut machine = machine_showing();
        let _ = machine.take_stat_events(); // drain the first Shown

        let dup = machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            text: "world".into(),
        });

        assert_eq!(dup, vec![]);
        assert_eq!(
            machine.take_stat_events(),
            vec![],
            "no second Shown for a duplicate delivery"
        );
    }

    #[test]
    fn cycle_rotates_to_the_next_candidate_and_wraps() {
        let mut machine = showing_candidates(&["alpha", "beta", "gamma"]);
        assert_eq!(
            machine.on_event(Event::Cycle),
            vec![Command::UpdateGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "beta".into(),
            }]
        );
        assert_eq!(
            machine.on_event(Event::Cycle),
            vec![Command::UpdateGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "gamma".into(),
            }]
        );
        // Wraps back to the first.
        assert_eq!(
            machine.on_event(Event::Cycle),
            vec![Command::UpdateGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "alpha".into(),
            }]
        );
    }

    #[test]
    fn cycle_with_one_candidate_is_a_noop() {
        let mut machine = showing_candidates(&["solo"]);
        assert_eq!(machine.on_event(Event::Cycle), vec![]);
    }

    #[test]
    fn cycle_with_nothing_showing_is_a_noop() {
        let mut machine = machine();
        assert_eq!(machine.on_event(Event::Cycle), vec![]);
    }

    #[test]
    fn force_show_re_presents_the_current_candidate_verbatim() {
        // ForceShow (Item 4 always-on hotkey) re-asserts the suggestion the
        // engine already holds — same candidate, no rotation (unlike Cycle).
        let mut machine = showing_three_words();
        assert_eq!(
            machine.on_event(Event::ForceShow),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "world there friend".into(),
            }]
        );
        // Re-show does not advance the candidate index: a second ForceShow shows
        // the *same* candidate, never the next one (that is Cycle's job).
        assert_eq!(
            machine.on_event(Event::ForceShow),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "world there friend".into(),
            }]
        );
    }

    #[test]
    fn force_show_re_presenting_a_held_ghost_records_no_new_shown_stat() {
        // ForceShow re-asserts a candidate the engine ALREADY holds and already
        // counted as Shown when it first appeared. Re-presenting it (Item 4
        // hotkey, or recovering a failed placement) must NOT buffer a second
        // Shown — that would inflate the acceptance-rate denominator and the
        // usage "shown" count. The ForceShow arm intentionally omits record_stat;
        // every OTHER show-path records one, so a "consistency" regression that
        // added record_stat(StatEvent::Shown) here would pass every other
        // force_show_* test (they assert only the emitted Command) yet double-
        // count on each hotkey press.
        let mut machine = showing_three_words();
        // Drain the single Shown buffered by the initial real show.
        assert_eq!(machine.take_stat_events(), vec![StatEvent::Shown]);

        // Two re-presents in a row each emit a ShowGhost...
        assert!(matches!(
            machine.on_event(Event::ForceShow).as_slice(),
            [Command::ShowGhost { .. }]
        ));
        assert!(matches!(
            machine.on_event(Event::ForceShow).as_slice(),
            [Command::ShowGhost { .. }]
        ));
        // ...but neither buffers a new Shown (nor a Superseded — the same ghost
        // stayed up, it was not replaced).
        assert_eq!(
            machine.take_stat_events(),
            vec![],
            "re-presenting a held ghost via ForceShow must not re-count it as Shown"
        );
    }

    #[test]
    fn force_show_re_presents_the_cycled_candidate_not_the_first() {
        // ForceShow re-asserts whatever candidate is *currently selected*, which
        // may not be candidates[0] after a Cycle. The ForceShow handler reads
        // `showing.current()` independently of Accept/preview, so a regression
        // that reset the index or cached candidates[0] on the ForceShow path
        // would still pass the verbatim/no-rotation test above (it never cycles)
        // yet break here. This is the ForceShow analog of
        // `preview_reports_the_cycled_candidate_not_the_first` and
        // `accept_full_inserts_the_cycled_candidate`.
        let mut machine = showing_candidates(&["alpha", "beta"]);
        // Cycle advances the selection to the second candidate.
        assert_eq!(
            machine.on_event(Event::Cycle),
            vec![Command::UpdateGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "beta".into(),
            }]
        );
        // ForceShow must re-present "beta" (the cycled-to candidate), not "alpha".
        assert_eq!(
            machine.on_event(Event::ForceShow),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "beta".into(),
            }]
        );
    }

    #[test]
    fn force_show_after_partial_accept_re_presents_the_remaining_completion() {
        // A word-by-word `AcceptWord` that leaves a remainder does NOT clear
        // `showing`: the handler retains it, collapsed to a single candidate
        // holding only the *unaccepted* tail (`candidates == [rest]`, index 0).
        // ForceShow reads that live post-accept state, so it must re-present the
        // remaining tail — not the original full candidate, and not a no-op.
        //
        // This exercises the ForceShow × AcceptWord-partial combination that the
        // verbatim/cycled/no-op tests never reach: they only cover the pristine
        // pre-accept `showing`. A regression that cached the pre-accept candidate
        // on the ForceShow path, or that failed to collapse `showing` to `rest`,
        // would pass every other force_show_* test yet re-show the already-typed
        // word here.
        let mut machine = showing_three_words(); // "world there friend"
                                                 // Partially accept the first word; the tail "there friend" stays held.
        let accept = machine.on_event(Event::AcceptWord);
        assert!(
            accept.iter().any(|c| matches!(c, Command::Insert { .. })),
            "AcceptWord should insert the accepted word: {accept:?}"
        );
        assert!(
            !accept.iter().any(|c| matches!(c, Command::Hide)),
            "a partial accept must NOT hide the ghost (rest remains): {accept:?}"
        );
        // ForceShow now re-asserts the *remaining* completion, verbatim.
        assert_eq!(
            machine.on_event(Event::ForceShow),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "there friend".into(),
            }],
            "ForceShow after a partial accept must re-present only the unaccepted \
             tail (the live `showing` state), never the original full candidate"
        );
    }

    #[test]
    fn force_show_emits_no_new_completion_request() {
        let mut machine = showing_solo(false);
        let out = machine.on_event(Event::ForceShow);
        assert!(
            !out.iter()
                .any(|c| matches!(c, Command::RequestCompletion { .. })),
            "ForceShow must re-present the held suggestion, never kick a fresh \
             inference request: {out:?}"
        );
    }

    #[test]
    fn force_show_with_nothing_pending_is_a_noop() {
        // Nothing held (fresh machine) → no panic, no spurious render.
        let mut machine = machine();
        assert_eq!(machine.on_event(Event::ForceShow), vec![]);

        // And after a held suggestion is dismissed (so `showing` is cleared and
        // no candidate is retained), ForceShow is likewise a no-op.
        let mut machine = showing_solo(false);
        let _ = machine.on_event(Event::Dismiss);
        assert_eq!(machine.on_event(Event::ForceShow), vec![]);
    }

    #[test]
    fn force_show_does_not_resurrect_a_suggestion_staled_by_an_edit() {
        // A snapshot-invalidating edit hides the ghost AND clears `showing`, so
        // the held candidate (captured at snapshot 1) is gone. ForceShow re-shows
        // ONLY what is still in `showing`; after the edit there is nothing, so it
        // must be a no-op — never re-render the stale "world there friend" against
        // the now-current (snapshot 2) document. A regression that retained the
        // candidate would emit a ShowGhost at the old snapshot and clobber text.
        let mut machine = showing_three_words();
        assert_eq!(
            machine.on_event(text_changed("xy", 2, 600)),
            vec![Command::Hide]
        );
        assert_eq!(
            machine.on_event(Event::ForceShow),
            vec![],
            "ForceShow must not resurrect a suggestion staled by an edit"
        );
    }

    #[test]
    fn force_show_does_not_resurrect_a_suggestion_staled_by_a_caret_move() {
        // Same invariant via the caret-move hide path: moving the caret hides the
        // ghost and clears `showing`, so the subsequent ForceShow has nothing to
        // re-present and must be a no-op rather than re-rendering the stale ghost.
        let mut machine = showing_three_words();
        assert_eq!(
            machine.on_event(Event::CaretMoved {
                field: field("field-a"),
                caret: 9,
            }),
            vec![Command::Hide]
        );
        assert_eq!(
            machine.on_event(Event::ForceShow),
            vec![],
            "ForceShow must not resurrect a suggestion staled by a caret move"
        );
    }

    #[test]
    fn accept_full_inserts_the_cycled_candidate() {
        let mut machine = showing_candidates(&["alpha", "beta"]);
        machine.on_event(Event::Cycle); // now showing "beta"
        assert_eq!(
            machine.on_event(Event::AcceptFull),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "beta".into(),
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn preview_reports_the_cycled_candidate_not_the_first() {
        // preview_accept_insert reads showing.current(), so after a Cycle the
        // host's self-insert-echo absorption must preview the CYCLED candidate
        // ("beta"), not candidates[0] ("alpha"). A regression reading the first
        // candidate would still pass every accept test (which exercise the
        // accept path) yet desync the echo absorption on the cycled path —
        // pinned here for both Full and Word (single-word "beta" is its own
        // whole word, and the default machine() appends no trailing space).
        let mut machine = showing_candidates(&["alpha", "beta"]);
        machine.on_event(Event::Cycle); // now showing "beta"
        assert_eq!(
            machine.preview_accept_insert(AcceptAction::Full),
            Some((field("field-a"), "beta".into(), 0)),
            "Full preview must reflect the cycled candidate, not candidates[0]"
        );
        assert_eq!(
            machine.preview_accept_insert(AcceptAction::Word),
            Some((field("field-a"), "beta".into(), 0)),
            "Word preview must reflect the cycled candidate, not candidates[0]"
        );
    }

    #[test]
    fn accept_word_collapses_to_the_active_candidate() {
        // After a partial (word) accept the sibling candidates are stale — they
        // still begin with the just-accepted word — so cycling must not re-offer
        // them (review finding #1).
        let mut machine = showing_candidates(&["world there friend", "world other text"]);
        machine.on_event(Event::AcceptWord); // inserts "world ", keeps "there friend"
        assert_eq!(machine.on_event(Event::Cycle), vec![]);
    }

    #[test]
    fn accept_full_after_word_collapse_inserts_the_remaining_candidate() {
        // Follow-through after the collapse in
        // accept_word_collapses_to_the_active_candidate: a word-accept collapses
        // the multi-candidate set to the single active candidate ("world there
        // friend") with "world " already inserted; a subsequent AcceptFull must
        // insert exactly the REMAINDER ("there friend"), not the original full
        // candidate or a sibling, then hide.
        let mut machine = showing_candidates(&["world there friend", "world other text"]);
        machine.on_event(Event::AcceptWord); // inserts "world ", keeps "there friend"

        assert_eq!(
            machine.on_event(Event::AcceptFull),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "there friend".into(),
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn near_duplicate_candidates_are_deduped() {
        // Candidates differing only by trailing space / case collapse to one
        // (review finding #4), so cycling never shows a visual duplicate.
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReadyMulti {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            candidates: vec![
                "Hello there".into(),
                "hello there ".into(),
                "other one".into(),
            ],
        });
        // Only "Hello there" and "other one" survive → one Cycle reaches "other
        // one", the next wraps back (no third near-duplicate).
        assert_eq!(
            machine.on_event(Event::Cycle),
            vec![Command::UpdateGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "other one".into(),
            }]
        );
        assert_eq!(
            machine.on_event(Event::Cycle),
            vec![Command::UpdateGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "Hello there".into(),
            }]
        );
    }

    #[test]
    fn duplicate_candidates_are_deduped() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReadyMulti {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            candidates: vec!["same".into(), "same".into(), "other".into()],
        });
        // Two distinct candidates survive → one Cycle reaches "other", the next
        // wraps to "same" (not a third identical entry).
        assert_eq!(
            machine.on_event(Event::Cycle),
            vec![Command::UpdateGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "other".into(),
            }]
        );
        assert_eq!(
            machine.on_event(Event::Cycle),
            vec![Command::UpdateGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "same".into(),
            }]
        );
    }

    #[test]
    fn dismiss_suppress_hides() {
        let mut machine = showing_machine();

        assert_eq!(
            machine.on_event(Event::DismissSuppress),
            vec![Command::Hide]
        );
    }

    #[test]
    fn dismiss_suppress_resumes_on_the_next_edit() {
        // Esc (DismissSuppress) suppresses the current suggestion in the current
        // field until the user edits or refocuses it; the clearing edit itself
        // should be eligible to arm the next request.
        let mut machine = showing_machine();
        machine.on_event(Event::DismissSuppress);

        // First edit after Esc: clears suppression and arms normally.
        machine.on_event(text_changed("xy", 2, 1000));
        assert!(matches!(
            machine.on_event(Event::Tick { now_ms: 1200 }).as_slice(),
            [Command::RequestCompletion { .. }]
        ));
    }

    #[test]
    fn focus_to_other_field_clears_suppression() {
        let mut machine = showing_machine();
        machine.on_event(Event::DismissSuppress);

        // Refocusing (a different field) clears suppression: the next edit arms.
        machine.on_event(Event::Focus {
            field: field("field-b"),
            caps: inline_caps(),
        });
        machine.on_event(Event::TextChanged {
            field: field("field-b"),
            value: "hello".into(),
            caret: 5,
            edit: EditKind::Insert,
            trigger: TriggerPolicy::Automatic,
            now_ms: 3000,
        });
        assert!(matches!(
            machine.on_event(Event::Tick { now_ms: 3200 }).as_slice(),
            [Command::RequestCompletion { .. }]
        ));
    }

    #[test]
    fn dismiss_suppress_blocks_an_inflight_completion() {
        // A request already in flight when Esc is pressed must not pop a ghost
        // back up after the dismiss.
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 }); // requested gen=1, snapshot=1
        machine.on_event(Event::DismissSuppress);

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "late ghost".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn caret_move_does_not_clear_suppression() {
        // Esc-suppression is cleared ONLY by Focus and TextChanged (the two paths
        // that set `suppressed = false`); a CaretMoved must NOT clear it. This
        // pins that deliberate non-clear: a no-op caret move (same field+caret)
        // leaves the field suppressed, so a local replacement offer is still
        // blocked. The mirror of `focus_to_other_field_clears_suppression`.
        let mut machine = focused_machine();
        let f = field("field-a");
        machine.on_event(Event::DismissSuppress); // suppress the current field
        machine.on_event(Event::CaretMoved {
            field: f.clone(),
            caret: 0, // same field+caret the machine already tracks → no-op move
        });

        // Suppression survived the caret move: the field is still blocked, so an
        // offer (gated by `self.suppressed`) produces nothing and shows no ghost.
        assert_eq!(
            machine.offer_replacement(&f, "\u{1F604}".into(), 5),
            vec![],
            "a no-op CaretMoved must not clear Esc-suppression"
        );
        assert!(machine.showing.is_none());
    }

    #[test]
    fn dismiss_discard_blocks_an_inflight_completion() {
        // The tray Disable path (`Event::DismissDiscard`) must stale an in-flight
        // request: dropping only the queued requests leaves one already submitted
        // to the inference worker, which would otherwise re-show a ghost after the
        // user disabled the app.
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 }); // requested gen=1, snapshot=1
        machine.on_event(Event::DismissDiscard);

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "late ghost".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn mutverify_dismiss_discard_does_not_suppress_field() {
        let mut machine = focused_machine();
        machine.on_event(Event::DismissDiscard);
        assert!(
            !machine
                .offer_replacement(&field("field-a"), "\u{1F604}".into(), 5)
                .is_empty(),
            "DismissDiscard must not suppress the field"
        );
    }

    #[test]
    fn dismiss_discard_cancels_an_armed_unfired_debounce() {
        // text_changed ARMS a debounce (pending_since set) but no Tick has fired
        // the request yet. DismissDiscard clears pending_since (and advances the
        // snapshot), so a later Tick past the debounce window finds nothing armed
        // and emits no RequestCompletion. Distinct from the in-flight tests above,
        // which arm AND fire before dismissing.
        let mut machine = machine();
        machine.on_event(text_changed("hello ", 6, 1000)); // arms, but not yet fired
        machine.on_event(Event::DismissDiscard);
        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 2000 }),
            vec![],
            "DismissDiscard must cancel an armed-but-unfired debounce"
        );
    }

    #[test]
    fn dismiss_suppress_cancels_an_armed_unfired_debounce() {
        // Same as the DismissDiscard case but via Esc: an armed-but-unfired
        // debounce is cancelled, so a later Tick emits no RequestCompletion.
        let mut machine = machine();
        machine.on_event(text_changed("hello ", 6, 1000)); // arms, but not yet fired
        machine.on_event(Event::DismissSuppress);
        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 2000 }),
            vec![],
            "DismissSuppress must cancel an armed-but-unfired debounce"
        );
    }

    #[test]
    fn dismiss_keeps_an_armed_debounce_so_the_request_still_fires() {
        // Plain Dismiss is snapshot-neutral and leaves pending_since intact (it
        // only hides a ghost). So an armed-but-unfired debounce survives a Dismiss
        // and a later Tick past the window still fires the RequestCompletion —
        // the contrast to the two cancelling dismiss variants above.
        let mut machine = machine();
        machine.on_event(text_changed("hello ", 6, 1000)); // arms, but not yet fired
        machine.on_event(Event::Dismiss);
        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 1300 }),
            vec![Command::RequestCompletion {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                prompt: "hello".into(),
            }],
            "plain Dismiss must not cancel an armed debounce"
        );
    }

    #[test]
    fn dismiss_is_snapshot_neutral_and_keeps_an_inflight_completion() {
        // Plain `Event::Dismiss` is the idempotent show-failed reconciliation: it
        // hides without advancing the snapshot, so a completion already requested
        // for the current snapshot still shows when it arrives. (Regression guard:
        // the tray-disable fix must NOT leak snapshot-advance into this path.)
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 }); // requested gen=1, snapshot=1
        machine.on_event(Event::Dismiss);

        assert!(matches!(
            machine
                .on_event(Event::CompletionReady {
                    generation: 1,
                    field: field("field-a"),
                    snapshot: 1,
                    text: "still valid".into(),
                })
                .as_slice(),
            [Command::ShowGhost { .. }]
        ));
    }

    /// Drive a machine to a showing ghost and drain the resulting Shown event,
    /// returning the machine ready for a supersede/accept/dismiss step.
    fn machine_showing() -> SuggestionMachine {
        let mut machine = machine();
        machine.on_event(text_changed("hello", 5, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            text: "world".into(),
        });
        machine
    }

    #[test]
    fn shown_stat_event_recorded_when_a_ghost_is_presented() {
        let mut machine = machine();
        machine.on_event(text_changed("hello", 5, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        let cmds = machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            text: "world".into(),
        });
        assert!(cmds.iter().any(|c| matches!(c, Command::ShowGhost { .. })));
        assert_eq!(machine.take_stat_events(), vec![StatEvent::Shown]);
        // Drained: a second take is empty.
        assert_eq!(machine.take_stat_events(), vec![]);
    }

    #[test]
    fn typing_over_a_showing_ghost_records_superseded() {
        let mut machine = machine_showing();
        assert_eq!(machine.take_stat_events(), vec![StatEvent::Shown]);
        // The user keeps typing → the visible ghost is replaced, not acted on.
        machine.on_event(text_changed("hello w", 7, 0));
        assert_eq!(machine.take_stat_events(), vec![StatEvent::Superseded]);
    }

    #[test]
    fn fresh_completion_replacing_a_showing_ghost_records_superseded() {
        // Defensive same-module setup: normal public `on_event` flow clears
        // `requested` after the first matching completion, and re-arming through
        // TextChanged hides/stales the old ghost first. Seeding the guarded state
        // is the direct way to pin the replacement-site accounting branch.
        let mut machine = machine_showing();
        assert_eq!(machine.take_stat_events(), vec![StatEvent::Shown]);
        machine.requested = Some(RequestedCompletion {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            kind: RequestedKind::Completion,
        });

        assert!(matches!(
            machine
                .on_event(Event::CompletionReady {
                    generation: 1,
                    field: field("field-a"),
                    snapshot: 1,
                    text: "again".into(),
                })
                .as_slice(),
            [Command::ShowGhost { text, .. }] if text == "again"
        ));
        assert_eq!(
            machine.take_stat_events(),
            vec![StatEvent::Superseded, StatEvent::Shown]
        );
    }

    #[test]
    fn focus_change_over_a_showing_ghost_records_superseded() {
        let mut machine = machine_showing();
        let _ = machine.take_stat_events(); // drop the Shown
        machine.on_event(Event::Focus {
            field: field("field-b"),
            caps: inline_caps(),
        });
        assert_eq!(machine.take_stat_events(), vec![StatEvent::Superseded]);
    }

    #[test]
    fn accept_and_dismiss_do_not_record_superseded() {
        // Accept is a user action, not a supersede. Anchor the pre-drain to the
        // Shown so the post-action empty assertion can't pass on a broken `take`.
        let mut accepted = machine_showing();
        assert_eq!(accepted.take_stat_events(), vec![StatEvent::Shown]);
        accepted.on_event(Event::AcceptFull);
        assert_eq!(accepted.take_stat_events(), vec![]);

        // Dismiss (Esc) is a user action, not a supersede.
        let mut dismissed = machine_showing();
        assert_eq!(dismissed.take_stat_events(), vec![StatEvent::Shown]);
        dismissed.on_event(Event::DismissSuppress);
        assert_eq!(dismissed.take_stat_events(), vec![]);
    }

    #[test]
    fn caret_move_over_a_showing_ghost_records_superseded() {
        let mut machine = machine_showing();
        let _ = machine.take_stat_events(); // drop the Shown
        machine.on_event(Event::CaretMoved {
            field: field("field-a"),
            caret: 2, // moved away from caret 5 → ghost invalidated
        });
        assert_eq!(machine.take_stat_events(), vec![StatEvent::Superseded]);
    }

    #[test]
    fn noop_caret_move_keeps_the_ghost_and_records_nothing() {
        let mut machine = machine_showing();
        let _ = machine.take_stat_events();
        // Same field + same caret (5, after "hello") → not moved → ghost stays.
        machine.on_event(Event::CaretMoved {
            field: field("field-a"),
            caret: 5,
        });
        assert_eq!(machine.take_stat_events(), vec![]);
    }

    #[test]
    fn secure_state_change_over_a_showing_ghost_records_superseded() {
        let mut machine = machine_showing();
        let _ = machine.take_stat_events();
        machine.on_event(Event::SecureStateChanged {
            caps: inline_caps(),
        });
        assert_eq!(machine.take_stat_events(), vec![StatEvent::Superseded]);
    }

    #[test]
    fn no_supersede_when_nothing_is_showing() {
        // The `was_showing` half of the guard: a non-user event with no ghost up
        // must not record a supersede.
        let mut machine = machine();
        machine.on_event(text_changed("hi", 2, 0));
        machine.on_event(Event::Focus {
            field: field("field-b"),
            caps: inline_caps(),
        });
        assert_eq!(machine.take_stat_events(), vec![]);
    }

    #[test]
    fn cycle_and_word_accept_keep_the_ghost_without_extra_events() {
        // Cycle rotates candidates (UpdateGhost, not ShowGhost) → no new Shown,
        // not a supersede. Word-accept with remaining text keeps the ghost too.
        let mut machine = showing_three_words();
        let _ = machine.take_stat_events(); // drop the initial Shown
        machine.on_event(Event::Cycle);
        assert_eq!(machine.take_stat_events(), vec![]);
        machine.on_event(Event::AcceptWord);
        assert_eq!(machine.take_stat_events(), vec![]);
    }

    #[test]
    fn stat_events_accumulate_across_turns_until_drained() {
        // Two show cycles without an intermediate drain: cycle-2's typing
        // supersedes cycle-1's ghost, so the buffer holds [Shown, Superseded,
        // Shown]; a second drain is empty.
        let mut machine = machine();
        machine.on_event(text_changed("hello", 5, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            text: "world".into(),
        });
        // New edit supersedes the showing ghost and arms a fresh request.
        machine.on_event(text_changed("hello world ", 12, 1000));
        machine.on_event(Event::Tick { now_ms: 1500 });
        machine.on_event(Event::CompletionReady {
            generation: 2,
            field: field("field-a"),
            snapshot: 2,
            text: "again".into(),
        });
        assert_eq!(
            machine.take_stat_events(),
            vec![StatEvent::Shown, StatEvent::Superseded, StatEvent::Shown]
        );
        assert_eq!(machine.take_stat_events(), vec![]);
    }

    #[test]
    fn cancel_last_shown_removes_only_the_trailing_shown() {
        // The host calls this when an overlay placement failed: the emitted-but-
        // never-presented ghost must not be counted as shown. (Scoped so the
        // local `machine` binding doesn't shadow the `machine()` constructor
        // used below.)
        {
            let mut machine = machine_showing();
            machine.cancel_last_shown();
            assert_eq!(machine.take_stat_events(), vec![]);
        }

        // With an interleaved buffer [Shown, Superseded, Shown] (two show cycles,
        // the first superseded), only the TRAILING Shown — the one whose
        // placement just failed — is retracted; the earlier, successfully-
        // presented Shown and the Superseded between them are preserved. This
        // pins the `rposition` choice: a regression to `position` would drop the
        // first Shown and corrupt shown-count stats.
        let mut machine = machine();
        machine.on_event(text_changed("hello", 5, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            text: "world".into(),
        });
        machine.on_event(text_changed("hello world ", 12, 1000));
        machine.on_event(Event::Tick { now_ms: 1500 });
        machine.on_event(Event::CompletionReady {
            generation: 2,
            field: field("field-a"),
            snapshot: 2,
            text: "again".into(),
        });
        machine.cancel_last_shown();
        assert_eq!(
            machine.take_stat_events(),
            vec![StatEvent::Shown, StatEvent::Superseded]
        );
    }

    fn showing_three_words() -> SuggestionMachine {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            text: "world there friend".into(),
        });
        machine
    }

    #[test]
    fn accept_full_inserts_all_and_hides() {
        let mut machine = showing_three_words();

        assert_eq!(
            machine.on_event(Event::AcceptFull),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "world there friend".into(),
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn accept_word_inserts_word_and_updates_ghost() {
        let mut machine = showing_three_words();

        assert_eq!(
            machine.on_event(Event::AcceptWord),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "world ".into(),
                },
                Command::UpdateGhost {
                    field: field("field-a"),
                    snapshot: 1,
                    text: "there friend".into(),
                },
            ]
        );
    }

    #[test]
    fn preview_accept_word_reports_inserted_word() {
        let machine = showing_three_words();

        assert_eq!(
            machine.preview_accept_insert(AcceptAction::Word),
            Some((field("field-a"), "world ".into(), 0))
        );
    }

    #[test]
    fn preview_accept_full_reports_remaining_completion() {
        let machine = showing_three_words();

        assert_eq!(
            machine.preview_accept_insert(AcceptAction::Full),
            Some((field("field-a"), "world there friend".into(), 0))
        );
    }

    #[test]
    fn accept_last_word_hides() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            text: "solo".into(),
        });

        assert_eq!(
            machine.on_event(Event::AcceptWord),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "solo".into(),
                },
                Command::Hide,
            ]
        );
    }

    fn showing_solo(trailing: bool) -> SuggestionMachine {
        let mut machine =
            SuggestionMachine::new(inline_caps(), 200, 4).with_trailing_space(trailing);
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            text: "solo".into(),
        });
        machine
    }

    #[test]
    fn append_single_word_space_adds_only_when_enabled_and_single_word() {
        // Enabled + single word, no existing trailing space → one space added.
        assert_eq!(append_single_word_space("solo", true), "solo ");
        // Disabled → unchanged regardless of shape.
        assert_eq!(append_single_word_space("solo", false), "solo");
        // Multi-word → never touched (self-gating), even when enabled.
        assert_eq!(append_single_word_space("a b", true), "a b");
        // Already ends in whitespace (e.g. `next_word`'s mid-completion word) →
        // no double space.
        assert_eq!(append_single_word_space("world ", true), "world ");
        // Empty → unchanged (no spurious lone space).
        assert_eq!(append_single_word_space("", true), "");
        // Trailing punctuation is still a single word → space appended.
        assert_eq!(append_single_word_space("hi!", true), "hi! ");
    }

    #[test]
    fn set_trailing_space_reverse_flip_stops_the_append() {
        // review-c110 optional gap: the true→false direction, pinned for
        // symmetry with the forward flip below.
        let mut machine = showing_solo(true);
        machine.set_trailing_space(false);
        assert_eq!(
            machine.on_event(Event::AcceptFull),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "solo".into(),
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn set_trailing_space_flips_the_append_at_runtime() {
        // The General-tab switch flips this live (c110), like mid-word: a
        // machine built without the space starts appending after the setter.
        let mut machine = showing_solo(false);
        machine.set_trailing_space(true);
        assert_eq!(
            machine.on_event(Event::AcceptFull),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "solo ".into(),
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn full_accept_single_word_appends_trailing_space_when_enabled() {
        let mut machine = showing_solo(true);
        assert_eq!(
            machine.on_event(Event::AcceptFull),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "solo ".into(),
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn word_accept_single_word_appends_trailing_space_when_enabled() {
        let mut machine = showing_solo(true);
        assert_eq!(
            machine.on_event(Event::AcceptWord),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "solo ".into(),
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn single_word_default_off_is_unchanged() {
        let mut machine = showing_solo(false);
        assert_eq!(
            machine.on_event(Event::AcceptFull),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "solo".into(),
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn multi_word_accept_is_unaffected_by_trailing_space_flag() {
        // Full-accept of a multi-word completion: no trailing space added.
        let mut machine = showing_three_words();
        machine = machine.with_trailing_space(true);
        // Re-seed showing under the new flag (with_trailing_space consumes self).
        machine.on_event(text_changed("xy", 2, 0));
        machine.on_event(Event::Tick { now_ms: 1500 });
        machine.on_event(Event::CompletionReady {
            generation: 2,
            field: field("field-a"),
            snapshot: 2,
            text: "world there friend".into(),
        });
        assert_eq!(
            machine.on_event(Event::AcceptFull),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "world there friend".into(),
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn word_accept_first_of_many_keeps_single_native_space() {
        // First word of a multi-word completion already carries its own space;
        // the flag must not add a second one.
        let mut machine = showing_solo(true); // reuse builder for the flag
        machine.on_event(text_changed("xy", 2, 0));
        machine.on_event(Event::Tick { now_ms: 1500 });
        machine.on_event(Event::CompletionReady {
            generation: 2,
            field: field("field-a"),
            snapshot: 2,
            text: "world there".into(),
        });
        let out = machine.on_event(Event::AcceptWord);
        assert_eq!(
            out[0],
            Command::Insert {
                field: field("field-a"),
                text: "world ".into(),
            }
        );
    }

    #[test]
    fn preview_matches_inserted_bytes_under_trailing_space() {
        // The host absorbs the self-insert echo using `preview`, so preview must
        // equal what `on_event` inserts — including the trailing space.
        let machine = showing_solo(true);
        assert_eq!(
            machine.preview_accept_insert(AcceptAction::Full),
            Some((field("field-a"), "solo ".into(), 0))
        );
        assert_eq!(
            machine.preview_accept_insert(AcceptAction::Word),
            Some((field("field-a"), "solo ".into(), 0))
        );
    }

    /// A machine with `field-a` focused (so `offer_replacement`'s field-identity
    /// guard passes). Focus advances the snapshot to 1.
    fn focused_machine() -> SuggestionMachine {
        let mut machine = machine();
        machine.on_event(Event::Focus {
            field: field("field-a"),
            caps: inline_caps(),
        });
        machine
    }

    #[test]
    fn offer_replacement_shows_ghost_then_full_accept_emits_replace() {
        let mut machine = focused_machine();
        let f = field("field-a");
        assert_eq!(
            machine.offer_replacement(&f, "😄".into(), 5),
            vec![Command::ShowGhost {
                field: f.clone(),
                snapshot: 1,
                text: "😄".into(),
            }]
        );
        assert!(machine.take_stat_events().contains(&StatEvent::Shown));
        // Accept deletes the typed ":smile" (5 chars) and inserts the glyph.
        assert_eq!(
            machine.on_event(Event::AcceptFull),
            vec![
                Command::Replace {
                    field: f.clone(),
                    text: "😄".into(),
                    replace_left: 5,
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn offer_replacement_word_accept_also_replaces_atomic_token() {
        let mut machine = focused_machine();
        let f = field("field-a");
        machine.offer_replacement(&f, "😄".into(), 5);
        // A replacement is a single atomic token: Word-accept completes it
        // (no rest) and carries the deletion, exactly like Full.
        assert_eq!(
            machine.on_event(Event::AcceptWord),
            vec![
                Command::Replace {
                    field: f,
                    text: "😄".into(),
                    replace_left: 5,
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn offer_replacement_blocked_when_suppressed_or_empty() {
        let f = field("field-a");
        // Post-Esc suppression blocks a local offer.
        let mut suppressed = focused_machine();
        suppressed.on_event(Event::DismissSuppress);
        assert_eq!(suppressed.offer_replacement(&f, "😄".into(), 5), vec![]);
        // Observable: nothing is offered, so there is nothing to preview or
        // accept — a later Accept inserts nothing (refactor-survivable vs. a
        // private `showing.is_none()` probe).
        assert_eq!(suppressed.preview_accept_insert(AcceptAction::Full), None);
        assert_eq!(suppressed.on_event(Event::AcceptFull), vec![]);
        // Empty text never offers (no spurious ghost).
        let mut machine = focused_machine();
        assert_eq!(machine.offer_replacement(&f, String::new(), 3), vec![]);
        assert_eq!(machine.preview_accept_insert(AcceptAction::Full), None);
        assert_eq!(machine.on_event(Event::AcceptFull), vec![]);
    }

    #[test]
    fn offer_replacement_rejects_zero_delete_count() {
        let f = field("field-a");
        let mut machine = focused_machine();

        assert_eq!(machine.offer_replacement(&f, "the".into(), 0), vec![]);
        assert!(machine.showing.is_none());
        assert_eq!(machine.preview_accept_insert(AcceptAction::Full), None);
        assert!(!machine.take_stat_events().contains(&StatEvent::Shown));
        assert_eq!(machine.on_event(Event::AcceptFull), vec![]);
    }

    #[test]
    fn stat_events_buffer_is_capped_when_host_never_drains() {
        // A host that stops draining must not grow the buffer without bound.
        // record_stat caps at STAT_EVENTS_CAP; past that, advisory stats drop.
        let mut machine = focused_machine();
        for _ in 0..(STAT_EVENTS_CAP + 50) {
            machine.record_stat(StatEvent::Shown);
        }
        assert_eq!(machine.take_stat_events().len(), STAT_EVENTS_CAP);
        // After draining, the buffer is empty and accepts events again.
        machine.record_stat(StatEvent::Superseded);
        assert_eq!(machine.take_stat_events().len(), 1);
    }

    #[test]
    fn offer_replacement_blocked_in_secure_or_unsupported_field() {
        // Security-critical gate: a secure field (password) is `UxMode::Blocked`,
        // so `enabled()` is false and no replacement ghost may be offered — a
        // replacement must never surface a glyph/synonym into a password field.
        // This is the `!self.enabled()` branch of `offer_replacement`.
        let mut secure = machine();
        secure.on_event(Event::Focus {
            field: field("field-a"),
            caps: secure_caps(),
        });
        assert_eq!(
            secure.offer_replacement(&field("field-a"), "😄".into(), 5),
            vec![]
        );
        assert!(secure.showing.is_none());
        assert!(!secure.take_stat_events().contains(&StatEvent::Shown));
    }

    #[test]
    fn offer_replacement_blocked_when_field_is_not_focused() {
        // Focus-race guard: an offer for a field other than the focused one (or
        // when nothing is focused) is dropped — no ghost tagged to a stale field.
        let mut focused = focused_machine(); // field-a focused
        assert_eq!(
            focused.offer_replacement(&field("other-field"), "😄".into(), 5),
            vec![]
        );
        assert!(focused.showing.is_none());
        let mut unfocused = machine();
        assert_eq!(
            unfocused.offer_replacement(&field("field-a"), "😄".into(), 5),
            vec![]
        );
    }

    #[test]
    fn offer_replacement_disarms_pending_model_request_so_it_cannot_supersede() {
        let mut machine = focused_machine();
        // An edit arms the debounce for a model completion (same turn the host
        // detects an emoji/typo and offers a replacement).
        machine.on_event(text_changed("color", 5, 0));
        machine.offer_replacement(&field("field-a"), "😄".into(), 5);
        let _ = machine.take_stat_events();
        // The debounce tick must NOT fire a model request — the offer preempted it.
        let tick = machine.on_event(Event::Tick { now_ms: 10_000 });
        assert!(
            !tick
                .iter()
                .any(|c| matches!(c, Command::RequestCompletion { .. })),
            "model request armed despite a local replacement offer: {tick:?}"
        );
        // The replacement ghost is still the one showing (not superseded).
        assert_eq!(
            machine.preview_accept_insert(AcceptAction::Full),
            Some((field("field-a"), "😄".into(), 5))
        );
    }

    #[test]
    fn offer_replacement_drops_a_prior_in_flight_completion_that_returns_after() {
        // The other half of the disarm guarantee (the sibling test pins the
        // freshly-armed debounce tick): a model request that was *already
        // in-flight* when the offer was made must not match-and-supersede the
        // replacement ghost when its completion finally returns. `offer_replacement`
        // clears `requested`, so the late completion fails the `matches_request`
        // guard in `on_completion_ready` and is dropped.
        let mut machine = focused_machine();
        // Arm and actually issue a model request (debounce elapsed).
        machine.on_event(text_changed("color", 5, 0));
        let issued = machine.on_event(Event::Tick { now_ms: 10_000 });
        let req = issued
            .iter()
            .find_map(|c| match c {
                Command::RequestCompletion {
                    generation,
                    snapshot,
                    ..
                } => Some((*generation, *snapshot)),
                _ => None,
            })
            .expect("a model request must have been issued");
        // The host detects an emoji/typo on the same snapshot and offers a
        // replacement — this disarms the in-flight request.
        machine.offer_replacement(&field("field-a"), "😄".into(), 5);
        let _ = machine.take_stat_events();
        // The previously-issued completion now returns (same generation+snapshot
        // it was requested with). It must be ignored — no ghost command at all.
        let late = machine.on_event(Event::CompletionReady {
            generation: req.0,
            field: field("field-a"),
            snapshot: req.1,
            text: "colorful".into(),
        });
        assert!(
            late.is_empty(),
            "a disarmed in-flight completion produced commands: {late:?}"
        );
        // The replacement ghost is untouched — still the one showing.
        assert_eq!(
            machine.preview_accept_insert(AcceptAction::Full),
            Some((field("field-a"), "😄".into(), 5))
        );
    }

    #[test]
    fn offer_replacement_multi_shows_ghost_and_supports_cycling() {
        let mut machine = focused_machine();
        let f = field("field-a");
        let candidates = vec!["large".into(), "huge".into()];
        assert_eq!(
            machine.offer_replacement_multi(&f, candidates, 3),
            vec![Command::ShowGhost {
                field: f.clone(),
                snapshot: 1,
                text: "large".into(),
            }]
        );
        // Cycle to the second candidate.
        assert_eq!(
            machine.on_event(Event::Cycle),
            vec![Command::UpdateGhost {
                field: f.clone(),
                snapshot: 1,
                text: "huge".into(),
            }]
        );
        // Accept the second candidate.
        assert_eq!(
            machine.on_event(Event::AcceptFull),
            vec![
                Command::Replace {
                    field: f,
                    text: "huge".into(),
                    replace_left: 3,
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn offer_replacement_multi_skips_empty_candidates_and_shows_the_first_nonempty() {
        // The single-candidate path rejects empty text outright; the multi seed
        // must hold the same contract so a malformed vec like ["", "huge"] never
        // shows or accepts a blank ghost. The empty entry is dropped and the
        // first NON-empty candidate ("huge") is shown.
        let mut machine = focused_machine();
        let f = field("field-a");
        assert_eq!(
            machine.offer_replacement_multi(&f, vec!["".into(), "huge".into()], 3),
            vec![Command::ShowGhost {
                field: f.clone(),
                snapshot: 1,
                text: "huge".into(),
            }]
        );
        // An all-empty vec yields no offer at all (post-filter empty).
        let mut machine = focused_machine();
        assert!(
            machine
                .offer_replacement_multi(&field("field-a"), vec!["".into(), "".into()], 3)
                .is_empty(),
            "all-empty candidates must produce no offer"
        );
    }

    #[test]
    fn offer_replacement_multi_dedups_candidates_so_cycle_never_repeats() {
        // The model/single paths dedup; the multi seed must too, or Cycle lands
        // on the same word twice. ["huge","huge","big"] → after dedup the second
        // Cycle target is "big", not a repeated "huge".
        let mut machine = focused_machine();
        let f = field("field-a");
        assert_eq!(
            machine.offer_replacement_multi(
                &f,
                vec!["huge".into(), "huge".into(), "big".into()],
                3
            ),
            vec![Command::ShowGhost {
                field: f.clone(),
                snapshot: 1,
                text: "huge".into(),
            }]
        );
        assert_eq!(
            machine.on_event(Event::Cycle),
            vec![Command::UpdateGhost {
                field: f,
                snapshot: 1,
                text: "big".into(),
            }],
            "Cycle must skip the duplicate and advance to the next distinct candidate"
        );
    }

    #[test]
    fn offer_replacement_only_on_axset_fields() {
        // A non-range-replace field (SyntheticKeys/Clipboard) can't honor the
        // deletion, so no replacement is offered there (avoids `:smile😄` + a
        // desynced host diff baseline).
        let mut caps = inline_caps();
        caps.insert_strategy = InsertStrategy::SyntheticKeys;
        let mut machine = SuggestionMachine::new(caps.clone(), 200, 4);
        machine.on_event(Event::Focus {
            field: field("field-a"),
            caps,
        });
        assert_eq!(
            machine.offer_replacement(&field("field-a"), "😄".into(), 5),
            vec![]
        );
        // Observable: no ghost was offered, so there is nothing to preview or
        // accept (refactor-survivable vs. a private `showing.is_none()` probe).
        assert_eq!(machine.preview_accept_insert(AcceptAction::Full), None);
        assert_eq!(machine.on_event(Event::AcceptFull), vec![]);
    }

    #[test]
    fn offer_replacement_multi_blocked_when_suppressed_or_empty() {
        // Production entry point (Engine::on_replacement calls this): the multi
        // path must honor the same suppression/empty guards as the single path.
        let f = field("field-a");
        // Post-Esc suppression blocks a local multi offer.
        let mut suppressed = focused_machine();
        suppressed.on_event(Event::DismissSuppress);
        assert_eq!(
            suppressed.offer_replacement_multi(&f, vec!["😄".into()], 5),
            vec![]
        );
        assert!(suppressed.showing.is_none());
        assert!(!suppressed.take_stat_events().contains(&StatEvent::Shown));
        // An empty candidate vec never offers (no spurious ghost).
        let mut machine = focused_machine();
        assert_eq!(machine.offer_replacement_multi(&f, vec![], 3), vec![]);
        assert!(machine.showing.is_none());
    }

    #[test]
    fn offer_replacement_multi_rejects_zero_delete_count() {
        // A replacement with replace_left == 0 is malformed (nothing to delete);
        // the multi seed rejects it exactly like the single path.
        let f = field("field-a");
        let mut machine = focused_machine();
        assert_eq!(
            machine.offer_replacement_multi(&f, vec!["the".into()], 0),
            vec![]
        );
        assert!(machine.showing.is_none());
        assert_eq!(machine.preview_accept_insert(AcceptAction::Full), None);
        assert!(!machine.take_stat_events().contains(&StatEvent::Shown));
        assert_eq!(machine.on_event(Event::AcceptFull), vec![]);
    }

    #[test]
    fn offer_replacement_multi_blocked_in_secure_or_unsupported_field() {
        // Security-critical gate: a secure field (password) is `UxMode::Blocked`,
        // so `enabled()` is false and no replacement ghost may be offered via the
        // multi path either — a synonym/glyph must never surface in a password
        // field. This is the `!self.enabled()` branch of `offer_replacement_multi`.
        let mut secure = machine();
        secure.on_event(Event::Focus {
            field: field("field-a"),
            caps: secure_caps(),
        });
        assert_eq!(
            secure.offer_replacement_multi(&field("field-a"), vec!["😄".into()], 5),
            vec![]
        );
        assert!(secure.showing.is_none());
        assert!(!secure.take_stat_events().contains(&StatEvent::Shown));
    }

    #[test]
    fn offer_replacement_multi_only_on_axset_fields() {
        // A non-range-replace field (SyntheticKeys/Clipboard) can't honor the
        // deletion, so no multi replacement is offered there — same guard as the
        // single path (`insert_strategy != AxSet`).
        let mut caps = inline_caps();
        caps.insert_strategy = InsertStrategy::SyntheticKeys;
        let mut machine = SuggestionMachine::new(caps.clone(), 200, 4);
        machine.on_event(Event::Focus {
            field: field("field-a"),
            caps,
        });
        assert_eq!(
            machine.offer_replacement_multi(&field("field-a"), vec!["😄".into()], 5),
            vec![]
        );
        // Observable: no ghost was offered, so there is nothing to preview or
        // accept, and no Shown stat is buffered (refactor-survivable vs. a
        // private `showing.is_none()` probe).
        assert_eq!(machine.preview_accept_insert(AcceptAction::Full), None);
        assert_eq!(machine.on_event(Event::AcceptFull), vec![]);
        assert!(!machine.take_stat_events().contains(&StatEvent::Shown));
    }

    #[test]
    fn offer_replacement_multi_blocked_when_field_is_not_focused() {
        // Focus-race guard: a multi offer for a field other than the focused one
        // (or when nothing is focused) is dropped — no ghost tagged to a stale
        // field.
        let mut focused = focused_machine(); // field-a focused
        assert_eq!(
            focused.offer_replacement_multi(&field("other-field"), vec!["😄".into()], 5),
            vec![]
        );
        assert!(focused.showing.is_none());
        let mut unfocused = machine();
        assert_eq!(
            unfocused.offer_replacement_multi(&field("field-a"), vec!["😄".into()], 5),
            vec![]
        );
        assert!(unfocused.showing.is_none());
    }

    #[test]
    fn offer_replacement_multi_disarms_pending_model_request_so_it_cannot_supersede() {
        // Mirrors the single-candidate disarm test for the multi production path:
        // an edit arms the debounce, then a multi replacement offer preempts it.
        let mut machine = focused_machine();
        machine.on_event(text_changed("color", 5, 0));
        machine.offer_replacement_multi(&field("field-a"), vec!["😄".into(), "🙂".into()], 5);
        let _ = machine.take_stat_events();
        // The debounce tick must NOT fire a model request — the offer preempted it.
        let tick = machine.on_event(Event::Tick { now_ms: 10_000 });
        assert!(
            !tick
                .iter()
                .any(|c| matches!(c, Command::RequestCompletion { .. })),
            "model request armed despite a local replacement offer: {tick:?}"
        );
        // The replacement ghost is still the one showing (not superseded).
        assert_eq!(
            machine.preview_accept_insert(AcceptAction::Full),
            Some((field("field-a"), "😄".into(), 5))
        );
    }

    #[test]
    fn offer_replacement_multi_drops_a_prior_in_flight_completion_that_returns_after() {
        // The in-flight half of the disarm guarantee for the multi production
        // path: a model request already in-flight when the multi offer is made
        // must not match-and-supersede the replacement ghost when its completion
        // finally returns. `offer_replacement_multi` clears `requested`, so the
        // late completion fails the `matches_request` guard and is dropped.
        let mut machine = focused_machine();
        machine.on_event(text_changed("color", 5, 0));
        let issued = machine.on_event(Event::Tick { now_ms: 10_000 });
        let req = issued
            .iter()
            .find_map(|c| match c {
                Command::RequestCompletion {
                    generation,
                    snapshot,
                    ..
                } => Some((*generation, *snapshot)),
                _ => None,
            })
            .expect("a model request must have been issued");
        // The host detects an emoji/synonym on the same snapshot and offers a
        // multi replacement — this disarms the in-flight request.
        machine.offer_replacement_multi(&field("field-a"), vec!["😄".into(), "🙂".into()], 5);
        let _ = machine.take_stat_events();
        // The previously-issued completion now returns (same generation+snapshot).
        // It must be ignored — no ghost command at all.
        let late = machine.on_event(Event::CompletionReady {
            generation: req.0,
            field: field("field-a"),
            snapshot: req.1,
            text: "colorful".into(),
        });
        assert!(
            late.is_empty(),
            "a disarmed in-flight completion produced commands: {late:?}"
        );
        // The replacement ghost is untouched — still the one showing.
        assert_eq!(
            machine.preview_accept_insert(AcceptAction::Full),
            Some((field("field-a"), "😄".into(), 5))
        );
    }

    #[test]
    fn offer_replacement_supersedes_a_showing_completion() {
        let mut machine = showing_three_words(); // TextChanged focuses field-a
        let _ = machine.take_stat_events(); // drop the completion's Shown
        let events_before = machine.offer_replacement(&field("field-a"), "😄".into(), 5);
        assert!(!events_before.is_empty()); // it showed
        let stats = machine.take_stat_events();
        assert!(stats.contains(&StatEvent::Superseded));
        assert!(stats.contains(&StatEvent::Shown));
    }

    #[test]
    fn replacement_word_accept_is_atomic_even_for_multi_word_text() {
        // A multi-word synonym must not be split on Word-accept — that would drop
        // the deletion and leave the typed token. It commits whole, like Full.
        let mut machine = focused_machine();
        let f = field("field-a");
        machine.offer_replacement(&f, "big deal".into(), 6);
        assert_eq!(
            machine.on_event(Event::AcceptWord),
            vec![
                Command::Replace {
                    field: f,
                    text: "big deal".into(),
                    replace_left: 6,
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn replacement_text_is_not_trailing_spaced() {
        // The trailing-space-after-single-word policy must not append to a
        // replacement glyph (the replacement text is inserted exactly).
        let mut machine = SuggestionMachine::new(inline_caps(), 200, 4).with_trailing_space(true);
        machine.on_event(Event::Focus {
            field: field("field-a"),
            caps: inline_caps(),
        });
        let f = field("field-a");
        machine.offer_replacement(&f, "😄".into(), 5);
        assert_eq!(
            machine.on_event(Event::AcceptFull),
            vec![
                Command::Replace {
                    field: f,
                    text: "😄".into(),
                    replace_left: 5,
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn preview_reports_replace_left_for_a_replacement_offer() {
        // The host absorbs the echo via preview, so preview must carry the same
        // (text, replace_left) the accept will Replace with — atomic + unfinalized
        // for both Full and Word.
        let mut machine = focused_machine();
        machine.offer_replacement(&field("field-a"), "😄".into(), 5);
        assert_eq!(
            machine.preview_accept_insert(AcceptAction::Full),
            Some((field("field-a"), "😄".into(), 5))
        );
        assert_eq!(
            machine.preview_accept_insert(AcceptAction::Word),
            Some((field("field-a"), "😄".into(), 5))
        );
    }

    #[test]
    fn long_completion_is_capped_to_max_words_when_shown() {
        // The `max_words` ceiling (set via `new`) flows through `on_completion_ready`
        // into `cap_words`: a non-degenerate completion with MORE words than the cap
        // must be shown truncated to exactly the first `max_words` words — not
        // suppressed, not shown whole. Pins the end-to-end plumbing of the cap, which
        // the existing degenerate-repetition tests exercise only as a side effect.
        let mut machine = SuggestionMachine::new(inline_caps(), 200, 3);
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "alpha beta gamma delta epsilon".into(),
            }),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "alpha beta gamma".into(),
            }]
        );
    }

    #[test]
    fn plain_dismiss_does_not_suppress_the_field() {
        // Plain `Event::Dismiss` only hides the ghost (show-failed reconciliation);
        // unlike Esc (`DismissSuppress`) it must NOT set the suppression gate, so a
        // subsequent local replacement offer in the same field still shows. The
        // sibling `mutverify_dismiss_discard_does_not_suppress_field` pins the same
        // for the tray-Disable path; this pins it for plain Dismiss.
        let mut machine = focused_machine();
        machine.on_event(Event::Dismiss);
        assert_eq!(
            machine.offer_replacement(&field("field-a"), "\u{1F604}".into(), 5),
            vec![Command::ShowGhost {
                field: field("field-a"),
                snapshot: 1,
                text: "\u{1F604}".into(),
            }],
            "plain Dismiss must not suppress the field"
        );
    }

    #[test]
    fn model_completion_accept_still_emits_plain_insert_not_replace() {
        // Regression guard: ordinary completions (replace_left == 0) must never
        // emit Replace — only append-only Insert.
        let mut machine = showing_three_words();
        let out = machine.on_event(Event::AcceptFull);
        assert!(
            matches!(out.first(), Some(Command::Insert { .. })),
            "expected Insert, got {:?}",
            out.first()
        );
    }

    #[test]
    fn accept_with_nothing_showing_is_noop() {
        let mut machine = machine();

        assert_eq!(machine.on_event(Event::AcceptFull), vec![]);
    }

    #[test]
    fn accept_word_with_nothing_showing_is_noop() {
        let mut machine = machine();

        assert_eq!(machine.on_event(Event::AcceptWord), vec![]);
    }

    #[test]
    fn whitespace_only_completion_is_suppressed() {
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "   \n\t".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn completion_ready_without_request_is_noop() {
        let mut machine = machine();

        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "unrequested".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn completion_ready_multi_without_request_is_noop() {
        // The multi-candidate sibling of `completion_ready_without_request_is_noop`:
        // a CompletionReadyMulti that matches no outstanding request is dropped
        // (no request was ever armed) — no ShowGhost, nothing showing.
        let mut machine = machine();

        assert_eq!(
            machine.on_event(Event::CompletionReadyMulti {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                candidates: vec!["unrequested".into(), "also unrequested".into()],
            }),
            vec![]
        );
        assert!(machine.showing.is_none());
    }

    #[test]
    fn tick_without_pending_is_noop() {
        let mut machine = machine();

        assert_eq!(machine.on_event(Event::Tick { now_ms: 9999 }), vec![]);
    }

    #[test]
    fn dismiss_with_nothing_showing_is_noop() {
        let mut machine = machine();

        assert_eq!(machine.on_event(Event::Dismiss), vec![]);
    }

    #[test]
    fn caret_moved_with_nothing_showing_is_noop() {
        let mut machine = machine();

        assert_eq!(
            machine.on_event(Event::CaretMoved {
                field: field("field-a"),
                caret: 4,
            }),
            vec![]
        );
    }

    #[test]
    fn secure_state_change_with_nothing_showing_emits_no_hide() {
        let mut machine = machine();

        assert_eq!(
            machine.on_event(Event::SecureStateChanged {
                caps: secure_caps(),
            }),
            vec![]
        );
    }

    #[test]
    fn focus_with_nothing_showing_emits_no_hide() {
        let mut machine = machine();

        assert_eq!(
            machine.on_event(Event::Focus {
                field: field("field-a"),
                caps: inline_caps(),
            }),
            vec![]
        );
    }

    #[test]
    fn accept_word_advances_internal_caret_so_matching_caret_keeps_showing() {
        let mut machine = showing_three_words();
        // Suggestion shown at caret 1 ("x"); accepting "world " (6 chars)
        // advances the tracked caret to 7.
        machine.on_event(Event::AcceptWord);

        // A caret report at the advanced position must NOT hide the remainder.
        assert_eq!(
            machine.on_event(Event::CaretMoved {
                field: field("field-a"),
                caret: 7,
            }),
            vec![]
        );
    }

    #[test]
    fn accept_word_advances_caret_by_scalar_count_not_bytes() {
        // accept_word advances the tracked caret by the accepted word's SCALAR
        // count, matching the context crate's scalar-offset contract. With a
        // multibyte first word, a bytes-based regression (word.len()) would
        // desync the caret and spuriously hide the remainder on CJK completions.
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            // First word "日本 " = 3 scalars / 7 bytes (each CJK char is 3 bytes).
            text: "日本 語".into(),
        });
        machine.on_event(Event::AcceptWord);
        // Caret advanced 1 + 3 scalars = 4; a byte-based advance would be 1+7=8,
        // so a report at the SCALAR position keeps the remainder showing.
        assert_eq!(
            machine.on_event(Event::CaretMoved {
                field: field("field-a"),
                caret: 4,
            }),
            vec![]
        );
    }

    #[test]
    fn tick_after_request_fired_does_not_refire() {
        // One arming yields exactly one RequestCompletion: firing clears
        // pending_since, so a second Tick further past the threshold is a noop.
        let mut machine = machine();
        machine.on_event(text_changed("hello ", 6, 1000));

        // First Tick past debounce fires the request.
        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 1200 }),
            vec![Command::RequestCompletion {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                prompt: "hello".into(),
            }]
        );

        // Second Tick even further past the threshold must not re-fire.
        assert_eq!(machine.on_event(Event::Tick { now_ms: 5000 }), vec![]);
    }

    fn showing_three_distinct_words() -> SuggestionMachine {
        // "alpha beta gamma" shares no token with the recent left context
        // ("x"), so it survives the repetition gate, and three words fit under
        // the max-words cap of 4.
        let mut machine = machine();
        machine.on_event(text_changed("x", 1, 0));
        machine.on_event(Event::Tick { now_ms: 500 });
        machine.on_event(Event::CompletionReady {
            generation: 1,
            field: field("field-a"),
            snapshot: 1,
            text: "alpha beta gamma".into(),
        });
        machine
    }

    #[test]
    fn accept_word_to_exhaustion_inserts_each_word_then_hides() {
        let mut machine = showing_three_distinct_words();

        // First word: insert "alpha " and show the remaining two words.
        assert_eq!(
            machine.on_event(Event::AcceptWord),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "alpha ".into(),
                },
                Command::UpdateGhost {
                    field: field("field-a"),
                    snapshot: 1,
                    text: "beta gamma".into(),
                },
            ]
        );

        // Second word: insert "beta " and show the final word.
        assert_eq!(
            machine.on_event(Event::AcceptWord),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "beta ".into(),
                },
                Command::UpdateGhost {
                    field: field("field-a"),
                    snapshot: 1,
                    text: "gamma".into(),
                },
            ]
        );

        // The tracked caret has advanced across both accepted words
        // (1 + "alpha ".len() + "beta ".len() = 12), so a caret report at the
        // advanced position keeps the final word showing rather than hiding it.
        assert_eq!(
            machine.on_event(Event::CaretMoved {
                field: field("field-a"),
                caret: 12,
            }),
            vec![]
        );

        // Third (last) word: insert "gamma" with no trailing space and hide.
        assert_eq!(
            machine.on_event(Event::AcceptWord),
            vec![
                Command::Insert {
                    field: field("field-a"),
                    text: "gamma".into(),
                },
                Command::Hide,
            ]
        );

        // Nothing is showing anymore: a further accept is a noop.
        assert_eq!(machine.on_event(Event::AcceptWord), vec![]);
    }

    #[test]
    fn preview_accept_insert_with_nothing_showing_returns_none() {
        // A fresh machine has no completion showing, so neither preview
        // variant can report an insertion.
        let machine = machine();

        assert_eq!(machine.preview_accept_insert(AcceptAction::Full), None);
        assert_eq!(machine.preview_accept_insert(AcceptAction::Word), None);
    }

    #[test]
    fn completion_ready_discarded_after_focus_advances_boundary() {
        let mut machine = machine();
        // Arm a request: generation/snapshot are now 1.
        machine.on_event(text_changed("hello ", 6, 1000));
        assert_eq!(
            machine.on_event(Event::Tick { now_ms: 1200 }),
            vec![Command::RequestCompletion {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                prompt: "hello".into(),
            }]
        );

        // Focusing a new field advances the snapshot/generation boundary and
        // clears the in-flight request.
        machine.on_event(Event::Focus {
            field: field("field-b"),
            caps: inline_caps(),
        });

        // A completion tagged with the now-stale generation/snapshot must be
        // discarded — nothing is shown.
        assert_eq!(
            machine.on_event(Event::CompletionReady {
                generation: 1,
                field: field("field-a"),
                snapshot: 1,
                text: "world".into(),
            }),
            vec![]
        );
    }

    #[test]
    fn arm_manual_grammar_request_round_trips_through_correction_ready_and_accept() {
        // The manual grammar-fix path has its own arming entry point distinct from
        // the auto-completion debounce: the host calls `arm_manual_grammar_request`,
        // gets back the (generation, snapshot) to stamp its request with, and a
        // later `CorrectionReady` carrying that stamp shows a correction the user can
        // accept. This end-to-end test pins arm (returns the live gen/snap and
        // cancels any armed completion debounce), `on_correction_ready` (matches the
        // GrammarFix request and emits ShowCorrection), `preview_accept_correction`
        // (reports the pending replacement), and AcceptCorrection through the grammar
        // arming path.
        let mut machine = machine();
        // Typing arms a completion debounce; the manual grammar arm must cancel it.
        machine.on_event(text_changed("teh quik", 8, 1000));
        let f = field("field-a");
        let (gen, snap) = machine
            .arm_manual_grammar_request(&f)
            .expect("focused, enabled, unsuppressed field arms a grammar request");

        // Arming cleared the completion debounce: a Tick past the window fires
        // nothing (the pending model request was preempted by the grammar arm).
        assert_eq!(machine.on_event(Event::Tick { now_ms: 9999 }), vec![]);

        let range = CorrectionRange { start: 0, end: 8 };
        assert_eq!(
            machine.on_event(Event::CorrectionReady {
                generation: gen,
                field: f.clone(),
                snapshot: snap,
                original: "teh quick".into(),
                suggestion: "the quick".into(),
                correction_range: range,
            }),
            vec![Command::ShowCorrection {
                field: f.clone(),
                snapshot: snap,
                suggestion: "the quick".into(),
                correction_range: range,
            }]
        );
        // The host previews the pending correction (echo absorption) before accept.
        assert_eq!(
            machine.preview_accept_correction(),
            Some((f.clone(), "the quick".into(), range))
        );
        assert_eq!(
            machine.on_event(Event::AcceptCorrection),
            vec![
                Command::ReplaceRange {
                    field: f,
                    expected_text: "teh quick".into(),
                    text: "the quick".into(),
                    correction_range: range,
                },
                Command::Hide,
            ]
        );
    }

    #[test]
    fn arm_manual_grammar_request_is_gated_by_focus_suppression_and_secure() {
        let f = field("field-a");
        // Nothing focused → no field identity to arm against.
        assert_eq!(machine().arm_manual_grammar_request(&f), None);
        // Focused elsewhere: an arm for a non-focused field is rejected.
        assert_eq!(
            focused_machine().arm_manual_grammar_request(&field("field-b")),
            None
        );
        // Esc-suppressed field: no manual grammar request while suppressed.
        let mut suppressed = focused_machine();
        suppressed.on_event(Event::DismissSuppress);
        assert_eq!(suppressed.arm_manual_grammar_request(&f), None);
        // Secure field (UxMode::Blocked → not enabled): never arm — a grammar fix
        // must never inspect a password field.
        let mut secure = machine();
        secure.on_event(Event::Focus {
            field: f.clone(),
            caps: secure_caps(),
        });
        assert_eq!(secure.arm_manual_grammar_request(&f), None);
    }

    #[test]
    fn correction_ready_dropped_when_stale_empty_or_kind_mismatched() {
        let f = field("field-a");
        let range = CorrectionRange { start: 0, end: 1 };

        // Stale snapshot: a grammar request is armed, but the correction carries a
        // snapshot that no longer matches the boundary → dropped, nothing shows.
        let mut grammar = focused_machine();
        let (gen, snap) = grammar.arm_manual_grammar_request(&f).expect("armed");
        assert_eq!(
            grammar.on_event(Event::CorrectionReady {
                generation: gen,
                field: f.clone(),
                snapshot: snap + 1,
                original: "t".into(),
                suggestion: "x".into(),
                correction_range: range,
            }),
            vec![]
        );
        assert!(grammar.preview_accept_correction().is_none());
        // Empty suggestion on the matching stamp is still dropped (no blank ghost).
        assert_eq!(
            grammar.on_event(Event::CorrectionReady {
                generation: gen,
                field: f.clone(),
                snapshot: snap,
                original: "t".into(),
                suggestion: String::new(),
                correction_range: range,
            }),
            vec![]
        );
        assert!(grammar.preview_accept_correction().is_none());

        // Kind discriminator: a CorrectionReady arriving against a COMPLETION
        // request (armed by the debounce, not by arm_manual_grammar_request) must
        // not match — the grammar-fix result cannot render over a completion slot.
        let mut completion = machine();
        completion.on_event(text_changed("hello ", 6, 1000));
        assert!(completion
            .on_event(Event::Tick { now_ms: 1200 })
            .iter()
            .any(|c| matches!(c, Command::RequestCompletion { .. })));
        assert_eq!(
            completion.on_event(Event::CorrectionReady {
                generation: 1,
                field: f,
                snapshot: 1,
                original: "wrold".into(),
                suggestion: "world".into(),
                correction_range: range,
            }),
            vec![]
        );
        assert!(completion.preview_accept_correction().is_none());
    }

    #[test]
    fn preview_accept_correction_is_none_unless_a_correction_is_showing() {
        // Nothing showing → nothing to preview.
        assert_eq!(machine().preview_accept_correction(), None);
        // A ghost (ordinary completion) is not a correction: the correction preview
        // must return None so the host never applies a ReplaceRange for a plain
        // completion. Pins the presentation discriminator.
        assert_eq!(showing_three_words().preview_accept_correction(), None);
    }
}
