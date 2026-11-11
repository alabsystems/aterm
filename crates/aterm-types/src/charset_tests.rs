// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn every_final_byte_designates_its_own_set_and_a_default_state_restores_with_nothing() {
    for set in [
        CharacterSet::Ascii,
        CharacterSet::DecLineDrawing,
        CharacterSet::DecSupplemental,
        CharacterSet::UnitedKingdom,
        CharacterSet::Dutch,
        CharacterSet::Finnish,
        CharacterSet::French,
        CharacterSet::FrenchCanadian,
        CharacterSet::German,
        CharacterSet::Italian,
        CharacterSet::NorwegianDanish,
        CharacterSet::Spanish,
        CharacterSet::Swedish,
        CharacterSet::Swiss,
    ] {
        assert_eq!(CharacterSet::from_final_byte(set.final_byte()), Some(set));
    }
    for set in [
        CharacterSet96::IsoLatin1Supplemental,
        CharacterSet96::IsoLatin2Supplemental,
        CharacterSet96::IsoLatin5Supplemental,
        CharacterSet96::IsoLatinCyrillic,
        CharacterSet96::IsoLatinGreek,
    ] {
        assert_eq!(CharacterSet96::from_final_byte(set.final_byte()), Some(set));
    }
    assert_eq!(CharacterSetState::new().restore_ansi(), "");
    let mut state = CharacterSetState::new();
    state.designate(0, CharacterSet::DecLineDrawing);
    state.designate_96(2, CharacterSet96::IsoLatinGreek);
    state.gl = GlMapping::G1;
    state.gr = GrMapping::G3;
    assert_eq!(state.restore_ansi(), "\x1b(0\x1b.F\x0e\x1b|");
}
