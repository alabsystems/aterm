// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Usage-limit reset time parser and timezone-aware timestamp resolver.

use std::path::Path;
use std::time::Duration;

/// Parsed reset specification from a usage-limit notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResetSpec {
    /// Wall-clock reset time (`Sep 19 at 11am (America/Los_Angeles)`, `7:30pm`).
    At {
        /// Optional `(month, day)` for weekly limits.
        date: Option<(u32, u32)>,
        hour: u32,
        minute: u32,
        /// Optional IANA timezone name.
        zone: Option<String>,
    },
    /// Relative duration from when the notice was printed (`in 3h`).
    In(Duration),
}

/// A clock time with no date that passed less than this long ago is today's, passed (the
/// notice sat unread across it); older, it is tomorrow's.
const SAME_DAY: i64 = 19 * 3600;

/// Maximum forward offset in seconds for a bare session-limit clock time.
pub const SESSION_AHEAD_MAX: i64 = 86_400 - SAME_DAY;

/// Imminent retry delay assumed for `continuing shortly` notices.
pub const SHORTLY: Duration = Duration::from_secs(60);

/// Parse a [`ResetSpec`] from usage-limit notice `text`.
pub fn parse_reset(text: &str) -> Option<ResetSpec> {
    let text = text.trim().trim_end_matches('.').trim();
    let lower = text.to_ascii_lowercase();
    if lower == "shortly" || lower == "continuing shortly" {
        return Some(ResetSpec::In(SHORTLY));
    }
    if let Some(rest) = lower.strip_prefix("in ") {
        return parse_span(rest).map(ResetSpec::In);
    }
    let auto = aterm_phase::anchor("wall.auto_continue");
    let text = match lower
        .strip_prefix(auto)
        .and_then(|rest| rest.strip_prefix(" at "))
    {
        Some(rest) => text[text.len() - rest.len()..].trim(),
        None => text,
    };
    let (body, zone) = match text.find('(') {
        Some(i) => {
            let zone = text[i + 1..].trim_end_matches(')').trim();
            (
                text[..i].trim(),
                (!zone.is_empty()).then(|| zone.to_string()),
            )
        }
        None => (text, None),
    };
    let words: Vec<&str> = body
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty() && !w.eq_ignore_ascii_case("at"))
        .collect();
    let (date, time) = match words.as_slice() {
        [month, day, time @ ..] if month_of(month).is_some() => {
            let m = month_of(month)?;
            let d: u32 = day
                .trim_end_matches(['s', 't', 'n', 'd', 'r', 'h'])
                .parse()
                .ok()?;
            if !(1..=31).contains(&d) {
                return None;
            }
            let time = match time {
                [year, rest @ ..]
                    if year.len() == 4 && year.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    rest
                }
                t => t,
            };
            (Some((m, d)), time)
        }
        time => (None, time),
    };
    let (hour, minute) = parse_clock(&time.join(" "))?;
    Some(ResetSpec::At {
        date,
        hour,
        minute,
        zone,
    })
}

/// Wall-clock `(date, hour, minute)` identity of a limit reset notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetClock {
    date: Option<(u32, u32)>,
    hour: u32,
    minute: u32,
}

impl ResetClock {
    /// Extract a [`ResetClock`] from optional notice text `reset`.
    #[must_use]
    pub fn of(reset: Option<&str>) -> Option<Self> {
        match parse_reset(reset?)? {
            ResetSpec::At {
                date, hour, minute, ..
            } => Some(Self { date, hour, minute }),
            ResetSpec::In(_) => None,
        }
    }

    /// Render the reset clock in human-readable form without its timezone suffix.
    #[must_use]
    pub fn words(&self) -> String {
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let meridian = if self.hour < 12 { "am" } else { "pm" };
        let hour = match self.hour % 12 {
            0 => 12,
            h => h,
        };
        let clock = if self.minute == 0 {
            format!("{hour}{meridian}")
        } else {
            format!("{hour}:{:02}{meridian}", self.minute)
        };
        let month = self
            .date
            .and_then(|(m, _)| usize::try_from(m).ok()?.checked_sub(1))
            .and_then(|m| MONTHS.get(m));
        match (self.date, month) {
            (Some((_, day)), Some(month)) => format!("on {month} {day} at {clock}"),
            _ => format!("at {clock}"),
        }
    }
}

/// Whether `message` announces an automatic vendor retry (`continuing automatically` / `continuing shortly`).
pub fn resumes_by_itself(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains(aterm_phase::anchor("wall.auto_continue"))
        || lower.contains("continuing shortly")
        || lower
            .replace('\u{2219}', "\u{b7}")
            .contains("available again \u{b7} continuing now")
        || lower.contains(&aterm_phase::anchor("wall.goal_auto").to_ascii_lowercase())
}

/// `3h`, `2h 30m`, `45m`, `3 hours`, `1h30m`, `90 minutes` → a span.
fn parse_span(s: &str) -> Option<Duration> {
    let mut secs: u64 = 0;
    let mut num = String::new();
    let mut any = false;
    for c in s.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        if num.is_empty() {
            continue;
        }
        let unit = c.to_ascii_lowercase();
        if unit == ' ' {
            continue;
        }
        let n: u64 = num.parse().ok()?;
        num.clear();
        secs += match unit {
            'h' => n * 3600,
            'm' => n * 60,
            's' => n,
            _ => return None,
        };
        any = true;
    }
    (any && num.is_empty()).then(|| Duration::from_secs(secs))
}

/// `11am`, `7:30pm`, `12 am`, `12:05pm`, `23:00` → (hour, minute), 24-hour.
fn parse_clock(s: &str) -> Option<(u32, u32)> {
    let s = s.trim().to_ascii_lowercase().replace(' ', "");
    let (digits, meridian) = if let Some(d) = s.strip_suffix("am") {
        (d, Some(false))
    } else if let Some(d) = s.strip_suffix("pm") {
        (d, Some(true))
    } else {
        (s.as_str(), None)
    };
    let (h, m) = match digits.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (digits.parse::<u32>().ok()?, 0),
    };
    if m > 59 {
        return None;
    }
    let hour = match meridian {
        Some(pm) => {
            if !(1..=12).contains(&h) {
                return None;
            }
            (h % 12) + if pm { 12 } else { 0 }
        }
        None => {
            if h > 23 {
                return None;
            }
            h
        }
    };
    Some((hour, m))
}

/// `Sep`, `sept`, `September` → 9.
fn month_of(word: &str) -> Option<u32> {
    let w = word.to_ascii_lowercase();
    if w.len() < 3 {
        return None;
    }
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    MONTHS
        .iter()
        .position(|m| w.starts_with(m))
        .map(|i| u32::try_from(i + 1).unwrap_or(1))
}

/// Resolve the reset time in `message` to a Unix timestamp using a fixed `zone_offset` lookup.
pub fn reset_at(
    spec: &ResetSpec,
    now: i64,
    local_offset_s: i64,
    zone_offset: fn(&str) -> Option<i64>,
) -> i64 {
    reset_at_with(spec, now, local_offset_s, &|zone, _| zone_offset(zone))
}

/// Resolve the reset time in `message` to a Unix timestamp using instant-specific `zone_offset_at`.
pub fn reset_at_with(
    spec: &ResetSpec,
    now: i64,
    local_offset_s: i64,
    zone_offset_at: &dyn Fn(&str, i64) -> Option<i64>,
) -> i64 {
    let offset_at = offset_in(spec, local_offset_s, zone_offset_at);
    reset_in_zone(spec, now, SESSION_AHEAD_MAX, &offset_at).unwrap_or(now)
}

/// The offset of the zone `spec` names at an instant, else the local one's.
fn offset_in<'a>(
    spec: &'a ResetSpec,
    local_offset_s: i64,
    zone_offset_at: &'a dyn Fn(&str, i64) -> Option<i64>,
) -> impl Fn(i64) -> Option<i64> + 'a {
    let zone = match spec {
        ResetSpec::At { zone, .. } => zone.as_deref(),
        ResetSpec::In(_) => None,
    };
    move |unix| {
        Some(
            zone.and_then(|z| zone_offset_at(z, unix))
                .unwrap_or(local_offset_s),
        )
    }
}

/// Resolve `spec` to a Unix timestamp at or after `now` using `offset_at`.
pub fn reset_in_zone(
    spec: &ResetSpec,
    now: i64,
    ahead_max: i64,
    offset_at: &dyn Fn(i64) -> Option<i64>,
) -> Option<i64> {
    let (date, hour, minute) = match spec {
        ResetSpec::In(span) => {
            return Some(now.saturating_add(i64::try_from(span.as_secs()).unwrap_or(0)));
        }
        ResetSpec::At {
            date, hour, minute, ..
        } => (date, hour, minute),
    };
    let clock = i64::from(*hour) * 3600 + i64::from(*minute) * 60;
    let now_offset = offset_at(now)?;
    let (y, m, d) = civil((now + now_offset).div_euclid(86_400));
    let place = |day: i64| -> Option<i64> {
        let local = day * 86_400 + clock;
        Some(local - offset_at(local - now_offset)?)
    };
    match date {
        Some((month, day)) => {
            let mut best: Option<i64> = None;
            for year in [y - 1, y, y + 1] {
                let at = place(days_from_civil(year, *month, *day))?;
                if best.is_none_or(|b| (at - now).abs() < (b - now).abs()) {
                    best = Some(at);
                }
            }
            best
        }
        None => {
            let today_day = days_from_civil(y, m, d);
            let today = place(today_day)?;
            if today > now {
                return Some(today);
            }
            let tomorrow = place(today_day + 1)?;
            Some(if tomorrow - now < ahead_max {
                tomorrow
            } else {
                today
            })
        }
    }
}

/// Maximum lag in seconds assumed between a watched turn ending and the first screen read.
pub const PRINT_LAG_S: i64 = 60;

/// Grace window in seconds within which a just-passed clock time is treated as already passed.
pub const CLOCK_SLACK_S: i64 = 6 * 60;

/// Fold a bare session-limit clock time back by one day when `at` exceeds `now + SESSION_AHEAD_MAX`.
#[must_use]
pub fn within_a_session_window(spec: &ResetSpec, at: i64, now: i64) -> i64 {
    if matches!(spec, ResetSpec::At { date: None, .. })
        && at.saturating_sub(now) > SESSION_AHEAD_MAX
    {
        at - 86_400
    } else {
        at
    }
}

/// Whether `message` names a 5-hour/session limit window.
#[must_use]
pub fn names_a_session_window(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("session limit") || lower.contains("5-hour")
}

/// Estimate when the wall on `rows` was printed, minus [`CLOCK_SLACK_S`].
#[must_use]
pub fn printed_by_the_screen(
    rows: &[String],
    now: i64,
    local_offset_s: i64,
    live: bool,
) -> Option<i64> {
    done_row_ended(rows, now, local_offset_s)
        .or(live.then_some(now - PRINT_LAG_S))
        .map(|p| p - CLOCK_SLACK_S)
}

/// Parse the turn completion Unix timestamp from a `done HH:MM` row in `rows`.
#[must_use]
pub fn done_row_ended(rows: &[String], now: i64, local_offset_s: i64) -> Option<i64> {
    let earliest = now - 86_400 + 60;
    super::policy::turn_end::done_row_clock(rows)
        .and_then(|c| parse_reset(&c))
        .and_then(|d| reset_in_zone(&d, earliest, i64::MAX, &|_| Some(local_offset_s)))
        .filter(|d| *d <= now)
}

/// Resolve a wall's reset Unix timestamp from `message`, `rows`, and `arrived_at`.
pub fn place_reset(
    spec: &ResetSpec,
    session_window: bool,
    now: i64,
    printed: Option<i64>,
    local_offset_s: i64,
    zone_offset: fn(&str) -> Option<i64>,
) -> i64 {
    place_reset_with(
        spec,
        session_window,
        now,
        printed,
        local_offset_s,
        &|zone, _| zone_offset(zone),
    )
}

/// Like [`place_reset`], resolving timezone offsets at the candidate target instant.
pub fn place_reset_with(
    spec: &ResetSpec,
    session_window: bool,
    now: i64,
    printed: Option<i64>,
    local_offset_s: i64,
    zone_offset_at: &dyn Fn(&str, i64) -> Option<i64>,
) -> i64 {
    match (spec, printed) {
        (ResetSpec::At { date: None, .. }, Some(printed)) => {
            let offset_at = offset_in(spec, local_offset_s, zone_offset_at);
            match reset_in_zone(spec, printed, i64::MAX, &offset_at) {
                Some(at) if !session_window || at - printed <= SESSION_AHEAD_MAX => at,
                Some(_) => reset_at_with(spec, now, local_offset_s, zone_offset_at),
                None => now,
            }
        }
        _ => reset_at_with(spec, now, local_offset_s, zone_offset_at),
    }
}

/// Convert days since `1970-01-01` to `(year, month, day)`.
pub(super) fn civil(days: i64) -> (i64, u32, u32) {
    let (y, m, d) = aterm_types::rfc3339::civil_from_days(days);
    (
        y,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

/// Convert `(year, month, day)` to days since `1970-01-01`.
pub(super) fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    aterm_types::rfc3339::days_from_civil(y, i64::from(m), i64::from(d))
}

/// Current Unix timestamp in seconds.
pub fn unix_now() -> i64 {
    aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX))
}

/// Current Unix timestamp in milliseconds.
pub fn unix_now_ms() -> i64 {
    aterm_time::unix_millis().map_or(0, |ms| i64::try_from(ms).unwrap_or(i64::MAX))
}

/// Local timezone offset from UTC in seconds.
pub fn tz_offset_s() -> i64 {
    let mut cmd = aterm_uds::spawnfd::command("date");
    cmd.arg("+%z");
    let out = aterm_uds::deadline::output_within(&mut cmd, Duration::from_secs(10)).ok();
    let text = out
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    parse_zone(&text).unwrap_or(0)
}

/// UTC offset in seconds for IANA `zone` at Unix timestamp `unix`.
pub fn zone_offset_at(zone: &str, unix: i64) -> Option<i64> {
    offset_at(Some(zone), unix)
}

/// UTC offset in seconds for `zone` (or local time when `None`) at Unix timestamp `unix`.
pub fn offset_at(zone: Option<&str>, unix: i64) -> Option<i64> {
    if zone.is_some_and(|z| !zone_known(z)) {
        return None;
    }
    date_offset(zone.map_or(DateZone::Inherited, DateZone::Named), unix)
}

/// Local timezone offset from UTC in seconds at Unix timestamp `unix`.
pub fn local_offset_at(unix: i64) -> Option<i64> {
    offset_at(None, unix)
}

/// Whose zone `date` reads ([`date_offset`]).
enum DateZone<'a> {
    /// This process's own (its `TZ`, else the machine's).
    Inherited,
    /// `TZ=<zone>`.
    Named(&'a str),
}

/// `date +%z` at the instant `unix` in `zone`: the one subprocess behind
/// [`offset_at`].
fn date_offset(zone: DateZone<'_>, unix: i64) -> Option<i64> {
    if !cfg!(unix) {
        return None;
    }
    let mut date = aterm_uds::spawnfd::command("date");
    match zone {
        DateZone::Inherited => {}
        DateZone::Named(zone) => {
            date.env("TZ", zone);
        }
    }
    if cfg!(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd"
    )) {
        date.arg("-r").arg(unix.to_string());
    } else {
        date.arg("-d").arg(format!("@{unix}"));
    }
    date.arg("+%z");
    let out = aterm_uds::deadline::output_within(&mut date, Duration::from_secs(10))
        .ok()
        .filter(|o| o.status.success())?;
    parse_zone(String::from_utf8_lossy(&out.stdout).trim())
}

/// Whether `zone` is a plain zone name `/usr/share/zoneinfo` has.
fn zone_known(zone: &str) -> bool {
    let plain = !zone.is_empty()
        && !zone.contains("..")
        && !zone.starts_with('/')
        && zone
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'));
    plain && Path::new("/usr/share/zoneinfo").join(zone).is_file()
}

/// Parse a `+hhmm` or `-hh:mm` timezone offset into seconds east of UTC.
pub fn parse_zone(text: &str) -> Option<i64> {
    let (negative, rest) = match text.strip_prefix('+') {
        Some(rest) => (false, rest),
        None => (true, text.strip_prefix('-')?),
    };
    let digits: String = rest.chars().filter(char::is_ascii_digit).collect();
    if digits.len() != 4 {
        return None;
    }
    let h: u8 = digits.get(..2)?.parse().ok()?;
    let m: u8 = digits.get(2..)?.parse().ok()?;
    let v = i64::from(h) * 3600 + i64::from(m) * 60;
    Some(if negative { -v } else { v })
}

/// Whether `c` is a control character or Unicode line/paragraph separator.
#[must_use]
pub fn breaks_a_line(c: char) -> bool {
    c.is_control() || c == '\u{2028}' || c == '\u{2029}'
}

/// Collapse control characters and whitespace runs in `text` into a single trimmed line.
pub fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = true;
    for c in text.chars() {
        let c = if breaks_a_line(c) { ' ' } else { c };
        if c == ' ' {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for bad in [
            "",
            "soon",
            "in",
            "in h",
            "25pm",
            "13pm",
            "Sep 40 at 1am",
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

    /// `date` answers a zone's offset at an instant, where the machine has
    /// the zone; a name no zone has, or a path, is asked nothing.
    #[cfg(unix)]
    #[test]
    fn a_zones_offset_is_read_at_the_instant() {
        if !Path::new("/usr/share/zoneinfo/America/Los_Angeles").is_file() {
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

    /// The zone `date +%z` prints, in both spellings, as seconds; anything
    /// else is no zone (and the times that read it fall back to UTC).
    #[test]
    fn the_local_zone_is_read_from_date() {
        for (text, want) in [
            ("+0000", Some(0)),
            ("+0200", Some(7200)),
            ("-0700", Some(-25_200)),
            ("+0530", Some(19_800)),
            ("-07:00", Some(-25_200)),
            ("", None),
            ("UTC", None),
            ("0200", None),
            ("0700", None),
            ("+07", None),
            ("+070000", None),
            ("x+0700", None),
            ("+1\u{e9}1", None),
        ] {
            assert_eq!(parse_zone(text), want, "{text:?}");
        }
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
    }
}
