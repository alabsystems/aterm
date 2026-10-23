// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared phosphor rain fixture for GPU rendering tests.

use std::sync::Arc;

use aterm_effects::matrix_rain::{EffectGeom, MatrixRain, RainConfig, RainTickInput};
use aterm_render::{RainHalo, RenderInput, SceneAtlas, SpriteQuad};

/// Deterministic replay seed.
pub const RAIN_SEED: u64 = 0xA7E2_11D3;

const CALM_TICK_MS: u64 = 83;

const DRIVE_CAP: usize = 2000;

/// A live rain field over a fixed grid snapshot.
pub struct RainScene {
    pub engine: MatrixRain,
    geom: EffectGeom,
    /// Current emission, row-sorted.
    pub quads: Vec<SpriteQuad>,
    /// Current bright-head halos (premultiplied additive light).
    pub add: Vec<RainHalo>,
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
            quads: Vec::new(),
            add: Vec::new(),
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
            &mut self.quads,
            &mut self.add,
        );
        self.quads.sort_by_key(|q| q.row);
        fp
    }

    /// Tick until a frame with tinted glyph quads, a head halo, and a baked atlas.
    pub fn drive_until_raining(&mut self) -> u64 {
        for i in 0..DRIVE_CAP {
            let fp = self.tick();
            if i % 100 == 0 {
                eprintln!(
                    "drive tick {i}: quads={} add={} tint0={:#x}",
                    self.quads.len(),
                    self.add.len(),
                    self.quads.first().map_or(0, |q| q.tint)
                );
            }
            if i >= 8
                && !self.quads.is_empty()
                && !self.add.is_empty()
                && self.quads.iter().any(|q| q.tint != 0x00FF_FFFF)
            {
                return fp;
            }
        }
        panic!("MatrixRain produced no tinted + haloed frame within {DRIVE_CAP} ticks");
    }

    /// The versioned rain atlas (`Some` once the first bake batch landed).
    pub fn atlas(&mut self) -> Option<Arc<SceneAtlas>> {
        self.engine.rain_atlas()
    }

    /// Load the current emission into `input`'s rain channels.
    pub fn apply(&mut self, input: &mut RenderInput) {
        input.rain_quads.clone_from(&self.quads);
        input.rain_add.clone_from(&self.add);
        input.rain_atlas = if self.quads.is_empty() {
            None
        } else {
            self.engine.rain_atlas()
        };
    }
}
