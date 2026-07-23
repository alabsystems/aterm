// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Vi mode navigation for the terminal.
//!
//! Provides vim-style keyboard navigation (cursor movement, marks,
//! inline search) as a state machine owned by the terminal.
//!
//! # Architecture
//!
//! - [`types`] — enums and value types (no grid dependency).
//! - `cursor` — cursor position + basic motions (dimensions only).
//! - [`BufferAccess`] — trait for reading buffer
//!   content (implemented by Grid). Defined in `aterm-types`.
//! - [`search`] — search match navigation for n/N motions.
//! - [`ViMode`] — top-level state: active flag, cursor, marks, search.
//!
//! Complex motions (word, bracket, paragraph) use
//! [`BufferAccess`] and are implemented in the
//! [`navigation`] and [`word`] submodules.
//! Search motions (n/N) use [`ViSearchState`] which is populated by
//! the caller from whatever search engine is available.

#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::all)]

mod cursor;
pub(crate) mod navigation;
pub(crate) mod search;
pub(crate) mod types;
mod visual;
pub(crate) mod word;

use aterm_types::BufferAccess;

pub use cursor::ViModeCursor;
pub use search::ViSearchState;
pub use types::{
    InlineSearchKind, InlineSearchState, ViBoundary, ViDirection, ViMarks, ViMotion, ViPoint,
    ViVisualType,
};

/// Read a character from the buffer, returning space for out-of-bounds.
///
/// Bridges `BufferAccess::char_at` (which returns `Option<char>`) to the
/// vi convention of treating missing cells as spaces.
pub(crate) fn cell_char(grid: &dyn BufferAccess, point: ViPoint) -> char {
    grid.char_at(point.line, point.col).unwrap_or(' ')
}

/// Find the column of the first non-blank character on a line.
///
/// Returns 0 if the line is entirely blank.
fn first_non_blank_col(grid: &dyn BufferAccess, line: i32) -> u16 {
    // Scrollback rows (line < 0) are expensive to read cell-by-cell: each
    // `char_at` re-materializes the whole row. Fetch the row once via
    // `line_text` (one char per cell, same resolution as `char_at`) and
    // scan it in a single pass — the char index equals the column index.
    if line < 0 {
        let Some(text) = grid.line_text(line) else {
            return 0;
        };
        for (col, ch) in text.chars().enumerate() {
            if ch != ' ' && ch != '\t' && ch != '\0' {
                return u16::try_from(col).unwrap_or(u16::MAX);
            }
        }
        return 0;
    }
    let line_len = grid.line_len(line);
    for c in 0..line_len {
        let ch = cell_char(grid, ViPoint::new(line, c));
        if ch != ' ' && ch != '\t' && ch != '\0' {
            return c;
        }
    }
    0
}

// ---------------------------------------------------------------------------
// ViMode state
// ---------------------------------------------------------------------------

/// Top-level vi mode state, owned by Terminal.
///
/// When `active` is false the cursor and other state are preserved but
/// have no effect on rendering or input handling.
#[derive(Debug, Clone)]
pub struct ViMode {
    /// Whether vi mode is currently active.
    active: bool,
    /// Cursor position.
    cursor: ViModeCursor,
    /// Named marks.
    marks: ViMarks,
    /// Last inline character search (for `;`/`,` repeat).
    inline_search: Option<InlineSearchState>,
    /// Semantic word separator characters for w/b/e/ge motions.
    separators: String,
    /// Search match positions for n/N navigation.
    search: ViSearchState,
    /// Visual selection anchor point (set when visual mode starts).
    visual_anchor: Option<ViPoint>,
    /// Type of visual selection (char/line/block), or `None` if not in visual mode.
    visual_type: Option<ViVisualType>,
}

impl Default for ViMode {
    fn default() -> Self {
        Self {
            active: false,
            cursor: ViModeCursor::default(),
            marks: ViMarks::default(),
            inline_search: None,
            separators: navigation::DEFAULT_SEPARATORS.to_string(),
            search: ViSearchState::default(),
            visual_anchor: None,
            visual_type: None,
        }
    }
}

impl ViMode {
    /// Create a new inactive `ViMode`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether vi mode is active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Get the current vi cursor.
    #[must_use]
    pub fn cursor(&self) -> ViModeCursor {
        self.cursor
    }

    /// Get the current cursor position.
    #[must_use]
    pub fn cursor_point(&self) -> ViPoint {
        self.cursor.point
    }

    /// Access the marks.
    #[must_use]
    pub fn marks(&self) -> &ViMarks {
        &self.marks
    }

    /// Mutable access to marks.
    pub fn marks_mut(&mut self) -> &mut ViMarks {
        &mut self.marks
    }

    /// Get the last inline search state.
    #[must_use]
    pub fn inline_search(&self) -> Option<InlineSearchState> {
        self.inline_search
    }

    /// Access the search state.
    #[must_use]
    pub fn search(&self) -> &ViSearchState {
        &self.search
    }

    /// Mutable access to the search state.
    pub fn search_mut(&mut self) -> &mut ViSearchState {
        &mut self.search
    }

    /// Set custom semantic word separator characters.
    pub fn set_separators(&mut self, separators: &str) {
        self.separators = separators.to_string();
    }

    /// Get the current semantic word separators.
    #[must_use]
    pub fn separators(&self) -> &str {
        &self.separators
    }

    /// Toggle vi mode on/off.
    ///
    /// When toggling on, the cursor is placed at the terminal cursor
    /// position (passed as `terminal_cursor`). When toggling off, the
    /// cursor position is preserved for next activation and any active
    /// visual selection is cancelled.
    pub fn toggle(&mut self, terminal_cursor: ViPoint) {
        self.active = !self.active;
        if self.active {
            self.cursor = ViModeCursor::new(terminal_cursor);
        } else {
            self.cancel_visual();
        }
    }

    /// Activate vi mode at the given terminal cursor position.
    pub fn activate(&mut self, terminal_cursor: ViPoint) {
        if !self.active {
            self.active = true;
            self.cursor = ViModeCursor::new(terminal_cursor);
        }
    }

    /// Deactivate vi mode.
    pub fn deactivate(&mut self) {
        self.active = false;
        self.cancel_visual();
    }

    /// Execute a basic motion (no grid content needed).
    ///
    /// For motions that require grid content (word, bracket, paragraph,
    /// search, marks), use [`Self::motion_with_grid`] instead. Those motions
    /// are silently ignored here.
    pub fn motion(
        &mut self,
        visible_rows: u16,
        cols: u16,
        topmost_line: i32,
        bottommost_line: i32,
        motion: ViMotion,
        boundary: ViBoundary,
    ) {
        if !self.active {
            return;
        }
        self.cursor = self.cursor.motion(
            visible_rows,
            cols,
            topmost_line,
            bottommost_line,
            motion,
            boundary,
        );
    }

    /// Execute any motion with buffer access.
    ///
    /// Basic motions are dispatched to the cursor directly. Complex
    /// motions (word, bracket, paragraph, inline search) use the buffer
    /// to read cell content via the [`navigation`] module.
    pub fn motion_with_grid(
        &mut self,
        grid: &dyn BufferAccess,
        motion: ViMotion,
        boundary: ViBoundary,
    ) {
        if !self.active {
            return;
        }

        let visible_rows = grid.visible_rows();
        let cols = grid.cols();
        // saturating_neg: identical for every real grid (total_lines >= 0).
        let topmost = grid.total_lines().saturating_neg();
        // saturating_sub is exact: `i32::from(u16)` is >= 0.
        let bottommost = i32::from(visible_rows).saturating_sub(1);
        let cur = self.cursor.point;
        let sep = &self.separators;

        match motion {
            // --- Mark motions (need marks state) ---
            ViMotion::GotoMark(ch) => {
                if let Some(target) = self.marks.get(ch) {
                    self.cursor.point = target;
                    self.cursor.point.line = self.cursor.point.line.clamp(topmost, bottommost);
                    self.cursor.point.col = self.cursor.point.col.min(cols.saturating_sub(1));
                }
            }
            ViMotion::GotoMarkLine(ch) => {
                if let Some(target) = self.marks.get(ch) {
                    self.cursor.point.line = target.line.clamp(topmost, bottommost);
                    self.cursor.point.col = first_non_blank_col(grid, self.cursor.point.line);
                }
            }

            // --- First non-blank (^) ---
            //
            // Double-tap behavior matching upstream alacritty:
            // 1. First call moves to first non-blank on the current visual row.
            // 2. Second call (cursor already at first non-blank) on a wrapped
            //    continuation row walks backward to the logical line start and
            //    moves to its first non-blank.
            ViMotion::FirstOccupied => {
                let line = cur.line;
                let first_col = first_non_blank_col(grid, line);
                if cur.col == first_col && grid.is_line_wrapped(line) && line > topmost {
                    // saturating_subs are exact: both are guarded by
                    // `> topmost`, so the operand exceeds i32::MIN.
                    let mut start = line.saturating_sub(1);
                    while start > topmost && grid.is_line_wrapped(start) {
                        start = start.saturating_sub(1);
                    }
                    self.cursor.point.line = start;
                    self.cursor.point.col = first_non_blank_col(grid, start);
                } else {
                    self.cursor.point.col = first_col;
                }
            }

            // --- Semantic word motions (w/b/e/ge) ---
            ViMotion::SemanticRight => {
                self.cursor.point = word::semantic_word_right(grid, cur, sep);
            }
            ViMotion::SemanticLeft => {
                self.cursor.point = word::semantic_word_left(grid, cur, sep);
            }
            ViMotion::SemanticRightEnd => {
                self.cursor.point = word::semantic_word_right_end(grid, cur, sep);
            }
            ViMotion::SemanticLeftEnd => {
                self.cursor.point = word::semantic_word_left_end(grid, cur, sep);
            }

            // --- Whitespace word motions (W/B/E/gE) ---
            ViMotion::WordRight => {
                self.cursor.point = word::whitespace_word_right(grid, cur);
            }
            ViMotion::WordLeft => {
                self.cursor.point = word::whitespace_word_left(grid, cur);
            }
            ViMotion::WordRightEnd => {
                self.cursor.point = word::whitespace_word_right_end(grid, cur);
            }
            ViMotion::WordLeftEnd => {
                self.cursor.point = word::whitespace_word_left_end(grid, cur);
            }

            // --- Bracket matching (%) ---
            ViMotion::Bracket => {
                if let Some(target) = navigation::bracket_match(grid, cur) {
                    self.cursor.point = target;
                }
            }

            // --- Paragraph motions ({/}) ---
            ViMotion::ParagraphUp => {
                self.cursor.point = navigation::paragraph_up(grid, cur);
            }
            ViMotion::ParagraphDown => {
                self.cursor.point = navigation::paragraph_down(grid, cur);
            }

            // --- Search (n/N) ---
            ViMotion::SearchNext => {
                if let Some(target) = self.search.focus_next(cur) {
                    self.cursor.point = target;
                    self.cursor.point.line = self.cursor.point.line.clamp(topmost, bottommost);
                    self.cursor.point.col = self.cursor.point.col.min(cols.saturating_sub(1));
                }
            }
            ViMotion::SearchPrevious => {
                if let Some(target) = self.search.focus_prev(cur) {
                    self.cursor.point = target;
                    self.cursor.point.line = self.cursor.point.line.clamp(topmost, bottommost);
                    self.cursor.point.col = self.cursor.point.col.min(cols.saturating_sub(1));
                }
            }

            // --- Basic motions — delegate to cursor ---
            _ => {
                self.cursor =
                    self.cursor
                        .motion(visible_rows, cols, topmost, bottommost, motion, boundary);
            }
        }
    }

    /// Execute an inline character search (f/F/t/T).
    ///
    /// Stores the search state for repeat with `;`/`,` and moves
    /// the cursor to the result. Returns `true` if a match was found.
    pub fn inline_search_execute(
        &mut self,
        grid: &dyn BufferAccess,
        needle: char,
        kind: InlineSearchKind,
    ) -> bool {
        if !self.active {
            return false;
        }

        self.inline_search = Some(InlineSearchState { char: needle, kind });
        self.perform_inline_search(grid, needle, kind)
    }

    /// Repeat the last inline search in the same direction (`;`).
    pub fn inline_search_repeat(&mut self, grid: &dyn BufferAccess) -> bool {
        if !self.active {
            return false;
        }
        let Some(state) = self.inline_search else {
            return false;
        };
        self.perform_inline_search(grid, state.char, state.kind)
    }

    /// Repeat the last inline search in the reverse direction (`,`).
    pub fn inline_search_repeat_reverse(&mut self, grid: &dyn BufferAccess) -> bool {
        if !self.active {
            return false;
        }
        let Some(state) = self.inline_search else {
            return false;
        };
        self.perform_inline_search(grid, state.char, state.kind.reversed())
    }

    /// Internal: execute an inline search and move cursor.
    fn perform_inline_search(
        &mut self,
        grid: &dyn BufferAccess,
        needle: char,
        kind: InlineSearchKind,
    ) -> bool {
        let cur = self.cursor.point;
        let result = match kind.direction() {
            ViDirection::Right => navigation::inline_search_right(grid, cur, needle),
            ViDirection::Left => navigation::inline_search_left(grid, cur, needle),
        };

        if let Some(mut target) = result {
            // For "till" variants, step one position back from the match.
            if kind.is_till() {
                match kind.direction() {
                    ViDirection::Right => {
                        if target.col > 0 {
                            target.col -= 1;
                        }
                    }
                    ViDirection::Left => {
                        target.col = target
                            .col
                            .saturating_add(1)
                            .min(grid.cols().saturating_sub(1));
                    }
                }
            }
            self.cursor.point = target;
            true
        } else {
            false
        }
    }

    /// Set a mark at the current cursor position.
    ///
    /// Returns `true` if the mark character is valid.
    pub fn set_mark(&mut self, mark: char) -> bool {
        self.marks.set(mark, self.cursor.point)
    }

    /// Record an inline search.
    pub fn set_inline_search(&mut self, state: InlineSearchState) {
        self.inline_search = Some(state);
    }

    /// Scroll the cursor by `delta` lines.
    pub fn scroll(&mut self, topmost_line: i32, bottommost_line: i32, delta: i32) {
        if !self.active {
            return;
        }
        self.cursor = self.cursor.scroll(topmost_line, bottommost_line, delta);
    }
}

#[cfg(test)]
mod test_utils;
#[cfg(test)]
#[path = "vi_mode_tests.rs"]
mod tests;
