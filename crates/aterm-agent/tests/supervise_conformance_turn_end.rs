// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER-1 for `SupervisorTurnEnd` (aterm-spec `supervisor_turn_end_model`):
//! the REAL turn-end decider (`decide_turn_end`, with the real
//! `TurnEndState` it keeps) driven along EVERY reachable state of the model.
//!
//! Each model action is mirrored on the real side — a box or a wall on the
//! screen is the reading the decider is handed, a wall's wait running out is
//! the clock moved by the real backoff, a turn's yield is the next point
//! `observe`d with that much busy work, an hour passing is the clock moved
//! past the budget's window, and the model's `Continue`/`Retry` are the real
//! decider's own act, recorded with `acted` — and at every state the real
//! verdict is checked against the model: it types a continuation exactly
//! where `Continue` is enabled, a retry exactly where `Retry` is, waits
//! exactly where a wall's wait runs, and escalates exactly where the model
//! allows no act and the budget, the tries or two short continuations are
//! spent — and, at an act's point that showed no busy read (`pending` 3),
//! waits for its deadline and then acts or escalates, never nothing (the
//! latch lane B2's review found: at every free point the real verdict is
//! something). Each state's verdict is the loop's: the point folded in
//! again with no work (`observe`), then decided — as `turn_end_now` does at
//! a deadline. The model's invariants are checked at every state. NEGATIVE
//! CONTROL: at the state with a box up and everything else free, the real
//! decider does nothing while the `Buggy = 1` model (which ignores the box)
//! would continue — so a green walk is not vacuous.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    Composer, RULE_API_RETRY, RULE_CONTINUE, TurnEndAction, TurnEndReading, TurnEndState,
    decide_turn_end,
};
use aterm_phase::WallKind;
use aterm_spec::derive::{Model, supervisor_turn_end_model};

type State = BTreeMap<&'static str, i64>;

/// Work a turn did that counts, and work that does not.
const LONG: Duration = Duration::from_secs(3 * 60);
const SHORT: Duration = Duration::from_secs(10);

fn cnst(m: &Model, name: &str) -> i64 {
    m.consts
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| *v)
        .unwrap_or_else(|| panic!("no const {name}"))
}

/// The screen the model state stands for.
fn reading(st: &State, worked: Option<Duration>) -> TurnEndReading {
    let phase = if st["pending"] == 1 || st["pending"] == 2 {
        Phase::Busy
    } else if st["box_up"] == 1 {
        Phase::Prompt
    } else {
        Phase::Idle
    };
    let wall = (st["wall"] > 0 && st["box_up"] == 0).then_some(WallKind::Overloaded);
    TurnEndReading {
        phase,
        authoritative: true,
        survey: false,
        wall,
        wall_message: if wall.is_some() {
            "API Error: 529 Overloaded.".to_string()
        } else {
            String::new()
        },
        reset_at: None,
        resumes_by_itself: false,
        composer: Composer::Empty,
        said_tail: Some("Stage done.".to_string()),
        worked,
        rules: None,
        pending_input: false,
        interrupted: false,
        upgrading: false,
    }
}

/// The real side of one walk: the decider's state and the loop's clock.
#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
}

/// What the real decider says at `st`, as one of the model's words.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Continue,
    Retry,
    Wait,
    Escalate,
    Nothing,
}

fn verdict(a: &TurnEndAction) -> Verdict {
    match a {
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_CONTINUE => Verdict::Continue,
        TurnEndAction::Accept { .. } => Verdict::Continue,
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_API_RETRY => Verdict::Retry,
        TurnEndAction::WaitUntil { .. } => Verdict::Wait,
        TurnEndAction::Escalate { .. } => Verdict::Escalate,
        TurnEndAction::Nothing => Verdict::Nothing,
        other => panic!("an act the model has no word for: {other:?}"),
    }
}

/// What the model says the decider must do at `st`.
fn expected(m: &Model, st: &State) -> Verdict {
    let (budget, tries) = (cnst(m, "Budget"), cnst(m, "Tries"));
    // An unseen point: waited for, then — at its deadline — decided as the
    // point after `DeadlineUnseen`.
    if st["pending"] == 3 {
        if st["waited"] < cnst(m, "Take") {
            return Verdict::Wait;
        }
        let mut judged = st.clone();
        assert!(m.fire("DeadlineUnseen", &mut judged), "{st:?}");
        return expected(m, &judged);
    }
    if m.action_enabled("Continue", st) {
        return Verdict::Continue;
    }
    if m.action_enabled("Retry", st) {
        return Verdict::Retry;
    }
    if st["pending"] > 0 || st["box_up"] == 1 {
        return Verdict::Nothing;
    }
    match st["wall"] {
        // Every act the model allows at a free point is disabled: the
        // budget, the tries or the short streak is spent.
        0 | 2 => {
            assert!(
                st["used"] >= budget || st["short"] >= 2 || st["tries"] >= tries,
                "a free point where the model allows nothing and nothing is spent: {st:?}"
            );
            Verdict::Escalate
        }
        _ if st["tries"] >= tries => Verdict::Escalate,
        _ => Verdict::Wait,
    }
}

/// Mirror one model action on the real side (the model has fired it).
fn mirror(
    m: &Model,
    action: &str,
    before: &State,
    after: &State,
    real: &mut Real,
    cfg: &SupervisorConfig,
) {
    let backoff = real.st.timing.retry_backoff.clone();
    match action {
        "BoxAppears" | "BoxLeaves" => {}
        "WallAppears" => real.st.observe(&reading(after, Some(SHORT)), real.now),
        "WallDue" => {
            if let Some(b) = backoff.get(usize::try_from(before["tries"]).unwrap()) {
                real.now += *b;
            }
        }
        "HourPasses" => real.now += real.st.timing.window + Duration::from_secs(1),
        "HumanWork" => real.st.observe(&reading(after, Some(LONG)), real.now),
        "Continue" | "Retry" => {
            let r = reading(before, None);
            let a = decide_turn_end(&real.st, &r, cfg, real.now);
            real.st.acted(&a, &r, real.now);
        }
        "WorkedShort" | "RetryHitsTheWall" => {
            real.st.observe(&reading(after, Some(SHORT)), real.now);
        }
        "WorkedLong" | "RetryTaken" => real.st.observe(&reading(after, Some(LONG)), real.now),
        "ReplyUnseen" | "DeadlineUnseen" => real.st.observe(&reading(after, None), real.now),
        "TimePassesUnseen" => {
            let take = u32::try_from(cnst(m, "Take")).unwrap();
            real.now += real.st.timing.take_within / take;
        }
        other => panic!("an action this bind does not mirror: {other}"),
    }
}

#[test]
fn tier1_the_real_decider_types_exactly_where_the_model_allows() {
    let m = supervisor_turn_end_model();
    let cfg = SupervisorConfig {
        continue_per_hour: u32::try_from(cnst(&m, "Budget")).unwrap(),
        ..SupervisorConfig::default()
    };
    let base = Instant::now() + Duration::from_secs(3600);
    let mut init = Real {
        st: TurnEndState::default(),
        now: base,
    };
    assert_eq!(
        init.st.timing.retry_backoff.len(),
        usize::try_from(cnst(&m, "Tries")).unwrap(),
        "the model's Tries is the real ladder's length"
    );
    // The walk starts at a point that ended a turn of real work.
    let s0 = m.init_state();
    init.st.observe(&reading(&s0, Some(LONG)), init.now);

    let mut seen: BTreeSet<State> = BTreeSet::new();
    let mut queue = VecDeque::from([(s0, init)]);
    let mut acts = BTreeMap::<&str, usize>::new();
    while let Some((st, real)) = queue.pop_front() {
        if !seen.insert(st.clone()) {
            continue;
        }
        for inv in &m.invariants {
            assert!(m.check_invariant(inv.name, &st), "{} at {st:?}", inv.name);
        }
        let mut at = real.st.clone();
        at.observe(&reading(&st, None), real.now);
        let got = verdict(&decide_turn_end(&at, &reading(&st, None), &cfg, real.now));
        assert_eq!(
            got,
            expected(&m, &st),
            "the real decider disagrees at {st:?}"
        );
        // No silent latch: at a free point the real decider says something.
        if st["box_up"] == 0 && (st["pending"] == 0 || st["pending"] == 3) {
            assert_ne!(got, Verdict::Nothing, "a silent point at {st:?}");
        }
        *acts
            .entry(match got {
                Verdict::Continue => "continue",
                Verdict::Retry => "retry",
                Verdict::Wait => "wait",
                Verdict::Escalate => "escalate",
                Verdict::Nothing => "nothing",
            })
            .or_default() += 1;
        for action in &m.actions {
            let mut next = st.clone();
            if !m.fire(action.name, &mut next) {
                continue;
            }
            let mut r = real.clone();
            mirror(&m, action.name, &st, &next, &mut r, &cfg);
            queue.push_back((next, r));
        }
    }
    // The whole reachable space was walked, and every verdict occurs.
    assert_eq!(
        seen.len(),
        123,
        "the reachable space changed: {}",
        seen.len()
    );
    for word in ["continue", "retry", "wait", "escalate", "nothing"] {
        assert!(
            acts.get(word).copied().unwrap_or(0) > 0,
            "{word} never occurred: {acts:?}"
        );
    }

    // NEGATIVE CONTROL: a box up, nothing else in the way.
    let mut boxed = m.init_state();
    boxed.insert("box_up", 1);
    let mut real = TurnEndState::default();
    real.observe(&reading(&m.init_state(), Some(LONG)), base);
    assert_eq!(
        decide_turn_end(&real, &reading(&boxed, None), &cfg, base),
        TurnEndAction::Nothing
    );
    assert!(!m.action_enabled("Continue", &boxed));
    assert!(
        aterm_spec::interp::with_buggy(&m, 1).action_enabled("Continue", &boxed),
        "the Buggy model types into the box: the walk above would have caught a real one"
    );
}
