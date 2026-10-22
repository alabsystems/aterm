// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The seamless update's history carry, grid half: an in-session update hands
//! the successor each tab's full scrollback, through steps that share one
//! notion of "the same history". The outgoing process takes a
//! [`Grid::history_fence`] and exports through
//! [`Grid::history_lines_since_fence`] in short lock holds; the successor
//! reserves keys ([`Grid::reserve_older_history_keys`]) before anything reads
//! an absolute row, builds an [`OlderHistory`] off the lock, and places it with
//! [`Grid::attach_older_history`], which an ED3 or reset since the reserve
//! refuses.

use aterm_scrollback::{Line, Scrollback, ScrollbackStorage};

use super::Grid;

/// What a history export is fenced on: while two fences from the same grid
/// satisfy [`Self::holds_at`], every absolute row the older one covers names
/// the same line (eviction only drops the oldest and keeps survivors' keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryFence {
    /// `GridStorage::history_renumber_epoch` — any width reflow or unscroll
    /// advances it, and with it the text an absolute row names.
    pub renumber_epoch: u64,
    /// `GridStorage::scrollback_clear_gen` — ED3 and a full reset advance it.
    pub clear_gen: u64,
    /// `GridStorage::history_reveal_gen`: a rows-grow that handed history lines
    /// back to the screen advances it. A revealed line can be rewritten and
    /// pushed back under the same absolute key with other text, which neither
    /// the epoch nor `end` would show.
    pub reveal_gen: u64,
    /// The width every history line is wrapped at.
    pub cols: u16,
    /// Absolute row of the oldest retained history line.
    pub oldest: u64,
    /// Absolute row one past the newest history line, the top visible row
    /// (`Grid::base_y`). Lines pushed after the fence land at `end` and above.
    pub end: u64,
    /// The tiered store was out with an off-thread reflow when this was taken:
    /// the history is not all here, so no export may be fenced on it.
    pub detached: bool,
}

impl HistoryFence {
    /// How many history lines the fence spans.
    #[must_use]
    #[inline]
    pub const fn lines(&self) -> u64 {
        self.end.saturating_sub(self.oldest)
    }

    /// Whether `now`, a fence taken later from the same grid, still describes
    /// this fence's history: same epoch, same clear generation, no reveal
    /// since, same width, neither side detached, and the absolute counter has
    /// not gone backwards (a replaced grid starts it over).
    #[must_use]
    #[inline]
    pub const fn holds_at(&self, now: &HistoryFence) -> bool {
        !self.detached
            && !now.detached
            && self.renumber_epoch == now.renumber_epoch
            && self.clear_gen == now.clear_gen
            && self.reveal_gen == now.reveal_gen
            && self.cols == now.cols
            && now.end >= self.end
    }
}

/// The successor's fence on its own import: the scrollback-clear generation
/// of the grid whose keys [`Grid::reserve_older_history_keys`] reserved.
/// [`Grid::attach_older_history`] requires it and refuses once that grid's
/// scrollback was cleared since: the import lands on a worker after Commit,
/// while the adopted shell already runs, and an ED3 or reset in that window
/// means the user wants this history gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OlderHistoryClaim {
    clear_gen: u64,
}

/// Why [`Grid::history_lines_since_fence`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryFenceBroken {
    /// The history changed under the fence: a width reflow, an unscroll, a
    /// scrollback clear, a reset, or an off-thread reflow now holds it.
    Moved,
    /// The requested row was evicted since the fence (retention outran the
    /// reader of this history).
    Evicted,
}

impl std::fmt::Display for HistoryFenceBroken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Moved => "the history was rewrapped, cleared or reset after it was fenced",
            Self::Evicted => "retention evicted lines the export had not read yet",
        })
    }
}

/// Lines to place before a grid's oldest retained line, already built into a
/// tiered store: the expensive half of an import, done off any lock.
#[derive(Debug)]
pub struct OlderHistory {
    // Boxed: the store is a large value, and every refusal hands it back.
    store: Box<Scrollback>,
    lines: usize,
    cols: u16,
}

impl OlderHistory {
    /// Build `lines` (oldest first, wrapped at `from_cols`) for a grid `cols`
    /// wide: rewrapped when the widths differ (the successor's window may have
    /// settled at another width), then pushed into an unbounded store. No line
    /// limit or budget evicts here; the grid's own retention applies at
    /// [`Grid::attach_older_history`].
    // Cost: O(imported history), off any lock: its one caller is the
    // successor's history-import worker.
    #[must_use]
    pub fn build(lines: &[Line], from_cols: u16, cols: u16) -> Self {
        let mut store = Scrollback::with_defaults();
        store.set_line_limit(None);
        // `usize::MAX` cannot be over budget, so this never evicts; the result
        // is discarded because enforcement on an empty store cannot fail.
        let _ = store.set_memory_budget(usize::MAX);
        if from_cols == cols || lines.is_empty() {
            for line in lines {
                store.push_line(line.clone());
            }
        } else {
            for line in super::scrollback_reflow::reflow_scrollback_lines(lines, cols) {
                store.push_line(line);
            }
        }
        let count = store.line_count();
        Self {
            store: Box::new(store),
            lines: count,
            cols,
        }
    }

    /// How many lines the built store holds (after any rewrap).
    #[must_use]
    #[inline]
    pub const fn lines(&self) -> usize {
        self.lines
    }

    /// The width the lines are wrapped at.
    #[must_use]
    #[inline]
    pub const fn cols(&self) -> u16 {
        self.cols
    }
}

/// Why [`Grid::attach_older_history`] refused. Each gives the built history
/// back so the caller may rebuild or retry without re-reading its source.
#[derive(Debug)]
pub enum OlderHistoryRefusal {
    /// An off-thread reflow holds the tiered store; retry once it is back.
    ReflowInFlight(OlderHistory),
    /// The grid is no longer as wide as the history was built for.
    WidthMoved {
        /// The grid's width now.
        now: u16,
        /// The history, still at the width it was built for.
        older: OlderHistory,
    },
    /// The grid keeps no tiered store (a ring-only grid) to place history in
    /// front of.
    NoTieredStore(OlderHistory),
    /// The pane's scrollback was cleared (ED3, a reset) after the keys were
    /// reserved ([`OlderHistoryClaim`]), so the history stays cleared: the
    /// user erased what the import would have restored.
    Cleared(OlderHistory),
}

impl std::fmt::Display for OlderHistoryRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReflowInFlight(_) => formatter.write_str("a scrollback rewrap was in flight"),
            Self::WidthMoved { now, older } => write!(
                formatter,
                "the pane is {now} columns wide, not the {} the history was built for",
                older.cols
            ),
            Self::NoTieredStore(_) => {
                formatter.write_str("the pane keeps no in-memory history store")
            }
            Self::Cleared(_) => formatter.write_str("the pane's scrollback was cleared since"),
        }
    }
}

impl Grid {
    /// The fence a history export of this grid is taken on. A pure read.
    #[must_use]
    #[inline]
    pub fn history_fence(&self) -> HistoryFence {
        HistoryFence {
            renumber_epoch: self.storage.history_renumber_epoch,
            clear_gen: self.storage.scrollback_clear_gen,
            reveal_gen: self.storage.history_reveal_gen,
            cols: self.cols(),
            oldest: self.oldest_absolute_row(),
            end: self.oldest_absolute_row() + self.scrollback_lines() as u64,
            detached: self.storage.scrollback_detached_for_reflow,
        }
    }

    /// Up to `max` history lines starting at absolute row `from` and never past
    /// `fence.end`: the lines `fence` covers, oldest first, read while the
    /// fence holds. An empty `Ok` once `from` reaches `fence.end`.
    ///
    /// An undecodable line is an empty line, as in every other history
    /// projection, so absolute rows and line counts stay exact.
    // COST: O(max × cols), bounded by the caller's chunk, so an exporter walks a
    // deep history in short lock holds.
    pub fn history_lines_since_fence(
        &self,
        fence: &HistoryFence,
        from: u64,
        max: usize,
    ) -> Result<Vec<Line>, HistoryFenceBroken> {
        let now = self.history_fence();
        if !fence.holds_at(&now) {
            return Err(HistoryFenceBroken::Moved);
        }
        if from < now.oldest {
            return Err(HistoryFenceBroken::Evicted);
        }
        let to = fence.end.min(from.saturating_add(max as u64));
        let Some(count) = to.checked_sub(from) else {
            return Ok(Vec::new());
        };
        let first = usize::try_from(from - now.oldest).map_err(|_| HistoryFenceBroken::Moved)?;
        let count = usize::try_from(count).map_err(|_| HistoryFenceBroken::Moved)?;
        let mut lines = Vec::with_capacity(count);
        for index in first..first + count {
            match self.try_get_history_line(index) {
                Ok(Some(line)) => lines.push(line.into_owned()),
                Ok(None) | Err(_) => lines.push(Line::new()),
            }
        }
        Ok(lines)
    }

    /// Make room for `lines` keys below the oldest retained line, so history
    /// later placed in front of it ([`Self::attach_older_history`]) lands
    /// under unused keys and no retained or visible row's key moves.
    ///
    /// Only the shortfall is raised: the `oldest_absolute_row()` keys already
    /// free below the oldest row go first. A grid that continued the source's
    /// numbering (`Self::continue_absolute_numbering`) already has the imported
    /// lines' keys free below it, so nothing moves and restored shell marks keep
    /// naming their lines; a grid numbered from zero is raised by the whole
    /// import.
    ///
    /// Only for a grid just built from a checkpoint, before anything but the
    /// restore has read an absolute row. The rows below the oldest retained
    /// line read as evicted until the import fills them.
    #[must_use]
    pub fn reserve_older_history_keys(&mut self, lines: u64) -> OlderHistoryClaim {
        let shortfall = lines.saturating_sub(self.oldest_absolute_row());
        self.renumber_rows(self.storage.absolute_row_counter.saturating_add(shortfall));
        OlderHistoryClaim {
            clear_gen: self.storage.scrollback_clear_gen,
        }
    }

    /// Whether `claim` still admits an import here: this grid's scrollback has
    /// not been cleared since the reserve that minted it. A pure read, so an
    /// importer can check before reading and decoding a sidecar the attach
    /// would refuse.
    #[must_use]
    pub fn older_history_claim_holds(&self, claim: OlderHistoryClaim) -> bool {
        claim.clear_gen == self.storage.scrollback_clear_gen
    }

    /// Place `older` before the oldest line this grid retains, unless its
    /// scrollback was cleared since `claim` was minted
    /// ([`OlderHistoryRefusal::Cleared`]), then apply the grid's own retention
    /// (line limit and memory budget evict the oldest, as in the exporting
    /// process). Returns how many imported lines were retained.
    ///
    /// Retained lines keep their absolute keys when the counter was reserved
    /// ([`Self::reserve_older_history_keys`]); otherwise (or if a rewrap made
    /// the history longer than reserved) the counter is raised and that is
    /// announced like any renumbering: the epoch advances and host coordinates
    /// are invalidated. The epoch advances either way, since the cached search
    /// index assumes the oldest row only moves forward.
    // COST: O(tiered-history-lines) under the caller's lock: the grid's own
    // tiered lines are re-pushed behind the imported ones (right after an
    // update, only the screen carry's lines); `OlderHistory::build` paid for
    // the imported depth off the lock.
    pub fn attach_older_history(
        &mut self,
        older: OlderHistory,
        claim: OlderHistoryClaim,
    ) -> Result<usize, OlderHistoryRefusal> {
        // First: a cleared history stays cleared, whatever else holds (an
        // erase during an offloaded reflow bumps the generation too).
        if !self.older_history_claim_holds(claim) {
            return Err(OlderHistoryRefusal::Cleared(older));
        }
        if self.storage.scrollback_detached_for_reflow {
            return Err(OlderHistoryRefusal::ReflowInFlight(older));
        }
        if older.cols != self.cols() {
            return Err(OlderHistoryRefusal::WidthMoved {
                now: self.cols(),
                older,
            });
        }
        if !matches!(self.storage.scrollback, Some(ScrollbackStorage::Memory(_))) {
            return Err(OlderHistoryRefusal::NoTieredStore(older));
        }
        if older.lines == 0 {
            return Ok(0);
        }
        // Everything older than the ring is in the store once the lazy buffer
        // is settled, so the store's lines are exactly the ones to go behind.
        self.drain_lazy_buffer();
        let limit = self.scrollback_line_limit();
        let budget = self.scrollback_memory_budget();
        let before = self.scrollback_lines();
        let mut store = *older.store;
        if let Some(ScrollbackStorage::Memory(current)) = self.storage.scrollback.as_ref() {
            for index in 0..current.line_count() {
                match current.get_line(index) {
                    Ok(Some(line)) => store.push_line(line.into_owned()),
                    Ok(None) | Err(_) => store.push_line(Line::new()),
                }
            }
        }
        self.storage.scrollback = Some(ScrollbackStorage::Memory(store));
        // base_y (`counter - visible`, and with it every retained and visible
        // key) does not move; only the oldest row moves back, unless the counter
        // cannot cover the retained history and is raised, a renumbering.
        let needed = u64::from(self.storage.visible_rows) + self.scrollback_lines() as u64;
        if self.storage.absolute_row_counter < needed {
            self.renumber_rows(needed);
            self.invalidate_host_coordinates();
        }
        self.storage.history_renumber_epoch = self.storage.history_renumber_epoch.saturating_add(1);
        if let Some(budget) = budget {
            // An enforcement failure leaves the store over its budget, as the
            // store's own pushes would; the next drain enforces again.
            let _ = self.set_scrollback_memory_budget(budget);
        }
        self.set_scrollback_line_limit(limit);
        self.storage.mark_content_full();
        self.clamp_display_offset();
        Ok(self.scrollback_lines().saturating_sub(before))
    }
}

#[cfg(test)]
#[path = "history_carry_tests.rs"]
mod tests;
