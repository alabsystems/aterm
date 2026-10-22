// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Last-resort fontless symbol glyphs: the geometric shapes, media controls and check
//! marks TUIs lean on, synthesized from the cell geometry when no face in the resolution
//! chain covers them. A bare Linux host may have no face at all for U+23F5 `⏵`, which
//! the Claude Code and Codex footers print.
//!
//! Unlike the parent module's box-drawing families, which pre-empt every font so borders
//! tile seam-free, these are the last tier of [`crate::font_chain::resolve_chain`]
//! ([`crate::font_chain::Tier::Synthetic`]): any real face that covers the code point
//! wins, and only `.notdef` tofu is replaced. The `⎿` painter here is the exception,
//! drawn by the pre-emptive family ([`draw_dentistry_bottom_right`]).
//!
//! Each glyph is a few primitives in a unit box (`u, v` in `0..=1`, `v` down) scaled to a
//! centred square of side `min(span * cell_w, cell_h) * size`, where `span` is the
//! glyph's cell count (2 for an emoji-presentation code point such as ⏩), so a wide
//! symbol is centred across both cells. Shapes are supersampled like the Powerline
//! separators ([`super::Canvas::paint_aa`]); strokes are a light box-drawing line thick
//! (`min(w, h) / 8`, at least one pixel). `Renderer::rasterize` produces the bytes once
//! and the CPU blit and the GPU atlas share that [`crate::GlyphImage`].

use super::{Canvas, Metrics, eighth, seg_dist};

/// How large the symbol's box is, as a fraction of `min(span * cell_w, cell_h)`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Size {
    /// ▪ ▫ ▴ ▸ ◦ — the "small" variants.
    Small,
    /// ⏴⏵⏶⏷ ⏸⏹⏺ ◻◼ — the "medium" media/shape variants.
    Medium,
    /// ■ □ ▲ ▶ ● ○ ◆ ✓ — the ordinary geometric shapes.
    Large,
    /// ◢◣◤◥ ◯ ⬤ — shapes that fill the whole box.
    Full,
}

impl Size {
    #[inline]
    const fn scale(self) -> f32 {
        match self {
            Size::Small => 0.52,
            Size::Medium => 0.8,
            Size::Large => 0.94,
            Size::Full => 1.0,
        }
    }
}

/// One drawing primitive, in unit-box coordinates.
#[derive(Clone, Copy, Debug)]
enum Prim {
    /// A filled CONVEX polygon (either winding).
    Poly(&'static [(f32, f32)]),
    /// The outline of a convex polygon: an inward band one stroke wide, so the
    /// outline and its filled twin occupy the same footprint (△ over ▲).
    PolyOutline(&'static [(f32, f32)]),
    /// A filled axis-aligned rectangle `(u0, v0, u1, v1)`.
    Rect(f32, f32, f32, f32),
    /// The inward outline of [`Prim::Rect`].
    RectOutline(f32, f32, f32, f32),
    /// A filled disc `(cu, cv, r)`.
    Disc(f32, f32, f32),
    /// A ring `(cu, cv, r)`: the inward band of the disc of radius `r`.
    Ring(f32, f32, f32),
    /// A half-disc `(cu, cv, r, du, dv)`: the part of the disc on the side of
    /// the direction `(du, dv)`.
    HalfDisc(f32, f32, f32, f32, f32),
    /// `n` small dots evenly spaced on the circle `(cu, cv, r)` (◌).
    DottedRing(f32, f32, f32, u32),
    /// A stroked polyline with round joins/caps.
    Line(&'static [(f32, f32)]),
    /// `n` teardrops about the box centre, the first pointing up and the
    /// rest every `360/n` degrees: each narrows to a point at the centre and
    /// ends in a round head of radius `r` (box units) touching the box's
    /// inscribed circle.
    Teardrops(u32, f32),
    /// `n` stroked spokes from the centre to the box's inscribed circle,
    /// round-capped, the first pointing up.
    Spokes(u32),
    /// A filled `n`-pointed star, the first point up, as large as the cell
    /// holds it (not the box: a six-pointed star is `cos 30°` as wide as it
    /// is tall, so in a tall cell its side points reach the cell's edges),
    /// its inner corners at `inner` of the points' radius (`1/√3` would be
    /// two crossed triangles for `n = 6`, a hexagon with bumps from about
    /// 0.6).
    Star(u32, f32),
}

/// One symbol: its box size, stroke weight (× the light stroke) and shape.
#[derive(Clone, Copy, Debug)]
struct Spec {
    size: Size,
    weight: f32,
    prims: &'static [Prim],
}

const fn spec(size: Size, prims: &'static [Prim]) -> Spec {
    Spec {
        size,
        weight: 1.0,
        prims,
    }
}

const fn heavy(size: Size, prims: &'static [Prim]) -> Spec {
    Spec {
        size,
        weight: 2.0,
        prims,
    }
}

const fn weighted(size: Size, weight: f32, prims: &'static [Prim]) -> Spec {
    Spec {
        size,
        weight,
        prims,
    }
}

// Triangles, apex inset a hair so the AA fringe stays inside the box.
const UP: &[(f32, f32)] = &[(0.5, 0.06), (1.0, 0.94), (0.0, 0.94)];
const DOWN: &[(f32, f32)] = &[(0.0, 0.06), (1.0, 0.06), (0.5, 0.94)];
const RIGHT: &[(f32, f32)] = &[(0.06, 0.0), (0.94, 0.5), (0.06, 1.0)];
const LEFT: &[(f32, f32)] = &[(0.94, 0.0), (0.06, 0.5), (0.94, 1.0)];
// ► ◄ — the flatter "pointer" triangles.
const POINTER_R: &[(f32, f32)] = &[(0.0, 0.2), (1.0, 0.5), (0.0, 0.8)];
const POINTER_L: &[(f32, f32)] = &[(1.0, 0.2), (0.0, 0.5), (1.0, 0.8)];
const DIAMOND: &[(f32, f32)] = &[(0.5, 0.0), (1.0, 0.5), (0.5, 1.0), (0.0, 0.5)];
const DIAMOND_INNER: &[(f32, f32)] = &[(0.5, 0.26), (0.74, 0.5), (0.5, 0.74), (0.26, 0.5)];
const LOZENGE: &[(f32, f32)] = &[(0.5, 0.0), (0.82, 0.5), (0.5, 1.0), (0.18, 0.5)];
// ◢◣◤◥ — the corner triangles fill half the box.
const CORNER_LR: &[(f32, f32)] = &[(1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
const CORNER_LL: &[(f32, f32)] = &[(0.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
const CORNER_UL: &[(f32, f32)] = &[(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)];
const CORNER_UR: &[(f32, f32)] = &[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)];
// Media controls.
const FF_A: &[(f32, f32)] = &[(0.0, 0.0), (0.5, 0.5), (0.0, 1.0)];
const FF_B: &[(f32, f32)] = &[(0.5, 0.0), (1.0, 0.5), (0.5, 1.0)];
const RW_A: &[(f32, f32)] = &[(1.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
const RW_B: &[(f32, f32)] = &[(0.5, 0.0), (0.0, 0.5), (0.5, 1.0)];
const UPUP_A: &[(f32, f32)] = &[(0.5, 0.0), (1.0, 0.5), (0.0, 0.5)];
const UPUP_B: &[(f32, f32)] = &[(0.5, 0.5), (1.0, 1.0), (0.0, 1.0)];
const DNDN_A: &[(f32, f32)] = &[(0.0, 0.0), (1.0, 0.0), (0.5, 0.5)];
const DNDN_B: &[(f32, f32)] = &[(0.0, 0.5), (1.0, 0.5), (0.5, 1.0)];
const NEXT_A: &[(f32, f32)] = &[(0.0, 0.1), (0.42, 0.5), (0.0, 0.9)];
const NEXT_B: &[(f32, f32)] = &[(0.42, 0.1), (0.84, 0.5), (0.42, 0.9)];
const PREV_A: &[(f32, f32)] = &[(1.0, 0.1), (0.58, 0.5), (1.0, 0.9)];
const PREV_B: &[(f32, f32)] = &[(0.58, 0.1), (0.16, 0.5), (0.58, 0.9)];
const PLAY_PAUSE: &[(f32, f32)] = &[(0.0, 0.1), (0.5, 0.5), (0.0, 0.9)];
// Check marks and crosses.
const CHECK: &[(f32, f32)] = &[(0.08, 0.55), (0.38, 0.86), (0.92, 0.12)];
const X_A: &[(f32, f32)] = &[(0.15, 0.15), (0.85, 0.85)];
const X_B: &[(f32, f32)] = &[(0.85, 0.15), (0.15, 0.85)];
const BALLOT_A: &[(f32, f32)] = &[(0.2, 0.1), (0.8, 0.9)];
const BALLOT_B: &[(f32, f32)] = &[(0.84, 0.08), (0.16, 0.92)];

use Prim::{
    Disc, DottedRing, HalfDisc, Line, Poly, PolyOutline, Rect, RectOutline, Ring, Spokes, Star,
    Teardrops,
};
use Size::{Full, Large, Medium, Small};

/// The symbol table: every code point this module draws, and how.
#[inline]
const fn lookup(cp: u32) -> Option<Spec> {
    let s = match cp {
        // ---- Miscellaneous Technical: media controls ----------------------
        0x23E9 => spec(Medium, &[Poly(FF_A), Poly(FF_B)]), // ⏩
        0x23EA => spec(Medium, &[Poly(RW_A), Poly(RW_B)]), // ⏪
        0x23EB => spec(Medium, &[Poly(UPUP_A), Poly(UPUP_B)]), // ⏫
        0x23EC => spec(Medium, &[Poly(DNDN_A), Poly(DNDN_B)]), // ⏬
        0x23ED => spec(
            Medium,
            &[Poly(NEXT_A), Poly(NEXT_B), Rect(0.86, 0.1, 1.0, 0.9)],
        ), // ⏭
        0x23EE => spec(
            Medium,
            &[Rect(0.0, 0.1, 0.14, 0.9), Poly(PREV_B), Poly(PREV_A)],
        ), // ⏮
        0x23EF => spec(
            Medium,
            &[
                Poly(PLAY_PAUSE),
                Rect(0.6, 0.1, 0.74, 0.9),
                Rect(0.86, 0.1, 1.0, 0.9),
            ],
        ), // ⏯
        0x23F4 => spec(Medium, &[Poly(LEFT)]),             // ⏴
        0x23F5 => spec(Medium, &[Poly(RIGHT)]),            // ⏵
        0x23F6 => spec(Medium, &[Poly(UP)]),               // ⏶
        0x23F7 => spec(Medium, &[Poly(DOWN)]),             // ⏷
        0x23F8 => spec(
            Medium,
            &[Rect(0.1, 0.0, 0.38, 1.0), Rect(0.62, 0.0, 0.9, 1.0)],
        ), // ⏸
        0x23F9 => spec(Medium, &[Rect(0.0, 0.0, 1.0, 1.0)]), // ⏹
        0x23FA => spec(Medium, &[Disc(0.5, 0.5, 0.5)]),    // ⏺
        // ---- Geometric Shapes ---------------------------------------------
        0x25A0 => spec(Large, &[Rect(0.0, 0.0, 1.0, 1.0)]), // ■
        0x25A1 | 0x25A2 => spec(Large, &[RectOutline(0.0, 0.0, 1.0, 1.0)]), // □ ▢
        0x25A3 => spec(
            Large,
            &[RectOutline(0.0, 0.0, 1.0, 1.0), Rect(0.3, 0.3, 0.7, 0.7)],
        ), // ▣
        0x25AA => spec(Small, &[Rect(0.0, 0.0, 1.0, 1.0)]), // ▪
        0x25AB => spec(Small, &[RectOutline(0.0, 0.0, 1.0, 1.0)]), // ▫
        0x25AC => spec(Large, &[Rect(0.0, 0.3, 1.0, 0.7)]), // ▬
        0x25AD => spec(Large, &[RectOutline(0.0, 0.3, 1.0, 0.7)]), // ▭
        0x25AE => spec(Large, &[Rect(0.25, 0.0, 0.75, 1.0)]), // ▮
        0x25AF => spec(Large, &[RectOutline(0.25, 0.0, 0.75, 1.0)]), // ▯
        0x25B2 => spec(Large, &[Poly(UP)]),                 // ▲
        0x25B3 => spec(Large, &[PolyOutline(UP)]),          // △
        0x25B4 => spec(Small, &[Poly(UP)]),                 // ▴
        0x25B5 => spec(Small, &[PolyOutline(UP)]),          // ▵
        0x25B6 => spec(Large, &[Poly(RIGHT)]),              // ▶
        0x25B7 => spec(Large, &[PolyOutline(RIGHT)]),       // ▷
        0x25B8 => spec(Small, &[Poly(RIGHT)]),              // ▸
        0x25B9 => spec(Small, &[PolyOutline(RIGHT)]),       // ▹
        0x25BA => spec(Large, &[Poly(POINTER_R)]),          // ►
        0x25BB => spec(Large, &[PolyOutline(POINTER_R)]),   // ▻
        0x25BC => spec(Large, &[Poly(DOWN)]),               // ▼
        0x25BD => spec(Large, &[PolyOutline(DOWN)]),        // ▽
        0x25BE => spec(Small, &[Poly(DOWN)]),               // ▾
        0x25BF => spec(Small, &[PolyOutline(DOWN)]),        // ▿
        0x25C0 => spec(Large, &[Poly(LEFT)]),               // ◀
        0x25C1 => spec(Large, &[PolyOutline(LEFT)]),        // ◁
        0x25C2 => spec(Small, &[Poly(LEFT)]),               // ◂
        0x25C3 => spec(Small, &[PolyOutline(LEFT)]),        // ◃
        0x25C4 => spec(Large, &[Poly(POINTER_L)]),          // ◄
        0x25C5 => spec(Large, &[PolyOutline(POINTER_L)]),   // ◅
        0x25C6 => spec(Large, &[Poly(DIAMOND)]),            // ◆
        0x25C7 => spec(Large, &[PolyOutline(DIAMOND)]),     // ◇
        0x25C8 => spec(Large, &[PolyOutline(DIAMOND), Poly(DIAMOND_INNER)]), // ◈
        0x25C9 => spec(Large, &[Ring(0.5, 0.5, 0.5), Disc(0.5, 0.5, 0.24)]), // ◉
        0x25CA => spec(Large, &[PolyOutline(LOZENGE)]),     // ◊
        0x25CB => spec(Large, &[Ring(0.5, 0.5, 0.5)]),      // ○
        0x25CC => spec(Large, &[DottedRing(0.5, 0.5, 0.44, 8)]), // ◌
        0x25CE => spec(Large, &[Ring(0.5, 0.5, 0.5), Ring(0.5, 0.5, 0.26)]), // ◎
        0x25CF => spec(Large, &[Disc(0.5, 0.5, 0.5)]),      // ●
        0x25D0 => spec(
            Large,
            &[Ring(0.5, 0.5, 0.5), HalfDisc(0.5, 0.5, 0.5, -1.0, 0.0)],
        ), // ◐
        0x25D1 => spec(
            Large,
            &[Ring(0.5, 0.5, 0.5), HalfDisc(0.5, 0.5, 0.5, 1.0, 0.0)],
        ), // ◑
        0x25D2 => spec(
            Large,
            &[Ring(0.5, 0.5, 0.5), HalfDisc(0.5, 0.5, 0.5, 0.0, 1.0)],
        ), // ◒
        0x25D3 => spec(
            Large,
            &[Ring(0.5, 0.5, 0.5), HalfDisc(0.5, 0.5, 0.5, 0.0, -1.0)],
        ), // ◓
        0x25E2 => spec(Full, &[Poly(CORNER_LR)]),           // ◢
        0x25E3 => spec(Full, &[Poly(CORNER_LL)]),           // ◣
        0x25E4 => spec(Full, &[Poly(CORNER_UL)]),           // ◤
        0x25E5 => spec(Full, &[Poly(CORNER_UR)]),           // ◥
        0x25E6 => spec(Small, &[Ring(0.5, 0.5, 0.5)]),      // ◦
        0x25EF => spec(Full, &[Ring(0.5, 0.5, 0.5)]),       // ◯
        0x25FB => spec(Medium, &[RectOutline(0.0, 0.0, 1.0, 1.0)]), // ◻
        0x25FC => spec(Medium, &[Rect(0.0, 0.0, 1.0, 1.0)]), // ◼
        0x25FD => spec(Small, &[RectOutline(0.0, 0.0, 1.0, 1.0)]), // ◽
        0x25FE => spec(Small, &[Rect(0.0, 0.0, 1.0, 1.0)]), // ◾
        // ---- Miscellaneous Symbols / Dingbats ------------------------------
        0x26AA => spec(Medium, &[Ring(0.5, 0.5, 0.5)]), // ⚪
        0x26AB => spec(Medium, &[Disc(0.5, 0.5, 0.5)]), // ⚫
        0x2713 => spec(Large, &[Line(CHECK)]),          // ✓
        0x2714 => heavy(Large, &[Line(CHECK)]),         // ✔
        0x2715 => spec(Large, &[Line(X_A), Line(X_B)]), // ✕
        0x2716 => heavy(Large, &[Line(X_A), Line(X_B)]), // ✖
        0x2717 => spec(Large, &[Line(BALLOT_A), Line(BALLOT_B)]), // ✗
        0x2718 => heavy(Large, &[Line(BALLOT_A), Line(BALLOT_B)]), // ✘
        // Claude Code's spinner frames ([`is_spinner_star`]): strokes and
        // petals solid enough, and inks close enough, that no frame dims
        // the cycle. `✶` keeps a face's proportions: its notches reach in to
        // 0.4 of its points (Iosevka Term's own `✶` is 0.40; DejaVu Sans
        // Mono's, Segoe UI Symbol's and Noto Sans Symbols 2's 0.29-0.33).
        0x2722 => spec(Full, &[Teardrops(4, 0.16)]), // ✢
        0x2733 => weighted(Full, 1.3, &[Spokes(8)]), // ✳
        0x2736 => spec(Full, &[Star(6, 0.4)]),       // ✶
        0x273B => spec(Full, &[Teardrops(6, 0.13)]), // ✻
        0x273D => spec(Full, &[Teardrops(6, 0.15)]), // ✽
        // ---- Miscellaneous Symbols and Arrows ------------------------------
        0x2B24 => spec(Full, &[Disc(0.5, 0.5, 0.5)]), // ⬤
        0x2B58 => heavy(Large, &[Ring(0.5, 0.5, 0.5)]), // ⭘
        _ => return None,
    };
    Some(s)
}

/// Whether this module can draw `ch` without any font.
#[inline]
pub(super) const fn covers(ch: char) -> bool {
    lookup(ch as u32).is_some()
}

/// Row-major coverage for `ch` at `span * cell_w` × `cell_h` (`span` clamped
/// to `1..=2`), or `None` when `ch` is not a symbol this module draws or the
/// cell is degenerate.
pub(super) fn coverage(ch: char, cell_w: usize, cell_h: usize, span: usize) -> Option<Vec<u8>> {
    if cell_w == 0 || cell_h == 0 {
        return None;
    }
    Some(draw(lookup(u32::from(ch))?, cell_w, cell_h, span))
}

/// Row-major coverage of `spec` at `span * cell_w` × `cell_h` (`span`
/// clamped to `1..=2`); `cell_w` and `cell_h` are non-zero.
fn draw(spec: Spec, cell_w: usize, cell_h: usize, span: usize) -> Vec<u8> {
    let w = cell_w * span.clamp(1, 2);
    let mut c = Canvas::new(w, cell_h);
    let (wf, hf) = (w as f32, cell_h as f32);
    let side = wf.min(hf) * spec.size.scale();
    let (bx, by) = ((wf - side) / 2.0, (hf - side) / 2.0);
    let to_px = |(u, v): (f32, f32)| (bx + u * side, by + v * side);
    // The parent module's light stroke (`min(w, h) / 8`), floored at one pixel
    // so a tiny cell keeps a connected outline; measured on ONE cell even for a
    // wide symbol, so ⚪ and ○ share a stroke weight.
    let stroke = (cell_w.min(cell_h) as f32 / 8.0).max(1.0) * spec.weight;
    for prim in spec.prims {
        match *prim {
            Poly(pts) => {
                let poly = ConvexPoly::new(pts, to_px);
                c.paint_aa(|px, py| poly.sdist(px, py) <= 0.0);
            }
            PolyOutline(pts) => {
                let poly = ConvexPoly::new(pts, to_px);
                c.paint_aa(|px, py| {
                    let d = poly.sdist(px, py);
                    d <= 0.0 && d > -stroke
                });
            }
            Rect(u0, v0, u1, v1) => {
                let (x0, y0) = to_px((u0, v0));
                let (x1, y1) = to_px((u1, v1));
                c.paint_aa(|px, py| px >= x0 && px < x1 && py >= y0 && py < y1);
            }
            RectOutline(u0, v0, u1, v1) => {
                let (x0, y0) = to_px((u0, v0));
                let (x1, y1) = to_px((u1, v1));
                c.paint_aa(|px, py| {
                    let inside = px >= x0 && px < x1 && py >= y0 && py < y1;
                    let inner = px >= x0 + stroke
                        && px < x1 - stroke
                        && py >= y0 + stroke
                        && py < y1 - stroke;
                    inside && !inner
                });
            }
            Disc(cu, cv, r) => {
                let (cx, cy) = to_px((cu, cv));
                let r = r * side;
                c.paint_aa(|px, py| (px - cx).hypot(py - cy) <= r);
            }
            Ring(cu, cv, r) => {
                let (cx, cy) = to_px((cu, cv));
                let r = r * side;
                c.paint_aa(|px, py| {
                    let d = (px - cx).hypot(py - cy);
                    d <= r && d > r - stroke
                });
            }
            HalfDisc(cu, cv, r, du, dv) => {
                let (cx, cy) = to_px((cu, cv));
                let r = r * side;
                c.paint_aa(|px, py| {
                    let (dx, dy) = (px - cx, py - cy);
                    dx.hypot(dy) <= r && dx * du + dy * dv >= 0.0
                });
            }
            DottedRing(cu, cv, r, n) => {
                let (cx, cy) = to_px((cu, cv));
                let r = r * side;
                let dot = (stroke * 0.75).max(0.75);
                let centres: Vec<(f32, f32)> = (0..n)
                    .map(|k| {
                        let a = k as f32 * std::f32::consts::TAU / n as f32;
                        (cx + r * a.cos(), cy + r * a.sin())
                    })
                    .collect();
                c.paint_aa(|px, py| {
                    centres
                        .iter()
                        .any(|&(dx, dy)| (px - dx).hypot(py - dy) <= dot)
                });
            }
            Line(pts) => {
                let pts: Vec<(f32, f32)> = pts.iter().copied().map(to_px).collect();
                let half = stroke / 2.0;
                c.paint_aa(|px, py| {
                    pts.windows(2)
                        .any(|s| seg_dist(px, py, s[0].0, s[0].1, s[1].0, s[1].1) <= half)
                });
            }
            Teardrops(n, r) => {
                let (cx, cy) = to_px((0.5, 0.5));
                let head = r * side;
                let reach = (0.5 - r) * side;
                let dirs = directions(n);
                c.paint_aa(|px, py| {
                    let (vx, vy) = (px - cx, py - cy);
                    dirs.iter().any(|&(dx, dy)| {
                        let along = vx * dx + vy * dy;
                        let (hx, hy) = (vx - dx * reach, vy - dy * reach);
                        // The head, or the cone from the centre that widens
                        // to it.
                        hx.hypot(hy) <= head
                            || ((0.0..=reach).contains(&along)
                                && (vx * dy - vy * dx).abs() <= head * along / reach)
                    })
                });
            }
            Star(n, inner) => {
                let (cx, cy) = to_px((0.5, 0.5));
                // The largest star whose points stay inside the cell: the
                // points' radius over the widest and tallest of their
                // directions (`cos 30°` and 1 for six standing on one).
                let (sx, sy) = directions(n)
                    .iter()
                    .fold((0.0f32, 0.0f32), |(sx, sy), &(dx, dy)| {
                        (sx.max(dx.abs()), sy.max(dy.abs()))
                    });
                let outer = (wf / 2.0 / sx).min(hf / 2.0 / sy) * spec.size.scale();
                let inner = inner * outer;
                let step = std::f32::consts::PI / n as f32;
                // One edge, in the frame of the inner corner's direction:
                // from the corner `(inner, 0)` out to the point.
                let (tx, ty) = (outer * step.cos(), outer * step.sin());
                c.paint_aa(|px, py| {
                    let (vx, vy) = (px - cx, py - cy);
                    let d = vx.hypot(vy);
                    if d <= inner {
                        return true;
                    }
                    // The angle from the nearest inner corner: 0 there,
                    // `step` at a point.
                    let a = ((vy.atan2(vx) + std::f32::consts::FRAC_PI_2).rem_euclid(2.0 * step)
                        - step)
                        .abs();
                    let (qx, qy) = (d * a.cos(), d * a.sin());
                    // On the centre's side of the edge.
                    (tx - inner) * qy - ty * (qx - inner) >= 0.0
                });
            }
            Spokes(n) => {
                let (cx, cy) = to_px((0.5, 0.5));
                let half = stroke / 2.0;
                let reach = side / 2.0 - half;
                let dirs = directions(n);
                c.paint_aa(|px, py| {
                    dirs.iter().any(|&(dx, dy)| {
                        seg_dist(px, py, cx, cy, cx + dx * reach, cy + dy * reach) <= half
                    })
                });
            }
        }
    }
    c.buf
}

/// `n` unit directions, the first straight up (`v` grows downward), then
/// every `360/n` degrees clockwise.
fn directions(n: u32) -> Vec<(f32, f32)> {
    (0..n)
        .map(|k| {
            let a = -std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::TAU / n as f32;
            (a.cos(), a.sin())
        })
        .collect()
}

/// Claude Code's spinner frames, `✢ ✳ ✶ ✻ ✽` (with `·`, which every text
/// face draws, and the face's own `*`, or `✳` in Ghostty). A face's dingbats are
/// pictures at text size: the Iosevka Term Semibold the Windows default
/// draws them as x-height marks, and the DejaVu Sans Mono its chain then
/// reached drew `✳`'s eight 1 px spokes and `✶`'s small star soft once
/// fitted into the cell — 8.5% and 14% of their ink solid, about half the
/// ink of `✻` and `✽` (measured 2026-10-18 at 9x23), so the spinner dimmed on
/// those frames. These are drawn from the cell instead, wherever the primary
/// yields them and the text is smaller than `bundled::SPINNER_STARS_CRISP_PX`
/// (`Renderer::spinner_star_preempts`); everywhere else they are only the
/// last resort for a host with no face for them.
#[inline]
pub(crate) const fn is_spinner_star(ch: char) -> bool {
    matches!(
        ch,
        '\u{2722}' | '\u{2733}' | '\u{2736}' | '\u{273B}' | '\u{273D}'
    )
}

/// The [`is_spinner_star`] frames, in Claude Code's order: `✢ ✳ ✶ ✻ ✽`.
pub(crate) const SPINNER_STARS: [char; 5] =
    ['\u{2722}', '\u{2733}', '\u{2736}', '\u{273B}', '\u{273D}'];

/// U+23BF `⎿`, Claude Code's tool-output connector: a light vertical on the `│` columns
/// from the top edge to the three-quarter line, then a light horizontal to the right
/// edge, hard 0/255.
///
/// Drawn by the pre-emptive family ([`super::covers`]), not as a last-resort symbol,
/// because a font's `⎿` cannot join the procedural `│` above it: Noto CJK, the only
/// Linux face with U+23BF, draws a short L at the cell bottom and leaves a gap.
pub(super) fn draw_dentistry_bottom_right(c: &mut Canvas) {
    let m = Metrics::new(c.w, c.h);
    let foot = eighth(6, m.h).clamp(m.light, m.h);
    c.rect(m.vl0, 0, m.vl1, foot);
    c.rect(m.vl0, foot - m.light, m.w, foot);
}

/// A convex polygon in pixel space with outward edge normals, for a signed distance:
/// negative inside, the max over edges of the distance past each.
struct ConvexPoly {
    /// `(ax, ay, nx, ny)` per edge: a point on the edge and its unit outward
    /// normal.
    edges: Vec<(f32, f32, f32, f32)>,
}

impl ConvexPoly {
    fn new(pts: &[(f32, f32)], to_px: impl Fn((f32, f32)) -> (f32, f32)) -> ConvexPoly {
        let px: Vec<(f32, f32)> = pts.iter().copied().map(to_px).collect();
        let n = px.len() as f32;
        let (gx, gy) = px
            .iter()
            .fold((0.0, 0.0), |(sx, sy), &(x, y)| (sx + x / n, sy + y / n));
        let edges = (0..px.len())
            .filter_map(|i| {
                let (ax, ay) = px[i];
                let (bx, by) = px[(i + 1) % px.len()];
                let (ex, ey) = (bx - ax, by - ay);
                let len = ex.hypot(ey);
                if len <= f32::EPSILON {
                    return None;
                }
                let (mut nx, mut ny) = (ey / len, -ex / len);
                // Orient AWAY from the centroid, so either winding works.
                if (gx - ax) * nx + (gy - ay) * ny > 0.0 {
                    nx = -nx;
                    ny = -ny;
                }
                Some((ax, ay, nx, ny))
            })
            .collect();
        ConvexPoly { edges }
    }

    fn sdist(&self, px: f32, py: f32) -> f32 {
        self.edges
            .iter()
            .map(|&(ax, ay, nx, ny)| (px - ax) * nx + (py - ay) * ny)
            .fold(f32::NEG_INFINITY, f32::max)
    }
}

#[cfg(test)]
mod tests {
    use super::{Full, Star, coverage, draw, is_spinner_star, spec};

    /// `(ink sum in full texels, solid share, peak)` of a coverage mask:
    /// solid is a texel at 90% coverage or more, ink any texel at all.
    fn ink(mask: &[u8]) -> (f32, f32, u8) {
        let inked = mask.iter().filter(|&&c| c > 0).count();
        let solid = mask.iter().filter(|&&c| c >= 230).count();
        let sum = mask.iter().map(|&c| f32::from(c) / 255.0).sum();
        let peak = mask.iter().copied().max().unwrap_or(0);
        (sum, solid as f32 / inked.max(1) as f32, peak)
    }

    /// Claude Code's spinner frames read as one growing star, not a cycle
    /// that dims: at the Windows default cell (9x23), its 1.25x and 1.5x
    /// zooms (11x29, 13x36), larger cells (14x34, 18x46) and the cells of
    /// other font sizes (8x17, 10x21), every frame reaches full ink, at
    /// least a quarter of its ink is solid, and the frames' ink sums are
    /// within 1.5x of each other. (The fitted DejaVu Sans Mono glyphs were
    /// 0-12% solid at 9x23, and `✳` and `✶` about half the ink of `✽`.) A
    /// quarter, not more: in an even-width cell a centred upright point
    /// straddles two texel columns, which leaves `✶`'s slender points 26%
    /// solid at 8x17. A 7 px cell is too narrow for six spokes to stay
    /// apart; there every frame still reaches full ink.
    #[test]
    fn spinner_frames_are_solid_and_even() {
        let frames = ['\u{2722}', '\u{2733}', '\u{2736}', '\u{273B}', '\u{273D}'];
        assert!(frames.iter().all(|&ch| is_spinner_star(ch)));
        for (w, h) in [
            (9, 23),
            (11, 29),
            (13, 36),
            (14, 34),
            (18, 46),
            (8, 17),
            (10, 21),
        ] {
            let mut sums = Vec::new();
            for ch in frames {
                let mask = coverage(ch, w, h, 1).expect("drawn");
                let (sum, solid, peak) = ink(&mask);
                assert_eq!(peak, 255, "{ch} at {w}x{h}");
                assert!(solid >= 0.25, "{ch} at {w}x{h}: {solid:.2} solid");
                sums.push(sum);
            }
            let (lo, hi) = sums
                .iter()
                .fold((f32::MAX, 0.0f32), |(lo, hi), &s| (lo.min(s), hi.max(s)));
            assert!(hi <= lo * 1.5, "{w}x{h}: ink sums {sums:?}");
        }
        for ch in frames {
            let mask = coverage(ch, 7, 15, 1).expect("drawn");
            assert_eq!(ink(&mask).2, 255, "{ch} at 7x15");
        }
    }

    /// The coverage of `mask` (`w` wide) at the continuous point `(x, y)`,
    /// bilinear between texel centres, in `0..=1`.
    fn sample(mask: &[u8], w: usize, x: f32, y: f32) -> f32 {
        let h = mask.len() / w;
        let at = |i: f32, j: f32| {
            let (i, j) = (i.clamp(0.0, (w - 1) as f32), j.clamp(0.0, (h - 1) as f32));
            f32::from(mask[j as usize * w + i as usize]) / 255.0
        };
        let (fx, fy) = (x - 0.5, y - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let top = at(x0, y0) * (1.0 - tx) + at(x0 + 1.0, y0) * tx;
        let bottom = at(x0, y0 + 1.0) * (1.0 - tx) + at(x0 + 1.0, y0 + 1.0) * tx;
        top * (1.0 - ty) + bottom * ty
    }

    /// `(least coverage along the six points, most along the six notches)`
    /// of a six-pointed star drawn at `w`x`h`, sampled at `reach` of its
    /// points' radius from the cell centre.
    fn points_and_notches(mask: &[u8], w: usize, h: usize, reach: f32) -> (f32, f32) {
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        // Side points at the cell's edges (`Prim::Star`).
        let r = reach * (w as f32 / 2.0) / (std::f32::consts::PI / 6.0).cos();
        let along = |deg: f32| {
            let a = deg.to_radians();
            sample(mask, w, cx + r * a.cos(), cy + r * a.sin())
        };
        let points = (0..6).map(|k| along(-90.0 + 60.0 * k as f32));
        let notches = (0..6).map(|k| along(-60.0 + 60.0 * k as f32));
        (points.fold(1.0, f32::min), notches.fold(0.0, f32::max))
    }

    /// `✶` READS AS A SIX-POINTED STAR, NOT A HEXAGON. At 0.6 of its points'
    /// radius each of its six points is at least half covered, and at 0.65
    /// each notch between them is at most 15% covered, at the Windows
    /// default's 1x, 1.25x and 1.5x cells (9x23, 11x29, 13x36) and at 7x15,
    /// 8x17 and 10x21; its side points reach the cell's edges, so it
    /// stands as tall against its width as a face's `✶`. Control: the
    /// proportions it had (inner corners at 0.65 of the points) fill the
    /// notches at least half — the hexagon with stub points the judges saw.
    #[test]
    fn the_six_pointed_star_keeps_its_notches_open() {
        for (w, h) in [(9, 23), (11, 29), (13, 36), (7, 15), (8, 17), (10, 21)] {
            let star = coverage('\u{2736}', w, h, 1).expect("drawn");
            let (points, _) = points_and_notches(&star, w, h, 0.6);
            let (_, notches) = points_and_notches(&star, w, h, 0.65);
            assert!(points >= 0.5, "✶ at {w}x{h}: a point holds {points:.2}");
            assert!(notches <= 0.15, "✶ at {w}x{h}: a notch holds {notches:.2}");
            let edge = |x: usize| (0..h).map(|y| star[y * w + x]).max().unwrap_or(0);
            assert!(
                edge(0) > 0 && edge(w - 1) > 0,
                "✶ at {w}x{h}: its side points reach the cell's edges"
            );
            let fat = draw(spec(Full, &[Star(6, 0.65)]), w, h, 1);
            let (_, filled) = points_and_notches(&fat, w, h, 0.65);
            assert!(
                filled >= 0.5,
                "control at {w}x{h}: notches hold {filled:.2}"
            );
        }
    }
}
