// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The anchors' lend through a width change.

use aterm_hash::FxHasher;
use std::hash::Hasher;

use super::Grid;
use super::reflow::is_mergeable_continuation;
use super::scroll_convert::{RowToLineCursorState, next_combining, next_complex_char};
use super::scrollback_offload::HOLD_BACK_SCREENS;
use crate::{AnchorRole, AnchorSet, FenceKnob, Row, knob};

/// How far above the screen a lend may read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LendReach {
    /// Everything the ring holds (a synchronous width change rewraps it all).
    Ring,
    /// At most this many rows above the screen's top.
    Rows(u64),
}

/// Where a lent anchor sits, relative to the cursor's logical line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lent {
    /// cursor's own line).
    Head(usize),
    /// On a continuation row of that line.
    Mid(usize),
    /// anchor.
    Below(bool),
    /// Not placeable (the cursor's own line could not be read).
    Unknown,
}

/// Anchors lent out of a grid for a width change; give them back with [`Grid::take_back_anchors`].
#[derive(Debug)]
#[must_use = "lent anchors must be taken back, or the fence loses every span"]
pub struct AnchorLend {
    set: AnchorSet,
    points: Vec<(usize, Lent)>,
    /// `trail[k]`: the rolling digest of lines `0..=k` (the cursor's line up).
    trail: Vec<u64>,
    /// The digests of the drawn lines below the cursor's line, in order.
    below: Option<Vec<u64>>,
}

/// A logical line: its first and last absolute rows and its glyph digest.
#[derive(Clone, Copy, Debug)]
struct LogicalLine {
    head: u64,
    last: u64,
    digest: u64,
}

/// Unknown complex text refuses the lend rather than becoming a shared token.
fn hash_glyphs(h: &mut FxHasher, grid: &Grid, absolute: u64, row: &Row) -> Option<()> {
    let base = grid.visible_to_absolute(0);
    let screen = if absolute >= base {
        Some(u16::try_from(absolute - base).ok()?)
    } else {
        None
    };
    let history = if screen.is_none() {
        let back = usize::try_from(base - absolute).ok()?;
        let index = grid.storage.ring_buffer_scrollback().checked_sub(back)?;
        grid.storage.ring_history_extras(index)
    } else {
        None
    };
    let mut cursors = RowToLineCursorState::default();
    let mut glyph = |c: char| {
        if c != ' ' && c != '\0' {
            h.write_u32(u32::from(c));
        }
    };
    for (col, cell) in row.as_slice().iter().enumerate() {
        let col = u16::try_from(col).ok()?;
        let stored_complex = history.and_then(|e| next_complex_char(e, &mut cursors, col));
        let stored_combining = history.and_then(|e| next_combining(e, &mut cursors, col));
        if row.is_cell_wide_continuation(col) {
            continue;
        }
        if let Some(screen) = screen {
            let data = grid.cell_render_data(screen, col, *cell);
            let extra = data.cell_extra();
            if cell.is_complex() {
                if let Some(cluster) = extra.and_then(|e| e.complex_char()) {
                    for c in cluster.chars() {
                        glyph(c);
                    }
                } else {
                    glyph(data.complex_char()?);
                }
            } else {
                glyph(cell.char());
            }
            if let Some(extra) = extra {
                for &c in extra.combining() {
                    glyph(c);
                }
            }
        } else {
            if cell.is_complex() {
                for c in stored_complex?.chars() {
                    glyph(c);
                }
            } else {
                glyph(cell.char());
            }
            for &c in stored_combining.unwrap_or(&[]) {
                glyph(c);
            }
        }
    }
    Some(())
}

impl Grid {
    /// The reach of an offloaded width change to `new_rows` × `new_cols`.
    #[must_use]
    pub fn anchor_reach_for_offload(&self, new_rows: u16, new_cols: u16) -> LendReach {
        let cells = usize::from(self.storage.visible_rows.max(new_rows))
            * usize::from(self.storage.cols.max(new_cols));
        let rows = HOLD_BACK_SCREENS.saturating_mul(cells) / usize::from(self.storage.cols.max(1));
        LendReach::Rows(u64::try_from(rows).unwrap_or(u64::MAX))
    }

    /// Whether a lend can read the cursor's logical line down from its head.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn lend_reads_cursor_line(&self) -> bool {
        self.cursor_line().is_some()
    }

    /// The cursor's logical line: its head and last absolute rows.
    fn cursor_line(&self) -> Option<(u64, u64)> {
        let visible = self.storage.visible_rows;
        let row = self.storage.cursor.row.min(visible.saturating_sub(1));
        let cursor = self.visible_to_absolute(row);
        let mut last = cursor;
        while let Some(next) = self.live_row_at_absolute(last + 1) {
            if !is_mergeable_continuation(next) {
                break;
            }
            last += 1;
        }
        let line = self.line_ending_at(last, 0)?;
        Some((line.head, line.last))
    }

    /// below the floor): an incomplete line is never vouched for.
    fn line_ending_at(&self, last: u64, floor: u64) -> Option<LogicalLine> {
        let mut head = last;
        loop {
            if head < floor {
                return None;
            }
            let row = self.live_row_at_absolute(head)?;
            if !is_mergeable_continuation(row) {
                break;
            }
            head = head.checked_sub(1)?;
        }
        let mut h = FxHasher::default();
        for r in head..=last {
            hash_glyphs(&mut h, self, r, self.live_row_at_absolute(r)?)?;
        }
        Some(LogicalLine {
            head,
            last,
            digest: h.finish(),
        })
    }

    /// read whole.
    fn lines_up(&self, last: u64, floor: u64) -> impl Iterator<Item = LogicalLine> + '_ {
        let mut next = Some(last);
        std::iter::from_fn(move || {
            let line = self.line_ending_at(next?, floor)?;
            next = line.head.checked_sub(1);
            Some(line)
        })
    }

    /// ink into an open span. Unreadable payloads still refuse the lend.
    fn below_digests(&self, last: u64) -> Option<Vec<u64>> {
        let end = self.visible_to_absolute(0) + u64::from(self.storage.visible_rows);
        let mut out = Vec::new();
        let mut row = last + 1;
        while row < end {
            let mut h = FxHasher::default();
            let mut drawn = false;
            let mut r = row;
            while let Some(cells) = self.live_row_at_absolute(r) {
                if r > row && !is_mergeable_continuation(cells) {
                    break;
                }
                hash_glyphs(&mut h, self, r, cells)?;
                drawn |= self.absolute_row_inked(r);
                r += 1;
            }
            if drawn {
                out.push(h.finish());
            }
            row = r.max(row + 1);
        }
        Some(out)
    }

    /// Lend this grid's anchors out for a width change (see the module doc).
    pub fn lend_anchors(&mut self, reach: LendReach) -> Option<AnchorLend> {
        if self.anchors().is_empty() {
            return None;
        }
        let set = self.storage.presentation.anchors.take_for_lend();
        let owed = self.pushed_rows() > 0;
        let floor = match reach {
            LendReach::Ring => 0,
            LendReach::Rows(rows) => self.visible_to_absolute(0).saturating_sub(rows),
        };
        let lowest = set
            .slots()
            .iter()
            .filter(|a| a.live)
            .map(|a| a.row)
            .min()
            .unwrap_or(0);
        let mut lines = Vec::new();
        let mut trail = Vec::new();
        let mut h = FxHasher::default();
        let cursor_line = self.cursor_line();
        if let Some((_, last0)) = cursor_line {
            for line in self.lines_up(last0, floor) {
                h.write_u64(line.digest);
                trail.push(h.finish());
                lines.push(line);
                if line.head <= lowest {
                    break;
                }
            }
        }
        let classify = |row: u64| -> Lent {
            let (Some((_, last0)), Some(oldest), false) = (cursor_line, lines.last(), owed) else {
                return Lent::Unknown;
            };
            let Some(past_screen) = self
                .visible_to_absolute(0)
                .checked_add(u64::from(self.storage.visible_rows))
            else {
                return Lent::Unknown;
            };
            if row > past_screen.saturating_add(u64::from(crate::MAX_GRID_ROWS)) {
                return Lent::Unknown;
            }
            if row > last0 {
                let mut gap_blank = true;
                for r in last0 + 1..row.min(past_screen) {
                    if self.live_row_at_absolute(r).is_none() {
                        return Lent::Unknown;
                    }
                    gap_blank &= !self.absolute_row_inked(r);
                }
                return Lent::Below(gap_blank);
            }
            if row < oldest.head {
                return Lent::Head(lines.len() - 1);
            }
            let k = lines
                .iter()
                .position(|l| l.head <= row)
                .unwrap_or(lines.len() - 1);
            if row == lines[k].head {
                Lent::Head(k)
            } else {
                Lent::Mid(k)
            }
        };
        let points = set
            .slots()
            .iter()
            .enumerate()
            .filter(|(_, a)| a.live)
            .map(|(slot, a)| (slot, classify(a.row)))
            .collect();
        let below = cursor_line.and_then(|(_, last0)| self.below_digests(last0));
        Some(AnchorLend {
            set,
            points,
            trail,
            below,
        })
    }

    /// Put lent anchors back where their text landed (see the module doc).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "Reflow",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    pub fn take_back_anchors(&mut self, lend: AnchorLend) {
        let AnchorLend {
            mut set,
            points,
            trail,
            below,
        } = lend;
        debug_assert!(
            self.anchors().is_empty(),
            "nothing places an anchor while a lend is out"
        );
        if knob(FenceKnob::NoReflowLend) {
            self.storage.presentation.anchors = set;
            return;
        }
        let mut lines = Vec::with_capacity(trail.len());
        let mut verified = true;
        let cursor_line = self.cursor_line();
        if let Some((_, last0)) = cursor_line {
            let mut h = FxHasher::default();
            for (line, want) in self.lines_up(last0, 0).zip(&trail) {
                h.write_u64(line.digest);
                if h.finish() != *want {
                    verified = false;
                    break;
                }
                lines.push(line);
            }
            match (below, self.below_digests(last0)) {
                (Some(below), Some(now)) if below.starts_with(&now) => {}
                _ => verified = false,
            }
        }
        let needs_lines = points.iter().any(|(_, p)| !matches!(p, Lent::Below(_)));
        let placeable = cursor_line.is_some() && (!needs_lines || !lines.is_empty());
        if !verified || !placeable || points.iter().any(|(_, p)| *p == Lent::Unknown) {
            set.lose_lent();
            self.storage.presentation.anchors = set;
            return;
        }
        let past_screen = self.visible_to_absolute(0) + u64::from(self.storage.visible_rows);
        let after_cursor_line = cursor_line.map_or(past_screen, |(_, last0)| last0 + 1);
        let head = |k: usize| lines[k.min(lines.len() - 1)].head;
        let mut points = points.into_iter();
        for (slot, anchor) in set.live_mut() {
            let Some((lent_slot, point)) = points.next() else {
                break;
            };
            debug_assert_eq!(slot, lent_slot, "live slots are lent in slot order");
            anchor.row = match (point, anchor.role) {
                (Lent::Head(k), _) | (Lent::Mid(k), AnchorRole::End) => head(k),
                (Lent::Mid(0), AnchorRole::Start) => after_cursor_line,
                (Lent::Mid(k), AnchorRole::Start) => head(k - 1),
                (Lent::Below(true), AnchorRole::Start) => after_cursor_line,
                (Lent::Below(false), AnchorRole::Start) => past_screen,
                (Lent::Below(_), AnchorRole::End) => after_cursor_line,
                (Lent::Unknown, _) => unreachable!("an unknown point loses the lend above"),
            };
        }
        set.rekey_lent();
        self.storage.presentation.anchors = set;
    }
}

#[cfg(test)]
mod tests {
    use super::LendReach;
    use crate::{AnchorRole, Grid};

    /// shell prints them), the cursor left on the row after the last.
    fn grid_printing(rows: u16, cols: u16, lines: &[&str]) -> Grid {
        let mut g = Grid::with_scrollback(rows, cols, 1000);
        for line in lines {
            for ch in line.chars() {
                g.write_char_wrap(ch);
            }
            g.carriage_return();
            g.line_feed();
        }
        g
    }

    fn row_text(g: &Grid, abs: u64) -> String {
        let row = g.live_row_at_absolute(abs).expect("a readable row");
        row.as_slice()
            .iter()
            .map(|c| char::from_u32(u32::from(c.char_data())).unwrap_or('?'))
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn a_widening_rewrap_lands_each_head_anchor_on_its_line() {
        let mut g = grid_printing(6, 8, &["one", "abcdefghij", "card", "tail"]);
        let base = g.visible_to_absolute(0);
        let card = (0..6)
            .map(|r| base + r)
            .find(|&r| row_text(&g, r) == "card")
            .unwrap();
        let start = g.anchors_mut().place(card, AnchorRole::Start).unwrap();
        let end = g.anchors_mut().place(card + 1, AnchorRole::End).unwrap();
        let lend = g.lend_anchors(LendReach::Ring).unwrap();
        assert!(
            g.anchors().is_empty(),
            "the resize runs with no anchor in the grid"
        );
        g.resize(6, 20);
        g.take_back_anchors(lend);
        assert!(!g.anchors().lost());
        let s = g.anchors().row(start).unwrap();
        assert_eq!(row_text(&g, s), "card");
        assert_eq!(g.anchors().row(end), Some(s + 1));

        let mut g = grid_printing(6, 8, &["one", "abcdefghij", "card", "tail"]);
        let cursor = g.cursor();
        g.set_cursor(0, 0);
        g.write_emoji_autowrap_fast(
            '😀',
            crate::PackedColors::DEFAULT,
            crate::CellFlags::COMPLEX,
        );
        g.set_cursor(cursor.row, cursor.col);
        let base = g.visible_to_absolute(0);
        let card = (0..6)
            .map(|r| base + r)
            .find(|&r| row_text(&g, r) == "card")
            .unwrap();
        let start = g.anchors_mut().place(card, AnchorRole::Start).unwrap();
        let end = g.anchors_mut().place(card + 1, AnchorRole::End).unwrap();
        let lend = g.lend_anchors(LendReach::Ring).unwrap();
        g.resize(6, 20);
        g.take_back_anchors(lend);
        assert!(
            !g.anchors().lost(),
            "unchanged resolved text survived reflow"
        );
        let start = g.anchors().row(start).unwrap();
        assert_eq!(row_text(&g, start), "card");
        assert_eq!(g.anchors().row(end), Some(start + 1));

        for (row, placeable) in [(u64::MAX - 1, false), (4, true)] {
            let mut g = Grid::with_scrollback(4, 10, 100);
            assert_eq!(g.visible_to_absolute(0), 0);
            let start = g.anchors_mut().place(row, AnchorRole::Start).unwrap();
            let lend = g.lend_anchors(LendReach::Ring).unwrap();
            g.resize(4, 12);
            g.take_back_anchors(lend);
            assert_eq!(!g.anchors().lost(), placeable);
            if placeable {
                assert_eq!(
                    g.anchors().row(start),
                    Some(1),
                    "past-screen boundary follows the blank gap"
                );
            }
        }

        let mut g = Grid::with_scrollback(8, 10, 100);
        let start = g.anchors_mut().place(8, AnchorRole::Start).unwrap();
        g.resize(4, 10);
        assert_eq!(g.visible_to_absolute(0), 0, "precondition: a trim");
        assert_eq!(g.anchors().row(start), Some(8), "precondition: past it");
        let lend = g.lend_anchors(LendReach::Ring).unwrap();
        g.resize(4, 12);
        g.take_back_anchors(lend);
        assert!(!g.anchors().lost(), "a trimmed-past Start is no unknown");
        assert_eq!(g.anchors().row(start), Some(1), "after the cursor's line");

        let mut g = Grid::with_scrollback(4, 10, 100);
        g.set_cursor(1, 0);
        g.write_char_at_cursor_packed(
            'p',
            crate::PackedColors::DEFAULT,
            crate::CellFlags::PROTECTED,
        );
        g.set_cursor(0, 0);
        let start = g.anchors_mut().place(3, AnchorRole::Start).unwrap();
        let lend = g.lend_anchors(LendReach::Ring).unwrap();
        g.resize(4, 12);
        g.take_back_anchors(lend);
        assert!(!g.anchors().lost());
        assert_eq!(
            g.anchors().row(start),
            Some(4),
            "protected ink is not a blank gap"
        );

        let mut g = Grid::with_scrollback(4, 8, 1000);
        g.write_emoji_autowrap_fast(
            '😀',
            crate::PackedColors::DEFAULT,
            crate::CellFlags::COMPLEX,
        );
        g.cell_extra_mut(0, 0).add_combining('\u{301}');
        g.carriage_return();
        g.line_feed();
        for line in ["one", "abcdefghij", "card", "tail"] {
            for ch in line.chars() {
                g.write_char_wrap(ch);
            }
            g.carriage_return();
            g.line_feed();
        }
        let oldest = g.oldest_absolute_row();
        let start = g.anchors_mut().place(oldest, AnchorRole::Start).unwrap();
        let lend = g.lend_anchors(LendReach::Ring).unwrap();
        g.resize(4, 20);
        g.take_back_anchors(lend);
        assert!(
            !g.anchors().lost(),
            "resolved ring-history text survived reflow"
        );
        assert!(g.anchors().row(start).is_some());
    }

    #[test]
    fn a_reflow_landing_inside_a_row_excludes_the_shared_row_for_start_and_end() {
        let mut g = grid_printing(6, 8, &["one", "abcdefghij", "two"]);
        let base = g.visible_to_absolute(0);
        let ij = (0..6)
            .map(|r| base + r)
            .find(|&r| row_text(&g, r) == "ij")
            .unwrap();
        let start = g.anchors_mut().place(ij, AnchorRole::Start).unwrap();
        let end = g.anchors_mut().place(ij, AnchorRole::End).unwrap();
        let lend = g.lend_anchors(LendReach::Ring).unwrap();
        g.resize(6, 20);
        g.take_back_anchors(lend);
        assert!(!g.anchors().lost());
        let line = g.anchors().row(end).unwrap();
        assert_eq!(
            row_text(&g, line),
            "abcdefghij",
            "an End goes to its line's head"
        );
        let s = g.anchors().row(start).unwrap();
        assert_eq!(
            row_text(&g, s),
            "two",
            "a Start goes to the next line's head"
        );
    }

    #[test]
    fn a_start_beyond_the_reach_is_clamped_to_the_oldest_line_read() {
        let mut g = grid_printing(4, 10, &["h0", "h1", "h2", "h3", "h4", "h5", "s0", "s1"]);
        let oldest = g.oldest_absolute_row();
        let start = g.anchors_mut().place(oldest, AnchorRole::Start).unwrap();
        let lend = g.lend_anchors(LendReach::Rows(2)).unwrap();
        g.resize(4, 12);
        g.take_back_anchors(lend);
        assert!(!g.anchors().lost());
        let s = g.anchors().row(start).unwrap();
        assert!(s > oldest, "clamped later, never earlier");
        assert_eq!(
            s,
            g.visible_to_absolute(0) - 2,
            "the oldest line the walk read"
        );
    }

    #[test]
    fn a_rewrap_that_changes_the_lines_marks_the_anchors_lost() {
        let mut g = grid_printing(4, 10, &["aaa", "bbb"]);
        let base = g.visible_to_absolute(0);
        g.anchors_mut().place(base, AnchorRole::Start).unwrap();
        let lend = g.lend_anchors(LendReach::Ring).unwrap();
        g.set_cursor(0, 0);
        g.write_char('X');
        g.set_cursor(2, 0);
        g.take_back_anchors(lend);
        assert!(g.anchors().lost());

        for (before, after, ring) in [
            ("😀", "🚀", false),
            ("😀", "🚀", true),
            ("👩\u{200d}💻", "👩\u{200d}🚀", false),
        ] {
            let mut g = grid_printing(4, 10, &["a", "b"]);
            let put = |g: &mut Grid, text: &str| {
                if ring {
                    g.set_cursor(0, 0);
                    g.write_emoji_autowrap_fast(
                        text.chars().next().unwrap(),
                        crate::PackedColors::DEFAULT,
                        crate::CellFlags::COMPLEX,
                    );
                } else {
                    g.set_cell_complex_char(0, 0, text);
                }
                g.set_cursor(2, 0);
            };
            put(&mut g, before);
            let base = g.visible_to_absolute(0);
            g.anchors_mut().place(base, AnchorRole::Start).unwrap();
            let lend = g.lend_anchors(LendReach::Ring).unwrap();
            put(&mut g, after);
            g.take_back_anchors(lend);
            assert!(
                g.anchors().lost(),
                "changed complex payload {before:?} → {after:?}"
            );
        }

        let mut g = grid_printing(4, 10, &["a", "b"]);
        g.set_cursor(0, 0);
        g.write_char_at_cursor_packed(
            'p',
            crate::PackedColors::DEFAULT,
            crate::CellFlags::PROTECTED,
        );
        g.set_cursor(2, 0);
        let base = g.visible_to_absolute(0);
        g.anchors_mut().place(base, AnchorRole::Start).unwrap();
        let lend = g.lend_anchors(LendReach::Ring).unwrap();
        g.set_cursor(0, 0);
        g.write_char_at_cursor_packed(
            'q',
            crate::PackedColors::DEFAULT,
            crate::CellFlags::PROTECTED,
        );
        g.set_cursor(2, 0);
        g.take_back_anchors(lend);
        assert!(
            g.anchors().lost(),
            "protected glyph changes are verified too"
        );
    }

    #[test]
    fn a_lend_of_an_empty_set_is_free() {
        let mut g = grid_printing(4, 10, &["a"]);
        assert!(g.lend_anchors(LendReach::Ring).is_none());
    }
}
