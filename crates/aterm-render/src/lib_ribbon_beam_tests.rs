// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    BeamClip, GlowBlend, GlowQuad, RIBBON_CORE_SHARE, RibbonVertex, ribbon_beam, ribbon_beam_v,
    ribbon_profile,
};

const W: usize = 160;
const H: usize = 120;
const CH: usize = 20;

fn clip() -> BeamClip {
    BeamClip::grid(W, H, CH)
}

fn vert(x: f32, spine: f32, up: f32, dn: f32, color: u32, cov: f32) -> RibbonVertex {
    RibbonVertex {
        x,
        spine,
        up,
        dn,
        core_up: up * 0.2,
        core_dn: dn * 0.2,
        color,
        cov,
        lift: 0.0,
        lift_span: 0.0,
    }
}

fn paint(quads: &[GlowQuad]) -> std::collections::BTreeMap<(i32, i32), u32> {
    let mut px = std::collections::BTreeMap::new();
    for q in quads {
        for y in q.y..q.y + q.h {
            for (i, x) in (q.x..q.x + q.w).enumerate() {
                let slot = px.entry((i32::from(y), i32::from(x))).or_insert(0u32);
                let c = super::glow_lerp_rgb(q.color, q.color2, i as u32, u32::from(q.w));
                *slot = super::add_sat(*slot, c);
            }
        }
    }
    px
}

fn peak(c: u32) -> i32 {
    (((c >> 16) & 0xff).max((c >> 8) & 0xff).max(c & 0xff)) as i32
}

#[test]
fn glow_lerp_is_the_identity_on_a_flat_pair_and_a_monotone_ramp_otherwise() {
    use super::{glow_lerp, glow_lerp_rgb};
    const LEVELS: [u32; 8] = [0, 1, 7, 100, 128, 200, 254, 255];
    for w in 1..=64u32 {
        for &c in &LEVELS {
            for i in 0..w {
                assert_eq!(glow_lerp(c, c, i, w), c, "identity at {c} w={w} i={i}");
            }
        }
        for &c0 in &LEVELS {
            for &c1 in &LEVELS {
                let step = |i: u32| i64::from(glow_lerp(c0, c1, i, w));
                let (lo, hi) = (i64::from(c0.min(c1)), i64::from(c0.max(c1)));
                let delta = hi - lo;
                assert!(
                    (step(0) - i64::from(c0)).abs() * 2 * i64::from(w) <= delta + 2 * i64::from(w),
                    "left end {c0}->{c1} w={w}: c(0)={}",
                    step(0)
                );
                assert!(
                    (step(w - 1) - i64::from(c1)).abs() * 2 * i64::from(w)
                        <= delta + 2 * i64::from(w),
                    "right end {c0}->{c1} w={w}: c(w-1)={}",
                    step(w - 1)
                );
                for i in 0..w {
                    let v = step(i);
                    assert!((lo..=hi).contains(&v), "{c0}->{c1} w={w} i={i} gives {v}");
                    if i + 1 < w {
                        let n = step(i + 1);
                        if c0 <= c1 {
                            assert!(v <= n, "{c0}->{c1} w={w}: c({i})={v} > c({})={n}", i + 1);
                        } else {
                            assert!(v >= n, "{c0}->{c1} w={w}: c({i})={v} < c({})={n}", i + 1);
                        }
                    }
                }
                if w == 1 {
                    assert_eq!(
                        step(0),
                        i64::from((c0 + c1).div_ceil(2)),
                        "w=1 is the rounded mean"
                    );
                }
            }
        }
    }
    let (a, b) = (0x00FF_4020u32, 0x0020_40FFu32);
    for w in [1u32, 2, 15, 40] {
        for i in 0..w + 3 {
            let want = (glow_lerp(0xFF, 0x20, i, w) << 16)
                | (glow_lerp(0x40, 0x40, i, w) << 8)
                | glow_lerp(0x20, 0xFF, i, w);
            assert_eq!(glow_lerp_rgb(a, b, i, w), want, "w={w} i={i}");
        }
    }
    assert_eq!(glow_lerp(0, 255, 0, 15), 9);
    assert_eq!(glow_lerp(0, 255, 14, 15), 247);
}

#[test]
fn a_one_slab_per_cell_ribbon_paints_a_continuous_ramp() {
    const SLAB: usize = 15;
    let verts = [
        vert(0.0, 60.0, 12.0, 4.0, 0x00FF_0000, 200.0),
        vert(120.0, 60.0, 12.0, 4.0, 0x0000_00FF, 200.0),
    ];
    let mut quads = Vec::new();
    assert!(ribbon_beam(
        &mut quads,
        clip(),
        &verts,
        1.0,
        SLAB,
        100_000,
        crate::GlowBlend::Add
    ));
    assert!(
        quads.iter().any(|q| q.w as usize == SLAB && !q.is_flat()),
        "non-vacuous: whole-cell slabs were emitted, as gradients"
    );
    let bound = (255 + SLAB as i32 - 1) / SLAB as i32 + 1;
    let max_adjacent = |px: &std::collections::BTreeMap<(i32, i32), u32>| {
        let mut worst = 0i32;
        for (&(y, x), &c) in px {
            if let Some(&n) = px.get(&(y, x + 1)) {
                for sh in [16, 8, 0] {
                    let d = (((c >> sh) & 0xff) as i32 - ((n >> sh) & 0xff) as i32).abs();
                    worst = worst.max(d);
                }
            }
        }
        worst
    };
    let smooth = max_adjacent(&paint(&quads));
    assert!(
        smooth <= bound,
        "adjacent lit columns differ by up to {smooth} levels; the bound is {bound}"
    );
    let flat: Vec<GlowQuad> = quads
        .iter()
        .map(|q| GlowQuad {
            color2: q.color,
            alpha2: q.alpha,
            ..*q
        })
        .collect();
    let blocky = max_adjacent(&paint(&flat));
    assert!(
        blocky > bound,
        "the flat control must step by more than {bound} (measured {blocky}), or \
         the continuity bound proves nothing"
    );
    let same = [
        vert(0.0, 60.0, 12.0, 4.0, 0x0040_C0FF, 200.0),
        vert(120.0, 60.0, 12.0, 4.0, 0x0040_C0FF, 200.0),
    ];
    let mut flat_only = Vec::new();
    assert!(ribbon_beam(
        &mut flat_only,
        clip(),
        &same,
        1.0,
        SLAB,
        100_000,
        crate::GlowBlend::Add
    ));
    assert!(!flat_only.is_empty());
    assert!(
        flat_only.iter().all(|q| q.is_flat()),
        "a producer with one colour and one coverage emits flat quads"
    );
}

#[test]
fn the_ribbon_profile_is_a_clamped_monotone_curve() {
    for &(core, reach, shoulder) in &[
        (2.0f32, 20.0f32, 1.0f32),
        (2.0, 20.0, 0.55),
        (0.0, 6.0, 1.0),
        (99.0, 6.0, 0.55),
    ] {
        let p = |d: f32| ribbon_profile(d, core, reach, shoulder);
        assert!((p(0.0) - 1.0).abs() < 1e-6, "{core}/{reach}/{shoulder}");
        assert_eq!(p(reach), 0.0);
        assert_eq!(p(reach * 3.0), 0.0);
        assert_eq!(p(-reach * 0.5), p(reach * 0.5), "symmetric in |d|");
        let mut prev = f32::INFINITY;
        for i in 0..=400 {
            let v = p(reach * i as f32 / 400.0);
            assert!(v <= prev + 1e-6 && (0.0..=1.0).contains(&v));
            prev = v;
        }
        let held = reach * RIBBON_CORE_SHARE;
        assert!(p(held) < 1.0, "the plateau ends by half the reach");
    }
    for &(d, core, reach, shoulder) in &[
        (f32::NAN, 4.0f32, 8.0f32, 1.0f32),
        (1.0, f32::NAN, 8.0, 1.0),
        (1.0, 4.0, f32::NAN, 1.0),
        (1.0, 4.0, 0.0, 1.0),
        (1.0, 4.0, -8.0, 1.0),
        (f32::INFINITY, 4.0, 8.0, 1.0),
    ] {
        let v = ribbon_profile(d, core, reach, shoulder);
        assert!(v.is_finite() && (0.0..=1.0).contains(&v), "{v}");
    }
}

#[test]
fn the_ribbon_reaches_further_above_its_spine_than_below() {
    let (up, dn) = (16.0f32, 4.0f32);
    let verts = [
        vert(20.0, 60.0, up, dn, 0x00FF_0000, 200.0),
        vert(60.0, 60.0, up, dn, 0x00FF_0000, 200.0),
    ];
    let mut out = Vec::new();
    assert!(ribbon_beam(
        &mut out,
        clip(),
        &verts,
        1.0,
        4,
        100_000,
        crate::GlowBlend::Add
    ));
    let px = paint(&out);
    let col: Vec<(i32, i32)> = px
        .iter()
        .filter(|((_, x), _)| *x == 40)
        .map(|(&(y, _), &c)| (y, peak(c)))
        .collect();
    assert!(!col.is_empty(), "the beam paints");
    let top = col.first().expect("lit").0;
    let bot = col.last().expect("lit").0;
    assert!(
        (60 - top) > (bot - 60) * 2,
        "the mark reaches further up ({}) than down ({})",
        60 - top,
        bot - 60
    );
    assert!(top >= (60.0 - up) as i32, "…and never past its own reach");
    assert!(bot <= (60.0 + dn).ceil() as i32);
    let crest = col.iter().max_by_key(|&&(_, c)| c).expect("lit").0;
    assert!((crest - 60).abs() <= 1, "the crest is the spine: {crest}");
    assert!(
        col.windows(2)
            .filter(|w| w[1].0 <= crest)
            .all(|w| w[1].1 >= w[0].1)
    );
    assert!(
        col.windows(2)
            .filter(|w| w[0].0 >= crest)
            .all(|w| w[1].1 <= w[0].1)
    );
}

#[test]
fn fractional_slab_vertices_own_each_column_exactly_once() {
    const CW: f32 = 14.0;
    let mut verts = Vec::new();
    for cell in 0..3 {
        let x0 = 10.0 + cell as f32 * CW;
        for j in 0..3 {
            verts.push(vert(
                x0 + j as f32 * CW / 3.0,
                60.0,
                8.0,
                4.0,
                0x00FF_E93C,
                150.0,
            ));
        }
    }
    verts.push(vert(10.0 + 3.0 * CW, 60.0, 8.0, 4.0, 0x00FF_E93C, 150.0));
    let mut out = Vec::new();
    assert!(ribbon_beam(
        &mut out,
        clip(),
        &verts,
        1.0,
        4,
        100_000,
        GlowBlend::Over
    ));
    let mut owners = std::collections::BTreeMap::new();
    for q in &out {
        for y in q.y..q.y + q.h {
            for x in q.x..q.x + q.w {
                *owners.entry((y, x)).or_insert(0u32) += 1;
            }
        }
    }
    assert!(!owners.is_empty(), "the beam paints");
    let doubled: Vec<_> = owners.iter().filter(|&(_, &n)| n > 1).collect();
    assert!(
        doubled.is_empty(),
        "a pixel is owned by two slabs (the comb): {doubled:?}"
    );
    let lit: std::collections::BTreeSet<u16> = out
        .iter()
        .filter(|q| (q.y..q.y + q.h).contains(&60))
        .flat_map(|q| q.x..q.x + q.w)
        .collect();
    for x in 10..(10 + 3 * 14) {
        assert!(lit.contains(&(x as u16)), "column {x} has no owner (a gap)");
    }
}

#[test]
fn ribbon_rows_keep_clip_and_grid_band_tags() {
    let clip = BeamClip {
        x0: 5,
        y0: 2,
        x1: 27,
        y1: 24,
        cell_h: 7,
        origin_y: 10,
    };
    let verts = [
        vert(0.0, 10.0, 12.0, 12.0, 0x00ff_8040, 180.0),
        vert(32.0, 10.0, 12.0, 12.0, 0x00ff_8040, 180.0),
    ];
    let mut out = Vec::new();
    assert!(ribbon_beam(
        &mut out,
        clip,
        &verts,
        1.0,
        4,
        100_000,
        GlowBlend::Add,
    ));
    assert!(!out.is_empty());
    assert!(out.iter().all(|q| {
        q.h == 1
            && i32::from(q.x) >= clip.x0
            && i32::from(q.x + q.w) <= clip.x1
            && i32::from(q.y) >= clip.y0
            && i32::from(q.y + q.h) <= clip.y1
            && q.row
                == (i32::from(q.y) - clip.origin_y)
                    .div_euclid(clip.cell_h)
                    .max(0) as u16
    }));
    assert!(
        out.iter().any(|q| q.row == 0) && out.iter().any(|q| q.row == 1),
        "the witness must cross the grid origin and a band boundary"
    );
}

#[test]
fn a_sub_pixel_spine_moves_the_edge_continuously() {
    let sample = |spine: f32| -> i64 {
        let verts = [
            vert(20.0, spine, 12.0, 4.0, 0x00FF_FFFF, 200.0),
            vert(60.0, spine, 12.0, 4.0, 0x00FF_FFFF, 200.0),
        ];
        let mut out = Vec::new();
        assert!(ribbon_beam(
            &mut out,
            clip(),
            &verts,
            1.0,
            4,
            100_000,
            crate::GlowBlend::Add
        ));
        paint(&out)
            .iter()
            .filter(|((_, x), _)| *x == 40)
            .map(|(_, &c)| i64::from(peak(c)))
            .sum()
    };
    let track: Vec<i64> = (0..=10).map(|i| sample(60.0 + i as f32 * 0.1)).collect();
    let steps: Vec<i64> = track.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
    assert!(
        steps.iter().any(|&s| s > 0),
        "a sub-pixel spine must move the raster at all: {track:?}"
    );
    let biggest = *steps.iter().max().expect("steps");
    let whole = (sample(60.0) - sample(61.0)).abs().max(1);
    assert!(
        biggest <= whole.max(24),
        "a tenth-pixel spine move stepped {biggest}, which is a staircase, \
         not a slide: {track:?}"
    );
}

#[test]
fn the_ordered_dither_tracks_a_fractional_request() {
    let mean_level = |cov: f32| -> f64 {
        let verts = [
            RibbonVertex {
                x: 8.0,
                spine: 60.0,
                up: 20.0,
                dn: 20.0,
                core_up: 8.0,
                core_dn: 8.0,
                color: 0x00FF_FFFF,
                cov,
                lift: 0.0,
                lift_span: 0.0,
            },
            RibbonVertex {
                x: 120.0,
                ..RibbonVertex {
                    x: 0.0,
                    spine: 60.0,
                    up: 20.0,
                    dn: 20.0,
                    core_up: 8.0,
                    core_dn: 8.0,
                    color: 0x00FF_FFFF,
                    cov,
                    lift: 0.0,
                    lift_span: 0.0,
                }
            },
        ];
        let mut out = Vec::new();
        assert!(ribbon_beam(
            &mut out,
            clip(),
            &verts,
            1.0,
            4,
            100_000,
            crate::GlowBlend::Add
        ));
        let vals: Vec<f64> = paint(&out)
            .iter()
            .filter(|((y, _), _)| (*y - 60).abs() < 6)
            .map(|(_, &c)| f64::from(peak(c)))
            .collect();
        assert!(vals.len() > 400, "the plateau must be wide: {}", vals.len());
        vals.iter().sum::<f64>() / vals.len() as f64
    };
    let base = 20.0f32;
    let sweep: Vec<f64> = (0..=20)
        .map(|i| mean_level(base + i as f32 / 20.0))
        .collect();
    for (i, &m) in sweep.iter().enumerate() {
        let want = f64::from(base) + i as f64 / 20.0;
        assert!(
            (m - want).abs() <= 0.2,
            "request {want} emitted a mean of {m}"
        );
    }
    assert!(
        sweep.windows(2).all(|w| w[1] >= w[0] - 1e-9),
        "…monotonically: {sweep:?}"
    );
    let distinct = sweep
        .iter()
        .map(|m| (m * 100.0).round() as i64)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    assert!(
        distinct >= 15,
        "a dithered sweep resolves the fraction: {distinct} distinct means"
    );
    let truncated = (0..=20)
        .map(|i| (base + i as f32 / 20.0) as u8)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    assert!(
        truncated <= 2 && truncated * 7 < distinct,
        "the retired truncation must collapse the same sweep to the two \
         integers it touches: {truncated} values against the dither's {distinct}"
    );
    for cap in [57.0f32, 108.0, 255.0] {
        let m = mean_level(cap);
        assert!(
            m <= f64::from(cap),
            "a capped request must not be dithered past its cap: {m} > {cap}"
        );
    }
}

#[test]
fn a_near_cap_reversed_segment_keeps_the_slab_next_to_its_head() {
    const BUDGET: usize = 10_240;
    const STEP: usize = 10;
    let clip = BeamClip::grid(4_320, 3_360, 56);
    let verts = [
        vert(90.0, 150.0, 36.0, 24.0, 0x00FF_8040, 200.0),
        vert(60.0, 150.0, 36.0, 24.0, 0x00FF_8040, 200.0),
    ];
    type Extent = fn(&GlowQuad) -> (u16, u16);
    let beams: [(&str, Beam, Extent); 2] = [
        ("x-major", ribbon_beam, |q| (q.x, q.w)),
        ("y-major", ribbon_beam_v, |q| (q.y, q.h)),
    ];
    for (name, beam, extent) in beams {
        let mut full = Vec::new();
        assert!(beam(
            &mut full,
            clip,
            &verts,
            1.0,
            STEP,
            BUDGET,
            GlowBlend::Over,
        ));
        let head_slab = full.iter().filter(|q| extent(q).0 == 80).count();
        assert!(
            head_slab > 30,
            "fixture must paint a Retina-sized slab ({name})"
        );
        let already_admitted = BUDGET - head_slab - 1;
        let mut near_cap = vec![GlowQuad::default(); already_admitted];
        assert!(
            !beam(
                &mut near_cap,
                clip,
                &verts,
                1.0,
                STEP,
                BUDGET,
                GlowBlend::Over,
            ),
            "fixture must saturate ({name})"
        );
        let kept = &near_cap[already_admitted..];
        assert!(near_cap.len() <= BUDGET);
        assert!(
            kept.iter().any(|q| {
                let (p, len) = extent(q);
                p <= 85 && 85 < p + len
            }),
            "the slab next to the head at 90 went black ({name})"
        );
        assert!(
            kept.iter().all(|q| {
                let (p, len) = extent(q);
                !(p <= 65 && 65 < p + len)
            }),
            "a capped frame kept the far slab before the head ({name})"
        );
    }
}

#[test]
fn a_reversed_walk_keeps_the_uncapped_slab_partition_bit_identical() {
    let clip = BeamClip::grid(4_320, 3_360, 56);
    let ascending = [
        vert(60.5, 150.0, 36.0, 24.0, 0x00FF_8040, 200.0),
        vert(91.5, 150.0, 36.0, 24.0, 0x00FF_8040, 200.0),
    ];
    let descending = [ascending[1], ascending[0]];
    type Ordered = fn(
        &mut Vec<GlowQuad>,
        BeamClip,
        &[RibbonVertex],
        f32,
        usize,
        usize,
        GlowBlend,
        bool,
    ) -> bool;
    let beams: [(&str, Beam, Ordered, Major); 2] = [
        ("x-major", ribbon_beam, super::ribbon_beam_ordered, |q| q.x),
        (
            "y-major",
            ribbon_beam_v,
            super::ribbon_beam_v_ordered,
            |q| q.y,
        ),
    ];
    for (name, beam, ordered, major) in beams {
        let mut forward = Vec::new();
        let mut head_first = Vec::new();
        assert!(beam(
            &mut forward,
            clip,
            &ascending,
            1.0,
            10,
            10_240,
            GlowBlend::Over,
        ));
        assert!(ordered(
            &mut head_first,
            clip,
            &descending,
            1.0,
            10,
            10_240,
            GlowBlend::Over,
            true,
        ));
        assert!(
            forward.len() > 100,
            "fixture must paint a real mark ({name})"
        );
        assert!(
            head_first.windows(2).all(|w| major(&w[0]) >= major(&w[1]))
                && major(&head_first[0]) > major(&head_first[head_first.len() - 1]),
            "the retry did not walk the reversed segment head-first ({name})"
        );
        let key = |q: &GlowQuad| {
            (
                q.row, q.x, q.y, q.w, q.h, q.color, q.alpha, q.color2, q.alpha2,
            )
        };
        forward.sort_unstable_by_key(key);
        head_first.sort_unstable_by_key(key);
        assert_eq!(
            forward, head_first,
            "the head-first retry moved pixels ({name})"
        );
    }
}

#[test]
fn the_two_ribbon_axes_are_exact_transposes_of_each_other() {
    let clip = BeamClip {
        x0: 0,
        y0: 0,
        x1: 128,
        y1: 128,
        cell_h: 128,
        origin_y: 0,
    };
    let verts = [
        vert(20.0, 20.0, 14.0, 5.0, 0x00FF_2010, 210.0),
        vert(56.5, 56.5, 9.0, 3.5, 0x0020_FF40, 150.0),
        vert(96.0, 96.0, 5.0, 2.0, 0x0010_30FF, 96.0),
    ];
    let lifted: Vec<RibbonVertex> = verts
        .iter()
        .map(|&v| RibbonVertex {
            lift: 40.0,
            lift_span: 3.0,
            ..v
        })
        .collect();
    for path in [&verts[..], &lifted[..]] {
        for blend in [GlowBlend::Add, GlowBlend::Over] {
            let mut major_x = Vec::new();
            let mut major_y = Vec::new();
            assert!(ribbon_beam(
                &mut major_x,
                clip,
                path,
                0.55,
                4,
                100_000,
                blend
            ));
            assert!(ribbon_beam_v(
                &mut major_y,
                clip,
                path,
                0.55,
                4,
                100_000,
                blend
            ));
            assert!(!major_x.is_empty(), "the witness must actually paint");
            assert_eq!(
                major_x.len(),
                major_y.len(),
                "the transpose emits quad for quad ({blend:?})"
            );
            assert!(major_x.iter().all(|q| q.h == 1));
            assert!(major_y.iter().all(|q| q.w == 1));
            assert_transposed(&major_x, &major_y, &format!("{blend:?}"));
        }
    }
    let plain = [
        vert(20.0, 20.0, 14.0, 5.0, 0x0040_C0FF, 180.0),
        vert(56.5, 56.5, 14.0, 5.0, 0x0040_C0FF, 180.0),
        vert(96.0, 96.0, 14.0, 5.0, 0x0040_C0FF, 180.0),
    ];
    for blend in [GlowBlend::Add, GlowBlend::Over] {
        let mut major_x = Vec::new();
        let mut major_y = Vec::new();
        assert!(ribbon_beam(
            &mut major_x,
            clip,
            &plain,
            0.55,
            4,
            100_000,
            blend
        ));
        assert!(ribbon_beam_v(
            &mut major_y,
            clip,
            &plain,
            0.55,
            4,
            100_000,
            blend
        ));
        assert!(!major_x.is_empty() && major_x.iter().all(|q| q.is_flat()));
        let mut mirrored: Vec<(u16, u16, u16, u16, u32, u8)> = major_x
            .iter()
            .map(|q| (q.y, q.x, q.h, q.w, q.color, q.alpha))
            .collect();
        let mut got: Vec<(u16, u16, u16, u16, u32, u8)> = major_y
            .iter()
            .map(|q| (q.x, q.y, q.w, q.h, q.color, q.alpha))
            .collect();
        mirrored.sort_unstable();
        got.sort_unstable();
        assert_eq!(
            mirrored, got,
            "on a single-colour path the twins are exact transposes ({blend:?})"
        );
    }
}

const TRANSPOSE_CENTRE_TOL: i32 = 2;

fn assert_transposed(major_x: &[GlowQuad], major_y: &[GlowQuad], label: &str) {
    use std::collections::BTreeMap;
    let mut mirrored: BTreeMap<(u16, u16, u16, u16), &GlowQuad> = BTreeMap::new();
    for q in major_x {
        assert!(
            mirrored.insert((q.y, q.x, q.h, q.w), q).is_none(),
            "the x-major set emits one quad per (row, slab) ({label})"
        );
    }
    let mut got: BTreeMap<(u16, u16, u16, u16), &GlowQuad> = BTreeMap::new();
    for q in major_y {
        assert!(
            got.insert((q.x, q.y, q.w, q.h), q).is_none(),
            "the y-major set emits one quad per (column, slab) ({label})"
        );
    }
    assert_eq!(
        mirrored.keys().collect::<Vec<_>>(),
        got.keys().collect::<Vec<_>>(),
        "the y-major twin is not the geometric transpose of the x-major one ({label})"
    );
    let mut worst = 0i32;
    for (key, x) in &mirrored {
        let y = got[key];
        let w = u32::from(x.w);
        let centre = |c0: u32, c1: u32| -> i32 {
            let (lo, hi) = ((w - 1) / 2, w / 2);
            (super::glow_lerp(c0, c1, lo, w) + super::glow_lerp(c0, c1, hi, w)).div_ceil(2) as i32
        };
        if x.is_flat() {
            assert_eq!(
                (x.color, x.alpha),
                (y.color, y.alpha),
                "a flat x-major quad transposes bit for bit ({label}, {key:?})"
            );
            continue;
        }
        let mut check = |name: &str, c0: u32, c1: u32, twin: u32| {
            let d = (centre(c0, c1) - twin as i32).abs();
            worst = worst.max(d);
            assert!(
                d <= TRANSPOSE_CENTRE_TOL,
                "{label}, {key:?}: {name} reads {twin} on the y-major twin but the \
                 x-major ramp {c0}->{c1} over {w} px is {} at its centre",
                centre(c0, c1)
            );
        };
        for (name, sh) in [("r", 16u32), ("g", 8), ("b", 0)] {
            check(
                name,
                (x.color >> sh) & 0xff,
                (x.color2 >> sh) & 0xff,
                (y.color >> sh) & 0xff,
            );
        }
        check(
            "alpha",
            u32::from(x.alpha),
            u32::from(x.alpha2),
            u32::from(y.alpha),
        );
    }
    eprintln!("transpose {label}: y-major flat vs x-major centre, worst {worst} level(s)");
}

#[test]
fn a_vertical_path_paints_only_through_the_transposed_twin() {
    let path = [(64.0f32, 24.0f32), (64.0, 60.0), (64.0, 96.0)];
    let flat: Vec<RibbonVertex> = path
        .iter()
        .map(|&(px, py)| vert(px, py, 9.0, 4.0, 0x00FF_C020, 200.0))
        .collect();
    let mut none = Vec::new();
    assert!(ribbon_beam(
        &mut none,
        clip(),
        &flat,
        1.0,
        4,
        100_000,
        GlowBlend::Over
    ));
    assert!(
        none.is_empty(),
        "the x-major primitive draws NOTHING on a vertical path — the \
         defect the twin exists for — but emitted {} quads",
        none.len()
    );
    let turned: Vec<RibbonVertex> = path
        .iter()
        .map(|&(px, py)| vert(py, px, 9.0, 4.0, 0x00FF_C020, 200.0))
        .collect();
    let mut out = Vec::new();
    assert!(ribbon_beam_v(
        &mut out,
        clip(),
        &turned,
        1.0,
        4,
        100_000,
        GlowBlend::Over
    ));
    assert!(!out.is_empty(), "the twin paints the same path");
    assert!(out.iter().all(|q| q.w == 1));
    let cols: std::collections::BTreeSet<u16> = out.iter().map(|q| q.x).collect();
    assert!(
        cols.iter().all(|&x| (55..=68).contains(&x)),
        "the mark keeps to its own transverse reach: {cols:?}"
    );
    assert!(cols.contains(&64), "…and the spine itself is lit");
    let lit: std::collections::BTreeSet<u16> = out.iter().flat_map(|q| q.y..q.y + q.h).collect();
    for y in 24u16..96 {
        assert!(
            lit.contains(&y),
            "the flight is continuous down its major axis; row {y} is dark"
        );
    }
}

#[test]
fn the_transposed_twin_clips_exactly_as_its_x_major_twin_does() {
    let clip = BeamClip {
        x0: 24,
        y0: 24,
        x1: 100,
        y1: 100,
        cell_h: 100,
        origin_y: 0,
    };
    let verts = [
        vert(-30.0, 30.0, 36.0, 30.0, 0x00FF_4020, 220.0),
        vert(150.0, 100.0, 30.0, 26.0, 0x0040_20FF, 150.0),
    ];
    let mut major_x = Vec::new();
    let mut major_y = Vec::new();
    assert!(ribbon_beam(
        &mut major_x,
        clip,
        &verts,
        0.55,
        4,
        100_000,
        GlowBlend::Over
    ));
    assert!(ribbon_beam_v(
        &mut major_y,
        clip,
        &verts,
        0.55,
        4,
        100_000,
        GlowBlend::Over
    ));
    assert!(
        !major_x.is_empty() && !major_y.is_empty(),
        "the witness must survive the clip with something to compare"
    );
    let inside = |q: &GlowQuad| {
        i32::from(q.x) >= clip.x0
            && i32::from(q.x + q.w) <= clip.x1
            && i32::from(q.y) >= clip.y0
            && i32::from(q.y + q.h) <= clip.y1
    };
    assert!(major_x.iter().all(inside));
    assert!(
        major_y.iter().all(inside),
        "the transposed twin let a quad out of the effects box"
    );
    assert!(
        major_y.iter().map(|q| q.x).min() == Some(clip.x0 as u16)
            && major_y.iter().map(|q| q.y).min() == Some(clip.y0 as u16),
        "the witness must actually reach the near edges it is clipped at"
    );
    assert_transposed(&major_x, &major_y, "clip");
}

#[test]
fn a_transposed_column_is_split_at_every_cell_row_boundary() {
    let clip = BeamClip {
        x0: 5,
        y0: 2,
        x1: 40,
        y1: 60,
        cell_h: 7,
        origin_y: 10,
    };
    let verts = [
        vert(0.0, 20.0, 9.0, 5.0, 0x00ff_8040, 190.0),
        vert(64.0, 20.0, 9.0, 5.0, 0x00ff_8040, 190.0),
    ];
    let mut out = Vec::new();
    assert!(ribbon_beam_v(
        &mut out,
        clip,
        &verts,
        1.0,
        5,
        100_000,
        GlowBlend::Add
    ));
    assert!(!out.is_empty());
    assert!(out.iter().all(|q| {
        let first = (i32::from(q.y) - clip.origin_y).div_euclid(clip.cell_h);
        let last = (i32::from(q.y + q.h) - 1 - clip.origin_y).div_euclid(clip.cell_h);
        q.w == 1
            && q.h >= 1
            && i32::from(q.x) >= clip.x0
            && i32::from(q.x + q.w) <= clip.x1
            && i32::from(q.y) >= clip.y0
            && i32::from(q.y + q.h) <= clip.y1
            && first == last
            && q.row == first.max(0) as u16
    }));
    assert!(
        out.iter().any(|q| q.row == 0) && out.iter().any(|q| q.row > 1),
        "the witness must cross the grid origin and several band boundaries"
    );
    assert!(
        out.iter().any(|q| q.h < 5),
        "…and at least one column must actually have been SPLIT short of \
         its stride"
    );
    let mut owners = std::collections::BTreeMap::new();
    for q in &out {
        for y in q.y..q.y + q.h {
            *owners.entry((y, q.x)).or_insert(0u32) += 1;
        }
    }
    let doubled: Vec<_> = owners.iter().filter(|&(_, &n)| n > 1).collect();
    assert!(
        doubled.is_empty(),
        "a pixel is owned by two pieces (a doubled band edge): {doubled:?}"
    );
    let transposed = BeamClip {
        x0: clip.y0,
        y0: clip.x0,
        x1: clip.y1,
        y1: clip.x1,
        cell_h: clip.cell_h,
        origin_y: clip.origin_y,
    };
    let mut flat = Vec::new();
    assert!(ribbon_beam(
        &mut flat,
        transposed,
        &verts,
        1.0,
        5,
        100_000,
        GlowBlend::Add
    ));
    assert!(flat.iter().all(|q| q.h == 1));
    let mirrored: std::collections::BTreeMap<(i32, i32), u32> = paint(&flat)
        .into_iter()
        .map(|((y, x), c)| ((x, y), c))
        .collect();
    assert_eq!(
        mirrored,
        paint(&out),
        "the band-split pieces do not re-tile the unsplit column"
    );
}

type Beam = fn(&mut Vec<GlowQuad>, BeamClip, &[RibbonVertex], f32, usize, usize, GlowBlend) -> bool;
type Major = fn(&GlowQuad) -> u16;

#[test]
fn a_reversed_non_dyadic_polyline_is_the_same_mark_to_within_a_level() {
    use std::collections::{BTreeMap, BTreeSet};
    let forward = [
        vert(0.3, 40.3, 14.3, 5.7, 0x00FF_2010, 151.5),
        vert(31.7, 52.9, 9.1, 3.3, 0x0020_FF40, 113.1),
        vert(63.1, 60.7, 5.3, 2.1, 0x0010_30FF, 96.1),
    ];
    let lifted: Vec<RibbonVertex> = forward
        .iter()
        .map(|&v| RibbonVertex {
            lift: 40.3,
            lift_span: 3.1,
            ..v
        })
        .collect();
    fn pixels(quads: &[GlowQuad], name: &str) -> BTreeMap<(u16, u16), (u32, u8)> {
        let mut map = BTreeMap::new();
        for q in quads {
            for x in q.x..q.x + q.w {
                for y in q.y..q.y + q.h {
                    assert!(
                        map.insert((x, y), (q.color, q.alpha)).is_none(),
                        "pixel ({x}, {y}) emitted twice ({name})"
                    );
                }
            }
        }
        map
    }
    let beams: [(&str, Beam); 2] = [("x-major", ribbon_beam), ("y-major", ribbon_beam_v)];
    let mut differing = 0usize;
    for path in [&forward[..], &lifted[..]] {
        let reversed: Vec<RibbonVertex> = path.iter().rev().copied().collect();
        for blend in [GlowBlend::Add, GlowBlend::Over] {
            for (name, beam) in beams {
                let mut fwd = Vec::new();
                let mut rev = Vec::new();
                assert!(beam(&mut fwd, clip(), path, 0.55, 4, 100_000, blend));
                assert!(beam(&mut rev, clip(), &reversed, 0.55, 4, 100_000, blend));
                let (a, b) = (pixels(&fwd, name), pixels(&rev, name));
                assert!(a.len() > 500, "the witness must paint a real mark ({name})");
                let keys: BTreeSet<(u16, u16)> = a.keys().chain(b.keys()).copied().collect();
                for k in keys {
                    let (ca, aa) = a.get(&k).copied().unwrap_or((0, 0));
                    let (cb, ab) = b.get(&k).copied().unwrap_or((0, 0));
                    differing += usize::from((ca, aa) != (cb, ab));
                    assert!(
                        aa.abs_diff(ab) <= 1,
                        "pixel {k:?}: coverage {aa} vs {ab} reversed ({name}, {blend:?})"
                    );
                    for sh in [16, 8, 0] {
                        let (u, v) = ((ca >> sh) & 0xff, (cb >> sh) & 0xff);
                        assert!(
                            u.abs_diff(v) <= 2,
                            "pixel {k:?}: channel {sh} {u} vs {v} reversed ({name}, {blend:?})"
                        );
                    }
                }
            }
        }
    }
    assert!(
        differing >= 1,
        "the witness must round differently somewhere, or the bound pins nothing"
    );
}

#[test]
fn the_dither_is_anchored_to_the_screen_not_the_mark() {
    use std::collections::{BTreeMap, BTreeSet};
    const STEP: usize = 4;
    let beams: [(&str, Beam, bool); 2] = [
        ("x-major", ribbon_beam, true),
        ("y-major", ribbon_beam_v, false),
    ];
    for (name, beam, x_major) in beams {
        let pixels = |at: f32, by: u16| -> BTreeMap<(u16, u16), (u32, u8)> {
            let verts = [
                vert(at, 40.0, 9.0, 5.0, 0x00FF_8040, 190.3),
                vert(at + 32.0, 52.0, 9.0, 5.0, 0x0020_FF40, 150.7),
            ];
            let mut quads = Vec::new();
            assert!(beam(
                &mut quads,
                clip(),
                &verts,
                0.55,
                STEP,
                100_000,
                GlowBlend::Over
            ));
            let mut map = BTreeMap::new();
            for q in &quads {
                for x in q.x..q.x + q.w {
                    for y in q.y..q.y + q.h {
                        let key = if x_major { (x - by, y) } else { (x, y - by) };
                        assert!(
                            map.insert(key, (q.color, q.alpha)).is_none(),
                            "pixel {key:?} emitted twice ({name})"
                        );
                    }
                }
            }
            map
        };
        let (here, one_slab, four_slabs) = (pixels(8.0, 0), pixels(12.0, 4), pixels(24.0, 16));
        assert!(
            here.len() > 200,
            "the witness must paint a real mark ({name})"
        );
        assert_eq!(
            four_slabs, here,
            "four slabs on is a pure translation ({name})"
        );
        let keys: BTreeSet<(u16, u16)> = here.keys().chain(one_slab.keys()).copied().collect();
        let mut changed = 0usize;
        for k in keys {
            let (ca, aa) = here.get(&k).copied().unwrap_or((0, 0));
            let (cb, ab) = one_slab.get(&k).copied().unwrap_or((0, 0));
            changed += usize::from((ca, aa) != (cb, ab));
            assert!(
                aa.abs_diff(ab) <= 1,
                "pixel {k:?}: a Bayer cell moves a byte by at most one level, not {aa} → {ab} ({name})"
            );
        }
        assert!(
            changed > 0,
            "one slab on must meet a different matrix column somewhere ({name})"
        );
    }
}

#[test]
fn a_saturated_budget_on_a_downward_flight_sheds_by_whole_segments() {
    const SEG: i32 = 16;
    let verts: Vec<RibbonVertex> = (0..=4)
        .rev()
        .map(|k| vert((k * SEG) as f32, 20.0, 9.0, 5.0, 0x00FF_8040, 190.0))
        .collect();
    let mut full = Vec::new();
    assert!(ribbon_beam_v(
        &mut full,
        clip(),
        &verts,
        1.0,
        4,
        100_000,
        GlowBlend::Add
    ));
    let mut head_first = Vec::new();
    assert!(super::ribbon_beam_v_ordered(
        &mut head_first,
        clip(),
        &verts,
        1.0,
        4,
        100_000,
        GlowBlend::Add,
        true,
    ));
    assert_eq!(head_first.len(), full.len());
    let mut inside_a_segment = 0;
    for cap in 1..full.len() {
        let mut cut = Vec::new();
        assert!(
            !ribbon_beam_v(&mut cut, clip(), &verts, 1.0, 4, cap, GlowBlend::Add),
            "cap {cap}: a saturated budget reports it"
        );
        assert!(cut.len() <= cap && cut.len() < full.len());
        assert_eq!(
            cut.as_slice(),
            &head_first[..cut.len()],
            "cap {cap}: not a head-first prefix"
        );
        let lit: std::collections::BTreeSet<i32> = cut
            .iter()
            .flat_map(|q| i32::from(q.y)..i32::from(q.y + q.h))
            .collect();
        let mut beyond = false;
        for k in (0..4).rev() {
            let rows: Vec<i32> = lit.range(k * SEG..(k + 1) * SEG).copied().collect();
            let n = rows.len() as i32;
            if beyond {
                assert_eq!(
                    n, 0,
                    "cap {cap}: light beyond the segment the budget ran out in: {rows:?}"
                );
            } else if n < SEG {
                beyond = true;
                if let (Some(&lo), Some(&hi)) = (rows.first(), rows.last()) {
                    assert_eq!(
                        n,
                        hi - lo + 1,
                        "cap {cap}: the cut segment's survivors are not one run: {rows:?}"
                    );
                    assert_eq!(
                        hi,
                        (k + 1) * SEG - 1,
                        "cap {cap}: the partial segment must reach its head: {rows:?}"
                    );
                    inside_a_segment += 1;
                }
            }
        }
    }
    assert!(
        inside_a_segment > 0,
        "the sweep must land inside a segment at least once"
    );
}

#[test]
fn a_saturated_budget_sheds_whole_columns_on_the_transposed_twin() {
    let clip = BeamClip {
        x0: 5,
        y0: 2,
        x1: 40,
        y1: 60,
        cell_h: 7,
        origin_y: 10,
    };
    let verts = [
        vert(0.0, 20.0, 12.0, 6.0, 0x00ff_8040, 190.0),
        vert(64.0, 20.0, 12.0, 6.0, 0x00ff_8040, 190.0),
    ];
    let mut full = Vec::new();
    assert!(ribbon_beam_v(
        &mut full,
        clip,
        &verts,
        1.0,
        16,
        100_000,
        GlowBlend::Add
    ));
    let mut bounds: Vec<usize> = (0..full.len())
        .filter(|&i| {
            i == 0 || full[i].x != full[i - 1].x || full[i].y != full[i - 1].y + full[i - 1].h
        })
        .collect();
    bounds.push(full.len());
    assert!(
        bounds.windows(2).any(|w| w[1] - w[0] >= 4),
        "the witness must have a column split into four bands"
    );
    let mut split_at_cut = 0;
    for cap in 1..full.len() {
        let mut cut = Vec::new();
        assert!(
            !ribbon_beam_v(&mut cut, clip, &verts, 1.0, 16, cap, GlowBlend::Add),
            "cap {cap}: a saturated budget reports it"
        );
        assert!(
            cut.len() <= cap,
            "cap {cap}: emitted {} — the cap is a ceiling, not a hint",
            cut.len()
        );
        assert_eq!(
            cut.as_slice(),
            &full[..cut.len()],
            "cap {cap}: not a prefix"
        );
        if cut.is_empty() {
            continue;
        }
        let (i, &end) = bounds
            .iter()
            .enumerate()
            .find(|&(_, &b)| b >= cut.len())
            .expect("the cut ends inside the full run");
        assert_eq!(
            end,
            cut.len(),
            "cap {cap}: the last column was cut mid-way through its pieces"
        );
        if end - bounds[i - 1] > 1 {
            split_at_cut += 1;
        }
    }
    assert!(
        split_at_cut > 0,
        "the sweep must end on a band-split column at least once"
    );
    let mut none = Vec::new();
    assert!(ribbon_beam_v(
        &mut none,
        clip,
        &verts[..1],
        1.0,
        16,
        100_000,
        GlowBlend::Add
    ));
    assert!(ribbon_beam_v(
        &mut none,
        clip,
        &[],
        1.0,
        16,
        100_000,
        GlowBlend::Add
    ));
    assert!(ribbon_beam_v(
        &mut none,
        BeamClip { cell_h: 0, ..clip },
        &verts,
        1.0,
        16,
        100_000,
        GlowBlend::Add
    ));
    assert!(none.is_empty());
}
