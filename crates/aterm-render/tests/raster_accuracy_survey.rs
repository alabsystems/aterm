// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Accuracy guard on `aterm_render::raster` against the analytic reference.

const DEJAVU: &[u8] = include_bytes!("../assets/DejaVuSansMono.ttf");
const NERD: &[u8] = include_bytes!("../assets/SymbolsNerdFontMono-Regular.ttf");

#[derive(Clone, Copy, Debug)]
struct P {
    x: f32,
    y: f32,
}

#[derive(Clone, Copy, Debug)]
enum Seg {
    Line(P, P),
    Quad(P, P, P),
    Cubic(P, P, P, P),
}

#[derive(Default)]
struct Rec {
    segs: Vec<Seg>,
    start: P,
    last: P,
    started: bool,
    cubics: usize,
}

impl Default for P {
    fn default() -> Self {
        P { x: 0.0, y: 0.0 }
    }
}

impl Rec {
    fn close_contour(&mut self) {
        if self.started && (self.last.x != self.start.x || self.last.y != self.start.y) {
            self.segs.push(Seg::Line(self.last, self.start));
            self.last = self.start;
        }
    }
}

impl aterm_font::OutlineSink for Rec {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close_contour();
        self.start = P { x, y };
        self.last = self.start;
        self.started = true;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = P { x, y };
        self.segs.push(Seg::Line(self.last, p));
        self.last = p;
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let p = P { x, y };
        self.segs.push(Seg::Quad(self.last, P { x: cx, y: cy }, p));
        self.last = p;
    }
    fn curve_to(&mut self, ax: f32, ay: f32, bx: f32, by: f32, x: f32, y: f32) {
        let p = P { x, y };
        self.segs.push(Seg::Cubic(
            self.last,
            P { x: ax, y: ay },
            P { x: bx, y: by },
            p,
        ));
        self.last = p;
        self.cubics += 1;
    }
    fn close(&mut self) {
        self.close_contour();
    }
}

fn record(face: &aterm_font::Face, gid: u16) -> Option<Rec> {
    let mut r = Rec::default();
    face.outline_glyph(aterm_font::GlyphId(gid), &mut r)?;
    r.close_contour();
    Some(r)
}

/// Sub-scanlines per pixel row. 64 is "8x supersampled" squared in y; the x
const SUB: usize = 64;
/// Uniform subdivisions flattening a quad/cubic, for the reference only. A
const REF_SEGMENTS: usize = 256;

/// A flat edge list in grid space (px, y down, origin at the canvas top-left).
fn flatten_ref(segs: &[Seg], scale: f32, ox: f32, oy: f32, n: usize) -> Vec<(P, P)> {
    let map = |p: P| P {
        x: p.x * scale - ox,
        y: oy - p.y * scale,
    };
    let lerp = |t: f32, a: P, b: P| P {
        x: a.x + t * (b.x - a.x),
        y: a.y + t * (b.y - a.y),
    };
    let mut out = Vec::new();
    for s in segs {
        match *s {
            Seg::Line(a, b) => out.push((map(a), map(b))),
            Seg::Quad(a, c, b) => {
                let (a, c, b) = (map(a), map(c), map(b));
                let mut prev = a;
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let p = lerp(t, lerp(t, a, c), lerp(t, c, b));
                    out.push((prev, p));
                    prev = p;
                }
            }
            Seg::Cubic(a, c0, c1, b) => {
                let (a, c0, c1, b) = (map(a), map(c0), map(c1), map(b));
                let mut prev = a;
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let p = lerp(
                        t,
                        lerp(t, lerp(t, a, c0), lerp(t, c0, c1)),
                        lerp(t, lerp(t, c0, c1), lerp(t, c1, b)),
                    );
                    out.push((prev, p));
                    prev = p;
                }
            }
        }
    }
    out
}

/// Two crossings this close in x are one crossing point: the interval between
const X_TIE_PX: f32 = 1e-6;

/// Nonzero-winding coverage of `edges` over a `w`x`h` grid, in 0..=1, and
fn reference_coverage_phased(
    edges: &[(P, P)],
    w: usize,
    h: usize,
    sub: usize,
    phase: f32,
) -> (Vec<f32>, bool) {
    let buckets = row_buckets(edges, h, phase);
    reference_sweep(edges, w, h, sub, phase, |row| buckets[row].as_slice())
}

/// The edge indices each pixel row's sub-scanlines can cross, in edge order.
fn row_buckets(edges: &[(P, P)], h: usize, phase: f32) -> Vec<Vec<u32>> {
    let mut buckets = vec![Vec::new(); h];
    if h == 0 {
        return buckets;
    }
    for (i, &(p0, p1)) in edges.iter().enumerate() {
        let (ylo, yhi) = if p0.y <= p1.y {
            (p0.y, p1.y)
        } else {
            (p1.y, p0.y)
        };
        if ylo.partial_cmp(&yhi) != Some(std::cmp::Ordering::Less) {
            continue;
        }
        let first = ((f64::from(ylo) - f64::from(phase)).floor() - 2.0).max(0.0);
        let last = ((f64::from(yhi) - f64::from(phase)).floor() + 2.0).min(h as f64 - 1.0);
        if first > last {
            continue;
        }
        for bucket in &mut buckets[first as usize..=last as usize] {
            bucket.push(i as u32);
        }
    }
    buckets
}

/// The sweep behind [`reference_coverage_phased`]: `candidates(row)` names the
fn reference_sweep<'a>(
    edges: &[(P, P)],
    w: usize,
    h: usize,
    sub: usize,
    phase: f32,
    candidates: impl Fn(usize) -> &'a [u32],
) -> (Vec<f32>, bool) {
    let mut cov = vec![0.0f64; w * h];
    let mut overlapped = false;
    let (mut saw_pos, mut saw_neg) = (false, false);
    let mut xs: Vec<(f32, i32)> = Vec::new();
    for row in 0..h {
        for k in 0..sub {
            let gy = row as f32 + (k as f32 + 0.5) / sub as f32 + phase;
            xs.clear();
            for &e in candidates(row) {
                let (p0, p1) = edges[e as usize];
                let (dir, lo, hi) = if p0.y <= p1.y {
                    (1, p0, p1)
                } else {
                    (-1, p1, p0)
                };
                if lo.y <= gy && gy < hi.y {
                    let t = (gy - lo.y) / (hi.y - lo.y);
                    xs.push((lo.x + t * (hi.x - lo.x), dir));
                }
            }
            if xs.is_empty() {
                continue;
            }
            xs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            let mut wind = 0i32;
            let mut span_start = 0.0f32;
            let mut i = 0usize;
            while i < xs.len() {
                let x = xs[i].0;
                let prev = wind;
                while i < xs.len() && xs[i].0 - x <= X_TIE_PX {
                    wind += xs[i].1;
                    i += 1;
                }
                if wind.abs() >= 2 {
                    overlapped = true;
                }
                if wind > 0 {
                    saw_pos = true;
                } else if wind < 0 {
                    saw_neg = true;
                }
                if prev == 0 && wind != 0 {
                    span_start = x;
                } else if prev != 0 && wind == 0 {
                    deposit(&mut cov[row * w..(row + 1) * w], span_start, x, w);
                }
            }
        }
    }
    (
        cov.iter().map(|v| (*v / sub as f64) as f32).collect(),
        overlapped || (saw_pos && saw_neg),
    )
}

/// Add the exact per-column overlap of `[xa, xb)` into one row.
fn deposit(row: &mut [f64], xa: f32, xb: f32, w: usize) {
    let xa = xa.max(0.0);
    let xb = xb.min(w as f32);
    if xb <= xa {
        return;
    }
    let c0 = xa.floor() as usize;
    let c1 = ((xb.ceil() as usize).saturating_sub(1)).min(w - 1);
    for (c, cell) in row.iter_mut().enumerate().take(c1 + 1).skip(c0) {
        let l = xa.max(c as f32);
        let r = xb.min(c as f32 + 1.0);
        if r > l {
            *cell += f64::from(r - l);
        }
    }
}

struct Canvas {
    x0: i32,
    ytop: i32,
    w: usize,
    h: usize,
}

/// Place a candidate mask (its own `xmin`/`ymin`/`w`/`h`, rows top-down) into
fn place(
    canvas: &Canvas,
    dst: &mut [f32],
    mask: &[u8],
    mw: usize,
    mh: usize,
    xmin: i32,
    ymin: i32,
) -> bool {
    if mw == 0 || mh == 0 {
        return true;
    }
    let cx = xmin - canvas.x0;
    let cy = canvas.ytop - (ymin + mh as i32);
    if cx < 0 || cy < 0 || cx + mw as i32 > canvas.w as i32 || cy + mh as i32 > canvas.h as i32 {
        return false;
    }
    for r in 0..mh {
        for c in 0..mw {
            dst[(cy as usize + r) * canvas.w + cx as usize + c] = f32::from(mask[r * mw + c]);
        }
    }
    true
}

/// The placement of one fill: design-to-pixel `scale`, the translation the
#[derive(Clone, Copy)]
struct FillGrid {
    scale: f32,
    ox: f32,
    oy: f32,
    gw: usize,
    gh: usize,
}

/// Rasterize with `aterm_render::raster`'s fill but the harness's own
fn fill_with_tolerance(
    segs: &[Seg],
    grid: FillGrid,
    tol: Option<f32>,
    segments: &mut usize,
) -> aterm_render::raster::Rasterizer {
    use aterm_render::raster::{Rasterizer, point};
    let FillGrid {
        scale,
        ox,
        oy,
        gw,
        gh,
    } = grid;
    let mut ras = Rasterizer::new(gw, gh);
    let map = |p: P| point(p.x * scale - ox, oy - p.y * scale);
    let lerp = |t: f32, a: aterm_render::raster::Point, b: aterm_render::raster::Point| {
        point(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y))
    };
    for s in segs {
        match *s {
            Seg::Line(a, b) => {
                ras.draw_line(map(a), map(b));
                *segments += 1;
            }
            Seg::Quad(a, c, b) => {
                let (a, c, b) = (map(a), map(c), map(b));
                match tol {
                    None => {
                        ras.draw_quad(a, c, b);
                        *segments += 1;
                    }
                    Some(tol) => {
                        let dx = a.x - 2.0 * c.x + b.x;
                        let dy = a.y - 2.0 * c.y + b.y;
                        let dev = (dx * dx + dy * dy).sqrt();
                        let n = ((dev / (4.0 * tol)).sqrt().ceil() as usize).clamp(1, 4096);
                        let mut prev = a;
                        for i in 1..=n {
                            let t = i as f32 / n as f32;
                            let p = lerp(t, lerp(t, a, c), lerp(t, c, b));
                            ras.draw_line(prev, p);
                            prev = p;
                        }
                        *segments += n;
                    }
                }
            }
            Seg::Cubic(a, c0, c1, b) => {
                let (a, c0, c1, b) = (map(a), map(c0), map(c1), map(b));
                match tol {
                    None => {
                        ras.draw_cubic(a, c0, c1, b);
                        *segments += 1;
                    }
                    Some(tol) => {
                        let d1x = a.x - 2.0 * c0.x + c1.x;
                        let d1y = a.y - 2.0 * c0.y + c1.y;
                        let d2x = c0.x - 2.0 * c1.x + b.x;
                        let d2y = c0.y - 2.0 * c1.y + b.y;
                        let dev =
                            ((d1x * d1x + d1y * d1y).sqrt()).max((d2x * d2x + d2y * d2y).sqrt());
                        let n = ((3.0 * dev / (4.0 * tol)).sqrt().ceil() as usize).clamp(1, 4096);
                        let mut prev = a;
                        for i in 1..=n {
                            let t = i as f32 / n as f32;
                            let p = lerp(
                                t,
                                lerp(t, lerp(t, a, c0), lerp(t, c0, c1)),
                                lerp(t, lerp(t, c0, c1), lerp(t, c1, b)),
                            );
                            ras.draw_line(prev, p);
                            prev = p;
                        }
                        *segments += n;
                    }
                }
            }
        }
    }
    ras
}

/// The crop + quantization `variation::crop_padded_coverage` performs.
fn crop(ras: &aterm_render::raster::Rasterizer, w: usize, h: usize, pad: usize) -> Vec<u8> {
    let gw = w + 2 * pad;
    let mut cov = vec![0u8; w * h];
    ras.for_each_pixel(|i, a| {
        let (gx, gy) = (i % gw, i / gw);
        let (Some(x), Some(y)) = (gx.checked_sub(pad), gy.checked_sub(pad)) else {
            return;
        };
        if x < w && y < h {
            cov[y * w + x] = (a * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
    });
    cov
}

#[derive(Default, Clone, Copy)]
struct ErrStat {
    sum: f64,
    max: f32,
    n: usize,
    ink: usize,
}

impl ErrStat {
    fn add(&mut self, e: f32, is_ink: bool) {
        self.sum += f64::from(e);
        if e > self.max {
            self.max = e;
        }
        self.n += 1;
        if is_ink {
            self.ink += 1;
        }
    }
    fn mean(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.sum / self.n as f64
        }
    }
    fn merge(&mut self, o: &ErrStat) {
        self.sum += o.sum;
        self.max = self.max.max(o.max);
        self.n += o.n;
        self.ink += o.ink;
    }
}

struct GlyphCase {
    canvas: Canvas,
    reference: Vec<f32>,
    segs: Vec<Seg>,
    scale: f32,
    vx: i32,
    vy: i32,
    vw: usize,
    vh: usize,
    overlapped: bool,
}

fn build_case(face: &aterm_font::Face, gid: u16, px: f32) -> Option<GlyphCase> {
    build_case_phased(face, gid, px, 0.0)
}

/// [`build_case`] with the reference's sub-scanline ladder displaced by `phase`
fn build_case_phased(face: &aterm_font::Face, gid: u16, px: f32, phase: f32) -> Option<GlyphCase> {
    let upem = f32::from(face.units_per_em());
    let scale = px / upem;
    let rec = record(face, gid)?;
    if rec.segs.is_empty() {
        return None;
    }
    let bbox = face.glyph_bounding_box(aterm_font::GlyphId(gid))?;
    let vx = (f32::from(bbox.x_min) * scale).floor() as i32;
    let vy = (f32::from(bbox.y_min) * scale).floor() as i32;
    let vw =
        ((f32::from(bbox.x_max) * scale).ceil() - (f32::from(bbox.x_min) * scale).floor()) as i32;
    let vh =
        ((f32::from(bbox.y_max) * scale).ceil() - (f32::from(bbox.y_min) * scale).floor()) as i32;
    if vw <= 0 || vh <= 0 || vw > 4096 || vh > 4096 {
        return None;
    }
    const M: i32 = 3;
    let canvas = Canvas {
        x0: vx - M,
        ytop: vy + vh + M,
        w: (vw + 2 * M) as usize,
        h: (vh + 2 * M) as usize,
    };
    let edges = flatten_ref(
        &rec.segs,
        scale,
        canvas.x0 as f32,
        canvas.ytop as f32,
        REF_SEGMENTS,
    );
    let (raw, overlapped) = reference_coverage_phased(&edges, canvas.w, canvas.h, SUB, phase);
    let reference = raw.iter().map(|c| c * 255.0).collect();
    Some(GlyphCase {
        canvas,
        reference,
        segs: rec.segs,
        scale,
        vx,
        vy,
        vw: vw as usize,
        vh: vh as usize,
        overlapped,
    })
}

impl GlyphCase {
    fn score(
        &self,
        mask: &[u8],
        mw: usize,
        mh: usize,
        xmin: i32,
        ymin: i32,
        e: &mut ErrStat,
    ) -> bool {
        let mut placed = vec![0.0f32; self.canvas.w * self.canvas.h];
        if !place(&self.canvas, &mut placed, mask, mw, mh, xmin, ymin) {
            return false;
        }
        for (i, r) in self.reference.iter().enumerate() {
            e.add((placed[i] - r).abs(), *r > 0.5);
        }
        true
    }

    /// The shipped first-party path, or a finer-flattened twin of it.
    fn first_party(
        &self,
        tol: Option<f32>,
        segments: &mut usize,
    ) -> (Vec<u8>, usize, usize, i32, i32) {
        const PAD: usize = 1;
        let ras = fill_with_tolerance(
            &self.segs,
            FillGrid {
                scale: self.scale,
                ox: (self.vx - PAD as i32) as f32,
                oy: (self.vy + self.vh as i32 + PAD as i32) as f32,
                gw: self.vw + 2 * PAD,
                gh: self.vh + 2 * PAD,
            },
            tol,
            segments,
        );
        (
            crop(&ras, self.vw, self.vh, PAD),
            self.vw,
            self.vh,
            self.vx,
            self.vy,
        )
    }
}

fn corpus_chars() -> Vec<char> {
    (0x21u32..0x7fu32).filter_map(char::from_u32).collect()
}

const PX: [f32; 8] = [8.0, 10.0, 12.0, 14.0, 16.0, 20.0, 24.0, 32.0];

/// Ground truth is expensive — 64 sub-scanlines over a 256-fold reference
const fn guard_corpus() -> (&'static [f32], usize, usize) {
    if cfg!(debug_assertions) {
        // where a coarse flattening degrades worst.
        (&[12.0, 32.0], 4, 130)
    } else {
        (&PX, 1, 2_050)
    }
}

/// The characters the guard scores on one face, at sampling `stride`.
fn guard_chars(bytes: &'static [u8], name: &str, stride: usize) -> Vec<char> {
    let all: Vec<char> = if name == "SymbolsNerd" {
        let face = aterm_font::Face::parse(bytes, 0).unwrap();
        let mut v = Vec::new();
        if let Some(st) = face
            .tables()
            .cmap
            .and_then(|c| c.subtables.into_iter().next())
        {
            st.codepoints(|cp| {
                if v.len() < 200
                    && let Some(c) = char::from_u32(cp)
                {
                    v.push(c);
                }
            });
        }
        v
    } else {
        corpus_chars()
    };
    all.into_iter().step_by(stride).collect()
}

/// The accuracy bound on `raster::draw_quad` / `raster::draw_cubic`, in three
#[test]
fn flattening_accuracy_is_bounded() {
    /// Corpus mean |coverage error|, in 0..255 units, pooled over every raster.
    const MEAN_LIMIT: f64 = 0.100;
    /// Mean |coverage error| of one (face, size) row, in 0..255 units.
    const ROW_MEAN_LIMIT: f64 = 0.130;
    /// Worst single cell of any one glyph, in 0..255 units.
    const MAX_LIMIT: f32 = 6.0;
    let (pxs, stride, floor) = guard_corpus();
    let mut total = ErrStat::default();
    let mut worst: Option<(String, f32)> = None;
    let mut scored = 0usize;
    let mut excluded = 0usize;
    let mut rows: Vec<(String, usize, ErrStat)> = Vec::new();
    for (bytes, name) in [(DEJAVU, "DejaVuSansMono"), (NERD, "SymbolsNerd")] {
        let face = aterm_font::Face::parse(bytes, 0).unwrap();
        let chars = guard_chars(bytes, name, stride);
        for &px in pxs {
            let mut row = ErrStat::default();
            let mut row_rasters = 0usize;
            for &ch in &chars {
                let Some(gid) = face.glyph_index(ch) else {
                    continue;
                };
                let Some(case) = build_case(&face, gid.0, px) else {
                    continue;
                };
                if case.overlapped {
                    excluded += 1;
                    continue;
                }
                let mut segs = 0usize;
                let (mask, w, h, x, y) = case.first_party(None, &mut segs);
                let mut per_glyph = ErrStat::default();
                assert!(
                    case.score(&mask, w, h, x, y, &mut per_glyph),
                    "{name} {ch:?} @ {px}px: the shipped mask escaped the canvas"
                );
                if worst.as_ref().is_none_or(|(_, m)| per_glyph.max > *m) {
                    worst = Some((format!("{name} {ch:?} @ {px}px"), per_glyph.max));
                }
                assert!(
                    per_glyph.max <= MAX_LIMIT,
                    "{name} {ch:?} @ {px}px: worst cell is {:.2}/255, over the {MAX_LIMIT}/255 \
                     per-glyph bound. The flattening in raster.rs (FLATTEN_SAGITTA_PX) is the \
                     thing this guards; re-derive the budget, do not raise this number.",
                    per_glyph.max
                );
                row.merge(&per_glyph);
                scored += 1;
                row_rasters += 1;
            }
            total.merge(&row);
            rows.push((format!("{name} {px:>4.0}px"), row_rasters, row));
        }
    }
    assert!(
        scored >= floor,
        "only {scored} glyph rasters scored, under the {floor} this corpus should reach — \
         the corpus went missing"
    );
    let mean = total.mean();
    let (worst_case, worst_err) = worst.expect("a scored corpus has a worst glyph");
    eprintln!(
        "\n{:<20} {:>8} {:>9} {:>9}",
        "row", "rasters", "mean", "headroom"
    );
    for (label, n, row) in &rows {
        eprintln!(
            "{label:<20} {n:>8} {:>9.4} {:>8.2}x",
            row.mean(),
            ROW_MEAN_LIMIT / row.mean().max(f64::MIN_POSITIVE),
        );
    }
    eprintln!(
        "flattening accuracy: mean {mean:.4}/255 (bound {MEAN_LIMIT}, headroom {:.2}x), worst \
         cell {worst_err:.2}/255 at {worst_case} (bound {MAX_LIMIT}, headroom {:.2}x), over \
         {scored} rasters ({excluded} excluded by the winding classifier)",
        MEAN_LIMIT / mean.max(f64::MIN_POSITIVE),
        f64::from(MAX_LIMIT) / f64::from(worst_err).max(f64::MIN_POSITIVE),
    );
    for (label, n, row) in &rows {
        assert!(
            *n > 0,
            "{label}: no rasters scored — a (face, size) row went missing"
        );
        assert!(
            row.mean() <= ROW_MEAN_LIMIT,
            "{label}: row mean |coverage error| is {:.4}/255 over {n} rasters, past the \
             {ROW_MEAN_LIMIT}/255 per-row bound. The flattening in raster.rs \
             (FLATTEN_SAGITTA_PX) is the thing this guards; re-derive the budget, do not raise \
             this number.",
            row.mean()
        );
    }
    assert!(
        mean <= MEAN_LIMIT,
        "corpus mean |coverage error| is {mean:.4}/255 over {scored} glyph rasters, past the \
         {MEAN_LIMIT}/255 bound (worst glyph: {worst_case} at {worst_err:.2}/255). The \
         flattening in raster.rs (FLATTEN_SAGITTA_PX) is the thing this guards; re-derive the \
         budget, do not raise this number."
    );
}
