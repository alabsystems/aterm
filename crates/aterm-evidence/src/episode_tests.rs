// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const HOUR: Duration = Duration::from_secs(3_600);

/// Looks that find a hold under one word, then another, then a third holding process, are one
/// episode: its start stays, and a longer horizon never moves the act later; a shorter one brings
/// it nearer.
#[test]
fn a_change_of_word_or_process_keeps_the_episode_and_never_extends_its_horizon() {
    let mut clock = EpisodeClock::default();
    assert_eq!(clock.current(), None);
    let first = clock.held(1_000, 2 * HOUR);
    assert_eq!(
        first,
        Episode {
            since: 1_000,
            horizon_at: 8_200
        }
    );
    assert_eq!(
        clock.held(1_600, 3 * HOUR),
        first,
        "another word, a longer horizon"
    );
    assert_eq!(
        clock.held(2_200, HOUR),
        Episode {
            since: 1_000,
            horizon_at: 4_600
        },
        "a shorter horizon counts from the episode's start"
    );
    assert_eq!(clock.held(3_000, 2 * HOUR).horizon_at, 4_600);
    assert_eq!(clock.current().map(|e| e.since), Some(1_000));
}

/// A hold that clears and comes back within CLEAR_GAP_S is the same episode, however many times;
/// clear for CLEAR_GAP_S it is over, and the next hold starts a new one; progress ends it at once.
#[test]
fn a_gap_shorter_than_the_clear_gap_keeps_the_episode() {
    let mut clock = EpisodeClock::default();
    clock.clear(10);
    assert_eq!(clock.current(), None, "a clear look starts nothing");
    let start = clock.held(100, HOUR);
    for gap in 0..10 {
        let at = 200 + gap * 400;
        clock.clear(at);
        clock.clear(at + CLEAR_GAP_S - 1);
        assert_eq!(clock.current(), Some(start), "gap {gap}: still standing");
        assert_eq!(clock.held(at + CLEAR_GAP_S - 1, HOUR), start, "gap {gap}");
    }
    clock.clear(10_000);
    clock.clear(10_000 + CLEAR_GAP_S);
    assert_eq!(clock.current(), None, "clear for the whole gap");
    assert_eq!(clock.held(10_400, HOUR).since, 10_400);
    clock.clear(10_500);
    assert_eq!(
        clock.held(10_500 + CLEAR_GAP_S, HOUR).since,
        10_500 + CLEAR_GAP_S,
        "a hold seen a whole clear gap after the first clear look starts anew"
    );
    clock.progress();
    assert_eq!(clock.current(), None);
    assert_eq!(clock.held(20_000, HOUR).since, 20_000);
}
