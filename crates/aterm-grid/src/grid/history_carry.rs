// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grid scrollback export and import fences for seamless in-session updates.

use aterm_scrollback::{Line, Scrollback, ScrollbackStorage};

use super::Grid;

/// Fence snapshot validating that exported scrollback rows remain unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryFence {
    /// `GridStorage::history_renumber_epoch` at fence time.
    pub renumber_epoch: u64,
    /// `GridStorage::scrollback_clear_gen` at fence time.
    pub clear_gen: u64,
    /// `GridStorage::history_reveal_gen` at fence time.
    pub reveal_gen: u64,
    /// Column width the history lines are wrapped at.
    pub cols: u16,
    /// Absolute row of the oldest retained history line.
    pub oldest: u64,
    /// Absolute row one past the newest history line (`Grid::base_y`).
    pub end: u64,
    /// Whether the tiered store was detached for an off-thread reflow.
    pub detached: bool,
}

impl HistoryFence {
    /// Number of history lines spanned by this fence.
    #[must_use]
    #[inline]
    pub const fn lines(&self) -> u64 {
        self.end.saturating_sub(self.oldest)
    }

    /// Returns whether a later fence `now` still describes the same history.
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

/// Claim token recording the scrollback-clear generation when older-history keys were reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OlderHistoryClaim {
    clear_gen: u64,
}

/// Reason [`Grid::history_lines_since_fence`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryFenceBroken {
    /// The history changed or detached under the fence.
    Moved,
    /// The requested row was evicted since the fence was taken.
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

/// Pre-built tiered scrollback store ready to attach before the oldest retained line.
#[derive(Debug)]
pub struct OlderHistory {
    store: Box<Scrollback>,
    lines: usize,
    cols: u16,
}

impl OlderHistory {
    /// Builds an [`OlderHistory`] from `lines` (wrapped at `from_cols`) for a grid `cols` wide.
    #[must_use]
    pub fn build(lines: &[Line], from_cols: u16, cols: u16) -> Self {
        let mut store = Scrollback::with_defaults();
        store.set_line_limit(None);
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

    /// Number of lines in the built store.
    #[must_use]
    #[inline]
    pub const fn lines(&self) -> usize {
        self.lines
    }

    /// Column width the lines are wrapped at.
    #[must_use]
    #[inline]
    pub const fn cols(&self) -> u16 {
        self.cols
    }
}

/// Reason [`Grid::attach_older_history`] refused, returning the unattached [`OlderHistory`].
#[derive(Debug)]
pub enum OlderHistoryRefusal {
    /// An off-thread reflow currently holds the tiered store.
    ReflowInFlight(OlderHistory),
    /// The grid width no longer matches the built history.
    WidthMoved {
        /// Current grid width.
        now: u16,
        /// Unattached history at its built width.
        older: OlderHistory,
    },
    /// The grid has no tiered store.
    NoTieredStore(OlderHistory),
    /// The pane's scrollback was cleared since `claim` was minted.
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
    /// Takes a [`HistoryFence`] snapshot of the current history state.
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

    /// Copies up to `max` history lines starting at absolute row `from` within `fence`.
    // COST: O(max × cols), bounded by the caller's chunk.
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

    /// Reserves `lines` key slots below the oldest retained line for a subsequent import.
    #[must_use]
    pub fn reserve_older_history_keys(&mut self, lines: u64) -> OlderHistoryClaim {
        let shortfall = lines.saturating_sub(self.oldest_absolute_row());
        self.renumber_rows(self.storage.absolute_row_counter.saturating_add(shortfall));
        OlderHistoryClaim {
            clear_gen: self.storage.scrollback_clear_gen,
        }
    }

    /// Returns whether `claim` is still valid (scrollback was not cleared since reservation).
    #[must_use]
    pub const fn older_history_claim_holds(&self, claim: OlderHistoryClaim) -> bool {
        claim.clear_gen == self.storage.scrollback_clear_gen
    }

    /// Attaches `older` before the oldest retained line if `claim` and grid state permit.
    // COST: O(tiered-history-lines) under the caller's lock.
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
