// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `momentum-glow.lane-fidelity` v1 corpus calibration and usefulness tests.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use aterm_aos::ArtifactKind;
use aterm_aos::contract::Verdict;
use aterm_aos::task::state_digest;
use aterm_aos::wire::Enc;
use aterm_render::GlowQuad;
use aterm_time::Instant;

use super::bundle::RowVerdict;
use super::corpus::{Corpus, CorpusSplit};
use super::fidelity::{
    CLEARED_BY_END, LANE_FIDELITY_NAME, LANE_SETTLES, LaneFidelity, NO_STEP_SLEPT, ORACLE,
    PROBE_DELTA,
};
use super::lane::{
    After, Exo, Glow, HANG_NS, LANE_MAX_EVENTS, LANE_MODEL, LaneFault, LaneRun, Presentation,
    S0_TAU, Scenario, encode_glass, run, s0, s0_geom,
};
use super::m4_tests::evaluator;
use super::plan::{DEMO_PLAN, SHIPPED_PLAN, SchedulePlan, Split, Wake};
use super::test_fixtures::{
    attached_for_lane, cfg, hosted, judged_log, refused_off_the_pinned_target,
};
use super::{
    Applied, MomentumCode, MomentumEvent, MomentumHost, MomentumState, NoStrandedLight, Sched,
    WAKE_NO_LATER_THAN_CLEAR,
};
use crate::cursor_glow::Geom;
use crate::cursor_momentum::{MomentumGlow, MomentumGlowConfig, MomentumGlowFrame};

fn corpus() -> Corpus {
    Corpus::generate().expect("generator v1's scenarios are admitted")
}

fn lane_items(c: &Corpus) -> Vec<(&str, CorpusSplit, &Scenario)> {
    c.items()
        .iter()
        .filter_map(|i| i.scenario().map(|s| (i.id(), i.split(), s)))
        .collect()
}

fn plan(split: Split, wake: Wake) -> SchedulePlan {
    SchedulePlan::new(split, wake).expect("fixture: an in-range plan")
}

const SHIPPED: Sched = Sched::Prebuilt(MomentumCode::Shipped);

fn hosted_run(t0: Instant, sched: Sched, s: &Scenario) -> (LaneRun, MomentumHost) {
    let mut h = attached_for_lane(t0, sched);
    let r = run(&mut h, s, t0);
    let log = hosted(&h).boundary.log().expect("fixture: attached");
    assert_eq!(log.inputs(), r.calls(), "the host logged the whole run");
    (r, h)
}

fn law(v: &Verdict) -> Option<&'static str> {
    match v {
        Verdict::Pass => None,
        Verdict::Fail { law, .. } => Some(law),
    }
}

fn cursor(v: &Verdict) -> Option<usize> {
    match v {
        Verdict::Pass => None,
        Verdict::Fail { cursor, .. } => usize::try_from(*cursor).ok(),
    }
}

fn assert_same_run(what: &str, a: &LaneRun, b: &LaneRun) {
    assert_eq!(a.calls(), b.calls(), "{what}: calls");
    assert_eq!(a.ticks(), b.ticks(), "{what}: ticks");
    assert_eq!(a.counts(), b.counts(), "{what}: counts");
    assert_eq!(a.fnv(), b.fnv(), "{what}: digest");
}

struct Rigged {
    g: MomentumGlow,
    always_active: bool,
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
        self.always_active || self.g.is_active()
    }
    fn settles(&self) -> bool {
        !self.always_active && self.g.settling()
    }
    fn wake(&mut self, now: Instant, _lane: Duration) -> Option<Instant> {
        (self.wake)(now)
    }
}

fn intervals(r: &LaneRun, mut each: impl FnMut(u64, u64, &MomentumGlow, &MomentumEvent, &[u8])) {
    let s = r.scenario();
    let (origin, inputs, ticks) = (r.origin(), s.events(), r.ticks());
    let mut g = MomentumGlow::default();
    let (mut out, mut under) = (Vec::new(), Vec::new());
    let mut n = 0usize;
    for call in r.calls() {
        match *call {
            MomentumEvent::Key { at, tau_bits, .. } => g.on_key(
                origin + Duration::from_nanos(u64::try_from(at).unwrap_or(0)),
                f32::from_bits(tau_bits),
            ),
            MomentumEvent::Reset => g.reset(),
            MomentumEvent::Query { .. } => {}
            MomentumEvent::Tick { cur, at, geom, cfg } => {
                let p = u64::try_from(at).unwrap_or(0);
                out.clear();
                under.clear();
                let frame = g.tick(
                    cur,
                    origin + Duration::from_nanos(p),
                    geom.geom(),
                    &cfg.config(),
                    &mut out,
                    &mut under,
                );
                let mut drawn = Enc::new();
                encode_glass(&mut drawn, &frame, &out, &under);
                n += 1;
                let next = [
                    ticks.get(n).map(|t| t.at),
                    inputs.iter().map(|&(t, _)| t).find(|&t| t > p),
                    Some(s.end_ns()),
                ]
                .into_iter()
                .flatten()
                .min()
                .unwrap_or(p);
                each(p, next, &g, call, drawn.as_bytes());
            }
        }
    }
}

fn probe(origin: Instant, g: &MomentumGlow, call: &MomentumEvent, s: u64) -> Vec<u8> {
    let MomentumEvent::Tick { cur, geom, cfg, .. } = *call else {
        panic!("fixture: a tick call");
    };
    let mut copy = *g;
    let (mut out, mut under) = (Vec::new(), Vec::new());
    let frame = copy.tick(
        cur,
        origin + Duration::from_nanos(s),
        geom.geom(),
        &cfg.config(),
        &mut out,
        &mut under,
    );
    let mut e = Enc::new();
    encode_glass(&mut e, &frame, &out, &under);
    e.as_bytes().to_vec()
}

#[test]
fn the_lane_fidelity_artifact_names_its_laws() {
    let id = LaneFidelity::artifact();
    assert_eq!(id.kind, ArtifactKind::Contract);
    assert_eq!(id.name, LANE_FIDELITY_NAME);
    assert_eq!(id.name, "momentum-glow.lane-fidelity");
    assert_eq!(id.version, 1);
    assert_eq!(PROBE_DELTA, Duration::from_millis(2));
    assert_eq!(
        (NO_STEP_SLEPT, CLEARED_BY_END, LANE_SETTLES),
        ("NoStepSlept", "ClearedByEnd", "LaneSettles")
    );
    let mut e = Enc::new();
    e.str("NoStepSlept");
    e.str("ClearedByEnd");
    e.str("LaneSettles");
    e.duration(Duration::from_millis(2));
    e.u64(65_536);
    e.u64(60_000_000_000);
    e.str(LANE_MODEL);
    e.str(ORACLE);
    assert_eq!(
        id,
        aterm_aos::ArtifactId::new(ArtifactKind::Contract, LANE_FIDELITY_NAME, 1, e.as_bytes())
    );
    assert_eq!(u64::try_from(LANE_MAX_EVENTS).ok(), Some(65_536));
    assert_eq!(HANG_NS, 60_000_000_000);
    assert_eq!(LaneFidelity::new().id(), &id);
    assert_ne!(id, NoStrandedLight::artifact());
    assert_eq!(
        id.digest.hex(),
        LANE_FIDELITY_ID,
        "the contract is fixed before any proposer exists"
    );
}

const LANE_FIDELITY_ID: &str = "d3fab71f2fcffd4b30d94a49b526527541efadb77a3a603beef09da7ad2c5b33";

#[test]
fn shipped_passes_lane_fidelity_on_every_item() {
    let t0 = Instant::now();
    let c = corpus();
    let judge = LaneFidelity::new();
    let mut judged = [0usize; 2];
    for (id, split, s) in lane_items(&c) {
        let direct = run(&mut MomentumGlow::default(), s, t0);
        assert_eq!(direct.fault(), None, "{id}: the run reaches its end");
        assert_eq!(judge.judge(&direct), Verdict::Pass, "{id}: the bare engine");
        let mut host = MomentumHost::default();
        let unattached = run(&mut host, s, t0);
        assert_same_run(&format!("{id}: unattached"), &unattached, &direct);
        assert_eq!(judge.judge(&unattached), Verdict::Pass, "{id}: unattached");
        let (attached, h) = hosted_run(t0, SHIPPED, s);
        assert_same_run(&format!("{id}: attached"), &attached, &direct);
        assert_eq!(judge.judge(&attached), Verdict::Pass, "{id}: attached");
        let (verdict, reproduced) = judged_log(&h);
        assert!(reproduced, "{id}: the log replays to the live head");
        assert_eq!(verdict, Verdict::Pass, "{id}: NoStrandedLight");
        assert_eq!(direct.counts().stranded, 0);
        judged[usize::from(split == CorpusSplit::HeldOut)] += 1;
    }
    assert_eq!(judged, [16, 9], "every lane item of both splits");
}

#[test]
fn cold_only_fails_no_step_slept() {
    let t0 = Instant::now();
    let c = corpus();
    let judge = LaneFidelity::new();
    let cold = Sched::Plan(plan(Split::Shipped, Wake::ColdOnly));
    let (mut failed, mut passed) = (Vec::new(), Vec::new());
    for (id, _, s) in lane_items(&c) {
        let (r, h) = hosted_run(t0, cold, s);
        assert_eq!(r.fault(), None, "{id}");
        let (verdict, reproduced) = judged_log(&h);
        assert!(reproduced, "{id}");
        assert_eq!(verdict, Verdict::Pass, "{id}: ColdOnly strands nothing");
        let shipped = run(&mut MomentumGlow::default(), s, t0);
        let frames =
            |r: &LaneRun| -> Vec<(u64, u64)> { r.ticks().iter().map(|t| (t.at, t.key)).collect() };
        let v = judge.judge(&r);
        if frames(&r) == frames(&shipped) {
            assert_eq!(v, Verdict::Pass, "{id}: the shipped lane's frames");
            passed.push(id);
            continue;
        }
        assert_eq!(law(&v), Some(NO_STEP_SLEPT), "{id}: {v:?}");
        let at = cursor(&v).expect("a failure names its tick");
        assert!(
            matches!(r.ticks()[at].after, After::Wake { wake: Some(_), .. }),
            "{id}: the tick that slept asked for a wake: {:?}",
            r.ticks()[at].after
        );
        assert!(
            r.counts().ticks < shipped.counts().ticks,
            "{id}: fewer ticks"
        );
        failed.push(id);
    }
    assert_eq!(
        passed,
        ["held-105"],
        "the items where the lane never settles hot"
    );
    assert_eq!(failed.len(), 24);
    assert!(failed.contains(&"s0"));
}

#[test]
fn a_poll_past_a_frame_fails_no_step_slept() {
    let t0 = Instant::now();
    let c = corpus();
    let judge = LaneFidelity::new();
    let slow = Sched::Plan(plan(Split::Shipped, Wake::Poll { period_us: 100_000 }));
    let mut stranded_light = Vec::new();
    for (id, _, s) in lane_items(&c) {
        let (r, h) = hosted_run(t0, slow, s);
        let v = judge.judge(&r);
        assert_eq!(law(&v), Some(NO_STEP_SLEPT), "{id}: {v:?}");
        let (verdict, reproduced) = judged_log(&h);
        assert!(reproduced, "{id}");
        stranded_light.push(law(&verdict));
    }
    println!("Poll{{100000}}: NoStrandedLight per lane item {stranded_light:?}");
    let s = s0();
    let one_frame = Sched::Plan(plan(Split::Shipped, Wake::Poll { period_us: 16_667 }));
    let (r, h) = hosted_run(t0, one_frame, &s);
    assert_eq!(
        judge.judge(&r),
        Verdict::Pass,
        "a poll of one frame sleeps no step"
    );
    let shipped = run(&mut MomentumGlow::default(), &s, t0);
    assert!(r.counts().ticks > shipped.counts().ticks);
    assert_eq!(law(&judged_log(&h).0), Some(WAKE_NO_LATER_THAN_CLEAR));
}

#[test]
fn the_shipped_plan_lane_equals_shipped_on_every_item() {
    let t0 = Instant::now();
    let c = corpus();
    let judge = LaneFidelity::new();
    let (mut equal, mut moving) = (0usize, 0usize);
    for (id, _, s) in lane_items(&c) {
        let (shipped, _) = hosted_run(t0, SHIPPED, s);
        let (planned, h) = hosted_run(t0, Sched::Plan(SHIPPED_PLAN), s);
        assert_eq!(planned.fault(), None, "{id}");
        assert_eq!(judge.judge(&planned), Verdict::Pass, "{id}");
        let (verdict, reproduced) = judged_log(&h);
        assert!(reproduced, "{id}");
        assert_eq!(verdict, Verdict::Pass, "{id}: NoStrandedLight");
        assert!(planned.counts().ticks >= shipped.counts().ticks, "{id}");
        if s.presentation_moves_only_at_keys() {
            assert_same_run(&format!("{id}: the shipped plan"), &planned, &shipped);
            equal += 1;
        } else {
            moving += 1;
            println!(
                "{id}: presentation moves between keys; T shipped {} plan {}, P {} / {}",
                shipped.counts().ticks,
                planned.counts().ticks,
                shipped.counts().presented,
                planned.counts().presented
            );
        }
    }
    assert_eq!(equal + moving, 25);
    assert!(equal > 0 && moving > 0, "fixture: both kinds of item");
}

#[test]
fn one_probe_covers_each_interval() {
    const SAMPLES: u64 = 48;
    let t0 = Instant::now();
    let c = corpus();
    let delta = u64::try_from(PROBE_DELTA.as_nanos()).unwrap_or(0);
    let cold = Sched::Plan(plan(Split::Shipped, Wake::ColdOnly));
    let (mut probed, mut walked) = (0usize, 0usize);
    for (id, _, s) in lane_items(&c) {
        let f = s.frame_interval_ns();
        let shipped = run(&mut MomentumGlow::default(), s, t0);
        let (cold_run, _) = hosted_run(t0, cold, s);
        for (r, is_shipped) in [(&shipped, true), (&cold_run, false)] {
            intervals(r, |p, next, g, call, drawn| {
                let floor = p + f;
                if next.saturating_sub(delta) <= floor {
                    return;
                }
                let end = next - delta;
                let mut seen: Vec<Vec<u8>> = vec![drawn.to_vec()];
                for k in 0..=SAMPLES {
                    let at = floor + (end - floor) * k / SAMPLES;
                    let e = probe(r.origin(), g, call, at);
                    if is_shipped {
                        assert_eq!(e, drawn, "{id}: the shipped lane at {at} ns after {p}");
                    }
                    if seen.last() != Some(&e) {
                        assert!(
                            !seen.contains(&e),
                            "{id}: a frame came back at {at} ns after the tick at {p} ns"
                        );
                        seen.push(e);
                    }
                }
                probed += 1;
                walked += seen.len() - 1;
            });
        }
    }
    assert!(probed > 1_000, "fixture: many intervals ({probed})");
    assert!(
        walked > 100,
        "fixture: the cold-only lane walks through steps ({walked})"
    );
}

/// A plan's host draws on its own calls what the first-party engine the judge reruns draws and what
/// the shipped code draws, and leaves the state the shipped code leaves. The shipped leg applies the
/// code to a state ([`Sched::apply`], the step a host's delivery runs): the shipped host's boundary,
/// log and chain around that step are `shipped_passes_lane_fidelity_on_every_item`'s, on its own
/// calls, and a fault in them that shows only on a plan's calls is not seen here.
///
/// NEGATIVE CONTROL (2026-11-05): `Sched::apply` resetting the glow on `{SettleAtStep{0}, NextStep}`'s
/// query fires `"… train-01: tick 2"` (left this leg's frame of 436 quads, right the host's of 340);
/// one that raises only the unseen count on any plan's query fires `"… train-01: the state"`, the
/// frames being equal.
#[test]
fn no_plan_changes_the_glow_the_judge_reruns() {
    let t0 = Instant::now();
    let c = corpus();
    let mut plans: Vec<SchedulePlan> = grid_plans().into_iter().map(|(_, p)| p).collect();
    plans.push(DEMO_PLAN);
    plans.push(plan(Split::Shipped, Wake::Poll { period_us: 100_000 }));
    let items = lane_items(&c);
    let checked = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for &p in &plans {
            let (items, checked) = (&items, &checked);
            scope.spawn(move || {
                for &(id, _, s) in items {
                    let (r, h) = hosted_run(t0, Sched::Plan(p), s);
                    let mut shipped = MomentumState::new(MomentumGlow::default());
                    let (mut out, mut under) = (Vec::new(), Vec::new());
                    let mut fresh = MomentumGlow::default();
                    let (mut fo, mut fu) = (Vec::new(), Vec::new());
                    let mut n = 0usize;
                    for call in r.calls() {
                        let at = |t: i128| t0 + Duration::from_nanos(u64::try_from(t).unwrap_or(0));
                        out.clear();
                        under.clear();
                        let applied = SHIPPED.apply(&mut shipped, t0, call, &mut out, &mut under);
                        match *call {
                            MomentumEvent::Key {
                                at: k, tau_bits, ..
                            } => fresh.on_key(at(k), f32::from_bits(tau_bits)),
                            MomentumEvent::Reset => fresh.reset(),
                            MomentumEvent::Query { .. } => {}
                            MomentumEvent::Tick {
                                cur,
                                at: k,
                                geom,
                                cfg,
                            } => {
                                let Applied::Frame(a) = applied else {
                                    panic!("{p:?} {id}: tick {n}: {applied:?}");
                                };
                                let (geom, cfg) = (geom.geom(), cfg.config());
                                fo.clear();
                                fu.clear();
                                let b = fresh.tick(cur, at(k), geom, &cfg, &mut fo, &mut fu);
                                let t = &r.ticks()[n];
                                assert_eq!(
                                    (a, out.len(), under.len()),
                                    (t.frame, t.out, t.under),
                                    "{p:?} {id}: tick {n}"
                                );
                                assert_eq!(
                                    (b, fo.len(), fu.len()),
                                    (t.frame, t.out, t.under),
                                    "{p:?} {id}: tick {n}"
                                );
                                assert_eq!(out, fo);
                                assert_eq!(under, fu);
                                n += 1;
                            }
                        }
                    }
                    assert_eq!(n, r.ticks().len());
                    let hp = hosted(&h);
                    assert_eq!(
                        state_digest(hp.boundary.schema(), hp.state()),
                        state_digest(hp.boundary.schema(), &shipped),
                        "{p:?} {id}: the state"
                    );
                    checked.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    assert_eq!(checked.into_inner(), plans.len() * 25);
}

const F: u64 = 16_666_667;

fn key() -> Exo {
    Exo::Key {
        tau_bits: S0_TAU.to_bits(),
    }
}

fn present() -> Presentation {
    Presentation::of(Some((3, 10)), &s0_geom(), &cfg(true))
}

#[test]
fn the_judge_names_the_first_failing_law() {
    let t0 = Instant::now();
    let judge = LaneFidelity::new();

    let (demo, h) = hosted_run(t0, Sched::Plan(DEMO_PLAN), &s0());
    let v = judge.judge(&demo);
    let first_stranded = demo
        .ticks()
        .iter()
        .position(|t| matches!(t.after, After::Wake { wake: None, .. }))
        .expect("the demonstration fault strands the lane");
    assert_eq!(law(&v), Some(NO_STEP_SLEPT), "{v:?}");
    assert_eq!(cursor(&v), Some(first_stranded));
    let Verdict::Fail { detail, .. } = &v else {
        unreachable!()
    };
    assert!(detail.contains("a step was slept through"), "{detail}");
    assert!(
        !judged_log(&h).0.passed(),
        "NoStrandedLight fails the fault too"
    );

    let early = Scenario::new(F, present(), vec![(0, key())], 2 * F + F / 2).expect("admitted");
    let r = run(&mut MomentumGlow::default(), &early, t0);
    assert_eq!(r.fault(), None);
    assert_eq!(r.counts().ticks, 2);
    let v = judge.judge(&r);
    assert_eq!(
        (law(&v), cursor(&v)),
        (Some(CLEARED_BY_END), Some(1)),
        "{v:?}"
    );

    let long = Scenario::new(F, present(), vec![(0, key())], 100_000_000_000).expect("admitted");
    let mut rearm = Rigged {
        g: MomentumGlow::default(),
        always_active: false,
        wake: Some,
    };
    let r = run(&mut rearm, &long, t0);
    assert!(matches!(r.fault(), Some(LaneFault::WakeNotAfter { .. })));
    let v = judge.judge(&r);
    assert_eq!(law(&v), Some(LANE_SETTLES), "{v:?}");
    assert_eq!(
        cursor(&v),
        Some(r.ticks().len() - 1),
        "the tick that re-armed"
    );

    let mut creep = Rigged {
        g: MomentumGlow::default(),
        always_active: false,
        wake: |now| Some(now + Duration::from_nanos(1)),
    };
    let r = run(&mut creep, &long, t0);
    assert!(matches!(r.fault(), Some(LaneFault::TooManyEvents { .. })));
    let v = judge.judge(&r);
    assert_eq!(law(&v), Some(LANE_SETTLES), "{v:?}");
    assert_eq!(cursor(&v), Some(r.ticks().len() - 1));

    let mut busy = Rigged {
        g: MomentumGlow::default(),
        always_active: true,
        wake: |_| None,
    };
    let r = run(&mut busy, &long, t0);
    assert!(matches!(r.fault(), Some(LaneFault::Hang { .. })));
    let v = judge.judge(&r);
    assert_eq!(law(&v), Some(LANE_SETTLES), "{v:?}");
    assert_eq!(cursor(&v), Some(r.ticks().len() - 1));
}

#[test]
fn a_presentation_change_between_ticks_is_not_judged() {
    let t0 = Instant::now();
    let eager = Sched::Plan(plan(Split::SettleAtStep { gap_us: 0 }, Wake::NextStep));
    let keys: Vec<(u64, Exo)> = (0..8).map(|k| (k * 90_000_000, key())).collect();
    let end = 20_000_000_000;
    let base = Scenario::new(F, present(), keys.clone(), end).expect("admitted");
    let (plain, _) = hosted_run(t0, eager, &base);
    assert_eq!(law(&LaneFidelity::new().judge(&plain)), None);
    let ticks = plain.ticks();
    let (p, w) = ticks
        .windows(2)
        .filter_map(|pair| match pair {
            [a, b] if b.out + b.under > 0 => match a.after {
                After::Wake { wake: Some(w), .. } if w == b.at => Some((a.at, w)),
                _ => None,
            },
            _ => None,
        })
        .max_by_key(|&(p, w)| w - p)
        .expect("fixture: the plan settles to a lit step");
    assert!(w - p > 10 * F, "fixture: a slow-tail step ({} ns)", w - p);
    let boundary = (p / F + 1) * F;
    let c = p + (boundary - p) / 2;
    let moved = Presentation::of(Some((3, 11)), &s0_geom(), &cfg(true));
    let mut events = keys;
    events.push((c, Exo::Present(moved)));
    let changed = Scenario::new(F, present(), events, end).expect("admitted");
    let new_cell = |r: &LaneRun, at: u64| {
        let t = r.ticks().iter().find(|t| t.at == at).expect("a tick there");
        matches!(
            r.calls()[t.call],
            MomentumEvent::Tick {
                cur: Some((3, 11)),
                ..
            }
        ) && t.out + t.under > 0
            && t.presented
    };

    let (lagged, _) = hosted_run(t0, eager, &changed);
    assert_eq!(
        law(&LaneFidelity::new().judge(&lagged)),
        None,
        "NoStepSlept passes the lag"
    );
    let after_p = lagged
        .ticks()
        .iter()
        .find(|t| t.at > p)
        .expect("a tick after p");
    assert_eq!(
        after_p.at, w,
        "no tick between p and its wake: the old cell stays on glass"
    );
    assert!(new_cell(&lagged, w), "the wake draws the new cell");

    let (shipped_plain, _) = hosted_run(t0, SHIPPED, &base);
    let (shipped, _) = hosted_run(t0, SHIPPED, &changed);
    assert_eq!(law(&LaneFidelity::new().judge(&shipped)), None);
    let first = shipped
        .ticks()
        .iter()
        .find(|t| t.at > c)
        .expect("a tick after the change");
    assert!(first.at <= c + F, "the shipped code ticks within a frame");
    assert!(new_cell(&shipped, first.at), "and draws the new cell");

    let delta = |a: &LaneRun, b: &LaneRun| {
        let (a, b) = (a.counts(), b.counts());
        let d = |x: u64, y: u64| i128::from(y) - i128::from(x);
        (
            d(a.ticks, b.ticks),
            d(a.presented, b.presented),
            d(a.wasted(), b.wasted()),
        )
    };
    println!(
        "a moved cell at {c} ns, between the step tick {p} ns and its wake {w} ns \
         ({} ms on glass): plan (T, P, Z) {:?}, shipped {:?}",
        (w - c) / 1_000_000,
        delta(&plain, &lagged),
        delta(&shipped_plain, &shipped)
    );
    assert_eq!(
        delta(&plain, &lagged),
        (1, 0, 1),
        "the plan: one wasted tick"
    );
    assert_eq!(
        delta(&shipped_plain, &shipped),
        (1, 1, 0),
        "the shipped code: one presented tick"
    );
}

fn grid_plans() -> Vec<(String, SchedulePlan)> {
    let mut v = vec![("planned(SHIPPED_PLAN)".to_owned(), SHIPPED_PLAN)];
    for g in [0, 8_333, 16_667, 25_000, 33_334, 50_000, 100_000, 250_000] {
        v.push((
            format!("{{SettleAtStep{{{g}}}, NextStep}}"),
            plan(Split::SettleAtStep { gap_us: g }, Wake::NextStep),
        ));
    }
    v.push((
        "{Shipped, ColdOnly}".to_owned(),
        plan(Split::Shipped, Wake::ColdOnly),
    ));
    v.push((
        "{Shipped, Cadence}".to_owned(),
        plan(Split::Shipped, Wake::Cadence),
    ));
    v
}

/// The name of the law `v` failed, if it failed one.
fn row_law(v: &RowVerdict) -> Option<&str> {
    match v {
        RowVerdict::Fail { law, .. } => Some(law),
        RowVerdict::Pass | RowVerdict::NotRun => None,
    }
}

/// The table of design §10's early check, read off the evaluator the tests share
/// ([`evaluator`]): a row is what a host attached with the code, driven by the lane and replayed
/// from its log was judged to do, and most of these pairs are ones the grid search and the chains
/// judge anyway, which this table judged again for itself. The shipped row reads the attached host:
/// that the bare engine draws the same on every item is `shipped_passes_lane_fidelity_on_every_item`'s.
/// Off the pinned target nothing here runs, as no evaluator is built; the calibrations stay held
/// there by that test, `cold_only_fails_no_step_slept` and
/// `the_shipped_plan_lane_equals_shipped_on_every_item`, while every other row's log replaying to
/// its live head is held only on the pinned target.
///
/// The table is the old one's on every row and every total (2026-11-05, the two printed side by side
/// in one build), its shipped row renamed.
///
/// NEGATIVE CONTROL (2026-11-05): a live chain corrupted at the third link for
/// `{SettleAtStep{33334}, NextStep}` alone (`Hosted::link`) fires
/// `"{SettleAtStep{33334}, NextStep}: every log replays to its live head"` (left 0, right 25);
/// `{Shipped, ColdOnly}` waking as `NextStep` fires `"{Shipped, ColdOnly}: the calibration"` (left
/// (25, 0, 25), right (1, 24, 25)).
#[test]
fn the_early_usefulness_table() {
    if refused_off_the_pinned_target() {
        return;
    }
    let ev = evaluator();
    let c = corpus();
    let items: Vec<(usize, CorpusSplit, &str)> = c
        .items()
        .iter()
        .enumerate()
        .filter(|(_, i)| i.scenario().is_some())
        .map(|(k, i)| (k, i.split(), i.id()))
        .collect();
    let golden_at = c
        .items()
        .iter()
        .position(|i| i.scenario().is_none())
        .expect("the golden trace is an item");
    let mut rows: Vec<(String, Sched)> = vec![("SHIPPED (attached host)".to_owned(), SHIPPED)];
    rows.extend(
        grid_plans()
            .into_iter()
            .map(|(name, p)| (name, Sched::Plan(p))),
    );
    let jobs: Vec<(usize, usize)> = (0..rows.len())
        .flat_map(|r| {
            items
                .iter()
                .map(move |&(k, ..)| (r, k))
                .chain(core::iter::once((r, golden_at)))
        })
        .collect();
    let judged = crate::test_pool::par_map(&jobs, |&(r, k)| {
        ev.judge(rows[r].1, k).expect("a corpus item").0
    });
    let n = items.len();
    assert_eq!(n, 25);
    let s0_ix = items
        .iter()
        .position(|i| i.2 == "s0")
        .expect("S0 is an item");

    let mut table = String::new();
    let _ = writeln!(
        table,
        "Aos M4 phase 2 — early usefulness check (corpus {}, generator v1)",
        c.digest().hex()
    );
    let _ = writeln!(
        table,
        "T ticks, P presented (differs from the glass), Z = T - P. train = the 16 generated \
         training lane items; held = S0 + the 8 generated held-out items. fidelity / NSL: \
         lane items passing lane-fidelity / NoStrandedLight (on the attached host's replayed \
         log) of 25; first failing law: NSS NoStepSlept, CBE ClearedByEnd, LS LaneSettles. \
         golden: the golden trace open loop (T and P are the trace's, the same for every code)."
    );
    let _ = writeln!(
        table,
        "{:<30} {:>6} {:>6} {:>6} | {:>7} {:>7} {:>7} | {:>7} {:>7} {:>7} | {:>8} {:>13} | {:>3} {:>5} | golden NSL",
        "code",
        "S0 T",
        "P",
        "Z",
        "train T",
        "P",
        "Z",
        "held T",
        "P",
        "Z",
        "fidelity",
        "NSS/CBE/LS",
        "NSL",
        "repr"
    );
    let mut summary = Vec::new();
    for (r, (name, sched)) in rows.iter().enumerate() {
        let of_row = &judged[r * (n + 1)..(r + 1) * (n + 1)];
        let (row, golden) = of_row.split_at(n);
        let golden = &golden[0];
        let sum = |split: CorpusSplit| {
            row.iter()
                .zip(&items)
                .filter(|(_, i)| i.1 == split)
                .fold((0u64, 0u64), |a, (c, _)| (a.0 + c.ticks, a.1 + c.presented))
        };
        let (tt, tp) = sum(CorpusSplit::Train);
        let (ht, hp) = sum(CorpusSplit::HeldOut);
        let s = &row[s0_ix];
        let pass = row.iter().filter(|c| c.lane_fidelity.passed()).count();
        let first = |l: &str| {
            row.iter()
                .filter(|c| row_law(&c.lane_fidelity) == Some(l))
                .count()
        };
        let nsl = row.iter().filter(|c| c.stranded_light.passed()).count();
        let repro = row.iter().filter(|c| c.run.passed()).count();
        let _ = writeln!(
            table,
            "{:<30} {:>6} {:>6} {:>6} | {:>7} {:>7} {:>7} | {:>7} {:>7} {:>7} | {:>5}/25 {:>13} | {:>3} {:>5} | {} (T {} P {})",
            name,
            s.ticks,
            s.presented,
            s.wasted,
            tt,
            tp,
            tt - tp,
            ht,
            hp,
            ht - hp,
            pass,
            format!(
                "{}/{}/{}",
                first(NO_STEP_SLEPT),
                first(CLEARED_BY_END),
                first(LANE_SETTLES)
            ),
            nsl,
            repro,
            row_law(&golden.stranded_light).unwrap_or("pass"),
            golden.ticks,
            golden.presented,
        );
        summary.push((
            name.clone(),
            *sched,
            pass,
            nsl,
            repro,
            golden.stranded_light.passed(),
            first(NO_STEP_SLEPT),
        ));
    }
    print!("{table}");

    for (name, sched, pass, nsl, repro, golden_pass, nss) in summary {
        assert_eq!(repro, 25, "{name}: every log replays to its live head");
        match sched {
            SHIPPED | Sched::Plan(SHIPPED_PLAN) => {
                assert_eq!((pass, nsl), (25, 25), "{name}: the calibration");
                assert!(golden_pass, "{name}: the golden trace");
            }
            Sched::Plan(p) if p == plan(Split::Shipped, Wake::ColdOnly) => {
                assert_eq!((pass, nss, nsl), (1, 24, 25), "{name}: the calibration");
            }
            _ => {}
        }
    }
}
