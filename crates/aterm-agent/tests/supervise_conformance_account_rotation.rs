// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `SupervisorAccountRotation` (aterm-spec
//! `supervisor_account_rotation_model`, `docs/DESIGN-claude-accounts-2026-10-01.md` §6
//! and §10): the real `harness::accounts::advise` and its transactions
//! (`begin_rotation`, `take_canary`, `prove`, `strike`, `pin`, `unpin`, `note_wall`,
//! `note_sample`, `note_session`, `forget_session`), with the real `condition`, checked
//! against every reachable state of the model; and, fed that advice as a host hands it
//! over, the real turn-end decider (`decide_turn_end` over a real `TurnEndState`, with
//! its `account_point`, `observe`, `restarted`, `account_answer` and `account_told`).
//!
//! `accounts::State` is plain data, so the bind is a simulation, state by state:
//!
//! * The projection ([`project`]) builds the real state a model state stands for and is
//!   the soundness pin: read back ([`abstraction`]), it gives the state it was built
//!   from.
//! * The advice: the real `advise` for each session at its own point (and, on a limited
//!   account, at a plain turn end: `Leave…`) is the model's move, hold reason or stay
//!   ([`expected`]).
//! * The transactions: every enabled model action is mirrored through the real
//!   transaction ([`mirror`]) and the result reads back as the model's next state, on
//!   which the real `advise` moves the sessions as the model does. A reset or the
//!   hold's lapse is the real clock passing that one account's wall or the canary's
//!   ten minutes.
//! * Around a rotation and a take ([`around_a_rotation`], [`around_a_take`]): a refused
//!   relaunch is undone exactly, and the session that loses the compare-and-set is held.
//!
//! The decider: for each session at every state, one decision in the loop's order
//! ([`round`]) on a `TurnEndState` that has seen just that point, the walk playing the
//! host (aterm-gui's `account_advice`) through the policy's own mapping ([`mapped`]).
//! The verdict is what the model enables: a `Restart::Account` where it moves the
//! session, a `WaitUntil` no later than today's where it holds it ([`the_wait_ends`]),
//! the wall's continuation at once at a lifted wall's notice (`Resume…`,
//! [`the_owners_clearing`]), and exactly today's action elsewhere. Each move is then
//! made on the policy's side ([`a_move_made`]). The session's own bounds, which the
//! model lacks, are checked around every ask ([`around_an_ask`]) and along carried
//! histories ([`Carried`]); each only withholds a move the model enables.
//! `[harness] accounts = "wall"` is the control ([`under_wall_mode`]).
//!
//! The model's `jobs`, `w1`, `w2` are the host's; its notices (`n1`, `n2`) are what a
//! session's screen shows, played as the point asked about; `mv`, `pa`, `ph`, `pw` and
//! `cx` are the model's record of its last step, which nothing real carries. The real
//! code is handed only [`SHOWN`], so it is asked once per projection and edge
//! ([`Lane`]); the model's laws are read at every state.
//!
//! The walk runs twice, over two readings of "account 1 sorts before account 2" (by
//! label with no week known; by the week that resets soonest, labels reversed), which
//! are also the two readings of a lapsed hold (the canary's record ten minutes old; no
//! record). Both run at the committed `Budget`, which stands for the end of the real
//! hour. The histories ([`InStep`]) keep one real state, begun empty and changed only by
//! real transactions and a real clock, in step with the model.
//!
//! Not bound here: the 15-minute rules and a wall not yet on record (the unit tests');
//! what a strike puts back beside `active` (the model keeps one account to go back to;
//! the real `strike` restores what each rotation wrote over, `accounts::State::undo`; see
//! [`a_strike_goes_back_to_an_account_that_never_served_as_unproven`] and the unit test
//! `a_candidate_struck_past_an_unanswered_account_leaves_that_account_as_it_was`); and
//! what wakes a held session beyond the decider's `WaitUntil` (a ledger row reaches it
//! through `Session::account_edge`, which this walk does not drive).
//!
//! Negative controls: the `Buggy = 1` model disagrees with the real code at every firing
//! of each of its ten defects ([`DEFECTS`]), in the advice and in the decider fed it;
//! and today's rule disagrees at a wall with a usable account, both as a wall waited out
//! in place of `advise` and as a host that advises nothing under the real decider.

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
    AccountPoint, Composer, RULE_ACCOUNT, RULE_CONTINUE, RULE_LIMIT_RESUME, Restart, TurnEndAction,
    TurnEndReading, TurnEndState, TurnEndTiming, decide_turn_end,
};
use aterm_phase::WallKind;
use aterm_spec::derive::{Invariant, Model, eq, int, or_, supervisor_account_rotation_model, var};

use crate::common::cnst;

type State = BTreeMap<&'static str, i64>;

/// A fixed clock: never `now()`.
const NOW: i64 = 1_790_000_000;
const MIN: i64 = 60;
const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;

/// The limit type every wall here is recorded under.
const WINDOW: &str = "five_hour";

/// A wall the walk records names a reset this far ahead: far enough that no
/// account "becomes usable within 15 minutes".
const WALL_S: i64 = 4 * HOUR;

/// The account whose window is about to reset: its wall names a reset this
/// near, and its sample's window ends this near. It is `low` until that
/// instant, however near it is.
const SOON_S: i64 = MIN;

/// The clock's step at a reset: past the near wall's end (its reset plus the
/// 150 s grace), and so past the near window's end.
const RESET_STEP_S: i64 = SOON_S + accounts::RESET_GRACE_S + 1;

/// How long the canary has held when the model's hold stands: long enough
/// ago that the step below ends it, and recent enough that a reset's step
/// does not.
const HELD_S: i64 = accounts::CANARY_HOLD_S / 2;

/// The clock's step at the hold's lapse: to the end of its ten minutes.
const LAPSE_STEP_S: i64 = accounts::CANARY_HOLD_S - HELD_S;

// A window's reset must not also end a hold that stands.
const _: () = assert!(HELD_S + RESET_STEP_S < accounts::CANARY_HOLD_S);

/// A session at the notice of a wall the owner lifted (the model's `n1`, `n2`): how
/// long before [`NOW`] the limit row it stands at was written, and how long before
/// [`NOW`] the owner cleared that account, after the row.
const NOTICE_ROW_S: i64 = MIN / 2;
const CLEARED_S: i64 = MIN / 4;
const _: () = assert!(CLEARED_S < NOTICE_ROW_S);

/// The session id of a canary whose tab is gone: no session row carries it.
const GONE: &str = "s-gone";

/// The model's variables for the three accounts' conditions, the two
/// sessions' accounts and waits, and its actions for a session's point.
const CONDS: [&str; 3] = ["d1", "d2", "sh"];
const SESSIONS: [&str; 2] = ["s1", "s2"];
const WAITING: [&str; 2] = ["w1", "w2"];
const MOVES: [&str; 2] = ["Move1", "Move2"];
const LEAVES: [&str; 2] = ["Leave1", "Leave2"];
const WAITS: [&str; 2] = ["Wait1", "Wait2"];
const RESUMES: [&str; 2] = ["Resume1", "Resume2"];
/// The model's notice of a wall the owner lifted, per session.
const NOTICES: [&str; 2] = ["n1", "n2"];
const SIDS: [&str; 2] = ["s-one", "s-two"];

/// The model's variables the real state carries; the rest are the host's or the
/// model's record of its last step.
const SEEN: [&str; 13] = [
    "d1", "d2", "sh", "act", "run", "prov", "prev", "pp", "pin", "hold", "s1", "s2", "rot",
];

/// The model's ten defects.
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

/// One reading of the model's three accounts as roster rows.
struct World {
    roster: Roster,
    enrolled: BTreeSet<String>,
    /// The label of the model's account 1, 2 and 3.
    labels: [&'static str; 3],
    /// Days until each account's week resets, where the samples carry one.
    /// `None`: no week is known, and a fresh account no session is on has
    /// never been observed.
    weeks: Option<[i64; 3]>,
    /// How a lapsed hold is read: the canary's record still there, ten minutes old
    /// (`true`), or none, its session forgotten (`false`).
    silent_record: bool,
}

impl World {
    /// The design's own roster: this machine's login, a second dedicated
    /// account and a shared one.
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

    /// Account 1 before account 2 by the key's last term, the label.
    fn by_label() -> World {
        World::of(["main", "work-2", "pool-a"], None, true)
    }

    /// Account 1 before account 2 because its week resets sooner — with the
    /// labels the other way round, and the shared account's week the soonest
    /// of all (its class still puts it last).
    fn by_week() -> World {
        World::of(["work-2", "main", "pool-a"], Some([2, 5, 1]), false)
    }

    /// The label of a model account; `None` for 0 (no account).
    fn label(&self, account: i64) -> Option<&'static str> {
        usize::try_from(account - 1)
            .ok()
            .and_then(|k| self.labels.get(k).copied())
    }

    fn named(&self, account: i64) -> &'static str {
        self.label(account)
            .unwrap_or_else(|| panic!("no account {account}"))
    }

    /// The model's number for a label; 0 for none.
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

    /// The rotations the real bound allows in an hour (twice the roster).
    fn bound(&self) -> i64 {
        i64::try_from(self.roster.rows.len()).unwrap() * 2
    }
}

/// The rotations already spent when the model's hour opens: the real bound less the
/// model's `Budget` (0 when the model is walked at the real bound).
fn spent_before(w: &World, m: &Model) -> i64 {
    let before = w.bound() - cnst(m, "Budget");
    assert!(before >= 0, "the model's Budget is past the real bound");
    before
}

/// A settled `/usage` read: low or not, in a session window ending at
/// `window_end`, with this world's week.
fn sample(w: &World, account: i64, low: bool, window_end: i64, at: i64) -> Sample {
    let k = usize::try_from(account - 1).unwrap();
    Sample {
        session_pct: Some(if low { 95 } else { 10 }),
        session_reset: Some(window_end),
        week_pct: Some(40),
        week_reset: w.weeks.map(|days| NOW + days[k] * DAY),
        at,
        by: String::new(),
    }
}

/// The projection: the real state the model state `st` stands for, at [`NOW`].
/// `soon` names the account whose window is about to reset.
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
    // The owner's word for the account a session stands at the notice of: its
    // walls were cleared a moment after the row that session stands at was
    // written (the real `pin` keeps the instant; the model's `Use…` is what
    // set the notice).
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
    // The canary of an account that has not answered: a session on it where
    // there is one, else one whose tab is gone. Its hold stands, or lapsed
    // ten minutes ago, or (this world's other reading of a lapse) nothing is
    // on record.
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

/// The read back: the real state onto the model's variables, through the real
/// `condition` at `now`.
fn abstraction(w: &World, m: &Model, real: &accounts::State, now: i64) -> State {
    let mut out = State::new();
    for (var, account) in CONDS.iter().zip(1..) {
        let cond = accounts::condition(w.row(w.named(account)), real, true, now);
        let value = match cond {
            Condition::Usable | Condition::Unseen => 0,
            Condition::Low => 1,
            Condition::Limited { .. } => 2,
            other => panic!("a condition the model has no value for: {other:?}"),
        };
        out.insert(*var, value);
    }
    let active = real.active.as_deref();
    let act = w.account(active);
    out.insert("act", act);
    // What is read of `active`: nothing to run on, low, or fresh.
    let reading = match active.map(|label| accounts::condition(w.row(label), real, true, now)) {
        Some(cond) if cond.fresh() => 2,
        Some(Condition::Low) => 1,
        _ => 0,
    };
    out.insert("run", reading);
    out.insert("prov", i64::from(real.proven));
    // Where a strike goes back to, and whether that account had served: read
    // only while `active` has not (the model forgets both once it has).
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

/// The model state's variables the real state carries.
fn seen(st: &State) -> State {
    SEEN.iter().map(|var| (*var, st[var])).collect()
}

/// `read == seen(st)` without building [`seen`]'s map (asked at every state and edge):
/// `read` names exactly the [`SEEN`] variables, each with `st`'s value.
fn read_is_seen(read: &State, st: &State) -> bool {
    read.len() == SEEN.len() && SEEN.iter().all(|var| read.get(var) == Some(&st[var]))
}

/// The model variables the real code is shown: [`SEEN`], which [`project`] builds the
/// real state from, and the notices, which say what point a session is asked at. The
/// real code answers the same at every model state that agrees on these.
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

/// A model state's [`SHOWN`] variables, in that order.
type Shown = [i64; SHOWN.len()];

fn shown(st: &State) -> Shown {
    SHOWN.map(|var| st[var])
}

/// The model state of just those variables: what the real code is handed, so a read
/// of any other is a missing key and fails loudly instead of making the memo wrong.
/// Built in its keys' order ([`SHOWN_SORTED`]), so collecting it sorts nothing.
fn shown_state(key: &Shown) -> State {
    SHOWN_SORTED.iter().map(|&k| (SHOWN[k], key[k])).collect()
}

/// The indices of [`SHOWN`] in the order of its names.
static SHOWN_SORTED: std::sync::LazyLock<[usize; SHOWN.len()]> = std::sync::LazyLock::new(|| {
    let mut order: [usize; SHOWN.len()] = std::array::from_fn(|k| k);
    order.sort_by_key(|&k| SHOWN[k]);
    order
});

/// A whole model state, in the variables' order: the key a walk visits and
/// memoises the model's own answers by.
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

/// The real advice for session `i`, and whether the point it was asked at is
/// its wall.
type Advisor = fn(&World, &accounts::State, usize, Ask, i64) -> (Advice, bool);

fn at_its_wall(w: &World, real: &accounts::State, i: usize, now: i64) -> bool {
    let mine = real.sessions[SIDS[i]].account.as_str();
    matches!(
        accounts::condition(w.row(mine), real, true, now),
        Condition::Limited { .. }
    )
}

/// The point the model has session `i` at in `st`, as it is asked about: the
/// notice of a wall the owner lifted, or its own point.
fn ask_of(st: &State, i: usize) -> Ask {
    if st[NOTICES[i]] == 1 {
        Ask::Notice
    } else {
        Ask::ItsPoint
    }
}

/// Whether session `i`, asked at `ask`, stands at a wall — and, at the notice
/// of one the owner lifted, when the row it stands at was written.
fn wall_of(w: &World, real: &accounts::State, i: usize, ask: Ask, now: i64) -> (bool, Option<i64>) {
    match ask {
        Ask::Notice => (true, Some(NOW - NOTICE_ROW_S)),
        Ask::ItsPoint => (at_its_wall(w, real, i, now), None),
        Ask::TurnEnd => (false, None),
    }
}

/// The roster's two askers, which share one signature: `accounts::advise`
/// and `accounts::advise_past_probe`.
type Asker = fn(&Roster, &accounts::State, &BTreeSet<String>, &Me<'_>, &Point, Mode, i64) -> Advice;

/// `asker` for session `i`, at its wall or at a plain turn end, under `mode`.
/// `written`: when the limit row it stands at was written, dated only at the notice
/// of a wall the owner lifted ([`Ask::Notice`]). Every other wall is asked about
/// undated: the model's walls are the ones on record, and an undated row is never
/// read as cleared.
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

/// The real `accounts::advise` for session `i` ([`asked_at`]).
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

/// The real `accounts::advise_past_probe` for session `i` ([`asked_at`]): what a
/// host hands on beside a due probe, and what its commit re-asks.
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

/// The real `accounts::advise` at `ask`, under `mode`.
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

/// The real `accounts::advise`, under the model's mode (`early`).
fn advise(w: &World, real: &accounts::State, i: usize, ask: Ask, now: i64) -> (Advice, bool) {
    advise_under(w, real, i, ask, Mode::Early, now)
}

/// Today's rule (a negative control): with no roster a usage wall is waited out, and
/// nothing else is done.
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

/// Whether session `i` is at its wall in the model state.
fn walled(st: &State, i: usize) -> bool {
    let mine = usize::try_from(st[SESSIONS[i]] - 1).unwrap();
    st[CONDS[mine]] == 2
}

/// `st` with the host free to relaunch session `i`, and the session awake.
fn freed(st: &State, i: usize) -> State {
    let mut free = st.clone();
    free.insert(WAITING[i], 0);
    if i == 0 {
        free.insert("jobs", 0);
    }
    free
}

/// The move the model's `action` makes for session `i` from `free`, where it
/// is enabled: a rotation where the model counts one, a take of the canary's
/// place where the account moved onto has not served a response.
fn the_models_move(
    m: &Model,
    action: &str,
    free: &State,
    i: usize,
    ahead: bool,
) -> Option<Verdict> {
    let mut moved = free.clone();
    let rotates = |moved: &State| moved["rot"] != free["rot"];
    m.fire(action, &mut moved).then(|| Verdict::Move {
        to: moved[SESSIONS[i]],
        rotates: rotates(&moved),
        takes: !rotates(&moved) && free["prov"] == 0,
        ahead,
    })
}

/// What the model tells session `i` at `st`, with the host free and awake: read off
/// its own guards (`Move…`, `Wait…`), never restated here.
fn expected(m: &Model, st: &State, i: usize) -> Verdict {
    let free = freed(st, i);
    if m.action_enabled(RESUMES[i], &free) {
        return Verdict::Resume;
    }
    let at_wall = walled(st, i);
    if let Some(moved) = the_models_move(m, MOVES[i], &free, i, !at_wall) {
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
    // The breaker is the reason exactly where an unspent hour would move it.
    let mut unspent = free.clone();
    unspent.insert("rot", 0);
    if m.action_enabled(MOVES[i], &unspent) {
        Verdict::Hold(Hold::Breaker)
    } else {
        Verdict::Hold(Hold::NothingUsable)
    }
}

/// What the model tells session `i` at a plain turn end on a limited account
/// (`Leave…`): the move, or a stay.
fn expected_leaving(m: &Model, st: &State, i: usize) -> Verdict {
    the_models_move(m, LEAVES[i], &freed(st, i), i, true).unwrap_or(Verdict::Stay)
}

/// The same move, or no move on either side. What is compared on a real
/// state the projection did not build: there a session may be on an account
/// with no sample, and its advice at a plain turn end is the probe, which
/// the model does not have and which moves nobody.
fn the_move(v: &Verdict) -> Option<&Verdict> {
    matches!(v, Verdict::Move { .. }).then_some(v)
}

/// What a model action decides for a session, where it is a session's decision: the
/// supervisor's own steps, and the defects that stand in for them.
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

/// The session a model action decides for, at which point, and what. A
/// defect that joins the active account is the herd's follow, never a take.
/// `Ask::ItsPoint` is the session's own point, which the walk asks about as
/// the model has it there ([`ask_of`]: its wall, a plain turn end, or the
/// notice of a wall the owner lifted).
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

/// Whether the real advice is the model's decision. A wait with the agent's
/// jobs running is the host's refusal of whatever was advised.
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

// ---------------------------------------------------------------------------
// The decider: `decide_turn_end` over a real `TurnEndState`
// ---------------------------------------------------------------------------

/// The busy work of the turn that ended at a point: real work, so no
/// short-turn back-off stands between a plain turn end and its continuation.
const REAL_WORK: Duration = Duration::from_secs(30 * 60);

/// The notice a session at its wall shows: the session window's, as
/// [`WINDOW`] is.
const NOTICE: &str = "You've hit your session limit · resets 3pm";

/// The supervisor's two clocks as one: the `Instant` that is the unix second [`NOW`],
/// read once a test, so every decision is made on a fixed clock.
#[derive(Debug, Clone, Copy)]
struct Clock(Instant);

impl Clock {
    fn start() -> Clock {
        Clock(Instant::now() + Duration::from_secs(10 * 3600))
    }

    /// The instant of the unix second `unix`, at or after [`NOW`].
    fn at(self, unix: i64) -> Instant {
        let ahead = u64::try_from(unix - NOW).expect("a second at or after NOW");
        self.0 + Duration::from_secs(ahead)
    }
}

/// A plain turn end as the loop reads it: idle, an empty composer, a task,
/// in a session whose host relaunches it. `worked`: the turn's busy work, or
/// `None` for the same point read again.
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
        worked,
        rules: None,
        pending_input: false,
        interrupted: false,
        restartable: true,
        resume: None,
        upgrading: false,
        taskless: false,
        login_back: false,
        auto_continue: None,
        since_limit: None,
        served_in_transcript: false,
        reset_clock: None,
        person: None,
        reach: Default::default(),
        program: aterm_phase::Program::Claude,
        goal: None,
        model_field: None,
        limits: Default::default(),
        sandbox_fell: None,
        thread_model: None,
        upgrade_goal: false,
        account: None,
        autopilot: aterm_agent::supervise::Autopilot::Cruise,
        login_label: None,
        login_superseded: false,
        login_notice: None,
    }
}

/// A turn that ended on the session window's limit, whose notice names
/// `reset`.
fn limit(reset: Instant, worked: Option<Duration>) -> TurnEndReading {
    TurnEndReading {
        wall: Some(WallKind::UsageSession),
        wall_message: NOTICE.to_string(),
        reset_at: Some(reset),
        ..turn_end(worked)
    }
}

/// `reading` with no advice.
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
    /// or the hour, whole. The control for the wait that ends at whatever comes first.
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

/// The roster's advice as the policy takes it, built as a window's host builds it:
/// `advice` (what the roster answered at `place`), the roster's real next opening,
/// and, only where `advice` is the probe, what the roster advises past it
/// (`accounts::advise_past_probe` on the same state, point, mode and second), all
/// through [`AccountAdvice::of_roster_at`]. So a due probe is handed over as
/// `Probe { otherwise }`, never as nothing with a move behind it lost.
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

/// The host asked about `point` for session `i` (aterm-gui's `account_advice`): the
/// real `advise` under `mode` at the loop's point (a turn end, or a usage limit the
/// host takes for the roster's only where it is the session's process's own; no
/// session here shows a limit another process wrote), through the policy's own
/// mapping.
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

/// The loop's order, which is part of what is bound: the point is folded in; the host
/// is asked where, and only where, the policy has a point for it
/// (`TurnEndState::account_point`), on the state after the fold; its answer goes on
/// the reading; the decision is made on both.
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

/// What the model has the decider ask, read off what it tells the session
/// ([`expected`], [`expected_leaving`]).
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

/// The same move asked, or no move asked on either side ([`the_move`]'s
/// reason: the probe).
fn the_ask(a: &Asked) -> Option<&Asked> {
    matches!(a, Asked::Move { .. }).then_some(a)
}

/// The instant a wait ends, where the action is one.
fn waits_until(action: &TurnEndAction) -> Option<Instant> {
    match action {
        TurnEndAction::WaitUntil { until, .. } => Some(*until),
        _ => None,
    }
}

/// A session's decision at one point of one real state.
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
            // The model's mode: every advisor of the walk answers under
            // `early` (the real `advise`), or is today's rule, which never
            // answers the probe.
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
    // The policy has a point for its host at every point of the model's, and
    // it is the model's: the session's wall, or a plain turn end.
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

/// Whether what the real decider asks is the model's decision. A wait with the
/// agent's jobs running is the host's `wait:changed` to whatever was asked: the loop
/// holds the point and records nothing.
fn asks(decided: &Decision, real: &Asked, jobs_hold: bool) -> bool {
    match decided {
        Decision::Move { to, .. } => matches!(real, Asked::Move { to: t, .. } if t == to),
        Decision::Waits => jobs_hold || *real == Asked::Waits,
        Decision::Resumes => *real == Asked::Resume,
    }
}

/// The wait's instant, by the model's reason for it: `Ok` where the real decider's
/// wait ends when that reason does. `d` is the decision at a wall the model holds the
/// session at for `why`.
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
        // The canary's ten minutes: the instant the model's `Lapse` is.
        Hold::Canary => clock.at(NOW + LAPSE_STEP_S),
        // The hour has room again: no step of the model's (its hour does not
        // lapse), and sooner than the wall's own reset.
        Hold::Breaker => {
            let room = accounts::breaker_until(real, &w.roster, NOW)
                .ok_or("the hour is not spent on the real side")?;
            clock.at(room)
        }
        // Every account is limited: today's instant, the session's own reset
        // (every wall here ends together).
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

/// The session a model action types a continuation of a move's own into, and
/// where that move was made: `Resumed…`'s at the session's wall, the
/// defect's ahead of one.
fn typed_after_a_move(action: &str) -> Option<(usize, AccountAt)> {
    match action {
        "Resumed1" => Some((0, AccountAt::Wall)),
        "Resumed2" => Some((1, AccountAt::Wall)),
        "CarriesOnAfterAnEarlyMove" => Some((0, AccountAt::Ahead)),
        _ => None,
    }
}

/// Whether the real code types a continuation of the move's own into a
/// session just relaunched under the account labelled `to` by a move made
/// `at`, at `now`: the relaunch's last step (`relaunch::carried_quietly` on
/// the record's cause, `account:<label>`), or the policy's (`restarted`, and
/// the wall's resume owed then).
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
/// relaunch (`restarted(.., true, ..)`). `after` is the accounts state the move's
/// transaction left, at `now`; `want` what the model tells the session after it.
///
/// The move is remembered until answered, and counted; the wall it left is behind the
/// session; nothing is told or asked at the relaunched prompt; the relaunch types
/// nothing; the point's act is the wall's resume after a move at a wall, and after one
/// ahead of a wall exactly what the decider gives the same point with no move; that
/// act, answered with work, is `Served`, and the session is then decided as the
/// model's next state says.
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
    // The relaunched process at its prompt, no turn asked of it yet.
    let prompt = turn_end(None);
    if te.account_answer(&prompt).is_some() {
        return Err("the host is told before any turn ran".to_string());
    }
    te.observe(&prompt, at);
    if te.account_point(&prompt, cfg).is_some() {
        return Err("the host is asked before any turn ran".to_string());
    }
    // The relaunch types nothing for an account move's record (cause
    // `account:<label>`), quiet or not. Negative control: the memory banner's
    // relaunch says why it came back.
    if !relaunch::carried_quietly(&format!("account:{to}"), false)
        || relaunch::carried_quietly("memory", false)
    {
        return Err("the relaunch of an account move types a continuation".to_string());
    }
    // The wall's resume: a move made at a wall owes the continuation that wall would
    // have had at its reset (the limit-resume rule); one made ahead of a wall owes
    // none, and the point's act is the continue policy's.
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
    // Ahead of a wall the move owes nothing: the point gets what the same session
    // gets there with no move.
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
    // The point's own act is typed, and pays the wall's resume: the first
    // turn asked under the new login, ending with work and no wall.
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
/// on record in, `after` the one the pin left, at `now`). `d` is the session's
/// decision at that wall, so its policy has seen the point.
///
/// Asked about the row it stands at (written before the clearing), the real `advise`
/// says `Resume`, and the real decider types that wall's continuation at once by the
/// limit-resume rule, where with no advice it waits for the reset; once only: the
/// limit met again under the same clearing gets the wall's own ladder. Negative
/// control: the same wall with no dated row is not read as cleared.
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
            // At a wall the roster never answers the probe: the place
            // asked is carried for the mapping's sake alone.
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
        text: cfg.continue_text.clone(),
        rule_id: RULE_LIMIT_RESUME,
    };
    if first.action != continued || waits_until(&first.today).is_none() {
        return Err(format!(
            "the cleared wall is not continued at once: {:?} (today: {:?})",
            first.action, first.today
        ));
    }
    // Once a clearing: typed, met by the limit again, advised the same.
    te.acted(&first.action, &first.reading, at);
    let again = decided_on(&mut te, &advice, Some(Duration::from_secs(3)));
    if again.action != again.today || waits_until(&again.action).is_none() {
        return Err(format!(
            "the same clearing is answered twice: {:?}",
            again.action
        ));
    }
    // Negative control: no dated row, no clearing read.
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

/// Around an ask: the session's own bounds, which no model state carries, tried on
/// the decision `d` wherever it asks for a move. Each withholds the move and gives
/// today's action, never another move, and each has its negative control beside it.
/// The latch's other ends (a new wall of the same class after real work, the
/// accounts' news reaching the session) are the unit tests'.
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

    // `wait:changed` (the agent's jobs): the loop records nothing and decides again
    // on its back-off: the same ask, and no latch.
    assert!(!d.te.account_refused(), "{st:?}");
    assert_eq!(decide(&d.te, reading, at), d.round.action, "{st:?}");

    // A word that is no wait (`refused:line`, `failed:…`).
    let mut refused = d.te.clone();
    refused.restarted(why, false, reading, at);
    match asked_at {
        AccountAt::Wall => {
            tried.at_a_wall += 1;
            // The latch: the host is not asked again, and advice handed over all the
            // same decides nothing, while the wall stands (read a minute and an hour on).
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
            // The same ahead of a wall, by label: no wall, so no latch, and the same
            // advice decides nothing; advice naming another account is a new ask.
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

    // The cap: the session's own moves of the last hour, a minute old.
    let ages = [Duration::from_secs(60); ACCOUNT_MOVES_PER_WINDOW];
    let mut spent = d.te.clone();
    spent.seed_account_moves(&ages, at);
    let capped = decide(&spent, reading, at);
    match asked_at {
        AccountAt::Wall => {
            // Waited out, and said: until the oldest move leaves the hour,
            // which is sooner than the wall's own reset.
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
    // Negative control: one move fewer in the hour, and it is asked.
    let mut room = d.te.clone();
    room.seed_account_moves(&ages[1..], at);
    assert_eq!(decide(&room, reading, at), d.round.action, "{st:?}");
}

/// The `wall` mode, the control (`[harness] accounts = "wall"`: move only at the
/// wall, no rotation ahead of a limit, no probe). Under it the advice for session `i`
/// is the model's own at its wall and for every move onto `active`, and a stay
/// (`nothing-due`, so today's decision) everywhere else at a plain turn end. `true`
/// where that withholds a rotation the model (which is `early`) makes ahead of a wall.
///
/// At every ask, under either mode, the roster does not answer the probe and asked
/// past the probe answers the same, so the advice the walk hands over is the roster's
/// whole answer at every state.
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
    // …so the reading carries no advice, and the decision on it is the one
    // already made as today's.
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

/// The model account a `…D1` / `…D2` / `…Sh` action is about.
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

/// Mirror one model action onto the real state through the real transaction.
/// `wall_s` is how far ahead a recorded wall names its reset. A window's reset and
/// the hold's lapse are the caller's: they are the clock. `Err` is the real code
/// refusing what the model did.
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
        // The host's: no part of the accounts state. A session continued
        // at the notice of a wall the owner lifted commits nothing, and nor
        // does a continuation typed after a move.
        "JobsStart"
        | "JobsEnd"
        | "Wait1"
        | "Wait2"
        | "Resume1"
        | "Resume2"
        | "Resumed1"
        | "Resumed2"
        | "CarriesOnAfterAnEarlyMove" => {}
        // The clock's: the caller moved it.
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
            // A wall is a word about the account, from whichever session on
            // it heard it.
            let heard = real.sessions_on(&active).first().map_or(GONE, |sid| *sid);
            let heard = heard.to_string();
            if accounts::strike(real, &active, &heard, &why, now).is_empty() {
                return Err("nothing was struck".to_string());
            }
        }
        // A session's move: the rotation's compare-and-set, the take of a
        // silent canary's place, or a follow, which commits nothing.
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
        // The world's one defect: the wall is real, and the real recorder reads it.
        "WallUnread" => {
            accounts::note_wall(real, w.named(2), WINDOW, until, now, now);
        }
        // The supervisor's defects are not the real code's moves: the real
        // side does nothing where they act.
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

    /// What the real code found at a projection of the model state (see
    /// [`Lane`]) — its counts and the places it parted from the model —
    /// found again at `state`, one of the model states that share it. Where
    /// `strict` the first disagreement fails the walk, at that state.
    fn add(&mut self, found: &Walk, strict: bool, state: &Arc<State>) {
        self.moves += found.moves;
        self.waits += found.waits;
        self.todays += found.todays;
        self.resumes += found.resumes;
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

/// A rotation the real advice asks of session `i`, tried and taken back, and
/// raced by the other session. Asked only of the real `advise`, on a state
/// the model holds sound.
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

    // The host refuses the relaunch (the agent's jobs, a line that cannot carry the
    // login): the rotation is taken back and the real state is the one it was made
    // on, whole, even past an account that had not served (its own `previous`, proof
    // and canary's record are back with it). Only the word of the session that was
    // to move undoes it; from any other it is no step and no row.
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

    // The other session read the same state and was advised a rotation too:
    // it loses the compare-and-set, nothing changes, and advised again it is
    // held until the account the winner chose has served a response.
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

/// A take of the canary's place the real advice asks of session `i`, tried
/// and given up, and raced by the other session. `active` is never put back:
/// the take is no rotation, counts toward no bound, and its undo is the hold
/// given up.
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

    // The host refuses the relaunch: the taker gives up its hold, and the
    // rotation it did not make stands. The place is open again at once.
    let mut refused = base.clone();
    accounts::take_canary(&mut refused, read.as_ref(), SIDS[i], NOW)
        .unwrap_or_else(|| panic!("the advised take lost at {st:?}"));
    assert_eq!(refused.canary_since(), Some(NOW), "{st:?}");
    assert_eq!(
        (&refused.active, &refused.rotations),
        (&base.active, &base.rotations),
        "a take rotates nothing: {st:?}"
    );
    // That word is the taker's alone: from the session whose place it took
    // (the rotation's maker, its relaunch refused only now) and from the
    // other one it is no step, and the taker's hold stands.
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

    // The other session read the same record and was advised the take too:
    // it loses the compare-and-set on the canary's record — at once, and
    // however late it comes with what it read, the winner's own hold long
    // lapsed — nothing changes, and advised again it is held by the session
    // that took the place.
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

/// The real code, as the walk asks it: `advisor` answers for the roster (the real
/// `advise`, or today's rule in its place) and `host` is whether the session's host
/// hands that answer to the decider at all. It is shown only the [`SHOWN`] variables,
/// so it is asked once per projection ([`AtState`]) and once per edge
/// ([`Lane::at_edge`]), and the answer is added at each model state and edge that has
/// it. The model's own answers ([`Graph`]) are asked at every model state.
struct Lane<'a> {
    w: &'a World,
    m: &'a Model,
    advisor: Advisor,
    host: Host,
    cfg: SupervisorConfig,
    clock: Clock,
}

/// What the real code finds at one projection of a model state.
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

    /// The real code at the projection `st`. A disagreement is collected here
    /// (`note(false, ..)`) and fails a strict walk where it is added.
    fn at_state(&self, st: State) -> AtState {
        let (w, m, advisor) = (self.w, self.m, self.advisor);
        let st = Arc::new(st);
        let mut found = Walk::default();
        let base = project(w, m, &st, None);
        found.note(
            false,
            read_is_seen(&abstraction(w, m, &base, NOW), &st),
            &st,
            None,
            || "the projection does not read back".to_string(),
        );
        let own = [0, 1].map(|i| ask_of(&st, i));
        let told = [0, 1].map(|i| verdict(w, &advisor(w, &base, i, own[i], NOW), NOW));
        // A session on a limited account, asked at a plain turn end: the wall is
        // one another tab recorded (`Leave…`). On any other account the plain turn
        // end is its own point, asked above.
        let told_leaving =
            [0, 1].map(|i| verdict(w, &advisor(w, &base, i, Ask::TurnEnd, NOW), NOW));
        // The decider, fed that advice as a host hands it over.
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
        // …and at a plain turn end on a limited account (`Leave…`).
        let decided_leaving =
            [0, 1].map(|i| walled(&st, i).then(|| self.decide_at(&base, i, Ask::TurnEnd, NOW)));
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

    /// What the real code is tried around at a state, which only a strict walk
    /// does. A failure names the projection, which is every model state it
    /// failed at.
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

    /// The real code on an edge from the projection `here`: what the action decides
    /// for a session is what that session was advised at this state, at the point the
    /// action stands for, and what it does to the state is what the real transaction
    /// does. Disagreements are collected ([`Lane::at_state`]).
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
        // The session whose move the action makes, and its decision.
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
            // …and what the real decider asked there.
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
        // What is typed after a move (the model's `cx`): a continuation of the
        // move's own goes into the session it relaunched only where the real code
        // types one. The relaunch for an account move's record never does, and the
        // policy owes the wall's resume only after a move made at a wall.
        // `Resumed…` follows a move made at a wall; the defect, one made ahead.
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
        // What the action does to the state is what the real transaction
        // does. A reset is the clock passing that one account's window;
        // the lapse is the clock passing the canary's ten minutes.
        let (mut real, now) = if name.starts_with("Reset") {
            // Until its window resets the account is what it was, however
            // near the reset is: low to the last minute, never fresh.
            let near = project(w, m, st, Some(account_of(name)));
            let read = abstraction(w, m, &near, NOW);
            out.note(false, read_is_seen(&read, st), st, Some(name), || {
                format!("about to reset, the real state reads back {read:?}")
            });
            // A session with nothing usable is decided again by the
            // instant this reset wakes the model's: its wait ends when
            // the window does, its own or another account's.
            // …and so is a session held for the canary or the breaker:
            // its wait ends at whatever comes first, and this reset comes
            // before the hold's end and the hour's.
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
                // Negative control: the same advice with no opening.
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
                // `next` is the model's state as the real code is shown it, so a
                // row names those variables alone; an edge's memo is shared by the
                // model states that differ only in the others.
                out.note(false, read_is_seen(&read, next), st, Some(name), || {
                    format!("the real state reads back {read:?}, the model {next:?}")
                });
                if let Some(after) = &edge.after {
                    // …and the state the real code reached moves the
                    // sessions as the model's next state does.
                    for (i, want) in after.iter().enumerate() {
                        let ask = ask_of(next, i);
                        let got = verdict(w, &advisor(w, &real, i, ask, now), now);
                        // The same move, or none on either side — and
                        // the same continuation at a lifted notice.
                        let same = the_move(&got) == the_move(want)
                            && (got == Verdict::Resume) == (*want == Verdict::Resume);
                        out.note(false, same, st, Some(name), || {
                            format!("after it, session {}: {got:?}, the model {want:?}", i + 1)
                        });
                    }
                    // The move made on the policy's side too, served, and
                    // the session decided at the state the model reached.
                    if let Some((i, d)) = made {
                        out.made += 1;
                        let done = a_move_made(w, i, d, &real, &after[i], now, *clock, cfg);
                        out.note(false, done.is_ok(), st, Some(name), || {
                            format!("session {}, decided: {}", i + 1, done.clone().unwrap_err())
                        });
                    }
                    // The owner's word for the account a session stands at the
                    // wall of: the model's session stands at its notice from here
                    // and is continued there (`Resume…`); the real one is continued
                    // at once, once for that clearing, and only at a row written
                    // before it.
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
        // The hold also ends, at once, when the canary's agent does.
        if name == "Lapse" {
            let mut ended = base.clone();
            let canary = base.canary.as_ref().map(|c| c.sid.clone());
            let forgot = canary.is_some_and(|sid| accounts::forget_session(&mut ended, &sid));
            // Forgetting it takes its session row too: that is put back,
            // so the rest can be read.
            ended.sessions.clone_from(&base.sessions);
            let read = abstraction(w, m, &ended, NOW);
            let same = forgot && read_is_seen(&read, next);
            out.note(false, same, st, Some(name), || {
                format!("the canary forgotten, the real state reads back {read:?}")
            });
        }
        out
    }
}

/// The model's half of a walk: every state of `m` the sound rule reaches, in the
/// walk's order (breadth first from `Init`), what the model tells both sessions at
/// each, and each step it takes from each. No world, advisor or host changes any of
/// it, so it is built once per model and walked by every lane ([`sound`]).
///
/// A defect's step (`Buggy = 1`) is taken from every such state and judged there, but
/// the walk does not go on from the state it reaches: that is no state of the real
/// code's.
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

/// One state of a [`Graph`].
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

/// A map key and its hash, computed where the graph's work is spread over the cores,
/// so the one-at-a-time merge only probes (hashing dominated building the graph).
/// `DefaultHasher::new` is the same hasher on every thread, so equal keys hash equal
/// wherever they were hashed.
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

/// One state as the graph's breadth-first search expands it ([`expand`]).
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

/// The state a [`Step`] reaches.
enum Reaches {
    /// A defect's, which the walk does not go on from: as the real code is
    /// shown it.
    Defect(Box<Shown>),
    /// A state reached before: its index.
    Reached(usize),
    /// One not reached before its breadth was expanded, and its key ([`flat`]).
    New(Box<(State, Hashed<Vec<i64>>)>),
}

/// The model at `st`: its laws, and its first successor by each action — by
/// its index, where it is among the states `reached` so far.
fn expand(m: &Model, st: &State, reached: &HashedMap<Vec<i64>, usize>) -> Expansion {
    let env = m.eval_env(st);
    let broken = m
        .invariants
        .iter()
        .find(|inv| !m.check_invariant_in(inv, &env))
        .map(|inv| inv.name);
    let steps = m
        .actions
        .iter()
        .filter_map(|action| {
            let next = m.successors_in(action, &env, st).into_iter().next()?;
            let jobs_hold =
                st["jobs"] == 1 && decision(action.name, st, &next).is_some_and(|(i, ..)| i == 0);
            let reaches = if DEFECTS.contains(&action.name) {
                Reaches::Defect(Box::new(shown(&next)))
            } else {
                let key = Hashed::of(flat(&next));
                match reached.get(&key) {
                    Some(&k) => Reaches::Reached(k),
                    None => Reaches::New(Box::new((next, key))),
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
    /// The graph of `m`. `strict`: for strict walks alone, which stop at the first
    /// state where a law of the model's does not hold, so the graph stops there too,
    /// that state its last node.
    ///
    /// Built level by level: the states of one breadth are expanded at once over the
    /// cores ([`pooled`]) and merged in the order the one-at-a-time search pops them,
    /// so every state, edge and projection has the index that search gives it. The
    /// model's answers and each state's edges are then made the same way, the edges a
    /// few thousand states at a time; a step carries its state only when it is new,
    /// which bounds the peak memory.
    fn of(m: Model, strict: bool) -> Graph {
        // Every state reached, in the order first reached, which is the
        // order popped; and each one's index.
        let init = m.init_state();
        let mut reached: HashedMap<Vec<i64>, usize> = HashedMap::default();
        reached.insert(Hashed::of(flat(&init)), 0);
        let mut states = vec![Arc::new(init)];
        // Each popped state's broken law and steps, every step's state by its
        // index.
        let mut popped: Vec<(Option<&'static str>, Vec<Step>)> = Vec::new();
        'search: while popped.len() < states.len() {
            let level = popped.len()..states.len();
            let expansions = pooled(level.len(), |k| {
                Some(expand(&m, &states[level.start + k], &reached))
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
        // Every state reached as the real code is shown it; what the model
        // tells both sessions there ([`expected`]), and at a plain turn end on
        // a limited account ([`expected_leaving`]).
        let shown_at: Vec<_> = pooled(states.len(), |k| {
            let st = &states[k];
            Some((
                shown(st),
                [0, 1].map(|i| expected(&m, st, i)),
                [0, 1].map(|i| walled(st, i).then(|| expected_leaving(&m, st, i))),
            ))
        })
        .into_iter()
        .map(Slot::into_value)
        .collect();
        let mut projections: HashedMap<Shown, usize> = HashedMap::default();
        // Each edge: its index, and the projection it leaves.
        let mut edge_ids: HashedMap<Edge, (usize, usize)> = HashedMap::default();
        let mut nodes = Vec::with_capacity(popped.len());
        for chunk in (0..popped.len()).step_by(4096) {
            let chunk = chunk..popped.len().min(chunk + 4096);
            // Each popped state's projection and edges, as the walk keys them.
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

/// The graph of the committed model, built once a process and walked by every lane
/// that asks about it (the Tier-1 walk's two readings and today's rule). It stays for
/// the rest of the suite's process: some 34,000 nodes and 107,000 edges, ~165 MB.
fn sound() -> &'static Graph {
    static SOUND: OnceLock<Graph> = OnceLock::new();
    SOUND.get_or_init(|| Graph::of(supervisor_account_rotation_model(), false))
}

/// What a [`pooled`] job left at its index.
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

    /// The job's value, or its panic raised here, on the walk's thread: the
    /// message again (the job's thread printed it with the place it was
    /// raised), so the test fails with the message a walk asking one state at
    /// a time failed with.
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

/// `job` at every index below `n`, spread over the machine's cores, each result at
/// its own index. A job that panics is caught and its panic kept, for the walk to
/// raise where it meets that job; its pool then runs no later index, which the walk,
/// meeting a pool's indices in order, never reaches. `None` from `job` (the job it
/// needs panicked) is a skip too.
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

/// Walk the graph `g` with the real code. `strict` asserts the model's laws and every
/// agreement; otherwise each disagreement is collected. `advisor` answers for the
/// roster (the real `advise`, or today's rule in its place) and `host` is whether the
/// session's host hands that answer to the decider at all.
///
/// What the real code finds at a projection and on an edge ([`Lane`]) depends on
/// nothing else, so all of it is asked first, spread over the cores ([`pooled`]), and
/// then judged state by state in the walk's order: a strict walk fails at the first
/// disagreement in that order, and a panic of the real code's is raised at the state
/// where a one-state-at-a-time walk would have met it, with the same message. A panic
/// of the model's own side ([`expected`], a successor) is raised while the [`Graph`]
/// is built, before any real code is asked.
fn walk(w: &World, g: &Graph, advisor: Advisor, host: Host, strict: bool) -> Walk {
    let lane = Lane {
        w,
        m: &g.m,
        advisor,
        host,
        cfg: SupervisorConfig::default(),
        clock: Clock::start(),
    };
    let clock = lane.clock;
    let at_state = pooled(g.shown.len(), |k| {
        Some(lane.at_state(shown_state(&g.shown[k])))
    });
    // What a strict walk tries around a state, met after the model's answer
    // to the state has been judged.
    let around = if strict {
        pooled(g.shown.len(), |k| {
            at_state[k].done().map(|here| lane.around(here))
        })
    } else {
        Vec::new()
    };
    let at_edge = pooled(g.edges.len(), |e| {
        let (from, edge) = &g.edges[e];
        at_state[*from].done().map(|here| lane.at_edge(here, edge))
    });
    let mut out = Walk::default();
    for node in &g.nodes {
        let st = &node.st;
        // The model's own laws are read at every model state, whatever the
        // real code was shown of it.
        if strict && let Some(law) = node.broken {
            panic!("{law} at {st:?}");
        }
        let here = at_state[node.projection].get();
        out.add(&here.found, strict, st);
        // Each session's own point, as the model has it there: what the real
        // advice is.
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
        // The decider: what it asks for each session at its point is what the
        // model tells the session.
        for (i, (d, want)) in here.decided.iter().zip(held).enumerate() {
            let model = wanted(want);
            out.note(strict, d.asked == model, st, None, || {
                format!(
                    "session {}, decided: {:?}, the model {model:?}",
                    i + 1,
                    d.asked
                )
            });
            // A wall the model holds the session at is waited at until the
            // reason for it ends.
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
            // On a limited account at the projection, so at the state.
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
            out.add(around[node.projection].get(), strict, st);
        }
        for &(action, e) in &node.steps {
            *out.fired.entry(action).or_default() += 1;
            out.add(at_edge[e].get(), strict, st);
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
        // The model's hour is the end of the real one: the rest of the real bound
        // is already spent when it opens.
        assert_eq!(spent_before(&w, m), w.bound() - cnst(m, "Budget"));
        let walked = walk(&w, g, advise, Host::Roster, true);
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
        // The decider was asked at both sessions' points at every state, and
        // each of its three answers is among them; the bounds, the wakes and
        // the `wall` mode were each tried somewhere.
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
        // Every move the model made was made on the policy's side.
        let moved: usize = MOVES
            .iter()
            .chain(&LEAVES)
            .map(|action| walked.fired.get(action).copied().unwrap_or(0))
            .sum();
        assert_eq!(walked.made, moved);
        // Every step of the sound rule was taken, and no defect was.
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

/// Negative control for the walk itself: the real code answers once per projection,
/// but the model's laws are read at every state, so a law broken only where the
/// agent's jobs run and a session sleeps (states that share their projection with
/// the state where the jobs ended) is still found.
#[test]
#[should_panic(expected = "JobsHoldNoSleeper at")]
fn the_models_laws_are_read_at_every_state_that_shares_a_projection() {
    let mut m = supervisor_account_rotation_model();
    m.invariants.push(Invariant {
        name: "JobsHoldNoSleeper",
        kind: aterm_spec::derive::InvariantKind::Claim,
        expr: or_(eq(var("jobs"), int(0)), eq(var("w1"), int(0))),
    });
    walk(
        &World::by_label(),
        &Graph::of(m, true),
        advise,
        Host::Roster,
        true,
    );
}

/// Negative controls: every defect of the `Buggy = 1` model, at every state the sound
/// rule reaches where it can fire, is disagreed with (the supervisor's by the real
/// advice and by the real decider fed it, the world's unread wall by the real
/// recorder, the carry-on after an early move by the real relaunch rule and policy);
/// and today's rule is not the model, in place of `advise` or as the decider's host.
#[test]
fn the_defects_and_todays_rule_disagree_with_the_real_code() {
    let w = World::by_label();
    let m = supervisor_account_rotation_model();
    // The two models' graphs are independent: the committed one's is built beside
    // the defects', whose failure, on this thread, is still the one raised first.
    let (buggy, sound) = thread::scope(|scope| {
        let sound = scope.spawn(sound);
        let buggy = Graph::of(aterm_spec::interp::with_buggy(&m, 1), false);
        let sound = sound
            .join()
            .unwrap_or_else(|payload| resume_unwind(payload));
        (buggy, sound)
    });
    let walked = walk(&w, &buggy, advise, Host::Roster, false);
    eprintln!(
        "account rotation Tier-1 (Buggy = 1): {} states walked, {} disagreements",
        walked.states,
        walked.off.len()
    );
    for defect in DEFECTS {
        let fired = walked.fired.get(defect).copied().unwrap_or(0);
        assert!(fired > 0, "{defect} never fired");
        // The real recorder read the wall, so `active` cannot be run on; the
        // real relaunch and policy type nothing of the move's own after a
        // move made ahead of a wall; the real advice at the state was not
        // the defect's move.
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
        // …and neither did the real decider ask for it.
        if how == "advised" {
            let parted = walked
                .off
                .iter()
                .filter(|off| off.action == Some(defect) && off.what.contains("decided"))
                .count();
            assert_eq!(parted, fired, "{defect}: decided otherwise at every firing");
        }
    }
    // …and nothing else parts: the sound steps of the same walk all agree.
    let other = walked
        .off
        .iter()
        .find(|off| !off.action.is_some_and(|action| DEFECTS.contains(&action)));
    assert!(other.is_none(), "{other:?}");

    // Today's rule: a wall is waited out. At a wall with a usable account the model
    // moves the session and today's rule does not. Walked once for both: a wall
    // waited out in the place of `advise`, and a host that hands the real decider no
    // advice at all.
    let today = walk(&w, sound, todays_rule, Host::NoRoster, false);
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
    // …and where every other account is limited too, waiting is right: the
    // two rules agree there.
    assert!(
        !today.off.iter().any(|off| off.action.is_none()
            && off.state["d1"] == 2
            && off.state["d2"] == 2
            && off.state["sh"] == 2),
        "today's rule is the model's where nothing is usable"
    );

    // Today's host under the real decider advises nothing (`TurnEndReading::account`
    // `None`): at that same wall the decider waits its reset out, and the model moves
    // the session.
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
    // …and at the notice of a wall the owner lifted it waits that notice
    // out, where the model continues the session there.
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
    // It asks for no move anywhere, and no move of the model's is made.
    assert_eq!((today.moves, today.made), (0, 0));
    assert_eq!(today.waits + today.todays, 2 * today.states);
    assert_eq!(today.resumes, 0, "nor is a lifted notice continued");
    // And it sleeps through the reset of another account, which wakes the model's
    // session: its wait is its own wall's.
    assert!(
        today.off.iter().any(
            |off| off.action.is_some_and(|action| action.starts_with("Reset"))
                && off.what.contains("a minute from the reset")
        ),
        "with no advice a wait ended at another account's reset"
    );
}

/// One real state, begun empty and changed only by the real transactions and a real
/// clock, kept in step with the model, which runs at the real bound so nothing is
/// seeded. Every wall names a reset `wall_s` ahead of the instant it is recorded at.
/// A history that counts on the hour's bound keeps `wall_s` short enough to stay
/// inside one rotation hour; one that needs a later wall to lift first changes it
/// between its walls, since a reset here is the real clock at the end of that one
/// account's wall.
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
        // Two sessions started by hand on this machine's own login, and the
        // first `/usage` read there.
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

    /// What the roster advises session `i` past a due probe
    /// (`accounts::advise_past_probe`), at the point the model has it at.
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

    /// The real state reads back as the model's, whose laws hold there.
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

    /// …and it moves each session as the model does.
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

    /// One model action and its real transaction, a minute later.
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

    /// A model reset and the real clock at the end of that account's wall.
    /// What the sessions are told then is the caller's to check: time has
    /// passed, and a sample may be due.
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

    /// The model's `Lapse` and the real clock at the end of the canary's ten
    /// minutes.
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

    // Both sessions hit the wall on this machine's login. One rotates alone.
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

    // Its window runs low: ahead of the wall the session moves to the only
    // fresh account, the shared one — which answers with a wall of its own.
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

    // The first wall lifts. The sample of the low account is half an hour
    // old by then, so the real advice is the probe; read again (still low),
    // the move ahead of the wall is to the dedicated account that came back.
    h.reset("ResetD1");
    assert_eq!(h.told(0), Advice::Probe);
    // Past the probe the roster already advises the model's move, on the sample it
    // holds: a probe nobody makes does not hold the session on the low account.
    // Handed over as a host builds it, the decider is given the probe with that move
    // behind it. Negative control: with no advice past it, nothing is behind it.
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

    // The owner's word: the shared account's wall is cleared and everyone
    // follows. Low there, a pinned session stays; a wall there ends the pin.
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

/// The breaker, at the real bound and from an empty state: two accounts take turns at
/// their walls, each lifting before the other's is hit, and the seventh wall inside
/// the hour is waited out.
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

    // The seventh wall: a fresh account is free, and nobody is moved.
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

/// The canary that never answers, from an empty state: the session that rotated
/// alone sits on the account it chose and never gets a response. For ten minutes the
/// other session is held; then it takes the canary's place (`active` is not put back
/// and nothing is rotated) and its served response proves the account. The first
/// session is never relaunched.
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

    // One second short of its ten minutes the hold stands; at them it does
    // not, and no timeout has put `active` back.
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

    // The second session takes its place: a compare-and-set on the canary's
    // record, a hold of its own, and no rotation.
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

    // Its served response proves the account, and the first session — on it
    // since the rotation — was never moved again.
    h.step("Served");
    assert!(h.real.proven && h.real.canary.is_none());
    assert_eq!(h.real.sessions_on("work-2"), SIDS);
    assert_eq!(
        (h.st["s1"], h.st["s2"], h.st["act"], h.st["prov"]),
        (2, 2, 2, 1)
    );

    // The canary's agent ends instead: its hold ends at once. (A new agent in that
    // tab is read on the account.)
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

/// Proof travels with the account, from an empty state. A wall is recorded on the
/// canary's account with no strike, the other session rotates past it, and the
/// account it chose answers with a wall of its own: `active` goes back to an account
/// that never served a response, and is not proven for having been gone back to.
/// When that account can be run on again the session at its wall takes the canary's
/// place on it; it does not follow. Negative control: the same history with a served
/// response before the wall goes back to a proven account, which is followed.
///
/// The strike is heard once the first canary's ten minutes are over: the real
/// `strike` puts back the record of the session still on the account gone back to,
/// with what is left of its hold, while the model's `Strike` ends every hold. Past
/// the ten minutes the two agree; inside them the real state holds (shown below; the
/// unit tests bind it).
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
        // The wall on the account the canary chose, recorded and not struck.
        // It is the first to lift, and it outlasts that canary's hold.
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
        // Inside the first canary's ten minutes the real strike and the model's
        // part: the session still alone on the account gone back to holds the
        // others for what is left of them, and the model's `Strike` ends every hold.
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
        // Past them (the step is a minute), they agree.
        h.now = chosen + accounts::CANARY_HOLD_S - MIN;
        h.step("Strike");
        assert_eq!(
            (h.real.active.as_deref(), h.real.proven),
            (Some("work-2"), served),
            "proven or not, as it was when it was left"
        );
        // The record of the session still on it is back with it; where the
        // account had answered there was none to put back.
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

/// The decider carried: [`InStep`]'s real accounts state and its model, and beside
/// them each session's real `TurnEndState`, carried from its first point on and
/// changed only as the loop changes it: a point folded in and decided ([`round`]), an
/// act typed (`acted`), a restart recorded (`restarted`), a move's answer told
/// (`account_told`). The walk plays the screen and the host.
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
    /// Both sessions' first turns end with work and no wall, and are carried
    /// on as today.
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

    /// What session `i`'s screen shows, as the loop reads it.
    fn shows(&self, i: usize, worked: Option<Duration>) -> TurnEndReading {
        match self.screen[i] {
            Some(reset) => limit(self.clock.at(reset), worked),
            None => turn_end(worked),
        }
    }

    /// Session `i`'s turn ran and ended, on the limit whose notice names `wall` or on
    /// none, and what that says of a move of its own not yet answered is told
    /// (`account_answer`, `account_told`: the loop's first step at a point). The
    /// host's half of the tell (the account proven, or struck) is the caller's: it is
    /// a step of the model's.
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

    /// One decision for session `i` at what its screen shows now, the host answering
    /// with the real advice on the real state.
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

    /// What the model tells session `i` where the model has it.
    fn model(&self, i: usize) -> Asked {
        wanted(&expected(&self.h.m, &self.h.st, i))
    }

    /// The loop typed `action`.
    fn typed(&mut self, i: usize, action: &TurnEndAction) {
        assert!(action.rule_id().is_some(), "nothing to type: {action:?}");
        let at = self.at();
        self.te[i].acted(action, &turn_end(None), at);
    }

    /// The model's wall on an account, and the turns of `sessions` ending on
    /// it.
    fn wall(&mut self, action: &str, sessions: &[usize]) {
        self.h.step(action);
        let reset = self.h.now + self.h.wall_s;
        for &i in sessions {
            assert_eq!(self.turn_ends(i, Some(reset)), None);
        }
    }

    /// The restart `r` asks for.
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

    /// The host could not make the relaunch `r` asks for, and says `word`: what it
    /// committed for the move is taken back exactly (`Strike::NotMoved`), and the
    /// loop does with the word what `restart_agent` does. `wait:…` (the agent's
    /// jobs): the point is held on the restart back-off and nothing is recorded. Any
    /// other word: `restarted(.., false, ..)`, and the point gets its `otherwise`.
    fn not_made(&mut self, i: usize, r: &Round, word: &str) {
        let (why, otherwise) = Carried::restart(r);
        let Restart::Account { to, .. } = why else {
            panic!("{why:?}");
        };
        let before = self.h.real.clone();
        let wall = r.point.is_some_and(AccountPoint::at_wall);
        // The host's commit re-asks past the probe: a due probe is no reason to
        // refuse a move the session was asked to make.
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

    /// The host made the relaunch `r` asks for: the model's `action`, the real
    /// transaction, and `restarted(.., true, ..)`. The session is on the account the
    /// model moved it to, at a fresh prompt.
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

    /// Session `i`'s first point after a move, at the relaunched prompt: the host is
    /// asked nothing, the relaunch typed nothing, and the point's own act is `rule`'s
    /// (the wall's resume after a move at a wall, the continue policy's after one
    /// ahead of a wall), and is typed.
    fn first_act(&mut self, i: usize, rule: &str) {
        let first = self.ask(i);
        assert_eq!(first.point, None, "{:?}", first.action);
        assert_eq!(first.action.rule_id(), Some(rule), "{:?}", first.action);
        self.typed(i, &first.action);
    }
}

/// The session's own bounds along one carried history (design §6, "Bounds. Per
/// session"; none is a model variable): the real decider carried through a relaunch
/// its host cannot make for the agent's jobs, a move refused at a wall, one refused
/// ahead of a wall, and four moves in an hour. Where nothing the session remembers
/// binds it, the decider asks what the model enables; where something does, the model
/// still enables the move and the decider gives today's action, never a move the
/// model does not make. Each bound ends where the design says, and the two agree
/// again from there.
#[test]
fn the_sessions_own_bounds_part_from_the_model_only_toward_todays_rule() {
    let mut c = Carried::begin(20 * MIN);
    let to = |account: i64, ahead: bool| Asked::Move { to: account, ahead };

    // The agent's jobs. Both turns end on this machine's login's limit, and session 1
    // asks for the next dedicated account.
    c.wall("WallD1", &[0, 1]);
    let asked = c.ask(0);
    assert_eq!((c.said(&asked), c.model(0)), (to(2, false), to(2, false)));
    // Its agent has background work: the host answers `wait:changed`, takes
    // the rotation back, and the loop records nothing. The model's session
    // sleeps; on the back-off the same move is asked, and made once the jobs
    // have ended.
    c.h.step("JobsStart");
    c.not_made(0, &asked, "wait:changed");
    assert!(!c.te[0].account_refused());
    c.h.step("Wait1");
    let again = c.ask(0);
    assert_eq!(again.action, asked.action, "no latch for a wait");
    c.h.step("JobsEnd");
    c.make(0, &again, "Move1");
    let chosen = c.h.now;

    // The other session, at the same wall, waits for the canary until its hold ends,
    // not until the wall's own reset.
    let held = c.ask(1);
    assert_eq!((c.said(&held), c.model(1)), (Asked::Waits, Asked::Waits));
    assert_eq!(
        waits_until(&held.action),
        Some(c.clock.at(chosen + accounts::CANARY_HOLD_S))
    );
    assert!(waits_until(&held.action) < waits_until(&held.today));
    c.h.step("Wait2");

    // The move not answered yet: at the relaunched prompt the host is asked nothing,
    // the relaunch typed nothing, and the point gets the resume of the wall the
    // session left. Typed, it is the first turn under the new login; served, it
    // proves the account.
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

    // The latch. The other session follows, and its host refuses the relaunch with a
    // word that is no wait (a line that cannot hold the login).
    let follow = c.ask(1);
    assert_eq!((c.said(&follow), c.model(1)), (to(2, false), to(2, false)));
    c.not_made(1, &follow, "refused:line");
    assert!(c.te[1].account_refused());
    // While that wall stands the host is not asked again and the wall is waited out
    // as today. The model still moves the session: the latch is no variable of its.
    let latched = c.ask(1);
    assert_eq!(latched.point, None);
    assert_eq!(latched.action, follow.today);
    assert_eq!(c.said(&latched), Asked::Waits);
    assert_eq!(c.model(1), to(2, false), "the stated exception");
    // The wall's reset comes: today's continuation, still nothing asked.
    c.h.reset("ResetD1");
    let resumed = c.ask(1);
    assert_eq!(resumed.point, None);
    assert_eq!(resumed.action.rule_id(), Some(RULE_LIMIT_RESUME));
    c.typed(1, &resumed.action);
    // Its turn ends with no wall: the latch ends with the wall's episode, at that
    // point (it is folded in before the host is asked), and from there the two agree
    // again: it follows, ahead of any wall, around the turn's own continuation.
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

    // The same ahead of a wall, by label. The owner pins the shared account; session
    // 1's move to it is refused, and its turn goes on.
    c.h.step("UseSh");
    assert_eq!(c.turn_ends(0, None), None);
    let pinned = c.ask(0);
    assert_eq!((c.said(&pinned), c.model(0)), (to(3, true), to(3, true)));
    c.not_made(0, &pinned, "refused:line");
    assert!(!c.te[0].account_refused(), "no wall, no wall's latch");
    // At its next turn end the same advice is handed over and decides
    // nothing; the model still moves it.
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
    // Advice naming another account is a new ask.
    c.h.step("UseD1");
    assert_eq!(c.turn_ends(0, None), None);
    let other = c.ask(0);
    assert_eq!((c.said(&other), c.model(0)), (to(1, true), to(1, true)));
    c.make(0, &other, "Move1");
    c.first_act(0, RULE_CONTINUE);
    assert_eq!(c.turn_ends(0, None), Some(AccountAnswer::Served));

    // The cap. Two more of the owner's pins, each followed: four moves this session
    // has made inside the hour.
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
    // The fifth, ahead of a wall: the advice is there, today's act is given,
    // and the model moves the session.
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
    // …and at a wall: waited out, and said, until the oldest of the four leaves the
    // hour, which here is sooner than the wall's own reset.
    c.h.wall_s = 4 * HOUR;
    c.wall("WallSh", &[0]);
    let capped = c.ask(0);
    assert_eq!(c.said(&capped), Asked::Waits);
    assert_eq!(c.model(0), to(1, false), "the stated exception");
    let TurnEndAction::WaitUntil { until, why, .. } = &capped.action else {
        panic!("{:?}", capped.action);
    };
    assert!(why.contains("a breaker: 4 account moves"), "{why}");
    assert_eq!(Some(*until), free);
    assert!(Some(*until) < waits_until(&capped.today));
    // The hour's lapse is not the model's. On the policy alone: at that
    // instant the move is asked.
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

/// The first turn under the new login is the model's `Served` or its `Strike`: what
/// the real decider reads off the point after its own move (`account_answer`) is the
/// model's step the host then takes. A limit on the relaunched screen before any turn
/// was asked says nothing (it is the old account's notice, in the resumed
/// conversation); the wall's resume ending on a limit is the candidate's own wall:
/// struck, `active` put back, and the session asks for the next account at once.
#[test]
fn the_first_turn_under_the_new_login_is_the_models_served_or_its_strike() {
    let mut c = Carried::begin(4 * HOUR);
    let to = |account: i64, ahead: bool| Asked::Move { to: account, ahead };
    c.wall("WallD1", &[0, 1]);
    let old = c.screen[0];
    let asked = c.ask(0);
    assert_eq!(c.said(&asked), to(2, false));
    c.make(0, &asked, "Move1");

    // Negative control: no turn asked yet, the old notice repainted.
    c.screen[0] = old;
    let repainted = c.shows(0, None);
    assert_eq!(c.te[0].account_answer(&repainted), None);
    c.screen[0] = None;

    // The wall's resume, typed: its turn ends on the new account's own limit.
    c.first_act(0, RULE_LIMIT_RESUME);
    let reset = c.h.now + c.h.wall_s;
    assert_eq!(
        c.turn_ends(0, Some(reset)),
        Some(AccountAnswer::Bad(AccountBad::Wall))
    );
    c.h.step("Strike");
    assert_eq!(c.h.real.active, None, "`active` goes back");
    // Told once: the move stands until the worker works again, and at its
    // wall the session asks for the next account.
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

    // The other session, still at the first wall, waits for that canary.
    let held = c.ask(1);
    assert_eq!((c.said(&held), c.model(1)), (Asked::Waits, Asked::Waits));
    c.h.step("Wait2");

    // This time the resume is served: the account is proven, and the other
    // session follows.
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
