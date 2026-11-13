// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use atpkg::Channel;
use atpkg_publish::pins;
use atpkg_publish::triple::{
    AARCH64_APPLE_DARWIN, AARCH64_UNKNOWN_LINUX_GNU, X86_64_APPLE_DARWIN, X86_64_UNKNOWN_LINUX_GNU,
};

fn channel(pin: &[(&str, u64)], overlays: &[(&str, &[(&str, u64)])]) -> Channel {
    let map = |kv: &[(&str, u64)]| {
        kv.iter()
            .map(|(k, v)| ((*k).to_string(), *v))
            .collect::<BTreeMap<_, _>>()
    };
    Channel {
        name: "stable".into(),
        channel_build: 1,
        min_build: 0,
        min_build_by_program: BTreeMap::new(),
        yanked: Vec::new(),
        pin: map(pin),
        pin_by_target: overlays
            .iter()
            .map(|(t, kv)| ((*t).to_string(), map(kv)))
            .collect(),
    }
}

#[test]
fn a_target_the_overlay_names_resolves_through_it() {
    let c = channel(
        &[("ay", 6255), ("trust-ir", 1066)],
        &[(
            "x86_64-unknown-linux-gnu",
            &[("trust-ir", 1100), ("onlylinux", 5)],
        )],
    );
    let linux = pins::for_targets(&c, &[X86_64_UNKNOWN_LINUX_GNU]).expect("resolves");
    assert_eq!(linux.get("trust-ir"), Some(&1100));
    assert_eq!(linux.get("onlylinux"), Some(&5));
    assert_eq!(linux.get("ay"), Some(&6255));
    let mac = pins::for_targets(&c, &[AARCH64_APPLE_DARWIN]).expect("resolves");
    assert_eq!(mac.get("trust-ir"), Some(&1066));
    assert!(
        !mac.contains_key("onlylinux"),
        "another target's overlay never leaks"
    );
}

#[test]
fn targets_that_agree_resolve_together_and_targets_that_diverge_are_refused() {
    let c = channel(
        &[("ay", 1)],
        &[
            ("x86_64-unknown-linux-gnu", &[("ay", 2), ("nn", 3)]),
            ("aarch64-unknown-linux-gnu", &[("ay", 2)]),
        ],
    );
    let both = pins::for_targets(&c, &[X86_64_UNKNOWN_LINUX_GNU, AARCH64_UNKNOWN_LINUX_GNU])
        .expect("agree");
    assert_eq!(
        both,
        BTreeMap::from([("ay".to_string(), 2), ("nn".to_string(), 3)])
    );
    pins::for_targets(&c, &[AARCH64_APPLE_DARWIN, X86_64_UNKNOWN_LINUX_GNU]).expect_err("diverge");
    assert!(
        pins::for_targets(&c, &[]).is_err(),
        "a lane must name its targets"
    );
}

#[test]
fn the_union_is_every_build_some_client_resolves() {
    let c = channel(
        &[("ay", 8256)],
        &[
            ("x86_64-unknown-linux-gnu", &[("ay", 9001)]),
            ("aarch64-unknown-linux-gnu", &[("ay", 8256)]),
        ],
    );
    assert_eq!(
        pins::union(&c),
        BTreeSet::from([("ay".to_string(), 8256), ("ay".to_string(), 9001)])
    );
}

#[test]
fn per_target_resolution_keeps_an_overlay_ahead_apart_and_floors_every_cell() {
    let c = channel(
        &[("astream", 1356), ("nn", 195)],
        &[("aarch64-apple-darwin", &[("astream", 1359)])],
    );
    let apple = [AARCH64_APPLE_DARWIN, X86_64_APPLE_DARWIN];
    pins::for_targets(&c, &apple).expect_err("one build cannot carry both");
    let by = pins::by_target(&c, &apple).expect("resolves per target");
    assert_eq!(
        by.get("astream"),
        Some(&BTreeMap::from([
            (AARCH64_APPLE_DARWIN, 1359),
            (X86_64_APPLE_DARWIN, 1356)
        ]))
    );
    assert_eq!(
        pins::lowest(&by),
        BTreeMap::from([("astream".to_string(), 1356), ("nn".to_string(), 195)])
    );
    let floors = pins::floors(&by);
    assert_eq!(floors.get("astream@aarch64-apple-darwin"), Some(&1359));
    assert_eq!(floors.get("astream@x86_64-apple-darwin"), Some(&1356));
    assert_eq!(floors.get("astream"), Some(&1356));
    assert_eq!(floors.len(), 6);
    assert!(
        pins::by_target(&c, &[]).is_err(),
        "a lane must name its targets"
    );
}
