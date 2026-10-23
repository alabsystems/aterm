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

/// An episode is told once while open and forgotten when closed.
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
    s.observe_closed(FactKind::Hold, &[hold(1), hold(2)]);
    assert!(s.is_told(hold(1)), "still open: still told");
    s.observe_closed(FactKind::Hold, &[hold(2)]);
    assert!(!s.is_told(hold(1)), "closed: forgotten");
}

/// A seed holds typed episodes for [`SEED_HOLD_MS`] until cleared and leaves intents untold.
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
    let mut s = FactsState::default();
    s.seed(&[(hold(1), T0, SeedOutcome::Typed)], now);
    s.observe_closed(FactKind::Hold, &[hold(2)]);
    assert!(s.is_told(hold(2)), "the condition never cleared");
    s.observe_closed(FactKind::Hold, &[]);
    assert!(!s.is_told(hold(2)), "seen clear: a new episode is untold");
    let mut s = FactsState::default();
    s.seed(&[(hold(1), T0, SeedOutcome::Typed)], T0 + SEED_HOLD_MS);
    assert!(s.is_told(hold(1)));
    assert!(
        !s.is_told(hold(2)),
        "a told row past the hold holds no kind"
    );
}

fn call(pgid: u32, birth: Option<u64>, min: u64) -> BackgroundCall {
    BackgroundCall {
        pid: pgid + 8,
        pgid,
        birth,
        exe: "aterm_gui-e0964a".to_string(),
        task: Some("bplle5dxj".to_string()),
        quiet_ms: min * 60_000 + 59_999,
        wait: Some("mutex".to_string()),
    }
}

fn reading(as_of_ms: i64, calls: Vec<BackgroundCall>) -> FactsReading {
    FactsReading {
        as_of_ms,
        no_progress: calls,
    }
}

/// THE KEY FOLDS THE GROUP AND THE LEADER'S BIRTH: the same pair is the same
/// episode, another birth in the same group or another group born at the same
/// instant is another, and a call whose birth was not read names none.
#[test]
fn an_episode_key_folds_the_group_and_the_leaders_birth() {
    let key = EpisodeKey::no_progress(27090, 7);
    assert_eq!(key.kind, FactKind::NoProgress);
    assert_eq!(EpisodeKey::of_call(&call(27090, Some(7), 31)), Some(key));
    assert_ne!(EpisodeKey::no_progress(27090, 8), key);
    assert_ne!(EpisodeKey::no_progress(27091, 7), key);
    assert_eq!(EpisodeKey::of_call(&call(27090, None, 31)), None);
}

/// ONCE PER EPISODE, FORGOTTEN ONLY ON A FRESH READING WITHOUT IT: the worst
/// untold call is the fact, then the next; told, a call stays told through an
/// absent reading and a stale one; a fresh reading that no longer names it
/// forgets it, so the same call quiet again is told again. A call with no
/// birth is never told, and a no-progress reading leaves a told hold alone.
#[test]
fn a_no_progress_episode_is_told_once_until_a_fresh_reading_no_longer_names_it() {
    let hung = call(27090, Some(1), 31);
    let newer = call(31000, Some(2), 20);
    let r = reading(T0, vec![newer.clone(), hung.clone()]);
    let mut s = FactsState::default();
    s.told(hold(9), T0);
    let first = s.pending(Some(&r), T0).expect("a fact");
    assert_eq!(first.key, EpisodeKey::of_call(&hung).expect("born"));
    assert_eq!(
        (first.text.as_str(), first.as_of_ms),
        (hung.told().as_str(), T0)
    );
    s.told(first.key, T0);
    let second = s.pending(Some(&r), T0).expect("the other");
    assert_eq!(second.key, EpisodeKey::of_call(&newer).expect("born"));
    s.told(second.key, T0);
    assert_eq!(s.pending(Some(&r), T0), None, "each told once");
    let aged = T0 + FACT_FRESH_MS + 1;
    assert_eq!(
        s.pending(Some(&reading(T0, vec![call(40001, Some(8), 90)])), aged),
        None,
        "a reading older than FACT_FRESH_MS tells nothing"
    );
    let stale = T0 + NO_PROGRESS_FRESH_MS + 1;
    for (seen, now) in [(None, T0), (Some(reading(T0, Vec::new())), stale)] {
        s.observe(seen.as_ref(), now);
        assert!(s.is_told(first.key), "{seen:?} at {now} forgets nothing");
    }
    assert_eq!(
        s.pending(Some(&r), stale),
        None,
        "a stale reading tells nothing"
    );
    s.observe(Some(&reading(T0, vec![newer])), T0);
    assert!(!s.is_told(first.key), "the call made progress: over");
    assert!(s.is_told(second.key) && s.is_told(hold(9)), "the rest kept");
    assert_eq!(
        s.pending(Some(&r), T0).map(|f| f.key),
        Some(first.key),
        "quiet again: told again"
    );
    let unborn = reading(T0, vec![call(40000, None, 90)]);
    assert_eq!(FactsState::default().pending(Some(&unborn), T0), None);
}

/// A RESTART TELLS A TOLD CALL NO MORE, AND ANOTHER CALL STILL: a seed holds
/// the no-progress episode its typed row names, and not the kind, so a call
/// born since is told; an intent alone leaves its episode untold.
#[test]
fn a_seed_holds_a_told_call_by_its_own_key_alone() {
    let told = EpisodeKey::no_progress(27090, 1);
    let meant = EpisodeKey::no_progress(28000, 3);
    let mut s = FactsState::default();
    s.seed(
        &[
            (told, T0, SeedOutcome::Intent),
            (told, T0, SeedOutcome::Typed),
            (meant, T0, SeedOutcome::Intent),
        ],
        T0 + 60_000,
    );
    assert!(s.is_told(told));
    assert!(!s.is_told(meant), "an intent alone was never told");
    let born_since = EpisodeKey::no_progress(27090, 2);
    assert!(!s.is_told(born_since), "a call born since");
    let r = reading(T0, vec![call(27090, Some(1), 40), call(27090, Some(2), 5)]);
    assert_eq!(s.pending(Some(&r), T0).map(|f| f.key), Some(born_since));
}
