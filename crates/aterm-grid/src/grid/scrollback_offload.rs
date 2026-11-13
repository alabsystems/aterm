// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Off-thread width-change scrollback reflow.

use aterm_scrollback::{Line, ScrollbackStorage};

use super::Grid;
use super::lazy_buffer::LazyBuffer;
use super::reflow::{ReflowMode, ResizePolicy};
use super::resize_handback::ResizeLaw;
use super::state::{DetachedReaderAim, FillDebt, PendingScrollbackSettings};
use crate::Damage;

/// How many screens of cells an offloaded resize holds back from its job.
pub(super) const HOLD_BACK_SCREENS: usize = 4;

/// One reader-facing viewport gesture ([`Grid::note_detached_reader_motion`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReaderMotion {
    /// `scroll_display(delta)`.
    By(i32),
    /// `scroll_to_top`.
    Top,
    /// `scroll_to_bottom`.
    Live,
}

/// A live-relative aim of `rows` taken at `at_counter`, grown by the lines that scrolled into
/// history since (`counter`); an aim of 0 (the live bottom) stays there.
#[inline]
fn grown_from_live(rows: usize, at_counter: u64, counter: u64) -> usize {
    if rows == 0 {
        return 0;
    }
    rows.saturating_add(usize::try_from(counter.saturating_sub(at_counter)).unwrap_or(usize::MAX))
}

impl DetachedReaderAim {
    /// The `display_offset` this aim names once the whole history (`sb` lines) is back.
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

/// A grid's off-screen scrollback ([`Grid::resize_offloading_scrollback`]).
#[must_use = "a detached scrollback store must be reflowed and re-attached, or history is lost"]
pub struct PendingScrollbackReflow {
    store: ScrollbackStorage,
    lazy_lines: Vec<Line>,
    /// The rows staged for the store at (re)detach, still raw ([`LazyBuffer::take_staged`]).
    lazy_staged: Box<LazyBuffer>,
    /// The ring's scrollback rows, materialized at detach ([`ReflowedScrollback::ring_out`]).
    ring_lines: Vec<Line>,
    new_cols: u16,
    /// The reader's scroll position at detach, restored on re-attach.
    prev_offset: usize,
    /// `GridStorage::reader_live_bottom_gen` at detach.
    detach_live_bottom_gen: u64,
    /// The scrollback-erase generation at detach.
    clear_gen: u64,
    /// Rewrap progress ([`reflow`](Self::reflow)).
    phase: ReflowPhase,
    /// `Some` from the first step until the store stream finishes.
    lifted_limits: Option<LiftedStoreLimits>,
    /// The history lines this job detached ([`Self::lines_done`]).
    lines_total: usize,
    /// Input lines the steps have read so far.
    lines_read: usize,
}

/// Where an incrementally-stepped rewrap currently is ([`ReflowStep`]).
enum ReflowPhase {
    /// The streaming rewrap.
    Stream {
        /// Input lines still unread at the store's front.
        store_input_left: usize,
        /// Next `lazy_lines` index to read once the store input is consumed.
        next_lazy: usize,
        /// The trailing logical line.
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
    /// The state every job starts in: the whole store front plus the lazy tail unread.
    fn start(store: &ScrollbackStorage) -> Self {
        ReflowPhase::Stream {
            store_input_left: store.line_count(),
            next_lazy: 0,
            carry: Vec::new(),
        }
    }
}

/// The store's eviction limits ([`PendingScrollbackReflow::reflow_step`]).
struct LiftedStoreLimits {
    line_limit: Option<usize>,
    memory_budget: usize,
}

/// The outcome of one [`PendingScrollbackReflow::reflow_step`] call.
#[must_use = "keep stepping InProgress (or re-attach Done), or history is lost"]
pub enum ReflowStep {
    /// More work remains.
    InProgress(PendingScrollbackReflow),
    /// Rewrap complete and re-filled into the store ([`Grid::reattach_reflowed_scrollback`]).
    Done(ReflowedScrollback),
}

/// A scrollback store already rewrapped to `new_cols` ([`Grid::reattach_reflowed_scrollback`]).
#[must_use = "a reflowed scrollback store must be re-attached, or history is lost"]
pub struct ReflowedScrollback {
    store: ScrollbackStorage,
    /// The ring history rewrapped to `new_cols` ([`Grid::reattach_ring_history`]).
    ring_out: Vec<Line>,
    new_cols: u16,
    prev_offset: usize,
    /// See [`PendingScrollbackReflow::detach_live_bottom_gen`].
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

    /// Input lines read so far, of [`Self::lines_total`].
    #[must_use]
    #[inline]
    pub fn lines_done(&self) -> usize {
        self.lines_read.min(self.lines_total)
    }

    /// The history lines this job detached ([`Self::line_count`]).
    #[must_use]
    #[inline]
    pub const fn lines_total(&self) -> usize {
        self.lines_total
    }

    /// The history lines the job holds now, input and output (tiered + lazy + ring), for logging.
    #[must_use]
    #[inline]
    pub fn line_count(&self) -> usize {
        self.store.line_count()
            + self.lazy_lines.len()
            + self.lazy_staged.len()
            + self.ring_lines.len()
    }

    /// The whole rewrap in one call.
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
    // COST: O(max_lines × cols) per step.
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
                let lazy_left = self.lazy_lines.len() - *next_lazy;
                let total_left = *store_input_left + lazy_left + self.lazy_staged.len();
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
                    // as the front offset crosses them), so the store never holds
                    // full input plus full output.
                    if let Err(error) = self.store.truncate_oldest(from_store) {
                        aterm_log::warn!("offload reflow: input front-truncation failed: {error}");
                    }
                    *store_input_left -= from_store;
                }
                let from_lazy = (read - from_store).min(lazy_left);
                for _ in 0..from_lazy {
                    carry.push(std::mem::replace(
                        &mut self.lazy_lines[*next_lazy],
                        Line::new(),
                    ));
                    *next_lazy += 1;
                }
                carry.extend(self.lazy_staged.drain_front(read - from_store - from_lazy));

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
                    // among the newly read lines; the trailing run stays carried
                    // (with no head, the whole chunk continues it).
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

/// Rewrap the completed runs of `carry` into `out`, keeping the trailing run carried.
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
    /// Resize like [`Grid::resize`].
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
        let lazy_staged = Box::new(self.storage.lazy_buffer.take_staged());
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
        self.storage
            .replay_carried_history
            .store(0, std::sync::atomic::Ordering::Relaxed);
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
        let lines_total = store.line_count() + lazy_staged.len() + ring_lines.len();
        Some(PendingScrollbackReflow {
            store,
            lazy_lines: Vec::new(),
            lazy_staged,
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

    /// Re-attach a store rewrapped off-thread ([`Self::resize_offloading_scrollback`]).
    pub fn reattach_reflowed_scrollback(&mut self, reflowed: ReflowedScrollback) {
        self.storage.scrollback_detached_for_reflow = false;
        self.close_reflow_window_cap_run();
        let reader_aim = self.storage.detached_reader_aim.take();
        let counter_at_reattach = self.storage.absolute_row_counter;

        if self.storage.scrollback.is_some() {
            self.drop_fill_debt();
            self.reattach_ring_history(reflowed.ring_out, reflowed.new_cols);
            self.reconcile_pending_scrollback_settings();
            return;
        }

        if reflowed.clear_gen != self.storage.scrollback_clear_gen {
            let mut store = reflowed.store;
            let _ = store.clear();
            self.storage.scrollback = Some(store);
            self.drop_fill_debt();
            self.settle_fill(0);
            self.drain_lazy_buffer();
            self.reconcile_pending_scrollback_settings();
            self.storage.display_offset = 0;
            self.storage.damage = Damage::Full;
            self.storage.content_gen += 1;
            self.storage.history_content_gen += 1;
            return;
        }

        self.storage.scrollback = Some(reflowed.store);
        // History returns in write order (the job's ring history, then the window's staged
        // output), and only then do the settings changed meanwhile replay: each setter settles
        // whatever is staged into the store, which ahead of the seat put the window's output
        // before the older ring history for good.
        self.reattach_ring_history(reflowed.ring_out, reflowed.new_cols);
        self.drain_lazy_buffer();
        self.reconcile_pending_scrollback_settings();
        if reflowed.new_cols == self.storage.cols {
            let pulled = self.pay_fill_debt();
            self.settle_fill(pulled);
        }
        let sb = self.storage.scrollback_lines();
        self.storage.deep_history_pullback_ordinal =
            self.storage.deep_history_pullback_ordinal.wrapping_add(1);
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
        self.storage.history_content_gen += 1;
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
        // The window's staged output leaves with the job's ring history before the settings
        // replay, so no setter settles it into the store ahead of those older lines; the run
        // then meets the settings now in force.
        let mut staged = Box::new(self.storage.lazy_buffer.take_staged());
        self.reconcile_pending_scrollback_settings();

        let (lazy_lines, lazy_staged) = if self.storage.store_retains_lines() {
            let mut ring_out = reflowed.ring_out;
            if let Some(store) = self.storage.scrollback.as_mut()
                && let Some(max) = store.line_limit()
            {
                let total = store
                    .line_count()
                    .saturating_add(ring_out.len() + staged.len());
                if total > max {
                    let excess = total - max;
                    let from_store = excess.min(store.line_count());
                    if from_store > 0 {
                        let _ = store.truncate_oldest(from_store);
                    }
                    let from_ring = (excess - from_store).min(ring_out.len());
                    ring_out.drain(..from_ring);
                    drop(staged.drain_front(excess - from_store - from_ring));
                }
            }
            (ring_out, staged)
        } else {
            self.storage.lazy_buffer.restore_staged(*staged);
            self.reattach_ring_history(reflowed.ring_out, reflowed.new_cols);
            self.drain_lazy_buffer();
            let rest = self.storage.lazy_buffer.drain_all().collect();
            (rest, Box::default())
        };

        let sb = self
            .storage
            .scrollback_lines()
            .saturating_add(lazy_lines.len() + lazy_staged.len());
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
        self.storage.history_content_gen += 1;
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
        self.storage
            .replay_carried_history
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.storage.staged_width_stale = false;
        self.storage.pending_scrollback_settings = Some(pending_settings);
        self.storage.detached_reader_aim = None;
        let phase = ReflowPhase::start(&store);
        let lines_total = store.line_count() + lazy_lines.len() + lazy_staged.len();
        Some(PendingScrollbackReflow {
            store,
            lazy_lines,
            lazy_staged,
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

    /// Return the job-carried ring history (rewrapped to `new_cols`) to the grid at re-attach.
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
                drop(ring_out.drain(..skip));
            }
        }
        if !ring_out.is_empty() {
            self.prepend_ring_scrollback_lines(ring_out, new_cols);
        }
    }

    /// [`Self::reattach_ring_history`] when the store keeps no line.
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

    /// Replay scrollback settings changed while the tiered store was detached. Each setter
    /// settles anything staged into the store, so a re-attach calls this once the window's
    /// staged output is seated after the job's ring history, or moved out with it.
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
                            None
                        }
                    }
                }
            }
        };
        if let Some(DetachedReaderAim::Above { .. } | DetachedReaderAim::FromTop { .. }) = next {
            self.set_display_offset_in_window(attached);
        }
        self.storage.detached_reader_aim = next;
    }

    /// Put the viewport at `offset`.
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

    /// True while a detach window is open.
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
        self.storage.history_content_gen += 1;
        debug_assert!(
            !self.storage.scrollback_detached_for_reflow,
            "abort_reflow_offload must close the detach window on every exit"
        );
    }
}

#[cfg(test)]
#[path = "scrollback_offload_tests.rs"]
mod tests;
