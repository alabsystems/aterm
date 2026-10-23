// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The library lane (design §10.3): it reproduces the perf harness's M3 story through `S0`, keeps the host's rules, and names a busy re-arm, an overrun and a hang.

use std::time::Duration;

use aterm_render::GlowQuad;
use aterm_time::Instant;

use super::lane::{
    After, Exo, Glow, HANG_NS, LANE_MAX_EVENTS, LaneFault, LaneRun, Presentation,
    S0_FRAME_INTERVAL_NS, S0_TAU, SCENARIO_FORMAT, Scenario, ScenarioFault, drive_m3_story, run,
    s0, s0_geom,
};
use super::test_fixtures::{attached, attached_for_lane, cfg, hosted};
use super::{MomentumCode, MomentumEvent, MomentumHost, Sched};
use crate::cursor_glow::Geom;
use crate::cursor_momentum::{AdmitRefusal, MomentumGlow, MomentumGlowConfig, MomentumGlowFrame};

/// The M3 story's digest (FNV-1a over every frame, quad, wake and reset), as `verify perf aos` reports it.
const M3_STORY_DIGEST: u64 = 0xbf64_2e76_44df_7e95;

/// The stranded trace's digest: the story program with the demonstration fault running the lane, as `verify perf aos --stranded` reports it.
const M3_STRANDED_DIGEST: u64 = 0x54bf_a4e7_d959_dc64;

/// The story's shape: 3,265 calls (192 keys, 1 reset, 2,776 ticks, 296 queries).
fn assert_m3_shape(what: &str, r: &LaneRun) {
    let c = r.counts();
    assert_eq!(r.fault(), None, "{what}: the story runs to its end");
    assert_eq!(
        (c.keys, c.resets, c.ticks, c.queries),
        (192, 1, 2_776, 296),
        "{what}: keys, resets, ticks, queries"
    );
    assert_eq!(r.calls().len(), 3_265, "{what}: calls");
    assert_eq!(c.calls(), 3_265, "{what}: counted calls");
    assert_eq!(c.stranded, 0, "{what}: nothing stranded");
    assert_eq!(r.fnv(), M3_STORY_DIGEST, "{what}: the story digest");
}

/// Two runs are the same run: the same calls in order, ticks, counts and digest.
fn assert_same_run(what: &str, a: &LaneRun, b: &LaneRun) {
    assert_eq!(a.calls(), b.calls(), "{what}: calls");
    assert_eq!(a.ticks(), b.ticks(), "{what}: ticks");
    assert_eq!(a.counts(), b.counts(), "{what}: counts");
    assert_eq!(a.fnv(), b.fnv(), "{what}: digest");
}

/// The library lane runs the frozen `S0`, the story program's own timeline, to the M3 story through the bare engine, an unattached host and an attached host that logs exactly its calls.
#[test]
fn the_library_lane_reproduces_the_m3_story() {
    let t0 = Instant::now();
    let program = drive_m3_story(&mut MomentumGlow::default(), t0);
    assert_m3_shape("the story program", &program);
    let frozen = s0();
    assert_eq!(
        program.scenario(),
        &frozen,
        "S0's pinned timeline is the program's under the shipped engine"
    );
    assert_eq!(frozen.validate(), Ok(()), "S0 is admitted");
    assert_eq!(frozen.keys(), 192);
    assert_eq!(program.span_ns(), frozen.end_ns());

    let direct = run(&mut MomentumGlow::default(), &frozen, t0);
    assert_m3_shape("direct", &direct);
    assert_same_run("direct vs the program", &direct, &program);

    let mut host = MomentumHost::default();
    let unattached = run(&mut host, &frozen, t0);
    assert!(!host.aos_attached());
    assert_m3_shape("unattached host", &unattached);
    assert_same_run("unattached host vs direct", &unattached, &direct);

    let mut host = attached(t0, MomentumCode::Shipped);
    let attached_run = run(&mut host, &frozen, t0);
    assert_m3_shape("attached shipped host", &attached_run);
    assert_same_run("attached shipped host vs direct", &attached_run, &direct);
    let log = hosted(&host).boundary.log().expect("attached");
    assert_eq!(
        log.inputs(),
        attached_run.calls(),
        "the attached host logged the lane's calls, once each"
    );

    // And on another origin: only offsets reach the engine.
    let later = t0 + Duration::from_secs(9);
    assert_same_run(
        "another origin",
        &run(&mut MomentumGlow::default(), &frozen, later),
        &direct,
    );
    let c = direct.counts();
    println!(
        "S0 under the shipped engine: T {} P {} Z {} queries {} parks {}",
        c.ticks,
        c.presented,
        c.wasted(),
        c.queries,
        c.parks
    );
}

/// The story program under the demonstration fault is the stranded trace `verify perf aos` reports: its lane parks while the glow is hot, so its bursts start where that lane parked, not where `S0`'s do.
#[test]
fn the_story_program_under_the_demo_fault_is_the_stranded_trace() {
    let t0 = Instant::now();
    let mut host = attached(t0, MomentumCode::DemoStrandedSettle);
    let stranded = drive_m3_story(&mut host, t0);
    assert_eq!(stranded.fault(), None);
    assert!(stranded.counts().stranded > 0, "the fault strands the lane");
    assert_eq!(stranded.fnv(), M3_STRANDED_DIGEST);
    assert_ne!(
        stranded.scenario(),
        &s0(),
        "the stranded program's timeline moved with its code"
    );
    let again = drive_m3_story(&mut attached(t0, MomentumCode::DemoStrandedSettle), t0);
    assert_same_run("two stranded drives", &stranded, &again);
}

const F: u64 = 16_666_667;

fn present(col: u16) -> Presentation {
    Presentation::of(Some((3, col)), &s0_geom(), &cfg(true))
}

fn key() -> Exo {
    Exo::Key {
        tau_bits: S0_TAU.to_bits(),
    }
}

fn scenario(events: Vec<(u64, Exo)>, end: u64) -> Scenario {
    Scenario::new(F, present(10), events, end).expect("fixture: an admitted scenario")
}

/// The lane keeps the host's rules: inputs before a due tick, cadence while active, the named wake while settling, a park until the next key, and nothing after the end.
#[test]
fn the_lane_keeps_the_host_rules() {
    let t0 = Instant::now();
    let k0 = 5_000_000;
    let events = vec![
        (k0, key()),
        (k0, Exo::Present(present(11))),
        (F + 1, Exo::Present(present(12))),
        // A key exactly on a due boundary (the hot glow's cadence tick at 2F) comes first.
        (2 * F, key()),
        (2 * F, Exo::Present(present(13))),
        // Long after the cool-down: the lane is parked and this key wakes it.
        (20_000_000_000, key()),
    ];
    let sc = scenario(events, 20_000_000_000 + 2 * F);
    let r = run(&mut MomentumGlow::default(), &sc, t0);
    assert_eq!(r.fault(), None);
    let ticks = r.ticks();
    assert_eq!(ticks[0].at, F, "the first key ticks at the next boundary");
    let calls = r.calls();
    let Some(MomentumEvent::Tick { cur, .. }) = calls.get(ticks[0].call) else {
        panic!("the first tick is a tick call");
    };
    assert_eq!(
        *cur,
        Some((3, 11)),
        "the inputs at the key's instant came first"
    );
    assert!(
        ticks.iter().all(|t| t.at != F + 1),
        "a presentation change draws nothing"
    );
    let at_2f = ticks.iter().find(|t| t.at == 2 * F).expect("a tick at 2F");
    let key_ix = calls
        .iter()
        .position(|c| matches!(c, MomentumEvent::Key { at, .. } if *at == i128::from(2 * F)))
        .expect("the second key");
    assert!(key_ix < at_2f.call, "the key at a due boundary comes first");
    assert!(matches!(
        calls[at_2f.call],
        MomentumEvent::Tick {
            cur: Some((3, 13)),
            ..
        }
    ));
    for (i, t) in ticks.iter().enumerate() {
        let next = ticks.get(i + 1).map(|n| n.at);
        match t.after {
            After::Cadence { at } => {
                assert_eq!(at, (t.at / F + 1) * F, "active: the next frame boundary");
                assert!(next.is_none_or(|n| n <= at), "nothing later is due first");
            }
            After::Wake { query, wake } => {
                let w = wake.expect("the shipped glow names its wake");
                assert!(w > t.at);
                assert!(
                    matches!(calls[query], MomentumEvent::Query { at, .. } if at == i128::from(t.at))
                );
                assert!(next.is_none_or(|n| n <= w));
            }
            After::Park => {
                assert!(!t.frame.hot, "the shipped lane parks only cold");
                assert!(
                    next.is_none_or(|n| n > 20_000_000_000),
                    "parked until the next key"
                );
            }
        }
    }
    let last = ticks.last().expect("ticks");
    assert!(last.at <= sc.end_ns(), "nothing is drawn after the end");
    let c = r.counts();
    assert_eq!((c.keys, c.resets), (3, 0));
    assert_eq!(c.ticks, u64::try_from(ticks.len()).unwrap_or(0));
    assert_eq!(
        c.presented,
        ticks.iter().filter(|t| t.presented).count() as u64
    );
    assert_eq!(c.presented + c.wasted(), c.ticks);
    assert!(
        ticks[0].presented,
        "the first hot frame differs from the inert glass"
    );
}

/// A frame counts in `P` exactly when it differs from the one on glass, the previous tick's or the inert frame before the first.
#[test]
fn p_counts_frames_that_differ_from_the_glass() {
    let t0 = Instant::now();
    // A disabled glow draws the inert frame on every tick: nothing is ever presented.
    let mut off = cfg(true);
    off.enabled = false;
    let dark = Presentation::of(Some((3, 10)), &s0_geom(), &off);
    let sc =
        Scenario::new(F, dark, vec![(0, key()), (F / 2, key())], 3 * F).expect("fixture: admitted");
    let r = run(&mut MomentumGlow::default(), &sc, t0);
    assert!(r.counts().ticks > 0);
    assert_eq!(
        r.counts().presented,
        0,
        "an inert frame on inert glass is not presented"
    );
    let lit = run(&mut MomentumGlow::default(), &s0(), t0);
    let mut glass: Option<(MomentumGlowFrame, usize, usize, u64)> = None;
    for t in lit.ticks() {
        let now = (t.frame, t.out, t.under, t.key);
        let changed = glass.map_or(
            t.frame != MomentumGlowFrame::default() || t.out + t.under > 0,
            |g| g != now,
        );
        assert_eq!(t.presented, changed, "the tick at {} ns", t.at);
        glass = Some(now);
    }
}

/// A reset of a glow at rest (before the first key, or after a reset with no key since) is no call on any path, bare, unattached or attached, while a reset after a key is one call.
#[test]
fn a_reset_of_a_glow_at_rest_is_no_call() {
    let t0 = Instant::now();
    let kept = vec![
        (10_000_000, key()),
        (60_000_000, key()),
        (500_000_000, Exo::Reset),
        (1_000_000_000, key()),
    ];
    let mut resting = vec![(0, Exo::Reset), (5_000_000, Exo::Reset)];
    resting.extend_from_slice(&kept[..3]);
    resting.push((501_000_000, Exo::Reset));
    resting.push((502_000_000, Exo::Reset));
    resting.push(kept[3]);
    let end = 30_000_000_000;
    let (with, without) = (scenario(resting, end), scenario(kept, end));
    let plain = run(&mut MomentumGlow::default(), &without, t0);
    assert_eq!(plain.fault(), None);
    assert_eq!(plain.counts().resets, 1, "the reset after a key is a call");
    let direct = run(&mut MomentumGlow::default(), &with, t0);
    assert_same_run("the bare engine", &direct, &plain);
    let unattached = run(&mut MomentumHost::default(), &with, t0);
    assert_same_run("an unattached host", &unattached, &plain);
    let mut host = attached_for_lane(t0, Sched::Prebuilt(MomentumCode::Shipped));
    let r = run(&mut host, &with, t0);
    assert_same_run("an attached host", &r, &plain);
    let log = hosted(&host).boundary.log().expect("attached");
    assert_eq!(log.inputs(), r.calls(), "the host logged the lane's calls");
    assert_eq!(
        log.inputs()
            .iter()
            .filter(|e| matches!(e, MomentumEvent::Reset))
            .count(),
        1,
        "one reset delivered"
    );
}

/// The lane stops at a busy re-arm, names a hang when a glow never stops asking for frames, and stops a one-nanosecond creep at its event bound.
#[test]
fn the_lane_stops_and_names_a_rearm_a_hang_and_its_event_bound() {
    /// The engine, with the scheduler answer replaced.
    struct Rigged {
        g: MomentumGlow,
        active: bool,
        settles: bool,
        wake: fn(Instant) -> Option<Instant>,
    }
    impl Glow for Rigged {
        fn key(&mut self, now: Instant, tau: f32) {
            self.g.on_key(now, tau);
        }
        fn rest(&mut self) {
            self.g.reset();
        }
        fn frame(
            &mut self,
            cur: Option<(u16, u16)>,
            now: Instant,
            geom: Geom,
            cfg: &MomentumGlowConfig,
            out: &mut Vec<GlowQuad>,
            under: &mut Vec<GlowQuad>,
        ) -> MomentumGlowFrame {
            self.g.tick(cur, now, geom, cfg, out, under)
        }
        fn active(&self) -> bool {
            self.active
        }
        fn settles(&self) -> bool {
            self.settles
        }
        fn wake(&mut self, now: Instant, _lane: Duration) -> Option<Instant> {
            (self.wake)(now)
        }
    }
    let t0 = Instant::now();
    let sc = scenario(vec![(0, key())], 100_000_000_000);

    let mut rearm = Rigged {
        g: MomentumGlow::default(),
        active: false,
        settles: true,
        wake: Some,
    };
    let r = run(&mut rearm, &sc, t0);
    assert_eq!(
        r.fault(),
        Some(LaneFault::WakeNotAfter {
            ticks: 1,
            at: F,
            wake: F
        })
    );
    assert_eq!(r.counts().ticks, 1, "the lane stopped at the re-arm");

    let mut busy = Rigged {
        g: MomentumGlow::default(),
        active: true,
        settles: false,
        wake: |_| None,
    };
    let r = run(&mut busy, &sc, t0);
    match r.fault() {
        Some(LaneFault::Hang { at, since: 0, .. }) => assert!(at > HANG_NS),
        other => panic!("a lane that never parks hangs: {other:?}"),
    }
    assert!(r.ticks().iter().all(|t| t.at <= HANG_NS));

    let mut creep = Rigged {
        g: MomentumGlow::default(),
        active: false,
        settles: true,
        wake: |now| Some(now + Duration::from_nanos(1)),
    };
    let r = run(&mut creep, &sc, t0);
    assert!(matches!(r.fault(), Some(LaneFault::TooManyEvents { .. })));
    assert!(r.calls().len() <= LANE_MAX_EVENTS);
    assert!(r.calls().len() + 2 > LANE_MAX_EVENTS, "it ran to its bound");
}

/// A scenario is admitted whole or refused naming what breaks: the frame interval, the start, the horizon, an input out of order or past the end, or an input admission refuses.
#[test]
fn a_scenario_is_admitted_or_refused_by_name() {
    let p = present(10);
    assert_eq!(
        Scenario::new(0, p, vec![], 1),
        Err(ScenarioFault::FrameInterval(Duration::ZERO))
    );
    assert_eq!(
        Scenario::new(2_000_000_000, p, vec![], 1),
        Err(ScenarioFault::FrameInterval(Duration::from_secs(2)))
    );
    let mut degenerate = s0_geom();
    degenerate.cw = 0;
    let bad = Presentation::of(Some((3, 10)), &degenerate, &cfg(true));
    assert!(matches!(
        Scenario::new(F, bad, vec![], 1),
        Err(ScenarioFault::Refused {
            index: None,
            refusal: AdmitRefusal::Geometry(_)
        })
    ));
    assert!(matches!(
        Scenario::new(F, p, vec![(5, key()), (4, key())], 10),
        Err(ScenarioFault::Order {
            index: 1,
            at: 4,
            before: 5
        })
    ));
    assert!(matches!(
        Scenario::new(F, p, vec![(11, key())], 10),
        Err(ScenarioFault::PastEnd {
            index: 0,
            at: 11,
            end: 10
        })
    ));
    assert!(matches!(
        Scenario::new(
            F,
            p,
            vec![(
                1,
                Exo::Key {
                    tau_bits: f32::NAN.to_bits()
                }
            )],
            10
        ),
        Err(ScenarioFault::Refused {
            index: Some(0),
            refusal: AdmitRefusal::Tau(_)
        })
    ));
    assert!(matches!(
        Scenario::new(F, p, vec![(1, Exo::Present(bad))], 10),
        Err(ScenarioFault::Refused {
            index: Some(0),
            refusal: AdmitRefusal::Geometry(_)
        })
    ));
    assert!(matches!(
        Scenario::new(F, p, vec![], u64::MAX),
        Err(ScenarioFault::Refused {
            index: None,
            refusal: AdmitRefusal::Horizon(_)
        })
    ));
    assert_eq!(S0_FRAME_INTERVAL_NS, F);
}

/// The encoding opens with the format byte and changes with every part of the scenario: the frame interval, the end, the start, and each input's time, kind and presentation.
#[test]
fn a_scenario_encoding_names_every_part() {
    let base = s0();
    let bytes = base.encode();
    assert_eq!(bytes.first(), Some(&SCENARIO_FORMAT));
    assert_eq!(base.digest(), s0().digest());
    let with = |f: u64, start: Presentation, events: Vec<(u64, Exo)>, end: u64| {
        Scenario::new(f, start, events, end).expect("fixture: admitted")
    };
    let (f, start, events, end) = (
        base.frame_interval_ns(),
        *base.start(),
        base.events().to_vec(),
        base.end_ns(),
    );
    let mut variants = vec![
        with(f + 1, start, events.clone(), end),
        with(f, start, events.clone(), end + 1),
        with(f, present(50), events.clone(), end),
    ];
    let mut later = events.clone();
    later[5].0 += 1;
    variants.push(with(f, start, later, end));
    let mut reset = events.clone();
    reset[0].1 = Exo::Reset;
    variants.push(with(f, start, reset, end));
    let mut moved = events.clone();
    moved[1].1 = Exo::Present(present(77));
    variants.push(with(f, start, moved, end));
    let mut fewer = events;
    fewer.pop();
    variants.push(with(f, start, fewer, end));
    for v in &variants {
        assert_ne!(v.encode(), bytes);
        assert_ne!(v.digest(), base.digest());
    }
}
