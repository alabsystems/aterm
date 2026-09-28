// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Howard Hinnant's civil-calendar conversions and the RFC3339 UTC stamp
//! built on them — the workspace home for this math (update, release, gui,
//! atpkg and the agent harness all stamp/parse the same
//! `YYYY-MM-DDTHH:MM:SSZ` shape). Pure functions only: callers keep their own
//! clock reads. The crates that do not depend on this one keep a date
//! formatter of their own: `aterm-messages` (no dependencies, by design),
//! `aterm-spec`'s report date and `xtask`'s perf date.
//!
//! PARSING has one home too. [`parse_utc`] is the strict inverse of
//! [`format_rfc3339`] and the default for every reader; [`parse_utc_fractional`]
//! additionally admits a fractional-seconds field, for the records whose
//! writer stamps one (the package pass end, `….000000000Z`; Claude Code's
//! transcript stamps, `….796Z`). Neither reads a zone offset: `…+05:30` names a
//! DIFFERENT instant than its digits, and reading it as UTC moves a deadline
//! the wrong way by up to 14 h. Five near-identical private copies (update
//! ledger, health streaks, machine roster, package stamps, the atpkg index
//! gate) were folded into these two on 2026-09-25, and the harness's
//! transcript-stamp reader (which took `…+05:30` as UTC) on 2026-09-26. The
//! one reader with a different grammar is the supervisor ledger's
//! (`aterm-agent`'s `supervise/ledger.rs`): it reads local times and zone
//! offsets ON PURPOSE, applying them, over this module's calendar.

/// Days since 1970-01-01 → proleptic-Gregorian `(year, month, day)` —
/// Howard Hinnant's branch-free `civil_from_days`. Pure and total.
#[must_use]
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    // Shift the epoch from 1970-01-01 to 0000-03-01 so leap days land at the
    // end of each 400-year era.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // day-of-era      [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day-of-year (Mar 1 = 0)
    let mp = (5 * doy + 2) / 153; // month, shifted so Mar = 0  [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day-of-month  [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month  [1, 12]
    let y = yoe + era * 400 + i64::from(m <= 2); // Jan/Feb belong to the next year
    (y, m, d)
}

/// Civil `(year, month, day)` → days since 1970-01-01 — Howard Hinnant's
/// `days_from_civil`, the exact inverse of [`civil_from_days`]. Field
/// validation is the CALLER's policy: out-of-range months/days extrapolate
/// arithmetically instead of erroring.
#[must_use]
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 }; // month, shifted so Mar = 0
    let doy = (153 * mp + 2) / 5 + d - 1; // day-of-year (Mar 1 = 0)
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // day-of-era
    era * 146_097 + doe - 719_468
}

/// Format seconds-since-Unix-epoch as an RFC3339 UTC instant
/// (`YYYY-MM-DDTHH:MM:SSZ`). The calendar date comes from
/// [`civil_from_days`]; time-of-day is a plain `secs % 86400` split. Pure and
/// total for all `u64` inputs.
///
/// Emitted via `ToString` + manual zero-padding, not `format!` — a
/// runtime-argument `format_args!` in this crate is a hard Trust-gate error
/// (see `trust_fmt`); the output is byte-identical.
#[must_use]
pub fn format_rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days);
    let mut out = String::with_capacity(20);
    // `days >= 0`, so the date is at/after 1970-01-01: every component is
    // nonnegative and the casts below are lossless.
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

/// `YYYY-MM-DDTHH:MM:SSZ` → seconds since the Unix epoch (negative before
/// it), or `None` for anything else. Exactly 20 bytes: a zone offset, a bare
/// stamp with no `Z`, a fractional-seconds field, a space for the `T` and any
/// trailing byte are all refused, as is a field out of range (month 1-12, day
/// 1-31, hour 0-23, minute 0-59, second 0-60 for a leap second). The exact
/// inverse of [`format_rfc3339`] over its whole range.
#[must_use]
pub fn parse_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[19] != b'Z' {
        return None;
    }
    parse_fields(b)
}

/// [`parse_utc`] plus an optional fractional-seconds field — `.` and one to
/// nine digits between the seconds and the `Z` — which is truncated to the
/// whole second. Offsets, a missing `Z` and trailing bytes are refused exactly
/// as [`parse_utc`] refuses them. Only for a record whose writer stamps a
/// fraction; every other reader wants [`parse_utc`].
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
fn push_padded(out: &mut String, v: u64, width: usize) {
    let digits = v.to_string();
    for _ in digits.len()..width {
        out.push('0');
    }
    out.push_str(&digits);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The goldens the per-crate copies were pinned to before unification:
    /// the epoch, a plain date, a leap day, and the pre-leap-day boundary.
    #[test]
    fn format_rfc3339_matches_known_instants() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_751_328_000), "2025-07-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_709_210_096), "2024-02-29T12:34:56Z");
        assert_eq!(format_rfc3339(1_709_164_799), "2024-02-28T23:59:59Z");
    }

    /// The strict parser inverts the formatter, and refuses every looser shape
    /// the five folded copies refused between them.
    #[test]
    fn parse_utc_inverts_format_and_refuses_looser_shapes() {
        for secs in [
            0_u64,
            86_400,
            1_709_164_799,
            1_709_210_096,
            1_751_328_000,
            1_785_801_600,
            4_102_444_800,
        ] {
            let stamp = format_rfc3339(secs);
            assert_eq!(parse_utc(&stamp), Some(secs as i64), "{stamp}");
            assert_eq!(parse_utc_fractional(&stamp), Some(secs as i64), "{stamp}");
        }
        assert_eq!(parse_utc("2026-07-05T12:00:00Z"), Some(1_783_252_800));
        assert_eq!(
            parse_utc("1969-12-31T23:59:59Z"),
            Some(-1),
            "pre-epoch is signed"
        );
        for bad in [
            "",
            "not a stamp at all!",
            "2026-08-04",
            "2026-08-04 00:00:00Z",
            "2026/08/04T00:00:00Z",
            "2026-13-04T00:00:00Z",
            "2026-00-04T00:00:00Z",
            "2026-08-00T00:00:00Z",
            "2026-08-32T00:00:00Z",
            "2026-08-04T24:00:00Z",
            "2026-08-04T00:60:00Z",
            "2026-08-04T00:00:61Z",
            "20xx-08-04T00:00:00Z",
            "2026-08-0xT00:00:00Z",
            "+026-08-04T00:00:00Z",
            "2026-08-04T00:00:00+05:30",
            "2026-08-04T00:00:00-08:00",
            "2026-08-04T00:00:00+00:00",
            "2026-08-04T00:00:00",
            "2026-08-04T00:00:00Z ",
            "2026-08-04T00:00:00Zjunk",
            "2026-08-04T00:00:00GARBAGE",
            "2026-08-04T00:00:00.500Z",
        ] {
            assert_eq!(parse_utc(bad), None, "{bad:?} must not parse strictly");
        }
    }

    /// The fractional reader admits exactly one extra shape — `.` and one to
    /// nine digits before the `Z`, truncated — and refuses everything the
    /// strict one refuses, offsets above all.
    #[test]
    fn parse_utc_fractional_truncates_a_fraction_and_nothing_else() {
        let whole = parse_utc("2026-09-10T06:40:53Z");
        assert!(whole.is_some());
        for ok in [
            "2026-09-10T06:40:53.1Z",
            "2026-09-10T06:40:53.123Z",
            "2026-09-10T06:40:53.000000000Z",
            "2026-09-10T06:40:53.999999999Z",
        ] {
            assert_eq!(parse_utc_fractional(ok), whole, "{ok:?}");
        }
        for bad in [
            "2026-09-10T06:40:53.Z",
            "2026-09-10T06:40:53.1234567890Z",
            "2026-09-10T06:40:53.12aZ",
            "2026-09-10T06:40:53,123Z",
            "2026-09-10T06:40:53.123",
            "2026-09-10T06:40:53.123+01:00",
            "2026-09-10T06:40:53+01:00",
            "2026-09-10T06:40:53.123Z ",
            "2026-09-10T06:40:53",
            "2026-13-10T06:40:53.1Z",
            "Z",
            "",
        ] {
            assert_eq!(parse_utc_fractional(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn days_from_civil_inverts_civil_from_days() {
        for days in [
            0_i64, 1, 58, 59, 60, 364, 365, 730, 20_000, 146_096, 146_097, 1_000_000,
        ] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        // Pre-epoch day counts round-trip too: the shared math is total, and
        // the pre-1970 rejection some call sites apply is THEIR policy.
        for days in [-1_i64, -365, -146_097, -719_468] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }
}
