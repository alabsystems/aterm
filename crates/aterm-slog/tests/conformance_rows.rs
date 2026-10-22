// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance: the timeline's rows ([`Log::keep_rows`], the row
//! counter and the row floor in `Log::evict_batch`) against the derived
//! `SessionLogRows` model (`aterm_models_session::session_log_rows_model`,
//! checked at Tier-0 in `aterm-models/tests/derived_ring_ty.rs`).
//!
//! A seeded walk drives a real ring of eight records (a floor of two rows)
//! under a session and an open run, both opened before the walk, through a
//! test grammar: a bell is no row, a directory mark one, and a handoff mark
//! or a handoff or crash gap one (a record of two rows, a session close, is
//! `row_floor.rs`'s). It appends, then takes at most one update or adoption: a carried
//! update (the successor's counter continued past the predecessor's head
//! plus the overlap's reserve), an uncarried one, or a crash adoption at a
//! new epoch. After every step the live records are projected onto the
//! model (the records that are no rows, the rows outside the floor and
//! within it, the newest's class, the counter, the log's own `lost`), and
//! the step must be the model's: the action, then whatever the batch took.
//! NEGATIVE CONTROLS, each its mutant's successor (T11): a successor whose
//! counter is not carried, a crash adoption at its predecessor's epoch (an
//! epoch the world picks: the projection's check; the host's own pick is
//! aterm-gui's test), a carried update that dropped a record uncounted, and
//! a batch that took a floor row while a bell could go; and a projection
//! that miscounts a row as a bell.

use aterm_models_session::session_log_rows_model;
use aterm_slog::{
    Actor, CarryAdopt, CarryBudget, Conf, Fields, GapWhy, Key, Kind, Limits, Log, Mark, Node, Op,
    Open, Rec, Src, row_floor,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, with_buggy};
use aterm_spec::rng::XorShift;

use crate::support::at;

/// The model's appends.
const MAX_SEQ: i64 = 9;
/// The overlap's reserve the world carries its count with (the model's).
const RESERVE: u64 = 1;
/// The floor of the world's ring.
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
        // The world's opens (the session's, the run's) are pinned: no
        // record of the walk's.
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

/// The broken state `broken` is no state the honest `action` (and the
/// batch after it) makes of `prev`, and it is exactly what mutant `mutant`
/// makes of `from` (T11: each negative control is its mutant's successor,
/// so the conformance check would catch that mutant's call in the code).
/// An append's mutant steps from `prev`; a batch's from the honest state
/// the batch left.
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

/// `s` with each `(key, delta)` added.
fn moved(s: &State, deltas: &[(&'static str, i64)]) -> State {
    let mut out = s.clone();
    for (k, d) in deltas {
        *out.get_mut(k).unwrap_or_else(|| panic!("{k}")) += d;
    }
    out
}

/// A world with two rows in the floor and bells after them, the next bell
/// running a batch: (the world, its state before that bell).
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

/// NEGATIVE CONTROLS on the update: a carried update whose successor's
/// counter is not carried (`HandoffRowsRestart`, a real call: its first
/// row takes id 1, which the predecessor's rows hold), a crash adoption at
/// its predecessor's epoch (`CrashKeepsEpoch`: the log built at that
/// epoch, its ids starting over in the same id space, which the world's
/// projection reads off the two epochs; the epoch is the host's choice,
/// so this is a check of the projection and the model, and the host's
/// own choice, `Epochs::crash`, is aterm-gui's
/// `a_keeper_recovery_names_its_rows_apart_by_epoch`), and a carried
/// update that dropped a record nobody counted (`CarryShedUncounted`).
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

/// NEGATIVE CONTROLS on the batch: one that also took a floor row while a
/// bell could go (`EvictRowUnderFloor`, its step from the state the honest
/// batch left), and a projection that miscounts a row as a bell.
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
    // A row counted as a bell (the projection's own miscount: no mutant
    // makes it, and no honest step does).
    let miscounted = moved(&honest, &[("rf", -1), ("u", 1)]);
    assert!(!reach(&m, "AppendOther", &prev).contains(&miscounted));
}
