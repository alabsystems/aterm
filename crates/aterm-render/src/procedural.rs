// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Procedural box-drawing / block-element / braille glyphs.
//!
//! Fonts draw U+2500–257F (box drawing), U+2580–259F (block elements) and
//! U+2800–28FF (braille) with glyph metrics that rarely fill the cell exactly,
//! leaving hairline gaps or overlaps where strokes should meet across cell
//! boundaries — the classic tmux pane-border / powerline seam artifact. This
//! module synthesizes those glyphs from the cell geometry instead: every
//! bitmap is exactly `cell_w x cell_h` and strokes always reach the cell edge.
//!
//! Coverage comes in two regimes, split by family:
//!
//! * ORTHOGONAL families (axis-aligned strokes and fills: lines, junctions,
//!   dashes, doubles, blocks, shades, quadrants, sextants, braille, the
//!   legacy eighth blocks U+1FB70–1FB8B) are HARD 0/255 — no antialiasing —
//!   so the CPU coverage blend and the GPU alpha blend produce EXACTLY the
//!   same pixels on these cells (coverage 255 -> pure foreground, coverage 0
//!   -> untouched background).
//! * DIAGONAL/CURVED families ([`antialiased`]: the diagonals U+2571–2573,
//!   rounded arcs U+256D–2570, Powerline separators U+E0B0–E0BF and the
//!   legacy wedges/triangles U+1FB3C–1FB6F) are rasterized at 4×4 subsamples
//!   per pixel and box-filtered down to 8-bit coverage, with stroke thickness
//!   measured PERPENDICULAR to the edge (a slanted stroke no longer thins
//!   into a beaded chain of per-row horizontal fills). Their CELL-EDGE texels
//!   are then forced back to hard 0/255 ([`Canvas::harden_edges`]) so
//!   adjacent cells still tile bit-exactly at the seam — the seam-tiling law,
//!   machine-checked by `tests/procedural_aa_edges.rs`.
//!
//! Dispatch lives in [`crate::Renderer::glyph_key`], which routes these ranges
//! to [`crate::FaceId::Procedural`] BEFORE any font lookup. The escape hatch
//! `ATERM_NO_PROCEDURAL_GLYPHS=1` (read at renderer construction, documented
//! with the other font env vars in `lib.rs`) restores font glyphs.
//!
//! ## The shared rounding rule
//!
//! Every stroke is sized and placed by ONE rule, so glyphs in adjacent cells
//! always meet exactly:
//!
//! * `light = max(1, round(min(w, h) / 8))` — in integers, `(min(w, h) + 4) / 8`.
//! * `heavy = 3 * light` — the same parity as `light`, so the heavy span
//!   exactly contains the light span on both axes (a heavy stroke widens a
//!   light one by `light` on each side, never shifting its centre).
//! * a stroke of thickness `t` across an extent `e` covers the half-open span
//!   `[(e - t) / 2, (e - t) / 2 + t)` (integer division: when `e - t` is odd
//!   the extra pixel goes to the right/bottom).
//! * a double line is the heavy span split into rails: its first and last
//!   `light` pixels, leaving a gap that is exactly the light span — so a
//!   single line threading a double junction passes through the gap.
//! * block-element fractions use `eighth(k, e) = (k * e + 4) / 8` (round half
//!   up), each block anchored to its defining edge — complementary halves
//!   (▀/▄, ▌/▐, the quadrants) overlap by one pixel on odd extents rather
//!   than leaving a seam.
//!
//! Within those rules, full coverage of the drawn blocks: solid, dashed
//! (double/triple/quadruple), rounded arcs (U+256D–2570), diagonals
//! (U+2571–2573), every light/heavy/double junction, eighth blocks, quadrants,
//! braille, sextants and the legacy wedge/eighth ranges. The shade characters
//! ░▒▓ (U+2591–2593) are necessarily rendered as 0/255 ordered dithers (25% /
//! 50% checkerboard / 75%) instead of translucent grey, keeping the CPU==GPU
//! exactness guarantee — keyed by the cell's ABSOLUTE pixel-position parity
//! ([`coverage_phased`]) so any odd cell width still tiles the dither with a
//! uniform period at every seam (no doubled line, machine-checked by
//! `tests/shade_phase.rs`).

/// Whether `ch` is in a range this module draws (box drawing U+2500–257F,
/// block elements U+2580–259F, braille U+2800–28FF, legacy sextants /
/// wedges / eighth blocks U+1FB00–1FB8B, Powerline separators U+E0B0–E0BF —
/// centred solid/outline triangles, rounded half-circles, and the four corner
/// ("angled") triangles + outlines).
pub fn covers(ch: char) -> bool {
    matches!(
        u32::from(ch),
        0x2500..=0x259F | 0x2800..=0x28FF | 0x1FB00..=0x1FB8B | 0xE0B0..=0xE0BF
    )
}

/// Whether `ch` is in an ANTI-ALIASED (diagonal/curved) procedural family:
/// rounded arcs U+256D–2570, diagonals U+2571–2573, legacy wedges/triangles
/// U+1FB3C–1FB6F, Powerline U+E0B0–E0BF. Interior texels of these glyphs may
/// take any 0..=255 coverage; their cell-edge texels are still hard 0/255
/// (the seam-tiling law). Everything else `covers()` is hard 0/255 throughout.
pub fn antialiased(ch: char) -> bool {
    matches!(
        u32::from(ch),
        0x256D..=0x2573 | 0x1FB3C..=0x1FB6F | 0xE0B0..=0xE0BF
    )
}

/// The procedural coverage bitmap for `ch` at a `cell_w x cell_h` cell:
/// row-major `cell_w * cell_h` bytes — hard 0/255 for the orthogonal
/// families, 8-bit box-filtered coverage (hard at the cell edges) for the
/// [`antialiased`] ones. `None` when `ch` is outside the procedural ranges
/// (or the cell is degenerate). Shades use phase `(0, 0)` here; the renderer
/// passes the cell's absolute-parity phase via [`coverage_phased`].
pub fn coverage(ch: char, cell_w: usize, cell_h: usize) -> Option<Vec<u8>> {
    coverage_phased(ch, cell_w, cell_h, false, false)
}

/// [`coverage`] with the shade-dither phase made explicit. `phase_x`/`phase_y`
/// are the PARITY of the destination cell's top-left pixel (`(pad + col *
/// cell_w) & 1` and the row twin): the ░▒▓ dithers are functions of ABSOLUTE
/// framebuffer parity, so two horizontally-adjacent cells of odd width get
/// opposite `phase_x` and the composed pattern keeps its uniform 2-pixel
/// period across the seam (cell-local parity doubled a dither line at EVERY
/// seam when `cell_w` was odd). At most 4 variants exist per shade; every
/// non-shade glyph ignores the phase. Both the CPU blit and the GPU quad
/// emission key their glyph lookups with this same phase
/// ([`crate::shade_phase_key`]), so the two backends stay byte-identical.
pub fn coverage_phased(
    ch: char,
    cell_w: usize,
    cell_h: usize,
    phase_x: bool,
    phase_y: bool,
) -> Option<Vec<u8>> {
    if cell_w == 0 || cell_h == 0 || !covers(ch) {
        return None;
    }
    let cp = u32::from(ch);
    let mut c = Canvas::new(cell_w, cell_h);
    let m = Metrics::new(cell_w, cell_h);
    match cp {
        0x2591..=0x2593 => shade(
            &mut c,
            (cp - 0x2590) as u8,
            usize::from(phase_x),
            usize::from(phase_y),
        ),
        0x2500..=0x257F => draw_box(&mut c, &m, cp),
        0x2580..=0x259F => draw_block(&mut c, cp),
        0x2800..=0x28FF => draw_braille(&mut c, cp),
        0x1FB00..=0x1FB3B => draw_sextant(&mut c, cp),
        0x1FB3C..=0x1FB6F => draw_wedge(&mut c, cp),
        0x1FB70..=0x1FB8B => draw_legacy_block(&mut c, cp),
        0xE0B0..=0xE0BF => draw_powerline(&mut c, cp),
        _ => unreachable!("covers() gates the ranges"),
    }
    if antialiased(ch) {
        // THE seam-tiling law: AA families still tile bit-exactly across cells.
        c.harden_edges();
    }
    Some(c.buf)
}

/// The anti-aliased coverage sprite for a [`crate::DecoGlyph`] "sparkle word"
/// decoration at a `cell_w x cell_h` cell: row-major `cell_w * cell_h` bytes,
/// each a coverage value `0..=255`.
///
/// Unlike the box-drawing glyphs (hard 0/255), these are soft-edged so the
/// sparkle reads cleanly at small cell sizes. The GPU path rasterizes the SAME
/// mask into its atlas, so CPU and GPU stay byte-parity-exact: the host bakes
/// colour + per-decoration alpha at composite time, and the mask is the single
/// shared source of shape.
#[must_use]
pub fn deco_coverage(glyph: crate::DecoGlyph, cell_w: usize, cell_h: usize) -> Vec<u8> {
    use crate::DecoGlyph;
    let mut buf = vec![0u8; cell_w * cell_h];
    if cell_w == 0 || cell_h == 0 {
        return buf;
    }
    let (fw, fh) = (cell_w as f32, cell_h as f32);
    let (cx, cy) = (fw * 0.5, fh * 0.5);
    let r = fw.min(fh) * 0.5 * 0.94;
    for y in 0..cell_h {
        for x in 0..cell_w {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let cov = match glyph {
                DecoGlyph::Dot => disc(px, py, cx, cy, r * 0.5),
                DecoGlyph::Plus => plus(px, py, cx, cy, r),
                DecoGlyph::Star4 => needle_star(px, py, cx, cy, r, 4, 0.0, 0.18),
                DecoGlyph::Star5 => {
                    needle_star(px, py, cx, cy, r, 5, -std::f32::consts::FRAC_PI_2, 0.16)
                }
                DecoGlyph::Paw => paw(px, py, fw, fh),
                DecoGlyph::Droplet => droplet(px, py, fw, fh),
                DecoGlyph::RingArc => ring_arc(px, py, cx, cy, r),
                DecoGlyph::Shade => soft_square(px, py, fw, fh),
            };
            buf[y * cell_w + x] = (cov.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    buf
}

/// Soft-edged filled disc: coverage `1` inside `rad`, ramping to `0` over ~1px.
fn disc(px: f32, py: f32, cx: f32, cy: f32, rad: f32) -> f32 {
    let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() - rad;
    (0.5 - d).clamp(0.0, 1.0)
}

/// A `+`-shaped sparkle: two centred bars of constant thickness.
fn plus(px: f32, py: f32, cx: f32, cy: f32, r: f32) -> f32 {
    let t = r * 0.30;
    let horiz = ((t - (py - cy).abs()) + 0.5).min((r - (px - cx).abs()) + 0.5);
    let vert = ((t - (px - cx).abs()) + 0.5).min((r - (py - cy).abs()) + 0.5);
    horiz.max(vert).clamp(0.0, 1.0)
}

/// An `n`-point sparkle built from tapering needles radiating from the centre,
/// plus a small core disc. `start` rotates the first point; `hw_frac` sets the
/// needle half-width at the base as a fraction of `r`.
#[allow(
    clippy::too_many_arguments,
    reason = "8 geometric scalars (point, centre, radius, point count, start angle, width) read more clearly as positional args than wrapped in a one-off struct"
)]
fn needle_star(
    px: f32,
    py: f32,
    cx: f32,
    cy: f32,
    r: f32,
    n: u32,
    start: f32,
    hw_frac: f32,
) -> f32 {
    let (dx, dy) = (px - cx, py - cy);
    let mut best = disc(px, py, cx, cy, r * 0.12);
    for k in 0..n {
        let ang = start + (k as f32) * std::f32::consts::TAU / (n as f32);
        let (s, c) = ang.sin_cos();
        let along = dx * c + dy * s;
        if along < 0.0 || along > r {
            continue;
        }
        let perp = (-dx * s + dy * c).abs();
        let hw = (r * hw_frac) * (1.0 - along / r) + 0.6;
        let cov = (hw - perp + 0.5).clamp(0.0, 1.0);
        if cov > best {
            best = cov;
        }
    }
    best
}

/// Soft-edged filled ellipse centred at `(ex, ey)` with radii `(rx, ry)`.
fn ellipse(px: f32, py: f32, ex: f32, ey: f32, rx: f32, ry: f32) -> f32 {
    let nx = (px - ex) / rx;
    let ny = (py - ey) / ry;
    let e = (nx * nx + ny * ny).sqrt();
    let d = (e - 1.0) * rx.min(ry);
    (0.5 - d).clamp(0.0, 1.0)
}

/// A cat's paw print: a large lower pad plus four toe beans above it.
fn paw(px: f32, py: f32, fw: f32, fh: f32) -> f32 {
    let cx = fw * 0.5;
    // Main pad, sitting low in the cell.
    let mut cov = ellipse(px, py, cx, fh * 0.66, fw * 0.30, fh * 0.24);
    // Four toe beans across the top, the outer pair slightly lower.
    let toe_r = fw * 0.135;
    let toes = [
        (cx - fw * 0.27, fh * 0.34),
        (cx - fw * 0.10, fh * 0.24),
        (cx + fw * 0.10, fh * 0.24),
        (cx + fw * 0.27, fh * 0.34),
    ];
    for (tx, ty) in toes {
        cov = cov.max(ellipse(px, py, tx, ty, toe_r, toe_r * 1.1));
    }
    cov
}

/// A water droplet / teardrop (the orca "splash" mark): a round bulb sitting low in the
/// cell, with a tapering point above it. The point's half-width grows linearly from `0`
/// at the tip to the bulb radius at the bulb centre, giving the classic teardrop.
fn droplet(px: f32, py: f32, fw: f32, fh: f32) -> f32 {
    let cx = fw * 0.5;
    let bulb_y = fh * 0.62;
    let r = fw.min(fh) * 0.30;
    let bulb = disc(px, py, cx, bulb_y, r);
    let tip_y = fh * 0.12;
    let point = if py >= tip_y && py <= bulb_y && bulb_y > tip_y {
        let half = r * ((py - tip_y) / (bulb_y - tip_y));
        (half - (px - cx).abs() + 0.5).clamp(0.0, 1.0)
    } else {
        0.0
    };
    bulb.max(point)
}

/// The Singularity nova's per-cell Over darkening mask (Sparkle Words v2
/// §6.1): a SOFT radial shadow — coverage `1` at the cell centre falling
/// quadratically to `0` at radius `r`. Centred in the cell and parameterized
/// only by the cell geometry, like every other deco glyph; the host builds
/// the visible collapse shadow by stamping one tinted decoration per covered
/// cell along the contracting ring, and the soft blobs fuse into a dark band.
/// (The original v2 mask was a hollow annulus per cell, which rendered as a
/// grid of faint "ghost rings" around the collapse — the v2.1 polish audit;
/// the glyph name stays for stream/parity stability.)
fn ring_arc(px: f32, py: f32, cx: f32, cy: f32, r: f32) -> f32 {
    let d2 = ((px - cx).powi(2) + (py - cy).powi(2)) / (r * r).max(1e-6);
    (1.0 - d2).clamp(0.0, 1.0)
}

/// The SUPER NOVA's light-background eclipse veil (Sparkle Words v3 §3.3): a
/// full-cell soft-edged square — coverage `1` across the cell interior,
/// ramping to ~half at the border texels over a ~1 px anti-aliased fringe on
/// all four edges. Coverage is the pixel centre's distance to the nearest
/// cell border, clamped to `0..=1`, so the square spans the WHOLE cell (no
/// inset, the shape's edge sits exactly on the cell border) with the same
/// ~1 px AA regime as the sibling soft masks ([`disc`]'s edge ramp).
/// (Named for the shape, not the glyph: `shade` is taken by the ░▒▓ shade
/// block-character painter.)
fn soft_square(px: f32, py: f32, fw: f32, fh: f32) -> f32 {
    px.min(fw - px).min(py.min(fh - py)).clamp(0.0, 1.0)
}

/// A coverage canvas: `w * h` bytes. The hard painters ([`Canvas::set`],
/// [`Canvas::rect`]) write 0/255 only; [`Canvas::paint_aa`] writes box-filtered
/// 8-bit coverage for the anti-aliased families.
struct Canvas {
    w: usize,
    h: usize,
    buf: Vec<u8>,
}

/// Subsamples per pixel axis for the AA families: 4× supersampling, box
/// filter (a pixel's coverage is the fraction of its 4×4 subsample grid the
/// shape contains, rounded to 8 bits).
const SS: usize = 4;

impl Canvas {
    fn new(w: usize, h: usize) -> Canvas {
        Canvas {
            w,
            h,
            buf: vec![0; w * h],
        }
    }

    fn set(&mut self, x: usize, y: usize) {
        if x < self.w && y < self.h {
            self.buf[y * self.w + x] = 255;
        }
    }

    /// Fill the half-open rect `[x0, x1) x [y0, y1)`, clamped to the canvas.
    fn rect(&mut self, x0: usize, y0: usize, x1: usize, y1: usize) {
        let (x1, y1) = (x1.min(self.w), y1.min(self.h));
        for y in y0..y1 {
            for x in x0..x1 {
                self.buf[y * self.w + x] = 255;
            }
        }
    }

    /// Paint an anti-aliased shape: `inside(px, py)` is evaluated at the 4×4
    /// subsample centres of every pixel (cell coordinates, y down) and
    /// box-filtered to 8-bit coverage. Union with whatever is already painted
    /// (max), so hard rects and AA shapes compose within one glyph.
    fn paint_aa(&mut self, inside: impl Fn(f32, f32) -> bool) {
        for y in 0..self.h {
            for x in 0..self.w {
                let mut hits = 0u32;
                for sy in 0..SS {
                    for sx in 0..SS {
                        let px = x as f32 + (sx as f32 + 0.5) / SS as f32;
                        let py = y as f32 + (sy as f32 + 0.5) / SS as f32;
                        if inside(px, py) {
                            hits += 1;
                        }
                    }
                }
                // round(hits * 255 / SS²): 16 hits -> 255, 8 -> 128, 0 -> 0.
                let cov = ((hits * 255 + (SS * SS) as u32 / 2) / (SS * SS) as u32) as u8;
                let i = y * self.w + x;
                if cov > self.buf[i] {
                    self.buf[i] = cov;
                }
            }
        }
    }

    /// Force the border rows/columns to hard 0/255 (majority coverage wins):
    /// the seam-tiling law. An AA glyph's interior may hold any coverage, but
    /// the texels ON the cell boundary — the only ones a neighbouring cell's
    /// glyph must meet — quantize so adjacent cells compose with no
    /// half-covered seam line (and so the CPU/GPU blends stay exact there).
    fn harden_edges(&mut self) {
        let (w, h) = (self.w, self.h);
        let mut harden = |i: usize| {
            self.buf[i] = if self.buf[i] >= 128 { 255 } else { 0 };
        };
        for x in 0..w {
            harden(x);
            harden((h - 1) * w + x);
        }
        for y in 0..h {
            harden(y * w);
            harden(y * w + (w - 1));
        }
    }
}

/// A centred stroke of thickness `t` across extent `e`: the half-open span
/// `[(e - t) / 2, (e - t) / 2 + t)`. THE placement rule (see module docs).
fn span(e: usize, t: usize) -> (usize, usize) {
    let t = t.min(e);
    let s = (e - t) / 2;
    (s, s + t)
}

/// `round(k * e / 8)` — the block-element eighth boundary (round half up).
fn eighth(k: u32, e: usize) -> usize {
    (k as usize * e + 4) / 8
}

/// Per-cell stroke geometry, all derived from the module's single rounding
/// rule so every glyph (and every neighbouring cell) agrees on positions.
struct Metrics {
    w: usize,
    h: usize,
    light: usize,
    heavy: usize,
    /// Light vertical stroke columns `[vl0, vl1)`.
    vl0: usize,
    vl1: usize,
    /// Light horizontal stroke rows `[hl0, hl1)`.
    hl0: usize,
    hl1: usize,
    /// Heavy vertical stroke columns `[vh0, vh1)` (also the double-line outer
    /// envelope: rails are its first/last `light` columns).
    vh0: usize,
    vh1: usize,
    /// Heavy horizontal stroke rows `[hh0, hh1)` (double-line envelope too).
    hh0: usize,
    hh1: usize,
}

impl Metrics {
    fn new(w: usize, h: usize) -> Metrics {
        let base = w.min(h);
        let light = ((base + 4) / 8).max(1);
        let heavy = (3 * light).min(base.max(1));
        let (vl0, vl1) = span(w, light);
        let (hl0, hl1) = span(h, light);
        let (vh0, vh1) = span(w, heavy);
        let (hh0, hh1) = span(h, heavy);
        Metrics {
            w,
            h,
            light,
            heavy,
            vl0,
            vl1,
            hl0,
            hl1,
            vh0,
            vh1,
            hh0,
            hh1,
        }
    }
}

/// One arm of a box-drawing junction: absent, light or heavy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arm {
    None,
    Light,
    Heavy,
}

impl Arm {
    fn thickness(self, m: &Metrics) -> Option<usize> {
        match self {
            Arm::None => None,
            Arm::Light => Some(m.light),
            Arm::Heavy => Some(m.heavy),
        }
    }
}

const N: Arm = Arm::None;
const L: Arm = Arm::Light;
const H: Arm = Arm::Heavy;

/// `[up, down, left, right]` arm weights for the solid light/heavy glyphs:
/// U+2500–254B (the dashed slots 2504–250B hold their solid equivalents;
/// `draw_box` intercepts those before this table is consulted) and the half
/// lines / weight transitions U+2574–257F.
fn arms(cp: u32) -> [Arm; 4] {
    #[rustfmt::skip]
    const TABLE: [[Arm; 4]; 0x4C] = [
        [N, N, L, L], // 2500 ─
        [N, N, H, H], // 2501 ━
        [L, L, N, N], // 2502 │
        [H, H, N, N], // 2503 ┃
        [N, N, L, L], // 2504 ┄ (dashed; intercepted)
        [N, N, H, H], // 2505 ┅ (dashed; intercepted)
        [L, L, N, N], // 2506 ┆ (dashed; intercepted)
        [H, H, N, N], // 2507 ┇ (dashed; intercepted)
        [N, N, L, L], // 2508 ┈ (dashed; intercepted)
        [N, N, H, H], // 2509 ┉ (dashed; intercepted)
        [L, L, N, N], // 250A ┊ (dashed; intercepted)
        [H, H, N, N], // 250B ┋ (dashed; intercepted)
        [N, L, N, L], // 250C ┌
        [N, L, N, H], // 250D ┍
        [N, H, N, L], // 250E ┎
        [N, H, N, H], // 250F ┏
        [N, L, L, N], // 2510 ┐
        [N, L, H, N], // 2511 ┑
        [N, H, L, N], // 2512 ┒
        [N, H, H, N], // 2513 ┓
        [L, N, N, L], // 2514 └
        [L, N, N, H], // 2515 ┕
        [H, N, N, L], // 2516 ┖
        [H, N, N, H], // 2517 ┗
        [L, N, L, N], // 2518 ┘
        [L, N, H, N], // 2519 ┙
        [H, N, L, N], // 251A ┚
        [H, N, H, N], // 251B ┛
        [L, L, N, L], // 251C ├
        [L, L, N, H], // 251D ┝
        [H, L, N, L], // 251E ┞
        [L, H, N, L], // 251F ┟
        [H, H, N, L], // 2520 ┠
        [H, L, N, H], // 2521 ┡
        [L, H, N, H], // 2522 ┢
        [H, H, N, H], // 2523 ┣
        [L, L, L, N], // 2524 ┤
        [L, L, H, N], // 2525 ┥
        [H, L, L, N], // 2526 ┦
        [L, H, L, N], // 2527 ┧
        [H, H, L, N], // 2528 ┨
        [H, L, H, N], // 2529 ┩
        [L, H, H, N], // 252A ┪
        [H, H, H, N], // 252B ┫
        [N, L, L, L], // 252C ┬
        [N, L, H, L], // 252D ┭
        [N, L, L, H], // 252E ┮
        [N, L, H, H], // 252F ┯
        [N, H, L, L], // 2530 ┰
        [N, H, H, L], // 2531 ┱
        [N, H, L, H], // 2532 ┲
        [N, H, H, H], // 2533 ┳
        [L, N, L, L], // 2534 ┴
        [L, N, H, L], // 2535 ┵
        [L, N, L, H], // 2536 ┶
        [L, N, H, H], // 2537 ┷
        [H, N, L, L], // 2538 ┸
        [H, N, H, L], // 2539 ┹
        [H, N, L, H], // 253A ┺
        [H, N, H, H], // 253B ┻
        [L, L, L, L], // 253C ┼
        [L, L, H, L], // 253D ┽
        [L, L, L, H], // 253E ┾
        [L, L, H, H], // 253F ┿
        [H, L, L, L], // 2540 ╀
        [L, H, L, L], // 2541 ╁
        [H, H, L, L], // 2542 ╂
        [H, L, H, L], // 2543 ╃
        [H, L, L, H], // 2544 ╄
        [L, H, H, L], // 2545 ╅
        [L, H, L, H], // 2546 ╆
        [H, L, H, H], // 2547 ╇
        [L, H, H, H], // 2548 ╈
        [H, H, H, L], // 2549 ╉
        [H, H, L, H], // 254A ╊
        [H, H, H, H], // 254B ╋
    ];
    #[rustfmt::skip]
    const HALF: [[Arm; 4]; 12] = [
        [N, N, L, N], // 2574 ╴
        [L, N, N, N], // 2575 ╵
        [N, N, N, L], // 2576 ╶
        [N, L, N, N], // 2577 ╷
        [N, N, H, N], // 2578 ╸
        [H, N, N, N], // 2579 ╹
        [N, N, N, H], // 257A ╺
        [N, H, N, N], // 257B ╻
        [N, N, L, H], // 257C ╼
        [L, H, N, N], // 257D ╽
        [N, N, H, L], // 257E ╾
        [H, L, N, N], // 257F ╿
    ];
    match cp {
        0x2500..=0x254B => TABLE[(cp - 0x2500) as usize],
        0x2574..=0x257F => HALF[(cp - 0x2574) as usize],
        _ => [N, N, N, N],
    }
}

/// Box drawing U+2500–257F.
fn draw_box(c: &mut Canvas, m: &Metrics, cp: u32) {
    match cp {
        0x2504 => dash_h(c, m, 3, m.light),
        0x2505 => dash_h(c, m, 3, m.heavy),
        0x2506 => dash_v(c, m, 3, m.light),
        0x2507 => dash_v(c, m, 3, m.heavy),
        0x2508 => dash_h(c, m, 4, m.light),
        0x2509 => dash_h(c, m, 4, m.heavy),
        0x250A => dash_v(c, m, 4, m.light),
        0x250B => dash_v(c, m, 4, m.heavy),
        0x254C => dash_h(c, m, 2, m.light),
        0x254D => dash_h(c, m, 2, m.heavy),
        0x254E => dash_v(c, m, 2, m.light),
        0x254F => dash_v(c, m, 2, m.heavy),
        0x2550..=0x256C => draw_double(c, m, cp),
        0x256D..=0x2570 => draw_arc(c, m, cp),
        0x2571 => draw_diag(c, m, true, false),
        0x2572 => draw_diag(c, m, false, true),
        0x2573 => draw_diag(c, m, true, true),
        _ => {
            let [up, down, left, right] = arms(cp);
            draw_arms(c, m, up, down, left, right);
        }
    }
}

/// Solid junctions: each present arm is a stroke from its cell edge through
/// the light centre span, so any combination joins solidly (the heavy span
/// contains the light span, and every arm reaches past the centre).
fn draw_arms(c: &mut Canvas, m: &Metrics, up: Arm, down: Arm, left: Arm, right: Arm) {
    if let Some(t) = up.thickness(m) {
        let (x0, x1) = span(m.w, t);
        c.rect(x0, 0, x1, m.hl1);
    }
    if let Some(t) = down.thickness(m) {
        let (x0, x1) = span(m.w, t);
        c.rect(x0, m.hl0, x1, m.h);
    }
    if let Some(t) = left.thickness(m) {
        let (y0, y1) = span(m.h, t);
        c.rect(0, y0, m.vl1, y1);
    }
    if let Some(t) = right.thickness(m) {
        let (y0, y1) = span(m.h, t);
        c.rect(m.vl0, y0, m.w, y1);
    }
}

/// Horizontal dashed line: `n` dashes, each centred in its `w/n` segment with
/// a `max(1, seg/3)` gap split across the segment ends (so the gaps stay
/// inside the cell — dashes are the one family that must NOT touch the seam).
fn dash_h(c: &mut Canvas, m: &Metrics, n: usize, t: usize) {
    let (y0, y1) = span(m.h, t);
    for i in 0..n {
        let s0 = i * m.w / n;
        let s1 = (i + 1) * m.w / n;
        let seg = s1.saturating_sub(s0);
        if seg == 0 {
            continue;
        }
        let gap = if seg >= 2 { (seg / 3).max(1) } else { 0 };
        let (g0, g1) = (gap / 2, gap - gap / 2);
        c.rect(s0 + g0, y0, s1 - g1, y1);
    }
}

/// Vertical dashed line (see [`dash_h`]).
fn dash_v(c: &mut Canvas, m: &Metrics, n: usize, t: usize) {
    let (x0, x1) = span(m.w, t);
    for i in 0..n {
        let s0 = i * m.h / n;
        let s1 = (i + 1) * m.h / n;
        let seg = s1.saturating_sub(s0);
        if seg == 0 {
            continue;
        }
        let gap = if seg >= 2 { (seg / 3).max(1) } else { 0 };
        let (g0, g1) = (gap / 2, gap - gap / 2);
        c.rect(x0, s0 + g0, x1, s1 - g1);
    }
}

/// Double-line glyphs U+2550–256C. Rails sit at the outer thirds of the heavy
/// envelope (`Metrics` docs); junction shapes follow the Unicode charts: outer
/// rails meet at the outer corner, inner rails at the inner corner, and a
/// branch breaks only the rail it attaches to.
fn draw_double(c: &mut Canvas, m: &Metrics, cp: u32) {
    let t = m.light;
    let (w, h) = (m.w, m.h);
    // Horizontal rail rows: top `[tr0, tr1)`, bottom `[br0, br1)`.
    let (tr0, tr1) = (m.hh0, m.hh0 + t);
    let (br0, br1) = (m.hh1 - t, m.hh1);
    // Vertical rail columns: left `[lr0, lr1)`, right `[rr0, rr1)`.
    let (lr0, lr1) = (m.vh0, m.vh0 + t);
    let (rr0, rr1) = (m.vh1 - t, m.vh1);
    match cp {
        0x2550 => {
            // ═
            c.rect(0, tr0, w, tr1);
            c.rect(0, br0, w, br1);
        }
        0x2551 => {
            // ║
            c.rect(lr0, 0, lr1, h);
            c.rect(rr0, 0, rr1, h);
        }
        0x2552 => {
            // ╒ down single, right double
            c.rect(m.vl0, tr0, w, tr1);
            c.rect(m.vl0, br0, w, br1);
            c.rect(m.vl0, tr0, m.vl1, h);
        }
        0x2553 => {
            // ╓ down double, right single
            c.rect(lr0, m.hl0, w, m.hl1);
            c.rect(lr0, m.hl0, lr1, h);
            c.rect(rr0, m.hl0, rr1, h);
        }
        0x2554 => {
            // ╔
            c.rect(lr0, tr0, w, tr1); // outer top rail
            c.rect(lr0, tr0, lr1, h); // outer left rail
            c.rect(rr0, br0, w, br1); // inner bottom rail
            c.rect(rr0, br0, rr1, h); // inner right rail
        }
        0x2555 => {
            // ╕ down single, left double
            c.rect(0, tr0, m.vl1, tr1);
            c.rect(0, br0, m.vl1, br1);
            c.rect(m.vl0, tr0, m.vl1, h);
        }
        0x2556 => {
            // ╖ down double, left single
            c.rect(0, m.hl0, rr1, m.hl1);
            c.rect(lr0, m.hl0, lr1, h);
            c.rect(rr0, m.hl0, rr1, h);
        }
        0x2557 => {
            // ╗
            c.rect(0, tr0, rr1, tr1); // outer top rail
            c.rect(rr0, tr0, rr1, h); // outer right rail
            c.rect(0, br0, lr1, br1); // inner bottom rail
            c.rect(lr0, br0, lr1, h); // inner left rail
        }
        0x2558 => {
            // ╘ up single, right double
            c.rect(m.vl0, tr0, w, tr1);
            c.rect(m.vl0, br0, w, br1);
            c.rect(m.vl0, 0, m.vl1, br1);
        }
        0x2559 => {
            // ╙ up double, right single
            c.rect(lr0, m.hl0, w, m.hl1);
            c.rect(lr0, 0, lr1, m.hl1);
            c.rect(rr0, 0, rr1, m.hl1);
        }
        0x255A => {
            // ╚
            c.rect(lr0, br0, w, br1); // outer bottom rail
            c.rect(lr0, 0, lr1, br1); // outer left rail
            c.rect(rr0, tr0, w, tr1); // inner top rail
            c.rect(rr0, 0, rr1, tr1); // inner right rail
        }
        0x255B => {
            // ╛ up single, left double
            c.rect(0, tr0, m.vl1, tr1);
            c.rect(0, br0, m.vl1, br1);
            c.rect(m.vl0, 0, m.vl1, br1);
        }
        0x255C => {
            // ╜ up double, left single
            c.rect(0, m.hl0, rr1, m.hl1);
            c.rect(lr0, 0, lr1, m.hl1);
            c.rect(rr0, 0, rr1, m.hl1);
        }
        0x255D => {
            // ╝
            c.rect(0, br0, rr1, br1); // outer bottom rail
            c.rect(rr0, 0, rr1, br1); // outer right rail
            c.rect(0, tr0, lr1, tr1); // inner top rail
            c.rect(lr0, 0, lr1, tr1); // inner left rail
        }
        0x255E => {
            // ╞ vertical single, right double
            c.rect(m.vl0, 0, m.vl1, h);
            c.rect(m.vl0, tr0, w, tr1);
            c.rect(m.vl0, br0, w, br1);
        }
        0x255F => {
            // ╟ vertical double, right single
            c.rect(lr0, 0, lr1, h);
            c.rect(rr0, 0, rr1, h);
            c.rect(rr0, m.hl0, w, m.hl1);
        }
        0x2560 => {
            // ╠
            c.rect(lr0, 0, lr1, h); // left rail, unbroken
            c.rect(rr0, 0, rr1, tr1); // right rail above the branch
            c.rect(rr0, br0, rr1, h); // right rail below the branch
            c.rect(rr0, tr0, w, tr1); // top branch rail
            c.rect(rr0, br0, w, br1); // bottom branch rail
        }
        0x2561 => {
            // ╡ vertical single, left double
            c.rect(m.vl0, 0, m.vl1, h);
            c.rect(0, tr0, m.vl1, tr1);
            c.rect(0, br0, m.vl1, br1);
        }
        0x2562 => {
            // ╢ vertical double, left single
            c.rect(lr0, 0, lr1, h);
            c.rect(rr0, 0, rr1, h);
            c.rect(0, m.hl0, lr1, m.hl1);
        }
        0x2563 => {
            // ╣
            c.rect(rr0, 0, rr1, h); // right rail, unbroken
            c.rect(lr0, 0, lr1, tr1); // left rail above the branch
            c.rect(lr0, br0, lr1, h); // left rail below the branch
            c.rect(0, tr0, lr1, tr1); // top branch rail
            c.rect(0, br0, lr1, br1); // bottom branch rail
        }
        0x2564 => {
            // ╤ down single, horizontal double
            c.rect(0, tr0, w, tr1);
            c.rect(0, br0, w, br1);
            c.rect(m.vl0, br0, m.vl1, h);
        }
        0x2565 => {
            // ╥ down double, horizontal single
            c.rect(0, m.hl0, w, m.hl1);
            c.rect(lr0, m.hl0, lr1, h);
            c.rect(rr0, m.hl0, rr1, h);
        }
        0x2566 => {
            // ╦
            c.rect(0, tr0, w, tr1); // top rail, unbroken
            c.rect(0, br0, lr1, br1); // bottom rail, left piece
            c.rect(rr0, br0, w, br1); // bottom rail, right piece
            c.rect(lr0, br0, lr1, h); // left descender
            c.rect(rr0, br0, rr1, h); // right descender
        }
        0x2567 => {
            // ╧ up single, horizontal double
            c.rect(0, tr0, w, tr1);
            c.rect(0, br0, w, br1);
            c.rect(m.vl0, 0, m.vl1, tr1);
        }
        0x2568 => {
            // ╨ up double, horizontal single
            c.rect(0, m.hl0, w, m.hl1);
            c.rect(lr0, 0, lr1, m.hl1);
            c.rect(rr0, 0, rr1, m.hl1);
        }
        0x2569 => {
            // ╩
            c.rect(0, br0, w, br1); // bottom rail, unbroken
            c.rect(0, tr0, lr1, tr1); // top rail, left piece
            c.rect(rr0, tr0, w, tr1); // top rail, right piece
            c.rect(lr0, 0, lr1, tr1); // left ascender
            c.rect(rr0, 0, rr1, tr1); // right ascender
        }
        0x256A => {
            // ╪ vertical single threads both rails
            c.rect(m.vl0, 0, m.vl1, h);
            c.rect(0, tr0, w, tr1);
            c.rect(0, br0, w, br1);
        }
        0x256B => {
            // ╫ horizontal single threads both rails
            c.rect(0, m.hl0, w, m.hl1);
            c.rect(lr0, 0, lr1, h);
            c.rect(rr0, 0, rr1, h);
        }
        0x256C => {
            // ╬ four corner pieces around an open centre
            c.rect(lr0, 0, lr1, tr1);
            c.rect(0, tr0, lr1, tr1);
            c.rect(rr0, 0, rr1, tr1);
            c.rect(rr0, tr0, w, tr1);
            c.rect(lr0, br0, lr1, h);
            c.rect(0, br0, lr1, br1);
            c.rect(rr0, br0, rr1, h);
            c.rect(rr0, br0, w, br1);
        }
        _ => unreachable!("draw_double covers 0x2550..=0x256C"),
    }
}

/// Light arcs U+256D–2570: the two straight half-arms of the matching corner,
/// joined by a quarter circle whose radius is the largest that fits between
/// the centre cross and the cell edges. The arc band is at least one pixel
/// wide at every angle so the curve stays connected at `light == 1`. The
/// curved band is supersampled ([`Canvas::paint_aa`], radial distance — i.e.
/// perpendicular to the curve); the straight stubs stay hard rects, so the
/// arm meets a neighbouring `─`/`│` stroke bit-exactly at the (hardened)
/// cell edge.
fn draw_arc(c: &mut Canvas, m: &Metrics, cp: u32) {
    let t = m.light as f32;
    let vmid = (m.vl0 + m.vl1) as f32 / 2.0;
    let hmid = (m.hl0 + m.hl1) as f32 / 2.0;
    let (wf, hf) = (m.w as f32, m.h as f32);
    // Arm directions: +1 = right/down. ╭ down+right, ╮ down+left,
    // ╯ up+left, ╰ up+right.
    let (dx, dy): (f32, f32) = match cp {
        0x256D => (1.0, 1.0),
        0x256E => (-1.0, 1.0),
        0x256F => (-1.0, -1.0),
        0x2570 => (1.0, -1.0),
        _ => unreachable!("draw_arc covers 0x256D..=0x2570"),
    };
    let rx = if dx > 0.0 { wf - vmid } else { vmid };
    let ry = if dy > 0.0 { hf - hmid } else { hmid };
    let r = rx.min(ry).max(t);
    // Centre of curvature, displaced from the stroke cross toward the corner
    // the arms point at; the arc joins the vertical stroke at y = cy and the
    // horizontal stroke at x = cx.
    let cx = vmid + dx * r;
    let cy = hmid + dy * r;
    let half = (t / 2.0).max(0.71);
    c.paint_aa(|px, py| {
        // Keep only the quarter between the two arm joints.
        if dx * (cx - px) < 0.0 || dy * (cy - py) < 0.0 {
            return false;
        }
        let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
        (d - r).abs() <= half
    });
    // Straight stubs from the arc joints to the cell edges (the floor/ceil
    // overlaps the arc end by up to a pixel, never gaps).
    if dx > 0.0 {
        c.rect((cx.floor() as usize).min(m.w), m.hl0, m.w, m.hl1);
    } else {
        c.rect(0, m.hl0, (cx.ceil().max(0.0) as usize).min(m.w), m.hl1);
    }
    if dy > 0.0 {
        c.rect(m.vl0, (cy.floor() as usize).min(m.h), m.vl1, m.h);
    } else {
        c.rect(m.vl0, 0, m.vl1, (cy.ceil().max(0.0) as usize).min(m.h));
    }
}

/// Light diagonals U+2571–2573, corner to corner. A subsample is lit when it
/// lies within half a light stroke of the ideal line, measured PERPENDICULAR
/// to it (floored at 0.6 so the line stays visually connected — and meets the
/// cell corners, so diagonals in adjacent cells chain without a break; the
/// corner texels are edge-hardened back to 255).
fn draw_diag(c: &mut Canvas, m: &Metrics, fwd: bool, back: bool) {
    let (wf, hf) = (m.w as f32, m.h as f32);
    let half = (m.light as f32 / 2.0).max(0.6);
    let norm = (wf * wf + hf * hf).sqrt();
    c.paint_aa(|px, py| {
        // ╱ runs (0, h) -> (w, 0): h*x + w*y - w*h = 0.
        let df = (hf * px + wf * py - wf * hf).abs() / norm;
        // ╲ runs (0, 0) -> (w, h): h*x - w*y = 0.
        let db = (hf * px - wf * py).abs() / norm;
        (fwd && df <= half) || (back && db <= half)
    });
}

/// Block elements U+2580–259F.
fn draw_block(c: &mut Canvas, cp: u32) {
    let (w, h) = (c.w, c.h);
    match cp {
        0x2580 => c.rect(0, 0, w, eighth(4, h)), // ▀ upper half
        0x2581..=0x2588 => {
            // ▁▂▃▄▅▆▇█ lower k/8, anchored to the bottom edge
            let k = cp - 0x2580;
            c.rect(0, h - eighth(k, h), w, h);
        }
        0x2589..=0x258F => {
            // ▉▊▋▌▍▎▏ left k/8, anchored to the left edge
            let k = 8 - (cp - 0x2588);
            c.rect(0, 0, eighth(k, w), h);
        }
        0x2590 => c.rect(w - eighth(4, w), 0, w, h), // ▐ right half
        // ░▒▓ (0x2591..=0x2593) are intercepted by `coverage_phased` (they
        // carry the absolute-parity dither phase) before this table.
        0x2594 => c.rect(0, 0, w, eighth(1, h)), // ▔ upper eighth
        0x2595 => c.rect(w - eighth(1, w), 0, w, h), // ▕ right eighth
        0x2596..=0x259F => quadrants(c, cp),
        _ => unreachable!("draw_block covers 0x2580..=0x259F"),
    }
}

/// The shade dithers: 1 = ░ 25% (even x AND even y), 2 = ▒ 50% checkerboard,
/// 3 = ▓ 75% (the complement of the 25% pattern's odd/odd holes). The pattern
/// is keyed by ABSOLUTE framebuffer parity: `phase_x`/`phase_y` (each 0 or 1)
/// are the parity of the destination cell's top-left pixel, so `x + phase_x ≡
/// absolute x (mod 2)` and the dither is one uniform lattice across the whole
/// grid — an odd cell width no longer doubles a dither line at every seam
/// (cell-local parity restarted the pattern per cell). Phase `(0, 0)`
/// reproduces the old pattern byte-for-byte.
fn shade(c: &mut Canvas, level: u8, phase_x: usize, phase_y: usize) {
    for y in 0..c.h {
        for x in 0..c.w {
            let (ax, ay) = (x + phase_x, y + phase_y);
            let on = match level {
                1 => ax % 2 == 0 && ay % 2 == 0,
                2 => (ax + ay) % 2 == 0,
                _ => !(ax % 2 == 1 && ay % 2 == 1),
            };
            if on {
                c.set(x, y);
            }
        }
    }
}

/// Quadrant blocks U+2596–259F: each lit quadrant is a half-by-half rect
/// anchored to its own corner and sized `eighth(4, ..)` (round half up), so
/// unions never leave an interior seam.
fn quadrants(c: &mut Canvas, cp: u32) {
    // bit 0 = upper-left, 1 = upper-right, 2 = lower-left, 3 = lower-right.
    let bits: u8 = match cp {
        0x2596 => 0b0100, // ▖
        0x2597 => 0b1000, // ▗
        0x2598 => 0b0001, // ▘
        0x2599 => 0b1101, // ▙
        0x259A => 0b1001, // ▚
        0x259B => 0b0111, // ▛
        0x259C => 0b1011, // ▜
        0x259D => 0b0010, // ▝
        0x259E => 0b0110, // ▞
        0x259F => 0b1110, // ▟
        _ => unreachable!("quadrants covers 0x2596..=0x259F"),
    };
    let (w, h) = (c.w, c.h);
    let (mw, mh) = (eighth(4, w), eighth(4, h));
    if bits & 0b0001 != 0 {
        c.rect(0, 0, mw, mh);
    }
    if bits & 0b0010 != 0 {
        c.rect(w - mw, 0, w, mh);
    }
    if bits & 0b0100 != 0 {
        c.rect(0, h - mh, mw, h);
    }
    if bits & 0b1000 != 0 {
        c.rect(w - mw, h - mh, w, h);
    }
}

/// Distance from `(px, py)` to the SEGMENT `(ax, ay)..(bx, by)` — the
/// perpendicular distance to the line, clamped to the endpoints. The stroke
/// primitive for the Powerline chevrons: a band `dist <= t/2` has thickness
/// `t` PERPENDICULAR to the edge at every slope (a per-row horizontal fill of
/// width `t` degenerates to `t / sqrt(1 + slope²)` — a beaded chain at 45°).
fn seg_dist(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (ax + t * dx, ay + t * dy);
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

/// Powerline separators U+E0B0–E0BF, synthesized full-bleed so they tile
/// seamlessly with the adjacent segment's background (the glyph paints the
/// segment colour; the rest of the cell shows the next segment's bg):
/// E0B0/E0B2 solid right/left triangles, E0B1/E0B3 their chevron outlines,
/// E0B4/E0B6 solid right/left rounded (half-ellipse) caps, E0B5/E0B7
/// outlines, E0B8–E0BF the corner ("angled") triangles + their hypotenuse
/// outlines. All are supersampled ([`Canvas::paint_aa`]); outline strokes
/// measure their thickness PERPENDICULAR to the edge ([`seg_dist`] / radial),
/// so a 45° chevron keeps a constant-width stroke instead of thinning into a
/// beaded chain of per-row fills. Cell-edge texels are hardened by the caller.
fn draw_powerline(c: &mut Canvas, cp: u32) {
    let (w, h) = (c.w, c.h);
    let (wf, hf) = (w as f32, h as f32);
    let mid = hf / 2.0;
    // Stroke thickness for the outline variants — the box "light" rule,
    // floored like the diagonals so tiny cells keep a connected line.
    let t = (((w.min(h) + 4) / 8) as f32).max(1.0);
    let half = (t / 2.0).max(0.6);

    // Corner ("angled") triangles E0B8–E0BF: the cell is split corner-to-corner
    // by a diagonal and one side is filled (odd code points draw the hypotenuse
    // as a centred perpendicular stroke — the same geometry as U+2571/2572).
    if (0xE0B8..=0xE0BF).contains(&cp) {
        let norm = (wf * wf + hf * hf).sqrt();
        // Signed distances: `dmain` to the main diagonal (0,0)->(w,h), positive
        // below it; `danti` to the anti-diagonal (0,h)->(w,0), positive below.
        c.paint_aa(|px, py| {
            let dmain = (wf * py - hf * px) / norm;
            let danti = (hf * px + wf * py - wf * hf) / norm;
            match cp {
                0xE0B8 => dmain >= 0.0,        // lower-left solid
                0xE0B9 => dmain.abs() <= half, // lower-left outline (╲ stroke)
                0xE0BA => danti >= 0.0,        // lower-right solid
                0xE0BB => danti.abs() <= half, // lower-right outline (╱ stroke)
                0xE0BC => danti <= 0.0,        // upper-left solid
                0xE0BD => danti.abs() <= half, // upper-left outline (╱ stroke)
                0xE0BE => dmain <= 0.0,        // upper-right solid
                _ => dmain.abs() <= half,      // E0BF upper-right outline (╲)
            }
        });
        return;
    }
    let right = matches!(cp, 0xE0B0 | 0xE0B1 | 0xE0B4 | 0xE0B5); // apex right
    let rounded = matches!(cp, 0xE0B4..=0xE0B7);
    let outline = matches!(cp, 0xE0B1 | 0xE0B3 | 0xE0B5 | 0xE0B7);
    // Right-pointing layout: flat side at x=0, apex at (w, mid). The
    // left-pointing twin is the exact mirror (px -> w - px), which keeps the
    // E0B0/E0B2 mirror-image law bit-exact under the symmetric subsample grid.
    c.paint_aa(|px, py| {
        let px = if right { px } else { wf - px };
        if rounded {
            // Half-ellipse cap: radii (w, mid), centred at (0, mid).
            let (nx, ny) = (px / wf, (py - mid) / mid);
            let e = (nx * nx + ny * ny).sqrt();
            if outline {
                // Radial distance scaled by the smaller radius — the
                // perpendicular stroke width bound, as in `ellipse`.
                ((e - 1.0) * wf.min(mid)).abs() <= half
            } else {
                e <= 1.0
            }
        } else if outline {
            // Chevron `>`: stroke the two hypotenuse segments
            // (0,0)->(w,mid) and (w,mid)->(0,h) at perpendicular width t.
            seg_dist(px, py, 0.0, 0.0, wf, mid).min(seg_dist(px, py, wf, mid, 0.0, hf)) <= half
        } else {
            // Solid triangle: inside the linear taper from both edges.
            px <= wf * (1.0 - ((py - mid).abs() / mid))
        }
    });
}

/// Block sextants U+1FB00–1FB3B: a 2×3 grid of filled sub-cells. The 60 code
/// points map to the 6-bit fill masks 1..=62 EXCLUDING 21 (left column) and 42
/// (right column) — those, plus 0 (space) and 63 (full block), have their own
/// characters. Bits: 1=upper-left 2=upper-right 4=mid-left 8=mid-right
/// 16=lower-left 32=lower-right. Sub-cells fully tile the cell (no AA) so the
/// CPU==GPU exactness holds and adjacent sextants seam perfectly.
fn draw_sextant(c: &mut Canvas, cp: u32) {
    let k = cp - 0x1FB00;
    // The k-th mask in 1..=62 skipping the two whole-column masks.
    let mut mask = 0u32;
    let mut idx = 0u32;
    for cand in 1..=62u32 {
        if cand == 21 || cand == 42 {
            continue;
        }
        if idx == k {
            mask = cand;
            break;
        }
        idx += 1;
    }
    let (w, h) = (c.w, c.h);
    let xm = eighth(4, w); // shared half-column boundary (matches ▌/▐)
    let y1 = (h + 1) / 3; // upper/middle split (round)
    let y2 = (2 * h + 1) / 3; // middle/lower split (round)
    if mask & 1 != 0 {
        c.rect(0, 0, xm, y1);
    }
    if mask & 2 != 0 {
        c.rect(xm, 0, w, y1);
    }
    if mask & 4 != 0 {
        c.rect(0, y1, xm, y2);
    }
    if mask & 8 != 0 {
        c.rect(xm, y1, w, y2);
    }
    if mask & 16 != 0 {
        c.rect(0, y2, xm, h);
    }
    if mask & 32 != 0 {
        c.rect(xm, y2, w, h);
    }
}

/// Block diagonal wedges + triangular blocks U+1FB3C–1FB6F (Symbols for
/// Legacy Computing), supersampled.
///
/// U+1FB3C–1FB67 ("<CORNER> BLOCK DIAGONAL <P1> TO <P2>") fill the side of
/// the `P1 -> P2` diagonal that contains the named corner. The endpoints lie
/// on the cell-edge sixth lattice: x ∈ {0, w/2, w}, y ∈ {0, h/3, 2h/3, h} —
/// stored as sixths `(x·6/w, y·6/h)`. Complementary pairs (the same diagonal
/// with opposite corners, e.g. U+1FB3C/U+1FB52) tile the cell with no gap:
/// both predicates are closed half-planes, so every subsample is claimed by
/// at least one of the pair.
///
/// U+1FB68–1FB6F are the quarter/three-quarter triangles cut by BOTH cell
/// diagonals: 6C/6D/6E/6F the left/upper/right/lower quarter, 68/69/6A/6B
/// everything BUT the (strict) left/upper/right/lower quarter.
fn draw_wedge(c: &mut Canvas, cp: u32) {
    let (wf, hf) = (c.w as f32, c.h as f32);
    // Quarter triangles: sign vs the main diagonal (s1 > 0 below (0,0)->(w,h))
    // and the anti-diagonal (s2 > 0 below (0,h)->(w,0)).
    if cp >= 0x1FB68 {
        let quarter = |px: f32, py: f32, strict: bool| -> [bool; 4] {
            let s1 = py * wf - px * hf;
            let s2 = py * wf + px * hf - wf * hf;
            if strict {
                // Open quarters, for the three-quarter complements.
                [
                    s1 > 0.0 && s2 < 0.0, // left
                    s1 < 0.0 && s2 < 0.0, // upper
                    s1 < 0.0 && s2 > 0.0, // right
                    s1 > 0.0 && s2 > 0.0, // lower
                ]
            } else {
                [
                    s1 >= 0.0 && s2 <= 0.0,
                    s1 <= 0.0 && s2 <= 0.0,
                    s1 <= 0.0 && s2 >= 0.0,
                    s1 >= 0.0 && s2 >= 0.0,
                ]
            }
        };
        c.paint_aa(|px, py| match cp {
            0x1FB68 => !quarter(px, py, true)[0], // all but left
            0x1FB69 => !quarter(px, py, true)[1], // all but upper
            0x1FB6A => !quarter(px, py, true)[2], // all but right
            0x1FB6B => !quarter(px, py, true)[3], // all but lower
            0x1FB6C => quarter(px, py, false)[0], // left quarter
            0x1FB6D => quarter(px, py, false)[1], // upper quarter
            0x1FB6E => quarter(px, py, false)[2], // right quarter
            _ => quarter(px, py, false)[3],       // 1FB6F lower quarter
        });
        return;
    }
    // ((x1, y1), (x2, y2), (cx, cy)) in cell sixths: the diagonal endpoints
    // and the corner the filled half-plane must contain. Derived one-to-one
    // from the Unicode 13 names (UL (0,0), UC (3,0), UR (6,0), UML (0,2),
    // UMR (6,2), LML (0,4), LMR (6,4), LL (0,6), LC (3,6), LR (6,6)).
    type WedgeSixths = ((i32, i32), (i32, i32), (i32, i32));
    #[rustfmt::skip]
    const WEDGES: [WedgeSixths; 44] = [
        ((0, 4), (3, 6), (0, 6)), // 1FB3C lower left,  LML to LC
        ((0, 4), (6, 6), (0, 6)), // 1FB3D lower left,  LML to LR
        ((0, 2), (3, 6), (0, 6)), // 1FB3E lower left,  UML to LC
        ((0, 2), (6, 6), (0, 6)), // 1FB3F lower left,  UML to LR
        ((0, 0), (3, 6), (0, 6)), // 1FB40 lower left,  UL  to LC
        ((0, 2), (3, 0), (6, 6)), // 1FB41 lower right, UML to UC
        ((0, 2), (6, 0), (6, 6)), // 1FB42 lower right, UML to UR
        ((0, 4), (3, 0), (6, 6)), // 1FB43 lower right, LML to UC
        ((0, 4), (6, 0), (6, 6)), // 1FB44 lower right, LML to UR
        ((0, 6), (3, 0), (6, 6)), // 1FB45 lower right, LL  to UC
        ((0, 4), (6, 2), (6, 6)), // 1FB46 lower right, LML to UMR
        ((3, 6), (6, 4), (6, 6)), // 1FB47 lower right, LC  to LMR
        ((0, 6), (6, 4), (6, 6)), // 1FB48 lower right, LL  to LMR
        ((3, 6), (6, 2), (6, 6)), // 1FB49 lower right, LC  to UMR
        ((0, 6), (6, 2), (6, 6)), // 1FB4A lower right, LL  to UMR
        ((3, 6), (6, 0), (6, 6)), // 1FB4B lower right, LC  to UR
        ((3, 0), (6, 2), (0, 6)), // 1FB4C lower left,  UC  to UMR
        ((0, 0), (6, 2), (0, 6)), // 1FB4D lower left,  UL  to UMR
        ((3, 0), (6, 4), (0, 6)), // 1FB4E lower left,  UC  to LMR
        ((0, 0), (6, 4), (0, 6)), // 1FB4F lower left,  UL  to LMR
        ((3, 0), (6, 6), (0, 6)), // 1FB50 lower left,  UC  to LR
        ((0, 2), (6, 4), (0, 6)), // 1FB51 lower left,  UML to LMR
        ((0, 4), (3, 6), (6, 0)), // 1FB52 upper right, LML to LC
        ((0, 4), (6, 6), (6, 0)), // 1FB53 upper right, LML to LR
        ((0, 2), (3, 6), (6, 0)), // 1FB54 upper right, UML to LC
        ((0, 2), (6, 6), (6, 0)), // 1FB55 upper right, UML to LR
        ((0, 0), (3, 6), (6, 0)), // 1FB56 upper right, UL  to LC
        ((0, 2), (3, 0), (0, 0)), // 1FB57 upper left,  UML to UC
        ((0, 2), (6, 0), (0, 0)), // 1FB58 upper left,  UML to UR
        ((0, 4), (3, 0), (0, 0)), // 1FB59 upper left,  LML to UC
        ((0, 4), (6, 0), (0, 0)), // 1FB5A upper left,  LML to UR
        ((0, 6), (3, 0), (0, 0)), // 1FB5B upper left,  LL  to UC
        ((0, 4), (6, 2), (0, 0)), // 1FB5C upper left,  LML to UMR
        ((3, 6), (6, 4), (0, 0)), // 1FB5D upper left,  LC  to LMR
        ((0, 6), (6, 4), (0, 0)), // 1FB5E upper left,  LL  to LMR
        ((3, 6), (6, 2), (0, 0)), // 1FB5F upper left,  LC  to UMR
        ((0, 6), (6, 2), (0, 0)), // 1FB60 upper left,  LL  to UMR
        ((3, 6), (6, 0), (0, 0)), // 1FB61 upper left,  LC  to UR
        ((3, 0), (6, 2), (6, 0)), // 1FB62 upper right, UC  to UMR
        ((0, 0), (6, 2), (6, 0)), // 1FB63 upper right, UL  to UMR
        ((3, 0), (6, 4), (6, 0)), // 1FB64 upper right, UC  to LMR
        ((0, 0), (6, 4), (6, 0)), // 1FB65 upper right, UL  to LMR
        ((3, 0), (6, 6), (6, 0)), // 1FB66 upper right, UC  to LR
        ((0, 2), (6, 4), (6, 0)), // 1FB67 upper right, UML to LMR
    ];
    let ((x1, y1), (x2, y2), (cx, cy)) = WEDGES[(cp - 0x1FB3C) as usize];
    let sx = |n: i32| n as f32 * wf / 6.0;
    let sy = |n: i32| n as f32 * hf / 6.0;
    let (ax, ay, bx, by) = (sx(x1), sy(y1), sx(x2), sy(y2));
    // Cross product side test; the corner fixes the filled (closed) half-plane.
    let side = move |px: f32, py: f32| (bx - ax) * (py - ay) - (by - ay) * (px - ax);
    let corner = side(sx(cx), sy(cy));
    c.paint_aa(|px, py| side(px, py) * corner >= 0.0);
}

/// Legacy eighth blocks U+1FB70–1FB8B (Symbols for Legacy Computing) —
/// ORTHOGONAL (hard 0/255) fills on the same `eighth` rounding rule as
/// U+2580–259F, so they seam exactly with the classic eighth blocks:
/// vertical/horizontal one-eighth bars 2..7 (1 and 8 live at U+258F/2595 and
/// U+2594/2581), the edge-pair combinations, the 1-3-5-8 scanline set, and
/// the upper/right k-eighths fills.
fn draw_legacy_block(c: &mut Canvas, cp: u32) {
    let (w, h) = (c.w, c.h);
    match cp {
        0x1FB70..=0x1FB75 => {
            // VERTICAL ONE EIGHTH BLOCK-k: the k-th eighth column band.
            let k = cp - 0x1FB70 + 2;
            c.rect(eighth(k - 1, w), 0, eighth(k, w), h);
        }
        0x1FB76..=0x1FB7B => {
            // HORIZONTAL ONE EIGHTH BLOCK-k: the k-th eighth row band.
            let k = cp - 0x1FB76 + 2;
            c.rect(0, eighth(k - 1, h), w, eighth(k, h));
        }
        0x1FB7C..=0x1FB7F => {
            // LEFT/RIGHT + LOWER/UPPER ONE EIGHTH BLOCK: an edge column plus
            // an edge row.
            let (ew, eh) = (eighth(1, w), eighth(1, h));
            if matches!(cp, 0x1FB7C | 0x1FB7D) {
                c.rect(0, 0, ew, h); // left eighth
            } else {
                c.rect(w - ew, 0, w, h); // right eighth
            }
            if matches!(cp, 0x1FB7C | 0x1FB7F) {
                c.rect(0, h - eh, w, h); // lower eighth
            } else {
                c.rect(0, 0, w, eh); // upper eighth
            }
        }
        0x1FB80 => {
            // UPPER AND LOWER ONE EIGHTH BLOCK.
            c.rect(0, 0, w, eighth(1, h));
            c.rect(0, h - eighth(1, h), w, h);
        }
        0x1FB81 => {
            // HORIZONTAL ONE EIGHTH BLOCK-1358: scanline rows 1, 3, 5 and 8.
            for k in [1u32, 3, 5, 8] {
                c.rect(0, eighth(k - 1, h), w, eighth(k, h));
            }
        }
        0x1FB82..=0x1FB86 => {
            // UPPER k-EIGHTHS BLOCK, k in {2,3,5,6,7} (4 is ▀, 8 is █).
            const K: [u32; 5] = [2, 3, 5, 6, 7];
            c.rect(0, 0, w, eighth(K[(cp - 0x1FB82) as usize], h));
        }
        0x1FB87..=0x1FB8B => {
            // RIGHT k-EIGHTHS BLOCK, k in {2,3,5,6,7} (4 is ▐, 8 is █).
            const K: [u32; 5] = [2, 3, 5, 6, 7];
            c.rect(w - eighth(K[(cp - 0x1FB87) as usize], w), 0, w, h);
        }
        _ => unreachable!("draw_legacy_block covers 0x1FB70..=0x1FB8B"),
    }
}

/// Braille U+2800–28FF: bit `n` of `cp - 0x2800` lights dot `n+1` in the
/// standard 2x4 layout (dots 1-3 left column top-down, 4-6 right column,
/// 7/8 the bottom pair). Each dot is a centred square-ish fill covering about
/// half of its 2x4 grid compartment.
fn draw_braille(c: &mut Canvas, cp: u32) {
    let bits = cp - 0x2800;
    let (w, h) = (c.w, c.h);
    // (column, row) of each dot bit.
    const DOTS: [(usize, usize); 8] = [
        (0, 0),
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 1),
        (1, 2),
        (0, 3),
        (1, 3),
    ];
    let xb = |i: usize| (i * w).div_ceil(2); // column band boundaries (round half up == div_ceil for /2)
    let yb = |i: usize| (i * h + 2) / 4; // row band boundaries (round half up)
    for (bit, &(col, row)) in DOTS.iter().enumerate() {
        if bits & (1 << bit) == 0 {
            continue;
        }
        let (x0, x1) = (xb(col), xb(col + 1));
        let (y0, y1) = (yb(row), yb(row + 1));
        let (bw, bh) = (x1 - x0, y1 - y0);
        if bw == 0 || bh == 0 {
            continue; // cell too small for this dot's compartment
        }
        let dw = bw.div_ceil(2);
        let dh = bh.div_ceil(2);
        let dx = x0 + (bw - dw) / 2;
        let dy = y0 + (bh - dh) / 2;
        c.rect(dx, dy, dx + dw, dy + dh);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The orca "splash" glyph is a teardrop: a narrow tip up top widening to a round
    /// bulb at the bottom. Prints ASCII art (`cargo test droplet -- --nocapture`).
    #[test]
    fn droplet_is_a_teardrop() {
        let (w, h) = (16usize, 18usize);
        let cov = deco_coverage(crate::DecoGlyph::Droplet, w, h);
        let mut art = String::from("\n");
        for y in 0..h {
            for x in 0..w {
                let c = cov[y * w + x];
                art.push(if c > 160 {
                    '#'
                } else if c > 50 {
                    '.'
                } else {
                    ' '
                });
            }
            art.push('\n');
        }
        println!("{art}");
        let filled = |row: usize| (0..w).filter(|&x| cov[row * w + x] > 128).count();
        assert!(
            filled(h * 3 / 4) > filled(h / 6),
            "the bulb (lower) must be wider than the tip (upper): {art}"
        );
    }

    /// The Singularity darkening mask is a SOFT radial shadow (v2.1 polish —
    /// the earlier hollow-annulus mask stamped visible "ghost rings" per
    /// cell): full coverage at the cell centre, monotone quadratic falloff
    /// outward, symmetric about the centre, and empty in the corners so
    /// neighbouring stamps fuse without box seams.
    #[test]
    fn ring_arc_is_a_soft_radial_shadow() {
        let (w, h) = (16usize, 18usize);
        let cov = deco_coverage(crate::DecoGlyph::RingArc, w, h);
        let at = |x: usize, y: usize| cov[y * w + x];
        let (cx, cy) = (w / 2, h / 2);
        assert!(at(cx, cy) > 220, "the shadow peaks at the cell centre");
        assert!(
            at(cx, cy) > at(cx - 4, cy) && at(cx - 4, cy) > at(cx - 6, cy),
            "coverage falls monotonically from the centre outward"
        );
        // Pixel CENTRES sample the field: x = cx−7 and x = cx+6 sit at the
        // same 6.5 px distance from the centre (w even ⇒ the ±k columns are
        // half-a-pixel asymmetric; ±(k, k−1) is the equidistant pair).
        assert_eq!(
            at(cx - 7, cy),
            at(cx + 6, cy),
            "the falloff is centre-symmetric"
        );
        assert!(
            at(cx - 6, cy) > 0,
            "the skirt still contributes (blobs must fuse)"
        );
        assert_eq!(at(0, 0), 0, "the corners lie outside the radius");
        assert_eq!(at(w - 1, h - 1), 0, "all corners are empty");
    }

    /// The SUPER NOVA eclipse veil (v3 §3.3) is a full-cell soft-edged
    /// square: every interior texel (a full pixel in from the border)
    /// saturates at 255, and the one-texel border ring on all four edges is a
    /// soft — partial but non-zero — AA fringe, so the veil covers the whole
    /// cell without reading as a hard box.
    #[test]
    fn shade_is_a_full_cell_soft_square() {
        let (w, h) = (16usize, 18usize);
        let cov = deco_coverage(crate::DecoGlyph::Shade, w, h);
        let at = |x: usize, y: usize| cov[y * w + x];
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                assert_eq!(at(x, y), 255, "interior texel ({x},{y}) must saturate");
            }
        }
        let mut edges: Vec<(usize, usize)> = Vec::new();
        edges.extend((0..w).flat_map(|x| [(x, 0), (x, h - 1)]));
        edges.extend((0..h).flat_map(|y| [(0, y), (w - 1, y)]));
        for (x, y) in edges {
            let c = at(x, y);
            assert!(
                c > 0 && c < 255,
                "border texel ({x},{y}) must be a soft partial fringe, got {c}"
            );
        }
    }

    /// Cell sizes exercised by the invariants below: odd/even mixes, squat,
    /// tall, and degenerate-tiny.
    const SIZES: &[(usize, usize)] = &[
        (1, 1),
        (2, 2),
        (3, 7),
        (7, 15),
        (8, 16),
        (9, 19),
        (10, 20),
        (11, 21),
        (12, 22),
        (20, 8),
    ];

    fn all_procedural_chars() -> impl Iterator<Item = char> {
        (0x2500u32..=0x259F)
            .chain(0x2800..=0x28FF)
            .chain(0x1FB00..=0x1FB8B)
            .chain(0xE0B0..=0xE0BF)
            .map(|cp| char::from_u32(cp).unwrap())
    }

    /// THE coverage contract, per family: every glyph, at every size, is
    /// exactly cell-sized; the ORTHOGONAL families contain only hard 0/255
    /// coverage (the property that makes CPU and GPU blending bit-identical
    /// on those cells); the [`antialiased`] families are hard 0/255 on their
    /// CELL-EDGE texels (the seam-tiling law — proven over the exhaustive
    /// size lattice in `tests/procedural_aa_edges.rs`).
    #[test]
    fn every_glyph_is_cell_sized_and_hard_where_required() {
        for &(w, h) in SIZES {
            for ch in all_procedural_chars() {
                let cov = coverage(ch, w, h)
                    .unwrap_or_else(|| panic!("{ch:?} must be procedural at {w}x{h}"));
                assert_eq!(cov.len(), w * h, "{ch:?} at {w}x{h}: wrong size");
                if antialiased(ch) {
                    let hard_edge = (0..w)
                        .flat_map(|x| [x, (h - 1) * w + x])
                        .chain((0..h).flat_map(|y| [y * w, y * w + w - 1]))
                        .all(|i| cov[i] == 0 || cov[i] == 255);
                    assert!(hard_edge, "{ch:?} at {w}x{h}: soft cell-edge texel");
                } else {
                    assert!(
                        cov.iter().all(|&b| b == 0 || b == 255),
                        "{ch:?} at {w}x{h}: non-hard coverage byte"
                    );
                }
            }
        }
        // Non-vacuity: the AA regime is real — a diagonal at a chunky size has
        // at least one INTERIOR texel that is neither 0 nor 255.
        let cov = coverage('╱', 11, 21).unwrap();
        assert!(
            (1..20).any(|y| (1..10).any(|x| !matches!(cov[y * 11 + x], 0 | 255))),
            "╱ at 11x21 must carry interior anti-aliasing"
        );
    }

    /// Chars outside the covered blocks are not intercepted.
    #[test]
    fn non_procedural_chars_are_not_covered() {
        for ch in [
            'A',
            ' ',
            '日',
            '\u{24FF}',
            '\u{25A0}',
            '\u{27FF}',
            '\u{2900}',
            '\u{1FB8C}',
            '\u{E0C0}',
        ] {
            assert!(!covers(ch), "{ch:?} must stay font-rendered");
            assert!(coverage(ch, 8, 16).is_none());
        }
    }

    /// Solid lines reach both cell edges (the seam guarantee in one cell).
    #[test]
    fn solid_lines_touch_their_edges() {
        for &(w, h) in SIZES {
            for ch in ['─', '━'] {
                let cov = coverage(ch, w, h).unwrap();
                let lit_col = |x: usize| (0..h).any(|y| cov[y * w + x] != 0);
                assert!(
                    lit_col(0) && lit_col(w - 1),
                    "{ch:?} at {w}x{h} must span the width"
                );
            }
            for ch in ['│', '┃', '║'] {
                let cov = coverage(ch, w, h).unwrap();
                let lit_row = |y: usize| (0..w).any(|x| cov[y * w + x] != 0);
                assert!(
                    lit_row(0) && lit_row(h - 1),
                    "{ch:?} at {w}x{h} must span the height"
                );
            }
        }
    }

    /// The heavy stroke span exactly contains the light span on both axes —
    /// the parity property the module's rounding rule promises.
    #[test]
    fn heavy_span_contains_light_span_centred() {
        for &(w, h) in SIZES {
            let m = Metrics::new(w, h);
            assert!(
                m.vh0 <= m.vl0 && m.vl1 <= m.vh1,
                "{w}x{h}: vertical containment"
            );
            assert!(
                m.hh0 <= m.hl0 && m.hl1 <= m.hh1,
                "{w}x{h}: horizontal containment"
            );
            if m.heavy == 3 * m.light {
                assert_eq!(m.vl0 - m.vh0, m.vh1 - m.vl1, "{w}x{h}: vertical centring");
                assert_eq!(m.hl0 - m.hh0, m.hh1 - m.hl1, "{w}x{h}: horizontal centring");
            }
        }
    }

    /// The full block is all-255; the empty braille pattern is all-0.
    #[test]
    fn full_block_and_braille_blank_are_extremes() {
        for &(w, h) in SIZES {
            assert!(coverage('█', w, h).unwrap().iter().all(|&b| b == 255));
            assert!(coverage('\u{2800}', w, h).unwrap().iter().all(|&b| b == 0));
        }
    }

    /// ▀/▄ and ▌/▐ cover the whole cell between them (overlap allowed on odd
    /// extents, gaps never) — the half-block tiling rule.
    #[test]
    fn complementary_halves_leave_no_gap() {
        for &(w, h) in SIZES {
            let top = coverage('▀', w, h).unwrap();
            let bottom = coverage('▄', w, h).unwrap();
            assert!(
                top.iter().zip(&bottom).all(|(&a, &b)| a == 255 || b == 255),
                "{w}x{h}: ▀+▄ must tile the cell"
            );
            let left = coverage('▌', w, h).unwrap();
            let right = coverage('▐', w, h).unwrap();
            assert!(
                left.iter().zip(&right).all(|(&a, &b)| a == 255 || b == 255),
                "{w}x{h}: ▌+▐ must tile the cell"
            );
        }
    }

    /// Braille dots land in their compartments: dot 1 is top-left, dot 8 is
    /// bottom-right, and they never bleed across the column midline.
    #[test]
    fn braille_dot_positions() {
        let (w, h): (usize, usize) = (10, 20);
        let mid_x = w.div_ceil(2);
        let d1 = coverage('\u{2801}', w, h).unwrap(); // dot 1: left column, top row
        let d8 = coverage('\u{2880}', w, h).unwrap(); // dot 8: right column, bottom row
        let lit = |cov: &[u8]| {
            (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .filter(|&(x, y)| cov[y * w + x] != 0)
                .collect::<Vec<_>>()
        };
        let l1 = lit(&d1);
        let l8 = lit(&d8);
        assert!(!l1.is_empty() && !l8.is_empty());
        assert!(
            l1.iter().all(|&(x, y)| x < mid_x && y < h / 4 + 1),
            "dot 1 confined to top-left"
        );
        assert!(
            l8.iter().all(|&(x, y)| x >= mid_x && y >= 3 * h / 4 - 1),
            "dot 8 confined to bottom-right"
        );
    }

    /// The shades dither at their nominal densities (exact for even dims).
    #[test]
    fn shades_have_correct_density() {
        let (w, h) = (8, 16);
        let count = |ch: char| {
            coverage(ch, w, h)
                .unwrap()
                .iter()
                .filter(|&&b| b == 255)
                .count()
        };
        assert_eq!(count('░'), w * h / 4);
        assert_eq!(count('▒'), w * h / 2);
        assert_eq!(count('▓'), 3 * w * h / 4);
    }

    /// Sextants are covered, hard 0/255, and laid out on the 2×3 grid: U+1FB00
    /// fills ONLY the upper-left sub-cell; the next-to-last (mask 62) fills all
    /// but the upper-left. Every one of the 60 draws ink and they are distinct.
    #[test]
    fn sextants_fill_the_2x3_grid() {
        let (w, h) = (12, 24);
        let mut seen = std::collections::HashSet::new();
        for cp in 0x1FB00u32..=0x1FB3B {
            let ch = char::from_u32(cp).unwrap();
            assert!(covers(ch), "U+{cp:04X} covered");
            let cov = coverage(ch, w, h).unwrap();
            assert_eq!(cov.len(), w * h);
            assert!(cov.iter().all(|&b| b == 0 || b == 255), "hard coverage");
            assert!(cov.contains(&255), "U+{cp:04X} draws ink");
            assert!(
                seen.insert(cov.clone()),
                "U+{cp:04X} duplicates another sextant"
            );
        }
        let at = |cov: &[u8], x: usize, y: usize| cov[y * w + x] == 255;
        let ul = coverage('\u{1FB00}', w, h).unwrap(); // upper-left only
        assert!(at(&ul, 0, 0), "U+1FB00 fills upper-left");
        assert!(
            !at(&ul, w - 1, 0) && !at(&ul, 0, h - 1),
            "U+1FB00 only upper-left"
        );
    }

    /// Powerline separators are covered, anti-aliased with hard cell edges,
    /// and shaped: a SOLID right triangle (E0B0) is widest at the vertical
    /// middle and empty at the edges, fills the apex column at mid-height, and
    /// is the EXACT byte mirror of the solid left triangle (E0B2). Outlines
    /// (E0B1) carry far less ink than the solid fill.
    #[test]
    fn powerline_separators_are_shaped() {
        let (w, h) = (12, 24);
        for cp in 0xE0B0u32..=0xE0BF {
            let ch = char::from_u32(cp).unwrap();
            assert!(covers(ch), "U+{cp:04X} must be covered");
            assert!(antialiased(ch), "U+{cp:04X} is an AA-family glyph");
            let cov = coverage(ch, w, h).unwrap();
            assert_eq!(cov.len(), w * h);
            assert!(cov.contains(&255), "U+{cp:04X} draws ink");
        }
        let at = |cov: &[u8], x: usize, y: usize| cov[y * w + x] == 255;
        // Corner triangles fill the named corner and leave the opposite empty.
        let ll = coverage('\u{E0B8}', w, h).unwrap(); // lower-left
        assert!(
            at(&ll, 0, h - 1) && !at(&ll, w - 1, 0),
            "E0B8 fills lower-left"
        );
        let ur = coverage('\u{E0BE}', w, h).unwrap(); // upper-right
        assert!(
            at(&ur, w - 1, 0) && !at(&ur, 0, h - 1),
            "E0BE fills upper-right"
        );
        let right = coverage('\u{E0B0}', w, h).unwrap();
        let left = coverage('\u{E0B2}', w, h).unwrap();
        // Apex column lit at mid-height, empty near the top row.
        assert!(at(&right, w - 1, h / 2), "E0B0 apex at right-middle");
        assert!(!at(&right, w - 1, 0), "E0B0 empty at top-right");
        assert!(at(&left, 0, h / 2), "E0B2 apex at left-middle");
        // Mirror image: row by row, right(x) == left(w-1-x) — BYTE-exact (the
        // subsample grid is symmetric, so AA coverage mirrors too).
        for y in 0..h {
            for x in 0..w {
                assert_eq!(
                    right[y * w + x],
                    left[y * w + (w - 1 - x)],
                    "E0B0/E0B2 mirror at ({x},{y})"
                );
            }
        }
        // The outline carries strictly less ink than the solid fill.
        let ink = |cov: &[u8]| cov.iter().map(|&b| usize::from(b)).sum::<usize>();
        assert!(
            ink(&coverage('\u{E0B1}', w, h).unwrap()) < ink(&right),
            "outline < solid"
        );
    }

    /// The chevron outline keeps a CONSTANT PERPENDICULAR stroke width: at a
    /// squat cell (steep ~45°+ edges) every interior row of E0B1 carries at
    /// least a full stroke's worth of ink and consecutive rows' ink intervals
    /// overlap (a connected band). The pre-fix per-row horizontal fill of
    /// width `t` degenerated at these slopes into a beaded chain — rows held
    /// only `t` ink and the band broke between rows.
    #[test]
    fn chevron_outline_width_is_perpendicular_not_horizontal() {
        for (w, h) in [(20usize, 8usize), (16, 10), (12, 24), (9, 19)] {
            let t = ((w.min(h) + 4) / 8).max(1);
            for ch in ['\u{E0B1}', '\u{E0B3}'] {
                let cov = coverage(ch, w, h).unwrap();
                // slope of the chevron edge in x-per-y: w / (h/2).
                let slope = w as f32 / (h as f32 / 2.0);
                // 0.55: headroom for apex/edge clipping (the band near the
                // apex extends past the cell and is cut) — still far above
                // the pre-fix per-row ink of `t * 255` at these slopes.
                let want = (t as f32 * (1.0 + slope * slope).sqrt() * 255.0 * 0.55) as usize;
                let mut prev: Option<(usize, usize)> = None;
                for y in 1..h - 1 {
                    let row = &cov[y * w..(y + 1) * w];
                    let ink: usize = row.iter().map(|&b| usize::from(b)).sum();
                    assert!(
                        ink >= want,
                        "{ch:?} at {w}x{h} row {y}: ink {ink} < {want} — stroke thinned \
                         (horizontal-width beading)"
                    );
                    let lit: Vec<usize> = (0..w).filter(|&x| row[x] != 0).collect();
                    let span = (lit[0], lit[lit.len() - 1]);
                    if let Some((p0, p1)) = prev {
                        assert!(
                            span.0 <= p1 + 1 && p0 <= span.1 + 1,
                            "{ch:?} at {w}x{h} rows {}/{y}: stroke disconnected (beaded)",
                            y - 1
                        );
                    }
                    prev = Some(span);
                }
            }
        }
    }

    /// Wedges (U+1FB3C–1FB67) obey the complementary-pair tiling law: each
    /// diagonal appears in exactly two glyphs filling opposite sides, and the
    /// pair composes to full coverage — per-pixel coverage sums to ≥ 254
    /// (± one rounding LSB in the interior), with the shared cell-edge texels
    /// hardened so at least one side owns each edge texel fully. Also spot
    /// checks orientation and the quarter/three-quarter triangles.
    #[test]
    fn wedge_pairs_tile_and_orient() {
        // (glyph, complement) — same diagonal, opposite corner.
        let pairs: Vec<(u32, u32)> = (0x1FB3C..=0x1FB51).zip(0x1FB52..=0x1FB67).collect();
        for &(w, h) in SIZES {
            for &(a, b) in &pairs {
                let ca = coverage(char::from_u32(a).unwrap(), w, h).unwrap();
                let cb = coverage(char::from_u32(b).unwrap(), w, h).unwrap();
                for i in 0..w * h {
                    let sum = usize::from(ca[i]) + usize::from(cb[i]);
                    assert!(
                        sum >= 254,
                        "U+{a:04X}+U+{b:04X} at {w}x{h} px {i}: gap (sum {sum})"
                    );
                }
            }
        }
        let (w, h) = (12, 24);
        let at = |cov: &[u8], x: usize, y: usize| cov[y * w + x] == 255;
        // 1FB3C: small lower-left triangle — bottom-left lit, top row empty.
        let c3c = coverage('\u{1FB3C}', w, h).unwrap();
        assert!(at(&c3c, 0, h - 1), "1FB3C fills the lower-left corner");
        assert!(
            !at(&c3c, w - 1, 0) && !at(&c3c, w - 1, h - 1),
            "1FB3C stays lower-left"
        );
        // 1FB6C: left quarter triangle — left-middle lit, right-middle empty.
        let c6c = coverage('\u{1FB6C}', w, h).unwrap();
        assert!(at(&c6c, 0, h / 2), "1FB6C fills the left edge middle");
        assert!(!at(&c6c, w - 1, h / 2), "1FB6C leaves the right edge");
        // 1FB68 is its complement-ish 3/4 block: right-middle lit, left-middle empty.
        let c68 = coverage('\u{1FB68}', w, h).unwrap();
        assert!(at(&c68, w - 1, h / 2), "1FB68 fills the right edge middle");
        assert!(!at(&c68, 0, h / 2), "1FB68 leaves the left edge middle");
        // Quarter + three-quarter tile the cell too.
        for i in 0..w * h {
            let sum = usize::from(c6c[i]) + usize::from(c68[i]);
            assert!(sum >= 254, "1FB6C+1FB68 px {i}: gap (sum {sum})");
        }
    }

    /// Legacy eighth blocks (U+1FB70–1FB8B) are ORTHOGONAL: hard 0/255, on
    /// the same `eighth` boundaries as U+2580–259F. Spot checks: vertical
    /// block-2 occupies exactly the second eighth column band; the 1358
    /// scanline set lights exactly rows {1,3,5,8}; UPPER ONE QUARTER aligns
    /// to `eighth(2, h)`; RIGHT ONE QUARTER anchors to the right edge.
    #[test]
    fn legacy_eighth_blocks_are_hard_and_aligned() {
        let (w, h) = (16, 32);
        for cp in 0x1FB70u32..=0x1FB8B {
            let ch = char::from_u32(cp).unwrap();
            assert!(covers(ch) && !antialiased(ch), "U+{cp:04X} orthogonal");
            let cov = coverage(ch, w, h).unwrap();
            assert!(cov.iter().all(|&b| b == 0 || b == 255), "U+{cp:04X} hard");
            assert!(cov.contains(&255), "U+{cp:04X} draws ink");
        }
        let lit = |cov: &[u8], x: usize, y: usize| cov[y * w + x] == 255;
        let v2 = coverage('\u{1FB70}', w, h).unwrap();
        for y in 0..h {
            for x in 0..w {
                let want = (eighth(1, w)..eighth(2, w)).contains(&x);
                assert_eq!(lit(&v2, x, y), want, "1FB70 at ({x},{y})");
            }
        }
        let scan = coverage('\u{1FB81}', w, h).unwrap();
        for y in 0..h {
            let want = [1u32, 3, 5, 8]
                .iter()
                .any(|&k| (eighth(k - 1, h)..eighth(k, h)).contains(&y));
            assert_eq!(lit(&scan, 0, y), want, "1FB81 row {y}");
        }
        let uq = coverage('\u{1FB82}', w, h).unwrap();
        for y in 0..h {
            assert_eq!(lit(&uq, 0, y), y < eighth(2, h), "1FB82 row {y}");
        }
        let rq = coverage('\u{1FB87}', w, h).unwrap();
        for x in 0..w {
            assert_eq!(lit(&rq, x, 0), x >= w - eighth(2, w), "1FB87 col {x}");
        }
    }
}
