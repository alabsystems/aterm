// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance test for [`aterm_slog::Fold::apply`] and clause projection against `SessionLogFold`.

use aterm_models_session::session_log_fold_model;
use aterm_slog::{
    Actor, Amend, ClauseRow, Close, Conf, Cursor, Fields, Fold, Key, Kind, Log, Lvl, Mark, Node,
    Op, Open, Out, Rec, RunOut, Src, StepOut, TurnOut, amend_refines_out, confidence_law,
    need_takes_amend, shown_label, turn_out_rebucket,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, with_buggy};
use aterm_spec::rng::XorShift;

use crate::support::{WALL0, at};

const WALL: &str = "wall:usage-session";
const TTL_S: u64 = 600;

struct World {
    log: Log,
    /// The fold under test: fed the window, and replays.
    fold: Fold,
    /// The last seq fed to it.
    fed: u64,
    clock: u64,
    run: Option<Node>,
    turn: Option<Node>,
    step: Option<Node>,
    /// The outcome S9 and A5 give the turn (`gw`, `gc`).
    gw: i64,
    gc: i64,
    /// The best rank the step's need was given (`sg`).
    sg: i64,
}

fn rank(c: Conf) -> i64 {
    i64::from(c.rank())
}

fn out_code(w: &str) -> i64 {
    match w {
        "done" => 1,
        w if w.starts_with("wall:") => 2,
        "gone" => 3,
        _ => 9,
    }
}

fn row_code(row: ClauseRow) -> i64 {
    match row {
        ClauseRow::Need => 1,
        ClauseRow::Label => 2,
        ClauseRow::Step | ClauseRow::Turn | ClauseRow::Run => 3,
    }
}

impl World {
    fn new() -> World {
        let mut log = Log::new(0);
        log.open_session(
            Kind::Spawn,
            Src::Aterm,
            Conf::Exact,
            Actor::Aterm,
            Fields::new(),
            at(0),
        )
        .expect("the session opens");
        let mut w = World {
            fold: Fold::new(log.ep()),
            log,
            fed: 0,
            clock: 0,
            run: None,
            turn: None,
            step: None,
            gw: 0,
            gc: 0,
            sg: 0,
        };
        w.feed();
        w
    }

    fn now(&self) -> u64 {
        WALL0 + self.clock
    }

    fn feed(&mut self) {
        let read = self
            .log
            .read(Cursor::new(self.log.ep(), self.fed), usize::MAX);
        assert!(read.gap.is_none(), "the walk outran the ring");
        let recs: Vec<Rec> = read.recs.into_iter().cloned().collect();
        for r in &recs {
            self.fold.apply(r);
            self.fed = r.seq;
        }
    }

    fn level(open: Option<Node>, last: Option<Node>, mine: Option<Node>) -> i64 {
        match mine {
            None if open.is_none() && last.is_none() => 0,
            Some(n) if open == Some(n) => 1,
            Some(n) if last == Some(n) && open.is_none() => 2,
            _ => 9,
        }
    }

    fn project(&self) -> State {
        project_fold(self, &self.fold)
    }

    fn stamp(&mut self) -> aterm_slog::Stamp {
        self.clock += 5;
        at(self.clock)
    }

    fn amend_turn(&mut self, conf: Conf) {
        let turn = self.turn.expect("a turn");
        let stamp = self.stamp();
        self.log
            .amend(
                turn,
                Amend::new(
                    Kind::Turn,
                    Src::Transcript,
                    conf,
                    Fields::new().word(Key::Out, WALL).expect("out"),
                ),
                stamp,
            )
            .expect("amends");
    }

    fn step(&mut self, m: &Model, rng: &mut XorShift) -> Option<&'static str> {
        const ACTIONS: [&str; 18] = [
            "OpenRun",
            "OpenTurn",
            "CloseDone",
            "AmendOutExact",
            "AmendOutStrong",
            "OpenStep",
            "ConfirmStep",
            "RetractStep",
            "CloseRun",
            "MarkNeed",
            "Label",
            "Expire",
            "Tick",
            "Replay",
            "AmendOutExact",
            "OpenStep",
            "Replay",
            "Tick",
        ];
        let pick = ACTIONS[rng.index(ACTIONS.len())];
        if !m.action_enabled(pick, &self.project()) {
            return None;
        }
        self.real(pick, rng);
        Some(pick)
    }

    fn real(&mut self, pick: &str, rng: &mut XorShift) {
        match pick {
            "OpenRun" => {
                let stamp = self.stamp();
                let f = Fields::new().tok(Key::Agent, "claude").expect("agent");
                self.run = Some(
                    self.log
                        .open(
                            Open::new(Kind::Agent, Src::Status, Conf::Strong).fields(f),
                            stamp,
                        )
                        .expect("the run opens"),
                );
            }
            "OpenTurn" => {
                let stamp = self.stamp();
                let run = self.run.expect("a run");
                self.turn = Some(
                    self.log
                        .open(
                            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                            stamp,
                        )
                        .expect("the turn opens"),
                );
            }
            "CloseDone" => {
                let stamp = self.stamp();
                self.log
                    .close(
                        self.turn.expect("a turn"),
                        Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
                        stamp,
                    )
                    .expect("closes");
                if self.gc != 4 {
                    (self.gw, self.gc) = (1, 3);
                }
            }
            "AmendOutExact" => {
                self.amend_turn(Conf::Exact);
                (self.gw, self.gc) = (2, 4);
            }
            "AmendOutStrong" => self.amend_turn(Conf::Strong),
            "OpenStep" => {
                let stamp = self.stamp();
                let f = Fields::new()
                    .word(Key::Wall, "usage-session")
                    .expect("wall");
                self.step = Some(
                    self.log
                        .open(
                            Open::new(Kind::Wall, Src::Screen, Conf::Strong)
                                .under(self.turn.expect("a turn"))
                                .fields(f),
                            stamp,
                        )
                        .expect("the wall opens"),
                );
                self.sg = 3;
            }
            "ConfirmStep" => {
                let stamp = self.stamp();
                self.log
                    .amend(
                        self.step.expect("a step"),
                        Amend::new(
                            Kind::Wall,
                            Src::Transcript,
                            Conf::Exact,
                            Fields::new().u(Key::Confirmed, 1).expect("confirmed"),
                        ),
                        stamp,
                    )
                    .expect("confirms");
                self.sg = 4;
            }
            "RetractStep" => {
                let stamp = self.stamp();
                self.log
                    .close(
                        self.step.expect("a step"),
                        Close::new(Out::Step(StepOut::Retracted), Src::Transcript, Conf::Exact),
                        stamp,
                    )
                    .expect("retracts");
            }
            "CloseRun" => {
                let turn_open = self.turn.is_some_and(|t| self.log.is_open(t));
                let stamp = self.stamp();
                self.log
                    .close(
                        self.run.expect("a run"),
                        Close::new(Out::Run(RunOut::Ok), Src::Status, Conf::Strong),
                        stamp,
                    )
                    .expect("closes");
                if turn_open && self.gc != 4 {
                    (self.gw, self.gc) = (3, 3);
                }
            }
            "MarkNeed" => {
                let stamp = self.stamp();
                self.log
                    .mark(
                        Mark::new(Kind::Need, Src::Mark, Conf::Claim)
                            .by(Actor::Harness)
                            .fields(Fields::new().word(Key::Word, "help").expect("word")),
                        stamp,
                    )
                    .expect("need");
            }
            "Label" => {
                let stamp = self.stamp();
                let f = Fields::new()
                    .text(Key::Text, "mine")
                    .and_then(|f| f.u(Key::Ttl, TTL_S))
                    .expect("label");
                self.log
                    .mark(
                        Mark::new(Kind::Label, Src::Mark, Conf::Claim)
                            .by(Actor::Harness)
                            .fields(f),
                        stamp,
                    )
                    .expect("label");
            }
            "Expire" => {
                let until = self
                    .fold
                    .state()
                    .label
                    .as_ref()
                    .and_then(|c| c.until)
                    .expect("a label's until");
                self.clock = until - WALL0 + 1;
            }
            "Tick" => self.clock += 1000,
            _ => {
                let as_of = self.fold.state().as_of;
                let old: Vec<Rec> = self
                    .log
                    .window()
                    .filter(|r| r.seq <= as_of)
                    .cloned()
                    .collect();
                let r = &old[rng.index(old.len())];
                self.fold.apply(r);
            }
        }
        if pick != "Replay" {
            self.feed();
        }
    }
}

fn project_fold(w: &World, fold: &Fold) -> State {
    let st = fold.state();
    let now = w.now();
    let open = |i: usize| st.open[i].as_ref().map(|o| o.node);
    let last = |i: usize| st.last[i].as_ref().map(|c| c.node);
    let r = World::level(open(0), last(0), w.run);
    let t = World::level(open(1), last(1), w.turn);
    let s = World::level(open(2), last(2), w.step);
    let (ow, oc) = match (&st.last[1], t) {
        (Some(c), 2) => {
            assert_eq!(
                c.f.word(Key::Out),
                Some(c.out),
                "the held out= field and the outcome disagree"
            );
            (
                out_code(c.out),
                c.f.held(Key::Out).map_or(0, |h| rank(h.conf)),
            )
        }
        _ => (0, 0),
    };
    let nc = st
        .needs
        .iter()
        .flatten()
        .find(|n| Some(n.node) == w.step)
        .map_or(0, |n| rank(n.conf));
    let top = st.top_need().map_or(0, |n| {
        if n.src == Src::Mark {
            2
        } else if Some(n.node) == w.step {
            1
        } else {
            9
        }
    });
    let lab = i64::from(st.label.is_some());
    [
        ("r", r),
        ("t", t),
        ("ow", ow),
        ("oc", oc),
        ("gw", w.gw),
        ("gc", w.gc),
        ("nd", i64::from(st.totals.done)),
        ("nw", i64::from(st.totals.walled)),
        ("ng", i64::from(st.totals.other)),
        ("s", s),
        ("nc", nc),
        ("sg", w.sg),
        (
            "cn",
            i64::from(st.needs.iter().flatten().any(|n| n.src == Src::Mark)),
        ),
        ("top", top),
        ("lab", lab),
        ("lx", i64::from(lab == 1 && shown_label(st, now).is_none())),
        ("cl", fold.clause(now).map_or(0, |c| row_code(c.row))),
        ("dup", i64::from(st != w.log.state())),
    ]
    .into_iter()
    .collect()
}

fn admitted(m: &Model, action: &str, prev: &State, next: &State) {
    assert!(
        m.successors(action, prev).contains(next),
        "real `{action}` is not the model's:\n  from {prev:?}\n  to   {next:?}\n  model {:?}",
        m.successors(action, prev)
    );
    for inv in &m.invariants {
        assert!(m.check_invariant(inv.name, next), "{}: {next:?}", inv.name);
    }
}

#[test]
fn fold_conforms_to_derived_model() {
    let m = session_log_fold_model();
    let mut ran = std::collections::BTreeSet::new();
    let mut exact_while_open = false;
    for seed in 1..=400u64 {
        let mut rng = XorShift::new(seed);
        let mut w = World::new();
        assert_eq!(w.project(), m.init_state(), "the empty fold is the model's");
        for _ in 0..40 {
            let prev = w.project();
            if let Some(action) = w.step(&m, &mut rng) {
                admitted(&m, action, &prev, &w.project());
                ran.insert(action);
                exact_while_open |= action == "AmendOutExact" && prev["t"] == 1;
            }
        }
    }
    assert_eq!(ran.len(), 14, "every action driven: {ran:?}");
    assert!(exact_while_open, "no exact amend reached an open turn");
}

fn world_after(m: &Model, path: &[&str]) -> World {
    let mut w = World::new();
    let mut rng = XorShift::new(7);
    for action in path {
        let prev = w.project();
        assert!(m.action_enabled(action, &prev), "{action}: {prev:?}");
        w.real(action, &mut rng);
        admitted(m, action, &prev, &w.project());
    }
    w
}

fn rejected(m: &Model, (action, mutant): (&str, &str), prev: &State, broken: &State) {
    assert!(
        !m.successors(action, prev).contains(broken),
        "the broken `{action}` passed as the model's: {broken:?}"
    );
    assert!(
        with_buggy(m, 1).successors(mutant, prev).contains(broken),
        "`{mutant}` is not the broken call it names:\n  from {prev:?}\n  to   {broken:?}"
    );
}

#[test]
fn fold_conformance_rejects_each_broken_call() {
    let m = session_log_fold_model();
    let closed = ["OpenRun", "OpenTurn", "CloseDone"];

    let w = world_after(&m, &closed);
    let prev = w.project();
    let close = w
        .log
        .window()
        .find(|r| r.op == Op::Close && Some(r.node) == w.turn)
        .cloned()
        .expect("the close");
    let mut honest = w.fold.clone();
    honest.apply(&close);
    assert_eq!(project_fold(&w, &honest), prev, "a replay changed nothing");
    let mut broken = w.fold.clone();
    broken.apply(&Rec {
        seq: w.fold.state().as_of + 1,
        ..close
    });
    let replayed = project_fold(&w, &broken);
    assert_eq!((replayed["dup"], replayed["nd"]), (1, 2), "{replayed:?}");
    assert!(!m.successors("Replay", &prev).contains(&replayed));
    assert!(!m.check_invariant("Idempotent", &replayed));

    let mut w = world_after(&m, &closed);
    let prev = w.project();
    let held = w.fold.state().last[1]
        .as_ref()
        .and_then(|c| c.f.held(Key::Out))
        .cloned()
        .expect("held");
    let seq = w.log.last_seq() + 1;
    assert!(confidence_law(held.conf, held.seq, Conf::Strong, seq));
    let out = w.fold.state().last[1]
        .as_ref()
        .map(|c| c.out)
        .expect("closed");
    assert_eq!(out, "done");
    assert!(
        !amend_refines_out(Lvl::Turn, out, (held.conf, held.seq), Conf::Strong, seq),
        "a turn's held outcome takes an exact amend only, even its `gone`"
    );
    assert!(!amend_refines_out(
        Lvl::Turn,
        "gone",
        (held.conf, held.seq),
        Conf::Strong,
        seq
    ));
    w.real("AmendOutStrong", &mut XorShift::new(1));
    let honest = w.project();
    admitted(&m, "AmendOutStrong", &prev, &honest);
    let mut totals = w.fold.state().totals.clone();
    turn_out_rebucket(&mut totals, "done", WALL);
    let mut broken = honest.clone();
    broken.insert("ow", 2);
    broken.insert("nd", i64::from(totals.done));
    broken.insert("nw", i64::from(totals.walled));
    rejected(&m, ("AmendOutStrong", "StrongAmendRefines"), &prev, &broken);

    let mut w = world_after(&m, &["OpenRun", "OpenTurn", "CloseDone", "AmendOutExact"]);
    let prev = w.project();
    let held = w.fold.state().last[1]
        .as_ref()
        .and_then(|c| c.f.held(Key::Out))
        .cloned()
        .expect("held");
    assert_eq!(held.conf, Conf::Exact);
    assert!(!confidence_law(
        held.conf,
        held.seq,
        Conf::Strong,
        w.log.last_seq() + 1
    ));
    w.real("AmendOutStrong", &mut XorShift::new(1));
    let honest = w.project();
    admitted(&m, "AmendOutStrong", &prev, &honest);
    let mut broken = honest.clone();
    broken.insert("oc", rank(Conf::Strong));
    rejected(&m, ("AmendOutStrong", "LowerAmendWins"), &prev, &broken);

    let mut w = world_after(&m, &["OpenRun", "OpenTurn", "OpenStep"]);
    let prev = w.project();
    let need = w.fold.state().top_need().cloned().expect("the wall's need");
    w.real("ConfirmStep", &mut XorShift::new(1));
    let confirm = w.log.window().last().cloned().expect("the confirmation");
    assert!(need_takes_amend(&need, &confirm));
    let honest = w.project();
    admitted(&m, "ConfirmStep", &prev, &honest);
    let mut broken = honest.clone();
    broken.insert("nc", rank(need.conf));
    rejected(&m, ("ConfirmStep", "ConfirmNoUpgrade"), &prev, &broken);

    let mut w = world_after(&m, &closed);
    let prev = w.project();
    let mut only_adds = w.fold.state().totals.clone();
    only_adds.walled += 1;
    w.real("AmendOutExact", &mut XorShift::new(1));
    let honest = w.project();
    admitted(&m, "AmendOutExact", &prev, &honest);
    let mut broken = honest.clone();
    broken.insert("nd", i64::from(only_adds.done));
    broken.insert("nw", i64::from(only_adds.walled));
    rejected(
        &m,
        ("AmendOutExact", "RebucketWithoutDecrement"),
        &prev,
        &broken,
    );

    let mut w = world_after(&m, &["OpenRun", "OpenTurn", "OpenStep"]);
    let prev = w.project();
    w.real("RetractStep", &mut XorShift::new(1));
    let honest = w.project();
    admitted(&m, "RetractStep", &prev, &honest);
    let mut broken = honest.clone();
    for k in ["nc", "top", "cl"] {
        broken.insert(k, prev[k]);
    }
    rejected(&m, ("RetractStep", "RetractKeepsNeed"), &prev, &broken);

    let mut w = world_after(&m, &["OpenRun", "OpenTurn"]);
    let prev = w.project();
    w.real("Tick", &mut XorShift::new(1));
    let later = w.fold.clause(w.now()).expect("a clause");
    assert_ne!(later.row, ClauseRow::Need);
    assert_eq!(w.fold.hints(w.now()).quiet_for_ms, 1000);
    let honest = w.project();
    admitted(&m, "Tick", &prev, &honest);
    let mut broken = honest.clone();
    broken.insert("cl", 1);
    rejected(&m, ("Tick", "TickRaisesNeed"), &prev, &broken);

    let mut w = world_after(&m, &["OpenRun", "OpenTurn"]);
    let prev = w.project();
    w.real("CloseRun", &mut XorShift::new(1));
    let honest = w.project();
    admitted(&m, "CloseRun", &prev, &honest);
    let mut broken = prev.clone();
    broken.insert("r", 2);
    rejected(&m, ("CloseRun", "CloseRunLeavesTurn"), &prev, &broken);

    let mut w = world_after(&m, &["OpenRun", "Label"]);
    let prev = w.project();
    w.real("Expire", &mut XorShift::new(1));
    let st = w.fold.state();
    assert!(st.label_proven.is_some() && shown_label(st, w.now()).is_none());
    let honest = w.project();
    admitted(&m, "Expire", &prev, &honest);
    let mut broken = honest.clone();
    broken.insert("cl", prev["cl"]);
    rejected(&m, ("Expire", "ExpiredLabelShown"), &prev, &broken);

    let mut w = world_after(&m, &["OpenRun", "OpenTurn", "OpenStep"]);
    let prev = w.project();
    w.real("Label", &mut XorShift::new(1));
    let honest = w.project();
    admitted(&m, "Label", &prev, &honest);
    let mut broken = honest.clone();
    broken.insert("cl", 2);
    rejected(&m, ("Label", "LabelHidesNeed"), &prev, &broken);
}
