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
mod tests {
    use super::*;

    fn write_lines(t: &mut Terminal, prefix: &str, count: usize) {
        for i in 0..count {
            t.process(format!("{prefix}{i}\r\n").as_bytes());
        }
    }

    fn history(t: &Terminal) -> Vec<String> {
        let grid = t.main_grid();
        (0..grid.scrollback_lines())
            .map(|i| {
                grid.get_history_line(i)
                    .map(|l| l.to_string().trim_end().to_string())
                    .unwrap_or_default()
            })
            .collect()
    }

    fn export(t: &Terminal) -> Vec<Line> {
        let fence = t.history_fence();
        let mut out = Vec::new();
        let mut cursor = fence.oldest;
        while cursor < fence.end {
            let lines = t
                .history_lines_since_fence(&fence, cursor, 64)
                .expect("the fence holds");
            cursor += lines.len() as u64;
            out.extend(lines);
        }
        out
    }

    #[test]
    fn the_saved_primarys_history_is_what_an_alt_screen_terminal_exports() {
        let mut t = Terminal::new(5, 30);
        write_lines(&mut t, "shell", 40);
        let before = history(&t);
        assert!(!before.is_empty());
        t.process(b"\x1b[?1049h");
        write_lines(&mut t, "tui", 20);
        let exported: Vec<String> = export(&t)
            .iter()
            .map(|l| l.to_string().trim_end().to_string())
            .collect();
        assert_eq!(
            exported, before,
            "the shell's history, not the alt screen's (which keeps none)"
        );
    }

    /// An ED3 the adopted shell sends after Commit, before the import lands,
    /// refuses the import; so does a RIS. Negative control: an ED2 leaves the
    /// claim standing.
    #[test]
    fn a_scrollback_the_shell_clears_before_the_import_stays_cleared() {
        let mut source = Terminal::new(5, 30);
        write_lines(&mut source, "secret", 50);
        let lines = export(&source);
        for clear in [&b"\x1b[3J"[..], b"\x1bc"] {
            let mut t =
                Terminal::with_scrollback(5, 30, 8, crate::scrollback::Scrollback::with_defaults());
            write_lines(&mut t, "carried", 8);
            let claim = t.reserve_older_history_keys(lines.len() as u64);
            t.process(clear);
            assert!(!t.older_history_claim_holds(claim));
            assert!(matches!(
                t.attach_older_history(OlderHistory::build(&lines, 30, t.history_cols()), claim),
                Err(OlderHistoryRefusal::Cleared(_))
            ));
            assert!(history(&t).is_empty(), "{clear:?} stays cleared");
        }
        let mut t =
            Terminal::with_scrollback(5, 30, 8, crate::scrollback::Scrollback::with_defaults());
        write_lines(&mut t, "carried", 8);
        let claim = t.reserve_older_history_keys(lines.len() as u64);
        t.process(b"\x1b[2J");
        assert!(
            t.older_history_claim_holds(claim),
            "ED2 clears the screen only"
        );
        assert_eq!(
            t.attach_older_history(OlderHistory::build(&lines, 30, t.history_cols()), claim)
                .expect("lands"),
            lines.len()
        );
    }

    #[test]
    fn an_import_lands_in_the_saved_primary_under_an_alt_screen() {
        let mut source = Terminal::new(5, 30);
        write_lines(&mut source, "old", 50);
        let lines = export(&source);

        // The successor's history grid is tiered: `restore_checkpoint` builds
        // it that way.
        let mut t =
            Terminal::with_scrollback(5, 30, 8, crate::scrollback::Scrollback::with_defaults());
        write_lines(&mut t, "carried", 8);
        t.process(b"\x1b[?1049h");
        let carried = history(&t);
        let claim = t.reserve_older_history_keys(lines.len() as u64);
        let retained = t
            .attach_older_history(OlderHistory::build(&lines, 30, t.history_cols()), claim)
            .expect("attached");
        assert_eq!(retained, lines.len());
        let mut expected: Vec<String> = lines
            .iter()
            .map(|l| l.to_string().trim_end().to_string())
            .collect();
        expected.extend(carried);
        assert_eq!(history(&t), expected);
        t.process(b"\x1b[?1049l");
        assert_eq!(
            history(&t),
            expected,
            "the history is the shell's when the program exits"
        );
    }
}
