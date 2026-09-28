// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! UAX #9 conformance against the Unicode Character Database's own test data.
//!
//! `tests/data/` carries a deterministic SUBSET of `BidiTest.txt` and
//! `BidiCharacterTest.txt` (UCD 18.0.0; Unicode License v3, UNICODE-LICENSE.txt
//! at the repo root): every case of the hand-written sections of
//! BidiCharacterTest (brackets, isolates, overrides, the UAX #9 examples) and an
//! even sample of the generated remainder of both files — see `subset.py`
//! beside the data for how it was cut. Each case checks the resolved level of
//! every character not removed by rule X9 and the visual order through L2.

use aterm_bidi::{
    BaseDirection, BidiClass, Bracket, bidi_class, bracket_of, removed_by_x9, reorder_from_levels,
    resolve_levels_from_classes,
};

fn class_named(name: &str) -> BidiClass {
    use BidiClass::*;
    match name {
        "L" => L,
        "R" => R,
        "AL" => AL,
        "EN" => EN,
        "ES" => ES,
        "ET" => ET,
        "AN" => AN,
        "CS" => CS,
        "NSM" => NSM,
        "BN" => BN,
        "B" => B,
        "S" => S,
        "WS" => WS,
        "ON" => ON,
        "LRE" => LRE,
        "LRO" => LRO,
        "RLE" => RLE,
        "RLO" => RLO,
        "PDF" => PDF,
        "LRI" => LRI,
        "RLI" => RLI,
        "FSI" => FSI,
        "PDI" => PDI,
        other => panic!("unknown bidi class {other}"),
    }
}

/// Compare one resolution against the expected levels (`None` = removed) and
/// visual order (removed characters skipped). Returns a failure description.
fn check(
    classes: &[BidiClass],
    brackets: &[Bracket],
    base: BaseDirection,
    levels: &[Option<u8>],
    order: &[usize],
) -> Option<String> {
    let got = resolve_levels_from_classes(classes, brackets, base);
    for (i, want) in levels.iter().enumerate() {
        if let Some(want) = want
            && got.get(i) != Some(want)
        {
            return Some(format!("levels {got:?} want {levels:?}"));
        }
    }
    let visual: Vec<usize> = reorder_from_levels(&got)
        .into_iter()
        .filter(|&i| !removed_by_x9(classes[i]))
        .collect();
    (visual != order).then(|| format!("order {visual:?} want {order:?} (levels {got:?})"))
}

fn parse_levels(field: &str) -> Vec<Option<u8>> {
    field
        .split_whitespace()
        .map(|t| {
            if t == "x" {
                None
            } else {
                Some(t.parse().expect("level"))
            }
        })
        .collect()
}

fn parse_order(field: &str) -> Vec<usize> {
    field
        .split_whitespace()
        .map(|t| t.parse().expect("index"))
        .collect()
}

#[test]
fn bidi_test_txt_subset_conforms() {
    let text = include_str!("data/BidiTest-subset.txt");
    let (mut levels, mut order) = (Vec::new(), Vec::new());
    let (mut cases, mut failures) = (0usize, Vec::new());
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("@Levels:") {
            levels = parse_levels(rest);
            continue;
        }
        if let Some(rest) = line.strip_prefix("@Reorder:") {
            order = parse_order(rest);
            continue;
        }
        if line.starts_with('@') {
            continue;
        }
        let (input, bits) = line.split_once(';').expect("data line");
        let classes: Vec<BidiClass> = input.split_whitespace().map(class_named).collect();
        let bits = u8::from_str_radix(bits.trim(), 16).expect("bitset");
        for (bit, base) in [
            (1, BaseDirection::Auto),
            (2, BaseDirection::Ltr),
            (4, BaseDirection::Rtl),
        ] {
            if bits & bit == 0 {
                continue;
            }
            cases += 1;
            if let Some(why) = check(&classes, &[], base, &levels, &order) {
                failures.push(format!("line {}: {input} {base:?}: {why}", n + 1));
            }
        }
    }
    assert!(
        cases > 1000,
        "the subset must actually test something ({cases})"
    );
    assert!(
        failures.is_empty(),
        "{} of {cases} BidiTest cases failed; first: {:#?}",
        failures.len(),
        &failures[..failures.len().min(10)]
    );
}

#[test]
fn bidi_character_test_txt_subset_conforms() {
    let text = include_str!("data/BidiCharacterTest-subset.txt");
    let (mut cases, mut failures) = (0usize, Vec::new());
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(';').collect();
        let chars: Vec<char> = fields[0]
            .split_whitespace()
            .map(|h| char::from_u32(u32::from_str_radix(h, 16).expect("hex")).expect("scalar"))
            .collect();
        let base = match fields[1].trim() {
            "0" => BaseDirection::Ltr,
            "1" => BaseDirection::Rtl,
            _ => BaseDirection::Auto,
        };
        let classes: Vec<BidiClass> = chars.iter().map(|&c| bidi_class(c)).collect();
        let brackets: Vec<Bracket> = chars.iter().map(|&c| bracket_of(c)).collect();
        cases += 1;
        if let Some(why) = check(
            &classes,
            &brackets,
            base,
            &parse_levels(fields[3]),
            &parse_order(fields[4]),
        ) {
            failures.push(format!("line {}: {}: {why}", n + 1, fields[0]));
        }
    }
    assert!(
        cases > 1000,
        "the subset must actually test something ({cases})"
    );
    assert!(
        failures.is_empty(),
        "{} of {cases} BidiCharacterTest cases failed; first: {:#?}",
        failures.len(),
        &failures[..failures.len().min(10)]
    );
}
