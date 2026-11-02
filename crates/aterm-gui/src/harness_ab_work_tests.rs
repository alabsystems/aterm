// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The A/B book's per-session sum of the agent's busy time after the move that
//! opened its task, which the one end's floor reads (`harness_task::is_a_task_end`).

use super::*;
use AbArm::{Cruise, Land};
use AbPhase::{AtBox, AtPrompt, Busy};

/// A local day: 2024-10-06, in UTC.
const DAY: i64 = 20_002;
/// Its noon, unix ms.
const NOON_MS: i64 = DAY * 86_400_000 + 12 * 3_600_000;
const SEC: u64 = 1_000;

fn utc(_unix: i64) -> Option<i64> {
    Some(0)
}

/// A book whose calendar read noon at ledger instant 0.
fn book() -> AbBook {
    let mut book = AbBook::default();
    book.set_calendar(AbCalendar::read(0, NOON_MS, utc));
    book
}

/// Busy 10 s → 70 s with the calendar read again at 40 s (a moved wall,
/// `Wake::WallMoved`), then 15 s more after a stretch at the prompt — 75 s,
/// as the arm's own busy time says; an open busy stretch counts up to the
/// instant read.
#[test]
fn the_sum_is_the_busy_time_charged_across_a_calendar_read() {
    let mut book = book();
    book.observe(1, Some(Busy), None, Cruise, 10 * SEC);
    book.set_calendar(AbCalendar::read(40 * SEC, NOON_MS + 40_000, utc));
    book.observe(1, Some(AtPrompt), None, Cruise, 70 * SEC);
    assert_eq!(
        book.worked(1, 80 * SEC),
        60 * SEC,
        "the read split nothing off"
    );
    book.observe(1, Some(Busy), None, Cruise, 80 * SEC);
    assert_eq!(
        book.worked(1, 95 * SEC),
        75 * SEC,
        "an open busy stretch counts up to the instant read"
    );
    assert_eq!(
        book.week_at(95 * SEC, &[]).arm(Cruise).busy_ms,
        75 * SEC,
        "the arm's own busy time"
    );
    book.observe(1, Some(AtPrompt), None, Cruise, 95 * SEC);
    assert_eq!(book.worked(1, 200 * SEC), 75 * SEC);
    assert_eq!(book.worked(2, 200 * SEC), 0, "a session never charged");
}

/// A busy piece open since 0 when the task opens at 20 s, closed at 50 s, is
/// 30 s of the task's work; the same while it is still open, read at 35 s, is
/// 15 s; and an opening zeroes what came before.
#[test]
fn a_busy_piece_counts_only_its_part_after_the_task_opened() {
    let mut book = book();
    book.observe(1, Some(Busy), None, Cruise, 0);
    book.task_opened(1, 20 * SEC);
    assert_eq!(book.worked(1, 35 * SEC), 15 * SEC, "open, read at 35 s");
    book.observe(1, Some(AtPrompt), None, Cruise, 50 * SEC);
    assert_eq!(book.worked(1, 60 * SEC), 30 * SEC, "closed at 50 s");
    book.observe(2, Some(Busy), None, Cruise, 100 * SEC);
    book.observe(2, Some(AtPrompt), None, Cruise, 140 * SEC);
    assert_eq!(book.worked(2, 141 * SEC), 40 * SEC);
    book.task_opened(2, 141 * SEC);
    assert_eq!(
        book.worked(2, 141 * SEC),
        0,
        "the reply ran before the opening"
    );
    book.observe(3, Some(Busy), None, Cruise, 200 * SEC);
    book.task_opened(3, 240 * SEC);
    book.observe(3, Some(AtPrompt), None, Cruise, 240 * SEC + 300);
    assert_eq!(book.worked(3, 241 * SEC), 300, "the lag alone");
}

/// `observe(…, None, …)` drops the session's open stretch, but the sum lives in
/// its task, apart — 20 s, a wall, then 15 s is 35 s.
#[test]
fn a_walls_word_between_two_busy_pieces_keeps_the_sum() {
    let mut book = book();
    book.observe(1, Some(Busy), None, Cruise, 0);
    book.observe(1, None, None, Cruise, 20 * SEC);
    assert_eq!(book.stretch(1), None, "the wall dropped the stretch");
    book.observe(1, Some(Busy), None, Cruise, 30 * SEC);
    book.observe(1, Some(AtPrompt), None, Cruise, 45 * SEC);
    assert_eq!(book.worked(1, 50 * SEC), 35 * SEC);
    book.retire(1, 60 * SEC);
    assert_eq!(book.worked(1, 70 * SEC), 0);
}

/// The mode moved into Land at 1 min, a first landing at 7 min, the next task
/// opened at 10 min and landed at 12 min is one sample in `bucket(2 min)`, not
/// in that of 11 min (from the mode's move).
#[test]
fn a_task_opened_under_land_starts_the_span_again() {
    const MIN: u64 = 60 * SEC;
    assert_ne!(bucket(2 * MIN), bucket(11 * MIN), "the control can tell");
    let mut book = book();
    book.follow_mode(1, false, 0);
    book.follow_mode(1, true, MIN);
    book.observe(1, Some(Busy), None, Land, MIN);
    book.landed(1, 7 * MIN);
    book.task_opened(1, 7 * MIN);
    book.task_opened(1, 10 * MIN);
    book.landed(1, 12 * MIN);
    let ends = book.week_at(12 * MIN, &[]).ends;
    assert_eq!(ends.landed, 2);
    assert_eq!(
        (ends.landing[bucket(6 * MIN)], ends.landing[bucket(2 * MIN)]),
        (1, 1),
        "each landing timed from its own start: {:?}",
        ends.landing
    );
    book.follow_mode(2, false, 0);
    book.task_opened(2, MIN);
    assert_eq!(book.land_from(2), None);
    book.task_opened(3, MIN);
    assert_eq!(book.land_from(3), None);
    book.follow_mode(3, true, 2 * MIN);
    assert_eq!(book.land_from(3), None, "first read in Land: no start");
}

/// `AbBook::carry_mode` reads the mode as a first read does — Land with no
/// start — but the work the task has done is the task's, kept: 25 s, the word
/// carried into Land, then 10 s is 35 s.
#[test]
fn a_carried_word_keeps_the_tasks_work() {
    let mut book = book();
    book.follow_mode(1, false, 0);
    book.task_opened(1, SEC);
    book.observe(1, Some(Busy), None, Cruise, SEC);
    book.observe(1, Some(AtPrompt), None, Cruise, 26 * SEC);
    book.carry_mode(1, true);
    assert_eq!(book.land_from(1), None, "carried into Land: no start");
    book.observe(1, Some(Busy), None, Land, 30 * SEC);
    book.observe(1, Some(AtPrompt), None, Land, 40 * SEC);
    assert_eq!(book.worked(1, 41 * SEC), 35 * SEC);
}

/// The floor weighs the agent's work under the harness, and under Off the work
/// is the person's — 40 s under Off counts nothing, 40 s under Cruise counts 40 s.
#[test]
fn busy_time_under_off_is_no_task_work() {
    let mut book = book();
    book.follow_mode(1, false, 0);
    book.observe(1, Some(Busy), None, AbArm::Off, 10 * SEC);
    book.observe(1, Some(AtPrompt), None, AbArm::Off, 50 * SEC);
    assert_eq!(book.worked(1, 60 * SEC), 0, "Off's work is the person's");
    book.observe(1, Some(Busy), None, AbArm::Off, 60 * SEC);
    assert_eq!(book.worked(1, 70 * SEC), 0, "nor an open stretch under Off");
    book.observe(1, Some(Busy), None, Cruise, 70 * SEC);
    assert_eq!(book.worked(1, 110 * SEC), 40 * SEC, "Cruise's open stretch");
    book.observe(1, Some(Busy), None, AbArm::Unsupervised, 110 * SEC);
    book.observe(1, Some(AtPrompt), None, AbArm::Unsupervised, 115 * SEC);
    assert_eq!(book.worked(1, 120 * SEC), 45 * SEC, "unclaimed is counted");
    book.observe(1, Some(Busy), None, AbArm::Off, 120 * SEC);
    book.set_calendar(AbCalendar::read(130 * SEC, NOON_MS + 130_000, utc));
    book.observe(1, Some(AtPrompt), None, AbArm::Off, 140 * SEC);
    assert_eq!(book.worked(1, 150 * SEC), 45 * SEC, "nor across a read");
}

/// THE LAND CHECK'S TURN IS NOT THE TASK'S WORK (v6 §14.17.13, departure 16): 20 s of work, the
/// check's act at 30 s, its turn busy 32 s → 42 s, is 20 s of the task's work; a busy piece the
/// book had open before the check (a late read of the turn before it) charges up to the check;
/// the next act typed ends the audit, so the turn after it counts again.
/// Negative control: the check's turn charged as any other (30 s).
#[test]
fn the_land_checks_turn_is_not_the_tasks_work() {
    let mut book = book();
    book.observe(1, Some(Busy), None, Land, 0);
    book.observe(1, Some(AtPrompt), None, Land, 20 * SEC);
    book.act_typed(1, Act::LandCheck, 30 * SEC);
    book.observe(1, Some(Busy), None, Land, 32 * SEC);
    assert_eq!(book.worked(1, 40 * SEC), 20 * SEC, "open, the check's turn");
    book.observe(1, Some(AtPrompt), None, Land, 42 * SEC);
    assert_eq!(
        book.worked(1, 50 * SEC),
        20 * SEC,
        "the check's turn closed"
    );
    book.observe(1, Some(Busy), None, Land, 60 * SEC);
    book.observe(1, Some(AtPrompt), None, Land, 70 * SEC);
    assert_eq!(
        book.worked(1, 80 * SEC),
        30 * SEC,
        "the turn after it counts"
    );
    // A nudge typed ends the audit before its turn.
    book.act_typed(3, Act::LandCheck, 0);
    book.act_typed(3, Act::Nudged, 5 * SEC);
    book.observe(3, Some(Busy), None, Land, 6 * SEC);
    book.observe(3, Some(AtPrompt), None, Land, 16 * SEC);
    assert_eq!(book.worked(3, 20 * SEC), 10 * SEC, "a nudge's turn counts");
}

/// THE CHECK'S TURN IS NEVER WORK, HOWEVER IT IS INTERRUPTED (the post-landing review,
/// 2026-11-01): 20 s of work, then the check's turn busy 32 s → 35 s, a box at 35 s approved at
/// 40 s (its act inside the turn), a dropped phase read at 44 s, busy again to 52 s: still 20 s.
/// A wall's prompt inside the check's turn and the resume aterm types past it charge nothing
/// either. CONTROL: the same box and the same resume in a nudge's turn are its work.
#[test]
fn a_box_or_a_wall_inside_the_land_checks_turn_charges_nothing() {
    let mut book = book();
    let at = |s: u64| s * SEC;
    book.observe(1, Some(Busy), None, Land, 0);
    book.observe(1, Some(AtPrompt), None, Land, at(20));
    book.act_typed(1, Act::LandCheck, at(30));
    book.observe(1, Some(Busy), None, Land, at(32));
    book.observe(1, Some(AtBox), None, Land, at(35));
    book.act_typed(1, Act::Approved, at(40));
    book.observe(1, Some(Busy), None, Land, at(40));
    book.observe(1, None, None, Land, at(44));
    book.observe(1, Some(Busy), None, Land, at(45));
    assert_eq!(book.worked(1, at(50)), at(20), "open, past the box");
    book.observe(1, Some(AtPrompt), None, Land, at(52));
    assert_eq!(book.worked(1, at(60)), at(20), "the box charged nothing");
    // A wall's prompt cut the check's turn; the resume aterm types goes on with it.
    book.act_typed(2, Act::LandCheck, 0);
    book.observe(2, Some(Busy), None, Land, at(1));
    book.observe(2, Some(AtPrompt), None, Land, at(5));
    book.act_typed(2, Act::Resumed, at(65));
    book.observe(2, Some(Busy), None, Land, at(66));
    book.observe(2, Some(AtPrompt), None, Land, at(80));
    assert_eq!(
        book.worked(2, at(90)),
        0,
        "the resumed check charged nothing"
    );
    // CONTROL: the same in a nudge's turn.
    book.act_typed(3, Act::Nudged, 0);
    book.observe(3, Some(Busy), None, Land, at(1));
    book.observe(3, Some(AtBox), None, Land, at(5));
    book.act_typed(3, Act::Approved, at(6));
    book.observe(3, Some(Busy), None, Land, at(6));
    book.observe(3, Some(AtPrompt), None, Land, at(10));
    book.act_typed(3, Act::Resumed, at(20));
    book.observe(3, Some(Busy), None, Land, at(20));
    book.observe(3, Some(AtPrompt), None, Land, at(30));
    assert_eq!(book.worked(3, at(40)), at(18), "a nudge's turn is work");
}

/// THE CHECK'S ACT REACHES THE BOOK AFTER ITS TURN BEGAN (the post-landing review, 2026-11-01):
/// the busy run open when it arrives is left out from its start, what of it was charged taken
/// back. That run may be the check's or the turn before it read late, so the prompt it comes back
/// to ends nothing; the next run, begun past the act, is taken as the check's own, and the prompt
/// after it ends the audit: the person's turn there is left out, and the turn after it counts. A
/// task opened past the check ends it too. A run open at the act that was the turn before it read
/// late charges nothing, and the check's turn after it charges nothing: a missing cheer, never a
/// false one.
#[test]
fn the_checks_act_arriving_inside_its_turn_takes_the_turn_back() {
    let mut book = book();
    let at = |s: u64| s * SEC;
    book.observe(1, Some(Busy), None, Land, 0);
    book.observe(1, Some(AtPrompt), None, Land, at(20));
    book.observe(1, Some(Busy), None, Land, at(30));
    book.set_calendar(AbCalendar::read(at(31), NOON_MS + 31_000, utc));
    book.act_typed(1, Act::LandCheck, at(32));
    assert_eq!(
        book.worked(1, at(35)),
        at(20),
        "the running check taken back"
    );
    book.observe(1, Some(AtPrompt), None, Land, at(42));
    // The person's next turn.
    book.observe(1, Some(Busy), None, Land, at(50));
    book.observe(1, Some(AtPrompt), None, Land, at(60));
    book.observe(1, Some(Busy), None, Land, at(61));
    book.observe(1, Some(AtPrompt), None, Land, at(71));
    assert_eq!(
        book.worked(1, at(80)),
        at(30),
        "the turn after the check's own: one turn left out, then work again"
    );
    // A task opened past the check ends its audit.
    book.observe(2, Some(Busy), None, Land, 0);
    book.act_typed(2, Act::LandCheck, at(1));
    book.observe(2, Some(AtPrompt), None, Land, at(5));
    book.task_opened(2, at(10));
    book.observe(2, Some(Busy), None, Land, at(11));
    book.observe(2, Some(AtPrompt), None, Land, at(21));
    assert_eq!(book.worked(2, at(30)), at(10), "the new task's turn counts");
    // The turn before the check, read late: left out, and the check's turn after it too.
    book.observe(4, Some(Busy), None, Land, 0);
    book.act_typed(4, Act::LandCheck, at(10));
    book.observe(4, Some(AtPrompt), None, Land, at(12));
    book.observe(4, Some(Busy), None, Land, at(13));
    book.observe(4, Some(AtPrompt), None, Land, at(25));
    assert_eq!(book.worked(4, at(30)), 0, "nothing past what is certain");
    book.observe(4, Some(Busy), None, Land, at(40));
    book.observe(4, Some(AtPrompt), None, Land, at(50));
    assert_eq!(book.worked(4, at(60)), at(10), "the turn after the check's");
}

/// A PROSE ANSWER ATERM TYPED PAST THE CHECK OPENS A TURN OF ITS OWN (the second review of the
/// post-landing fixes, 2026-11-01): the check's act read late inside its turn (busy 30 s → 42 s),
/// then `go-ahead@v1` typed at its answer, an `Answered` that opens a turn: its run 50 s → 60 s
/// is the task's work, 10 s past the 20 s before the check. CONTROL: a dialog's keys answered
/// inside the check's turn (an `Answered` that opens none) end nothing, and the check's turn past
/// them charges nothing. Negative control: the typed answer read as a dialog's keys
/// (`AbBook::turn_opened` ending nothing) leaves the go-ahead's run out (20 s).
#[test]
fn a_prose_answer_typed_past_the_check_is_the_tasks_work() {
    let mut book = book();
    let at = |s: u64| s * SEC;
    book.observe(1, Some(Busy), None, Land, 0);
    book.observe(1, Some(AtPrompt), None, Land, at(20));
    book.observe(1, Some(Busy), None, Land, at(30));
    book.act_typed(1, Act::LandCheck, at(32));
    book.observe(1, Some(AtPrompt), None, Land, at(42));
    book.act_typed(1, Act::Answered, at(45));
    book.turn_opened(1, Act::Answered);
    book.observe(1, Some(Busy), None, Land, at(50));
    book.observe(1, Some(AtPrompt), None, Land, at(60));
    assert_eq!(
        book.worked(1, at(70)),
        at(30),
        "the go-ahead's turn is work"
    );
    // CONTROL: a dialog's keys inside the check's turn.
    book.observe(2, Some(Busy), None, Land, 0);
    book.observe(2, Some(AtPrompt), None, Land, at(20));
    book.act_typed(2, Act::LandCheck, at(30));
    book.observe(2, Some(Busy), None, Land, at(32));
    book.observe(2, Some(AbPhase::Asked), None, Land, at(35));
    book.act_typed(2, Act::Answered, at(36));
    book.observe(2, Some(Busy), None, Land, at(36));
    book.observe(2, Some(AtPrompt), None, Land, at(42));
    assert_eq!(
        book.worked(2, at(50)),
        at(20),
        "the keys' turn is the check's"
    );
}
