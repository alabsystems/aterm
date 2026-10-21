// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fx layers' damage (`docs/DESIGN-card-engine-2026-10-19.md` §4.4): which
//! cell-row bands a change of the layers or of the occluders between two frames
//! can repaint, for the shared dirty-row seam ([`crate::compute_dirty_rows`]) and
//! the scroll planner ([`crate::scroll_shift_plan`]).
//!
//! Layers compare by IDENTITY ([`FxLayer`]'s `PartialEq`: the content `Arc`,
//! never its bytes), so a settled layer costs a pointer compare and marks
//! nothing. Every pixel a layer writes lies in [`FxLayer::extent`] at its
//! space's origin (`fx_raster`'s `fx_placement_is_the_spec`), so marking the
//! bands of the previous and the current extent repaints every pixel the
//! change can touch: the vacated light is rebuilt away, the new light lands on
//! a freshly rebuilt band and never re-accumulates. No fingerprint is taken and
//! no payload is cloned.
//!
//! The band rule is the renderer's ([`crate::fx_raster::band_px`]): row `r`
//! spans window pixels `[grid_top + r·cell_h, grid_top + (r + 1)·cell_h)`, with
//! row 0 open to the frame's top and the last row to its bottom, so light in the
//! padding repaints with the edge row whose band holds it (the free-sprite
//! edge-row rule).

use std::ops::Range;
use std::sync::Arc;

use aterm_core::render::{FxLayer, FxSpace, PxRect};

/// Where a frame's cell-row bands lie in window pixels: the grid's top edge
/// and the band pitch. The damage seam needs it to map a window-space fx
/// layer, or an occluder, to the rows it repaints; both renderers hand the
/// seam their own (`Renderer::row_bands`), and a cache keyed on `grid_top`
/// guarantees the two frames compared share it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowBands {
    /// The grid's top edge in window pixels (`pad_top + head`).
    pub grid_top: usize,
    /// The cell height: the band pitch.
    pub cell_h: usize,
}

impl RowBands {
    /// The bands of a grid whose top edge is `grid_top` with `cell_h`-tall rows.
    #[must_use]
    pub const fn new(grid_top: usize, cell_h: usize) -> Self {
        Self { grid_top, cell_h }
    }

    /// The band of a `rows`-row grid holding window pixel row `y` (`rows > 0`):
    /// row 0 also holds every pixel above the grid, the last row every pixel
    /// below it.
    fn band_of(self, y: i128, rows: usize) -> usize {
        // Widen before subtracting: RowBands is public and usize origins
        // can exceed the positive i64 range even when dirty holds five rows.
        let top = self.grid_top as i128;
        if y < top {
            return 0;
        }
        // `y - top` is non-negative; past `usize` it is past every band.
        let r = usize::try_from((y - top) / self.cell_h.max(1) as i128).unwrap_or(usize::MAX);
        r.min(rows - 1)
    }

    /// Mark every band the window pixel rows `[y0, y1)` reach.
    pub(crate) fn mark_span(self, dirty: &mut [bool], y0: i128, y1: i128) {
        let rows = dirty.len();
        if rows == 0 || y0 >= y1 {
            return;
        }
        let (lo, hi) = (self.band_of(y0, rows), self.band_of(y1 - 1, rows));
        for d in &mut dirty[lo..=hi] {
            *d = true;
        }
    }

    /// The window pixel rows `[y0, y1)` the CELLS of `rows` cover:
    /// `[grid_top + start·cell_h, grid_top + end·cell_h)`, bounded (past
    /// `i32` it stops short of the unbounded sentinel). The vertical extent
    /// of a chrome occluder over those rows: chrome paints its rows' cells
    /// (and their full width), never the head or the padding above or below
    /// them, so light there keeps drawing. An empty `rows` gives an empty
    /// span.
    #[must_use]
    pub fn rows_px(self, rows: Range<usize>) -> (i32, i32) {
        if rows.start >= rows.end {
            return (0, 0);
        }
        let edge = |r: usize| {
            i32::try_from(self.grid_top.saturating_add(r.saturating_mul(self.cell_h)))
                .unwrap_or(i32::MAX - 1)
                .min(i32::MAX - 1)
        };
        (edge(rows.start), edge(rows.end))
    }
}

/// The window pixel rows `[y0, y1)` `layer` may write: its extent, moved to
/// window space (`grid_top` down for a Grid layer). `None` when it writes
/// nothing.
fn window_rows(layer: &FxLayer, bands: RowBands) -> Option<(i128, i128)> {
    let e = layer.extent();
    if e.is_empty() {
        return None;
    }
    let dy = match layer.space {
        FxSpace::Window => 0,
        FxSpace::Grid => bands.grid_top as i128,
    };
    Some((i128::from(e.y0) + dy, i128::from(e.y1) + dy))
}

/// Mark the bands `layer` may write.
fn mark_layer(dirty: &mut [bool], bands: RowBands, layer: &FxLayer) {
    if let Some((y0, y1)) = window_rows(layer, bands) {
        bands.mark_span(dirty, y0, y1);
    }
}

/// The index in `other` of `list[i]`'s counterpart: the k-th layer of its id
/// in `other`, when `list[i]` is the k-th of that id in `list`. Ids are the
/// host's stable names; a repeated id pairs in order. O(n), allocation-free.
fn counterpart(list: &[FxLayer], i: usize, other: &[FxLayer]) -> Option<usize> {
    let id = list[i].id;
    let k = list[..i].iter().filter(|l| l.id == id).count();
    other
        .iter()
        .enumerate()
        .filter(|(_, l)| l.id == id)
        .nth(k)
        .map(|(j, _)| j)
}

/// Mark what the change from `prev`'s layers to `cur`'s repaints.
///
/// Layers are matched by id. A matched pair whose tuple differs marks both
/// extents; a layer with no counterpart (appeared or vanished) marks its own;
/// an equal pair marks nothing, unless the equal pairs changed their relative
/// order, which can change their overlap's pixels (layers draw in list
/// order), and then every equal pair marks its extent.
fn mark_layer_changes(dirty: &mut [bool], bands: RowBands, prev: &[FxLayer], cur: &[FxLayer]) {
    let mut last_kept: Option<usize> = None;
    let mut reordered = false;
    for (i, layer) in cur.iter().enumerate() {
        match counterpart(cur, i, prev) {
            Some(j) if prev[j] == *layer => {
                reordered |= last_kept.is_some_and(|k| j < k);
                last_kept = Some(last_kept.map_or(j, |k| k.max(j)));
            }
            Some(j) => {
                mark_layer(dirty, bands, &prev[j]);
                mark_layer(dirty, bands, layer);
            }
            None => mark_layer(dirty, bands, layer),
        }
    }
    for (j, layer) in prev.iter().enumerate() {
        if counterpart(prev, j, cur).is_none() {
            mark_layer(dirty, bands, layer);
        }
    }
    if reordered {
        for (i, layer) in cur.iter().enumerate() {
            if counterpart(cur, i, prev).is_some_and(|j| prev[j] == *layer) {
                mark_layer(dirty, bands, layer);
            }
        }
    }
}

/// Mark what a change of the occluders repaints: an occluder present in one
/// frame and not the other changes pixels only where it meets a layer of
/// either frame, so the bands of that meeting are marked (rows only; an
/// occluder beside a layer on its rows over-marks, never under-marks).
fn mark_occluder_changes(
    dirty: &mut [bool],
    bands: RowBands,
    (prev_layers, cur_layers): (&[FxLayer], &[FxLayer]),
    (prev_occ, cur_occ): (&[PxRect], &[PxRect]),
) {
    let changed = prev_occ
        .iter()
        .filter(|o| !cur_occ.contains(o))
        .chain(cur_occ.iter().filter(|o| !prev_occ.contains(o)));
    for o in changed {
        if o.is_empty() {
            continue;
        }
        for layer in prev_layers.iter().chain(cur_layers) {
            if let Some((y0, y1)) = window_rows(layer, bands) {
                bands.mark_span(dirty, y0.max(i128::from(o.y0)), y1.min(i128::from(o.y1)));
            }
        }
    }
}

/// THE FX ARM of [`crate::compute_dirty_rows`]: mark the bands the change of
/// the fx layers and occluders from `prev` to `cur` repaints, and say whether
/// they differ in a way that can change a pixel (the frame must not gate-hit).
///
/// Occluders only subtract from layers, so with no layer in either frame
/// their change paints nothing and is not damage.
pub(crate) fn mark_fx_changes(
    dirty: &mut [bool],
    bands: RowBands,
    (prev_layers, prev_occ): (&[FxLayer], &[PxRect]),
    (cur_layers, cur_occ): (&[FxLayer], &[PxRect]),
) -> bool {
    let layers_changed = prev_layers != cur_layers;
    let occluders_changed =
        !(prev_layers.is_empty() && cur_layers.is_empty()) && prev_occ != cur_occ;
    if layers_changed {
        mark_layer_changes(dirty, bands, prev_layers, cur_layers);
    }
    if occluders_changed {
        mark_occluder_changes(dirty, bands, (prev_layers, cur_layers), (prev_occ, cur_occ));
    }
    layers_changed || occluders_changed
}

/// Whether `cur` is `prev` moved by the blit and by nothing else, so the blit
/// already put its light where `cur` draws it: the same layer (id, content
/// `Arc`, slot, space, blend, bloom, clip) at the same x and `da` rows higher,
/// its light cut by its clip in neither frame, inside the grid band in both
/// (the blit moves the band's pixels and keeps the padding), and no occluder of
/// either frame on its rows (an occluder cuts light; rows only, so an occluder
/// beside it disqualifies it too). A layer's pixels are its content's at the
/// pixel less its offset, whatever band slices them, so the slid light is the
/// light a full render draws there. The caller pairs layers by INDEX: two such
/// layers that overlap keep their order.
fn rides_the_blit(
    prev: &FxLayer,
    cur: &FxLayer,
    bands: RowBands,
    rows: usize,
    da: i32,
    occluders: [&[PxRect]; 2],
) -> bool {
    let moved = i128::from(prev.offset.1) - i128::from(da) * bands.cell_h as i128;
    if !(prev.id == cur.id
        && Arc::ptr_eq(&prev.content, &cur.content)
        && prev.slot == cur.slot
        && prev.space == cur.space
        && prev.blend == cur.blend
        && prev.bloom == cur.bloom
        && prev.clip == cur.clip
        && prev.offset.0 == cur.offset.0
        && i128::from(cur.offset.1) == moved)
    {
        return false;
    }
    let top = bands.grid_top as i128;
    let bottom = top.saturating_add((rows as i128).saturating_mul(bands.cell_h as i128));
    [prev, cur].into_iter().all(|layer| {
        let whole = layer
            .content
            .extent()
            .translate(layer.offset.0, layer.offset.1);
        let Some((y0, y1)) = window_rows(layer, bands) else {
            return false;
        };
        whole == layer.extent()
            && top <= y0
            && y1 <= bottom
            && occluders
                .iter()
                .flat_map(|o| o.iter())
                .all(|o| o.is_empty() || i128::from(o.y1) <= y0 || y1 <= i128::from(o.y0))
    })
}

/// THE FX ARM of [`crate::scroll_shift_plan`]: the layers never refuse a
/// scroll blit; their light is rebuilt over the slid pixels instead.
///
/// The blit moved every pixel of the grid band (`[grid_top, grid_top +
/// rows·cell_h)`, full width) by `da` rows, light included, and kept the
/// padding above and below it in place. So `prev`'s light now lies at its
/// grid-band rows shifted by `−da` and in the edge rows whose bands hold its
/// padding part; both are marked, and so is every band of `cur`'s layers,
/// whose light the damaged tail stamps over the rebuilt bands. Occluders need
/// no term of their own: they only cut light inside the extents marked here.
///
/// Except a layer that RIDES THE BLIT ([`rides_the_blit`], paired with the
/// layer at its index in the other frame): an inline card riding its stream,
/// moved by exactly the scroll, whose light the blit carried to its new
/// place. Neither of its extents is marked, so its rows are slid, not rebuilt.
pub(crate) fn mark_fx_scroll(
    dirty: &mut [bool],
    bands: RowBands,
    (prev_layers, prev_occ): (&[FxLayer], &[PxRect]),
    (cur_layers, cur_occ): (&[FxLayer], &[PxRect]),
    da: i32,
) {
    let rows = dirty.len();
    if rows == 0 {
        return;
    }
    let rides = |i: usize| {
        prev_layers
            .get(i)
            .zip(cur_layers.get(i))
            .is_some_and(|(p, c)| rides_the_blit(p, c, bands, rows, da, [prev_occ, cur_occ]))
    };
    let top = bands.grid_top as i128;
    // Public band geometry need not fit a framebuffer. Saturation places a
    // still-higher bottom beyond every possible i32 layer extent plus usize top.
    let bottom = top.saturating_add((rows as i128).saturating_mul(bands.cell_h as i128));
    for (i, layer) in prev_layers.iter().enumerate() {
        let Some((y0, y1)) = window_rows(layer, bands) else {
            continue;
        };
        if rides(i) {
            continue;
        }
        if y0 < top {
            dirty[0] = true;
        }
        if y1 > bottom {
            dirty[rows - 1] = true;
        }
        let (g0, g1) = (y0.max(top), y1.min(bottom));
        if g0 < g1 {
            let (lo, hi) = (bands.band_of(g0, rows), bands.band_of(g1 - 1, rows));
            for b in lo..=hi {
                // Row `b`'s pixels now show at row `b − da`; off the grid they
                // fell out of the frame.
                let r = b as i128 - i128::from(da);
                if (0..rows as i128).contains(&r) {
                    dirty[r as usize] = true;
                }
            }
        }
    }
    for (i, layer) in cur_layers.iter().enumerate() {
        if !rides(i) {
            mark_layer(dirty, bands, layer);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aterm_core::render::{FxBlend, FxContent, FxInk, FxItem, FxRect, FxSlot};

    use super::*;

    fn layer(id: u64, space: FxSpace, y: i32, h: u16) -> FxLayer {
        FxLayer::new(
            id,
            Arc::new(FxContent::new(
                vec![FxItem::Rect(FxRect {
                    x: 0,
                    y,
                    w: 4,
                    h,
                    ink: 0,
                })],
                vec![FxInk::add(0x0010_1010); 2],
            )),
            FxSlot::Under,
            space,
            FxBlend::Exact,
        )
    }

    fn marked(dirty: &[bool]) -> Vec<usize> {
        dirty
            .iter()
            .enumerate()
            .filter_map(|(r, &d)| d.then_some(r))
            .collect()
    }

    /// The band rule: row 0 opens to the top, the last row to the bottom, and
    /// `grid_top` moves every band; a Grid layer is offset by it. A chrome
    /// span is its rows' cells.
    #[test]
    fn bands_follow_the_renderer_band_rule() {
        let bands = RowBands::new(7, 10);
        let mut dirty = vec![false; 5];
        bands.mark_span(&mut dirty, -40, 3);
        assert_eq!(marked(&dirty), vec![0], "above the grid is row 0's band");
        dirty.fill(false);
        bands.mark_span(&mut dirty, 16, 18);
        assert_eq!(
            marked(&dirty),
            vec![0, 1],
            "y 16 and 17 lie in [7, 17) and [17, 27)"
        );
        dirty.fill(false);
        bands.mark_span(&mut dirty, 17, 18);
        assert_eq!(marked(&dirty), vec![1]);
        dirty.fill(false);
        bands.mark_span(&mut dirty, 56, 9_000);
        assert_eq!(
            marked(&dirty),
            vec![4],
            "below the grid is the last row's band"
        );
        dirty.fill(false);
        bands.mark_span(&mut dirty, 20, 20);
        assert!(marked(&dirty).is_empty(), "an empty span marks nothing");

        let mut dirty = vec![false; 5];
        mark_layer(&mut dirty, bands, &layer(1, FxSpace::Grid, 10, 1));
        assert_eq!(marked(&dirty), vec![1], "grid y 10 is window y 17");
        dirty.fill(false);
        mark_layer(&mut dirty, bands, &layer(1, FxSpace::Window, 10, 1));
        assert_eq!(marked(&dirty), vec![0], "window y 10 is in row 0's band");

        assert_eq!(bands.rows_px(0..1), (7, 17), "a row's cells, not its band");
        assert_eq!(bands.rows_px(2..4), (27, 47));
        assert_eq!(bands.rows_px(3..5), (37, 57));
        assert_eq!(bands.rows_px(3..3), (0, 0));
        assert_eq!(RowBands::new(7, usize::MAX).rows_px(0..2).1, i32::MAX - 1);

        // Large public geometry still maps tiny layers; it never wraps through
        // the signed origin or multiplies in usize before the scroll decision.
        for top in [1usize << (usize::BITS - 1), usize::MAX] {
            let bands = RowBands::new(top, 10);
            dirty.fill(false);
            mark_layer(&mut dirty, bands, &layer(2, FxSpace::Window, 0, 1));
            assert_eq!(
                marked(&dirty),
                vec![0],
                "the window layer is above a huge grid"
            );
            dirty.fill(false);
            mark_layer(&mut dirty, bands, &layer(3, FxSpace::Grid, 10, 1));
            assert_eq!(
                marked(&dirty),
                vec![1],
                "grid-local row survives its huge origin"
            );
            dirty.fill(false);
            mark_fx_scroll(
                &mut dirty,
                bands,
                (&[layer(4, FxSpace::Grid, 10, 1)], &[]),
                (&[], &[]),
                1,
            );
            assert_eq!(marked(&dirty), vec![0], "the old light slides up one row");
        }
        let bands = RowBands::new(0, usize::MAX);
        dirty.fill(false);
        mark_layer(&mut dirty, bands, &layer(5, FxSpace::Window, 10, 1));
        assert_eq!(
            marked(&dirty),
            vec![0],
            "a huge pitch has a positive first band"
        );
        dirty.fill(false);
        mark_fx_scroll(
            &mut dirty,
            bands,
            (&[layer(6, FxSpace::Window, 10, 1)], &[]),
            (&[], &[]),
            -1,
        );
        assert_eq!(
            marked(&dirty),
            vec![1],
            "a huge-pitch scroll preserves its row shift"
        );
    }

    /// A reorder of two unchanged overlapping layers changes their overlap's
    /// pixels, so it is damage; a repeated id pairs in order.
    #[test]
    fn a_reorder_of_equal_layers_marks_them() {
        let bands = RowBands::new(0, 10);
        let (a, b) = (
            layer(1, FxSpace::Window, 0, 5),
            layer(2, FxSpace::Window, 30, 5),
        );
        let mut dirty = vec![false; 6];
        mark_layer_changes(
            &mut dirty,
            bands,
            &[a.clone(), b.clone()],
            &[a.clone(), b.clone()],
        );
        assert!(marked(&dirty).is_empty(), "equal layers mark nothing");
        mark_layer_changes(
            &mut dirty,
            bands,
            &[a.clone(), b.clone()],
            &[b.clone(), a.clone()],
        );
        assert_eq!(marked(&dirty), vec![0, 3], "a swap marks both");

        // Two layers share id 9: the second one moves.
        let (c, d) = (
            layer(9, FxSpace::Window, 10, 5),
            layer(9, FxSpace::Window, 40, 5),
        );
        let mut moved = d.clone();
        moved.offset.1 += 10;
        let mut dirty = vec![false; 6];
        mark_layer_changes(&mut dirty, bands, &[c.clone(), d], &[c, moved]);
        assert_eq!(
            marked(&dirty),
            vec![4, 5],
            "the k-th of an id pairs with the k-th"
        );
    }
}
