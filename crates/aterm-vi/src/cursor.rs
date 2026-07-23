// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Vi mode cursor: position tracking and basic motion execution.
//!
//! The cursor applies "simple" motions (up/down/left/right, H/M/L, 0/$)
//! that only need grid dimensions. Complex motions (word, bracket,
//! paragraph, search) require grid content and are dispatched by
//! [`super::ViMode`] through [`BufferAccess`](aterm_types::BufferAccess).

use super::types::{ViBoundary, ViMotion, ViPoint};

/// Vi mode cursor state.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub struct ViModeCursor {
    /// Current cursor position.
    pub point: ViPoint,
}

impl ViModeCursor {
    /// Create a cursor at the given point.
    #[must_use]
    pub fn new(point: ViPoint) -> Self {
        Self { point }
    }

    /// Execute a basic motion.
    ///
    /// `visible_rows` and `cols` are the grid's visible dimensions.
    /// `topmost_line` is the smallest valid line value (negative =
    /// scrollback). `bottommost_line` is the largest valid line.
    ///
    /// Returns the updated cursor. Complex motions (word/bracket/
    /// paragraph/search/mark) are no-ops here — they must be handled
    /// by the caller with grid access.
    #[must_use]
    pub fn motion(
        mut self,
        visible_rows: u16,
        cols: u16,
        topmost_line: i32,
        bottommost_line: i32,
        motion: ViMotion,
        boundary: ViBoundary,
    ) -> Self {
        let last_col = cols.saturating_sub(1);

        // All the saturating ops below are exact under their guards:
        // `x > lo` implies `x > MIN` so `x - 1` cannot underflow, and
        // `x < hi` implies `x < MAX` so `x + 1` cannot overflow. They
        // only discharge the hardened-boundary L0 arithmetic obligations.
        match motion {
            ViMotion::Up => {
                if self.point.line > topmost_line {
                    self.point.line = self.point.line.saturating_sub(1);
                }
            }
            ViMotion::Down => {
                if self.point.line < bottommost_line {
                    self.point.line = self.point.line.saturating_add(1);
                }
            }
            ViMotion::Left => {
                if self.point.col > 0 {
                    self.point.col = self.point.col.saturating_sub(1);
                } else if boundary == ViBoundary::None && self.point.line > topmost_line {
                    self.point.line = self.point.line.saturating_sub(1);
                    self.point.col = last_col;
                }
            }
            ViMotion::Right => {
                if self.point.col < last_col {
                    self.point.col = self.point.col.saturating_add(1);
                } else if boundary == ViBoundary::None && self.point.line < bottommost_line {
                    self.point.line = self.point.line.saturating_add(1);
                    self.point.col = 0;
                }
            }
            ViMotion::First => {
                self.point.col = 0;
            }
            ViMotion::Last => {
                self.point.col = last_col;
            }
            ViMotion::High => {
                // Top of visible area (line 0, not scrollback).
                self.point.line = 0;
            }
            ViMotion::Middle => {
                self.point.line = i32::from(visible_rows) / 2;
            }
            ViMotion::Low => {
                // saturating_sub is exact: `i32::from(u16)` is >= 0.
                self.point.line = i32::from(visible_rows).saturating_sub(1);
            }

            // Complex motions — require grid content. No-op here.
            ViMotion::FirstOccupied
            | ViMotion::SemanticLeft
            | ViMotion::SemanticRight
            | ViMotion::SemanticLeftEnd
            | ViMotion::SemanticRightEnd
            | ViMotion::WordLeft
            | ViMotion::WordRight
            | ViMotion::WordLeftEnd
            | ViMotion::WordRightEnd
            | ViMotion::Bracket
            | ViMotion::ParagraphUp
            | ViMotion::ParagraphDown
            | ViMotion::SearchNext
            | ViMotion::SearchPrevious
            | ViMotion::GotoMark(_)
            | ViMotion::GotoMarkLine(_) => {}
        }

        // Clamp to grid bounds.
        self.point.line = self.point.line.clamp(topmost_line, bottommost_line);
        self.point.col = self.point.col.min(last_col);

        self
    }

    /// Scroll the cursor position by `delta` lines.
    ///
    /// Positive = content scrolls up (cursor line increases),
    /// negative = content scrolls down (cursor line decreases).
    #[must_use]
    pub fn scroll(mut self, topmost_line: i32, bottommost_line: i32, delta: i32) -> Self {
        // saturating_add: identical for every real grid (line and delta are
        // bounded by grid/scrollback sizes, nowhere near i32 limits); on the
        // unreachable overflow path saturation followed by the clamp still
        // yields the nearest grid boundary.
        self.point.line = self
            .point
            .line
            .saturating_add(delta)
            .clamp(topmost_line, bottommost_line);
        self
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor_at(line: i32, col: u16) -> ViModeCursor {
        ViModeCursor::new(ViPoint::new(line, col))
    }

    // Typical 24×80 terminal: lines 0..23, cols 0..79.
    const ROWS: u16 = 24;
    const COLS: u16 = 80;
    const TOP: i32 = 0;
    const BOT: i32 = 23;

    #[test]
    fn motion_up() {
        let c = cursor_at(5, 10).motion(ROWS, COLS, TOP, BOT, ViMotion::Up, ViBoundary::Grid);
        assert_eq!(c.point, ViPoint::new(4, 10));
    }

    #[test]
    fn motion_down() {
        let c = cursor_at(5, 10).motion(ROWS, COLS, TOP, BOT, ViMotion::Down, ViBoundary::Grid);
        assert_eq!(c.point, ViPoint::new(6, 10));
    }

    #[test]
    fn motion_left() {
        let c = cursor_at(5, 10).motion(ROWS, COLS, TOP, BOT, ViMotion::Left, ViBoundary::Grid);
        assert_eq!(c.point, ViPoint::new(5, 9));
    }

    #[test]
    fn motion_right() {
        let c = cursor_at(5, 10).motion(ROWS, COLS, TOP, BOT, ViMotion::Right, ViBoundary::Grid);
        assert_eq!(c.point, ViPoint::new(5, 11));
    }

    #[test]
    fn motion_first_last() {
        let c = cursor_at(5, 40).motion(ROWS, COLS, TOP, BOT, ViMotion::First, ViBoundary::Grid);
        assert_eq!(c.point.col, 0);

        let c = cursor_at(5, 40).motion(ROWS, COLS, TOP, BOT, ViMotion::Last, ViBoundary::Grid);
        assert_eq!(c.point.col, 79);
    }

    #[test]
    fn motion_high_middle_low() {
        let h = cursor_at(5, 10).motion(ROWS, COLS, TOP, BOT, ViMotion::High, ViBoundary::Grid);
        assert_eq!(h.point.line, 0);

        let m = cursor_at(5, 10).motion(ROWS, COLS, TOP, BOT, ViMotion::Middle, ViBoundary::Grid);
        assert_eq!(m.point.line, 12); // 24 / 2

        let l = cursor_at(5, 10).motion(ROWS, COLS, TOP, BOT, ViMotion::Low, ViBoundary::Grid);
        assert_eq!(l.point.line, 23);
    }

    #[test]
    fn motion_up_at_top_clamped() {
        let c = cursor_at(0, 5).motion(ROWS, COLS, TOP, BOT, ViMotion::Up, ViBoundary::Grid);
        assert_eq!(c.point.line, 0);
    }

    #[test]
    fn motion_down_at_bottom_clamped() {
        let c = cursor_at(23, 5).motion(ROWS, COLS, TOP, BOT, ViMotion::Down, ViBoundary::Grid);
        assert_eq!(c.point.line, 23);
    }

    #[test]
    fn motion_left_at_edge_grid_boundary() {
        let c = cursor_at(5, 0).motion(ROWS, COLS, TOP, BOT, ViMotion::Left, ViBoundary::Grid);
        assert_eq!(c.point, ViPoint::new(5, 0));
    }

    #[test]
    fn motion_left_at_edge_wraps_no_boundary() {
        let c = cursor_at(5, 0).motion(ROWS, COLS, TOP, BOT, ViMotion::Left, ViBoundary::None);
        assert_eq!(c.point, ViPoint::new(4, 79));
    }

    #[test]
    fn motion_right_at_edge_wraps_no_boundary() {
        let c = cursor_at(5, 79).motion(ROWS, COLS, TOP, BOT, ViMotion::Right, ViBoundary::None);
        assert_eq!(c.point, ViPoint::new(6, 0));
    }

    #[test]
    fn motion_with_scrollback() {
        // Scrollback extends to line -100.
        let c = cursor_at(0, 5).motion(ROWS, COLS, -100, BOT, ViMotion::Up, ViBoundary::Grid);
        assert_eq!(c.point.line, -1);
    }

    #[test]
    fn scroll_up_and_down() {
        let c = cursor_at(10, 5).scroll(TOP, BOT, 5);
        assert_eq!(c.point.line, 15);

        let c = cursor_at(10, 5).scroll(TOP, BOT, -5);
        assert_eq!(c.point.line, 5);
    }

    #[test]
    fn scroll_clamped_to_bounds() {
        let c = cursor_at(20, 5).scroll(TOP, BOT, 100);
        assert_eq!(c.point.line, 23);

        let c = cursor_at(5, 5).scroll(TOP, BOT, -100);
        assert_eq!(c.point.line, 0);
    }
}
