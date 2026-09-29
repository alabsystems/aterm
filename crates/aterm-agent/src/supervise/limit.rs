// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The usage-limit episode's clock: when the notice says the limit resets,
//! read as a Unix time, so the turn-end policy can continue the worker a
//! minute past it (`turn_end_loop.rs`'s `wall_reset_at`) and the loop can
//! stretch its budget past it (`run.rs`'s `Session::extend`). The band's
//! countdown reads the same grammar (aterm-gui `presence::countdown_to_reset`).
//!
//! Measured 2026-09-15 16:51 → 2026-09-17 08:55: the manager's and the
//! worker's Claude Code drew on ONE account, its weekly limit hit both at
//! once, and the watcher printed `EVENT limited … reset=Sep 19 at 11am
//! (America/Los_Angeles)`, hit its own `--max-s` and exited. Nobody could act
//! for two days; the worker answered a one-line message in 23 s once the
//! owner was back. The reset was on the screen the whole time — this module
//! reads it.
//!
//! Two spellings, as Claude Code prints them after `resets ` / `reset at `
//! (`aterm_phase::limit_notice` hands over the text after that word):
//!
//! * `Sep 19 at 11am (America/Los_Angeles)`, `7:30pm (America/Los_Angeles)`,
//!   `3am`, `12:05 pm (UTC)` — a clock time, with or without a month and day,
//!   with or without a zone in parentheses ([`ResetSpec::At`]);
//! * `in 3h`, `in 2h 30m`, `in 45m`, `in 3 hours` — a span
//!   ([`ResetSpec::In`]), counted from the notice's print. The loop can
//!   only count it from its first read — late by however long the notice
//!   sat before the loop read it — and counts it ONCE an episode (`run.rs`'s
//!   `Session::refresh_reset`): the same text read again is the same reset,
//!   not one a span later.
//!
//! A third, the auto-continue notice (measured 2026-09-17 13:50: `⚠ Usage
//! limit reached · continuing automatically at 1:50pm · esc to cancel`,
//! then `continuing shortly`), names no reset; it says when Claude Code goes
//! on BY ITSELF, which is the same clock: `aterm_phase` hands over the time
//! after `continuing automatically at ` (`1:50pm`, a bare clock time: today's,
//! or tomorrow's by the rule above) or the word `shortly`, read as a minute
//! from now ([`SHORTLY`]) — so the budget stretches past it as for any reset,
//! and Claude Code's own continuation goes first. The phrases themselves
//! parse too. Such a notice is over the moment the worker is read busy
//! ([`resumes_by_itself`]): the continuation IS the worker working, and the
//! row stays on the screen while it does.
//!
//! The zone's offset comes from the caller ([`reset_at`]'s `zone_offset`):
//! in production [`zone_offset_s`], which asks `date` under `TZ=<zone>` for
//! the zone's offset TODAY — right for a reset within the week unless a DST
//! change falls between now and it (then an hour off; the continuation that
//! follows finds the wall again and waits again) — and only for a zone
//! `/usr/share/zoneinfo` has, since an unknown `TZ` reads as UTC in silence
//! (measured: `TZ=Nonsense/Zone date +%z` prints `+0000`). A zone the machine
//! does not know, or none named, is the local zone: Claude Code prints the
//! notice in the user's own. A caller that must be right across a DST change
//! (the Claude Code footer's limit wall) places with [`reset_in_zone`] and
//! [`offset_at`], the one reader of a zone's offset AT an instant.

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

/// A clock time with no date that passed less than this long ago is today's,
/// passed (the notice sat unread across it); older, it is tomorrow's. A
/// session limit resets within its five-hour window, so the time a notice
/// names is never more than five hours ahead: tomorrow's is right only once
/// today's is 19 hours gone (24 − 5), and `3am` read at 3:30pm is twelve and
/// a half hours passed, not eleven and a half ahead.
const SAME_DAY: i64 = 19 * 3600;

/// The furthest ahead a session limit's bare clock time can be, in REAL
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
            // 9am`, MEASURED: 2.1.283's formatter adds the year when it
            // differs): the year is not the clock. `reset_at` already puts
            // the date in the year nearest now.
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

/// Whether the notice says Claude Code goes on by itself — `continuing
/// automatically at …` / `continuing shortly` — so that the worker read
/// BUSY after it is the continuation, and the episode is over then, the
/// notice row on the screen or not. A notice naming a reset (`resets Sep 19
/// at 11am`) says no such thing: a busy spell after it is someone's turn
/// (the manager's retry, a human's, the continuation), which may hit the wall
/// again, and the episode ends when the worker answers on a point that is
/// not the notice.
pub fn resumes_by_itself(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains(aterm_phase::anchor("wall.auto_continue"))
        || lower.contains("continuing shortly")
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

/// The reset as Unix seconds, from the spec and `now`: a span is added to
/// `now`; a clock time is placed in its zone (`zone_offset(zone)` seconds
/// east of UTC, `local_offset_s` when the notice named none or the machine
/// does not know it) on the date named, in whichever of last year, this
/// year and next puts it nearest `now` (`Jan 2` said on Dec 30 is three
/// days ahead, `Dec 31` read on Jan 2 two days behind — never a year off
/// either way), and with no date, today, or tomorrow when today's is more
/// than [`SAME_DAY`] gone. A time in the past is returned as it is: the
/// limit reset while the notice sat, and the continuation goes at once.
pub fn reset_at(
    spec: &ResetSpec,
    now: i64,
    local_offset_s: i64,
    zone_offset: fn(&str) -> Option<i64>,
) -> i64 {
    let offset = match spec {
        ResetSpec::At { zone, .. } => zone
            .as_deref()
            .and_then(zone_offset)
            .unwrap_or(local_offset_s),
        ResetSpec::In(_) => 0,
    };
    reset_in_zone(spec, now, SESSION_AHEAD_MAX, &|_| Some(offset)).unwrap_or(now)
}

/// The reset as Unix seconds, placed in REAL time: `offset_at(t)` is the
/// notice's zone's offset from UTC at the instant `t` (daylight saving's
/// offset THEN, not today's), and a clock time lands where that zone's clock
/// shows it — each candidate day placed with the offset that holds on it. A
/// span is added to `now`; a dated clock time is the nearest of last year's,
/// this year's and next year's; a bare clock time is today's while it is
/// ahead, else tomorrow's when tomorrow's is less than `ahead_max` seconds
/// ahead in real time ([`SESSION_AHEAD_MAX`] for a session limit — so
/// across a daylight-saving jump a `4am` five hours ahead is still
/// tomorrow's), else today's, passed. `None` when an offset cannot be read.
///
/// With a constant offset, `ahead_max = SESSION_AHEAD_MAX` is exactly
/// [`reset_at`]'s rule (it delegates here): tomorrow's is under five hours
/// ahead exactly when today's is more than [`SAME_DAY`] gone.
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
    // with the offset at a first guess, then with the offset THERE.
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
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The local time's offset from UTC in seconds, from `date +%z` (the one
/// place a zone is read; UTC when it cannot be run).
pub fn tz_offset_s() -> i64 {
    let out = std::process::Command::new("date").arg("+%z").output().ok();
    let text = out
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    parse_zone(&text).unwrap_or(0)
}

/// A named zone's offset from UTC today, in seconds: `date +%z` under
/// `TZ=<zone>`, asked only for a zone `/usr/share/zoneinfo` has (an unknown
/// `TZ` reads as UTC in silence). `None` for one it has not, or a name with
/// characters no zone has.
pub fn zone_offset_s(zone: &str) -> Option<i64> {
    offset_at(Some(zone), unix_now())
}

/// A zone's offset from UTC AT the instant `unix`, in seconds — the offset
/// daylight saving gave that instant, not today's: `date -r <unix> +%z`
/// (BSD) / `date -d @<unix> +%z` (GNU). `zone` names the zone (`TZ=<zone>`,
/// asked only for one [`zone_offset_s`] would ask about); `None` is the
/// local clock's own. The workspace's ONE reader of an offset at an instant
/// (aterm-gui's `presence::local_offset_at` is this with no zone). A
/// subprocess per call: a caller on a hot path caches. `None` where `date`
/// cannot say, or off Unix.
pub fn offset_at(zone: Option<&str>, unix: i64) -> Option<i64> {
    if !cfg!(unix) || zone.is_some_and(|z| !zone_known(z)) {
        return None;
    }
    let mut date = std::process::Command::new("date");
    if let Some(zone) = zone {
        date.env("TZ", zone);
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
    let out = date
        .arg("+%z")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
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

/// Whether `c` would break a payload that promises to be ONE LINE.
///
/// `char::is_control()` alone is NOT that question and it is the trap every
/// hand-rolled sweep in this tree has fallen into: it is Cc-only
/// (`U+0000..=U+001F`, `U+007F..=U+009F`), so `U+2028` LINE SEPARATOR and
/// `U+2029` PARAGRAPH SEPARATOR — which are Zl and Zp, and which a terminal,
/// a JSON reader and an editor all treat as line breaks — pass straight
/// through. This predicate is the one home of that rule; [`one_line`] folds
/// what it names to a space and the harness's one-line surfaces (the
/// statusline, the usage HUD, the alignment sidecar's wire word) drop or fold
/// it the same way rather than each asking the wrong question.
#[must_use]
pub fn breaks_a_line(c: char) -> bool {
    c.is_control() || c == '\u{2028}' || c == '\u{2029}'
}

/// A rules file as ONE turn's text: every line break and control character a
/// space, runs of spaces one, the ends trimmed — the wire frames a request
/// per line, and Claude Code submits on Enter.
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

    /// `date +%z`'s answer, and the shapes near it: the sign is the first
    /// byte, exactly four digits follow (a `:` between them is tolerated),
    /// and anything else is refused rather than read.
    #[test]
    fn a_zone_is_a_sign_and_four_digits() {
        assert_eq!(parse_zone("+0130"), Some(5_400));
        assert_eq!(parse_zone("-0700"), Some(-25_200));
        assert_eq!(parse_zone("+07:00"), Some(25_200));
        assert_eq!(parse_zone("+0000"), Some(0));
        assert_eq!(parse_zone("0700"), None, "no sign");
        assert_eq!(parse_zone("+070"), None, "three digits");
        assert_eq!(parse_zone("x+0700"), None, "sign not first");
        assert_eq!(parse_zone("+1\u{e9}1"), None, "not four digits");
        assert_eq!(parse_zone(""), None);
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
    /// 2026-09-17T08:55:00-07:00 — when the owner logged back in.
    const NOW: i64 = 1_789_660_500;
    const PDT: i64 = -7 * 3600;

    fn at(text: &str) -> i64 {
        reset_at(&parse_reset(text).expect(text), NOW, PDT, zones)
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
        // Read on 2026-12-30 12:00 UTC, it is next year's Jan 2.
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

    /// The auto-continue notice's two shapes (measured 2026-09-17): the time
    /// after `continuing automatically at ` — as `aterm_phase` hands it over
    /// (`1:50pm`) and as the phrase — is a bare clock time, today's when
    /// ahead or just passed, tomorrow's once today's is long gone, am and pm
    /// alike; `continuing shortly` (and the `shortly` handed over) is a
    /// minute from now. The predicate that tells such a notice from one
    /// naming a reset reads the words, not the time.
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
        // Read at 13:50:18 — as the live notice was, the time it named 18 s
        // gone — it is today's, passed: the continuation goes at once.
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
        // The notice says the worker goes on by itself; the old ones do not.
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
        // ago — last December's, not next December's 362 days off (the
        // budget would have stretched to it).
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

    /// A clock time with no date: today's when ahead, or passed less than
    /// 12 hours ago; else tomorrow's — and midnight is `12am`.
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
        // A session limit names a time at most five hours ahead: `3am` read
        // at 15:30 (a watcher started onto a stale screen) is twelve and a
        // half hours passed — the continuation goes now — not eleven and a half
        // ahead; read at 23:00 it is tomorrow's, four hours ahead.
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

    /// THE PLACER BEFORE `reset_in_zone`, kept as the oracle: every caller
    /// of `reset_at` (the supervisor's clock, `harness limits`, the band)
    /// places exactly as it did, every five minutes of three days — the exact
    /// edges among them — and every spelling: dated, bare, zoned, span.
    #[test]
    fn reset_at_places_as_it_always_did() {
        fn before(spec: &ResetSpec, now: i64, local: i64) -> i64 {
            match spec {
                ResetSpec::In(span) => now + i64::try_from(span.as_secs()).unwrap(),
                ResetSpec::At {
                    date,
                    hour,
                    minute,
                    zone,
                } => {
                    let offset = zone.as_deref().and_then(zones).unwrap_or(local);
                    let (y, m, d) = civil((now + offset).div_euclid(86_400));
                    let clock = i64::from(*hour) * 3600 + i64::from(*minute) * 60;
                    match date {
                        Some((month, day)) => [y - 1, y, y + 1]
                            .into_iter()
                            .map(|year| {
                                days_from_civil(year, *month, *day) * 86_400 + clock - offset
                            })
                            .min_by_key(|at| (at - now).abs())
                            .unwrap(),
                        None => {
                            let today = days_from_civil(y, m, d) * 86_400 + clock - offset;
                            if today > now || now - today <= SAME_DAY {
                                today
                            } else {
                                today + 86_400
                            }
                        }
                    }
                }
            }
        }
        let texts = [
            "3am",
            "3pm (America/Los_Angeles)",
            "11:30pm (America/New_York)",
            "12am (UTC)",
            "Sep 19 at 11am (America/Los_Angeles)",
            "Jan 2 at 9am (UTC)",
            "Dec 31 at 11pm",
            "in 3h",
        ];
        for text in texts {
            let spec = parse_reset(text).expect(text);
            // Every five minutes — the rule's edges fall on the hour — and
            // a few seconds past each.
            for step in 0..(3 * 24 * 12 * 2) {
                let now = NOW + 5 * 60 + (step / 2) * 300 + (step % 2) * 17;
                for local in [PDT, 0, 5 * 3600 + 1800] {
                    assert_eq!(
                        reset_at(&spec, now, local, zones),
                        before(&spec, now, local),
                        "{text} at {now} local {local}"
                    );
                }
            }
        }
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

    /// ACROSS A DAYLIGHT-SAVING JUMP a bare clock time lands where the
    /// zone's clock shows it THEN, and the five-hour rule counts real hours.
    /// Spring forward: `4am` written at 22:30 PST on Mar 7 is 4am PDT on Mar
    /// 8 (11:00Z), four and a half hours ahead — not Mar 7's, 18.5 local
    /// hours back. Fall back: `3am` written at 23:30 PDT on Oct 31 is 3am PST
    /// (11:00Z), not 10:00Z.
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
        // A DATED reset on the far side of the jump, too.
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

    /// `date` answers a zone's offset AT an instant, where the machine has
    /// the zone; a name no zone has, or a path, is asked nothing.
    #[test]
    fn a_zones_offset_is_read_at_the_instant() {
        if !Path::new("/usr/share/zoneinfo/America/Los_Angeles").is_file() {
            return;
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
        ] {
            assert_eq!(parse_zone(text), want, "{text:?}");
        }
        // Whatever this machine's zone is, it reads as a whole number of
        // minutes within a day.
        assert!(tz_offset_s().abs() < 86_400);
        // A zone no machine has, and a path that is not a zone name.
        assert_eq!(zone_offset_s("Mars/Olympus"), None);
        assert_eq!(zone_offset_s("../etc/passwd"), None);
    }
}
