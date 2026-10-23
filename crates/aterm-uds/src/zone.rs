// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A zone's offset from UTC at an instant, read in this process: the offset daylight saving
//! gives that instant, in seconds east of UTC, with no `date +%z` child per question.
//!
//! * The local clock's ([`local_offset_at`](crate::zone::local_offset_at)) is `localtime_r`'s
//!   `tm_gmtoff`, after a `tzset` so a replaced system zone is followed (glibc keeps its first
//!   zone otherwise). This process's `TZ` wins, as it would for a `date` child.
//! * A named zone's ([`zone_offset_at`](crate::zone::zone_offset_at)) is read from its TZif file
//!   under `/usr/share/zoneinfo` (RFC 8536), the file `TZ=<zone> date` reads: the transition in
//!   force at the instant, and past the last transition the file's footer rule (a POSIX `TZ`
//!   string, [`posix_offset_at`](crate::zone::posix_offset_at)). No `TZ` is set: a process's
//!   `TZ` is shared by every thread, and setting it beside a reader is undefined.

use std::path::Path;

/// Where the system's zone files are: what `TZ=<zone>` names under.
pub const ZONEINFO: &str = "/usr/share/zoneinfo";

/// The local clock's offset from UTC at the instant `unix`, in seconds east of UTC:
/// `localtime_r`'s `tm_gmtoff` under this process's `TZ` (else the system zone), after
/// `tzset`. `None` where the C library cannot say, and off Unix.
#[must_use]
pub fn local_offset_at(unix: i64) -> Option<i64> {
    imp::local_offset_at(unix)
}

/// The local clock's offset from UTC now ([`local_offset_at`](crate::zone::local_offset_at)).
#[must_use]
pub fn local_offset_now() -> Option<i64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    local_offset_at(i64::try_from(now.as_secs()).ok()?)
}

/// Whether `zone` is a plain zone name (`America/New_York`, `Etc/GMT+5`, `UTC`): what
/// [`zone_offset_at`] reads a file under [`ZONEINFO`] for. Never a path out of it.
#[must_use]
pub fn plain_zone_name(zone: &str) -> bool {
    !zone.is_empty()
        && !zone.contains("..")
        && !zone.starts_with('/')
        && zone
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'))
}

/// The named zone's offset from UTC at the instant `unix`, in seconds east of UTC, from its
/// TZif file under [`ZONEINFO`] ([`tzif_offset_at`]). `None` for a name that is no plain
/// zone name ([`plain_zone_name`]), a zone the machine has no file for, or a file that does
/// not parse.
#[must_use]
pub fn zone_offset_at(zone: &str, unix: i64) -> Option<i64> {
    zone_offset_in(Path::new(ZONEINFO), zone, unix)
}

/// [`zone_offset_at`] with the zone files under `root`: a test's fixture tree.
#[must_use]
pub fn zone_offset_in(root: &Path, zone: &str, unix: i64) -> Option<i64> {
    if !plain_zone_name(zone) {
        return None;
    }
    // A zone file is a few KiB; anything past this is not one.
    const MOST: u64 = 1 << 20;
    let file = std::fs::File::open(root.join(zone)).ok()?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(file, MOST), &mut bytes).ok()?;
    tzif_offset_at(&bytes, unix)
}

/// A TZif file's (RFC 8536) offset from UTC at `unix`, in seconds east of UTC: the time type
/// of the last transition at or before `unix`; time type 0 before the first; and, from the
/// last transition on, the footer's rule where the file has one (version 2 and later). A
/// version 2+ file is read from its 64-bit block. `None` where the bytes are no TZif.
#[must_use]
pub fn tzif_offset_at(bytes: &[u8], unix: i64) -> Option<i64> {
    let first = Header::read(bytes)?;
    let (block, header, wide) = if first.version >= b'2' {
        let second = bytes.get(HEADER + first.block_len(4)?..)?;
        (second, Header::read(second)?, true)
    } else {
        (bytes, first, false)
    };
    let width = if wide { 8 } else { 4 };
    let data = block.get(HEADER..)?;
    let times_len = header.timecnt.checked_mul(width)?;
    let times = data.get(..times_len)?;
    let kinds = data.get(times_len..times_len.checked_add(header.timecnt)?)?;
    let types_at = times_len + header.timecnt;
    let types = data.get(types_at..types_at.checked_add(header.typecnt.checked_mul(6)?)?)?;
    if header.typecnt == 0 {
        return None;
    }
    let utoff = |kind: usize| -> Option<i64> {
        let row = types.get(kind * 6..kind * 6 + 4)?;
        Some(i64::from(i32::from_be_bytes(row.try_into().ok()?)))
    };
    let time = |i: usize| -> Option<i64> {
        let at = times.get(i * width..(i + 1) * width)?;
        Some(if wide {
            i64::from_be_bytes(at.try_into().ok()?)
        } else {
            i64::from(i32::from_be_bytes(at.try_into().ok()?))
        })
    };
    let footer = if wide {
        let after = HEADER + header.block_len(8)?;
        footer_of(block.get(after..)?)
    } else {
        None
    };
    let count = header.timecnt;
    if let Some(rule) = footer
        && (count == 0 || unix >= time(count - 1)?)
    {
        return posix_offset_at(rule, unix);
    }
    if count == 0 || unix < time(0)? {
        return utoff(0);
    }
    // The last transition at or before `unix`: the times ascend (RFC 8536 §3.2).
    let (mut lo, mut hi) = (0usize, count);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if time(mid)? <= unix {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let kind = usize::from(*kinds.get(lo)?);
    if kind >= header.typecnt {
        return None;
    }
    utoff(kind)
}

/// A TZif header's length.
const HEADER: usize = 44;

/// A TZif header's version and counts (RFC 8536 §3.1).
#[derive(Clone, Copy)]
struct Header {
    version: u8,
    isutcnt: usize,
    isstdcnt: usize,
    leapcnt: usize,
    timecnt: usize,
    typecnt: usize,
    charcnt: usize,
}

impl Header {
    fn read(bytes: &[u8]) -> Option<Self> {
        if bytes.get(..4)? != b"TZif" {
            return None;
        }
        let count = |i: usize| -> Option<usize> {
            let at = 20 + i * 4;
            let word = bytes.get(at..at + 4)?;
            usize::try_from(u32::from_be_bytes(word.try_into().ok()?)).ok()
        };
        Some(Self {
            version: *bytes.get(4)?,
            isutcnt: count(0)?,
            isstdcnt: count(1)?,
            leapcnt: count(2)?,
            timecnt: count(3)?,
            typecnt: count(4)?,
            charcnt: count(5)?,
        })
    }

    /// The data block's length after this header, with `width`-byte times.
    fn block_len(self, width: usize) -> Option<usize> {
        let leap = width.checked_add(4)?;
        self.timecnt
            .checked_mul(width)?
            .checked_add(self.timecnt)?
            .checked_add(self.typecnt.checked_mul(6)?)?
            .checked_add(self.charcnt)?
            .checked_add(self.leapcnt.checked_mul(leap)?)?
            .checked_add(self.isstdcnt)?
            .checked_add(self.isutcnt)
    }
}

/// The footer's POSIX `TZ` string (`\n<TZ>\n`), `None` when it is empty or absent.
fn footer_of(rest: &[u8]) -> Option<&str> {
    let rest = rest.strip_prefix(b"\n")?;
    let end = rest.iter().position(|b| *b == b'\n')?;
    let tz = std::str::from_utf8(rest.get(..end)?).ok()?;
    (!tz.is_empty()).then_some(tz)
}

/// A POSIX `TZ` string's offset from UTC at `unix`, in seconds east of UTC: `std offset [dst
/// [offset] [,start[/time],end[/time]]]`, with RFC 8536's extensions (a rule time of -167 to
/// 167 hours). A `dst` with no rule takes the US rule (`M3.2.0,M11.1.0`), as tzcode does.
/// `None` where the string does not parse.
#[must_use]
pub fn posix_offset_at(tz: &str, unix: i64) -> Option<i64> {
    let mut p = Posix {
        rest: tz.as_bytes(),
    };
    p.name()?;
    // POSIX offsets are west of UTC: `EST5` is five hours behind.
    let std_off = -p.clock(24)?;
    if p.rest.is_empty() {
        return Some(std_off);
    }
    p.name()?;
    let dst_off = match p.rest.first() {
        Some(b',') | None => std_off + 3600,
        Some(_) => -p.clock(24)?,
    };
    let (start, end) = if p.rest.is_empty() {
        let mut us = Posix {
            rest: b",M3.2.0,M11.1.0",
        };
        (us.rule()?, us.rule()?)
    } else {
        (p.rule()?, p.rule()?)
    };
    if !p.rest.is_empty() {
        return None;
    }
    let (year, _, _) = civil_from_days((unix + std_off).div_euclid(86_400));
    // The start is said in standard time, the end in daylight time.
    let starts = start.local_seconds(year)? - std_off;
    let ends = end.local_seconds(year)? - dst_off;
    let daylight = if starts < ends {
        starts <= unix && unix < ends
    } else {
        !(ends <= unix && unix < starts)
    };
    Some(if daylight { dst_off } else { std_off })
}

/// The unread rest of a POSIX `TZ` string.
struct Posix<'a> {
    rest: &'a [u8],
}

/// When in a year a rule changes the clock.
#[derive(Clone, Copy)]
enum Day {
    /// `Jn`: day `n` of 1 to 365, February 29 never counted.
    Julian(i64),
    /// `n`: day `n` of 0 to 365, February 29 counted.
    Zero(i64),
    /// `Mm.w.d`: weekday `d` (0 Sunday) of week `w` (5 the last) of month `m`.
    Month { m: i64, w: i64, d: i64 },
}

/// A rule's day and its local time of day.
#[derive(Clone, Copy)]
struct Rule {
    day: Day,
    seconds: i64,
}

impl Rule {
    /// When this rule changes the clock in `year`, in local seconds since the epoch.
    fn local_seconds(self, year: i64) -> Option<i64> {
        let jan1 = days_from_civil(year, 1, 1);
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let day = match self.day {
            Day::Julian(n) => jan1 + n - 1 + i64::from(leap && n >= 60),
            Day::Zero(n) => jan1 + n,
            Day::Month { m, w, d } => {
                let first = days_from_civil(year, m, 1);
                let weekday = (first + 4).rem_euclid(7);
                let mut day = first + (d - weekday).rem_euclid(7) + (w - 1) * 7;
                let next = if m == 12 {
                    days_from_civil(year + 1, 1, 1)
                } else {
                    days_from_civil(year, m + 1, 1)
                };
                while day >= next {
                    day -= 7;
                }
                day
            }
        };
        day.checked_mul(86_400)?.checked_add(self.seconds)
    }
}

impl Posix<'_> {
    /// A zone abbreviation: `<...>` quoted, or three or more letters.
    fn name(&mut self) -> Option<()> {
        if let Some(rest) = self.rest.strip_prefix(b"<") {
            let end = rest.iter().position(|b| *b == b'>')?;
            self.rest = rest.get(end + 1..)?;
            return (end >= 3).then_some(());
        }
        let len = self
            .rest
            .iter()
            .take_while(|b| b.is_ascii_alphabetic())
            .count();
        self.rest = self.rest.get(len..)?;
        (len >= 3).then_some(())
    }

    fn number(&mut self) -> Option<i64> {
        let len = self.rest.iter().take_while(|b| b.is_ascii_digit()).count();
        if len == 0 || len > 3 {
            return None;
        }
        let digits = std::str::from_utf8(self.rest.get(..len)?).ok()?;
        self.rest = self.rest.get(len..)?;
        digits.parse().ok()
    }

    /// `[+|-]hh[:mm[:ss]]` as seconds, `hh` at most `most` hours.
    fn clock(&mut self, most: i64) -> Option<i64> {
        let sign = match self.rest.first() {
            Some(b'-') => -1,
            Some(b'+') => 1,
            _ => 0,
        };
        if sign != 0 {
            self.rest = self.rest.get(1..)?;
        }
        let hours = self.number()?;
        if hours > most {
            return None;
        }
        let mut seconds = hours * 3600;
        for scale in [60, 1] {
            let Some(rest) = self.rest.strip_prefix(b":") else {
                break;
            };
            self.rest = rest;
            let part = self.number()?;
            if part > 59 {
                return None;
            }
            seconds += part * scale;
        }
        Some(if sign < 0 { -seconds } else { seconds })
    }

    /// `,date[/time]`.
    fn rule(&mut self) -> Option<Rule> {
        self.rest = self.rest.strip_prefix(b",")?;
        let day = match self.rest.first()? {
            b'J' => {
                self.rest = self.rest.get(1..)?;
                let n = self.number()?;
                (1..=365).contains(&n).then_some(Day::Julian(n))?
            }
            b'M' => {
                self.rest = self.rest.get(1..)?;
                let m = self.number()?;
                self.rest = self.rest.strip_prefix(b".")?;
                let w = self.number()?;
                self.rest = self.rest.strip_prefix(b".")?;
                let d = self.number()?;
                ((1..=12).contains(&m) && (1..=5).contains(&w) && (0..=6).contains(&d))
                    .then_some(Day::Month { m, w, d })?
            }
            _ => {
                let n = self.number()?;
                (0..=365).contains(&n).then_some(Day::Zero(n))?
            }
        };
        let seconds = match self.rest.strip_prefix(b"/") {
            Some(rest) => {
                self.rest = rest;
                self.clock(167)?
            }
            None => 2 * 3600,
        };
        Some(Rule { day, seconds })
    }
}

/// (year, month, day) → days since 1970-01-01, proleptic Gregorian (Hinnant's
/// `days_from_civil`). `aterm_types::rfc3339`, the workspace's calendar, depends on this
/// crate, so the zone rules carry their own two lines of it; `the_calendar_is_civil` holds
/// it to known dates.
const fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Days since 1970-01-01 → (year, month, day), the inverse of [`days_from_civil`].
const fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

#[cfg(unix)]
mod imp {
    /// `struct tm` (`<time.h>`), laid out alike on Darwin and glibc: nine `int`s, then the
    /// BSD/GNU `tm_gmtoff` and `tm_zone`. Not in the workspace's `libc`, so declared here
    /// with the calls that refresh and fill it.
    #[repr(C)]
    struct Tm {
        _date_and_time: [libc::c_int; 9],
        tm_gmtoff: libc::c_long,
        _tm_zone: *const libc::c_char,
    }

    #[cfg(target_pointer_width = "64")]
    const _: () = assert!(size_of::<Tm>() == 56);

    unsafe extern "C" {
        fn localtime_r(clock: *const libc::time_t, result: *mut Tm) -> *mut Tm;
        fn tzset();
    }

    pub(super) fn local_offset_at(unix: i64) -> Option<i64> {
        let clock: libc::time_t = unix;
        let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
        // SAFETY: `tzset` takes no pointers and reads the environment without changing
        // it; `clock` is a live `time_t` and `tm` a writable `struct tm` of the pinned
        // size, the one thing `localtime_r` writes. No thread may change `TZ` beside these
        // readers (setting a variable is `unsafe` for that reason).
        let filled = unsafe {
            tzset();
            localtime_r(&raw const clock, tm.as_mut_ptr())
        };
        if filled.is_null() {
            return None;
        }
        #[expect(
            clippy::useless_conversion,
            reason = "`tm_gmtoff` is a C `long`: `i64` on the LP64 targets aterm builds, narrower elsewhere"
        )]
        // SAFETY: a non-null answer filled every field; all are plain integers or a
        // pointer this never reads.
        let offset = i64::from(unsafe { tm.assume_init() }.tm_gmtoff);
        Some(offset)
    }
}

#[cfg(not(unix))]
mod imp {
    pub(super) fn local_offset_at(_unix: i64) -> Option<i64> {
        None
    }
}

#[cfg(test)]
#[path = "zone_tests.rs"]
mod tests;
