// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shared pure-integer procedural fire field behind `FirePatch`.
//!
//! [`FireRow`] samples the field along a scanline and [`fire_shade_add`] /
//! [`fire_shade_over`] turn each sample into premultiplied additive light or straight ink
//! plus alpha, using only `i32`/`u32` ops: no state, floats or tables. aterm-gpu's
//! `fs_fire_add`/`fs_fire_over` shaders (WGSL in `renderer.rs`, Metal in `cell.metal`)
//! mirror it op for op, so CPU and GPU produce identical bytes
//! (`aterm-gpu/tests/fire_patch_parity.rs`).
//!
//! Field anatomy (fixed point, 8 fractional bits unless noted):
//!
//! * Silhouette ridge: per-column tongue height from 2-octave value noise advected in time
//!   (`phase`), folded (`255 − |2n − 255|`) and squared into pointed crests.
//! * Upward advection: body noise samples at `v − phase·rise_speed`, so pockets change as
//!   they rise; rise and churn speed up with `temp`.
//! * Body: three octaves (1×, 2×, 4×) modulate palette index and coverage.
//! * Lean shear: the sample column drifts `lean/4` px per full rise.
//! * AA: coverage rolls off over ~`cell_h/4` px inside the silhouette, and a root ramp
//!   lifts the flame off its fuel line.
//! * Palette: a 5-anchor black-body LUT interpolated over 1024 steps; white only at a hot
//!   root, tips die into deep red. [`FireMode::Over`](crate::render_api::FireMode::Over)
//!   uses a red-brown ink ramp that reads on white.
//!
//! Time-advected lattice axes are masked so the `u32` ring wraps onto itself
//! (`(phase·speed) >> 10` spans `2^22` fp8 units). The field is a pure function of absolute
//! window coordinates, so patches sharing a burn's parameters are continuous across patch
//! boundaries.

/// Per-patch field parameters in window pixel space (pad already applied), built the same
/// way by both backends from a `FirePatch`.
#[derive(Clone, Copy, Debug)]
pub struct FireFieldParams {
    /// Flame root y in window px (field `v = base_y − py`; flames rise upward).
    pub base_y: i32,
    /// Max flame height in px (the tongue envelope); clamped to `1..=2048`.
    pub peak_h: i32,
    /// Churn phase, units of 1/1024 s (producer-quantized).
    pub phase: u32,
    /// Display temperature `0..=255`.
    pub temp: i32,
    /// Per-cell envelope `0..=255`.
    pub strength: i32,
    /// Horizontal shear: `lean/4` px drift at full rise. `-128..=127`.
    pub lean: i32,
    /// Coverage/alpha ceiling `0..=255`.
    pub cov_cap: i32,
    /// Cell height in px (the field's spatial unit); clamped to `>= 2`.
    pub cell_h: i32,
    /// Top-edge fade anchor: the window-px y of the grid top (the render pad). Coverage is
    /// 0 there and full two cells below ([`fire_top_fade`]); anchored at the grid top, not
    /// framebuffer 0, so a letterboxed window fades at its visible edge.
    pub top_fade_y: i32,
}

// Hash / mix constants (splitmix-style avalanche over lattice coordinates).
const FIRE_P0: u32 = 0x9E37_79B1;
const FIRE_P1: u32 = 0x85EB_CA77;
const FIRE_P2: u32 = 0xC2B2_AE3D;
const FIRE_M0: u32 = 0x2C1B_3C6D;
const FIRE_M1: u32 = 0x297A_2D39;

// Per-octave seeds: ridge (2), body (3).
const FIRE_SEED_R0: u32 = 0x51F0_A3B7;
const FIRE_SEED_R1: u32 = 0x9D2C_5680;
const FIRE_SEED_B0: u32 = 0xB529_7A4D;
const FIRE_SEED_B1: u32 = 0x68E3_1DA4;
const FIRE_SEED_B2: u32 = 0x1B56_C4E9;

/// Legibility ceiling on the palette index (`0..=1023`). The ramp's top is near-white
/// (`0xFFF0C0`), which makes white glyphs vanish; capping here keeps the hottest pixel
/// amber-gold (`~0xFFCF6E`). Both GPU shaders repeat the literal `850`.
const FIRE_IDX_MAX: i32 = 850;

/// Integer lattice hash → 32 avalanche bits, all wrapping `u32` so the shader agrees
/// bit-for-bit.
#[inline]
#[must_use]
pub fn fire_hash(x: u32, y: u32, seed: u32) -> u32 {
    let mut h = x
        .wrapping_mul(FIRE_P0)
        .wrapping_add(y.wrapping_mul(FIRE_P1))
        .wrapping_add(seed.wrapping_mul(FIRE_P2));
    h ^= h >> 15;
    h = h.wrapping_mul(FIRE_M0);
    h ^= h >> 12;
    h = h.wrapping_mul(FIRE_M1);
    h ^= h >> 15;
    h
}

/// Fixed-point smoothstep: `t` in `0..=255` → `t²(3·256 − 2t) >> 16`, C¹ at lattice lines.
#[inline]
#[must_use]
pub fn fire_fade(t: i32) -> i32 {
    (t * t * (768 - 2 * t)) >> 16
}

/// 2-D integer value noise at fp8 `(x, y)` → `0..=255`. `ymask` masks the time-advected Y
/// lattice index so the `2^22` ring wraps seamlessly (`0x3FFF`/`0x7FFF`/`0xFFFF` for
/// 1×/2×/4×); X is spatial and unmasked.
#[inline]
#[must_use]
pub fn fire_vnoise(x: u32, y: u32, ymask: u32, seed: u32) -> i32 {
    let ix = x >> 8;
    let iy0 = (y >> 8) & ymask;
    let iy1 = (iy0 + 1) & ymask;
    let ix1 = ix.wrapping_add(1);
    let fx = (x & 255) as i32;
    let fy = (y & 255) as i32;
    let n00 = (fire_hash(ix, iy0, seed) >> 24) as i32;
    let n10 = (fire_hash(ix1, iy0, seed) >> 24) as i32;
    let n01 = (fire_hash(ix, iy1, seed) >> 24) as i32;
    let n11 = (fire_hash(ix1, iy1, seed) >> 24) as i32;
    let ux = fire_fade(fx);
    let uy = fire_fade(fy);
    let a = n00 * (256 - ux) + n10 * ux;
    let b = n01 * (256 - ux) + n11 * ux;
    (a * (256 - uy) + b * uy) >> 16
}

/// The field decomposition at one window pixel, shared by both emitters. A dead pixel is
/// all-zero with `q = 256`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FireCore {
    /// Palette index `0..=1023`.
    pub idx: i32,
    /// Relative height in the local tongue, `0` root → `256` tip.
    pub q: i32,
    /// Anti-aliased silhouette coverage `0..=256`.
    pub edge: i32,
    /// Interior turbulence `0..=255`.
    pub body: i32,
    /// Root lift ramp `0..=256`.
    pub root: i32,
    /// Rim proximity `0..=256` (256 at the outline, 0 deep inside): the cross-section cue,
    /// a cooler sheath on Add, pooled ink on Over.
    pub rim: i32,
}

const FIRE_DEAD: FireCore = FireCore {
    idx: 0,
    q: 256,
    edge: 0,
    body: 0,
    root: 0,
    rim: 0,
};

/// The patch-constant field terms (from cell_h, peak_h, phase, temp), computed once per
/// patch by [`fire_precomp`]. A CPU-only hoist: the shader recomputes them per fragment
/// and the bytes are identical.
#[derive(Clone, Copy, Debug)]
pub struct FirePrecomp {
    ch: i32,
    chu: u32,
    peak: i32,
    tr: u32,
    tr2: u32,
    aa: i32,
    offr: u32,
}

/// Compute the patch-constant terms of the field once per FirePatch.
#[inline]
#[must_use]
pub fn fire_precomp(p: &FireFieldParams) -> FirePrecomp {
    let ch = p.cell_h.max(2);
    let ts = (300 + p.temp * 2) as u32;
    let tr = p.phase.wrapping_mul(ts) >> 10;
    let rs = (350 + p.temp) as u32;
    let offr = p.phase.wrapping_mul(rs) >> 10;
    FirePrecomp {
        ch,
        chu: ch as u32,
        peak: p.peak_h.clamp(1, 2048),
        tr,
        tr2: tr.wrapping_mul(2).wrapping_add(37199),
        aa: (ch / 4).max(3) * 256,
        offr,
    }
}

/// One value-noise octave cached along a left-to-right sweep of a fixed row. The y side is
/// row-constant and the x lattice index only advances, so a step shifts `n10→n00`,
/// `n11→n01` and hashes the two trailing corners: about `2/lattice-period` hashes per pixel
/// instead of [`fire_vnoise`]'s 4. `x` must be non-decreasing across calls.
#[derive(Clone, Copy, Debug)]
struct FireOctave {
    seed: u32,
    iy0: u32,
    iy1: u32,
    uy: i32,
    cur_ix: u32,
    n00: i32,
    n10: i32,
    n01: i32,
    n11: i32,
}

impl FireOctave {
    #[inline]
    fn new(y: u32, ymask: u32, seed: u32, x0: u32) -> Self {
        let iy0 = (y >> 8) & ymask;
        let iy1 = (iy0 + 1) & ymask;
        let uy = fire_fade((y & 255) as i32);
        let ix = x0 >> 8;
        Self {
            seed,
            iy0,
            iy1,
            uy,
            cur_ix: ix,
            n00: (fire_hash(ix, iy0, seed) >> 24) as i32,
            n10: (fire_hash(ix.wrapping_add(1), iy0, seed) >> 24) as i32,
            n01: (fire_hash(ix, iy1, seed) >> 24) as i32,
            n11: (fire_hash(ix.wrapping_add(1), iy1, seed) >> 24) as i32,
        }
    }

    /// Sample at fp8 `x` (`>=` the previous call's); equal to
    /// `fire_vnoise(x, y, ymask, seed)`.
    #[inline]
    fn sample(&mut self, x: u32) -> i32 {
        let ix = x >> 8;
        while self.cur_ix < ix {
            self.n00 = self.n10;
            self.n01 = self.n11;
            self.cur_ix = self.cur_ix.wrapping_add(1);
            let nx = self.cur_ix.wrapping_add(1);
            self.n10 = (fire_hash(nx, self.iy0, self.seed) >> 24) as i32;
            self.n11 = (fire_hash(nx, self.iy1, self.seed) >> 24) as i32;
        }
        let ux = fire_fade((x & 255) as i32);
        let a = self.n00 * (256 - ux) + self.n10 * ux;
        let b = self.n01 * (256 - ux) + self.n11 * ux;
        (a * (256 - self.uy) + b * self.uy) >> 16
    }
}

/// The shared field sampler: walks one scanline left to right (`px` non-decreasing) and
/// returns the [`FireCore`] both emitters build on. Total for any input. The five octaves
/// use `FireOctave` caches instead of per-pixel [`fire_vnoise`] calls; the interpolant is
/// the same, so every sample matches the shader's `fire_core`.
pub struct FireRow<'a> {
    p: &'a FireFieldParams,
    pc: &'a FirePrecomp,
    /// Root-lift ramp from the height above the root (row-constant).
    root: i32,
    /// Height above the root in fp8 (row-constant).
    vv: i32,
    /// `lean·vn/4` shear (row-constant).
    shear: i32,
    /// The 5 octaves: ridge n0/n1, body m0/m1/m2.
    n0: FireOctave,
    n1: FireOctave,
    m0: FireOctave,
    m1: FireOctave,
    m2: FireOctave,
}

impl<'a> FireRow<'a> {
    /// Set up the scanline at `py` with first sample column `x_start` (device px), priming
    /// the row-constant terms and octave caches.
    #[must_use]
    pub fn new(py: i32, x_start: i32, p: &'a FireFieldParams, pc: &'a FirePrecomp) -> Self {
        // v: height above the root. Below it the envelope mirrors 5x-compressed, an ember
        // bed that dissolves into the glyph row instead of a hard horizontal cut; it draws
        // in the under-ink fire slot, so glyphs still paint over it.
        let v = p.base_y - py;
        let v = if v >= 0 { v } else { -v * 5 };
        let vv = v * 256;
        // vn: rise fraction of the envelope (fp8, capped at 2×). The shear drifts lean/4 px
        // at full rise.
        let vn = ((v * 256) / pc.peak).min(512);
        let shear = p.lean * vn / 4;
        // The first sheared fp8 x, where each octave primes its cache.
        let xq = (x_start * 256 + shear + (1 << 22)) as u32;
        let sx = (xq * 4) / (5 * pc.chu);
        // Body advection axis: one lattice cell per 1.5·cell_h px (tongues are taller than
        // wide), sampled at v − phase·rise so pockets change as they rise.
        let sy = ((vv as u32) * 2) / (3 * pc.chu);
        let by = sy.wrapping_sub(pc.offr);
        Self {
            p,
            pc,
            root: ((v * 1536) / pc.ch).min(256),
            vv,
            shear,
            n0: FireOctave::new(pc.tr, 0x3FFF, FIRE_SEED_R0, sx),
            n1: FireOctave::new(pc.tr2, 0x7FFF, FIRE_SEED_R1, sx * 2 + 12799),
            m0: FireOctave::new(by, 0x3FFF, FIRE_SEED_B0, sx * 3 / 2 + 5023),
            m1: FireOctave::new(
                by.wrapping_mul(2).wrapping_add(15913),
                0x7FFF,
                FIRE_SEED_B1,
                sx * 3 + 9531,
            ),
            m2: FireOctave::new(
                by.wrapping_mul(4).wrapping_add(37633),
                0xFFFF,
                FIRE_SEED_B2,
                sx * 6 + 26251,
            ),
        }
    }

    /// Sample the field at device column `px` (non-decreasing).
    #[must_use]
    pub fn core(&mut self, px: i32) -> FireCore {
        let p = self.p;
        let pc = self.pc;

        // Sheared window-px fp8 x, offset +16384 px so lattice math stays positive; one
        // lattice cell per 1.25·cell_h px.
        let xq = (px * 256 + self.shear + (1 << 22)) as u32;
        let sx = (xq * 4) / (5 * pc.chu);

        // Silhouette ridge: 2-octave noise over (column, churn time), folded and squared.
        let n0 = self.n0.sample(sx);
        let n1 = self.n1.sample(sx * 2 + 12799);
        let n = (n0 * 9 + n1 * 7) >> 4;
        let ridge = 255 - (2 * n - 255).abs();
        // Exponent ~2.5 (square blended with cube): pointed crests over wide valleys.
        let hs2 = (ridge * ridge) >> 8;
        let hshape = (hs2 * (256 + ridge)) >> 9;
        // Tongue height, fp8 px: a ~12% floor keeps dark valleys between tongues; crests
        // reach ~1.12·peak; scaled by strength.
        let hq = 30 + ((hshape * 260) >> 8);
        let hcol = (pc.peak * hq * p.strength) / 255;

        let vv = self.vv;
        let d = hcol - vv;
        if d <= 0 {
            return FIRE_DEAD;
        }
        // Silhouette AA: coverage ramps over ~cell_h/4 px inside the edge.
        let edge = ((d * 256) / pc.aa).min(256);
        // RIM: 256 at the outline, fading to 0 by 2·aa inside — the sheath band.
        let rim = ((2 * pc.aa - d) * 256 / pc.aa).clamp(0, 256);
        // q: relative height inside the local tongue (0 root → 255 tip).
        let q = (vv * 256) / hcol;

        // Interior body: three upward-advected octaves, sampled for live pixels only.
        let m0 = self.m0.sample(sx * 3 / 2 + 5023);
        let m1 = self.m1.sample(sx * 3 + 9531);
        let m2 = self.m2.sample(sx * 6 + 26251);
        let body0 = (m0 * 4 + m1 * 3 + m2) >> 3;
        // Turbulence flattens toward neutral across the AA fringe, keeping outlines clean.
        let body = 128 + (((body0 - 128) * edge) >> 8);

        // Palette index: heat falls root → tip, reaches whiter when hot, modulated by the
        // body turbulence.
        let heat = ((256 - q) * (112 + ((p.temp * 120) >> 8))) >> 6;
        let idx = ((heat * (150 + ((body * 212) >> 8))) >> 8).clamp(0, FIRE_IDX_MAX);

        // Root lift over cell_h/6 px, slightly cooling the extreme root (an amber fuel
        // line, not a white strip).
        let root = self.root;
        let idx = (idx * (192 + (root >> 2))) >> 8;
        FireCore {
            idx,
            q,
            edge,
            body,
            root,
            rim,
        }
    }
}

// Black-body anchors (aterm-effects' `fire_ramp` colours): deep red → near-white.
const FIRE_PAL_ADD: [[i32; 3]; 5] = [
    [0x2A, 0x00, 0x00],
    [0x8B, 0x1A, 0x00],
    [0xE0, 0x4A, 0x00],
    [0xFF, 0xB0, 0x20],
    [0xFF, 0xF0, 0xC0],
];

// Ink palette (FireMode::Over): red-browns and burnt oranges that read on white.
const FIRE_PAL_OVER: [[i32; 3]; 5] = [
    [0x66, 0x18, 0x04],
    [0xA8, 0x28, 0x00],
    [0xD0, 0x46, 0x00],
    [0xEE, 0x6E, 0x00],
    [0xFF, 0xA6, 0x1E],
];

/// Piecewise-linear palette fetch: `idx 0..=1023` across 4 segments of 256.
#[inline]
fn fire_pal(pal: &[[i32; 3]; 5], idx: i32) -> (i32, i32, i32) {
    let seg = ((idx >> 8).clamp(0, 3)) as usize;
    let f = idx - ((seg as i32) << 8);
    let a = pal[seg];
    let b = pal[seg + 1];
    let mix = |x: i32, y: i32| (x * (256 - f) + y * f) >> 8;
    (mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2]))
}

/// The [`FireMode::Add`](crate::render_api::FireMode::Add) coverage law: silhouette AA ×
/// temp density × interior pockets × root lift, capped. `0..=cov_cap`.
#[inline]
#[must_use]
pub fn fire_cov_add(c: &FireCore, p: &FireFieldParams) -> i32 {
    let dens = 150 + ((p.temp * 106) >> 8);
    let bodyc = 110 + ((c.body * 146) >> 8);
    ((((((c.edge * dens) >> 8) * bodyc) >> 8) * c.root) >> 8).min(p.cov_cap)
}

/// The [`FireMode::Over`](crate::render_api::FireMode::Over) alpha law: the Add shaping
/// with a gentler interior grain, rim pooling and a root→tip fade. `0..=cov_cap`.
#[inline]
#[must_use]
pub fn fire_alpha_over(c: &FireCore, p: &FireFieldParams) -> i32 {
    let bodyc = 130 + ((c.body * 166) >> 8);
    let tipf = 120 + (((256 - c.q) * 136) >> 8);
    let pool = 256 + ((c.rim * 96) >> 8);
    let a = (((((((c.edge * bodyc) >> 8) * c.root) >> 8) * tipf) >> 8) * pool) >> 8;
    a.min(p.cov_cap)
}

/// Top-edge fade `0..=255`: `0` at the grid top (`py == top_fade_y`), smoothstepping to
/// `255` two cells below, so a flame on the top row dissipates instead of clipping against
/// the edge. Rows lower down get `255`, no effect.
#[inline]
#[must_use]
pub fn fire_top_fade(py: i32, p: &FireFieldParams) -> i32 {
    let fade_px = (p.cell_h * 2).max(2);
    let t = (((py - p.top_fade_y) * 255) / fade_px).clamp(0, 255);
    fire_fade(t)
}

/// [`FireMode::Add`](crate::render_api::FireMode::Add) emission for one [`FireRow`]
/// sample: premultiplied `0x00RRGGBB` light for `add_sat` (`(c·cov + 127)/255` per
/// channel, the single rounding point, as in `fs_fire_add`); `0` outside the flame. `tf`
/// is the row's [`fire_top_fade`]; `tf == 255` skips the multiply, which is exact.
#[inline]
#[must_use]
pub fn fire_shade_add(c: &FireCore, p: &FireFieldParams, tf: i32) -> u32 {
    let cov0 = fire_cov_add(c, p);
    let cov = if tf == 255 { cov0 } else { (cov0 * tf) / 255 };
    if cov <= 0 {
        return 0;
    }
    // Rim cooling: the outline drops toward deep red while the core stays hot.
    let idx = (c.idx * (256 - ((c.rim * 112) >> 8))) >> 8;
    let (r, g, b) = fire_pal(&FIRE_PAL_ADD, idx);
    let m = |ch: i32| ((ch * cov + 127) / 255) as u32;
    (m(r) << 16) | (m(g) << 8) | m(b)
}

/// [`FireMode::Over`](crate::render_api::FireMode::Over) emission for one [`FireRow`]
/// sample: `(straight 0x00RRGGBB ink, alpha)` for `over_rgb` / SrcAlpha-OneMinusSrcAlpha;
/// `(0, 0)` outside. `tf` is the row's top fade, as for [`fire_shade_add`].
#[inline]
#[must_use]
pub fn fire_shade_over(c: &FireCore, p: &FireFieldParams, tf: i32) -> (u32, u8) {
    let a0 = fire_alpha_over(c, p);
    let a = if tf == 255 { a0 } else { (a0 * tf) / 255 };
    if a <= 0 {
        return (0, 0);
    }
    // Rim darkening: ink pools at the outline, a red-brown contour around amber.
    let idx = (c.idx * (256 - ((c.rim * 128) >> 8))) >> 8;
    let (r, g, b) = fire_pal(&FIRE_PAL_OVER, idx);
    (
        ((r as u32) << 16) | ((g as u32) << 8) | (b as u32),
        a.clamp(0, 255) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(temp: i32, lean: i32, phase: u32) -> FireFieldParams {
        FireFieldParams {
            base_y: 200,
            peak_h: 120,
            phase,
            temp,
            strength: 230,
            lean,
            cov_cap: 200,
            cell_h: 40,
            top_fade_y: 0,
        }
    }

    /// The field at one window pixel, through the shipping [`FireRow`] sampler.
    fn core_at(px: i32, py: i32, p: &FireFieldParams) -> FireCore {
        FireRow::new(py, px, p, &fire_precomp(p)).core(px)
    }

    /// Below the root the envelope mirrors 5x-compressed: live just under `base_y`, equal to
    /// the mirrored sample above, and dead a fifth of the peak below.
    #[test]
    fn root_skirt_mirrors_compressed_below_the_root() {
        let p = FireFieldParams {
            base_y: 200,
            peak_h: 60,
            phase: 7,
            temp: 128,
            strength: 220,
            lean: 0,
            cov_cap: 200,
            cell_h: 16,
            top_fade_y: 0,
        };
        for px in [3, 40, 77] {
            // d px below the root samples the envelope 5·d above.
            for d in 1..=4 {
                let below = core_at(px, p.base_y + d, &p);
                let above = core_at(px, p.base_y - 5 * d, &p);
                assert_eq!(below, above, "5x mirror at d={d} px={px}");
            }
            // Dead within peak/5 (+1 for the mirror rounding).
            let deep = core_at(px, p.base_y + p.peak_h / 5 + 2, &p);
            assert_eq!(deep.idx, 0, "skirt extinct below peak/5: px={px}");
        }
    }

    /// No vertically adjacent coverage/alpha step exceeds 0.6·cov_cap. The analytic budget
    /// (AA ramp, root lift, turbulence, rim pool, tip fade and cross terms) is ≈ 0.55·cov_cap
    /// at aa = ch/4, so a full-cap texel edge cannot occur.
    #[test]
    fn aa_law_no_hard_silhouette_edges() {
        let mut worst = 0i32;
        for temp in [51, 128, 230] {
            for phase in [0u32, 8192, 1 << 20] {
                let p = params(temp, -50, phase);
                let bound = (p.cov_cap * 3) / 5; // 120 at cap 200
                for px in 0..200 {
                    let mut prev = 0i32;
                    // Walk tip → root so each column crosses the edge.
                    for py in 40..=200 {
                        let c = core_at(px, py, &p);
                        let cov = fire_cov_add(&c, &p).max(fire_alpha_over(&c, &p));
                        let delta = (cov - prev).abs();
                        assert!(
                            delta <= bound,
                            "cov step {delta} > {bound} at ({px},{py}) temp={temp} phase={phase}"
                        );
                        worst = worst.max(delta);
                        prev = cov;
                    }
                }
            }
        }
        eprintln!("aa law: worst adjacent cov/alpha step = {worst} (bound 120, cap 200)");
    }

    /// The field stays total and in range across the temporal ring wrap.
    #[test]
    fn field_ignores_patch_geometry_and_wraps_seamlessly() {
        // Phases whose advection offsets cross 2^32: pins totality and range.
        for phase in [u32::MAX, u32::MAX / 2, 0] {
            let p = params(255, 127, phase);
            for py in 80..200 {
                for px in 0..64 {
                    let c = core_at(px, py, &p);
                    assert!((0..=1023).contains(&c.idx));
                    assert!((0..=256).contains(&c.q));
                    assert!((0..=p.cov_cap).contains(&fire_cov_add(&c, &p)));
                    assert!((0..=p.cov_cap).contains(&fire_alpha_over(&c, &p)));
                }
            }
        }
    }

    /// Degenerate params (zero peak or cell_h, saturated values) neither panic nor overflow.
    #[test]
    fn field_is_total_on_degenerate_params() {
        let degens = [
            FireFieldParams {
                base_y: 0,
                peak_h: 0,
                phase: u32::MAX,
                temp: 255,
                strength: 255,
                lean: -128,
                cov_cap: 255,
                cell_h: 0,
                top_fade_y: 0,
            },
            FireFieldParams {
                base_y: 65535 + 64,
                peak_h: 65535,
                phase: u32::MAX,
                temp: 255,
                strength: 255,
                lean: 127,
                cov_cap: 255,
                cell_h: 65535,
                top_fade_y: 0,
            },
            FireFieldParams {
                base_y: 100,
                peak_h: 1,
                phase: 0,
                temp: 0,
                strength: 0,
                lean: 0,
                cov_cap: 0,
                cell_h: 2,
                top_fade_y: 0,
            },
        ];
        for p in &degens {
            let pc = fire_precomp(p);
            for py in 0..80 {
                let tf = fire_top_fade(py, p);
                for (lo, hi) in [(0, 65), (16300, 16384)] {
                    let mut row = FireRow::new(py, lo, p, &pc);
                    for px in lo..hi {
                        let c = row.core(px);
                        let _ = fire_shade_add(&c, p, tf);
                        let _ = fire_shade_over(&c, p, tf);
                    }
                }
            }
        }
    }

    /// A hot patch is substantially lit, its root reaches the amber/white range, and its
    /// tips stay cooler than its root.
    #[test]
    fn field_burns_with_structure() {
        let p = params(230, 0, 4096);
        let mut lit = 0usize;
        let mut root_idx_max = 0i32;
        let mut tip_idx_max = 0i32;
        for px in 0..300 {
            for py in 60..200 {
                let c = core_at(px, py, &p);
                if fire_cov_add(&c, &p) > 0 {
                    lit += 1;
                    if c.q < 48 {
                        root_idx_max = root_idx_max.max(c.idx);
                    }
                    if c.q > 208 {
                        tip_idx_max = tip_idx_max.max(c.idx);
                    }
                }
            }
        }
        assert!(
            lit > 4000,
            "a hot 300px burn must be substantially lit ({lit})"
        );
        assert!(
            root_idx_max > 700,
            "the root must reach the amber/white palette range ({root_idx_max})"
        );
        assert!(
            tip_idx_max < root_idx_max,
            "tips must stay cooler than the root ({tip_idx_max} vs {root_idx_max})"
        );
    }
}
