// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Terminal-level wrappers for `aterm_grid::grid::history_carry` targeting [`Terminal::main_grid`].

use aterm_grid::{
    HistoryFence, HistoryFenceBroken, OlderHistory, OlderHistoryClaim, OlderHistoryRefusal,
};

use super::Terminal;
use crate::grid::Grid;
use crate::scrollback::Line;

impl Terminal {
    /// Mutable counterpart of [`Self::main_grid`].
    #[inline]
    pub(super) const fn history_grid_mut(&mut self) -> &mut Grid {
        if self.modes.alternate_screen
            && let Some(primary) = self.alt_grid.as_mut()
        {
            return primary;
        }
        &mut self.grid
    }

    /// Captures a [`HistoryFence`] on the main grid.
    #[inline]
    #[must_use]
    pub fn history_fence(&self) -> HistoryFence {
        self.main_grid().history_fence()
    }

    /// Up to `max` history lines from absolute row `from`, while `fence`
    /// holds ([`Grid::history_lines_since_fence`]).
    #[inline]
    pub fn history_lines_since_fence(
        &self,
        fence: &HistoryFence,
        from: u64,
        max: usize,
    ) -> Result<Vec<Line>, HistoryFenceBroken> {
        self.main_grid().history_lines_since_fence(fence, from, max)
    }

    /// Column width of the main grid for [`OlderHistory::build`].
    #[inline]
    #[must_use]
    pub const fn history_cols(&self) -> u16 {
        self.main_grid().cols()
    }

    /// Reserves absolute-row keys below the oldest history line for `lines` imported later
    /// ([`Grid::reserve_older_history_keys`]); call immediately after [`Self::restore_checkpoint`].
    #[inline]
    #[must_use]
    pub fn reserve_older_history_keys(&mut self, lines: u64) -> OlderHistoryClaim {
        self.history_grid_mut().reserve_older_history_keys(lines)
    }

    /// Whether `claim` still admits an import ([`Grid::older_history_claim_holds`]).
    #[inline]
    #[must_use]
    pub const fn older_history_claim_holds(&self, claim: OlderHistoryClaim) -> bool {
        self.main_grid().older_history_claim_holds(claim)
    }

    /// Attaches `older` before the oldest history line ([`Grid::attach_older_history`]),
    /// invalidating the search index and scroll state.
    pub fn attach_older_history(
        &mut self,
        older: OlderHistory,
        claim: OlderHistoryClaim,
    ) -> Result<usize, OlderHistoryRefusal> {
        let retained = self.history_grid_mut().attach_older_history(older, claim)?;
        self.release_search_index();
        self.content_scroll_state.invalidate();
        Ok(retained)
    }
}

#[cfg(test)]
#[path = "history_carry_tests.rs"]
mod tests;
