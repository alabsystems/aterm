// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::bonk_sound_gain;

/// Each `None` case flips one term of the audible precondition.
#[test]
fn bonk_gain_policy_gates_master_focus_knob_motion_and_volume() {
    assert_eq!(
        bonk_sound_gain(true, true, true, false, 0.4),
        Some(0.4),
        "precondition: the shipped posture must actually bonk",
    );
    assert_eq!(
        bonk_sound_gain(false, true, true, false, 0.4),
        None,
        "raw focus"
    );
    assert_eq!(
        bonk_sound_gain(true, false, true, false, 0.4),
        None,
        "Music effects master off silences the bonk with the knob still on"
    );
    assert_eq!(
        bonk_sound_gain(true, true, false, false, 0.4),
        None,
        "bonk knob"
    );
    assert_eq!(
        bonk_sound_gain(true, true, true, true, 0.4),
        None,
        "reduced motion"
    );
    assert_eq!(
        bonk_sound_gain(true, true, true, false, 0.0),
        None,
        "zero volume"
    );
}
