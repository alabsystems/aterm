// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// Known instants: the epoch, a plain date, a leap day and the day before it.
#[test]
fn format_rfc3339_matches_known_instants() {
    assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(format_rfc3339(1_751_328_000), "2025-07-01T00:00:00Z");
    assert_eq!(format_rfc3339(1_709_210_096), "2024-02-29T12:34:56Z");
    assert_eq!(format_rfc3339(1_709_164_799), "2024-02-28T23:59:59Z");
}

/// The strict parser inverts the formatter and refuses every looser shape.
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
        253_402_300_799, // the last second of year 9999
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
    assert_eq!(format_rfc3339(253_402_300_800), "10000-01-01T00:00:00Z");
    for secs in [253_402_300_800, u64::MAX] {
        let extended = format_rfc3339(secs);
        assert_eq!(parse_utc(&extended), None, "{extended}");
        assert_eq!(parse_utc_fractional(&extended), None, "{extended}");
    }
    assert_eq!(
        parse_utc("2000-02-30T00:00:00Z"),
        parse_utc("2000-03-01T00:00:00Z"),
        "numeric field admission preserves arithmetic normalization"
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
        i64::MIN,
        i64::MIN + 1,
        -865_566,
        -865_565,
        -719_469,
        -719_468,
        -146_097,
        -365,
        -1,
        0,
        1,
        58,
        59,
        60,
        364,
        365,
        730,
        20_000,
        146_096,
        146_097,
        1_000_000,
        i64::MAX - 719_468,
        i64::MAX - 1,
        i64::MAX,
    ] {
        let (y, m, d) = civil_from_days(days);
        assert_eq!(days_from_civil(y, m, d), days);
        assert!((1..=12).contains(&m) && (1..=31).contains(&d));
        if let Some(era_later) = days.checked_add(146_097) {
            assert_eq!(civil_from_days(era_later), (y + 400, m, d));
        }
    }
    let (y, m, d) = civil_from_days(i64::MIN);
    assert_eq!(days_from_civil(y, m, d - 1), i64::MIN);
    let (y, m, d) = civil_from_days(i64::MAX);
    assert_eq!(days_from_civil(y, m, d + 1), i64::MAX);
    assert_eq!(days_from_civil(i64::MIN, 1, 1), i64::MIN);
    assert_eq!(days_from_civil(i64::MAX, 12, 31), i64::MAX);
}
