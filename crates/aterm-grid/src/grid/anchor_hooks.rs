// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grid span-anchor hooks and row fingerprinting ([`AnchorSet`]).

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

    /// Whether the live screen row at absolute `row` shows anything, by the lay taint's ink test
    /// (a row off the screen shows nothing).
    #[must_use]
    pub fn absolute_row_inked(&self, row: u64) -> bool {
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
#[path = "anchor_hooks_tests.rs"]
mod tests;
