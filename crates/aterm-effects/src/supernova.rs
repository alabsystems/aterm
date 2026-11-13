// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supernova.

use aterm_render::{DecoBlend, DecoGlyph, GlowQuad, WordDecoration};

use crate::color_math::hsv2rgb;
use crate::effect_util::lerp_rgb as mix_rgb;

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
/// Detonation-chance escalation per combo link past the first, in percentage points.
pub const COMBO_CHANCE_STEP: u8 = 30;
/// The level at which the ladder tops out.
pub const COMBO_EXTREME_LEVEL: u32 = 5;

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
        .min((gh as f32) * 0.5)
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
#[derive(Clone, Copy, Debug)]
pub struct SuperCounts {
    pub charge: usize,
    pub wash: usize,
    pub crown: usize,
    pub ring: usize,
}

impl SuperCounts {
    /// Zeroed quad counts before emission.
    pub const EMPTY: Self = Self {
        charge: 0,
        wash: 0,
        crown: 0,
        ring: 0,
    };
}

impl Default for SuperCounts {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

struct QuadSink<'a> {
    out: &'a mut Vec<GlowQuad>,
    grid_w: i32,
    grid_h: i32,
    cell_h: i32,
    budget: usize,
}

impl QuadSink<'_> {
    #[inline]
    const fn intersects(&self, x: i32, y: i32, w: i32, h: i32) -> bool {
        if self.budget == 0 || w <= 0 || h <= 0 || self.cell_h <= 0 {
            return false;
        }
        let x0 = if x > 0 { x } else { 0 };
        let x1 = if x + w < self.grid_w {
            x + w
        } else {
            self.grid_w
        };
        let y0 = if y > 0 { y } else { 0 };
        let y1 = if y + h < self.grid_h {
            y + h
        } else {
            self.grid_h
        };
        x1 > x0 && y1 > y0
    }

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
        let mut row = y0 / self.cell_h;
        let mut yy = y0;
        while yy < y1 {
            if self.budget == 0 {
                return;
            }
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
            row += 1;
        }
    }
}

/// Premultiply a `0x00RRGGBB` color by coverage (single quantization point).
#[inline]
const fn premul(color: u32, cov: f32) -> u32 {
    let a = cov.clamp(0.0, 1.0);
    const fn m(color: u32, a: f32, sh: u32) -> u32 {
        ((((color >> sh) & 0xff) as f32) * a).round() as u32
    }
    (m(color, a, 16) << 16) | (m(color, a, 8) << 8) | m(color, a, 0)
}

/// `sin(π·e)` — one rise-and-fall, never periodic (flash-safety shape).
#[inline]
const fn rise_fall(e: f32) -> f32 {
    sin_cos_f32(core::f32::consts::PI * e.clamp(0.0, 1.0)).0
}

/// `1 − (1 − q)²` ease-out (the nova shockwave curve).
#[inline]
const fn ease_out(q: f32) -> f32 {
    let u = 1.0 - q.clamp(0.0, 1.0);
    1.0 - u * u
}

const WASH_CORE: u32 = 0x00FF_EED2;
const VEIL_TONE: u32 = 0x001A_1022;

const WASH_ROW_TABLE: [(u32, f32); MAX_WASH_ROWS + 1] = {
    let mut table = [(0u32, 0.0f32); MAX_WASH_ROWS + 1];
    let mut i = 0usize;
    while i <= MAX_WASH_ROWS {
        let dr = i as f32;
        let falloff = 0.55 + 0.45 / (1.0 + dr * 0.08);
        let t = (1.0 - dr * 0.05).clamp(0.0, 0.6);
        table[i] = (mix_rgb(WASH_CORE, 0x00FF_FFFF, t), falloff);
        i += 1;
    }
    table
};

const CROWN_SEG_SPECS: [(f32, f32, u32); 3] = {
    let mut table = [(0.0f32, 0.0f32, 0u32); 3];
    let mut seg = 0usize;
    while seg < 3 {
        let f0 = 0.15 + 0.28 * seg as f32;
        let tone = mix_rgb(WASH_CORE, 0x00FF_FFFF, 1.0 - f0);
        table[seg] = (f0, 1.0 - 0.55 * f0, tone);
        seg += 1;
    }
    table
};

const VEIL_DEFAULT_WIDTH: usize = MAX_VEIL_CELLS / 7;
const VEIL_HALF_W: usize = VEIL_DEFAULT_WIDTH / 2;

const VEIL_FALLOFF_28: [[f32; VEIL_HALF_W]; 4] = {
    let mut table = [[0.0f32; VEIL_HALF_W]; 4];
    let mut dy = 0usize;
    while dy < 4 {
        let ny = dy as f32 / 3.5;
        let ny2 = ny * ny;
        let mut dx = 0usize;
        while dx < VEIL_HALF_W {
            let nx = dx as f32 / (VEIL_HALF_W as f32);
            let d2 = nx * nx + ny2;
            if d2 < 1.0 {
                table[dy][dx] = crate::color_math::powf_pos_f32(1.0 - d2, 0.8);
            }
            dx += 1;
        }
        dy += 1;
    }
    table
};

/// Emit one supernova frame's additive quads under `budget`.
#[inline]
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
        // The grid inside the overlay pixel space: the sink narrows its pixels to `u16`.
        grid_w: env.grid_w.min(crate::cursor_glow::OVERLAY_PX_MAX),
        grid_h: env.grid_h.min(crate::cursor_glow::OVERLAY_PX_MAX),
        cell_h: env.cell_h,
        budget: budget.min(MAX_SUPER_QUADS_PER),
    };
    const INV_CHARGE_END_MS: f32 = 1.0 / CHARGE_END_MS as f32;
    const INV_DETONATION_SPAN_MS: f32 = 1.0 / (DETONATION_END_MS - CHARGE_END_MS) as f32;
    const INV_SHOCK_SPAN_MS: f32 = 1.0 / (SHOCK_END_MS - DETONATION_END_MS) as f32;
    if t_ms < CHARGE_END_MS {
        // Charge: 12 motes spiral toward the word centre. Dark themes only;
        // light themes draw them on the Over deco stream (`emit_super_decos`).
        if !env.light {
            let p = t_ms as f32 * INV_CHARGE_END_MS;
            let cov = env.intensity * (0.25 + 0.75 * p);
            if cov > 0.0 {
                let n0 = sink.out.len();
                let sz = (env.cell_h / 5).max(2);
                let half_sz = sz / 2;
                let mut k = 0;
                while k < CHARGE_MOTES {
                    let (x, y) = charge_mote_pos(env, k, p);
                    let (qx, qy) = (x - half_sz, y - half_sz);
                    if sink.intersects(qx, qy, sz, sz) {
                        let hue = env.base_hue + k as f32 * 30.0;
                        let tone = hsv2rgb(hue, 0.55, 1.0);
                        sink.push(qx, qy, sz, sz, premul(tone, cov));
                    }
                    k += 1;
                }
                counts.charge = sink.out.len() - n0;
            }
        }
    } else if t_ms < DETONATION_END_MS {
        let e = (t_ms - CHARGE_END_MS) as f32 * INV_DETONATION_SPAN_MS;
        let env_a = rise_fall(e) * env.intensity;
        if !env.light && env_a > 0.0 {
            let vrows = env.grid_h / env.cell_h.max(1);
            let rows = vrows.min(MAX_WASH_ROWS as i32);
            let center_row = env.cy / env.cell_h.max(1);
            let r0 = (center_row - rows / 2).clamp(0, (vrows - rows).max(0));
            let n0 = sink.out.len();
            let base_wash_cov = 0.9 * env_a;
            let r_end = r0 + rows;
            let mut r = r0;
            while r < r_end {
                let dr_idx = (r - center_row).unsigned_abs() as usize;
                let (tone, falloff) = if dr_idx < WASH_ROW_TABLE.len() {
                    WASH_ROW_TABLE[dr_idx]
                } else {
                    let dr = dr_idx as f32;
                    let falloff = 0.55 + 0.45 / (1.0 + dr * 0.08);
                    (
                        mix_rgb(WASH_CORE, 0x00FF_FFFF, (1.0 - dr * 0.05).clamp(0.0, 0.6)),
                        falloff,
                    )
                };
                let cov = base_wash_cov * falloff;
                sink.push(0, r * env.cell_h, env.grid_w, env.cell_h, premul(tone, cov));
                r += 1;
            }
            counts.wash = sink.out.len() - n0;
            let n0 = sink.out.len();
            emit_crown_quads(&mut sink, env, env_a);
            counts.crown = sink.out.len() - n0;
        }
    } else if t_ms < SHOCK_END_MS {
        let q = (t_ms - DETONATION_END_MS) as f32 * INV_SHOCK_SPAN_MS;
        let fade = 1.0 - q;
        let ring_cov = (crate::color_math::sqrt_f32(fade) * env.intensity).min(1.0);
        let n0 = sink.out.len();
        if ring_cov > 0.0 {
            const RING_SPECS: [(usize, f32); 2] = [(0, 1.0), (1, 0.72)];
            let base_r = env.r_max * ease_out(q);
            let mut ring_idx = 0;
            while ring_idx < RING_SPECS.len() {
                let (ring, rscale) = RING_SPECS[ring_idx];
                ring_idx += 1;
                let r = base_r * rscale;
                let thick = 3.0 - ring as f32;
                let hue = env.base_hue + ring as f32 * 40.0;
                let tone = if env.light {
                    hsv2rgb(hue, 1.0, 0.72)
                } else {
                    mix_rgb(hsv2rgb(hue, 0.7, 1.0), 0x00FF_FFFF, 0.35)
                };
                emit_ring_chords(&mut sink, env, r, thick, premul(tone, ring_cov));
            }
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
    let mut i = 0;
    while i < quads.len() {
        let q = &mut quads[i];
        let c1 = q.color;
        let s1 = scale_rgb_floor(c1, MAX_VIEWPORT_OVERLAY, peak);
        q.color = s1;
        q.color2 = if q.color2 == c1 {
            s1
        } else {
            scale_rgb_floor(q.color2, MAX_VIEWPORT_OVERLAY, peak)
        };
        i += 1;
    }
}

#[inline]
fn peak_additive_channel(quads: &[GlowQuad]) -> u32 {
    if quads.is_empty() {
        return 0;
    }
    if let [q] = quads {
        return if q.w > 0 && q.h > 0 {
            let c = q.color;
            ((c >> 16) & 0xff).max((c >> 8) & 0xff).max(c & 0xff)
        } else {
            0
        };
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
        let mut r0 = 0;
        while r0 < by_row.len() {
            let cur_row = by_row[r0].row;
            let mut r1 = r0 + 1;
            while r1 < by_row.len() && by_row[r1].row == cur_row {
                r1 += 1;
            }
            let row = &mut by_row[r0..r1];
            r0 = r1;
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
            let mut row_sum = [0u32; 3];
            let mut qi = 0;
            while qi < row.len() {
                let q = &row[qi];
                qi += 1;
                if q.w > 0 && q.h > 0 {
                    let c = q.color;
                    row_sum[0] += (c >> 16) & 0xff;
                    row_sum[1] += (c >> 8) & 0xff;
                    row_sum[2] += c & 0xff;
                }
            }
            if row_sum[0].max(row_sum[1]).max(row_sum[2]) <= peak {
                continue;
            }
            row.sort_unstable_by_key(|q| q.x);
            let mut by_end_ready = false;
            let row = &*row;
            let mut yi = 0;
            while yi < row.len() {
                let y_edge = &row[yi];
                if y_edge.w == 0 || y_edge.h == 0 {
                    yi += 1;
                    continue;
                }
                let mut dup = false;
                let mut pi = 0;
                while pi < yi {
                    if row[pi].y == y_edge.y && row[pi].w > 0 && row[pi].h > 0 {
                        dup = true;
                        break;
                    }
                    pi += 1;
                }
                yi += 1;
                if dup {
                    continue;
                }
                let py = u32::from(y_edge.y);
                let mut py_sum = [0u32; 3];
                let mut qi = 0;
                while qi < row.len() {
                    let q = &row[qi];
                    qi += 1;
                    if q.w > 0 && py >= u32::from(q.y) && py < u32::from(q.y) + u32::from(q.h) {
                        py_sum[0] += (q.color >> 16) & 0xff;
                        py_sum[1] += (q.color >> 8) & 0xff;
                        py_sum[2] += q.color & 0xff;
                    }
                }
                if py_sum[0].max(py_sum[1]).max(py_sum[2]) <= peak {
                    continue;
                }
                if !by_end_ready {
                    by_end.clear();
                    by_end.extend_from_slice(row);
                    by_end.sort_unstable_by_key(|q| u32::from(q.x) + u32::from(q.w));
                    by_end_ready = true;
                }
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
                        peak = peak.max(sum[0].max(sum[1]).max(sum[2]));
                    }
                }
            }
        }
        peak
    })
}

#[cfg(test)]
const fn max_channel(rgb: u32) -> u32 {
    let r = (rgb >> 16) & 0xff;
    let g = (rgb >> 8) & 0xff;
    let b = rgb & 0xff;
    let rg = if r > g { r } else { g };
    if rg > b { rg } else { b }
}

#[inline]
const fn scale_rgb_floor(rgb: u32, numerator: u32, denominator: u32) -> u32 {
    debug_assert!(denominator > 0);
    const fn scale(rgb: u32, numerator: u32, denominator: u32, shift: u32) -> u32 {
        (((rgb >> shift) & 0xff) * numerator / denominator) << shift
    }
    scale(rgb, numerator, denominator, 16)
        | scale(rgb, numerator, denominator, 8)
        | scale(rgb, numerator, denominator, 0)
}

/// Charge mote `k`'s px position at progress `p` (0..1): a spiral converging on
/// the word centre, shared by the dark and light paths.
#[inline]
const fn charge_mote_pos(env: &SuperEnv, k: usize, p: f32) -> (i32, i32) {
    const ANG_SCALE: f32 = core::f32::consts::TAU / 4096.0;
    let s = crate::genome::mix(env.seed ^ (k as u64).wrapping_mul(0xA24B_AED4_963E_E407));
    let ang = (s & 4095) as f32 * ANG_SCALE + p * 1.8;
    let r0 = env.r_max * (1.1 + (s >> 13 & 0x3) as f32 * 0.15);
    let r = r0 * (1.0 - p * p);
    let (sin_a, cos_a) = sin_cos_f32(ang);
    (
        env.cx + (cos_a * r) as i32,
        env.cy + (sin_a * r * 0.6) as i32,
    )
}

#[inline]
pub(crate) const fn sin_cos_f32(ang: f32) -> (f32, f32) {
    if !ang.is_finite() {
        return (f32::NAN, f32::NAN);
    }
    const PI_HI: f64 = core::f64::consts::PI;
    const PI_LO: f64 = 1.22464679914735320717376402945839660462569212467758e-16;
    let x = ang as f64;
    let (u, sign) = if x >= -core::f64::consts::FRAC_PI_2 && x <= core::f64::consts::FRAC_PI_2 {
        (x, 1.0_f64)
    } else if x >= 0.0 && x <= 3.0 * core::f64::consts::FRAC_PI_2 {
        ((x - PI_HI) - PI_LO, -1.0_f64)
    } else if x >= 0.0 && x <= core::f64::consts::TAU {
        ((x - 2.0 * PI_HI) - 2.0 * PI_LO, 1.0_f64)
    } else {
        let k = (x * core::f64::consts::FRAC_1_PI).round();
        let ki = k as i64;
        let sign = if (ki & 1) != 0 { -1.0_f64 } else { 1.0_f64 };
        ((x - k * PI_HI) - k * PI_LO, sign)
    };
    let u2 = u * u;
    let mut s_term = u;
    let mut sin_u = u;
    let mut c_term = 1.0_f64;
    let mut cos_u = 1.0_f64;
    let mut n = 1;
    while n <= 10 {
        let k = (2 * n) as f64;
        c_term *= -u2 / ((k - 1.0) * k);
        cos_u += c_term;
        s_term *= -u2 / (k * (k + 1.0));
        sin_u += s_term;
        n += 1;
    }
    ((sign * sin_u) as f32, (sign * cos_u) as f32)
}

const CROWN_DIRS: [(f32, f32); 8] = {
    let mut out = [(0.0f32, 0.0f32); 8];
    let mut point = 0;
    while point < 8 {
        let ang = core::f32::consts::TAU * point as f32 / 8.0;
        out[point] = sin_cos_f32(ang);
        point += 1;
    }
    out
};

const SHOCK_DIRS: [(f32, f32); 16] = {
    let mut out = [(0.0f32, 0.0f32); 16];
    let mut k = 0;
    while k < 16 {
        let ang = core::f32::consts::TAU * k as f32 / 16.0;
        out[k] = sin_cos_f32(ang);
        k += 1;
    }
    out
};

#[inline]
const fn crown_dirs() -> &'static [(f32, f32); 8] {
    &CROWN_DIRS
}

#[inline]
const fn shock_dirs() -> &'static [(f32, f32); 16] {
    &SHOCK_DIRS
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
    let ch_scale = ch as f32 * 0.6;
    let mut segs = [(0.0f32, 0i32, 0i32, 0u32); 3];
    let mut seg = 0;
    while seg < 3 {
        let (f0, cov_scale, tone) = CROWN_SEG_SPECS[seg];
        let d = reach * f0;
        let sz = ((1.0 - f0) * ch_scale).max(2.0) as i32;
        segs[seg] = (d, sz, sz / 2, premul(tone, env_a * cov_scale));
        seg += 1;
    }
    let dirs = crown_dirs();
    let mut di = 0;
    while di < dirs.len() {
        let (sin_a, cos_a) = dirs[di];
        di += 1;
        let (dx, dy) = (cos_a, sin_a * 0.75);
        let mut si = 0;
        while si < segs.len() {
            let (d, sz, half_sz, color) = segs[si];
            si += 1;
            sink.push(
                env.cx + (dx * d) as i32 - half_sz,
                env.cy + (dy * d) as i32 - half_sz,
                sz,
                sz,
                color,
            );
        }
    }
}

/// One ring as ≤ [`SUPER_RING_BANDS`] row-band chord pairs (annulus geometry:
/// ≤ 2 chords per band).
#[inline]
fn emit_ring_chords(sink: &mut QuadSink<'_>, env: &SuperEnv, r: f32, thick: f32, premul: u32) {
    if premul == 0 || r <= 1.0 {
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
        ((extent * 0.5).floor() as usize).clamp(1, SUPER_RING_BANDS)
    };
    const INV_075: f32 = 1.0 / 0.75;
    let inv_bands = 1.0 / bands as f32;
    let cyf = env.cy as f32;
    let top = cyf - ro;
    let mut prev_y1 = top;
    let mut b = 0;
    while b < bands {
        b += 1;
        let y0 = prev_y1;
        let y1 = top + extent * b as f32 * inv_bands;
        prev_y1 = y1;
        let ym = 0.5 * (y0 + y1) - cyf;
        let dy = ym * INV_075;
        if dy.abs() >= ro {
            continue;
        }
        let dy2 = dy * dy;
        let xo = crate::color_math::sqrt_f32(ro2 - dy2);
        let xi = if dy.abs() < ri {
            crate::color_math::sqrt_f32(ri2 - dy2)
        } else {
            0.0
        };
        let y0i = y0.round() as i32;
        let y1i = (y1.round() as i32).max(y0i + 1);
        let h = y1i - y0i;
        let xoi = xo.round() as i32;
        if xi > 1.0 {
            let w = (xo - xi).round().max(1.0) as i32;
            sink.push(env.cx - xoi, y0i, w, h, premul);
            sink.push(env.cx + xi.round() as i32, y0i, w, h, premul);
        } else {
            sink.push(
                env.cx - xoi,
                y0i,
                (2.0 * xo).round().max(1.0) as i32,
                h,
                premul,
            );
        }
    }
}

/// Emit one supernova frame's `WordDecoration` stream within `cap`.
#[inline]
pub fn emit_super_decos(t_ms: u64, env: &SuperEnv, out: &mut Vec<WordDecoration>, cap: usize) {
    const INV_CHARGE_END_MS: f32 = 1.0 / CHARGE_END_MS as f32;
    const INV_DETONATION_SPAN_MS: f32 = 1.0 / (DETONATION_END_MS - CHARGE_END_MS) as f32;
    const INV_SHOCK_SPAN_MS: f32 = 1.0 / (SHOCK_END_MS - DETONATION_END_MS) as f32;
    const INV_DEBRIS_SPAN_MS: f32 = 1.0 / (SUPER_TOTAL_MS - DEBRIS_START_MS) as f32;
    if out.len() >= cap {
        return;
    }
    let out_start = out.len();
    let (cw, ch) = (env.cell_w.max(1), env.cell_h.max(1));
    if env.light && t_ms < CHARGE_END_MS {
        let p = t_ms as f32 * INV_CHARGE_END_MS;
        let cov = env.intensity * (0.25 + 0.75 * p);
        let alpha = (255.0 * cov.min(1.0)) as u8;
        if alpha > 0 {
            let half_cw = env.cell_w / 2;
            let half_ch = env.cell_h / 2;
            let mut k = 0;
            while k < CHARGE_MOTES {
                if out.len() >= cap {
                    break;
                }
                let (x, y) = charge_mote_pos(env, k, p);
                let k_f32 = k as f32;
                k += 1;
                if x < 0 || y < 0 || x >= env.grid_w || y >= env.grid_h {
                    continue;
                }
                let hue = env.base_hue + k_f32 * 30.0;
                let (row, col) = (y / ch, x / cw);
                out.push(WordDecoration {
                    row: row as u16,
                    col: col as u16,
                    dx: ((x - col * cw) - half_cw).clamp(-6, 6) as i8,
                    dy: ((y - row * ch) - half_ch).clamp(-6, 6) as i8,
                    glyph: DecoGlyph::Dot,
                    blend: DecoBlend::Over,
                    color: hsv2rgb(hue, 1.0, 0.62),
                    alpha,
                });
            }
        }
    }
    if env.light && (CHARGE_END_MS..DETONATION_END_MS).contains(&t_ms) {
        let e = (t_ms - CHARGE_END_MS) as f32 * INV_DETONATION_SPAN_MS;
        let env_a = rise_fall(e) * env.intensity;
        let rows_span = 7i32;
        let r0 = i32::from(env.row) - rows_span / 2;
        let width = (MAX_VEIL_CELLS / rows_span as usize).min(usize::from(env.cols));
        let word_mid =
            i32::from(env.start_col) + (i32::from(env.end_col) - i32::from(env.start_col)) / 2;
        let c0 = (word_mid - width as i32 / 2)
            .clamp(0, i32::from(env.cols).saturating_sub(width as i32).max(0));
        const INV_HALF_ROWS: f32 = 2.0 / 7.0;
        let inv_half_w = 2.0 / width as f32;
        let base_a = 235.0 * env_a;
        let mut stamped = 0usize;
        if base_a >= 14.0 {
            let use_table = width == VEIL_DEFAULT_WIDTH;
            let width_i32 = width as i32;
            let mut dr = 0;
            while dr < rows_span {
                if out.len() >= cap || stamped >= MAX_VEIL_CELLS {
                    break;
                }
                let r = r0 + dr;
                if r < 0 {
                    dr += 1;
                    continue;
                }
                let row_u = r as u16;
                let dy_abs = (dr - rows_span / 2).unsigned_abs() as usize;
                let row_falloff = &VEIL_FALLOFF_28[dy_abs];
                let ny2 = if use_table {
                    0.0
                } else {
                    let ny = (dr - rows_span / 2) as f32 * INV_HALF_ROWS;
                    ny * ny
                };
                dr += 1;
                let mut dc = 0;
                while dc < width_i32 {
                    if out.len() >= cap || stamped >= MAX_VEIL_CELLS {
                        break;
                    }
                    let c = c0 + dc;
                    dc += 1;
                    let falloff = if use_table {
                        let dx_abs = (c - word_mid).unsigned_abs() as usize;
                        if dx_abs >= VEIL_HALF_W {
                            continue;
                        }
                        let f = row_falloff[dx_abs];
                        if f <= 0.0 {
                            continue;
                        }
                        f
                    } else {
                        let nx = (c - word_mid) as f32 * inv_half_w;
                        let d2 = nx * nx + ny2;
                        if d2 >= 1.0 {
                            continue;
                        }
                        crate::color_math::powf_pos_f32(1.0 - d2, 0.8)
                    };
                    let a = base_a * falloff;
                    if a < 14.0 {
                        continue;
                    }
                    out.push(WordDecoration {
                        row: row_u,
                        col: c as u16,
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
        }
        if env_a > 0.0 {
            let mut crown_segs = [(0.0f32, 0u8); 3];
            let mut i = 0;
            while i < crown_segs.len() {
                let seg = (i + 1) as f32;
                crown_segs[i] = (
                    env.r_max * 0.3 * seg,
                    (230.0 * env_a * (1.0 - 0.2 * seg)) as u8,
                );
                i += 1;
            }
            let dirs = crown_dirs();
            let mut di = 0;
            while di < dirs.len() {
                if out.len() >= cap {
                    break;
                }
                let (sin_a, cos_a) = dirs[di];
                di += 1;
                let mut si = 0;
                while si < crown_segs.len() {
                    if out.len() >= cap {
                        break;
                    }
                    let (d, alpha) = crown_segs[si];
                    si += 1;
                    let x = env.cx + (cos_a * d) as i32;
                    let y = env.cy + (sin_a * d * 0.75) as i32;
                    if x < 0 || y < 0 || x >= env.grid_w || y >= env.grid_h {
                        continue;
                    }
                    let (row, col) = (y / ch, x / cw);
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
        }
    }
    if env.light && (DETONATION_END_MS..SHOCK_END_MS).contains(&t_ms) {
        let q = (t_ms - DETONATION_END_MS) as f32 * INV_SHOCK_SPAN_MS;
        let fade = 1.0 - q;
        let alpha = (170.0 * fade * env.intensity) as u8;
        if alpha > 0 {
            let r = env.r_max * ease_out(q);
            let dirs = shock_dirs();
            let mut di = 0;
            while di < dirs.len() {
                if out.len() >= cap {
                    break;
                }
                let (sin_a, cos_a) = dirs[di];
                di += 1;
                let x = env.cx + (cos_a * r) as i32;
                let y = env.cy + (sin_a * r * 0.75) as i32;
                if x < 0 || y < 0 || x >= env.grid_w || y >= env.grid_h {
                    continue;
                }
                let (row, col) = (y / ch, x / cw);
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
    }
    if (DEBRIS_START_MS..SUPER_TOTAL_MS).contains(&t_ms) {
        const ANG_SCALE: f32 = core::f32::consts::TAU / 4096.0;
        const INV_255: f32 = 1.0 / 255.0;
        let p = (t_ms - DEBRIS_START_MS) as f32 * INV_DEBRIS_SPAN_MS;
        let fade_p = 1.0 - p * p;
        let alphas = [
            (255.0 * env.intensity * 1.0 * fade_p) as u8,
            (255.0 * env.intensity * crate::effect_util::TWINKLE_FLOOR * fade_p) as u8,
        ];
        if alphas[0] > 0 || alphas[1] > 0 {
            let n = 24 + (env.seed % 17) as usize;
            let base_r = env.r_max * (0.3 + 1.1 * p);
            let grav_y = (p * p * env.cell_h as f32) as i32;
            let grid_tick = t_ms / crate::nova::TWINKLE_GRID_MS;
            let hue_step = 360.0 / n as f32;
            let (sat, val, blend) = if env.light {
                (1.0, 0.62, DecoBlend::Over)
            } else {
                (0.85, 1.0, DecoBlend::Add)
            };
            let half_cw = env.cell_w / 2;
            let half_ch = env.cell_h / 2;
            let base_hue_drift = env.base_hue + p * 90.0;
            let mut k = 0;
            while k < n {
                if out.len() >= cap {
                    break;
                }
                let k_u64 = k as u64;
                let k_f32 = k as f32;
                k += 1;
                let s = crate::genome::mix(env.seed ^ k_u64.wrapping_mul(0x9E37_79B9_7F4A_7C15));
                let ang = (s & 4095) as f32 * ANG_SCALE;
                let speed = 0.5 + (s >> 13 & 0xff) as f32 * INV_255;
                let r = base_r * speed;
                let (sin_a, cos_a) = sin_cos_f32(ang);
                let x = env.cx + (cos_a * r) as i32;
                let y = env.cy + (sin_a * r * 0.7) as i32 + grav_y;
                if x < 0 || y < 0 || x >= env.grid_w || y >= env.grid_h {
                    continue;
                }
                let (row, col) = (y / ch, x / cw);
                let hue = base_hue_drift + k_f32 * hue_step;
                let tone = hsv2rgb(hue, sat, val);
                let grid_phase = ((grid_tick + k_u64) & 1) as usize;
                out.push(WordDecoration {
                    row: row as u16,
                    col: col as u16,
                    dx: ((x - col * cw) - half_cw).clamp(-6, 6) as i8,
                    dy: ((y - row * ch) - half_ch).clamp(-6, 6) as i8,
                    glyph: if s & 1 == 0 {
                        DecoGlyph::Star4
                    } else {
                        DecoGlyph::Dot
                    },
                    blend,
                    color: tone,
                    alpha: alphas[grid_phase],
                });
            }
        }
    }
    if env.light && (CHARGE_END_MS..SHOCK_END_MS).contains(&t_ms) {
        bound_shade_opacity(&mut out[out_start..]);
    }
}

/// The population the eclipse bound governs, by blend and glyph: light-theme
/// charge motes and debris are Over too but must not be darkened with the veil.
#[inline]
const fn is_shade(d: &WordDecoration) -> bool {
    matches!(d.blend, DecoBlend::Over) && matches!(d.glyph, DecoGlyph::Shade)
}

const SHADE_TAB_BITS: u32 = 9;
const SHADE_TAB: usize = 1 << SHADE_TAB_BITS;

/// The worst per-`(row, col)` aggregate Shade alpha in one pass: per-cell sums
/// accumulate in a fixed linear-probe table on the stack.
#[inline]
const fn peak_shade_probe(decos: &[WordDecoration]) -> u32 {
    const EMPTY: u64 = u64::MAX;
    let mut keys = [EMPTY; SHADE_TAB];
    let mut sums = [0u32; SHADE_TAB];
    let mut peak = 0u32;
    let mut i = 0;
    while i < decos.len() {
        let d = &decos[i];
        i += 1;
        if !is_shade(d) {
            continue;
        }
        debug_assert!(d.dx == 0 && d.dy == 0);
        let key = ((d.row as u64) << 16) | (d.col as u64);
        let mut slot = (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - SHADE_TAB_BITS)) as usize;
        loop {
            if keys[slot] == key {
                sums[slot] += d.alpha as u32;
                break;
            }
            if keys[slot] == EMPTY {
                keys[slot] = key;
                sums[slot] = d.alpha as u32;
                break;
            }
            slot = (slot + 1) & (SHADE_TAB - 1);
        }
        if sums[slot] > peak {
            peak = sums[slot];
        }
    }
    peak
}

#[inline]
const fn peak_shade_quadratic(decos: &[WordDecoration]) -> u32 {
    let mut peak = 0u32;
    let mut i = 0;
    while i < decos.len() {
        let d = &decos[i];
        i += 1;
        if !is_shade(d) {
            continue;
        }
        debug_assert!(d.dx == 0 && d.dy == 0);
        let mut sum = 0u32;
        let mut j = 0;
        while j < decos.len() {
            let other = &decos[j];
            j += 1;
            if is_shade(other) && other.row == d.row && other.col == d.col {
                sum += other.alpha as u32;
            }
        }
        if sum > peak {
            peak = sum;
        }
    }
    peak
}

/// Bound the eclipse's full-cell source-over opacity per host cell. Every
/// supernova Shade is cell-confined with zero jitter, so `(row, col)` is an
/// exact overlap class.
#[inline]
const fn bound_shade_opacity(decos: &mut [WordDecoration]) {
    if decos.is_empty() {
        return;
    }
    let peak = if decos.len() <= SHADE_TAB / 2 {
        peak_shade_probe(decos)
    } else {
        let mut shade_count = 0usize;
        let mut i = 0;
        while i < decos.len() {
            if is_shade(&decos[i]) {
                shade_count += 1;
            }
            i += 1;
        }
        if shade_count == 0 {
            return;
        }
        if shade_count <= SHADE_TAB / 2 {
            peak_shade_probe(decos)
        } else {
            peak_shade_quadratic(decos)
        }
    };
    if peak <= MAX_VIEWPORT_OVERLAY {
        return;
    }
    let mut i = 0;
    while i < decos.len() {
        let d = &mut decos[i];
        if is_shade(d) {
            d.alpha = ((d.alpha as u32) * MAX_VIEWPORT_OVERLAY / peak) as u8;
        }
        i += 1;
    }
}

#[cfg(test)]
#[path = "supernova_tests.rs"]
mod tests;
