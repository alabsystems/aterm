// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! DIFFERENTIAL ORACLE for [`aterm_render::raster`], the first-party coverage
//! rasterizer that retired `ab_glyph_rasterizer` from the shipped graph.
//!
//! The retired crate is kept as a `[dev-dependencies]` entry for exactly this
//! file. A dev-dependency never enters the shipped dependency graph, so the
//! evidence is free — and it is the only kind of evidence that is worth
//! anything here, because antialiased glyph coverage is where an off-by-one is
//! invisible to a "the glyph is not blank" assertion and glaring on screen.
//!
//! # What this oracle covers, and what it deliberately does not
//!
//! It covers the FILL: `draw_line` and everything downstream of it — the
//! two-cell fast path, the `s = 1/(x1 - x0)` trapezoid split, the interior
//! slabs, the incremental `x += dxdy * dy` march, the accumulator's cross-row
//! carry. That arithmetic computes exact analytic per-cell area, where two
//! independent implementations agree to the bit because there is only one right
//! answer, and it is also the only expression of cross-machine determinism this
//! crate has (the GPU/CPU parity suites lean on it).
//!
//! It does NOT cover the FLATTENING. `raster.rs` used to carry
//! `ab_glyph_rasterizer`'s `devsq < 0.333` / `tol = 3.0` / `FLATNESS = 0.35`
//! verbatim, and this file pinned them — but those were the retired crate's
//! tuning constants, not a correctness property, and they held aterm's
//! rasterizer 3.2× further from ground truth than the `fontdue` it also
//! retired. So every case below **flattens ONCE, here in the harness**
//! ([`flatten`]) and feeds both rasterizers the identical polyline through
//! `draw_line` only. The flattening is guarded instead by measured accuracy
//! against an independent analytic reference, in
//! `tests/raster_accuracy_survey.rs`. The full argument for the split is
//! `docs/measured/fontdue-oracle-decision-2026-08-29.md`.
//!
//! Every case below feeds the SAME polyline, vertex for vertex, into both
//! rasterizers and compares the full coverage grid at f32 bit-equality — not
//! "close enough", not the quantised mask, the exact bits. The corpus is:
//!
//!   1. real glyph outlines from the two embedded faces, pulled through the
//!      real ttf-parser outline path at a spread of ppem values (quadratics,
//!      thousands of contours, including the grid-fitting-adjacent geometry
//!      that broke `'2'` at ppem 19);
//!   2. synthetic cubics, which the embedded TrueType faces cannot exercise
//!      because CFF is where cubics live;
//!   3. random line/quad/cubic soup from a fixed PRNG, to reach the
//!      degenerate shapes real fonts are too well-behaved to produce.

use aterm_render::raster;

// ---------------------------------------------------------------------------
// Shared outline plumbing: one recording, replayed into both rasterizers.
// ---------------------------------------------------------------------------

/// One recorded outline command in grid space (y already flipped, y DOWN).
#[derive(Clone, Copy, Debug)]
enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    Quad(f32, f32, f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Close,
}

/// Records a ttf-parser outline in design units; the caller maps to grid space.
#[derive(Default)]
struct Recorder {
    segs: Vec<Seg>,
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
    seen: bool,
}

impl Recorder {
    fn see(&mut self, x: f32, y: f32) {
        if self.seen {
            self.min_x = self.min_x.min(x);
            self.min_y = self.min_y.min(y);
            self.max_x = self.max_x.max(x);
            self.max_y = self.max_y.max(y);
        } else {
            self.seen = true;
            self.min_x = x;
            self.max_x = x;
            self.min_y = y;
            self.max_y = y;
        }
    }
}

impl ttf_parser::OutlineBuilder for Recorder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.see(x, y);
        self.segs.push(Seg::Move(x, y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.see(x, y);
        self.segs.push(Seg::Line(x, y));
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.see(cx, cy);
        self.see(x, y);
        self.segs.push(Seg::Quad(cx, cy, x, y));
    }
    fn curve_to(&mut self, c0x: f32, c0y: f32, c1x: f32, c1y: f32, x: f32, y: f32) {
        self.see(c0x, c0y);
        self.see(c1x, c1y);
        self.see(x, y);
        self.segs.push(Seg::Cubic(c0x, c0y, c1x, c1y, x, y));
    }
    fn close(&mut self) {
        self.segs.push(Seg::Close);
    }
}

/// Affine map from recorded space into the grid, matching `variation.rs`:
/// scale, translate by the ink box's left edge, flip y about its top edge.
#[derive(Clone, Copy)]
struct Map {
    scale: f32,
    ox: f32,
    oy: f32,
}

impl Map {
    fn at(&self, x: f32, y: f32) -> (f32, f32) {
        (x * self.scale - self.ox, self.oy - y * self.scale)
    }
}

/// The sagitta, in grid cells, this harness flattens curves to.
///
/// A deliberate COPY of `raster.rs`'s own `FLATTEN_SAGITTA_PX` rather than a
/// reference to it: the value only sets how dense the shared polyline is, and
/// matching production density is what keeps the fill exercised over the
/// segment lengths it actually sees — but a change to the product's budget must
/// not silently change what this oracle exercises, so the number is written
/// out here and moves only when someone decides it should.
const HARNESS_SAGITTA_PX: f32 = 1.0 / 255.0;

/// One flattened outline command, in GRID space (y already flipped, y DOWN):
/// the shared polyline both rasterizers are fed.
#[derive(Clone, Copy, Debug)]
enum Flat {
    Move(f32, f32),
    Line(f32, f32),
    Close,
}

/// Map `segs` into grid space and flatten every curve to [`HARNESS_SAGITTA_PX`]
/// — the ONE flattening in this file, so neither rasterizer's own curve code is
/// under test here and both see byte-identical vertices.
///
/// Uniform parameter splitting, sized from the second difference: a quadratic's
/// `n`-segment polyline departs from the curve by at most `|dev| / (4n²)` and a
/// cubic's by at most `3·max(|d1|, |d2|) / (4n²)`. Written out independently of
/// `raster.rs` (which derives the same counts) so the two cannot share a bug.
fn flatten(segs: &[Seg], m: Map) -> Vec<Flat> {
    let t = HARNESS_SAGITTA_PX;
    let mut out = Vec::with_capacity(segs.len() * 8);
    // The pen, in GRID space — curves are subdivided from where they start —
    // and the contour start, because `Close` returns the pen to it. No corpus
    // in this file opens a curve straight after a `Close` without a `Move`
    // first, but the fills downstream both do return the pen, so the flattener
    // has to as well or it would be modelling a different outline than they do.
    let (mut cx, mut cy) = (0.0f32, 0.0f32);
    let (mut sx, mut sy) = (0.0f32, 0.0f32);
    let lerp =
        |t: f32, a: (f32, f32), b: (f32, f32)| (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1));
    let norm = |dx: f32, dy: f32| (dx * dx + dy * dy).sqrt();
    for seg in segs {
        match *seg {
            Seg::Move(x, y) => {
                let p = m.at(x, y);
                out.push(Flat::Move(p.0, p.1));
                (cx, cy) = p;
                (sx, sy) = p;
            }
            Seg::Line(x, y) => {
                let p = m.at(x, y);
                out.push(Flat::Line(p.0, p.1));
                (cx, cy) = p;
            }
            Seg::Quad(qx, qy, x, y) => {
                let p0 = (cx, cy);
                let c = m.at(qx, qy);
                let p2 = m.at(x, y);
                let dev = norm(p0.0 - 2.0 * c.0 + p2.0, p0.1 - 2.0 * c.1 + p2.1);
                let n = ((dev / (4.0 * t)).sqrt().ceil() as usize).clamp(1, 4096);
                for i in 1..=n {
                    let u = i as f32 / n as f32;
                    let p = lerp(u, lerp(u, p0, c), lerp(u, c, p2));
                    out.push(Flat::Line(p.0, p.1));
                }
                (cx, cy) = p2;
            }
            Seg::Cubic(ax, ay, bx, by, x, y) => {
                let p0 = (cx, cy);
                let c0 = m.at(ax, ay);
                let c1 = m.at(bx, by);
                let p3 = m.at(x, y);
                let d1 = norm(p0.0 - 2.0 * c0.0 + c1.0, p0.1 - 2.0 * c0.1 + c1.1);
                let d2 = norm(c0.0 - 2.0 * c1.0 + p3.0, c0.1 - 2.0 * c1.1 + p3.1);
                let dev = d1.max(d2);
                let n = ((3.0 * dev / (4.0 * t)).sqrt().ceil() as usize).clamp(1, 4096);
                for i in 1..=n {
                    let u = i as f32 / n as f32;
                    let p = lerp(
                        u,
                        lerp(u, lerp(u, p0, c0), lerp(u, c0, c1)),
                        lerp(u, lerp(u, c0, c1), lerp(u, c1, p3)),
                    );
                    out.push(Flat::Line(p.0, p.1));
                }
                (cx, cy) = p3;
            }
            Seg::Close => {
                out.push(Flat::Close);
                (cx, cy) = (sx, sy);
            }
        }
    }
    out
}

/// Replay the shared polyline into the FIRST-PARTY rasterizer, closing contours
/// implicitly the way every caller in the crate does. `draw_line` only.
fn fill_mine(flat: &[Flat], w: usize, h: usize) -> Vec<f32> {
    let mut ras = raster::Rasterizer::new(w, h);
    let mut last = raster::point(0.0, 0.0);
    let mut start = last;
    for cmd in flat {
        match *cmd {
            Flat::Move(x, y) => {
                if last != start {
                    ras.draw_line(last, start);
                }
                last = raster::point(x, y);
                start = last;
            }
            Flat::Line(x, y) => {
                let q = raster::point(x, y);
                ras.draw_line(last, q);
                last = q;
            }
            Flat::Close => {
                if last != start {
                    ras.draw_line(last, start);
                    last = start;
                }
            }
        }
    }
    if last != start {
        ras.draw_line(last, start);
    }
    let mut out = vec![0.0f32; w * h];
    ras.for_each_pixel(|i, a| out[i] = a);
    out
}

/// Replay the same polyline into the RETIRED crate. Deliberately a separate
/// function rather than a generic one: the two must not be able to share a bug.
fn fill_oracle(flat: &[Flat], w: usize, h: usize) -> Vec<f32> {
    use ab_glyph_rasterizer as ab;
    let mut ras = ab::Rasterizer::new(w, h);
    let mut last = ab::point(0.0, 0.0);
    let mut start = last;
    for cmd in flat {
        match *cmd {
            Flat::Move(x, y) => {
                if last != start {
                    ras.draw_line(last, start);
                }
                last = ab::point(x, y);
                start = last;
            }
            Flat::Line(x, y) => {
                let q = ab::point(x, y);
                ras.draw_line(last, q);
                last = q;
            }
            Flat::Close => {
                if last != start {
                    ras.draw_line(last, start);
                    last = start;
                }
            }
        }
    }
    if last != start {
        ras.draw_line(last, start);
    }
    let mut out = vec![0.0f32; w * h];
    ras.for_each_pixel(|i, a| out[i] = a);
    out
}

/// Compare two coverage grids at f32 BIT equality and report the first
/// disagreement with enough context to reproduce it.
fn assert_identical(mine: &[f32], theirs: &[f32], w: usize, ctx: &str) {
    assert_eq!(mine.len(), theirs.len(), "{ctx}: grid size");
    for (i, (a, b)) in mine.iter().zip(theirs).enumerate() {
        assert!(
            a.to_bits() == b.to_bits(),
            "{ctx}: coverage differs at cell {i} (x={}, y={}): first-party {a} ({:#010x}) \
             vs oracle {b} ({:#010x}); as u8 {} vs {}",
            i % w,
            i / w,
            a.to_bits(),
            b.to_bits(),
            (a * 255.0 + 0.5).clamp(0.0, 255.0) as u8,
            (b * 255.0 + 0.5).clamp(0.0, 255.0) as u8,
        );
    }
}

// ---------------------------------------------------------------------------
// 1. Real glyph outlines through the real font path.
// ---------------------------------------------------------------------------

/// Rasterize one glyph of `face` at `px` through both implementations, using
/// exactly the ink box + `RASTER_PAD` framing `variation.rs` uses. Returns
/// `false` when the glyph has no ink at this size (nothing to compare).
fn compare_glyph(face: &ttf_parser::Face<'_>, gid: u16, px: f32) -> bool {
    let upem = f32::from(face.units_per_em());
    let scale = px / upem;
    let mut rec = Recorder::default();
    if face
        .outline_glyph(ttf_parser::GlyphId(gid), &mut rec)
        .is_none()
        || !rec.seen
    {
        return false;
    }
    let x_min = (rec.min_x * scale).floor();
    let x_max = (rec.max_x * scale).ceil();
    let y_min = (-rec.max_y * scale).floor();
    let y_max = (-rec.min_y * scale).ceil();
    let (w, h) = ((x_max - x_min) as i32, (y_max - y_min) as i32);
    if w <= 0 || h <= 0 || w > 4096 || h > 4096 {
        return false;
    }
    // The one-cell slack every fill in the crate uses, so nothing sits on the
    // grid boundary — see `variation::RASTER_PAD`.
    const PAD: usize = 1;
    let (gw, gh) = (w as usize + 2 * PAD, h as usize + 2 * PAD);
    let m = Map {
        scale,
        ox: x_min - PAD as f32,
        oy: -y_min + PAD as f32,
    };
    let flat = flatten(&rec.segs, m);
    let mine = fill_mine(&flat, gw, gh);
    let theirs = fill_oracle(&flat, gw, gh);
    assert_identical(&mine, &theirs, gw, &format!("gid {gid} @ {px}px"));
    true
}

/// Sweep a face's glyphs across the ppem range aterm actually renders at.
fn sweep_face(bytes: &'static [u8], label: &str, glyph_stride: u16) {
    let face = ttf_parser::Face::parse(bytes, 0).expect("embedded face parses");
    let n = face.number_of_glyphs();
    let mut compared = 0usize;
    // 19 is the ppem the RASTER_PAD note names as the one that detonated on the
    // default face; 6 and 64 bracket the range the renderer clamps to.
    for px in [6.0f32, 9.0, 11.0, 12.0, 13.0, 17.0, 19.0, 24.0, 33.0, 64.0] {
        let mut gid = 0u16;
        while gid < n {
            if compare_glyph(&face, gid, px) {
                compared += 1;
            }
            gid = gid.saturating_add(glyph_stride);
            if gid == 0 {
                break;
            }
        }
    }
    assert!(
        compared > 10_000,
        "{label}: only {compared} glyph rasters compared — the corpus went missing"
    );
}

#[test]
fn embedded_text_face_glyphs_match_the_oracle() {
    sweep_face(aterm_render::embedded_font(), "DejaVu Sans Mono", 1);
}

#[test]
fn embedded_symbol_face_glyphs_match_the_oracle() {
    // ~2.5MB / thousands of glyphs: stride so the sweep stays a test, not a
    // build step, while still covering every region of the face.
    sweep_face(
        aterm_render::embedded_symbols_font(),
        "Symbols Nerd Font",
        7,
    );
}

// ---------------------------------------------------------------------------
// 2 + 3. Synthetic corpora — cubics, and degenerate soup.
// ---------------------------------------------------------------------------

/// SplitMix64, so the corpus is identical on every machine and every run.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// A float in `[lo, hi)`.
    fn f32_in(&mut self, lo: f32, hi: f32) -> f32 {
        let u = (self.next_u64() >> 40) as f32 / (1u32 << 24) as f32;
        lo + u * (hi - lo)
    }
}

/// The FOUR outlines `raster.rs` pins as in-tree golden grids, fed through the
/// RETIRED rasterizer.
///
/// The module's own `#[cfg(test)] mod tests` compares its output against
/// hard-coded numbers, so that evidence survives the day this dev-dependency
/// goes away. This test is what makes those numbers trustworthy in the first
/// place — with one boundary, since the flattening left this oracle:
///
/// * the SQUARE and the TRIANGLE contain no curves, so [`flatten`] is the
///   identity on them and this still proves those two pinned grids are the
///   retired crate's own answer, cell for cell. If someone edits the fill in
///   `raster.rs` and re-records those goldens from it, this fails;
/// * the QUAD and the CUBIC arrive here pre-flattened, so what is proved is the
///   FILL over that polyline. `raster.rs` pins those two grids over its OWN
///   flattening, which is a strictly larger computation than this test covers,
///   and `tests/raster_accuracy_survey.rs` is what bounds the extra part.
#[test]
fn in_module_golden_shapes_match_the_oracle() {
    let square = vec![
        Seg::Move(1.0, -1.0),
        Seg::Line(3.0, -1.0),
        Seg::Line(3.0, -3.0),
        Seg::Line(1.0, -3.0),
        Seg::Close,
    ];
    let triangle = vec![
        Seg::Move(0.5, -0.25),
        Seg::Line(3.25, -1.75),
        Seg::Line(1.0, -3.5),
        Seg::Close,
    ];
    let quad = vec![
        Seg::Move(0.5, -0.5),
        Seg::Quad(5.5, -2.0, 0.5, -5.5),
        Seg::Line(0.5, -0.5),
        Seg::Close,
    ];
    let cubic = vec![
        Seg::Move(0.5, -0.5),
        Seg::Cubic(5.5, -1.0, 5.5, -5.0, 0.5, -5.5),
        Seg::Line(0.5, -0.5),
        Seg::Close,
    ];
    // Identity map — the recorded coordinates ARE grid coordinates, with y
    // pre-negated so `oy - y` at oy = 0 lands them the right way up.
    let m = Map {
        scale: 1.0,
        ox: 0.0,
        oy: 0.0,
    };
    for (what, segs, w, h) in [
        ("integer-aligned square", square, 5usize, 5usize),
        ("fractional triangle", triangle, 4, 4),
        ("quadratic", quad, 6, 6),
        ("cubic", cubic, 6, 6),
    ] {
        let flat = flatten(&segs, m);
        let mine = fill_mine(&flat, w, h);
        let theirs = fill_oracle(&flat, w, h);
        assert_identical(&mine, &theirs, w, &format!("golden shape: {what}"));
    }
}

/// Random closed contours of a given segment kind, compared grid-for-grid.
fn soup(seed: u64, kinds: &[u8], w: usize, h: usize, rounds: usize) {
    let mut rng = Rng(seed);
    for round in 0..rounds {
        let mut segs = Vec::new();
        let contours = 1 + (rng.next_u64() % 3) as usize;
        for _ in 0..contours {
            // Stay strictly inside the grid: the padded callers guarantee it,
            // and outside it the two implementations are ALLOWED to differ.
            // The difference is not "one of them is sloppy out there": the
            // retired crate's write macro `continue`s the SCANLINE on an
            // out-of-range index, abandoning the rest of that row AND the
            // `x = xnext` march step, so an escape corrupts later rows that are
            // still inside the grid; the first-party one drops the single write
            // and marches on. Both are documented in raster.rs, and its own
            // `#[cfg(test)] mod tests` drives every escape path directly —
            // asserting termination and bounds, which is the property that
            // survives the disagreement.
            let (lo_x, hi_x) = (0.5, w as f32 - 0.5);
            let (lo_y, hi_y) = (0.5, h as f32 - 0.5);
            let rnd_pt = |r: &mut Rng| (r.f32_in(lo_x, hi_x), r.f32_in(lo_y, hi_y));
            let (sx, sy) = rnd_pt(&mut rng);
            segs.push(Seg::Move(sx, sy));
            let n = 2 + (rng.next_u64() % 6) as usize;
            for _ in 0..n {
                let kind = kinds[(rng.next_u64() as usize) % kinds.len()];
                match kind {
                    0 => {
                        let (x, y) = rnd_pt(&mut rng);
                        segs.push(Seg::Line(x, y));
                    }
                    1 => {
                        let (cx, cy) = rnd_pt(&mut rng);
                        let (x, y) = rnd_pt(&mut rng);
                        segs.push(Seg::Quad(cx, cy, x, y));
                    }
                    _ => {
                        let (ax, ay) = rnd_pt(&mut rng);
                        let (bx, by) = rnd_pt(&mut rng);
                        let (x, y) = rnd_pt(&mut rng);
                        segs.push(Seg::Cubic(ax, ay, bx, by, x, y));
                    }
                }
            }
            segs.push(Seg::Close);
        }
        // Identity map: the recorded coordinates ARE grid coordinates here.
        let m = Map {
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
        };
        // `oy - y` with oy = 0 flips the contour above the grid, so feed y
        // straight through instead by pre-negating.
        let segs: Vec<Seg> = segs
            .iter()
            .map(|s| match *s {
                Seg::Move(x, y) => Seg::Move(x, -y),
                Seg::Line(x, y) => Seg::Line(x, -y),
                Seg::Quad(a, b, x, y) => Seg::Quad(a, -b, x, -y),
                Seg::Cubic(a, b, c, d, x, y) => Seg::Cubic(a, -b, c, -d, x, -y),
                Seg::Close => Seg::Close,
            })
            .collect();
        let flat = flatten(&segs, m);
        let mine = fill_mine(&flat, w, h);
        let theirs = fill_oracle(&flat, w, h);
        assert_identical(
            &mine,
            &theirs,
            w,
            &format!("soup seed {seed} round {round}"),
        );
    }
}

#[test]
fn random_line_contours_match_the_oracle() {
    soup(0x51ED_0001, &[0], 23, 31, 400);
}

#[test]
fn random_quadratic_contours_match_the_oracle() {
    soup(0x51ED_0002, &[1], 29, 19, 400);
}

/// Cubics are the CFF outline form; the embedded faces are TrueType, so this is
/// the only corpus whose polylines come from cubic geometry — long runs of very
/// short, sharply turning segments, which is where the fill's per-scanline
/// bookkeeping is least forgiving.
#[test]
fn random_cubic_contours_match_the_oracle() {
    soup(0x51ED_0003, &[2], 37, 41, 400);
}

#[test]
fn mixed_contours_match_the_oracle() {
    soup(0x51ED_0004, &[0, 1, 2], 64, 64, 300);
}

/// Tiny grids, where a single scanline's span covers most of the row and the
/// two-cell fast path and the trapezoid path trade places constantly.
#[test]
fn tiny_grids_match_the_oracle() {
    soup(0x51ED_0005, &[0, 1, 2], 3, 3, 500);
    soup(0x51ED_0006, &[0, 1, 2], 1, 8, 500);
    soup(0x51ED_0007, &[0, 1, 2], 8, 1, 500);
}

/// Axis-aligned and exactly-on-cell-boundary geometry: the grid-fitted case
/// that made `RASTER_PAD` necessary, driven deliberately rather than by luck.
#[test]
fn integer_aligned_geometry_matches_the_oracle() {
    let (w, h) = (12usize, 12usize);
    for x0 in 1..=6u32 {
        for x1 in 7..=11u32 {
            for y0 in 1..=6u32 {
                for y1 in 7..=11u32 {
                    let segs = vec![
                        Seg::Move(x0 as f32, -(y0 as f32)),
                        Seg::Line(x1 as f32, -(y0 as f32)),
                        Seg::Line(x1 as f32, -(y1 as f32)),
                        Seg::Line(x0 as f32, -(y1 as f32)),
                        Seg::Close,
                    ];
                    let m = Map {
                        scale: 1.0,
                        ox: 0.0,
                        oy: 0.0,
                    };
                    let flat = flatten(&segs, m);
                    let mine = fill_mine(&flat, w, h);
                    let theirs = fill_oracle(&flat, w, h);
                    assert_identical(&mine, &theirs, w, &format!("rect {x0},{y0}..{x1},{y1}"));
                }
            }
        }
    }
}

/// Sub-cell slivers: spans narrower than one cell in x, y, or both — the
/// arithmetic where `s = 1/(x1 - x0)` is largest and least forgiving.
#[test]
fn subcell_slivers_match_the_oracle() {
    let (w, h) = (6usize, 6usize);
    let mut rng = Rng(0x51ED_0008);
    for round in 0..2000 {
        let x = rng.f32_in(0.6, 4.4);
        let y = rng.f32_in(0.6, 4.4);
        let dx = rng.f32_in(0.001, 0.9);
        let dy = rng.f32_in(0.001, 0.9);
        let segs = vec![
            Seg::Move(x, -y),
            Seg::Line(x + dx, -y),
            Seg::Line(x + dx, -(y + dy)),
            Seg::Line(x, -(y + dy)),
            Seg::Close,
        ];
        let m = Map {
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
        };
        let flat = flatten(&segs, m);
        let mine = fill_mine(&flat, w, h);
        let theirs = fill_oracle(&flat, w, h);
        assert_identical(&mine, &theirs, w, &format!("sliver {round}"));
    }
}
