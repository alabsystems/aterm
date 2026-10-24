// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared phosphor rain fixture for GPU rendering tests.

use aterm_effects::matrix_rain::{
    EffectGeom, MatrixRain, RainCell, RainConfig, RainLayers, RainTickInput,
};
use aterm_render::{RainHalo, RenderInput};

/// Deterministic replay seed.
pub const RAIN_SEED: u64 = 0xA7E2_11D3;

const CALM_TICK_MS: u64 = 83;

const DRIVE_CAP: usize = 2000;

/// A live rain field over a fixed grid snapshot.
pub struct RainScene {
    pub engine: MatrixRain,
    geom: EffectGeom,
    /// Current emission, row-sorted.
    pub cells: Vec<RainCell>,
    /// Current bright-head halos (premultiplied additive light).
    pub add: Vec<RainHalo>,
    /// The rain's layer publisher: it lives across frames, as the host's does,
    /// so a row whose cells held is a layer that held.
    layers: RainLayers,
}

impl RainScene {
    /// Build an enabled engine over `base` and run the initial occupancy scan.
    pub fn new(rows: usize, cols: usize, cell: (usize, usize), base: &RenderInput) -> Self {
        let cfg = RainConfig {
            enabled: true,
            density: 12,
            seed: RAIN_SEED,
            default_bg: base.default_bg,
            output_material: false,
            ..RainConfig::default()
        };
        let mut engine = MatrixRain::new(cfg);
        engine.rescan_from_cells(
            &base.cells,
            &base.line_sizes,
            &base.images,
            rows,
            cols,
            base.default_bg,
            1,
        );
        Self {
            engine,
            geom: EffectGeom {
                cell_w: cell.0 as u16,
                cell_h: cell.1 as u16,
                rows: rows as u16,
                cols: cols as u16,
            },
            cells: Vec::new(),
            add: Vec::new(),
            layers: RainLayers::default(),
        }
    }

    fn tick_input() -> RainTickInput<'static> {
        RainTickInput {
            cursor: None,
            hidden_band: &[],
            sel: None,
            display_offset: 0,
            is_alt_screen: false,
        }
    }

    /// Advance one engine tick and emit the frame fingerprint.
    pub fn tick(&mut self) -> u64 {
        self.engine.note_keystroke();
        self.engine.advance_ms(CALM_TICK_MS);
        self.emit()
    }

    /// Re-emit without advancing the clock.
    pub fn emit(&mut self) -> u64 {
        let fp = self.engine.emit(
            self.geom,
            &Self::tick_input(),
            &mut self.cells,
            &mut self.add,
        );
        self.cells.sort_by_key(|q| q.row);
        fp
    }

    /// Tick until a frame with tinted cells, a head halo, and baked tiles.
    pub fn drive_until_raining(&mut self) -> u64 {
        for i in 0..DRIVE_CAP {
            let fp = self.tick();
            if i % 100 == 0 {
                eprintln!(
                    "drive tick {i}: cells={} add={} tint0={:#x}",
                    self.cells.len(),
                    self.add.len(),
                    self.cells.first().map_or(0, |q| q.tint)
                );
            }
            if i >= 8
                && !self.add.is_empty()
                && self.cells.iter().any(|q| q.tint != 0x00FF_FFFF)
                && self.baker_complete()
            {
                return fp;
            }
        }
        panic!("MatrixRain produced no tinted + haloed frame within {DRIVE_CAP} ticks");
    }

    /// Whether every glyph tile is baked (a cell whose tile is not draws
    /// nothing).
    pub fn baker_complete(&self) -> bool {
        self.engine.baker().complete()
    }

    /// The cells of the current emission whose tile is baked: the ones that
    /// draw.
    pub fn drawn(&self) -> impl Iterator<Item = &RainCell> {
        self.cells
            .iter()
            .filter(|c| self.engine.tile(c.glyph).is_some() && c.alpha > 0)
    }

    /// Load the current emission into `input`: its cells as the rain's fx
    /// layers, its halos into `rain_add`.
    pub fn apply(&mut self, input: &mut RenderInput) {
        self.apply_cells(input);
        input.rain_add.clone_from(&self.add);
    }

    /// Load only the current emission's cells into `input`, as the rain's fx
    /// layers.
    pub fn apply_cells(&mut self, input: &mut RenderInput) {
        self.layers
            .publish(&self.cells, self.engine.baker(), &mut input.fx_layers);
    }
}
