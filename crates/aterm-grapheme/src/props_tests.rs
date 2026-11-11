// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn scripts_of_known_characters() {
    assert_eq!(script('a'), Script::LATIN);
    assert_eq!(script('1'), Script::COMMON);
    assert_eq!(script('\u{301}'), Script::INHERITED);
    assert_eq!(script('\u{628}'), Script::ARABIC);
    assert_eq!(script('\u{5D0}'), Script::HEBREW);
    assert_eq!(script('\u{915}'), Script(*b"Deva"));
    assert_eq!(script('\u{4E2D}'), Script(*b"Hani"));
    assert_eq!(script('\u{378}'), Script::UNKNOWN);
    assert_eq!(script('\u{10FFFF}'), Script::UNKNOWN);
    assert_eq!(script('\0'), Script::COMMON);
}

#[test]
fn categories_of_known_characters() {
    use GeneralCategory as G;
    assert_eq!(general_category('A'), G::UppercaseLetter);
    assert_eq!(general_category('a'), G::LowercaseLetter);
    assert_eq!(general_category('\u{1C5}'), G::TitlecaseLetter);
    assert_eq!(general_category('\u{301}'), G::NonspacingMark);
    assert_eq!(general_category('\u{903}'), G::SpacingMark);
    assert_eq!(general_category('\u{20E3}'), G::EnclosingMark);
    assert_eq!(general_category('7'), G::DecimalNumber);
    assert_eq!(general_category('\u{661}'), G::DecimalNumber);
    assert_eq!(general_category(' '), G::SpaceSeparator);
    assert_eq!(general_category('\u{200D}'), G::Format);
    assert_eq!(general_category('\u{1F3FB}'), G::ModifierSymbol);
    assert_eq!(general_category('\u{E000}'), G::PrivateUse);
    assert_eq!(general_category('\u{378}'), G::Unassigned);
    assert_eq!(general_category('\u{10FFFF}'), G::Unassigned);
    assert_eq!(general_category('\0'), G::Control);
    assert!(G::SpacingMark.is_mark() && !G::Format.is_mark());
    assert!(G::ModifierLetter.is_letter() && !G::LetterNumber.is_letter());
}

#[test]
fn script_runs_answer_what_script_answers() {
    let runs: Vec<(u32, Script)> = script_runs().collect();
    assert_eq!(runs.first().map(|r| r.0), Some(0));
    let mut checked = 0u32;
    for (i, &(first, run_script)) in runs.iter().enumerate() {
        let end = runs.get(i + 1).map_or(0x11_0000, |r| r.0);
        for cp in (first..end).filter_map(char::from_u32) {
            assert_eq!(script(cp), run_script, "U+{:04X}", u32::from(cp));
            checked += 1;
        }
    }
    assert_eq!(checked, 0x11_0000 - 0x800, "every scalar value, once");
}

#[test]
fn run_tables_are_well_formed() {
    for (runs, values) in [
        (&SCRIPT_RUNS[..], SCRIPT_CODES.len()),
        (&GENERAL_CATEGORY_RUNS[..], GeneralCategory::ALL.len()),
    ] {
        assert_eq!(runs.first().map(|e| e >> 8), Some(0));
        assert!(runs.windows(2).all(|w| (w[0] >> 8) < (w[1] >> 8)));
        assert!(runs.iter().all(|e| ((e & 0xFF) as usize) < values));
    }
    for (i, g) in GeneralCategory::ALL.iter().enumerate() {
        assert_eq!(*g as usize, i);
    }
}
