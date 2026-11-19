// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LoginEvidence Tier-1 conformance: `Ledger::observe` and `Ledger::verdict`, driven from every
//! state the model reaches, each real step one of the model's, every law kept; and the negative
//! control, the sticky word restored through its seam, which the healthy model refuses and its
//! StickyWord mutant performs exactly.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_models_harness::{LOGIN_EVIDENCE_LAWS, login_evidence_model};
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_spec::xref::StepEvidence;

use super::*;
use crate::obs::Source;

const MACHINE: &str = "LoginEvidence";

/// The model's actions: `Observe` is the ledger's, the rest compose the next reading.
const ACTIONS: [&str; 5] = ["Observe", "Later", "Struck", "Foreign", "Again"];

/// A login reading, ordered as the model orders values: struck over serving at one date.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Login {
    Serves,
    Struck,
}

/// The model's date `d` (1 to 3) is a minute apart from the next, from 2026-11-13 09:00 UTC.
fn date(d: i64) -> u64 {
    1_794_646_800 + 60 * u64::try_from(d).expect("a model date is positive")
}

fn model_date(at: u64) -> i64 {
    i64::try_from((at - 1_794_646_800) / 60).expect("a fixture date")
}

const fn login(k: i64) -> Login {
    if k == 2 { Login::Struck } else { Login::Serves }
}

const fn model_value(value: Login) -> i64 {
    match value {
        Login::Serves => 1,
        Login::Struck => 2,
    }
}

/// The print current now.
fn current() -> CredentialPrint {
    CredentialPrint::new("claude", "m16", 1)
}

/// Another print: the same row's next generation.
fn another() -> CredentialPrint {
    CredentialPrint::new("claude", "m16", 2)
}

/// The current print's `n`th witness.
fn witness(n: i64) -> WitnessId {
    WitnessId::Reading(format!("w{n}"))
}

/// The real ledger, the next reading as the environment composed it, and what the last
/// `Observe` read.
#[derive(Clone)]
struct World {
    ledger: Ledger<Login>,
    /// Readings of another print so far, each its own witness.
    foreign: u32,
    oa: i64,
    ok: i64,
    os: i64,
    pa: i64,
    pk: i64,
    last: i64,
    nw: i64,
}

impl World {
    fn new() -> Self {
        Self {
            ledger: Ledger::new(),
            foreign: 0,
            oa: 1,
            ok: 1,
            os: 1,
            pa: 0,
            pk: 0,
            last: 0,
            nw: 0,
        }
    }
}

/// The verdict for the current print as the model's `(va, vk)`.
fn verdict_of(ledger: &Ledger<Login>) -> (i64, i64) {
    ledger
        .verdict(&current())
        .map_or((0, 0), |obs| (model_date(obs.at), model_value(obs.value)))
}

/// The current print's `n`th witness's first reading as the model's `(a, k)`.
fn slot(ledger: &Ledger<Login>, n: i64) -> (i64, i64) {
    let subject = Subject::Credential(current());
    ledger
        .readings_of(&subject)
        .find(|obs| obs.witness == witness(n))
        .map_or((0, 0), |obs| (model_date(obs.at), model_value(obs.value)))
}

/// PROJECTION (`aterm_evidence::ledger::conformance::project`, named by the anchors on
/// `Ledger::observe` and `Ledger::verdict`): the current print's two witnesses and verdict, read
/// off the real ledger, beside the driver's next reading and what its last `Observe` read.
fn project(world: &World) -> State {
    let (a1, k1) = slot(&world.ledger, 1);
    let (a2, k2) = slot(&world.ledger, 2);
    let (va, vk) = verdict_of(&world.ledger);
    State::from([
        ("a1", a1),
        ("k1", k1),
        ("a2", a2),
        ("k2", k2),
        ("va", va),
        ("vk", vk),
        ("pa", world.pa),
        ("pk", world.pk),
        ("last", world.last),
        ("nw", world.nw),
        ("oa", world.oa),
        ("ok", world.ok),
        ("os", world.os),
    ])
}

/// One real step of `action` from `world`; `Observe` inside its own evidence window.
fn step(world: &World, action: &str, evidence: &mut StepEvidence) -> World {
    let mut next = world.clone();
    match action {
        "Later" => next.oa += 1,
        "Struck" => next.ok = 2,
        "Foreign" => next.os = 2,
        "Again" => next.os = 3,
        "Observe" => {
            let (a1, k1) = slot(&world.ledger, 1);
            let (a2, k2) = slot(&world.ledger, 2);
            let newer = |(a, k): (i64, i64)| world.oa > a || (world.oa == a && world.ok > k);
            // Each reading carries the composed date and value, a re-read (`os == 3`) included:
            // `Struck` then `Again` re-reads the first witness with a value its first reading
            // may not have had, so a re-read that replaced a stored value is a refused step.
            let (witness, value, subject) = match world.os {
                1 => (
                    witness(if a1 == 0 { 1 } else { 2 }),
                    login(world.ok),
                    current(),
                ),
                2 => {
                    next.foreign += 1;
                    (
                        WitnessId::Reading(format!("foreign-{}", next.foreign)),
                        login(world.ok),
                        another(),
                    )
                }
                _ => (witness(1), login(world.ok), current()),
            };
            let obs = Obs {
                value,
                at: date(world.oa),
                witness,
                source: Source::Reading,
                subject: Subject::Credential(subject),
            };
            (next.pa, next.pk) = verdict_of(&world.ledger);
            next.last = world.os;
            next.nw = i64::from(world.os == 1 && newer((a1, k1)) && newer((a2, k2)));
            evidence.step("Observe", || {
                next.ledger.observe(obs);
                std::hint::black_box(next.ledger.verdict(&current()));
            });
            (next.oa, next.ok, next.os) = (1, 1, 1);
        }
        other => unreachable!("no action {other}"),
    }
    next
}

/// A real step the judging model does not take: from, action, the real result, the model's.
type Refusal = (State, &'static str, State, Vec<State>);

/// What one walk found.
struct Walk {
    refused: Vec<Refusal>,
    /// Real states that break a law of the judging model, each with the laws it breaks.
    lawless: Vec<(State, Vec<&'static str>)>,
    reached: BTreeSet<State>,
    evidence: StepEvidence,
}

/// Every real state reachable from a fresh ledger by the steps `judge` enables, each step judged
/// against `judge`'s successors; the walk follows the real result whether or not it is taken.
fn walk(judge: &Model) -> Walk {
    let mut evidence = StepEvidence::new(MACHINE);
    let fresh = World::new();
    let init = project(&fresh);
    assert_eq!(init, judge.init_state(), "a fresh ledger is Init");
    let mut worlds = BTreeMap::from([(init.clone(), fresh)]);
    let mut queue = VecDeque::from([init]);
    let (mut refused, mut lawless) = (Vec::new(), Vec::new());
    while let Some(state) = queue.pop_front() {
        let world = worlds[&state].clone();
        for action in ACTIONS {
            let successors = judge.successors(action, &state);
            if successors.is_empty() {
                continue;
            }
            let next = step(&world, action, &mut evidence);
            let real = project(&next);
            if !successors.contains(&real) {
                refused.push((state.clone(), action, real.clone(), successors));
            }
            if !worlds.contains_key(&real) {
                let broken: Vec<_> = judge
                    .invariants
                    .iter()
                    .filter(|law| !judge.check_invariant(law.name, &real))
                    .map(|law| law.name)
                    .collect();
                if !broken.is_empty() {
                    lawless.push((real.clone(), broken));
                }
                worlds.insert(real.clone(), next);
                queue.push_back(real);
            }
        }
    }
    Walk {
        refused,
        lawless,
        reached: worlds.into_keys().collect(),
        evidence,
    }
}

/// Tier 1: from every state the model reaches, each real call the model allows there is the
/// model's own successor and keeps every law; the walk reaches exactly the model's states; and
/// every `Observe` step entered the anchored ledger functions.
#[test]
fn login_evidence_conforms() {
    let model = login_evidence_model();
    assert_eq!(
        model
            .consts
            .iter()
            .find(|(k, _)| *k == "Buggy")
            .map(|c| c.1),
        Some(0),
        "the committed config is the fixed machine"
    );
    let walk = walk(&model);
    assert!(
        walk.refused.is_empty(),
        "{} real step(s) the model does not take, e.g. {:?}",
        walk.refused.len(),
        walk.refused.first()
    );
    assert!(walk.lawless.is_empty(), "{:?}", walk.lawless.first());
    let reachable: BTreeSet<State> = interp::reachable_states(&model).into_iter().collect();
    assert_eq!(
        walk.reached, reachable,
        "the driver reaches every reachable model state and no other"
    );
    let audit = walk.evidence.audit();
    assert!(
        audit.anchors >= 2,
        "observe and verdict are anchored: {audit:?}"
    );
    assert!(
        audit.uninstrumented.is_empty()
            && audit.ambiguous.is_empty()
            && audit.undriven.is_empty()
            && audit.unwitnessed.is_empty()
            && audit.blind_steps.is_empty(),
        "{audit:?}"
    );
    eprintln!(
        "LoginEvidence Tier-1: {} states, every Observe step in the anchored ledger",
        reachable.len()
    );
}

/// Negative control: the sticky word restored in the real ledger is refused by the healthy model,
/// on the incident's own step (a strike, then the login serving later, still read as struck), and
/// performed step for step by the StickyWord mutant, which breaks the law the table names for it.
#[test]
fn a_restored_sticky_word_is_refused_and_is_the_sticky_word_mutant() {
    let model = login_evidence_model();
    let (law, mutant) = LOGIN_EVIDENCE_LAWS[0];
    assert_eq!(mutant, "StickyWord");
    let sticky = interp::with_buggy(&model, 1);
    restore_sticky_word(true);
    let healthy = walk(&model);
    let as_mutant = walk(&sticky);
    restore_sticky_word(false);
    assert!(
        !healthy.refused.is_empty(),
        "the healthy model refuses the sticky word"
    );
    let incident = healthy.refused.iter().any(|(from, action, real, _)| {
        *action == "Observe"
            && (
                from["a1"], from["k1"], from["vk"], from["os"], from["oa"], from["ok"],
            ) == (1, 2, 2, 1, 2, 1)
            && (real["va"], real["vk"]) == (1, 2)
    });
    assert!(
        incident,
        "a strike at 1, then serving at 2, read as struck: {:?}",
        healthy.refused.first()
    );
    assert!(
        as_mutant.refused.is_empty(),
        "the seam is the StickyWord mutant exactly: {:?}",
        as_mutant.refused.first()
    );
    assert!(
        as_mutant
            .lawless
            .iter()
            .any(|(_, broken)| broken.contains(&law)),
        "and it breaks {law}"
    );
    let fixed = walk(&model);
    assert!(fixed.refused.is_empty(), "the seam is off again");
}
