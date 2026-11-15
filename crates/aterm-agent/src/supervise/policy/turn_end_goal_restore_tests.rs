// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE GOAL CLAUDE CODE CLEARED AFTER AN UNRECOVERABLE ERROR, PUT BACK ([`super::goal_restore`]):
// what the policy decides at a session's points once its transcript reads the goal lost. Each
// test carries its negative control.

use crate::harness::goal::{GoalRead, Lost, Untypable};
use crate::supervise::policy::goal_restore::{self, RULE_GOAL_RESTORE};

/// The goal 2.1.323 cleared on m2 on 2026-11-13.
const CONDITION: &str = "the account registry and automated account switching just works";

fn lost(clear: &str, served: bool) -> GoalRead {
    GoalRead::Lost(Lost {
        condition: CONDITION.to_string(),
        line: Ok(CONDITION.to_string()),
        label: "authentication failed".to_string(),
        clear: clear.to_string(),
        served,
    })
}

fn reads(goal: GoalRead, r: TurnEndReading) -> TurnEndReading {
    TurnEndReading { goal, ..r }
}

/// A turn of three minutes' work that ended on `said`.
fn worked(said: &str) -> TurnEndReading {
    idle(said, Some(3 * MIN))
}

fn restored() -> TurnEndAction {
    TurnEndAction::TypeCommand {
        command: format!("/goal {CONDITION}"),
        rule_id: RULE_GOAL_RESTORE,
        then: Then::Nothing,
    }
}

fn restores(a: &TurnEndAction) -> bool {
    matches!(a, TurnEndAction::TypeCommand { rule_id, .. } if *rule_id == RULE_GOAL_RESTORE)
}

/// T2: UNDER CRUISE, AT AN IDLE POINT SERVED AFTER AN ERROR CLEAR, exactly `/goal <condition>` is
/// typed, once: a later point that still reads the same clear gets the mode's continuation. The
/// same holds under Land. Negative controls: a person's `/goal clear` (the transcript reads no
/// goal) and a goal they set since get the continuation a session with no goal read gets; under
/// Off nothing is typed.
#[test]
fn a_goal_cleared_by_an_error_is_typed_back_once() {
    let t = t0();
    let mut st = TurnEndState::default();
    let first = reads(lost("c1", true), worked("Suites are running."));
    assert_eq!(act(&mut st, &first, t), restored());
    let again = reads(lost("c1", true), worked("The parser compiles."));
    let a = act(&mut st, &again, t + 3 * MIN);
    assert!(!restores(&a), "once per clear: {a:?}");
    assert!(
        matches!(
            a,
            TurnEndAction::Type {
                rule_id: RULE_CONTINUE,
                ..
            }
        ),
        "{a:?}"
    );

    // Land gives its instruction first, and puts the goal back in its nudge's place.
    let mut landing = TurnEndState::default();
    let first = land(reads(lost("c1", true), worked("Suites are running.")));
    let a = act(&mut landing, &first, t);
    assert!(
        matches!(&a, TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_LAND),
        "Land's instruction is typed as decided: {a:?}"
    );
    let next = land(reads(lost("c1", true), worked("The parser compiles.")));
    let mut bare = TurnEndState::default();
    let _ = act(&mut bare, &land(worked("Suites are running.")), t);
    let mode = act(
        &mut bare,
        &land(worked("The parser compiles.")),
        t + 3 * MIN,
    );
    assert!(
        matches!(&mode, TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_LAND_NUDGE),
        "PRECONDITION: Land nudges there: {mode:?}"
    );
    assert_eq!(
        act(&mut landing, &next, t + 3 * MIN),
        restored(),
        "Land puts the goal back in its nudge's place"
    );

    for goal in [GoalRead::Off, GoalRead::Active("another goal".to_string())] {
        let mut st = TurnEndState::default();
        let mut control = TurnEndState::default();
        let r = reads(goal.clone(), worked("Suites are running."));
        let a = act(&mut st, &r, t);
        assert!(!restores(&a), "{goal:?}: {a:?}");
        assert_eq!(
            a,
            act(&mut control, &worked("Suites are running."), t),
            "{goal:?}: as with no goal read"
        );
    }

    let mut off = TurnEndState::default();
    let r = under(
        Autopilot::Off,
        reads(lost("c1", true), worked("Suites are running.")),
    );
    assert_eq!(act(&mut off, &r, t), TurnEndAction::Nothing);
}

/// T3: A GOAL CLAUDE CODE CLEARS AGAIN WITHIN THE WAIT after a restore is typed back only once the
/// wait passes, ten minutes after the first restore and twenty after the second; the points in
/// between get the mode's continuation, never a restore.
#[test]
fn a_second_clear_waits_out_the_back_off() {
    let t = t0();
    let mut st = TurnEndState::default();
    assert_eq!(
        act(
            &mut st,
            &reads(lost("c1", true), worked("Suites are running.")),
            t
        ),
        restored()
    );
    let set = reads(
        GoalRead::Active(CONDITION.to_string()),
        worked("The parser compiles."),
    );
    assert!(!restores(&act(&mut st, &set, t + 3 * MIN)));
    for (at, said) in [(6, "The lexer compiles."), (9, "The docs build.")] {
        let r = reads(lost("c2", true), worked(said));
        let a = act(&mut st, &r, t + at * MIN);
        assert!(!restores(&a), "{at} min, within the wait: {a:?}");
    }
    let r = reads(lost("c2", true), worked("The tests pass."));
    assert_eq!(act(&mut st, &r, t + 12 * MIN), restored(), "past the wait");
    let r = reads(lost("c3", true), worked("The bench runs."));
    assert!(
        !restores(&act(&mut st, &r, t + 30 * MIN)),
        "the second restore's wait is twenty minutes"
    );
    let r = reads(lost("c3", true), worked("The fuzzer runs."));
    assert_eq!(act(&mut st, &r, t + 33 * MIN), restored());
}

/// T4: NOTHING IS TYPED BACK WHILE A WALL IS DRAWN, while input is pending, over a draft in the
/// composer, or before the error passed (nothing served since the clear and no wall seen gone).
/// Negative control: the same session past each, idle and served, gets the restore.
#[test]
fn no_restore_at_a_wall_pending_input_a_draft_or_an_unpassed_error() {
    let t = t0();
    let wall = WallKind::ApiError {
        code: Some(403),
        retryable: true,
        cause: ApiCause::Server,
    };
    let walled_lost = reads(
        lost("c1", false),
        walled(wall, "API Error: 403 · Please run /login", None),
    );
    let pending = TurnEndReading {
        pending_input: true,
        ..reads(lost("c1", true), worked("Suites are running."))
    };
    let draft = TurnEndReading {
        composer: Composer::Typed,
        ..reads(lost("c1", true), worked("Suites are running."))
    };
    let unpassed = reads(lost("c1", false), worked("Suites are running."));
    let past = [
        "The parser compiles.",
        "The lexer compiles.",
        "The docs build.",
        "The tests pass.",
    ];
    for (case, r) in [
        ("a wall", &walled_lost),
        ("pending input", &pending),
        ("a draft", &draft),
        ("nothing served", &unpassed),
    ] {
        let mut st = TurnEndState::default();
        let a = act(&mut st, r, t);
        assert!(!restores(&a), "{case}: {a:?}");
        // Past it, idle and served: the restore comes at the first point whose mode types
        // (a wall's own retry first, where the wall owes one).
        let at = (1..=past.len()).find(|&n| {
            let r = reads(lost("c1", true), worked(past[n - 1]));
            restores(&act(
                &mut st,
                &r,
                t + 3 * MIN * u32::try_from(n).expect("small"),
            ))
        });
        assert!(at.is_some_and(|n| n <= 2), "{case}, then past it: {at:?}");
    }
}

/// THE RESTORE TAKES A CONTINUATION'S PLACE ONLY: a point whose mode answers the prose question
/// the turn ended on types that answer, exactly as with no goal read, and the restore waits for
/// the next point whose act is a continuation. Negative control: the same question with no lost
/// goal gets the same answer, and the continuation after it is the restore.
#[test]
fn an_answer_is_typed_and_the_restore_waits_for_a_continuation() {
    let t = t0();
    let question = "Should I rewrite the parser or patch the lexer?";
    let mut st = TurnEndState::default();
    let a = act(&mut st, &reads(lost("c1", true), worked(question)), t);
    let mut control = TurnEndState::default();
    let answer = act(&mut control, &worked(question), t);
    assert!(
        matches!(&answer, TurnEndAction::Type { rule_id, .. } if !goal_restore::replaces(rule_id)),
        "PRECONDITION: an answer, no continuation: {answer:?}"
    );
    assert_eq!(a, answer, "the answer is typed as with no goal read");
    let next = reads(lost("c1", true), worked("The lexer is patched."));
    assert_eq!(
        act(&mut st, &next, t + 3 * MIN),
        restored(),
        "at the next continuation"
    );
}

/// A CONDITION THAT CANNOT BE TYPED BACK AS IT WAS IS NEVER TYPED CHANGED: a goal read lost whose
/// condition held a line break, or ran past Claude Code's limit, gets the plain continuation, as
/// with no goal read, and the harness restores it no more (the goal-cleared exception stands for
/// it). Negative control: the same clear with a typable line is restored.
#[test]
fn an_untypable_condition_is_left_cleared() {
    let t = t0();
    for why in [Untypable::Breaks, Untypable::Long] {
        let goal = match lost("c1", true) {
            GoalRead::Lost(lost) => GoalRead::Lost(Lost {
                line: Err(why),
                ..lost
            }),
            other => other,
        };
        let mut st = TurnEndState::default();
        let r = reads(goal, worked("Suites are running."));
        let a = act(&mut st, &r, t);
        let mut control = TurnEndState::default();
        assert_eq!(
            a,
            act(&mut control, &worked("Suites are running."), t),
            "{why:?}: as with no goal read"
        );
        assert!(!st.goal_restoring(&r), "{why:?}: not restored");
    }
    let mut st = TurnEndState::default();
    let r = reads(lost("c1", true), worked("Suites are running."));
    assert!(st.goal_restoring(&r));
    assert_eq!(act(&mut st, &r, t), restored());
}
