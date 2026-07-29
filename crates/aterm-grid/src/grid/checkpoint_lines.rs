// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Line-level grid checkpoint primitives (GREEN-ORDER step 4 / design B.3.2).
//!
//! These two methods form the *foundation gate* for `TerminalCheckpoint`:
//! they project a [`Grid`]'s full cell content (scrollback + visible rows)
//! into a flat `Vec<Line>` and restore the visible rows back from a slice of
//! [`Line`]s — with full per-cell fidelity (wide chars, combining marks,
//! complex/ZWJ graphemes, hyperlinks, RGB colors, SGR flags, and the
//! per-row wrapped flag).
//!
//! Both directions reuse the already-tested scrollback conversion paths:
//! - capture: [`extract_row_extras`](super::scroll_convert) +
//!   [`row_to_line_with_stored_extras`](super::scroll_convert), the exact pair
//!   the ring buffer uses when a row scrolls off into scrollback, so a row →
//!   `Line` projection is faithful by construction (it resolves `StyleId` and
//!   RGB-overflow colors into the line attrs the same way scrollback does).
//! - restore: [`fill_row_from_line`](super::Grid::fill_row_from_line), the
//!   exact path Kitty CSI +T "unscroll" uses to materialize a scrollback
//!   `Line` back into a visible row (cells + wrapped flag + combining/complex/
//!   RGB/hyperlink extras).
//!
//! Reusing the production scroll paths (rather than a bespoke walk) is what
//! makes the round-trip honest: any fidelity bug here is also a scrollback
//! fidelity bug, and is covered by the unit tests below plus the existing
//! scroll_fill / scroll_convert suites.

use aterm_scrollback::Line;

use super::Grid;

impl Grid {
    /// Project the full grid content into a flat list of [`Line`]s.
    ///
    /// Layout of the returned vector is **scrollback-then-visible**:
    /// 1. `0..scrollback_lines()` history lines, oldest first (via
    ///    [`try_get_history_line`](Self::try_get_history_line)).
    /// 2. `0..rows()` visible rows, top to bottom, each converted with the
    ///    same stored-extras path the ring buffer uses for scroll-off.
    ///
    /// History lines that fail to decompress (a corrupt/quarantined cold
    /// block) are represented as an empty [`Line`] so the index space stays
    /// dense and the visible-row split on restore remains exact; this is the
    /// same lossy-but-bounded posture `checkpoint_snapshot` takes for
    /// scrollback. The common in-memory case never hits this branch.
    #[must_use]
    pub fn checkpoint_lines(&self) -> Vec<Line> {
        let scrollback_count = self.scrollback_lines();
        let mut lines = Vec::with_capacity(scrollback_count + self.rows() as usize);

        // 1) Scrollback history, oldest (idx 0) → newest.
        for idx in 0..scrollback_count {
            match self.try_get_history_line(idx) {
                Ok(Some(cow)) => lines.push(cow.into_owned()),
                // Out-of-bounds (racey shrink) or decode failure: keep the slot
                // dense with an empty line so the visible split stays exact.
                Ok(None) | Err(_) => lines.push(Line::new()),
            }
        }

        // 2) Visible rows, top → bottom, via the stored-extras scroll path.
        lines.extend(self.checkpoint_visible_lines());
        lines
    }

    /// Project the visible rows preceded by at most `max_history` lines of the
    /// MOST RECENT scrollback, in the same dense scrollback-then-visible layout
    /// [`Self::checkpoint_lines`] produces.
    ///
    /// This is the seamless-handoff projection.
    /// [`Self::checkpoint_visible_lines`] bounded the cost by carrying no history
    /// at all, which meant every in-session update truncated every tab to a single
    /// screen — seamless for the shell, lossy for the human reading it. Carrying
    /// the *whole* ring is the opposite failure: work proportional to an arbitrary
    /// history inside a parked-reader window measured in milliseconds.
    ///
    /// Cost is `O((rows + max_history) × cols)` regardless of how deep the ring
    /// goes. The restore split is unaffected: `restore_grid` reads the LAST `rows`
    /// lines as the visible grid and everything before them as history, so
    /// trimming from the FRONT is always sound.
    ///
    /// Returns the same thing as [`Self::checkpoint_visible_lines`] when
    /// `max_history == 0`, and as [`Self::checkpoint_lines`] when `max_history`
    /// meets or exceeds [`Self::scrollback_lines`].
    #[must_use]
    pub fn checkpoint_lines_bounded(&self, max_history: usize) -> Vec<Line> {
        let scrollback_count = self.scrollback_lines();
        let keep = scrollback_count.min(max_history);
        // History is indexed oldest (0) → newest, so the newest `keep` lines are
        // the tail. Skipping the head is what makes the bound cheap: the skipped
        // indices are never fetched, so a cold/compressed block is never touched.
        let first = scrollback_count - keep;
        let mut lines = Vec::with_capacity(keep + self.rows() as usize);
        for idx in first..scrollback_count {
            match self.try_get_history_line(idx) {
                Ok(Some(cow)) => lines.push(cow.into_owned()),
                // Same dense-slot posture as `checkpoint_lines`: a corrupt or
                // racily-shrunk slot becomes an empty line so the visible split
                // stays exact.
                Ok(None) | Err(_) => lines.push(Line::new()),
            }
        }
        lines.extend(self.checkpoint_visible_lines());
        lines
    }

    /// Project only the visible grid rows, excluding scrollback.
    ///
    /// Seamless process handoff uses this bounded projection while PTY readers are
    /// parked: its cost is `O(rows × cols)` regardless of months of history, yet
    /// restoring it paints the exact screen because checkpoint restore interprets the
    /// last `rows` serialized lines as the visible grid.
    #[must_use]
    pub fn checkpoint_visible_lines(&self) -> Vec<Line> {
        let rows = self.rows();
        let mut lines = Vec::with_capacity(rows as usize);
        let extras = self.extras();
        let styles = self.styles();
        for r in 0..rows {
            // A checkpoint carries the live terminal frame, not the user's
            // independently-scrolled viewport. `row()` applies display_offset;
            // `row_at_screen()` deliberately ignores it.
            let Some(row) = self.row_at_screen(r) else {
                lines.push(Line::new());
                continue;
            };
            let row_extras = Self::extract_row_extras(row, extras, r, styles);
            lines.push(Self::row_to_line_with_stored_extras(row, &row_extras));
        }

        lines
    }

    /// Restore the visible rows of this grid from a slice of [`Line`]s.
    ///
    /// `visible[r]` is written into visible row `r` for `r in 0..rows()` (any
    /// extra entries past `rows()` are ignored; any missing entries leave the
    /// corresponding row cleared). Each line's cells, wrapped flag, and extras
    /// (combining marks, complex/ZWJ chars, RGB colors, hyperlinks) are
    /// transferred via [`fill_row_from_line`](Self::fill_row_from_line) — the
    /// same internal placement logic `materialize_from_line` uses, so wide
    /// chars and grapheme clusters land identically to a live write.
    ///
    /// This does **not** touch scrollback; callers reconstruct scrollback at
    /// grid-construction time (e.g. by attaching a populated
    /// `ScrollbackStorage`). It is the inverse of the *visible* half of
    /// [`checkpoint_lines`](Self::checkpoint_lines).
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

    /// Build a grid whose visible rows + scrollback round-trip through the
    /// line codec, returning `(rebuilt_grid, original_lines)`.
    ///
    /// Re-creates a fresh grid of the same size with the captured scrollback
    /// re-attached (oldest-first) and the visible rows restored — exactly the
    /// shape `Terminal::from_checkpoint` uses.
    fn rebuild_from_lines(rows: u16, cols: u16, lines: &[Line]) -> Grid {
        // Split scrollback (all but the last `rows`) from visible (last `rows`).
        let total = lines.len();
        let visible_start = total.saturating_sub(rows as usize);
        let (scrollback_lines, visible_lines) = lines.split_at(visible_start);

        let mut sb = Scrollback::with_defaults();
        // Don't let the default 100k line cap interfere with tiny tests.
        sb.set_line_limit(None);
        for line in scrollback_lines {
            sb.push_line(line.clone());
        }

        let mut grid = Grid::with_tiered_scrollback(rows, cols, 1000, sb);
        grid.restore_visible_from_lines(visible_lines);
        grid
    }

    /// Materialize a grid's visible rows to plain strings (trailing spaces
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
    fn roundtrip_plain_text() {
        let mut g = Grid::new(4, 20);
        write_text(&mut g, 0, "hello world");
        write_text(&mut g, 1, "second line");
        write_text(&mut g, 3, "bottom");

        let lines = g.checkpoint_lines();
        assert_eq!(lines.len(), g.scrollback_lines() + 4);

        let rebuilt = rebuild_from_lines(4, 20, &lines);
        assert_eq!(visible_text(&g), visible_text(&rebuilt));
        assert_eq!(visible_text(&rebuilt)[0], "hello world");
        assert_eq!(visible_text(&rebuilt)[1], "second line");
        assert_eq!(visible_text(&rebuilt)[3], "bottom");
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
        // Bold + indexed-256 fg + RGB bg cell at (0,0).
        let flags = CellFlags::BOLD.union(CellFlags::UNDERLINE);
        let fg = PackedColor::indexed(202); // 256-color
        let bg = PackedColor::rgb(10, 20, 30); // truecolor
        let styled = Cell::with_style('X', fg, bg, flags);
        g.set_cell(0, 0, styled);
        // RGB at (0,1) needs CellExtras overflow — set via the extras path.
        let styled2 = Cell::with_style('Y', PackedColor::rgb(200, 100, 50), bg, flags);
        g.set_cell(0, 1, styled2);
        if g.row(0).is_some() {
            let e = g.extras_mut().get_or_create(crate::CellCoord::new(0, 1));
            e.set_fg_rgb(Some([200, 100, 50]));
            e.set_bg_rgb(Some([10, 20, 30]));
        }

        let lines = g.checkpoint_lines();
        let rebuilt = rebuild_from_lines(3, 16, &lines);

        // Char fidelity.
        assert_eq!(visible_text(&rebuilt)[0], "XY");
        // Flags fidelity on the plain (non-RGB-overflow) cell.
        let c0 = *rebuilt.row(0).unwrap().get(0).unwrap();
        assert!(c0.flags().contains(CellFlags::BOLD), "bold preserved");
        assert!(
            c0.flags().contains(CellFlags::UNDERLINE),
            "underline preserved"
        );
        // RGB extra fidelity on cell (0,1).
        let extra = rebuilt
            .extras()
            .get(crate::CellCoord::new(0, 1))
            .expect("rgb extra restored");
        assert_eq!(extra.fg_rgb(), Some([200, 100, 50]), "fg rgb preserved");
    }

    #[test]
    fn roundtrip_wide_char() {
        let mut g = Grid::new(2, 10);
        // '世' is a wide (2-cell) CJK char.
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
        // base 'e' + combining acute accent U+0301.
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
        // A non-BMP / ZWJ emoji sequence stored as a complex char at col 0,
        // followed by a normal trailing char so the row's cached `len` (which
        // the test-only `set_cell` only extends on a non-empty write) covers
        // the complex cell. In the live engine the parser write path maintains
        // `len`; here we establish it explicitly.
        // The family emoji is a WIDE (2-cell) ZWJ sequence: it occupies cols
        // 0-1 (col 1 = wide continuation). Place the trailing normal char at
        // col 2 so it doesn't collide, and write the wide-continuation spacer.
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
        // Write more than `rows` lines so scrollback fills, by pushing lines
        // directly into a tiered scrollback then checkpointing.
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
        // 10 scrollback + 3 visible.
        assert_eq!(lines.len(), 10 + 3);
        // Oldest scrollback line first.
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
