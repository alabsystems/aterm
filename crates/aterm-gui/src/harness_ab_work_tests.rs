// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WORK SINCE A TASK OPENED (`docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §11.2.3, item 2; §11.2.5 "The work sum"): the A/B book's per-session sum of
//! the agent's busy time after the move that opened its task, which the one
//! end's floor reads (`harness_task::is_a_task_end`).

use super::*;
use AbArm::{Cruise, Land};
use AbPhase::{AtPrompt, Busy};

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

/// THE SUM IS THE BUSY TIME CHARGED, ACROSS A CALENDAR READ MID-STRETCH: busy
/// 10 s → 70 s with the calendar read again at 40 s (a moved wall,
/// `Wake::WallMoved`), then 15 s more after a stretch at the prompt — 75 s,
/// as the arm's own busy time says; an open busy stretch counts up to the
/// instant read. NEGATIVE CONTROL: the add dropped from `set_calendar`'s
/// split loses the 30 s before the read (45 s), so this fails.
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
    // At the prompt and at a box is no work.
    book.observe(1, Some(AtPrompt), None, Cruise, 95 * SEC);
    assert_eq!(book.worked(1, 200 * SEC), 75 * SEC);
    assert_eq!(book.worked(2, 200 * SEC), 0, "a session never charged");
}

/// ONLY THE PART AFTER THE OPENING COUNTS (the clip): a busy piece open since
/// 0 when the task opens at 20 s, closed at 50 s, is 30 s of the task's work;
/// the same while it is still open, read at 35 s, is 15 s; and an opening
/// zeroes what came before. A piece wholly before the opening counts nothing.
/// NEGATIVE CONTROL: the whole piece counted (50 s) fails it.
#[test]
fn a_busy_piece_counts_only_its_part_after_the_task_opened() {
    let mut book = book();
    book.observe(1, Some(Busy), None, Cruise, 0);
    book.task_opened(1, 20 * SEC);
    assert_eq!(book.worked(1, 35 * SEC), 15 * SEC, "open, read at 35 s");
    book.observe(1, Some(AtPrompt), None, Cruise, 50 * SEC);
    assert_eq!(book.worked(1, 60 * SEC), 30 * SEC, "closed at 50 s");
    // The done seed's withdrawal: a reply of 40 s that ran before the move
    // that opened the task counts none of it.
    book.observe(2, Some(Busy), None, Cruise, 100 * SEC);
    book.observe(2, Some(AtPrompt), None, Cruise, 140 * SEC);
    assert_eq!(book.worked(2, 141 * SEC), 40 * SEC);
    book.task_opened(2, 141 * SEC);
    assert_eq!(
        book.worked(2, 141 * SEC),
        0,
        "the reply ran before the opening"
    );
    // The published word told after the loop's point: only the lag counts.
    book.observe(3, Some(Busy), None, Cruise, 200 * SEC);
    book.task_opened(3, 240 * SEC);
    book.observe(3, Some(AtPrompt), None, Cruise, 240 * SEC + 300);
    assert_eq!(book.worked(3, 241 * SEC), 300, "the lag alone");
}

/// A WALL'S WORD BETWEEN TWO BUSY PIECES KEEPS THE SUM: `observe(…, None, …)`
/// drops the session's open stretch, but the sum lives in its task, apart —
/// 20 s, a wall, then 15 s is 35 s. NEGATIVE CONTROL: the sum kept in the
/// stretch's clock would go with the wall (15 s).
#[test]
fn a_walls_word_between_two_busy_pieces_keeps_the_sum() {
    let mut book = book();
    book.observe(1, Some(Busy), None, Cruise, 0);
    book.observe(1, None, None, Cruise, 20 * SEC);
    assert_eq!(book.stretch(1), None, "the wall dropped the stretch");
    book.observe(1, Some(Busy), None, Cruise, 30 * SEC);
    book.observe(1, Some(AtPrompt), None, Cruise, 45 * SEC);
    assert_eq!(book.worked(1, 50 * SEC), 35 * SEC);
    // Only the session's end forgets it.
    book.retire(1, 60 * SEC);
    assert_eq!(book.worked(1, 70 * SEC), 0);
}

/// THE TASK'S OPENING UNDER LAND STARTS THE TIME FROM LAND TO LANDED AGAIN
/// (§11.6.3, item 4: `task_opened`'s reset of `land_from`): the mode moved
/// into Land at 1 min, a first landing at 7 min, the next task opened at 10
/// min and landed at 12 min is one sample in `bucket(2 min)`, not in that of
/// 11 min (from the mode's move). An opening outside Land starts nothing, and
/// a book that never read the mode starts nothing either (the first read of
/// Land has no start). NEGATIVE CONTROL: without the reset, the second
/// landing is a count with no sample — the test's second assertion fails.
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
    // Under Cruise an opening starts no span.
    book.follow_mode(2, false, 0);
    book.task_opened(2, MIN);
    assert_eq!(book.land_from(2), None);
    // Never read: no start, though it opened.
    book.task_opened(3, MIN);
    assert_eq!(book.land_from(3), None);
    book.follow_mode(3, true, 2 * MIN);
    assert_eq!(book.land_from(3), None, "first read in Land: no start");
}

/// A WORD CARRIED FROM BEFORE THIS PROCESS MOVES NO TASK (`AbBook::carry_mode`):
/// it reads the mode as a first read does — Land
/// with no start — but the work the task has done is the task's, kept: 25 s,
/// the word carried into Land, then 10 s is 35 s. NEGATIVE CONTROL: the
/// carry as a fresh entry zeroes the
/// sum (10 s), so this fails.
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

/// OFF'S BUSY TIME IS NO TASK WORK (§11.2.3, item 2; §11.2.4 "Off, the
/// agent's own switch, the master switch"): the floor weighs the agent's
/// work under the harness, and under Off the work is the person's — 40 s
/// under Off counts nothing, 40 s under Cruise counts 40 s. A stretch with
/// no claim of this host's under a supervised word (`Unsupervised`: the
/// agent not yet attached, a loop between its runs, a successor before its
/// loop claims) is still the harness's task, and counts. NEGATIVE CONTROL:
/// the arm not read in `observe`'s add (every busy piece counted, as built
/// first) counts Off's 40 s, so this fails.
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
    // A calendar read mid-stretch under Off splits off nothing either.
    book.observe(1, Some(Busy), None, AbArm::Off, 120 * SEC);
    book.set_calendar(AbCalendar::read(130 * SEC, NOON_MS + 130_000, utc));
    book.observe(1, Some(AtPrompt), None, AbArm::Off, 140 * SEC);
    assert_eq!(book.worked(1, 150 * SEC), 45 * SEC, "nor across a read");
}
