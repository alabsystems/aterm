// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Howard Hinnant's civil-calendar conversions and the RFC3339 UTC stamp
//! built on them — the ONE workspace home for this math (update, release,
//! gui, and atpkg all stamp/parse the same `YYYY-MM-DDTHH:MM:SSZ` shape).
//! Pure functions only: callers keep their own clock reads and their own
//! parse/validation policies, which deliberately differ per call site.

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
