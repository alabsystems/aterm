// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A LOOP THAT FOLLOWS ANOTHER BEHAVES AS THE FIRST LOOP, AT EVERY HANDOFF
//! (the owner's rule for the successor's seed: a seamless update, a new
//! window, a policy reload or a fault restart starts a loop on the
//! session's ledger). For each scenario — a usage window, the weekly
//! window, a spend limit, a model bucket with and without the fallback's
//! relaunch, the done check, the take window — the FIRST loop runs a
//! scripted timeline on the fixed clock. At every instant a handoff could
//! fall (just after each event and act, just before each act, inside long
//! gaps) a SUCCESSOR is started on the first loop's ledger as it stood then
//! (its rows restamped on the test's clock, so every `ts` the seed reads is
//! the instant it was written), seeded as `drive_loop` seeds
//! ([`Session::seed_from_ledger`], [`Session::seed_wall_memory`]), and fed
//! the rest of the timeline — and at every handoff a CHAIN too: that
//! successor run to the next handoff, where a third loop attaches on its
//! books ([`run_chain`]). The successor's acts are matched to the first
//! loop's (the same text within 40 s); the first that differs is the
//! cell's, and it must be no sooner than the first loop's act — not by a
//! second — the same text, never one where the first loop types nothing, and
//! never a wait with nothing due where the first loop acts; later is
//! fail-closed only while bounded by a fresh appearance's first rung (30
//! minutes from attaching, a person's standing draft's grace on top:
//! [`LATER_BOUND_S`]). The world answers an act once it is typed: an answer
//! to an act a successor typed later (inside the 40 s) comes as much later.
//! A successor the host started as Off lifted finds no park on its books:
//! the host stopped the loop before it under Off, and the row is its own to
//! write.
//! Where the take window is modelled, a successor attached between an act
//! and the first loop's first read after it first looks there (the read
//! latency is the world's). Time on the fixed clock moves the policy's
//! instants as a real clock would ([`TurnEndState::slept`] at every step,
//! the watch's marks moved with it so no sleep is read). The take window,
//! where a scenario models it, is production's 30 s ([`TAKE_S`]) on the
//! worker's clock, which the test's does not move: a read the model places
//! past it is decided with no window ([`Loop::take_clock`]), so the matrix
//! sleeps for none and decides the same every run (the scenarios run on the
//! machine's threads, each cell's loops its own). NEGATIVE CONTROL:
//! successors that take nothing up from the ledger type sooner, and the
//! matrix says so. An API retry wall's ladder (a 529) is outside the rule
//! and not seeded ([`TurnEndState::seed_acts`]); its scenario is not here.
//!
//! What the seed takes up, by the ledger's rows (the verifier of 341e1fa81,
//! RS7-1..7): an awaited act's take window from the first loop's first read
//! after it (its wall's row) or, nothing read yet, this loop's
//! ([`TurnEndState::seed_awaited`]); a try met again from where it was judged
//! met (that window's end); a try aged out over before this loop's first wall,
//! the wall standing ([`TurnEndState::take_try_over`],
//! [`crate::supervise::approvals::RULE_TRY_OVER`]); Off standing
//! ([`crate::supervise::approvals::RULE_OFF_STOOD`]), parked as the loop that
//! parked holds its books — a record, no read of a try; a task's end
//! ([`crate::supervise::approvals::RULE_TASK_DONE`]), the act before it
//! answered; the Land episode ([`crate::supervise::approvals::land_episode`]);
//! and the short streak ([`crate::supervise::approvals::short_streak`]): an
//! act's mark, its yield still to come, or the record of the last point a
//! loop judged ([`crate::supervise::approvals::RULE_POINT`], round 8: every
//! act's answer and every move of the streak; round 9: every move of the
//! point's time), that point's streak and time, and the answer of the check
//! or a land act judged there — unless a turn was read running after it
//! ([`crate::supervise::approvals::RULE_TURN_RAN`], round 9): the next idle
//! point is that turn's end, a later turn's. A person's exchange left to them
//! is in the ledger whenever it is left, the task's end said before it or not
//! (round 9). Round 9's gap cells ([`run_follower`] on books cut at a point
//! the loop before never read) are their own test, and so is the cut a loop's
//! start makes of a ledger past its bound
//! ([`crate::supervise::approvals::bound`]).

use super::*;
use crate::supervise::approvals::Ledger;
use std::fmt::Write as _;

const SID: &str = "@s-1";

#[derive(Clone, Debug)]
enum Ev {
    /// The screen shows a point: a turn's end, a wall.
    Point(Vec<String>),
    /// A live turn read busy.
    Busy(Vec<String>),
    /// The host's transcript of the agent changes (nothing drawn).
    Tail(Option<String>),
    /// Nothing happens: a handoff instant (one second after it) where the
    /// rest of the timeline has none.
    Mark,
    /// A person's keystroke in the window (`status human_ms=` from it on);
    /// nothing drawn.
    Key,
    /// The session's Autopilot stood at Off and lifted
    /// ([`TurnEndState::park_off`]); nothing drawn. A loop attached one
    /// second after it is the one the host started as Off lifted
    /// ([`Session::seed_from_ledger`]'s `lifted_off`).
    OffLifted,
}

#[derive(Clone, Copy, Debug)]
enum At {
    /// Seconds from the scenario's start.
    Start(i64),
    /// Seconds after the first loop's `k`-th act (0-based).
    Act(usize, i64),
}

struct Beat {
    at: At,
    label: String,
    ev: Ev,
}

fn beat(at: At, label: &str, ev: Ev) -> Beat {
    Beat {
        at,
        label: label.to_string(),
        ev,
    }
}

struct Scenario {
    name: String,
    t0: i64,
    policy: SupervisorConfig,
    /// The host's restart answers, and its transcript at the start; `None`:
    /// no host.
    host: Option<(Vec<&'static str>, Option<String>)>,
    beats: Vec<Beat>,
    /// Seconds past the last beat simulated.
    tail_s: i64,
    /// The take window is modelled ([`TAKE_S`]); else every point is
    /// judged at once.
    take: bool,
    /// Every screen read carries the person's last keystroke ([`Ev::Key`]),
    /// as production's reads carry `human_ms`: a point is judged by the stamp
    /// its own read carries, before any `status` is read. Else only the
    /// window's `status` says it.
    person_on_screen: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Act {
    at: i64,
    rule: String,
    text: String,
}

/// One loop over the scripted timeline.
struct Loop<'m> {
    s: Session<'m, Mock>,
    opts: SuperviseOpts,
    host: Option<Arc<AccountHost>>,
    ledger: PathBuf,
    read: usize,
    /// Every ledger row, stamped with the test's clock when it was written.
    stamped: Vec<(i64, String)>,
    /// The lines said and noted since the last look ([`Journaled`]), each
    /// with the count of ledger rows written before it.
    lines: Vec<(usize, String)>,
    /// Every line the loop said or noted, journaled as production journals
    /// it beside the ledger ([`crate::supervise::approvals::journal_beside`]),
    /// stamped where it fell among the rows.
    journal: Vec<(i64, String)>,
    acts: Vec<Act>,
    seq: u64,
    /// The wait standing (unix seconds).
    due: Option<i64>,
    busy_since: Option<i64>,
    /// The last point decided (what a wait's end decides again).
    seen: Option<Turn>,
    /// The screen standing.
    rows: Vec<String>,
    log: Vec<String>,
    /// The take window is modelled ([`Scenario::take`]).
    take: bool,
    /// The first point after the last act no busy read followed: when, on
    /// the test's clock.
    unseen: Option<i64>,
    /// An act awaited, no busy read since.
    awaiting: bool,
    /// A person's last keystroke ([`Ev::Key`]), on the test's clock.
    key_at: Option<i64>,
    /// Every screen read carries it ([`Scenario::person_on_screen`]).
    person_on_screen: bool,
}

/// The production take window ([`TurnEndTiming::take_within`]), on the
/// test's clock.
const TAKE_S: i64 = 30;

/// The take window a scenario's loops run with: production's where it is
/// modelled (the worker's clock, which the test's does not move, never runs
/// it out: [`Loop::take_clock`] ends it), else none.
fn take_within(take: bool) -> Duration {
    if take {
        Duration::from_secs(TAKE_S.unsigned_abs())
    } else {
        Duration::ZERO
    }
}

/// THE LOOP'S REVIEW, AS PRODUCTION'S JOURNALS: every line said or noted
/// goes to the session's journal (`Lines`, `StopAtReview`: `Sink::record`),
/// which the window's host keeps beside the ledger and a loop that follows
/// reads the landing from (`landed_before`). With no journal, the matrix
/// never met that seed (the merge review's MG9-S2). Each line is kept with
/// the count of ledger rows written before it, so [`Loop::collect`] stamps it
/// where it fell among them, as production's one clock orders them.
struct Journaled<'j> {
    ledger: &'j Path,
    /// The rows [`Loop::collect`] has stamped: the count a line takes where
    /// the file cannot be read.
    read: usize,
    lines: &'j mut Vec<(usize, String)>,
}

impl Journaled<'_> {
    fn record(&mut self, line: &str) {
        let rows = std::fs::read(self.ledger)
            .map_or(self.read, |b| b.iter().filter(|&&c| c == b'\n').count());
        self.lines.push((rows, line.to_string()));
    }
}

impl Review for Journaled<'_> {
    fn approved(&mut self, _seq: u64, _command: &str) -> Result<(), String> {
        Ok(())
    }
    fn review(
        &mut self,
        _turn: &Turn,
        _report: Option<ReportBrief>,
        _fold: Option<Fold>,
    ) -> Result<bool, String> {
        Ok(false)
    }
    fn say(&mut self, line: &str) -> Result<(), String> {
        self.record(line);
        Ok(())
    }
    fn note(&mut self, line: &str) {
        self.record(line);
    }
    fn unattended(&self) -> bool {
        false
    }
}

/// The rows of `stamped` (ledger rows or journal lines) written before `ms`.
fn before_ms(stamped: &[(i64, String)], ms: i64) -> Vec<(i64, String)> {
    stamped.iter().filter(|(t, _)| *t < ms).cloned().collect()
}

fn restamp(line: &str, ms: i64) -> String {
    let Some(rest) = line.strip_prefix("{\"ts\":") else {
        return line.to_string();
    };
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    format!("{{\"ts\":{ms}{}", &rest[digits..])
}

impl Loop<'_> {
    fn now(&self) -> i64 {
        self.s.now_unix()
    }

    fn advance(&mut self, to: i64) {
        let from_ms = self.s.now_unix_ms();
        if to * 1000 <= from_ms {
            return;
        }
        self.s.set_clock(to, PDT);
        let by = u64::try_from(to * 1000 - from_ms).unwrap_or(0);
        self.s.turn_end.slept(Duration::from_millis(by));
        // The person's last keystroke the last `status` read, and the
        // draft's last change, are as far back.
        let back = |p: &mut Instant| {
            if let Some(earlier) = p.checked_sub(Duration::from_millis(by)) {
                *p = earlier;
            }
        };
        if let Some(p) = self.s.person.as_mut() {
            back(p);
        }
        if let Some((_, p)) = self.s.draft_seen.as_mut() {
            back(p);
        }
        self.s.clock_marks = Some((Instant::now(), self.s.now_unix_ms()));
    }

    /// The rows written since the last look, stamped now; typed rows are
    /// acts.
    fn collect(&mut self) -> Vec<Act> {
        let body = std::fs::read_to_string(&self.ledger).unwrap_or_default();
        let lines: Vec<&str> = body.lines().collect();
        let now = self.now();
        let mut new = Vec::new();
        for (i, line) in lines.iter().enumerate().skip(self.read) {
            let ms = now * 1000 + i64::try_from(i - self.read).unwrap_or(0);
            self.stamped.push((ms, restamp(line, ms)));
            let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
                continue;
            };
            let field = |k: &str| {
                v.get(k)
                    .and_then(aterm_json::Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let rule = field("rule_id");
            self.log.push(format!(
                "    +{} row {} {} {:?} {:?}",
                now,
                field("decision"),
                rule,
                field("command"),
                field("reason")
            ));
            if field("decision") == "typed" {
                new.push(Act {
                    at: now,
                    rule,
                    text: field("command"),
                });
            }
        }
        // The lines said meanwhile, stamped as the next row after the ones
        // before them would be.
        for (rows, line) in std::mem::take(&mut self.lines) {
            let ms = now * 1000 + i64::try_from(rows.saturating_sub(self.read)).unwrap_or(0);
            self.journal.push((
                ms,
                crate::supervise::journal::JournalRecord::of_line(ms, Some(SID), &line, None)
                    .to_json(),
            ));
        }
        self.read = lines.len();
        self.acts.extend(new.clone());
        new
    }

    /// The take window, as the worker's clock would run it: a point decided
    /// [`TAKE_S`] (the test's clock) after the first unjudged one is past it
    /// — the window is zero for that read — and one inside is not.
    fn take_clock(&mut self) {
        let mut window = take_within(self.take);
        if self.take && self.awaiting {
            match self.unseen {
                Some(t1) if self.now() - t1 >= TAKE_S => window = Duration::ZERO,
                Some(_) => {}
                None => self.unseen = Some(self.now()),
            }
        }
        self.s.turn_end.timing.take_within = window;
    }

    /// The person as the window's status says it now ([`Ev::Key`]).
    fn status_person(&mut self) {
        let now = self.now();
        self.s.ctl.human_ms = self
            .key_at
            .map(|k| u64::try_from((now - k) * 1000).unwrap_or(0));
    }

    /// A point drawn now.
    fn point(&mut self, rows: Vec<String>) -> Vec<Act> {
        self.take_clock();
        self.status_person();
        self.seq += 1;
        self.rows.clone_from(&rows);
        self.s.ctl.screens = vec![rows.clone()];
        self.s.ctl.served = 0;
        let worked = self
            .busy_since
            .take()
            .map(|b| Duration::from_secs(u64::try_from(self.now() - b).unwrap_or(0)));
        let mut p = point_at(&self.s, rows, self.seq);
        // The read's own stamp ([`Scenario::person_on_screen`]).
        if self.person_on_screen
            && let Some(k) = self.key_at
        {
            p.screen.human = crate::supervise::screen::HumanInput::Ago(
                u64::try_from((self.now() - k) * 1000).unwrap_or(0),
            );
        }
        let a = self
            .s
            .turn_end_at_point(&p, worked, &self.opts)
            .unwrap_or(TurnEndAction::Nothing);
        self.log.push(format!(
            "  {} point seq={} worked={:?} -> {}",
            self.now(),
            self.seq,
            worked.map(|d| d.as_secs()),
            brief(&self.s, &a)
        ));
        let mut review = Journaled {
            ledger: &self.ledger,
            read: self.read,
            lines: &mut self.lines,
        };
        let _ = self.s.turn_end_execute(&p, a, &self.opts, &[], &mut review);
        self.seen = Some(p);
        self.after_act()
    }

    /// The standing wait ran out: the point is decided again.
    fn wake(&mut self) -> Vec<Act> {
        let Some(seen) = self.seen.clone() else {
            self.due = None;
            return Vec::new();
        };
        self.s.ctl.screens = vec![self.rows.clone()];
        self.s.ctl.served = 0;
        self.due = None;
        self.take_clock();
        self.status_person();
        let mut review = Journaled {
            ledger: &self.ledger,
            read: self.read,
            lines: &mut self.lines,
        };
        let _ = self.s.turn_end_now(&seen, &self.opts, &[], &mut review);
        let new = self.after_act();
        self.log.push(format!(
            "  {} wake -> due {:?}",
            self.now(),
            self.due.map(|u| u - self.now())
        ));
        new
    }

    fn after_act(&mut self) -> Vec<Act> {
        let new = self.collect();
        if !new.is_empty() {
            self.awaiting = true;
            self.unseen = None;
        }
        // The wait the decision (or a restart's tries) left standing; the
        // take window's on the production clock.
        let unseen = self.unseen;
        self.due = self.s.turn_end_due.as_ref().map(|d| {
            if self.take
                && d.why.starts_with("the worker to take")
                && let Some(t1) = unseen
            {
                return (t1 + TAKE_S).max(self.now() + 1);
            }
            let u = self.s.unix_of(d.at);
            u.max(self.now() + 1)
        });
        new
    }

    /// A live turn read busy now.
    fn busy(&mut self, rows: Vec<String>) {
        self.seq += 1;
        self.rows.clone_from(&rows);
        let reader = self.s.reader(&rows);
        self.s.end_memory_on_live_turn(reader, &rows, self.seq);
        self.s.took(self.seq);
        self.awaiting = false;
        self.unseen = None;
        self.busy_since.get_or_insert(self.now());
        self.due = None;
        self.s.turn_end_due = None;
        let _ = self.collect();
        self.log
            .push(format!("  {} busy seq={}", self.now(), self.seq));
    }

    fn tail(&mut self, t: Option<String>) {
        if let Some(h) = &self.host {
            *h.tail.lock().unwrap() = t;
        }
    }

    /// Run the standing waits that fall before `t`; the first act, if
    /// `stop`, ends the run.
    fn waits_before(&mut self, t: i64, stop: bool) -> Option<Act> {
        let mut n = 0;
        while let Some(u) = self.due {
            if u >= t || n > 400 {
                break;
            }
            n += 1;
            self.advance(u);
            let new = self.wake();
            if stop && let Some(a) = new.into_iter().next() {
                return Some(a);
            }
        }
        None
    }

    fn event(&mut self, ev: &Ev) -> Vec<Act> {
        match ev.clone() {
            Ev::Point(rows) => self.point(rows),
            Ev::Busy(rows) => {
                self.busy(rows);
                Vec::new()
            }
            Ev::Tail(t) => {
                self.tail(t);
                Vec::new()
            }
            Ev::Mark => Vec::new(),
            Ev::Key => {
                self.key_at = Some(self.now());
                Vec::new()
            }
            Ev::OffLifted => {
                // `park_while_off`'s fold: nothing read under Off is folded
                // in, and the park ends what it ends, its ledger row with it.
                self.s.off_stood(self.seq);
                self.collect()
            }
        }
    }
}

fn brief(s: &Session<'_, Mock>, a: &TurnEndAction) -> String {
    match a {
        TurnEndAction::WaitUntil { until, why, .. } => {
            format!(
                "wait +{}s ({})",
                s.unix_of(*until) - s.now_unix(),
                clip(why)
            )
        }
        TurnEndAction::Type { text, rule_id } => format!("type {rule_id} {:?}", clip(text)),
        TurnEndAction::Restart { rule_id, why, .. } => format!("restart {rule_id} {why:?}"),
        other => format!("{other:?}").chars().take(80).collect(),
    }
}

fn opts_for(sc: &Scenario, host: Option<&Arc<AccountHost>>) -> SuperviseOpts {
    SuperviseOpts {
        idle_host: host.map(|h| Arc::clone(h) as Arc<dyn IdleHost>),
        policy: sc.policy.clone(),
        ..auto(30, None)
    }
}

fn host_for(
    sc: &Scenario,
    answers: &[&'static str],
    tail: Option<String>,
) -> Option<Arc<AccountHost>> {
    sc.host.as_ref().map(|_| {
        let h = AccountHost::scripted(&[None], answers);
        *h.tail.lock().unwrap() = tail;
        h
    })
}

/// The first loop's run: its concrete timeline (absolute times), its acts,
/// and its ledger rows stamped.
struct First {
    /// (time, label, event, the act it answers: `None` for the world's).
    events: Vec<(i64, String, Ev, Option<usize>)>,
    acts: Vec<Act>,
    stamped: Vec<(i64, String)>,
    /// Its journal's lines, stamped ([`Loop::journal`]).
    journal: Vec<(i64, String)>,
    log: Vec<String>,
    /// When the run ends (unix seconds): its last event, and the
    /// scenario's tail past it.
    end: i64,
}

fn run_first(sc: &Scenario, dir: &Path) -> First {
    let ledger = dir.join("first.jsonl");
    let _ = std::fs::remove_file(&ledger);
    let host = host_for(
        sc,
        sc.host.as_ref().map_or(&[][..], |(a, _)| a.as_slice()),
        sc.host.as_ref().and_then(|(_, t)| t.clone()),
    );
    let opts = opts_for(sc, host.as_ref());
    let mut m = Mock::new(vec![Vec::new()]);
    let mut l = Loop {
        s: session(&mut m, Some(SID.to_string())),
        opts,
        host,
        ledger: ledger.clone(),
        read: 0,
        stamped: Vec::new(),
        lines: Vec::new(),
        journal: Vec::new(),
        acts: Vec::new(),
        seq: 100,
        due: None,
        busy_since: None,
        seen: None,
        rows: Vec::new(),
        log: Vec::new(),
        take: sc.take,
        unseen: None,
        awaiting: false,
        key_at: None,
        person_on_screen: sc.person_on_screen,
    };
    l.s.set_zone_lookup(test_zones);
    l.s.ledger = Ledger::open(Some(&ledger), Some(SID), &mut Vec::new());
    l.s.set_turn_end_timing(TurnEndTiming {
        take_within: take_within(sc.take),
        ..TurnEndTiming::default()
    });
    let first_at = sc
        .beats
        .iter()
        .filter_map(|b| match b.at {
            At::Start(dt) => Some(sc.t0 + dt),
            At::Act(..) => None,
        })
        .min()
        .unwrap_or(sc.t0);
    l.s.set_clock(first_at.min(sc.t0), PDT);
    l.s.turn_end_seeded = true;
    let mut events = Vec::new();
    let mut stamps = std::collections::BTreeMap::new();
    for b in &sc.beats {
        let t = match b.at {
            At::Start(dt) => sc.t0 + dt,
            At::Act(k, dt) => {
                // The act this beat answers: the loop's waits run until it
                // is typed.
                let mut guard = 0;
                while l.acts.len() <= k && guard < 400 {
                    guard += 1;
                    let Some(u) = l.due else { break };
                    l.advance(u);
                    l.wake();
                }
                let Some(a) = l.acts.get(k) else {
                    l.log
                        .push(format!("  !! no act {k} for beat {:?}", b.label));
                    break;
                };
                a.at + dt
            }
        };
        l.waits_before(t, false);
        if t < l.now() {
            l.log.push(format!(
                "  !! beat {:?} at +{} comes after +{}",
                b.label,
                t - sc.t0,
                l.now() - sc.t0
            ));
        }
        let t = t.max(l.now());
        l.advance(t);
        l.log.push(format!("  {} == {}", t - sc.t0, b.label));
        let ev = match &b.ev {
            Ev::Point(rows) => Ev::Point(resolve(rows, t, &mut stamps)),
            Ev::Busy(rows) => Ev::Busy(resolve(rows, t, &mut stamps)),
            other => other.clone(),
        };
        l.event(&ev);
        let answers = match b.at {
            At::Start(_) => None,
            At::Act(k, _) => Some(k),
        };
        events.push((t, b.label.clone(), ev, answers));
    }
    let end = events.iter().map(|e| e.0).max().unwrap_or(sc.t0) + sc.tail_s;
    l.waits_before(end, false);
    let log = std::mem::take(&mut l.log);
    let out = First {
        events,
        acts: l.acts.clone(),
        stamped: l.stamped.clone(),
        journal: l.journal.clone(),
        log,
        end,
    };
    drop(l);
    out
}

/// A cell's follower: its first act that is not the first loop's next, the
/// first loop's act it was due to match, the wait it left standing (unix
/// seconds), and its log.
type Cell = (Option<Act>, usize, Option<i64>, Vec<String>);

/// What a loop that follows did ([`run_follower`]).
struct Followed {
    /// Its first act that is not the first loop's next.
    diverged: Option<Act>,
    /// The first loop's act it was due to match next.
    at_k: usize,
    /// The last act it (or a loop before it) typed.
    last: Option<i64>,
    /// The wait it left standing (unix seconds).
    due: Option<i64>,
    log: Vec<String>,
    /// The books it attached on and every row it wrote, stamped: the ledger
    /// a loop that follows IT attaches on ([`run_chain`]).
    stamped: Vec<(i64, String)>,
    /// The journal it attached beside and every line it said, stamped: the
    /// journal beside that ledger.
    journal: Vec<(i64, String)>,
}

/// The books a loop that follows attaches on: the ledger's rows and the
/// journal's lines beside it, stamped — the ones before its attach standing.
#[derive(Clone, Copy)]
struct Books<'b> {
    rows: &'b [(i64, String)],
    journal: &'b [(i64, String)],
}

impl First {
    /// The first loop's books ([`Books`]).
    fn books(&self) -> Books<'_> {
        Books {
            rows: &self.stamped,
            journal: &self.journal,
        }
    }
}

impl Followed {
    /// The books a loop that follows this one attaches on ([`Books`]).
    fn books(&self) -> Books<'_> {
        Books {
            rows: &self.stamped,
            journal: &self.journal,
        }
    }
}

/// A successor attached at `h` on the first loop's books as they stood
/// then, fed the rest of the timeline: its first act, and its last wait.
fn run_successor(sc: &Scenario, first: &First, h: i64, dir: &Path, seed: bool) -> Cell {
    let before: Vec<&Act> = first.acts.iter().filter(|a| a.at < h).collect();
    let last = before.last().map(|a| a.at);
    let f = run_follower(
        sc,
        first,
        first.books(),
        (h, before.len(), last),
        None,
        dir,
        seed,
    );
    (f.diverged, f.at_k, f.due, f.log)
}

/// A loop attached at `h` on `on` (rows and journal lines stamped, the ones
/// before `h` standing), the first loop's acts before `k0` typed by the loops before
/// it (the last of them at `last`), fed the rest of the timeline — up to
/// `until` (exclusive), where a loop that follows it takes over, or to the
/// run's end.
fn run_follower(
    sc: &Scenario,
    first: &First,
    on: Books<'_>,
    (h, k0, last): (i64, usize, Option<i64>),
    until: Option<i64>,
    dir: &Path,
    seed: bool,
) -> Followed {
    let books = dir.join(format!("books-{h}-{}.jsonl", until.unwrap_or(0)));
    // Whether the host started this loop as the session's Off lifted: the
    // loop before it was stopped under Off, so the park's row is this
    // loop's to write ([`Session::seed_from_ledger`]), not on its books.
    let lifted_at = first
        .events
        .iter()
        .find(|(t, _, ev, _)| matches!(ev, Ev::OffLifted) && *t < h && h - *t <= 1)
        .map(|e| e.0);
    let lifted_off = lifted_at.is_some();
    let standing_rows: Vec<(i64, String)> = on
        .rows
        .iter()
        .filter(|(ms, _)| *ms < h * 1000)
        .filter(|(ms, row)| {
            !(lifted_at.is_some_and(|t| *ms >= t * 1000)
                && row.contains(&format!(
                    "\"rule_id\":\"{}\"",
                    crate::supervise::approvals::RULE_OFF_STOOD
                )))
        })
        .cloned()
        .collect();
    let body: String = standing_rows
        .iter()
        .map(|(_, l)| format!("{l}\n"))
        .collect();
    std::fs::write(&books, body).expect("the books");
    // The journal beside them, as the window's host keeps it.
    let journal = crate::supervise::approvals::journal_beside(&books);
    let standing_lines = before_ms(on.journal, h * 1000);
    let said: String = standing_lines
        .iter()
        .map(|(_, l)| format!("{l}\n"))
        .collect();
    std::fs::write(&journal, said).expect("the journal");
    // The transcript as it stands at `h`.
    let mut tail = sc.host.as_ref().and_then(|(_, t)| t.clone());
    for (t, _, ev, _) in &first.events {
        if *t < h
            && let Ev::Tail(x) = ev
        {
            tail.clone_from(x);
        }
    }
    // The window's person as its status says it at `h`.
    let key_at = first
        .events
        .iter()
        .filter(|(t, _, ev, _)| *t < h && matches!(ev, Ev::Key))
        .map(|(t, ..)| *t)
        .max();
    let host = host_for(sc, &["relaunched"], tail);
    let opts = opts_for(sc, host.as_ref());
    let mut m = Mock::new(vec![Vec::new()]);
    let mut l = Loop {
        s: session(&mut m, Some(SID.to_string())),
        opts,
        host,
        ledger: books.clone(),
        read: 0,
        stamped: Vec::new(),
        lines: Vec::new(),
        journal: Vec::new(),
        acts: Vec::new(),
        seq: 500,
        due: None,
        busy_since: None,
        seen: None,
        rows: Vec::new(),
        log: Vec::new(),
        take: sc.take,
        unseen: None,
        awaiting: false,
        key_at,
        person_on_screen: sc.person_on_screen,
    };
    l.s.set_zone_lookup(test_zones);
    l.s.set_turn_end_timing(TurnEndTiming {
        take_within: take_within(sc.take),
        ..TurnEndTiming::default()
    });
    l.s.set_clock(h, PDT);
    // `drive_loop`'s seed. Its take window on the test's clock: an act
    // younger than [`TAKE_S`] is awaited (production's 30 s), whose window
    // then runs on the worker's clock as the first loop's does.
    // (Not seeded: the negative control, a loop with no books.)
    l.s.turn_end.timing.take_within = Duration::from_secs(TAKE_S.unsigned_abs());
    // The ledger is open as the seed is read, as `drive_loop` opens it: the
    // rows so far are the predecessor's, and any the seed writes this loop's.
    l.s.ledger = Ledger::open(Some(&books), Some(SID), &mut Vec::new());
    l.read = standing_rows.len();
    let open = seed
        .then(|| l.s.seed_from_ledger(&books, h * 1000, lifted_off))
        .flatten();
    let _ = l.collect();
    l.s.turn_end.timing.take_within = take_within(sc.take);
    l.s.seed_wall_memory(open);
    l.awaiting = sc.take && l.s.turn_end.untaken_act().is_some();
    // The window the seed counts from the first loop's read, on the test's
    // clock ([`TurnEndState::seed_awaited`]).
    if l.awaiting {
        l.unseen = l.s.turn_end.untaken_act_seen().map(|t| {
            let ago = Instant::now().saturating_duration_since(t).as_millis();
            h - i64::try_from((ago + 500) / 1000).unwrap_or(0)
        });
    }
    if lifted_off {
        l.log
            .push(format!("  {} == attached as Off lifted", h - sc.t0));
    }
    // Its first look: the screen standing at `h`.
    let standing = first
        .events
        .iter()
        .rev()
        .find(|(t, _, ev, _)| *t < h && matches!(ev, Ev::Point(_) | Ev::Busy(_)));
    // On the fixed clock (no take window modelled) a point is judged at once:
    // a first look, inside an act's own window, at the screen the act was
    // typed at would judge what no loop judges there (production's window is
    // 30 s, and the act is awaited: [`TurnEndState::seed_awaited`]). Those
    // scenarios' successors first look at the next event. (The act the
    // loops before this one typed: a chain's successor may have typed none
    // where the first loop did.)
    //
    // Where the take window is modelled, a successor attached after an act
    // and before the first loop's first read after it (a point or a busy
    // read, inside twice the window) first looks there too: a loop reads at
    // its poll, and a handoff takes longer, so the read latency is the
    // world's. One that read sooner would open the act's window sooner, as
    // the first loop itself would reading then, and judge a turn whose busy
    // read comes in the gap as no taking of the act.
    let first_read = |a: i64| {
        first
            .events
            .iter()
            .filter(|(t, _, ev, _)| *t > a && matches!(ev, Ev::Point(_) | Ev::Busy(_)))
            .map(|e| e.0)
            .min()
    };
    let standing = standing.filter(|(t, ..)| match last {
        Some(a) if *t <= a && sc.take => first_read(a).is_none_or(|r| r - a >= 2 * TAKE_S),
        Some(a) if *t <= a => h - a >= TAKE_S,
        _ => true,
    });
    // The successor's acts that match the first loop's from `k0` on (the
    // same text, no sooner and within 40 s later; any relaunch for a
    // relaunch) carry it on: the
    // world's answers to them are its events too. The first that does not
    // match is the cell's.
    let same = |want: Option<&Act>, got: &Act| {
        want.is_some_and(|w| {
            (w.rule == "model-fallback@v1" && got.rule == w.rule)
                || (w.text == got.text && (0..=40).contains(&(got.at - w.at)))
        })
    };
    let mut matched = 0usize;
    let mut diverged: Option<Act> = None;
    let mut chain: Vec<Act> = Vec::new();
    let mut take = |acts: Vec<Act>, matched: &mut usize, chain: &mut Vec<Act>| -> bool {
        for a in acts {
            if same(first.acts.get(k0 + *matched), &a) {
                *matched += 1;
                chain.push(a);
            } else {
                diverged = Some(a);
                return false;
            }
        }
        true
    };
    let mut going = true;
    if let Some((_, label, ev, _)) = standing {
        l.log.push(format!("  {} == attach on {label}", h - sc.t0));
        let acts = match ev {
            Ev::Point(rows) => l.point(rows.clone()),
            Ev::Busy(rows) => {
                l.busy(rows.clone());
                Vec::new()
            }
            _ => Vec::new(),
        };
        going = take(acts, &mut matched, &mut chain);
    }
    let end = until.unwrap_or(first.end);
    let mut evs: Vec<&(i64, String, Ev, Option<usize>)> = first
        .events
        .iter()
        .filter(|(t, ..)| *t >= h && *t < end)
        .collect();
    evs.sort_by_key(|e| e.0);
    // The world answers an act once it is typed: an answer to one this loop
    // typed later than the first loop (inside the 40 s it matches by) comes
    // as much later, one to an act it typed sooner on the first loop's
    // timeline (the screen an act was typed at stands until the first loop's
    // own answer comes), and one to an act it has not typed, not yet.
    let at_of = |e: &(i64, String, Ev, Option<usize>), chain: &[Act]| match e.3 {
        Some(k) if k >= k0 => chain
            .get(k - k0)
            .map(|mine| e.0 + (mine.at - first.acts[k].at).max(0)),
        _ => Some(e.0),
    };
    while going {
        let next = evs
            .iter()
            .enumerate()
            .filter_map(|(i, e)| at_of(e, &chain).map(|t| (i, t)))
            .min_by_key(|&(i, t)| (t, i))
            .filter(|&(_, t)| t < end);
        let typed = matched;
        while going && let Some(a) = l.waits_before(next.map_or(end, |(_, t)| t), true) {
            going = take(vec![a], &mut matched, &mut chain);
        }
        // An act typed meanwhile: its answers may come first.
        if !going || matched != typed {
            continue;
        }
        let Some((i, t)) = next else {
            break;
        };
        let (_, label, ev, _) = evs.remove(i);
        l.advance(t);
        l.log.push(format!("  {} == {label}", t - sc.t0));
        let acts = l.event(ev);
        going = take(acts, &mut matched, &mut chain);
    }
    while going && let Some(a) = l.waits_before(end, true) {
        going = take(vec![a], &mut matched, &mut chain);
    }
    let due = l.due;
    let mut log = std::mem::take(&mut l.log);
    log.push(format!(
        "  matched the first loop's acts {}..{}: {:?}",
        k0,
        k0 + matched,
        chain
            .iter()
            .map(|a| format!("+{} {}", a.at - sc.t0, a.rule))
            .collect::<Vec<_>>()
    ));
    let mut stamped = standing_rows;
    stamped.extend(l.stamped.iter().cloned());
    let mut lines = standing_lines;
    lines.extend(l.journal.iter().cloned());
    drop(l);
    let _ = std::fs::remove_file(&books);
    let _ = std::fs::remove_file(&journal);
    Followed {
        diverged,
        at_k: k0 + matched,
        // The world answers the first loop's acts on its timeline: the
        // screen an act was typed at stands until the first loop's own
        // answer comes (a relaunch matched however much sooner).
        last: chain.last().map(|a| a.at).or(last).max(
            (k0 + matched)
                .checked_sub(1)
                .and_then(|k| first.acts.get(k))
                .map(|a| a.at),
        ),
        due,
        log,
        stamped,
        journal: lines,
    }
}

/// A CHAIN OF THREE LOOPS: a successor attached at `h1` on the first loop's
/// books, run up to `h2`, where a third loop attaches on ITS books (the
/// first loop's rows and the successor's own) and runs to the end. `None`
/// where the successor itself parted from the first loop before `h2` (that
/// cell is the pair's).
fn run_chain(sc: &Scenario, first: &First, h1: i64, h2: i64, dir: &Path) -> Option<Cell> {
    let before: Vec<&Act> = first.acts.iter().filter(|a| a.at < h1).collect();
    let last = before.last().map(|a| a.at);
    let second = run_follower(
        sc,
        first,
        first.books(),
        (h1, before.len(), last),
        Some(h2),
        dir,
        true,
    );
    if second.diverged.is_some() {
        return None;
    }
    let third = run_follower(
        sc,
        first,
        second.books(),
        (h2, second.at_k, second.last),
        None,
        dir,
        true,
    );
    let mut log = vec![format!("  the successor at +{}:", h1 - sc.t0)];
    log.extend(second.log);
    log.push(format!("  the third loop at +{}:", h2 - sc.t0));
    log.extend(third.log);
    Some((third.diverged, third.at_k, third.due, log))
}

/// The handoff instants of a run: just after each event and act, just
/// before each act, and the middle of every long gap.
fn handoffs(first: &First) -> Vec<i64> {
    let mut moments: Vec<i64> = first.events.iter().map(|e| e.0).collect();
    moments.sort_unstable();
    moments.extend(first.acts.iter().map(|a| a.at));
    moments.sort_unstable();
    moments.dedup();
    let mut hs = Vec::new();
    for (i, &m) in moments.iter().enumerate() {
        hs.push(m + 1);
        if first.acts.iter().any(|a| a.at == m) {
            hs.push(m - 1);
        }
        if let Some(&next) = moments.get(i + 1)
            && next - m > 240
        {
            hs.push(m + (next - m) / 2);
            hs.push(next - 2);
            if next - m > 1200 {
                hs.push(m + (next - m) / 4);
                hs.push(m + 3 * (next - m) / 4);
            }
        }
    }
    hs.sort_unstable();
    hs.dedup();
    let start = first.events.iter().map(|e| e.0).min().unwrap_or(0);
    hs.retain(|h| *h > start);
    hs
}

/// The cells of a scenario's matrix that break the rule, and how many it ran.
#[derive(Default)]
struct Tally {
    cells: usize,
    broken: Vec<String>,
}

/// How long past the first loop's act a successor's may come: a fresh
/// appearance's first rung from attaching (30 min), a person's draft standing
/// in the composer, whose grace each loop counts from its own first sight
/// of it (`human_grace_s`, 2 min: a loop held by it past the try's next
/// rung leaves the try aged out for the loop after it, which works that wall
/// as a fresh appearance), and the take window's slack.
const LATER_BOUND_S: i64 = 30 * MIN + 2 * MIN + 40;

/// The rule a follower's first differing act breaks, if any (module docs):
/// `mine` the first loop's act it was due to match, `theirs` its own, `due`
/// the wait it left standing.
fn verdict(mine: Option<&Act>, theirs: Option<&Act>, due: Option<i64>) -> Option<&'static str> {
    match (mine, theirs) {
        (Some(a), Some(b)) if b.at < a.at => Some("SOONER"),
        (Some(a), Some(b)) if a.text != b.text => Some("OTHER-TEXT"),
        (None, Some(_)) => Some("TYPES-WHERE-FIRST-DOES-NOT"),
        (Some(_), None) if due.is_none() => Some("UNBOUNDED"),
        (Some(a), Some(b)) if b.at > a.at + LATER_BOUND_S => Some("LATER-UNBOUNDED"),
        (Some(a), None) if due.is_some_and(|d| d > a.at + LATER_BOUND_S) => Some("LATER-UNBOUNDED"),
        _ => None,
    }
}

/// One scenario's matrix: the first loop's run, and a successor at every
/// handoff (`seed`: taken up from the ledger, else started blank, the
/// negative control) — and, seeded, a CHAIN at every handoff: the
/// successor followed in its turn by a third loop at the next handoff
/// ([`run_chain`]). The log of every cell, and the cells that break the
/// rule (module docs) with their successor's log.
fn run_matrix(sc: &Scenario, seed: bool) -> (String, Tally) {
    let mut out = String::new();
    let mut tally = Tally::default();
    let dir = crate::supervise::test_scratch_path("seed-matrix", &sc.name.replace(' ', "-"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let first = run_first(sc, &dir);
    let _ = writeln!(out, "\n=== {} (t0={})", sc.name, sc.t0);
    for line in &first.log {
        let _ = writeln!(out, "{line}");
    }
    let _ = writeln!(
        out,
        "  first loop acts: {:?}",
        first
            .acts
            .iter()
            .map(|a| format!("+{} {} {:?}", a.at - sc.t0, a.rule, a.text))
            .collect::<Vec<_>>()
    );
    let rel = |a: Option<&Act>| {
        a.map_or("none".to_string(), |a| {
            format!("+{} {} {:?}", a.at - sc.t0, a.rule, a.text)
        })
    };
    let cell =
        |what: String, (theirs, at_k, due, log): Cell, out: &mut String, tally: &mut Tally| {
            tally.cells += 1;
            let mine = first.acts.get(at_k);
            let line = format!(
                "{} | {what} (act {at_k}): first {} ; successor {}{}",
                sc.name,
                rel(mine),
                rel(theirs.as_ref()),
                due.map_or(String::new(), |d| format!(
                    " (still waiting until +{})",
                    d - sc.t0
                ))
            );
            let v = verdict(mine, theirs.as_ref(), due);
            let _ = writeln!(out, "  [{}] {line}", v.unwrap_or("ok"));
            if let Some(v) = v {
                let mut broken = format!("{v}: {line}");
                for l in &log {
                    let _ = write!(broken, "\n      {l}");
                }
                tally.broken.push(broken);
            }
        };
    let hs = handoffs(&first);
    for (i, &h) in hs.iter().enumerate() {
        cell(
            format!("handoff +{}", h - sc.t0),
            run_successor(sc, &first, h, &dir, seed),
            &mut out,
            &mut tally,
        );
        if let Some(&h2) = hs.get(i + 1).filter(|_| seed)
            && let Some(c) = run_chain(sc, &first, h, h2, &dir)
        {
            cell(
                format!("chain +{} then +{}", h - sc.t0, h2 - sc.t0),
                c,
                &mut out,
                &mut tally,
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    (out, tally)
}

// ---------------------------------------------------------------- screens

/// `unix` on the test's clock as Claude Code's done row says it (`3:11 PM`).
fn done_clock(unix: i64) -> String {
    let local = unix + PDT;
    let h = (local / 3600).rem_euclid(24);
    let m = (local / 60).rem_euclid(60);
    let (h12, ap) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    format!("{h12}:{m:02} {ap}")
}

/// A reset's clock as a notice names it (`5:15pm`, `5pm`).
fn reset_clock(unix: i64) -> String {
    let local = unix + PDT;
    let h = (local / 3600).rem_euclid(24);
    let m = (local / 60).rem_euclid(60);
    let (h12, ap) = match h {
        0 => (12, "am"),
        1..=11 => (h, "am"),
        12 => (12, "pm"),
        _ => (h - 12, "pm"),
    };
    if m == 0 {
        format!("{h12}{ap}")
    } else {
        format!("{h12}:{m:02}{ap}")
    }
}

/// Claude Code's screen: its launch card, the conversation `history`, the
/// composer, `footer` rows and the mode line.
fn cc(history: &[String], footer: &[String], mode_extra: &str) -> Vec<String> {
    let mut r = vec![
        " ▐▛███▛█   Claude Code v2.1.300".to_string(),
        "▝▜██████▀  Sonnet 5.5 · Claude API".to_string(),
        " ▝▝   ▝▝   /private/tmp/at.5NoF/proj".to_string(),
        String::new(),
        String::new(),
    ];
    r.extend(history.iter().cloned());
    let bar = "─".repeat(120);
    r.push(bar.clone());
    r.push("❯".to_string());
    r.push(bar);
    r.extend(footer.iter().cloned());
    r.push(format!(
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents{mode_extra}"
    ));
    r
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_string()).collect()
}

/// A turn: `prompt` (none: the worker's own continuation), its reply rows,
/// its done row (`worked` minutes, done at `at`).
fn turn(prompt: Option<&str>, reply: &[&str], worked_s: i64, at: i64) -> Vec<String> {
    let mut r = Vec::new();
    if let Some(p) = prompt {
        r.push(format!("❯ {p}"));
        r.push(String::new());
    }
    for line in reply {
        r.push((*line).to_string());
    }
    r.push(String::new());
    let (m, sec) = (worked_s / 60, worked_s % 60);
    let took = if m > 0 {
        format!("{m}m {sec}s")
    } else {
        format!("{sec}s")
    };
    // A turn whose end is not known yet (`at` 0) is stamped when its screen
    // is first drawn ([`resolve`]).
    let done = if at == 0 {
        format!(
            "@T{}@",
            TURN_IDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        )
    } else {
        done_clock(at)
    };
    r.push(format!("✻ Worked for {took} · done {done}"));
    r.push(String::new());
    r
}

static TURN_IDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

/// The screen's turns ended at `t` where first drawn: each `@T<n>@` stamp is
/// the done clock of the first screen that drew it.
fn resolve(
    rows: &[String],
    t: i64,
    seen: &mut std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    rows.iter()
        .map(|r| {
            let mut out = r.clone();
            while let Some(a) = out.find("@T") {
                let Some(len) = out[a + 2..].find('@') else {
                    break;
                };
                let key = out[a..a + 2 + len + 1].to_string();
                let clock = seen
                    .entry(key.clone())
                    .or_insert_with(|| done_clock(t))
                    .clone();
                out = out.replacen(&key, &clock, 1);
            }
            out
        })
        .collect()
}

fn live(history: &[String], prompt: &str) -> Vec<String> {
    let mut h = history.to_vec();
    h.push(format!("❯ {prompt}"));
    h.push(String::new());
    h.push("⏺ Working.".to_string());
    h.push(String::new());
    h.push("✻ Synthesizing… (18s)".to_string());
    h.push(String::new());
    let mut r = cc(&h, &[], "");
    let n = r.len();
    r[n - 1] = "  esc to interrupt".to_string();
    r
}

// ---------------------------------------------------------------- scenarios

const MIN: i64 = 60;
const HOUR: i64 = 3600;
/// 2026-09-17 15:00 PDT.
const T0: i64 = TEST_NOW + 6 * HOUR + 5 * MIN;

fn default_policy() -> SupervisorConfig {
    SupervisorConfig {
        model_fallback: None,
        ..SupervisorConfig::default()
    }
}

/// The five-hour window's wall as a turn's end draws it, the vendor's
/// countdown armed (`armed`) or not.
fn session_wall(
    history: &[String],
    reset: i64,
    at: i64,
    worked_s: i64,
    armed: bool,
) -> (Vec<String>, Vec<String>) {
    let clock = reset_clock(reset);
    let mut h = history.to_vec();
    let mut reply = vec![
        "⏺ Migrating the second table.".to_string(),
        format!("  ⎿  You've hit your session limit · resets {clock} (America/Los_Angeles)"),
    ];
    if armed {
        reply.push(String::new());
        reply.push(format!(
            "⏺ Usage limit reached · continuing automatically at {clock} · esc to cancel"
        ));
    }
    let refs: Vec<&str> = reply.iter().map(String::as_str).collect();
    h.extend(turn(None, &refs, worked_s, at));
    let footer = if armed {
        vec![
            format!("  ⚠ Usage limit reached · limit resets {clock}"),
            format!("    Continuing automatically at {clock} · esc to cancel"),
        ]
    } else {
        Vec::new()
    };
    (h, footer)
}

/// The session's work turns as the screen keeps them.
fn work0() -> Vec<String> {
    turn(
        Some("carry on with the task"),
        &["⏺ Migrated the first table."],
        9 * MIN,
        T0,
    )
}

fn with(history: &[String], more: &[String]) -> Vec<String> {
    let mut h = history.to_vec();
    h.extend(more.iter().cloned());
    h
}

/// The five-hour wall's history after Esc on its countdown.
fn cancelled(h: &[String]) -> Vec<String> {
    with(
        h,
        &s(&[
            "⏺ Automatic continue cancelled · /rate-limit-options to re-arm",
            "",
        ]),
    )
}

const CANCELLED_NOTE: &str =
    "                             Automatic continue cancelled · /rate-limit-options to re-arm";

/// The spend limit, as a turn's end draws it.
fn spend_turn(prompt: Option<&str>, worked_s: i64, at: i64) -> Vec<String> {
    turn(
        prompt,
        &[
            "⏺ Running the suite.",
            "  ⎿  You've hit your monthly spend limit.",
        ],
        worked_s,
        at,
    )
}

/// After the try: a busy read of its turn two seconds on.
fn taken(k: usize, h: &[String], prompt: &str) -> Beat {
    beat(
        At::Act(k, 2),
        "the act's turn runs",
        Ev::Busy(live(h, prompt)),
    )
}

// The lab's Sonnet bucket (2.1.300): its reset, Oct 17 4:20pm PDT.
const BUCKET_RESET: i64 = 1_792_279_200;

fn bucket_armed() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300,
    )
}

/// The bucket's screen with its countdown rows gone and `add` above the
/// composer.
fn bucket_with(add: &[&str], model: &str) -> Vec<String> {
    let mut rows: Vec<String> = bucket_armed()
        .into_iter()
        .filter(|r| {
            !["Usage limit reached", "Continuing automatically"]
                .iter()
                .any(|g| r.contains(g))
        })
        .map(|r| {
            r.replace(
                "Sonnet 5.5 · Claude API",
                &format!("{model} 5.5 · Claude API"),
            )
        })
        .collect();
    let top = rows
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("the composer's frame");
    rows.splice(top..top, add.iter().map(|x| (*x).to_string()));
    rows
}

/// The relaunched process's redraw (as the existing tests construct it).
fn bucket_redrawn() -> Vec<String> {
    bucket_armed()
        .iter()
        .filter(|r| {
            ![
                "Usage limit reached",
                "Continuing automatically",
                "/rate-limit-options",
                "Claude Code will continue automatically",
                "to cancel the wait",
            ]
            .iter()
            .any(|g| r.contains(g))
        })
        .map(|r| r.replace("Sonnet 5.5 · Claude API", "Opus 5.5 · Claude API"))
        .collect()
}

fn above_composer(base: &[String], add: &[&str]) -> Vec<String> {
    let mut rows = base.to_vec();
    let top = rows
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("the composer's frame");
    rows.splice(top..top, add.iter().map(|x| (*x).to_string()));
    rows
}

const BUCKET_NOTICE: &str =
    "You've hit your Sonnet limit · resets Oct 17 at 4:20pm (America/Los_Angeles)";

/// The transcript ending on `prompt` answered by the bucket's notice,
/// written at `at` (the A2 test's).
fn bucket_tail(prompt: &str, at: i64) -> String {
    let stamp = crate::harness::usage::rfc3339_utc(at);
    include_str!("fixtures/claude-2.1.294-transcript-session-limit.jsonl")
        .lines()
        .skip(1)
        .filter(|l| !l.contains("\"subtype\":\"informational\""))
        .map(|l| {
            let mut row = l
                .replace("seventh try", prompt)
                .replace(
                    "You've hit your session limit · resets 11:33am (America/Los_Angeles)",
                    BUCKET_NOTICE,
                )
                .replace("\"five_hour\"", "\"seven_day_sonnet\"");
            for ms in ["921", "951", "952", "953"] {
                row = row.replace(
                    &format!("\"2026-10-08T16:34:58.{ms}Z\""),
                    &format!("\"{stamp}\""),
                );
            }
            row
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();
    let reset = T0 + 12 * MIN + 2 * HOUR + 15 * MIN; // 17:27
    let w0 = work0();
    let wall_at = T0 + 12 * MIN;
    let (h_wall, f_armed) = session_wall(&w0, reset, wall_at, 12 * MIN, true);
    let walled = cc(&h_wall, &f_armed, "");
    let start = |label: &str| {
        beat(
            At::Start(0),
            label,
            Ev::Busy(live(&w0, "carry on with the task")),
        )
    };
    // ---- A: five-hour, armed; the vendor goes on by itself.
    for (tag, reply, busy_s) in [
        ("progress", "⏺ Stage 3 of 5 is in; stage 4 is next.", 0),
        (
            "done",
            "⏺ All done: the change is committed and pushed, and every suite passes.",
            0,
        ),
        ("worked", "⏺ Stage 3 of 5 is in; stage 4 is next.", 9 * MIN),
    ] {
        let fired_at = reset + 65;
        let mut h2 = h_wall.clone();
        h2.push("⏺ Usage limit reset · continuing automatically".to_string());
        h2.push(String::new());
        let end_at = fired_at + busy_s;
        h2.extend(turn(None, &[reply], busy_s, end_at));
        let fired = cc(&h2, &[], "");
        let mut beats = vec![
            start("the worker's turn"),
            beat(
                At::Start(12 * MIN),
                "the five-hour wall, armed",
                Ev::Point(walled.clone()),
            ),
        ];
        if busy_s > 0 {
            beats.push(beat(
                At::Start(fired_at - T0),
                "the vendor's continuation runs",
                Ev::Busy(live(&h_wall, "carry on with the task")),
            ));
        }
        beats.push(beat(
            At::Start(end_at - T0),
            "the vendor's continuation ends",
            Ev::Point(fired),
        ));
        beats.push(taken(0, &h2, "keep going"));
        out.push(Scenario {
            name: format!("A five-hour armed, vendor goes on ({tag})"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    // ---- B: five-hour, Esc on the countdown; the loop's try at the reset.
    let h_c = cancelled(&h_wall);
    let esc = cc(&h_c, &[], CANCELLED_NOTE);
    let notice_again = format!(
        "  ⎿  You've hit your session limit · resets {} (America/Los_Angeles)",
        reset_clock(reset)
    );
    for tag in [
        "served",
        "met again",
        "a person's turn met the wall",
        "a /clear",
        "done report, the done check",
    ] {
        let mut beats = vec![
            start("the worker's turn"),
            beat(
                At::Start(12 * MIN),
                "the five-hour wall, armed",
                Ev::Point(walled.clone()),
            ),
            beat(
                At::Start(22 * MIN),
                "Esc on the countdown",
                Ev::Point(esc.clone()),
            ),
        ];
        let mut h_after = h_c.clone();
        match tag {
            "a person's turn met the wall" => {
                beats.push(beat(
                    At::Start(40 * MIN),
                    "a person's prompt runs",
                    Ev::Busy(live(&h_c, "please look at the schema")),
                ));
                h_after = with(
                    &h_c,
                    &turn(
                        Some("please look at the schema"),
                        &[&notice_again],
                        20,
                        T0 + 40 * MIN + 20,
                    ),
                );
                beats.push(beat(
                    At::Start(40 * MIN + 20),
                    "the person's prompt met the wall",
                    Ev::Point(cc(&h_after, &[], "")),
                ));
            }
            "a /clear" => {
                beats.push(beat(
                    At::Start(30 * MIN),
                    "a person's /clear",
                    Ev::Point(cleared_at_an_armed_limit()),
                ));
            }
            _ => {}
        }
        if tag == "a /clear" {
            // The new task's work meets the window again; its countdown armed.
            let fresh = turn(
                Some("start on the parser"),
                &["⏺ Reading the parser."],
                5 * MIN,
                T0 + 55 * MIN,
            );
            let (h_new, f_new) = session_wall(&fresh, reset, T0 + 55 * MIN, 5 * MIN, true);
            beats.push(beat(
                At::Start(50 * MIN),
                "a person's new task runs",
                Ev::Busy(live(&[], "start on the parser")),
            ));
            beats.push(beat(
                At::Start(55 * MIN),
                "the new task meets the window, armed",
                Ev::Point(cc(&h_new, &f_new, "")),
            ));
            let mut h2 = h_new.clone();
            h2.push("⏺ Usage limit reset · continuing automatically".to_string());
            h2.push(String::new());
            h2.extend(turn(None, &["⏺ Reading the lexer next."], 0, reset + 65));
            beats.push(beat(
                At::Start(reset + 65 - T0),
                "the vendor goes on",
                Ev::Point(cc(&h2, &[], "")),
            ));
            beats.push(taken(0, &h2, "keep going"));
        } else {
            beats.push(taken(0, &h_after, "keep going"));
            match tag {
                "served" => {
                    let h3 = with(
                        &h_after,
                        &turn(
                            Some("keep going"),
                            &["⏺ Stage 3 of 5 is in; stage 4 is next."],
                            15 * MIN,
                            0,
                        ),
                    );
                    beats.push(beat(
                        At::Act(0, 15 * MIN),
                        "the try's work ends",
                        Ev::Point(cc(&h3, &[], "")),
                    ));
                    beats.push(taken(1, &h3, "keep going"));
                }
                "met again" => {
                    let h3 = with(&h_after, &turn(Some("keep going"), &[&notice_again], 0, 0));
                    // (no busy read: replace the take's busy)
                    beats.pop();
                    beats.push(beat(
                        At::Act(0, 3),
                        "the try met the wall again",
                        Ev::Point(cc(&h3, &[], "")),
                    ));
                    beats.push(taken(1, &h3, "keep going"));
                    let h4 = with(
                        &h3,
                        &turn(Some("keep going"), &["⏺ Stage 3 of 5 is in."], 12 * MIN, 0),
                    );
                    beats.push(beat(
                        At::Act(1, 12 * MIN),
                        "the second try's work ends",
                        Ev::Point(cc(&h4, &[], "")),
                    ));
                    beats.push(taken(2, &h4, "keep going"));
                }
                "done report, the done check" => {
                    let h3 = with(
                        &h_after,
                        &turn(
                            Some("keep going"),
                            &[
                                "⏺ All done: the change is committed and pushed, and every suite passes.",
                            ],
                            10 * MIN,
                            0,
                        ),
                    );
                    beats.push(beat(
                        At::Act(0, 10 * MIN),
                        "the try's work ends in a done report",
                        Ev::Point(cc(&h3, &[], "")),
                    ));
                    beats.push(taken(1, &h3, "Before you stop:"));
                    let h4 = with(
                        &h3,
                        &turn(
                            Some(crate::supervise::policy::turn_end::DONE_CHECK),
                            &["⏺ All done: verified again, every suite passes."],
                            70,
                            0,
                        ),
                    );
                    beats.push(beat(
                        At::Act(1, 72),
                        "the done check's answer: done again",
                        Ev::Point(cc(&h4, &[], "")),
                    ));
                }
                _ => {}
            }
        }
        out.push(Scenario {
            name: format!("B five-hour Esc'd, the loop's try ({tag})"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    // ---- C: the weekly window, no countdown.
    let weekly_notice =
        "  ⎿  You've hit your weekly limit · resets Sep 19 at 11am (America/Los_Angeles)";
    for tag in ["served", "met again"] {
        let h_w = with(
            &w0,
            &turn(
                None,
                &["⏺ Migrating the second table.", weekly_notice],
                12 * MIN,
                wall_at,
            ),
        );
        let mut beats = vec![
            start("the worker's turn"),
            beat(
                At::Start(12 * MIN),
                "the weekly wall",
                Ev::Point(cc(&h_w, &[], "")),
            ),
        ];
        if tag == "met again" {
            let h3 = with(&h_w, &turn(Some("keep going"), &[weekly_notice], 0, 0));
            beats.push(beat(
                At::Act(0, 3),
                "the try met the wall again",
                Ev::Point(cc(&h3, &[], "")),
            ));
            beats.push(taken(1, &h3, "keep going"));
        } else {
            beats.push(taken(0, &h_w, "keep going"));
            let h3 = with(
                &h_w,
                &turn(Some("keep going"), &["⏺ Stage 3 of 5 is in."], 20 * MIN, 0),
            );
            beats.push(beat(
                At::Act(0, 20 * MIN),
                "the try's work ends",
                Ev::Point(cc(&h3, &[], "")),
            ));
            beats.push(taken(1, &h3, "keep going"));
        }
        out.push(Scenario {
            name: format!("C weekly, no countdown ({tag})"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    // ---- D-H: the spend limit (the entitlement ladder).
    let sp0 = with(&w0, &spend_turn(None, 3 * MIN, T0 + 3 * MIN));
    let spend_wall = cc(&sp0, &[], "");
    let met = |h: &[String]| with(h, &spend_turn(Some("keep going"), 0, 0));
    for tag in [
        "met again, the ladder",
        "the try's own work meets it",
        "the try's own short turn meets it",
        "met again, a person's turn read busy ends on it",
        "met again, a /clear, a new task meets it",
        "the try's turn ends on a 529, its retry's work meets it",
        "met again, a person's turn not read busy ends on it",
    ] {
        let mut beats = vec![
            start("the worker's turn"),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
        ];
        let h1 = met(&sp0);
        match tag {
            "met again, the ladder" => {
                beats.push(beat(
                    At::Act(0, 30),
                    "the try met again",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                let h2 = met(&h1);
                beats.push(beat(
                    At::Act(1, 30),
                    "the second try met again",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(2, &h2, "keep going"));
            }
            "the try's own work meets it" => {
                beats.push(taken(0, &sp0, "keep going"));
                let h2 = with(&sp0, &spend_turn(Some("keep going"), 70 * MIN, 0));
                beats.push(beat(
                    At::Act(0, 70 * MIN),
                    "the try's work meets the limit",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "keep going"));
            }
            "the try's own short turn meets it" => {
                // Under `progress` (no new episode: the rung counts from its
                // end), and long enough for a handoff inside it past the try's
                // take window.
                beats.push(taken(0, &sp0, "keep going"));
                let h2 = with(&sp0, &spend_turn(Some("keep going"), 4 * MIN + 30, 0));
                beats.push(beat(
                    At::Act(0, 4 * MIN + 32),
                    "the try's short turn meets the limit",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "keep going"));
            }
            "met again, a person's turn read busy ends on it" => {
                beats.push(beat(
                    At::Act(0, 30),
                    "the try met again",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                beats.push(beat(
                    At::Act(0, 20 * MIN),
                    "a person's turn runs",
                    Ev::Busy(live(&h1, "please look at the schema")),
                ));
                let h2 = with(
                    &h1,
                    &spend_turn(Some("please look at the schema"), 20 * MIN, 0),
                );
                beats.push(beat(
                    At::Act(0, 40 * MIN),
                    "the person's turn ends on the limit",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "keep going"));
            }
            "met again, a person's turn not read busy ends on it" => {
                beats.push(beat(
                    At::Act(0, 30),
                    "the try met again",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                let h2 = with(&h1, &spend_turn(Some("please look at the schema"), 0, 0));
                beats.push(beat(
                    At::Act(0, 20 * MIN),
                    "a person's prompt met the limit",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "keep going"));
            }
            "met again, a /clear, a new task meets it" => {
                beats.push(beat(
                    At::Act(0, 30),
                    "the try met again",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                beats.push(beat(
                    At::Act(0, 5 * MIN),
                    "a person's /clear",
                    Ev::Point(cleared_at_an_armed_limit()),
                ));
                beats.push(beat(
                    At::Act(0, 100 * MIN),
                    "a new task runs",
                    Ev::Busy(live(&[], "start on the parser")),
                ));
                let h2 = spend_turn(Some("start on the parser"), 5 * MIN, 0);
                beats.push(beat(
                    At::Act(0, 105 * MIN),
                    "the new task meets the limit",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "keep going"));
            }
            "the try's turn ends on a 529, its retry's work meets it" => {
                beats.push(taken(0, &sp0, "keep going"));
                let h2 = with(
                    &sp0,
                    &turn(
                        Some("keep going"),
                        &[
                            "⏺ Running the suite.",
                            "  ⎿  API Error: 529 Overloaded. This is a server-side issue, usually temporary — try again in a moment. If it persists, check https://status.claude.com.",
                        ],
                        20 * MIN,
                        0,
                    ),
                );
                beats.push(beat(
                    At::Act(0, 20 * MIN),
                    "the try's turn ends on a 529",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "try again"));
                let h3 = with(&h2, &spend_turn(Some("try again"), 60 * MIN, 0));
                beats.push(beat(
                    At::Act(1, 60 * MIN),
                    "the retry's work meets the spend limit",
                    Ev::Point(cc(&h3, &[], "")),
                ));
                beats.push(taken(2, &h3, "keep going"));
            }
            _ => unreachable!(),
        }
        out.push(Scenario {
            name: format!("D spend ({tag})"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 8 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    // ---- I: the Sonnet bucket, no fallback.
    let bt0 = BUCKET_RESET - 2 * 86_400;
    let busy0 = live(&s(&["❯ start the task", ""]), "carry on with the task");
    for tag in [
        "the vendor goes on",
        "Esc'd, the loop's try",
        "Esc'd, the try met again",
    ] {
        let mut beats = vec![
            beat(
                At::Start(-5 * MIN),
                "the worker's turn",
                Ev::Busy(busy0.clone()),
            ),
            beat(At::Start(0), "the bucket, armed", Ev::Point(bucket_armed())),
        ];
        match tag {
            "the vendor goes on" => {
                let fired = bucket_with(
                    &[
                        "⏺ Usage limit reset · continuing automatically",
                        "",
                        "⏺ Stage 3 of 5 is in; stage 4 is next.",
                        "",
                        "✻ Worked for 0s · done 4:21 PM",
                        "",
                    ],
                    "Sonnet",
                );
                beats.push(beat(
                    At::Start(BUCKET_RESET + 65 - bt0),
                    "the vendor goes on",
                    Ev::Point(fired.clone()),
                ));
                beats.push(taken(0, &[], "keep going"));
            }
            _ => {
                let esc = bucket_with(
                    &[
                        "⏺ Automatic continue cancelled · /rate-limit-options to re-arm",
                        "",
                    ],
                    "Sonnet",
                );
                beats.push(beat(
                    At::Start(10 * MIN),
                    "Esc on the countdown",
                    Ev::Point(esc.clone()),
                ));
                if tag.ends_with("met again") {
                    let again = above_composer(
                        &esc,
                        &[
                            "❯ keep going",
                            "  ⎿  You've hit your Sonnet limit · resets Oct 24 at 4:20pm (America/Los_Angeles)",
                            "",
                            "✻ Churned for 0s · done 4:23 PM",
                            "",
                        ],
                    );
                    beats.push(beat(
                        At::Act(0, 3),
                        "the try met a new bucket",
                        Ev::Point(again),
                    ));
                    beats.push(taken(1, &[], "keep going"));
                } else {
                    beats.push(taken(0, &[], "keep going"));
                }
            }
        }
        out.push(Scenario {
            name: format!("I bucket, no fallback ({tag})"),
            t0: bt0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    // ---- J/K: the bucket with the fallback's relaunch.
    let redrawn = bucket_redrawn();
    let before = bucket_tail("carry on with the task", bt0 - 600);
    for tag in [
        "relaunched, the continuation works",
        "relaunched, the continuation met by the bucket",
        "relaunched, a person's prompt met by the bucket",
        "wait:resume thrice, a person's prompt met by the bucket before the adoption",
        "wait:resume thrice, the redraw",
    ] {
        let answers: Vec<&'static str> = if tag.starts_with("wait:resume") {
            vec!["wait:resume", "wait:resume", "wait:resume", "relaunched"]
        } else {
            vec!["relaunched"]
        };
        // The bucket met at once (no work read): the redraw is a short turn.
        let mut beats = vec![beat(
            At::Start(0),
            "the bucket, armed",
            Ev::Point(bucket_armed()),
        )];
        if !tag.contains("person's prompt") {
            beats.insert(
                0,
                beat(
                    At::Start(-5 * MIN),
                    "the worker's turn",
                    Ev::Busy(busy0.clone()),
                ),
            );
        }
        let met_prompt = |prompt: &str| {
            above_composer(
                &redrawn,
                &[
                    &format!("❯ {prompt}"),
                    &format!("  ⎿  {BUCKET_NOTICE}"),
                    "",
                    "✻ Churned for 0s · done 8:49 AM",
                    "",
                ],
            )
        };
        match tag {
            "relaunched, the continuation works" => {
                beats.push(beat(
                    At::Act(0, 30),
                    "the relaunch's redraw",
                    Ev::Point(redrawn.clone()),
                ));
                beats.push(taken(1, &[], "keep going"));
                let worked = above_composer(
                    &redrawn,
                    &[
                        "❯ keep going",
                        "",
                        "⏺ Stage 3 of 5 is in.",
                        "",
                        "✻ Worked for 15m 0s · done 8:59 AM",
                        "",
                    ],
                );
                beats.push(beat(
                    At::Act(1, 15 * MIN),
                    "the continuation's work ends",
                    Ev::Point(worked),
                ));
                beats.push(taken(2, &[], "keep going"));
            }
            "relaunched, the continuation met by the bucket" => {
                beats.push(beat(
                    At::Act(0, 30),
                    "the relaunch's redraw",
                    Ev::Point(redrawn.clone()),
                ));
                beats.push(beat(
                    At::Act(1, 3),
                    "the continuation met by the bucket",
                    Ev::Point(met_prompt("keep going")),
                ));
                beats.push(taken(2, &[], "keep going"));
            }
            "relaunched, a person's prompt met by the bucket" => {
                beats.push(beat(
                    At::Act(0, 30),
                    "the relaunch's redraw",
                    Ev::Point(redrawn.clone()),
                ));
                beats.push(beat(
                    At::Act(0, 85),
                    "the transcript: the person's prompt met",
                    Ev::Tail(Some(bucket_tail(
                        "please run the integration tests",
                        bt0 + 85,
                    ))),
                ));
                beats.push(beat(
                    At::Act(0, 90),
                    "a person's prompt met by the bucket",
                    Ev::Point(met_prompt("please run the integration tests")),
                ));
                beats.push(taken(1, &[], "keep going"));
            }
            "wait:resume thrice, a person's prompt met by the bucket before the adoption" => {
                beats.push(beat(
                    At::Start(55),
                    "the transcript: the person's prompt met",
                    Ev::Tail(Some(bucket_tail(
                        "please run the integration tests",
                        bt0 + 55,
                    ))),
                ));
                beats.push(beat(
                    At::Start(60),
                    "a person's prompt met by the bucket",
                    Ev::Point(met_prompt("please run the integration tests")),
                ));
                beats.push(beat(
                    At::Act(0, 20),
                    "the relaunch's redraw",
                    Ev::Point(met_prompt("please run the integration tests")),
                ));
                beats.push(taken(1, &[], "keep going"));
            }
            "wait:resume thrice, the redraw" => {
                beats.push(beat(
                    At::Act(0, 20),
                    "the relaunch's redraw",
                    Ev::Point(redrawn.clone()),
                ));
                beats.push(taken(1, &[], "keep going"));
            }
            _ => unreachable!(),
        }
        out.push(Scenario {
            name: format!("J bucket, the fallback ({tag})"),
            t0: bt0,
            policy: SupervisorConfig::default(),
            host: Some((answers, Some(before.clone()))),
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    out.extend(more_scenarios());
    out.extend(grown_scenarios());
    out.extend(round8_scenarios());
    out.extend(round9_scenarios());
    out
}

/// The take window on the worker's clock, the done check a wall cut, and
/// limits of mixed kinds.
fn more_scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();
    let w0 = work0();
    let start = || {
        beat(
            At::Start(0),
            "the worker's turn",
            Ev::Busy(live(&w0, "carry on with the task")),
        )
    };
    let sp0 = with(&w0, &spend_turn(None, 3 * MIN, T0 + 3 * MIN));
    let spend_wall = cc(&sp0, &[], "");
    let window = true;
    // ---- TW: the spend try, its take window thirty seconds.
    for tag in [
        "the notice read inside the window, the try taken, its turn ends on it",
        "the notice read inside the window, a local command, a person's 3h turn ends on it",
        "the notice read inside the window, a local command, a person's 1h turn ends on it",
        "met inside the window, judged, a person's turn read busy ends on it",
        "met inside the window, judged, the ladder",
    ] {
        let mut beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
        ];
        let local = with(
            &sp0,
            &s(&["❯ /model", "  ⎿  Kept model as Sonnet 5.5 (default)", ""]),
        );
        let person = |h: &[String], at: i64, work: i64| -> Vec<Beat> {
            let end = with(h, &spend_turn(Some("please look at the schema"), work, 0));
            vec![
                beat(
                    At::Act(0, at),
                    "a person's turn runs",
                    Ev::Busy(live(h, "please look at the schema")),
                ),
                beat(
                    At::Act(0, at + work),
                    "the person's turn ends on the limit",
                    Ev::Point(cc(&end, &[], "")),
                ),
                taken(1, &end, "keep going"),
            ]
        };
        match tag {
            "the notice read inside the window, the try taken, its turn ends on it" => {
                beats.push(beat(
                    At::Act(0, 5),
                    "the try's screen read again",
                    Ev::Point(spend_wall.clone()),
                ));
                beats.push(beat(
                    At::Act(0, 20),
                    "the try's turn runs",
                    Ev::Busy(live(&sp0, "keep going")),
                ));
                let end = with(&sp0, &spend_turn(Some("keep going"), 19 * MIN, 0));
                beats.push(beat(
                    At::Act(0, 20 * MIN),
                    "the try's turn ends on the limit",
                    Ev::Point(cc(&end, &[], "")),
                ));
                beats.push(taken(1, &end, "keep going"));
            }
            "met inside the window, judged, a person's turn read busy ends on it" => {
                let h1 = with(&sp0, &spend_turn(Some("keep going"), 0, 0));
                beats.push(beat(
                    At::Act(0, 3),
                    "the try met again",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                beats.extend(person(&h1, 20 * MIN, 20 * MIN));
            }
            "met inside the window, judged, the ladder" => {
                let h1 = with(&sp0, &spend_turn(Some("keep going"), 0, 0));
                beats.push(beat(
                    At::Act(0, 3),
                    "the try met again",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                let h2 = with(&h1, &spend_turn(Some("keep going"), 0, 0));
                beats.push(beat(
                    At::Act(1, 3),
                    "the second try met again",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(2, &h2, "keep going"));
            }
            _ => {
                let work = if tag.contains("3h") { 3 * HOUR } else { HOUR };
                beats.push(beat(
                    At::Act(0, 2),
                    "the try's screen read again",
                    Ev::Point(spend_wall.clone()),
                ));
                beats.push(beat(
                    At::Act(0, 4),
                    "a person's local command under it",
                    Ev::Point(cc(&local, &[], "")),
                ));
                beats.extend(person(&local, 10, work));
            }
        }
        out.push(Scenario {
            name: format!("TW spend, take window ({tag})"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 8 * HOUR,
            take: window,
            person_on_screen: false,
        });
    }
    // ---- TW-long: the try taken, its own turn longer than the try's rung.
    for work in [3 * HOUR, 90 * MIN] {
        let mut beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
            beat(
                At::Act(0, 5),
                "the try's screen read again",
                Ev::Point(spend_wall.clone()),
            ),
            beat(
                At::Act(0, 20),
                "the try's turn runs",
                Ev::Busy(live(&sp0, "keep going")),
            ),
        ];
        let end = with(&sp0, &spend_turn(Some("keep going"), work, 0));
        beats.push(beat(
            At::Act(0, 20 + work),
            "the try's turn ends on the limit",
            Ev::Point(cc(&end, &[], "")),
        ));
        beats.push(taken(1, &end, "keep going"));
        out.push(Scenario {
            name: format!(
                "TW spend, take window (the try taken, its own {}m turn ends on it)",
                work / 60
            ),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 8 * HOUR,
            take: window,
            person_on_screen: false,
        });
    }
    // ---- TW-draft: the try's Enter did not take (its line left in the composer).
    {
        let mut drafted = spend_wall.clone();
        let caret = drafted
            .iter()
            .rposition(|r| r == "❯")
            .expect("the composer");
        drafted[caret] = "❯ keep going".to_string();
        let beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
            beat(
                At::Act(0, 2),
                "the try's line left in the composer",
                Ev::Point(drafted.clone()),
            ),
            taken(1, &sp0, "keep going"),
        ];
        out.push(Scenario {
            name: "TW spend, take window (the try's Enter did not take)".to_string(),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 8 * HOUR,
            take: window,
            person_on_screen: false,
        });
    }
    // ---- O: a model bucket no reset ends (the entitlement ladder).
    let ob0 = with(
        &w0,
        &turn(
            None,
            &["⏺ Running the suite.", "  ⎿  You've hit your Opus limit"],
            3 * MIN,
            T0 + 3 * MIN,
        ),
    );
    let ob_wall = cc(&ob0, &[], "");
    let ob_met = |h: &[String], p: &str, work: i64| {
        with(
            h,
            &turn(Some(p), &["  ⎿  You've hit your Opus limit"], work, 0),
        )
    };
    for tag in [
        "met again, the ladder",
        "met again, a person's 3h turn read busy ends on it",
        "take window: the notice read inside, a local command, a person's 3h turn ends on it",
        "take window: the try taken, its own 3h turn ends on it",
    ] {
        let mut beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the Opus bucket, no reset",
                Ev::Point(ob_wall.clone()),
            ),
        ];
        let mut take = false;
        match tag {
            "met again, the ladder" => {
                let h1 = ob_met(&ob0, "keep going", 0);
                beats.push(beat(
                    At::Act(0, 30),
                    "the try met again",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                let h2 = ob_met(&h1, "keep going", 0);
                beats.push(beat(
                    At::Act(1, 30),
                    "the second try met again",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(2, &h2, "keep going"));
            }
            "met again, a person's 3h turn read busy ends on it" => {
                let h1 = ob_met(&ob0, "keep going", 0);
                beats.push(beat(
                    At::Act(0, 30),
                    "the try met again",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                beats.push(beat(
                    At::Act(0, 20 * MIN),
                    "a person's turn runs",
                    Ev::Busy(live(&h1, "please look at the schema")),
                ));
                let h2 = ob_met(&h1, "please look at the schema", 3 * HOUR);
                beats.push(beat(
                    At::Act(0, 20 * MIN + 3 * HOUR),
                    "the person's turn ends on the bucket",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "keep going"));
            }
            "take window: the notice read inside, a local command, a person's 3h turn ends on it" =>
            {
                take = window;
                let local = with(
                    &ob0,
                    &s(&["❯ /model", "  ⎿  Kept model as Opus 5.5 (default)", ""]),
                );
                beats.push(beat(
                    At::Act(0, 2),
                    "the try's screen read again",
                    Ev::Point(ob_wall.clone()),
                ));
                beats.push(beat(
                    At::Act(0, 4),
                    "a person's local command under it",
                    Ev::Point(cc(&local, &[], "")),
                ));
                beats.push(beat(
                    At::Act(0, 10),
                    "a person's turn runs",
                    Ev::Busy(live(&local, "please look at the schema")),
                ));
                let h2 = ob_met(&local, "please look at the schema", 3 * HOUR);
                beats.push(beat(
                    At::Act(0, 10 + 3 * HOUR),
                    "the person's turn ends on the bucket",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "keep going"));
            }
            "take window: the try taken, its own 3h turn ends on it" => {
                take = window;
                beats.push(beat(
                    At::Act(0, 5),
                    "the try's screen read again",
                    Ev::Point(ob_wall.clone()),
                ));
                beats.push(beat(
                    At::Act(0, 20),
                    "the try's turn runs",
                    Ev::Busy(live(&ob0, "keep going")),
                ));
                let h2 = ob_met(&ob0, "keep going", 3 * HOUR);
                beats.push(beat(
                    At::Act(0, 20 + 3 * HOUR),
                    "the try's turn ends on the bucket",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "keep going"));
            }
            _ => unreachable!(),
        }
        out.push(Scenario {
            name: format!("O Opus bucket, no reset ({tag})"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 8 * HOUR,
            take,
            person_on_screen: false,
        });
    }
    // ---- K: the done check a wall cut, waited out, its retry.
    let reset = T0 + 12 * MIN + 2 * HOUR + 15 * MIN;
    let wall_at = T0 + 12 * MIN;
    let (h_wall, f_armed) = session_wall(&w0, reset, wall_at, 12 * MIN, true);
    let h_c = cancelled(&h_wall);
    let weekly_notice =
        "  ⎿  You've hit your weekly limit · resets Sep 19 at 11am (America/Los_Angeles)";
    {
        let mut beats = vec![
            start(),
            beat(
                At::Start(12 * MIN),
                "the five-hour wall, armed",
                Ev::Point(cc(&h_wall, &f_armed, "")),
            ),
            beat(
                At::Start(22 * MIN),
                "Esc on the countdown",
                Ev::Point(cc(&h_c, &[], CANCELLED_NOTE)),
            ),
            taken(0, &h_c, "keep going"),
        ];
        let h3 = with(
            &h_c,
            &turn(
                Some("keep going"),
                &["⏺ All done: the change is committed and pushed, and every suite passes."],
                10 * MIN,
                0,
            ),
        );
        beats.push(beat(
            At::Act(0, 10 * MIN),
            "the try's work ends in a done report",
            Ev::Point(cc(&h3, &[], "")),
        ));
        let check = crate::supervise::policy::turn_end::DONE_CHECK;
        let h4 = with(&h3, &turn(Some(check), &[weekly_notice], 0, 0));
        beats.push(beat(
            At::Act(1, 3),
            "the done check met by the weekly limit",
            Ev::Point(cc(&h4, &[], "")),
        ));
        beats.push(taken(2, &h4, "keep going"));
        let h5 = with(
            &h4,
            &turn(
                Some("keep going"),
                &["⏺ All done: verified again, every suite passes."],
                70,
                0,
            ),
        );
        beats.push(beat(
            At::Act(2, 72),
            "the retry's answer: done again",
            Ev::Point(cc(&h5, &[], "")),
        ));
        out.push(Scenario {
            name: "K the done check cut by the weekly limit, its retry's done reply".to_string(),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    // ---- P: the open-items challenge, its answer done (the merge review's
    // MG7-A3): the first loop types the challenge at a FINISHED report that
    // names open work, and the answer — done in words, or FINISHED again
    // with an item still listed — ends the task. A follower reads that
    // answer as the challenge's: typed nothing more, never the done check
    // (read as a first done report) or the challenge again (the job not
    // known challenged).
    for (tag, reply) in [
        (
            "done in words",
            &["⏺ Ran the lexer's e2e suite: green. All done; nothing left to do."][..],
        ),
        (
            "FINISHED again, an item listed",
            &[
                "⏺ Ran the lexer's e2e suite: green, committed.",
                "  Left: the docs.",
                "  ATERM-TASK-DONE",
            ][..],
        ),
    ] {
        let challenge =
            crate::supervise::policy::turn_end::open_items_text(&["still open"], None, false);
        let h1 = with(
            &w0,
            &turn(
                None,
                &[
                    "⏺ Built and tested the parser; committed on main.",
                    "  Still open: the lexer's e2e suite (not run).",
                    "  ATERM-TASK-DONE",
                ],
                12 * MIN,
                T0 + 12 * MIN,
            ),
        );
        let h2 = with(&h1, &turn(Some(&challenge), reply, 5 * MIN, 0));
        out.push(Scenario {
            name: format!("P the open-items challenge answered ({tag})"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats: vec![
                start(),
                beat(
                    At::Start(12 * MIN),
                    "a finished report that names open work",
                    Ev::Point(cc(&h1, &[], "")),
                ),
                taken(0, &h1, "Your report ends with ATERM-TASK-DONE"),
                beat(
                    At::Act(0, 5 * MIN),
                    "the challenge's answer",
                    Ev::Point(cc(&h2, &[], "")),
                ),
            ],
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    // ---- P: the challenge answered done, then a NEW JOB's FINISHED that
    // names open work (the merge of the ledger's records onto MG7-A3): the
    // answer ended the job (`task-done@v1`), so the new job is challenged
    // afresh — by a follower too. A follower seeded as challenged from the
    // answered challenge read the new job's report as the challenged job's
    // and typed nothing.
    {
        let challenge =
            crate::supervise::policy::turn_end::open_items_text(&["still open"], None, false);
        let h1 = with(
            &w0,
            &turn(
                None,
                &[
                    "⏺ Built and tested the parser; committed on main.",
                    "  Still open: the lexer's e2e suite (not run).",
                    "  ATERM-TASK-DONE",
                ],
                12 * MIN,
                T0 + 12 * MIN,
            ),
        );
        let h2 = with(
            &h1,
            &turn(
                Some(&challenge),
                &["⏺ Ran the lexer's e2e suite: green. All done; nothing left to do."],
                5 * MIN,
                0,
            ),
        );
        let ask = "now port the docs to the new site";
        let h3 = with(
            &h2,
            &turn(
                Some(ask),
                &[
                    "⏺ Ported the docs; committed on main.",
                    "  Still open: the search index (not built).",
                    "  ATERM-TASK-DONE",
                ],
                12 * MIN,
                0,
            ),
        );
        let h4 = with(
            &h3,
            &turn(
                Some(&challenge),
                &["⏺ Built the search index: green. All done; nothing left to do."],
                5 * MIN,
                0,
            ),
        );
        out.push(Scenario {
            name: "P the open-items challenge answered, then a new job's open items".to_string(),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats: vec![
                start(),
                beat(
                    At::Start(12 * MIN),
                    "a finished report that names open work",
                    Ev::Point(cc(&h1, &[], "")),
                ),
                taken(0, &h1, "Your report ends with ATERM-TASK-DONE"),
                beat(
                    At::Act(0, 5 * MIN),
                    "the challenge's answer: done",
                    Ev::Point(cc(&h2, &[], "")),
                ),
                beat(
                    At::Act(0, 20 * MIN),
                    "a new job's turn runs",
                    Ev::Busy(live(&h2, ask)),
                ),
                beat(
                    At::Act(0, 32 * MIN),
                    "the new job's finished report names open work",
                    Ev::Point(cc(&h3, &[], "")),
                ),
                taken(1, &h3, "Your report ends with ATERM-TASK-DONE"),
                beat(
                    At::Act(1, 5 * MIN),
                    "the new job's challenge's answer: done",
                    Ev::Point(cc(&h4, &[], "")),
                ),
            ],
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    // ---- P: the challenge PAST ITS ANSWER, then the job's FINISHED that
    // names open work (the merge review's MG9-S3): the job was challenged,
    // so the first loop ends the task there, whatever it still lists. The
    // owed check says the challenge only until its answer is judged — (a)
    // the answer is progress, no done report, and `keep going` follows it;
    // (b) a weekly limit cuts the challenge and Off stands and lifts before
    // its retry, which drops the cut check — so a follower past that point
    // read the job as unchallenged and challenged it a second time. The
    // rows the loop writes mark the job challenged
    // ([`crate::supervise::approvals::Carried::challenged`]).
    {
        let challenge =
            crate::supervise::policy::turn_end::open_items_text(&["still open"], None, false);
        let h1 = with(
            &w0,
            &turn(
                None,
                &[
                    "⏺ Built and tested the parser; committed on main.",
                    "  Still open: the lexer's e2e suite (not run).",
                    "  ATERM-TASK-DONE",
                ],
                12 * MIN,
                T0 + 12 * MIN,
            ),
        );
        let finished = [
            "⏺ Ran the lexer's e2e suite: green, committed.",
            "  Left: the docs.",
            "  ATERM-TASK-DONE",
        ];
        let head = || {
            vec![
                start(),
                beat(
                    At::Start(12 * MIN),
                    "a finished report that names open work",
                    Ev::Point(cc(&h1, &[], "")),
                ),
                taken(0, &h1, "Your report ends with ATERM-TASK-DONE"),
            ]
        };
        // (a) The answer is progress; `keep going` follows it.
        let h2 = with(
            &h1,
            &turn(
                Some(&challenge),
                &["⏺ Running the lexer's e2e suite: 3 of 9 pass so far; fixing the rest."],
                5 * MIN,
                0,
            ),
        );
        let h3 = with(&h2, &turn(Some("keep going"), &finished, 10 * MIN, 0));
        let mut beats = head();
        beats.push(beat(
            At::Act(0, 5 * MIN),
            "the challenge's answer: progress",
            Ev::Point(cc(&h2, &[], "")),
        ));
        beats.push(taken(1, &h2, "keep going"));
        beats.push(beat(
            At::Act(1, 10 * MIN),
            "FINISHED again, an item listed",
            Ev::Point(cc(&h3, &[], "")),
        ));
        out.push(Scenario {
            name: "P the open-items challenge answered with progress, then FINISHED with an item"
                .to_string(),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
        // (b) The weekly limit cuts the challenge; Off stands and lifts
        // before the wall's retry.
        let h2 = with(&h1, &turn(Some(&challenge), &[weekly_notice], 0, 0));
        let h3 = with(&h2, &turn(Some("keep going"), &finished, 10 * MIN, 0));
        let mut beats = head();
        beats.push(beat(
            At::Act(0, 3),
            "the challenge met by the weekly limit",
            Ev::Point(cc(&h2, &[], "")),
        ));
        beats.push(beat(
            At::Act(0, 10 * MIN),
            "Off stood and lifted",
            Ev::OffLifted,
        ));
        beats.push(taken(1, &h2, "keep going"));
        beats.push(beat(
            At::Act(1, 10 * MIN),
            "the retry's answer: FINISHED, an item listed",
            Ev::Point(cc(&h3, &[], "")),
        ));
        out.push(Scenario {
            name: "P the open-items challenge cut by the weekly limit, Off lifts, then FINISHED \
                   with an item"
                .to_string(),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    // ---- M: limits of mixed kinds.
    for tag in [
        "the spend try met by the five-hour window",
        "the five-hour try met by the weekly window",
        "the vendor's continuation met by the weekly window",
        "the five-hour try met by the spend limit",
    ] {
        let mut beats = vec![start()];
        let five_notice = format!(
            "  ⎿  You've hit your session limit · resets {} (America/Los_Angeles)",
            reset_clock(T0 + 5 * HOUR)
        );
        match tag {
            "the spend try met by the five-hour window" => {
                beats.push(beat(
                    At::Start(3 * MIN),
                    "the spend limit",
                    Ev::Point(spend_wall.clone()),
                ));
                let h1 = with(&sp0, &turn(Some("keep going"), &[&five_notice], 0, 0));
                beats.push(beat(
                    At::Act(0, 3),
                    "the try met by the five-hour window",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                beats.push(taken(1, &h1, "keep going"));
            }
            "the five-hour try met by the weekly window"
            | "the five-hour try met by the spend limit" => {
                beats.push(beat(
                    At::Start(12 * MIN),
                    "the five-hour wall, armed",
                    Ev::Point(cc(&h_wall, &f_armed, "")),
                ));
                beats.push(beat(
                    At::Start(22 * MIN),
                    "Esc on the countdown",
                    Ev::Point(cc(&h_c, &[], CANCELLED_NOTE)),
                ));
                let met = if tag.ends_with("spend limit") {
                    "  ⎿  You've hit your monthly spend limit."
                } else {
                    weekly_notice
                };
                let h1 = with(&h_c, &turn(Some("keep going"), &[met], 0, 0));
                beats.push(beat(
                    At::Act(0, 3),
                    "the try met by another limit",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                beats.push(taken(1, &h1, "keep going"));
            }
            "the vendor's continuation met by the weekly window" => {
                beats.push(beat(
                    At::Start(12 * MIN),
                    "the five-hour wall, armed",
                    Ev::Point(cc(&h_wall, &f_armed, "")),
                ));
                let mut h2 = h_wall.clone();
                h2.push("⏺ Usage limit reset · continuing automatically".to_string());
                h2.push(String::new());
                h2.extend(turn(None, &[weekly_notice], 0, reset + 65));
                beats.push(beat(
                    At::Start(reset + 65 - T0),
                    "the vendor's continuation met by the weekly window",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(0, &h2, "keep going"));
            }
            _ => unreachable!(),
        }
        out.push(Scenario {
            name: format!("M mixed ({tag})"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    out
}

/// The five-hour window's notice again (the try met it: its reset placed
/// early).
fn five_again(reset: i64) -> String {
    format!(
        "  ⎿  You've hit your session limit · resets {} (America/Los_Angeles)",
        reset_clock(reset)
    )
}

/// A person's `/compact` running (its spinner, `esc to interrupt`).
fn compacting(history: &[String]) -> Vec<String> {
    let mut h = history.to_vec();
    h.extend(s(&[
        "❯ /compact",
        "",
        "✳ Compacting conversation… (0s)",
        "",
    ]));
    let mut r = cc(&h, &[], "");
    let n = r.len();
    r[n - 1] = "  ⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt".to_string();
    r
}

/// The conversation after a person's `/compact`: redrawn from the
/// compaction's banner, the kept turn, and the command's answer.
fn compacted(kept: &str) -> Vec<String> {
    cc(
        &s(&[
            "✻ Conversation compacted (ctrl+o for history)",
            "",
            "",
            kept,
            "",
            "✻ Worked for 9m 0s · done 3:09 PM",
            "",
            "❯ /compact",
            "  ⎿  Compacted (ctrl+o to see full summary)",
            "",
        ]),
        &[],
        "",
    )
}

/// The weekly window's wall at `at`, its reset at `reset` named with its
/// date (`resets Sep 17 at 5:27pm`), the vendor's countdown armed or not.
fn weekly_wall(history: &[String], reset: i64, at: i64, armed: bool) -> (Vec<String>, Vec<String>) {
    let clock = format!("Sep 17 at {}", reset_clock(reset));
    let mut h = history.to_vec();
    let mut reply = vec![
        "⏺ Migrating the second table.".to_string(),
        format!("  ⎿  You've hit your weekly limit · resets {clock} (America/Los_Angeles)"),
    ];
    if armed {
        reply.push(String::new());
        reply.push(format!(
            "⏺ Usage limit reached · continuing automatically at {clock} · esc to cancel"
        ));
    }
    let refs: Vec<&str> = reply.iter().map(String::as_str).collect();
    h.extend(turn(None, &refs, 12 * MIN, at));
    let footer = if armed {
        vec![
            format!("  ⚠ Usage limit reached · limit resets {clock}"),
            format!("    Continuing automatically at {clock} · esc to cancel"),
        ]
    } else {
        Vec::new()
    };
    (h, footer)
}

fn land_policy() -> SupervisorConfig {
    SupervisorConfig {
        autopilot: crate::supervise::config::Autopilot::Land,
        model_fallback: None,
        ..SupervisorConfig::default()
    }
}

/// THE SHAPES GROWN IN ROUND 7 (the seed's real inputs the matrix lacked):
/// a handoff in the read latency past a try's take window; acts in a
/// limit's place other than the done check; a cancelled notice under the
/// screen a try was typed at; a loop started as Off lifts; a done check
/// already answered, then a person's turn; the weekly window reached; a
/// usage window's try met by a model bucket; tries in a row; a person's
/// typing grace; a `/compact` between; the vendor's `continuing
/// automatically` words standing with no countdown.
#[allow(clippy::too_many_lines)]
fn grown_scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();
    let w0 = work0();
    let start = || {
        beat(
            At::Start(0),
            "the worker's turn",
            Ev::Busy(live(&w0, "carry on with the task")),
        )
    };
    let reset = T0 + 12 * MIN + 2 * HOUR + 15 * MIN;
    let wall_at = T0 + 12 * MIN;
    let (h_wall, f_armed) = session_wall(&w0, reset, wall_at, 12 * MIN, true);
    let walled = cc(&h_wall, &f_armed, "");
    let h_c = cancelled(&h_wall);
    let esc = cc(&h_c, &[], CANCELLED_NOTE);
    let again = five_again(reset);
    let sp0 = with(&w0, &spend_turn(None, 3 * MIN, T0 + 3 * MIN));
    let spend_wall = cc(&sp0, &[], "");
    let done = "⏺ All done: the change is committed and pushed, and every suite passes.";
    let force = "⏺ Shall I force-push the rewritten history over main?";
    let progress = "⏺ Stage 3 of 5 is in; stage 4 is next.";
    let check = "If the task is finished and verified, end your reply with ATERM-TASK-DONE on a line by itself and stop; otherwise continue.";
    let b_head = || {
        vec![
            start(),
            beat(
                At::Start(12 * MIN),
                "the five-hour wall, armed",
                Ev::Point(walled.clone()),
            ),
            beat(
                At::Start(22 * MIN),
                "Esc on the countdown",
                Ev::Point(esc.clone()),
            ),
        ]
    };
    let sc = |name: String, policy: SupervisorConfig, beats: Vec<Beat>, tail_s: i64, take: bool| {
        Scenario {
            name,
            t0: T0,
            policy,
            host: None,
            beats,
            tail_s,
            take,
            person_on_screen: false,
        }
    };
    // ---- R1: a handoff in the read latency past the try's take window.
    for (tag, first_read, busy_at) in [
        ("the first read after the try 35 s on", 35, 40),
        ("the first read 2 s on, the busy read 32 s on", 2, 32),
    ] {
        let mut beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
        ];
        let read = beat(
            At::Act(0, first_read),
            "the try's screen read",
            Ev::Point(spend_wall.clone()),
        );
        let mark = beat(At::Act(0, 30), "(a handoff instant)", Ev::Mark);
        if first_read < 30 {
            beats.extend([read, mark]);
        } else {
            beats.extend([mark, read]);
        }
        beats.push(beat(
            At::Act(0, busy_at),
            "the try's turn runs",
            Ev::Busy(live(&sp0, "keep going")),
        ));
        let end = with(&sp0, &spend_turn(Some("keep going"), 3 * HOUR, 0));
        beats.push(beat(
            At::Act(0, busy_at + 3 * HOUR),
            "the try's 3h turn ends on the limit",
            Ev::Point(cc(&end, &[], "")),
        ));
        beats.push(taken(1, &end, "keep going"));
        out.push(sc(
            format!("R1 spend, take window, the read latency ({tag})"),
            default_policy(),
            beats,
            8 * HOUR,
            true,
        ));
    }
    // ---- R2: acts in a limit's place other than the done check. The
    // vendor's continuation at the reset, the worker's reply under it, the
    // limit's rows scrolled off.
    for (tag, reply, policy) in [
        ("the reversible answer", force, default_policy()),
        ("the done check (control)", done, default_policy()),
        ("Land: the land instruction", done, land_policy()),
        ("Land: the land answer", force, land_policy()),
    ] {
        for take in [false, true] {
            let fired = cc(
                &turn(
                    None,
                    &["⏺ Usage limit reset · continuing automatically", "", reply],
                    0,
                    reset + 65,
                ),
                &[],
                "",
            );
            let mut beats = vec![
                start(),
                beat(
                    At::Start(12 * MIN),
                    "the five-hour wall, armed",
                    Ev::Point(walled.clone()),
                ),
                beat(
                    At::Start(reset + 65 - T0),
                    "the vendor's continuation, its limit scrolled off",
                    Ev::Point(fired.clone()),
                ),
            ];
            if take {
                beats.push(beat(
                    At::Act(0, 5),
                    "the act's screen read again",
                    Ev::Point(fired.clone()),
                ));
                beats.push(beat(At::Act(0, 20), "(a handoff instant)", Ev::Mark));
                beats.push(beat(
                    At::Act(0, 25),
                    "the act's turn runs",
                    Ev::Busy(live(&fired[5..fired.len() - 4], "(the act)")),
                ));
            } else {
                beats.push(taken(0, &fired[5..fired.len() - 4], "(the act)"));
            }
            let answered = with(
                &turn(
                    None,
                    &["⏺ Usage limit reset · continuing automatically", "", reply],
                    0,
                    reset + 65,
                ),
                &turn(Some("(the act)"), &[progress], 10 * MIN, 0),
            );
            beats.push(beat(
                At::Act(0, 10 * MIN + 25),
                "the act's answer: progress",
                Ev::Point(cc(&answered, &[], "")),
            ));
            beats.push(taken(1, &answered, "keep going"));
            out.push(sc(
                format!(
                    "R2 in the limit's place: {tag}{}",
                    if take { ", take window" } else { "" }
                ),
                policy.clone(),
                beats,
                3 * HOUR,
                take,
            ));
        }
    }
    // ---- R2b: a land nudge in the limit's place: the instruction typed at
    // an ordinary point, its turn meets the five-hour wall, the vendor goes
    // on and the reply is a done report.
    {
        let p0 = cc(
            &with(&w0, &turn(None, &[progress], 0, T0 + 2 * MIN)),
            &[],
            "",
        );
        let mut beats = vec![
            start(),
            beat(At::Start(2 * MIN), "the worker's progress", Ev::Point(p0)),
        ];
        let land_turn = with(
            &w0,
            &turn(Some("(land)"), &["⏺ Landing the change."], 10 * MIN, 0),
        );
        beats.push(taken(0, &w0, "(land)"));
        let (h_w2, f_w2) = session_wall(&land_turn, reset, 0, 10 * MIN, true);
        beats.push(beat(
            At::Act(0, 10 * MIN),
            "the land turn meets the five-hour wall, armed",
            Ev::Point(cc(&h_w2, &f_w2, "")),
        ));
        let fired = cc(
            &turn(
                None,
                &["⏺ Usage limit reset · continuing automatically", "", done],
                0,
                reset + 65,
            ),
            &[],
            "",
        );
        beats.push(beat(
            At::Start(reset + 65 - T0),
            "the vendor's continuation, its limit scrolled off: done",
            Ev::Point(fired.clone()),
        ));
        beats.push(taken(1, &fired[5..fired.len() - 4], "(nudge)"));
        out.push(sc(
            "R2 in the limit's place: Land: a land nudge".to_string(),
            land_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- R2c: Land, its landing spent (the instruction, then its nudges,
    // each answered with progress): nothing more is typed for it.
    {
        let p0 = cc(
            &with(&w0, &turn(None, &[progress], 0, T0 + 2 * MIN)),
            &[],
            "",
        );
        let mut beats = vec![
            start(),
            beat(At::Start(2 * MIN), "the worker's progress", Ev::Point(p0)),
        ];
        let mut h = w0.clone();
        for k in 0..3 {
            beats.push(taken(k, &h, "(land)"));
            h = with(
                &h,
                &turn(Some("(land)"), &["⏺ Stage 4 of 5 is in."], 10 * MIN, 0),
            );
            beats.push(beat(
                At::Act(k, 10 * MIN),
                "the land act's turn ends: progress",
                Ev::Point(cc(&h, &[], "")),
            ));
        }
        out.push(sc(
            "R2 Land, the landing spent".to_string(),
            land_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- R3: the cancelled notice under the screen the try was typed at
    // (the take window modelled).
    for tag in [
        "read again inside the window",
        "no read before the busy one",
    ] {
        let mut beats = b_head();
        if tag.starts_with("read again") {
            beats.push(beat(
                At::Act(0, 3),
                "the try's screen read again (the cancel under it)",
                Ev::Point(esc.clone()),
            ));
            beats.push(beat(At::Act(0, 6), "(a handoff instant)", Ev::Mark));
        }
        beats.push(beat(
            At::Act(0, 10),
            "the try's turn runs",
            Ev::Busy(live(&h_c, "keep going")),
        ));
        let h3 = with(&h_c, &turn(Some("keep going"), &[progress], 15 * MIN, 0));
        beats.push(beat(
            At::Act(0, 10 + 15 * MIN),
            "the try's work ends",
            Ev::Point(cc(&h3, &[], "")),
        ));
        beats.push(taken(1, &h3, "keep going"));
        out.push(sc(
            format!("R3 five-hour Esc'd, take window, the cancel under the try ({tag})"),
            default_policy(),
            beats,
            3 * HOUR,
            true,
        ));
    }
    // ---- R4: Off stood and lifted (a loop the host starts as it lifts).
    {
        // (a) Inside the try's take window.
        let mut beats = b_head();
        beats.push(beat(
            At::Act(0, 3),
            "the try's screen read again",
            Ev::Point(esc.clone()),
        ));
        beats.push(beat(At::Act(0, 5), "Off stood and lifted", Ev::OffLifted));
        beats.push(beat(
            At::Act(0, 12),
            "the try's turn runs",
            Ev::Busy(live(&h_c, "keep going")),
        ));
        let h3 = with(&h_c, &turn(Some("keep going"), &[progress], 15 * MIN, 0));
        beats.push(beat(
            At::Act(0, 12 + 15 * MIN),
            "the try's work ends",
            Ev::Point(cc(&h3, &[], "")),
        ));
        beats.push(taken(1, &h3, "keep going"));
        out.push(sc(
            "R4 Off lifts inside the try's take window (five-hour Esc'd)".to_string(),
            default_policy(),
            beats,
            3 * HOUR,
            true,
        ));
    }
    {
        // (b) A done check the weekly limit cut, Off lifting before its retry.
        let weekly_notice =
            "  ⎿  You've hit your weekly limit · resets Sep 19 at 11am (America/Los_Angeles)";
        let mut beats = b_head();
        beats.push(taken(0, &h_c, "keep going"));
        let h3 = with(&h_c, &turn(Some("keep going"), &[done], 10 * MIN, 0));
        beats.push(beat(
            At::Act(0, 10 * MIN),
            "the try's work ends in a done report",
            Ev::Point(cc(&h3, &[], "")),
        ));
        let h4 = with(&h3, &turn(Some(check), &[weekly_notice], 0, 0));
        beats.push(beat(
            At::Act(1, 3),
            "the done check met by the weekly limit",
            Ev::Point(cc(&h4, &[], "")),
        ));
        beats.push(beat(
            At::Act(1, 10 * MIN),
            "Off stood and lifted",
            Ev::OffLifted,
        ));
        beats.push(taken(2, &h4, "keep going"));
        let h5 = with(
            &h4,
            &turn(
                Some("keep going"),
                &["⏺ All done: verified again, every suite passes."],
                70,
                0,
            ),
        );
        beats.push(beat(
            At::Act(2, 72),
            "the retry's answer: done again",
            Ev::Point(cc(&h5, &[], "")),
        ));
        beats.push(taken(3, &h5, "(the check)"));
        out.push(sc(
            "R4 Off lifts at a done check the weekly limit cut".to_string(),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    {
        // (c) The spend limit's ladder, Off lifting between its tries.
        let h1 = with(&sp0, &spend_turn(Some("keep going"), 0, 0));
        let beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
            beat(
                At::Act(0, 30),
                "the try met again",
                Ev::Point(cc(&h1, &[], "")),
            ),
            beat(At::Act(0, 10 * MIN), "Off stood and lifted", Ev::OffLifted),
            taken(1, &h1, "keep going"),
        ];
        out.push(sc(
            "R4 Off lifts between the spend limit's tries".to_string(),
            default_policy(),
            beats,
            8 * HOUR,
            false,
        ));
    }
    {
        // (d) Land: the instruction typed, Off lifting before its answer.
        let p0 = cc(
            &with(&w0, &turn(None, &[progress], 0, T0 + 2 * MIN)),
            &[],
            "",
        );
        let land_turn = with(
            &w0,
            &turn(Some("(land)"), &["⏺ Stage 4 of 5 is in."], 10 * MIN, 0),
        );
        let beats = vec![
            start(),
            beat(At::Start(2 * MIN), "the worker's progress", Ev::Point(p0)),
            taken(0, &w0, "(land)"),
            beat(At::Act(0, 5 * MIN), "Off stood and lifted", Ev::OffLifted),
            beat(
                At::Act(0, 10 * MIN),
                "the land turn ends: progress",
                Ev::Point(cc(&land_turn, &[], "")),
            ),
            taken(1, &land_turn, "(next)"),
        ];
        out.push(sc(
            "R4 Off lifts during the land instruction's turn (Land)".to_string(),
            land_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- R5: the done check answered (the task done), then a person's
    // turn ending in a done report.
    for (tag, busy) in [("read busy", true), ("not read busy", false)] {
        let mut beats = b_head();
        beats.push(taken(0, &h_c, "keep going"));
        let h3 = with(&h_c, &turn(Some("keep going"), &[done], 10 * MIN, 0));
        beats.push(beat(
            At::Act(0, 10 * MIN),
            "the try's work ends in a done report",
            Ev::Point(cc(&h3, &[], "")),
        ));
        beats.push(taken(1, &h3, "If the task is finished"));
        let h4 = with(
            &h3,
            &turn(
                Some(check),
                &["⏺ All done: verified again, every suite passes."],
                70,
                0,
            ),
        );
        beats.push(beat(
            At::Act(1, 72),
            "the done check's answer: done again",
            Ev::Point(cc(&h4, &[], "")),
        ));
        if busy {
            beats.push(beat(
                At::Act(1, 20 * MIN),
                "a person's turn runs",
                Ev::Busy(live(&h4, "please also update the changelog")),
            ));
        }
        let h5 = with(
            &h4,
            &turn(
                Some("please also update the changelog"),
                &["⏺ All done: the changelog is updated, committed and pushed."],
                if busy { 12 * MIN } else { 0 },
                0,
            ),
        );
        beats.push(beat(
            At::Act(1, 32 * MIN),
            "the person's turn ends in a done report",
            Ev::Point(cc(&h5, &[], "")),
        ));
        if busy {
            beats.push(taken(2, &h5, "(the check)"));
        }
        out.push(sc(
            format!("R5 the done check answered, then a person's turn {tag}"),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- R6: the weekly window reached within the run.
    let (hw, fw) = weekly_wall(&w0, reset, wall_at, true);
    let wk_walled = cc(&hw, &fw, "");
    let hw_c = cancelled(&hw);
    let wk_esc = cc(&hw_c, &[], CANCELLED_NOTE);
    let wk_again = format!(
        "  ⎿  You've hit your weekly limit · resets Sep 17 at {} (America/Los_Angeles)",
        reset_clock(reset)
    );
    for tag in [
        "the vendor goes on",
        "Esc'd, the loop's try served",
        "Esc'd, the try met again",
        "Esc'd, the try met again twice",
    ] {
        let mut beats = vec![
            start(),
            beat(
                At::Start(12 * MIN),
                "the weekly wall, armed",
                Ev::Point(wk_walled.clone()),
            ),
        ];
        if tag == "the vendor goes on" {
            let mut h2 = hw.clone();
            h2.push("⏺ Usage limit reset · continuing automatically".to_string());
            h2.push(String::new());
            h2.extend(turn(None, &[progress], 0, reset + 65));
            beats.push(beat(
                At::Start(reset + 65 - T0),
                "the vendor goes on",
                Ev::Point(cc(&h2, &[], "")),
            ));
            beats.push(taken(0, &h2, "keep going"));
        } else {
            beats.push(beat(
                At::Start(22 * MIN),
                "Esc on the countdown",
                Ev::Point(wk_esc.clone()),
            ));
            let mut h_at = hw_c.clone();
            let tries = match tag {
                "Esc'd, the try met again" => 1,
                "Esc'd, the try met again twice" => 2,
                _ => 0,
            };
            for k in 0..tries {
                h_at = with(&h_at, &turn(Some("keep going"), &[&wk_again], 0, 0));
                beats.push(beat(
                    At::Act(k, 3),
                    "the try met the weekly window again",
                    Ev::Point(cc(&h_at, &[], "")),
                ));
            }
            beats.push(taken(tries, &h_at, "keep going"));
            let h3 = with(&h_at, &turn(Some("keep going"), &[progress], 15 * MIN, 0));
            beats.push(beat(
                At::Act(tries, 15 * MIN),
                "the try's work ends",
                Ev::Point(cc(&h3, &[], "")),
            ));
            beats.push(taken(tries + 1, &h3, "keep going"));
        }
        out.push(sc(
            format!("R6 weekly, its reset reached ({tag})"),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- R7: the five-hour window's try met by a model bucket.
    for (tag, notice, take) in [
        (
            "the Opus bucket, no reset",
            "  ⎿  You've hit your Opus limit",
            false,
        ),
        (
            "the Sonnet bucket, its reset named",
            "  ⎿  You've hit your Sonnet limit · resets Sep 19 at 4:20pm (America/Los_Angeles)",
            false,
        ),
        (
            "the Opus bucket, no reset, take window",
            "  ⎿  You've hit your Opus limit",
            true,
        ),
    ] {
        let mut beats = b_head();
        let h1 = with(&h_c, &turn(Some("keep going"), &[notice], 0, 0));
        beats.push(beat(
            At::Act(0, 3),
            "the try met by a model bucket",
            Ev::Point(cc(&h1, &[], "")),
        ));
        beats.push(taken(1, &h1, "keep going"));
        let h2 = with(&h1, &turn(Some("keep going"), &[progress], 15 * MIN, 0));
        beats.push(beat(
            At::Act(1, 15 * MIN),
            "the bucket's try's work ends",
            Ev::Point(cc(&h2, &[], "")),
        ));
        beats.push(taken(2, &h2, "keep going"));
        out.push(sc(
            format!("R7 five-hour try met by a bucket ({tag})"),
            default_policy(),
            beats,
            8 * HOUR,
            take,
        ));
    }
    // ---- R8: tries in a row before the handoff (the five-hour window).
    for take in [false, true] {
        let mut beats = b_head();
        let mut h_at = h_c.clone();
        for k in 0..2 {
            h_at = with(&h_at, &turn(Some("keep going"), &[&again], 0, 0));
            beats.push(beat(
                At::Act(k, 3),
                "the try met the five-hour window again",
                Ev::Point(cc(&h_at, &[], "")),
            ));
        }
        beats.push(taken(2, &h_at, "keep going"));
        let h3 = with(&h_at, &turn(Some("keep going"), &[progress], 12 * MIN, 0));
        beats.push(beat(
            At::Act(2, 12 * MIN),
            "the third try's work ends",
            Ev::Point(cc(&h3, &[], "")),
        ));
        beats.push(taken(3, &h3, "keep going"));
        out.push(sc(
            format!(
                "R8 five-hour Esc'd, met again twice{}",
                if take { ", take window" } else { "" }
            ),
            default_policy(),
            beats,
            3 * HOUR,
            take,
        ));
    }
    // ---- R9: a person's typing grace.
    {
        // (a) The Esc and a keystroke a minute before the try.
        let mut beats = b_head();
        beats.insert(3, beat(At::Start(22 * MIN), "the Esc's keystroke", Ev::Key));
        beats.push(beat(
            At::Start(reset + 100 - T0),
            "a person's keystroke",
            Ev::Key,
        ));
        beats.push(taken(0, &h_c, "keep going"));
        let h3 = with(&h_c, &turn(Some("keep going"), &[progress], 15 * MIN, 0));
        beats.push(beat(
            At::Act(0, 15 * MIN),
            "the try's work ends",
            Ev::Point(cc(&h3, &[], "")),
        ));
        beats.push(taken(1, &h3, "keep going"));
        out.push(sc(
            "R9 five-hour Esc'd, a person's keystroke before the try".to_string(),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    for take in [false, true] {
        // (b) The spend limit: a keystroke 20 s before the first rung, and
        // one inside the try's take window.
        let mut beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
            beat(At::Start(33 * MIN - 20), "a person's keystroke", Ev::Key),
        ];
        beats.push(beat(At::Act(0, 3), "a person's keystroke", Ev::Key));
        if take {
            beats.push(beat(
                At::Act(0, 5),
                "the try's screen read again",
                Ev::Point(spend_wall.clone()),
            ));
        }
        let h1 = with(&sp0, &spend_turn(Some("keep going"), 0, 0));
        beats.push(beat(
            At::Act(0, 8),
            "the try met again",
            Ev::Point(cc(&h1, &[], "")),
        ));
        beats.push(taken(1, &h1, "keep going"));
        out.push(sc(
            format!(
                "R9 spend, a person's keystrokes at the first rung and the try{}",
                if take { ", take window" } else { "" }
            ),
            default_policy(),
            beats,
            8 * HOUR,
            take,
        ));
    }
    // ---- R11: a person's `/compact` between.
    for (tag, busy) in [("read busy", true), ("not read busy", false)] {
        let mut beats = b_head();
        if busy {
            beats.push(beat(
                At::Start(30 * MIN),
                "a person's /compact runs",
                Ev::Busy(compacting(&h_c)),
            ));
        }
        let after = compacted("⏺ Migrated the first table.");
        beats.push(beat(
            At::Start(31 * MIN),
            "the /compact's answer",
            Ev::Point(after.clone()),
        ));
        beats.push(taken(0, &after[5..after.len() - 4], "keep going"));
        let h3 = with(
            &after[5..after.len() - 4],
            &turn(Some("keep going"), &[progress], 15 * MIN, 0),
        );
        beats.push(beat(
            At::Act(0, 15 * MIN),
            "the try's work ends",
            Ev::Point(cc(&h3, &[], "")),
        ));
        beats.push(taken(1, &h3, "keep going"));
        out.push(sc(
            format!("R11 five-hour Esc'd, a person's /compact {tag}"),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    {
        // The spend limit met again, a /compact, the ladder.
        let h1 = with(&sp0, &spend_turn(Some("keep going"), 0, 0));
        let after = compacted("⏺ Running the suite.");
        let kept = &after[5..after.len() - 4];
        let h2 = with(kept, &spend_turn(Some("keep going"), 0, 0));
        let beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
            beat(
                At::Act(0, 30),
                "the try met again",
                Ev::Point(cc(&h1, &[], "")),
            ),
            beat(
                At::Act(0, 10 * MIN),
                "a person's /compact runs",
                Ev::Busy(compacting(&h1)),
            ),
            beat(
                At::Act(0, 11 * MIN),
                "the /compact's answer",
                Ev::Point(after.clone()),
            ),
            beat(
                At::Act(1, 30),
                "the second try met again",
                Ev::Point(cc(&h2, &[], "")),
            ),
            taken(2, &h2, "keep going"),
        ];
        out.push(sc(
            "R11 spend met again, a person's /compact, the ladder".to_string(),
            default_policy(),
            beats,
            12 * HOUR,
            false,
        ));
    }
    // ---- R12: the vendor's `continuing automatically` words.
    {
        // (a) The dialog's answer `Claude Code will continue automatically
        // at …`, the countdown armed; the vendor goes on.
        let clock = reset_clock(reset);
        let dialog = with(
            &h_wall,
            &[
                "❯ /rate-limit-options".to_string(),
                format!(
                    "  ⎿  Claude Code will continue automatically at {clock}. Keep this session open; it may still pause for permission prompts. Press esc"
                ),
                "     to cancel the wait.".to_string(),
                String::new(),
            ],
        );
        let mut h2 = dialog.clone();
        h2.push("⏺ Usage limit reset · continuing automatically".to_string());
        h2.push(String::new());
        h2.extend(turn(None, &[progress], 0, reset + 65));
        let beats = vec![
            start(),
            beat(
                At::Start(12 * MIN),
                "the five-hour wall, armed",
                Ev::Point(walled.clone()),
            ),
            beat(
                At::Start(14 * MIN),
                "the dialog's 'Wait here': continue automatically",
                Ev::Point(cc(&dialog, &f_armed, "")),
            ),
            beat(
                At::Start(reset + 65 - T0),
                "the vendor goes on",
                Ev::Point(cc(&h2, &[], "")),
            ),
            taken(0, &h2, "keep going"),
        ];
        out.push(sc(
            "R12 the dialog's 'will continue automatically', the vendor goes on".to_string(),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    for (tag, seen_armed) in [("after the armed wall", true), ("first seen so", false)] {
        // (b) The countdown's record `⏺ Usage limit reached · continuing
        // automatically at …` standing with no countdown (a `/resume` back
        // into the conversation dropped it): nobody goes on.
        let mut beats = vec![start()];
        if seen_armed {
            beats.push(beat(
                At::Start(12 * MIN),
                "the five-hour wall, armed",
                Ev::Point(walled.clone()),
            ));
        }
        beats.push(beat(
            At::Start(20 * MIN),
            "a /resume back: the countdown's record, no countdown",
            Ev::Point(cc(&h_wall, &[], "")),
        ));
        beats.push(taken(0, &h_wall, "keep going"));
        let h3 = with(&h_wall, &turn(Some("keep going"), &[progress], 15 * MIN, 0));
        beats.push(beat(
            At::Act(0, 15 * MIN),
            "the try's work ends",
            Ev::Point(cc(&h3, &[], "")),
        ));
        beats.push(taken(1, &h3, "keep going"));
        out.push(sc(
            format!("R12 the countdown's record standing, no countdown ({tag})"),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- R13: short turns in a row (the streak's back-off is read from
    // the screen again, not seeded), and Off lifting in its back-off before
    // a person's own short exchange (the carry the park drops, which the
    // acts a successor seeds give back).
    for tag in [
        "short turns in a row",
        "Off lifts in the back-off, a person's exchange",
    ] {
        let p0 = cc(&with(&w0, &turn(None, &[progress], 0, T0)), &[], "");
        let mut beats = vec![beat(At::Start(0), "the worker's short turn", Ev::Point(p0))];
        let mut h = with(&w0, &turn(None, &[progress], 0, T0));
        if tag == "short turns in a row" {
            for k in 0..3 {
                h = with(
                    &h,
                    &turn(Some("keep going"), &["⏺ Stage 3 of 5: still on it."], 20, 0),
                );
                beats.push(beat(
                    At::Act(k, 20),
                    "the act's short turn ends",
                    Ev::Point(cc(&h, &[], "")),
                ));
            }
            beats.push(taken(3, &h, "keep going"));
        } else {
            h = with(
                &h,
                &turn(Some("keep going"), &["⏺ Stage 3 of 5: still on it."], 20, 0),
            );
            beats.push(beat(
                At::Act(0, 20),
                "the act's short turn ends",
                Ev::Point(cc(&h, &[], "")),
            ));
            beats.push(beat(At::Act(0, 60), "Off stood and lifted", Ev::OffLifted));
            beats.push(beat(At::Act(0, 70), "a person's keystroke", Ev::Key));
            beats.push(beat(
                At::Act(0, 75),
                "a person's message runs",
                Ev::Busy(live(&h, "what is left to do?")),
            ));
            let h2 = with(
                &h,
                &turn(
                    Some("what is left to do?"),
                    &["⏺ Two stages are left: the parser and the docs."],
                    3 * MIN,
                    0,
                ),
            );
            beats.push(beat(
                At::Act(0, 75 + 3 * MIN),
                "the person's exchange ends",
                Ev::Point(cc(&h2, &[], "")),
            ));
            beats.push(taken(1, &h2, "(next)"));
        }
        out.push(sc(
            format!("R13 {tag}"),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    out
}

/// THE SHAPES GROWN IN ROUND 8 (the verifier of 97daeeace, RS8): Off
/// standing between a try and its late first read (a record of the loop's
/// is no read); the short streak moved at points no act follows — a
/// person's short turn, a wake's, a done check's answer judged not done —
/// and held past the window, to the ladder's top; the task ended, then a
/// later turn's done report; and an act's yield seen run by a successor
/// attached in its turn (a land act's done reply, a spend limit's try's
/// real work).
#[allow(clippy::too_many_lines)]
fn round8_scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();
    let w0 = work0();
    let done = "⏺ All done: the change is committed and pushed, and every suite passes.";
    let done2 = "⏺ All done: verified again, every suite passes.";
    let progress = "⏺ Stage 3 of 5 is in; stage 4 is next.";
    let still = "⏺ Stage 3 of 5: still on it.";
    let check = "If the task is finished and verified, end your reply with ATERM-TASK-DONE on a line by itself and stop; otherwise continue.";
    let start = || {
        beat(
            At::Start(0),
            "the worker's turn",
            Ev::Busy(live(&w0, "carry on with the task")),
        )
    };
    let sc = |name: String, policy: SupervisorConfig, beats: Vec<Beat>, tail_s: i64, take: bool| {
        Scenario {
            name,
            t0: T0,
            policy,
            host: None,
            beats,
            tail_s,
            take,
            person_on_screen: false,
        }
    };
    // The worker's short turn, then `n` short yields to `keep going`.
    let short_yields = |n: usize, beats: &mut Vec<Beat>| {
        let p0 = cc(&with(&w0, &turn(None, &[progress], 0, T0)), &[], "");
        beats.push(beat(At::Start(0), "the worker's short turn", Ev::Point(p0)));
        let mut h = with(&w0, &turn(None, &[progress], 0, T0));
        for k in 0..n {
            h = with(&h, &turn(Some("keep going"), &[still], 20, 0));
            beats.push(beat(
                At::Act(k, 20),
                "the act's short turn ends",
                Ev::Point(cc(&h, &[], "")),
            ));
        }
        h
    };
    // ---- RS8-R1: Off stood between the spend limit's try and its first
    // read 20 s on (the record of it no read of the try's).
    let sp0 = with(&w0, &spend_turn(None, 3 * MIN, T0 + 3 * MIN));
    let spend_wall = cc(&sp0, &[], "");
    for off_at in [2, 10] {
        let end = with(&sp0, &spend_turn(Some("keep going"), 3 * HOUR, 0));
        let beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
            beat(At::Act(0, off_at), "Off stood and lifted", Ev::OffLifted),
            beat(
                At::Act(0, 20),
                "the try's screen read",
                Ev::Point(spend_wall.clone()),
            ),
            beat(At::Act(0, 34), "(a handoff instant)", Ev::Mark),
            beat(
                At::Act(0, 40),
                "the try's turn runs",
                Ev::Busy(live(&sp0, "keep going")),
            ),
            beat(
                At::Act(0, 40 + 3 * HOUR),
                "the try's 3h turn ends on the limit",
                Ev::Point(cc(&end, &[], "")),
            ),
            taken(1, &end, "keep going"),
        ];
        out.push(sc(
            format!("RS8-R1 spend, take window, Off stood {off_at} s after the try"),
            default_policy(),
            beats,
            8 * HOUR,
            true,
        ));
    }
    // ---- RS8-R3: short turns in a row, then a person's short turn after
    // the last yield was judged (no act between).
    for acts in [2, 3] {
        let mut beats = Vec::new();
        let h = short_yields(acts, &mut beats);
        let k = acts - 1;
        beats.push(beat(At::Act(k, 30), "a person's keystroke", Ev::Key));
        beats.push(beat(
            At::Act(k, 35),
            "a person's message runs",
            Ev::Busy(live(&h, "what is left to do?")),
        ));
        let h2 = with(
            &h,
            &turn(
                Some("what is left to do?"),
                &["⏺ Two stages are left: the parser and the docs."],
                20,
                0,
            ),
        );
        beats.push(beat(
            At::Act(k, 55),
            "the person's short turn ends",
            Ev::Point(cc(&h2, &[], "")),
        ));
        beats.push(taken(acts, &h2, "keep going"));
        out.push(sc(
            format!("RS8-R3 {acts} short turns, then a person's short turn"),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- RS8-R4: the done check answered NOT done (a short reply), then a
    // person's turn ending in a done report.
    {
        let p0 = with(&w0, &turn(None, &[done], 10 * MIN, T0 + 10 * MIN));
        let mut beats = vec![
            start(),
            beat(
                At::Start(10 * MIN),
                "the worker's done report",
                Ev::Point(cc(&p0, &[], "")),
            ),
            taken(0, &p0, "If the task is finished"),
        ];
        let h4 = with(
            &p0,
            &turn(
                Some(check),
                &["⏺ Not yet: one suite still fails; I am on it."],
                20,
                0,
            ),
        );
        beats.push(beat(
            At::Act(0, 22),
            "the done check's answer: not done",
            Ev::Point(cc(&h4, &[], "")),
        ));
        beats.push(beat(At::Act(0, 40), "a person's keystroke", Ev::Key));
        beats.push(beat(
            At::Act(0, 45),
            "a person's turn runs",
            Ev::Busy(live(&h4, "please also update the changelog")),
        ));
        let h5 = with(
            &h4,
            &turn(
                Some("please also update the changelog"),
                &["⏺ All done: the changelog is updated, committed and pushed."],
                3 * MIN,
                0,
            ),
        );
        beats.push(beat(
            At::Act(0, 45 + 3 * MIN),
            "the person's turn ends in a done report",
            Ev::Point(cc(&h5, &[], "")),
        ));
        beats.push(taken(1, &h5, "(the check)"));
        out.push(sc(
            "RS8-R4 the done check answered not done, then a person's done report".to_string(),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- RS8-F1: the task done (the check answered), then a new short
    // turn read busy: its done report a first one.
    for (tag, reply, hand) in [
        ("nobody's hand, a done report", done, false),
        ("nobody's hand, progress", progress, false),
        ("a person's hand, progress", progress, true),
    ] {
        let h1 = with(
            &w0,
            &turn(Some("carry on with the task"), &[done], 10 * MIN, 0),
        );
        let mut beats = vec![
            start(),
            beat(
                At::Start(10 * MIN),
                "the worker's done report",
                Ev::Point(cc(&h1, &[], "")),
            ),
            taken(0, &h1, "If the task is finished"),
        ];
        let h2 = with(&h1, &turn(Some(check), &[done2], 70, 0));
        beats.push(beat(
            At::Act(0, 72),
            "the check's answer: done again",
            Ev::Point(cc(&h2, &[], "")),
        ));
        if hand {
            beats.push(beat(
                At::Act(0, 20 * MIN - 5),
                "a person's keystroke",
                Ev::Key,
            ));
        }
        beats.push(beat(
            At::Act(0, 20 * MIN),
            "a new turn runs",
            Ev::Busy(live(&h2, "now bump the version and tag it")),
        ));
        let h3 = with(
            &h2,
            &turn(Some("now bump the version and tag it"), &[reply], 30, 0),
        );
        beats.push(beat(
            At::Act(0, 20 * MIN + 30),
            "the new turn ends",
            Ev::Point(cc(&h3, &[], "")),
        ));
        beats.push(taken(1, &h3, "(next)"));
        let h4 = with(&h3, &turn(Some("(next)"), &[progress], 10 * MIN, 0));
        beats.push(beat(
            At::Act(1, 10 * MIN),
            "the next act's turn ends: progress",
            Ev::Point(cc(&h4, &[], "")),
        ));
        out.push(sc(
            format!("RS8-F1 the task done, then a new short turn ({tag})"),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- RS8-F2: Land, landed by the land act's 10 min done reply, then a
    // new task's short turn (the land act's yield seen run).
    for (tag, reply) in [("a done report", done), ("progress", progress)] {
        let p0 = cc(
            &with(&w0, &turn(None, &[progress], 0, T0 + 2 * MIN)),
            &[],
            "",
        );
        let mut beats = vec![
            start(),
            beat(At::Start(2 * MIN), "the worker's progress", Ev::Point(p0)),
            taken(0, &w0, "(land)"),
        ];
        let h1 = with(&w0, &turn(Some("(land)"), &[done], 10 * MIN, 0));
        beats.push(beat(
            At::Act(0, 10 * MIN),
            "the land turn ends: done (landed)",
            Ev::Point(cc(&h1, &[], "")),
        ));
        beats.push(beat(
            At::Act(0, 20 * MIN),
            "a new task's turn runs",
            Ev::Busy(live(&h1, "now write the docs")),
        ));
        let h2 = with(&h1, &turn(Some("now write the docs"), &[reply], 30, 0));
        beats.push(beat(
            At::Act(0, 20 * MIN + 30),
            "the new task's short turn ends",
            Ev::Point(cc(&h2, &[], "")),
        ));
        beats.push(taken(1, &h2, "(next)"));
        let h3 = with(&h2, &turn(Some("(next)"), &[progress], 10 * MIN, 0));
        beats.push(beat(
            At::Act(1, 10 * MIN),
            "the next act's turn ends: progress",
            Ev::Point(cc(&h3, &[], "")),
        ));
        out.push(sc(
            format!("RS8-F2 Land, landed, then a new task's short turn ({tag})"),
            land_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- RS8-F2: a wake's short turn between two acts.
    {
        let mut beats = Vec::new();
        let mut h = short_yields(1, &mut beats);
        beats.push(beat(
            At::Act(0, 80),
            "a turn nobody typed runs (the worker's own wake)",
            Ev::Busy(live(&h, "(background task finished)")),
        ));
        h = with(
            &h,
            &turn(
                Some("(background task finished)"),
                &["⏺ The build is still going."],
                20,
                0,
            ),
        );
        beats.push(beat(
            At::Act(0, 100),
            "the wake's short turn ends",
            Ev::Point(cc(&h, &[], "")),
        ));
        for k in 1..3 {
            h = with(&h, &turn(Some("keep going"), &[still], 20, 0));
            beats.push(beat(
                At::Act(k, 20),
                "the act's short turn ends",
                Ev::Point(cc(&h, &[], "")),
            ));
        }
        beats.push(taken(3, &h, "keep going"));
        out.push(sc(
            "RS8-F2 short turns, a wake's short turn between two acts".to_string(),
            default_policy(),
            beats,
            4 * HOUR,
            false,
        ));
    }
    // ---- RS8-F2: short turns, the task done at the check's short answer,
    // a wake two hours on (past the window).
    {
        let mut beats = Vec::new();
        let mut h = short_yields(1, &mut beats);
        h = with(&h, &turn(Some("keep going"), &[done], 20, 0));
        beats.push(beat(
            At::Act(1, 20),
            "the act's short turn ends: done",
            Ev::Point(cc(&h, &[], "")),
        ));
        h = with(&h, &turn(Some(check), &[done2], 20, 0));
        beats.push(beat(
            At::Act(2, 20),
            "the check's short answer: done",
            Ev::Point(cc(&h, &[], "")),
        ));
        beats.push(beat(
            At::Act(2, 2 * HOUR),
            "a turn nobody typed runs (the worker's own wake)",
            Ev::Busy(live(&h, "(background task finished)")),
        ));
        h = with(
            &h,
            &turn(
                Some("(background task finished)"),
                &["⏺ The build is still going."],
                20,
                0,
            ),
        );
        beats.push(beat(
            At::Act(2, 2 * HOUR + 20),
            "the wake's short turn ends",
            Ev::Point(cc(&h, &[], "")),
        ));
        beats.push(taken(3, &h, "keep going"));
        out.push(sc(
            "RS8-F2 short turns, the task done, a wake two hours on".to_string(),
            default_policy(),
            beats,
            4 * HOUR,
            false,
        ));
    }
    // ---- RS8-F2/F3: short turns in a row to the ladder's top (60 min).
    {
        let mut beats = Vec::new();
        let h = short_yields(7, &mut beats);
        beats.push(taken(7, &h, "keep going"));
        out.push(sc(
            "RS8-F3 short turns in a row to the ladder's top".to_string(),
            default_policy(),
            beats,
            6 * HOUR,
            false,
        ));
    }
    // ---- RS8-F2: short turns, the spend limit at a short yield, the try's
    // real work (no continuation: its yield keeps the streak).
    {
        let mut beats = Vec::new();
        let mut h = short_yields(1, &mut beats);
        h = with(&h, &spend_turn(Some("keep going"), 20, 0));
        beats.push(beat(
            At::Act(1, 20),
            "the act's short turn ends on the spend limit",
            Ev::Point(cc(&h, &[], "")),
        ));
        beats.push(taken(2, &h, "keep going"));
        h = with(&h, &turn(Some("keep going"), &[progress], 10 * MIN, 0));
        beats.push(beat(
            At::Act(2, 10 * MIN),
            "the try's 10 min of work ends: progress",
            Ev::Point(cc(&h, &[], "")),
        ));
        beats.push(taken(3, &h, "keep going"));
        h = with(&h, &turn(Some("keep going"), &[progress], 10 * MIN, 0));
        beats.push(beat(
            At::Act(3, 10 * MIN),
            "the next act's work ends: progress",
            Ev::Point(cc(&h, &[], "")),
        ));
        out.push(sc(
            "RS8-F2 short turns, the spend limit, the try's real work".to_string(),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    out
}

/// Round 9's shapes (the verifier of 75b03ac6d): a point whose time moved
/// with nothing a record held (RS9-F2), a person's exchange after the task's
/// end (RS9-F3), and the land act answered not done (RS9-F4).
fn round9_scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();
    let w0 = work0();
    let progress = "⏺ Stage 3 of 5 is in; stage 4 is next.";
    let finished = [
        "⏺ All done: the change is committed and pushed.",
        "",
        "  ATERM-TASK-DONE",
    ];
    let start = || {
        beat(
            At::Start(0),
            "the worker's turn",
            Ev::Busy(live(&w0, "carry on with the task")),
        )
    };
    let sc =
        |name: &str, policy: SupervisorConfig, beats: Vec<Beat>, person_on_screen: bool| Scenario {
            name: name.to_string(),
            t0: T0,
            policy,
            host: None,
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen,
        };
    // ---- RS9-F2: the worker's FINISHED (the streak 0), then a turn nobody
    // here typed (an orchestrator's), 5 min of work, stopped with Esc: the
    // Esc hold counts from that turn's end, which moved no streak. The Esc
    // with no keystroke the window stamps (an orchestrator's `aterm ctl
    // key`), and, the control, the Esc a person's key.
    for (tag, key) in [("no key", false), ("the Esc a key", true)] {
        let h1 = with(&w0, &turn(None, &finished, 10 * MIN, 0));
        let mut beats = vec![
            start(),
            beat(
                At::Start(10 * MIN),
                "the worker's FINISHED",
                Ev::Point(cc(&h1, &[], "")),
            ),
            beat(
                At::Start(20 * MIN),
                "an orchestrator's turn runs",
                Ev::Busy(live(&h1, "now bump the version and tag it")),
            ),
        ];
        if key {
            beats.push(beat(At::Start(25 * MIN), "a person's Esc", Ev::Key));
        }
        let mut h2 = h1.clone();
        h2.extend(s(&[
            "❯ now bump the version and tag it",
            "",
            "⏺ Bumping the version.",
            "  ⎿  Interrupted · What should Claude do instead?",
            "",
        ]));
        beats.push(beat(
            At::Start(25 * MIN),
            "the turn stopped with Esc",
            Ev::Point(cc(&h2, &[], "")),
        ));
        beats.push(taken(0, &h2, "keep going"));
        let h3 = with(&h2, &turn(Some("keep going"), &[progress], 10 * MIN, 0));
        beats.push(beat(
            At::Act(0, 10 * MIN),
            "the act's turn ends: progress",
            Ev::Point(cc(&h3, &[], "")),
        ));
        out.push(sc(
            &format!("RS9-F2 FINISHED, an orchestrator's long turn Esc'd ({tag})"),
            default_policy(),
            beats,
            false,
        ));
    }
    // ---- RS9-F3: the worker's FINISHED, then a person's own exchange (3
    // min, left to them), then a wake's short turn (nobody's hand). Every
    // read carries the person's stamp.
    {
        let h1 = with(&w0, &turn(None, &finished, 10 * MIN, 0));
        let mut beats = vec![
            start(),
            beat(
                At::Start(10 * MIN),
                "the worker's FINISHED",
                Ev::Point(cc(&h1, &[], "")),
            ),
            beat(At::Start(15 * MIN), "a person's keystroke", Ev::Key),
            beat(
                At::Start(15 * MIN + 1),
                "a person's message runs",
                Ev::Busy(live(&h1, "what is left?")),
            ),
        ];
        let h2 = with(
            &h1,
            &turn(
                Some("what is left?"),
                &["⏺ The parser and the docs."],
                3 * MIN,
                0,
            ),
        );
        beats.push(beat(
            At::Start(18 * MIN + 1),
            "the person's exchange ends",
            Ev::Point(cc(&h2, &[], "")),
        ));
        beats.push(beat(
            At::Start(40 * MIN),
            "a wake runs",
            Ev::Busy(live(&h2, "(background task finished)")),
        ));
        let h3 = with(
            &h2,
            &turn(
                Some("(background task finished)"),
                &["⏺ The build is still going."],
                20,
                0,
            ),
        );
        beats.push(beat(
            At::Start(40 * MIN + 20),
            "the wake's short turn ends",
            Ev::Point(cc(&h3, &[], "")),
        ));
        beats.push(taken(0, &h3, "keep going"));
        let h4 = with(&h3, &turn(Some("keep going"), &[progress], 10 * MIN, 0));
        beats.push(beat(
            At::Act(0, 10 * MIN),
            "the act's turn ends: progress",
            Ev::Point(cc(&h4, &[], "")),
        ));
        out.push(sc(
            "RS9-F3 FINISHED, a person's exchange, then a wake's short turn",
            default_policy(),
            beats,
            true,
        ));
    }
    // ---- RS9-F4: RS8-R4 under Land: the land instruction answered NOT
    // done (a short reply), then a turn ending in a done report — a
    // person's, and nobody's: no landing in hand, so Land nudges.
    for (tag, with_key) in [("a person's hand", true), ("nobody's hand", false)] {
        let p0 = cc(
            &with(&w0, &turn(None, &[progress], 0, T0 + 2 * MIN)),
            &[],
            "",
        );
        let mut beats = vec![
            start(),
            beat(At::Start(2 * MIN), "the worker's progress", Ev::Point(p0)),
            taken(0, &w0, "(land)"),
        ];
        let h1 = with(
            &w0,
            &turn(
                Some("(land)"),
                &["⏺ Not yet: one suite still fails; I am on it."],
                20,
                0,
            ),
        );
        beats.push(beat(
            At::Act(0, 22),
            "the land act's answer: not done",
            Ev::Point(cc(&h1, &[], "")),
        ));
        if with_key {
            beats.push(beat(At::Act(0, 40), "a person's keystroke", Ev::Key));
        }
        beats.push(beat(
            At::Act(0, 41),
            "a turn runs",
            Ev::Busy(live(&h1, "please also update the changelog")),
        ));
        let h2 = with(
            &h1,
            &turn(
                Some("please also update the changelog"),
                &["⏺ All done: the changelog is updated, committed and pushed."],
                3 * MIN,
                0,
            ),
        );
        beats.push(beat(
            At::Act(0, 41 + 3 * MIN),
            "the turn ends in a done report",
            Ev::Point(cc(&h2, &[], "")),
        ));
        beats.push(taken(1, &h2, "(next)"));
        let h3 = with(&h2, &turn(Some("(next)"), &[progress], 10 * MIN, 0));
        beats.push(beat(
            At::Act(1, 10 * MIN),
            "the next act's turn ends: progress",
            Ev::Point(cc(&h3, &[], "")),
        ));
        out.push(sc(
            &format!("RS9-F4 Land, the land act answered not done, then a done report ({tag})"),
            land_policy(),
            beats,
            false,
        ));
    }
    out
}

/// Each scenario's matrix, in order: run on the machine's threads (each
/// cell's loops are its own, on the fixed clock).
fn run_all(scenarios: &[Scenario], seed: bool) -> Vec<(String, Tally)> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let mut out: Vec<(usize, (String, Tally))> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers.min(scenarios.len()))
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(sc) = scenarios.get(i) else {
                            return done;
                        };
                        done.push((i, run_matrix(sc, seed)));
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a scenario's matrix"))
            .collect()
    });
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, r)| r).collect()
}

#[test]
fn a_loop_that_follows_types_as_the_first_loop_at_every_handoff() {
    let scenarios = scenarios();
    let runs = run_all(&scenarios, true);
    let cells: usize = runs.iter().map(|(_, t)| t.cells).sum();
    let broken: Vec<&String> = runs.iter().flat_map(|(_, t)| &t.broken).collect();
    assert!(cells > 500, "the matrix ran {cells} cells");
    assert!(
        broken.is_empty(),
        "{} of {cells} cells break the rule:\n{}",
        broken.len(),
        broken
            .iter()
            .map(|b| b.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    // NEGATIVE CONTROL: successors that take nothing up from the ledger, at
    // a spend limit met again and at a five-hour window's try met again,
    // type sooner than the first loop, and the matrix says so.
    let controls: Vec<Scenario> = scenarios
        .into_iter()
        .filter(|sc| {
            sc.name == "D spend (met again, the ladder)"
                || sc.name == "B five-hour Esc'd, the loop's try (met again)"
        })
        .collect();
    assert_eq!(controls.len(), 2, "the control's scenarios");
    for (out, tally) in run_all(&controls, false) {
        assert!(
            tally.broken.iter().any(|b| b.starts_with("SOONER")),
            "CONTROL: a successor with no seed broke nothing:\n{out}"
        );
    }
}

/// A LAND EPISODE A READING IN ANOTHER MODE ENDED IS OVER FOR A LOOP THAT
/// FOLLOWS (the record of it, [`crate::supervise::approvals::RULE_LAND_OVER`];
/// the episode's seed, RS7-2). The land instruction is typed under Land; the
/// next point, its short yield, is read under Cruise and types nothing (the
/// short streak's back-off): the episode is over, a Land after it is a new
/// one, and the ledger holds no episode for a loop that follows
/// ([`crate::supervise::approvals::land_episode`]). With no row, a loop that
/// follows under Land again would nudge, or type nothing for a landing
/// spent, where the loop that read Cruise types the instruction anew — the
/// matrix's scenarios hold one mode each. NEGATIVE CONTROL: the same point
/// read under Land keeps the episode.
#[test]
fn a_land_episode_a_reading_in_another_mode_ended_is_over_for_a_loop_that_follows() {
    use crate::supervise::config::Autopilot;
    let dir = crate::supervise::test_scratch_path("seed-matrix", "land-over");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let w0 = work0();
    let progress = cc(
        &with(
            &w0,
            &turn(None, &["⏺ Stage 3 of 5 is in."], 0, T0 + 2 * MIN),
        ),
        &[],
        "",
    );
    let yielded = cc(
        &with(
            &w0,
            &turn(
                Some("(land)"),
                &["⏺ Stage 4 of 5 is in."],
                20,
                T0 + 2 * MIN + 20,
            ),
        ),
        &[],
        "",
    );
    fn decide(
        s: &mut Session<'_, Mock>,
        rows: &[String],
        seq: u64,
        worked: u64,
        opts: &SuperviseOpts,
    ) -> Option<&'static str> {
        s.ctl.screens = vec![rows.to_vec()];
        s.ctl.served = 0;
        let p = point_at(s, rows.to_vec(), seq);
        let a = s
            .turn_end_at_point(&p, Some(Duration::from_secs(worked)), opts)
            .expect("a decision");
        let rule = a.rule_id();
        s.turn_end_execute(&p, a, opts, &[], &mut Alone)
            .expect("decided");
        rule
    }
    let under = |autopilot: Autopilot| SuperviseOpts {
        policy: SupervisorConfig {
            autopilot,
            ..land_policy()
        },
        ..auto(30, None)
    };
    for (mode, over) in [(Autopilot::Cruise, true), (Autopilot::Land, false)] {
        let ledger = dir.join(format!("{}.jsonl", mode.word()));
        let mut m = Mock::new(vec![Vec::new()]);
        let mut s = session(&mut m, Some(SID.to_string()));
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some(SID), &mut Vec::new());
        s.turn_end_seeded = true;
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        s.set_clock(T0 + 2 * MIN, PDT);
        let rule = decide(&mut s, &progress, 101, 120, &under(Autopilot::Land));
        assert_eq!(
            rule,
            Some(crate::supervise::policy::turn_end::RULE_LAND),
            "the instruction"
        );
        s.set_clock(T0 + 2 * MIN + 20, PDT);
        let rule = decide(&mut s, &yielded, 102, 20, &under(mode));
        assert_eq!(rule, None, "{mode:?}: the short yield is backed off");
        drop(s);
        let episode = crate::supervise::approvals::land_episode(&ledger, Some(SID));
        assert_eq!(episode.is_none(), over, "{mode:?}: {episode:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A LOOP WHOSE SEED FOUND THE LEDGER'S TRY AGED OUT WRITES THE TRY OVER,
/// NOT THE WALL'S END, before its first wall row (RS8-R2,
/// [`crate::supervise::approvals::RULE_TRY_OVER`]). The ledger: a done check
/// typed in a spend limit's place three hours ago, past its two-hour rung.
/// This loop's first point is the spend limit: the try is over for a loop
/// that follows it ([`crate::supervise::approvals::usage_attempt`]), and no
/// row ends a wall — what a wall stands for (a check it cut, a move refused
/// at it) stands, as for this loop. NEGATIVE CONTROL: the try an hour old is
/// inside its rung, and the wall row alone is written. Written as the wall's
/// end ([`crate::supervise::approvals::LIMIT_ENDED`]), a third loop read a
/// check the wall cut as answered by nothing and asked it again after the
/// retry's done reply, and asked a move again at the wall it was refused at.
#[test]
fn a_try_a_seed_found_aged_out_is_over_and_ends_no_wall() {
    use crate::supervise::approvals::{self, Outcome, Row};
    use crate::supervise::policy::turn_end::{LimitTry, RULE_DONE_CHECK};
    let dir = crate::supervise::test_scratch_path("seed-matrix", "try-over");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let limit = LimitTry {
        bucket: false,
        in_place: true,
        attempts: 1,
        entitlement: true,
    };
    for (ago_s, over) in [(3 * HOUR, true), (HOUR, false)] {
        let ledger = dir.join(format!("books-{ago_s}.jsonl"));
        let now_ms = crate::supervise::journal::unix_ms();
        let at = now_ms - ago_s * 1000;
        let wall = approvals::wall_wait_reason(
            None,
            None,
            aterm_phase::WallKind::Spend,
            "You've hit your monthly spend limit.",
        );
        let check = approvals::act_reason(Some(limit));
        let body: String = [
            (
                at - 1000,
                approvals::RULE_WALL_WAIT,
                Outcome::Waiting,
                "",
                &wall,
            ),
            (
                at,
                RULE_DONE_CHECK,
                Outcome::Typed,
                "If the task is finished",
                &check,
            ),
        ]
        .iter()
        .map(|(ts, rule_id, outcome, command, reason)| {
            let row = Row {
                rule_id,
                outcome: *outcome,
                command,
                reason,
                box_seq: 1,
            };
            format!("{}\n", row.to_json(*ts, Some(SID)))
        })
        .collect();
        std::fs::write(&ledger, &body).expect("the books");
        let mut m = Mock::new(vec![Vec::new()]);
        let mut s = session(&mut m, Some(SID.to_string()));
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some(SID), &mut Vec::new());
        s.turn_end_seeded = true;
        s.set_clock(T0, PDT);
        let open = s.seed_from_ledger(&ledger, now_ms, false);
        s.seed_wall_memory(open);
        let sp = with(
            &work0(),
            &spend_turn(Some("If the task is finished"), 20, T0),
        );
        let rows = cc(&sp, &[], "");
        s.ctl.screens = vec![rows.clone()];
        s.ctl.served = 0;
        let opts = auto(30, None);
        let p = point_at(&s, rows, 101);
        let a = s.turn_end_at_point(&p, None, &opts).expect("a decision");
        s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
            .expect("decided");
        drop(s);
        let after = std::fs::read_to_string(&ledger).expect("the books");
        let rules: Vec<String> = after
            .lines()
            .skip(2)
            .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
            .filter_map(|v| {
                v.get("rule_id")
                    .and_then(aterm_json::Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        let wall_at = rules
            .iter()
            .position(|r| r == approvals::RULE_WALL_WAIT)
            .unwrap_or_else(|| panic!("{ago_s}: the wall: {rules:?}"));
        let before: Vec<&str> = rules[..wall_at].iter().map(String::as_str).collect();
        let want: &[&str] = if over {
            &[approvals::RULE_TRY_OVER]
        } else {
            &[]
        };
        assert_eq!(before, want, "{ago_s}: {rules:?}");
        assert!(
            !rules.iter().any(|r| r == approvals::RULE_WALL_WAIT_END),
            "{ago_s}: no end of the wall: {rules:?}"
        );
        assert_eq!(
            approvals::usage_attempt(&ledger, Some(SID)).is_none(),
            over,
            "{ago_s}: {after}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE TRY'S FIRST READ CAME LATE, AND A SUCCESSOR ATTACHED INSIDE THE
/// WINDOW IT OPENED AWAITS THE TRY (RS8-F4): the spend limit's try, its
/// first read 70 s on (the Mac asleep, an outage ridden out), its turn's
/// busy read 90 s on, its three-hour turn ending on the limit again. The
/// first loop's window counts from its late read, so the busy read takes
/// the try, and it types 30 minutes from the wall that turn ends on; so does
/// a successor attached after that read, the window the ledger shows (the
/// wall's row) counted from there however late it came. Capped at twice the
/// window, the successor judged the try met at the screen it was typed at,
/// kept its two-hour rung through the try's own turn, and typed into the
/// standing limit 30 minutes sooner. NEGATIVE CONTROL: a successor that
/// takes nothing up does so. (The cells before the late read are the
/// matrix's to model: a loop reading sooner opens its window sooner.)
#[test]
fn a_try_s_late_first_read_opens_the_window_a_successor_awaits() {
    let w0 = work0();
    let sp0 = with(&w0, &spend_turn(None, 3 * MIN, T0 + 3 * MIN));
    let spend_wall = cc(&sp0, &[], "");
    let end = with(&sp0, &spend_turn(Some("keep going"), 3 * HOUR, 0));
    let sc = Scenario {
        name: "RS8-F4 spend, take window, the first read 70 s on".to_string(),
        t0: T0,
        policy: default_policy(),
        host: None,
        beats: vec![
            beat(
                At::Start(0),
                "the worker's turn",
                Ev::Busy(live(&w0, "carry on with the task")),
            ),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
            beat(
                At::Act(0, 70),
                "the try's screen read",
                Ev::Point(spend_wall),
            ),
            beat(At::Act(0, 75), "(a handoff instant)", Ev::Mark),
            beat(
                At::Act(0, 90),
                "the try's turn runs",
                Ev::Busy(live(&sp0, "keep going")),
            ),
            beat(
                At::Act(0, 90 + 3 * HOUR),
                "the try's 3h turn ends on the limit",
                Ev::Point(cc(&end, &[], "")),
            ),
            taken(1, &end, "keep going"),
        ],
        tail_s: 8 * HOUR,
        take: true,
        person_on_screen: false,
    };
    let dir = crate::supervise::test_scratch_path("seed-matrix", "late-first-read");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let first = run_first(&sc, &dir);
    let tried = first.acts.first().expect("the try").at;
    assert_eq!(first.acts.len(), 2, "the try and the next: {:?}", first.log);
    for after_read in [71, 76] {
        let h = tried + after_read;
        for (seed, want) in [(true, None), (false, Some("SOONER"))] {
            let (theirs, at_k, due, log) = run_successor(&sc, &first, h, &dir, seed);
            assert_eq!(
                verdict(first.acts.get(at_k), theirs.as_ref(), due),
                want,
                "+{after_read} seeded={seed}:\n{}",
                log.join("\n")
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A LEDGER NO LOOP RECORDED POINTS IN SEEDS NO YIELD (RS8-F2): an older
/// build wrote no point records ([`crate::supervise::approvals::RULE_POINT`]),
/// so its last act may have been answered already, and a turn a successor
/// sees run is judged as someone else's, its short turn lengthening the
/// streak, as before. A ledger this build wrote records every act's answer:
/// its last act's yield is still to come, and moves the streak as the act's
/// own arm does (a wall's retry: none). NEGATIVE CONTROL: the same acts with
/// a point record before them.
#[test]
fn a_ledger_no_loop_recorded_points_in_seeds_no_yield() {
    use crate::supervise::approvals::{self, Outcome, Row};
    use crate::supervise::policy::turn_end::{RULE_API_RETRY, RULE_CONTINUE};
    let dir = crate::supervise::test_scratch_path("seed-matrix", "unrecorded");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    for (recorded, want) in [(false, 4), (true, 3)] {
        let ledger = dir.join(format!("books-{recorded}.jsonl"));
        let now_ms = crate::supervise::journal::unix_ms();
        let marked = approvals::act_reason_after(approvals::Carried::streak(3), None);
        let point = approvals::point_reason(approvals::Carried::streak(3), false);
        let mut rows = vec![(
            now_ms - 20 * MIN * 1000,
            RULE_CONTINUE,
            Outcome::Typed,
            "keep going",
            marked.as_str(),
        )];
        if recorded {
            rows.push((
                now_ms - 15 * MIN * 1000,
                approvals::RULE_POINT,
                Outcome::Waiting,
                "",
                point.as_str(),
            ));
        }
        rows.push((
            now_ms - 10 * MIN * 1000,
            RULE_API_RETRY,
            Outcome::Typed,
            "keep going",
            marked.as_str(),
        ));
        let body: String = rows
            .iter()
            .map(|(ts, rule_id, outcome, command, reason)| {
                let row = Row {
                    rule_id,
                    outcome: *outcome,
                    command,
                    reason,
                    box_seq: 1,
                };
                format!("{}\n", row.to_json(*ts, Some(SID)))
            })
            .collect();
        std::fs::write(&ledger, &body).expect("the books");
        let mut m = Mock::new(vec![Vec::new()]);
        let mut s = session(&mut m, Some(SID.to_string()));
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some(SID), &mut Vec::new());
        s.turn_end_seeded = true;
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        s.set_clock(T0, PDT);
        let open = s.seed_from_ledger(&ledger, now_ms, false);
        s.seed_wall_memory(open);
        assert_eq!(s.turn_end.short_streak(), 3, "{recorded}: the act's mark");
        // A turn this loop sees run, and its short end.
        s.turn_end.took();
        let h = with(
            &work0(),
            &turn(
                Some("keep going"),
                &["⏺ Stage 3 of 5: still on it."],
                20,
                T0,
            ),
        );
        let rows = cc(&h, &[], "");
        s.ctl.screens = vec![rows.clone()];
        s.ctl.served = 0;
        let opts = auto(30, None);
        let p = point_at(&s, rows, 101);
        let _ = s
            .turn_end_at_point(&p, Some(Duration::from_secs(20)), &opts)
            .expect("a decision");
        assert_eq!(s.turn_end.short_streak(), want, "recorded={recorded}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A TURN A LOOP READ RUNNING BEFORE IT STOPPED IS A LATER TURN FOR THE LOOP
/// AFTER IT (the verifier of 75b03ac6d, RS9-F1;
/// [`crate::supervise::approvals::RULE_TURN_RAN`]). A GAP HANDOFF: the loop
/// before read a turn running and stopped before the turn ended (an update,
/// a restart: its books end at a point it never read), and a successor
/// attaches a second after that point, on the screen as it stands. Read as
/// the point recorded before it, the turn moved no streak and the back-off
/// counted from the older point: the successor typed up to a rung sooner
/// than the first loop, into a standing five-hour limit and a standing spend
/// limit too — and read that turn's done report as the answer of the check
/// judged before it. Every point of these scenarios is such a handoff, and
/// so is every point a loop that took the streak up from a record stopped
/// before (a chain: attached a second after the point before, stopped at
/// this one). NEGATIVE CONTROL: the same books with no record of the turn
/// read running type sooner.
#[test]
fn a_turn_a_loop_read_running_before_it_stopped_is_a_later_turn_for_the_loop_after_it() {
    let picked: Vec<Scenario> = scenarios()
        .into_iter()
        .filter(|sc| {
            ["R11 five-hour", "RS8-R3", "RS8-F1", "RS8-F2", "RS9-F2"]
                .iter()
                .any(|p| sc.name.starts_with(p))
        })
        .collect();
    assert!(picked.len() >= 12, "{} scenarios", picked.len());
    let gaps = |sc: &Scenario, unrecorded: bool| -> (usize, Vec<String>) {
        let dir = crate::supervise::test_scratch_path("seed-matrix", "gap");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let first = run_first(sc, &dir);
        let start = first.events.first().map_or(sc.t0, |e| e.0);
        let mut cells = 0;
        let mut broken = Vec::new();
        for (t, label, ev, _) in &first.events {
            if !matches!(ev, Ev::Point(_)) || *t <= start {
                continue;
            }
            let books: Vec<(i64, String)> = first
                .stamped
                .iter()
                .filter(|(ms, row)| {
                    *ms < t * 1000
                        && !(unrecorded && row.contains(crate::supervise::approvals::RULE_TURN_RAN))
                })
                .cloned()
                .collect();
            // The journal as the loop before left it: stopped before the
            // point, it said nothing there.
            let journal = before_ms(&first.journal, t * 1000);
            let before: Vec<&Act> = first.acts.iter().filter(|a| a.at < *t).collect();
            let last = before.last().map(|a| a.at);
            let mut follow = |on: Books<'_>, k0: usize, last: Option<i64>, what: &str| {
                let f = run_follower(sc, &first, on, (t + 1, k0, last), None, &dir, true);
                cells += 1;
                if let Some(v) = verdict(first.acts.get(f.at_k), f.diverged.as_ref(), f.due) {
                    broken.push(format!(
                        "{v}: {} | {what} stopped before +{} ({label}), attached a second on\n      {}",
                        sc.name,
                        t - sc.t0,
                        f.log.join("\n      ")
                    ));
                }
            };
            follow(
                Books {
                    rows: &books,
                    journal: &journal,
                },
                before.len(),
                last,
                "the first loop",
            );
            // The chain: a loop attached a second after the point before
            // this one, stopped at this one.
            if let Some(&(t1, ..)) = first
                .events
                .iter()
                .rev()
                .find(|(t1, _, ev, _)| t1 < t && *t1 > start && matches!(ev, Ev::Point(_)))
            {
                let before1: Vec<&Act> = first.acts.iter().filter(|a| a.at < t1 + 1).collect();
                let second = run_follower(
                    sc,
                    &first,
                    first.books(),
                    (t1 + 1, before1.len(), before1.last().map(|a| a.at)),
                    Some(*t),
                    &dir,
                    true,
                );
                if second.diverged.is_none() {
                    let books: Vec<(i64, String)> = second
                        .stamped
                        .iter()
                        .filter(|(_, row)| {
                            !(unrecorded
                                && row.contains(crate::supervise::approvals::RULE_TURN_RAN))
                        })
                        .cloned()
                        .collect();
                    let journal = before_ms(&second.journal, t * 1000);
                    follow(
                        Books {
                            rows: &books,
                            journal: &journal,
                        },
                        second.at_k,
                        second.last,
                        "a loop that followed",
                    );
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        (cells, broken)
    };
    let run = |unrecorded: bool| -> (usize, Vec<String>) {
        std::thread::scope(|scope| {
            let hs: Vec<_> = picked
                .iter()
                .map(|sc| scope.spawn(move || gaps(sc, unrecorded)))
                .collect();
            hs.into_iter()
                .map(|h| h.join().expect("a scenario's gaps"))
                .fold((0, Vec::new()), |(c, mut b), (c1, b1)| {
                    b.extend(b1);
                    (c + c1, b)
                })
        })
    };
    let (cells, broken) = run(false);
    assert!(cells > 40, "{cells} gap cells");
    assert!(
        broken.is_empty(),
        "{} of {cells} gap cells break the rule:\n{}",
        broken.len(),
        broken.join("\n")
    );
    let (_, broken) = run(true);
    assert!(
        broken.iter().any(|b| b.starts_with("SOONER")),
        "CONTROL: books with no record of the turn read running broke nothing"
    );
}

/// THE ESC HOLD A LOOP THAT FOLLOWS TAKES UP COUNTS FROM THE TURN'S END
/// (the verifier of 75b03ac6d, RS9-F2). The worker's FINISHED, then an
/// orchestrator's five-minute turn stopped with Esc (no keystroke the window
/// stamps): the first loop holds two minutes from that point, whose streak
/// stays 0, and types `keep going` then. Every point's time is recorded
/// ([`crate::supervise::approvals::RULE_POINT`]), so a loop attached inside
/// the hold types when the first loop does. NEGATIVE CONTROL: the books
/// without that point's record — the turn read running before it is the
/// last word — and the hold counts from the attach, as much later.
#[test]
fn the_esc_hold_a_loop_that_follows_takes_up_counts_from_the_turn_s_end() {
    let sc = round9_scenarios()
        .into_iter()
        .find(|sc| sc.name == "RS9-F2 FINISHED, an orchestrator's long turn Esc'd (no key)")
        .expect("the scenario");
    let dir = crate::supervise::test_scratch_path("seed-matrix", "esc-hold");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let first = run_first(&sc, &dir);
    let esc = sc.t0 + 25 * MIN;
    let act = first.acts.first().expect("the first loop's act").at;
    assert_eq!(
        act,
        esc + 2 * MIN,
        "the first loop's hold: {:?}",
        first.acts
    );
    let typed_at = |h: i64, unrecorded: bool| {
        let books: Vec<(i64, String)> = first
            .stamped
            .iter()
            .filter(|(ms, row)| {
                !(unrecorded
                    && *ms == esc * 1000
                    && row.contains(crate::supervise::approvals::RULE_POINT))
            })
            .cloned()
            .collect();
        // Its first act, on the test's clock.
        let on = Books {
            rows: &books,
            journal: &first.journal,
        };
        run_follower(&sc, &first, on, (h, 0, None), None, &dir, true)
            .stamped
            .iter()
            .find(|(ms, row)| *ms >= h * 1000 && row.contains("\"decision\":\"typed\""))
            .map(|(ms, _)| ms / 1000)
    };
    for h in [esc + 1, esc + 60, act - 1] {
        assert_eq!(typed_at(h, false), Some(act), "attached at +{}", h - sc.t0);
    }
    let late = typed_at(act - 1, true).expect("CONTROL: an act");
    assert!(late > act + 60, "CONTROL: typed at +{}", late - sc.t0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A CUT OF THE LEDGER KEEPS A LANDING SPENT FOR A LOOP THAT FOLLOWS (the
/// verifier of 75b03ac6d, RS9-R1; [`crate::supervise::approvals::bound`]).
/// Land: the land instruction and two nudges, each answered with progress —
/// the landing spent, nothing more typed — then 600 wakes five minutes
/// apart, each a 20 s short turn: a turn read running and its point, each
/// recorded. On a tab's ledger of older approvals past the bound, the next
/// loop's start cuts it, and that loop types nothing, as the first loop
/// types nothing. NEGATIVE CONTROL: the plain cut keeps the newest rows
/// whole, every one a wake's, and the loop that follows types the land
/// instruction anew.
#[test]
fn a_cut_of_the_ledger_keeps_a_landing_spent_for_a_loop_that_follows() {
    use crate::supervise::approvals::{self, Outcome, Row};
    let dir = crate::supervise::test_scratch_path("seed-matrix", "cut-land");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let w0 = work0();
    let p0 = cc(
        &with(
            &w0,
            &turn(None, &["⏺ Stage 3 of 5 is in."], 0, T0 + 2 * MIN),
        ),
        &[],
        "",
    );
    let mut beats = vec![
        beat(
            At::Start(0),
            "the worker's turn",
            Ev::Busy(live(&w0, "carry on with the task")),
        ),
        beat(At::Start(2 * MIN), "the worker's progress", Ev::Point(p0)),
    ];
    let mut h = w0.clone();
    for k in 0..3 {
        beats.push(taken(k, &h, "(land)"));
        h = with(
            &h,
            &turn(Some("(land)"), &["⏺ Stage 4 of 5 is in."], 10 * MIN, 0),
        );
        beats.push(beat(
            At::Act(k, 10 * MIN),
            "the land act's turn ends: progress",
            Ev::Point(cc(&h, &[], "")),
        ));
    }
    for i in 1..=600 {
        let at = 10 * MIN + 5 * MIN * i;
        beats.push(beat(
            At::Act(2, at - 20),
            "a wake runs",
            Ev::Busy(live(&h, "(background task finished)")),
        ));
        let woke = with(
            &h,
            &turn(
                Some("(background task finished)"),
                &["⏺ The build is still going."],
                20,
                0,
            ),
        );
        beats.push(beat(
            At::Act(2, at),
            "the wake's short turn ends",
            Ev::Point(cc(&woke, &[], "")),
        ));
    }
    let sc = Scenario {
        name: "a landing spent, then wakes".to_string(),
        t0: T0,
        policy: land_policy(),
        host: None,
        beats,
        tail_s: 3 * HOUR,
        take: false,
        person_on_screen: false,
    };
    let first = run_first(&sc, &dir);
    assert_eq!(first.acts.len(), 3, "the landing spent: {:?}", first.acts);
    // A turn read running is recorded after a point, not after an act.
    let ran = first
        .stamped
        .iter()
        .filter(|(_, row)| row.contains(approvals::RULE_TURN_RAN))
        .count();
    assert_eq!(ran, 600, "the wakes' turns read running");
    let h = first.end - 2 * HOUR;
    let older = "cd /Users//someone/Developer/project && cargo test -p project-core --lib -- parser::tests 2>&1 | tail -40";
    let mut rows: Vec<(i64, String)> = (0..2400)
        .map(|i| {
            let ts = (T0 - 10 * 86_400 + i * 60) * 1000;
            let row = Row {
                rule_id: "allow-once@v1",
                outcome: Outcome::Approved,
                command: older,
                reason: "the safe rules proved the command reads and runs tests",
                box_seq: 1,
            };
            (ts, row.to_json(ts, Some(SID)))
        })
        .collect();
    rows.extend(
        first
            .stamped
            .iter()
            .filter(|(ms, _)| *ms < h * 1000)
            .cloned(),
    );
    let after_cut = |plain: bool| {
        let path = dir.join(format!("cut-{plain}.jsonl"));
        let body: String = rows.iter().map(|(_, l)| format!("{l}\n")).collect();
        std::fs::write(&path, body).expect("the books");
        assert!(std::fs::metadata(&path).unwrap().len() > approvals::MAX_BYTES);
        if plain {
            crate::harness::cli::bound_ledger(&path, approvals::MAX_BYTES, approvals::KEEP_ROWS)
                .expect("cut");
        } else {
            approvals::bound(&path);
        }
        let kept: Vec<(i64, String)> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|l| {
                let ms = l
                    .strip_prefix("{\"ts\":")
                    .and_then(|r| r.split(',').next())
                    .and_then(|d| d.parse().ok())
                    .unwrap_or(0);
                (ms, l.to_string())
            })
            .collect();
        // The journal beside it is the first loop's, within its own bound.
        let on = Books {
            rows: &kept,
            journal: &first.journal,
        };
        let f = run_follower(
            &sc,
            &first,
            on,
            (h, 3, first.acts.last().map(|a| a.at)),
            None,
            &dir,
            true,
        );
        verdict(first.acts.get(f.at_k), f.diverged.as_ref(), f.due)
    };
    assert_eq!(after_cut(false), None, "the cut keeps the landing spent");
    assert_eq!(
        after_cut(true),
        Some("TYPES-WHERE-FIRST-DOES-NOT"),
        "CONTROL: the plain cut"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
