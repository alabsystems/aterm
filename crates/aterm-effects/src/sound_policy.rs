// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The sound-cue policy: how the cues the cursor family and the word engine
//! record become synth events. The trail-sound gain law (real focus × the
//! music master × volume), the sing-along riff's and the curse bonk's laws
//! beside it, and the two drains that map cues onto
//! [`crate::trail_sound::SoundEvent`]s with the policy stamped on each.
//!
//! The policy is a pure function of the host's knobs and the engines' cues,
//! so every host shares it (`docs/DESIGN-host-boundary-2026-08-30.md` §3.1).
//! The sink stays the host's (the native app's `TrailAudio` queue; a web
//! page drops the events), as does the input clock each event is stamped with.

/// THE SYNTH VOICES, and which of a window's policies each one obeys. The
/// host resolves every voice's gain through [`voice_gain`], and Settings
/// reads the same table to say which rows a policy idles, so the page and
/// the speaker cannot disagree (a row that says "Inactive" while its voice
/// still plays is worse than no note).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Voice {
    /// The keystroke click, which carries the tone-adaptive melody and the
    /// ambient bed (the host's key seam, `sound_seam`).
    Keystroke,
    /// The held-key sing-along riff ([`sing_riff_gain`]).
    Riff,
    /// The curse bonk ([`bonk_sound_gain`]).
    Bonk,
    /// The output streak's pip, the note when a program starts printing.
    StreakPip,
    /// The command verdict that replaces the pip's exhale.
    Verdict,
    /// The supervisor's answer chime.
    Chime,
}

impl Voice {
    /// Every voice, for a caller that asks what a policy idles.
    pub const ALL: [Self; 6] = [
        Self::Keystroke,
        Self::Riff,
        Self::Bonk,
        Self::StreakPip,
        Self::Verdict,
        Self::Chime,
    ];

    /// Whether real window focus gates this voice: a background window may
    /// animate but never speak. The chime is the exception: its point is to
    /// hear a background session's answer.
    #[must_use]
    pub const fn follows_focus(self) -> bool {
        !matches!(self, Self::Chime)
    }

    /// The motion effect this voice is the sound of, where a reduced motion
    /// policy stops it with its light: the bonk with the word sparkles' wince,
    /// the pip with the output streak's comet. Every other voice plays under
    /// Reduce Motion and the load shed alike (a motion setting is not an audio
    /// setting, owner, 2026-09-22).
    #[must_use]
    pub const fn motion_twin(self) -> Option<crate::motion::MotionEffect> {
        match self {
            Self::Bonk => Some(crate::motion::MotionEffect::WordSparkles),
            Self::StreakPip => Some(crate::motion::MotionEffect::OutputStreak),
            Self::Keystroke | Self::Riff | Self::Verdict | Self::Chime => None,
        }
    }
}

/// The terms one voice's gain resolves from.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct VoiceTerms {
    /// The Music effects master (`trail_sounds`), with any host policy that
    /// silences every voice (serious mode) folded in.
    pub master: bool,
    /// The voice's own switch (`trail_sound_riff`, `choice_sound`, the bonk's,
    /// `output_streak.sound`); `true` for a voice that has none.
    pub own: bool,
    /// `trail_sound_volume`.
    pub volume: f32,
    /// Real window focus, never a recording's pinned motion focus.
    pub raw_focused: bool,
    /// Whether the window's motion policy has stopped this voice's motion
    /// twin ([`Voice::motion_twin`]) this tick.
    pub reduced_motion: bool,
}

/// THE ONE SOUND LAW: the gain `voice` plays at, or `None` while it is
/// silent. The master, the voice's own switch and the volume gate every
/// voice; focus gates every voice that [`Voice::follows_focus`]; a reduced
/// motion policy silences only a voice with a [`Voice::motion_twin`].
#[must_use]
pub fn voice_gain(voice: Voice, terms: &VoiceTerms) -> Option<f32> {
    if terms.reduced_motion && voice.motion_twin().is_some() {
        return None;
    }
    trail_sound_gain(
        terms.raw_focused || !voice.follows_focus(),
        terms.master && terms.own,
        terms.volume,
    )
}

/// Resolve trail-audio policy from real window focus, never the synthetic
/// `motion_focus` bit that recordings use to keep visual effects moving. A
/// background recording may animate; it must never make the Mac speak.
pub fn trail_sound_gain(raw_focused: bool, configured: bool, volume: f32) -> Option<f32> {
    (raw_focused && configured && volume > 0.0).then_some(volume)
}

/// Resolve sing-along riff audio policy: the trail-sound law
/// ([`trail_sound_gain`]) and the riff's own switch.
///
/// `trail_sound_riff` (default on) quiets the held-key riff, the loudest
/// voice the engine emits, without muting the keystroke palette. It gates
/// sound only: the celebration's ribbon saturation, star shower, dancing cat
/// and singing face are a motion contract and keep running, so this resolves
/// a gain rather than suppressing the celebration. Both riff seams (the
/// single-pane present and the split-pane compose) use it, so a split pane
/// never sings a song a single pane suppresses.
pub fn sing_riff_gain(
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

/// Whether the person asked for any aurora at all: the resolved brightness
/// knob (`cursor_trail_intensity`, or a style pack that resolves to zero),
/// read before any policy fold. Zero is their explicit off: it silences the
/// key seam, and the verb calls it `trail-off` rather than `engine-silent`.
pub fn user_lit(resolved: &crate::cursor_glow::GlowConfig) -> bool {
    resolved.intensity > 0.0
}

/// The host's split of one resolved config into its light half and its
/// audio half for one window's tick.
///
/// The light takes both motion policies (the accessibility stage's amplitude
/// and the load-shed envelope); the key seam takes neither: `audible` is
/// whose window the key landed in and whether the person asked for any
/// aurora at all, so a shed frame or Reduce Motion never silences keystrokes.
/// The `TrailSoundSeam` conformance drives this function directly.
pub fn fold_window_audibility(
    cfg: &mut crate::cursor_glow::GlowConfig,
    motion_amplitude: f32,
    shed_env: f32,
    win_focused: bool,
) {
    // Read BEFORE the fold below: the one point at which the three meanings
    // of a zero `intensity` are still separable.
    let lit = user_lit(cfg);
    cfg.intensity *= motion_amplitude * shed_env;
    cfg.audible = win_focused && lit;
}

#[cfg(test)]
mod sing_riff_gain_tests {
    use super::sing_riff_gain;

    /// The riff's own switch is independent of the trail-sound master: with
    /// the master on, the window focused and the volume up (asserted as a
    /// precondition so the `None` cases cannot pass vacuously), flipping
    /// `trail_sound_riff` off is the single change that silences the song.
    #[test]
    fn the_riff_switch_alone_silences_the_song_under_shipped_defaults() {
        assert_eq!(
            sing_riff_gain(true, true, true, 0.4),
            Some(0.4),
            "precondition: the shipped default posture must actually sing",
        );
        assert_eq!(
            sing_riff_gain(true, true, false, 0.4),
            None,
            "trail_sound_riff = false must silence the riff on its own",
        );
    }

    /// Every other term of the trail-sound law still applies to the riff, so
    /// the switch is not a bypass. Each case flips exactly one term away from
    /// the singing precondition.
    #[test]
    fn the_riff_stays_subordinate_to_focus_master_and_volume() {
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
    /// its own switch and the volume silence each; focus silences all but the
    /// chime; a reduced motion policy silences only the bonk and the pip,
    /// whose light it stops.
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
            ] {
                assert_eq!(voice_gain(voice, &closed), None, "{voice:?} {closed:?}");
            }
            let unfocused = VoiceTerms {
                raw_focused: false,
                ..OPEN
            };
            assert_eq!(
                voice_gain(voice, &unfocused).is_none(),
                voice != Voice::Chime,
                "{voice:?}: only the chime speaks from a background window"
            );
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
/// stamps on every emitted [`SoundEvent`], so the synth stays policy-free.
pub struct TrailSoundPolicy {
    /// The `trail_sound_style` override (default `Style` = follow the visual
    /// trail style).
    pub voice: crate::trail_sound::SoundVoice,
    /// Resolved gain (`None` = muted: focus/knob/volume law — see
    /// [`trail_sound_gain`]).
    pub gain: Option<f32>,
    /// The window's cached tone-of-typing verdict (`tone_infer`), resolved
    /// by the host like gain (knob off ⇒ the neutral `Technical` identity).
    pub tone: crate::tone::Tone,
    /// The `trail_sound_bed` knob (default on): off, no event feeds the
    /// synth's bed layer, so the ambient texture contributes zero samples
    /// while the notes keep playing (owner, 2026-09-09).
    pub bed: bool,
}

/// Map one drained cue onto its synth event: the pan normalization, the
/// gesture namespacing, and the policy stamp. The key-time typing click
/// (`app_input`) and the frame drain both build events here, so the same
/// keystroke sounds the same whichever seam carries it.
pub fn trail_sound_event(
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
        // Rides the cue, so the key-time seam and the frame drain agree:
        // only `cue_keystroke_shifted` sets it; every echo-born cue carries
        // `false`.
        shifted: cue.shifted,
    }
}

/// Drain every visual spawn cue and optionally emit its allocation-free sound
/// twin. Both single-pane and split-pane composition route through this seam,
/// so muting never leaves a backlog and layouts cannot silently lose audio.
#[allow(clippy::too_many_arguments)] // Explicit policy axes keep cue emission allocation-free.
pub fn drain_trail_sound_cues(
    glow: &mut crate::cursor_glow::CursorGlow,
    style: crate::cursor_glow::GlowStyle,
    cols: u16,
    policy: TrailSoundPolicy,
    mut emit: impl FnMut(crate::trail_sound::SoundEvent),
) -> usize {
    let mut emitted = 0;
    for cue in glow.drain_sound_cues() {
        let Some(gain) = policy.gain else {
            continue;
        };
        emit(trail_sound_event(&cue, style, cols, &policy, gain));
        emitted += 1;
    }
    emitted
}

/// Resolve curse-bonk policy: the music master (`trail_sounds`), the
/// profanity `bonk` knob, raw window focus (a background recording may
/// animate, never speak), the motion policy (a Reduced window pushes no
/// events, matching the glow engine's intensity-0 silence), and the shared
/// `trail_sound_volume`. Per-class and master sparkle gates need no
/// re-check: the engine records cues only for words it decorates.
///
/// The bonk is a synth voice, so the master gates it as the Sound box's
/// footnote (`prefs::group_footnote`, "Sound") promises; the terminal bell
/// (`lib.rs::on_bell`) is the one stated exception. The focus, master and
/// volume terms delegate to [`trail_sound_gain`], as [`sing_riff_gain`] does.
pub fn bonk_sound_gain(
    raw_focused: bool,
    trail_sounds: bool,
    enabled: bool,
    reduced_motion: bool,
    volume: f32,
) -> Option<f32> {
    // The bonk's ONE extra term over the shared law: a Reduced-motion window
    // renders no wince, so it must not clash either (its motion twin).
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CurseDrain {
    /// Bonk events handed to the sink this drain (0 while muted).
    pub emitted: usize,
    /// Distinct profanity locations this tick. A typed cue and its supernova
    /// detonation at the same word are one visual wince, while `fuck fuck`
    /// produces two beats. This remains independent of sound policy.
    pub wince_hits: u8,
}

/// Drain every curse-bonk cue the word-decoration tick recorded and emit the
/// enabled ones as namespaced [`crate::trail_sound::WordGesture::Bonk`]
/// gestures; the sparkle-words twin of [`drain_trail_sound_cues`], so a
/// disabled knob never leaves a backlog. `detonations` gates the
/// [`crate::word_decorations::CurseCueKind::Detonated`] kind.
pub fn drain_curse_bonk_cues(
    decos: &mut crate::word_decorations::WordDecorations,
    style: crate::cursor_glow::GlowStyle,
    // The `trail_sound_style` override — the bonk's clash SHAPE is
    // style-agnostic, but its register anchor follows the speaking palette.
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
            // The active trail style keys the bonk's clash register (its
            // palette anchor), so the wrong note is wrong against the melody
            // playing; the gesture itself is style-agnostic.
            style,
            voice,
            kind: crate::trail_sound::SoundGesture::Words(crate::trail_sound::WordGesture::Bonk),
            pan: if cols > 1 {
                let last = (cols - 1) as f32;
                ((cue.col as f32).min(last) / last) * 2.0 - 1.0
            } else {
                0.0
            },
            heat: 0.0,
            hue: 0.0,
            gain,
            // The bonk is tone-blind (the wrong note is wrong in every mood,
            // and its path is byte-pinned): always the neutral identity.
            tone: crate::tone::Tone::Technical,
            // Word gestures never feed the bed (punctuation must not swell
            // the ambience); carried off so the event states the policy it gets.
            bed: false,
            shifted: false, // a bonk is punctuation over the text, not a typed glyph
        });
        result.emitted += 1;
    }
    result
}

#[cfg(test)]
mod bonk_gain_tests {
    use super::bonk_sound_gain;

    /// The policy resolver: the music master, raw focus, the bonk knob, the
    /// reduced-motion demotion and the shared volume each silence
    /// independently; the survivor passes the volume through as the event
    /// gain. Every `None` case flips exactly one term away from the audible
    /// precondition.
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
        // The master switch gates the bonk, as `prefs::group_footnote`
        // ("Sound") promises; the bonk knob stays on so nothing else mutes it.
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
