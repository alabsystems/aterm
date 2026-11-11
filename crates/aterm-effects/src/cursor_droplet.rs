// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Liquid droplet block-cursor effect (`water` trail style) with bead, glints, ripples, and drips.

use aterm_time::Instant;

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_fx::{CARET_PAGE_MIN_CONTRAST, EFFECT_CARET_PAGE_GUARD};
use crate::cursor_glow::{Geom, push_ripple_ring};
use crate::effect_util::{push_soft_disc, water_ramp};

/// Slosh rate in turns/second, at rest and at full surge. The phase advances
/// only on rendered frames, so a settled bead sways at the blink cadence.
const SLOSH_IDLE: f32 = 0.9;
const SLOSH_ACTIVE: f32 = 4.2;

/// Bead radius in cell-heights: the diameter must cover the cell height, or
/// the block's corners poke out and it reads as a rectangle.
const RADIUS_IDLE: f32 = 0.52;
const RADIUS_MAX: f32 = 0.78;

/// Innermost-disc coverage before intensity, at rest and at full surge. The
/// outer discs scale down from it, each clamped by [`COV_CAP`].
const COV_IDLE: f32 = 60.0;
const COV_MAX: f32 = 148.0;
/// Per-quad additive coverage ceiling: inner discs saturate here over the
/// cursor cell; rim quads and rings over neighbouring glyphs stay a tint.
const COV_CAP: f32 = 92.0;

/// A soft disc carries its full coverage only at its heart and falls off to
/// nothing at its rim, where the slab discs it replaced held theirs to the
/// edge: this much more coverage keeps the bead as bright to the eye.
const DISC_SOFT_GAIN: f32 = 1.35;

/// Ripple cadence in rings/second, at rest and at full surge. Two rings run
/// half a cycle apart.
const RIPPLE_IDLE: f32 = 0.35;
const RIPPLE_ACTIVE: f32 = 1.15;
/// How far a ring rolls out, in bead-radii, over its life (`u` 0→1).
const RIPPLE_REACH: f32 = 2.2;

/// Drip cadence in drops/second/lane, at rest and at full surge.
const DRIP_IDLE: f32 = 0.45;
const DRIP_ACTIVE: f32 = 1.8;
/// Drip lanes: x offset from the bead centre (× cell width), phase offset, and
/// the surge at which the lane wakes.
const DRIP_LANES: [(f32, f32, f32); 3] =
    [(-0.26, 0.00, 0.0), (0.06, 0.37, 0.30), (0.30, 0.71, 0.62)];
/// How far a drop falls, in cell-heights, before it has fully faded.
const DRIP_FALL: f32 = 1.05;

/// Below this surge the droplet reports inactive, so the host stops the tick.
const SETTLED_SURGE: f32 = 0.02;

/// Per-frame configuration for the droplet cursor effect.
#[derive(Clone, Copy, Debug)]
pub struct DropletConfig {
    /// Whether the `water` style is active on a focused, visible block cursor.
    pub enabled: bool,
    /// Overall effect scale in `0.0..=1.0` (`0.0` is inert).
    pub intensity: f32,
    /// Default background color (`0x00RRGGBB`) against which fill contrast is solved.
    pub ground: Option<u32>,
}

/// Output of [`CursorDroplet::tick`] for one frame.
#[derive(Clone, Copy, Debug)]
pub struct DropletFrame {
    /// Liquid block-fill color (`0x00RRGGBB`), or `None` when disabled.
    pub fill: Option<u32>,
    /// Frame fingerprint of the emitted fill and quads (`0` when inert).
    pub fp: u64,
}

impl DropletFrame {
    /// Dormant frame output (`fill: None, fp: 0`).
    pub const DORMANT: Self = Self { fill: None, fp: 0 };
}

impl Default for DropletFrame {
    #[inline]
    fn default() -> Self {
        Self::DORMANT
    }
}

/// The floor the liquid caret's band is solved to ([`ramp_band_on`]): every effect caret's margin
/// past the page's (the after-land lows' W8). Solved to the floor itself, the band's edge laid
/// Water's caret on Solarized Dark at 4.51:1.
const DROPLET_CARET_FLOOR: f32 = CARET_PAGE_MIN_CONTRAST + EFFECT_CARET_PAGE_GUARD;

/// The band of water-ramp positions whose colour keeps [`DROPLET_CARET_FLOOR`] against
/// `ground`, as `(lo, hi)`.
#[inline]
const fn ramp_band_on(ground: u32, dark: bool) -> (f32, f32) {
    const REF_DARK_BAND: (f32, f32) =
        ramp_band_solve(crate::cursor_glow::REF_DARK_PAGE, true, DROPLET_CARET_FLOOR);
    const REF_WHITE_BAND: (f32, f32) = ramp_band_solve(0x00FF_FFFF, false, DROPLET_CARET_FLOOR);
    let ground = ground & 0x00FF_FFFF;
    if ground == crate::cursor_glow::REF_DARK_PAGE {
        REF_DARK_BAND
    } else if ground == 0x00FF_FFFF {
        REF_WHITE_BAND
    } else {
        ramp_band_solve(ground, dark, DROPLET_CARET_FLOOR)
    }
}

/// The band of water-ramp positions whose colour keeps `floor` against `ground`, as
/// `(lo, hi)`.
#[inline]
const fn ramp_band_solve(ground: u32, dark: bool, floor: f32) -> (f32, f32) {
    let table = crate::color_math::relative_luminance_table();
    let lum_g = crate::color_math::relative_luminance_with(table, ground);
    let lum_g_plus = lum_g + 0.05;
    #[inline]
    const fn meets_rgb(
        table: &crate::color_math::RelativeLuminanceTable,
        (lum_g, lum_g_plus): (f32, f32),
        rgb: u32,
        floor: f32,
    ) -> bool {
        let lum_w = crate::color_math::relative_luminance_with(table, rgb);
        let (hi_plus, lo_plus) = if lum_w > lum_g {
            (lum_w + 0.05, lum_g_plus)
        } else {
            (lum_g_plus, lum_w + 0.05)
        };
        hi_plus / lo_plus >= floor
    }
    let far = if dark { 1.0 } else { 0.0 };
    let far_rgb = water_ramp(far);
    if !meets_rgb(table, (lum_g, lum_g_plus), far_rgb, floor) {
        return (far, far);
    }
    let near = 1.0 - far;
    let near_rgb = water_ramp(near);
    if meets_rgb(table, (lum_g, lum_g_plus), near_rgb, floor) {
        return (0.0, 1.0);
    }
    let (mut pass, mut fail) = (far, near);
    let (mut pass_rgb, mut fail_rgb) = (far_rgb, near_rgb);
    let mut step = 0;
    while step < 20 {
        step += 1;
        let mid = 0.5 * (pass + fail);
        let mid_rgb = water_ramp(mid);
        if mid_rgb == pass_rgb {
            pass = mid;
        } else if mid_rgb == fail_rgb {
            fail = mid;
        } else if meets_rgb(table, (lum_g, lum_g_plus), mid_rgb, floor) {
            pass = mid;
            pass_rgb = mid_rgb;
        } else {
            fail = mid;
            fail_rgb = mid_rgb;
        }
        if step >= 2 && pass_rgb == fail_rgb {
            break;
        }
    }
    if dark { (pass, 1.0) } else { (0.0, pass) }
}

/// Per-window state for the liquid droplet cursor effect.
pub struct CursorDroplet {
    /// Rolling slosh (caustic shimmer) phase in turns `0..1`.
    slosh: f32,
    /// Rolling ripple-ring phase in ring-lives `0..1` (two rings ride it, half a
    /// cycle apart).
    ripple: f32,
    /// Rolling drip phase in drop-lives `0..1` (each lane adds its own offset).
    drip: f32,
    last: Option<Instant>,
    /// Latched surge at the last tick, so [`Self::is_active`] needs no clock.
    surge: f32,
    /// Cached `(ground, (lo, hi))` from [`ramp_band_on`].
    band_memo: Option<(u32, (f32, f32))>,
    /// A test's negative control: the ripples and drips run past the row's
    /// foot as they did before [`cut_at_surface`].
    #[cfg(test)]
    uncut: bool,
}

impl Default for CursorDroplet {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl CursorDroplet {
    /// Fresh dormant droplet-cursor animation state.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slosh: 0.0,
            ripple: 0.0,
            drip: 0.0,
            last: None,
            surge: 0.0,
            band_memo: None,
            #[cfg(test)]
            uncut: false,
        }
    }

    /// Reset animation state while preserving the contrast-band memo.
    #[inline]
    pub const fn reset(&mut self) {
        *self = Self {
            band_memo: self.band_memo,
            ..Self::new()
        };
    }

    /// Returns whether the droplet surge is still active and requires animation ticks.
    #[must_use]
    #[inline]
    pub const fn is_active(&self) -> bool {
        self.surge > SETTLED_SURGE
    }

    /// Advances one frame at `now` and appends additive droplet quads to `out`.
    #[inline]
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        surge: f32,
        geom: Geom,
        cfg: &DropletConfig,
        out: &mut Vec<GlowQuad>,
    ) -> DropletFrame {
        let e = (surge.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || cfg.intensity <= 0.0 {
            self.surge = 0.0;
            self.last = Some(now);
            return DropletFrame::DORMANT;
        }
        self.surge = e;

        let dt = match self.last {
            Some(t) => now.saturating_duration_since(t).as_secs_f32().min(0.25),
            None => 0.0,
        };
        self.last = Some(now);
        if dt > 0.0 {
            self.slosh = (self.slosh + dt * (SLOSH_IDLE + SLOSH_ACTIVE * e)).fract();
            self.ripple = (self.ripple + dt * (RIPPLE_IDLE + RIPPLE_ACTIVE * e)).fract();
            self.drip = (self.drip + dt * (DRIP_IDLE + DRIP_ACTIVE * e)).fract();
        }
        let phase = self.slosh * std::f32::consts::TAU;
        let (phase_sin, phase_cos) = crate::supernova::sin_cos_f32(phase);
        let shimmer =
            0.5 + 0.35 * phase_sin + 0.15 * crate::supernova::sin_cos_f32(phase * 2.7 + 1.3).0;

        let dark = match cfg.ground {
            Some(ground) => aterm_render::theme_is_dark(ground),
            None => true,
        };
        let caustic = 0.05 * (shimmer - 0.5);
        let t = if dark {
            0.36 + 0.42 * e + caustic
        } else {
            0.36 - 0.16 * e + caustic
        };
        let t = match cfg.ground {
            Some(ground) => {
                let (lo, hi) = match self.band_memo {
                    Some((g, band)) if g == ground => band,
                    _ => {
                        let band = ramp_band_on(ground, dark);
                        self.band_memo = Some((ground, band));
                        band
                    }
                };
                t.clamp(lo, hi)
            }
            None => t,
        };
        let fill = water_ramp(t);

        if dark
            && let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
        {
            self.emit_bead(cr, cc, e, shimmer, (phase_sin, phase_cos), geom, cfg, out);
        }

        let fp = ((self.slosh * 512.0) as u64)
            .wrapping_mul(1_000_003)
            .wrapping_add(((self.ripple * 512.0) as u64) << 20)
            .wrapping_add(((self.drip * 512.0) as u64) << 40)
            .wrapping_add((e * 255.0) as u64)
            .wrapping_add((fill as u64) << 12)
            | 1;

        DropletFrame {
            fill: Some(fill),
            fp,
        }
    }

    /// Emit the bead, glint, ripple rings and drips as premultiplied additive
    /// quads: ≤ ~4 discs × (2·radius / slab) slabs, two rings, three drips,
    /// two glints.
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor cell + surge/shimmer/phase + geometry/config; one internal call site"
    )]
    #[inline]
    fn emit_bead(
        &self,
        cr: u16,
        cc: u16,
        e: f32,
        shimmer: f32,
        (phase_sin, phase_cos): (f32, f32),
        geom: Geom,
        cfg: &DropletConfig,
        out: &mut Vec<GlowQuad>,
    ) {
        const DISC_PHASE_SIN_COS: [(f32, f32); 4] = {
            let mut arr = [(0.0f32, 1.0f32); 4];
            let mut k = 1;
            while k < 4 {
                arr[k] = crate::supernova::sin_cos_f32(k as f32 * 0.6);
                k += 1;
            }
            arr
        };
        const DRIP_PHASE_SIN_COS: [(f32, f32); 3] = {
            let mut arr = [(0.0f32, 1.0f32); 3];
            let mut k = 0;
            while k < 3 {
                arr[k] = crate::supernova::sin_cos_f32(DRIP_LANES[k].1 * 7.0);
                k += 1;
            }
            arr
        };
        const INV_DRIP_GROW: f32 = 1.0 / 0.30;
        const INV_DRIP_FALL: f32 = 1.0 / 0.70;

        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let cx = f32::from(geom.origin_x) + (cc as f32 + 0.5) * cw;
        let cy = f32::from(geom.origin_y) + (cr as f32 + 0.46) * ch;
        let radius = (RADIUS_IDLE + (RADIUS_MAX - RADIUS_IDLE) * e) * ch;
        let cov_core =
            (COV_IDLE + (COV_MAX - COV_IDLE) * e) * cfg.intensity * (0.84 + 0.16 * shimmer);

        let discs: [(f32, f32, f32); 4] = [
            (1.00, 0.30, 0.24 + 0.06 * e),
            (0.82, 0.58, 0.46 + 0.12 * e),
            (0.62, 0.85, 0.66 + 0.16 * e),
            (0.40, 1.00, 0.86 + 0.14 * e),
        ];
        let e_shear = 0.30 + 0.70 * e;
        let mut k = 0;
        while k < discs.len() {
            let (rx, cov_x, ramp_t) = discs[k];
            let cur_k = k;
            k += 1;
            let r = radius * rx;
            if r < 1.0 {
                continue;
            }
            let cov = (cov_core * cov_x * DISC_SOFT_GAIN).min(COV_CAP);
            let (sin_dk, cos_dk) = DISC_PHASE_SIN_COS[cur_k];
            let s = phase_sin * cos_dk + phase_cos * sin_dk;
            let shear = s * r * 0.10 * e_shear;
            push_soft_disc(
                out,
                geom,
                (cx + shear, cy + r * 0.06),
                (r * 0.94, r),
                water_ramp(ramp_t),
                cov,
            );
        }

        let glint_cov = (cov_core * (0.80 + 0.20 * shimmer)).min(COV_CAP);
        let gx = cx - radius * 0.30 + (phase_sin * radius * 0.05);
        let gr = (radius * 0.14).max(1.5);
        push_soft_disc(
            out,
            geom,
            (gx, cy - radius * 0.30),
            (gr, gr * 0.7),
            0x00B4_EAF0,
            glint_cov,
        );
        push_soft_disc(
            out,
            geom,
            (cx + radius * 0.20, cy + radius * 0.14),
            (gr * 0.6, gr * 0.5),
            0x00B4_EAF0,
            glint_cov * 0.5,
        );

        // The ripples and drips stay in the caret's row: a ring rolled out
        // under the bead, and a drop falling off it, would reach into the row
        // below (the ripple on the selected line under the prompt), so both
        // are cut at the row's foot, the water's surface: the near side of a
        // ring is under it, and a drop falls through it.
        let foot = i32::from(geom.origin_y) + (i32::from(cr) + 1) * ch as i32;
        #[cfg(test)]
        let cut_surface = !self.uncut;
        #[cfg(not(test))]
        let cut_surface = true;
        let surface_from = out.len();
        let cov_ring_scale = cov_core * 0.55 * e;
        if cov_ring_scale >= 1.0 {
            let ring_rgb = water_ramp(0.74 + 0.10 * e);
            let y0 = cy + radius * 0.88;
            let halves = [0.0f32, 0.5];
            let mut hi = 0;
            while hi < halves.len() {
                let half = halves[hi];
                hi += 1;
                let u = (self.ripple + half).fract();
                let one_minus_u = 1.0 - u;
                let env = one_minus_u * crate::color_math::sqrt_f32(one_minus_u);
                let cov = (cov_ring_scale * env).min(COV_CAP) as u8;
                if cov == 0 {
                    continue;
                }
                let rx = radius * (0.9 + RIPPLE_REACH * u);
                let ry = (rx * 0.22).max(2.0);
                let thick = (rx * 0.12).max(1.5);
                if cut_surface && (y0 - ry - thick * 0.5).floor() as i32 >= foot {
                    continue;
                }
                let ring = premul_rgb(ring_rgb, cov);
                push_ripple_ring(out, geom, (cx, y0), (rx, ry), thick, ring);
            }
        }
        const DRIP_BODY_RGB: u32 = water_ramp(0.68);
        let belly = cy + radius * 0.80;
        let body = DRIP_BODY_RGB;
        let cov_drip = cov_core * 0.70;
        let r_grow = radius * 0.08;
        let r_fall = 1.2 + radius * 0.05;
        let mut lane_idx = 0;
        while lane_idx < DRIP_LANES.len() {
            let (dx, off, wake) = DRIP_LANES[lane_idx];
            let cur_lane = lane_idx;
            lane_idx += 1;
            if e < wake && wake > 0.0 {
                continue;
            }
            let u = (self.drip + off).fract();
            let (y, r, stretch, cov) = if u < 0.30 {
                let s = u * INV_DRIP_GROW;
                (belly, 1.2 + r_grow * s, 1.0, cov_drip * s)
            } else {
                let v = (u - 0.30) * INV_DRIP_FALL;
                (
                    belly + v * v * DRIP_FALL * ch,
                    r_fall,
                    1.0 + 0.8 * v,
                    cov_drip * (1.0 - v),
                )
            };
            if cov < 1.0 {
                continue;
            }
            let ry = r * stretch;
            if cut_surface && (y - ry).floor() as i32 >= foot {
                continue;
            }
            let (sin_do, cos_do) = DRIP_PHASE_SIN_COS[cur_lane];
            let x = cx + dx * cw + (phase_sin * cos_do + phase_cos * sin_do) * 1.5;
            push_soft_disc(out, geom, (x, y), (r, ry), body, cov.min(COV_CAP));
        }
        if !cut_surface || out.len() <= surface_from {
            return;
        }
        cut_at_surface(out, surface_from, foot);
    }
}

/// Cut the quads `out[from..]` at window row `foot`: a quad wholly at or below
/// it is dropped, one that crosses it ends there.
#[inline]
fn cut_at_surface(out: &mut Vec<GlowQuad>, from: usize, foot: i32) {
    let mut keep = from;
    let mut i = from;
    while i < out.len() {
        let mut q = out[i];
        let cur_i = i;
        i += 1;
        let (y, h) = (i32::from(q.y), i32::from(q.h));
        if y >= foot || h == 0 {
            continue;
        }
        if y + h > foot {
            q.h = (foot - y) as u16;
        }
        if keep != cur_i || q.h != out[cur_i].h {
            out[keep] = q;
        }
        keep += 1;
    }
    out.truncate(keep);
}

#[cfg(test)]
#[path = "cursor_droplet_tests.rs"]
mod tests;
