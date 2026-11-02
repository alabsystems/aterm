// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The work variation deltas cost: `gvar` inference and phantom points under
//! the outline's variation budget, in the reader and in the hinter, and item
//! variation deltas (`HVAR`) under their own.

use aterm_font::{Face, GlyphId, PointKind, PointSink, Rect};

use crate::gvar_tuples::face_bytes_with;
use crate::never_panic::sfnt;

/// One tuple of a glyph's variation data, applying at every instance (an
/// embedded peak of 0): byte deltas `x` and `y` for the `points` it numbers,
/// or for every point (phantom points included) when it numbers none.
#[derive(Clone, Copy)]
struct Tuple<'a> {
    points: &'a [u16],
    x: &'a [i8],
    y: &'a [i8],
}

/// Packed point numbers: the count, then runs of up to 128 word-sized
/// numbers, each the difference from the one before.
fn packed_points(points: &[u16]) -> Vec<u8> {
    let count = u16::try_from(points.len()).expect("a short point list");
    let mut v = if count < 0x80 {
        vec![u8::try_from(count).expect("under 0x80")]
    } else {
        (count | 0x8000).to_be_bytes().to_vec()
    };
    let mut prev = 0u16;
    for run in points.chunks(128) {
        v.push(0x80 | u8::try_from(run.len() - 1).expect("a run of 128"));
        for &p in run {
            v.extend(p.wrapping_sub(prev).to_be_bytes());
            prev = p;
        }
    }
    v
}

/// Packed deltas: runs of up to 64 bytes.
fn packed_deltas(deltas: &[i8]) -> Vec<u8> {
    let mut v = Vec::new();
    for run in deltas.chunks(64) {
        v.push(u8::try_from(run.len() - 1).expect("a run of 64"));
        v.extend(run.iter().map(|d| d.to_be_bytes()[0]));
    }
    v
}

fn variations(tuples: &[Tuple]) -> Vec<u8> {
    let serialized: Vec<Vec<u8>> = tuples
        .iter()
        .map(|t| {
            let mut s = if t.points.is_empty() {
                Vec::new()
            } else {
                packed_points(t.points)
            };
            s.extend(packed_deltas(t.x));
            s.extend(packed_deltas(t.y));
            s
        })
        .collect();
    let mut v = u16::try_from(tuples.len())
        .expect("a 12-bit count")
        .to_be_bytes()
        .to_vec();
    v.extend(
        u16::try_from(4 + 6 * tuples.len())
            .expect("headers fit")
            .to_be_bytes(),
    );
    for (t, s) in tuples.iter().zip(&serialized) {
        v.extend(u16::try_from(s.len()).expect("small").to_be_bytes());
        let private = if t.points.is_empty() { 0 } else { 0x2000 };
        v.extend((0x8000u16 | private).to_be_bytes()); // EMBEDDED_PEAK_TUPLE
        v.extend(0u16.to_be_bytes()); // peak 0 on `wght`
    }
    v.extend(serialized.concat());
    v
}

/// A simple glyph's `glyf` record: on-curve `contours`, coordinates as `i16`
/// deltas, padded to an even length.
fn contours(contours: &[&[(i16, i16)]]) -> Vec<u8> {
    let mut g = i16::try_from(contours.len())
        .expect("few contours")
        .to_be_bytes()
        .to_vec();
    g.extend([0; 8]);
    let mut end = 0;
    for contour in contours {
        end += contour.len();
        g.extend(u16::try_from(end - 1).expect("few points").to_be_bytes());
    }
    g.extend(0u16.to_be_bytes()); // no instructions
    g.extend(vec![1u8; end]);
    let mut prev = (0i16, 0i16);
    let (mut xs, mut ys) = (Vec::new(), Vec::new());
    for &(x, y) in contours.iter().flat_map(|c| c.iter()) {
        xs.extend(x.wrapping_sub(prev.0).to_be_bytes());
        ys.extend(y.wrapping_sub(prev.1).to_be_bytes());
        prev = (x, y);
    }
    g.extend(xs);
    g.extend(ys);
    g.resize(g.len().next_multiple_of(2), 0);
    g
}

/// A simple glyph's `glyf` record: one contour of `n` on-curve points, all
/// at the origin, in a few hundred bytes (repeated flags, no coordinates).
fn coincident(n: usize) -> Vec<u8> {
    let mut g = 1i16.to_be_bytes().to_vec();
    g.extend([0; 8]);
    g.extend(
        u16::try_from(n - 1)
            .expect("a u16 point count")
            .to_be_bytes(),
    );
    g.extend(0u16.to_be_bytes());
    let mut left = n;
    while left > 0 {
        let run = left.min(256);
        // On curve, repeated, x and y the same as the point before.
        g.extend([0x39, u8::try_from(run - 1).expect("a repeat count")]);
        left -= run;
    }
    g.resize(g.len().next_multiple_of(2), 0);
    g
}

/// A one-glyph `hmtx`: advance 500.
const HMTX: [u8; 4] = [0x01, 0xF4, 0, 0];

/// A composite `glyf` record placing glyph 0 `count` times, each at the origin.
fn copies_of_glyph_0(count: usize) -> Vec<u8> {
    let mut g = (-1i16).to_be_bytes().to_vec();
    g.extend([0; 8]);
    for left in (0..count).rev() {
        // ARG_1_AND_2_ARE_WORDS | ARGS_ARE_XY_VALUES, and MORE_COMPONENTS
        // on all but the last.
        let more = if left > 0 { 0x0020 } else { 0 };
        g.extend((0x0003u16 | more).to_be_bytes());
        g.extend(0u16.to_be_bytes());
        g.extend([0; 4]);
    }
    g
}

/// Every point of glyph 0 as stored, or `None` when the outline is refused.
fn drawn(face: &Face, glyph: u16) -> Option<Vec<(f32, f32)>> {
    struct Points(Vec<(f32, f32)>);
    impl PointSink for Points {
        fn point(&mut self, x: f32, y: f32, _: PointKind) {
            self.0.push((x, y));
        }
        fn end_contour(&mut self) {}
    }
    let mut points = Points(Vec::new());
    face.outline_glyph_points(GlyphId(glyph), &mut points)?;
    Some(points.0)
}

fn advance(bytes: &[u8]) -> Option<u16> {
    Face::parse(bytes, 0)
        .expect("the synthetic face parses")
        .glyph_hor_advance(GlyphId(0))
}

/// A tuple moves the points it numbers; each point between two of them in a
/// contour moves between their deltas, as far along as it lies between them
/// on each axis (and as the nearer one past either end), wrapping around the
/// contour; a contour with one numbered point moves with it, one with none
/// stays.
#[test]
fn untouched_points_follow_their_contour() {
    let glyph = contours(&[
        &[(0, 0), (50, 25), (100, 50), (75, 100)],
        &[(200, 0), (300, 0), (250, 80)],
        &[(400, 400), (500, 400)],
    ]);
    let tuple = Tuple {
        points: &[0, 2, 5],
        x: &[10, 30, -10],
        y: &[0, 20, 5],
    };
    let bytes = face_bytes_with(&[(glyph, variations(&[tuple]))], &[]);
    let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
    assert_eq!(
        drawn(&face, 0),
        Some(vec![
            (10.0, 0.0),
            (70.0, 35.0),
            (130.0, 70.0),
            (100.0, 120.0),
            (190.0, 5.0),
            (290.0, 5.0),
            (240.0, 85.0),
            (400.0, 400.0),
            (500.0, 400.0),
        ])
    );
}

/// A glyph of `points` coincident points under `tuples` tuples, each moving
/// its first and last point by (+5, +5) and leaving the rest to inference.
fn ends_moved(points: usize, tuples: usize) -> Vec<u8> {
    let last = [0, u16::try_from(points - 1).expect("a u16 point count")];
    let tuple = Tuple {
        points: &last,
        x: &[5, 5],
        y: &[5, 5],
    };
    face_bytes_with(
        &[(coincident(points), variations(&vec![tuple; tuples]))],
        &[(b"hmtx", &HMTX)],
    )
}

const fn at(n: i16) -> Option<Rect> {
    Some(Rect {
        x_min: n,
        y_min: n,
        x_max: n,
        y_max: n,
    })
}

/// Inferring a tuple's untouched points is a pass over each contour, not a
/// search from every point, and the outline's variation budget is charged a
/// step per point for it (phantom points included) and one for the tuple's
/// header, the structure's budget a step per glyph and per point as before.
#[test]
fn inference_is_charged_a_step_per_point_per_tuple() {
    let bytes = ends_moved(32_000, 1);
    let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
    let _ = aterm_font::take_spent();
    assert_eq!(face.glyph_bounding_box(GlyphId(0)), at(5));
    let spent = aterm_font::take_spent();
    assert_eq!(spent.variation_steps, 1 + 32_004);
    assert_eq!(spent.outline_steps, 1 + 32_000);
}

/// Points times tuples past the variation budget refuse the outline and the
/// phantom points before any inference runs; the same glyph under four
/// tuples draws.
#[test]
fn points_times_tuples_past_the_budget_are_refused() {
    let four = ends_moved(60_000, 4);
    let face = Face::parse(&four, 0).expect("the synthetic face parses");
    assert_eq!(face.glyph_bounding_box(GlyphId(0)), at(20));
    assert!(face.glyph_phantom_points(GlyphId(0)).is_some());

    let many = ends_moved(60_000, 140);
    let face = Face::parse(&many, 0).expect("the synthetic face parses");
    assert_eq!(face.glyph_bounding_box(GlyphId(0)), None);
    assert!(face.glyph_phantom_points(GlyphId(0)).is_none());
}

/// Composite-heavy variable faces (Handjet, Bitcount) spend over a million
/// variation steps on one glyph at interior instances: a thousand copies of
/// a square under 128 tuples (1,152,000 steps) still outline, and the
/// structure's own budget sees only the glyphs and their points.
#[test]
fn a_million_steps_of_small_components_still_outline() {
    let bytes = small_components(1_000, 128);
    let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
    let _ = aterm_font::take_spent();
    assert_eq!(
        face.glyph_bounding_box(GlyphId(1)),
        Some(Rect {
            x_min: 128,
            y_min: 0,
            x_max: 228,
            y_max: 100,
        })
    );
    let spent = aterm_font::take_spent();
    assert_eq!(spent.variation_steps, 1_000 * (128 + 128 * (4 + 4)));
    assert_eq!(spent.outline_steps, 1 + 1_000 * (1 + 4));
}

/// A square that every tuple moves by (+1, 0), every point and phantom point
/// alike, under `tuples` tuples, and a composite of `count` copies of it.
fn small_components(count: usize, tuples: usize) -> Vec<u8> {
    let square = contours(&[&[(0, 0), (100, 0), (100, 100), (0, 100)]]);
    let shift = Tuple {
        points: &[],
        x: &[1; 8],
        y: &[0; 8],
    };
    face_bytes_with(
        &[
            (square, variations(&vec![shift; tuples])),
            (copies_of_glyph_0(count), Vec::new()),
        ],
        &[(b"hmtx", &HMTX)],
    )
}

/// A glyph of `points` coincident points and advance 500 under `tuples`
/// tuples, each moving only its right phantom point, by +1.
fn advance_moved(points: usize, tuples: usize) -> Vec<u8> {
    let right = [u16::try_from(points + 1).expect("a u16 point index")];
    let tuple = Tuple {
        points: &right,
        x: &[1],
        y: &[0],
    };
    face_bytes_with(
        &[(coincident(points), variations(&vec![tuple; tuples]))],
        &[(b"hmtx", &HMTX)],
    )
}

/// Reaching the phantom points steps each tuple through every outline point,
/// charged to the variation budget: past it the advance is `hmtx`'s alone.
#[test]
fn phantom_points_are_charged_every_point_of_every_tuple() {
    assert_eq!(advance(&advance_moved(100, 4)), Some(504));
    assert_eq!(advance(&advance_moved(60_000, 4)), Some(504));
    assert_eq!(
        advance(&advance_moved(60_000, 140)),
        Some(500),
        "140 tuples over 60,004 points are past the budget"
    );
}

/// The hinter's own `gvar` loader, at full weight, into a count of the
/// outline's commands: `None` when it refuses the glyph.
#[cfg(any(feature = "hint", debug_assertions))]
fn hinted(bytes: &[u8], glyph: u16) -> Option<u64> {
    use aterm_font::autohint::{HintingInstance, Target};
    struct Count(u64);
    impl aterm_font::OutlineSink for Count {
        fn move_to(&mut self, _: f32, _: f32) {
            self.0 += 1;
        }
        fn line_to(&mut self, _: f32, _: f32) {
            self.0 += 1;
        }
        fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {
            self.0 += 1;
        }
        fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {
            self.0 += 1;
        }
        fn close(&mut self) {
            self.0 += 1;
        }
    }
    let full = [(u32::from_be_bytes(*b"wght"), 900.0)];
    let hint = HintingInstance::new(bytes, 0, 16.0, &full, Target::Normal)
        .expect("the synthetic face hints");
    let mut count = Count(0);
    hint.draw(bytes, 0, glyph, &mut count)?;
    Some(count.0)
}

/// The hinter charges its `gvar` deltas as the reader does, a step per
/// tuple header and per point of every tuple, against the same budget:
/// past it the glyph is refused before its deltas are read, not drawn
/// unvaried, and the same glyph under four tuples draws.
#[cfg(any(feature = "hint", debug_assertions))]
#[test]
fn the_hinter_refuses_points_times_tuples_past_the_budget() {
    assert!(hinted(&ends_moved(60_000, 4), 0).is_some());
    assert_eq!(hinted(&ends_moved(60_000, 140), 0), None);
}

/// What the reader outlines at a million variation steps, the hinter hints.
#[cfg(any(feature = "hint", debug_assertions))]
#[test]
fn a_million_steps_of_small_components_still_hint() {
    let bytes = small_components(1_000, 128);
    let _ = aterm_font::take_spent();
    assert!(hinted(&bytes, 1).is_some_and(|commands| commands > 0));
    assert_eq!(
        aterm_font::take_spent().variation_steps,
        1_000 * (128 + 128 * (4 + 4))
    );
}

/// A face of `axes` axes whose `HVAR` sends every glyph to one delta set of
/// `regions` region indices, all naming one region that applies everywhere
/// (a peak of 0 on every axis), each with a delta of +1.
fn hvar_face(axes: u16, regions: u16) -> Vec<u8> {
    let mut fvar = Vec::new();
    for v in [1u16, 0, 16, 2, axes, 20, 0, 8] {
        fvar.extend(v.to_be_bytes());
    }
    for axis in 0..axes {
        fvar.extend(*b"X");
        fvar.extend(format!("{axis:03}").bytes());
        for user in [0i32, 0, 1] {
            fvar.extend((user << 16).to_be_bytes());
        }
        fvar.extend([0, 0, 1, 0]);
    }
    let regions_at = 20 + 6 + 12; // header, DeltaSetIndexMap, store header
    let data_at = regions_at + 4 + 6 * u32::from(axes);
    let mut hvar = 0x0001_0000u32.to_be_bytes().to_vec();
    hvar.extend(26u32.to_be_bytes()); // the store, after the map
    hvar.extend(20u32.to_be_bytes()); // advance mapping
    hvar.extend([0; 8]); // no side-bearing mappings
    hvar.extend([0, 0, 0, 1, 0, 0]); // format 0, one 1-byte entry: (0, 0)
    hvar.extend(1u16.to_be_bytes()); // store format
    hvar.extend((regions_at - 26).to_be_bytes());
    hvar.extend(1u16.to_be_bytes());
    hvar.extend((data_at - 26).to_be_bytes());
    hvar.extend(axes.to_be_bytes());
    hvar.extend(1u16.to_be_bytes()); // one region
    hvar.extend(vec![0u8; 6 * usize::from(axes)]); // start, peak, end 0
    hvar.extend(1u16.to_be_bytes()); // one item
    hvar.extend(0u16.to_be_bytes()); // byte deltas
    hvar.extend(regions.to_be_bytes());
    hvar.extend(vec![0u8; 2 * usize::from(regions)]); // region 0 each time
    hvar.extend(vec![1u8; usize::from(regions)]);
    sfnt(1, &[(b"HVAR", &hvar), (b"fvar", &fvar), (b"hmtx", &HMTX)])
}

/// An item variation delta evaluates every region its set names over every
/// axis, charged per region and axis: past the budget the delta is refused
/// and the advance is `hmtx`'s alone.
#[test]
fn item_variation_deltas_are_charged_every_axis_of_every_region() {
    assert_eq!(advance(&hvar_face(64, 4)), Some(504));
    assert_eq!(advance(&hvar_face(64, 1_000)), Some(1_500));
    assert_eq!(
        advance(&hvar_face(64, 2_048)),
        Some(500),
        "2,048 regions of 64 axes are past the budget"
    );
}
