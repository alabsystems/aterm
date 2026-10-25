// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The sound-cue policy.

/// THE SYNTH VOICES, and which of a window's policies each one obeys.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Voice {
    /// The keystroke click, which carries the tone-adaptive melody and the ambient bed.
    Keystroke,
    /// The held-key sing-along riff ([`sing_riff_gain`]).
    Riff,
    /// The curse bonk ([`bonk_sound_gain`]).
    Bonk,
    /// The output streak's pip, the note when a program starts printing.
    StreakPip,
    /// The command verdict that replaces the pip's exhale.
    Verdict,
}

impl Voice {
    /// Every voice, for a caller that asks what a policy idles.
    pub const ALL: [Self; 5] = [
        Self::Keystroke,
        Self::Riff,
        Self::Bonk,
        Self::StreakPip,
        Self::Verdict,
    ];

    /// The motion effect this voice is the sound of.
    #[must_use]
    #[inline]
    pub const fn motion_twin(self) -> Option<crate::motion::MotionEffect> {
        match self {
            Self::Bonk => Some(crate::motion::MotionEffect::WordSparkles),
            Self::StreakPip => Some(crate::motion::MotionEffect::OutputStreak),
            Self::Keystroke | Self::Riff | Self::Verdict => None,
        }
    }
}

/// The terms one voice's gain resolves from.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct VoiceTerms {
    /// The Music effects master, with any host policy that silences every voice folded in.
    pub master: bool,
    /// The voice's own switch; `true` for a voice that has none.
    pub own: bool,
    /// `trail_sound_volume`.
    pub volume: f32,
    /// Real window focus, never a recording's pinned motion focus.
    pub raw_focused: bool,
    /// Whether the window's motion policy has stopped this voice's motion twin this tick.
    pub reduced_motion: bool,
}

/// THE ONE SOUND LAW: the gain `voice` plays at, or `None` while it is silent.
#[inline]
#[must_use]
pub const fn voice_gain(voice: Voice, terms: &VoiceTerms) -> Option<f32> {
    if terms.reduced_motion && voice.motion_twin().is_some() {
        return None;
    }
    trail_sound_gain(terms.raw_focused, terms.master && terms.own, terms.volume)
}

/// Resolve trail-audio policy from real window focus.
#[inline]
pub const fn trail_sound_gain(raw_focused: bool, configured: bool, volume: f32) -> Option<f32> {
    if raw_focused && configured && volume > 0.0 {
        Some(volume)
    } else {
        None
    }
}

/// Resolve sing-along riff audio.
#[inline]
pub const fn sing_riff_gain(
    raw_focused: bool,
    trail_sounds: bool,
    riff: bool,
    volume: f32,
) -> Option<f32> {
    voice_gain(
        Voice::Riff,
        &VoiceTerms {
            master: trail_sounds,
            own: riff,
            volume,
            raw_focused,
            reduced_motion: false,
        },
    )
}

/// Whether the person asked for any aurora at all.
#[inline]
pub const fn user_lit(resolved: &crate::cursor_glow::GlowConfig) -> bool {
    resolved.intensity > 0.0
}

/// The host's split of one resolved config into its light half and its
/// audio half for one window's tick.
#[inline]
pub const fn fold_window_audibility(
    cfg: &mut crate::cursor_glow::GlowConfig,
    motion_amplitude: f32,
    shed_env: f32,
    win_focused: bool,
) {
    let lit = user_lit(cfg);
    cfg.intensity *= motion_amplitude * shed_env;
    cfg.audible = win_focused && lit;
}

#[cfg(test)]
mod sing_riff_gain_tests {
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
}

#[cfg(test)]
mod voice_gain_tests {
    use super::{Voice, VoiceTerms, voice_gain};

    const OPEN: VoiceTerms = VoiceTerms {
        master: true,
        own: true,
        volume: 0.4,
        raw_focused: true,
        reduced_motion: false,
    };

    /// THE VOICE TABLE: every voice plays with every gate open; the master,
    /// its own switch, the volume and a background window silence each; a
    /// reduced motion policy silences only the bonk and the pip, whose light
    /// it stops.
    #[test]
    fn each_voice_obeys_exactly_its_gates() {
        for voice in Voice::ALL {
            assert_eq!(voice_gain(voice, &OPEN), Some(0.4), "{voice:?} plays");
            for closed in [
                VoiceTerms {
                    master: false,
                    ..OPEN
                },
                VoiceTerms { own: false, ..OPEN },
                VoiceTerms {
                    volume: 0.0,
                    ..OPEN
                },
                VoiceTerms {
                    raw_focused: false,
                    ..OPEN
                },
            ] {
                assert_eq!(voice_gain(voice, &closed), None, "{voice:?} {closed:?}");
            }
            let reduced = VoiceTerms {
                reduced_motion: true,
                ..OPEN
            };
            assert_eq!(
                voice_gain(voice, &reduced).is_none(),
                matches!(voice, Voice::Bonk | Voice::StreakPip),
                "{voice:?}: reduced motion silences only the voices whose light it stops"
            );
        }
    }
}

/// The per-event trail-audio policy the host resolves once per drain and
/// stamps on every emitted [`crate::trail_sound::SoundEvent`], so the synth stays policy-free.
#[derive(Clone, Copy, Debug)]
pub struct TrailSoundPolicy {
    /// The `trail_sound_style` override (default `Style` = follow the visual trail style).
    pub voice: crate::trail_sound::SoundVoice,
    /// Resolved gain (`None` = muted: focus/knob/volume law — see [`trail_sound_gain`]).
    pub gain: Option<f32>,
    /// The window's cached tone-of-typing verdict, resolved by the host like gain.
    pub tone: crate::tone::Tone,
    /// The `trail_sound_bed` knob (default on).
    pub bed: bool,
}

/// Map one drained cue onto its synth event.
#[inline]
pub const fn trail_sound_event(
    cue: &crate::cursor_glow::SoundCue,
    style: crate::cursor_glow::GlowStyle,
    cols: u16,
    policy: &TrailSoundPolicy,
    gain: f32,
) -> crate::trail_sound::SoundEvent {
    crate::trail_sound::SoundEvent {
        style,
        voice: policy.voice,
        kind: crate::trail_sound::SoundGesture::Trail(cue.kind),
        pan: if cols > 1 {
            let last = (cols - 1) as f32;
            ((cue.col as f32).min(last) / last) * 2.0 - 1.0
        } else {
            0.0
        },
        heat: cue.heat,
        hue: cue.hue,
        gain,
        tone: policy.tone,
        bed: policy.bed,
        shifted: cue.shifted,
    }
}

/// Drain every visual spawn cue, emitting its sound twin unless muted.
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn drain_trail_sound_cues(
    glow: &mut crate::cursor_glow::CursorGlow,
    style: crate::cursor_glow::GlowStyle,
    cols: u16,
    policy: TrailSoundPolicy,
    mut emit: impl FnMut(crate::trail_sound::SoundEvent),
) -> usize {
    let drain = glow.drain_sound_cues();
    let Some(gain) = policy.gain else {
        return 0;
    };
    let mut emitted = 0;
    for cue in drain {
        emit(trail_sound_event(&cue, style, cols, &policy, gain));
        emitted += 1;
    }
    emitted
}

/// Resolve curse-bonk policy.
#[inline]
pub const fn bonk_sound_gain(
    raw_focused: bool,
    trail_sounds: bool,
    enabled: bool,
    reduced_motion: bool,
    volume: f32,
) -> Option<f32> {
    voice_gain(
        Voice::Bonk,
        &VoiceTerms {
            master: trail_sounds,
            own: enabled,
            volume,
            raw_focused,
            reduced_motion,
        },
    )
}

/// What [`drain_curse_bonk_cues`] reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurseDrain {
    /// Bonk events handed to the sink this drain (0 while muted).
    pub emitted: usize,
    /// Distinct profanity locations this tick, independent of sound policy.
    pub wince_hits: u8,
}

impl CurseDrain {
    /// Empty drain report before any cues are processed.
    pub const EMPTY: Self = Self {
        emitted: 0,
        wince_hits: 0,
    };
}

impl Default for CurseDrain {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// Drain every curse-bonk cue the word-decoration tick recorded and emit the
/// enabled ones as namespaced [`crate::trail_sound::WordGesture::Bonk`]
/// gestures; the sparkle-words twin of [`drain_trail_sound_cues`], so a
/// disabled knob never leaves a backlog.
pub fn drain_curse_bonk_cues(
    decos: &mut crate::word_decorations::WordDecorations,
    style: crate::cursor_glow::GlowStyle,
    voice: crate::trail_sound::SoundVoice,
    cols: u16,
    gain: Option<f32>,
    detonations: bool,
    mut emit: impl FnMut(crate::trail_sound::SoundEvent),
) -> CurseDrain {
    use crate::word_decorations::CurseCueKind;
    let mut result = CurseDrain::default();
    let mut seen = [u32::MAX; 16];
    let mut seen_len = 0usize;
    let last_col = (cols > 1).then(|| {
        let last = (cols - 1) as f32;
        (last, 1.0 / last)
    });
    for cue in decos.drain_curse_cues() {
        let location = u32::from(cue.row) << 16 | u32::from(cue.col);
        if !seen[..seen_len].contains(&location) {
            if seen_len < seen.len() {
                seen[seen_len] = location;
                seen_len += 1;
            }
            result.wince_hits = result.wince_hits.saturating_add(1);
        }
        let Some(gain) = gain else {
            continue;
        };
        if cue.kind == CurseCueKind::Detonated && !detonations {
            continue;
        }
        emit(crate::trail_sound::SoundEvent {
            style,
            voice,
            kind: crate::trail_sound::SoundGesture::Words(crate::trail_sound::WordGesture::Bonk),
            pan: match last_col {
                Some((last, inv_last)) => ((cue.col as f32).min(last) * inv_last) * 2.0 - 1.0,
                None => 0.0,
            },
            heat: 0.0,
            hue: 0.0,
            gain,
            tone: crate::tone::Tone::Technical,
            bed: false,
            shifted: false,
        });
        result.emitted += 1;
    }
    result
}

#[cfg(test)]
mod bonk_gain_tests {
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
}
