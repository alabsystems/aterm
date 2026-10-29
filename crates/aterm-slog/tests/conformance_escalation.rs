// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance (fix E3, finding E2E-3): what [`aterm_slog::Fold::apply`] makes of the
//! harness's escalation of a decision step, against the derived `SessionLogEscalation` model
//! (`aterm_models_session::session_log_escalation_model`, checked at Tier-0 in
//! `aterm-models/tests/derived_ring_ty.rs`). The walk plays the producer as
//! `SessionLog::observe_meta` does: an ask names [`Log::open_decision_step`], a wait names `-`,
//! and the transcript opens tool steps beside the box, as Claude Code's does.

use aterm_models_session::session_log_escalation_model;
use aterm_slog::{
    Actor, Close, Conf, Fields, Key, Kind, Log, Lvl, Mark, Node, Open, Out, Src, StepOut,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, with_buggy};
use aterm_spec::rng::XorShift;

use crate::support::{agent_session, at};

/// The most tool steps the walk keeps open beside a box.
const TOOLS_MAX: usize = 4;

struct World {
    log: Log,
    clock: u64,
    turn: Node,
    /// The decision step the walk opened last.
    step: Option<Node>,
    /// The tool steps the transcript opened that are still open, oldest first.
    tools: Vec<Node>,
    /// The harness's ask stands (the model's `a`).
    asked: bool,
    /// The standing escalation is an ask raised on the open step (the model's ghost `about`).
    about: bool,
    /// An ask was raised while a tool step opened after the box was the newest open node.
    asked_under_a_tool: bool,
}

impl World {
    fn new() -> World {
        let mut log = Log::new(0);
        let run = agent_session(&mut log, 0);
        let turn = log
            .open(
                Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                at(1),
            )
            .expect("the turn");
        World {
            log,
            clock: 1,
            turn,
            step: None,
            tools: Vec::new(),
            asked: false,
            about: false,
            asked_under_a_tool: false,
        }
    }

    fn project(&self) -> State {
        let st = self.log.state();
        let mut s = State::new();
        let s_word = match self.step {
            None => 0,
            Some(p) if self.log.is_open(p) => 1,
            Some(_) => 2,
        };
        s.insert("s", s_word);
        s.insert(
            "e",
            st.needs
                .iter()
                .flatten()
                .find(|n| n.word == "escalated")
                .map_or(0, |n| if n.node.lvl == Lvl::Step { 1 } else { 2 }),
        );
        s.insert("a", i64::from(self.asked));
        s.insert("about", i64::from(self.about));
        s.insert(
            "top",
            st.top_observed_need()
                .map_or(0, |n| if n.word == "escalated" { 2 } else { 1 }),
        );
        s
    }

    fn tick(&mut self) -> u64 {
        self.clock += 5;
        self.clock
    }

    /// Open decision step `kind` under the turn; with `beside`, a tool step the transcript opens
    /// beside it in the same ms.
    fn open_step(&mut self, kind: Kind, beside: bool) {
        let ms = self.tick();
        let f = if kind == Kind::Approval {
            Fields::new().word(Key::Box, "bash").expect("box")
        } else {
            Fields::new()
        };
        let p = self
            .log
            .open(
                Open::new(kind, Src::Screen, Conf::Strong)
                    .under(self.turn)
                    .fields(f),
                at(ms),
            )
            .expect("the decision step");
        self.step = Some(p);
        if beside {
            self.open_tool(ms);
        }
    }

    fn open_tool(&mut self, ms: u64) {
        if self.tools.len() >= TOOLS_MAX {
            return;
        }
        let tool = self
            .log
            .open(
                Open::new(Kind::Tool, Src::Transcript, Conf::Strong)
                    .under(self.turn)
                    .by(Actor::Vendor)
                    .fields(Fields::new().word(Key::Cls, "shell").expect("cls")),
                at(ms),
            )
            .expect("a tool step");
        self.tools.push(tool);
    }

    /// `mark <node> need|clear word=escalated src=meta conf=strong by=aterm`, as
    /// `observe_meta` writes it.
    fn escalation(&mut self, kind: Kind, node: Node) {
        let ms = self.tick();
        self.log
            .mark(
                Mark::new(kind, Src::Meta, Conf::Strong)
                    .at(node)
                    .by(Actor::Aterm)
                    .fields(Fields::new().word(Key::Word, "escalated").expect("word")),
                at(ms),
            )
            .expect("the escalation's mark");
    }

    /// The producer's ask: the open decision step's, else the session's.
    fn ask(&mut self) {
        let step = self.log.open_decision_step();
        self.asked_under_a_tool |= step.is_some()
            && self
                .log
                .open_nodes()
                .last()
                .is_some_and(|e| e.kind == Kind::Tool);
        self.escalation(Kind::Need, step.unwrap_or(Node::SESSION));
        self.asked = true;
        self.about = step.is_some();
    }

    fn wait(&mut self) {
        self.escalation(Kind::Need, Node::SESSION);
        self.asked = true;
        self.about = false;
    }

    fn close_step(&mut self, rng: &mut XorShift) {
        let p = self.step.expect("an open step");
        let kind = self.log.open_entry(p).map(|e| e.kind).expect("open");
        let outs: &[StepOut] = if kind == Kind::Approval {
            &[StepOut::Approved, StepOut::Declined, StepOut::Passed]
        } else {
            &[StepOut::Answered, StepOut::Passed]
        };
        let by = [Actor::Human, Actor::Harness, Actor::Ctl][rng.index(3)];
        let ms = self.tick();
        self.log
            .close(
                p,
                Close::new(
                    Out::Step(outs[rng.index(outs.len())]),
                    Src::Screen,
                    Conf::Strong,
                )
                .by(by),
                at(ms),
            )
            .expect("the step closes");
        self.about = false;
    }

    fn real(&mut self, pick: &str, rng: &mut XorShift) {
        match pick {
            "OpenStep" => {
                let kind = if rng.index(2) == 0 {
                    Kind::Approval
                } else {
                    Kind::Question
                };
                self.open_step(kind, rng.index(2) == 0);
            }
            "Escalate" | "Reraise" => self.ask(),
            "EscalateWait" | "ReraiseWait" => self.wait(),
            "CloseStep" => self.close_step(rng),
            "HarnessClear" => {
                self.escalation(Kind::Clear, Node::SESSION);
                self.asked = false;
                self.about = false;
            }
            other => panic!("no real step for {other}"),
        }
    }

    /// The transcript's own moves, which the model does not see: a tool step opened beside the
    /// box, or the oldest one's result.
    fn noise(&mut self, pick: &str) {
        let ms = self.tick();
        if pick == "ToolBeside" {
            self.open_tool(ms);
        } else if !self.tools.is_empty() {
            let tool = self.tools.remove(0);
            self.log
                .close(
                    tool,
                    Close::new(Out::Step(StepOut::Ok), Src::Transcript, Conf::Strong)
                        .by(Actor::Vendor),
                    at(ms),
                )
                .expect("the tool's result");
        }
    }

    /// One real step and the model's name for it, when it is enabled; the transcript's moves
    /// are stutters the projection never sees.
    fn step(&mut self, m: &Model, rng: &mut XorShift) -> Option<&'static str> {
        const ACTIONS: [&str; 9] = [
            "OpenStep",
            "Escalate",
            "EscalateWait",
            "Reraise",
            "ReraiseWait",
            "CloseStep",
            "HarnessClear",
            "ToolBeside",
            "ToolDone",
        ];
        let s = self.project();
        let pick = ACTIONS[rng.index(ACTIONS.len())];
        if matches!(pick, "ToolBeside" | "ToolDone") {
            self.noise(pick);
            assert_eq!(
                self.project(),
                s,
                "the transcript's {pick} moved the escalation"
            );
            return None;
        }
        if !m.action_enabled(pick, &s) {
            return None;
        }
        self.real(pick, rng);
        Some(pick)
    }
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
fn escalation_conforms_to_derived_model() {
    let m = session_log_escalation_model();
    let mut ran = std::collections::BTreeSet::new();
    let mut under_a_tool = false;
    for seed in 1..=300u64 {
        let mut rng = XorShift::new(seed);
        let mut w = World::new();
        assert_eq!(w.project(), m.init_state(), "the empty fold is the model's");
        for _ in 0..40 {
            let prev = w.project();
            if let Some(action) = w.step(&m, &mut rng) {
                admitted(&m, action, &prev, &w.project());
                ran.insert(action);
            }
        }
        under_a_tool |= w.asked_under_a_tool;
    }
    assert_eq!(ran.len(), 7, "every action driven: {ran:?}");
    assert!(
        under_a_tool,
        "no ask was raised while a tool step stood beside the box, newest"
    );
}

/// NEGATIVE CONTROLS: the pre-fix producer and fold (every escalation the session's), a close
/// that leaves the step's escalation standing, and a clear that uncovers what it lifted, are not
/// steps the model makes; each is the mutant it names.
#[test]
fn escalation_conformance_rejects_each_broken_call() {
    let m = session_log_escalation_model();
    let boxed = || {
        let mut w = World::new();
        w.open_step(Kind::Approval, true);
        w
    };

    let mut w = boxed();
    let prev = w.project();
    w.escalation(Kind::Need, Node::SESSION);
    w.asked = true;
    w.about = true;
    let broken = w.project();
    rejected(&m, ("Escalate", "EscalateOnTheSession"), &prev, &broken);
    assert!(!m.check_invariant("AskOnAStepIsTheSteps", &broken));

    let mut w = boxed();
    let prev = w.project();
    w.ask();
    admitted(&m, "Escalate", &prev, &w.project());
    let prev = w.project();
    w.close_step(&mut XorShift::new(1));
    let honest = w.project();
    admitted(&m, "CloseStep", &prev, &honest);
    let mut broken = honest.clone();
    broken.insert("e", 1);
    broken.insert("about", 1);
    rejected(&m, ("CloseStep", "CloseStepKeepsBinding"), &prev, &broken);

    let mut w = boxed();
    w.ask();
    let prev = w.project();
    w.escalation(Kind::Clear, Node::SESSION);
    w.asked = false;
    w.about = false;
    let honest = w.project();
    admitted(&m, "HarnessClear", &prev, &honest);
    assert_eq!(honest["top"], 1, "the box's own need stands");
    let mut broken = honest.clone();
    broken.insert("top", 2);
    rejected(&m, ("HarnessClear", "ClearUncovers"), &prev, &broken);
}
