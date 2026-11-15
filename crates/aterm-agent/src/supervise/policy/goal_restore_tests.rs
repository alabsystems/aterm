// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The restore's memory on its own: the wait between restores, when an error has passed, what
// followed a restore, and what a ledger row carries to a later loop. The decisions at a session's
// points are `turn_end_goal_restore_tests.rs`.

use super::*;

const MIN: Duration = Duration::from_secs(60);

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

fn of(read: &GoalRead) -> &Lost {
    match read {
        GoalRead::Lost(lost) => lost,
        GoalRead::Unread | GoalRead::Off | GoalRead::Active(_) => panic!("not lost: {read:?}"),
    }
}

/// The wait doubles from ten minutes at each restore in a row and stops at six hours.
#[test]
fn the_wait_doubles_to_its_cap() {
    assert_eq!(backoff(0), 10 * MIN);
    assert_eq!(backoff(1), 10 * MIN);
    assert_eq!(backoff(2), 20 * MIN);
    assert_eq!(backoff(3), 40 * MIN);
    assert_eq!(backoff(6), 320 * MIN);
    assert_eq!(backoff(7), GOAL_RESTORE_CAP);
    assert_eq!(backoff(u32::MAX), GOAL_RESTORE_CAP);
}

/// An error has passed once a response was served after the clear, or once a wall a point drew
/// while it read the clear is gone. Negative control: a clear no point read under a wall, with
/// nothing served, has not passed, nor has one whose wall still stands.
#[test]
fn an_error_passes_at_a_served_answer_or_its_wall_gone() {
    let t = Instant::now();
    let mut r = GoalRestores::default();
    let served = lost("a", true);
    assert!(r.passed(of(&served), true), "served after the clear");
    let unserved = lost("a", false);
    r.observe(&unserved, false, t);
    assert!(
        !r.passed(of(&unserved), false),
        "no wall seen, nothing served"
    );
    r.observe(&unserved, true, t);
    assert!(!r.passed(of(&unserved), true), "its wall still stands");
    assert!(r.passed(of(&unserved), false), "its wall is gone");
    let next = lost("b", false);
    r.observe(&next, false, t);
    assert!(!r.passed(of(&next), false), "the wall was another clear's");
}

/// One restore per clear; another clear waits out the wait after the last restore; a clear first
/// read long after the last restore starts the doubling over.
#[test]
fn one_restore_per_clear_and_the_wait_between() {
    let t = Instant::now();
    let mut r = GoalRestores::default();
    let a = lost("a", true);
    r.observe(&a, false, t);
    assert!(r.due(of(&a), t));
    r.typed(&command(CONDITION), t, 1);
    assert!(
        !r.due(of(&a), t + GOAL_RESTORE_CAP),
        "never twice for one clear"
    );
    let b = lost("b", true);
    r.observe(&b, false, t + MIN);
    assert!(!r.due(of(&b), t + 10 * MIN - Duration::from_secs(1)));
    assert!(r.due(of(&b), t + 10 * MIN));
    r.typed(&command(CONDITION), t + 10 * MIN, 2);
    let c = lost("c", true);
    r.observe(&c, false, t + 11 * MIN);
    assert!(
        !r.due(of(&c), t + 29 * MIN),
        "the second restore's wait is twenty minutes"
    );
    assert!(r.due(of(&c), t + 30 * MIN));
    r.typed(&command(CONDITION), t + 30 * MIN, 3);
    let d = lost("d", true);
    let late = t + 30 * MIN + GOAL_RESTORE_CAP;
    r.observe(&d, false, late);
    r.typed(&command(CONDITION), late, 4);
    assert_eq!(
        r.last(),
        Some(("d", 1)),
        "a clear first read six hours after the last restore starts the doubling over"
    );
}

/// A GOAL CLAUDE CODE CLEARS AGAIN AFTER EVERY RESTORE SETTLES AT ONE RESTORE EVERY SIX HOURS:
/// each clear read a minute after the restore before it, each restore typed the moment it is due,
/// for ten rungs. The waits double to the cap and stay there. Negative control: the rung is held
/// by when each clear was first read, never by the gap between restores, which at the cap is the
/// cap itself.
#[test]
fn a_clear_after_every_restore_holds_the_wait_at_its_cap() {
    let t = Instant::now();
    let mut r = GoalRestores::default();
    let mut at = t;
    let mut waits = Vec::new();
    for n in 0..11 {
        let clear = lost(&format!("c{n}"), true);
        r.observe(&clear, false, at + MIN);
        let due = (0..)
            .map(|m| at + MIN * m)
            .find(|&when| r.due(of(&clear), when))
            .expect("due");
        r.typed(&command(CONDITION), due, n);
        if n > 0 {
            waits.push(due.saturating_duration_since(at));
        }
        at = due;
    }
    let minutes: Vec<u64> = waits.iter().map(|w| w.as_secs() / 60).collect();
    assert_eq!(
        minutes,
        [10, 20, 40, 80, 160, 320, 360, 360, 360, 360],
        "every wait from rung 7 on is the cap"
    );
    assert_eq!(r.last().map(|(_, rung)| rung), Some(11));
}

/// What followed a restore is told once: the condition set again; at a later point the same
/// clear still standing, or the goal moved on. Negative control: the restore's own point, and an
/// unread goal, tell nothing.
#[test]
fn a_restores_outcome_is_told_once() {
    let t = Instant::now();
    let a = lost("a", true);
    let typed = |point| {
        let mut r = GoalRestores::default();
        r.observe(&a, false, t);
        r.typed(&command(CONDITION), t, point);
        r
    };
    let mut r = typed(4);
    assert_eq!(r.outcome(&a, 4), None, "its own point");
    assert_eq!(r.outcome(&GoalRead::Unread, 5), None, "nothing read");
    assert_eq!(
        r.outcome(&GoalRead::Active(CONDITION.to_string()), 5),
        Some(Outcome::Restored(CONDITION.to_string()))
    );
    assert_eq!(r.outcome(&GoalRead::Active(CONDITION.to_string()), 6), None);
    let mut r = typed(4);
    assert_eq!(
        r.outcome(&a, 5),
        Some(Outcome::Unconfirmed(CONDITION.to_string()))
    );
    let mut r = typed(4);
    assert_eq!(
        r.outcome(&GoalRead::Off, 5),
        Some(Outcome::Moved(CONDITION.to_string()))
    );
    let mut r = typed(4);
    assert_eq!(
        r.outcome(&GoalRead::Active("another goal".to_string()), 5),
        Some(Outcome::Moved(CONDITION.to_string()))
    );
    assert!(
        Outcome::Restored(String::new())
            .reason()
            .contains("set again")
    );
}

/// THE HARNESS RESTORES A CLEAR UNTIL ITS RESTORE GOES UNCONFIRMED, and never one it cannot type
/// back as it was: such a clear is told once as untypable, and an unconfirmed restore is never
/// typed again. Negative control: a typable clear whose restore is owed, typed, or confirmed is
/// being restored.
#[test]
fn a_clear_is_restored_unless_untypable_or_unconfirmed() {
    let t = Instant::now();
    let mut r = GoalRestores::default();
    let a = lost("a", true);
    assert!(r.restoring(of(&a)), "owed");
    r.observe(&a, false, t);
    r.typed(&command(CONDITION), t, 1);
    assert!(r.restoring(of(&a)), "typed, its outcome not yet read");
    assert_eq!(
        r.outcome(&a, 2),
        Some(Outcome::Unconfirmed(CONDITION.to_string()))
    );
    assert!(!r.restoring(of(&a)), "unconfirmed");
    assert_eq!(
        r.held(of(&a), Seat::AT_BREAK, t + GOAL_RESTORE_CAP),
        Err(Held::Typed)
    );
    let b = lost("b", true);
    assert!(r.restoring(of(&b)), "another clear is owed its own");

    let untypable = GoalRead::Lost(Lost {
        line: Err(Untypable::Breaks),
        ..of(&lost("u", true)).clone()
    });
    let mut r = GoalRestores::default();
    assert!(!r.restoring(of(&untypable)));
    assert_eq!(
        r.held(of(&untypable), Seat::AT_BREAK, t),
        Err(Held::Untypable(Untypable::Breaks))
    );
    assert_eq!(r.untypable(&untypable), Some(Untypable::Breaks));
    assert_eq!(r.untypable(&untypable), None, "once per clear");
    assert_eq!(r.untypable(&a), None, "a typable clear");
}

/// A RESTORE'S LEDGER ROW CARRIES ITS CLEAR AND RUNG TO A LATER LOOP: a loop seeded from it types
/// no restore of that clear and waits its back-off before another's. Negative control: a reason
/// that names no restore, or a clear id longer than a row id, carries nothing.
#[test]
fn a_restores_row_carries_its_clear_and_rung_to_a_later_loop() {
    // At a break the row names the carry's streak after it too.
    let reason =
        crate::supervise::approvals::background_carry_reason(&typed_reason("why", "c-1", 3), 2);
    assert_eq!(restore_of(&reason), Some(("c-1".to_string(), 3)));
    assert_eq!(restore_of("the turn-end policy judged the point"), None);
    assert_eq!(
        restore_of(&typed_reason("why", &"x".repeat(ROW_ID_BYTES + 1), 1)),
        None
    );
    let t = Instant::now();
    let mut r = GoalRestores::default();
    r.seed("c-1".to_string(), 3, t);
    let c1 = lost("c-1", true);
    assert!(
        !r.due(of(&c1), t + GOAL_RESTORE_CAP),
        "never twice for one clear"
    );
    let c2 = lost("c-2", true);
    assert!(
        !r.due(of(&c2), t + 39 * MIN),
        "the third restore's wait is forty minutes"
    );
    assert!(r.due(of(&c2), t + 40 * MIN));
    assert_eq!(
        r.outcome(&c1, 9),
        None,
        "a seeded restore's outcome was its loop's to tell"
    );
}
