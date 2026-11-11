// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{BeamRows, RAMP_COST, RampCost, RampSink};
use aterm_core::render::{GlowQuad, GlowRamps};

/// A cost under which five lights are the fewest that pay
/// (`5 · 8 > 35 ≥ 4 · 8`).
const FIVE: RampCost = RampCost { light: 8, ramp: 35 };

/// One light of a slab: its band's tag, its column, its top and height,
/// and whether it is lit.
#[derive(Clone, Copy)]
struct Light {
    tag: u16,
    x: u16,
    y: u16,
    h: u16,
    lit: bool,
}

/// `n` lit one-pixel rows down one column, `gap` dark rows between each.
fn column(n: u16, gap: u16) -> Vec<Light> {
    (0..n)
        .map(|k| Light {
            tag: 2,
            x: 40,
            y: 80 + k * (gap + 1),
            h: 1,
            lit: true,
        })
        .collect()
}

/// Lay `slab` as one slab of rows, a taller light as a uniform rect.
fn lay(out: &mut impl BeamRows, slab: &[Light]) {
    out.slab();
    for (k, l) in (0u32..).zip(slab) {
        let ink = if l.lit {
            0x0010_2030 + k * 0x0001_0101
        } else {
            0
        };
        if l.h == 1 {
            out.row(0, l.tag, (l.x, l.y, 6), (ink, 0), (ink >> 1, 0));
        } else {
            out.rect(0, l.tag, (l.x, l.y, 6, l.h), (ink, 0));
        }
    }
    out.slab();
}

/// `slab` laid through a sink costed `cost`, held to the legacy stream:
/// its ramps expanded draw the legacy quads in their order and count
/// their lights.
fn fold(cost: RampCost, slab: &[Light]) -> (Vec<GlowQuad>, GlowRamps) {
    let mut legacy = Vec::new();
    lay(&mut legacy, slab);
    let (mut quads, mut ramps) = (Vec::new(), GlowRamps::default());
    lay(
        &mut RampSink::new(&mut quads, &mut ramps).costed(cost),
        slab,
    );
    let mut got = Vec::new();
    ramps.expand_into(&quads, &mut got);
    assert_eq!(got, legacy, "the stream draws the legacy quads");
    assert_eq!(ramps.laid(quads.len()), legacy.len(), "and counts them");
    assert_eq!(ramps.lit, ramps.count_lit(), "its lit count is its rows'");
    (quads, ramps)
}

/// The floor is the fewest lights that pay as a ramp: every fewer pays
/// nothing, and where a light spares nothing no run pays.
#[test]
fn the_floor_is_the_fewest_lights_that_pay() {
    for light in 0..16 {
        for ramp in 0..64 {
            let cost = RampCost { light, ramp };
            let fewest = (1..=128).find(|&n| cost.pays(n));
            let floor = cost.floor();
            assert_eq!(fewest, (floor != u32::MAX).then_some(floor), "{cost:?}");
            assert_eq!(floor == u32::MAX, light == 0, "{cost:?}");
        }
    }
    assert_eq!(FIVE.floor(), 5);
    assert_eq!(RampCost::NEVER.floor(), u32::MAX);
    assert_eq!(RampCost::ALWAYS.floor(), 1);
    // A sink says a run rides where its lights pay; the legacy stream
    // carries none.
    for cost in [FIVE, RampCost::NEVER, RampCost::ALWAYS, RAMP_COST] {
        let (mut quads, mut ramps) = (Vec::new(), GlowRamps::default());
        let sink = RampSink::new(&mut quads, &mut ramps).costed(cost);
        for n in 0..=8u8 {
            let lights = usize::from(n);
            assert_eq!(sink.rides(lights), cost.pays(n.into()), "{cost:?} {n}");
            assert!(!Vec::<GlowQuad>::new().rides(lights));
        }
    }
}

/// The measured costs ([`super::RAMP_COST`]) fold a run from four
/// lights: a sink laid without a cost of its own keeps a three-row wake
/// (classic's) as its quads and carries four rows on one ramp.
#[test]
fn the_measured_costs_keep_a_three_row_wake_as_quads() {
    for n in 1..=6 {
        let (mut quads, mut ramps) = (Vec::new(), GlowRamps::default());
        lay(&mut RampSink::new(&mut quads, &mut ramps), &column(n, 0));
        let folds = n >= 4;
        assert_eq!(ramps.items.len(), usize::from(folds), "{n} rows");
        assert_eq!(
            quads.len(),
            if folds { 0 } else { usize::from(n) },
            "{n} rows"
        );
    }
}

/// A run rides one ramp only where its lights pay for it: under a
/// five-light floor four rows stay the legacy quads and five fold whole;
/// a dark row between two lights ends the run, so each side pays or not
/// on its own.
#[test]
fn a_run_rides_a_ramp_only_where_it_pays() {
    for n in 1..=9 {
        let (quads, ramps) = fold(FIVE, &column(n, 0));
        let pays = n >= 5;
        assert_eq!(ramps.items.len(), usize::from(pays), "{n} rows");
        assert_eq!(
            quads.len(),
            if pays { 0 } else { usize::from(n) },
            "{n} rows"
        );
        let (quads, ramps) = fold(RampCost::NEVER, &column(n, 0));
        assert!(
            ramps.is_empty() && quads.len() == usize::from(n),
            "{n} rows"
        );
        let (quads, ramps) = fold(RampCost::ALWAYS, &column(n, 0));
        assert!(ramps.items.len() == 1 && quads.is_empty(), "{n} rows");
    }
    // Lights a dark row apart are runs of one light each.
    let (quads, ramps) = fold(RampCost::ALWAYS, &column(5, 1));
    assert!(ramps.items.len() == 5 && quads.is_empty());
    let (quads, ramps) = fold(FIVE, &column(8, 1));
    assert!(ramps.is_empty() && quads.len() == 8);
    // Ten rows with one missing after the sixth: six fold, four stay.
    let mut slab = column(10, 0);
    for l in &mut slab[6..] {
        l.y += 1;
    }
    let (quads, ramps) = fold(FIVE, &slab);
    assert!(ramps.items.len() == 1 && ramps.items[0].h == 6 && quads.len() == 4);
    // A dark light pays as no ramp however cheap a ramp is: it stays its
    // quad, so the stream still counts it, and the lit rows on each side
    // of it ride ramps of their own.
    let mut slab = column(3, 0);
    slab[1].lit = false;
    let (quads, ramps) = fold(RampCost::ALWAYS, &slab);
    assert!(ramps.items.len() == 2 && ramps.lit == 2, "{ramps:?}");
    assert!(
        quads.len() == 1 && quads[0].color | quads[0].color2 == 0,
        "{quads:?}"
    );
}

/// The fold the sink once ran after laying a slab, kept as this law's
/// oracle: `legacy`'s rows, laid as quads, then each maximal run of them
/// that pays under `cost` folded into one ramp where it stood.
fn fold_after_lay(cost: RampCost, legacy: &[GlowQuad]) -> (Vec<GlowQuad>, GlowRamps) {
    use aterm_core::render::glow_end;
    let ends = |q: &GlowQuad| (glow_end(q.color, q.alpha), glow_end(q.color2, q.alpha2));
    let (mut quads, mut ramps) = (Vec::new(), GlowRamps::default());
    let mut i = 0;
    while i < legacy.len() {
        let q = legacy[i];
        let (a, b) = ends(&q);
        let lit = a | b != 0;
        let mut j = i + 1;
        if lit {
            let (mut next, mut tall) = (q.y + q.h, q.h > 1);
            while let Some(n) = legacy.get(j) {
                let (na, nb) = ends(n);
                if n.row != q.row
                    || (n.x, n.w) != (q.x, q.w)
                    || n.y != next
                    || (tall && n.h > 1)
                    || na | nb == 0
                {
                    break;
                }
                next = n.y + n.h;
                tall |= n.h > 1;
                j += 1;
            }
        }
        if lit && cost.pays((j - i) as u32) {
            ramps.open_run(quads.len(), q.row, (q.x, q.y, q.w), q.h, (a, b));
            for n in &legacy[i + 1..j] {
                assert!(ramps.extend_run(0, n.h, ends(n)));
            }
        } else {
            quads.extend_from_slice(&legacy[i..j]);
        }
        i = j;
    }
    (quads, ramps)
}

/// Laying a run on its ramp as it goes lays the stream the fold after
/// laying did, ramp for ramp and quad for quad: over slabs of runs broken
/// every way a run breaks (another band or column, a gap, a climb, a
/// second tall light, a dark light), under every cost.
#[test]
fn laying_a_run_directly_lays_what_the_fold_after_laying_did() {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = |m: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % m
    };
    let (mut runs, mut ramps_laid) = (0, 0);
    for _ in 0..400 {
        let mut slab = Vec::new();
        let (mut tag, mut x, mut y) = (1u16, 40u16, 80u16);
        for _ in 0..1 + next(40) {
            match next(12) {
                0 => tag += 1,
                1 => x += 1,
                2 => y += 1,
                3 => y -= 2,
                _ => {}
            }
            let h = if next(7) == 0 { 2 + next(3) as u16 } else { 1 };
            slab.push(Light {
                tag,
                x,
                y,
                h,
                lit: next(9) != 0,
            });
            y += h;
        }
        for cost in [FIVE, RampCost::ALWAYS, RampCost::NEVER, super::RAMP_COST] {
            let mut legacy = Vec::new();
            lay(&mut legacy, &slab);
            let want = fold_after_lay(cost, &legacy);
            let got = fold(cost, &slab);
            assert_eq!(got, want, "{cost:?} over {} lights", slab.len());
            runs += got.1.items.iter().filter(|r| r.joined > 0).count();
            ramps_laid += got.1.items.len();
        }
    }
    assert!(
        runs > 0 && ramps_laid > 400,
        "{runs} runs, {ramps_laid} ramps"
    );
}

/// A run ends where its lights cannot share one ramp: another band or
/// column, a light not just below the last one, a second light taller
/// than a row, or a dark light, which stays a quad; each run then pays or
/// not on its own lights.
#[test]
fn a_run_ends_where_its_lights_cannot_share_a_ramp() {
    let at = |tag, x, y, h, lit| Light { tag, x, y, h, lit };
    let mut slab = Vec::new();
    // Six rows with one tall light among them, then a second tall light
    // and five rows after it: two runs, both paying.
    slab.extend((0..3).map(|k| at(1, 8, 10 + k, 1, true)));
    slab.push(at(1, 8, 13, 4, true));
    slab.extend((0..2).map(|k| at(1, 8, 17 + k, 1, true)));
    slab.push(at(1, 8, 19, 3, true));
    slab.extend((0..4).map(|k| at(1, 8, 22 + k, 1, true)));
    // A dark light, then four rows: the dark light a quad, the four too.
    slab.push(at(1, 8, 26, 1, false));
    slab.extend((0..4).map(|k| at(1, 8, 27 + k, 1, true)));
    // The same rows in another band, then in another column: two runs.
    slab.extend((0..5).map(|k| at(2, 8, 31 + k, 1, true)));
    slab.extend((0..5).map(|k| at(2, 9, 36 + k, 1, true)));
    // Rows climbing back up start a run of their own.
    slab.extend((0..5).map(|k| at(2, 9, 30 + k, 1, true)));
    let (quads, ramps) = fold(FIVE, &slab);
    let runs: Vec<_> = ramps
        .items
        .iter()
        .map(|r| (r.row, r.x, r.y, r.lights(&ramps.ends), r.joined))
        .collect();
    assert_eq!(
        runs,
        [
            (1, 8, 10, 6, 3),
            (1, 8, 19, 5, 2),
            (2, 8, 31, 5, 0),
            (2, 9, 36, 5, 0),
            (2, 9, 30, 5, 0),
        ],
    );
    assert_eq!(quads.len(), 5, "the dark light and the four after it");
    assert!(quads[0].color == 0 && quads[0].color2 == 0, "{quads:?}");
}
