// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn channel(min_build: u64, pin: &[(&str, u64)], yanked: &[&str]) -> Channel {
    Channel {
        name: "stable".into(),
        channel_build: 1,
        min_build,
        min_build_by_program: Default::default(),
        yanked: yanked.iter().map(|s| (*s).to_string()).collect(),
        pin: pin.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
        pin_by_target: Default::default(),
    }
}

fn channel_floors(
    min_build: u64,
    per_program: &[(&str, u64)],
    pin: &[(&str, u64)],
    yanked: &[&str],
) -> Channel {
    Channel {
        min_build_by_program: per_program
            .iter()
            .map(|(k, v)| ((*k).to_string(), *v))
            .collect(),
        ..channel(min_build, pin, yanked)
    }
}

#[test]
fn is_yanked_matches_program_at_build() {
    let ch = channel(
        0,
        &[],
        &[
            "trust@4790",
            "ay@17",
            "zero@0",
            "plus@+4790",
            "pad@04790",
            "garbage",
            "ny@notanum",
        ],
    );
    assert!(is_yanked(&ch, "trust", 4790));
    assert!(is_yanked(&ch, "ay", 17));
    assert!(is_yanked(&ch, "zero", 0));
    assert!(!is_yanked(&ch, "trust", 4791));
    assert!(!is_yanked(&ch, "ay", 18));
    assert!(!is_yanked(&ch, "plus", 4790)); // non-canonical "+4790" rejected
    assert!(!is_yanked(&ch, "pad", 4790)); // non-canonical leading-zero "04790" rejected
    assert!(!is_yanked(&ch, "ny", 0)); // malformed "ny@notanum" never matches
}

#[test]
fn fresh_install_and_force_upgrade_yield_install() {
    let ch = channel(10, &[("ay", 18)], &[]);
    assert_eq!(decide(&ch, "ay", None), ApplyDecision::Install); // fresh
    assert_eq!(decide(&ch, "ay", Some(17)), ApplyDecision::Install); // upgrade
    assert_eq!(decide(&ch, "ay", Some(18)), ApplyDecision::UpToDate); // already current
}

#[test]
fn pin_below_floor_or_yanked_tombstones() {
    let low = channel(120, &[("ay", 100)], &[]);
    assert_eq!(decide(&low, "ay", None), ApplyDecision::Tombstone);
    assert_eq!(decide(&low, "ay", Some(100)), ApplyDecision::Tombstone);
    let yanked = channel(0, &[("trust", 4790)], &["trust@4790"]);
    assert_eq!(
        decide(&yanked, "trust", Some(4790)),
        ApplyDecision::Tombstone
    );
    assert_eq!(decide(&yanked, "trust", None), ApplyDecision::Tombstone);
}

#[test]
fn current_build_ok_gates_a_local_pin_hold() {
    let yanked = channel(0, &[("trust", 4800)], &["trust@4790"]);
    assert!(
        !current_build_ok(&yanked, "trust", Some(4790)),
        "yanked current build"
    );
    assert!(
        current_build_ok(&yanked, "trust", Some(4800)),
        "valid current build"
    );
    let floored = channel(100, &[("ay", 120)], &[]);
    assert!(!current_build_ok(&floored, "ay", Some(90)), "below floor");
    assert!(
        current_build_ok(&floored, "ay", Some(110)),
        "at/above floor"
    );
    assert!(current_build_ok(&floored, "ay", None));
}

#[test]
fn a_program_floor_never_tombstones_a_program_it_does_not_name() {
    let ch = channel_floors(
        0,
        &[("trust", 7000)],
        &[("trust", 7100), ("nn", 108), ("ty", 3007)],
        &[],
    );
    assert_eq!(decide(&ch, "trust", Some(6808)), ApplyDecision::Install);
    assert!(!current_build_ok(&ch, "trust", Some(6808)));
    assert_eq!(decide(&ch, "trust", Some(7100)), ApplyDecision::UpToDate);
    assert_eq!(decide(&ch, "nn", Some(108)), ApplyDecision::UpToDate);
    assert_eq!(decide(&ch, "ty", Some(3007)), ApplyDecision::UpToDate);
    assert_eq!(decide(&ch, "nn", None), ApplyDecision::Install);
    assert!(current_build_ok(&ch, "nn", Some(108)));
    assert!(current_build_ok(&ch, "ty", Some(3007)));
}

#[test]
fn a_program_below_its_own_floor_tombstones_alone() {
    let ch = channel_floors(0, &[("trust", 7000)], &[("trust", 6808), ("nn", 108)], &[]);
    assert_eq!(decide(&ch, "trust", Some(6808)), ApplyDecision::Tombstone);
    assert_eq!(decide(&ch, "trust", None), ApplyDecision::Tombstone);
    assert_eq!(decide(&ch, "nn", None), ApplyDecision::Install);
    assert_eq!(decide(&ch, "nn", Some(108)), ApplyDecision::UpToDate);
}

#[test]
fn a_program_entry_can_only_raise_the_channel_wide_floor() {
    let ch = channel_floors(120, &[("ay", 50)], &[("ay", 130), ("ny", 130)], &[]);
    assert_eq!(ch.min_build_for("ay"), 120);
    assert!(!current_build_ok(&ch, "ay", Some(100)));
    assert_eq!(decide(&ch, "ay", Some(100)), ApplyDecision::Install);
    assert_eq!(ch.min_build_for("ny"), 120);
    assert!(!current_build_ok(&ch, "ny", Some(119)));
}

#[test]
fn unpinned_program_is_not_part_of_the_channel() {
    let ch = channel(0, &[("ay", 18)], &[]);
    assert_eq!(decide(&ch, "dotfiles", None), ApplyDecision::NotPinned);
}
