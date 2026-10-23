// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 of `KeeperCompanionCustody`: the real `KeeperCore` against the model.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::{
    CompanionId, ConnId, Holders, KeeperCore, KeeperEnv, OfferJournal, Out, Rdev, RecordState,
    fault,
};
use crate::wire::{Birth, CAP_JOURNAL, CAP_SENDS_BYE, COMPANION_JOURNAL, MasterHeader, PeerClass};
use aterm_models_core::keeper_companion_custody_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};

const R: Rdev = 0x0f00_0077;
const LEN: u64 = 8 << 20;

/// Every shell lives, nobody else holds the master, and the live windows are the runner's.
struct World<'a> {
    live: &'a BTreeSet<u32>,
}

impl KeeperEnv for World<'_> {
    fn holders(&mut self, _rdev: Rdev) -> Holders {
        Holders::None
    }
    fn shell_alive(&mut self, _pid: u32, _birth: Birth) -> bool {
        true
    }
    fn pid_alive(&mut self, pid: u32, _birth: Option<Birth>) -> bool {
        self.live.contains(&pid)
    }
}

fn header() -> MasterHeader {
    MasterHeader {
        rdev: R,
        shell_pid: 9_000,
        shell_birth: Birth::default(),
        local_id: 1,
    }
}

fn conn_of(window: i64) -> ConnId {
    u64::try_from(window).expect("a window number")
}

fn pid_of(window: i64) -> u32 {
    1_000 + u32::try_from(window).expect("a window number")
}

/// The runner: the real core, the model state it is in, the live processes, and which window
/// wrote each companion.
#[derive(Clone)]
struct Rig {
    core: KeeperCore,
    st: State,
    live: BTreeSet<u32>,
    writers: BTreeMap<CompanionId, i64>,
    next_id: CompanionId,
    now: u64,
}

impl Rig {
    /// Window 1 journals, has registered the master and sent its region: the model's initial
    /// state.
    fn new(model: &Model) -> Self {
        let mut rig = Rig {
            core: KeeperCore::new(true),
            st: model.init_state(),
            live: BTreeSet::new(),
            writers: BTreeMap::new(),
            next_id: 1,
            now: 0,
        };
        rig.hello(1, true);
        rig.core.register(conn_of(1), header(), Vec::new(), R);
        rig.companion(1);
        let projected = rig.keeper_vars(model.init_state());
        assert_eq!(projected, model.init_state(), "the initial projection");
        rig
    }

    fn tick(&mut self) -> u64 {
        self.now += 10;
        self.now
    }

    /// Window `n` connects and says HELLO (stating `CAP_JOURNAL` when `journal`); the outs.
    fn hello(&mut self, n: i64, journal: bool) -> Vec<Out> {
        let now = self.tick();
        assert!(self.core.accept(conn_of(n), pid_of(n), None), "accepted");
        self.live.insert(pid_of(n));
        let caps = CAP_SENDS_BYE | if journal { CAP_JOURNAL } else { 0 };
        let mut world = World { live: &self.live };
        self.core
            .hello(conn_of(n), PeerClass::App, caps, now, &mut world)
    }

    /// Window `n` sends its region.
    fn companion(&mut self, n: i64) -> Vec<Out> {
        let id = self.next_id;
        self.next_id += 1;
        self.writers.insert(id, n);
        self.core
            .companion(conn_of(n), R, COMPANION_JOURNAL, LEN, id)
    }

    /// Window `n`'s process dies by a signal: the EOF, then the kernel's status.
    fn die(&mut self, n: i64) -> Vec<Out> {
        let now = self.tick();
        self.live.remove(&pid_of(n));
        let mut world = World { live: &self.live };
        let mut out = self.core.eof(conn_of(n), now, &mut world);
        out.extend(self.core.peer_exit(pid_of(n), 9, now, &mut world));
        out
    }

    /// The keeper's variables of `st`, read from the core; the runner's own stay as `st` has
    /// them. `done` and `attempts` keep their value once the record is gone, as the model's do.
    fn keeper_vars(&self, mut st: State) -> State {
        match self.core.companion_view(R) {
            None => {
                // The record is gone: any companion the core still holds anywhere (there is no
                st.insert("state", 3);
                st.insert(
                    "comp",
                    self.core
                        .companions_held()
                        .first()
                        .map_or(0, |id| self.writers.get(id).copied().unwrap_or(-1)),
                );
            }
            Some(view) => {
                let state = match self.core.records()[&R].state {
                    RecordState::Claimed => 0,
                    RecordState::Orphaned => 1,
                    RecordState::Offered { .. } => 2,
                    RecordState::Held { .. } => 99,
                };
                st.insert("state", state);
                st.insert(
                    "comp",
                    view.offered
                        .map_or(0, |id| self.writers.get(&id).copied().unwrap_or(-1)),
                );
                if let Some(done) = view.reader_settled {
                    st.insert("done", i64::from(done));
                }
                st.insert("attempts", i64::from(view.attempts));
                st.insert(
                    "ocomp",
                    view.outgoing
                        .map_or(0, |id| self.writers.get(&id).copied().unwrap_or(-1)),
                );
            }
        }
        st
    }

    /// The model's `bad_*` flags after the real OFFER in `outs` to a window that did (or did
    /// not) state `CAP_JOURNAL`.
    fn judge_offer(&self, st: &mut State, outs: &[Out], journal: bool) {
        let offered: Option<Option<OfferJournal>> = outs.iter().find_map(|o| match o {
            Out::Offer { rdev, journal, .. } if *rdev == R => Some(*journal),
            _ => None,
        });
        let Some(trail) = offered else {
            return;
        };
        let region = trail.and_then(|j| j.companion.map(|c| (j.attempts, c.id)));
        if let Some((attempts, id)) = region {
            let writer = self.writers.get(&id).copied().unwrap_or(-1);
            if attempts == 0 && writer != st["dead_reader"] {
                st.insert("bad_exact", 1);
            }
            if attempts > 1 {
                st.insert("bad_poison", 1);
            }
            if !journal {
                st.insert("bad_old", 1);
            }
        }
    }
}

/// One real step for the model action `action`, from the rig's state: `(before, after)`.
fn step(rig: &mut Rig, action: &'static str) -> (State, State) {
    let before = rig.st.clone();
    let mut env = before.clone();
    let reader = before["reader"];
    let next = before["next"];
    match action {
        "Release" => {
            // The last claim closes the tab: the reader's, or the outgoing window's once its
            let last = if before["live"] == 1 {
                reader
            } else {
                before["outgoing"]
            };
            rig.core.release(conn_of(last), R);
        }
        "SuccessorCommits" => {
            // The reader names window `next` as its successor; the successor says HELLO as a
            let now = rig.tick();
            rig.live.insert(pid_of(next));
            rig.core.pending(conn_of(reader), pid_of(next), now, None);
            rig.hello(next, true);
            rig.core.register(conn_of(next), header(), Vec::new(), R);
            env.insert("outgoing", reader);
            env.insert("reader", next);
            env.insert("next", next + 1);
            env.insert("done", 0);
            env.insert("jcap", 1);
        }
        "OutgoingEnds" => {
            // The outgoing window's process ends by a signal (its Commit's own `exit(0)` inside
            rig.die(before["outgoing"]);
            env.insert("outgoing", 0);
        }
        "ReaderDies" => {
            rig.die(reader);
            env.insert("live", 0);
            env.insert("dead_reader", reader);
        }
        "OfferNew" | "OfferOld" => {
            let journal = action == "OfferNew";
            let outs = rig.hello(next, journal);
            env.insert("jcap", i64::from(journal));
            rig.judge_offer(&mut env, &outs, journal);
        }
        "RecipientDrops" => {
            rig.die(next);
            env.insert("next", next + 1);
        }
        "Claim" => {
            rig.core.register(conn_of(next), header(), Vec::new(), R);
            env.insert("live", 1);
            env.insert("reader", next);
            env.insert("next", next + 1);
        }
        "ReaderCompanion" => {
            rig.companion(reader);
        }
        "ReaderReleases" => {
            rig.core
                .companion_release(conn_of(reader), R, COMPANION_JOURNAL);
        }
        other => panic!("no real step for {other}"),
    }
    let after = rig.keeper_vars(env);
    rig.st = after.clone();
    (before, after)
}

/// What one exploration saw.
#[derive(Default)]
struct Explored {
    steps: BTreeSet<(State, &'static str, State)>,
    reached: BTreeSet<State>,
    fired: BTreeSet<&'static str>,
}

/// Breadth-first over the model's states, each reached by real steps: from every state first
/// reached, every action the model enables there is taken on a copy of the real core. With
/// `strict`, each step must be a transition of `model` with every invariant holding after it.
fn explore(model: &Model, strict: bool) -> Explored {
    let mut out = Explored::default();
    let first = Rig::new(model);
    out.reached.insert(first.st.clone());
    let mut queue = std::collections::VecDeque::from([first]);
    while let Some(rig) = queue.pop_front() {
        for action in model.actions.iter().map(|a| a.name) {
            if !model.action_enabled(action, &rig.st) {
                continue;
            }
            let mut next = rig.clone();
            let (before, after) = step(&mut next, action);
            if strict {
                assert!(
                    model.successors(action, &before).contains(&after),
                    "{action}: {before:?} -> {after:?} does not conform to {}",
                    model.name
                );
                for inv in &model.invariants {
                    assert!(
                        model.check_invariant(inv.name, &after),
                        "after {action}: {} on {after:?}",
                        inv.name
                    );
                }
            }
            out.fired.insert(action);
            out.steps.insert((before, action, after.clone()));
            if out.reached.insert(after) {
                queue.push_back(next);
            }
        }
    }
    out
}

/// Every state the model reaches.
fn model_states(model: &Model) -> BTreeSet<State> {
    let mut all = BTreeSet::from([model.init_state()]);
    let mut queue = vec![model.init_state()];
    while let Some(st) = queue.pop() {
        for a in &model.actions {
            for n in model.successors(a.name, &st) {
                if all.insert(n.clone()) {
                    queue.push(n);
                }
            }
        }
    }
    all
}

/// The real keeper refines the model: every real step is a model transition, every model state
/// is reached by real steps, every action fires. Negative control: each defect of the model
/// rejects real steps at its own site.
#[test]
fn the_real_keeper_conforms_to_the_companion_model() {
    let model = keeper_companion_custody_model();
    let out = explore(&model, true);
    for action in model.actions.iter().map(|a| a.name) {
        assert!(
            out.fired.contains(action),
            "the exploration never fired {action}: {:?}",
            out.fired
        );
    }
    let all = model_states(&model);
    assert_eq!(
        out.reached,
        all,
        "the real keeper reaches exactly the model's states ({} of {})",
        out.reached.len(),
        all.len()
    );
    eprintln!(
        "companion conformance: {} model states, {} distinct real steps",
        out.reached.len(),
        out.steps.len()
    );

    for (buggy, sites) in [
        (1, &["ReaderDies", "RecipientDrops"][..]),
        (2, &["OfferOld"][..]),
        (3, &["Release"][..]),
        (4, &["ReaderDies", "RecipientDrops"][..]),
        (5, &["ReaderDies"][..]),
    ] {
        let mutant = interp::with_buggy(&model, buggy);
        let rejected: BTreeSet<&str> = out
            .steps
            .iter()
            .filter(|(before, action, after)| !mutant.successors(action, before).contains(after))
            .map(|(_, action, _)| *action)
            .collect();
        for site in sites {
            assert!(
                rejected.contains(site),
                "Buggy = {buggy} accepts every real {site} step: rejected at {rejected:?}"
            );
        }
    }

    let mut per_action: BTreeMap<&str, usize> = BTreeMap::new();
    for (before, action, after) in &out.steps {
        let n = per_action.entry(action).or_default();
        if *n >= 2 {
            continue;
        }
        *n += 1;
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            before,
            after,
            Some(action),
            "keeper companion conformance",
        );
        assert!(ok, "{action}: {why}");
    }
}

/// THE PLAN'S NEGATIVE CONTROL: with the poison brake's attempt count removed from the real core
/// (its `fault` point, armed on this thread only), the committed model rejects real steps where
/// a restorer dies, and those same steps are exactly what the model's `Buggy = 1` admits: the
/// conformance sees the defect, and sees it as that defect.
#[test]
fn a_keeper_that_counts_no_attempt_is_rejected() {
    let model = keeper_companion_custody_model();
    let faulty = fault::with_attempts_uncounted(|| explore(&model, false));
    let rejected: BTreeSet<&str> = faulty
        .steps
        .iter()
        .filter(|(before, action, after)| !model.successors(action, before).contains(after))
        .map(|(_, action, _)| *action)
        .collect();
    assert!(
        rejected.contains("ReaderDies") && rejected.contains("RecipientDrops"),
        "the committed model must reject the uncounted deaths: {rejected:?}"
    );
    let buggy = interp::with_buggy(&model, 1);
    for (before, action, after) in &faulty.steps {
        assert!(
            buggy.successors(action, before).contains(after),
            "{action}: {before:?} -> {after:?} is not Buggy = 1's either"
        );
    }
    assert!(explore(&model, true).fired.contains("RecipientDrops"));
}

/// The anchors the conformance exercises are compiled in and were entered.
#[test]
fn the_companion_anchors_are_live() {
    assert!(
        aterm_spec::xref::reset_entered_anchors(),
        "the evidence window opens"
    );
    let model = keeper_companion_custody_model();
    let mut rig = Rig::new(&model);
    for action in [
        "SuccessorCommits",
        "OutgoingEnds",
        "ReaderDies",
        "OfferNew",
        "RecipientDrops",
        "OfferNew",
        "Claim",
        "ReaderReleases",
        "Release",
    ] {
        assert!(model.action_enabled(action, &rig.st), "{action}");
        step(&mut rig, action);
    }
    let anchored: BTreeSet<&str> = aterm_spec::xref::refinements()
        .filter(|r| r.machine == "KeeperCompanionCustody")
        .map(|r| r.action)
        .collect();
    for action in model.actions.iter().map(|a| a.name) {
        assert!(anchored.contains(action), "{action}: {anchored:?}");
    }
    let entered = aterm_spec::xref::entered_anchor_ids();
    for id in [
        "KeeperCompanionCustody::ReaderDies @ peer_died",
        "KeeperCompanionCustody::OfferNew @ offers_for",
        "KeeperCompanionCustody::RecipientDrops @ eof",
        "KeeperCompanionCustody::Claim @ register",
        "KeeperCompanionCustody::SuccessorCommits @ register",
        "KeeperCompanionCustody::OutgoingEnds @ peer_died",
        "KeeperCompanionCustody::ReaderCompanion @ companion",
        "KeeperCompanionCustody::ReaderReleases @ companion_release",
        "KeeperCompanionCustody::Release @ release",
    ] {
        assert!(entered.contains(id), "{id} never entered: {entered:?}");
    }
}
