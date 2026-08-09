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
//! O(1) *before* resizing, so the synchronous reflow touches only the bounded
//! ring, and hands the detached store back as a `Send` [`PendingScrollbackReflow`].
//! The expensive decompress + rewrap ([`PendingScrollbackReflow::reflow`], or
//! the budgeted [`PendingScrollbackReflow::reflow_step`] increments for a
//! caller with no worker thread — the wasm cooperative offload) then runs off
//! the lock; the result is re-attached under a brief lock via
//! [`Grid::reattach_reflowed_scrollback`]. History is preserved (detached,
//! not dropped), and the main-thread cost becomes O(ring), independent of
//! session lifetime.
//!
//! Ordering: while a reflow is in flight the grid has no tiered store, so new
//! scroll-off accumulates in the (newer) ring; re-attaching the rewrapped tiered
//! store as `storage.scrollback` restores the correct [old history | new output]
//! order. The "one detach, tiered = None until re-attach" invariant serialises
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
/// Off-screen history lives in TWO places, both extracted here without touching
/// the compressor/decompressor on the caller's thread: the tiered `store`
/// (compressed, older) and `lazy_lines` (uncompressed staging, newer — these
/// would be silently discarded by a plain resize once the store is detached, see
/// `drain_lazy_buffer`). `lazy_lines` are newer than `store`'s lines.
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
}

/// Where an incrementally-stepped rewrap currently is. Private: callers only
/// see [`ReflowStep`].
enum ReflowPhase {
    /// Reading input lines (the tiered store's, oldest first, then the newer
    /// `lazy_lines` tail) and rewrapping every COMPLETED logical line.
    Rewrap {
        /// Next input line to read: `0..store.line_count()` index the store,
        /// the rest index `lazy_lines` (input order == age order).
        next_input: usize,
        /// The wrap state carried across steps: the trailing logical line
        /// (a non-wrapped head plus its soft-wrapped continuations) whose run
        /// may continue into not-yet-read input, held back so a step boundary
        /// can NEVER split a logical line. Invariant: only `carry[0]` can be
        /// non-wrapped.
        carry: Vec<Line>,
        /// Rewrapped output accumulated so far (completed logical lines only).
        out: Vec<Line>,
    },
    /// All input consumed and the store cleared: pushing the rewrapped lines
    /// back into the same store (re-compressing), `next_out` lines done.
    Refill { out: Vec<Line>, next_out: usize },
}

impl ReflowPhase {
    /// The state every job starts in (nothing read, nothing rewrapped).
    fn start() -> Self {
        ReflowPhase::Rewrap {
            next_input: 0,
            carry: Vec::new(),
            out: Vec::new(),
        }
    }
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

    /// The number of history lines to rewrap (for logging / progress).
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.store.line_count() + self.lazy_lines.len()
    }

    /// The EXPENSIVE step — run this OFF the caller's thread and lock. It
    /// decompresses every stored line, rewraps the whole history to `new_cols`
    /// (`reflow_scrollback_lines`, O(total cells)), and re-fills the same store
    /// (re-compressing), preserving its `line_limit` / memory budget / tier
    /// configuration. This is the work that, run synchronously on the main
    /// thread, froze the Mac.
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
    // (tiered decompress or lazy take) or output-line pushes (re-compress),
    // plus rewrap of the completed logical lines among them. NOT UNBOUNDED in
    // the census sense — the caller's budget bounds it — with two honest
    // caveats: (1) a logical line is never split, so the single step that
    // completes a soft-wrapped run longer than max_lines rewraps that whole
    // run at once (runs are capped at MAX_LOGICAL_WIDTH accumulated display
    // cells); (2) the one step that exhausts the input also clears the store,
    // O(store blocks) of buffer drops.
    pub fn reflow_step(mut self, max_lines: usize) -> ReflowStep {
        let budget = max_lines.max(1);
        match &mut self.phase {
            ReflowPhase::Rewrap {
                next_input,
                carry,
                out,
            } => {
                let store_count = self.store.line_count();
                let total = store_count + self.lazy_lines.len();
                let end = next_input.saturating_add(budget).min(total);
                let appended_from = carry.len();
                carry.reserve(end - *next_input);
                for i in *next_input..end {
                    let line = if i < store_count {
                        // Tiered store lines first (older), decompressed here
                        // off the lock.
                        match self.store.get_line(i) {
                            Ok(Some(line)) => line.into_owned(),
                            // A decode failure must not silently truncate older
                            // history: keep a blank placeholder so ordering
                            // stays sane (mirrors `take_scrollback_lines`).
                            Ok(None) | Err(_) => Line::new(),
                        }
                    } else {
                        // Then the (newer) lazy-staged lines that had not yet
                        // been compressed (blank left behind; dropped below).
                        std::mem::replace(&mut self.lazy_lines[i - store_count], Line::new())
                    };
                    carry.push(line);
                }
                *next_input = end;

                if end == total {
                    // Input exhausted: every carried run is complete by
                    // definition — rewrap the remainder in one call (for an
                    // unlimited budget this is the WHOLE input in one call,
                    // the exact one-shot shape).
                    out.extend(super::scrollback_reflow::reflow_scrollback_lines(
                        carry,
                        self.new_cols,
                    ));
                    let out = std::mem::take(out);
                    self.lazy_lines = Vec::new(); // free the drained placeholders
                    // Re-fill the SAME store: clear() once, then push the
                    // rewrapped lines. This keeps the store's line_limit /
                    // budget / block config, and honours the line limit on push
                    // (older-beyond-cap evicted, the configured behaviour).
                    if let Err(error) = self.store.clear() {
                        aterm_log::warn!("offload reflow: scrollback clear failed: {error}");
                    }
                    self.phase = ReflowPhase::Refill { out, next_out: 0 };
                } else if let Some(rel) = carry[appended_from..]
                    .iter()
                    .rposition(|line| !line.is_wrapped())
                {
                    // Carve at the LAST unwrapped-logical-line boundary among
                    // the newly read lines (older carry is all continuations of
                    // its head by the `carry` invariant, so scanning the
                    // appended region is exhaustive): everything before it is
                    // completed runs; the trailing run stays carried.
                    let boundary = appended_from + rel;
                    if boundary > 0 {
                        let tail = carry.split_off(boundary);
                        let head = std::mem::replace(carry, tail);
                        out.extend(super::scrollback_reflow::reflow_scrollback_lines(
                            &head,
                            self.new_cols,
                        ));
                    }
                }
                // No boundary in this chunk: the whole chunk continues the
                // carried run — keep carrying (never split a logical line).
                ReflowStep::InProgress(self)
            }
            ReflowPhase::Refill { out, next_out } => {
                let end = next_out.saturating_add(budget).min(out.len());
                for slot in &mut out[*next_out..end] {
                    let line = std::mem::replace(slot, Line::new());
                    if let Err(error) = self.store.push_line(line) {
                        aterm_log::warn!("offload reflow: scrollback push_line failed: {error}");
                    }
                }
                *next_out = end;
                if end == out.len() {
                    ReflowStep::Done(ReflowedScrollback {
                        store: self.store,
                        new_cols: self.new_cols,
                        prev_offset: self.prev_offset,
                        clear_gen: self.clear_gen,
                    })
                } else {
                    ReflowStep::InProgress(self)
                }
            }
        }
    }
}

impl ReflowedScrollback {
    /// The width this history was rewrapped to.
    #[must_use]
    pub fn new_cols(&self) -> u16 {
        self.new_cols
    }

    /// The number of rewrapped history lines ready to re-attach.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.store.line_count()
    }
}

impl Grid {
    /// Resize like [`Grid::resize`], but move the unbounded tiered off-screen
    /// scrollback OFF the synchronous path.
    ///
    /// On a WIDTH change with reflow enabled and a tiered store attached: detach
    /// the tiered store (O(1)) BEFORE resizing, so the synchronous reflow inside
    /// [`Grid::resize`] touches only the bounded in-memory ring, then return the
    /// detached store as a `Send` [`PendingScrollbackReflow`]. The caller must
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
        // thread — both halves, in age order:
        //  1. lazy_lines: uncompressed staged lines (newer). Must be taken BEFORE
        //     detaching the store, else the resize's `drain_lazy_buffer` discards
        //     them (it drops staged lines when no store is attached).
        //  2. store: the compressed tiered/disk history (older), detached O(1).
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

        // With the store detached and the lazy buffer emptied, the resize's
        // `take_scrollback_lines` only rewraps the bounded ring — O(ring), the
        // synchronous budget the bounded-cost obligation checks.
        self.resize(new_rows, new_cols);

        // Enter the reflow window AFTER resize (so the resize itself runs exactly as
        // the plain path): from here until re-attach, scroll-off keeps being staged
        // to the lazy buffer instead of dropped (audit bug B).
        self.storage.scrollback_detached_for_reflow = true;
        self.storage.pending_scrollback_settings = Some(pending_settings);

        Some(PendingScrollbackReflow {
            store,
            lazy_lines,
            new_cols,
            prev_offset,
            clear_gen,
            phase: ReflowPhase::start(),
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
    pub fn reattach_reflowed_scrollback(&mut self, reflowed: ReflowedScrollback) {
        // The reflow window is over: stop staging scroll-off as "detached" — the
        // store (old or freshly attached below) is authoritative again.
        self.storage.scrollback_detached_for_reflow = false;

        if self.storage.scrollback.is_some() {
            // A terminal reset re-created the tiered store during the reflow; don't
            // clobber it (would corrupt history ordering). The reflowed store drops.
            self.reconcile_pending_scrollback_settings();
            return;
        }

        // Audit bug C: scrollback was ERASED (ED3 / `clear` / reset) during the
        // window. Do NOT resurrect the pre-erase history — attach an EMPTY store and
        // keep only the post-erase output the ring/lazy captured after the clear.
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
    /// thread died mid-rewrap. The detached tiered history is unrecoverable (it was
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
                store_fingerprint(&stepped.store),
                store_fingerprint(&one_shot.store),
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
            store_fingerprint(&stepped.store),
            store_fingerprint(&one_shot.store),
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
}
