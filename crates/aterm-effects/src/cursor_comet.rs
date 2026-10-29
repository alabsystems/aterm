// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The comet cursor (`comet` trail style).

use aterm_time::Instant;

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::{Geom, OVER_INK_COV_CAP};
use crate::effect_util::lerp_rgb;

use crate::effect_util::push_fx_rect as push_rect;

const SHIMMER_IDLE: f32 = 0.7;
const SHIMMER_ACTIVE: f32 = 3.2;

const RADIUS_IDLE: f32 = 0.54;
const RADIUS_MAX: f32 = 0.82;

const COV_IDLE: f32 = 58.0;
const COV_MAX: f32 = 148.0;
const COV_CAP: f32 = 92.0;

const COMA_DISCS: [(f32, f32, f32, f32); 4] = [
    (1.00, 0.30, 0.18, 0.06),
    (0.80, 0.58, 0.44, 0.12),
    (0.60, 0.85, 0.66, 0.18),
    (0.38, 1.00, 0.86, 0.14),
];

#[inline]
const fn cov_capped(cov: f32) -> f32 {
    if cov < COV_CAP { cov } else { COV_CAP }
}

const COMA_STACK_MAX: f32 = cov_capped(COV_MAX * COMA_DISCS[0].1)
    + cov_capped(COV_MAX * COMA_DISCS[1].1)
    + cov_capped(COV_MAX * COMA_DISCS[2].1)
    + cov_capped(COV_MAX * COMA_DISCS[3].1);

const GLINT_SHARE: f32 = 0.4;
const RING_BUDGET: f32 = OVER_INK_COV_CAP * (1.0 - GLINT_SHARE);
const GLINT_BUDGET: f32 = OVER_INK_COV_CAP * GLINT_SHARE;

const COMA_OUTSIDE_SCALE: f32 = RING_BUDGET / COMA_STACK_MAX;
const _: () = assert!(
    COMA_OUTSIDE_SCALE < 1.0 && RING_BUDGET + GLINT_BUDGET <= OVER_INK_COV_CAP,
    "the coma outside the caret must fit the over-ink budget"
);

const GLINTS: u32 = 4;

const SETTLED_BLAZE: f32 = 0.02;

/// The near-white the ice freezes toward.
pub(crate) const ICE_WHITE: u32 = 0x00F2_FAFF;

/// Resolved per-frame inputs (Copy so the host reads it out before borrowing state).
#[derive(Clone, Copy, Debug)]
pub struct CometConfig {
    /// On when the `comet` style is chosen and the cursor is a focused, visible block.
    pub enabled: bool,
    /// Overall scale `0..1`.
    pub intensity: f32,
    /// The trail's base hue (`GlowConfig::color`); the coma freezes off it.
    pub color: u32,
    /// The trail's accent hue (`GlowConfig::accent`): the coma's outer fringe.
    pub accent: u32,
    /// The ground is dark (the coma is light) or light (the coma is ink under the letters).
    pub dark_theme: bool,
    /// The frame's light (`docs/design/PRISM-GLASS.md` §15.2 1.3, `GlowConfig::light`): what a
    /// light arm will read. Off on every dark ground; nothing reads it yet.
    pub light: crate::cursor_glow::Light,
    /// The theme's text and paper.
    pub theme_fg: u32,
    pub theme_bg: u32,
    /// [`Self::color`] is a PINNED cursor colour.
    pub pinned: bool,
}

/// The least WCAG contrast the comet's nucleus keeps against a LIGHT page at rest.
pub const LIGHT_NUCLEUS_CONTRAST: f32 = 6.0;

/// What a tick produced.
#[derive(Clone, Copy, Debug)]
pub struct CometFrame {
    /// The frosted block-fill colour `0x00RRGGBB`, or `None` when the nucleus is off.
    pub fill: Option<u32>,
    /// Fingerprint of the emitted fill + coma + glints (0 ⇒ nothing this frame).
    pub fp: u64,
}

impl CometFrame {
    /// Dormant frame output (`fill: None, fp: 0`).
    pub const DORMANT: Self = Self { fill: None, fp: 0 };
}

impl Default for CometFrame {
    #[inline]
    fn default() -> Self {
        Self::DORMANT
    }
}

/// Per-window nucleus state: shimmer phase, last clock reading, latched blaze.
pub struct CursorComet {
    /// Rolling shimmer phase in turns `0..1`.
    shimmer: f32,
    last: Option<Instant>,
    /// Latched blaze at the last tick, so [`Self::is_active`] needs no clock.
    blaze: f32,
    /// Cached `(color, theme_fg, theme_bg)` ink for the light-page coma.
    light_ink_memo: Option<((u32, u32, u32), u32)>,
    /// Cached `(color, theme_bg, e.to_bits())` fill for the light-page nucleus.
    light_fill_memo: Option<((u32, u32, u32), u32)>,
    /// Test-only: the coma's rim twinkles four `+` glints, as it did until
    /// looks10 L9 (its negative control).
    #[cfg(test)]
    rim_glints: bool,
}

impl Default for CursorComet {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl CursorComet {
    /// Fresh dormant comet-cursor animation state.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            shimmer: 0.0,
            last: None,
            blaze: 0.0,
            light_ink_memo: None,
            light_fill_memo: None,
            #[cfg(test)]
            rim_glints: false,
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

    /// Whether the host must keep arming the animation tick: only while the comet is charged.
    #[must_use]
    #[inline]
    pub const fn is_active(&self) -> bool {
        self.blaze > SETTLED_BLAZE
    }

    /// Advance one frame at `now` with aurora `blaze`, block cursor cell `cur`, `geom`, and `cfg`.
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
        cfg: &CometConfig,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> CometFrame {
        let e = (blaze.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || cfg.intensity <= 0.0 {
            self.blaze = 0.0;
            self.last = Some(now);
            return CometFrame::DORMANT;
        }
        self.blaze = e;

        let dt = match self.last {
            Some(t) => now.saturating_duration_since(t).as_secs_f32().min(0.25),
            None => 0.0,
        };
        self.last = Some(now);
        self.shimmer = (self.shimmer + dt * (SHIMMER_IDLE + SHIMMER_ACTIVE * e)).fract();
        let phase = self.shimmer * std::f32::consts::TAU;
        let breathe = 0.5
            + 0.35 * crate::supernova::sin_cos_f32(phase).0
            + 0.15 * crate::supernova::sin_cos_f32(phase * 2.3 + 0.9).0;

        let mut dark_stops = (0u32, 0u32);
        let styled = if cfg.dark_theme {
            let dimmed = lerp_rgb(cfg.color, 0x0000_0000, 0.50);
            let whitened = lerp_rgb(cfg.color, ICE_WHITE, 0.55);
            dark_stops = (dimmed, whitened);
            ice_ramp_with(
                cfg.color,
                dimmed,
                whitened,
                0.34 + 0.40 * e + 0.05 * (breathe - 0.5),
            )
        } else {
            let key = (
                cfg.color & 0x00FF_FFFF,
                cfg.theme_bg & 0x00FF_FFFF,
                e.to_bits(),
            );
            if let Some((prev_key, fill)) = self.light_fill_memo
                && prev_key == key
            {
                fill
            } else {
                let fill = crate::color_math::floor_contrast_keeping_hue(
                    crate::color_math::light_body_fill(cfg.color, e),
                    cfg.theme_bg & 0x00FF_FFFF,
                    LIGHT_NUCLEUS_CONTRAST,
                );
                self.light_fill_memo = Some((key, fill));
                fill
            }
        };
        let fill = if cfg.pinned {
            crate::color_math::wear_pinned(cfg.color, styled, e, SETTLED_BLAZE)
        } else {
            styled
        };

        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
        {
            if cfg.dark_theme {
                self.emit_coma(cr, cc, e, breathe, phase, dark_stops, geom, cfg, out);
            } else {
                self.emit_coma_light(cr, cc, e, breathe, geom, cfg, under);
            }
        }

        let fp = ((self.shimmer * 512.0) as u64)
            .wrapping_mul(1_000_003)
            .wrapping_add((e * 255.0) as u64)
            .wrapping_add((fill as u64) << 12)
            | 1;

        CometFrame {
            fill: Some(fill),
            fp,
        }
    }

    /// The coma on a LIGHT GROUND: a round head of ink under the letters,
    /// centred on the caret and held inside its row, a cell either side, its
    /// ink solved against the theme's text; it breathes with the shimmer as
    /// the dark coma does.
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor cell + blaze/breathe + geometry/config; one internal call site"
    )]
    #[inline]
    fn emit_coma_light(
        &mut self,
        cr: u16,
        cc: u16,
        e: f32,
        breathe: f32,
        geom: Geom,
        cfg: &CometConfig,
        under: &mut Vec<GlowQuad>,
    ) {
        use crate::cursor_glow::{LIGHT_BODY_ALPHA_CAP, light_body_ink, push_ink_ellipse};
        let peak = LIGHT_BODY_ALPHA_CAP
            * (0.35 + 0.65 * e)
            * (1.0 - e)
            * (0.86 + 0.14 * breathe)
            * cfg.intensity.clamp(0.0, 1.0);
        if peak < 1.0 {
            return;
        }
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let centre = geom.cell_center(cr, cc);
        let key = (
            cfg.color & 0x00FF_FFFF,
            cfg.theme_fg & 0x00FF_FFFF,
            cfg.theme_bg & 0x00FF_FFFF,
        );
        let ink = match self.light_ink_memo {
            Some((k, ink)) if k == key => ink,
            _ => {
                let ink = light_body_ink(
                    cfg.color,
                    cfg.theme_fg,
                    cfg.theme_bg,
                    LIGHT_BODY_ALPHA_CAP as u8,
                );
                self.light_ink_memo = Some((key, ink));
                ink
            }
        };
        let budget = under
            .len()
            .saturating_add(crate::cursor_glow::LIGHT_MARK_QUADS);
        let rx = cw * (0.9 + 0.3 * e);
        push_ink_ellipse(under, geom, centre, (rx, rx), ch * 0.46, ink, peak, budget);
    }

    /// Emit the coma as premultiplied additive quads (dark page). Its rim
    /// twinkled four `+` glints, a grey cross beside the caret in every
    /// frame of the trail gallery's dark Comet (`split.b0`, `cjk.t0`), a
    /// star the Sparkle style owns: the coma is round light alone (looks10
    /// L9).
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor cell + blaze/breathe/phase + geometry/config; one internal call site"
    )]
    #[inline]
    fn emit_coma(
        &self,
        cr: u16,
        cc: u16,
        e: f32,
        breathe: f32,
        phase: f32,
        (dimmed, whitened): (u32, u32),
        geom: Geom,
        cfg: &CometConfig,
        out: &mut Vec<GlowQuad>,
    ) {
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let cx = f32::from(geom.origin_x) + (cc as f32 + 0.5) * cw;
        let cy = f32::from(geom.origin_y) + (cr as f32 + 0.5) * ch;
        let cell_x0 = i32::from(geom.origin_x) + i32::from(cc) * geom.cw as i32;
        let cell_y0 = i32::from(geom.origin_y) + i32::from(cr) * geom.ch as i32;
        let (cell_x1, cell_y1) = (cell_x0 + geom.cw as i32, cell_y0 + geom.ch as i32);
        let radius = (RADIUS_IDLE + (RADIUS_MAX - RADIUS_IDLE) * e) * ch * (0.96 + 0.06 * breathe);
        let cov_core =
            (COV_IDLE + (COV_MAX - COV_IDLE) * e) * cfg.intensity * (0.86 + 0.14 * breathe);

        let mut discs = [(0.0f32, 0.0f32, 0.0f32, 0u32); 4];
        let mut di = 0;
        while di < 4 {
            let (rx, cov_x, t0, dt) = COMA_DISCS[di];
            let ramp = ice_ramp_with(cfg.color, dimmed, whitened, t0 + dt * e);
            let color = if rx >= 1.0 {
                lerp_rgb(cfg.accent, ramp, 0.45)
            } else {
                ramp
            };
            let r = radius * rx;
            discs[di] = (r, r * r, (cov_core * cov_x).min(COV_CAP), color);
            di += 1;
        }
        let slab = ((ch * 0.125) as i32).max(2);
        let wob_freq = 0.55 / ch.max(1.0) * 10.0;
        let wob_phase = phase * 1.3;

        let (cxi, cyi) = (cx as i32, cy as i32);
        let mut di = 0;
        while di < discs.len() {
            let (r, r2, cov, color) = discs[di];
            di += 1;
            let cov = cov as u8;
            if r < 1.0 || cov == 0 {
                continue;
            }
            let premul = premul_rgb(color, cov);
            let r_i = r as i32;
            let mut dy = -r_i;
            while dy < r_i {
                let cy_dy = cyi + dy;
                if cy_dy >= cell_y1 {
                    break;
                }
                let h = slab.min(r_i - dy);
                let (y0, y1) = (cy_dy.max(cell_y0), (cy_dy + h).min(cell_y1));
                if y1 > y0 {
                    let ym = dy as f32 + h as f32 * 0.5;
                    let hw = (crate::color_math::sqrt_f32((r2 - ym * ym).max(0.0))
                        * coma_wobble(ym, wob_freq, wob_phase)) as i32;
                    let (x0, x1) = ((cxi - hw).max(cell_x0), (cxi + hw).min(cell_x1));
                    if hw >= 1 && x1 > x0 {
                        push_rect(out, geom, x0, y0, x1 - x0, y1 - y0, premul);
                    }
                }
                dy += h;
            }
        }

        let outer = discs[0].0;
        if outer >= 1.0 {
            let mut ring_premul = [0u32; 4];
            let (mut sum, mut acc) = (0.0f32, [0.0f32; 3]);
            let mut i = 0;
            while i < discs.len() {
                let (_, _, cov, color) = discs[i];
                sum += cov;
                acc[0] += ((color >> 16) & 0xff) as f32 * cov;
                acc[1] += ((color >> 8) & 0xff) as f32 * cov;
                acc[2] += (color & 0xff) as f32 * cov;
                if sum > 0.0 {
                    let cov = (sum * COMA_OUTSIDE_SCALE) as u8;
                    if cov != 0 {
                        let inv_sum = 1.0 / sum;
                        let r_avg = ((acc[0] * inv_sum).round() as u32).min(255);
                        let g_avg = ((acc[1] * inv_sum).round() as u32).min(255);
                        let b_avg = ((acc[2] * inv_sum).round() as u32).min(255);
                        ring_premul[i] = premul_rgb((r_avg << 16) | (g_avg << 8) | b_avg, cov);
                    }
                }
                i += 1;
            }
            if ring_premul[3] != 0 {
                let top = (cy - outer).floor() as i32;
                let bot = (cy + outer).ceil() as i32;
                let mut y = cell_y0 - (cell_y0 - top).div_euclid(slab) * slab - slab;
                while y < bot {
                    let mut h = slab;
                    if y < cell_y1 && y + h > cell_y1 {
                        h = cell_y1 - y;
                    }
                    if y + h > top {
                        let in_band = y >= cell_y0 && y < cell_y1;
                        ring_row(
                            &discs,
                            &ring_premul,
                            (cx, cy),
                            (y, h),
                            (wob_freq, wob_phase),
                            in_band,
                            (cell_x0, cell_x1),
                            geom,
                            out,
                        );
                    }
                    y += h;
                }
            }
        }

        #[cfg(test)]
        let rim_glints = self.rim_glints;
        #[cfg(not(test))]
        let rim_glints = false;
        if !rim_glints {
            return;
        }
        const GLINT_TW_PHASES: [f32; GLINTS as usize] = [0.0, 2.4, 4.8, 7.2];
        const INV_GLINT_TW_SPAN: f32 = 1.0 / 0.45;
        let glint_r = radius * 0.72;
        let arm = ((ch * 0.11) as i32).max(1);
        let two_arm_plus_1 = 2 * arm + 1;
        let cov_base = cov_core * (0.35 + 0.65 * e);
        let cov_scale = cov_base * INV_GLINT_TW_SPAN;
        let phase2 = phase * 2.0;
        let (sin_p, cos_p) = crate::supernova::sin_cos_f32(phase * 0.55);
        let glint_dirs: [(f32, f32); GLINTS as usize] = [
            (sin_p, cos_p),
            (cos_p, -sin_p),
            (-sin_p, -cos_p),
            (-cos_p, sin_p),
        ];
        let mut gi = 0;
        while gi < GLINTS as usize {
            let cur_gi = gi;
            gi += 1;
            let tw_off = GLINT_TW_PHASES[cur_gi];
            let tw = 0.5 + 0.5 * crate::supernova::sin_cos_f32(phase2 + tw_off).0;
            if tw < 0.55 {
                continue;
            }
            let cov = ((tw - 0.55) * cov_scale).min(GLINT_BUDGET) as u8;
            if cov == 0 {
                continue;
            }
            let (sin_a, cos_a) = glint_dirs[cur_gi];
            let gx = (cx + cos_a * glint_r) as i32;
            let gy = (cy + sin_a * glint_r * 0.9) as i32;
            let spark = premul_rgb(ICE_WHITE, cov);
            push_rect(out, geom, gx - arm, gy, two_arm_plus_1, 1, spark);
            push_rect(out, geom, gx, gy - arm, 1, arm, spark);
            push_rect(out, geom, gx, gy + 1, 1, arm, spark);
        }
    }
}

#[inline]
const fn coma_wobble(ym: f32, wob_freq: f32, wob_phase: f32) -> f32 {
    1.0 + 0.05 * crate::supernova::sin_cos_f32(ym * wob_freq + wob_phase).0
}

/// One slab row `[y, y + h)` of the outside rings: band `i` spans
/// between disc `i`'s half-width and disc `i + 1`'s and carries the
/// coverage-weighted colour of discs `0..=i` at their summed coverage ×
/// [`COMA_OUTSIDE_SCALE`].
#[allow(
    clippy::too_many_arguments,
    reason = "the slab row, the shared disc table and the cell cut; one call site"
)]
#[inline]
fn ring_row(
    discs: &[(f32, f32, f32, u32); 4],
    ring_premul: &[u32; 4],
    (cx, cy): (f32, f32),
    (y, h): (i32, i32),
    (wob_freq, wob_phase): (f32, f32),
    in_band: bool,
    (cell_x0, cell_x1): (i32, i32),
    geom: Geom,
    out: &mut Vec<GlowQuad>,
) {
    let ym = y as f32 + h as f32 * 0.5 - cy;
    let ym2 = ym * ym;
    let d2_0 = discs[0].1 - ym2;
    if d2_0 <= 0.0 {
        return;
    }
    let wob = coma_wobble(ym, wob_freq, wob_phase);
    let hw0 = crate::color_math::sqrt_f32(d2_0) * wob;
    if hw0 < 1.0 {
        return;
    }
    let mut hw = [hw0, 0.0, 0.0, 0.0];
    let cx_round = cx.round() as i32;
    let mut x_edges = [(cx_round, cx_round); 4];
    x_edges[0] = ((cx - hw0).round() as i32, (cx + hw0).round() as i32);
    let d2_1 = discs[1].1 - ym2;
    if d2_1 > 0.0 {
        let w1 = crate::color_math::sqrt_f32(d2_1) * wob;
        hw[1] = w1;
        x_edges[1] = ((cx - w1).round() as i32, (cx + w1).round() as i32);
        let d2_2 = discs[2].1 - ym2;
        if d2_2 > 0.0 {
            let w2 = crate::color_math::sqrt_f32(d2_2) * wob;
            hw[2] = w2;
            x_edges[2] = ((cx - w2).round() as i32, (cx + w2).round() as i32);
            let d2_3 = discs[3].1 - ym2;
            if d2_3 > 0.0 {
                let w3 = crate::color_math::sqrt_f32(d2_3) * wob;
                hw[3] = w3;
                x_edges[3] = ((cx - w3).round() as i32, (cx + w3).round() as i32);
            }
        }
    }
    let mut i = 0;
    while i < discs.len() {
        let premul = ring_premul[i];
        if hw[i] >= 1.0 && premul != 0 {
            let (xo0, xo1) = x_edges[i];
            let (xi0, xi1) = if i + 1 < 4 {
                x_edges[i + 1]
            } else {
                (cx_round, cx_round)
            };
            let spans: [(i32, i32); 2] = if xi1 > xi0 {
                [(xo0, xi0), (xi1, xo1)]
            } else {
                [(xo0, xo1), (0, 0)]
            };
            let mut si = 0;
            while si < 2 {
                let (a, b) = spans[si];
                si += 1;
                let pieces: [(i32, i32); 2] = if in_band {
                    [(a, b.min(cell_x0)), (a.max(cell_x1), b)]
                } else {
                    [(a, b), (0, 0)]
                };
                let mut pi = 0;
                while pi < 2 {
                    let (x0, x1) = pieces[pi];
                    pi += 1;
                    if x1 > x0 {
                        push_rect(out, geom, x0, y, x1 - x0, h, premul);
                    }
                }
            }
        }
        i += 1;
    }
}

/// The ice ramp off the trail's base hue, `t` 0 (cold, dim) to 1 (sunlit,
/// near-white): dimmed hue, pure hue, whitened hue, [`ICE_WHITE`].
#[inline]
const fn ice_ramp_with(color: u32, dimmed: u32, whitened: u32, t: f32) -> u32 {
    const INV_045: f32 = 1.0 / 0.45;
    const INV_033: f32 = 1.0 / 0.33;
    const INV_022: f32 = 1.0 / 0.22;
    let t = t.clamp(0.0, 1.0);
    if t < 0.45 {
        lerp_rgb(dimmed, color, t * INV_045)
    } else if t < 0.78 {
        lerp_rgb(color, whitened, (t - 0.45) * INV_033)
    } else {
        lerp_rgb(whitened, ICE_WHITE, (t - 0.78) * INV_022)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_util::ink;
    use std::time::Duration;

    const fn geom() -> Geom {
        Geom {
            cw: 8,
            ch: 16,
            rows: 6,
            cols: 40,
            origin_x: 0,
            origin_y: 0,
            win_w: 320,
            win_h: 96,
            head: 0,
        }
    }
    const fn cfg() -> CometConfig {
        CometConfig {
            light: crate::cursor_glow::Light::OFF,
            enabled: true,
            intensity: 1.0,
            color: 0x009E_D6FF,
            accent: 0x00C8_EAFF,
            dark_theme: true,
            theme_fg: 0x00C8_C8D2,
            theme_bg: 0x0010_1016,
            pinned: false,
        }
    }

    #[test]
    fn disabled_or_zero_intensity_is_inert() {
        for (why, config) in [
            (
                "disabled",
                CometConfig {
                    enabled: false,
                    ..cfg()
                },
            ),
            (
                "zero intensity",
                CometConfig {
                    intensity: 0.0,
                    ..cfg()
                },
            ),
        ] {
            let mut nc = CursorComet::default();
            let mut out = Vec::new();
            let f = nc.tick(
                Some((1, 1)),
                Instant::now(),
                1.0,
                geom(),
                &config,
                &mut out,
                &mut Vec::new(),
            );
            assert!(f.fill.is_none(), "{why}: keeps the plain cursor");
            assert_eq!(f.fp, 0, "{why}");
            assert!(out.is_empty(), "{why}: no additive light");
            assert!(!nc.is_active(), "{why}: settled");
        }
    }

    #[test]
    fn hidden_cursor_draws_no_coma() {
        let mut nc = CursorComet::default();
        let mut out = Vec::new();
        let f = nc.tick(
            None,
            Instant::now(),
            0.5,
            geom(),
            &cfg(),
            &mut out,
            &mut Vec::new(),
        );
        assert!(out.is_empty(), "no additive light without a cursor cell");
        assert!(f.fill.is_some());
    }

    #[test]
    fn blaze_swells_and_brightens() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let run = |blaze: f32| -> (u64, i32, i32) {
            let mut nc = CursorComet::default();
            let mut out = Vec::new();
            nc.tick(Some((3, 20)), t, blaze, g, &c, &mut out, &mut Vec::new());
            out.clear();
            nc.tick(
                Some((3, 20)),
                t + Duration::from_millis(16),
                blaze,
                g,
                &c,
                &mut out,
                &mut Vec::new(),
            );
            let x0 = out.iter().map(|q| q.x as i32).min().unwrap_or(0);
            let x1 = out.iter().map(|q| (q.x + q.w) as i32).max().unwrap_or(0);
            (ink(&out), x0, x1)
        };
        let (cold_ink, cold_x0, cold_x1) = run(0.0);
        let (hot_ink, hot_x0, hot_x1) = run(1.0);
        assert!(cold_ink > 0, "the resting nucleus is still visibly lit");
        assert!(
            hot_ink > cold_ink * 2,
            "full blaze far outshines the cold drift ({hot_ink} vs {cold_ink})"
        );
        assert!(
            hot_x1 - hot_x0 > cold_x1 - cold_x0,
            "the sunlit coma swells wider ({} vs {})",
            hot_x1 - hot_x0,
            cold_x1 - cold_x0
        );
    }

    const fn light_cfg() -> CometConfig {
        CometConfig {
            color: 0x0009_69DA,
            accent: 0x0040_90F0,
            dark_theme: false,
            theme_fg: 0x001F_2328,
            theme_bg: 0x00FF_FFFF,
            ..cfg()
        }
    }

    fn contrast(a: u32, b: u32) -> f32 {
        let (x, y) = (
            crate::color_math::relative_luminance(a),
            crate::color_math::relative_luminance(b),
        );
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn light_nucleus_inks_a_round_head_under_the_letters() {
        let g = geom();
        let (row, col) = (3u16, 20u16);
        let inked = |blaze: f32| {
            let mut nc = CursorComet::default();
            let (mut out, mut under) = (Vec::new(), Vec::new());
            nc.tick(
                Some((row, col)),
                Instant::now(),
                blaze,
                g,
                &light_cfg(),
                &mut out,
                &mut under,
            );
            assert!(out.is_empty(), "no additive light on paper");
            under
        };
        assert!(
            inked(1.0).is_empty(),
            "at full blaze the streak's nucleus is the head"
        );
        let width = |blaze: f32| {
            let mut nc = CursorComet::default();
            let (mut out, mut under) = (Vec::new(), Vec::new());
            nc.tick(
                Some((row, col)),
                Instant::now(),
                blaze,
                g,
                &light_cfg(),
                &mut out,
                &mut under,
            );
            assert!(out.is_empty(), "no additive light on paper");
            assert!(!under.is_empty(), "the head is inked (blaze {blaze})");
            let top = row * g.ch as u16;
            assert!(
                under
                    .iter()
                    .all(|q| q.alpha > 0 && q.y >= top && q.y + q.h <= top + g.ch as u16),
                "source-over, inside its row"
            );
            let x0 = under.iter().map(|q| q.x).min().unwrap_or(0);
            let x1 = under.iter().map(|q| q.x + q.w).max().unwrap_or(0);
            let (left, right) = (col * g.cw as u16, (col + 1) * g.cw as u16);
            assert!(
                x0 + g.cw as u16 / 4 <= left && x1 >= right + g.cw as u16 / 4,
                "past both edges of the caret's cell: {x0}..{x1}"
            );
            x1 - x0
        };
        assert!(
            width(0.1) > g.cw as u16,
            "the resting head is round and wide"
        );
        let mut nc = CursorComet::default();
        let (mut out, mut under) = (Vec::new(), Vec::new());
        nc.tick(
            Some((row, col)),
            Instant::now(),
            0.5,
            g,
            &cfg(),
            &mut out,
            &mut under,
        );
        assert!(
            !out.is_empty() && under.is_empty(),
            "dark: the additive coma"
        );
    }

    #[test]
    fn light_ground_nucleus_deepens_under_the_keys() {
        let g = geom();
        let fill = |c: &CometConfig, blaze: f32| {
            let mut nc = CursorComet::default();
            nc.tick(
                Some((1, 1)),
                Instant::now(),
                blaze,
                g,
                c,
                &mut Vec::new(),
                &mut Vec::new(),
            )
            .fill
            .expect("a fill")
        };
        let c = light_cfg();
        let (cold, hot) = (fill(&c, 0.0), fill(&c, 1.0));
        let paper = c.theme_bg;
        assert!(contrast(hot, paper) >= contrast(cold, paper));
        assert!(contrast(hot, paper) >= contrast(c.color, paper));
        let luma = crate::color_math::relative_luminance;
        let d = cfg();
        assert!(luma(fill(&d, 1.0)) > luma(fill(&d, 0.0)), "dark: it frosts");
    }

    #[test]
    fn quads_respect_grid_hue_and_cap() {
        let g = geom();
        let c = cfg();
        let mut nc = CursorComet::default();
        let mut out = Vec::new();
        let t = Instant::now();
        nc.tick(Some((0, 0)), t, 1.0, g, &c, &mut out, &mut Vec::new());
        nc.tick(
            Some((0, 0)),
            t + Duration::from_millis(16),
            1.0,
            g,
            &c,
            &mut out,
            &mut Vec::new(),
        );
        assert!(!out.is_empty());
        let gw = (g.cols * g.cw) as u32;
        let gh = (g.rows * g.ch) as u32;
        for q in &out {
            let band = q.row as u32 * g.ch as u32;
            assert!(
                q.y as u32 >= band && q.y as u32 + q.h as u32 <= band + g.ch as u32,
                "single-row: {q:?}"
            );
            assert!(
                q.x as u32 + q.w as u32 <= gw && q.y as u32 + q.h as u32 <= gh,
                "in grid: {q:?}"
            );
            let (r, gg, b) = (
                (q.color >> 16) & 0xff,
                (q.color >> 8) & 0xff,
                q.color & 0xff,
            );
            assert!(
                b + 1 >= gg && gg + 1 >= r,
                "premultiplied icy hue ordering: {q:?}"
            );
            for sh in [16u32, 8, 0] {
                assert!(
                    (q.color >> sh) & 0xff <= COV_CAP as u32 + 1,
                    "coverage capped: {q:?}"
                );
            }
        }
    }

    #[test]
    fn settles_to_inactive_when_blaze_dies() {
        let g = geom();
        let c = cfg();
        let mut nc = CursorComet::default();
        let mut out = Vec::new();
        let t = Instant::now();
        nc.tick(Some((1, 1)), t, 0.8, g, &c, &mut out, &mut Vec::new());
        assert!(nc.is_active(), "a charged nucleus keeps the tick armed");
        nc.tick(
            Some((1, 1)),
            t + Duration::from_millis(16),
            0.0,
            g,
            &c,
            &mut out,
            &mut Vec::new(),
        );
        assert!(
            !nc.is_active(),
            "the settled nucleus idles on the blink cadence"
        );
    }

    #[test]
    fn nucleus_follows_the_configured_hue() {
        let g = geom();
        let t = Instant::now();
        let mut out = Vec::new();
        let mut nc = CursorComet::default();
        let mut c = cfg();
        c.color = 0x00FF_4020;
        let f = nc
            .tick(Some((1, 1)), t, 0.2, g, &c, &mut out, &mut Vec::new())
            .fill
            .unwrap();
        let (r, gg, b) = ((f >> 16) & 0xff, (f >> 8) & 0xff, f & 0xff);
        assert!(
            r > b && r > gg,
            "the fill follows the pinned hue, got {f:#08x}"
        );
    }

    #[test]
    fn coma_surrounds_the_caret_in_a_padded_headed_window() {
        let t0 = Instant::now();
        let real = Geom {
            cw: 15,
            ch: 28,
            rows: 30,
            cols: 100,
            origin_x: 24,
            origin_y: 84,
            win_w: 1548,
            win_h: 948,
            head: 80,
        };
        let (cr, cc) = (12u16, 47u16);
        let mut e = CursorComet::default();
        let mut out = Vec::new();
        e.tick(
            Some((cr, cc)),
            t0,
            1.0,
            real,
            &cfg(),
            &mut out,
            &mut Vec::new(),
        );
        assert!(!out.is_empty(), "the emitter drew nothing to place");
        let (x0c, y0c) = (
            i32::from(real.origin_x) + i32::from(cc) * real.cw as i32,
            i32::from(real.origin_y) + i32::from(cr) * real.ch as i32,
        );
        let (x1c, y1c) = (x0c + real.cw as i32, y0c + real.ch as i32);
        let x0 = out.iter().map(|q| i32::from(q.x)).min().unwrap();
        let x1 = out.iter().map(|q| i32::from(q.x + q.w)).max().unwrap();
        let y0 = out.iter().map(|q| i32::from(q.y)).min().unwrap();
        let y1 = out.iter().map(|q| i32::from(q.y + q.h)).max().unwrap();
        assert!(
            x0 < x0c && x1 > x1c && y0 < y0c && y1 > y1c,
            "bbox x {x0}..{x1} y {y0}..{y1} does not surround the caret cell \
             x {x0c}..{x1c} y {y0c}..{y1c} — a grid-relative emitter lands at \
             x {}..  y {}.., short by (origin_x, origin_y) = ({}, {})",
            x0 + i32::from(real.origin_x),
            y0 + i32::from(real.origin_y),
            real.origin_x,
            real.origin_y
        );
        for q in &out {
            assert!(
                i32::from(q.x) >= real.fx_left() && i32::from(q.x + q.w) <= real.fx_right(),
                "quad x {}..{} outside the effects box",
                q.x,
                q.x + q.w
            );
            assert!(
                i32::from(q.y) >= real.fx_top() && i32::from(q.y + q.h) <= real.fx_bot(),
                "quad y {}..{} outside the effects box",
                q.y,
                q.y + q.h
            );
        }
        let mut at_origin = Vec::new();
        let mut f = CursorComet::default();
        f.tick(
            Some((3, 10)),
            t0,
            1.0,
            geom(),
            &cfg(),
            &mut at_origin,
            &mut Vec::new(),
        );
        let mut shifted = Vec::new();
        let mut g = CursorComet::default();
        let mut o = geom();
        o.origin_x = 24;
        o.origin_y = 84;
        o.win_w += 48;
        o.win_h += 84;
        o.head = 84;
        g.tick(
            Some((3, 10)),
            t0,
            1.0,
            o,
            &cfg(),
            &mut shifted,
            &mut Vec::new(),
        );
        assert_eq!(
            at_origin.len(),
            shifted.len(),
            "the origin changed the quad COUNT"
        );
        for (a, b) in at_origin.iter().zip(&shifted) {
            assert_eq!(
                (i32::from(a.x) + 24, i32::from(a.y) + 84, a.w, a.h, a.color),
                (i32::from(b.x), i32::from(b.y), b.w, b.h, b.color),
                "a quad is not the grid form translated by the origin"
            );
        }
    }

    fn light_per_pixel(out: &[GlowQuad], w: usize, h: usize) -> Vec<u32> {
        let mut px = vec![[0u32; 3]; w * h];
        for q in out {
            assert_eq!(q.alpha, 0, "the dark coma is additive light");
            for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                    for (k, sh) in [16u32, 8, 0].into_iter().enumerate() {
                        px[y * w + x][k] += (q.color >> sh) & 0xff;
                    }
                }
            }
        }
        px.iter().map(|c| c[0].max(c[1]).max(c[2])).collect()
    }

    #[test]
    fn the_coma_outside_the_caret_stays_inside_the_over_ink_budget() {
        let g = geom();
        let (w, h) = (g.cols * g.cw, g.rows * g.ch);
        let (cr, cc) = (3u16, 20u16);
        let (x0, y0) = (usize::from(cc) * g.cw, usize::from(cr) * g.ch);
        let mut nc = CursorComet::default();
        let t = Instant::now();
        let mut heart_max = 0u32;
        for i in 0..40u64 {
            let mut out = Vec::new();
            nc.tick(
                Some((cr, cc)),
                t + Duration::from_millis(16 * i),
                1.0,
                g,
                &cfg(),
                &mut out,
                &mut Vec::new(),
            );
            let light = light_per_pixel(&out, w, h);
            for y in 0..h {
                for x in 0..w {
                    let inside = (x0..x0 + g.cw).contains(&x) && (y0..y0 + g.ch).contains(&y);
                    let l = light[y * w + x];
                    if inside {
                        heart_max = heart_max.max(l);
                    } else {
                        assert!(
                            l as f32 <= OVER_INK_COV_CAP + 3.0,
                            "frame {i}: {l} levels of light on ({x},{y}) outside the caret"
                        );
                    }
                }
            }
        }
        assert!(
            heart_max as f32 > OVER_INK_COV_CAP,
            "control: the stacked heart inside the caret ({heart_max}) exceeds the budget"
        );
    }

    /// looks10 L9, THE COMA IS ROUND LIGHT ALONE: through a second and a
    /// half of a charged caret on a dark page, the coma lays no cross (a bar
    /// one pixel tall with a stroke one pixel wide standing on it or hanging
    /// from it inside its span). Negative control: its rim's twinkling
    /// glints (`rim_glints`), the grey `+` beside the caret in the trail
    /// gallery's dark Comet, lay crosses.
    #[test]
    fn the_coma_twinkles_no_cross() {
        let g = geom();
        let crosses = |glints: bool| {
            let mut nc = CursorComet {
                rim_glints: glints,
                ..CursorComet::default()
            };
            let t = Instant::now();
            let mut n = 0usize;
            for i in 0..90u64 {
                let mut out = Vec::new();
                nc.tick(
                    Some((3, 20)),
                    t + Duration::from_millis(16 * i),
                    1.0,
                    g,
                    &cfg(),
                    &mut out,
                    &mut Vec::new(),
                );
                for bar in out.iter().filter(|q| q.h == 1 && q.w >= 3) {
                    let (x0, x1, y) = (bar.x, bar.x + bar.w, bar.y);
                    if out.iter().any(|s| {
                        s.w == 1 && s.x > x0 && s.x + 1 < x1 && (s.y + s.h == y || s.y == y + 1)
                    }) {
                        n += 1;
                    }
                }
            }
            n
        };
        assert_eq!(crosses(false), 0, "the coma lays a cross");
        assert!(
            crosses(true) > 0,
            "control: the rim's twinkling glints lay no cross"
        );
    }

    #[test]
    fn the_light_comet_keeps_its_caret_and_lays_nothing_over_the_letters() {
        const WHITE: u32 = 0x00FF_FFFF;
        let g = geom();
        let (cr, cc) = (3u16, 20u16);
        let cell = (
            i32::from(cc) * g.cw as i32,
            i32::from(cr) * g.ch as i32,
            g.cw as i32,
            g.ch as i32,
        );
        let caret = |dark: bool| {
            let mut nc = CursorComet::default();
            let mut out = Vec::new();
            let c = CometConfig {
                dark_theme: dark,
                theme_fg: if dark { 0x00C8_C8D2 } else { 0x001F_2328 },
                theme_bg: if dark { 0x0010_1016 } else { WHITE },
                ..cfg()
            };
            let fill = nc
                .tick(
                    Some((cr, cc)),
                    Instant::now(),
                    1.0,
                    g,
                    &c,
                    &mut out,
                    &mut Vec::new(),
                )
                .fill
                .expect("an enabled nucleus fills the block");
            let worst = crate::effect_util::caret_cell_worst_contrast(fill, &out, cell, WHITE);
            (worst, out)
        };
        let (light, over) = caret(false);
        assert!(
            light >= 3.0,
            "light arm: the nucleus must keep 3:1 on white, got {light:.2}:1"
        );
        assert!(
            over.is_empty(),
            "the light nucleus lays nothing over the letters: {over:?}"
        );
        let (dark_arm, coma) = caret(true);
        assert!(
            dark_arm < 3.0,
            "control: the frosting dark arm sinks into white ({dark_arm:.2}:1)"
        );
        assert!(!coma.is_empty(), "control: the dark arm lays its coma");
    }

    #[test]
    fn the_light_nucleus_rests_clearly_above_the_page_floor() {
        use crate::color_math::{contrast_ratio, light_body_fill, rgb2hsv};
        let base = cfg().color;
        for page in [0x00FF_FFFFu32, 0x00F6_F7F9, 0x00FD_F6E3] {
            let fill_at = |blaze: f32| {
                let c = CometConfig {
                    dark_theme: false,
                    theme_bg: page,
                    ..cfg()
                };
                CursorComet::default()
                    .tick(
                        Some((1, 1)),
                        Instant::now(),
                        blaze,
                        geom(),
                        &c,
                        &mut Vec::new(),
                        &mut Vec::new(),
                    )
                    .fill
                    .expect("an enabled nucleus fills the block")
            };
            let rest = fill_at(0.0);
            let ratio = contrast_ratio(rest, page);
            assert!(
                ratio >= LIGHT_NUCLEUS_CONTRAST,
                "on {page:06x} the resting nucleus {rest:06x} reads {ratio:.2}:1"
            );
            let (h0, _, _) = rgb2hsv(light_body_fill(base, 0.0));
            let (h1, _, v1) = rgb2hsv(rest);
            assert!((h0 - h1).abs() < 4.0, "the ice keeps its hue: {h0} vs {h1}");
            assert!(
                v1 < rgb2hsv(base).2,
                "on {page:06x} the nucleus deepens, never whitens: {rest:06x}"
            );
            let hot = fill_at(1.0);
            assert!(
                contrast_ratio(hot, page) >= ratio,
                "on {page:06x} the heat deepens the nucleus further"
            );
        }
        let unsolved = contrast_ratio(light_body_fill(base, 0.0), 0x00FF_FFFF);
        assert!(
            unsolved < 4.5,
            "control: the deepened ice alone reads {unsolved:.2}:1 on white"
        );
    }
}
