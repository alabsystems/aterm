// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The grid's side of the holder fence: its span anchors
//! ([`AnchorSet`](crate::AnchorSet)) applied at the row-motion choke points,
//! and the row reads the fence makes (a row's fingerprint for a seal, on
//! the screen or in the ring's history, and the lay row's ink).
//!
//! Every hook returns on one branch while no anchor lives.

use aterm_hash::FxHasher;
use std::hash::Hasher;

use super::Grid;
use crate::{AnchorSet, FenceKnob, Row, knob};

impl Grid {
    /// The holder fence's span anchors.
    #[must_use]
    #[inline]
    pub fn anchors(&self) -> &AnchorSet {
        &self.storage.presentation.anchors
    }

    /// The holder fence's span anchors, to place, move and free them.
    #[inline]
    pub fn anchors_mut(&mut self) -> &mut AnchorSet {
        &mut self.storage.presentation.anchors
    }

    /// Install a restored set (a checkpoint's): its rows were keyed under
    /// absolute row counter `counter`, so a grid that numbers its rows
    /// otherwise marks them lost, and one below the oldest row clamps to it.
    pub fn install_anchors(&mut self, set: AnchorSet, counter: u64) {
        self.storage.presentation.anchors = set;
        if self.storage.absolute_row_counter != counter {
            self.storage.presentation.anchors.lose();
        }
        self.anchors_clamp_to_oldest();
    }

    /// Screen rows `top..=bottom` moved by `delta`: carry the anchors in that
    /// band (see [`AnchorSet`](crate::AnchorSet)). The screen's absolute base
    /// is read here, before or after the move alike: a band move adds nothing
    /// to history.
    #[inline]
    pub(crate) fn anchors_band_move(&mut self, top: u16, bottom: u16, delta: i64) {
        if self.storage.presentation.anchors.is_empty() {
            return;
        }
        let base = self.visible_to_absolute(0);
        self.storage
            .presentation
            .anchors
            .band_move(base, top, bottom, delta);
    }

    /// `inserted` rows were inserted at absolute `at`.
    #[inline]
    pub(crate) fn anchors_splice(&mut self, at: u64, inserted: u64) {
        self.storage.presentation.anchors.splice(at, inserted);
    }

    /// History was discarded (ED 3, `clear_scrollback`): an anchor in it
    /// clamps to the oldest row left, so a span wholly in it empties.
    #[inline]
    pub(crate) fn anchors_clamp_to_oldest(&mut self) {
        if self.storage.presentation.anchors.is_empty() {
            return;
        }
        let oldest = self.oldest_absolute_row();
        self.storage.presentation.anchors.clamp_below(oldest);
    }

    /// Rows moved in a way no anchor rule describes: a rectangle shifted or
    /// copied across rows (a margined IL/DL or region scroll, DECCRA).
    #[inline]
    pub(crate) fn lose_anchors(&mut self) {
        self.storage.presentation.anchors.lose();
    }

    /// Rows were renumbered or rotated outside a lend.
    #[inline]
    pub(crate) fn lose_anchors_renumbered(&mut self) {
        self.storage.presentation.anchors.lose_renumbered();
    }

    /// History came back onto the screen under its keys (a rows-grow reveal):
    /// a seal's history rows are writable again.
    #[inline]
    pub(crate) fn note_rows_rekeyed(&mut self) {
        self.storage.presentation.anchors.note_rekeyed();
    }

    /// [`Self::invalidate_host_coordinates`] for an alt-screen swap: the band
    /// record is poisoned as for any swap, but this grid's rows did not move,
    /// so its anchors stay where they are.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "Alt1047",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    #[inline]
    pub fn invalidate_host_coordinates_for_swap(&mut self) {
        self.storage.coordinates_invalidated = true;
        self.storage.presentation.poison_row_band_moves();
        if knob(FenceKnob::SwapLoses) {
            self.lose_anchors();
        }
    }

    /// [`Self::invalidate_host_coordinates`] for a move that keeps every
    /// retained row's absolute key (a rows-only top-demote: the rows cross
    /// into history under the keys they had).
    #[inline]
    pub(crate) fn invalidate_host_coordinates_keeping_rows(&mut self) {
        self.storage.coordinates_invalidated = true;
        self.storage.presentation.poison_row_band_moves();
    }

    /// The live row at absolute `row` (ignoring the reader's scroll): on the
    /// screen, or in the ring's history. `None` past the screen and for
    /// history in the tiered store or the lazy buffer.
    #[must_use]
    pub(crate) fn live_row_at_absolute(&self, row: u64) -> Option<&Row> {
        let base = self.visible_to_absolute(0);
        if row >= base {
            let screen = u16::try_from(row - base).ok()?;
            if screen >= self.storage.visible_rows {
                return None;
            }
            return self.storage.row_at_screen(screen);
        }
        let back = usize::try_from(base - row).ok()?;
        let ring = self.storage.ring_buffer_scrollback();
        if back > ring || row < self.oldest_absolute_row() {
            return None;
        }
        self.storage.ring_history_row(ring - back)
    }

    /// A fingerprint of live screen row `screen_row` (ignoring the reader's
    /// scroll): every cell's character, colours and flags, the extras behind
    /// them (a cluster, combining marks, true colours, wherever the screen
    /// keeps each), and the row's wrap and size. Equality of these resolved
    /// values is subject to a 64-bit collision. `None` past the screen or
    /// when a required complex/RGB overflow value cannot be resolved.
    #[must_use]
    pub fn live_row_fingerprint(&self, screen_row: u16) -> Option<u64> {
        let row = self.storage.row_at_screen(screen_row)?;
        fingerprint(row, |col, cell, h| {
            let data = self.cell_render_data(screen_row, col, *cell);
            if (cell.is_complex() && data.complex_char().is_none())
                || (cell.fg_needs_overflow() && data.fg_rgb().is_none())
                || (cell.bg_needs_overflow() && data.bg_rgb().is_none())
            {
                return None;
            }
            let extra = data.cell_extra();
            let cluster = extra.and_then(crate::CellExtra::complex_char);
            hash_extras(
                h,
                match (cluster, data.complex_char()) {
                    (Some(cluster), _) => Cluster::Text(cluster),
                    (None, Some(base)) => Cluster::Base(base),
                    (None, None) => Cluster::None,
                },
                extra.map_or(&[][..], crate::CellExtra::combining),
                data.fg_rgb(),
                data.bg_rgb(),
            );
            Some(())
        })
    }

    /// [`Self::live_row_fingerprint`] of the live row at absolute `row`, on
    /// the screen or in the ring's history: a row that scrolled off keeps its
    /// value, its extras read where the scroll moved them. `None` past the
    /// screen and for history the ring no longer holds.
    #[must_use]
    pub fn row_fingerprint_at_absolute(&self, row: u64) -> Option<u64> {
        let base = self.visible_to_absolute(0);
        if row >= base {
            return self.live_row_fingerprint(u16::try_from(row - base).ok()?);
        }
        let back = usize::try_from(base - row).ok()?;
        let ring = self.storage.ring_buffer_scrollback();
        if back > ring || row < self.oldest_absolute_row() {
            return None;
        }
        let index = ring - back;
        let cells = self.storage.ring_history_row(index)?;
        let extras = self.storage.ring_history_extras(index);
        fingerprint(cells, |col, cell, h| {
            let cluster = extras.and_then(|e| at_col(&e.complex_chars, col));
            let fg = extras.and_then(|e| at_col(&e.rgb_fg, col)).copied();
            let bg = extras.and_then(|e| at_col(&e.rgb_bg, col)).copied();
            if (cell.is_complex() && cluster.is_none())
                || (cell.fg_needs_overflow() && fg.is_none())
                || (cell.bg_needs_overflow() && bg.is_none())
            {
                return None;
            }
            hash_extras(
                h,
                cluster.map_or(Cluster::None, |t| Cluster::Text(t)),
                extras
                    .and_then(|e| at_col(&e.combining, col))
                    .map_or(&[][..], |m| m),
                fg,
                bg,
            );
            Some(())
        })
    }

    /// Whether anything is drawn on the live screen from `(screen_row, col)`
    /// onward: on that row from `col`, and on every row below. Drawn means a
    /// glyph other than a space, or a background other than the default.
    /// The holder fence's lay taint (O(screen), once per drawer cut).
    #[must_use]
    pub fn has_ink_from(&self, screen_row: u16, col: u16) -> bool {
        let start = if knob(FenceKnob::LayTaintSkipsCursorRow) {
            screen_row.saturating_add(1)
        } else {
            screen_row
        };
        (start..self.storage.visible_rows).any(|r| {
            let from = if r == screen_row { col } else { 0 };
            self.storage
                .row_at_screen(r)
                .is_some_and(|row| row_inked(row, from))
        })
    }
}

/// A row's fingerprint: its wrap and size, every cell's character, colours
/// and flags, and what `extra` hashes for each non-spacer cell (its column,
/// the cell and the hasher), so a screen row and the same row in history hash
/// alike.
fn fingerprint(
    row: &Row,
    mut extra: impl FnMut(u16, &crate::Cell, &mut FxHasher) -> Option<()>,
) -> Option<u64> {
    let mut h = FxHasher::default();
    h.write_u8(u8::from(row.is_wrapped()));
    h.write_u8(row.line_size() as u8);
    for (cell, col) in row.as_slice().iter().zip(0u16..) {
        h.write_u16(cell.char_data());
        h.write_u32(cell.colors().0);
        h.write_u16(cell.flags().bits());
        // A wide glyph's spacer repeats its lead's colours on the screen
        // only: the lead carries them in either tier.
        // Dense complex/RGB rings do not require HAS_EXTRAS. Bit 10 alone
        // also denotes a protected glyph, so only the row can name a spacer.
        if !row.is_cell_wide_continuation(col) {
            extra(col, cell, &mut h)?;
        }
    }
    Some(h.finish())
}

/// The value a history row's extras keep for column `col`.
fn at_col<T>(entries: &[(u16, T)], col: u16) -> Option<&T> {
    entries.iter().find(|(c, _)| *c == col).map(|(_, v)| v)
}

/// A cell's cluster as one tier keeps it: the whole text, or a base scalar
/// (the screen's dense ring for a lone non-BMP scalar).
enum Cluster<'a> {
    None,
    Text(&'a str),
    Base(char),
}

/// Hash a cell's extras in one form whichever tier holds them: the cluster's
/// scalars, its combining marks, its true colours.
fn hash_extras(
    h: &mut FxHasher,
    cluster: Cluster<'_>,
    combining: &[char],
    fg: Option<[u8; 3]>,
    bg: Option<[u8; 3]>,
) {
    match cluster {
        Cluster::None => {}
        Cluster::Text(text) => text.chars().for_each(|c| h.write_u32(u32::from(c))),
        Cluster::Base(c) => h.write_u32(u32::from(c)),
    }
    h.write_u32(u32::MAX);
    for c in combining {
        h.write_u32(u32::from(*c));
    }
    h.write_u32(u32::MAX);
    for rgb in [fg, bg] {
        match rgb {
            Some(rgb) => {
                h.write_u8(1);
                h.write(&rgb);
            }
            None => h.write_u8(0),
        }
    }
}

/// Whether `row` is drawn from `from` onward: ignore only genuine wide
/// spacers, keeping protected ordinary and wide lead glyphs.
pub(super) fn row_inked(row: &Row, from: u16) -> bool {
    row.as_slice()
        .iter()
        .zip(0u16..)
        .skip(usize::from(from))
        .any(|(cell, col)| {
            !row.is_cell_wide_continuation(col)
                && ((cell.char_data() != u16::from(b' ') && cell.char_data() != 0)
                    || cell.is_complex()
                    || !cell.colors().bg_is_default())
        })
}

#[cfg(test)]
mod tests {
    use crate::{AnchorRole, Grid};

    fn grid_with(text: &[&str]) -> Grid {
        let mut g = Grid::with_scrollback(4, 10, 100);
        for (r, line) in text.iter().enumerate() {
            g.set_cursor(u16::try_from(r).unwrap(), 0);
            for ch in line.chars() {
                g.write_char(ch);
            }
        }
        g
    }

    #[test]
    fn the_lay_scan_reads_the_cursor_row_from_its_column_and_every_row_below() {
        let g = grid_with(&["ab", "", "  x"]);
        assert!(g.has_ink_from(0, 1), "'b' at column 1");
        assert!(g.has_ink_from(1, 0), "row 2's 'x' lies below");
        assert!(!g.has_ink_from(3, 0));
        let g = grid_with(&["ab"]);
        assert!(!g.has_ink_from(0, 2), "nothing from column 2 on");
        assert!(g.has_ink_from(0, 0));

        let mut g = Grid::with_scrollback(4, 10, 100);
        g.write_char_at_cursor_packed(
            'p',
            crate::PackedColors::DEFAULT,
            crate::CellFlags::PROTECTED,
        );
        assert!(g.has_ink_from(0, 0), "a protected glyph is not a spacer");
        let mut g = Grid::with_scrollback(4, 10, 100);
        assert!(g.write_wide_char_at_cursor_packed_ecols(
            '界',
            crate::PackedColors::DEFAULT,
            crate::CellFlags::PROTECTED,
            10,
        ));
        assert!(g.has_ink_from(0, 0), "the protected wide lead is ink");
        assert!(
            !g.has_ink_from(0, 1),
            "its genuine spacer is not another glyph"
        );
    }

    #[test]
    fn a_fingerprint_moves_iff_the_row_changes() {
        let mut g = grid_with(&["abc", "abc"]);
        let a = g.live_row_fingerprint(0).unwrap();
        assert_eq!(a, g.live_row_fingerprint(1).unwrap());
        g.set_cursor(0, 1);
        g.write_char('X');
        assert_ne!(a, g.live_row_fingerprint(0).unwrap());
        assert_eq!(g.live_row_fingerprint(9), None);

        // Actual fast writes keep the codepoint outside the extras map.
        let mut g = Grid::with_scrollback(4, 10, 100);
        g.write_emoji_autowrap_fast(
            '😀',
            crate::PackedColors::DEFAULT,
            crate::CellFlags::COMPLEX,
        );
        let first = g.live_row_fingerprint(0).unwrap();
        assert_eq!(g.live_row_fingerprint(0), Some(first));
        g.set_cursor(0, 0);
        g.write_emoji_autowrap_fast(
            '🚀',
            crate::PackedColors::DEFAULT,
            crate::CellFlags::COMPLEX,
        );
        assert_ne!(g.live_row_fingerprint(0), Some(first), "ring glyph changed");

        // RGB-only bulk/BCE writes also resolve through a dense ring.
        let colors = crate::PackedColors::DEFAULT.with_rgb_fg().with_rgb_bg();
        g.set_cursor(1, 0);
        g.write_char_at_cursor_packed('x', colors, crate::CellFlags::empty());
        g.extras_mut()
            .set_rgb_ring_range(1, 0, 1, Some([1, 2, 3]), Some([4, 5, 6]), 4, 10);
        let first = g.live_row_fingerprint(1).unwrap();
        assert_eq!(g.live_row_fingerprint(1), Some(first));
        g.extras_mut()
            .set_rgb_ring_range(1, 0, 1, Some([3, 2, 1]), Some([4, 5, 6]), 4, 10);
        let second = g.live_row_fingerprint(1).unwrap();
        assert_ne!(second, first, "ring foreground changed");
        g.extras_mut()
            .set_rgb_ring_range(1, 0, 1, Some([3, 2, 1]), Some([6, 5, 4]), 4, 10);
        assert_ne!(
            g.live_row_fingerprint(1),
            Some(second),
            "ring background changed"
        );

        for flags in [
            crate::CellFlags::COMPLEX,
            crate::CellFlags::COMPLEX | crate::CellFlags::PROTECTED,
        ] {
            let mut g = Grid::with_scrollback(4, 10, 100);
            g.write_emoji_autowrap_fast('😀', crate::PackedColors::DEFAULT, flags);
            let first = g.live_row_fingerprint(0).unwrap();
            g.set_cursor(0, 0);
            g.write_emoji_autowrap_fast('🚀', crate::PackedColors::DEFAULT, flags);
            let changed = g.live_row_fingerprint(0).unwrap();
            assert_ne!(
                changed, first,
                "protected and ordinary dense glyphs are resolved"
            );
            let absolute = g.visible_to_absolute(0);
            g.set_cursor(3, 0);
            g.line_feed();
            assert_eq!(g.row_fingerprint_at_absolute(absolute), Some(changed));
        }

        let mut g = Grid::with_scrollback(4, 10, 100);
        let colors = crate::PackedColors::DEFAULT.with_rgb_fg().with_rgb_bg();
        g.write_char_at_cursor_packed('p', colors, crate::CellFlags::PROTECTED);
        g.extras_mut()
            .set_rgb_ring_range(0, 0, 1, Some([1, 2, 3]), Some([4, 5, 6]), 4, 10);
        let first = g.live_row_fingerprint(0).unwrap();
        g.extras_mut()
            .set_rgb_ring_range(0, 0, 1, Some([3, 2, 1]), Some([4, 5, 6]), 4, 10);
        let changed = g.live_row_fingerprint(0).unwrap();
        assert_ne!(changed, first, "protected RGB payload is resolved");
        let absolute = g.visible_to_absolute(0);
        g.set_cursor(3, 0);
        g.line_feed();
        assert_eq!(g.row_fingerprint_at_absolute(absolute), Some(changed));

        let mut g = Grid::with_scrollback(4, 10, 100);
        g.write_char_at_cursor_packed('x', colors, crate::CellFlags::empty());
        assert_eq!(
            g.live_row_fingerprint(0),
            None,
            "missing required RGB cannot vouch for a row"
        );
        let absolute = g.visible_to_absolute(0);
        g.set_cursor(3, 0);
        g.line_feed();
        assert_eq!(g.row_fingerprint_at_absolute(absolute), None);
    }

    /// A grid with `history` lines scrolled into history and the screen full
    /// of numbered lines, the cursor on the last row.
    fn numbered(rows: u16, history: usize) -> Grid {
        let mut g = Grid::with_scrollback(rows, 10, 1000);
        for n in 0..history + usize::from(rows) {
            g.carriage_return();
            for ch in format!("L{n}").chars() {
                g.write_char(ch);
            }
            if n + 1 < history + usize::from(rows) {
                g.line_feed();
            }
        }
        g
    }

    #[test]
    fn insert_and_delete_lines_carry_anchors_with_their_band() {
        let mut g = numbered(8, 0);
        let base = g.visible_to_absolute(0);
        let below_cursor = g.anchors_mut().place(base + 4, AnchorRole::Start).unwrap();
        let above_cursor = g.anchors_mut().place(base + 1, AnchorRole::End).unwrap();
        g.set_cursor(2, 0);
        g.insert_lines(2);
        assert_eq!(g.anchors().row(below_cursor), Some(base + 6));
        assert_eq!(
            g.anchors().row(above_cursor),
            Some(base + 1),
            "above the band"
        );
        g.delete_lines(3);
        assert_eq!(g.anchors().row(below_cursor), Some(base + 3));
        assert!(!g.anchors().lost());

        // A span ending before the inserted band owns none of its new blanks.
        let mut g = numbered(4, 0);
        let base = g.visible_to_absolute(0);
        let before = g.anchors_mut().place(base, AnchorRole::Start).unwrap();
        let end = g.anchors_mut().place(base + 1, AnchorRole::End).unwrap();
        let moved = g.anchors_mut().place(base + 1, AnchorRole::Start).unwrap();
        g.set_cursor(1, 0);
        g.insert_lines(1);
        assert_eq!(g.anchors().row(before), Some(base));
        assert_eq!(
            g.anchors().row(end),
            Some(base + 1),
            "inserted row excluded"
        );
        assert_eq!(g.anchors().row(moved), Some(base + 2), "old row followed");
    }

    #[test]
    fn region_scrolls_carry_anchors_and_a_full_screen_scroll_keeps_their_keys() {
        let mut g = numbered(8, 0);
        let base = g.visible_to_absolute(0);
        let id = g.anchors_mut().place(base + 5, AnchorRole::Start).unwrap();
        g.set_scroll_region(2, 6);
        g.scroll_region_down(1);
        assert_eq!(g.anchors().row(id), Some(base + 6));
        g.scroll_region_up(3);
        assert_eq!(g.anchors().row(id), Some(base + 3));
        g.set_scroll_region(0, 7);
        g.set_cursor(7, 0);
        g.line_feed();
        assert_eq!(g.anchors().row(id), Some(base + 3), "history keeps the key");
        assert!(!g.anchors().lost());
    }

    #[test]
    fn the_archival_scroll_splices_and_an_end_at_the_boundary_stays() {
        let mut g = numbered(8, 2);
        let base = g.visible_to_absolute(0);
        // The transcript is rows 0..=5, the footer rows 6..=7.
        let end = g.anchors_mut().place(base + 6, AnchorRole::End).unwrap();
        let footer = g.anchors_mut().place(base + 6, AnchorRole::Start).unwrap();
        let inside = g.anchors_mut().place(base + 3, AnchorRole::Start).unwrap();
        g.set_scroll_region(0, 5);
        g.set_cursor(5, 0);
        g.line_feed();
        assert_eq!(g.anchors().row(end), Some(base + 6), "End == at stays");
        assert_eq!(
            g.anchors().row(footer),
            Some(base + 7),
            "Start == at shifts"
        );
        assert_eq!(
            g.anchors().row(inside),
            Some(base + 3),
            "the region keeps its keys"
        );
        assert!(!g.anchors().lost());
    }

    #[test]
    fn a_margined_line_insert_loses_the_anchors() {
        let mut g = numbered(8, 0);
        let row = g.visible_to_absolute(3);
        g.anchors_mut().place(row, AnchorRole::Start).unwrap();
        g.set_horizontal_margins(2, 5);
        g.set_cursor(1, 2);
        g.insert_lines_margined(1, true);
        assert!(g.anchors().lost());
    }

    #[test]
    fn ed3_clamps_an_anchor_in_history_to_the_oldest_row() {
        let mut g = numbered(4, 6);
        let oldest = g.oldest_absolute_row();
        let start = g
            .anchors_mut()
            .place(oldest + 1, AnchorRole::Start)
            .unwrap();
        let screen = g.visible_to_absolute(1);
        let kept = g.anchors_mut().place(screen, AnchorRole::End).unwrap();
        g.erase_scrollback();
        assert_eq!(g.anchors().row(start), Some(g.oldest_absolute_row()));
        assert_eq!(g.anchors().row(kept), Some(screen));
        assert!(!g.anchors().lost());
    }

    #[test]
    fn an_unscroll_loses_the_anchors_unless_the_knob_keeps_them() {
        for knob in [false, true] {
            let mut g = numbered(4, 6);
            let row = g.visible_to_absolute(1);
            g.anchors_mut().place(row, AnchorRole::Start).unwrap();
            let run = |g: &mut Grid| assert_eq!(g.unscroll_from_scrollback(2), 2);
            if knob {
                crate::with_knob(crate::FenceKnob::UnlentRenumberKeeps, || run(&mut g));
            } else {
                run(&mut g);
            }
            assert_eq!(g.anchors().lost(), !knob);
        }
    }

    #[test]
    fn a_rows_only_resize_keeps_every_anchor_on_its_row() {
        let mut g = numbered(8, 4);
        let base = g.visible_to_absolute(0);
        let id = g.anchors_mut().place(base + 2, AnchorRole::Start).unwrap();
        // Shrink (the cursor's at the bottom: the top rows demote into
        // history), then grow (they come back).
        g.resize(5, 10);
        assert_eq!(g.anchors().row(id), Some(base + 2));
        let epoch = g.anchors().seal_epoch();
        g.resize(8, 10);
        assert_eq!(g.anchors().row(id), Some(base + 2));
        assert!(!g.anchors().lost());
        assert_ne!(
            g.anchors().seal_epoch(),
            epoch,
            "a reveal rekeys sealed rows"
        );
    }

    #[test]
    fn an_unlent_width_rewrap_loses_the_anchors() {
        let mut g = numbered(4, 2);
        let row = g.visible_to_absolute(1);
        g.anchors_mut().place(row, AnchorRole::Start).unwrap();
        g.resize(4, 12);
        assert!(g.anchors().lost());
    }

    #[test]
    fn a_swap_invalidation_keeps_anchors_and_a_plain_one_loses_them() {
        let mut g = grid_with(&["a"]);
        let id = g.anchors_mut().place(0, AnchorRole::Start).unwrap();
        g.invalidate_host_coordinates_for_swap();
        assert!(!g.anchors().lost());
        assert_eq!(g.anchors().row(id), Some(0));
        g.invalidate_host_coordinates();
        assert!(g.anchors().lost());
    }
}
