// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::log::{LogRecord, LogState};
use crate::model::{Glyph, MessageId, Origin, Severity, WallStamp, tags};

#[test]
fn the_time_words_change_exactly_when_words_change_after_says() {
    let said = |now: u64, at: u64, offset: i64| {
        let (day, today) = (local_day(at, offset), local_day(now, offset));
        (
            relative_words(now, at),
            spoken_relative_words(now, at),
            local_words(now, at, offset),
            day_heading(today, day, false),
            day_heading(today, day, true),
            day < today,
        )
    };
    let ages = [
        0,
        30_000,
        MS_PER_MIN - 1,
        MS_PER_MIN,
        5 * MS_PER_MIN + 3_000,
        MS_PER_HOUR - 1,
        MS_PER_HOUR,
        3 * MS_PER_HOUR + 7 * MS_PER_MIN,
        MS_PER_DAY - 1,
        MS_PER_DAY,
        30 * MS_PER_HOUR,
        2 * MS_PER_DAY - 1,
        2 * MS_PER_DAY,
        3 * MS_PER_DAY + 5 * MS_PER_HOUR,
        6 * MS_PER_DAY + 11 * MS_PER_HOUR,
        10 * MS_PER_DAY,
        30 * MS_PER_DAY,
        100 * MS_PER_DAY,
        400 * MS_PER_DAY,
    ];
    for at in [0, 1_758_470_000_250, 1_766_952_600_125] {
        for offset in [0, -7 * 3600, 9 * 3600, 5 * 3600 + 1800, i64::MIN, i64::MAX] {
            let mut nows: Vec<u64> = ages.iter().map(|age| at + age).collect();
            nows.extend(at.checked_sub(10_000));
            for now in nows {
                let change = words_change_after(now, at, offset);
                assert!(change > now, "{now} at {offset}");
                assert_eq!(
                    said(change - 1, at, offset),
                    said(now, at, offset),
                    "moved before {change} (at {at}, now {now}, offset {offset})"
                );
                assert_ne!(
                    said(change, at, offset),
                    said(now, at, offset),
                    "nothing moved at {change} (at {at}, now {now}, offset {offset})"
                );
            }
        }
    }
}

#[test]
fn local_words_say_the_readers_own_clock() {
    let at = 1_758_470_000_000;
    assert_eq!(local_words(at, at, 0), "Today 3:53:20 PM");
    assert_eq!(local_words(at, at, -7 * 3600), "Today 8:53:20 AM");
    assert_eq!(local_words(at + MS_PER_DAY, at, 0), "Yesterday 3:53:20 PM");
    assert_eq!(local_words(at + 3 * MS_PER_DAY, at, 0), "Sunday 3:53:20 PM");
    assert_eq!(
        local_words(at + 30 * MS_PER_DAY, at, 0),
        "Sunday, 21 September, 3:53:20 PM"
    );
    assert_eq!(local_words(at, at, 9 * 3600), "Today 12:53:20 AM");
    assert_eq!(
        spoken_relative_words(at + 6 * MS_PER_HOUR, at),
        "6 hours ago"
    );
    assert_eq!(spoken_relative_words(at + MS_PER_MIN, at), "1 minute ago");
    assert_eq!(spoken_relative_words(at, at), "just now");
    assert_eq!(
        spoken_relative_words(at + 36 * MS_PER_HOUR, at),
        "36 hours ago"
    );
    assert_eq!(
        spoken_relative_words(at + 3 * MS_PER_DAY + MS_PER_HOUR, at),
        "3 days ago"
    );
}

#[test]
fn relative_words_read_like_a_person_says_them() {
    let now = 1_758_470_000_000;
    assert_eq!(relative_words(now, now), "just now");
    assert_eq!(relative_words(now, now + 5_000), "just now");
    assert_eq!(relative_words(now, now - 59_999), "just now");
    assert_eq!(relative_words(now, now - 60_000), "1 min ago");
    assert_eq!(relative_words(now, now - 3 * MS_PER_MIN), "3 min ago");
    assert_eq!(relative_words(now, now - 2 * MS_PER_HOUR), "2 h ago");
    assert_eq!(relative_words(now, now - 30 * MS_PER_HOUR), "yesterday");
    assert_eq!(relative_words(now, now - 3 * MS_PER_DAY), "2025-09-18");
}

#[test]
fn day_headings_follow_the_readers_calendar() {
    let noon = 1_790_510_400_000;
    assert_eq!(date_words(noon), "2026-09-27");
    let today = local_day(noon, 0);
    assert_eq!(local_day(noon, 13 * 3600), today + 1, "past midnight east");
    assert_eq!(local_day(noon, -13 * 3600), today - 1, "before it west");
    assert_eq!(local_day(0, -3600), -1, "a day before the epoch is -1");
    assert_eq!(
        day_heading(today, -1, false),
        "Wednesday, 31 December 1969",
        "a day before the epoch keeps its own date"
    );
    assert_eq!(day_heading(today, -719_528, true), "Sat, 1 Jan 0");
    assert_eq!(local_words(0, 0, -7 * 3600), "Today 5:00:00 PM");
    assert_eq!(clock_words(0, -7 * 3600), "5:00 PM");
    let h = |ago: i64| day_heading(today, today - ago, false);
    assert_eq!(h(-1), "Today");
    assert_eq!(h(0), "Today");
    assert_eq!(h(1), "Yesterday");
    assert_eq!(h(2), "Friday");
    assert_eq!(h(3), "Thursday");
    assert_eq!(h(6), "Monday");
    assert_eq!(h(7), "Sunday, 20 September");
    assert_eq!(h(9), "Friday, 18 September");
    assert_eq!(h(269), "Thursday, 1 January");
    assert_eq!(h(270), "Wednesday, 31 December 2025");
    let short = |ago: i64| day_heading(today, today - ago, true);
    assert_eq!(short(1), "Yesterday");
    assert_eq!(short(3), "Thursday");
    assert_eq!(short(9), "Fri, 18 Sep");
    assert_eq!(short(270), "Wed, 31 Dec 2025");
    let forms = |ago: i64| day_forms(today, today - ago);
    assert_eq!(forms(1), ["Yesterday", "Sat"]);
    assert_eq!(forms(3), ["Thursday", "Thu"]);
    assert_eq!(forms(9), ["Fri, 18 Sep", "18 Sep"]);
    assert_eq!(forms(270), ["Wed, 31 Dec 2025", "31 Dec 2025", "Dec 2025"]);
    assert_eq!(clock_words(noon, 0), "12:00 PM");
    assert_eq!(clock_words(noon, -12 * 3600 - 60), "11:59 PM");
    assert_eq!(clock_words(noon + 9 * 60_000, 9 * 3600), "9:09 PM");
    assert_eq!(day_heading(i64::MAX, i64::MAX, false), "Today");
    assert!(!day_heading(i64::MAX, i64::MAX - 2, false).is_empty());
    assert!(!day_forms(i64::MAX, i64::MAX - 3).is_empty());
}

#[test]
fn stamp_words_match_the_rfc3339_goldens() {
    assert_eq!(stamp_words(0), "1970-01-01 00:00:00 UTC");
    assert_eq!(stamp_words(1_700_000_000_000), "2023-11-14 22:13:20 UTC");
    assert_eq!(stamp_words(1_709_164_800_000), "2024-02-29 00:00:00 UTC");
    assert_eq!(stamp_words(1_709_164_799_000), "2024-02-28 23:59:59 UTC");
    assert_eq!(stamp_words(1_758_470_000_123), "2025-09-21 15:53:20 UTC");
}

#[test]
fn a_work_title_reads_finished_through_the_closed_table() {
    assert_eq!(
        finished_form("Installing ALab tools").as_deref(),
        Some("Installed ALab tools")
    );
    assert_eq!(
        finished_form("Building index").as_deref(),
        Some("Built index")
    );
    for kept in [
        "aterm vX is ready",
        "downloading aterm",
        "Downloadingx aterm",
        "Re-Downloading",
        "",
        "Working",
    ] {
        assert_eq!(finished_form(kept), None, "{kept:?} keeps its words");
    }
}

#[test]
fn a_delivered_title_with_no_past_tense_says_done() {
    assert_eq!(
        finished_form("Deploying site").as_deref(),
        Some("Deployed site")
    );
    assert_eq!(
        done_form("Uploading the backup").as_deref(),
        Some("Uploading the backup \u{2014} done")
    );
    for kept in [
        "Deploying site",
        "Paste stopped",
        "aterm is ready",
        "Sing",
        "Re-Uploading",
    ] {
        assert_eq!(done_form(kept), None, "{kept:?}");
    }
}

#[test]
fn a_work_title_reads_ended_through_the_closed_table() {
    assert_eq!(
        ended_form("Indexing docs").as_deref(),
        Some("Indexing ended")
    );
    assert_eq!(
        ended_form("Installing ALab tools").as_deref(),
        Some("Installing ended")
    );
    assert_eq!(ended_form("Paste stopped"), None);
    assert_eq!(ended_form("Typing slowed by yes"), None);
}

#[test]
fn copy_text_is_title_stamp_and_lines() {
    let rec = LogRecord {
        id: MessageId::FIRST,
        stamp: WallStamp {
            unix_ms: 1_700_000_000_000,
        },
        tag: tags::CRASH,
        severity: Severity::Error,
        posted_severity: Severity::Error,
        glyph: Glyph::FALLBACK,
        title: "aterm closed unexpectedly last time".into(),
        detail: vec!["crash log at /x".into(), "signal 11".into()],
        actions: vec![],
        key: None,
        origin: Origin::Host,
        repeats: 1,
        state: LogState::Posted,
        retired_unix_ms: None,
        retired_at: None,
        last_action: None,
        later: false,
    };
    assert_eq!(
        copy_text(&rec),
        "aterm closed unexpectedly last time\n2023-11-14 22:13:20 UTC \u{00b7} crash \u{00b7} error\ncrash log at /x\nsignal 11"
    );
}
