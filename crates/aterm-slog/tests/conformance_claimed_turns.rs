// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance: what [`TurnTracker::claim_edge`] and a transcript read `state=partial`
//! make of two codex-alab turns, against the derived `ClaimedTurnEdges` model
//! (`aterm_models_session::claimed_turn_edges_model`, checked at Tier-0 in
//! `aterm-models/tests/derived_ring_ty.rs`; ATERM-KI-1029-1).

use std::collections::BTreeSet;

use aterm_models_session::claimed_turn_edges_model;
use aterm_slog::{
    Claimed, Key, Kind, Log, Node, Op, Rec, ShortId, Src, TurnTracker, TxEvent, TxPos, TxState,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, with_buggy};
use aterm_spec::rng::XorShift;

use crate::support::{WALL0, agent_session, at, lines, time_order_holds};

const PROCESS_START: u64 = WALL0 - 60_000;
const FP: u64 = 0xc0de_0000_0000_7501;

/// The model's steps the walk drives: each is the real call [`World::step`] makes.
const STEPS: [&str; 9] = [
    "Parse",
    "FeedBegin1",
    "FeedEnd1",
    "FeedBegin2",
    "FeedEnd2",
    "Prompt1",
    "RowEnd1",
    "Prompt2",
    "RowEnd2",
];

/// The host feeding the engine's edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Host {
    /// The shipping host: every edge reaches [`TurnTracker::claim_edge`].
    Feeds,
    /// The host before the fix, for the negative control: no beginning reaches the tracker (a
    /// turn between two status samples moved the count alone).
    NoBeginnings,
}

/// The real tracker and its log, bound to a rollout read `state=partial`, and the driver's
/// counts: edges parsed (`d`) and fed (`f`), rollout rows read (`p`).
struct World {
    log: Log,
    run: Node,
    tr: TurnTracker,
    d: i64,
    f: i64,
    p: i64,
    off: u64,
}

impl World {
    fn new() -> World {
        let mut log = Log::new(1);
        let run = agent_session(&mut log, 0);
        let mut w = World {
            log,
            run,
            tr: TurnTracker::new(run),
            d: 0,
            f: 0,
            p: 0,
            off: 0,
        };
        let bind = TxEvent::Bind {
            len: 0,
            floor_ms: PROCESS_START,
        };
        let pos = w.pos(WALL0);
        w.tr.transcript(&mut w.log, bind, pos, TxState::Partial, at(0));
        w
    }

    const fn pos(&self, wall_ms: u64) -> TxPos {
        TxPos {
            fp: FP,
            id: Some((1, FP)),
            off: self.off,
            wall_ms,
        }
    }

    /// An engine edge at its batch time `ms`, the run's bucket granting every token it asks.
    fn edge(&mut self, claimed: Claimed, ms: u64) {
        self.tr
            .claim_edge(&mut self.log, claimed, at(ms), &mut || true);
    }

    /// A rollout row written at `ms` and read a moment later.
    fn row(&mut self, ev: TxEvent<'_>, ms: u64) {
        self.off += 200;
        let pos = self.pos(WALL0 + ms);
        self.tr
            .transcript(&mut self.log, ev, pos, TxState::Partial, at(ms + 1));
    }

    /// The real call for the model's `step`, and the model action it is under `host`. Span `j`'s
    /// beginning is at `j * 1000` ms, its prompt row 3 ms later, its end row at 15 and its end at
    /// 17.
    fn step(&mut self, step: &'static str, host: Host) -> &'static str {
        let ms = if step.ends_with('1') { 1_000 } else { 2_000 };
        match step {
            "Parse" => self.d += 1,
            "FeedBegin1" | "FeedBegin2" => {
                self.f += 1;
                if host == Host::NoBeginnings {
                    return "BeginUnfed";
                }
                self.edge(Claimed::Working, ms);
            }
            "FeedEnd1" | "FeedEnd2" => {
                self.f += 1;
                self.edge(Claimed::Rested, ms + 17);
            }
            "Prompt1" | "Prompt2" => {
                self.p += 1;
                let vref = ShortId::new(if ms == 1_000 { "v1" } else { "v2" }).expect("an id");
                self.row(TxEvent::Prompt { vref }, ms + 3);
            }
            "RowEnd1" | "RowEnd2" => {
                self.p += 1;
                let end = TxEvent::TurnEnd {
                    vdur_ms: 12,
                    tok_in: 900,
                    tok_out: 120,
                };
                self.row(end, ms + 15);
            }
            other => panic!("no step `{other}`"),
        }
        step
    }

    fn named(&self, node: Node) -> bool {
        self.log.window().any(|r| {
            r.node == node && matches!(r.op, Op::Open | Op::Amend) && r.f.get(Key::Vref).is_some()
        })
    }

    /// The model's state: a turn's source names its variable (`src=engine` the claim's `c`,
    /// `src=transcript` the rollout's `x`) and its open's time its span (the second from 2000
    /// ms); a second turn in one slot projects as 3, which the model never holds.
    fn project(&self) -> State {
        let mut slots: [[Vec<Node>; 2]; 2] = Default::default();
        let opens = self
            .log
            .window()
            .filter(|r| r.op == Op::Open && r.k == Kind::Turn && r.par == Some(self.run));
        for r in opens {
            let span = usize::from(r.t >= WALL0 + 2_000);
            let src = match r.src {
                Src::Engine => 0,
                Src::Transcript => 1,
                other => panic!("a turn opened at src={other:?}: {r:?}"),
            };
            slots[src][span].push(r.node);
        }
        let one = |nodes: &[Node]| match nodes {
            [] => (0, None),
            [n] => (if self.log.is_open(*n) { 1 } else { 2 }, Some(*n)),
            _ => (3, None),
        };
        let untaken: Vec<Node> = self.tr.untaken().collect();
        let mut s = State::new();
        for (k, v) in [("d", self.d), ("f", self.f), ("p", self.p)] {
            s.insert(k, v);
        }
        for (j, (c, x)) in [("c1", "x1"), ("c2", "x2")].into_iter().enumerate() {
            let (cv, cn) = one(&slots[0][j]);
            let (xv, xn) = one(&slots[1][j]);
            s.insert(c, cv);
            s.insert(x, xv);
            let named = cn.is_some_and(|n| self.named(n));
            s.insert(["n1", "n2"][j], i64::from(named));
            let held = xn.is_some_and(|n| untaken.contains(&n));
            s.insert(["u1", "u2"][j], i64::from(held));
        }
        let x2 = one(&slots[1][1]).1;
        let claimed = |r: &Rec| Some(r.node) == x2 && r.op == Op::Close && r.src == Src::Engine;
        s.insert("xc2", i64::from(self.log.window().any(claimed)));
        s
    }
}

/// One walk's real steps, in order, as (the model action, the state before, the state after),
/// and the laws its last step broke (none on a walk that ran to the end).
struct Walked {
    steps: Vec<(&'static str, State, State)>,
    broken: Vec<&'static str>,
}

/// Drives `host` from a fresh world through `m`'s enabled steps, picked by `seed`, until none is
/// enabled or a law breaks: each real step must be the model's.
fn walk(m: &Model, seed: u64, host: Host) -> Result<Walked, String> {
    let mut rng = XorShift::new(seed);
    let mut w = World::new();
    let mut walked = Walked {
        steps: Vec::new(),
        broken: Vec::new(),
    };
    loop {
        let prev = w.project();
        let open: Vec<&'static str> = STEPS
            .into_iter()
            .filter(|a| m.action_enabled(a, &prev))
            .collect();
        if open.is_empty() {
            break;
        }
        let step = *rng.pick(&open);
        let action = w.step(step, host);
        let next = w.project();
        if !m.successors(action, &prev).contains(&next) {
            return Err(format!(
                "seed {seed}: real `{step}` is not the model's `{action}`:\n  from {prev:?}\n  \
                 to   {next:?}\n  model {:?}\n{}",
                m.successors(action, &prev),
                lines(&w.log).join("\n")
            ));
        }
        walked.broken = m
            .invariants
            .iter()
            .filter(|inv| !m.check_invariant(inv.name, &next))
            .map(|inv| inv.name)
            .collect();
        walked.steps.push((action, prev, next));
        if !walked.broken.is_empty() {
            return Ok(walked);
        }
    }
    time_order_holds(&lines(&w.log)).map_err(|e| format!("seed {seed}: {e}"))?;
    Ok(walked)
}

#[test]
fn claimed_turn_edges_conform_to_derived_model() {
    let m = claimed_turn_edges_model();
    assert_eq!(World::new().project(), m.init_state(), "the walk's start");
    let mut ran = BTreeSet::new();
    let mut held_by = BTreeSet::new();
    for seed in 1..=400u64 {
        let walked = walk(&m, seed, Host::Feeds).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            walked.broken.is_empty(),
            "seed {seed}: the shipping tracker broke {:?}",
            walked.broken
        );
        let (_, _, last) = walked.steps.last().expect("a walk takes steps");
        assert_eq!(
            (last["d"], last["f"], last["p"]),
            (4, 4, 4),
            "seed {seed}: every edge fed and every row read"
        );
        held_by.insert((last["c1"] > 0, last["c2"] > 0, last["xc2"]));
        ran.extend(walked.steps.iter().map(|(a, ..)| *a));
    }
    for a in STEPS {
        assert!(ran.contains(a), "the walks never drove `{a}`: {ran:?}");
    }
    for (c1, c2) in [(false, false), (false, true), (true, false), (true, true)] {
        assert!(
            held_by.iter().any(|h| (h.0, h.1) == (c1, c2)),
            "no walk left span 1 to {} and span 2 to {}: {held_by:?}",
            ["the rollout", "the claim"][usize::from(c1)],
            ["the rollout", "the claim"][usize::from(c2)],
        );
    }
    assert!(
        held_by.iter().any(|h| h.2 == 1),
        "no walk had the claim close the rollout's second turn"
    );
}

#[test]
fn claimed_turn_edges_conformance_catches_a_host_that_feeds_no_beginning() {
    let m = claimed_turn_edges_model();
    let buggy = with_buggy(&m, 2);
    let (mut caught, mut refused) = (0, 0);
    for seed in 1..=400u64 {
        let walked = walk(&buggy, seed, Host::NoBeginnings).unwrap_or_else(|e| {
            panic!("NEGATIVE CONTROL: the broken host is not Buggy=2's `BeginUnfed`: {e}")
        });
        for (action, prev, next) in &walked.steps {
            if *action == "BeginUnfed" {
                let honest = if prev["f"] == 0 {
                    "FeedBegin1"
                } else {
                    "FeedBegin2"
                };
                refused += usize::from(!m.successors(honest, prev).contains(next));
            }
        }
        assert!(
            walked.broken.iter().all(|law| *law == "EveryFedTurnLogged"),
            "seed {seed}: {:?}",
            walked.broken
        );
        caught += usize::from(!walked.broken.is_empty());
    }
    assert!(
        refused > 0,
        "NEGATIVE CONTROL: the honest model admitted every unfed beginning"
    );
    assert!(
        caught > 0,
        "NEGATIVE CONTROL: no walk of a host that feeds no beginning broke EveryFedTurnLogged"
    );
}
