// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An open list's marking (docs/PLAN.md SF-37), on made-up words.

use super::*;

/// A typed word marks the word it is before a longer word that starts with it, wherever that
/// longer word stands; a part of a word marks the first word it starts; none marks nothing.
/// NEGATIVE CONTROL: with `Choice::follow` marking the first word that starts with what was
/// typed (its exact pass removed), `release` marks `release-candidate`: `` left:
/// Some("release-candidate") right: Some("release") ``.
#[test]
fn a_typed_word_marks_itself_before_a_longer_word_it_starts() {
    let mut choice = Choice {
        words: ["release-candidate", "release", "spare"]
            .map(String::from)
            .to_vec(),
        said: ["release-candidate", "release", "spare"]
            .map(String::from)
            .to_vec(),
        many: false,
        whole: false,
        at: None,
        wrote: String::new(),
    };
    choice.follow("Release ");
    assert_eq!(choice.marked(), Some("release"));
    choice.follow("rel");
    assert_eq!(choice.marked(), Some("release-candidate"));
    choice.follow("x");
    assert_eq!(choice.marked(), None);
}
