// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared PHOSPHOR rain fixture for the GPU parity/dirty-gate/scissor suites:
//! drives a REAL `MatrixRain` engine (aterm-effects) over a frame snapshot's
//! empty default-bg cells until it produces a genuine emission — glyph quads
//! carrying non-trivial ramp tints plus at least one bright-head halo — under
//! a deterministic seed. Consuming tests therefore exercise the exact channels
//! the gui host feeds (`rain_quads` / `rain_atlas` / `rain_add`), not
//! hand-built approximations of them.

use std::sync::Arc;

use aterm_effects::matrix_rain::{EffectGeom, MatrixRain, RainConfig, RainTickInput};
use aterm_render::{RainHalo, RenderInput, SceneAtlas, SpriteQuad};

/// Deterministic replay seed (any fixed value; pinned so failures reproduce).
pub const RAIN_SEED: u64 = 0xA7E2_11D3;

/// CALM engine tick period in ms (the engine's 12 Hz weather gate). Advancing
/// exactly one period per [`RainScene::tick`] keeps the drive one engine tick
/// per call, so tick counts in tests mean what they say.
const CALM_TICK_MS: u64 = 83;

/// [`RainScene::drive_until_raining`] gives up after this many ticks — the
/// deterministic seed reaches a tinted, haloed frame far sooner, so hitting
/// the cap means the engine (not the test) broke.
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
    /// Build an enabled engine over `base` (an already-extracted frame
    /// snapshot whose empty default-bg cells are the eligible field) and run
    /// the Tier-A occupancy scan once — the grid never changes in these tests.
    pub fn new(rows: usize, cols: usize, cell: (usize, usize), base: &RenderInput) -> Self {
        let cfg = RainConfig {
            enabled: true,
            // Max density: the drive reaches a rich, haloed frame quickly.
            density: 12,
            seed: RAIN_SEED,
            default_bg: base.default_bg,
            // This fixture validates renderer atlas/quads, not the material
            // sampler. Literal mode requires a sampled output table before it
            // can emit; classic mode keeps the renderer fixture self-contained.
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

    /// The Tier-B live inputs every test uses: cursor hidden (DECTCEM off in
    /// the consuming terminals), no recently-damaged band, no selection, at
    /// the scrollback bottom, main screen — nothing masked.
    fn tick_input() -> RainTickInput<'static> {
        RainTickInput {
            cursor: None,
            hidden_band: &[],
            sel: None,
            display_offset: 0,
            is_alt_screen: false,
        }
    }

    /// One engine tick: a keystroke first (so CALM never decays to SLEEP
    /// mid-test), one CALM period of clock, then emit. Returns the frame
    /// fingerprint.
    pub fn tick(&mut self) -> u64 {
        self.engine.note_keystroke();
        self.engine.advance_ms(CALM_TICK_MS);
        self.emit()
    }

    /// Re-emit WITHOUT advancing the clock: the field is a pure function of
    /// `(seed, tick)`, so the same engine tick reproduces the identical
    /// (settled, fp-stable) emission — the dirty gate's steady state.
    pub fn emit(&mut self) -> u64 {
        let fp = self.engine.emit(
            self.geom,
            &Self::tick_input(),
            &mut self.quads,
            &mut self.add,
        );
        // `compute_dirty_rows`' per-row merge-diff requires `rain_quads` to
        // arrive row-sorted; the engine emits column-major (hash-stride
        // order). Stable sort keeps the deterministic within-row order, so
        // settled re-emissions stay byte-equal frame over frame.
        self.quads.sort_by_key(|q| q.row);
        fp
    }

    /// Tick until a genuinely rich frame: non-empty glyph quads carrying
    /// non-trivial ramp tints AND at least one bright-head halo, with the
    /// 64-tile atlas fully baked (>= 8 progressive bake ticks). Deterministic
    /// under [`RAIN_SEED`]; panics past [`DRIVE_CAP`] rather than skipping.
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

    /// Load the current emission into `input`'s rain channels, honoring the
    /// gui host contract: the atlas Arc rides ONLY alongside non-empty quads,
    /// so a rain-free frame is byte-identical to the pre-feature input.
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
