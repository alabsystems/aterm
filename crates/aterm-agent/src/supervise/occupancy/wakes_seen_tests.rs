// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for wake door words, claim answers, and book head ordering.

use super::*;

fn every_claim() -> Vec<WakeClaim> {
    [
        WakeClaim::Granted(WakeFacts::default()),
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

#[test]
fn each_word_reads_back_as_what_wrote_it() {
    for door in WakeDoor::ALL {
        assert_eq!(WakeDoor::of_word(door.word()), Some(door));
    }
    let claims = every_claim();
    let words: std::collections::BTreeSet<&str> = claims.iter().map(|c| c.word()).collect();
    assert_eq!(words.len(), claims.len(), "{words:?}");
    for claim in claims {
        assert_eq!(WakeClaim::of_word(claim.word()), Some(claim));
        assert_eq!(
            claim.lets(),
            matches!(claim, WakeClaim::Granted(_)),
            "{claim:?}"
        );
    }
    for word in ["", "refused:", "refused:tired", "granted ", "Gone", "loop"] {
        assert_eq!(WakeClaim::of_word(word), None, "{word:?}");
        assert_eq!(WakeDoor::of_word(word), None, "{word:?}");
    }
}

#[test]
fn the_books_words_read_back() {
    fn back<T: Copy + PartialEq + std::fmt::Debug>(
        all: &[T],
        word: fn(T) -> &'static str,
        of: fn(&str) -> Option<T>,
    ) {
        let words: std::collections::BTreeSet<&str> = all.iter().map(|t| word(*t)).collect();
        assert_eq!(words.len(), all.len(), "{words:?}");
        for t in all {
            assert_eq!(of(word(*t)), Some(*t));
        }
        for bad in ["", "Ready", "yours ", "later"] {
            assert_eq!(of(bad), None, "{bad:?}");
        }
    }
    back(&Declarer::ALL, Declarer::word, Declarer::of_word);
    back(&Prio::ALL, Prio::word, Prio::of_word);
    back(&AtomKind::ALL, AtomKind::word, AtomKind::of_word);
    back(&ExpireWhy::ALL, ExpireWhy::word, ExpireWhy::of_word);
}

fn wake(ord: u16, prio: Prio, state: WakeState, await_only: bool) -> WakeSeen {
    WakeSeen {
        ord,
        class: Declarer::Agent,
        prio,
        state,
        declared_ms: 1_000,
        ready_since_ms: None,
        kinds: vec![AtomKind::File],
        need: None,
        need_slots: false,
        need_met: true,
        await_only,
    }
}

#[test]
fn the_head_is_critical_first_then_the_lowest_ordinal() {
    let ready = WakeState::Ready;
    let seen = WakesSeen {
        wakes: vec![
            wake(1, Prio::Normal, WakeState::Pending, false),
            wake(2, Prio::Normal, ready, true),
            wake(3, Prio::Normal, ready, false),
            wake(4, Prio::Normal, ready, false),
            wake(5, Prio::Critical, WakeState::Answered { at_ms: 9 }, false),
        ],
    };
    assert_eq!(seen.head_due().map(|w| w.ord), Some(5));
    assert_eq!((seen.pending(), seen.ready()), (5, 4));
    let mut normal = seen.clone();
    normal.wakes.pop();
    assert_eq!(
        normal.head_due().map(|w| w.ord),
        Some(3),
        "the lowest ordinal, not await-only"
    );
    let none = WakesSeen {
        wakes: vec![
            wake(1, Prio::Critical, WakeState::Pending, false),
            wake(2, Prio::Normal, ready, true),
            wake(
                3,
                Prio::Normal,
                WakeState::Claimed {
                    door: WakeDoor::Break,
                    at_ms: 1,
                },
                false,
            ),
        ],
    };
    assert!(none.head_due().is_none());
}

#[test]
fn a_wakes_episode_is_its_declaration_and_ordinal() {
    let one = wake(1, Prio::Normal, WakeState::Ready, false);
    assert_eq!(
        one.episode(),
        aterm_hash::fnv1a_64(b"wake@v1 builtin 1000 1")
    );
    let mut other = one.clone();
    other.ord = 2;
    assert_ne!(other.episode(), one.episode());
    other.ord = 1;
    other.declared_ms = 1_001;
    assert_ne!(other.episode(), one.episode());
}
