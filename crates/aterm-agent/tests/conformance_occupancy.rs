// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of `OccupancyDoor` to `occupancy::decide`, `write_held`, `HoldClock` and
//! `Books`; in its stuck runs (`Key = 1`), to `FactsState` and `stuck_at_break` under the
//! `[harness] stuck_break_note` grant; in its wake runs (`due = 2`), to the book's head a break
//! tells (`rules::wake_opens`, which `OpenWake` drives in its own evidence window), the wake's
//! episode told per episode (`FactsState`) and the hold a pending wake declares; in its hold-rule
//! runs, `age` and `esc` to the K clock (`DueClock`), whose `sample` its `Sample` and `SampleLast`
//! drive in their own evidence windows; and at each break, the host's own step (`UpgradeTypes`,
//! the upgrade's notice) to `door::host_step_may_go` of the decider's due line, which
//! `UpgradeTypes` drives in its own evidence window; and at an idle point in a wake run (`door =
//! 2`), the wake's fact riding the policy's act to `rules::at_point` on the same guards (the
//! slice-2 plan's 2.9).

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use aterm_agent::harness::disk::Level;
use aterm_agent::supervise::facts::{EpisodeKey, FactKind, FactsReading, FactsState, SeedOutcome};
use aterm_agent::supervise::occupancy::door::host_step_may_go;
use aterm_agent::supervise::occupancy::due::DueClock;
use aterm_agent::supervise::occupancy::fault::{self, Fault};
use aterm_agent::supervise::occupancy::reading::LOOK_EVERY_MS;
use aterm_agent::supervise::occupancy::rules::{
    HOLD_ROOM_AFTER_MS, HOLD_ROOM_APART_MS, SESSION_PACE_MS, WAIT_NUDGE_AFTER_MS, at_point,
    episode, wake_opens,
};
use aterm_agent::supervise::occupancy::wakes_seen::{AtomKind, Declarer, Prio, WakeState};
use aterm_agent::supervise::occupancy::{
    AtWrite, Books, Guards, HoldClock, HoldEvent, HoldSeen, OccupancyReading, RuleId, SlotsSeen,
    StuckVerdict, Verdict, WakeSeen, WakeSight, WakesSeen, decide, stuck_at_break, write_held,
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
    /// The K clock, fed as the loop's `door_k` feeds it. Its clock, `look`, moves only at
    /// `Sample` and `SampleLast`, so the hold's and the pace's clocks never age it.
    due: DueClock<EpisodeKey>,
    look: i64,
    /// The episode the K clock was last fed, if any.
    k_key: Option<EpisodeKey>,
    /// The session's book of wakes as the loop reads it: in a wake run, the episode's wake (its
    /// condition held and its need met where the model's `cond` holds); empty otherwise.
    wakes: WakesSeen,
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
    /// The negative control of the point's judgement: its decider handed no person's guard.
    blind_point: bool,
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

/// Wake `ord` as declared at `T0 + ord`, pending, its need named: the episode a wake run walks.
fn declared(ord: u16) -> WakeSeen {
    WakeSeen {
        ord,
        class: Declarer::Agent,
        prio: Prio::Normal,
        state: WakeState::Pending,
        declared_ms: T0 + i64::from(ord),
        ready_since_ms: None,
        kinds: vec![AtomKind::File],
        need: Some(String::from("sys slots>=1 for=1200000ms")),
        need_slots: true,
        need_met: false,
        await_only: false,
    }
}

/// The book's head as a break reads it at `r`: the wake whose line it tells, if any.
fn sight<'a>(r: &'a Real, told: &'a dyn Fn(u64) -> bool) -> WakeSight<'a> {
    WakeSight {
        seen: &r.wakes,
        wall_ms: r.now,
        told,
        ranked: true,
    }
}

/// The wake episodes `r`'s rows say were typed.
fn wake_told(r: &Real) -> impl Fn(u64) -> bool + '_ {
    |id| {
        r.rows.iter().any(|(key, _, outcome)| {
            key.kind == FactKind::Wake && key.id == id && *outcome == SeedOutcome::Typed
        })
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
fn verdicts(m: &Model, st: &State, r: &Real, run: Run) -> [(RuleId, Verdict); RuleId::COUNT] {
    let reading = match run.rule {
        RuleId::WaitNudge | RuleId::Wake => OccupancyReading::default(),
        RuleId::HoldRoom => board(st["cond"] == 1),
    };
    let hold = r.clock.since_ms().map(|since_ms| HoldSeen {
        since_ms,
        declared: r.clock.declared(),
    });
    let g = guards(st, autopilot(m), run.variant);
    let told = wake_told(r);
    decide(
        &reading,
        hold,
        &r.books,
        &g,
        WAIT_NUDGE_AFTER_MS,
        r.now,
        &sight(r, &told),
    )
}

/// What the door's write is told at the break: the host's line and the upgrade's notice.
fn at_write(r: &Real) -> AtWrite {
    AtWrite {
        host_line: r.host_line,
        withheld: false,
        upgrade_due: r.upgrade_due,
    }
}

/// Whether `st` is an idle point the run's wake may ride the policy's act at (the slice-2 plan's
/// 2.9): a wake run's `door = 2`.
fn rides_at(st: &State, run: Run) -> bool {
    st["door"] == 2 && run.rule == RuleId::Wake && !run.stuck
}

/// What the point's rider decides at `st` on the real side: the book's head due at an idle point
/// (`rules::at_point`) on the break's guards, with no write-time hold of the break's (the host's
/// line and the upgrade's notice are a break's alone, and a wake goes over the notice).
fn point_decided(m: &Model, st: &State, r: &Real, run: Run) -> Said {
    let mut g = guards(st, autopilot(m), run.variant);
    if run.blind_point {
        (g.person, g.halted, g.draft) = (false, false, false);
    }
    let told = wake_told(r);
    match at_point(&r.books, &g, r.now, &sight(r, &told)).0 {
        Verdict::Quiet => Said::Quiet,
        Verdict::Due(_) => Said::Due,
        Verdict::Held(g) => Said::Held(term(g)),
        Verdict::Paced { .. } => Said::Paced,
        other => Said::Other(format!("{other:?}")),
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
            Verdict::Outranked => Said::Other(String::from("outranked")),
        };
    }
    said
}

/// The K clock's feed at `st`, as the loop's `door_k` makes it after the decider: the run's
/// episode with the guard holding it (for a due line, the one its write finds; for a paced one,
/// none), or nothing for a quiet or spent line. The stuck fact's line does not feed it.
fn k_feed(m: &Model, st: &State, r: &mut Real, run: Run) {
    if run.stuck || run.rule == RuleId::Wake {
        return;
    }
    let held = verdicts(m, st, r, run)
        .into_iter()
        .find(|(rule, _)| *rule == run.rule)
        .and_then(|(rule, verdict)| match verdict {
            Verdict::Held(g) => Some(Some(g)),
            Verdict::Due(_) => Some(write_held(rule, at_write(r))),
            Verdict::Paced { .. } | Verdict::Outranked => Some(None),
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

/// Whether the walk holds `age` and `esc` to the K clock: in a hold rule's run, committed or with
/// a defect planted in the clock. A defect in the decider or the hold already shows through `held`
/// and the verdict, so the clock downstream of it is not checked there. A wake's line feeds the
/// window's own wake K clock, never the loop's.
fn k_projected(run: Run) -> bool {
    !run.stuck
        && run.rule != RuleId::Wake
        && run.fault.is_none_or(|f| {
            matches!(
                f,
                Fault::LateRaise | Fault::KFromLastWord | Fault::KUnseeded
            )
        })
}

/// A person holds the line the K clock was last fed: the real clock never raises under them,
/// which the model's `SampleLast` does not draw.
fn person_holds(r: &Real) -> bool {
    r.k_key
        .and_then(|key| r.due.get(&key))
        .is_some_and(|due| due.last == Some(Guard::Person))
}

fn agrees(m: &Model, st: &State, said: &Said, run: Run) -> bool {
    let mut s = st.clone();
    let open = match (run.stuck, run.rule) {
        (true, _) => "OpenStuck",
        (false, RuleId::Wake) => "OpenWake",
        (false, _) => "OpenNudge",
    };
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
        "OpenWake" => !run.stuck && rule == RuleId::Wake,
        "OpenNudge" => !run.stuck && rule != RuleId::Wake,
        "OpenStuck" => run.stuck,
        // At an idle point a wake's fact rides the policy's act; nothing else is typed there.
        "Intend" | "Type" => st["door"] == 1 || rides_at(st, run),
        "UpgradeTypes" => st["held"] == 1,
        // A wake's line is of no hold's age: the hold a wake run opens never ages for it.
        "HoldAges" => rule != RuleId::Wake && st["told"] == 0 && st["paced"] == 0,
        // A stuck line typed in a young hold: the pace's lapse must not age it unseen.
        "PaceLapses" if run.stuck => {
            st["held"] == 1
                || r.clock
                    .held_ms(r.now + SESSION_PACE_MS)
                    .is_none_or(|ms| ms < age_ms(rule))
        }
        "Holds" => run.stuck || rule != RuleId::WaitNudge,
        "Fails" => run.stuck || rule != RuleId::WaitNudge || st["owed"] == 0,
        // A door's second line is held by the pace its first one set, which the real door says
        // and the model's `lines` term does not tell apart: an episode closes past the door.
        "Close" if run.stuck || rule == RuleId::Wake => st["lines"] == 0,
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
        RuleId::Wake => i64::MAX,
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
    // The book names a wake still to be told: the loop's settled read declares the hold.
    if rule == RuleId::Wake && r.wakes.pending() > 0 {
        r.clock.declare();
    }
}

/// The wake the episode is of: the last the book holds.
fn wake_now(r: &mut Real) -> &mut WakeSeen {
    r.wakes
        .wakes
        .last_mut()
        .expect("a wake run's book holds its wake")
}

fn episode_now(r: &Real, rule: RuleId) -> EpisodeKey {
    if rule == RuleId::Wake {
        let wake = r
            .wakes
            .wakes
            .last()
            .expect("a wake run's book holds its wake");
        return EpisodeKey {
            kind: FactKind::Wake,
            id: wake.episode(),
        };
    }
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
            // The loop asks of the decider's due line whether the host's step may go before it
            // (read before the window, which enters `host_step_may_go` alone).
            let due = (!run.stuck && decided(m, st, r, run) == Said::Due).then_some(rule);
            if !ev.step(action, || host_step_may_go(due)) {
                return false;
            }
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
        "Holds" if rule == RuleId::Wake => {
            let now = r.now;
            let wake = wake_now(r);
            if wake.state == WakeState::Pending {
                wake.state = WakeState::Ready;
                wake.ready_since_ms = Some(now);
            }
            wake.need_met = true;
        }
        "Fails" if rule == RuleId::Wake => wake_now(r).need_met = false,
        "OpenWake" => {
            let told = wake_told(r);
            let opened = ev.step(action, || wake_opens(&sight(r, &told), &r.books).cloned());
            if opened.is_none() {
                return false;
            }
        }
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
            if rule == RuleId::Wake && !run.stuck {
                // The told wake leaves the book, and the agent declares its next.
                let next = r.wakes.wakes.last().map_or(1, |w| w.ord + 1);
                r.wakes.wakes = vec![declared(next)];
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
            } else if rule == RuleId::Wake {
                r.books.woke(key.id, r.now);
            } else {
                r.books.fired(rule, r.now, true);
            }
            r.rows.push((key, r.now, SeedOutcome::Typed));
            // A wake's line answers no hold (the model's `owed = held`, a wake's hold never old).
            if rule != RuleId::Wake {
                r.clock.step(HoldEvent::HarnessLine);
            }
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
    /// How many `Type` steps went at an idle point (`door = 2`): a wake's fact riding an act.
    point_types: usize,
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
        let said = if st["door"] == 1 {
            Some(decided(m, st, r, run))
        } else {
            rides_at(st, run).then(|| point_decided(m, st, r, run))
        };
        if let Some(said) = &said {
            ok &= agrees(m, st, said, run);
            self.said.insert(format!("{said:?}"));
        }
        if let Some(said) = said.as_ref().filter(|_| st["door"] == 1) {
            // §2.1's ranks 2 and 3: the host's own step waits for a wake's line the decider says
            // due (the model's `UpgradeTypes` waits for `due = 2`), and goes before any other
            // line. A wake the decider holds lets it go, a step the model's `due = 2` does not
            // take: the loop orders a due line alone, so that state is not judged here.
            let real_due = (!run.stuck && *said == Said::Due).then_some(run.rule);
            let wake_open = st["due"] == 2
                || (run.rule == RuleId::Wake && !run.stuck && m.action_enabled("OpenWake", st));
            if real_due == Some(RuleId::Wake) || !wake_open {
                ok &= host_step_may_go(real_due) == !wake_open;
            }
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
            wakes: if run.rule == RuleId::Wake && !run.stuck {
                WakesSeen {
                    wakes: vec![declared(1)],
                }
            } else {
                WakesSeen::default()
            },
        };
        let mut w = Walked {
            states: HashSet::new(),
            off: Vec::new(),
            fired: BTreeMap::new(),
            said: BTreeSet::new(),
            point_types: 0,
        };
        let mut seen: HashSet<(Vec<i64>, usize)> = HashSet::new();
        w.check(m, &m.init_state(), &init, true, strict, run);
        let mut queue = VecDeque::from([(m.init_state(), init)]);
        while let Some((st, real)) = queue.pop_front() {
            let key = crate::common::key(m, &st);
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
                if action.name == "Type" && st["door"] == 2 {
                    w.point_types += 1;
                }
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
                        blind_point: false,
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
                    blind_point: false,
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
        let wake = !stuck && rule == RuleId::Wake;
        let mut own = vec![
            "Turn",
            "Break",
            "Point",
            "IdleRead",
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
            match (stuck, wake) {
                (true, _) => "OpenStuck",
                (false, true) => "OpenWake",
                (false, false) => "OpenNudge",
            },
            "Intend",
            "Skip",
            "Lapse",
            "Crash",
            "Sample",
            "SampleLast",
        ];
        // A wake's line is of no hold's age: its run's hold never ages, closes or takes the
        // upgrade's notice (which a due wake goes over).
        if !wake {
            own.extend(["HoldAges", "HoldCloses", "UpgradeTypes"]);
        }
        if stuck || rule != RuleId::WaitNudge {
            own.push("Holds");
        }
        let types = mode == 1 || ((stuck || wake) && mode == 2);
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
        let want: &[&str] = if types && wake {
            &[
                "Quiet",
                "Due",
                "Held(\"person\")",
                "Held(\"task\")",
                "Held(\"brake\")",
                "Held(\"untaken\")",
                "Paced",
            ]
        } else if types {
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
        // A wake's fact rides the policy's act at an idle point: the real point's decider types
        // there exactly where the model's `Type` goes at `door = 2`, and nothing else does.
        assert_eq!(
            w.point_types > 0,
            types && wake,
            "{what}, Mode = {mode}: {} types at a point",
            w.point_types
        );
        assert!(w.states.len() > 100, "{}", w.states.len());
    }
}

type Shows = fn(&State) -> bool;

#[test]
fn tier1_each_planted_defect_disagrees_where_the_model_says() {
    let controls: [(Fault, Option<RuleId>, Shows, Option<Shows>); 14] = [
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
        // The wake's: a successor's seed that holds the whole kind told from one wake's typed
        // row, so the next wake's episode reads told at a crash (S3).
        (
            Fault::KindWideWakeSeed,
            Some(RuleId::Wake),
            |st| st["told"] == 0 && st["due"] > 0,
            Some(|st| st["intent"] == 0),
        ),
        // The host's step before a due wake's line (§2.1's ranks 2 and 3 turned): the break the
        // decider says the wake due at, its episode open or opening there.
        (
            Fault::HostBeforeWake,
            Some(RuleId::Wake),
            |st| st["door"] == 1 && st["cond"] == 1 && (st["due"] == 2 || st["due"] == 0),
            Some(|st| st["due"] == 2),
        ),
    ];
    let mut runs = Vec::new();
    let mut checks = Vec::new();
    for (fault, only, shows, among) in controls {
        // A control of the hold rules' walks the hold rules alone; the wake's names its rule.
        for rule in RuleId::ALL
            .into_iter()
            .filter(|r| only.map_or(*r != RuleId::Wake, |o| o == *r))
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
                        blind_point: false,
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
                    blind_point: false,
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
            "OpenWake",
            "Sample",
            "SampleLast",
            "Skip",
            "Type",
            "UpgradeTypes"
        ]),
        "{anchored:?}"
    );
}

#[test]
fn tier1_sample_and_sample_last_enter_the_k_clock_in_their_own_windows() {
    let mut ev = Ev(Some(StepEvidence::new("OccupancyDoor")));
    for rule in RuleId::ALL.into_iter().filter(|r| *r != RuleId::Wake) {
        let run = Run {
            rule,
            variant: 0,
            fault: None,
            key: 0,
            stuck: false,
            blind_point: false,
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

/// `OpenWake` ENTERS THE BOOK'S HEAD IN ITS OWN WINDOW: a wake run's every `OpenWake` step drives
/// `rules::wake_opens` alone, which opens the wake the model opens (the mirror's own check), and
/// no other anchored function is entered there.
#[test]
fn tier1_open_wake_enters_the_books_head_in_its_own_window() {
    let mut ev = Ev(Some(StepEvidence::new("OccupancyDoor")));
    for mode in [1, 2] {
        let run = Run {
            rule: RuleId::Wake,
            variant: 0,
            fault: None,
            key: 0,
            stuck: false,
            blind_point: false,
        };
        walk(&under(mode, 0), true, run, &mut ev);
    }
    let audit = ev.0.expect("the audited walk's evidence").audit();
    assert!(
        audit.actions.contains("OpenWake") && !audit.undriven.contains(&"OpenWake"),
        "{audit:?}"
    );
    assert!(!audit.unwitnessed.contains(&"OpenWake"), "{audit:?}");
    assert!(!audit.blind_steps.contains(&"OpenWake"), "{audit:?}");
    assert!(audit.stray.is_empty(), "{audit:?}");
    assert!(audit.unanchored_steps.is_empty(), "{audit:?}");
    assert!(audit.witnessed_steps > 0, "{audit:?}");
}

/// `UpgradeTypes` ENTERS `host_step_may_go` IN ITS OWN WINDOW: each step the host's own line takes
/// in the hold rules' runs asks the predicate of the break's due line (the decider's, read before
/// the window), and no other anchored function is entered there.
#[test]
fn tier1_upgrade_types_asks_the_host_steps_rank_in_its_own_window() {
    let mut ev = Ev(Some(StepEvidence::new("OccupancyDoor")));
    for rule in RuleId::ALL.into_iter().filter(|r| *r != RuleId::Wake) {
        let run = Run {
            rule,
            variant: 0,
            fault: None,
            key: 0,
            stuck: false,
            blind_point: false,
        };
        walk(&under(1, 0), true, run, &mut ev);
    }
    let audit = ev.0.expect("the audited walk's evidence").audit();
    assert!(
        audit.actions.contains("UpgradeTypes") && !audit.undriven.contains(&"UpgradeTypes"),
        "{audit:?}"
    );
    assert!(!audit.unwitnessed.contains(&"UpgradeTypes"), "{audit:?}");
    assert!(!audit.blind_steps.contains(&"UpgradeTypes"), "{audit:?}");
    assert!(audit.stray.is_empty(), "{audit:?}");
    assert!(audit.unanchored_steps.is_empty(), "{audit:?}");
    assert!(audit.witnessed_steps > 0, "{audit:?}");
}

/// THE POINT'S JUDGEMENT HAS TEETH (the slice-2 plan's 2.9): a point decider handed no person's
/// guard (`Run::blind_point`) says the wake's fact due where the model's `Type` waits for the
/// person at an idle point, and disagrees nowhere else: the walk judges the real point's decider.
#[test]
fn tier1_a_point_blind_to_the_person_disagrees_at_the_point_alone() {
    let runs: Vec<(i64, Run)> = (0..3)
        .map(|variant| {
            (
                1,
                Run {
                    rule: RuleId::Wake,
                    variant,
                    fault: None,
                    key: 0,
                    stuck: false,
                    blind_point: true,
                },
            )
        })
        .collect();
    for ((_, run), w) in runs.iter().zip(walks(&runs, false)) {
        assert!(
            !w.off.is_empty(),
            "variant {}: agreed everywhere",
            run.variant
        );
        let elsewhere: Vec<&State> = w
            .off
            .iter()
            .filter(|st| !(st["door"] == 2 && st["person"] == 1))
            .collect();
        assert!(
            elsewhere.is_empty(),
            "variant {}: {elsewhere:?}",
            run.variant
        );
    }
}
