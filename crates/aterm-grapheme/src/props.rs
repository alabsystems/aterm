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
#[path = "props_tests.rs"]
mod tests;
