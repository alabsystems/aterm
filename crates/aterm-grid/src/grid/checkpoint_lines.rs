// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Line-level grid checkpoint projection and visible-row restoration.

use aterm_scrollback::Line;

use super::Grid;

impl Grid {
    /// Projects scrollback (oldest first) followed by visible rows into a flat `Vec<Line>`.
    #[must_use]
    #[cfg(test)]
    pub fn checkpoint_lines(&self) -> Vec<Line> {
        let scrollback_count = self.scrollback_lines();
        let mut lines = Vec::with_capacity(scrollback_count + self.rows() as usize);

        lines.extend(
            self.history_lines_from(0)
                .take(scrollback_count)
                .map(|line| line.map_or_else(Line::new, std::borrow::Cow::into_owned)),
        );

        lines.extend(self.checkpoint_visible_lines());
        lines
    }

    /// Projects at most `max_history` recent scrollback lines followed by the visible rows.
    #[must_use]
    pub fn checkpoint_lines_bounded(&self, max_history: usize) -> Vec<Line> {
        let scrollback_count = self.scrollback_lines();
        let keep = scrollback_count.min(max_history);
        let first = scrollback_count - keep;
        let mut lines = Vec::with_capacity(keep + self.rows() as usize);
        lines.extend(
            self.history_lines_from(first)
                .take(keep)
                .map(|line| line.map_or_else(Line::new, std::borrow::Cow::into_owned)),
        );
        lines.extend(self.checkpoint_visible_lines());
        lines
    }

    /// Projects only the live visible rows into `Vec<Line>`.
    #[must_use]
    pub fn checkpoint_visible_lines(&self) -> Vec<Line> {
        let rows = self.rows();
        let mut lines = Vec::with_capacity(rows as usize);
        let extras = self.extras();
        for r in 0..rows {
            let Some(row) = self.row_at_screen(r) else {
                lines.push(Line::new());
                continue;
            };
            let row_extras = Self::extract_row_extras(row, extras, r);
            let len = super::scrollback_reflow::history_row_len(row, self.row_at_screen(r + 1));
            lines.push(Self::row_to_line_with_stored_extras_at_len(
                row,
                &row_extras,
                len,
            ));
        }

        lines
    }

    /// Restores visible rows `0..rows()` of this grid from `visible`.
    pub fn restore_visible_from_lines(&mut self, visible: &[Line]) {
        let rows = self.rows();
        let cols = self.cols();
        for r in 0..rows {
            match visible.get(r as usize) {
                Some(line) => self.fill_row_from_line(r, line, cols),
                None => {
                    if let Some(row) = self.row_mut(r) {
                        row.clear();
                    }
                }
            }
        }
    }

    /// Clones this grid's history, live frame, and cursor state for mid-recording replay.
    #[must_use]
    pub fn replay_fork(&self) -> Option<Grid> {
        let storage = &self.storage;
        if storage.scrollback.is_some()
            || storage.scrollback_detached_for_reflow
            || storage.resize_undo.is_some()
            || !self.handback_idle()
        {
            return None;
        }
        let (rows, cols) = (self.rows(), self.cols());
        let lines = self.checkpoint_lines_bounded(usize::MAX);
        let (history, visible) = lines.split_at(lines.len().saturating_sub(usize::from(rows)));
        let mut fork = Grid::with_scrollback(rows, cols, storage.max_scrollback);
        for line in history {
            fork.fill_row_from_line(0, line, cols);
            fork.scroll_up(1);
        }
        fork.restore_visible_from_lines(visible);
        let from = &storage.cursor_state;
        let to = &mut fork.storage.cursor_state;
        to.cursor = from.cursor;
        to.saved_cursor = from.saved_cursor;
        to.scroll_region = from.scroll_region;
        to.horizontal_margins = from.horizontal_margins;
        to.tab_stops.clone_from(&from.tab_stops);
        to.tab_defaults_suppressed = from.tab_defaults_suppressed;
        to.pending_wrap = from.pending_wrap;
        to.cursor_template = from.cursor_template;
        to.cursor_template_bg_rgb = from.cursor_template_bg_rgb;
        fork.storage.has_horizontal_margins = storage.has_horizontal_margins;
        fork.storage.any_double_width = storage.any_double_width;
        fork.storage.resize_law = storage.resize_law;
        fork.continue_absolute_numbering(self.absolute_row_counter());
        Some(fork)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cell, CellFlags, PackedColor};
    use aterm_scrollback::{Rle, Scrollback};

    /// Build a plain-text [`Line`] for scrollback fixtures.
    fn plain_line(text: &str) -> Line {
        Line::with_attrs(text, Rle::new())
    }

    /// re-attached (oldest first) and the visible rows restored.
    fn rebuild_from_lines(rows: u16, cols: u16, lines: &[Line]) -> Grid {
        let total = lines.len();
        let visible_start = total.saturating_sub(rows as usize);
        let (scrollback_lines, visible_lines) = lines.split_at(visible_start);

        let mut sb = Scrollback::with_defaults();
        sb.set_line_limit(None);
        for line in scrollback_lines {
            sb.push_line(line.clone());
        }

        let mut grid = Grid::with_tiered_scrollback(rows, cols, 1000, sb);
        grid.restore_visible_from_lines(visible_lines);
        grid
    }

    /// trimmed) for content comparison.
    fn visible_text(grid: &Grid) -> Vec<String> {
        (0..grid.rows())
            .map(|r| {
                let row = grid.row(r).expect("row in range");
                let len = row.len() as usize;
                let cells = &row.as_slice()[..len];
                let mut s = String::new();
                let mut col = 0usize;
                while col < cells.len() {
                    let cell = cells[col];
                    if cell.is_wide_continuation() {
                        col += 1;
                        continue;
                    }
                    if cell.is_complex() {
                        if let Some(extra) = grid.extras().get(crate::CellCoord::new(r, col as u16))
                            && let Some(cx) = extra.complex_char()
                        {
                            s.push_str(cx);
                        }
                    } else {
                        s.push(cell.char());
                        if let Some(extra) = grid.extras().get(crate::CellCoord::new(r, col as u16))
                        {
                            for m in extra.combining() {
                                s.push(*m);
                            }
                        }
                    }
                    col += 1;
                }
                s.trim_end().to_string()
            })
            .collect()
    }

    fn write_text(grid: &mut Grid, row: u16, text: &str) {
        for (col, ch) in text.chars().enumerate() {
            grid.set_cell(row, col as u16, Cell::new(ch));
        }
    }

    #[test]
    fn visible_checkpoint_cost_does_not_scale_with_scrollback() {
        let mut g = Grid::with_scrollback(3, 20, 100);
        let mut sb = Scrollback::with_defaults();
        sb.set_line_limit(None);
        for i in 0..40 {
            sb.push_line(plain_line(&format!("history-{i}")));
        }
        g.attach_scrollback(sb);
        write_text(&mut g, 0, "visible-a");
        write_text(&mut g, 2, "visible-c");

        let visible = g.checkpoint_visible_lines();
        assert_eq!(
            visible.len(),
            3,
            "only fixed-height visible rows are copied"
        );
        let mut rebuilt = Grid::new(3, 20);
        rebuilt.restore_visible_from_lines(&visible);
        assert_eq!(visible_text(&g), visible_text(&rebuilt));
        assert_eq!(rebuilt.scrollback_lines(), 0);
    }

    #[test]
    fn visible_checkpoint_captures_live_rows_while_viewport_is_scrolled_up() {
        let mut g = Grid::with_scrollback(3, 20, 100);
        for i in 0..8 {
            g.carriage_return();
            for ch in format!("line-{i}").chars() {
                g.write_char(ch);
            }
            if i < 7 {
                g.line_feed();
            }
        }
        let live = aterm_scrollback::serialize_lines(&g.checkpoint_visible_lines());
        let live_top = g.row_at_screen(0).unwrap().to_string();
        g.scroll_display(2);
        assert!(g.display_offset() > 0);
        assert_ne!(
            g.row(0).unwrap().to_string(),
            live_top,
            "negative control: fixture must expose a different viewport row"
        );
        assert_eq!(
            aterm_scrollback::serialize_lines(&g.checkpoint_visible_lines()),
            live,
            "viewport offset must not alter seamless live-screen capture"
        );
    }

    #[test]
    fn roundtrip_sgr_colors_and_attrs() {
        let mut g = Grid::new(3, 16);
        let flags = CellFlags::BOLD.union(CellFlags::UNDERLINE);
        let fg = PackedColor::indexed(202); // 256-color
        let bg = PackedColor::rgb(10, 20, 30); // truecolor
        let styled = Cell::with_style('X', fg, bg, flags);
        g.set_cell(0, 0, styled);
        let styled2 = Cell::with_style('Y', PackedColor::rgb(200, 100, 50), bg, flags);
        g.set_cell(0, 1, styled2);
        if g.row(0).is_some() {
            let e = g.extras_mut().get_or_create(crate::CellCoord::new(0, 1));
            e.set_fg_rgb(Some([200, 100, 50]));
            e.set_bg_rgb(Some([10, 20, 30]));
        }

        let lines = g.checkpoint_lines();
        let rebuilt = rebuild_from_lines(3, 16, &lines);

        assert_eq!(visible_text(&rebuilt)[0], "XY");
        let c0 = *rebuilt.row(0).unwrap().get(0).unwrap();
        assert!(c0.flags().contains(CellFlags::BOLD), "bold preserved");
        assert!(
            c0.flags().contains(CellFlags::UNDERLINE),
            "underline preserved"
        );
        let extra = rebuilt
            .extras()
            .get(crate::CellCoord::new(0, 1))
            .expect("rgb extra restored");
        assert_eq!(extra.fg_rgb(), Some([200, 100, 50]), "fg rgb preserved");
    }

    #[test]
    fn roundtrip_wide_char() {
        let mut g = Grid::new(2, 10);
        g.set_cell(
            0,
            0,
            Cell::with_style(
                '世',
                PackedColor::DEFAULT_FG,
                PackedColor::DEFAULT_BG,
                CellFlags::WIDE,
            ),
        );
        g.set_cell(
            0,
            1,
            Cell::with_style(
                ' ',
                PackedColor::DEFAULT_FG,
                PackedColor::DEFAULT_BG,
                CellFlags::WIDE_CONTINUATION,
            ),
        );
        write_text(&mut g, 0, ""); // no-op, keep wide cell

        let lines = g.checkpoint_lines();
        let rebuilt = rebuild_from_lines(2, 10, &lines);

        let c0 = *rebuilt.row(0).unwrap().get(0).unwrap();
        assert_eq!(c0.char(), '世', "wide char preserved");
        assert!(c0.is_wide(), "wide flag preserved");
        let c1 = *rebuilt.row(0).unwrap().get(1).unwrap();
        assert!(c1.is_wide_continuation(), "wide continuation preserved");
    }

    #[test]
    fn roundtrip_combining_mark() {
        let mut g = Grid::new(2, 10);
        g.set_cell(0, 0, Cell::new('e'));
        g.extras_mut()
            .get_or_create(crate::CellCoord::new(0, 0))
            .add_combining('\u{0301}');
        if let Some(cell) = g.row_mut(0).and_then(|r| r.get_mut(0)) {
            cell.set_has_extras(true);
        }

        let lines = g.checkpoint_lines();
        let rebuilt = rebuild_from_lines(2, 10, &lines);

        let extra = rebuilt
            .extras()
            .get(crate::CellCoord::new(0, 0))
            .expect("combining extra restored");
        assert_eq!(
            extra.combining(),
            ['\u{0301}'].as_slice(),
            "combining mark preserved"
        );
        assert_eq!(visible_text(&rebuilt)[0], "e\u{0301}");
    }

    #[test]
    fn roundtrip_complex_char() {
        let mut g = Grid::new(2, 10);
        g.set_cell(0, 2, Cell::new('Z'));
        g.set_cell(
            0,
            0,
            Cell::with_style(
                'X',
                PackedColor::DEFAULT_FG,
                PackedColor::DEFAULT_BG,
                CellFlags::WIDE,
            ),
        );
        g.set_cell(
            0,
            1,
            Cell::with_style(
                ' ',
                PackedColor::DEFAULT_FG,
                PackedColor::DEFAULT_BG,
                CellFlags::WIDE_CONTINUATION,
            ),
        );
        g.set_cell_complex_char(0, 0, "👨‍👩‍👧");

        let lines = g.checkpoint_lines();
        let rebuilt = rebuild_from_lines(2, 10, &lines);

        let extra = rebuilt
            .extras()
            .get(crate::CellCoord::new(0, 0))
            .expect("complex extra restored");
        assert_eq!(
            extra.complex_char().map(|s| s.to_string()),
            Some("👨‍👩‍👧".to_string()),
            "complex char preserved"
        );
        assert!(
            rebuilt.row(0).unwrap().get(0).unwrap().is_complex(),
            "complex flag preserved"
        );
        let c2 = *rebuilt.row(0).unwrap().get(2).unwrap();
        assert_eq!(
            c2.char(),
            'Z',
            "trailing normal char preserved after wide complex"
        );
    }

    #[test]
    fn roundtrip_hyperlink() {
        use std::sync::Arc;
        let mut g = Grid::new(2, 12);
        write_text(&mut g, 0, "link");
        let url: Arc<str> = Arc::from("https://example.com");
        for col in 0..4u16 {
            let e = g.extras_mut().get_or_create(crate::CellCoord::new(0, col));
            e.set_hyperlink(Some(url.clone()));
            if let Some(cell) = g.row_mut(0).and_then(|r| r.get_mut(col)) {
                cell.set_has_extras(true);
            }
        }

        let lines = g.checkpoint_lines();
        let rebuilt = rebuild_from_lines(2, 12, &lines);

        let extra = rebuilt
            .extras()
            .get(crate::CellCoord::new(0, 0))
            .expect("hyperlink extra restored");
        assert_eq!(
            extra.hyperlink().map(|u| u.to_string()),
            Some("https://example.com".to_string()),
            "hyperlink url preserved"
        );
    }

    #[test]
    fn roundtrip_wrapped_line() {
        let mut g = Grid::new(3, 6);
        write_text(&mut g, 0, "abcdef"); // fills the row
        if let Some(row) = g.row_mut(0) {
            row.set_wrapped(true);
        }
        write_text(&mut g, 1, "ghi");

        let lines = g.checkpoint_lines();
        let rebuilt = rebuild_from_lines(3, 6, &lines);

        assert!(
            rebuilt.row(0).unwrap().is_wrapped(),
            "wrapped flag preserved"
        );
        assert!(
            !rebuilt.row(1).unwrap().is_wrapped(),
            "non-wrapped flag preserved"
        );
        assert_eq!(visible_text(&rebuilt)[0], "abcdef");
        assert_eq!(visible_text(&rebuilt)[1], "ghi");
    }

    #[test]
    fn roundtrip_scrollback_fills() {
        let mut g = Grid::with_tiered_scrollback(3, 12, 1000, Scrollback::with_defaults());
        let mut sb = Scrollback::with_defaults();
        sb.set_line_limit(None);
        for i in 0..10 {
            sb.push_line(plain_line(&format!("history-{i}")));
        }
        g.attach_scrollback(sb);
        write_text(&mut g, 0, "visible0");
        write_text(&mut g, 1, "visible1");
        write_text(&mut g, 2, "visible2");

        let lines = g.checkpoint_lines();
        assert_eq!(lines.len(), 10 + 3);
        assert_eq!(lines[0].as_str(), Some("history-0"));
        assert_eq!(lines[9].as_str(), Some("history-9"));

        let rebuilt = rebuild_from_lines(3, 12, &lines);
        assert_eq!(rebuilt.scrollback_lines(), 10, "scrollback line count");
        assert_eq!(
            rebuilt.try_get_history_line(0).unwrap().unwrap().as_str(),
            Some("history-0"),
            "oldest scrollback line preserved (order)"
        );
        assert_eq!(
            rebuilt.try_get_history_line(9).unwrap().unwrap().as_str(),
            Some("history-9"),
            "newest scrollback line preserved (order)"
        );
        assert_eq!(visible_text(&rebuilt)[0], "visible0");
        assert_eq!(visible_text(&rebuilt)[2], "visible2");
    }
}
