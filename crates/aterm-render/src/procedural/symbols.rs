// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LAST-RESORT fontless SYMBOL glyphs: the geometric shapes, media controls,
//! check marks and tree connector that TUIs lean on, synthesized from the cell
//! geometry when NO face in the resolution chain covers them.
//!
//! # Why this exists (the 2026-09-24 `⏵⏵ bypass permissions on` tofu)
//!
//! Claude Code's and Codex's footers print U+23F5 BLACK MEDIUM RIGHT-POINTING
//! TRIANGLE. macOS always has a face for it (STIX Two Math, Apple Symbols), so
//! it was never seen there. A bare Linux desktop does not: measured on the
//! reporting machine, `fc-list ':charset=23f5'` answers NOTHING — not one
//! installed face covers it — so no fallback chain, fontconfig-driven or not,
//! can ever find a glyph, and the cell drew the primary face's `.notdef` box.
//! The same host has no face at all for ⏴⏶⏷ and ⭘, and reaches ⏩⏸⏹⏺ only
//! through the colour emoji face.
//!
//! # Where it sits
//!
//! Unlike the box-drawing families in the parent module (which pre-empt every
//! font so borders tile seam-free), these glyphs are the LAST tier of
//! [`crate::font_chain::resolve_chain`] ([`crate::font_chain::Tier::Synthetic`]):
//! any real face that covers the code point — primary, broad fallback, symbol
//! face, colour face, runtime discovery — always wins, so a user's font keeps
//! its own design. Only the alternative of `.notdef` tofu is replaced.
//!
//! # Geometry
//!
//! Each glyph is a small list of primitives in a UNIT box (`u, v` in `0..=1`,
//! `v` down) that is scaled to a centred square of side
//! `min(span * cell_w, cell_h) * size` — `span` is the cell count the glyph
//! occupies (2 for an emoji-presentation code point such as ⏩), so a wide
//! symbol is centred across both cells rather than hugging the left one.
//! Filled shapes and strokes are evaluated at the parent module's 4×4
//! subsamples and box-filtered to 8-bit coverage ([`super::Canvas::paint_aa`]),
//! the same regime as the Powerline separators. The strokes use the parent
//! module's `light` rule (`min(w, h) / 8`, floored at one pixel), so an outline
//! symbol has the weight of a light box-drawing line beside it.
//!
//! The bytes are produced ONCE, by `Renderer::rasterize`, and the CPU blit and
//! the GPU atlas both consume that same [`crate::GlyphImage`], so the two
//! backends cannot disagree about these cells.

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
    fn scale(self) -> f32 {
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

use Prim::{Disc, DottedRing, HalfDisc, Line, Poly, PolyOutline, Rect, RectOutline, Ring};
use Size::{Full, Large, Medium, Small};

/// The symbol table: every code point this module draws, and how.
fn lookup(cp: u32) -> Option<Spec> {
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
        // ---- Miscellaneous Symbols and Arrows ------------------------------
        0x2B24 => spec(Full, &[Disc(0.5, 0.5, 0.5)]), // ⬤
        0x2B58 => heavy(Large, &[Ring(0.5, 0.5, 0.5)]), // ⭘
        _ => return None,
    };
    Some(s)
}

/// Whether this module can draw `ch` without any font.
pub(super) fn covers(ch: char) -> bool {
    lookup(u32::from(ch)).is_some()
}

/// Row-major coverage for `ch` at `span * cell_w` × `cell_h` (`span` clamped
/// to `1..=2`), or `None` when `ch` is not a symbol this module draws or the
/// cell is degenerate.
pub(super) fn coverage(ch: char, cell_w: usize, cell_h: usize, span: usize) -> Option<Vec<u8>> {
    if cell_w == 0 || cell_h == 0 {
        return None;
    }
    let cp = u32::from(ch);
    let w = cell_w * span.clamp(1, 2);
    let spec = lookup(cp)?;
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
        }
    }
    Some(c.buf)
}

/// U+23BF DENTISTRY SYMBOL LIGHT VERTICAL AND BOTTOM RIGHT — Claude Code's
/// `⎿` tool-output connector: a light vertical on the `│` columns from the
/// cell's top edge down to the three-quarter line, then a light horizontal
/// from there to the right edge — hard 0/255, the box-drawing regime.
///
/// It is drawn by the PRE-EMPTIVE family ([`super::covers`]), NOT as a
/// last-resort symbol, because a font's `⎿` cannot join the procedural `│`
/// above it: MEASURED on the Linux host this was written on, the only face
/// covering U+23BF is Noto Sans/Serif CJK, whose `⎿` is a short L sitting at
/// the bottom of the cell, leaving a visible gap under the `│` on the row
/// above. As a last-resort tier the join was designed but never reached.
pub(super) fn draw_dentistry_bottom_right(c: &mut Canvas) {
    let m = Metrics::new(c.w, c.h);
    let foot = eighth(6, m.h).clamp(m.light, m.h);
    c.rect(m.vl0, 0, m.vl1, foot);
    c.rect(m.vl0, foot - m.light, m.w, foot);
}

/// A convex polygon in PIXEL space with outward edge normals, for a signed
/// distance: negative inside, the max over edges of the distance past each.
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
