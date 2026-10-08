// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER-1 for `SupervisorUsageWall` (aterm-spec
//! `supervisor_usage_wall_model`): the REAL screen reader (`aterm_phase::read`,
//! over the measured 2026-09-28 weekly wall with and without the background
//! shell the tab counted) and the REAL turn-end decider (`decide_turn_end`,
//! with the real `TurnEndState` it keeps) driven along EVERY reachable state
//! of the model, so "a wall whose tokens are back does not stand for good" is
//! the code's, state for state.
//!
//! Each model action is mirrored on the real side. The world (`BackgroundStarts`,
//! `BackgroundEnds`, the account's `due` tick) is invisible to the decider
//! except through the SCREEN: `bg` decides whether the done row counts `1 shell
//! still running` and the footer `· 1 shell ·`, and the real reader must still
//! hand the decider the wall; a wall appearing is the point `observe`d with
//! the turn's work; time at the wall is the real clock moved a minute per tick
//! (`reset_grace` = `Grace`, `limit_backoff` = `Rung1`, `Rung2`); `Act` is the
//! decider's own act, recorded with `acted`; `Met` is the next point observed
//! with the wall on the screen again, `Taken` a point with none after real
//! work. At every state the model's invariant holds and the decider TYPES
//! exactly where `Act` is enabled (`usage-resume@v1`, `keep going`), and
//! otherwise WAITS until exactly the tick `Act` becomes enabled.
//!
//! The reset the decider is handed is the REAL placement's, kept on the real
//! side as the loop keeps it: a wall seen arriving is placed on its print
//! ([`place_reset`]); a wall FOUND sitting has no print to go by and is placed
//! by the session window's rule, its far bare clock in the past (`view = 1`);
//! and at `Met` the wall is placed on the print again exactly where the loop
//! reads it as one it watched — a turn seen run before it, or the act left
//! untaken past its window by the REAL `TurnEndState::untaken_act` (Claude
//! Code held the line, no turn ran: the walk runs `Met` both ways).
//! NEGATIVE CONTROLS: the `Buggy = 1` model disagrees (a first act at once); the same walk with the
//! reader replaced by the precedence of 2026-09-28 (Busy beats a wall, so a
//! screen that counts a shell reads busy) disagrees at every `bg = 1` wall —
//! the stall the model's liveness calls `WallMasked`; and the loop of
//! 2026-10-01, which read a wall standing after an untaken act as no print,
//! disagrees after every unseen `Met` of a wall found sitting.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::limit::{ResetSpec, place_reset, reset_at};
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    RULE_LIMIT_RESUME, TurnEndAction, TurnEndReading, TurnEndState, TurnEndTiming, decide_turn_end,
};
use aterm_phase::prompt::fixtures::screen;
use aterm_spec::derive::{Model, supervisor_usage_wall_model};

type State = BTreeMap<&'static str, i64>;

/// The model as this bind reads it: the wall's rows never leave the screen
/// (`Hidden = 0`). A wall that scrolls out of view is remembered by the
/// loop's `Session` (`wall_memory.rs`), which a reader and a decider do not
/// reach; `tier1_the_real_loop_remembers_a_hidden_wall_exactly_where_the_model_does`
/// (`turn_end_loop_tests.rs`) binds that half, on the real `Session`.
fn walled_model() -> Model {
    aterm_spec::interp::with_consts(&supervisor_usage_wall_model(), &[("Hidden", 0)])
}

const MIN: Duration = Duration::from_secs(60);
const LONG: Duration = Duration::from_secs(10 * 60);
const SHORT: Duration = Duration::from_secs(5);

const WALL: &str = include_str!(
    "../../aterm-phase/src/fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
);
const LIVE: &str = include_str!(
    "../../aterm-phase/src/fixtures/claude-2.1.283-wall-weekly-goal-paused-spinner.txt"
);

/// 2026-09-28 20:00 PDT, when the wall in every state is printed.
const PRINT: i64 = 1_790_650_800;
const PDT: i64 = -7 * 3600;

fn zone(_: &str) -> Option<i64> {
    Some(PDT)
}

/// The weekly wall says `resets <a bare clock>`, `ResetAt` ticks (minutes)
/// after its print, as the vendor prints one for a reset under a day off. The
/// reset the loop waits for is the REAL placement's ([`place_reset`], the print
/// known to the done row's minute and the slack): how many minutes after the
/// print the decider is handed — so the placement is inside the bind, and a
/// misplacement ([`reset_at`], the session window's five-hour rule) is a
/// disagreement the walk sees.
fn placed_after_the_print(m: &Model, ticks: i64) -> Duration {
    let clock = 20 * 60 + ticks;
    let spec = ResetSpec::At {
        date: None,
        hour: u32::try_from(clock / 60).unwrap(),
        minute: u32::try_from(clock % 60).unwrap(),
        zone: Some("America/Los_Angeles".to_string()),
    };
    let at = place_reset(&spec, false, PRINT, Some(PRINT - 7 * 60), PDT, zone);
    assert!(at > PRINT, "{m:?}");
    Duration::from_secs(u64::try_from(at - PRINT).unwrap())
}

/// The bare clock of a wall FOUND sitting, 13 h after the print (9am read at
/// 20:00), placed as the loop places a wall with nothing to date it: the
/// session window's rule, 11 h in the PAST — how far before the wall's
/// appearance the decider is handed its reset.
fn misplaced_before_the_print() -> Duration {
    let nine = ResetSpec::At {
        date: None,
        hour: 9,
        minute: 0,
        zone: Some("America/Los_Angeles".to_string()),
    };
    let at = place_reset(&nine, false, PRINT, None, PDT, zone);
    assert!(at < PRINT, "the session rule places it in the past");
    Duration::from_secs(u64::try_from(PRINT - at).unwrap())
}

fn cnst(m: &Model, name: &str) -> i64 {
    m.consts
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| *v)
        .unwrap_or_else(|| panic!("no const {name}"))
}

fn minutes(m: &Model, name: &str) -> Duration {
    MIN * u32::try_from(cnst(m, name)).expect("a small constant")
}

/// The real policy's clocks, scaled so one model tick is one minute; an act's
/// take window is no tick at all (a `Met` with no busy read is judged at once).
fn timing(m: &Model) -> TurnEndTiming {
    TurnEndTiming {
        reset_grace: minutes(m, "Grace"),
        reset_jitter: Duration::ZERO,
        limit_backoff: vec![minutes(m, "Rung1"), minutes(m, "Rung2")],
        window: MIN * 24 * 60,
        take_within: Duration::ZERO,
        ..TurnEndTiming::default()
    }
}

/// The screen a state stands for: the wall (`bg`: the shell the tab counted
/// kept, else the done row and the footer without it), or a worker at work.
fn rows(st: &State) -> Vec<String> {
    if st["wall"] != 1 {
        return screen(LIVE);
    }
    let mut rows = screen(WALL);
    if st["bg"] == 0 {
        for r in &mut rows {
            *r = r
                .replace(" · 1 shell still running", "")
                .replace(" · 1 shell ·", " ·");
        }
    }
    rows
}

/// What the reader hands the decider at `st`, the wall's reset as the real
/// side placed it (`reset`). `masked`: the precedence of 2026-09-28 — a screen
/// that counts background work is busy, whatever the turn ended on.
fn reading(st: &State, worked: Option<Duration>, reset: Instant, masked: bool) -> TurnEndReading {
    let r = rows(st);
    let read = aterm_phase::read(Some("claude"), &r, Some(2));
    let reset_at = (st["wall"] == 1).then_some(reset);
    let mut t = TurnEndReading::of(&read, &r, false, worked, reset_at, None);
    t.restartable = true;
    if masked && st["bg"] == 1 && st["wall"] == 1 {
        t.phase = Phase::Busy;
        t.wall = None;
        t.reset_at = None;
        t.wall_message = String::new();
    }
    t
}

/// How the real side runs: `unseen`, a `Met` is the line held under the wall
/// with no turn run (no busy read), else a turn that ran into it again;
/// `blind_to_untaken`, the loop of 2026-10-01, which read a wall standing after
/// an untaken act as no print (the NEGATIVE CONTROL), else the loop as it is.
#[derive(Clone, Copy)]
struct Run {
    masked: bool,
    unseen: bool,
    blind_to_untaken: bool,
}

#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    appeared: Instant,
    /// The wall's reset as the loop placed it.
    reset: Instant,
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Act,
    Wait(Duration),
    Nothing,
}

fn expected(m: &Model, st: &State) -> Verdict {
    if st["wall"] != 1 {
        return Verdict::Nothing;
    }
    if m.action_enabled("Act", st) {
        return Verdict::Act;
    }
    let cap = cnst(m, "Cap");
    let due = (1..=cap)
        .find(|&k| {
            let mut later = st.clone();
            later.insert("clock", (st["clock"] + k).min(cap));
            later.insert("waited", st["waited"] + k);
            m.action_enabled("Act", &later)
        })
        .unwrap_or_else(|| panic!("a wall no tick ever acts on: {st:?}"));
    Verdict::Wait(MIN * u32::try_from(due).unwrap())
}

fn verdict(a: &TurnEndAction, now: Instant) -> Verdict {
    match a {
        TurnEndAction::Type { text, rule_id } => {
            assert_eq!(*rule_id, RULE_LIMIT_RESUME);
            assert_eq!(text, "keep going");
            Verdict::Act
        }
        TurnEndAction::WaitUntil { until, .. } => {
            Verdict::Wait(until.saturating_duration_since(now))
        }
        TurnEndAction::Nothing => Verdict::Nothing,
        other => panic!("an act the model has no word for: {other:?}"),
    }
}

fn mirror(
    m: &Model,
    action: &str,
    before: &State,
    after: &State,
    real: &mut Real,
    cfg: &SupervisorConfig,
    run: Run,
) -> bool {
    let now = real.now;
    let masked = run.masked;
    let on_the_print = |appeared: Instant| appeared + placed_after_the_print(m, cnst(m, "ResetAt"));
    match action {
        "BackgroundStarts" | "BackgroundEnds" => {}
        // The Buggy model's misplaced reset is a wall like any other on the
        // REAL side: the real loop places the reset where the notice says.
        "WallAppears" | "WallAppearsLate" | "WallMisplaced" => {
            real.appeared = now;
            real.reset = on_the_print(now);
            real.st
                .observe(&reading(after, Some(LONG), real.reset, masked), now);
        }
        // Found standing, nothing to date its print: the session rule's
        // placement, in the past.
        "WallFoundSitting" => {
            real.appeared = now;
            real.reset = now
                .checked_sub(misplaced_before_the_print())
                .expect("an instant 11 h back");
            real.st
                .observe(&reading(after, None, real.reset, masked), now);
        }
        "Tick" => real.now += MIN,
        "Act" => {
            let r = reading(before, None, real.reset, masked);
            real.st.observe(&r, now);
            let a = decide_turn_end(&real.st, &r, cfg, now);
            if !matches!(a, TurnEndAction::Type { .. }) {
                return false;
            }
            real.st.acted(&a, &r, now);
        }
        // The wall answers the act: printed again after a turn, or standing
        // with the line held and no turn run. Read BEFORE the point is folded
        // in, as the loop reads it: a print the loop watched, placed where the
        // notice says.
        "Met" => {
            let worked = (!run.unseen).then_some(SHORT);
            let answered = !run.blind_to_untaken && real.st.untaken_act(now).is_some();
            if worked.is_some() || answered {
                real.reset = on_the_print(real.appeared);
            }
            real.st
                .observe(&reading(after, worked, real.reset, masked), now);
        }
        "Taken" => {
            let mut quiet = after.clone();
            quiet.insert("wall", 0);
            let mut t = reading(&quiet, Some(LONG), real.reset, masked);
            t.phase = Phase::Idle;
            t.said_tail = Some("Stage done.".to_string());
            real.st.observe(&t, now);
        }
        // The defects: not the real code's moves.
        "WallMasked" | "GivesUp" | "MetKeepsTheGuess" => return false,
        other => panic!("an action this bind does not mirror: {other}"),
    }
    true
}

fn walk(m: &Model, strict: bool, run: Run) -> (usize, Vec<State>) {
    let cfg = SupervisorConfig::default();
    let base = Instant::now() + Duration::from_secs(24 * 3600);
    let init = Real {
        st: TurnEndState::new(timing(m)),
        now: base,
        appeared: base,
        reset: base,
    };
    let mut seen: BTreeSet<State> = BTreeSet::new();
    let mut queue = VecDeque::from([(m.init_state(), init)]);
    let mut disagree = Vec::new();
    while let Some((st, real)) = queue.pop_front() {
        if !seen.insert(st.clone()) {
            continue;
        }
        if strict {
            for inv in &m.invariants {
                assert!(m.check_invariant(inv.name, &st), "{} at {st:?}", inv.name);
            }
        }
        let mut at = real.st.clone();
        let here = reading(&st, None, real.reset, run.masked);
        at.observe(&here, real.now);
        let got = verdict(&decide_turn_end(&at, &here, &cfg, real.now), real.now);
        let want = expected(m, &st);
        if got != want {
            assert!(
                !strict,
                "the real decider disagrees at {st:?}: {got:?}, not {want:?}"
            );
            disagree.push(st.clone());
        }
        for action in &m.actions {
            let mut next = st.clone();
            if !m.fire(action.name, &mut next) {
                continue;
            }
            let mut r = real.clone();
            if mirror(m, action.name, &st, &next, &mut r, &cfg, run) {
                queue.push_back((next, r));
            }
        }
    }
    (seen.len(), disagree)
}

#[test]
fn tier1_the_real_reader_and_decider_continue_a_usage_wall_exactly_where_the_model_allows() {
    let m = walled_model();
    // A wall that answers an act after a turn ran into it, and one that
    // answers with the line held and no turn run.
    for unseen in [false, true] {
        let run = Run {
            masked: false,
            unseen,
            blind_to_untaken: false,
        };
        let (states, _) = walk(&m, true, run);
        eprintln!("usage wall Tier-1 (unseen {unseen}): {states} states walked");
        assert!(states > 100, "{states}");
    }
}

/// THE LOOP OF 2026-10-01 is not the model: a wall found sitting, its far bare
/// clock guessed passed, tried once and answered with the line held — no turn
/// run, no busy read — kept the guess, and the ladder typed into the standing
/// wall at every rung (24 lines until the reset). Read without
/// `untaken_act`, the walk disagrees exactly there: after an unseen `Met` of a
/// wall found sitting, where the model waits for the reset. With it (the
/// strict walk above), nowhere.
#[test]
fn the_loop_that_reads_no_print_in_an_untaken_act_types_into_a_standing_wall() {
    let m = walled_model();
    let run = Run {
        masked: false,
        unseen: true,
        blind_to_untaken: true,
    };
    let (_, off) = walk(&m, false, run);
    assert!(!off.is_empty(), "the blind loop agreed everywhere");
    assert!(
        off.iter()
            .all(|st| st["wall"] == 1 && st["sitting"] == 1 && st["view"] == 0 && st["tries"] >= 1),
        "{off:?}"
    );
    // ... and the model's defect `MetKeepsTheGuess` is that loop: it types a
    // second act into the wall before the tokens are back.
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut st = buggy.init_state();
    for a in ["WallFoundSitting", "Act", "MetKeepsTheGuess", "Tick", "Act"] {
        assert!(buggy.fire(a, &mut st), "{a} at {st:?}");
    }
    assert!(!buggy.check_invariant("NoBlindAct", &st), "{st:?}");
    let mut st = m.init_state();
    for a in ["WallFoundSitting", "Act", "Met", "Tick"] {
        assert!(m.fire(a, &mut st), "{a} at {st:?}");
    }
    assert!(!m.action_enabled("Act", &st), "{st:?}");
}

/// NEGATIVE CONTROLS: the reader of 2026-09-28 (a screen that counts a shell is
/// busy) disagrees at every wall that shows over background work; and the
/// `Buggy = 1` model (a first act at once, a reset placed in the past) is not
/// the real decider either.
#[test]
fn the_masked_reader_and_the_buggy_model_disagree_with_the_real_code() {
    let m = walled_model();
    let seen = |masked: bool| Run {
        masked,
        unseen: false,
        blind_to_untaken: false,
    };
    let (_, masked) = walk(&m, false, seen(true));
    assert!(!masked.is_empty(), "the masked reader agreed everywhere");
    assert!(
        masked.iter().all(|st| st["bg"] == 1 && st["wall"] == 1),
        "{masked:?}"
    );
    // The placement control: the session window's rule, for the same wall a
    // day-less clock 13 h off, is in the PAST — the misplacement the model's
    // `view = 1` stands for, which the real placement makes only for a wall
    // found with nothing to date its print.
    let nine = ResetSpec::At {
        date: None,
        hour: 9,
        minute: 0,
        zone: Some("America/Los_Angeles".to_string()),
    };
    assert!(reset_at(&nine, PRINT, PDT, zone) < PRINT);
    assert_eq!(
        place_reset(&nine, false, PRINT, Some(PRINT - 7 * 60), PDT, zone),
        PRINT + 13 * 3600
    );
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let (_, off) = walk(&buggy, false, seen(false));
    assert!(
        off.iter()
            .any(|st| st["wall"] == 1 && st["view"] == 1 && st["sitting"] == 0 && st["tries"] == 0),
        "no disagreement at a misplaced reset's first act: {} states",
        off.len()
    );
}
