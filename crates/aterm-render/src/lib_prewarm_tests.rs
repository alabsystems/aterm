// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn renderer() -> Renderer {
    Renderer::from_bytes(embedded_font(), 16.0, Theme::default()).expect("embedded font")
}

#[test]
fn prewarm_after_seal_warms_regular_and_bold_ascii() {
    let mut r = renderer();
    r.seal_admitted_font_sources();
    assert_eq!(
        r.glyph_cache_len(),
        0,
        "the seal leaves an empty raster memo"
    );
    r.prewarm_ascii();
    let warmed = r.glyph_cache_len();
    assert!(
        warmed >= 2 * 95,
        "95 printable ASCII code points x (regular, bold) must be resident; got {warmed}"
    );
    for ch in ['$', 'a', 'Z', '~', ' '] {
        let regular = r.glyph_key(ch);
        let bold = r.glyph_key_styled(ch, StyleBits::BOLD);
        let _ = r.glyph_image(regular);
        let _ = r.glyph_image(bold);
    }
    assert_eq!(
        r.glyph_cache_len(),
        warmed,
        "a post-warm look-up of a regular or BOLD ASCII key must be a cache hit"
    );
}
