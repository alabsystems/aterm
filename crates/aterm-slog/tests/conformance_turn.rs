// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance: the shipping [`TurnTracker`] (over a real [`Log`])
//! against the derived `TurnTracker` model
//! (`aterm_models_session::turn_tracker_model`, checked at Tier-0 in
//! `aterm-models/tests/derived_ring_ty.rs`).
//!
//! Seeded random host schedules, and the hand-written replays of
//! `turn_replays.rs`, drive the real tracker the way the host does: one call
//! per verdict, time passing in half-second steps (the model's `Tick`), and
//! wakes that are not tied to the deadline: early (nothing changes), on
//! time and late (time passes with no wake, and the next call, a verdict
//! included, applies the settle first: the model's `Rest` then the verdict).
//! A `ctl turn` while the agent rests is the model's `CtlTurn`. The
//! transcript (phase 3B) drives the same tracker: a vendor turn's prompt and
//! end on a measured build (`TxClose` when no screen step holds the turn,
//! else only its amends), and the wall judge refuting the screen's wall
//! (`TxRefute`). After every
//! step the real state is projected onto the model's variables, the step
//! must be a path of the model's actions with every invariant holding, and
//! the turn records the step appended must be exactly the turn opens and
//! closes along that path: a tracker that splits one turn into two, or
//! supersedes its own turn, is not the model's.
//!
//! Inputs (phase 5, ruling T1): the host ADMITS an input under the lock it
//! takes before a byte (`TurnTracker::admit`) and ACCEPTS it after the write
//! (`TurnTracker::accept`), the model's `AdmitCtl` or `AdmitHarness` (by
//! the input's actor, phase 6, 6A-5) and `Accept`, one in flight
//! at a time; a person's input is both at once (`Person`); time passing
//! past `STEP_ACT_MS` and `STEP_INPUT_LEAD_MS` is its `Age` and `LeadOut`.
//! The rig keeps the model's ghosts from what it fed and what the log
//! shows: an input bound to a step it was neither admitted under nor led to
//! (`gn`), a binding still held after it attributed a close (`ab`), a close
//! read approved, declined or answered with no fresh, proven, acted input
//! bound to it (`ga`), and who sent the input the open step is bound to
//! (`bs`), which the tracker's binding must name (`ba`).
//!
//! NEGATIVE CONTROLS: the Codex mid-answer idle closed at once, an early
//! wake that closes, a turn closed on `unknown`, a projection that forgets
//! a step's parent, a transcript end that closes the screen's wall with its
//! turn, a refutation that leaves the wall's settle armed, and T1's five
//! mutants (an acceptance after box 1 closed that binds box 2, a stale input
//! that approves, an act a person's input does not supersede, on the box or
//! on the lead, a binding a close keeps) are each rejected, and so is a
//! binding that names the other acting actor (`ActorSwap`, 6A-5), from a
//! ctl act and from the harness's.

use aterm_models_session::turn_tracker_model;
use aterm_slog::{
    ActWord, Actor, Admission, Binding, BoxWord, CarryAdopt, CarryBudget, Conf, Cursor, Fields,
    Key, Kind, LastClose, Limits, Log, Lvl, Node, Op, Open, Rec, STEP_ACT_MS, ScreenStep,
    SettleCause, ShortId, Src, Stamp, TurnTracker, TxEvent, TxPos, TxState, Verdict, WallJudge,
    WallKind, dismisses, within_lead,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, with_buggy};
use aterm_spec::rng::XorShift;
use aterm_spec::verify;

use crate::seq_twins::{self, Took};
use crate::support::{WALL0, agent_session, at};

/// The model's settle unit: `Settle = 2` ticks of 500 ms is the real 1 s.
const TICK_MS: u64 = 500;

/// The model's word code for a verdict.
fn code(v: Verdict) -> i64 {
    match v {
        Verdict::Unknown => 0,
        Verdict::Busy => 1,
        Verdict::Idle => 2,
        Verdict::Prompt(b) if b.is_question() => 3,
        Verdict::Prompt(_) => 4,
        Verdict::Wall(_) => 5,
        Verdict::Unresponsive => 6,
        Verdict::Survey => 7,
        Verdict::Question => 8,
        Verdict::Gone => 9,
    }
}

/// The model's action for observing `v`.
fn see(v: Verdict) -> &'static str {
    match v {
        Verdict::Busy => "SeeBusy",
        Verdict::Prompt(b) if b.is_question() => "SeeDialog",
        Verdict::Prompt(_) => "SeeBox",
        Verdict::Wall(_) => "SeeWall",
        Verdict::Unresponsive => "SeeStall",
        Verdict::Survey => "SeeSurvey",
        Verdict::Idle => "SeeIdle",
        Verdict::Question => "SeeQuestion",
        Verdict::Gone => "SeeGone",
        Verdict::Unknown => "SeeUnknown",
    }
}

/// The model's action for observing `v` on `rig`: another box's word while
/// a box is open replaces it (`SeeNextBox`); boxes of one word are one step.
fn see_in(rig: &Rig, v: Verdict) -> &'static str {
    match (v, rig.tracker.view(rig.now).step) {
        (Verdict::Prompt(b), Some((_, ScreenStep::Approval(a), _)))
            if !b.is_question() && a != b =>
        {
            "SeeNextBox"
        }
        _ => see(v),
    }
}

/// The model's time actions for `prev` to `next`: a `Tick` per settle
/// half-second newly held, `Age` when the bound input went stale, `LeadOut`
/// when the lead passed, `PersonLeadOut` when a person's did (T1).
fn timed(prev: &State, next: &State, ticks: usize) -> Vec<&'static str> {
    let mut path = vec!["Tick"; ticks];
    if matches!(prev["bk"], 1 | 2) && next["bk"] == 3 {
        path.push("Age");
    }
    if matches!(prev["pd"], 2 | 3) && next["pd"] != prev["pd"] {
        path.push("LeadOut");
    }
    if prev["pl"] == 1 && next["pl"] == 0 {
        path.push("PersonLeadOut");
    }
    path
}

/// The model's code for an acting actor (6A-5): 1 ctl, 2 the harness, 3
/// any other (no model step makes it, so a path or a law refuses it).
fn actor_code(by: Actor) -> i64 {
    match by {
        Actor::Ctl => 1,
        Actor::Harness => 2,
        _ => 3,
    }
}

/// The model's admission of an input from `by` (6A-5): the model knows two
/// acting actors beside the person.
fn admit_action(by: Actor) -> &'static str {
    match by {
        Actor::Ctl => "AdmitCtl",
        Actor::Harness => "AdmitHarness",
        other => panic!("the model's acting actors are ctl and the harness, not {other:?}"),
    }
}

fn step_code(s: ScreenStep) -> i64 {
    match s {
        ScreenStep::Approval(_) => 1,
        ScreenStep::Question => 2,
        ScreenStep::Wall(_) => 3,
        ScreenStep::Stall => 4,
        ScreenStep::Survey => 5,
    }
}

/// The real tracker and its log at a moment.
pub struct Rig {
    pub log: Log,
    pub run: Node,
    pub tracker: TurnTracker,
    pub now: u64,
    /// The last `ctl turn` ledger id handed out.
    pub ledger: u64,
    /// The transcript's read offset: each row ends past the last.
    pub tx_off: u64,
    /// The vendor turns prompted so far.
    pub vturns: u64,
    /// A ghost (the model's `wg`): a transcript call left the screen's
    /// wall with no step holding its need, and retracted none.
    pub wall_gone: bool,
    /// The input admitted and not yet accepted (T1): its admission, author
    /// and act, and the approval and question steps opened and closed by
    /// then.
    pub pending: Option<(Admission, Actor, Option<ActWord>, u64, u64)>,
    /// The open approval or question step and when the rig saw it open
    /// (`None`: carried across an update).
    pub opened: Option<(Node, Option<u64>)>,
    /// The step a person's input bound, and when (the model's `pp`).
    pub person_on: Option<(Node, u64)>,
    /// Ghosts (the model's `gn`, `ab`, `ga`).
    pub bound_next: bool,
    pub bound_twice: bool,
    pub approved_unseen: bool,
    /// Who sent the input the open step is bound to, as the rig fed it
    /// (the model's `bs`, 6A-5), and who sent the act accepted on the lead.
    pub bound_sender: Option<Actor>,
    pub lead_sender: Option<Actor>,
    /// The ring's bounds, which an update's successor keeps.
    pub limits: Limits,
    /// Its birth order and the host calls it made, while
    /// [`crate::seq_twins::record`] records (nothing otherwise).
    pub born: usize,
    pub path: Vec<Took>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        seq_twins::died(self.born, std::mem::take(&mut self.path), &self.log);
    }
}

impl Rig {
    pub fn new() -> Rig {
        Rig::with_limits(Limits::SESSION)
    }

    /// A rig whose ring holds `limits` (a `.seq` file's `ring=`).
    pub fn with_limits(limits: Limits) -> Rig {
        let mut log = Log::with_limits(0, limits);
        let run = agent_session(&mut log, 0);
        Rig {
            log,
            run,
            tracker: TurnTracker::new(run),
            now: 0,
            ledger: 0,
            tx_off: 0,
            vturns: 0,
            wall_gone: false,
            pending: None,
            opened: None,
            person_on: None,
            bound_next: false,
            bound_twice: false,
            approved_unseen: false,
            bound_sender: None,
            lead_sender: None,
            limits,
            born: seq_twins::born(),
            path: Vec::new(),
        }
    }

    pub fn stamp(&self) -> Stamp {
        at(self.now)
    }

    pub fn observe(&mut self, v: Verdict) -> Option<u64> {
        self.observe_with(v, None)
    }

    /// [`Self::observe`] with the publication's detail (a wall's reset).
    pub fn observe_with(&mut self, v: Verdict, detail: Option<&str>) -> Option<u64> {
        let before = self.before();
        let stamp = self.stamp();
        let next = self.tracker.observe(&mut self.log, v, detail, stamp);
        self.judge(before, Call::See);
        next
    }

    /// A `ctl turn` from `by` now, with the next ledger id.
    pub fn ctl(&mut self, by: Actor) -> u64 {
        let before = self.before();
        self.ledger += 1;
        let stamp = self.stamp();
        self.tracker.ctl_turn(&mut self.log, by, self.ledger, stamp);
        self.judge(before, Call::Other);
        self.ledger
    }

    /// [`Self::ctl`] naming its cause (`turn cause=<cause>`).
    pub fn ctl_caused(&mut self, by: Actor, cause: ShortId) -> u64 {
        let before = self.before();
        self.ledger += 1;
        let stamp = self.stamp();
        self.tracker
            .ctl_turn_caused(&mut self.log, by, self.ledger, Some(cause), stamp);
        self.judge(before, Call::Other);
        self.ledger
    }

    /// The host wakes the tracker now; its next deadline.
    pub fn tick(&mut self) -> Option<u64> {
        let before = self.before();
        let stamp = self.stamp();
        let next = self.tracker.tick(&mut self.log, stamp);
        self.judge(before, Call::Other);
        next
    }

    /// The host admits an input from `by` with `act` now (T1): what the
    /// tracker holds under the lock the host takes before a byte.
    pub fn admit(&mut self, by: Actor, act: Option<ActWord>) {
        let adm = self.tracker.admit(self.stamp());
        let a = self.tracker.attribution();
        self.pending = Some((adm, by, act, a.opens, a.closes));
    }

    /// The host accepts the admitted input now (its bytes written), or
    /// `None` when none is in flight.
    pub fn accept(&mut self) -> Option<Binding> {
        let before = self.before();
        let (adm, by, act, ..) = self.pending.take()?;
        let stamp = self.stamp();
        let bound = self.tracker.accept(&self.log, by, act, adm, stamp);
        match bound {
            Binding::Step(_) => {
                self.person_on = None;
                self.bound_sender = Some(by);
            }
            Binding::Lead => self.lead_sender = Some(by),
            Binding::Late(_) | Binding::Unbound => {}
        }
        self.judge(before, Call::Accept);
        Some(bound)
    }

    /// A person's input now (admitted and accepted at once).
    pub fn person(&mut self) -> Binding {
        let before = self.before();
        let stamp = self.stamp();
        let bound = self.tracker.person(&self.log, stamp);
        if let Binding::Step(n) = bound {
            self.person_on = Some((n, self.now));
        }
        self.judge(before, Call::Person);
        bound
    }

    /// The open approval or question step.
    pub fn open_binding(&self) -> Option<Node> {
        self.tracker
            .view(self.now)
            .step
            .filter(|(_, w, _)| matches!(w, ScreenStep::Approval(_) | ScreenStep::Question))
            .map(|(n, ..)| n)
    }

    /// What a call is judged against: the tracker's binding, the model's
    /// `pd` and the log's head before it.
    fn before(&self) -> Before {
        Before {
            bound: self.tracker.attribution().bound,
            pd: pending_code(self),
            pl: person_lead(self),
            seq: self.log.last_seq(),
        }
    }

    /// The ghosts a call moved (T1), and the open step it showed.
    fn judge(&mut self, b: Before, call: Call) {
        let open = self.open_binding();
        if open != self.opened.map(|(n, _)| n) {
            self.opened = open.map(|n| (n, Some(self.now)));
        }
        let a = self.tracker.attribution();
        // A binding the call made: only to the open step, and only the one
        // the call's input was admitted under or led to.
        if let Some((node, by, ..)) = a.bound
            && a.bound != b.bound
        {
            let legit = Some(node) == open
                && match call {
                    Call::Accept => b.pd == 1,
                    Call::Person => by == Actor::Human,
                    Call::See => {
                        (b.pd == 3 && by != Actor::Human) || (b.pl == 1 && by == Actor::Human)
                    }
                    Call::Other => false,
                };
            self.bound_next |= !legit;
            // An act's lead the open bound: its sender is the lead's.
            if matches!(call, Call::See) && by != Actor::Human {
                self.bound_sender = self.lead_sender;
            }
            // A person's lead the open bound: the step's latest input is
            // the person's, from when it was accepted (the model's `pp`).
            if matches!(call, Call::See)
                && by == Actor::Human
                && let Some((_, _, _, acc)) = a.bound
            {
                self.person_on = Some((node, acc.mono_ms));
            }
        }
        let now = self.stamp();
        for r in self.since(b.seq) {
            if r.op != Op::Close || !matches!(r.k, Kind::Approval | Kind::Question) {
                continue;
            }
            let held = b.bound.filter(|(n, ..)| *n == r.node);
            if let Some((_, by, act, acc)) = held
                && r.by == by
                && a.bound
                    .is_some_and(|(_, by2, act2, acc2)| (by2, act2, acc2) == (by, act, acc))
            {
                self.bound_twice = true;
            }
            if matches!(r.out(), Some("approved" | "declined" | "answered")) {
                let seen = held.is_some_and(|(_, by, act, acc)| {
                    by.is_proven() && act.is_some() && dismisses(acc, now)
                });
                self.approved_unseen |= !seen;
            }
        }
    }

    /// One transcript row of a measured build, dated `wall_ms`, at the next
    /// offset of the one file (bound at the first row, with nothing in it
    /// before: the session started with the run).
    pub fn tx(&mut self, ev: TxEvent<'_>, wall_ms: u64) -> Option<u64> {
        let stamp = self.stamp();
        let pos = |off: u64, wall_ms: u64| TxPos {
            fp: 0x5e55_1015,
            id: Some((1, 7)),
            off,
            wall_ms,
        };
        if self.tracker.tx_view().through.is_none() {
            let bind = TxEvent::Bind {
                len: 0,
                floor_ms: WALL0,
            };
            self.tracker
                .transcript(&mut self.log, bind, pos(0, WALL0), TxState::On, stamp);
        }
        let before = self.before();
        self.tx_off += 120;
        let wall = self
            .tracker
            .view(self.now)
            .step
            .filter(|(_, w, _)| matches!(w, ScreenStep::Wall(_)))
            .map(|(n, ..)| n);
        let seq0 = self.log.last_seq();
        let next = self.tracker.transcript(
            &mut self.log,
            ev,
            pos(self.tx_off, wall_ms),
            TxState::On,
            stamp,
        );
        // The wall's need is lost when no wall step holds it after the call
        // (its own, or the run's that a due settle continued it into) and
        // the call retracted none.
        let held = self
            .tracker
            .view(self.now)
            .step
            .is_some_and(|(_, w, _)| matches!(w, ScreenStep::Wall(_)));
        let retracted = self
            .since(seq0)
            .iter()
            .any(|r| r.op == Op::Close && r.out() == Some("retracted"));
        if wall.is_some() && !held && !retracted {
            self.wall_gone = true;
        }
        self.judge(before, Call::Other);
        next
    }

    /// The vendor turn of the open turn: its prompt (dated at the turn's
    /// open: it caused it) unless the turn already maps one, then its end.
    /// `false` when the open turn's vendor turn already ended (a screen step
    /// held it open): its transcript has nothing more to say.
    pub fn tx_turn(&mut self) -> bool {
        let view = self.tracker.tx_view();
        if let Some(t) = self.tracker.turn() {
            if view.mapped == Some(t) && view.ended {
                return false;
            }
            if view.mapped != Some(t) {
                self.vturns += 1;
                let vref = ShortId::new(&format!("v{}", self.vturns)).expect("an id");
                let opened = self.log.open_entry(t).map_or(WALL0, |e| e.t);
                self.tx(TxEvent::Prompt { vref }, opened);
            }
        }
        let end = TxEvent::TurnEnd {
            vdur_ms: 900,
            tok_in: 40,
            tok_out: 12,
        };
        self.tx(end, WALL0 + self.now);
        true
    }

    /// The records appended after seq `after`. Seqs the log never held (an
    /// update's overlap) are no loss; an evicted one is.
    pub fn since(&self, after: u64) -> Vec<Rec> {
        let read = self.log.read(Cursor::new(self.log.ep(), after), usize::MAX);
        assert!(
            read.gap.is_none_or(|g| g.lost == 0),
            "a step appended past the ring"
        );
        read.recs.into_iter().cloned().collect()
    }
}

/// What a rig call was, for its judge.
#[derive(Clone, Copy)]
enum Call {
    See,
    Accept,
    Person,
    Other,
}

/// What a rig call is judged against.
#[derive(Clone, Copy)]
struct Before {
    bound: Option<(Node, Actor, Option<ActWord>, Stamp)>,
    pd: i64,
    pl: i64,
    seq: u64,
}

/// The model's `pd` (T1): 1 the input in flight was admitted under the open
/// step (or led to it: the first to open, inside the lead, none closing
/// between), 2 admitted with none open and its lead running, 3 accepted on
/// a lead still running, 4 its step closed or its lead passed, else 0.
fn pending_code(rig: &Rig) -> i64 {
    let a = rig.tracker.attribution();
    let open = rig.open_binding();
    if let Some((adm, _, _, opens, closes)) = rig.pending {
        return match (adm.step(), open) {
            (Some(n), Some(o)) if n == o => 1,
            (Some(_), _) => 4,
            (None, Some(o)) => {
                let led = a.opens == opens + 1
                    && a.closes == closes
                    && rig.opened.is_some_and(|(n, t)| {
                        n == o && t.is_some_and(|t| within_lead(adm.at(), at(t)))
                    });
                if led { 1 } else { 4 }
            }
            (None, None) => {
                let running =
                    a.opens == opens && a.closes == closes && within_lead(adm.at(), rig.stamp());
                if running { 2 } else { 4 }
            }
        };
    }
    match a.lead {
        Some((by, _, admitted, _, opens, closes))
            if by != Actor::Human
                && opens == a.opens
                && closes == a.closes
                && within_lead(admitted, rig.stamp()) =>
        {
            3
        }
        _ => 0,
    }
}

/// The model's `pl` (T1): 1 while a person's input waits on a lead still
/// running (none open at its key, the first such step to open within
/// `STEP_INPUT_LEAD_MS` is its), else 0.
fn person_lead(rig: &Rig) -> i64 {
    let a = rig.tracker.attribution();
    match a.lead {
        Some((Actor::Human, _, admitted, _, opens, closes))
            if opens == a.opens && closes == a.closes && within_lead(admitted, rig.stamp()) =>
        {
            1
        }
        _ => 0,
    }
}

/// The seqs an update's host reserves past the carried head (its successor
/// starts above them: a read across says `gap why=handoff`).
pub const HANDOFF_RESERVE: u64 = 8;

/// A seamless update of `log` and `tracker` at `at` (design §5.2), as the
/// glue's adoption makes it (`session_log.rs`'s `adopt`): CARRIED, the log
/// and the tracker's line cross (`Log::carry`, `Log::from_carry`, the line
/// as a host line); UNCARRIED, the successor starts a fresh log after a
/// `gap why=handoff` at the next epoch, continuing the node ids, its session
/// `open - adopt` and the agent's run opened again, a fresh tracker on it.
/// Returns the run.
pub fn handoff(
    log: &mut Log,
    tracker: &mut TurnTracker,
    limits: Limits,
    carried: bool,
    at: Stamp,
) -> Node {
    let next_seq = log.last_seq() + 1 + HANDOFF_RESERVE;
    if carried {
        let line = tracker.carry_line(at);
        let out = log
            .carry(CarryBudget::SESSION, &[line])
            .expect("the carry fits");
        let adopt = CarryAdopt {
            ep: log.ep() + 1,
            next_seq,
            ids: log.id_counters(),
            limits,
            at,
        };
        let (next, host, _) = Log::from_carry(&out.lines, adopt).expect("the carry reads");
        *log = next;
        let line = host
            .iter()
            .find(|l| l.starts_with("tracker "))
            .expect("the tracker's line crossed");
        *tracker = TurnTracker::from_carry_line(line, at).expect("its own line reads");
        return tracker.run();
    }
    let mut next = Log::after_handoff(log.ep() + 1, limits, log.id_counters(), next_seq, at);
    next.open_session(
        Kind::Adopt,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at,
    )
    .expect("the successor's session opens");
    let f = Fields::new()
        .tok(Key::Agent, "claude")
        .expect("agent is a token key");
    let run = next
        .open(
            Open::new(Kind::Agent, Src::Status, Conf::Strong).fields(f),
            at,
        )
        .expect("the agent's run opens again");
    *log = next;
    *tracker = TurnTracker::new(run);
    run
}

/// The projection onto the model's variables. `sturn_override` replaces
/// the step's parent for the negative control.
pub fn project(rig: &Rig, sturn_override: Option<i64>) -> State {
    let view = rig.tracker.view(rig.now);
    let turn = view.turn.is_some_and(|t| rig.log.is_open(t));
    let open_turns = rig.log.open_under(rig.run, Lvl::Turn).count() as i64;
    let (step, sturn) = view.step.map_or((0, 0), |(node, what, under_turn)| {
        assert!(rig.log.is_open(node), "the tracker holds a closed step");
        let parent = rig.log.open_entry(node).map(|e| e.parent.lvl);
        assert_eq!(parent == Some(Lvl::Turn), under_turn, "the step's parent");
        (step_code(what), i64::from(under_turn))
    });
    let (pend, ticks) = view
        .settle
        .map_or((0, 0), |(_, held)| (1, (held / TICK_MS).min(2) as i64));
    let (cw, cticks) = view.last_close.map_or((10, 0), |c: LastClose| {
        (
            if c.tx {
                12
            } else if c.ctl {
                11
            } else {
                code(c.cause)
            },
            c.settled_ms.map_or(0, |ms| (ms / TICK_MS).min(2) as i64),
        )
    });
    // T1: the input bound to the open approval or question step, fresh
    // (`dismisses` at now) or stale, a person's or not.
    let open = rig.open_binding();
    let bk = match rig.tracker.attribution().bound {
        None => 0,
        Some((node, by, _, accepted)) => {
            assert_eq!(Some(node), open, "a binding outlived its step");
            match (dismisses(accepted, rig.stamp()), by == Actor::Human) {
                (false, _) => 3,
                (true, true) => 2,
                (true, false) => 1,
            }
        }
    };
    let pp = rig
        .person_on
        .is_some_and(|(n, t)| Some(n) == open && rig.now - t <= STEP_ACT_MS);
    // 6A-5: who sent the input in flight or on the lead (`pa`), whom the
    // fresh act's binding names (`ba`) and who sent it (`bs`, the rig's).
    let a = rig.tracker.attribution();
    let pd = pending_code(rig);
    let pa = match rig.pending {
        Some((_, by, ..)) => actor_code(by),
        None if pd == 3 => a.lead.map_or(0, |(by, ..)| actor_code(by)),
        None => 0,
    };
    let (ba, bs) = if bk == 1 {
        (
            a.bound.map_or(0, |(_, by, ..)| actor_code(by)),
            rig.bound_sender.map_or(0, actor_code),
        )
    } else {
        (0, 0)
    };
    [
        ("turn", i64::from(turn)),
        ("extra", (open_turns - 1).max(0)),
        ("step", step),
        ("sturn", sturn_override.unwrap_or(sturn)),
        ("word", code(view.word)),
        ("pend", pend),
        ("ticks", ticks),
        ("cw", cw),
        ("cticks", cticks),
        ("rf", i64::from(rig.tracker.tx_view().refuted.is_some())),
        ("wg", i64::from(rig.wall_gone)),
        ("bk", bk),
        ("pd", pd),
        ("pp", i64::from(pp)),
        ("pl", person_lead(rig)),
        ("pa", pa),
        ("ba", ba),
        ("bs", bs),
        ("ab", i64::from(rig.bound_twice)),
        ("gn", i64::from(rig.bound_next)),
        ("ga", i64::from(rig.approved_unseen)),
    ]
    .into_iter()
    .collect()
}

/// The states of a path of the model's `actions` from `from` that ends at
/// `to`, or `None` (the model is deterministic per action, but the search
/// does not rely on it).
pub fn path_to(m: &Model, actions: &[&str], from: &State, to: &State) -> Option<Vec<State>> {
    let Some((first, rest)) = actions.split_first() else {
        return (from == to).then(|| vec![from.clone()]);
    };
    m.successors(first, from).into_iter().find_map(|mid| {
        path_to(m, rest, &mid, to).map(|mut tail| {
            tail.insert(0, from.clone());
            tail
        })
    })
}

/// The real step from `prev` to `next` is the model's path `actions`, every
/// invariant holds along it, and the turn records it appended (after seq
/// `seq0`, with `turn0` the tracker's turn before) are exactly the path's
/// turn opens and closes: identity, not just presence.
fn admitted(
    m: &Model,
    rig: &Rig,
    actions: &[&str],
    prev: &State,
    next: &State,
    (seq0, turn0): (u64, Option<Node>),
) {
    let Some(states) = path_to(m, actions, prev, next) else {
        panic!(
            "real step is not the model's {actions:?}:\n  from {prev:?}\n  to   {next:?}\n  model {:?}",
            actions
                .first()
                .map(|a| m.successors(a, prev))
                .unwrap_or_default()
        );
    };
    for s in &states {
        for inv in &m.invariants {
            assert!(
                m.check_invariant(inv.name, s),
                "{} broken along real {actions:?}: {s:?}",
                inv.name
            );
        }
    }
    let ups = states
        .windows(2)
        .filter(|w| w[0]["turn"] == 0 && w[1]["turn"] == 1)
        .count();
    let downs = states
        .windows(2)
        .filter(|w| w[0]["turn"] == 1 && w[1]["turn"] == 0)
        .count();
    let appended = rig.since(seq0);
    let turn_recs = |op: Op| {
        appended
            .iter()
            .filter(|r| r.op == op && r.node.lvl == Lvl::Turn)
            .count()
    };
    assert_eq!(
        (turn_recs(Op::Open), turn_recs(Op::Close)),
        (ups, downs),
        "turn records of real {actions:?} are not the model's: {appended:?}"
    );
    assert!(
        !appended.iter().any(|r| r.out() == Some("superseded")),
        "the tracker superseded its own turn: {appended:?}"
    );
    if ups == 0 && downs == 0 {
        assert_eq!(rig.tracker.turn(), turn0, "the open turn changed identity");
    }
}

/// One host step against the model.
#[derive(Clone, Copy, Debug)]
pub enum Step {
    /// The verdict moved (a repeat is sent too: it must change nothing but
    /// an overdue settle).
    See(Verdict),
    /// Half a second passes, then the host wakes the tracker: early (it
    /// changes nothing) or once the settle is due.
    Half,
    /// Half a second passes with no wake: the host's timer is late.
    Elapse,
    /// The host wakes the tracker now, due or not.
    Wake,
    /// A `ctl turn` exchange from the harness.
    Ctl,
    /// [`Step::Ctl`] naming what it acted on (`turn cause=`): the same
    /// model step.
    CtlCaused(ShortId),
    /// The open turn's vendor turn is prompted and ends in the transcript
    /// (after a wake, when a settle is due).
    TxEnd,
    /// The wall judge refutes the screen's wall (any wall, when none is
    /// open: nothing to refute).
    TxRefute,
    /// [`Step::See`] with the publication's detail (a wall's reset).
    SeeWith(Verdict, ShortId),
    /// `ms` pass, then the host wakes the tracker if its deadline passed
    /// (`Rig::wait`): the model's `Tick`s for the settle's half-seconds
    /// held, then its `Rest` when due.
    Wait(u64),
    /// `ms` pass with no wake (the host's timer is late): `Tick`s only.
    Pass(u64),
    /// Input from the actor reached the session: it attributes, and moves
    /// nothing the model holds.
    Input(Actor),
    /// The screen reported the turn interrupted: its idle close says so,
    /// and nothing the model holds moves.
    Interrupted,
    /// A seamless update, carried (`true`) or not ([`handoff`]). Carried,
    /// the tracker holds what it held but the last close (a view's, which
    /// does not cross) and every input's binding (T1: none is carried);
    /// uncarried, it starts as the model's initial state.
    Handoff(bool),
    /// The host admits an input from the actor with its act (T1's
    /// admission: the model's `AdmitCtl` or `AdmitHarness` by the actor,
    /// 6A-5; one is in flight at a time).
    Admit(Actor, Option<ActWord>),
    /// The host accepts the input in flight (`Accept`; nothing when none
    /// is).
    Accept,
    /// A person's input (`Person` while an approval or question step is
    /// open; else `PersonLeads`: it waits on the lead, taking it from an act
    /// waiting there).
    Person,
}

/// Drive `steps`, checking each against the model; returns the rig.
pub fn drive(steps: &[Step]) -> Rig {
    let m = turn_tracker_model();
    let mut rig = Rig::new();
    for step in steps {
        apply(&m, &mut rig, *step);
    }
    rig
}

/// One host step on `rig`, checked against the model.
pub fn checked(rig: &mut Rig, step: Step) {
    apply(&turn_tracker_model(), rig, step);
}

/// `ms` pass with no wake: the model's `Tick` for each half-second the
/// settle newly held, or nothing when none is counting.
fn pass(m: &Model, rig: &mut Rig, ms: u64) -> State {
    let prev = project(rig, None);
    let before = (rig.log.last_seq(), rig.tracker.turn());
    rig.now += ms;
    let next = project(rig, None);
    let ticks = usize::try_from((next["ticks"] - prev["ticks"]).max(0)).unwrap_or(0);
    admitted(m, rig, &timed(&prev, &next, ticks), &prev, &next, before);
    next
}

/// Time passes by half a second: the model's `Tick`, or nothing when no
/// settle is counting.
fn elapse(m: &Model, rig: &mut Rig) -> State {
    let prev = project(rig, None);
    let before = (rig.log.last_seq(), rig.tracker.turn());
    rig.now += TICK_MS;
    let ticked = project(rig, None);
    let ticks = usize::from(m.action_enabled("Tick", &prev));
    admitted(
        m,
        rig,
        &timed(&prev, &ticked, ticks),
        &prev,
        &ticked,
        before,
    );
    ticked
}

/// The host wakes the tracker now: the model's `Rest` when the settle is
/// due, else nothing at all (the deadline it asks for included).
fn wake(m: &Model, rig: &mut Rig) {
    let prev = project(rig, None);
    let before = (rig.log.last_seq(), rig.tracker.turn());
    let asked = rig.tracker.wake_at();
    rig.tick();
    let next = project(rig, None);
    if m.action_enabled("Rest", &prev) {
        admitted(m, rig, &["Rest"], &prev, &next, before);
    } else {
        admitted(m, rig, &[], &prev, &next, before);
        assert_eq!(
            rig.tracker.wake_at(),
            asked,
            "an early wake moved the deadline"
        );
    }
}

/// The verdict moved to `v` (a repeat is sent too: it must change nothing
/// but an overdue settle): the model's `Rest` when due, then its `See*`.
fn see_step(m: &Model, rig: &mut Rig, v: Verdict, detail: Option<ShortId>) {
    let prev = project(rig, None);
    let before = (rig.log.last_seq(), rig.tracker.turn());
    let due = m.action_enabled("Rest", &prev);
    let repeat = v == rig.tracker.view(rig.now).word;
    let name = see_in(rig, v);
    rig.observe_with(v, detail.as_ref().map(ShortId::as_str));
    let mut path = Vec::new();
    if due {
        path.push("Rest");
    }
    if !repeat {
        path.push(name);
    }
    admitted(m, rig, &path, &prev, &project(rig, None), before);
}

/// One host step on `rig`, checked against `m` (the derived model), and
/// noted on its path while the twins record.
pub fn apply(m: &Model, rig: &mut Rig, step: Step) {
    let seq0 = rig.log.last_seq();
    apply_step(m, rig, step);
    if seq_twins::on() {
        let took = Took {
            step: format!("{step:?}"),
            state: Some(project(rig, None)),
            lines: seq_twins::lines_since(&rig.log, seq0),
        };
        rig.path.push(took);
    }
}

fn apply_step(m: &Model, rig: &mut Rig, step: Step) {
    let prev = project(rig, None);
    let before = (rig.log.last_seq(), rig.tracker.turn());
    let due = m.action_enabled("Rest", &prev);
    match step {
        Step::See(v) => see_step(m, rig, v, None),
        Step::SeeWith(v, detail) => see_step(m, rig, v, Some(detail)),
        Step::Half => {
            elapse(m, rig);
            wake(m, rig);
        }
        Step::Elapse => {
            elapse(m, rig);
        }
        Step::Wake => wake(m, rig),
        Step::Ctl | Step::CtlCaused(_) => {
            let resting = m.action_enabled("CtlTurn", &prev);
            match step {
                Step::CtlCaused(cause) => rig.ctl_caused(Actor::Harness, cause),
                _ => rig.ctl(Actor::Harness),
            };
            let path: &[&str] = if due {
                &["Rest"]
            } else if resting {
                &["CtlTurn"]
            } else {
                &[]
            };
            admitted(m, rig, path, &prev, &project(rig, None), before);
        }
        Step::TxEnd => {
            if due {
                wake(m, rig);
            }
            let prev = project(rig, None);
            let before = (rig.log.last_seq(), rig.tracker.turn());
            let closes = m.action_enabled("TxClose", &prev);
            if rig.tx_turn() {
                let path: &[&str] = if closes { &["TxClose"] } else { &[] };
                admitted(m, rig, path, &prev, &project(rig, None), before);
            }
        }
        Step::TxRefute => {
            let kind = match rig.tracker.view(rig.now).step {
                Some((_, ScreenStep::Wall(k), _)) => k,
                _ => WallKind::UsageSession,
            };
            let mut path = Vec::new();
            let mut mid = prev.clone();
            if due {
                path.push("Rest");
                mid = m.successors("Rest", &prev).remove(0);
            }
            if m.action_enabled("TxRefute", &mid) {
                path.push("TxRefute");
            }
            let wall = TxEvent::Wall {
                kind,
                judge: WallJudge::Refute,
            };
            rig.tx(wall, WALL0 + rig.now);
            admitted(m, rig, &path, &prev, &project(rig, None), before);
        }
        Step::Wait(ms) => {
            pass(m, rig, ms);
            if rig.tracker.wake_at().is_some_and(|d| d <= rig.now) {
                wake(m, rig);
            }
        }
        Step::Pass(ms) => {
            pass(m, rig, ms);
        }
        Step::Input(by) => {
            let stamp = rig.stamp();
            rig.tracker.note_input(by, stamp);
            admitted(m, rig, &[], &prev, &project(rig, None), before);
        }
        Step::Interrupted => {
            rig.tracker.note_interrupted();
            admitted(m, rig, &[], &prev, &project(rig, None), before);
        }
        Step::Admit(by, act) => {
            assert_eq!(prev["pd"], 0, "one input in flight at a time");
            rig.admit(by, act);
            admitted(
                m,
                rig,
                &[admit_action(by)],
                &prev,
                &project(rig, None),
                before,
            );
        }
        Step::Accept => {
            let path: &[&str] = if rig.accept().is_some() {
                &["Accept"]
            } else {
                &[]
            };
            admitted(m, rig, path, &prev, &project(rig, None), before);
        }
        Step::Person => {
            rig.person();
            let path: &[&str] = if matches!(prev["step"], 1 | 2) {
                &["Person"]
            } else {
                &["PersonLeads"]
            };
            admitted(m, rig, path, &prev, &project(rig, None), before);
        }
        Step::Handoff(carried) => {
            let stamp = rig.stamp();
            rig.run = handoff(&mut rig.log, &mut rig.tracker, rig.limits, carried, stamp);
            // An input in flight is the predecessor's; a carried step was
            // not seen to open here (T1).
            rig.pending = None;
            rig.person_on = None;
            (rig.bound_sender, rig.lead_sender) = (None, None);
            rig.opened = rig.open_binding().map(|n| (n, None));
            let want = if carried {
                let mut kept = prev.clone();
                for (k, v) in [
                    ("cw", 10),
                    ("cticks", 0),
                    ("bk", 0),
                    ("pd", 0),
                    ("pp", 0),
                    ("pl", 0),
                    ("pa", 0),
                    ("ba", 0),
                    ("bs", 0),
                ] {
                    kept.insert(k, v);
                }
                kept
            } else {
                rig.wall_gone = false;
                (rig.bound_next, rig.bound_twice, rig.approved_unseen) = (false, false, false);
                m.init_state()
            };
            let next = project(rig, None);
            assert_eq!(
                next, want,
                "an update (carried: {carried}) moved what the tracker holds"
            );
            for inv in &m.invariants {
                assert!(m.check_invariant(inv.name, &next), "{} broken", inv.name);
            }
            let turns = rig.since(before.0);
            assert!(
                !turns.iter().any(|r| r.node.lvl == Lvl::Turn),
                "an update wrote a turn record: {turns:?}"
            );
        }
    }
}

const POOL: &[Verdict] = &[
    Verdict::Busy,
    Verdict::Busy,
    Verdict::Idle,
    Verdict::Idle,
    Verdict::Prompt(BoxWord::QUESTION),
    Verdict::Unknown,
    Verdict::Survey,
    Verdict::Question,
    Verdict::Unresponsive,
    Verdict::Gone,
    Verdict::Wall(WallKind::UsageSession),
    Verdict::Wall(WallKind::Overloaded),
];

#[test]
fn turn_tracker_conforms_to_derived_model() {
    let m = turn_tracker_model();
    let mut ran = std::collections::BTreeSet::new();
    let mut from_due = std::collections::BTreeSet::new();
    for seed in 1..=120u64 {
        let mut rng = XorShift::new(seed);
        let mut rig = Rig::new();
        for _ in 0..300 {
            let prev = project(&rig, None);
            let step = match rng.below(17) {
                0..=2 => Step::Half,
                3 => Step::Elapse,
                4 => Step::Wake,
                5 => Step::Ctl,
                12 => Step::TxEnd,
                13 => Step::TxRefute,
                14 if prev["pd"] == 0 => {
                    let by = [Actor::Ctl, Actor::Harness][rng.index(2)];
                    let act = [
                        None,
                        Some(ActWord::Approve),
                        Some(ActWord::Deny),
                        Some(ActWord::Answer),
                    ][rng.index(4)];
                    Step::Admit(by, act)
                }
                14 | 15 => Step::Accept,
                16 => Step::Person,
                _ => {
                    let mut v = POOL[rng.index(POOL.len())];
                    if rng.below(5) == 0 {
                        let b = ["bash:not-read-only", "edit", "bash:read-only"][rng.index(3)];
                        v = Verdict::Prompt(BoxWord::from_detail(b));
                    }
                    Step::See(v)
                }
            };
            if let Step::See(v) = step {
                ran.insert(see_in(&rig, v));
                if m.action_enabled("Rest", &prev) {
                    from_due.insert(see(v));
                }
            }
            if matches!(step, Step::Ctl) && m.action_enabled("CtlTurn", &prev) {
                ran.insert("CtlTurn");
            }
            if matches!(step, Step::Wake) && m.action_enabled("Rest", &prev) {
                ran.insert("Rest");
            }
            if matches!(step, Step::TxEnd) && m.action_enabled("TxClose", &prev) {
                ran.insert("TxClose");
            }
            if matches!(step, Step::TxEnd)
                && prev["turn"] == 1
                && !m.action_enabled("TxClose", &prev)
            {
                ran.insert("TxEnd held by a screen step");
            }
            if matches!(step, Step::TxRefute) && m.action_enabled("TxRefute", &prev) {
                ran.insert(if prev["sturn"] == 1 {
                    "TxRefute under the turn"
                } else {
                    "TxRefute on the run"
                });
            }
            let seq0 = rig.log.last_seq();
            apply(&m, &mut rig, step);
            let next = project(&rig, None);
            for (name, moved) in [
                ("AdmitCtl", matches!(step, Step::Admit(Actor::Ctl, _))),
                (
                    "AdmitHarness",
                    matches!(step, Step::Admit(Actor::Harness, _)),
                ),
                ("a ctl act binds", next["bk"] == 1 && next["ba"] == 1),
                ("a harness act binds", next["bk"] == 1 && next["ba"] == 2),
                ("Accept binds", prev["pd"] == 1 && next["bk"] == 1),
                ("Accept on the lead", prev["pd"] == 2 && next["pd"] == 3),
                ("Accept late", prev["pd"] == 4 && next["pd"] == 0),
                ("the lead's open binds", prev["pd"] == 3 && next["bk"] == 1),
                ("the lead's open admits", prev["pd"] == 2 && next["pd"] == 1),
                ("Person", matches!(step, Step::Person) && next["bk"] == 2),
                (
                    "PersonLeads",
                    matches!(step, Step::Person) && next["pl"] == 1,
                ),
                ("a person's lead binds", prev["pl"] == 1 && next["bk"] == 2),
                ("Age", prev["bk"] != 3 && next["bk"] == 3),
                (
                    "LeadOut",
                    matches!(prev["pd"], 2 | 3)
                        && matches!(next["pd"], 0 | 4)
                        && !matches!(step, Step::See(_) | Step::Accept),
                ),
            ] {
                if moved {
                    ran.insert(name);
                }
            }
            for r in rig.since(seq0) {
                if r.op == Op::Close && matches!(r.k, Kind::Approval | Kind::Question) {
                    ran.insert(match (r.out(), r.by) {
                        (Some("approved" | "declined" | "answered"), _) => "a close an act decided",
                        (_, Actor::Human) => "a close a person's input attributed",
                        (_, Actor::Ctl | Actor::Harness) => {
                            "a close an unproven or unacted input attributed"
                        }
                        _ => "a close no input attributed",
                    });
                }
            }
        }
        // Every turn the tracker opened is the run's only open turn.
        assert!(rig.log.open_under(rig.run, Lvl::Turn).count() <= 1);
    }
    for a in [
        "SeeBusy",
        "SeeDialog",
        "SeeBox",
        "SeeWall",
        "SeeStall",
        "SeeSurvey",
        "SeeIdle",
        "SeeQuestion",
        "SeeGone",
        "SeeUnknown",
        "CtlTurn",
        "Rest",
        "TxClose",
        "TxEnd held by a screen step",
        "TxRefute under the turn",
        "TxRefute on the run",
        "SeeNextBox",
        "AdmitCtl",
        "AdmitHarness",
        "a ctl act binds",
        "a harness act binds",
        "Accept binds",
        "Accept on the lead",
        "Accept late",
        "the lead's open binds",
        "the lead's open admits",
        "Person",
        "PersonLeads",
        "a person's lead binds",
        "Age",
        "LeadOut",
        "a close an act decided",
        "a close a person's input attributed",
        "a close an unproven or unacted input attributed",
        "a close no input attributed",
    ] {
        assert!(ran.contains(a), "the walks never drove `{a}`");
    }
    // A late wake: verdicts arrive while the settle is due but unwoken.
    for a in ["SeeBusy", "SeeIdle", "SeeUnknown", "SeeWall"] {
        assert!(
            from_due.contains(a),
            "the walks never drove `{a}` past a due settle"
        );
    }

    // The ty tier re-checks one real transition where it is installed.
    let mut rig = Rig::new();
    let prev = project(&rig, None);
    rig.observe(Verdict::Busy);
    let (ok, why) = verify::validate_transition_tiered(
        &m,
        &[],
        &prev,
        &project(&rig, None),
        Some("SeeBusy"),
        "TurnTracker conformance",
    );
    assert!(ok, "real SeeBusy rejected: {why}");
}

/// THE SETTLE POLICY, by name: `settle_due` closes exactly where the
/// model's `Rest` is enabled (two half-second ticks held), and `settle_out`
/// closes an idle `done` (or `interrupted`), a prose question `asked` at
/// `heuristic`, and a wall on its kind.
#[test]
fn settle_policy_is_the_models() {
    use aterm_slog::{Conf, SettleCause, TurnOut, settle_due, settle_out};
    let m = turn_tracker_model();
    for held in (0..=3000u64).step_by(250) {
        let mut s = m.init_state();
        for (k, v) in [("turn", 1), ("pend", 1), ("word", 2)] {
            s.insert(k, v);
        }
        s.insert("ticks", (held / TICK_MS).min(2) as i64);
        assert_eq!(
            settle_due(10_000, 10_000 + held),
            m.action_enabled("Rest", &s),
            "held {held} ms"
        );
    }
    assert_eq!(
        settle_out(SettleCause::Idle, false),
        (TurnOut::Done, Conf::Strong)
    );
    assert_eq!(
        settle_out(SettleCause::Survey, true),
        (TurnOut::Interrupted, Conf::Strong)
    );
    assert_eq!(
        settle_out(SettleCause::Question, false),
        (TurnOut::Asked, Conf::Heuristic)
    );
    let k = WallKind::UsageWeekly;
    assert_eq!(
        settle_out(SettleCause::Wall(k), false),
        (TurnOut::Wall(k), Conf::Strong)
    );
}

/// NEGATIVE CONTROLS: the model rejects what the tracker refuses to do, and
/// the projection is load-bearing.
#[test]
fn turn_tracker_conformance_rejects_early_and_unknown_closes() {
    let m = turn_tracker_model();
    let buggy = with_buggy(&m, 1);

    // The Codex mid-answer idle: busy, idle, and 220 ms later busy again.
    let mut rig = drive(&[Step::See(Verdict::Busy), Step::See(Verdict::Idle)]);
    let armed = project(&rig, None);
    assert_eq!((armed["turn"], armed["pend"]), (1, 1));
    rig.now += 220;
    // An early wake 220 ms in closes nothing and keeps its deadline.
    let asked = rig.tracker.wake_at();
    let stamp = rig.stamp();
    let again = rig.tracker.tick(&mut rig.log, stamp);
    assert!(
        rig.tracker.turn().is_some(),
        "an early wake closed the turn"
    );
    assert_eq!((again, rig.tracker.wake_at()), (asked, asked));
    rig.observe(Verdict::Busy);
    assert!(
        rig.tracker.turn().is_some(),
        "the mid-answer idle kept its turn"
    );
    // A tracker that closed on the idle at once is not the model's...
    let mut early = armed.clone();
    for (k, v) in [("turn", 0), ("pend", 0), ("cw", 2), ("cticks", 0)] {
        early.insert(k, v);
    }
    assert!(aterm_spec::interp::admits(&m, &armed, &early).is_none());
    // ...and is exactly the Buggy model's EarlyRest, which its law catches.
    assert!(buggy.successors("EarlyRest", &armed).contains(&early));
    assert!(!m.check_invariant("IdleClosesOnlyAfterSettle", &early));

    // A turn closed on `unknown` is rejected.
    let busy = project(&drive(&[Step::See(Verdict::Busy)]), None);
    let mut closed = busy.clone();
    for (k, v) in [("turn", 0), ("word", 0), ("cw", 0)] {
        closed.insert(k, v);
    }
    assert!(!m.successors("SeeUnknown", &busy).contains(&closed));
    assert!(!m.check_invariant("UnknownNeverClosesATurn", &closed));

    // A projection that hangs every step from the turn: the wall the turn
    // closed on stays open on the run, and that projection breaks the law.
    let rested = drive(&[
        Step::See(Verdict::Busy),
        Step::See(Verdict::Wall(WallKind::UsageSession)),
        Step::Half,
        Step::Half,
    ]);
    let honest = project(&rested, None);
    assert_eq!((honest["turn"], honest["step"], honest["sturn"]), (0, 3, 0));
    let blind = project(&rested, Some(1));
    assert!(!m.check_invariant("StepsNestInTheirTurn", &blind));
    assert!(
        rested
            .tracker
            .view(rested.now)
            .settle
            .is_none_or(|(c, _)| c != SettleCause::Idle)
    );
}

/// NEGATIVE CONTROLS for the transcript (rulings S2, S4): a turn end never
/// closes a turn the screen's wall is open under (the wall's need outlives
/// it), and a refutation shuts the wall and its settle together.
#[test]
fn turn_tracker_conformance_rejects_tx_closes_that_take_a_wall() {
    let m = turn_tracker_model();
    let buggy = with_buggy(&m, 1);

    // The vendor turn ends while the screen shows a wall under its turn:
    // the turn and the wall stay open, and the end only amends.
    let mut rig = drive(&[
        Step::See(Verdict::Busy),
        Step::See(Verdict::Wall(WallKind::UsageSession)),
    ]);
    let walled = project(&rig, None);
    assert_eq!((walled["turn"], walled["step"], walled["sturn"]), (1, 3, 1));
    checked(&mut rig, Step::TxEnd);
    assert_eq!(
        project(&rig, None),
        walled,
        "the end moved nothing it holds"
    );
    assert!(rig.tracker.tx_view().ended);
    // A join that closed the turn, its cascade taking the wall, is not the
    // model's, and is exactly the Buggy model's TxCloseTakesWall.
    let mut taken = walled.clone();
    for (k, v) in [
        ("turn", 0),
        ("cw", 12),
        ("step", 0),
        ("sturn", 0),
        ("pend", 0),
        ("wg", 1),
    ] {
        taken.insert(k, v);
    }
    assert!(aterm_spec::interp::admits(&m, &walled, &taken).is_none());
    assert!(
        buggy
            .successors("TxCloseTakesWall", &walled)
            .contains(&taken)
    );
    assert!(!m.check_invariant("WallNeedOutlivesTxClose", &taken));

    // The judge refutes it: the wall closes `out=retracted`, its settle is
    // disarmed, and the turn waits for the screen's next word.
    let mut rig = drive(&[
        Step::See(Verdict::Busy),
        Step::See(Verdict::Wall(WallKind::UsageSession)),
    ]);
    let armed = project(&rig, None);
    assert_eq!(armed["pend"], 1);
    checked(&mut rig, Step::TxRefute);
    let shut = project(&rig, None);
    assert_eq!(
        (shut["turn"], shut["step"], shut["pend"], shut["rf"]),
        (1, 0, 0, 1)
    );
    assert!(
        rig.tracker.wake_at().is_none(),
        "the wall's settle stayed armed"
    );
    assert!(!rig.wall_gone, "the retraction is the wall's own close");
    let mut kept = shut.clone();
    kept.insert("pend", 1);
    assert!(aterm_spec::interp::admits(&m, &armed, &kept).is_none());
    assert!(
        buggy
            .successors("RefuteKeepsSettle", &armed)
            .contains(&kept)
    );
    assert!(!m.check_invariant("RefutedWallStaysShut", &kept));
    // Its turn now ends in the transcript as done.
    checked(&mut rig, Step::TxEnd);
    assert_eq!(project(&rig, None)["cw"], 12);
}

/// NEGATIVE CONTROLS for who dismissed a box (ruling T1): each mutant's
/// successor, hand-built from a real state, is one the model refuses, the
/// `Buggy = 2` family's mutant admits, and its law breaks; the real tracker
/// takes the model's step from the same state instead.
#[test]
fn turn_tracker_conformance_rejects_inputs_that_bind_the_wrong_box() {
    let m = turn_tracker_model();
    let buggy = with_buggy(&m, 2);
    let edit = Verdict::Prompt(BoxWord::from_detail("edit"));
    let bash = Verdict::Prompt(BoxWord::from_detail("bash:not-read-only"));
    let caught = |name: &str, law: &str, from: &State, bad: &State| {
        assert!(
            aterm_spec::interp::admits(&m, from, bad).is_none(),
            "the model admits {name}'s successor"
        );
        assert!(
            buggy.successors(name, from).contains(bad),
            "{name} is not the successor"
        );
        assert!(
            !m.check_invariant(law, bad),
            "{law} holds at {name}'s successor"
        );
    };

    // Admitted under box 1, accepted after one observe replaced it with box
    // 2: the real acceptance binds nothing; binding box 2 is ActBindsNextBox.
    let mut rig = drive(&[
        Step::See(Verdict::Busy),
        Step::See(edit),
        Step::Admit(Actor::Harness, Some(ActWord::Approve)),
        Step::See(bash),
    ]);
    let late = project(&rig, None);
    assert_eq!((late["step"], late["pd"]), (1, 4));
    assert_eq!(
        rig.accept().map(|b| matches!(b, Binding::Late(_))),
        Some(true)
    );
    assert_eq!(
        project(&rig, None)["bk"],
        0,
        "a late acceptance bound box 2"
    );
    let mut bad = late.clone();
    for (k, v) in [
        ("bk", 1),
        ("ba", late["pa"]),
        ("bs", late["pa"]),
        ("pa", 0),
        ("pd", 0),
        ("gn", 1),
    ] {
        bad.insert(k, v);
    }
    caught("ActBindsNextBox", "ActNeverBindsNextBox", &late, &bad);

    // A harness approve accepted 600 ms before the box goes: the close reads
    // `by=- out=passed`; reading it approved is AttributeStaleInput.
    let mut rig = drive(&[
        Step::See(Verdict::Busy),
        Step::See(edit),
        Step::Admit(Actor::Harness, Some(ActWord::Approve)),
        Step::Accept,
        Step::Pass(600),
    ]);
    let stale = project(&rig, None);
    assert_eq!((stale["step"], stale["bk"]), (1, 3));
    let seq0 = rig.log.last_seq();
    checked(&mut rig, Step::See(Verdict::Busy));
    let close = rig
        .since(seq0)
        .into_iter()
        .find(|r| r.op == Op::Close && r.k == Kind::Approval)
        .expect("the box closed");
    assert_eq!((close.out(), close.by), (Some("passed"), Actor::Nobody));
    assert_eq!(
        close.f.get_word(Key::Act),
        Some("approve"),
        "the close names the orphaned act"
    );
    let mut bad = m.successors("SeeBusy", &stale).remove(0);
    bad.insert("ga", 1);
    caught(
        "AttributeStaleInput",
        "ApprovalByObservedInput",
        &stale,
        &bad,
    );

    // A person's key after the harness's approve: the person's input is
    // the box's latest, and the close reads `by=human out=passed`; leaving
    // the act bound is ActOverPerson.
    let mut rig = drive(&[
        Step::See(Verdict::Busy),
        Step::See(edit),
        Step::Admit(Actor::Harness, Some(ActWord::Approve)),
        Step::Accept,
    ]);
    let acted = project(&rig, None);
    assert_eq!(acted["bk"], 1);
    checked(&mut rig, Step::Person);
    assert_eq!(project(&rig, None)["bk"], 2);
    let seq0 = rig.log.last_seq();
    checked(&mut rig, Step::See(Verdict::Busy));
    let close = rig
        .since(seq0)
        .into_iter()
        .find(|r| r.op == Op::Close && r.k == Kind::Approval)
        .expect("the box closed");
    assert_eq!((close.out(), close.by), (Some("passed"), Actor::Human));
    let mut bad = acted.clone();
    bad.insert("pp", 1);
    caught("ActOverPerson", "PersonSupersedesAct", &acted, &bad);

    // A harness approve accepted on the lead (no box open yet), then a
    // person's key before the box shows: the person's is the latest input,
    // so it takes the lead and the box it leads to closes `by=human
    // out=passed`; leaving the act on the lead to bind that box is
    // ActLeadOverPerson. NEGATIVE CONTROL (by scratch revert): a `person`
    // that binds only an open step closed it `out=approved by=harness`.
    let mut rig = drive(&[
        Step::See(Verdict::Busy),
        Step::Admit(Actor::Harness, Some(ActWord::Approve)),
        Step::Accept,
    ]);
    let led = project(&rig, None);
    assert_eq!((led["step"], led["pd"], led["pl"]), (0, 3, 0));
    checked(&mut rig, Step::Pass(100));
    checked(&mut rig, Step::Person);
    let took = project(&rig, None);
    assert_eq!((took["pd"], took["pl"]), (0, 1), "the person took the lead");
    checked(&mut rig, Step::Pass(200));
    checked(&mut rig, Step::See(edit));
    assert_eq!(project(&rig, None)["bk"], 2, "the box is the person's");
    checked(&mut rig, Step::Pass(100));
    let seq0 = rig.log.last_seq();
    checked(&mut rig, Step::See(Verdict::Busy));
    let close = rig
        .since(seq0)
        .into_iter()
        .find(|r| r.op == Op::Close && r.k == Kind::Approval)
        .expect("the box closed");
    assert_eq!((close.out(), close.by), (Some("passed"), Actor::Human));
    let mut bad = led.clone();
    bad.insert("pl", 1);
    caught("ActLeadOverPerson", "PersonSupersedesAct", &led, &bad);

    // The harness's approve closes box 1 `approved` and is spent: box 2
    // opens unbound; keeping the binding for it is ActTwice.
    let mut rig = drive(&[
        Step::See(Verdict::Busy),
        Step::See(edit),
        Step::Admit(Actor::Harness, Some(ActWord::Approve)),
        Step::Accept,
    ]);
    let bound = project(&rig, None);
    let seq0 = rig.log.last_seq();
    checked(&mut rig, Step::See(bash));
    let after = project(&rig, None);
    assert_eq!((after["step"], after["bk"], after["ab"]), (1, 0, 0));
    let close = rig
        .since(seq0)
        .into_iter()
        .find(|r| r.op == Op::Close && r.k == Kind::Approval)
        .expect("box 1 closed");
    assert_eq!((close.out(), close.by), (Some("approved"), Actor::Harness));
    let mut bad = m.successors("SeeNextBox", &bound).remove(0);
    for (k, v) in [
        ("bk", 1),
        ("ba", bound["ba"]),
        ("bs", bound["bs"]),
        ("ab", 1),
    ] {
        bad.insert(k, v);
    }
    caught("ActTwice", "AttributesOnce", &bound, &bad);

    // 6A-5: an act binds as the actor who sent it. The harness's approve
    // closes its box `approved by=harness`; reading it as ctl's
    // (`ActorSwap`) breaks HarnessActNotCtl. A ctl approve, bound under the
    // box or on the lead, closes `passed by=ctl` (ctl is no proven author);
    // reading it as the harness's breaks ActorKeepsItsAct.
    assert_eq!((bound["bk"], bound["ba"], bound["bs"]), (1, 2, 2));
    let bad = with(&bound, &[("ba", 1)]);
    caught("ActorSwap", "HarnessActNotCtl", &bound, &bad);
    for lead in [false, true] {
        let mut steps = vec![Step::See(Verdict::Busy)];
        if !lead {
            steps.push(Step::See(edit));
        }
        steps.extend([
            Step::Admit(Actor::Ctl, Some(ActWord::Approve)),
            Step::Accept,
        ]);
        if lead {
            steps.push(Step::See(edit));
        }
        let mut rig = drive(&steps);
        let ctl = project(&rig, None);
        assert_eq!(
            (ctl["bk"], ctl["ba"], ctl["bs"], ctl["pa"]),
            (1, 1, 1, 0),
            "a ctl act binds as ctl's (on the lead: {lead})"
        );
        let seq0 = rig.log.last_seq();
        checked(&mut rig, Step::See(Verdict::Busy));
        let close = rig
            .since(seq0)
            .into_iter()
            .find(|r| r.op == Op::Close && r.k == Kind::Approval)
            .expect("the box closed");
        assert_eq!((close.out(), close.by), (Some("passed"), Actor::Ctl));
        let bad = with(&ctl, &[("ba", 2)]);
        caught("ActorSwap", "ActorKeepsItsAct", &ctl, &bad);
    }
}

/// `s` with `kv` set.
fn with(s: &State, kv: &[(&'static str, i64)]) -> State {
    let mut b = s.clone();
    for (k, v) in kv {
        b.insert(k, *v);
    }
    b
}

/// The fold's `appr_by` (human, harness, ctl, other) counts each approval
/// by the input that dismissed it (T1): a person's key, the harness's
/// approve, a control client's approve, and a box no input dismissed.
#[test]
fn the_fold_counts_each_approval_by_who_dismissed_it() {
    let edit = Verdict::Prompt(BoxWord::from_detail("edit"));
    let mut steps = vec![Step::See(Verdict::Busy)];
    for input in [
        vec![Step::Person],
        vec![
            Step::Admit(Actor::Harness, Some(ActWord::Approve)),
            Step::Accept,
        ],
        vec![
            Step::Admit(Actor::Ctl, Some(ActWord::Approve)),
            Step::Accept,
        ],
        vec![],
    ] {
        steps.push(Step::See(edit));
        steps.extend(input);
        steps.push(Step::See(Verdict::Busy));
    }
    let rig = drive(&steps);
    assert_eq!(rig.log.state().totals.appr_by, [1, 1, 1, 1]);
    assert_eq!(rig.log.state().totals.appr, 4);
}
