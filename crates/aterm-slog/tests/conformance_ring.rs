// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance: the shipping [`Log`] against the derived
//! `SessionLogRing` model (`aterm_spec::derive::session_log_ring_model`, the
//! model Tier-0 checks in `aterm-spec/tests/derived_ring_ty.rs`).
//!
//! A seeded random walk drives a real three-record ring (the model's `Cap`,
//! room for the longest cascade) through every action the model has: opens
//! and closes of a run, its turn and the turn's step (a turn closed over an
//! open step writes the cascade the model calls `CloseTurnCascade`, a run
//! closed over its turn and step `CloseRunCascade`), an orphan step (a
//! stall asked under the run with no turn open, or under a closed turn,
//! which attaches to the run `orphan=1`), marks, amends and gaps (each one
//! record, the model's `Mark`), a seamless update that carries no records
//! (`Log::after_handoff`), and a reader pulling through [`Log::read`] one
//! record at a time. After every action the real state is projected onto
//! the model's variables and the step must be one the model's own action
//! admits, with every invariant holding after it.
//!
//! The projection is the soundness pin, so the negative controls show it is
//! load-bearing: an eviction left uncounted, a reader that ignores its gap,
//! and a fold recomputed from the ring are each rejected.

use aterm_slog::{
    Amend, Close, Conf, Cursor, Fields, Fold, GapWhy, Key, Kind, Limits, Log, Lvl, Mark, Node,
    Open, Out, Rec, RunOut, Src, StepOut, TurnOut,
};
use aterm_spec::derive::{Model, session_log_ring_model};
use aterm_spec::interp::{State, with_consts};
use aterm_spec::rng::XorShift;
use aterm_spec::verify;

use crate::support::at;

/// The real ring's record bound: the model's `Cap`. Three, so a pull after
/// any action (the longest, `CloseRunCascade`, appends three) sees all of
/// it.
const CAP: usize = 3;

/// The model at the real ring's bound, with `MaxSeq` out of the walk's way.
fn model() -> Model {
    with_consts(
        &session_log_ring_model(),
        &[("Cap", CAP as i64), ("MaxSeq", 1_000_000_000)],
    )
}

fn limits() -> Limits {
    Limits::new(CAP, 1 << 20).expect("a three-record ring")
}

/// A reader pulling through `Log::read`: its epoch and cursor, what it was
/// handed and what it was told it lost.
#[derive(Clone, Copy, Debug, Default)]
struct Reader {
    ep: u32,
    cur: u64,
    got: u64,
    rlost: u64,
}

/// The real system under test plus the history the projection needs.
struct World {
    log: Log,
    ep0: u32,
    reader: Reader,
    run: Option<Node>,
    turn: Option<Node>,
    step: Option<Node>,
    /// The newest turn closed, for a step asked under it.
    closed_turn: Option<Node>,
    /// Every record this log appended, in order (each action appends at
    /// most `CAP`, so a pull after it sees all of them).
    history: Vec<Rec>,
    clock: u64,
}

impl World {
    fn new() -> World {
        World {
            log: Log::with_limits(0, limits()),
            ep0: 0,
            reader: Reader::default(),
            run: None,
            turn: None,
            step: None,
            closed_turn: None,
            history: Vec::new(),
            clock: 0,
        }
    }

    fn tick(&mut self) -> aterm_slog::Stamp {
        self.clock += 7;
        at(self.clock)
    }

    /// Pull every record appended since the last pull into `history`.
    fn record(&mut self) {
        let last = self.history.last().map_or(0, |r| r.seq);
        let read = self.log.read(Cursor::new(self.log.ep(), last), usize::MAX);
        assert!(
            read.gap.is_none(),
            "an action appended more than the ring holds: {:?}",
            read.gap
        );
        self.history.extend(read.recs.into_iter().cloned());
    }

    /// The current turn node: the newest turn this log opened.
    fn current_turn(&self) -> Option<Node> {
        self.history
            .iter()
            .rev()
            .find(|r| r.op == aterm_slog::Op::Open && r.node.lvl == Lvl::Turn)
            .map(|r| r.node)
    }

    fn project(&self) -> State {
        project_with(
            &self.log,
            self.log.state().turn().is_some(),
            self.ep0,
            &self.reader,
            &self.history,
            self.current_turn(),
        )
    }
}

/// The projection onto the model's variables. `fturn` is passed in so the
/// negative control can substitute a fold recomputed from the ring.
fn project_with(
    log: &Log,
    fold_turn: bool,
    ep0: u32,
    reader: &Reader,
    history: &[Rec],
    current_turn: Option<Node>,
) -> State {
    let open = |lvl: Lvl| {
        log.open_nodes()
            .iter()
            .filter(|e| e.node.lvl == lvl)
            .count() as i64
    };
    let turn_open = current_turn.is_some_and(|t| log.is_open(t));
    let tclose = current_turn.map_or(0, |t| {
        history
            .iter()
            .filter(|r| r.op == aterm_slog::Op::Close && r.node == t)
            .count() as i64
    });
    // The open step hangs from the run: an orphan.
    let sorph = log
        .open_nodes()
        .iter()
        .filter(|e| e.node.lvl == Lvl::Step)
        .any(|e| e.parent.lvl == Lvl::Run);
    let topen = if turn_open {
        history
            .iter()
            .find(|r| r.op == aterm_slog::Op::Open && Some(r.node) == current_turn)
            .map_or(0, |r| r.seq as i64)
    } else {
        0
    };
    [
        ("seq", log.last_seq() as i64),
        ("lo", log.oldest_seq() as i64),
        ("lost", log.lost() as i64),
        ("ep", i64::from(log.ep() - ep0)),
        ("run", open(Lvl::Run)),
        ("turn", open(Lvl::Turn)),
        ("step", open(Lvl::Step)),
        ("sorph", i64::from(sorph)),
        ("tclose", tclose),
        ("topen", topen),
        ("fturn", i64::from(fold_turn)),
        ("rep", i64::from(reader.ep - ep0)),
        ("cur", reader.cur as i64),
        ("got", reader.got as i64),
        ("rlost", reader.rlost as i64),
    ]
    .into_iter()
    .collect()
}

/// The real step `action` took from `prev` to `next` is one the model's own
/// action admits, and every invariant holds after it.
fn admitted(m: &Model, action: &str, prev: &State, next: &State) {
    assert!(
        m.successors(action, prev).contains(next),
        "real `{action}` is not the model's:\n  from {prev:?}\n  to   {next:?}\n  model {:?}",
        m.successors(action, prev)
    );
    for inv in &m.invariants {
        assert!(
            m.check_invariant(inv.name, next),
            "{} broken after real `{action}`: {next:?}",
            inv.name
        );
    }
}

/// Perform the real counterpart of the model's correct `action`; returns
/// the projected states it passed through (a read is one or two model
/// steps: the gap it crossed, then the record).
fn perform(w: &mut World, action: &'static str) -> Vec<(&'static str, State)> {
    let stamp = w.tick();
    let mut steps = Vec::new();
    match action {
        "OpenRun" => {
            w.run = Some(
                w.log
                    .open(Open::new(Kind::Agent, Src::Status, Conf::Strong), stamp)
                    .expect("run opens"),
            );
        }
        "OpenTurn" => {
            let run = w.run.expect("a run is open");
            w.turn = Some(
                w.log
                    .open(
                        Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                        stamp,
                    )
                    .expect("turn opens"),
            );
        }
        "OpenStep" => {
            let turn = w.turn.expect("a turn is open");
            let f = Fields::new().word(Key::Box, "bash").expect("a box word");
            w.step = Some(
                w.log
                    .open(
                        Open::new(Kind::Approval, Src::Screen, Conf::Strong)
                            .under(turn)
                            .fields(f),
                        stamp,
                    )
                    .expect("step opens"),
            );
        }
        "OpenOrphanStep" => {
            // Asked under the run, or under the closed turn: either way it
            // attaches to the run, marked an orphan.
            let run = w.run.expect("a run is open");
            let under = w
                .closed_turn
                .filter(|_| w.clock.is_multiple_of(2))
                .unwrap_or(run);
            let step = w
                .log
                .open(
                    Open::new(Kind::Stall, Src::Screen, Conf::Strong).under(under),
                    stamp,
                )
                .expect("an orphan step opens");
            let r = w.log.window().last().expect("its open").clone();
            assert_eq!(
                (r.node, r.par),
                (step, Some(run)),
                "an orphan hangs from the run"
            );
            assert_eq!(r.f.get_u(Key::Orphan), Some(1));
            w.step = Some(step);
        }
        "CloseStep" => {
            let step = w.step.take().expect("a step is open");
            w.log
                .close(
                    step,
                    Close::new(Out::Step(StepOut::Passed), Src::Screen, Conf::Strong),
                    stamp,
                )
                .expect("step closes");
        }
        "CloseTurn" | "CloseTurnCascade" => {
            let turn = w.turn.take().expect("a turn is open");
            if action == "CloseTurnCascade" {
                w.step = None;
            }
            w.log
                .close(
                    turn,
                    Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
                    stamp,
                )
                .expect("turn closes");
            w.closed_turn = Some(turn);
        }
        "CloseRun" | "CloseRunCascade" => {
            let run = w.run.take().expect("a run is open");
            (w.step, w.turn) = (None, None);
            let parents: Vec<(Node, Node)> = w
                .log
                .open_nodes()
                .iter()
                .filter(|e| e.node != run)
                .map(|e| (e.node, e.parent))
                .collect();
            let seq = w.log.last_seq();
            w.log
                .close(
                    run,
                    Close::new(Out::Run(RunOut::Ok), Src::Status, Conf::Strong),
                    stamp,
                )
                .expect("run closes");
            // Every open node under the run closes `out=gone`, each before
            // its parent (innermost first); the run closes last, as asked.
            let closed: Vec<(Node, &str)> = w
                .log
                .window()
                .filter(|r| r.seq > seq)
                .map(|r| (r.node, r.out().unwrap_or("?")))
                .collect();
            assert_eq!(closed.last(), Some(&(run, "ok")), "{closed:?}");
            assert_eq!(closed.len(), parents.len() + 1, "{closed:?}");
            let at = |n: Node| closed.iter().position(|c| c.0 == n);
            for (child, parent) in &parents {
                assert_eq!(at(*child).map(|i| closed[i].1), Some("gone"), "{closed:?}");
                if let (Some(c), Some(p)) = (at(*child), at(*parent)) {
                    assert!(
                        c < p,
                        "{child} closed after its parent {parent}: {closed:?}"
                    );
                }
            }
            // A turn of a closed run is no orphan's way back to a new run.
            w.closed_turn = None;
        }
        "Mark" => {
            w.log
                .mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), stamp)
                .expect("mark lands");
        }
        "Amend" => {
            // One record on an issued node: the model's `Mark`.
            let (node, kind) = w
                .history
                .iter()
                .rev()
                .find(|r| r.op == aterm_slog::Op::Open)
                .map_or((Node::SESSION, Kind::Spawn), |r| (r.node, r.k));
            let f = Fields::new().u(Key::TokIn, w.clock).expect("a count");
            w.log
                .amend(
                    node,
                    Amend::new(kind, Src::Transcript, Conf::Exact, f),
                    stamp,
                )
                .expect("an issued node amends");
        }
        "Gap" => {
            w.log.gap(GapWhy::Crash, 1, stamp);
        }
        "Handoff" => {
            let ids = w.log.id_counters();
            w.log = Log::after_handoff(w.log.ep() + 1, limits(), ids, stamp);
            w.history.clear();
            (w.run, w.turn, w.step, w.closed_turn) = (None, None, None, None);
        }
        "ReadReset" => {
            let read = w.log.read(Cursor::new(w.reader.ep, w.reader.cur), 0);
            let gap = read.gap.expect("another epoch's cursor is told so");
            assert_eq!(gap.why, GapWhy::Reset);
            w.reader = Reader {
                ep: w.log.ep(),
                ..Reader::default()
            };
            w.record();
            steps.push(("ReadReset", w.project()));
            if gap.lost > 0 {
                w.reader.rlost += gap.lost;
                w.reader.cur = gap.resync_to;
                steps.push(("ReadGap", w.project()));
            }
            return steps;
        }
        "ReadGap" | "ReadOne" => {
            let read = w.log.read(Cursor::new(w.reader.ep, w.reader.cur), 1);
            let (gap, first) = (read.gap, read.recs.first().map(|r| r.seq));
            if let Some(gap) = gap {
                assert_eq!(gap.why, GapWhy::Evicted);
                assert_eq!(gap.lost, gap.resync_to - w.reader.cur, "the gap is exact");
                w.reader.rlost += gap.lost;
                w.reader.cur = gap.resync_to;
                w.record();
                steps.push(("ReadGap", w.project()));
            }
            if let Some(seq) = first {
                assert_eq!(seq, w.reader.cur + 1, "since= is exclusive, seqs dense");
                w.reader.cur = seq;
                w.reader.got += 1;
                w.record();
                steps.push(("ReadOne", w.project()));
            }
            return steps;
        }
        other => panic!("no real counterpart for `{other}`"),
    }
    w.record();
    steps.push((model_action(action), w.project()));
    steps
}

/// The model action a real action is: an amend and a gap each append one
/// record on no open-path node, the model's `Mark`.
fn model_action(real: &'static str) -> &'static str {
    match real {
        "Amend" | "Gap" => "Mark",
        other => other,
    }
}

/// The model's correct actions (the `Buggy` mutants are dead at 0).
const ACTIONS: &[&str] = &[
    "OpenRun",
    "OpenTurn",
    "OpenStep",
    "OpenOrphanStep",
    "CloseStep",
    "CloseTurn",
    "CloseTurnCascade",
    "CloseRun",
    "CloseRunCascade",
    "Mark",
    "Amend",
    "Gap",
    "Handoff",
    "ReadReset",
    "ReadGap",
    "ReadOne",
];

/// One seeded walk of `len` actions; returns how often each action ran.
fn walk(m: &Model, seed: u64, len: usize) -> std::collections::BTreeMap<&'static str, usize> {
    let mut rng = XorShift::new(seed);
    let mut w = World::new();
    let mut ran = std::collections::BTreeMap::new();
    let mut handoffs = 0;
    for _ in 0..len {
        let prev = w.project();
        let enabled: Vec<&'static str> = ACTIONS
            .iter()
            .copied()
            // One update per walk: the model's epoch is 0 or 1.
            .filter(|a| *a != "Handoff" || handoffs == 0)
            .filter(|a| m.action_enabled(model_action(a), &prev))
            .collect();
        let action = enabled[rng.index(enabled.len())];
        if action == "Handoff" {
            handoffs += 1;
        }
        *ran.entry(action).or_insert(0usize) += 1;
        let mut from = prev;
        for (name, next) in perform(&mut w, action) {
            admitted(m, name, &from, &next);
            if name != action {
                *ran.entry(name).or_insert(0usize) += 1;
            }
            from = next;
        }
    }
    ran
}

#[test]
fn slog_ring_conforms_to_derived_model() {
    let m = model();
    let mut ran = std::collections::BTreeMap::new();
    for seed in 1..=40u64 {
        for (k, v) in walk(&m, seed, 400) {
            *ran.entry(k).or_insert(0usize) += v;
        }
    }
    for a in ACTIONS {
        assert!(
            ran.get(a).copied().unwrap_or(0) > 0,
            "the walks never drove `{a}`: {ran:?}"
        );
    }

    // The ty tier re-checks a sample of real transitions where it is
    // installed; the interpreter checked every one above.
    let mut w = World::new();
    for action in [
        "OpenRun",
        "OpenTurn",
        "OpenStep",
        "Mark",
        "CloseTurnCascade",
        "ReadGap",
        "OpenOrphanStep",
        "OpenTurn",
        "CloseRunCascade",
    ] {
        let prev = w.project();
        let mut from = prev;
        for (name, next) in perform(&mut w, action) {
            let (ok, why) = verify::validate_transition_tiered(
                &session_log_ring_model(),
                &[("Cap", CAP as i64), ("MaxSeq", 1_000_000_000)],
                &from,
                &next,
                Some(name),
                "SessionLogRing conformance",
            );
            assert!(ok, "real `{name}` rejected: {why}");
            from = next;
        }
    }
}

/// THE EVICTION POLICY, by name: `over_limits` evicts exactly where the
/// model's append does (`seq + 2 - lo > Cap`: the ring would hold more than
/// `Cap` records), for every fill of the ring.
#[test]
fn eviction_policy_is_the_models() {
    let m = model();
    for live in 0..=CAP as i64 {
        let mut s = m.init_state();
        s.insert("seq", live);
        let evicts = m
            .successors("Mark", &s)
            .first()
            .is_some_and(|next| next["lo"] > s["lo"]);
        let after = usize::try_from(live + 1).expect("small");
        assert_eq!(
            aterm_slog::over_limits(after, 0, limits()),
            evicts,
            "{live} live"
        );
    }
    // The byte bound evicts too, whatever the count.
    assert!(aterm_slog::over_limits(1, (1 << 20) + 1, limits()));
}

/// NEGATIVE CONTROLS: the projection is load-bearing.
#[test]
fn slog_ring_conformance_rejects_what_the_ring_never_does() {
    let m = model();
    let mut w = World::new();
    for action in ["OpenRun", "OpenTurn", "Mark", "Mark"] {
        let _ = perform(&mut w, action);
    }
    let prev = w.project();
    let next = perform(&mut w, "Mark").pop().expect("a step").1;
    admitted(&m, "Mark", &prev, &next);
    assert_eq!(next["lost"], 2, "the turn's open record (seq 2) is gone");

    // An eviction nobody counted.
    let mut uncounted = next.clone();
    uncounted.insert("lost", next["lost"] - 1);
    assert!(!m.successors("Mark", &prev).contains(&uncounted));
    assert!(!m.check_invariant("NoSilentLoss", &uncounted));

    // A fold recomputed from the ring: it no longer sees the open turn.
    let mut refold = Fold::new(w.log.ep());
    for r in w.log.window() {
        refold.apply(r);
    }
    assert!(
        refold.state().turn().is_none(),
        "the ring lost the turn's open"
    );
    assert!(w.log.state().turn().is_some(), "the fold beside it did not");
    let from_ring = project_with(
        &w.log,
        refold.state().turn().is_some(),
        w.ep0,
        &w.reader,
        &w.history,
        w.current_turn(),
    );
    assert!(!m.check_invariant("ProjectionIndependentOfEviction", &from_ring));
    assert!(!m.successors("Mark", &prev).contains(&from_ring));

    // A reader that skips its gap without counting it.
    let read = w.log.read(Cursor::new(0, 0), 1);
    let gap = read.gap.expect("the reader at 0 is behind");
    assert_eq!((gap.lost, gap.resync_to), (2, 2));
    let mut blind = w.reader;
    blind.cur = gap.resync_to;
    let skipped = project_with(&w.log, true, w.ep0, &blind, &w.history, w.current_turn());
    assert!(!m.check_invariant("NoSilentLoss", &skipped));
    assert!(!m.successors("ReadGap", &next).contains(&skipped));

    // A projection that forgets an orphan's parent: the stall asked with no
    // turn open hangs from the run, and reading it as a turn's step breaks
    // the nesting law.
    let mut o = World::new();
    let _ = perform(&mut o, "OpenRun");
    let before = o.project();
    let honest = perform(&mut o, "OpenOrphanStep").pop().expect("a step").1;
    admitted(&m, "OpenOrphanStep", &before, &honest);
    assert_eq!((honest["turn"], honest["step"], honest["sorph"]), (0, 1, 1));
    let mut blind = honest.clone();
    blind.insert("sorph", 0);
    assert!(!m.check_invariant("WellNested", &blind));
    assert!(!m.successors("OpenOrphanStep", &before).contains(&blind));
}
