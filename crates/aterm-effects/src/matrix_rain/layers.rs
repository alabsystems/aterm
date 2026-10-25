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
    pub fn publish(&mut self, cells: &[RainCell], tiles: &RainBaker, layers: &mut Vec<FxLayer>) {
        withdraw(layers, &self.ids);
        self.ids.clear();
        self.next.clear();
        for v in &mut self.by_row {
            v.clear();
        }
        for c in cells {
            let r = usize::from(c.row);
            if self.by_row.len() <= r {
                self.by_row.resize_with(r + 1, Vec::new);
            }
            self.by_row[r].push(*c);
        }
        let at = layers
            .iter()
            .position(|l| l.blend == FxBlend::Linear && l.slot == FxSlot::Under)
            .unwrap_or(layers.len());
        let version = tiles.version();
        let mut held = std::mem::take(&mut self.held);
        for (r, row_cells) in self.by_row.iter().enumerate() {
            if row_cells.is_empty() {
                continue;
            }
            let row = r as u16;
            let entry = match held.iter().position(|h| h.row == row) {
                Some(i) => {
                    let mut prev = held.swap_remove(i);
                    if prev.version != version || prev.cells != *row_cells {
                        prev.version = version;
                        prev.cells.clone_from(row_cells);
                        prev.content = Arc::new(row_content(row_cells, tiles));
                    }
                    prev
                }
                None => Row {
                    row,
                    version,
                    cells: row_cells.clone(),
                    content: Arc::new(row_content(row_cells, tiles)),
                },
            };
            if entry.content.items().is_empty() {
                continue;
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
        }
        layers.splice(at..at, self.fresh.drain(..));
        held.clear();
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

/// One row's sprites over a palette of its distinct inks.
fn row_content(cells: &[RainCell], tiles: &RainBaker) -> FxContent {
    let mut palette: Vec<FxInk> = Vec::new();
    let mut items = Vec::with_capacity(cells.len());
    for c in cells {
        if c.alpha == 0 || c.w == 0 || c.h == 0 {
            continue;
        }
        let Some(art) = tiles.tile(c.glyph) else {
            continue;
        };
        let ink = FxInk::over(c.tint & 0x00FF_FFFF, c.alpha);
        let k = palette.iter().position(|&p| p == ink).unwrap_or_else(|| {
            palette.push(ink);
            palette.len() - 1
        });
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
mod tests {
    use super::super::rom::rasterize_master;
    use super::*;

    fn baker() -> RainBaker {
        let rom = rasterize_master();
        let mut b = RainBaker::default();
        b.begin_frame(8, 16);
        while !b.complete() {
            b.bake_tiles(&rom);
        }
        b
    }

    const fn cell(row: u16, col: u16, glyph: u8) -> RainCell {
        RainCell {
            row,
            x: col * 8,
            y: row * 16,
            w: 8,
            h: 16,
            glyph,
            tint: 0x0022_CC44,
            alpha: 90,
            flip_x: false,
        }
    }

    #[test]
    fn one_layer_a_row_and_an_unchanged_row_is_an_unchanged_layer() {
        let tiles = baker();
        let mut rain = RainLayers::default();
        let mut a = Vec::new();
        let cells = [cell(1, 0, 3), cell(1, 2, 4), cell(4, 1, 5)];
        rain.publish(&cells, &tiles, &mut a);
        assert_eq!(a.len(), 2, "rows 1 and 4");
        assert_eq!(a[0].content.items().len(), 2);
        assert_eq!(a[0].content.palette().len(), 1, "one ink, deduplicated");
        let mut b = a.clone();
        rain.publish(
            &[cell(1, 0, 3), cell(1, 2, 4), cell(4, 1, 6)],
            &tiles,
            &mut b,
        );
        assert_eq!(a[0], b[0], "row 1 kept its cells: the same layer");
        assert_ne!(a[1], b[1], "row 4 changed glyph: a new layer");
    }

    #[test]
    fn the_rain_sits_under_every_other_linear_under_layer() {
        let tiles = baker();
        let mut rain = RainLayers::default();
        let mut pet = aterm_render::sprites::SpriteLayers::new(9);
        let art = tiles.tile(0).expect("baked").clone();
        let body = aterm_render::sprites::FreeSprite {
            z: aterm_render::sprites::FreeZ::UnderText,
            ..aterm_render::sprites::FreeSprite::whole(0, 0, art)
        };
        let mut layers = Vec::new();
        pet.publish(std::slice::from_ref(&body), &mut layers);
        rain.publish(&[cell(0, 0, 1)], &tiles, &mut layers);
        assert!(layers[0].id == rain.ids()[0] && layers[1].id == pet.ids()[0]);
        assert!(
            is_rain_row(&layers[0]) && !is_rain_row(&layers[1]),
            "the pet knocks: no rain row"
        );
        assert!(
            !aterm_render::sprites::is_sprite_layer(&layers[0])
                && aterm_render::sprites::is_sprite_layer(&layers[1]),
            "a rain row is no companion sprite"
        );
        rain.publish(&[cell(0, 0, 2)], &tiles, &mut layers);
        assert!(
            layers[0].id == rain.ids()[0],
            "a republish stays under the pet"
        );
        pet.publish(std::slice::from_ref(&body), &mut layers);
        assert!(layers[0].id == rain.ids()[0], "…in either publish order");
        rain.withdraw(&mut layers);
        assert_eq!(layers.len(), 1);
    }

    #[test]
    fn an_unbaked_tile_draws_nothing_and_a_rebake_rebuilds_the_row() {
        let rom = rasterize_master();
        let mut tiles = RainBaker::default();
        tiles.begin_frame(8, 16);
        tiles.bake_tiles(&rom);
        let mut rain = RainLayers::default();
        let mut a = Vec::new();
        rain.publish(&[cell(0, 0, 1), cell(0, 1, 63)], &tiles, &mut a);
        assert_eq!(a[0].content.items().len(), 1, "glyph 63 is not baked yet");
        while !tiles.complete() {
            tiles.bake_tiles(&rom);
        }
        let mut b = a.clone();
        rain.publish(&[cell(0, 0, 1), cell(0, 1, 63)], &tiles, &mut b);
        assert_eq!(
            b[0].content.items().len(),
            2,
            "a new tile set rebuilds the row"
        );
    }
}
