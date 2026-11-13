// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn strain(level: Option<f32>, over: f32, soak: f32) -> Strain {
    Strain { level, over, soak }
}

#[test]
fn the_needle_reads_the_level_then_the_redline() {
    let g = Gauge::of(&strain(Some(0.7), 0.0, 0.0));
    assert_eq!(g.needle, Some(0.7));
    assert!(
        (tip_deg(0.7) - 12.0).abs() < 1e-4,
        "busy 70% points past 12 o'clock"
    );
    let hot = Gauge::of(&strain(Some(1.0), 1.0, 1.0));
    assert_eq!((hot.needle, hot.soak), (Some(2.0), 1.0));
    assert!(hot.pinned());
    let red = Gauge::of(&strain(Some(1.0), 0.12, 0.2));
    assert!(red.needle.is_some_and(|v| v > 1.0) && !red.pinned());
    assert!(tip_deg(1.12) > RED);
    assert_eq!(Gauge::of(&strain(None, 0.0, 0.0)), Gauge::UNREAD);
    let odd = Gauge::of(&strain(Some(f32::NAN), f32::NAN, f32::NAN));
    assert_eq!(odd, Gauge::UNREAD, "a non-finite reading draws as unread");
}

#[test]
fn the_bezel_warms_with_the_soak_and_never_shouts_until_soaked() {
    let rim = [0x4A, 0x4C, 0x52];
    let over = [0xFF, 0x9A, 0x2E];
    assert_eq!(soak_ink(rim, over, 0.0), rim);
    assert_eq!(soak_ink(rim, over, SOAK_SHOWN), rim);
    assert_eq!(soak_ink(rim, over, 0.07), rim, "drawn 0.027");
    let chroma = |c: Rgb| {
        let l = ink::to_lab(c);
        l.a.hypot(l.b)
    };
    let ladder = [0.1, 0.25, 0.54, 0.8, 1.0].map(|s| soak_ink(rim, over, s));
    for w in ladder.windows(2) {
        assert!(chroma(w[1]) > chroma(w[0]), "warmer in turn: {ladder:?}");
        assert!(
            ink::to_lab(w[1]).l > ink::to_lab(w[0]).l,
            "lighter in turn: {ladder:?}"
        );
    }
    assert!(ink::distance(soak_ink(rim, over, 1.0), over) < 0.01);
    assert!(
        ink::distance(soak_ink(rim, over, 0.54), over) > 0.1,
        "a recovering bezel is copper, not the alarm"
    );
}

/// The bezel draws the soak on the sketch's scale: the owner's spike (`Strain` 0.21) about
/// 0.12 and recovering (0.54) 0.43..0.50, as the sketch drew them; rest and full stay put.
#[test]
fn the_bezel_draws_the_soak_on_the_sketchs_scale() {
    let near = |raw: f32, lo: f32, hi: f32| {
        let d = drawn_soak(raw);
        assert!((lo..=hi).contains(&d), "{raw} draws {d}, not {lo}..{hi}");
    };
    near(0.21, 0.115, 0.125);
    near(0.54, 0.43, 0.50);
    assert_eq!((drawn_soak(0.0), drawn_soak(1.0)), (0.0, 1.0));
    assert_eq!(drawn_soak(f32::NAN), 0.0);
    let ladder = [0.05, 0.2, 0.4, 0.6, 0.8, 0.95].map(drawn_soak);
    assert!(ladder.windows(2).all(|w| w[0] < w[1]), "{ladder:?}");
    let rim = [0x4A, 0x4C, 0x52];
    let over = [0xFF, 0x9A, 0x2E];
    assert!(
        ink::distance(soak_ink(rim, over, 0.21), rim)
            < ink::distance(soak_ink(rim, over, 0.54), rim),
        "spike's trace is fainter than recovering's copper"
    );
}

/// As the needle sweeps, the pieces of the lit arc and of the redline's print stand still on
/// their lattice from the stop: from one tip to the next within a piece, only the last,
/// partial piece of the arc changes (the first of the print, which runs from the tip to the
/// end), so the arc neither crawls nor shimmers. The glow's arcs keep all but their end.
#[test]
fn a_sweeping_needle_changes_only_the_arcs_partial_piece() {
    let inks =
        aterm_vitals::ink::vitals_inks([0x16, 0x18, 0x1D], [0xE6, 0xE8, 0xEE], [0x58, 0xA6, 0xFF]);
    let k = GaugeInks::new(&inks, inks.heat, 0.0);
    let s = 2.0;
    let drawn = |f: &dyn Fn(&mut Pen<'_>)| {
        let mut out = Vec::new();
        let mut p = Pen {
            out: &mut out,
            cx: 80.0,
            cy: 80.0,
            s,
        };
        f(&mut p);
        out
    };
    let line = |p: &DrawPrim| match *p {
        DrawPrim::Line {
            x1,
            y1,
            x2,
            y2,
            width,
            color,
        } => Some(([x1, y1, x2, y2, width], color)),
        _ => None,
    };
    let band = (BAND_W * s) as f32;
    let segments = |prims: &[DrawPrim]| {
        prims
            .iter()
            .filter_map(line)
            .filter(|(g, _)| g[4] == band)
            .collect::<Vec<_>>()
    };
    // Two tips whose chains (which stop a round cap short of the tip, 7.7°) end inside one
    // piece of the ramp's 2.5° lattice, and of the redline's 7.5°.
    for (a, b) in [(-42.0, -40.0), (17.9, 20.0), (113.0, 119.5)] {
        let (la, lb) = (
            segments(&drawn(&|p| p.lit(a, &k))),
            segments(&drawn(&|p| p.lit(b, &k))),
        );
        assert_eq!(la.len(), lb.len(), "{a}° and {b}°");
        let n = la.len();
        assert!(n > 4, "CONTROL {a}°: a chain");
        assert_eq!(la[..n - 1], lb[..n - 1], "{a}° to {b}°: the arc crawls");
        assert_ne!(
            la[n - 1],
            lb[n - 1],
            "CONTROL {a}° to {b}°: the tip's piece moves"
        );
    }
    // The glow: the same arcs, of which only the end moves.
    let glow = |tip: f64| {
        drawn(&|p| p.arc_glow(0.8, tip, &k))
            .into_iter()
            .map(|p| match p {
                DrawPrim::Arc {
                    cx,
                    cy,
                    r,
                    width,
                    from,
                    to,
                    color,
                } => ([cx, cy, r, width, from, color[3].into()], to),
                other => panic!("the glow draws only arcs: {other:?}"),
            })
            .collect::<Vec<_>>()
    };
    let (ga, gb) = (glow(33.0), glow(34.0));
    assert_eq!(
        ga.len(),
        2 * GLOW[0].1.len(),
        "CONTROL: every layer of both passes"
    );
    for (a, b) in ga.iter().zip(&gb) {
        assert_eq!(a.0, b.0, "the glow moves behind the tip");
        assert!(b.1 > a.1, "CONTROL: its end follows the tip");
    }
    // The print from the tip to the end: only its first piece changes.
    let print = |from: f64| {
        drawn(&|p| p.print(from, &k))
            .iter()
            .filter_map(line)
            .collect::<Vec<_>>()
    };
    let (pa, pb) = (print(76.0), print(81.0));
    assert_eq!(pa.len(), pb.len());
    assert_eq!(pa[1..], pb[1..], "the print crawls");
    assert_ne!(pa[0], pb[0], "CONTROL: the print starts at the tip");
}

#[test]
fn erf_matches_its_table() {
    for (x, want) in [
        (0.0, 0.0),
        (0.5, 0.520_499_9),
        (1.0, 0.842_700_8),
        (2.0, 0.995_322_3),
    ] {
        assert!((erf(x) - want).abs() < 1e-6, "erf({x})");
        assert!((erf(-x) + want).abs() < 1e-6, "erf(-{x})");
    }
}

#[test]
fn warm_and_hot_load_emit_themed_arc_glow_while_calm_does_not() {
    let inks =
        aterm_vitals::ink::vitals_inks([0x16, 0x18, 0x1D], [0xE6, 0xE8, 0xEE], [0x58, 0xA6, 0xFF]);
    let k = GaugeInks::new(&inks, inks.heat, 0.0);
    let place = Place {
        centre: (80.0, 80.0),
        slot: [10.0, 10.0, 150.0, 150.0],
        s: 2.0,
        r: R_FACE,
    };
    let drawn = |needle: f32| {
        let mut out = Vec::new();
        draw(
            Gauge {
                needle: Some(needle),
                soak: 0.0,
            },
            place,
            &k,
            &mut out,
        );
        out
    };
    let translucent_lines = |needle: f32| {
        drawn(needle)
            .into_iter()
            .filter(|p| matches!(p, DrawPrim::Line { color, .. } if color[3] < 0xFF))
            .count()
    };
    // The glow's arcs: where they end, turns, and their summed opacity.
    let glow = |needle: f32| {
        let arcs = drawn(needle)
            .into_iter()
            .filter_map(|p| match p {
                DrawPrim::Arc { to, color, .. } => Some((to, u32::from(color[3]))),
                _ => None,
            })
            .collect::<Vec<_>>();
        let end = arcs.iter().map(|a| a.0).fold(f32::MIN, f32::max);
        (arcs.len(), end, arcs.iter().map(|a| a.1).sum::<u32>())
    };
    // The needle's shadow is the only translucent line, at any load; calm load has no glow.
    for v in [0.25, 0.70, 1.40] {
        assert_eq!(translucent_lines(v), 2, "{v}");
    }
    assert_eq!(glow(0.25).0, 0, "calm load glows");
    // Warm (70%) and redline (140%) loads add the two-pass themed arc glow halo.
    let (warm, redline) = (glow(0.70), glow(1.40));
    assert!(warm.0 > 0, "70% load should emit dial arc glow: {warm:?}");
    assert!(
        redline.1 > warm.1 && redline.2 > warm.2,
        "redline's glow reaches further and brighter than 70%'s: {redline:?} vs {warm:?}"
    );
    // Redline scale warms past base `over` toward `thermal[4]` crimson-red.
    assert_ne!(k.scale(RED + 5.0), k.scale(END));
}

/// The glow as the 2026-10-24 gauge drew it: translucent round-capped pieces on a 12° lattice
/// from the stop, every one at least alpha 1 from a load of 0.55. The negative control of
/// the glow's pixel tests.
fn chained_glow(p: &mut Pen<'_>, v: f32, tip: f64, k: &GaugeInks) {
    if v < 0.55 {
        return;
    }
    let boost = 1.0 + 0.25 * (v - 1.0).clamp(0.0, 1.0);
    let strength = (((v - 0.55) / 0.65).clamp(0.0, 1.0) * boost).min(1.25);
    let glow = k.glow_ink(v);
    for (shift, w_add, (dark, light)) in [(-0.9, 4.2, (0.20, 0.14)), (-0.4, 2.0, (0.34, 0.24))] {
        let a = if k.dark { dark } else { light };
        let color = rgba(glow, ((a * strength * 255.0).round() as u8).max(1));
        let (r, w) = (R_MID + shift, BAND_W + w_add);
        for (t0, t1) in lattice(START, tip.min(END), 12.0) {
            p.line(p.at(t0, r), p.at(t1, r), w, color);
        }
    }
}

/// The glow `paint` draws, alone over the face at 2 device px per pt, as the tray raster
/// draws it, and the dial's centre.
fn glow_pixels(k: &GaugeInks, paint: &dyn Fn(&mut Pen<'_>)) -> Vec<u8> {
    let mut out = Vec::new();
    paint(&mut Pen {
        out: &mut out,
        cx: 80.0,
        cy: 80.0,
        s: 2.0,
    });
    crate::tray_raster::rasterize_tray(&out, 160, 160, 1.0, rgba(k.face, 0xFF)).0
}

/// Each theme's gauge inks.
fn both_grounds() -> [GaugeInks; 2] {
    [
        ([0x16, 0x18, 0x1D], [0xE6, 0xE8, 0xEE]),
        ([0xF5, 0xF5, 0xF7], [0x1C, 0x1E, 0x24]),
    ]
    .map(|(ground, fg)| {
        let inks = aterm_vitals::ink::vitals_inks(ground, fg, [0x58, 0xA6, 0xFF]);
        GaugeInks::new(&inks, inks.heat, 0.0)
    })
}

/// The glow neither beads nor shimmers as the needle sweeps a device pixel a frame: from one
/// frame to the next no pixel behind the tip it left changes, and ahead of it the glow grows
/// by about as much each frame. The 12° chain it replaced laid each new piece's round cap
/// over the last one's where they met, a bead that popped in as the tip crossed each point
/// of its lattice and reached back behind it.
#[test]
fn a_needle_sweeping_a_pixel_a_frame_grows_the_glow_evenly() {
    // Frames a device px apart at the needle's point, across six of the chain's lattice
    // points (-39° to 21°).
    let step = (1.0 / (2.0 * NEEDLE_TIP)).to_degrees();
    let tips = (0..)
        .map(|i| -45.0 + step * f64::from(i))
        .take_while(|&t| t < 25.0)
        .collect::<Vec<_>>();
    for k in both_grounds() {
        let sweep = |chain: bool| {
            let frames = tips
                .iter()
                .map(|&tip| {
                    glow_pixels(&k, &|p| {
                        if chain {
                            chained_glow(p, 1.0, tip, &k);
                        } else {
                            p.arc_glow(1.0, tip, &k);
                        }
                    })
                })
                .collect::<Vec<_>>();
            let mut behind = 0_u64;
            let mut ahead = Vec::new();
            for (pair, &tip) in frames.windows(2).zip(&tips) {
                let mut grew = 0_u64;
                for (i, (a, b)) in pair[0].chunks(4).zip(pair[1].chunks(4)).enumerate() {
                    let d = a
                        .iter()
                        .zip(b)
                        .map(|(x, y)| u64::from(x.abs_diff(*y)))
                        .sum::<u64>();
                    let (x, y) = ((i % 160) as f64 - 79.5, (i / 160) as f64 - 79.5);
                    if x.atan2(-y).to_degrees() < tip {
                        behind += d;
                    } else {
                        grew += d;
                    }
                }
                ahead.push(grew);
            }
            let lo = ahead.iter().copied().min().unwrap_or(0);
            let hi = ahead.iter().copied().max().unwrap_or(0);
            (behind, lo, hi)
        };
        let (behind, lo, hi) = sweep(false);
        assert_eq!(
            behind, 0,
            "dark {}: the glow changes behind the tip",
            k.dark
        );
        assert!(lo > 0, "CONTROL dark {}: the glow grows", k.dark);
        assert!(
            hi * 2 <= lo * 3,
            "dark {}: the glow grows unevenly, {lo}..{hi} a frame",
            k.dark
        );
        let (behind, lo, hi) = sweep(true);
        assert!(
            behind > 0 && hi * 2 > lo * 3,
            "CONTROL dark {}: the chain beads ({behind} behind, {lo}..{hi} a frame)",
            k.dark
        );
    }
}

/// The glow fades in from nothing as the load climbs past 0.55: in each pass at most one
/// layer steps, by one, from one load to the next a ten-thousandth on, so its first light
/// is one layer at the least opacity the raster draws. The chain it replaced lit every piece
/// of both its passes at once, at alpha 1, from 0.55.
#[test]
fn the_glow_fades_in_one_layer_at_a_time() {
    for k in both_grounds() {
        let tip = tip_deg(0.55);
        let passes = |v: f32| {
            let mut out = Vec::new();
            arc_glow_of(&k, v, tip, &mut out);
            GLOW.map(|(shift, ..)| {
                let r = (2.0 * (R_MID + shift)) as f32;
                out.iter()
                    .map(|p| match *p {
                        DrawPrim::Arc { r: at, color, .. } if at == r => u32::from(color[3]),
                        _ => 0,
                    })
                    .sum::<u32>()
            })
        };
        assert_eq!(passes(0.55), [0, 0], "dark {}: no glow at 0.55", k.dark);
        let mut was = [0, 0];
        for i in 1..=700 {
            let now = passes(0.55 + i as f32 * 1e-4);
            for (pass, (a, b)) in was.iter().zip(&now).enumerate() {
                assert!(
                    (*a..=a + 1).contains(b),
                    "dark {} pass {pass} at {i}: {a} to {b}",
                    k.dark
                );
            }
            was = now;
        }
        assert!(was[0] > 3 && was[1] > 3, "CONTROL dark {}: {was:?}", k.dark);
        // In pixels: the first light, against the glow at full load.
        let light = |pixels: Vec<u8>| {
            let face = rgba(k.face, 0xFF);
            pixels
                .chunks(4)
                .map(|p| {
                    (0..3)
                        .map(|c| u64::from(p[c].abs_diff(face[c])))
                        .sum::<u64>()
                })
                .sum::<u64>()
        };
        let first = (1..)
            .map(|i| 0.55 + i as f32 * 1e-4)
            .find(|&v| passes(v) != [0, 0])
            .unwrap_or(0.56);
        let full = light(glow_pixels(&k, &|p| p.arc_glow(1.0, tip, &k)));
        let faint = light(glow_pixels(&k, &|p| p.arc_glow(first, tip, &k)));
        assert!(
            faint * 40 < full,
            "dark {}: first light {faint} of {full}",
            k.dark
        );
        // Over the light face the least opacity moves no byte, the chain's no more than one
        // layer's; over the dark face the chain's first light was a seventh of its full glow.
        let chain = light(glow_pixels(&k, &|p| chained_glow(p, 0.55, tip, &k)));
        assert!(
            !k.dark || chain * 40 > full,
            "CONTROL: the chain's first light {chain} of {full}"
        );
    }
}

/// [`Pen::arc_glow`] at 2 device px per pt.
fn arc_glow_of(k: &GaugeInks, v: f32, tip: f64, out: &mut Vec<DrawPrim>) {
    Pen {
        out,
        cx: 80.0,
        cy: 80.0,
        s: 2.0,
    }
    .arc_glow(v, tip, k);
}

/// Where `c` lies on the OkLab segment from `a` to `b`, to a channel's rounding, as a share
/// of the way (searched in thousandths), or `None` off it.
fn along(c: Rgb, a: Rgb, b: Rgb) -> Option<f32> {
    let off = |t: f32| {
        let m = ink::mix(a, b, t);
        (0..3).map(|i| m[i].abs_diff(c[i])).max().unwrap_or(0)
    };
    let t = (0..=1_000)
        .map(|i| i as f32 / 1_000.0)
        .min_by_key(|&t| off(t))?;
    (off(t) <= 1).then_some(t)
}

/// The dial keeps main's glowing orange (the owner, 2026-10-26: "I like the glowing orange
/// on the dial") while every data cell draws the original heat ramp: on both grounds, under
/// a green accent or a blue one and in both contrasts, a calm load draws no glow; past its
/// threshold the glow is one warm ink that climbs from amber toward orange, is the orange
/// stop at the redline, and climbs on toward crimson past it (`thermal[2..=4]`). CONTROL: the
/// data cell's ink at each load lies on neither warm segment, so the check tells them apart.
#[test]
fn the_dials_glow_draws_the_warm_stops_past_its_threshold() {
    let place = Place {
        centre: (80.0, 80.0),
        slot: [10.0, 10.0, 150.0, 150.0],
        s: 2.0,
        r: R_FACE,
    };
    for (ground, fg) in [
        ([0x16, 0x18, 0x1D], [0xE6, 0xE8, 0xEE]),
        ([0xF5, 0xF5, 0xF7], [0x1C, 0x1E, 0x24]),
    ] {
        for accent in [[0x50, 0xFA, 0x7B], [0x58, 0xA6, 0xFF]] {
            let inks = aterm_vitals::ink::vitals_inks(ground, fg, accent);
            for (ic, heat, thermal) in [
                (false, inks.heat, inks.thermal),
                (true, inks.heat_ic, inks.thermal_ic),
            ] {
                let k = GaugeInks::new(&inks, heat, 0.0);
                let at = format!("{ground:?} {accent:?} ic {ic}");
                // The one ink the dial's arcs take at `v`, or `None` for none.
                let glow = |v: f32| -> Option<Rgb> {
                    let mut out = Vec::new();
                    let gauge = Gauge {
                        needle: Some(v),
                        soak: 0.0,
                    };
                    draw(gauge, place, &k, &mut out);
                    let arcs: Vec<Rgb> = out
                        .into_iter()
                        .filter_map(|p| match p {
                            DrawPrim::Arc { color, .. } => Some([color[0], color[1], color[2]]),
                            _ => None,
                        })
                        .collect();
                    let first = *arcs.first()?;
                    assert!(arcs.iter().all(|&c| c == first), "{at} {v}: {arcs:?}");
                    Some(first)
                };
                assert_eq!(glow(0.25), None, "{at}: a calm load draws no glow");
                let (amber, orange, crimson) = (thermal[2], thermal[3], thermal[4]);
                let mut was = -1.0;
                for v in [0.60_f32, 0.70, 0.85, 0.92, 1.0] {
                    let c = glow(v).unwrap_or_else(|| panic!("{at} {v}: the glow draws"));
                    let t = along(c, amber, orange)
                        .unwrap_or_else(|| panic!("{at} {v}: {c:?} off amber..orange"));
                    assert!(t > was, "{at} {v}: {t} after {was}");
                    was = t;
                    assert_eq!(
                        along(inks.cell_at(v, ic), amber, orange),
                        None,
                        "CONTROL {at} {v}"
                    );
                }
                assert_eq!(glow(1.0), Some(orange), "{at}: the redline glows orange");
                let mut was = 0.0;
                for v in [1.2_f32, 1.4] {
                    let c = glow(v).unwrap_or_else(|| panic!("{at} {v}: the glow draws"));
                    let t = along(c, orange, crimson)
                        .unwrap_or_else(|| panic!("{at} {v}: {c:?} off orange..crimson"));
                    assert!(t > was, "{at} {v}: {t} after {was}");
                    was = t;
                    assert_eq!(
                        along(inks.cell_at(v, ic), orange, crimson),
                        None,
                        "CONTROL {at} {v}"
                    );
                }
            }
        }
    }
}

#[test]
fn running_hot_blooms_orange_to_red_immediately_without_waiting_for_hour_soak() {
    for (ground, fg) in [
        ([0x16, 0x18, 0x1D], [0xE6, 0xE8, 0xEE]),
        ([0xF5, 0xF5, 0xF7], [0x1C, 0x1E, 0x24]),
    ] {
        let inks = aterm_vitals::ink::vitals_inks(ground, fg, [0x58, 0xA6, 0xFF]);
        let k = GaugeInks::new(&inks, inks.heat, 0.0);
        let place = Place {
            centre: (80.0, 80.0),
            slot: [10.0, 10.0, 150.0, 150.0],
            s: 2.0,
            r: R_FACE,
        };
        let face_px = (R_FACE * 2.0) as f32;
        let bloom_rings = |needle: f32| -> Vec<(f32, Rgba)> {
            let mut out = Vec::new();
            draw(
                Gauge {
                    needle: Some(needle),
                    soak: 0.0,
                },
                place,
                &k,
                &mut out,
            );
            out.into_iter()
                .filter_map(|p| match p {
                    DrawPrim::Ring { r_outer, fill, .. } if r_outer > face_px + 0.5 => {
                        Some((r_outer, fill))
                    }
                    _ => None,
                })
                .collect()
        };
        assert!(
            bloom_rings(0.70).is_empty(),
            "moderate 70% busy stays un-bloomed"
        );
        let orange = bloom_rings(0.92);
        let red = bloom_rings(1.65);
        assert!(
            !orange.is_empty(),
            "92% hot load blooms immediately with soak=0"
        );
        assert!(
            red.len() >= orange.len(),
            "redline load blooms at least as wide as 92%: {} vs {}",
            red.len(),
            orange.len()
        );
        assert_ne!(
            orange.last().map(|r| r.1),
            red.last().map(|r| r.1),
            "inner bloom shifts from thermal orange at 92% toward redline crimson at 165%"
        );
    }
}
