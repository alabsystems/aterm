// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance tests for [`Log`] against `SessionLogRing`
//! ([`Log::after_handoff`], [`Log::carry`], [`Log::from_carry`], [`Log::class_of`],
//! [`the_over_half_rule_is_the_models_where_it_decides`]).

use aterm_models_session::session_log_ring_model;
use aterm_slog::{
    Actor, Amend, Close, Conf, EvictClass, Fields, Fold, GapWhy, Key, Kind, Limits, Log, Lvl, Mark,
    Node, Open, Out, Rec, RunOut, Src, StepOut, TurnOut,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, with_consts};
use aterm_spec::rng::XorShift;
use aterm_spec::verify;

use crate::support::at;

/// The real ring's record bound: the model's `Cap`.
const CAP: usize = 3;

/// The model's eviction actions, in no order.
const EVICTS: &[&str] = &[
    "EvictOldDetail",
    "EvictTurnDetail",
    "EvictClosedOpen",
    "EvictClose",
    "EvictTurnClose",
    "EvictCurrent",
    "EvictCurrentOut",
    "EvictDone",
    "CarryDone",
];

/// The host's overlap reserve in the walk: the model's `Reserve`.
const RESERVE: u64 = 2;

/// The carry's record bound in the walk: the model's `CarryCap`.
const CARRY_CAP: usize = 2;

/// The model at the real ring's bound, with `MaxSeq` and `After` out of the walk's way.
fn model() -> Model {
    with_consts(
        &session_log_ring_model(),
        &[
            ("Cap", CAP as i64),
            ("MaxSeq", 1_000_000_000),
            ("After", 1_000_000_000),
            ("Reserve", RESERVE as i64),
            ("CarryCap", CARRY_CAP as i64),
        ],
    )
}

fn limits() -> Limits {
    Limits::new(CAP, 1 << 20).expect("a three-record ring")
}

/// The real system under test plus the history the projection needs.
struct World {
    log: Log,
    /// The ring's bounds (the model's `Cap`).
    limits: Limits,
    run: Option<Node>,
    turn: Option<Node>,
    step: Option<Node>,
    /// Whether the open step is an orphan, and its open record's seq.
    orphan_open: Option<u64>,
    /// The newest turn opened, the newest closed, and how many opened.
    current_turn: Option<Node>,
    closed_turn: Option<Node>,
    turns: i64,
    /// Close records written for the current turn node.
    tclose: i64,
    /// The update, played as the host plays it.
    ceil: Option<u64>,
    hi: u64,
    updated: bool,
    /// The successor's first seq.
    hseq: u64,
    cpath: i64,
    moved: bool,
    clock: u64,
}

impl World {
    fn new() -> World {
        World::with_limits(limits())
    }

    /// A world whose ring has `limits` (a model instantiated at its `Cap`).
    fn with_limits(limits: Limits) -> World {
        World {
            log: Log::with_limits(0, limits),
            limits,
            run: None,
            turn: None,
            step: None,
            orphan_open: None,
            current_turn: None,
            closed_turn: None,
            turns: 0,
            tclose: 0,
            ceil: None,
            hi: 0,
            updated: false,
            hseq: 0,
            cpath: 0,
            moved: false,
            clock: 0,
        }
    }

    fn tick(&mut self) -> aterm_slog::Stamp {
        self.clock += 7;
        at(self.clock)
    }

    fn project(&self) -> State {
        project_with(
            self,
            self.log.state().turn().is_some(),
            self.log.state().totals.turns,
        )
    }
}

/// Whether `r`'s node lies at or under `root`, by the log's index.
fn under(log: &Log, r: &Rec, root: Node) -> bool {
    let mut at = r.node;
    for _ in 0..80 {
        if at == root {
            return true;
        }
        match log.parent_of(at) {
            Some(p) => at = p,
            None => return false,
        }
    }
    false
}

/// The projection onto the model's variables.
fn project_with(w: &World, fold_turn: bool, fold_turns: u32) -> State {
    let log = &w.log;
    let open = |lvl: Lvl| {
        log.open_nodes()
            .iter()
            .filter(|e| e.node.lvl == lvl)
            .count() as i64
    };
    let pinned = |lvl: Lvl| {
        log.window()
            .filter(|r| r.node.lvl == lvl && log.class_of(r) == EvictClass::Pinned)
            .count() as i64
    };
    let (mut dturn, mut dout, mut tdet, mut dold, mut ocl, mut cls) = (0, 0, 0, 0, 0, 0);
    for r in log.window() {
        match log.class_of(r) {
            EvictClass::OpenDetail => {
                if w.turn.is_some_and(|t| under(log, r, t)) {
                    dturn += 1;
                } else {
                    dout += 1;
                }
            }
            EvictClass::ClosedDetail => {
                if w.closed_turn.is_some_and(|t| under(log, r, t)) {
                    tdet += 1;
                } else {
                    dold += 1;
                }
            }
            EvictClass::ClosedOpen => ocl += 1,
            EvictClass::CloseRecord => cls += 1,
            EvictClass::Pinned => {}
        }
    }
    let close_of = |n: Option<Node>| {
        n.and_then(|n| {
            log.window()
                .find(|r| r.op == aterm_slog::Op::Close && r.node == n)
                .map(|r| r.seq)
        })
    };
    let tcl = close_of(w.closed_turn);
    let newest = log.window().last();
    let nw = newest.map_or(0, |r| match log.class_of(r) {
        EvictClass::OpenDetail if w.turn.is_some_and(|t| under(log, r, t)) => 1,
        EvictClass::OpenDetail => 2,
        EvictClass::CloseRecord if tcl == Some(r.seq) => 3,
        EvictClass::CloseRecord => 4,
        _ => 0,
    });
    let oold = match (w.orphan_open, w.run.and_then(|r| log.node_view(r))) {
        (Some(seq), Some(v)) => i64::from(seq < v.closed_below),
        _ => 0,
    };
    [
        ("seq", log.last_seq() as i64),
        ("lost", log.lost() as i64),
        ("first", log.first_seq() as i64),
        ("hi", w.hi as i64),
        ("ceil", w.ceil.map_or(0, |c| c as i64)),
        ("drawn", i64::from(w.ceil.is_some())),
        ("ep", i64::from(log.ep() > 0)),
        (
            "holes",
            log.holes().map(|(a, b)| (b - a + 1) as i64).sum::<i64>(),
        ),
        ("hseq", w.hseq as i64),
        ("cv", 0),
        ("cpath", w.cpath),
        ("moved", i64::from(w.moved)),
        ("eplast", 0),
        ("adoptions", i64::from(w.updated)),
        ("run", open(Lvl::Run)),
        ("turn", open(Lvl::Turn)),
        ("step", open(Lvl::Step)),
        ("sorph", i64::from(w.orphan_open.is_some())),
        ("oold", oold),
        ("tclose", w.tclose),
        ("tn", w.turns),
        ("fturns", i64::from(fold_turns)),
        ("fturn", i64::from(fold_turn)),
        ("prun", pinned(Lvl::Run)),
        ("pturn", pinned(Lvl::Turn)),
        ("pstep", pinned(Lvl::Step)),
        ("dturn", dturn),
        ("dout", dout),
        ("tdet", tdet),
        ("dold", dold),
        ("ocl", ocl),
        ("cls", cls),
        ("tcl", i64::from(tcl.is_some())),
        ("nw", nw),
        ("ev", 0),
    ]
    .into_iter()
    .collect()
}

/// The eviction actions some path of the model's took, by name.
type Used = std::collections::BTreeSet<&'static str>;

/// Merge state `s` and its `used` eviction actions into `into`.
fn merge(into: &mut Vec<(State, Used)>, s: State, used: Used) {
    match into.iter_mut().find(|(t, _)| *t == s) {
        Some((_, u)) => u.extend(used),
        None => into.push((s, used)),
    }
}

/// Reachable states from `prev` via `action` and its eviction batch, with actions taken.
fn after_batch_paths(m: &Model, action: &str, prev: &State) -> Vec<(State, Used)> {
    let mut done: Vec<(State, Used)> = Vec::new();
    let mut frontier: Vec<(State, Used)> = m
        .successors(action, prev)
        .into_iter()
        .map(|s| (s, Used::new()))
        .collect();
    for _ in 0..32 {
        let mut next: Vec<(State, Used)> = Vec::new();
        for (s, used) in frontier {
            if s["ev"] == 0 {
                for inv in &m.invariants {
                    assert!(
                        m.check_invariant(inv.name, &s),
                        "{} broken: {s:?}",
                        inv.name
                    );
                }
                merge(&mut done, s, used);
                continue;
            }
            for inv in &m.invariants {
                assert!(
                    m.check_invariant(inv.name, &s),
                    "{} broken mid-batch: {s:?}",
                    inv.name
                );
            }
            for e in EVICTS {
                for t in m.successors(e, &s) {
                    let mut u = used.clone();
                    u.insert(e);
                    merge(&mut next, t, u);
                }
            }
        }
        if next.is_empty() {
            return done;
        }
        frontier = next;
    }
    panic!("a batch that never ends from {prev:?} by `{action}`");
}

/// Reachable states from `prev` via `action` and its eviction batch.
fn after_batch(m: &Model, action: &str, prev: &State) -> Vec<State> {
    after_batch_paths(m, action, prev)
        .into_iter()
        .map(|(s, _)| s)
        .collect()
}

/// Assert that the step from `prev` to `next` under `action` is admitted by the model.
fn admitted(m: &Model, action: &str, prev: &State, next: &State) -> Used {
    let reach = after_batch_paths(m, action, prev);
    match reach.iter().find(|(s, _)| s == next) {
        Some((_, used)) => used.clone(),
        None => panic!(
            "real `{action}` is not the model's:\n  from {prev:?}\n  to   {next:?}\n  model {:?}",
            reach.iter().map(|(s, _)| s).collect::<Vec<_>>()
        ),
    }
}

/// Perform the real counterpart of the model's correct `action`.
fn perform(w: &mut World, action: &'static str) {
    let stamp = w.tick();
    if action.starts_with("Open") || action.starts_with("Close") {
        w.moved = true;
    }
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
            let t = w
                .log
                .open(
                    Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                    stamp,
                )
                .expect("turn opens");
            (w.turn, w.current_turn, w.tclose) = (Some(t), Some(t), 0);
            w.turns += 1;
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
            let run = w.run.expect("a run is open");
            let asked = w
                .closed_turn
                .filter(|t| w.clock.is_multiple_of(2) && w.log.parent_of(*t) == Some(run))
                .unwrap_or(run);
            let step = w
                .log
                .open(
                    Open::new(Kind::Stall, Src::Screen, Conf::Strong).under(asked),
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
            w.orphan_open = Some(r.seq);
        }
        "CloseStep" => {
            let step = w.step.take().expect("a step is open");
            w.orphan_open = None;
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
            w.tclose += 1;
        }
        "CloseRun" | "CloseRunCascade" => {
            let run = w.run.take().expect("a run is open");
            let had_turn = w.turn.take();
            (w.step, w.orphan_open) = (None, None);
            let (seq, open_before) = (w.log.last_seq(), w.log.open_nodes().len() as u64);
            w.log
                .close(
                    run,
                    Close::new(Out::Run(RunOut::Ok), Src::Status, Conf::Strong),
                    stamp,
                )
                .expect("run closes");
            if let Some(t) = had_turn {
                w.closed_turn = Some(t);
                w.tclose += 1;
            }
            assert_eq!(w.log.last_seq() - seq, open_before);
            assert!(
                w.log.open_nodes().is_empty(),
                "the cascade closed everything"
            );
        }
        "Mark" | "Amend" => {
            if action == "Mark" {
                w.log
                    .mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), stamp)
                    .expect("mark lands");
            } else {
                let (node, kind) = w
                    .log
                    .open_nodes()
                    .last()
                    .map_or((Node::SESSION, Kind::Spawn), |e| (e.node, e.kind));
                let f = Fields::new().u(Key::TokIn, w.clock).expect("a count");
                w.log
                    .amend(
                        node,
                        Amend::new(kind, Src::Transcript, Conf::Exact, f),
                        stamp,
                    )
                    .expect("an open node amends");
            }
        }
        "MarkOutside" | "Gap" | "Human" => match action {
            "Gap" => {
                w.log.gap(GapWhy::Crash, 1, stamp);
            }
            "Human" => {
                w.log
                    .mark(
                        Mark::new(Kind::Human, Src::Input, Conf::Strong)
                            .at(Node::SESSION)
                            .by(Actor::Human),
                        stamp,
                    )
                    .expect("a session mark");
            }
            _ => {
                let at_node = w.run.unwrap_or(Node::SESSION);
                let f = Fields::new().word(Key::Word, "busy").expect("a word");
                w.log
                    .mark(
                        Mark::new(Kind::Agent, Src::Screen, Conf::Strong)
                            .at(at_node)
                            .fields(f),
                        stamp,
                    )
                    .expect("a verdict mark");
            }
        },
        "Draw" => w.ceil = Some(w.log.last_seq() + RESERVE),
        "StandDown" => w.ceil = None,
        "HandoffUncarried" => {
            let ceil = w.ceil.take().expect("a drawn manifest");
            assert!(w.log.last_seq() <= ceil, "the Commit stands");
            w.hi = w.log.last_seq();
            let ids = w.log.id_counters();
            w.log = Log::after_handoff(w.log.ep() + 1, w.limits, ids, ceil + 1, stamp);
            (w.run, w.turn, w.step, w.orphan_open) = (None, None, None, None);
            (w.current_turn, w.closed_turn, w.turns, w.tclose) = (None, None, 0, 0);
            w.updated = true;
            w.hseq = ceil + 1;
        }
        "Crash" => {
            let ep = w.log.ep() + 1;
            w.log = Log::with_limits(ep, w.limits);
            w.log.gap(GapWhy::Crash, 0, stamp);
            (w.run, w.turn, w.step, w.orphan_open) = (None, None, None, None);
            (w.current_turn, w.closed_turn, w.turns, w.tclose) = (None, None, 0, 0);
            (w.ceil, w.hi, w.hseq, w.updated) = (None, 0, 1, true);
        }
        "HandoffCarried" => {
            let ceil = w.ceil.take().expect("a drawn manifest");
            assert!(w.log.last_seq() <= ceil, "the Commit stands");
            w.hi = w.log.last_seq();
            let code = |n: Option<Node>, k: i64| i64::from(n.is_some()) * k;
            w.cpath = 1 + code(w.run, 1) + code(w.turn, 2) + code(w.step, 4);
            let out = w
                .log
                .carry(
                    aterm_slog::CarryBudget {
                        records: CARRY_CAP,
                        ..aterm_slog::CarryBudget::SESSION
                    },
                    &[],
                )
                .expect("the model admitted the mandatory records within CarryCap");
            let a = aterm_slog::CarryAdopt {
                ep: w.log.ep() + 1,
                next_seq: ceil + 1,
                ids: w.log.id_counters(),
                limits: w.limits,
                at: stamp,
            };
            w.log = Log::from_carry(&out.lines, a).expect("the carry reads").0;
            w.updated = true;
            w.hseq = ceil + 1;
            w.moved = false;
        }
        other => panic!("no real counterpart for `{other}`"),
    }
}

/// The model action a real action is.
fn model_action(real: &'static str) -> &'static str {
    match real {
        "Amend" => "Mark",
        "Gap" | "Human" => "MarkOutside",
        other => other,
    }
}

/// The model's correct appends, with their real variants.
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
    "MarkOutside",
    "Gap",
    "Human",
    "Draw",
    "StandDown",
    "HandoffUncarried",
    "HandoffCarried",
    "Crash",
];

/// One seeded walk of `len` actions; returns how often each ran.
fn walk(m: &Model, seed: u64, len: usize) -> std::collections::BTreeMap<&'static str, usize> {
    let mut rng = XorShift::new(seed);
    let mut w = World::new();
    let mut ran = std::collections::BTreeMap::new();
    for _ in 0..len {
        let prev = w.project();
        let enabled: Vec<&'static str> = ACTIONS
            .iter()
            .copied()
            .filter(|a| m.action_enabled(model_action(a), &prev))
            .collect();
        let enabled: Vec<&'static str> = if w.clock < 2000 {
            enabled
                .into_iter()
                .filter(|a| !matches!(*a, "Draw" | "HandoffUncarried" | "HandoffCarried" | "Crash"))
                .collect()
        } else {
            enabled
        };
        let action = enabled[rng.index(enabled.len())];
        *ran.entry(action).or_insert(0usize) += 1;
        perform(&mut w, action);
        let next = w.project();
        for e in admitted(m, model_action(action), &prev, &next) {
            *ran.entry(e).or_insert(0usize) += 1;
        }
        if next["lost"] > prev["lost"] {
            *ran.entry("(evicted)").or_insert(0usize) += 1;
        }
    }
    ran
}

#[test]
fn slog_ring_conforms_to_derived_model() {
    let m = model();
    let mut too_wide = World::new();
    for action in ["OpenRun", "OpenTurn", "OpenStep", "Draw"] {
        perform(&mut too_wide, action);
    }
    let before = too_wide.project();
    assert!(!m.action_enabled("HandoffCarried", &before));
    assert!(
        too_wide
            .log
            .carry(
                aterm_slog::CarryBudget {
                    records: CARRY_CAP,
                    ..aterm_slog::CarryBudget::SESSION
                },
                &[]
            )
            .is_none()
    );
    assert!(m.action_enabled("HandoffUncarried", &before));
    perform(&mut too_wide, "HandoffUncarried");
    admitted(&m, "HandoffUncarried", &before, &too_wide.project());
    let mut ran = std::collections::BTreeMap::new();
    let mut carried = World::new();
    for action in ["OpenRun", "Mark", "Mark", "Draw", "HandoffCarried"] {
        let prev = carried.project();
        perform(&mut carried, action);
        for observed in admitted(&m, model_action(action), &prev, &carried.project()) {
            *ran.entry(observed).or_insert(0usize) += 1;
        }
        *ran.entry(action).or_insert(0usize) += 1;
    }
    let seeds: Vec<u64> = (1..=40).collect();
    for walked in aterm_spec::check::each(&seeds, |&seed| walk(&m, seed, 400)) {
        for (k, v) in walked {
            *ran.entry(k).or_insert(0usize) += v;
        }
    }
    for a in ACTIONS.iter().chain(EVICTS).chain(&["(evicted)"]) {
        assert!(
            ran.get(a).copied().unwrap_or(0) > 0,
            "the walks never drove `{a}`: {ran:?}"
        );
    }
    eprintln!("ring conformance coverage: {ran:?}");

    let mut w = World::new();
    for action in ["OpenRun", "OpenTurn", "OpenStep"] {
        let from = w.project();
        perform(&mut w, action);
        let next = w.project();
        let (ok, why) = verify::validate_transition_tiered(
            &session_log_ring_model(),
            &[
                ("Cap", CAP as i64),
                ("MaxSeq", 1_000_000_000),
                ("After", 1_000_000_000),
            ],
            &from,
            &next,
            Some(model_action(action)),
            "SessionLogRing conformance",
        );
        assert!(ok, "real `{action}` rejected: {why}");
    }
}

/// THE POLICY FUNCTIONS, by name: a batch starts where the model's append
/// arms one (`over_limits`, the ring past `Cap`) and stops at the model's
/// `Low` (`low_water`), and closes go before current detail exactly when
/// the model's `Hf` says they pass half (`closes_over_half`).
#[test]
fn eviction_policy_is_the_models() {
    let low = aterm_slog::low_water(limits());
    assert_eq!(low.records, 2, "the model's Low");
    for live in 0..=CAP + 2 {
        assert_eq!(aterm_slog::over_limits(live, 0, limits()), live > CAP);
    }
    assert!(aterm_slog::over_limits(1, (1 << 20) + 1, limits()));
    for closes in 0..=CAP {
        assert_eq!(
            aterm_slog::closes_over_half(closes, 0, limits()),
            2 * closes > CAP,
            "{closes}"
        );
    }
}

/// THE OVER-HALF RULE IS THE MODEL'S, WHERE IT DECIDES: a three-record ring
/// evicts two records a batch from at most three candidates, so closes and
/// current detail are taken alike whichever goes first, and the rule never
/// shows in the walk above. At the model's `Cap = 6` (`Low = 5`), five runs
/// opened and closed, then session marks: a batch faces four closes (past
/// half) and two current marks, and takes one close, then (the closes no
/// longer past half) one mark. The real ring's step is the model's, by
/// `EvictClose` then `EvictCurrentOut`. NEGATIVE CONTROL: the ring a rule
/// without the half guard leaves (both marks taken, every close kept) is
/// one the model never reaches, and so is the one that takes two closes.
#[test]
fn the_over_half_rule_is_the_models_where_it_decides() {
    const CAP6: usize = 6;
    let m = with_consts(
        &session_log_ring_model(),
        &[
            ("Cap", CAP6 as i64),
            (
                "Low",
                aterm_slog::low_water(Limits::new(CAP6, 1 << 20).expect("six")).records as i64,
            ),
            ("MaxSeq", 1_000_000_000),
            ("After", 1_000_000_000),
            ("Reserve", RESERVE as i64),
            ("CarryCap", CARRY_CAP as i64),
        ],
    );
    let mut w = World::with_limits(Limits::new(CAP6, 1 << 20).expect("a six-record ring"));
    let script = ["OpenRun", "CloseRun"].repeat(5);
    for action in script.iter().copied().chain(["Human", "Human"]) {
        let prev = w.project();
        perform(&mut w, action);
        admitted(&m, model_action(action), &prev, &w.project());
    }
    let prev = w.project();
    assert_eq!(
        (prev["cls"], prev["dout"]),
        (4, 2),
        "PRECONDITION: four closes and two marks live"
    );
    perform(&mut w, "Human");
    let next = w.project();
    let used = admitted(&m, "MarkOutside", &prev, &next);
    assert_eq!(
        (next["cls"], next["dout"]),
        (3, 2),
        "one close, then one mark"
    );
    assert!(
        used.contains("EvictClose") && used.contains("EvictCurrentOut"),
        "{used:?}"
    );
    let reach = after_batch(&m, "MarkOutside", &prev);
    let mut detail_first = next.clone();
    detail_first.insert("cls", 4);
    detail_first.insert("dout", 1);
    assert!(!reach.contains(&detail_first), "the guard is load-bearing");
    let mut closes_only = next.clone();
    closes_only.insert("cls", 2);
    closes_only.insert("dout", 3);
    assert!(!reach.contains(&closes_only));
}

/// NEGATIVE CONTROLS: the projection is load-bearing.
#[test]
fn slog_ring_conformance_rejects_what_the_ring_never_does() {
    let m = model();
    let mut w = World::new();
    for action in ["OpenRun", "OpenTurn", "Mark"] {
        perform(&mut w, action);
    }
    let prev = w.project();
    perform(&mut w, "CloseTurn");
    let next = w.project();
    admitted(&m, "CloseTurn", &prev, &next);
    assert_eq!(
        (
            next["tdet"],
            next["ocl"],
            next["cls"],
            next["tcl"],
            next["lost"]
        ),
        (0, 0, 1, 1, 2),
        "the turn's detail and open went; its close stayed"
    );

    let mut uncounted = next.clone();
    uncounted.insert("lost", next["lost"] - 1);
    assert!(!after_batch(&m, "CloseTurn", &prev).contains(&uncounted));
    assert!(!m.check_invariant("NoSilentLoss", &uncounted));

    let mut early = next.clone();
    early.insert("tdet", 1);
    early.insert("tcl", 0);
    assert!(!after_batch(&m, "CloseTurn", &prev).contains(&early));
    assert!(!m.check_invariant("RollupOutlivesDetail", &early));

    let mut o = World::new();
    for action in ["OpenRun", "OpenTurn", "Mark", "Mark"] {
        perform(&mut o, action);
    }
    let before = o.project();
    perform(&mut o, "Mark");
    let honest = o.project();
    admitted(&m, "Mark", &before, &honest);
    assert_eq!(
        (honest["pturn"], honest["prun"]),
        (1, 1),
        "the open path stays"
    );
    let mut taken = honest.clone();
    taken.insert("pturn", 0);
    taken.insert("dturn", honest["dturn"] + 1);
    assert!(!m.check_invariant("OpenPathRetained", &taken));
    assert!(!after_batch(&m, "Mark", &before).contains(&taken));

    let mut refold = Fold::new(w.log.ep());
    for r in w.log.window() {
        refold.apply(r);
    }
    assert_eq!(
        refold.state().totals.turns,
        0,
        "the ring lost the turn's open"
    );
    assert_eq!(w.log.state().totals.turns, 1, "the fold beside it did not");
    let from_ring = project_with(&w, false, refold.state().totals.turns);
    assert!(!m.check_invariant("ProjectionIndependentOfEviction", &from_ring));
    assert!(!after_batch(&m, "CloseTurn", &prev).contains(&from_ring));

    let mut c = World::new();
    for action in ["OpenRun", "OpenTurn", "Draw"] {
        perform(&mut c, action);
    }
    let before = c.project();
    perform(&mut c, "HandoffCarried");
    let honest = c.project();
    admitted(&m, "HandoffCarried", &before, &honest);
    assert_eq!(
        (honest["run"], honest["turn"], honest["ep"]),
        (1, 1, 1),
        "open across it"
    );
    let mut forgot = honest.clone();
    for (k, v) in [
        ("run", 0),
        ("turn", 0),
        ("prun", 0),
        ("pturn", 0),
        ("fturn", 0),
    ] {
        forgot.insert(k, v);
    }
    forgot.insert("lost", honest["lost"] + 2);
    assert!(!m.check_invariant("CarryKeepsOpenPath", &forgot));
    assert!(!after_batch(&m, "HandoffCarried", &before).contains(&forgot));

    let mut k = World::new();
    for action in ["OpenRun", "Mark"] {
        perform(&mut k, action);
    }
    let before = k.project();
    perform(&mut k, "Crash");
    admitted(&m, "Crash", &before, &k.project());
    let stamp = k.tick();
    k.log = Log::with_limits(0, limits());
    k.log.gap(GapWhy::Crash, 0, stamp);
    let same = k.project();
    assert_eq!(same["ep"], 0, "the epoch did not rise");
    assert!(!m.check_invariant("EpochGrows", &same));
    assert!(!after_batch(&m, "Crash", &before).contains(&same));
}
