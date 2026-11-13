// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
