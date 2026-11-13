// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

type Fields = (u32, u32, u32, u32, Option<u32>);

fn v(text: &str) -> Version {
    Version::parse(text).unwrap_or_else(|| panic!("{text} should parse"))
}

fn revision(f: Fields) -> Version {
    Version::with_revision(f.0, f.1, f.2, f.3, f.4).unwrap_or_else(|| panic!("{f:?}"))
}

fn stream(mut seed: u64) -> impl FnMut() -> u64 {
    move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    }
}

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
        "2024-02-29T23:59:59Z",
        "2000-02-29T00:00:00Z",
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
        "2026-02-29T00:00:00Z",
        "1900-02-29T00:00:00Z",
        "2026-02-31T00:00:00Z",
        "2026-04-31T00:00:00Z",
        "2026-09-21T24:00:00Z",
        "2026-09-21T20:60:00Z",
        "2026-09-21T20:55:60Z",
        "2026-09-21 20:55:27Z",
        "2026-09-21T20:55:27.Z",
        "2026-09-21T20:55:27.1234567890Z",
        "+026-09-21T20:55:27Z",
    ] {
        assert_eq!(BuildDate::parse(bad), None, "{bad:?} must be refused");
    }
}
