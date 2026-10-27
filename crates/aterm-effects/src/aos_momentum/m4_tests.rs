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
use super::corpus::SplitMix;
use super::corpus::{Corpus, CorpusSplit, FRAME_INTERVALS_NS, GOLDEN_TRACE, ItemKind};
use super::fidelity::{CLEARED_BY_END, NO_STEP_SLEPT};
use super::lane::{Exo, Scenario};
use super::m4::{
    Admitted, CandidateRefusal, Compared, Evaluator, Fault, GOLDEN_UNREADABLE, LANE_NOT_LOGGED,
    LANE_NOT_REPRODUCED, LabelRefusal, LabelScope, LookToken, MintRefusal, NoOutcome, PracticeSet,
    ReadToken, ReplayToken, Serial, Spread, TrainLabel, list_digest,
};
use super::m6_policy::{
    JUDGED, LOOK_LIST_CAP, POOL_PLANS, READ_LIST_CAP, SetId, SetTag, Side, draw, set_pools, side,
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

/// The evaluator the tests share, judging each item under each schedule once
/// ([`Evaluator::memoizing`]; its read counters are not asserted on). A test that counts reads,
/// or compares judgments made at two origins, builds its own.
pub(super) fn evaluator() -> &'static Evaluator {
    static EV: OnceLock<Evaluator> = OnceLock::new();
    EV.get_or_init(|| {
        Evaluator::v1(BUILD, Instant::now())
            .expect("the pinned policy v1")
            .memoizing()
    })
}

/// The grid search, run once, with the test harness's wall time for it.
pub(super) struct Searched {
    /// The result.
    pub(super) admitted: Admitted,
    /// How long `search` took, as this test process's clock read it.
    pub(super) elapsed: Duration,
}

/// The grid search on the shared evaluator, its candidates trained on threads.
pub(super) fn searched() -> &'static Searched {
    static S: OnceLock<Searched> = OnceLock::new();
    S.get_or_init(|| {
        let t = Instant::now();
        let admitted = grid_search(evaluator());
        Searched {
            admitted,
            elapsed: t.elapsed(),
        }
    })
}

/// [`Evaluator::search`] of the grid on `ev`, its candidates trained on threads: `search` is
/// `held_out` of `select`, and that `select_spread` selects as `select` does is
/// `select_spread_equals_select`'s.
fn grid_search(ev: &Evaluator) -> Admitted {
    let selection = ev
        .select_spread(&mut GridProposer::new(), &Threads)
        .expect("the grid has a training winner");
    ev.held_out(selection)
        .expect("the grid search has an outcome")
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
    let again = grid_search(&ev);
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

/// The shared evaluator's memo ([`Evaluator::memoizing`]) answers each item under each schedule
/// as a fresh evaluator judges it, judges each pair once however often and on however many
/// threads it is asked, and forgets what it kept when an item's judgment changes: a replaced
/// scenario and a faked fault.
#[test]
fn a_memoizing_evaluator_judges_each_pair_once_as_a_fresh_one_does() {
    let ev = evaluator().anew().memoizing();
    let fresh = evaluator().anew();
    let reads = |e: &Evaluator| {
        let n = |r: &AtomicU64| r.load(Ordering::Relaxed);
        (n(&e.reads().train), n(&e.reads().held_out))
    };
    let (golden, t1, h1) = (
        index_of("golden"),
        index_of("train-01"),
        index_of("held-101"),
    );
    let shipped = Sched::Prebuilt(MomentumCode::Shipped);
    let pairs = [
        (Sched::Plan(gap0()), t1),
        (Sched::Plan(cold_only()), t1),
        (Sched::Plan(gap0()), h1),
        (shipped, golden),
    ];
    let first: Vec<_> = pairs.iter().map(|&(s, k)| ev.judge(s, k)).collect();
    for (&(s, k), got) in pairs.iter().zip(&first) {
        assert_eq!(got, &fresh.judge(s, k), "{s:?} on item {k}");
    }
    assert!(
        matches!(
            &first[1],
            Some((_, Some(CandidateRefusal::LaneFidelity { .. })))
        ),
        "fixture: a refusal is kept with its row"
    );
    assert_eq!(reads(&ev), (3, 1));
    let asks: Vec<usize> = (0..4 * pairs.len()).map(|i| i % pairs.len()).collect();
    let again = crate::test_pool::par_map(&asks, |&i| ev.judge(pairs[i].0, pairs[i].1));
    for (&i, got) in asks.iter().zip(&again) {
        assert_eq!(got, &first[i], "{:?} asked again", pairs[i]);
    }
    let new = [(Sched::Plan(cold_only()), h1); 8];
    let at_once = crate::test_pool::par_map(&new, |&(s, k)| ev.judge(s, k));
    assert!(at_once.iter().all(|got| *got == at_once[0]));
    assert_eq!(
        at_once[0],
        fresh.judge(new[0].0, new[0].1),
        "the judgment kept from a thread is the one a fresh evaluator makes serially"
    );
    assert_eq!(
        reads(&ev),
        (3, 2),
        "asked again, and a new pair on eight threads at once: each pair judged once"
    );
    let corpus = Corpus::generate().expect("generator v1");
    let s = corpus
        .items()
        .iter()
        .find(|i| i.id() == "held-101")
        .and_then(|i| i.scenario())
        .expect("a held-out lane item");
    let cut = Scenario::new(
        s.frame_interval_ns(),
        *s.start(),
        s.events()[..1].to_vec(),
        s.end_ns(),
    )
    .expect("an admitted scenario");
    let afresh = evaluator()
        .anew()
        .replacing("held-101", cut.clone())
        .judge(Sched::Plan(gap0()), h1);
    assert_ne!(
        afresh, first[2],
        "fixture: the cut scenario changes the row"
    );
    let replaced = ev.replacing("held-101", cut);
    assert_eq!(
        replaced.judge(Sched::Plan(gap0()), h1),
        afresh,
        "a replaced scenario is judged afresh"
    );
    assert_eq!(replaced.judge(shipped, golden), first[3], "kept again");
    let faulted = replaced.faulting("golden", Fault::GoldenBytes(vec![0; 8]));
    let (row, refusal) = faulted.judge(shipped, golden).expect("an item");
    assert_eq!(row.run, RowVerdict::fail(GOLDEN_UNREADABLE, 0));
    assert_eq!(refusal.map(|r| r.law()), Some(GOLDEN_UNREADABLE));
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
    let fresh = evaluator().anew();
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
    let fresh = || evaluator().anew();
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
    let ev = evaluator().anew().replacing(id, resting);
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
    let ev = evaluator().anew();
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
    let ev = evaluator().anew().replacing("held-104", lit);
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
    let ev = evaluator().anew();
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

// ---- M6 labels and outcomes (design §13.3, stage B) ----

/// Every job on a scoped thread of its own.
struct Threads;

impl Spread for Threads {
    fn run(&self, n: usize, job: &(dyn Fn(usize) + Sync)) {
        std::thread::scope(|s| {
            for i in 0..n {
                s.spawn(move || job(i));
            }
        });
    }
}

/// A token scope over a list.
type ScopeOf<'a> = dyn Fn(&[[u8; 12]]) -> LabelScope + 'a;

/// The plans of `set`'s pool `p`, as bytes.
fn pool_bytes(set: SetId, p: usize) -> Vec<[u8; 12]> {
    set_pools(set).expect("the set fills")[p]
        .iter()
        .map(SchedulePlan::encode)
        .collect()
}

/// `T_0`'s first pool and its labels, judged once on the shared evaluator.
fn train_pool() -> &'static (Vec<SchedulePlan>, Vec<TrainLabel>) {
    static POOL: OnceLock<(Vec<SchedulePlan>, Vec<TrainLabel>)> = OnceLock::new();
    POOL.get_or_init(|| {
        let set = SetId {
            tag: SetTag::Train,
            index: 0,
        };
        let pool = set_pools(set).expect("the train set fills").swap_remove(0);
        let bytes: Vec<[u8; 12]> = pool.iter().map(SchedulePlan::encode).collect();
        let labels = evaluator()
            .label_spread(LabelScope::Train, &bytes, &Threads)
            .expect("a train pool is in the train scope");
        (pool, labels)
    })
}

/// The first plan a fixed stream draws that `want` takes.
fn drawn_where(want: impl Fn(&SchedulePlan) -> bool) -> SchedulePlan {
    let mut r = SplitMix::new(0x6d36_7363_6f70_6573);
    loop {
        let p = draw(&mut r).expect("a draw is in range");
        if want(&p) {
            return p;
        }
    }
}

#[test]
fn each_grid_plans_label_is_selects_feedback() {
    let ev = evaluator();
    let grid: Vec<SchedulePlan> = GRID_V1
        .iter()
        .filter_map(|&(s, w)| SchedulePlan::new(s, w).ok())
        .collect();
    let bytes: Vec<[u8; 12]> = grid.iter().map(SchedulePlan::encode).collect();
    let labels = ev
        .label_spread(LabelScope::Train, &bytes, &Threads)
        .expect("the grid is in the train scope");
    let mut spy = Spy {
        plans: grid,
        asked: Vec::new(),
        shown: Vec::new(),
    };
    let _ = ev.select_spread(&mut spy, &Threads);
    let shown = spy.shown.last().expect("four rounds");
    assert_eq!((labels.len(), shown.len()), (54, 54));
    for (label, f) in labels.iter().zip(shown) {
        assert_eq!(label.id(), f.plan());
        assert_eq!(label.totals().is_some(), f.is_admitted(), "{label:?}");
        assert_eq!(label.totals().map(|t| t.ticks), f.ticks(), "{label:?}");
        assert_eq!(label.law(), f.law(), "{label:?}");
    }
}

/// A pool's outcome, folded from its labels, is the outcome the search selects from its plans,
/// on sixteen subsets of `T_0`'s first pool. Each `select` trains its plans anew on every
/// training item (the test's whole cost: 64 subsets ran past a minute at a machine load of 40),
/// and these sixteen already reach every way a search ends: a selection, `NoGain` and
/// `NoneAdmitted`, each with one to three plans admitted, and a winner tied on `ΣT` (so `ΣZ`
/// and the bytes decide) under a selection and under `NoGain`.
#[test]
fn a_pool_outcome_is_selects_outcome() {
    let ev = evaluator();
    let (pool, labels) = train_pool();
    let mut r = SplitMix::new(0x6d36_6469_6666_6572);
    let mut classes = [0usize; 3];
    let mut tied = [false; 2];
    for _ in 0..16 {
        let mut picked: Vec<usize> = Vec::new();
        while picked.len() < JUDGED {
            let i = usize::try_from(r.below(64)).expect("an index");
            if !picked.contains(&i) {
                picked.push(i);
            }
        }
        let outcome = ev.pool_outcome(core::array::from_fn(|n| &labels[picked[n]]));
        classes[usize::from(outcome.class())] += 1;
        let ticks: Vec<u64> = picked
            .iter()
            .filter_map(|&i| labels[i].totals().map(|t| t.ticks))
            .collect();
        if let Some(least) = ticks.iter().min()
            && ticks.iter().filter(|&&t| t == *least).count() > 1
            && let Some(tie) = tied.get_mut(usize::from(outcome.class()))
        {
            *tie = true;
        }
        let mut spy = Spy {
            plans: picked.iter().map(|&i| pool[i]).collect(),
            asked: Vec::new(),
            shown: Vec::new(),
        };
        let got = (
            outcome.class(),
            outcome.plan(),
            outcome.totals(),
            outcome.gain(),
        );
        let base = ev.policy().totals(CorpusSplit::Train);
        match ev.select_spread(&mut spy, &Threads) {
            Ok(s) => assert_eq!(
                got,
                (
                    0,
                    Some(s.plan()),
                    s.train(),
                    (base.ticks - s.train().ticks, base.wasted - s.train().wasted)
                )
            ),
            Err(NoOutcome::NoGain { plan, train, .. }) => {
                assert_eq!(got, (1, Some(plan), train.candidate, (0, 0)));
            }
            Err(NoOutcome::NoneAdmitted { .. }) => {
                assert_eq!(got, (2, None, Totals::default(), (0, 0)));
            }
            Err(other) => panic!("select ended {other:?}"),
        }
    }
    assert!(
        classes.iter().all(|&n| n > 0) && tied.iter().all(|&t| t),
        "the subsets reach a selection, NoGain and NoneAdmitted, and a tied winner under the \
         first two, so each way a search ends is tested: {classes:?} {tied:?}"
    );
}

#[test]
fn with_k_equal_to_n_every_ranking_gives_one_outcome() {
    let ev = evaluator();
    let (_, labels) = train_pool();
    assert_eq!(labels.len(), POOL_PLANS);
    let mut refs: Vec<&TrainLabel> = labels.iter().collect();
    let first = ev.oracle_outcome(&refs);
    let mut r = SplitMix::new(0x6d36_6f72_6163_6c65);
    for _ in 0..16 {
        for i in (1..refs.len()).rev() {
            let j = usize::try_from(r.below(u64::try_from(i + 1).expect("a length")))
                .expect("an index");
            refs.swap(i, j);
        }
        assert_eq!(ev.oracle_outcome(&refs), first);
    }
    refs.reverse();
    assert_eq!(ev.oracle_outcome(&refs), first);
}

#[test]
fn each_scope_refuses_what_it_does_not_take() {
    let ev = Evaluator::v1(BUILD, Instant::now()).expect("the pinned policy v1");
    let grid = |p: &SchedulePlan| GRID_V1.contains(&(p.split(), p.wake()));
    let [train, held, report, practice] =
        [Side::Train, Side::HeldOut, Side::Report, Side::Practice]
            .map(|s| drawn_where(|p| side(p) == s && !grid(p)).encode());
    let mismatch = |plan| Err(LabelRefusal::ScopeMismatch { plan });
    for other in [held, report, practice] {
        assert_eq!(
            ev.label_spread(LabelScope::Train, &[train, other], &Serial),
            mismatch(Some(other)),
            "the train scope takes side-0 and grid plans alone"
        );
    }
    for other in [train, held, report] {
        assert_eq!(
            ev.label_spread(LabelScope::Practice(7), &[practice, other], &Serial),
            mismatch(Some(other)),
            "a practice scope takes side-3 plans alone"
        );
    }
    let look_set = PracticeSet::HeldOut(7);
    let read_set = PracticeSet::Report(7);
    let look = |l: &[[u8; 12]]| {
        LabelScope::HeldOut(LookToken::mint_practice(look_set, l).expect("a practice look token"))
    };
    let read = |l: &[[u8; 12]]| {
        LabelScope::Report(ReadToken::mint_practice(read_set, l).expect("a practice read token"))
    };
    let replay = |l: &[[u8; 12]]| {
        LabelScope::Replay(
            ReplayToken::mint_practice(look_set, l).expect("a practice replay token"),
        )
    };
    let scopes: [(&str, PracticeSet, &ScopeOf<'_>); 3] = [
        ("look", look_set, &look),
        ("read", read_set, &read),
        ("replay", look_set, &replay),
    ];
    let mut lists = Vec::new();
    for (name, set, scope) in scopes {
        let mut list = pool_bytes(set.set(), 0)[..3].to_vec();
        list.sort_unstable();
        assert_eq!(
            ev.label_spread(scope(&list), &list[..2], &Serial),
            mismatch(None),
            "{name}: a token takes its list, not part of it"
        );
        let mut more = list.clone();
        more.push(pool_bytes(set.set(), 1)[0]);
        assert_eq!(
            ev.label_spread(scope(&list), &more, &Serial),
            mismatch(Some(more[3])),
            "{name}: nor more than it"
        );
        let twice = [list[0], list[0], list[1], list[2]];
        assert_eq!(
            ev.label_spread(scope(&list), &twice, &Serial),
            mismatch(None),
            "{name}: nor a plan twice"
        );
        let mut reversed = list.clone();
        reversed.reverse();
        assert_eq!(
            ev.label_spread(scope(&list), &reversed, &Serial),
            mismatch(None),
            "{name}: nor its list in another order"
        );
        let drawn = set_pools(set.set()).expect("the set fills");
        let stray =
            drawn_where(|p| side(p) == Side::Practice && !drawn.iter().flatten().any(|q| q == p))
                .encode();
        assert_eq!(
            ev.label_spread(scope(&[stray]), &[stray], &Serial),
            mismatch(Some(stray)),
            "{name}: a token's plans are its set's"
        );
        lists.push((name, scope, list));
    }
    assert_eq!(
        ev.reads().train.load(Ordering::Relaxed),
        0,
        "a refusal judges nothing"
    );
    for (name, scope, list) in lists {
        let labels = ev
            .label_spread(scope(&list), &list, &Serial)
            .unwrap_or_else(|e| panic!("{name}: a token takes its own list: {e}"));
        assert_eq!(
            labels.iter().map(TrainLabel::id).collect::<Vec<_>>(),
            list.iter()
                .map(|b| SchedulePlan::decode(b).expect("a plan").digest())
                .collect::<Vec<_>>(),
            "{name}: in its order"
        );
    }
}

#[test]
fn a_label_is_judged_once_per_evaluator() {
    let ev = Evaluator::v1(BUILD, Instant::now())
        .expect("the pinned policy v1")
        .capping_labels(2);
    let (pool, _) = train_pool();
    let [a, b, c] = [pool[0], pool[1], pool[2]].map(|p| p.encode());
    let judged = || ev.reads().train.load(Ordering::Relaxed);
    let first = ev
        .label_spread(LabelScope::Train, &[a, b, a], &Serial)
        .expect("two new plans");
    let after = judged();
    assert!(after > 0, "the first labels are judged");
    assert_eq!(first[0], first[2], "a plan twice is labelled once");
    let again = ev
        .label_spread(LabelScope::Train, &[b, a], &Serial)
        .expect("cached plans");
    assert_eq!(judged(), after, "a cached label is not judged again");
    assert_eq!(again, [first[1].clone(), first[0].clone()]);
    assert_eq!(
        ev.label_spread(LabelScope::Train, &[a, c], &Serial),
        Err(LabelRefusal::Budget {
            labelled: 2,
            asked: 1,
            cap: 2,
        })
    );
    assert_eq!(judged(), after, "a refusal past the cap judges nothing");
}

#[test]
fn a_mint_refuses_a_long_list_a_wrong_side_and_a_wrong_set() {
    let set = PracticeSet::HeldOut(7);
    let report = PracticeSet::Report(7);
    let p = pool_bytes(set.set(), 0)[0];
    let held = drawn_where(|q| side(q) == Side::HeldOut).encode();
    assert_eq!(
        LookToken::mint_practice(set, &vec![p; LOOK_LIST_CAP + 1]).map(|t| t.digest()),
        Err(MintRefusal::TooLong {
            len: LOOK_LIST_CAP + 1,
            cap: LOOK_LIST_CAP,
        })
    );
    assert_eq!(
        ReplayToken::mint_practice(set, &vec![p; LOOK_LIST_CAP + 1]).map(|t| t.digest()),
        Err(MintRefusal::TooLong {
            len: LOOK_LIST_CAP + 1,
            cap: LOOK_LIST_CAP,
        })
    );
    let q = pool_bytes(report.set(), 0)[0];
    assert_eq!(
        ReadToken::mint_practice(report, &vec![q; READ_LIST_CAP + 1]).map(|t| t.digest()),
        Err(MintRefusal::TooLong {
            len: READ_LIST_CAP + 1,
            cap: READ_LIST_CAP,
        })
    );
    assert!(ReadToken::mint_practice(report, &vec![q; READ_LIST_CAP]).is_ok());
    assert_eq!(
        LookToken::mint_practice(set, &[p, held]).map(|t| t.digest()),
        Err(MintRefusal::Side { plan: held })
    );
    assert_eq!(
        LookToken::mint_practice(report, &[q]).map(|t| t.digest()),
        Err(MintRefusal::WrongSet { set: report.set() })
    );
    assert_eq!(
        ReplayToken::mint_practice(report, &[q]).map(|t| t.digest()),
        Err(MintRefusal::WrongSet { set: report.set() })
    );
    assert_eq!(
        ReadToken::mint_practice(set, &[p]).map(|t| t.digest()),
        Err(MintRefusal::WrongSet { set: set.set() })
    );
    let token = LookToken::mint_practice(set, &[p, p]).expect("a token");
    assert_eq!(token.list(), [p], "a token's list is deduplicated");
    assert_eq!(token.set(), set.set());
    let mut two = pool_bytes(set.set(), 0)[..2].to_vec();
    two.sort_unstable();
    let both = LookToken::mint_practice(set, &[two[1], two[0], two[1]]).expect("a token");
    assert_eq!(
        both.digest(),
        list_digest(&two),
        "a token holds its list's digest, whatever order and repeats it was minted from"
    );
    assert_eq!(list_digest(&[two[1], two[0]]), list_digest(&two));
    assert_ne!(list_digest(&two[..1]), list_digest(&two));
}

/// The real sets' capability, named only through `concat!` (design §13.3).
const REAL: &str = concat!("Real", "Sets");

/// The real-set mints' name, which the mint scan refuses in this file (design §13.3).
const MINT_REAL: &str = concat!("mint_", "real");

/// The names of the functions of the one `impl {ty} {` block of `src`.
fn impl_fns<'s>(src: &'s str, ty: &str) -> Vec<&'s str> {
    let head = format!("impl {ty} {{");
    assert_eq!(src.matches(&head).count(), 1, "one impl block of {ty}");
    let at = src.find(&head).expect("its impl");
    let body = &src[at..at + src[at..].find("\n}\n").expect("its end")];
    body.split("fn ")
        .skip(1)
        .filter_map(|f| f.split('(').next())
        .collect()
}

/// Every `.rs` source of this crate, with its text.
fn crate_sources() -> Vec<(std::path::PathBuf, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(std::path::PathBuf, String)>) {
        for entry in std::fs::read_dir(dir).expect("a readable source dir") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|x| x == "rs") {
                let text = std::fs::read_to_string(&path).expect("a readable source");
                out.push((path, text));
            }
        }
    }
    let mut out = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut out,
    );
    out
}

/// How often `needle` occurs across this crate's sources.
fn crate_count(needle: &str) -> usize {
    crate_sources()
        .iter()
        .map(|(_, text)| text.matches(needle).count())
        .sum()
}

#[test]
fn a_train_label_stays_private_to_m4() {
    let src = include_str!("m4.rs");
    let decl = "#[derive(Clone, Debug, PartialEq, Eq)]\npub struct TrainLabel {\n    id: Digest,\n    plan: SchedulePlan,\n    outcome: Result<Totals, &'static str>,\n}";
    assert_eq!(
        src.matches(decl).count(),
        1,
        "TrainLabel's fields are private"
    );
    let code: Vec<&str> = src
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//") && l.contains("TrainLabel"))
        .collect();
    assert_eq!(
        code,
        [
            "labels: Mutex<BTreeMap<Digest, TrainLabel>>,",
            "pub struct TrainLabel {",
            "impl TrainLabel {",
            "fn of(plan: SchedulePlan, judged: Result<TrainAdmitted, CandidateRefusal>) -> TrainLabel {",
            "TrainLabel {",
            ") -> Result<Vec<TrainLabel>, LabelRefusal> {",
            "TrainLabel::of(*p, self.train(&p.encode()))",
            "pub(super) fn pool_outcome(&self, labels: [&TrainLabel; JUDGED]) -> PoolOutcome {",
            "pub(super) fn oracle_outcome(&self, labels: &[&TrainLabel]) -> PoolOutcome {",
            "fn outcome(&self, labels: &[&TrainLabel]) -> PoolOutcome {",
        ],
        "every m4 line naming a label: one private constructor, built in one place, which only \
         label_spread calls, the cache that keeps them, and the folds that read them"
    );
    assert!(
        src.contains("\n    fn of(plan: SchedulePlan, judged"),
        "the constructor is private to m4"
    );
    assert_eq!(
        impl_fns(src, "TrainLabel"),
        ["of", "id", "plan", "totals", "law"],
        "its accessors read training facts alone"
    );
    assert_eq!(
        crate_count(&format!("for {}", "TrainLabel")),
        0,
        "no hand-written trait impl anywhere"
    );
}

#[test]
fn the_tokens_stay_private_to_m4() {
    let src = include_str!("m4.rs");
    for token in ["LookToken", "ReadToken", "ReplayToken"] {
        let decl = format!(
            "#[cfg(any(test, debug_assertions, feature = \"aos-learn\"))]\n#[derive(Debug)]\npub struct {token} {{\n    bound: Bound,\n}}"
        );
        assert_eq!(
            src.matches(&decl).count(),
            1,
            "{token}: private, neither Clone nor Copy"
        );
        assert_eq!(
            crate_count(&format!("for {token}")),
            0,
            "{token}: no hand-written trait impl anywhere"
        );
        assert_eq!(
            src.matches(&format!("{token} {{ bound }}")).count(),
            2,
            "{token}: built only by its two mints"
        );
        assert_eq!(
            impl_fns(src, token),
            [MINT_REAL, "mint_practice", "set", "list", "digest"],
            "{token}: its mints and accessors alone"
        );
    }
    assert!(src.contains("#[derive(Debug, PartialEq, Eq)]\nstruct Bound {"));
    assert_eq!(
        src.matches("Bound { set, list }").count(),
        1,
        "a binding is built only by Bound::mint"
    );
}

#[test]
fn the_mints_are_pinned() {
    let src = include_str!("m4.rs");
    for (token, cap) in [
        ("LookToken", "LOOK_LIST_CAP"),
        ("ReadToken", "READ_LIST_CAP"),
        ("ReplayToken", "LOOK_LIST_CAP"),
    ] {
        let real = format!(
            "    pub(super) fn {MINT_REAL}(\n        _sets: &{REAL},\n        i: u32,\n        list: &[[u8; PLAN_LEN]],\n    ) -> Result<{token}, MintRefusal> {{"
        );
        assert_eq!(
            src.matches(&real).count(),
            1,
            "{token}'s real mint takes the capability"
        );
        let practice = format!(
            "    pub(super) fn mint_practice(\n        set: PracticeSet,\n        list: &[[u8; PLAN_LEN]],\n    ) -> Result<{token}, MintRefusal> {{"
        );
        assert_eq!(
            src.matches(&practice).count(),
            1,
            "{token}'s practice mint takes a practice set"
        );
        assert!(src.contains(&format!(
            "Bound::mint(set, {cap}, list).map(|bound| {token} {{ bound }})"
        )));
    }
    assert_eq!(src.matches(&format!("fn {MINT_REAL}(")).count(), 3);
    assert_eq!(src.matches("fn mint_practice(").count(), 3);
    assert_eq!(
        src.matches("Bound::mint(").count(),
        6,
        "every token comes from a mint"
    );
}

#[test]
fn the_real_sets_capability_is_pinned() {
    let src = include_str!("m4.rs");
    let decl = format!(
        "pub struct {REAL}(\n    #[expect(\n        dead_code,\n        reason = \"read by nothing: it makes the zero bit pattern invalid, so `mem::zeroed` panics\"\n    )]\n    NonZeroU64,\n);\n"
    );
    assert_eq!(
        src.matches(&decl).count(),
        1,
        "one private NonZeroU64 field, nothing else"
    );
    let at = src.find(&decl).expect("its declaration");
    let attrs: Vec<&str> = src[..at]
        .lines()
        .rev()
        .take_while(|l| !l.starts_with("///"))
        .collect();
    assert_eq!(
        attrs,
        ["#[cfg(any(test, debug_assertions, feature = \"aos-learn\"))]"],
        "no derive, between its doc comment and its declaration"
    );
    assert_eq!(impl_fns(src, REAL), ["new"], "one constructor");
    assert!(src.contains(&format!(
        "    pub const fn new() -> {REAL} {{\n        {REAL}(NonZeroU64::MIN)\n    }}"
    )));
    for (needle, n, what) in [
        (format!("for {REAL}"), 0, "no hand-written trait impl"),
        (format!("impl {REAL}"), 1, "its one inherent impl"),
        (format!("-> {REAL}"), 1, "one function returns one"),
        (format!("{REAL}("), 2, "its declaration and its constructor"),
    ] {
        assert_eq!(crate_count(&needle), n, "{what}, anywhere in the crate");
    }
}
