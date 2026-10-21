// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The usage-limit episode's clock: when the notice says the limit resets, as a Unix time, so
//! the turn-end policy can continue the worker 150 s past it (`turn_end_loop.rs`'s
//! `wall_reset_at`) and the loop can stretch its budget past it (`run.rs`'s
//! `Session::extend`). The band's countdown reads the same grammar (aterm-gui
//! `presence::countdown_to_reset`).
//!
//! Spellings after `resets ` / `reset at ` (as `aterm_phase::limit_notice` hands them over):
//!
//! * a clock time, with or without a month and day and a zone
//!   (`Sep 19 at 11am (America/Los_Angeles)`, `3am`; [`ResetSpec::At`]);
//! * a span (`in 2h 30m`, `in 3 hours`; [`ResetSpec::In`]), counted from the loop's first
//!   read, once per episode;
//! * the auto-continue notice's time or word (`⚠ Usage limit reached · continuing automatically
//!   at 1:50pm · esc to cancel`, then `continuing shortly`): a bare clock time (today's or
//!   tomorrow's), or `shortly`, read as a minute from now ([`SHORTLY`]) so Claude Code's own
//!   continuation goes first. Such a notice is over once the worker is read busy
//!   ([`resumes_by_itself`]); its row stays on screen while it works.
//!
//! The auto-continue notice (`⚠ Usage limit reached · continuing
//! automatically at 1:50pm · esc to cancel`, then `continuing shortly`) names
//! when Claude Code goes on by itself, the same clock: `aterm_phase` hands over
//! the time after `continuing automatically at ` (a bare clock time: today's,
//! or tomorrow's) or `shortly`, read as a minute from now ([`SHORTLY`]), so
//! Claude Code's own continuation goes first. The phrases themselves parse
//! too. Such a notice is over once the worker is read busy
//! ([`resumes_by_itself`]); the row stays on screen while it works.
//!
//! The zone's offset comes from the caller. [`reset_at_with`] and
//! [`place_reset_with`] (the supervisor's) take a reader of it AT an instant, in
//! production [`zone_offset_at`], which asks `date` under `TZ=<zone>`: a reset
//! that falls across a daylight-saving change is placed where the zone's clock
//! shows it, not an hour off by today's offset. [`reset_at`] and
//! [`place_reset`] keep the offset of ONE instant (`zone_offset(zone)`: today's,
//! [`zone_offset_at`] at the caller's now), right unless such a change falls
//! between now and the reset. Either way only for a zone `/usr/share/zoneinfo`
//! has, since an unknown `TZ` reads as UTC in silence (measured:
//! `TZ=Nonsense/Zone date +%z` prints `+0000`). A zone the machine does not
//! know, or none named, is the local zone: Claude Code prints the notice in the
//! user's own, at the one offset the caller read for it.

use std::path::Path;
use std::time::Duration;

/// When a limit notice says it resets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResetSpec {
    /// A clock time: `Sep 19 at 11am (America/Los_Angeles)`, `7:30pm`.
    At {
        /// The month and day, when the notice names them (a weekly limit).
        date: Option<(u32, u32)>,
        hour: u32,
        minute: u32,
        /// The zone in parentheses, as printed.
        zone: Option<String>,
    },
    /// A span from the notice's print, read as from now: `in 3h` (see the
    /// module doc).
    In(Duration),
}

/// A clock time with no date that passed less than this long ago is today's, passed (the
/// notice sat unread across it); older, it is tomorrow's. A session limit resets within its
/// five-hour window, so tomorrow's is right only once today's is 19 hours gone (24 − 5): `3am`
/// read at 3:30pm is 12.5 hours passed, not 11.5 ahead.
const SAME_DAY: i64 = 19 * 3600;

/// The furthest ahead a session limit's bare clock time can be, in real
/// time: its five-hour window (24 h − [`SAME_DAY`]). [`reset_in_zone`]
/// takes it as `ahead_max`; `i64::MAX` there reads a bare clock time as its
/// next occurrence instead (a weekly reset under a day away, printed with no
/// date).
pub const SESSION_AHEAD_MAX: i64 = 86_400 - SAME_DAY;

/// What `continuing shortly` is read as: a minute from the read. Claude Code
/// says it when its retry is imminent; by then the worker is read working
/// (the busy read closes the episode) or the notice is still up (the
/// turn-end policy's back-off takes over).
pub const SHORTLY: Duration = Duration::from_secs(60);

/// Read the reset out of the notice's text after `resets ` / `reset at ` —
/// or the auto-continue notice's time (`continuing automatically at 1:50pm`,
/// or `1:50pm` as `aterm_phase` hands it over) or word (`continuing
/// shortly`, `shortly`: [`SHORTLY`] from now).
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
            // A reset in another calendar year names it (`Jan 2, 2027 at
            // 9am`): the year is not the clock. `reset_at` already puts the
            // date in the year nearest now.
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

/// A LIMIT NOTICE'S RESET AS ITS CLOCK SAYS IT — the date it names, the hour
/// and the minute — what tells one limit's notice from another's: the one a
/// supervisor waits on from an older one a `/resume`d conversation redraws
/// (`policy/turn_end.rs`'s fold of [`aterm_phase::SinceLimit`]). The zone is
/// left out (a notice row cut at its wrap drops it), and so is a span (`in
/// 3h` counts from a print nobody kept): a reset that names no clock tells
/// no limit apart, and [`Self::of`] reads `None` for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetClock {
    date: Option<(u32, u32)>,
    hour: u32,
    minute: u32,
}

impl ResetClock {
    /// The clock a notice's reset text (`10:38am (America/Los_Angeles)`,
    /// `Oct 20 at 9am`, the auto-continue's `continuing automatically at
    /// 10:38am`) names; `None` for none, a span or `shortly`.
    #[must_use]
    pub fn of(reset: Option<&str>) -> Option<Self> {
        match parse_reset(reset?)? {
            ResetSpec::At {
                date, hour, minute, ..
            } => Some(Self { date, hour, minute }),
            ResetSpec::In(_) => None,
        }
    }

    /// The reset as a person reads it, as the vendor's notice names it less
    /// its zone (the zone the notice is drawn in): `on Oct 23 at 9am`, `on
    /// Oct 23 at 9:30pm`, or `at 9am` for a clock that names no date — what
    /// a usage limit's notice tells the owner the tab continues after
    /// ([`super::policy::notice::NoticeText::expect_reset`]).
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

/// Whether the notice says Claude Code goes on by itself (`continuing automatically at …`,
/// `continuing shortly`), so the worker read busy after it is the continuation and the episode
/// is over, notice row on screen or not. A notice naming a reset (`resets Sep 19 at 11am`) says
/// no such thing: a busy spell after it is someone's turn, which may hit the wall again, and
/// the episode ends when the worker answers on a point that is not the notice.
pub fn resumes_by_itself(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains(aterm_phase::anchor("wall.auto_continue"))
        || lower.contains("continuing shortly")
        // The moment the vendor fires it (2.1.284).
        || lower.replace('\u{2219}', "\u{b7}").contains("available again \u{b7} continuing now")
        // The /goal echo of a wall whose auto-continue is armed (`Goal paused · usage limit
        // reached · continues automatically when it resets`).
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
        // The unit is the letter that follows the number, spaces skipped.
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

/// The reset as Unix seconds, placed by [`reset_in_zone`]'s rule for a session limit with one
/// offset: `zone_offset(zone)` seconds east of UTC, or `local_offset_s` when the notice named no
/// zone or the machine does not know it. A dated time takes the year that puts it nearest `now`
/// (`Jan 2` said on Dec 30 is three days ahead); a time in the past is returned as is (the limit
/// reset while the notice sat, and the continuation goes at once).
pub fn reset_at(
    spec: &ResetSpec,
    now: i64,
    local_offset_s: i64,
    zone_offset: fn(&str) -> Option<i64>,
) -> i64 {
    reset_at_with(spec, now, local_offset_s, &|zone, _| zone_offset(zone))
}

/// [`reset_at`] with the zone's offset read at the instant placed (`zone_offset_at(zone,
/// unix)`, [`zone_offset_at`] in production), so a reset across a daylight-saving change lands
/// where the zone's clock shows it: read on Oct 30 at 12:00 PDT, `Nov 2 at 9am
/// (America/Los_Angeles)` is 17:00Z (PST), where [`reset_at`] says 16:00Z.
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

/// The reset as Unix seconds, placed in real time: `offset_at(t)` is the
/// notice's zone's offset from UTC at instant `t`, and each candidate day is
/// placed with the offset that holds on it. A span is added to `now`; a dated
/// clock time is the nearest of last, this and next year's; a bare clock time
/// is today's while ahead, else tomorrow's when that is less than `ahead_max`
/// seconds ahead ([`SESSION_AHEAD_MAX`] for a session limit), else today's,
/// passed. `None` when an offset cannot be read.
///
/// With a constant offset and `ahead_max = SESSION_AHEAD_MAX` this is exactly
/// [`reset_at`]'s rule.
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
    // The instant the zone's clock reads `clock` on civil day `day`: placed
    // with the offset at a first guess, then with the offset there.
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

/// How long before the loop's first look a wall it saw arrive (the watched turn ended on it),
/// whose done row says no more, may have been drawn: a minute, the done row clock's resolution
/// and the loop's lag. A longer lag would put a weekly wall printed just after the clock it
/// names (its reset a day off) in the past.
pub const PRINT_LAG_S: i64 = 60;

/// A clock that passed this long before the print is read as passed, not tomorrow's. The
/// vendor does not print a reset already gone, but the machine's and the server's clocks
/// differ, and a wrong "tomorrow" is a day's stall where a wrong "passed" is one rejected try.
/// Five minutes, plus the done row's minute.
pub const CLOCK_SLACK_S: i64 = 6 * 60;

/// A bare clock time kept within a session window: placed at `at` more than
/// [`SESSION_AHEAD_MAX`] after `now`, it is the day before's, passed; anything else is `at`.
/// Used for a cancelled limit read as a turn's answer (`Session::wall_reset_at`): a `/resume`d
/// conversation's older cancelled limit draws the same, and read as tomorrow's it would cost a
/// day's wait; a wrong "passed" costs one try.
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

/// Whether a wall's notice names a session window (`session limit`, `5-hour limit`), so the
/// session rule places its bare clock. A `Usage limit reached · …` that names no window may be
/// a weekly one.
#[must_use]
pub fn names_a_session_window(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("session limit") || lower.contains("5-hour")
}

/// When the wall on `rows` was printed, less [`CLOCK_SLACK_S`], for [`place_reset`]: the done
/// row's clock (`done 7:58 PM`, Claude Code's stamp of when the turn ended), its last
/// occurrence within a day, at the start of its minute; else, for a wall the loop saw arrive
/// (`live`), [`PRINT_LAG_S`] ago; else `None`, and the session rule places it.
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

/// WHEN THE TURN ON `rows` ENDED by its done row's clock (`done 7:58 PM`,
/// Claude Code's own stamp, [`super::policy::turn_end::done_row_clock`]):
/// the start of that minute (unix seconds), its last occurrence within a day
/// up to `now`. `None` without a done row's clock.
#[must_use]
pub fn done_row_ended(rows: &[String], now: i64, local_offset_s: i64) -> Option<i64> {
    let earliest = now - 86_400 + 60;
    super::policy::turn_end::done_row_clock(rows)
        .and_then(|c| parse_reset(&c))
        .and_then(|d| reset_in_zone(&d, earliest, i64::MAX, &|_| Some(local_offset_s)))
        .filter(|d| *d <= now)
}

/// Where a wall's reset falls. `harness limits` places it by the zone's offset today, the
/// loop's wait by the offset at the instant placed ([`place_reset_with`]).
///
/// Claude Code prints a bare clock time only for a reset within 24 hours of the print and the
/// date beyond that (2.1.283/284), so a bare clock time is the first occurrence of that clock
/// strictly after the print (`printed`, Unix seconds). With no evidence of the print (`None`:
/// a wall found sitting, no done row) the session rule stands ([`reset_at`]): right whenever
/// the next occurrence is under five hours off, else at worst one blind try early, which the
/// wall answers with a print the loop sees (`Session::wall_reset_at`). A span and a dated
/// time are placed as [`reset_at`] places them; a time in the past is returned as is.
///
/// A session window (`session_window`) is placed by the print too, so a stale session wall
/// found sitting reads as passed rather than tonight's, but only where the clock falls within
/// [`SESSION_AHEAD_MAX`] of it; a clock further off contradicts the window (a stale row, a
/// misread) and the session rule places it.
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

/// [`place_reset`] with the zone's offset read at the instant placed for, as
/// [`reset_at_with`] reads it.
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

/// Days since 1970-01-01 → (year, month, day): `aterm_types::rfc3339`'s
/// calendar, the workspace's one copy, in this module's field types.
pub(super) fn civil(days: i64) -> (i64, u32, u32) {
    let (y, m, d) = aterm_types::rfc3339::civil_from_days(days);
    (
        y,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

/// (year, month, day) → days since 1970-01-01, the inverse of [`civil`].
pub(super) fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    aterm_types::rfc3339::days_from_civil(y, i64::from(m), i64::from(d))
}

/// Unix seconds now.
pub fn unix_now() -> i64 {
    aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX))
}

/// Unix milliseconds now.
pub fn unix_now_ms() -> i64 {
    aterm_time::unix_millis().map_or(0, |ms| i64::try_from(ms).unwrap_or(i64::MAX))
}

/// The local time's offset from UTC in seconds, from `date +%z` (UTC when it cannot be run).
/// Through `aterm_uds::spawnfd::command`: on Windows a `date` on `PATH` (Git for Windows, MSYS)
/// is a console image, which would flash a console from the window's harness host.
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

/// A named zone's offset from UTC AT the instant `unix` ([`offset_at`], for a
/// zone `/usr/share/zoneinfo` has): the reader [`reset_at_with`] and
/// [`place_reset_with`] take. `None` for a zone it has not, or a name with
/// characters no zone has. The caller hands the instant in — the supervising
/// loop from its clock door (`supervise::fx`) — so there is no "today's
/// offset" reader beside it: one would read the system clock behind the door.
pub fn zone_offset_at(zone: &str, unix: i64) -> Option<i64> {
    offset_at(Some(zone), unix)
}

/// A zone's offset from UTC at the instant `unix`, in seconds (the offset
/// daylight saving gave that instant): `date -r <unix> +%z` (BSD) /
/// `date -d @<unix> +%z` (GNU). `zone` names the zone (`TZ=<zone>`, only one
/// `/usr/share/zoneinfo` has); `None` is the local clock. The
/// workspace's one reader of an offset at an instant (aterm-gui's
/// `presence::local_offset_at` is this with no zone). A subprocess per call,
/// so hot callers cache. `None` where `date` cannot say, or off Unix.
pub fn offset_at(zone: Option<&str>, unix: i64) -> Option<i64> {
    if zone.is_some_and(|z| !zone_known(z)) {
        return None;
    }
    date_offset(zone.map_or(DateZone::Inherited, DateZone::Named), unix)
}

/// The local clock's offset from UTC at `unix` ([`offset_at`] with no zone):
/// the supervising loop's reader of a day the vendor draws with no zone,
/// which the loop asks through its door (`supervise::fx`).
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

/// `+hhmm` / `-hh:mm` as seconds.
pub fn parse_zone(text: &str) -> Option<i64> {
    let (negative, rest) = match text.strip_prefix('+') {
        Some(rest) => (false, rest),
        None => (true, text.strip_prefix('-')?),
    };
    let digits: String = rest.chars().filter(char::is_ascii_digit).collect();
    if digits.len() != 4 {
        return None;
    }
    // Two ASCII digits each: `u8` fields, so the sum below is bounded by type.
    let h: u8 = digits.get(..2)?.parse().ok()?;
    let m: u8 = digits.get(2..)?.parse().ok()?;
    let v = i64::from(h) * 3600 + i64::from(m) * 60;
    Some(if negative { -v } else { v })
}

/// Whether `c` would break a payload that promises to be one line: a control
/// character, or `U+2028`/`U+2029` (line and paragraph separators, which
/// `char::is_control` misses). [`one_line`] folds what it names to a space.
#[must_use]
pub fn breaks_a_line(c: char) -> bool {
    c.is_control() || c == '\u{2028}' || c == '\u{2029}'
}

/// A rules file as one turn's text: every line break and control character a
/// space, runs of spaces one, the ends trimmed (the wire frames a request per
/// line, and Claude Code submits on Enter).
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
    /// date, midnight and noon on the twelve. NEGATIVE CONTROL: a span names
    /// no clock, so it has no words to say.
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
        // Read at 2026-12-30 12:00 UTC, it is next year's Jan 2.
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
        // `Your limit will reset at 3pm (America/New_York).` hands over
        // `3pm (America/New_York)`; a trailing period is tolerated.
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
    /// clock time, and `continuing shortly` (or `shortly`) a minute from now. Whether a notice
    /// resumes by itself is read from its words, not its time.
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
        // NOW is 08:55 local: 1:50pm is ahead today, 4 h 55 min.
        assert_eq!(at("1:50pm") - NOW, (4 * 60 + 55) * 60);
        assert_eq!(at("continuing automatically at 1:50pm"), at("1:50pm"));
        // Read 18 s after 1:50pm it is today's, passed: the continuation goes at once.
        let read_at = NOW + (4 * 60 + 55) * 60 + 18;
        assert_eq!(read_at - reset_at(&one_fifty, read_at, PDT, zones), 18);
        // `3am` read at 23:00 is tomorrow's, four hours ahead; `11:05 am` read
        // at 08:55 is today's, 2 h 10 min ahead.
        let late = NOW + 14 * 3600 + 5 * 60;
        let three = parse_reset("continuing automatically at 3am").unwrap();
        assert_eq!(reset_at(&three, late, PDT, zones) - late, 4 * 3600);
        assert_eq!(
            at("continuing automatically at 11:05 am") - NOW,
            (2 * 60 + 10) * 60
        );
        // A minute from now, whichever spelling.
        assert_eq!(at("shortly"), NOW + 60);
        assert_eq!(at("continuing shortly"), NOW + 60);
        // This notice says the worker goes on by itself; the others do not.
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
    /// continuation. The echo that only asks for a message is not.
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
        // 2026-09-19T11:00:00-07:00.
        let sep_19 = 1_789_840_800;
        assert_eq!(at("Sep 19 at 11am (America/Los_Angeles)"), sep_19);
        // The same instant in New York: 2pm there.
        assert_eq!(at("Sep 19 at 2pm (America/New_York)"), sep_19);
        // No zone, or one the machine does not know: the local one.
        assert_eq!(at("Sep 19 at 11am"), sep_19);
        assert_eq!(at("Sep 19 at 11am (Mars/Olympus)"), sep_19);
        // Two days ahead of NOW.
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
        // `Jan 2 at 3am` said in September is next January's.
        let jan_2 = at("Jan 2 at 3am (America/Los_Angeles)");
        assert!(jan_2 > NOW);
        assert_eq!(civil((jan_2 + PDT).div_euclid(86_400)), (2027, 1, 2));
        // `Dec 31 at 11am` read on 2027-01-02 at 16:55 UTC passed two days
        // ago: last December's, not next December's 362 days off.
        let jan_2 = days_from_civil(2027, 1, 2) * 86_400 + 16 * 3600 + 55 * 60;
        let dec_31 = reset_at(
            &parse_reset("Dec 31 at 11am (UTC)").unwrap(),
            jan_2,
            0,
            zones,
        );
        assert_eq!(jan_2 - dec_31, 2 * 86_400 + 5 * 3600 + 55 * 60);
        assert_eq!(civil(dec_31.div_euclid(86_400)), (2026, 12, 31));
        // A span is from now.
        assert_eq!(at("in 3h"), NOW + 3 * 3600);
    }

    /// A clock time with no date: today's when ahead or passed at most 19 hours ago
    /// ([`SAME_DAY`]), else tomorrow's; midnight is `12am`.
    #[test]
    fn a_bare_time_is_todays_or_tomorrows_and_midnight_is_12am() {
        // NOW is 08:55 local. 7:30pm is ahead today.
        let t = at("7:30pm (America/Los_Angeles)");
        assert_eq!(t - NOW, (19 * 60 + 30 - (8 * 60 + 55)) * 60);
        // 3am passed 5h55m ago: passed.
        let t = at("3am");
        assert_eq!(NOW - t, (5 * 60 + 55) * 60);
        // 8:50am was 5 min ago: passed.
        assert_eq!(NOW - at("8:50am"), 5 * 60);
        // 12am (midnight) was 8h55m ago: passed, today's.
        assert_eq!(NOW - at("12am"), (8 * 60 + 55) * 60);
        // 12pm is noon, ahead.
        assert_eq!(at("12pm") - NOW, (3 * 60 + 5) * 60);
        // A session limit names a time at most five hours ahead: `3am` read at 15:30 (a watcher
        // started onto a stale screen) is 12.5 hours passed, not 11.5 ahead; read at 23:00 it
        // is tomorrow's, four hours ahead.
        let half_past_three = NOW + (6 * 60 + 35) * 60; // 15:30 local
        let t = reset_at(&parse_reset("3am").unwrap(), half_past_three, PDT, zones);
        assert_eq!(half_past_three - t, (12 * 60 + 30) * 60);
        let late = NOW + 14 * 3600 + 5 * 60; // 23:00 local
        let t = reset_at(&parse_reset("3am").unwrap(), late, PDT, zones);
        assert_eq!(t - late, 4 * 3600);
        // The edge: at 22:00 `3am` is 19 h gone, still today's; a minute
        // later it is tomorrow's, 4 h 59 m ahead.
        let ten_pm = NOW + (13 * 60 + 5) * 60; // 22:00 local
        let three = parse_reset("3am").unwrap();
        assert_eq!(ten_pm - reset_at(&three, ten_pm, PDT, zones), 19 * 3600);
        assert_eq!(
            reset_at(&three, ten_pm + 60, PDT, zones) - (ten_pm + 60),
            5 * 3600 - 60
        );
        // 24-hour spelling.
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
    /// clock shows it then, and the five-hour rule counts real hours. Spring
    /// forward: `4am` written at 22:30 PST on Mar 7 is 4am PDT on Mar 8
    /// (11:00Z), four and a half hours ahead. Fall back: `3am` written at 23:30
    /// PDT on Oct 31 is 3am PST (11:00Z), not 10:00Z.
    #[test]
    fn a_reset_across_a_dst_jump_is_placed_in_real_time() {
        let four = parse_reset("4am (America/Los_Angeles)").unwrap();
        let spring = 1_772_951_400; // 2026-03-08T06:30Z
        assert_eq!(
            reset_in_zone(&four, spring, SESSION_AHEAD_MAX, &la_at),
            Some(1_772_967_600)
        );
        let three = parse_reset("3am (America/Los_Angeles)").unwrap();
        let fall = 1_793_514_600; // 2026-11-01T06:30Z
        assert_eq!(
            reset_in_zone(&three, fall, SESSION_AHEAD_MAX, &la_at),
            Some(1_793_530_800)
        );
        // A dated reset on the far side of the jump, too.
        let dated = parse_reset("Mar 8 at 9am (America/Los_Angeles)").unwrap();
        assert_eq!(
            reset_in_zone(&dated, spring, SESSION_AHEAD_MAX, &la_at),
            Some(1_772_985_600)
        );
        // `ahead_max = i64::MAX` is the next occurrence: `9am` written at
        // 14:00 is tomorrow's.
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
        // An offset that cannot be read places nothing.
        assert_eq!(reset_in_zone(&nine, day, i64::MAX, &|_| None), None);
    }

    /// `date` answers a zone's offset at an instant, where the machine has
    /// the zone; a name no zone has, or a path, is asked nothing. Unix only:
    /// off Unix [`offset_at`] reads no offset at all.
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
        // Whatever this machine's zone is, it reads as a whole number of
        // minutes within a day.
        assert!(tz_offset_s().abs() < 86_400);
        // A zone no machine has, and a path that is not a zone name, at any
        // instant.
        for unix in [0, 1_790_000_000] {
            assert_eq!(zone_offset_at("Mars/Olympus", unix), None);
            assert_eq!(zone_offset_at("../etc/passwd", unix), None);
        }
    }
    /// With the print known, a wall's bare clock time is the first occurrence of that clock
    /// strictly after it, checked against a brute-force scan for every print over two days and
    /// every clock on the hour and half hour; a session window's is too while within
    /// [`SESSION_AHEAD_MAX`] of the print, else [`reset_at`]'s. With no print the session rule
    /// stands. Control: [`reset_at`] disagrees with the scan wherever the next occurrence is
    /// five hours or more off.
    #[test]
    fn a_walls_bare_clock_is_the_first_occurrence_after_its_print() {
        fn zone(_: &str) -> Option<i64> {
            Some(-7 * 3600)
        }
        let local = -7 * 3600;
        // 2026-09-17 00:00 PDT.
        let day0 = 1_789_628_400_i64;
        // The oracle walks forward from the print a second at a time up to the first whole
        // local minute, then a minute at a time: every clock swept is on the minute, and so is
        // every instant whose local time of day equals one, so the skipped seconds never match.
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
                    // A session window: placed by the print where the clock is within five
                    // hours of it, else by the five-hour rule.
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
                    // No evidence of the print: the session rule.
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
        // A span and a dated time are never moved by the print.
        let span = ResetSpec::In(Duration::from_secs(3 * 3600));
        assert_eq!(
            place_reset(&span, false, day0, Some(day0 - 86_400), local, zone),
            day0 + 3 * 3600
        );
    }
}
