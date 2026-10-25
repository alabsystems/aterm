// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The merged marks' laws: they follow the trend, an absence is dashed, the slats stop at ten,
//! clips balance, every mark stays in its rect, and each keeps to its prim budget.

use aterm_vitals::ink::VitalsInks;

use super::*;

fn dark() -> VitalsInks {
    aterm_vitals::ink::vitals_inks([0x30, 0x31, 0x35], [0xF8, 0xF8, 0xF2], [0x50, 0xFA, 0x7B])
}

fn light() -> VitalsInks {
    aterm_vitals::ink::vitals_inks([0xE2, 0xE3, 0xE5], [0x24, 0x29, 0x2F], [0x1A, 0x7F, 0x37])
}

/// The prims `draw` pushes on a fresh painter at scale `s`.
fn drawn(inks: &VitalsInks, s: f32, draw: impl FnOnce(&mut Marks<'_, '_>)) -> Vec<DrawPrim> {
    let mut p = Painter::new(inks, s, 13.0 * s, false);
    draw(&mut Marks::new(&mut p, false));
    p.prims
}

fn trend(level: f32, slow: f32, mad: f32) -> TrendView {
    TrendView {
        raw: level,
        level,
        slow,
        mad,
    }
}

const GATE: f32 = 0.02;

/// The panels inked `c`, as `(y, h)`.
fn panels_in(prims: &[DrawPrim], c: [u8; 4]) -> Vec<(f32, f32)> {
    prims
        .iter()
        .filter_map(|p| match *p {
            DrawPrim::Panel { y, h, fill, .. } if fill == c => Some((y, h)),
            _ => None,
        })
        .collect()
}

/// The top of the fill: the panel in `ink`, opaque.
fn tide(prims: &[DrawPrim], ink: Rgb) -> f32 {
    let fills = panels_in(prims, rgba(ink, 0xFF));
    assert_eq!(fills.len(), 1, "one fill: {prims:?}");
    fills[0].0
}

#[test]
fn marks_follow_the_trend() {
    let inks = dark();
    let k = inks.merged;
    let line = a(inks.tide, 0.92);
    let gauge_line = a(inks.tide, 0.75);
    let (s, h, bottom) = (2.0, 34.0, 34.0);
    for (name, t, motion) in [
        ("rising", trend(0.8, 0.5, 0.0), Motion::Rising),
        ("falling", trend(0.3, 0.7, 0.0), Motion::Falling),
        ("steady", trend(0.6, 0.6, 0.0), Motion::Steady),
    ] {
        let m = Reading::of(&t, 1.0, GATE);
        assert_eq!(m.motion, motion, "{name}");
        let ink = k.heat_at(m.level);
        let band = a(ink, 0.38);
        let slow_y = (bottom - m.slow * h).round();

        let tile = drawn(&inks, s, |mk| mk.part_tile((0.0, 0.0, 7.0, h), m, None));
        let top = tide(&tile, ink);
        let (lines, bands) = (panels_in(&tile, line), panels_in(&tile, band));
        match motion {
            Motion::Rising => {
                assert_eq!(lines.len(), 1, "{name}: one slow line: {tile:?}");
                let (y, lh) = lines[0];
                assert!(
                    y > top && y + lh <= bottom,
                    "{name}: the line is inside the fill"
                );
                assert!(
                    (y + lh / 2.0 - slow_y).abs() <= 1.0,
                    "{name}: at the slow level"
                );
                assert!(bands.is_empty(), "{name}: nothing fell away");
            }
            Motion::Falling => {
                assert_eq!(bands, [(slow_y, top - slow_y)], "{name}: the fallen band");
                assert!(slow_y < top, "{name}: above the fill");
                assert!(lines.is_empty(), "{name}: the band's top is the slow line");
            }
            Motion::Steady => {
                assert!(lines.is_empty() && bands.is_empty(), "{name}: {tile:?}");
            }
        }

        let gauge = drawn(&inks, s, |mk| {
            mk.gauge((0.0, 0.0, 19.0, h), m, GaugeLook::default());
        });
        let top = tide(&gauge, ink);
        let lines = panels_in(&gauge, gauge_line);
        let bands = panels_in(&gauge, band);
        match motion {
            Motion::Rising => {
                assert_eq!(lines.len(), 1, "{name} gauge: {gauge:?}");
                assert!(
                    lines[0].0 > top,
                    "{name} gauge: the slow line inside the fill"
                );
                assert!(bands.is_empty());
            }
            Motion::Falling => {
                assert_eq!(lines.len(), 1, "{name} gauge: {gauge:?}");
                assert!(
                    lines[0].0 + lines[0].1 <= top,
                    "{name} gauge: above the fill"
                );
                assert_eq!(
                    bands,
                    [(slow_y, top - slow_y)],
                    "{name} gauge: the fallen band"
                );
            }
            Motion::Steady => assert!(lines.is_empty() && bands.is_empty(), "{name} gauge"),
        }
    }
}

/// A still reading (Network's, which keeps no slow average) draws no slow line on its gauge, even
/// where its fill is floored above its level; a slow level as far below the same fill does.
#[test]
fn a_still_gauge_at_its_floor_draws_no_slow_line() {
    let inks = dark();
    let gauge_line = a(inks.tide, 0.75);
    for s in [1.0, 2.0] {
        let r = (0.0, 0.0, 9.5 * s, 17.0 * s);
        let still = drawn(&inks, s, |mk| {
            mk.gauge(r, Reading::still(0.01), GaugeLook::default());
        });
        assert!(
            panels_in(&still, gauge_line).is_empty(),
            "{s}×: a still 1% draws no slow line: {still:?}"
        );
        let fell = drawn(&inks, s, |mk| {
            mk.gauge(
                r,
                Reading::of(&trend(0.01, 0.2, 0.0), 1.0, GATE),
                GaugeLook::default(),
            );
        });
        assert_eq!(
            panels_in(&fell, gauge_line).len(),
            1,
            "{s}×: CONTROL: a slow 20% over a 1% fill does: {fell:?}"
        );
    }
}

#[test]
fn active_parts_keep_a_floor_and_an_idle_one_draws_none() {
    let inks = dark();
    let ink = inks.merged.heat_at(0.0);
    for s in [1.0, 2.0] {
        let active = drawn(&inks, s, |mk| {
            mk.part_tile((0.0, 0.0, 7.0, 34.0), Reading::still(0.02), Some(ink));
        });
        let fill = panels_in(&active, rgba(ink, 0xFF));
        assert_eq!(fill.len(), 1);
        assert!(fill[0].1 >= 1.25 * s, "{s}×: {fill:?}");
        let idle = drawn(&inks, s, |mk| {
            mk.part_tile((0.0, 0.0, 7.0, 34.0), Reading::still(0.004), Some(ink));
        });
        assert!(
            panels_in(&idle, rgba(ink, 0xFF)).is_empty(),
            "{s}×: {idle:?}"
        );
    }
}

#[test]
fn a_gauge_swings_jagged_and_carries_its_over_and_needs_marks() {
    let inks = dark();
    let k = inks.merged;
    let r = (0.0, 0.0, 19.0, 34.0);
    let calm = drawn(&inks, 2.0, |mk| {
        mk.gauge(
            r,
            Reading::of(&trend(0.12, 0.12, 0.04), 1.0, GATE),
            GaugeLook::default(),
        );
    });
    let swings = drawn(&inks, 2.0, |mk| {
        mk.gauge(
            r,
            Reading::of(&trend(0.12, 0.12, 0.15), 1.0, GATE),
            GaugeLook::default(),
        );
    });
    let fill = rgba(k.heat_at(0.12), 0xFF);
    assert_eq!(
        panels_in(&calm, fill).len(),
        1,
        "1.36 px of spread: a flat top"
    );
    let steps = panels_in(&swings, fill);
    assert!(steps.len() >= 3, "5.1 px of spread: stepped: {steps:?}");
    let (lo, hi) = (steps[0].0, steps[1].0);
    assert!(lo - hi >= 3.0 && lo - hi <= 6.0, "{steps:?}");
    let marked = drawn(&inks, 2.0, |mk| {
        let look = GaugeLook {
            ink: None,
            over: true,
            needs: true,
        };
        mk.gauge(r, Reading::still(0.98), look);
    });
    let orange = rgba(inks.over, 0xFF);
    let top = |p: &DrawPrim| match *p {
        DrawPrim::Panel { y, fill, .. } => y == 0.0 && fill == orange,
        _ => false,
    };
    assert!(marked.iter().any(top), "an orange top");
    let magenta = rgba(k.needs, 0xFF);
    let ring = |p: &DrawPrim| match *p {
        DrawPrim::Stroke { x, color, .. } => x < 0.0 && color == magenta,
        _ => false,
    };
    assert!(marked.iter().any(ring), "a magenta ring outside the gauge");
}

#[test]
fn a_hollow_bucket_is_dashed_never_a_zero() {
    let inks = dark();
    let k = inks.merged;
    let cell = |heat: Option<f32>| HeatCell {
        heat,
        max: heat,
        over: false,
    };
    let cells = [
        cell(Some(0.0)),
        cell(None),
        cell(Some(0.5)),
        cell(Some(f32::NAN)),
    ];
    for s in [1.0, 2.0] {
        let prims = drawn(&inks, s, |mk| mk.run((0.0, 0.0, 39.0, 30.0), &cells));
        let dashed: Vec<f32> = prims
            .iter()
            .filter_map(|p| match *p {
                DrawPrim::Dashed { x, .. } => Some(x),
                _ => None,
            })
            .collect();
        assert_eq!(dashed, [10.0, 30.0], "{s}×: the unread buckets: {prims:?}");
        let zero = panels_in(&prims, rgba(k.floor, 0xFF));
        assert_eq!(zero.len(), 1, "{s}×: a zero is a stub: {prims:?}");
        assert!(
            zero[0].1 >= 1.0 && zero[0].0 + zero[0].1 == 30.0,
            "{zero:?}"
        );
    }
}

/// A bar too low to reach the stub's height draws the stub in the floor ink, whatever ink the
/// run asks for (the coolest heat fades into the well, §4); one that reaches it keeps its own.
#[test]
fn a_stub_is_drawn_in_the_floor_ink() {
    for inks in [dark(), light()] {
        let k = inks.merged;
        let teal = k.all_tabs;
        for s in [1.0_f32, 2.0] {
            let h = 17.0 * s;
            let stub = (1.75 * s).min(h * 0.2).round().max(1.0);
            let cell = |px: f32| HeatCell {
                heat: Some(px / h),
                max: None,
                over: false,
            };
            let cells = [cell(0.0), cell(stub - 0.5), cell(stub + 0.5), cell(h / 2.0)];
            for (ask, ink) in [("heat", None), ("teal", Some(teal))] {
                let prims = drawn(&inks, s, |mk| {
                    mk.run_with((0.0, 0.0, 40.0 * s, h), &cells, |_, v| {
                        ink.unwrap_or_else(|| k.heat_at(v))
                    });
                });
                let floors = panels_in(&prims, rgba(k.floor, 0xFF));
                assert_eq!(
                    floors.len(),
                    2,
                    "{s}× {ask}: the zero and the sliver: {prims:?}"
                );
                assert!(
                    floors.iter().all(|&(y, fh)| fh == stub && y + fh == h),
                    "{s}× {ask}: each a stub on the foot: {floors:?}"
                );
                let own = |v: f32| ink.unwrap_or_else(|| k.heat_at(v));
                let reached = (stub + 0.5) / h;
                assert!(
                    !panels_in(&prims, rgba(own(reached), 0xFF)).is_empty(),
                    "{s}× {ask}: a bar past the stub keeps its ink: {prims:?}"
                );
                assert_ne!(k.floor, own(0.0), "CONTROL: {ask}'s own ink differs");
            }
        }
    }
}

#[test]
fn a_run_inks_each_bar_as_asked_and_caps_what_went_over() {
    let inks = dark();
    let cells: Vec<HeatCell> = (0..RUN_BARS)
        .map(|i| HeatCell {
            heat: Some(0.5),
            max: Some(0.5),
            over: i == 7,
        })
        .collect();
    let past = [0x10, 0x20, 0x30];
    let prims = drawn(&inks, 2.0, |mk| {
        mk.run_with((0.0, 0.0, 130.0, 34.0), &cells, |i, _| {
            if i < 5 {
                past
            } else {
                inks.merged.heat_at(0.5)
            }
        });
    });
    assert_eq!(panels_in(&prims, rgba(past, 0xFF)).len(), 5);
    let caps = panels_in(&prims, rgba(inks.over, 0xFF));
    assert_eq!(caps.len(), 1, "{prims:?}");
    assert_eq!(caps[0].0, 0.0, "at the bar's top");
}

#[test]
fn slats_stop_at_ten() {
    let inks = dark();
    let over = rgba(inks.over, 0xFF);
    let widths = |units: f32| -> Vec<f32> {
        drawn(&inks, 2.0, |mk| mk.slats((0.0, 0.0, 12.0, 34.0), units))
            .iter()
            .filter_map(|p| match *p {
                DrawPrim::Panel { w, fill, .. } if fill == over => Some(w),
                _ => None,
            })
            .collect()
    };
    assert_eq!(widths(14.3).len(), MAX_SLATS);
    assert_eq!(widths(10.0).len(), MAX_SLATS);
    assert_eq!(widths(2.5), [12.0, 12.0, 6.0], "the last one partial");
    assert!(widths(0.04).is_empty() && widths(f32::NAN).is_empty());
}

/// Ten slats in a 2× lane of any height keep one height and one gap: they read one per core,
/// never as pairs.
#[test]
fn slats_keep_one_rhythm() {
    let inks = dark();
    let over = rgba(inks.over, 0xFF);
    for h in [30.0, 34.0, 38.0, 40.0] {
        let slats = panels_in(
            &drawn(&inks, 2.0, |mk| mk.slats((0.0, 0.0, 12.0, h), 10.0)),
            over,
        );
        assert_eq!(slats.len(), MAX_SLATS, "{h} px: {slats:?}");
        let gaps: Vec<f32> = slats
            .windows(2)
            .map(|pair| pair[0].0 - (pair[1].0 + pair[1].1))
            .collect();
        assert!(
            gaps.iter().all(|&g| g >= 1.0 && g == gaps[0]),
            "{h} px: one gap: {gaps:?}"
        );
        assert!(
            slats.iter().all(|&(_, sh)| sh == slats[0].1),
            "{h} px: one height: {slats:?}"
        );
    }
}

#[test]
fn run_ends_name_both_ends_or_draw_nothing() {
    let inks = dark();
    let m = FaceMetrics::helvetica_neue();
    let size = crate::type_scale::TypeStep::Caption.px_clamped(20.0, 20.0, 20.0);
    let mut fits = None;
    let wide = drawn(&inks, 2.0, |mk| {
        fits = Some(mk.run_ends((100.0, 240.0), 12.0, 18.0, ("10m", "now"), size, &m));
    });
    assert_eq!(fits, Some(true));
    let words: Vec<&str> = wide
        .iter()
        .filter_map(|p| match p {
            DrawPrim::Text { s, color, .. } if *color == rgba(inks.merged.axis, 0xFF) => {
                Some(s.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(words, ["10m", "now"]);
    let narrow = drawn(&inks, 2.0, |mk| {
        fits = Some(mk.run_ends((100.0, 150.0), 0.0, 18.0, ("10m", "now"), size, &m));
    });
    assert_eq!(fits, Some(false));
    assert!(narrow.is_empty(), "{narrow:?}");
}

/// What a prim can ink, `[x0, y0, x1, y1]`; a text's from its measured advance.
fn ink_box(p: &DrawPrim, m: &FaceMetrics) -> Option<[f32; 4]> {
    Some(match *p {
        DrawPrim::Panel { x, y, w, h, .. } => [x, y, x + w, y + h],
        DrawPrim::Stroke {
            x, y, w, h, width, ..
        } => {
            let o = width / 2.0;
            [x - o, y - o, x + w + o, y + h + o]
        }
        DrawPrim::Dashed {
            x, y, w, h, width, ..
        } => match (w == 0.0, h == 0.0) {
            (true, _) => [x, y, x + width, y + h],
            (_, true) => [x, y, x + w, y + width],
            _ => [x, y, x + w, y + h],
        },
        DrawPrim::Line {
            x1,
            y1,
            x2,
            y2,
            width,
            ..
        } => {
            let o = width / 2.0;
            [
                x1.min(x2) - o,
                y1.min(y2) - o,
                x1.max(x2) + o,
                y1.max(y2) + o,
            ]
        }
        DrawPrim::Text {
            x,
            baseline,
            ref s,
            px,
            ..
        } => {
            let w = m.width(s, f64::from(px), false) as f32;
            [x, baseline - px, x + w, baseline]
        }
        _ => return None,
    })
}

/// Every prim's ink box, clipped by the clips around it, inside `[x0, y0, x1, y1]`; and the
/// clips balance.
fn assert_inside(prims: &[DrawPrim], within: [f32; 4], m: &FaceMetrics, what: &str) {
    let mut clips: Vec<[f32; 4]> = Vec::new();
    for p in prims {
        match *p {
            DrawPrim::ClipPush { x, y, w, h } => clips.push([x, y, x + w, y + h]),
            DrawPrim::ClipPop => {
                assert!(clips.pop().is_some(), "{what}: a pop without its push");
            }
            _ => {
                let Some(mut b) = ink_box(p, m) else {
                    continue;
                };
                for c in &clips {
                    b = [
                        b[0].max(c[0]),
                        b[1].max(c[1]),
                        b[2].min(c[2]),
                        b[3].min(c[3]),
                    ];
                }
                if b[0] >= b[2] || b[1] >= b[3] {
                    continue;
                }
                let e = 1e-3;
                assert!(
                    b[0] >= within[0] - e
                        && b[1] >= within[1] - e
                        && b[2] <= within[2] + e
                        && b[3] <= within[3] + e,
                    "{what}: {p:?} inks {b:?}, outside {within:?}"
                );
            }
        }
    }
    assert!(
        clips.is_empty(),
        "{what}: {} clips left pushed",
        clips.len()
    );
}

#[test]
fn marks_stay_inside_their_rect() {
    let m = FaceMetrics::helvetica_neue();
    for inks in [dark(), light()] {
        for s in [1.0_f32, 1.5, 2.0] {
            let (x, y, w, h) = (10.0, 20.0, 7.0 * s / 2.0, 17.0 * s);
            let tile = (x, y, w, h);
            let inside = |r: Rect| [r.0, r.1, r.0 + r.2, r.1 + r.3];
            let readings = [
                Reading::of(&trend(0.8, 0.5, 0.0), 1.0, GATE),
                Reading::of(&trend(0.3, 0.9, 0.3), 1.0, GATE),
                Reading::of(&trend(1.4, 0.2, 0.0), 1.0, GATE),
                Reading::of(&trend(f32::NAN, -1.0, f32::NAN), 1.0, GATE),
                Reading::still(1.0),
            ];
            for r in readings {
                let what = format!("{s}× tile {r:?}");
                let prims = drawn(&inks, s, |mk| mk.part_tile(tile, r, None));
                assert_inside(&prims, inside(tile), &m, &what);
                let g = (x, y, 9.5 * s, 17.0 * s);
                for (over, needs) in [(false, false), (true, false), (true, true)] {
                    let look = GaugeLook {
                        ink: None,
                        over,
                        needs,
                    };
                    let mut reach = 0.0;
                    let prims = drawn(&inks, s, |mk| {
                        reach = if needs { mk.ring_reach() } else { 0.0 };
                        mk.gauge(g, r, look);
                    });
                    let mut within = inside(g);
                    within = [
                        within[0] - reach,
                        within[1] - reach,
                        within[2] + reach,
                        within[3] + reach,
                    ];
                    assert_inside(&prims, within, &m, &format!("{s}× gauge {look:?} {r:?}"));
                }
            }
            let run = (x, y, 66.0 * s, 17.0 * s);
            let cells: Vec<HeatCell> = (0..RUN_BARS)
                .map(|i| HeatCell {
                    heat: (i % 7 != 3).then_some(i as f32 / 12.0 - 0.2),
                    max: None,
                    over: i % 5 == 0,
                })
                .collect();
            let prims = drawn(&inks, s, |mk| mk.run(run, &cells));
            assert_inside(&prims, inside(run), &m, &format!("{s}× run"));
            let bar = (x, y, 65.0 * s, 4.5 * s);
            for share in [0.0, 0.001, 0.4, 1.0, 7.0, f32::NAN] {
                let prims = drawn(&inks, s, |mk| {
                    mk.share_bar(bar, share, inks.merged.all_tabs)
                });
                assert_inside(&prims, inside(bar), &m, &format!("{s}× share {share}"));
            }
            let prims = drawn(&inks, s, |mk| mk.wall(x, y, y + h));
            let half = (s.round().max(1.0) / 2.0).ceil();
            assert_inside(&prims, [x - half, y, x + half, y + h], &m, "wall");
            let slats = (x, y, 6.0 * s, 17.0 * s);
            for units in [0.5, 3.7, 10.0, 25.0] {
                let prims = drawn(&inks, s, |mk| mk.slats(slats, units));
                assert_inside(&prims, inside(slats), &m, &format!("{s}× slats {units}"));
            }
            let prims = drawn(&inks, s, |mk| mk.hatch(tile));
            assert_inside(&prims, inside(tile), &m, &format!("{s}× hatch"));
            let size =
                crate::type_scale::TypeStep::Caption.px_clamped(10.0 * s, 10.0 * s, 10.0 * s);
            let (from, to, lead) = (x + 6.0 * s, x + 72.0 * s, 3.0 * s);
            let prims = drawn(&inks, s, |mk| {
                assert!(mk.run_ends((from, to), lead, y + 9.0 * s, ("24h", "now"), size, &m));
            });
            assert_inside(
                &prims,
                [from - lead, y - s, to, y + 20.0 * s],
                &m,
                "run ends",
            );
        }
    }
}

#[test]
fn clips_balance() {
    let inks = dark();
    let m = FaceMetrics::helvetica_neue();
    let mut p = Painter::new(&inks, 2.0, 26.0, false);
    specimen(&mut p, 0.0, &m);
    let mut depth = 0_i32;
    let mut pushes = 0;
    for prim in &p.prims {
        match prim {
            DrawPrim::ClipPush { .. } => {
                depth += 1;
                pushes += 1;
            }
            DrawPrim::ClipPop => depth -= 1,
            _ => {}
        }
        assert!(depth >= 0, "a pop before its push");
    }
    assert_eq!(depth, 0, "every clip pushed is popped");
    assert!(pushes >= 3, "the over top and the hatches clip: {pushes}");
}

#[test]
fn marks_keep_to_their_prim_budgets() {
    let inks = dark();
    for s in [1.0, 2.0] {
        let hollow = vec![HeatCell::default(); RUN_BARS];
        let n = drawn(&inks, s, |mk| {
            mk.run((0.0, 0.0, 66.0 * s, 17.0 * s), &hollow)
        })
        .len();
        assert!(n <= 25, "{s}×: a hollow run of 20 costs {n} prims");
        let busy: Vec<HeatCell> = (0..RUN_BARS)
            .map(|i| HeatCell {
                heat: Some(i as f32 / 19.0),
                max: None,
                over: true,
            })
            .collect();
        let n = drawn(&inks, s, |mk| mk.run((0.0, 0.0, 66.0 * s, 17.0 * s), &busy)).len();
        assert!(n <= 3 * RUN_BARS, "{s}×: a busy run of 20 costs {n} prims");
        for t in [
            trend(0.8, 0.5, 0.0),
            trend(0.3, 0.7, 0.0),
            trend(0.6, 0.6, 0.0),
            trend(1.0, 1.0, 0.0),
            trend(0.0, 0.0, 0.0),
        ] {
            for (w, h) in [(3.5, 17.0), (2.3, 13.25), (4.0, 7.75), (9.5, 17.0)] {
                let n = drawn(&inks, s, |mk| {
                    mk.part_tile((0.0, 0.0, w * s, h * s), Reading::of(&t, 1.0, GATE), None);
                })
                .len();
                assert!(n <= 4, "{s}×: a {w}×{h} pt tile at {t:?} costs {n} prims");
            }
        }
    }
}

/// The marks as the merged deck draws them, dark and light at 2× and 1×, under a crop of the key
/// mockup's CPU and Memory blocks (`docs/design-studies/top-deck-key-dark-2026-10-23.png`, 2×),
/// to read side by side.
#[test]
#[ignore = "writes a PNG for a person to read: set ATERM_DECK_GALLERY to a directory"]
fn marks_sheet() {
    let Some(dir) = std::env::var_os("ATERM_DECK_GALLERY") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("the gallery directory");
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let metrics = crate::deck_face::deck_metrics();
    let (mut prims, mut top, mut width) = (Vec::new(), 0.0_f32, 0.0_f32);
    for inks in [dark(), light()] {
        for s in [2.0_f32, 1.0] {
            let mut p = Painter::new(&inks, s, 13.0 * s, false);
            let (w, h) = specimen(&mut p, top, &metrics);
            prims.append(&mut p.prims);
            (top, width) = (top + h + 8.0, width.max(w));
        }
    }
    let (ours, w, h) =
        crate::tray_raster::rasterize_tray(&prims, width as u32, top as u32, 1.0, [0, 0, 0, 0xFF]);
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/design-studies/top-deck-key-dark-2026-10-23.png");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let (key, kw, _) = aterm_png::decode_rgba8(&bytes, 4096).expect("the key decodes");
    let (cx, cy, cw, ch) = (862, 548, (w as usize).min(718), 104);
    let w = w as usize;
    let mut sheet = vec![0_u8; w * (ch + 8) * 4];
    for row in 0..ch {
        let from = ((cy + row) * kw + cx) * 4;
        sheet[row * w * 4..(row * w + cw) * 4].copy_from_slice(&key[from..from + cw * 4]);
    }
    sheet.extend_from_slice(&ours);
    let sh = (ch + 8) as u32 + h;
    let png = crate::app_introspect::encode_rgba8_png(&sheet, w as u32, sh).expect("encodes");
    let file = dir.join("merged_marks_sheet.png");
    std::fs::write(&file, png).expect("the PNG writes");
    eprintln!("{}", file.display());
}
