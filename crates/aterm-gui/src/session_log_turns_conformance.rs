// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance: what the session log's feed of a codex's OSC 7501 turn edges
//! ([`SessionLog::feed_leaving`](super::SessionLog::feed_leaving),
//! [`SessionLog::feed_front`](super::SessionLog::feed_front)) makes of one foreground move, a
//! real engine's bytes read as the status sweep reads them into a real session log, against the
//! derived `ProgramEdgeFeed` model (`aterm_models_session::program_edge_feed_model`, checked at
//! Tier-0 in `aterm-models/tests/derived_ring_ty.rs`; ATERM-KI-1029-1).

use std::collections::BTreeSet;
use std::sync::Mutex;

use aterm_core::terminal::program_state::{ProgramApp, ProgramState, ProgramStateWord as W};
use aterm_models_session::program_edge_feed_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, with_buggy};
use aterm_spec::rng::XorShift;

use super::super::tests::{Engine, Hand, lines_of, opened};
use super::ProgramRead;
use crate::session_timeline::SessionTimeline;

/// The session's shell.
const ROOT: i32 = 100;
/// The group of the program in front first.
const A: i32 = 300;
/// The group that takes the front from it.
const B: i32 = 301;

/// The model's steps the walk drives: each is the real call [`World::step`] makes.
const STEPS: [&str; 19] = [
    "StartCodex",
    "StartOther",
    "WriteA",
    "Exit",
    "ExitInTurn",
    "Kill",
    "Leave",
    "ArriveCodex",
    "ArriveOther",
    "WriteB",
    "ResumeB",
    "Name",
    "SweepFront",
    "SweepMoveSplit",
    "SweepMoveKilled",
    "SweepMoveOther",
    "Hold",
    "SweepCodex",
    "SweepOther",
];

/// The host reading the engine's turn edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Host {
    /// The shipping host: every sweep's read reaches `feed_leaving` and `feed_front`.
    Feeds,
    /// The gate before the fix (`ProgramStateGate::observe_fed`), for a negative control: a sweep
    /// that sees the move, or reads under an unnamed front, consumes the edges unfed and unsaid.
    OldGate,
    /// A reader of the engine's edges alone, for a negative control: no resumed beginning.
    NoResumed,
}

fn codex(state: W) -> Option<ProgramState> {
    Some(ProgramState {
        state,
        kind: None,
        app: ProgramApp::Codex,
    })
}

/// The real engine and log, and the world's facts the walk drove: the program in front first
/// (`lk`), how it left (`ex`, `ia`), the program that took the front (`bk`), what each wrote
/// (`wa`, `wb`, and `eb`, whether B's write drew an edge the host reads), whether B was named
/// (`nm`), and the ghosts no record holds: B's turn dropped quietly (`qb`) and an ambiguous move
/// seen (`am`).
struct World {
    tl: Mutex<SessionTimeline>,
    hand: Hand,
    engine: Engine,
    host: Host,
    ms: u64,
    f: [i64; 10],
    /// The report sequence of A's and B's writes.
    a_seq: u64,
    b_seq: u64,
    /// A's agent run, when A is a codex.
    a_run: Option<String>,
}

/// The facts' names, in `World::f`'s order.
const FACTS: [&str; 10] = ["lk", "ex", "bk", "wa", "ia", "wb", "eb", "nm", "qb", "am"];

impl World {
    fn new(host: Host) -> World {
        let (tl, hand) = opened();
        let mut w = World {
            tl,
            hand,
            engine: Engine::new(),
            host,
            ms: 0,
            f: [0; 10],
            a_seq: 0,
            b_seq: 0,
            a_run: None,
        };
        w.bytes(b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\");
        w.sweep(ROOT, false);
        w.name(ROOT, "zsh");
        w.sweep(ROOT, false);
        w
    }

    fn fact(&self, name: &str) -> i64 {
        self.f[FACTS.iter().position(|n| *n == name).expect("a fact")]
    }

    fn set(&mut self, name: &str, v: i64) {
        self.f[FACTS.iter().position(|n| *n == name).expect("a fact")] = v;
    }

    /// The log's clock 10 ms on.
    fn tick(&mut self) {
        self.ms += 10;
        self.hand.set(self.ms);
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.tick();
        self.engine.bytes(&self.tl.lock().unwrap(), bytes);
    }

    fn says(&mut self, shown: Option<ProgramState>) -> u64 {
        self.tick();
        self.engine.parse(&self.tl.lock().unwrap(), shown);
        self.engine.term().program_state().seq
    }

    fn name(&self, pgid: i32, name: &str) {
        let _ = self.tl.lock().unwrap().set_program(pgid, Some(name.into()));
    }

    /// One status sweep with `pgid` in front, in the sweep's order; `old_gate`: the read is
    /// consumed unfed when the sweep sees a move or the front is unnamed. Answers whether the
    /// read was so consumed.
    fn sweep(&mut self, pgid: i32, old_gate: bool) -> bool {
        self.tick();
        let mut g = self.tl.lock().unwrap();
        let term = self.engine.term();
        let mut read = ProgramRead::of(term, g.slog().program_cursor());
        if self.host == Host::NoResumed {
            read.drop_resumed();
        }
        let skipped = old_gate && (g.agent().program_pgid != pgid || g.agent().program.is_none());
        if skipped {
            g.slog_mut().skip_program_read(&mut read);
        }
        let blocks = crate::session_status::block_read(
            term,
            g.slog().block_cursor(),
            g.slog().opened_wall(),
            &crate::command_ran::Ran::default(),
        );
        let _ = g.sweep_slog_turns(&blocks, false, ROOT, pgid, &mut read);
        let at = g.slog().now();
        g.slog_mut().program_tick(at);
        g.feed_program_turns(&mut read);
        skipped
    }

    /// The newest codex agent run's node other than `but`.
    fn codex_run(&self, but: Option<&str>) -> Option<String> {
        lines_of(&self.tl)
            .iter()
            .rev()
            .filter(|l| l.contains(" open r") && l.contains(" agent=codex "))
            .map(|l| l.split(' ').nth(2).expect("a node").to_string())
            .find(|n| Some(n.as_str()) != but)
    }

    fn step(&mut self, step: &'static str) -> &'static str {
        match step {
            "StartCodex" | "StartOther" => {
                let codex = step == "StartCodex";
                let name = if codex { "codex" } else { "less" };
                self.bytes(format!("{name}\r\n\x1b]133;C\x1b\\").as_bytes());
                self.sweep(A, false);
                self.name(A, name);
                self.sweep(A, false);
                if codex {
                    self.a_run = Some(self.codex_run(None).expect("A's run"));
                }
                self.set("lk", if codex { 1 } else { 2 });
            }
            "WriteA" => {
                self.a_seq = self.says(codex(W::Working));
                self.set("wa", 1);
            }
            "Exit" => {
                if self.fact("wa") == 1 {
                    let _ = self.says(codex(W::Done));
                }
                let _ = self.says(None);
                self.bytes(b"\x1b]133;D;0\x1b\\\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\");
                self.set("ex", 1);
            }
            "ExitInTurn" => {
                let _ = self.says(None);
                self.bytes(b"\r\n\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\");
                self.set("ex", 1);
                self.set("ia", 1);
            }
            "Kill" | "Leave" => {
                let code = if step == "Kill" { 137 } else { 0 };
                self.bytes(
                    format!("\x1b]133;D;{code}\x1b\\\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\").as_bytes(),
                );
                self.set("ex", 2);
            }
            "ArriveCodex" | "ArriveOther" => {
                let name = if step == "ArriveCodex" {
                    "codex"
                } else {
                    "less"
                };
                self.bytes(format!("{name}\r\n\x1b]133;C\x1b\\").as_bytes());
                self.set("bk", if step == "ArriveCodex" { 1 } else { 2 });
            }
            "WriteB" => {
                let _ = self.says(codex(W::Idle));
                self.b_seq = self.says(codex(W::Working));
                self.set("wb", 1);
                self.set("eb", 1);
            }
            "ResumeB" => {
                self.b_seq = self.says(codex(W::Working));
                self.set("wb", 2);
                if self.host == Host::NoResumed {
                    return "ResumeUndrawn";
                }
                self.set("eb", 1);
            }
            "Name" => {
                let name = if self.fact("bk") == 1 {
                    "codex"
                } else {
                    "less"
                };
                self.name(B, name);
                self.set("nm", 1);
            }
            "SweepFront" | "SweepMoveSplit" | "SweepMoveKilled" | "SweepMoveOther" | "Hold"
            | "SweepCodex" | "SweepOther" => {
                let pgid = if self.fact("ex") == 0 { A } else { B };
                let before = self.project();
                let skipped = self.sweep(pgid, self.host == Host::OldGate);
                let after = self.project();
                if before["mv"] == 0
                    && after["mv"] == 1
                    && self.fact("ex") == 2
                    && self.fact("lk") == 1
                {
                    self.set("am", 1);
                }
                if step == "SweepOther"
                    && before["cb"] == 0
                    && after["cb"] == 1
                    && self.fact("eb") == 1
                {
                    self.set("qb", 1);
                }
                if skipped {
                    return if step == "Hold" {
                        "UnnamedUnfed"
                    } else {
                        "MoveUnfed"
                    };
                }
            }
            other => panic!("no step `{other}`"),
        }
        step
    }

    /// The claimed turns opened under run `run`.
    fn turns_under(&self, run: Option<&str>) -> i64 {
        let Some(run) = run else {
            return 0;
        };
        let n = lines_of(&self.tl)
            .iter()
            .filter(|l| {
                l.contains(" open t")
                    && l.contains(&format!(" par={run} "))
                    && l.contains(" src=engine conf=claim ")
            })
            .count();
        i64::try_from(n).expect("a count")
    }

    /// The model's state: the world's facts; the log's view of the move (`mv`); where its cursor
    /// stands past each write (`ca`, `cb`); the claimed turns under A's and B's runs (`fa`, `fb`,
    /// and `wr` when B's run holds more than B's own edges gave it); and the turns said
    /// (`sd`: the `dropped=` the log wrote and the count it holds for the next record).
    fn project(&self) -> State {
        let mut s = State::new();
        for (name, v) in FACTS.into_iter().zip(self.f) {
            s.insert(name, v);
        }
        let g = self.tl.lock().unwrap();
        s.insert("mv", i64::from(g.agent().program_pgid == B));
        let cursor = g.slog().program_cursor().map_or(0, |c| c.seq);
        let pending = g.slog().program_dropped_pending();
        drop(g);
        let ca = self.fact("wa") == 1 && cursor >= self.a_seq;
        let cb = self.fact("wb") > 0 && cursor >= self.b_seq;
        s.insert("ca", i64::from(ca));
        s.insert("cb", i64::from(cb));
        s.insert("fa", self.turns_under(self.a_run.as_deref()));
        let b_run = self.codex_run(self.a_run.as_deref());
        let fb = self.turns_under(b_run.as_deref());
        s.insert("fb", fb);
        let own = i64::from(cb && self.fact("eb") == 1);
        s.insert("wr", i64::from(fb > own));
        let written: u64 = lines_of(&self.tl)
            .iter()
            .filter_map(|l| {
                l.split(' ')
                    .find_map(|kv| kv.strip_prefix("dropped="))
                    .and_then(|v| v.parse::<u64>().ok())
            })
            .sum();
        s.insert("sd", i64::try_from(written + pending).expect("a count"));
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
    // Spread over the generator's state: xorshift's first outputs from small seeds share their low
    // bits, so unspread, every walk's third pick of four was even and never drove `ExitInTurn`.
    let mut rng = XorShift::new(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let mut w = World::new(host);
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
        let action = w.step(step);
        let next = w.project();
        if !m.successors(action, &prev).contains(&next) {
            return Err(format!(
                "seed {seed}: real `{step}` is not the model's `{action}`:\n  from {prev:?}\n  \
                 to   {next:?}\n  model {:?}\n{}",
                m.successors(action, &prev),
                lines_of(&w.tl).join("\n")
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
    Ok(walked)
}

#[test]
fn program_edge_feed_conforms_to_derived_model() {
    let m = program_edge_feed_model();
    assert_eq!(
        World::new(Host::Feeds).project(),
        m.init_state(),
        "the walk's start"
    );
    let mut ran = BTreeSet::new();
    let mut ends = BTreeSet::new();
    for seed in 1..=300u64 {
        let walked = walk(&m, seed, Host::Feeds).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            walked.broken.is_empty(),
            "seed {seed}: the shipping host broke {:?}",
            walked.broken
        );
        let (_, _, last) = walked.steps.last().expect("a walk takes steps");
        ends.insert((last["fa"], last["fb"], last["sd"], last["qb"]));
        ran.extend(walked.steps.iter().map(|(a, ..)| *a));
    }
    for a in STEPS {
        assert!(ran.contains(a), "the walks never drove `{a}`: {ran:?}");
    }
    for (what, hit) in [
        ("fed A's turn to A's run", ends.iter().any(|e| e.0 == 1)),
        ("fed B's turn to B's run", ends.iter().any(|e| e.1 == 1)),
        ("said both turns", ends.iter().any(|e| e.2 == 2)),
        ("dropped B's turn quietly", ends.iter().any(|e| e.3 == 1)),
    ] {
        assert!(hit, "no walk {what}: {ends:?}");
    }
}

/// NEGATIVE CONTROLS: each broken host is its mutant's step, which the healthy model refuses, and
/// breaks only its law.
#[test]
fn program_edge_feed_conformance_catches_the_old_gate_and_a_reader_with_no_resumed_entry() {
    let m = program_edge_feed_model();
    for (host, dial, mutants, law) in [
        (
            Host::OldGate,
            1,
            &["MoveUnfed", "UnnamedUnfed"][..],
            "EveryConsumedFedOrSaid",
        ),
        (
            Host::NoResumed,
            6,
            &["ResumeUndrawn"][..],
            "ResumedSpanLogged",
        ),
    ] {
        let buggy = with_buggy(&m, dial);
        let (mut caught, mut refused) = (0, 0);
        for seed in 1..=300u64 {
            let walked = walk(&buggy, seed, host).unwrap_or_else(|e| {
                panic!("NEGATIVE CONTROL: the {host:?} host is not Buggy={dial}'s steps: {e}")
            });
            for (action, prev, next) in &walked.steps {
                if mutants.contains(action) {
                    refused += usize::from(
                        STEPS
                            .iter()
                            .all(|honest| !m.successors(honest, prev).contains(next)),
                    );
                }
            }
            assert!(
                walked.broken.iter().all(|l| *l == law),
                "{host:?} seed {seed}: {:?}",
                walked.broken
            );
            caught += usize::from(!walked.broken.is_empty());
        }
        assert!(
            caught > 0 && refused > 0,
            "NEGATIVE CONTROL: the {host:?} host broke {law} on {caught} walks, {refused} of its \
             steps no healthy step takes"
        );
    }
}
