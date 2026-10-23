// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Conformance for the colour-presentation gate in [`aterm_render::select_face`].

use aterm_render::{FaceId, select_face};

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

        if face == FaceId::ColorEmoji {
            colored_count += 1;
            assert!(
                wants_emoji,
                "select_face resolved to ColorEmoji for a text-presentation code \
                 point (procedural={procedural}, primary={primary}, fallback={fallback}, \
                 symbol={symbol}, color_has={color_has}, wants_emoji={wants_emoji})"
            );
            assert!(
                color_has && !procedural && !primary && !fallback && !symbol,
                "ColorEmoji chosen without exhausting the mono faces / colour coverage"
            );
        }
    }

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
    assert_eq!(
        select_face(false, false, false, true, true, false),
        FaceId::SymbolFallback,
        "⏺ with a mono symbol glyph available must use it, not colour"
    );
    assert_eq!(
        select_face(false, false, false, false, true, false),
        FaceId::ColorEmojiMono,
        "⏺ with no mono glyph anywhere must be monochromatized, never colour"
    );
}
