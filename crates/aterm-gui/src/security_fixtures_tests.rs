// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_settings::{debugger_row_copy, devtools_card_copy};

#[test]
fn every_listed_name_is_a_distinct_posture() {
    let postures: Vec<MacosAccess> = NAMES
        .iter()
        .map(|name| named(name).unwrap_or_else(|| panic!("{name} is in the catalog")))
        .collect();
    for (name, access) in NAMES.iter().zip(&postures) {
        let expected = (*name != "no-bundle-id").then_some("com.example.fixture");
        assert_eq!(
            access.bundle_id.as_ref().map(ResetSubject::as_str),
            expected,
            "{name}: the subject is read from the owned fixture bundle"
        );
    }
    for (i, a) in postures.iter().enumerate() {
        for b in &postures[i + 1..] {
            assert_ne!(a, b, "two names, one posture");
        }
    }
    assert!(named("nonsense").is_none());
}

/// ONE PRIVACY GATE (s-24): the host checks the MACOS ACCESS block's Full Disk Access, the
/// DEVELOPER TOOLS card and the debugger row under one gate (`[privacy]`'s `enabled` and
/// `check`; a refused gate leaves the probe `RefusedDisabled`), but every posture once paired
/// a checked block with two cards saying the checks were switched off in aterm.toml. Each
/// posture now keeps the host's coupling, and only the postures that switch the checks off
/// (the negative controls) say so.
#[test]
fn only_the_switched_off_postures_say_the_checks_are_off() {
    const OFF: &str = "switched off in aterm.toml";
    let mut unchecked = Vec::new();
    for name in NAMES {
        let access = named(name).expect("a catalog name");
        let checked = access.probe != ProbeLabel::RefusedDisabled;
        assert_eq!(
            access.devtools.enabled, checked,
            "{name}: the DEVELOPER TOOLS card is checked with the block's probe"
        );
        assert_eq!(
            access.debugger.enabled, checked,
            "{name}: the debugger row is checked with the block's probe"
        );
        assert!(
            access.enabled || !checked,
            "{name}: nothing is checked with [privacy] switched off"
        );
        assert_eq!(
            devtools_card_copy(&access).state.contains(OFF),
            !checked,
            "{name}: the DEVELOPER TOOLS card"
        );
        assert_eq!(
            debugger_row_copy(&access).headline.contains(OFF),
            !checked,
            "{name}: the debugger row"
        );
        if !checked {
            unchecked.push(*name);
        }
    }
    assert_eq!(
        unchecked, CHECKS_OFF,
        "only the postures that switch the checks off leave them off"
    );
}
