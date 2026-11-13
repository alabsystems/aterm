// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn specs() -> [MultiScaleSpec; 8] {
    use aterm_vitals::ScaleBand;
    [
        MultiScaleSpec::FULL_6H,
        MultiScaleSpec::FOLD_4H,
        MultiScaleSpec::COMPACT_6M,
        MultiScaleSpec::MINI_1M,
        MultiScaleSpec {
            bands: &[],
            ticks: &[],
        },
        MultiScaleSpec {
            bands: &[ScaleBand {
                slots: 1,
                period_ms: 1000,
                weight: 1,
                live_now: false,
            }],
            ticks: &[],
        },
        MultiScaleSpec {
            bands: &[ScaleBand {
                slots: 3,
                period_ms: 1000,
                weight: 1,
                live_now: true,
            }],
            ticks: &[],
        },
        MultiScaleSpec {
            bands: &[
                ScaleBand {
                    slots: 0,
                    period_ms: 1000,
                    weight: 9,
                    live_now: false,
                },
                ScaleBand {
                    slots: 1,
                    period_ms: 0,
                    weight: 0,
                    live_now: true,
                },
                ScaleBand {
                    slots: 2,
                    period_ms: 1000,
                    weight: 3,
                    live_now: false,
                },
                ScaleBand {
                    slots: 1,
                    period_ms: 1000,
                    weight: 1,
                    live_now: true,
                },
                ScaleBand {
                    slots: 2,
                    period_ms: 2000,
                    weight: 4,
                    live_now: false,
                },
            ],
            ticks: &[],
        },
    ]
}

#[test]
fn slot_rectangles_preserve_indices_clipping_order_and_float_bits() {
    let (mut cases, mut entering, mut clipped) = (0, 0, 0);
    for spec in specs() {
        for x in [0.0, 12.25, 1_000_000.125] {
            for cols in [0, 1, 19, 64] {
                for pitch in [0.0, 1.0, 5.25] {
                    for cell in [0.0, 3.0, f64::INFINITY] {
                        let strip = Strip {
                            x,
                            cols,
                            period_s: 1,
                            pitch,
                            cell,
                            spec,
                        };
                        for scale in [f64::NAN, -1.0, 0.25, 0.5, 1.0, 2.0, f64::INFINITY] {
                            for lag in [
                                f32::NEG_INFINITY,
                                -100.0,
                                -1.0,
                                -0.0,
                                0.0,
                                0.49,
                                0.5,
                                1.0,
                                2.0,
                                4.0,
                                8.0,
                                20.0,
                                100.0,
                                1000.0,
                                f32::INFINITY,
                                f32::NAN,
                            ] {
                                let before = previous_rects(&strip, scale, lag);
                                let after = multi_scale_slot_rects(&strip, scale, lag);
                                let bits = |&(i, x, w, live): &(usize, f32, f32, bool)| {
                                    (i, x.to_bits(), w.to_bits(), live)
                                };
                                assert_eq!(
                                    before.len(),
                                    after.len(),
                                    "length: {strip:?} scale={scale} lag={lag}"
                                );
                                for (a, b) in before.iter().zip(&after) {
                                    assert_eq!(
                                        bits(a),
                                        bits(b),
                                        "rectangle: {strip:?} scale={scale} lag={lag}"
                                    );
                                }
                                entering += usize::from(after.windows(2).any(|w| w[0].0 == w[1].0));
                                clipped += usize::from(after.len() < spec.slot_count());
                                cases += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        entering > 0 && clipped > 0,
        "CONTROL: entering={entering} clipped={clipped}"
    );
    println!("slot rectangle cases={cases} entering={entering} clipped={clipped}");
}

#[test]
#[ignore = "manual paired slot-layout allocation and instruction measurement"]
fn measure_slot_rectangles() {
    use aterm_bench::measurement_clock::counters;
    use std::hint::black_box;
    type Probe = fn(&Strip, f64, f32) -> Vec<(usize, f32, f32, bool)>;
    let probes: [(&str, Probe); 2] = [
        ("reference", previous_rects),
        ("candidate", multi_scale_slot_rects),
    ];
    for (spec_id, spec) in specs().into_iter().enumerate() {
        for cols in [19, 64] {
            for scale in [1.0, 2.0] {
                for lag in [0.0, 1.0, 4.0, 100.0, f32::NAN, f32::INFINITY] {
                    let strip = Strip {
                        x: 12.25,
                        cols,
                        period_s: 1,
                        pitch: 5.25,
                        cell: 3.0,
                        spec,
                    };
                    for (_, probe) in probes {
                        for _ in 0..100 {
                            black_box(probe(black_box(&strip), black_box(scale), black_box(lag)));
                        }
                    }
                    for sample in 0..5 {
                        let order = if sample % 2 == 0 { [0, 1] } else { [1, 0] };
                        for i in order {
                            let (variant, probe) = probes[i];
                            let run = || {
                                black_box(probe(
                                    black_box(&strip),
                                    black_box(scale),
                                    black_box(lag),
                                ))
                            };
                            let (_, allocations) = aterm_heap_count::allocations_in(run);
                            let start = counters();
                            for _ in 0..2_000 {
                                run();
                            }
                            let instructions = start
                                .zip(counters())
                                .map(|(a, b)| b.instructions.saturating_sub(a.instructions));
                            println!(
                                "rects spec={spec_id} cols={cols} scale={scale} lag={lag} variant={variant} sample={sample} iterations=2000 allocations={allocations} instructions={instructions:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}

// Frozen from 91252493e7d: compare the immediate previous materialized layout.
fn previous_band(slots: &[ProgSlot]) -> Option<std::ops::Range<usize>> {
    let start = slots.iter().position(|slot| !slot.is_live_now)?;
    let period = |slot: &ProgSlot| slot.age_hi_ms - slot.age_lo_ms;
    let p = period(&slots[start]);
    let len = slots[start..]
        .iter()
        .take_while(|slot| !slot.is_live_now && period(slot) == p)
        .count();
    Some(start..start + len)
}

fn previous_rects(strip: &Strip, scale: f64, lag_px: f32) -> Vec<(usize, f32, f32, bool)> {
    let s = scale.max(0.5);
    let px = |v: f64| (v * s + 0.5).floor() as f32;
    let slots = strip.spec.slots();
    let rest: Vec<(usize, f32, f32, bool)> = strip
        .box_edges()
        .into_iter()
        .zip(&slots)
        .enumerate()
        .map(|(j, ((l, r), slot))| {
            let lx = px(l);
            (j, lx, (px(r) - lx).max(1.0), slot.is_live_now)
        })
        .collect();
    let lag = lag_px.round();
    let Some(band) = previous_band(&slots).filter(|band| lag > 0.0 && !band.is_empty()) else {
        return rest;
    };
    let gap_px = px(BOX_GAP).max(1.0);
    let oldest = band.end - 1;
    let (band_x0, (_, old_x, old_w, _)) = (rest[band.start].1, rest[oldest]);
    let band_x1 = old_x + old_w;
    // The entering box stands a pitch past the band's oldest, as the band's own boxes do.
    let after = band_x1 + gap_px;
    let shows = |x: f32, w: f32| x.max(band_x0) + 1.0 <= (x + w).min(band_x1);
    let mut out = Vec::with_capacity(rest.len() + 1);
    out.extend_from_slice(&rest[..band.start]);
    out.extend(
        rest[band.clone()]
            .iter()
            .map(|&(j, lx, cw, live)| (j, lx - lag, cw, live))
            .filter(|&(_, x, w, _)| shows(x, w)),
    );
    if shows(after - lag, old_w) {
        out.push((oldest, after - lag, old_w, false));
    }
    out.extend_from_slice(&rest[band.end..]);
    out
}
