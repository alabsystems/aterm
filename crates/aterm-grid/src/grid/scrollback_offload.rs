// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Off-thread width-change scrollback reflow.

use aterm_scrollback::{Line, ScrollbackStorage};

use super::Grid;
use super::reflow::{ReflowMode, ResizePolicy};
use super::resize_handback::ResizeLaw;
use super::state::{DetachedReaderAim, FillDebt, PendingScrollbackSettings};
use crate::Damage;

/// How many screens of cells, at the larger of a width change's two sizes, an offloaded resize holds back from its job.
pub(super) const HOLD_BACK_SCREENS: usize = 4;

/// One reader-facing viewport gesture, as the detach window records it (see [`Grid::note_detached_reader_motion`]).
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
#[inline]
fn grown_from_live(rows: usize, at_counter: u64, counter: u64) -> usize {
    if rows == 0 {
        return 0;
    }
    rows.saturating_add(usize::try_from(counter.saturating_sub(at_counter)).unwrap_or(usize::MAX))
}

impl DetachedReaderAim {
    /// The `display_offset` this aim names once the whole history (`sb` lines)
    /// is back with the row counter at `counter`, given `restored` (where the
    /// re-attach's own rule would put the reader).
    #[inline]
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

#[cfg(clean_islands)]
macro_rules! trust_island {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! trust_island {
    ($($tt:tt)*) => {};
}

trust_island! {
    clean {
        namespace ScrollbackOffload

        -- Model of the detach-window lifecycle and the fast redetach stream sequence.
        structure WindowState where
            detached : Bool
            store_lines : Nat
            lazy_lines : Nat
        deriving Repr

        def reattachWindow (s : WindowState) : WindowState :=
            { detached := false, store_lines := s.store_lines + s.lazy_lines, lazy_lines := 0 }

        def abortWindow (s : WindowState) : WindowState :=
            { detached := false, store_lines := 0, lazy_lines := 0 }

        -- Postcondition: both terminal transitions close the detach window.
        theorem reattach_closes_detach_window (s : WindowState) :
            (reattachWindow s).detached = false := by
            rfl

        theorem abort_closes_detach_window (s : WindowState) :
            (abortWindow s).detached = false := by
            rfl

        -- Fast redetach: keeping (ring_out ++ lazy_buf) in lazy_lines instead of
        -- synchronously compressing them into store on the UI thread preserves
        -- the stream input sequence (List.append_assoc) and total line count.
        def slowRedetachStream (store ring_out lazy_buf : List Nat) : List Nat :=
            ((store ++ ring_out) ++ lazy_buf) ++ []

        def fastRedetachStream (store ring_out lazy_buf : List Nat) : List Nat :=
            store ++ (ring_out ++ lazy_buf)

        theorem fast_redetach_matches_slow_trace :
            fastRedetachStream [1, 2] [3, 4] [5] = slowRedetachStream [1, 2] [3, 4] [5] := by
            rfl

        end ScrollbackOffload
    }
}

/// A grid's off-screen scrollback, detached for an off-thread rewrap to `new_cols`; made by [`Grid::resize_offloading_scrollback`].
#[must_use = "a detached scrollback store must be reflowed and re-attached, or history is lost"]
pub struct PendingScrollbackReflow {
    store: ScrollbackStorage,
    lazy_lines: Vec<Line>,
    /// The ring's scrollback rows, materialized at detach; rewrapped into
    /// [`ReflowedScrollback::ring_out`].
    ring_lines: Vec<Line>,
    new_cols: u16,
    /// The reader's scroll position at detach, restored on re-attach.
    prev_offset: usize,
    /// `GridStorage::reader_live_bottom_gen` at detach, so re-attach can tell
    /// whether the reader went to the live bottom meanwhile.
    detach_live_bottom_gen: u64,
    /// The scrollback-erase generation at detach; if it advanced by re-attach, an
    /// erase happened during the window and the reflowed store is dropped.
    clear_gen: u64,
    /// Rewrap progress; [`reflow`](Self::reflow) steps this same state to the end.
    phase: ReflowPhase,
    /// `Some` from the first step until the store stream finishes: the store's
    /// own limits, lifted out of the way of the streaming reads.
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
    /// The streaming rewrap: read input from the front of the store (truncating it as consumed), then from the newer `lazy_lines`.
    Stream {
        /// Input lines still unread at the store's front.
        store_input_left: usize,
        /// Next `lazy_lines` index to read once the store input is consumed.
        next_lazy: usize,
        /// The trailing logical line (a non-wrapped head plus its soft-wrapped
        /// continuations) that may continue into unread input, held back so a
        /// step boundary never splits a logical line.
        carry: Vec<Line>,
    },
    /// After the store stream.
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

/// The store's eviction limits (line cap and byte budget), lifted at the first
/// [`PendingScrollbackReflow::reflow_step`] and restored, with one enforcement
/// pass, by the step that finishes the store's output.
struct LiftedStoreLimits {
    line_limit: Option<usize>,
    memory_budget: usize,
}

/// The outcome of one [`PendingScrollbackReflow::reflow_step`] call.
#[must_use = "keep stepping InProgress (or re-attach Done), or history is lost"]
pub enum ReflowStep {
    /// More work remains; step it again.
    InProgress(PendingScrollbackReflow),
    /// The rewrap is complete and re-filled into the store, ready for [`Grid::reattach_reflowed_scrollback`].
    Done(ReflowedScrollback),
}

/// A scrollback store already rewrapped to `new_cols`, ready to re-attach under a brief lock with [`Grid::reattach_reflowed_scrollback`].
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

const _: fn() = || {
    const fn assert_send<T: Send>() {}
    assert_send::<PendingScrollbackReflow>();
    assert_send::<ReflowedScrollback>();
    assert_send::<ReflowStep>();
};

impl PendingScrollbackReflow {
    /// The width this history will be rewrapped to.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub const fn new_cols(&self) -> u16 {
        self.new_cols
    }

    /// Input lines read so far, of [`Self::lines_total`]: monotone, and equal to the total on the step that returns `Done`.
    #[must_use]
    #[inline]
    pub fn lines_done(&self) -> usize {
        self.lines_read.min(self.lines_total)
    }

    /// The history lines this job detached, fixed at detach (unlike [`Self::line_count`], which moves as the stream consumes input and re-fills the store with output).
    #[must_use]
    #[inline]
    pub const fn lines_total(&self) -> usize {
        self.lines_total
    }

    /// The history lines the job holds now, input and output (tiered + lazy + ring), for logging.
    #[must_use]
    #[inline]
    pub fn line_count(&self) -> usize {
        self.store.line_count() + self.lazy_lines.len() + self.ring_lines.len()
    }

    /// The whole rewrap in one call; run it off the caller's thread and lock.
    // COST: O(session history).
    pub fn reflow(self) -> ReflowedScrollback {
        let mut job = self;
        loop {
            match job.reflow_step(usize::MAX) {
                ReflowStep::InProgress(next) => job = next,
                ReflowStep::Done(reflowed) => return reflowed,
            }
        }
    }

    /// Advance the rewrap by up to `max_lines` input lines (0 counts as 1), then yield.
    // COST: O(max_lines × cols) per step (≤ max_lines line reads plus rewrap and
    pub fn reflow_step(mut self, max_lines: usize) -> ReflowStep {
        let budget = max_lines.max(1);
        if self.lifted_limits.is_none() && matches!(self.phase, ReflowPhase::Stream { .. }) {
            let lifted = LiftedStoreLimits {
                line_limit: self.store.line_limit(),
                memory_budget: self.store.memory_budget(),
            };
            self.store.set_line_limit(None);
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
                    let line = match self.store.get_line(i) {
                        Ok(Some(line)) => line.into_owned(),
                        Ok(None) | Err(_) => Line::new(),
                    };
                    carry.push(line);
                }
                if from_store > 0 {
                    // Release consumed input now (warm blocks and cold pages free
                    if let Err(error) = self.store.truncate_oldest(from_store) {
                        aterm_log::warn!("offload reflow: input front-truncation failed: {error}");
                    }
                    *store_input_left -= from_store;
                }
                for _ in from_store..read {
                    carry.push(std::mem::replace(
                        &mut self.lazy_lines[*next_lazy],
                        Line::new(),
                    ));
                    *next_lazy += 1;
                }

                if read == total_left {
                    let straddling = if self.ring_lines.first().is_some_and(Line::is_wrapped) {
                        let head = carry.iter().rposition(|line| !line.is_wrapped());
                        carry.split_off(head.unwrap_or(0))
                    } else {
                        Vec::new()
                    };
                    let out =
                        super::scrollback_reflow::reflow_scrollback_lines(carry, self.new_cols);
                    carry.clear();
                    carry.shrink_to_fit();
                    self.lazy_lines = Vec::new();
                    for line in out {
                        if let Err(error) = self.store.push_line(line) {
                            aterm_log::warn!(
                                "offload reflow: scrollback push_line failed: {error}"
                            );
                        }
                    }
                    if let Some(limits) = self.lifted_limits.take() {
                        self.store.set_line_limit(limits.line_limit);
                        if let Err(error) = self.store.set_memory_budget(limits.memory_budget) {
                            aterm_log::warn!(
                                "offload reflow: restoring the memory budget failed: {error}"
                            );
                        }
                    }
                    self.phase = ReflowPhase::RingRewrap {
                        next_input: 0,
                        carry: straddling,
                        out: Vec::new(),
                    };
                } else {
                    // Rewrap and push the runs completed before the last run head
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
                    carry.push(std::mem::replace(line, Line::new()));
                }
                *next_input = end;

                if end == total {
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

/// Rewrap the completed runs of `carry` into `out`, keeping the trailing run
/// carried.
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
    #[inline]
    #[cfg(test)]
    pub const fn new_cols(&self) -> u16 {
        self.new_cols
    }

    /// The number of rewrapped history lines ready to re-attach (store + ring history).
    #[must_use]
    #[inline]
    pub fn line_count(&self) -> usize {
        self.store.line_count() + self.ring_out.len()
    }
}

impl Grid {
    /// Resize like [`Grid::resize`], but move the tiered off-screen scrollback's rewrap off the synchronous path.
    pub fn resize_offloading_scrollback(
        &mut self,
        new_rows: u16,
        new_cols: u16,
    ) -> Option<PendingScrollbackReflow> {
        self.resize_offloading_scrollback_with_policy(new_rows, new_cols, ResizePolicy::Native)
    }

    /// [`Self::resize_offloading_scrollback`] under an explicit [`ResizePolicy`].
    pub fn resize_offloading_scrollback_with_policy(
        &mut self,
        new_rows: u16,
        new_cols: u16,
        policy: ResizePolicy,
    ) -> Option<PendingScrollbackReflow> {
        let new_rows = new_rows.clamp(1, crate::MAX_GRID_ROWS);
        let new_cols = new_cols.clamp(1, crate::MAX_GRID_COLS);
        let old_cols = self.storage.cols;
        if new_cols == old_cols || self.storage.scrollback.is_none() {
            self.resize_with_policy(new_rows, new_cols, policy);
            return None;
        }
        let handed = self.hand_back_before_resize(new_cols, ReflowMode::Enabled, policy);
        let screen_cells = usize::from(self.storage.visible_rows.max(new_rows))
            * usize::from(old_cols.max(new_cols));
        let rows_step = self.split_rows_step(new_rows, new_cols, ReflowMode::Enabled, policy);
        if policy == ResizePolicy::Native
            && self.storage.resize_law == ResizeLaw::Rewrap
            && !self.handback_one_pass()
        {
            match self.prepare_rewrap(new_cols) {
                Some(plan) => self.offer_plan(plan),
                None => self.forget_handback(),
            }
        }

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
        self.close_reflow_window_cap_run();
        self.storage.scrollback_detached_for_reflow = true;
        self.storage.staged_width_stale = false;
        self.storage.pending_scrollback_settings = Some(pending_settings);
        self.storage.detached_reader_aim = None;
        self.storage.fill_debt = FillDebt::default();
        let held = self.ring_rows_to_hold_back(HOLD_BACK_SCREENS * screen_cells);
        let mut ring_lines = self.take_older_ring_scrollback_lines(held);
        if policy == ResizePolicy::Native && held == 0 {
            let lifted = self.take_boundary_continuation_lines();
            self.note_rows_lifted(lifted.len());
            ring_lines.extend(lifted);
        }

        self.resize_with_policy(new_rows, new_cols, policy);
        if let Some(handed) = handed {
            self.merge_handback_step(handed);
        }
        if let Some(rows_step) = rows_step {
            self.merge_rows_step(rows_step);
        }
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

    /// Re-attach a store rewrapped off-thread as the oldest tiered history, after [`resize_offloading_scrollback`](Self::resize_offloading_scrollback) detached it.
    pub fn reattach_reflowed_scrollback(&mut self, reflowed: ReflowedScrollback) {
        self.storage.scrollback_detached_for_reflow = false;
        self.close_reflow_window_cap_run();
        let reader_aim = self.storage.detached_reader_aim.take();
        let counter_at_reattach = self.storage.absolute_row_counter;

        if self.storage.scrollback.is_some() {
            self.drop_fill_debt();
            self.reconcile_pending_scrollback_settings();
            self.reattach_ring_history(reflowed.ring_out, reflowed.new_cols);
            return;
        }

        if reflowed.clear_gen != self.storage.scrollback_clear_gen {
            let mut store = reflowed.store;
            let _ = store.clear();
            self.storage.scrollback = Some(store);
            self.drop_fill_debt();
            self.settle_fill(0);
            self.reconcile_pending_scrollback_settings();
            self.drain_lazy_buffer();
            self.storage.display_offset = 0;
            self.storage.damage = Damage::Full;
            self.storage.content_gen += 1;
            return;
        }

        self.storage.scrollback = Some(reflowed.store);
        self.reconcile_pending_scrollback_settings();
        self.reattach_ring_history(reflowed.ring_out, reflowed.new_cols);
        self.drain_lazy_buffer();
        if reflowed.new_cols == self.storage.cols {
            let pulled = self.pay_fill_debt();
            self.settle_fill(pulled);
        }
        let sb = self.storage.scrollback_lines();
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
        self.cover_retained_rows();
    }

    /// Re-attach, then converge.
    #[must_use = "a returned job is a re-detached store: drive it and re-attach it (or abort), or history is lost"]
    pub fn reattach_reflowed_scrollback_or_redetach(
        &mut self,
        reflowed: ReflowedScrollback,
    ) -> Option<PendingScrollbackReflow> {
        let was_erased = reflowed.clear_gen != self.storage.scrollback_clear_gen;
        let had_replacement = self.storage.scrollback.is_some();
        let stale_cols = reflowed.new_cols;
        let staged_stale = self.storage.staged_width_stale
            && (reflowed.store.line_limit() != Some(0)
                || self
                    .storage
                    .pending_scrollback_settings
                    .as_ref()
                    .is_some_and(|pending| pending.line_limit_changed));
        let current = stale_cols == self.storage.cols && !staged_stale;
        if was_erased || had_replacement || current {
            self.reattach_reflowed_scrollback(reflowed);
            return None;
        }

        self.storage.scrollback_detached_for_reflow = false;
        self.close_reflow_window_cap_run();
        let reader_aim = self.storage.detached_reader_aim.take();
        let counter_at_reattach = self.storage.absolute_row_counter;

        self.storage.scrollback = Some(reflowed.store);
        self.reconcile_pending_scrollback_settings();

        let lazy_lines = if self.storage.store_retains_lines() {
            let mut staged = reflowed.ring_out;
            staged.extend(self.storage.lazy_buffer.drain_all());
            self.storage.lazy_buffer.release_spine_slack();
            if let Some(store) = self.storage.scrollback.as_mut()
                && let Some(max) = store.line_limit()
            {
                let total = store.line_count().saturating_add(staged.len());
                if total > max {
                    let excess = total - max;
                    let from_store = excess.min(store.line_count());
                    if from_store > 0 {
                        let _ = store.truncate_oldest(from_store);
                    }
                    let from_staged = excess - from_store;
                    if from_staged > 0 {
                        staged.drain(..from_staged);
                    }
                }
            }
            staged
        } else {
            self.reattach_ring_history(reflowed.ring_out, reflowed.new_cols);
            self.drain_lazy_buffer();
            self.storage.lazy_buffer.drain_all().collect()
        };

        let sb = self
            .storage
            .scrollback_lines()
            .saturating_add(lazy_lines.len());
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
        let retained = u64::from(self.storage.visible_rows).saturating_add(sb as u64);
        if self.storage.absolute_row_counter < retained {
            self.renumber_rows(retained);
            self.storage.history_renumber_epoch =
                self.storage.history_renumber_epoch.saturating_add(1);
            self.invalidate_host_coordinates();
        }

        self.storage.scrollback.as_ref()?;

        let prev_offset = self.storage.display_offset;
        let clear_gen = self.storage.scrollback_clear_gen;
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
        self.clamp_display_offset();
        let detach_live_bottom_gen = self.storage.reader_live_bottom_gen;
        self.close_reflow_window_cap_run();
        self.storage.scrollback_detached_for_reflow = true;
        self.storage.staged_width_stale = false;
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

    /// Return the job-carried ring history (rewrapped to `new_cols`) to the grid
    /// at re-attach.
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
                // Unreachable (a store-less grid took the ring route above); the
                drop(ring_out.drain(..skip));
            }
        }
        if !ring_out.is_empty() {
            self.prepend_ring_scrollback_lines(ring_out, new_cols);
        }
    }

    /// [`reattach_ring_history`](Self::reattach_ring_history) when the store
    /// keeps no line: the job's ring history (`lines`, rewrapped at `new_cols`)
    /// and then the window's staged output go back into the ring, newest first,
    /// into the room the window's own output left, at the grid's current width;
    /// the rest is dropped as a ring-only grid's cap drops it.
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

    /// Record a reader-facing viewport gesture made during a detach window (no-op outside one).
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
    /// left it.
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

    /// Whether the reader has aimed the viewport inside the open detach window.
    #[must_use]
    pub const fn reader_aim_held(&self) -> bool {
        self.storage.detached_reader_aim.is_some()
    }

    /// True while a detach window is open: the tiered store is out for an off-thread reflow and scroll-off is staged for re-attach.
    #[must_use]
    pub const fn reflow_offload_in_flight(&self) -> bool {
        self.storage.scrollback_detached_for_reflow
    }

    /// Recover from a reflow that will never re-attach (the worker panicked or died).
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
        self.drop_fill_debt();
        self.storage.detached_reader_aim = None;
        if self.storage.scrollback.is_some() {
            self.reconcile_pending_scrollback_settings();
            self.drain_lazy_buffer();
        } else {
            self.storage.lazy_buffer.clear();
            self.clamp_display_offset();

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
    /// when longer than the width, a wide glyph early-wrapping as output does),
    /// then terminate it with a hard newline.
    fn logical_line(grid: &mut Grid, text: &str) {
        use crate::{CellFlags, PackedColor};
        for c in text.chars() {
            // Deferred autowrap: long text soft-wraps.
            if aterm_grapheme::char_width(c) == 2 {
                grid.write_wide_char_wrap_styled(
                    c,
                    PackedColor::DEFAULT_FG,
                    PackedColor::DEFAULT_BG,
                    CellFlags::empty(),
                );
            } else {
                grid.write_char_wrap(c);
            }
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
                0 => logical_line(grid, ""),
                1 => logical_line(grid, &format!("S{i}")),
                2 => {
                    let len = cols as usize * (2 + rng.below(4) as usize)
                        + rng.below(u64::from(cols)) as usize;
                    let mut s = format!("R{i}-");
                    while s.len() < len {
                        s.push((b'a' + rng.below(26) as u8) as char);
                    }
                    logical_line(grid, &s);
                }
                3 => {
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

    /// For any schedule of step budgets the stepped rewrap matches the one-shot
    /// [`PendingScrollbackReflow::reflow`] line for line (full Debug rendering),
    /// over mixed content plus a lazy-staged tail.
    #[test]
    fn reflow_step_any_schedule_matches_one_shot() {
        for seed in [1u64, 7, 42, 0x00C0_FFEE] {
            let (rows, cols, new_cols) = (10u16, 40u16, 23u16);
            let mut a = tiered_grid(rows, cols, 8);
            let mut b = tiered_grid(rows, cols, 8);
            for g in [&mut a, &mut b] {
                feed_mixed_history(g, seed, 400, cols);
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

    /// With budget 1, a many-row soft-wrapped run crosses step boundaries intact,
    /// matches the one-shot and survives re-attach.
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
                // scroll it all off-screen
                logical_line(g, &format!("pad{i}"));
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
        logical_line(&mut g, "visible only");
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

    /// `lines_done` climbs monotonically through both phases to the fixed
    /// `lines_total` on the step returning `Done`.
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

    /// Dropping a half-stepped job loses its history, and
    /// [`Grid::abort_reflow_offload`] leaves the grid bounded, not wedged.
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
        drop(half);
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

    /// One width change for the offloaded-vs-plain differential below: the
    /// grid's shape and ring, then the drag of `(rows, cols)` steps.
    struct Drag {
        label: &'static str,
        policy: ResizePolicy,
        ring: usize,
        rows: u16,
        cols: u16,
        steps: &'static [(u16, u16)],
        output: usize,
    }

    const DRAGS: [Drag; 13] = [
        Drag {
            label: "one narrowing step, the ring holds its overflow",
            policy: ResizePolicy::Native,
            ring: 32,
            rows: 10,
            cols: 40,
            steps: &[(10, 23)],
            output: 0,
        },
        Drag {
            label: "a ring larger than the window holds back",
            policy: ResizePolicy::Native,
            ring: 64,
            rows: 4,
            cols: 40,
            steps: &[(4, 23), (6, 30), (5, 26)],
            output: 0,
        },
        Drag {
            label: "one widening step",
            policy: ResizePolicy::Native,
            ring: 8,
            rows: 10,
            cols: 23,
            steps: &[(10, 40)],
            output: 0,
        },
        Drag {
            label: "a drag whose later steps land inside the window",
            policy: ResizePolicy::Native,
            ring: 8,
            rows: 13,
            cols: 40,
            steps: &[(10, 35), (4, 30), (3, 28)],
            output: 0,
        },
        Drag {
            label: "a drag with output streaming between its steps",
            policy: ResizePolicy::Native,
            ring: 8,
            rows: 13,
            cols: 40,
            steps: &[(10, 35), (4, 30), (3, 28)],
            output: 12,
        },
        Drag {
            label: "a drag back to the job's width with output staged between",
            policy: ResizePolicy::Native,
            ring: 4,
            rows: 8,
            cols: 40,
            steps: &[(8, 31), (8, 26), (8, 31)],
            output: 12,
        },
        Drag {
            label: "a grow inside the window after output filled the screen",
            policy: ResizePolicy::Native,
            ring: 2,
            rows: 10,
            cols: 17,
            steps: &[(6, 15), (15, 16)],
            output: 8,
        },
        Drag {
            label: "a widening inside the window after output filled the screen",
            policy: ResizePolicy::Native,
            ring: 2,
            rows: 10,
            cols: 17,
            steps: &[(10, 15), (10, 30)],
            output: 8,
        },
        Drag {
            label: "a rows-only grow inside the window after output",
            policy: ResizePolicy::Native,
            ring: 2,
            rows: 10,
            cols: 17,
            steps: &[(6, 15), (15, 15)],
            output: 8,
        },
        Drag {
            label: "one step pushing more rows than the ring holds",
            policy: ResizePolicy::Native,
            ring: 8,
            rows: 13,
            cols: 40,
            steps: &[(3, 23)],
            output: 0,
        },
        Drag {
            label: "the same push under ConPTY",
            policy: ResizePolicy::ConPty,
            ring: 8,
            rows: 13,
            cols: 40,
            steps: &[(3, 23)],
            output: 0,
        },
        Drag {
            label: "one narrowing step under ConPTY",
            policy: ResizePolicy::ConPty,
            ring: 8,
            rows: 10,
            cols: 40,
            steps: &[(10, 23)],
            output: 0,
        },
        Drag {
            label: "a drag with output under ConPTY",
            policy: ResizePolicy::ConPty,
            ring: 8,
            rows: 13,
            cols: 40,
            steps: &[(10, 35), (4, 30), (3, 28)],
            output: 12,
        },
    ];

    /// Every history line, oldest first, as `get_history_line` reads it across
    /// the tiers: text and wrap flag.
    fn history_seq(g: &Grid) -> Vec<(String, bool)> {
        (0..g.scrollback_lines())
            .map(|i| {
                g.get_history_line(i).map_or_else(
                    || (String::new(), false),
                    |line| (line.as_str().unwrap_or("").to_string(), line.is_wrapped()),
                )
            })
            .collect()
    }

    /// Every screen row's text.
    fn screen_rows(g: &Grid) -> Vec<String> {
        (0..g.rows())
            .map(|r| g.row_text(r).unwrap_or_default())
            .collect()
    }

    /// With the ring history riding the job, the offloaded width change ends
    /// where the plain synchronous `Grid::resize` does: the same history
    /// sequence (texts, wrap flags, order, count), the same screen and the
    /// same reading position, over deep single-row history, random step
    /// schedules, a reader at the live bottom or scrolled back, and each
    /// [`Drag`].
    #[test]
    fn offload_round_trip_matches_plain_resize_history_sequence() {
        let (read, screen) = (history_seq, screen_rows);
        for drag in &DRAGS {
            for seed in [3u64, 11, 0xFEED] {
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
                        feed_mixed_history(g, seed, 300, drag.cols);
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
                    let held = HOLD_BACK_SCREENS
                        * usize::from(drag.rows.max(first_rows))
                        * usize::from(drag.cols.max(first_cols))
                        / usize::from(drag.cols);
                    assert!(
                        drag.ring <= held || !job.ring_lines.is_empty(),
                        "{case}: detach lifts the ring history past the hold-back into the job (RFL-1)"
                    );
                    plain.resize_with_policy(first_rows, first_cols, drag.policy);
                    for (step, &(rows, cols)) in drag.steps[1..].iter().enumerate() {
                        for g in [&mut offloaded, &mut plain] {
                            feed_mixed_history(g, seed ^ step as u64, drag.output, cols);
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
                    let grows = drag
                        .steps
                        .windows(2)
                        .any(|w| w[1].0 > w[0].0 || w[1].1 > w[0].1);
                    assert!(
                        offloaded.ring_buffer_scrollback() > 0 || grows,
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

    /// The text of every logical line in the history and on the screen, in
    /// order: what a resize must keep, wherever it puts each row.
    fn logical_lines(g: &Grid) -> Vec<String> {
        let rows = history_seq(g).into_iter().chain((0..g.rows()).map(|r| {
            let wrapped = g
                .storage
                .row_at_screen(r)
                .is_some_and(|row| row.is_wrapped());
            (g.row_text(r).unwrap_or_default(), wrapped)
        }));
        let mut lines: Vec<String> = Vec::new();
        for (text, wrapped) in rows {
            match lines.last_mut() {
                Some(line) if wrapped => line.push_str(&text),
                _ => lines.push(text),
            }
        }
        lines.retain(|line| !line.is_empty());
        lines
    }

    /// Seeded random drags under both policies: any shape and ring, a first
    /// step that detaches, then up to three steps inside the window (shrinks,
    /// grows, widenings, narrowings), with output between them or none, on
    /// full screens and on screens cleared and partly drawn again, and in
    /// half of them a shell's prompt redrawn after every resize (`CR`, `ED 0`,
    /// the prompt).
    #[test]
    fn random_offloaded_drags_end_where_the_plain_resize_does() {
        let mut exact = 0;
        for policy in [ResizePolicy::Native, ResizePolicy::ConPty] {
            for seed in 0..300u64 {
                let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9) ^ 0x5EED);
                let rows = 2 + rng.below(13) as u16;
                let cols = 5 + rng.below(40) as u16;
                let ring = 1 + rng.below(20) as usize;
                let mut steps: Vec<(u16, u16)> = (0..1 + rng.below(4))
                    .map(|_| (2 + rng.below(13) as u16, 5 + rng.below(40) as u16))
                    .collect();
                if steps[0].1 == cols {
                    steps[0].1 += 1;
                }
                let output = [0, 5, 10][rng.below(3) as usize];
                let redrawn = (rng.below(2) == 0).then(|| rng.below(u64::from(rows)) as usize);
                let prompt = rng.below(2) == 0;
                let case = format!(
                    "{policy:?} seed {seed}: {rows}x{cols} ring {ring}, steps {steps:?}, \
                     output {output}, redrawn {redrawn:?}, prompt {prompt}"
                );
                let redraw = |g: &mut Grid| {
                    if prompt {
                        g.carriage_return();
                        g.erase_to_end_of_screen();
                        "$ ".chars().for_each(|c| g.write_char_wrap(c));
                    }
                };
                let mut offloaded = tiered_grid(rows, cols, ring);
                let mut plain = tiered_grid(rows, cols, ring);
                for g in [&mut offloaded, &mut plain] {
                    feed_mixed_history(g, seed, 120, cols);
                    if let Some(lines) = redrawn {
                        g.erase_screen();
                        g.set_cursor(0, 0);
                        feed_mixed_history(g, seed ^ 0xC1EA, lines, cols);
                    }
                }
                let job = offloaded
                    .resize_offloading_scrollback_with_policy(steps[0].0, steps[0].1, policy)
                    .expect("a width change with a tiered store detaches");
                plain.resize_with_policy(steps[0].0, steps[0].1, policy);
                redraw(&mut offloaded);
                redraw(&mut plain);
                for (k, &(rows, cols)) in steps[1..].iter().enumerate() {
                    for g in [&mut offloaded, &mut plain] {
                        feed_mixed_history(g, seed ^ (k as u64 + 7), output, cols);
                        g.resize_with_policy(rows, cols, policy);
                        redraw(g);
                    }
                }
                let mut schedule = Rng(seed ^ 0xD1FF);
                let mut next = Some(job);
                while let Some(job) = next {
                    let (done, _steps) = step_with_schedule(job, &mut schedule, 29);
                    next = offloaded.reattach_reflowed_scrollback_or_redetach(done);
                }
                assert_eq!(
                    logical_lines(&offloaded),
                    logical_lines(&plain),
                    "{case}: every line, once and in order"
                );
                if redrawn.is_none() || steps.len() == 1 {
                    exact += 1;
                    assert_eq!(
                        history_seq(&offloaded),
                        history_seq(&plain),
                        "{case}: history"
                    );
                    assert_eq!(
                        screen_rows(&offloaded),
                        screen_rows(&plain),
                        "{case}: screen"
                    );
                    assert_eq!(
                        offloaded.display_offset(),
                        plain.display_offset(),
                        "{case}: the reader's place"
                    );
                }
                offloaded.assert_invariants();
                plain.assert_invariants();
            }
        }
        assert!(
            exact > 300,
            "only {exact} drags were held to the plain resize's rows"
        );
    }

    /// What a program does inside a detach window, the same on both grids of a
    /// differential: output, a prompt redrawn on its row (`CR`, `ED 0`), one
    /// redrawn from rows above it (`CUU`, `CR`, `ED 0`, as a multi-line prompt
    /// does), a write at an addressed row (`CUP`), or a screen cleared and drawn
    /// again (`ED 2`).
    #[derive(Clone, Copy, Debug)]
    enum WindowAction {
        Output,
        Prompt,
        PromptAbove,
        Addressed,
        Cleared,
    }

    fn window_action(g: &mut Grid, action: WindowAction, seed: u64) {
        let mut rng = Rng(seed);
        match action {
            WindowAction::Output => feed_mixed_history(g, seed, 5, g.cols()),
            WindowAction::Prompt | WindowAction::PromptAbove => {
                if let WindowAction::PromptAbove = action {
                    g.cursor_up(1 + rng.below(3) as u16);
                }
                g.carriage_return();
                g.erase_to_end_of_screen();
                "$ ".chars().for_each(|c| g.write_char_wrap(c));
            }
            WindowAction::Addressed => {
                g.set_cursor(rng.below(u64::from(g.rows())) as u16, 0);
                g.write_char_wrap('Z');
            }
            WindowAction::Cleared => {
                g.erase_screen();
                g.set_cursor(0, 0);
                feed_mixed_history(g, seed, 2, g.cols());
            }
        }
    }

    /// Seeded random drags as a session makes them, over a ring that holds more
    /// history than the window needs (the shipped ring is 1000 rows): any
    /// shape, a first step that detaches and up to three inside the window,
    /// full screens and cleared ones, and every [`WindowAction`] after every
    /// step.
    #[test]
    fn random_offloaded_drags_over_a_full_ring_end_where_the_plain_resize_does() {
        use WindowAction::{Addressed, Cleared, Output, Prompt, PromptAbove};
        let (mut drags, mut owed_drags) = (0, 0);
        for policy in [ResizePolicy::Native, ResizePolicy::ConPty] {
            for action in [Output, Prompt, PromptAbove, Addressed, Cleared] {
                for seed in 0..100u64 {
                    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9) ^ 0xF011);
                    let rows = 2 + rng.below(13) as u16;
                    let cols = 5 + rng.below(40) as u16;
                    let mut steps: Vec<(u16, u16)> = (0..1 + rng.below(4))
                        .map(|_| (2 + rng.below(13) as u16, 5 + rng.below(40) as u16))
                        .collect();
                    if steps[0].1 == cols {
                        steps[0].1 += 1;
                    }
                    let redrawn = (rng.below(2) == 0).then(|| rng.below(u64::from(rows)) as usize);
                    let act_seed = rng.next();
                    let case = format!(
                        "{policy:?} {action:?} seed {seed}: {rows}x{cols}, steps {steps:?}, \
                         redrawn {redrawn:?}"
                    );
                    let mut offloaded = tiered_grid(rows, cols, 300);
                    let mut plain = tiered_grid(rows, cols, 300);
                    for g in [&mut offloaded, &mut plain] {
                        feed_mixed_history(g, seed, 120, cols);
                        if let Some(lines) = redrawn {
                            g.erase_screen();
                            g.set_cursor(0, 0);
                            feed_mixed_history(g, seed ^ 0xC1EA, lines, cols);
                        }
                    }
                    let job = offloaded
                        .resize_offloading_scrollback_with_policy(steps[0].0, steps[0].1, policy)
                        .expect("a width change with a tiered store detaches");
                    plain.resize_with_policy(steps[0].0, steps[0].1, policy);
                    let mut owed = offloaded.storage.fill_debt != FillDebt::default();
                    for (k, &(rows, cols)) in steps.iter().enumerate() {
                        if k > 0 {
                            offloaded.resize_with_policy(rows, cols, policy);
                            plain.resize_with_policy(rows, cols, policy);
                            owed |= offloaded.storage.fill_debt != FillDebt::default();
                        }
                        window_action(&mut offloaded, action, act_seed ^ k as u64);
                        window_action(&mut plain, action, act_seed ^ k as u64);
                    }
                    let mut schedule = Rng(seed ^ 0xD1FF);
                    let mut next = Some(job);
                    while let Some(job) = next {
                        let (done, _steps) = step_with_schedule(job, &mut schedule, 29);
                        next = offloaded.reattach_reflowed_scrollback_or_redetach(done);
                    }
                    drags += 1;
                    if owed {
                        owed_drags += 1;
                    } else {
                        assert_eq!(
                            history_seq(&offloaded),
                            history_seq(&plain),
                            "{case}: history"
                        );
                        assert_eq!(
                            screen_rows(&offloaded),
                            screen_rows(&plain),
                            "{case}: screen"
                        );
                        assert_eq!(
                            offloaded.display_offset(),
                            plain.display_offset(),
                            "{case}: the reader's place"
                        );
                    }
                    offloaded.assert_invariants();
                    plain.assert_invariants();
                }
            }
        }
        assert!(
            owed_drags * 50 < drags,
            "{owed_drags} of {drags} drags owed the re-attach rows: the window held too little back"
        );
    }

    /// The window holds back the history its screen shows: a full screen
    /// narrowed in one step pulls the newest history rows down on both paths,
    /// so a prompt redrawn from two rows up erases the same rows.
    #[test]
    fn a_prompt_redrawn_from_rows_above_finds_the_rows_the_resize_pulled_down() {
        let mut offloaded = tiered_grid(5, 42, 11);
        let mut plain = tiered_grid(5, 42, 11);
        for g in [&mut offloaded, &mut plain] {
            feed_mixed_history(g, 5, 40, 42);
            logical_line(g, "first");
            let long: String = (0..178).map(|i| (b'a' + (i % 26) as u8) as char).collect();
            logical_line(g, &long);
        }
        let job = offloaded
            .resize_offloading_scrollback(3, 12)
            .expect("a width change with a tiered store detaches");
        plain.resize(3, 12);
        assert_eq!(
            offloaded.storage.fill_debt,
            FillDebt::default(),
            "the window filled its screen from the lines it held back"
        );
        assert_eq!(screen_rows(&offloaded), screen_rows(&plain), "the screen");
        for g in [&mut offloaded, &mut plain] {
            g.cursor_up(2);
            g.carriage_return();
            g.erase_to_end_of_screen();
            "> ".chars().for_each(|c| g.write_char_wrap(c));
        }
        offloaded.reattach_reflowed_scrollback(job.reflow());
        assert_eq!(screen_rows(&offloaded), screen_rows(&plain), "the screen");
        assert_eq!(history_seq(&offloaded), history_seq(&plain), "the history");
        offloaded.assert_invariants();
    }

    /// What a detach window owes its re-attach (`FillDebt`) follows what the
    /// program does in it as the synchronous screen moves, each case against
    /// the plain resize from the same grid: lines printed into the blank rows
    /// under the cursor move the band, not the rows owed; a prompt redrawn
    /// after the resize (`CR`, `ED 0`) keeps them owed; a program that homes
    /// the cursor and erases draws a new screen, filled with nothing; and a
    /// cleared screen shrunk and grown back inside the window gets its blank
    /// rows back, not history.
    #[test]
    fn window_debt_follows_what_the_program_does() {
        type Step = fn(&mut Grid);
        fn print_two(g: &mut Grid) {
            logical_line(g, "one");
            logical_line(g, "two");
        }
        fn prompt(g: &mut Grid) {
            g.carriage_return();
            g.erase_to_end_of_screen();
            "$ ".chars().for_each(|c| g.write_char_wrap(c));
        }
        fn home_and_erase(g: &mut Grid) {
            g.set_cursor(0, 0);
            g.erase_to_end_of_screen();
            logical_line(g, "new screen");
        }
        fn shrink_and_grow(g: &mut Grid) {
            g.resize(4, 30);
            g.resize(12, 30);
        }
        let cases: [(&str, Option<usize>, Step); 4] = [
            ("print", Some(5), print_two),
            ("prompt", None, prompt),
            ("home and erase", None, home_and_erase),
            ("shrink and grow", Some(0), shrink_and_grow),
        ];
        for (name, redrawn, step) in cases {
            let mut offloaded = tiered_grid(12, 12, 1);
            let mut plain = tiered_grid(12, 12, 1);
            for g in [&mut offloaded, &mut plain] {
                feed_mixed_history(g, 7, 120, 12);
                if let Some(lines) = redrawn {
                    g.erase_screen();
                    g.set_cursor(0, 0);
                    feed_mixed_history(g, 9, lines, 12);
                }
            }
            let job = offloaded
                .resize_offloading_scrollback(12, 30)
                .expect("a width change with a tiered store detaches");
            plain.resize(12, 30);
            assert!(
                redrawn == Some(0) || offloaded.storage.fill_debt.rows > 0,
                "{name}: precondition, the widening owes rows"
            );
            step(&mut offloaded);
            step(&mut plain);
            offloaded.reattach_reflowed_scrollback(job.reflow());
            assert_eq!(
                screen_rows(&offloaded),
                screen_rows(&plain),
                "{name}: the screen"
            );
            assert_eq!(
                (offloaded.cursor_row(), offloaded.cursor_col()),
                (plain.cursor_row(), plain.cursor_col()),
                "{name}: the cursor"
            );
            offloaded.assert_invariants();
        }
    }

    /// A materialize-everything reference rewrap: read all store+lazy input
    /// into a Vec, rewrap in one pass, `clear()` the store, and push every
    /// output line with the store's limits active.
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
    /// limits.
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
    #[test]
    fn streaming_matches_materialize_all_reference() {
        for (limit, budget_bytes) in [(None, 8_000_000usize), (Some(120), 8_000_000)] {
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
                assert_eq!(one_shot.store.line_limit(), limit);
                assert_eq!(one_shot.store.memory_budget(), budget_bytes);
            }
        }
    }

    /// Under a tight byte budget the two shapes enforce at different moments
    /// (push-time with the newest output still hot, versus once after
    /// re-tiering), so they evict to different depths.
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
    /// materialized history.
    #[test]
    fn streaming_step_carry_stays_bounded() {
        let (rows, cols) = (10u16, 30u16);
        let mut g = tiered_grid(rows, cols, 8);
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

    /// A width change landing while a job is in flight (detaching nothing):
    /// the converging re-attach re-detaches at the settled width, and one more
    /// pass leaves the store wrapped at the current width with no further job.
    #[test]
    fn converging_reattach_rewraps_to_the_settled_width() {
        let (rows, w0, w1, w2) = (10u16, 80u16, 40u16, 60u16);
        let mut g = tiered_grid(rows, w0, 8);
        for i in 0..300 {
            let mut s = format!("C{i}-");
            while s.len() + 1 < w0 as usize {
                s.push('z');
            }
            logical_line(&mut g, &s);
        }
        let job = g.resize_offloading_scrollback(rows, w1).expect("job");
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

    #[test]
    fn converging_redetach_stages_ring_and_window_output_without_ui_store_push() {
        let (rows, w0, w1, w2) = (10u16, 80u16, 40u16, 60u16);
        let mut g = tiered_grid(rows, w0, 64);
        for i in 0..300 {
            logical_line(&mut g, &format!("H{i}-content"));
        }
        let job = g.resize_offloading_scrollback(rows, w1).expect("job");
        let ring_carried = job.ring_lines.len();
        assert!(ring_carried > 0, "precondition: ring history rides job");

        for i in 0..40 {
            logical_line(&mut g, &format!("W{i}-window"));
        }
        let staged_before = g.storage.lazy_buffer.len();
        assert!(
            staged_before > 0,
            "precondition: lazy_buffer has window output"
        );
        g.resize(rows, w2);
        let staged_at_redetach = g.storage.lazy_buffer.len();

        let reflowed = job.reflow();
        let store_lines_before_redetach = reflowed.store.line_count();
        let ring_out_len = reflowed.ring_out.len();
        let follow = g
            .reattach_reflowed_scrollback_or_redetach(reflowed)
            .expect("width mismatch must redetach");

        assert_eq!(follow.store.line_count(), store_lines_before_redetach);
        assert_eq!(follow.lazy_lines.len(), ring_out_len + staged_at_redetach);
        assert!(
            g.reattach_reflowed_scrollback_or_redetach(follow.reflow())
                .is_none()
        );
        g.assert_invariants();
    }

    /// A line whose head scrolled past the ring (into lazy staging) while its
    /// continuation is still the ring's oldest row: the job rewraps it whole
    /// under either policy, leaving the history the synchronous resize leaves.
    #[test]
    fn a_job_rewraps_a_line_straddling_the_store_ring_seam_whole() {
        for policy in [ResizePolicy::ConPty, ResizePolicy::Native] {
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
