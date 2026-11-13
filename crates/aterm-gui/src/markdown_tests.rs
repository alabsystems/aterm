// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn block_markup_becomes_plain_text() {
    for (case, md, want) in [
        (
            "a heading loses its markers and gets a blank separator",
            "Intro line\n### Features\n- a",
            "Intro line\n\nFeatures\n\u{2022} a",
        ),
        (
            "bullets become dots, keeping their indent",
            "- top\n  - nested\n* star\n+ plus",
            "\u{2022} top\n  \u{2022} nested\n\u{2022} star\n\u{2022} plus",
        ),
        (
            "ordered lists keep their numbers",
            "1. first\n2. **second**",
            "1. first\n2. second",
        ),
        (
            "rules drop and blank runs collapse",
            "A\n\n\n---\n\n\nB",
            "A\n\nB",
        ),
    ] {
        assert_eq!(to_plain_text(md), want, "{case}");
    }
}

#[test]
fn inline_emphasis_code_and_links_unwrapped() {
    assert_eq!(
        inline("**bold** and _em_ and `code`"),
        "bold and em and code"
    );
    assert_eq!(
        inline("see [the docs](https://x.y) now"),
        "see the docs now"
    );
    assert_eq!(inline("2 * 3 = 6"), "2 * 3 = 6");
}

#[test]
fn trims_and_is_idempotent_on_plain_text() {
    let plain = "Version 0.5.14\n\nJust some notes.";
    assert_eq!(to_plain_text(plain), plain);
    let once = to_plain_text("### Title\n- x");
    assert_eq!(to_plain_text(&once), once);
}
