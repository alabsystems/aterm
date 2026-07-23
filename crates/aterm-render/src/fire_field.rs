// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! EMBERFORGE **FirePatch** — the shared, per-pixel, PURE-INTEGER procedural
//! fire field (the [`RainHalo`](aterm_render_api::RainHalo) parity trick at
//! full art scale).
//!
//! [`fire_field_add`] / [`fire_field_over`] map `(window_px, window_py, patch
//! params)` to the final composited operands — a premultiplied additive light
//! colour, or a straight ink colour + alpha — using ONLY `i32`/`u32` ops with
//! explicit shifts: no state, no floats, no tables the two backends could
//! load differently. The GPU `fs_fire_add`/`fs_fire_over` WGSL in
//! `aterm-gpu` mirrors this module OP-FOR-OP, so the CPU rasterizer and the
//! GPU fragment shader compute the IDENTICAL byte at every pixel (pinned by
//! `aterm-gpu/tests/fire_patch_parity.rs`, delta 0).
//!
//! Field anatomy (all quantities fixed-point, 8 fractional bits unless said):
//!
//! * **Silhouette ridge** — a per-column tongue height from 2-octave integer
//!   value noise (lattice hash → smoothstep bilinear), advected sideways in
//!   TIME (`phase`) and folded (`255 − |2n − 255|`) then squared into narrow,
//!   pointed licking crests over wide valleys.
//! * **Upward advection** — the interior body noise samples at
//!   `v_scaled − phase·rise_speed`, so turbulent pockets brighten/darken AS
//!   THEY RISE; rise and churn speeds accelerate with `temp`.
//! * **Body + grain** — three interior octaves (1×, 2×, 4× on the advected
//!   axis) modulate both the palette index and the coverage: the flame is
//!   never a gradient fill and never a flat slab.
//! * **Lean shear** — the sample column drifts `lean/4` px per full rise, so
//!   tongues drag against the typing direction.
//! * **Anti-aliased everywhere** — coverage rolls off linearly over
//!   `~cell_h/5` px inside the silhouette (lateral softness follows because
//!   the ridge is continuous in x), and a thin root ramp lifts the flame off
//!   its fuel line (no hard bottom edge).
//! * **Black-body palette** — a 5-anchor integer LUT (the shipped
//!   `fire_ramp` colours), piecewise-linearly interpolated over 1024 steps;
//!   the index is `(1 − v_rel)·temp_reach·body`, so white lives only at a
//!   hot root and tips die into deep red. The [`FireMode::Over`] twin uses a
//!   deep red-brown INK ramp shaped by the same field — flames that read on
//!   white.
//!
//! CONTINUITY / PERIOD: field coordinates live on the wrapping `u32` ring and
//! every time-advected lattice axis is masked so the ring wraps onto itself
//! (`(phase·speed) >> 10` spans exactly `2^22` fp8 units; the 1×/2×/4× lattice
//! masks `0x3FFF`/`0x7FFF`/`0xFFFF` make the wrap seamless). The pattern is
//! hash-driven — it does not visibly repeat, and the exact period is hours,
//! not seconds. Because the field is a pure function of ABSOLUTE window
//! coordinates, adjacent patches sharing a burn's parameters are continuous
//! across patch boundaries: two half-width patches are byte-identical to one
//! wide patch.

/// The per-patch field parameters in WINDOW pixel space (pad already applied
/// by the caller — the same convention as the `RainHalo` falloff basis). Both
/// backends build this from a `FirePatch` the identical way.
#[derive(Clone, Copy, Debug)]
pub struct FireFieldParams {
    /// Flame ROOT y in window px (field `v = base_y − py`; flames rise upward).
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
    /// TOP-EDGE FADE anchor: the window-px y of the GRID TOP (== the render
    /// pad). Coverage dissolves to 0 as a pixel approaches this line and ramps
    /// back to full one cell below it, so a flame licking the top of the
    /// terminal tapers out instead of a hard bright slab. A flame anywhere lower
    /// is untouched. Anchored at the grid top (not framebuffer 0) so a
    /// letterboxed window fades at the visible top edge.
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

/// LEGIBILITY ceiling on the palette index (`0..=1023`). The top of the black-
/// body ramp is near-WHITE (`0xFFF0C0`); a white-hot flame over white glyphs is
/// white-on-white and the letters vanish (owner: "cannot read the letters if the
/// flame is too bright"). Capping the index just short of white keeps the hottest
/// pixel a bright AMBER-GOLD (`~0xFFCF6E`) — still reads as a roaring hot fire,
/// but a white glyph keeps its contrast against it. Mirrored as the literal `850`
/// in the WGSL `fs_fire_*` twins (`fire_patch_parity` delta-0 contract).
const FIRE_IDX_MAX: i32 = 850;

/// Integer lattice hash → 32 avalanche bits. All arithmetic WRAPPING `u32`
/// (WGSL's native semantics), so the twins agree bit-for-bit.
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

/// Fixed-point smoothstep fade: `t` in `0..=255` → `t²(3·256 − 2t) >> 16` in
/// `0..=255` — the C¹ interpolant that keeps value-noise gradients continuous
/// at lattice lines (no texel edges).
#[inline]
#[must_use]
pub fn fire_fade(t: i32) -> i32 {
    (t * t * (768 - 2 * t)) >> 16
}

/// 2-D integer value noise at fp8 coordinates `(x, y)` → `0..=255`.
///
/// `ymask` masks the Y lattice index: the advected (time-scrolled) axis lives
/// on a `2^22`-fp8 ring (see the module doc), so a coordinate at 1×/2×/4×
/// frequency passes `0x3FFF`/`0x7FFF`/`0xFFFF` and the ring wraps seamlessly.
/// The X axis is spatial (bounded, never wraps) and stays unmasked.
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

/// The raw field decomposition at one window pixel — everything both
/// emitters share. `idx` is the palette index `0..=1023`; `q` the relative
/// height inside the local tongue (0 root → 256 tip); `edge` the anti-aliased
/// silhouette coverage `0..=256`; `body` the interior turbulence `0..=255`;
/// `root` the fuel-line lift ramp `0..=256`. A dead pixel returns all-zero
/// with `q = 256`.
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
    /// RIM proximity to the silhouette `0..=256` (256 at the outline, 0 in
    /// the deep interior): the cross-section volume cue — flames wear a
    /// cooler deep-red sheath around a hot core, ink pools at its edges.
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

/// The shared field core: sample the fire at window pixel `(px, py)`.
/// Pure function; total for ANY input. Both public emitters — and the GPU
/// WGSL twins — compose their coverage/alpha from exactly these components.
///
/// The PATCH-CONSTANT terms of the field — those that depend only on the
/// FirePatch params (cell_h, peak_h, phase, temp), never on `(px, py)`.
/// [`fire_precomp`] evaluates them ONCE per patch so the per-pixel core
/// ([`fire_core_px`]) reads them instead of re-deriving them at every pixel — a
/// CPU-only hoist that keeps the emitted byte BIT-IDENTICAL (the WGSL twin still
/// recomputes them per fragment, cheaply, in parallel), so the delta-0 parity
/// contract is untouched.
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

/// Compute the patch-constant terms of [`fire_core`] once per FirePatch.
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

/// Sample the fire field at `(px, py)` given the patch-constant [`FirePrecomp`].
/// Arithmetic is VERBATIM the original `fire_core`; the only change is reading
/// `pc.ch/chu/peak/tr/tr2/aa/offr` instead of re-deriving them — so the result
/// is bit-identical to [`fire_core`] (and thus to the WGSL twin) at every pixel.
#[must_use]
pub fn fire_core_px(px: i32, py: i32, p: &FireFieldParams, pc: &FirePrecomp) -> FireCore {
    // v: height above the flame root, px. Below the root the envelope MIRRORS,
    // compressed 5x — the ROOT SKIRT: the flame's dense base dissolves through
    // the top of the glyph row as a short ember bed instead of cutting off in a
    // hard horizontal line (the owner's "no hard clippings; organic" note). The
    // skirt draws in the UNDER-INK fire slot, so glyph ink still paints over it.
    let v = p.base_y - py;
    let v = if v >= 0 { v } else { -v * 5 };
    // vn: rise fraction vs the patch envelope (fp8, capped ×2 for overshoot).
    let vn = ((v * 256) / pc.peak).min(512);
    // LEAN SHEAR: the sample column drifts lean/4 px at full rise (fp8).
    let shear = p.lean * vn / 4;
    // Window-px fp8 X, sheared, hoisted +16384 px so all lattice math is
    // positive (identical truncation on both backends).
    let xq = (px * 256 + shear + (1 << 22)) as u32;
    // Horizontal field unit: one lattice cell per 1.25·cell_h px.
    let sx = (xq * 4) / (5 * pc.chu);

    // SILHOUETTE RIDGE: 2-octave value noise over (column, churn-time),
    // folded + squared into pointed licking tongues.
    let n0 = fire_vnoise(sx, pc.tr, 0x3FFF, FIRE_SEED_R0);
    let n1 = fire_vnoise(sx * 2 + 12799, pc.tr2, 0x7FFF, FIRE_SEED_R1);
    let n = (n0 * 9 + n1 * 7) >> 4;
    let ridge = 255 - (2 * n - 255).abs();
    // Fold-sharpened tongue shape, exponent ~2.5 (square blended with cube):
    // narrow pointed crests over wide valleys — no flat-top mesas.
    let hs2 = (ridge * ridge) >> 8;
    let hshape = (hs2 * (256 + ridge)) >> 9;
    // Tongue height for this column, fp8 px: a low 12% floor keeps deep dark
    // valleys BETWEEN tongues (separate licks, not a webbed mountain range),
    // crests overshoot to ~1.12·peak, scaled by strength.
    let hq = 30 + ((hshape * 260) >> 8);
    let hcol = (pc.peak * hq * p.strength) / 255;

    let vv = v * 256;
    let d = hcol - vv;
    if d <= 0 {
        return FIRE_DEAD;
    }
    // ANTI-ALIASED silhouette: coverage ramps over ~cell_h/4 px inside the
    // edge (lateral softness follows from the ridge's continuity in x).
    let edge = ((d * 256) / pc.aa).min(256);
    // RIM: 256 at the outline, fading to 0 by 2·aa inside — the sheath band.
    let rim = ((2 * pc.aa - d) * 256 / pc.aa).clamp(0, 256);
    // q: relative height inside the local tongue (0 root → 255 tip).
    let q = (vv * 256) / hcol;

    // INTERIOR BODY: three octaves advected UPWARD (sample at v − phase·rise)
    // — pockets brighten/darken as they rise, faster when hotter. Vertical
    // field unit: one lattice cell per 1.5·cell_h px (features taller than
    // wide, like real tongues).
    let sy = ((vv as u32) * 2) / (3 * pc.chu);
    let by = sy.wrapping_sub(pc.offr);
    let m0 = fire_vnoise(sx * 3 / 2 + 5023, by, 0x3FFF, FIRE_SEED_B0);
    let m1 = fire_vnoise(
        sx * 3 + 9531,
        by.wrapping_mul(2).wrapping_add(15913),
        0x7FFF,
        FIRE_SEED_B1,
    );
    let m2 = fire_vnoise(
        sx * 6 + 26251,
        by.wrapping_mul(4).wrapping_add(37633),
        0xFFFF,
        FIRE_SEED_B2,
    );
    let body0 = (m0 * 4 + m1 * 3 + m2) >> 3;
    // The turbulence flattens toward neutral across the AA fringe: silhouette
    // edges stay clean (no speckled outline on a white ground), the interior
    // keeps its full churn.
    let body = 128 + (((body0 - 128) * edge) >> 8);

    // PALETTE INDEX: heat falls from root to tip, reaches further (whiter)
    // when hot, modulated by the rising body turbulence — white-hot lives
    // only in body pockets at a hot root; tips die into deep red.
    let heat = ((256 - q) * (112 + ((p.temp * 120) >> 8))) >> 6;
    let idx = ((heat * (150 + ((body * 212) >> 8))) >> 8).clamp(0, FIRE_IDX_MAX);

    // ROOT LIFT: a thin fade over cell_h/6 px lifts the flame off the fuel
    // line — no hard bottom edge.
    let root = ((v * 1536) / pc.ch).min(256);
    // The extreme root cools slightly with the lift (amber fuel line, not a
    // desaturated white strip where the coverage fades).
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

/// The GPU-mirror / parity anchor: [`fire_core_px`] with a freshly computed
/// [`fire_precomp`]. Byte-identical to the pre-hoist `fire_core`.
#[must_use]
pub fn fire_core(px: i32, py: i32, p: &FireFieldParams) -> FireCore {
    fire_core_px(px, py, p, &fire_precomp(p))
}

/// Incremental cache for ONE value-noise octave along a left-to-right x-sweep at
/// a FIXED row. Its Y-side (`iy0`/`iy1`/`uy`) is row-constant; only the X lattice
/// index advances, by 0 or 1 as the sample column steps, so the 4 corner hashes
/// (`n00`/`n10`/`n01`/`n11` at `cur_ix`, `cur_ix+1`) carry over: a step just
/// shifts `n10→n00`, `n11→n01` and recomputes the 2 trailing corners. This turns
/// [`fire_vnoise`]'s 4 hashes/pixel into ~`2/lattice-period` — the bulk of the
/// per-pixel cost. `x` MUST be non-decreasing across calls (guaranteed: `sx`
/// grows monotonically with `px`).
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

    /// Sample the octave at fp8 x-coord `x` (`>= ` the previous call's `x`).
    /// Reconstructs [`fire_vnoise`]'s exact bilinear-with-fade interpolant, so
    /// the returned byte is IDENTICAL to `fire_vnoise(x, y, ymask, seed)`.
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

/// Row-incremental [`fire_core_px`]: a stateful sampler that walks ONE scanline
/// left-to-right, replacing the 5 per-pixel [`fire_vnoise`] calls (20 lattice
/// hashes) with the 5 [`FireOctave`] caches (~`2/period` hashes). Every emitted
/// [`FireCore`] is BIT-IDENTICAL to `fire_core_px(px, py, p, pc)` — the shaping
/// arithmetic is copied verbatim; only the noise SOURCE is cached. `px` must be
/// non-decreasing. Pinned by `fire_row_matches_fire_core` (an exhaustive
/// equivalence test) so it can never silently drift from the GPU-mirror.
pub struct FireRow<'a> {
    p: &'a FireFieldParams,
    pc: &'a FirePrecomp,
    /// Height above the root (row-constant) and its fp8 form.
    v: i32,
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
    /// Set up the scanline at `py`, with the first sample column `x_start`
    /// (device px). All the row-constant Y-sides and the octave caches are
    /// primed here so [`Self::core`] does only the per-pixel work.
    #[must_use]
    pub fn new(py: i32, x_start: i32, p: &'a FireFieldParams, pc: &'a FirePrecomp) -> Self {
        let v = p.base_y - py;
        // ROOT SKIRT twin of fire_core_px: mirrored 5x-compressed below the root.
        let v = if v >= 0 { v } else { -v * 5 };
        let vv = v * 256;
        // vn/shear are row-constant (depend on v only), matching fire_core_px.
        let vn = ((v * 256) / pc.peak).min(512);
        let shear = p.lean * vn / 4;
        // The starting sheared fp8 X, so each octave primes its cache at x_start.
        let xq = (x_start * 256 + shear + (1 << 22)) as u32;
        let sx = (xq * 4) / (5 * pc.chu);
        // Body vertical advection axis (row-constant).
        let sy = ((vv as u32) * 2) / (3 * pc.chu);
        let by = sy.wrapping_sub(pc.offr);
        Self {
            p,
            pc,
            v,
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

    /// Sample the field at device column `px` (non-decreasing). Bit-identical to
    /// `fire_core_px(px, self.py, self.p, self.pc)`.
    #[must_use]
    pub fn core(&mut self, px: i32) -> FireCore {
        let p = self.p;
        let pc = self.pc;

        let xq = (px * 256 + self.shear + (1 << 22)) as u32;
        let sx = (xq * 4) / (5 * pc.chu);

        // SILHOUETTE RIDGE (always evaluated — it decides the tongue height).
        let n0 = self.n0.sample(sx);
        let n1 = self.n1.sample(sx * 2 + 12799);
        let n = (n0 * 9 + n1 * 7) >> 4;
        let ridge = 255 - (2 * n - 255).abs();
        let hs2 = (ridge * ridge) >> 8;
        let hshape = (hs2 * (256 + ridge)) >> 9;
        let hq = 30 + ((hshape * 260) >> 8);
        let hcol = (pc.peak * hq * p.strength) / 255;

        let vv = self.vv;
        let d = hcol - vv;
        if d <= 0 {
            return FIRE_DEAD;
        }
        let edge = ((d * 256) / pc.aa).min(256);
        let rim = ((2 * pc.aa - d) * 256 / pc.aa).clamp(0, 256);
        let q = (vv * 256) / hcol;

        // INTERIOR BODY (live pixels only; the caches jump forward on demand).
        let m0 = self.m0.sample(sx * 3 / 2 + 5023);
        let m1 = self.m1.sample(sx * 3 + 9531);
        let m2 = self.m2.sample(sx * 6 + 26251);
        let body0 = (m0 * 4 + m1 * 3 + m2) >> 3;
        let body = 128 + (((body0 - 128) * edge) >> 8);

        let heat = ((256 - q) * (112 + ((p.temp * 120) >> 8))) >> 6;
        let idx = ((heat * (150 + ((body * 212) >> 8))) >> 8).clamp(0, FIRE_IDX_MAX);

        let root = ((self.v * 1536) / pc.ch).min(256);
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

// Black-body palette anchors — the shipped `fire_ramp` colours (deep red →
// orange → amber → near-white core), interpolated in integer over 1024 steps.
const FIRE_PAL_ADD: [[i32; 3]; 5] = [
    [0x2A, 0x00, 0x00],
    [0x8B, 0x1A, 0x00],
    [0xE0, 0x4A, 0x00],
    [0xFF, 0xB0, 0x20],
    [0xFF, 0xF0, 0xC0],
];

// INK-FIRE palette (FireMode::Over): deep red-browns and burnt oranges that
// read on WHITE — tips near-black brown, root a rich amber. Same index law.
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

/// [`FireMode::Add`](aterm_render_api::FireMode::Add) emission at one window
/// pixel: the PREMULTIPLIED `0x00RRGGBB` light (`(c·cov + 127)/255` per
/// channel — the single rounding point, == the GPU `fs_fire_add`), ready for
/// `add_sat`. `0` when the pixel is outside the flame.
/// The [`FireMode::Add`](aterm_render_api::FireMode::Add) coverage law:
/// silhouette AA × temp density × interior pockets × root lift, then the
/// readability ceiling. `0..=cov_cap`.
#[inline]
#[must_use]
pub fn fire_cov_add(c: &FireCore, p: &FireFieldParams) -> i32 {
    let dens = 150 + ((p.temp * 106) >> 8);
    let bodyc = 110 + ((c.body * 146) >> 8);
    ((((((c.edge * dens) >> 8) * bodyc) >> 8) * c.root) >> 8).min(p.cov_cap)
}

/// The [`FireMode::Over`](aterm_render_api::FireMode::Over) alpha law: the
/// same silhouette/root shaping, but a gentler interior grain (paint pools,
/// it does not sparkle) and a strong head→tip fade — dense grounded pigment
/// at the root, wispy translucent tips. `0..=cov_cap`.
#[inline]
#[must_use]
pub fn fire_alpha_over(c: &FireCore, p: &FireFieldParams) -> i32 {
    let bodyc = 130 + ((c.body * 166) >> 8);
    let tipf = 120 + (((256 - c.q) * 136) >> 8);
    let pool = 256 + ((c.rim * 96) >> 8);
    let a = (((((((c.edge * bodyc) >> 8) * c.root) >> 8) * tipf) >> 8) * pool) >> 8;
    a.min(p.cov_cap)
}

/// TOP-EDGE FADE factor `0..=255`: `0` at the grid top (`py == top_fade_y`),
/// ramping to `255` TWO cells below via the shared [`fire_fade`] smoothstep, so a
/// flame that has no room to rise (typing on the terminal's top row) DISSIPATES
/// over a long gradient instead of ending in a defined band edge that reads as a
/// hard clip. A 2-cell span (was 1) also thins whatever fraction of a flame
/// reaches the very top so a bright root can't sit right against the edge. Pure
/// integer, mirrored OP-FOR-OP by the WGSL `fs_fire_*` twins (the
/// `fire_patch_parity` delta-0 contract). A pixel two cells below the top
/// returns `255` — zero effect on every normal typing row lower down.
#[inline]
#[must_use]
pub fn fire_top_fade(py: i32, p: &FireFieldParams) -> i32 {
    let fade_px = (p.cell_h * 2).max(2);
    let t = (((py - p.top_fade_y) * 255) / fade_px).clamp(0, 255);
    fire_fade(t)
}

/// [`fire_field_add`] with the patch-constant [`FirePrecomp`] and the per-ROW
/// top-fade `tf` (`0..=255`) hoisted out of the per-pixel loop — the fast path
/// [`draw_fire_patch`](crate::Renderer) sweeps. Byte-identical to
/// [`fire_field_add`]: `tf == 255` (every row a full cell below the grid top)
/// takes the no-fade shortcut, exactly equal to `(cov·255)/255`.
/// Shade a sampled [`FireCore`] into the premultiplied additive light, given the
/// per-row top-fade `tf` (`0..=255`). The palette + rim-cool + fade tail of
/// [`fire_field_add`], factored out so the direct and the row-INCREMENTAL
/// ([`FireRow`]) paths share it VERBATIM — the byte can only come from here.
#[inline]
#[must_use]
pub fn fire_shade_add(c: &FireCore, p: &FireFieldParams, tf: i32) -> u32 {
    let cov0 = fire_cov_add(c, p);
    let cov = if tf == 255 { cov0 } else { (cov0 * tf) / 255 };
    if cov <= 0 {
        return 0;
    }
    // RIM COOLING: the tongue's outline drops toward deep red while the core
    // stays hot — the cross-section volume of a real flame.
    let idx = (c.idx * (256 - ((c.rim * 112) >> 8))) >> 8;
    let (r, g, b) = fire_pal(&FIRE_PAL_ADD, idx);
    let m = |ch: i32| ((ch * cov + 127) / 255) as u32;
    (m(r) << 16) | (m(g) << 8) | m(b)
}

#[must_use]
pub fn fire_field_add_row(px: i32, py: i32, p: &FireFieldParams, pc: &FirePrecomp, tf: i32) -> u32 {
    fire_shade_add(&fire_core_px(px, py, p, pc), p, tf)
}

#[must_use]
pub fn fire_field_add(px: i32, py: i32, p: &FireFieldParams) -> u32 {
    fire_field_add_row(px, py, p, &fire_precomp(p), fire_top_fade(py, p))
}

/// [`FireMode::Over`](aterm_render_api::FireMode::Over) emission at one
/// window pixel: `(straight 0x00RRGGBB ink, alpha)` for `over_rgb` /
/// SrcAlpha-OneMinusSrcAlpha. The alpha is the same field coverage, denser
/// at the root (`q = 0`) so the paint grounds itself; `(0, 0)` outside.
/// [`fire_field_over`] with the hoisted [`FirePrecomp`] + per-row `tf`. The fast
/// path `draw_fire_patch` sweeps; byte-identical to [`fire_field_over`].
/// Shade a sampled [`FireCore`] into `(ink, alpha)`, given the per-row `tf`. The
/// rim-darken + palette + fade tail of [`fire_field_over`], factored out so the
/// direct and [`FireRow`] paths share it verbatim.
#[inline]
#[must_use]
pub fn fire_shade_over(c: &FireCore, p: &FireFieldParams, tf: i32) -> (u32, u8) {
    let a0 = fire_alpha_over(c, p);
    let a = if tf == 255 { a0 } else { (a0 * tf) / 255 };
    if a <= 0 {
        return (0, 0);
    }
    // RIM DARKENING: ink pools at the outline — a deep red-brown contour
    // around a warm amber interior (the watercolor edge law).
    let idx = (c.idx * (256 - ((c.rim * 128) >> 8))) >> 8;
    let (r, g, b) = fire_pal(&FIRE_PAL_OVER, idx);
    (
        ((r as u32) << 16) | ((g as u32) << 8) | (b as u32),
        a.clamp(0, 255) as u8,
    )
}

#[must_use]
pub fn fire_field_over_row(
    px: i32,
    py: i32,
    p: &FireFieldParams,
    pc: &FirePrecomp,
    tf: i32,
) -> (u32, u8) {
    fire_shade_over(&fire_core_px(px, py, p, pc), p, tf)
}

#[must_use]
pub fn fire_field_over(px: i32, py: i32, p: &FireFieldParams) -> (u32, u8) {
    fire_field_over_row(px, py, p, &fire_precomp(p), fire_top_fade(py, p))
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

    /// THE ROOT SKIRT law: below the root the envelope mirrors 5x-compressed —
    /// nonzero right under `base_y` (the ember bed dissolving through the glyph
    /// row's top), byte-equal to the mirrored above-root sample, and dead again
    /// a fifth of the peak below (no hard horizontal cut in either direction).
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
        let pc = fire_precomp(&p);
        for px in [3, 40, 77] {
            // Mirror law: d px BELOW the root samples the envelope at 5·d ABOVE.
            for d in 1..=4 {
                let below = fire_core_px(px, p.base_y + d, &p, &pc);
                let above = fire_core_px(px, p.base_y - 5 * d, &p, &pc);
                assert_eq!(below, above, "5x mirror at d={d} px={px}");
            }
            // The skirt DIES within peak/5 (+1 for the mirror rounding).
            let deep = fire_core_px(px, p.base_y + p.peak_h / 5 + 2, &p, &pc);
            assert_eq!(deep.idx, 0, "skirt extinct below peak/5: px={px}");
        }
    }

    /// EQUIVALENCE (the incremental fast path's safety net): [`FireRow::core`]
    /// must return the EXACT same [`FireCore`] as the pure [`fire_core_px`] — and
    /// thus the GPU-mirror — at EVERY pixel of a patch, across a broad param
    /// grid. The incremental octave caches recompute only the trailing lattice
    /// corners across an x-run; this pins that they never drift by a single byte.
    #[test]
    fn fire_row_matches_fire_core() {
        let mut checks = 0u64;
        for &temp in &[0, 51, 128, 200, 255] {
            for &lean in &[-128i32, -50, 0, 60, 127] {
                for &phase in &[0u32, 8192, 123_456, 1 << 20, u32::MAX] {
                    for &cell_h in &[2i32, 20, 40, 65] {
                        for &(peak, strength) in &[(1, 0), (120, 230), (160, 255), (2048, 128)] {
                            let p = FireFieldParams {
                                base_y: 200,
                                peak_h: peak,
                                phase,
                                temp,
                                strength,
                                lean,
                                cov_cap: 200,
                                cell_h,
                                top_fade_y: 0,
                            };
                            let pc = fire_precomp(&p);
                            // Sweep several scanlines, each left-to-right (the
                            // sampler's monotonic-px contract), starting a little
                            // left of the grid so the shear/lattice priming is
                            // exercised at negative x too.
                            for py in [40, 90, 140, 180, 199] {
                                let x_start = -24;
                                let mut row = FireRow::new(py, x_start, &p, &pc);
                                for px in x_start..280 {
                                    let inc = row.core(px);
                                    let direct = fire_core_px(px, py, &p, &pc);
                                    assert_eq!(
                                        inc, direct,
                                        "FireRow drift: temp={temp} lean={lean} phase={phase} \
                                         ch={cell_h} peak={peak} str={strength} px={px} py={py}"
                                    );
                                    checks += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(checks > 500_000, "equivalence grid too small ({checks})");
    }

    /// DETERMINISM: the field is a pure function — same inputs, same bytes,
    /// twice, across the whole patch.
    #[test]
    fn field_is_deterministic() {
        let p = params(180, -60, 123_456);
        for py in 60..200 {
            for px in 0..160 {
                assert_eq!(
                    fire_field_add(px, py, &p),
                    fire_field_add(px, py, &p),
                    "add at ({px},{py})"
                );
                assert_eq!(
                    fire_field_over(px, py, &p),
                    fire_field_over(px, py, &p),
                    "over at ({px},{py})"
                );
            }
        }
    }

    /// AA LAW: along the silhouette the coverage/alpha must roll off smoothly
    /// — no two vertically adjacent pixels may differ by more than the
    /// analytic slope budget. Coverage is a product of ≤1 factors, so the
    /// worst adjacent step is bounded by `cov_cap · Σ(per-factor slope)`:
    /// AA ramp 256/aa_fp8 → 0.1 (aa = ch/4 = 10 px), amplified ≤ 1.6× by the
    /// rim pool (1.375) and turbulence (1.16) factors → 0.16, + root lift 1536/ch/256
    /// → 0.15 + turbulence (three fade-interpolated octaves, worst vertical
    /// lattice step 17 fp8/px → Δbody ≈ 29, ×166/256 weight) → 0.073 + rim
    /// pool 96/256 · 0.125 → 0.047 + tip fade → ≤ 0.09, plus cross terms —
    /// ≈ 0.55·cov_cap. Bound at 0.6·cov_cap: a hard texel edge (a full-cap
    /// jump) is IMPOSSIBLE and every silhouette crossing spans ≥ 2 px.
    #[test]
    fn aa_law_no_hard_silhouette_edges() {
        let mut worst = 0i32;
        for temp in [51, 128, 230] {
            for phase in [0u32, 8192, 1 << 20] {
                let p = params(temp, -50, phase);
                let bound = (p.cov_cap * 3) / 5; // 120 at cap 200
                for px in 0..200 {
                    let mut prev = 0i32;
                    // walk downward (tip → root) so each column crosses the edge
                    for py in 40..=200 {
                        let c = fire_core(px, py, &p);
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

    /// SEAM LAW (field level): the field is a pure function of ABSOLUTE
    /// coordinates — a patch split cannot change any pixel, and the temporal
    /// ring wrap of the advection offsets is seamless (masked lattice).
    #[test]
    fn field_ignores_patch_geometry_and_wraps_seamlessly() {
        // Ring wrap: phase such that phase·ts crosses 2^32 — the lattice
        // masks make the wrap continuous; here we only pin totality + range.
        for phase in [u32::MAX, u32::MAX / 2, 0] {
            let p = params(255, 127, phase);
            for py in 80..200 {
                for px in 0..64 {
                    let c = fire_core(px, py, &p);
                    assert!((0..=1023).contains(&c.idx));
                    assert!((0..=256).contains(&c.q));
                    assert!((0..=p.cov_cap).contains(&fire_cov_add(&c, &p)));
                    assert!((0..=p.cov_cap).contains(&fire_alpha_over(&c, &p)));
                }
            }
        }
    }

    /// TOTALITY: degenerate params (zero peak, zero cell_h, saturated
    /// values) must not panic or overflow anywhere in the patch.
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
            for py in 0..80 {
                for px in 0..16384i32 {
                    if px > 64 && px < 16300 {
                        continue;
                    }
                    let _ = fire_field_add(px, py, p);
                    let _ = fire_field_over(px, py, p);
                }
            }
        }
    }

    /// The flame must actually burn: a hot patch produces live coverage,
    /// tongues (column height variance), white-hot only near the root and
    /// deep red at the tips — the palette-reach law.
    #[test]
    fn field_burns_with_structure() {
        let p = params(230, 0, 4096);
        let mut lit = 0usize;
        let mut root_idx_max = 0i32;
        let mut tip_idx_max = 0i32;
        for px in 0..300 {
            for py in 60..200 {
                let c = fire_core(px, py, &p);
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
