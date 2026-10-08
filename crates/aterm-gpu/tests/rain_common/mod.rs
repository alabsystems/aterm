// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared phosphor rain fixture for the GPU parity/dirty-gate/scissor suites:
//! drives a real `MatrixRain` engine over a frame's empty default-bg cells
//! under a deterministic seed until it emits glyph quads with non-trivial
//! ramp tints plus at least one head halo, so tests exercise the channels
//! the gui feeds (`rain_quads` / `rain_atlas` / `rain_add`).

use std::sync::Arc;

use aterm_effects::matrix_rain::{EffectGeom, MatrixRain, RainConfig, RainTickInput};
use aterm_render::{RainHalo, RenderInput, SceneAtlas, SpriteQuad};

/// Deterministic replay seed (any fixed value; pinned so failures reproduce).
pub const RAIN_SEED: u64 = 0xA7E2_11D3;

/// Calm engine tick period in ms (the 12 Hz weather gate); each
/// [`RainScene::tick`] advances exactly one engine tick.
const CALM_TICK_MS: u64 = 83;

/// [`RainScene::drive_until_raining`] gives up after this many ticks; the
/// seed reaches a rich frame far sooner, so hitting it means the engine broke.
const DRIVE_CAP: usize = 2000;

/// A live rain field over a fixed grid snapshot.
pub struct RainScene {
    pub engine: MatrixRain,
    geom: EffectGeom,
    /// Current emission, row-sorted (`compute_dirty_rows` requires row-sorted
    /// arrival for its per-row merge-diff; see [`RainScene::emit`]).
    pub quads: Vec<SpriteQuad>,
    /// Current bright-head halos (premultiplied additive light).
    pub add: Vec<RainHalo>,
}

impl RainScene {
    /// Build an enabled engine over `base` (a frame snapshot whose empty
    /// default-bg cells are the eligible field) and run the occupancy scan once
    /// (the grid never changes in these tests).
    pub fn new(rows: usize, cols: usize, cell: (usize, usize), base: &RenderInput) -> Self {
        let cfg = RainConfig {
            enabled: true,
            // Max density: the drive reaches a rich, haloed frame quickly.
            density: 12,
            seed: RAIN_SEED,
            default_bg: base.default_bg,
            // Literal mode needs a sampled output table before it emits;
            // classic mode keeps this renderer fixture self-contained.
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

    /// Live inputs every test uses: cursor hidden, no damaged band, no
    /// selection, at the scrollback bottom, main screen.
    fn tick_input() -> RainTickInput<'static> {
        RainTickInput {
            cursor: None,
            hidden_band: &[],
            sel: None,
            display_offset: 0,
            is_alt_screen: false,
        }
    }

    /// One engine tick: a keystroke (so calm never decays to sleep), one calm
    /// period of clock, then emit. Returns the frame fingerprint.
    pub fn tick(&mut self) -> u64 {
        self.engine.note_keystroke();
        self.engine.advance_ms(CALM_TICK_MS);
        self.emit()
    }

    /// Re-emit without advancing the clock: the field is a pure function of
    /// `(seed, tick)`, so this reproduces the same settled emission.
    pub fn emit(&mut self) -> u64 {
        let fp = self.engine.emit(
            self.geom,
            &Self::tick_input(),
            &mut self.quads,
            &mut self.add,
        );
        // `compute_dirty_rows` needs `rain_quads` row-sorted; the engine emits
        // column-major. A stable sort keeps within-row order deterministic.
        self.quads.sort_by_key(|q| q.row);
        fp
    }

    /// Tick until a rich frame: glyph quads with non-trivial ramp tints, at
    /// least one head halo, and the 64-tile atlas fully baked (>= 8 bake
    /// ticks). Panics past [`DRIVE_CAP`] rather than skipping.
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

    /// Load the current emission into `input`'s rain channels. The atlas Arc
    /// is set only alongside non-empty quads, as the gui does.
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
