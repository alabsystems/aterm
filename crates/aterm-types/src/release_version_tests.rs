// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn historical_lines_and_calendar_versions_have_one_spelling_and_full_order() {
    for version in ["0.191.0", "1.101.0", "1.20240229.1", "1.20261109.999999999"] {
        assert_eq!(ReleaseVersion::parse(version).unwrap().to_string(), version);
    }
    for version in [
        "1.20261109.0",
        "1.20260229.1",
        "1.20261131.1",
        "1.20261301.1",
        "1.20261100.1",
        "1.20261109.1000000000",
        "1.020261109.1",
        "1.20261109.01",
        "2.20261109.1",
        "1.20261109.1+gabcdef012",
        "00.191.0",
        "0.191.1",
    ] {
        assert!(ReleaseVersion::parse(version).is_none(), "{version}");
    }
    let versions = [
        "0.191.0",
        "1.20261109.1",
        "1.20261109.2",
        "1.20261110.1",
        "2.0.0",
    ];
    for pair in versions.windows(2) {
        assert!(ReleaseVersion::parse(pair[0]) < ReleaseVersion::parse(pair[1]));
    }
}

#[test]
fn utc_allocation_refuses_regression_and_overflow_without_changing_legacy_policy() {
    let at = |stamp| u64::try_from(crate::rfc3339::parse_utc(stamp).unwrap()).unwrap();
    let previous = ReleaseVersion::parse("1.20261109.1").unwrap();
    assert_eq!(
        previous
            .next_at(at("2026-11-09T23:59:59Z"))
            .unwrap()
            .to_string(),
        "1.20261109.2"
    );
    assert_eq!(
        previous
            .next_at(at("2026-11-10T00:00:00Z"))
            .unwrap()
            .to_string(),
        "1.20261110.1"
    );
    assert_eq!(
        previous.next_at(at("2026-11-08T23:59:59Z")),
        Err(NextVersionError::ClockRegression)
    );
    let last = ReleaseVersion::parse("1.20261109.999999999").unwrap();
    assert_eq!(
        last.next_at(at("2026-11-09T23:59:59Z")),
        Err(NextVersionError::SequenceExhausted)
    );
    assert_eq!(
        last.next_at(at("2026-11-10T00:00:00Z"))
            .unwrap()
            .to_string(),
        "1.20261110.1"
    );
    assert_eq!(
        previous.next_at(u64::MAX),
        Err(NextVersionError::DateOutOfRange)
    );
    assert_eq!(
        ReleaseVersion::parse("0.191.0")
            .unwrap()
            .next_at(at("2026-11-10T00:00:00Z"))
            .unwrap()
            .to_string(),
        "0.192.0"
    );
    assert_eq!(
        ReleaseVersion::from_components((0, u64::MAX, 0))
            .unwrap()
            .successor(),
        Err(NextVersionError::Overflow)
    );
}
