// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    BeamClip, BeamVertex, CometSample, GlowQuad, GlowRamps, RampCost, RampSink, comet_beam,
    comet_beam_over, premul_rgb, rdp_keep_indices, rdp_simplify,
};

fn v(x: f32, y: f32, cov: u8) -> BeamVertex {
    BeamVertex {
        x,
        y,
        color: 0x0050_FA7B,
        cov,
    }
}

/// AFTER-LAND LOWS W6: A FEATHERED TUBE ROUNDS EVERY LAYER'S ENDS OFF. Laid whole to its
/// run's ends, Beam's sleeve stood as a box with hard edges as tall as it is thick (over a
/// Backspace's erased cells a white slab between lavender boxes, and on the dark Settings
/// card a lavender column with a hard edge, composed r3's judges). Fed an even run, a
/// feathered tube lays the column a pixel in from its end at under a third of the light
/// of the column a whole thickness in, at both ends. Control: `feather` 0, every layer
/// whole to the run's last pixel.
#[test]
fn a_feathered_beam_rounds_each_layer_s_ends() {
    let clip = BeamClip::grid(300, 96, 32);
    let run = [
        CometSample {
            x: 40.0,
            y: 48.0,
            cov: 255,
            pos: 0.0,
        },
        CometSample {
            x: 240.0,
            y: 48.0,
            cov: 255,
            pos: 1.0,
        },
    ];
    let core = 6.0;
    let reach = (core * 2.5) as i32;
    // The light (a light's premultiplied green, the white tube's every channel, summed down
    // each column) the tube lays in each column.
    let columns = |feather: f32| {
        let (mut scratch, mut out) = (super::BeamScratch::default(), Vec::<GlowQuad>::new());
        super::layered_beam_quads_with(
            &mut scratch,
            &mut out,
            clip,
            &run,
            (core, feather),
            0.0,
            &|_| 0x00FF_FFFF,
            &super::BEAM_LAYERS,
        );
        let mut col = vec![0u32; 300];
        for q in &out {
            let w = u32::from(q.w.max(1));
            for i in 0..w {
                let (g0, g1) = ((q.color >> 8) & 0xff, (q.color2 >> 8) & 0xff);
                let a = if q.is_flat() {
                    g0
                } else {
                    (g0 * (2 * w - (2 * i + 1)) + g1 * (2 * i + 1) + w) / (2 * w)
                };
                col[usize::from(q.x) + i as usize] += a * u32::from(q.h);
            }
        }
        col
    };
    // The end's light against the light a whole thickness in, at the tail and the head.
    let ends = |col: &[u32]| {
        let first = col
            .iter()
            .position(|&c| c > 0)
            .expect("the tube lays light") as i32;
        let last = col
            .iter()
            .rposition(|&c| c > 0)
            .expect("the tube lays light") as i32;
        let at = |x: i32| col[x as usize] as f32;
        (
            at(first + 1) / at(first + reach).max(1.0),
            at(last - 1) / at(last - reach).max(1.0),
        )
    };
    let (tail, head) = ends(&columns(1.5));
    assert!(
        tail < 0.34 && head < 0.34,
        "the tube's ends are hard: tail {tail:.2}, head {head:.2} of its light"
    );
    let (tail, head) = ends(&columns(0.0));
    assert!(
        tail >= 0.34 && head >= 0.34,
        "control: a tube laid whole to its ends ends hard (tail {tail:.2}, head {head:.2})"
    );
}

/// A comet beam laid into a ramp sink is its legacy quads: an x-major
/// slab's column rides one ramp per band where a run of three lights
/// (its edge, a core piece and its far edge) pays, its core the ramp's
/// run (each of whose rows reads the core's ends), every other piece a
/// quad, in the legacy stream's order and count; where three lights do
/// not pay (the measured costs) the slab lays only quads.
#[test]
fn a_comet_beam_laid_as_ramps_is_its_quads() {
    let clip = BeamClip::grid(320, 96, 16);
    let paths = [
        vec![v(4.0, 8.0, 40), v(150.0, 21.0, 200), v(300.0, 29.5, 120)],
        vec![v(20.0, 2.0, 200), v(28.0, 90.0, 90)],
        vec![v(4.0, 8.0, 40), v(90.0, 37.0, 200), v(140.0, 70.0, 120)],
    ];
    let three = RampCost {
        light: 30,
        ramp: 76,
    };
    assert_eq!(three.floor(), 3);
    assert_eq!(crate::glow_ramps::RAMP_COST.floor(), 4);
    for cost in [RampCost::ALWAYS, three, crate::glow_ramps::RAMP_COST] {
        let rides = cost.floor() <= 3;
        let (mut fewer, mut runs) = (0usize, 0usize);
        for path in &paths {
            for (thick, step) in [(1.0, 1), (3.0, 1), (5.5, 2), (14.0, 1)] {
                for over in [false, true] {
                    let (mut legacy, mut quads, mut ramps) =
                        (Vec::new(), Vec::new(), GlowRamps::default());
                    {
                        let mut sink = RampSink::new(&mut quads, &mut ramps).costed(cost);
                        if over {
                            comet_beam_over(&mut legacy, clip, path, thick, step, 0.0);
                            comet_beam_over(&mut sink, clip, path, thick, step, 0.0);
                        } else {
                            comet_beam(&mut legacy, clip, path, thick, step, 0.0);
                            comet_beam(&mut sink, clip, path, thick, step, 0.0);
                        }
                    }
                    let mut got = Vec::new();
                    ramps.expand_into(&quads, &mut got);
                    let what =
                        format!("{cost:?} thick {thick} step {step} over {over} path {path:?}");
                    assert_eq!(got, legacy, "{what}");
                    assert_eq!(ramps.laid(quads.len()), legacy.len(), "{what}");
                    assert_eq!(ramps.lit, ramps.count_lit(), "{what}");
                    assert!(rides || ramps.is_empty(), "{what}");
                    for r in &ramps.items {
                        let head = r.row_ends(&ramps.ends, r.join);
                        for k in r.join + 1..=r.join + r.joined {
                            assert_eq!(r.row_ends(&ramps.ends, k), head, "{what}: {r:?}");
                        }
                        runs += usize::from(r.joined > 0);
                    }
                    fewer += usize::from(ramps.items.len() + quads.len() < legacy.len());
                }
            }
        }
        assert_eq!(
            fewer > 0 && runs > 0,
            rides,
            "{cost:?}: the fixtures lay columns as ramps where three lights pay"
        );
    }
}

#[test]
fn comet_beam_over_is_the_additive_beam_painted_source_over() {
    let clip = BeamClip::grid(320, 96, 16);
    let path = [v(4.0, 8.0, 40), v(90.0, 37.0, 200), v(140.0, 70.0, 120)];
    for (thick, step, tol) in [(1.0, 1, 0.0), (5.5, 1, 0.0), (9.0, 2, 3.0)] {
        let (mut add, mut over) = (Vec::new(), Vec::new());
        comet_beam(&mut add, clip, &path, thick, step, tol);
        comet_beam_over(&mut over, clip, &path, thick, step, tol);
        assert!(!over.is_empty(), "the fixture paints");
        assert_eq!(add.len(), over.len(), "one quad per additive quad");
        for (a, o) in add.iter().zip(&over) {
            assert_eq!(
                (a.row, a.x, a.y, a.w, a.h, a.color),
                (o.row, o.x, o.y, o.w, o.h, o.color),
                "same rect, same premultiplied colour"
            );
            assert_eq!(a.alpha, 0, "the additive beam stays light");
            assert!(o.alpha > 0 && o.alpha2 == o.alpha, "paint, flat: {o:?}");
            assert_eq!(
                o.color,
                premul_rgb(path[0].color, o.alpha),
                "the colour is the ink premultiplied by the opacity: {o:?}"
            );
        }
    }
    let black = path.map(|p| BeamVertex { color: 0, ..p });
    let (mut add, mut over) = (Vec::new(), Vec::new());
    comet_beam(&mut add, clip, &black, 5.5, 1, 0.0);
    comet_beam_over(&mut over, clip, &black, 5.5, 1, 0.0);
    assert!(add.is_empty(), "additive black lays no light");
    assert!(
        !over.is_empty() && over.iter().all(|o| o.color == 0 && o.alpha > 0),
        "black ink paints source-over"
    );
}

#[test]
fn rdp_keep_indices_matches_rdp_simplify() {
    let paths: &[Vec<(f32, f32)>] = &[
        vec![],
        vec![(0.0, 0.0)],
        vec![(0.0, 0.0), (10.0, 10.0)],
        vec![(0.0, 0.0), (5.0, 0.2), (10.0, 0.0)],
        (0..40)
            .map(|i| (i as f32, (i as f32 * 0.7).sin() * 6.0))
            .collect(),
        (0..25).map(|i| (i as f32 * 2.0, i as f32 * 2.0)).collect(),
        (0..30).map(|i| (3.0, i as f32)).collect(),
    ];
    for path in paths {
        let run: Vec<CometSample> = path
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| CometSample {
                x,
                y,
                cov: (10 + i) as u8,
                pos: i as f32 / path.len().max(1) as f32,
            })
            .collect();
        let verts: Vec<BeamVertex> = run.iter().map(|s| v(s.x, s.y, s.cov)).collect();
        for &tol in &[0.25f32, 0.5, 1.0, 2.5, 5.0] {
            let kept = rdp_keep_indices(&run, tol);
            let simplified = rdp_simplify(&verts, tol);
            let from_idx: Vec<(f32, f32)> = kept.iter().map(|&i| (run[i].x, run[i].y)).collect();
            let from_rdp: Vec<(f32, f32)> = simplified.iter().map(|v| (v.x, v.y)).collect();
            assert_eq!(
                from_idx,
                from_rdp,
                "rdp_keep_indices drift: len={} tol={tol}",
                run.len()
            );
        }
    }
}

#[test]
fn quads_are_single_row_and_in_bounds() {
    let (gw, gh, ch) = (640usize, 320usize, 16usize);
    let mut out = Vec::new();
    comet_beam(
        &mut out,
        BeamClip::grid(gw, gh, ch),
        &[v(20.0, 300.0, 60), v(300.0, 20.0, 220)],
        4.0,
        1,
        0.0,
    );
    comet_beam(
        &mut out,
        BeamClip::grid(gw, gh, ch),
        &[v(10.0, 40.0, 60), v(600.0, 90.0, 220)],
        4.0,
        1,
        0.0,
    );
    assert!(!out.is_empty(), "a diagonal must emit light");
    for q in &out {
        let band = (q.row as usize) * ch;
        assert!(
            (q.y as usize) >= band && (q.y as usize) + (q.h as usize) <= band + ch,
            "quad spans >1 cell-row: {q:?}"
        );
        assert!(
            (q.x as usize) + (q.w as usize) <= gw,
            "past right edge: {q:?}"
        );
        assert!(
            (q.y as usize) + (q.h as usize) <= gh,
            "past bottom edge: {q:?}"
        );
    }
}

#[test]
fn diagonal_is_connected_and_antialiased() {
    let (gw, gh, ch) = (320usize, 320usize, 16usize);
    let mut out = Vec::new();
    comet_beam(
        &mut out,
        BeamClip::grid(gw, gh, ch),
        &[v(8.0, 8.0, 200), v(300.0, 300.0, 200)],
        3.0,
        1,
        0.0,
    );
    let rows: std::collections::BTreeSet<u16> = out.iter().map(|q: &GlowQuad| q.row).collect();
    let (lo, hi) = (
        *rows.iter().next().unwrap(),
        *rows.iter().next_back().unwrap(),
    );
    for r in lo..=hi {
        assert!(
            rows.contains(&r),
            "row {r} missing → staircase gap, not a line"
        );
    }
    assert!(hi - lo >= 15, "diagonal should span the grid's rows");
    assert!(
        out.iter().any(|q| q.w == 1),
        "expected a 1px AA edge quad somewhere"
    );
}
