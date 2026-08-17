// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! COLR (vector color glyph) rasterization (M3).
//!
//! `sbix`/`CBDT` color emoji are bitmaps (handled via `glyph_raster_image`); COLR
//! glyphs are VECTOR — a stack of outline layers, each filled with a palette color
//! (COLRv0) or a gradient (COLRv1), under affine transforms. ttf-parser drives the
//! paint graph through its [`Painter`](ttf_parser::colr::Painter) trait; this module
//! implements that trait with a small self-contained scanline rasterizer (no extra
//! deps — outlines come from ttf-parser, the fill is ours), compositing each layer
//! into an RGBA8 buffer the renderer blits like any other color glyph.
//!
//! Scope: solid layers (COLRv0 + COLRv1 solid) render exactly, with full affine
//! transform support; gradients are approximated by their first color stop (a
//! documented refinement — the layer shape/placement is still correct). Clip and
//! composite modes beyond src-over are treated as src-over.

use ttf_parser::colr::{CompositeMode, Paint, Painter};
use ttf_parser::{Face, GlyphId, RgbaColor, Transform};

/// A 2×3 affine: `(x,y) -> (a·x + c·y + e, b·x + d·y + f)` (ttf-parser convention).
#[derive(Clone, Copy)]
struct Affine {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Affine {
    fn apply(self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }
    /// `self ∘ t` — apply `t` first, then `self`.
    fn then(self, t: Affine) -> Affine {
        Affine {
            a: self.a * t.a + self.c * t.b,
            b: self.b * t.a + self.d * t.b,
            c: self.a * t.c + self.c * t.d,
            d: self.b * t.c + self.d * t.d,
            e: self.a * t.e + self.c * t.f + self.e,
            f: self.b * t.e + self.d * t.f + self.f,
        }
    }
}

impl From<Transform> for Affine {
    fn from(t: Transform) -> Self {
        Affine {
            a: t.a,
            b: t.b,
            c: t.c,
            d: t.d,
            e: t.e,
            f: t.f,
        }
    }
}

/// Flatten a glyph's outline into pixel-space contours (a flat polyline per
/// contour), transforming every point through `xform`.
struct OutlineCollector {
    xform: Affine,
    contours: Vec<Vec<(f32, f32)>>,
    cur: Vec<(f32, f32)>,
    last: (f32, f32),
}

impl OutlineCollector {
    fn flush(&mut self) {
        if self.cur.len() > 1 {
            self.contours.push(std::mem::take(&mut self.cur));
        } else {
            self.cur.clear();
        }
    }
    fn push(&mut self, x: f32, y: f32) {
        let (px, py) = self.xform.apply(x, y);
        self.cur.push((px, py));
        self.last = (x, y);
    }
}

impl ttf_parser::OutlineBuilder for OutlineCollector {
    fn move_to(&mut self, x: f32, y: f32) {
        self.flush();
        self.push(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.push(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (x0, y0) = self.last;
        const N: usize = 10;
        for i in 1..=N {
            let t = i as f32 / N as f32;
            let mt = 1.0 - t;
            let bx = mt * mt * x0 + 2.0 * mt * t * x1 + t * t * x;
            let by = mt * mt * y0 + 2.0 * mt * t * y1 + t * t * y;
            self.push(bx, by);
        }
        self.last = (x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (x0, y0) = self.last;
        const N: usize = 12;
        for i in 1..=N {
            let t = i as f32 / N as f32;
            let mt = 1.0 - t;
            let bx =
                mt * mt * mt * x0 + 3.0 * mt * mt * t * x1 + 3.0 * mt * t * t * x2 + t * t * t * x;
            let by =
                mt * mt * mt * y0 + 3.0 * mt * mt * t * y1 + 3.0 * mt * t * t * y2 + t * t * t * y;
            self.push(bx, by);
        }
        self.last = (x, y);
    }
    fn close(&mut self) {
        self.flush();
    }
}

/// RGBA8 accumulator + the ttf-parser COLR painter state.
struct ColrCanvas<'a> {
    face: &'a Face<'a>,
    w: usize,
    h: usize,
    buf: Vec<u8>,
    stack: Vec<Affine>,
    cur: Affine,
    palette: u16,
    foreground: RgbaColor,
    pending: Option<GlyphId>,
}

impl ColrCanvas<'_> {
    /// src-over composite `color` at coverage `cov` (0..=1) into pixel (x,y).
    fn blend(&mut self, x: usize, y: usize, color: RgbaColor, cov: f32) {
        if x >= self.w || y >= self.h {
            return;
        }
        let sa = f32::from(color.alpha) / 255.0 * cov;
        if sa <= 0.0 {
            return;
        }
        let i = (y * self.w + x) * 4;
        for (k, sc) in [color.red, color.green, color.blue].into_iter().enumerate() {
            let dst = f32::from(self.buf[i + k]);
            self.buf[i + k] = (f32::from(sc) * sa + dst * (1.0 - sa)).round() as u8;
        }
        let da = f32::from(self.buf[i + 3]) / 255.0;
        self.buf[i + 3] = ((sa + da * (1.0 - sa)) * 255.0).round() as u8;
    }

    /// Scanline-fill the current pending outline (transformed by `cur`) with `color`,
    /// 4× vertical supersampling + analytic horizontal coverage, nonzero winding.
    fn fill_pending(&mut self, color: RgbaColor) {
        let Some(gid) = self.pending.take() else {
            return;
        };
        let mut oc = OutlineCollector {
            xform: self.cur,
            contours: Vec::new(),
            cur: Vec::new(),
            last: (0.0, 0.0),
        };
        if self.face.outline_glyph(gid, &mut oc).is_none() {
            return;
        }
        oc.flush();
        let contours = oc.contours;
        if contours.is_empty() {
            return;
        }
        const SS: usize = 4;
        let (w, h) = (self.w, self.h);
        // Flatten the contours into ONE edge list per layer instead of re-walking
        // the Vec-of-Vec (with its `% n` wrap) for each of the h×SS sample lines.
        // The closing edge still comes from the `contour[(i + 1) % n]` rule, so
        // contour closure is preserved, and edges are emitted in the SAME
        // contour-then-index order — the per-scanline crossings are therefore
        // pushed in the same order and the STABLE `sort_by` below yields the
        // identical ordering. Horizontal edges are dropped: `y0 == y1` can never
        // satisfy the straddle test `(y0 <= sy && y1 > sy) || (y1 <= sy && y0 > sy)`
        // (one endpoint would have to be both ≤ and > sy), so removing them is
        // exact. A NaN endpoint makes `y0 == y1` false, so such an edge is KEPT and
        // still fails the test, exactly as before.
        let mut edges: Vec<(f32, f32, f32, f32)> = Vec::new();
        // The layer's y extent, folded from the same edges. `f32::min`/`max` ignore
        // NaN, so a NaN endpoint cannot poison the range (and its edge can never
        // cross a scanline anyway).
        let (mut min_y, mut max_y) = (f32::INFINITY, f32::NEG_INFINITY);
        for contour in &contours {
            let n = contour.len();
            for i in 0..n {
                let (x0, y0) = contour[i];
                let (x1, y1) = contour[(i + 1) % n];
                if y0 == y1 {
                    continue;
                }
                edges.push((x0, y0, x1, y1));
                min_y = min_y.min(y0).min(y1);
                max_y = max_y.max(y0).max(y1);
            }
        }
        if edges.is_empty() {
            return;
        }
        // Scan only the rows the layer's y extent can reach. OUTSIDE that range no
        // edge satisfies the straddle test for any sample line, so `xs` would be
        // empty, `wind` would stay 0, `add_span` would never run and `cov` would
        // stay all-zero — and the `c > 0.0` guard already suppresses every `blend`
        // for a zero row. So the skipped rows wrote nothing: this is exact, not an
        // approximation. The 1-row margin absorbs the +0.125/+0.875 sample offsets.
        // Float→int casts saturate (and NaN → 0), so a degenerate extent collapses
        // to an empty range rather than wrapping.
        let py_lo = (min_y - 1.0).floor().max(0.0) as usize;
        let py_hi = ((max_y + 1.0).ceil().max(0.0) as usize).min(h);
        // Both scratch buffers are hoisted out of the loops and reused: `fill(0.0)`
        // is exactly the old fresh `vec![0.0f32; w]`, and `clear()` + the identical
        // push sequence is exactly the old fresh `Vec` — only the capacity survives.
        let mut cov = vec![0.0f32; w];
        let mut xs: Vec<(f32, i32)> = Vec::new();
        for py in py_lo..py_hi {
            cov.fill(0.0);
            for s in 0..SS {
                let sy = py as f32 + (s as f32 + 0.5) / SS as f32;
                // Collect (x, winding-dir) crossings of all edges at scanline sy.
                xs.clear();
                for &(x0, y0, x1, y1) in &edges {
                    if (y0 <= sy && y1 > sy) || (y1 <= sy && y0 > sy) {
                        let t = (sy - y0) / (y1 - y0);
                        xs.push((x0 + t * (x1 - x0), if y1 > y0 { 1 } else { -1 }));
                    }
                }
                xs.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut wind = 0;
                let mut prev = 0.0f32;
                for &(x, d) in &xs {
                    if wind != 0 {
                        add_span(&mut cov, prev, x, 1.0 / SS as f32);
                    }
                    wind += d;
                    prev = x;
                }
            }
            for (px, &c) in cov.iter().enumerate() {
                if c > 0.0 {
                    self.blend(px, py, color, c.min(1.0));
                }
            }
        }
    }

    /// Approximate a gradient by its first color stop (shape/placement stay exact).
    fn gradient_first_stop(&self, paint: &Paint) -> Option<RgbaColor> {
        // `stops(palette, coords)` — pass default (non-variable) coords.
        let mut stops = match paint {
            Paint::LinearGradient(g) => g.stops(self.palette, &[]),
            Paint::RadialGradient(g) => g.stops(self.palette, &[]),
            Paint::SweepGradient(g) => g.stops(self.palette, &[]),
            Paint::Solid(_) => return None,
        };
        stops.next().map(|s| s.color)
    }
}

impl<'a> Painter<'a> for ColrCanvas<'a> {
    fn outline_glyph(&mut self, glyph_id: GlyphId) {
        self.pending = Some(glyph_id);
    }
    fn paint(&mut self, paint: Paint<'a>) {
        let color = match paint {
            Paint::Solid(c) => c,
            ref g => self.gradient_first_stop(g).unwrap_or(self.foreground),
        };
        self.fill_pending(color);
    }
    fn push_transform(&mut self, transform: Transform) {
        self.stack.push(self.cur);
        self.cur = self.cur.then(Affine::from(transform));
    }
    fn pop_transform(&mut self) {
        if let Some(t) = self.stack.pop() {
            self.cur = t;
        }
    }
    // Clip + layer composition beyond src-over are not modeled; the layer outlines
    // self-clip, so ignoring these renders solid-layer COLR correctly.
    fn push_clip(&mut self) {}
    fn push_clip_box(&mut self, _clipbox: ttf_parser::colr::ClipBox) {}
    fn pop_clip(&mut self) {}
    fn push_layer(&mut self, _mode: CompositeMode) {}
    fn pop_layer(&mut self) {}
}

/// Rasterize the COLR color glyph `gid` into an `w×h` RGBA8 buffer (em square fit
/// to the box, centered, y-flipped). Returns `None` if `gid` is not a COLR glyph or
/// the face has no COLR table. Pure + panic-free.
pub(crate) fn rasterize_colr(face: &Face, gid: GlyphId, w: usize, h: usize) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || !face.is_color_glyph(gid) {
        return None;
    }
    let upem = f32::from(face.units_per_em());
    if upem <= 0.0 {
        return None;
    }
    // Fit the em square into the box (by height), centered horizontally, y-flipped:
    // font (0,0) -> bottom-left, (upem,upem) -> top, so the glyph fills the cell.
    let scale = h as f32 / upem;
    let base = Affine {
        a: scale,
        b: 0.0,
        c: 0.0,
        d: -scale,
        e: (w as f32 - upem * scale) / 2.0,
        f: h as f32,
    };
    let fg = RgbaColor::new(0, 0, 0, 255);
    let mut canvas = ColrCanvas {
        face,
        w,
        h,
        buf: vec![0u8; w * h * 4],
        stack: Vec::new(),
        cur: base,
        palette: 0,
        foreground: fg,
        pending: None,
    };
    face.paint_color_glyph(gid, 0, fg, &mut canvas)?;
    // The blend accumulator stored PREMULTIPLIED rgb — each layer composited over a
    // transparent start yields rgb = color·alpha — but `GlyphImage::Rgba` is blitted
    // as STRAIGHT alpha (blit_rgba_* do bg·(1−a) + rgb·a, like the PNG color-emoji
    // path). Un-premultiply so a partially-transparent COLR layer is not darkened
    // twice (rendered at ~a² instead of a). Fully-opaque/transparent pixels are
    // already correct (×1 / rgb=0), so skip them — the opaque case stays byte-exact.
    let mut buf = canvas.buf;
    for px in buf.as_chunks_mut::<4>().0 {
        let a = u32::from(px[3]);
        if a == 0 || a == 255 {
            continue;
        }
        let (rgb, _alpha) = px.split_at_mut(3);
        for c in rgb {
            *c = (((u32::from(*c) * 255) + a / 2) / a).min(255) as u8;
        }
    }
    Some(buf)
}

/// Add `amount` coverage to `cov` over the half-open pixel span `[x0, x1)`, with
/// fractional coverage at the partially-covered end pixels.
fn add_span(cov: &mut [f32], x0: f32, x1: f32, amount: f32) {
    let (x0, x1) = (x0.max(0.0), x1.min(cov.len() as f32));
    if x1 <= x0 {
        return;
    }
    let xi0 = x0.floor() as usize;
    let xi1 = x1.ceil() as usize;
    for (px, slot) in cov.iter_mut().enumerate().take(xi1).skip(xi0) {
        let left = (px as f32).max(x0);
        let right = ((px + 1) as f32).min(x1);
        if right > left {
            *slot += amount * (right - left);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_span_full_and_partial_coverage() {
        let mut cov = vec![0.0f32; 5];
        add_span(&mut cov, 1.0, 3.0, 1.0); // pixels 1,2 fully
        assert_eq!(cov, vec![0.0, 1.0, 1.0, 0.0, 0.0]);
        let mut cov = vec![0.0f32; 5];
        add_span(&mut cov, 1.5, 2.5, 1.0); // half of 1, half of 2
        assert!((cov[1] - 0.5).abs() < 1e-6 && (cov[2] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn affine_compose_then_apply() {
        // Translate-then-scale composition matches manual math.
        let scale = Affine {
            a: 2.0,
            b: 0.0,
            c: 0.0,
            d: 2.0,
            e: 0.0,
            f: 0.0,
        };
        let translate = Affine {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 3.0,
            f: 4.0,
        };
        let m = scale.then(translate); // apply translate first, then scale
        assert_eq!(m.apply(0.0, 0.0), (6.0, 8.0));
    }

    /// Locate ttf-parser's bundled COLR test font in the cargo registry cache, if
    /// present (it ships `tests/fonts/colr_1.ttf`). Returns the bytes or `None`.
    fn colr_test_font() -> Option<Vec<u8>> {
        let home = std::env::var_os("CARGO_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cargo"))
            })?;
        let reg = home.join("registry/src");
        let entries = std::fs::read_dir(&reg).ok()?;
        for idx in entries.flatten() {
            let mut p = idx.path();
            // newest ttf-parser-* dir wins (any works for the COLR font)
            if let Ok(rd) = std::fs::read_dir(&p) {
                let mut best: Option<std::path::PathBuf> = None;
                for e in rd.flatten() {
                    let name = e.file_name();
                    if name.to_string_lossy().starts_with("ttf-parser-") {
                        best = Some(e.path());
                    }
                }
                if let Some(b) = best {
                    p = b.join("tests/fonts/colr_1.ttf");
                    if let Ok(bytes) = std::fs::read(&p) {
                        return Some(bytes);
                    }
                }
            }
        }
        None
    }

    #[test]
    fn colr_glyph_rasterizes_straight_alpha() {
        let Some(bytes) = colr_test_font() else {
            eprintln!("SKIP: ttf-parser colr_1.ttf not in the cargo cache");
            return;
        };
        let face = Face::parse(&bytes, 0).expect("valid font");
        let gid = (0..face.number_of_glyphs())
            .map(GlyphId)
            .find(|&g| face.is_color_glyph(g))
            .expect("colr_1.ttf has color glyphs");
        let (w, h) = (48, 48);
        let rgba = rasterize_colr(&face, gid, w, h).expect("COLR glyph rasterizes");
        assert_eq!(rgba.len(), w * h * 4);
        assert!(
            rgba.as_chunks::<4>().0.iter().any(|p| p[3] > 0),
            "COLR glyph produced no painted pixels"
        );
        // The opaque interior defines the glyph's TRUE (straight) colour. This font's
        // first color glyph is a single solid fill, so there is exactly one.
        let opaque: std::collections::HashSet<(u8, u8, u8)> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] == 255)
            .map(|p| (p[0], p[1], p[2]))
            .collect();
        assert_eq!(
            opaque.len(),
            1,
            "expected one solid opaque colour, got {opaque:?}"
        );
        let (r0, g0, b0) = *opaque.iter().next().unwrap();
        // REGRESSION: `GlyphImage::Rgba` is blitted as STRAIGHT alpha, so a
        // semi-transparent AA-edge pixel must carry the SAME straight colour as the
        // opaque interior — NOT the premultiplied (colour·alpha) value that used to
        // darken every COLR glyph toward black. (Previously this test "passed" only
        // because it counted those premultiplied shades as distinct "colours".)
        let edges: Vec<&[u8; 4]> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| (96..255).contains(&p[3]))
            .collect();
        assert!(!edges.is_empty(), "COLR glyph has AA-edge pixels to verify");
        for p in &edges {
            assert!(
                p[0].abs_diff(r0) < 40 && p[1].abs_diff(g0) < 40 && p[2].abs_diff(b0) < 40,
                "AA-edge pixel {:?} (a={}) must match the straight interior colour ({r0},{g0},{b0}), not be premultiplied-dark",
                &p[..3],
                p[3]
            );
        }
    }
}
