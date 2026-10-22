// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The facts state: told once per episode, and seeded from the ledger without telling twice.

use super::*;

const T0: i64 = 1_760_000_000_000;

fn hold(id: u64) -> EpisodeKey {
    EpisodeKey {
        kind: FactKind::Hold,
        id,
    }
}

/// Each kind's rule reads back as that kind, and a rule that is no fact's is none.
#[test]
fn a_kinds_rule_names_it_and_no_other_rule_does() {
    for kind in FactKind::ALL {
        assert!(kind.rule().starts_with("fact-") && kind.rule().ends_with("@v1"));
        assert_eq!(FactKind::of_rule(kind.rule()), Some(kind));
    }
    for rule in ["harness-turn@v1", "fact-hold@v2", "", "fact-hold"] {
        assert_eq!(FactKind::of_rule(rule), None, "{rule}");
    }
}

/// AN EPISODE IS TOLD ONCE, and only while it is open: a told episode stays told while a reading
/// still holds it, another episode of the kind is untold, and a closed one is forgotten.
#[test]
fn an_episode_is_told_once_and_forgotten_when_it_closes() {
    let mut s = FactsState::default();
    assert!(!s.is_told(hold(1)));
    s.told(hold(1), T0);
    assert!(s.is_told(hold(1)));
    assert!(
        !s.is_told(hold(2)),
        "another episode of the kind is its own"
    );
    s.observe_closed(&[hold(1), hold(2)]);
    assert!(s.is_told(hold(1)), "still open: still told");
    s.observe_closed(&[hold(2)]);
    assert!(!s.is_told(hold(1)), "closed: forgotten");
}

/// A SEED NEVER TELLS TWICE AND NEVER SWALLOWS AN UNTOLD EPISODE: an episode whose last row is
/// typed is told, and its kind is held told for [`SEED_HOLD_MS`] until its condition has been
/// seen clear once; an intent with no outcome, or a skip, is untold. NEGATIVE CONTROLS: the same
/// intent followed by its outcome is told, and a typed row older than the hold leaves a new
/// episode of its kind untold.
#[test]
fn a_seed_holds_what_was_told_and_frees_what_was_only_meant() {
    let now = T0 + 60_000;
    for (rows, told) in [
        (vec![(hold(1), T0, SeedOutcome::Intent)], false),
        (
            vec![
                (hold(1), T0, SeedOutcome::Intent),
                (hold(1), T0, SeedOutcome::Skipped),
            ],
            false,
        ),
        (
            vec![
                (hold(1), T0, SeedOutcome::Intent),
                (hold(1), T0, SeedOutcome::Typed),
            ],
            true,
        ),
    ] {
        let mut s = FactsState::default();
        s.seed(&rows, now);
        assert_eq!(s.is_told(hold(1)), told, "{rows:?}");
        assert_eq!(s.is_told(hold(2)), told, "the kind held with it: {rows:?}");
    }
    // Held until the condition is seen clear once.
    let mut s = FactsState::default();
    s.seed(&[(hold(1), T0, SeedOutcome::Typed)], now);
    s.observe_closed(&[hold(2)]);
    assert!(s.is_told(hold(2)), "the condition never cleared");
    s.observe_closed(&[]);
    assert!(!s.is_told(hold(2)), "seen clear: a new episode is untold");
    // Older than the hold: the episode itself is told, its kind free.
    let mut s = FactsState::default();
    s.seed(&[(hold(1), T0, SeedOutcome::Typed)], T0 + SEED_HOLD_MS);
    assert!(s.is_told(hold(1)));
    assert!(
        !s.is_told(hold(2)),
        "a told row past the hold holds no kind"
    );
}
