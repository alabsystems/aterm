// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn relative_reset_keeps_the_full_unsigned_duration() {
    let large = 1u64 << 63;
    for (now, seconds, expected) in [
        (i64::MIN, large, 0),
        (-1, large, i64::MAX),
        (i64::MIN, u64::MAX, i64::MAX),
        (10, u64::MAX, i64::MAX),
        (i64::MAX, 0, i64::MAX),
        (10, 60, 70),
    ] {
        let spec = parse_reset(&format!("in {seconds}s")).expect("valid unsigned seconds");
        assert_eq!(
            reset_in_zone(&spec, now, SESSION_AHEAD_MAX, &|_| None),
            Some(expected)
        );
    }
}

/// The test's zone table: what `date` answers on a machine that knows
/// these two zones, in mid-September (daylight time).
fn zones(zone: &str) -> Option<i64> {
    match zone {
        "America/Los_Angeles" => Some(-7 * 3600),
        "America/New_York" => Some(-4 * 3600),
        "UTC" => Some(0),
        _ => None,
    }
}
/// 2026-09-17T08:55:00-07:00.
const NOW: i64 = 1_789_660_500;
const PDT: i64 = -7 * 3600;

fn at(text: &str) -> i64 {
    reset_at(&parse_reset(text).expect(text), NOW, PDT, zones)
}

/// Los Angeles over 2026-11-01's change and 2027-03-14's.
fn la_either_change(unix: i64) -> Option<i64> {
    Some(if (1_793_523_600..1_805_018_400).contains(&unix) {
        -8 * 3600
    } else {
        -7 * 3600
    })
}

/// What the wall read at `unix`, seconds into its day.
fn clock_at(unix: i64) -> i64 {
    (unix + la_either_change(unix).unwrap()).rem_euclid(86_400)
}

/// Every clock of the day, read every 23 minutes with seconds on the clock for two days either
/// side of a change, against walking the minutes of the reach: the newest minute that showed
/// it, or none where no minute did. Los Angeles' two changes, Nuuk's through midnight and a
/// half-hour one.
/// NEGATIVE CONTROL: dropping the reach's bound, asking two days not three, or answering
/// `now` for a clock no minute showed fails.
#[test]
fn every_clock_of_the_day_is_its_newest_minute_in_the_reach() {
    fn nuuk(unix: i64) -> Option<i64> {
        Some(if unix < 1_806_195_600 { -7200 } else { -3600 })
    }
    const HALF: i64 = 1_790_000_100;
    fn half_hour(unix: i64) -> Option<i64> {
        Some(if unix < HALF { 37_800 } else { 39_600 })
    }
    let walk = |change: i64, offset: &dyn Fn(i64) -> Option<i64>| {
        let mut now = change - 52 * 3600 + 37;
        while now <= change + 52 * 3600 {
            let earliest = now - 86_400 + 60;
            let mut want = vec![None; 1440];
            let mut minute = now.div_euclid(60) * 60;
            while minute > earliest {
                let clock = (minute + offset(minute).unwrap()).rem_euclid(86_400) / 60;
                want[usize::try_from(clock).unwrap()].get_or_insert(minute);
                minute -= 60;
            }
            for (clock, want) in (0_i64..).zip(want) {
                assert_eq!(
                    newest_instant_of_clock(clock * 60, earliest, now, offset),
                    want,
                    "{clock} minutes into the day, read at {now}"
                );
            }
            now += 23 * 60;
        }
    };
    walk(1_793_523_600, &la_either_change);
    walk(1_805_018_400, &la_either_change);
    walk(1_806_195_600, &nuuk);
    walk(HALF, &half_hour);
}

/// Every minute for a day and a half either side of each change, against walking the minutes
/// themselves: a done row's clock, read from zero to 23 hours after it was printed, is the
/// newest minute in the reach that showed it.
/// NEGATIVE CONTROL: `reset_in_zone`'s first instant past the reach, or a constant offset,
/// fails around the change.
#[test]
fn a_done_rows_clock_is_the_newest_minute_that_showed_it() {
    for change in [1_793_523_600, 1_805_018_400_i64] {
        let mut printed = change - 36 * 3600;
        while printed <= change + 36 * 3600 {
            for after in [
                0,
                60,
                1800,
                3540,
                3660,
                7200,
                5 * 3600,
                12 * 3600,
                23 * 3600,
            ] {
                let now = printed + after;
                let earliest = now - 86_400 + 60;
                let clock = clock_at(printed);
                let want = (0..=1440)
                    .map(|m| now - now.rem_euclid(60) - m * 60)
                    .find(|&at| at > earliest && clock_at(at) == clock);
                assert_eq!(
                    newest_instant_of_clock(clock, earliest, now, &la_either_change),
                    want,
                    "printed {printed}, read {after} s after"
                );
            }
            printed += 60;
        }
    }
}

/// Every 10 minutes for two days either side of each change, for clocks every 15 minutes,
/// against walking the minutes: a clock within a session's reach is the first coming minute that
/// shows it. A clock the change repeats, read from an hour before the change to an hour after
/// it, is left out, and a clock it skips has no minute to compare (ATERM-KI-1101-1).
/// NEGATIVE CONTROL: placing it by the offset as of now fails around either change.
#[test]
fn a_zoneless_clock_within_reach_is_the_first_minute_that_shows_it() {
    let no_zone = |_: &str, _: i64| None;
    for change in [1_793_523_600, 1_805_018_400_i64] {
        let mut now = change - 48 * 3600;
        while now <= change + 48 * 3600 {
            for clock in (0..86_400).step_by(900) {
                let spec = ResetSpec::At {
                    date: None,
                    hour: u32::try_from(clock / 3600).unwrap(),
                    minute: u32::try_from(clock % 3600 / 60).unwrap(),
                    zone: None,
                };
                let want = (1..5 * 60)
                    .map(|m| now + m * 60)
                    .find(|&at| clock_at(at) == clock);
                let repeated = change == 1_793_523_600
                    && clock / 3600 == 1
                    && (change - 3600..change + 3600).contains(&now);
                if let Some(want) = want.filter(|_| !repeated) {
                    assert_eq!(
                        reset_at_local(&spec, now, &la_either_change, &no_zone),
                        want,
                        "{clock} s into the day, read at {now}"
                    );
                }
            }
            now += 600;
        }
    }
}

#[test]
fn later_day_comparison_preserves_wall_clock_near_timestamp_limits() {
    let spec = ResetSpec::At {
        date: None,
        hour: 0,
        minute: 0,
        zone: None,
    };
    for (at, named, offset, expected) in [
        (i64::MAX, i64::MAX - 86_400, 3600, true),
        (i64::MIN + 86_400, i64::MIN, -3600, true),
        (i64::MAX, i64::MAX - 90_000, 3600, false),
        (i64::MIN + 90_000, i64::MIN, -3600, false),
        // The complete signed span is u64::MAX seconds, with remainder 25,215 in a day.
        (i64::MAX, i64::MIN + 25_215, 0, true),
        (i64::MAX, i64::MIN + 25_156, 0, true),
        (i64::MAX, i64::MIN + 25_155, 0, false),
        (i64::MIN, i64::MAX, 0, false),
    ] {
        assert_eq!(
            is_a_later_day_of(&spec, at, named, &|_| Some(offset), &|_, _| None),
            expected,
            "at={at}, named={named}, offset={offset}"
        );
    }
}

/// A clock named before either change, read one or two days on, is that clock's later day
/// however many hours passed, and a flat 24 hours across a change is another clock.
/// NEGATIVE CONTROL: comparing the instants, not the clocks, fails.
#[test]
fn a_later_day_of_a_clock_is_told_by_the_wall() {
    let no_zone = |_: &str, _: i64| None;
    let spec = ResetSpec::At {
        date: None,
        hour: 0,
        minute: 0,
        zone: None,
    };
    let later = |at, named| is_a_later_day_of(&spec, at, named, &la_either_change, &no_zone);
    // 2026-10-31 23:00 PDT, and the wall's next 23:00: 2026-11-01 23:00 PST, 25 hours on.
    let named = 1_793_512_800;
    assert!(later(named + 25 * 3600, named));
    assert!(later(named + 25 * 3600 + 59, named), "within a minute");
    assert!(!later(named + 25 * 3600 + 60, named), "past the minute");
    assert!(later(named + 49 * 3600, named), "two days on");
    assert!(!later(named + 24 * 3600, named), "22:00, another clock");
    // 2027-03-13 12:00 PST, and 2027-03-14 12:00 PDT, 23 hours on.
    let named = 1_805_054_400 - 86_400;
    assert!(later(named + 23 * 3600, named));
    assert!(!later(named + 24 * 3600, named), "13:00, another clock");
    // A day with no change is 24 hours, and the same instant is no later day.
    let named = 1_781_000_000;
    assert!(later(named + 86_400, named));
    assert!(!later(named + 86_400 - 3600, named), "an hour off");
    assert!(!later(named, named), "the same instant");
    assert!(!later(named - 86_400, named), "the day before");
}

/// Fold a clock back across either change and it is the same clock a day before: 23 or 25 hours,
/// and a flat 24 only where no change lies between. A clock the day skips stays a flat 24.
/// NEGATIVE CONTROL: folding a flat 86,400 s fails.
#[test]
fn a_clock_folded_across_a_change_keeps_its_clock() {
    let no_zone = |_: &str, _: i64| None;
    let spec = ResetSpec::At {
        date: None,
        hour: 0,
        minute: 0,
        zone: None,
    };
    let fold = |at, now| within_a_session_window_local(&spec, at, now, &la_either_change, &no_zone);
    // 2026-11-02 00:30 PST folds to 2026-11-01 00:30 PDT, 25 hours before.
    let at = 1_793_523_600 + 23 * 3600 + 30 * 60;
    assert_eq!(fold(at, at - 20 * 3600), at - 25 * 3600);
    // 2027-03-15 01:30 PDT folds to 2027-03-14 01:30 PST, 23 hours before...
    let on = 1_805_018_400;
    let at = on + 22 * 3600 + 30 * 60;
    assert_eq!(fold(at, at - 20 * 3600), at - 23 * 3600, "01:30 PST");
    // ...and 2027-03-15 02:30 PDT has no 02:30 the day before.
    let at = at + 3600;
    assert_eq!(fold(at, at - 20 * 3600), at - 86_400, "skipped");
    // No change between, and a clock within the window stays.
    let at = 1_781_000_000;
    assert_eq!(fold(at, at - 20 * 3600), at - 86_400);
    assert_eq!(fold(at, at - 3600), at);
}

/// A RESET READS AS ITS NOTICE SAID IT, less the zone: the date and the
/// clock where the notice names both, the clock alone where it names no
/// date, midnight and noon on the twelve.
#[test]
fn a_reset_clock_reads_as_its_notice_said_it() {
    let words = |text: &str| ResetClock::of(Some(text)).map(|c| c.words());
    for (text, said) in [
        ("Oct 23 at 9am (America/Los_Angeles)", "on Oct 23 at 9am"),
        ("Sep 30 at 12:30pm (UTC)", "on Sep 30 at 12:30pm"),
        ("Jan 2, 2027 at 12am (UTC)", "on Jan 2 at 12am"),
        ("10:38am (America/Los_Angeles)", "at 10:38am"),
        ("23:05", "at 11:05pm"),
    ] {
        assert_eq!(words(text).as_deref(), Some(said), "{text}");
    }
    assert_eq!(words("in 3h"), None);
}

/// A reset in the next calendar year names the year (2.1.283's formatter
/// adds it when the year differs): read as the date and clock, the year
/// left to `reset_at`, which places the date in the year nearest now.
#[test]
fn a_reset_that_names_its_year_parses() {
    for text in [
        "Jan 2, 2027 at 9am (America/Los_Angeles)",
        "Jan 2, 2027, 9am (UTC)",
    ] {
        assert!(
            matches!(
                parse_reset(text),
                Some(ResetSpec::At {
                    date: Some((1, 2)),
                    hour: 9,
                    minute: 0,
                    ..
                })
            ),
            "{text}: {:?}",
            parse_reset(text)
        );
    }
    let dec30 = 1_798_632_000;
    let spec = parse_reset("Jan 2, 2027 at 9am (UTC)").expect("parses");
    assert_eq!(reset_at(&spec, dec30, 0, zones), 1_798_880_400);
}

/// Both spellings the real notices carry, with the zone read.
#[test]
fn both_spellings_parse_with_the_zone() {
    assert_eq!(
        parse_reset("Sep 19 at 11am (America/Los_Angeles)"),
        Some(ResetSpec::At {
            date: Some((9, 19)),
            hour: 11,
            minute: 0,
            zone: Some("America/Los_Angeles".to_string()),
        })
    );
    assert_eq!(
        parse_reset("7:30pm (America/Los_Angeles)"),
        Some(ResetSpec::At {
            date: None,
            hour: 19,
            minute: 30,
            zone: Some("America/Los_Angeles".to_string()),
        })
    );
    assert_eq!(
        parse_reset("in 3h"),
        Some(ResetSpec::In(Duration::from_secs(3 * 3600)))
    );
    assert_eq!(
        parse_reset("in 2h 30m"),
        Some(ResetSpec::In(Duration::from_secs(9000)))
    );
    assert_eq!(
        parse_reset("in 45 minutes"),
        Some(ResetSpec::In(Duration::from_secs(2700)))
    );
    assert_eq!(
        parse_reset("3pm (America/New_York)."),
        Some(ResetSpec::At {
            date: None,
            hour: 15,
            minute: 0,
            zone: Some("America/New_York".to_string()),
        })
    );
    assert_eq!(
        parse_reset("September 19, 11:05 am"),
        Some(ResetSpec::At {
            date: Some((9, 19)),
            hour: 11,
            minute: 5,
            zone: None,
        })
    );
    assert_eq!(
        parse_reset("2026-10-29 09:31 PDT"),
        Some(ResetSpec::At {
            date: Some((10, 29)),
            hour: 9,
            minute: 31,
            zone: Some("America/Los_Angeles".to_string()),
        })
    );
    assert_eq!(
        parse_reset("2026-10-29 16:31 UTC"),
        Some(ResetSpec::At {
            date: Some((10, 29)),
            hour: 16,
            minute: 31,
            zone: Some("UTC".to_string()),
        })
    );
    assert_eq!(
        parse_reset("2026-10-29 11:31 CEST"),
        Some(ResetSpec::At {
            date: Some((10, 29)),
            hour: 11,
            minute: 31,
            zone: None,
        })
    );
    for bad in [
        "",
        "soon",
        "in",
        "in h",
        "in -3h",
        "in +3h",
        "15",
        "+7pm",
        "7:5pm",
        "7:+30pm",
        "25pm",
        "13pm",
        "Sep 40 at 1am",
        "Sep +19 at 11am",
        "Sep 19h at 11am",
        "Sep 19stth at 11am",
        "Maybe 19 at 11am",
        "2026-13-29 09:31 PDT",
        "2026-+1-29 09:31 PDT",
        "2026-1-2 09:31 PDT",
        "7:60pm",
    ] {
        assert_eq!(parse_reset(bad), None, "{bad:?}");
    }
}

/// The auto-continue notice: its time (handed over as `1:50pm`, or the phrase) is a bare
/// clock time, and `continuing shortly` (or `shortly`) a minute from now.
#[test]
fn the_auto_continue_shapes_are_a_time_today_or_tomorrow_or_a_minute() {
    let one_fifty = ResetSpec::At {
        date: None,
        hour: 13,
        minute: 50,
        zone: None,
    };
    assert_eq!(parse_reset("1:50pm"), Some(one_fifty.clone()));
    assert_eq!(
        parse_reset("continuing automatically at 1:50pm"),
        Some(one_fifty.clone())
    );
    assert_eq!(
        parse_reset("Continuing automatically at 11:05 am"),
        Some(ResetSpec::At {
            date: None,
            hour: 11,
            minute: 5,
            zone: None,
        })
    );
    assert_eq!(
        parse_reset("continuing automatically at 12am"),
        Some(ResetSpec::At {
            date: None,
            hour: 0,
            minute: 0,
            zone: None,
        })
    );
    assert_eq!(parse_reset("shortly"), Some(ResetSpec::In(SHORTLY)));
    assert_eq!(
        parse_reset("continuing shortly"),
        Some(ResetSpec::In(SHORTLY))
    );
    assert_eq!(
        parse_reset("Continuing shortly."),
        Some(ResetSpec::In(SHORTLY))
    );
    for bad in [
        "continuing automatically at",
        "continuing",
        "continuing automatically at soon",
    ] {
        assert_eq!(parse_reset(bad), None, "{bad:?}");
    }
    assert_eq!(at("1:50pm") - NOW, (4 * 60 + 55) * 60);
    assert_eq!(at("continuing automatically at 1:50pm"), at("1:50pm"));
    let read_at = NOW + (4 * 60 + 55) * 60 + 18;
    assert_eq!(read_at - reset_at(&one_fifty, read_at, PDT, zones), 18);
    let late = NOW + 14 * 3600 + 5 * 60;
    let three = parse_reset("continuing automatically at 3am").unwrap();
    assert_eq!(reset_at(&three, late, PDT, zones) - late, 4 * 3600);
    assert_eq!(
        at("continuing automatically at 11:05 am") - NOW,
        (2 * 60 + 10) * 60
    );
    assert_eq!(at("shortly"), NOW + 60);
    assert_eq!(at("continuing shortly"), NOW + 60);
    assert!(resumes_by_itself(
        "⚠ Usage limit reached · continuing automatically at 1:50pm · esc to cancel"
    ));
    assert!(resumes_by_itself(
        "⚠ Usage limit reached · Continuing shortly · esc to cancel"
    ));
    assert!(!resumes_by_itself(
        "You've hit your weekly limit · resets Sep 19 at 11am (America/Los_Angeles)"
    ));
    assert!(!resumes_by_itself(
        "You've reached your Fable limit · resets in 3h"
    ));
}

/// A `/goal` echo that says Claude Code continues by itself is the
/// vendor's countdown, armed: the worker busy after it is the
/// continuation.
#[test]
fn a_goal_echo_that_continues_by_itself_is_resumes_by_itself() {
    assert!(resumes_by_itself(
        "Goal paused · usage limit reached · continues automatically when it resets"
    ));
    assert!(!resumes_by_itself(
        "Goal paused · usage limit reached · send a message after it resets to continue"
    ));
}

/// The weekly reset is placed in its zone: 11am Pacific is 18:00 UTC.
#[test]
fn a_dated_time_is_placed_in_its_zone() {
    let sep_19 = 1_789_840_800;
    assert_eq!(at("Sep 19 at 11am (America/Los_Angeles)"), sep_19);
    assert_eq!(at("Sep 19 at 2pm (America/New_York)"), sep_19);
    assert_eq!(at("Sep 19 at 11am"), sep_19);
    assert_eq!(at("Sep 19 at 11am (Mars/Olympus)"), sep_19);
    assert!(sep_19 > NOW && sep_19 - NOW < 3 * 86_400);
}

/// Past and future: a date behind now is behind (the limit reset while
/// the notice sat), and the year the notice leaves out is the one that
/// puts the date nearest now — across New Year either way.
#[test]
fn a_passed_date_is_passed_and_the_year_left_out_is_the_nearest() {
    let sep_15 = at("Sep 15 at 4pm (America/Los_Angeles)");
    assert!(sep_15 < NOW, "{sep_15} < {NOW}");
    assert_eq!(NOW - sep_15, 40 * 3600 + 55 * 60);
    let jan_2 = at("Jan 2 at 3am (America/Los_Angeles)");
    assert!(jan_2 > NOW);
    assert_eq!(civil((jan_2 + PDT).div_euclid(86_400)), (2027, 1, 2));
    let jan_2 = days_from_civil(2027, 1, 2) * 86_400 + 16 * 3600 + 55 * 60;
    let dec_31 = reset_at(
        &parse_reset("Dec 31 at 11am (UTC)").unwrap(),
        jan_2,
        0,
        zones,
    );
    assert_eq!(jan_2 - dec_31, 2 * 86_400 + 5 * 3600 + 55 * 60);
    assert_eq!(civil(dec_31.div_euclid(86_400)), (2026, 12, 31));
    assert_eq!(at("in 3h"), NOW + 3 * 3600);
}

/// A clock time with no date: today's when ahead or passed at most 19 hours ago
/// ([`SAME_DAY`]), else tomorrow's; midnight is `12am`.
#[test]
fn a_bare_time_is_todays_or_tomorrows_and_midnight_is_12am() {
    let t = at("7:30pm (America/Los_Angeles)");
    assert_eq!(t - NOW, (19 * 60 + 30 - (8 * 60 + 55)) * 60);
    let t = at("3am");
    assert_eq!(NOW - t, (5 * 60 + 55) * 60);
    assert_eq!(NOW - at("8:50am"), 5 * 60);
    assert_eq!(NOW - at("12am"), (8 * 60 + 55) * 60);
    assert_eq!(at("12pm") - NOW, (3 * 60 + 5) * 60);
    let half_past_three = NOW + (6 * 60 + 35) * 60;
    let t = reset_at(&parse_reset("3am").unwrap(), half_past_three, PDT, zones);
    assert_eq!(half_past_three - t, (12 * 60 + 30) * 60);
    let late = NOW + 14 * 3600 + 5 * 60;
    let t = reset_at(&parse_reset("3am").unwrap(), late, PDT, zones);
    assert_eq!(t - late, 4 * 3600);
    let ten_pm = NOW + (13 * 60 + 5) * 60;
    let three = parse_reset("3am").unwrap();
    assert_eq!(ten_pm - reset_at(&three, ten_pm, PDT, zones), 19 * 3600);
    assert_eq!(
        reset_at(&three, ten_pm + 60, PDT, zones) - (ten_pm + 60),
        5 * 3600 - 60
    );
    assert_eq!(parse_clock("23:00"), Some((23, 0)));
}

/// America/Los_Angeles around its 2026 switches: PST until 2026-03-08
/// 10:00Z, PDT until 2026-11-01 09:00Z, PST after.
fn la_at(t: i64) -> Option<i64> {
    Some(if (1_772_964_000..1_793_523_600).contains(&t) {
        -7 * 3600
    } else {
        -8 * 3600
    })
}

/// Across a daylight-saving jump a bare clock time lands where the zone's
/// clock shows it then, and the five-hour rule counts real hours.
#[test]
fn a_reset_across_a_dst_jump_is_placed_in_real_time() {
    let four = parse_reset("4am (America/Los_Angeles)").unwrap();
    let spring = 1_772_951_400;
    assert_eq!(
        reset_in_zone(&four, spring, SESSION_AHEAD_MAX, &la_at),
        Some(1_772_967_600)
    );
    let three = parse_reset("3am (America/Los_Angeles)").unwrap();
    let fall = 1_793_514_600;
    assert_eq!(
        reset_in_zone(&three, fall, SESSION_AHEAD_MAX, &la_at),
        Some(1_793_530_800)
    );
    let dated = parse_reset("Mar 8 at 9am (America/Los_Angeles)").unwrap();
    assert_eq!(
        reset_in_zone(&dated, spring, SESSION_AHEAD_MAX, &la_at),
        Some(1_772_985_600)
    );
    let nine = parse_reset("9am (UTC)").unwrap();
    let day = NOW - NOW.rem_euclid(86_400);
    assert_eq!(
        reset_in_zone(&nine, day + 14 * 3600, i64::MAX, &|_| Some(0)),
        Some(day + 86_400 + 9 * 3600)
    );
    assert_eq!(
        reset_in_zone(&nine, day + 14 * 3600, SESSION_AHEAD_MAX, &|_| Some(0)),
        Some(day + 9 * 3600),
        "the session rule: today's, passed"
    );
    assert_eq!(reset_in_zone(&nine, day, i64::MAX, &|_| None), None);
}

#[test]
fn repeated_and_skipped_dst_clocks_place_at_second_pass_and_pushed_forward_instant() {
    let no_zone = |_: &str, _: i64| None;
    for change in [1_793_523_600, 1_805_018_400_i64] {
        let mut now = change - 48 * 3600;
        while now <= change + 48 * 3600 {
            for clock in (0..86_400).step_by(900) {
                let spec = ResetSpec::At {
                    date: None,
                    hour: u32::try_from(clock / 3600).unwrap(),
                    minute: u32::try_from(clock % 3600 / 60).unwrap(),
                    zone: None,
                };
                if let Some(want) = (1..5 * 60)
                    .map(|m| now + m * 60)
                    .find(|&at| clock_at(at) == clock)
                {
                    assert_eq!(
                        reset_at_local(&spec, now, &la_either_change, &no_zone),
                        want,
                        "{clock} s into the day, read at {now}"
                    );
                }
            }
            now += 600;
        }
    }
    // Fall-back repeated hour (2026-11-01 02:00 PDT -> 01:00 PST at 09:00Z = 1_793_523_600):
    // reading `resets 1am` at 01:30 PDT (08:30Z) lands on 01:00 PST (09:00Z), not gone by or tomorrow.
    let fall_change = 1_793_523_600_i64;
    let one_am = parse_reset("1am (America/Los_Angeles)").unwrap();
    assert_eq!(
        reset_in_zone(&one_am, fall_change - 1800, SESSION_AHEAD_MAX, &la_at),
        Some(fall_change)
    );
    assert_eq!(
        reset_in_zone(&one_am, fall_change - 1800, i64::MAX, &la_at),
        Some(fall_change)
    );
    assert_eq!(
        place_reset_local(
            &one_am,
            true,
            fall_change - 1800,
            Some(fall_change - 2400),
            &la_at,
            &|_, t| la_at(t)
        ),
        fall_change
    );
    let dated_repeat = parse_reset("Nov 1 at 1:15am (America/Los_Angeles)").unwrap();
    assert_eq!(
        reset_in_zone(&dated_repeat, fall_change - 1800, SESSION_AHEAD_MAX, &la_at),
        Some(fall_change + 900)
    );
    // Spring-forward skipped hour (2026-03-08 02:00 PST -> 03:00 PDT at 10:00Z = 1_772_964_000):
    // reading `resets 2:30am` at 01:00 PST (09:00Z) pushes forward to 10:30Z (03:30 PDT), never 09:30Z (01:30 PST).
    let spring_change = 1_772_964_000_i64;
    let two_thirty = parse_reset("2:30am (America/Los_Angeles)").unwrap();
    assert_eq!(
        reset_in_zone(&two_thirty, spring_change - 3600, SESSION_AHEAD_MAX, &la_at),
        Some(spring_change + 1800)
    );
    let dated_skip = parse_reset("Mar 8 at 2:30am (America/Los_Angeles)").unwrap();
    assert_eq!(
        reset_in_zone(&dated_skip, spring_change - 3600, SESSION_AHEAD_MAX, &la_at),
        Some(spring_change + 1800)
    );
    // Nuuk midnight skip (2027-03-27 23:00 -02:00 -> 2027-03-28 00:00 -01:00 at 1_806_195_600):
    // reading `resets 11:30pm` at 00:15 on 2027-03-28 places tonight's 23:30, not yesterday's skipped hour.
    let nuuk_change = 1_806_195_600_i64;
    let nuuk = |t: i64| Some(if t < nuuk_change { -7200 } else { -3600 });
    let eleven_thirty = parse_reset("11:30pm").unwrap();
    assert_eq!(
        reset_in_zone(&eleven_thirty, nuuk_change + 900, i64::MAX, &nuuk),
        Some(nuuk_change + 84_600)
    );
    assert_eq!(
        reset_in_zone(&eleven_thirty, nuuk_change - 300, i64::MAX, &nuuk),
        Some(nuuk_change + 1800)
    );
}

/// `date` answers a zone's offset at an instant, where the machine has
/// the zone; a name no zone has, or a path, is asked nothing.
#[cfg(unix)]
#[test]
fn a_zones_offset_is_read_at_the_instant() {
    if !std::path::Path::new("/usr/share/zoneinfo/America/Los_Angeles").is_file() {
        aterm_spec::host::could_not_run("this machine has no zoneinfo for America/Los_Angeles");
    }
    assert_eq!(
        offset_at(Some("America/Los_Angeles"), 1_772_951_400),
        Some(-8 * 3600)
    );
    assert_eq!(
        offset_at(Some("America/Los_Angeles"), 1_772_967_600),
        Some(-7 * 3600)
    );
    assert_eq!(offset_at(Some("UTC"), 1_772_967_600), Some(0));
    assert_eq!(offset_at(Some("Mars/Olympus"), 0), None);
    assert_eq!(offset_at(Some("../etc/passwd"), 0), None);
    assert!(offset_at(None, 0).is_some(), "the local clock's own");
}

/// A dated reset is placed with the offset its zone has where it falls: read on 2026-10-30
/// 12:00 PDT, `Nov 2 at 9am` is PST, 17:00Z, not 16:00Z; read on 2026-03-06 12:00 PST,
/// `Mar 9 at 9am` is PDT, 16:00Z, not 17:00Z.
#[test]
fn a_dated_reset_across_a_dst_change_is_placed_with_the_offset_there() {
    let la = |text: &str, now: i64| {
        let spec = parse_reset(&format!("{text} (America/Los_Angeles)")).expect(text);
        reset_at_with(&spec, now, 0, &|_, t| la_at(t))
    };
    assert_eq!(la("Nov 2 at 9am", 1_793_386_800), 1_793_638_800);
    assert_eq!(la("Mar 9 at 9am", 1_772_827_200), 1_773_072_000);
}

#[test]
fn a_rules_file_becomes_one_line() {
    assert_eq!(
        one_line("Rules:\n  1. run nothing heavy\r\n\n  2. report\t counts\n"),
        "Rules: 1. run nothing heavy 2. report counts"
    );
    assert_eq!(one_line("   "), "");
}

/// The local zone is the C library's (`tm_gmtoff`, held to `date +%z` in
/// `aterm_uds::zone`'s tests), within a day whatever this machine's zone is;
/// a zone no machine has is none.
#[test]
fn the_local_zone_is_the_c_librarys_and_no_zone_is_made_up() {
    assert!(tz_offset_s().abs() < 86_400);
    for unix in [0, 1_790_000_000] {
        assert_eq!(zone_offset_at("Mars/Olympus", unix), None);
        assert_eq!(zone_offset_at("../etc/passwd", unix), None);
    }
}
/// With the print known, a wall's bare clock time is the first occurrence of that clock
/// strictly after it, checked against a brute-force scan for every print over two days and
/// every clock on the hour and half hour; a session window's is too while within
/// [`SESSION_AHEAD_MAX`] of the print, else [`reset_at`]'s.
#[test]
fn a_walls_bare_clock_is_the_first_occurrence_after_its_print() {
    fn zone(_: &str) -> Option<i64> {
        Some(-7 * 3600)
    }
    let local = -7 * 3600;
    let day0 = 1_789_628_400_i64;
    let scan = |clock_s: i64, printed: i64| -> i64 {
        assert_eq!(clock_s % 60, 0, "a clock on the minute");
        let mut t = printed + 1;
        while (t + local).rem_euclid(60) != 0 {
            t += 1;
        }
        loop {
            if (t + local).rem_euclid(86_400) == clock_s && (t - printed) >= 1 {
                // The first whole minute after the print at that clock.
                if t % 60 == 0 || (t + local).rem_euclid(60) == 0 {
                    return t;
                }
            }
            t += 60;
        }
    };
    let mut checked = 0;
    let mut session_disagrees = 0;
    for printed_min in (0..2 * 1440).step_by(7) {
        let printed = day0 + printed_min * 60;
        for hour in 0..24_u32 {
            for minute in [0_u32, 30] {
                let spec = ResetSpec::At {
                    date: None,
                    hour,
                    minute,
                    zone: Some("America/Los_Angeles".to_string()),
                };
                let clock_s = i64::from(hour) * 3600 + i64::from(minute) * 60;
                let want = scan(clock_s, printed);
                let got = place_reset(&spec, false, printed, Some(printed), local, zone);
                assert_eq!(
                    got, want,
                    "print {printed_min} min, clock {hour}:{minute:02}"
                );
                let printed_earlier = printed - 999;
                let want_earlier = scan(clock_s, printed_earlier);
                let expect = if want_earlier - printed_earlier <= SESSION_AHEAD_MAX {
                    want_earlier
                } else {
                    reset_at(&spec, printed, local, zone)
                };
                assert_eq!(
                    place_reset(&spec, true, printed, Some(printed_earlier), local, zone),
                    expect,
                    "session window, print {printed_min} min, clock {hour}:{minute:02}"
                );
                assert_eq!(
                    place_reset(&spec, false, printed, None, local, zone),
                    reset_at(&spec, printed, local, zone)
                );
                if reset_at(&spec, printed, local, zone) != want {
                    session_disagrees += 1;
                    assert!(
                        want - printed >= SESSION_AHEAD_MAX,
                        "{printed_min} {hour}:{minute}"
                    );
                }
                checked += 1;
            }
        }
    }
    assert!(checked > 10_000, "{checked}");
    assert!(
        session_disagrees > 1_000,
        "the control found no disagreement: {session_disagrees}"
    );
    let span = ResetSpec::In(Duration::from_secs(3 * 3600));
    assert_eq!(
        place_reset(&span, false, day0, Some(day0 - 86_400), local, zone),
        day0 + 3 * 3600
    );
    assert!(parse_reset("resets in 9999999999999999999w").is_none());
    assert!(parse_reset("resets in 18446744073709551615h").is_none());
    let _ = printed_by_the_screen(&[], i64::MIN, 0, true);
    let _ = within_a_session_window(
        &ResetSpec::At {
            date: None,
            hour: 0,
            minute: 0,
            zone: None,
        },
        i64::MIN,
        i64::MIN,
    );
}
