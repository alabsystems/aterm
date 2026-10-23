// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TOML 1.0 date-times across the spec's four shapes, serialized through serde under a
//! private struct name compatible with `toml`.

use core::fmt;
use core::str::FromStr;

/// The reserved struct name a `Datetime` travels under through serde.
pub(crate) const DATETIME_STRUCT: &str = "$__aterm_toml_private_Datetime";
/// The reserved field name inside [`DATETIME_STRUCT`].
pub(crate) const DATETIME_FIELD: &str = "$__aterm_toml_private_datetime";

/// A TOML date-time in any of the spec's four shapes (offset date-time, local date-time,
/// local date, or local time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Datetime {
    /// Calendar date, absent only for a local time.
    pub date: Option<Date>,
    /// Wall-clock time, absent only for a local date.
    pub time: Option<Time>,
    /// UTC offset, present only for an offset date-time.
    pub offset: Option<Offset>,
}

/// Proleptic-Gregorian calendar date, range-checked against the month's length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    /// Four-digit year.
    pub year: u16,
    /// 1..=12.
    pub month: u8,
    /// 1..=31, bounded by the month.
    pub day: u8,
}

/// Wall-clock time with nanosecond resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Time {
    /// 0..=23.
    pub hour: u8,
    /// 0..=59.
    pub minute: u8,
    /// `0..=60` (`60` is a leap second).
    pub second: u8,
    /// `0..=999_999_999` (digits past nanosecond resolution are truncated).
    pub nanosecond: u32,
}

/// UTC offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Offset {
    /// UTC (`Z`).
    Z,
    /// Offset in minutes east of UTC (`-1439..=1439`).
    Custom {
        /// Minutes east of UTC, `-1439..=1439`.
        minutes: i16,
    },
}

impl fmt::Display for Datetime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(date) = self.date {
            write!(f, "{date}")?;
        }
        if let Some(time) = self.time {
            if self.date.is_some() {
                f.write_str("T")?;
            }
            write!(f, "{time}")?;
        }
        if let Some(offset) = self.offset {
            write!(f, "{offset}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}:{:02}", self.hour, self.minute, self.second)?;
        if self.nanosecond != 0 {
            let mut frac = format!("{:09}", self.nanosecond);
            while frac.ends_with('0') {
                frac.pop();
            }
            write!(f, ".{frac}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Offset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Offset::Z => f.write_str("Z"),
            Offset::Custom { minutes } => {
                let sign = if minutes < 0 { '-' } else { '+' };
                let magnitude = minutes.unsigned_abs();
                write!(f, "{sign}{:02}:{:02}", magnitude / 60, magnitude % 60)
            }
        }
    }
}

/// Error returned by [`Datetime::from_str`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatetimeParseError {
    pub(crate) reason: &'static str,
}

impl fmt::Display for DatetimeParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid TOML date-time: {}", self.reason)
    }
}

impl std::error::Error for DatetimeParseError {}

impl FromStr for Datetime {
    type Err = DatetimeParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_datetime(s.as_bytes())
            .filter(|(_, consumed)| *consumed == s.len())
            .map(|(dt, _)| dt)
            .ok_or(DatetimeParseError {
                reason: "not an RFC 3339 date, time, or date-time",
            })
    }
}

#[inline]
fn digits(b: &[u8], at: usize, n: usize) -> Option<u32> {
    let end = at.checked_add(n)?;
    if end > b.len() {
        return None;
    }
    let mut acc = 0u32;
    for &c in &b[at..end] {
        if !c.is_ascii_digit() {
            return None;
        }
        acc = acc * 10 + u32::from(c - b'0');
    }
    Some(acc)
}

#[inline]
const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap =
                (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
            if leap { 29 } else { 28 }
        }
        _ => 0,
    }
}

/// Parses a date-time prefix of `b` and returns `(datetime, consumed_bytes)`.
pub(crate) fn parse_datetime(b: &[u8]) -> Option<(Datetime, usize)> {
    // Local time: hh:mm:ss[.frac]
    if b.len() >= 3 && b[2] == b':' {
        let (time, used) = parse_time(b, 0)?;
        return Some((
            Datetime {
                date: None,
                time: Some(time),
                offset: None,
            },
            used,
        ));
    }

    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let year = u16::try_from(digits(b, 0, 4)?).ok()?;
    let month = u8::try_from(digits(b, 5, 2)?).ok()?;
    let day = u8::try_from(digits(b, 8, 2)?).ok()?;
    if month == 0 || month > 12 || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    let date = Date { year, month, day };
    let mut pos = 10;

    // A space separator only counts when `hh:` follows, so `1979-05-27 # comment` stays a local date.
    let sep = match b.get(pos) {
        Some(b'T' | b't') => true,
        Some(b' ') => {
            matches!(digits(b, pos + 1, 2), Some(h) if h < 24) && b.get(pos + 3) == Some(&b':')
        }
        _ => false,
    };
    if !sep {
        return Some((
            Datetime {
                date: Some(date),
                time: None,
                offset: None,
            },
            pos,
        ));
    }
    pos += 1;
    let (time, used) = parse_time(b, pos)?;
    pos = used;

    let offset = match b.get(pos) {
        Some(b'Z' | b'z') => {
            pos += 1;
            Some(Offset::Z)
        }
        Some(sign @ (b'+' | b'-')) => {
            let negative = *sign == b'-';
            let hour = digits(b, pos + 1, 2)?;
            if b.get(pos + 3) != Some(&b':') {
                return None;
            }
            let minute = digits(b, pos + 4, 2)?;
            if hour > 23 || minute > 59 {
                return None;
            }
            pos += 6;
            let total = i16::try_from(hour * 60 + minute).ok()?;
            Some(Offset::Custom {
                minutes: if negative { -total } else { total },
            })
        }
        _ => None,
    };

    Some((
        Datetime {
            date: Some(date),
            time: Some(time),
            offset,
        },
        pos,
    ))
}

fn parse_time(b: &[u8], mut pos: usize) -> Option<(Time, usize)> {
    let hour = u8::try_from(digits(b, pos, 2)?).ok()?;
    if b.get(pos + 2) != Some(&b':') {
        return None;
    }
    let minute = u8::try_from(digits(b, pos + 3, 2)?).ok()?;
    if b.get(pos + 5) != Some(&b':') {
        return None;
    }
    let second = u8::try_from(digits(b, pos + 6, 2)?).ok()?;
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    pos += 8;

    let mut nanosecond = 0u32;
    if b.get(pos) == Some(&b'.') {
        let start = pos + 1;
        let mut end = start;
        while end < b.len() && b[end].is_ascii_digit() {
            end += 1;
        }
        if end == start {
            return None;
        }
        let mut scale = 100_000_000u32;
        for &c in &b[start..end.min(start + 9)] {
            nanosecond += u32::from(c - b'0') * scale;
            scale /= 10;
        }
        pos = end;
    }

    Some((
        Time {
            hour,
            minute,
            second,
            nanosecond,
        },
        pos,
    ))
}

impl serde::Serialize for Datetime {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct as _;
        let mut s = serializer.serialize_struct(DATETIME_STRUCT, 1)?;
        s.serialize_field(DATETIME_FIELD, &self.to_string())?;
        s.end()
    }
}

impl<'de> serde::Deserialize<'de> for Datetime {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct DatetimeVisitor;

        impl<'de> serde::de::Visitor<'de> for DatetimeVisitor {
            type Value = Datetime;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a TOML date-time")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Datetime, A::Error> {
                let key: DatetimeKey = map.next_key()?.ok_or_else(|| {
                    <A::Error as serde::de::Error>::custom("date-time value is missing")
                })?;
                let DatetimeKey = key;
                let text: String = map.next_value()?;
                text.parse().map_err(serde::de::Error::custom)
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Datetime, E> {
                v.parse().map_err(serde::de::Error::custom)
            }
        }

        deserializer.deserialize_struct(DATETIME_STRUCT, &[DATETIME_FIELD], DatetimeVisitor)
    }
}

struct DatetimeKey;

impl<'de> serde::Deserialize<'de> for DatetimeKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FieldVisitor;

        impl serde::de::Visitor<'_> for FieldVisitor {
            type Value = ();

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a valid date-time field")
            }

            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<(), E> {
                if s == DATETIME_FIELD {
                    Ok(())
                } else {
                    Err(serde::de::Error::custom("expected field with custom name"))
                }
            }
        }

        deserializer.deserialize_identifier(FieldVisitor)?;
        Ok(DatetimeKey)
    }
}
