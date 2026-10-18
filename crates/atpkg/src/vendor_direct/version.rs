// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A vendor version (canonical only) and the store build id it maps to, plus the signed
//! `buildDate` claude's manifest carries.
//!
//! Two grammars, each a tier of ids `10^18` wide:
//! - `x.y.z`: `10^18 + x·10^12 + y·10^6 + z`, each component below `10^6`;
//! - the revision form `x.y.z-R<n>[.<s>]` (Meta's): `2·10^18 + x·10^15 + y·10^12 + z·10^9 +
//!   n·10^3 + s'`, with `x`, `y`, `z` ≤ 999, `n` ≤ 999 999 and `s'` 0 without `.<s>`, else
//!   `s + 1` (`s` ≤ 998).
//!
//! Every id from `10^18` up to `3·10^18` is exactly one version and no other id is one. An
//! id is decimal-readable, order-preserving within its tier, above every ALab
//! `YYYYMMDDnn` build and below `i64::MAX`, so a vendor build shares
//! `store/<program>/<build>/` and TOML integers with every other build. A program speaks one
//! grammar ([`Version::has_revision`]), so its versions never compare across tiers.

use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The smallest vendor build id: `0.0.0`. Every ALab build number is below it.
pub const VENDOR_BUILD_BASE: u64 = 1_000_000_000_000_000_000;

/// The smallest revision-form build id, `0.0.0-R0`: one past the largest `x.y.z` one.
const REVISION_BASE: u64 = 2 * VENDOR_BUILD_BASE;

/// One past the largest revision-form build id: no id from here up is a version.
const VENDOR_BUILD_END: u64 = 3 * VENDOR_BUILD_BASE;

/// Each `x.y.z` component, and a revision's `n`, is strictly below this (six digits).
const COMPONENT_LIMIT: u64 = 1_000_000;

/// The weight of `major` in an `x.y.z` build id.
const MAJOR_SCALE: u64 = 1_000_000_000_000;

/// A revision form's `x`, `y`, `z` and `s'` are strictly below this (three digits); it is
/// also the weight of `n`.
const SHORT_LIMIT: u64 = 1_000;

/// The weights of a revision form's `x`, `y` and `z`.
const REVISION_MAJOR_SCALE: u64 = 1_000_000_000_000_000;
const REVISION_MINOR_SCALE: u64 = 1_000_000_000_000;
const REVISION_PATCH_SCALE: u64 = 1_000_000_000;

/// Whether `build` names a vendor-direct build rather than an ALab index build.
#[must_use]
pub const fn is_vendor_build(build: u64) -> bool {
    build >= VENDOR_BUILD_BASE
}

/// A canonical vendor version in either grammar. It is its build id, so `Ord` is version
/// order within a grammar and two spellings of one version cannot exist.
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
        Some(Self(
            VENDOR_BUILD_BASE + major * MAJOR_SCALE + minor * COMPONENT_LIMIT + patch,
        ))
    }

    /// `major.minor.patch-R<revision>[.<sub>]`, or `None` when `major`, `minor` or `patch`
    /// is above 999, `revision` above 999 999 or `sub` above 998. Never clamps.
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

    /// Parse `x.y.z` or `x.y.z-R<n>[.<s>]`, every number ASCII decimal without a leading
    /// zero (a lone `0` is fine) and within its grammar's range. Anything else is refused:
    /// a version with two spellings would have two build ids.
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

    /// The version a store build id names, or `None` for an ALab build number or an id
    /// past the revision tier.
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

    // Each field is `id / weight % limit`: a tier's base is a multiple of every weight
    // times its limit, so it drops out.

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

    /// Revision-form fields `(x, y, z, n, s)`: the derived tuple order is the version
    /// order (`None` below `Some(0)`, as `s'` is).
    type Fields = (u32, u32, u32, u32, Option<u32>);

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap_or_else(|| panic!("{text} should parse"))
    }

    fn revision(f: Fields) -> Version {
        Version::with_revision(f.0, f.1, f.2, f.3, f.4).unwrap_or_else(|| panic!("{f:?}"))
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

    /// Revision-form fields biased toward the edges (999, 999_999, `.998`, no `.<s>`).
    fn revision_samples() -> Vec<Fields> {
        let small = [0u32, 1, 9, 10, 998, 999];
        let revisions = [0u32, 1, 6297, 6298, 999_999];
        let subs = [None, Some(0), Some(1), Some(998)];
        let mut out = Vec::new();
        for &a in &small {
            for &b in &small {
                for &c in &small {
                    for &n in &revisions {
                        for &s in &subs {
                            out.push((a, b, c, n, s));
                        }
                    }
                }
            }
        }
        let mut next = stream(0x2545_f491_4f6c_dd1d);
        for _ in 0..4000 {
            let mut small = || (next() % 1000) as u32;
            let (a, b, c) = (small(), small(), small());
            let s = small().checked_sub(1);
            out.push((a, b, c, (next() % COMPONENT_LIMIT) as u32, s));
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
    }

    /// Installed claude and codex build ids decode exactly as the `x.y.z`-only decoder
    /// read them.
    #[test]
    fn the_installed_builds_decode_unchanged() {
        for (id, text, fields) in [
            (1_000_002_000_001_000_301, "2.1.301", (2, 1, 301)),
            (1_000_000_000_162_000_001, "0.162.1", (0, 162, 1)),
        ] {
            let ver = Version::from_build_id(id).unwrap();
            assert_eq!(ver.to_string(), text);
            assert_eq!((ver.major(), ver.minor(), ver.patch()), fields);
            assert!(!ver.has_revision());
            assert_eq!(ver.revision(), None);
            assert_eq!(v(text).build_id(), id);
        }
    }

    /// A revision-form head and its `.<s>`-less spelling each keep their exact text.
    #[test]
    fn the_revision_form_round_trips_its_spelling() {
        let head = v("1.4.9-R6297.1");
        assert_eq!(head.build_id(), 2_001_004_009_006_297_002);
        assert_eq!(head.to_string(), "1.4.9-R6297.1");
        assert_eq!((head.major(), head.minor(), head.patch()), (1, 4, 9));
        assert_eq!(head.revision(), Some((6297, Some(1))));
        assert!(head.has_revision());
        let bare = v("1.4.9-R6297");
        assert_eq!(bare.build_id(), 2_001_004_009_006_297_000);
        assert_eq!(bare.to_string(), "1.4.9-R6297");
        assert_eq!(bare.revision(), Some((6297, None)));
        assert_eq!(Version::with_revision(1, 4, 9, 6297, Some(1)), Some(head));
    }

    #[test]
    fn the_revision_form_orders_by_revision_then_suffix() {
        let chain = [
            "1.4.9-R6297",
            "1.4.9-R6297.0",
            "1.4.9-R6297.1",
            "1.4.9-R6298",
            "1.4.10-R1",
        ];
        for pair in chain.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
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
        let all = revision_samples();
        for _ in 0..20_000 {
            let a = all[(next() % all.len() as u64) as usize];
            let b = all[(next() % all.len() as u64) as usize];
            let (va, vb) = (revision(a), revision(b));
            assert_eq!(a.cmp(&b), va.cmp(&vb), "{a:?} vs {b:?}");
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
            assert!(!ver.has_revision());
        }
        for f in revision_samples() {
            let ver = revision(f);
            assert_eq!((ver.major(), ver.minor(), ver.patch()), (f.0, f.1, f.2));
            assert_eq!(ver.revision(), Some((f.3, f.4)));
            assert_eq!(Version::from_build_id(ver.build_id()), Some(ver));
            assert_eq!(Version::parse(&ver.to_string()), Some(ver));
            assert!(is_vendor_build(ver.build_id()));
            assert!(ver.has_revision());
        }
    }

    #[test]
    fn above_every_alab_build_and_below_i64_max() {
        let lowest = Version::new(0, 0, 0).unwrap().build_id();
        let highest = revision((999, 999, 999, 999_999, Some(998))).build_id();
        for alab in [0u64, 1, 8595, 20_065, 2_026_091_901, 9_999_999_999] {
            assert!(alab < lowest, "{alab}");
            assert!(!is_vendor_build(alab));
            assert_eq!(Version::from_build_id(alab), None);
        }
        assert!(highest < i64::MAX as u64);
        assert!(i64::try_from(highest).is_ok());
    }

    /// The two tiers tile `[10^18, 3·10^18)` with nothing between them: each id there is
    /// one version whose text names it again, and no id outside is a version.
    #[test]
    fn the_tiers_tile_the_vendor_id_range_and_nothing_else_decodes() {
        let at = |id: u64| Version::from_build_id(id).map(|v| v.to_string());
        assert_eq!(at(VENDOR_BUILD_BASE - 1), None);
        assert_eq!(at(VENDOR_BUILD_BASE).as_deref(), Some("0.0.0"));
        assert_eq!(
            at(REVISION_BASE - 1).as_deref(),
            Some("999999.999999.999999")
        );
        assert_eq!(at(REVISION_BASE).as_deref(), Some("0.0.0-R0"));
        assert_eq!(
            at(VENDOR_BUILD_END - 1).as_deref(),
            Some("999.999.999-R999999.998")
        );
        assert_eq!(at(VENDOR_BUILD_END), None);
        assert_eq!(at(u64::MAX), None);
        let mut next = stream(7);
        for _ in 0..20_000 {
            for id in [next(), VENDOR_BUILD_BASE + next() % (2 * VENDOR_BUILD_BASE)] {
                let inside = (VENDOR_BUILD_BASE..VENDOR_BUILD_END).contains(&id);
                match Version::from_build_id(id) {
                    Some(ver) => {
                        assert!(inside, "{id} decoded outside both tiers");
                        assert_eq!(ver.build_id(), id);
                        assert_eq!(Version::parse(&ver.to_string()), Some(ver), "{id}");
                        assert_eq!(ver.has_revision(), id >= REVISION_BASE, "{id}");
                    }
                    None => assert!(!inside, "{id} is in a tier but did not decode"),
                }
            }
        }
    }

    #[test]
    fn bounds_and_non_canonical_spellings_are_refused() {
        assert_eq!(Version::new(1_000_000, 0, 0), None);
        assert_eq!(Version::new(0, 1_000_000, 0), None);
        assert_eq!(Version::new(0, 0, 1_000_000), None);
        assert_eq!(Version::new(u32::MAX, u32::MAX, u32::MAX), None);
        assert_eq!(Version::with_revision(1_000, 0, 0, 0, None), None);
        assert_eq!(Version::with_revision(0, 1_000, 0, 0, None), None);
        assert_eq!(Version::with_revision(0, 0, 1_000, 0, None), None);
        assert_eq!(Version::with_revision(0, 0, 0, 1_000_000, None), None);
        assert_eq!(Version::with_revision(0, 0, 0, 0, Some(999)), None);
        assert_eq!(Version::with_revision(0, 0, 0, 0, Some(u32::MAX)), None);
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
            "1.4.9-r6297",
            "1.4.9-R",
            "1.4.9-R01",
            "1.4.9-R6297.1.2",
            "01.4.9-R1",
            "1.04.9-R1",
            "1000.0.0-R1",
            "0.1000.0-R1",
            "0.0.1000-R1",
            "1.4.9-R1000000",
            "1.4.9-R6297.999",
            "1.4.9-R6297.01",
            "1.4.9-R6297.",
            "1.4.9-R.1",
            "1.4-R6297",
            "1.4.9.R6297",
            "1.4.9R6297",
            "1.4.9--R6297",
            "1.4.9-R6297-R1",
            "1.4.9-R+1",
            "1.4.9-R6297 ",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad:?} must be refused");
        }
    }

    #[test]
    fn serde_spells_the_version_as_text_and_refuses_a_bad_one() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Row {
            v: Version,
        }
        for text in ["2.1.280", "1.4.9-R6297.1", "1.4.9-R6297"] {
            let row = Row { v: v(text) };
            let toml = aterm_toml::to_string(&row).unwrap();
            assert_eq!(toml.trim(), format!("v = \"{text}\""));
            assert_eq!(aterm_toml::from_str::<Row>(&toml).unwrap(), row);
        }
        assert!(aterm_toml::from_str::<Row>("v = \"2.1.08\"").is_err());
        assert!(aterm_toml::from_str::<Row>("v = \"1.4.9-R06297\"").is_err());
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
