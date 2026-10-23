// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Unicode `Script` and `General_Category` character property lookups.

use crate::tables::props::{GENERAL_CATEGORY_RUNS, SCRIPT_CODES, SCRIPT_RUNS};

pub use crate::tables::props::UNICODE_VERSION as PROPS_UNICODE_VERSION;

/// A script, by its four-letter ISO 15924 code (`Latn`, `Arab`, `Zyyy`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Script(pub [u8; 4]);

impl Script {
    /// `Zyyy`: characters shared by several scripts (digits, punctuation).
    pub const COMMON: Script = Script(*b"Zyyy");
    /// `Zinh`: marks that take the script of the character they follow.
    pub const INHERITED: Script = Script(*b"Zinh");
    /// `Zzzz`: unassigned code points.
    pub const UNKNOWN: Script = Script(*b"Zzzz");
    /// `Arab`.
    pub const ARABIC: Script = Script(*b"Arab");
    /// `Hebr`.
    pub const HEBREW: Script = Script(*b"Hebr");
    /// `Latn`.
    pub const LATIN: Script = Script(*b"Latn");
    /// `Mong`.
    pub const MONGOLIAN: Script = Script(*b"Mong");
    /// `Syrc`.
    pub const SYRIAC: Script = Script(*b"Syrc");

    /// The ISO 15924 code.
    #[must_use]
    #[inline]
    pub const fn code(self) -> [u8; 4] {
        self.0
    }
}

/// The Unicode `General_Category` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum GeneralCategory {
    /// `Lu`.
    UppercaseLetter,
    /// `Ll`.
    LowercaseLetter,
    /// `Lt`.
    TitlecaseLetter,
    /// `Lm`.
    ModifierLetter,
    /// `Lo`.
    OtherLetter,
    /// `Mn`.
    NonspacingMark,
    /// `Mc`.
    SpacingMark,
    /// `Me`.
    EnclosingMark,
    /// `Nd`.
    DecimalNumber,
    /// `Nl`.
    LetterNumber,
    /// `No`.
    OtherNumber,
    /// `Pc`.
    ConnectorPunctuation,
    /// `Pd`.
    DashPunctuation,
    /// `Ps`.
    OpenPunctuation,
    /// `Pe`.
    ClosePunctuation,
    /// `Pi`.
    InitialPunctuation,
    /// `Pf`.
    FinalPunctuation,
    /// `Po`.
    OtherPunctuation,
    /// `Sm`.
    MathSymbol,
    /// `Sc`.
    CurrencySymbol,
    /// `Sk`.
    ModifierSymbol,
    /// `So`.
    OtherSymbol,
    /// `Zs`.
    SpaceSeparator,
    /// `Zl`.
    LineSeparator,
    /// `Zp`.
    ParagraphSeparator,
    /// `Cc`.
    Control,
    /// `Cf`.
    Format,
    /// `Cs`.
    Surrogate,
    /// `Co`.
    PrivateUse,
    /// `Cn`.
    Unassigned,
}

impl GeneralCategory {
    const ALL: [GeneralCategory; 30] = [
        Self::UppercaseLetter,
        Self::LowercaseLetter,
        Self::TitlecaseLetter,
        Self::ModifierLetter,
        Self::OtherLetter,
        Self::NonspacingMark,
        Self::SpacingMark,
        Self::EnclosingMark,
        Self::DecimalNumber,
        Self::LetterNumber,
        Self::OtherNumber,
        Self::ConnectorPunctuation,
        Self::DashPunctuation,
        Self::OpenPunctuation,
        Self::ClosePunctuation,
        Self::InitialPunctuation,
        Self::FinalPunctuation,
        Self::OtherPunctuation,
        Self::MathSymbol,
        Self::CurrencySymbol,
        Self::ModifierSymbol,
        Self::OtherSymbol,
        Self::SpaceSeparator,
        Self::LineSeparator,
        Self::ParagraphSeparator,
        Self::Control,
        Self::Format,
        Self::Surrogate,
        Self::PrivateUse,
        Self::Unassigned,
    ];

    /// `Mn`, `Mc` or `Me`.
    #[must_use]
    #[inline]
    pub const fn is_mark(self) -> bool {
        matches!(
            self,
            Self::NonspacingMark | Self::SpacingMark | Self::EnclosingMark
        )
    }

    /// `Lu`, `Ll`, `Lt`, `Lm` or `Lo`.
    #[must_use]
    #[inline]
    pub const fn is_letter(self) -> bool {
        matches!(
            self,
            Self::UppercaseLetter
                | Self::LowercaseLetter
                | Self::TitlecaseLetter
                | Self::ModifierLetter
                | Self::OtherLetter
        )
    }
}

/// The value of the run table entry that covers `c`: the last entry whose
/// start is at or below it. Entry 0 starts at U+0000, so one always does.
#[inline]
fn run_value(runs: &[u32], c: char) -> u8 {
    let cp = u32::from(c);
    let at = runs.partition_point(|&e| (e >> 8) <= cp);
    let entry = at.checked_sub(1).and_then(|i| runs.get(i)).copied();
    entry.map_or(0, |e| (e & 0xFF) as u8)
}

/// The `Script` property of `c`.
#[must_use]
#[inline]
pub fn script(c: char) -> Script {
    let index = usize::from(run_value(&SCRIPT_RUNS, c));
    SCRIPT_CODES
        .get(index)
        .copied()
        .map_or(Script::UNKNOWN, Script)
}

/// Returns an iterator of `(first_codepoint, script)` runs in ascending order from `U+0000`.
#[inline]
pub fn script_runs() -> impl Iterator<Item = (u32, Script)> {
    SCRIPT_RUNS.iter().map(|&e| {
        let index = usize::from((e & 0xFF) as u8);
        let script = SCRIPT_CODES
            .get(index)
            .copied()
            .map_or(Script::UNKNOWN, Script);
        (e >> 8, script)
    })
}

/// The `General_Category` property of `c`.
#[must_use]
#[inline]
pub fn general_category(c: char) -> GeneralCategory {
    let index = usize::from(run_value(&GENERAL_CATEGORY_RUNS, c));
    GeneralCategory::ALL
        .get(index)
        .copied()
        .unwrap_or(GeneralCategory::Unassigned)
}

#[cfg(test)]
mod tests {
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
}
