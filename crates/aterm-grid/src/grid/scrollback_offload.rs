// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Off-thread width-change scrollback reflow.
//!
//! [`Grid::resize`]'s width branch rewraps the entire off-screen scrollback
//! synchronously (`scrollback_reflow`), costing O(session history) on the GUI
//! main thread under the per-session `term` mutex. This module moves that
//! work off the lock.
//!
//! A real session keeps its unbounded, disk-tiered history in the tiered
//! [`ScrollbackStorage`] (`storage.scrollback`), separate from the fixed
//! in-memory ring. [`Grid::resize_offloading_scrollback`] detaches that store
//! in O(1) before resizing and lifts the ring's scrollback rows into the job
//! as materialized [`Line`]s, so the synchronous reflow touches only the
//! visible viewport; the detached history is returned as a `Send`
//! [`PendingScrollbackReflow`]. The decompress + rewrap
//! ([`PendingScrollbackReflow::reflow`], or budgeted
//! [`PendingScrollbackReflow::reflow_step`] increments for a caller with no
//! worker thread, like the wasm cooperative offload) runs off the lock, and
//! the result is re-attached under a brief lock via
//! [`Grid::reattach_reflowed_scrollback`]. Main-thread cost is O(viewport)
//! plus one O(ring) row-to-[`Line`] materialization.
//!
//! Memory: the rewrap streams. Input is read from the front of the detached
//! store and truncated out as consumed; completed logical runs are rewrapped
//! and re-compressed onto the same store's back. Peak transient usage is
//! O(step budget + one logical run) uncompressed, never the whole history.
//!
//! Ordering: while a reflow is in flight the grid has no tiered store and no
//! ring scrollback (both ride the job), so new scroll-off accumulates in the
//! empty ring. Re-attaching the store and returning the ring history (to the
//! ring, or to the store where flight content took its seat; see
//! [`Grid::reattach_ring_history`]) restores [old history | old ring history |
//! window output | live ring]. Only one detach can be in flight: a resize
//! racing it detaches nothing and only rewraps the ring.
//!
//! Detach-window integrity: output scrolling off during the window is staged
//! to the lazy buffer (`scrollback_detached_for_reflow`) and flushed on
//! re-attach; a scrollback erase during the window is honoured via
//! `scrollback_clear_gen` (the stale store is dropped); and the reader's
//! scroll position rides `prev_offset`, restored unless
//! `detach_live_bottom_gen` shows the reader themselves went to the live
//! bottom meanwhile. One accepted transient: a concurrent control-socket
//! reader (`search`/`text`/`lines`) during the window sees only the ring, so
//! it can briefly miss text in off-screen history. It self-heals on
//! re-attach (the `content_gen` bump invalidates the read cache); a second
//! live snapshot is not worth the memory.

use aterm_scrollback::{Line, ScrollbackStorage};

use super::Grid;
use super::reflow::ResizePolicy;
use super::state::{DetachedReaderAim, PendingScrollbackSettings};
use crate::Damage;

/// One reader-facing viewport gesture, as the detach window records it
/// (see [`Grid::note_detached_reader_motion`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReaderMotion {
    /// `scroll_display(delta)`: up positive, toward the live bottom negative.
    By(i32),
    /// `scroll_to_top`.
    Top,
    /// `scroll_to_bottom`.
    Live,
}

/// A live-relative aim of `rows`, taken at `at_counter`, grown by the lines
/// that scrolled into history since (`counter`): a reader off the live bottom
/// rides up with the content they were reading; a reader at the live bottom
/// stays there.
fn grown_from_live(rows: usize, at_counter: u64, counter: u64) -> usize {
    if rows == 0 {
        return 0;
    }
    rows.saturating_add(usize::try_from(counter.saturating_sub(at_counter)).unwrap_or(usize::MAX))
}

impl DetachedReaderAim {
    /// The `display_offset` this aim names once the whole history (`sb` lines)
    /// is back with the row counter at `counter`, given `restored` (where the
    /// re-attach's own rule would put the reader). An aim above the attached
    /// rows lands at least as far up as asked and never below `restored`.
    fn resolve(self, counter: u64, sb: usize, restored: usize) -> usize {
        match self {
            DetachedReaderAim::Above { rows, at_counter } => {
                grown_from_live(rows, at_counter, counter)
                    .max(restored)
                    .min(sb)
            }
            DetachedReaderAim::FromTop { rows } => sb.saturating_sub(rows),
            DetachedReaderAim::At { rows, at_counter } => {
                grown_from_live(rows, at_counter, counter).min(sb)
            }
        }
    }
}

/// The off-screen scrollback of a grid, detached and awaiting off-thread rewrap
/// to `new_cols`. `Send`, so the decompress + rewrap runs on a worker off the
/// caller's lock. Produced by [`Grid::resize_offloading_scrollback`].
///
/// Off-screen history lives in three places, all extracted without
/// (de)compressing on the caller's thread, in age order: the tiered `store`
/// (compressed, oldest), `lazy_lines` (uncompressed staging, which a plain
/// resize would discard once the store is detached; see
/// `drain_lazy_buffer`), and `ring_lines` (the ring's scrollback rows as
/// [`Line`]s, newest).
///
/// `[store | lazy_lines]` rewrap as one logical sequence back into the store;
/// `ring_lines` rewrap as a separate sequence (the ring phase) returned to the
/// ring at re-attach. A soft-wrapped run straddling the lazy/ring boundary is
/// handed to the ring phase whole, so it rewraps as the synchronous resize
/// rewraps it.
///
/// Two ways to run the rewrap, identical results:
/// * [`reflow`](Self::reflow): one shot, for a worker thread.
/// * [`reflow_step`](Self::reflow_step): budgeted increments, for a caller
///   that must keep every task short (the single-threaded wasm event loop).
///
/// Either way the job owns its data: the grid never sees partial progress, and
/// re-attach takes only a completed [`ReflowedScrollback`].
#[must_use = "a detached scrollback store must be reflowed and re-attached, or history is lost"]
pub struct PendingScrollbackReflow {
    store: ScrollbackStorage,
    lazy_lines: Vec<Line>,
    /// The ring's scrollback rows, materialized at detach. Rewrapped in
    /// [`ReflowPhase::RingRewrap`]; the result rides
    /// [`ReflowedScrollback::ring_out`] back to the ring. Lost with a dropped
    /// job, like the store.
    ring_lines: Vec<Line>,
    new_cols: u16,
    /// The reader's scroll position at detach, restored on re-attach.
    prev_offset: usize,
    /// `GridStorage::reader_live_bottom_gen` sampled at detach, so re-attach can
    /// tell whether the reader brought the viewport to the live bottom while
    /// this job was out. `display_offset` cannot: the detach's own clamp, a
    /// retention clamp, a rows-only re-anchor, an output batch's repin and the
    /// reader's End all write the same 0. Only reader-facing primitives advance
    /// the generation, and only on a real descent.
    detach_live_bottom_gen: u64,
    /// The scrollback-erase generation at detach; if it advanced by re-attach an
    /// erase happened during the window and the reflowed store is dropped.
    /// Captured once per job, never re-sampled per step.
    clear_gen: u64,
    /// Incremental-rewrap progress (`reflow_step`). Starts at the beginning of
    /// the input; the one-shot [`reflow`](Self::reflow) is just "step until
    /// done" over this same state.
    phase: ReflowPhase,
    /// `Some` from the first step until completion: the store's own limits,
    /// out of the way of the streaming reads (see [`LiftedStoreLimits`]).
    lifted_limits: Option<LiftedStoreLimits>,
    /// The history lines this job detached (tiered + lazy + ring), fixed at
    /// detach: the denominator of its progress ([`Self::lines_done`]).
    lines_total: usize,
    /// Input lines the steps have read so far (every read line is rewrapped
    /// by the step that completes its logical run).
    lines_read: usize,
}

/// Where an incrementally-stepped rewrap currently is. Private: callers only
/// see [`ReflowStep`].
enum ReflowPhase {
    /// The streaming rewrap: read input from the front of the store
    /// (truncating it as consumed), then from the newer `lazy_lines`; rewrap
    /// every completed logical line and push it onto the back of the same
    /// store. The tier pipeline is a FIFO, so the store holds [unread input |
    /// rewrapped output] and `0..store_input_left` addresses exactly the input.
    Stream {
        /// Input lines still unread at the store's front.
        store_input_left: usize,
        /// Next `lazy_lines` index to read once the store input is consumed.
        next_lazy: usize,
        /// The trailing logical line (a non-wrapped head plus its soft-wrapped
        /// continuations) that may continue into unread input, held back so a
        /// step boundary never splits a logical line. Invariant: only
        /// `carry[0]` can be non-wrapped. Bounded by one logical run
        /// (`streaming_step_carry_stays_bounded`).
        carry: Vec<Line>,
    },
    /// Store streaming complete: rewrapping the job-carried ring history
    /// (`ring_lines`) as its own logical sequence, with the same budgeted carve
    /// and carry, into `out`, which becomes [`ReflowedScrollback::ring_out`].
    /// Runs last because the ring history is newer than the store and never
    /// enters the store's line-limit accounting.
    RingRewrap {
        /// Next `ring_lines` index to read.
        next_input: usize,
        /// Trailing, possibly-still-continuing logical run (never split).
        carry: Vec<Line>,
        /// Rewrapped ring history accumulated so far (completed runs only).
        out: Vec<Line>,
    },
}

impl ReflowPhase {
    /// The state every job starts in: the whole store front plus the lazy
    /// tail unread, nothing carried.
    fn start(store: &ScrollbackStorage) -> Self {
        ReflowPhase::Stream {
            store_input_left: store.line_count(),
            next_lazy: 0,
            carry: Vec::new(),
        }
    }
}

/// The store's own eviction limits (total-line cap and byte budget), lifted at
/// the first [`PendingScrollbackReflow::reflow_step`] and restored, with one
/// enforcement pass, by the step that finishes the store's output.
///
/// Both limits evict the store's oldest lines, which mid-stream are the unread
/// input at its front. Enforcing once at the end is safe. For the line limit
/// it gives the identical final state (the newest `limit` lines;
/// `streaming_matches_materialize_all_reference`).
///
/// For the byte budget, end-of-flight enforcement (after re-tiering) and
/// push-time enforcement (with the newest output still uncompressed) stop at
/// different eviction depths: surviving lines are identical and in order,
/// and end-of-flight never keeps fewer
/// (`streaming_under_byte_budget_pressure_keeps_a_longer_suffix`).
///
/// While lifted, a store already at its byte budget can end the flight over it
/// until the restoring step trims it. The per-tier `hot_limit` / `warm_limit`
/// promotions are not lifted, so hot+warm RAM stays bounded and the excess
/// sits in the compressed cold tier.
struct LiftedStoreLimits {
    line_limit: Option<usize>,
    memory_budget: usize,
}

/// The outcome of one [`PendingScrollbackReflow::reflow_step`] call. An
/// exhausted job cannot be stepped again and a half-stepped job cannot be
/// re-attached: the only way to a [`ReflowedScrollback`] is through `Done`.
#[must_use = "keep stepping InProgress (or re-attach Done), or history is lost"]
pub enum ReflowStep {
    /// More work remains; call [`PendingScrollbackReflow::reflow_step`] again.
    /// Dropping this loses the detached history, like dropping a never-started
    /// job; recover the grid via [`Grid::abort_reflow_offload`] (or supersede
    /// it, as the wasm modules' newest-wins overwrite does).
    InProgress(PendingScrollbackReflow),
    /// The rewrap is complete and re-filled into the store, ready for
    /// [`Grid::reattach_reflowed_scrollback`]. Identical to what the one-shot
    /// [`PendingScrollbackReflow::reflow`] produces, for ANY step schedule.
    Done(ReflowedScrollback),
}

/// A scrollback store already rewrapped to `new_cols` (via
/// [`PendingScrollbackReflow::reflow`]), ready to re-attach under a brief lock
/// with [`Grid::reattach_reflowed_scrollback`].
#[must_use = "a reflowed scrollback store must be re-attached, or history is lost"]
pub struct ReflowedScrollback {
    store: ScrollbackStorage,
    /// The ring history rewrapped to `new_cols`, returned to the ring (or the
    /// store, where flight content took its seat) by
    /// [`Grid::reattach_ring_history`].
    ring_out: Vec<Line>,
    new_cols: u16,
    prev_offset: usize,
    /// See [`PendingScrollbackReflow::detach_live_bottom_gen`]; carried through
    /// unchanged, so every chunked schedule re-attaches on the same baseline.
    detach_live_bottom_gen: u64,
    clear_gen: u64,
}

// The whole point is to move this across a thread boundary; guarantee it.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<PendingScrollbackReflow>();
    assert_send::<ReflowedScrollback>();
    assert_send::<ReflowStep>();
};

impl PendingScrollbackReflow {
    /// The width this history will be rewrapped to.
    #[must_use]
    #[cfg(test)]
    pub fn new_cols(&self) -> u16 {
        self.new_cols
    }

    /// How far the rewrap is: input lines read so far, of
    /// [`Self::lines_total`]. Monotone, and equal to the total on the step that
    /// returns `Done`. O(1), no store access.
    #[must_use]
    pub fn lines_done(&self) -> usize {
        self.lines_read.min(self.lines_total)
    }

    /// The history lines this job detached, fixed at detach (unlike
    /// [`Self::line_count`], which moves as the stream consumes input and
    /// re-fills the store with output).
    #[must_use]
    pub fn lines_total(&self) -> usize {
        self.lines_total
    }

    /// The number of history lines to rewrap (for logging / progress) —
    /// tiered + lazy-staged + ring.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.store.line_count() + self.lazy_lines.len() + self.ring_lines.len()
    }

    /// The expensive step; run it off the caller's thread and lock. Decompresses
    /// every stored line, rewraps the whole history to `new_cols`
    /// (`reflow_scrollback_lines`, O(total cells)), and streams the result back
    /// into the same store (input truncated as read, completed runs
    /// re-compressed onto the back), so only O(one logical run) is held
    /// uncompressed. The store keeps its identity and tier configuration; its
    /// `line_limit` and memory budget are lifted for the flight and re-enforced
    /// when the store's output completes ([`LiftedStoreLimits`]).
    ///
    /// Exactly [`reflow_step`](Self::reflow_step) with an unlimited budget,
    /// driven until `Done`; every chunked schedule produces the identical
    /// [`ReflowedScrollback`].
    // Cost: O(session history) in ONE call — the whole job. Callers
    // that must bound per-call latency use `reflow_step` instead.
    pub fn reflow(self) -> ReflowedScrollback {
        let mut job = self;
        loop {
            match job.reflow_step(usize::MAX) {
                ReflowStep::InProgress(next) => job = next,
                ReflowStep::Done(reflowed) => return reflowed,
            }
        }
    }

    /// Advance the rewrap by a bounded increment: read and rewrap up to
    /// `max_lines` input history lines (0 is treated as 1), then yield. Call
    /// again on [`ReflowStep::InProgress`]; [`ReflowStep::Done`] carries the
    /// completed [`ReflowedScrollback`], byte-identical to the one-shot
    /// [`reflow`](Self::reflow) for any schedule
    /// (`reflow_step_any_schedule_matches_one_shot`).
    ///
    /// Chunks are carved only at unwrapped-logical-line boundaries: a logical
    /// line (a row plus its soft-wrapped continuations) is never split across
    /// steps; its trailing run is carried to the next step.
    ///
    /// The job owns its data, so stepping runs outside the term lock and the
    /// grid never observes partial progress; re-attach takes only a completed
    /// result; `clear_gen` is captured per job at detach; and dropping a
    /// half-stepped job loses the detached history (recover via
    /// [`Grid::abort_reflow_offload`]).
    // COST: O(max_lines × cols) per step: ≤ max_lines input-line reads (tiered
    // decompress, lazy take, or ring-line move) plus rewrap and re-compress of
    // the completed runs among them. Memory: O(max_lines + one logical run)
    // uncompressed. Bounded by the caller's budget, with two caveats: a
    // logical line is never split, so the step completing a run longer than
    // max_lines rewraps it whole (runs are capped at MAX_LOGICAL_WIDTH
    // cells); and a front truncation landing mid-block makes the next step
    // re-decode at most one warm/cold block, amortized by the block caches.
    pub fn reflow_step(mut self, max_lines: usize) -> ReflowStep {
        let budget = max_lines.max(1);
        // First step of the stream phase: lift the store's eviction limits for
        // the flight (see `LiftedStoreLimits`); the step finishing the store's
        // output restores them.
        //
        // Gated on the phase, not just `is_none()`: the store phase clears
        // `lifted_limits` when restoring, and the ring phase runs afterwards,
        // so an `is_none()`-only guard would re-lift on the first ring step and
        // never restore. `streaming_matches_materialize_all_reference`'s limit
        // round-trip assertions guard this.
        if self.lifted_limits.is_none() && matches!(self.phase, ReflowPhase::Stream { .. }) {
            let lifted = LiftedStoreLimits {
                line_limit: self.store.line_limit(),
                memory_budget: self.store.memory_budget(),
            };
            self.store.set_line_limit(None);
            // Raising a budget evicts nothing, and the watermark threshold
            // math is u128-saturating (`threshold_bytes`), so MAX is safe.
            if let Err(error) = self.store.set_memory_budget(usize::MAX) {
                aterm_log::warn!("offload reflow: lifting the memory budget failed: {error}");
            }
            self.lifted_limits = Some(lifted);
        }
        match &mut self.phase {
            ReflowPhase::Stream {
                store_input_left,
                next_lazy,
                carry,
            } => {
                let total_left = *store_input_left + (self.lazy_lines.len() - *next_lazy);
                let read = budget.min(total_left);
                self.lines_read = self.lines_read.saturating_add(read);
                let appended_from = carry.len();
                carry.reserve(read);
                let from_store = read.min(*store_input_left);
                for i in 0..from_store {
                    // Tiered store lines first (older), decompressed off the
                    // lock. Output from earlier steps sits behind the input
                    // region (FIFO), so `0..store_input_left` is exactly the
                    // input.
                    let line = match self.store.get_line(i) {
                        Ok(Some(line)) => line.into_owned(),
                        // A decode failure must not silently truncate older
                        // history: keep a blank placeholder so ordering stays
                        // sane (mirrors `take_scrollback_lines`).
                        Ok(None) | Err(_) => Line::new(),
                    };
                    carry.push(line);
                }
                if from_store > 0 {
                    // Release the consumed input now: whole warm blocks / cold
                    // pages free as the front offset crosses them, so the store
                    // never holds full input plus full output.
                    if let Err(error) = self.store.truncate_oldest(from_store) {
                        aterm_log::warn!("offload reflow: input front-truncation failed: {error}");
                    }
                    *store_input_left -= from_store;
                }
                for _ in from_store..read {
                    // Then the newer lazy-staged lines (blank left behind,
                    // freed when the input exhausts).
                    carry.push(std::mem::replace(
                        &mut self.lazy_lines[*next_lazy],
                        Line::new(),
                    ));
                    *next_lazy += 1;
                }

                if read == total_left {
                    // Input exhausted: every carried run is complete, unless the
                    // newest continues into the job-carried ring history (its
                    // head left the ring for the store, its tail is still in the
                    // ring). That run goes to the ring phase whole: rewrapped
                    // here it would come back as a head plus an orphan tail
                    // line, where the synchronous resize rewraps it as one.
                    // Ruling 2026-10-11: under every policy, not only ConPTY
                    // (where conhost's repaint first showed the orphan).
                    let straddling = if self.ring_lines.first().is_some_and(Line::is_wrapped) {
                        let head = carry.iter().rposition(|line| !line.is_wrapped());
                        carry.split_off(head.unwrap_or(0))
                    } else {
                        Vec::new()
                    };
                    // Rewrap the rest and push it straight onto the store.
                    let out =
                        super::scrollback_reflow::reflow_scrollback_lines(carry, self.new_cols);
                    carry.clear();
                    carry.shrink_to_fit();
                    self.lazy_lines = Vec::new(); // free the drained placeholders
                    for line in out {
                        if let Err(error) = self.store.push_line(line) {
                            aterm_log::warn!(
                                "offload reflow: scrollback push_line failed: {error}"
                            );
                        }
                    }
                    // Restore the store's limits, enforcing: the line limit
                    // first (keeping the newest `limit` lines, as push-time
                    // eviction would), then the byte budget (re-tier, then FIFO
                    // evict from the front of the finished output). Mid-flight
                    // this would have evicted unread input. The ring phase never
                    // touches the store, so restoring here keeps eviction order
                    // exact.
                    if let Some(limits) = self.lifted_limits.take() {
                        self.store.set_line_limit(limits.line_limit);
                        if let Err(error) = self.store.set_memory_budget(limits.memory_budget) {
                            aterm_log::warn!(
                                "offload reflow: restoring the memory budget failed: {error}"
                            );
                        }
                    }
                    // The ring history rewraps next as its own budgeted phase,
                    // carrying the straddling run as the head of its first
                    // logical line (only `carry[0]` is not a continuation).
                    self.phase = ReflowPhase::RingRewrap {
                        next_input: 0,
                        carry: straddling,
                        out: Vec::new(),
                    };
                } else {
                    // Carve at the last unwrapped-logical-line boundary among the
                    // newly read lines (older carry is all continuations of its
                    // head): completed runs are rewrapped and re-compressed onto
                    // the store now; the trailing run stays carried. With no
                    // boundary, the whole chunk continues the carried run.
                    if let Some(rel) = carry[appended_from..]
                        .iter()
                        .rposition(|line| !line.is_wrapped())
                    {
                        let boundary = appended_from + rel;
                        if boundary > 0 {
                            let tail = carry.split_off(boundary);
                            let head = std::mem::replace(carry, tail);
                            for line in super::scrollback_reflow::reflow_scrollback_lines(
                                &head,
                                self.new_cols,
                            ) {
                                if let Err(error) = self.store.push_line(line) {
                                    aterm_log::warn!(
                                        "offload reflow: scrollback push_line failed: {error}"
                                    );
                                }
                            }
                        }
                    }
                }
                ReflowStep::InProgress(self)
            }
            ReflowPhase::RingRewrap {
                next_input,
                carry,
                out,
            } => {
                let total = self.ring_lines.len();
                let end = next_input.saturating_add(budget).min(total);
                let appended_from = carry.len();
                carry.reserve(end - *next_input);
                self.lines_read = self.lines_read.saturating_add(end - *next_input);
                for line in &mut self.ring_lines[*next_input..end] {
                    // Take by replace: ring lines are already materialized, so
                    // this is a move, not a decompress.
                    carry.push(std::mem::replace(line, Line::new()));
                }
                *next_input = end;

                if end == total {
                    // Input exhausted: every carried run is complete by
                    // definition — rewrap the remainder in one call.
                    out.extend(super::scrollback_reflow::reflow_scrollback_lines(
                        carry,
                        self.new_cols,
                    ));
                    let ring_out = std::mem::take(out);
                    ReflowStep::Done(ReflowedScrollback {
                        store: self.store,
                        ring_out,
                        new_cols: self.new_cols,
                        prev_offset: self.prev_offset,
                        detach_live_bottom_gen: self.detach_live_bottom_gen,
                        clear_gen: self.clear_gen,
                    })
                } else {
                    carve_completed_runs(carry, appended_from, self.new_cols, out);
                    ReflowStep::InProgress(self)
                }
            }
        }
    }
}

/// Carve `carry` at the last unwrapped-logical-line boundary among the lines
/// appended this step (older carry is all continuations of its head, so
/// scanning the appended region suffices): completed runs before it are
/// rewrapped into `out`; the trailing run stays carried. With no boundary the
/// whole chunk keeps carrying. Used by the ring phase; the store phase inlines
/// its carve and streams completed runs straight into the store.
fn carve_completed_runs(
    carry: &mut Vec<Line>,
    appended_from: usize,
    new_cols: u16,
    out: &mut Vec<Line>,
) {
    if let Some(rel) = carry[appended_from..]
        .iter()
        .rposition(|line| !line.is_wrapped())
    {
        let boundary = appended_from + rel;
        if boundary > 0 {
            let tail = carry.split_off(boundary);
            let head = std::mem::replace(carry, tail);
            out.extend(super::scrollback_reflow::reflow_scrollback_lines(
                &head, new_cols,
            ));
        }
    }
}

impl ReflowedScrollback {
    /// The width this history was rewrapped to.
    #[must_use]
    #[cfg(test)]
    pub fn new_cols(&self) -> u16 {
        self.new_cols
    }

    /// The number of rewrapped history lines ready to re-attach (store +
    /// ring history).
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.store.line_count() + self.ring_out.len()
    }
}

impl Grid {
    /// Resize like [`Grid::resize`], but move the unbounded tiered off-screen
    /// scrollback off the synchronous path.
    ///
    /// On a width change with reflow enabled and a tiered store attached:
    /// detach the store (O(1)) and lift the ring's scrollback rows into the job
    /// (O(ring) materialization) before resizing, so the synchronous reflow
    /// inside [`Grid::resize`] costs O(viewport), then return the detached
    /// history as a `Send` [`PendingScrollbackReflow`]. The caller must
    /// [`reflow`](PendingScrollbackReflow::reflow) it off-thread and
    /// [`reattach`](Grid::reattach_reflowed_scrollback) the result, or history is
    /// lost.
    ///
    /// Returns `None` (and is exactly [`Grid::resize`]) when the width is
    /// unchanged, reflow is disabled, or no tiered store is attached; those
    /// resizes are already bounded by the viewport (for the width-unchanged
    /// case because `adjust_row_count_rows_only` reclassifies in place, gated
    /// by `tests/reflow/rows_only_cost_bound.rs`).
    pub fn resize_offloading_scrollback(
        &mut self,
        new_rows: u16,
        new_cols: u16,
    ) -> Option<PendingScrollbackReflow> {
        self.resize_offloading_scrollback_with_policy(new_rows, new_cols, ResizePolicy::Native)
    }

    /// [`Self::resize_offloading_scrollback`] under an explicit
    /// [`ResizePolicy`]. `Native` is exactly that method; `ConPty` skips the
    /// two bottom-anchoring moves conhost's post-resize repaint would paint
    /// over: the boundary-continuation lift into the job and the deficit fill
    /// at re-attach (see `ResizePolicy`).
    pub fn resize_offloading_scrollback_with_policy(
        &mut self,
        new_rows: u16,
        new_cols: u16,
        policy: ResizePolicy,
    ) -> Option<PendingScrollbackReflow> {
        let old_cols = self.storage.cols;
        // Only a width change rewraps scrollback, and only a tiered grid has the
        // unbounded off-screen history worth offloading. Otherwise the plain
        // resize is already O(viewport).
        if new_cols == old_cols || self.storage.scrollback.is_none() {
            self.resize_with_policy(new_rows, new_cols, policy);
            return None;
        }

        // Extract the off-screen history without (de)compressing on this
        // thread, all three layers in age order:
        //  1. lazy_lines: uncompressed staged lines (mid-age), taken before
        //     detaching the store, else the resize's `drain_lazy_buffer` drops
        //     them.
        //  2. store: the compressed tiered/disk history (oldest), detached O(1).
        //  3. ring_lines: the ring's scrollback rows (newest), materialized
        //     below; the only ring-sized synchronous cost.
        let prev_offset = self.storage.display_offset; // bug D: reader's scroll pos pre-resize
        let clear_gen = self.storage.scrollback_clear_gen; // bug C: erase generation at detach
        let lazy_lines: Vec<Line> = self.storage.lazy_buffer.drain_all().collect();
        // Keep the store's effective settings observable while the worker owns
        // it. Mutators update this snapshot and mark the corresponding dirty
        // bit; re-attach replays only values that actually changed.
        let pending_settings = PendingScrollbackSettings {
            line_limit: self.storage.scrollback_line_limit(),
            memory_budget: self
                .storage
                .scrollback
                .as_ref()
                .expect("scrollback.is_none() early-returned above")
                .memory_budget(),
            line_limit_changed: false,
            memory_budget_changed: false,
        };
        let store = self
            .storage
            .scrollback
            .take()
            .expect("scrollback.is_none() early-returned above");
        // The window opens with the store out, before the resize below
        // (ruling 2026-10-11). That resize pushes the screen's top rows into
        // history, and with the history in the job they land in an empty ring:
        // inside a window the ones past the ring cap are staged for re-attach
        // (`prepend_ring_scrollback_lines`), outside one they were dropped as
        // a ring-only grid drops them, so a narrowing that pushed more rows
        // than the ring holds lost its oldest ones. Until re-attach, scroll-off
        // is staged to the lazy buffer too.
        self.close_reflow_window_cap_run();
        self.storage.scrollback_detached_for_reflow = true;
        self.storage.pending_scrollback_settings = Some(pending_settings);
        // Ruling 238: the window opens with no reader aim.
        self.storage.detached_reader_aim = None;
        // Lift the ring history into the job too; materializing rows → Lines is
        // the only remaining synchronous ring cost. The ring is left empty, so
        // the resize below rewraps nothing off-screen
        // (`offloaded_resize_synchronous_rewrap_is_zero`).
        //
        // Capture the pre-resize trailing-blank count first: the deficit fill
        // cannot run inside the synchronous resize (the history is with the
        // worker), so it runs at re-attach against this target.
        let pending_fill_target = self.trailing_blank_rows_below_cursor();
        let mut ring_lines = self.take_ring_scrollback_lines();
        // Lift the viewport's leading soft-wrap continuation rows into the
        // job's ring sequence: their head is the newest ring history line, so
        // the straddling line rewraps as one unit, and the re-attach deficit
        // fill pulls the tail back into view.
        //
        // Not under ConPTY: the fill would run after conhost has repainted the
        // viewport row-0-anchored, re-seating a line on top of a complete frame
        // and duplicating it; and without the fill the lift would strand the
        // tail in history while conhost keeps painting it at row 0. The
        // synchronous resize below severs that fragment from its head
        // (`sever_top_row_continuation`) after the take above read the head at
        // its full width.
        if policy == ResizePolicy::Native {
            ring_lines.extend(self.take_boundary_continuation_lines());
            self.storage.pending_fill_target = Some(pending_fill_target);
        }

        // With the store detached, the lazy buffer emptied and the ring history
        // in the job, the resize's `take_scrollback_lines` rewraps nothing: the
        // synchronous cost is the visible-grid reflow, O(viewport).
        self.resize_with_policy(new_rows, new_cols, policy);
        // This window's reader-descent baseline. The resize just clamped
        // `display_offset` against the history the job left (none but the
        // rows the resize pushed), so its value says nothing about the reader;
        // the generation does, because machine motion (that clamp included)
        // never advances it. Sampled after the resize so the
        // resize's own clamp is inside the baseline. See the guard in
        // `reattach_reflowed_scrollback`.
        let detach_live_bottom_gen = self.storage.reader_live_bottom_gen;

        let phase = ReflowPhase::start(&store);
        let lines_total = store.line_count() + lazy_lines.len() + ring_lines.len();
        Some(PendingScrollbackReflow {
            store,
            lazy_lines,
            ring_lines,
            lines_total,
            lines_read: 0,
            new_cols,
            prev_offset,
            detach_live_bottom_gen,
            clear_gen,
            phase,
            lifted_limits: None,
        })
    }

    /// Re-attach an off-thread-rewrapped scrollback store as the (oldest) tiered
    /// history, after [`resize_offloading_scrollback`](Self::resize_offloading_scrollback)
    /// detached it and a worker [`reflow`](PendingScrollbackReflow::reflow)ed it.
    ///
    /// If a tiered store already exists (e.g. a terminal reset raced the
    /// reflow), the rewrapped history is dropped rather than clobber current
    /// state. If the width changed again meanwhile, the store is still attached
    /// with valid content; the stale wrapping heals on the next width change.
    ///
    /// Shipping drivers use
    /// [`reattach_reflowed_scrollback_or_redetach`](Self::reattach_reflowed_scrollback_or_redetach),
    /// which converges immediately instead; this entry point keeps one-window
    /// semantics and is the Tier-1 conformance binding (the spec models one
    /// detach window at a time).
    pub fn reattach_reflowed_scrollback(&mut self, reflowed: ReflowedScrollback) {
        // The reflow window is over: stop staging scroll-off as "detached" — the
        // store (old or freshly attached below) is authoritative again.
        self.storage.scrollback_detached_for_reflow = false;
        self.close_reflow_window_cap_run();
        // Where the reader asked to go while the history was away, and the row
        // counter the aim's growth is measured against, sampled before this
        // re-attach moves any line. Applied only on the normal branch (an erase
        // resets the viewport; a replacement store is not the history aimed at).
        let reader_aim = self.storage.detached_reader_aim.take();
        let counter_at_reattach = self.storage.absolute_row_counter;

        if self.storage.scrollback.is_some() {
            // A terminal reset re-created the tiered store during the reflow;
            // don't clobber it. The reflowed store drops; the job-carried ring
            // history re-enters the ring (capped). The reset also supersedes the
            // deficit-fill debt.
            self.storage.pending_fill_target = None;
            self.reconcile_pending_scrollback_settings();
            self.reattach_ring_history(reflowed.ring_out, reflowed.new_cols);
            return;
        }

        // Scrollback was erased (ED3 / `clear` / reset) during the window: do
        // not resurrect pre-erase history. Attach an empty store and keep only
        // the post-erase output the ring/lazy captured. The job-carried ring
        // history predates the erase too, so `ring_out` drops as well.
        if reflowed.clear_gen != self.storage.scrollback_clear_gen {
            let mut store = reflowed.store;
            let _ = store.clear();
            self.storage.scrollback = Some(store);
            // An erase means the pre-erase blanks are GENUINE now: never fill
            // them from (erased) history.
            self.storage.pending_fill_target = None;
            self.reconcile_pending_scrollback_settings();
            self.drain_lazy_buffer(); // post-erase window output (erase cleared the pre-erase lazy)
            self.storage.display_offset = 0; // an erase resets the scroll position
            self.storage.damage = Damage::Full;
            self.storage.content_gen += 1;
            return;
        }

        self.storage.scrollback = Some(reflowed.store);
        self.reconcile_pending_scrollback_settings();
        // The ring history returns before the lazy drain below, so the final
        // order is [old history | old ring history | window output | live
        // ring]; see `reattach_ring_history` for the seat arithmetic.
        self.reattach_ring_history(reflowed.ring_out, reflowed.new_cols);
        // Flush the lines that scrolled off during the window (staged in the
        // lazy buffer) into the store after the reflowed old history.
        self.drain_lazy_buffer();
        // Bottom-anchor the viewport: the width-grow that opened this window
        // unwrapped content into fewer rows, and the synchronous resize could not
        // pull history back (it was with this job). Fill the blank band now,
        // re-seating the boundary line's tail. Only when the job's width still
        // matches; a stale width re-detaches instead
        // (`reattach_reflowed_scrollback_or_redetach`) and fills on its own
        // re-attach, so the target survives until then.
        if reflowed.new_cols == self.storage.cols
            && let Some(target) = self.storage.pending_fill_target.take()
        {
            self.fill_viewport_deficit_from_history(target);
        }
        let sb = self.storage.scrollback_lines();
        // Restore the reader's pre-detach scroll position, clamped to the
        // regrown history (the synchronous resize clamped it against an empty
        // history while the store was out), unless the reader went to the live
        // bottom during the window (e.g. pressed End to follow output).
        //
        // Only the reader's own action counts, and `display_offset` cannot
        // show it: the detach's clamp writes the same 0 as an End press, so a
        // bare `display_offset == 0` test would lose every scrolled-back
        // reader's place on a width change. A high-water of the offset during
        // the window fails the other way, counting machine raises (per-batch
        // repin, rows-only re-anchor) as something to descend from.
        //
        // `reader_live_bottom_gen` advances on exactly one event: a
        // reader-facing scroll primitive taking `display_offset` from non-zero
        // to zero; machine paths never advance it. So an advance since the
        // detach is the reader's choice, including scrolling up over output
        // that streamed in during the window and then pressing End.
        //
        // Residual: where the detach already pinned the viewport to the live
        // bottom, an End press moves nothing and records nothing, so the
        // restore wins, which is the right default.
        //
        // This is offset identity, not line identity: `prev_offset` is a
        // distance from the live bottom, so output arriving during the window
        // shifts the restored view by that much. A width reflow has no
        // pre→post absolute-row map to do better with.
        //
        // The reader's aim: a reader who asked during the window for more than
        // the attached rows could show gets that place now: at least as far up
        // as asked (never below the restore), the top if they asked for the
        // top, the live bottom if they came back to it. Gestures that fit the
        // attached rows record no aim.
        let reader_descended_to_live =
            self.storage.reader_live_bottom_gen != reflowed.detach_live_bottom_gen;
        if !(reader_descended_to_live && self.storage.display_offset == 0) {
            self.storage.display_offset = reflowed.prev_offset.min(sb);
        }
        if let Some(aim) = reader_aim {
            self.storage.display_offset =
                aim.resolve(counter_at_reattach, sb, self.storage.display_offset);
        }
        self.storage.damage = Damage::Full;
        self.storage.content_gen += 1;
        // The rewrapped history can hold more rows than the counter covers.
        self.cover_retained_rows();
    }

    /// Re-attach, then converge: if the width changed while the reflow ran (a
    /// superseding drag step, which detached nothing), the re-attached store
    /// (ring history included, routed there by `reattach_ring_history`) is
    /// wrapped at the job's stale width. Instead of waiting for a next width
    /// change that may never come, immediately re-detach it for one more
    /// off-thread rewrap at the current width and return the job.
    ///
    /// Termination: each job carries the width seen at its detach, so once the
    /// width settles at most one extra pass runs. Nothing is cancelled; content
    /// re-attaches before every re-detach. The erase-during-window and
    /// replacement-store races re-attach as
    /// [`reattach_reflowed_scrollback`](Self::reattach_reflowed_scrollback) and
    /// never re-detach. The follow-up job carries no `ring_lines`: the
    /// superseding resize already rewrapped the live ring at the settled width.
    /// Behind a store that keeps no line the follow-up carries nothing: the
    /// re-attach put the history back into the ring at the current width
    /// (`reattach_history_into_ring`), and the pass only lands the deficit
    /// fill its re-attach owes.
    #[must_use = "a returned job is a re-detached store: drive it and re-attach it (or abort), or history is lost"]
    pub fn reattach_reflowed_scrollback_or_redetach(
        &mut self,
        reflowed: ReflowedScrollback,
    ) -> Option<PendingScrollbackReflow> {
        let was_erased = reflowed.clear_gen != self.storage.scrollback_clear_gen;
        let had_replacement = self.storage.scrollback.is_some();
        let stale_cols = reflowed.new_cols;
        self.reattach_reflowed_scrollback(reflowed);
        if was_erased || had_replacement || stale_cols == self.storage.cols {
            return None;
        }
        // Defensive: the normal re-attach above always installs a store.
        // Written as `?` because `clippy::question_mark` rejects the long form.
        self.storage.scrollback.as_ref()?;

        // Width mismatch on a normal re-attach: open one more detach window at
        // the settled width. Same captures as `resize_offloading_scrollback`
        // minus the resize: the grid already has the target geometry and its
        // ring was rewrapped by the superseding resize.
        let prev_offset = self.storage.display_offset;
        let clear_gen = self.storage.scrollback_clear_gen;
        let lazy_lines: Vec<Line> = self.storage.lazy_buffer.drain_all().collect();
        let pending_settings = PendingScrollbackSettings {
            line_limit: self.storage.scrollback_line_limit(),
            memory_budget: self
                .storage
                .scrollback
                .as_ref()
                .expect("scrollback.is_none() early-returned above")
                .memory_budget(),
            line_limit_changed: false,
            memory_budget_changed: false,
        };
        let store = self
            .storage
            .scrollback
            .take()
            .expect("scrollback.is_none() early-returned above");
        // Clamp: taking the lazy buffer and store removed history under
        // `display_offset`, which the re-attach just restored to full-history
        // depth. Unclamped, the viewport would address history that is with the
        // worker. `prev_offset` was captured before the clamp, so the reader's
        // real depth still rides the job.
        self.clamp_display_offset();
        // This window's gesture baseline, sampled like the first detach's. The
        // clamp above does not advance the generation.
        let detach_live_bottom_gen = self.storage.reader_live_bottom_gen;
        self.close_reflow_window_cap_run();
        self.storage.scrollback_detached_for_reflow = true;
        self.storage.pending_scrollback_settings = Some(pending_settings);
        self.storage.detached_reader_aim = None;
        let phase = ReflowPhase::start(&store);
        let lines_total = store.line_count() + lazy_lines.len();
        Some(PendingScrollbackReflow {
            store,
            lazy_lines,
            ring_lines: Vec::new(),
            lines_total,
            lines_read: 0,
            new_cols: self.storage.cols,
            prev_offset,
            detach_live_bottom_gen,
            clear_gen,
            phase,
            lifted_limits: None,
        })
    }

    /// Return the job-carried ring history (already rewrapped to `new_cols`)
    /// to the grid at re-attach.
    ///
    /// The newest lines re-enter the ring as scrollback rows ahead of any
    /// flight scroll-off, via `prepend_ring_scrollback_lines` (the O(ring ×
    /// cols) row rebuild, run here under the brief re-attach lock). Lines that
    /// no longer fit (rewrap growth, or a ring that partly refilled during the
    /// flight) go into the tiered store before the caller drains the staged
    /// window output, preserving [old store history | old ring history |
    /// window output | live ring]. The whole ring history takes the store
    /// route when staged window output exists (the ring's current rows and the
    /// spill are both newer, so the store's front is the only order-correct
    /// seat), and when the job's width is stale (ring rows can only be rebuilt
    /// at the current width, while the store tolerates over-wide stale lines
    /// until the next width change rewraps them).
    ///
    /// With the store at its line limit, lines routed there are subject to its
    /// push-time eviction; the all-to-store case can evict a few lines, bounded
    /// by the ring cap.
    ///
    /// A store that keeps no line (`GridStorage::store_retains_lines`: a total
    /// at or below the ring cap) is no seat at any width, ruling 2026-10-11:
    /// the ring is the whole retention, as on a ring-only grid of cap = total
    /// (see [`reattach_history_into_ring`](Self::reattach_history_into_ring)).
    fn reattach_ring_history(&mut self, mut ring_out: Vec<Line>, new_cols: u16) {
        if !self.storage.store_retains_lines() {
            self.reattach_history_into_ring(ring_out, new_cols);
            return;
        }
        if ring_out.is_empty() {
            return;
        }
        let room = if self.storage.lazy_buffer.is_empty() && new_cols == self.storage.cols {
            self.storage
                .max_scrollback
                .saturating_sub(self.storage.ring_buffer_scrollback())
        } else {
            // Staged flight output, or a width-stale result: the store is the
            // only seat that is both order-correct and width-tolerant.
            0
        };
        let skip = ring_out.len().saturating_sub(room);
        if skip > 0 {
            if let Some(store) = self.storage.scrollback.as_mut() {
                for line in ring_out.drain(..skip) {
                    if let Err(error) = store.push_line(line) {
                        aterm_log::warn!(
                            "ring-history push during reflow re-attach failed: {error}"
                        );
                    }
                }
            } else {
                // No store to route into (unreachable: a grid with no store,
                // like one whose store keeps nothing, took the ring route
                // above): the ring cap is the retention bound, oldest evicted.
                drop(ring_out.drain(..skip));
            }
        }
        if !ring_out.is_empty() {
            self.prepend_ring_scrollback_lines(ring_out, new_cols);
        }
    }

    /// [`reattach_ring_history`](Self::reattach_ring_history) when the store
    /// keeps no line: the job's ring history (`lines`, rewrapped at
    /// `new_cols`) and then the window's staged output go back into the ring,
    /// the newest first into the room the window's own output left, at the
    /// grid's current width; the rest goes, as a ring-only grid's cap drops
    /// it. History stays a contiguous suffix of what was written.
    ///
    /// Ruling 2026-10-11. The store route this replaces had the ring history
    /// pushed into that store, which dropped every line, whenever the job came
    /// back for a width the grid had left or the window had staged output: a
    /// drag step during the rewrap (the window's own resize path) lost the
    /// whole scrollback the total allows. Staged output is taken with it
    /// because it is newer than the job's lines and older than the ring's:
    /// seating one without the other would leave a hole.
    ///
    /// It can keep more than a ring-only grid of that cap would, never past
    /// the total and never out of order: where a rows grow inside the window
    /// pulled ring rows onto the screen, the staged rows fill the room that
    /// opened, which the ring-only grid had already emptied at its cap.
    ///
    /// Rewrapped only when a seat needs it (a width the grid has left, or
    /// staged rows of whatever width they left at), and then only the newest
    /// logical lines that fill the room (`reflow_newest_rows`), so the cost
    /// under the re-attach lock is the ring rebuild's O(room × cols) plus
    /// materializing the staged rows, which a store that keeps lines pays at
    /// this same re-attach by draining them into it.
    fn reattach_history_into_ring(&mut self, mut lines: Vec<Line>, new_cols: u16) {
        let cols = self.storage.cols;
        let room = self
            .storage
            .max_scrollback
            .saturating_sub(self.storage.ring_buffer_scrollback());
        if self.storage.lazy_buffer.is_empty() && new_cols == cols {
            lines.drain(..lines.len().saturating_sub(room));
        } else {
            lines.extend(self.storage.lazy_buffer.drain_all());
            self.storage.lazy_buffer.release_spine_slack();
            lines = super::scrollback_reflow::reflow_newest_rows(&lines, cols, room);
        }
        if !lines.is_empty() {
            self.prepend_ring_scrollback_lines(lines, cols);
        }
    }

    /// Replay scrollback settings changed while the tiered store was detached.
    ///
    /// The byte budget is applied first so draining staged window output cannot
    /// evict against a stale, lower budget. A line-limit setter may itself drain
    /// that staged output before truncating the unified ring+store retained set.
    fn reconcile_pending_scrollback_settings(&mut self) {
        let Some(settings) = self.storage.pending_scrollback_settings.take() else {
            return;
        };
        if settings.memory_budget_changed
            && self.storage.scrollback.is_some()
            && let Err(error) = self.set_scrollback_memory_budget(settings.memory_budget)
        {
            aterm_log::warn!(
                "re-attached scrollback could not fully enforce deferred memory budget: {error}"
            );
        }
        if settings.line_limit_changed {
            self.set_scrollback_line_limit(settings.line_limit);
        }
    }

    /// Record a reader-facing viewport gesture made while the history is away
    /// for a reflow; called by the primitive after it moved the viewport over
    /// what is attached, with `before` the offset it moved from. No-op outside
    /// a detach window.
    ///
    /// A gesture that fits the attached rows records nothing: the re-attach's
    /// restore rule covers those. A gesture asking for more (up past the
    /// attached top) is kept as the reader's aim and applied at re-attach. The
    /// viewport meanwhile rests at the attached top; scrolling back down spends
    /// the aim first, after which the reader's exact place is kept (`At`).
    /// `Top` aims at the top of the whole history. `Live` (End, a keystroke's
    /// snap) brings an aim back to the live bottom; with no aim it records
    /// nothing (an End that moved nothing is not evidence of a choice).
    pub(crate) fn note_detached_reader_motion(&mut self, motion: ReaderMotion, before: usize) {
        if !self.storage.scrollback_detached_for_reflow {
            return;
        }
        let counter = self.storage.absolute_row_counter;
        let attached = self.storage.scrollback_lines();
        let current = self.storage.detached_reader_aim;
        let next = match motion {
            ReaderMotion::Top => Some(DetachedReaderAim::FromTop { rows: 0 }),
            ReaderMotion::Live => current.map(|_| DetachedReaderAim::At {
                rows: 0,
                at_counter: counter,
            }),
            ReaderMotion::By(0) => return,
            ReaderMotion::By(delta) => {
                let by = i64::from(delta);
                let moved = |rows: usize, by: i64| {
                    let rows = i64::try_from(rows).unwrap_or(i64::MAX);
                    usize::try_from(rows.saturating_add(by).max(0)).unwrap_or(usize::MAX)
                };
                match current {
                    Some(DetachedReaderAim::FromTop { rows }) => Some(DetachedReaderAim::FromTop {
                        rows: moved(rows, -by),
                    }),
                    // Where the reader stood: past the attached top by an aim,
                    // else wherever the viewport was.
                    aim => {
                        let from = match aim {
                            Some(DetachedReaderAim::Above { rows, at_counter }) => {
                                grown_from_live(rows, at_counter, counter)
                            }
                            _ => before,
                        };
                        let want = moved(from, by);
                        if want > attached {
                            Some(DetachedReaderAim::Above {
                                rows: want,
                                at_counter: counter,
                            })
                        } else if aim.is_some() {
                            // Back inside the attached rows after aiming past
                            // them: the viewport goes exactly there, the live
                            // bottom included.
                            self.set_display_offset_in_window(want);
                            Some(DetachedReaderAim::At {
                                rows: want,
                                at_counter: counter,
                            })
                        } else {
                            // Every gesture so far fit: the restore rule stands.
                            None
                        }
                    }
                }
            }
        };
        if let Some(DetachedReaderAim::Above { .. } | DetachedReaderAim::FromTop { .. }) = next {
            // The aim is past the attached top: the viewport rests there.
            self.set_display_offset_in_window(attached);
        }
        self.storage.detached_reader_aim = next;
    }

    /// Put the viewport at `offset` (clamped to what is attached) with targeted
    /// display-offset damage: the aim's own correction of where a primitive
    /// left it. Not a gesture record.
    fn set_display_offset_in_window(&mut self, offset: usize) {
        let target = offset.min(self.storage.scrollback_lines());
        let old = self.storage.display_offset;
        if target == old {
            return;
        }
        self.storage.display_offset = target;
        let dmg = crate::damage::compute_display_offset_damage(old, target, self.rows());
        self.storage.damage.apply_display_offset_damage(dmg);
    }

    /// Whether the reader has aimed the viewport inside the open detach window;
    /// a keystroke's snap to the live bottom must then reach
    /// [`Grid::scroll_to_bottom`] even though the viewport already sits at 0.
    #[must_use]
    pub fn reader_aim_held(&self) -> bool {
        self.storage.detached_reader_aim.is_some()
    }

    /// True while a detach window is open: the tiered store is out for an
    /// off-thread reflow and scroll-off is staged for re-attach. Must be false
    /// again after [`reattach_reflowed_scrollback`](Self::reattach_reflowed_scrollback)
    /// or [`abort_reflow_offload`](Self::abort_reflow_offload); a lingering
    /// `true` means un-drainable lazy growth. Projected as the spec's `detached`
    /// variable by `tests/conformance_offload.rs`.
    #[must_use]
    pub fn reflow_offload_in_flight(&self) -> bool {
        self.storage.scrollback_detached_for_reflow
    }

    /// Recover from a reflow that will never re-attach (the worker panicked or
    /// its thread died). The detached tiered and ring history, owned by the
    /// lost [`PendingScrollbackReflow`], is unrecoverable; this returns the grid
    /// to a bounded state. Without it `scrollback_detached_for_reflow` would
    /// stay `true`, every scroll-off would stage into an un-drainable
    /// `lazy_buffer`, and tiered history would stay invisible. No-op if not
    /// mid-window (already re-attached, or a reset re-created the store).
    ///
    /// Every exit, including the no-op early return, closes the detach window.
    /// That postcondition is checked by debug assertions rather than an
    /// `ensures` clause: stock rustc cannot parse contracts, and it compiles
    /// this crate for the wasm32 lane, the x86_64 compat slice and the cross
    /// cells, while the `#[cfg_attr(trust_verify, ...)]` spelling does not
    /// lower to the same obligation.
    pub fn abort_reflow_offload(&mut self) {
        if !self.storage.scrollback_detached_for_reflow {
            debug_assert!(
                !self.storage.scrollback_detached_for_reflow,
                "abort_reflow_offload must close the detach window on every exit"
            );
            return;
        }
        self.storage.scrollback_detached_for_reflow = false;
        self.close_reflow_window_cap_run();
        // The job (and the history a deficit fill would have pulled) is lost:
        // drop the fill debt and the reader's aim with it.
        self.storage.pending_fill_target = None;
        self.storage.detached_reader_aim = None;
        if self.storage.scrollback.is_some() {
            // A reset/recovery path installed a replacement store before the
            // failed worker was noticed. It is authoritative: replay the newest
            // settings and preserve window output by draining into it.
            self.reconcile_pending_scrollback_settings();
            self.drain_lazy_buffer();
        } else {
            // Fall back to ring-only scrollback: the tiered store is gone, so
            // the lazy buffer's window output can no longer be tiered — discard
            // it (bounded) rather than leave it un-drainable.
            self.storage.lazy_buffer.clear();
            // History just went away under `display_offset`, so re-clamp: a
            // reader who scrolled up during the window was clamped against a
            // total that counted the staged lines.
            self.clamp_display_offset();

            // A LOWER requested total still tightens the surviving ring. Never
            // expand this emergency fallback for a higher/unlimited request:
            // abort exists to recover to the construction-bounded ring after
            // the worker (and its long-term store) was lost. A byte budget has
            // no backend here and is intentionally discarded.
            if let Some(settings) = self.storage.pending_scrollback_settings.take()
                && settings.line_limit_changed
                && let Some(limit) = settings.line_limit
                && limit < self.storage.max_scrollback
            {
                self.set_scrollback_line_limit(Some(limit));
            }
        }
        self.storage.damage = Damage::Full;
        self.storage.content_gen += 1;
        debug_assert!(
            !self.storage.scrollback_detached_for_reflow,
            "abort_reflow_offload must close the detach window on every exit"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_scrollback::Scrollback;

    /// Deterministic PCG-flavoured generator (no dev-dependency needed).
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Small ring so nearly all scroll-off spills to the tiered store, like a
    /// real session (the cost_bound.rs recipe).
    fn tiered_grid(rows: u16, cols: u16, ring: usize) -> Grid {
        let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
        Grid::with_tiered_scrollback(rows, cols, ring, sb)
    }

    /// Write one logical line (autowrapping into soft-wrapped continuation rows
    /// when longer than the width), then terminate it with a hard newline.
    fn logical_line(grid: &mut Grid, text: &str) {
        for c in text.chars() {
            grid.write_char_wrap(c); // deferred autowrap: long text soft-wraps
        }
        grid.line_feed();
        grid.carriage_return();
    }

    /// Seeded mixed-shape history: blanks, short lines, multi-row soft-wrapped
    /// runs, wide (CJK) content — the shapes whose carving could go wrong.
    fn feed_mixed_history(grid: &mut Grid, seed: u64, logical_lines: usize, cols: u16) {
        let mut rng = Rng(seed);
        for i in 0..logical_lines {
            match rng.below(5) {
                0 => logical_line(grid, ""), // blank hard-newline row
                1 => logical_line(grid, &format!("S{i}")),
                2 => {
                    // A soft-wrapped run of 2..=6 rows at the old width.
                    let len = cols as usize * (2 + rng.below(4) as usize)
                        + rng.below(u64::from(cols)) as usize;
                    let mut s = format!("R{i}-");
                    while s.len() < len {
                        s.push((b'a' + rng.below(26) as u8) as char);
                    }
                    logical_line(grid, &s);
                }
                3 => {
                    // Wide chars (2 cells each), possibly wrapping.
                    let n = 1 + rng.below(u64::from(cols)) as usize;
                    let mut s = format!("W{i}-");
                    for _ in 0..n {
                        s.push(if rng.below(2) == 0 { '世' } else { '界' });
                    }
                    logical_line(grid, &s);
                }
                _ => {
                    let pad = "x".repeat(rng.below(u64::from(cols)) as usize);
                    logical_line(grid, &format!("M{i}-{pad}"));
                }
            }
        }
    }

    /// Deep fingerprint of a store's full decoded content — text, attrs, wrap
    /// flags, hyperlink + underline-colour spans, everything a [`Line`]
    /// carries — via each line's Debug rendering.
    fn store_fingerprint(store: &ScrollbackStorage) -> Vec<String> {
        (0..store.line_count())
            .map(|i| format!("{:?}", store.get_line(i).expect("decode").expect("present")))
            .collect()
    }

    /// Deep fingerprint of a completed rewrap: the store's decoded content plus
    /// the job-carried ring history, the whole result any step schedule must
    /// reproduce.
    fn reflowed_fingerprint(reflowed: &ReflowedScrollback) -> Vec<String> {
        let mut fp = store_fingerprint(&reflowed.store);
        fp.extend(reflowed.ring_out.iter().map(|line| format!("{line:?}")));
        fp
    }

    /// Drive a job to completion with random per-step budgets in
    /// `1..=max_budget`, returning the result and the number of steps taken.
    fn step_with_schedule(
        mut job: PendingScrollbackReflow,
        rng: &mut Rng,
        max_budget: u64,
    ) -> (ReflowedScrollback, usize) {
        let mut steps = 0usize;
        loop {
            steps += 1;
            assert!(steps < 1_000_000, "stepping must terminate");
            match job.reflow_step(1 + rng.below(max_budget) as usize) {
                ReflowStep::InProgress(next) => job = next,
                ReflowStep::Done(done) => return (done, steps),
            }
        }
    }

    #[test]
    fn detached_settings_apply_on_normal_reattach() {
        let (rows, cols) = (10u16, 40u16);
        let mut g = tiered_grid(rows, cols, 8);
        g.set_scrollback_line_limit(Some(200));
        g.set_scrollback_memory_budget(7_000_000)
            .expect("initial budget");
        for i in 0..400 {
            logical_line(&mut g, &format!("H{i}"));
        }
        assert!(g.storage.pending_scrollback_settings.is_none());

        let job = g
            .resize_offloading_scrollback(rows, 20)
            .expect("tiered store detaches");
        assert!(
            g.storage.pending_scrollback_settings.is_some(),
            "settings snapshot exists exactly while detach is unresolved"
        );
        assert_eq!(
            g.scrollback_line_limit(),
            Some(200),
            "detach keeps the effective total observable"
        );
        assert_eq!(g.scrollback_memory_budget(), Some(7_000_000));

        g.set_scrollback_line_limit(Some(40));
        g.set_scrollback_memory_budget(4_000_000)
            .expect("detached mutation is deferred");
        assert_eq!(g.scrollback_line_limit(), Some(40));
        assert_eq!(g.scrollback_memory_budget(), Some(4_000_000));
        assert!(g.scrollback().is_none(), "the worker still owns the store");

        g.reattach_reflowed_scrollback(job.reflow());
        assert!(
            g.storage.pending_scrollback_settings.is_none(),
            "reattach consumes the snapshot exactly once"
        );
        let store = g.scrollback().expect("store re-attached");
        assert_eq!(g.scrollback_line_limit(), Some(40));
        assert_eq!(
            store.line_limit(),
            Some(32),
            "raw store share is total 40 minus the fixed 8-line ring"
        );
        assert_eq!(store.memory_budget(), 4_000_000);
        assert!(
            g.scrollback_lines() <= 40,
            "deferred shrink enforces the unified total immediately"
        );
        g.assert_invariants();
    }

    #[test]
    fn detached_unlimited_limit_round_trips_on_reattach() {
        let (rows, cols) = (10u16, 40u16);
        let mut g = tiered_grid(rows, cols, 8);
        g.set_scrollback_line_limit(Some(40));
        for i in 0..100 {
            logical_line(&mut g, &format!("U{i}"));
        }

        let job = g
            .resize_offloading_scrollback(rows, 20)
            .expect("tiered store detaches");
        g.set_scrollback_line_limit(None);
        assert_eq!(
            g.scrollback_line_limit(),
            None,
            "nested pending state must distinguish unlimited from no mutation"
        );

        g.reattach_reflowed_scrollback(job.reflow());
        assert!(g.storage.pending_scrollback_settings.is_none());
        assert_eq!(g.scrollback_line_limit(), None);
        assert_eq!(
            g.scrollback().expect("store re-attached").line_limit(),
            None,
            "unlimited reaches the raw store"
        );
        g.assert_invariants();
    }

    #[test]
    fn detached_settings_survive_clear_before_reattach() {
        let (rows, cols) = (10u16, 40u16);
        let mut g = tiered_grid(rows, cols, 8);
        logical_line(&mut g, "PRE_CLEAR_ONLY");
        for i in 0..200 {
            logical_line(&mut g, &format!("C{i}"));
        }

        let job = g
            .resize_offloading_scrollback(rows, 20)
            .expect("tiered store detaches");
        g.set_scrollback_line_limit(Some(25));
        g.set_scrollback_memory_budget(3_000_000)
            .expect("detached mutation is deferred");
        g.erase_scrollback();
        for i in 0..30 {
            logical_line(&mut g, &format!("POST_CLEAR_{i}"));
        }

        g.reattach_reflowed_scrollback(job.reflow());
        let store = g.scrollback().expect("cleared store re-attached");
        assert_eq!(g.scrollback_line_limit(), Some(25));
        assert_eq!(store.line_limit(), Some(17));
        assert_eq!(store.memory_budget(), 3_000_000);
        let history = (0..g.scrollback_lines())
            .filter_map(|i| g.get_history_line(i))
            .map(|line| line.to_string())
            .collect::<String>();
        assert!(
            history.contains("POST_CLEAR_"),
            "output produced after clear survives the detach window"
        );
        assert!(
            !history.contains("PRE_CLEAR_ONLY"),
            "clear during reflow must not resurrect pre-clear history"
        );
        assert!(g.storage.pending_scrollback_settings.is_none());
        g.assert_invariants();
    }

    #[test]
    fn detached_settings_apply_to_replacement_store_race() {
        let (rows, cols) = (10u16, 40u16);
        let mut g = tiered_grid(rows, cols, 8);
        for i in 0..200 {
            logical_line(&mut g, &format!("R{i}"));
        }

        let job = g
            .resize_offloading_scrollback(rows, 20)
            .expect("tiered store detaches");
        g.set_scrollback_line_limit(Some(30));
        g.set_scrollback_memory_budget(2_000_000)
            .expect("detached mutation is deferred");
        g.erase_scrollback();
        let mut replacement: ScrollbackStorage = Scrollback::new(64, 512, 6_000_000).into();
        replacement
            .push_line(Line::from("REPLACEMENT_STORE_ONLY"))
            .expect("seed replacement");
        g.attach_scrollback(replacement);
        assert_eq!(
            g.scrollback_line_limit(),
            Some(30),
            "dirty settings apply as soon as the replacement attaches"
        );
        assert_eq!(g.scrollback_memory_budget(), Some(2_000_000));

        // A newer mutation after replacement attach must win over both the
        // original request and the stale worker's settings.
        g.set_scrollback_line_limit(Some(35));
        g.set_scrollback_memory_budget(1_500_000)
            .expect("latest replacement budget");

        g.reattach_reflowed_scrollback(job.reflow());
        let store = g.scrollback().expect("replacement store remains attached");
        assert!(
            (0..store.line_count()).any(|i| {
                store
                    .get_line(i)
                    .ok()
                    .flatten()
                    .and_then(|line| line.as_str().map(|text| text == "REPLACEMENT_STORE_ONLY"))
                    .unwrap_or(false)
            }),
            "the stale worker store must not clobber replacement content"
        );
        assert_eq!(g.scrollback_line_limit(), Some(35));
        assert_eq!(store.line_limit(), Some(27));
        assert_eq!(store.memory_budget(), 1_500_000);
        assert!(g.storage.pending_scrollback_settings.is_none());
        g.assert_invariants();
    }

    #[test]
    fn clean_detach_snapshot_adopts_replacement_store_settings() {
        let (rows, cols) = (10u16, 40u16);
        let mut g = tiered_grid(rows, cols, 8);
        for i in 0..100 {
            logical_line(&mut g, &format!("B{i}"));
        }
        let job = g
            .resize_offloading_scrollback(rows, 20)
            .expect("tiered store detaches");
        g.erase_scrollback();

        let mut replacement: ScrollbackStorage = Scrollback::new(64, 512, 2_500_000).into();
        replacement.set_line_limit(Some(12));
        replacement
            .push_line(Line::from("NEW_BASELINE"))
            .expect("seed replacement");
        g.attach_scrollback(replacement);
        assert_eq!(
            g.scrollback_line_limit(),
            Some(20),
            "clean snapshot follows replacement raw 12 + ring 8"
        );
        assert_eq!(g.scrollback_memory_budget(), Some(2_500_000));

        g.reattach_reflowed_scrollback(job.reflow());
        let store = g.scrollback().expect("replacement remains authoritative");
        assert_eq!(g.scrollback_line_limit(), Some(20));
        assert_eq!(store.line_limit(), Some(12));
        assert_eq!(store.memory_budget(), 2_500_000);
        assert!(g.storage.pending_scrollback_settings.is_none());
        g.assert_invariants();
    }

    #[test]
    fn detached_line_limit_applies_to_ring_after_abort() {
        let (rows, cols) = (10u16, 40u16);
        let mut g = tiered_grid(rows, cols, 8);
        for i in 0..200 {
            logical_line(&mut g, &format!("A{i}"));
        }

        let job = g
            .resize_offloading_scrollback(rows, 20)
            .expect("tiered store detaches");
        g.set_scrollback_line_limit(Some(5));
        g.set_scrollback_memory_budget(1_000_000)
            .expect("detached mutation is deferred");
        drop(job);
        g.abort_reflow_offload();

        assert!(g.storage.pending_scrollback_settings.is_none());
        assert!(g.scrollback().is_none(), "the failed worker lost its store");
        assert_eq!(
            g.scrollback_memory_budget(),
            None,
            "a byte budget has no backend after abort"
        );
        assert_eq!(
            g.scrollback_line_limit(),
            Some(5),
            "the requested total still caps the surviving ring"
        );
        for i in 0..100 {
            logical_line(&mut g, &format!("P{i}"));
        }
        assert_eq!(g.scrollback_lines(), 5);
        g.assert_invariants();
    }

    #[test]
    fn abort_never_expands_fallback_ring_for_raise_or_unlimited() {
        for requested in [Some(100), None] {
            let (rows, cols) = (10u16, 40u16);
            let mut g = tiered_grid(rows, cols, 8);
            for i in 0..100 {
                logical_line(&mut g, &format!("E{i}"));
            }
            let job = g
                .resize_offloading_scrollback(rows, 20)
                .expect("tiered store detaches");
            g.set_scrollback_line_limit(requested);
            drop(job);
            g.abort_reflow_offload();

            assert_eq!(
                g.scrollback_line_limit(),
                Some(8),
                "emergency ring stays construction-bounded for request {requested:?}"
            );
            assert!(g.storage.pending_scrollback_settings.is_none());
            for i in 0..100 {
                logical_line(&mut g, &format!("F{i}"));
            }
            assert_eq!(g.scrollback_lines(), 8);
            g.assert_invariants();
        }
    }

    #[test]
    fn abort_preserves_staged_output_when_replacement_store_exists() {
        let (rows, cols) = (10u16, 40u16);
        let mut g = tiered_grid(rows, cols, 8);
        for i in 0..100 {
            logical_line(&mut g, &format!("OLD{i}"));
        }
        let job = g
            .resize_offloading_scrollback(rows, 20)
            .expect("tiered store detaches");
        g.erase_scrollback();
        g.set_scrollback_line_limit(Some(100));
        g.set_scrollback_memory_budget(2_000_000)
            .expect("defer replacement budget");

        let mut replacement: ScrollbackStorage = Scrollback::new(64, 512, 6_000_000).into();
        replacement
            .push_line(Line::from("ABORT_REPLACEMENT_BASE"))
            .expect("seed replacement");
        g.attach_scrollback(replacement);
        g.set_compress_offload_active(true);
        for i in 0..30 {
            logical_line(&mut g, &format!("WINDOW_AFTER_REPLACEMENT_{i}"));
        }
        assert!(
            g.lazy_backlog_len() > 0,
            "precondition: replacement-window rows are staged"
        );

        drop(job);
        g.abort_reflow_offload();
        assert!(!g.reflow_offload_in_flight());
        assert_eq!(g.lazy_backlog_len(), 0, "abort drains into replacement");
        assert!(g.storage.pending_scrollback_settings.is_none());
        assert_eq!(g.scrollback_line_limit(), Some(100));
        assert_eq!(g.scrollback_memory_budget(), Some(2_000_000));
        let history = (0..g.scrollback_lines())
            .filter_map(|i| g.get_history_line(i))
            .map(|line| line.to_string())
            .collect::<String>();
        assert!(history.contains("ABORT_REPLACEMENT_BASE"));
        assert!(
            history.contains("WINDOW_AFTER_REPLACEMENT_"),
            "abort must preserve rows staged after replacement attach"
        );
        g.assert_invariants();
    }

    #[test]
    fn attached_memory_budget_settles_lazy_rows_under_new_budget() {
        let (rows, cols) = (10u16, 40u16);
        let mut g = tiered_grid(rows, cols, 8);
        g.set_compress_offload_active(true);
        for i in 0..100 {
            logical_line(&mut g, &format!("M{i}"));
        }
        assert!(g.lazy_backlog_len() > 0, "precondition: rows are staged");
        let before = g.scrollback_lines();

        g.set_scrollback_memory_budget(4_000_000)
            .expect("new budget enforces after staged rows are drained");

        assert_eq!(g.lazy_backlog_len(), 0);
        assert_eq!(g.scrollback_memory_budget(), Some(4_000_000));
        assert_eq!(
            g.scrollback_lines(),
            before,
            "a generous budget moves staged rows without losing history"
        );
        g.assert_invariants();
    }

    /// Measurement harness for the per-line rewrap cost — the number the wasm
    /// modules' `REFLOW_STEP_BUDGET_LINES` default is derived from. Run
    /// manually in release:
    /// `targo --unverified test -p aterm-grid --release --lib reflow_step_timing -- --ignored --nocapture`
    ///
    /// Measured 2026-07-14 (Apple Silicon, release): one-shot 49_969 near-full
    /// 80-col input lines in ~69ms = ~1.4 µs/input-line; stepped @4000/step:
    /// 38 steps, worst step ~4.8ms, total ~69ms (stepping adds no overhead).
    #[test]
    #[ignore = "manual timing harness (release), not a correctness test"]
    fn reflow_step_timing() {
        let (rows, cols, new_cols) = (24u16, 80u16, 40u16);
        let n = 50_000usize;
        let mut g = {
            let sb: ScrollbackStorage = Scrollback::new(64, 512, 512_000_000).into();
            Grid::with_tiered_scrollback(rows, cols, 8, sb)
        };
        for i in 0..n {
            let mut s = format!("L{i}-");
            while s.len() + 1 < cols as usize {
                s.push('x');
            }
            logical_line(&mut g, &s);
        }
        let job = g.resize_offloading_scrollback(rows, new_cols).expect("job");
        let lines = job.line_count();
        let t0 = std::time::Instant::now();
        let done = job.reflow();
        let one_shot = t0.elapsed();
        println!(
            "one-shot: {lines} input lines (~{cols} cols) -> {} output lines in {:?} \
             = {:.2} us/input-line",
            done.line_count(),
            one_shot,
            one_shot.as_secs_f64() * 1e6 / lines as f64
        );

        // Stepped at the candidate wasm budget, worst step recorded.
        let mut g2 = {
            let sb: ScrollbackStorage = Scrollback::new(64, 512, 512_000_000).into();
            Grid::with_tiered_scrollback(rows, cols, 8, sb)
        };
        for i in 0..n {
            let mut s = format!("L{i}-");
            while s.len() + 1 < cols as usize {
                s.push('x');
            }
            logical_line(&mut g2, &s);
        }
        let mut job = g2
            .resize_offloading_scrollback(rows, new_cols)
            .expect("job");
        let budget = 4_000usize;
        let (mut steps, mut worst, mut total) =
            (0usize, std::time::Duration::ZERO, std::time::Duration::ZERO);
        loop {
            let t = std::time::Instant::now();
            let step = job.reflow_step(budget);
            let dt = t.elapsed();
            steps += 1;
            worst = worst.max(dt);
            total += dt;
            match step {
                ReflowStep::InProgress(next) => job = next,
                ReflowStep::Done(_) => break,
            }
        }
        println!("stepped @ {budget}/step: {steps} steps, worst step {worst:?}, total {total:?}");
    }

    /// THE ACCEPTANCE PROPERTY for the chunking seam: for ANY schedule of step
    /// budgets, the stepped rewrap is content-IDENTICAL to the one-shot
    /// [`PendingScrollbackReflow::reflow`] — same store lines, same order,
    /// compared by each decoded line's full Debug rendering. Random small
    /// budgets across several seeds, over mixed content (soft-wrapped runs,
    /// wide chars, blanks) plus a lazy-staged tail. The per-line model is
    /// `rewrap_round_trip_is_content_stable`; this is the job-level
    /// schedule-independence property.
    #[test]
    fn reflow_step_any_schedule_matches_one_shot() {
        for seed in [1u64, 7, 42, 0x00C0_FFEE] {
            let (rows, cols, new_cols) = (10u16, 40u16, 23u16);
            let mut a = tiered_grid(rows, cols, 8);
            let mut b = tiered_grid(rows, cols, 8);
            for g in [&mut a, &mut b] {
                feed_mixed_history(g, seed, 400, cols);
                // Stage a lazy (not-yet-compressed) tail so the job's
                // `lazy_lines` input path is exercised too.
                g.set_compress_offload_active(true);
                feed_mixed_history(g, seed ^ 0xABCD, 40, cols);
            }
            assert!(
                a.lazy_backlog_len() > 0,
                "seed {seed}: precondition — a staged lazy tail exists"
            );

            let one_shot = a
                .resize_offloading_scrollback(rows, new_cols)
                .expect("width change with a tiered store detaches")
                .reflow();
            let job = b
                .resize_offloading_scrollback(rows, new_cols)
                .expect("width change with a tiered store detaches");
            let mut rng = Rng(seed ^ 0x5EED);
            let (stepped, steps) = step_with_schedule(job, &mut rng, 37);

            assert!(
                steps > 4,
                "seed {seed}: the schedule must actually chunk (took {steps} steps)"
            );
            assert_eq!(stepped.new_cols(), one_shot.new_cols());
            assert_eq!(
                stepped.line_count(),
                one_shot.line_count(),
                "seed {seed}: line counts diverge"
            );
            assert_eq!(
                reflowed_fingerprint(&stepped),
                reflowed_fingerprint(&one_shot),
                "seed {seed}: stepped content != one-shot content"
            );
        }
    }

    /// A logical line is never split mid-run: with budget 1 a soft-wrapped run
    /// many rows long crosses step boundaries intact, matches the one-shot and
    /// round-trips through the re-attached grid.
    #[test]
    fn reflow_step_budget_one_never_splits_a_soft_wrapped_run() {
        let (rows, cols, new_cols) = (10u16, 20u16, 33u16);
        let giant = {
            let mut s = String::from("G-");
            while s.len() < cols as usize * 8 {
                s.push_str("abcdefghij");
            }
            s
        };
        let mut a = tiered_grid(rows, cols, 8);
        let mut b = tiered_grid(rows, cols, 8);
        for g in [&mut a, &mut b] {
            logical_line(g, "before");
            logical_line(g, &giant);
            logical_line(g, "after");
            for i in 0..300 {
                logical_line(g, &format!("pad{i}")); // scroll it all off-screen
            }
        }
        let one_shot = a
            .resize_offloading_scrollback(rows, new_cols)
            .expect("job")
            .reflow();
        let mut job = b.resize_offloading_scrollback(rows, new_cols).expect("job");
        let mut steps = 0usize;
        let stepped = loop {
            steps += 1;
            assert!(steps < 100_000, "budget-1 stepping must terminate");
            match job.reflow_step(1) {
                ReflowStep::InProgress(next) => job = next,
                ReflowStep::Done(done) => break done,
            }
        };
        assert_eq!(
            reflowed_fingerprint(&stepped),
            reflowed_fingerprint(&one_shot),
            "budget-1 stepping must match the one-shot"
        );

        // Round-trip through the real protocol: re-attach and confirm the giant
        // logical line's text survived (joined across its rewrapped rows).
        b.reattach_reflowed_scrollback(stepped);
        let mut joined = String::new();
        for i in 0..b.scrollback_lines() {
            if let Some(line) = b.get_history_line(i) {
                joined.push_str(line.as_str().unwrap_or(""));
            }
        }
        assert!(
            joined.contains(&giant),
            "the soft-wrapped run's content must survive budget-1 stepping \
             (giant {} chars, joined {} chars)",
            giant.len(),
            joined.len()
        );
        b.assert_invariants();
    }

    /// An empty detached history steps to `Done` and re-attaches cleanly.
    #[test]
    fn reflow_step_empty_history_terminates() {
        let (rows, cols) = (10u16, 40u16);
        let mut g = tiered_grid(rows, cols, 8);
        logical_line(&mut g, "visible only"); // never scrolls off
        let job = g
            .resize_offloading_scrollback(rows, 20)
            .expect("a tiered store detaches even when empty");
        let mut rng = Rng(3);
        let (done, _steps) = step_with_schedule(job, &mut rng, 4);
        assert_eq!(done.line_count(), 0);
        g.reattach_reflowed_scrollback(done);
        g.assert_invariants();
    }

    /// A budget of 0 is clamped to 1: a caller bug cannot stall a job forever.
    #[test]
    fn reflow_step_zero_budget_still_makes_progress() {
        let (rows, cols) = (10u16, 30u16);
        let mut g = tiered_grid(rows, cols, 8);
        for i in 0..50 {
            logical_line(&mut g, &format!("Z{i}"));
        }
        let mut job = g.resize_offloading_scrollback(rows, 15).expect("job");
        let mut steps = 0usize;
        loop {
            steps += 1;
            assert!(steps < 10_000, "budget 0 must clamp to 1 and terminate");
            match job.reflow_step(0) {
                ReflowStep::InProgress(next) => job = next,
                ReflowStep::Done(done) => {
                    g.reattach_reflowed_scrollback(done);
                    break;
                }
            }
        }
        assert!(g.scrollback_lines() > 30, "history preserved");
        g.assert_invariants();
    }

    /// `lines_done` of the fixed `lines_total` climbs monotonically through
    /// both phases and reaches the total on the step returning `Done`; the
    /// total never moves while the store swaps input for output.
    #[test]
    fn reflow_progress_climbs_to_the_detached_total() {
        let (rows, cols) = (10u16, 30u16);
        let mut g = tiered_grid(rows, cols, 8);
        for i in 0..400 {
            logical_line(&mut g, &format!("P{i}"));
        }
        let mut job = g.resize_offloading_scrollback(rows, 15).expect("job");
        let total = job.lines_total();
        assert_eq!(total, job.line_count(), "the total is what was detached");
        assert!(total > 300, "{total}");
        assert_eq!(job.lines_done(), 0);
        let mut last = 0;
        let mut steps = 0;
        loop {
            steps += 1;
            match job.reflow_step(37) {
                ReflowStep::InProgress(next) => {
                    assert!(next.lines_done() >= last, "monotone");
                    assert!(next.lines_done() <= total);
                    assert_eq!(next.lines_total(), total, "fixed at detach");
                    last = next.lines_done();
                    job = next;
                }
                ReflowStep::Done(done) => {
                    assert_eq!(
                        last + 37.min(total - last),
                        total,
                        "the last step read the rest"
                    );
                    g.reattach_reflowed_scrollback(done);
                    break;
                }
            }
        }
        assert!(steps > 5, "{steps}");
        g.assert_invariants();
    }

    /// Dropping a half-stepped job loses the detached history (re-attach only
    /// accepts a completed [`ReflowedScrollback`]), and
    /// [`Grid::abort_reflow_offload`] returns the grid to a bounded, non-wedged
    /// state.
    #[test]
    fn dropping_a_half_stepped_job_recovers_via_abort() {
        let (rows, cols) = (10u16, 40u16);
        let mut g = tiered_grid(rows, cols, 8);
        for i in 0..500 {
            logical_line(&mut g, &format!("H{i}"));
        }
        let job = g.resize_offloading_scrollback(rows, 20).expect("job");
        let ReflowStep::InProgress(half) = job.reflow_step(5) else {
            panic!("a 500-line history cannot complete in one 5-line step");
        };
        drop(half); // the cooperative "worker" dies mid-stepping
        g.abort_reflow_offload();
        for i in 0..5000 {
            logical_line(&mut g, &format!("R{i}"));
        }
        assert!(
            g.scrollback_lines() < 1000,
            "post-abort the grid is ring-only bounded, not wedged (got {})",
            g.scrollback_lines()
        );
        g.assert_invariants();
    }

    /// Output scrolling off between steps is staged as in the one-shot window
    /// and lands after the rewrapped old history on re-attach.
    #[test]
    fn stepping_interleaved_with_window_output_keeps_history_and_order() {
        let (rows, cols) = (10u16, 40u16);
        let mut g = tiered_grid(rows, cols, 8);
        for i in 0..400 {
            logical_line(&mut g, &format!("H{i}"));
        }
        let before = g.scrollback_lines();
        let mut job = g.resize_offloading_scrollback(rows, 20).expect("job");
        let mut wrote = 0usize;
        let done = loop {
            // The foreground program keeps streaming while the cooperative
            // "worker" steps — the wasm modules' exact interleaving.
            for _ in 0..3 {
                logical_line(&mut g, &format!("W{wrote}"));
                wrote += 1;
            }
            match job.reflow_step(64) {
                ReflowStep::InProgress(next) => job = next,
                ReflowStep::Done(done) => break done,
            }
        };
        g.reattach_reflowed_scrollback(done);
        let after = g.scrollback_lines();
        assert!(
            after + rows as usize + 8 >= before + wrote,
            "window output must survive stepping (before={before}, wrote={wrote}, \
             after={after})"
        );
        let texts: Vec<String> = (0..after)
            .filter_map(|i| {
                g.get_history_line(i)
                    .and_then(|l| l.as_str().map(str::to_string))
            })
            .collect();
        let last_h = texts.iter().rposition(|t| t.starts_with('H'));
        let first_w = texts.iter().position(|t| t.starts_with('W'));
        match (last_h, first_w) {
            (Some(lh), Some(fw)) => assert!(
                lh < fw,
                "old history must precede window output after re-attach \
                 (last H at {lh}, first W at {fw})"
            ),
            _ => panic!("both H and W lines must be present in history"),
        }
        g.assert_invariants();
    }

    /// Single-row logical lines only (blanks, short/medium ASCII, wide CJK
    /// under one row). No multi-row runs: a run straddling a detach window's
    /// seam (the job's history, then what an in-window resize re-took) still
    /// re-splits there, and this corpus isolates placement/order identity.
    fn feed_single_row_history(grid: &mut Grid, seed: u64, logical_lines: usize, cols: u16) {
        let mut rng = Rng(seed);
        for i in 0..logical_lines {
            match rng.below(4) {
                0 => logical_line(grid, ""),
                1 => logical_line(grid, &format!("S{i}")),
                2 => {
                    // Wide chars, capped to fit one row at BOTH toggle widths.
                    let n = 1 + rng.below(u64::from(cols) / 4) as usize;
                    let mut s = format!("W{i}-");
                    for _ in 0..n {
                        s.push(if rng.below(2) == 0 { '世' } else { '界' });
                    }
                    logical_line(grid, &s);
                }
                _ => {
                    let pad = "x".repeat(rng.below(u64::from(cols) / 2) as usize);
                    logical_line(grid, &format!("M{i}-{pad}"));
                }
            }
        }
    }

    /// One width change for the offloaded-vs-plain differential below: the
    /// grid's shape and ring, then the drag of `(rows, cols)` steps. The first
    /// step detaches; the rest land while the job is out, as plain resizes
    /// inside the detach window (the GUI's superseding drag steps), each after
    /// `output` more lines (scroll-off a full ring stages for re-attach).
    /// `fit` caps the corpus so every line is one row at every width, unless
    /// the drag is `wrapping` (`feed_wrapping_history`, one step only).
    ///
    /// A drag with output runs with the reader at the live bottom only: a
    /// reader scrolled back over window output is restored by offset (see
    /// "offset identity" in `reattach_reflowed_scrollback`), which the plain
    /// path's per-batch repin does not share.
    struct Drag {
        label: &'static str,
        wrapping: bool,
        policy: ResizePolicy,
        ring: usize,
        rows: u16,
        cols: u16,
        steps: &'static [(u16, u16)],
        output: usize,
        fit: u16,
    }

    const DRAGS: [Drag; 8] = [
        Drag {
            wrapping: false,
            label: "one narrowing step, the ring holds its overflow",
            policy: ResizePolicy::Native,
            ring: 32,
            rows: 10,
            cols: 40,
            steps: &[(10, 23)],
            output: 0,
            fit: 40,
        },
        // The window's steps push the screen's top rows off while the ring
        // holds the first step's: the history must read them in that order.
        Drag {
            wrapping: false,
            label: "a drag whose later steps land inside the window",
            policy: ResizePolicy::Native,
            ring: 8,
            rows: 13,
            cols: 40,
            steps: &[(10, 35), (4, 30), (3, 28)],
            output: 0,
            fit: 20,
        },
        // ... and behind the output the window staged meanwhile.
        Drag {
            wrapping: false,
            label: "a drag with output streaming between its steps",
            policy: ResizePolicy::Native,
            ring: 8,
            rows: 13,
            cols: 40,
            steps: &[(10, 35), (4, 30), (3, 28)],
            output: 12,
            fit: 20,
        },
        // One step pushing more rows off the screen than the ring holds: the
        // rows past the ring are history too.
        Drag {
            wrapping: false,
            label: "one step pushing more rows than the ring holds",
            policy: ResizePolicy::Native,
            ring: 8,
            rows: 13,
            cols: 40,
            steps: &[(3, 23)],
            output: 0,
            fit: 20,
        },
        Drag {
            wrapping: false,
            label: "the same push under ConPTY",
            policy: ResizePolicy::ConPty,
            ring: 8,
            rows: 13,
            cols: 40,
            steps: &[(3, 23)],
            output: 0,
            fit: 20,
        },
        // Lines that wrap: a run straddling the store/ring seam rewraps whole,
        // as the plain resize rewraps it.
        Drag {
            wrapping: true,
            label: "one narrowing step over lines that wrap",
            policy: ResizePolicy::Native,
            ring: 8,
            rows: 10,
            cols: 40,
            steps: &[(10, 23)],
            output: 0,
            fit: 40,
        },
        Drag {
            wrapping: true,
            label: "the same over lines that wrap, under ConPTY",
            policy: ResizePolicy::ConPty,
            ring: 8,
            rows: 10,
            cols: 40,
            steps: &[(10, 23)],
            output: 0,
            fit: 40,
        },
        Drag {
            wrapping: true,
            label: "one step over lines that wrap, pushing more rows than the ring holds",
            policy: ResizePolicy::Native,
            ring: 8,
            rows: 13,
            cols: 40,
            steps: &[(3, 23)],
            output: 0,
            fit: 40,
        },
    ];

    /// Logical lines of 0..3 widths of ASCII at `cols`, so most wrap into
    /// multi-row runs and some straddle the store/ring seam.
    fn feed_wrapping_history(grid: &mut Grid, seed: u64, logical_lines: usize, cols: u16) {
        let mut rng = Rng(seed);
        for i in 0..logical_lines {
            let len = rng.below(u64::from(cols) * 3) as usize;
            let mut s = format!("R{i}-");
            while s.len() < len {
                s.push((b'a' + rng.below(26) as u8) as char);
            }
            logical_line(grid, &s);
        }
    }

    /// With the ring history riding the job, the offloaded width change ends
    /// where the plain synchronous `Grid::resize` does: the same history
    /// sequence (texts, wrap flags, order, count), the same screen and the
    /// same reading position, over deep single-row history, random step
    /// schedules, a reader at the live bottom or scrolled back, and each
    /// [`Drag`]. Tier placement differs (the plain path migrates ring content
    /// into the store), so the unified `get_history_line` view is compared.
    /// Mid-flight the ring history must be in the job.
    #[test]
    fn offload_round_trip_matches_plain_resize_history_sequence() {
        let read = |g: &Grid| -> Vec<(String, bool)> {
            (0..g.scrollback_lines())
                .map(|i| {
                    g.get_history_line(i).map_or_else(
                        || (String::new(), false),
                        |line| (line.as_str().unwrap_or("").to_string(), line.is_wrapped()),
                    )
                })
                .collect()
        };
        let screen = |g: &Grid| -> Vec<String> {
            (0..g.rows())
                .map(|r| g.row_text(r).unwrap_or_default())
                .collect()
        };
        for drag in &DRAGS {
            for seed in [3u64, 11, 0xFEED] {
                // The reader: at the live bottom, then scrolled back by a
                // seeded 1..=12 rows.
                let backs: &[i32] = if drag.output == 0 {
                    &[0, 1 + (seed % 12) as i32]
                } else {
                    &[0]
                };
                for &back in backs {
                    let case = format!("{} / seed {seed} / back {back}", drag.label);
                    let mut offloaded = tiered_grid(drag.rows, drag.cols, drag.ring);
                    let mut plain = tiered_grid(drag.rows, drag.cols, drag.ring);
                    for g in [&mut offloaded, &mut plain] {
                        if drag.wrapping {
                            feed_wrapping_history(g, seed, 300, drag.cols);
                        } else {
                            feed_single_row_history(g, seed, 300, drag.fit);
                        }
                        g.scroll_display(back);
                    }
                    assert!(
                        offloaded.ring_buffer_scrollback() > 0,
                        "{case}: precondition — ring scrollback exists to ride the job"
                    );

                    let (first_rows, first_cols) = drag.steps[0];
                    let job = offloaded
                        .resize_offloading_scrollback_with_policy(
                            first_rows,
                            first_cols,
                            drag.policy,
                        )
                        .expect("width change with a tiered store detaches");
                    // Reach guard: the ring history rides the job. The grid's
                    // ring is not asserted empty: the visible-grid shrink may
                    // spill fresh overflow rows into it during the synchronous
                    // resize.
                    assert!(
                        !job.ring_lines.is_empty(),
                        "{case}: detach lifts the ring history into the job (RFL-1)"
                    );
                    plain.resize_with_policy(first_rows, first_cols, drag.policy);
                    for (step, &(rows, cols)) in drag.steps[1..].iter().enumerate() {
                        for g in [&mut offloaded, &mut plain] {
                            for i in 0..drag.output {
                                logical_line(g, &format!("O{step}-{i}"));
                            }
                        }
                        assert!(
                            drag.output == 0 || !offloaded.storage.lazy_buffer.is_empty(),
                            "{case}: precondition — the window staged its output"
                        );
                        offloaded.resize_with_policy(rows, cols, drag.policy);
                        plain.resize_with_policy(rows, cols, drag.policy);
                        offloaded.storage.lazy_buffer.assert_running_sums();
                    }
                    let mut rng = Rng(seed ^ 0xD1FF);
                    let mut next = Some(job);
                    while let Some(job) = next {
                        let (done, _steps) = step_with_schedule(job, &mut rng, 29);
                        next = offloaded.reattach_reflowed_scrollback_or_redetach(done);
                    }
                    assert!(
                        offloaded.ring_buffer_scrollback() > 0,
                        "{case}: re-attach returns ring history to the ring"
                    );

                    assert_eq!(
                        read(&offloaded),
                        read(&plain),
                        "{case}: offloaded history sequence != plain-resize sequence"
                    );
                    assert_eq!(
                        offloaded.display_offset(),
                        plain.display_offset(),
                        "{case}: the reader's place"
                    );
                    assert_eq!(screen(&offloaded), screen(&plain), "{case}: the screen");
                    offloaded.assert_invariants();
                    plain.assert_invariants();
                }
            }
        }
    }

    /// A materialize-everything reference rewrap: read all store+lazy input
    /// into a Vec, rewrap in one pass, `clear()` the store, and push every
    /// output line with the store's limits active. Takes an unstepped job.
    /// `ring_lines` are ignored on both sides (the streaming path routes them to
    /// `ring_out`), and so is a trailing run that continues into them, which
    /// the job hands to its ring phase whole.
    fn reference_materialize_all(job: PendingScrollbackReflow) -> ScrollbackStorage {
        let mut store = job.store;
        let mut input: Vec<Line> = Vec::with_capacity(store.line_count() + job.lazy_lines.len());
        for i in 0..store.line_count() {
            input.push(match store.get_line(i) {
                Ok(Some(line)) => line.into_owned(),
                Ok(None) | Err(_) => Line::new(),
            });
        }
        input.extend(job.lazy_lines);
        if job.ring_lines.first().is_some_and(Line::is_wrapped) {
            let head = input.iter().rposition(|line| !line.is_wrapped());
            input.truncate(head.unwrap_or(0));
        }
        let out = super::super::scrollback_reflow::reflow_scrollback_lines(&input, job.new_cols);
        store.clear().expect("reference clear");
        for line in out {
            store.push_line(line).expect("reference push");
        }
        store
    }

    /// Build the parity corpus: a deep tiered history plus a staged lazy tail
    /// (the `set_compress_offload_active` half), under the given retention
    /// limits. Shared by both parity oracles so they cannot drift.
    fn parity_grid(seed: u64, limit: Option<usize>, budget_bytes: usize) -> Grid {
        let (rows, cols) = (10u16, 40u16);
        let mut sb: ScrollbackStorage = Scrollback::new(64, 512, budget_bytes).into();
        sb.set_line_limit(limit);
        let mut g = Grid::with_tiered_scrollback(rows, cols, 8, sb);
        feed_mixed_history(&mut g, seed, 700, cols);
        g.set_compress_offload_active(true);
        feed_mixed_history(&mut g, seed ^ 0xABCD, 40, cols);
        g
    }

    /// The streaming rewrap (front-consuming, re-filling, limits lifted in
    /// flight) is content-identical to the materialize-everything reference,
    /// including a tight line limit (end-of-flight vs push-time enforcement), a
    /// staged lazy tail, and one-shot and chunked schedules.
    ///
    /// Under a tight byte budget the two evict to different depths; see
    /// `streaming_under_byte_budget_pressure_keeps_a_longer_suffix`.
    #[test]
    fn streaming_matches_materialize_all_reference() {
        for (limit, budget_bytes) in [
            (None, 8_000_000usize), // unconstrained
            (Some(120), 8_000_000), // tight total-line cap
        ] {
            for seed in [5u64, 21] {
                let (rows, new_cols) = (10u16, 23u16);
                let mut reference_grid = parity_grid(seed, limit, budget_bytes);
                let mut streamed_grid = parity_grid(seed, limit, budget_bytes);
                let mut chunked_grid = parity_grid(seed, limit, budget_bytes);

                let reference_store = reference_materialize_all(
                    reference_grid
                        .resize_offloading_scrollback(rows, new_cols)
                        .expect("reference job"),
                );
                let one_shot = streamed_grid
                    .resize_offloading_scrollback(rows, new_cols)
                    .expect("streaming job")
                    .reflow();
                let chunked_job = chunked_grid
                    .resize_offloading_scrollback(rows, new_cols)
                    .expect("chunked job");
                let mut rng = Rng(seed ^ 0x57EA);
                let (chunked, steps) = step_with_schedule(chunked_job, &mut rng, 41);

                assert!(steps > 4, "reach: the schedule actually chunked");
                assert_eq!(
                    store_fingerprint(&one_shot.store),
                    store_fingerprint(&reference_store),
                    "limit {limit:?} budget {budget_bytes} seed {seed}: streaming \
                     one-shot != materialize-all reference"
                );
                assert_eq!(
                    store_fingerprint(&chunked.store),
                    store_fingerprint(&reference_store),
                    "limit {limit:?} budget {budget_bytes} seed {seed}: streaming \
                     chunked != materialize-all reference"
                );
                // The limits round-tripped through the lift/restore.
                assert_eq!(one_shot.store.line_limit(), limit);
                assert_eq!(one_shot.store.memory_budget(), budget_bytes);
            }
        }
    }

    /// Under a tight byte budget the two shapes enforce at different moments
    /// (push-time with the newest output still hot, versus once after
    /// re-tiering), so they evict different numbers of the oldest lines. The
    /// difference must be depth only: every line the reference kept is present,
    /// in order, as the newest suffix of the streamed store; the streamed store
    /// never keeps fewer lines; and the divergence is real for at least one
    /// corpus.
    #[test]
    fn streaming_under_byte_budget_pressure_keeps_a_longer_suffix() {
        const BUDGET_BYTES: usize = 10_000;
        let (rows, new_cols) = (10u16, 23u16);
        let mut deeper_anywhere = false;

        for seed in [5u64, 21] {
            let mut reference_grid = parity_grid(seed, None, BUDGET_BYTES);
            let mut streamed_grid = parity_grid(seed, None, BUDGET_BYTES);
            let mut chunked_grid = parity_grid(seed, None, BUDGET_BYTES);

            let reference_store = reference_materialize_all(
                reference_grid
                    .resize_offloading_scrollback(rows, new_cols)
                    .expect("reference job"),
            );
            let one_shot = streamed_grid
                .resize_offloading_scrollback(rows, new_cols)
                .expect("streaming job")
                .reflow();
            let chunked_job = chunked_grid
                .resize_offloading_scrollback(rows, new_cols)
                .expect("chunked job");
            let mut rng = Rng(seed ^ 0x57EA);
            let (chunked, steps) = step_with_schedule(chunked_job, &mut rng, 41);
            assert!(steps > 4, "reach: the schedule actually chunked");

            let reference = store_fingerprint(&reference_store);
            // Reach: the budget really bit on BOTH sides — without eviction
            // the suffix relation below is the trivial equality case.
            assert!(
                reference_store.total_memory_used() <= BUDGET_BYTES,
                "seed {seed}: the reference must end under the byte budget"
            );
            assert!(
                one_shot.store.total_memory_used() <= BUDGET_BYTES,
                "seed {seed}: the streamed store must end under the byte budget"
            );

            for (name, store) in [("one-shot", &one_shot.store), ("chunked", &chunked.store)] {
                let streamed = store_fingerprint(store);
                assert!(
                    streamed.len() >= reference.len(),
                    "seed {seed} {name}: end-of-flight enforcement must never retain \
                     FEWER lines than push-time enforcement ({} < {})",
                    streamed.len(),
                    reference.len()
                );
                assert_eq!(
                    streamed[streamed.len() - reference.len()..],
                    reference[..],
                    "seed {seed} {name}: the historical shape's retained history must be \
                     the byte-identical NEWEST SUFFIX of the streamed history — only the \
                     eviction depth may differ"
                );
                deeper_anywhere |= streamed.len() > reference.len();
            }

            // The limits round-tripped through the lift/restore.
            assert_eq!(one_shot.store.line_limit(), None);
            assert_eq!(one_shot.store.memory_budget(), BUDGET_BYTES);
        }

        assert!(
            deeper_anywhere,
            "reach: the byte-budget corpus must actually evict to DIFFERENT depths — \
             that divergence is the boundary this test exists to pin"
        );
    }

    /// While streaming, the job's only growing uncompressed holding is the
    /// carry (one trailing logical run plus the current chunk), never the
    /// materialized history. The ring phase's `out` holds the rewrapped ring
    /// history, bounded by the ring capacity.
    #[test]
    fn streaming_step_carry_stays_bounded() {
        let (rows, cols) = (10u16, 30u16);
        let mut g = tiered_grid(rows, cols, 8);
        // Runs of at most ~5 rows at the old width → carry can never
        // legitimately exceed budget + one such run.
        let mut rng = Rng(9);
        for i in 0..1200 {
            let len = (rng.below(4) as usize) * cols as usize + rng.below(20) as usize;
            let mut s = format!("B{i}-");
            while s.len() < len {
                s.push('y');
            }
            logical_line(&mut g, &s);
        }
        let depth = g.scrollback().map_or(0, |s| s.line_count());
        assert!(depth > 800, "precondition: deep tiered input ({depth})");

        let budget = 64usize;
        let mut job = g.resize_offloading_scrollback(rows, 17).expect("job");
        let mut steps = 0usize;
        loop {
            steps += 1;
            assert!(steps < 100_000, "stepping must terminate");
            match job.reflow_step(budget) {
                ReflowStep::InProgress(next) => {
                    job = next;
                    let carry_len = match &job.phase {
                        ReflowPhase::Stream { carry, .. }
                        | ReflowPhase::RingRewrap { carry, .. } => carry.len(),
                    };
                    assert!(
                        carry_len <= budget + 8,
                        "carry must stay O(budget + one run), got {carry_len} at step {steps}"
                    );
                }
                ReflowStep::Done(done) => {
                    g.reattach_reflowed_scrollback(done);
                    break;
                }
            }
        }
        assert!(steps > 10, "reach: the budget actually chunked the input");
        assert!(g.scrollback_lines() > 400, "history preserved");
        g.assert_invariants();
    }

    /// A width change landing while a job is in flight (detaching nothing)
    /// would leave the re-attached store wrapped at a stale width; the
    /// converging re-attach detects the mismatch and re-detaches at the
    /// settled width, and one more driver pass leaves it wrapped at the
    /// current width with no further job.
    #[test]
    fn converging_reattach_rewraps_to_the_settled_width() {
        let (rows, w0, w1, w2) = (10u16, 80u16, 40u16, 60u16);
        let mut g = tiered_grid(rows, w0, 8);
        for i in 0..300 {
            let mut s = format!("C{i}-");
            while s.len() + 1 < w0 as usize {
                s.push('z');
            }
            logical_line(&mut g, &s); // near-full at 80: must re-wrap at 60
        }
        let job = g.resize_offloading_scrollback(rows, w1).expect("job");
        // Superseding width change mid-flight: throttled — detaches nothing.
        assert!(
            g.resize_offloading_scrollback(rows, w2).is_none(),
            "an in-flight reflow self-throttles a superseding resize"
        );

        let follow = g
            .reattach_reflowed_scrollback_or_redetach(job.reflow())
            .expect("width mismatch must re-detach for convergence");
        assert_eq!(
            follow.new_cols(),
            w2,
            "the follow-up job targets the settled width"
        );
        assert!(g.reflow_offload_in_flight(), "convergence window open");
        assert!(
            g.reattach_reflowed_scrollback_or_redetach(follow.reflow())
                .is_none(),
            "widths agree after exactly one extra pass"
        );
        assert!(!g.reflow_offload_in_flight());

        // The store is wrapped at the settled width now: the near-full 80-col
        // fill would exceed it if the stale result had been kept.
        let store = g.scrollback().expect("store re-attached");
        let max_len = (0..store.line_count())
            .filter_map(|i| {
                store
                    .get_line(i)
                    .ok()
                    .flatten()
                    .map(|line| line.to_string().chars().count())
            })
            .max()
            .unwrap_or(0);
        assert!(
            max_len <= usize::from(w2),
            "settled store must be wrapped at {w2} cols (max stored width {max_len})"
        );
        g.assert_invariants();
    }

    /// A line whose head scrolled past the ring (into lazy staging) while its
    /// continuation is still the ring's oldest row: the job rewraps it whole
    /// under either policy, leaving the history the synchronous resize leaves.
    /// Before 2026-10-11 a Native job re-split it at the seam, into
    /// `0123456789` and an orphan `ABCDE`.
    #[test]
    fn a_job_rewraps_a_line_straddling_the_store_ring_seam_whole() {
        for policy in [ResizePolicy::ConPty, ResizePolicy::Native] {
            // 3 visible rows over a 4-row ring: the 15-char line (2 rows at
            // 10 columns) and s0..s4 leave 5 history rows, so exactly its head
            // leaves the ring.
            let mut g = tiered_grid(3, 10, 4);
            let mut plain = tiered_grid(3, 10, 4);
            for g in [&mut g, &mut plain] {
                logical_line(g, "0123456789ABCDE");
                for i in 0..5 {
                    logical_line(g, &format!("s{i}"));
                }
            }
            assert_eq!(g.storage.ring_buffer_scrollback(), 4, "precondition");
            assert_eq!(g.scrollback_lines(), 5, "precondition: the head left");

            let pending = g
                .resize_offloading_scrollback_with_policy(3, 20, policy)
                .expect("a width change with a store offloads");
            g.reattach_reflowed_scrollback(pending.reflow());
            plain.resize_with_policy(3, 20, policy);
            let history = |g: &Grid| -> Vec<String> {
                (0..g.scrollback_lines())
                    .filter_map(|i| g.get_history_line(i).map(|l| l.to_string()))
                    .map(|s| s.trim_end().to_string())
                    .collect()
            };
            assert_eq!(
                history(&g).first().map(String::as_str),
                Some("0123456789ABCDE"),
                "{policy:?}: the straddling line, whole"
            );
            assert_eq!(history(&g), history(&plain), "{policy:?}");
            g.assert_invariants();
        }
    }
}
