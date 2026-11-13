// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{AppActivation, LaunchPosture, launch_posture};

#[test]
fn only_a_cold_windowed_launch_asks_to_activate_and_a_successor_waits_for_commit() {
    let prohibited = LaunchPosture {
        policy: Some(AppActivation::Prohibited),
        activate_on_launch: false,
    };
    let successor = LaunchPosture {
        policy: LaunchPosture::WINIT_DEFAULT.policy,
        activate_on_launch: false,
    };
    for (headless, candidate, want) in [
        (true, false, prohibited),
        (true, true, prohibited),
        (false, true, successor),
        (false, false, LaunchPosture::WINIT_DEFAULT),
    ] {
        assert_eq!(
            launch_posture(headless, candidate),
            want,
            "headless={headless} handoff_candidate={candidate}"
        );
    }
}
