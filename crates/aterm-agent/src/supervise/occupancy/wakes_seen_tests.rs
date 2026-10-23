// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The wake doors' words: each reads back as the door or answer that wrote it, and a word no door
//! or answer writes reads as none.

use super::*;

/// Every claim answer, each refusal among them.
fn every_claim() -> Vec<WakeClaim> {
    [
        WakeClaim::Granted,
        WakeClaim::Answered,
        WakeClaim::Busy,
        WakeClaim::Stale,
        WakeClaim::Unmet,
        WakeClaim::Reserved,
        WakeClaim::Machine,
        WakeClaim::Gone,
    ]
    .into_iter()
    .chain(PaceRefusal::ALL.map(WakeClaim::Refused))
    .collect()
}

/// Each door and each claim answer reads back from its word, and the words are distinct; only a
/// grant lets the door tell. NEGATIVE CONTROL: a word none writes (a refusal the host has no word
/// for, a door none is, an empty word) reads as none.
#[test]
fn each_word_reads_back_as_what_wrote_it() {
    for door in WakeDoor::ALL {
        assert_eq!(WakeDoor::of_word(door.word()), Some(door));
    }
    let claims = every_claim();
    let words: std::collections::BTreeSet<String> = claims.iter().map(|c| c.word()).collect();
    assert_eq!(words.len(), claims.len(), "{words:?}");
    for claim in claims {
        assert_eq!(WakeClaim::of_word(&claim.word()), Some(claim));
        assert_eq!(claim.lets(), claim == WakeClaim::Granted, "{claim:?}");
    }
    for word in ["", "refused:", "refused:tired", "granted ", "Gone", "loop"] {
        assert_eq!(WakeClaim::of_word(word), None, "{word:?}");
        assert_eq!(WakeDoor::of_word(word), None, "{word:?}");
    }
}

/// The default host's book is empty.
#[test]
fn the_default_book_is_empty() {
    let seen = WakesSeen::default();
    assert_eq!((seen.pending, seen.ready), (0, 0));
}
