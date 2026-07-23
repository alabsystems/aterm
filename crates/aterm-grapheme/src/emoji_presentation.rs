// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The Unicode `Emoji_Presentation` property — the single source of truth for
//! whether a bare code point (no variation selector) defaults to **emoji**
//! (colour, wide) or **text** (monochrome, narrow) presentation.
//!
//! This is the property real terminals gate colour-vs-mono font selection on:
//!   - Ghostty derives default presentation from `uucode.get(.is_emoji_presentation, cp)`
//!     (`src/font/CodepointResolver.zig`): `.emoji` iff the property is set, else `.text`.
//!   - iTerm2 tests membership in `emojiWithDefaultEmojiPresentation`
//!     (`sources/Categories/NSCharacterSet+iTerm.m`) before ever consulting the
//!     emoji font (`iTermAttributedStringBuilder.m`).
//!
//! Per UTS #51, a code point with `Emoji_Presentation=No` defaults to TEXT even
//! if it is `Emoji=Yes` (e.g. U+23FA ⏺ BLACK CIRCLE FOR RECORD — `Emoji=Yes`,
//! `Emoji_Presentation=No`), and only renders as colour emoji when followed by
//! VS16 (U+FE0F). Coverage of a colour glyph by the emoji font is NOT sufficient
//! to choose emoji presentation — this property (or an explicit VS16) is the gate.
//!
//! Contrast with [`crate::is_emoji_char`], which answers the broader, fuzzier
//! "is this in an emoji-ish block" question used for grapheme-width clustering.
//! `is_emoji_presentation` is the precise, spec-exact default-presentation gate;
//! the two are deliberately different sets and must not be conflated.

/// `Emoji_Presentation=Yes` ranges, inclusive `(lo, hi)`, sorted and disjoint.
///
/// Source: Unicode 16.0.0 `emoji-data.txt` (the `Emoji_Presentation` property),
/// the same UCD version the width/GCB tables in [`crate::tables`] are built from.
/// Extracted and range-merged from the canonical data file; verified by the
/// boundary tests below. Update by re-extracting from the matching UCD release.
static EMOJI_PRESENTATION: &[(u32, u32)] = &[
    (0x231A, 0x231B),
    (0x23E9, 0x23EC),
    (0x23F0, 0x23F0),
    (0x23F3, 0x23F3),
    (0x25FD, 0x25FE),
    (0x2614, 0x2615),
    (0x2648, 0x2653),
    (0x267F, 0x267F),
    (0x2693, 0x2693),
    (0x26A1, 0x26A1),
    (0x26AA, 0x26AB),
    (0x26BD, 0x26BE),
    (0x26C4, 0x26C5),
    (0x26CE, 0x26CE),
    (0x26D4, 0x26D4),
    (0x26EA, 0x26EA),
    (0x26F2, 0x26F3),
    (0x26F5, 0x26F5),
    (0x26FA, 0x26FA),
    (0x26FD, 0x26FD),
    (0x2705, 0x2705),
    (0x270A, 0x270B),
    (0x2728, 0x2728),
    (0x274C, 0x274C),
    (0x274E, 0x274E),
    (0x2753, 0x2755),
    (0x2757, 0x2757),
    (0x2795, 0x2797),
    (0x27B0, 0x27B0),
    (0x27BF, 0x27BF),
    (0x2B1B, 0x2B1C),
    (0x2B50, 0x2B50),
    (0x2B55, 0x2B55),
    (0x1F004, 0x1F004),
    (0x1F0CF, 0x1F0CF),
    (0x1F18E, 0x1F18E),
    (0x1F191, 0x1F19A),
    (0x1F1E6, 0x1F1FF),
    (0x1F201, 0x1F201),
    (0x1F21A, 0x1F21A),
    (0x1F22F, 0x1F22F),
    (0x1F232, 0x1F236),
    (0x1F238, 0x1F23A),
    (0x1F250, 0x1F251),
    (0x1F300, 0x1F320),
    (0x1F32D, 0x1F335),
    (0x1F337, 0x1F37C),
    (0x1F37E, 0x1F393),
    (0x1F3A0, 0x1F3CA),
    (0x1F3CF, 0x1F3D3),
    (0x1F3E0, 0x1F3F0),
    (0x1F3F4, 0x1F3F4),
    (0x1F3F8, 0x1F43E),
    (0x1F440, 0x1F440),
    (0x1F442, 0x1F4FC),
    (0x1F4FF, 0x1F53D),
    (0x1F54B, 0x1F54E),
    (0x1F550, 0x1F567),
    (0x1F57A, 0x1F57A),
    (0x1F595, 0x1F596),
    (0x1F5A4, 0x1F5A4),
    (0x1F5FB, 0x1F64F),
    (0x1F680, 0x1F6C5),
    (0x1F6CC, 0x1F6CC),
    (0x1F6D0, 0x1F6D2),
    (0x1F6D5, 0x1F6D7),
    (0x1F6DC, 0x1F6DF),
    (0x1F6EB, 0x1F6EC),
    (0x1F6F4, 0x1F6FC),
    (0x1F7E0, 0x1F7EB),
    (0x1F7F0, 0x1F7F0),
    (0x1F90C, 0x1F93A),
    (0x1F93C, 0x1F945),
    (0x1F947, 0x1F9FF),
    (0x1FA70, 0x1FA7C),
    (0x1FA80, 0x1FA89),
    (0x1FA8F, 0x1FAC6),
    (0x1FACE, 0x1FADC),
    (0x1FADF, 0x1FAE9),
    (0x1FAF0, 0x1FAF8),
];

/// Whether `c` has the Unicode `Emoji_Presentation` property — i.e. a BARE
/// occurrence (no following VS16) defaults to **emoji** (colour, wide)
/// presentation rather than text.
///
/// This is the precise gate a renderer should consult before selecting a
/// colour-emoji face for a code point with no explicit VS16: `true` means the
/// colour face is the correct default (🚀 ☔ ✨ ⭐); `false` means text is the
/// default even when the colour font happens to carry a glyph (⏺ ⏸ ⏹ — these
/// are `Emoji=Yes` but `Emoji_Presentation=No`, and need VS16 to go colour).
///
/// REQUIRES: nothing (total over all `char`).
/// ENSURES: returns `true` iff `c`'s scalar value is in the Unicode
/// `Emoji_Presentation=Yes` set.
#[must_use]
pub fn is_emoji_presentation(c: char) -> bool {
    let cp = c as u32;
    // The table is sorted and disjoint, so a single binary search by range
    // settles membership: find the range whose `lo <= cp`, then check `cp <= hi`.
    EMOJI_PRESENTATION
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                std::cmp::Ordering::Greater
            } else if cp > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// Whether `c` is eligible for VS16 emoji-presentation WIDENING: it has the
/// Unicode `Emoji` property but defaults to text presentation (width 1), so a
/// following VS16 (U+FE0F) converts it to a 2-cell emoji-presentation cell.
///
/// This is the single source of the eligibility set consumed by BOTH the live
/// write path (`aterm-core` `widen_previous_cell_for_vs16`) and the scrollback
/// materialization/reflow paths (`aterm-grid`), so a scrolled-back `❤️` keeps
/// exactly the width the live grid gave it. Covers the common subset defined
/// in Unicode Emoji data (UTS #51 emoji-data.txt, `Emoji` property).
///
/// REQUIRES: nothing (total over all `char`).
/// ENSURES: `true` iff a bare-narrow occurrence of `c` should widen to 2 cells
/// when followed by VS16.
#[must_use]
pub fn is_vs16_emoji_capable(c: char) -> bool {
    let cp = c as u32;
    match cp {
        // Number sign, asterisk, digits 0-9 (keycap base characters)
        0x0023 | 0x002A | 0x0030..=0x0039 => true,
        // Copyright, Registered
        0x00A9 | 0x00AE => true,
        // Letterlike Symbols
        0x2122 => true, // Trade Mark
        0x2139 => true, // Information Source
        // Arrows — select emoji
        0x2194..=0x2199 => true, // Directional arrows
        0x21A9 | 0x21AA => true, // Curved arrows
        // Miscellaneous Technical — select emoji
        0x231A | 0x231B => true, // Watch, Hourglass
        0x2328 => true,          // Keyboard
        0x23CF => true,          // Eject
        0x23E9..=0x23F3 => true, // Various media controls
        0x23F8..=0x23FA => true, // Pause, Record, etc.
        // Double Exclamation Mark, Exclamation Question Mark
        0x203C | 0x2049 => true,
        // Enclosed Alphanumerics
        0x24C2 => true, // Circled M
        // Geometric Shapes
        0x25AA | 0x25AB => true, // Small squares
        0x25B6 | 0x25C0 => true, // Play buttons
        0x25FB..=0x25FE => true, // Medium squares
        // Miscellaneous Symbols (U+2600-U+26FF) — large emoji block
        0x2600..=0x26FF => true,
        // Dingbats (U+2700-U+27BF) — many emoji-capable chars
        0x2700..=0x27BF => true,
        // Supplemental Arrows-B
        0x2934 | 0x2935 => true, // Curved arrows
        // CJK Symbols
        0x3030 => true, // Wavy Dash
        0x303D => true, // Part Alternation Mark
        // Enclosed CJK Letters
        0x3297 => true, // Circled Ideograph Congratulation
        0x3299 => true, // Circled Ideograph Secret
        // Any SMP char that got width 1 from our lookup (rare edge case)
        0x1F000..=0x1FFFF => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_text_emoji_are_not_emoji_presentation() {
        // Emoji=Yes but Emoji_Presentation=No: default TEXT, need VS16 for colour.
        // U+23FA is the exact code point Claude Code prints before each line.
        for c in ['\u{23F8}', '\u{23F9}', '\u{23FA}'] {
            assert!(
                !is_emoji_presentation(c),
                "{c:?} (U+{:04X}) is Emoji_Presentation=No — must default to text",
                c as u32
            );
        }
    }

    #[test]
    fn non_emoji_symbols_are_not_emoji_presentation() {
        // Plain symbols, not emoji at all: bullet, black circle, six-pointed star.
        for c in ['\u{2022}', '\u{25CF}', '\u{2736}', 'a', '世'] {
            assert!(!is_emoji_presentation(c), "{c:?} is not an emoji");
        }
    }

    #[test]
    fn default_emoji_are_emoji_presentation() {
        // Emoji_Presentation=Yes: default colour/wide even without VS16.
        for c in [
            '\u{231A}',  // ⌚ watch
            '\u{2614}',  // ☔ umbrella with rain
            '\u{2728}',  // ✨ sparkles
            '\u{2B50}',  // ⭐ star
            '\u{26AA}',  // ⚪ medium white circle
            '\u{1F680}', // 🚀 rocket
            '\u{1F600}', // 😀 grinning face
        ] {
            assert!(
                is_emoji_presentation(c),
                "{c:?} (U+{:04X}) is Emoji_Presentation=Yes — must default to emoji",
                c as u32
            );
        }
    }

    #[test]
    fn table_is_sorted_and_disjoint() {
        // The binary search depends on this invariant; assert it holds so a
        // future hand-edit of the table can't silently break membership.
        for w in EMOJI_PRESENTATION.windows(2) {
            assert!(w[0].0 <= w[0].1, "range not ordered lo<=hi: {:?}", w[0]);
            assert!(
                w[0].1 < w[1].0,
                "ranges not strictly increasing/disjoint: {:?} then {:?}",
                w[0],
                w[1]
            );
        }
        let last = EMOJI_PRESENTATION.last().unwrap();
        assert!(last.0 <= last.1, "final range not ordered: {last:?}");
    }

    #[test]
    fn range_boundaries_are_exact() {
        // Spot-check that membership is exact at range edges, not just interiors.
        assert!(is_emoji_presentation('\u{231A}')); // lo of first range
        assert!(is_emoji_presentation('\u{231B}')); // hi of first range
        assert!(!is_emoji_presentation('\u{2319}')); // just below
        assert!(!is_emoji_presentation('\u{231C}')); // just above
        assert!(is_emoji_presentation('\u{1FAF8}')); // hi of last range
        assert!(!is_emoji_presentation('\u{1FAF9}')); // just above last
    }
}
