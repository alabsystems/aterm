// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Off-thread width-change scrollback reflow (the L0 whole-Mac-freeze fix).
//!
//! [`Grid::resize`]'s width branch rewraps the ENTIRE off-screen scrollback
//! synchronously (`scrollback_reflow`), cost O(session history). On the GUI main
//! thread, under the per-session `term` mutex, that is the 42s freeze this
//! module exists to remove.
//!
//! The lever: a real session keeps its unbounded, disk-tiered history in the
//! tiered [`ScrollbackStorage`] (`storage.scrollback`), separate from the fixed
//! in-memory ring. [`Grid::resize_offloading_scrollback`] DETACHES that store in
//! O(1) *before* resizing — and (RFL-1) lifts the ring's scrollback rows into the
//! job as materialized [`Line`]s in the same breath — so the synchronous reflow
//! touches only the VISIBLE VIEWPORT, and hands the detached history back as a
//! `Send` [`PendingScrollbackReflow`].
//! The expensive decompress + rewrap ([`PendingScrollbackReflow::reflow`], or
//! the budgeted [`PendingScrollbackReflow::reflow_step`] increments for a
//! caller with no worker thread — the wasm cooperative offload) then runs off
//! the lock; the result is re-attached under a brief lock via
//! [`Grid::reattach_reflowed_scrollback`]. History is preserved (detached,
//! not dropped), and the main-thread cost becomes O(viewport) plus one O(ring)
//! row-to-[`Line`] materialization — the ring-sized REWRAP and the ring-sized
//! row REBUILD both left the synchronous phase (the rewrap to the worker, the
//! rebuild to the brief re-attach lock) — independent of session lifetime.
//!
//! MEMORY (RFL-2): the job's rewrap STREAMS. Input is read from the front of
//! the detached store and front-truncated out of it as it is consumed (whole
//! warm blocks / cold pages free in flight); completed logical runs are
//! rewrapped and immediately re-compressed onto the same store's back. Peak
//! transient usage is O(step budget + one logical run) uncompressed — never
//! the O(total-history-uncompressed) cliff of materializing every line before
//! refilling, which for the multi-million-line disk-tier histories this
//! offload exists for meant transiently inflating GBs of RAM per resize.
//!
//! Ordering: while a reflow is in flight the grid has no tiered store and no
//! ring scrollback (both ride the job), so new scroll-off accumulates in the
//! now-empty ring; re-attaching the rewrapped tiered store as
//! `storage.scrollback` and returning the rewrapped ring history to the ring
//! (or, where flight content already claimed its seat, to the store — see
//! [`Grid::reattach_ring_history`]) restores the correct
//! [old history | old ring history | window output | live ring] order. The "one detach, tiered = None until re-attach" invariant serialises
//! reflows: a resize that races an in-flight reflow detaches nothing and only
//! rewraps the ring, so the in-flight store is never lost or double-detached.
//!
//! Detach-window integrity (audited): output that scrolls off during the window is
//! kept via `scrollback_detached_for_reflow` (staged to the lazy buffer, flushed on
//! re-attach); a scrollback erase during the window is honored via
//! `scrollback_clear_gen` (the stale store is dropped, not resurrected); and the
//! reader's scroll position is carried through `prev_offset`. One ACCEPTED
//! TRANSIENT remains: a concurrent control-socket reader (`search`/`text`/`lines`)
//! during the window sees only the ring, not the detached tiered history, so it can
//! momentarily report not-found for text that is in off-screen scrollback. It
//! self-heals on re-attach (the `content_gen` bump invalidates the read cache); no
//! durable state persists the truncated view (there is no concurrent checkpoint
//! writer). Making the detached store readable mid-reflow would require a second
//! live snapshot — not worth the memory for a self-healing read anomaly.

use aterm_scrollback::{Line, ScrollbackStorage};

use super::Grid;
use super::state::PendingScrollbackSettings;
use crate::Damage;

/// The off-screen scrollback of a grid, detached and awaiting off-thread rewrap
/// to `new_cols`. `Send`, so the expensive decompress + rewrap runs on a worker
/// off the caller's lock. Produced by [`Grid::resize_offloading_scrollback`].
///
/// Off-screen history lives in THREE places, all extracted here without touching
/// the compressor/decompressor on the caller's thread: the tiered `store`
/// (compressed, oldest), `lazy_lines` (uncompressed staging, newer — these
/// would be silently discarded by a plain resize once the store is detached, see
/// `drain_lazy_buffer`), and `ring_lines` (the ring's scrollback rows
/// materialized as [`Line`]s, newest — RFL-1: pre-RFL-1 these were rewrapped AND
/// rebuilt synchronously under the caller's lock; now only the O(ring)
/// materialization is synchronous). Age order: `store`, then `lazy_lines`, then
/// `ring_lines`.
///
/// `[store | lazy_lines]` rewrap as one logical sequence back into the store;
/// `ring_lines` rewrap as a SEPARATE sequence (the ring phase) whose output
/// returns to the ring at re-attach — the same seam the pre-RFL-1 design had,
/// where the ring was rewrapped by the synchronous resize while the job
/// rewrapped the store, so a soft-wrapped run straddling the lazy/ring boundary
/// re-splits exactly as it always did.
///
/// Two ways to run the rewrap, identical results:
/// * [`reflow`](Self::reflow) — one shot, for a caller with a whole thread to
///   burn (the native worker).
/// * [`reflow_step`](Self::reflow_step) — budgeted increments, for a caller
///   that must keep every task short (the single-threaded wasm event loop).
///
/// Either way the job OWNS its data for the whole rewrap: stepping happens
/// outside the grid/term lock exactly like the one-shot, the grid never sees
/// partial progress, and re-attach still takes only a COMPLETED
/// [`ReflowedScrollback`] (partial re-attach is deliberately not a thing).
#[must_use = "a detached scrollback store must be reflowed and re-attached, or history is lost"]
pub struct PendingScrollbackReflow {
    store: ScrollbackStorage,
    lazy_lines: Vec<Line>,
    /// The ring's scrollback rows, materialized at detach (RFL-1). Rewrapped
    /// off-thread in [`ReflowPhase::RingRewrap`]; the result rides
    /// [`ReflowedScrollback::ring_out`] back to the ring. Dies with a dropped
    /// job exactly like the store (the abort path's bounded-loss semantics now
    /// cover the ring history too).
    ring_lines: Vec<Line>,
    new_cols: u16,
    /// The reader's scroll position at detach, restored on re-attach (audit bug D).
    prev_offset: usize,
    /// The scrollback-erase generation at detach; if it advanced by re-attach an
    /// erase happened during the window and the reflowed store is dropped (bug C).
    /// Captured once PER JOB at detach — stepping never re-samples it, so a
    /// chunked rewrap keeps exactly the one-shot's staleness semantics.
    clear_gen: u64,
    /// Incremental-rewrap progress (`reflow_step`). Starts at the beginning of
    /// the input; the one-shot [`reflow`](Self::reflow) is just "step until
    /// done" over this same state.
    phase: ReflowPhase,
    /// `Some` from the first step until completion: the store's own limits,
    /// out of the way of the streaming reads (see [`LiftedStoreLimits`]).
    lifted_limits: Option<LiftedStoreLimits>,
}

/// Where an incrementally-stepped rewrap currently is. Private: callers only
/// see [`ReflowStep`].
enum ReflowPhase {
    /// The streaming rewrap (RFL-2): read input from the FRONT of the store
    /// (front-truncating it as it is consumed), then from the newer
    /// `lazy_lines` tail; rewrap every COMPLETED logical line and push it
    /// straight onto the BACK of the SAME store (re-compressing immediately).
    /// The tier pipeline is a FIFO, so at any instant the store holds
    /// [unread input | rewrapped output] in age order and the indices
    /// `0..store_input_left` address exactly the input.
    Stream {
        /// Input lines still unread at the store's front.
        store_input_left: usize,
        /// Next `lazy_lines` index to read once the store input is consumed.
        next_lazy: usize,
        /// The wrap state carried across steps: the trailing logical line
        /// (a non-wrapped head plus its soft-wrapped continuations) whose run
        /// may continue into not-yet-read input, held back so a step boundary
        /// can NEVER split a logical line. Invariant: only `carry[0]` can be
        /// non-wrapped. This is the job's ONLY unbounded-ish uncompressed
        /// holding, and it is bounded by one logical run
        /// (`streaming_step_carry_stays_bounded`).
        carry: Vec<Line>,
    },
    /// Store streaming complete: rewrapping the job-carried ring history
    /// (`ring_lines`, RFL-1) as its own logical sequence — same budgeted
    /// carve, same never-split-a-run carry — into `out`, which becomes
    /// [`ReflowedScrollback::ring_out`]. Runs LAST because the ring history is
    /// newer than everything in the store and never enters the store's
    /// line-limit accounting.
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
/// the first [`PendingScrollbackReflow::reflow_step`] and restored — with one
/// final enforcement pass — by the step that finishes the store's output.
///
/// WHY: both limits enforce by evicting the store's OLDEST lines, which
/// mid-stream is the UNREAD INPUT at the store's front. An eviction there
/// would silently drop history the materialize-everything rewrap would have
/// kept. Lifting for the flight and enforcing once at the end is safe, and
/// for the LINE limit it lands on the identical final state: truncation keeps
/// the newest `limit` lines, exactly the survivors of push-time eviction
/// (`streaming_matches_materialize_all_reference` pins that, tight-limit
/// corpus included).
///
/// The BYTE budget is the honest exception, and it is a difference of
/// eviction DEPTH, never of content: push-time enforcement runs while the
/// newest output is still uncompressed in the hot tier, end-of-flight
/// enforcement runs once the whole result has been re-tiered, so the two stop
/// at "under budget" after evicting different numbers of the OLDEST lines.
/// Every line that survives is byte-identical and in order — the historical
/// shape's result is exactly the newest suffix of this one — and end-of-flight
/// enforcement never keeps FEWER lines (measured: 377 vs 277 retained at a
/// 10 KB budget, i.e. MORE history for the same bytes).
/// `streaming_under_byte_budget_pressure_keeps_a_longer_suffix` pins that
/// boundary in both directions.
///
/// Transient cost of the lift, stated plainly: while the budget is off the
/// output side does not evict, so a store that was already AT its byte budget
/// can end the flight over it (by the rewrap's own growth) until the
/// restoring step trims it. The per-tier `hot_limit` / `warm_limit`
/// promotions are NOT lifted, so hot+warm RAM stays bounded throughout and
/// the excess lives in the compressed cold tier (on disk for the disk-backed
/// store) — several orders below the O(total-history-UNCOMPRESSED) cliff this
/// design exists to delete.
struct LiftedStoreLimits {
    line_limit: Option<usize>,
    memory_budget: usize,
}

/// The outcome of one [`PendingScrollbackReflow::reflow_step`] call — a
/// consuming-iterator design so an exhausted job cannot be stepped again and a
/// half-stepped job cannot be re-attached: the ONLY way to a
/// [`ReflowedScrollback`] is through `Done`, i.e. a completed rewrap.
#[must_use = "keep stepping InProgress (or re-attach Done), or history is lost"]
pub enum ReflowStep {
    /// More work remains — call [`PendingScrollbackReflow::reflow_step`] again.
    /// Dropping this instead has the same bounded-loss semantics as dropping a
    /// never-started job: the detached history is lost with it, and the grid
    /// must be recovered via [`Grid::abort_reflow_offload`] (or superseded, as
    /// the wasm modules' newest-wins overwrite does).
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
    /// The ring history rewrapped to `new_cols` (RFL-1), returned to the ring
    /// (or the store, where flight content claimed its seat) by
    /// [`Grid::reattach_ring_history`].
    ring_out: Vec<Line>,
    new_cols: u16,
    prev_offset: usize,
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
    pub fn new_cols(&self) -> u16 {
        self.new_cols
    }

    /// The number of history lines to rewrap (for logging / progress) —
    /// tiered + lazy-staged + ring (RFL-1).
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.store.line_count() + self.lazy_lines.len() + self.ring_lines.len()
    }

    /// The EXPENSIVE step — run this OFF the caller's thread and lock. It
    /// decompresses every stored line, rewraps the whole history to `new_cols`
    /// (`reflow_scrollback_lines`, O(total cells)), and STREAMS the result
    /// back into the same store: input is front-truncated out as it is read,
    /// completed runs re-compress immediately onto the back (RFL-2), so even
    /// this one-shot holds only O(one logical run) of history uncompressed at
    /// a time. The store keeps its identity and tier configuration; its
    /// `line_limit` / memory budget are lifted for the flight and re-enforced
    /// when the store's output completes ([`LiftedStoreLimits`]). This is the
    /// work that, run synchronously on the main thread, froze the Mac.
    ///
    /// Exactly [`reflow_step`](Self::reflow_step) with an unlimited budget,
    /// driven until `Done` — the one-shot and every chunked schedule produce
    /// the identical [`ReflowedScrollback`].
    // Cost: O(session history) in ONE call, honestly — the whole job. Callers
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

    /// Advance the rewrap by a BOUNDED increment: read and rewrap up to
    /// `max_lines` INPUT history lines (a `max_lines` of 0 is treated as 1 so a
    /// step always makes progress), then yield. Call again on
    /// [`ReflowStep::InProgress`]; [`ReflowStep::Done`] carries the completed
    /// [`ReflowedScrollback`] — byte-identical to the one-shot
    /// [`reflow`](Self::reflow) for ANY schedule of budgets (the
    /// `reflow_step_any_schedule_matches_one_shot` property).
    ///
    /// Chunks are carved ONLY at unwrapped-logical-line boundaries: a logical
    /// line (a row plus its soft-wrapped continuations) is never split across
    /// steps — its trailing, possibly-still-continuing run is carried to the
    /// next step instead (a wrong carve here would corrupt scrollback content,
    /// the exact thing the offload protocol's NoLoss/ErasedStaysErased
    /// obligations exist to prevent).
    ///
    /// Safety invariants while stepping (all unchanged from the one-shot):
    /// the job owns its data — the store is detached, so stepping runs outside
    /// the term lock and the grid never observes partial progress; re-attach
    /// still takes only a COMPLETED result (partial-progress re-attach is
    /// explicitly out of scope); the `clear_gen` staleness capture is per-JOB,
    /// taken at detach, never re-sampled per step; and dropping a half-stepped
    /// job has the same bounded-loss semantics as dropping the one-shot job
    /// (the detached history goes with it — recover the grid via
    /// [`Grid::abort_reflow_offload`]).
    // COST: O(max_lines × cols) per step — ≤ max_lines input-line reads
    // (tiered decompress, lazy take, or ring-line move), plus rewrap + push
    // (re-compress) of the completed logical lines among them. MEMORY:
    // O(max_lines + one logical run) uncompressed — input is front-truncated
    // out of the store as it is consumed and output re-compresses immediately
    // (RFL-2), so the job never holds the history materialized. NOT UNBOUNDED
    // in the census sense — the caller's budget bounds it — with two honest
    // caveats: (1) a logical line is never split, so the single step that
    // completes a soft-wrapped run longer than max_lines rewraps that whole
    // run at once (runs are capped at MAX_LOGICAL_WIDTH accumulated display
    // cells); (2) a step's front truncation may land mid-block, so the next
    // step re-decodes at most one warm/cold block — bounded, amortized by the
    // block caches.
    pub fn reflow_step(mut self, max_lines: usize) -> ReflowStep {
        let budget = max_lines.max(1);
        // First step of the STREAM phase: move the store's own eviction limits
        // out of the way for the flight (see `LiftedStoreLimits`); the step
        // that finishes the store's output restores and enforces them.
        //
        // The gate is the PHASE, not `is_none()` alone: the store phase clears
        // `lifted_limits` when it restores them, and the ring phase (RFL-1)
        // runs afterwards — an `is_none()`-only guard would re-lift on the
        // first ring step and, with no second restore to follow, hand back a
        // store whose line limit and byte budget were gone for good.
        // `streaming_matches_materialize_all_reference`'s limit round-trip
        // assertions are the regression guard.
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
                let appended_from = carry.len();
                carry.reserve(read);
                let from_store = read.min(*store_input_left);
                for i in 0..from_store {
                    // Tiered store lines first (older), decompressed here off
                    // the lock. The store's FRONT is the oldest unread input;
                    // output pushed by earlier steps lives BEHIND the input
                    // region (the tier pipeline is a FIFO), so
                    // `0..store_input_left` addresses exactly the input.
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
                    // Release the consumed input NOW — the memory lever of the
                    // streaming design (RFL-2): whole warm blocks / cold pages
                    // free as the front offset crosses them, so the store never
                    // holds [full input | full output] at once and the job
                    // never holds the input materialized.
                    if let Err(error) = self.store.truncate_oldest(from_store) {
                        aterm_log::warn!(
                            "offload reflow: input front-truncation failed: {error}"
                        );
                    }
                    *store_input_left -= from_store;
                }
                for _ in from_store..read {
                    // Then the (newer) lazy-staged lines that had not yet been
                    // compressed (blank left behind; freed when the input
                    // exhausts).
                    carry.push(std::mem::replace(
                        &mut self.lazy_lines[*next_lazy],
                        Line::new(),
                    ));
                    *next_lazy += 1;
                }

                if read == total_left {
                    // Input exhausted: every carried run is complete by
                    // definition — rewrap the remainder and push it straight
                    // onto the store.
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
                    // Restore the store's own limits, in the enforcing
                    // direction: the line limit first (front-truncates to the
                    // newest `limit` lines — the same survivors push-time
                    // eviction leaves), then the byte budget (re-tiers and, if
                    // still over, FIFO-evicts from the front of the finished
                    // output). Only now is enforcement safe: mid-flight it
                    // would have evicted the store's FRONT, i.e. unread INPUT.
                    // The ring phase never touches the store, so restoring
                    // here — not at Done — keeps eviction ordering exact.
                    if let Some(limits) = self.lifted_limits.take() {
                        self.store.set_line_limit(limits.line_limit);
                        if let Err(error) = self.store.set_memory_budget(limits.memory_budget) {
                            aterm_log::warn!(
                                "offload reflow: restoring the memory budget failed: {error}"
                            );
                        }
                    }
                    // The ring history (RFL-1) rewraps next, as its own
                    // budgeted phase.
                    self.phase = ReflowPhase::RingRewrap {
                        next_input: 0,
                        carry: Vec::new(),
                        out: Vec::new(),
                    };
                } else {
                    // Carve at the LAST unwrapped-logical-line boundary among
                    // the newly read lines (older carry is all continuations
                    // of its head by the `carry` invariant): completed runs
                    // are rewrapped and re-compressed onto the store NOW; the
                    // trailing run stays carried. No boundary in this chunk:
                    // the whole chunk continues the carried run — keep
                    // carrying (never split a logical line).
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
                for line in &mut self.ring_lines[*next_input..end] {
                    // Take by replace (blank left behind, freed below): the
                    // ring lines are already materialized, so a "read" here is
                    // a move, not a decompress.
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

/// Carve `carry` at the LAST unwrapped-logical-line boundary among the lines
/// appended this step (older carry is all continuations of its head by the
/// `carry` invariant, so scanning the appended region is exhaustive):
/// everything before the boundary is completed runs — rewrapped into `out` —
/// while the trailing, possibly-still-continuing run stays carried. No boundary
/// in the appended region means the whole chunk continues the carried run:
/// keep carrying (never split a logical line). Used by the ring phase; the
/// store phase streams its completed runs straight into the store instead of
/// accumulating them (RFL-2), so its carve is inlined there.
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
    pub fn new_cols(&self) -> u16 {
        self.new_cols
    }

    /// The number of rewrapped history lines ready to re-attach (store +
    /// ring history, RFL-1).
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.store.line_count() + self.ring_out.len()
    }
}

impl Grid {
    /// Resize like [`Grid::resize`], but move the unbounded tiered off-screen
    /// scrollback OFF the synchronous path.
    ///
    /// On a WIDTH change with reflow enabled and a tiered store attached: detach
    /// the tiered store (O(1)) and lift the ring's scrollback rows into the job
    /// (O(ring) materialization, RFL-1) BEFORE resizing, so the synchronous
    /// reflow inside [`Grid::resize`] finds no off-screen history at all and
    /// costs O(viewport), then return the detached history as a `Send`
    /// [`PendingScrollbackReflow`]. The caller must
    /// [`reflow`](PendingScrollbackReflow::reflow) it off-thread and
    /// [`reattach`](Grid::reattach_reflowed_scrollback) the result, or history is
    /// lost.
    ///
    /// Returns `None` (and is exactly [`Grid::resize`]) when the width is
    /// unchanged, reflow is disabled, or no tiered store is attached — in those
    /// cases the resize is already bounded by the viewport.
    pub fn resize_offloading_scrollback(
        &mut self,
        new_rows: u16,
        new_cols: u16,
    ) -> Option<PendingScrollbackReflow> {
        let old_cols = self.storage.cols;
        // Only a width change rewraps scrollback, and only a tiered grid has the
        // unbounded off-screen history worth offloading. Otherwise the plain
        // resize is already O(viewport).
        if new_cols == old_cols || self.storage.scrollback.is_none() {
            self.resize(new_rows, new_cols);
            return None;
        }

        // Extract the off-screen history cheaply, WITHOUT (de)compressing on this
        // thread — all three layers, in age order:
        //  1. lazy_lines: uncompressed staged lines (mid-age). Must be taken BEFORE
        //     detaching the store, else the resize's `drain_lazy_buffer` discards
        //     them (it drops staged lines when no store is attached).
        //  2. store: the compressed tiered/disk history (oldest), detached O(1).
        //  3. ring_lines: the ring's scrollback rows (newest), materialized below
        //     — the ONLY ring-sized synchronous cost left (RFL-1): the rewrap and
        //     the row rebuild ride the job instead of running under this lock.
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
        // RFL-1: lift the ring history into the job too. Materializing rows →
        // Lines is the whole remaining synchronous ring cost — no rewrap, no
        // row rebuild. The ring is left EMPTY, so the resize below finds
        // nothing off-screen and its synchronous rewrap count is ZERO
        // (`offloaded_resize_synchronous_rewrap_is_zero` pins this).
        let ring_lines = self.take_ring_scrollback_lines();

        // With the store detached, the lazy buffer emptied and the ring history
        // lifted into the job, the resize's `take_scrollback_lines` rewraps
        // NOTHING — the synchronous cost is the visible-grid reflow,
        // O(viewport), the budget the bounded-cost obligation checks.
        self.resize(new_rows, new_cols);

        // Enter the reflow window AFTER resize (so the resize itself runs exactly as
        // the plain path): from here until re-attach, scroll-off keeps being staged
        // to the lazy buffer instead of dropped (audit bug B).
        self.storage.scrollback_detached_for_reflow = true;
        self.storage.pending_scrollback_settings = Some(pending_settings);

        let phase = ReflowPhase::start(&store);
        Some(PendingScrollbackReflow {
            store,
            lazy_lines,
            ring_lines,
            new_cols,
            prev_offset,
            clear_gen,
            phase,
            lifted_limits: None,
        })
    }

    /// Re-attach an off-thread-rewrapped scrollback store as the (oldest) tiered
    /// history, after [`resize_offloading_scrollback`](Self::resize_offloading_scrollback)
    /// detached it and a worker [`reflow`](PendingScrollbackReflow::reflow)ed it.
    ///
    /// If a tiered store already exists (e.g. a terminal reset raced the reflow),
    /// the rewrapped history is dropped rather than clobber current state. If the
    /// grid width changed again while the reflow ran, the store is still attached
    /// — its content is valid; the stale wrapping self-heals on the next width
    /// change (which re-detaches and re-reflows it).
    ///
    /// Shipping drivers use
    /// [`reattach_reflowed_scrollback_or_redetach`](Self::reattach_reflowed_scrollback_or_redetach)
    /// (RFL-3), which turns that "next width change" self-heal into an
    /// immediate convergence pass; this non-converging entry point keeps the
    /// exact one-window semantics and remains the Tier-1 conformance binding
    /// (the spec models one detach window at a time).
    pub fn reattach_reflowed_scrollback(&mut self, reflowed: ReflowedScrollback) {
        // The reflow window is over: stop staging scroll-off as "detached" — the
        // store (old or freshly attached below) is authoritative again.
        self.storage.scrollback_detached_for_reflow = false;

        if self.storage.scrollback.is_some() {
            // A terminal reset re-created the tiered store during the reflow; don't
            // clobber it (would corrupt history ordering). The reflowed store drops.
            // The job-carried ring history is NOT part of the store the
            // replacement supersedes — it re-enters the ring (capped), the same
            // seat the pre-RFL-1 design kept it in across a replacement race.
            self.reconcile_pending_scrollback_settings();
            self.reattach_ring_history(reflowed.ring_out, reflowed.new_cols);
            return;
        }

        // Audit bug C: scrollback was ERASED (ED3 / `clear` / reset) during the
        // window. Do NOT resurrect the pre-erase history — attach an EMPTY store and
        // keep only the post-erase output the ring/lazy captured after the clear.
        // The job-carried ring history predates the erase too: `ring_out` drops
        // with the store content (ErasedStaysErased — pre-RFL-1 the erase wiped
        // those rows in the ring; in flight they die here, same outcome).
        if reflowed.clear_gen != self.storage.scrollback_clear_gen {
            let mut store = reflowed.store;
            let _ = store.clear();
            self.storage.scrollback = Some(store);
            self.reconcile_pending_scrollback_settings();
            self.drain_lazy_buffer(); // post-erase window output (erase cleared the pre-erase lazy)
            self.storage.display_offset = 0; // an erase resets the scroll position
            self.storage.damage = Damage::Full;
            self.storage.content_gen += 1;
            return;
        }

        self.storage.scrollback = Some(reflowed.store);
        self.reconcile_pending_scrollback_settings();
        // RFL-1: the ring history returns BEFORE the lazy drain below, so the
        // final order is [old history | old ring history | window output | live
        // ring] — see `reattach_ring_history` for the seat arithmetic.
        self.reattach_ring_history(reflowed.ring_out, reflowed.new_cols);
        // Audit bug B: flush the lines that scrolled off during the window (staged in
        // the lazy buffer) into the store AFTER the reflowed old history — yielding
        // the documented order [old history | window output | live ring].
        self.drain_lazy_buffer();
        let sb = self.storage.scrollback_lines();
        // Audit bug D: restore the reader's pre-detach scroll position clamped to the
        // regrown full history (the synchronous resize had clamped it to the ring-only
        // count while the store was detached). EXCEPT when the reader followed output
        // to the live bottom during the window (display_offset collapsed to 0, e.g.
        // pressed End to watch streaming output) — honor that instead of yanking the
        // viewport back up to the stale deep position (audit #7).
        if !(self.storage.display_offset == 0 && reflowed.prev_offset > 0) {
            self.storage.display_offset = reflowed.prev_offset.min(sb);
        }
        self.storage.damage = Damage::Full;
        self.storage.content_gen += 1;
    }

    /// Re-attach, then CONVERGE (RFL-3): if the grid's width changed while the
    /// reflow ran (a superseding drag step — the one-detach-in-flight throttle
    /// meant it detached nothing at the time), the store that just re-attached
    /// is wrapped at the job's STALE width (its ring history included — the
    /// mismatch case routes it into the store, see `reattach_ring_history`).
    /// Instead of parking that staleness until "the next width change" — which
    /// after a settled drag never comes — immediately re-detach the store for
    /// one more off-thread rewrap at the CURRENT width and hand the job to the
    /// caller's existing worker/pump loop.
    ///
    /// Termination: each returned job carries the width observed at ITS
    /// detach, so once the width stops moving at most ONE extra pass runs. The
    /// zero-data-loss supersede semantics are unchanged — nothing is
    /// cancelled, content re-attaches before every re-detach. The
    /// erase-during-window and replacement-store races re-attach exactly as
    /// [`reattach_reflowed_scrollback`](Self::reattach_reflowed_scrollback)
    /// and never re-detach: an erased store is empty (nothing is mis-wrapped)
    /// and a replacement store was never rewrapped by this job at all. The
    /// follow-up job carries no `ring_lines`: the superseding resize already
    /// rewrapped the live ring synchronously at the settled width.
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
        // Defensive: the normal-path re-attach above always installs a store;
        // without one there is nothing to converge on. Written as `?` on the
        // borrow rather than an `if`/`return None` — `clippy::question_mark`
        // rejects the long form, and the crate denies `clippy::all`.
        self.storage.scrollback.as_ref()?;

        // Width mismatch on a normal re-attach: open one more detach window at
        // the settled width. Same capture set as
        // `resize_offloading_scrollback`, minus the resize — the grid is
        // already AT the target geometry (the superseding resize ran it) and
        // its ring was already rewrapped synchronously by that same resize.
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
        self.storage.scrollback_detached_for_reflow = true;
        self.storage.pending_scrollback_settings = Some(pending_settings);
        let phase = ReflowPhase::start(&store);
        Some(PendingScrollbackReflow {
            store,
            lazy_lines,
            ring_lines: Vec::new(),
            new_cols: self.storage.cols,
            prev_offset,
            clear_gen,
            phase,
            lifted_limits: None,
        })
    }

    /// Return the job-carried ring history (already rewrapped to `new_cols`,
    /// RFL-1) to the grid at re-attach.
    ///
    /// Seat arithmetic: the newest lines re-enter the ring as scrollback rows
    /// ahead of any flight scroll-off (the pre-RFL-1 placement), via
    /// `prepend_ring_scrollback_lines` — the O(ring × cols) row rebuild that
    /// used to run synchronously under the resize now runs here, under the
    /// brief re-attach lock on the worker's thread. Lines that no longer fit —
    /// rewrap growth, or a ring that partially refilled during the flight — are
    /// pushed into the tiered store BEFORE the caller drains the staged window
    /// output, preserving [old store history | old ring history | window
    /// output | live ring]; that is exactly where the pre-RFL-1 design's
    /// over-cap staging landed them too. When staged window output exists (the
    /// ring filled AND spilled during the flight, or a mid-window height
    /// shrink staged rows), the WHOLE ring history takes the store route: the
    /// ring's current rows and the staged spill are both newer, so the front
    /// of the store is the only order-correct seat left. The store route is
    /// also taken when the job's width is STALE (a superseding resize landed
    /// mid-flight): ring rows can only be rebuilt at the grid's CURRENT
    /// width — a narrower rebuild would truncate the stale-wrapped lines —
    /// while the store already tolerates over-wide stale lines (its own
    /// content is equally stale in that race, and the next width change
    /// re-detaches and rewraps them together).
    ///
    /// Retention note (honest): with the store at its line limit, lines routed
    /// to the store are subject to its push-time eviction exactly like the
    /// pre-RFL-1 staging route was; only the rare all-to-store case (flight
    /// spill) can evict a few lines the old design would have briefly kept in
    /// the ring — bounded by the ring cap, self-healing as output arrives.
    fn reattach_ring_history(&mut self, mut ring_out: Vec<Line>, new_cols: u16) {
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
                // No store to route into (unreachable from the normal re-attach,
                // which just attached one): the ring cap is the retention bound —
                // oldest evicted, the configured ring-only behavior.
                drop(ring_out.drain(..skip));
            }
        }
        if !ring_out.is_empty() {
            self.prepend_ring_scrollback_lines(ring_out, new_cols);
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

    /// True while a detach window is open: the tiered store is out for an
    /// off-thread reflow and scroll-off is being staged for re-attach. MUST be
    /// false again after [`reattach_reflowed_scrollback`](Self::reattach_reflowed_scrollback)
    /// or [`abort_reflow_offload`](Self::abort_reflow_offload) — a `true` that
    /// outlives them is the wedge (un-drainable lazy growth) the abort path exists
    /// to prevent. Projected as the spec's `detached` variable by the Tier-1
    /// conformance binding (`tests/conformance_offload.rs`), which is what makes a
    /// wedged flag a RED test instead of a silent leak (mutation-proven).
    #[must_use]
    pub fn reflow_offload_in_flight(&self) -> bool {
        self.storage.scrollback_detached_for_reflow
    }

    /// Recover from a reflow that will NEVER re-attach — the worker panicked or the
    /// thread died mid-rewrap. The detached tiered history — and, since RFL-1,
    /// the ring history riding the same job — is unrecoverable (it was
    /// owned by the lost [`PendingScrollbackReflow`]); the job here is to return the
    /// grid to a BOUNDED state rather than leave it wedged. Without this,
    /// `scrollback_detached_for_reflow` stays `true` for the rest of the session, so
    /// every future scroll-off stages into an un-drainable `lazy_buffer` (unbounded
    /// growth) and all tiered history stays invisible (audit #5). No-op if not
    /// mid-window (already re-attached, or a reset re-created the store).
    ///
    /// The wedge guard — on EVERY exit, including the no-op early return, the
    /// detach window is closed.
    ///
    /// This was stated as a compiler obligation (`ensures
    /// !self.storage.scrollback_detached_for_reflow`, first measured provable
    /// on stage2 51bf8a270, 2026-08-05). The clause is WITHDRAWN for now
    /// because it ICEs the toolchain rather than proving anything:
    /// `trustc 1.99.0-dev (6fbfab9f8)` panics with `trimmed_def_paths called,
    /// diagnostics were expected but none were emitted` (rustc_errors/src/
    /// lib.rs:478, in `DiagCtxtInner::drop`) whenever this crate is compiled
    /// under `-Ztrust-verify=off` — which is the workspace-wide setting at
    /// `.cargo/config.toml:35`, i.e. every ordinary build. Isolated to this one
    /// clause by single-variable bisect: removing it compiles the crate clean,
    /// restoring it panics, all else held equal. It also cannot survive the
    /// public snapshot, whose `rust-toolchain.toml` is swapped to stock 1.97.1
    /// by `publish/transforms.sh:81` and cannot PARSE `ensures` at all (`#[cfg]`
    /// strips after parsing, so gating would not have helped).
    ///
    /// The obligation is kept as debug assertions on both exits until the
    /// toolchain can carry it. RESTORE THE CLAUSE once trustc stops ICEing with
    /// verification off and the public lane can parse contracts.
    pub fn abort_reflow_offload(&mut self) {
        if !self.storage.scrollback_detached_for_reflow {
            debug_assert!(
                !self.storage.scrollback_detached_for_reflow,
                "abort_reflow_offload must close the detach window on every exit"
            );
            return;
        }
        self.storage.scrollback_detached_for_reflow = false;
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

    /// Deep fingerprint of a COMPLETED rewrap: the store's decoded content
    /// plus the job-carried ring history (RFL-1) — the whole result surface
    /// any step schedule must reproduce byte-for-byte.
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
    /// `cargo test -p aterm-grid --release --lib reflow_step_timing -- --ignored --nocapture`
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

    /// A logical line is NEVER split mid-run: with the most adversarial
    /// schedule (budget 1 — every step reads ONE input line) a soft-wrapped run
    /// many rows long crosses step boundaries intact, matches the one-shot
    /// byte-for-byte, and its text round-trips through the re-attached grid.
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

    /// Dropping a HALF-STEPPED job keeps the one-shot's bounded-loss semantics:
    /// the detached history is gone with the job (the grid NEVER sees partial
    /// progress — re-attach only accepts a completed [`ReflowedScrollback`]),
    /// and [`Grid::abort_reflow_offload`] returns the grid to a bounded,
    /// non-wedged state — the same recovery as a dead one-shot worker.
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

    /// Output that scrolls off BETWEEN steps stages exactly like it does under
    /// the one-shot window (audit bug B) and lands AFTER the rewrapped old
    /// history on re-attach — the documented [old history | window output]
    /// order, unchanged by stepping.
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

    /// Single-row logical lines only: blanks, short ASCII, medium ASCII, and
    /// wide (CJK) lines capped under one row. Deliberately NO multi-row
    /// soft-wrapped runs: a run straddling the store/ring detach seam re-splits
    /// differently on the offload path than on the plain path (a PRE-EXISTING
    /// seam — the pre-RFL-1 design rewrapped the ring separately too), and this
    /// corpus isolates the RFL-1 claim (placement/order identity) from it.
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

    /// RFL-1 DIFFERENTIAL ORACLE: with the ring history riding the job, the
    /// offloaded round trip must still produce EXACTLY the history sequence the
    /// plain synchronous `Grid::resize` produces — same texts, same wrap
    /// flags, same order, same count — over deep single-row synthetic history
    /// and several random step schedules. Tier PLACEMENT legitimately differs
    /// (the plain path migrates ring content into the store; the offload
    /// returns it to the ring), so the comparison reads the unified
    /// `get_history_line` view, not the tiers. Mid-flight the ring must be
    /// EMPTY — that is the synchronous cost this design removes.
    #[test]
    fn offload_round_trip_matches_plain_resize_history_sequence() {
        for seed in [3u64, 11, 0xFEED] {
            let (rows, cols, new_cols) = (10u16, 40u16, 23u16);
            let mut offloaded = tiered_grid(rows, cols, 32);
            let mut plain = tiered_grid(rows, cols, 32);
            for g in [&mut offloaded, &mut plain] {
                feed_single_row_history(g, seed, 300, cols);
            }
            assert!(
                offloaded.ring_buffer_scrollback() > 0,
                "seed {seed}: precondition — ring scrollback exists to ride the job"
            );

            let job = offloaded
                .resize_offloading_scrollback(rows, new_cols)
                .expect("width change with a tiered store detaches");
            // Reach guard (two-sided with the precondition above): the ring
            // history really rides the job. The grid's ring is NOT asserted
            // empty here — the visible-grid shrink legitimately spills fresh
            // overflow rows into the ring during the synchronous resize.
            assert!(
                !job.ring_lines.is_empty(),
                "seed {seed}: detach lifts the ring history into the job (RFL-1)"
            );
            let mut rng = Rng(seed ^ 0xD1FF);
            let (done, _steps) = step_with_schedule(job, &mut rng, 29);
            offloaded.reattach_reflowed_scrollback(done);
            assert!(
                offloaded.ring_buffer_scrollback() > 0,
                "seed {seed}: re-attach returns ring history to the ring"
            );

            plain.resize(rows, new_cols);

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
            assert_eq!(
                read(&offloaded),
                read(&plain),
                "seed {seed}: offloaded history sequence != plain-resize sequence"
            );
            offloaded.assert_invariants();
            plain.assert_invariants();
        }
    }

    /// The HISTORICAL materialize-everything rewrap, reproduced verbatim as
    /// the RFL-2 reference: read ALL store+lazy input into a Vec, rewrap in
    /// one pass, `clear()` the store, push every output line with the store's
    /// own limits active throughout. Takes an UNSTEPPED job (its limits are
    /// still the store's own). The job's `ring_lines` are ignored on both
    /// sides — the streaming path routes them to `ring_out`, never the store.
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

    /// RFL-2 PARITY ORACLE: the streaming rewrap (front-consuming,
    /// immediately re-filling, limits lifted in flight) must be
    /// content-identical to the historical materialize-everything shape.
    /// Corpora include a TIGHT line limit so the end-of-flight re-enforcement
    /// is compared against push-time enforcement — the exact seam the limit
    /// lift could get wrong — plus a staged lazy tail, one-shot AND chunked
    /// schedules.
    ///
    /// The TIGHT-BYTE-BUDGET regime is the one place the two shapes are NOT
    /// bit-identical (they evict to different depths, same content); it has
    /// its own oracle, `streaming_under_byte_budget_pressure_keeps_a_longer_suffix`.
    #[test]
    fn streaming_matches_materialize_all_reference() {
        for (limit, budget_bytes) in [
            (None, 8_000_000usize),  // unconstrained
            (Some(120), 8_000_000),  // tight total-line cap
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

    /// RFL-2, the honest BOUNDARY of the parity claim. With a tight byte
    /// budget the two shapes enforce it at different moments — push-time,
    /// while the newest output is still uncompressed in the hot tier, versus
    /// once at the end, when the whole result has been re-tiered — so they
    /// stop at "under budget" after evicting different numbers of the OLDEST
    /// lines.
    ///
    /// What must hold, and what this pins in BOTH directions, is that the
    /// difference is eviction DEPTH only: every line the historical shape kept
    /// is present byte-for-byte, in order, as the NEWEST SUFFIX of the
    /// streamed store; the streamed store never keeps FEWER lines; and the
    /// divergence is real for at least one corpus (otherwise this test would
    /// silently degrade into a duplicate of the exact-parity oracle above).
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

            for (name, store) in [
                ("one-shot", &one_shot.store),
                ("chunked", &chunked.store),
            ] {
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

    /// RFL-2 MEMORY GUARD: while streaming, the job's only growing
    /// uncompressed holding is the carry — one trailing logical run plus the
    /// current chunk — never the materialized history. The old shape held
    /// EVERY input line in `carry`/`out` at once (peak O(total uncompressed));
    /// this pins the new bound so a refactor cannot quietly reintroduce the
    /// cliff. (The ring phase's `out` holds the rewrapped ring history — that
    /// is bounded by the ring capacity, a construction constant, by design.)
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

    /// RFL-3: a width change that lands while a job is in flight (detaching
    /// nothing, by the one-in-flight throttle) used to leave the re-attached
    /// store wrapped at the job's stale width until "the next width change" —
    /// which after a settled drag never comes. The converging re-attach closes
    /// the loop: attach, detect the mismatch, re-detach at the settled width;
    /// one more driver pass leaves the store genuinely wrapped at the CURRENT
    /// width and returns no further job.
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

        // The store's content is genuinely wrapped at the settled width now —
        // the near-full 80-col fill would exceed it if the stale result had
        // been parked (the pre-RFL-3 behavior).
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
}
