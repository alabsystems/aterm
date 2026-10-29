// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::claude_lights::Clock;
use AbArm::{Cruise, Land, Off, Unsupervised};
use AbPhase::{Asked, AtBox, AtPrompt, Busy};

const DAY: i64 = 20_002;
const MIDNIGHT_MS: i64 = DAY * 86_400_000;
const HOUR: u64 = 3_600_000;
const HOUR_MS: i64 = 3_600_000;

fn utc(_unix: i64) -> Option<i64> {
    Some(0)
}

fn book_at(unix_ms: i64) -> AbBook {
    let mut book = AbBook::default();
    book.set_calendar(AbCalendar::read(0, unix_ms, utc));
    book
}

const NOON_MS: i64 = MIDNIGHT_MS + 12 * HOUR_MS;

fn day_at(book: &AbBook, now: u64, day: i64) -> Option<AbDay> {
    let (ring, _) = book.ring_at(now)?;
    ring.iter().flatten().find(|kept| kept.day == day).copied()
}

#[test]
fn the_book_charges_each_stretch_to_the_arm_phase_and_cause_in_force() {
    let mut book = book_at(NOON_MS);
    book.observe(1, Some(Busy), None, Cruise, 0);
    book.observe(1, Some(AtBox), None, Cruise, 1_000);
    book.observe(1, Some(Busy), None, Cruise, 1_500);
    book.observe(1, Some(AtPrompt), Some(AbCause::Pausing), Cruise, 2_000);
    book.observe(1, Some(AtPrompt), Some(AbCause::Pausing), Off, 5_000);
    assert_eq!(book.stretch(1), Some((AtPrompt, Off, None)));
    book.observe(2, Some(Asked), None, Land, 0);
    book.observe(2, None, None, Land, 700);
    let week = book.week_at(6_000, &[]);
    assert_eq!(
        week.arm(Cruise),
        AbArmTotals {
            busy_ms: 1_500,
            at_box_ms: 500,
            at_prompt_ms: 3_000,
            pausing_ms: 3_000,
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
    book.observe(2, None, None, Land, 90_000);
    assert_eq!(book.week_at(6_000, &[]).arm(Land).measured_ms(), 700);
    book.retire(1, 8_000);
    assert_eq!(book.week_at(1_000_000, &[]).arm(Off).at_prompt_ms, 3_000);
    assert_eq!(book.open_stretches(), 0);
    book.observe(3, Some(AtPrompt), None, Unsupervised, 10_000);
    book.observe(3, Some(Busy), None, Unsupervised, 9_000);
    book.retire(4, 50_000);
    book.retire(3, 10_000);
    assert_eq!(
        book.week_at(1_000_000, &[]).arm(Unsupervised),
        AbArmTotals::default()
    );
    let mut unread = AbBook::default();
    unread.observe(1, Some(Busy), None, Cruise, 0);
    unread.observe(1, None, None, Cruise, 5_000);
    assert_eq!(unread.week_at(5_000, &[]), AbWeek::default());
}

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

#[test]
fn a_cause_is_kept_only_under_cruise_and_land_at_the_prompt() {
    for arm in [Cruise, Land] {
        let at = Some(AtPrompt);
        let landed = (arm == Land).then_some(AbCause::Landed);
        assert_eq!(
            AbCause::of(arm, at, true, true),
            landed.or(Some(AbCause::Pausing)),
            "{arm:?}"
        );
        assert_eq!(AbCause::of(arm, at, true, false), landed, "{arm:?}");
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
    book.observe(1, Some(AtPrompt), Some(AbCause::Landed), Off, 0);
    book.observe(2, Some(AtPrompt), Some(AbCause::Landed), Land, 0);
    book.observe(3, Some(Busy), Some(AbCause::Pausing), Cruise, 0);
    let week = book.week_at(1_000, &[]);
    assert_eq!(week.arm(Off).landed_ms, 0);
    assert_eq!(week.arm(Land).landed_ms, 1_000, "CONTROL: Land's");
    assert_eq!(week.arm(Cruise).pausing_ms, 0, "busy is never pausing");
}

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

#[test]
fn every_sessions_stretch_across_midnight_is_split_there() {
    let at_23 = MIDNIGHT_MS + 23 * HOUR_MS;
    let busy = |book: &AbBook, now: u64, day| {
        day_at(book, now, day).map(|kept| kept.arms[Cruise.index()].busy_ms)
    };
    let mut book = book_at(at_23);
    book.observe(1, Some(Busy), None, Cruise, 0);
    book.observe(2, Some(Busy), None, Cruise, 0);
    book.observe(1, None, None, Cruise, 2 * HOUR);
    book.observe(2, None, None, Cruise, 3 * HOUR);
    assert_eq!(busy(&book, 3 * HOUR, DAY), Some(2 * HOUR), "an hour each");
    assert_eq!(busy(&book, 3 * HOUR, DAY + 1), Some(3 * HOUR));
    let mut open = book_at(at_23);
    open.observe(1, Some(Busy), None, Cruise, 0);
    open.observe(2, Some(Busy), None, Cruise, 0);
    assert_eq!(busy(&open, 3 * HOUR, DAY), Some(2 * HOUR));
    assert_eq!(busy(&open, 3 * HOUR, DAY + 1), Some(4 * HOUR));
    open.set_calendar(AbCalendar::read(3 * HOUR, at_23 + 3 * HOUR_MS, utc));
    assert_eq!(busy(&open, 3 * HOUR, DAY), Some(2 * HOUR));
    assert_eq!(busy(&open, 3 * HOUR, DAY + 1), Some(4 * HOUR));
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

    let mut alone = book_at(at_23);
    alone.observe(2, Some(Busy), None, Cruise, 0);
    alone.observe(2, None, None, Cruise, 3 * HOUR);
    assert_eq!(busy(&alone, 3 * HOUR, DAY), Some(HOUR));
    assert_eq!(busy(&alone, 3 * HOUR, DAY + 1), Some(2 * HOUR));
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

#[test]
fn a_moved_wall_moves_the_midnight() {
    let at_22 = MIDNIGHT_MS + 22 * HOUR_MS;
    let mut slept = book_at(at_22);
    slept.observe(1, Some(Busy), None, Cruise, 0);
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

    let mut back = book_at(NOON_MS);
    back.observe(1, Some(Busy), None, Cruise, 0);
    back.set_calendar(AbCalendar::read(HOUR, NOON_MS, utc));
    assert_eq!(back.week_at(2 * HOUR, &[]).arm(Cruise).busy_ms, 2 * HOUR);
}

const SPRING: i64 = (DAY + 1) * 86_400 + 10 * 3_600;

fn springs(unix: i64) -> Option<i64> {
    Some(if unix < SPRING {
        -8 * 3_600
    } else {
        -7 * 3_600
    })
}

fn fixed(_unix: i64) -> Option<i64> {
    Some(-8 * 3_600)
}

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

fn busy_day(day: i64, first_unix_ms: i64, busy_ms: u64) -> AbDay {
    let mut kept = AbDay::new(day, first_unix_ms);
    kept.arms[Cruise.index()].busy_ms = busy_ms;
    kept
}

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

fn samples(at: usize, n: u32) -> Hist {
    let mut hist = [0; BUCKETS];
    hist[at] = n;
    hist
}

const MIN: u64 = 60_000;

fn drawn_week() -> AbWeek {
    let mut week = AbWeek::default();
    week.arms[Cruise.index()] = AbArmTotals {
        busy_ms: 96 * MIN,
        at_box_ms: 3 * MIN,
        asked_ms: MIN,
        at_prompt_ms: 30 * MIN,
        pausing_ms: 15 * MIN,
        ..AbArmTotals::default()
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
        landed_ms: 64 * MIN,
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
                "Cruise 23% of 2h 10m: pausing 12% \u{b7} Off 17% of 2h"
            ),
            s(
                "Land",
                "held 2% \u{b7} at its prompt 70%, landed 64% \u{b7} boxes about 0.4 s (12)"
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
        "held 0% \u{b7} at its prompt 50%, landed 0%, pausing 10% \u{b7} boxes not enough yet (1)"
    );
}

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
    let mut land = AbWeek::default();
    land.arms[Land.index()].busy_ms = 6 * HOUR;
    assert_eq!(ab_lead(&land), None, "Land is never part of it");
    assert_eq!(ab_lead(&AbWeek::default()), None);
    let mut idle_off = working(TEN, 0);
    idle_off.arms[Off.index()].at_prompt_ms = 5 * HOUR;
    assert_eq!(ab_lead(&idle_off), Some(AbLead::NoOff));
}

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

#[test]
fn the_ends_row_says_each_part_it_counted() {
    let ends = |landed, landing: Hist| AbEnds { landed, landing };
    let none = [0; BUCKETS];
    assert_eq!(
        ends_words(&ends(5, samples(bucket(6 * MIN), 5))).as_deref(),
        Some("Land landed 5 \u{b7} Land to landed: about 6m (5)")
    );
    assert_eq!(
        ends_words(&ends(2, samples(bucket(6 * MIN), 2))).as_deref(),
        Some("Land landed 2 \u{b7} Land to landed: not enough yet (2)")
    );
    assert_eq!(
        ends_words(&ends(1, none)).as_deref(),
        Some("Land landed 1"),
        "a landing with no span counts alone"
    );
    assert_eq!(ends_words(&AbEnds::default()), None);
    let mut week = drawn_week();
    week.ends = ends(5, samples(bucket(6 * MIN), 5));
    let rows = ab_rows(&week, DAY);
    let at = |label: &str| rows.iter().position(|(l, _)| l == label);
    assert_eq!(at("Ends"), at("Land").map(|land| land + 1), "{rows:?}");
    assert_eq!(
        rows[at("Ends").unwrap()].1,
        "Land landed 5 \u{b7} Land to landed: about 6m (5)"
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

fn note(book: &mut AbBook, session: u64, (arm, lands): (AbArm, bool), at_ms: u64) {
    book.follow_mode(session, lands, at_ms);
    book.observe(session, Some(Busy), None, arm, at_ms);
}

#[test]
fn the_book_counts_each_end_and_the_time_from_land_to_landed() {
    let mut book = book_at(NOON_MS);
    let t0 = HOUR;
    note(&mut book, 1, (Cruise, false), 0);
    note(&mut book, 1, (Land, true), t0);
    assert_eq!(book.land_from(1), Some(t0));
    note(&mut book, 1, (Land, true), t0 + 5 * MIN);
    assert_eq!(book.land_from(1), Some(t0), "carried while Land stays");
    book.landed(1, t0 + 6 * MIN);
    assert_eq!(book.land_from(1), None, "spent by its landing");
    let today = |book: &AbBook| day_at(book, t0 + 6 * MIN, DAY).expect("today").ends;
    let mut want = AbEnds {
        landed: 1,
        ..AbEnds::default()
    };
    want.landing[bucket(6 * MIN)] = 1;
    assert_eq!(today(&book), want);
    book.landed(1, t0 + 9 * MIN);
    want.landed = 2;
    assert_eq!(today(&book), want, "a count, no sample");
    note(&mut book, 2, (Land, true), t0);
    assert_eq!(book.land_from(2), None, "first read");
    book.landed(2, t0 + 6 * MIN);
    want.landed = 3;
    assert_eq!(today(&book), want);
    note(&mut book, 4, (Off, false), 0);
    note(&mut book, 4, (Unsupervised, true), MIN);
    note(&mut book, 4, (Unsupervised, false), 2 * MIN);
    assert_eq!(book.land_from(4), None, "left Land");
    note(&mut book, 4, (Unsupervised, true), 3 * MIN);
    assert_eq!(book.land_from(4), Some(3 * MIN), "a new move into Land");
    let midnight = u64::try_from(12 * HOUR_MS).unwrap();
    book.landed(4, midnight + MIN);
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
    let week = book.week_at(midnight + MIN, &[]);
    assert_eq!(week.ends.landed, 4);
    assert_eq!(book.tasks(), 3, "sessions 1, 2 and 4");
    book.retire(4, midnight + 2 * MIN);
    assert_eq!((book.tasks(), book.land_from(4)), (2, None));
    let mut unread = AbBook::default();
    note(&mut unread, 1, (Cruise, false), 0);
    note(&mut unread, 1, (Land, true), MIN);
    unread.landed(1, 7 * MIN);
    assert_eq!(unread.week_at(7 * MIN, &[]).ends, AbEnds::default());
}

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
    book.landed(1, 7 * MIN);
    let ends = book.week_at(7 * MIN, &[]).ends;
    assert_eq!(ends.landed, 1);
    assert_eq!(ends.landing[bucket(6 * MIN)], 1, "{:?}", ends.landing);
    assert_eq!(ends.landing.iter().sum::<u32>(), 1);
}

#[test]
fn two_days_ends_add() {
    let mut mine = busy_day(DAY, 50, HOUR);
    mine.ends.landed = 2;
    mine.ends.landing[3] = 1;
    let mut theirs = busy_day(DAY, 20, HOUR);
    theirs.ends.landed = 1;
    theirs.ends.landing[3] = 2;
    let mut old = busy_day(DAY - 7, 1, HOUR);
    old.ends.landed = 100;
    let week = AbWeek::of_days([mine, theirs, old].iter(), DAY);
    assert_eq!((week.ends.landed, week.ends.landing[3]), (3, 3));
    mine.plus(&theirs);
    assert_eq!(mine.ends, week.ends);
}

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
        note(7, (Unsupervised, true), 0);
        note(7, (Land, true), 20_000);
        note(8, (Cruise, false), 0);
        note(8, (Land, true), MIN);
        note(8, (Unsupervised, true), 5 * MIN);
        note(8, (Land, true), 5 * MIN + 10_000);
        book.landed(7, 60_000);
        book.landed(8, 7 * MIN);
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
