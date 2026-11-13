// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{GITHUB_RELEASE_BODY_LIMIT, bound_release_body, release_notes_document};

/// One that does not fit ends on a whole entry, under the limit, saying so.
#[test]
fn an_oversized_body_is_cut_at_a_section_and_says_it_was() {
    let entry = "### Section\n\n- an entry long enough to matter, repeated to overflow\n\n";
    let body = entry.repeat(4000);
    assert!(
        body.len() > GITHUB_RELEASE_BODY_LIMIT,
        "fixture must actually overflow: {} bytes",
        body.len()
    );
    let bounded = bound_release_body(&body);
    assert!(
        bounded.len() <= GITHUB_RELEASE_BODY_LIMIT,
        "bounded body is still {} bytes, over the {GITHUB_RELEASE_BODY_LIMIT} GitHub takes",
        bounded.len()
    );
    let kept = bounded
        .split("\n\n---\n\n*These notes")
        .next()
        .unwrap_or_default();
    assert!(
        kept.ends_with("\n\n") || kept.ends_with('\n'),
        "the kept text must end on a whole entry"
    );
}

/// A budget that lands inside a multi-byte character does not panic: em dashes cover
/// the whole budget neighbourhood across several pads, rather than one guessed offset.
#[test]
fn a_cut_that_falls_inside_a_character_does_not_panic() {
    for pad in 0..8usize {
        let body = format!(
            "### Section\n\n{}{}\n",
            "a".repeat(pad),
            "—x".repeat(GITHUB_RELEASE_BODY_LIMIT)
        );
        let bounded = bound_release_body(&body);
        assert!(
            super::github_length(&bounded) <= GITHUB_RELEASE_BODY_LIMIT,
            "pad {pad}: bounded body is {} units",
            super::github_length(&bounded)
        );
    }
}

/// The limit counts characters, not bytes: em dashes are three bytes each.
#[test]
fn the_limit_counts_characters_not_bytes() {
    let body = "—".repeat(GITHUB_RELEASE_BODY_LIMIT - 1);
    assert!(
        body.len() > GITHUB_RELEASE_BODY_LIMIT,
        "fixture must overflow a BYTE budget: {} bytes",
        body.len()
    );
    assert_eq!(
        bound_release_body(&body),
        body,
        "a body inside the character limit must survive whole"
    );
}

/// The whole document, preamble plus body, lands under the limit: GitHub counts the
/// document, not the section inside it.
#[test]
fn the_assembled_document_fits_what_github_accepts() {
    let body = "### Section\n\n- entry — with em dashes — in UTF-8\n\n".repeat(4000);
    let doc = release_notes_document("0.87.0", &body);
    assert!(
        super::github_length(&doc) <= GITHUB_RELEASE_BODY_LIMIT,
        "assembled release notes are {} UTF-16 units ({} bytes), over the \
         {GITHUB_RELEASE_BODY_LIMIT} GitHub takes",
        super::github_length(&doc),
        doc.len()
    );
}

/// Bounding an already-bounded body is a no-op, byte for byte, so the POST can re-bound
/// whatever it reads off disk.
#[test]
fn bounding_an_already_bounded_body_changes_nothing() {
    let overflowing = "### Section\n\n- an entry long enough to matter\n\n".repeat(5000);
    assert!(overflowing.len() > GITHUB_RELEASE_BODY_LIMIT);
    let once = bound_release_body(&overflowing);
    let twice = bound_release_body(&once);
    assert_eq!(once, twice, "the bound is not idempotent");
    let doc = release_notes_document("0.87.0", &overflowing);
    assert_eq!(doc, bound_release_body(&doc));
}
