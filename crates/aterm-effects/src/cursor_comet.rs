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
    /// [`Self::color`] is one the user chose (a pinned cursor colour, or the trail colour):
    /// paper darkens the nucleus in its own hue and saturation, never saturates it
    /// ([`crate::color_math::light_caret_fill`]).
    pub chosen: bool,
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
    light_fill_memo: Option<((u32, u32, u32, bool), u32)>,
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
        if dt > 0.0 {
            self.shimmer = (self.shimmer + dt * (SHIMMER_IDLE + SHIMMER_ACTIVE * e)).fract();
        }
        let phase = self.shimmer * std::f32::consts::TAU;
        let breathe = 0.5
            + 0.35 * crate::supernova::sin_cos_f32(phase).0
            + 0.15 * crate::supernova::sin_cos_f32(phase * 2.3 + 0.9).0;

        let mut dark_stops = (0u32, 0u32);
        let styled = if cfg.dark_theme {
            // A colour the user chose freezes toward white, not ice: the ice's blue is the
            // style's own, and under a pin no part of the trail keeps it (composed r0's judges).
            let ice = if cfg.chosen { 0x00FF_FFFF } else { ICE_WHITE };
            let dimmed = lerp_rgb(cfg.color, 0x0000_0000, 0.50);
            let whitened = lerp_rgb(cfg.color, ice, 0.55);
            dark_stops = (dimmed, whitened);
            ice_ramp_with(
                cfg.color,
                (dimmed, whitened, ice),
                0.34 + 0.40 * e + 0.05 * (breathe - 0.5),
            )
        } else {
            let key = (
                cfg.color & 0x00FF_FFFF,
                cfg.theme_bg & 0x00FF_FFFF,
                e.to_bits(),
                cfg.chosen,
            );
            if let Some((prev_key, fill)) = self.light_fill_memo
                && prev_key == key
            {
                fill
            } else {
                let fill = crate::color_math::floor_contrast_keeping_hue(
                    crate::color_math::light_caret_fill(cfg.color, e, cfg.chosen),
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
        use crate::cursor_glow::{LIGHT_BODY_ALPHA_CAP, LightGround, push_ink_ellipse};
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
            let ice = if cfg.chosen { 0x00FF_FFFF } else { ICE_WHITE };
            let ramp = ice_ramp_with(cfg.color, (dimmed, whitened, ice), t0 + dt * e);
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
        let cov_base = cov_core * (0.35 + 0.65 * e);
        if cov_base < 1.0 {
            return;
        }
        const GLINT_TW_PHASES: [f32; GLINTS as usize] = [0.0, 2.4, 4.8, 7.2];
        const INV_GLINT_TW_SPAN: f32 = 1.0 / 0.45;
        let glint_r = radius * 0.72;
        let arm = ((ch * 0.11) as i32).max(1);
        let two_arm_plus_1 = 2 * arm + 1;
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
/// near-white): dimmed hue, pure hue, whitened hue, `ice` ([`ICE_WHITE`], or white under a
/// chosen colour).
#[inline]
const fn ice_ramp_with(color: u32, (dimmed, whitened, ice): (u32, u32, u32), t: f32) -> u32 {
    const INV_045: f32 = 1.0 / 0.45;
    const INV_033: f32 = 1.0 / 0.33;
    const INV_022: f32 = 1.0 / 0.22;
    let t = t.clamp(0.0, 1.0);
    if t < 0.45 {
        lerp_rgb(dimmed, color, t * INV_045)
    } else if t < 0.78 {
        lerp_rgb(color, whitened, (t - 0.45) * INV_033)
    } else {
        lerp_rgb(whitened, ice, (t - 0.78) * INV_022)
    }
}

#[cfg(test)]
#[path = "cursor_comet_tests.rs"]
mod tests;
