// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/set_digest.rs and modified by the
// aterm project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

use super::*;

#[test]
fn digest_has_no_false_negatives() {
    let mut d = Digest::new();
    for g in [2u16, 70, 300, 65_535] {
        d.add(GlyphId(g));
    }
    d.add_range(GlyphId(1000), GlyphId(1100));
    d.add_range(GlyphId(60), GlyphId(66));
    for g in [2u16, 70, 300, 65_535, 1000, 1050, 1100, 60, 63, 64, 66] {
        assert!(d.may_have_glyph(GlyphId(g)), "{g}");
    }
    let mut one = Digest::new();
    one.add(GlyphId(5));
    assert!(!one.may_have_glyph(GlyphId(6)));
    let mut other = Digest::new();
    other.add(GlyphId(6));
    assert!(!one.may_have(&other));
    other.add(GlyphId(5));
    assert!(one.may_have(&other));
    let mut inverted = Digest::new();
    inverted.add_range(GlyphId(500), GlyphId(10));
    assert!(inverted.may_have_glyph(GlyphId(123)));
}
