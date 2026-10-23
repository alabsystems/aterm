// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Event-script test fixtures and admission checks under [`MOMENTUM_PROFILE`] for momentum glow.

use std::time::Duration;

use aterm_render::GlowQuad;
use aterm_time::Instant;

use super::{
    MOMENTUM_GLOW_RADIUS_CELLS, MOMENTUM_GLOW_TAU_S, MOMENTUM_PROFILE, MomentumGlow,
    MomentumGlowConfig, MomentumGlowFrame, admit_frame_interval, admit_tau, admit_tick, admit_time,
};
use crate::cursor_glow::Geom;

/// One millisecond, in the fixtures' nanosecond offsets.
pub(super) const MS: i128 = 1_000_000;

pub(super) const TAU: f32 = MOMENTUM_GLOW_TAU_S;

/// One input to the glow.
#[derive(Clone, Copy)]
pub(super) enum Ev {
    Key {
        at: i128,
        tau: f32,
    },
    Reset,
    Tick {
        cur: Option<(u16, u16)>,
        at: i128,
        geom: Geom,
        cfg: MomentumGlowConfig,
    },
    Query {
        at: i128,
        lane: Duration,
    },
}

/// Observed result of one input, with instants expressed as offsets from the clock origin.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Obs {
    Keyed,
    WasReset,
    Frame {
        frame: MomentumGlowFrame,
        out: Vec<GlowQuad>,
        under: Vec<GlowQuad>,
    },
    Schedule {
        is_active: bool,
        settling: bool,
        cold_at: Option<i128>,
        deadline: Option<i128>,
        value: u32,
    },
}

/// Maps the fixtures' offsets onto a real `Instant`.
#[derive(Clone, Copy)]
pub(super) struct Clock {
    pub(super) origin: Instant,
}

impl Clock {
    /// A clock a day ahead of now, so offsets on either side are representable.
    pub(super) fn ahead(extra: Duration) -> Self {
        Self {
            origin: Instant::now() + Duration::from_secs(86_400) + extra,
        }
    }

    pub(super) fn at(&self, offset: i128) -> Instant {
        let span = Duration::from_nanos(
            u64::try_from(offset.unsigned_abs()).expect("fixture offsets fit u64 ns"),
        );
        if offset >= 0 {
            self.origin.checked_add(span)
        } else {
            self.origin.checked_sub(span)
        }
        .expect("fixture instants are representable")
    }

    pub(super) fn offset(&self, t: Instant) -> i128 {
        match t.checked_duration_since(self.origin) {
            Some(after) => i128::try_from(after.as_nanos()).expect("fits"),
            None => -i128::try_from(self.origin.duration_since(t).as_nanos()).expect("fits"),
        }
    }
}

/// Applies one input, asserting the tick stays inside its admitted scratch.
pub(super) fn step(g: &mut MomentumGlow, clock: &Clock, ev: &Ev) -> Obs {
    match *ev {
        Ev::Key { at, tau } => {
            g.on_key(clock.at(at), tau);
            Obs::Keyed
        }
        Ev::Reset => {
            g.reset();
            Obs::WasReset
        }
        Ev::Tick { cur, at, geom, cfg } => {
            let budget = admit_tick(&MOMENTUM_PROFILE, cur, &geom, &cfg)
                .expect("every fixture tick is admitted");
            let mut out = Vec::with_capacity(budget.out);
            let mut under = Vec::with_capacity(budget.under);
            let reserved = (out.capacity(), under.capacity());
            let frame = g.tick(cur, clock.at(at), geom, &cfg, &mut out, &mut under);
            assert!(
                out.len() <= budget.out && under.len() <= budget.under,
                "a fixture tick pushed ({}, {}) past its budget {budget:?}",
                out.len(),
                under.len()
            );
            assert_eq!(
                reserved,
                (out.capacity(), under.capacity()),
                "a fixture tick reallocated its reserved scratch"
            );
            Obs::Frame { frame, out, under }
        }
        Ev::Query { at, lane } => {
            let now = clock.at(at);
            Obs::Schedule {
                is_active: g.is_active(),
                settling: g.settling(),
                cold_at: g.cold_at().map(|t| clock.offset(t)),
                deadline: g.next_change_deadline(now, lane).map(|t| clock.offset(t)),
                value: g.value(now, TAU).to_bits(),
            }
        }
    }
}

/// Assert every key, query and instant in `evs` is admitted under [`MOMENTUM_PROFILE`] (ticks are
/// admitted by [`step`]).
pub(super) fn assert_admitted(evs: &[Ev]) {
    let p = &MOMENTUM_PROFILE;
    let mut last = None;
    for ev in evs {
        let at = match *ev {
            Ev::Key { at, tau } => {
                admit_tau(p, tau).expect("fixture key tau is admitted");
                Some(at)
            }
            Ev::Reset => None,
            Ev::Tick { at, .. } => Some(at),
            Ev::Query { at, lane } => {
                admit_frame_interval(p, lane).expect("fixture lane is admitted");
                Some(at)
            }
        };
        if let Some(at) = at {
            admit_time(p, last, at).expect("fixture time is admitted");
            last = Some(at);
        }
    }
}

/// Script recorder backed by a scratch [`MomentumGlow`].
pub(super) struct Recorder {
    pub(super) evs: Vec<Ev>,
    g: MomentumGlow,
    clock: Clock,
}

impl Recorder {
    pub(super) fn new() -> Self {
        Self {
            evs: Vec::new(),
            g: MomentumGlow::default(),
            clock: Clock::ahead(Duration::ZERO),
        }
    }

    pub(super) fn push(&mut self, ev: Ev) -> Obs {
        let obs = step(&mut self.g, &self.clock, &ev);
        self.evs.push(ev);
        obs
    }

    /// Advance along `next_change_deadline` until the glow parks, returning the park offset.
    pub(super) fn cool_down(
        &mut self,
        mut now: i128,
        cur: Option<(u16, u16)>,
        geom: Geom,
        cfg: MomentumGlowConfig,
        lane: Duration,
    ) -> i128 {
        let give_up = now + 60_000 * MS;
        loop {
            let Obs::Schedule { deadline, .. } = self.push(Ev::Query { at: now, lane }) else {
                panic!("a query observes the schedule");
            };
            let Some(next) = deadline else {
                return now;
            };
            assert!(
                next > now && next < give_up,
                "the fixture's glow never parked"
            );
            now = next;
            self.push(Ev::Tick {
                cur,
                at: now,
                geom,
                cfg,
            });
        }
    }
}

/// 10×20 cells, 24×80, at the window's origin.
pub(super) fn plain() -> Geom {
    Geom {
        cw: 10,
        ch: 20,
        rows: 24,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 800,
        win_h: 480,
        head: 0,
    }
}

/// A real window: 15×28 cells, padded, under an 80 px head band.
pub(super) fn padded() -> Geom {
    Geom {
        cw: 15,
        ch: 28,
        rows: 24,
        cols: 100,
        origin_x: 24,
        origin_y: 84,
        win_w: 1548,
        win_h: 780,
        head: 80,
    }
}

/// The host's configuration on a dark or a light page, in `hue`.
pub(super) fn config(dark: bool, hue: u32) -> MomentumGlowConfig {
    MomentumGlowConfig {
        enabled: true,
        intensity: 1.0,
        tau_s: TAU,
        radius_cells: MOMENTUM_GLOW_RADIUS_CELLS,
        base: hue,
        dark_theme: dark,
        hue,
        theme_fg: if dark { 0x00C8_C8D2 } else { 0x0024_292F },
        theme_bg: if dark { 0x0010_1016 } else { 0x00FF_FFFF },
        block: true,
        glyph_span: None,
    }
}

/// Dark-page typing burst followed by a 60 Hz cool-down and cold tick.
pub(super) fn dark_burst_and_cooldown() -> Vec<Ev> {
    let mut r = Recorder::new();
    let geom = padded();
    let cfg = MomentumGlowConfig {
        intensity: 0.25,
        ..config(true, 0x0050_FA7B)
    };
    let lane = Duration::from_micros(16_667);
    let mut now = 0;
    for k in 0..6u16 {
        now = i128::from(k) * 60 * MS;
        r.push(Ev::Key { at: now, tau: TAU });
        r.push(Ev::Tick {
            cur: Some((4, 10 + k)),
            at: now,
            geom,
            cfg,
        });
        r.push(Ev::Query { at: now, lane });
    }
    let parked = r.cool_down(now, Some((4, 15)), geom, cfg, lane);
    let later = parked + 1_000 * MS;
    r.push(Ev::Tick {
        cur: Some((4, 15)),
        at: later,
        geom,
        cfg,
    });
    r.push(Ev::Query { at: later, lane });
    r.evs
}

/// Light-page typing burst with hue/ink/caret transitions and a 30 Hz cool-down.
pub(super) fn light_page_burst() -> Vec<Ev> {
    let mut r = Recorder::new();
    let geom = plain();
    let lane = Duration::from_micros(33_333);
    let mut now = 0;
    let mut cfg = config(false, 0x0009_69DA);
    for k in 0..6u16 {
        now = i128::from(k) * 90 * MS;
        let col = 20 + k;
        cfg = MomentumGlowConfig {
            intensity: 0.3,
            glyph_span: Some((2, i32::from(col))),
            hue: if k >= 3 { 0x00CF_222E } else { 0x0009_69DA },
            theme_fg: if k == 4 { 0xFF24_292F } else { 0x0024_292F },
            block: k != 5,
            ..cfg
        };
        r.push(Ev::Key { at: now, tau: TAU });
        r.push(Ev::Tick {
            cur: Some((6, col)),
            at: now,
            geom,
            cfg,
        });
        r.push(Ev::Query { at: now, lane });
    }
    let parked = r.cool_down(now, Some((6, 25)), geom, cfg, lane);
    r.push(Ev::Tick {
        cur: Some((6, 25)),
        at: parked + 500 * MS,
        geom,
        cfg,
    });
    r.evs
}

/// Event script covering resets, inert arms, and boundary τ values.
pub(super) fn resets_and_inert_arms() -> Vec<Ev> {
    let mut r = Recorder::new();
    let (dark, light) = (padded(), plain());
    let lane = Duration::from_micros(16_667);
    let hot = MomentumGlowConfig {
        tau_s: 1.5,
        intensity: 0.5,
        ..config(true, 0x00A0_A0A0)
    };
    let tick = |r: &mut Recorder, cur, at, geom, cfg| {
        r.push(Ev::Tick { cur, at, geom, cfg });
        r.push(Ev::Query { at, lane });
    };
    r.push(Ev::Key { at: 0, tau: 0.8 });
    tick(&mut r, Some((3, 3)), 0, dark, hot);
    tick(&mut r, Some((3, 3)), 0, dark, hot);
    tick(&mut r, None, 10 * MS, dark, hot);
    r.push(Ev::Key {
        at: 20 * MS,
        tau: 0.8,
    });
    tick(&mut r, Some((3, 4)), 20 * MS, dark, hot);
    tick(&mut r, Some((24, 0)), 30 * MS, dark, hot);
    tick(
        &mut r,
        Some((3, 4)),
        40 * MS,
        dark,
        MomentumGlowConfig {
            enabled: false,
            ..hot
        },
    );
    tick(
        &mut r,
        Some((3, 4)),
        50 * MS,
        dark,
        MomentumGlowConfig {
            intensity: 0.0,
            ..hot
        },
    );
    r.push(Ev::Key {
        at: 60 * MS,
        tau: TAU,
    });
    r.push(Ev::Key {
        at: 60 * MS,
        tau: TAU,
    });
    let paper = MomentumGlowConfig {
        glyph_span: Some((0, 7)),
        ..config(false, 0x001A_7F37)
    };
    tick(&mut r, Some((5, 7)), 60 * MS, light, paper);
    r.push(Ev::Reset);
    r.push(Ev::Query { at: 70 * MS, lane });
    tick(&mut r, Some((5, 7)), 80 * MS, light, paper);
    r.push(Ev::Key {
        at: 90 * MS,
        tau: MOMENTUM_PROFILE.max_tau_s,
    });
    let slow = MomentumGlowConfig {
        tau_s: MOMENTUM_PROFILE.max_tau_s,
        ..hot
    };
    tick(&mut r, Some((23, 99)), 90 * MS, dark, slow);
    tick(&mut r, Some((23, 99)), 5_000 * MS, dark, slow);
    let quick = MomentumGlowConfig { tau_s: 0.05, ..hot };
    tick(&mut r, Some((23, 99)), 8_000 * MS, dark, quick);
    r.push(Ev::Key {
        at: 9_000 * MS,
        tau: TAU,
    });
    tick(&mut r, Some((23, 79)), 9_000 * MS, light, paper);
    tick(&mut r, Some((23, 79)), 9_030 * MS, light, paper);
    r.evs
}

/// Every fixture, named.
pub(super) fn fixtures() -> [(&'static str, Vec<Ev>); 3] {
    [
        ("dark burst and cool-down", dark_burst_and_cooldown()),
        ("light page burst", light_page_burst()),
        ("resets and inert arms", resets_and_inert_arms()),
    ]
}
