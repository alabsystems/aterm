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
#[path = "words_tests.rs"]
mod tests;
