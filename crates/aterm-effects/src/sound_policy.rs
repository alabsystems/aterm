// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE SOUND-CUE POLICY** — how the cues the cursor family and the word
//! engine record become synth events: the trail-sound gain law (REAL focus ×
//! the music master × volume), the sing-along riff's and the curse bonk's
//! laws beside it, and the two drains that map cues onto
//! [`crate::trail_sound::SoundEvent`]s with the policy stamped on each.
//!
//! Engine-side since the host-boundary Phase 4 move of 2026-09-27
//! (`docs/DESIGN-host-boundary-2026-08-30.md` §3.1, "typing sound cues"):
//! the policy is a pure function of the host's knobs and the engines' cues,
//! so every host shares ONE author for it. The SINK stays the host's — the
//! native app's `TrailAudio` (AudioToolbox) queue; a web page drops the
//! events — and so does the host input clock each event is stamped with.

/// Resolve trail-audio policy from REAL window focus, never the synthetic
/// `motion_focus` bit that recordings use to keep visual effects moving. A
/// background recording may animate; it must never make the Mac speak.
pub fn trail_sound_gain(raw_focused: bool, configured: bool, volume: f32) -> Option<f32> {
    (raw_focused && configured && volume > 0.0).then_some(volume)
}

/// Resolve SING-ALONG RIFF audio policy: the trail-sound law verbatim
/// ([`trail_sound_gain`]) AND the riff's own switch.
///
/// `trail_sound_riff` (owner ask, Sound menu audit; default ON) exists because
/// the held-key riff is the LOUDEST voice the engine emits and until now the
/// only ways to quiet it were the master `trail_sounds` (which also kills the
/// keystroke palette the owner wants to keep) or `trail_sound_volume` (which
/// turns the keystrokes down with it). It is a SOUND gate only — the
/// celebration's ribbon saturation, star shower, dancing cat and singing face
/// are a MOTION contract and keep running, which is why this resolves a GAIN
/// rather than suppressing the celebration.
///
/// ONE author for that law across BOTH riff seams (the single-pane present and
/// the split-pane compose) — the same reason `trail_sound_gain` and
/// `bonk_sound_gain` are functions: a split pane must never sing a song a
/// single pane suppresses.
pub fn sing_riff_gain(
    raw_focused: bool,
    trail_sounds: bool,
    riff: bool,
    volume: f32,
) -> Option<f32> {
    trail_sound_gain(raw_focused, trail_sounds && riff, volume)
}

/// Whether the person asked for any aurora at all: the RESOLVED brightness
/// knob (`cursor_trail_intensity`, or a style pack that resolves to zero),
/// read BEFORE any policy fold. Zero is their explicit off (binding decision
/// C) — it silences the key seam, and it is what the verb must call
/// `trail-off` rather than `engine-silent`.
pub fn user_lit(resolved: &crate::cursor_glow::GlowConfig) -> bool {
    resolved.intensity > 0.0
}

/// THE HOST'S SPLIT of one resolved config into its light half and its audio
/// half, for one window's tick — the ONE place a host's cursor tick decides both.
///
/// The light takes both motion policies (the accessibility stage's amplitude
/// and the load-shed envelope); the key seam takes NEITHER: `audible` is
/// whose window the key landed in and whether the person asked for any
/// aurora at all. Before 2026-09-22 a shed frame or a `Reduce Motion`
/// session zeroed `intensity`, the engine read that as "dark ⇒ silent", and
/// every keystroke went quiet while `aterm ctl tone` still said `audio=live`.
///
/// A function rather than three inline lines so the `TrailSoundSeam`
/// conformance drives THIS fold instead of a transcription of it.
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

    /// The riff's own switch is INDEPENDENT of the trail-sound master: with the
    /// master on, the window focused and the volume up — the shipped default
    /// posture, asserted here as this test's own precondition so the `None`
    /// cases below cannot pass vacuously — flipping `trail_sound_riff` off is
    /// the single change that silences the song.
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

    /// Every OTHER term of the trail-sound law still applies to the riff, so
    /// the new switch cannot be read as a bypass. Each case flips exactly one
    /// term away from the singing precondition above.
    #[test]
    fn the_riff_stays_subordinate_to_focus_master_and_volume() {
        assert_eq!(sing_riff_gain(false, true, true, 0.4), None, "raw focus");
        assert_eq!(sing_riff_gain(true, false, true, 0.4), None, "trail_sounds");
        assert_eq!(sing_riff_gain(true, true, true, 0.0), None, "volume");
    }
}

/// The per-event trail-audio POLICY the host resolves once per drain and
/// stamps on every emitted [`SoundEvent`] — the knobs ride together so the
/// synth stays policy-free.
pub struct TrailSoundPolicy {
    /// The `trail_sound_style` override (default `Style` = follow the visual
    /// trail style).
    pub voice: crate::trail_sound::SoundVoice,
    /// Resolved gain (`None` = muted: focus/knob/volume law — see
    /// [`trail_sound_gain`]).
    pub gain: Option<f32>,
    /// The window's cached tone-of-typing verdict (`tone_infer`). The host
    /// resolves it (knob off ⇒ the neutral `Technical` identity) exactly
    /// like it resolves gain.
    pub tone: crate::tone::Tone,
    /// The `trail_sound_bed` knob (default ON since the owner's 2026-09-09
    /// ruling): with it off no event ever feeds the synth's bed layer, so
    /// the ambient texture contributes exactly zero samples while the notes
    /// keep playing.
    pub bed: bool,
}

/// Map ONE drained cue onto its synth event: the pan normalization, the gesture
/// namespacing, and the policy stamp.
///
/// Extracted so the KEY-TIME typing click (`app_input`, which hands its cue
/// straight to the synth instead of waiting for the next drain) is built by the
/// exact same code as the frame drain. Two constructions of "a trail cue as a
/// sound event" would drift — a differently-normalized pan or a dropped `bed`
/// flag would make the same keystroke sound like two different instruments
/// depending on which seam carried it.
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
        // Rides the CUE, so the key-time seam and the frame drain agree by
        // construction: only `cue_keystroke_shifted` can set it, and every
        // echo-born cue carries `false`.
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

/// Resolve curse-BONK policy — the MASTER music switch (`trail_sounds`), the
/// profanity `bonk` knob, RAW window focus (the trail-sound law verbatim: a
/// background recording may animate, it must never make the Mac speak), the
/// motion policy (a Reduced window pushes no events, matching the glow
/// engine's intensity-0 silence), and the shared `trail_sound_volume`.
/// Per-class/master SPARKLE gates need no re-check: the engine records cues
/// only for words it is actually decorating.
///
/// THE MASTER TERM IS THE FIX FOR A LIE THE UI WAS TELLING. The Sound box's
/// consequence copy (`prefs::group_footnote`, "Sound") promises that *"Music
/// effects in Top Settings is the master switch for the synth voices"*, and
/// the bonk IS a synth voice — it is pushed into the same
/// `TrailAudio` host host, as a
/// [`crate::trail_sound::WordGesture::Bonk`], and it is already scaled
/// by the same `trail_sound_volume`. Until this parameter existed the function
/// took no `trail_sounds` input at all and both render paths passed only the
/// profanity toggle, so muting Music effects and typing profanity still bonked
/// — the one voice that escaped the switch the menu names. The honest law is
/// the one the UI already promises: the master gates EVERY synth voice, and
/// the terminal bell (an OS alert sound emitted from `lib.rs::on_bell`, which
/// reads neither key) remains the single stated exception.
///
/// Delegating the focus/master/volume terms to [`trail_sound_gain`] keeps that
/// law authored ONCE — the same reason [`sing_riff_gain`] delegates — so the
/// bonk cannot drift away from the riff and the keystroke palette again.
pub fn bonk_sound_gain(
    raw_focused: bool,
    trail_sounds: bool,
    enabled: bool,
    reduced_motion: bool,
    volume: f32,
) -> Option<f32> {
    // The bonk's ONE extra term over the shared law: a Reduced-motion window
    // renders no wince, so it must not clash either.
    if reduced_motion {
        return None;
    }
    trail_sound_gain(raw_focused, trail_sounds && enabled, volume)
}

/// Drain every curse-BONK cue the word-decoration tick recorded and emit the
/// enabled ones as namespaced [`WordGesture::Bonk`] gestures — the
/// sparkle-words twin of [`drain_trail_sound_cues`], one seam for glass and
/// capture alike so a disabled knob never leaves a backlog. `detonations`
/// separately gates the on-screen [`CurseCueKind::Detonated`] kind (typed
/// provenance stays typed-only unless the user opted the blast edge in).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CurseDrain {
    /// Bonk events handed to the sink this drain (0 while muted).
    pub emitted: usize,
    /// Distinct profanity locations this tick. A typed cue and its supernova
    /// detonation at the same word are one visual wince, while `fuck fuck`
    /// produces two beats. This remains independent of sound policy.
    pub wince_hits: u8,
}

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
            // The active trail style keys the bonk's clash REGISTER (its
            // palette anchor) so the wrong note is wrong against the melody
            // actually playing; the gesture itself is style-agnostic.
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
            // The bonk is tone-BLIND by contract (the wrong note is wrong in
            // every mood, and its path is byte-pinned): always the neutral
            // identity, never the window's inferred tone.
            tone: crate::tone::Tone::Technical,
            // Words gestures never feed the bed (punctuation must not swell
            // the ambience) — carried OFF so the event states the policy it
            // actually gets, independent of the `trail_sound_bed` knob.
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

    /// The policy resolver: the MASTER music switch, raw focus, the bonk knob,
    /// the reduced-motion demotion, and the shared volume each silence
    /// independently; the survivor passes the volume through as the event
    /// gain. Every `None` case below flips exactly ONE term away from the
    /// audible precondition, so none of them can pass vacuously.
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
        // THE MENU'S OWN PROMISE, as a law rather than as prose: *"Music effects in
        // Top Settings is the master switch for the synth voices"*
        // (`prefs::group_footnote`, "Sound"). `bonk_sound_gain` once had no master
        // input at all, so the bonk was audible with Music effects off; the knob
        // stays ON here so nothing ELSE can be what muted it.
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
