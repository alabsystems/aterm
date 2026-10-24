// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shared pure-integer procedural fire field behind `FirePatch`.

/// Per-patch field parameters in window pixel space (pad already applied).
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
    /// Horizontal shear: `lean/4` px drift at full rise.
    pub lean: i32,
    /// Coverage/alpha ceiling `0..=255`.
    pub cov_cap: i32,
    /// Cell height in px (the field's spatial unit); clamped to `>= 2`.
    pub cell_h: i32,
    /// Top-edge fade anchor: the window-px y of the grid top (the render pad).
    pub top_fade_y: i32,
}

const FIRE_P0: u32 = 0x9E37_79B1;
const FIRE_P1: u32 = 0x85EB_CA77;
const FIRE_P2: u32 = 0xC2B2_AE3D;
const FIRE_M0: u32 = 0x2C1B_3C6D;
const FIRE_M1: u32 = 0x297A_2D39;

const FIRE_SEED_R0: u32 = 0x51F0_A3B7;
const FIRE_SEED_R1: u32 = 0x9D2C_5680;
const FIRE_SEED_B0: u32 = 0xB529_7A4D;
const FIRE_SEED_B1: u32 = 0x68E3_1DA4;
const FIRE_SEED_B2: u32 = 0x1B56_C4E9;

const FIRE_IDX_MAX: i32 = 850;

/// Integer lattice hash → 32 avalanche bits, all wrapping `u32` so the shader agrees bit-for-bit.
#[inline]
#[must_use]
pub const fn fire_hash(x: u32, y: u32, seed: u32) -> u32 {
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
pub const fn fire_fade(t: i32) -> i32 {
    (t * t * (768 - 2 * t)) >> 16
}

/// 2-D integer value noise at fp8 `(x, y)` → `0..=255`.
#[inline]
#[must_use]
pub const fn fire_vnoise(x: u32, y: u32, ymask: u32, seed: u32) -> i32 {
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

/// The field decomposition at one window pixel, shared by both emitters.
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
    /// Rim proximity `0..=256` (256 at the outline, 0 deep inside).
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

/// Patch-constant field terms computed once per patch by [`fire_precomp`].
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
pub const fn fire_precomp(p: &FireFieldParams) -> FirePrecomp {
    let ch = if p.cell_h > 2 { p.cell_h } else { 2 };
    let ts = (300 + p.temp * 2) as u32;
    let tr = p.phase.wrapping_mul(ts) >> 10;
    let rs = (350 + p.temp) as u32;
    let offr = p.phase.wrapping_mul(rs) >> 10;
    let peak = if p.peak_h < 1 {
        1
    } else if p.peak_h > 2048 {
        2048
    } else {
        p.peak_h
    };
    let q_ch = ch / 4;
    let aa = (if q_ch > 3 { q_ch } else { 3 }) * 256;
    FirePrecomp {
        ch,
        chu: ch as u32,
        peak,
        tr,
        tr2: tr.wrapping_mul(2).wrapping_add(37199),
        aa,
        offr,
    }
}

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
    const fn new(y: u32, ymask: u32, seed: u32, x0: u32) -> Self {
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

    #[inline]
    const fn sample(&mut self, x: u32) -> i32 {
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

/// Scanline field sampler returning the [`FireCore`] both emitters build on.
pub struct FireRow<'a> {
    p: &'a FireFieldParams,
    pc: &'a FirePrecomp,
    root: i32,
    vv: i32,
    shear: i32,
    n0: FireOctave,
    n1: FireOctave,
    m0: FireOctave,
    m1: FireOctave,
    m2: FireOctave,
}

impl<'a> FireRow<'a> {
    /// Set up the scanline at `py` starting at column `x_start` (device px).
    #[inline]
    #[must_use]
    pub const fn new(py: i32, x_start: i32, p: &'a FireFieldParams, pc: &'a FirePrecomp) -> Self {
        let v = p.base_y - py;
        let v = if v >= 0 { v } else { -v * 5 };
        let vv = v * 256;
        let vn_raw = (v * 256) / pc.peak;
        let vn = if vn_raw < 512 { vn_raw } else { 512 };
        let shear = p.lean * vn / 4;
        let xq = (x_start * 256 + shear + (1 << 22)) as u32;
        let sx = (xq * 4) / (5 * pc.chu);
        let sy = ((vv as u32) * 2) / (3 * pc.chu);
        let by = sy.wrapping_sub(pc.offr);
        let root_raw = (v * 1536) / pc.ch;
        Self {
            p,
            pc,
            root: if root_raw < 256 { root_raw } else { 256 },
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
    #[inline]
    #[must_use]
    pub const fn core(&mut self, px: i32) -> FireCore {
        let p = self.p;
        let pc = self.pc;

        let xq = (px * 256 + self.shear + (1 << 22)) as u32;
        let sx = (xq * 4) / (5 * pc.chu);

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
        let edge_raw = (d * 256) / pc.aa;
        let edge = if edge_raw < 256 { edge_raw } else { 256 };
        let rim_raw = (2 * pc.aa - d) * 256 / pc.aa;
        let rim = if rim_raw < 0 {
            0
        } else if rim_raw > 256 {
            256
        } else {
            rim_raw
        };
        let q = (vv * 256) / hcol;

        let m0 = self.m0.sample(sx * 3 / 2 + 5023);
        let m1 = self.m1.sample(sx * 3 + 9531);
        let m2 = self.m2.sample(sx * 6 + 26251);
        let body0 = (m0 * 4 + m1 * 3 + m2) >> 3;
        let body = 128 + (((body0 - 128) * edge) >> 8);

        let heat = ((256 - q) * (112 + ((p.temp * 120) >> 8))) >> 6;
        let idx_raw = (heat * (150 + ((body * 212) >> 8))) >> 8;
        let idx = if idx_raw < 0 {
            0
        } else if idx_raw > FIRE_IDX_MAX {
            FIRE_IDX_MAX
        } else {
            idx_raw
        };

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

const FIRE_PAL_ADD: [[i32; 3]; 5] = [
    [0x2A, 0x00, 0x00],
    [0x8B, 0x1A, 0x00],
    [0xE0, 0x4A, 0x00],
    [0xFF, 0xB0, 0x20],
    [0xFF, 0xF0, 0xC0],
];

/// The hottest palette index additive fire lays: a breath past the amber
/// stop ([`FIRE_PAL_ADD`]`[3]`), short of the field's own ceiling
/// ([`FIRE_IDX_MAX`]). Up there the palette mixes toward its cream stop, and
/// a pale gold laid thin over a dark page (a tongue's feathered foot and
/// flanks) reads as a tan curtain, not a flame (`#c1964c` to `#ca9a42`, the
/// trail gallery's dark `burst`, looks6 round 3); amber laid thin stays an
/// amber. The shaders' `fire_pal_add` hold the same cap.
pub const FIRE_ADD_IDX_MAX: i32 = 780;

/// [`FIRE_PAL_ADD`] at `idx`, held to at most `cap`.
#[inline]
const fn fire_pal_add(idx: i32, cap: i32) -> (i32, i32, i32) {
    fire_pal(&FIRE_PAL_ADD, if idx > cap { cap } else { idx })
}

/// Brightest light per unit coverage laid by additive fire ([`FIRE_ADD_IDX_MAX`]).
pub const FIRE_ADD_HOTTEST: u32 = {
    let (r, g, b) = fire_pal_add(FIRE_IDX_MAX, FIRE_ADD_IDX_MAX);
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
};

/// Brightest light per unit coverage laid by additive fire on scanline `py`.
#[inline]
#[must_use]
pub const fn fire_add_hottest_at(p: &FireFieldParams, py: i32) -> u32 {
    let idx = fire_add_index_at(p, py);
    if idx < 0 { 0 } else { fire_add_light(idx) }
}

/// The palette index behind [`fire_add_hottest_at`].
#[inline]
#[must_use]
pub const fn fire_add_index_at(p: &FireFieldParams, py: i32) -> i32 {
    let v = p.base_y - py;
    let v = if v >= 0 { v } else { -v * 5 };
    let vv = v * 256;
    let peak = if p.peak_h < 1 {
        1
    } else if p.peak_h > 2048 {
        2048
    } else {
        p.peak_h
    };
    let hcol = (peak * FIRE_HQ_MAX * p.strength) / 255;
    if hcol <= vv {
        return -1;
    }
    let q = (vv * 256) / hcol;
    let heat = ((256 - q) * (112 + ((p.temp * 120) >> 8))) >> 6;
    let idx_raw = (heat * (150 + ((255 * 212) >> 8))) >> 8;
    let idx = if idx_raw > FIRE_IDX_MAX {
        FIRE_IDX_MAX
    } else {
        idx_raw
    };
    let ch = if p.cell_h > 2 { p.cell_h } else { 2 };
    let root_raw = (v * 1536) / ch;
    let root = if root_raw < 256 { root_raw } else { 256 };
    (idx * (192 + (root >> 2))) >> 8
}

/// Additive fire light per unit coverage at palette index `idx` (clamped to
/// `0..=`[`FIRE_ADD_IDX_MAX`]).
#[inline]
#[must_use]
pub const fn fire_add_light(idx: i32) -> u32 {
    let idx = if idx < 0 { 0 } else { idx };
    let (r, g, b) = fire_pal_add(idx, FIRE_ADD_IDX_MAX);
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

const FIRE_HQ_MAX: i32 = 30 + ((((((255 * 255) >> 8) * (256 + 255)) >> 9) * 260) >> 8);

/// The darkest ink the ink-fire ([`FireMode::Over`](crate::render_api::FireMode::Over)) lays.
pub const FIRE_OVER_RIM_INK: u32 = 0x0066_1804;

const FIRE_PAL_OVER: [[i32; 3]; 5] = [
    [
        ((FIRE_OVER_RIM_INK >> 16) & 0xFF) as i32,
        ((FIRE_OVER_RIM_INK >> 8) & 0xFF) as i32,
        (FIRE_OVER_RIM_INK & 0xFF) as i32,
    ],
    [0xA8, 0x28, 0x00],
    [0xD0, 0x46, 0x00],
    [0xEE, 0x6E, 0x00],
    [0xFF, 0xA6, 0x1E],
];

#[inline]
const fn fire_pal(pal: &[[i32; 3]; 5], idx: i32) -> (i32, i32, i32) {
    let seg_raw = idx >> 8;
    let seg = if seg_raw < 0 {
        0
    } else if seg_raw > 3 {
        3
    } else {
        seg_raw as usize
    };
    let f = idx - ((seg as i32) << 8);
    let a = pal[seg];
    let b = pal[seg + 1];
    #[inline]
    const fn mix(x: i32, y: i32, f: i32) -> i32 {
        (x * (256 - f) + y * f) >> 8
    }
    (mix(a[0], b[0], f), mix(a[1], b[1], f), mix(a[2], b[2], f))
}

/// The [`FireMode::Add`](crate::render_api::FireMode::Add) coverage law.
#[inline]
#[must_use]
pub const fn fire_cov_add(c: &FireCore, p: &FireFieldParams) -> i32 {
    let dens = 150 + ((p.temp * 106) >> 8);
    let bodyc = 110 + ((c.body * 146) >> 8);
    let cov = (((((c.edge * dens) >> 8) * bodyc) >> 8) * c.root) >> 8;
    if cov < p.cov_cap { cov } else { p.cov_cap }
}

/// The [`FireMode::Over`](crate::render_api::FireMode::Over) alpha law.
#[inline]
#[must_use]
pub const fn fire_alpha_over(c: &FireCore, p: &FireFieldParams) -> i32 {
    let bodyc = 130 + ((c.body * 166) >> 8);
    let tipf = 120 + (((256 - c.q) * 136) >> 8);
    let pool = 256 + ((c.rim * 96) >> 8);
    let a = (((((((c.edge * bodyc) >> 8) * c.root) >> 8) * tipf) >> 8) * pool) >> 8;
    if a < p.cov_cap { a } else { p.cov_cap }
}

/// Top-edge fade `0..=255` from `0` at `top_fade_y` to `255` two cells below.
#[inline]
#[must_use]
pub const fn fire_top_fade(py: i32, p: &FireFieldParams) -> i32 {
    let fade_px = if p.cell_h * 2 > 2 { p.cell_h * 2 } else { 2 };
    let t_raw = ((py - p.top_fade_y) * 255) / fade_px;
    let t = if t_raw < 0 {
        0
    } else if t_raw > 255 {
        255
    } else {
        t_raw
    };
    fire_fade(t)
}

/// [`FireMode::Add`](crate::render_api::FireMode::Add) emission for one [`FireRow`] sample.
#[inline]
#[must_use]
pub const fn fire_shade_add(c: &FireCore, p: &FireFieldParams, tf: i32) -> u32 {
    fire_shade_add_to(c, p, tf, FIRE_ADD_IDX_MAX)
}

/// [`fire_shade_add`] with its palette held to index `cap` in place of
/// [`FIRE_ADD_IDX_MAX`] (`1023`: the field's own ceiling alone).
#[inline]
#[must_use]
pub const fn fire_shade_add_to(c: &FireCore, p: &FireFieldParams, tf: i32, cap: i32) -> u32 {
    let cov0 = fire_cov_add(c, p);
    let cov = if tf == 255 { cov0 } else { (cov0 * tf) / 255 };
    if cov <= 0 {
        return 0;
    }
    let idx = (c.idx * (256 - ((c.rim * 112) >> 8))) >> 8;
    let (r, g, b) = fire_pal_add(idx, cap);
    #[inline]
    const fn m(ch: i32, cov: i32) -> u32 {
        ((ch * cov + 127) / 255) as u32
    }
    (m(r, cov) << 16) | (m(g, cov) << 8) | m(b, cov)
}

/// [`FireMode::Over`](crate::render_api::FireMode::Over) emission for one [`FireRow`] sample.
#[inline]
#[must_use]
pub const fn fire_shade_over(c: &FireCore, p: &FireFieldParams, tf: i32) -> (u32, u8) {
    let a0 = fire_alpha_over(c, p);
    let a = if tf == 255 { a0 } else { (a0 * tf) / 255 };
    if a <= 0 {
        return (0, 0);
    }
    let idx = (c.idx * (256 - ((c.rim * 128) >> 8))) >> 8;
    let (r, g, b) = fire_pal(&FIRE_PAL_OVER, idx);
    let a_clamped = if a > 255 { 255 } else { a };
    (
        ((r as u32) << 16) | ((g as u32) << 8) | (b as u32),
        a_clamped as u8,
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

    fn core_at(px: i32, py: i32, p: &FireFieldParams) -> FireCore {
        FireRow::new(py, px, p, &fire_precomp(p)).core(px)
    }

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
            for d in 1..=4 {
                let below = core_at(px, p.base_y + d, &p);
                let above = core_at(px, p.base_y - 5 * d, &p);
                assert_eq!(below, above, "5x mirror at d={d} px={px}");
            }
            let deep = core_at(px, p.base_y + p.peak_h / 5 + 2, &p);
            assert_eq!(deep.idx, 0, "skirt extinct below peak/5: px={px}");
        }
    }

    #[test]
    fn aa_law_no_hard_silhouette_edges() {
        let mut worst = 0i32;
        for temp in [51, 128, 230] {
            for phase in [0u32, 8192, 1 << 20] {
                let p = params(temp, -50, phase);
                let bound = (p.cov_cap * 3) / 5; // 120 at cap 200
                for px in 0..200 {
                    let mut prev = 0i32;
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

    #[test]
    fn the_hottest_light_bounds_every_additive_sample() {
        let over = |bound: u32| {
            let mut over = 0usize;
            for temp in [0, 128, 255] {
                let p = params(temp, 0, 7_000);
                for px in (0..300).step_by(3) {
                    for py in (40..200).step_by(2) {
                        let c = core_at(px, py, &p);
                        let cov = fire_cov_add(&c, &p);
                        let lit = fire_shade_add(&c, &p, 255);
                        let ch = |v: u32, sh: u32| (v >> sh) & 0xff;
                        if [16, 8, 0]
                            .iter()
                            .any(|&sh| ch(lit, sh) > (ch(bound, sh) * cov as u32 + 127) / 255)
                        {
                            over += 1;
                        }
                    }
                }
            }
            over
        };
        assert_eq!(
            over(FIRE_ADD_HOTTEST),
            0,
            "a sample outshines the hottest light"
        );
        let amber = {
            let [r, g, b] = FIRE_PAL_ADD[3];
            ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
        };
        assert!(
            over(amber) > 0,
            "control: the hot cores pass the amber anchor"
        );
    }

    #[test]
    fn additive_fire_laid_thin_over_a_dark_page_reads_amber_not_tan() {
        // looks6 round 3: the field's hottest light, mixed toward the cream
        // stop, laid thin over a dark page read as a tan curtain (#c1964c to
        // #ca9a42: warm, greyed, mid-bright) under the dark fire's feathered
        // feet and flanks. Held a breath past amber, no sample of a hot or
        // warm burn over the gallery's dark page reads tan but a few
        // samples at the edge of the band (160 of 128,250); up to the
        // field's own ceiling (the negative control), fifteen times as many
        // do.
        const PAGE: u32 = 0x0011_1318;
        let tan = |cap: i32| {
            let mut tan = 0usize;
            for temp in [128, 200, 255] {
                for phase in [7_000u32, 91_337, 400_001] {
                    let p = params(temp, 0, phase);
                    for px in (0..300).step_by(2) {
                        for py in (40..230).step_by(2) {
                            let c = core_at(px, py, &p);
                            let lit = fire_shade_add_to(&c, &p, 255, cap);
                            let ch = |sh: u32| {
                                (((PAGE >> sh) & 0xff) + ((lit >> sh) & 0xff)).min(255) as f32
                                    / 255.0
                            };
                            let (r, g, b) = (ch(16), ch(8), ch(0));
                            let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
                            if hi <= 0.5 || hi >= 0.86 || r < hi {
                                continue;
                            }
                            let sat = (hi - lo) / hi;
                            let hue = 60.0 * (g - b) / (hi - lo);
                            if (0.30..=0.72).contains(&sat) && (25.0..=55.0).contains(&hue) {
                                tan += 1;
                            }
                        }
                    }
                }
            }
            tan
        };
        let (held, pale) = (tan(FIRE_ADD_IDX_MAX), tan(1023));
        assert!(held <= 400, "thin additive fire reads tan {held} times");
        assert!(
            pale >= 8 * held.max(250),
            "control: up to the field's own ceiling, thin fire reads tan only {pale} times ({held} held)"
        );
    }

    #[test]
    fn the_light_at_a_height_bounds_every_sample_on_it() {
        let over = |up: i32| {
            let mut over = 0usize;
            for (temp, strength, lean) in [(0, 255, 0), (128, 230, 40), (255, 255, -60)] {
                let mut p = params(temp, lean, 7_000);
                p.strength = strength;
                for phase in [7_000u32, 91_337, 400_001] {
                    p.phase = phase;
                    for py in (40..230).step_by(2) {
                        let bound = fire_add_hottest_at(&p, py - up);
                        for px in (0..300).step_by(2) {
                            let c = core_at(px, py, &p);
                            let cov = fire_cov_add(&c, &p);
                            let lit = fire_shade_add(&c, &p, 255);
                            let ch = |v: u32, sh: u32| (v >> sh) & 0xff;
                            if [16, 8, 0].iter().any(|&sh| {
                                ch(lit, sh) > (ch(bound, sh) * cov as u32 + 127) / 255 + 1
                            }) {
                                over += 1;
                            }
                        }
                    }
                }
            }
            over
        };
        assert_eq!(over(0), 0, "a sample outshines its scanline's bound");
        let p = params(128, 0, 7_000);
        let high = fire_add_hottest_at(&p, p.base_y - p.peak_h * 3 / 4);
        assert!(
            (high >> 8) & 0xff < 0x60,
            "three quarters up a tongue its light is still #{high:06x}"
        );
        assert!(over(40) > 0, "control: the bound read higher up is passed");
    }
}
