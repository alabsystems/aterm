// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rain's cells as fx layers (`docs/DESIGN-card-engine-2026-10-19.md` §6), one a grid row
//! ([`RainLayers`]), so the renderer repaints only the rows the rain moved in.

use std::sync::Arc;

use aterm_core::render::{
    FxBlend, FxContent, FxInk, FxItem, FxLayer, FxSlot, FxSpace, FxSprite, PxRect,
};

use super::RainBaker;
use aterm_render::sprites::withdraw;

/// One rain cell: glyph `glyph`'s baked tile ([`RainBaker::tile`]) drawn
/// `w × h` at `(x, y)` (grid-interior pixels) in grid row `row`, tinted and
/// faded under the Linear law.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RainCell {
    /// The grid row the cell lies in (its layer).
    pub row: u16,
    /// Left edge.
    pub x: u16,
    /// Top edge.
    pub y: u16,
    /// Width (the cell width: tiles draw 1:1).
    pub w: u16,
    /// Height (the cell height).
    pub h: u16,
    /// The glyph, `0..64`.
    pub glyph: u8,
    /// Multiplied into the tile's white (`0x00RRGGBB`).
    pub tint: u32,
    /// Multiplied into the tile's coverage.
    pub alpha: u8,
    /// Mirror the tile horizontally.
    pub flip_x: bool,
}

impl RainCell {
    /// Zeroed transparent rain cell.
    pub const EMPTY: Self = Self {
        row: 0,
        x: 0,
        y: 0,
        w: 0,
        h: 0,
        glyph: 0,
        tint: 0,
        alpha: 0,
        flip_x: false,
    };
}

impl Default for RainCell {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// One published row: its cells, the tile-set version they were drawn
/// against, and the content they built.
#[derive(Debug)]
struct Row {
    row: u16,
    version: u64,
    cells: Vec<RainCell>,
    content: Arc<FxContent>,
}

/// Publishes a frame's [`RainCell`]s as fx layers, one a grid row, inserted
/// under every other Linear [`FxSlot::Under`] layer (the companions stand on
/// the rain). A row whose cells and tile set did not change keeps its
/// content's `Arc`, so its layer is unchanged.
#[derive(Debug)]
pub struct RainLayers {
    /// The ids this publisher put into the frame last.
    ids: Vec<u64>,
    held: Vec<Row>,
    next: Vec<Row>,
    /// Per-row scatter scratch: the cells of each row, in emission order.
    by_row: Vec<Vec<RainCell>>,
    /// The layers a publish inserts, kept for its capacity.
    fresh: Vec<FxLayer>,
}

impl Default for RainLayers {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// The salt of the rain's layer ids.
const RAIN_SALT: u64 = 0x7261_696E_2D72_6F77;

impl RainLayers {
    /// Construct an empty row-layer publisher.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ids: Vec::new(),
            held: Vec::new(),
            next: Vec::new(),
            by_row: Vec::new(),
            fresh: Vec::new(),
        }
    }
    /// The rain's declared CPU-GPU parity: the Linear law's bar (a glyph's
    /// white coverage under its cell's tint and fade blends in linear light).
    pub const PARITY: u8 = FxBlend::Linear.parity();

    /// Replace this publisher's layers in `layers` with `cells`', drawn from
    /// `tiles` (a cell whose tile is not baked yet draws nothing).
    #[inline]
    pub fn publish(&mut self, cells: &[RainCell], tiles: &RainBaker, layers: &mut Vec<FxLayer>) {
        if cells.is_empty() {
            self.withdraw(layers);
            return;
        }
        withdraw(layers, &self.ids);
        self.ids.clear();
        self.next.clear();
        let mut at = layers.len();
        let mut li = 0;
        while li < layers.len() {
            if matches!(layers[li].blend, FxBlend::Linear)
                && matches!(layers[li].slot, FxSlot::Under)
            {
                at = li;
                break;
            }
            li += 1;
        }
        let version = tiles.version();
        let mut held = std::mem::take(&mut self.held);
        {
            let mut held_iter = held.drain(..).peekable();
            let mut emit_row = |row: u16, row_cells: &[RainCell]| {
                while held_iter.peek().is_some_and(|h| h.row < row) {
                    let _ = held_iter.next();
                }
                let entry = if held_iter.peek().is_some_and(|h| h.row == row) {
                    let mut prev = held_iter.next().expect("peeked matching row");
                    if prev.version != version || prev.cells.as_slice() != row_cells {
                        prev.version = version;
                        prev.cells.clear();
                        prev.cells.extend_from_slice(row_cells);
                        prev.content = Arc::new(row_content(row_cells, tiles));
                    }
                    prev
                } else {
                    Row {
                        row,
                        version,
                        cells: row_cells.to_vec(),
                        content: Arc::new(row_content(row_cells, tiles)),
                    }
                };
                if entry.content.items().is_empty() {
                    return;
                }
                let id = crate::genome::mix(RAIN_SALT ^ u64::from(row));
                let layer = FxLayer::new(
                    id,
                    Arc::clone(&entry.content),
                    FxSlot::Under,
                    FxSpace::Grid,
                    FxBlend::Linear,
                );
                self.ids.push(id);
                self.fresh.push(layer);
                self.next.push(entry);
            };
            if cells_sorted_by_row(cells) {
                let mut start = 0;
                while start < cells.len() {
                    let row = cells[start].row;
                    let mut end = start + 1;
                    while end < cells.len() && cells[end].row == row {
                        end += 1;
                    }
                    emit_row(row, &cells[start..end]);
                    start = end;
                }
            } else {
                let mut bi = 0;
                while bi < self.by_row.len() {
                    self.by_row[bi].clear();
                    bi += 1;
                }
                let mut ci = 0;
                while ci < cells.len() {
                    let c = cells[ci];
                    let r = usize::from(c.row);
                    if self.by_row.len() <= r {
                        self.by_row.resize_with(r + 1, Vec::new);
                    }
                    self.by_row[r].push(c);
                    ci += 1;
                }
                let mut r = 0;
                while r < self.by_row.len() {
                    let row_cells = &self.by_row[r];
                    let cur_r = r;
                    r += 1;
                    if !row_cells.is_empty() {
                        emit_row(cur_r as u16, row_cells);
                    }
                }
            }
        }
        layers.splice(at..at, self.fresh.drain(..));
        self.held = std::mem::replace(&mut self.next, held);
    }

    /// Take this publisher's layers out of `layers` and forget its rows.
    #[inline]
    pub fn withdraw(&mut self, layers: &mut Vec<FxLayer>) {
        withdraw(layers, &self.ids);
        self.ids.clear();
        self.held.clear();
    }

    /// The ids of the layers the last publish put into the frame.
    #[inline]
    #[must_use]
    pub const fn ids(&self) -> &[u64] {
        self.ids.as_slice()
    }
}

/// Whether `layer` has a rain row's shape: a Grid-space Linear
/// [`FxSlot::Under`] layer that knocks nothing out (a companion under the text
/// knocks its letters out, [`aterm_render::sprites::is_sprite_layer`]).
#[must_use]
#[inline]
pub const fn is_rain_row(layer: &FxLayer) -> bool {
    matches!(layer.slot, FxSlot::Under)
        && matches!(layer.blend, FxBlend::Linear)
        && matches!(layer.space, FxSpace::Grid)
        && !layer.knock
}

#[inline]
pub(super) const fn cells_sorted_by_row(cells: &[RainCell]) -> bool {
    let mut i = 1;
    while i < cells.len() {
        if cells[i - 1].row > cells[i].row {
            return false;
        }
        i += 1;
    }
    true
}

/// One row's sprites over a palette of its distinct inks.
#[inline]
fn row_content(cells: &[RainCell], tiles: &RainBaker) -> FxContent {
    let mut palette: Vec<FxInk> = Vec::new();
    let mut items = Vec::with_capacity(cells.len());
    let mut ci = 0;
    while ci < cells.len() {
        let c = &cells[ci];
        ci += 1;
        if c.alpha == 0 || c.w == 0 || c.h == 0 {
            continue;
        }
        let Some(art) = tiles.tile(c.glyph) else {
            continue;
        };
        let ink = FxInk::over(c.tint & 0x00FF_FFFF, c.alpha);
        let mut k = palette.len();
        let mut pi = 0;
        while pi < palette.len() {
            if palette[pi] == ink {
                k = pi;
                break;
            }
            pi += 1;
        }
        if k == palette.len() {
            palette.push(ink);
        }
        items.push(FxItem::Sprite(FxSprite {
            x: i32::from(c.x),
            y: i32::from(c.y),
            dw: c.w,
            dh: c.h,
            flip_x: c.flip_x,
            art: Arc::clone(art),
            ink: k as u32,
            clip: PxRect::EVERYWHERE,
        }));
    }
    FxContent::new(items, palette)
}

#[cfg(test)]
#[path = "layers_tests.rs"]
mod tests;
