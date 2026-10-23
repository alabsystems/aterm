// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for momentum admission and the scratch budget.

use std::time::Duration;

use aterm_render::GlowQuad;
use aterm_time::Instant;

use super::*;
use crate::cursor_momentum::test_fixtures::{Clock, TAU, config, padded, plain};
use crate::cursor_momentum::{MOMENTUM_GLOW_RADIUS_CELLS, MomentumGlow, MomentumGlowFrame};

const OTHER: GlowQuad = GlowQuad {
    row: 9,
    x: 3,
    y: 4,
    w: 5,
    h: 6,
    color: 0x0012_3456,
    alpha: 0,
    color2: 0x0065_4321,
    alpha2: 0,
};

/// The quads one hosted tick appended to each stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Pushed {
    out: usize,
    under: usize,
}

/// What the hosted path does: admit, reserve the budget past what the
/// streams already hold, tick. Asserts that no push went past the budget, that
/// neither stream reallocated, and that the other effects' quads are
/// untouched.
#[allow(
    clippy::too_many_arguments,
    reason = "the engine's tick arguments plus the profile"
)]
fn hosted_tick(
    g: &mut MomentumGlow,
    p: &MomentumProfile,
    cur: Option<(u16, u16)>,
    now: Instant,
    geom: Geom,
    cfg: &MomentumGlowConfig,
    out: &mut Vec<GlowQuad>,
    under: &mut Vec<GlowQuad>,
) -> Result<(MomentumGlowFrame, ScratchBudget, Pushed), AdmitRefusal> {
    let budget = admit_tick(p, cur, &geom, cfg)?;
    out.reserve_exact(budget.out);
    under.reserve_exact(budget.under);
    let (before_out, before_under) = (out.clone(), under.clone());
    let reserved = (out.capacity(), under.capacity());
    let frame = g.tick(cur, now, geom, cfg, out, under);
    let pushed = Pushed {
        out: out.len() - before_out.len(),
        under: under.len() - before_under.len(),
    };
    assert!(
        pushed.out <= budget.out && pushed.under <= budget.under,
        "pushed {pushed:?} past {budget:?}: cell {}x{}, grid {}x{}, origin ({}, {}), head {}, \
         cursor {cur:?}, radius {}, dark {}",
        geom.cw,
        geom.ch,
        geom.rows,
        geom.cols,
        geom.origin_x,
        geom.origin_y,
        geom.head,
        cfg.radius_cells,
        cfg.dark_theme
    );
    assert_eq!(
        reserved,
        (out.capacity(), under.capacity()),
        "a tick inside its reserved budget reallocated"
    );
    assert_eq!(&out[..before_out.len()], before_out.as_slice());
    assert_eq!(&under[..before_under.len()], before_under.as_slice());
    Ok((frame, budget, pushed))
}

fn tick_once(
    p: &MomentumProfile,
    cur: (u16, u16),
    geom: Geom,
    cfg: &MomentumGlowConfig,
) -> (MomentumGlowFrame, ScratchBudget, Pushed) {
    let now = Clock::ahead(Duration::ZERO).origin;
    let mut g = MomentumGlow::default();
    for _ in 0..12 {
        g.on_key(now, TAU);
    }
    let (mut out, mut under) = (vec![OTHER; 3], vec![OTHER; 2]);
    hosted_tick(&mut g, p, Some(cur), now, geom, cfg, &mut out, &mut under)
        .unwrap_or_else(|e| panic!("an admitted case: {e}"))
}

fn dark_closed_form(geom: &Geom, radius: f32) -> usize {
    let ch = geom.ch as f32;
    let ry = ch * (0.45 + 0.6 * radius * 1.0);
    let row_h = ((ch / HALO_ROW_DIV) as i32).max(2) as usize;
    let rows_in_reach = (2.0 * f64::from(ry) / row_h as f64).ceil() as usize;
    let box_h = usize::from(geom.head) + geom.rows * geom.ch;
    16 * rows_in_reach.min(box_h.div_ceil(row_h) + 1)
}

/// How close the bound came: the largest `pushed / budget` seen on a dark
/// budget of at least [`Tightness::LARGE`] quads and on a light budget below
/// `ribbon_beam`'s own cap (where the derived formula, not the cap, binds),
/// and how often the light cap bound.
#[derive(Default)]
struct Tightness {
    dark: f64,
    light: f64,
    light_capped: usize,
    /// Cases that pushed exactly their budget: one quad less would overflow.
    attained: (usize, usize),
}

impl Tightness {
    const LARGE: usize = 100;

    fn note(&mut self, budget: ScratchBudget, pushed: Pushed) {
        self.attained.0 += usize::from(budget.out > 0 && pushed.out == budget.out);
        self.attained.1 += usize::from(budget.under > 0 && pushed.under == budget.under);
        if budget.out >= Self::LARGE {
            self.dark = self.dark.max(pushed.out as f64 / budget.out as f64);
        }
        if budget.under + 1 == LIGHT_MARK_QUADS {
            self.light_capped += 1;
        } else if budget.under > 0 {
            self.light = self.light.max(pushed.under as f64 / budget.under as f64);
        }
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

#[test]
fn every_admitted_tick_pushes_within_its_budget_and_never_reallocates() {
    let p = &MOMENTUM_PROFILE;
    let mut tight = Tightness::default();
    let mut check = |geom: Geom, cur: (u16, u16), cfg: MomentumGlowConfig| {
        let (_, budget, pushed) = tick_once(p, cur, geom, &cfg);
        if cfg.dark_theme {
            assert!(budget.under == 0 && budget.out <= dark_closed_form(&geom, cfg.radius_cells));
        } else {
            assert!(budget.out == 0 && budget.under < LIGHT_MARK_QUADS);
        }
        tight.note(budget, pushed);
    };
    for (i, cw) in [1usize, 2, 7, 8, 9, 10, 16, 17, 33].into_iter().enumerate() {
        for (j, ch) in [1usize, 2, 3, 11, 12, 13, 20, 23, 24, 28, 35, 36, 47, 61]
            .into_iter()
            .enumerate()
        {
            let padded_grid = (i + j) % 2 == 1;
            let geom = Geom {
                cw,
                ch,
                rows: 30,
                cols: 60,
                origin_x: if padded_grid { 24 } else { 0 },
                origin_y: if padded_grid { 84 } else { 0 },
                win_w: 4000,
                win_h: 4000,
                head: if padded_grid { 80 } else { 0 },
            };
            for radius in [0.0f32, 0.3, MOMENTUM_GLOW_RADIUS_CELLS, 4.0] {
                for dark in [true, false] {
                    for cur in [(0u16, 0u16), (29, 59), (15, 30), (0, 59)] {
                        for intensity in [1.0f32, 0.3, 0.05] {
                            let cfg = MomentumGlowConfig {
                                radius_cells: radius,
                                intensity,
                                glyph_span: (!dark).then_some((0, i32::from(cur.1))),
                                ..config(dark, 0x0050_FA7B)
                            };
                            check(geom, cur, cfg);
                        }
                    }
                }
            }
        }
    }
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..1_500 {
        let cw = 1 + rng.below(64) as usize;
        let ch = 1 + rng.below(96) as usize;
        let origin_x = rng.below(200) as u16;
        let origin_y = rng.below(200) as u16;
        let head = rng.below(u64::from(origin_y) + 1) as u16;
        let cols = 1 + rng.below((120usize.min((65_535 - usize::from(origin_x)) / cw) - 1) as u64)
            as usize;
        let rows =
            1 + rng.below((80usize.min((65_535 - usize::from(origin_y)) / ch) - 1) as u64) as usize;
        let geom = Geom {
            cw,
            ch,
            rows,
            cols,
            origin_x,
            origin_y,
            win_w: 0,
            win_h: 0,
            head,
        };
        let cur = (rng.below(rows as u64) as u16, rng.below(cols as u64) as u16);
        let dark = rng.below(2) == 0;
        let radius = match rng.below(5) {
            0 => 0.0,
            1 => 4.0,
            _ => rng.unit() * 4.0,
        };
        let first = rng.below(cols as u64 + 1) as i32;
        let end = first + rng.below(cols as u64 + 1 - first as u64) as i32;
        let cfg = MomentumGlowConfig {
            radius_cells: radius,
            intensity: 0.02 + rng.unit() * 0.98,
            hue: rng.next() as u32,
            block: rng.below(2) == 0,
            glyph_span: (rng.below(3) != 0).then_some((first, end)),
            ..config(dark, rng.next() as u32 & 0x00FF_FFFF)
        };
        check(geom, cur, cfg);
    }
    assert!(
        tight.light_capped > 0,
        "fixture: some light mark meets ribbon_beam's own cap"
    );
    assert!(
        tight.attained.0 > 0 && tight.attained.1 > 0,
        "negative control: a budget one quad smaller would hold everywhere ({:?})",
        tight.attained
    );
    assert!(
        tight.dark >= 0.75 && tight.light >= 0.75,
        "the derived budget is loose everywhere: at most dark {:.2}, light {:.2} of it is pushed",
        tight.dark,
        tight.light
    );
}

#[test]
fn the_budget_is_the_worst_case_over_every_amplitude() {
    let p = &MOMENTUM_PROFILE;
    let odd = Geom {
        cw: 9,
        ch: 19,
        rows: 30,
        cols: 90,
        win_w: 810,
        win_h: 570,
        ..plain()
    };
    let tall = Geom {
        cw: 17,
        ch: 35,
        rows: 20,
        cols: 50,
        ..padded()
    };
    let mut drew = 0usize;
    for geom in [plain(), padded(), odd, tall] {
        for radius in [
            MOMENTUM_GLOW_RADIUS_CELLS,
            MOMENTUM_PROFILE.max_radius_cells,
        ] {
            for dark in [true, false] {
                let cur = (5u16, 7u16);
                for q in 5u16..=255 {
                    let cfg = MomentumGlowConfig {
                        radius_cells: radius,
                        intensity: (f32::from(q) + 0.5) / 255.0,
                        ..config(dark, 0x0009_69DA)
                    };
                    let (frame, _, pushed) = tick_once(p, cur, geom, &cfg);
                    drew += usize::from(frame.hot && pushed != Pushed::default());
                }
            }
        }
    }
    assert!(drew > 0, "fixture: the amplitude sweep draws");
}

#[test]
fn the_maximum_admitted_profile_runs_within_its_budget() {
    let p = &MOMENTUM_PROFILE;
    let square = Geom {
        cw: 255,
        ch: 255,
        rows: 257,
        cols: 257,
        origin_x: 0,
        origin_y: 0,
        win_w: u16::MAX,
        win_h: u16::MAX,
        head: 0,
    };
    let headed = Geom {
        cw: 100,
        ch: 5,
        rows: 13_091,
        cols: 655,
        origin_x: 35,
        origin_y: 80,
        win_w: u16::MAX,
        win_h: u16::MAX,
        head: 80,
    };
    let tower = Geom {
        cw: 1,
        ch: 65_535,
        rows: 1,
        cols: 65_535,
        origin_x: 0,
        origin_y: 0,
        win_w: u16::MAX,
        win_h: u16::MAX,
        head: 0,
    };
    let clock = Clock::ahead(Duration::ZERO);
    let horizon = i128::try_from(p.horizon.as_nanos()).expect("fits");
    admit_time(p, Some(0), horizon).expect("the horizon itself is admitted");
    admit_frame_interval(p, p.max_frame_interval).expect("the longest lane is admitted");
    admit_tau(p, p.max_tau_s).expect("the longest tau is admitted");
    let now = clock.at(horizon);
    for geom in [square, headed, tower] {
        let last = (
            u16::try_from(geom.rows - 1).expect("fits"),
            u16::try_from(geom.cols - 1).expect("fits"),
        );
        let middle = (last.0 / 2, last.1 / 2);
        for cur in [(0u16, 0u16), last, middle] {
            for dark in [true, false] {
                let cfg = MomentumGlowConfig {
                    radius_cells: p.max_radius_cells,
                    tau_s: p.max_tau_s,
                    glyph_span: (!dark).then_some((0, i32::from(cur.1))),
                    ..config(dark, 0x0050_FA7B)
                };
                let mut g = MomentumGlow::default();
                for _ in 0..12 {
                    g.on_key(now, p.max_tau_s);
                }
                let (mut out, mut under) = (Vec::new(), Vec::new());
                let (frame, budget, pushed) =
                    hosted_tick(&mut g, p, Some(cur), now, geom, &cfg, &mut out, &mut under)
                        .unwrap_or_else(|e| panic!("the profile's maximum is admitted: {e}"));
                assert!(
                    frame.hot && pushed != Pushed::default(),
                    "fixture: it draws"
                );
                assert!(budget.out <= dark_closed_form(&geom, cfg.radius_cells));
                assert!(g.next_change_deadline(now, p.max_frame_interval).is_some());
                assert!(g.cold_at().is_some_and(|cold| cold > now));
            }
        }
    }
}

#[test]
fn one_step_past_the_profile_is_refused_before_the_tick() {
    let p = &MOMENTUM_PROFILE;
    let up = |x: f32| f32::from_bits(x.to_bits() + 1);
    let edge = Geom {
        cw: 255,
        ch: 255,
        rows: 257,
        cols: 257,
        origin_x: 0,
        origin_y: 0,
        win_w: u16::MAX,
        win_h: u16::MAX,
        head: 0,
    };
    let at_limit = MomentumGlowConfig {
        radius_cells: p.max_radius_cells,
        tau_s: p.max_tau_s,
        ..config(true, 0x00FF_FFFF)
    };
    let cases = [
        (
            edge,
            MomentumGlowConfig {
                radius_cells: up(p.max_radius_cells),
                ..at_limit
            },
            AdmitRefusal::Radius(up(p.max_radius_cells).to_bits()),
        ),
        (
            edge,
            MomentumGlowConfig {
                tau_s: up(p.max_tau_s),
                ..at_limit
            },
            AdmitRefusal::Tau(up(p.max_tau_s).to_bits()),
        ),
        (
            Geom {
                origin_x: 1,
                ..edge
            },
            at_limit,
            AdmitRefusal::Geometry(GeometryFault::GridWidth {
                origin_x: 1,
                cols: 257,
                cw: 255,
                limit: 65_535,
            }),
        ),
        (
            Geom {
                origin_y: 1,
                ..edge
            },
            at_limit,
            AdmitRefusal::Geometry(GeometryFault::GridHeight {
                origin_y: 1,
                rows: 257,
                ch: 255,
                limit: 65_535,
            }),
        ),
    ];
    let clock = Clock::ahead(Duration::ZERO);
    let mut g = MomentumGlow::default();
    g.on_key(clock.origin, TAU);
    let (mut out, mut under) = (vec![OTHER], vec![OTHER]);
    hosted_tick(
        &mut g,
        p,
        Some((1, 1)),
        clock.origin,
        edge,
        &at_limit,
        &mut out,
        &mut under,
    )
    .expect("the limit itself is admitted");
    for (geom, cfg, want) in cases {
        let state = g.snapshot(clock.origin);
        let (out_before, under_before) = (out.clone(), under.clone());
        let capacity = (out.capacity(), under.capacity());
        let got = hosted_tick(
            &mut g,
            p,
            Some((1, 1)),
            clock.origin,
            geom,
            &cfg,
            &mut out,
            &mut under,
        );
        assert_eq!(got.map(|_| ()), Err(want));
        assert_eq!(
            g.snapshot(clock.origin),
            state,
            "a refused tick left the state alone"
        );
        assert_eq!((&out, &under), (&out_before, &under_before));
        assert_eq!((out.capacity(), under.capacity()), capacity);
    }
    assert_eq!(
        admit_tau(p, up(p.max_tau_s)),
        Err(AdmitRefusal::Tau(up(p.max_tau_s).to_bits()))
    );
    let lane = p.max_frame_interval + Duration::from_nanos(1);
    assert_eq!(
        admit_frame_interval(p, lane),
        Err(AdmitRefusal::FrameInterval(lane))
    );
    let horizon = i128::try_from(p.horizon.as_nanos()).expect("fits");
    assert_eq!(
        admit_time(p, None, horizon + 1),
        Err(AdmitRefusal::Horizon(horizon + 1))
    );
}

#[test]
fn admission_refuses_each_non_finite_or_out_of_range_value_with_its_own_variant() {
    let p = &MOMENTUM_PROFILE;
    let tiny = f32::from_bits(1);
    for tau in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        0.0,
        -0.0,
        -1.0,
        -tiny,
    ] {
        assert_eq!(
            admit_tau(p, tau),
            Err(AdmitRefusal::Tau(tau.to_bits())),
            "{tau}"
        );
        let cfg = MomentumGlowConfig {
            tau_s: tau,
            ..config(true, 0)
        };
        assert_eq!(
            admit_tick(p, Some((1, 1)), &plain(), &cfg),
            Err(AdmitRefusal::Tau(tau.to_bits())),
            "{tau}"
        );
    }
    for tau in [tiny, 0.05, TAU, p.max_tau_s] {
        assert_eq!(admit_tau(p, tau), Ok(()), "{tau}");
    }
    for radius in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -tiny, -1.0] {
        let cfg = MomentumGlowConfig {
            radius_cells: radius,
            ..config(false, 0)
        };
        assert_eq!(
            admit_tick(p, Some((1, 1)), &plain(), &cfg),
            Err(AdmitRefusal::Radius(radius.to_bits())),
            "{radius}"
        );
    }
    for radius in [0.0f32, -0.0, MOMENTUM_GLOW_RADIUS_CELLS] {
        let cfg = MomentumGlowConfig {
            radius_cells: radius,
            ..config(false, 0)
        };
        assert!(
            admit_tick(p, Some((1, 1)), &plain(), &cfg).is_ok(),
            "{radius}"
        );
    }
    for intensity in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let cfg = MomentumGlowConfig {
            intensity,
            ..config(true, 0)
        };
        assert_eq!(
            admit_tick(p, Some((1, 1)), &plain(), &cfg),
            Err(AdmitRefusal::Intensity(intensity.to_bits())),
            "{intensity}"
        );
    }
    for intensity in [-5.0f32, 0.0, 0.5, 7.0] {
        let cfg = MomentumGlowConfig {
            intensity,
            ..config(true, 0)
        };
        assert!(
            admit_tick(p, Some((1, 1)), &plain(), &cfg).is_ok(),
            "{intensity}"
        );
    }
    let all_bad = MomentumGlowConfig {
        tau_s: f32::NAN,
        radius_cells: f32::NAN,
        intensity: f32::NAN,
        ..config(true, 0)
    };
    assert_eq!(
        admit_tick(p, Some((1, 1)), &plain(), &all_bad),
        Err(AdmitRefusal::Tau(f32::NAN.to_bits()))
    );
    let zero_cell = Geom { cw: 0, ..plain() };
    assert_eq!(
        admit_tick(p, Some((1, 1)), &zero_cell, &all_bad),
        Err(AdmitRefusal::Geometry(GeometryFault::CellWidthZero))
    );
}

#[test]
fn admission_refuses_degenerate_and_oversized_geometry() {
    let p = &MOMENTUM_PROFILE;
    let cfg = config(true, 0x00FF_FFFF);
    let refuse = |geom: Geom, fault: GeometryFault| {
        assert_eq!(
            admit_tick(p, Some((0, 0)), &geom, &cfg),
            Err(AdmitRefusal::Geometry(fault))
        );
    };
    refuse(Geom { cw: 0, ..plain() }, GeometryFault::CellWidthZero);
    refuse(Geom { ch: 0, ..plain() }, GeometryFault::CellHeightZero);
    refuse(
        Geom {
            cw: 0,
            ch: 0,
            ..plain()
        },
        GeometryFault::CellWidthZero,
    );
    refuse(
        Geom {
            cols: usize::MAX,
            cw: 2,
            ..plain()
        },
        GeometryFault::GridWidth {
            origin_x: 0,
            cols: usize::MAX,
            cw: 2,
            limit: 65_535,
        },
    );
    refuse(
        Geom {
            rows: usize::MAX / 2 + 1,
            ch: 2,
            ..plain()
        },
        GeometryFault::GridHeight {
            origin_y: 0,
            rows: usize::MAX / 2 + 1,
            ch: 2,
            limit: 65_535,
        },
    );
    refuse(
        Geom {
            rows: 3_277,
            ..plain()
        },
        GeometryFault::GridHeight {
            origin_y: 0,
            rows: 3_277,
            ch: 20,
            limit: 65_535,
        },
    );
    refuse(
        Geom {
            cols: 0,
            cw: 70_000,
            ..plain()
        },
        GeometryFault::GridWidth {
            origin_x: 0,
            cols: 0,
            cw: 70_000,
            limit: 65_535,
        },
    );
    refuse(
        Geom {
            origin_y: 84,
            head: 85,
            ..padded()
        },
        GeometryFault::HeadAboveOrigin {
            head: 85,
            origin_y: 84,
        },
    );
    let narrow = MomentumProfile {
        max_extent_px: 1_000,
        ..MOMENTUM_PROFILE
    };
    let geom = Geom {
        cols: 100,
        cw: 10,
        origin_x: 1,
        ..plain()
    };
    assert_eq!(
        admit_tick(&narrow, Some((0, 0)), &geom, &cfg),
        Err(AdmitRefusal::Geometry(GeometryFault::GridWidth {
            origin_x: 1,
            cols: 100,
            cw: 10,
            limit: 1_000,
        }))
    );
    let empty = Geom {
        rows: 0,
        cols: 0,
        ..plain()
    };
    assert_eq!(
        admit_tick(p, Some((0, 0)), &empty, &cfg),
        Ok(ScratchBudget::default()),
        "an empty grid is admitted and can draw nothing"
    );
    assert!(
        admit_tick(
            p,
            Some((0, 0)),
            &Geom {
                origin_y: 84,
                head: 84,
                ..padded()
            },
            &cfg
        )
        .is_ok(),
        "a head band up to the origin is admitted"
    );
}

#[test]
fn admission_refuses_a_glyph_span_outside_the_grids_columns() {
    let p = &MOMENTUM_PROFILE;
    let cols = i32::try_from(plain().cols).expect("fits");
    let with = |span| MomentumGlowConfig {
        glyph_span: Some(span),
        ..config(false, 0x0009_69DA)
    };
    for (first, end) in [
        (-1, 2),
        (3, 2),
        (0, cols + 1),
        (i32::MIN, i32::MAX),
        (cols + 1, cols + 1),
    ] {
        assert_eq!(
            admit_tick(p, Some((1, 1)), &plain(), &with((first, end))),
            Err(AdmitRefusal::GlyphSpan(first, end)),
            "{first}..{end}"
        );
    }
    for span in [(0, 0), (0, cols), (cols, cols), (2, 5)] {
        assert!(
            admit_tick(p, Some((1, 1)), &plain(), &with(span)).is_ok(),
            "{span:?}"
        );
    }
}

#[test]
fn time_admission_holds_logical_time_nondecreasing_inside_the_horizon() {
    let p = &MOMENTUM_PROFILE;
    let horizon = i128::try_from(p.horizon.as_nanos()).expect("fits");
    for at in [-1, i128::MIN, horizon + 1, i128::MAX] {
        assert_eq!(admit_time(p, None, at), Err(AdmitRefusal::Horizon(at)));
    }
    assert_eq!(
        admit_time(p, Some(5), 4),
        Err(AdmitRefusal::TimeWentBack { last: 5, at: 4 })
    );
    assert_eq!(
        admit_time(p, Some(10), -1),
        Err(AdmitRefusal::Horizon(-1)),
        "the range is checked before the order"
    );
    for (last, at) in [
        (None, 0),
        (Some(0), 0),
        (Some(7), 7),
        (Some(7), 8),
        (Some(0), horizon),
    ] {
        assert_eq!(admit_time(p, last, at), Ok(()), "{last:?} then {at}");
    }
}

#[test]
fn frame_interval_admission_refuses_past_the_profile() {
    let p = &MOMENTUM_PROFILE;
    for d in [
        Duration::ZERO,
        Duration::from_micros(16_667),
        p.max_frame_interval,
    ] {
        assert_eq!(admit_frame_interval(p, d), Ok(()), "{d:?}");
    }
    for d in [
        p.max_frame_interval + Duration::from_nanos(1),
        Duration::MAX,
    ] {
        assert_eq!(
            admit_frame_interval(p, d),
            Err(AdmitRefusal::FrameInterval(d))
        );
    }
}

#[test]
fn a_tick_that_cannot_be_hot_is_budgeted_nothing_and_pushes_nothing() {
    let p = &MOMENTUM_PROFILE;
    let geom = plain();
    let (rows, cols) = (
        u16::try_from(geom.rows).expect("fits"),
        u16::try_from(geom.cols).expect("fits"),
    );
    let on = config(true, 0x00FF_FFFF);
    let cases = [
        (None, on),
        (Some((rows, 0)), on),
        (Some((0, cols)), on),
        (
            Some((1, 1)),
            MomentumGlowConfig {
                enabled: false,
                ..on
            },
        ),
        (
            Some((1, 1)),
            MomentumGlowConfig {
                intensity: 0.0,
                ..on
            },
        ),
        (
            Some((1, 1)),
            MomentumGlowConfig {
                intensity: -0.5,
                ..config(false, 0x0009_69DA)
            },
        ),
    ];
    let clock = Clock::ahead(Duration::ZERO);
    for (cur, cfg) in cases {
        assert_eq!(
            admit_tick(p, cur, &geom, &cfg),
            Ok(ScratchBudget::default())
        );
        let mut g = MomentumGlow::default();
        for _ in 0..12 {
            g.on_key(clock.origin, TAU);
        }
        let (mut out, mut under) = (Vec::new(), Vec::new());
        let (frame, _, pushed) = hosted_tick(
            &mut g,
            p,
            cur,
            clock.origin,
            geom,
            &cfg,
            &mut out,
            &mut under,
        )
        .expect("admitted");
        assert_eq!(
            (frame, pushed),
            (MomentumGlowFrame::default(), Pushed::default())
        );
        assert_eq!((out.capacity(), under.capacity()), (0, 0));
    }
}

#[test]
fn a_profile_wider_than_the_envelope_is_held_to_the_envelope() {
    let e = &MOMENTUM_ENVELOPE;
    let up = |x: f32| f32::from_bits(x.to_bits() + 1);
    let wide = MomentumProfile {
        max_extent_px: u32::MAX,
        max_radius_cells: f32::MAX,
        max_tau_s: f32::INFINITY,
        max_frame_interval: Duration::MAX,
        horizon: Duration::MAX,
    };
    let geom = Geom {
        cols: 65_536,
        cw: 1,
        ..plain()
    };
    assert_eq!(
        admit_tick(&wide, Some((0, 0)), &geom, &config(true, 0)),
        Err(AdmitRefusal::Geometry(GeometryFault::GridWidth {
            origin_x: 0,
            cols: 65_536,
            cw: 1,
            limit: 65_535,
        }))
    );
    let at_radius = |r: f32| MomentumGlowConfig {
        radius_cells: r,
        ..config(true, 0)
    };
    assert!(
        admit_tick(
            &wide,
            Some((0, 0)),
            &plain(),
            &at_radius(e.max_radius_cells)
        )
        .is_ok()
    );
    assert_eq!(
        admit_tick(
            &wide,
            Some((0, 0)),
            &plain(),
            &at_radius(up(e.max_radius_cells))
        ),
        Err(AdmitRefusal::Radius(up(e.max_radius_cells).to_bits()))
    );
    assert_eq!(admit_tau(&wide, e.max_tau_s), Ok(()));
    assert_eq!(
        admit_tau(&wide, up(e.max_tau_s)),
        Err(AdmitRefusal::Tau(up(e.max_tau_s).to_bits()))
    );
    let past = e.max_frame_interval + Duration::from_nanos(1);
    assert_eq!(
        admit_frame_interval(&wide, past),
        Err(AdmitRefusal::FrameInterval(past))
    );
    let horizon = i128::try_from(e.horizon.as_nanos()).expect("fits");
    assert_eq!(admit_time(&wide, None, horizon), Ok(()));
    assert_eq!(
        admit_time(&wide, None, horizon + 1),
        Err(AdmitRefusal::Horizon(horizon + 1))
    );
    let nan = MomentumProfile {
        max_radius_cells: f32::NAN,
        max_tau_s: f32::NAN,
        ..MOMENTUM_PROFILE
    };
    assert_eq!(admit_tau(&nan, TAU), Err(AdmitRefusal::Tau(TAU.to_bits())));
    let cfg = MomentumGlowConfig {
        tau_s: TAU,
        ..at_radius(1.0)
    };
    assert_eq!(
        admit_tick(&nan, Some((0, 0)), &plain(), &cfg),
        Err(AdmitRefusal::Tau(TAU.to_bits()))
    );
}

#[test]
fn the_envelopes_largest_halo_stays_within_its_budget() {
    let e = &MOMENTUM_ENVELOPE;
    for geom in [
        Geom {
            cw: 1,
            ch: 65_535,
            rows: 1,
            cols: 65_535,
            origin_x: 0,
            origin_y: 0,
            win_w: u16::MAX,
            win_h: u16::MAX,
            head: 0,
        },
        Geom {
            cw: 35,
            ch: 35,
            rows: 1_872,
            cols: 1_872,
            origin_x: 0,
            origin_y: 0,
            win_w: u16::MAX,
            win_h: u16::MAX,
            head: 0,
        },
    ] {
        for dark in [true, false] {
            let cfg = MomentumGlowConfig {
                radius_cells: e.max_radius_cells,
                ..config(dark, 0x0050_FA7B)
            };
            let (frame, budget, _) = tick_once(e, (0, 0), geom, &cfg);
            assert!(frame.hot);
            assert!(budget.out <= dark_closed_form(&geom, cfg.radius_cells));
        }
    }
}

#[test]
fn the_budget_anchors_its_rows_where_the_halo_does() {
    let geom = padded();
    let grid = admit_geometry(&MOMENTUM_PROFILE, &geom).expect("admitted");
    assert_eq!(
        i64::from(grid.origin_y) - i64::from(grid.head),
        i64::from(geom.fx_top())
    );
    assert_eq!(
        i64::from(grid.origin_y) + i64::from(grid.rows) * i64::from(grid.ch),
        i64::from(geom.fx_bot())
    );
    assert_eq!(
        i64::from(grid.origin_x) + i64::from(grid.cols) * i64::from(grid.cw),
        i64::from(geom.fx_right())
    );
    let (_, cy) = geom.cell_center(4, 9);
    assert_eq!(cy, f32::from(grid.origin_y) + 4.5 * f32::from(grid.ch));
}
