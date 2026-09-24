// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A vendor version (`x.y.z`, canonical only) and the store build id it maps to, plus the
//! signed `buildDate` claude's manifest carries.
//!
//! The build id is `10^18 + major·10^12 + minor·10^6 + patch`: decimal-readable,
//! order-preserving, above every ALab `YYYYMMDDnn` build and below `i64::MAX`, so a vendor
//! build shares `store/<program>/<build>/` and TOML integers with every other build.

use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The smallest vendor build id: `0.0.0`. Every ALab build number is below it.
pub const VENDOR_BUILD_BASE: u64 = 1_000_000_000_000_000_000;

/// Each component is strictly below this (at most six decimal digits).
const COMPONENT_LIMIT: u64 = 1_000_000;

/// The weight of `major` in a build id.
const MAJOR_SCALE: u64 = 1_000_000_000_000;

/// Whether `build` names a vendor-direct build rather than an ALab index build.
#[must_use]
pub const fn is_vendor_build(build: u64) -> bool {
    build >= VENDOR_BUILD_BASE
}

/// A canonical `x.y.z` vendor version. It IS its build id, so `Ord` is version order and
/// two spellings of one version cannot exist.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(u64);

impl Version {
    /// `major.minor.patch`, or `None` when a component is `>= 10^6`. Never clamps.
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Option<Self> {
        let (major, minor, patch) = (major as u64, minor as u64, patch as u64);
        if major >= COMPONENT_LIMIT || minor >= COMPONENT_LIMIT || patch >= COMPONENT_LIMIT {
            return None;
        }
        let Some(m) = major.checked_mul(MAJOR_SCALE) else {
            return None;
        };
        let Some(n) = minor.checked_mul(COMPONENT_LIMIT) else {
            return None;
        };
        let Some(sum) = m.checked_add(n) else {
            return None;
        };
        let Some(sum) = sum.checked_add(patch) else {
            return None;
        };
        match VENDOR_BUILD_BASE.checked_add(sum) {
            Some(build) => Some(Self(build)),
            None => None,
        }
    }

    /// Parse exactly three dot-separated ASCII-decimal components, each without a leading
    /// zero (a lone `0` is fine) and at most six digits. Anything else is refused: a
    /// version with two spellings would have two build ids.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let major = component(parts.next()?)?;
        let minor = component(parts.next()?)?;
        let patch = component(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        Self::new(major, minor, patch)
    }

    /// The version a store build id names, or `None` for an ALab build number or an id
    /// whose major component is out of range.
    #[must_use]
    pub const fn from_build_id(build: u64) -> Option<Self> {
        let Some(rest) = build.checked_sub(VENDOR_BUILD_BASE) else {
            return None;
        };
        if rest / MAJOR_SCALE >= COMPONENT_LIMIT {
            return None;
        }
        Some(Self(build))
    }

    /// The store build id: `10^18 + major·10^12 + minor·10^6 + patch`.
    #[must_use]
    pub const fn build_id(self) -> u64 {
        self.0
    }

    /// The major component.
    #[must_use]
    pub const fn major(self) -> u32 {
        ((self.0 - VENDOR_BUILD_BASE) / MAJOR_SCALE) as u32
    }

    /// The minor component.
    #[must_use]
    pub const fn minor(self) -> u32 {
        ((self.0 / COMPONENT_LIMIT) % COMPONENT_LIMIT) as u32
    }

    /// The patch component.
    #[must_use]
    pub const fn patch(self) -> u32 {
        (self.0 % COMPONENT_LIMIT) as u32
    }
}

/// One canonical component: 1–6 ASCII digits, no leading zero unless it is `0`.
fn component(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 6 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if s.len() > 1 && s.starts_with('0') {
        return None;
    }
    s.parse().ok()
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major(), self.minor(), self.patch())
    }
}

impl fmt::Debug for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Version({self})")
    }
}

impl Serialize for Version {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Self::parse(&text).ok_or_else(|| D::Error::custom("not a canonical x.y.z version"))
    }
}

/// A UTC instant as claude's signed manifest spells it (`2026-09-21T20:55:27Z`, optional
/// fraction). Fields are ordered so the derived `Ord` is chronological.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct BuildDate {
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    nanos: u32,
}

impl BuildDate {
    /// Parse `YYYY-MM-DDTHH:MM:SS[.f{1,9}]Z`. Any other shape (an offset, a missing `Z`,
    /// an out-of-range field) is `None`, never a guess.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let b = text.as_bytes();
        if b.len() < 20 || b.last() != Some(&b'Z') {
            return None;
        }
        let sep = |i: usize, c: u8| b.get(i) == Some(&c);
        if !(sep(4, b'-') && sep(7, b'-') && sep(10, b'T') && sep(13, b':') && sep(16, b':')) {
            return None;
        }
        let num = |from: usize, to: usize| -> Option<u32> {
            let s = text.get(from..to)?;
            if !s.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            s.parse().ok()
        };
        let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
        let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
        if !(1..=12).contains(&month)
            || !(1..=31).contains(&day)
            || hour > 23
            || minute > 59
            || second > 60
        {
            return None;
        }
        let tail = text.get(19..text.len() - 1)?;
        let nanos = if tail.is_empty() {
            0
        } else {
            let frac = tail.strip_prefix('.')?;
            if frac.is_empty() || frac.len() > 9 || !frac.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let mut padded = String::from(frac);
            while padded.len() < 9 {
                padded.push('0');
            }
            padded.parse().ok()?
        };
        Some(Self {
            year: u16::try_from(year).ok()?,
            month: u8::try_from(month).ok()?,
            day: u8::try_from(day).ok()?,
            hour: u8::try_from(hour).ok()?,
            minute: u8::try_from(minute).ok()?,
            second: u8::try_from(second).ok()?,
            nanos,
        })
    }
}

impl fmt::Display for BuildDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )?;
        if self.nanos != 0 {
            let frac = format!("{:09}", self.nanos);
            write!(f, ".{}", frac.trim_end_matches('0'))?;
        }
        f.write_str("Z")
    }
}

impl Serialize for BuildDate {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for BuildDate {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Self::parse(&text).ok_or_else(|| D::Error::custom("not a UTC build date"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap_or_else(|| panic!("{text} should parse"))
    }

    /// A deterministic xorshift stream: property coverage without a new dependency.
    fn stream(mut seed: u64) -> impl FnMut() -> u64 {
        move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        }
    }

    /// Components biased toward the edges (0, 1, 999_999) so the bounds are exercised.
    fn samples() -> Vec<(u32, u32, u32)> {
        let edges = [0u32, 1, 2, 9, 10, 99, 100, 280, 999_998, 999_999];
        let mut out = Vec::new();
        for &a in &edges {
            for &b in &edges {
                for &c in &edges {
                    out.push((a, b, c));
                }
            }
        }
        let mut next = stream(0x9e37_79b9_7f4a_7c15);
        for _ in 0..4000 {
            let pick = |r: u64| (r % COMPONENT_LIMIT) as u32;
            out.push((pick(next()), pick(next()), pick(next())));
        }
        out
    }

    #[test]
    fn the_worked_example_and_the_extremes() {
        assert_eq!(v("2.1.280").build_id(), 1_000_002_000_001_000_280);
        assert_eq!(v("0.0.0").build_id(), VENDOR_BUILD_BASE);
        assert_eq!(
            v("999999.999999.999999").build_id(),
            1_999_999_999_999_999_999
        );
        assert_eq!(v("0.156.0").to_string(), "0.156.0");
        assert_eq!(format!("{:?}", v("2.1.280")), "Version(2.1.280)");
    }

    #[test]
    fn order_preserving_and_injective() {
        let all = samples();
        let mut next = stream(42);
        for _ in 0..20_000 {
            let a = all[(next() % all.len() as u64) as usize];
            let b = all[(next() % all.len() as u64) as usize];
            let va = Version::new(a.0, a.1, a.2).unwrap();
            let vb = Version::new(b.0, b.1, b.2).unwrap();
            assert_eq!(a.cmp(&b), va.cmp(&vb), "{a:?} vs {b:?}");
            assert_eq!(
                a.cmp(&b),
                va.build_id().cmp(&vb.build_id()),
                "{a:?} vs {b:?}"
            );
            assert_eq!(a == b, va.build_id() == vb.build_id(), "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn round_trips_through_text_and_build_id() {
        for (a, b, c) in samples() {
            let ver = Version::new(a, b, c).unwrap();
            assert_eq!((ver.major(), ver.minor(), ver.patch()), (a, b, c));
            assert_eq!(Version::from_build_id(ver.build_id()), Some(ver));
            assert_eq!(Version::parse(&ver.to_string()), Some(ver));
            assert!(is_vendor_build(ver.build_id()));
        }
    }

    #[test]
    fn above_every_alab_build_and_below_i64_max() {
        let lowest = Version::new(0, 0, 0).unwrap().build_id();
        let highest = Version::new(999_999, 999_999, 999_999).unwrap().build_id();
        for alab in [0u64, 1, 8595, 20_065, 2_026_091_901, 9_999_999_999] {
            assert!(alab < lowest, "{alab}");
            assert!(!is_vendor_build(alab));
            assert_eq!(Version::from_build_id(alab), None);
        }
        assert!(highest < i64::MAX as u64);
        assert!(i64::try_from(highest).is_ok());
    }

    #[test]
    fn bounds_and_non_canonical_spellings_are_refused() {
        assert_eq!(Version::new(1_000_000, 0, 0), None);
        assert_eq!(Version::new(0, 1_000_000, 0), None);
        assert_eq!(Version::new(0, 0, 1_000_000), None);
        assert_eq!(Version::new(u32::MAX, u32::MAX, u32::MAX), None);
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "1..3",
            ".1.2",
            "1.2.",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "00.0.0",
            "1000000.0.0",
            "0.1000000.0",
            "0.0.1000000",
            "+1.2.3",
            "-1.2.3",
            " 1.2.3",
            "1.2.3 ",
            "1.2.3\n",
            "1.2.3-beta",
            "v1.2.3",
            "1.2.3+g1",
            "\u{661}.2.3",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad:?} must be refused");
        }
        // Past the largest major: 10^18 + 10^6·10^12 is not a version.
        assert_eq!(
            Version::from_build_id(VENDOR_BUILD_BASE + COMPONENT_LIMIT * MAJOR_SCALE),
            None
        );
        assert_eq!(Version::from_build_id(u64::MAX), None);
    }

    #[test]
    fn serde_spells_the_version_as_text_and_refuses_a_bad_one() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Row {
            v: Version,
        }
        let text = aterm_toml::to_string(&Row { v: v("2.1.280") }).unwrap();
        assert_eq!(text.trim(), "v = \"2.1.280\"");
        assert_eq!(
            aterm_toml::from_str::<Row>(&text).unwrap(),
            Row { v: v("2.1.280") }
        );
        assert!(aterm_toml::from_str::<Row>("v = \"2.1.08\"").is_err());
    }

    #[test]
    fn build_dates_order_chronologically_and_round_trip() {
        let a = BuildDate::parse("2026-09-21T20:55:27Z").unwrap();
        let b = BuildDate::parse("2026-09-21T20:55:27.5Z").unwrap();
        let c = BuildDate::parse("2026-09-21T20:55:28Z").unwrap();
        let d = BuildDate::parse("2027-01-01T00:00:00Z").unwrap();
        assert!(a < b && b < c && c < d);
        assert_eq!(BuildDate::parse("2026-09-21T20:55:27.000Z"), Some(a));
        for t in [
            "2026-09-21T20:55:27Z",
            "2026-09-21T20:55:27.5Z",
            "2026-09-21T20:55:27.123456789Z",
        ] {
            assert_eq!(BuildDate::parse(t).unwrap().to_string(), t);
        }
        for bad in [
            "",
            "2026-09-21",
            "2026-09-21T20:55:27",
            "2026-09-21T20:55:27+00:00",
            "2026-13-21T20:55:27Z",
            "2026-09-00T20:55:27Z",
            "2026-09-21T24:00:00Z",
            "2026-09-21T20:60:00Z",
            "2026-09-21 20:55:27Z",
            "2026-09-21T20:55:27.Z",
            "2026-09-21T20:55:27.1234567890Z",
            "+026-09-21T20:55:27Z",
        ] {
            assert_eq!(BuildDate::parse(bad), None, "{bad:?} must be refused");
        }
    }
}
