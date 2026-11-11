// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::{BTreeMap, BTreeSet};

/// Unicode 18.0.0's `CaseFolding.txt`, the source of [`FOLD`]: its `C` and `S` rows are default
/// simple case folding (`T`, the Turkic rows, and `F`, full folding, are not).
const CASE_FOLDING: &str = include_str!("../tests/fixtures/CaseFolding-18.0.0.txt");

/// The one Turkic-only mapping (`T` in `CaseFolding.txt`), excluded from default simple folding.
const TURKIC_ONLY: &[char] = &['\u{131}'];

/// The four `CaseFolding.txt` `C` and `S` edges whose `std` case mappings are multi-code-point.
const NON_DERIVABLE: &[(char, char)] = &[
    ('\u{0390}', '\u{1fd3}'),
    ('\u{03b0}', '\u{1fe3}'),
    ('\u{fb05}', '\u{fb06}'),
    ('\u{1df95}', '\u{df}'),
];

/// The `C` and `S` rows of [`CASE_FOLDING`], each a code point and its simple fold.
fn simple_folds() -> Vec<(char, char)> {
    let scalar = |hex: &str| {
        u32::from_str_radix(hex, 16)
            .ok()
            .and_then(char::from_u32)
            .expect("CaseFolding.txt names scalar values")
    };
    CASE_FOLDING
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('#').next()?.split(';').map(str::trim);
            let (code, status, fold) = (fields.next()?, fields.next()?, fields.next()?);
            matches!(status, "C" | "S").then(|| (scalar(code), scalar(fold)))
        })
        .collect()
}

fn single_lower(c: char) -> Option<char> {
    let mut it = c.to_lowercase();
    let first = it.next()?;
    it.next().is_none().then_some(first)
}

fn single_upper(c: char) -> Option<char> {
    let mut it = c.to_uppercase();
    let first = it.next()?;
    it.next().is_none().then_some(first)
}

/// Derive every fold orbit from `CaseFolding.txt`'s simple folding and assert [`FOLD`] matches.
#[test]
fn fold_table_is_case_foldings_simple_orbits() {
    let mut parent: BTreeMap<char, char> = BTreeMap::new();
    fn root(parent: &mut BTreeMap<char, char>, x: char) -> char {
        let mut r = x;
        while let Some(&p) = parent.get(&r) {
            if p == r {
                break;
            }
            r = p;
        }
        r
    }
    let folds = simple_folds();
    assert!(
        folds.len() > 1_500,
        "CaseFolding.txt read whole: {}",
        folds.len()
    );
    for (a, b) in folds {
        parent.entry(a).or_insert(a);
        parent.entry(b).or_insert(b);
        let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
        if ra != rb {
            parent.insert(ra, rb);
        }
    }

    let keys: Vec<char> = parent.keys().copied().collect();
    let mut components: BTreeMap<char, BTreeSet<char>> = BTreeMap::new();
    for k in keys {
        let r = root(&mut parent, k);
        components.entry(r).or_default().insert(k);
    }

    let mut expected: Vec<(u32, u32)> = Vec::new();
    for set in components.values() {
        let members: Vec<char> = set.iter().copied().collect();
        for (i, &c) in members.iter().enumerate() {
            expected.push((c as u32, members[(i + 1) % members.len()] as u32));
        }
    }
    expected.sort_unstable();
    assert_eq!(
        FOLD, expected,
        "the baked fold table is no longer Unicode 18.0.0's simple case-folding orbits \
         (CaseFolding.txt's C and S rows)"
    );

    for &(a, b) in NON_DERIVABLE {
        let orbit: BTreeSet<char> = fold_orbit(a).collect();
        assert!(
            orbit.contains(&b),
            "U+{:04X} is missing the non-derivable fold edge to U+{:04X}",
            a as u32,
            b as u32
        );
    }
}

/// The toolchain's own case mappings never split a [`FOLD`] orbit: a single-code-point
/// `to_lowercase` or `to_uppercase` edge between two code points the table folds stays inside
/// one orbit. A `std` built from older or newer Unicode tables than the table's knows fewer or
/// more cased code points, so an edge reaching a code point the table leaves alone is not judged.
#[test]
fn std_never_joins_two_fold_orbits() {
    let folded = |c: char| FOLD.binary_search_by_key(&(c as u32), |&(k, _)| k).is_ok();
    let mut judged = 0_usize;
    for cp in 0u32..0x11_0000 {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        for mapped in [single_lower(c), single_upper(c)].into_iter().flatten() {
            if mapped == c
                || TURKIC_ONLY.contains(&c)
                || TURKIC_ONLY.contains(&mapped)
                || !folded(c)
                || !folded(mapped)
            {
                continue;
            }
            judged += 1;
            assert!(
                fold_orbit(c).any(|o| o == mapped),
                "std maps U+{:04X} to U+{:04X}, outside its fold orbit",
                c as u32,
                mapped as u32
            );
        }
    }
    assert!(
        judged > 2_000,
        "std's case mappings reached the table: {judged}"
    );
}

/// Every fold orbit is symmetric and closed.
#[test]
fn fold_orbits_are_symmetric() {
    for &(cp, _) in FOLD {
        let c = char::from_u32(cp).expect("table holds scalar values");
        let orbit: BTreeSet<char> = fold_orbit(c).collect();
        assert!(orbit.contains(&c), "U+{cp:04X} missing from its own orbit");
        for member in &orbit {
            let back: BTreeSet<char> = fold_orbit(*member).collect();
            assert_eq!(orbit, back, "orbit of U+{cp:04X} is not symmetric");
        }
    }
}

/// Spot-checks fold orbits used by built-in patterns and search.
#[test]
fn named_orbits() {
    let orbit = |c: char| -> Vec<char> {
        let mut v: Vec<char> = fold_orbit(c).collect();
        v.sort_unstable();
        v
    };
    assert_eq!(orbit('k'), vec!['K', 'k', '\u{212a}']);
    assert_eq!(orbit('\u{212a}'), vec!['K', 'k', '\u{212a}']);
    assert_eq!(orbit('s'), vec!['S', 's', '\u{17f}']);
    assert_eq!(orbit('\u{3c3}'), vec!['\u{3a3}', '\u{3c2}', '\u{3c3}']);
    assert_eq!(orbit('\u{df}'), vec!['\u{df}', '\u{1e9e}', '\u{1df95}']);
    assert_eq!(
        orbit('\u{131}'),
        vec!['\u{131}'],
        "Turkic dotless i stands alone"
    );
    assert_eq!(orbit('i'), vec!['I', 'i']);
    assert_eq!(orbit('7'), vec!['7'], "uncased code points are singletons");
}

/// Range folding includes orbit members outside the range.
#[test]
fn range_folding_reaches_outside_the_range() {
    let mut out = Vec::new();
    fold_range('a', 'z', &mut out);
    assert!(
        out.contains(&('\u{212a}', '\u{212a}')),
        "KELVIN SIGN missing"
    );
    assert!(out.contains(&('\u{17f}', '\u{17f}')), "LONG S missing");
    assert!(out.contains(&('A', 'A')));
    assert!(!out.iter().any(|&(lo, _)| lo.is_ascii_lowercase()));
}

#[test]
fn perl_classes_are_unicode_aware() {
    assert!(is_digit('7') && is_digit('\u{0661}') && !is_digit('\u{00bd}'));
    assert!(is_word('_') && is_word('e') && is_word('\u{0301}') && !is_word('-'));
    assert!(is_space(' ') && is_space('\u{00a0}') && !is_space('x'));
}

/// Range tables are sorted, disjoint, non-adjacent, and bounded by `U+10FFFF`.
#[test]
fn range_tables_are_well_formed() {
    for (name, table) in [("ND", ND), ("MARK", MARK), ("PC", PC)] {
        assert!(!table.is_empty(), "{name}: table must not be empty");
        for &(lo, hi) in table {
            assert!(lo <= hi, "{name}: inverted range ({lo:#x}, {hi:#x})");
            assert!(
                hi <= 0x10_FFFF,
                "{name}: range ({lo:#x}, {hi:#x}) exceeds U+10FFFF"
            );
        }
        for w in table.windows(2) {
            assert!(w[0].1 + 1 < w[1].0, "{name}: {:?} touches {:?}", w[0], w[1]);
        }
    }
    for &(cp, next) in FOLD {
        assert!(
            cp <= 0x10_FFFF && next <= 0x10_FFFF,
            "FOLD entry ({cp:#x}, {next:#x}) exceeds U+10FFFF"
        );
    }
    for w in FOLD.windows(2) {
        assert!(w[0].0 < w[1].0, "FOLD is not sorted by code point");
    }
}
