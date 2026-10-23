// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supernova.

use aterm_render::{DecoBlend, DecoGlyph, GlowQuad, WordDecoration};
use aterm_scene::mix_rgb;

use crate::color_math::hsv2rgb;

/// Roll salt: `mix(gkey ^ birth_seq ^ SUPERNOVA_SALT) % 100 < chance_pct`.
pub const SUPERNOVA_SALT: u64 = 0xF0CC_AC1A_5EED_B00F;

/// The three detonation tiers.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SuperTier {
    /// The everyday bang.
    Flash,
    /// The full supernova (the module's phase timeline).
    #[default]
    Nova,
    /// The mushroom cloud (`crate::nuke`).
    Nuke,
}

/// Tier windows over the high half of the birth draw (`mix` is splitmix64).
pub const TIER_FLASH_END: u64 = 700;
pub const TIER_NOVA_END: u64 = 950;

/// Decode the tier from the high half of the birth draw (base windows).
#[must_use]
#[cfg(test)]
pub const fn tier_of(draw: u64) -> SuperTier {
    tier_for(draw, 1)
}

/// The f-bomb combo.
pub const COMBO_WINDOW_MS: u64 = 30_000;
/// Ring capacity for recent typed-f-bomb instants.
pub const COMBO_CAP: usize = 8;
/// Detonation-chance escalation per combo link past the first, percentage points on top of the configured `chance_pct`.
pub const COMBO_CHANCE_STEP: u8 = 30;
/// The level at which the ladder tops out.
pub const COMBO_EXTREME_LEVEL: u32 = 5;

/// Per-level tier windows `(flash_end, nova_end)` over the high half's `% 1000`
/// bucket, indexed by `min(level, COMBO_EXTREME_LEVEL) - 1` (level 0 uses the
/// base row).
const COMBO_TIER_WINDOWS: [(u64, u64); COMBO_EXTREME_LEVEL as usize] = [
    (TIER_FLASH_END, TIER_NOVA_END),
    (500, 850),
    (250, 700),
    (0, 450),
    (0, 0),
];
const _: () = {
    let mut i = 0;
    while i < COMBO_TIER_WINDOWS.len() {
        let (f, n) = COMBO_TIER_WINDOWS[i];
        assert!(f <= n && n <= 1000, "COMBO_TIER_WINDOWS bucket bounds");
        if i + 1 < COMBO_TIER_WINDOWS.len() {
            let (f_next, n_next) = COMBO_TIER_WINDOWS[i + 1];
            assert!(
                f_next <= f && n_next < n,
                "COMBO_TIER_WINDOWS must lower P(Flash) and raise P(Nuke) each level"
            );
        }
        i += 1;
    }
    assert!(
        COMBO_TIER_WINDOWS[COMBO_EXTREME_LEVEL as usize - 1].0 == 0
            && COMBO_TIER_WINDOWS[COMBO_EXTREME_LEVEL as usize - 1].1 == 0,
        "COMBO_EXTREME_LEVEL must be 100% Nuke"
    );
};

/// Decode the tier from the high half of the birth draw at a combo level.
#[inline]
#[must_use]
pub const fn tier_for(draw: u64, level: u32) -> SuperTier {
    let clamped = if level < 1 {
        1
    } else if level > COMBO_EXTREME_LEVEL {
        COMBO_EXTREME_LEVEL
    } else {
        level
    };
    let row = clamped as usize - 1;
    let (flash_end, nova_end) = COMBO_TIER_WINDOWS[row];
    match (draw >> 32) % 1000 {
        d if d < flash_end => SuperTier::Flash,
        d if d < nova_end => SuperTier::Nova,
        _ => SuperTier::Nuke,
    }
}

/// Escalate the configured chance by combo level.
#[inline]
#[must_use]
pub const fn combo_chance(cfg: u8, level: u32) -> u8 {
    if cfg == 0 {
        return 0;
    }
    let steps = level.saturating_sub(1);
    let steps = if steps > 4 { 4 } else { steps as u8 };
    let boost = COMBO_CHANCE_STEP.saturating_mul(steps);
    let sum = cfg.saturating_add(boost);
    if sum > 100 { 100 } else { sum }
}

/// Total visible window per tier.
#[inline]
#[must_use]
pub const fn total_ms(tier: SuperTier) -> u64 {
    match tier {
        SuperTier::Flash => 1100,
        SuperTier::Nova => SUPER_TOTAL_MS,
        SuperTier::Nuke => crate::nuke::NUKE_TOTAL_MS,
    }
}

/// Phase boundaries, ms.
pub const CHARGE_END_MS: u64 = 350;
pub const DETONATION_END_MS: u64 = 650;
pub const SHOCK_END_MS: u64 = 1600;
pub const DEBRIS_START_MS: u64 = 1200;
/// Total window.
pub const SUPER_TOTAL_MS: u64 = 2400;

/// §3.2 shockwave reach in rows (clamped by [`r_max_for`]).
pub const SUPER_RADIUS_ROWS: f32 = 6.0;

/// The reach clamp in px.
#[inline]
#[must_use]
pub const fn r_max_for(cell_h: i32, grid_h: i32) -> f32 {
    let ch = if cell_h < 1 { 1 } else { cell_h };
    let gh = if grid_h < cell_h { cell_h } else { grid_h };
    (SUPER_RADIUS_ROWS * ch as f32)
        .min((gh as f32) / 2.0)
        .max(1.0)
}

/// Per-supernova quad budget (own `QuadSink` clone under it).
pub const MAX_SUPER_QUADS_PER: usize = 1024;
/// Concurrency: at most one live supernova, window-wide.
pub const MAX_ACTIVE_SUPERNOVAE: usize = 1;
/// Charge motes.
pub const CHARGE_MOTES: usize = 12;
/// Detonation wash rows counted in the closed form (viewport rows clamp).
pub const MAX_WASH_ROWS: usize = 160;
/// The light-theme veil cell cap (inside `MAX_DECORATIONS = 256` with debris headroom).
pub const MAX_VEIL_CELLS: usize = 200;
/// Ring band count (the classic nova's §6.3 construction).
pub const SUPER_RING_BANDS: usize = 30;

/// Readability ceiling for the viewport-scale detonation channels.
pub const MAX_VIEWPORT_OVERLAY: u32 = 64;

/// Closed-form per-frame worst case.
pub const S_MAX_BOUND: usize = 900;

const _: () = {
    assert!(S_MAX_BOUND <= MAX_SUPER_QUADS_PER);
    assert!(MAX_SUPER_QUADS_PER <= crate::nova::MAX_NOVA_QUADS);
    assert!(crate::nova::MAX_ACTIVE_NOVAS * 392 <= crate::nova::MAX_NOVA_QUADS);
    assert!(S_MAX_BOUND <= crate::nova::MAX_NOVA_QUADS);
};

/// Everything the pure emitters need for one supernova, resolved by the host once per frame.
#[derive(Clone, Copy, Debug)]
pub struct SuperEnv {
    /// Grid extent, px.
    pub grid_w: i32,
    pub grid_h: i32,
    /// Cell metrics, px (row advance already ×2 on DECDWL rows).
    pub cell_w: i32,
    pub cell_h: i32,
    /// Detonation center, px (the word span's visual midpoint).
    pub cx: i32,
    pub cy: i32,
    /// Shockwave reach, px ([`r_max_for`]).
    pub r_max: f32,
    /// Word span, cells (the light-theme veil anchors around it).
    pub row: u16,
    pub start_col: u16,
    pub end_col: u16,
    /// Grid cols (veil clamp).
    pub cols: u16,
    /// Theme branch.
    pub light: bool,
    /// Config profanity intensity.
    pub intensity: f32,
    /// The occurrence seed.
    pub seed: u64,
    /// The episode's rainbow base hue, degrees (debris hue-cycles from it).
    pub base_hue: f32,
}

/// Per-shape quad counts of one [`emit_super`] call (budget tests).
#[derive(Default, Clone, Copy, Debug)]
pub struct SuperCounts {
    pub charge: usize,
    pub wash: usize,
    pub crown: usize,
    pub ring: usize,
}

/// A budgeted, grid-clamped, row-band-splitting [`GlowQuad`] sink; this
/// module's own copy of `nova::QuadSink`.
struct QuadSink<'a> {
    out: &'a mut Vec<GlowQuad>,
    grid_w: i32,
    grid_h: i32,
    cell_h: i32,
    budget: usize,
}

impl QuadSink<'_> {
    #[inline]
    fn push(&mut self, x: i32, y: i32, w: i32, h: i32, premul: u32) {
        if w <= 0 || h <= 0 || premul == 0 || self.cell_h <= 0 {
            return;
        }
        let x0 = x.max(0);
        let x1 = (x + w).min(self.grid_w);
        let y0 = y.max(0);
        let y1 = (y + h).min(self.grid_h);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let x0_u = x0 as u16;
        let w_u = (x1 - x0) as u16;
        let mut yy = y0;
        while yy < y1 {
            if self.budget == 0 {
                return;
            }
            let row = yy / self.cell_h;
            let band_end = ((row + 1) * self.cell_h).min(y1);
            self.out.push(GlowQuad {
                row: row as u16,
                x: x0_u,
                y: yy as u16,
                w: w_u,
                h: (band_end - yy) as u16,
                color: premul,
                alpha: 0,
                color2: premul,
                alpha2: 0,
            });
            self.budget -= 1;
            yy = band_end;
        }
    }
}

/// Premultiply a `0x00RRGGBB` color by coverage (single quantization point).
#[inline]
fn premul(color: u32, cov: f32) -> u32 {
    let a = cov.clamp(0.0, 1.0);
    let m = |sh: u32| ((((color >> sh) & 0xff) as f32) * a).round() as u32;
    (m(16) << 16) | (m(8) << 8) | m(0)
}

/// `sin(π·e)` — one rise-and-fall, never periodic (flash-safety shape).
#[inline]
fn rise_fall(e: f32) -> f32 {
    (core::f32::consts::PI * e.clamp(0.0, 1.0)).sin()
}

/// `1 − (1 − q)²` ease-out (the nova shockwave curve).
#[inline]
const fn ease_out(q: f32) -> f32 {
    let u = 1.0 - q.clamp(0.0, 1.0);
    1.0 - u * u
}

/// Warm detonation core tint.
const WASH_CORE: u32 = 0x00FF_EED2;
/// The light-theme veil / dark-crown stamp tone (near-black warm violet — the
/// Singularity's own Over tone family).
const VEIL_TONE: u32 = 0x001A_1022;

/// Emit one supernova frame's additive quads (charge motes / wash / crown / double ring) under `budget`.
pub fn emit_super(
    t_ms: u64,
    env: &SuperEnv,
    budget: usize,
    out: &mut Vec<GlowQuad>,
) -> SuperCounts {
    let out_start = out.len();
    let mut counts = SuperCounts::default();
    let mut sink = QuadSink {
        out,
        grid_w: env.grid_w,
        grid_h: env.grid_h,
        cell_h: env.cell_h,
        budget: budget.min(MAX_SUPER_QUADS_PER),
    };
    if t_ms < CHARGE_END_MS {
        // light themes draw them on the Over deco stream (`emit_super_decos`).
        if !env.light {
            let p = t_ms as f32 / CHARGE_END_MS as f32;
            let n0 = sink.out.len();
            for k in 0..CHARGE_MOTES {
                let (x, y) = charge_mote_pos(env, k, p);
                let sz = (env.cell_h / 5).max(2);
                let hue = (env.base_hue + k as f32 * 30.0).rem_euclid(360.0);
                let tone = hsv2rgb(hue, 0.55, 1.0);
                let cov = env.intensity * (0.25 + 0.75 * p);
                sink.push(x - sz / 2, y - sz / 2, sz, sz, premul(tone, cov));
            }
            counts.charge = sink.out.len() - n0;
        }
    } else if t_ms < DETONATION_END_MS {
        let e = (t_ms - CHARGE_END_MS) as f32 / (DETONATION_END_MS - CHARGE_END_MS) as f32;
        let env_a = rise_fall(e) * env.intensity;
        if !env.light {
            let vrows = env.grid_h / env.cell_h.max(1);
            let rows = vrows.min(MAX_WASH_ROWS as i32);
            let center_row = env.cy / env.cell_h.max(1);
            let r0 = (center_row - rows / 2).clamp(0, (vrows - rows).max(0));
            let n0 = sink.out.len();
            for r in r0..r0 + rows {
                let dr = (r - center_row).unsigned_abs() as f32;
                let falloff = 0.55 + 0.45 / (1.0 + dr * 0.08);
                let cov = 0.9 * env_a * falloff;
                let tone = mix_rgb(WASH_CORE, 0x00FF_FFFF, (1.0 - dr * 0.05).clamp(0.0, 0.6));
                sink.push(0, r * env.cell_h, env.grid_w, env.cell_h, premul(tone, cov));
            }
            counts.wash = sink.out.len() - n0;
            let n0 = sink.out.len();
            emit_crown_quads(&mut sink, env, env_a);
            counts.crown = sink.out.len() - n0;
        }
    } else if t_ms < SHOCK_END_MS {
        let q = (t_ms - DETONATION_END_MS) as f32 / (SHOCK_END_MS - DETONATION_END_MS) as f32;
        let fade = 1.0 - q;
        let n0 = sink.out.len();
        for (ring, rscale) in [(0usize, 1.0f32), (1, 0.72)] {
            let r = env.r_max * ease_out(q) * rscale;
            let thick = 3.0 - ring as f32;
            let hue = (env.base_hue + ring as f32 * 40.0).rem_euclid(360.0);
            let tone = if env.light {
                hsv2rgb(hue, 1.0, 0.72)
            } else {
                mix_rgb(hsv2rgb(hue, 0.7, 1.0), 0x00FF_FFFF, 0.35)
            };
            emit_ring_chords(
                &mut sink,
                env,
                r,
                thick,
                premul(tone, (fade.sqrt() * env.intensity).min(1.0)),
            );
        }
        counts.ring = sink.out.len() - n0;
    }
    bound_additive_overlap(&mut sink.out[out_start..]);
    counts
}

/// Cap aggregate additive light without a framebuffer-sized coverage map.
#[inline]
fn bound_additive_overlap(quads: &mut [GlowQuad]) {
    let peak = peak_additive_channel(quads);
    if peak <= MAX_VIEWPORT_OVERLAY {
        return;
    }
    for q in quads {
        q.color = scale_rgb_floor(q.color, MAX_VIEWPORT_OVERLAY, peak);
        q.color2 = scale_rgb_floor(q.color2, MAX_VIEWPORT_OVERLAY, peak);
    }
}

/// The exact peak aggregate RGB channel over the probe points of `quads`.
fn peak_additive_channel(quads: &[GlowQuad]) -> u32 {
    if quads.is_empty() {
        return 0;
    }
    thread_local! {
        static SCRATCH: std::cell::RefCell<(Vec<GlowQuad>, Vec<GlowQuad>)> =
            const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
    }
    SCRATCH.with_borrow_mut(|(by_row, by_end)| {
        by_row.clear();
        by_row.extend_from_slice(quads);
        by_row.sort_unstable_by_key(|q| q.row);
        let mut peak = 0u32;
        for row in by_row.chunk_by_mut(|a, b| a.row == b.row) {
            if let [q] = row {
                if q.w > 0 && q.h > 0 {
                    let c = q.color;
                    peak = peak
                        .max((c >> 16) & 0xff)
                        .max((c >> 8) & 0xff)
                        .max(c & 0xff);
                }
                continue;
            }
            row.sort_unstable_by_key(|q| q.x);
            by_end.clear();
            by_end.extend_from_slice(row);
            by_end.sort_unstable_by_key(|q| u32::from(q.x) + u32::from(q.w));
            let row = &*row;
            for (yi, y_edge) in row.iter().enumerate() {
                if row[..yi].iter().any(|q| q.y == y_edge.y) {
                    continue;
                }
                let py = u32::from(y_edge.y);
                let mut sum = [0u32; 3];
                let (mut i, mut j) = (0, 0);
                while i < row.len() {
                    let px = u32::from(row[i].x);
                    while j < by_end.len() {
                        let q = &by_end[j];
                        if u32::from(q.x) + u32::from(q.w) > px {
                            break;
                        }
                        if u32::from(q.w) > 0
                            && py >= u32::from(q.y)
                            && py < u32::from(q.y) + u32::from(q.h)
                        {
                            sum[0] -= (q.color >> 16) & 0xff;
                            sum[1] -= (q.color >> 8) & 0xff;
                            sum[2] -= q.color & 0xff;
                        }
                        j += 1;
                    }
                    let mut read = false;
                    while i < row.len() && u32::from(row[i].x) == px {
                        let q = &row[i];
                        if u32::from(q.w) > 0
                            && py >= u32::from(q.y)
                            && py < u32::from(q.y) + u32::from(q.h)
                        {
                            sum[0] += (q.color >> 16) & 0xff;
                            sum[1] += (q.color >> 8) & 0xff;
                            sum[2] += q.color & 0xff;
                            read = true;
                        }
                        i += 1;
                    }
                    if read {
                        peak = peak.max(sum.into_iter().max().unwrap_or(0));
                    }
                }
            }
        }
        peak
    })
}

#[cfg(test)]
fn max_channel(rgb: u32) -> u32 {
    ((rgb >> 16) & 0xff).max((rgb >> 8) & 0xff).max(rgb & 0xff)
}

#[inline]
fn scale_rgb_floor(rgb: u32, numerator: u32, denominator: u32) -> u32 {
    debug_assert!(denominator > 0);
    let scale = |shift: u32| (((rgb >> shift) & 0xff) * numerator / denominator) << shift;
    scale(16) | scale(8) | scale(0)
}

/// Charge mote `k`'s px position at progress `p` (0..1): a spiral converging on
/// the word centre, shared by the dark and light paths.
#[inline]
fn charge_mote_pos(env: &SuperEnv, k: usize, p: f32) -> (i32, i32) {
    let s = crate::genome::mix(env.seed ^ (k as u64).wrapping_mul(0xA24B_AED4_963E_E407));
    let ang = (s % 4096) as f32 / 4096.0 * core::f32::consts::TAU + p * 1.8;
    let r0 = env.r_max * (1.1 + (s >> 13 & 0x3) as f32 * 0.15);
    let r = r0 * (1.0 - p * p);
    (
        env.cx + (ang.cos() * r) as i32,
        env.cy + (ang.sin() * r * 0.6) as i32,
    )
}

/// The 8-point star crown, additive (dark themes): 4 axes (2 orthogonal + 2
/// diagonal), 3 tapered segments per point ⇒ ≤ 8 × 3 + 2 = 26 ≤ 50 quads.
#[inline]
fn emit_crown_quads(sink: &mut QuadSink<'_>, env: &SuperEnv, env_a: f32) {
    let reach = env.r_max;
    let core = premul(0x00FF_FFFF, env_a);
    let ch = env.cell_h;
    sink.push(env.cx - ch, env.cy - ch / 2, 2 * ch, ch, core);
    sink.push(env.cx - ch / 2, env.cy - ch, ch, 2 * ch, core);
    for point in 0..8 {
        let ang = core::f32::consts::TAU * point as f32 / 8.0;
        let (dx, dy) = (ang.cos(), ang.sin() * 0.75);
        for seg in 0..3 {
            let f0 = 0.15 + 0.28 * seg as f32;
            let d = reach * f0;
            let sz = ((1.0 - f0) * ch as f32 * 0.6).max(2.0) as i32;
            let tone = mix_rgb(WASH_CORE, 0x00FF_FFFF, 1.0 - f0);
            sink.push(
                env.cx + (dx * d) as i32 - sz / 2,
                env.cy + (dy * d) as i32 - sz / 2,
                sz,
                sz,
                premul(tone, env_a * (1.0 - 0.55 * f0)),
            );
        }
    }
}

/// One ring as ≤ [`SUPER_RING_BANDS`] row-band chord pairs (annulus geometry:
/// ≤ 2 chords per band).
#[inline]
fn emit_ring_chords(sink: &mut QuadSink<'_>, env: &SuperEnv, r: f32, thick: f32, premul: u32) {
    if r <= 1.0 {
        return;
    }
    let ro = r + 0.5 * thick;
    let ri = (r - 0.5 * thick).max(0.0);
    let ro2 = ro * ro;
    let ri2 = ri * ri;
    let extent = 2.0 * ro;
    let bands = if extent >= 2.0 * SUPER_RING_BANDS as f32 {
        SUPER_RING_BANDS
    } else {
        ((extent / 2.0).floor() as usize).clamp(1, SUPER_RING_BANDS)
    };
    let top = env.cy as f32 - ro;
    for b in 0..bands {
        let y0 = top + extent * b as f32 / bands as f32;
        let y1 = top + extent * (b + 1) as f32 / bands as f32;
        let ym = 0.5 * (y0 + y1) - env.cy as f32;
        let dy = ym / 0.75;
        if dy.abs() >= ro {
            continue;
        }
        let dy2 = dy * dy;
        let xo = (ro2 - dy2).sqrt();
        let xi = if dy.abs() < ri {
            (ri2 - dy2).sqrt()
        } else {
            0.0
        };
        let (y0i, y1i) = (
            y0.round() as i32,
            (y1.round() as i32).max(y0.round() as i32 + 1),
        );
        if xi > 1.0 {
            sink.push(
                env.cx - xo.round() as i32,
                y0i,
                (xo - xi).round().max(1.0) as i32,
                y1i - y0i,
                premul,
            );
            sink.push(
                env.cx + xi.round() as i32,
                y0i,
                (xo - xi).round().max(1.0) as i32,
                y1i - y0i,
                premul,
            );
        } else {
            sink.push(
                env.cx - xo.round() as i32,
                y0i,
                (2.0 * xo).round().max(1.0) as i32,
                y1i - y0i,
                premul,
            );
        }
    }
}

/// Emit one supernova frame's `WordDecoration` stream (Over veil, dark crown and ring fringe on light themes; rainbow debris on both), within `cap`.
pub fn emit_super_decos(t_ms: u64, env: &SuperEnv, out: &mut Vec<WordDecoration>, cap: usize) {
    let out_start = out.len();
    if env.light && t_ms < CHARGE_END_MS {
        let p = t_ms as f32 / CHARGE_END_MS as f32;
        for k in 0..CHARGE_MOTES {
            if out.len() >= cap {
                break;
            }
            let (x, y) = charge_mote_pos(env, k, p);
            if x < 0 || y < 0 || x >= env.grid_w || y >= env.grid_h {
                continue;
            }
            let (row, col) = (y / env.cell_h.max(1), x / env.cell_w.max(1));
            let hue = (env.base_hue + k as f32 * 30.0).rem_euclid(360.0);
            let cov = env.intensity * (0.25 + 0.75 * p);
            out.push(WordDecoration {
                row: row as u16,
                col: col as u16,
                dx: ((x % env.cell_w.max(1)) - env.cell_w / 2).clamp(-6, 6) as i8,
                dy: ((y % env.cell_h.max(1)) - env.cell_h / 2).clamp(-6, 6) as i8,
                glyph: DecoGlyph::Dot,
                blend: DecoBlend::Over,
                color: hsv2rgb(hue, 1.0, 0.62),
                alpha: (255.0 * cov.min(1.0)) as u8,
            });
        }
    }
    if env.light && (CHARGE_END_MS..DETONATION_END_MS).contains(&t_ms) {
        let e = (t_ms - CHARGE_END_MS) as f32 / (DETONATION_END_MS - CHARGE_END_MS) as f32;
        let env_a = rise_fall(e) * env.intensity;
        let rows_span = 7i32;
        let r0 = i32::from(env.row) - rows_span / 2;
        let width = (MAX_VEIL_CELLS / rows_span as usize).min(usize::from(env.cols));
        let word_mid =
            i32::from(env.start_col) + (i32::from(env.end_col) - i32::from(env.start_col)) / 2;
        let c0 = (word_mid - width as i32 / 2)
            .clamp(0, i32::from(env.cols).saturating_sub(width as i32).max(0));
        let mut stamped = 0usize;
        for dr in 0..rows_span {
            if out.len() >= cap || stamped >= MAX_VEIL_CELLS {
                break;
            }
            let r = r0 + dr;
            if r < 0 {
                continue;
            }
            let ny = (dr - rows_span / 2) as f32 / (rows_span as f32 / 2.0);
            let ny2 = ny * ny;
            for dc in 0..width as i32 {
                if out.len() >= cap || stamped >= MAX_VEIL_CELLS {
                    break;
                }
                let nx = (c0 + dc - word_mid) as f32 / (width as f32 / 2.0);
                let d2 = (nx * nx + ny2).min(1.0);
                let a = 235.0 * env_a * (1.0 - d2).powf(0.8);
                if a < 14.0 {
                    continue;
                }
                out.push(WordDecoration {
                    row: r as u16,
                    col: (c0 + dc) as u16,
                    dx: 0,
                    dy: 0,
                    glyph: DecoGlyph::Shade,
                    blend: DecoBlend::Over,
                    color: VEIL_TONE,
                    alpha: a as u8,
                });
                stamped += 1;
            }
        }
        for point in 0..8 {
            if out.len() >= cap {
                break;
            }
            let ang = core::f32::consts::TAU * point as f32 / 8.0;
            let (cos_a, sin_a) = (ang.cos(), ang.sin());
            for seg in 1..=3 {
                if out.len() >= cap {
                    break;
                }
                let d = env.r_max * 0.3 * seg as f32;
                let x = env.cx + (cos_a * d) as i32;
                let y = env.cy + (sin_a * d * 0.75) as i32;
                if x < 0 || y < 0 || x >= env.grid_w || y >= env.grid_h {
                    continue;
                }
                let (row, col) = (y / env.cell_h.max(1), x / env.cell_w.max(1));
                out.push(WordDecoration {
                    row: row as u16,
                    col: col as u16,
                    dx: 0,
                    dy: 0,
                    glyph: DecoGlyph::Shade,
                    blend: DecoBlend::Over,
                    color: VEIL_TONE,
                    alpha: (230.0 * env_a * (1.0 - 0.2 * seg as f32)) as u8,
                });
            }
        }
    }
    if env.light && (DETONATION_END_MS..SHOCK_END_MS).contains(&t_ms) {
        let q = (t_ms - DETONATION_END_MS) as f32 / (SHOCK_END_MS - DETONATION_END_MS) as f32;
        let r = env.r_max * ease_out(q);
        let fade = 1.0 - q;
        let alpha = (170.0 * fade * env.intensity) as u8;
        let n = 16usize;
        for k in 0..n {
            if out.len() >= cap {
                break;
            }
            let ang = core::f32::consts::TAU * k as f32 / n as f32;
            let x = env.cx + (ang.cos() * r) as i32;
            let y = env.cy + (ang.sin() * r * 0.75) as i32;
            if x < 0 || y < 0 || x >= env.grid_w || y >= env.grid_h {
                continue;
            }
            let (row, col) = (y / env.cell_h.max(1), x / env.cell_w.max(1));
            out.push(WordDecoration {
                row: row as u16,
                col: col as u16,
                dx: 0,
                dy: 0,
                glyph: DecoGlyph::Shade,
                blend: DecoBlend::Over,
                color: VEIL_TONE,
                alpha,
            });
        }
    }
    if (DEBRIS_START_MS..SUPER_TOTAL_MS).contains(&t_ms) {
        let n = 24 + (env.seed % 17) as usize;
        let p = (t_ms - DEBRIS_START_MS) as f32 / (SUPER_TOTAL_MS - DEBRIS_START_MS) as f32;
        for k in 0..n {
            if out.len() >= cap {
                break;
            }
            let s = crate::genome::mix(env.seed ^ (k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let ang = (s % 4096) as f32 / 4096.0 * core::f32::consts::TAU;
            let speed = 0.5 + (s >> 13 & 0xff) as f32 / 255.0;
            let r = env.r_max * (0.3 + 1.1 * p) * speed;
            let x = env.cx + (ang.cos() * r) as i32;
            let y = env.cy + (ang.sin() * r * 0.7) as i32 + (p * p * env.cell_h as f32) as i32;
            if x < 0 || y < 0 || x >= env.grid_w || y >= env.grid_h {
                continue;
            }
            let (row, col) = (y / env.cell_h.max(1), x / env.cell_w.max(1));
            let hue = (env.base_hue + k as f32 * (360.0 / n as f32) + p * 90.0).rem_euclid(360.0);
            let tone = if env.light {
                hsv2rgb(hue, 1.0, 0.62)
            } else {
                hsv2rgb(hue, 0.85, 1.0)
            };
            let grid_phase = (t_ms / crate::nova::TWINKLE_GRID_MS + k as u64) % 2;
            let tw = if grid_phase == 0 {
                1.0
            } else {
                crate::effect_util::TWINKLE_FLOOR
            };
            out.push(WordDecoration {
                row: row as u16,
                col: col as u16,
                dx: ((x % env.cell_w.max(1)) - env.cell_w / 2).clamp(-6, 6) as i8,
                dy: ((y % env.cell_h.max(1)) - env.cell_h / 2).clamp(-6, 6) as i8,
                glyph: if s & 1 == 0 {
                    DecoGlyph::Star4
                } else {
                    DecoGlyph::Dot
                },
                blend: if env.light {
                    DecoBlend::Over
                } else {
                    DecoBlend::Add
                },
                color: tone,
                alpha: (255.0 * env.intensity * tw * (1.0 - p * p)) as u8,
            });
        }
    }
    bound_shade_opacity(&mut out[out_start..]);
}

/// The population the eclipse bound governs, by blend and glyph: light-theme
/// charge motes and debris are Over too but must not be darkened with the veil.
#[inline]
const fn is_shade(d: &WordDecoration) -> bool {
    matches!(d.blend, DecoBlend::Over) && matches!(d.glyph, DecoGlyph::Shade)
}

/// Slot count of [`peak_shade_probe`]'s stack table: 2x the emitter's Shade
/// ceiling (≤ 200 veil + 24 crown; the ≤ 16 fringe stamps live in a disjoint
/// window).
const SHADE_TAB_BITS: u32 = 9;
const SHADE_TAB: usize = 1 << SHADE_TAB_BITS;

/// The worst per-`(row, col)` aggregate Shade alpha in one pass: per-cell sums
/// accumulate in a fixed linear-probe table on the stack.
#[inline]
fn peak_shade_probe(decos: &[WordDecoration]) -> u32 {
    const EMPTY: u64 = u64::MAX;
    let mut keys = [EMPTY; SHADE_TAB];
    let mut sums = [0u32; SHADE_TAB];
    let mut peak = 0u32;
    for d in decos.iter().filter(|d| is_shade(d)) {
        debug_assert_eq!((d.dx, d.dy), (0, 0));
        let key = u64::from(d.row) << 16 | u64::from(d.col);
        let mut slot = (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - SHADE_TAB_BITS)) as usize;
        loop {
            if keys[slot] == key {
                sums[slot] += u32::from(d.alpha);
                break;
            }
            if keys[slot] == EMPTY {
                keys[slot] = key;
                sums[slot] = u32::from(d.alpha);
                break;
            }
            slot = (slot + 1) & (SHADE_TAB - 1);
        }
        peak = peak.max(sums[slot]);
    }
    peak
}

/// The quadratic peak scan: the reference [`peak_shade_probe`] is tested
/// against, and the fallback for Shade populations past the probe table's
/// admission (unreachable from `emit_super_decos`, which caps Shade at
/// [`MAX_VEIL_CELLS`] veil + 24 crown + 16 fringe, but kept total).
fn peak_shade_quadratic(decos: &[WordDecoration]) -> u32 {
    decos
        .iter()
        .filter(|d| is_shade(d))
        .map(|d| {
            debug_assert_eq!((d.dx, d.dy), (0, 0));
            decos
                .iter()
                .filter(|other| is_shade(other) && other.row == d.row && other.col == d.col)
                .map(|other| u32::from(other.alpha))
                .sum()
        })
        .max()
        .unwrap_or(0)
}

/// Bound the eclipse's full-cell source-over opacity per host cell.
#[inline]
fn bound_shade_opacity(decos: &mut [WordDecoration]) {
    if decos.is_empty() {
        return;
    }
    let shade_count = decos.iter().filter(|d| is_shade(d)).count();
    if shade_count == 0 {
        return;
    }
    let peak = if shade_count <= SHADE_TAB / 2 {
        peak_shade_probe(decos)
    } else {
        peak_shade_quadratic(decos)
    };
    if peak <= MAX_VIEWPORT_OVERLAY {
        return;
    }
    for d in decos.iter_mut().filter(|d| is_shade(d)) {
        d.alpha = (u32::from(d.alpha) * MAX_VIEWPORT_OVERLAY / peak) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn max_cell_shade_alpha(decos: &[WordDecoration]) -> u32 {
        decos
            .iter()
            .filter(|d| matches!(d.blend, DecoBlend::Over) && matches!(d.glyph, DecoGlyph::Shade))
            .map(|d| {
                decos
                    .iter()
                    .filter(|other| {
                        matches!(other.blend, DecoBlend::Over)
                            && matches!(other.glyph, DecoGlyph::Shade)
                            && other.row == d.row
                            && other.col == d.col
                    })
                    .map(|other| u32::from(other.alpha))
                    .sum()
            })
            .max()
            .unwrap_or(0)
    }

    fn env(ch: i32, light: bool, rows: i32) -> SuperEnv {
        let cw = (ch / 2).max(1);
        let cols = 240i32;
        SuperEnv {
            grid_w: cols * cw,
            grid_h: rows * ch,
            cell_w: cw,
            cell_h: ch,
            cx: cols * cw / 2,
            cy: rows * ch / 2,
            r_max: r_max_for(ch, rows * ch),
            row: (rows / 2) as u16,
            start_col: 118,
            end_col: 121,
            cols: cols as u16,
            light,
            intensity: 1.0,
            seed: 0xDECAF_C0FFEE,
            base_hue: 42.0,
        }
    }

    /// [`r_max_for`] is the engine clamp `min(6 rows, grid_h / 2)` with the
    /// 1 px floor, so tests never use radii the engine cannot produce.
    #[test]
    fn r_max_for_matches_engine_clamp() {
        assert_eq!(r_max_for(20, 64 * 20), 6.0 * 20.0);
        assert_eq!(r_max_for(20, 8 * 20), 4.0 * 20.0);
        assert_eq!(r_max_for(0, 0), 1.0);
    }

    /// Structural budget: `S_max ≤ 900` at `ch ∈ {14, 20, 40, 56}` over every
    /// 10 ms step, both themes, 64 and 160 viewport rows, and the budgeted
    /// emission equals the unbounded one (the cap never binds).
    #[test]
    fn super_budget_s_max_bound() {
        for ch in [14i32, 20, 40, 56] {
            for rows in [64i32, 160] {
                for light in [false, true] {
                    let e = env(ch, light, rows);
                    for t in (0..SUPER_TOTAL_MS).step_by(10) {
                        let mut capped = Vec::new();
                        let counts = emit_super(t, &e, MAX_SUPER_QUADS_PER, &mut capped);
                        let mut free = Vec::new();
                        emit_super(t, &e, usize::MAX, &mut free);
                        assert_eq!(
                            capped, free,
                            "budget bound at t={t} ch={ch} rows={rows} light={light}"
                        );
                        let worst = capped.len() + 2 * counts.wash;
                        assert!(
                            worst <= S_MAX_BOUND,
                            "S_max: {worst} > {S_MAX_BOUND} at t={t} ch={ch} rows={rows} light={light} ({counts:?})"
                        );
                        assert!(capped.len() <= MAX_SUPER_QUADS_PER);
                        assert!(
                            peak_additive_channel(&capped) <= MAX_VIEWPORT_OVERLAY,
                            "aggregate additive bound at t={t} ch={ch} rows={rows} light={light}"
                        );
                        for q in &capped {
                            assert!(i32::from(q.h) <= ch, "band split at t={t}");
                            assert_eq!(i32::from(q.y) / ch, i32::from(q.row));
                        }
                    }
                }
            }
        }
    }

    /// The deco stream stays within the host cap: veil ≤ 200, light charge ≤ 12,
    /// debris ≤ 40, never above 256 in one phase.
    #[test]
    fn super_deco_stream_bounded() {
        for light in [false, true] {
            let e = env(20, light, 64);
            for t in (0..SUPER_TOTAL_MS).step_by(10) {
                let mut out = Vec::new();
                emit_super_decos(t, &e, &mut out, 256);
                assert!(out.len() <= 256, "t={t} light={light}: {}", out.len());
                assert!(
                    max_cell_shade_alpha(&out) <= MAX_VIEWPORT_OVERLAY,
                    "aggregate Shade opacity at t={t} light={light}"
                );
                if light && t < CHARGE_END_MS {
                    assert!(
                        out.len() <= CHARGE_MOTES,
                        "light charge ≤ {CHARGE_MOTES} decos, got {} at t={t}",
                        out.len()
                    );
                }
                if light && (CHARGE_END_MS..DETONATION_END_MS).contains(&t) {
                    assert!(
                        out.iter()
                            .filter(|d| matches!(d.blend, DecoBlend::Over))
                            .count()
                            <= MAX_VEIL_CELLS + 24,
                        "veil cap"
                    );
                }
                for d in &out {
                    if !light {
                        assert!(
                            matches!(d.blend, DecoBlend::Add),
                            "dark decos stay additive, got {:?} at t={t}",
                            d.blend
                        );
                    }
                    assert_eq!(
                        d.color == VEIL_TONE,
                        matches!(d.glyph, DecoGlyph::Shade),
                        "veil-tone ⇔ Shade (§3.3), got {:?}/{:06x} at t={t}",
                        d.glyph,
                        d.color
                    );
                    if matches!(d.glyph, DecoGlyph::Shade) {
                        assert!(
                            matches!(d.blend, DecoBlend::Over),
                            "Shade stamps blend Over at t={t}"
                        );
                    }
                }
            }
        }
    }

    /// Theme branch: a dark-bg detonation uses the bounded additive wash,
    /// a light-bg one rides the Over deco stream (the eclipse) instead.
    #[test]
    fn detonation_is_theme_branched() {
        let t = 500u64;
        let (dark, light) = (env(20, false, 64), env(20, true, 64));
        let mut dq = Vec::new();
        let dc = emit_super(t, &dark, MAX_SUPER_QUADS_PER, &mut dq);
        assert!(dc.wash > 0 && dc.crown > 0, "dark: wash + crown ({dc:?})");
        let mut lq = Vec::new();
        let lc = emit_super(t, &light, MAX_SUPER_QUADS_PER, &mut lq);
        assert_eq!(lc.wash, 0, "light: additive wash is INVISIBLE on white");
        let mut ld = Vec::new();
        emit_super_decos(t, &light, &mut ld, 256);
        let veil = ld
            .iter()
            .filter(|d| matches!(d.blend, DecoBlend::Over))
            .count();
        assert!(
            veil > 100,
            "light: the Over-blend dark veil ({veil} stamps)"
        );
        let mut dd = Vec::new();
        emit_super_decos(t, &dark, &mut dd, 256);
        assert!(dd.is_empty(), "dark detonation emits no veil");
    }

    /// A rolled supernova must not whiten the frame: at the phase peak both
    /// theme branches stay within the overlay bound, with negative controls
    /// showing each limiter binds.
    #[test]
    fn detonation_has_a_hard_aggregate_readability_ceiling() {
        let t = 500;

        let raw_wash = max_channel(premul(WASH_CORE, 0.9));
        assert!(
            raw_wash > MAX_VIEWPORT_OVERLAY,
            "negative control must exceed the new ceiling: {raw_wash}"
        );
        let dark = env(20, false, 64);
        let mut quads = Vec::new();
        let counts = emit_super(t, &dark, MAX_SUPER_QUADS_PER, &mut quads);
        assert!(counts.wash > 0 && counts.crown > 0, "real peak emitted");
        let bounded_peak = peak_additive_channel(&quads);
        assert!(
            (MAX_VIEWPORT_OVERLAY / 2..=MAX_VIEWPORT_OVERLAY).contains(&bounded_peak),
            "the peak stays vivid but cannot cross the aggregate light ceiling: {bounded_peak}"
        );

        let add_bound = |rgb: u32| {
            let add =
                |shift: u32| (((rgb >> shift) & 0xff) + MAX_VIEWPORT_OVERLAY).min(255) << shift;
            add(16) | add(8) | add(0)
        };
        let theme = aterm_render::Theme::default();
        let lit_fg = crate::color_math::relative_luminance(add_bound(theme.fg));
        let lit_bg = crate::color_math::relative_luminance(add_bound(theme.bg));
        let contrast = (lit_fg + 0.05) / (lit_bg + 0.05);
        assert!(contrast >= 4.5, "default-theme contrast fell to {contrast}");

        let raw_shade_alpha = 235u32;
        assert!(raw_shade_alpha > MAX_VIEWPORT_OVERLAY);
        let light = env(20, true, 64);
        let mut decos = Vec::new();
        emit_super_decos(t, &light, &mut decos, 256);
        assert!(
            decos.iter().any(|d| matches!(d.glyph, DecoGlyph::Shade)),
            "real eclipse emitted"
        );
        let bounded_alpha = max_cell_shade_alpha(&decos);
        assert!(
            (MAX_VIEWPORT_OVERLAY / 2..=MAX_VIEWPORT_OVERLAY).contains(&bounded_alpha),
            "the eclipse stays visible but cannot cross the aggregate opacity ceiling: {bounded_alpha}"
        );

        let mut debris = Vec::new();
        emit_super_decos(2000, &light, &mut debris, 256);
        assert!(
            debris.iter().any(|d| {
                !matches!(d.glyph, DecoGlyph::Shade)
                    && matches!(d.blend, DecoBlend::Over)
                    && u32::from(d.alpha) > MAX_VIEWPORT_OVERLAY
            }),
            "small rainbow debris stays vivid"
        );
    }

    /// The probe-table peak equals [`peak_shade_quadratic`] over the deployed
    /// emission space (every 10 ms × both themes × deployed cell heights × both
    /// row counts) and over hand-built edge cases: crown-on-veil duplicates,
    /// Over non-Shade and Add-blend Shade impostors, an alpha-zero stamp, the
    /// `(u16::MAX, u16::MAX)` key beside the empty sentinel, and a population
    /// past `SHADE_TAB / 2` that must take the fallback.
    #[test]
    fn shade_peak_probe_is_bit_identical_to_quadratic_reference() {
        fn reference_bound(decos: &mut [WordDecoration]) {
            let peak = peak_shade_quadratic(decos);
            if peak <= MAX_VIEWPORT_OVERLAY {
                return;
            }
            for d in decos.iter_mut().filter(|d| is_shade(d)) {
                d.alpha = (u32::from(d.alpha) * MAX_VIEWPORT_OVERLAY / peak) as u8;
            }
        }
        let check = |label: &str, stream: &[WordDecoration]| {
            assert_eq!(
                peak_shade_probe(stream),
                peak_shade_quadratic(stream),
                "peak diverged: {label}"
            );
            let mut fast = stream.to_vec();
            let mut slow = stream.to_vec();
            bound_shade_opacity(&mut fast);
            reference_bound(&mut slow);
            assert_eq!(fast, slow, "bounded bytes diverged: {label}");
        };

        for ch in [14i32, 20, 40, 56] {
            for rows in [64i32, 160] {
                for light in [false, true] {
                    let e = env(ch, light, rows);
                    for t in (0..SUPER_TOTAL_MS).step_by(10) {
                        let mut out = Vec::new();
                        emit_super_decos(t, &e, &mut out, 256);
                        check(
                            &format!("emitted t={t} ch={ch} rows={rows} light={light}"),
                            &out,
                        );
                    }
                }
            }
        }

        let stamp =
            |row: u16, col: u16, glyph: DecoGlyph, blend: DecoBlend, alpha: u8| WordDecoration {
                row,
                col,
                dx: 0,
                dy: 0,
                glyph,
                blend,
                color: VEIL_TONE,
                alpha,
            };
        let mut veil = Vec::new();
        for r in 0..7u16 {
            for c in 0..28u16 {
                veil.push(stamp(
                    30 + r,
                    40 + c,
                    DecoGlyph::Shade,
                    DecoBlend::Over,
                    200,
                ));
            }
        }
        for k in 0..24u16 {
            veil.push(stamp(33, 40 + k, DecoGlyph::Shade, DecoBlend::Over, 180));
        }
        veil.push(stamp(33, 44, DecoGlyph::Dot, DecoBlend::Over, 255));
        veil.push(stamp(33, 44, DecoGlyph::Shade, DecoBlend::Add, 255));
        veil.push(stamp(0, 0, DecoGlyph::Shade, DecoBlend::Over, 0));
        veil.push(stamp(
            u16::MAX,
            u16::MAX,
            DecoGlyph::Shade,
            DecoBlend::Over,
            7,
        ));
        assert!(
            peak_shade_quadratic(&veil) > MAX_VIEWPORT_OVERLAY,
            "the synthetic eclipse must exceed the cap, or the scale-arm \
             comparison below is two no-ops agreeing"
        );
        check("synthetic pre-bound eclipse", &veil);

        let mut big = Vec::new();
        for r in 0..24u16 {
            for c in 0..24u16 {
                big.push(stamp(r, c, DecoGlyph::Shade, DecoBlend::Over, 100));
            }
        }
        big.push(stamp(0, 0, DecoGlyph::Shade, DecoBlend::Over, 100));
        let mut fast = big.clone();
        let mut slow = big;
        bound_shade_opacity(&mut fast);
        reference_bound(&mut slow);
        assert_eq!(fast, slow, "fallback-path bytes diverged");
    }

    /// The limiter is one frame-wide scale, not per-row normalization, so
    /// falloff ratios survive.
    #[test]
    fn overlap_bound_preserves_falloff_and_finds_crossed_edges() {
        let quad = |row, x, y, w, h, color| GlowQuad {
            row,
            x,
            y,
            w,
            h,
            color,
            alpha: 0,
            color2: color,
            alpha2: 0,
        };
        let mut gradient = vec![
            quad(0, 0, 0, 20, 10, 0x00C8_C8C8),
            quad(1, 0, 10, 20, 10, 0x0064_6464),
        ];
        bound_additive_overlap(&mut gradient);
        assert_eq!(max_channel(gradient[0].color), MAX_VIEWPORT_OVERLAY);
        assert_eq!(max_channel(gradient[1].color), MAX_VIEWPORT_OVERLAY / 2);

        let mut crossed = vec![
            quad(0, 0, 5, 10, 10, 0x00FF_0000),
            quad(0, 5, 0, 10, 10, 0x00FF_0000),
        ];
        assert_eq!(
            peak_additive_channel(&crossed),
            510,
            "negative control: crossed rectangles really overlap"
        );
        bound_additive_overlap(&mut crossed);
        assert!(peak_additive_channel(&crossed) <= MAX_VIEWPORT_OVERLAY);
        assert!(
            crossed.iter().all(|q| q.color != 0),
            "both crossed shapes remain visible"
        );
    }

    /// The edge sweep equals the exhaustive probe over the deployed emission
    /// space (every 10 ms × both themes × {64, 160} rows × deployed cell
    /// heights) and over hand-built edge cases: half-open retirement, shared
    /// start edges, duplicates, zero-extent quads, and a hottest point that is
    /// neither quad's corner.
    #[test]
    fn peak_sweep_is_bit_identical_to_exhaustive_probe() {
        fn exhaustive(quads: &[GlowQuad]) -> u32 {
            let mut by_row: Vec<&GlowQuad> = quads.iter().collect();
            by_row.sort_unstable_by_key(|q| q.row);
            let mut peak = 0u32;
            for row in by_row.chunk_by(|a, b| a.row == b.row) {
                for x_edge in row {
                    let px = u32::from(x_edge.x);
                    for y_edge in row {
                        let py = u32::from(y_edge.y);
                        let mut sum = [0u32; 3];
                        for q in row {
                            if px >= u32::from(q.x)
                                && px < u32::from(q.x) + u32::from(q.w)
                                && py >= u32::from(q.y)
                                && py < u32::from(q.y) + u32::from(q.h)
                            {
                                sum[0] += (q.color >> 16) & 0xff;
                                sum[1] += (q.color >> 8) & 0xff;
                                sum[2] += q.color & 0xff;
                            }
                        }
                        peak = peak.max(sum.into_iter().max().unwrap_or(0));
                    }
                }
            }
            peak
        }
        for ch in [14i32, 20, 40, 56] {
            for rows in [64i32, 160] {
                for light in [false, true] {
                    let e = env(ch, light, rows);
                    for t in (0..SUPER_TOTAL_MS).step_by(10) {
                        let mut quads = Vec::new();
                        emit_super(t, &e, MAX_SUPER_QUADS_PER, &mut quads);
                        assert_eq!(
                            peak_additive_channel(&quads),
                            exhaustive(&quads),
                            "sweep drifted at t={t} ch={ch} rows={rows} light={light}"
                        );
                    }
                }
            }
        }
        let quad = |row, x, y, w, h, color| GlowQuad {
            row,
            x,
            y,
            w,
            h,
            color,
            alpha: 0,
            color2: color,
            alpha2: 0,
        };
        let shapes: [&[GlowQuad]; 5] = [
            &[],
            &[
                quad(0, 0, 5, 10, 10, 0x00FF_0000),
                quad(0, 5, 0, 10, 10, 0x00FF_0000),
            ],
            &[
                quad(3, 4, 4, 8, 8, 0x0011_2233),
                quad(3, 4, 4, 8, 8, 0x0044_5566),
                quad(3, 4, 8, 2, 2, 0x0077_8899),
            ],
            &[
                quad(1, 2, 2, 0, 4, 0x00FF_FFFF),
                quad(1, 2, 2, 4, 0, 0x00FF_FFFF),
                quad(1, 0, 0, 6, 6, 0x0010_2030),
            ],
            &[
                quad(2, 0, 0, 4, 4, 0x00A0_0000),
                quad(2, 2, 1, 4, 4, 0x0000_B000),
                quad(2, 4, 2, 4, 4, 0x0000_00C0),
                quad(9, 1, 1, 3, 3, 0x0055_5555),
            ],
        ];
        for (i, s) in shapes.into_iter().enumerate() {
            assert_eq!(
                peak_additive_channel(s),
                exhaustive(s),
                "adversarial shape {i}"
            );
        }
    }

    /// On a viewport taller than [`MAX_WASH_ROWS`] the wash window centres on
    /// the blast row (clamped to the grid), so a detonation at the bottom
    /// washes the word's row.
    #[test]
    fn wash_window_centers_on_blast_row() {
        let rows = 200i32;
        let mut e = env(20, false, rows);
        let blast_row = 190i32;
        e.row = blast_row as u16;
        e.cy = blast_row * e.cell_h + e.cell_h / 2;
        let mut q = Vec::new();
        let c = emit_super(500, &e, MAX_SUPER_QUADS_PER, &mut q);
        assert_eq!(c.wash, MAX_WASH_ROWS, "row count unchanged (closed form)");
        let wash_rows: Vec<i32> = q[..c.wash].iter().map(|d| i32::from(d.row)).collect();
        assert!(
            wash_rows.contains(&blast_row),
            "the blast row itself is washed (rows {:?}..{:?})",
            wash_rows.first(),
            wash_rows.last()
        );
        assert!(
            wash_rows.iter().all(|r| (40..rows).contains(r)),
            "window clamps inside the grid"
        );
        let e = env(20, false, 64);
        let mut q = Vec::new();
        let c = emit_super(500, &e, MAX_SUPER_QUADS_PER, &mut q);
        assert_eq!(c.wash, 64);
        assert_eq!(i32::from(q[0].row), 0);
    }

    /// Light themes emit charge motes and debris as `DecoBlend::Over`; the dark
    /// theme keeps its additive output.
    #[test]
    fn light_charge_and_debris_ride_over_stream() {
        let (dark, light) = (env(20, false, 64), env(20, true, 64));
        let mut lq = Vec::new();
        let lc = emit_super(200, &light, MAX_SUPER_QUADS_PER, &mut lq);
        assert_eq!(lc.charge, 0, "light charge leaves the additive channel");
        assert!(lq.is_empty());
        let mut ld = Vec::new();
        emit_super_decos(200, &light, &mut ld, 256);
        assert!(
            !ld.is_empty() && ld.len() <= CHARGE_MOTES,
            "1..=12 charge decos, got {}",
            ld.len()
        );
        let mut dq = Vec::new();
        let dc = emit_super(200, &dark, MAX_SUPER_QUADS_PER, &mut dq);
        assert!(dc.charge > 0, "dark charge stays on the additive channel");
        let mut dd = Vec::new();
        emit_super_decos(200, &dark, &mut dd, 256);
        assert!(dd.is_empty(), "dark charge emits no decos");
        let probe = |e: &SuperEnv| {
            let mut d = Vec::new();
            emit_super_decos(2000, e, &mut d, 256);
            d
        };
        let (ldeb, ddeb) = (probe(&light), probe(&dark));
        assert!(!ldeb.is_empty() && ldeb.len() == ddeb.len());
        assert!(ddeb.iter().all(|m| matches!(m.blend, DecoBlend::Add)));
        for m in ld.iter().chain(&ldeb) {
            assert!(matches!(m.blend, DecoBlend::Over), "light motes blend Over");
        }
    }

    /// The tier decode is independent of the detonate decode: among draws
    /// that detonate at 30%, the tier split is the split over every draw, so
    /// the tiers do not skew with `chance_pct`.
    #[test]
    fn the_tier_decode_is_independent_of_the_detonate_decode() {
        let mut all = [0u32; 3];
        let mut detonated = [0u32; 3];
        for i in 0..200_000u64 {
            let mut x = i ^ SUPERNOVA_SALT;
            x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            let draw = x ^ (x >> 31);
            let tier = tier_of(draw) as usize;
            all[tier] += 1;
            if draw % 100 < 30 {
                detonated[tier] += 1;
            }
        }
        let share =
            |c: &[u32; 3], k: usize| f64::from(c[k]) * 100.0 / f64::from(c.iter().sum::<u32>());
        assert!(
            detonated.iter().sum::<u32>() > 1000,
            "too few detonations to judge"
        );
        for k in 0..3 {
            assert!(
                (share(&all, k) - share(&detonated, k)).abs() < 2.0,
                "tier {k} skews with the detonate decode: {all:?} vs {detonated:?}"
            );
        }
    }

    /// Levels 0 and 1 decode exactly as the base windows (`tier_of`).
    #[test]
    fn combo_baseline_is_byte_identical() {
        for i in 0..200_000u64 {
            let mut x = i ^ SUPERNOVA_SALT;
            x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            let draw = x ^ (x >> 31);
            assert_eq!(tier_of(draw), tier_for(draw, 0));
            assert_eq!(tier_of(draw), tier_for(draw, 1));
        }
    }

    /// The combo ladder is monotone: P(Nuke) rises, P(Flash) falls to zero,
    /// level 5 and above is Nuke on every draw, and the chance tops out at 100
    /// without lifting a configured 0.
    #[test]
    fn combo_ladder_climbs_to_guaranteed_nuke() {
        let n = 1_000u64;
        let mut flash_pct = Vec::new();
        let mut nuke_pct = Vec::new();
        for level in 1..=COMBO_EXTREME_LEVEL {
            let (mut flash, mut nuke) = (0u32, 0u32);
            for bucket in 0..n {
                match tier_for(bucket << 32, level) {
                    SuperTier::Flash => flash += 1,
                    SuperTier::Nova => {}
                    SuperTier::Nuke => nuke += 1,
                }
            }
            flash_pct.push(f64::from(flash) * 100.0 / n as f64);
            nuke_pct.push(f64::from(nuke) * 100.0 / n as f64);
        }
        for w in nuke_pct.windows(2) {
            assert!(w[1] > w[0], "P(Nuke) must climb each level: {nuke_pct:?}");
        }
        for w in flash_pct.windows(2) {
            assert!(w[1] <= w[0], "P(Flash) must never climb: {flash_pct:?}");
        }
        assert!(
            nuke_pct[COMBO_EXTREME_LEVEL as usize - 1] == 100.0,
            "EXTREME level is Nuke on every draw"
        );
        assert_eq!(tier_for(7, COMBO_EXTREME_LEVEL + 3), SuperTier::Nuke);

        assert_eq!(combo_chance(10, 1), 10, "a lone f-bomb keeps its chance");
        assert!(combo_chance(10, 2) > 10);
        assert_eq!(combo_chance(1, COMBO_EXTREME_LEVEL), 100);
        assert_eq!(combo_chance(100, 1), 100);
        for level in 0..=COMBO_EXTREME_LEVEL + 2 {
            assert_eq!(combo_chance(0, level), 0, "0 is the off-switch");
        }
    }

    #[test]
    fn phases_live_in_their_windows() {
        let e = env(20, false, 64);
        let probe = |t: u64| {
            let mut q = Vec::new();
            let c = emit_super(t, &e, MAX_SUPER_QUADS_PER, &mut q);
            let mut d = Vec::new();
            emit_super_decos(t, &e, &mut d, 256);
            (c, q, d)
        };
        let (c, q1, _) = probe(100);
        assert!(c.charge > 0 && c.wash == 0 && c.ring == 0);
        let (_, q2, _) = probe(100);
        assert_eq!(q1, q2, "pure: same inputs, same bytes");
        let (c, ..) = probe(500);
        assert!(c.wash > 0 && c.charge == 0);
        let (c, _, d) = probe(1000);
        assert!(c.ring > 0 && c.wash == 0);
        assert!(d.is_empty(), "debris starts at 1200");
        let (c, _, d) = probe(1500);
        assert!(
            c.ring > 0 && !d.is_empty(),
            "ring/debris overlap 1200..1600"
        );
        let (c, q, d) = probe(2000);
        assert!(c.ring == 0 && q.is_empty() && !d.is_empty());
        let (_, q, d) = probe(SUPER_TOTAL_MS);
        assert!(q.is_empty() && d.is_empty(), "afterglow emits nothing here");
    }
}
