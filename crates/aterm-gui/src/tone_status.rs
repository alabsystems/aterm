// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `tone` control verb's report: the window's tone tracker
//! ([`aterm_effects::tone_infer::ToneTracker`]) read beside every gate this
//! host owns — the knob, the trail-sound master, the audio host, the key
//! seam — so the wire line is a pure function of plain data.

use aterm_effects::tone::Tone;

/// Everything the `tone` control verb reports, gathered from the App in one
/// place so the WIRE FORMAT is a pure function of plain data (no `App`, no
/// window, no audio device) and can be pinned by a headless test.
///
/// The fields are chosen to answer the ONE question a driver actually asks —
/// "is the mood steering my typing sounds, and if not, which gate stopped
/// it?" — so every conjunct of `App::tone_infer_active` appears as its own
/// field rather than being collapsed into the single `active` verdict.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct ToneStatus {
    /// The classifier's cached verdict, BEFORE the knob.
    pub(crate) tone: Tone,
    /// What the drain seams actually stamp onto events (`tone` with the knob
    /// on, `technical` with it off) — the field to read when asking what the
    /// synth HEARD, as opposed to what the model thought.
    pub(crate) effective: Tone,
    /// `tone_melody` config knob.
    pub(crate) knob: bool,
    /// `trail_sounds` master toggle.
    pub(crate) sounds: bool,
    /// `trail_sound_volume`, already clamped to 0..1.
    pub(crate) volume: f32,
    /// The audio host's honest state — not merely whether a channel exists
    /// (see [`crate::trail_audio::HostState`]).
    pub(crate) audio: crate::trail_audio::HostState,
    /// The AND of the four above: whether inference may run at all.
    pub(crate) active: bool,
    /// Chars in the typed window (never the text — see
    /// [`aterm_effects::tone_infer::ToneTracker::window_chars`]).
    pub(crate) window_chars: usize,
    /// Inferences run since the window was created. ZERO with a non-neutral
    /// `tone` is impossible; zero with `active=true` after typing means the
    /// feed is broken, not that your prose reads technical.
    pub(crate) inferences: u64,
    /// Saturating count of sound cues dropped on a full ingress channel over
    /// the host's lifetime. A handful is a burst; thousands alongside
    /// `audio=wedged` is the wedge's cost, printed instead of private.
    pub(crate) dropped: u64,
    /// THE ANSWER: `None` ⇒ a keypress right now makes a sound; `Some(r)` ⇒
    /// the gate that stops it. Carried as the `Copy` enum rather than a
    /// `String` so this struct keeps `Copy` for the wire tests; `line()`
    /// renders it.
    pub(crate) seam: Option<crate::sound_seam::SeamReason>,
    /// `CursorGlow::sound_seam_open()` — the engine's own key gate, the term
    /// that was MISSING while a load-shed frame and macOS `Reduce Motion`
    /// silenced typing with `audio=live` on the row.
    pub(crate) engine_sound: bool,
    /// [`crate::sound_seam::trail_on`]: `GlowConfig::enabled` — the master
    /// knob AND serious mode AND a style that resolves, the gate the engine
    /// itself reads — AND a nonzero brightness knob, the person's own off.
    pub(crate) trail: bool,
    /// The cursor-effect focus fold, the SAME value `trail status` prints as
    /// `focused=`, so the two verbs cannot contradict each other.
    pub(crate) focused: bool,
    /// `SeriousEffect::TerminalSound`, spelled as `streak status` spells it.
    pub(crate) serious_sound: bool,
    /// `full` or `reduced` — the D1/D2 evidence pair with `shed`, in the same
    /// spellings and formats `trail status` uses, so a human diffing the two
    /// rows on one process is never misled by precision skew.
    pub(crate) motion_stage: &'static str,
    /// The soft load-shed envelope in 0..1 (1.0 = not shedding).
    pub(crate) shed: f32,
    /// Wedge revivals SPENT. A revival stands up a fresh worker with a fresh
    /// counter, so `dropped=0` beside `revives=2` stops being a lie by
    /// omission.
    pub(crate) revives: u64,
    /// Device reopens the audio worker has LEFT ([`REOPEN_BUDGET`] at birth).
    /// One lower than baseline after a transient CoreAudio fault is the
    /// signal that the host recovered rather than died; `0` beside
    /// `audio=failed` is the one honestly terminal reading.
    pub(crate) reopens_left: u8,
}

impl ToneStatus {
    /// The wire tail after `OK ` — one line, `key=value` pairs in a fixed
    /// order, the shape `rain status` established.
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

    /// The wire line: every gate is its own field, in a fixed order, and the
    /// tone words are the classifier's own labels (so a driver can compare
    /// them against `Tone::label`, not a second spelling of the same enum).
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

    /// The wedge is its own word on the wire: a stuck worker must never be
    /// spelled `live` (the 2026-08 incident) nor collapsed into `inert`
    /// (which names the sealed/failed forms a restart cannot revive).
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
