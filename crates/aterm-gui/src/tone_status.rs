// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `tone` control verb's report of the window's tone tracker and gates.

use aterm_effects::tone::Tone;

/// Everything the `tone` control verb reports.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct ToneStatus {
    /// The classifier's cached verdict, before the knob.
    pub(crate) tone: Tone,
    /// What the drain seams stamp onto events.
    pub(crate) effective: Tone,
    /// `tone_melody` config knob.
    pub(crate) knob: bool,
    /// `trail_sounds` master toggle.
    pub(crate) sounds: bool,
    /// `trail_sound_volume`, already clamped to 0..1.
    pub(crate) volume: f32,
    /// The audio host's state.
    pub(crate) audio: crate::trail_audio::HostState,
    /// Whether tone inference may run at all.
    pub(crate) active: bool,
    /// Chars in the typed window.
    pub(crate) window_chars: usize,
    /// Inferences run since the window was created.
    pub(crate) inferences: u64,
    /// Saturating lifetime count of dropped sound cues.
    pub(crate) dropped: u64,
    /// `None` when a keypress makes a sound, or `Some(r)` with the gate that stops it.
    pub(crate) seam: Option<crate::sound_seam::SeamReason>,
    /// Whether the cursor-glow engine's own key gate is open.
    pub(crate) engine_sound: bool,
    /// Whether the cursor trail is enabled with nonzero brightness.
    pub(crate) trail: bool,
    /// The cursor-effect focus fold.
    pub(crate) focused: bool,
    /// Whether the serious-mode policy allows terminal sound (`false` in serious mode).
    pub(crate) serious_sound: bool,
    /// `full` or `reduced`, as `trail status` spells it.
    pub(crate) motion_stage: &'static str,
    /// The soft load-shed envelope in 0..1.
    pub(crate) shed: f32,
    /// Wedge revivals spent.
    pub(crate) revives: u64,
    /// Device reopens the audio worker has left.
    pub(crate) reopens_left: u8,
}

impl ToneStatus {
    /// The wire tail after `OK `: one line of `key=value` pairs in a fixed order.
    pub(crate) fn line(&self) -> String {
        format!(
            "tone={} effective={} knob={} sounds={} volume={:.2} audio={} \
             active={} window_chars={} inferences={} dropped={} \
             seam={} engine_sound={} trail={} focused={} serious_sound={} \
             motion_stage={} shed={:.2} revives={} reopens_left={}",
            self.tone.label(),
            self.effective.label(),
            if self.knob { "on" } else { "off" },
            if self.sounds { "on" } else { "off" },
            self.volume,
            self.audio.label(),
            self.active,
            self.window_chars,
            self.inferences,
            self.dropped,
            crate::sound_seam::seam_field(self.seam),
            if self.engine_sound { "open" } else { "closed" },
            if self.trail { "on" } else { "off" },
            self.focused,
            self.serious_sound,
            self.motion_stage,
            self.shed,
            self.revives,
            self.reopens_left,
        )
    }
}

#[cfg(test)]
mod tests {
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
}
