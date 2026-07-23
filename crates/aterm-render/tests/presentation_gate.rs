// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tier-1 conformance for the colour-presentation gate: the SHIPPING
//! [`aterm_render::select_face`] policy never resolves a default-TEXT code point
//! to the colour-emoji face.
//!
//! This is the ⏺ (U+23FA) fix bound to real code. aterm used to choose the
//! colour-emoji face for ANY code point the monochrome faces missed but that
//! Apple Color Emoji happened to cover — ignoring the Unicode `Emoji_Presentation`
//! property. U+23FA is `Emoji=Yes` but `Emoji_Presentation=No`, so it defaults to
//! text; the reference terminals gate the colour face on that property (iTerm2:
//! `emojiWithDefaultEmojiPresentation` set membership; Ghostty:
//! `uucode.get(.is_emoji_presentation, cp)`), never on raw font coverage.
//!
//! ## Two-tier proof
//!
//! * **Tier-0 (abstract, model-checked by the Trust `ty` compiler)** — the
//!   `PresentationGate` derived model (`aterm_spec::derive::presentation_gate_model`)
//!   carries the `NoColorForText` invariant. `cargo test -p aterm-spec`
//!   (`derived_presentation_gate_proves_and_catches_text_colored_as_emoji`) runs the
//!   REAL `ty` binary over the whole bounded state space: it PROVES the invariant at
//!   `Buggy=0` and CATCHES the old coverage-only gate at `Buggy=1` (counterexample).
//! * **Tier-1 (concrete, this file)** — `select_face` has a finite boolean domain,
//!   so we don't sample: we enumerate ALL 2^6 = 64 input combinations and check the
//!   invariant on every one. That is a complete proof of `NoColorForText` for the
//!   real policy, with a non-vacuous control (the colour face IS reached for a
//!   genuine default-emoji code point).

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

        // THE INVARIANT (the same `NoColorForText` that `ty` model-checks abstractly
        // in aterm-spec): colour face implies emoji presentation. Exhaustive.
        if face == FaceId::ColorEmoji {
            colored_count += 1;
            assert!(
                wants_emoji,
                "select_face resolved to ColorEmoji for a text-presentation code \
                 point (procedural={procedural}, primary={primary}, fallback={fallback}, \
                 symbol={symbol}, color_has={color_has}, wants_emoji={wants_emoji})"
            );
            // When the colour face IS chosen it is precisely because every mono
            // face missed and the colour font covers an emoji-presentation glyph.
            assert!(
                color_has && !procedural && !primary && !fallback && !symbol,
                "ColorEmoji chosen without exhausting the mono faces / colour coverage"
            );
        }
    }

    // NON-VACUOUS: the gate is not trivially "never colour" — a genuine
    // default-emoji code point with colour coverage and no mono glyph DOES reach
    // the colour face. (Mirrors the model's reachable colour state at Buggy=0.)
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
    // ⏺ U+23FA in its real situation on stock macOS: procedural? no. primary mono?
    // miss. broad fallback mono? miss. symbol fallback (STIX)? present. colour
    // font? covers it. wants_emoji? FALSE (Emoji_Presentation=No). Must resolve to
    // the symbol fallback — a MONOCHROME glyph — never the colour face.
    assert_eq!(
        select_face(false, false, false, true, true, false),
        FaceId::SymbolFallback,
        "⏺ with a mono symbol glyph available must use it, not colour"
    );
    // And if even the symbol face misses it (no STIX on the system), it falls to
    // the monochromatized colour glyph — still NOT the colour (Rgba) face.
    assert_eq!(
        select_face(false, false, false, false, true, false),
        FaceId::ColorEmojiMono,
        "⏺ with no mono glyph anywhere must be monochromatized, never colour"
    );
}
