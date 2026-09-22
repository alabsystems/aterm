// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE AUDIBILITY ORACLE** — one table that decides whether a keypress
//! right now makes a sound, and names the gate when it does not.
//!
//! `aterm ctl tone` used to be structurally incapable of refuting "sound is
//! broken": its `audio=` field came only from `TrailAudio::is_live()` and
//! `wedged_for()`, and there was NO field for the engine's own key seam, the
//! motion stage, or the load-shed envelope. So a shed frame and a
//! `Reduce Motion` session both printed `audio=live sounds=on volume=0.40
//! active=true dropped=0` while the user heard silence — the same
//! false-reassurance shape as the recorded 2026-08 "silent for hours while
//! tone said live" incident.
//!
//! PRECEDENCE, fixed and enforced by a test rather than merely written down:
//! what the user SET, then where the user IS, then whether the host CAN
//! sound, then transients, then the engine. The boolean answer is an AND over
//! every term, so `open` can never disagree with the key path whatever
//! precedence names; precedence only chooses WHICH closed gate gets reported
//! when several are closed at once.
//!
//! Pure: no `App`, no window, no clock. Headless-constructible, exactly as
//! `tone_infer` is.

use crate::trail_audio::HostState;

/// Everything that decides whether a keypress sounds, gathered once so the
/// verb and the key path cannot drift.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct SeamInputs {
    /// `trail_sounds`.
    pub(crate) sounds_on: bool,
    /// `trail_sound_volume`, already clamped to 0..1.
    pub(crate) volume: f32,
    /// `GlowConfig::enabled` — the master knob AND serious mode AND a style
    /// that resolves to something, which is the gate the ENGINE reads.
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
    /// `CursorGlow::sound_seam_open()` — whether the last tick left the
    /// engine's key seam open.
    pub(crate) engine_sound_live: bool,
}

/// WHY a keypress would make no sound. One variant per gate, so the verb can
/// name the gate instead of leaving a human to bisect six settings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SeamReason {
    SoundsOff,
    VolumeZero,
    TrailOff,
    SeriousMode,
    Unfocused,
    HostInert,
    HostWedged,
    ResizeQuiet,
    /// The engine refuses for a reason the config gates do not explain.
    ///
    /// THE SENSOR THAT MATTERS. Before 2026-09-22 this was the shipping
    /// state under load shed and under macOS `Reduce Motion`, and nothing on
    /// the tone row could see it. After the decoupling it should be
    /// UNREACHABLE — and if a future performance or accessibility policy
    /// re-closes the key seam, `tone` prints `seam=closed:engine-silent`
    /// instead of `audio=live`. That is the eleven-months-later regression
    /// made visible.
    EngineSilent,
}

impl SeamReason {
    /// FROZEN. Adding a variant means deleting a line of
    /// `the_reason_vocabulary_is_frozen` in the same commit and saying why —
    /// which is exactly the friction a new "audio is subordinate to a motion
    /// or performance policy" reason should meet.
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
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::SoundsOff => "sounds-off",
            Self::VolumeZero => "volume-zero",
            Self::TrailOff => "trail-off",
            Self::SeriousMode => "serious-mode",
            Self::Unfocused => "unfocused",
            Self::HostInert => "host-inert",
            Self::HostWedged => "host-wedged",
            Self::ResizeQuiet => "resize-quiet",
            Self::EngineSilent => "engine-silent",
        }
    }
}

/// `None` ⇒ a keypress right now produces a sound.
pub(crate) fn sound_seam(i: &SeamInputs) -> Option<SeamReason> {
    // What the user SET.
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
    // Where the user IS.
    if !i.focused {
        return Some(SeamReason::Unfocused);
    }
    // Whether the host CAN sound.
    if matches!(i.host, HostState::Inert) {
        return Some(SeamReason::HostInert);
    }
    if matches!(i.host, HostState::Wedged) {
        return Some(SeamReason::HostWedged);
    }
    // Transients.
    if i.resize_quiet {
        return Some(SeamReason::ResizeQuiet);
    }
    // And the engine, last, so a gate with a NAME always outranks it.
    if !i.engine_sound_live {
        return Some(SeamReason::EngineSilent);
    }
    None
}

/// `"open"` or `"closed:<token>"` — the one field on the row that answers the
/// question the verb exists for. ONE renderer, so the wire spelling the tests
/// assert on and the one `ToneStatus::line` emits cannot diverge.
pub(crate) fn seam_field(verdict: Option<SeamReason>) -> String {
    match verdict {
        None => "open".to_string(),
        Some(r) => format!("closed:{}", r.token()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every term open: the baseline the table is perturbed from.
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

    /// THE ANSWER FIELD IS EXACT, and every reason is reachable.
    #[test]
    fn the_seam_names_the_gate_that_would_silence_the_key() {
        // NEGATIVE CONTROL first: without it a `sound_seam` that returned
        // `Some` unconditionally would pass the whole table below.
        assert_eq!(seam_field(sound_seam(&open())), "open");

        for reason in SeamReason::ALL {
            let i = close(reason);
            assert_eq!(
                seam_field(sound_seam(&i)),
                format!("closed:{}", reason.token()),
                "closing exactly {reason:?} must report itself"
            );
        }

        // PRECEDENCE is enforced, not merely documented: what the user SET
        // outranks where the user IS.
        let mut both = open();
        both.sounds_on = false;
        both.focused = false;
        assert_eq!(seam_field(sound_seam(&both)), "closed:sounds-off");

        // …and a gate with a NAME outranks the engine's bare refusal, so a
        // human is never sent hunting the engine for a knob they turned off.
        let mut engine_and_knob = open();
        engine_and_knob.engine_sound_live = false;
        engine_and_knob.trail_on = false;
        assert_eq!(seam_field(sound_seam(&engine_and_knob)), "closed:trail-off");
    }

    /// THE REGRESSION THIS FILE EXISTS FOR. The 2026-08 "live over silence"
    /// incident and the 2026-09 load-shed mute are the same shape a year
    /// apart: a policy that is not about sound acquiring a mute switch, with
    /// no verb able to say so. Adding a `perf-shed` or `reduce-motion` reason
    /// here means re-subordinating audio to a motion or performance policy —
    /// so it cannot land silently: the author must delete a line of this test
    /// in the same commit and say why.
    #[test]
    fn the_reason_vocabulary_is_frozen() {
        let tokens: Vec<&str> = SeamReason::ALL.iter().map(|r| r.token()).collect();
        assert_eq!(
            tokens,
            [
                "sounds-off",
                "volume-zero",
                "trail-off",
                "serious-mode",
                "unfocused",
                "host-inert",
                "host-wedged",
                "resize-quiet",
                "engine-silent",
            ]
        );
        // And `ALL` really is all of them: a new variant is a compile error
        // in `close()`'s exhaustive `match` above and a length error here.
        assert_eq!(SeamReason::ALL.len(), 9);
    }

    /// THE VERB AND THE KEY PATH SHARE ONE TABLE. `keystroke_click_audible`
    /// is the same function with the host, trail, focus and engine terms
    /// PINNED OPEN — and this pins that the rewrite did not change what the
    /// key path decides, by checking it against the legacy five-term AND over
    /// the whole cartesian product.
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
