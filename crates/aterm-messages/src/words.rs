// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Words the Settings page and the band share: relative and absolute time,
//! the `~` abbreviation, and the copy-to-clipboard text of a record. Pure.

use crate::log::LogRecord;

const MS_PER_MIN: u64 = 60_000;
const MS_PER_HOUR: u64 = 3_600_000;
const MS_PER_DAY: u64 = 86_400_000;

/// How long ago `at` was, in words: `just now` (under a minute, or in the
/// future), `3 min ago`, `2 h ago`, `yesterday` (24–48 h), else the date
/// `YYYY-MM-DD`.
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

/// `YYYY-MM-DD HH:MM:SS UTC` — a local twin of
/// `aterm_types::rfc3339::format_rfc3339` (no aterm-types dependency), and
/// it SAYS the zone: the page's meta line and the clipboard sit beside local
/// clocks (the terminal's own, the presence row's `Session started 19:10`),
/// and a bare `02:10:28` seven hours off them reads as a wrong clock, not a
/// different one (Phase 2 review).
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

/// When `at` was on the reader's LOCAL clock (design ruling 262), `offset_s`
/// seconds from UTC: `Today 12:53:49 PM`, `Yesterday 9:02:11 AM`, else the
/// day as its header says it ([`day_heading`]: `Thursday 9:02:11 AM`,
/// `Tuesday, 15 September, 9:02:11 AM`) — the local date the meta line of an
/// expanded entry says, beside the relative words its row already shows.
/// Round 18, day four (D14): an ISO `2026-09-24 10:30:00 AM` sat under a
/// `Thursday` header. Copy keeps the UTC stamp ([`stamp_words`]). Pure: the
/// host supplies the offset.
#[must_use]
pub fn local_words(now_unix_ms: u64, at_unix_ms: u64, offset_s: i64) -> String {
    let local = |ms: u64| {
        u64::try_from((i64::try_from(ms / 1000).unwrap_or(i64::MAX)).saturating_add(offset_s))
            .unwrap_or(0)
    };
    let (at, now) = (local(at_unix_ms), local(now_unix_ms));
    let (at_day, now_day) = (at / 86_400, now / 86_400);
    let clock = twelve_hour(at % 86_400, true);
    let day = match now_day.checked_sub(at_day) {
        Some(0) => "Today".to_string(),
        Some(1) => "Yesterday".to_string(),
        _ => {
            let day = |d: u64| i64::try_from(d).unwrap_or(i64::MAX);
            let words = day_heading(day(now_day), day(at_day), false);
            // A dated heading has commas of its own: one more before the
            // clock keeps the time from reading as part of the date.
            if words.contains(',') {
                format!("{words},")
            } else {
                words
            }
        }
    };
    format!("{day} {clock}")
}

/// How long ago `at` was, SPOKEN (design ruling 262): [`relative_words`] with
/// its abbreviations said whole — `6 hours ago`, `1 minute ago`, `just now`.
#[must_use]
pub fn spoken_relative_words(now_unix_ms: u64, at_unix_ms: u64) -> String {
    let words = relative_words(now_unix_ms, at_unix_ms);
    let whole = |n: &str, unit: &str| {
        let plural = if n == "1" { "" } else { "s" };
        format!("{n} {unit}{plural} ago")
    };
    if let Some(n) = words.strip_suffix(" min ago") {
        whole(n, "minute")
    } else if let Some(n) = words.strip_suffix(" h ago") {
        whole(n, "hour")
    } else {
        words
    }
}

/// `secs` seconds into a day on a twelve-hour clock: `9:02 AM`, or with
/// `seconds` `9:02:11 AM`.
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

/// The reader's LOCAL calendar day of `at_unix_ms` — days since 1970-01-01
/// on a clock `offset_s` seconds from UTC (design ruling 273). Pure: the
/// host supplies the offset, as for [`local_words`].
#[must_use]
pub fn local_day(at_unix_ms: u64, offset_s: i64) -> i64 {
    i64::try_from(at_unix_ms / 1000)
        .unwrap_or(i64::MAX)
        .saturating_add(offset_s)
        .div_euclid(86_400)
}

/// When `at` was on the reader's local clock, `offset_s` from UTC, without
/// the day or the seconds: `9:02 AM` — the time a Settings ▸ Messages row
/// shows under a day header older than today (design ruling 273).
#[must_use]
pub fn clock_words(at_unix_ms: u64, offset_s: i64) -> String {
    let secs = i64::try_from(at_unix_ms / 1000)
        .unwrap_or(i64::MAX)
        .saturating_add(offset_s)
        .rem_euclid(86_400);
    twelve_hour(u64::try_from(secs).unwrap_or(0), false)
}

/// A DAY HEADER's words (design ruling 273), for local day `day` read on
/// local day `today` (both [`local_day`] numbers) — the macOS Mail and
/// Messages convention: `Today`, `Yesterday`, the weekday within the last
/// week (`Thursday`), else the date, `Friday, 18 September`, with the year
/// only when it is not this year (`Friday, 18 September 2025`). A day after
/// `today` (a clock set back) reads `Today`. `short` abbreviates the weekday
/// and the month of a date, for a column too narrow for the whole words
/// (`Fri, 18 Sep`, `Fri, 18 Sep 2025`); the other words are already short.
#[must_use]
pub fn day_heading(today: i64, day: i64, short: bool) -> String {
    const WEEKDAYS: [&str; 7] = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
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
    // 1970-01-01 was a Thursday (index 3).
    let weekday = WEEKDAYS[usize::try_from((day + 3).rem_euclid(7)).unwrap_or(0)];
    match today.saturating_sub(day) {
        i64::MIN..=0 => "Today".to_string(),
        1 => "Yesterday".to_string(),
        2..=6 => weekday.to_string(),
        _ => {
            let civil = |d: i64| civil_from_days(u64::try_from(d).unwrap_or(0));
            let ((y, m, d), (this_year, _, _)) = (civil(day), civil(today));
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

/// `YYYY-MM-DD` in UTC.
#[must_use]
pub(crate) fn date_words(at_unix_ms: u64) -> String {
    let days = at_unix_ms / MS_PER_DAY;
    let (y, m, d) = civil_from_days(days);
    let mut out = String::with_capacity(10);
    push_padded(&mut out, y, 4);
    out.push('-');
    push_padded(&mut out, m, 2);
    out.push('-');
    push_padded(&mut out, d, 2);
    out
}

/// Days since 1970-01-01 → proleptic-Gregorian `(year, month, day)` —
/// Howard Hinnant's `civil_from_days`, on the non-negative half only.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    (y, m, d)
}

/// `v` in decimal, zero-padded to at least `width` digits.
fn push_padded(out: &mut String, v: u64, width: usize) {
    let digits = v.to_string();
    for _ in digits.len()..width {
        out.push('0');
    }
    out.push_str(&digits);
}

/// `path` with a leading `home` component abbreviated to `~` — `~` for
/// home itself, `~/sub` below it, sibling and foreign paths verbatim (the
/// pure twin of `app_tabs::home_abbreviated`). `None` or an empty home
/// leaves the path alone; a home given with a trailing slash names the
/// same directory.
#[must_use]
pub(crate) fn home_abbreviate(path: &str, home: Option<&str>) -> String {
    let Some(home) = home.filter(|h| !h.is_empty()) else {
        return path.to_string();
    };
    let trimmed = home.trim_end_matches('/');
    let home = if trimmed.is_empty() { "/" } else { trimmed };
    match path.strip_prefix(home) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// Every path atom in `text` (a whitespace-delimited `home/…` token)
/// abbreviated with [`home_abbreviate`]; the band prints `~/…` while the
/// log keeps the absolute path (D4).
#[must_use]
pub(crate) fn abbreviate_paths_in(text: &str, home: Option<&str>) -> String {
    let Some(home) = home.filter(|h| !h.is_empty()) else {
        return text.to_string();
    };
    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    for c in text.chars() {
        if c.is_whitespace() {
            out.push_str(&home_abbreviate(&token, Some(home)));
            token.clear();
            out.push(c);
        } else {
            token.push(c);
        }
    }
    out.push_str(&home_abbreviate(&token, Some(home)));
    out
}

/// The closed table a Complete echo reads a title's leading present
/// participle through: only the verbs a band title uses, or plausibly will —
/// a script's own titles too (ruling 270: day three's `Deploying site`,
/// `Generating thumbnails` and `Rendering the video` were logged, delivered,
/// in their in-flight words).
const FINISHED_VERBS: [(&str, &str); 40] = [
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
];

/// A work title in its FINISHED form (design ruling 154): `Downloading aterm
/// v0.91.0` → `Downloaded aterm v0.91.0`, through [`FINISHED_VERBS`]. `None`
/// when the first word is not in the table — the title keeps its words.
#[must_use]
pub(crate) fn finished_form(title: &str) -> Option<String> {
    let first = title.split(' ').next().unwrap_or(title);
    let (_, done) = FINISHED_VERBS.iter().find(|(ing, _)| *ing == first)?;
    Some(format!("{done}{}", &title[first.len()..]))
}

/// A DELIVERED work title the table has no past tense for (ruling 270):
/// `Uploading the backup` → `Uploading the backup — done`, the words its
/// Complete echo showed (`DONE_WORD` in the time slot), so the log never
/// reads "still uploading" beside a ✓. Only a title that leads with a
/// present participle (a capitalised word of five letters or more ending
/// `ing`); any other title reads as its outcome already and keeps its
/// words. `None` for a title in the table ([`finished_form`] answers it).
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

/// A work title that ended WITHOUT delivering (design ruling 259):
/// `Indexing photo library` → `Indexing stopped`, through the same table as
/// [`finished_form`]. `None` when the first word is not in it.
#[must_use]
pub(crate) fn stopped_form(title: &str) -> Option<String> {
    let first = title.split(' ').next().unwrap_or(title);
    FINISHED_VERBS
        .iter()
        .any(|(ing, _)| *ing == first)
        .then(|| format!("{first} stopped"))
}

/// A work title that ended with NO outcome to claim (design ruling 265):
/// `Indexing docs` → `Indexing ended`, through the same table as
/// [`finished_form`] — a plain withdraw's record, whose live title read as
/// still running. `None` when the first word is not in it.
#[must_use]
pub(crate) fn ended_form(title: &str) -> Option<String> {
    let first = title.split(' ').next().unwrap_or(title);
    FINISHED_VERBS
        .iter()
        .any(|(ing, _)| *ing == first)
        .then(|| format!("{first} ended"))
}

/// What the page copies for one record: the title, then `<stamp> · <tag> ·
/// <sev>`, then every detail line.
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

    /// THE LOCAL CLOCK (ruling 262): today, yesterday, else the local date,
    /// on a twelve-hour clock — the offset moves the day as well as the hour.
    #[test]
    fn local_words_say_the_readers_own_clock() {
        // 2025-09-21 15:53:20 UTC.
        let at = 1_758_470_000_000;
        assert_eq!(local_words(at, at, 0), "Today 3:53:20 PM");
        assert_eq!(local_words(at, at, -7 * 3600), "Today 8:53:20 AM");
        assert_eq!(local_words(at + MS_PER_DAY, at, 0), "Yesterday 3:53:20 PM");
        // Round 18 (D14): an older day as its header says it, never ISO.
        assert_eq!(local_words(at + 3 * MS_PER_DAY, at, 0), "Sunday 3:53:20 PM");
        assert_eq!(
            local_words(at + 30 * MS_PER_DAY, at, 0),
            "Sunday, 21 September, 3:53:20 PM"
        );
        // Nine hours east, the same instant is past midnight: the next day.
        assert_eq!(local_words(at, at, 9 * 3600), "Today 12:53:20 AM");
        assert_eq!(
            spoken_relative_words(at + 6 * MS_PER_HOUR, at),
            "6 hours ago"
        );
        assert_eq!(spoken_relative_words(at + MS_PER_MIN, at), "1 minute ago");
        assert_eq!(spoken_relative_words(at, at), "just now");
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

    /// DAY HEADERS (ruling 273): the reader's local day — the offset moves
    /// an instant across midnight — and the Mail/Messages words: today,
    /// yesterday, a weekday within the week, then the date, with the year
    /// only off this year; a day ahead of today (a clock set back) is today.
    #[test]
    fn day_headings_follow_the_readers_calendar() {
        // 2026-09-27 12:00:00 UTC is a Sunday.
        let noon = 1_790_510_400_000;
        assert_eq!(date_words(noon), "2026-09-27");
        let today = local_day(noon, 0);
        assert_eq!(local_day(noon, 13 * 3600), today + 1, "past midnight east");
        assert_eq!(local_day(noon, -13 * 3600), today - 1, "before it west");
        assert_eq!(local_day(0, -3600), -1, "a day before the epoch is -1");
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
        assert_eq!(clock_words(noon, 0), "12:00 PM");
        assert_eq!(clock_words(noon, -12 * 3600 - 60), "11:59 PM");
        assert_eq!(clock_words(noon + 9 * 60_000, 9 * 3600), "9:09 PM");
    }

    /// The goldens `format_rfc3339` is pinned to: the epoch, a plain date, a
    /// leap day, the pre-leap-day boundary — each naming its zone.
    #[test]
    fn stamp_words_match_the_rfc3339_goldens() {
        assert_eq!(stamp_words(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(stamp_words(1_700_000_000_000), "2023-11-14 22:13:20 UTC");
        assert_eq!(stamp_words(1_709_164_800_000), "2024-02-29 00:00:00 UTC");
        assert_eq!(stamp_words(1_709_164_799_000), "2024-02-28 23:59:59 UTC");
        assert_eq!(stamp_words(1_758_470_000_123), "2025-09-21 15:53:20 UTC");
    }

    #[test]
    fn home_abbreviation_is_the_tab_label_rule() {
        let home = Some("/Users//w");
        assert_eq!(home_abbreviate("/Users//w", home), "~");
        assert_eq!(
            home_abbreviate("/Users//w/Library/x.log", home),
            "~/Library/x.log"
        );
        assert_eq!(home_abbreviate("/Users//wx/y", home), "/Users//wx/y");
        assert_eq!(home_abbreviate("/tmp/x", home), "/tmp/x");
        assert_eq!(home_abbreviate("/Users//w/x", None), "/Users//w/x");
        assert_eq!(home_abbreviate("/Users//w/x", Some("")), "/Users//w/x");
        // A trailing slash on the home is the same home.
        assert_eq!(
            home_abbreviate("/Users//w/x.log", Some("/Users//w/")),
            "~/x.log"
        );
        assert_eq!(home_abbreviate("/Users//w", Some("/Users//w/")), "~");
        assert_eq!(
            home_abbreviate("/Users//wx/y", Some("/Users//w/")),
            "/Users//wx/y"
        );
        assert_eq!(
            home_abbreviate("/Users//w", Some("/")),
            "/Users//w",
            "the root is nobody's home"
        );
        assert_eq!(home_abbreviate("/Users//w", Some("//")), "/Users//w");
        assert_eq!(
            abbreviate_paths_in("crash log at /Users//w/Library/x.log under /Users//w", home),
            "crash log at ~/Library/x.log under ~"
        );
        assert_eq!(abbreviate_paths_in("no paths here", home), "no paths here");
        assert_eq!(abbreviate_paths_in("/Users//w/x", None), "/Users//w/x");
    }

    /// The finished forms (ruling 154): the leading participle only, through
    /// the closed table; anything else keeps its words.
    #[test]
    fn a_work_title_reads_finished_through_the_closed_table() {
        for (ing, done) in FINISHED_VERBS {
            assert_eq!(finished_form(ing).as_deref(), Some(done));
            assert_eq!(
                finished_form(&format!("{ing} aterm v0.91.0")),
                Some(format!("{done} aterm v0.91.0"))
            );
        }
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

    /// RULING 270: a delivered title the table has no past tense for says
    /// `— done`, as its echo did; a title with no leading participle, and
    /// one the table answers, is left to the other forms.
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

    /// A plain withdraw's record (ruling 265): the leading participle and
    /// `ended`, through the same closed table; anything else keeps its words.
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
        };
        assert_eq!(
            copy_text(&rec),
            "aterm closed unexpectedly last time\n2023-11-14 22:13:20 UTC \u{00b7} crash \u{00b7} error\ncrash log at /x\nsignal 11"
        );
    }
}
