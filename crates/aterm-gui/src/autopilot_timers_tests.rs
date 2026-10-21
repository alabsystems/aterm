// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Autopilot's timer (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3.8):
//! the `until=` parser and its DST rules, the reply, the index, and the
//! App's writer, wire, fire and close.

use super::*;
use crate::session_timeline::{AUTOPILOT_USAGE, Fired, fire_land_at};

/// 2026-10-16T19:00:00Z — a Friday — in unix seconds.
const FRI_19Z: i64 = 1_792_177_200;

/// A zone at a fixed offset from UTC.
fn fixed(offset_s: i64) -> impl Fn(i64) -> Option<i64> {
    move |_| Some(offset_s)
}

/// A European-style zone: UTC+1 in winter, UTC+2 in summer, the clocks going
/// forward at 2027-03-28T01:00Z (02:00 local becomes 03:00) and back at
/// 2026-10-25T01:00Z (03:00 local becomes 02:00).
fn europe(unix: i64) -> Option<i64> {
    const BACK: i64 = 1_792_890_000; // 2026-10-25T01:00:00Z
    const FORWARD: i64 = 1_806_195_600; // 2027-03-28T01:00:00Z
    Some(if (BACK..FORWARD).contains(&unix) {
        3_600
    } else {
        7_200
    })
}

/// Unix seconds of a local reading under `offset_s`.
fn at(date_days: i64, hh: i64, mm: i64, offset_s: i64) -> i64 {
    date_days * 86_400 + hh * 3_600 + mm * 60 - offset_s
}

/// Days since the epoch of a civil date (Hinnant's `days_from_civil`), the
/// test's own inverse of `Date::of_days`.
fn days(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `<when>`: `HH:MM`, and `+<n>h`, `+<n>m`, `+<n>h<n>m` from 1 minute to 24
/// hours. NEGATIVE CONTROLS: every other spelling, `+0m` and one minute past
/// a day among them.
#[test]
fn until_takes_a_clock_time_or_a_duration_from_a_minute_to_a_day() {
    for (text, want) in [
        ("18:00", When::Clock(18 * 60)),
        ("00:00", When::Clock(0)),
        ("23:59", When::Clock(23 * 60 + 59)),
        ("+1m", When::In(1)),
        ("+2h", When::In(120)),
        ("+90m", When::In(90)),
        ("+1h30m", When::In(90)),
        ("+24h", When::In(1440)),
        ("+23h60m", When::In(1440)),
        ("+0h1m", When::In(1)),
    ] {
        assert_eq!(parse_when(text), Some(want), "{text}");
    }
    for text in [
        "", "+", "+h", "+m", "+0m", "+0h", "+24h1m", "+1441m", "+1d", "+1h30", "+h30m", "+-1m",
        "+1.5h", "2h", "1h30m", "24:00", "18:60", "7:00", "18:0", " 18:00", "18:00 ", "now",
        "+99999h",
    ] {
        assert_eq!(parse_when(text), None, "{text:?}");
    }
}

/// `HH:MM` is the earliest instant STRICTLY after now that reads it: today's
/// while it is ahead, else tomorrow's (the midnight wrap), and now itself is
/// not after now. `+DURATION` is elapsed time from the next whole second.
#[test]
fn a_clock_time_is_todays_while_ahead_else_tomorrows() {
    let plus1 = fixed(3_600);
    let now_ms = FRI_19Z * 1000; // 20:00 local
    let today = (FRI_19Z + 3_600).div_euclid(86_400);
    let r = resolve(When::Clock(21 * 60), now_ms, &plus1).unwrap();
    assert_eq!(r.at_ms, at(today, 21, 0, 3_600) * 1000, "today");
    let r = resolve(When::Clock(18 * 60), now_ms, &plus1).unwrap();
    assert_eq!(r.at_ms, at(today + 1, 18, 0, 3_600) * 1000, "tomorrow");
    assert_eq!(
        r.reply(),
        format!(
            "land_at={} local=2026-10-17T18:00+01:00",
            at(today + 1, 18, 0, 3_600)
        )
    );
    let r = resolve(When::Clock(20 * 60), now_ms, &plus1).unwrap();
    assert_eq!(
        r.at_ms,
        at(today + 1, 20, 0, 3_600) * 1000,
        "now itself is no instant after now"
    );
    let r = resolve(When::Clock(20 * 60), now_ms - 1, &plus1).unwrap();
    assert_eq!(r.at_ms, now_ms, "a millisecond before it, it is today's");
    // Durations: from the next whole second, never short.
    let r = resolve(When::In(2), now_ms + 250, &plus1).unwrap();
    assert_eq!(r.at_ms, (FRI_19Z + 1 + 120) * 1000);
    let r = resolve(When::In(1440), now_ms, &plus1).unwrap();
    assert_eq!(r.at_ms - now_ms, 24 * 3_600 * 1000);
    // A zone the platform cannot read places no clock time, and a duration
    // still lands.
    let none = |_| None;
    assert_eq!(
        resolve(When::Clock(60), now_ms, &none),
        Err(UntilRefused::NoZone)
    );
    let r = resolve(When::In(60), now_ms, &none).unwrap();
    assert_eq!(r.reply(), format!("land_at={} local=-", FRI_19Z + 3_600));
}

/// DST, §9.3.5: a time a spring-forward skips is refused WITH THE DATE on the
/// day it would fall — and only there: one already behind now takes
/// tomorrow's. A fall-back night reads a time twice: the earlier reading
/// still ahead. So an `HH:MM` lies at most 25 h of elapsed time ahead.
#[test]
fn dst_skipped_times_are_refused_and_repeated_ones_take_the_earlier_reading_ahead() {
    let mar27 = days(2027, 3, 27);
    let mar28 = days(2027, 3, 28);
    let oct24 = days(2026, 10, 24);
    let oct25 = days(2026, 10, 25);
    // The evening before the clocks go forward: 02:30 tomorrow does not exist.
    let now = at(mar27, 22, 0, 3_600) * 1000;
    let refused = resolve(When::Clock(2 * 60 + 30), now, &europe).unwrap_err();
    assert_eq!(
        refused.reason().as_deref(),
        Some("02:30 does not exist on 2027-03-28 (the clocks go forward)")
    );
    // 03:30 that morning exists, an hour after 01:59 by the wall.
    let r = resolve(When::Clock(3 * 60 + 30), now, &europe).unwrap();
    assert_eq!(r.at_ms, at(mar28, 3, 30, 7_200) * 1000);
    // The same morning, after the jump: the skipped reading is behind now,
    // so tomorrow's 02:30 is taken.
    let now = at(mar28, 10, 0, 7_200) * 1000;
    let r = resolve(When::Clock(2 * 60 + 30), now, &europe).unwrap();
    assert_eq!(r.at_ms, at(mar28 + 1, 2, 30, 7_200) * 1000);
    // A 23 h night: 01:30 on the 27th to 01:30 on the 28th... is 24 h of wall
    // reading and 24 h elapsed (before the jump), and 03:30 is 23 h ahead of
    // 04:30 the day before.
    let now = at(mar27, 4, 30, 3_600) * 1000;
    let r = resolve(When::Clock(4 * 60), now, &europe).unwrap();
    assert_eq!((r.at_ms - now) / 1000, 23 * 3_600 - 30 * 60);

    // The night the clocks go back reads 02:30 twice (at +02:00, then +01:00).
    let now = at(oct24, 23, 0, 7_200) * 1000;
    let r = resolve(When::Clock(2 * 60 + 30), now, &europe).unwrap();
    assert_eq!(
        r.at_ms,
        at(oct25, 2, 30, 7_200) * 1000,
        "the earlier reading"
    );
    assert_eq!(
        r.reply(),
        format!("land_at={} local=2026-10-25T02:30+02:00", r.at_ms / 1000)
    );
    // Between the two readings: the earlier is behind, the later still ahead.
    let now = at(oct25, 2, 45, 7_200) * 1000;
    let r = resolve(When::Clock(2 * 60 + 30), now, &europe).unwrap();
    assert_eq!(r.at_ms, at(oct25, 2, 30, 3_600) * 1000, "the later reading");
    assert_eq!(
        r.reply(),
        format!("land_at={} local=2026-10-25T02:30+01:00", r.at_ms / 1000)
    );
    // The bound: 03:30 on the 24th, asking for 03:30 — the day after holds a
    // 25 h night, and it is never further.
    let now = at(oct24, 3, 30, 7_200) * 1000;
    let r = resolve(When::Clock(3 * 60 + 30), now, &europe).unwrap();
    assert_eq!((r.at_ms - now) / 1000, 25 * 3_600);
    for minute in (0..1440).step_by(7) {
        for now in [
            at(oct24, 3, 30, 7_200),
            at(oct25, 1, 0, 7_200),
            at(mar27, 12, 0, 3_600),
        ] {
            if let Ok(r) = resolve(When::Clock(minute), now * 1000, &europe) {
                let ahead = r.at_ms - now * 1000;
                assert!(
                    ahead > 0 && ahead <= 25 * 3_600 * 1000,
                    "{minute} from {now}: {ahead}"
                );
            }
        }
    }
}

/// The reply's `local=` reading at any offset, a negative one and one with
/// minutes included, and the civil dates under it.
#[test]
fn the_reply_names_the_local_date_and_offset() {
    assert_eq!(
        local_stamp(FRI_19Z * 1000, Some(-5 * 3_600)),
        "2026-10-16T14:00-05:00"
    );
    assert_eq!(
        local_stamp(FRI_19Z * 1000, Some(5 * 3_600 + 1_800)),
        "2026-10-17T00:30+05:30"
    );
    assert_eq!(
        local_stamp(FRI_19Z * 1000, Some(0)),
        "2026-10-16T19:00+00:00"
    );
    assert_eq!(local_stamp(FRI_19Z * 1000, None), "-");
    for (d, text) in [
        (0, "1970-01-01"),
        (days(2000, 2, 29), "2000-02-29"),
        (days(2026, 10, 17), "2026-10-17"),
        (days(1969, 12, 31), "1969-12-31"),
    ] {
        assert_eq!(Date::of_days(d).to_string(), text);
    }
}

/// The face's time and the OTHER-DAY bit (§9.3.2): the local `HH:MM`, and the
/// instant's weekday only where it falls on another local date than now's.
#[test]
fn the_face_names_the_weekday_only_on_another_day() {
    let utc = fixed(0);
    let now = FRI_19Z * 1000;
    let same = glass_of((FRI_19Z + 3_600) * 1000, now, &utc).unwrap();
    assert_eq!((same.clock.as_str(), same.other_day), ("20:00", None));
    let next = glass_of((FRI_19Z + 21 * 3_600) * 1000, now, &utc).unwrap();
    assert_eq!(
        (next.clock.as_str(), next.other_day),
        ("16:00", Some(Weekday::Sat))
    );
    // The same instant, read from a zone where it is still Friday.
    let west = glass_of((FRI_19Z + 21 * 3_600) * 1000, now, &fixed(-18 * 3_600)).unwrap();
    assert_eq!((west.clock.as_str(), west.other_day), ("22:00", None));
    assert_eq!(glass_of(now, now, &|_| None), None);
}

/// THE OFFER NAMES THE DAY IT WOULD ARM (§9.3.3, §9.3.4; the Together run's
/// D6: `land_at = "18:36"` chosen at 18:49 armed Saturday's 18:36 while the
/// menu row and the Settings button read `18:36`). Offered on Friday before
/// 18:36 it arms today and reads `18:36`; at 18:36 itself (an `HH:MM` is
/// strictly after now) and at 18:49 it arms Saturday and reads `Sat 18:36`,
/// the words the state line says once armed. Across midnight: `00:10`
/// offered at 23:59 Friday is `Sat 00:10`, at 00:05 Saturday it is today's,
/// and at 00:10 Saturday Sunday's. The local date decides, not UTC's. At
/// every offer the face is the armed instant's ([`resolve`], [`glass_of`]).
/// NEGATIVE CONTROLS: a zone that cannot be read, and a time the clocks skip
/// on the day it would fall, give the clock alone (no choice arms there).
#[test]
fn the_land_at_offer_names_the_day_once_todays_time_has_passed() {
    use aterm_agent::supervise::LandAt;
    let utc = fixed(0);
    let fri = local_days(FRI_19Z, 0);
    assert_eq!(Weekday::of_days(fri), Weekday::Fri);
    let read = |at: LandAt, now_s: i64, zone: &dyn Fn(i64) -> Option<i64>| {
        let glass = offer_glass(at, now_s * 1000, zone);
        (glass.clock.as_str().to_owned(), glass.other_day)
    };
    let six = LandAt::parse("18:36").expect("a time");
    let ten = LandAt::parse("00:10").expect("a time");
    for (land, now, want) in [
        (six, at(fri, 18, 30, 0), None),
        (six, at(fri, 18, 35, 0) + 59, None),
        (six, at(fri, 18, 36, 0), Some(Weekday::Sat)),
        (six, at(fri, 18, 49, 0), Some(Weekday::Sat)),
        (ten, at(fri, 23, 59, 0), Some(Weekday::Sat)),
        (ten, at(fri + 1, 0, 5, 0), None),
        (ten, at(fri + 1, 0, 10, 0), Some(Weekday::Sun)),
    ] {
        assert_eq!(
            read(land, now, &utc),
            (land.text(), want),
            "{} at {now}",
            land.text()
        );
        let armed = resolve(When::Clock(land.minute()), now * 1000, &utc).expect("arms");
        assert_eq!(
            glass_of(armed.at_ms, now * 1000, &utc),
            Some(offer_glass(land, now * 1000, &utc)),
            "the offer is the armed instant's face"
        );
    }
    // 23:30 UTC on Friday is already Saturday 01:30 two hours east: there,
    // 18:36 is still ahead today; seven hours west it is Friday 16:30.
    let late = at(fri, 23, 30, 0);
    assert_eq!(
        read(six, late, &utc),
        ("18:36".to_owned(), Some(Weekday::Sat))
    );
    assert_eq!(
        read(six, late, &fixed(2 * 3_600)),
        ("18:36".to_owned(), None)
    );
    assert_eq!(
        read(six, late, &fixed(-7 * 3_600)),
        ("18:36".to_owned(), None)
    );
    // NEGATIVE CONTROLS: no zone; a time the spring-forward skips that day.
    assert_eq!(read(six, late, &|_| None), ("18:36".to_owned(), None));
    let skipped = LandAt::parse("02:30").expect("a time");
    let night = at(days(2027, 3, 28), 1, 30, 3_600);
    assert!(resolve(When::Clock(skipped.minute()), night * 1000, &europe).is_err());
    assert_eq!(read(skipped, night, &europe), ("02:30".to_owned(), None));
}

/// THE OFFER'S WORDS TURN AT ITS INSTANT AND AT MIDNIGHT, AND NOWHERE ELSE
/// (§9.3.3: a menu or a page held open across the minute is built again
/// then): before 18:36 the next turn is 18:36 itself (from there it arms
/// tomorrow and names the day); after it, the next local midnight (from there
/// tomorrow is today, and the day goes). Across every minute of a Friday and
/// a Saturday morning, in three zones, the offer reads the same from one
/// turn to the instant before the next, and differently at the turn — so a
/// wake at the turn is the one that keeps the words true. NEGATIVE CONTROL:
/// with no zone the offer is the clock alone and never turns (`None`).
#[test]
fn the_land_at_offer_turns_at_its_instant_and_at_midnight_only() {
    use aterm_agent::supervise::LandAt;
    let fri = local_days(FRI_19Z, 0);
    let six = LandAt::parse("18:36").expect("a time");
    let utc = fixed(0);
    assert_eq!(
        offer_turns_at(six, at(fri, 18, 30, 0) * 1000, &utc),
        Some(at(fri, 18, 36, 0) * 1000)
    );
    assert_eq!(
        offer_turns_at(six, at(fri, 18, 36, 0) * 1000, &utc),
        Some(at(fri + 1, 0, 0, 0) * 1000),
        "at the instant itself the next turn is midnight"
    );
    assert_eq!(
        offer_turns_at(six, at(fri + 1, 0, 0, 0) * 1000, &utc),
        Some(at(fri + 1, 18, 36, 0) * 1000),
        "past midnight, the instant again"
    );
    for (land, offset) in [
        (six, 0),
        (six, 2 * 3_600),
        (LandAt::parse("00:10").expect("a time"), -7 * 3_600),
        (LandAt::parse("00:00").expect("a time"), 0),
    ] {
        let zone = fixed(offset);
        let mut now = at(fri, 0, 0, offset) * 1000 + 1;
        let end = at(fri + 2, 0, 0, offset) * 1000;
        while now < end {
            let turn = offer_turns_at(land, now, &zone).expect("the zone reads");
            assert!(turn > now, "a turn lies ahead");
            let face = offer_glass(land, now, &zone);
            assert_eq!(
                offer_glass(land, turn - 1, &zone),
                face,
                "{} from {now}: no turn before {turn}",
                land.text()
            );
            assert_ne!(
                offer_glass(land, turn, &zone),
                face,
                "{} from {now}: the words turn at {turn}",
                land.text()
            );
            now += 60_000;
        }
    }
    // NEGATIVE CONTROL.
    assert_eq!(offer_turns_at(six, FRI_19Z * 1000, &|_| None), None);
}

/// THE INDEX says when to look: a passed instant is due at once; the earliest
/// due is the one deadline; a due entry is taken OUT; a moved wall derives
/// every due again; an entry goes with its session. With nothing armed there
/// is no deadline at all.
#[test]
fn the_index_derives_its_dues_from_the_wall_and_takes_each_out_once() {
    let mut timers = AutopilotTimers::default();
    let utc = fixed(0);
    let now = Instant::now();
    let wall = FRI_19Z * 1000;
    assert_eq!(timers.earliest(), None, "nothing armed, nothing folded");
    assert!(timers.arm(7, wall + 120_000, now, wall, &utc, None));
    assert!(
        !timers.arm(7, wall + 120_000, now, wall, &utc, None),
        "no move"
    );
    assert!(timers.arm(8, wall - 5_000, now, wall, &utc, Some(ClosedOver::Aterm)));
    assert!(timers.arm(9, wall - 5_000, now, wall, &utc, Some(ClosedOver::Tab)));
    assert!(timers.arm(10, wall + 5_000, now, wall, &utc, Some(ClosedOver::Tab)));
    assert_eq!(
        timers.earliest(),
        Some(now),
        "a passed instant is due at once"
    );
    assert_eq!(
        timers.entry(8).unwrap().passed_while,
        Some(ClosedOver::Aterm)
    );
    assert_eq!(timers.entry(9).unwrap().passed_while, Some(ClosedOver::Tab));
    assert_eq!(
        timers.entry(10).unwrap().passed_while,
        None,
        "an instant still ahead passed during nothing"
    );
    assert_eq!(timers.entry(7).unwrap().passed_while, None);
    assert!(timers.forget(9) && timers.forget(10));
    assert_eq!(
        timers
            .glass(7)
            .map(|g| g.clock.as_str().to_string())
            .as_deref(),
        Some("19:02")
    );
    let due = timers.take_due(now);
    assert_eq!(due.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [8]);
    assert!(timers.take_due(now).is_empty(), "taken out once");
    assert_eq!(timers.earliest(), Some(now + Duration::from_secs(120)));
    // Asleep for 100 s: the monotonic due stands still until the wall is read.
    let later = now + Duration::from_secs(1);
    timers.rederive(later, wall + 101_000, &utc);
    assert_eq!(timers.earliest(), Some(later + Duration::from_secs(19)));
    assert!(timers.forget(7));
    assert!(!timers.forget(7));
    assert!(timers.is_empty());
    assert_eq!(timers.earliest(), None);
}

/// THE SAME INSTANT INDEXED AGAIN IS NO MOVE: a meta write that touched
/// neither half of the pair (a title, an attention, a claim) re-reads the
/// store, and the index derives the same instant's `due` from two clocks read
/// later, a few microseconds off every time. That is no move, so the footer
/// gets no wake for it (§9.3.6, item 1: the footer follows either half); the
/// due itself follows the later reading, and what a restore's passed instant
/// passed while stands for the fire's record. NEGATIVE CONTROLS: another
/// instant, and another face (the zone moved), each move it.
#[test]
fn the_same_instant_indexed_again_is_no_move() {
    let mut timers = AutopilotTimers::default();
    let utc = fixed(0);
    let now = Instant::now();
    let wall = FRI_19Z * 1000;
    assert!(timers.arm(7, wall + 120_000, now, wall, &utc, None));
    // Read again 1.234 s later on the monotonic clock, 1.233 s on the wall.
    let later = now + Duration::from_millis(1_234);
    assert!(
        !timers.arm(7, wall + 120_000, later, wall + 1_233, &utc, None),
        "the same instant, its clocks read later"
    );
    assert_eq!(
        timers.entry(7).unwrap().due,
        later + Duration::from_millis(118_767),
        "the due follows the later reading"
    );
    // A restore's passed instant, indexed again live before its fire.
    assert!(timers.arm(8, wall - 5_000, now, wall, &utc, Some(ClosedOver::Aterm)));
    assert!(!timers.arm(8, wall - 5_000, later, wall + 1_233, &utc, None));
    assert_eq!(
        timers.entry(8).unwrap().passed_while,
        Some(ClosedOver::Aterm)
    );
    // NEGATIVE CONTROLS.
    assert!(
        timers.arm(7, wall + 180_000, later, wall + 1_233, &utc, None),
        "another instant"
    );
    assert!(
        timers.arm(7, wall + 180_000, later, wall + 1_233, &fixed(3_600), None),
        "another face: the zone moved"
    );
}

/// DISPLAY DAY WORDS follow the date's first instant, even where clocks
/// skip 00:00. Ordinary midnights, the first of a repeated midnight and an
/// unreadable zone stay controls; an explicitly requested missing 00:00
/// remains refused. This exercises the production deadline helpers only.
#[test]
fn day_words_refresh_where_the_date_starts_when_midnight_is_skipped() {
    use aterm_agent::supervise::LandAt;
    const DAY: i64 = 86_400_000;
    const HOUR: i64 = 3_600_000;
    let six = LandAt::parse("18:36").expect("a time");
    for offset in [-5 * 3_600, 0, 2 * 3_600] {
        let zone = fixed(offset);
        let now = 9 * DAY + 20 * HOUR - offset * 1000;
        let next = 10 * DAY - offset * 1000;
        assert_eq!(day_words_turn_at(now, &zone), Some(next));
        assert_eq!(offer_turns_at(six, now, &zone), Some(next));
    }
    assert_eq!(day_words_turn_at(9 * DAY, &|_| None), None);
    assert_eq!(offer_turns_at(six, 9 * DAY, &|_| None), None);

    // A one-hour spring jump at 00:00 (-5 to -4), and a fall return
    // at 01:00 to 00:00 (-4 to -5), the existing day-start helper's lane.
    let spring = 10 * DAY + 5 * HOUR;
    let fall = 20 * DAY + 5 * HOUR;
    let zone = |unix_s: i64| {
        Some(if (spring..fall).contains(&(unix_s * 1000)) {
            -4 * 3_600
        } else {
            -5 * 3_600
        })
    };
    let before_first = 20 * DAY + 3 * HOUR;
    let first = 20 * DAY + 4 * HOUR;
    assert_eq!(day_words_turn_at(before_first, &zone), Some(first));
    assert_eq!(offer_turns_at(six, before_first, &zone), Some(first));
    let repeated = first + HOUR / 2;
    assert_eq!(
        resolve(When::Clock(0), repeated, &zone).unwrap().at_ms,
        fall,
        "a requested 00:00 still takes its next reading"
    );
    let before_skip = spring - 60_000;
    assert!(matches!(
        resolve(When::Clock(0), before_skip, &zone),
        Err(UntilRefused::Skipped { .. })
    ));
    let no_offer = day_words_turn_at(before_skip, &zone);
    let with_offer = offer_turns_at(six, before_skip, &zone);
    assert!(
        no_offer == Some(spring) && with_offer == Some(spring),
        "AUTOPILOT_DAY_WORDS_MISSED_SKIPPED_MIDNIGHT: no offer={no_offer:?}, with offer={with_offer:?}"
    );
}

/// A test App with a host, so the harness carries a mode here.
fn hosted() -> (App, crate::harness_host::HostHandle) {
    let mut app = App::headless_for_test();
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    (app, host)
}

/// Session 0's stored pair.
fn stored(app: &App) -> (Option<String>, Option<i64>) {
    let meta = app.pool.get(0).unwrap().ctx.meta.lock().unwrap().clone();
    (meta.autopilot, meta.land_at)
}

/// Session 0's `meta-change` payloads, oldest first.
fn meta_changes(app: &App) -> Vec<String> {
    app.pool
        .get(0)
        .unwrap()
        .ctx
        .timeline
        .lock()
        .unwrap()
        .since(None)
        .filter(|e| e.kind == "meta-change")
        .map(|e| e.payload.to_string())
        .collect()
}

/// The wire's `meta` on session 0, as the dispatch arm runs it: the reply, and
/// whether the stored value moved (its `Wake::MetaChanged`, applied here).
fn wire(app: &mut App, rest: &str) -> (String, bool) {
    let session = app.pool.get(0).unwrap();
    let (term, ctx) = (session.term.clone(), session.ctx.clone());
    let (reply, changed) = crate::control::cmd_meta(&term, &app.store, 0, &ctx, rest);
    if changed {
        app.on_meta_changed(0);
    }
    (reply, changed)
}

/// THE WIRE (§9.3.5): `meta set autopilot cruise until=+DURATION` stores the
/// word and its instant as one write, answers `OK land_at=… local=…` with the
/// `OK` leading, records the `meta-change` with the instant, and its wake
/// indexes the timer; the bare `meta` prints `land_at=`. THE SAME WORD
/// CANCELS: `meta set autopilot cruise` after it leaves no timer and no
/// entry, and is a change (recorded, its wake posted). The owner's word for
/// Cruise takes a time too. NEGATIVE CONTROLS: `until=` with `land`, `off` or
/// no word, two of them, and a malformed `<when>` each refuse with the one
/// usage line and store nothing.
#[test]
fn the_wire_arms_a_timed_cruise_and_the_same_word_cancels_it() {
    let (mut app, host) = hosted();
    let wall = FRI_19Z * 1000;
    let mono = Instant::now();
    set_fake_clocks(Some((mono, wall)));
    let (reply, changed) = wire(&mut app, "set autopilot cruise until=+2h");
    assert!(changed);
    let land_at = FRI_19Z + 2 * 3_600;
    assert!(
        reply.starts_with(&format!("OK land_at={land_at} local=")),
        "{reply}"
    );
    assert_eq!(reply.split_whitespace().next(), Some("OK"), "OK leads");
    assert_eq!(stored(&app), (Some("cruise".into()), Some(land_at * 1000)));
    assert_eq!(
        app.autopilot_timers.entry(0).map(|e| e.land_at_ms),
        Some(land_at * 1000),
        "the wake indexed it"
    );
    assert_eq!(
        app.autopilot_timer_deadline(),
        Some(mono + Duration::from_secs(7_200))
    );
    assert!(
        app.chip_glass(0).land_at.is_some(),
        "the chip wears its time"
    );
    let (read, _) = wire(&mut app, "");
    assert!(
        read.contains(&format!(
            " autopilot=cruise land_at={land_at} autopilot_by=ctl autopilot_since="
        )),
        "{read}"
    );
    assert_eq!(
        meta_changes(&app).last().map(String::as_str),
        Some(format!("field=autopilot value=cruise land_at={land_at}").as_str())
    );

    // THE SAME WORD AGAIN, by hand: the timer goes, as a change.
    let before = meta_changes(&app).len();
    let (reply, changed) = wire(&mut app, "set autopilot cruise");
    assert_eq!(
        (reply.as_str(), changed),
        ("OK\n", true),
        "a change: its wake posts"
    );
    assert_eq!(stored(&app), (Some("cruise".into()), None));
    assert_eq!(meta_changes(&app).len(), before + 1, "recorded");
    assert_eq!(
        meta_changes(&app).last().map(String::as_str),
        Some("field=autopilot value=cruise")
    );
    assert!(app.autopilot_timers.is_empty(), "no entry left");
    assert_eq!(app.autopilot_timer_deadline(), None);

    // The owner's word for Cruise; then `unset` cancels too.
    let (reply, _) = wire(&mut app, "set autopilot forever until=+1h30m");
    assert!(reply.starts_with("OK land_at="), "{reply}");
    assert!(!app.autopilot_timers.is_empty());
    let (_, changed) = wire(&mut app, "unset autopilot");
    assert!(changed);
    assert_eq!(stored(&app), (None, None));
    assert!(app.autopilot_timers.is_empty());

    // Refusals: the one usage line, nothing stored.
    let usage = format!("ERR {AUTOPILOT_USAGE}\n");
    let only_cruise = format!("ERR until= goes with cruise only: {AUTOPILOT_USAGE}\n");
    for (rest, want) in [
        ("set autopilot land until=+1h", &only_cruise),
        ("set autopilot off until=18:00", &only_cruise),
        ("set autopilot until=+1h", &only_cruise),
        ("set autopilot wrap-up until=+1h", &only_cruise),
        ("set autopilot cruise until=+1h until=+2h", &usage),
        ("set autopilot cruise until=", &usage),
        ("set autopilot cruise until=+0m", &usage),
        ("set autopilot cruise until=+25h", &usage),
        ("set autopilot cruise until=6pm", &usage),
        ("set autopilot cruise until=24:00", &usage),
        ("set autopilot maybe", &usage),
    ] {
        let (reply, changed) = wire(&mut app, rest);
        assert_eq!(&reply, want, "{rest}");
        assert!(!changed, "{rest}");
        assert_eq!(stored(&app), (None, None), "{rest}");
    }
    set_fake_clocks(None);
    host.shutdown_and_join();
}

/// THE FIRE IS A COMPARE-AND-SET (§9.3.6, item 5). Due, with the pair still
/// stored, it writes `land` with no timer, marks the driver bit, records the
/// change, rings the host and leaves the record. NEGATIVE CONTROLS: an `off`
/// stored over the wire whose wake the App has not applied wins (the fire
/// finds it and writes nothing, leaving no entry); a clock set back finds
/// the wall short (nothing written; the remainder indexed); a re-arm stored
/// first is indexed again at its new instant.
#[test]
fn the_fire_lands_only_the_stored_pair_and_the_hand_always_wins() {
    let (mut app, host) = hosted();
    let wall = FRI_19Z * 1000;
    let mono = Instant::now();
    set_fake_clocks(Some((mono, wall)));
    let land_at = wall + 120_000;

    // Due, stored: Land.
    assert!(app.set_session_autopilot_timed(
        0,
        Some(Autopilot::Cruise),
        Some(land_at),
        crate::autopilot::Surface::Menu
    ));
    assert_eq!(
        app.autopilot_timer_deadline(),
        Some(mono + Duration::from_secs(120))
    );
    let rings = crate::harness_host::rings_here();
    set_fake_clocks(Some((mono + Duration::from_secs(120), land_at)));
    app.expire_autopilot_timers();
    assert_eq!(stored(&app), (Some("land".into()), None));
    assert_eq!(
        crate::harness_host::rings_here(),
        rings + 1,
        "the host is rung"
    );
    assert!(app.autopilot_timers.is_empty());
    assert!(
        app.pool
            .get(0)
            .unwrap()
            .ctx
            .meta
            .lock()
            .unwrap()
            .driver_wrote(crate::session_timeline::MetaField::Autopilot),
        "the fire's write is this process's: no later graft reverts it"
    );
    assert_eq!(
        meta_changes(&app).last().map(String::as_str),
        Some("field=autopilot value=land")
    );
    let key = format!(
        "harness.autopilot.{}",
        app.pool.get(0).unwrap().ctx.self_id.as_str()
    );
    let record = app
        .messages
        .log()
        .records()
        .rfind(|r| r.key.as_deref() == Some(key.as_str()))
        .map(|r| r.title.clone());
    // Under the glass title rule (§10.2.1): the time is the line's.
    assert_eq!(
        record.as_deref(),
        Some("Autopilot switched to land"),
        "{record:?}"
    );

    // The race: armed, then `off` stored over the wire, its wake NOT applied.
    set_fake_clocks(Some((mono, wall)));
    assert!(app.set_session_autopilot_timed(
        0,
        Some(Autopilot::Cruise),
        Some(land_at),
        crate::autopilot::Surface::Menu
    ));
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    let (reply, changed) = crate::control::cmd_meta(
        &app.pool.get(0).unwrap().term.clone(),
        &app.store,
        0,
        &ctx,
        "set autopilot off",
    );
    assert_eq!((reply.as_str(), changed), ("OK\n", true));
    assert!(
        !app.autopilot_timers.is_empty(),
        "the index trails the store"
    );
    set_fake_clocks(Some((mono + Duration::from_secs(120), land_at)));
    app.expire_autopilot_timers();
    assert_eq!(stored(&app), (Some("off".into()), None), "the hand wins");
    assert!(app.autopilot_timers.is_empty(), "and no entry is left");
    app.on_meta_changed(0);
    assert!(app.autopilot_timers.is_empty());

    // The clock set back under a due entry: the wall is short; nothing is
    // written, and the remainder is indexed.
    set_fake_clocks(Some((mono, wall)));
    assert!(app.set_session_autopilot_timed(
        0,
        Some(Autopilot::Cruise),
        Some(land_at),
        crate::autopilot::Surface::Menu
    ));
    let due = mono + Duration::from_secs(120);
    set_fake_clocks(Some((due, land_at - 30_000)));
    app.expire_autopilot_timers();
    assert_eq!(stored(&app), (Some("cruise".into()), Some(land_at)));
    assert_eq!(
        app.autopilot_timer_deadline(),
        Some(due + Duration::from_secs(30))
    );

    // A re-arm stored first (over the wire, its wake not applied): the fire
    // finds the new instant and indexes it.
    let later = land_at + 600_000;
    assert!(
        crate::session_timeline::apply_autopilot(
            &ctx,
            Some("cruise".into()),
            Some(later),
            aterm_slog::Actor::Ctl,
            None
        )
        .moved
    );
    set_fake_clocks(Some((due + Duration::from_secs(30), land_at)));
    app.expire_autopilot_timers();
    assert_eq!(
        stored(&app),
        (Some("cruise".into()), Some(later)),
        "not landed"
    );
    assert_eq!(
        app.autopilot_timers.entry(0).map(|e| e.land_at_ms),
        Some(later),
        "indexed again at its new instant"
    );
    assert_eq!(
        fire_land_at(&ctx, land_at, land_at),
        Fired::Moved(Some(later)),
        "the old instant is no longer the stored one"
    );
    assert_eq!(fire_land_at(&ctx, later, later - 1), Fired::NotYet(1));
    set_fake_clocks(None);
    host.shutdown_and_join();
}

/// A SESSION CLOSED WITH A TIMER ARMED leaves no entry, so nothing wakes
/// after it (§9.3.7): its entry goes with its registration, and no deadline
/// is folded. NEGATIVE CONTROL: before the close, the deadline is there.
#[test]
fn closing_the_session_drops_its_timer() {
    let (mut app, host) = hosted();
    let wid = crate::WindowId(0);
    let terminal = app.split_active_stub_tab(wid);
    let wall = FRI_19Z * 1000;
    set_fake_clocks(Some((Instant::now(), wall)));
    assert!(app.set_session_autopilot_timed(
        terminal,
        Some(Autopilot::Cruise),
        Some(wall + 3_600_000),
        crate::autopilot::Surface::Menu
    ));
    assert!(app.autopilot_timer_deadline().is_some(), "armed");
    assert!(app.close_session_by_id(terminal).is_ok());
    assert!(app.pool.get(terminal).is_none(), "closed");
    assert_eq!(app.autopilot_timers.entry(terminal), None);
    assert_eq!(app.autopilot_timer_deadline(), None, "nothing folded");
    set_fake_clocks(None);
    host.shutdown_and_join();
}

/// Session 0's newest Autopilot record: its title and its lines.
fn last_record(app: &App) -> (String, Vec<String>) {
    let key = format!(
        "harness.autopilot.{}",
        app.pool.get(0).unwrap().ctx.self_id.as_str()
    );
    app.messages
        .log()
        .records()
        .rfind(|r| r.key.as_deref() == Some(key.as_str()))
        .map(|r| (r.title.clone(), r.detail.clone()))
        .expect("a record")
}

/// Session 0 runs Claude Code.
fn run_claude(app: &App) {
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    let mut tl = ctx.timeline.lock().unwrap();
    tl.note_foreground_group(7);
    tl.set_program(7, Some("claude".into()));
}

/// THE FIRE SAYS WHAT IT FOUND (§10.2.3 item 8; the lifecycle hunt: main's
/// line said "the harness wraps the work up at its next point" over every
/// session): `Autopilot switched to land`, and at 18:00-style time Autopilot
/// switched to land, then — over a hosted agent's work, whatever face a loop
/// told before (Cruise takes nothing as the end, so the turn to Land starts
/// the landing) — that the harness lands all of it on the remote main branch;
/// with the harness switched off, or Claude Code's own switch off, that
/// nothing is landed; in a tab with no agent, that there is nothing to land.
/// NEGATIVE CONTROL: only work the harness carries is landed.
#[test]
fn the_fire_says_what_it_found() {
    use aterm_agent::harness::acts::TaskFace;
    let wall = FRI_19Z * 1000;
    let land_at = wall + 120_000;
    let fire = |agent: bool, face: Option<TaskFace>, after: Option<&str>| {
        let (mut app, host) = hosted();
        if agent {
            run_claude(&app);
        }
        let mono = Instant::now();
        set_fake_clocks(Some((mono, wall)));
        if let Some(face) = face {
            assert!(crate::harness_glass::on_task(
                &mut app,
                0,
                Some(face),
                wall - 60_000
            ));
        }
        assert!(app.set_session_autopilot_timed(
            0,
            Some(Autopilot::Cruise),
            Some(land_at),
            crate::autopilot::Surface::Menu
        ));
        if let Some(policy) = after {
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read(policy),
            );
        }
        set_fake_clocks(Some((mono + Duration::from_secs(120), land_at)));
        app.expire_autopilot_timers();
        assert_eq!(
            stored(&app),
            (Some("land".into()), None),
            "the fire stored land"
        );
        let record = last_record(&app);
        set_fake_clocks(None);
        host.shutdown_and_join();
        record
    };
    let clock = glass_of(land_at, wall, &local_offset_at)
        .expect("the zone reads")
        .clock;
    let turned = format!("at {} Autopilot switched to land; ", clock.as_str());
    for (agent, face, after, then) in [
        (
            true,
            None,
            None,
            "the harness lands all of the work on the remote main branch, then stops".to_string(),
        ),
        (
            true,
            Some(TaskFace::Open),
            None,
            "the harness lands all of the work on the remote main branch, then stops".to_string(),
        ),
        (
            true,
            Some(TaskFace::Landed),
            None,
            "the harness lands all of the work on the remote main branch, then stops".to_string(),
        ),
        (
            true,
            None,
            Some("[harness]\nenabled = false\n"),
            "the harness is off in Settings, so nothing is landed".to_string(),
        ),
        (
            true,
            None,
            Some("[harness.claude]\nenabled = false\n"),
            "Supervise Claude Code sessions is off, so nothing is landed".to_string(),
        ),
        (
            false,
            None,
            None,
            "no agent runs in the tab, so there is nothing to land".to_string(),
        ),
    ] {
        let (title, detail) = fire(agent, face, after);
        let what = format!("{agent} {face:?} {after:?}");
        assert_eq!(title, "Autopilot switched to land", "{what}");
        assert_eq!(detail.len(), 2, "{what}: {detail:?}");
        // Who set it: the timer, by its own clock (§11.7.1).
        assert_eq!(
            detail[1],
            format!("set at the {} timer", clock.as_str()),
            "{what}"
        );
        assert!(
            detail[0].ends_with(&format!("{turned}{then}")),
            "{what}: {detail:?}"
        );
        assert_eq!(
            detail[0].contains("remote main branch"),
            agent && after.is_none(),
            "NEGATIVE CONTROL {what}: only work the harness carries is landed"
        );
    }
}

/// A FIRE OVER A PANE NO AGENT RUNS IN SAYS SO OF THE PANE (behaviour-4 of
/// the post-landing check of Autopilot v3 wave 1, Claude Code 2.1.302): in
/// a tab split Claude Code | shell, a timer set on the shell's pane — or on
/// Claude Code's before it exited there — fires with its place naming that
/// pane, `Tab 1, right pane: …`, and says `no agent runs in this pane`,
/// never `in the tab`, where Claude Code still runs in the left pane. Main
/// read `Tab 1: … no agent runs in the tab`: the place named panes only
/// among agents, and one agent beside a shell names none. NEGATIVE
/// CONTROLS: a shell alone in its tab still reads `Tab 1: … no agent runs
/// in the tab`, with no pane; and Claude Code's own record beside that
/// shell still names no pane (one agent in the tab, §10.3.5).
#[test]
fn a_fire_over_a_pane_with_no_agent_says_so_of_the_pane() {
    let wall = FRI_19Z * 1000;
    let land_at = wall + 120_000;
    let record_of = |app: &App, session: u64| {
        let key = format!(
            "harness.autopilot.{}",
            app.pool.get(session).unwrap().ctx.self_id.as_str()
        );
        app.messages
            .log()
            .records()
            .rfind(|r| r.key.as_deref() == Some(key.as_str()))
            .map(|r| (r.title.clone(), r.detail.clone()))
            .expect("a record")
    };
    let fire = |app: &mut App, session: u64| {
        let mono = Instant::now();
        set_fake_clocks(Some((mono, wall)));
        assert!(app.set_session_autopilot_timed(
            session,
            Some(Autopilot::Cruise),
            Some(land_at),
            crate::autopilot::Surface::Menu
        ));
        set_fake_clocks(Some((mono + Duration::from_secs(120), land_at)));
        app.expire_autopilot_timers();
        set_fake_clocks(None);
        record_of(app, session)
    };
    let clock = glass_of(land_at, wall, &local_offset_at)
        .expect("the zone reads")
        .clock;
    let turned = format!("at {} Autopilot switched to land; ", clock.as_str());

    // Claude Code | shell: the timer on the shell's pane.
    let (mut app, host) = hosted();
    let shell = app.split_active_stub_tab(crate::WindowId(0));
    run_claude(&app);
    let (title, detail) = fire(&mut app, shell);
    assert_eq!(title, "Autopilot switched to land");
    assert_eq!(
        detail,
        [
            format!(
                "Tab 1, right pane: {turned}no agent runs in this pane, so there is nothing to \
                 land"
            ),
            format!("set at the {} timer", clock.as_str())
        ],
        "the pane named, and the pane's own words"
    );
    // CONTROL: Claude Code's own record beside the shell names no pane.
    let (_, claude) = fire(&mut app, 0);
    assert!(
        claude[0].starts_with("Tab 1: ") && !claude[0].contains("pane"),
        "{claude:?}"
    );
    host.shutdown_and_join();

    // CONTROL: a shell alone in its tab is the tab.
    let (mut app, host) = hosted();
    let (_, alone) = fire(&mut app, 0);
    assert_eq!(
        alone,
        [
            format!("Tab 1: {turned}no agent runs in the tab, so there is nothing to land"),
            format!("set at the {} timer", clock.as_str())
        ]
    );
    host.shutdown_and_join();
}

/// A HAND CHANGE NAMES THE TIMER IT DROPPED (§10.2.3 item 8): a timed Cruise
/// set from the window is recorded `Autopilot cruises until HH:MM`; a hand
/// change to Land over it is recorded `Autopilot set to land` with a last
/// line `the HH:MM landing time is cancelled`. NEGATIVE CONTROL: the same
/// change over no timer carries no such line.
#[test]
fn a_hand_change_names_the_timer_it_dropped() {
    let (mut app, host) = hosted();
    run_claude(&app);
    let wall = FRI_19Z * 1000;
    set_fake_clocks(Some((Instant::now(), wall)));
    let land_at = wall + 3_600_000;
    assert!(app.set_session_autopilot_timed(
        0,
        Some(Autopilot::Cruise),
        Some(land_at),
        crate::autopilot::Surface::Menu
    ));
    let when = crate::autopilot_menu::when(
        glass_of(land_at, wall, &local_offset_at).expect("the zone reads"),
    );
    assert_eq!(
        last_record(&app).0,
        format!("Autopilot cruises until {when}")
    );
    assert!(app.set_session_autopilot(0, Some(Autopilot::Land), crate::autopilot::Surface::Menu));
    let (title, detail) = last_record(&app);
    assert_eq!(title, "Autopilot set to land");
    assert_eq!(
        detail[detail.len().saturating_sub(2)..],
        [
            format!("the {when} landing time is cancelled"),
            "set from the Autopilot menu".to_string()
        ],
        "{detail:?}"
    );
    // NEGATIVE CONTROL: no timer, no cancel line.
    assert!(app.set_session_autopilot(0, Some(Autopilot::Cruise), crate::autopilot::Surface::Menu));
    assert!(app.set_session_autopilot(0, Some(Autopilot::Land), crate::autopilot::Surface::Menu));
    let (_, detail) = last_record(&app);
    assert!(
        !detail.iter().any(|l| l.contains("cancelled")),
        "{detail:?}"
    );
    set_fake_clocks(None);
    host.shutdown_and_join();
}

/// THE WINDOW'S CHOICES (the menu, Settings): `In(minutes)` arms from now, `At`
/// takes `[harness] land_at`'s next reading, `Cancel` is Cruise with no timer.
/// NEGATIVE CONTROLS: `At` with no `land_at` names nothing, and under the
/// master switch (or with no host) nothing is written.
#[test]
fn the_windows_timer_choices_arm_and_cancel() {
    let (mut app, host) = hosted();
    let wall = FRI_19Z * 1000;
    set_fake_clocks(Some((Instant::now(), wall)));
    assert!(app.choose_autopilot_timer(0, TimerChoice::In(60), crate::autopilot::Surface::Menu));
    assert_eq!(
        stored(&app),
        (Some("cruise".into()), Some(wall + 3_600_000))
    );
    assert_eq!(
        app.resolve_timer_choice(TimerChoice::At),
        None,
        "no land_at"
    );
    assert!(!app.choose_autopilot_timer(0, TimerChoice::At, crate::autopilot::Surface::Menu));
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nland_at = \"18:00\"\n"),
    );
    assert!(app.resolve_timer_choice(TimerChoice::At).is_some());
    assert!(app.choose_autopilot_timer(0, TimerChoice::At, crate::autopilot::Surface::Menu));
    let at = stored(&app).1.expect("armed at 18:00");
    assert!(at > wall && at - wall <= 25 * 3_600 * 1000);
    assert!(app.choose_autopilot_timer(0, TimerChoice::Cancel, crate::autopilot::Surface::Menu));
    assert_eq!(stored(&app), (Some("cruise".into()), None));
    assert!(app.autopilot_timers.is_empty());
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
    assert!(
        !app.choose_autopilot_timer(0, TimerChoice::In(60), crate::autopilot::Surface::Menu),
        "master switch"
    );
    assert_eq!(stored(&app), (Some("cruise".into()), None));
    app.harness = None;
    assert!(
        !app.choose_autopilot_timer(0, TimerChoice::In(60), crate::autopilot::Surface::Menu),
        "no host"
    );
    set_fake_clocks(None);
    host.shutdown_and_join();
}

/// ...AND BY THE SESSION'S OWN POSTURE, as its mode buttons are: a Claude
/// Code session under `[harness.claude] enabled = false` is steered by
/// nothing, so no timer choice arms or cancels there, though the window's
/// harness runs (for Codex). NEGATIVE CONTROL: with only Codex's switch Off
/// the same choice arms the Claude Code session's timed Cruise.
#[test]
fn a_timer_for_a_switched_off_agent_is_refused() {
    let wall = FRI_19Z * 1000;
    let armed = |policy: &str| {
        let (mut app, host) = hosted();
        {
            let ctx = app.pool.get(0).unwrap().ctx.clone();
            let mut tl = ctx.timeline.lock().unwrap();
            tl.note_foreground_group(7);
            tl.set_program(7, Some("claude".into()));
        }
        app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
            crate::app_config::HarnessPolicy::read(policy),
        );
        assert!(
            matches!(
                app.harness_posture(),
                crate::autopilot::HarnessPosture::Restricted(..)
            ),
            "{policy}"
        );
        set_fake_clocks(Some((Instant::now(), wall)));
        let moved =
            app.choose_autopilot_timer(0, TimerChoice::In(60), crate::autopilot::Surface::Menu);
        let pair = stored(&app);
        set_fake_clocks(None);
        host.shutdown_and_join();
        (moved, pair)
    };
    assert_eq!(
        armed("[harness.claude]\nenabled = false\n"),
        (false, (None, None))
    );
    assert_eq!(
        armed("[harness.codex]\nenabled = false\n"),
        (true, (Some("cruise".into()), Some(wall + 3_600_000)))
    );
}

/// THE WALL MOVED (§9.3.6, item 6): asleep through the time, the monotonic
/// due lies in the future still; `Wake::WallMoved` derives it again from the
/// wall and the fire lands it. A focus gain does the same. NEGATIVE CONTROL:
/// with no timer armed it reads no clock and moves nothing.
#[test]
fn a_moved_wall_derives_the_due_again() {
    let (mut app, host) = hosted();
    let wall = FRI_19Z * 1000;
    let mono = Instant::now();
    set_fake_clocks(Some((mono, wall)));
    app.on_wall_moved();
    assert!(app.autopilot_timers.is_empty());
    assert!(app.set_session_autopilot_timed(
        0,
        Some(Autopilot::Cruise),
        Some(wall + 600_000),
        crate::autopilot::Surface::Menu
    ));
    // Slept an hour: the monotonic clock moved a second.
    let woke = mono + Duration::from_secs(1);
    set_fake_clocks(Some((woke, wall + 3_600_000)));
    app.expire_autopilot_timers();
    assert_eq!(
        stored(&app).0.as_deref(),
        Some("cruise"),
        "the stale due is ahead"
    );
    app.on_wall_moved();
    assert_eq!(app.autopilot_timer_deadline(), Some(woke), "due at once");
    app.expire_autopilot_timers();
    assert_eq!(stored(&app), (Some("land".into()), None));
    set_fake_clocks(None);
    host.shutdown_and_join();
}

/// BEFORE A SUCCESSOR'S COMMIT nothing is folded and nothing fires (it is a
/// candidate); the first park after the Commit rebuilds the index from every
/// stored timer before it fires anything, a time that came meanwhile due at
/// once — owed once. NEGATIVE CONTROL: the timer armed over the wire while
/// the wake was dropped is not in the index until the rebuild.
#[test]
fn a_successor_fires_nothing_before_its_commit_and_rebuilds_at_it() {
    let (mut app, host) = hosted();
    let wall = FRI_19Z * 1000;
    let mono = Instant::now();
    set_fake_clocks(Some((mono, wall)));
    app.handoff_successor = true;
    app.incoming_handoff_pending = true;
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    assert!(
        crate::session_timeline::apply_autopilot(
            &ctx,
            Some("cruise".into()),
            Some(wall + 60_000),
            aterm_slog::Actor::Ctl,
            None
        )
        .moved
    );
    // Its wake is dropped before the Commit.
    assert!(
        app.admit_wake_before_commit(crate::Wake::MetaChanged { session: 0 })
            .is_none()
    );
    assert!(
        app.autopilot_timers.is_empty(),
        "not indexed: the wake was dropped"
    );
    // An entry indexed by a graft is not folded or fired either.
    app.index_autopilot_timer(0, Some(ClosedOver::Aterm));
    assert_eq!(
        app.autopilot_timer_deadline(),
        None,
        "no fold before Commit"
    );
    set_fake_clocks(Some((mono + Duration::from_secs(120), wall + 120_000)));
    app.expire_autopilot_timers();
    assert_eq!(
        stored(&app).0.as_deref(),
        Some("cruise"),
        "no fire before Commit"
    );
    app.autopilot_timers.clear();
    // The Commit lifts the gate (`Wake::ActivateCommittedHandoff`'s first
    // step); the park after it rebuilds from the store, then the time that
    // came lands.
    app.incoming_handoff_pending = false;
    app.settle_autopilot_commit();
    assert_eq!(
        app.autopilot_timers.entry(0).map(|e| e.land_at_ms),
        Some(wall + 60_000),
        "rebuilt"
    );
    app.autopilot_timers.clear();
    app.settle_autopilot_commit();
    assert!(app.autopilot_timers.is_empty(), "owed once");
    // The park itself settles first: a fresh successor, lifted, then parked.
    let (mut app, host2) = hosted();
    app.handoff_successor = true;
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    crate::session_timeline::apply_autopilot(
        &ctx,
        Some("cruise".into()),
        Some(wall + 60_000),
        aterm_slog::Actor::Ctl,
        None,
    );
    assert!(app.autopilot_timers.is_empty(), "stored, never indexed");
    app.expire_autopilot_timers();
    assert_eq!(
        stored(&app),
        (Some("land".into()), None),
        "the park lands it"
    );
    set_fake_clocks(None);
    host.shutdown_and_join();
    host2.shutdown_and_join();
}
