// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The evaluator (design §10.5–§10.7).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use aterm_aos::Digest;
use aterm_aos::contract::Contract;
use aterm_aos::effects::NoEffects;
use aterm_aos::replay::replay;
use aterm_time::Instant;

use super::bundle::{BUNDLE_DOMAIN, CandidateBundle, Row, RowVerdict};
use super::corpus::{Corpus, CorpusSplit, FRAME_INTERVALS_NS, GOLDEN_TRACE, ItemKind};
use super::fidelity::{CLEARED_BY_END, NO_STEP_SLEPT};
use super::lane::{Exo, Scenario};
use super::m4::{
    Admitted, CandidateRefusal, Compared, Evaluator, Fault, GOLDEN_UNREADABLE, LANE_NOT_LOGGED,
    LANE_NOT_REPRODUCED, NoOutcome,
};
use super::plan::{DEMO_PLAN, PlanRefusal, SHIPPED_PLAN, SchedulePlan, Split, Wake};
use super::policy::{GRID_V1, ITEMS_V1, MAX_PROPOSAL_BYTES, MAX_ROUNDS, Totals};
use super::search::{
    GridProposer, Mode, ON_DEMAND, Proposer, ProposerId, SearchStats, TrainFeedback,
};
use super::test_fixtures::BUILD;
use super::{
    HOT_HAS_WAKE, MomentumCode, MomentumEvent, MomentumObservation, MomentumTrace, NoStrandedLight,
    PrebuiltMomentum, SETTLE_MARGIN, Sched, WAKE_NO_LATER_THAN_CLEAR,
};

/// The evaluator the tests share (its read counters are not asserted on).
pub(super) fn evaluator() -> &'static Evaluator {
    static EV: OnceLock<Evaluator> = OnceLock::new();
    EV.get_or_init(|| Evaluator::v1(BUILD, Instant::now()).expect("the pinned policy v1"))
}

/// The grid search, run once, with the test harness's wall time for it.
pub(super) struct Searched {
    /// The result.
    pub(super) admitted: Admitted,
    /// How long `search` took, as this test process's clock read it.
    pub(super) elapsed: Duration,
}

/// The grid search on the shared evaluator.
pub(super) fn searched() -> &'static Searched {
    static S: OnceLock<Searched> = OnceLock::new();
    S.get_or_init(|| {
        let ev = evaluator();
        let t = Instant::now();
        let admitted = ev
            .search(&mut GridProposer::new())
            .expect("the grid search has an outcome");
        Searched {
            admitted,
            elapsed: t.elapsed(),
        }
    })
}

pub(super) fn plan(split: Split, wake: Wake) -> SchedulePlan {
    SchedulePlan::new(split, wake).expect("fixture: an in-range plan")
}

/// `{SettleAtStep{0}, NextStep}`, the plan the grid search selects.
pub(super) fn gap0() -> SchedulePlan {
    plan(Split::SettleAtStep { gap_us: 0 }, Wake::NextStep)
}

pub(super) fn cold_only() -> SchedulePlan {
    plan(Split::Shipped, Wake::ColdOnly)
}

/// The corpus index of item `id`.
pub(super) fn index_of(id: &str) -> usize {
    ITEMS_V1
        .iter()
        .position(|p| p.id() == id)
        .expect("fixture: a policy item")
}

fn rows_of(ev: &Evaluator, sched: Sched) -> Vec<(Row, Option<CandidateRefusal>)> {
    let ks: Vec<usize> = (0..ITEMS_V1.len()).collect();
    crate::test_pool::par_map(&ks, |&k| ev.judge(sched, k).expect("an item"))
}

const fn tpz(t: u64, p: u64, z: u64) -> Totals {
    Totals {
        ticks: t,
        presented: p,
        wasted: z,
    }
}

fn golden_replay(
    t0: Instant,
    plan: SchedulePlan,
) -> (
    aterm_aos::replay::Run<super::MomentumSchema>,
    Vec<MomentumEvent>,
) {
    let trace = MomentumTrace::decode(GOLDEN_TRACE).expect("the golden trace decodes");
    let read = trace.read(t0).expect("the golden trace reads");
    let start = read
        .checkpoint
        .restore(&read.schema, &trace.identity)
        .expect("its checkpoint restores");
    let code = PrebuiltMomentum::planned(plan, BUILD);
    let r = replay(
        &read.schema,
        &code,
        start,
        read.log.inputs(),
        read.log.header().first_cursor,
        &mut NoEffects,
    );
    let v = NoStrandedLight::new().judge(&read.schema, read.log.inputs(), &r);
    assert!(!v.passed(), "fixture: a refused plan");
    (r, read.log.inputs().to_vec())
}

fn answer_at(
    r: &aterm_aos::replay::Run<super::MomentumSchema>,
    inputs: &[MomentumEvent],
    first: u64,
    cursor: u64,
) -> (i128, Duration, Option<i128>, Option<i128>) {
    let i = usize::try_from(cursor - first).expect("a cursor in the log");
    match (&inputs[i], &r.observations[i].1) {
        (
            MomentumEvent::Query { at, frame_interval },
            MomentumObservation::Schedule {
                cold_at, deadline, ..
            },
        ) => (*at, *frame_interval, *cold_at, *deadline),
        other => panic!("cursor {cursor} is not a schedule query: {other:?}"),
    }
}

#[test]
fn the_evaluator_measures_as_the_baseline_was_measured() {
    let ev = evaluator();
    let shipped = rows_of(ev, Sched::Prebuilt(MomentumCode::Shipped));
    let planned = rows_of(ev, Sched::Plan(SHIPPED_PLAN));
    let mut differs = Vec::new();
    for (k, pin) in ITEMS_V1.iter().enumerate() {
        let b = pin.baseline();
        for (what, (row, refusal)) in [("shipped", &shipped[k]), ("planned", &planned[k])] {
            assert_eq!(refusal, &None, "{what} {}: admitted", pin.id());
            assert_eq!(
                (row.item.as_str(), row.split, row.kind),
                (pin.id(), pin.split(), pin.kind()),
                "{what}: policy order"
            );
            assert!(
                row.run.passed() && row.stranded_light.passed(),
                "{what} {row:?}"
            );
            let golden = pin.kind() == ItemKind::GoldenTrace;
            assert_eq!(
                row.lane_fidelity,
                if golden {
                    RowVerdict::NotRun
                } else {
                    RowVerdict::Pass
                },
                "{what} {}",
                pin.id()
            );
            assert_eq!(row.firings.is_none(), golden, "{what} {}", pin.id());
        }
        let (row, _) = &shipped[k];
        assert_eq!(
            (row.ticks, row.presented, row.wasted, row.lane, row.chain),
            (b.ticks, b.presented, b.wasted, b.lane, b.chain),
            "the shipped code on {} is its pin",
            pin.id()
        );
        assert_eq!(
            row.firings.unwrap_or(0),
            0,
            "prebuilt codes never fire the guard"
        );
        let (p, _) = &planned[k];
        if (p.ticks, p.presented, p.wasted) != (b.ticks, b.presented, b.wasted) {
            differs.push((
                pin.id(),
                i128::from(p.ticks) - i128::from(b.ticks),
                p.firings,
            ));
        }
    }
    println!("the shipped plan against the pinned baseline: {differs:?}");
    assert_eq!(differs.len(), 1, "{differs:?}");
    assert_eq!((differs[0].0, differs[0].1), ("held-106", 7));
    assert!(differs[0].2.is_some_and(|f| f > 0), "the guard fired there");
}

#[test]
fn the_grid_search_selects_a_plan_useful_on_held_out() {
    let s = searched();
    let a = &s.admitted;
    let parts = a.bundle().parts();
    let sel = &parts.selection;
    println!(
        "grid search: {:?} (test harness, this profile); winner {:?}",
        s.elapsed,
        a.plan()
    );
    println!(
        "train T {} P {} Z {} vs baseline T {} P {} Z {}; held-out T {} P {} Z {} vs T {} P {} Z {}; \
         gain {} ppm; S0 T {} P {} Z {} vs T {} P {} Z {}, gain {} ppm; {:?}",
        sel.train.candidate.ticks,
        sel.train.candidate.presented,
        sel.train.candidate.wasted,
        sel.train.baseline.ticks,
        sel.train.baseline.presented,
        sel.train.baseline.wasted,
        sel.held_out.candidate.ticks,
        sel.held_out.candidate.presented,
        sel.held_out.candidate.wasted,
        sel.held_out.baseline.ticks,
        sel.held_out.baseline.presented,
        sel.held_out.baseline.wasted,
        sel.gain_ppm,
        sel.s0.candidate.ticks,
        sel.s0.candidate.presented,
        sel.s0.candidate.wasted,
        sel.s0.baseline.ticks,
        sel.s0.baseline.presented,
        sel.s0.baseline.wasted,
        sel.s0_gain_ppm,
        sel.stats
    );
    assert_eq!(a.plan(), gap0());
    assert_eq!(a.policy(), evaluator().policy().id());
    assert!(a.useful());
    assert_eq!(
        sel.stats,
        SearchStats {
            rounds: MAX_ROUNDS,
            proposed: 54,
            oversized: 0,
            duplicates: 0,
            over_budget: 0,
            distinct: 54,
            admitted: 18,
        },
        "every NextStep and Cadence plan is admitted on training; every other is refused"
    );
    assert_eq!(sel.mode, Mode::Search);
    assert_eq!(sel.held_out_looks, 1);
    assert_eq!(
        sel.train,
        Compared {
            candidate: tpz(6_394, 6_300, 94),
            baseline: tpz(8_441, 6_266, 2_175),
        }
    );
    assert!(sel.train.beats());
    assert_eq!(
        sel.held_out,
        Compared {
            candidate: tpz(5_119, 4_958, 161),
            baseline: tpz(6_685, 4_935, 1_750),
        }
    );
    assert!(sel.held_out.beats());
    assert_eq!(
        sel.s0,
        Compared {
            candidate: tpz(2_500, 2_490, 10),
            baseline: tpz(2_776, 2_480, 296),
        }
    );
    assert_eq!((sel.gain_ppm, sel.s0_gain_ppm), (234_255, 99_423));
    assert_eq!(a.gain().ppm(), sel.gain_ppm);
    assert_eq!(a.gain().s0_ppm(), sel.s0_gain_ppm);
    assert_eq!((a.gain().held_out, a.gain().s0), (sel.held_out, sel.s0));
    assert_eq!(parts.proposer, GridProposer::new().id());
    let grid: Vec<Vec<u8>> = GridProposer::plans();
    let at = grid
        .iter()
        .position(|p| p.as_slice() == gap0().encode())
        .expect("the winner is a grid plan");
    assert_eq!(
        (parts.round, parts.index),
        (0, u64::try_from(at).expect("small"))
    );
    assert!(parts.rows.iter().all(|r| r.run.passed()));
}

#[test]
fn same_policy_same_winner_same_bundle_digest() {
    let first = &searched().admitted;
    let t0 = Instant::now()
        .checked_add(Duration::from_secs(17))
        .expect("an instant 17 s out");
    let ev = Evaluator::v1(BUILD, t0).expect("the pinned policy v1");
    let again = ev
        .search(&mut GridProposer::new())
        .expect("the grid search has an outcome");
    assert_eq!(again.plan(), first.plan());
    assert_eq!(again.useful(), first.useful());
    assert_eq!(again.gain(), first.gain());
    let (a, b) = (first.bundle().encode(), again.bundle().encode());
    assert_eq!(
        Digest::of(BUNDLE_DOMAIN, &[&a]),
        Digest::of(BUNDLE_DOMAIN, &[&b])
    );
    assert_eq!(a, b, "the same bytes");
}

#[test]
fn cold_only_passes_no_stranded_light_and_fails_no_step_slept() {
    let ev = evaluator();
    let p = cold_only();
    let refusal = ev.admit_one(&p.encode()).expect_err("refused");
    match &refusal {
        CandidateRefusal::LaneFidelity { item, law, .. } => {
            assert_eq!((item.as_str(), *law), ("train-01", NO_STEP_SLEPT));
        }
        other => panic!("{other:?}"),
    }
    let rows = rows_of(ev, Sched::Plan(p));
    assert!(
        rows.iter()
            .all(|(r, _)| r.run.passed() && r.stranded_light.passed()),
        "NoStrandedLight passes everywhere"
    );
    let failing: Vec<&str> = rows
        .iter()
        .filter(|(r, _)| matches!(&r.lane_fidelity, RowVerdict::Fail { law, .. } if law == NO_STEP_SLEPT))
        .map(|(r, _)| r.item.as_str())
        .collect();
    let passing: Vec<&str> = rows
        .iter()
        .filter(|(r, _)| r.lane_fidelity.passed())
        .map(|(r, _)| r.item.as_str())
        .collect();
    assert_eq!(failing.len(), 24, "{failing:?}");
    assert_eq!(passing, ["held-105"]);
}

#[test]
fn a_poll_past_a_frame_is_refused() {
    let ev = evaluator();
    let mut plans = Vec::new();
    for f in FRAME_INTERVALS_NS {
        let f_us = u32::try_from(f / 1_000).expect("a frame is under a second");
        for q in [f_us + 1, f_us + 2_000] {
            for split in [Split::Shipped, Split::SettleAtStep { gap_us: 0 }] {
                plans.push((Some(f), plan(split, Wake::Poll { period_us: q })));
            }
        }
    }
    for &(split, wake) in &GRID_V1 {
        if let Wake::Poll { .. } = wake {
            plans.push((None, plan(split, wake)));
        }
    }
    assert_eq!(plans.len(), 12 + 18);
    let refusals = crate::test_pool::par_map(&plans, |(_, p)| ev.admit_one(&p.encode()).err());
    for ((_, p), r) in plans.iter().zip(&refusals) {
        let r = r.as_ref().unwrap_or_else(|| panic!("{p:?} is refused"));
        assert_eq!(r.law(), WAKE_NO_LATER_THAN_CLEAR, "{p:?}: {r}");
        assert!(
            matches!(
                r,
                CandidateRefusal::OpenLoop { .. } | CandidateRefusal::StrandedLight { .. }
            ),
            "{p:?}: {r}"
        );
    }
    let corpus = Corpus::generate().expect("generator v1");
    let lanes: Vec<(usize, u64)> = corpus
        .items()
        .iter()
        .enumerate()
        .filter(|(_, i)| i.split() == CorpusSplit::Train)
        .filter_map(|(k, i)| i.scenario().map(|s| (k, s.frame_interval_ns())))
        .collect();
    let jobs: Vec<(SchedulePlan, usize)> = plans
        .iter()
        .filter_map(|&(f, p)| f.map(|f| (f, p)))
        .flat_map(|(f, p)| {
            lanes
                .iter()
                .filter(move |&&(_, at)| at == f)
                .map(move |&(k, _)| (p, k))
        })
        .collect();
    let rows = crate::test_pool::par_map(&jobs, |&(p, k)| {
        (p, ev.judge(Sched::Plan(p), k).expect("an item").0)
    });
    for (f, p) in plans.iter().filter_map(|&(f, p)| f.map(|f| (f, p))) {
        let of: Vec<&Row> = rows
            .iter()
            .filter(|(q, _)| *q == p)
            .map(|(_, r)| r)
            .collect();
        assert!(!of.is_empty(), "{f} ns: training items at the interval");
        assert!(
            of.iter().all(|r| r.lane_fidelity.passed()),
            "{p:?}: the slack is unjudged by lane fidelity"
        );
        assert!(
            of.iter().any(|r| matches!(&r.stranded_light, RowVerdict::Fail { law, .. } if law == WAKE_NO_LATER_THAN_CLEAR)),
            "{p:?}: refused at {f} ns"
        );
    }
}

#[test]
fn a_poll_beyond_the_clear_fails_wake_no_later_than_clear() {
    let ev = evaluator();
    let t0 = Instant::now();
    let margin = i128::try_from(SETTLE_MARGIN.as_nanos()).expect("small");
    for q in [250_000, 1_000_000] {
        for split in [Split::Shipped, Split::SettleAtStep { gap_us: 0 }] {
            let p = plan(split, Wake::Poll { period_us: q });
            let r = ev.admit_one(&p.encode()).expect_err("refused");
            let CandidateRefusal::OpenLoop {
                item, law, cursor, ..
            } = &r
            else {
                panic!("{p:?}: {r}");
            };
            assert_eq!((item.as_str(), *law), ("golden", WAKE_NO_LATER_THAN_CLEAR));
            let (replayed, inputs) = golden_replay(t0, p);
            let first = replayed.observations[0].0;
            let (at, f, cold_at, deadline) = answer_at(&replayed, &inputs, first, *cursor);
            let d = deadline.expect("a poll names a wake");
            let cold = cold_at.expect("a settling glow has a cold forecast");
            let floor = at + i128::try_from(f.as_nanos()).expect("small");
            assert!(
                d > cold + margin && d > floor,
                "{p:?}: the wake {d} is past the clear {cold} + margin and the floor {floor}"
            );
        }
    }
}

#[test]
fn the_demo_plan_is_refused_hot_has_wake() {
    let ev = evaluator();
    let mut plans = vec![DEMO_PLAN];
    plans.extend(
        GRID_V1
            .iter()
            .filter(|(_, w)| *w == Wake::Never)
            .map(|&(s, w)| plan(s, w)),
    );
    assert_eq!(plans.len(), 10);
    for p in plans {
        match ev.admit_one(&p.encode()) {
            Err(CandidateRefusal::OpenLoop { item, law, .. }) => {
                assert_eq!((item.as_str(), law), ("golden", HOT_HAS_WAKE), "{p:?}");
            }
            other => panic!("{p:?}: {other:?}"),
        }
    }
}

#[test]
fn cadence_is_admitted_and_not_useful() {
    let ev = evaluator();
    let p = plan(Split::Shipped, Wake::Cadence);
    let a = ev.admit_one(&p.encode()).expect("admitted");
    let sel = &a.bundle().parts().selection;
    println!(
        "cadence: train {:?}, held-out {:?}, gain {} ppm",
        sel.train,
        sel.held_out,
        a.gain().ppm()
    );
    assert!(!a.useful());
    assert!(!sel.useful);
    assert!(!sel.held_out.beats() && !sel.train.beats());
    assert!(sel.held_out.candidate.ticks > sel.held_out.baseline.ticks);
    assert!(sel.held_out.candidate.wasted > sel.held_out.baseline.wasted);
    assert!(a.gain().ppm() < 0);
    assert_eq!(sel.mode, Mode::OnDemand);
    assert_ne!(searched().admitted.plan(), p);
}

#[test]
fn size_and_decode_are_refused_first() {
    let fresh = Evaluator::v1(BUILD, Instant::now()).expect("the pinned policy v1");
    let ev = &fresh;
    let long = vec![1u8; MAX_PROPOSAL_BYTES + 1];
    assert_eq!(
        ev.admit_one(&long).err(),
        Some(CandidateRefusal::Budget {
            len: MAX_PROPOSAL_BYTES + 1,
            limit: MAX_PROPOSAL_BYTES,
        })
    );
    let mut bad = SHIPPED_PLAN.encode();
    bad[0] = 2;
    let r = ev.admit_one(&bad).expect_err("refused");
    assert_eq!(
        r,
        CandidateRefusal::Decode(PlanRefusal::Format { found: 2 })
    );
    assert_eq!(r.law(), "PlanRefusal");
    assert_eq!(r.item(), None);
    let short = ev.admit_one(&[1, 0, 0]).expect_err("refused");
    assert!(matches!(
        short,
        CandidateRefusal::Decode(PlanRefusal::Length { found: 3, .. })
    ));
    let reads = ev.reads();
    let n = |r: &AtomicU64| r.load(Ordering::Relaxed);
    assert_eq!(
        (n(&reads.train), n(&reads.held_out), n(&reads.looks)),
        (0, 0, 0),
        "no item ran"
    );
    assert_eq!(ev.stepped(), 0, "no input was stepped");
}

#[test]
fn an_unreadable_trace_an_unlogged_lane_and_an_unreproduced_log_are_refused() {
    let golden = index_of("golden");
    let t1 = index_of("train-01");
    let mut damaged = GOLDEN_TRACE.to_vec();
    let mid = damaged.len() / 2;
    damaged[mid] ^= 0x01;
    let fresh = || Evaluator::v1(BUILD, Instant::now()).expect("the pinned policy v1");
    let p = gap0();

    let ev = fresh().faulting("golden", Fault::GoldenBytes(damaged));
    let r = ev.admit_one(&p.encode()).expect_err("refused");
    match &r {
        CandidateRefusal::Entry { item, law, .. } => {
            assert_eq!((item.as_str(), *law), ("golden", GOLDEN_UNREADABLE));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!((r.law(), r.item()), (GOLDEN_UNREADABLE, Some("golden")));
    let (row, refusal) = ev.judge(Sched::Plan(p), golden).expect("an item");
    assert_eq!(refusal.as_ref(), Some(&r));
    assert_eq!(row.run, RowVerdict::fail(GOLDEN_UNREADABLE, 0));
    assert_eq!(
        row.stranded_light,
        RowVerdict::NotRun,
        "nothing was replayed"
    );
    assert_eq!(ev.stepped(), 0, "the unreadable trace stepped nothing");

    let ev = fresh().faulting("train-01", Fault::Unlogged);
    let r = ev.admit_one(&p.encode()).expect_err("refused");
    assert_eq!(
        r,
        CandidateRefusal::LaneNotLogged {
            item: "train-01".to_owned()
        }
    );
    assert_eq!((r.law(), r.item()), (LANE_NOT_LOGGED, Some("train-01")));
    let (row, _) = ev.judge(Sched::Plan(p), t1).expect("an item");
    assert_eq!(row.run, RowVerdict::fail(LANE_NOT_LOGGED, 0));
    assert_eq!(
        row.stranded_light,
        RowVerdict::NotRun,
        "nothing was replayed"
    );

    let ev = fresh().faulting("train-01", Fault::ReplayShort);
    let r = ev.admit_one(&p.encode()).expect_err("refused");
    assert_eq!(
        r,
        CandidateRefusal::LaneNotReproduced {
            item: "train-01".to_owned()
        }
    );
    assert_eq!((r.law(), r.item()), (LANE_NOT_REPRODUCED, Some("train-01")));
    let (row, _) = ev.judge(Sched::Plan(p), t1).expect("an item");
    assert!(
        matches!(&row.run, RowVerdict::Fail { law, .. } if law == LANE_NOT_REPRODUCED),
        "{:?}",
        row.run
    );

    let (row, refusal) = ev.judge(Sched::Plan(p), golden).expect("an item");
    assert!(refusal.is_none() && row.run.passed());
}

#[test]
fn a_reset_at_rest_is_judged_as_the_host_logged_it() {
    let id = "held-101";
    let k = index_of(id);
    let corpus = Corpus::generate().expect("generator v1");
    let s = corpus
        .items()
        .iter()
        .find(|i| i.id() == id)
        .and_then(|i| i.scenario())
        .expect("a held-out lane item");
    let first = s.events().first().map(|&(t, _)| t).expect("it has inputs");
    let mut events = vec![(first, Exo::Reset), (first, Exo::Reset)];
    events.extend_from_slice(s.events());
    let resting = Scenario::new(s.frame_interval_ns(), *s.start(), events, s.end_ns())
        .expect("an admitted scenario");
    let shipped = Sched::Prebuilt(MomentumCode::Shipped);
    let (plain, refusal) = evaluator().judge(shipped, k).expect("an item");
    assert!(refusal.is_none(), "{refusal:?}");
    let ev = Evaluator::v1(BUILD, Instant::now())
        .expect("the pinned policy v1")
        .replacing(id, resting);
    let (row, refusal) = ev.judge(shipped, k).expect("an item");
    assert_eq!(refusal, None, "the shipped code is admitted there");
    assert_eq!(
        row, plain,
        "the resets at rest change nothing the row records"
    );
}

struct Spy {
    plans: Vec<SchedulePlan>,
    asked: Vec<u8>,
    shown: Vec<Vec<TrainFeedback>>,
}

impl Proposer for Spy {
    fn id(&self) -> ProposerId {
        ProposerId::new("spy", 1, &[])
    }

    fn propose(&mut self, round: u8, feedback: &[TrainFeedback]) -> Vec<Vec<u8>> {
        self.asked.push(round);
        self.shown.push(feedback.to_vec());
        if round == 0 {
            self.plans.iter().map(|p| p.encode().to_vec()).collect()
        } else {
            Vec::new()
        }
    }
}

#[test]
fn held_out_never_reaches_the_proposer_and_is_read_once() {
    let ev = Evaluator::v1(BUILD, Instant::now()).expect("the pinned policy v1");
    let cadence = plan(Split::Shipped, Wake::Cadence);
    let mut spy = Spy {
        plans: vec![SHIPPED_PLAN, gap0(), cadence, DEMO_PLAN],
        asked: Vec::new(),
        shown: Vec::new(),
    };
    let selection = ev.select(&mut spy).expect("a training winner");
    let reads = ev.reads();
    let held = |r: &AtomicU64| r.load(Ordering::Relaxed);
    assert_eq!(held(&reads.held_out), 0, "selection reads no held-out item");
    assert_eq!(held(&reads.looks), 0);
    assert_eq!(
        held(&reads.train),
        3 * 17 + 1,
        "three plans admitted on all 17 training items; the demo plan refused at the first"
    );
    assert_eq!(spy.asked, (0..MAX_ROUNDS).collect::<Vec<_>>());
    assert!(spy.shown[0].is_empty());
    let last = spy.shown.last().expect("four rounds");
    assert_eq!(last.len(), 4);
    let train_t: Vec<Option<u64>> = last.iter().map(TrainFeedback::ticks).collect();
    assert_eq!(
        train_t,
        [
            Some(ev.policy().totals(CorpusSplit::Train).ticks),
            Some(selection.train().ticks),
            Some(12_579),
            None,
        ],
        "training sums only: the shipped plan is the baseline on training"
    );
    assert_eq!(last[3].law(), Some(HOT_HAS_WAKE));
    assert_eq!(selection.plan(), gap0());
    let a = ev.held_out(selection).expect("useful on held-out");
    assert_eq!(held(&reads.looks), 1);
    assert_eq!(
        held(&reads.held_out),
        9,
        "the held-out split, once, for the winner"
    );
    assert_eq!(a.plan(), gap0());
    assert_eq!(a.bundle().parts().selection.held_out_looks, 1);
    let rows = &a.bundle().parts().rows;
    let train: u64 = rows
        .iter()
        .filter(|r| r.split == CorpusSplit::Train)
        .map(|r| r.ticks)
        .sum();
    assert_eq!(last[1].ticks(), Some(train));
}

#[test]
fn a_failure_on_one_held_out_item_leaves_no_outcome() {
    let corpus = Corpus::generate().expect("generator v1");
    let s = corpus
        .items()
        .iter()
        .find(|i| i.id() == "held-104")
        .and_then(|i| i.scenario())
        .expect("held-104 is a lane item");
    let last = s.events().last().map_or(0, |&(at, _)| at);
    assert!(
        s.events().iter().any(|(_, e)| matches!(e, Exo::Key { .. })),
        "fixture: it types"
    );
    let lit = Scenario::new(
        s.frame_interval_ns(),
        *s.start(),
        s.events().to_vec(),
        last + 50_000_000,
    )
    .expect("an admitted scenario");
    let ev = Evaluator::v1(BUILD, Instant::now())
        .expect("the pinned policy v1")
        .replacing("held-104", lit);
    let runner_up = plan(Split::SettleAtStep { gap_us: 8_333 }, Wake::NextStep);
    let mut spy = Spy {
        plans: vec![runner_up, gap0()],
        asked: Vec::new(),
        shown: Vec::new(),
    };
    let selection = ev.select(&mut spy).expect("a training winner");
    assert_eq!(selection.plan(), gap0());
    let r = ev.held_out(selection).expect_err("no outcome");
    match &r {
        NoOutcome::HeldOut {
            plan,
            refusal: CandidateRefusal::LaneFidelity { item, law, .. },
        } => {
            assert_eq!(*plan, gap0());
            assert_eq!((item.as_str(), *law), ("held-104", CLEARED_BY_END));
        }
        other => panic!("{other:?}"),
    }
    let reads = ev.reads();
    let n = |r: &AtomicU64| r.load(Ordering::Relaxed);
    assert_eq!(n(&reads.looks), 1);
    assert_eq!(
        n(&reads.held_out),
        5,
        "s0, held-101..103, then held-104: no further item, and no runner-up"
    );
}

#[test]
fn a_selection_is_held_out_only_in_its_build() {
    let ev = evaluator();
    let other = Evaluator::v1("another-build", Instant::now()).expect("the pinned policy v1");
    let mut spy = Spy {
        plans: vec![gap0()],
        asked: Vec::new(),
        shown: Vec::new(),
    };
    let selection = ev.select(&mut spy).expect("a training winner");
    assert_eq!(
        other.held_out(selection).err(),
        Some(NoOutcome::Foreign {
            build: BUILD.to_owned()
        })
    );
}

#[test]
fn a_winner_without_a_training_gain_is_no_outcome() {
    let ev = Evaluator::v1(BUILD, Instant::now()).expect("the pinned policy v1");
    let mut spy = Spy {
        plans: vec![plan(Split::Shipped, Wake::Cadence), SHIPPED_PLAN],
        asked: Vec::new(),
        shown: Vec::new(),
    };
    match ev.select(&mut spy) {
        Err(NoOutcome::NoGain { plan, train, stats }) => {
            assert_eq!(plan, SHIPPED_PLAN);
            assert_eq!(train.candidate, train.baseline);
            assert_eq!((stats.distinct, stats.admitted), (2, 2));
        }
        other => panic!("{other:?}"),
    }
    let n = ev.reads().held_out.load(Ordering::Relaxed);
    assert_eq!(n, 0);
}

#[test]
fn admit_one_is_an_on_demand_re_evaluation() {
    let ev = evaluator();
    let mut a = ev.admit_one(&gap0().encode()).expect("admitted");
    let s = &searched().admitted;
    let (p, q) = (a.bundle().parts(), s.bundle().parts());
    assert_eq!(p.rows, q.rows);
    assert_eq!(p.selection.train, q.selection.train);
    assert_eq!(p.selection.held_out, q.selection.held_out);
    assert_eq!((a.useful(), a.gain()), (s.useful(), s.gain()));
    assert_eq!(p.selection.mode, Mode::OnDemand);
    assert_eq!(p.selection.stats, SearchStats::ONE);
    assert_eq!(p.proposer.name, ON_DEMAND);
    assert_eq!((p.round, p.index), (0, 0));
    a.declare_toolchain("targo --unverified test (declared by the test)");
    assert!(a.record_timing("admit_one (test harness)", Duration::from_millis(1_234)));
    assert!(!a.record_timing("", Duration::ZERO));
    for i in 1..16 {
        assert!(a.record_timing(&format!("t{i}"), Duration::from_nanos(i)));
    }
    assert!(!a.record_timing("one too many", Duration::ZERO));
    let bundle = a.into_bundle();
    assert_eq!(bundle.parts().timings.len(), 16);
    assert_eq!(
        bundle.parts().toolchain,
        "targo --unverified test (declared by the test)"
    );
    let verified = CandidateBundle::verify(&bundle.encode(), ev).expect("it verifies");
    assert_eq!(verified, bundle);
}

#[test]
fn a_selection_states_training_facts() {
    let ev = evaluator();
    let mut spy = Spy {
        plans: vec![gap0()],
        asked: Vec::new(),
        shown: Vec::new(),
    };
    let s = ev.select(&mut spy).expect("a training winner");
    assert_eq!(s.plan(), gap0());
    assert_eq!(s.train(), tpz(6_394, 6_300, 94));
    assert_eq!(s.proposer(), &ProposerId::new("spy", 1, &[]));
    assert_eq!((s.round(), s.index()), (0, 0));
    assert_eq!(s.stats().admitted, 1);
}
