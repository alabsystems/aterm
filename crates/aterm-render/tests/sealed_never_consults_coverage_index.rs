// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verifies that a sealed renderer generation never builds or queries the font-coverage index.

use aterm_render::{Renderer, Theme};

const PROBES: &[char] = &[
    '\u{4F60}',  // 你 — CJK Unified Ideograph (broad chain / native-CJK tier)
    '\u{D55C}',  // 한 — Hangul syllable
    '\u{E0A0}',  // Nerd Font powerline branch (PUA; bundled backstop)
    '\u{1F680}', // 🚀 — colour emoji
    '\u{23F8}',  // ⏸ — symbol slot
    '\u{2AFF}',  // ⫿ — math operator (Arial Unicode / STIX territory)
    '\u{0E01}',  // ก — Thai
    '\u{FFFF}',  // noncharacter: the honest `.notdef`
];

#[test]
fn a_sealed_system_generation_never_builds_or_queries_the_coverage_index() {
    if !crate::alone::in_own_process(
        module_path!(),
        "a_sealed_system_generation_never_builds_or_queries_the_coverage_index",
    ) {
        return;
    }
    let mut r = Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    assert!(
        !aterm_render::font_coverage_index_built(),
        "precondition: building a renderer must not build the coverage index"
    );
    r.seal_admitted_font_sources();
    assert!(r.admitted_font_sources_sealed());

    for &ch in PROBES {
        let key = r.glyph_key(ch);
        let img = r.glyph_image(key);
        let _ = (img.width(), img.height(), key.source);
    }
    for &ch in PROBES {
        let key = r.glyph_key_styled(ch, aterm_render::StyleBits::BOLD);
        let _ = r.glyph_image(key);
    }

    assert_eq!(
        aterm_render::font_coverage_scan_queries(),
        0,
        "a sealed generation consulted runtime_fallback_scan_candidates — the \
         RuntimeDecisions tier leaked past the seal"
    );
    assert!(
        !aterm_render::font_coverage_index_built(),
        "a sealed generation built the font-coverage index — ~700 MB of system-font \
         reads on the render thread that the seal is supposed to make impossible"
    );

    let mut unsealed = Renderer::from_system(18.0, Theme::default()).expect("resolved once");
    unsealed.debug_block_on_lazy_fallbacks();
    let key = unsealed.glyph_key('\u{FFFF}');
    let _ = unsealed.glyph_image(key);
    assert!(
        aterm_render::font_coverage_scan_queries() > 0,
        "control: an unsealed generation never reached runtime_fallback_scan_candidates, \
         so the sealed half's zero is vacuous"
    );
    assert!(
        aterm_render::font_coverage_index_built(),
        "control: the scan ran but the index was not built"
    );
}
