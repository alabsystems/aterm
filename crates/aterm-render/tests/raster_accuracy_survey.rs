// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! MEASUREMENT HARNESS for `docs/measured/fontdue-oracle-decision-2026-08-29.md`
//! — and, since that decision was acted on, THE GUARD it derived.
//!
//! [`flattening_accuracy_is_bounded`] is a live test: it holds the shipped
//! rasterizer to a pooled mean absolute coverage error of 0.100/255, a
//! per-(face, size) row mean of 0.130/255 and a per-glyph worst cell of 6/255
//! against ground truth, and to beating `fontdue` on every row. It is the only
//! guard on `raster::draw_quad`/`draw_cubic`, which no longer carry
//! `ab_glyph_rasterizer`'s flattening constants and are no longer covered by
//! `tests/rasterizer_oracle.rs` (that oracle now flattens once itself and pins
//! only the FILL — see its header). Read its doc comment for where each bound
//! comes from and for the three things it does NOT cover.
//!
//! The file used to carry thirteen `#[ignore]`d instruments beside it that
//! printed tables rather than asserting — how accurate is `aterm_render::raster`
//! (the first-party coverage fill that retired `ab_glyph_rasterizer`) against
//! GROUND TRUTH, next to `fontdue`? Nothing ever ran them, so they were retired
//! on 2026-09-24; their source is recoverable at `e8a8c80ab` and their readings
//! are recorded in the decision note above. What stays is the ground truth the
//! guard scores against.
//!
//! GROUND TRUTH is an independent scanline reference (`reference_coverage`):
//! exact analytic span coverage in x, midpoint-sampled at `SUB` sub-scanlines
//! per pixel row in y, over an outline flattened `REF_SEGMENTS_*` times finer
//! than any candidate. It shares no code with either candidate — no signed-area
//! accumulator, no incremental x march.
//!
//! Run (the headroom table prints with `--nocapture`):
//!   targo --unverified test -p aterm-render --test raster_accuracy_survey -- --nocapture

const DEJAVU: &[u8] = include_bytes!("../assets/DejaVuSansMono.ttf");
const NERD: &[u8] = include_bytes!("../assets/SymbolsNerdFontMono-Regular.ttf");

// ---------------------------------------------------------------------------
// Outline recording, in DESIGN units (y up).
// ---------------------------------------------------------------------------

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

impl ttf_parser::OutlineBuilder for Rec {
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

fn record(face: &ttf_parser::Face, gid: u16) -> Option<Rec> {
    let mut r = Rec::default();
    face.outline_glyph(ttf_parser::GlyphId(gid), &mut r)?;
    r.close_contour();
    Some(r)
}

// ---------------------------------------------------------------------------
// The reference: exact-in-x scanline coverage, y sampled at SUB per row.
// ---------------------------------------------------------------------------

/// Sub-scanlines per pixel row. 64 is "8x supersampled" squared in y; the x
/// axis is not sampled at all (spans are integrated exactly).
const SUB: usize = 64;
/// Uniform subdivisions used to flatten a quad/cubic FOR THE REFERENCE ONLY.
/// A quadratic's max deviation from its `n`-segment polyline is `|dev| / (4n^2)`
/// where `dev` is the second difference in px; at n = 256 and a 32 px glyph
/// (`|dev|` under ~12 px) that is under 5e-5 px. The retired
/// `reference_is_converged` measured the residual rather than trusting this
/// arithmetic (decision note §1).
const REF_SEGMENTS: usize = 256;

/// A flat edge list in GRID space (px, y DOWN, origin at the canvas top-left).
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

/// Two crossings this close in x are ONE crossing point, not two: the winding
/// number is a property of the open REGION between distinct crossings, and the
/// interval between two coincident crossings has no interior to have a winding
/// number in.
///
/// The tie is normally EXACT — a sub-scanline that lands on a local-minimum
/// vertex picks up both of that vertex's edges at `t = 0`, and `lo.x + 0.0 *
/// (hi.x - lo.x)` is `lo.x` to the bit for each — so this epsilon only absorbs
/// float dust. It is deliberately tiny: a genuine sliver `X_TIE_PX` wide
/// contributes under `4e-9` of one texel's coverage, so nothing measurable is
/// classified away by it.
const X_TIE_PX: f32 = 1e-6;

/// Nonzero-winding coverage of `edges` over a `w`x`h` grid, in 0..=1.
///
/// The second return value is whether the outline reaches a winding number
/// outside `{0, +1}` (or, for a reversed outline, outside `{0, -1}`) — i.e.
/// whether it SELF-OVERLAPS or mixes contour orientations. That flag matters: a
/// signed-area rasterizer (both candidates are one) accumulates area TIMES
/// winding, so in an overlap it reports more than 1 and the caller's clamp turns
/// a partially covered pixel into a fully covered one, while oppositely wound
/// ink in one pixel CANCELS where the nonzero rule fills. That is a shared,
/// structural difference from true nonzero-winding coverage, not a flattening
/// difference, so the survey keeps the two glyph classes apart.
///
/// # The flag is read off REGIONS, not off the sweep's intermediate states
///
/// This classifier used to advance the winding count one crossing at a time and
/// flag on every value it passed through. That flagged a SAMPLING ARTIFACT, not
/// geometry. When a sub-scanline lands exactly on a local-minimum vertex, both
/// of that vertex's edges cross it at the SAME x with directions `+1` and `-1`;
/// the sort is by x alone, so whichever ties first decides whether the sweep
/// transiently reads `-1` (flagging `saw_neg`) or `+2` (flagging `overlapped`)
/// on an interval of zero width. Whether that happens depends only on whether a
/// vertex's y lands on `row + (k + 0.5) / SUB` — a property of the sampling
/// phase.
///
/// Measured: `'M'`, `'N'`, `'W'` and `'X'` in DejaVuSansMono contain no curves
/// and cannot self-overlap, and were flagged at 16px and at no other size,
/// because at 16px (scale `1/128`) the integer design coordinates land exactly
/// on the odd-`128ths` the sub-scanlines sample. Nudging the sampling phase by
/// `1/4900` px — which changes no geometry at all — dropped the flagged count
/// from 561/2320 to 212/2320 and DejaVu's from 71 to 0.
///
/// So crossings are now GROUPED by x ([`X_TIE_PX`]) and the winding is read
/// only between distinct groups, which is the winding number of an actual
/// planar region. Coverage is unaffected either way: the old form deposited a
/// zero-width span at each tie, which sums to the same total.
///
/// `phase` displaces the sub-scanline ladder by that many px. `phase = 0` is
/// the shipped instrument; every other value is a DIFFERENT, equally valid
/// instrument sampling the same geometry (the retired
/// `reference_phase_sensitivity` read the instrument's own error bar off it).
fn reference_coverage_phased(
    edges: &[(P, P)],
    w: usize,
    h: usize,
    sub: usize,
    phase: f32,
) -> (Vec<f32>, bool) {
    let mut cov = vec![0.0f64; w * h];
    let mut overlapped = false;
    // GLYPH-GLOBAL, not per-scanline: a pixel whose top half holds positively
    // wound ink and whose bottom half holds negatively wound ink is CANCELLED
    // by a signed-area accumulator and FILLED by the nonzero rule, and no
    // single scanline sees both. Any outline that mixes orientations at all is
    // therefore outside the class where the two definitions agree. A wholly
    // REVERSED outline is not: `for_each_pixel` reports `|acc|`, so a uniform
    // -1 reads exactly as a uniform +1 would.
    let (mut saw_pos, mut saw_neg) = (false, false);
    let mut xs: Vec<(f32, i32)> = Vec::new();
    for row in 0..h {
        for k in 0..sub {
            let gy = row as f32 + (k as f32 + 0.5) / sub as f32 + phase;
            xs.clear();
            for &(p0, p1) in edges {
                // Half-open [y_low, y_high): a shared joint is counted once.
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
                // One crossing POINT, however many edges meet there.
                while i < xs.len() && xs[i].0 - x <= X_TIE_PX {
                    wind += xs[i].1;
                    i += 1;
                }
                // |winding| >= 2 is one contour stacked on another; a glyph that
                // has BOTH a positively and a negatively wound region is two
                // contours of OPPOSITE direction crossing (an ordinary hole
                // never goes negative — its regions are +1 and 0). A signed-area
                // accumulator gets both wrong, in opposite directions: it
                // over-reports the first and CANCELS the second.
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
    // `max`/`min` return the non-NaN operand, so neither bound is NaN by here
    // and a plain `<=` is the whole test — an empty or inverted span deposits
    // nothing.
    if xb <= xa {
        return;
    }
    let c0 = xa.floor() as usize;
    let c1 = ((xb.ceil() as usize).saturating_sub(1)).min(w - 1);
    // `c1` is already clamped to `w - 1`, so `take(c1 + 1).skip(c0)` walks
    // exactly the columns `c0..=c1` and hands each its own cell.
    for (c, cell) in row.iter_mut().enumerate().take(c1 + 1).skip(c0) {
        let l = xa.max(c as f32);
        let r = xb.min(c as f32 + 1.0);
        if r > l {
            *cell += f64::from(r - l);
        }
    }
}

// ---------------------------------------------------------------------------
// Canvas: one absolute pixel grid every candidate is placed into.
// ---------------------------------------------------------------------------

struct Canvas {
    x0: i32,
    ytop: i32,
    w: usize,
    h: usize,
}

/// Place a candidate mask (its own `xmin`/`ymin`/`w`/`h`, rows top-down) into
/// the canvas. Returns false if any of it falls outside — never silently drops.
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

// ---------------------------------------------------------------------------
// Candidate 2: the same signed-area fill, flattened FINER.
// ---------------------------------------------------------------------------

/// Rasterize with `aterm_render::raster`'s fill but the harness's own
/// flattening: every quad/cubic is split into `n` uniform pieces where `n` is
/// chosen so the polyline's max deviation from the curve is under `tol` px.
/// `tol = None` means "use the shipped `draw_quad`/`draw_cubic`", i.e. the
/// shipped rasterizer exactly.
/// The placement of one fill: design-to-pixel `scale`, the translation the
/// outline is mapped through, and the grid it lands in. Bundled because they
/// always travel together and eight loose parameters is past what reads.
#[derive(Clone, Copy)]
struct FillGrid {
    scale: f32,
    ox: f32,
    oy: f32,
    gw: usize,
    gh: usize,
}

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
                        *segments += 1; // counted separately below
                    }
                    Some(tol) => {
                        // Max deviation of the n-piece polyline from the quad
                        // is |dev| / (4 n^2), dev = second difference.
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

// ---------------------------------------------------------------------------
// One glyph, one px: every candidate against ground truth.
// ---------------------------------------------------------------------------

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
    reference: Vec<f32>, // 0..=255
    segs: Vec<Seg>,
    scale: f32,
    // The shipped variation.rs ink box, in absolute px.
    vx: i32,
    vy: i32,
    vw: usize,
    vh: usize,
    overlapped: bool,
}

fn build_case(face: &ttf_parser::Face, gid: u16, px: f32) -> Option<GlyphCase> {
    build_case_phased(face, gid, px, 0.0)
}

/// [`build_case`] with the reference's sub-scanline ladder displaced by `phase`
/// px. Only the REFERENCE moves; the candidate rasters are untouched, so the
/// error this changes is entirely the instrument's.
fn build_case_phased(face: &ttf_parser::Face, gid: u16, px: f32, phase: f32) -> Option<GlyphCase> {
    let upem = f32::from(face.units_per_em());
    let scale = px / upem;
    let rec = record(face, gid)?;
    if rec.segs.is_empty() {
        return None;
    }
    let bbox = face.glyph_bounding_box(ttf_parser::GlyphId(gid))?;
    let vx = (f32::from(bbox.x_min) * scale).floor() as i32;
    let vy = (f32::from(bbox.y_min) * scale).floor() as i32;
    let vw =
        ((f32::from(bbox.x_max) * scale).ceil() - (f32::from(bbox.x_min) * scale).floor()) as i32;
    let vh =
        ((f32::from(bbox.y_max) * scale).ceil() - (f32::from(bbox.y_min) * scale).floor()) as i32;
    if vw <= 0 || vh <= 0 || vw > 4096 || vh > 4096 {
        return None;
    }
    // Canvas: the declared box plus 3 px of margin on every side, which every
    // candidate's own box must fit inside (asserted at placement).
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

    /// The SHIPPED first-party path, or a finer-flattened twin of it.
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

// ---------------------------------------------------------------------------
// The corpus.
// ---------------------------------------------------------------------------

fn corpus_chars() -> Vec<char> {
    (0x21u32..0x7fu32).filter_map(char::from_u32).collect()
}

const PX: [f32; 8] = [8.0, 10.0, 12.0, 14.0, 16.0, 20.0, 24.0, 32.0];

// ---------------------------------------------------------------------------
// THE GUARD.
//
// Everything above builds the ground truth. This is the test that fails.
// ---------------------------------------------------------------------------

/// Ground truth is expensive — 64 sub-scanlines over a 256-fold reference
/// flattening, per glyph, ~120 ms per raster in an unoptimised build — so the
/// guard scores two corpora and picks by build profile. A plain `cargo test`
/// gets the SAMPLE (137 of 144 rasters, 7 dropped by the winding classifier,
/// 16 s); `cargo test --release` gets the whole thing (2,108 of 2,320 rasters,
/// 212 dropped, 17 s). Both are held to the same bounds.
///
/// **Quote the corpus size WITH its exclusion count, always.** "2,108 rasters"
/// on its own hides that 212 were classified out, and the classifier is the
/// part of this instrument most able to rot — see
/// [`reference_coverage_phased`], which used to drop 561 of the 2,320 because
/// it was reading its own sampling phase.
///
/// The sample is not a weaker test of the FLATTENING: the error this guards is
/// a per-curve property that every glyph with a curve in it carries, so it
/// shows up in 137 rasters as plainly as in 2,108 — measured, at the loosenings
/// this guard was watched failing at, the sample tripped on the FIRST offending
/// glyph. What the full corpus adds is worst-cell coverage, which is why the
/// max bound is the limb that benefits from a release run.
///
/// Returns `(px sweep, per-face character sampling stride, glyph-count floor)`.
const fn guard_corpus() -> (&'static [f32], usize, usize) {
    if cfg!(debug_assertions) {
        // 12 px is the desktop size the shipped path is tuned around; 32 px is
        // where a once-at-parse flattening degrades worst and so where a
        // per-px one has the most to prove.
        (&[12.0, 32.0], 4, 130)
    } else {
        (&PX, 1, 2_050)
    }
}

/// The characters the guard scores on one face, at sampling `stride`.
fn guard_chars(bytes: &'static [u8], name: &str, stride: usize) -> Vec<char> {
    let all: Vec<char> = if name == "SymbolsNerd" {
        // The Nerd face's coverage is in the PUA; walk its own cmap.
        let face = ttf_parser::Face::parse(bytes, 0).unwrap();
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

/// THE ACCURACY BOUND on `raster::draw_quad` / `raster::draw_cubic`.
///
/// This is the guard that replaced bit-equality for the FLATTENING when
/// `raster.rs` stopped carrying `ab_glyph_rasterizer`'s `devsq < 0.333` /
/// `tol = 3.0` / `FLATNESS = 0.35`. Those constants were the retired crate's
/// tuning, and pinning them pinned aterm 3.2x further from ground truth than
/// the `fontdue` it also retired. The fill keeps its bit-equality oracle
/// (`tests/rasterizer_oracle.rs`); this covers what the oracle gave up.
///
/// # Three limbs, because one mean cannot see what the other two can
///
/// * `MEAN_LIMIT` — the pooled corpus mean.
/// * `ROW_MEAN_LIMIT` — the mean of EACH (face, size) row on its own.
/// * `MAX_LIMIT` — the worst single cell of any one glyph.
///
/// plus a per-row and a pooled comparison against the `fontdue` this rasterizer
/// replaced, scored on the SAME glyphs, in the SAME run, against the SAME
/// reference.
///
/// The per-row limb exists because [`ErrStat::mean`] is TEXEL-pooled and the
/// corpus is not evenly spread: measured on the release corpus, the Nerd face
/// is **76.8 %** of all scored texels, and DejaVu at 12px and 16px — the face
/// and the sizes a terminal actually draws — are **1.9 %** and **2.5 %**. A
/// regression that doubled DejaVu 12px alone would move the pooled mean by
/// under 0.002/255, inside the printing precision, and the pooled limb would
/// never see it. Every row therefore carries its own bound.
///
/// The per-row `fontdue` comparison is also the only thing that guards the
/// claim the tightening was made for: not "better on average" but better at
/// EVERY (face, size).
///
/// # Where the numbers come from — measured, then given stated headroom
///
/// **The flattening's own budget** is `raster::FLATTEN_SAGITTA_PX`: the mask is
/// 8 bits, a polyline whose sagitta is `t` px displaces the true edge by at
/// most `t`, an edge crossing a cell spans at most 1 px of it, so flattening
/// contributes at most `t` of coverage error to any cell — and `t = 1/255` px
/// holds that under one output level.
///
/// **The bounds here pin that derived point.** Shipped readings on this box,
/// with the headroom each bound leaves:
///
/// ```text
///                          debug     release     bound   headroom
/// pooled mean            0.0644      0.0749     0.100    1.33x (release)
/// worst (face,size) row  0.0856      0.0912     0.130    1.43x
/// worst single cell        3.83        4.42       6.0    1.36x
/// ```
///
/// The bounds this guard SHIPPED WITH were 0.20 mean / 8.0 max and no row limb,
/// which a 5x loosening of the flattening walked straight through: at
/// `FLATTEN_SAGITTA_PX = 0.020 px` the corpus reads mean 0.172 (debug) / 0.190
/// (release) and worst cell 6.72, all of it inside the old bounds. Against the
/// bounds above, 0.020 px fails on the pooled mean (1.72x / 1.90x over), on
/// every row, on the worst cell, and on the per-row fontdue comparison — the
/// guard now pins the derived budget instead of merely permitting it.
///
/// **These numbers must be RE-DERIVED, not nudged, if the reference changes.**
/// A bound that moves to accommodate a result it was supposed to judge is not a
/// bound.
///
/// # What it does not cover — three limits, recorded rather than papered
///
/// **1. The worst-cell reading is INSTRUMENT-LIMITED and is a lower bound.**
/// `reference_is_converged` (retired; see the header) refined the reference by
/// powers of two, so every ladder it compared CONTAINS the coarser ladder's
/// sample positions and the comparison is structurally blind to the sampling
/// PHASE. Displace that ladder instead (`reference_phase_sensitivity`, also
/// retired) and the same rasterizer scores
/// differently: at a phase move of 1/303 px the corpus mean goes 0.0749 ->
/// 0.0970 (+0.0221/255), one reference cell moves by up to **7.97/255** and one
/// scored cell's error by up to **3.98/255** — against a shipped worst cell of
/// 4.42. So 4.42/255 is a LOWER BOUND on the worst-cell figure, not a
/// measurement of it, and no worst-cell RATIO against fontdue is resolvable by
/// this instrument. The pooled mean survives the same test with room to spare
/// (the 0.862 -> 0.075 tightening is ~36x the phase spread); the worst cell does
/// not.
///
/// **2. The HINTED path is unmeasured.** `hinted.rs:143` returns
/// `HintMode::Full` when `ATERM_FONT_HINTING` is unset, so ordinary body text on
/// linux and windows is rasterized from GRID-FITTED outlines that this survey
/// never builds — it drives `variation.rs`'s unhinted path only. The
/// `RASTER_PAD` grid-escape class lives entirely in that gap (see §6/§8 of the
/// decision note); `raster.rs`'s own `#[cfg(test)] mod tests` drives the escape
/// paths directly, and extending an accuracy guard over the hinted path is
/// still open.
///
/// **3. "Better than fontdue" is an AGGREGATE claim, not a per-glyph one.**
/// `per_glyph_vs_fontdue` measured it: on 52 of the 2,108 scored rasters
/// (2.5 % — DejaVu 3.7 %, Nerd 1.8 %) the first-party mean is WORSE than
/// fontdue's, concentrated in rectilinear glyphs where there is little curve for
/// a finer flattening to improve (`.` `E` `I` `M` `T` `Z` `[` `|` `#` `+`).
/// Every (face, size) row wins, and this test asserts that; no per-glyph
/// always-wins claim should be made from it.
///
/// **4. The SHIPPED entry point is guarded by neither instrument.** This test
/// builds its own design->grid mapping and its own crop, and
/// `tests/rasterizer_oracle.rs` flattens and maps the outline itself and reads
/// f32 out of `for_each_pixel` upstream of every quantisation. So a defect
/// living in `variation.rs` between them — a rounding-mode change in
/// `crop_padded_coverage`, a sub-LSB shift in `OutlineToRaster::map` — is
/// outside both. Reproduced 2026-08-30: both injections leave the entire
/// `cargo test -p aterm-render` suite green, oracle included. See the
/// CORRECTION in §6 of the decision note; the gap is open.
///
/// # Why self-overlapping glyphs are excluded
///
/// In an overlap a signed-area rasterizer accumulates area TIMES winding and
/// the caller clamps — a structural difference from nonzero-winding coverage
/// that `fontdue` shares identically (measured at up to 184.5/255 for both).
/// Scoring them would measure that, not the flattening. The classifier is
/// [`reference_coverage_phased`], and its own failure mode is documented
/// there.
#[test]
fn flattening_accuracy_is_bounded() {
    /// Corpus mean |coverage error|, in 0..255 units, pooled over every raster.
    const MEAN_LIMIT: f64 = 0.100;
    /// Mean |coverage error| of ONE (face, size) row, in 0..255 units.
    const ROW_MEAN_LIMIT: f64 = 0.130;
    /// Worst single cell of any one glyph, in 0..255 units.
    const MAX_LIMIT: f32 = 6.0;
    let (pxs, stride, floor) = guard_corpus();
    let mut total = ErrStat::default();
    // `fontdue` scored on the SAME glyphs, in the SAME run, against the SAME
    // reference. A hard-coded 0.252/255 would be weaker than `MEAN_LIMIT` and
    // therefore dead: this limb is only a real assertion if the crate this
    // rasterizer replaced is measured here rather than quoted. It is also the
    // one limb that survives a change to the instrument — if the reference
    // moves, both means move with it and the comparison still means what it
    // says.
    let mut fd_total = ErrStat::default();
    let mut worst: Option<(String, f32)> = None;
    let mut scored = 0usize;
    let mut excluded = 0usize;
    // One row PER (face, size). See the doc comment: the pooled mean cannot see
    // a regression at the sizes a terminal actually draws.
    let mut rows: Vec<(String, usize, ErrStat, ErrStat)> = Vec::new();
    for (bytes, name) in [(DEJAVU, "DejaVuSansMono"), (NERD, "SymbolsNerd")] {
        let face = ttf_parser::Face::parse(bytes, 0).unwrap();
        let fd = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).unwrap();
        let chars = guard_chars(bytes, name, stride);
        for &px in pxs {
            let mut row = ErrStat::default();
            let mut fd_row = ErrStat::default();
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
                let (fm, fb) = fd.rasterize_indexed(gid.0, px);
                assert!(
                    case.score(&fb, fm.width, fm.height, fm.xmin, fm.ymin, &mut fd_row),
                    "{name} {ch:?} @ {px}px: the fontdue mask escaped the canvas"
                );
                scored += 1;
                row_rasters += 1;
            }
            total.merge(&row);
            fd_total.merge(&fd_row);
            rows.push((format!("{name} {px:>4.0}px"), row_rasters, row, fd_row));
        }
    }
    // A corpus that quietly went missing would pass every bound above.
    assert!(
        scored >= floor,
        "only {scored} glyph rasters scored, under the {floor} this corpus should reach — \
         the corpus went missing"
    );
    let mean = total.mean();
    let (worst_case, worst_err) = worst.expect("a scored corpus has a worst glyph");
    // Printed, not only asserted: the decision this guard came from requires the
    // headroom to be visible rather than assumed, so `--nocapture` shows how far
    // the shipped path actually is from every bound, per row and pooled.
    eprintln!(
        "\n{:<20} {:>8} {:>9} {:>9} {:>9} {:>9}",
        "row", "rasters", "mean", "headroom", "fontdue", "mine/fd"
    );
    for (label, n, row, fd_row) in &rows {
        eprintln!(
            "{label:<20} {n:>8} {:>9.4} {:>8.2}x {:>9.4} {:>8.2}x",
            row.mean(),
            ROW_MEAN_LIMIT / row.mean().max(f64::MIN_POSITIVE),
            fd_row.mean(),
            fd_row.mean() / row.mean().max(f64::MIN_POSITIVE),
        );
    }
    let fd_mean = fd_total.mean();
    eprintln!(
        "flattening accuracy: mean {mean:.4}/255 (bound {MEAN_LIMIT}, headroom {:.2}x), worst \
         cell {worst_err:.2}/255 at {worst_case} (bound {MAX_LIMIT}, headroom {:.2}x), fontdue \
         {fd_mean:.4}/255 on the same {scored} rasters ({excluded} excluded by the winding \
         classifier)",
        MEAN_LIMIT / mean.max(f64::MIN_POSITIVE),
        f64::from(MAX_LIMIT) / f64::from(worst_err).max(f64::MIN_POSITIVE),
    );
    // PER-ROW, and only then pooled. The pooled mean is texel-weighted and the
    // corpus is 76.8% Nerd-face, so DejaVu 12px and 16px — the face and the
    // sizes a terminal actually draws — are 1.9% and 2.5% of it and a full
    // regression there moves the pooled figure by less than its own printing
    // precision. Every row therefore carries its own bound.
    for (label, n, row, fd_row) in &rows {
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
        assert!(
            row.mean() <= fd_row.mean(),
            "{label}: row mean is {:.4}/255, WORSE than the fontdue this rasterizer replaced \
             ({:.4}/255 on the same rasters). Beating fontdue at EVERY (face, size) — not only \
             pooled — is the claim the flattening budget was derived to support.",
            row.mean(),
            fd_row.mean()
        );
    }
    assert!(
        mean <= MEAN_LIMIT,
        "corpus mean |coverage error| is {mean:.4}/255 over {scored} glyph rasters, past the \
         {MEAN_LIMIT}/255 bound (worst glyph: {worst_case} at {worst_err:.2}/255). The \
         flattening in raster.rs (FLATTEN_SAGITTA_PX) is the thing this guards; re-derive the \
         budget, do not raise this number."
    );
    assert!(
        mean <= fd_mean,
        "corpus mean is {mean:.4}/255, WORSE than the fontdue this rasterizer replaced \
         ({fd_mean:.4}/255 on the same {scored} rasters) — passing it is the whole point of \
         the flattening budget raster.rs derives"
    );
}

// ---------------------------------------------------------------------------
// The COST side of the budget, on the path that actually pays it.
// ---------------------------------------------------------------------------
