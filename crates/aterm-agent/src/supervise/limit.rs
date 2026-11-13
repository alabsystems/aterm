// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Usage-limit reset time parser and timezone-aware timestamp resolver.

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
    let auto = aterm_phase::anchor!("wall.auto_continue");
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
        [ymd, time @ ..] if iso_month_day(ymd).is_some() => (iso_month_day(ymd), time),
        [month, day, time @ ..] if month_of(month).is_some() => {
            let m = month_of(month)?;
            let day = day
                .strip_suffix("st")
                .or_else(|| day.strip_suffix("nd"))
                .or_else(|| day.strip_suffix("rd"))
                .or_else(|| day.strip_suffix("th"))
                .unwrap_or(day);
            if day.is_empty() || !day.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let d: u32 = day.parse().ok()?;
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
    let (time, zone) = match (zone, time) {
        (None, [rest @ .., tz])
            if !rest.is_empty()
                && !tz.eq_ignore_ascii_case("am")
                && !tz.eq_ignore_ascii_case("pm")
                && (3..=5).contains(&tz.len())
                && tz.bytes().all(|b| b.is_ascii_alphabetic()) =>
        {
            (rest, tz_abbrev_zone(tz).map(str::to_owned))
        }
        (z, t) => (t, z),
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
    lower.contains(aterm_phase::anchor!("wall.auto_continue"))
        || lower.contains("continuing shortly")
        || lower
            .replace('\u{2219}', "\u{b7}")
            .contains("available again \u{b7} continuing now")
        || lower.contains(&aterm_phase::anchor!("wall.goal_auto").to_ascii_lowercase())
}

/// Whether `message`, a usage wall's words, draws Claude Code's own continue as at hand
/// (`Continuing shortly`): its countdown has run out, so the reset it was armed for is at most
/// [`SHORTLY`] away, whatever bare clock time the footer still draws beside it (2.1.302's `limit
/// resets 12am — Continuing shortly`, measured a day and more past that midnight).
#[must_use]
pub fn continues_shortly(message: &str) -> bool {
    message.to_ascii_lowercase().contains("continuing shortly")
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
            if !c.is_ascii_alphabetic() && !c.is_whitespace() {
                return None;
            }
            continue;
        }
        let unit = c.to_ascii_lowercase();
        if unit == ' ' {
            continue;
        }
        let n: u64 = num.parse().ok()?;
        num.clear();
        let unit_secs: u64 = match unit {
            'w' => 7 * 86_400,
            'd' => 86_400,
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
        secs = secs.checked_add(n.checked_mul(unit_secs)?)?;
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
        Some((h, m)) => {
            if h.is_empty()
                || m.len() != 2
                || !h.bytes().chain(m.bytes()).all(|b| b.is_ascii_digit())
            {
                return None;
            }
            (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?)
        }
        None => {
            meridian?;
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (digits.parse::<u32>().ok()?, 0)
        }
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
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    MONTHS
        .iter()
        .position(|m| m.starts_with(&w))
        .map(|i| u32::try_from(i + 1).unwrap_or(1))
}

/// `2026-10-29` → `(10, 29)`.
fn iso_month_day(word: &str) -> Option<(u32, u32)> {
    let mut parts = word.split('-');
    let year = parts.next()?;
    let month = parts.next()?;
    let day = parts.next()?;
    if parts.next().is_some()
        || year.len() != 4
        || month.len() != 2
        || day.len() != 2
        || !year
            .bytes()
            .chain(month.bytes())
            .chain(day.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let m: u32 = month.parse().ok()?;
    let d: u32 = day.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((m, d))
}

/// Map a trailing timezone abbreviation (`PDT`, `UTC`, …) to an IANA zone where unambiguous.
fn tz_abbrev_zone(tz: &str) -> Option<&'static str> {
    match tz.to_ascii_uppercase().as_str() {
        "UTC" | "GMT" => Some("UTC"),
        "PDT" | "PST" => Some("America/Los_Angeles"),
        "EDT" | "EST" => Some("America/New_York"),
        "CDT" | "CST" => Some("America/Chicago"),
        "MDT" | "MST" => Some("America/Denver"),
        _ => None,
    }
}

/// The offset in seconds a trailing timezone abbreviation names. Claude Code's `%Z` prints the
/// zone's en-US short name, so Phoenix's row says `MST` in July: the name, not [`tz_abbrev_zone`]'s
/// zone, tells the offset.
fn tz_abbrev_offset_s(tz: &str) -> Option<i64> {
    let hours = match tz.to_ascii_uppercase().as_str() {
        "UTC" | "GMT" => 0,
        "ADT" => -3,
        "AST" | "EDT" => -4,
        "EST" | "CDT" => -5,
        "CST" | "MDT" => -6,
        "MST" | "PDT" => -7,
        "PST" | "AKDT" => -8,
        "AKST" | "HADT" => -9,
        "HST" | "HAST" => -10,
        _ => return None,
    };
    Some(hours * 3600)
}

/// A done row's clock, less any offset its end names: the `24-hour-utc` format's `16:31Z`, en-US's
/// `12:31 AM GMT+8` for a zone with no short name, or an abbreviation ([`tz_abbrev_offset_s`]),
/// which [`parse_reset`] reads past itself. Those last two may sit in parentheses or carry
/// punctuation; an end naming no offset this knows, `(GMT+0800)`, names none.
fn row_clock_and_offset(text: &str) -> (&str, Option<i64>) {
    if let Some(clock) = text
        .strip_suffix(['Z', 'z'])
        .filter(|clock| clock.ends_with(|c: char| c.is_ascii_digit()))
    {
        return (clock, Some(0));
    }
    let tail = text.trim_end_matches([')', '.', ',']);
    let (clock, end) = tail.split_at(tail.rfind([' ', '(']).map_or(0, |at| at + 1));
    if let Some(offset) = end.strip_prefix("GMT").and_then(signed_offset_s) {
        return (clock.trim_end_matches([' ', '(']), Some(offset));
    }
    (text, tz_abbrev_offset_s(end))
}

/// The seconds in `+8`, `-3` or `+5:30`.
fn signed_offset_s(offset: &str) -> Option<i64> {
    let (sign, offset) = match offset.split_at_checked(1)? {
        ("+", rest) => (1, rest),
        ("-", rest) => (-1, rest),
        _ => return None,
    };
    let (hours, minutes) = offset.split_once(':').unwrap_or((offset, "0"));
    let hours: u8 = hours.parse().ok()?;
    let minutes: u8 = minutes.parse().ok()?;
    (hours <= 14 && minutes < 60)
        .then(|| sign * (i64::from(hours) * 3600 + i64::from(minutes) * 60))
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
    reset_at_local(spec, now, &|_| Some(local_offset_s), zone_offset_at)
}

/// [`reset_at_with`], reading the local zone at the candidate instant as well (`local_at`).
pub fn reset_at_local(
    spec: &ResetSpec,
    now: i64,
    local_at: &dyn Fn(i64) -> Option<i64>,
    zone_offset_at: &dyn Fn(&str, i64) -> Option<i64>,
) -> i64 {
    let offset_at = offset_in(spec, local_at, zone_offset_at);
    reset_in_zone(spec, now, SESSION_AHEAD_MAX, &offset_at).unwrap_or(now)
}

/// The offset of the zone `spec` names at an instant, else the local one's. A span names no clock
/// and a zone known here is not the local one, so neither reads `local_at`.
fn offset_in<'a>(
    spec: &'a ResetSpec,
    local_at: &'a dyn Fn(i64) -> Option<i64>,
    zone_offset_at: &'a dyn Fn(&str, i64) -> Option<i64>,
) -> impl Fn(i64) -> Option<i64> + 'a {
    let zone = match spec {
        ResetSpec::At { zone, .. } => zone.as_deref(),
        ResetSpec::In(_) => None,
    };
    move |unix| {
        zone.and_then(|z| zone_offset_at(z, unix))
            .or_else(|| local_at(unix))
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
            return Some(now.saturating_add_unsigned(span.as_secs()));
        }
        ResetSpec::At {
            date, hour, minute, ..
        } => (date, hour, minute),
    };
    let clock = i64::from(*hour) * 3600 + i64::from(*minute) * 60;
    let now_offset = offset_at(now)?;
    let (y, m, d) = civil(now.saturating_add(now_offset).div_euclid(86_400));
    match date {
        Some((month, day)) => {
            let mut best: Option<i64> = None;
            for year in [y.saturating_sub(1), y, y.saturating_add(1)] {
                let instants = day_instants_or_pushed_forward(
                    days_from_civil(year, *month, *day),
                    clock,
                    offset_at,
                )?;
                let at = instants
                    .into_iter()
                    .flatten()
                    .find(|&t| t > now)
                    .or_else(|| instants.into_iter().flatten().next_back())?;
                if best.is_none_or(|b| at.abs_diff(now) < b.abs_diff(now)) {
                    best = Some(at);
                }
            }
            best
        }
        None => {
            let today_day = days_from_civil(y, m, d);
            let prev = sorted_day_instants(today_day.saturating_sub(1), clock, offset_at);
            let today = day_instants_or_pushed_forward(today_day, clock, offset_at)?;
            if let Some(upcoming) = prev.into_iter().chain(today).flatten().find(|&t| t > now) {
                return Some(upcoming);
            }
            let today_passed = today.into_iter().flatten().next_back()?;
            let tomorrow =
                day_instants_or_pushed_forward(today_day.saturating_add(1), clock, offset_at)?
                    .into_iter()
                    .flatten()
                    .next()?;
            Some(if tomorrow.saturating_sub(now) < ahead_max {
                tomorrow
            } else {
                today_passed
            })
        }
    }
}

fn sorted_day_instants(
    day: i64,
    clock: i64,
    local_at: &dyn Fn(i64) -> Option<i64>,
) -> [Option<i64>; 2] {
    match instants_of_clock(day, clock, local_at) {
        [Some(a), Some(b)] if a == b => [Some(a), None],
        [Some(a), Some(b)] => [Some(a.min(b)), Some(a.max(b))],
        [Some(a), None] | [None, Some(a)] => [Some(a), None],
        [None, None] => [None, None],
    }
}

fn day_instants_or_pushed_forward(
    day: i64,
    clock: i64,
    local_at: &dyn Fn(i64) -> Option<i64>,
) -> Option<[Option<i64>; 2]> {
    match sorted_day_instants(day, clock, local_at) {
        [None, None] => {
            let local = day.checked_mul(86_400)?.checked_add(clock)?;
            let before = local_at(local.saturating_sub(86_400))?;
            Some([Some(local.checked_sub(before)?), None])
        }
        instants => Some(instants),
    }
}

/// Maximum lag in seconds assumed between a watched turn ending and the first screen read.
pub const PRINT_LAG_S: i64 = 60;

/// Grace window in seconds within which a just-passed clock time is treated as already passed.
pub const CLOCK_SLACK_S: i64 = 6 * 60;

/// Fold a bare session-limit clock time back by one day when `at` exceeds `now + SESSION_AHEAD_MAX`.
#[must_use]
pub fn within_a_session_window(spec: &ResetSpec, at: i64, now: i64) -> i64 {
    within_a_session_window_local(spec, at, now, &|_| Some(0), &|_, _| None)
}

/// [`within_a_session_window`] by the calendar: the same clock a day before, whichever side of a
/// daylight change that is. A clock that day skips stays 24 hours before `at`.
#[must_use]
pub fn within_a_session_window_local(
    spec: &ResetSpec,
    at: i64,
    now: i64,
    local_at: &dyn Fn(i64) -> Option<i64>,
    zone_offset_at: &dyn Fn(&str, i64) -> Option<i64>,
) -> i64 {
    if !matches!(spec, ResetSpec::At { date: None, .. })
        || at.saturating_sub(now) <= SESSION_AHEAD_MAX
    {
        return at;
    }
    let offset = offset_in(spec, local_at, zone_offset_at);
    let flat = at.saturating_sub(86_400);
    let same_clock = || {
        let there = offset(flat)?;
        let folded = flat.checked_add(offset(at)?)?.checked_sub(there)?;
        (offset(folded)? == there).then_some(folded)
    };
    same_clock().unwrap_or(flat)
}

/// Whether `at` shows, a whole number of days later, the clock `named` showed in `spec`'s zone
/// (within a minute). A day across a daylight change is 23 or 25 hours, and a flat 24 hours across
/// it is another clock.
#[must_use]
pub fn is_a_later_day_of(
    spec: &ResetSpec,
    at: i64,
    named: i64,
    local_at: &dyn Fn(i64) -> Option<i64>,
    zone_offset_at: &dyn Fn(&str, i64) -> Option<i64>,
) -> bool {
    if at.saturating_sub(named) < 43_200 {
        return false;
    }
    let offset = offset_in(spec, local_at, zone_offset_at);
    let (Some(then), Some(there)) = (offset(at), offset(named)) else {
        return false;
    };
    let before = (i128::from(at) + i128::from(then)) - (i128::from(named) + i128::from(there));
    let days = (before + 43_200).div_euclid(86_400);
    days >= 1 && (before - days * 86_400).abs() < 60
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
    printed_from(done_row_ended(rows, now, local_offset_s), now, live)
}

/// [`printed_by_the_screen`], from the done row's `ended`.
#[must_use]
pub fn printed_from(ended: Option<i64>, now: i64, live: bool) -> Option<i64> {
    ended
        .or(live.then_some(now.saturating_sub(PRINT_LAG_S)))
        .map(|p| p.saturating_sub(CLOCK_SLACK_S))
}

/// Parse the turn completion Unix timestamp from a `done HH:MM` row in `rows`.
#[must_use]
pub fn done_row_ended(rows: &[String], now: i64, local_offset_s: i64) -> Option<i64> {
    done_row_ended_local(rows, now, &|_| Some(local_offset_s))
}

/// [`done_row_ended`], reading the local zone at each instant (`local_at`): the row was printed
/// under the offset of its own time, which a daylight change since has moved. The row is the turn
/// just past, so its clock is the newest instant at or before `now` that shows it: a day of 23
/// hours can hold one clock twice within the reach, and one of 25 hours can repeat it. A row ending
/// in an offset ([`row_clock_and_offset`]) names it outright, whichever zone printed it, so that
/// offset settles the instant. A row naming its date (`2026-11-01 01:30`) is that date in its nearest year,
/// older than the reach or none while still to come.
#[must_use]
pub fn done_row_ended_local(
    rows: &[String],
    now: i64,
    local_at: &dyn Fn(i64) -> Option<i64>,
) -> Option<i64> {
    let earliest = now.saturating_sub(86_400).saturating_add(60);
    let text = super::policy::turn_end::done_row_clock(rows)?;
    let (clock, named) = row_clock_and_offset(&text);
    let spec = parse_reset(clock)?;
    let ResetSpec::At {
        date, hour, minute, ..
    } = &spec
    else {
        return reset_in_zone(&spec, earliest, i64::MAX, local_at).filter(|d| *d <= now);
    };
    let clock = i64::from(*hour) * 3600 + i64::from(*minute) * 60;
    let offset = |unix: i64| named.or_else(|| local_at(unix));
    match date {
        None => newest_instant_of_clock(clock, earliest, now, &offset),
        Some((month, day)) => {
            let today = now.checked_add(offset(now)?)?.div_euclid(86_400);
            let (year, ..) = civil(today);
            let named = [year.saturating_sub(1), year, year.saturating_add(1)]
                .map(|year| days_from_civil(year, *month, *day))
                .into_iter()
                .min_by_key(|named| named.abs_diff(today))?;
            newest_instant_on_day(named, clock, i64::MIN, now, &offset)
        }
    }
}

/// The newest instant after `earliest` and at or before `now` at which the local clock reads
/// `clock` seconds into its day. The reach is under 24 hours but can need three local days: a
/// change that skips a day's last hour, as Nuuk's does, leaves that clock on neither the day read
/// nor the one before.
fn newest_instant_of_clock(
    clock: i64,
    earliest: i64,
    now: i64,
    local_at: &dyn Fn(i64) -> Option<i64>,
) -> Option<i64> {
    let today = now.checked_add(local_at(now)?)?.div_euclid(86_400);
    [today, today.saturating_sub(1), today.saturating_sub(2)]
        .into_iter()
        .find_map(|day| newest_instant_on_day(day, clock, earliest, now, local_at))
}

/// [`instants_of_clock`], the newest of them after `earliest` and at or before `now`.
fn newest_instant_on_day(
    day: i64,
    clock: i64,
    earliest: i64,
    now: i64,
    local_at: &dyn Fn(i64) -> Option<i64>,
) -> Option<i64> {
    instants_of_clock(day, clock, local_at)
        .into_iter()
        .flatten()
        .filter(|at| *at > earliest && *at <= now)
        .max()
}

/// The instants at which the local clock reads `clock` seconds into `day`: the offset a day either
/// side of it, where that offset does hold at the instant it gives. Two for a clock a change
/// repeats, none for one a change skips, otherwise the one instant: from both sides far from a
/// change, from one side, the other `None`, on a change's day and the days beside it.
fn instants_of_clock(
    day: i64,
    clock: i64,
    local_at: &dyn Fn(i64) -> Option<i64>,
) -> [Option<i64>; 2] {
    let local = day.checked_mul(86_400).and_then(|d| d.checked_add(clock));
    let at_offset_near = |side: i64| {
        let local = local?;
        let offset = local_at(local.saturating_add(side))?;
        let at = local.checked_sub(offset)?;
        (local_at(at)? == offset).then_some(at)
    };
    [at_offset_near(-86_400), at_offset_near(86_400)]
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
    place_reset_local(
        spec,
        session_window,
        now,
        printed,
        &|_| Some(local_offset_s),
        zone_offset_at,
    )
}

/// [`place_reset_with`], reading the local zone at the candidate instant as well (`local_at`).
pub fn place_reset_local(
    spec: &ResetSpec,
    session_window: bool,
    now: i64,
    printed: Option<i64>,
    local_at: &dyn Fn(i64) -> Option<i64>,
    zone_offset_at: &dyn Fn(&str, i64) -> Option<i64>,
) -> i64 {
    match (spec, printed) {
        (ResetSpec::At { date: None, .. }, Some(printed)) => {
            let offset_at = offset_in(spec, local_at, zone_offset_at);
            match reset_in_zone(spec, printed, i64::MAX, &offset_at) {
                Some(at) if !session_window || at.saturating_sub(printed) <= SESSION_AHEAD_MAX => {
                    at
                }
                Some(_) => reset_at_local(spec, now, local_at, zone_offset_at),
                None => now,
            }
        }
        _ => reset_at_local(spec, now, local_at, zone_offset_at),
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
    aterm_uds::zone::local_offset_now().unwrap_or(0)
}

/// UTC offset in seconds for IANA `zone` at Unix timestamp `unix`.
pub fn zone_offset_at(zone: &str, unix: i64) -> Option<i64> {
    offset_at(Some(zone), unix)
}

/// UTC offset in seconds for `zone` (or local time when `None`) at Unix timestamp `unix`.
pub fn offset_at(zone: Option<&str>, unix: i64) -> Option<i64> {
    read_offset(zone, unix)
}

/// [`offset_at`]'s look at the machine, the one `fn` here that reads a zone (the
/// census's `supervise-fx` home for it).
fn read_offset(zone: Option<&str>, unix: i64) -> Option<i64> {
    match zone {
        Some(zone) => aterm_uds::zone::zone_offset_at(zone, unix),
        None => aterm_uds::zone::local_offset_at(unix),
    }
}

/// Local timezone offset from UTC in seconds at Unix timestamp `unix`.
pub fn local_offset_at(unix: i64) -> Option<i64> {
    offset_at(None, unix)
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
#[path = "limit_tests.rs"]
mod tests;
