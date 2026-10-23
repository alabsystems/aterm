// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grid span-anchor hooks and row fingerprinting ([`AnchorSet`](crate::AnchorSet)).

use aterm_hash::FxHasher;
use std::hash::Hasher;
use std::sync::Arc;

use super::Grid;
use super::scroll_convert::{
    RowToLineCursorState, ScrolledRowExtras, next_combining, next_complex_char, next_keyed,
};
use crate::{AnchorSet, Cell, CellExtra, CellFlags, FenceKnob, Row, knob};

impl Grid {
    /// The holder fence's span anchors.
    #[must_use]
    #[inline]
    pub const fn anchors(&self) -> &AnchorSet {
        &self.storage.cursor_state.presentation.anchors
    }

    /// The holder fence's span anchors, to place, move and free them.
    #[inline]
    pub const fn anchors_mut(&mut self) -> &mut AnchorSet {
        &mut self.storage.cursor_state.presentation.anchors
    }

    /// Install a restored set (a checkpoint's).
    pub fn install_anchors(&mut self, set: AnchorSet, counter: u64) {
        self.storage.presentation.anchors = set;
        if self.storage.absolute_row_counter != counter {
            self.storage.presentation.anchors.lose();
        }
        self.anchors_clamp_to_oldest();
    }

    /// Screen rows `top..=bottom` moved by `delta` ([`AnchorSet`]).
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

    /// History was discarded (ED 3, `clear_scrollback`).
    #[inline]
    pub(crate) fn anchors_clamp_to_oldest(&mut self) {
        if self.storage.presentation.anchors.is_empty() {
            return;
        }
        let oldest = self.oldest_absolute_row();
        self.storage.presentation.anchors.clamp_below(oldest);
    }

    /// Rows moved in a way no anchor rule describes.
    #[inline]
    pub(crate) fn lose_anchors(&mut self) {
        self.storage.presentation.anchors.lose();
    }

    /// Rows were renumbered or rotated outside a lend.
    #[inline]
    pub(crate) fn lose_anchors_renumbered(&mut self) {
        self.storage.presentation.anchors.lose_renumbered();
    }

    /// History came back onto the screen under its keys (a rows-grow reveal).
    #[inline]
    pub(crate) fn note_rows_rekeyed(&mut self) {
        self.storage.presentation.anchors.note_rekeyed();
    }

    /// [`Self::invalidate_host_coordinates`] for an alt-screen swap.
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

    /// [`Self::invalidate_host_coordinates`] for a move preserving retained rows' absolute keys.
    #[inline]
    pub(crate) fn invalidate_host_coordinates_keeping_rows(&mut self) {
        self.storage.coordinates_invalidated = true;
        self.storage.presentation.poison_row_band_moves();
    }

    /// The live row at absolute `row` (ignoring the reader's scroll).
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

    /// A fingerprint of live screen row `screen_row` (ignoring the reader's scroll).
    #[must_use]
    pub fn live_row_fingerprint(&self, screen_row: u16) -> Option<u64> {
        let row = self.storage.row_at_screen(screen_row)?;
        fingerprint(row, |col, cell, spacer, h| {
            let data = self.cell_render_data(screen_row, col, *cell);
            if !spacer
                && ((cell.is_complex() && data.complex_char().is_none())
                    || (cell.fg_needs_overflow() && data.fg_rgb().is_none())
                    || (cell.bg_needs_overflow() && data.bg_rgb().is_none()))
            {
                return None;
            }
            let extra = data.cell_extra();
            let cluster = extra.and_then(crate::CellExtra::complex_char);
            let mut tiles: Vec<Tile> = extra.map_or_else(Vec::new, |e| {
                e.images()
                    .iter()
                    .map(|layer| {
                        (
                            layer.draw_key(),
                            Arc::as_ptr(&layer.image).addr(),
                            layer.cell_row,
                            layer.cell_col,
                        )
                    })
                    .collect()
            });
            hash_extras(
                h,
                &Payload {
                    cluster: match (cluster, data.complex_char()) {
                        (Some(cluster), _) => Cluster::Text(cluster),
                        (None, Some(base)) => Cluster::Base(base),
                        (None, None) => Cluster::None,
                    },
                    combining: extra.map_or(&[][..], crate::CellExtra::combining),
                    fg: data.fg_rgb(),
                    bg: data.bg_rgb(),
                    underline: extra.and_then(crate::CellExtra::underline_color_u32),
                    link: extra.and_then(|e| {
                        e.hyperlink()
                            .map(|url| (&**url, e.hyperlink_id().map(|id| &**id)))
                    }),
                },
                &mut tiles,
                spacer,
            );
            Some(())
        })
    }

    /// [`Self::live_row_fingerprint`] of the live row at absolute `row` on screen or in history.
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
        let empty = ScrolledRowExtras::default();
        let extras = self.storage.ring_history_extras(index).unwrap_or(&empty);
        let mut cursors = RowToLineCursorState::default();
        let mut underline = 0;
        let mut link = 0;
        fingerprint(cells, |col, cell, spacer, h| {
            let cluster = next_complex_char(extras, &mut cursors, col);
            let fg = next_keyed(&extras.rgb_fg, &mut cursors.rgb_fg_idx, col).copied();
            let bg = next_keyed(&extras.rgb_bg, &mut cursors.rgb_bg_idx, col).copied();
            if !spacer
                && ((cell.is_complex() && cluster.is_none())
                    || (cell.fg_needs_overflow() && fg.is_none())
                    || (cell.bg_needs_overflow() && bg.is_none()))
            {
                return None;
            }
            while extras
                .hyperlinks
                .get(link)
                .is_some_and(|span| span.end_col <= col)
            {
                link += 1;
            }
            let mut tiles: Vec<Tile> = extras
                .images
                .iter()
                .filter(|span| (span.start_col..span.end_col).contains(&col))
                .map(|span| {
                    (
                        aterm_types::layer_order(span.image.z_index, span.kitty),
                        Arc::as_ptr(&span.image).addr(),
                        span.image_row,
                        span.first_cell_col + (col - span.start_col),
                    )
                })
                .collect();
            hash_extras(
                h,
                &Payload {
                    cluster: cluster.map_or(Cluster::None, |t| Cluster::Text(t)),
                    combining: next_combining(extras, &mut cursors, col).unwrap_or(&[]),
                    fg,
                    bg,
                    underline: next_keyed(&extras.underline_colors, &mut underline, col).copied(),
                    link: extras
                        .hyperlinks
                        .get(link)
                        .filter(|span| span.start_col <= col)
                        .map(|span| (&*span.url, span.id.as_deref())),
                },
                &mut tiles,
                spacer,
            );
            Some(())
        })
    }

    /// Whether anything is drawn on the live screen from `(screen_row, col)` onward.
    #[must_use]
    pub fn has_ink_from(&self, screen_row: u16, col: u16) -> bool {
        let start = if knob(FenceKnob::LayTaintSkipsCursorRow) {
            screen_row.saturating_add(1)
        } else {
            screen_row
        };
        (start..self.storage.visible_rows)
            .any(|r| self.screen_row_inked(r, if r == screen_row { col } else { 0 }))
    }

    /// Whether live screen row `screen_row` shows anything from column `from` on.
    pub(super) fn screen_row_inked(&self, screen_row: u16, from: u16) -> bool {
        let Some(row) = self.storage.row_at_screen(screen_row) else {
            return false;
        };
        if row.line_size() != crate::LineSize::SingleWidth {
            return true;
        }
        row.as_slice()
            .iter()
            .zip(0u16..)
            .skip(usize::from(from))
            .any(|(cell, col)| {
                let extra = || {
                    cell.has_extras()
                        .then(|| self.storage.cell_extra(screen_row, col))
                        .flatten()
                };
                if row.is_cell_wide_continuation(col) {
                    extra().is_some_and(|e| !e.images().is_empty())
                } else {
                    cell_inked(cell, extra())
                }
            })
    }

    /// [`Self::screen_row_inked`] of the screen row at absolute `row`, from its first column.
    pub(super) fn absolute_row_inked(&self, row: u64) -> bool {
        row.checked_sub(self.visible_to_absolute(0))
            .and_then(|r| u16::try_from(r).ok())
            .is_some_and(|r| self.screen_row_inked(r, 0))
    }
}

/// The flags that draw on a blank cell.
const DRAWN_ON_BLANK: CellFlags =
    CellFlags(CellFlags::INVERSE.0 | CellFlags::ALL_UNDERLINES.0 | CellFlags::STRIKETHROUGH.0);

/// Whether `cell` (with its `extra` entry) renders anything non-blank.
#[must_use]
pub(super) fn cell_inked(cell: &Cell, extra: Option<&CellExtra>) -> bool {
    let flags = cell.flags();
    (cell.char_data() != u16::from(b' ') && cell.char_data() != 0)
        || cell.is_complex()
        || !cell.colors().bg_is_default()
        || flags.intersects(DRAWN_ON_BLANK)
        || flags.contains(CellFlags::OVERLINE)
        || extra.is_some_and(|e| {
            !e.images().is_empty() || !e.combining().is_empty() || e.hyperlink().is_some()
        })
}

/// A row's fingerprint.
fn fingerprint(
    row: &Row,
    mut extra: impl FnMut(u16, &Cell, bool, &mut FxHasher) -> Option<()>,
) -> Option<u64> {
    let mut h = FxHasher::default();
    h.write_u8(u8::from(row.is_wrapped()));
    h.write_u8(row.line_size() as u8);
    for (cell, col) in row.as_slice().iter().zip(0u16..) {
        h.write_u16(cell.char_data());
        h.write_u32(cell.colors().0);
        h.write_u16(cell.flags().bits());
        extra(col, cell, row.is_cell_wide_continuation(col), &mut h)?;
    }
    Some(h.finish())
}

/// A cell's cluster as one tier keeps it.
enum Cluster<'a> {
    None,
    Text(&'a str),
    Base(char),
}

/// One image layer over a cell.
type Tile = ((i32, u32, u32), usize, u16, u16);

/// A cell's extras resolved from whichever tier holds them.
struct Payload<'a> {
    cluster: Cluster<'a>,
    combining: &'a [char],
    fg: Option<[u8; 3]>,
    bg: Option<[u8; 3]>,
    /// SGR 58, packed (`CellExtra::underline_color_u32`).
    underline: Option<u32>,
    /// OSC 8: the URL and its `id=`.
    link: Option<(&'a str, Option<&'a str>)>,
}

/// Hash a cell's extras in one form whichever tier holds them.
fn hash_extras(h: &mut FxHasher, payload: &Payload<'_>, tiles: &mut [Tile], spacer: bool) {
    if !spacer {
        match payload.cluster {
            Cluster::None => {}
            Cluster::Text(text) => text.chars().for_each(|c| h.write_u32(u32::from(c))),
            Cluster::Base(c) => h.write_u32(u32::from(c)),
        }
        h.write_u32(u32::MAX);
        for c in payload.combining {
            h.write_u32(u32::from(*c));
        }
        h.write_u32(u32::MAX);
        for rgb in [payload.fg, payload.bg] {
            match rgb {
                Some(rgb) => {
                    h.write_u8(1);
                    h.write(&rgb);
                }
                None => h.write_u8(0),
            }
        }
        match payload.underline {
            Some(packed) => {
                h.write_u8(1);
                h.write_u32(packed);
            }
            None => h.write_u8(0),
        }
        match payload.link {
            Some((url, id)) => {
                h.write_u8(1);
                h.write(url.as_bytes());
                h.write_u8(0xFF);
                if let Some(id) = id {
                    h.write_u8(1);
                    h.write(id.as_bytes());
                }
                h.write_u8(0xFF);
            }
            None => h.write_u8(0),
        }
    }
    tiles.sort_by_key(|tile| tile.0);
    h.write_usize(tiles.len());
    for &(key, image, row, col) in tiles.iter() {
        h.write_i32(key.0);
        h.write_u32(key.1);
        h.write_u32(key.2);
        h.write_usize(image);
        h.write_u16(row);
        h.write_u16(col);
    }
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

    /// One `rows`×`cols` inline image.
    fn image(rows: u16, cols: u16) -> std::sync::Arc<crate::ImageData> {
        std::sync::Arc::new(crate::ImageData {
            bytes: vec![7u8; 16],
            format: crate::ImageFormat::Png,
            cols,
            rows,
            z_index: 0,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: crate::ImageScaling::Fit,
            source_rect: None,
        })
    }

    fn tile(image: &std::sync::Arc<crate::ImageData>, cell_col: u16) -> crate::ImageRef {
        crate::ImageRef {
            image: std::sync::Arc::clone(image),
            cell_row: 0,
            cell_col,
            kitty: None,
        }
    }

    /// Ink is anything an erased default cell does not show, whatever draws
    /// it: a reversed, underlined, struck or overlined blank, a blank under
    /// an image, a combining mark on a space, a linked space. A bold, dim,
    /// italic, blinking, hidden or superscript space, or one in a foreground
    /// colour, shows nothing.
    #[test]
    fn the_lay_scan_sees_every_ink_an_erased_cell_does_not_show() {
        let blank_with = |flags: crate::CellFlags, colors: crate::PackedColors| {
            let mut g = Grid::with_scrollback(4, 10, 100);
            g.set_cursor(2, 3);
            g.write_char_at_cursor_packed(' ', colors, flags);
            g.has_ink_from(1, 0)
        };
        let plain = crate::PackedColors::DEFAULT;
        for (flags, why) in [
            (crate::CellFlags::INVERSE, "reverse video"),
            (crate::CellFlags::UNDERLINE, "an underline"),
            (crate::CellFlags::DOUBLE_UNDERLINE, "a double underline"),
            (crate::CellFlags::CURLY_UNDERLINE, "a curly underline"),
            (crate::CellFlags::DOTTED_UNDERLINE, "a dotted underline"),
            (crate::CellFlags::DASHED_UNDERLINE, "a dashed underline"),
            (crate::CellFlags::STRIKETHROUGH, "a strikethrough"),
            (crate::CellFlags::OVERLINE, "an overline"),
        ] {
            assert!(blank_with(flags, plain), "{why} on a space is ink");
        }
        for (flags, why) in [
            (crate::CellFlags::BOLD, "bold"),
            (crate::CellFlags::DIM, "dim"),
            (crate::CellFlags::ITALIC, "italic"),
            (crate::CellFlags::BLINK, "blink"),
            (crate::CellFlags::HIDDEN, "hidden"),
            (crate::CellFlags::SUPERSCRIPT, "superscript"),
            (crate::CellFlags::SUBSCRIPT, "subscript"),
        ] {
            assert!(!blank_with(flags, plain), "a {why} space shows nothing");
        }
        assert!(
            !blank_with(
                crate::CellFlags::empty(),
                crate::PackedColors::with_indexed_fg(1)
            ),
            "a foreground colour shows nothing on a space"
        );
        assert!(
            blank_with(
                crate::CellFlags::empty(),
                crate::PackedColors::with_indexed_bg(1)
            ),
            "a background is ink"
        );

        let picture = image(1, 2);
        let mut g = Grid::with_scrollback(4, 10, 100);
        g.set_cell_image(3, 4, tile(&picture, 0));
        assert!(g.has_ink_from(1, 0), "an image below");
        assert!(g.has_ink_from(3, 4), "an image at the cursor");
        assert!(!g.has_ink_from(3, 5), "nothing right of it");

        let mut g = Grid::with_scrollback(4, 10, 100);
        g.set_cursor(2, 0);
        g.write_char(' ');
        g.cell_extra_mut(2, 0).add_combining('\u{301}');
        assert!(g.has_ink_from(1, 0), "an accent on a space");
        let mut g = Grid::with_scrollback(4, 10, 100);
        g.cell_extra_mut(2, 1)
            .set_hyperlink(Some(std::sync::Arc::from("https://example.com")));
        assert!(g.has_ink_from(1, 0), "a linked space");

        let mut g = Grid::with_scrollback(4, 10, 100);
        g.set_cursor(2, 0);
        assert!(g.write_wide_char_at_cursor_packed_ecols(
            '界',
            plain,
            crate::CellFlags::empty(),
            10,
        ));
        g.set_cell_image(2, 1, tile(&picture, 1));
        assert!(g.has_ink_from(2, 1), "the spacer's image tile");
    }

    /// A row's fingerprint reads what its extras paint as well as its cells:
    /// an image stacked on a cell that already had extras, a link, an
    /// underline colour. And the row keeps that value in the ring's history.
    #[test]
    fn a_fingerprint_reads_images_links_and_underline_colours_in_either_tier() {
        let mut g = Grid::with_scrollback(4, 10, 100);
        g.write_char('e');
        g.cell_extra_mut(0, 0).add_combining('\u{301}');
        g.write_char('x');
        let mut seen = vec![g.live_row_fingerprint(0).unwrap()];
        let mut changed = |g: &Grid, why: &str| {
            let now = g.live_row_fingerprint(0).unwrap();
            assert!(!seen.contains(&now), "{why} moves the fingerprint");
            seen.push(now);
        };
        let picture = image(1, 2);
        g.stack_cell_image(0, 0, tile(&picture, 0));
        changed(&g, "an image over a cell that had extras");
        g.stack_cell_image(0, 0, tile(&image(1, 2), 0));
        changed(&g, "a second layer");
        g.cell_extra_mut(0, 1)
            .set_hyperlink(Some(std::sync::Arc::from("https://example.com")));
        changed(&g, "a link");
        g.cell_extra_mut(0, 1)
            .set_hyperlink_id(Some(std::sync::Arc::from("id")));
        changed(&g, "a link's id");
        g.cell_extra_mut(0, 1).set_underline_color(Some([1, 2, 3]));
        changed(&g, "an underline colour");
        g.cell_extra_mut(0, 1)
            .set_underline_color_u32(Some(0x02_00_00_05));
        changed(&g, "an indexed underline colour");

        let on_screen = g.live_row_fingerprint(0);
        let absolute = g.visible_to_absolute(0);
        g.set_cursor(3, 0);
        for _ in 0..2 {
            g.line_feed();
        }
        assert!(g.visible_to_absolute(0) > absolute, "the row is history");
        assert_eq!(g.row_fingerprint_at_absolute(absolute), on_screen);

        let mut g = Grid::with_scrollback(4, 10, 100);
        assert!(g.write_wide_char_at_cursor_packed_ecols(
            '界',
            crate::PackedColors::DEFAULT,
            crate::CellFlags::empty(),
            10,
        ));
        g.set_cursor(0, 2);
        g.write_emoji_autowrap_fast(
            '😀',
            crate::PackedColors::DEFAULT,
            crate::CellFlags::COMPLEX,
        );
        g.set_cell_image(0, 1, tile(&picture, 1));
        assert!(g.row(0).unwrap().is_cell_wide_continuation(1));
        let cells = g.row(0).unwrap().as_slice().to_vec();
        let before = g.live_row_fingerprint(0).unwrap();
        let replacement = image(1, 2);
        g.set_cell_image(0, 1, tile(&replacement, 1));
        assert_eq!(g.row(0).unwrap().as_slice(), cells);
        let replaced = g.live_row_fingerprint(0).unwrap();
        assert_ne!(replaced, before, "the spacer's image payload changed");
        assert_eq!(g.live_row_fingerprint(0), Some(replaced));
        let absolute = g.visible_to_absolute(0);
        g.set_cursor(3, 0);
        g.line_feed();
        assert_eq!(g.row_fingerprint_at_absolute(absolute), Some(replaced));

        let tagged = |cell_col, serial| crate::ImageRef {
            kitty: Some(crate::KittyPlacementTag {
                image_id: 7,
                placement_id: u32::from(cell_col) + 1,
                image_serial: std::num::NonZeroU32::new(1).unwrap(),
                serial: std::num::NonZeroU32::new(serial).unwrap(),
            }),
            ..tile(&picture, cell_col)
        };
        let mut g = Grid::with_scrollback(4, 10, 100);
        g.set_cell_image(0, 0, tagged(0, 2));
        g.stack_cell_image(0, 0, tagged(1, 3));
        let cells = g.row(0).unwrap().as_slice().to_vec();
        let before = g.live_row_fingerprint(0).unwrap();
        assert_eq!(
            g.cell_extra(0, 0)
                .unwrap()
                .images()
                .iter()
                .map(|t| t.cell_col)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        g.set_cell_image(0, 0, tagged(1, 3));
        g.stack_cell_image(0, 0, tagged(0, 4));
        assert_eq!(g.row(0).unwrap().as_slice(), cells);
        assert_eq!(
            g.cell_extra(0, 0)
                .unwrap()
                .images()
                .iter()
                .map(|t| t.cell_col)
                .collect::<Vec<_>>(),
            [1, 0]
        );
        let reordered = g.live_row_fingerprint(0).unwrap();
        assert_ne!(reordered, before, "the image tile paint order changed");
        let absolute = g.visible_to_absolute(0);
        g.set_cursor(3, 0);
        g.line_feed();
        assert_eq!(g.row_fingerprint_at_absolute(absolute), Some(reordered));
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

    /// Grid with `history` lines scrolled off and numbered lines on screen.
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
