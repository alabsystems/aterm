// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance tests binding `OccupancyDoor` to `occupancy::decide`, `write_held`,
//! `HoldClock`, and `Books`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_agent::harness::disk::Level;
use aterm_agent::supervise::facts::{EpisodeKey, FactKind, FactsState, SeedOutcome};
use aterm_agent::supervise::occupancy::fault::{self, Fault};
use aterm_agent::supervise::occupancy::rules::{
    HOLD_ROOM_AFTER_MS, HOLD_ROOM_APART_MS, SESSION_PACE_MS, WAIT_NUDGE_AFTER_MS, episode,
};
use aterm_agent::supervise::occupancy::{
    AtWrite, Books, Guards, HoldClock, HoldEvent, HoldSeen, OccupancyReading, RuleId, SlotsSeen,
    Verdict, decide, write_held,
};
use aterm_agent::supervise::{Autopilot, Guard};
use aterm_models_harness::occupancy_door_model;
use aterm_spec::derive::Model;

use crate::common::cnst;

type State = BTreeMap<&'static str, i64>;

/// The loop's clock at the walk's start, ms.
const T0: i64 = 1_000_000_000;

/// A new break's read comes this long after the last read.
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
}

/// How a walk departs from the committed real code, for the negative controls.
#[derive(Clone, Copy)]
struct Run {
    rule: RuleId,
    /// Which real guard stands for each guard term (cycled).
    variant: usize,
    /// The defect planted, if any.
    fault: Option<Fault>,
}

/// THE PROJECTION the anchors name (`decide`, `HoldClock::step`, `FactsState::seed`): the model
/// variables the real side keeps, `held`, the clock's open hold having held `rule`'s age, and
/// `owed`, the clock awaiting the answer to the harness's line.
fn project(r: &Real, rule: RuleId) -> State {
    let held = r.clock.held_ms(r.now).is_some_and(|ms| ms >= age_ms(rule));
    State::from([
        ("held", i64::from(held)),
        ("owed", i64::from(r.clock.wants_tail())),
    ])
}

/// The real guards the model's terms at `st` stand for, under `mode`.
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

/// The model term a real guard stands for.
fn term(g: Guard) -> &'static str {
    match g {
        // A manager's fabric note holds the door as a person's hand does.
        Guard::Person | Guard::Halted | Guard::Draft | Guard::Managed => "person",
        Guard::Taskless | Guard::Exchange => "task",
        Guard::Stall | Guard::Credits | Guard::Memory | Guard::Reach => "brake",
        Guard::ActUntaken => "untaken",
        // A host's withhold is a brake of the break's own.
        Guard::HostWithheld => "brake",
        Guard::UpgradeDue => "upg",
        Guard::HostLine => "lines",
        Guard::Autopilot => "mode",
        other => panic!("no model term for {other:?}"),
    }
}

/// The autopilot the model's `Mode` names.
fn autopilot(m: &Model) -> Autopilot {
    match cnst(m, "Mode") {
        0 => Autopilot::Off,
        1 => Autopilot::Cruise,
        _ => Autopilot::Land,
    }
}

/// The board as hold-room reads it: every machine term met but the room, which is `room`.
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

/// The real decider's verdict for `run.rule` at `st`'s break, a due line then judged by the
/// write's own holds at the host's state; the other rule must be quiet.
fn decided(m: &Model, st: &State, r: &Real, run: Run) -> Said {
    let reading = match run.rule {
        RuleId::WaitNudge => OccupancyReading::default(),
        RuleId::HoldRoom => board(st["cond"] == 1),
    };
    let hold = r.clock.since_ms().map(|since_ms| HoldSeen {
        since_ms,
        declared: r.clock.declared(),
    });
    let g = guards(st, autopilot(m), run.variant);
    let verdicts = decide(&reading, hold, &r.books, &g, r.now);
    let mut said = Said::Other(String::from("no verdict"));
    for (rule, verdict) in verdicts {
        if rule != run.rule {
            if verdict != Verdict::Quiet {
                return Said::Other(format!("{rule:?} {verdict:?}"));
            }
            continue;
        }
        said = match verdict {
            Verdict::Quiet => Said::Quiet,
            Verdict::Due(_) => {
                let at = AtWrite {
                    host_line: r.host_line,
                    withheld: false,
                    upgrade_due: r.upgrade_due,
                };
                write_held(rule, at).map_or(Said::Due, |g| Said::Held(term(g)))
            }
            Verdict::Held(g) => Said::Held(term(g)),
            Verdict::Paced { .. } => Said::Paced,
            Verdict::Spent => Said::Other(String::from("spent")),
        };
    }
    said
}

/// Whether the decision `said` at `st`'s break is the model's.
fn agrees(m: &Model, st: &State, said: &Said) -> bool {
    let mut s = st.clone();
    if m.action_enabled("OpenNudge", &s) {
        m.fire("OpenNudge", &mut s);
    }
    if s["due"] == 0 || s["cond"] == 0 {
        // Nothing due, or a due line whose condition failed: `Lapse`.
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
        ("mode", mode == 1 || (mode == 2 && s["due"] == 2)),
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

/// Whether the walk follows `action` from `st`: what the real loop can do ([module docs](self)).
fn realizable(st: &State, action: &str, r: &Real, rule: RuleId) -> bool {
    let eager = (st["due"] > 0 && st["intent"] == 0 && st["cond"] == 0)
        || (st["intent"] == 1 && st["cond"] == 0);
    if eager && action != "Lapse" && action != "Skip" {
        return false;
    }
    match action {
        "OpenWake" => false,
        "Intend" | "Type" => st["door"] == 1,
        // The model's `owed = held` keeps no young hold's answer, which the real clock owes.
        "UpgradeTypes" => st["held"] == 1,
        "HoldAges" => st["told"] == 0 && st["paced"] == 0,
        "Holds" => rule == RuleId::HoldRoom,
        "Fails" => rule == RuleId::HoldRoom || st["owed"] == 0,
        "Close" => {
            rule == RuleId::WaitNudge
                || (st["paced"] == 0
                    && st["lines"] == 0
                    && (st["held"] == 1 || r.clock.since_ms().is_none()))
        }
        _ => true,
    }
}

/// The rule's age, ms.
fn age_ms(rule: RuleId) -> i64 {
    match rule {
        RuleId::WaitNudge => WAIT_NUDGE_AFTER_MS,
        RuleId::HoldRoom => HOLD_ROOM_AFTER_MS,
    }
}

/// A settled read of a new break; the clock opens on it where none is open (and, for hold-room,
/// the agent declares the hold at once, so the nudge stays quiet beside it), and the agent's last
/// words are read where it wants them, declaring nothing.
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

/// The episode a line of `rule` would be of now.
fn episode_now(r: &Real, rule: RuleId) -> EpisodeKey {
    let since = r.clock.since_ms().expect("an open hold");
    EpisodeKey {
        kind: FactKind::Hold,
        id: episode(rule, since, r.books.this_hold(rule)),
    }
}

/// A decided point, by `run.variant`: one that would close the hold (it acts, or finds no
/// background work) where `closing`, else one that decides nothing while the work shows.
fn point(closing: bool, variant: usize) -> HoldEvent {
    let (acts, background) = match (closing, variant % 3) {
        (false, _) => (false, true),
        (true, 0) => (true, false),
        (true, 1) => (true, true),
        (true, _) => (false, false),
    };
    HoldEvent::DecidedPoint { acts, background }
}

/// Mirrors `action` (from `st` to `next`) on the real side.
fn mirror(action: &str, st: &State, next: &State, r: &mut Real, run: Run) -> bool {
    let rule = run.rule;
    if matches!(action, "Break" | "IdleRead" | "Point" | "Turn") {
        // A new break, a point or a live turn: the host's line was this break's.
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
                // nothing while the work showed.
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
        "Fails" if rule == RuleId::WaitNudge => {
            r.clock.step(HoldEvent::HarnessLine);
            r.clock.step(HoldEvent::AnswerTail { waiting_line: true });
        }
        "PaceLapses" => r.now += SESSION_PACE_MS,
        "Close" => {
            if rule == RuleId::HoldRoom {
                r.now += HOLD_ROOM_APART_MS;
            }
            r.episode = None;
        }
        "Intend" => {
            let key = episode_now(r, rule);
            r.rows.push((key, r.now, SeedOutcome::Intent));
            r.intent = Some(key);
            r.episode = Some(key);
        }
        "Type" => {
            let key = r.intent.take().expect("an intent row before the line");
            r.books.fired(rule, r.now, true);
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
        }
        _ => {}
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
    }
    true
}

/// The walk's result: every state reached, those where the real side disagreed, how often each
/// action was mirrored, and each decision seen.
struct Walked {
    states: BTreeSet<State>,
    off: Vec<State>,
    fired: BTreeMap<&'static str, usize>,
    said: BTreeSet<String>,
}

impl Walked {
    /// Checks the real side `r` against the model state `st` it reached (by a step that agreed,
    /// `stepped`): the hold's clock, and at a break the decision.
    fn check(&mut self, m: &Model, st: &State, r: &Real, stepped: bool, strict: bool, run: Run) {
        let real = project(r, run.rule);
        let mut ok = stepped && ["held", "owed"].iter().all(|v| real[v] == st[v]);
        let said = (st["door"] == 1).then(|| decided(m, st, r, run));
        if let Some(said) = &said {
            ok &= agrees(m, st, said);
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

/// How many lines the real side has typed, as far as the walk tells histories apart (a later
/// hold's nudge after an earlier one's is another visit than a first nudge).
fn typed(r: &Real) -> usize {
    r.rows
        .iter()
        .filter(|(_, _, outcome)| *outcome == SeedOutcome::Typed)
        .count()
        .min(2)
}

/// Every reachable state of `m` the real loop can reach, with up to two lines typed before it,
/// the real side mirrored along the first path to each; every step the walk follows is checked
/// where it lands, a state seen before included.
fn walk(m: &Model, strict: bool, run: Run) -> Walked {
    let go = || {
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
        };
        let mut w = Walked {
            states: BTreeSet::new(),
            off: Vec::new(),
            fired: BTreeMap::new(),
            said: BTreeSet::new(),
        };
        let mut seen: BTreeSet<(State, usize)> = BTreeSet::new();
        w.check(m, &m.init_state(), &init, true, strict, run);
        let mut queue = VecDeque::from([(m.init_state(), init)]);
        while let Some((st, real)) = queue.pop_front() {
            if !seen.insert((st.clone(), typed(&real))) {
                continue;
            }
            w.states.insert(st.clone());
            for action in &m.actions {
                let mut next = st.clone();
                if !m.fire(action.name, &mut next) || !realizable(&st, action.name, &real, run.rule)
                {
                    continue;
                }
                let mut r = real.clone();
                let stepped = mirror(action.name, &st, &next, &mut r, run);
                *w.fired.entry(action.name).or_default() += 1;
                w.check(m, &next, &r, stepped, strict, run);
                queue.push_back((next, r));
            }
        }
        w
    };
    match run.fault {
        Some(f) => fault::with(f, go),
        None => go(),
    }
}

/// [`walk`] of [`under`]`(mode)` for each `(mode, run)`, on every core (the fault seam is per
/// thread, so each walk plants its own), in `runs`' order.
fn walks(runs: &[(i64, Run)], strict: bool) -> Vec<Walked> {
    aterm_spec::check::each(runs, |&(mode, run)| walk(&under(mode), strict, run))
}

/// The model under autopilot `mode` (0 Off, 1 Cruise, 2 Land).
fn under(mode: i64) -> Model {
    aterm_spec::interp::with_consts(&occupancy_door_model(), &[("Mode", mode)])
}

/// THE DOOR, WALKED: every state the real loop can reach agrees with the real decider, clock,
/// books and seed — for each rule, under each autopilot, with each real guard standing for its
/// term — and every action the walk follows is mirrored.
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
                    },
                ));
            }
        }
    }
    for (&(mode, Run { rule, .. }), w) in runs.iter().zip(walks(&runs, true)) {
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
            "OpenNudge",
            "Intend",
            "Skip",
            "Lapse",
            "Crash",
            "Sample",
            "SampleLast",
        ];
        if rule == RuleId::HoldRoom {
            own.push("Holds");
        }
        if mode == 1 {
            own.extend(["Type", "PaceLapses", "Close"]);
        } else {
            assert!(
                !w.fired.contains_key("Type"),
                "{rule:?} is Cruise's: nothing is typed under Mode = {mode}"
            );
        }
        for a in own {
            assert!(
                w.fired.contains_key(a),
                "{rule:?}, Mode = {mode}: {a} never mirrored: {:?}",
                w.fired
            );
        }
        let want: &[&str] = if mode == 1 {
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
                "{rule:?}, Mode = {mode}: never said {said}: {:?}",
                w.said
            );
        }
        assert!(w.states.len() > 100, "{}", w.states.len());
    }
}

/// Where a planted defect may disagree with the model: a state the walk reached by a step the
/// defect changed.
type Shows = fn(&State) -> bool;

/// NEGATIVE CONTROLS, planted in the real code, each walked for both rules and every point kind:
/// books that dedupe by the rule's name leave a later hold's nudge untold where the model has it
/// due; a clock restarted at an idle read, or one that ends the hold at the harness line's own
/// turn, at the point that ends its answer, or at a point that decides nothing (F5), loses the
/// hold the model keeps; a seed that reads an intent or a skip as told says told where the
/// model's episode is untold; a write that lets a nudge go over the upgrade's notice due, or a
/// second line at the door the host's line went at, says due where the model holds it.
#[test]
fn tier1_each_planted_defect_disagrees_where_the_model_says() {
    let controls: [(Fault, Option<RuleId>, Shows, Option<Shows>); 9] = [
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
                    },
                ));
                checks.push((fault, shows, among));
            }
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
        assert!(
            w.off.iter().all(shows),
            "{fault:?} ({rule:?}, variant {variant}): {:?}",
            w.off
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

/// The anchors this bind stands for are linked into this binary, on the actions the code
/// implements; `Sample` and `SampleLast` (the K Entry, not implemented yet) are not anchored.
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
            "Skip",
            "Type"
        ]),
        "{anchored:?}"
    );
}
