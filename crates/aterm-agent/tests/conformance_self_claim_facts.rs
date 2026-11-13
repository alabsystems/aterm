// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of `SelfClaimFact` (`aterm_models_harness::self_claim_fact_model`) to a
//! proven self-claim's facts account (gap 23): each reader's read on [`SelfClaimFacts::pick`] and
//! [`SelfClaimFacts::offer`], each ack on [`SelfClaimFacts::ack`], and each episode's close on
//! [`SelfClaimFacts::observe`]. The model's two episodes are two calls at no progress (the first
//! quiet longer, so the reading names it first), its two readers two root threads of one claim;
//! the call going quiet, an act going without its fact, its turn starting, its ack lost and a lease
//! running out are the environment. The walk's clock stands still but at two steps: a read that
//! leases an episode while the other holds a live lease moves it 1 ms past that lease's offer, so
//! the leases' offers keep the model's order (`older`), and a lapse moves it to the lapsing lease's
//! end, which an older lease reaches while every younger one stands. At
//! every state the account is probed on copies: what each reader would be offered now, and whether
//! an ack of each episode would tell it.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use aterm_agent::supervise::BackgroundCall;
use aterm_agent::supervise::FactsReading;
use aterm_agent::supervise::facts::EpisodeKey;
use aterm_agent::supervise::facts_offer::{
    OFFER_MS, Pick, SelfClaimFacts, WHY_NONE, WHY_OFFERED, WHY_TOLD,
};
use aterm_agent::supervise::occupancy::fault::{self, Fault};
use aterm_models_harness::self_claim_fact_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_spec::xref::{self, StepEvidence};

type State = BTreeMap<&'static str, i64>;

const T0: i64 = 1_760_000_000_000;

/// The one claim's proof.
const CLAIM: &str = "codex-alab@4242 started Mon";

/// The model's two readers (`R1`, `R2`).
const READERS: [&str; 2] = ["thread-1", "thread-2"];

/// The model's mutants, which no real step mirrors.
const MUTANTS: [&str; 3] = ["R1AcksEarly", "R2ReadsLeased1", "SlotSteal"];

/// The steps the code performs, each on its anchored function.
const ANCHORED: [&str; 8] = [
    "Close1", "Close2", "R1Acks", "R1Reads1", "R1Reads2", "R2Acks", "R2Reads1", "R2Reads2",
];

/// The call's, the act's and the clock's steps, which the account only sees.
const ENVIRONMENT: [&str; 10] = [
    "Lapse1",
    "Lapse2",
    "Open1",
    "Open2",
    "R1Drops",
    "R1LosesAck",
    "R1Starts",
    "R2Drops",
    "R2LosesAck",
    "R2Starts",
];

/// Episode `e`'s call (1 or 2) in its run `run` (the engine's episode, which each `Open` moves
/// on): the first quiet longer, each its own group and birth.
fn call(e: usize, run: u32) -> BackgroundCall {
    let pid = 700 + u32::try_from(e).expect("an episode");
    BackgroundCall {
        pid,
        pgid: pid,
        birth: Some(u64::from(pid) * 10),
        episode: run,
        task: None,
        quiet_ms: if e == 1 { 9 } else { 5 } * 60_000,
        wait: None,
        held_by: None,
    }
}

/// Episode `e`'s key in `r`'s run of it.
fn key(e: usize, r: &Real) -> EpisodeKey {
    EpisodeKey::of_call(&call(e, r.runs[e - 1])).expect("a call with a birth")
}

/// The reading at `now` naming the episodes open in `st`, the first first, each in `r`'s run.
fn reading(st: &State, r: &Real, now: i64) -> FactsReading {
    FactsReading {
        waiting: 0,
        as_of_ms: now,
        no_progress: [1, 2]
            .into_iter()
            .filter(|e| st[open(*e)] == 1)
            .map(|e| call(e, r.runs[e - 1]))
            .collect(),
        held: None,
        left: None,
    }
}

fn open(e: usize) -> &'static str {
    ["open1", "open2"][e - 1]
}

fn told(e: usize) -> &'static str {
    ["told1", "told2"][e - 1]
}

fn offer(e: usize) -> &'static str {
    ["offer1", "offer2"][e - 1]
}

fn lapsed(e: usize) -> &'static str {
    ["lapsed1", "lapsed2"][e - 1]
}

fn hand(r: usize) -> &'static str {
    ["hand1", "hand2"][r - 1]
}

/// The real side of a model state, along the first path the walk took to it.
#[derive(Clone)]
struct Real {
    /// The clock, unix ms.
    now: i64,
    account: SelfClaimFacts<&'static str>,
    /// When each episode was last offered (the walk's own note, for `Lapse`).
    at: [i64; 2],
    /// Each call's run (the engine's episode number): an `Open` is a new run of no progress.
    runs: [u32; 2],
}

impl Real {
    fn new() -> Self {
        Self {
            now: T0,
            account: SelfClaimFacts::default(),
            at: [T0; 2],
            runs: [1; 2],
        }
    }
}

/// The model's view of the account: for each episode, 1 where an ack of it would tell it now (an
/// offer of it stands, untold), else 0, probed on a copy.
fn project(r: &Real) -> State {
    let mut out = State::new();
    for e in [1, 2] {
        let tells = r.account.clone().ack(key(e, r).id, &CLAIM, r.now).is_some();
        out.insert(offer(e), i64::from(tells));
    }
    out
}

/// Whether reader `r` may be offered episode `e` at `st`: open, untold, its offer empty, the
/// reader's own, or lapsed.
fn eligible(st: &State, r: usize, e: usize) -> bool {
    let lease = st[offer(e)];
    st[open(e)] == 1
        && st[told(e)] == 0
        && (lease == 0 || lease == i64::try_from(r).expect("a reader") || st[lapsed(e)] == 1)
}

/// What the model offers reader `r` at `st`: the first eligible episode, else why none.
fn offered(st: &State, r: usize) -> Result<usize, &'static str> {
    let named: Vec<usize> = [1, 2].into_iter().filter(|e| st[open(*e)] == 1).collect();
    if named.is_empty() {
        return Err(WHY_NONE);
    }
    if named.iter().all(|e| st[told(*e)] == 1) {
        return Err(WHY_TOLD);
    }
    named
        .into_iter()
        .find(|e| eligible(st, r, *e))
        .ok_or(WHY_OFFERED)
}

/// The account agrees with the model at `st`: what each reader would be offered, and whether
/// an ack of each episode would tell it.
fn agrees(st: &State, r: &Real) -> bool {
    let read = reading(st, r, r.now);
    let picks = [1, 2].into_iter().all(|reader| {
        let pick = r
            .account
            .pick(Some(&read), &CLAIM, READERS[reader - 1], r.now);
        match (offered(st, reader), pick) {
            (Ok(e), Pick::Fact(fact)) => fact.key == key(e, r),
            (Err(why), Pick::Why(got)) => why == got,
            _ => false,
        }
    });
    let real = project(r);
    picks
        && [1, 2].into_iter().all(|e| {
            let tells = st[offer(e)] > 0 && st[told(e)] == 0;
            real[offer(e)] == i64::from(tells)
        })
}

/// The evidence windows of an audited walk, or none.
struct Ev(Option<StepEvidence>);

impl Ev {
    fn step<T>(&mut self, action: &'static str, drive: impl FnOnce() -> T) -> T {
        match &mut self.0 {
            Some(ev) => ev.step(action, drive),
            None => drive(),
        }
    }
}

/// The real step for `action` at `st`; false where the code answers otherwise than the step
/// says. A step's own calls run in its evidence window, and nothing else does.
fn mirror(action: &'static str, st: &State, r: &mut Real, ev: &mut Ev) -> bool {
    let now = r.now;
    match action {
        "Open1" | "Open2" => {
            let e = if action == "Open1" { 1 } else { 2 };
            r.runs[e - 1] += 1;
            true
        }
        "R1Drops" | "R2Drops" | "R1Starts" | "R2Starts" | "R1LosesAck" | "R2LosesAck" => true,
        "Lapse1" | "Lapse2" => {
            let e = if action == "Lapse1" { 1 } else { 2 };
            r.now = r.now.max(r.at[e - 1] + OFFER_MS);
            true
        }
        "Close1" | "Close2" => {
            let e = if action == "Close1" { 1 } else { 2 };
            let mut after = st.clone();
            after.insert(open(e), 0);
            let read = reading(&after, r, now);
            ev.step(action, || r.account.observe(Some(&read), now));
            true
        }
        "R1Reads1" | "R1Reads2" | "R2Reads1" | "R2Reads2" => {
            let (reader, e) = match action {
                "R1Reads1" => (1, 1),
                "R1Reads2" => (1, 2),
                "R2Reads1" => (2, 1),
                _ => (2, 2),
            };
            // A lease taken while the other episode's stands live is the younger: offered after it.
            let other = 3 - e;
            if st[offer(other)] > 0 && st[lapsed(other)] == 0 {
                r.now = r.now.max(r.at[other - 1] + 1);
            }
            let now = r.now;
            let read = reading(st, r, now);
            // The verb's observe at the read: the account forgets nothing the model keeps open.
            r.account.observe(Some(&read), now);
            let by = READERS[reader - 1];
            let account = &mut r.account;
            let pick = ev.step(action, || {
                let pick = account.pick(Some(&read), &CLAIM, by, now);
                if let Pick::Fact(fact) = &pick {
                    account.offer(fact, CLAIM, by, now);
                }
                pick
            });
            r.at[e - 1] = now;
            matches!(pick, Pick::Fact(fact) if fact.key == key(e, r))
        }
        "R1Acks" | "R2Acks" => {
            let reader = if action == "R1Acks" { 1 } else { 2 };
            let e = usize::try_from(st[hand(reader)]).expect("a hand");
            if e == 0 {
                return false;
            }
            let id = key(e, r).id;
            let account = &mut r.account;
            let answer = ev.step(action, || account.ack(id, &CLAIM, now));
            answer.is_some() == (st[offer(e)] > 0 && st[told(e)] == 0)
        }
        other => panic!("no real step for `{other}`"),
    }
}

struct Walked {
    states: usize,
    off: Vec<State>,
    fired: BTreeSet<&'static str>,
}

fn walk(m: &Model, strict: bool, fault: Option<Fault>, ev: &mut Ev) -> Walked {
    let mut go = || {
        let mut w = Walked {
            states: 0,
            off: Vec::new(),
            fired: BTreeSet::new(),
        };
        let init = Real::new();
        assert!(
            agrees(&m.init_state(), &init),
            "the empty account is the model's start"
        );
        let mut seen: HashSet<Vec<i64>> = HashSet::new();
        let mut queue = VecDeque::from([(m.init_state(), init)]);
        while let Some((st, real)) = queue.pop_front() {
            if !seen.insert(crate::common::key(m, &st)) {
                continue;
            }
            let env = m.eval_env(&st);
            for action in &m.actions {
                let Some(next) = m.successors_in(action, &env, &st).into_iter().next() else {
                    continue;
                };
                let mut r = real.clone();
                let stepped = mirror(action.name, &st, &mut r, ev);
                w.fired.insert(action.name);
                if stepped && agrees(&next, &r) {
                    queue.push_back((next, r));
                    continue;
                }
                assert!(
                    !strict,
                    "the account disagrees after {} from {st:?}: the model's {next:?}, the \
                     account's {:?}",
                    action.name,
                    project(&r),
                );
                w.off.push(next);
            }
        }
        w.states = seen.len();
        w
    };
    match fault {
        Some(f) => fault::with(f, go),
        None => go(),
    }
}

#[test]
fn tier1_the_account_offers_and_tells_exactly_where_the_model_does() {
    let m = self_claim_fact_model();
    let w = walk(&m, true, None, &mut Ev(None));
    let want: BTreeSet<&str> = ANCHORED.into_iter().chain(ENVIRONMENT).collect();
    assert_eq!(w.fired, want, "the steps mirrored");
    assert_eq!(
        w.states,
        interp::reachable_states(&m).len(),
        "every state the model reaches is walked"
    );
}

/// NEGATIVE CONTROL: an account of one offer slot (the first draft's, `SlotSteal`): a second
/// reader's offer replaces the first's, so an ack of the first episode no longer tells it and the
/// first reader is offered it again; the account disagrees exactly where both offers stand.
#[test]
fn tier1_one_offer_slot_disagrees_where_both_offers_stand() {
    let w = walk(
        &self_claim_fact_model(),
        false,
        Some(Fault::OneOfferSlot),
        &mut Ev(None),
    );
    assert!(!w.off.is_empty(), "one offer slot agreed everywhere");
    assert!(
        w.off.iter().all(|st| st["offer1"] > 0 && st["offer2"] > 0),
        "{:?}",
        w.off
    );
}

/// NEGATIVE CONTROL: an account that ages a lease its reader takes again before it lapses from that
/// reader's first offer: once the other episode's offer, older than the retake but younger than
/// the first, lapses, the episode taken again lapses with it, so the other reader is offered it
/// while the model's lease stands. Only the lease order finds it: a model in which no lease lapses
/// beside a live one agrees with it everywhere. The account disagrees only where a lease the
/// model keeps live stands.
#[test]
fn tier1_a_lease_aged_from_its_first_offer_disagrees_where_a_retake_outlives_the_other() {
    let w = walk(
        &self_claim_fact_model(),
        false,
        Some(Fault::LeaseFromFirstOffer),
        &mut Ev(None),
    );
    assert!(
        !w.off.is_empty(),
        "a lease aged from its first offer agreed everywhere"
    );
    assert!(
        w.off.iter().all(|st| [1, 2]
            .into_iter()
            .any(|e| st[offer(e)] > 0 && st[lapsed(e)] == 0)),
        "{:?}",
        w.off
    );
}

#[test]
fn tier1_every_anchored_step_enters_its_own_function() {
    let m = self_claim_fact_model();
    let mut ev = Ev(Some(StepEvidence::new("SelfClaimFact")));
    let w = walk(&m, true, None, &mut ev);
    assert_eq!(w.states, interp::reachable_states(&m).len());
    let audit = ev.0.expect("the audited walk's evidence").audit();
    assert_eq!(
        audit.actions,
        ANCHORED.into_iter().collect::<BTreeSet<_>>(),
        "{audit:?}"
    );
    assert!(audit.uninstrumented.is_empty(), "{audit:?}");
    assert!(audit.ambiguous.is_empty(), "{audit:?}");
    assert!(audit.unanchored_steps.is_empty(), "{audit:?}");
    assert!(audit.undriven.is_empty(), "{audit:?}");
    assert!(audit.unwitnessed.is_empty(), "{audit:?}");
    assert!(audit.blind_steps.is_empty(), "{audit:?}");
    assert!(audit.stray.is_empty(), "{audit:?}");
    assert!(audit.dark_sites.is_empty(), "{audit:?}");
}

#[test]
fn tier1_the_anchors_name_what_the_code_implements() {
    let anchored: BTreeSet<&str> = xref::refinements()
        .filter(|r| r.machine == "SelfClaimFact")
        .map(|r| r.action)
        .collect();
    assert_eq!(anchored, ANCHORED.into_iter().collect(), "{anchored:?}");
    let waived: BTreeSet<&str> = xref::waivers()
        .filter(|w| w.machine == "SelfClaimFact")
        .map(|w| w.action)
        .collect();
    let want: BTreeSet<&str> = ENVIRONMENT.into_iter().chain(MUTANTS).collect();
    assert_eq!(waived, want, "{waived:?}");
    let actions: BTreeSet<&str> = self_claim_fact_model()
        .actions
        .iter()
        .map(|a| a.name)
        .collect();
    let bound: BTreeSet<&str> = anchored.union(&waived).copied().collect();
    assert_eq!(bound, actions, "every action anchored or waived");
}
