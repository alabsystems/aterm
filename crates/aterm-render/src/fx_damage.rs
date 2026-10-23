// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cell-row band damage tracking for `FxLayer`s and occluders across frames and scroll blits.

use std::ops::Range;
use std::sync::Arc;

use aterm_core::render::{FxLayer, FxSpace, PxRect};

/// Window-pixel geometry of a frame's cell-row bands (`grid_top` and `cell_h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowBands {
    /// Top edge of the cell grid in window pixels.
    pub grid_top: usize,
    /// Cell height in window pixels.
    pub cell_h: usize,
}

impl RowBands {
    /// Construct [`RowBands`] for a grid starting at `grid_top` with `cell_h`-pixel rows.
    #[inline]
    #[must_use]
    pub const fn new(grid_top: usize, cell_h: usize) -> Self {
        Self { grid_top, cell_h }
    }

    /// The band of a `rows`-row grid holding window pixel row `y` (`rows > 0`):
    /// row 0 also holds every pixel above the grid, the last row every pixel
    /// below it.
    #[inline]
    const fn band_of(self, y: i128, rows: usize) -> usize {
        let top = self.grid_top as i128;
        if y < top {
            return 0;
        }
        let pitch = if self.cell_h > 1 { self.cell_h } else { 1 };
        let d = y - top;
        // In u64 whenever it fits: one divide instruction, where i128's is a
        // library call (a changed layer pays this twice per item).
        let q = if d <= u64::MAX as i128 && pitch as u128 <= u64::MAX as u128 {
            (d as u64 / pitch as u64) as i128
        } else {
            d / pitch as i128
        };
        let r = if q > usize::MAX as i128 {
            usize::MAX
        } else {
            q as usize
        };
        if r < rows - 1 { r } else { rows - 1 }
    }

    /// Mark every band in `dirty` overlapped by window pixel rows `[y0, y1)`.
    #[inline]
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

    /// Window pixel row range `(y0, y1)` covered by grid cell `rows`.
    #[inline]
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
/// window space (`grid_top` down for a Grid layer).
#[inline]
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

/// Mark the bands `layer` may write: each item's rows (its bounds moved and
/// clipped as the layer draws it), not their hull, so a layer whose items
/// stand apart down the frame (the cursor's stars) marks the rows they stand
/// on, as their quads did, and none between.
///
/// Every item's rows lie inside the hull's bands, so the walk stops once each
/// of those is dirty: a changed layer whose old self already marked its bands
/// (a card redrawn in place) costs no walk, and one whose items tile their hull
/// (a card) stops at the first items that reach all of them. A hull of one band
/// is marked whole: at worst that over-marks a band whose items the clip cut
/// away, which damage allows.
#[inline]
fn mark_layer(dirty: &mut [bool], bands: RowBands, layer: &FxLayer) {
    let Some((y0, y1)) = window_rows(layer, bands) else {
        return;
    };
    let rows = dirty.len();
    if rows == 0 {
        return;
    }
    let (lo, hi) = (bands.band_of(y0, rows), bands.band_of(y1 - 1, rows));
    let mut open = dirty[lo..=hi].iter().filter(|d| !**d).count();
    if open == 0 {
        return;
    }
    if lo == hi {
        dirty[lo] = true;
        return;
    }
    let dy = match layer.space {
        FxSpace::Window => 0,
        FxSpace::Grid => bands.grid_top as i128,
    };
    for item in layer.content.items() {
        let r = item
            .bounds()
            .translate(layer.offset.0, layer.offset.1)
            .intersect(layer.clip);
        if r.is_empty() {
            continue;
        }
        let a = bands.band_of(i128::from(r.y0) + dy, rows);
        let b = bands.band_of(i128::from(r.y1) - 1 + dy, rows);
        for (i, d) in dirty.iter_mut().enumerate().take(b + 1).skip(a) {
            if !*d {
                *d = true;
                open -= usize::from((lo..=hi).contains(&i));
            }
        }
        if open == 0 {
            return;
        }
    }
}

/// The index in `other` of `list[i]`'s counterpart: the k-th layer of its id
/// in `other`, when `list[i]` is the k-th of that id in `list`.
#[inline]
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

/// Mark cell-row bands dirtied by fx-layer or occluder changes between `prev` and `cur`.
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
/// beside it disqualifies it too).
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

/// Mark cell-row bands dirtied by fx layers and occluders across a viewport scroll blit.
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
    /// `grid_top` moves every band; a Grid layer is offset by it.
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

    /// The band of a row is i128's quotient on both sides of where the
    /// distance below the grid stops fitting u64's one-instruction divide.
    #[test]
    fn a_band_is_the_same_on_either_side_of_u64() {
        let bands = RowBands::new(3, 7);
        let rows = usize::MAX;
        let edge = i128::from(u64::MAX);
        for d in [0, 6, 7, 700, edge - 1, edge, edge + 1, edge * 9] {
            let q = d / 7;
            let want = if q > (rows - 1) as i128 {
                rows - 1
            } else {
                q as usize
            };
            assert_eq!(bands.band_of(3 + d, rows), want, "{d} px below the grid");
        }
    }

    /// A changed layer marks the rows each of its items stands on, never the
    /// rows between two items far apart (the hull would); its clip still cuts
    /// an item, and a Grid layer still moves down by `grid_top`.
    #[test]
    fn a_changed_layer_marks_its_items_rows_not_their_hull() {
        let bands = RowBands::new(0, 10);
        let item = |y: i32| {
            FxItem::Rect(FxRect {
                x: 0,
                y,
                w: 4,
                h: 3,
                ink: 0,
            })
        };
        let sky = |space| {
            FxLayer::new(
                7,
                Arc::new(FxContent::new(
                    vec![item(2), item(71)],
                    vec![FxInk::add(0x0010_1010); 2],
                )),
                FxSlot::Glow,
                space,
                FxBlend::Exact,
            )
        };
        let mut dirty = vec![false; 10];
        mark_layer_changes(&mut dirty, bands, &[], &[sky(FxSpace::Window)]);
        assert_eq!(marked(&dirty), vec![0, 7], "two stars, two rows");

        let mut dirty = vec![false; 10];
        dirty[1..7].fill(true);
        mark_layer(&mut dirty, bands, &sky(FxSpace::Window));
        assert_eq!(
            marked(&dirty),
            (0..8).collect::<Vec<_>>(),
            "the walk stops only once the last open band is marked"
        );

        let mut cut = sky(FxSpace::Window);
        cut.clip = PxRect::new(0, 60, 100, 100);
        let mut dirty = vec![false; 10];
        mark_layer_changes(&mut dirty, bands, &[], &[cut]);
        assert_eq!(marked(&dirty), vec![7], "the clip cuts the item above it");

        let low = RowBands::new(15, 10);
        let mut dirty = vec![false; 10];
        mark_layer_changes(&mut dirty, low, &[sky(FxSpace::Window)], &[]);
        assert_eq!(
            marked(&dirty),
            vec![0, 5],
            "a Window layer under a 15 px head"
        );
        let mut dirty = vec![false; 10];
        mark_layer_changes(&mut dirty, low, &[sky(FxSpace::Grid)], &[]);
        assert_eq!(
            marked(&dirty),
            vec![0, 7],
            "a Grid layer sits grid_top lower"
        );
    }
}
