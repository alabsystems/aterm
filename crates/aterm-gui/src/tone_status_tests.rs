// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn status_line_reports_every_gate_in_a_fixed_order() {
    let status = ToneStatus {
        tone: Tone::Frustrated,
        effective: Tone::Technical,
        knob: false,
        sounds: true,
        volume: 0.4,
        audio: crate::trail_audio::HostState::Running,
        active: false,
        window_chars: 27,
        inferences: 5,
        dropped: 0,
        seam: None,
        engine_sound: true,
        trail: true,
        focused: true,
        serious_sound: true,
        motion_stage: "full",
        shed: 1.0,
        revives: 0,
        reopens_left: 6,
    };
    assert_eq!(
        status.line(),
        "tone=frustrated effective=technical knob=off sounds=on volume=0.40 \
         audio=live active=false window_chars=27 inferences=5 dropped=0 \
         seam=open engine_sound=open trail=on focused=true serious_sound=true \
         motion_stage=full shed=1.00 revives=0 reopens_left=6",
    );
}

#[test]
fn status_line_spells_a_wedged_host_wedged() {
    let status = ToneStatus {
        tone: Tone::Technical,
        effective: Tone::Technical,
        knob: true,
        sounds: true,
        volume: 0.4,
        audio: crate::trail_audio::HostState::Wedged,
        active: false,
        window_chars: 0,
        inferences: 104,
        dropped: 4_096,
        seam: Some(crate::sound_seam::SeamReason::HostWedged),
        engine_sound: true,
        trail: true,
        focused: true,
        serious_sound: true,
        motion_stage: "full",
        shed: 1.0,
        revives: 2,
        reopens_left: 4,
    };
    assert_eq!(
        status.line(),
        "tone=technical effective=technical knob=on sounds=on volume=0.40 \
         audio=wedged active=false window_chars=0 inferences=104 dropped=4096 \
         seam=closed:host-wedged engine_sound=open trail=on focused=true \
         serious_sound=true motion_stage=full shed=1.00 revives=2 reopens_left=4",
    );
}
