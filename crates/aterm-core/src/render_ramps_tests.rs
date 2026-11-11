// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn quad(x: u16) -> GlowQuad {
    GlowQuad::flat(0, x, 0, 1, 1, 0x10, 0)
}

/// Two ramps between three quads: rows `(1, dark, 2)` and `(3)`.
fn stream() -> (Vec<GlowQuad>, GlowRamps) {
    let mut ramps = GlowRamps::default();
    let quads = vec![quad(0), quad(1), quad(2)];
    ramps.open(1, 0, (10, 0, 4), (glow_end(0x01, 0), glow_end(0x02, 0)));
    ramps.extend_last(1, (glow_end(0x03, 0), 0));
    ramps.open(2, 0, (20, 5, 2), (glow_end(0x04, 9), glow_end(0x05, 9)));
    (quads, ramps)
}

/// A ramp's lit rows are the quads it stands for, in the stream's draw
/// order, a dark row none; a truncation by light count keeps exactly the
/// legacy stream's first lights.
#[test]
fn ramps_expand_and_truncate_as_the_quads_they_stand_for() {
    let (quads, ramps) = stream();
    assert_eq!(ramps.lit, 3);
    assert_eq!(ramps.laid(quads.len()), 6);
    let mut legacy = Vec::new();
    ramps.expand_into(&quads, &mut legacy);
    let order: Vec<_> = legacy.iter().map(|q| (q.x, q.y)).collect();
    assert_eq!(order, [(0, 0), (10, 0), (10, 2), (1, 0), (20, 5), (2, 0)]);
    assert_eq!(legacy[2].color, 0x03);
    assert_eq!(legacy[2].color2, 0);
    for n in 0..=7 {
        let (mut q, mut r) = stream();
        r.truncate_laid(&mut q, n);
        let mut got = Vec::new();
        r.expand_into(&q, &mut got);
        assert_eq!(got, legacy[..n.min(legacy.len())], "cut at {n}");
        assert_eq!(r.laid(q.len()), n.min(legacy.len()), "count at {n}");
    }
}

/// Keeping some of a stream's lights, a ramp clipped, is keeping (and
/// clipping) the quads they stand for, in order.
#[test]
fn retained_and_clipped_ramps_are_their_quads_retained_and_clipped() {
    let clip = (11, 1, 21, 6);
    let narrow = |(x, y, w, h): (u16, u16, u16, u16)| {
        let (x0, y0) = (x.max(clip.0), y.max(clip.1));
        let (x1, y1) = (
            x.saturating_add(w).min(clip.2),
            y.saturating_add(h).min(clip.3),
        );
        (x1 > x0 && y1 > y0).then(|| (x0, y0, x1 - x0, y1 - y0))
    };
    let keep = |q: &mut GlowQuad| q.x != 1;
    let (quads, ramps) = stream();
    let mut want = Vec::new();
    ramps.expand_into(&quads, &mut want);
    want.retain_mut(|q| {
        // The legacy stream's rows of ramp one keep their quads' x.
        if q.x < 10 {
            return keep(q);
        }
        match narrow((q.x, q.y, q.w, q.h)) {
            Some((x, y, w, h)) => {
                (q.x, q.y, q.w, q.h) = (x, y, w, h);
                true
            }
            None => false,
        }
    });
    let (mut q, mut r) = stream();
    r.retain_mut(&mut q, keep, |r| r.clip_to(clip));
    let mut got = Vec::new();
    r.expand_into(&q, &mut got);
    assert_eq!(got, want);
    assert_eq!(r.lit, 2);
}

/// A slab's column as a ramp beside two quads: an edge row, a dark row,
/// a run of three rows (one light), an edge row; then a run alone.
fn column() -> (Vec<GlowQuad>, GlowRamps) {
    let e = |c: u32| (glow_end(c, 0), glow_end(c, 0));
    let mut ramps = GlowRamps::default();
    ramps.open_run(1, 0, (10, 2, 2), 1, e(0x01));
    assert!(ramps.extend_run(1, 3, e(0x02)));
    assert!(!ramps.extend_run(0, 2, e(0x03)), "a ramp holds one run");
    assert!(ramps.extend_run(0, 1, e(0x04)));
    ramps.open_run(2, 0, (20, 0, 1), 2, e(0x05));
    (vec![quad(0), quad(1)], ramps)
}

/// A run is the one light its rows lay, its every row reading the ends
/// it keeps once: the stream truncates by light count and clips as the
/// legacy stream of its quads does, a run's quad whole or cut to the box.
#[test]
fn a_run_is_one_light_as_tall_as_its_rows() {
    let (quads, ramps) = column();
    assert_eq!(ramps.lit, 4);
    let mut legacy = Vec::new();
    ramps.expand_into(&quads, &mut legacy);
    let rects: Vec<_> = legacy
        .iter()
        .map(|q| (q.x, q.y, q.w, q.h, q.color))
        .collect();
    assert_eq!(
        rects,
        [
            (0, 0, 1, 1, 0x10),
            (10, 2, 2, 1, 0x01),
            (10, 4, 2, 3, 0x02),
            (10, 7, 2, 1, 0x04),
            (1, 0, 1, 1, 0x10),
            (20, 0, 1, 2, 0x05),
        ]
    );
    let r = ramps.items[0];
    assert_eq!((r.h, r.join, r.joined), (6, 2, 2));
    // Each run keeps its ends once: four rows of ends, then one.
    assert_eq!((r.end_count(), ramps.ends.len()), (8, 10));
    for k in 2..5 {
        assert_eq!(
            r.row_ends(&ramps.ends, k),
            (glow_end(0x02, 0), glow_end(0x02, 0))
        );
    }
    for n in 0..=legacy.len() + 1 {
        let (mut q, mut r) = column();
        r.truncate_laid(&mut q, n);
        let mut got = Vec::new();
        r.expand_into(&q, &mut got);
        assert_eq!(got, legacy[..n.min(legacy.len())], "cut at {n}");
        assert_eq!(r.laid(q.len()), n.min(legacy.len()), "count at {n}");
    }
    for y0 in 0..9 {
        for y1 in y0 + 1..10 {
            let clip = (0, y0, 30, y1);
            let narrow = |q: &mut GlowQuad| {
                let (a, b) = (q.y.max(y0), (q.y + q.h).min(y1));
                (q.y, q.h) = (a, b.saturating_sub(a));
                b > a
            };
            let mut want = legacy.clone();
            want.retain_mut(narrow);
            let (mut q, mut r) = column();
            r.retain_mut(&mut q, narrow, |r| r.clip_to(clip));
            // A ramp's lights read off the ends it keeps are its rows'.
            for ramp in &r.items {
                let rows: Vec<_> = (0..ramp.h)
                    .filter_map(|k| ramp.row_quad(&r.ends, k))
                    .collect();
                assert_eq!(ramp.quads(&r.ends).collect::<Vec<_>>(), rows);
            }
            let mut got = Vec::new();
            r.expand_into(&q, &mut got);
            assert_eq!(got, want, "rows {y0}..{y1}");
            assert_eq!(r.laid(q.len()), want.len(), "count over rows {y0}..{y1}");
        }
    }
}

/// A band of rows reads a ramp's quads that meet it, the run whole where
/// any of its rows does, as filtering every quad by the band would.
#[test]
fn a_band_reads_the_quads_that_meet_it() {
    let (_, ramps) = column();
    let mut met = 0;
    for ramp in &ramps.items {
        for y0 in -2..12 {
            for y1 in y0 + 1..13 {
                let want: Vec<_> = ramp
                    .quads(&ramps.ends)
                    .filter(|q| i32::from(q.y) < y1 && i32::from(q.y + q.h) > y0)
                    .collect();
                met += want.len();
                let got: Vec<_> = ramp.quads_within(&ramps.ends, (y0, y1)).collect();
                assert_eq!(got, want, "rows {y0}..{y1} of {ramp:?}");
            }
        }
    }
    assert!(met > 0, "the bands met the ramps' lights");
}

/// Appending a stream's ramps after another's quads keeps both orders.
#[test]
fn appended_ramps_keep_their_place_after_the_base() {
    let (q0, r0) = stream();
    let (q1, r1) = stream();
    let (mut q, mut r) = (q0.clone(), r0.clone());
    r.append_from(&r1, q.len());
    q.extend_from_slice(&q1);
    let (mut want, mut tail) = (Vec::new(), Vec::new());
    r0.expand_into(&q0, &mut want);
    r1.expand_into(&q1, &mut tail);
    want.extend(tail);
    let mut got = Vec::new();
    r.expand_into(&q, &mut got);
    assert_eq!(got, want);
    assert_eq!(r.lit, 6);
}
