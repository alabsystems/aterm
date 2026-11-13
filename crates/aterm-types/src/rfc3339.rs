// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! RFC 3339 UTC stamps (`YYYY-MM-DDTHH:MM:SSZ`) over the shared civil calendar.

pub use aterm_stamp::civil::{civil_from_days, days_from_civil};

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
