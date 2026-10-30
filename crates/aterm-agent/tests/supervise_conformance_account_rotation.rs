// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance test for `SupervisorAccountRotation`.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{BuildHasherDefault, DefaultHasher, Hash, Hasher};
use std::num::NonZero;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use aterm_agent::harness::accounts::{
    self, Advice, Canary, Class, Condition, Lost, Me, Mode, Mover, Point, Roster, Row, Sample,
    StayWhy, Strike,
};
use aterm_agent::harness::relaunch;
use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    ACCOUNT_MOVES_PER_WINDOW, AccountAdvice, AccountAnswer, AccountAt, AccountBad, AccountMove,
    AccountPoint, Cleared, Clearing, Composer, RULE_ACCOUNT, RULE_CONTINUE, RULE_LIMIT_RESUME,
    Restart, TurnEndAction, TurnEndReading, TurnEndState, TurnEndTiming, cleared_text,
    decide_turn_end,
};
use aterm_models_harness::supervisor_account_rotation_model;
use aterm_phase::WallKind;
use aterm_spec::derive::Model;
use aterm_spec::interp::Resolved;

use crate::common::cnst;

type State = BTreeMap<&'static str, i64>;

const NOW: i64 = 1_790_000_000;
const MIN: i64 = 60;
const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;

const WINDOW: &str = "five_hour";

const WALL_S: i64 = 4 * HOUR;

const SOON_S: i64 = MIN;

const RESET_STEP_S: i64 = SOON_S + accounts::RESET_GRACE_S + 1;

const HELD_S: i64 = accounts::CANARY_HOLD_S / 2;

const LAPSE_STEP_S: i64 = accounts::CANARY_HOLD_S - HELD_S;

const _: () = assert!(HELD_S + RESET_STEP_S < accounts::CANARY_HOLD_S);

const NOTICE_ROW_S: i64 = MIN / 2;
const CLEARED_S: i64 = MIN / 4;
const _: () = assert!(CLEARED_S < NOTICE_ROW_S);

const GONE: &str = "s-gone";

const CONDS: [&str; 3] = ["d1", "d2", "sh"];
const SESSIONS: [&str; 2] = ["s1", "s2"];
const WAITING: [&str; 2] = ["w1", "w2"];
const MOVES: [&str; 2] = ["Move1", "Move2"];
const LEAVES: [&str; 2] = ["Leave1", "Leave2"];
const WAITS: [&str; 2] = ["Wait1", "Wait2"];
const RESUMES: [&str; 2] = ["Resume1", "Resume2"];
const NOTICES: [&str; 2] = ["n1", "n2"];
const SIDS: [&str; 2] = ["s-one", "s-two"];

const SEEN: [&str; 13] = [
    "d1", "d2", "sh", "act", "run", "prov", "prev", "pp", "pin", "hold", "s1", "s2", "rot",
];

const DEFECTS: [&str; 10] = [
    "RotatesIntoALimited",
    "WallUnread",
    "SharedFirst",
    "LowFirst",
    "FollowsUnproven",
    "WaitsOnTheWall",
    "MovesEarlyUnderAPin",
    "RotatesPastTheBreaker",
    "WaitsAtTheLiftedWall",
    "CarriesOnAfterAnEarlyMove",
];

struct World {
    roster: Roster,
    enrolled: BTreeSet<String>,
    /// The label of the model's account 1, 2 and 3.
    labels: [&'static str; 3],
    /// Days until each account's week resets, where the samples carry one.
    weeks: Option<[i64; 3]>,
    /// How a lapsed hold is read: the canary's record still there, ten minutes old
    /// (`true`), or none, its session forgotten (`false`).
    silent_record: bool,
}

impl World {
    fn of(labels: [&'static str; 3], weeks: Option<[i64; 3]>, silent_record: bool) -> World {
        let row = |label: &str, class: Class, dir: Option<&str>| Row {
            label: label.to_string(),
            class,
            dir: dir.map(str::to_string),
            ..Row::default()
        };
        let roster = Roster {
            rows: vec![
                row("main", Class::Dedicated, None),
                row("work-2", Class::Dedicated, Some("/logins/work-2")),
                row("pool-a", Class::Shared, Some("/logins/pool-a")),
            ],
        };
        let enrolled = roster.rows.iter().map(|r| r.label.clone()).collect();
        World {
            roster,
            enrolled,
            labels,
            weeks,
            silent_record,
        }
    }

    fn by_label() -> World {
        World::of(["main", "work-2", "pool-a"], None, true)
    }

    fn by_week() -> World {
        World::of(["work-2", "main", "pool-a"], Some([2, 5, 1]), false)
    }

    fn label(&self, account: i64) -> Option<&'static str> {
        usize::try_from(account - 1)
            .ok()
            .and_then(|k| self.labels.get(k).copied())
    }

    fn named(&self, account: i64) -> &'static str {
        self.label(account)
            .unwrap_or_else(|| panic!("no account {account}"))
    }

    fn account(&self, label: Option<&str>) -> i64 {
        match label {
            None => 0,
            Some(label) => {
                let k = self
                    .labels
                    .iter()
                    .position(|l| *l == label)
                    .unwrap_or_else(|| panic!("no row {label}"));
                i64::try_from(k).unwrap() + 1
            }
        }
    }

    fn row(&self, label: &str) -> &Row {
        self.roster
            .row(label)
            .unwrap_or_else(|| panic!("no row {label}"))
    }

    fn bound(&self) -> i64 {
        i64::try_from(self.roster.rows.len()).unwrap() * 2
    }
}

fn spent_before(w: &World, m: &Model) -> i64 {
    let before = w.bound() - cnst(m, "Budget");
    assert!(before >= 0, "the model's Budget is past the real bound");
    before
}

fn sample(w: &World, account: i64, low: bool, window_end: i64, at: i64) -> Sample {
    let k = usize::try_from(account - 1).unwrap();
    Sample {
        session_pct: Some(if low { 95 } else { 10 }),
        session_reset: Some(window_end),
        week_pct: Some(40),
        week_reset: w.weeks.map(|days| NOW + days[k] * DAY),
        at,
        by: String::new(),
        ..Sample::default()
    }
}

fn project(w: &World, m: &Model, st: &State, soon: Option<i64>) -> accounts::State {
    let mut real = accounts::State {
        machine: "0a0b".to_string(),
        ..accounts::State::default()
    };
    for (var, account) in CONDS.iter().zip(1..) {
        let name = w.named(account);
        let near = soon == Some(account);
        let cond = st[var];
        let occupied = SESSIONS.iter().any(|s| st[s] == account);
        if cond == 1 || w.weeks.is_some() || (cond == 0 && occupied) {
            let window_end = NOW + if near { SOON_S } else { WALL_S };
            let read = sample(w, account, cond == 1, window_end, NOW - MIN);
            assert_eq!(accounts::note_sample(&mut real, name, read, NOW).len(), 1);
        }
        if cond == 2 {
            let until = NOW + if near { SOON_S } else { WALL_S };
            let wrote = accounts::note_wall(
                &mut real,
                name,
                WINDOW,
                Some(until),
                NOW - NOTICE_ROW_S,
                NOW,
            );
            assert_eq!(wrote.len(), 1);
        }
    }
    for (var, notice) in SESSIONS.iter().zip(NOTICES) {
        if st[notice] == 1 {
            let name = w.named(st[var]).to_string();
            real.accounts.entry(name).or_default().cleared_at = Some(NOW - CLEARED_S);
        }
    }
    real.active = w.label(st["act"]).map(str::to_string);
    real.previous = w.label(st["prev"]).map(str::to_string);
    real.previous_proven = st["pp"] == 1;
    real.proven = st["prov"] == 1;
    real.pinned = st["pin"] != 0;
    if st["act"] != 0 && st["prov"] == 0 {
        let sid = SIDS
            .iter()
            .zip(SESSIONS)
            .find(|(_, var)| st[var] == st["act"])
            .map_or(GONE, |(sid, _)| sid);
        let held = if st["hold"] == 1 {
            Some(HELD_S)
        } else {
            w.silent_record.then_some(accounts::CANARY_HOLD_S)
        };
        real.canary = held.map(|ago| Canary {
            sid: sid.to_string(),
            at: NOW - ago,
            took: false,
        });
    }
    let rotations = usize::try_from(spent_before(w, m) + st["rot"]).unwrap();
    real.rotations = vec![NOW - 10; rotations];
    for (sid, var) in SIDS.iter().zip(SESSIONS) {
        accounts::note_session(&mut real, sid, w.named(st[var]), None, NOW - MIN);
    }
    real
}

/// The [`SEEN`] variables' values, in that order: the real state read back
/// ([`abstraction`]), or a model state's ([`seen`]).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Seen([i64; SEEN.len()]);

fn seen_at(var: &str) -> usize {
    (SEEN.iter().position(|seen| *seen == var))
        .unwrap_or_else(|| panic!("`{var}` is no variable the real state carries"))
}

impl Seen {
    fn insert(&mut self, var: &str, value: i64) {
        self.0[seen_at(var)] = value;
    }
}

impl std::ops::Index<&str> for Seen {
    type Output = i64;

    fn index(&self, var: &str) -> &i64 {
        &self.0[seen_at(var)]
    }
}

impl std::fmt::Debug for Seen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut by_name: Vec<_> = SEEN.iter().zip(self.0).collect();
        by_name.sort_unstable();
        f.debug_map().entries(by_name).finish()
    }
}

fn abstraction(w: &World, m: &Model, real: &accounts::State, now: i64) -> Seen {
    let mut out = Seen([0; SEEN.len()]);
    for (var, account) in CONDS.iter().zip(1..) {
        let cond = accounts::condition(w.row(w.named(account)), real, true, now);
        let value = match cond {
            Condition::Usable | Condition::Unseen => 0,
            Condition::Low => 1,
            Condition::Limited { .. } => 2,
            other => panic!("a condition the model has no value for: {other:?}"),
        };
        out.insert(var, value);
    }
    let active = real.active.as_deref();
    let act = w.account(active);
    out.insert("act", act);
    let reading = match active.map(|label| accounts::condition(w.row(label), real, true, now)) {
        Some(cond) if cond.fresh() => 2,
        Some(Condition::Low) => 1,
        _ => 0,
    };
    out.insert("run", reading);
    out.insert("prov", i64::from(real.proven));
    let unproven = act != 0 && !real.proven;
    let previous = if unproven {
        w.account(real.previous.as_deref())
    } else {
        0
    };
    out.insert("prev", previous);
    out.insert("pp", i64::from(previous != 0 && real.previous_proven));
    out.insert("pin", if real.pinned { act } else { 0 });
    out.insert("hold", i64::from(real.canary_holds(now)));
    for (sid, var) in SIDS.iter().zip(SESSIONS) {
        out.insert(var, w.account(Some(real.sessions[*sid].account.as_str())));
    }
    let recent = real
        .rotations
        .iter()
        .filter(|t| now - **t < accounts::ROTATION_WINDOW_S)
        .count();
    out.insert("rot", i64::try_from(recent).unwrap() - spent_before(w, m));
    out
}

fn seen(st: &State) -> Seen {
    Seen(SEEN.map(|var| st[var]))
}

fn read_is_seen(read: &Seen, key: &Shown) -> bool {
    read.0 == key[..SEEN.len()]
}

const SHOWN: [&str; SEEN.len() + NOTICES.len()] = {
    let mut shown = [""; SEEN.len() + NOTICES.len()];
    let mut k = 0;
    while k < shown.len() {
        shown[k] = if k < SEEN.len() {
            SEEN[k]
        } else {
            NOTICES[k - SEEN.len()]
        };
        k += 1;
    }
    shown
};

type Shown = [i64; SHOWN.len()];

fn shown(st: &State) -> Shown {
    SHOWN.map(|var| st[var])
}

fn shown_state(key: &Shown) -> State {
    SHOWN_SORTED.iter().map(|&k| (SHOWN[k], key[k])).collect()
}

static SHOWN_SORTED: std::sync::LazyLock<[usize; SHOWN.len()]> = std::sync::LazyLock::new(|| {
    let mut order: [usize; SHOWN.len()] = std::array::from_fn(|k| k);
    order.sort_by_key(|&k| SHOWN[k]);
    order
});

fn flat(st: &State) -> Vec<i64> {
    st.values().copied().collect()
}

/// Why a session at its wall is not moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Hold {
    /// An account that has not served a response yet is out, and its
    /// canary's hold stands.
    Canary,
    /// The hour's rotations are spent.
    Breaker,
    /// Every other account is limited: wait for an instant ahead.
    NothingUsable,
}

/// What a session is told at its point, in the model's words.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Verdict {
    /// Relaunch under the model's account `to`: by a rotation, in the place
    /// of a canary that did not answer (`takes`), or following.
    Move {
        to: i64,
        rotates: bool,
        takes: bool,
        ahead: bool,
    },
    /// At its wall, no move.
    Hold(Hold),
    /// At the notice of a wall the owner lifted: continued there, at once.
    Resume,
    /// At a plain turn end, no move.
    Stay,
    /// Advice the model has no word for at this point: a probe, a wall with
    /// no instant to wait for.
    Other(String),
}

/// Which point a session is asked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    /// Its own: its wall where its account is limited, else a plain turn end.
    ItsPoint,
    /// Its own, where the model has it at the notice of a wall the owner lifted
    /// ([`ask_of`]): a wall whose row was written before the owner cleared the
    /// account, which is no longer limited.
    Notice,
    /// A plain turn end, whatever its account: on a limited one, the wall is
    /// one another tab recorded.
    TurnEnd,
}

type Advisor = fn(&World, &accounts::State, usize, Ask, i64) -> (Advice, bool);

fn at_its_wall(w: &World, real: &accounts::State, i: usize, now: i64) -> bool {
    let mine = real.sessions[SIDS[i]].account.as_str();
    matches!(
        accounts::condition(w.row(mine), real, true, now),
        Condition::Limited { .. }
    )
}

fn ask_of(st: &State, i: usize) -> Ask {
    if st[NOTICES[i]] == 1 {
        Ask::Notice
    } else {
        Ask::ItsPoint
    }
}

fn wall_of(w: &World, real: &accounts::State, i: usize, ask: Ask, now: i64) -> (bool, Option<i64>) {
    match ask {
        Ask::Notice => (true, Some(NOW - NOTICE_ROW_S)),
        Ask::ItsPoint => (at_its_wall(w, real, i, now), None),
        Ask::TurnEnd => (false, None),
    }
}

type Asker = fn(&Roster, &accounts::State, &BTreeSet<String>, &Me<'_>, &Point, Mode, i64) -> Advice;

/// `asker` for session `i`, at its wall or at a plain turn end, under `mode`.
#[allow(clippy::too_many_arguments)]
fn asked_at(
    asker: Asker,
    w: &World,
    real: &accounts::State,
    i: usize,
    at_wall: bool,
    written: Option<i64>,
    mode: Mode,
    now: i64,
) -> Advice {
    let mine = real.sessions[SIDS[i]].account.clone();
    let point = if at_wall {
        Point::Wall {
            window: WINDOW.to_string(),
            at: written,
        }
    } else {
        Point::TurnEnd
    };
    let me = Me {
        account: Some(&mine),
        left: None,
        default_home: true,
        stored_login: true,
    };
    asker(&w.roster, real, &w.enrolled, &me, &point, mode, now)
}

fn advise_at(
    w: &World,
    real: &accounts::State,
    i: usize,
    at_wall: bool,
    written: Option<i64>,
    mode: Mode,
    now: i64,
) -> Advice {
    asked_at(accounts::advise, w, real, i, at_wall, written, mode, now)
}

fn advise_past_at(
    w: &World,
    real: &accounts::State,
    i: usize,
    at_wall: bool,
    written: Option<i64>,
    mode: Mode,
    now: i64,
) -> Advice {
    asked_at(
        accounts::advise_past_probe,
        w,
        real,
        i,
        at_wall,
        written,
        mode,
        now,
    )
}

fn advise_under(
    w: &World,
    real: &accounts::State,
    i: usize,
    ask: Ask,
    mode: Mode,
    now: i64,
) -> (Advice, bool) {
    let (at_wall, written) = wall_of(w, real, i, ask, now);
    let advice = advise_at(w, real, i, at_wall, written, mode, now);
    (advice, at_wall)
}

fn advise(w: &World, real: &accounts::State, i: usize, ask: Ask, now: i64) -> (Advice, bool) {
    advise_under(w, real, i, ask, Mode::Early, now)
}

fn todays_rule(w: &World, real: &accounts::State, i: usize, ask: Ask, now: i64) -> (Advice, bool) {
    let (at_wall, _) = wall_of(w, real, i, ask, now);
    let advice = if at_wall {
        Advice::Wait {
            until: now + WALL_S,
        }
    } else {
        Advice::Stay(StayWhy::NothingDue)
    };
    (advice, at_wall)
}

fn verdict(w: &World, (advice, at_wall): &(Advice, bool), now: i64) -> Verdict {
    match advice {
        Advice::Move {
            to,
            ahead,
            rotates,
            takes_canary,
        } => Verdict::Move {
            to: w.account(Some(to.as_str())),
            rotates: *rotates,
            takes: *takes_canary,
            ahead: *ahead,
        },
        Advice::Resume { .. } if *at_wall => Verdict::Resume,
        Advice::Wait { until } if *at_wall && *until > now => Verdict::Hold(Hold::NothingUsable),
        Advice::Stay(StayWhy::Canary) if *at_wall => Verdict::Hold(Hold::Canary),
        Advice::Stay(StayWhy::Breaker { .. }) if *at_wall => Verdict::Hold(Hold::Breaker),
        Advice::Stay(_) if !*at_wall => Verdict::Stay,
        other => Verdict::Other(format!("{other:?}")),
    }
}

fn walled(st: &State, i: usize) -> bool {
    let mine = usize::try_from(st[SESSIONS[i]] - 1).unwrap();
    st[CONDS[mine]] == 2
}

fn freed(st: &State, i: usize) -> State {
    let mut free = st.clone();
    free.insert(WAITING[i], 0);
    if i == 0 {
        free.insert("jobs", 0);
    }
    free
}

trait Rule {
    fn model(&self) -> &Model;

    fn fired(&self, action: &str, st: &State) -> Option<State>;
}

impl Rule for Model {
    fn model(&self) -> &Model {
        self
    }

    fn fired(&self, action: &str, st: &State) -> Option<State> {
        let mut next = st.clone();
        self.fire(action, &mut next).then_some(next)
    }
}

struct Resolving<'m> {
    m: &'m Model,
    at: &'m Resolution,
}

impl Rule for Resolving<'_> {
    fn model(&self) -> &Model {
        self.m
    }

    fn fired(&self, action: &str, st: &State) -> Option<State> {
        let a = (self.m.actions.iter().position(|a| a.name == action))
            .unwrap_or_else(|| panic!("no action `{action}` in model `{}`", self.m.name));
        let r = &self.at.resolved;
        let next = r.successors(a, &flat(st)).into_iter().next()?;
        Some(r.state(&next))
    }
}

fn the_models_move(
    rule: &(impl Rule + ?Sized),
    action: &str,
    free: &State,
    i: usize,
    ahead: bool,
) -> Option<Verdict> {
    let moved = rule.fired(action, free)?;
    let rotates = moved["rot"] != free["rot"];
    Some(Verdict::Move {
        to: moved[SESSIONS[i]],
        rotates,
        takes: !rotates && free["prov"] == 0,
        ahead,
    })
}

fn expected(rule: &(impl Rule + ?Sized), st: &State, i: usize) -> Verdict {
    let m = rule.model();
    let free = freed(st, i);
    if m.action_enabled(RESUMES[i], &free) {
        return Verdict::Resume;
    }
    let at_wall = walled(st, i);
    if let Some(moved) = the_models_move(rule, MOVES[i], &free, i, !at_wall) {
        return moved;
    }
    if !at_wall {
        return Verdict::Stay;
    }
    if !m.action_enabled(WAITS[i], &free) {
        return Verdict::Other("the model has neither a move nor a wait".to_string());
    }
    if st["run"] >= 1 && st["prov"] == 0 {
        return Verdict::Hold(Hold::Canary);
    }
    let mut unspent = free.clone();
    unspent.insert("rot", 0);
    if m.action_enabled(MOVES[i], &unspent) {
        Verdict::Hold(Hold::Breaker)
    } else {
        Verdict::Hold(Hold::NothingUsable)
    }
}

fn expected_leaving(rule: &(impl Rule + ?Sized), st: &State, i: usize) -> Verdict {
    the_models_move(rule, LEAVES[i], &freed(st, i), i, true).unwrap_or(Verdict::Stay)
}

fn the_move(v: &Verdict) -> Option<&Verdict> {
    matches!(v, Verdict::Move { .. }).then_some(v)
}

enum Decision {
    Move {
        to: i64,
        rotates: bool,
        takes: bool,
    },
    Waits,
    /// Continued at the notice of a wall the owner lifted.
    Resumes,
}

fn decision(action: &str, before: &State, after: &State) -> Option<(usize, Ask, Decision)> {
    let moved = |i: usize, ask: Ask, sound: bool| {
        let rotates = after["rot"] != before["rot"];
        let takes = sound && !rotates && before["prov"] == 0;
        Some((
            i,
            ask,
            Decision::Move {
                to: after[SESSIONS[i]],
                rotates,
                takes,
            },
        ))
    };
    match action {
        "Move1" => moved(0, Ask::ItsPoint, true),
        "Move2" => moved(1, Ask::ItsPoint, true),
        "Leave1" => moved(0, Ask::TurnEnd, true),
        "Leave2" => moved(1, Ask::TurnEnd, true),
        "RotatesIntoALimited"
        | "SharedFirst"
        | "LowFirst"
        | "MovesEarlyUnderAPin"
        | "RotatesPastTheBreaker" => moved(0, Ask::ItsPoint, false),
        "FollowsUnproven" => moved(1, Ask::ItsPoint, false),
        "Wait1" | "WaitsOnTheWall" | "WaitsAtTheLiftedWall" => {
            Some((0, Ask::ItsPoint, Decision::Waits))
        }
        "Wait2" => Some((1, Ask::ItsPoint, Decision::Waits)),
        "Resume1" => Some((0, Ask::ItsPoint, Decision::Resumes)),
        "Resume2" => Some((1, Ask::ItsPoint, Decision::Resumes)),
        _ => None,
    }
}

fn agrees(decided: &Decision, real: &Verdict, jobs_hold: bool) -> bool {
    match decided {
        Decision::Move { to, rotates, takes } => matches!(
            real,
            Verdict::Move { to: t, rotates: r, takes: k, .. } if t == to && r == rotates && k == takes
        ),
        Decision::Waits => jobs_hold || matches!(real, Verdict::Hold(_)),
        Decision::Resumes => *real == Verdict::Resume,
    }
}

const REAL_WORK: Duration = Duration::from_secs(30 * 60);

const NOTICE: &str = "You've hit your session limit · resets 3pm";

/// The supervisor's two clocks as one: the `Instant` that is the unix second [`NOW`],
/// read once a test, so every decision is made on a fixed clock.
#[derive(Debug, Clone, Copy)]
struct Clock(Instant);

impl Clock {
    fn start() -> Clock {
        Clock(Instant::now() + Duration::from_secs(10 * 3600))
    }

    fn at(self, unix: i64) -> Instant {
        let ahead = u64::try_from(unix - NOW).expect("a second at or after NOW");
        self.0 + Duration::from_secs(ahead)
    }
}

fn turn_end(worked: Option<Duration>) -> TurnEndReading {
    TurnEndReading {
        phase: Phase::Idle,
        authoritative: true,
        survey: false,
        wall: None,
        wall_message: String::new(),
        reset_at: None,
        jitter: Duration::ZERO,
        resumes_by_itself: false,
        composer: Composer::Empty,
        said_tail: Some("Suites are running.".to_string()),
        said_cut: false,
        opens_cut: false,
        turn_said: None,
        opened_by: None,
        opened_words: None,
        message_drawn: false,
        submits: None,
        inputs_before: None,
        screen_mark: None,
        worked,
        rules: None,
        pending_input: false,
        interrupted: false,
        restartable: true,
        relaunch_off: false,
        resume: None,
        upgrading: false,
        taskless: false,
        finished_record: None,
        left_record: false,
        left_work: None,
        login_back: false,
        auto_continue: None,
        since_limit: None,
        background: false,
        served_in_transcript: false,
        chosen_in_transcript: None,
        conversation: aterm_agent::supervise::policy::turn_end::Conversation::Hostless,
        notice_elsewhere: false,
        reset_clock: None,
        person: None,
        reach: Default::default(),
        program: aterm_phase::Program::Claude,
        seat: Default::default(),
        account: None,
        autopilot: aterm_agent::supervise::Autopilot::Cruise,
        login_label: None,
        login_superseded: false,
        login_notice: None,
        login_screen: None,
        login_interrupted: false,
        login_at_wall: None,
        login_method: None,
        login_shared: None,
        login_moved: false,
        roster: None,
        wall_remembered: false,
        answered: None,
        worked_doubt: Duration::ZERO,
        // A turn of real work ran a tool.
        ran_tools: worked.is_some_and(|w| w >= REAL_WORK),
        tools_unread: false,
        record_turn: None,
        credits: None,
        repo: aterm_agent::harness::repo_work::RepoRead::Clean,
        fact: None,
        wake: None,
    }
}

fn limit(reset: Instant, worked: Option<Duration>) -> TurnEndReading {
    TurnEndReading {
        wall: Some(WallKind::UsageSession),
        wall_message: NOTICE.to_string(),
        reset_at: Some(reset),
        ..turn_end(worked)
    }
}

fn unadvised(reading: &TurnEndReading) -> TurnEndReading {
    TurnEndReading {
        account: None,
        ..reading.clone()
    }
}

/// Which host the session has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Host {
    /// One that keeps the roster: asked about the loop's point, it answers with the
    /// advice as a window's host builds it ([`mapped`]).
    Roster,
    /// Today's host (a negative control): it advises nothing (`IdleHost::account`'s
    /// default).
    NoRoster,
    /// A host that hands the roster's advice over with no opening
    /// ([`AccountAdvice::of_roster`]): a session held at its wall waits out its hold,
    /// or the hour, whole.
    Uncut,
}

/// Where session `i` was asked: its point, the date of the row it stands at
/// where the walk dates one, and the mode.
#[derive(Debug, Clone, Copy)]
struct Place {
    i: usize,
    at_wall: bool,
    written: Option<i64>,
    mode: Mode,
}

fn mapped(
    w: &World,
    real: &accounts::State,
    advice: &Advice,
    place: Place,
    now: i64,
    clock: Clock,
) -> Option<AccountAdvice> {
    let Place {
        i,
        at_wall,
        written,
        mode,
    } = place;
    let opening = accounts::next_opening(&w.roster, real, &w.enrolled, now);
    let past = matches!(advice, Advice::Probe)
        .then(|| advise_past_at(w, real, i, at_wall, written, mode, now));
    AccountAdvice::of_roster_at(
        advice,
        past.as_ref(),
        real,
        opening,
        at_wall,
        now,
        clock.at(now),
    )
}

fn hosted(
    w: &World,
    real: &accounts::State,
    i: usize,
    point: AccountPoint,
    mode: Mode,
    now: i64,
    clock: Clock,
) -> Option<AccountAdvice> {
    let at_wall = point.at_wall();
    let advice = advise_at(w, real, i, at_wall, None, mode, now);
    let place = Place {
        i,
        at_wall,
        written: None,
        mode,
    };
    mapped(w, real, &advice, place, now, clock)
}

/// One decision of the loop's for a session (`turn_end_loop.rs`: `observe`, then
/// `account_advise`, then `decide_turn_end`).
#[derive(Debug, Clone)]
struct Round {
    /// The point the host was asked about; `None`: it was not asked.
    point: Option<AccountPoint>,
    /// The reading the decision was made on, the host's advice on it.
    reading: TurnEndReading,
    /// The decision.
    action: TurnEndAction,
    /// Today's rule: the decision on the same state and reading with no advice.
    today: TurnEndAction,
}

fn round(
    te: &mut TurnEndState,
    mut reading: TurnEndReading,
    cfg: &SupervisorConfig,
    at: Instant,
    host: impl FnOnce(AccountPoint) -> Option<AccountAdvice>,
) -> Round {
    te.observe(&reading, at);
    let point = te.account_point(&reading, cfg);
    reading.account = point.and_then(host);
    let action = decide_turn_end(te, &reading, cfg, at);
    let today = if reading.account.is_none() {
        action.clone()
    } else {
        decide_turn_end(te, &unadvised(&reading), cfg, at)
    };
    Round {
        point,
        reading,
        action,
        today,
    }
}

/// What the real decider asks for a session at one point, in the model's words.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Asked {
    /// The host is asked to relaunch it under the model's account `to` — at
    /// its wall, or ahead of one — under `account-rotate@v1`, and where the
    /// relaunch is not made the point gets exactly today's action.
    Move { to: i64, ahead: bool },
    /// Its wall is waited out: until today's instant, or a sooner one.
    Waits,
    /// The wall's own continuation, typed at once by the limit-resume rule
    /// where today's rule waits: the notice of a wall the owner lifted.
    Resume,
    /// Exactly today's action, at a point that is not waited at.
    Today,
    /// Anything the model has no word for.
    Other(String),
}

fn asked(w: &World, r: &Round) -> Asked {
    match (&r.action, &r.today) {
        (
            TurnEndAction::Restart {
                why: Restart::Account { to, at },
                rule_id,
                otherwise,
            },
            today,
        ) if *rule_id == RULE_ACCOUNT && **otherwise == *today => Asked::Move {
            to: w.account(Some(to.as_str())),
            ahead: *at == AccountAt::Ahead,
        },
        (TurnEndAction::WaitUntil { until, .. }, TurnEndAction::WaitUntil { until: due, .. })
            if until <= due =>
        {
            Asked::Waits
        }
        (TurnEndAction::Type { rule_id, .. }, TurnEndAction::WaitUntil { .. })
            if *rule_id == RULE_LIMIT_RESUME =>
        {
            Asked::Resume
        }
        (action, today) if action == today => Asked::Today,
        (action, _) => Asked::Other(format!("{action:?}")),
    }
}

fn wanted(told: &Verdict) -> Asked {
    match told {
        Verdict::Move { to, ahead, .. } => Asked::Move {
            to: *to,
            ahead: *ahead,
        },
        Verdict::Hold(_) => Asked::Waits,
        Verdict::Resume => Asked::Resume,
        Verdict::Stay => Asked::Today,
        Verdict::Other(what) => Asked::Other(what.clone()),
    }
}

fn the_ask(a: &Asked) -> Option<&Asked> {
    matches!(a, Asked::Move { .. }).then_some(a)
}

fn waits_until(action: &TurnEndAction) -> Option<Instant> {
    match action {
        TurnEndAction::WaitUntil { until, .. } => Some(*until),
        _ => None,
    }
}

struct Decided {
    /// The session's policy state, that one point folded in.
    te: TurnEndState,
    round: Round,
    asked: Asked,
}

/// The real decider for session `i` of `real`, at the point `ask`: a `TurnEndState`
/// that has seen nothing before it; the turn's end as the loop reads it (on its
/// account's limit where the session is at its wall, with the reset on record, or the
/// one a lifted wall's notice still names; else a plain turn end); the host asked as
/// the loop asks it; and `advisor`'s advice handed over as a host hands it.
#[allow(clippy::too_many_arguments)]
fn decide(
    w: &World,
    real: &accounts::State,
    i: usize,
    ask: Ask,
    advisor: Advisor,
    host: Host,
    now: i64,
    clock: Clock,
    cfg: &SupervisorConfig,
) -> Decided {
    let mine = real.sessions[SIDS[i]].account.as_str();
    let (at_wall, written) = wall_of(w, real, i, ask, now);
    let reading = if ask == Ask::Notice {
        limit(clock.at(now + WALL_S), Some(REAL_WORK))
    } else if at_wall {
        let reset = real
            .account(mine)
            .walls
            .get(WINDOW)
            .and_then(|wall| wall.until)
            .unwrap_or_else(|| panic!("no wall on record for {mine}"));
        limit(clock.at(reset), Some(REAL_WORK))
    } else {
        turn_end(Some(REAL_WORK))
    };
    let mut te = TurnEndState::new(TurnEndTiming::default());
    assert_eq!(te.account_answer(&reading), None, "no move to answer");
    let round = round(&mut te, reading, cfg, clock.at(now), |_| match host {
        Host::NoRoster => None,
        Host::Roster => {
            let (advice, wall) = advisor(w, real, i, ask, now);
            let place = Place {
                i,
                at_wall: wall,
                written,
                mode: Mode::Early,
            };
            mapped(w, real, &advice, place, now, clock)
        }
        Host::Uncut => {
            let (advice, wall) = advisor(w, real, i, ask, now);
            AccountAdvice::of_roster(&advice, real, wall, now, clock.at(now))
        }
    });
    let point = if at_wall {
        AccountPoint::Wall {
            kind: WallKind::UsageSession,
        }
    } else {
        AccountPoint::TurnEnd
    };
    assert_eq!(round.point, Some(point), "session {}", i + 1);
    let asked = asked(w, &round);
    Decided { te, round, asked }
}

fn asks(decided: &Decision, real: &Asked, jobs_hold: bool) -> bool {
    match decided {
        Decision::Move { to, .. } => matches!(real, Asked::Move { to: t, .. } if t == to),
        Decision::Waits => jobs_hold || *real == Asked::Waits,
        Decision::Resumes => *real == Asked::Resume,
    }
}

fn the_wait_ends(
    w: &World,
    real: &accounts::State,
    d: &Decided,
    why: Hold,
    clock: Clock,
) -> Result<(), String> {
    let (Some(until), Some(due)) = (waits_until(&d.round.action), waits_until(&d.round.today))
    else {
        return Err(format!("no wait: {:?}", d.round.action));
    };
    let want = match why {
        Hold::Canary => clock.at(NOW + LAPSE_STEP_S),
        Hold::Breaker => {
            let room = accounts::breaker_until(real, &w.roster, NOW)
                .ok_or("the hour is not spent on the real side")?;
            clock.at(room)
        }
        Hold::NothingUsable => due,
    };
    if until == want && until <= due {
        Ok(())
    } else {
        Err(format!(
            "its wait ends {:?} from now, not {:?} ({why:?})",
            until.saturating_duration_since(clock.at(NOW)),
            want.saturating_duration_since(clock.at(NOW)),
        ))
    }
}

fn typed_after_a_move(action: &str) -> Option<(usize, AccountAt)> {
    match action {
        "Resumed1" => Some((0, AccountAt::Wall)),
        "Resumed2" => Some((1, AccountAt::Wall)),
        "CarriesOnAfterAnEarlyMove" => Some((0, AccountAt::Ahead)),
        _ => None,
    }
}

fn a_moves_own_continuation(to: &str, at: AccountAt, now: Instant) -> bool {
    let relaunch_types = !relaunch::carried_quietly(&format!("account:{to}"), false);
    let mut te = TurnEndState::new(TurnEndTiming::default());
    let why = Restart::Account {
        to: to.to_string(),
        at,
    };
    te.restarted(&why, true, &turn_end(None), now);
    relaunch_types || te.account_resume_owed()
}

/// A move made, on the policy's side: the model's `Move…` / `Leave…` of the session
/// whose decision `d` asked for it, recorded as `restart_agent` records an adopted
/// relaunch (`restarted(.., true, ..)`).
#[allow(clippy::too_many_arguments)]
fn a_move_made(
    w: &World,
    i: usize,
    d: &Decided,
    after: &accounts::State,
    want: &Verdict,
    now: i64,
    clock: Clock,
    cfg: &SupervisorConfig,
) -> Result<(), String> {
    let TurnEndAction::Restart {
        why: why @ Restart::Account { to, at: asked_at },
        ..
    } = &d.round.action
    else {
        return Err(format!("no account move was asked: {:?}", d.round.action));
    };
    let at = clock.at(now);
    let mut te = d.te.clone();
    te.restarted(why, true, &d.round.reading, at);
    let made = AccountMove {
        to: to.clone(),
        at,
        wall: *asked_at == AccountAt::Wall,
    };
    if te.account_move() != Some(&made) {
        return Err(format!(
            "the move is not remembered: {:?}",
            te.account_move()
        ));
    }
    if te.account_moves_of(at).0 != 1 {
        return Err("the move is not counted once".to_string());
    }
    if te.account_refused() || te.wall_attempts(WallKind::UsageSession).is_some() {
        return Err("the wall it left still stands for the policy".to_string());
    }
    let prompt = turn_end(None);
    if te.account_answer(&prompt).is_some() {
        return Err("the host is told before any turn ran".to_string());
    }
    te.observe(&prompt, at);
    if te.account_point(&prompt, cfg).is_some() {
        return Err("the host is asked before any turn ran".to_string());
    }
    if !relaunch::carried_quietly(&format!("account:{to}"), false)
        || relaunch::carried_quietly("memory", false)
    {
        return Err("the relaunch of an account move types a continuation".to_string());
    }
    let owed = decide_turn_end(&te, &prompt, cfg, at);
    let at_a_wall = matches!(
        why,
        Restart::Account {
            at: AccountAt::Wall,
            ..
        }
    );
    let rule = if at_a_wall {
        RULE_LIMIT_RESUME
    } else {
        RULE_CONTINUE
    };
    if owed.rule_id() != Some(rule) || te.account_resume_owed() != at_a_wall {
        return Err(format!(
            "at the relaunched prompt (moved at a wall: {at_a_wall}): {owed:?}"
        ));
    }
    if !at_a_wall {
        let mut unmoved = d.te.clone();
        unmoved.observe(&prompt, at);
        let with_no_move = decide_turn_end(&unmoved, &prompt, cfg, at);
        if owed != with_no_move {
            return Err(format!(
                "moved ahead of a wall, the relaunched prompt gets {owed:?}; with no move \
                 {with_no_move:?}"
            ));
        }
    }
    te.acted(&owed, &prompt, at);
    if te.account_resume_owed() {
        return Err("the point's own act did not pay the wall's resume".to_string());
    }
    let ended = turn_end(Some(REAL_WORK));
    if te.account_answer(&ended) != Some(AccountAnswer::Served) {
        return Err(format!(
            "the first turn under the new login is not served: {:?}",
            te.account_answer(&ended)
        ));
    }
    te.account_told(AccountAnswer::Served);
    if te.account_move().is_some() {
        return Err("a served move still stands".to_string());
    }
    let next = round(&mut te, ended, cfg, at, |point| {
        hosted(w, after, i, point, Mode::Early, now, clock)
    });
    if next.point != Some(AccountPoint::TurnEnd) {
        return Err(format!("served, the host is asked {:?}", next.point));
    }
    let (got, want) = (asked(w, &next), wanted(want));
    if the_ask(&got) == the_ask(&want) {
        Ok(())
    } else {
        Err(format!(
            "made and served, it asks {got:?}, the model {want:?}"
        ))
    }
}

/// The owner lifted the wall session `i` stands at (the model's `Use…` of its
/// account, mirrored through the real `pin`: `before` is the real state the wall was
/// on record in, `after` the one the pin left, at `now`).
#[allow(clippy::too_many_arguments)]
fn the_owners_clearing(
    w: &World,
    before: &accounts::State,
    after: &accounts::State,
    i: usize,
    d: &Decided,
    now: i64,
    clock: Clock,
    cfg: &SupervisorConfig,
) -> Result<(), String> {
    let mine = before.sessions[SIDS[i]].account.clone();
    let wall = before
        .account(&mine)
        .walls
        .get(WINDOW)
        .cloned()
        .ok_or_else(|| format!("no wall on record for {mine}"))?;
    let reset = wall.until.ok_or("the wall names no reset")?;
    let me = Me {
        account: Some(&mine),
        left: None,
        default_home: true,
        stored_login: true,
    };
    let asked_at = |written: Option<i64>| {
        let point = Point::Wall {
            window: WINDOW.to_string(),
            at: written,
        };
        accounts::advise(&w.roster, after, &w.enrolled, &me, &point, Mode::Early, now)
    };
    let at = clock.at(now);
    let decided_on = |te: &mut TurnEndState, advice: &Advice, worked: Option<Duration>| {
        round(te, limit(clock.at(reset), worked), cfg, at, |_| {
            let place = Place {
                i,
                at_wall: true,
                written: None,
                mode: Mode::Early,
            };
            mapped(w, after, advice, place, now, clock)
        })
    };

    let advice = asked_at(Some(wall.at));
    if !matches!(advice, Advice::Resume { .. }) {
        return Err(format!("at the row the owner cleared, advised {advice:?}"));
    }
    let mut te = d.te.clone();
    let first = decided_on(&mut te, &advice, None);
    let continued = TurnEndAction::Type {
        text: cleared_text(Cleared::UsageLimit, Clearing::Measured).to_string(),
        rule_id: RULE_LIMIT_RESUME,
    };
    if first.action != continued || waits_until(&first.today).is_none() {
        return Err(format!(
            "the cleared wall is not continued at once: {:?} (today: {:?})",
            first.action, first.today
        ));
    }
    te.acted(&first.action, &first.reading, at);
    let again = decided_on(&mut te, &advice, Some(Duration::from_secs(3)));
    if again.action != again.today || waits_until(&again.action).is_none() {
        return Err(format!(
            "the same clearing is answered twice: {:?}",
            again.action
        ));
    }
    let undated = asked_at(None);
    let rotated = decided_on(&mut d.te.clone(), &undated, None);
    if matches!(undated, Advice::Resume { .. }) || rotated.action == continued {
        return Err(format!(
            "an undated row was read as cleared: {undated:?}, {:?}",
            rotated.action
        ));
    }
    Ok(())
}

/// How many asks the session's own bounds were tried around.
#[derive(Debug, Default, Clone, Copy)]
struct Bounds {
    at_a_wall: usize,
    ahead: usize,
}

fn around_an_ask(
    w: &World,
    st: &State,
    d: &Decided,
    at: Instant,
    cfg: &SupervisorConfig,
    tried: &mut Bounds,
) {
    let TurnEndAction::Restart {
        why: why @ Restart::Account { to, at: asked_at },
        ..
    } = &d.round.action
    else {
        return;
    };
    let (reading, today) = (&d.round.reading, &d.round.today);
    let decide = |te: &TurnEndState, r: &TurnEndReading, t: Instant| decide_turn_end(te, r, cfg, t);
    let window = TurnEndTiming::default().window;

    assert!(!d.te.account_refused(), "{st:?}");
    assert_eq!(decide(&d.te, reading, at), d.round.action, "{st:?}");

    let mut refused = d.te.clone();
    refused.restarted(why, false, reading, at);
    match asked_at {
        AccountAt::Wall => {
            tried.at_a_wall += 1;
            assert!(refused.account_refused(), "{st:?}");
            assert_eq!(refused.account_point(reading, cfg), None, "{st:?}");
            assert_eq!(&decide(&refused, reading, at), today, "{st:?}");
            for on in [MIN, HOUR] {
                let later = at + Duration::from_secs(u64::try_from(on).unwrap());
                let again = TurnEndReading {
                    worked: None,
                    ..reading.clone()
                };
                let mut still = refused.clone();
                assert_eq!(still.account_point(&again, cfg), None, "{st:?}");
                still.observe(&again, later);
                assert!(still.account_refused(), "{on} s on: {st:?}");
                let waits = decide(&still, &again, later);
                assert_eq!(waits, decide(&still, &unadvised(&again), later), "{st:?}");
                assert_eq!(waits_until(&waits), waits_until(today), "{st:?}");
            }
        }
        AccountAt::Ahead => {
            tried.ahead += 1;
            assert!(!refused.account_refused(), "{st:?}");
            assert_eq!(&decide(&refused, reading, at), today, "{st:?}");
            let other = w
                .labels
                .iter()
                .find(|label| **label != to.as_str())
                .expect("three accounts");
            let named = TurnEndReading {
                account: Some(AccountAdvice::Move {
                    to: (*other).to_string(),
                }),
                ..reading.clone()
            };
            assert!(
                matches!(
                    decide(&refused, &named, at),
                    TurnEndAction::Restart {
                        why: Restart::Account { to: t, at: AccountAt::Ahead },
                        ..
                    } if t == *other
                ),
                "{st:?}"
            );
        }
    }

    let ages = [Duration::from_secs(60); ACCOUNT_MOVES_PER_WINDOW];
    let mut spent = d.te.clone();
    spent.seed_account_moves(&ages, at);
    let capped = decide(&spent, reading, at);
    match asked_at {
        AccountAt::Wall => {
            let TurnEndAction::WaitUntil { until, why, .. } = &capped else {
                panic!("the cap does not wait: {capped:?} at {st:?}");
            };
            assert!(why.contains("a breaker"), "{why} at {st:?}");
            let free = at + window - Duration::from_secs(60);
            assert_eq!(*until, free, "{st:?}");
            assert!(waits_until(today).is_some_and(|due| free < due), "{st:?}");
        }
        AccountAt::Ahead => assert_eq!(&capped, today, "{st:?}"),
    }
    let mut room = d.te.clone();
    room.seed_account_moves(&ages[1..], at);
    assert_eq!(decide(&room, reading, at), d.round.action, "{st:?}");
}

fn under_wall_mode(
    w: &World,
    st: &State,
    real: &accounts::State,
    i: usize,
    ask: Ask,
    d: &Decided,
    clock: Clock,
) -> bool {
    let (early, at_wall) = advise(w, real, i, ask, NOW);
    let (walled, _) = advise_under(w, real, i, ask, Mode::Wall, NOW);
    let (_, written) = wall_of(w, real, i, ask, NOW);
    for (mode, plain) in [(Mode::Early, &early), (Mode::Wall, &walled)] {
        assert_ne!(*plain, Advice::Probe, "session {} at {st:?}", i + 1);
        assert_eq!(
            advise_past_at(w, real, i, at_wall, written, mode, NOW),
            *plain,
            "session {} at {st:?}, {mode:?}: past the probe",
            i + 1
        );
    }
    let onto_active = matches!(early, Advice::Move { rotates: false, .. });
    if at_wall || onto_active || walled == early {
        assert_eq!(walled, early, "session {} at {st:?}", i + 1);
        return false;
    }
    assert_eq!(
        walled,
        Advice::Stay(StayWhy::NothingDue),
        "session {} at {st:?}",
        i + 1
    );
    assert_eq!(
        AccountAdvice::of_roster(&walled, real, at_wall, NOW, clock.at(NOW)),
        None,
        "{st:?}"
    );
    match &d.asked {
        Asked::Move { ahead: true, .. } => {
            assert!(
                matches!(early, Advice::Move { rotates: true, .. }),
                "{early:?}"
            );
            true
        }
        Asked::Today => false,
        other => panic!("ahead of a wall the decider asks {other:?} at {st:?}"),
    }
}

fn account_of(action: &str) -> i64 {
    if action.ends_with("D1") {
        1
    } else if action.ends_with("D2") {
        2
    } else if action.ends_with("Sh") {
        3
    } else {
        panic!("no account in `{action}`")
    }
}

fn mirror(
    w: &World,
    action: &str,
    before: &State,
    after: &State,
    real: &mut accounts::State,
    now: i64,
    wall_s: i64,
) -> Result<(), String> {
    let until = Some(now + wall_s);
    match action {
        "JobsStart"
        | "JobsEnd"
        | "Wait1"
        | "Wait2"
        | "Resume1"
        | "Resume2"
        | "Resumed1"
        | "Resumed2"
        | "CarriesOnAfterAnEarlyMove" => {}
        "ResetD1" | "ResetD2" | "ResetSh" | "Lapse" => {}
        "WallD1" | "WallD2" | "WallSh" => {
            let name = w.named(account_of(action));
            if accounts::note_wall(real, name, WINDOW, until, now, now).is_empty() {
                return Err(format!("the wall on {name} was not recorded"));
            }
        }
        "LowD1" | "LowD2" | "LowSh" => {
            let account = account_of(action);
            let read = sample(w, account, true, now + WALL_S, now);
            if accounts::note_sample(real, w.named(account), read, now).is_empty() {
                return Err(format!("the sample on account {account} was not recorded"));
            }
        }
        "UseD1" | "UseD2" | "UseSh" => {
            let name = w.named(account_of(action));
            accounts::pin(real, &w.roster, &w.enrolled, name, now)?;
        }
        "Unpin" => {
            accounts::unpin(real, "auto", now).ok_or("no pin held")?;
        }
        "Served" => {
            let active = real.active.clone().ok_or("no active account")?;
            accounts::prove(real, &active, now).ok_or("nothing was proven")?;
        }
        "Strike" => {
            let active = real.active.clone().ok_or("no active account")?;
            let why = Strike::Wall {
                window: WINDOW.to_string(),
                until,
            };
            let heard = real.sessions_on(&active).first().map_or(GONE, |sid| *sid);
            let heard = heard.to_string();
            if accounts::strike(real, &active, &heard, &why, now).is_empty() {
                return Err("nothing was struck".to_string());
            }
        }
        "Move1" | "Move2" | "Leave1" | "Leave2" => {
            let i = usize::from(action.ends_with('2'));
            let to = w.named(after[SESSIONS[i]]);
            if after["rot"] != before["rot"] {
                let read = real.active.clone();
                let me = w.named(before[SESSIONS[i]]);
                accounts::begin_rotation(
                    real,
                    &w.roster,
                    &w.enrolled,
                    read.as_deref(),
                    Mover {
                        sid: SIDS[i],
                        account: me,
                    },
                    to,
                    now,
                )
                .map_err(|lost| format!("the compare-and-set lost: {lost:?}"))?;
            } else if before["prov"] == 0 {
                let read = real.canary.clone();
                accounts::take_canary(real, read.as_ref(), SIDS[i], now)
                    .ok_or("the canary's place was not taken")?;
            }
            accounts::note_session(real, SIDS[i], to, None, now);
        }
        "WallUnread" => {
            accounts::note_wall(real, w.named(2), WINDOW, until, now, now);
        }
        "RotatesIntoALimited"
        | "SharedFirst"
        | "LowFirst"
        | "FollowsUnproven"
        | "WaitsOnTheWall"
        | "MovesEarlyUnderAPin"
        | "RotatesPastTheBreaker"
        | "WaitsAtTheLiftedWall" => {}
        other => panic!("an action this bind does not mirror: {other}"),
    }
    Ok(())
}

/// One place the real code and the model part.
#[derive(Debug)]
struct Off {
    /// The model state it was found at, shared by every row found there.
    state: Arc<State>,
    /// The model action it was found on; `None` at the state itself.
    action: Option<&'static str>,
    what: String,
}

#[derive(Default)]
struct Walk {
    states: usize,
    /// How many times each model action was taken.
    fired: BTreeMap<&'static str, usize>,
    off: Vec<Off>,
    /// What the real decider asked, over every state and both sessions at
    /// their own points: a move, a wait, today's action.
    moves: usize,
    waits: usize,
    todays: usize,
    /// …and the wall's own continuation, at the notice of one the owner
    /// lifted.
    resumes: usize,
    /// The moves the real decider asked of a session at a plain turn end on a
    /// limited account (`Leave…`).
    leaving_moves: usize,
    /// How many of the model's moves were made on the policy's side.
    made: usize,
    /// The asks the session's own bounds were tried around.
    bounds: Bounds,
    /// The rotations ahead of a wall that `[harness] accounts = "wall"`
    /// does not advise ([`under_wall_mode`]): where the two modes differ.
    wall_withholds: usize,
    /// The sessions with nothing usable whose wait was read a minute from a
    /// window's reset.
    woken: usize,
    /// The sessions held for the canary or the breaker whose wait was read a
    /// minute from a window's reset, and how many of those the same advice
    /// with no opening left asleep past it.
    held_woken: usize,
    held_asleep: usize,
    /// The sessions at the wall of the account the owner pinned.
    resumed: usize,
}

impl Walk {
    fn note(
        &mut self,
        strict: bool,
        same: bool,
        state: &Arc<State>,
        action: Option<&'static str>,
        what: impl FnOnce() -> String,
    ) {
        if same {
            return;
        }
        let what = what();
        assert!(!strict, "{what}\n  at {state:?}\n  on {action:?}");
        self.off.push(Off {
            state: Arc::clone(state),
            action,
            what,
        });
    }

    fn add(&mut self, found: &Walk, strict: bool, state: &Arc<State>) {
        self.moves += found.moves;
        self.waits += found.waits;
        self.todays += found.todays;
        self.resumes += found.resumes;
        self.leaving_moves += found.leaving_moves;
        self.made += found.made;
        self.bounds.at_a_wall += found.bounds.at_a_wall;
        self.bounds.ahead += found.bounds.ahead;
        self.wall_withholds += found.wall_withholds;
        self.woken += found.woken;
        self.held_woken += found.held_woken;
        self.held_asleep += found.held_asleep;
        self.resumed += found.resumed;
        for off in &found.off {
            self.note(strict, false, state, off.action, || off.what.clone());
        }
    }
}

fn around_a_rotation(w: &World, m: &Model, st: &State, base: &accounts::State, i: usize) {
    let (
        Advice::Move {
            to, rotates: true, ..
        },
        _,
    ) = advise(w, base, i, Ask::ItsPoint, NOW)
    else {
        return;
    };
    let read = base.active.clone();
    let me = base.sessions[SIDS[i]].account.clone();
    let commit = |real: &mut accounts::State| {
        let mover = Mover {
            sid: SIDS[i],
            account: &me,
        };
        accounts::begin_rotation(
            real,
            &w.roster,
            &w.enrolled,
            read.as_deref(),
            mover,
            &to,
            NOW,
        )
        .unwrap_or_else(|lost| panic!("the advised rotation lost: {lost:?} at {st:?}"))
    };

    let not_moved = Strike::NotMoved("wait:changed".to_string());
    let mut refused = base.clone();
    commit(&mut refused);
    let committed = refused.clone();
    for other in [SIDS[1 - i], GONE, ""] {
        let rows = accounts::strike(&mut refused, &to, other, &not_moved, NOW);
        assert!(rows.is_empty(), "{other:?}: {rows:?} at {st:?}");
        assert_eq!(refused, committed, "{other:?} at {st:?}");
    }
    let undone = accounts::strike(&mut refused, &to, SIDS[i], &not_moved, NOW);
    assert_eq!(undone.len(), 1, "{st:?}");
    assert_eq!(
        abstraction(w, m, &refused, NOW),
        seen(st),
        "a refused relaunch is no step: {st:?}"
    );
    assert_eq!(&refused, base, "…of the real state either: {st:?}");

    let j = 1 - i;
    let (
        Advice::Move {
            to: theirs,
            rotates: true,
            ..
        },
        _,
    ) = advise(w, base, j, Ask::ItsPoint, NOW)
    else {
        return;
    };
    let mut raced = base.clone();
    commit(&mut raced);
    accounts::note_session(&mut raced, SIDS[i], &to, None, NOW);
    let won = raced.clone();
    let them = base.sessions[SIDS[j]].account.clone();
    let lost = accounts::begin_rotation(
        &mut raced,
        &w.roster,
        &w.enrolled,
        read.as_deref(),
        Mover {
            sid: SIDS[j],
            account: &them,
        },
        &theirs,
        NOW,
    );
    assert_eq!(
        lost,
        Err(Lost::Raced {
            active: Some(to.clone())
        }),
        "{st:?}"
    );
    assert_eq!(raced, won, "a lost compare-and-set writes nothing: {st:?}");
    assert_eq!(
        advise(w, &raced, j, Ask::ItsPoint, NOW).0,
        Advice::Stay(StayWhy::Canary),
        "the loser does not follow an unproven account: {st:?}"
    );
}

fn around_a_take(w: &World, m: &Model, st: &State, base: &accounts::State, i: usize) {
    let taking = |advice: Advice| match advice {
        Advice::Move {
            to,
            takes_canary: true,
            rotates: false,
            ..
        } => Some(to),
        _ => None,
    };
    let Some(to) = taking(advise(w, base, i, Ask::ItsPoint, NOW).0) else {
        return;
    };
    assert_eq!(base.active.as_deref(), Some(to.as_str()), "{st:?}");
    let read = base.canary.clone();

    let mut refused = base.clone();
    accounts::take_canary(&mut refused, read.as_ref(), SIDS[i], NOW)
        .unwrap_or_else(|| panic!("the advised take lost at {st:?}"));
    assert_eq!(refused.canary_since(), Some(NOW), "{st:?}");
    assert_eq!(
        (&refused.active, &refused.rotations),
        (&base.active, &base.rotations),
        "a take rotates nothing: {st:?}"
    );
    let not_moved = Strike::NotMoved("wait:changed".to_string());
    let taken = refused.clone();
    let silent = read.as_ref().map_or(GONE, |c| c.sid.as_str());
    for other in [silent, SIDS[1 - i], ""] {
        let rows = accounts::strike(&mut refused, &to, other, &not_moved, NOW);
        assert!(rows.is_empty(), "{other:?}: {rows:?} at {st:?}");
        assert_eq!(refused, taken, "{other:?} at {st:?}");
    }
    let undone = accounts::strike(&mut refused, &to, SIDS[i], &not_moved, NOW);
    assert_eq!(undone.len(), 1, "{st:?}");
    assert_eq!(
        abstraction(w, m, &refused, NOW),
        seen(st),
        "a refused take is no step: {st:?}"
    );
    assert_eq!(
        taking(advise(w, &refused, i, Ask::ItsPoint, NOW).0),
        Some(to.clone()),
        "the place is open again: {st:?}"
    );

    let j = 1 - i;
    if taking(advise(w, base, j, Ask::ItsPoint, NOW).0).is_none() {
        return;
    }
    let mut raced = base.clone();
    accounts::take_canary(&mut raced, read.as_ref(), SIDS[i], NOW)
        .unwrap_or_else(|| panic!("the advised take lost at {st:?}"));
    accounts::note_session(&mut raced, SIDS[i], &to, None, NOW);
    let won = raced.clone();
    for late in [0, accounts::CANARY_HOLD_S] {
        assert_eq!(
            accounts::take_canary(&mut raced, read.as_ref(), SIDS[j], NOW + late),
            None,
            "{late} s on, at {st:?}"
        );
        assert_eq!(raced, won, "a lost take writes nothing: {st:?}");
    }
    assert_eq!(
        advise(w, &raced, j, Ask::ItsPoint, NOW).0,
        Advice::Stay(StayWhy::Canary),
        "the loser is held by the session that took the place: {st:?}"
    );
}

struct Lane<'a> {
    w: &'a World,
    m: &'a Model,
    advisor: Advisor,
    host: Host,
    cfg: SupervisorConfig,
    clock: Clock,
}

struct AtState {
    /// The projection, as the real code was handed it.
    st: Arc<State>,
    /// Its own counts and disagreements: the projection read back, and what
    /// the decider asked.
    found: Walk,
    base: accounts::State,
    /// Each session's own point, as the model has it there: its wall, a
    /// plain turn end, or the notice of a wall the owner lifted.
    own: [Ask; 2],
    told: [Verdict; 2],
    /// A session on a limited account, asked at a plain turn end.
    told_leaving: [Verdict; 2],
    decided: [Decided; 2],
    decided_leaving: [Option<Decided>; 2],
}

/// One edge of the walk as the real code is shown it, and what the model says
/// at its two ends: everything [`Lane::at_edge`] reads, so the key of what it
/// finds.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Edge {
    from: Shown,
    action: &'static str,
    to: Shown,
    /// The agent's jobs run at the state the edge leaves, which is the host's
    /// and no [`SHOWN`] variable, and the edge is session 1's decision: a wait
    /// is then the host's refusal of whatever was advised.
    jobs_hold: bool,
    /// What the model tells both sessions at the state the edge leaves, and
    /// at the one it reaches (`None` for a defect: that state is no one's).
    held: [Verdict; 2],
    after: Option<[Verdict; 2]>,
}

impl Lane<'_> {
    fn decide_at(&self, real: &accounts::State, i: usize, ask: Ask, now: i64) -> Decided {
        decide(
            self.w,
            real,
            i,
            ask,
            self.advisor,
            self.host,
            now,
            self.clock,
            &self.cfg,
        )
    }

    fn at_state(&self, key: &Shown) -> AtState {
        let (w, m, advisor) = (self.w, self.m, self.advisor);
        let st = Arc::new(shown_state(key));
        let mut found = Walk::default();
        let base = project(w, m, &st, None);
        found.note(
            false,
            read_is_seen(&abstraction(w, m, &base, NOW), key),
            &st,
            None,
            || "the projection does not read back".to_string(),
        );
        let own = [0, 1].map(|i| ask_of(&st, i));
        let told = [0, 1].map(|i| verdict(w, &advisor(w, &base, i, own[i], NOW), NOW));
        let told_leaving =
            [0, 1].map(|i| verdict(w, &advisor(w, &base, i, Ask::TurnEnd, NOW), NOW));
        let decided = [0, 1].map(|i| self.decide_at(&base, i, own[i], NOW));
        for d in &decided {
            match d.asked {
                Asked::Move { .. } => found.moves += 1,
                Asked::Waits => found.waits += 1,
                Asked::Today => found.todays += 1,
                Asked::Resume => found.resumes += 1,
                Asked::Other(_) => {}
            }
        }
        let decided_leaving =
            [0, 1].map(|i| walled(&st, i).then(|| self.decide_at(&base, i, Ask::TurnEnd, NOW)));
        found.leaving_moves = decided_leaving
            .iter()
            .flatten()
            .filter(|d| matches!(d.asked, Asked::Move { .. }))
            .count();
        AtState {
            st,
            found,
            base,
            own,
            told,
            told_leaving,
            decided,
            decided_leaving,
        }
    }

    fn around(&self, here: &AtState) -> Walk {
        let (w, m, st, base) = (self.w, self.m, &*here.st, &here.base);
        let mut found = Walk::default();
        for i in 0..2 {
            around_a_rotation(w, m, st, base, i);
            around_a_take(w, m, st, base, i);
            let asks = [
                (here.own[i], Some(&here.decided[i])),
                (Ask::TurnEnd, here.decided_leaving[i].as_ref()),
            ];
            for (ask, d) in asks {
                let Some(d) = d else {
                    continue;
                };
                around_an_ask(w, st, d, self.clock.at(NOW), &self.cfg, &mut found.bounds);
                if under_wall_mode(w, st, base, i, ask, d, self.clock) {
                    found.wall_withholds += 1;
                }
            }
        }
        found
    }

    fn at_edge(&self, here: &AtState, edge: &Edge) -> Walk {
        let Lane {
            w,
            m,
            advisor,
            host,
            cfg,
            clock,
        } = self;
        let (st, name) = (&here.st, edge.action);
        let next = &shown_state(&edge.to);
        let (base, decided) = (&here.base, &here.decided);
        let defect = DEFECTS.contains(&name);
        let mut out = Walk::default();
        let mut made = None;
        if let Some((i, ask, decision)) = decision(name, st, next) {
            let jobs_hold = edge.jobs_hold;
            let told = match ask {
                Ask::ItsPoint | Ask::Notice => &here.told[i],
                Ask::TurnEnd => &here.told_leaving[i],
            };
            out.note(
                false,
                agrees(&decision, told, jobs_hold),
                st,
                Some(name),
                || format!("session {}: advised {told:?}", i + 1),
            );
            let d = match ask {
                Ask::ItsPoint | Ask::Notice => &decided[i],
                Ask::TurnEnd => here.decided_leaving[i]
                    .as_ref()
                    .unwrap_or_else(|| panic!("{name} off a wall at {st:?}")),
            };
            let same = asks(&decision, &d.asked, jobs_hold);
            out.note(false, same, st, Some(name), || {
                format!("session {}, decided: {:?}", i + 1, d.asked)
            });
            made = (same && !defect && matches!(decision, Decision::Move { .. })).then_some((i, d));
        }
        if let Some((i, at)) = typed_after_a_move(name) {
            let to = w.named(st[SESSIONS[i]]);
            let typed = a_moves_own_continuation(to, at, clock.at(NOW));
            out.note(false, typed, st, Some(name), || {
                format!(
                    "session {}: a continuation of the move's own typed after a move made {at:?}",
                    i + 1
                )
            });
        }
        let (mut real, now) = if name.starts_with("Reset") {
            let near = project(w, m, st, Some(account_of(name)));
            let read = abstraction(w, m, &near, NOW);
            out.note(
                false,
                read_is_seen(&read, &edge.from),
                st,
                Some(name),
                || format!("about to reset, the real state reads back {read:?}"),
            );
            for (i, want) in edge.held.iter().enumerate() {
                let Verdict::Hold(why) = want else {
                    continue;
                };
                let d = self.decide_at(&near, i, here.own[i], NOW);
                let ends = clock.at(NOW + SOON_S + accounts::RESET_GRACE_S);
                let until = waits_until(&d.round.action);
                let woken = if *why == Hold::NothingUsable {
                    out.woken += 1;
                    until == Some(ends)
                } else {
                    out.held_woken += 1;
                    until.is_some_and(|until| until <= ends)
                };
                out.note(false, woken, st, Some(name), || {
                    format!(
                        "session {}, decided: a minute from the reset, {:?}",
                        i + 1,
                        d.round.action
                    )
                });
                if *why != Hold::NothingUsable && *host == Host::Roster {
                    let uncut = decide(
                        w,
                        &near,
                        i,
                        here.own[i],
                        *advisor,
                        Host::Uncut,
                        NOW,
                        *clock,
                        cfg,
                    );
                    if waits_until(&uncut.round.action).is_some_and(|until| until > ends) {
                        out.held_asleep += 1;
                    }
                }
            }
            (near, NOW + RESET_STEP_S)
        } else if name == "Lapse" {
            (base.clone(), NOW + LAPSE_STEP_S)
        } else {
            (base.clone(), NOW)
        };
        match mirror(w, name, st, next, &mut real, now, WALL_S) {
            Err(refused) => out.note(false, false, st, Some(name), || refused),
            Ok(()) => {
                let read = abstraction(w, m, &real, now);
                out.note(false, read_is_seen(&read, &edge.to), st, Some(name), || {
                    format!("the real state reads back {read:?}, the model {next:?}")
                });
                if let Some(after) = &edge.after {
                    for (i, want) in after.iter().enumerate() {
                        let ask = ask_of(next, i);
                        let got = verdict(w, &advisor(w, &real, i, ask, now), now);
                        let same = the_move(&got) == the_move(want)
                            && (got == Verdict::Resume) == (*want == Verdict::Resume);
                        out.note(false, same, st, Some(name), || {
                            format!("after it, session {}: {got:?}, the model {want:?}", i + 1)
                        });
                    }
                    if let Some((i, d)) = made {
                        out.made += 1;
                        let done = a_move_made(w, i, d, &real, &after[i], now, *clock, cfg);
                        out.note(false, done.is_ok(), st, Some(name), || {
                            format!("session {}, decided: {}", i + 1, done.clone().unwrap_err())
                        });
                    }
                    if name.starts_with("Use") {
                        let pinned = account_of(name);
                        for (i, d) in decided.iter().enumerate() {
                            if st[SESSIONS[i]] != pinned || !walled(st, i) {
                                continue;
                            }
                            out.resumed += 1;
                            let stays = after[i] == Verdict::Resume;
                            out.note(false, stays, st, Some(name), || {
                                format!(
                                    "session {}, at the wall the owner cleared: the model {:?}",
                                    i + 1,
                                    after[i]
                                )
                            });
                            let done = the_owners_clearing(w, base, &real, i, d, now, *clock, cfg);
                            out.note(false, done.is_ok(), st, Some(name), || {
                                format!("session {}, decided: {}", i + 1, done.clone().unwrap_err())
                            });
                        }
                    }
                }
            }
        }
        if name == "Lapse" {
            let mut ended = base.clone();
            let canary = base.canary.as_ref().map(|c| c.sid.clone());
            let forgot = canary.is_some_and(|sid| accounts::forget_session(&mut ended, &sid));
            ended.sessions.clone_from(&base.sessions);
            let read = abstraction(w, m, &ended, NOW);
            let same = forgot && read_is_seen(&read, &edge.to);
            out.note(false, same, st, Some(name), || {
                format!("the canary forgotten, the real state reads back {read:?}")
            });
        }
        out
    }
}

struct Graph {
    m: Model,
    /// Every state reached, in the walk's order.
    nodes: Vec<Node>,
    /// How many states the sound rule reached: every node, and — where the
    /// graph stops at a broken law — the states queued behind it.
    reached: usize,
    /// Each projection ([`Shown`]) of a node, in the order first visited.
    shown: Vec<Shown>,
    /// Each edge as the real code is shown it ([`Edge`]), in the order first
    /// taken, and the projection it leaves (an index into `shown`).
    edges: Vec<(usize, Edge)>,
}

struct Node {
    st: Arc<State>,
    /// Its projection: an index into [`Graph::shown`].
    projection: usize,
    /// The first of the model's laws that does not hold here, if any.
    broken: Option<&'static str>,
    /// What the model tells both sessions here ([`expected`]).
    held: [Verdict; 2],
    /// What it tells a session on a limited account at a plain turn end
    /// ([`expected_leaving`]); `None` for a session on any other account.
    leaving: [Option<Verdict>; 2],
    /// Each action enabled here, in the model's order, and its edge (an index
    /// into [`Graph::edges`]).
    steps: Vec<(&'static str, usize)>,
}

fn sound_steps<'g>(
    g: &'g Graph,
    node: &'g Node,
) -> impl Iterator<Item = (&'static str, &'g Edge)> + 'g {
    node.steps
        .iter()
        .filter(|(action, _)| !DEFECTS.contains(action))
        .map(|&(action, e)| (action, &g.edges[e].1))
}

struct Hashed<K> {
    hash: u64,
    key: K,
}

impl<K: Hash> Hashed<K> {
    fn of(key: K) -> Hashed<K> {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        Hashed {
            hash: hasher.finish(),
            key,
        }
    }
}

impl<K> Hash for Hashed<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash);
    }
}

impl<K: PartialEq> PartialEq for Hashed<K> {
    fn eq(&self, other: &Hashed<K>) -> bool {
        self.hash == other.hash && self.key == other.key
    }
}

impl<K: Eq> Eq for Hashed<K> {}

/// The hasher of a map keyed by [`Hashed`]: the hash it was handed.
#[derive(Default)]
struct Prehashed(u64);

impl Hasher for Prehashed {
    fn write(&mut self, _: &[u8]) {
        unreachable!("only a `Hashed` key is hashed here");
    }

    fn write_u64(&mut self, hash: u64) {
        self.0 = hash;
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

type HashedMap<K, V> = HashMap<Hashed<K>, V, BuildHasherDefault<Prehashed>>;

struct Expansion {
    /// The first of the model's laws that does not hold there, if any.
    broken: Option<&'static str>,
    /// Each action enabled there, in the model's order: its first successor.
    steps: Vec<Step>,
}

struct Step {
    action: &'static str,
    /// See [`Edge::jobs_hold`].
    jobs_hold: bool,
    reaches: Reaches,
}

enum Reaches {
    /// A defect's, which the walk does not go on from: as the real code is
    /// shown it.
    Defect(Box<Shown>),
    /// A state reached before: its index.
    Reached(usize),
    /// One not reached before its breadth was expanded, and its key ([`flat`]).
    New(Box<(State, Hashed<Vec<i64>>)>),
}

struct Resolution {
    resolved: Resolved,
    shown: [usize; SHOWN.len()],
}

impl Resolution {
    fn of(m: &Model) -> Resolution {
        let resolved = Resolved::new(m).expect("the rotation model binds every variable it writes");
        let shown = SHOWN.map(|var| resolved.slot(var));
        Resolution { resolved, shown }
    }
}

fn expand(
    m: &Model,
    at: &Resolution,
    st: &State,
    reached: &HashedMap<Vec<i64>, usize>,
) -> Expansion {
    let r = &at.resolved;
    let values = flat(st);
    let broken = m
        .invariants
        .iter()
        .enumerate()
        .find(|&(i, _)| !r.holds(i, &values))
        .map(|(_, inv)| inv.name);
    let steps = m
        .actions
        .iter()
        .enumerate()
        .filter_map(|(a, action)| {
            let next = r.successors(a, &values).into_iter().next()?;
            let jobs_hold = st["jobs"] == 1
                && decision(action.name, st, &r.state(&next)).is_some_and(|(i, ..)| i == 0);
            let reaches = if DEFECTS.contains(&action.name) {
                Reaches::Defect(Box::new(at.shown.map(|k| next[k])))
            } else {
                let key = Hashed::of(next);
                match reached.get(&key) {
                    Some(&k) => Reaches::Reached(k),
                    None => Reaches::New(Box::new((r.state(&key.key), key))),
                }
            };
            Some(Step {
                action: action.name,
                jobs_hold,
                reaches,
            })
        })
        .collect();
    Expansion { broken, steps }
}

impl Graph {
    fn of(m: Model, strict: bool) -> Graph {
        let at = Resolution::of(&m);
        let init = m.init_state();
        let mut reached: HashedMap<Vec<i64>, usize> = HashedMap::default();
        reached.insert(Hashed::of(flat(&init)), 0);
        let mut states = vec![Arc::new(init)];
        let mut popped: Vec<(Option<&'static str>, Vec<Step>)> = Vec::new();
        'search: while popped.len() < states.len() {
            let level = popped.len()..states.len();
            let expansions = pooled(level.len(), |k| {
                Some(expand(&m, &at, &states[level.start + k], &reached))
            });
            for expansion in expansions {
                let Expansion { broken, mut steps } = expansion.into_value();
                let stop = strict && broken.is_some();
                if stop {
                    steps.clear();
                }
                let steps = steps
                    .into_iter()
                    .map(|step| match step.reaches {
                        Reaches::New(new) => {
                            let (next, key) = *new;
                            let fresh = states.len();
                            let k = *reached.entry(key).or_insert(fresh);
                            if k == fresh {
                                states.push(Arc::new(next));
                            }
                            Step {
                                reaches: Reaches::Reached(k),
                                ..step
                            }
                        }
                        _ => step,
                    })
                    .collect();
                popped.push((broken, steps));
                if stop {
                    break 'search;
                }
            }
        }
        let resolving = Resolving { m: &m, at: &at };
        let shown_at: Vec<_> = pooled(states.len(), |k| {
            let st = &states[k];
            Some((
                shown(st),
                [0, 1].map(|i| expected(&resolving, st, i)),
                [0, 1].map(|i| walled(st, i).then(|| expected_leaving(&resolving, st, i))),
            ))
        })
        .into_iter()
        .map(Slot::into_value)
        .collect();
        let mut projections: HashedMap<Shown, usize> = HashedMap::default();
        let mut edge_ids: HashedMap<Edge, (usize, usize)> = HashedMap::default();
        let mut nodes = Vec::with_capacity(popped.len());
        for chunk in (0..popped.len()).step_by(4096) {
            let chunk = chunk..popped.len().min(chunk + 4096);
            let keyed = pooled(chunk.len(), |k| {
                let k = chunk.start + k;
                let (key, held, _) = &shown_at[k];
                let edges: Vec<_> = popped[k]
                    .1
                    .iter()
                    .map(|step| {
                        let (to, after) = match &step.reaches {
                            Reaches::Defect(to) => (**to, None),
                            Reaches::Reached(t) => (shown_at[*t].0, Some(shown_at[*t].1.clone())),
                            Reaches::New(_) => unreachable!("every new state is merged"),
                        };
                        Hashed::of(Edge {
                            from: *key,
                            action: step.action,
                            to,
                            jobs_hold: step.jobs_hold,
                            held: held.clone(),
                            after,
                        })
                    })
                    .collect();
                Some((Hashed::of(*key), edges))
            });
            for (k, keyed) in chunk.zip(keyed) {
                let (key, edges) = keyed.into_value();
                let fresh = projections.len();
                let projection = *projections.entry(key).or_insert(fresh);
                let (broken, steps) = &popped[k];
                let steps = steps
                    .iter()
                    .zip(edges)
                    .map(|(step, edge)| {
                        let fresh = edge_ids.len();
                        let (id, _) = *edge_ids.entry(edge).or_insert((fresh, projection));
                        (step.action, id)
                    })
                    .collect();
                let (_, held, leaving) = shown_at[k].clone();
                nodes.push(Node {
                    st: Arc::clone(&states[k]),
                    projection,
                    broken: *broken,
                    held,
                    leaving,
                    steps,
                });
            }
        }
        let mut shown_first: Vec<_> = projections.into_iter().collect();
        shown_first.sort_unstable_by_key(|(_, k)| *k);
        let mut edges: Vec<_> = edge_ids.into_iter().collect();
        edges.sort_unstable_by_key(|(_, (id, _))| *id);
        Graph {
            m,
            nodes,
            reached: states.len(),
            shown: shown_first.into_iter().map(|(key, _)| key.key).collect(),
            edges: edges
                .into_iter()
                .map(|(edge, (_, from))| (from, edge.key))
                .collect(),
        }
    }
}

fn sound() -> &'static Graph {
    static SOUND: OnceLock<Graph> = OnceLock::new();
    SOUND.get_or_init(|| Graph::of(supervisor_account_rotation_model(), false))
}

enum Slot<T> {
    Done(T),
    /// It panicked: the payload, raised where the walk meets the job
    /// ([`Slot::get`]).
    Panicked(Mutex<Option<Box<dyn Any + Send>>>),
    /// Not run: an earlier job of its pool panicked, or the job it needs did,
    /// and the walk meets that one first.
    Skipped,
}

impl<T> Slot<T> {
    fn done(&self) -> Option<&T> {
        match self {
            Slot::Done(value) => Some(value),
            Slot::Panicked(_) | Slot::Skipped => None,
        }
    }

    fn get(&self) -> &T {
        let payload = match self {
            Slot::Done(value) => return value,
            Slot::Panicked(payload) => payload
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .expect("a job's panic is raised once"),
            Slot::Skipped => unreachable!("a job skipped behind a panic the walk raises first"),
        };
        match payload.downcast::<String>() {
            Ok(message) => panic!("{message}"),
            Err(payload) => match payload.downcast::<&'static str>() {
                Ok(message) => panic!("{message}"),
                Err(payload) => resume_unwind(payload),
            },
        }
    }

    fn into_value(self) -> T {
        match self {
            Slot::Done(value) => value,
            failed => {
                failed.get();
                unreachable!("a failed job's panic is raised")
            }
        }
    }
}

fn pooled<T: Send>(n: usize, job: impl Fn(usize) -> Option<T> + Sync) -> Vec<Slot<T>> {
    let next = AtomicUsize::new(0);
    let first_panic = AtomicUsize::new(usize::MAX);
    let workers = thread::available_parallelism()
        .map_or(1, NonZero::get)
        .min(n)
        .max(1);
    let found: Vec<Vec<(usize, Slot<T>)>> = thread::scope(|scope| {
        let pool: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let k = next.fetch_add(1, Ordering::Relaxed);
                        if k >= n {
                            break mine;
                        }
                        if k > first_panic.load(Ordering::Relaxed) {
                            continue;
                        }
                        let slot = match catch_unwind(AssertUnwindSafe(|| job(k))) {
                            Ok(Some(value)) => Slot::Done(value),
                            Ok(None) => Slot::Skipped,
                            Err(payload) => {
                                first_panic.fetch_min(k, Ordering::Relaxed);
                                Slot::Panicked(Mutex::new(Some(payload)))
                            }
                        };
                        mine.push((k, slot));
                    }
                })
            })
            .collect();
        pool.into_iter()
            .map(|worker| {
                worker
                    .join()
                    .unwrap_or_else(|payload| resume_unwind(payload))
            })
            .collect()
    });
    let mut slots: Vec<Slot<T>> = (0..n).map(|_| Slot::Skipped).collect();
    for (k, slot) in found.into_iter().flatten() {
        slots[k] = slot;
    }
    slots
}

/// What a walk asks of the real code: the projections it judges and the edges it
/// takes.
#[derive(Clone, Copy)]
struct Part {
    /// Every projection judged against the model's answers at its states (and,
    /// in a strict walk, the model's laws there and the bounds tried around
    /// it); else only the projections the edges taken leave, as those edges
    /// read them.
    states: bool,
    /// The actions whose edges are taken. Every action's firings are counted
    /// all the same.
    edges: fn(&str) -> bool,
}

impl Part {
    const WHOLE: Part = Part {
        states: true,
        edges: |_| true,
    };
}

fn walk(w: &World, g: &Graph, advisor: Advisor, host: Host, strict: bool, part: Part) -> Walk {
    let lane = Lane {
        w,
        m: &g.m,
        advisor,
        host,
        cfg: SupervisorConfig::default(),
        clock: Clock::start(),
    };
    let clock = lane.clock;
    let mut asked = vec![part.states; g.shown.len()];
    for (from, edge) in &g.edges {
        asked[*from] |= (part.edges)(edge.action);
    }
    let at_state = pooled(g.shown.len(), |k| {
        Some(asked[k].then(|| lane.at_state(&g.shown[k])))
    });
    let around = if strict && part.states {
        pooled(g.shown.len(), |k| {
            at_state[k]
                .done()
                .map(|here| here.as_ref().map(|here| lane.around(here)))
        })
    } else {
        Vec::new()
    };
    let at_edge = pooled(g.edges.len(), |e| {
        let (from, edge) = &g.edges[e];
        if !(part.edges)(edge.action) {
            return Some(None);
        }
        at_state[*from]
            .done()
            .map(|here| here.as_ref().map(|here| lane.at_edge(here, edge)))
    });
    let mut out = Walk::default();
    for node in &g.nodes {
        let st = &node.st;
        if !part.states {
            for &(action, e) in &node.steps {
                *out.fired.entry(action).or_default() += 1;
                if let Some(found) = at_edge[e].get() {
                    out.add(found, strict, st);
                }
            }
            continue;
        }
        if strict && let Some(law) = node.broken {
            panic!("{law} at {st:?}");
        }
        let here = at_state[node.projection]
            .get()
            .as_ref()
            .expect("every projection is asked");
        out.add(&here.found, strict, st);
        let held = &node.held;
        for (i, (told, want)) in here.told.iter().zip(held).enumerate() {
            out.note(strict, told == want, st, None, || {
                format!("session {}: advised {told:?}, the model {want:?}", i + 1)
            });
        }
        for (i, told) in here.told_leaving.iter().enumerate() {
            let Some(want) = &node.leaving[i] else {
                continue;
            };
            out.note(strict, told == want, st, None, || {
                format!(
                    "session {}, at a plain turn end on a limited account: advised {told:?}, \
                     the model {want:?}",
                    i + 1
                )
            });
        }
        for (i, (d, want)) in here.decided.iter().zip(held).enumerate() {
            let model = wanted(want);
            out.note(strict, d.asked == model, st, None, || {
                format!(
                    "session {}, decided: {:?}, the model {model:?}",
                    i + 1,
                    d.asked
                )
            });
            if let (Verdict::Hold(why), Asked::Waits) = (want, &d.asked) {
                let ends = the_wait_ends(w, &here.base, d, *why, clock);
                out.note(strict, ends.is_ok(), st, None, || {
                    format!("session {}, decided: {}", i + 1, ends.clone().unwrap_err())
                });
            }
        }
        for (i, d) in here.decided_leaving.iter().enumerate() {
            let Some(d) = d else {
                continue;
            };
            let model = wanted(
                node.leaving[i]
                    .as_ref()
                    .expect("a session on a limited account"),
            );
            out.note(strict, d.asked == model, st, None, || {
                format!(
                    "session {}, at a plain turn end on a limited account, decided: {:?}, the \
                     model {model:?}",
                    i + 1,
                    d.asked
                )
            });
        }
        if strict {
            let tried = around[node.projection].get();
            out.add(tried.as_ref().expect("tried around"), strict, st);
        }
        for &(action, e) in &node.steps {
            *out.fired.entry(action).or_default() += 1;
            if let Some(found) = at_edge[e].get() {
                out.add(found, strict, st);
            }
        }
    }
    out.states = g.reached;
    out
}

#[test]
fn tier1_the_real_advice_and_transactions_are_the_models_at_every_reachable_state() {
    let g = sound();
    let m = &g.m;
    for (reading, w) in [
        ("by label", World::by_label()),
        ("by week", World::by_week()),
    ] {
        assert_eq!(spent_before(&w, m), w.bound() - cnst(m, "Budget"));
        let walked = walk(&w, g, advise, Host::Roster, true, Part::WHOLE);
        eprintln!(
            "account rotation Tier-1 ({reading}): {} states walked; the real decider asked \
             {} moves, {} waits, {} of today's acts and {} continuations at a notice the \
             owner lifted, {} moves were made and served, the \
             session's bounds were tried around {} asks at a wall and {} ahead of one, {} \
             waits were read a minute from a reset and {} holds beside them ({} of which the \
             advice with no opening sleeps through), {} sessions stood at the wall the owner \
             cleared and were continued there, and `accounts = \"wall\"` withholds {} rotations \
             ahead of a wall",
            walked.states,
            walked.moves,
            walked.waits,
            walked.todays,
            walked.resumes,
            walked.made,
            walked.bounds.at_a_wall,
            walked.bounds.ahead,
            walked.woken,
            walked.held_woken,
            walked.held_asleep,
            walked.resumed,
            walked.wall_withholds
        );
        assert!(walked.states > 50, "{}", walked.states);
        assert_eq!(
            walked.moves + walked.waits + walked.todays + walked.resumes,
            2 * walked.states
        );
        for (what, n) in [
            ("moves", walked.moves),
            ("waits", walked.waits),
            ("today's acts", walked.todays),
            ("continuations at a lifted notice", walked.resumes),
            ("moves made", walked.made),
            ("asks at a wall", walked.bounds.at_a_wall),
            ("asks ahead of one", walked.bounds.ahead),
            ("waits a minute from a reset", walked.woken),
            ("holds a minute from a reset", walked.held_woken),
            ("holds the uncut advice sleeps through", walked.held_asleep),
            ("sessions at the wall the owner cleared", walked.resumed),
            (
                "rotations ahead of a wall that `wall` withholds",
                walked.wall_withholds,
            ),
        ] {
            assert!(n > 0, "no {what}");
        }
        let moved: usize = MOVES
            .iter()
            .chain(&LEAVES)
            .map(|action| walked.fired.get(action).copied().unwrap_or(0))
            .sum();
        assert_eq!(walked.made, moved);
        for action in &m.actions {
            let fired = walked.fired.get(action.name).copied().unwrap_or(0);
            assert_eq!(
                fired == 0,
                DEFECTS.contains(&action.name),
                "{}: taken {fired} times",
                action.name
            );
        }
    }
}

#[test]
fn the_defects_and_todays_rule_disagree_with_the_real_code() {
    let w = World::by_label();
    let m = supervisor_account_rotation_model();
    let (buggy, sound) = thread::scope(|scope| {
        let sound = scope.spawn(sound);
        let buggy = Graph::of(aterm_spec::interp::with_buggy(&m, 1), false);
        let sound = sound
            .join()
            .unwrap_or_else(|payload| resume_unwind(payload));
        (buggy, sound)
    });
    let defects = Part {
        states: false,
        edges: |action| DEFECTS.contains(&action),
    };
    let walked = walk(&w, &buggy, advise, Host::Roster, false, defects);
    eprintln!(
        "account rotation Tier-1 (Buggy = 1): {} states walked, {} disagreements",
        walked.states,
        walked.off.len()
    );
    for defect in DEFECTS {
        let fired = walked.fired.get(defect).copied().unwrap_or(0);
        assert!(fired > 0, "{defect} never fired");
        let how = match defect {
            "WallUnread" => "reads back",
            "CarriesOnAfterAnEarlyMove" => "typed after a move",
            _ => "advised",
        };
        let parted = walked
            .off
            .iter()
            .filter(|off| off.action == Some(defect) && off.what.contains(how))
            .count();
        assert_eq!(parted, fired, "{defect}: disagreed with at every firing");
        if how == "advised" {
            let parted = walked
                .off
                .iter()
                .filter(|off| off.action == Some(defect) && off.what.contains("decided"))
                .count();
            assert_eq!(parted, fired, "{defect}: decided otherwise at every firing");
        }
    }
    assert_eq!(
        (buggy.nodes.len(), buggy.reached),
        (sound.nodes.len(), sound.reached)
    );
    for (b, s) in buggy.nodes.iter().zip(&sound.nodes) {
        assert!(
            (&b.st, &b.held, &b.leaving, b.broken) == (&s.st, &s.held, &s.leaving, s.broken),
            "the buggy model parts from the committed one at {:?}",
            b.st
        );
        assert!(
            sound_steps(&buggy, b).eq(sound_steps(sound, s)),
            "the buggy model steps otherwise from {:?}",
            b.st
        );
    }

    let resets = Part {
        states: true,
        edges: |action| action.starts_with("Reset"),
    };
    let today = walk(&w, sound, todays_rule, Host::NoRoster, false, resets);
    assert_eq!(today.states, walked.states);
    let parted = today
        .off
        .iter()
        .find(|off| {
            off.action.is_none()
                && off.what.starts_with("session 1:")
                && off.state["s1"] == 1
                && off.state["d1"] == 2
                && off.state["d2"] == 0
                && off.state["run"] == 0
                && off.state["rot"] == 0
        })
        .unwrap_or_else(|| panic!("today's rule agreed at a wall with a fresh account"));
    assert!(
        parted.what.contains("Hold(NothingUsable)") && parted.what.contains("Move"),
        "{parted:?}"
    );
    assert_eq!(
        expected(&m, &parted.state, 0),
        Verdict::Move {
            to: 2,
            rotates: true,
            takes: false,
            ahead: false
        },
        "{parted:?}"
    );
    assert!(
        !today.off.iter().any(|off| off.action.is_none()
            && off.state["d1"] == 2
            && off.state["d2"] == 2
            && off.state["sh"] == 2),
        "today's rule is the model's where nothing is usable"
    );

    let waited = today
        .off
        .iter()
        .find(|off| {
            off.action.is_none()
                && off.what.starts_with("session 1, decided:")
                && off.state == parted.state
        })
        .unwrap_or_else(|| panic!("with no advice the decider agreed at {:?}", parted.state));
    assert!(
        waited.what.contains("decided: Waits")
            && waited
                .what
                .contains("the model Move { to: 2, ahead: false }"),
        "{waited:?}"
    );
    let lifted = today
        .off
        .iter()
        .find(|off| {
            off.action.is_none()
                && off.state["n1"] == 1
                && off.what.starts_with("session 1, decided:")
        })
        .unwrap_or_else(|| panic!("with no advice the decider agreed at a lifted notice"));
    assert!(
        lifted.what.contains("decided: Waits") && lifted.what.contains("the model Resume"),
        "{lifted:?}"
    );
    assert_eq!((today.moves, today.leaving_moves, today.made), (0, 0, 0));
    assert_eq!(today.waits + today.todays, 2 * today.states);
    assert_eq!(today.resumes, 0, "nor is a lifted notice continued");
    assert!(
        today.off.iter().any(
            |off| off.action.is_some_and(|action| action.starts_with("Reset"))
                && off.what.contains("a minute from the reset")
        ),
        "with no advice a wait ended at another account's reset"
    );
}

struct InStep {
    w: World,
    m: Model,
    st: State,
    real: accounts::State,
    now: i64,
    wall_s: i64,
}

impl InStep {
    fn begin(wall_s: i64) -> InStep {
        let w = World::by_label();
        let m = aterm_spec::interp::with_consts(
            &supervisor_account_rotation_model(),
            &[("Budget", w.bound())],
        );
        let mut real = accounts::State {
            machine: "0a0b".to_string(),
            ..accounts::State::default()
        };
        for sid in SIDS {
            accounts::note_session(&mut real, sid, "main", None, NOW);
        }
        let read = sample(&w, 1, false, NOW + 4 * HOUR, NOW);
        accounts::note_sample(&mut real, "main", read, NOW);
        let st = m.init_state();
        let out = InStep {
            w,
            m,
            st,
            real,
            now: NOW,
            wall_s,
        };
        out.in_step("at the start");
        out
    }

    fn told(&self, i: usize) -> Advice {
        advise(&self.w, &self.real, i, ask_of(&self.st, i), self.now).0
    }

    fn told_past(&self, i: usize) -> Advice {
        let (at_wall, written) = wall_of(&self.w, &self.real, i, ask_of(&self.st, i), self.now);
        advise_past_at(
            &self.w,
            &self.real,
            i,
            at_wall,
            written,
            Mode::Early,
            self.now,
        )
    }

    fn reads_back(&self, after: &str) {
        let (w, m) = (&self.w, &self.m);
        assert_eq!(
            abstraction(w, m, &self.real, self.now),
            seen(&self.st),
            "after {after}"
        );
        for inv in &m.invariants {
            assert!(
                m.check_invariant(inv.name, &self.st),
                "{} after {after}",
                inv.name
            );
        }
    }

    fn in_step(&self, after: &str) {
        self.reads_back(after);
        let (w, m) = (&self.w, &self.m);
        for i in 0..2 {
            let got = verdict(
                w,
                &advise(w, &self.real, i, ask_of(&self.st, i), self.now),
                self.now,
            );
            let want = expected(m, &self.st, i);
            assert_eq!(
                (the_move(&got), got == Verdict::Resume),
                (the_move(&want), want == Verdict::Resume),
                "session {} after {after}: {got:?}, the model {want:?}",
                i + 1
            );
        }
    }

    fn step(&mut self, action: &str) {
        let before = self.st.clone();
        assert!(self.m.fire(action, &mut self.st), "{action} at {before:?}");
        self.now += MIN;
        let after = self.st.clone();
        mirror(
            &self.w,
            action,
            &before,
            &after,
            &mut self.real,
            self.now,
            self.wall_s,
        )
        .unwrap_or_else(|refused| panic!("{action}: {refused}"));
        self.in_step(action);
    }

    fn reset(&mut self, action: &str) {
        let before = self.st.clone();
        assert!(self.m.fire(action, &mut self.st), "{action} at {before:?}");
        let name = self.w.named(account_of(action));
        self.now = self
            .real
            .account(name)
            .limited_until(self.now)
            .unwrap_or_else(|| panic!("{name} is not limited"));
        self.reads_back(action);
    }

    fn lapse(&mut self) {
        let before = self.st.clone();
        assert!(self.m.fire("Lapse", &mut self.st), "Lapse at {before:?}");
        let since = self
            .real
            .canary_since()
            .unwrap_or_else(|| panic!("no canary holds at {before:?}"));
        self.now = since + accounts::CANARY_HOLD_S;
        self.reads_back("Lapse");
    }
}

#[test]
fn a_real_history_from_an_empty_state_stays_in_step_with_the_model() {
    let mut h = InStep::begin(30 * MIN);
    let rotate = |to: &str, ahead: bool| Advice::Move {
        to: to.to_string(),
        ahead,
        rotates: true,
        takes_canary: false,
    };
    let follow = |to: &str, ahead: bool| Advice::Move {
        to: to.to_string(),
        ahead,
        rotates: false,
        takes_canary: false,
    };

    h.step("WallD1");
    assert_eq!(h.told(0), rotate("work-2", false));
    assert_eq!(h.told(1), rotate("work-2", false));
    h.step("Move1");
    assert_eq!(h.real.canary_since(), Some(h.now));
    assert_eq!(
        h.told(1),
        Advice::Stay(StayWhy::Canary),
        "the herd does not follow an account that has not served a response"
    );
    h.step("Wait2");
    h.step("Served");
    assert_eq!(h.told(1), follow("work-2", false));
    h.step("Move2");

    h.step("LowD2");
    assert_eq!(h.told(0), rotate("pool-a", true));
    h.step("Move1");
    h.step("Strike");
    assert_eq!(
        (h.real.active.as_deref(), h.real.proven),
        (Some("work-2"), true),
        "`active` goes back, and nobody else was moved"
    );
    assert_eq!(h.real.sessions["s-two"].account, "work-2");
    assert_eq!(
        h.told(0),
        follow("work-2", false),
        "at the struck account's wall the session goes back to the low one"
    );
    h.step("Move1");

    h.reset("ResetD1");
    assert_eq!(h.told(0), Advice::Probe);
    assert_eq!(h.told_past(0), rotate("main", true));
    let place = Place {
        i: 0,
        at_wall: false,
        written: None,
        mode: Mode::Early,
    };
    let clock = Clock::start();
    assert_eq!(
        mapped(&h.w, &h.real, &Advice::Probe, place, h.now, clock),
        Some(AccountAdvice::Probe {
            otherwise: Some(Box::new(AccountAdvice::Move {
                to: "main".to_string()
            })),
        })
    );
    assert_eq!(
        AccountAdvice::of_roster(&Advice::Probe, &h.real, false, h.now, clock.at(h.now)),
        Some(AccountAdvice::Probe { otherwise: None })
    );
    let read = sample(&h.w, 2, true, h.now + 4 * HOUR, h.now);
    assert_eq!(
        accounts::note_sample(&mut h.real, "work-2", read, h.now).len(),
        1
    );
    h.in_step("the probe's read");
    assert_eq!(h.told(0), rotate("main", true));
    h.step("Move1");
    h.step("Served");
    assert_eq!(h.told(1), follow("main", true));
    h.step("Move2");

    h.step("UseSh");
    assert_eq!(h.told(0), follow("pool-a", true));
    h.step("Move1");
    h.step("Move2");
    h.step("LowSh");
    assert_eq!(h.told(0), Advice::Stay(StayWhy::Pinned));
    h.step("WallSh");
    assert!(!h.real.pinned, "a wall on the pinned account ends the pin");
    assert_eq!(h.told(1), rotate("main", false));
    h.step("Move2");
    h.step("Served");
    h.step("Move1");
    assert_eq!(
        (h.st["s1"], h.st["s2"], h.st["act"], h.st["rot"]),
        (1, 1, 1, 4)
    );
    assert!(
        h.now - NOW < accounts::ROTATION_WINDOW_S,
        "the history is one rotation hour"
    );
}

#[test]
fn six_rotations_in_an_hour_are_the_bound_and_the_seventh_wall_is_waited_out() {
    let mut h = InStep::begin(5 * MIN);
    assert_eq!(cnst(&h.m, "Budget"), 6);
    let turns = [
        ("WallD1", "ResetD1", "main", "work-2"),
        ("WallD2", "ResetD2", "work-2", "main"),
    ];
    for (round, (wall, reset, from, to)) in turns.into_iter().cycle().take(6).enumerate() {
        h.step(wall);
        assert_eq!(
            h.told(0),
            Advice::Move {
                to: to.to_string(),
                ahead: false,
                rotates: true,
                takes_canary: false
            },
            "round {round}"
        );
        h.step("Move1");
        h.step("Served");
        h.step("Move2");
        assert_eq!(h.real.sessions_on(to), SIDS, "round {round}");
        h.reset(reset);
        assert_eq!(
            h.real.account(from).limited_until(h.now),
            None,
            "round {round}"
        );
    }
    assert_eq!(h.st["rot"], 6);
    assert_eq!(h.real.rotations.len(), 6);
    assert!(
        h.now - h.real.rotations[0] < accounts::ROTATION_WINDOW_S,
        "all six are inside the hour"
    );

    h.step("WallD1");
    for i in 0..2 {
        assert_eq!(
            h.told(i),
            Advice::Stay(StayWhy::Breaker {
                until: h.real.rotations[0] + accounts::ROTATION_WINDOW_S
            }),
            "session {i}"
        );
        assert_eq!(expected(&h.m, &h.st, i), Verdict::Hold(Hold::Breaker));
    }
    assert!(!h.m.action_enabled("Move1", &h.st) && h.m.action_enabled("Wait1", &h.st));
    let before = h.real.clone();
    assert_eq!(
        accounts::begin_rotation(
            &mut h.real,
            &h.w.roster,
            &h.w.enrolled,
            Some("main"),
            Mover {
                sid: SIDS[0],
                account: "main"
            },
            "work-2",
            h.now
        ),
        Err(Lost::Spent)
    );
    assert_eq!(h.real, before, "a refusal writes nothing");
}

#[test]
fn a_canary_that_never_answers_is_replaced_by_the_next_session_after_its_hold() {
    let mut h = InStep::begin(4 * HOUR);
    let take = Advice::Move {
        to: "work-2".to_string(),
        ahead: false,
        rotates: false,
        takes_canary: true,
    };
    h.step("WallD1");
    h.step("Move1");
    let chosen = h.now;
    assert_eq!(
        h.real.canary,
        Some(Canary {
            sid: "s-one".to_string(),
            at: chosen,
            took: false
        })
    );
    assert_eq!(h.told(1), Advice::Stay(StayWhy::Canary));
    h.step("Wait2");

    h.now = chosen + accounts::CANARY_HOLD_S - 1;
    assert_eq!(h.told(1), Advice::Stay(StayWhy::Canary));
    h.reads_back("the hold, a second from its end");
    h.lapse();
    assert_eq!(
        (
            h.real.active.as_deref(),
            h.real.proven,
            h.real.rotations.len()
        ),
        (Some("work-2"), false, 1)
    );
    assert_eq!(h.told(1), take);
    assert_eq!(
        the_move(&verdict(&h.w, &(h.told(0), false), h.now)),
        None,
        "the silent canary stays where it is"
    );

    h.step("Move2");
    assert_eq!(
        h.real.canary,
        Some(Canary {
            sid: "s-two".to_string(),
            at: h.now,
            took: true
        })
    );
    assert_eq!(
        (
            h.real.active.as_deref(),
            h.real.proven,
            h.real.rotations.len()
        ),
        (Some("work-2"), false, 1)
    );
    assert_eq!(h.st["rot"], 1);

    h.step("Served");
    assert!(h.real.proven && h.real.canary.is_none());
    assert_eq!(h.real.sessions_on("work-2"), SIDS);
    assert_eq!(
        (h.st["s1"], h.st["s2"], h.st["act"], h.st["prov"]),
        (2, 2, 2, 1)
    );

    let mut h = InStep::begin(4 * HOUR);
    h.step("WallD1");
    h.step("Move1");
    assert_eq!(h.told(1), Advice::Stay(StayWhy::Canary));
    assert!(accounts::forget_session(&mut h.real, "s-one"));
    accounts::note_session(&mut h.real, "s-one", "work-2", None, h.now);
    assert!(h.m.fire("Lapse", &mut h.st));
    h.in_step("the canary's agent ending");
    assert_eq!(h.told(1), take);
}

#[test]
fn a_strike_goes_back_to_an_account_that_never_served_as_unproven() {
    let onto = |takes_canary: bool| Advice::Move {
        to: "work-2".to_string(),
        ahead: false,
        rotates: false,
        takes_canary,
    };
    for served in [false, true] {
        let mut h = InStep::begin(HOUR);
        h.step("WallD1");
        h.step("Move1");
        let chosen = h.now;
        let first = h.real.canary.clone();
        assert_eq!(first.as_ref().map(|c| c.sid.as_str()), Some("s-one"));
        if served {
            h.step("Served");
        }
        h.wall_s = 20 * MIN;
        h.step("WallD2");
        assert_eq!(
            (h.real.active.as_deref(), h.real.proven),
            (Some("work-2"), served),
            "a recorded wall strikes nothing"
        );
        h.wall_s = HOUR;
        assert_eq!(
            h.told(1),
            Advice::Move {
                to: "pool-a".to_string(),
                ahead: false,
                rotates: true,
                takes_canary: false
            }
        );
        h.step("Move2");
        assert_eq!(
            (h.real.previous.as_deref(), h.real.previous_proven),
            (Some("work-2"), served)
        );
        if !served {
            let mut inside = h.real.clone();
            let word = Strike::Wall {
                window: WINDOW.to_string(),
                until: Some(h.now + HOUR),
            };
            let at = h.now + MIN;
            assert!(at - chosen < accounts::CANARY_HOLD_S);
            accounts::strike(&mut inside, "pool-a", "s-two", &word, at);
            assert_eq!(inside.canary, first);
            assert_eq!(abstraction(&h.w, &h.m, &inside, at)["hold"], 1);
            let mut model = h.st.clone();
            assert!(h.m.fire("Strike", &mut model));
            assert_eq!(model["hold"], 0, "the model keeps no record to put back");
        }
        h.now = chosen + accounts::CANARY_HOLD_S - MIN;
        h.step("Strike");
        assert_eq!(
            (h.real.active.as_deref(), h.real.proven),
            (Some("work-2"), served),
            "proven or not, as it was when it was left"
        );
        assert_eq!(h.real.canary, if served { None } else { first });
        assert!(!h.real.canary_holds(h.now));
        assert_eq!(
            (h.st["act"], h.st["prov"], h.st["rot"]),
            (2, i64::from(served), 2)
        );

        h.reset("ResetD2");
        assert_eq!(h.told(1), onto(!served), "served: {served}");
        h.step("Move2");
        assert_eq!(h.real.canary.is_some(), !served);
        if !served {
            h.step("Served");
        }
        assert!(h.real.proven);
        assert_eq!(h.real.sessions_on("work-2"), SIDS);
    }
}

struct Carried {
    h: InStep,
    clock: Clock,
    cfg: SupervisorConfig,
    te: [TurnEndState; 2],
    /// The reset the limit on each session's screen names; `None`: no limit.
    screen: [Option<i64>; 2],
    /// The busy work of a turn that has ended and no decision has read yet.
    worked: [Option<Duration>; 2],
}

impl Carried {
    fn begin(wall_s: i64) -> Carried {
        let mut c = Carried {
            h: InStep::begin(wall_s),
            clock: Clock::start(),
            cfg: SupervisorConfig::default(),
            te: [0, 1].map(|_| TurnEndState::new(TurnEndTiming::default())),
            screen: [None; 2],
            worked: [None; 2],
        };
        for i in 0..2 {
            assert_eq!(c.turn_ends(i, None), None);
            let first = c.ask(i);
            assert_eq!(c.said(&first), Asked::Today);
            assert_eq!(first.action.rule_id(), Some(RULE_CONTINUE));
            c.typed(i, &first.action);
        }
        c
    }

    fn at(&self) -> Instant {
        self.clock.at(self.h.now)
    }

    fn shows(&self, i: usize, worked: Option<Duration>) -> TurnEndReading {
        match self.screen[i] {
            Some(reset) => limit(self.clock.at(reset), worked),
            None => turn_end(worked),
        }
    }

    fn turn_ends(&mut self, i: usize, wall: Option<i64>) -> Option<AccountAnswer> {
        self.screen[i] = wall;
        self.worked[i] = Some(REAL_WORK);
        let reading = self.shows(i, self.worked[i]);
        let answer = self.te[i].account_answer(&reading);
        if let Some(answer) = answer {
            self.te[i].account_told(answer);
        }
        answer
    }

    fn ask(&mut self, i: usize) -> Round {
        let at = self.at();
        let worked = self.worked[i].take();
        let reading = self.shows(i, worked);
        assert_eq!(
            self.te[i].account_answer(&reading),
            None,
            "an answer nobody told"
        );
        let (w, real, now, clock) = (&self.h.w, &self.h.real, self.h.now, self.clock);
        round(&mut self.te[i], reading, &self.cfg, at, |point| {
            hosted(w, real, i, point, Mode::Early, now, clock)
        })
    }

    fn said(&self, r: &Round) -> Asked {
        asked(&self.h.w, r)
    }

    fn model(&self, i: usize) -> Asked {
        wanted(&expected(&self.h.m, &self.h.st, i))
    }

    fn typed(&mut self, i: usize, action: &TurnEndAction) {
        assert!(action.rule_id().is_some(), "nothing to type: {action:?}");
        let at = self.at();
        self.te[i].acted(action, &turn_end(None), at);
    }

    fn wall(&mut self, action: &str, sessions: &[usize]) {
        self.h.step(action);
        let reset = self.h.now + self.h.wall_s;
        for &i in sessions {
            assert_eq!(self.turn_ends(i, Some(reset)), None);
        }
    }

    fn restart(r: &Round) -> (&Restart, &TurnEndAction) {
        match &r.action {
            TurnEndAction::Restart {
                why,
                rule_id,
                otherwise,
            } if *rule_id == RULE_ACCOUNT => (why, otherwise),
            other => panic!("no account move is asked: {other:?}"),
        }
    }

    fn not_made(&mut self, i: usize, r: &Round, word: &str) {
        let (why, otherwise) = Carried::restart(r);
        let Restart::Account { to, .. } = why else {
            panic!("{why:?}");
        };
        let before = self.h.real.clone();
        let wall = r.point.is_some_and(AccountPoint::at_wall);
        let advice = advise_past_at(&self.h.w, &before, i, wall, None, Mode::Early, self.h.now);
        let me = before.sessions[SIDS[i]].account.clone();
        match advice {
            Advice::Move { rotates: true, .. } => {
                let read = before.active.clone();
                accounts::begin_rotation(
                    &mut self.h.real,
                    &self.h.w.roster,
                    &self.h.w.enrolled,
                    read.as_deref(),
                    Mover {
                        sid: SIDS[i],
                        account: &me,
                    },
                    to,
                    self.h.now,
                )
                .unwrap_or_else(|lost| panic!("the commit lost: {lost:?}"));
            }
            Advice::Move {
                takes_canary: true, ..
            } => {
                let read = before.canary.clone();
                accounts::take_canary(&mut self.h.real, read.as_ref(), SIDS[i], self.h.now)
                    .expect("the take");
            }
            Advice::Move { .. } => {}
            other => panic!("no move was advised: {other:?}"),
        }
        let undo = Strike::NotMoved(word.to_string());
        accounts::strike(&mut self.h.real, to, SIDS[i], &undo, self.h.now);
        assert_eq!(self.h.real, before, "a relaunch not made is no step");
        if word.starts_with("wait:") {
            return;
        }
        let at = self.at();
        self.te[i].restarted(why, false, &r.reading, at);
        if otherwise.rule_id().is_some() {
            self.typed(i, &otherwise.clone());
        }
    }

    fn make(&mut self, i: usize, r: &Round, action: &str) {
        let (why, _) = Carried::restart(r);
        let Asked::Move { to, .. } = self.said(r) else {
            panic!("no move the model has a word for: {:?}", r.action);
        };
        assert_eq!(self.model(i), self.said(r), "before {action}");
        self.h.step(action);
        assert_eq!(self.h.st[SESSIONS[i]], to, "{action}");
        let at = self.at();
        self.te[i].restarted(why, true, &r.reading, at);
        self.screen[i] = None;
        self.worked[i] = None;
    }

    fn first_act(&mut self, i: usize, rule: &str) {
        let first = self.ask(i);
        assert_eq!(first.point, None, "{:?}", first.action);
        assert_eq!(first.action.rule_id(), Some(rule), "{:?}", first.action);
        self.typed(i, &first.action);
    }
}

#[test]
fn the_sessions_own_bounds_part_from_the_model_only_toward_todays_rule() {
    let mut c = Carried::begin(20 * MIN);
    let to = |account: i64, ahead: bool| Asked::Move { to: account, ahead };

    c.wall("WallD1", &[0, 1]);
    let asked = c.ask(0);
    assert_eq!((c.said(&asked), c.model(0)), (to(2, false), to(2, false)));
    c.h.step("JobsStart");
    c.not_made(0, &asked, "wait:changed");
    assert!(!c.te[0].account_refused());
    c.h.step("Wait1");
    let again = c.ask(0);
    assert_eq!(again.action, asked.action, "no latch for a wait");
    c.h.step("JobsEnd");
    c.make(0, &again, "Move1");
    let chosen = c.h.now;

    let held = c.ask(1);
    assert_eq!((c.said(&held), c.model(1)), (Asked::Waits, Asked::Waits));
    assert_eq!(
        waits_until(&held.action),
        Some(c.clock.at(chosen + accounts::CANARY_HOLD_S))
    );
    assert!(waits_until(&held.action) < waits_until(&held.today));
    c.h.step("Wait2");

    let prompt = c.ask(0);
    assert_eq!(prompt.point, None);
    assert_eq!(prompt.action.rule_id(), Some(RULE_LIMIT_RESUME));
    c.typed(0, &prompt.action);
    assert!(!c.te[0].account_resume_owed());
    assert_eq!(c.turn_ends(0, None), Some(AccountAnswer::Served));
    c.h.step("Served");
    let on = c.ask(0);
    assert_eq!(on.point, Some(AccountPoint::TurnEnd));
    assert_eq!((c.said(&on), c.model(0)), (Asked::Today, Asked::Today));
    c.typed(0, &on.action);

    let follow = c.ask(1);
    assert_eq!((c.said(&follow), c.model(1)), (to(2, false), to(2, false)));
    c.not_made(1, &follow, "refused:line");
    assert!(c.te[1].account_refused());
    let latched = c.ask(1);
    assert_eq!(latched.point, None);
    assert_eq!(latched.action, follow.today);
    assert_eq!(c.said(&latched), Asked::Waits);
    assert_eq!(c.model(1), to(2, false), "the stated exception");
    c.h.reset("ResetD1");
    let resumed = c.ask(1);
    assert_eq!(resumed.point, None);
    assert_eq!(resumed.action.rule_id(), Some(RULE_LIMIT_RESUME));
    c.typed(1, &resumed.action);
    assert_eq!(c.turn_ends(1, None), None);
    assert!(c.te[1].account_refused(), "until the point is folded in");
    let follow = c.ask(1);
    assert!(!c.te[1].account_refused(), "…which ends it");
    assert_eq!(follow.point, Some(AccountPoint::TurnEnd));
    assert_eq!((c.said(&follow), c.model(1)), (to(2, true), to(2, true)));
    assert_eq!(follow.today.rule_id(), Some(RULE_CONTINUE));
    c.make(1, &follow, "Move2");
    c.first_act(1, RULE_CONTINUE);
    assert_eq!(c.turn_ends(1, None), Some(AccountAnswer::Served));

    c.h.step("UseSh");
    assert_eq!(c.turn_ends(0, None), None);
    let pinned = c.ask(0);
    assert_eq!((c.said(&pinned), c.model(0)), (to(3, true), to(3, true)));
    c.not_made(0, &pinned, "refused:line");
    assert!(!c.te[0].account_refused(), "no wall, no wall's latch");
    assert_eq!(c.turn_ends(0, None), None);
    let same = c.ask(0);
    assert_eq!(same.point, Some(AccountPoint::TurnEnd));
    assert_eq!(
        same.reading.account,
        Some(AccountAdvice::Move {
            to: "pool-a".to_string()
        })
    );
    assert_eq!(c.said(&same), Asked::Today);
    assert_eq!(c.model(0), to(3, true), "the stated exception");
    c.typed(0, &same.action);
    c.h.step("UseD1");
    assert_eq!(c.turn_ends(0, None), None);
    let other = c.ask(0);
    assert_eq!((c.said(&other), c.model(0)), (to(1, true), to(1, true)));
    c.make(0, &other, "Move1");
    c.first_act(0, RULE_CONTINUE);
    assert_eq!(c.turn_ends(0, None), Some(AccountAnswer::Served));

    for (pin, account) in [("UseD2", 2), ("UseSh", 3)] {
        c.h.step(pin);
        let follow = c.ask(0);
        assert_eq!(
            (c.said(&follow), c.model(0)),
            (to(account, true), to(account, true)),
            "{pin}"
        );
        c.make(0, &follow, "Move1");
        c.first_act(0, RULE_CONTINUE);
        assert_eq!(c.turn_ends(0, None), Some(AccountAnswer::Served), "{pin}");
    }
    let (made, free) = c.te[0].account_moves_of(c.at());
    assert_eq!(made, ACCOUNT_MOVES_PER_WINDOW);
    assert!(
        c.h.now - NOW < accounts::ROTATION_WINDOW_S,
        "the history is one rotation hour"
    );
    c.h.step("UseD1");
    let fifth = c.ask(0);
    assert_eq!(
        fifth.reading.account,
        Some(AccountAdvice::Move {
            to: "main".to_string()
        })
    );
    assert_eq!(c.said(&fifth), Asked::Today);
    assert_eq!(c.model(0), to(1, true), "the stated exception");
    c.typed(0, &fifth.action);
    c.h.wall_s = 4 * HOUR;
    c.wall("WallSh", &[0]);
    let capped = c.ask(0);
    assert_eq!(c.said(&capped), Asked::Waits);
    assert_eq!(c.model(0), to(1, false), "the stated exception");
    let TurnEndAction::WaitUntil { until, .. } = &capped.action else {
        panic!("{:?}", capped.action);
    };
    assert_eq!(Some(*until), free);
    assert!(Some(*until) < waits_until(&capped.today));
    let lapsed = decide_turn_end(&c.te[0], &capped.reading, &c.cfg, *until);
    assert!(
        matches!(
            &lapsed,
            TurnEndAction::Restart {
                why: Restart::Account { to, at: AccountAt::Wall },
                ..
            } if to == "main"
        ),
        "{lapsed:?}"
    );
}

#[test]
fn the_first_turn_under_the_new_login_is_the_models_served_or_its_strike() {
    let mut c = Carried::begin(4 * HOUR);
    let to = |account: i64, ahead: bool| Asked::Move { to: account, ahead };
    c.wall("WallD1", &[0, 1]);
    let old = c.screen[0];
    let asked = c.ask(0);
    assert_eq!(c.said(&asked), to(2, false));
    c.make(0, &asked, "Move1");

    c.screen[0] = old;
    let repainted = c.shows(0, None);
    assert_eq!(c.te[0].account_answer(&repainted), None);
    c.screen[0] = None;

    c.first_act(0, RULE_LIMIT_RESUME);
    let reset = c.h.now + c.h.wall_s;
    assert_eq!(
        c.turn_ends(0, Some(reset)),
        Some(AccountAnswer::Bad(AccountBad::Wall))
    );
    c.h.step("Strike");
    assert_eq!(c.h.real.active, None, "`active` goes back");
    assert!(c.te[0].account_move().is_some());
    let next = c.ask(0);
    assert_eq!(
        next.point,
        Some(AccountPoint::Wall {
            kind: WallKind::UsageSession
        })
    );
    assert_eq!((c.said(&next), c.model(0)), (to(3, false), to(3, false)));
    c.make(0, &next, "Move1");

    let held = c.ask(1);
    assert_eq!((c.said(&held), c.model(1)), (Asked::Waits, Asked::Waits));
    c.h.step("Wait2");

    c.first_act(0, RULE_LIMIT_RESUME);
    assert_eq!(c.turn_ends(0, None), Some(AccountAnswer::Served));
    c.h.step("Served");
    assert!(c.te[0].account_move().is_none());
    let follow = c.ask(1);
    assert_eq!((c.said(&follow), c.model(1)), (to(3, false), to(3, false)));
    c.make(1, &follow, "Move2");
    c.first_act(1, RULE_LIMIT_RESUME);
    assert_eq!(c.turn_ends(1, None), Some(AccountAnswer::Served));
    assert_eq!(c.h.real.sessions_on("pool-a"), SIDS);
    assert_eq!((c.h.st["s1"], c.h.st["s2"], c.h.st["act"]), (3, 3, 3));
}
