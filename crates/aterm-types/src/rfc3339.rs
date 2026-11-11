// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Howard Hinnant's civil-calendar conversions and the RFC 3339 UTC stamp (`YYYY-MM-DDTHH:MM:SSZ`)
//! built on them: the workspace's one home for this math.

/// Days since 1970-01-01 → proleptic-Gregorian `(year, month, day)` —
/// Howard Hinnant's `civil_from_days`.
#[inline]
#[must_use]
pub const fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days as i128 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // day-of-era      [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day-of-year (Mar 1 = 0)
    let mp = (5 * doy + 2) / 153; // month, shifted so Mar = 0  [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day-of-month  [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month  [1, 12]
    let y = yoe + era * 400 + (m <= 2) as i128; // Jan/Feb belong to the next year
    (y as i64, m as i64, d as i64)
}

/// Civil `(year, month, day)` → days since 1970-01-01 — Howard Hinnant's `days_from_civil`.
#[inline]
#[must_use]
pub const fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let (y, m, d) = (y as i128, m as i128, d as i128);
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 }; // month, shifted so Mar = 0
    let doy = (153 * mp + 2) / 5 + d - 1; // day-of-year (Mar 1 = 0)
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // day-of-era
    let days = era * 146_097 + doe - 719_468;
    if days < i64::MIN as i128 {
        i64::MIN
    } else if days > i64::MAX as i128 {
        i64::MAX
    } else {
        days as i64
    }
}

/// Format seconds-since-Unix-epoch as an RFC3339 UTC instant.
#[must_use]
pub fn format_rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days);
    let mut out = String::with_capacity(20);
    push_padded(&mut out, y as u64, 4);
    out.push('-');
    push_padded(&mut out, m as u64, 2);
    out.push('-');
    push_padded(&mut out, d as u64, 2);
    out.push('T');
    push_padded(&mut out, hh, 2);
    out.push(':');
    push_padded(&mut out, mm, 2);
    out.push(':');
    push_padded(&mut out, ss, 2);
    out.push('Z');
    out
}

/// `YYYY-MM-DDTHH:MM:SSZ` → seconds since the Unix epoch, or `None` for anything else.
#[inline]
#[must_use]
pub fn parse_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[19] != b'Z' {
        return None;
    }
    parse_fields(b)
}

/// [`parse_utc`] plus an optional fractional-seconds field.
#[inline]
#[must_use]
pub fn parse_utc_fractional(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let (&last, head) = b.split_last()?;
    if last != b'Z' || head.len() < 19 {
        return None;
    }
    let fraction = &head[19..];
    if !fraction.is_empty() {
        let (&dot, digits) = fraction.split_first()?;
        if dot != b'.' || digits.is_empty() || digits.len() > 9 {
            return None;
        }
        if !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
    }
    parse_fields(b)
}

/// The `YYYY-MM-DDTHH:MM:SS` head both parsers share: separators at their
/// fixed offsets, ASCII digits in every field, every field in range.
#[inline]
fn parse_fields(b: &[u8]) -> Option<i64> {
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let field = |from: usize, to: usize| -> Option<i64> {
        let mut value = 0_i64;
        for &digit in &b[from..to] {
            if !digit.is_ascii_digit() {
                return None;
            }
            value = value * 10 + i64::from(digit - b'0');
        }
        Some(value)
    };
    let (y, mo, d) = (field(0, 4)?, field(5, 7)?, field(8, 10)?);
    let (h, mi, se) = (field(11, 13)?, field(14, 16)?, field(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se)
}

/// Append `v` in decimal, zero-padded to at least `width` digits —
/// byte-identical to `format!("{v:0width$}")` for unsigned values.
#[inline]
fn push_padded(out: &mut String, v: u64, width: usize) {
    let digits = v.to_string();
    for _ in digits.len()..width {
        out.push('0');
    }
    out.push_str(&digits);
}

#[cfg(test)]
#[path = "rfc3339_tests.rs"]
mod tests;
