// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::claude_lights::Clock;
use AbArm::{Cruise, Land, Off, Unsupervised};
use AbPhase::{Asked, AtBox, AtPrompt, Busy};

/// A local day: 2024-10-06, a Sunday (so six days before is a Monday).
const DAY: i64 = 20_002;
/// Its midnight, unix ms, in UTC.
const MIDNIGHT_MS: i64 = DAY * 86_400_000;
const HOUR: u64 = 3_600_000;
const HOUR_MS: i64 = 3_600_000;

fn utc(_unix: i64) -> Option<i64> {
    Some(0)
}

/// A book whose calendar read `unix_ms` at ledger instant 0, in UTC.
fn book_at(unix_ms: i64) -> AbBook {
    let mut book = AbBook::default();
    book.set_calendar(AbCalendar::read(0, unix_ms, utc));
    book
}

/// Noon of [`DAY`].
const NOON_MS: i64 = MIDNIGHT_MS + 12 * HOUR_MS;

/// The ring's day `day` at `now`.
fn day_at(book: &AbBook, now: u64, day: i64) -> Option<AbDay> {
    let (ring, _) = book.ring_at(now)?;
    ring.iter().flatten().find(|kept| kept.day == day).copied()
}

/// THE BOOK CHARGES EACH STRETCH TO THE ARM, PHASE AND CAUSE IN FORCE when
/// it ran: a word that moves closes the stretch under the arm, phase and
/// cause of then; an arm that moves mid-phase charges the time so far to the
/// arm that was in force; the open stretches count to the instant read, and
/// a session's end settles its stretch for good. A cause is kept only at the
/// prompt under Cruise or Land, whatever is passed. NEGATIVE CONTROLS: an
/// instant before a stretch's start counts nothing; a session with no
/// stretch open is not charged by its end; a word that is no phase (a wall)
/// counts nothing however long it stands; and a book fed before its
/// calendar is read counts nothing.
#[test]
fn the_book_charges_each_stretch_to_the_arm_phase_and_cause_in_force() {
    let mut book = book_at(NOON_MS);
    // Session 1, Cruise: busy 0..1000, a box 1000..1500, busy 1500..2000,
    // at its prompt, finished, 2000..5000.
    book.observe(1, Some(Busy), None, Cruise, 0);
    book.observe(1, Some(AtBox), None, Cruise, 1_000);
    book.observe(1, Some(Busy), None, Cruise, 1_500);
    book.observe(1, Some(AtPrompt), Some(AbCause::Finished), Cruise, 2_000);
    // Its arm turns Off at the prompt: a cause passed for Off is not kept.
    book.observe(1, Some(AtPrompt), Some(AbCause::Pausing), Off, 5_000);
    assert_eq!(book.stretch(1), Some((AtPrompt, Off, None)));
    // Session 2, Land: asked 0..700, then a wall.
    book.observe(2, Some(Asked), None, Land, 0);
    book.observe(2, None, None, Land, 700);
    let week = book.week_at(6_000, &[]);
    assert_eq!(
        week.arm(Cruise),
        AbArmTotals {
            busy_ms: 1_500,
            at_box_ms: 500,
            at_prompt_ms: 3_000,
            finished_ms: 3_000,
            ..AbArmTotals::default()
        }
    );
    assert_eq!(
        week.arm(Off),
        AbArmTotals {
            at_prompt_ms: 1_000,
            ..AbArmTotals::default()
        },
        "the open stretch counts to the instant read, with no cause"
    );
    assert_eq!(week.arm(Land).asked_ms, 700);
    assert_eq!(week.hist(Cruise, 0)[bucket(500)], 1, "the box: one hold");
    assert_eq!(week.hist(Land, 1)[bucket(700)], 1, "the question: one hold");
    // The wall counts nothing however long it stands.
    book.observe(2, None, None, Land, 90_000);
    assert_eq!(book.week_at(6_000, &[]).arm(Land).measured_ms(), 700);
    // Its end settles session 1's stretch; nothing grows after.
    book.retire(1, 8_000);
    assert_eq!(book.week_at(1_000_000, &[]).arm(Off).at_prompt_ms, 3_000);
    assert_eq!(book.open_stretches(), 0);
    // An instant before a stretch's start counts nothing (never negative).
    book.observe(3, Some(AtPrompt), None, Unsupervised, 10_000);
    book.observe(3, Some(Busy), None, Unsupervised, 9_000);
    book.retire(4, 50_000);
    book.retire(3, 10_000);
    assert_eq!(
        book.week_at(1_000_000, &[]).arm(Unsupervised),
        AbArmTotals::default()
    );
    // Before its calendar is read, the book counts nothing.
    let mut unread = AbBook::default();
    unread.observe(1, Some(Busy), None, Cruise, 0);
    unread.observe(1, None, None, Cruise, 5_000);
    assert_eq!(unread.week_at(5_000, &[]), AbWeek::default());
}

/// EACH PUBLISHED WORD IS ITS PHASE (§10.5.3 B.1): `busy`, `prompt` (at a
/// box), `question` (asked), `idle` (at its prompt); and only a box and a
/// question are holds. The arms are the design's: Cruise and Land each
/// their own under this host's claim (NEGATIVE CONTROL: Land is never
/// Cruise's), Off is plain Claude Code with nobody's claim live, the rest
/// unsupervised.
#[test]
fn each_word_is_its_phase_and_the_arms_are_the_designs() {
    for (word, phase) in [
        ("busy", Busy),
        ("prompt", AtBox),
        ("question", Asked),
        ("idle", AtPrompt),
    ] {
        assert_eq!(AbPhase::of(word), Some(phase), "{word}");
    }
    for word in ["wall:limit", "wall:unresponsive", "survey", "unknown", "-"] {
        assert_eq!(AbPhase::of(word), None, "{word}");
    }
    assert_eq!(
        [Busy, AtBox, Asked, AtPrompt].map(AbPhase::held),
        [None, Some(0), Some(1), None]
    );
    assert_eq!(AbArm::of(AutopilotView::Cruise, true, true), Cruise);
    assert_eq!(AbArm::of(AutopilotView::Land, true, true), Land);
    for view in [AutopilotView::Cruise, AutopilotView::Land] {
        assert_eq!(AbArm::of(view, false, false), Unsupervised, "{view:?}");
        assert_eq!(AbArm::of(view, false, true), Unsupervised, "{view:?}");
    }
    for view in [
        AutopilotView::Off,
        AutopilotView::MasterOff,
        AutopilotView::Unhosted,
    ] {
        assert_eq!(AbArm::of(view, false, false), Off, "{view:?}");
        assert_eq!(AbArm::of(view, false, true), Unsupervised, "{view:?}");
        assert_eq!(AbArm::of(view, true, true), Unsupervised, "{view:?}");
    }
}

/// A CAUSE ONLY UNDER CRUISE AND LAND AT THE PROMPT (§10.5.3 B.2):
/// finished where the face says done or landed — first, an agent done with
/// its work is not waiting on a pause — else pausing where a pause stands.
/// NEGATIVE CONTROLS: Off and unsupervised time has none whatever the glass
/// holds, nor does any other phase; and the book drops a cause passed for
/// one, so Off's prompt time never reads "finished".
#[test]
fn a_cause_is_kept_only_under_cruise_and_land_at_the_prompt() {
    for arm in [Cruise, Land] {
        let at = Some(AtPrompt);
        assert_eq!(AbCause::of(arm, at, true, true), Some(AbCause::Finished));
        assert_eq!(AbCause::of(arm, at, true, false), Some(AbCause::Finished));
        assert_eq!(AbCause::of(arm, at, false, true), Some(AbCause::Pausing));
        assert_eq!(AbCause::of(arm, at, false, false), None);
        for phase in [Some(Busy), Some(AtBox), Some(Asked), None] {
            assert_eq!(AbCause::of(arm, phase, true, true), None, "{phase:?}");
        }
    }
    for arm in [Off, Unsupervised] {
        assert_eq!(AbCause::of(arm, Some(AtPrompt), true, true), None);
    }
    let mut book = book_at(NOON_MS);
    book.observe(1, Some(AtPrompt), Some(AbCause::Finished), Off, 0);
    book.observe(2, Some(AtPrompt), Some(AbCause::Finished), Cruise, 0);
    book.observe(3, Some(Busy), Some(AbCause::Pausing), Cruise, 0);
    let week = book.week_at(1_000, &[]);
    assert_eq!(week.arm(Off).finished_ms, 0);
    assert_eq!(week.arm(Cruise).finished_ms, 1_000, "CONTROL: Cruise's");
    assert_eq!(week.arm(Cruise).pausing_ms, 0, "busy is never pausing");
}

/// THE BUCKET EDGES (§10.5.5): `min(19, ⌊log₂(ms / 250)⌋)` — 249 ms and
/// 250 ms in the first, 500 ms the second, 36 h the 19th (18), and from
/// 2¹⁹ × 250 ms (about 36.4 h) on, the last. NEGATIVE CONTROL: a
/// millisecond under each edge stays below it.
#[test]
fn a_hold_falls_in_its_log2_bucket() {
    assert_eq!(bucket(0), 0);
    assert_eq!(bucket(249), 0);
    assert_eq!(bucket(250), 0);
    assert_eq!(bucket(499), 0);
    assert_eq!(bucket(500), 1);
    assert_eq!(bucket(999), 1);
    assert_eq!(bucket(1_000), 2);
    assert_eq!(bucket(36 * HOUR), 18);
    assert_eq!(bucket((250 << 19) - 1), 18);
    assert_eq!(bucket(250 << 19), 19);
    assert_eq!(bucket(u64::MAX), BUCKETS - 1);
}

/// THE MEDIAN IS THE BUCKET HOLDING THE ⌈n/2⌉-TH SAMPLE, said at its
/// geometric centre with "about" (§10.5.3 B.4). NEGATIVE CONTROL: an even
/// count takes the lower middle, and no sample is no median.
#[test]
fn the_median_is_the_middle_samples_bucket_said_about() {
    let mut hist = [0; BUCKETS];
    assert_eq!(median(&hist), (None, 0));
    hist[1] = 2;
    hist[9] = 1;
    assert_eq!(median(&hist), (Some(707), 3));
    hist[9] = 3;
    assert_eq!(median(&hist), (Some(180_992), 5), "the 3rd of 5");
    let mut even = [0; BUCKETS];
    even[1] = 1;
    even[9] = 1;
    assert_eq!(median(&even), (Some(707), 2), "the 1st of 2");
    for (ms, words) in [
        (353, "about 0.4 s"),
        (707, "about 0.7 s"),
        (5_656, "about 5.7 s"),
        (11_312, "about 11 s"),
        (45_248, "about 45 s"),
        (90_496, "about 2m"),
        (180_992, "about 3m"),
        (2_895_872, "about 48m"),
        (5_791_744, "about 1.6h"),
        (46_333_952, "about 13h"),
        (185_335_808, "about 51h"),
    ] {
        assert_eq!(about_words(ms), words, "{ms}");
    }
}

/// A STRETCH ACROSS THE MIDNIGHT IS SPLIT THERE (§10.5.3 B.5), and the
/// next day's first stretch begins at its midnight. NEGATIVE CONTROL: the
/// same three hours inside one day stay whole, in that day alone.
#[test]
fn a_stretch_across_midnight_is_split_there() {
    let mut book = book_at(MIDNIGHT_MS + 23 * HOUR_MS);
    book.observe(1, Some(Busy), None, Cruise, 0);
    book.observe(1, None, None, Cruise, 3 * HOUR);
    let (_, today) = book.ring_at(3 * HOUR).unwrap();
    assert_eq!(today, DAY + 1);
    let before = day_at(&book, 3 * HOUR, DAY).unwrap();
    let after = day_at(&book, 3 * HOUR, DAY + 1).unwrap();
    assert_eq!(before.arms[Cruise.index()].busy_ms, HOUR);
    assert_eq!(after.arms[Cruise.index()].busy_ms, 2 * HOUR);
    assert_eq!(before.first_unix_ms, MIDNIGHT_MS + 23 * HOUR_MS);
    assert_eq!(after.first_unix_ms, MIDNIGHT_MS + 24 * HOUR_MS);
    assert_eq!(book.week_at(3 * HOUR, &[]).arm(Cruise).busy_ms, 3 * HOUR);

    let mut whole = book_at(NOON_MS);
    whole.observe(1, Some(Busy), None, Cruise, 0);
    whole.observe(1, None, None, Cruise, 3 * HOUR);
    let (ring, today) = whole.ring_at(3 * HOUR).unwrap();
    assert_eq!(today, DAY);
    assert_eq!(ring.iter().flatten().count(), 1);
    assert_eq!(
        day_at(&whole, 3 * HOUR, DAY).unwrap().arms[0].busy_ms,
        3 * HOUR
    );
}

/// EVERY SESSION'S STRETCH ACROSS THE MIDNIGHT IS SPLIT THERE, not only the
/// first one's to cross it: the book's one calendar has moved on to the new
/// day by the time the second session's change charges its time from
/// before the midnight, and that time still goes in the day it was spent —
/// whether the second stretch closes by a change ([`AbBook::observe`]),
/// stands open at a page reading ([`AbBook::ring_at`]), or is charged by a
/// moved wall ([`AbBook::set_calendar`]); a new day's first stretch begins
/// no earlier than its midnight; and a box that ended before the midnight
/// but is told after another session's change crossed it is a sample of
/// the day it ended. NEGATIVE CONTROL: one session alone splits the same
/// way, and a session open since before the ring's oldest day never pushes
/// a kept day out of its slot.
#[test]
fn every_sessions_stretch_across_midnight_is_split_there() {
    let at_23 = MIDNIGHT_MS + 23 * HOUR_MS;
    let busy = |book: &AbBook, now: u64, day| {
        day_at(book, now, day).map(|kept| kept.arms[Cruise.index()].busy_ms)
    };
    // Both busy from 23:00; session 1 moves at 01:00, session 2 at 02:00.
    let mut book = book_at(at_23);
    book.observe(1, Some(Busy), None, Cruise, 0);
    book.observe(2, Some(Busy), None, Cruise, 0);
    book.observe(1, None, None, Cruise, 2 * HOUR);
    book.observe(2, None, None, Cruise, 3 * HOUR);
    assert_eq!(busy(&book, 3 * HOUR, DAY), Some(2 * HOUR), "an hour each");
    assert_eq!(busy(&book, 3 * HOUR, DAY + 1), Some(3 * HOUR));
    // Both still open at a reading at 02:00.
    let mut open = book_at(at_23);
    open.observe(1, Some(Busy), None, Cruise, 0);
    open.observe(2, Some(Busy), None, Cruise, 0);
    assert_eq!(busy(&open, 3 * HOUR, DAY), Some(2 * HOUR));
    assert_eq!(busy(&open, 3 * HOUR, DAY + 1), Some(4 * HOUR));
    // Both open where the wall is read again at 02:00.
    open.set_calendar(AbCalendar::read(3 * HOUR, at_23 + 3 * HOUR_MS, utc));
    assert_eq!(busy(&open, 3 * HOUR, DAY), Some(2 * HOUR));
    assert_eq!(busy(&open, 3 * HOUR, DAY + 1), Some(4 * HOUR));
    // Fed at 23:00 by session 2; session 1 works 00:30..00:31; session 2
    // moves at 01:00. The old day keeps its hour, and the new day begins at
    // its midnight.
    let mut late = book_at(at_23);
    late.observe(2, Some(Busy), None, Cruise, 0);
    late.observe(1, Some(Busy), None, Cruise, 3 * HOUR / 2);
    late.observe(1, None, None, Cruise, 3 * HOUR / 2 + MIN);
    late.observe(2, None, None, Cruise, 2 * HOUR);
    let old = day_at(&late, 2 * HOUR, DAY).expect("the old day is kept");
    assert_eq!(
        (old.arms[Cruise.index()].busy_ms, old.first_unix_ms),
        (HOUR, at_23)
    );
    let new = day_at(&late, 2 * HOUR, DAY + 1).unwrap();
    assert_eq!(new.arms[Cruise.index()].busy_ms, HOUR + MIN);
    assert_eq!(new.first_unix_ms, MIDNIGHT_MS + 24 * HOUR_MS);
    // A box from 23:00 that ended at 23:50, told after 00:30's change.
    let mut told = book_at(at_23);
    told.observe(2, Some(AtBox), None, Cruise, 0);
    told.observe(1, Some(Busy), None, Cruise, 3 * HOUR / 2);
    told.observe(1, None, None, Cruise, 3 * HOUR / 2 + MIN);
    told.observe(2, Some(Busy), None, Cruise, 50 * MIN);
    let box_day = day_at(&told, 2 * HOUR, DAY).unwrap();
    assert_eq!(box_day.holds[0][0][bucket(50 * MIN)], 1);
    assert_eq!(box_day.arms[Cruise.index()].at_box_ms, 50 * MIN);
    assert_eq!(
        day_at(&told, 2 * HOUR, DAY + 1).unwrap().holds[0][0],
        [0; BUCKETS]
    );

    // NEGATIVE CONTROL: one session alone, as before.
    let mut alone = book_at(at_23);
    alone.observe(2, Some(Busy), None, Cruise, 0);
    alone.observe(2, None, None, Cruise, 3 * HOUR);
    assert_eq!(busy(&alone, 3 * HOUR, DAY), Some(HOUR));
    assert_eq!(busy(&alone, 3 * HOUR, DAY + 1), Some(2 * HOUR));
    // NEGATIVE CONTROL: an agent at its prompt for ten days beside one
    // that works a minute each day: the ring keeps the eight newest days,
    // each with its own day's minute, and the oldest two days' time is in
    // none.
    let mut long = book_at(at_23);
    long.observe(1, Some(AtPrompt), None, Off, 0);
    for d in 1..=10 {
        let noon = (24 * d - 11) * HOUR;
        long.observe(2, Some(Busy), None, Cruise, noon);
        long.observe(2, None, None, Cruise, noon + MIN);
    }
    let now = 10 * 24 * HOUR;
    let (ring, today) = long.ring_at(now).unwrap();
    assert_eq!(today, DAY + 10);
    let mut kept: Vec<_> = ring.iter().flatten().map(|d| d.day).collect();
    kept.sort_unstable();
    assert_eq!(kept, (DAY + 3..=DAY + 10).collect::<Vec<_>>());
    for kept in ring.iter().flatten() {
        let whole = if kept.day == today {
            23 * HOUR
        } else {
            24 * HOUR
        };
        assert_eq!(kept.arms[Cruise.index()].busy_ms, MIN, "{}", kept.day);
        assert_eq!(kept.arms[Off.index()].at_prompt_ms, whole, "{}", kept.day);
    }
}

/// THE MIDNIGHT MOVED BY A MOVED WALL (§10.5.3 B.5, `Wake::WallMoved`): a
/// sleep stops the ledger clock, so at 22:00 the next midnight is two ledger
/// hours off — until the wall is read again. Read after an 8 h sleep, the
/// hour before it stays in its day and the next hour is the new day's. A
/// clock set back charges no negative. NEGATIVE CONTROL: with no read
/// again, both hours land in the old day.
#[test]
fn a_moved_wall_moves_the_midnight() {
    let at_22 = MIDNIGHT_MS + 22 * HOUR_MS;
    let mut slept = book_at(at_22);
    slept.observe(1, Some(Busy), None, Cruise, 0);
    // One ledger hour later, the wall reads 07:00 the next day.
    slept.set_calendar(AbCalendar::read(HOUR, MIDNIGHT_MS + 31 * HOUR_MS, utc));
    let (_, today) = slept.ring_at(2 * HOUR).unwrap();
    assert_eq!(today, DAY + 1);
    assert_eq!(day_at(&slept, 2 * HOUR, DAY).unwrap().arms[0].busy_ms, HOUR);
    assert_eq!(
        day_at(&slept, 2 * HOUR, DAY + 1).unwrap().arms[0].busy_ms,
        HOUR
    );

    let mut unread = book_at(at_22);
    unread.observe(1, Some(Busy), None, Cruise, 0);
    assert_eq!(
        day_at(&unread, 2 * HOUR, DAY).unwrap().arms[0].busy_ms,
        2 * HOUR,
        "NEGATIVE CONTROL: the old midnight is still two ledger hours off"
    );
    assert_eq!(day_at(&unread, 2 * HOUR, DAY + 1), None);

    // A clock set back an hour: the ledger's time is all counted, once.
    let mut back = book_at(NOON_MS);
    back.observe(1, Some(Busy), None, Cruise, 0);
    back.set_calendar(AbCalendar::read(HOUR, NOON_MS, utc));
    assert_eq!(back.week_at(2 * HOUR, &[]).arm(Cruise).busy_ms, 2 * HOUR);
}

/// When the zone springs forward: 02:00 local on [`DAY`] + 1, seconds.
const SPRING: i64 = (DAY + 1) * 86_400 + 10 * 3_600;

/// A zone at UTC−8 that springs forward to UTC−7 at [`SPRING`].
fn springs(unix: i64) -> Option<i64> {
    Some(if unix < SPRING {
        -8 * 3_600
    } else {
        -7 * 3_600
    })
}

/// A fixed UTC−8.
fn fixed(_unix: i64) -> Option<i64> {
    Some(-8 * 3_600)
}

/// THE DAY DAYLIGHT SAVING STARTS IS 23 HOURS (§10.5.4): a stretch from
/// 23:00 the day before to 01:00 the day after it charges 1 h, 23 h and
/// 1 h. NEGATIVE CONTROL: a zone with no shift charges 1 h and 24 h, and
/// nothing to the third day.
#[test]
fn the_day_daylight_saving_starts_is_23_hours() {
    let at_23 = MIDNIGHT_MS + (23 + 8) * HOUR_MS;
    let mut book = AbBook::default();
    book.set_calendar(AbCalendar::read(0, at_23, springs));
    book.observe(1, Some(Busy), None, Cruise, 0);
    book.observe(1, None, None, Cruise, 25 * HOUR);
    let busy = |book: &AbBook, day| day_at(book, 25 * HOUR, day).map(|d| d.arms[0].busy_ms);
    assert_eq!(busy(&book, DAY), Some(HOUR));
    assert_eq!(busy(&book, DAY + 1), Some(23 * HOUR));
    assert_eq!(busy(&book, DAY + 2), Some(HOUR));

    let mut flat = AbBook::default();
    flat.set_calendar(AbCalendar::read(0, at_23, fixed));
    flat.observe(1, Some(Busy), None, Cruise, 0);
    flat.observe(1, None, None, Cruise, 25 * HOUR);
    assert_eq!(busy(&flat, DAY), Some(HOUR));
    assert_eq!(busy(&flat, DAY + 1), Some(24 * HOUR));
    assert_eq!(busy(&flat, DAY + 2), None);
}

/// AN ARM CHANGE INSIDE A BOX KEEPS ONE SAMPLE (§10.5.3 B.4): the time is
/// split between the arms, the hold is one sample of its whole length for
/// the arm it began under, and a box still standing is no sample yet. A
/// cause that moves at the prompt splits the shares the same way.
/// NEGATIVE CONTROL: a box that turns into a question is two holds.
#[test]
fn an_arm_change_inside_a_box_keeps_one_sample() {
    let mut book = book_at(NOON_MS);
    book.observe(1, Some(AtBox), None, Cruise, 0);
    book.observe(1, Some(AtBox), None, Off, 1_000);
    book.observe(1, Some(Busy), None, Off, 3_000);
    book.observe(2, Some(AtBox), None, Off, 3_000);
    let week = book.week_at(60_000, &[]);
    assert_eq!(week.arm(Cruise).at_box_ms, 1_000);
    assert_eq!(week.arm(Off).at_box_ms, 2_000 + 57_000);
    assert_eq!(
        median(&week.hist(Cruise, 0)),
        (Some(centre_ms(bucket(3_000))), 1)
    );
    assert_eq!(
        median(&week.hist(Off, 0)).1,
        0,
        "the standing box: none yet"
    );

    let mut prompt = book_at(NOON_MS);
    prompt.observe(1, Some(AtPrompt), None, Cruise, 0);
    prompt.observe(1, Some(AtPrompt), Some(AbCause::Pausing), Cruise, 1_000);
    let cruise = prompt.week_at(3_000, &[]).arm(Cruise);
    assert_eq!((cruise.at_prompt_ms, cruise.pausing_ms), (3_000, 2_000));

    let mut two = book_at(NOON_MS);
    two.observe(1, Some(AtBox), None, Cruise, 0);
    two.observe(1, Some(Asked), None, Cruise, 1_000);
    two.observe(1, Some(Busy), None, Cruise, 3_000);
    let week = two.week_at(3_000, &[]);
    assert_eq!(week.hist(Cruise, 0)[bucket(1_000)], 1);
    assert_eq!(week.hist(Cruise, 1)[bucket(2_000)], 1);
}

/// A day with `busy_ms` under Cruise.
fn busy_day(day: i64, first_unix_ms: i64, busy_ms: u64) -> AbDay {
    let mut kept = AbDay::new(day, first_unix_ms);
    kept.arms[Cruise.index()].busy_ms = busy_ms;
    kept
}

/// THE WEEK IS TODAY AND THE SIX DAYS BEFORE, summed, its first stretch the
/// earliest day's; two books' days of one date add (the earlier first
/// stretch kept). NEGATIVE CONTROLS: the day before the week and a day after
/// today (a clock set back) are left out.
#[test]
fn the_week_sums_today_and_the_six_days_before() {
    let days = [
        busy_day(DAY - 7, 1, HOUR),
        busy_day(DAY - 6, 2, HOUR),
        busy_day(DAY, 3, 2 * HOUR),
        busy_day(DAY + 1, 4, 5 * HOUR),
    ];
    let week = AbWeek::of_days(days.iter(), DAY);
    assert_eq!(week.arm(Cruise).busy_ms, 3 * HOUR);
    assert_eq!(week.first, Some((DAY - 6, 2)));
    let later = AbWeek::of_days(days.iter(), DAY + 1);
    assert_eq!(later.arm(Cruise).busy_ms, 7 * HOUR);
    assert_eq!(later.first, Some((DAY, 3)));
    assert_eq!(
        AbWeek::of_days(std::iter::empty::<&AbDay>(), DAY),
        AbWeek::default()
    );

    let mut mine = busy_day(DAY, 50, HOUR);
    mine.holds[0][0][3] = 2;
    let mut theirs = busy_day(DAY, 20, 2 * HOUR);
    theirs.holds[0][0][3] = 1;
    theirs.holds[2][1][7] = 4;
    mine.plus(&theirs);
    assert_eq!(mine.first_unix_ms, 20);
    assert_eq!(mine.arms[0].busy_ms, 3 * HOUR);
    assert_eq!((mine.holds[0][0][3], mine.holds[2][1][7]), (3, 4));
}

/// THE OTHER PROCESSES' DAYS JOIN THE LIVE BOOK'S WEEK (§10.5.3 C.4): summed
/// day by day against the book's today; a book never fed shows the others'
/// alone, against the newest of their days. NEGATIVE CONTROL: a day before
/// either week is left out of both.
#[test]
fn the_others_days_join_the_live_week() {
    let others = [busy_day(DAY - 8, 1, 4 * HOUR), busy_day(DAY - 1, 2, HOUR)];
    let mut book = book_at(NOON_MS);
    book.observe(1, Some(Busy), None, Cruise, 0);
    let week = book.week_at(2 * HOUR, &others);
    assert_eq!(
        week.arm(Cruise).busy_ms,
        3 * HOUR,
        "the book's 2h and the day before"
    );
    assert_eq!(week.first.map(|(day, _)| day), Some(DAY - 1));
    let unfed = AbBook::default().week_at(2 * HOUR, &others);
    assert_eq!(unfed.arm(Cruise).busy_ms, HOUR, "the newest day's week");
    assert_eq!(unfed.first, Some((DAY - 1, 2)));
}

/// A histogram of `n` samples in bucket `at`.
fn samples(at: usize, n: u32) -> Hist {
    let mut hist = [0; BUCKETS];
    hist[at] = n;
    hist
}

const MIN: u64 = 60_000;

/// A week as the design draws it (§10.5.1).
fn drawn_week() -> AbWeek {
    let mut week = AbWeek::default();
    week.arms[Cruise.index()] = AbArmTotals {
        busy_ms: 96 * MIN,
        at_box_ms: 3 * MIN,
        asked_ms: MIN,
        at_prompt_ms: 30 * MIN,
        finished_ms: 12 * MIN,
        pausing_ms: 15 * MIN,
    };
    week.arms[Off.index()] = AbArmTotals {
        busy_ms: 86 * MIN,
        at_box_ms: 14 * MIN,
        at_prompt_ms: 20 * MIN,
        ..AbArmTotals::default()
    };
    week.arms[Land.index()] = AbArmTotals {
        busy_ms: 28 * MIN,
        at_box_ms: 2 * MIN,
        at_prompt_ms: 70 * MIN,
        finished_ms: 64 * MIN,
        ..AbArmTotals::default()
    };
    week.holds[0] = [samples(1, 41), samples(4, 5)];
    week.holds[2] = [samples(9, 9), samples(10, 2)];
    week.holds[1][0] = samples(0, 12);
    week.first = Some((DAY - 6, 1));
    week.since = Some(Stamp {
        clock: Clock::of_minute(9 * 60 + 12),
        day: DAY - 6,
    });
    week.until = Some(Stamp {
        clock: Clock::of_minute(14 * 60 + 5),
        day: DAY,
    });
    week
}

/// THE CARD, AS THE DESIGN DRAWS IT (§10.5.1, §11.6.1): Boxes and
/// Questions as medians with n on each side (`not enough yet (2)` under
/// three), At its prompt with Cruise's causes, Land on its own row, the
/// Ends and Unsupervised rows saying they have counted nothing yet, and how
/// long it measured, to when; the held shares are the lead's, so no Held
/// row (each side has 100 minutes of working time). NEGATIVE CONTROLS: a
/// landing that waits on and on moves Land's row and no Cruise-against-Off
/// row; there is never a "time saved"; and unsupervised time moves its
/// row's words, never the card's rows.
#[test]
fn the_card_reads_as_the_design_draws_it() {
    let rows = |week: &AbWeek| ab_rows(week, DAY);
    let s = |label: &str, words: &str| (label.to_string(), words.to_string());
    let week = drawn_week();
    assert_eq!(
        rows(&week),
        [
            s(
                "Boxes",
                "about 0.7 s on Cruise \u{b7} about 3m off \u{b7} median of 41 and 9 boxes seen"
            ),
            s(
                "Questions",
                "about 5.7 s on Cruise \u{b7} not enough yet (2) off \u{b7} median of 5 questions seen"
            ),
            s(
                "At its prompt",
                "Cruise 23% of 2h 10m: finished 9%, pausing 12% \u{b7} Off 17% of 2h"
            ),
            s(
                "Land",
                "held 2% \u{b7} at its prompt 70%, finished 64% \u{b7} boxes about 0.4 s (12)"
            ),
            s("Ends", "none yet"),
            s("Unsupervised", "none"),
            s("Measured", "7 days, since Mon 09:12, to 14:05"),
        ]
    );
    assert_eq!(
        ab_lead(&week).map(|lead| lead.words()).as_deref(),
        Some(
            "Working time held at a box or a question: 4% of 1h 40m on Cruise \u{b7} 14% of \
             1h 40m off"
        )
    );
    let mut landing = week;
    landing.arms[Land.index()].at_prompt_ms *= 10;
    let compared = |week: &AbWeek| {
        rows(week)
            .into_iter()
            .filter(|(label, _)| label != "Land")
            .collect::<Vec<_>>()
    };
    assert_eq!(compared(&landing), compared(&week), "Land stands alone");
    assert!(
        !rows(&week).iter().any(|(_, words)| words.contains("saved")),
        "never a time saved"
    );
    let mut unsupervised = week;
    unsupervised.arms[Unsupervised.index()] = AbArmTotals {
        busy_ms: 30 * MIN,
        at_prompt_ms: 30 * MIN,
        ..AbArmTotals::default()
    };
    assert_eq!(
        rows(&unsupervised)[5],
        s("Unsupervised", "at its prompt 50% of 1h, in no comparison")
    );
    assert_eq!(rows(&unsupervised).len(), rows(&week).len());
}

/// THE QUIET CARD AND ITS EDGES: nothing measured reads so on every row; a
/// side under three holds reads `not enough yet (n)` and no median; a week
/// begun today reads `today, since 09:12`; a zone that cannot be read
/// names the days alone; Land's pause and its few boxes read on its row.
#[test]
fn the_card_says_what_it_has_not_measured_yet() {
    let rows = |week: &AbWeek| ab_rows(week, DAY);
    let words = |week: &AbWeek| rows(week).into_iter().map(|(_, w)| w).collect::<Vec<_>>();
    assert_eq!(
        words(&AbWeek::default()),
        [
            "none seen yet",
            "none seen yet",
            "not measured yet",
            "not measured yet",
            "not measured yet",
            "none yet",
            "none",
            "nothing yet",
        ]
    );
    let mut few = AbWeek::default();
    few.holds[0][0] = samples(1, 2);
    few.arms[Off.index()].busy_ms = MIN;
    few.first = Some((DAY, 1));
    few.since = Some(Stamp {
        clock: Clock::of_minute(9 * 60 + 12),
        day: DAY,
    });
    assert_eq!(
        words(&few)[..3],
        [
            "not enough yet (2) on Cruise \u{b7} not enough yet (0) off",
            "none seen yet",
            "not measured yet on Cruise \u{b7} 0% of working time off",
        ]
    );
    assert_eq!(words(&few)[7], "today, since 09:12");
    few.since = None;
    few.first = Some((DAY - 2, 1));
    assert_eq!(words(&few)[7], "3 days");
    let mut land = AbWeek::default();
    land.arms[Land.index()] = AbArmTotals {
        busy_ms: 50 * MIN,
        at_prompt_ms: 50 * MIN,
        pausing_ms: 10 * MIN,
        ..AbArmTotals::default()
    };
    land.holds[1][0] = samples(2, 1);
    assert_eq!(
        words(&land)[4],
        "held 0% \u{b7} at its prompt 50%, finished 0%, pausing 10% \u{b7} boxes not enough yet (1)"
    );
}

/// A week whose Cruise and Off sides have `cruise_ms` and `off_ms` of
/// working time, a tenth of each held at a box.
fn working(cruise_ms: u64, off_ms: u64) -> AbWeek {
    let mut week = AbWeek::default();
    for (arm, ms) in [(Cruise, cruise_ms), (Off, off_ms)] {
        week.arms[arm.index()] = AbArmTotals {
            busy_ms: ms - ms / 10,
            at_box_ms: ms / 10,
            ..AbArmTotals::default()
        };
    }
    week
}

/// THE LEAD'S FLOOR (§11.6.1, §11.6.4 "Too little Off time"): both sides at
/// 10 minutes of working time lead with both shares, each with its working
/// time; Cruise with no Off time leads with the note; Off at 9 min 59 s,
/// Cruise at 9 min 59 s, Land alone and nothing at all give no lead.
/// NEGATIVE CONTROL: the floor's comparison flipped (or moved off by a
/// second either way) fails the edge pair.
#[test]
fn the_lead_stands_at_ten_minutes_of_each_side() {
    const TEN: u64 = 10 * MIN;
    let edge = ab_lead(&working(TEN, TEN));
    assert_eq!(
        edge,
        Some(AbLead::Both {
            cruise: LeadShare {
                share: "10%".to_string(),
                of: "of 10m on Cruise".to_string()
            },
            off: LeadShare {
                share: "10%".to_string(),
                of: "of 10m off".to_string()
            },
        })
    );
    assert_eq!(ab_lead(&working(TEN, TEN - 1_000)), None, "Off at 9:59");
    assert_eq!(ab_lead(&working(TEN - 1_000, TEN)), None, "Cruise at 9:59");
    let mut no_off = working(TEN - 1_000, TEN);
    no_off.arms[Off.index()] = AbArmTotals::default();
    assert_eq!(ab_lead(&no_off), Some(AbLead::NoOff));
    assert_eq!(
        AbLead::NoOff.words(),
        "Set a session Off to measure plain Claude Code beside Cruise"
    );
    let mut land = AbWeek::default();
    land.arms[Land.index()].busy_ms = 6 * HOUR;
    assert_eq!(ab_lead(&land), None, "Land is never part of it");
    assert_eq!(ab_lead(&AbWeek::default()), None);
    // At its prompt is no working time: an Off side that only idled has none.
    let mut idle_off = working(TEN, 0);
    idle_off.arms[Off.index()].at_prompt_ms = 5 * HOUR;
    assert_eq!(ab_lead(&idle_off), Some(AbLead::NoOff));
}

/// THE HELD ROW GIVES WAY TO THE LEAD (§11.6.3, item 2): where both
/// shares lead, no Held row; under the note, and with no lead, the Held row
/// stands. NEGATIVE CONTROL: Held drawn always would put it in the first.
#[test]
fn the_held_row_gives_way_to_the_lead_alone() {
    let labels = |week: &AbWeek| {
        ab_rows(week, DAY)
            .into_iter()
            .map(|(label, _)| label)
            .collect::<Vec<_>>()
    };
    let both = working(HOUR, HOUR);
    assert!(
        !labels(&both).contains(&"Held".to_string()),
        "{:?}",
        labels(&both)
    );
    let mut no_off = working(HOUR, HOUR);
    no_off.arms[Off.index()] = AbArmTotals::default();
    assert_eq!(ab_lead(&no_off), Some(AbLead::NoOff));
    assert!(labels(&no_off).contains(&"Held".to_string()));
    let short = working(HOUR, 9 * MIN);
    assert_eq!(ab_lead(&short), None);
    assert_eq!(
        ab_rows(&short, DAY)[2],
        (
            "Held".to_string(),
            "10% of working time on Cruise \u{b7} 10% off".to_string()
        )
    );
}

/// THE ENDS ROW (§11.6.1, §11.6.7): after Land, its parts only where they
/// counted — Cruise's dones with the floor's words, Land's landings and
/// quiet ends, the time from Land to landed with its n (`not enough yet
/// (2)` under three) — and `none yet` with nothing to count. NEGATIVE
/// CONTROL: the floor's words dropped from Cruise's count fails the first
/// form.
#[test]
fn the_ends_row_says_each_part_it_counted() {
    let ends = |done, landed, quiet, landing: Hist| AbEnds {
        done,
        landed,
        quiet,
        landing,
    };
    let none = [0; BUCKETS];
    assert_eq!(
        ends_words(&ends(6, 5, 1, samples(bucket(6 * MIN), 5))).as_deref(),
        Some(
            "Cruise said done 6 times (30 s of work or more) \u{b7} Land landed 5, quiet 1 \
             \u{b7} Land to landed: about 6m (5)"
        )
    );
    assert_eq!(
        ends_words(&ends(1, 0, 0, none)).as_deref(),
        Some("Cruise said done once (30 s of work or more)")
    );
    assert_eq!(
        ends_words(&ends(0, 2, 0, samples(bucket(6 * MIN), 2))).as_deref(),
        Some("Land landed 2 \u{b7} Land to landed: not enough yet (2)")
    );
    assert_eq!(
        ends_words(&ends(0, 1, 0, none)).as_deref(),
        Some("Land landed 1"),
        "a landing with no span counts alone"
    );
    assert_eq!(
        ends_words(&ends(0, 0, 3, none)).as_deref(),
        Some("Land quiet 3")
    );
    assert_eq!(ends_words(&AbEnds::default()), None);
    // On the card: after Land, before Unsupervised and Measured.
    let mut week = drawn_week();
    week.ends = ends(0, 5, 1, samples(bucket(6 * MIN), 5));
    let rows = ab_rows(&week, DAY);
    let at = |label: &str| rows.iter().position(|(l, _)| l == label);
    assert_eq!(at("Ends"), at("Land").map(|land| land + 1), "{rows:?}");
    assert_eq!(
        rows[at("Ends").unwrap()].1,
        "Land landed 5, quiet 1 \u{b7} Land to landed: about 6m (5)"
    );
    let quiet = ab_rows(&drawn_week(), DAY);
    assert_eq!(
        quiet
            .iter()
            .find(|(l, _)| l == "Ends")
            .map(|(_, w)| w.as_str()),
        Some("none yet")
    );
}

/// THE CARD'S ROWS STAND THROUGH THE WEEK'S FIRST LANDING AND ITS FIRST
/// UNSUPERVISED MINUTE (§11.6.7): EFFECT sits above SUPERVISION's
/// switches, so the rows a background event changes keep their places —
/// the same labels in the same order before and after, only the words
/// moved. NEGATIVE CONTROL: the Ends row drawn only once something counted
/// (the first build) adds a row at the landing.
#[test]
fn the_cards_rows_stand_through_a_landing() {
    let labels = |week: &AbWeek| {
        ab_rows(week, DAY)
            .into_iter()
            .map(|(label, _)| label)
            .collect::<Vec<_>>()
    };
    let before = drawn_week();
    let mut after = before;
    after.ends.landed = 1;
    after.ends.landing[bucket(6 * MIN)] = 1;
    after.arms[Unsupervised.index()].busy_ms = MIN;
    assert_ne!(ab_rows(&before, DAY), ab_rows(&after, DAY), "words moved");
    assert_eq!(labels(&before), labels(&after));
}

/// THE END CLOCK (§11.6.3, item 3): Measured names the instant the
/// numbers stop at, a clock time, on every form; with no zone read, none.
/// NEGATIVE CONTROL: with no `until`, the row says no `to`.
#[test]
fn measured_names_the_instant_it_counted_to() {
    let mut week = AbWeek {
        first: Some((DAY, 1)),
        since: Some(Stamp {
            clock: Clock::of_minute(9 * 60 + 12),
            day: DAY,
        }),
        until: Some(Stamp {
            clock: Clock::of_minute(14 * 60 + 5),
            day: DAY,
        }),
        ..AbWeek::default()
    };
    let measured = |week: &AbWeek| ab_rows(week, DAY).pop().map(|(_, words)| words);
    assert_eq!(
        measured(&week).as_deref(),
        Some("today, since 09:12, to 14:05")
    );
    week.since = None;
    assert_eq!(measured(&week).as_deref(), Some("today, to 14:05"));
    week.first = Some((DAY - 2, 1));
    assert_eq!(measured(&week).as_deref(), Some("3 days, to 14:05"));
    week.until = None;
    assert_eq!(measured(&week).as_deref(), Some("3 days"), "CONTROL");
}

/// What `App::note_harness_arm` tells the book: session `session`'s mode
/// (`lands`: Land), then its stretch under `arm`, at `at_ms`.
fn note(book: &mut AbBook, session: u64, (arm, lands): (AbArm, bool), at_ms: u64) {
    book.follow_mode(session, lands, at_ms);
    book.observe(session, Some(Busy), None, arm, at_ms);
}

/// THE BOOK COUNTS EACH END IN ITS DAY, AND THE TIME FROM LAND TO LANDED
/// (§11.6.3, item 4; §11.6.5 "The book"): the mode moved Cruise to Land at
/// t0 and landed at t0 + 6 min is landed 1 and one sample in `bucket(6
/// min)`, the start spent; a session whose mode is first read as Land is
/// landed 1 and no sample; a quiet end and a done are counts alone; a mode
/// that leaves Land drops the start; an end after midnight goes in its own
/// day; the session's end forgets its task. NEGATIVE CONTROLS: a book fed
/// no calendar counts nothing; a second landing in the same Land stretch
/// has no start (slice 2a's `task_opened` sets one), so it is a count with
/// no sample.
#[test]
fn the_book_counts_each_end_and_the_time_from_land_to_landed() {
    let mut book = book_at(NOON_MS);
    let t0 = HOUR;
    note(&mut book, 1, (Cruise, false), 0);
    note(&mut book, 1, (Land, true), t0);
    assert_eq!(book.land_from(1), Some(t0));
    note(&mut book, 1, (Land, true), t0 + 5 * MIN);
    assert_eq!(book.land_from(1), Some(t0), "carried while Land stays");
    book.end(1, AbEnd::Landed, t0 + 6 * MIN);
    assert_eq!(book.land_from(1), None, "spent by its landing");
    let today = |book: &AbBook| day_at(book, t0 + 6 * MIN, DAY).expect("today").ends;
    let mut want = AbEnds {
        landed: 1,
        ..AbEnds::default()
    };
    want.landing[bucket(6 * MIN)] = 1;
    assert_eq!(today(&book), want);
    // CONTROL: a second landing in the same Land stretch has no start.
    book.end(1, AbEnd::Landed, t0 + 9 * MIN);
    want.landed = 2;
    assert_eq!(today(&book), want, "a count, no sample");
    // First read in Land: a count, no span.
    note(&mut book, 2, (Land, true), t0);
    assert_eq!(book.land_from(2), None, "first read");
    book.end(2, AbEnd::Landed, t0 + 6 * MIN);
    book.end(2, AbEnd::Quiet, t0 + 6 * MIN);
    book.end(3, AbEnd::Done, t0 + 6 * MIN);
    want.landed = 3;
    want.quiet = 1;
    want.done = 1;
    assert_eq!(today(&book), want);
    // A mode that leaves Land drops the start, whatever comes back.
    note(&mut book, 4, (Off, false), 0);
    note(&mut book, 4, (Unsupervised, true), MIN);
    note(&mut book, 4, (Unsupervised, false), 2 * MIN);
    assert_eq!(book.land_from(4), None, "left Land");
    note(&mut book, 4, (Unsupervised, true), 3 * MIN);
    assert_eq!(book.land_from(4), Some(3 * MIN), "a new move into Land");
    // Past midnight: the next day's.
    let midnight = u64::try_from(12 * HOUR_MS).unwrap();
    book.end(4, AbEnd::Landed, midnight + MIN);
    let tomorrow = day_at(&book, midnight + MIN, DAY + 1)
        .expect("tomorrow")
        .ends;
    assert_eq!(
        (
            tomorrow.landed,
            tomorrow.landing[bucket(midnight - 2 * MIN)]
        ),
        (1, 1)
    );
    assert_eq!(
        day_at(&book, midnight + MIN, DAY)
            .expect("today")
            .ends
            .landed,
        3,
        "today keeps its own"
    );
    // The week sums them.
    let week = book.week_at(midnight + MIN, &[]);
    assert_eq!(
        (week.ends.landed, week.ends.quiet, week.ends.done),
        (4, 1, 1)
    );
    // The session's end forgets its task.
    assert_eq!(book.tasks(), 3, "sessions 1, 2 and 4");
    book.retire(4, midnight + 2 * MIN);
    assert_eq!((book.tasks(), book.land_from(4)), (2, None));
    // CONTROL: no calendar, no count.
    let mut unread = AbBook::default();
    note(&mut unread, 1, (Cruise, false), 0);
    note(&mut unread, 1, (Land, true), MIN);
    unread.end(1, AbEnd::Landed, 7 * MIN);
    assert_eq!(unread.week_at(7 * MIN, &[]).ends, AbEnds::default());
}

/// A WALL'S WORD KEEPS THE LANDING'S START (§11.6.3, item 4; §11.6.5 "The
/// book"): a word that counts in no phase drops the session's open stretch
/// (`observe(…, None, …)`), but the start lives apart from it, so the
/// landing six minutes after the move into Land is still one sample in
/// `bucket(6 min)`. NEGATIVE CONTROL: the start kept in the stretch's clock
/// would be dropped by the wall (no sample) or begun again at the next word
/// (`bucket(4 min)`, another bucket).
#[test]
fn a_walls_word_between_land_and_landed_keeps_the_span() {
    assert_ne!(bucket(6 * MIN), bucket(4 * MIN), "the control can tell");
    let mut book = book_at(NOON_MS);
    note(&mut book, 1, (Cruise, false), 0);
    note(&mut book, 1, (Land, true), MIN);
    book.follow_mode(1, true, 2 * MIN);
    book.observe(1, None, None, Land, 2 * MIN);
    assert_eq!(book.stretch(1), None, "the wall dropped the stretch");
    note(&mut book, 1, (Land, true), 3 * MIN);
    book.end(1, AbEnd::Landed, 7 * MIN);
    let ends = book.week_at(7 * MIN, &[]).ends;
    assert_eq!(ends.landed, 1);
    assert_eq!(ends.landing[bucket(6 * MIN)], 1, "{:?}", ends.landing);
    assert_eq!(ends.landing.iter().sum::<u32>(), 1);
}

/// TWO DAYS' ENDS ADD (two processes' books of one date, the week's sum).
/// NEGATIVE CONTROL: a day before the week adds nothing.
#[test]
fn two_days_ends_add() {
    let mut mine = busy_day(DAY, 50, HOUR);
    mine.ends.landed = 2;
    mine.ends.landing[3] = 1;
    let mut theirs = busy_day(DAY, 20, HOUR);
    theirs.ends.landed = 1;
    theirs.ends.quiet = 4;
    theirs.ends.done = 7;
    theirs.ends.landing[3] = 2;
    let mut old = busy_day(DAY - 7, 1, HOUR);
    old.ends.landed = 100;
    let week = AbWeek::of_days([mine, theirs, old].iter(), DAY);
    assert_eq!(
        (
            week.ends.landed,
            week.ends.quiet,
            week.ends.done,
            week.ends.landing[3]
        ),
        (3, 4, 7, 3)
    );
    mine.plus(&theirs);
    assert_eq!(mine.ends, week.ends);
}

/// THE START FOLLOWS THE MODE, NEVER THE CLAIM (§11.6.4 "Before this
/// process"; §11.6.7): a session restored mid-landing is first read with
/// its word Land and no claim of this host's (unsupervised), then claimed
/// (Land): its landing counts with NO span; and a claim that lapses and is
/// taken again mid-landing keeps the start the mode's move set, so the
/// landing six minutes after the press is `bucket(6 min)`. The App's
/// wiring of the two is `harness_task_tests`'
/// `the_time_from_land_to_landed_starts_where_the_mode_moved`. NEGATIVE
/// CONTROL (run here): the mode told as the arm is (the first build's
/// rule) puts a sample in the restored landing and starts the blip's at the
/// retake (`bucket(1m 50s)`).
#[test]
fn the_start_follows_the_mode_never_the_claim() {
    assert_ne!(
        bucket(6 * MIN),
        bucket(2 * MIN - 10_000),
        "the control can tell"
    );
    let run = |keyed_on_arm: bool| {
        let mut book = book_at(NOON_MS);
        let mut note = |session, (arm, lands): (AbArm, bool), at_ms| {
            let lands = if keyed_on_arm { arm == Land } else { lands };
            note(&mut book, session, (arm, lands), at_ms);
        };
        // Restored: the word Land when first read, the claim 20 s later.
        note(7, (Unsupervised, true), 0);
        note(7, (Land, true), 20_000);
        // A blip: the press at 1m, the claim lapsed at 5m and taken again
        // at 5m 10s, landed at 7m.
        note(8, (Cruise, false), 0);
        note(8, (Land, true), MIN);
        note(8, (Unsupervised, true), 5 * MIN);
        note(8, (Land, true), 5 * MIN + 10_000);
        book.end(7, AbEnd::Landed, 60_000);
        book.end(8, AbEnd::Landed, 7 * MIN);
        book.week_at(7 * MIN, &[]).ends
    };
    let ends = run(false);
    assert_eq!(ends.landed, 2);
    assert_eq!(ends.landing[bucket(6 * MIN)], 1, "{:?}", ends.landing);
    assert_eq!(
        ends.landing.iter().sum::<u32>(),
        1,
        "the restored landing has none: {:?}",
        ends.landing
    );
    let control = run(true);
    assert_eq!(control.landing[bucket(6 * MIN)], 0, "CONTROL");
    assert_eq!(control.landing.iter().sum::<u32>(), 2, "CONTROL");
}

/// A QUIET LANDING THAT LANDS AFTER ALL IS ONE END (its face spent →
/// landed): the quiet count this book took is moved to landed, in the day
/// each fell on, and the span is the landing's; a quiet end with no landing
/// after it stays quiet. NEGATIVE CONTROL: the landing counted with
/// `end` alone (the first build) leaves the quiet end standing beside it.
#[test]
fn a_quiet_landing_that_lands_after_all_is_one_end() {
    let mut book = book_at(NOON_MS);
    let midnight = u64::try_from(12 * HOUR_MS).unwrap();
    note(&mut book, 1, (Cruise, false), 0);
    note(&mut book, 1, (Land, true), midnight - 10 * MIN);
    book.end(1, AbEnd::Quiet, midnight - MIN);
    note(&mut book, 2, (Land, true), 0);
    book.end(2, AbEnd::Quiet, midnight - MIN);
    let quiet = book.week_at(midnight - MIN, &[]).ends;
    assert_eq!((quiet.landed, quiet.quiet), (0, 2));
    book.quiet_landed(1, midnight + MIN);
    let today = day_at(&book, midnight + MIN, DAY).expect("today").ends;
    let tomorrow = day_at(&book, midnight + MIN, DAY + 1)
        .expect("tomorrow")
        .ends;
    assert_eq!((today.landed, today.quiet), (0, 1), "session 2 stays quiet");
    assert_eq!((tomorrow.landed, tomorrow.quiet), (1, 0));
    assert_eq!(tomorrow.landing[bucket(11 * MIN)], 1);
    // Told once more, or with no quiet this book counted: a landing alone.
    book.quiet_landed(1, midnight + 2 * MIN);
    note(&mut book, 3, (Land, true), 0);
    book.quiet_landed(3, midnight + 2 * MIN);
    let week = book.week_at(midnight + 2 * MIN, &[]).ends;
    assert_eq!((week.landed, week.quiet), (3, 1));
}
