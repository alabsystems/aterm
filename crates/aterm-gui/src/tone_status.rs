// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `tone` control verb's report: the window's tone tracker read beside every gate
//! this host owns, as plain data so the wire line is headless-testable.

use aterm_effects::tone::Tone;

/// Everything the `tone` control verb reports. Each conjunct of
/// `App::tone_infer_active` is its own field, so a driver can see which gate stopped
/// the mood from steering typing sounds.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct ToneStatus {
    /// The classifier's cached verdict, before the knob.
    pub(crate) tone: Tone,
    /// What the drain seams stamp onto events: `tone` with the knob on, `technical` off.
    pub(crate) effective: Tone,
    /// `tone_melody` config knob.
    pub(crate) knob: bool,
    /// `trail_sounds` master toggle.
    pub(crate) sounds: bool,
    /// `trail_sound_volume`, already clamped to 0..1.
    pub(crate) volume: f32,
    /// The audio host's state (`live` means started, not merely that a channel exists).
    pub(crate) audio: crate::trail_audio::HostState,
    /// `App::tone_infer_active`: whether inference may run at all.
    pub(crate) active: bool,
    /// Chars in the typed window (never the text).
    pub(crate) window_chars: usize,
    /// Inferences run since the window was created. Zero with `active=true` after
    /// typing means the feed is broken.
    pub(crate) inferences: u64,
    /// Saturating lifetime count of dropped sound cues (`TrailAudio::dropped_cues`).
    pub(crate) dropped: u64,
    /// `None`: a keypress now makes a sound; `Some(r)`: the gate that stops it.
    pub(crate) seam: Option<crate::sound_seam::SeamReason>,
    /// `CursorGlow::sound_seam_open()`: the engine's own key gate.
    pub(crate) engine_sound: bool,
    /// [`crate::sound_seam::trail_on`]: `GlowConfig::enabled` and a nonzero brightness.
    pub(crate) trail: bool,
    /// The cursor-effect focus fold; the same value `trail status` prints as `focused=`.
    pub(crate) focused: bool,
    /// `SeriousEffect::TerminalSound`, spelled as `streak status` spells it.
    pub(crate) serious_sound: bool,
    /// `full` or `reduced`, spelled as `trail status` spells it.
    pub(crate) motion_stage: &'static str,
    /// The soft load-shed envelope in 0..1 (1.0 = not shedding).
    pub(crate) shed: f32,
    /// Wedge revivals spent; a revival replaces the worker, so read it beside `dropped`.
    pub(crate) revives: u64,
    /// Device reopens the audio worker has left (`trail_audio::REOPEN_BUDGET` at birth);
    /// `0` beside `audio=failed` is terminal.
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

    /// Every gate is its own field, in a fixed order, using `Tone::label` spellings.
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

    /// A stuck worker is spelled `wedged`, never `live` or `inert`.
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
