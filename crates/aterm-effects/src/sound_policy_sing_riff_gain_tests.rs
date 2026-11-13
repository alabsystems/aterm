// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::sing_riff_gain;

/// The riff's own switch is independent of the trail-sound master, and
/// every other term of the trail-sound law still applies to the riff, so
/// the switch is not a bypass.
#[test]
fn the_riff_switch_silences_the_song_and_stays_under_focus_master_and_volume() {
    assert_eq!(
        sing_riff_gain(true, true, true, 0.4),
        Some(0.4),
        "precondition: the shipped default posture must actually sing",
    );
    assert_eq!(sing_riff_gain(true, true, false, 0.4), None, "riff switch");
    assert_eq!(sing_riff_gain(false, true, true, 0.4), None, "raw focus");
    assert_eq!(sing_riff_gain(true, false, true, 0.4), None, "trail_sounds");
    assert_eq!(sing_riff_gain(true, true, true, 0.0), None, "volume");
}
