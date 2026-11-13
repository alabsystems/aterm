// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typing-momentum glow: the cursor glows with how fast you type and cools when you stop.

use aterm_scene::smoothstep;

pub mod admission;
pub mod snapshot;
#[cfg(test)]
mod test_fixtures;

pub use admission::{
    AdmitRefusal, GeometryFault, MOMENTUM_ENVELOPE, MOMENTUM_PROFILE, MomentumProfile,
    ScratchBudget, admit_frame_interval, admit_tau, admit_tick, admit_time,
};
pub use snapshot::{
    MOMENTUM_GLOW_SNAPSHOT_SCHEMA, MomentumGlowSnapshot, SnapshotField, SnapshotRefusal,
};

use std::time::Duration;

use aterm_render::{GlowQuad, premul_rgb};
use aterm_time::Instant;

use crate::cursor_glow::{Geom, OVER_INK_COV_CAP};
use crate::effect_util::mix_rgb;
use crate::effect_util::push_fx_ramp;

/// What one printable key adds, from wherever the decayed value is.
pub const MOMENTUM_GLOW_KEY_CREDIT: f32 = 0.10;

/// The cool-down time constant in seconds (half-life ≈ 0.83 s).
pub const MOMENTUM_GLOW_TAU_S: f32 = 1.2;

/// At or above this the cursor is hot: blink suppressed, halo drawn.
pub const MOMENTUM_GLOW_HOT: f32 = 0.02;

const SNAP_ZERO: f32 = 0.005;

/// Halo reach in cell widths at full momentum, beyond the cell's own half.
pub const MOMENTUM_GLOW_RADIUS_CELLS: f32 = 1.6;

const HALO_PEAK_COV: f32 = OVER_INK_COV_CAP;

const HALO_SEGMENTS: usize = 4;

const HALO_ROW_DIV: f32 = 12.0;

const HALO_WARMTH: f32 = 0.7;
const HALO_WARM_CORE: f32 = 0.42;

/// The light-page glow's peak opacity at full momentum, beneath the letters.
pub(crate) const LIGHT_GLOW_PEAK: f32 = crate::cursor_glow::LIGHT_BODY_ALPHA_CAP * 0.85;

const SETTLE_MARGIN: Duration = Duration::from_millis(1);

/// The typing-rate integrator — pure in the injected `now`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TypingRate {
    /// Value at the `at` stamp (reads decay it lazily from there).
    value: f32,
    /// The last key stamp; `None` ⇒ at rest (0).
    at: Option<Instant>,
}

impl TypingRate {
    /// The value at `now`: decayed over the elapsed gap on `tau`, snapped to exactly 0 once
    /// negligible.
    #[must_use]
    #[inline]
    pub fn value(&self, now: Instant, tau: f32) -> f32 {
        if self.value < SNAP_ZERO {
            return 0.0;
        }
        let Some(at) = self.at else {
            return 0.0;
        };
        if !tau.is_finite() || tau <= 0.0 {
            return 0.0;
        }
        let dt = now.saturating_duration_since(at).as_secs_f32();
        let v = if dt == 0.0 {
            self.value
        } else {
            self.value * (-dt / tau).exp()
        };
        if !v.is_finite() || v < SNAP_ZERO {
            0.0
        } else {
            v.min(1.0)
        }
    }

    /// One printable key at `now`: decay to `now`, credit, clamp to 1, stamp.
    #[inline]
    pub fn on_key(&mut self, now: Instant, tau: f32) {
        self.value = (self.value(now, tau) + MOMENTUM_GLOW_KEY_CREDIT).min(1.0);
        self.at = Some(now);
    }
}

/// Host-side inputs for one tick.
#[derive(Clone, Copy, Debug)]
pub struct MomentumGlowConfig {
    /// The user's switch (default on) combined with the host's gates: serious mode, focus, live
    /// viewport.
    pub enabled: bool,
    /// `0..=1` — the aurora's folded intensity (the brightness dial × the motion amplitude × the
    /// load-shed envelope), as every body takes it.
    pub intensity: f32,
    /// The cool-down τ in seconds ([`MOMENTUM_GLOW_TAU_S`] unless configured).
    pub tau_s: f32,
    /// Halo reach at full momentum, in cell widths.
    pub radius_cells: f32,
    /// The resolved cursor colour `0x00RRGGBB` — what the tint is built from.
    pub base: u32,
    /// The page the light lands on is dark: additive halo.
    pub dark_theme: bool,
    /// The frame's light (`docs/design/PRISM-GLASS.md` §15.2 1.3, `GlowConfig::light`): what a
    /// light arm will read. Off on every dark ground; nothing reads it yet.
    pub light: crate::cursor_glow::Light,
    /// The hue a light page's glow wears, `0x00RRGGBB`.
    pub hue: u32,
    /// The theme's text and paper, which a light ground's glow solves its ink against.
    pub theme_fg: u32,
    pub theme_bg: u32,
    /// Whether the cursor is a block (the body tint applies); bar/underline get the halo only.
    pub block: bool,
    /// Whether the cursor is a bar: the halo then sits on the bar's insertion point
    /// ([`crate::cursor_fx::BAR_HEAD_DX`] of the cell across), not the block's centre.
    pub bar: bool,
    /// The caret row's letters as columns `first..end` (a blank between two glyphs counts, as it
    /// reads as part of the line), or `None` for a blank row.
    pub glyph_span: Option<(i32, i32)>,
    /// Whether the light-page glow wears its hue's vivid ink (Sparkle's paper gold) rather than
    /// the walk toward white every other glow keeps ([`light_glow_ink`]).
    pub light_vivid: bool,
    /// Whether `hue` is a colour the user CHOSE (a pinned cursor colour, or the trail colour a
    /// style wears): a light page's glow then wears that colour itself, walked toward white only
    /// as far as the theme's text asks ([`chosen_glow_ink`]), its hue kept and its chroma only
    /// falling, never its hue's vivid ink.
    pub chosen_hue: bool,
    /// Whether `base` is a colour the user CHOSE: the block caret's charge then keeps its hue
    /// and never raises its chroma ([`chosen_charge`]), on either page.
    pub chosen_base: bool,
}

impl MomentumGlowConfig {
    /// Where the halo sits on the caret `(cr, cc)`: [`seat_at`] for [`Self::bar`].
    #[inline]
    #[must_use]
    pub(crate) const fn seat(&self, geom: &Geom, at: (u16, u16)) -> (f32, f32) {
        seat_at(geom, at, self.bar)
    }
}

/// Where a halo sits on the caret `(cr, cc)`: on a `bar`, its insertion point
/// ([`crate::cursor_fx::BAR_HEAD_DX`] of the cell across), else the cell's centre, at the
/// cell's middle height.
#[inline]
#[must_use]
pub(crate) const fn seat_at(geom: &Geom, (cr, cc): (u16, u16), bar: bool) -> (f32, f32) {
    let (x, y) = geom.cell_center(cr, cc);
    if bar {
        (
            x + (crate::cursor_fx::BAR_HEAD_DX - 0.5) * geom.cw as f32,
            y,
        )
    } else {
        (x, y)
    }
}

/// One tick's outputs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MomentumGlowFrame {
    /// The block body tint, `None` when cold, off, or not a block.
    pub fill: Option<u32>,
    /// Hot: the host pins a Blinking* style to its Steady* twin.
    pub hot: bool,
    /// Fingerprint of everything visible; 0 when nothing is.
    pub fp: u64,
}

impl MomentumGlowFrame {
    /// Inert frame (`fill: None, hot: false, fp: 0`).
    pub const DEFAULT: Self = Self {
        fill: None,
        hot: false,
        fp: 0,
    };
}

/// Cached light-page glow ink and text-alpha ceiling for `(hue, theme_fg, theme_bg)`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LightInkMemo {
    key: (u32, u32, u32, bool, bool),
    ink: u32,
    text_alpha: f32,
}

/// The engine: one integrator plus the fingerprint law that tells frame cadence from the
/// cool-down's sparse steps.
#[derive(Clone, Copy, Debug, Default)]
pub struct MomentumGlow {
    rate: TypingRate,
    fp_last: u64,
    fp_prev: u64,
    /// The last real tick's post-intensity amplitude; 0 once inert.
    latched: f32,
    /// That tick's instant and τ, for the settle schedule.
    stamp: Option<Instant>,
    tau: f32,
    /// Cached `(ink, text_alpha)` for the light-page glow.
    light_ink_memo: Option<LightInkMemo>,
}

impl MomentumGlow {
    /// Resting momentum glow state.
    pub const DEFAULT: Self = Self {
        rate: TypingRate {
            value: 0.0,
            at: None,
        },
        fp_last: 0,
        fp_prev: 0,
        latched: 0.0,
        stamp: None,
        tau: 0.0,
        light_ink_memo: None,
    };

    /// One printable key.
    #[inline]
    pub fn on_key(&mut self, now: Instant, tau_s: f32) {
        self.rate.on_key(now, tau_s);
    }

    /// The decayed value at `now` — for diagnostics and the host's cadence.
    #[must_use]
    #[inline]
    pub fn value(&self, now: Instant, tau_s: f32) -> f32 {
        self.rate.value(now, tau_s)
    }

    /// Back to rest (layout/space resets only).
    #[inline]
    pub const fn reset(&mut self) {
        let light_ink_memo = self.light_ink_memo;
        *self = Self {
            light_ink_memo,
            ..Self::DEFAULT
        };
    }

    /// Frame cadence: the light is hot and its last two frames differed (the rainbow body's
    /// fingerprint law).
    #[must_use]
    #[inline]
    pub const fn is_active(&self) -> bool {
        self.hot() && self.changed()
    }

    /// Hot, but its last two frames were identical: not settled.
    #[must_use]
    #[inline]
    pub const fn settling(&self) -> bool {
        self.hot() && !self.changed()
    }

    /// Whether the engine is in its exact resting state ([`Self::DEFAULT`], ignoring `light_ink_memo`).
    #[must_use]
    #[inline]
    pub(crate) const fn is_at_rest(&self) -> bool {
        self.rate.value.to_bits() == 0
            && self.rate.at.is_none()
            && self.fp_last == 0
            && self.fp_prev == 0
            && self.latched.to_bits() == 0
            && self.stamp.is_none()
            && self.tau.to_bits() == 0
    }

    /// The last real tick latched an amplitude at or above [`MOMENTUM_GLOW_HOT`].
    #[must_use]
    #[inline]
    pub(crate) const fn hot(&self) -> bool {
        self.latched >= MOMENTUM_GLOW_HOT
    }

    /// The glass changed at the last tick.
    #[must_use]
    #[inline]
    pub(crate) const fn changed(&self) -> bool {
        self.fp_last != self.fp_prev
    }

    /// The instant of the last real tick; `None` once inert.
    #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
    #[must_use]
    #[inline]
    pub(crate) const fn stamp(&self) -> Option<Instant> {
        self.stamp
    }

    /// The analytic instant the latched amplitude decays below [`MOMENTUM_GLOW_HOT`]
    /// (`stamp + τ·ln(a/floor)`).
    #[must_use]
    #[inline]
    pub fn cold_at(&self) -> Option<Instant> {
        self.decays_to(MOMENTUM_GLOW_HOT)
    }

    /// When the glass next changes.
    #[must_use]
    #[inline]
    pub fn next_change_deadline(&self, now: Instant, frame_interval: Duration) -> Option<Instant> {
        let floor = now + frame_interval;
        if self.is_active() {
            return Some(floor);
        }
        if !self.settling() {
            return None;
        }
        let next = self.settle_forecast();
        Some(match next {
            Some(at) => (at + SETTLE_MARGIN).max(floor),
            None => floor,
        })
    }

    /// The earlier of the latched amplitude's next `u8` step and [`Self::cold_at`].
    #[must_use]
    #[inline]
    pub(crate) fn settle_forecast(&self) -> Option<Instant> {
        self.settle_forecast_with_cold(self.cold_at())
    }

    #[must_use]
    #[inline]
    pub(crate) fn settle_forecast_with_cold(&self, cold_at: Option<Instant>) -> Option<Instant> {
        let step = (self.latched * 255.0).floor();
        let step_at = if step >= 1.0 {
            self.decays_to(step / 255.0)
        } else {
            None
        };
        match (step_at, cold_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, b) => b,
        }
    }

    /// The instant the latched amplitude decays to `level`, from the last real
    /// tick: `stamp + τ·ln(latched / level)`.
    #[inline]
    fn decays_to(&self, level: f32) -> Option<Instant> {
        if self.latched < MOMENTUM_GLOW_HOT || level <= 0.0 {
            return None;
        }
        let stamp = self.stamp?;
        let secs = self.tau * (self.latched / level).ln();
        let after = Duration::try_from_secs_f32(secs.max(0.0)).ok()?;
        Some(stamp + after)
    }

    /// One frame for the caret at `cur`: a dark page's halo into `out`, a light page's glow into
    /// `under`.
    #[inline]
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        geom: Geom,
        cfg: &MomentumGlowConfig,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> MomentumGlowFrame {
        let Some((cr, cc)) = cur else {
            return self.inert();
        };
        if !cfg.enabled
            || cfg.intensity <= 0.0
            || !cfg.intensity.is_finite()
            || geom.cw == 0
            || geom.ch == 0
            || (cr as usize) >= geom.rows
            || (cc as usize) >= geom.cols
        {
            return self.inert();
        }
        let raw = self.rate.value(now, cfg.tau_s);
        let a = (raw * cfg.intensity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        if a < MOMENTUM_GLOW_HOT {
            return self.inert();
        }
        self.latched = a;
        self.stamp = Some(now);
        self.tau = cfg.tau_s;

        let q = (a * 255.0) as u8;
        let a = f32::from(q) / 255.0;
        let s = smoothstep(a);
        let fill = if cfg.dark_theme {
            let tint = ramp(a, true);
            emit_halo((cr, cc), a, s, tint, geom, cfg, out);
            if !cfg.block {
                None
            } else if cfg.chosen_base {
                Some(chosen_charge(cfg.base, s, true))
            } else {
                Some(mix_rgb(cfg.base, tint, 0.55 * s))
            }
        } else {
            let key = (
                cfg.hue & 0x00FF_FFFF,
                cfg.theme_fg & 0x00FF_FFFF,
                cfg.theme_bg & 0x00FF_FFFF,
                cfg.light_vivid,
                cfg.chosen_hue,
            );
            let light_ink = match self.light_ink_memo {
                Some(memo) if memo.key == key => (memo.ink, memo.text_alpha),
                _ => {
                    let ink = if cfg.chosen_hue {
                        chosen_glow_ink(cfg.hue, cfg.theme_fg, cfg.theme_bg)
                    } else {
                        light_glow_ink(cfg.hue, cfg.theme_fg, cfg.theme_bg, cfg.light_vivid)
                    };
                    let text_alpha = light_glow_text_alpha(ink, cfg.theme_fg, cfg.theme_bg);
                    self.light_ink_memo = Some(LightInkMemo {
                        key,
                        ink,
                        text_alpha,
                    });
                    (ink, text_alpha)
                }
            };
            emit_light_halo((cr, cc), a, s, light_ink, geom, cfg, under);
            if !cfg.block {
                None
            } else if cfg.chosen_base {
                Some(chosen_charge(cfg.base, s, false))
            } else {
                Some(crate::color_math::light_charge_in_hue(cfg.base, s))
            }
        };

        let fp = u64::from(q)
            .wrapping_mul(1_000_003)
            .wrapping_add(u64::from(fill.unwrap_or(0)) << 8)
            .wrapping_add((u64::from(cr) << 40) ^ (u64::from(cc) << 52))
            .wrapping_add(if cfg.dark_theme {
                0
            } else {
                let span = match cfg.glyph_span {
                    Some((first, end)) => (u64::from(first as u32) << 20) ^ u64::from(end as u32),
                    None => 0,
                };
                (u64::from(cfg.hue & 0x00FF_FFFF) << 16
                    | 1 << 63
                    | u64::from(cfg.light_vivid) << 62
                    | u64::from(cfg.chosen_hue) << 61)
                    ^ span.rotate_left(37)
            })
            | 1;
        self.fp_prev = self.fp_last;
        self.fp_last = fp;
        MomentumGlowFrame {
            fill,
            hot: true,
            fp,
        }
    }

    #[inline]
    const fn inert(&mut self) -> MomentumGlowFrame {
        self.latched = 0.0;
        self.stamp = None;
        self.fp_prev = self.fp_last;
        self.fp_last = 0;
        MomentumGlowFrame::DEFAULT
    }
}

#[inline]
fn emit_halo(
    (cr, cc): (u16, u16),
    a: f32,
    s: f32,
    tint: u32,
    geom: Geom,
    cfg: &MomentumGlowConfig,
    out: &mut Vec<GlowQuad>,
) {
    let (cw, ch) = (geom.cw as f32, geom.ch as f32);
    let (cx, cy) = cfg.seat(&geom, (cr, cc));
    let rx = cw * (0.55 + cfg.radius_cells * s);
    let ry = ch * (0.45 + 0.6 * cfg.radius_cells * s);
    let peak = HALO_PEAK_COV * a * (2.0 - a);
    if !(rx >= 1.0 && ry >= 1.0 && peak >= 0.5) {
        return;
    }
    let row_h = ((ch / HALO_ROW_DIV) as i32).max(2);
    let half_row_h = row_h as f32 * 0.5;
    let cell_y0 = i32::from(geom.origin_y) + i32::from(cr) * geom.ch as i32;
    let top = (cy - ry).floor() as i32;
    let bot = (cy + ry).ceil() as i32;
    let inv_ry = 1.0 / ry;
    let mut y = cell_y0 - (cell_y0 - top + row_h - 1).div_euclid(row_h) * row_h;
    while y < bot {
        let y_center = y as f32 + half_row_h;
        let v = (y_center - cy) * inv_ry;
        let v2 = v * v;
        let w2 = 1.0 - v2;
        let fade_y = crate::effect_util::fx_fade_y(geom, y_center);
        if w2 > 0.0 && fade_y > 0.0 {
            let half = rx * crate::color_math::sqrt_f32(w2);
            let slice = (cx, half, v2, w2, peak, fade_y);
            let mut prev = halo_knot(0, slice, (tint, a), geom);
            let mut k = 1;
            while k <= HALO_SEGMENTS {
                let next = halo_knot(k, slice, (tint, a), geom);
                k += 1;
                let (r0, l1, r_in, right_in, l_in, left_in) = prev;
                let (r1, l0, r_out, right_out, l_out, left_out) = next;
                prev = next;
                if r_in == 0 && r_out == 0 && l_in == 0 && l_out == 0 {
                    break;
                }
                if r_in > 0 || r_out > 0 {
                    push_fx_ramp(out, geom, r0, y, r1 - r0, row_h, right_in, right_out);
                }
                if l_in > 0 || l_out > 0 {
                    push_fx_ramp(out, geom, l0, y, l1 - l0, row_h, left_out, left_in);
                }
            }
        }
        y += row_h;
    }
}

#[inline(always)]
const fn halo_end(
    geom: Geom,
    x: f32,
    (peak, f, fade_y): (f32, f32, f32),
    colour: u32,
) -> (u8, (u32, u8)) {
    let fade = fade_y * crate::effect_util::fx_fade_x(geom, x);
    let cov = (peak * f * f * fade).round().min(HALO_PEAK_COV) as u8;
    (
        cov,
        (if cov == 0 { 0 } else { premul_rgb(colour, cov) }, 0u8),
    )
}

#[inline(always)]
const fn halo_knot(
    k: usize,
    (cx, half, v2, w2, peak, fade_y): (f32, f32, f32, f32, f32, f32),
    (tint, a): (u32, f32),
    geom: Geom,
) -> (i32, i32, u8, (u32, u8), u8, (u32, u8)) {
    const INV_HALO_SEGMENTS: f32 = 1.0 / HALO_SEGMENTS as f32;
    let t = k as f32 * INV_HALO_SEGMENTS;
    let dx = half * t;
    let (rx_i, lx_i) = ((cx + dx).round() as i32, (cx - dx).round() as i32);
    let rho2 = v2 + w2 * t * t;
    let f = (1.0 - rho2).max(0.0);
    if peak * f * f * fade_y < 0.5 {
        return (rx_i, lx_i, 0, (0, 0), 0, (0, 0));
    }
    let colour = halo_colour(tint, a, crate::color_math::sqrt_f32(rho2));
    let env = (peak, f, fade_y);
    if k == 0 {
        let (cov, end) = halo_end(geom, cx, env, colour);
        return (rx_i, lx_i, cov, end, cov, end);
    }
    let ((cov_r, right), (cov_l, left)) = (
        halo_end(geom, cx + dx, env, colour),
        halo_end(geom, cx - dx, env, colour),
    );
    (rx_i, lx_i, cov_r, right, cov_l, left)
}

/// How much of a fire's ember a dark page's typing halo takes ([`warm_halo`]).
pub const MOMENTUM_FIRE_WARMTH: f32 = 0.8;

/// The ember a fire's dark typing halo is warmed toward ([`warm_halo`]): a deep orange-red (hue
/// 20 degrees), since the rod's amber reads tan and khaki round the caret.
pub const MOMENTUM_FIRE_EMBER: u32 = 0x00E0_4A00;

/// Warm a dark page's additive halo (`quads`) toward `hue` by `share`.
#[inline]
pub const fn warm_halo(quads: &mut [GlowQuad], hue: u32, share: f32) {
    let share = share.clamp(0.0, 1.0);
    let hue = hue & 0x00FF_FFFF;
    let r_h = (hue >> 16) & 0xff;
    let g_h = (hue >> 8) & 0xff;
    let b_h = hue & 0xff;
    let mut top = r_h;
    if g_h > top {
        top = g_h;
    }
    if b_h > top {
        top = b_h;
    }
    if top == 0 || share <= 0.0 {
        return;
    }
    let inv_top = 1.0 / (top as f32);
    let hue_scale = [
        r_h as f32 * inv_top,
        g_h as f32 * inv_top,
        b_h as f32 * inv_top,
    ];
    let mut i = 0;
    while i < quads.len() {
        let q = &mut quads[i];
        if q.alpha == 0 {
            let c1 = q.color;
            let w1 = warm_premul_rgb(c1, hue_scale, share);
            q.color = w1;
            q.color2 = if q.color2 == c1 {
                w1
            } else {
                warm_premul_rgb(q.color2, hue_scale, share)
            };
        }
        i += 1;
    }
}

#[inline]
const fn warm_premul_rgb(c: u32, hue_scale: [f32; 3], share: f32) -> u32 {
    let r_c = (c >> 16) & 0xff;
    let g_c = (c >> 8) & 0xff;
    let b_c = c & 0xff;
    let mut m = r_c;
    if g_c > m {
        m = g_c;
    }
    if b_c > m {
        m = b_c;
    }
    if m == 0 {
        return c;
    }
    let m_f = m as f32;
    let r_own = r_c as f32;
    let r_lit = hue_scale[0] * m_f;
    let r_w = (r_own + (r_lit - r_own) * share).round() as u32;
    let r_out = if r_w < m { r_w } else { m };

    let g_own = g_c as f32;
    let g_lit = hue_scale[1] * m_f;
    let g_w = (g_own + (g_lit - g_own) * share).round() as u32;
    let g_out = if g_w < m { g_w } else { m };

    let b_own = b_c as f32;
    let b_lit = hue_scale[2] * m_f;
    let b_w = (b_own + (b_lit - b_own) * share).round() as u32;
    let b_out = if b_w < m { b_w } else { m };

    (c & 0xFF00_0000) | (r_out << 16) | (g_out << 8) | b_out
}

/// The halo's colour at normalised radius `rho`: the tint mixed with the ramp
/// up to [`HALO_WARMTH`] hotter toward the core.
#[inline]
const fn halo_colour(tint: u32, a: f32, rho: f32) -> u32 {
    const WARM_SCALE: f32 = HALO_WARMTH / (1.0 - HALO_WARM_CORE);
    let warm = ((1.0 - rho) * WARM_SCALE).clamp(0.0, HALO_WARMTH);
    mix_rgb(tint, ramp((a + warm).min(1.0), true), 0.5)
}

/// The light glow's ink for `hue`: walked toward white until, laid opaque on the page, the
/// theme's text keeps [`LIGHT_GLOW_INK_BAR`] over it; or, `vivid` (Sparkle's paper gold), its
/// vivid ink lifted to the same floor, so a gold glow stays gold rather than greying to khaki.
///
/// The vivid ink is kept only while the glow, stacked at any of its opacities on any light body
/// of the spectrum, still leaves the theme's text [`crate::cursor_glow::BODY_CONTRAST_BAR`]; past
/// that it is mixed back toward the walked ink, which always keeps it.
#[inline]
const fn light_glow_ink(hue: u32, theme_fg: u32, theme_bg: u32, vivid: bool) -> u32 {
    use crate::cursor_glow::{
        BODY_CONTRAST_BAR, LIGHT_BODY_ALPHA_CAP, light_body_ink, light_floor_at,
        light_ink_over_floor, light_ink_vivid_over_floor,
    };
    let (fg, bg) = (theme_fg & 0x00FF_FFFF, theme_bg & 0x00FF_FFFF);
    let floor = light_floor_at(fg, LIGHT_GLOW_INK_BAR);
    let walked = light_ink_over_floor(hue & 0x00FF_FFFF, bg, u8::MAX, floor);
    if !vivid {
        return walked;
    }
    let vivid = light_ink_vivid_over_floor(hue & 0x00FF_FFFF, bg, u8::MAX, floor);
    if vivid == walked {
        return walked;
    }
    const SPECTRUM_BODIES: [u32; 36] = {
        let mut out = [0u32; 36];
        let mut k = 0;
        while k < 36 {
            out[k] = crate::color_math::hsv2rgb(k as f32 * 10.0, 1.0, 1.0);
            k += 1;
        }
        out
    };
    const STACK_ALPHAS: [u8; 17] = {
        let mut out = [0u8; 17];
        let mut k = 1;
        while k <= 17 {
            out[k - 1] = (LIGHT_GLOW_PEAK * k as f32 / 17.0).min(LIGHT_GLOW_PEAK) as u8;
            k += 1;
        }
        out
    };
    #[inline]
    const fn lay(ground: u32, rgb: u32, alpha: f32) -> u32 {
        let alpha = alpha as u8;
        aterm_render::over_premul(ground, premul_rgb(rgb, alpha), alpha) & 0x00FF_FFFF
    }
    let mut bodies = [0u32; 36];
    let mut k = 0;
    while k < 36 {
        bodies[k] = lay(
            bg,
            light_body_ink(SPECTRUM_BODIES[k], fg, bg, LIGHT_BODY_ALPHA_CAP as u8),
            LIGHT_BODY_ALPHA_CAP,
        );
        k += 1;
    }
    #[inline]
    const fn stacks(
        table: &crate::color_math::RelativeLuminanceTable,
        bodies: &[u32; 36],
        l_fg: f32,
        ink: u32,
    ) -> bool {
        const BAR: f32 = BODY_CONTRAST_BAR + LIGHT_GLOW_STACK_GUARD;
        let mut lays = [(0u32, 0u8); 17];
        let mut k = 0;
        while k < 17 {
            let a = STACK_ALPHAS[k];
            lays[k] = (premul_rgb(ink, a), a);
            k += 1;
        }
        let mut i = 0;
        while i < 36 {
            let under = bodies[i];
            let mut k = 17;
            while k > 0 {
                k -= 1;
                let (premul, a) = lays[k];
                let comp = aterm_render::over_premul(under, premul, a) & 0x00FF_FFFF;
                let l_bg = crate::color_math::relative_luminance_with(table, comp);
                if (l_fg.max(l_bg) + 0.05) < BAR * (l_fg.min(l_bg) + 0.05) {
                    return false;
                }
            }
            i += 1;
        }
        true
    }
    let table = crate::color_math::relative_luminance_table();
    let l_fg = crate::color_math::relative_luminance_with(table, fg);
    if stacks(table, &bodies, l_fg, vivid) {
        return vivid;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (walked, vivid);
    let mut step = 0;
    while step < 10 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = mix_rgb(walked, vivid, mid);
        let passes = if mid_rgb == lo_rgb {
            true
        } else if mid_rgb == hi_rgb {
            false
        } else {
            stacks(table, &bodies, l_fg, mid_rgb)
        };
        if passes {
            lo = mid;
            lo_rgb = mid_rgb;
        } else {
            hi = mid;
            hi_rgb = mid_rgb;
        }
        step += 1;
    }
    lo_rgb
}

/// The light glow's ink for a colour the user CHOSE ([`MomentumGlowConfig::chosen_hue`]): the
/// colour itself, walked toward white only as far as laid opaque on the page it leaves the
/// theme's text [`LIGHT_GLOW_INK_BAR`] over it. Every channel's distance from white scales down
/// together, so its hue stays the colour's and its chroma only falls. [`light_glow_ink`] starts
/// from its hue's vivid ink instead: beside Sparkle pinned to `#b0b0c8` (chroma 0.094) the
/// paper halo was laid up to `#c1c1ee`, chroma 0.176 (review9fix round 2's judge).
#[inline]
const fn chosen_glow_ink(chosen: u32, theme_fg: u32, theme_bg: u32) -> u32 {
    use crate::cursor_glow::{ink_over_floor_from, light_floor_at};
    let floor = light_floor_at(theme_fg & 0x00FF_FFFF, LIGHT_GLOW_INK_BAR);
    ink_over_floor_from(chosen & 0x00FF_FFFF, theme_bg & 0x00FF_FFFF, u8::MAX, floor)
}

/// A block caret's charge at `s` `0..=1` where its colour is one the user CHOSE
/// ([`MomentumGlowConfig::chosen_base`]): on a dark page lifted toward white, as far as the
/// momentum tint's mix reaches, rather than mixed toward the tint's amber; on a light page
/// darkened in its own hue ([`crate::color_math::light_chosen_fill`]) rather than concentrated.
/// Either way its channels move together, so its hue stays the colour's and its chroma never
/// rises.
#[inline]
const fn chosen_charge(chosen: u32, s: f32, dark_theme: bool) -> u32 {
    if dark_theme {
        mix_rgb(chosen & 0x00FF_FFFF, 0x00FF_FFFF, 0.55 * s)
    } else {
        crate::color_math::light_chosen_fill(chosen, s)
    }
}

/// The margin past the body bar [`light_glow_ink`]'s vivid lift keeps when stacked, for the
/// renderer's rounding.
const LIGHT_GLOW_STACK_GUARD: f32 = 0.02;

/// The share of the theme's text contrast on its paper a light glow keeps where it lies under a
/// letter of the caret's row ([`MomentumGlowConfig::glyph_span`]).
pub const LIGHT_GLOW_TEXT_KEEP: f32 = 0.97;

/// The opacity `0..=255` at which the light glow's `ink` leaves the theme's
/// text [`LIGHT_GLOW_TEXT_KEEP`] of its contrast on its paper, composited as
/// the renderer composites a source-over quad.
#[inline]
const fn light_glow_text_alpha(ink: u32, theme_fg: u32, theme_bg: u32) -> f32 {
    crate::cursor_glow::light_alpha_keeping(ink, theme_fg, theme_bg, LIGHT_GLOW_TEXT_KEEP)
}

const LIGHT_GLOW_INK_BAR: f32 = 7.5;

#[inline]
fn emit_light_halo(
    (cr, cc): (u16, u16),
    a: f32,
    s: f32,
    (ink, text_alpha): (u32, f32),
    geom: Geom,
    cfg: &MomentumGlowConfig,
    under: &mut Vec<GlowQuad>,
) {
    use crate::cursor_glow::{LIGHT_MARK_QUADS, push_ink_ellipse_shaped};
    let (cw, ch) = (geom.cw as f32, geom.ch as f32);
    let (cx, cy) = cfg.seat(&geom, (cr, cc));
    let rx_full = cw * (0.55 + cfg.radius_cells * s);
    let ry_full = ch * (0.45 + 0.6 * cfg.radius_cells * s);
    let peak = LIGHT_GLOW_PEAK * a;
    let budget = under.len().saturating_add(LIGHT_MARK_QUADS);
    let on_letters = (text_alpha / peak.max(1.0)).min(1.0);
    let ox = f32::from(geom.origin_x);
    let caret = i32::from(cc);
    let (letters, caret_cell) = if let Some((first, end)) = cfg.glyph_span {
        let caret_cell = if cfg.block && caret >= first && caret < end {
            let c0 = ox + caret as f32 * cw;
            Some((c0, c0 + cw))
        } else {
            None
        };
        (
            Some((ox + first as f32 * cw, ox + end as f32 * cw)),
            caret_cell,
        )
    } else {
        (None, None)
    };
    #[inline]
    const fn light_halo_share(
        letters: Option<(f32, f32)>,
        caret_cell: Option<(f32, f32)>,
        on_letters: f32,
        x0: f32,
        x1: f32,
    ) -> f32 {
        match letters {
            Some((l0, l1)) if x1 > l0 && x0 < l1 => {
                // A sample whose reach lies inside the covered caret cell alone
                // keeps the whole glow.
                if matches!(caret_cell, Some((c0, c1)) if x0 >= c0 && x1 <= c1) {
                    1.0
                } else {
                    on_letters
                }
            }
            _ => 1.0,
        }
    }
    push_ink_ellipse_shaped(
        under,
        geom,
        (cx, cy),
        (rx_full, rx_full),
        ry_full,
        ink,
        peak,
        budget,
        |x0, x1| light_halo_share(letters, caret_cell, on_letters, x0, x1),
    );
}

/// Colour temperature with momentum: amber → gold → warm white, then white-hot on a dark theme or
/// deep orange on a light one.
#[inline]
#[must_use]
pub const fn ramp(a: f32, dark_theme: bool) -> u32 {
    const AMBER: u32 = 0x00FF_9A3C;
    const GOLD: u32 = 0x00FF_C46B;
    const WARM_WHITE: u32 = 0x00FF_E8C8;
    const WHITE: u32 = 0x00FF_FFFF;
    const DEEP_ORANGE: u32 = 0x00E0_5A00;
    let a = a.clamp(0.0, 1.0);
    let top = if dark_theme { WHITE } else { DEEP_ORANGE };
    if a < 0.35 {
        mix_rgb(AMBER, GOLD, a * (1.0 / 0.35))
    } else if a < 0.75 {
        mix_rgb(GOLD, WARM_WHITE, (a - 0.35) * (1.0 / 0.40))
    } else {
        mix_rgb(WARM_WHITE, top, (a - 0.75) * (1.0 / 0.25))
    }
}

#[cfg(test)]
#[path = "cursor_momentum_tests.rs"]
mod tests;
