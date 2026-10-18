// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance: the shipping [`TurnTracker`] (over a real [`Log`])
//! against the derived `TurnTracker` model
//! (`aterm_models_session::turn_tracker_model`, checked at Tier-0 in
//! `aterm-models/tests/derived_ring_ty.rs`).
//!
//! Seeded random host schedules, and the hand-written replays of
//! `turn_replays.rs`, drive the real tracker the way the host does: one call
//! per verdict, time passing in half-second steps (the model's `Tick`), and
//! wakes that are not tied to the deadline: early (nothing changes), on
//! time and late (time passes with no wake, and the next call, a verdict
//! included, applies the settle first: the model's `Rest` then the verdict).
//! A `ctl turn` while the agent rests is the model's `CtlTurn`. After every
//! step the real state is projected onto the model's variables, the step
//! must be a path of the model's actions with every invariant holding, and
//! the turn records the step appended must be exactly the turn opens and
//! closes along that path: a tracker that splits one turn into two, or
//! supersedes its own turn, is not the model's.
//!
//! NEGATIVE CONTROLS: the Codex mid-answer idle closed at once, an early
//! wake that closes, a turn closed on `unknown`, and a projection that
//! forgets a step's parent are each rejected.

use aterm_models_session::turn_tracker_model;
use aterm_slog::{
    Actor, BoxWord, Cursor, LastClose, Log, Lvl, Node, Op, Rec, ScreenStep, SettleCause, Stamp,
    TurnTracker, Verdict, WallKind,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, with_buggy};
use aterm_spec::rng::XorShift;
use aterm_spec::verify;

use crate::support::{agent_session, at};

/// The model's settle unit: `Settle = 2` ticks of 500 ms is the real 1 s.
const TICK_MS: u64 = 500;

/// The model's word code for a verdict.
fn code(v: Verdict) -> i64 {
    match v {
        Verdict::Unknown => 0,
        Verdict::Busy => 1,
        Verdict::Idle => 2,
        Verdict::Prompt(b) if b.is_question() => 3,
        Verdict::Prompt(_) => 4,
        Verdict::Wall(_) => 5,
        Verdict::Unresponsive => 6,
        Verdict::Survey => 7,
        Verdict::Question => 8,
        Verdict::Gone => 9,
    }
}

/// The model's action for observing `v`.
fn see(v: Verdict) -> &'static str {
    match v {
        Verdict::Busy => "SeeBusy",
        Verdict::Prompt(b) if b.is_question() => "SeeDialog",
        Verdict::Prompt(_) => "SeeBox",
        Verdict::Wall(_) => "SeeWall",
        Verdict::Unresponsive => "SeeStall",
        Verdict::Survey => "SeeSurvey",
        Verdict::Idle => "SeeIdle",
        Verdict::Question => "SeeQuestion",
        Verdict::Gone => "SeeGone",
        Verdict::Unknown => "SeeUnknown",
    }
}

fn step_code(s: ScreenStep) -> i64 {
    match s {
        ScreenStep::Approval(_) => 1,
        ScreenStep::Question => 2,
        ScreenStep::Wall(_) => 3,
        ScreenStep::Stall => 4,
        ScreenStep::Survey => 5,
    }
}

/// The real tracker and its log at a moment.
pub struct Rig {
    pub log: Log,
    pub run: Node,
    pub tracker: TurnTracker,
    pub now: u64,
    /// The last `ctl turn` ledger id handed out.
    pub ledger: u64,
}

impl Rig {
    pub fn new() -> Rig {
        let mut log = Log::new(0);
        let run = agent_session(&mut log, 0);
        Rig {
            log,
            run,
            tracker: TurnTracker::new(run),
            now: 0,
            ledger: 0,
        }
    }

    pub fn stamp(&self) -> Stamp {
        at(self.now)
    }

    pub fn observe(&mut self, v: Verdict) -> Option<u64> {
        let stamp = self.stamp();
        self.tracker.observe(&mut self.log, v, None, stamp)
    }

    /// A `ctl turn` from `by` now, with the next ledger id.
    pub fn ctl(&mut self, by: Actor) -> u64 {
        self.ledger += 1;
        let stamp = self.stamp();
        self.tracker.ctl_turn(&mut self.log, by, self.ledger, stamp);
        self.ledger
    }

    /// Time passes by `ms`; the tracker is ticked when its deadline passed,
    /// as the host's scheduled wake would.
    pub fn wait(&mut self, ms: u64) {
        self.now += ms;
        if self.tracker.wake_at().is_some_and(|d| d <= self.now) {
            let stamp = self.stamp();
            self.tracker.tick(&mut self.log, stamp);
        }
    }

    /// The records appended after seq `after`.
    fn since(&self, after: u64) -> Vec<Rec> {
        let read = self.log.read(Cursor::new(self.log.ep(), after), usize::MAX);
        assert!(read.gap.is_none(), "a step appended past the ring");
        read.recs.into_iter().cloned().collect()
    }
}

/// The projection onto the model's variables. `sturn_override` replaces
/// the step's parent for the negative control.
fn project(rig: &Rig, sturn_override: Option<i64>) -> State {
    let view = rig.tracker.view(rig.now);
    let turn = view.turn.is_some_and(|t| rig.log.is_open(t));
    let open_turns = rig.log.open_under(rig.run, Lvl::Turn).count() as i64;
    let (step, sturn) = view.step.map_or((0, 0), |(node, what, under_turn)| {
        assert!(rig.log.is_open(node), "the tracker holds a closed step");
        let parent = rig.log.open_entry(node).map(|e| e.parent.lvl);
        assert_eq!(parent == Some(Lvl::Turn), under_turn, "the step's parent");
        (step_code(what), i64::from(under_turn))
    });
    let (pend, ticks) = view
        .settle
        .map_or((0, 0), |(_, held)| (1, (held / TICK_MS).min(2) as i64));
    let (cw, cticks) = view.last_close.map_or((10, 0), |c: LastClose| {
        (
            if c.ctl { 11 } else { code(c.cause) },
            c.settled_ms.map_or(0, |ms| (ms / TICK_MS).min(2) as i64),
        )
    });
    [
        ("turn", i64::from(turn)),
        ("extra", (open_turns - 1).max(0)),
        ("step", step),
        ("sturn", sturn_override.unwrap_or(sturn)),
        ("word", code(view.word)),
        ("pend", pend),
        ("ticks", ticks),
        ("cw", cw),
        ("cticks", cticks),
    ]
    .into_iter()
    .collect()
}

/// The states of a path of the model's `actions` from `from` that ends at
/// `to`, or `None` (the model is deterministic per action, but the search
/// does not rely on it).
fn path_to(m: &Model, actions: &[&str], from: &State, to: &State) -> Option<Vec<State>> {
    let Some((first, rest)) = actions.split_first() else {
        return (from == to).then(|| vec![from.clone()]);
    };
    m.successors(first, from).into_iter().find_map(|mid| {
        path_to(m, rest, &mid, to).map(|mut tail| {
            tail.insert(0, from.clone());
            tail
        })
    })
}

/// The real step from `prev` to `next` is the model's path `actions`, every
/// invariant holds along it, and the turn records it appended (after seq
/// `seq0`, with `turn0` the tracker's turn before) are exactly the path's
/// turn opens and closes: identity, not just presence.
fn admitted(
    m: &Model,
    rig: &Rig,
    actions: &[&str],
    prev: &State,
    next: &State,
    (seq0, turn0): (u64, Option<Node>),
) {
    let Some(states) = path_to(m, actions, prev, next) else {
        panic!(
            "real step is not the model's {actions:?}:\n  from {prev:?}\n  to   {next:?}\n  model {:?}",
            actions
                .first()
                .map(|a| m.successors(a, prev))
                .unwrap_or_default()
        );
    };
    for s in &states {
        for inv in &m.invariants {
            assert!(
                m.check_invariant(inv.name, s),
                "{} broken along real {actions:?}: {s:?}",
                inv.name
            );
        }
    }
    let ups = states
        .windows(2)
        .filter(|w| w[0]["turn"] == 0 && w[1]["turn"] == 1)
        .count();
    let downs = states
        .windows(2)
        .filter(|w| w[0]["turn"] == 1 && w[1]["turn"] == 0)
        .count();
    let appended = rig.since(seq0);
    let turn_recs = |op: Op| {
        appended
            .iter()
            .filter(|r| r.op == op && r.node.lvl == Lvl::Turn)
            .count()
    };
    assert_eq!(
        (turn_recs(Op::Open), turn_recs(Op::Close)),
        (ups, downs),
        "turn records of real {actions:?} are not the model's: {appended:?}"
    );
    assert!(
        !appended.iter().any(|r| r.out() == Some("superseded")),
        "the tracker superseded its own turn: {appended:?}"
    );
    if ups == 0 && downs == 0 {
        assert_eq!(rig.tracker.turn(), turn0, "the open turn changed identity");
    }
}

/// One host step against the model.
#[derive(Clone, Copy, Debug)]
pub enum Step {
    /// The verdict moved (a repeat is sent too: it must change nothing but
    /// an overdue settle).
    See(Verdict),
    /// Half a second passes, then the host wakes the tracker: early (it
    /// changes nothing) or once the settle is due.
    Half,
    /// Half a second passes with no wake: the host's timer is late.
    Elapse,
    /// The host wakes the tracker now, due or not.
    Wake,
    /// A `ctl turn` exchange from the harness.
    Ctl,
}

/// Drive `steps`, checking each against the model; returns the rig.
pub fn drive(steps: &[Step]) -> Rig {
    let m = turn_tracker_model();
    let mut rig = Rig::new();
    for step in steps {
        apply(&m, &mut rig, *step);
    }
    rig
}

/// One host step on `rig`, checked against the model.
pub fn checked(rig: &mut Rig, step: Step) {
    apply(&turn_tracker_model(), rig, step);
}

/// Time passes by half a second: the model's `Tick`, or nothing when no
/// settle is counting.
fn elapse(m: &Model, rig: &mut Rig) -> State {
    let prev = project(rig, None);
    let before = (rig.log.last_seq(), rig.tracker.turn());
    rig.now += TICK_MS;
    let ticked = project(rig, None);
    let path: &[&str] = if m.action_enabled("Tick", &prev) {
        &["Tick"]
    } else {
        &[]
    };
    admitted(m, rig, path, &prev, &ticked, before);
    ticked
}

/// The host wakes the tracker now: the model's `Rest` when the settle is
/// due, else nothing at all (the deadline it asks for included).
fn wake(m: &Model, rig: &mut Rig) {
    let prev = project(rig, None);
    let before = (rig.log.last_seq(), rig.tracker.turn());
    let asked = rig.tracker.wake_at();
    let stamp = rig.stamp();
    rig.tracker.tick(&mut rig.log, stamp);
    let next = project(rig, None);
    if m.action_enabled("Rest", &prev) {
        admitted(m, rig, &["Rest"], &prev, &next, before);
    } else {
        admitted(m, rig, &[], &prev, &next, before);
        assert_eq!(
            rig.tracker.wake_at(),
            asked,
            "an early wake moved the deadline"
        );
    }
}

fn apply(m: &Model, rig: &mut Rig, step: Step) {
    let prev = project(rig, None);
    let before = (rig.log.last_seq(), rig.tracker.turn());
    let due = m.action_enabled("Rest", &prev);
    match step {
        Step::See(v) => {
            let repeat = v == rig.tracker.view(rig.now).word;
            rig.observe(v);
            let mut path = Vec::new();
            if due {
                path.push("Rest");
            }
            if !repeat {
                path.push(see(v));
            }
            admitted(m, rig, &path, &prev, &project(rig, None), before);
        }
        Step::Half => {
            elapse(m, rig);
            wake(m, rig);
        }
        Step::Elapse => {
            elapse(m, rig);
        }
        Step::Wake => wake(m, rig),
        Step::Ctl => {
            let resting = m.action_enabled("CtlTurn", &prev);
            rig.ctl(Actor::Harness);
            let path: &[&str] = if due {
                &["Rest"]
            } else if resting {
                &["CtlTurn"]
            } else {
                &[]
            };
            admitted(m, rig, path, &prev, &project(rig, None), before);
        }
    }
}

const POOL: &[Verdict] = &[
    Verdict::Busy,
    Verdict::Busy,
    Verdict::Idle,
    Verdict::Idle,
    Verdict::Prompt(BoxWord::QUESTION),
    Verdict::Unknown,
    Verdict::Survey,
    Verdict::Question,
    Verdict::Unresponsive,
    Verdict::Gone,
    Verdict::Wall(WallKind::UsageSession),
    Verdict::Wall(WallKind::Overloaded),
];

#[test]
fn turn_tracker_conforms_to_derived_model() {
    let m = turn_tracker_model();
    let mut ran = std::collections::BTreeSet::new();
    let mut from_due = std::collections::BTreeSet::new();
    for seed in 1..=60u64 {
        let mut rng = XorShift::new(seed);
        let mut rig = Rig::new();
        for _ in 0..300 {
            let step = match rng.below(12) {
                0..=2 => Step::Half,
                3 => Step::Elapse,
                4 => Step::Wake,
                5 => Step::Ctl,
                _ => {
                    let mut v = POOL[rng.index(POOL.len())];
                    if rng.below(5) == 0 {
                        let b = ["bash:not-read-only", "edit", "bash:read-only"][rng.index(3)];
                        v = Verdict::Prompt(BoxWord::from_detail(b));
                    }
                    Step::See(v)
                }
            };
            let prev = project(&rig, None);
            if let Step::See(v) = step {
                ran.insert(see(v));
                if m.action_enabled("Rest", &prev) {
                    from_due.insert(see(v));
                }
            }
            if matches!(step, Step::Ctl) && m.action_enabled("CtlTurn", &prev) {
                ran.insert("CtlTurn");
            }
            if matches!(step, Step::Wake) && m.action_enabled("Rest", &prev) {
                ran.insert("Rest");
            }
            apply(&m, &mut rig, step);
        }
        // Every turn the tracker opened is the run's only open turn.
        assert!(rig.log.open_under(rig.run, Lvl::Turn).count() <= 1);
    }
    for a in [
        "SeeBusy",
        "SeeDialog",
        "SeeBox",
        "SeeWall",
        "SeeStall",
        "SeeSurvey",
        "SeeIdle",
        "SeeQuestion",
        "SeeGone",
        "SeeUnknown",
        "CtlTurn",
        "Rest",
    ] {
        assert!(ran.contains(a), "the walks never drove `{a}`");
    }
    // A late wake: verdicts arrive while the settle is due but unwoken.
    for a in ["SeeBusy", "SeeIdle", "SeeUnknown", "SeeWall"] {
        assert!(
            from_due.contains(a),
            "the walks never drove `{a}` past a due settle"
        );
    }

    // The ty tier re-checks one real transition where it is installed.
    let mut rig = Rig::new();
    let prev = project(&rig, None);
    rig.observe(Verdict::Busy);
    let (ok, why) = verify::validate_transition_tiered(
        &m,
        &[],
        &prev,
        &project(&rig, None),
        Some("SeeBusy"),
        "TurnTracker conformance",
    );
    assert!(ok, "real SeeBusy rejected: {why}");
}

/// THE SETTLE POLICY, by name: `settle_due` closes exactly where the
/// model's `Rest` is enabled (two half-second ticks held), and `settle_out`
/// closes an idle `done` (or `interrupted`), a prose question `asked` at
/// `heuristic`, and a wall on its kind.
#[test]
fn settle_policy_is_the_models() {
    use aterm_slog::{Conf, SettleCause, TurnOut, settle_due, settle_out};
    let m = turn_tracker_model();
    for held in (0..=3000u64).step_by(250) {
        let mut s = m.init_state();
        for (k, v) in [("turn", 1), ("pend", 1), ("word", 2)] {
            s.insert(k, v);
        }
        s.insert("ticks", (held / TICK_MS).min(2) as i64);
        assert_eq!(
            settle_due(10_000, 10_000 + held),
            m.action_enabled("Rest", &s),
            "held {held} ms"
        );
    }
    assert_eq!(
        settle_out(SettleCause::Idle, false),
        (TurnOut::Done, Conf::Strong)
    );
    assert_eq!(
        settle_out(SettleCause::Survey, true),
        (TurnOut::Interrupted, Conf::Strong)
    );
    assert_eq!(
        settle_out(SettleCause::Question, false),
        (TurnOut::Asked, Conf::Heuristic)
    );
    let k = WallKind::UsageWeekly;
    assert_eq!(
        settle_out(SettleCause::Wall(k), false),
        (TurnOut::Wall(k), Conf::Strong)
    );
}

/// NEGATIVE CONTROLS: the model rejects what the tracker refuses to do, and
/// the projection is load-bearing.
#[test]
fn turn_tracker_conformance_rejects_early_and_unknown_closes() {
    let m = turn_tracker_model();
    let buggy = with_buggy(&m, 1);

    // The Codex mid-answer idle: busy, idle, and 220 ms later busy again.
    let mut rig = drive(&[Step::See(Verdict::Busy), Step::See(Verdict::Idle)]);
    let armed = project(&rig, None);
    assert_eq!((armed["turn"], armed["pend"]), (1, 1));
    rig.now += 220;
    // An early wake 220 ms in closes nothing and keeps its deadline.
    let asked = rig.tracker.wake_at();
    let stamp = rig.stamp();
    let again = rig.tracker.tick(&mut rig.log, stamp);
    assert!(
        rig.tracker.turn().is_some(),
        "an early wake closed the turn"
    );
    assert_eq!((again, rig.tracker.wake_at()), (asked, asked));
    rig.observe(Verdict::Busy);
    assert!(
        rig.tracker.turn().is_some(),
        "the mid-answer idle kept its turn"
    );
    // A tracker that closed on the idle at once is not the model's...
    let mut early = armed.clone();
    for (k, v) in [("turn", 0), ("pend", 0), ("cw", 2), ("cticks", 0)] {
        early.insert(k, v);
    }
    assert!(aterm_spec::interp::admits(&m, &armed, &early).is_none());
    // ...and is exactly the Buggy model's EarlyRest, which its law catches.
    assert!(buggy.successors("EarlyRest", &armed).contains(&early));
    assert!(!m.check_invariant("IdleClosesOnlyAfterSettle", &early));

    // A turn closed on `unknown` is rejected.
    let busy = project(&drive(&[Step::See(Verdict::Busy)]), None);
    let mut closed = busy.clone();
    for (k, v) in [("turn", 0), ("word", 0), ("cw", 0)] {
        closed.insert(k, v);
    }
    assert!(!m.successors("SeeUnknown", &busy).contains(&closed));
    assert!(!m.check_invariant("UnknownNeverClosesATurn", &closed));

    // A projection that hangs every step from the turn: the wall the turn
    // closed on stays open on the run, and that projection breaks the law.
    let rested = drive(&[
        Step::See(Verdict::Busy),
        Step::See(Verdict::Wall(WallKind::UsageSession)),
        Step::Half,
        Step::Half,
    ]);
    let honest = project(&rested, None);
    assert_eq!((honest["turn"], honest["step"], honest["sturn"]), (0, 3, 0));
    let blind = project(&rested, Some(1));
    assert!(!m.check_invariant("StepsNestInTheirTurn", &blind));
    assert!(
        rested
            .tracker
            .view(rested.now)
            .settle
            .is_none_or(|(c, _)| c != SettleCause::Idle)
    );
}
