// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! FUCK SUPER NOVA — the v3 §3.2 escalation of the rainbow profanity ink: a
//! viewport-scale, theme-aware detonation that ~10% of rainbow episodes roll.
//!
//! **Pure emitter** (the `nova.rs` discipline): every function is a function
//! of `(t = now − ignition, env)`; same inputs ⇒ byte-identical output. The
//! host state machine (`word_decorations.rs`) owns the roll
//! (`mix(gkey ^ birth_seq ^ SUPERNOVA_SALT)`), the flash-limiter grant, the
//! `MAX_ACTIVE_SUPERNOVAE = 1` cap, the global burst mutex (classic nova
//! ignitions defer while a supernova is live), and the selection wash-split.
//!
//! ## Phases (2400 ms)
//!
//! * **Charge** `0–350`: the word's ink accelerates (host-side ink fx);
//!   12 converging motes spiral into the word — additive quads on dark
//!   themes, Over-blend deep-candy decos on light (additive is a no-op
//!   over white).
//! * **Detonation** `350–650` — THEME-BRANCHED (§3.2
//!   normative: additive white is INVISIBLE on light themes):
//!   dark bg ⇒ full-viewport per-row additive wash (`sin(π·e)` envelope, warm
//!   core tint) + a giant 8-point additive star crown, with their aggregate
//!   per-row light capped before they reach the renderer;
//!   light bg (*the eclipse*) ⇒ an Over-blend dark veil of per-cell
//!   `DecoGlyph::Shade` stamps (design §3.3) around the word (≤ 200 cells)
//!   + a dark crown, with aggregate per-cell opacity capped.
//! * **Shockwave** `650–1600`: a double ring expanding to
//!   `min(6 rows, grid extent)`; 30-band chord construction (≤ 2 chords per
//!   band per ring). Light themes keep a saturated additive core PLUS an
//!   Over dark fringe of stamps along the circumference.
//! * **Rainbow debris** `1200–2400`: 24–40 hue-cycled motes on the shared
//!   350 ms twinkle grid (additive on dark; Over deep-candy on light).
//! * **Afterglow** `≥ 2400`: the ink settles to the static rainbow; the ember
//!   star fades ≤ 2 s (both host-side).

use aterm_render::{DecoBlend, DecoGlyph, GlowQuad, WordDecoration};
use aterm_scene::mix_rgb;

use crate::color_math::hsv2rgb;

/// Session roll salt: `mix(gkey ^ birth_seq ^ SUPERNOVA_SALT) % 100 <
/// chance_pct` (design §3.2 — deterministic for the driven script, yet
/// decorrelated across repeats of the same word at the same prompt).
pub const SUPERNOVA_SALT: u64 = 0xF0CC_AC1A_5EED_B00F;

/// Phase boundaries, ms.
pub const CHARGE_END_MS: u64 = 350;
pub const DETONATION_END_MS: u64 = 650;
pub const SHOCK_END_MS: u64 = 1600;
pub const DEBRIS_START_MS: u64 = 1200;
/// Total window; the host sets `nova_done` here and starts the ember fade.
pub const SUPER_TOTAL_MS: u64 = 2400;

/// §3.2 shockwave reach in rows (clamped to the grid extent by the caller).
pub const SUPER_RADIUS_ROWS: f32 = 6.0;

/// THE §3.2 reach clamp, px: `min(6 rows · cell_h, grid_h / 2)`, floored at
/// 1 px — the exact expression the engine's supernova prepass resolves
/// [`SuperEnv::r_max`] with (word_decorations.rs). The parity suite builds
/// its `SuperEnv` through this helper so its pins run at a radius the engine
/// can actually produce; keep every `r_max` producer on it.
#[must_use]
pub fn r_max_for(cell_h: i32, grid_h: i32) -> f32 {
    (SUPER_RADIUS_ROWS * cell_h.max(1) as f32)
        .min((grid_h.max(cell_h) as f32) / 2.0)
        .max(1.0)
}

/// Per-supernova quad budget (own `QuadSink` clone under it).
pub const MAX_SUPER_QUADS_PER: usize = 1024;
/// Concurrency: at most ONE live supernova, window-wide.
pub const MAX_ACTIVE_SUPERNOVAE: usize = 1;
/// Charge motes.
pub const CHARGE_MOTES: usize = 12;
/// Detonation wash rows counted in the closed form (viewport rows clamp).
pub const MAX_WASH_ROWS: usize = 160;
/// The light-theme veil cell cap (inside `MAX_DECORATIONS = 256` with debris
/// headroom).
pub const MAX_VEIL_CELLS: usize = 200;
/// Ring band count (the classic nova's §6.3 construction).
pub const SUPER_RING_BANDS: usize = 30;

/// Readability ceiling for the viewport-scale detonation channels.
///
/// Dark-theme light is premultiplied RGB and composited with saturating
/// `One`/`One` addition.  We find the largest aggregate RGB channel at any
/// rectangle-overlap point, then scale the complete frame uniformly to this
/// value.  At any pixel, every individual RGB channel can therefore increase
/// by at most 64/255, even where the wash and several crown pieces overlap.
///
/// The light-theme eclipse uses full-cell `Shade` sprites with source-over
/// blending.  We independently bound the SUM of their alphas at each cell to
/// the same value, so its effective aggregate opacity is at most 64/255 (the
/// union bound; the exact source-over opacity is lower).  Small rainbow debris
/// and charge-mote glyphs are intentionally excluded: they do not cover a cell
/// and are the sparkle detail the effect is meant to retain.
pub const MAX_VIEWPORT_OVERLAY: u32 = 64;

/// Closed-form per-frame worst case: wash ≤ 160 rows × ≤ 3 quads/row after
/// host-side selection splits (480) + double ring ≤ 120 + crown ≤ 50 +
/// charge ≤ 12 + slack ⇒ **S_max ≤ 900**, pinned by the structural test
/// below (plain `#[test]`; the ay bundle is untouched — design §3.2).
pub const S_MAX_BOUND: usize = 900;

// Re-derived §3.2 budget asserts: the bound fits the per-super cap, and the
// GLOBAL BURST MUTEX — TWO-WAY: classic ignitions are limiter-deferred while
// a supernova is live AND `super_prepass` defers a supernova grant behind any
// live classic window (word_decorations.rs busy scan keyed on
// `Episode::burst_kind`) — keeps the combined `nova_add` channel under
// `MAX_NOVA_QUADS`: the "never binds" claim re-derived, not silently
// falsified.
const _: () = {
    assert!(S_MAX_BOUND <= MAX_SUPER_QUADS_PER);
    assert!(MAX_SUPER_QUADS_PER <= crate::nova::MAX_NOVA_QUADS);
    assert!(crate::nova::MAX_ACTIVE_NOVAS * 392 <= crate::nova::MAX_NOVA_QUADS);
    assert!(S_MAX_BOUND <= crate::nova::MAX_NOVA_QUADS);
};

/// Everything the pure emitters need for one supernova, resolved by the host
/// once per frame.
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
    /// Shockwave reach, px: `min(6 rows · cell_h, grid extent)`.
    pub r_max: f32,
    /// Word span, cells (the light-theme veil anchors around it).
    pub row: u16,
    pub start_col: u16,
    pub end_col: u16,
    /// Grid cols (veil clamp).
    pub cols: u16,
    /// THEME BRANCH (§3.2 normative): `relative_luminance(ink_bg) > 0.5`,
    /// resolved per occurrence by the host.
    pub light: bool,
    /// Config profanity intensity.
    pub intensity: f32,
    /// The occurrence seed — the debris/mote randomness root.
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

/// A budgeted, grid-clamped, row-band-splitting [`GlowQuad`] sink — the
/// `nova::QuadSink` clone this module owns (design §3.2: "own constants, own
/// QuadSink clone").
struct QuadSink<'a> {
    out: &'a mut Vec<GlowQuad>,
    grid_w: i32,
    grid_h: i32,
    cell_h: i32,
    budget: usize,
}

impl QuadSink<'_> {
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
        let mut yy = y0;
        while yy < y1 {
            if self.budget == 0 {
                return;
            }
            let row = yy / self.cell_h;
            let band_end = ((row + 1) * self.cell_h).min(y1);
            self.out.push(GlowQuad {
                row: row as u16,
                x: x0 as u16,
                y: yy as u16,
                w: (x1 - x0) as u16,
                h: (band_end - yy) as u16,
                color: premul,
            });
            self.budget -= 1;
            yy = band_end;
        }
    }
}

/// Premultiply a `0x00RRGGBB` color by coverage (single quantization point).
fn premul(color: u32, cov: f32) -> u32 {
    let a = cov.clamp(0.0, 1.0);
    let m = |sh: u32| ((((color >> sh) & 0xff) as f32) * a).round() as u32;
    (m(16) << 16) | (m(8) << 8) | m(0)
}

/// `sin(π·e)` — one rise-and-fall, never periodic (flash-safety shape).
fn rise_fall(e: f32) -> f32 {
    (core::f32::consts::PI * e.clamp(0.0, 1.0)).sin()
}

/// `1 − (1 − q)²` ease-out (the nova shockwave curve).
fn ease_out(q: f32) -> f32 {
    let u = 1.0 - q.clamp(0.0, 1.0);
    1.0 - u * u
}

/// Warm detonation core tint.
const WASH_CORE: u32 = 0x00FF_EED2;
/// The light-theme veil / dark-crown stamp tone (near-black warm violet — the
/// Singularity's own Over tone family).
const VEIL_TONE: u32 = 0x001A_1022;

/// Emit one supernova frame's additive quads (charge motes / wash / crown /
/// double ring) under `budget`. Pure; returns per-shape counts.
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
        // Charge: 12 converging motes spiral toward the word center. Dark
        // themes only — additive motes are a no-op over white, so on light
        // themes the charge rides the Over deco stream (`emit_super_decos`).
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
            // Dark bg: full-viewport per-row wash, peak coverage 0.9, warm
            // core tint brightening toward the center row. The (up to)
            // `MAX_WASH_ROWS`-row window is CENTERED on the blast row and
            // clamped to the grid — anchoring it at row 0 would leave the
            // word's own row unwashed on a > 160-row viewport. Row count is
            // unchanged (still `min(vrows, 160)`, the S_max closed form).
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
            // Giant 8-point additive star crown (~6 rows tall).
            let n0 = sink.out.len();
            emit_crown_quads(&mut sink, env, env_a);
            counts.crown = sink.out.len() - n0;
        }
        // Light bg detonation rides the Over deco stream (emit_super_decos):
        // additive white is invisible on white.
    } else if t_ms < SHOCK_END_MS {
        // Double shockwave ring, 30-band chords, ≤ 2 chords/band/ring.
        let q = (t_ms - DETONATION_END_MS) as f32 / (SHOCK_END_MS - DETONATION_END_MS) as f32;
        let fade = 1.0 - q;
        let n0 = sink.out.len();
        for (ring, rscale) in [(0usize, 1.0f32), (1, 0.72)] {
            let r = env.r_max * ease_out(q) * rscale;
            let thick = 3.0 - ring as f32;
            let hue = (env.base_hue + ring as f32 * 40.0).rem_euclid(360.0);
            // Light themes keep a SATURATED core (deep candy, not white) so
            // the additive ring survives a white bg; the dark fringe rides
            // the Over stream.
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
    // The renderer uses a raw One/One additive blend, so it cannot infer alpha
    // or recover contrast after overlapping quads saturate.  Normalize the
    // complete episode here, once, while the stream is still bounded and
    // backend-independent.  Geometry/order/timing stay unchanged and CPU/GPU
    // consume the same final bytes.
    bound_additive_overlap(&mut sink.out[out_start..]);
    counts
}

/// Cap aggregate additive light without a framebuffer-sized coverage map.
/// Every emitted quad is an axis-aligned integer rectangle confined to one cell
/// row.  Rectangle coverage is constant between start edges, so the maximum
/// overlap has an `(x, y)` drawn from those edges.  Exhaustively probing that
/// bounded cross-product finds the exact peak for each channel; one uniform
/// scale then preserves the wash falloff, hues, and crown-to-wash proportions.
///
/// The emitter's structural cap is 160 wash rows plus at most 120 ring / 26
/// crown / 12 charge pieces, with only the small crown/ring subset sharing a
/// row.  This allocation-free overlap sweep is therefore much smaller than a
/// pixel raster and remains in the few-dozen-microsecond engine budget.
fn bound_additive_overlap(quads: &mut [GlowQuad]) {
    let peak = peak_additive_channel(quads);
    if peak <= MAX_VIEWPORT_OVERLAY {
        return;
    }
    for q in quads {
        q.color = scale_rgb_floor(q.color, MAX_VIEWPORT_OVERLAY, peak);
    }
}

fn peak_additive_channel(quads: &[GlowQuad]) -> u32 {
    let mut peak = 0u32;
    for x_edge in quads {
        let px = u32::from(x_edge.x);
        for y_edge in quads.iter().filter(|q| q.row == x_edge.row) {
            let py = u32::from(y_edge.y);
            let mut sum = [0u32; 3];
            for q in quads.iter().filter(|q| q.row == x_edge.row) {
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
    peak
}

#[cfg(test)]
fn max_channel(rgb: u32) -> u32 {
    ((rgb >> 16) & 0xff).max((rgb >> 8) & 0xff).max(rgb & 0xff)
}

fn scale_rgb_floor(rgb: u32, numerator: u32, denominator: u32) -> u32 {
    debug_assert!(denominator > 0);
    let scale = |shift: u32| (((rgb >> shift) & 0xff) * numerator / denominator) << shift;
    scale(16) | scale(8) | scale(0)
}

/// Charge mote `k`'s px position at progress `p` (0..1): a gentle spiral
/// converging on the word center. Shared by the dark additive-quad path and
/// the light Over-deco path so both themes animate the same geometry.
fn charge_mote_pos(env: &SuperEnv, k: usize, p: f32) -> (i32, i32) {
    let s = crate::genome::mix(env.seed ^ (k as u64).wrapping_mul(0xA24B_AED4_963E_E407));
    let ang = (s % 4096) as f32 / 4096.0 * core::f32::consts::TAU + p * 1.8; // gentle spiral
    let r0 = env.r_max * (1.1 + (s >> 13 & 0x3) as f32 * 0.15);
    let r = r0 * (1.0 - p * p); // accelerating convergence
    (
        env.cx + (ang.cos() * r) as i32,
        env.cy + (ang.sin() * r * 0.6) as i32,
    )
}

/// The 8-point star crown, additive (dark themes): 4 axes (2 orthogonal + 2
/// diagonal), 3 tapered segments per point ⇒ ≤ 8 × 3 + 2 = 26 ≤ 50 quads.
fn emit_crown_quads(sink: &mut QuadSink<'_>, env: &SuperEnv, env_a: f32) {
    let reach = env.r_max; // ~6 rows
    let core = premul(0x00FF_FFFF, env_a);
    let ch = env.cell_h;
    // Core block.
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
fn emit_ring_chords(sink: &mut QuadSink<'_>, env: &SuperEnv, r: f32, thick: f32, premul: u32) {
    if r <= 1.0 {
        return;
    }
    let ro = r + 0.5 * thick;
    let ri = (r - 0.5 * thick).max(0.0);
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
        let ym = 0.5 * (y0 + y1) - env.cy as f32; // band center, ring coords
        // Vertical squash 0.75 keeps the wave reading as a blast on wide
        // grids (rows are taller than they are many).
        let dy = ym / 0.75;
        if dy.abs() >= ro {
            continue;
        }
        let xo = (ro * ro - dy * dy).sqrt();
        let xi = if dy.abs() < ri {
            (ri * ri - dy * dy).sqrt()
        } else {
            0.0
        };
        let (y0i, y1i) = (
            y0.round() as i32,
            (y1.round() as i32).max(y0.round() as i32 + 1),
        );
        if xi > 1.0 {
            // Two chords: the annulus edges.
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

/// Emit one supernova frame's `WordDecoration` stream (Over veil / dark crown
/// / ring fringe on light themes, rainbow debris on both), respecting `cap`
/// (the host passes `MAX_DECORATIONS`).
pub fn emit_super_decos(t_ms: u64, env: &SuperEnv, out: &mut Vec<WordDecoration>, cap: usize) {
    let out_start = out.len();
    if env.light && t_ms < CHARGE_END_MS {
        // Light-theme charge: the converging motes ride the Over deco stream
        // in the deep candy tones (`v = 0.62` family — the same tones the
        // light rainbow ink resolves to) — additive motes are a no-op over
        // white. ≤ [`CHARGE_MOTES`] decos, counted against the shared `cap`.
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
        // THE ECLIPSE (light bg): an Over-blend dark veil of per-cell
        // `DecoGlyph::Shade` stamps (§3.3) over a ~7-row region around the
        // word (≤ 200 cells), plus a dark crown along the star axes.
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
            let r = r0 + dr;
            if r < 0 {
                continue;
            }
            for dc in 0..width as i32 {
                if out.len() >= cap || stamped >= MAX_VEIL_CELLS {
                    break;
                }
                // Design-director round: RADIAL (elliptical) falloff from the
                // word center — the uniform row-feathered rectangle read as a
                // gray selection block, not an eclipse. Alpha peaks over the
                // word and blooms out to nothing before the rect edge, so the
                // veil has no hard rim on any side; sub-threshold corner
                // stamps are skipped entirely (fewer decos, same budget pin).
                let ny = (dr - rows_span / 2) as f32 / (rows_span as f32 / 2.0);
                let nx = (c0 + dc - word_mid) as f32 / (width as f32 / 2.0);
                let d2 = (nx * nx + ny * ny).min(1.0);
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
        // Dark crown: stamps along the 8 star axes.
        for point in 0..8 {
            let ang = core::f32::consts::TAU * point as f32 / 8.0;
            for seg in 1..=3 {
                if out.len() >= cap {
                    break;
                }
                let d = env.r_max * 0.3 * seg as f32;
                let col = ((env.cx + (ang.cos() * d) as i32) / env.cell_w.max(1))
                    .clamp(0, i32::from(env.cols) - 1);
                let row = ((env.cy + (ang.sin() * d * 0.75) as i32) / env.cell_h.max(1)).max(0);
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
        // Ring dark fringe: Over stamps along the outer circumference.
        let q = (t_ms - DETONATION_END_MS) as f32 / (SHOCK_END_MS - DETONATION_END_MS) as f32;
        let r = env.r_max * ease_out(q);
        let fade = 1.0 - q;
        let n = 16usize;
        for k in 0..n {
            if out.len() >= cap {
                break;
            }
            let ang = core::f32::consts::TAU * k as f32 / n as f32;
            let col = ((env.cx + (ang.cos() * r) as i32) / env.cell_w.max(1))
                .clamp(0, i32::from(env.cols) - 1);
            let row = (env.cy + (ang.sin() * r * 0.75) as i32) / env.cell_h.max(1);
            if row < 0 {
                continue;
            }
            out.push(WordDecoration {
                row: row as u16,
                col: col as u16,
                dx: 0,
                dy: 0,
                glyph: DecoGlyph::Shade,
                blend: DecoBlend::Over,
                color: VEIL_TONE,
                alpha: (170.0 * fade * env.intensity) as u8,
            });
        }
    }
    if (DEBRIS_START_MS..SUPER_TOTAL_MS).contains(&t_ms) {
        // Rainbow debris: 24–40 hue-cycled motes, twinkle LOCKED to the shared
        // 350 ms grid (2 phases ⇒ region onsets ≤ 3/s, the WCAG floor).
        let n = 24 + (env.seed % 17) as usize; // 24..=40
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
            // Slight ballistic sag.
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
            let tw = if grid_phase == 0 { 1.0 } else { 0.55 };
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
                // Additive debris is a no-op over white: light themes blend
                // the deep-candy motes Over instead (dark stays additive —
                // byte-identical to the pre-branch stream).
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

/// Bound the eclipse's full-cell source-over opacity at each host cell.
/// `Shade` is cell-confined and every supernova Shade has zero jitter, so
/// `(row, col)` is an exact overlap class.  The stream is capped at 224 shade
/// stamps.  One frame-wide scale, based on the worst exact `(row, col)` overlap,
/// preserves the radial veil falloff; the allocation-free quadratic scan runs
/// only during the 300 ms light-theme detonation.
fn bound_shade_opacity(decos: &mut [WordDecoration]) {
    let is_shade = |d: &WordDecoration| {
        matches!(d.blend, DecoBlend::Over) && matches!(d.glyph, DecoGlyph::Shade)
    };
    let peak = decos
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
        .unwrap_or(0);
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

    /// FIX 9 regression: [`r_max_for`] IS the engine clamp —
    /// `min(6 rows, grid_h / 2)` with the 1 px floor — NOT the parity suite's
    /// former `min(grid_w, grid_h)`, which admitted radii the engine can
    /// never produce (a shockwave never reaches past half the grid height).
    #[test]
    fn r_max_for_matches_engine_clamp() {
        // Tall grid: the 6-row reach wins (64 rows: grid_h/2 = 32 rows).
        assert_eq!(r_max_for(20, 64 * 20), 6.0 * 20.0);
        // Short grid (8 rows): the grid_h/2 clamp binds at 4 rows — the case
        // the old min(grid_w, grid_h) formula let escape to 6 rows.
        assert_eq!(r_max_for(20, 8 * 20), 4.0 * 20.0);
        // Degenerate metrics floor at 1 px (the engine's .max(1.0)).
        assert_eq!(r_max_for(0, 0), 1.0);
    }

    /// §3.2 structural budget pin: closed-form `S_max ≤ 900` at
    /// `ch ∈ {14, 20, 40, 56}` over EVERY 10 ms step of the window, both
    /// themes, at both 64 and the closed form's 160 viewport rows — and the
    /// budgeted emission is byte-identical to unbounded (the cap never binds
    /// on a reachable frame). Plain `#[test]` — no SMT claim (§3.2).
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
                        // Host-side selection splits multiply wash rows ≤ 3×;
                        // the closed form charges that worst case here.
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
                        // Every quad confined to one cell-row band.
                        for q in &capped {
                            assert!(i32::from(q.h) <= ch, "band split at t={t}");
                            assert_eq!(i32::from(q.y) / ch, i32::from(q.row));
                        }
                    }
                }
            }
        }
    }

    /// The deco stream stays inside the host cap with debris headroom: veil
    /// ≤ 200 cells, light charge ≤ 12, debris ≤ 40 — never above 256 in one
    /// phase. Every dark-veil stamp (veil / crown / ring fringe — the
    /// `VEIL_TONE` family) carries the §3.3 `DecoGlyph::Shade` mask and Over
    /// blend; the light-theme charge/debris motes are Over too (additive is
    /// a no-op over white) but keep their mote glyphs. Dark themes emit NO
    /// Over decos at all (their deco stream is the additive debris only).
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
                    // §3.3: the dark-veil tone family is exactly the Shade
                    // stamps, and Shade only ever blends Over.
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
        let t = 500u64; // mid-detonation
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

    /// Captured-frame regression (8 rapid `fuc fuck kitty` lines): one rolled
    /// supernova used to put a 90%-white full-viewport quad in `nova_add`, then
    /// stack its crown on top.  The renderer's One/One blend faithfully turned
    /// that into a near-white frame even though the burst mutex admitted only
    /// one episode.  Pin both theme branches at the exact phase peak and retain
    /// negative controls proving that each limiter actually binds.
    #[test]
    fn detonation_has_a_hard_aggregate_readability_ceiling() {
        let t = 500; // sin(pi * 0.5): the exact detonation peak

        // Negative control: the pre-limiter center wash alone contributed 230
        // channel levels, before any crown overlap.
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

        // On the shipping default dark palette, even the deliberately
        // conservative neutral worst case (all three channels receive the
        // whole bound) leaves normal text well above WCAG AA contrast.
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

        // Negative control for the light branch: its central full-cell Shade
        // used alpha 235.  The final stream keeps the entire overlap class at
        // no more than 64, including duplicate crown stamps on a veil cell.
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

        // The cap is scoped to viewport-covering Shade stamps.  The tiny
        // rainbow debris keeps its bright sparkle detail.
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

    /// The limiter is one frame-wide scale, not one normalization per row.
    /// This pins the visual-review fix for a flat charcoal screen with darker
    /// crown bands: falloff ratios survive.  The crossed rectangles are also a
    /// negative control for the overlap sweep — their hottest point combines
    /// the x-start of one with the y-start of the other and is not either
    /// rectangle's own top-left corner.
    #[test]
    fn overlap_bound_preserves_falloff_and_finds_crossed_edges() {
        let quad = |row, x, y, w, h, color| GlowQuad {
            row,
            x,
            y,
            w,
            h,
            color,
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

    /// FIX I regression: on a viewport TALLER than [`MAX_WASH_ROWS`], the
    /// detonation wash window centers on the blast row (clamped to the grid)
    /// instead of anchoring at row 0 — a bottom-of-screen detonation must
    /// wash the word's own row. Row count stays `min(vrows, 160)` (the
    /// S_max closed form is unchanged).
    #[test]
    fn wash_window_centers_on_blast_row() {
        let rows = 200i32; // > MAX_WASH_ROWS = 160
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
        // Window [40, 200): centered on 190 then clamped to the grid bottom.
        assert!(
            wash_rows.iter().all(|r| (40..rows).contains(r)),
            "window clamps inside the grid"
        );
        // A ≤160-row viewport still washes every row from 0 (byte-identical
        // to the pre-fix emission — the window only moves when it must).
        let e = env(20, false, 64);
        let mut q = Vec::new();
        let c = emit_super(500, &e, MAX_SUPER_QUADS_PER, &mut q);
        assert_eq!(c.wash, 64);
        assert_eq!(i32::from(q[0].row), 0);
    }

    /// FIX II regression: light themes emit the charge motes and rainbow
    /// debris as `DecoBlend::Over` decorations in the deep candy tones
    /// (`v = 0.62` family) — additive blending is a no-op over white. The
    /// dark theme keeps its additive quads/decos byte-identically.
    #[test]
    fn light_charge_and_debris_ride_over_stream() {
        let (dark, light) = (env(20, false, 64), env(20, true, 64));
        // Charge (t = 200): light emits NO additive quads; ≤ 12 Over decos.
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
        // Debris (t = 2000): Over on light, Add on dark; same mote count.
        let probe = |e: &SuperEnv| {
            let mut d = Vec::new();
            emit_super_decos(2000, e, &mut d, 256);
            d
        };
        let (ldeb, ddeb) = (probe(&light), probe(&dark));
        assert!(!ldeb.is_empty() && ldeb.len() == ddeb.len());
        assert!(ddeb.iter().all(|m| matches!(m.blend, DecoBlend::Add)));
        // Every light mote (charge + debris) is Over in a deep candy tone:
        // v = 0.62 caps every channel at round(0.62 · 255) = 158 — never the
        // white-invisible additive family.
        for m in ld.iter().chain(&ldeb) {
            assert!(matches!(m.blend, DecoBlend::Over), "light motes blend Over");
            let max_ch = (0..3).map(|i| (m.color >> (8 * i)) & 0xff).max().unwrap();
            assert!(
                (1..=158).contains(&max_ch),
                "deep candy tone (v = 0.62): {:06x}",
                m.color
            );
        }
    }

    /// Pure-emitter determinism + phase windows: same (t, env) ⇒ identical
    /// bytes; charge/detonation/shockwave/debris live only in their windows.
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
        assert!(d.len() >= 20 && d.len() <= 44, "24..=40 motes: {}", d.len());
        let (_, q, d) = probe(SUPER_TOTAL_MS);
        assert!(q.is_empty() && d.is_empty(), "afterglow emits nothing here");
    }
}
