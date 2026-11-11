// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Replayable ANSI serialization of a [`Terminal`].

use std::fmt::Write as _;

use super::handler::handler_decrsps::{CursorRestore, write_cursor_restore};
use super::{SavedCursorState, Terminal};
use crate::grid::{Cursor, Grid};
use aterm_grid::ScrollRegion;
use aterm_types::CharacterSetState;

#[cfg(test)]
#[path = "serialize_ansi_wrap_tests.rs"]
mod wrap_tests;

impl Terminal {
    /// Serialize the terminal to a replayable ANSI string.
    #[must_use]
    pub fn serialize_ansi(&self, rows: usize, scrollback_rows: Option<u32>) -> String {
        let grid = self.grid();
        let cap = scrollback_rows.map(|n| n as usize);
        let active_history = grid.scrollback_lines();
        let take = cap.map_or(active_history, |n| n.min(active_history));
        let mut out = String::with_capacity(
            take.saturating_mul(8)
                .saturating_add(rows.saturating_mul(16)),
        );
        out.push_str("\x1b[0m");
        for i in (active_history - take)..active_history {
            let line = grid.get_history_line(i);
            if let Some(s) = line.as_ref().and_then(|l| l.as_str()) {
                out.push_str(s.trim_end());
            }
            out.push_str("\r\n");
        }
        if take > 0 {
            let _ = write!(out, "\x1b[{rows};1H");
            for _ in 0..take.min(rows.saturating_sub(1)) {
                out.push('\n');
            }
        }
        out.push_str("\x1b[H");
        for r in 0..u16::try_from(rows).unwrap_or(u16::MAX) {
            let _ = write!(out, "\x1b[{};1H\x1b[K", r + 1);
            let _ = grid.push_row_ansi_text_screen(r, &mut out);
            out.push_str("\x1b[0m");
        }
        let c = self.cursor();
        let cols = grid.cols();
        if grid.pending_wrap()
            && self.modes().auto_wrap
            && c.col.saturating_add(1) == cols
            && grid.row_at_screen(c.row).is_some_and(|r| r.len() == cols)
        {
            let _ = write!(out, "\x1b[{};1H\x1b[K", c.row as usize + 1);
            let _ = grid.push_row_ansi_text_screen(c.row, &mut out);
            out.push_str("\x1b[0m");
        } else {
            let _ = write!(out, "\x1b[{};{}H", c.row as usize + 1, c.col as usize + 1);
        }
        out
    }

    /// Bytes that bring a terminal this size, from any state, to this one's screens and to the state that decides where and how later output lands.
    /// They reset (RIS), then restore the title, the palette, tab stops, the main screen and its saved cursor, the alternate screen and its saved cursor when it is up, margins, origin mode, the cursor with its deferred wrap, the character sets, the insert, newline, wrap, reverse-video, cursor-visible and reverse-wrap modes, and the pen; an alternate screen kept for a later `?47h` is not.
    ///
    /// Standard DECRSPS restores cursor flags without drawing or replacing a save. The ordinary
    /// CUP/full-width-row prelude keeps common exports useful to readers that ignore that
    /// control; those readers approximate origin/margin and edited-edge wraps and protection.
    #[must_use]
    pub fn serialize_restore_ansi(&self) -> String {
        let rows = self.grid.rows();
        let cols = self.grid.cols();
        let mut out = String::with_capacity(usize::from(rows) * usize::from(cols) * 2);
        self.restore_header(&mut out);
        let region = self.grid.scroll_region();
        let alt = self.modes.alternate_screen;
        let main = if alt {
            self.alt_grid.as_ref()
        } else {
            Some(&self.grid)
        };
        if let Some(main) = main {
            paint_rows(&mut out, main);
        }
        if alt {
            if let Some(saved) = &self.cursor_save.main {
                saved_slot(&mut out, saved, main.unwrap_or(&self.grid), region);
                out.push_str("\x1b[?1049h");
                reset_slot(&mut out, saved);
            } else {
                out.push_str("\x1b[?47h");
            }
            paint_rows(&mut out, &self.grid);
        }
        let slot = if alt {
            &self.cursor_save.alt
        } else {
            &self.cursor_save.main
        };
        if let Some(saved) = slot {
            saved_slot(&mut out, saved, &self.grid, region);
            out.push_str("\x1b7");
            reset_slot(&mut out, saved);
        }
        self.restore_cursor_and_modes(&mut out);
        out
    }

    /// [`Self::serialize_restore_ansi`]'s start: the reset, the title, the palette and the tab stops.
    fn restore_header(&self, out: &mut String) {
        let cols = self.grid.cols();
        out.push_str("\x1bc");
        if !self.title.window.is_empty() {
            let title: String = self
                .title
                .window
                .chars()
                .filter(|c| !c.is_control())
                .collect();
            let _ = write!(out, "\x1b]2;{title}\x07");
        }
        let colors = self.capture_color_repr();
        let rgb = |c: aterm_types::Rgb| format!("rgb:{:02x}/{:02x}/{:02x}", c.r, c.g, c.b);
        for entry in &colors.palette {
            let _ = write!(out, "\x1b]4;{};{}\x07", entry.index, rgb(entry.rgb));
        }
        for (number, color) in [(10, colors.foreground), (11, colors.background)] {
            if let Some(color) = color {
                let _ = write!(out, "\x1b]{number};{}\x07", rgb(color));
            }
        }
        let stops = self.grid.tab_stops();
        if (0..usize::from(cols)).any(|col| {
            stops.get(col).copied().unwrap_or(false) != aterm_grid::is_default_tab_stop(col)
        }) {
            out.push_str("\x1b[3g");
            for col in
                (0..usize::from(cols)).filter(|col| stops.get(*col).copied().unwrap_or(false))
            {
                let _ = write!(out, "\x1b[1;{}H\x1bH", col + 1);
            }
        }
    }

    /// [`Self::serialize_restore_ansi`]'s end: the margins, origin mode, the cursor, the character sets, the modes and the pen.
    fn restore_cursor_and_modes(&self, out: &mut String) {
        let (rows, cols) = (self.grid.rows(), self.grid.cols());
        let region = self.grid.scroll_region();
        if region.top != 0 || region.bottom + 1 != rows {
            let _ = write!(out, "\x1b[{};{}r", region.top + 1, region.bottom + 1);
        }
        let lrmm = self.modes.left_right_margin_mode;
        let margins = self.grid.horizontal_margins();
        if lrmm {
            out.push_str("\x1b[?69h");
            if margins.left != 0 || margins.right + 1 != cols {
                let _ = write!(out, "\x1b[{};{}s", margins.left + 1, margins.right + 1);
            }
        }
        let cursor = self.grid.cursor();
        if self.modes.origin_mode {
            out.push_str("\x1b[?6h");
            let col = if lrmm {
                cursor.col.saturating_sub(margins.left)
            } else {
                cursor.col
            };
            let _ = write!(
                out,
                "\x1b[{};{}H",
                cursor.row.saturating_sub(region.top) + 1,
                col + 1
            );
        } else if lrmm
            || !self.modes.auto_wrap
            || !self.grid.pending_wrap()
            || !reprint_wrapped(out, &self.grid, cursor)
        {
            let _ = write!(out, "\x1b[{};{}H", cursor.row + 1, cursor.col + 1);
        }
        write_cursor_restore(
            out,
            CursorRestore {
                cursor,
                origin_mode: self.modes.origin_mode,
                pending_wrap: self.grid.pending_wrap(),
                protected: self.style.protected,
            },
        );
        out.push_str(&self.charset.restore_ansi());
        for (on, set) in [
            (self.modes.insert_mode, "\x1b[4h"),
            (self.modes.new_line_mode, "\x1b[20h"),
            (!self.modes.auto_wrap, "\x1b[?7l"),
            (self.modes.reverse_video, "\x1b[?5h"),
            (!self.modes.cursor_visible, "\x1b[?25l"),
            (self.modes.reverse_wraparound, "\x1b[?45h"),
        ] {
            if on {
                out.push_str(set);
            }
        }
        let _ = write!(
            out,
            "\x1b[{}m",
            super::handler_decrqss::sgr_of(&self.style, self.transient.current_underline_color)
        );
    }

    /// Scrollback history only (the lines above the viewport) as text + CRLF, no cursor or
    /// grid framing.
    #[must_use]
    pub fn serialize_scrollback_ansi(&self, max_rows: Option<u32>) -> String {
        let grid = self.main_grid();
        let history = grid.scrollback_lines();
        if history == 0 {
            return String::new();
        }
        let take = max_rows.map_or(history, |n| (n as usize).min(history));
        let mut out = String::with_capacity(take.saturating_mul(8));
        for i in (history - take)..history {
            let line = grid.get_history_line(i);
            if let Some(s) = line.as_ref().and_then(|l| l.as_str()) {
                out.push_str(s.trim_end());
            }
            out.push_str("\r\n");
        }
        out
    }
}

/// Paint every screen row of `grid` from the top, each from a cleared line in the default pen.
fn paint_rows(out: &mut String, grid: &Grid) {
    out.push_str("\x1b[0m");
    for r in 0..grid.rows() {
        let _ = write!(out, "\x1b[{};1H\x1b[K", r + 1);
        let _ = grid.push_row_ansi_text_screen(r, out);
        out.push_str("\x1b[0m");
    }
}

/// When `cursor` sits on the last column of a full row of `grid`, reprint that row so the cursor ends there with its wrap deferred, as it was; whether it did.
fn reprint_wrapped(out: &mut String, grid: &Grid, cursor: Cursor) -> bool {
    let cols = grid.cols();
    if cursor.col + 1 != cols
        || grid
            .row_at_screen(cursor.row)
            .is_none_or(|r| r.len() != cols)
    {
        return false;
    }
    let _ = write!(out, "\x1b[{};1H\x1b[K", cursor.row + 1);
    let _ = grid.push_row_ansi_text_screen(cursor.row, out);
    out.push_str("\x1b[0m");
    true
}

/// Put the cursor, origin mode, character sets and pen where `saved` holds them, so a DECSC (or `?1049h`) that follows saves it; `region` is the scroll region an origin-mode slot counts from.
fn saved_slot(out: &mut String, saved: &SavedCursorState, grid: &Grid, region: ScrollRegion) {
    let c = saved.cursor;
    if saved.origin_mode {
        let _ = write!(
            out,
            "\x1b[{};{}r\x1b[?6h",
            region.top + 1,
            region.bottom + 1
        );
        let _ = write!(
            out,
            "\x1b[{};{}H",
            c.row.saturating_sub(region.top) + 1,
            c.col + 1
        );
    } else if !saved.pending_wrap || !reprint_wrapped(out, grid, c) {
        let _ = write!(out, "\x1b[{};{}H", c.row + 1, c.col + 1);
    }
    write_cursor_restore(
        out,
        CursorRestore {
            cursor: c,
            origin_mode: saved.origin_mode,
            pending_wrap: saved.pending_wrap,
            protected: saved.style.protected,
        },
    );
    out.push_str(&saved.charset.restore_ansi());
    let _ = write!(
        out,
        "\x1b[{}m",
        super::handler_decrqss::sgr_of(&saved.style, saved.underline_color)
    );
}

/// Undo [`saved_slot`]: the default pen and character sets, origin mode off and the whole screen the scroll region.
fn reset_slot(out: &mut String, saved: &SavedCursorState) {
    out.push_str("\x1b[0m");
    if saved.style.protected {
        out.push_str("\x1b[0\"q");
    }
    if saved.charset != CharacterSetState::new() {
        out.push_str("\x1b(B\x1b)B\x1b*B\x1b+B\x0f\x1b}");
    }
    if saved.origin_mode {
        out.push_str("\x1b[?6l\x1b[r");
    }
}

#[cfg(test)]
#[path = "serialize_ansi_tests.rs"]
mod tests;
