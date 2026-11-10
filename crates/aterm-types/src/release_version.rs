// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Human product release versions, separate from build ids and technical upstream versions.

use std::fmt;

/// The daily counter's existing client bound (a canonical component of at most nine digits).
pub const MAX_CALENDAR_SEQUENCE: u64 = 999_999_999;

/// A historical `MAJOR.MINOR.0` or a calendar product version `1.YYYYMMDD.N`.
/// Calendar dates are Gregorian, and `N` is positive. Ordering compares all three components.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReleaseVersion((u64, u64, u64));

/// Why the next product version cannot be allocated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NextVersionError {
    /// The observed UTC day is before the previous calendar release's day.
    ClockRegression,
    /// This day already used the greatest counter accepted by the clients.
    SequenceExhausted,
    /// The legacy minor component cannot be incremented.
    Overflow,
    /// The UTC year or resulting date cannot be represented by `YYYYMMDD`.
    DateOutOfRange,
}

impl fmt::Display for NextVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ClockRegression => "UTC clock precedes the previous release date",
            Self::SequenceExhausted => "daily release counter exhausted its client bound",
            Self::Overflow => "legacy release minor component overflow",
            Self::DateOutOfRange => "release date is outside canonical YYYYMMDD",
        })
    }
}

impl ReleaseVersion {
    /// Parse an exact public product version, without qualifiers or a `v` prefix.
    #[must_use]
    pub fn parse(version: &str) -> Option<Self> {
        let mut parts = version.split('.');
        let number = |s: &str| {
            (!s.is_empty()
                && (s == "0" || !s.starts_with('0'))
                && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u64>().ok())
            .flatten()
        };
        let core = (
            number(parts.next()?)?,
            number(parts.next()?)?,
            number(parts.next()?)?,
        );
        if parts.next().is_some() {
            return None;
        }
        Self::from_components(core)
    }

    /// Admit a parsed product core; upstream and compiler versions use their own types.
    #[must_use]
    pub fn from_components(core: (u64, u64, u64)) -> Option<Self> {
        let candidate = Self(core);
        if candidate.is_calendar() {
            (valid_date(core.1) && (1..=MAX_CALENDAR_SEQUENCE).contains(&core.2))
                .then_some(candidate)
        } else {
            (core.2 == 0).then_some(candidate)
        }
    }

    /// The full version core, for comparisons with historical records.
    #[must_use]
    pub const fn components(self) -> (u64, u64, u64) {
        self.0
    }

    /// Whether this uses the calendar product namespace.
    #[must_use]
    pub fn is_calendar(self) -> bool {
        self.0.0 == 1 && (10_000_000..100_000_000).contains(&self.0.1)
    }

    /// The immediate next version in this release's line (same day for calendar versions).
    /// This is an ordering bound; a new cut must use [`Self::next_at`] with its UTC clock.
    /// # Errors
    /// Counter/minor overflow, or a legacy increment entering an invalid calendar date.
    pub fn successor(self) -> Result<Self, NextVersionError> {
        let (major, minor, patch) = self.0;
        if self.is_calendar() {
            if patch == MAX_CALENDAR_SEQUENCE {
                return Err(NextVersionError::SequenceExhausted);
            }
            Ok(Self((major, minor, patch + 1)))
        } else {
            Self::from_components((
                major,
                minor.checked_add(1).ok_or(NextVersionError::Overflow)?,
                0,
            ))
            .ok_or(NextVersionError::DateOutOfRange)
        }
    }

    /// Allocate after the source's previous version at a fixed UTC selection time.
    /// Legacy products keep their minor bump until their source adopts calendar versions.
    /// The caller freezes the result in its existing source CAS/claim/journal, and reuses
    /// that identity on resume rather than allocating again.
    /// # Errors
    /// An out-of-range UTC date, clock regression, or exhausted counter/minor component.
    pub fn next_at(self, unix_seconds: u64) -> Result<Self, NextVersionError> {
        if !self.is_calendar() {
            return self.successor();
        }
        let days =
            i64::try_from(unix_seconds / 86_400).map_err(|_| NextVersionError::DateOutOfRange)?;
        let (year, month, day) = crate::rfc3339::civil_from_days(days);
        if !(1000..=9999).contains(&year) {
            return Err(NextVersionError::DateOutOfRange);
        }
        let date = (year * 10_000 + month * 100 + day) as u64;
        match date.cmp(&self.0.1) {
            std::cmp::Ordering::Less => Err(NextVersionError::ClockRegression),
            std::cmp::Ordering::Equal => self.successor(),
            std::cmp::Ordering::Greater => Ok(Self((1, date, 1))),
        }
    }
}

impl fmt::Display for ReleaseVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0.0, self.0.1, self.0.2)
    }
}

fn valid_date(date: u64) -> bool {
    let (year, month, day) = (
        (date / 10_000) as i64,
        ((date / 100) % 100) as i64,
        (date % 100) as i64,
    );
    (1000..=9999).contains(&year)
        && (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && crate::rfc3339::civil_from_days(crate::rfc3339::days_from_civil(year, month, day))
            == (year, month, day)
}

#[cfg(test)]
mod tests {
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
}
