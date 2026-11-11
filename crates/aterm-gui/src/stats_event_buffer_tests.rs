// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

// Frozen coordinate builders retain the pre-streaming implementation and allocation costs.
fn previous_gap(a: u8, b: u8) -> f64 {
    if a == b { plan::BOX_GAP } else { plan::GATE }
}
fn previous_slots(spec: MultiScaleSpec) -> Vec<ProgSlot> {
    let mut age = 0_i64;
    let mut out = Vec::with_capacity(spec.slot_count());
    for (bi, band) in spec.bands.iter().enumerate() {
        for _ in 0..band.slots {
            let next_age = age + band.period_ms;
            out.push(ProgSlot {
                age_hi_ms: next_age,
                age_lo_ms: age,
                weight: band.weight,
                is_live_now: band.live_now,
                band: u8::try_from(bi).unwrap_or(u8::MAX),
            });
            age = next_age;
        }
    }
    out
}

fn previous_box_edges(strip: Strip) -> Vec<(f64, f64)> {
    let slots = previous_slots(strip.spec);
    let gaps: f64 = slots
        .windows(2)
        .map(|w| previous_gap(w[0].band, w[1].band))
        .sum();
    let room = (strip.width() - gaps).max(0.0);
    let total = f64::from(strip.spec.total_weight());
    let (mut before, mut weight) = (0.0, 0_u32);
    let mut edges = Vec::with_capacity(slots.len());
    for (j, slot) in slots.iter().enumerate() {
        if j > 0 {
            before += previous_gap(slots[j - 1].band, slot.band);
        }
        let at = strip.x + before;
        let left = at + room * f64::from(weight) / total;
        weight += u32::from(slot.weight);
        edges.push((left, at + room * f64::from(weight) / total));
    }
    edges
}

fn previous_x_at_age(strip: Strip, age_ms: i64) -> Option<f64> {
    if age_ms < -2_000 || age_ms > strip.spec.horizon_ms() {
        return None;
    }
    let age = age_ms.max(0);
    let slots = previous_slots(strip.spec);
    let edges = previous_box_edges(strip);
    let n = slots.len().min(edges.len());
    (0..n).find_map(|j| {
        let slot = slots[j];
        if age > slot.age_hi_ms {
            return None;
        }
        let lo = if j == 0 {
            edges[0].0
        } else {
            (edges[j - 1].1 + edges[j].0) / 2.0
        };
        let hi = if j + 1 == n {
            edges[j].1
        } else {
            (edges[j].1 + edges[j + 1].0) / 2.0
        };
        let span = (slot.age_hi_ms - slot.age_lo_ms).max(1) as f64;
        let into = ((age - slot.age_lo_ms) as f64 / span).clamp(0.0, 1.0);
        Some(lo + (hi - lo) * into)
    })
}

// The marker painter at cc5fd459e49, before its temporary buffers moved inline.
fn previous_axis(
    deck: &mut Deck<'_>,
    strip: &Strip,
    base: f64,
    y0: f64,
    last: f64,
    holes: &[(f64, f64)],
    words: &DeckWords,
) {
    let caps = std::mem::take(&mut deck.caps);
    if !deck.draw {
        return;
    }
    let x0 = deck.px(strip.x);
    let x1 = deck.px(strip.end());
    if x1 <= x0 {
        return;
    }
    let y_tick0 = deck.px(base + 1.0);
    let y_tick1 = deck.px(base + 2.5).max(y_tick0 + TICK_MIN_PX);
    let y_axis = (y_tick1 - 1.0).max(y_tick0);
    let pieces = Deck::rail_pieces((x0, x1), y_axis, &caps);
    deck.rail(&pieces, (x0, x1), y_axis, deck.inks.grid);
    let Some(now_ms) = deck.at_ms else {
        return;
    };
    let horizon = strip.spec.horizon_ms();
    // Each event's pip and line, `(pip's left px, line's left px, kind)`, placed before the
    // gaps are drawn, so a gap's notch can stand clear of every pip.
    let mut events: Vec<&StripEvent> = words
        .strip_events
        .iter()
        .filter(|ev| ev.at_unix_ms <= now_ms && now_ms - ev.at_unix_ms <= horizon)
        .collect();
    // Highest-severity events first (Crash > Restart > PressureSpike > SleepWake), then newest.
    events.sort_by(|a, b| {
        b.kind
            .cmp(&a.kind)
            .then_with(|| b.at_unix_ms.cmp(&a.at_unix_ms))
    });
    let mut drawn_xs: Vec<i32> = Vec::with_capacity(6);
    let lw = (0.85 * deck.s).round().max(1.0) as f32;
    let pip = (2.2 * deck.s).round().max(2.0) as f32;
    let mut marks: Vec<(f32, f32, StripEventKind)> = Vec::with_capacity(6);
    for ev in events {
        if drawn_xs.len() >= 6 {
            break;
        }
        let Some(x_pt) = previous_x_at_age(*strip, now_ms - ev.at_unix_ms) else {
            continue;
        };
        let ex = (x_pt * deck.s) as f32;
        let bucket_px = ex.round() as i32;
        if drawn_xs.iter().any(|&px| (px - bucket_px).abs() <= 1) {
            continue;
        }
        drawn_xs.push(bucket_px);
        let lx = (ex - lw / 2.0).round().clamp(x0, (x1 - lw).max(x0));
        let px_pip = (ex - pip / 2.0).round().clamp(x0, (x1 - pip).max(x0));
        marks.push((px_pip, lx, ev.kind));
    }
    let pips: Vec<f32> = marks.iter().map(|m| m.0).collect();
    for gap in words.time_gaps.iter().rev().take(4) {
        if gap.end_ms <= now_ms - horizon || gap.start_ms >= now_ms {
            continue;
        }
        let age_start = (now_ms - gap.start_ms).clamp(0, horizon);
        let age_end = (now_ms - gap.end_ms).clamp(0, horizon);
        let (Some(old), Some(new)) = (
            previous_x_at_age(*strip, age_start),
            previous_x_at_age(*strip, age_end),
        ) else {
            continue;
        };
        // The gap's start is the older end, right of its end (x grows with age).
        let (gx_old, gx_new) = (
            ((old * deck.s) as f32).round().clamp(x0, x1),
            ((new * deck.s) as f32).round().clamp(x0, x1),
        );
        let (gx0, gx1) = (gx_new.min(gx_old), gx_new.max(gx_old));
        if gx1 > gx0 {
            let gink = deck.gap_ink(gap.kind);
            deck.rail(&pieces, (gx0, gx1), y_axis, gink);
            // Each end's notch stands up from the rail's row; its foot on that row is the gap's
            // own rail, just drawn, so where the rail breaks over a cap the notch does too and
            // keeps a device px of ground over the cap. Where an event's pip stands on an end
            // or within a device px of its notch, the notch steps inward, clear of the pip
            // ([`Deck::notch_columns`]).
            let nh = (2.0 * deck.s as f32).round().max(2.0);
            for c in Deck::notch_columns((gx0, gx1), &pips, pip)
                .into_iter()
                .flatten()
            {
                deck.rect((c, y_axis + 1.0 - nh, 1.0, nh - 1.0), 0.0, gink);
            }
        }
    }
    if marks.is_empty() {
        return;
    }
    for &(px_pip, _, kind) in &marks {
        let pip_rect = (px_pip, y_axis + 1.0 - pip, pip, pip);
        let ev_ink = deck.event_ink(kind);
        deck.pip(pip_rect, &pieces, (0.5 * deck.s) as f32, ev_ink);
    }
    let lines: Vec<(f32, StripEventKind)> = marks.iter().map(|&(_, lx, kind)| (lx, kind)).collect();
    let spans = previous_spans(y0, last, holes);
    let rows: Vec<(f32, f32)> = spans
        .iter()
        .map(|&(a, b)| (deck.px(a), deck.px(b)))
        .filter(|(ya, yb)| yb > ya)
        .collect();
    // A device px of ground each side keeps a line off the boxes' columns and ticks
    // (`docs/DESIGN-stats-deck-units-2026-11-04.md` §3, "States"). Every keyline goes down
    // before any line: two events' lines can touch, and a keyline drawn after its neighbour's
    // line would cut it.
    let ground = rgba(deck.inks.ground, 0xFF);
    for &(lx, _) in &lines {
        for &(ya, yb) in &rows {
            for kx in [lx - 1.0, lx + lw] {
                deck.p.prims.push(panel(kx, ya, 1.0, yb - ya, 0.0, ground));
            }
        }
    }
    for (lx, kind) in lines {
        let ev_ink = deck.event_ink(kind);
        for &(ya, yb) in &rows {
            match kind {
                StripEventKind::Crash | StripEventKind::Restart => {
                    let alpha = if kind == StripEventKind::Crash {
                        0xFF
                    } else {
                        0xE0
                    };
                    deck.p
                        .prims
                        .push(panel(lx, ya, lw, yb - ya, 0.0, rgba(ev_ink, alpha)));
                }
                StripEventKind::SleepWake | StripEventKind::PressureSpike => {
                    let on = (2.0 * deck.s).round().max(2.0) as f32;
                    let off = (1.5 * deck.s).round().max(1.0) as f32;
                    deck.p.prims.push(DrawPrim::Dashed {
                        x: lx,
                        y: ya,
                        w: 0.0,
                        h: yb - ya,
                        width: lw,
                        on,
                        off,
                        // Historical guides stay below the data's contrast.
                        // The full-strength cap pip still marks the event;
                        // Increase Contrast retains the stronger guide.
                        color: rgba(ev_ink, if deck.ic { 0xDF } else { 0x70 }),
                    });
                }
            }
        }
    }
}

fn previous_spans(y0: f64, y1: f64, holes: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut spans = Vec::new();
    let mut y = y0;
    for &(a, b) in holes {
        if a > y {
            spans.push((y, a));
        }
        y = y.max(b);
    }
    if y < y1 {
        spans.push((y, y1));
    }
    spans
}

#[test]
fn streamed_age_positions_keep_weighted_edges_bit_for_bit() {
    use aterm_vitals::ScaleBand;
    const MIXED: [ScaleBand; 4] = [
        ScaleBand {
            slots: 0,
            period_ms: 1000,
            weight: 9,
            live_now: false,
        },
        ScaleBand {
            slots: 2,
            period_ms: 0,
            weight: 0,
            live_now: true,
        },
        ScaleBand {
            slots: 2,
            period_ms: 1000,
            weight: 4,
            live_now: false,
        },
        ScaleBand {
            slots: 1,
            period_ms: 2000,
            weight: 1,
            live_now: false,
        },
    ];
    let specs = [
        MultiScaleSpec::FULL_6H,
        MultiScaleSpec::FOLD_4H,
        MultiScaleSpec::COMPACT_6M,
        MultiScaleSpec::MINI_1M,
        MultiScaleSpec {
            bands: &MIXED,
            ticks: &[],
        },
        MultiScaleSpec {
            bands: &[],
            ticks: &[],
        },
    ];
    let mut compared = 0;
    for spec in specs {
        let horizon = spec.horizon_ms();
        let mut ages = vec![-2001, -2000, -1, 0, 1, horizon - 1, horizon, horizon + 1];
        for slot in previous_slots(spec) {
            ages.extend([
                slot.age_lo_ms - 1,
                slot.age_lo_ms,
                slot.age_lo_ms + 1,
                slot.age_hi_ms - 1,
                slot.age_hi_ms,
                slot.age_hi_ms + 1,
            ]);
        }
        ages.sort_unstable();
        ages.dedup();
        for x in [0.0, 12.25, 1_000_000.125] {
            for cols in [0, 1, 19, 64] {
                for pitch in [1.0, 4.0, 5.25] {
                    for cell in [0.0, 3.0] {
                        let strip = Strip {
                            x,
                            cols,
                            period_s: 1,
                            pitch,
                            cell,
                            spec,
                        };
                        let bits = |pairs: Vec<(f64, f64)>| {
                            pairs
                                .into_iter()
                                .map(|(a, b)| (a.to_bits(), b.to_bits()))
                                .collect::<Vec<_>>()
                        };
                        assert_eq!(
                            bits(previous_box_edges(strip)),
                            bits(strip.box_edges()),
                            "x={x} cols={cols} pitch={pitch} cell={cell} spec={spec:?}"
                        );
                        for &age in &ages {
                            assert_eq!(
                                previous_x_at_age(strip, age).map(f64::to_bits),
                                strip.x_at_age_ms(age).map(f64::to_bits),
                                "age={age} x={x} cols={cols} pitch={pitch} cell={cell} spec={spec:?}"
                            );
                            compared += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(compared > 10_000);
    println!("age position bit comparisons: {compared}");
}

#[derive(Debug, PartialEq, Eq)]
enum MarkerPrim {
    Panel([u32; 5], [u8; 4]),
    Dashed([u32; 7], [u8; 4]),
    Clip([u32; 4]),
    Pop,
}

fn signature(prims: &[DrawPrim]) -> Vec<MarkerPrim> {
    prims
        .iter()
        .map(|p| match p {
            DrawPrim::Panel {
                x,
                y,
                w,
                h,
                radius,
                fill,
            } => MarkerPrim::Panel([*x, *y, *w, *h, *radius].map(f32::to_bits), *fill),
            DrawPrim::Dashed {
                x,
                y,
                w,
                h,
                width,
                on,
                off,
                color,
            } => MarkerPrim::Dashed(
                [*x, *y, *w, *h, *width, *on, *off].map(f32::to_bits),
                *color,
            ),
            DrawPrim::ClipPush { x, y, w, h } => {
                MarkerPrim::Clip([*x, *y, *w, *h].map(f32::to_bits))
            }
            DrawPrim::ClipPop => MarkerPrim::Pop,
            other => panic!("unexpected marker primitive: {other:?}"),
        })
        .collect()
}

#[test]
fn streamed_spans_match_materialized_coordinates() {
    let pairs = [
        (-2.0, -1.0),
        (-0.0, 0.0),
        (0.0, 1.0),
        (1.0, 0.0),
        (1.0, 2.0),
        (2.0, 4.0),
        (f64::NEG_INFINITY, f64::INFINITY),
        (f64::NAN, 0.0),
        (1.0, f64::NAN),
    ];
    let ends = [
        f64::NEG_INFINITY,
        -1.0,
        -0.0,
        0.0,
        1.0,
        4.0,
        f64::INFINITY,
        f64::NAN,
    ];
    let mut compared = 0;
    for len in 0..=3 {
        for mut code in 0..pairs.len().pow(len) {
            let holes: Vec<_> = (0..len)
                .map(|_| {
                    let pair = pairs[code % pairs.len()];
                    code /= pairs.len();
                    pair
                })
                .collect();
            for y0 in ends {
                for y1 in ends {
                    let expected: Vec<_> = previous_spans(y0, y1, &holes)
                        .into_iter()
                        .map(|(a, b)| (a.to_bits(), b.to_bits()))
                        .collect();
                    let mut spans = Deck::strip_spans(y0, y1, &holes);
                    let actual: Vec<_> = spans
                        .by_ref()
                        .map(|(a, b)| (a.to_bits(), b.to_bits()))
                        .collect();
                    assert_eq!(expected, actual, "range={y0}..{y1}, holes={holes:?}");
                    assert!(spans.next().is_none());
                    assert!(spans.next().is_none());
                    compared += 1;
                }
            }
        }
    }
    assert!(compared > 50_000);
    println!("span coordinate comparisons: {compared}");
}

fn event_batch(batch: u8, now: i64, horizon: i64) -> Vec<StripEvent> {
    use StripEventKind::{Crash, PressureSpike, Restart, SleepWake};
    let kinds = [Crash, Restart, PressureSpike, SleepWake];
    let event = |age, kind| StripEvent {
        at_unix_ms: now - age,
        kind,
        label: "event".to_owned(),
    };
    match batch {
        0 => Vec::new(),
        1 => vec![event(0, Restart)],
        2 => kinds
            .into_iter()
            .zip(0_i64..)
            .map(|(kind, i)| event(horizon * i / 5, kind))
            .collect(),
        3 => kinds
            .into_iter()
            .map(|kind| event(horizon / 4, kind))
            .chain([event(-1000, Crash), event(horizon + 1000, Crash)])
            .collect(),
        4 => (0_u8..24)
            .map(|i| event(horizon * i64::from(i + 1) / 25, kinds[usize::from(i) % 4]))
            .collect(),
        _ => (0_u8..20)
            .map(|i| event(i64::from(i), kinds[usize::from(i) % 4]))
            .collect(),
    }
}

fn gaps(now: i64, horizon: i64) -> Vec<TimeGap> {
    [
        (horizon / 3, horizon / 4, GapKind::Crash),
        (horizon / 10, horizon / 12, GapKind::Sleep),
        (horizon / 2, horizon / 3, GapKind::Paused),
    ]
    .into_iter()
    .map(|(a, b, kind)| TimeGap {
        start_ms: now - a,
        end_ms: now - b,
        kind,
    })
    .collect()
}

fn caps_at(x0: f32, x1: f32, axis: f32, case: usize) -> Vec<(Rect, u8)> {
    match case {
        0 => Vec::new(),
        1 => vec![((x0 + 1.0, axis - 2.0, 2.0, 4.0), 0)],
        _ => vec![
            ((x0 + 1.0, axis - 2.0, 2.0, 4.0), 0),
            (((x0 + x1) / 2.0, axis - 1.0, 4.0, 3.0), 1),
        ],
    }
}

#[test]
fn event_buffers_preserve_every_primitive_and_its_order() {
    use super::paint_tests::{Scene, scene, scene_input};
    let m = FaceMetrics::helvetica_neue();
    let snap = scene(Scene::Calm);
    let holes = [
        vec![],
        vec![(33.0, 38.0)],
        vec![(25.0, 28.0), (50.0, 61.0), (88.0, 92.0)],
        vec![(20.0, 25.0), (24.0, 40.0), (70.0, 100.0)],
    ];
    let (mut compared, mut clips, mut dashed, mut most_guides) = (0, 0, 0, 0);
    for form in [DeckForm::Folded, DeckForm::Full] {
        for width in [350.0, 550.0, 1119.0] {
            let strip = match form {
                DeckForm::Full => plan::plan_full(width, &m).strip,
                DeckForm::Folded => plan::plan_folded(width, &m).blocks[0].strip,
            };
            for scale in [0.75, 1.0, 2.0] {
                let g = DeckGeom {
                    w: (width * scale) as f32,
                    h: (130.0 * scale) as f32,
                    pad: 0.0,
                    scale: scale as f32,
                    font_px: (13.0 * scale) as f32,
                };
                let px = |v: f64| (v * scale + 0.5).floor() as f32;
                let (base, y0, last) = (8.0, 20.0, 100.0);
                let axis =
                    (px(base + 2.5).max(px(base + 1.0) + TICK_MIN_PX) - 1.0).max(px(base + 1.0));
                for contrast in [false, true] {
                    let mut input = scene_input(&snap, form);
                    input.contrast = contrast;
                    let mut words = DeckWords::new(&input, &m);
                    for gap_data in [false, true] {
                        words.time_gaps = if gap_data {
                            gaps(snap.at_unix_ms, strip.spec.horizon_ms())
                        } else {
                            Vec::new()
                        };
                        for batch in 0..6 {
                            words.strip_events =
                                event_batch(batch, snap.at_unix_ms, strip.spec.horizon_ms());
                            for hs in &holes {
                                for cap_case in 0..3 {
                                    let caps =
                                        caps_at(px(strip.x), px(strip.end()), axis, cap_case);
                                    for clock in [false, true] {
                                        for draw in [false, true] {
                                            let mut a = Deck::new(&g, &input, &m, draw);
                                            let mut b = Deck::new(&g, &input, &m, draw);
                                            if !clock {
                                                a.at_ms = None;
                                                b.at_ms = None;
                                            }
                                            a.caps.clone_from(&caps);
                                            b.caps.clone_from(&caps);
                                            previous_axis(
                                                &mut a, &strip, base, y0, last, hs, &words,
                                            );
                                            b.strip_axis_and_events(
                                                &strip, base, y0, last, hs, &words,
                                            );
                                            assert_eq!(
                                                signature(&a.p.prims),
                                                signature(&b.p.prims),
                                                "{form:?} width={width} scale={scale} contrast={contrast} batch={batch} gaps={gap_data} holes={hs:?} caps={cap_case} clock={clock} draw={draw}"
                                            );
                                            assert!(a.caps.is_empty() && b.caps.is_empty());
                                            clips += b
                                                .p
                                                .prims
                                                .iter()
                                                .filter(|p| matches!(p, DrawPrim::ClipPush { .. }))
                                                .count();
                                            dashed += b
                                                .p
                                                .prims
                                                .iter()
                                                .filter(|p| matches!(p, DrawPrim::Dashed { .. }))
                                                .count();
                                            if hs.is_empty() {
                                                let ground = rgba(b.inks.ground, 0xFF);
                                                let h = px(last) - px(y0);
                                                let guides = b
                                                    .p
                                                    .prims
                                                    .iter()
                                                    .filter(|p| match p {
                                                        DrawPrim::Panel {
                                                            y, h: ph, fill, ..
                                                        } => {
                                                            *y == px(y0)
                                                                && *ph == h
                                                                && *fill != ground
                                                        }
                                                        DrawPrim::Dashed { y, h: ph, .. } => {
                                                            *y == px(y0) && *ph == h
                                                        }
                                                        _ => false,
                                                    })
                                                    .count();
                                                most_guides = most_guides.max(guides);
                                            }
                                            compared += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(compared > 20_000 && clips > 0 && dashed > 0);
    assert_eq!(
        most_guides, 6,
        "the existing marker limit was never reached"
    );
    println!(
        "event paths compared={compared}, clips={clips}, dashed={dashed}, max_guides={most_guides}"
    );
}

fn candidate_axis(
    deck: &mut Deck<'_>,
    strip: &Strip,
    base: f64,
    y0: f64,
    last: f64,
    holes: &[(f64, f64)],
    words: &DeckWords,
) {
    deck.strip_axis_and_events(strip, base, y0, last, holes, words);
}

#[test]
#[ignore = "manual paired event-painter allocation and instruction measurement"]
fn measure_event_buffers() {
    use super::paint_tests::{Scene, scene, scene_input};
    use aterm_bench::measurement_clock::counters;
    use std::hint::black_box;
    type Probe = fn(&mut Deck<'_>, &Strip, f64, f64, f64, &[(f64, f64)], &DeckWords);
    let probes: [(&str, Probe); 2] = [("reference", previous_axis), ("candidate", candidate_axis)];
    let m = deck_metrics();
    let snap = scene(Scene::Calm);
    for form in [DeckForm::Folded, DeckForm::Full] {
        for width in [350.0, 1119.0] {
            let strip = match form {
                DeckForm::Full => plan::plan_full(width, &m).strip,
                DeckForm::Folded => plan::plan_folded(width, &m).blocks[0].strip,
            };
            for scale in [1.0, 2.0] {
                let g = DeckGeom {
                    w: (width * scale) as f32,
                    h: (130.0 * scale) as f32,
                    pad: 0.0,
                    scale: scale as f32,
                    font_px: (13.0 * scale) as f32,
                };
                let input = scene_input(&snap, form);
                let mut words = DeckWords::new(&input, &m);
                let px = |v: f64| (v * scale + 0.5).floor() as f32;
                let (base, y0, last) = (8.0, 20.0, 100.0);
                let axis =
                    (px(base + 2.5).max(px(base + 1.0) + TICK_MIN_PX) - 1.0).max(px(base + 1.0));
                for (name, batch, gap_data, cap_case, clock, draw) in [
                    ("empty", 0, false, 0, true, true),
                    ("gaps-only", 0, true, 1, true, true),
                    ("one", 1, false, 1, true, true),
                    ("mixed", 2, true, 2, true, true),
                    ("saturated", 4, true, 2, true, true),
                    ("colliding", 5, true, 1, true, true),
                    ("no-clock", 4, true, 1, false, true),
                    ("layout-only", 4, true, 1, true, false),
                ] {
                    words.strip_events =
                        event_batch(batch, snap.at_unix_ms, strip.spec.horizon_ms());
                    words.time_gaps = if gap_data {
                        gaps(snap.at_unix_ms, strip.spec.horizon_ms())
                    } else {
                        Vec::new()
                    };
                    let caps = caps_at(px(strip.x), px(strip.end()), axis, cap_case);
                    let hs = if gap_data {
                        vec![(33.0, 38.0), (50.0, 61.0)]
                    } else {
                        Vec::new()
                    };
                    let mut decks = [
                        Deck::new(&g, &input, &m, draw),
                        Deck::new(&g, &input, &m, draw),
                    ];
                    for (i, deck) in decks.iter_mut().enumerate() {
                        if !clock {
                            deck.at_ms = None;
                        }
                        for _ in 0..100 {
                            deck.p.prims.clear();
                            deck.caps.extend_from_slice(&caps);
                            probes[i].1(deck, &strip, base, y0, last, &hs, &words);
                            black_box(&deck.p.prims);
                        }
                    }
                    assert_eq!(signature(&decks[0].p.prims), signature(&decks[1].p.prims));
                    for sample in 0..5 {
                        let order = if sample % 2 == 0 { [0, 1] } else { [1, 0] };
                        for i in order {
                            let (variant, probe) = probes[i];
                            let run = |deck: &mut Deck<'_>| {
                                deck.p.prims.clear();
                                deck.caps.extend_from_slice(&caps);
                                probe(deck, &strip, base, y0, last, &hs, &words);
                                black_box(&deck.p.prims);
                            };
                            let deck = &mut decks[i];
                            let (_, allocations) = aterm_heap_count::allocations_in(|| run(deck));
                            let start = counters();
                            for _ in 0..5_000 {
                                run(black_box(&mut *deck));
                            }
                            let instructions = start
                                .zip(counters())
                                .map(|(a, b)| b.instructions.saturating_sub(a.instructions));
                            println!(
                                "events case={name} form={form:?} width={width} scale={scale} variant={variant} sample={sample} iterations=5000 allocations={allocations} instructions={instructions:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}
