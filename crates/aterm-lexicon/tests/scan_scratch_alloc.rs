// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A warmed `ScanScratch` scan allocates nothing.

use aterm_lexicon::{Lexicon, Match, ScanOptions, ScanScratch};

use crate::allocations_during;

#[test]
fn warmed_scan_scratch_is_allocation_free_across_every_scanner_lane() {
    let lexicon = Lexicon::with_languages(&["all"]);
    let options = ScanOptions {
        allow_bare_cat: true,
        cjk_single_char: true,
        ignore: None,
    };
    // Ordinary and possessive spaced tokens, a CJK maximal run, Arabic
    // definite-article + one-letter clitic fallbacks, and five result entries.
    let text = "kitty's 子猫 القطة وقطة fuck";
    let mut chars = Vec::new();
    let mut hits: Vec<Match> = Vec::new();
    let mut scratch = ScanScratch::default();

    lexicon.scan_into_with_scratch(text, &options, &mut chars, &mut hits, &mut scratch);
    assert_eq!(hits.len(), 5, "warmup must exercise every scanner lane");

    let allocations = allocations_during(|| {
        lexicon.scan_into_with_scratch(text, &options, &mut chars, &mut hits, &mut scratch);
    });
    assert_eq!(allocations, 0, "warmed scratch scan must not allocate");
}
