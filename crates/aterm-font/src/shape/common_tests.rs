// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/common.rs (`Direction`) and
// src/hb/tag.rs (the script half of `tags_from_script_and_language`) and
// modified by the aterm project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

use super::*;

fn tags(code: &[u8; 4]) -> Vec<[u8; 4]> {
    script_tags(Some(Script(*code)))
        .into_iter()
        .map(Tag::to_bytes)
        .collect()
}

#[test]
fn script_tags_try_new_then_old() {
    assert_eq!(tags(b"Latn"), [*b"latn"]);
    assert_eq!(tags(b"Arab"), [*b"arab"]);
    assert_eq!(tags(b"Deva"), [*b"dev3", *b"dev2", *b"deva"]);
    assert_eq!(tags(b"Mymr"), [*b"mym2", *b"mymr"]);
    assert_eq!(tags(b"Hira"), [*b"kana"]);
    assert_eq!(tags(b"Laoo"), [*b"lao "]);
    assert!(script_tags(None).is_empty());
}

#[test]
fn directions_of_scripts() {
    assert_eq!(
        Direction::from_script(Script::ARABIC),
        Some(Direction::RightToLeft)
    );
    assert_eq!(
        Direction::from_script(Script::LATIN),
        Some(Direction::LeftToRight)
    );
    assert_eq!(Direction::from_script(Script(*b"Runr")), None);
}
