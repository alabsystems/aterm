// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Canonical vendor versions, their bijective store build IDs in `[10^18, 3·10^18)`,
//! and the UTC `buildDate` in claude's signed manifest.
//!
//! Two `10^18`-wide tiers above every ALab `YYYYMMDDnn` build and below `i64::MAX`:
//! - `x.y.z`: `10^18 + x·10^12 + y·10^6 + z` (`x, y, z < 10^6`).
//! - `x.y.z-R<n>[.<s>]`: `2·10^18 + x·10^15 + y·10^12 + z·10^9 + n·10^3 + s'`
//!   (`x, y, z <= 999`, `n <= 999_999`, `s' = 0` or `s + 1` with `s <= 998`).

use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Smallest vendor build ID (`0.0.0`); every ALab build number is below it.
pub const VENDOR_BUILD_BASE: u64 = 1_000_000_000_000_000_000;

const REVISION_BASE: u64 = 2 * VENDOR_BUILD_BASE;
const VENDOR_BUILD_END: u64 = 3 * VENDOR_BUILD_BASE;
const COMPONENT_LIMIT: u64 = 1_000_000;
const MAJOR_SCALE: u64 = 1_000_000_000_000;
const SHORT_LIMIT: u64 = 1_000;
const REVISION_MAJOR_SCALE: u64 = 1_000_000_000_000_000;
const REVISION_MINOR_SCALE: u64 = 1_000_000_000_000;
const REVISION_PATCH_SCALE: u64 = 1_000_000_000;

/// Whether `build` names a vendor-direct build rather than an ALab index build.
#[must_use]
pub const fn is_vendor_build(build: u64) -> bool {
    build >= VENDOR_BUILD_BASE
}

/// Canonical vendor version in either grammar, represented directly by its build ID.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(u64);

impl Version {
    /// Construct `major.minor.patch`, or `None` if any component is `>= 10^6`.
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Option<Self> {
        let (major, minor, patch) = (major as u64, minor as u64, patch as u64);
        if major >= COMPONENT_LIMIT || minor >= COMPONENT_LIMIT || patch >= COMPONENT_LIMIT {
            return None;
        }
        Some(Self(
            VENDOR_BUILD_BASE + major * MAJOR_SCALE + minor * COMPONENT_LIMIT + patch,
        ))
    }

    /// Construct `major.minor.patch-R<revision>[.<sub>]`, or `None` if any field exceeds its bound.
    #[must_use]
    pub const fn with_revision(
        major: u32,
        minor: u32,
        patch: u32,
        revision: u32,
        sub: Option<u32>,
    ) -> Option<Self> {
        let (major, minor, patch) = (major as u64, minor as u64, patch as u64);
        let revision = revision as u64;
        let sub = match sub {
            Some(s) => s as u64 + 1,
            None => 0,
        };
        if major >= SHORT_LIMIT
            || minor >= SHORT_LIMIT
            || patch >= SHORT_LIMIT
            || revision >= COMPONENT_LIMIT
            || sub >= SHORT_LIMIT
        {
            return None;
        }
        Some(Self(
            REVISION_BASE
                + major * REVISION_MAJOR_SCALE
                + minor * REVISION_MINOR_SCALE
                + patch * REVISION_PATCH_SCALE
                + revision * SHORT_LIMIT
                + sub,
        ))
    }

    /// Parse canonical `x.y.z` or `x.y.z-R<n>[.<s>]` without leading zeros.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let (plain, revision) = match text.split_once("-R") {
            Some((plain, revision)) => (plain, Some(revision)),
            None => (text, None),
        };
        let mut parts = plain.split('.');
        let major = component(parts.next()?)?;
        let minor = component(parts.next()?)?;
        let patch = component(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        let Some(revision) = revision else {
            return Self::new(major, minor, patch);
        };
        let (n, sub) = match revision.split_once('.') {
            Some((n, sub)) => (component(n)?, Some(component(sub)?)),
            None => (component(revision)?, None),
        };
        Self::with_revision(major, minor, patch, n, sub)
    }

    /// Version for a vendor store build ID, or `None` outside `[10^18, 3·10^18)`.
    #[must_use]
    pub const fn from_build_id(build: u64) -> Option<Self> {
        if is_vendor_build(build) && build < VENDOR_BUILD_END {
            Some(Self(build))
        } else {
            None
        }
    }

    /// The store build id.
    #[must_use]
    pub const fn build_id(self) -> u64 {
        self.0
    }

    /// Whether this is the revision form (`x.y.z-R<n>[.<s>]`) rather than `x.y.z`.
    #[must_use]
    pub const fn has_revision(self) -> bool {
        self.0 >= REVISION_BASE
    }

    /// The major component.
    #[must_use]
    pub const fn major(self) -> u32 {
        (if self.has_revision() {
            self.0 / REVISION_MAJOR_SCALE % SHORT_LIMIT
        } else {
            self.0 / MAJOR_SCALE % COMPONENT_LIMIT
        }) as u32
    }

    /// The minor component.
    #[must_use]
    pub const fn minor(self) -> u32 {
        (if self.has_revision() {
            self.0 / REVISION_MINOR_SCALE % SHORT_LIMIT
        } else {
            self.0 / COMPONENT_LIMIT % COMPONENT_LIMIT
        }) as u32
    }

    /// The patch component.
    #[must_use]
    pub const fn patch(self) -> u32 {
        (if self.has_revision() {
            self.0 / REVISION_PATCH_SCALE % SHORT_LIMIT
        } else {
            self.0 % COMPONENT_LIMIT
        }) as u32
    }

    /// `(n, s)` of `x.y.z-R<n>[.<s>]`; `None` for an `x.y.z` version.
    #[must_use]
    pub const fn revision(self) -> Option<(u32, Option<u32>)> {
        if !self.has_revision() {
            return None;
        }
        let n = (self.0 / SHORT_LIMIT % COMPONENT_LIMIT) as u32;
        let sub = match (self.0 % SHORT_LIMIT) as u32 {
            0 => None,
            s => Some(s - 1),
        };
        Some((n, sub))
    }
}

/// One canonical number: 1–6 ASCII digits, no leading zero unless it is `0`.
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
        write!(f, "{}.{}.{}", self.major(), self.minor(), self.patch())?;
        match self.revision() {
            Some((n, Some(s))) => write!(f, "-R{n}.{s}"),
            Some((n, None)) => write!(f, "-R{n}"),
            None => Ok(()),
        }
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
        Self::parse(&text).ok_or_else(|| D::Error::custom("not a canonical vendor version"))
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
        const fn days_in_month(year: u32, month: u32) -> u32 {
            match month {
                1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
                4 | 6 | 9 | 11 => 30,
                2 if (year.is_multiple_of(4) && !year.is_multiple_of(100))
                    || year.is_multiple_of(400) =>
                {
                    29
                }
                2 => 28,
                _ => 0,
            }
        }
        let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
        let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
        if !(1..=12).contains(&month)
            || !(1..=days_in_month(year, month)).contains(&day)
            || hour > 23
            || minute > 59
            || second > 59
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
#[path = "version_tests.rs"]
mod tests;
