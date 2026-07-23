// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Paragraph direction hint for BiDi text layout.
//!
//! Extracted from `aterm-bidi` to break the dependency cycle between
//! `aterm-core` (which stores `ParagraphDirection` in `TerminalModes`)
//! and `aterm-bidi` (which implements the resolution algorithm).

/// Hint for determining paragraph direction.
///
/// Controls how the Unicode Bidirectional Algorithm determines base direction.
/// Set via SCP (Select Character Path) — CSI n SPACE k.
#[non_exhaustive]
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ParagraphDirection {
    /// Auto-detect from first strong character, default to LTR.
    #[default]
    Auto = 0,
    /// Auto-detect from first strong character, default to RTL.
    AutoRtl = 1,
    /// Force left-to-right.
    Ltr = 2,
    /// Force right-to-left.
    Rtl = 3,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify ParagraphDirection discriminants match checkpoint wire format (#7278).
    #[test]
    fn paragraph_direction_discriminants_match_wire_format() {
        assert_eq!(ParagraphDirection::Auto as u8, 0);
        assert_eq!(ParagraphDirection::AutoRtl as u8, 1);
        assert_eq!(ParagraphDirection::Ltr as u8, 2);
        assert_eq!(ParagraphDirection::Rtl as u8, 3);
    }
}
