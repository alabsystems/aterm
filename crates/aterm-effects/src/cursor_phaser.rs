// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The phaser emitter cursor: under the `phaser` trail style the block cursor is the emitter the beam fires from.

use aterm_time::Instant;

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::{Geom, hsv2rgb};
use crate::effect_util::push_fx_rect as push_rect;
use crate::effect_util::{lerp, lerp_rgb as mix_rgb};

/// The block base the beam hue tints from on a dark theme when the host names
/// none: white.
const BASE_DARK_THEME: u32 = 0x00FF_FFFF;

/// The emitter block on a light page with no pinned cursor colour: the beam's
/// own hue, clean and fully saturated (the wheel stepping over the hues that
/// can only read on paper as khaki or olive,
/// [`crate::color_math::light_spectrum_hue`]), deepening with the charge
/// ([`crate::color_math::light_body_fill`]) and held by lightness alone to
/// the light emitter's contrast on white
/// ([`crate::cursor_beam::LIGHT_EMITTER_CONTRAST`]), so it reads as the
/// emitter rather than a drab near-black.
#[inline]
const fn light_emitter_fill(hue: f32, e: f32) -> u32 {
    let clean = hsv2rgb(crate::color_math::light_spectrum_hue(hue), 1.0, 1.0);
    crate::color_math::floor_contrast_keeping_hue_with_ground_lum(
        crate::color_math::relative_luminance_table(),
        crate::color_math::light_body_fill(clean, e),
        1.0,
        crate::cursor_beam::LIGHT_EMITTER_CONTRAST,
    )
}

/// Charged breath (turns/sec of the wing pulse) and its depth. The phase freezes
/// below [`SETTLED_ENERGY`], so an unrelated present after a long idle never
/// advances a hidden clock and changes brightness.
const PULSE_HZ: f32 = 0.30;
const PULSE_DEPTH: f32 = 0.55;

/// Saturation / value / base-mix ramps from idle whisper to vivid charged fill.
const SAT_IDLE: f32 = 0.45;
const SAT_MAX: f32 = 1.0;
const VAL_IDLE: f32 = 0.85;
const VAL_MAX: f32 = 1.0;
const MIX_IDLE: f32 = 0.22;
const MIX_MAX: f32 = 0.88;

/// Wing reach past the cell edge in cell widths: a tight idle glow, up to over a
/// neighbour cell at full charge.
const REACH_IDLE: f32 = 0.30;
const REACH_MAX: f32 = 1.60;

/// The nested lens layers, innermost last: (reach ×, coverage ×, hue fringe in
/// turns, height ×).
const WING_LAYERS: [(f32, f32, f32, f32); 4] = [
    (1.00, 0.18, 0.10, 0.45),
    (0.74, 0.38, 0.06, 0.62),
    (0.50, 0.68, 0.025, 0.82),
    (0.28, 1.00, 0.0, 1.00),
];

/// Innermost-layer peak coverage (× charge, pre-cap): a dim idle port, a bright
/// charged core.
const COV_IDLE: f32 = 46.0;
const COV_MAX: f32 = 132.0;
const COV_CAP: f32 = 96.0;

/// Stack budget for the strip where all four [`WING_LAYERS`] overlap (hugging the cursor's cell
/// edges, where the newest typed glyphs sit).
const WING_STACK_BUDGET: f32 = 150.0;
/// Σ of the [`WING_LAYERS`] coverage multipliers (the worst-case overlap).
const WING_STACK_OVERLAP: f32 = 2.24;

/// Below this charge the emitter is settled: it reports inactive so the host
/// stops the 60 fps tick (the idle glow rides the blink cadence).
const SETTLED_ENERGY: f32 = 0.02;

/// Resolved per-frame inputs (Copy so the host reads it out before borrowing state).
#[derive(Clone, Copy, Debug)]
pub struct PhaserConfig {
    /// Master on/off (the `phaser` style opted in and the cursor is a focused, visible block).
    pub enabled: bool,
    /// Overall scale `0..1` — the reduced-motion / load-shed amplitude, folded in by the host exactly like the aurora. 0 ⇒ fully inert (plain themed cursor).
    pub intensity: f32,
    /// The block's colour at rest, `0x00RRGGBB`: a PINNED cursor colour (a configured `cursor_color` or a program's OSC 12).
    pub base: Option<u32>,
}

/// What a tick produced: the beam-hued block fill to hand the renderer and a fingerprint that changes on every visible step (0 when off).
#[derive(Clone, Copy, Debug)]
pub struct PhaserFrame {
    /// The beam-hued block-fill colour `0x00RRGGBB`, or `None` when the emitter is off (the renderer then keeps the ordinary themed cursor fill).
    pub fill: Option<u32>,
    /// Fingerprint of the emitted fill + wings (0 ⇒ nothing this frame).
    pub fp: u64,
}

impl PhaserFrame {
    /// Dormant frame output (`fill: None, fp: 0`).
    pub const DORMANT: Self = Self { fill: None, fp: 0 };
}

impl Default for PhaserFrame {
    #[inline]
    fn default() -> Self {
        Self::DORMANT
    }
}

/// Per-window emitter animation state — one breath phase plus the last clock reading and a latched charge.
pub struct CursorPhaser {
    /// Idle-breath phase in turns `0..1`.
    pulse: f32,
    last: Option<Instant>,
    /// Latched charge at the last tick (so [`Self::is_active`] answers without a clock).
    energy: f32,
    /// Cached `((hue_bits, e_bits), fill)` for [`light_emitter_fill`].
    light_fill_memo: Option<((u32, u32), u32)>,
}

impl Default for CursorPhaser {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl CursorPhaser {
    /// Fresh dormant phaser-cursor animation state.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pulse: 0.0,
            last: None,
            energy: 0.0,
            light_fill_memo: None,
        }
    }

    /// Reset animation state while preserving the light-fill memo.
    #[inline]
    pub const fn reset(&mut self) {
        *self = Self {
            light_fill_memo: self.light_fill_memo,
            ..Self::new()
        };
    }

    /// Whether the host must keep arming the animation tick: only while the emitter is charged (typing or cooling).
    #[inline]
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.energy > SETTLED_ENERGY
    }

    /// Advance one frame at `now` with the beam's rolling `hue` (turns `0..1`, read from [`crate::cursor_glow::CursorGlow::beam_hue`] after the aurora tick), typing `energy` (`0..1`), the block cursor cell `cur` (`None`: hidden), theme darkness, grid `geom` and resolved `cfg`.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        hue: f32,
        energy: f32,
        dark_theme: bool,
        geom: Geom,
        cfg: &PhaserConfig,
        out: &mut Vec<GlowQuad>,
    ) -> PhaserFrame {
        let e = (energy.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || cfg.intensity <= 0.0 {
            self.energy = 0.0;
            self.last = Some(now);
            return PhaserFrame::DORMANT;
        }
        let was_active = self.energy > SETTLED_ENERGY;
        self.energy = e;

        let active = e > SETTLED_ENERGY;
        let dt = if was_active && active {
            match self.last {
                Some(t) => now.saturating_duration_since(t).as_secs_f32().min(0.25),
                None => 0.0,
            }
        } else {
            0.0
        };
        self.last = Some(now);
        if active && dt > 0.0 {
            self.pulse = (self.pulse + dt * PULSE_HZ).fract();
        }
        let breath = if self.pulse == 0.0 {
            0.5
        } else {
            0.5 + 0.5 * crate::supernova::sin_cos_f32(self.pulse * std::f32::consts::TAU).0
        };

        let fill = match cfg.base {
            Some(pinned) if e <= SETTLED_ENERGY => pinned & 0x00FF_FFFF,
            Some(pinned) => {
                let beam = hsv2rgb(hue, lerp(SAT_IDLE, SAT_MAX, e), lerp(VAL_IDLE, VAL_MAX, e));
                crate::color_math::wear_pinned(
                    pinned,
                    mix_rgb(pinned, beam, lerp(MIX_IDLE, MIX_MAX, e)),
                    e,
                    SETTLED_ENERGY,
                )
            }
            None if dark_theme => {
                let beam = hsv2rgb(hue, lerp(SAT_IDLE, SAT_MAX, e), lerp(VAL_IDLE, VAL_MAX, e));
                mix_rgb(BASE_DARK_THEME, beam, lerp(MIX_IDLE, MIX_MAX, e))
            }
            None => {
                let key = (hue.to_bits(), e.to_bits());
                if let Some((cached_key, cached_fill)) = self.light_fill_memo
                    && cached_key == key
                {
                    cached_fill
                } else {
                    let solved = light_emitter_fill(hue, e);
                    self.light_fill_memo = Some((key, solved));
                    solved
                }
            }
        };

        const COV_IDLE_OVER_MAX: f32 = COV_IDLE / COV_MAX;
        let charge = COV_IDLE_OVER_MAX * (0.45 + PULSE_DEPTH * breath) + e;
        if dark_theme
            && let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
            && charge > 0.01
        {
            self.emit_wings(cr, cc, e, charge, hue, geom, cfg, out);
        }

        let fp = ((hue * 1024.0) as u64)
            .wrapping_mul(1_000_003)
            .wrapping_add((charge * 255.0) as u64)
            .wrapping_add(((fill as u64) << 12) ^ ((self.pulse * 64.0) as u64))
            | 1;

        PhaserFrame {
            fill: Some(fill),
            fp,
        }
    }

    /// Emit the two lens-shaped wings as premultiplied additive quads. Bounded by construction:
    /// `WING_LAYERS.len()` layers × (cell_h / slab) slabs × 2 sides.
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor cell + charge/hue + geometry/config; one internal call site"
    )]
    #[inline]
    fn emit_wings(
        &self,
        cr: u16,
        cc: u16,
        e: f32,
        charge: f32,
        hue: f32,
        geom: Geom,
        cfg: &PhaserConfig,
        out: &mut Vec<GlowQuad>,
    ) {
        const WING_COV_CEILING: f32 = WING_STACK_BUDGET / WING_STACK_OVERLAP;
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let reach = (REACH_IDLE + (REACH_MAX - REACH_IDLE) * e) * cw;
        let cov_core = (COV_MAX * charge * cfg.intensity).min(WING_COV_CEILING);
        if reach < 1.0 || cov_core < 1.0 {
            return;
        }
        let cell_l = i32::from(geom.origin_x) + cc as i32 * geom.cw as i32;
        let cell_r = cell_l + geom.cw as i32;
        let draw_left = cell_l > geom.fx_left();
        let draw_right = cell_r < geom.fx_right();
        if !draw_left && !draw_right {
            return;
        }
        let cy = f32::from(geom.origin_y) + (cr as f32 + 0.5) * ch;

        let slab = ((ch * 0.125) as i32).max(2);
        let cy_i32 = cy as i32;
        let mut wli = 0;
        while wli < WING_LAYERS.len() {
            let (rx, cov_x, fringe, hx) = WING_LAYERS[wli];
            wli += 1;
            let r = reach * rx;
            if r < 1.0 {
                continue;
            }
            let cov = (cov_core * cov_x).min(COV_CAP) as u8;
            if cov == 0 {
                continue;
            }
            let left_premul = if draw_left || fringe == 0.0 {
                premul_rgb(hsv2rgb(hue - fringe, 0.95, 1.0), cov)
            } else {
                0
            };
            let right_premul = if !draw_right {
                0
            } else if fringe == 0.0 {
                left_premul
            } else {
                premul_rgb(hsv2rgb(hue + fringe, 0.95, 1.0), cov)
            };
            let half_h = (ch * 0.5 * hx).max(1.0);
            let inv_half_h = 1.0 / half_h;
            let half_h_i32 = half_h as i32;
            let mut dy = -half_h_i32;
            while dy < half_h_i32 {
                let h = slab.min(half_h_i32 - dy).max(1);
                let ym = (dy as f32 + h as f32 * 0.5) * inv_half_h;
                let hw = (r * crate::color_math::sqrt_f32((1.0 - ym * ym).max(0.0))) as i32;
                if hw >= 1 {
                    let y = cy_i32 + dy;
                    if draw_left {
                        push_rect(out, geom, cell_l - hw, y, hw, h, left_premul);
                    }
                    if draw_right {
                        push_rect(out, geom, cell_r, y, hw, h, right_premul);
                    }
                }
                dy += h;
            }
        }
    }
}

#[cfg(test)]
#[path = "cursor_phaser_tests.rs"]
mod tests;
