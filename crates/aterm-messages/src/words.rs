// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Relative and local time words and copy-to-clipboard formatting.

use crate::log::LogRecord;

const MS_PER_MIN: u64 = 60_000;
const MS_PER_HOUR: u64 = 3_600_000;
const MS_PER_DAY: u64 = 86_400_000;

/// How long ago `at` was, in words.
#[must_use]
pub fn relative_words(now_unix_ms: u64, at_unix_ms: u64) -> String {
    let ago = now_unix_ms.saturating_sub(at_unix_ms);
    if ago < MS_PER_MIN {
        "just now".to_string()
    } else if ago < MS_PER_HOUR {
        format!("{} min ago", ago / MS_PER_MIN)
    } else if ago < MS_PER_DAY {
        format!("{} h ago", ago / MS_PER_HOUR)
    } else if ago < 2 * MS_PER_DAY {
        "yesterday".to_string()
    } else {
        date_words(at_unix_ms)
    }
}

/// `YYYY-MM-DD HH:MM:SS UTC`.
#[must_use]
pub fn stamp_words(at_unix_ms: u64) -> String {
    let secs = at_unix_ms / 1000;
    let rem = secs % 86_400;
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let mut out = date_words(at_unix_ms);
    out.push(' ');
    push_padded(&mut out, hh, 2);
    out.push(':');
    push_padded(&mut out, mm, 2);
    out.push(':');
    push_padded(&mut out, ss, 2);
    out.push_str(" UTC");
    out
}

/// When `at` was on the reader's local clock ([`day_heading`], [`stamp_words`]).
#[must_use]
pub fn local_words(now_unix_ms: u64, at_unix_ms: u64, offset_s: i64) -> String {
    local_words_at(now_unix_ms, offset_s, at_unix_ms, offset_s)
}

/// [`local_words`], with `now_unix_ms` at `now_offset_s` and `at_unix_ms` at `at_offset_s`.
#[must_use]
pub fn local_words_at(
    now_unix_ms: u64,
    now_offset_s: i64,
    at_unix_ms: u64,
    at_offset_s: i64,
) -> String {
    let clock = twelve_hour(seconds_into_day(at_unix_ms, at_offset_s), true);
    let words = day_heading(
        local_day(now_unix_ms, now_offset_s),
        local_day(at_unix_ms, at_offset_s),
        false,
    );
    let day = if words.contains(',') {
        format!("{words},")
    } else {
        words
    };
    format!("{day} {clock}")
}

/// How long ago `at` was, spoken ([`relative_words`]).
#[must_use]
pub fn spoken_relative_words(now_unix_ms: u64, at_unix_ms: u64) -> String {
    let ago = now_unix_ms.saturating_sub(at_unix_ms);
    let whole = |n: u64, unit: &str| {
        let plural = if n == 1 { "" } else { "s" };
        format!("{n} {unit}{plural} ago")
    };
    if ago < MS_PER_MIN {
        "just now".to_string()
    } else if ago < MS_PER_HOUR {
        whole(ago / MS_PER_MIN, "minute")
    } else if ago < 2 * MS_PER_DAY {
        whole(ago / MS_PER_HOUR, "hour")
    } else {
        whole(ago / MS_PER_DAY, "day")
    }
}

/// The first instant after `now_unix_ms` at which a time word said of a record stamped
/// `at_unix_ms` reads otherwise ([`relative_words`], [`spoken_relative_words`], [`local_words`],
/// [`day_heading`]).
#[must_use]
pub fn words_change_after(now_unix_ms: u64, at_unix_ms: u64, offset_s: i64) -> u64 {
    words_change_after_at(now_unix_ms, offset_s, at_unix_ms, offset_s)
}

/// [`words_change_after`], with `now_unix_ms` at `now_offset_s` and `at_unix_ms` at `at_offset_s`.
#[must_use]
pub fn words_change_after_at(
    now_unix_ms: u64,
    now_offset_s: i64,
    at_unix_ms: u64,
    at_offset_s: i64,
) -> u64 {
    let ago = now_unix_ms.saturating_sub(at_unix_ms);
    let next =
        |unit: u64| at_unix_ms.saturating_add((ago / unit).saturating_add(1).saturating_mul(unit));
    let age_words = if ago < MS_PER_HOUR {
        next(MS_PER_MIN)
    } else if ago < 2 * MS_PER_DAY {
        next(MS_PER_HOUR)
    } else {
        next(MS_PER_DAY)
    };
    let (day, today) = (
        local_day(at_unix_ms, at_offset_s),
        local_day(now_unix_ms, now_offset_s),
    );
    let turn = match today.saturating_sub(day) {
        i64::MIN..=0 => Some(today.max(day).saturating_add(1)),
        1 => Some(today.saturating_add(1)),
        2..=6 => Some(day.saturating_add(7)),
        _ => {
            let ((at_year, _, _), (this_year, _, _)) =
                (civil_from_days(day), civil_from_days(today));
            (at_year == this_year && this_year < i64::MAX)
                .then(|| days_from_civil(this_year + 1, 1, 1))
        }
    };
    let midnight = turn.map_or(u64::MAX, |turn| {
        let secs = i128::from(turn) * 86_400 - i128::from(now_offset_s);
        u64::try_from(secs).map_or(u64::MAX, |secs| secs.saturating_mul(1000))
    });
    age_words.min(midnight)
}

/// `secs` seconds into a day on a twelve-hour clock.
fn twelve_hour(secs: u64, seconds: bool) -> String {
    let (hh, mm, ss) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    let (h12, half) = match hh {
        0 => (12, "AM"),
        1..=11 => (hh, "AM"),
        12 => (12, "PM"),
        _ => (hh - 12, "PM"),
    };
    let mut clock = h12.to_string();
    clock.push(':');
    push_padded(&mut clock, mm, 2);
    if seconds {
        clock.push(':');
        push_padded(&mut clock, ss, 2);
    }
    clock.push(' ');
    clock.push_str(half);
    clock
}

/// `at_unix_ms`'s whole seconds on a clock `offset_s` from UTC, exact: before 1970 they are
/// negative.
#[inline]
fn local_seconds(at_unix_ms: u64, offset_s: i64) -> i128 {
    i128::from(at_unix_ms / 1000) + i128::from(offset_s)
}

/// The seconds `at_unix_ms` is into its local day, `offset_s` from UTC.
#[inline]
fn seconds_into_day(at_unix_ms: u64, offset_s: i64) -> u64 {
    u64::try_from(local_seconds(at_unix_ms, offset_s).rem_euclid(86_400)).unwrap_or(0)
}

/// The reader's local calendar day of `at_unix_ms`.
#[must_use]
pub fn local_day(at_unix_ms: u64, offset_s: i64) -> i64 {
    i64::try_from(local_seconds(at_unix_ms, offset_s).div_euclid(86_400)).unwrap_or(i64::MAX)
}

/// When `at` was on the reader's local clock, `offset_s` from UTC, without
/// the day or the seconds.
#[must_use]
pub fn clock_words(at_unix_ms: u64, offset_s: i64) -> String {
    twelve_hour(seconds_into_day(at_unix_ms, offset_s), false)
}

/// A day header's words for local day `day` read on local day `today` ([`local_day`]).
#[must_use]
pub fn day_heading(today: i64, day: i64, short: bool) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let weekday = weekday(day);
    match today.saturating_sub(day) {
        i64::MIN..=0 => "Today".to_string(),
        1 => "Yesterday".to_string(),
        2..=6 => weekday.to_string(),
        _ => {
            let ((y, m, d), (this_year, _, _)) = (civil_from_days(day), civil_from_days(today));
            let mut month = MONTHS[usize::try_from(m.saturating_sub(1)).unwrap_or(0) % 12];
            let mut weekday = weekday;
            if short {
                month = &month[..3];
                weekday = &weekday[..3];
            }
            if y == this_year {
                format!("{weekday}, {d} {month}")
            } else {
                format!("{weekday}, {d} {month} {y}")
            }
        }
    }
}

/// The name of local day `day`'s weekday ([`local_day`]).
fn weekday(day: i64) -> &'static str {
    const WEEKDAYS: [&str; 7] = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
    WEEKDAYS[usize::try_from((day.rem_euclid(7) + 3) % 7).unwrap_or(0)]
}

/// The words a past local day `day` may take, read on local day `today`, where no header names
/// it, longest first: a header's short words ([`day_heading`]), then fewer that still tell the
/// day where a narrow column would wrap those: a day of the last week's weekday, short ("Yesterday"
/// or "Thursday", then "Sat" or "Thu"); an older one's date ("Sun, 18 Oct", then "18 Oct"),
/// and a past year's its month ("Wed, 31 Dec 2025", "31 Dec 2025", then "Dec 2025").
#[must_use]
pub fn day_forms(today: i64, day: i64) -> Vec<String> {
    let heading = day_heading(today, day, true);
    let mut forms = vec![heading.clone()];
    match today.saturating_sub(day) {
        i64::MIN..=0 => {}
        1..=6 => forms.push(weekday(day).chars().take(3).collect()),
        _ => {
            if let Some((_, date)) = heading.split_once(", ") {
                forms.push(date.to_string());
                let mut words = date.split(' ');
                if let (Some(_), Some(month), Some(year)) =
                    (words.next(), words.next(), words.next())
                {
                    forms.push(format!("{month} {year}"));
                }
            }
        }
    }
    forms
}

/// `YYYY-MM-DD` in UTC.
#[must_use]
pub(crate) fn date_words(at_unix_ms: u64) -> String {
    let days = i64::try_from(at_unix_ms / MS_PER_DAY).unwrap_or(i64::MAX);
    let (y, m, d) = civil_from_days(days);
    let mut out = String::with_capacity(10);
    push_padded(&mut out, u64::try_from(y).unwrap_or(0), 4);
    out.push('-');
    push_padded(&mut out, m, 2);
    out.push('-');
    push_padded(&mut out, d, 2);
    out
}

/// Days since 1970-01-01, negative before it → proleptic-Gregorian `(year, month, day)`
/// (Howard Hinnant's `civil_from_days`), exact for every `i64` day: the year stays within
/// ±2.6·10^16, the month in 1..=12 and the day in 1..=31.
#[inline]
fn civil_from_days(days: i64) -> (i64, u64, u64) {
    let z = i128::from(days) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i128::from(m <= 2);
    (
        i64::try_from(y).unwrap_or(i64::MAX),
        u64::try_from(m).unwrap_or(1),
        u64::try_from(d).unwrap_or(1),
    )
}

/// Proleptic-Gregorian `(year, month, day)` → days since 1970-01-01, negative before it, the
/// inverse of [`civil_from_days`] (Howard Hinnant's `days_from_civil`), clamped to `i64`.
#[inline]
fn days_from_civil(year: i64, month: u64, day: u64) -> i64 {
    let month = i128::from(month);
    let y = i128::from(year) - i128::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + i128::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    i64::try_from(days).unwrap_or(if days < 0 { i64::MIN } else { i64::MAX })
}

/// `v` in decimal, zero-padded to at least `width` digits.
fn push_padded(out: &mut String, v: u64, width: usize) {
    let digits = v.to_string();
    for _ in digits.len()..width {
        out.push('0');
    }
    out.push_str(&digits);
}

/// The closed table a Complete echo reads a title's leading present participle through: the verbs
/// band titles use.
const FINISHED_VERBS: [(&str, &str); 43] = [
    ("Downloading", "Downloaded"),
    ("Installing", "Installed"),
    ("Updating", "Updated"),
    ("Checking", "Checked"),
    ("Verifying", "Verified"),
    ("Finishing", "Finished"),
    ("Saving", "Saved"),
    ("Restoring", "Restored"),
    ("Loading", "Loaded"),
    ("Preparing", "Prepared"),
    ("Copying", "Copied"),
    ("Moving", "Moved"),
    ("Indexing", "Indexed"),
    ("Building", "Built"),
    ("Fetching", "Fetched"),
    ("Deploying", "Deployed"),
    ("Generating", "Generated"),
    ("Rendering", "Rendered"),
    ("Exporting", "Exported"),
    ("Importing", "Imported"),
    ("Publishing", "Published"),
    ("Compiling", "Compiled"),
    ("Converting", "Converted"),
    ("Processing", "Processed"),
    ("Encoding", "Encoded"),
    ("Compressing", "Compressed"),
    ("Extracting", "Extracted"),
    ("Scanning", "Scanned"),
    ("Testing", "Tested"),
    ("Signing", "Signed"),
    ("Packaging", "Packaged"),
    ("Linking", "Linked"),
    ("Migrating", "Migrated"),
    ("Cloning", "Cloned"),
    ("Pulling", "Pulled"),
    ("Pushing", "Pushed"),
    ("Transcoding", "Transcoded"),
    ("Resizing", "Resized"),
    ("Cleaning", "Cleaned"),
    ("Analyzing", "Analyzed"),
    ("Pasting", "Pasted"),
    ("Rewrapping", "Rewrapped"),
    ("Removing", "Removed"),
];

/// A work title in its finished form ([`FINISHED_VERBS`]).
#[must_use]
pub(crate) fn finished_form(title: &str) -> Option<String> {
    let first = title.split(' ').next().unwrap_or(title);
    let (_, done) = FINISHED_VERBS.iter().find(|(ing, _)| *ing == first)?;
    Some(format!("{done}{}", &title[first.len()..]))
}

/// A delivered work title the table has no past tense for: `Uploading the backup` → `Uploading the
/// backup — done` ([`finished_form`]).
#[must_use]
pub(crate) fn done_form(title: &str) -> Option<String> {
    let first = title.split(' ').next().unwrap_or(title);
    let participle = first.len() >= 5
        && first.ends_with("ing")
        && first.starts_with(|c: char| c.is_ascii_uppercase())
        && first.chars().all(|c| c.is_ascii_alphabetic());
    (participle && finished_form(title).is_none())
        .then(|| format!("{title} \u{2014} {}", crate::DONE_WORD))
}

/// A work title that ended without delivering ([`FINISHED_VERBS`]).
#[must_use]
pub(crate) fn stopped_form(title: &str) -> Option<String> {
    let first = title.split(' ').next().unwrap_or(title);
    FINISHED_VERBS
        .iter()
        .any(|(ing, _)| *ing == first)
        .then(|| format!("{first} stopped"))
}

/// A work title that ended with no outcome to claim ([`FINISHED_VERBS`]).
#[must_use]
pub(crate) fn ended_form(title: &str) -> Option<String> {
    let first = title.split(' ').next().unwrap_or(title);
    FINISHED_VERBS
        .iter()
        .any(|(ing, _)| *ing == first)
        .then(|| format!("{first} ended"))
}

/// What the page copies for one record.
#[must_use]
pub fn copy_text(rec: &LogRecord) -> String {
    let mut out = rec.title.clone();
    out.push('\n');
    out.push_str(&stamp_words(rec.stamp.unix_ms));
    out.push_str(" \u{00b7} ");
    out.push_str(rec.tag.as_str());
    out.push_str(" \u{00b7} ");
    out.push_str(rec.severity.as_str());
    for line in &rec.detail {
        out.push('\n');
        out.push_str(line);
    }
    out
}

#[cfg(test)]
mod tests {
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
}
