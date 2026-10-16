// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A sealed generation never builds or queries the font-coverage index.
//!
//! `font_coverage_index` reads every system font whole (~373 files, ~700 MB on
//! a Mac) to serve `runtime_fallback_scan_candidates`, the
//! `Tier::RuntimeDecisions` lane `font_chain` removes from every sealed policy.
//! Every GUI renderer is sealed before its first pixel, so nothing pre-warms
//! the index; a sealed generation reaching it would be a pathname scan on the
//! render thread, the freeze the seal exists to prevent.
//!
//! The test drives a sealed system generation through one code point per
//! fallback lane, rasterizing each, then reads the process-global counters. It
//! is its own test binary so no sibling's unsealed renderer moves them.

use aterm_render::{Renderer, Theme};

/// One code point per lane the unsealed resolver would scan for.
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
        // Touch the raster so the whole resolve → rasterize path runs.
        let _ = (img.width(), img.height(), key.source);
    }
    // And the bold/italic routing, which has its own memo and its own probe.
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

    // Non-vacuity control, after the sealed half so the counters above read
    // zero: an unsealed generation asked for a code point no face covers must
    // reach the scan. It reads the whole font tree, so it runs once, last.
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
