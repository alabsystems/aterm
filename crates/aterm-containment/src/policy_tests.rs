// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn test_monotonic_capabilities() {
    let modes = [
        ContainmentMode::Containment,
        ContainmentMode::Safety,
        ContainmentMode::User,
    ];
    for (i, &lower) in modes.iter().enumerate() {
        for &higher in &modes[i..] {
            let cl = ContainmentPolicy::capabilities(lower);
            let ch = ContainmentPolicy::capabilities(higher);
            assert!(
                cl.network <= ch.network,
                "network: {lower} should <= {higher}"
            );
            assert!(cl.fs <= ch.fs, "fs: {lower} should <= {higher}");
            assert!(
                cl.process <= ch.process,
                "process: {lower} should <= {higher}"
            );
        }
    }
}

#[test]
fn test_numeric_policy_table() {
    let table: [(u8, [u8; 3]); 3] = [
        (0, [0, 0, 0]), // Containment
        (1, [1, 1, 1]), // Safety
        (2, [1, 2, 2]), // User
    ];

    let modes = [
        ContainmentMode::Containment,
        ContainmentMode::Safety,
        ContainmentMode::User,
    ];

    for (mode, &(expected_level, ref expected_caps)) in modes.iter().zip(table.iter()) {
        assert_eq!(mode.level(), expected_level, "mode {mode} level mismatch");

        let caps = ContainmentPolicy::capabilities(*mode);
        assert_eq!(caps.network as u8, expected_caps[0], "network({mode})");
        assert_eq!(caps.fs as u8, expected_caps[1], "fs({mode})");
        assert_eq!(caps.process as u8, expected_caps[2], "process({mode})");
    }
}
