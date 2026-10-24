// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance tests binding `OccupancyDoor` to `occupancy::decide`, `write_held`,
//! `HoldClock`, and `Books`, and, in its stuck runs (`Key = 1`), to the stuck fact's door:
//! `FactsState::observe`/`pending` on a facts reading and `stuck_at_break` under the owner's
//! `[harness] stuck_break_note` grant; and, in its hold-rule runs, `age` and `esc` to the K clock
//! (`DueClock`) the loop feeds after the decider, whose `sample` its `Sample` and `SampleLast`
//! drive in their own evidence windows.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use aterm_agent::harness::disk::Level;
use aterm_agent::supervise::facts::{EpisodeKey, FactKind, FactsReading, FactsState, SeedOutcome};
use aterm_agent::supervise::occupancy::due::DueClock;
use aterm_agent::supervise::occupancy::fault::{self, Fault};
use aterm_agent::supervise::occupancy::reading::LOOK_EVERY_MS;
use aterm_agent::supervise::occupancy::rules::{
    HOLD_ROOM_AFTER_MS, HOLD_ROOM_APART_MS, SESSION_PACE_MS, WAIT_NUDGE_AFTER_MS, episode,
};
use aterm_agent::supervise::occupancy::{
    AtWrite, Books, Guards, HoldClock, HoldEvent, HoldSeen, OccupancyReading, RuleId, SlotsSeen,
    StuckVerdict, Verdict, decide, stuck_at_break, write_held,
};
use aterm_agent::supervise::{Autopilot, BackgroundCall, Guard, SupervisorConfig};
use aterm_models_harness::occupancy_door_model;
use aterm_spec::derive::Model;
use aterm_spec::xref::StepEvidence;

use crate::common::cnst;

type State = BTreeMap<&'static str, i64>;

const T0: i64 = 1_000_000_000;

const READ_MS: i64 = 1_000;

/// The real side of a model state, along the first path the walk took to it with as many lines
/// typed before it ([`typed`]).
#[derive(Clone)]
struct Real {
    /// The loop's clock, ms.
    now: i64,
    clock: HoldClock,
    books: Books,
    /// Every fact row booked, oldest first.
    rows: Vec<(EpisodeKey, i64, SeedOutcome)>,
    /// The episode the standing intent row is of.
    intent: Option<EpisodeKey>,
    /// The model's current episode: the one its last row is of, until `Close`.
    episode: Option<EpisodeKey>,
    /// This idle point's decision was fed to the clock already.
    pointed: bool,
    /// The host's answer: the upgrade's notice is due (`IdleHost::upgrade_due`).
    upgrade_due: bool,
    /// The host typed its own line at this break (`IdleHost::at_background`).
    host_line: bool,
    /// The stuck fact's book: which no-progress episodes this session was told.
    facts: FactsState,
    /// The hung call's leader's birth: an episode closed in an odd variant's run reopens on a
    /// call born later in the same group.
    birth: u64,
    /// The K clock, fed as the loop's `door_k` feeds it, on its own clock: `look` moves one look
    /// at each `Sample` and `SampleLast` alone, so the hold's and the pace's clocks never age it.
    due: DueClock<EpisodeKey>,
    look: i64,
    /// The episode the K clock was last fed, if any.
    k_key: Option<EpisodeKey>,
}

/// How a walk departs from the committed real code, for the negative controls.
#[derive(Clone, Copy)]
struct Run {
    rule: RuleId,
    /// Which real guard stands for each guard term (cycled).
    variant: usize,
    /// The defect planted, if any.
    fault: Option<Fault>,
    /// The model's `Key`: the owner's `stuck_break_note` grant, on or off.
    key: i64,
    /// The run walks the stuck fact's line (`OpenStuck`, `cond` a facts reading naming the hung
    /// call) instead of the rule's nudge.
    stuck: bool,
}

/// The hung call's group, which every episode of the stuck runs is of.
const PGID: u32 = 27_098;

fn hung(birth: u64) -> BackgroundCall {
    BackgroundCall {
        pid: PGID,
        pgid: PGID,
        birth: Some(birth),
        exe: String::from("cargo"),
        task: Some(String::from("bplle5dxj")),
        quiet_ms: 31 * 60_000,
        wait: None,
    }
}

/// The host's facts reading at the loop's clock: naming the hung call, or fresh and naming none.
fn facts_read(r: &Real, names: bool) -> FactsReading {
    FactsReading {
        as_of_ms: r.now,
        no_progress: if names {
            vec![hung(r.birth)]
        } else {
            Vec::new()
        },
    }
}

/// The owner's grant as the root table makes it, under `key`.
fn grant(key: i64) -> Option<aterm_agent::supervise::config::StuckBreakGrant> {
    let toml = if key == 1 {
        "[harness]\nstuck_break_note = true\n"
    } else {
        "[harness]\n"
    };
    SupervisorConfig::from_aterm_toml(toml)
        .0
        .stuck_break_grant()
}

fn project(m: &Model, r: &Real, rule: RuleId) -> State {
    let held = r.clock.held_ms(r.now).is_some_and(|ms| ms >= age_ms(rule));
    let (age, esc) = r
        .k_key
        .and_then(|key| Some((r.due.looks(&key, r.look)?, r.due.get(&key)?.raised)))
        .map_or((0, false), |(looks, raised)| {
            (looks.min(cnst(m, "K")), raised)
        });
    State::from([
        ("held", i64::from(held)),
        ("owed", i64::from(r.clock.wants_tail())),
        ("age", age),
        ("esc", i64::from(esc)),
    ])
}

fn guards(st: &State, mode: Autopilot, variant: usize) -> Guards {
    let mut g = Guards::clear(mode);
    if st["person"] == 1 {
        match variant % 3 {
            0 => g.person = true,
            1 => g.halted = true,
            _ => g.draft = true,
        }
    }
    if st["task"] == 0 {
        if variant.is_multiple_of(2) {
            g.taskless = true;
        } else {
            g.exchange = true;
        }
    }
    if st["brake"] == 1 {
        match variant % 4 {
            0 => g.brake = Some(Guard::Stall),
            1 => g.memory = true,
            2 => g.reach_down = true,
            _ => g.brake = Some(Guard::Credits),
        }
    }
    if st["untaken"] == 1 && g.brake.is_none() {
        g.brake = Some(Guard::ActUntaken);
    }
    g
}

fn term(g: Guard) -> &'static str {
    match g {
        Guard::Person | Guard::Halted | Guard::Draft | Guard::Managed => "person",
        Guard::Taskless | Guard::Exchange => "task",
        Guard::Stall | Guard::Credits | Guard::Memory | Guard::Reach => "brake",
        Guard::ActUntaken => "untaken",
        Guard::HostWithheld => "brake",
        Guard::UpgradeDue => "upg",
        Guard::HostLine => "lines",
        Guard::Autopilot => "mode",
        other => panic!("no model term for {other:?}"),
    }
}

fn autopilot(m: &Model) -> Autopilot {
    match cnst(m, "Mode") {
        0 => Autopilot::Off,
        1 => Autopilot::Cruise,
        _ => Autopilot::Land,
    }
}

fn board(room: bool) -> OccupancyReading {
    OccupancyReading {
        taken_ms: T0,
        stale: false,
        avail_mib: Some(16 * 1024),
        pressure: None,
        disk: Some(Level::Ok),
        slots: Some(SlotsSeen { usable: 1, n: 2 }),
        slots_free_ms: Some(20 * 60_000),
        room: Some(room),
        held_slot_last_ms: Some(T0),
    }
}

/// What a break's decision is, as the model tells it apart.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Said {
    Quiet,
    Due,
    /// The model term the guard stands for.
    Held(&'static str),
    Paced,
    Other(String),
}

/// What the stuck fact's door says at a break: quiet with no untold episode on a fresh reading,
/// else `stuck_at_break`'s verdict under the run's grant.
fn stuck_said(m: &Model, st: &State, r: &Real, run: Run) -> Said {
    let read = facts_read(r, st["cond"] == 1);
    let mut facts = r.facts.clone();
    facts.observe(Some(&read), r.now);
    if facts.pending(Some(&read), r.now).is_none() {
        return Said::Quiet;
    }
    let g = guards(st, autopilot(m), run.variant);
    let at = AtWrite {
        host_line: r.host_line,
        withheld: false,
        upgrade_due: r.upgrade_due,
    };
    match stuck_at_break(grant(cnst(m, "Key")).as_ref(), &g, at, &r.books, r.now) {
        StuckVerdict::Ungranted => Said::Quiet,
        StuckVerdict::Held(g) => Said::Held(term(g)),
        StuckVerdict::Paced { .. } => Said::Paced,
        StuckVerdict::Due => Said::Due,
    }
}

/// The decider's verdicts at `st` on the real side.
fn verdicts(m: &Model, st: &State, r: &Real, run: Run) -> [(RuleId, Verdict); 2] {
    let reading = match run.rule {
        RuleId::WaitNudge => OccupancyReading::default(),
        RuleId::HoldRoom => board(st["cond"] == 1),
    };
    let hold = r.clock.since_ms().map(|since_ms| HoldSeen {
        since_ms,
        declared: r.clock.declared(),
    });
    let g = guards(st, autopilot(m), run.variant);
    decide(&reading, hold, &r.books, &g, WAIT_NUDGE_AFTER_MS, r.now)
}

/// What the door's write is told at the break: the host's line and the upgrade's notice.
fn at_write(r: &Real) -> AtWrite {
    AtWrite {
        host_line: r.host_line,
        withheld: false,
        upgrade_due: r.upgrade_due,
    }
}

fn decided(m: &Model, st: &State, r: &Real, run: Run) -> Said {
    if run.stuck {
        return stuck_said(m, st, r, run);
    }
    let mut said = Said::Other(String::from("no verdict"));
    for (rule, verdict) in verdicts(m, st, r, run) {
        if rule != run.rule {
            if verdict != Verdict::Quiet {
                return Said::Other(format!("{rule:?} {verdict:?}"));
            }
            continue;
        }
        said = match verdict {
            Verdict::Quiet => Said::Quiet,
            Verdict::Due(_) => {
                write_held(rule, at_write(r)).map_or(Said::Due, |g| Said::Held(term(g)))
            }
            Verdict::Held(g) => Said::Held(term(g)),
            Verdict::Paced { .. } => Said::Paced,
            Verdict::Spent => Said::Other(String::from("spent")),
        };
    }
    said
}

/// The K clock's feed at `st`, as the loop's `door_k` makes it after the decider: the run's
/// episode with the guard holding it (for a due line, the one its write finds; for a paced one,
/// none), or nothing for a quiet or spent line. The stuck fact's line is not the loop's K clock's.
fn k_feed(m: &Model, st: &State, r: &mut Real, run: Run) {
    if run.stuck {
        return;
    }
    let held = verdicts(m, st, r, run)
        .into_iter()
        .find(|(rule, _)| *rule == run.rule)
        .and_then(|(rule, verdict)| match verdict {
            Verdict::Held(g) => Some(Some(g)),
            Verdict::Due(_) => Some(write_held(rule, at_write(r))),
            Verdict::Paced { .. } => Some(None),
            Verdict::Quiet | Verdict::Spent => None,
        });
    let named: Vec<(EpisodeKey, Option<Guard>)> = held
        .filter(|_| r.clock.since_ms().is_some())
        .map(|guard| (episode_now(r, run.rule), guard))
        .into_iter()
        .collect();
    r.k_key = named.first().map(|(key, _)| *key);
    r.due.observe(&named, r.look);
}

/// The walk holds `age` and `esc` to the K clock: in a hold rule's run, committed or with a
/// defect planted in the clock. A defect planted in the decider or the hold shows where the model
/// says through `held` and the verdict; the clock downstream of it would show it again wherever
/// the line it forgets stood due.
fn k_projected(run: Run) -> bool {
    !run.stuck
        && run.fault.is_none_or(|f| {
            matches!(
                f,
                Fault::LateRaise | Fault::KFromLastWord | Fault::KUnseeded
            )
        })
}

/// A person holds the line the K clock was last fed: the real clock never raises under them (the
/// slice-2 plan's S15), which the model's `SampleLast` does not draw.
fn person_holds(r: &Real) -> bool {
    r.k_key
        .and_then(|key| r.due.get(&key))
        .is_some_and(|due| due.last == Some(Guard::Person))
}

fn agrees(m: &Model, st: &State, said: &Said, stuck: bool) -> bool {
    let mut s = st.clone();
    let open = if stuck { "OpenStuck" } else { "OpenNudge" };
    if m.action_enabled(open, &s) {
        m.fire(open, &mut s);
    }
    if s["due"] == 0 || s["cond"] == 0 {
        return *said == Said::Quiet;
    }
    if s["intent"] == 0 {
        assert!(m.fire("Intend", &mut s), "Intend at {s:?}");
    }
    if m.action_enabled("Type", &s) {
        return *said == Said::Due;
    }
    let mode = cnst(m, "Mode");
    let failing: Vec<&str> = [
        ("person", s["person"] == 0),
        ("task", s["task"] == 1),
        ("brake", s["brake"] == 0),
        ("untaken", s["untaken"] == 0),
        ("mode", mode == 1 || (mode == 2 && s["due"] > 1)),
        ("upg", s["due"] == 2 || s["upg"] == 0),
        ("lines", s["lines"] == 0),
    ]
    .into_iter()
    .filter_map(|(name, holds)| (!holds).then_some(name))
    .collect();
    match said {
        Said::Held(t) => failing.contains(t),
        Said::Paced => failing.is_empty() && s["paced"] == 1,
        _ => false,
    }
}

fn realizable(st: &State, action: &str, r: &Real, run: Run) -> bool {
    let rule = run.rule;
    let eager = (st["due"] > 0 && st["intent"] == 0 && st["cond"] == 0)
        || (st["intent"] == 1 && st["cond"] == 0);
    if eager && action != "Lapse" && action != "Skip" {
        return false;
    }
    // A fresh reading that no longer names a told call forgets it at once (`FactsState::observe`):
    // the call is named again only in an episode the model has closed.
    if run.stuck && st["due"] == 0 && st["told"] > 0 && st["cond"] == 0 && action == "Holds" {
        return false;
    }
    match action {
        "SampleLast" if !run.stuck => !person_holds(r),
        // A successor raises a line again at its first look, which a person's hand holds: the
        // Entry is down while they are at the tab.
        "Crash" if !run.stuck && st["esc"] == 1 => !person_holds(r),
        "OpenWake" => false,
        "OpenNudge" => !run.stuck,
        "OpenStuck" => run.stuck,
        "Intend" | "Type" => st["door"] == 1,
        "UpgradeTypes" => st["held"] == 1,
        "HoldAges" => st["told"] == 0 && st["paced"] == 0,
        // A stuck line typed in a young hold: the pace's lapse must not age it unseen.
        "PaceLapses" if run.stuck => {
            st["held"] == 1
                || r.clock
                    .held_ms(r.now + SESSION_PACE_MS)
                    .is_none_or(|ms| ms < age_ms(rule))
        }
        "Holds" => run.stuck || rule == RuleId::HoldRoom,
        "Fails" => run.stuck || rule == RuleId::HoldRoom || st["owed"] == 0,
        // A door's second line is held by the pace its first one set, which the real door says
        // and the model's `lines` term does not tell apart: an episode closes past the door.
        "Close" if run.stuck => st["lines"] == 0,
        "Close" => {
            rule == RuleId::WaitNudge
                || (st["paced"] == 0
                    && st["lines"] == 0
                    && (st["held"] == 1 || r.clock.since_ms().is_none()))
        }
        _ => true,
    }
}

fn age_ms(rule: RuleId) -> i64 {
    match rule {
        RuleId::WaitNudge => WAIT_NUDGE_AFTER_MS,
        RuleId::HoldRoom => HOLD_ROOM_AFTER_MS,
    }
}

fn break_read(r: &mut Real, rule: RuleId) {
    r.now += READ_MS;
    let opened = r.clock.step(HoldEvent::Background {
        since_ms: r.now,
        now_ms: r.now,
        settled: true,
    });
    if r.clock.wants_tail() {
        r.clock.step(HoldEvent::AnswerTail {
            waiting_line: false,
        });
    }
    if opened.is_some() && rule == RuleId::HoldRoom {
        r.clock.step(HoldEvent::HarnessLine);
        r.clock.step(HoldEvent::AnswerTail { waiting_line: true });
    }
}

fn episode_now(r: &Real, rule: RuleId) -> EpisodeKey {
    let since = r.clock.since_ms().expect("an open hold");
    EpisodeKey {
        kind: FactKind::Hold,
        id: episode(rule, since, r.books.this_hold(rule)),
    }
}

fn point(closing: bool, variant: usize) -> HoldEvent {
    let (acts, background) = match (closing, variant % 3) {
        (false, _) => (false, true),
        (true, 0) => (true, false),
        (true, 1) => (true, true),
        (true, _) => (false, false),
    };
    HoldEvent::DecidedPoint { acts, background }
}

/// The evidence windows of an audited walk, or none.
struct Ev(Option<StepEvidence>);

impl Ev {
    fn step<T>(&mut self, action: &'static str, drive: impl FnOnce() -> T) -> T {
        match &mut self.0 {
            Some(ev) => ev.step(action, drive),
            None => drive(),
        }
    }
}

/// The real step for `action` from `st` to `next`; false where the code answers otherwise than
/// the step says. `Sample` and `SampleLast` run the K clock's `sample` in their own evidence
/// window, and nothing else runs in one.
fn mirror(
    m: &Model,
    action: &'static str,
    (st, next): (&State, &State),
    r: &mut Real,
    run: Run,
    ev: &mut Ev,
) -> bool {
    let rule = run.rule;
    if matches!(action, "Break" | "IdleRead" | "Point" | "Turn") {
        r.host_line = false;
    }
    match action {
        "Break" | "IdleRead" => break_read(r, rule),
        "Point" => r.pointed = false,
        "UpgradeDue" => r.upgrade_due = true,
        "UpgradeTypes" => {
            r.upgrade_due = false;
            r.host_line = true;
            r.clock.step(HoldEvent::HarnessLine);
        }
        "Turn" => {
            if st["door"] == 2 && !std::mem::take(&mut r.pointed) {
                r.clock.step(point(st["owed"] == 1, run.variant));
            }
            if st["door"] == 1 && st["owed"] == 1 {
                r.clock.step(HoldEvent::LiveTurn { ours: true });
            }
        }
        "HoldAges" => {
            let since = r.clock.since_ms().expect("a break opened the hold");
            r.now = r.now.max(since + age_ms(rule));
            r.clock.step(HoldEvent::Background {
                since_ms: since,
                now_ms: r.now,
                settled: true,
            });
        }
        "HoldCloses" => {
            if st["door"] == 0 {
                r.clock.step(HoldEvent::LiveTurn { ours: false });
            } else {
                r.clock.step(point(true, run.variant));
                r.pointed = true;
            }
        }
        "Fails" if run.stuck => {}
        "Fails" if rule == RuleId::WaitNudge => {
            r.clock.step(HoldEvent::HarnessLine);
            r.clock.step(HoldEvent::AnswerTail { waiting_line: true });
        }
        "PaceLapses" => r.now += SESSION_PACE_MS,
        "Sample" | "SampleLast" => r.look += LOOK_EVERY_MS,
        "Close" => {
            if rule == RuleId::HoldRoom && !run.stuck {
                r.now += HOLD_ROOM_APART_MS;
            }
            if run.stuck && run.variant % 2 == 1 {
                r.birth += 1;
            }
            r.episode = None;
        }
        "Intend" => {
            let key = if run.stuck {
                EpisodeKey::no_progress(PGID, r.birth)
            } else {
                episode_now(r, rule)
            };
            r.rows.push((key, r.now, SeedOutcome::Intent));
            r.intent = Some(key);
            r.episode = Some(key);
        }
        "Type" => {
            let key = r.intent.take().expect("an intent row before the line");
            if run.stuck {
                r.books.lined(r.now);
                r.facts.told(key, r.now);
            } else {
                r.books.fired(rule, r.now, true);
            }
            r.rows.push((key, r.now, SeedOutcome::Typed));
            r.clock.step(HoldEvent::HarnessLine);
        }
        "Skip" => {
            let key = r.intent.take().expect("an intent row");
            r.rows.push((key, r.now, SeedOutcome::Skipped));
        }
        "Crash" => {
            let key = r.intent.take().expect("an intent row");
            let mut seeded = FactsState::default();
            seeded.seed(&r.rows, r.now);
            if seeded.is_told(key) != (next["told"] > 0) {
                return false;
            }
            if run.stuck {
                r.facts = seeded;
            }
            // The successor's K clock, seeded from the episode's earliest row in the ledger: taken
            // here as the predecessor's own `since` (the loop's tests bind the rows to it).
            let mut successor = DueClock::default();
            if let Some(key) = r.k_key
                && let Some(due) = r.due.get(&key)
            {
                successor.seed(key, due.since_ms);
            }
            r.due = successor;
        }
        _ => {}
    }
    if run.stuck && st["cond"] == 1 && next["cond"] == 0 {
        let gone = facts_read(r, false);
        r.facts.observe(Some(&gone), r.now);
    }
    if let Some(since) = r.clock.since_ms() {
        r.books.hold(since);
    }
    if let Some(key) = r.episode {
        let mut seeded = FactsState::default();
        seeded.seed(&r.rows, i64::MAX);
        if seeded.is_told(key) != (next["told"] > 0) {
            return false;
        }
        if run.stuck && next["cond"] == 1 && r.facts.is_told(key) != (next["told"] > 0) {
            return false;
        }
    }
    k_feed(m, next, r, run);
    let k = cnst(m, "K");
    match action {
        "Sample" | "SampleLast" if !run.stuck => {
            ev.step(action, || r.due.sample(k, r.look));
        }
        // The successor's first look, at the predecessor's last.
        "Crash" if !run.stuck => {
            r.due.sample(k, r.look);
        }
        _ => {}
    }
    true
}

struct Walked {
    states: HashSet<Vec<i64>>,
    off: Vec<State>,
    fired: BTreeMap<&'static str, usize>,
    said: BTreeSet<String>,
}

impl Walked {
    fn check(&mut self, m: &Model, st: &State, r: &Real, stepped: bool, strict: bool, run: Run) {
        let real = project(m, r, run.rule);
        let mut vars = vec!["held", "owed"];
        // A line whose condition failed lapsed at that look on the real side: the model's `Lapse`
        // takes one more step.
        if k_projected(run) && !(st["due"] > 0 && st["cond"] == 0) {
            vars.extend(["age", "esc"]);
        }
        let mut ok = stepped && vars.iter().all(|v| real[v] == st[v]);
        let said = (st["door"] == 1).then(|| decided(m, st, r, run));
        if let Some(said) = &said {
            ok &= agrees(m, st, said, run.stuck);
            self.said.insert(format!("{said:?}"));
        }
        if !ok {
            assert!(
                !strict,
                "the real side disagrees at {st:?} ({:?}, held {:?}): {said:?}",
                run.rule,
                r.clock.held_ms(r.now),
            );
            self.off.push(st.clone());
        }
    }
}

fn typed(r: &Real) -> usize {
    r.rows
        .iter()
        .filter(|(_, _, outcome)| *outcome == SeedOutcome::Typed)
        .count()
        .min(2)
}

fn walk(m: &Model, strict: bool, run: Run, ev: &mut Ev) -> Walked {
    let go = |ev: &mut Ev| {
        let init = Real {
            now: T0,
            clock: HoldClock::default(),
            books: Books::default(),
            rows: Vec::new(),
            intent: None,
            episode: None,
            pointed: false,
            upgrade_due: false,
            host_line: false,
            facts: FactsState::default(),
            birth: 11,
            due: DueClock::default(),
            look: T0,
            k_key: None,
        };
        let mut w = Walked {
            states: HashSet::new(),
            off: Vec::new(),
            fired: BTreeMap::new(),
            said: BTreeSet::new(),
        };
        let mut seen: HashSet<(Vec<i64>, usize)> = HashSet::new();
        w.check(m, &m.init_state(), &init, true, strict, run);
        let mut queue = VecDeque::from([(m.init_state(), init)]);
        while let Some((st, real)) = queue.pop_front() {
            let key: Vec<i64> = st.values().copied().collect();
            if !seen.insert((key.clone(), typed(&real))) {
                continue;
            }
            w.states.insert(key);
            let env = m.eval_env(&st);
            for action in &m.actions {
                let Some(next) = m.successors_in(action, &env, &st).into_iter().next() else {
                    continue;
                };
                if !realizable(&st, action.name, &real, run) {
                    continue;
                }
                let mut r = real.clone();
                let stepped = mirror(m, action.name, (&st, &next), &mut r, run, ev);
                *w.fired.entry(action.name).or_default() += 1;
                w.check(m, &next, &r, stepped, strict, run);
                queue.push_back((next, r));
            }
        }
        w
    };
    match run.fault {
        Some(f) => fault::with(f, || go(ev)),
        None => go(ev),
    }
}

fn walks(runs: &[(i64, Run)], strict: bool) -> Vec<Walked> {
    aterm_spec::check::each(runs, |&(mode, run)| {
        walk(&under(mode, run.key), strict, run, &mut Ev(None))
    })
}

fn under(mode: i64, key: i64) -> Model {
    aterm_spec::interp::with_consts(&occupancy_door_model(), &[("Mode", mode), ("Key", key)])
}

#[test]
fn tier1_the_real_decider_says_due_exactly_where_the_model_types() {
    let mut runs = Vec::new();
    for mode in [1, 2, 0] {
        for rule in RuleId::ALL {
            let variants = if mode == 1 { 0..4 } else { 0..1 };
            for variant in variants {
                runs.push((
                    mode,
                    Run {
                        rule,
                        variant,
                        fault: None,
                        key: 0,
                        stuck: false,
                    },
                ));
            }
        }
        let variants = if mode == 0 { 0..1 } else { 0..4 };
        for variant in variants {
            runs.push((
                mode,
                Run {
                    rule: RuleId::WaitNudge,
                    variant,
                    fault: None,
                    key: 1,
                    stuck: true,
                },
            ));
        }
    }
    for (&(mode, Run { rule, stuck, .. }), w) in runs.iter().zip(walks(&runs, true)) {
        let what = if stuck {
            String::from("the stuck fact")
        } else {
            format!("{rule:?}")
        };
        let mut own = vec![
            "Turn",
            "Break",
            "Point",
            "HoldAges",
            "IdleRead",
            "HoldCloses",
            "PersonIn",
            "PersonOut",
            "TaskEnds",
            "TaskBegins",
            "BrakeOn",
            "BrakeOff",
            "ActOwed",
            "ActTaken",
            "Fails",
            "UpgradeDue",
            "UpgradeTypes",
            if stuck { "OpenStuck" } else { "OpenNudge" },
            "Intend",
            "Skip",
            "Lapse",
            "Crash",
            "Sample",
            "SampleLast",
        ];
        if stuck || rule == RuleId::HoldRoom {
            own.push("Holds");
        }
        let types = mode == 1 || (stuck && mode == 2);
        if types {
            own.extend(["Type", "PaceLapses", "Close"]);
        } else {
            assert!(
                !w.fired.contains_key("Type"),
                "{what} is not Mode = {mode}'s: nothing is typed under it"
            );
        }
        for a in own {
            assert!(
                w.fired.contains_key(a),
                "{what}, Mode = {mode}: {a} never mirrored: {:?}",
                w.fired
            );
        }
        let want: &[&str] = if types {
            &[
                "Quiet",
                "Due",
                "Held(\"person\")",
                "Held(\"task\")",
                "Held(\"brake\")",
                "Held(\"untaken\")",
                "Held(\"upg\")",
                "Held(\"lines\")",
            ]
        } else {
            &["Quiet", "Held(\"mode\")"]
        };
        for said in want {
            assert!(
                w.said.contains(*said),
                "{what}, Mode = {mode}: never said {said}: {:?}",
                w.said
            );
        }
        if stuck && types {
            assert!(w.said.contains("Paced"), "{what}: {:?}", w.said);
        }
        assert!(w.states.len() > 100, "{}", w.states.len());
    }
}

type Shows = fn(&State) -> bool;

#[test]
fn tier1_each_planted_defect_disagrees_where_the_model_says() {
    let controls: [(Fault, Option<RuleId>, Shows, Option<Shows>); 12] = [
        (
            Fault::DedupeByRule,
            Some(RuleId::WaitNudge),
            |st| st["door"] == 1 && st["held"] == 1 && st["told"] == 0,
            None,
        ),
        (Fault::RestartOnIdle, None, |st| st["held"] == 1, None),
        (
            Fault::CloseOnOurTurn,
            None,
            |st| st["held"] == 1,
            Some(|st| st["door"] == 0 && st["owed"] == 1),
        ),
        (
            Fault::CloseAtTheAnswer,
            None,
            |st| st["door"] == 0 && st["held"] == 1 && st["owed"] == 0,
            None,
        ),
        (
            Fault::CloseAtAnyPoint,
            None,
            |st| st["door"] == 0 && st["held"] == 1 && st["owed"] == 0,
            None,
        ),
        (
            Fault::SeedIntentAsTold,
            None,
            |st| st["told"] == 0 && st["due"] > 0,
            Some(|st| st["intent"] == 0),
        ),
        (
            Fault::SeedSkipAsTold,
            None,
            |st| st["told"] == 0 && st["intent"] == 0 && st["due"] > 0,
            None,
        ),
        (
            Fault::NudgeOverNotice,
            None,
            |st| st["door"] == 1 && st["upg"] == 1,
            None,
        ),
        (
            Fault::StackOnHostLine,
            None,
            |st| st["door"] == 1 && st["lines"] == 1,
            None,
        ),
        // The K clock's: the model's `LateRaise`, a raise a look past K; its `RestartClock`, a
        // clock from the rule's last word; and its `LoseDeadline`, a successor's clock that starts
        // at its first look.
        (Fault::LateRaise, None, |st| st["esc"] == 1, None),
        (
            Fault::KFromLastWord,
            None,
            |st| st["due"] > 0 && st["age"] > 0,
            None,
        ),
        (
            Fault::KUnseeded,
            None,
            |st| st["due"] > 0 && st["age"] > 0,
            Some(|st| st["intent"] == 0),
        ),
    ];
    let mut runs = Vec::new();
    let mut checks = Vec::new();
    for (fault, only, shows, among) in controls {
        for rule in RuleId::ALL
            .into_iter()
            .filter(|r| only.is_none_or(|o| o == *r))
        {
            for variant in 0..3 {
                runs.push((
                    1,
                    Run {
                        rule,
                        variant,
                        fault: Some(fault),
                        key: 0,
                        stuck: false,
                    },
                ));
                checks.push((fault, shows, among));
            }
        }
    }
    // The stuck fact's: over the upgrade's notice with the key on, and with the key off judged
    // as if it were on (`Key = 0`, where the model never opens the episode).
    let stuck: [(Fault, i64, Shows); 2] = [
        (Fault::StuckOverNotice, 1, |st| {
            st["door"] == 1 && st["upg"] == 1
        }),
        (Fault::StuckWithoutKey, 0, |st| {
            st["door"] == 1 && st["cond"] == 1
        }),
    ];
    for (fault, key, shows) in stuck {
        for variant in 0..3 {
            runs.push((
                1,
                Run {
                    rule: RuleId::WaitNudge,
                    variant,
                    fault: Some(fault),
                    key,
                    stuck: true,
                },
            ));
            checks.push((fault, shows, None));
        }
    }
    let walked = walks(&runs, false);
    for ((&(_, Run { rule, variant, .. }), (fault, shows, among)), w) in
        runs.iter().zip(checks).zip(walked)
    {
        assert!(
            !w.off.is_empty(),
            "{fault:?} ({rule:?}, variant {variant}) agreed everywhere"
        );
        let elsewhere: Vec<&State> = w.off.iter().filter(|st| !shows(st)).collect();
        assert!(
            elsewhere.is_empty(),
            "{fault:?} ({rule:?}, variant {variant}) disagrees elsewhere: {elsewhere:?}"
        );
        if let Some(among) = among {
            assert!(
                w.off.iter().any(among),
                "{fault:?} ({rule:?}, variant {variant}): {:?}",
                w.off
            );
        }
    }
}

#[test]
fn tier1_the_anchors_name_what_the_code_implements() {
    let anchored: BTreeSet<&str> = aterm_spec::xref::refinements()
        .filter(|r| r.machine == "OccupancyDoor")
        .map(|r| r.action)
        .collect();
    assert_eq!(
        anchored,
        BTreeSet::from([
            "Crash",
            "HoldCloses",
            "IdleRead",
            "Intend",
            "Lapse",
            "Sample",
            "SampleLast",
            "Skip",
            "Type"
        ]),
        "{anchored:?}"
    );
}

#[test]
fn tier1_sample_and_sample_last_enter_the_k_clock_in_their_own_windows() {
    let mut ev = Ev(Some(StepEvidence::new("OccupancyDoor")));
    for rule in RuleId::ALL {
        let run = Run {
            rule,
            variant: 0,
            fault: None,
            key: 0,
            stuck: false,
        };
        walk(&under(1, 0), true, run, &mut ev);
    }
    let audit = ev.0.expect("the audited walk's evidence").audit();
    for action in ["Sample", "SampleLast"] {
        assert!(
            audit.actions.contains(action) && !audit.undriven.contains(&action),
            "{audit:?}"
        );
        assert!(!audit.unwitnessed.contains(&action), "{audit:?}");
        assert!(!audit.blind_steps.contains(&action), "{audit:?}");
    }
    assert!(audit.uninstrumented.is_empty(), "{audit:?}");
    assert!(audit.ambiguous.is_empty(), "{audit:?}");
    assert!(audit.unanchored_steps.is_empty(), "{audit:?}");
    assert!(audit.stray.is_empty(), "{audit:?}");
    assert!(audit.witnessed_steps > 0, "{audit:?}");
}
