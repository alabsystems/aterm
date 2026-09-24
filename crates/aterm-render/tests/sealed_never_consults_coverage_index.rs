// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! REACHABILITY PIN: A SEALED GENERATION NEVER BUILDS THE FONT-COVERAGE INDEX.
//!
//! `font_coverage_index` reads EVERY system font whole (~373 files, ~700 MB on
//! a Mac, the 192 MB emoji collection included) and walks each cmap, to serve
//! `runtime_fallback_scan_candidates` — the `Tier::RuntimeDecisions` lane that
//! `font_chain` removes from every SEALED policy. Every GUI renderer is sealed
//! before its first pixel, so the GUI's old `aterm-font-warm` thread built a
//! table no GUI process could consult; it was deleted on that argument, and
//! this test is the argument made checkable.
//!
//! The pin drives a sealed system generation through exactly the code points
//! that USED to reach the scan — CJK (broad chain), a Nerd-Font PUA icon
//! (embedded backstop), a colour emoji, a math symbol and a noncharacter (the
//! honest `.notdef`) — rasterizing each, and then asks the process whether the
//! index was ever built or queried. Its own process (an integration test
//! binary with one test), so no sibling's UNSEALED renderer can bump the
//! counters behind it.
//!
//! If this ever goes red the warm's deletion is wrong for the sealed path and
//! the seal is leaking a pathname scan onto the render thread — which is the
//! freeze the seal exists to prevent, not a perf regression to re-warm.

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
    let Some(mut r) = Renderer::from_system(18.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    assert!(
        !aterm_render::font_coverage_index_built(),
        "precondition: building a renderer must not build the coverage index"
    );
    r.seal_admitted_font_sources();
    assert!(r.admitted_font_sources_sealed());

    for &ch in PROBES {
        let key = r.glyph_key(ch);
        let img = r.glyph_image(key);
        // Touch the raster so the whole resolve → rasterize path runs, exactly
        // as a frame would drive it.
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

    // NON-VACUITY CONTROL, in the same process and AFTER the sealed half so
    // the counters above were read at zero: an UNSEALED generation asked for a
    // code point no face covers must reach the scan, or the zero above proves
    // nothing about the seal. This half is what the deleted GUI warm used to
    // pre-pay — and it is the whole-font-tree read, so it runs exactly once,
    // here, last.
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
