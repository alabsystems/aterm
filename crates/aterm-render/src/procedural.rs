// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Procedural box-drawing, block-element, and braille glyphs routed by
//! [`crate::Renderer::glyph_key`] to [`crate::FaceId::Procedural`], with [`antialiased`] curves
//! ([`Canvas::harden_edges`]) and [`coverage_phased`] shade dithers.

mod icons;
mod symbols;

#[cfg(test)]
pub(crate) use icons::mark_stroke;
pub use icons::{IconMetrics, band_icon_coverage, band_icon_width, boxed_icon_metrics};

/// Whether `ch` is a last-resort symbol this module can draw without a font.
#[inline]
pub const fn covers_symbol(ch: char) -> bool {
    symbols::covers(ch)
}

/// Whether `ch` is one of Claude Code's spinner frames the symbol set draws (`✢ ✳ ✶ ✻ ✽`).
#[inline]
pub const fn is_spinner_star(ch: char) -> bool {
    symbols::is_spinner_star(ch)
}

/// The [`is_spinner_star`] frames, `✢ ✳ ✶ ✻ ✽`.
pub const SPINNER_STARS: [char; 5] = symbols::SPINNER_STARS;

/// The spinner frame Claude Code rests on.
pub const SPINNER_REST: char = '\u{273B}';

/// The last-resort coverage bitmap for a [`covers_symbol`] char.
#[inline]
pub fn symbol_coverage(ch: char, cell_w: usize, cell_h: usize, span: usize) -> Option<Vec<u8>> {
    symbols::coverage(ch, cell_w, cell_h, span)
}

/// Whether `ch` is in a range this module draws.
#[inline]
pub const fn covers(ch: char) -> bool {
    matches!(
        ch as u32,
        0x23BF
            | 0x2500..=0x259F
            | 0x2800..=0x28FF
            | 0x1FB00..=0x1FB8B
            | 0x1CD00..=0x1CDE5
            | 0xE0B0..=0xE0BF
    )
}

/// Whether `ch` is in an anti-aliased (diagonal or curved) family.
#[inline]
pub const fn antialiased(ch: char) -> bool {
    matches!(
        ch as u32,
        0x256D..=0x2573 | 0x1FB3C..=0x1FB6F | 0xE0B0..=0xE0BF
    )
}

/// The procedural coverage bitmap for `ch` at a `cell_w x cell_h` cell.
pub fn coverage(ch: char, cell_w: usize, cell_h: usize) -> Option<Vec<u8>> {
    coverage_phased(ch, cell_w, cell_h, false, false)
}

/// [`coverage`] with the shade-dither phase explicit.
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
        0x23BF => symbols::draw_dentistry_bottom_right(&mut c),
        0x2500..=0x257F => draw_box(&mut c, &m, cp),
        0x2580..=0x259F => draw_block(&mut c, cp),
        0x2800..=0x28FF => draw_braille(&mut c, cp),
        0x1FB00..=0x1FB3B => draw_sextant(&mut c, cp),
        0x1FB3C..=0x1FB6F => draw_wedge(&mut c, cp),
        0x1FB70..=0x1FB8B => draw_legacy_block(&mut c, cp),
        0x1CD00..=0x1CDE5 => draw_octant(&mut c, OCTANT_MASKS[(cp - 0x1CD00) as usize]),
        0xE0B0..=0xE0BF => draw_powerline(&mut c, cp),
        _ => unreachable!("covers() gates the ranges"),
    }
    if antialiased(ch) {
        c.harden_edges();
    }
    Some(c.buf)
}

/// The anti-aliased coverage sprite (`0..=255` per byte, row-major `cell_w * cell_h`).
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

/// An `n`-point sparkle built from tapering needles radiating from the centre.
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
    let mut cov = ellipse(px, py, cx, fh * 0.66, fw * 0.30, fh * 0.24);
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

/// The Singularity nova's per-cell Over darkening mask: a soft
/// radial shadow, coverage `1` at the cell centre falling quadratically to `0` at `r`.
fn ring_arc(px: f32, py: f32, cx: f32, cy: f32, r: f32) -> f32 {
    let d2 = ((px - cx).powi(2) + (py - cy).powi(2)) / (r * r).max(1e-6);
    (1.0 - d2).clamp(0.0, 1.0)
}

/// The super nova's light-background eclipse veil: a full-cell
/// square whose coverage is the pixel centre's distance to the nearest cell border,
/// clamped to `0..=1`, so it fills the cell with a ~1 px fringe like [`disc`]'s.
fn soft_square(px: f32, py: f32, fw: f32, fh: f32) -> f32 {
    px.min(fw - px).min(py.min(fh - py)).clamp(0.0, 1.0)
}

/// A coverage canvas: `w * h` bytes.
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
    /// box-filtered to 8-bit coverage.
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
                let cov = ((hits * 255 + (SS * SS) as u32 / 2) / (SS * SS) as u32) as u8;
                let i = y * self.w + x;
                if cov > self.buf[i] {
                    self.buf[i] = cov;
                }
            }
        }
    }

    /// Force the border rows and columns to hard 0/255 (majority wins): the seam-tiling
    /// law.
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
/// `[(e - t) / 2, (e - t) / 2 + t)`, the module's placement rule.
#[inline]
const fn span(e: usize, t: usize) -> (usize, usize) {
    let t = if t < e { t } else { e };
    let s = (e - t) / 2;
    (s, s + t)
}

/// `round(k * e / 8)` — the block-element eighth boundary (round half up).
#[inline]
const fn eighth(k: u32, e: usize) -> usize {
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
    /// Heavy vertical stroke columns `[vh0, vh1)` (also the double-line outer envelope).
    vh0: usize,
    vh1: usize,
    /// Heavy horizontal stroke rows `[hh0, hh1)` (also the double-line outer envelope).
    hh0: usize,
    hh1: usize,
}

impl Metrics {
    #[inline]
    const fn new(w: usize, h: usize) -> Metrics {
        let base = if w < h { w } else { h };
        let l0 = (base + 4) / 8;
        let light = if l0 > 1 { l0 } else { 1 };
        let b1 = if base > 1 { base } else { 1 };
        let h0 = 3 * light;
        let heavy = if h0 < b1 { h0 } else { b1 };
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
    #[inline]
    const fn thickness(self, m: &Metrics) -> Option<usize> {
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
#[inline]
const fn arms(cp: u32) -> [Arm; 4] {
    #[rustfmt::skip]
    const TABLE: [[Arm; 4]; 0x4C] = [
        [N, N, L, L],
        [N, N, H, H],
        [L, L, N, N],
        [H, H, N, N],
        [N, N, L, L],
        [N, N, H, H],
        [L, L, N, N],
        [H, H, N, N],
        [N, N, L, L],
        [N, N, H, H],
        [L, L, N, N],
        [H, H, N, N],
        [N, L, N, L],
        [N, L, N, H],
        [N, H, N, L],
        [N, H, N, H],
        [N, L, L, N],
        [N, L, H, N],
        [N, H, L, N],
        [N, H, H, N],
        [L, N, N, L],
        [L, N, N, H],
        [H, N, N, L],
        [H, N, N, H],
        [L, N, L, N],
        [L, N, H, N],
        [H, N, L, N],
        [H, N, H, N],
        [L, L, N, L],
        [L, L, N, H],
        [H, L, N, L],
        [L, H, N, L],
        [H, H, N, L],
        [H, L, N, H],
        [L, H, N, H],
        [H, H, N, H],
        [L, L, L, N],
        [L, L, H, N],
        [H, L, L, N],
        [L, H, L, N],
        [H, H, L, N],
        [H, L, H, N],
        [L, H, H, N],
        [H, H, H, N],
        [N, L, L, L],
        [N, L, H, L],
        [N, L, L, H],
        [N, L, H, H],
        [N, H, L, L],
        [N, H, H, L],
        [N, H, L, H],
        [N, H, H, H],
        [L, N, L, L],
        [L, N, H, L],
        [L, N, L, H],
        [L, N, H, H],
        [H, N, L, L],
        [H, N, H, L],
        [H, N, L, H],
        [H, N, H, H],
        [L, L, L, L],
        [L, L, H, L],
        [L, L, L, H],
        [L, L, H, H],
        [H, L, L, L],
        [L, H, L, L],
        [H, H, L, L],
        [H, L, H, L],
        [H, L, L, H],
        [L, H, H, L],
        [L, H, L, H],
        [H, L, H, H],
        [L, H, H, H],
        [H, H, H, L],
        [H, H, L, H],
        [H, H, H, H],
    ];
    #[rustfmt::skip]
    const HALF: [[Arm; 4]; 12] = [
        [N, N, L, N],
        [L, N, N, N],
        [N, N, N, L],
        [N, L, N, N],
        [N, N, H, N],
        [H, N, N, N],
        [N, N, N, H],
        [N, H, N, N],
        [N, N, L, H],
        [L, H, N, N],
        [N, N, H, L],
        [H, L, N, N],
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

/// Double-line glyphs U+2550–256C.
fn draw_double(c: &mut Canvas, m: &Metrics, cp: u32) {
    let t = m.light;
    let (w, h) = (m.w, m.h);
    let (tr0, tr1) = (m.hh0, m.hh0 + t);
    let (br0, br1) = (m.hh1 - t, m.hh1);
    let (lr0, lr1) = (m.vh0, m.vh0 + t);
    let (rr0, rr1) = (m.vh1 - t, m.vh1);
    match cp {
        0x2550 => {
            c.rect(0, tr0, w, tr1);
            c.rect(0, br0, w, br1);
        }
        0x2551 => {
            c.rect(lr0, 0, lr1, h);
            c.rect(rr0, 0, rr1, h);
        }
        0x2552 => {
            c.rect(m.vl0, tr0, w, tr1);
            c.rect(m.vl0, br0, w, br1);
            c.rect(m.vl0, tr0, m.vl1, h);
        }
        0x2553 => {
            c.rect(lr0, m.hl0, w, m.hl1);
            c.rect(lr0, m.hl0, lr1, h);
            c.rect(rr0, m.hl0, rr1, h);
        }
        0x2554 => {
            c.rect(lr0, tr0, w, tr1);
            c.rect(lr0, tr0, lr1, h);
            c.rect(rr0, br0, w, br1);
            c.rect(rr0, br0, rr1, h);
        }
        0x2555 => {
            c.rect(0, tr0, m.vl1, tr1);
            c.rect(0, br0, m.vl1, br1);
            c.rect(m.vl0, tr0, m.vl1, h);
        }
        0x2556 => {
            c.rect(0, m.hl0, rr1, m.hl1);
            c.rect(lr0, m.hl0, lr1, h);
            c.rect(rr0, m.hl0, rr1, h);
        }
        0x2557 => {
            c.rect(0, tr0, rr1, tr1);
            c.rect(rr0, tr0, rr1, h);
            c.rect(0, br0, lr1, br1);
            c.rect(lr0, br0, lr1, h);
        }
        0x2558 => {
            c.rect(m.vl0, tr0, w, tr1);
            c.rect(m.vl0, br0, w, br1);
            c.rect(m.vl0, 0, m.vl1, br1);
        }
        0x2559 => {
            c.rect(lr0, m.hl0, w, m.hl1);
            c.rect(lr0, 0, lr1, m.hl1);
            c.rect(rr0, 0, rr1, m.hl1);
        }
        0x255A => {
            c.rect(lr0, br0, w, br1);
            c.rect(lr0, 0, lr1, br1);
            c.rect(rr0, tr0, w, tr1);
            c.rect(rr0, 0, rr1, tr1);
        }
        0x255B => {
            c.rect(0, tr0, m.vl1, tr1);
            c.rect(0, br0, m.vl1, br1);
            c.rect(m.vl0, 0, m.vl1, br1);
        }
        0x255C => {
            c.rect(0, m.hl0, rr1, m.hl1);
            c.rect(lr0, 0, lr1, m.hl1);
            c.rect(rr0, 0, rr1, m.hl1);
        }
        0x255D => {
            c.rect(0, br0, rr1, br1);
            c.rect(rr0, 0, rr1, br1);
            c.rect(0, tr0, lr1, tr1);
            c.rect(lr0, 0, lr1, tr1);
        }
        0x255E => {
            c.rect(m.vl0, 0, m.vl1, h);
            c.rect(m.vl0, tr0, w, tr1);
            c.rect(m.vl0, br0, w, br1);
        }
        0x255F => {
            c.rect(lr0, 0, lr1, h);
            c.rect(rr0, 0, rr1, h);
            c.rect(rr0, m.hl0, w, m.hl1);
        }
        0x2560 => {
            c.rect(lr0, 0, lr1, h);
            c.rect(rr0, 0, rr1, tr1);
            c.rect(rr0, br0, rr1, h);
            c.rect(rr0, tr0, w, tr1);
            c.rect(rr0, br0, w, br1);
        }
        0x2561 => {
            c.rect(m.vl0, 0, m.vl1, h);
            c.rect(0, tr0, m.vl1, tr1);
            c.rect(0, br0, m.vl1, br1);
        }
        0x2562 => {
            c.rect(lr0, 0, lr1, h);
            c.rect(rr0, 0, rr1, h);
            c.rect(0, m.hl0, lr1, m.hl1);
        }
        0x2563 => {
            c.rect(rr0, 0, rr1, h);
            c.rect(lr0, 0, lr1, tr1);
            c.rect(lr0, br0, lr1, h);
            c.rect(0, tr0, lr1, tr1);
            c.rect(0, br0, lr1, br1);
        }
        0x2564 => {
            c.rect(0, tr0, w, tr1);
            c.rect(0, br0, w, br1);
            c.rect(m.vl0, br0, m.vl1, h);
        }
        0x2565 => {
            c.rect(0, m.hl0, w, m.hl1);
            c.rect(lr0, m.hl0, lr1, h);
            c.rect(rr0, m.hl0, rr1, h);
        }
        0x2566 => {
            c.rect(0, tr0, w, tr1);
            c.rect(0, br0, lr1, br1);
            c.rect(rr0, br0, w, br1);
            c.rect(lr0, br0, lr1, h);
            c.rect(rr0, br0, rr1, h);
        }
        0x2567 => {
            c.rect(0, tr0, w, tr1);
            c.rect(0, br0, w, br1);
            c.rect(m.vl0, 0, m.vl1, tr1);
        }
        0x2568 => {
            c.rect(0, m.hl0, w, m.hl1);
            c.rect(lr0, 0, lr1, m.hl1);
            c.rect(rr0, 0, rr1, m.hl1);
        }
        0x2569 => {
            c.rect(0, br0, w, br1);
            c.rect(0, tr0, lr1, tr1);
            c.rect(rr0, tr0, w, tr1);
            c.rect(lr0, 0, lr1, tr1);
            c.rect(rr0, 0, rr1, tr1);
        }
        0x256A => {
            c.rect(m.vl0, 0, m.vl1, h);
            c.rect(0, tr0, w, tr1);
            c.rect(0, br0, w, br1);
        }
        0x256B => {
            c.rect(0, m.hl0, w, m.hl1);
            c.rect(lr0, 0, lr1, h);
            c.rect(rr0, 0, rr1, h);
        }
        0x256C => {
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
/// the centre cross and the cell edges.
fn draw_arc(c: &mut Canvas, m: &Metrics, cp: u32) {
    let t = m.light as f32;
    let vmid = (m.vl0 + m.vl1) as f32 / 2.0;
    let hmid = (m.hl0 + m.hl1) as f32 / 2.0;
    let (wf, hf) = (m.w as f32, m.h as f32);
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

/// Light diagonals U+2571–2573, corner to corner.
fn draw_diag(c: &mut Canvas, m: &Metrics, fwd: bool, back: bool) {
    let (wf, hf) = (m.w as f32, m.h as f32);
    let half = (m.light as f32 / 2.0).max(0.6);
    let norm = (wf * wf + hf * hf).sqrt();
    c.paint_aa(|px, py| {
        let df = (hf * px + wf * py - wf * hf).abs() / norm;
        let db = (hf * px - wf * py).abs() / norm;
        (fwd && df <= half) || (back && db <= half)
    });
}

/// Block elements U+2580–259F.
fn draw_block(c: &mut Canvas, cp: u32) {
    let (w, h) = (c.w, c.h);
    match cp {
        0x2580 => c.rect(0, 0, w, eighth(4, h)),
        0x2581..=0x2588 => {
            let k = cp - 0x2580;
            c.rect(0, h - eighth(k, h), w, h);
        }
        0x2589..=0x258F => {
            let k = 8 - (cp - 0x2588);
            c.rect(0, 0, eighth(k, w), h);
        }
        0x2590 => c.rect(w - eighth(4, w), 0, w, h),
        0x2594 => c.rect(0, 0, w, eighth(1, h)),
        0x2595 => c.rect(w - eighth(1, w), 0, w, h),
        0x2596..=0x259F => quadrants(c, cp),
        _ => unreachable!("draw_block covers 0x2580..=0x259F"),
    }
}

/// The shade dithers: 1 = ░ 25% (even x and even y), 2 = ▒ 50% checkerboard, 3 = ▓ 75%
/// (all but odd/odd).
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
    let bits: u8 = match cp {
        0x2596 => 0b0100,
        0x2597 => 0b1000,
        0x2598 => 0b0001,
        0x2599 => 0b1101,
        0x259A => 0b1001,
        0x259B => 0b0111,
        0x259C => 0b1011,
        0x259D => 0b0010,
        0x259E => 0b0110,
        0x259F => 0b1110,
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

/// Distance from `(px, py)` to the segment `(ax, ay)..(bx, by)`.
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

/// Powerline separators U+E0B0–E0BF, full-bleed so they tile with the adjacent
/// segment's background: E0B0/E0B2 solid right/left triangles, E0B1/E0B3 their chevron
/// outlines, E0B4/E0B6 solid rounded (half-ellipse) caps, E0B5/E0B7 their outlines,
/// E0B8–E0BF the corner triangles and their hypotenuse outlines.
fn draw_powerline(c: &mut Canvas, cp: u32) {
    let (w, h) = (c.w, c.h);
    let (wf, hf) = (w as f32, h as f32);
    let mid = hf / 2.0;
    let t = (((w.min(h) + 4) / 8) as f32).max(1.0);
    let half = (t / 2.0).max(0.6);

    if (0xE0B8..=0xE0BF).contains(&cp) {
        let norm = (wf * wf + hf * hf).sqrt();
        c.paint_aa(|px, py| {
            let dmain = (wf * py - hf * px) / norm;
            let danti = (hf * px + wf * py - wf * hf) / norm;
            match cp {
                0xE0B8 => dmain >= 0.0,
                0xE0B9 => dmain.abs() <= half,
                0xE0BA => danti >= 0.0,
                0xE0BB => danti.abs() <= half,
                0xE0BC => danti <= 0.0,
                0xE0BD => danti.abs() <= half,
                0xE0BE => dmain <= 0.0,
                _ => dmain.abs() <= half,
            }
        });
        return;
    }
    let right = matches!(cp, 0xE0B0 | 0xE0B1 | 0xE0B4 | 0xE0B5);
    let rounded = matches!(cp, 0xE0B4..=0xE0B7);
    let outline = matches!(cp, 0xE0B1 | 0xE0B3 | 0xE0B5 | 0xE0B7);
    c.paint_aa(|px, py| {
        let px = if right { px } else { wf - px };
        if rounded {
            let (nx, ny) = (px / wf, (py - mid) / mid);
            let e = (nx * nx + ny * ny).sqrt();
            if outline {
                ((e - 1.0) * wf.min(mid)).abs() <= half
            } else {
                e <= 1.0
            }
        } else if outline {
            seg_dist(px, py, 0.0, 0.0, wf, mid).min(seg_dist(px, py, wf, mid, 0.0, hf)) <= half
        } else {
            px <= wf * (1.0 - ((py - mid).abs() / mid))
        }
    });
}

/// Block sextants U+1FB00–1FB3B: a 2×3 grid of filled sub-cells.
fn draw_sextant(c: &mut Canvas, cp: u32) {
    let k = cp - 0x1FB00;
    let mask = k + 1 + u32::from(k >= 20) + u32::from(k >= 40);
    let (w, h) = (c.w, c.h);
    let xm = eighth(4, w);
    let y1 = (h + 1) / 3;
    let y2 = (2 * h + 1) / 3;
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

/// Unicode 16 block octants U+1CD00–1CDE5, indexed by `cp - 0x1CD00`: the
/// 2×4 fill mask of each, bit `n - 1` for octant `n` (1 upper-left, 2
/// upper-right, 3/4 the second row, 5/6 the third, 7/8 the bottom row).
const OCTANT_MASKS: [u8; 230] = [
    0x04, 0x06, 0x07, 0x08, 0x09, 0x0B, 0x0C, 0x0D, 0x0E, 0x10, 0x11, 0x12, 0x13, 0x15, 0x16, 0x17,
    0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27,
    0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38,
    0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A,
    0x4B, 0x4C, 0x4D, 0x4E, 0x4F, 0x51, 0x52, 0x53, 0x54, 0x56, 0x57, 0x58, 0x59, 0x5B, 0x5C, 0x5D,
    0x5E, 0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E,
    0x6F, 0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E,
    0x7F, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x8D, 0x8E, 0x8F,
    0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9D, 0x9E, 0x9F,
    0xA1, 0xA2, 0xA3, 0xA4, 0xA6, 0xA7, 0xA8, 0xA9, 0xAB, 0xAC, 0xAD, 0xAE, 0xB0, 0xB1, 0xB2, 0xB3,
    0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF, 0xC1, 0xC2, 0xC3, 0xC4,
    0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xCB, 0xCC, 0xCD, 0xCE, 0xCF, 0xD0, 0xD1, 0xD2, 0xD3, 0xD4,
    0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xDB, 0xDC, 0xDD, 0xDE, 0xDF, 0xE0, 0xE1, 0xE2, 0xE3, 0xE4,
    0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xEB, 0xEC, 0xED, 0xEE, 0xEF, 0xF1, 0xF2, 0xF3, 0xF4, 0xF6,
    0xF7, 0xF8, 0xF9, 0xFB, 0xFD, 0xFE,
];

/// Draw one octant mask.
fn draw_octant(c: &mut Canvas, mask: u8) {
    let (w, h) = (c.w, c.h);
    let xs = [0, eighth(4, w), w];
    let ys = [0, eighth(2, h), eighth(4, h), eighth(6, h), h];
    for bit in 0..8usize {
        if mask & (1 << bit) != 0 {
            let (col, row) = (bit % 2, bit / 2);
            c.rect(xs[col], ys[row], xs[col + 1], ys[row + 1]);
        }
    }
}

/// Block diagonal wedges + triangular blocks U+1FB3C–1FB6F (Symbols for
/// Legacy Computing), supersampled.
fn draw_wedge(c: &mut Canvas, cp: u32) {
    let (wf, hf) = (c.w as f32, c.h as f32);
    if cp >= 0x1FB68 {
        let quarter = |px: f32, py: f32, strict: bool| -> [bool; 4] {
            let s1 = py * wf - px * hf;
            let s2 = py * wf + px * hf - wf * hf;
            if strict {
                [
                    s1 > 0.0 && s2 < 0.0,
                    s1 < 0.0 && s2 < 0.0,
                    s1 < 0.0 && s2 > 0.0,
                    s1 > 0.0 && s2 > 0.0,
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
            0x1FB68 => !quarter(px, py, true)[0],
            0x1FB69 => !quarter(px, py, true)[1],
            0x1FB6A => !quarter(px, py, true)[2],
            0x1FB6B => !quarter(px, py, true)[3],
            0x1FB6C => quarter(px, py, false)[0],
            0x1FB6D => quarter(px, py, false)[1],
            0x1FB6E => quarter(px, py, false)[2],
            _ => quarter(px, py, false)[3],
        });
        return;
    }
    type WedgeSixths = ((i32, i32), (i32, i32), (i32, i32));
    #[rustfmt::skip]
    const WEDGES: [WedgeSixths; 44] = [
        ((0, 4), (3, 6), (0, 6)),
        ((0, 4), (6, 6), (0, 6)),
        ((0, 2), (3, 6), (0, 6)),
        ((0, 2), (6, 6), (0, 6)),
        ((0, 0), (3, 6), (0, 6)),
        ((0, 2), (3, 0), (6, 6)),
        ((0, 2), (6, 0), (6, 6)),
        ((0, 4), (3, 0), (6, 6)),
        ((0, 4), (6, 0), (6, 6)),
        ((0, 6), (3, 0), (6, 6)),
        ((0, 4), (6, 2), (6, 6)),
        ((3, 6), (6, 4), (6, 6)),
        ((0, 6), (6, 4), (6, 6)),
        ((3, 6), (6, 2), (6, 6)),
        ((0, 6), (6, 2), (6, 6)),
        ((3, 6), (6, 0), (6, 6)),
        ((3, 0), (6, 2), (0, 6)),
        ((0, 0), (6, 2), (0, 6)),
        ((3, 0), (6, 4), (0, 6)),
        ((0, 0), (6, 4), (0, 6)),
        ((3, 0), (6, 6), (0, 6)),
        ((0, 2), (6, 4), (0, 6)),
        ((0, 4), (3, 6), (6, 0)),
        ((0, 4), (6, 6), (6, 0)),
        ((0, 2), (3, 6), (6, 0)),
        ((0, 2), (6, 6), (6, 0)),
        ((0, 0), (3, 6), (6, 0)),
        ((0, 2), (3, 0), (0, 0)),
        ((0, 2), (6, 0), (0, 0)),
        ((0, 4), (3, 0), (0, 0)),
        ((0, 4), (6, 0), (0, 0)),
        ((0, 6), (3, 0), (0, 0)),
        ((0, 4), (6, 2), (0, 0)),
        ((3, 6), (6, 4), (0, 0)),
        ((0, 6), (6, 4), (0, 0)),
        ((3, 6), (6, 2), (0, 0)),
        ((0, 6), (6, 2), (0, 0)),
        ((3, 6), (6, 0), (0, 0)),
        ((3, 0), (6, 2), (6, 0)),
        ((0, 0), (6, 2), (6, 0)),
        ((3, 0), (6, 4), (6, 0)),
        ((0, 0), (6, 4), (6, 0)),
        ((3, 0), (6, 6), (6, 0)),
        ((0, 2), (6, 4), (6, 0)),
    ];
    let ((x1, y1), (x2, y2), (cx, cy)) = WEDGES[(cp - 0x1FB3C) as usize];
    let sx = |n: i32| n as f32 * wf / 6.0;
    let sy = |n: i32| n as f32 * hf / 6.0;
    let (ax, ay, bx, by) = (sx(x1), sy(y1), sx(x2), sy(y2));
    let side = move |px: f32, py: f32| (bx - ax) * (py - ay) - (by - ay) * (px - ax);
    let corner = side(sx(cx), sy(cy));
    c.paint_aa(|px, py| side(px, py) * corner >= 0.0);
}

/// Legacy eighth blocks U+1FB70–1FB8B (Symbols for Legacy Computing): hard 0/255 fills
/// on the same `eighth` rule as U+2580–259F, so they seam with the classic eighth
/// blocks: one-eighth bars 2..7 (1 and 8 live at U+258F/2595 and U+2594/2581), the
/// edge pairs, the 1-3-5-8 scanline set, and the upper/right k-eighths fills.
///
/// A one-eighth bar starts at its eighth's boundary and is as thick as the edge bars
/// (`eighth(1, ·)`, at least a pixel), whatever the rounding makes of its own eighth: a line
/// drawn in them (the quota row's projected week) keeps one stroke from level to level, where
/// `eighth(k − 1)..eighth(k)` is 1 px at some levels and 2 px at others in a 14-px cell.
fn draw_legacy_block(c: &mut Canvas, cp: u32) {
    let (w, h) = (c.w, c.h);
    let bar = |k: u32, e: usize| {
        let t = eighth(1, e).max(1);
        let from = eighth(k - 1, e).min(e - t);
        (from, from + t)
    };
    match cp {
        0x1FB70..=0x1FB75 => {
            let (x0, x1) = bar(cp - 0x1FB70 + 2, w);
            c.rect(x0, 0, x1, h);
        }
        0x1FB76..=0x1FB7B => {
            let (y0, y1) = bar(cp - 0x1FB76 + 2, h);
            c.rect(0, y0, w, y1);
        }
        0x1FB7C..=0x1FB7F => {
            let (ew, eh) = (eighth(1, w), eighth(1, h));
            if matches!(cp, 0x1FB7C | 0x1FB7D) {
                c.rect(0, 0, ew, h);
            } else {
                c.rect(w - ew, 0, w, h);
            }
            if matches!(cp, 0x1FB7C | 0x1FB7F) {
                c.rect(0, h - eh, w, h);
            } else {
                c.rect(0, 0, w, eh);
            }
        }
        0x1FB80 => {
            c.rect(0, 0, w, eighth(1, h));
            c.rect(0, h - eighth(1, h), w, h);
        }
        0x1FB81 => {
            for k in [1u32, 3, 5, 8] {
                c.rect(0, eighth(k - 1, h), w, eighth(k, h));
            }
        }
        0x1FB82..=0x1FB86 => {
            const K: [u32; 5] = [2, 3, 5, 6, 7];
            c.rect(0, 0, w, eighth(K[(cp - 0x1FB82) as usize], h));
        }
        0x1FB87..=0x1FB8B => {
            const K: [u32; 5] = [2, 3, 5, 6, 7];
            c.rect(w - eighth(K[(cp - 0x1FB87) as usize], w), 0, w, h);
        }
        _ => unreachable!("draw_legacy_block covers 0x1FB70..=0x1FB8B"),
    }
}

/// Braille U+2800–28FF: bit `n` of `cp - 0x2800` lights dot `n+1` in the
/// standard 2x4 layout (dots 1-3 left column top-down, 4-6 right column,
/// 7/8 the bottom pair).
fn draw_braille(c: &mut Canvas, cp: u32) {
    let bits = cp - 0x2800;
    let (w, h) = (c.w, c.h);
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
    let xb = |i: usize| (i * w).div_ceil(2);
    let yb = |i: usize| (i * h + 2) / 4;
    for (bit, &(col, row)) in DOTS.iter().enumerate() {
        if bits & (1 << bit) == 0 {
            continue;
        }
        let (x0, x1) = (xb(col), xb(col + 1));
        let (y0, y1) = (yb(row), yb(row + 1));
        let (bw, bh) = (x1 - x0, y1 - y0);
        if bw == 0 || bh == 0 {
            continue;
        }
        let dw = bw.div_ceil(2);
        let dh = bh.div_ceil(2);
        let dx = x0 + (bw - dw) / 2;
        let dy = y0 + (bh - dh) / 2;
        c.rect(dx, dy, dx + dw, dy + dh);
    }
}

#[cfg(test)]
#[path = "procedural_tests.rs"]
mod tests;
