// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance: the program share ([`Log::enforce_program_share`],
//! run by every append of a program mark and by the readers' fence lifting)
//! against the derived `SessionLogProgramShare` model
//! (`aterm_models_session::session_log_program_share_model`, checked at
//! Tier-0 in `aterm-models/tests/derived_ring_ty.rs`).
//!
//! A seeded walk drives a real 64-record ring (a share of two program marks
//! a node) under a session and an open run, both opened before the walk (the
//! model's records are the walk's): observations (a bell at the session),
//! program marks at the run (the model's node `a`) and at the session (node
//! `b`), the readers fenced and the fence lifted (ruling R4). After every
//! step the live records are projected onto the model (others and each
//! node's program marks, fenced or not, the newest's node, the log's own
//! `lost`), and the step must be the model's: the action, then whatever the
//! level-aware batch took. NEGATIVE CONTROLS: a projection that miscounts a
//! program mark as another record, one in which the share took a fenced
//! program mark, and one in which it took the other node's, are each
//! rejected.

use aterm_models_session::session_log_program_share_model;
use aterm_slog::{
    Actor, Conf, Fields, Key, Kind, Limits, Log, Mark, Node, Open, Src, program_share,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, with_buggy};
use aterm_spec::rng::XorShift;

use crate::support::at;

/// The model's appends.
const MAX_SEQ: u64 = 7;
/// The records the world opens before the walk: the session and its run.
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

    /// Which program node `r` is a mark of: 1 the run, 2 the session, 0 none.
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

/// The states the model reaches from `prev` by `action`, then any evictions.
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

/// The broken state `broken` is no state the honest `action` (and the
/// ring's level-aware batch after it) makes of `prev`, and it is exactly
/// what mutant `mutant` makes of `prev` (T11: each negative control is its
/// mutant's successor, so the conformance check would catch that mutant's
/// call in the code).
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

/// `s` with each `(key, delta)` added.
fn moved(s: &State, deltas: &[(&'static str, i64)]) -> State {
    let mut out = s.clone();
    for (k, d) in deltas {
        *out.get_mut(k).unwrap_or_else(|| panic!("{k}")) += d;
    }
    out
}

/// NEGATIVE CONTROLS, each the successor of its mutant from the real run's
/// state before the append (T11): the share removed (`AppendAUnshared`),
/// a take left uncounted (`ShareUncounted`), the share taking another
/// record (`ProgramShareEvictsAny`) or the other node's program mark
/// (`ShareTakesOtherNode`) beside the run's own, and the share taking a
/// fenced program mark (`ShareTakesFenced`); a ghost the broken take sets
/// (`qoth`, `fgone`) is what the real run's projection reads as zero. And
/// a projection that miscounts a program mark as another record.
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
    // The share removed: the third program mark of the run is kept.
    let unshared = moved(&honest, &[("ua", 1), ("lost", -1)]);
    rejected(&m, ("AppendA", "AppendAUnshared"), &prev, &unshared);
    // The take is not counted into `lost`.
    let uncounted = moved(&honest, &[("lost", -1)]);
    rejected(&m, ("AppendA", "ShareUncounted"), &prev, &uncounted);
    // The share took the bell beside the run's own mark.
    let any = moved(&honest, &[("uo", -1), ("lost", 1), ("qoth", 1)]);
    rejected(&m, ("AppendA", "ProgramShareEvictsAny"), &prev, &any);
    // The share took the session's program mark beside the run's own.
    let other_node = moved(&honest, &[("ub", -1), ("lost", 1), ("qoth", 1)]);
    rejected(&m, ("AppendA", "ShareTakesOtherNode"), &prev, &other_node);
    // A program mark counted as another record (the projection's own
    // miscount: no mutant makes it, and no honest step does).
    let miscounted = moved(&honest, &[("ua", -1), ("uo", 1)]);
    assert!(!reach(&m, "AppendA", &prev).contains(&miscounted));

    // Fenced: two program marks held by the fence, the third beside them.
    let mut f = World::new();
    f.program(f.run, "working");
    f.program(f.run, "idle");
    f.log.fence_reads();
    let prev = f.project();
    f.program(f.run, "working");
    let honest = f.project();
    admitted(&m, "AppendA", &prev, &honest);
    assert_eq!((honest["fa"], honest["ua"], honest["lost"]), (2, 1, 0));
    // The same append had the share taken a fenced program mark.
    let took_fenced = moved(&honest, &[("fa", -1), ("lost", 1), ("fgone", 1)]);
    rejected(&m, ("AppendA", "ShareTakesFenced"), &prev, &took_fenced);
}
