// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The audibility oracle: one table that decides whether a keypress makes a sound now, and
//! names the gate when it does not (`tone`'s `seam=` field).

use crate::trail_audio::HostState;

/// Tier-1 conformance of the derived `TrailSoundSeam` machine
/// (`aterm_models_misc::trail_sound_seam_model`): the real host fold, the
/// real engine tick, the real key seam and this table, bound to the model.
#[cfg(test)]
#[path = "sound_seam_conformance.rs"]
mod conformance;

pub(crate) use aterm_effects::sound_policy::{fold_window_audibility, user_lit};

/// `SeamInputs::trail_on` for a resolved (pre-fold) config: `enabled` and a nonzero
/// brightness knob. An aurora dimmed to zero is silent by design and reads `trail-off`,
/// never `engine-silent` (the reason reserved for a gate with no name).
pub(crate) const fn trail_on(resolved: &aterm_effects::cursor_glow::GlowConfig) -> bool {
    resolved.enabled && user_lit(resolved)
}

/// Everything that decides whether a keypress sounds, gathered once so the
/// verb and the key path cannot drift.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct SeamInputs {
    /// `trail_sounds`.
    pub(crate) sounds_on: bool,
    /// `trail_sound_volume`, already clamped to 0..1.
    pub(crate) volume: f32,
    /// [`trail_on`] of the resolved config: the master knob, serious mode and a style that
    /// resolves (`GlowConfig::enabled`, the engine's gate), and a nonzero brightness knob.
    pub(crate) trail_on: bool,
    /// `SeriousEffect::TerminalSound`.
    pub(crate) serious_allows: bool,
    /// The cursor-effect focus fold, the same value `trail status` prints as
    /// `focused=`.
    pub(crate) focused: bool,
    /// The post-resize quiet window.
    pub(crate) resize_quiet: bool,
    /// The audio worker's honest state.
    pub(crate) host: HostState,
    /// `CursorGlow::sound_seam_open()`: whether the last tick left the engine's key seam
    /// open.
    pub(crate) engine_sound_live: bool,
}

/// The host's own [`SeamInputs`] terms: those neither derived from the resolved
/// `GlowConfig` nor read off the `CursorGlow`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct HostSoundTerms {
    pub(crate) sounds_on: bool,
    pub(crate) volume: f32,
    pub(crate) serious_allows: bool,
    pub(crate) focused: bool,
    pub(crate) resize_quiet: bool,
    pub(crate) host: HostState,
}

/// The verb's inputs: `App::tone_status` and the `TrailSoundSeam` conformance's verb reader
/// both build [`SeamInputs`] here, so the conformance covers how the verb reads the config
/// and the engine. `resolved` is the resolved config (`App::glow_config`), never the one
/// `cursor_frame` folds: a dark trail under Reduce Motion or load shed is still `trail_on`.
pub(crate) fn verb_seam_inputs(
    resolved: &aterm_effects::cursor_glow::GlowConfig,
    engine: &aterm_effects::cursor_glow::CursorGlow,
    host: HostSoundTerms,
) -> SeamInputs {
    SeamInputs {
        sounds_on: host.sounds_on,
        volume: host.volume,
        trail_on: trail_on(resolved),
        serious_allows: host.serious_allows,
        focused: host.focused,
        resize_quiet: host.resize_quiet,
        host: host.host,
        engine_sound_live: engine.sound_seam_open(),
    }
}

/// Why a keypress would make no sound: one variant per gate, so the verb names the gate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SeamReason {
    SoundsOff,
    VolumeZero,
    TrailOff,
    SeriousMode,
    Unfocused,
    HostInert,
    #[cfg(any(target_os = "macos", test))]
    HostWedged,
    ResizeQuiet,
    /// The engine refuses for a reason the config gates do not explain. Should be
    /// unreachable: if a performance or accessibility policy re-closes the key seam, `tone`
    /// prints `seam=closed:engine-silent` rather than `audio=live`.
    EngineSilent,
}

impl SeamReason {
    /// Every reason, for the tests that close each gate in turn.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 9] = [
        Self::SoundsOff,
        Self::VolumeZero,
        Self::TrailOff,
        Self::SeriousMode,
        Self::Unfocused,
        Self::HostInert,
        Self::HostWedged,
        Self::ResizeQuiet,
        Self::EngineSilent,
    ];

    /// The wire vocabulary: stable, greppable, lowercase kebab.
    pub(crate) const fn token(self) -> &'static str {
        match self {
            Self::SoundsOff => "sounds-off",
            Self::VolumeZero => "volume-zero",
            Self::TrailOff => "trail-off",
            Self::SeriousMode => "serious-mode",
            Self::Unfocused => "unfocused",
            Self::HostInert => "host-inert",
            #[cfg(any(target_os = "macos", test))]
            Self::HostWedged => "host-wedged",
            Self::ResizeQuiet => "resize-quiet",
            Self::EngineSilent => "engine-silent",
        }
    }
}

/// `None` ⇒ a keypress right now produces a sound.
pub(crate) fn sound_seam(i: &SeamInputs) -> Option<SeamReason> {
    // What the user set.
    if !i.sounds_on {
        return Some(SeamReason::SoundsOff);
    }
    if i.volume <= 0.0 {
        return Some(SeamReason::VolumeZero);
    }
    if !i.trail_on {
        return Some(SeamReason::TrailOff);
    }
    if !i.serious_allows {
        return Some(SeamReason::SeriousMode);
    }
    if !i.focused {
        return Some(SeamReason::Unfocused);
    }
    if matches!(i.host, HostState::Inert) {
        return Some(SeamReason::HostInert);
    }
    #[cfg(any(target_os = "macos", test))]
    if matches!(i.host, HostState::Wedged) {
        return Some(SeamReason::HostWedged);
    }
    if i.resize_quiet {
        return Some(SeamReason::ResizeQuiet);
    }
    if !i.engine_sound_live {
        return Some(SeamReason::EngineSilent);
    }
    None
}

/// `"open"` or `"closed:<token>"`. One renderer, so the spelling the tests assert and the
/// one `ToneStatus::line` emits cannot diverge.
pub(crate) fn seam_field(verdict: Option<SeamReason>) -> String {
    match verdict {
        None => "open".to_string(),
        Some(r) => format!("closed:{}", r.token()),
    }
}

#[cfg(test)]
mod tests {
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
}
