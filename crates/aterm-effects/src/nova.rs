// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sparkle-words nova emitters (docs/sparkle-words-v2-design.md §6).

use aterm_render::{DecoBlend, DecoGlyph, GlowQuad, WordDecoration, premul_rgb};
use aterm_scene::{mix_rgb, smoothstep};

use crate::color_math::{hsv2rgb, rgb2hsv};
use crate::genome::{NovaFeatures, NovaMagic, mix};

/// §6.1 anticipation-dip window (ms): ink dims ~35 %, no quads.
pub const DIP_MS: u64 = 120;
/// §6.1 core-flash window end (ms): the star-glint crown, one rise-and-fall.
pub const FLASH_END_MS: u64 = 240;
/// §6.1 shockwave window end (ms): ring + rays sweep out (or in) and fade.
pub const RING_END_MS: u64 = 900;
/// §6.1 debris window start (ms); it ends at the genome duration `D`.
pub const DEBRIS_START_MS: u64 = 500;
/// Label boundary between Ember and Settled (ms past `D`): both emit the same (zero quads); Settled names the finished window.
pub const EMBER_TAIL_MS: u64 = 500;

/// §6.3 fixed ring band count (exact subdivision; the step scales, never this).
pub const RING_BANDS: usize = 30;
/// §6.3 fixed per-ray major-axis sample count.
pub const RAY_SAMPLES: usize = 15;
/// §6.3 Quasar jet sample count (doubled length ⇒ the same relative granularity).
pub const JET_SAMPLES: usize = 30;
/// §6.3 caps: per-nova quad budget (spend-order truncation past it).
pub const MAX_NOVA_QUADS_PER: usize = 512;
/// §6.3 caps: concurrent animating novas (excess skips straight to Ember).
pub const MAX_ACTIVE_NOVAS: usize = 3;
/// §6.3 cap: the decoration producers' backstop on the shared `nova_add` channel (`word_decorations` funds every classic nova and supernova under `MAX_NOVA_QUADS.saturating_sub(nova.len())`).
pub const MAX_NOVA_QUADS: usize = 1536;
const CROWN_QUADS: usize = 12;
/// §6.1 Singularity darkening-ring cell cap (Over-blend `RingArc` wdecos).
pub const MAX_RING_ARC_CELLS: usize = 20;
/// §6.5 blast-coupling cap: the N nearest ink-bearing occurrences per nova.
pub const MAX_COUPLING_WORDS: usize = 16;
/// §6.5 chroma-pulse window (ms) and amplitude cap.
pub const PULSE_MS: u64 = 150;
pub const PULSE_AMP: f32 = 0.35;
/// Debris twinkle phase grid (ms): mote phases snap to this shared grid so the region's combined onsets stay ≤ 3/s (§6.4).
pub const TWINKLE_GRID_MS: u64 = 350;

/// `NOVA_PALETTES[8]` (§6.2): `(core, fringe)` as `0x00RRGGBB`, Gray-indexed so neighbouring indices are similar tints: solar · ember · rose · violet · ion · plasma · aurora · emerald.
pub const NOVA_PALETTES: [(u32, u32); 8] = [
    (0x00FF_F2C8, 0x00FF_9A3C),
    (0x00FF_D8A0, 0x00FF_5C3C),
    (0x00FF_E0F0, 0x00FF_5CA8),
    (0x00EE_E0FF, 0x00A0_5CFF),
    (0x00E0_F0FF, 0x006C_8CFF),
    (0x00E8_F6FF, 0x004C_C8FF),
    (0x00E0_FFF0, 0x003C_E8A0),
    (0x00E8_FFD8, 0x007C_D83C),
];

/// The `(core, fringe)` pair for a Gray-decoded palette index.
#[inline]
pub const fn palette(idx: u8) -> (u32, u32) {
    NOVA_PALETTES[(idx & 7) as usize]
}

/// The §6.1 phase, a pure function of `(t = now − ignition, D)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NovaPhase {
    Armed,
    Dip,
    Flash,
    Ring,
    Debris,
    Ember,
    Settled,
}

#[inline]
pub const fn phase(t_ms: i64, d_ms: u32) -> NovaPhase {
    let d = d_ms as i64;
    match t_ms {
        t if t < 0 => NovaPhase::Armed,
        t if t < DIP_MS as i64 => NovaPhase::Dip,
        t if t < FLASH_END_MS as i64 => NovaPhase::Flash,
        t if t < DEBRIS_START_MS as i64 => NovaPhase::Ring,
        t if t < d => NovaPhase::Debris,
        t if t < d + EMBER_TAIL_MS as i64 => NovaPhase::Ember,
        _ => NovaPhase::Settled,
    }
}

/// The Dip's ink envelope multiplier at `t`: `1 − 0.35·smoothstep(p)` inside the dip window, `1.0` outside (rides the ink channel, no quads).
#[inline]
pub const fn dip_envelope(t_ms: u64) -> f32 {
    if t_ms >= DIP_MS {
        return 1.0;
    }
    1.0 - 0.35 * smoothstep(t_ms as f32 / DIP_MS as f32)
}

/// Everything the pure emitters need for one nova, resolved by the host once per frame (geometry in px; colors already genome-hue-nudged).
pub struct NovaEnv {
    /// Grid extent in px (quads clamp here; the one-band split uses `cell_h`).
    pub grid_w: i32,
    pub grid_h: i32,
    /// Row advance in px for this word's row (2× cell width on DECDWL rows); the centre anchor and debris cells follow it.
    pub cell_w: i32,
    pub cell_h: i32,
    /// Nova center, px (the word span's visual midpoint, row band center).
    pub cx: i32,
    pub cy: i32,
    /// Genome ring radius in px (`radius_rows · cell_h`, 1.6..=2.2 rows).
    pub r_max: f32,
    pub feats: NovaFeatures,
    pub magic: Option<NovaMagic>,
    /// Hue-nudged palette endpoints (§6.2 + the genome ink-pair window).
    pub core: u32,
    pub fringe: u32,
    /// Config profanity intensity, used as the nova brightness.
    pub intensity: f32,
    /// The occurrence seed — the §6.1 randomness root (debris ballistics).
    pub seed: u64,
}

/// Per-shape quad counts of one [`emit_nova`] call, read by the budget regression and the spend-order truncation tests.
#[derive(Default, Clone, Copy, Debug)]
pub struct NovaCounts {
    pub ring: usize,
    pub rays: usize,
    pub crown: usize,
    /// Ray samples that emitted ≥ 1 quad this frame, summed over rays.
    pub ray_samples: usize,
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

    /// One uniform-coverage chord quad: fractional x-extent folded into the
    /// coverage, quantized once through `premul_rgb`, the single rounding
    /// point both backends then add.
    #[inline]
    fn chord(&mut self, x0: f32, x1: f32, y0: i32, y1: i32, color: u32, cov: f32) {
        let xi0 = x0.floor() as i32;
        let xi1 = (x1.ceil() as i32).max(xi0 + 1);
        let frac = ((x1 - x0) / (xi1 - xi0) as f32).clamp(0.0, 1.0);
        let a = (255.0 * cov.clamp(0.0, 1.0) * frac).round() as u8;
        self.push(xi0, y0, xi1 - xi0, y1 - y0, premul_rgb(color, a));
    }
}

/// `1 − (1 − q)²` — the shockwave's ease-out radius curve (invertible, which
/// §6.5's analytic crossing time relies on).
#[inline]
const fn ease_out(q: f32) -> f32 {
    let u = 1.0 - q.clamp(0.0, 1.0);
    1.0 - u * u
}

/// Ring radius + fade envelope at `t`, or `None` outside the ring window.
#[inline]
pub const fn ring_radius(t_ms: u64, r_max: f32, singularity: bool) -> Option<(f32, f32)> {
    if t_ms < FLASH_END_MS || t_ms >= RING_END_MS {
        return None;
    }
    let q = (t_ms - FLASH_END_MS) as f32 / (RING_END_MS - FLASH_END_MS) as f32;
    let e = ease_out(q);
    let r = if singularity {
        r_max * (1.0 - e)
    } else {
        r_max * e
    };
    Some((r, 1.0 - q))
}

/// Ring/debris visibility floor: dark palette fringes (ion `#6c8cff`, emerald `#7cd83c`, …) under-read as additive light on dark glass, so lift the fringe's relative luminance to ≥ 0.42 by stepping toward the palette's own core (never white, keeping the pair's identity).
#[inline]
pub fn vivid_fringe(core: u32, fringe: u32) -> u32 {
    const FLOOR: f32 = 0.42;
    let table = crate::color_math::relative_luminance_table();
    let mut c = fringe;
    for k in 1..=8u32 {
        if crate::color_math::relative_luminance_with(table, c) >= FLOOR {
            return c;
        }
        c = mix_rgb(fringe, core, k as f32 / 8.0);
    }
    c
}

/// Band count: exactly [`RING_BANDS`] when the extent admits ≥ 2 px bands (`2r ≥ 60 px`, true from `cell_h = 14` at genome radii); below that `⌊2r/2⌋`, fewer quads, so the closed form still bounds.
#[inline]
pub fn ring_band_count(extent_px: f32) -> usize {
    if extent_px >= 2.0 * RING_BANDS as f32 {
        RING_BANDS
    } else {
        ((extent_px / 2.0).floor() as usize).clamp(1, RING_BANDS)
    }
}

/// Emit one nova frame's additive quads (crown/ring/rays) in §6.3 spend order.
pub fn emit_nova(t_ms: u64, env: &NovaEnv, budget: usize, out: &mut Vec<GlowQuad>) -> NovaCounts {
    let mut counts = NovaCounts::default();
    let mut sink = QuadSink {
        out,
        grid_w: env.grid_w,
        grid_h: env.grid_h,
        cell_h: env.cell_h,
        budget,
    };
    let singularity = env.magic == Some(NovaMagic::Singularity);
    match phase(t_ms as i64, env.feats.duration_ms) {
        NovaPhase::Ring | NovaPhase::Debris => {
            if let Some((r, fade)) = ring_radius(t_ms, env.r_max, singularity) {
                let n0 = sink.out.len();
                emit_ring(&mut sink, env, r, fade);
                counts.ring = sink.out.len() - n0;
                let n0 = sink.out.len();
                counts.ray_samples = emit_rays(&mut sink, env, t_ms, fade, singularity);
                counts.rays = sink.out.len() - n0;
            }
        }
        NovaPhase::Flash if sink.budget >= CROWN_QUADS => {
            let n0 = sink.out.len();
            emit_crown(&mut sink, env, t_ms);
            counts.crown = sink.out.len() - n0;
        }
        _ => {}
    }
    counts
}

fn emit_ring(sink: &mut QuadSink<'_>, env: &NovaEnv, r: f32, fade: f32) {
    let thick = env.feats.ring_thick;
    let chroma = env.feats.chroma;
    let re = r + chroma + 0.5 * thick;
    if re <= 0.5 {
        return;
    }
    let extent = 2.0 * re;
    let bands = ring_band_count(extent);
    let top = env.cy as f32 - re;
    let base_cov = (fade.max(0.0).sqrt() * env.intensity).clamp(0.0, 1.0);
    if base_cov == 0.0 {
        return;
    }
    let fringe = vivid_fringe(env.core, env.fringe);
    let cxf = env.cx as f32;
    let rings = [(r, env.core, 1.0f32), (r + chroma, fringe, 0.55)].map(|(rr, color, scale)| {
        let ro = rr + 0.5 * thick;
        let ri = (rr - 0.5 * thick).max(0.0);
        (ro * ro, ri * ri, color, base_cov * scale)
    });
    let mut prev_y1f = 0.0f32;
    for i in 0..bands {
        let y0f = prev_y1f;
        let y1f = (extent * (i + 1) as f32 / bands as f32).round();
        prev_y1f = y1f;
        if y1f <= y0f {
            continue;
        }
        let y0 = (top + y0f) as i32;
        let y1 = (top + y1f) as i32;
        let dy = (y0f + y1f) * 0.5 - re;
        let dy2 = dy * dy;
        for (ro2, ri2, color, cov) in rings {
            let d2_o = ro2 - dy2;
            if d2_o <= 0.0 {
                continue;
            }
            let wo = d2_o.sqrt();
            let d2_i = ri2 - dy2;
            let wi = if d2_i > 0.0 { d2_i.sqrt() } else { 0.0 };
            if wo - wi < 0.05 {
                continue;
            }
            if wi < 0.5 {
                sink.chord(cxf - wo, cxf + wo, y0, y1, color, cov);
            } else {
                sink.chord(cxf - wo, cxf - wi, y0, y1, color, cov);
                sink.chord(cxf + wi, cxf + wo, y0, y1, color, cov);
            }
        }
    }
}

fn emit_rays(
    sink: &mut QuadSink<'_>,
    env: &NovaEnv,
    t_ms: u64,
    fade: f32,
    singularity: bool,
) -> usize {
    let quasar = env.magic == Some(NovaMagic::Quasar);
    let q = (t_ms - FLASH_END_MS) as f32 / (RING_END_MS - FLASH_END_MS) as f32;
    let mut e = ease_out(q);
    if singularity {
        e = 1.0 - e;
    }
    let (nrays, samples, len_scale) = if quasar {
        (2usize, JET_SAMPLES, 2.0f32)
    } else {
        (usize::from(env.feats.rays), RAY_SAMPLES, 1.0)
    };
    let len = (0.4 + 0.6 * e) * env.r_max * len_scale;
    if len < 1.0 {
        return 0;
    }
    let (cxf, cyf) = (env.cx as f32, env.cy as f32);
    let core = env.core;
    let samples_f = samples as f32;
    let rot_turn = f32::from(env.feats.rot) / 16.0;
    let nrays_f = nrays as f32;
    let mut emitted_samples = 0usize;
    for k in 0..nrays {
        let ang = if quasar {
            // Twin vertical polar jets.
            if k == 0 {
                -std::f32::consts::FRAC_PI_2
            } else {
                std::f32::consts::FRAC_PI_2
            }
        } else {
            std::f32::consts::TAU * (rot_turn + k as f32 / nrays_f)
        };
        let (dy, dx) = ang.sin_cos();
        let major_x = dx.abs() >= dy.abs();
        let maj = dx.abs().max(dy.abs());
        let lm = len * maj;
        let (ux, uy) = (dx / maj, dy / maj);
        let mut prev_m1 = 0.0f32;
        for j in 0..samples {
            let m0 = prev_m1;
            let m1 = (lm * (j + 1) as f32 / samples_f).round();
            prev_m1 = m1;
            if m1 <= m0 {
                continue;
            }
            let quads_before = sink.out.len();
            let sf = (j as f32 + 0.5) / samples_f;
            let th = 2.5 - 1.5 * sf;
            let cov = (fade * (1.0 - 0.65 * sf) * env.intensity).clamp(0.0, 1.0);
            let mm = 0.5 * (m0 + m1);
            if major_x {
                let xa = cxf + ux * m0;
                let xb = cxf + ux * m1;
                let (x0, x1) = if xa <= xb { (xa, xb) } else { (xb, xa) };
                let x0_i = x0.floor() as i32;
                let yc = cyf + uy * mm;
                let y0 = (yc - 0.5 * th).floor() as i32;
                let y1 = ((yc + 0.5 * th).ceil() as i32).max(y0 + 1);
                let frac = (th / (y1 - y0) as f32).min(1.0);
                let a = (255.0 * cov * frac).round() as u8;
                sink.push(
                    x0_i,
                    y0,
                    (x1.ceil() as i32 - x0_i).max(1),
                    y1 - y0,
                    premul_rgb(core, a),
                );
            } else {
                let ya = cyf + uy * m0;
                let yb = cyf + uy * m1;
                let (y0, y1) = if ya <= yb { (ya, yb) } else { (yb, ya) };
                let y0_i = y0.floor() as i32;
                let xc = cxf + ux * mm;
                let x0 = (xc - 0.5 * th).floor() as i32;
                let x1 = ((xc + 0.5 * th).ceil() as i32).max(x0 + 1);
                let frac = (th / (x1 - x0) as f32).min(1.0);
                let a = (255.0 * cov * frac).round() as u8;
                sink.push(
                    x0,
                    y0_i,
                    x1 - x0,
                    (y1.ceil() as i32 - y0_i).max(1),
                    premul_rgb(core, a),
                );
            }
            emitted_samples += usize::from(sink.out.len() > quads_before);
        }
    }
    emitted_samples
}

fn emit_crown(sink: &mut QuadSink<'_>, env: &NovaEnv, t_ms: u64) {
    let e = (t_ms.saturating_sub(DIP_MS)) as f32 / (FLASH_END_MS - DIP_MS) as f32;
    let envl = (std::f32::consts::PI * e.clamp(0.0, 1.0)).sin();
    let ch = env.cell_h as f32;
    let (cxf, cyf) = (env.cx as f32, env.cy as f32);
    let r = 1.3 * ch;
    let beam = 0.30 * ch;
    for (f0, f1) in [(0.0f32, 0.45f32), (0.45, 1.0)] {
        let q = 1.0 - (f0 + f1) * 0.5;
        let hw = 0.24 * ch * q * q + 0.8;
        let cov = envl * env.intensity * (0.10 + 0.60 * q);
        let color = mix_rgb(env.core, 0x00FF_FFFF, 0.5 * q);
        for side in [-1.0f32, 1.0] {
            let ya = cyf + side * (beam + (r - beam) * f0);
            let yb = cyf + side * (beam + (r - beam) * f1);
            let (y0, y1) = if ya <= yb { (ya, yb) } else { (yb, ya) };
            sink.chord(cxf - hw, cxf + hw, y0 as i32, y1 as i32, color, cov);
        }
    }
    let color = mix_rgb(env.core, 0x00FF_FFFF, 0.7);
    for side in [-1.0f32, 1.0] {
        let yb = cyf + side * beam;
        let (y0, y1) = if cyf <= yb { (cyf, yb) } else { (yb, cyf) };
        let cov = envl * env.intensity * 0.85;
        sink.chord(
            cxf - 1.05 * ch,
            cxf + 1.05 * ch,
            y0 as i32,
            y1 as i32,
            color,
            cov,
        );
    }
}

/// Debris: 8–20 motes on analytic ballistic arcs (position is a closed form of age) on the wdeco Add stream, twinkling on the shared ≥ 350 ms phase grid (§6.4).
pub fn emit_debris(t_ms: u64, env: &NovaEnv, out: &mut Vec<WordDecoration>, cap: usize) {
    if out.len() >= cap || env.feats.debris == 0 {
        return;
    }
    let d = u64::from(env.feats.duration_ms);
    if !(DEBRIS_START_MS..d).contains(&t_ms) {
        return;
    }
    let singularity = env.magic == Some(NovaMagic::Singularity);
    let age = (t_ms - DEBRIS_START_MS) as f32 / 1000.0;
    let qn = (t_ms - DEBRIS_START_MS) as f32 / (d - DEBRIS_START_MS) as f32;
    let fade = 1.0 - qn;
    let (rows, cols) = (
        env.grid_h / env.cell_h.max(1),
        env.grid_w / env.cell_w.max(1),
    );
    if rows <= 0 || cols <= 0 {
        return;
    }
    let fringe = vivid_fringe(env.core, env.fringe);
    let tw_pair = [0.0f32, 0.5].map(|grid_phase| {
        0.35 + 0.65
            * (0.5
                + 0.5
                    * (std::f32::consts::TAU
                        * (t_ms as f32 / (2.0 * TWINKLE_GRID_MS as f32) + grid_phase))
                        .sin())
    });
    let alpha_pair = tw_pair.map(|tw| (env.intensity * fade * tw).clamp(0.0, 1.0));
    if alpha_pair[0] <= 0.01 && alpha_pair[1] <= 0.01 {
        return;
    }
    let alpha_u8_pair = alpha_pair.map(|a| (a * 255.0).round() as u8);
    let sing_contract = if singularity { 1.0 - ease_out(qn) } else { 0.0 };
    let ch_f = env.cell_h as f32;
    let grav_drop = 0.5 * (2.2 * ch_f) * age * age;
    let (cxf, cyf) = (env.cx as f32, env.cy as f32);
    let (grid_wf, grid_hf) = (env.grid_w as f32, env.grid_h as f32);
    for m in 0..u32::from(env.feats.debris) {
        if out.len() >= cap {
            break;
        }
        let s = mix(env.seed ^ u64::from(m).wrapping_mul(0xA24B_AED4_963E_E407));
        let phase_idx = ((s >> 33) & 1) as usize;
        if alpha_pair[phase_idx] <= 0.01 {
            continue;
        }
        let r0 = (s & 0xffff) as f32 / 65535.0;
        let r1 = ((s >> 16) & 0xffff) as f32 / 65535.0;
        let r2 = ((s >> 32) & 0xff) as f32 / 255.0;
        let ang = std::f32::consts::TAU * r0;
        let (sin_a, cos_a) = ang.sin_cos();
        let (x, y) = if singularity {
            let rr = env.r_max * (0.6 + 0.4 * r1) * sing_contract;
            (cxf + cos_a * rr, cyf + sin_a * rr)
        } else {
            let speed = (0.4 + 1.2 * r1) * ch_f;
            let (vx, vy) = (cos_a * speed, sin_a * speed - 0.6 * ch_f);
            (cxf + vx * age, cyf + vy * age + grav_drop)
        };
        if x < 0.0 || y < 0.0 || x >= grid_wf || y >= grid_hf {
            continue;
        }
        let col = ((x as i32) / env.cell_w).clamp(0, cols - 1);
        let row = ((y as i32) / env.cell_h).clamp(0, rows - 1);
        let dx = (x as i32 - col * env.cell_w - env.cell_w / 2).clamp(-127, 127) as i8;
        let dy = (y as i32 - row * env.cell_h - env.cell_h / 2).clamp(-127, 127) as i8;
        out.push(WordDecoration {
            row: row as u16,
            col: col as u16,
            dx,
            dy,
            glyph: if s & 4 == 0 {
                DecoGlyph::Star4
            } else {
                DecoGlyph::Dot
            },
            blend: DecoBlend::Add,
            color: mix_rgb(env.core, fringe, r2),
            alpha: alpha_u8_pair[phase_idx],
        });
    }
}

/// The Singularity darkening ring: ≤ [`MAX_RING_ARC_CELLS`] per-cell Over [`DecoGlyph::RingArc`] wdecos on the cells the contracting ring crosses (`nova_add` is additive and can only brighten).
pub fn emit_ring_arc(t_ms: u64, env: &NovaEnv, out: &mut Vec<WordDecoration>, cap: usize) {
    if cap == 0 {
        return;
    }
    let Some((r, fade)) = ring_radius(t_ms, env.r_max, true) else {
        return;
    };
    if r < 1.0 {
        return;
    }
    let (rows, cols) = (
        env.grid_h / env.cell_h.max(1),
        env.grid_w / env.cell_w.max(1),
    );
    let half = 0.5 * env.cell_w.max(env.cell_h) as f32;
    let min_r = (r - half).max(0.0);
    let max_r = r + half;
    let min_d2 = min_r * min_r;
    let max_d2 = max_r * max_r;
    let row0 = ((env.cy as f32 - r) as i32 / env.cell_h).max(0);
    let row1 = ((env.cy as f32 + r) as i32 / env.cell_h).min(rows - 1);
    let col0 = ((env.cx as f32 - r) as i32 / env.cell_w).max(0);
    let col1 = ((env.cx as f32 + r) as i32 / env.cell_w).min(cols - 1);
    let alpha = (150.0 * fade * env.intensity).clamp(0.0, 255.0) as u8;
    let (cxf, cyf) = (env.cx as f32, env.cy as f32);
    let mut emitted = 0usize;
    for row in row0..=row1 {
        let ccy = (row * env.cell_h + env.cell_h / 2) as f32;
        let dy = ccy - cyf;
        let dy2 = dy * dy;
        if dy2 > max_d2 {
            continue;
        }
        for col in col0..=col1 {
            if emitted >= cap {
                return;
            }
            let ccx = (col * env.cell_w + env.cell_w / 2) as f32;
            let dx = ccx - cxf;
            let d2 = dx * dx + dy2;
            if d2 < min_d2 || d2 > max_d2 {
                continue;
            }
            out.push(WordDecoration {
                row: row as u16,
                col: col as u16,
                dx: 0,
                dy: 0,
                glyph: DecoGlyph::RingArc,
                blend: DecoBlend::Over,
                color: 0x001A_1022,
                alpha,
            });
            emitted += 1;
        }
    }
}

/// When the expanding (or contracting) ring first crosses `dist` px from the centre, in ms since ignition: the analytic inverse of [`ring_radius`]'s ease-out, so the pulse needs no stored state.
#[inline]
pub fn crossing_ms(dist: f32, r_max: f32, singularity: bool) -> Option<u64> {
    if !(0.0..=r_max).contains(&dist) || r_max <= 0.0 {
        return None;
    }
    let q = if singularity {
        1.0 - (dist / r_max).sqrt()
    } else {
        1.0 - (1.0 - dist / r_max).sqrt()
    };
    Some(
        (FLASH_END_MS + (q.clamp(0.0, 1.0) * (RING_END_MS - FLASH_END_MS) as f32) as u64)
            .min(RING_END_MS - 1),
    )
}

/// The ~150 ms smoothstep in/out pulse envelope at `t` for a crossing at `cross_ms`; `None` outside the window.
#[inline]
pub const fn pulse_env(t_ms: u64, cross_ms: u64) -> Option<f32> {
    if t_ms < cross_ms || t_ms >= cross_ms + PULSE_MS {
        return None;
    }
    let e = (t_ms - cross_ms) as f32 / PULSE_MS as f32;
    Some(smoothstep((2.0 * e.min(1.0 - e)).clamp(0.0, 1.0)))
}

/// Constant-luminance pairing by construction: take `target`'s hue and saturation, then bisect value (and, where the hue cannot reach a bright anchor at full value, saturation toward white) until the WCAG relative luminance matches `anchor`'s.
pub fn constant_lum_toward(anchor: u32, target: u32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    let want = crate::color_math::relative_luminance_with(table, anchor);
    let (h, s, _) = rgb2hsv(target);
    let full_v = hsv2rgb(h, s, 1.0);
    if crate::color_math::relative_luminance_with(table, full_v) >= want {
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        let (mut lo_rgb, mut hi_rgb) = (0x0000_0000, full_v);
        for i in 0..20 {
            let mid = 0.5 * (lo + hi);
            let mid_rgb = hsv2rgb(h, s, mid);
            if mid_rgb == lo_rgb {
                lo = mid;
            } else if mid_rgb == hi_rgb {
                hi = mid;
            } else if crate::color_math::relative_luminance_with(table, mid_rgb) < want {
                lo = mid;
                lo_rgb = mid_rgb;
            } else {
                hi = mid;
                hi_rgb = mid_rgb;
            }
            if i >= 1 && lo_rgb == hi_rgb {
                return lo_rgb;
            }
        }
        return hsv2rgb(h, s, 0.5 * (lo + hi));
    }
    let (mut lo, mut hi) = (0.0f32, s);
    let (mut lo_rgb, mut hi_rgb) = (0x00FF_FFFF, full_v);
    for i in 0..20 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = hsv2rgb(h, mid, 1.0);
        if mid_rgb == lo_rgb {
            lo = mid;
        } else if mid_rgb == hi_rgb {
            hi = mid;
        } else if crate::color_math::relative_luminance_with(table, mid_rgb) < want {
            hi = mid;
            hi_rgb = mid_rgb;
        } else {
            lo = mid;
            lo_rgb = mid_rgb;
        }
        if i >= 1 && lo_rgb == hi_rgb {
            return lo_rgb;
        }
    }
    hsv2rgb(h, 0.5 * (lo + hi), 1.0)
}

/// The applied pulse colour: the anchor's hue/saturation shifted toward `fringe` at amplitude `amp` (≤ [`PULSE_AMP`]), then luminance-matched back to the anchor, so |ΔL| ≤ 5% by construction.
#[inline]
pub fn pulse_color(anchor: u32, fringe: u32, amp: f32) -> u32 {
    constant_lum_toward(anchor, mix_rgb(anchor, fringe, amp.clamp(0.0, PULSE_AMP)))
}

/// §6.1 ember ink anchors: the settled gradient shifted to the palette's ember tone; the Singularity ends in a dim violet ember instead.
#[inline]
pub const fn ember_pair(pair: (u32, u32), magic: Option<NovaMagic>) -> (u32, u32) {
    if matches!(magic, Some(NovaMagic::Singularity)) {
        return (0x002A_2040, 0x005A_48A8);
    }
    const fn dim(c: u32, f: f32) -> u32 {
        const fn m(c: u32, f: f32, sh: u32) -> u32 {
            ((((c >> sh) & 0xff) as f32) * f) as u32
        }
        (m(c, f, 16) << 16) | (m(c, f, 8) << 8) | m(c, f, 0)
    }
    (dim(pair.0, 0.8), dim(pair.1, 0.55))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::genome::nova_features;

    fn worst_feats() -> NovaFeatures {
        let mut best: Option<NovaFeatures> = None;
        for g in 0..(1u64 << 15) {
            let f = nova_features(g);
            if f.rays == 8 && f.radius > 2.19 && f.chroma > 2.49 && f.ring_thick > 3.49 {
                best = Some(f);
                break;
            }
        }
        best.expect("genome-max feature combination is reachable (§3.4 reachability)")
    }

    fn env_at(ch: i32, magic: Option<NovaMagic>) -> NovaEnv {
        let feats = worst_feats();
        let cw = ch / 2;
        NovaEnv {
            grid_w: 200 * cw,
            grid_h: 60 * ch,
            cell_w: cw,
            cell_h: ch,
            cx: 100 * cw,
            cy: 30 * ch,
            r_max: feats.radius * ch as f32,
            feats,
            magic,
            core: NOVA_PALETTES[0].0,
            fringe: NOVA_PALETTES[0].1,
            intensity: 1.0,
            seed: 0xDECAF,
        }
    }

    #[test]
    fn nova_budget_geometry_invariant() {
        for ch in [14i32, 20, 40, 56] {
            let env = env_at(ch, None);
            let extent = 2.0 * (env.r_max + env.feats.chroma + 0.5 * env.feats.ring_thick);
            let bands = ring_band_count(extent);
            assert_eq!(
                bands, 30,
                "band count must be exactly 30 at ch={ch} (extent {extent:.1} px), counted {bands}"
            );
            let mut late = Vec::new();
            let cl = emit_nova(860, &env, MAX_NOVA_QUADS_PER, &mut late);
            let nrays = usize::from(env.feats.rays);
            assert_eq!(nrays, 8, "genome-max ray count, counted {nrays}");
            assert_eq!(
                cl.ray_samples,
                RAY_SAMPLES * nrays,
                "ray samples must be 15 per ray at ch={ch}: counted {} over {nrays} rays \
                 (want {} = 15 × {nrays})",
                cl.ray_samples,
                RAY_SAMPLES * nrays
            );
            let mut out = Vec::new();
            let mut uncapped = Vec::new();
            let mut worst = 0usize;
            for t in (0..1400u64).step_by(10) {
                out.clear();
                let counts = emit_nova(t, &env, MAX_NOVA_QUADS_PER, &mut out);
                worst = worst.max(out.len());
                assert!(
                    out.len() <= 392,
                    "Q ≤ 392 violated at ch={ch} t={t}: counted {} (ring {} rays {} crown {})",
                    out.len(),
                    counts.ring,
                    counts.rays,
                    counts.crown
                );
                uncapped.clear();
                emit_nova(t, &env, usize::MAX, &mut uncapped);
                assert_eq!(
                    out,
                    uncapped,
                    "the {MAX_NOVA_QUADS_PER}/nova cap bound at ch={ch} t={t} \
                     (capped {} vs unbounded {} quads)",
                    out.len(),
                    uncapped.len()
                );
                for q in &out {
                    let band = i32::from(q.row) * ch;
                    assert!(
                        i32::from(q.y) >= band && i32::from(q.y) + i32::from(q.h) <= band + ch,
                        "quad escapes its row band at ch={ch} t={t}: {q:?}"
                    );
                    assert!(i32::from(q.x) + i32::from(q.w) <= env.grid_w);
                }
            }
            assert!(worst > 0, "the nova must actually emit at ch={ch}");
            assert!(
                worst <= 392 && 392 < MAX_NOVA_QUADS_PER,
                "headroom must hold at ch={ch}: worst {worst} ≤ 392 < {MAX_NOVA_QUADS_PER}"
            );
        }
        const { assert!(MAX_ACTIVE_NOVAS * 392 <= MAX_NOVA_QUADS) };
    }

    #[test]
    fn spend_order_truncation_ring_first_crown_drops() {
        let env = env_at(20, None);
        let t = 600u64;
        let mut full = Vec::new();
        let counts = emit_nova(t, &env, MAX_NOVA_QUADS_PER, &mut full);
        assert!(counts.ring > 0 && counts.rays > 0);
        let mut capped = Vec::new();
        let c = emit_nova(t, &env, counts.ring + 10, &mut capped);
        assert_eq!(c.ring, counts.ring, "the ring is never gapped by a cap");
        assert_eq!(c.rays, 10, "rays truncate to the remaining budget");
        assert_eq!(c.crown, 0);
        assert_eq!(
            capped[..counts.ring],
            full[..counts.ring],
            "identical ring bytes"
        );
        let tf = 180u64;
        let mut crown_full = Vec::new();
        let cf = emit_nova(tf, &env, MAX_NOVA_QUADS_PER, &mut crown_full);
        assert!(
            (1..=CROWN_QUADS).contains(&cf.crown),
            "crown ≤ ~12 quads: {}",
            cf.crown
        );
        let mut crown_capped = Vec::new();
        let cc = emit_nova(tf, &env, CROWN_QUADS - 1, &mut crown_capped);
        assert_eq!(
            cc.crown, 0,
            "a partial crown never emits (crown drops first)"
        );
        assert!(crown_capped.is_empty());
    }

    #[test]
    fn coupling_crossing_is_deterministic_and_monotone() {
        let rm = 44.0f32;
        let a = crossing_ms(10.0, rm, false).unwrap();
        let b = crossing_ms(10.0, rm, false).unwrap();
        assert_eq!(a, b, "pure function of (dist, r_max)");
        let far = crossing_ms(40.0, rm, false).unwrap();
        assert!(far > a, "the expanding ring reaches farther words later");
        assert!(
            crossing_ms(50.0, rm, false).is_none(),
            "beyond r_max: never"
        );
        let sa = crossing_ms(10.0, rm, true).unwrap();
        let sfar = crossing_ms(40.0, rm, true).unwrap();
        assert!(sfar < sa, "the contracting ring reaches far words FIRST");
        for (d, sing) in [
            (0.0f32, false),
            (10.0, false),
            (40.0, false),
            (rm, false),
            (0.0, true),
            (10.0, true),
            (rm, true),
        ] {
            let t = crossing_ms(d, rm, sing).unwrap();
            assert!((FLASH_END_MS..RING_END_MS).contains(&t));
            let (r, _) = ring_radius(t, rm, sing).unwrap();
            assert!((r - d).abs() < 2.5, "R(t_cross) ≈ dist: {r} vs {d}");
        }
        assert!(pulse_env(299, 300).is_none());
        assert!(pulse_env(300, 300).is_some());
        assert!(pulse_env(375, 300).unwrap() > 0.9, "peak mid-window");
        assert!(pulse_env(450, 300).is_none());
    }

    #[test]
    fn delta_l_bound_over_palette_table() {
        use crate::color_math::{hue_nudge, relative_luminance};
        let mut anchors: Vec<u32> = vec![0x007C_C8FF, 0x00C8_9AFF, 0x00F7_A8B8, 0x00FF_D9C2];
        for (core, fringe) in NOVA_PALETTES {
            anchors.push(core);
            anchors.push(fringe);
        }
        for &base in &anchors {
            for code in 0..16u8 {
                let anchor = hue_nudge(base, code);
                let la = relative_luminance(anchor);
                for (_, fringe) in NOVA_PALETTES {
                    let tone = constant_lum_toward(anchor, fringe);
                    let dl = (relative_luminance(tone) - la).abs();
                    assert!(
                        dl <= 0.05,
                        "|ΔL| {dl:.4} > 5% for anchor {anchor:06x} → fringe {fringe:06x}"
                    );
                    for amp in [0.1f32, 0.2, PULSE_AMP] {
                        let mixed = pulse_color(anchor, fringe, amp);
                        let dm = (relative_luminance(mixed) - la).abs();
                        assert!(
                            dm <= 0.05,
                            "pulse |ΔL| {dm:.4} > 5% at amp {amp} for {anchor:06x}"
                        );
                        let (r, g, b) = ((mixed >> 16) & 0xff, (mixed >> 8) & 0xff, mixed & 0xff);
                        let red_frac = r as f32 / (r + g + b).max(1) as f32;
                        assert!(red_frac < 0.8, "pulse produced saturated red {mixed:06x}");
                    }
                }
            }
        }
    }

    #[test]
    fn singularity_contracts_and_darkens() {
        let env = env_at(20, Some(NovaMagic::Singularity));
        let (r_early, _) = ring_radius(300, env.r_max, true).unwrap();
        let (r_late, _) = ring_radius(800, env.r_max, true).unwrap();
        assert!(r_early > r_late, "the Singularity ring contracts");
        let mut decos = Vec::new();
        emit_ring_arc(300, &env, &mut decos, MAX_RING_ARC_CELLS);
        assert!(!decos.is_empty() && decos.len() <= MAX_RING_ARC_CELLS);
        for d in &decos {
            assert_eq!(d.glyph, DecoGlyph::RingArc);
            assert!(
                matches!(d.blend, DecoBlend::Over),
                "darkening must be Over, not Add"
            );
        }
        let mut late = Vec::new();
        emit_ring_arc(850, &env, &mut late, MAX_RING_ARC_CELLS);
        let mean_dist = |ds: &[WordDecoration]| {
            ds.iter()
                .map(|d| {
                    let x = i32::from(d.col) * env.cell_w + env.cell_w / 2 - env.cx;
                    let y = i32::from(d.row) * env.cell_h + env.cell_h / 2 - env.cy;
                    ((x * x + y * y) as f32).sqrt()
                })
                .sum::<f32>()
                / ds.len().max(1) as f32
        };
        assert!(
            mean_dist(&late) < mean_dist(&decos),
            "the shadow ring falls inward"
        );
        let mote_dist = |t: u64| {
            let mut motes = Vec::new();
            emit_debris(t, &env, &mut motes, 64);
            assert!(!motes.is_empty());
            mean_dist(&motes)
        };
        let late_t = u64::from(env.feats.duration_ms) - 100;
        assert!(
            mote_dist(late_t) < mote_dist(600),
            "Singularity debris collapses inward"
        );
    }
}
