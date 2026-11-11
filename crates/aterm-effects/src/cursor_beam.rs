// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Light-rod bar and emitter-block cursor effects ([`aterm_render::BEAM_SPACE_HAZE`],
//! [`crate::color_math::light_body_fill`], [`crate::cursor_fx::floor_block_fill`], [`GlowQuad`]).

use aterm_time::Instant;

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::{Geom, OVER_INK_COV_CAP};
use crate::effect_util::TWINKLE_FLASH_BOUND_HZ;
use crate::effect_util::push_fx_rect as push_rect;
use crate::effect_util::{lerp_rgb, push_fx_ramp};

/// Breathing rate in turns/second: a slow, calm shimmer. The phase advances
/// only when frames render, so a settled rod breathes at the blink cadence.
const BREATHE_IDLE: f32 = 0.55;
const BREATHE_ACTIVE: f32 = 1.6;

/// Sparkle's shimmer runs the same sine this many times faster: a glitter
/// tremble rather than a breath.
const SHIMMER_RATE_MUL: f32 = 4.5;
/// The shimmer's ceiling in turns/second. The breath is one unrectified sine,
/// one peak per turn, so turns/second is its flash rate, and it stays inside
/// the crate's general-flash bound however hard the keys charge the rod.
const SHIMMER_MAX_TURNS_PER_S: f32 = TWINKLE_FLASH_BOUND_HZ;
const _: () = assert!(
    BREATHE_IDLE * SHIMMER_RATE_MUL <= SHIMMER_MAX_TURNS_PER_S
        && SHIMMER_MAX_TURNS_PER_S <= TWINKLE_FLASH_BOUND_HZ,
    "the sparkle shimmer must breathe inside the general-flash bound, idle included"
);

/// Rod overshoot past the cell's top/bottom edges, in cell-heights: a resting
/// filament hugs the cell; a charged one extends like a powered rod.
const OVERSHOOT_IDLE: f32 = 0.06;
const OVERSHOOT_MAX: f32 = 0.22;

/// Axis coverage before intensity, rest → full blaze. The sleeve layers scale
/// down from it and every quad is clamped to [`COV_CAP`].
const COV_IDLE: f32 = 70.0;
const COV_MAX: f32 = 150.0;
/// Per-quad additive coverage ceiling (the comet and droplet cursors use the
/// same value): the thin axis saturates here; sleeve quads that reach a
/// neighbour glyph run below it.
const COV_CAP: f32 = 92.0;

/// The blaze below which the rod is settled: the animator reports inactive so
/// the host stops the 60 fps tick and the resting glow rides the blink cadence.
const SETTLED_BLAZE: f32 = 0.02;

/// The least WCAG contrast the emitter block keeps against a light page at rest.
pub const LIGHT_EMITTER_CONTRAST: f32 = crate::cursor_comet::LIGHT_NUCLEUS_CONTRAST;

/// The block aura's brightest light outside the cell at full blaze: the
/// over-ink budget, spent once, at the cell's edge.
const AURA_PEAK_COV: f32 = OVER_INK_COV_CAP;
/// Linear ramp pieces a side of each aura row: the profile to within a few
/// per cent.
const AURA_SEGMENTS: usize = 4;
/// Aura row height is `ch / AURA_ROW_DIV`, at least 2 px, so the falloff
/// toward the row's edges is a few levels a step.
const AURA_ROW_DIV: f32 = 12.0;

/// Resolved per-frame inputs (Copy so the host reads it out before borrowing state).
#[derive(Clone, Copy, Debug)]
pub struct BeamRodConfig {
    /// The trail is on and the cursor is a focused bar.
    pub enabled: bool,
    /// Reduced-motion / load-shed amplitude `0..1`.
    pub intensity: f32,
    /// The rod's hue `0x00RRGGBB` (`GlowConfig::color`, or the style's signature shade).
    pub color: u32,
    /// The sleeve colour `0x00RRGGBB` the outer layers and aura rim mix toward.
    pub haze: u32,
    /// The live cursor is a bar (rod treatment) rather than a block (emitter).
    pub bar: bool,
    /// Sparkle's twinkle: the breathing runs [`SHIMMER_RATE_MUL`] times faster.
    pub shimmer: bool,
    /// The ground is dark (the rod is light) or light.
    pub dark_theme: bool,
    /// The frame's light (`docs/design/PRISM-GLASS.md` §15.2 1.3, `GlowConfig::light`): what a
    /// light arm will read. Off on every dark ground; nothing reads it yet.
    pub light: crate::cursor_glow::Light,
    /// The theme's text and paper, against which a light ground's ink is solved.
    pub theme_fg: u32,
    pub theme_bg: u32,
    /// [`Self::color`] is a pinned cursor colour.
    pub pinned: bool,
    /// [`Self::color`] is one the user chose (a pinned cursor colour, or the trail colour):
    /// paper darkens it in its own hue and saturation, never saturates it
    /// ([`crate::color_math::light_caret_fill`]).
    pub chosen: bool,
}

/// What a tick produced.
#[derive(Clone, Copy, Debug)]
pub struct BeamRodFrame {
    /// The emitter block-fill colour `0x00RRGGBB`, or `None` when off / bar.
    pub fill: Option<u32>,
    /// Fingerprint of the emitted fill + rod light (0 ⇒ nothing this frame).
    pub fp: u64,
}

impl BeamRodFrame {
    /// Dormant frame output (`fill: None, fp: 0`).
    pub const DORMANT: Self = Self { fill: None, fp: 0 };
}

impl Default for BeamRodFrame {
    #[inline]
    fn default() -> Self {
        Self::DORMANT
    }
}

/// Per-window light-rod animation state.
pub struct CursorBeamRod {
    /// Rolling breathing phase in turns `0..1`.
    breathe: f32,
    last: Option<Instant>,
    /// Latched blaze at the last tick (so [`Self::is_active`] needs no clock).
    blaze: f32,
    /// Cached `((color, theme_fg, theme_bg), ink)` for [`Self::emit_rod_light`].
    light_ink_memo: Option<((u32, u32, u32), u32)>,
    /// Cached `((color, theme_bg, e_bits), fill)` for the light-theme emitter block.
    light_fill_memo: Option<((u32, u32, u32, bool), u32)>,
}

impl Default for CursorBeamRod {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl CursorBeamRod {
    /// Fresh dormant light-rod animation state.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            breathe: 0.0,
            last: None,
            blaze: 0.0,
            light_ink_memo: None,
            light_fill_memo: None,
        }
    }

    /// Reset animation state while preserving the light-theme memos.
    #[inline]
    pub const fn reset(&mut self) {
        *self = Self {
            light_ink_memo: self.light_ink_memo,
            light_fill_memo: self.light_fill_memo,
            ..Self::new()
        };
    }

    /// Whether the host must keep arming the animation tick.
    #[must_use]
    #[inline]
    pub const fn is_active(&self) -> bool {
        self.blaze > SETTLED_BLAZE
    }

    /// Advance one frame using [`crate::cursor_glow::CursorGlow::blaze`].
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor cell + clock + blaze + geometry + config + the two streams it draws into"
    )]
    #[inline]
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        blaze: f32,
        geom: Geom,
        cfg: &BeamRodConfig,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> BeamRodFrame {
        let e = (blaze.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || cfg.intensity <= 0.0 {
            self.blaze = 0.0; // inert: report settled so the host disarms the tick
            self.last = Some(now);
            return BeamRodFrame::DORMANT;
        }
        self.blaze = e;

        let dt = match self.last {
            Some(t) => now.saturating_duration_since(t).as_secs_f32().min(0.25),
            None => 0.0,
        };
        self.last = Some(now);
        if dt > 0.0 {
            let rate = BREATHE_IDLE + BREATHE_ACTIVE * e;
            let rate = if cfg.shimmer {
                (rate * SHIMMER_RATE_MUL).min(SHIMMER_MAX_TURNS_PER_S)
            } else {
                rate
            };
            self.breathe = (self.breathe + dt * rate).fract();
        }

        let fill = if cfg.bar {
            None
        } else {
            let styled = if cfg.dark_theme {
                lerp_rgb(cfg.color, 0x00FF_FFFF, 0.10 + 0.35 * e)
            } else {
                let bg = cfg.theme_bg & 0x00FF_FFFF;
                let key = (cfg.color, bg, e.to_bits(), cfg.chosen);
                if let Some((cached_key, cached_fill)) = self.light_fill_memo
                    && cached_key == key
                {
                    cached_fill
                } else {
                    let solved = crate::color_math::floor_contrast_keeping_hue(
                        crate::color_math::light_caret_fill(cfg.color, e, cfg.chosen),
                        bg,
                        LIGHT_EMITTER_CONTRAST,
                    );
                    self.light_fill_memo = Some((key, solved));
                    solved
                }
            };
            Some(if cfg.pinned {
                crate::color_math::wear_pinned(cfg.color, styled, e, SETTLED_BLAZE)
            } else {
                styled
            })
        };

        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
            && (cfg.dark_theme || cfg.bar)
        {
            let phase = self.breathe * std::f32::consts::TAU;
            let breathe = 0.5 + 0.5 * crate::supernova::sin_cos_f32(phase).0;
            match (cfg.dark_theme, cfg.bar) {
                (true, true) => self.emit_rod(cr, cc, e, breathe, geom, cfg, out),
                (true, false) => self.emit_aura(cr, cc, e, breathe, geom, cfg, out),
                (false, true) => self.emit_rod_light(cr, cc, e, breathe, geom, cfg, under),
                (false, false) => {}
            }
        }

        let fp = ((self.breathe * 512.0) as u64)
            .wrapping_mul(1_000_003)
            .wrapping_add((e * 255.0) as u64)
            .wrapping_add((fill.unwrap_or(cfg.color) as u64) << 12)
            | 1; // never 0 while enabled — the resting glow is always drawable

        BeamRodFrame { fill, fp }
    }

    /// The bar treatment: a vertical rod of light on the bar's own x.
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor cell + blaze/breathe + geometry/config; one internal call site"
    )]
    #[inline]
    fn emit_rod(
        &self,
        cr: u16,
        cc: u16,
        e: f32,
        breathe: f32,
        geom: Geom,
        cfg: &BeamRodConfig,
        out: &mut Vec<GlowQuad>,
    ) {
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let (ox, oy) = (geom.origin_x as f32, geom.origin_y as f32);
        let ax = ox + (cc as f32) * cw + cw * 0.10;
        let over =
            (OVERSHOOT_IDLE + (OVERSHOOT_MAX - OVERSHOOT_IDLE) * e) * ch * (0.85 + 0.15 * breathe);
        let y0 = oy + (cr as f32) * ch - over;
        let h = ch + 2.0 * over;
        let cov_axis =
            (COV_IDLE + (COV_MAX - COV_IDLE) * e) * cfg.intensity * (0.90 + 0.10 * breathe);
        if cov_axis < 1.0 {
            return;
        }
        const WHITE: u32 = 0x00FF_FFFF;
        let layers: [(f32, f32, u32, f32); 4] = [
            (0.045, 1.00, WHITE, 0.55),   // white-hot axis
            (0.13, 0.55, WHITE, 0.12),    // inner sleeve
            (0.30, 0.24, cfg.haze, 0.55), // haze glow (beam: nebula; others: deep self-shade)
            (0.55, 0.10, cfg.haze, 0.85), // outer wash (a tint at the neighbour)
        ];
        let (y0_i, h_i) = (y0 as i32, h as i32);
        let (cell_y, ch_i) = (oy as i32 + (cr as i32) * ch as i32, ch as i32);
        let mut i = 0;
        while i < layers.len() {
            let (hw, cmul, target, mix) = layers[i];
            let is_core = i < 2;
            i += 1;
            let cov = (cov_axis * cmul).min(COV_CAP) as u8;
            if cov == 0 {
                break;
            }
            let w = ((cw * hw * 2.0) as i32).max(1);
            let x = (ax - cw * hw) as i32;
            let color = lerp_rgb(cfg.color, target, mix);
            let premul = premul_rgb(color, cov);
            if is_core {
                // Full rod including the overshoot caps.
                push_rect(out, geom, x, y0_i, w, h_i, premul);
            } else {
                // Sleeve layers stay inside the cell band.
                push_rect(out, geom, x, cell_y, w, ch_i, premul);
            }
        }
    }

    /// The bar on a light ground.
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor cell + blaze/breathe + geometry/config; one internal call site"
    )]
    #[inline]
    fn emit_rod_light(
        &mut self,
        cr: u16,
        cc: u16,
        e: f32,
        breathe: f32,
        geom: Geom,
        cfg: &BeamRodConfig,
        under: &mut Vec<GlowQuad>,
    ) {
        use crate::cursor_glow::{LIGHT_BODY_ALPHA_CAP, LightGround};
        const INV_COV_MAX: f32 = 1.0 / COV_MAX;
        let cw = geom.cw as f32;
        let ax = f32::from(geom.origin_x) + (cc as f32) * cw + cw * 0.10;
        let y = i32::from(geom.origin_y) + cr as i32 * geom.ch as i32;
        let strength = (COV_IDLE + (COV_MAX - COV_IDLE) * e)
            * INV_COV_MAX
            * cfg.intensity.clamp(0.0, 1.0)
            * (0.90 + 0.10 * breathe);
        if LIGHT_BODY_ALPHA_CAP * strength < 1.0 {
            return;
        }
        let key = (
            (cfg.color & 0x00FF_FFFF) | (u32::from(cfg.chosen) << 24),
            cfg.theme_fg & 0x00FF_FFFF,
            cfg.theme_bg & 0x00FF_FFFF,
        );
        let ink = match self.light_ink_memo {
            Some((k, ink)) if k == key => ink,
            _ => {
                // A colour the user chose keeps its own chroma ([`LightGround::chosen`]).
                let ground = LightGround::on(cfg.theme_fg, cfg.theme_bg);
                let ink = if cfg.chosen { ground.chosen() } else { ground }.body_ink(cfg.color);
                self.light_ink_memo = Some((key, ink));
                ink
            }
        };
        let ch_i = geom.ch as i32;
        const BANDS: [(f32, f32); 2] = [(0.30, 0.45), (0.11, 1.0)];
        let mut bi = 0;
        while bi < BANDS.len() {
            let (hw, share) = BANDS[bi];
            bi += 1;
            let a = (LIGHT_BODY_ALPHA_CAP * share * strength) as u8;
            if a == 0 {
                continue;
            }
            let w = ((cw * hw * 2.0) as i32).max(1);
            crate::effect_util::push_fx_quad(
                under,
                geom,
                (ax - cw * hw) as i32,
                y,
                w,
                ch_i,
                premul_rgb(ink, a),
                a,
            );
        }
    }

    /// The block treatment: a soft round aura beside the emitter cell, inside the caret's row.
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor cell + blaze/breathe + geometry/config; one internal call site"
    )]
    #[inline]
    fn emit_aura(
        &self,
        cr: u16,
        cc: u16,
        e: f32,
        breathe: f32,
        geom: Geom,
        cfg: &BeamRodConfig,
        out: &mut Vec<GlowQuad>,
    ) {
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let (cx, cy) = geom.cell_center(cr, cc);
        let reach = (0.28 + 0.30 * e) * (0.85 + 0.15 * breathe) * ch;
        let (rx, ry) = (cw * 0.5 + reach, ch * 0.5);
        let rho_edge = (cw * 0.5) / rx;
        let f_edge_1 = 1.0 - rho_edge * rho_edge;
        let f_edge = f_edge_1 * f_edge_1;
        let peak = AURA_PEAK_COV * (0.55 + 0.45 * e) * (0.9 + 0.1 * breathe) * cfg.intensity;
        if !(reach >= 1.0 && f_edge > 0.0 && peak >= 0.5) {
            return;
        }
        let inv_f_edge = 1.0 / f_edge;
        let inv_rx = 1.0 / rx;
        let inv_ry = 1.0 / ry;
        let hw = cw * 0.5;
        let cell_x0 = (cx - hw).round() as i32;
        let cell_x1 = cell_x0 + geom.cw as i32;
        let ch_i = geom.ch as i32;
        const INV_AURA_ROW_DIV: f32 = 1.0 / AURA_ROW_DIV;
        let band_y0 = i32::from(geom.origin_y) + i32::from(cr) * ch_i;
        let band_y1 = band_y0 + ch_i;
        let row_h = ((ch * INV_AURA_ROW_DIV) as i32).max(2);
        let rgb_knot = (cfg.color, cfg.haze);
        let mut y = band_y0;
        while y < band_y1 {
            let h = row_h.min(band_y1 - y);
            let v = (y as f32 + h as f32 * 0.5 - cy) * inv_ry;
            let v2 = v * v;
            let w2 = 1.0 - v2;
            if w2 > 0.0 {
                let half = rx * crate::color_math::sqrt_f32(w2);
                if half > hw {
                    let span = half - hw;
                    let geom_knot = (cx, hw, span);
                    let falloff_knot = (inv_rx, v2, inv_f_edge, peak);
                    let mut prev = aura_knot(0, geom_knot, falloff_knot, rgb_knot);
                    let mut k = 1;
                    while k <= AURA_SEGMENTS {
                        let next = aura_knot(k, geom_knot, falloff_knot, rgb_knot);
                        k += 1;
                        let ((r_in, l_in, c_in, inner), (r_out, l_out, c_out, outer)) =
                            (prev, next);
                        prev = next;
                        if c_in == 0 && c_out == 0 {
                            break;
                        }
                        let (r0, r1) = (r_in.max(cell_x1), r_out);
                        push_fx_ramp(out, geom, r0, y, r1 - r0, h, (inner, 0), (outer, 0));
                        let (l0, l1) = (l_out, l_in.min(cell_x0));
                        push_fx_ramp(out, geom, l0, y, l1 - l0, h, (outer, 0), (inner, 0));
                    }
                }
            }
            y += h;
        }
    }
}

#[inline(always)]
const fn aura_knot(
    k: usize,
    (cx, hw, span): (f32, f32, f32),
    (inv_rx, v2, inv_f_edge, peak): (f32, f32, f32, f32),
    (color, haze): (u32, u32),
) -> (i32, i32, u8, u32) {
    const INV_AURA_SEGMENTS: f32 = 1.0 / AURA_SEGMENTS as f32;
    let t = k as f32 * INV_AURA_SEGMENTS;
    let dx = hw + span * t;
    let (rx_i, lx_i) = ((cx + dx).round() as i32, (cx - dx).round() as i32);
    if k == AURA_SEGMENTS {
        return (rx_i, lx_i, 0, 0);
    }
    let u = dx * inv_rx;
    let rho2 = u * u + v2;
    let f_1 = (1.0 - rho2).max(0.0);
    let f = f_1 * f_1 * inv_f_edge;
    if peak * f < 0.5 {
        return (rx_i, lx_i, 0, 0);
    }
    let cov = (peak * f).round().min(OVER_INK_COV_CAP) as u8;
    let rgb = lerp_rgb(color, haze, 0.75 * crate::color_math::sqrt_f32(rho2));
    (rx_i, lx_i, cov, premul_rgb(rgb, cov))
}

#[cfg(test)]
#[path = "cursor_beam_tests.rs"]
mod tests;
