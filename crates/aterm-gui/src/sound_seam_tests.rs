// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn open() -> SeamInputs {
    SeamInputs {
        sounds_on: true,
        volume: 0.4,
        trail_on: true,
        serious_allows: true,
        focused: true,
        resize_quiet: false,
        host: HostState::Running,
        engine_sound_live: true,
    }
}

fn close(reason: SeamReason) -> SeamInputs {
    let mut i = open();
    match reason {
        SeamReason::SoundsOff => i.sounds_on = false,
        SeamReason::VolumeZero => i.volume = 0.0,
        SeamReason::TrailOff => i.trail_on = false,
        SeamReason::SeriousMode => i.serious_allows = false,
        SeamReason::Unfocused => i.focused = false,
        SeamReason::HostInert => i.host = HostState::Inert,
        SeamReason::HostWedged => i.host = HostState::Wedged,
        SeamReason::ResizeQuiet => i.resize_quiet = true,
        SeamReason::EngineSilent => i.engine_sound_live = false,
    }
    i
}

#[test]
fn the_seam_names_the_gate_that_would_silence_the_key() {
    assert_eq!(seam_field(sound_seam(&open())), "open");

    for reason in SeamReason::ALL {
        let i = close(reason);
        assert_eq!(
            seam_field(sound_seam(&i)),
            format!("closed:{}", reason.token()),
            "closing exactly {reason:?} must report itself"
        );
    }

    let mut both = open();
    both.sounds_on = false;
    both.focused = false;
    assert_eq!(seam_field(sound_seam(&both)), "closed:sounds-off");

    let mut engine_and_knob = open();
    engine_and_knob.engine_sound_live = false;
    engine_and_knob.trail_on = false;
    assert_eq!(seam_field(sound_seam(&engine_and_knob)), "closed:trail-off");
}

#[test]
fn the_push_decision_is_the_seam_with_host_and_engine_pinned_open() {
    for sounds_on in [false, true] {
        for volume in [0.0_f32, 0.4] {
            for serious_allows in [false, true] {
                for resize_quiet in [false, true] {
                    for host in HostState::ALL {
                        let got = crate::app_input::keystroke_click_audible(
                            host,
                            sounds_on,
                            volume,
                            serious_allows,
                            resize_quiet,
                        );
                        let legacy = !matches!(host, HostState::Inert)
                            && sounds_on
                            && volume > 0.0
                            && serious_allows
                            && !resize_quiet;
                        assert_eq!(
                            got, legacy,
                            "{host:?} {sounds_on} {volume} {serious_allows} {resize_quiet}"
                        );
                    }
                }
            }
        }
    }
}
