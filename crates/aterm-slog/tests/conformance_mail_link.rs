// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance: the heuristic mail → turn link
//! ([`aterm_slog::Fold::mail_link`], `fold_mail.rs`; session log phase 5,
//! plan 5D-5, rulings T10, T11, T36, T39) against the derived
//! `SessionLogMailLink` model
//! (`aterm_models_session::session_log_mail_link_model`, checked at Tier-0
//! in `aterm-models/tests/derived_ring_ty.rs`).
//!
//! A seeded walk writes real records into a real [`Log`]: delivered mail
//! (`mail dir=in`, `kind=task` or `kind=note`), a person's `human` mark
//! and one no program-bound input came with (`reached=0`), turns of `ctl`
//! (no cause), of `ctl` naming its cause (`cause=@<off>` of
//! the newest waiting mail, or a record's seq), and of the person
//! (`by=human`), their closes; time passes past the window (`Age`). The
//! projection is the log's own fold: the mails it holds waiting (inside the
//! window or past it, by the walk's clock), the newest turn's link, its
//! confidence, its cause and its author, and the totals' `mail_amb=`. The
//! ghosts are what the walk knows of each mail it sent (waiting, taken by a
//! person's act, linked, or gone) and whether a link the fold made names a
//! mail it must not: one a person's act took, one past the window, one
//! already linked, a note, or one of two. After every step the real state
//! is projected onto the model and the step must be the model's.
//!
//! NEGATIVE CONTROLS (T11), one per mutant: each replays its witness over
//! the real fold, shows with the real rule ([`mail_link_of`] over what the
//! broken fold would hold or ask) that the broken call links, and hands the
//! model the successor that link makes; the honest action never makes it,
//! the mutant does, and its law fails there.
//!
//! THE CARRY (T10a): a turn's link crosses a seamless update on its own
//! `fold` line; a carry without the `ml` tokens (3B-shaped) reads with no
//! link, the same state otherwise, and the same brief, byte for byte.

use aterm_models_session::session_log_mail_link_model;
use aterm_slog::{
    Actor, CarryAdopt, CarryBudget, Close, Conf, Fields, Key, Kind, Limits, Log,
    MAIL_LINK_WINDOW_MS, MailLink, MailLinkOf, MailWaiting, Mark, Node, Open, Out, Rec, Src,
    TurnOut, mail_in_window, mail_link_of,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, with_buggy};
use aterm_spec::rng::XorShift;

use crate::support::{WALL0, agent_session, at};

/// What the walk knows of a mail it sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    /// No turn or person's act has taken it.
    Waiting,
    /// A person's act took it (a `human` mark or a person's turn).
    Person,
    /// A turn took it: linked heuristically, or named by `cause=`.
    Linked,
    /// Weighed, aged or forgotten.
    Gone,
}

#[derive(Clone, Copy, Debug)]
struct Sent {
    off: u64,
    task: bool,
    /// Delivered at (wall ms).
    t: u64,
    fate: Fate,
}

impl Sent {
    fn inside(&self, now: u64) -> bool {
        mail_in_window(
            &MailWaiting {
                off: self.off,
                t: self.t,
            },
            now,
        )
    }
}

struct World {
    log: Log,
    run: Node,
    clock: u64,
    next_off: u64,
    sent: Vec<Sent>,
    turn: Option<Node>,
    ga: i64,
    gw: i64,
    g2: i64,
    gn: i64,
    gm: i64,
}

fn conf_code(c: Conf) -> i64 {
    if c == Conf::Heuristic { 1 } else { 2 }
}

impl World {
    fn new() -> World {
        let mut log = Log::new(0);
        let run = agent_session(&mut log, 0);
        World {
            log,
            run,
            clock: 0,
            next_off: 3,
            sent: Vec::new(),
            turn: None,
            ga: 0,
            gw: 0,
            g2: 0,
            gn: 0,
            gm: 0,
        }
    }

    fn now(&self) -> u64 {
        WALL0 + self.clock
    }

    fn stamp(&mut self) -> aterm_slog::Stamp {
        self.clock += 1;
        at(self.clock)
    }

    /// The task mails waiting inside the window, as the walk sent them.
    fn waiting_inside(&self) -> Vec<usize> {
        let now = self.now();
        (0..self.sent.len())
            .filter(|&i| {
                let s = &self.sent[i];
                s.task && s.fate == Fate::Waiting && s.inside(now)
            })
            .collect()
    }

    fn project(&self) -> State {
        let fold = self.log.fold();
        let now = self.now();
        let waiting = fold.mail_waiting();
        let cands = waiting.iter().filter(|m| mail_in_window(m, now)).count();
        let old = waiting.iter().any(|m| !mail_in_window(m, now));
        let st = fold.state();
        let (t, node, ex, th) = match (&st.open[1], &st.last[1]) {
            (Some(o), _) => (
                1,
                Some(o.node),
                o.f.held(Key::Cause).is_some(),
                o.by == Actor::Human,
            ),
            (None, Some(c)) => (
                2,
                Some(c.node),
                c.f.held(Key::Cause).is_some(),
                c.opened_by == Actor::Human,
            ),
            (None, None) => (0, None, false, false),
        };
        assert_eq!(node, self.turn, "the fold's newest turn is the walk's");
        let (lk, lc) = node
            .and_then(|n| fold.mail_link(n))
            .map_or((0, 0), |l| (1, conf_code(l.conf)));
        let any = |f: &dyn Fn(&Sent) -> bool| i64::from(self.sent.iter().any(f));
        [
            ("cands", i64::try_from(cands).expect("small")),
            ("old", i64::from(old)),
            ("t", t),
            ("lk", lk),
            ("lc", lc),
            ("ex", i64::from(ex)),
            ("th", i64::from(th)),
            ("amb", i64::from(st.totals.mail_amb.min(1))),
            (
                "hm",
                any(&|s| s.task && s.fate == Fate::Person && s.inside(now)),
            ),
            (
                "notes",
                any(&|s| !s.task && s.fate == Fate::Waiting && s.inside(now)),
            ),
            (
                "lm",
                any(&|s| s.task && s.fate == Fate::Linked && s.inside(now)),
            ),
            ("ga", self.ga),
            ("gw", self.gw),
            ("g2", self.g2),
            ("gn", self.gn),
            ("gm", self.gm),
        ]
        .into_iter()
        .collect()
    }

    fn mail(&mut self, task: bool) {
        let stamp = self.stamp();
        let off = self.next_off;
        self.next_off += 1;
        let f = Fields::new()
            .word(Key::Dir, "in")
            .and_then(|f| f.word(Key::MailKind, if task { "task" } else { "note" }))
            .and_then(|f| f.u(Key::Off, off))
            .expect("mail fields");
        self.log
            .mark(
                Mark::new(Kind::Mail, Src::Fabric, Conf::Exact)
                    .at(Node::SESSION)
                    .by(Actor::Bridge)
                    .fields(f),
                stamp,
            )
            .expect("the mail mark");
        self.sent.push(Sent {
            off,
            task,
            t: WALL0 + self.clock,
            fate: Fate::Waiting,
        });
    }

    /// A person acted: what waited is the person's, the rest forgotten.
    fn person_took(&mut self) {
        let now = self.now();
        for s in &mut self.sent {
            s.fate = match s.fate {
                Fate::Waiting | Fate::Person if s.task && s.inside(now) => Fate::Person,
                _ => Fate::Gone,
            };
        }
    }

    /// What the walk makes of the link `link` the turn opened at `turn_t`
    /// was given, `inside` the task mails waiting in its window when it
    /// opened.
    fn judge(&mut self, inside: &[usize], link: Option<MailLink>, turn_t: u64) {
        let Some(l) = link else { return };
        let s = *self
            .sent
            .iter()
            .find(|s| s.off == l.off)
            .expect("a link names a mail the walk sent");
        self.gn |= i64::from(!s.task);
        self.ga |= i64::from(s.fate == Fate::Person);
        self.g2 |= i64::from(s.fate == Fate::Linked);
        self.gw |= i64::from(!s.inside(turn_t));
        self.gm |= i64::from(inside.len() >= 2);
    }

    fn open_turn(&mut self, by: Actor, src: Src, f: Fields) -> Node {
        let stamp = self.stamp();
        let node = self
            .log
            .open(
                Open::new(Kind::Turn, src, Conf::Strong)
                    .under(self.run)
                    .by(by)
                    .fields(f),
                stamp,
            )
            .expect("the turn opens");
        self.turn = Some(node);
        node
    }

    /// One real step and the model's name for it, when it is enabled.
    fn step(&mut self, m: &Model, rng: &mut XorShift) -> Option<&'static str> {
        const ACTIONS: [&str; 14] = [
            "TaskMail",
            "NoteMail",
            "Human",
            "HumanNoReach",
            "Age",
            "OpenTurn",
            "OpenCausedTurn",
            "OpenPersonTurn",
            "CloseTurn",
            // More likely: two waiting, and a turn's close before the next.
            "TaskMail",
            "TaskMail",
            "OpenTurn",
            "CloseTurn",
            "CloseTurn",
        ];
        let pick = ACTIONS[rng.index(ACTIONS.len())];
        if !m.action_enabled(pick, &self.project()) {
            return None;
        }
        self.real(pick);
        Some(pick)
    }

    /// The real records (or time) of model action `pick`.
    fn real(&mut self, pick: &str) {
        match pick {
            "TaskMail" => self.mail(true),
            "NoteMail" => self.mail(false),
            "Human" => {
                let stamp = self.stamp();
                self.log
                    .mark(
                        Mark::new(Kind::Human, Src::Input, Conf::Strong)
                            .at(Node::SESSION)
                            .by(Actor::Human),
                        stamp,
                    )
                    .expect("the human mark");
                self.person_took();
            }
            // A burst's first gesture that reached no program (a view
            // scroll): its mark says so, and nothing is taken.
            "HumanNoReach" => {
                let stamp = self.stamp();
                self.log
                    .mark(
                        Mark::new(Kind::Human, Src::Input, Conf::Strong)
                            .at(Node::SESSION)
                            .by(Actor::Human)
                            .fields(Fields::new().u(Key::Reached, 0).expect("reached=0")),
                        stamp,
                    )
                    .expect("the human mark");
            }
            "Age" => self.clock += MAIL_LINK_WINDOW_MS + 1000,
            "OpenTurn" => {
                let inside = self.waiting_inside();
                let node = self.open_turn(Actor::Ctl, Src::Ctl, Fields::new());
                let link = self.log.fold().mail_link(node);
                self.judge(&inside, link, self.now());
                if inside.len() >= 2 {
                    for &i in &inside {
                        self.sent[i].fate = Fate::Gone;
                    }
                }
                if let Some(l) = link
                    && let Some(s) = self.sent.iter_mut().find(|s| s.off == l.off)
                {
                    s.fate = Fate::Linked;
                }
            }
            "OpenCausedTurn" => {
                let named = self.waiting_inside().last().copied();
                let cause = named.map_or_else(
                    || self.log.last_seq().to_string(),
                    |i| format!("@{}", self.sent[i].off),
                );
                let f = Fields::new().cause(&cause).expect("a cause");
                let node = self.open_turn(Actor::Ctl, Src::Ctl, f);
                let link = self.log.fold().mail_link(node);
                self.judge(&[], link, self.now());
                if let Some(i) = named {
                    self.sent[i].fate = Fate::Linked;
                }
            }
            "OpenPersonTurn" => {
                let node = self.open_turn(Actor::Human, Src::Input, Fields::new());
                let link = self.log.fold().mail_link(node);
                self.judge(&[], link, self.now());
                self.person_took();
            }
            "CloseTurn" => {
                let stamp = self.stamp();
                self.log
                    .close(
                        self.turn.expect("a turn"),
                        Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
                        stamp,
                    )
                    .expect("the turn closes");
            }
            other => panic!("no real step for {other}"),
        }
    }

    /// The newest record: the turn open a step just wrote.
    fn newest(&self) -> Rec {
        self.log.window().last().cloned().expect("a record")
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

#[test]
fn mail_link_conforms_to_derived_model() {
    let m = session_log_mail_link_model();
    let mut ran = std::collections::BTreeSet::new();
    let mut seen = std::collections::BTreeSet::new();
    for seed in 1..=400u64 {
        let mut rng = XorShift::new(seed);
        let mut w = World::new();
        assert_eq!(w.project(), m.init_state(), "the empty fold is the model's");
        let mut last = "";
        for _ in 0..40 {
            let prev = w.project();
            if let Some(action) = w.step(&m, &mut rng) {
                let next = w.project();
                admitted(&m, action, &prev, &next);
                ran.insert(action);
                let opened = action == "OpenTurn";
                let past_view = std::mem::replace(&mut last, action) == "HumanNoReach";
                for (case, hit) in [
                    ("linked", opened && next["lk"] == 1),
                    ("ambiguous", opened && prev["cands"] == 2),
                    (
                        "caused by mail",
                        action == "OpenCausedTurn" && prev["cands"] > 0,
                    ),
                    ("after a person took mail", opened && prev["hm"] == 1),
                    ("past the window", opened && prev["old"] == 1),
                    ("beside a note", opened && prev["notes"] == 1),
                    (
                        "after a link",
                        opened && prev["lm"] == 1 && prev["cands"] == 0,
                    ),
                    (
                        "linked past a gesture that reached no program",
                        opened && past_view && next["lk"] == 1,
                    ),
                ] {
                    if hit {
                        seen.insert(case);
                    }
                }
            }
        }
    }
    assert_eq!(ran.len(), 9, "every action driven: {ran:?}");
    assert_eq!(seen.len(), 8, "every witness reached: {seen:?}");
}

/// The world after `path` of the model's actions, each checked.
fn world_after(m: &Model, path: &[&str]) -> World {
    let mut w = World::new();
    for action in path {
        let prev = w.project();
        assert!(m.action_enabled(action, &prev), "{action}: {prev:?}");
        w.real(action);
        admitted(m, action, &prev, &w.project());
    }
    w
}

/// The honest `action` from `w`: its pre- and post-state, checked, and the
/// turn open it wrote.
fn honest(m: &Model, w: &mut World, action: &str) -> (State, State, Rec) {
    let prev = w.project();
    w.real(action);
    let next = w.project();
    admitted(m, action, &prev, &next);
    (prev, next, w.newest())
}

/// The broken state `broken` is not the honest `action`'s, it is exactly
/// what mutant `mutant` makes of `prev`, and law `law` fails there.
fn rejected(m: &Model, (action, mutant, law): (&str, &str, &str), prev: &State, broken: &State) {
    assert!(
        !m.successors(action, prev).contains(broken),
        "the broken `{action}` passed as the model's: {broken:?}"
    );
    assert!(
        with_buggy(m, 1).successors(mutant, prev).contains(broken),
        "`{mutant}` is not the broken call it names:\n  from {prev:?}\n  to   {broken:?}"
    );
    assert!(!m.check_invariant(law, broken), "{law} holds at {broken:?}");
}

/// `honest` with `changes`.
fn with(honest: &State, changes: &[(&'static str, i64)]) -> State {
    let mut s = honest.clone();
    for (k, v) in changes {
        s.insert(k, *v);
    }
    s
}

/// NEGATIVE CONTROLS: each mutant's broken call over the real fold, from its
/// witness, makes a state the honest action never does, and exactly the one
/// its mutant does.
#[test]
fn mail_link_conformance_rejects_each_broken_call() {
    let m = session_log_mail_link_model();

    // A link at the turn's own confidence: the real rule links the one
    // waiting mail, and the turn is `strong`; the link written at that.
    let mut w = world_after(&m, &["TaskMail"]);
    let (prev, next, turn) = honest(&m, &mut w, "OpenTurn");
    let link = w.log.fold().mail_link(turn.node).expect("linked");
    assert_eq!((link.conf, turn.conf), (Conf::Heuristic, Conf::Strong));
    let broken = with(&next, &[("lc", conf_code(turn.conf))]);
    rejected(
        &m,
        ("OpenTurn", "LinkAtTurnConf", "HeuristicStaysHeuristic"),
        &prev,
        &broken,
    );

    // The rule without its `cause=` test: the caused turn, its cause not
    // read, links the mail it named.
    let mut w = world_after(&m, &["TaskMail"]);
    let waiting = w.log.fold().mail_waiting().to_vec();
    let (prev, next, turn) = honest(&m, &mut w, "OpenCausedTurn");
    assert_eq!(mail_link_of(&waiting, &turn), MailLinkOf::None);
    assert_eq!(w.log.fold().mail_link(turn.node), None);
    let bare = Rec {
        f: Fields::new(),
        ..turn.clone()
    };
    assert_eq!(
        mail_link_of(&waiting, &bare),
        MailLinkOf::Link(waiting[0].off)
    );
    let broken = with(&next, &[("lk", 1), ("lc", 1)]);
    rejected(
        &m,
        ("OpenCausedTurn", "LinkOverExplicit", "ExplicitCauseWins"),
        &prev,
        &broken,
    );

    // A `human` mark that does not clear the waiting list: the turn after
    // it links the mail the person's act took.
    let mut w = world_after(&m, &["TaskMail"]);
    let before = w.log.fold().mail_waiting().to_vec();
    w.real("Human");
    assert!(w.log.fold().mail_waiting().is_empty(), "a person acted");
    let (prev, next, turn) = honest(&m, &mut w, "OpenTurn");
    assert_eq!(prev["hm"], 1);
    assert_eq!(
        mail_link_of(&before, &turn),
        MailLinkOf::Link(before[0].off)
    );
    let broken = with(&next, &[("lk", 1), ("lc", 1), ("ga", 1), ("hm", 0)]);
    rejected(
        &m,
        ("OpenTurn", "LinkAcrossHuman", "NoLinkAcrossHuman"),
        &prev,
        &broken,
    );

    // The rule without its window: the mail delivered past it, still
    // listed, linked (the turn read as if it opened when the mail came).
    let mut w = world_after(&m, &["TaskMail", "Age"]);
    let waiting = w.log.fold().mail_waiting().to_vec();
    let (prev, next, turn) = honest(&m, &mut w, "OpenTurn");
    assert_eq!(prev["old"], 1);
    assert!(!mail_in_window(&waiting[0], turn.t));
    assert_eq!(mail_link_of(&waiting, &turn), MailLinkOf::None);
    let undated = Rec {
        t: waiting[0].t,
        ..turn.clone()
    };
    assert_eq!(
        mail_link_of(&waiting, &undated),
        MailLinkOf::Link(waiting[0].off)
    );
    let broken = with(&next, &[("lk", 1), ("lc", 1), ("gw", 1)]);
    rejected(
        &m,
        ("OpenTurn", "LinkPastWindow", "LinkWithinWindow"),
        &prev,
        &broken,
    );

    // A linked turn that does not take its mail off the list: the next
    // turn links it again.
    let mut w = world_after(&m, &["TaskMail"]);
    let waiting = w.log.fold().mail_waiting().to_vec();
    w.real("OpenTurn");
    assert!(w.log.fold().mail_waiting().is_empty(), "the turn took it");
    w.real("CloseTurn");
    let (prev, next, turn) = honest(&m, &mut w, "OpenTurn");
    assert_eq!((prev["lm"], next["lk"]), (1, 0));
    assert_eq!(
        mail_link_of(&waiting, &turn),
        MailLinkOf::Link(waiting[0].off)
    );
    let broken = with(&next, &[("lk", 1), ("lc", 1), ("g2", 1)]);
    rejected(
        &m,
        ("OpenTurn", "MailLinksTwice", "OneMailOneTurn"),
        &prev,
        &broken,
    );

    // A mail of any kind waiting: the note, listed, linked.
    let mut w = world_after(&m, &["NoteMail"]);
    let note = w.newest();
    assert!(
        w.log.fold().mail_waiting().is_empty(),
        "a note waits for none"
    );
    let listed = [MailWaiting {
        off: note.f.get_u(Key::Off).expect("off"),
        t: note.t,
    }];
    let (prev, next, turn) = honest(&m, &mut w, "OpenTurn");
    assert_eq!(
        mail_link_of(&listed, &turn),
        MailLinkOf::Link(listed[0].off)
    );
    let broken = with(&next, &[("lk", 1), ("lc", 1), ("gn", 1), ("notes", 0)]);
    rejected(
        &m,
        ("OpenTurn", "LinkNote", "OnlyTaskLinks"),
        &prev,
        &broken,
    );

    // The rule taking the first of two: the older linked, no ambiguity
    // counted.
    let mut w = world_after(&m, &["TaskMail", "TaskMail"]);
    let waiting = w.log.fold().mail_waiting().to_vec();
    let (prev, next, turn) = honest(&m, &mut w, "OpenTurn");
    assert_eq!(mail_link_of(&waiting, &turn), MailLinkOf::Ambiguous);
    assert_eq!(w.log.fold().state().totals.mail_amb, 1);
    assert_eq!(
        mail_link_of(&waiting[..1], &turn),
        MailLinkOf::Link(waiting[0].off)
    );
    let broken = with(
        &next,
        &[
            ("lk", 1),
            ("lc", 1),
            ("gm", 1),
            ("lm", 1),
            ("amb", prev["amb"]),
        ],
    );
    rejected(
        &m,
        ("OpenTurn", "LinkAmongMany", "AmbiguityLinksNone"),
        &prev,
        &broken,
    );
}

/// Every `ml`-prefixed and `mail_amb=` token taken off `lines`: the carry a
/// 3B build writes.
fn strip_links(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            l.split(' ')
                .filter(|w| !(w.starts_with("ml") || w.starts_with("mail_amb=")))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

fn adopt_at(log: &Log, ms: u64) -> CarryAdopt {
    CarryAdopt {
        ep: log.ep() + 1,
        next_seq: log.last_seq() + 1000,
        ids: log.id_counters(),
        limits: Limits::SESSION,
        at: at(ms),
    }
}

/// THE CARRY (T10a): a turn's link (open, then closed) and the ambiguity
/// count cross a seamless update on their own fold lines; a 3B-shaped carry
/// (no `ml` tokens, no `mail_amb=`) reads with neither and the same state
/// otherwise; a token this build does not know on the same line is skipped
/// (the R13 rule); and no brief, `log` line or record prints the link.
#[test]
fn a_carried_link_reads_back_and_a_3b_carry_reads_none() {
    let mut w = World::new();
    for a in ["TaskMail", "OpenTurn"] {
        w.real(a);
    }
    let turn = w.turn.expect("the turn");
    let link = w.log.fold().mail_link(turn).expect("linked");
    assert_eq!(link.conf, Conf::Heuristic);

    for closed in [false, true] {
        if closed {
            // An ambiguous turn, then a linked one, closed.
            for a in ["CloseTurn", "TaskMail", "TaskMail", "OpenTurn"] {
                w.real(a);
            }
            assert_eq!(w.log.fold().state().totals.mail_amb, 1);
            let turn = w.turn.expect("the ambiguous turn");
            assert_eq!(w.log.fold().mail_link(turn), None, "ambiguous");
            for a in ["CloseTurn", "TaskMail", "OpenTurn", "CloseTurn"] {
                w.real(a);
            }
        }
        let newest = w.turn.expect("the newest turn");
        let held = w.log.fold().mail_link(newest);
        assert!(held.is_some(), "the newest turn is linked");
        let out = w
            .log
            .carry(CarryBudget::SESSION, &[])
            .expect("the carry fits");
        let ml: Vec<&String> = out.lines.iter().filter(|l| l.contains(" mloff=")).collect();
        assert_eq!(ml.len(), 1, "{:?}", out.lines);
        let l = held.expect("held");
        assert!(ml[0].starts_with(if closed { "fold last 1 " } else { "fold open " }));
        assert!(ml[0].ends_with(&format!(" mloff={} mlconf=heuristic", l.off)));
        assert_eq!(
            out.lines.iter().any(|l| l.contains(" mail_amb=1")),
            closed,
            "the ambiguity rides the totals line"
        );
        let (back, _, _) =
            Log::from_carry(&out.lines, adopt_at(&w.log, 900_000)).expect("the carry reads");
        assert_eq!(back.fold().mail_link(newest), held, "the link crossed");

        if closed {
            let slot = out
                .lines
                .iter()
                .position(|l| l.starts_with("fold last 1 "))
                .expect("the newest closed turn's slot");
            for older in [false, true] {
                let mut duplicate = out.lines.clone();
                let extra = if older {
                    let old = duplicate[slot].replacen(
                        &format!("fold last 1 {newest} "),
                        &format!("fold last 1 {turn} "),
                        1,
                    );
                    assert_ne!(old, duplicate[slot], "an earlier turn in the same slot");
                    old
                } else {
                    duplicate[slot].clone()
                };
                duplicate.insert(slot, extra);
                // Admit the extra line through the outer framing, so the
                // refusal must come from the fold's duplicate-slot check.
                let count = duplicate.len();
                duplicate[0] = duplicate[0]
                    .split_whitespace()
                    .map(|w| {
                        if w.starts_with("lines=") {
                            format!("lines={count}")
                        } else {
                            w.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(matches!(
                    Log::from_carry(&duplicate, adopt_at(&w.log, 900_000)),
                    Err(aterm_slog::CarryError::Fold(aterm_slog::SnapError::Bound))
                ));
            }
        }

        let old = strip_links(&out.lines);
        let (b3, _, _) =
            Log::from_carry(&old, adopt_at(&w.log, 900_000)).expect("a 3B carry reads");
        assert_eq!(
            b3.fold().mail_link(newest),
            None,
            "a 3B carry holds no link"
        );
        assert_eq!(b3.fold().state().totals.mail_amb, 0);
        assert_eq!(
            back.fold().state().totals.mail_amb,
            u32::from(closed),
            "the ambiguity crossed"
        );
        let mut st = back.fold().state().clone();
        st.totals.mail_amb = 0;
        assert_eq!(&st, b3.fold().state(), "the tokens moved nothing else");

        let later: Vec<String> = out
            .lines
            .iter()
            .map(|l| {
                if l.contains(" mloff=") {
                    format!("{l} mlnext=9")
                } else {
                    l.clone()
                }
            })
            .collect();
        let (skips, _, _) = Log::from_carry(&later, adopt_at(&w.log, 900_000))
            .expect("an unknown ml token is skipped");
        assert_eq!(skips.fold(), back.fold());

        // HeuristicStaysHeuristic at the carry's reader: a link line naming
        // any other confidence is no line this build writes, and refuses
        // the snapshot, so a carried link never reads back certain.
        // NEGATIVE CONTROL: a reader that keeps `mlconf=` as it reads it
        // returned `MailLink { conf: Exact }` here.
        for wrong in ["exact", "strong", "claim", "unknown", "maybe"] {
            let planted: Vec<String> = out
                .lines
                .iter()
                .map(|l| l.replace(" mlconf=heuristic", &format!(" mlconf={wrong}")))
                .collect();
            assert_ne!(planted, out.lines);
            assert!(
                Log::from_carry(&planted, adopt_at(&w.log, 900_000)).is_err(),
                "a carried link read back mlconf={wrong}"
            );
        }

        let now = WALL0 + 900_000;
        assert_eq!(
            back.brief("s-1", 4000, now).text,
            b3.brief("s-1", 4000, now).text,
            "the link changes no brief byte"
        );
        let recs = |log: &Log| -> Vec<String> {
            log.window()
                .map(|r| aterm_slog::encode(r, aterm_slog::Tier::Content).expect("fits"))
                .collect()
        };
        assert_eq!(recs(&back), recs(&b3), "no record carries the link");
        assert!(
            recs(&w.log).iter().all(|l| !l.contains(" ml")),
            "no record names a link"
        );
    }
}
