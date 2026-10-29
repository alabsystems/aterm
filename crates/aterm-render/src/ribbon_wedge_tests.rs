// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The wedge emitter ([`super::ribbon_wedge_into`], `docs/design/PRISM-GLASS.md`
//! §2.2 and §15.2 1.6): a slab it lays as a slab is main's emission byte for
//! byte, over a seeded corpus folded to the hash main laid at the wedge's
//! parent; a wedge's rows tile its slab inside its edges, its clip and its
//! bands; a glass tail's oldest end climbs by at most a pixel a column at
//! one, two and three slabs a cell, where the slab emission steps; the dither
//! stays the screen's; a zero-opacity end carries no colour; a sub-pixel move
//! slides the wedge; and a saturated budget sheds it as the slab emission is
//! shed, head first.

use super::{
    BeamClip, BeamRows, GlowBlend, GlowQuad, RampCost, RampSink, RibbonVertex, WEDGE_EDGE_PX,
    glass_profile, glow_lerp, glow_lerp_rgb, ribbon_beam_into, ribbon_glass_into,
    ribbon_wedge_into, smoothstep01,
};
use crate::test_counters::{take_glass_slabs, take_wedge_slabs};
use aterm_core::render::GlowRamps;
use std::collections::BTreeMap;

/// The edge step the light glass and lip pass ([`ribbon_wedge_into`]'s
/// `edge_px`, PRISM-GLASS §15.2 1.7's [`WEDGE_EDGE_PX`]).
const EDGE: f32 = WEDGE_EDGE_PX;

/// One call of a ribbon emitter: its clip, polyline and settings.
struct Case {
    clip: BeamClip,
    verts: Vec<RibbonVertex>,
    shoulder: f32,
    step: usize,
    max_quads: usize,
    blend: GlowBlend,
}

/// A seeded xorshift stream.
struct Seed(u64);

impl Seed {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A value in `0..m`.
    fn below(&mut self, m: u64) -> u64 {
        self.next() % m
    }

    /// A value in `lo..hi` on a grid of quarter pixels, plus a non-dyadic
    /// fraction one time in three.
    fn px(&mut self, lo: f32, hi: f32) -> f32 {
        let quarters = ((hi - lo) * 4.0) as u64;
        let v = lo + self.below(quarters.max(1)) as f32 / 4.0;
        if self.below(3) == 0 {
            v + self.below(1000) as f32 / 1371.0
        } else {
            v
        }
    }
}

/// The corpus: `n` emitter calls from `seed`, every polyline's height and
/// spine constant along it where `flat` (each slab's edges then stand at one
/// height), else varying from vertex to vertex.
fn corpus(seed: u64, n: usize, flat: bool) -> Vec<Case> {
    let mut s = Seed(seed);
    let clips = [
        BeamClip::grid(320, 200, 28),
        BeamClip {
            x0: 13,
            y0: 9,
            x1: 290,
            y1: 170,
            cell_h: 28,
            origin_y: 6,
        },
        BeamClip {
            x0: 0,
            y0: 0,
            x1: 320,
            y1: 200,
            cell_h: 7,
            origin_y: -3,
        },
    ];
    let steps = [1usize, 3, 4, 5, 8, 15];
    let shoulders = [1.0f32, 0.55, 0.8];
    (0..n)
        .map(|_| {
            let mut x = s.px(-6.0, 120.0);
            let (spine, up, dn) = (s.px(20.0, 170.0), s.px(0.0, 30.0), s.px(0.0, 12.0));
            let verts = (0..2 + s.below(6))
                .map(|_| {
                    let v = RibbonVertex {
                        x,
                        spine: if flat { spine } else { s.px(20.0, 170.0) },
                        up: if flat { up } else { s.px(0.0, 30.0) },
                        dn: if flat { dn } else { s.px(0.0, 12.0) },
                        core_up: if flat { up * 0.4 } else { s.px(0.0, 20.0) },
                        core_dn: if flat { dn * 0.4 } else { s.px(0.0, 8.0) },
                        color: (s.next() >> 40) as u32 & 0x00FF_FFFF,
                        cov: s.px(0.0, 255.0),
                        lift: if s.below(3) == 0 {
                            s.px(0.0, 60.0)
                        } else {
                            0.0
                        },
                        lift_span: s.px(0.0, 10.0),
                    };
                    // Mostly forward, a reversed segment one time in five
                    // and a degenerate one now and then.
                    x += match s.below(10) {
                        0 | 1 => -s.px(1.0, 40.0),
                        2 => 0.0,
                        _ => s.px(1.0, 60.0),
                    };
                    v
                })
                .collect();
            Case {
                clip: clips[s.below(clips.len() as u64) as usize],
                verts,
                shoulder: shoulders[s.below(shoulders.len() as u64) as usize],
                step: steps[s.below(steps.len() as u64) as usize],
                max_quads: if s.below(4) == 0 {
                    s.below(600) as usize
                } else {
                    100_000
                },
                blend: if s.below(2) == 0 {
                    GlowBlend::Add
                } else {
                    GlowBlend::Over
                },
            }
        })
        .collect()
}

/// An FNV-1a fold of 64-bit words.
struct Fold(u64);

impl Fold {
    const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn word(&mut self, w: u64) {
        for b in w.to_le_bytes() {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn quads(&mut self, quads: &[GlowQuad]) {
        self.word(quads.len() as u64);
        for q in quads {
            self.word(
                u64::from(q.row)
                    | u64::from(q.x) << 16
                    | u64::from(q.y) << 32
                    | u64::from(q.w) << 48,
            );
            self.word(u64::from(q.h) | u64::from(q.alpha) << 16 | u64::from(q.alpha2) << 24);
            self.word(u64::from(q.color) | u64::from(q.color2) << 32);
        }
    }

    fn ramps(&mut self, ramps: &GlowRamps) {
        self.word(ramps.items.len() as u64);
        for r in &ramps.items {
            self.word(u64::from(r.at) | u64::from(r.ink) << 32);
            self.word(
                u64::from(r.row)
                    | u64::from(r.x) << 16
                    | u64::from(r.y) << 32
                    | u64::from(r.w) << 48,
            );
            self.word(u64::from(r.h) | u64::from(r.join) << 16 | u64::from(r.joined) << 32);
        }
        self.word(ramps.ends.len() as u64);
        for &e in &ramps.ends {
            self.word(u64::from(e));
        }
        self.word(ramps.lit as u64);
    }
}

/// What a corpus emits through `emit`, folded: each call into the legacy
/// stream and into a ramp sink under the measured cost and under
/// [`RampCost::ALWAYS`], each with whether it was whole.
fn fold_corpus(cases: &[Case], emit: impl Fn(&mut dyn Emit, &Case) -> bool) -> u64 {
    let mut fold = Fold::new();
    for case in cases {
        let mut legacy = Vec::new();
        let whole = emit(&mut legacy, case);
        fold.word(u64::from(whole));
        fold.quads(&legacy);
        for cost in [None, Some(RampCost::ALWAYS)] {
            let (mut quads, mut ramps) = (Vec::new(), GlowRamps::default());
            let sink = RampSink::new(&mut quads, &mut ramps);
            let mut sink = match cost {
                Some(cost) => sink.costed(cost),
                None => sink,
            };
            let whole = emit(&mut sink, case);
            drop(sink);
            fold.word(u64::from(whole));
            fold.quads(&quads);
            fold.ramps(&ramps);
        }
    }
    fold.0
}

/// The sinks a corpus is emitted into, as one object-safe face of
/// [`BeamRows`]: each emitter is called on the sink's own type.
trait Emit {
    fn beam(&mut self, case: &Case) -> bool;
    fn wedge(&mut self, case: &Case, edge_px: f32) -> bool;
}

impl<S: BeamRows> Emit for S {
    fn beam(&mut self, c: &Case) -> bool {
        ribbon_beam_into(
            self,
            c.clip,
            &c.verts,
            c.shoulder,
            c.step,
            c.max_quads,
            c.blend,
        )
    }

    fn wedge(&mut self, c: &Case, edge_px: f32) -> bool {
        ribbon_wedge_into(
            self,
            c.clip,
            &c.verts,
            c.shoulder,
            c.step,
            c.max_quads,
            c.blend,
            edge_px,
        )
    }
}

/// The corpora: polylines whose heights and spines vary from vertex to
/// vertex, and polylines level along their length.
fn corpora() -> (Vec<Case>, Vec<Case>) {
    (
        corpus(0x9e37_79b9_7f4a_7c15, 240, false),
        corpus(0x2545_f491_4f6c_dd1d, 240, true),
    )
}

/// [`fold_corpus`] of [`ribbon_beam_into`] over [`corpora`], captured at the
/// wedge's parent (origin/main `ef3188605c`, before
/// [`ribbon_wedge_into`] existed): main's slab emission.
const MAIN_FULL: u64 = 0x48b5_2c99_81f9_77c7;
/// The level corpus's twin of [`MAIN_FULL`].
const MAIN_FLAT: u64 = 0x8039_fb48_7c20_0583;

/// A slab the wedge emitter lays as a slab is main's emission: over the
/// seeded corpora (sloped and level polylines, reversed and degenerate
/// segments, partial clips, odd bands, both blends, saturated budgets),
/// laid into the legacy stream and into ramp sinks under the measured cost
/// and [`RampCost::ALWAYS`], the slab emitter still folds to the hashes main
/// laid at the wedge's parent; the wedge emitter at an edge no slab exceeds
/// folds to them too, and at the glass's edge on the level corpus, whose
/// slabs' edges stand at one height, it lays no wedge and the same bytes.
/// The negative controls: on the sloped corpus at the glass's edge it lays
/// wedges and folds otherwise, and a quarter-pixel move of one polyline's
/// spine moves the fold.
#[test]
fn a_slab_emission_is_byte_identical_to_main() {
    let (full, flat) = corpora();
    let mut stats = (0usize, 0usize, 0usize);
    for c in &full {
        let mut quads = Vec::new();
        stats.0 += usize::from(!quads.beam(c));
        stats.1 += quads.len();
        stats.2 += usize::from(c.verts.windows(2).any(|w| w[1].x < w[0].x));
    }
    assert!(
        stats.0 >= 20 && stats.1 >= 100_000 && stats.2 >= 100,
        "the corpus cuts, paints and reverses: {stats:?}"
    );
    let _ = take_wedge_slabs();
    assert_eq!(
        fold_corpus(&full, |out, c| out.beam(c)),
        MAIN_FULL,
        "ribbon_beam_into lays main's bytes"
    );
    assert_eq!(
        fold_corpus(&flat, |out, c| out.beam(c)),
        MAIN_FLAT,
        "ribbon_beam_into lays main's bytes on the level corpus"
    );
    assert_eq!(take_wedge_slabs(), 0, "the slab emitter lays no wedge");
    assert_eq!(
        fold_corpus(&full, |out, c| out.wedge(c, f32::INFINITY)),
        MAIN_FULL,
        "the wedge emitter lays main's bytes where every slab is a slab"
    );
    assert_eq!(take_wedge_slabs(), 0, "an infinite edge admits no wedge");
    assert_eq!(
        fold_corpus(&flat, |out, c| out.wedge(c, EDGE)),
        MAIN_FLAT,
        "level slabs lay main's bytes at the glass's edge"
    );
    assert_eq!(take_wedge_slabs(), 0, "no level slab is a wedge");
    let wedged = fold_corpus(&full, |out, c| out.wedge(c, EDGE));
    let wedges = take_wedge_slabs();
    assert!(wedges > 1_000, "the sloped corpus lays wedges: {wedges}");
    assert_ne!(wedged, MAIN_FULL, "the fold sees the wedges");
    let mut planted = corpora().0;
    let moved = planted
        .iter_mut()
        .find(|c| c.max_quads > 1_000)
        .expect("an uncapped case");
    for v in &mut moved.verts {
        v.spine += 0.25;
    }
    assert_ne!(
        fold_corpus(&planted, |out, c| out.beam(c)),
        MAIN_FULL,
        "a quarter-pixel move of one polyline moves the fold"
    );
}

/// Device pixels of a cell at 2x (`docs/design/PRISM-GLASS.md` §2).
const CH: i32 = 28;
const CW: i32 = 15;
/// The tail's spine: the bottom of row 2.
const SPINE: i32 = 3 * CH;
/// The tail's oldest end.
const TAIL_X: f32 = 30.0;
/// The tail's cells.
const TAIL_CELLS: i32 = 8;

fn tail_clip() -> BeamClip {
    BeamClip::grid(320, 160, CH as usize)
}

/// A light glass's tail (PRISM-GLASS §2.2): the spine at a row's bottom and
/// nothing below it, `up` 0.96 ch at the hand eased in from nothing at the
/// oldest end over 2.5 cells, its core half of it, a vertex at each cell
/// edge, oldest first, its hue walking.
fn tail(at: f32, spine: f32) -> Vec<RibbonVertex> {
    (0..=TAIL_CELLS)
        .map(|k| {
            let up = 0.96 * CH as f32 * smoothstep01(k as f32 / 2.5);
            RibbonVertex {
                x: at + (k * CW) as f32,
                spine,
                up,
                dn: 0.0,
                core_up: 0.5 * up,
                core_dn: 0.0,
                color: [0x00FF_4020, 0x00F0_C010, 0x0020_E040, 0x0040_40FF][k as usize % 4],
                cov: 255.0,
                lift: 0.0,
                lift_span: 0.0,
            }
        })
        .collect()
}

/// The stride a cell's `slabs` take (`Ribbon`'s `cw.div_ceil(slabs)`).
const fn stride(slabs: i32) -> usize {
    (CW + slabs - 1).div_euclid(slabs) as usize
}

/// Each pixel `quads` lay, its `(colour, opacity)` by the [`GlowQuad`]
/// gradient law; a pixel laid twice fails.
fn pixels(quads: &[GlowQuad]) -> BTreeMap<(i32, i32), (u32, u8)> {
    let mut px = BTreeMap::new();
    for q in quads {
        let w = u32::from(q.w);
        for (i, x) in (q.x..q.x + q.w).enumerate() {
            for y in q.y..q.y + q.h {
                let i = i as u32;
                let ink = (
                    glow_lerp_rgb(q.color, q.color2, i, w),
                    glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), i, w) as u8,
                );
                let key = (i32::from(x), i32::from(y));
                assert!(px.insert(key, ink).is_none(), "pixel {key:?} laid twice");
            }
        }
    }
    px
}

/// Per column, the glass's height above the spine where it is at least
/// half opaque ([`pixels`]' opacity ≥ 128), oldest column first.
fn glass_heights(px: &BTreeMap<(i32, i32), (u32, u8)>) -> Vec<(i32, i32)> {
    let mut top = BTreeMap::new();
    for (&(x, y), &(_, a)) in px {
        if a >= 128 {
            let t = top.entry(x).or_insert(y);
            *t = (*t).min(y);
        }
    }
    top.into_iter().map(|(x, y)| (x, SPINE - y)).collect()
}

/// The largest climb between neighbouring glass columns.
fn max_step(heights: &[(i32, i32)]) -> i32 {
    heights
        .windows(2)
        .filter(|w| w[1].0 == w[0].0 + 1)
        .map(|w| (w[1].1 - w[0].1).abs())
        .max()
        .unwrap_or(0)
}

/// The glass's oldest end, laid at one, two and three slabs a cell, climbs
/// by at most a pixel from one column to the next and starts as a point, a
/// column at most two pixels tall: each slab whose edges stand at other
/// heights is a wedge whose rows run where the row meets the profile's
/// lines. The negative control is the slab emission of the same tail, which
/// samples each slab at its centre and climbs it by whole steps.
#[test]
fn the_oldest_end_has_no_step_over_one_pixel_at_1_2_and_3_slabs() {
    let verts = tail(TAIL_X, SPINE as f32);
    for slabs in [1, 2, 3] {
        let step = stride(slabs);
        let (mut wedge, mut slab) = (Vec::new(), Vec::new());
        let _ = take_wedge_slabs();
        assert!(ribbon_wedge_into(
            &mut wedge,
            tail_clip(),
            &verts,
            1.0,
            step,
            100_000,
            GlowBlend::Over,
            EDGE,
        ));
        assert!(
            take_wedge_slabs() > 0,
            "{slabs} slabs: the tail lays wedges"
        );
        assert!(ribbon_beam_into(
            &mut slab,
            tail_clip(),
            &verts,
            1.0,
            step,
            100_000,
            GlowBlend::Over,
        ));
        let (glass, stair) = (
            glass_heights(&pixels(&wedge)),
            glass_heights(&pixels(&slab)),
        );
        let (steps, stairs) = (max_step(&glass), max_step(&stair));
        eprintln!(
            "{slabs} slabs a cell: the wedge climbs at most {steps} px a column, the slabs {stairs} \
             px ({} and {} quads)",
            wedge.len(),
            slab.len()
        );
        let (first, last) = (glass.first().expect("glass"), glass.last().expect("glass"));
        assert!(
            glass.len() as i32 >= (TAIL_CELLS - 1) * CW && last.1 >= 18,
            "{slabs} slabs: the glass rises over the tail: {glass:?}"
        );
        assert!(
            steps <= 1,
            "{slabs} slabs: the oldest end steps {steps} px between columns: {glass:?}"
        );
        assert!(
            first.1 <= 2,
            "{slabs} slabs: the oldest end starts {} px tall, not as a point",
            first.1
        );
        assert!(
            stairs > 1,
            "{slabs} slabs: the slab control climbs by whole steps ({stairs} px), or the pin \
             proves nothing"
        );
    }
}

/// The wedge's dither is the screen's: its rows read the Bayer column of
/// their slab's screen ordinal, so a tail moved four slabs on lays the same
/// wedges moved, byte for byte, and one moved a slab on meets another
/// column of the matrix, a pixel's opacity moving by at most one level.
#[test]
fn the_wedge_keeps_the_dither_anchored_to_the_screen() {
    for slabs in [1, 2, 3] {
        let step = stride(slabs);
        let laid = |by: i32| {
            let mut quads = Vec::new();
            let _ = take_wedge_slabs();
            assert!(ribbon_wedge_into(
                &mut quads,
                tail_clip(),
                &tail(TAIL_X + 0.25 + by as f32, SPINE as f32 - 0.5),
                0.55,
                step,
                100_000,
                GlowBlend::Over,
                EDGE,
            ));
            assert!(
                take_wedge_slabs() > 0,
                "{slabs} slabs: the tail lays wedges"
            );
            // The tail's first three cells, where every slab is a wedge.
            pixels(&quads)
                .into_iter()
                .map(|((x, y), ink)| ((x - by, y), ink))
                .filter(|((x, _), _)| *x < TAIL_X as i32 + 3 * CW)
                .collect::<BTreeMap<_, _>>()
        };
        let one = step as i32;
        let (here, one_slab, four_slabs) = (laid(0), laid(one), laid(4 * one));
        assert!(
            here.len() > 400,
            "{slabs} slabs: the wedges paint: {}",
            here.len()
        );
        assert_eq!(
            four_slabs, here,
            "{slabs} slabs: four slabs on is a pure translation"
        );
        let mut changed = 0usize;
        for k in here.keys().chain(one_slab.keys()) {
            let (_, a) = here.get(k).copied().unwrap_or((0, 0));
            let (_, b) = one_slab.get(k).copied().unwrap_or((0, 0));
            changed += usize::from(a != b);
            assert!(
                a.abs_diff(b) <= 1,
                "{slabs} slabs: pixel {k:?} moved {a} -> {b}, more than a Bayer cell's level"
            );
        }
        assert!(
            changed > 0,
            "{slabs} slabs: a slab on meets another column of the matrix"
        );
    }
}

/// A wedge's end at zero opacity carries no colour, so no column it lays
/// adds light: over the glass tail and the sloped corpus laid source-over,
/// every row end at opacity 0 has colour 0 (in the legacy stream and as a
/// ramp sink's ends), and every pixel whose opacity lerps to 0 lerps to no
/// colour and leaves the page's bytes. The negative control is a wedge row
/// whose zero end keeps its colour: that column adds light to the page.
#[test]
fn a_zero_opacity_wedge_end_carries_no_colour() {
    use crate::fx_raster::fill_glow_span;
    use aterm_core::render::{FxInk, end_alpha, end_color};
    let page = 0xFF80_8080u32;
    let paint = |q: &GlowQuad| {
        let mut span = vec![page; usize::from(q.w)];
        fill_glow_span(
            &mut span,
            0,
            u32::from(q.w),
            FxInk::over(q.color, q.alpha),
            FxInk::over(q.color2, q.alpha2),
        );
        span
    };
    let (full, _) = corpora();
    let mut cases: Vec<Case> = full
        .into_iter()
        .filter(|c| matches!(c.blend, GlowBlend::Over))
        .collect();
    for slabs in [1, 2, 3] {
        cases.push(Case {
            clip: tail_clip(),
            verts: tail(TAIL_X, SPINE as f32),
            shoulder: 1.0,
            step: stride(slabs),
            max_quads: 100_000,
            blend: GlowBlend::Over,
        });
    }
    let (mut zero_ends, mut zero_columns) = (0usize, 0usize);
    for c in &cases {
        let mut quads = Vec::new();
        let _ = take_wedge_slabs();
        quads.wedge(c, EDGE);
        if take_wedge_slabs() == 0 {
            continue;
        }
        for q in &quads {
            for (color, alpha) in [(q.color, q.alpha), (q.color2, q.alpha2)] {
                if alpha == 0 {
                    zero_ends += 1;
                    assert_eq!(color, 0, "a zero-opacity end carries no colour: {q:?}");
                }
            }
            let span = paint(q);
            for (i, &px) in span.iter().enumerate() {
                let i = i as u32;
                let w = u32::from(q.w);
                if glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), i, w) == 0 {
                    zero_columns += 1;
                    assert_eq!(
                        glow_lerp_rgb(q.color, q.color2, i, w),
                        0,
                        "{q:?} column {i}"
                    );
                    assert_eq!(px, page, "{q:?}: column {i} at opacity 0 left the page");
                }
            }
        }
        let (mut sunk, mut ramps) = (Vec::new(), GlowRamps::default());
        let mut sink = RampSink::new(&mut sunk, &mut ramps).costed(RampCost::ALWAYS);
        sink.wedge(c, EDGE);
        drop(sink);
        for &end in &ramps.ends {
            if end_alpha(end) == 0 {
                assert_eq!(
                    end_color(end),
                    0,
                    "a ramp's zero-opacity end carries no colour"
                );
            }
        }
    }
    assert!(
        zero_ends > 100 && zero_columns > 0,
        "the wedges lay zero-opacity ends: {zero_ends} ends, {zero_columns} columns"
    );
    // The control: a faint row from a dark end, whose first column lerps to
    // opacity 0; its twin's dark end keeps the lit end's colour.
    let lit = super::premul_rgb(0x00FF_8040, 3);
    let row = GlowQuad {
        row: 2,
        x: 40,
        y: 60,
        w: 9,
        h: 1,
        color: 0,
        alpha: 0,
        color2: lit,
        alpha2: 3,
    };
    let control = GlowQuad { color: lit, ..row };
    assert_eq!(glow_lerp(0, 3, 0, 9), 0, "the first column is at opacity 0");
    assert_eq!(paint(&row)[0], page, "a dark end leaves the page");
    assert_ne!(
        paint(&control)[0],
        page,
        "a coloured zero-opacity end adds light, or the pin proves nothing"
    );
}

/// A wedge's rows stay where a slab's do: one pixel tall, inside the clip
/// and tagged by their grid band (ribbon-quads-clip-and-band); each lit
/// pixel within a pixel of its own column's reach, the vertex lerp at the
/// column's centre (ribbon-transverse-extent); and its runs tile the slab,
/// no pixel laid twice and no column of the mark's body left out
/// (ribbon-tiling-partition), on the glass tail with fractional vertices
/// and over the sloped corpus.
#[test]
fn a_wedge_lays_each_row_inside_its_slab_its_edges_its_clip_and_its_band() {
    let (full, _) = corpora();
    let mut wedged = 0usize;
    for c in &full {
        let mut quads = Vec::new();
        let _ = take_wedge_slabs();
        quads.wedge(c, EDGE);
        wedged += take_wedge_slabs();
        let clip = c.clip;
        for q in &quads {
            assert!(
                q.h == 1
                    && i32::from(q.x) >= clip.x0
                    && i32::from(q.x + q.w) <= clip.x1
                    && i32::from(q.y) >= clip.y0
                    && i32::from(q.y + q.h) <= clip.y1
                    && q.row
                        == (i32::from(q.y) - clip.origin_y)
                            .div_euclid(clip.cell_h)
                            .max(0) as u16,
                "{q:?} leaves its clip or band"
            );
        }
    }
    assert!(wedged > 1_000, "the corpus lays wedges: {wedged}");
    for slabs in [1, 2, 3] {
        let at = TAIL_X + 0.4;
        let mut verts = tail(at, SPINE as f32 - 0.3);
        // A vertex a third of a cell into each of the first cells.
        let thirds: Vec<RibbonVertex> = verts
            .windows(2)
            .take(3)
            .map(|w| {
                let t = 1.0 / 3.0;
                let lerp = |a: f32, b: f32| a + (b - a) * t;
                RibbonVertex {
                    x: lerp(w[0].x, w[1].x),
                    up: lerp(w[0].up, w[1].up) * 1.1,
                    core_up: lerp(w[0].core_up, w[1].core_up),
                    ..w[0]
                }
            })
            .collect();
        for (k, v) in thirds.into_iter().enumerate().rev() {
            verts.insert(k + 1, v);
        }
        let mut quads = Vec::new();
        assert!(ribbon_wedge_into(
            &mut quads,
            tail_clip(),
            &verts,
            1.0,
            stride(slabs),
            100_000,
            GlowBlend::Over,
            EDGE,
        ));
        let px = pixels(&quads);
        let reach = |x: i32| {
            let (first, last) = (verts[0].x, verts[verts.len() - 1].x);
            let centre = (x as f32 + 0.5).clamp(first, last);
            let w = verts
                .windows(2)
                .find(|w| w[0].x <= centre && centre <= w[1].x)
                .expect("a column of the tail");
            let t = (centre - w[0].x) / (w[1].x - w[0].x);
            w[0].spine - (w[0].up + (w[1].up - w[0].up) * t)
        };
        for (&(x, y), _) in px.iter().filter(|(_, ink)| ink.1 > 0) {
            assert!(
                y >= reach(x).floor() as i32 - 1 && y < SPINE,
                "{slabs} slabs: pixel ({x}, {y}) lies past its column's reach {}",
                reach(x)
            );
        }
        let body = SPINE - 1;
        let first = at.ceil() as i32;
        let lit: Vec<i32> = (first + CW..first + TAIL_CELLS * CW)
            .filter(|&x| !px.contains_key(&(x, body)))
            .collect();
        assert!(
            lit.is_empty(),
            "{slabs} slabs: columns of the body without an owner: {lit:?}"
        );
    }
}

/// A sub-pixel move of a wedge's spine slides it (ribbon-subpixel-continuity):
/// a tenth of a pixel moves a tail column's light by no more than a whole
/// pixel's move or 24 levels, and moves it at all.
#[test]
fn a_sub_pixel_spine_slides_a_wedge_continuously() {
    for slabs in [1, 2, 3] {
        // A column a cell into the tail, where its slabs are wedges.
        let column = TAIL_X as i32 + CW + 4;
        let light = |spine: f32| -> i64 {
            let mut quads = Vec::new();
            let _ = take_wedge_slabs();
            assert!(ribbon_wedge_into(
                &mut quads,
                tail_clip(),
                &tail(TAIL_X, spine),
                1.0,
                stride(slabs),
                100_000,
                GlowBlend::Over,
                EDGE,
            ));
            assert!(
                take_wedge_slabs() > 0,
                "{slabs} slabs: the tail lays wedges"
            );
            pixels(&quads)
                .iter()
                .filter(|((x, _), _)| *x == column)
                .map(|(_, &(_, a))| i64::from(a))
                .sum()
        };
        let spine = SPINE as f32;
        let track: Vec<i64> = (0..=10).map(|i| light(spine + i as f32 * 0.1)).collect();
        let steps: Vec<i64> = track.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
        assert!(
            steps.iter().any(|&s| s > 0),
            "{slabs} slabs: a sub-pixel spine moves the wedge: {track:?}"
        );
        let biggest = *steps.iter().max().expect("steps");
        let whole = (light(spine) - light(spine + 1.0)).abs().max(1);
        assert!(
            biggest <= whole.max(24),
            "{slabs} slabs: a tenth-pixel move stepped {biggest}, a staircase: {track:?}"
        );
    }
}

/// A saturated budget sheds a wedge as it sheds a slab emission
/// (ribbon-budget-shedding): under every cap the emitter reports the cut,
/// never exceeds the cap and lays a prefix of its uncapped walk; and a
/// reversed segment near the cap keeps the slab next to its head, not the
/// far one.
#[test]
fn a_saturated_budget_sheds_a_wedge_head_first() {
    let verts = tail(TAIL_X, SPINE as f32);
    let mut full = Vec::new();
    assert!(ribbon_wedge_into(
        &mut full,
        tail_clip(),
        &verts,
        1.0,
        stride(2),
        100_000,
        GlowBlend::Over,
        EDGE,
    ));
    assert!(full.len() > 200, "the tail paints: {}", full.len());
    for cap in 1..full.len() {
        let mut cut = Vec::new();
        assert!(
            !ribbon_wedge_into(
                &mut cut,
                tail_clip(),
                &verts,
                1.0,
                stride(2),
                cap,
                GlowBlend::Over,
                EDGE,
            ),
            "cap {cap}: a saturated budget reports it"
        );
        assert!(cut.len() <= cap, "cap {cap}: {} lights", cut.len());
        assert_eq!(
            cut[..],
            full[..cut.len()],
            "cap {cap}: not a prefix of the walk"
        );
    }
    const BUDGET: usize = 10_240;
    const STEP: usize = 10;
    let clip = BeamClip::grid(4_320, 3_360, 56);
    let head = RibbonVertex {
        x: 90.0,
        spine: 150.0,
        up: 36.0,
        dn: 24.0,
        core_up: 12.0,
        core_dn: 8.0,
        color: 0x00FF_8040,
        cov: 200.0,
        lift: 0.0,
        lift_span: 0.0,
    };
    let verts = [
        head,
        RibbonVertex {
            x: 60.0,
            up: 12.0,
            dn: 6.0,
            ..head
        },
    ];
    let mut full = Vec::new();
    assert!(ribbon_wedge_into(
        &mut full,
        clip,
        &verts,
        1.0,
        STEP,
        BUDGET,
        GlowBlend::Over,
        EDGE,
    ));
    let head_slab = full.iter().filter(|q| (80..90).contains(&q.x)).count();
    assert!(head_slab > 30, "the head's slab is a Retina-sized wedge");
    let admitted = BUDGET - head_slab - 1;
    let mut near_cap = vec![GlowQuad::default(); admitted];
    assert!(
        !ribbon_wedge_into(
            &mut near_cap,
            clip,
            &verts,
            1.0,
            STEP,
            BUDGET,
            GlowBlend::Over,
            EDGE,
        ),
        "the fixture saturates"
    );
    let kept = &near_cap[admitted..];
    assert!(near_cap.len() <= BUDGET);
    assert!(
        kept.iter().any(|q| q.x <= 85 && 85 < q.x + q.w),
        "the wedge next to the head at 90 went dark"
    );
    assert!(
        kept.iter().all(|q| !(q.x <= 65 && 65 < q.x + q.w)),
        "a capped frame kept the far wedge before the head"
    );
}

/// The glass's top edge, px (PRISM-GLASS §2.2, the effects' `GLASS_EDGE_PX`).
const MELT: f32 = 2.5;

/// The glass's profile (PRISM-GLASS §2.2, §15.2 1.7): whole from the spine
/// to [`MELT`] px under its reach, `smoothstep01((reach - d) / melt)` over
/// those px, nothing at or past the reach, the same on both sides, and
/// nothing for a reach that is not a positive number; a melt that is not one
/// is a hard edge. The negative control: [`super::ribbon_profile`] at shoulder
/// `1` melts over at least half of every reach, so it cannot draw the glass.
#[test]
fn the_glass_profile_is_whole_to_its_melt_and_nothing_past_its_reach() {
    for reach in [2.0f32, 2.5, 7.0, 17.28, 26.88] {
        let mut last = 1.0f32;
        for k in 0..=400 {
            let d = reach * 1.1 * k as f32 / 400.0;
            let p = glass_profile(d, reach, MELT);
            assert!((0.0..=1.0).contains(&p), "reach {reach}, d {d}: {p}");
            assert_eq!(p.to_bits(), glass_profile(-d, reach, MELT).to_bits());
            assert!(p <= last, "reach {reach}: rises at d {d}");
            last = p;
            if reach - d >= MELT {
                assert_eq!(p, 1.0, "reach {reach}: whole at d {d}");
            } else if d < reach {
                assert_eq!(p, smoothstep01((reach - d) / MELT), "reach {reach}, d {d}");
            } else {
                assert_eq!(p, 0.0, "reach {reach}: dark at d {d}");
            }
        }
        if reach >= 4.0 * MELT {
            let d = reach * 0.75;
            assert!(
                super::ribbon_profile(d, reach, reach, 1.0) < 0.99
                    && glass_profile(d, reach, MELT) == 1.0,
                "control: ribbon_profile melts over half of reach {reach}"
            );
        }
    }
    for bad in [f32::NAN, 0.0, -3.0] {
        assert_eq!(glass_profile(0.0, bad, MELT), 0.0, "reach {bad}");
    }
    assert_eq!(glass_profile(f32::NAN, 10.0, MELT), 0.0);
    for hard in [0.0, -1.0, f32::NAN] {
        assert_eq!(glass_profile(9.9, 10.0, hard), 1.0, "melt {hard}");
        assert_eq!(glass_profile(10.0, 10.0, hard), 0.0, "melt {hard}");
    }
}

/// The glass's body (PRISM-GLASS §2.2, §15.2 1.7): over a level run laid by
/// [`ribbon_glass_into`], every row from [`MELT`] px under its top to its
/// spine is whole, the rows above them melt, nothing is laid below the
/// spine or above the top, and every quad is source-over; each slab is
/// counted a glass slab and none of them is a wedge. Its sloped tail lays
/// wedges and nothing below its spine, and climbs at most two pixels a column
/// (a wedge row's runs are levelled at their pixel edges, which the melt's
/// 2.5 px turn by up to a row at a vertex), where its slabs laid at their
/// centres climb by whole steps. The negative control is
/// [`ribbon_wedge_into`] at shoulder `1` over the same vertices, which melts
/// over half the reach and lays no glass slab.
#[test]
fn a_glass_lays_whole_light_to_its_melt_and_nothing_below_its_spine() {
    let up = 0.96 * CH as f32;
    let level: Vec<RibbonVertex> = (0..=TAIL_CELLS)
        .map(|k| RibbonVertex {
            x: TAIL_X + (k * CW) as f32,
            up,
            core_up: 0.5 * up,
            ..tail(TAIL_X, SPINE as f32)[0]
        })
        .collect();
    let whole_rows = |quads: &[GlowQuad]| -> BTreeMap<i32, usize> {
        let mut rows = BTreeMap::new();
        for (&(x, _), &(_, a)) in &pixels(quads) {
            if a == 255 {
                *rows.entry(x).or_insert(0) += 1;
            }
        }
        rows
    };
    for slabs in [1, 2, 3] {
        let step = stride(slabs);
        let _ = (take_glass_slabs(), take_wedge_slabs());
        let mut glass = Vec::new();
        assert!(ribbon_glass_into(
            &mut glass,
            tail_clip(),
            &level,
            step,
            100_000,
            WEDGE_EDGE_PX,
            MELT,
        ));
        let (glass_slabs, wedges) = (take_glass_slabs(), take_wedge_slabs());
        assert!(
            glass_slabs >= (TAIL_CELLS * CW) as usize / step,
            "{slabs} slabs: {glass_slabs}"
        );
        assert_eq!(wedges, 0, "{slabs} slabs: a level glass lays no wedge");
        let top = SPINE as f32 - up;
        for q in &glass {
            assert!(q.alpha > 0 && q.alpha2 > 0, "{slabs} slabs: source-over");
            assert!(
                i32::from(q.y) < SPINE,
                "{slabs} slabs: a row below the spine: {q:?}"
            );
            assert!(
                f32::from(q.y) + 1.0 > top,
                "{slabs} slabs: a row above the top: {q:?}"
            );
        }
        let solid = SPINE - (top + MELT).ceil() as i32;
        let rows = whole_rows(&glass);
        assert_eq!(
            rows.len() as i32,
            TAIL_CELLS * CW,
            "{slabs} slabs: every column"
        );
        assert!(
            rows.values().all(|&n| n as i32 >= solid),
            "{slabs} slabs: a column under {solid} whole rows: {rows:?}"
        );
        assert!(
            rows.values()
                .all(|&n| (n as i32) < SPINE - top.floor() as i32),
            "{slabs} slabs: the top melts: {rows:?}"
        );

        let mut beam = Vec::new();
        assert!(ribbon_wedge_into(
            &mut beam,
            tail_clip(),
            &level,
            1.0,
            step,
            100_000,
            GlowBlend::Over,
            WEDGE_EDGE_PX,
        ));
        assert_eq!(
            take_glass_slabs(),
            0,
            "control: the wedge emitter lays no glass slab"
        );
        let beam_rows = whole_rows(&beam);
        assert!(
            beam_rows.values().all(|&n| n as i32 + 6 <= solid),
            "control ({slabs} slabs): the shouldered profile melts over half its reach, whole \
             over fewer than {solid} rows: {beam_rows:?}"
        );

        let sloped = |edge_px: f32| {
            let mut quads = Vec::new();
            assert!(ribbon_glass_into(
                &mut quads,
                tail_clip(),
                &tail(TAIL_X, SPINE as f32),
                step,
                100_000,
                edge_px,
                MELT,
            ));
            quads
        };
        let (wedged, slabbed) = (sloped(WEDGE_EDGE_PX), sloped(f32::INFINITY));
        let wedges = take_wedge_slabs();
        assert!(wedges > 0, "{slabs} slabs: the tail lays wedges");
        let (steps, stairs) = (
            max_step(&glass_heights(&pixels(&wedged))),
            max_step(&glass_heights(&pixels(&slabbed))),
        );
        eprintln!(
            "{slabs} slabs a cell: the glass's tail climbs at most {steps} px a column wedged, \
             {stairs} px as slabs"
        );
        assert!(
            steps <= 2 && stairs > steps,
            "{slabs} slabs: the glass's tail climbs {steps} px a column wedged, {stairs} as slabs"
        );
        assert!(
            wedged.iter().all(|q| i32::from(q.y) < SPINE),
            "{slabs} slabs: the tail lays nothing below the spine"
        );
    }
}
