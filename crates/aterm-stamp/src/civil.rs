// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shared Gregorian conversions for product admission, UTC reports and build stamps.

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
