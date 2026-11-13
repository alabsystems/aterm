// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The forms: bare reads, `seed=<u64>` pins, `seed=off` unpins; junk
/// and a release build are refused.
#[test]
fn the_card_verb_parses_its_forms() {
    assert_eq!(parse("", true), Ok(Asked::Read));
    assert_eq!(parse("seed=42", true), Ok(Asked::Pin(Some(42))));
    assert_eq!(parse(" seed=off ", true), Ok(Asked::Pin(None)));
    assert!(parse("seed=-1", true).is_err());
    assert!(parse("seed", true).is_err());
    assert!(parse("seed=1 seed=2", true).is_err());
    assert!(parse("stars=3", true).is_err());
    assert!(parse("seed=42", false).is_err(), "a release build refuses");
    assert_eq!(line(Some(7), 2), "seed=7 rearmed=2");
    assert_eq!(line(None, 0), "seed=off rearmed=0");
}
