// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Distributed search (design §11.3, §11.6, §11.7): a worker's reply is a claim, judged here as
//! far as the winner needs before any held-out read; the wire; the coordinator under faults.

use core::ops::ControlFlow;
use std::collections::BTreeSet;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use aterm_aos::Digest;
use aterm_aos::wire::{Enc, WireFault};

use super::bundle::{
    BundleField, BundleRefusal, CandidateBundle, Row, RowVerdict, SelectionProvenance,
};
use super::dist::{
    Action, Binary, Claim, ClaimKind, Coordinator, Event, ExecIdentity, FrameFault, JOB_FORMAT,
    JOB_MAGIC, JobEnd, KIND_TRAIN, MAX_ROWS, Msg, RawClaim, Refusal, Sender, ShapeFault,
    claim_digest, job_id, run_job,
};
use super::lease::{LIMITS_V1, LeaseRefusal, Limits, Nanos, Outcome, Token, WorkerId, WorkerState};
use super::m4::{CheckLevel, DistNoOutcome, Evaluator, NoOutcome, Selection, Serial, Spread};
use super::m4_tests::{evaluator, gap0, plan};
use super::plan::{SHIPPED_PLAN, SchedulePlan, Split, Wake};
use super::search::{FileProposer, GridProposer, Mode, Proposer, ProposerId, TrainFeedback};

/// Every job on the test pool's threads.
struct Threads;

impl Spread for Threads {
    fn run(&self, n: usize, job: &(dyn Fn(usize) + Sync)) {
        let all: Vec<usize> = (0..n).collect();
        crate::test_pool::par_map(&all, |&i| job(i));
    }
}

/// Only the even jobs, on the caller's thread: the caller must run the rest in place. Counts the
/// jobs it skipped.
#[derive(Default)]
struct Evens {
    skipped: AtomicU64,
}

impl Spread for Evens {
    fn run(&self, n: usize, job: &(dyn Fn(usize) + Sync)) {
        (0..n).step_by(2).for_each(job);
        self.skipped.fetch_add((n / 2) as u64, Ordering::Relaxed);
    }
}

/// An evaluator whose reads and grid cache are this test's alone.
fn fresh() -> Evaluator {
    evaluator().anew()
}

fn n(r: &AtomicU64) -> u64 {
    r.load(Ordering::Relaxed)
}

/// `plan`'s honest claim, judged by `ev`.
fn honest(ev: &Evaluator, plan: SchedulePlan) -> Claim {
    match run_job(ev, plan, &mut |_| ControlFlow::Continue(())).expect("an honest shape") {
        JobEnd::Claimed(c) => c,
        JobEnd::Stopped { .. } => panic!("nothing stops it"),
    }
}

fn grid_plans() -> Vec<SchedulePlan> {
    GridProposer::plans()
        .iter()
        .map(|b| SchedulePlan::decode(b).expect("a grid plan"))
        .collect()
}

/// The grid's honest claims, by slot, judged once on the shared evaluator's threads.
fn grid_claims() -> &'static [Claim] {
    static C: OnceLock<Vec<Claim>> = OnceLock::new();
    C.get_or_init(|| {
        let ev = evaluator();
        crate::test_pool::par_map(&grid_plans(), |&p| honest(ev, p))
    })
}

/// The grid's local selection, run once as `select` runs it (serially) for the tests that compare
/// a distributed or spread one with it. The rows it reads are judged first, on threads, by the
/// shared search, so its serial fold waits on no serial judgment.
fn local_grid() -> &'static Selection {
    static L: OnceLock<Selection> = OnceLock::new();
    L.get_or_init(|| {
        let _ = super::m4_tests::searched();
        evaluator()
            .select(&mut GridProposer::new())
            .expect("a winner")
    })
}

/// `plan`'s honest claim, for a plan of [`six`]: each judged once on the shared evaluator's
/// threads.
fn six_claim(plan: SchedulePlan) -> Claim {
    static C: OnceLock<Vec<(SchedulePlan, Claim)>> = OnceLock::new();
    let claims = C.get_or_init(|| {
        let ev = evaluator();
        crate::test_pool::par_map(&six(), |&p| (p, honest(ev, p)))
    });
    let (_, claim) = claims
        .iter()
        .find(|(p, _)| *p == plan)
        .expect("a plan of six");
    claim.clone()
}

/// The six-line plans file of the process tests: three admitted, three refused.
fn six() -> Vec<SchedulePlan> {
    vec![
        gap0(),
        SHIPPED_PLAN,
        plan(Split::Shipped, Wake::Never),
        plan(Split::Shipped, Wake::ColdOnly),
        plan(Split::Shipped, Wake::Poll { period_us: 1_000 }),
        plan(Split::Shipped, Wake::Cadence),
    ]
}

/// A plans file of `plans`, with an undecodable line, an oversized one and a duplicate.
fn plans_text(plans: &[SchedulePlan]) -> String {
    let mut text: String = plans
        .iter()
        .map(|p| format!("{}\n", hex(&p.encode())))
        .collect();
    text.push_str("ffff0000\n");
    text.push_str(&"ab".repeat(80));
    text.push('\n');
    if let Some(p) = plans.first() {
        text.push_str(&format!("{}  # again\n", hex(&p.encode())));
    }
    text
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A distributed search of `proposer` on `ev`, given `claims` (by slot) in `order`.
fn distribute<'a>(
    ev: &'a Evaluator,
    proposer: &'a mut dyn Proposer,
    check: CheckLevel,
    claim_of: &dyn Fn(SchedulePlan) -> Claim,
    order: &[usize],
) -> super::m4::Distributed<'a> {
    let mut d = ev.distributed(proposer, check);
    let jobs = d.jobs().to_vec();
    for &i in order {
        let j = jobs.get(i).expect("a job");
        d.claim(j.slot, claim_of(j.plan)).expect("an open slot");
    }
    d
}

fn same_selection(a: &Selection, b: &Selection) {
    assert_eq!(
        (
            a.plan(),
            a.round(),
            a.index(),
            a.stats(),
            a.train(),
            a.proposer()
        ),
        (
            b.plan(),
            b.round(),
            b.index(),
            b.stats(),
            b.train(),
            b.proposer()
        )
    );
}

/// The orders claims arrive in: forward, backward, and four interleavings.
fn orders(n: usize) -> Vec<Vec<usize>> {
    let fwd: Vec<usize> = (0..n).collect();
    let back: Vec<usize> = (0..n).rev().collect();
    let evens_first: Vec<usize> = (0..n).step_by(2).chain((1..n).step_by(2)).collect();
    let odds_first: Vec<usize> = (1..n).step_by(2).chain((0..n).step_by(2)).collect();
    let mut rot = fwd.clone();
    rot.rotate_left(n / 3);
    let mut swapped = fwd.clone();
    for pair in swapped.chunks_mut(2) {
        pair.reverse();
    }
    vec![fwd, back, evens_first, odds_first, rot, swapped]
}

/// Honest claims, in any completion order, give the local selection, and after the one
/// held-out read the local bundle, mode aside: the grid and a plans file, at both check levels.
/// The order a claim arrives in reaches only how it is filed, so every order of the grid must file
/// the claims the slot order files, and that search, finished at each level, is every order's.
#[test]
fn honest_claims_in_any_completion_order_give_the_local_selection() {
    let ev = evaluator();
    let plans = grid_plans();
    let local = local_grid();
    let local_bundle = ev.held_out(local.clone()).expect("useful");
    let orders_of_the_grid = orders(plans.len());
    for check in [CheckLevel::Dominance, CheckLevel::All] {
        // The order is the law here; that a spread judges as serial does is
        // `select_spread_equals_select`'s and `finish_with_a_skipping_spread_equals_serial`'s.
        let mut g = GridProposer::new();
        let slot_order = distribute(ev, &mut g, check, &grid_claim, &orders_of_the_grid[0]);
        for (i, order) in orders_of_the_grid.iter().enumerate().skip(1) {
            let mut g = GridProposer::new();
            let filed = distribute(ev, &mut g, check, &grid_claim, order);
            assert!(
                filed.claims() == slot_order.claims(),
                "order {i} files other claims"
            );
        }
        let s = slot_order.finish(&Threads).expect("a winner");
        same_selection(&s, local);
        let mut parts = ev.held_out(s).expect("useful").bundle().parts().clone();
        let want = match check {
            CheckLevel::Dominance => Mode::Distributed,
            CheckLevel::All => Mode::Search,
        };
        assert_eq!(parts.selection.mode, want);
        parts.selection.mode = Mode::Search;
        assert_eq!(&parts, local_bundle.bundle().parts(), "{check:?}");
    }
    let text = plans_text(&six());
    let mut file = FileProposer::from_text(&text);
    let local = ev.select(&mut file).expect("a winner");
    for order in orders(6) {
        let mut file = FileProposer::from_text(&text);
        let s = distribute(ev, &mut file, CheckLevel::Dominance, &six_claim, &order)
            .finish(&Threads)
            .expect("a winner");
        same_selection(&s, &local);
    }
    assert_eq!(local.stats().admitted, 3);
    assert_eq!(local.stats().oversized, 1);
    assert_eq!(local.stats().duplicates, 1);
}

/// `select_spread`, on threads or on a spread that skips jobs, selects as `select` does, and so
/// does a serial finish at `CheckLevel::All`: the grid on threads; the plans file of [`six`] on
/// the spread that skips (which runs the jobs it skips in place, on one thread) and serially,
/// where the grid's 54 plans, judged twice over on one thread, took some 30 s in a debug build.
/// Every selection here reads the shared evaluator's judgments ([`Evaluator::memoizing`]), so
/// this is the fold's and the spread's law; that a judgment made on a thread is the one a fresh
/// evaluator makes serially is `a_memoizing_evaluator_judges_each_pair_once_as_a_fresh_one_does`'s
/// and `m6_tests::the_chain_is_deterministic`'s.
#[test]
fn select_spread_equals_select() {
    let ev = evaluator();
    let threaded = ev
        .select_spread(&mut GridProposer::new(), &Threads)
        .expect("a winner");
    same_selection(&threaded, local_grid());
    let text = plans_text(&six());
    let local = ev
        .select(&mut FileProposer::from_text(&text))
        .expect("a winner");
    let evens = Evens::default();
    let skipping = ev
        .select_spread(&mut FileProposer::from_text(&text), &evens)
        .expect("a winner");
    assert!(n(&evens.skipped) > 0, "the spread skips a job");
    same_selection(&skipping, &local);
    let slots: Vec<usize> = (0..six().len()).collect();
    let mut file = FileProposer::from_text(&text);
    let serial = distribute(ev, &mut file, CheckLevel::All, &six_claim, &slots)
        .finish(&Serial)
        .expect("every threaded row equals its serial judgment");
    same_selection(&serial, &local);
}

/// `plan`'s honest claim, for a plan of the grid ([`grid_claims`]).
fn grid_claim(plan: SchedulePlan) -> Claim {
    let at = grid_plans()
        .iter()
        .position(|q| *q == plan)
        .expect("a grid plan");
    grid_claims().get(at).expect("a claim").clone()
}

/// Every grid job's index, in slot order.
fn grid_slots() -> Vec<usize> {
    (0..grid_plans().len()).collect()
}

/// `finish` with a spread that skips jobs runs the rest in place: the same selection and the
/// same judgments as serial, at both check levels, the spread skipping jobs at each. Over the
/// plans file of [`six`] (three admitted, three refused), whose finish judges a handful of claims
/// where the grid's 54, judged four times over on one thread, took 30-45 s in a debug build.
#[test]
fn finish_with_a_skipping_spread_equals_serial() {
    let ev = evaluator();
    let text = plans_text(&six());
    let slots: Vec<usize> = (0..six().len()).collect();
    let set = |v: &[(u32, usize)]| v.iter().copied().collect::<BTreeSet<_>>();
    for check in [CheckLevel::Dominance, CheckLevel::All] {
        let finish = |spread: &dyn Spread| {
            let mut file = FileProposer::from_text(&text);
            let (s, judged) =
                distribute(ev, &mut file, check, &six_claim, &slots).finish_noting(spread);
            (s.expect("a winner"), judged)
        };
        let (serial, judged_serial) = finish(&Serial);
        let evens = Evens::default();
        let (skipped, judged_skipped) = finish(&evens);
        assert!(
            n(&evens.skipped) > 0,
            "{check:?}: the spread skips a job, which the finish must run in place"
        );
        same_selection(&serial, &skipped);
        assert_eq!(set(&judged_serial), set(&judged_skipped), "{check:?}");
        assert!(
            !judged_serial.is_empty(),
            "{check:?}: the finish judges rows"
        );
    }
}

/// Records what the proposer was shown each round.
struct Shown<P> {
    inner: P,
    shown: Vec<Vec<TrainFeedback>>,
}

impl<P: Proposer> Proposer for Shown<P> {
    fn id(&self) -> ProposerId {
        self.inner.id()
    }

    fn propose(&mut self, round: u8, feedback: &[TrainFeedback]) -> Vec<Vec<u8>> {
        self.shown.push(feedback.to_vec());
        self.inner.propose(round, feedback)
    }
}

/// The order claims complete in never reaches the proposer: it is shown the local search's
/// feedback, in proposal order, every round.
#[test]
fn completion_order_never_reaches_the_proposer() {
    let ev = evaluator();
    let text = plans_text(&six());
    let mut local = Shown {
        inner: FileProposer::from_text(&text),
        shown: Vec::new(),
    };
    ev.select(&mut local).expect("a winner");
    for order in orders(6) {
        let mut spy = Shown {
            inner: FileProposer::from_text(&text),
            shown: Vec::new(),
        };
        distribute(ev, &mut spy, CheckLevel::Dominance, &six_claim, &order)
            .finish(&Serial)
            .expect("a winner");
        assert_eq!(spy.shown, local.shown);
    }
}

/// An admitted plan claimed refused at item `j` is contradicted, and only its claimed prefix,
/// `j + 1` training items, is judged.
#[test]
fn a_refusal_claimed_for_an_admitted_plan_is_contradicted() {
    let ev = fresh();
    let pins = ev.policy().items().to_vec();
    let truth = honest(evaluator(), gap0());
    let j = 4;
    let mut raw = RawClaim::from(truth);
    raw = raw.altered(ClaimKind::Refused, |rows| {
        rows.truncate(j + 1);
        if let Some(r) = rows.last_mut() {
            r.stranded_light = RowVerdict::fail("HotHasWake", 3);
        }
    });
    let lie = raw.shaped(&pins).expect("a refusal's shape");
    let text = plans_text(&[gap0()]);
    let mut file = FileProposer::from_text(&text);
    let mut d = ev.distributed(&mut file, CheckLevel::Dominance);
    d.claim(0, lie).expect("an open slot");
    let (got, judged) = d.finish_noting(&Serial);
    assert!(
        matches!(
            got,
            Err(DistNoOutcome::Contradicted {
                slot: 0,
                field: BundleField::StrandedLightVerdict,
                ..
            })
        ),
        "{got:?}"
    );
    assert_eq!(judged, (0..=j).map(|t| (0, t)).collect::<Vec<_>>());
    assert_eq!(n(&ev.reads().train), u64::try_from(j + 1).expect("small"));
    assert_eq!(n(&ev.reads().held_out), 0);
}

/// A dominated plan's claim lowered by one tick on every row, so that it claims the best key, is
/// judged in full as the claimed winner and contradicted: no selection, so no held-out read.
#[test]
fn a_forged_best_is_contradicted_before_any_held_out_read() {
    let ev = fresh();
    let pins = ev.policy().items().to_vec();
    let six = six();
    let liar = SHIPPED_PLAN;
    let claim_of = |p: SchedulePlan| {
        let c = honest(evaluator(), p);
        if p != liar {
            return c;
        }
        RawClaim::from(c)
            .altered(ClaimKind::Admitted, |rows| {
                for r in rows {
                    r.ticks = r.ticks.saturating_sub(400);
                    r.presented = r.presented.min(r.ticks);
                    r.wasted = r.ticks - r.presented;
                }
            })
            .shaped(&pins)
            .expect("still a training run's shape")
    };
    let text = plans_text(&six);
    let mut file = FileProposer::from_text(&text);
    let all: Vec<usize> = (0..6).collect();
    let got = distribute(&ev, &mut file, CheckLevel::Dominance, &claim_of, &all).finish(&Serial);
    assert!(
        matches!(got, Err(DistNoOutcome::Contradicted { slot: 1, .. })),
        "{got:?}"
    );
    assert_eq!(n(&ev.reads().held_out), 0);
    assert_eq!(n(&ev.reads().looks), 0);
}

/// The steering attack (design §11.1): the true winner's claim, inflated so another admitted plan
/// claims the best key, is never dominated, so once the claimed winner is judged in full the check
/// reaches an inflated row and voids the search before the held-out split is read.
#[test]
fn an_inflated_winner_is_found_by_the_dominance_check() {
    let ev = fresh();
    let pins = ev.policy().items().to_vec();
    let claim_of = |p: SchedulePlan| {
        let c = honest(evaluator(), p);
        if p != gap0() {
            return c;
        }
        RawClaim::from(c)
            .altered(ClaimKind::Admitted, |rows| {
                for r in rows {
                    r.ticks += 10_000;
                    r.presented += 10_000;
                }
            })
            .shaped(&pins)
            .expect("still a training run's shape")
    };
    let text = plans_text(&six());
    let mut file = FileProposer::from_text(&text);
    let all: Vec<usize> = (0..6).collect();
    let (got, judged) =
        distribute(&ev, &mut file, CheckLevel::Dominance, &claim_of, &all).finish_noting(&Serial);
    match got {
        Err(DistNoOutcome::Contradicted { slot: 0, field, .. }) => {
            assert_eq!(field, BundleField::Ticks);
        }
        other => panic!("{other:?}"),
    }
    assert!(
        judged.contains(&(1, 16)),
        "the claimed winner judged in full"
    );
    assert_eq!(n(&ev.reads().held_out), 0);
}

/// The pairs step 5 judges, from the claims alone: every refused prefix, the claimed winner in
/// full, and each other admitted claim in descending claimed `T` until its `ΣT` exceeds the
/// winner's.
fn predicted(claims: &[Claim], plans: &[SchedulePlan]) -> BTreeSet<(u32, usize)> {
    let key = |i: usize| {
        let c = claims.get(i).expect("a claim");
        let t = c.totals();
        (t.ticks, t.wasted, plans.get(i).expect("a plan").encode())
    };
    let best = (0..claims.len())
        .filter(|&i| claims.get(i).map(Claim::kind) == Some(ClaimKind::Admitted))
        .min_by_key(|&i| key(i))
        .expect("an admitted claim");
    let winner = claims.get(best).expect("a claim").totals().ticks;
    let mut out = BTreeSet::new();
    for (i, c) in claims.iter().enumerate() {
        let slot = u32::try_from(i).expect("small");
        match c.kind() {
            ClaimKind::Refused => out.extend((0..c.len()).map(|t| (slot, t))),
            ClaimKind::Admitted if i == best => out.extend((0..c.len()).map(|t| (slot, t))),
            ClaimKind::Admitted => {
                let mut order: Vec<usize> = (0..c.len()).collect();
                order.sort_by_key(|&t| (core::cmp::Reverse(c.ticks(t)), t));
                let mut sum = 0u64;
                for t in order {
                    out.insert((slot, t));
                    sum += c.ticks(t).expect("a row");
                    if sum > winner {
                        break;
                    }
                }
            }
        }
    }
    out
}

/// On the grid, the dominance check judges exactly the pairs §11.3 predicts, and the evaluator
/// read exactly that many training items, none held out.
#[test]
fn a_dominated_claim_stops_at_the_winners_sum() {
    let ev = fresh();
    let plans = grid_plans();
    let claims = grid_claims();
    let mut g = GridProposer::new();
    let (got, judged) = distribute(
        &ev,
        &mut g,
        CheckLevel::Dominance,
        &grid_claim,
        &grid_slots(),
    )
    .finish_noting(&Threads);
    let s = got.expect("a winner");
    assert_eq!(s.plan(), gap0());
    let judged: BTreeSet<(u32, usize)> = judged.into_iter().collect();
    assert_eq!(judged, predicted(claims, &plans));
    assert_eq!(
        n(&ev.reads().train),
        u64::try_from(judged.len()).expect("small")
    );
    assert_eq!(n(&ev.reads().held_out), 0);
    let full = claims
        .iter()
        .filter(|c| c.kind() == ClaimKind::Admitted)
        .count()
        * 17;
    assert!(
        judged.len() < full,
        "the check judged less than every admitted claim in full"
    );
}

/// Under `CheckLevel::All` every claim is judged in full, the result is recorded as a search,
/// and a grid search fills the evaluator's grid cache.
#[test]
fn check_all_judges_every_admitted_claim_and_records_search() {
    let ev = fresh();
    let claims = grid_claims();
    let mut g = GridProposer::new();
    let (got, judged) = distribute(&ev, &mut g, CheckLevel::All, &grid_claim, &grid_slots())
        .finish_noting(&Threads);
    let s = got.expect("a winner");
    let want: usize = claims.iter().map(Claim::len).sum();
    assert_eq!(judged.len(), want);
    assert!(ev.grid_cached(), "the search filled the grid's cache");
    let before = n(&ev.reads().train);
    let grid = ev.grid_selection(&Serial).expect("the grid's selection");
    assert_eq!(
        n(&ev.reads().train),
        before,
        "the cache was filled: nothing re-ran"
    );
    assert_eq!(
        (grid.plan, grid.index, grid.stats),
        (s.plan(), s.index(), s.stats())
    );
    let a = ev.held_out(s).expect("useful");
    assert_eq!(a.bundle().parts().selection.mode, Mode::Search);
}

/// Under the dominance check the grid cache is never filled from claims.
#[test]
fn claims_never_fill_the_grid_cache() {
    let ev = fresh();
    let mut g = GridProposer::new();
    distribute(
        &ev,
        &mut g,
        CheckLevel::Dominance,
        &grid_claim,
        &grid_slots(),
    )
    .finish(&Threads)
    .expect("a winner");
    assert!(!ev.grid_cached(), "a claim filled the grid's cache");
}

/// A distributed bundle verifies with declared provenance; one naming the grid must name a grid
/// plan at its index with the grid's counts, at round 0, and every distributed bundle must beat
/// the baseline on training.
#[test]
fn a_distributed_bundle_is_checked_and_declared() {
    let ev = evaluator();
    let mut g = GridProposer::new();
    let s = distribute(
        ev,
        &mut g,
        CheckLevel::Dominance,
        &grid_claim,
        &grid_slots(),
    )
    .finish(&Serial)
    .expect("a winner");
    let parts = ev.held_out(s).expect("useful").bundle().parts().clone();
    assert_eq!(parts.selection.mode, Mode::Distributed);
    let verify = |p: &super::bundle::UncheckedBundle| CandidateBundle::verify(&p.encode(), ev);
    let ok = verify(&parts).expect("a distributed bundle verifies");
    assert_eq!(ok.selection_provenance(), SelectionProvenance::Declared);
    let forged = |field| Err(BundleRefusal::Forged { field, row: None });
    let mut index = parts.clone();
    index.index += 1;
    let mut round = parts.clone();
    round.round = 1;
    let mut counts = parts.clone();
    counts.selection.stats.duplicates += 1;
    counts.selection.stats.proposed += 1;
    let mut admitted = parts.clone();
    admitted.selection.stats.admitted -= 1;
    let mut other_proposer = parts.clone();
    other_proposer.proposer = ProposerId::new("someone", 1, b"x");
    other_proposer.round = 0;
    other_proposer.index = 3;
    assert_eq!(verify(&index), forged(BundleField::Proposer));
    assert_eq!(verify(&round), forged(BundleField::Proposer));
    assert_eq!(verify(&counts), forged(BundleField::Stats));
    let declared = verify(&admitted).expect("the admitted count is declared");
    assert_eq!(
        declared.selection_provenance(),
        SelectionProvenance::Declared
    );
    verify(&other_proposer).expect("another proposer's distributed search is declared data");
    let shipped = ev
        .admit_one(&SHIPPED_PLAN.encode())
        .expect("the shipped plan is admitted")
        .bundle()
        .parts()
        .clone();
    assert!(
        !shipped.selection.train.beats(),
        "fixture: the baseline does not beat itself"
    );
    let mut no_gain = shipped;
    no_gain.selection.mode = Mode::Distributed;
    no_gain.selection.stats = parts.selection.stats;
    no_gain.proposer = ProposerId::new("someone", 1, b"x");
    assert_eq!(verify(&no_gain), forged(BundleField::Sums));
    assert_eq!(Mode::from_tag(2), Some(Mode::Distributed));
    assert_eq!(Mode::Distributed.tag(), 2);
    assert_eq!(Mode::from_tag(3), None);
}

/// A proposer that proposes after round 0 ends a distributed search `Adaptive`.
#[test]
fn a_proposal_after_round_zero_is_adaptive() {
    struct Late;
    impl Proposer for Late {
        fn id(&self) -> ProposerId {
            ProposerId::new("late", 1, &[])
        }
        fn propose(&mut self, round: u8, _: &[TrainFeedback]) -> Vec<Vec<u8>> {
            match round {
                0 => vec![gap0().encode().to_vec()],
                2 => vec![SHIPPED_PLAN.encode().to_vec()],
                _ => Vec::new(),
            }
        }
    }
    let ev = fresh();
    let mut late = Late;
    let mut d = ev.distributed(&mut late, CheckLevel::Dominance);
    d.claim(0, honest(evaluator(), gap0()))
        .expect("an open slot");
    assert_eq!(
        d.finish(&Serial).err(),
        Some(DistNoOutcome::Adaptive { round: 2 })
    );
    assert_eq!(n(&ev.reads().looks), 0, "no held-out gate is entered");
    assert_eq!(n(&ev.reads().held_out), 0);
}

/// A job with no claim, or a claim for no job or a second one, is refused by name.
#[test]
fn claims_must_fill_every_job_once() {
    let ev = evaluator();
    let text = plans_text(&[gap0(), SHIPPED_PLAN]);
    let mut file = FileProposer::from_text(&text);
    let mut d = ev.distributed(&mut file, CheckLevel::Dominance);
    assert_eq!(d.jobs().len(), 2);
    d.claim(0, honest(ev, gap0())).expect("an open slot");
    assert_eq!(
        d.claim(0, honest(ev, gap0())),
        Err(DistNoOutcome::Misplaced { slot: 0 })
    );
    assert_eq!(
        d.claim(7, honest(ev, gap0())),
        Err(DistNoOutcome::Misplaced { slot: 7 })
    );
    assert_eq!(
        d.finish(&Serial).err(),
        Some(DistNoOutcome::Unclaimed { slot: 1 })
    );
}

/// When every claim is refused and each refusal holds, there is no winner, as in a local search.
#[test]
fn every_refusal_held_is_none_admitted() {
    let ev = evaluator();
    let refused = [
        plan(Split::Shipped, Wake::Never),
        plan(Split::Shipped, Wake::ColdOnly),
    ];
    let text = plans_text(&refused);
    let mut file = FileProposer::from_text(&text);
    let local = ev.select(&mut file).expect_err("none admitted");
    let mut file = FileProposer::from_text(&text);
    let honest_of = |p: SchedulePlan| honest(ev, p);
    let got = distribute(ev, &mut file, CheckLevel::Dominance, &honest_of, &[0, 1]).finish(&Serial);
    assert_eq!(got.map(|s| s.plan()), Err(DistNoOutcome::Local(local)));
    assert!(matches!(
        got_none(ev, &text),
        NoOutcome::NoneAdmitted { .. }
    ));
}

fn got_none(ev: &Evaluator, text: &str) -> NoOutcome {
    ev.select(&mut FileProposer::from_text(text))
        .expect_err("none admitted")
}

fn rows_of(c: &Claim) -> Vec<Row> {
    let mut e = Enc::new();
    c.encode(&mut e);
    let bytes = e.finish();
    let mut d = aterm_aos::wire::Dec::new(&bytes);
    let raw = RawClaim::decode(&mut d).expect("its own bytes");
    let mut rows = Vec::new();
    let _ = raw.altered(c.kind(), |r| rows.clone_from(r));
    rows
}

/// Every shape fault is refused by name: a count, another item's row, a failing row, a refusal
/// whose last row passes, and a `Z` that is not `T − P`.
#[test]
fn each_shape_fault_is_refused_by_name() {
    let ev = evaluator();
    let pins = ev.policy().items().to_vec();
    let admitted = rows_of(&honest(ev, gap0()));
    let refused = rows_of(&honest(ev, plan(Split::Shipped, Wake::ColdOnly)));
    assert_eq!(refused.len(), 2, "fixture: ColdOnly refuses at item 1");
    let shaped = |kind, rows: Vec<Row>| Claim::shaped(kind, rows, &pins).map(|_| ());
    assert_eq!(shaped(ClaimKind::Admitted, admitted.clone()), Ok(()));
    assert_eq!(shaped(ClaimKind::Refused, refused.clone()), Ok(()));
    let mut short = admitted.clone();
    short.pop();
    assert_eq!(
        shaped(ClaimKind::Admitted, short),
        Err(ShapeFault::Count {
            kind: ClaimKind::Admitted,
            rows: 16
        })
    );
    assert_eq!(
        shaped(ClaimKind::Refused, Vec::new()),
        Err(ShapeFault::Count {
            kind: ClaimKind::Refused,
            rows: 0
        })
    );
    let mut swapped = admitted.clone();
    swapped.swap(2, 3);
    assert_eq!(
        shaped(ClaimKind::Admitted, swapped),
        Err(ShapeFault::Item { row: 2 })
    );
    let mut failing = admitted.clone();
    if let Some(r) = failing.get_mut(5) {
        r.run = RowVerdict::fail("LaneNotLogged", 0);
    }
    assert_eq!(
        shaped(ClaimKind::Admitted, failing),
        Err(ShapeFault::Fails { row: 5 })
    );
    let mut passes = refused.clone();
    if let Some(r) = passes.last_mut() {
        r.lane_fidelity = RowVerdict::Pass;
        r.stranded_light = RowVerdict::Pass;
        r.run = RowVerdict::Pass;
    }
    assert_eq!(
        shaped(ClaimKind::Refused, passes),
        Err(ShapeFault::LastPasses { row: 1 })
    );
    let mut wasted = admitted.clone();
    if let Some(r) = wasted.get_mut(7) {
        r.wasted += 1;
    }
    assert_eq!(
        shaped(ClaimKind::Admitted, wasted),
        Err(ShapeFault::Wasted { row: 7 })
    );
    let mut e = Enc::new();
    e.u8(ClaimKind::Admitted.tag());
    e.u64(MAX_ROWS + 1);
    let bytes = e.finish();
    assert_eq!(
        RawClaim::decode(&mut aterm_aos::wire::Dec::new(&bytes)),
        Err(WireFault::TooLong {
            field: "claim.rows",
            len: MAX_ROWS + 1,
            limit: MAX_ROWS
        }),
        "a count past the bound is refused before any row is read"
    );
}

/// `run_job` judges the training split only, and stops within one item of being told to.
#[test]
fn run_job_reads_no_held_out_item_and_stops_when_told() {
    let ev = fresh();
    let c = honest(&ev, gap0());
    assert_eq!(c.kind(), ClaimKind::Admitted);
    assert_eq!(c.len(), 17);
    assert_eq!((n(&ev.reads().train), n(&ev.reads().held_out)), (17, 0));
    let ev = fresh();
    let stopped = run_job(&ev, gap0(), &mut |k| {
        if k == 3 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .expect("a shape");
    assert_eq!(stopped, JobEnd::Stopped { at: 4 });
    assert_eq!((n(&ev.reads().train), n(&ev.reads().held_out)), (4, 0));
}

fn identity(ev: &Evaluator) -> ExecIdentity {
    ExecIdentity::of(
        ev,
        Binary {
            toolchain: "trustc 1.95.0".to_owned(),
            build_profile: "debug opt=0 effects-debug-assertions".to_owned(),
            executable: Digest([3; 32]),
            faults: Vec::new(),
        },
    )
}

fn token(slot: u32, attempt: u8, worker: u32) -> Token {
    Token {
        search: 1,
        slot,
        attempt,
        worker,
    }
}

/// Every message encodes to bytes that decode to it; each cut names its field; trailing bytes,
/// an unknown tag, a message sent the wrong way, another format and a reserved job kind are each
/// refused by variant.
#[test]
fn every_message_round_trips_and_every_fault_is_named() {
    let ev = evaluator();
    let c = RawClaim::from(honest(ev, gap0()));
    let worker_msgs = [
        Msg::Hello {
            identity: Box::new(identity(ev)),
            pid: 41,
            evaluator_ns: 9,
        },
        Msg::Progress {
            token: token(2, 1, 3),
            k: 7,
        },
        Msg::Result {
            token: token(2, 1, 3),
            job: Digest([5; 32]),
            claim: c,
            stepped: 11,
            judge_ns: 12,
        },
        Msg::Cancelled {
            token: token(2, 1, 3),
            at: 4,
        },
    ];
    let coordinator_msgs = [
        Msg::Lease {
            token: token(2, 1, 3),
            job: Digest([5; 32]),
            kind: KIND_TRAIN,
            plan: gap0().encode(),
        },
        Msg::Cancel {
            token: token(2, 1, 3),
        },
    ];
    for (msgs, from, other) in [
        (&worker_msgs[..], Sender::Worker, Sender::Coordinator),
        (&coordinator_msgs[..], Sender::Coordinator, Sender::Worker),
    ] {
        for m in msgs {
            let bytes = m.encode();
            assert!(bytes.len() < 64 * 1024);
            assert_eq!(Msg::decode(&bytes, from).as_ref(), Ok(m));
            assert_eq!(
                Msg::decode(&bytes, other),
                Err(FrameFault::Direction { tag: m.tag() })
            );
            for cut in 9..bytes.len() {
                let got = Msg::decode(bytes.get(..cut).expect("a prefix"), from);
                assert!(
                    matches!(got, Err(FrameFault::Wire(WireFault::Truncated { .. }))),
                    "tag {} cut at {cut}: {got:?}",
                    m.tag()
                );
            }
            let mut long = bytes.clone();
            long.push(0);
            assert_eq!(
                Msg::decode(&long, from),
                Err(FrameFault::Wire(WireFault::Trailing { extra: 1 }))
            );
        }
    }
    let mut bad = JOB_MAGIC.to_vec();
    bad.extend([JOB_FORMAT, 9]);
    assert_eq!(
        Msg::decode(&bad, Sender::Worker),
        Err(FrameFault::Tag { tag: 9 })
    );
    let mut format = JOB_MAGIC.to_vec();
    format.extend([2, 1]);
    assert_eq!(
        Msg::decode(&format, Sender::Worker),
        Err(FrameFault::Format { found: 2 })
    );
    assert_eq!(
        Msg::decode(b"AOSJOX\0\x01\x01", Sender::Worker),
        Err(FrameFault::Magic)
    );
    let mut proof = coordinator_msgs[0].encode();
    let kind_at = proof.len() - 13;
    if let Some(b) = proof.get_mut(kind_at) {
        *b = 2;
    }
    assert_eq!(
        Msg::decode(&proof, Sender::Coordinator),
        Err(FrameFault::Kind { kind: 2 })
    );
}

/// A job's id digests the execution identity, the kind and the plan; a claim's digest leaves
/// out the worker's declared figures.
#[test]
fn ids_and_digests_name_what_they_cover() {
    let ev = evaluator();
    let id = identity(ev);
    let exec = id.digest();
    let mut other = id.clone();
    other.toolchain.push('x');
    assert_ne!(
        job_id(&exec, KIND_TRAIN, gap0()),
        job_id(&other.digest(), KIND_TRAIN, gap0())
    );
    assert_ne!(
        job_id(&exec, KIND_TRAIN, gap0()),
        job_id(&exec, KIND_TRAIN, SHIPPED_PLAN)
    );
    assert_ne!(job_id(&exec, KIND_TRAIN, gap0()), job_id(&exec, 2, gap0()));
    let c = RawClaim::from(honest(ev, gap0()));
    let job = job_id(&exec, KIND_TRAIN, gap0());
    assert_eq!(claim_digest(&job, &c), claim_digest(&job, &c.clone()));
    assert_ne!(claim_digest(&job, &c), claim_digest(&Digest([0; 32]), &c));
}

/// A simulated pool: the coordinator, and an honest worker's claim for each job.
struct Pool {
    c: Coordinator,
    now: Nanos,
    sent: Vec<(WorkerId, Msg)>,
    killed: Vec<WorkerId>,
}

impl Pool {
    fn new(workers: u32, limits: Limits) -> Pool {
        let ev = evaluator();
        let mut pool = Pool {
            c: Coordinator::new(identity(ev), limits, ev.policy().items()),
            now: 0,
            sent: Vec::new(),
            killed: Vec::new(),
        };
        for _ in 0..workers {
            pool.c.join(0).expect("clock").expect("room");
        }
        for w in 0..workers {
            pool.hello(WorkerId(w), identity(ev));
        }
        pool
    }

    fn act(&mut self, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::Send(w, bytes) => {
                    let m =
                        Msg::decode(&bytes, Sender::Coordinator).expect("the coordinator's own");
                    self.sent.push((w, m));
                }
                Action::Kill(w) => self.killed.push(w),
            }
        }
    }

    fn hello(&mut self, w: WorkerId, identity: ExecIdentity) {
        let bytes = Msg::Hello {
            identity: Box::new(identity),
            pid: 100 + w.0,
            evaluator_ns: 1,
        }
        .encode();
        self.frame(w, bytes);
    }

    fn frame(&mut self, w: WorkerId, bytes: Vec<u8>) {
        self.event(w, Event::Frame(bytes));
    }

    fn event(&mut self, w: WorkerId, event: Event) {
        self.now += 1;
        let a = self.c.on(w, event, self.now).expect("clock");
        self.act(a);
    }

    fn begin(&mut self, jobs: &[super::m4::Job]) {
        self.now += 1;
        let a = self.c.begin(jobs, self.now).expect("clock");
        self.act(a);
    }

    /// The leases sent and not yet answered.
    fn leases(&mut self) -> Vec<(WorkerId, Token, Digest, SchedulePlan)> {
        let out = self
            .sent
            .iter()
            .filter_map(|(w, m)| match m {
                Msg::Lease {
                    token, job, plan, ..
                } => Some((
                    *w,
                    *token,
                    *job,
                    SchedulePlan::decode(plan).expect("a plan"),
                )),
                _ => None,
            })
            .collect();
        self.sent.retain(|(_, m)| !matches!(m, Msg::Lease { .. }));
        out
    }

    /// Every lease answered honestly until none is left, `alter` choosing a lie by plan.
    fn run(&mut self, alter: &dyn Fn(SchedulePlan, RawClaim) -> RawClaim) {
        loop {
            let leases = self.leases();
            if leases.is_empty() {
                break;
            }
            for (w, token, job, p) in leases {
                let claim = alter(p, RawClaim::from(honest(evaluator(), p)));
                let bytes = Msg::Result {
                    token,
                    job,
                    claim,
                    stepped: 0,
                    judge_ns: 0,
                }
                .encode();
                self.frame(w, bytes);
            }
        }
    }
}

/// A distributed search of `text` through a simulated pool of `workers`, each claim passed
/// through `alter`, then step 5.
fn pooled(
    text: &str,
    workers: u32,
    alter: &dyn Fn(SchedulePlan, RawClaim) -> RawClaim,
) -> (Pool, Result<Selection, DistNoOutcome>) {
    let ev = evaluator();
    let mut pool = Pool::new(workers, LIMITS_V1);
    let mut file = FileProposer::from_text(text);
    let mut d = ev.distributed(&mut file, CheckLevel::Dominance);
    pool.begin(d.jobs());
    pool.run(alter);
    if pool.c.ledger().outcome() != Outcome::Complete {
        return (pool, Err(DistNoOutcome::Unclaimed { slot: u32::MAX }));
    }
    for (slot, claim) in pool.c.take_claims() {
        d.claim(slot, claim).expect("an open slot");
    }
    let got = d.finish(&Serial);
    (pool, got)
}

/// Honest workers through the coordinator give the local selection.
#[test]
fn an_honest_pool_gives_the_local_selection() {
    let ev = evaluator();
    let text = plans_text(&six());
    let local = ev
        .select(&mut FileProposer::from_text(&text))
        .expect("a winner");
    let (pool, got) = pooled(&text, 3, &|_, c| c);
    same_selection(&got.expect("a winner"), &local);
    assert_eq!(pool.c.refusal(), None);
    assert_eq!(pool.c.ledger().counts().leases, 6);
}

/// A lie the shape cannot see survives the pool and is contradicted at step 5; one the shape
/// sees voids at once.
#[test]
fn lies_are_contradicted_or_refused_by_shape() {
    let ev = evaluator();
    let pins = ev.policy().items().to_vec();
    let text = plans_text(&six());
    let refuse_winner = |p: SchedulePlan, c: RawClaim| {
        if p != gap0() {
            return c;
        }
        c.altered(ClaimKind::Refused, |rows| {
            rows.truncate(1);
            if let Some(r) = rows.first_mut() {
                r.stranded_light = RowVerdict::fail("HotHasWake", 0);
            }
        })
    };
    let (_, got) = pooled(&text, 2, &refuse_winner);
    assert!(
        matches!(got, Err(DistNoOutcome::Contradicted { slot: 0, .. })),
        "{got:?}"
    );
    let short = |p: SchedulePlan, c: RawClaim| {
        if p != gap0() {
            return c;
        }
        c.altered(ClaimKind::Admitted, |rows| {
            rows.pop();
        })
    };
    let (pool, _) = pooled(&text, 2, &short);
    assert!(
        matches!(
            pool.c.refusal(),
            Some(Refusal::Shape {
                fault: ShapeFault::Count { rows: 16, .. },
                ..
            })
        ),
        "{:?}",
        pool.c.refusal()
    );
    assert!(matches!(pool.c.ledger().outcome(), Outcome::Void(_)));
    let _ = pins;
}

/// Garbage, a result naming another job id, and a foreign hello: the first two void, killing
/// their sender; the third refuses that worker alone, by the field it differs in.
#[test]
fn garbage_another_job_id_and_a_foreign_hello() {
    let ev = evaluator();
    let text = plans_text(&six());
    let mut file = FileProposer::from_text(&text);
    let d = ev.distributed(&mut file, CheckLevel::Dominance);
    let mut pool = Pool::new(2, LIMITS_V1);
    pool.begin(d.jobs());
    pool.frame(WorkerId(0), b"not a frame".to_vec());
    assert_eq!(
        pool.c.refusal(),
        Some(&Refusal::Frame {
            worker: WorkerId(0),
            fault: FrameFault::Magic
        })
    );
    assert!(pool.killed.contains(&WorkerId(0)));
    let cancels = pool
        .sent
        .iter()
        .filter(|(w, m)| *w == WorkerId(1) && matches!(m, Msg::Cancel { .. }))
        .count();
    assert_eq!(cancels, 1, "the other holder is sent Cancel");

    let mut pool = Pool::new(1, LIMITS_V1);
    pool.begin(d.jobs());
    let (w, token, _, p) = pool.leases().into_iter().next().expect("a lease");
    let bytes = Msg::Result {
        token,
        job: Digest([9; 32]),
        claim: RawClaim::from(honest(ev, p)),
        stepped: 0,
        judge_ns: 0,
    }
    .encode();
    pool.frame(w, bytes);
    assert_eq!(
        pool.c.refusal(),
        Some(&Refusal::JobId {
            worker: w,
            sent: token
        })
    );
    assert!(pool.killed.contains(&w), "the liar is killed");
    assert!(
        !pool
            .sent
            .iter()
            .any(|(x, m)| *x == w && matches!(m, Msg::Cancel { .. })),
        "and never drained"
    );

    let fields = [
        "policy",
        "stranded_light",
        "lane_fidelity",
        "grade",
        "tested_assumptions",
        "plan_assumptions",
        "corpus",
        "build",
        "toolchain",
        "profile",
        "build_profile",
        "schema",
        "base",
        "interpreter",
        "lane",
        "oracle",
        "executable",
        "faults",
    ];
    let base = identity(ev);
    for field in fields {
        let mut other = base.clone();
        match field {
            "policy" => other.policy.version += 1,
            "stranded_light" => other.stranded_light.version += 1,
            "lane_fidelity" => other.lane_fidelity.version += 1,
            "grade" => other.grade.push('x'),
            "tested_assumptions" => other.tested_assumptions.pop().map_or((), |_| ()),
            "plan_assumptions" => other.plan_assumptions.pop().map_or((), |_| ()),
            "corpus" => other.corpus = Digest([1; 32]),
            "build" => other.build.push_str("-other"),
            "toolchain" => other.toolchain.push('x'),
            "profile" => other.profile.version += 1,
            "build_profile" => other.build_profile.push('x'),
            "schema" => other.schema.version += 1,
            "base" => other.base.version += 1,
            "interpreter" => other.interpreter.push('x'),
            "lane" => other.lane.push('x'),
            "oracle" => other.oracle.push('x'),
            "executable" => other.executable = Digest([4; 32]),
            _ => other.faults.push("kill@0.1:3".to_owned()),
        }
        assert_eq!(base.first_difference(&other), Some(field));
        let mut c = Coordinator::new(base.clone(), LIMITS_V1, ev.policy().items());
        let w = c.join(0).expect("clock").expect("room");
        let hello = Msg::Hello {
            identity: Box::new(other),
            pid: 1,
            evaluator_ns: 1,
        };
        let a = c.on(w, Event::Frame(hello.encode()), 1).expect("clock");
        assert_eq!(a, [Action::Kill(w)]);
        assert_eq!(c.foreign(), [(w, field)]);
        assert_eq!(c.refusal(), None, "a foreign hello voids nothing");
    }
}

/// A resend equal to its accepted claim is counted; a frame from a killed worker is dropped
/// unread; a resend that differs voids.
#[test]
fn resends_and_frames_from_the_gone() {
    let ev = evaluator();
    let text = plans_text(&[gap0(), SHIPPED_PLAN]);
    let mut file = FileProposer::from_text(&text);
    let d = ev.distributed(&mut file, CheckLevel::Dominance);
    let mut pool = Pool::new(1, LIMITS_V1);
    pool.begin(d.jobs());
    let (w, token, job, p) = pool.leases().into_iter().next().expect("a lease");
    let result = |claim: RawClaim| {
        Msg::Result {
            token,
            job,
            claim,
            stepped: 0,
            judge_ns: 0,
        }
        .encode()
    };
    let honest_claim = RawClaim::from(honest(ev, p));
    pool.frame(w, result(honest_claim.clone()));
    pool.frame(w, result(honest_claim.clone()));
    assert_eq!(pool.c.ledger().counts().duplicate, 1);
    assert_eq!(pool.c.refusal(), None);
    let lie = honest_claim.altered(ClaimKind::Admitted, |rows| {
        if let Some(r) = rows.first_mut() {
            r.ticks += 1;
            r.presented += 1;
        }
    });
    let mut pool2 = Pool::new(1, LIMITS_V1);
    pool2.begin(d.jobs());
    let (w2, token2, job2, p2) = pool2.leases().into_iter().next().expect("a lease");
    let send = |claim: RawClaim| {
        Msg::Result {
            token: token2,
            job: job2,
            claim,
            stepped: 0,
            judge_ns: 0,
        }
        .encode()
    };
    pool2.frame(w2, send(RawClaim::from(honest(ev, p2))));
    pool2.frame(w2, send(lie));
    assert_eq!(
        pool2.c.refusal(),
        Some(&Refusal::Lease(LeaseRefusal::Differs {
            search: 1,
            slot: 0
        }))
    );
    assert!(pool2.killed.contains(&w2));
    let before = pool2.c.ledger().counts().dropped;
    pool2.frame(w2, b"anything".to_vec());
    assert_eq!(
        pool2.c.ledger().counts().dropped,
        before + 1,
        "dropped unread"
    );
}

/// Under every fault the coordinator meets, the held-out split is read at most once: a void
/// search reaches no selection, and a complete one is read once by the gate.
#[test]
fn the_held_out_split_is_read_once_under_every_fault() {
    let text = plans_text(&six());
    let lies: [&dyn Fn(SchedulePlan, RawClaim) -> RawClaim; 3] = [
        &|_, c| c,
        &|p, c| {
            if p == SHIPPED_PLAN {
                c.altered(ClaimKind::Admitted, |rows| {
                    for r in rows {
                        r.ticks = r.ticks.saturating_sub(500);
                        r.presented = r.presented.min(r.ticks);
                        r.wasted = r.ticks - r.presented;
                    }
                })
            } else {
                c
            }
        },
        &|p, c| {
            if p == gap0() {
                c.altered(ClaimKind::Admitted, |rows| {
                    for r in rows {
                        r.ticks += 1_000;
                        r.presented += 1_000;
                    }
                })
            } else {
                c
            }
        },
    ];
    for (i, lie) in lies.iter().enumerate() {
        let ev = fresh();
        let mut pool = Pool::new(3, LIMITS_V1);
        let mut file = FileProposer::from_text(&text);
        let mut d = ev.distributed(&mut file, CheckLevel::Dominance);
        pool.begin(d.jobs());
        pool.run(lie);
        for (slot, claim) in pool.c.take_claims() {
            d.claim(slot, claim).expect("an open slot");
        }
        match d.finish(&Serial) {
            Ok(s) => {
                assert_eq!(i, 0, "only the honest pool selects");
                ev.held_out(s).expect("useful");
                assert_eq!((n(&ev.reads().looks), n(&ev.reads().held_out)), (1, 9));
            }
            Err(e) => {
                assert!(matches!(e, DistNoOutcome::Contradicted { .. }), "{e:?}");
                assert_eq!((n(&ev.reads().looks), n(&ev.reads().held_out)), (0, 0));
            }
        }
    }
}

/// A reader's transport fault (an oversized header, a bad CRC, a cut frame) voids as a frame
/// fault of its sender, which is killed and never drained; the other holders are cancelled.
#[test]
fn a_broken_stream_voids_and_kills_its_sender() {
    let ev = evaluator();
    let text = plans_text(&six());
    let mut file = FileProposer::from_text(&text);
    let d = ev.distributed(&mut file, CheckLevel::Dominance);
    for fault in [
        FrameFault::Oversize { len: 70_000 },
        FrameFault::Corrupt {
            detail: "frame payload CRC mismatch".to_owned(),
        },
    ] {
        let mut pool = Pool::new(2, LIMITS_V1);
        pool.begin(d.jobs());
        pool.event(WorkerId(0), Event::Broken(fault.clone()));
        assert_eq!(
            pool.c.refusal(),
            Some(&Refusal::Frame {
                worker: WorkerId(0),
                fault
            })
        );
        assert!(pool.killed.contains(&WorkerId(0)));
        let cancelled: Vec<WorkerId> = pool
            .sent
            .iter()
            .filter(|(_, m)| matches!(m, Msg::Cancel { .. }))
            .map(|(w, _)| *w)
            .collect();
        assert_eq!(cancelled, vec![WorkerId(1)], "the sender is not drained");
    }
}

/// A cut frame from a worker already gone (one the coordinator killed mid-write) is dropped
/// unread: it neither voids nor moves the search.
#[test]
fn a_broken_tail_from_a_gone_worker_is_dropped() {
    let ev = evaluator();
    let text = plans_text(&six());
    let mut file = FileProposer::from_text(&text);
    let d = ev.distributed(&mut file, CheckLevel::Dominance);
    let mut pool = Pool::new(2, LIMITS_V1);
    pool.begin(d.jobs());
    pool.event(WorkerId(0), Event::Eof);
    assert!(matches!(
        pool.c.ledger().worker(WorkerId(0)),
        Some(WorkerState::Gone(_))
    ));
    let before = pool.c.ledger().counts().dropped;
    pool.event(
        WorkerId(0),
        Event::Broken(FrameFault::Corrupt {
            detail: "the stream ended inside a frame".to_owned(),
        }),
    );
    assert_eq!(pool.c.refusal(), None);
    assert_eq!(pool.c.ledger().outcome(), Outcome::Open);
    assert_eq!(pool.c.ledger().counts().dropped, before + 1);
}

/// A claim's rows stay out of m4 only while `Claim`'s and `RawClaim`'s fields are private: E0616
/// refuses a read of a private field outside `dist.rs`, not of one made `pub(super)`, so the
/// declarations themselves are pinned.
#[test]
fn a_claims_rows_stay_private_to_dist() {
    let src = include_str!("dist.rs");
    for name in ["Claim", "RawClaim"] {
        let decl = format!("pub struct {name} {{\n    kind: ClaimKind,\n    rows: Vec<Row>,\n}}");
        assert_eq!(src.matches(&decl).count(), 1, "{name}'s fields are private");
    }
    for accessor in ["pub fn rows(", "pub(super) fn rows(", "pub(crate) fn rows("] {
        assert!(
            !src.contains(accessor),
            "no accessor hands rows out: {accessor}"
        );
    }
}
