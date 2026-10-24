// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance: the timeline's rows ([`Log::keep_rows`], the row counter and the row floor
//! in `Log::evict_batch`) against the derived `SessionLogRows` model
//! (`aterm_models_session::session_log_rows_model`, checked at Tier-0 in
//! `aterm-models/tests/derived_ring_ty.rs`).

use aterm_models_session::session_log_rows_model;
use aterm_slog::{
    Actor, CarryAdopt, CarryBudget, Conf, Fields, GapWhy, Key, Kind, Limits, Log, Mark, Node, Op,
    Open, Rec, Src, row_floor,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, with_buggy};
use aterm_spec::rng::XorShift;

use crate::support::at;

const MAX_SEQ: i64 = 9;
const RESERVE: u64 = 1;
const FLOOR: usize = 2;

/// The test grammar: a directory mark one row, a handoff mark and a handoff
/// or crash gap one; nothing else.
fn rule(r: &Rec) -> u8 {
    match (r.op, r.k) {
        (Op::Mark, Kind::Cwd | Kind::Handoff) => 1,
        (Op::Gap, _) => u8::from(matches!(r.f.get_word(Key::Why), Some("handoff" | "crash"))),
        _ => 0,
    }
}

fn limits() -> Limits {
    Limits::new(8, 1 << 20).expect("a small ring")
}

struct World {
    log: Log,
    clock: u64,
    /// The records this log accounts for (the model's `seq`).
    seq: i64,
    /// The highest id an earlier process of this id space may hold.
    rprev: u64,
    /// Whether the update or adoption happened.
    ups: i64,
}

impl World {
    fn new() -> World {
        assert_eq!(row_floor(limits()).records, FLOOR, "the model's floor");
        let mut log = Log::with_limits(1, limits());
        log.open_session(
            Kind::Spawn,
            Src::Aterm,
            Conf::Exact,
            Actor::Aterm,
            Fields::new(),
            at(1),
        )
        .expect("the session opens");
        log.open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(2))
            .expect("a run opens");
        log.keep_rows(rule, 0);
        World {
            log,
            clock: 2,
            seq: 0,
            rprev: 0,
            ups: 0,
        }
    }

    fn project(&self) -> State {
        let rows: std::collections::BTreeSet<u64> = self.log.rows().map(|r| r.rec.seq).collect();
        let walk: Vec<&Rec> = self.log.window().filter(|r| r.op != Op::Open).collect();
        let nrows = walk.iter().filter(|r| rows.contains(&r.seq)).count();
        let rf = nrows.min(FLOOR);
        let nw = match self.log.window().last() {
            Some(r) if r.op == Op::Open => 0,
            Some(r) if rows.contains(&r.seq) => 2,
            Some(_) => 1,
            None => 0,
        };
        let mut s = State::new();
        s.insert("seq", self.seq);
        s.insert("lost", self.log.lost() as i64);
        s.insert("u", (walk.len() - nrows) as i64);
        s.insert("ro", (nrows - rf) as i64);
        s.insert("rf", rf as i64);
        s.insert("nw", nw);
        s.insert("rid", self.log.row_head() as i64);
        s.insert("rfirst", self.log.row_first() as i64);
        s.insert("rapp", self.log.rows_issued() as i64);
        s.insert("rprev", self.rprev as i64);
        s.insert("ups", self.ups);
        s.insert("under", 0);
        s
    }

    fn mark(&mut self, kind: Kind) {
        self.clock += 5;
        let f = match kind {
            Kind::Cwd => Fields::new().text(Key::Cwd, "/private/tmp/x"),
            _ => Ok(Fields::new()),
        }
        .expect("a field");
        self.log
            .mark(
                Mark::new(kind, Src::Engine, Conf::Exact)
                    .at(Node::SESSION)
                    .fields(f),
                at(self.clock),
            )
            .expect("a mark");
        self.seq += 1;
    }

    /// The carried update, its successor's count continued at `carried`
    /// (`None`: the predecessor's head plus the reserve, as a host carries
    /// it).
    fn handoff_carried(&mut self, carried: Option<u64>) {
        self.clock += 5;
        let ceiling = self.log.row_head() + RESERVE;
        let lines = self
            .log
            .carry(CarryBudget::SESSION, &[])
            .expect("the carry fits")
            .lines;
        let (mut next, _, _) = Log::from_carry(
            &lines,
            CarryAdopt {
                ep: self.log.ep() + 1,
                next_seq: self.log.last_seq() + 1,
                ids: self.log.id_counters(),
                limits: limits(),
                at: at(self.clock),
            },
        )
        .expect("the carry reads");
        next.keep_rows(rule, carried.unwrap_or(ceiling));
        self.log = next;
        self.seq += 1;
        self.rprev = ceiling;
        self.ups = 1;
    }

    fn handoff_uncarried(&mut self) {
        self.clock += 5;
        let ceiling = self.log.row_head() + RESERVE;
        let mut next = Log::after_handoff(
            self.log.ep() + 1,
            limits(),
            self.log.id_counters(),
            self.log.last_seq() + 1,
            at(self.clock),
        );
        next.keep_rows(rule, ceiling);
        self.log = next;
        self.seq = 1;
        self.rprev = ceiling;
        self.ups = 1;
    }

    /// A crash adoption at epoch `ep` (a keeper tag's: above every earlier
    /// one); the predecessor's ids are this one's to avoid only when it
    /// keeps the predecessor's epoch.
    fn crash(&mut self, ep: u32) {
        self.clock += 5;
        let same = ep == self.log.ep();
        let head = self.log.row_head();
        let mut next = Log::with_limits(ep, limits());
        next.gap(GapWhy::Crash, 0, at(self.clock));
        next.keep_rows(rule, 0);
        self.log = next;
        self.seq = 1;
        self.rprev = if same { head } else { 0 };
        self.ups = 1;
    }
}

/// The states the model reaches from `prev` by `action`, then any evictions.
fn reach(m: &Model, action: &str, prev: &State) -> Vec<State> {
    let mut all: Vec<State> = m.successors(action, prev);
    let mut i = 0;
    while i < all.len() {
        for e in ["EvictOther", "EvictOutside", "EvictFloor"] {
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
    let room = w.seq < MAX_SEQ;
    match rng.below(10) {
        0..=2 if room => {
            w.mark(Kind::Bell);
            Some("AppendOther")
        }
        3..=6 if room => {
            w.mark(Kind::Cwd);
            Some("AppendRow")
        }
        7 if w.ups == 0 && room => {
            w.handoff_carried(None);
            Some("HandoffCarried")
        }
        8 if w.ups == 0 => {
            w.handoff_uncarried();
            Some("HandoffUncarried")
        }
        9 if w.ups == 0 => {
            let ep = w.log.ep() + 5;
            w.crash(ep);
            Some("Crash")
        }
        _ => None,
    }
}

#[test]
fn slog_rows_conform_to_derived_model() {
    let m = session_log_rows_model();
    assert_eq!(World::new().project(), m.init_state(), "the walk's start");
    let mut ran = std::collections::BTreeSet::new();
    let (mut took, mut outside) = (0, 0);
    for seed in 1..=600u64 {
        let mut rng = XorShift::new(seed);
        let mut w = World::new();
        for _ in 0..24 {
            let prev = w.project();
            if let Some(action) = step(&mut w, &mut rng) {
                let next = w.project();
                admitted(&m, action, &prev, &next);
                ran.insert(action);
                outside += i64::from(next["lost"] > prev["lost"] && prev["ro"] > next["ro"]);
            }
        }
        took += w.log.lost();
    }
    for a in [
        "AppendOther",
        "AppendRow",
        "HandoffCarried",
        "HandoffUncarried",
        "Crash",
    ] {
        assert!(ran.contains(a), "the walks never drove `{a}`: {ran:?}");
    }
    assert!(took > 0, "no batch ever ran");
    assert!(outside > 0, "no batch took a row outside the floor");
}

/// Asserts `broken` is unreachable by `action` from `prev` and is `mutant`'s successor from `from`.
fn rejected(m: &Model, action: &str, prev: &State, (mutant, from): (&str, &State), broken: &State) {
    assert!(
        !reach(m, action, prev).contains(broken),
        "the broken `{action}` passed as the model's: {broken:?}"
    );
    assert!(
        with_buggy(m, 1).successors(mutant, from).contains(broken),
        "`{mutant}` is not the broken call it names:\n  from {from:?}\n  to   {broken:?}"
    );
}

fn moved(s: &State, deltas: &[(&'static str, i64)]) -> State {
    let mut out = s.clone();
    for (k, d) in deltas {
        *out.get_mut(k).unwrap_or_else(|| panic!("{k}")) += d;
    }
    out
}

fn floor_world() -> (World, State) {
    let mut w = World::new();
    w.mark(Kind::Cwd);
    w.mark(Kind::Cwd);
    for _ in 0..4 {
        w.mark(Kind::Bell);
    }
    let prev = w.project();
    w.mark(Kind::Bell);
    (w, prev)
}

/// Negative controls for `CarryShedUncounted`, `HandoffRowsRestart`, and `CrashKeepsEpoch`.
#[test]
fn slog_rows_conformance_rejects_each_broken_update() {
    let m = session_log_rows_model();
    let mut w = World::new();
    w.mark(Kind::Cwd);
    w.mark(Kind::Bell);
    w.mark(Kind::Cwd);
    let prev = w.project();
    let mut honest = World::new();
    honest.mark(Kind::Cwd);
    honest.mark(Kind::Bell);
    honest.mark(Kind::Cwd);
    honest.handoff_carried(None);
    let carried = honest.project();
    admitted(&m, "HandoffCarried", &prev, &carried);
    let shed = moved(&carried, &[("u", -1)]);
    rejected(
        &m,
        "HandoffCarried",
        &prev,
        ("CarryShedUncounted", &prev),
        &shed,
    );
    w.handoff_carried(Some(0));
    let restarted = w.project();
    assert_eq!((restarted["rid"], restarted["rfirst"]), (1, 1));
    rejected(
        &m,
        "HandoffCarried",
        &prev,
        ("HandoffRowsRestart", &prev),
        &restarted,
    );

    let mut c = World::new();
    c.mark(Kind::Cwd);
    c.mark(Kind::Cwd);
    let prev = c.project();
    let ep = c.log.ep();
    c.crash(ep);
    let same_epoch = c.project();
    assert_eq!((same_epoch["rid"], same_epoch["rprev"]), (1, 2));
    rejected(&m, "Crash", &prev, ("CrashKeepsEpoch", &prev), &same_epoch);
}

/// Negative controls for `EvictRowUnderFloor` and row-versus-other miscounting.
#[test]
fn slog_rows_conformance_rejects_a_floor_row_taken_early_and_a_miscount() {
    let m = session_log_rows_model();
    let (w, prev) = floor_world();
    let honest = w.project();
    admitted(&m, "AppendOther", &prev, &honest);
    assert_eq!(
        (honest["rf"], honest["u"], honest["lost"]),
        (2, 3, 2),
        "the batch took the two oldest bells and kept both rows"
    );
    let under = moved(&honest, &[("rf", -1), ("lost", 1), ("under", 1)]);
    rejected(
        &m,
        "AppendOther",
        &prev,
        ("EvictRowUnderFloor", &honest),
        &under,
    );
    let miscounted = moved(&honest, &[("rf", -1), ("u", 1)]);
    assert!(!reach(&m, "AppendOther", &prev).contains(&miscounted));
}
