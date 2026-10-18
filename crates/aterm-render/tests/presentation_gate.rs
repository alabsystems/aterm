// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Conformance for the colour-presentation gate: [`aterm_render::select_face`]
//! never resolves a default-text code point to the colour-emoji face.
//!
//! The gate is the Unicode `Emoji_Presentation` property, not font coverage
//! (as in iTerm2 and Ghostty): ⏺ U+23FA is `Emoji=Yes` but
//! `Emoji_Presentation=No`, so it draws as text even though Apple Color Emoji
//! covers it.
//!
//! `select_face` has a finite boolean domain, so this file checks the
//! `NoColorForText` invariant on all 2^6 inputs, with a non-vacuous control.

use aterm_render::{FaceId, select_face};

/// Iterate every (procedural, primary_has, fallback_has, symbol_has, color_has,
/// wants_emoji) tuple over `{false, true}^6`.
fn all_inputs() -> impl Iterator<Item = (bool, bool, bool, bool, bool, bool)> {
    (0u32..64).map(|bits| {
        let b = |i: u32| (bits >> i) & 1u32 == 1;
        (b(0), b(1), b(2), b(3), b(4), b(5))
    })
}

#[test]
fn select_face_never_colors_text_presentation() {
    let mut colored_count = 0usize;
    for (procedural, primary, fallback, symbol, color_has, wants_emoji) in all_inputs() {
        let face = select_face(
            procedural,
            primary,
            fallback,
            symbol,
            color_has,
            wants_emoji,
        );

        // `NoColorForText`: the colour face implies emoji presentation.
        if face == FaceId::ColorEmoji {
            colored_count += 1;
            assert!(
                wants_emoji,
                "select_face resolved to ColorEmoji for a text-presentation code \
                 point (procedural={procedural}, primary={primary}, fallback={fallback}, \
                 symbol={symbol}, color_has={color_has}, wants_emoji={wants_emoji})"
            );
            // And only once every mono face missed and the colour font covers it.
            assert!(
                color_has && !procedural && !primary && !fallback && !symbol,
                "ColorEmoji chosen without exhausting the mono faces / colour coverage"
            );
        }
    }

    // Non-vacuity: a default-emoji code point with colour coverage and no mono
    // glyph does reach the colour face.
    assert!(
        colored_count > 0,
        "select_face never returns ColorEmoji — the gate would be vacuously true"
    );
    assert_eq!(
        select_face(false, false, false, false, true, true),
        FaceId::ColorEmoji,
        "a default-emoji code point (wants_emoji) with colour coverage and no mono \
         glyph must still render in colour"
    );
}

#[test]
fn record_symbol_exact_case_is_not_colored() {
    // ⏺ U+23FA on stock macOS: the primary and broad fallback miss, the symbol
    // fallback (STIX) and the colour font cover it, and Emoji_Presentation=No.
    assert_eq!(
        select_face(false, false, false, true, true, false),
        FaceId::SymbolFallback,
        "⏺ with a mono symbol glyph available must use it, not colour"
    );
    // With no symbol face either, the monochromatized colour glyph, still not
    // the colour face.
    assert_eq!(
        select_face(false, false, false, false, true, false),
        FaceId::ColorEmojiMono,
        "⏺ with no mono glyph anywhere must be monochromatized, never colour"
    );
}
