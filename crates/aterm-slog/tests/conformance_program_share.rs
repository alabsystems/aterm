// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance tests for [`Log::enforce_program_share`] against `SessionLogProgramShare`.

use aterm_models_session::session_log_program_share_model;
use aterm_slog::{
    Actor, Conf, Fields, Key, Kind, Limits, Log, Mark, Node, Open, Src, program_share,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, with_buggy};
use aterm_spec::rng::XorShift;

use crate::support::at;

const MAX_SEQ: u64 = 7;
const SETUP: u64 = 2;

struct World {
    log: Log,
    run: Node,
    clock: u64,
}

impl World {
    fn new() -> World {
        let limits = Limits::new(64, 1 << 20).expect("a small ring");
        assert_eq!(program_share(limits), 2, "the model's share");
        let mut log = Log::with_limits(0, limits);
        log.open_session(
            Kind::Spawn,
            Src::Aterm,
            Conf::Exact,
            Actor::Aterm,
            Fields::new(),
            at(1),
        )
        .expect("the session opens");
        let run = log
            .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(2))
            .expect("a run opens");
        assert_eq!(log.last_seq(), SETUP);
        World { log, run, clock: 2 }
    }

    fn program_node(&self, r: &aterm_slog::Rec) -> i64 {
        match (r.k == Kind::Program, r.node) {
            (true, n) if n == self.run => 1,
            (true, n) if n == Node::SESSION => 2,
            _ => 0,
        }
    }

    fn project(&self) -> State {
        let fence = self.log.read_fence();
        let fenced = |seq: u64| fence.is_some_and(|f| seq <= f);
        let mut s = State::new();
        for k in ["uo", "fo", "ua", "fa", "ub", "fb"] {
            s.insert(k, 0);
        }
        for r in self.log.window().filter(|r| r.seq > SETUP) {
            let key = match (self.program_node(r), fenced(r.seq)) {
                (0, false) => "uo",
                (0, true) => "fo",
                (1, false) => "ua",
                (1, true) => "fa",
                (_, false) => "ub",
                (_, true) => "fb",
            };
            *s.get_mut(key).expect("seeded") += 1;
        }
        let last = self.log.last_seq();
        let newest = self.log.window().last().filter(|r| r.seq > SETUP);
        s.insert("seq", (last - SETUP) as i64);
        s.insert("lost", self.log.lost() as i64);
        s.insert("fence", i64::from(fence.is_some()));
        s.insert("nwp", newest.map_or(0, |r| self.program_node(r)));
        s.insert("nwf", i64::from(last > SETUP && fenced(last)));
        s.insert("qoth", 0);
        s.insert("fgone", 0);
        s
    }

    fn obs(&mut self) {
        self.clock += 5;
        self.log
            .mark(
                Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(Node::SESSION),
                at(self.clock),
            )
            .expect("an observation");
    }

    fn program(&mut self, node: Node, state: &str) {
        self.clock += 5;
        self.log
            .mark(
                Mark::new(Kind::Program, Src::Engine, Conf::Claim)
                    .at(node)
                    .fields(Fields::new().word(Key::State, state).expect("a state")),
                at(self.clock),
            )
            .expect("a program mark");
    }
}

fn reach(m: &Model, action: &str, prev: &State) -> Vec<State> {
    let mut all: Vec<State> = m.successors(action, prev);
    let mut i = 0;
    while i < all.len() {
        for e in ["EvictOther", "EvictA", "EvictB"] {
            for t in m.successors(e, &all[i]) {
                if !all.contains(&t) {
                    all.push(t);
                }
            }
        }
        i += 1;
    }
    all
}

fn admitted(m: &Model, action: &str, prev: &State, next: &State) {
    let frontier = reach(m, action, prev);
    assert!(
        frontier.contains(next),
        "real `{action}` is not the model's:\n  from {prev:?}\n  to   {next:?}"
    );
    for inv in &m.invariants {
        assert!(m.check_invariant(inv.name, next), "{}: {next:?}", inv.name);
    }
}

fn step(w: &mut World, rng: &mut XorShift) -> Option<&'static str> {
    let room = w.log.last_seq() < SETUP + MAX_SEQ;
    let state = if rng.below(2) == 0 { "working" } else { "idle" };
    match rng.below(7) {
        0 if room => {
            w.obs();
            Some("AppendOther")
        }
        1..=3 if room => {
            w.program(w.run, state);
            Some("AppendA")
        }
        4 if room => {
            w.program(Node::SESSION, state);
            Some("AppendB")
        }
        5 if w.log.read_fence().is_none() => {
            w.log.fence_reads();
            Some("Fence")
        }
        6 if w.log.read_fence().is_some() => {
            w.log.lift_read_fence();
            Some("Lift")
        }
        _ => None,
    }
}

#[test]
fn slog_program_share_conforms_to_derived_model() {
    let m = session_log_program_share_model();
    assert_eq!(World::new().project(), m.init_state(), "the walk's start");
    let mut ran = std::collections::BTreeSet::new();
    let mut took = 0;
    for seed in 1..=400u64 {
        let mut rng = XorShift::new(seed);
        let mut w = World::new();
        for _ in 0..24 {
            let prev = w.project();
            if let Some(action) = step(&mut w, &mut rng) {
                admitted(&m, action, &prev, &w.project());
                ran.insert(action);
            }
        }
        took += w.log.lost();
    }
    for a in ["AppendOther", "AppendA", "AppendB", "Fence", "Lift"] {
        assert!(ran.contains(a), "the walks never drove `{a}`: {ran:?}");
    }
    assert!(took > 0, "the share never took a mark");
}

fn rejected(m: &Model, (action, mutant): (&str, &str), prev: &State, broken: &State) {
    assert!(
        !reach(m, action, prev).contains(broken),
        "the broken `{action}` passed as the model's: {broken:?}"
    );
    assert!(
        with_buggy(m, 1).successors(mutant, prev).contains(broken),
        "`{mutant}` is not the broken call it names:\n  from {prev:?}\n  to   {broken:?}"
    );
}

fn moved(s: &State, deltas: &[(&'static str, i64)]) -> State {
    let mut out = s.clone();
    for (k, d) in deltas {
        *out.get_mut(k).unwrap_or_else(|| panic!("{k}")) += d;
    }
    out
}

#[test]
fn slog_program_share_conformance_rejects_a_miscount_and_a_wrong_take() {
    let m = session_log_program_share_model();
    let mut w = World::new();
    w.program(w.run, "working");
    w.obs();
    w.program(Node::SESSION, "working");
    w.program(w.run, "idle");
    let prev = w.project();
    w.program(w.run, "working");
    let honest = w.project();
    admitted(&m, "AppendA", &prev, &honest);
    assert_eq!(
        (honest["ua"], honest["ub"], honest["uo"], honest["lost"]),
        (2, 1, 1, 1)
    );
    let unshared = moved(&honest, &[("ua", 1), ("lost", -1)]);
    rejected(&m, ("AppendA", "AppendAUnshared"), &prev, &unshared);
    let uncounted = moved(&honest, &[("lost", -1)]);
    rejected(&m, ("AppendA", "ShareUncounted"), &prev, &uncounted);
    let any = moved(&honest, &[("uo", -1), ("lost", 1), ("qoth", 1)]);
    rejected(&m, ("AppendA", "ProgramShareEvictsAny"), &prev, &any);
    let other_node = moved(&honest, &[("ub", -1), ("lost", 1), ("qoth", 1)]);
    rejected(&m, ("AppendA", "ShareTakesOtherNode"), &prev, &other_node);
    let miscounted = moved(&honest, &[("ua", -1), ("uo", 1)]);
    assert!(!reach(&m, "AppendA", &prev).contains(&miscounted));

    let mut f = World::new();
    f.program(f.run, "working");
    f.program(f.run, "idle");
    f.log.fence_reads();
    let prev = f.project();
    f.program(f.run, "working");
    let honest = f.project();
    admitted(&m, "AppendA", &prev, &honest);
    assert_eq!((honest["fa"], honest["ua"], honest["lost"]), (2, 1, 0));
    let took_fenced = moved(&honest, &[("fa", -1), ("lost", 1), ("fgone", 1)]);
    rejected(&m, ("AppendA", "ShareTakesFenced"), &prev, &took_fenced);
}
