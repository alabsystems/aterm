// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! DIFFERENTIAL ORACLE for the in-tree 8x8 glyph table that retired the
//! `font8x8` crate from the shipped graph.
//!
//! The retired crate is kept as a `[dev-dependencies]` entry for exactly this
//! file. A dev-dependency never enters the shipped graph, so the evidence is
//! free — and here it is not a sample or a spot check: the assertion runs over
//! EVERY code point in Unicode, so "the copy is faithful" stops being a claim
//! about the moment the table was generated and becomes a property the test
//! suite re-establishes on every run.

use aterm_effects::matrix_rain::rom::material_bitmap;
use font8x8::{
    BASIC_FONTS, BLOCK_FONTS, BOX_FONTS, GREEK_FONTS, HIRAGANA_FONTS, LATIN_FONTS, MISC_FONTS,
    UnicodeFonts,
};

/// The lookup PHOSPHOR used before the table moved in-tree: seven tables in
/// priority order, with blank glyphs treated as "not covered".
///
/// Deliberately spelled out here rather than imported, because it is the thing
/// being checked against — if the shipped code and the oracle shared a helper,
/// the test would only prove the helper agrees with itself.
fn oracle(c: char) -> Option<[u8; 8]> {
    BASIC_FONTS
        .get(c)
        .or_else(|| LATIN_FONTS.get(c))
        .or_else(|| GREEK_FONTS.get(c))
        .or_else(|| BOX_FONTS.get(c))
        .or_else(|| BLOCK_FONTS.get(c))
        .or_else(|| HIRAGANA_FONTS.get(c))
        .or_else(|| MISC_FONTS.get(c))
        .filter(|bitmap| bitmap.iter().any(|&row| row != 0))
}

#[test]
fn the_copied_table_matches_the_crate_over_all_of_unicode() {
    let mut covered = 0usize;
    for cp in 0u32..=0x0010_FFFF {
        // Surrogates are not scalar values; nothing can look one up.
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        let mine = material_bitmap(c);
        let theirs = oracle(c);
        assert_eq!(
            mine, theirs,
            "U+{cp:04X}: in-tree table says {mine:?}, font8x8 says {theirs:?}"
        );
        if theirs.is_some() {
            covered += 1;
        }
    }
    assert_eq!(
        covered, 499,
        "the supported glyph set changed size — regenerate the table deliberately, \
         do not weaken this number"
    );
}
