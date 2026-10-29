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
    let later = reading(T0, vec![call(40001, Some(8), 90)]);
    for (age, tells) in [(FACT_FRESH_MS, true), (FACT_FRESH_MS + 1, false)] {
        assert_eq!(
            s.pending(Some(&later), T0 + age).is_some(),
            tells,
            "a reading {age} ms old tells only within FACT_FRESH_MS"
        );
    }
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

/// TWO WAKES, THE FIRST TYPED (the slice-2 plan's S3): a successor seeded from the ledger's rows
/// holds wake #1's episode told and no other, so #2, ready, is its break's due line; and a wake
/// its await answered and the loop heard is told too. NEGATIVE CONTROL: `KindWideWakeSeed`, the
/// hold's kind-wide seed, holds #2 told as well.
#[test]
fn a_seed_holds_a_typed_wake_by_its_own_episode() {
    use crate::supervise::Autopilot;
    use crate::supervise::occupancy::OccupancyReading;
    use crate::supervise::occupancy::door;
    use crate::supervise::occupancy::fault::{self, Fault};
    use crate::supervise::occupancy::rules::{
        Books, Guards, HoldSeen, RuleId, Verdict, WAIT_NUDGE_AFTER_MS, WakeSight, decide,
    };
    use crate::supervise::occupancy::wakes_seen::{
        AtomKind, Declarer, Prio, WakeSeen, WakeState, WakesSeen, wake_episode,
    };
    let wake = |ord: u16| WakeSeen {
        ord,
        class: Declarer::Agent,
        prio: Prio::Normal,
        state: WakeState::Ready,
        declared_ms: T0 + i64::from(ord),
        ready_since_ms: Some(T0 + 10),
        kinds: vec![AtomKind::File],
        need: None,
        need_slots: false,
        need_met: true,
        await_only: false,
    };
    let row = |ts: i64, decision: &str, reason: &str| {
        format!(
            r#"{{"ts":{ts},"sid":"s-1","rule_id":"fact-wake@v1","decision":"{decision}","reason":"{reason}"}}"#
        )
    };
    let typed = format!(
        "rule=wake@v1 episode={:016x} held_ms=0",
        wake_episode(T0 + 1, 1)
    );
    let body = [
        row(T0 + 20, "typed", &typed),
        row(
            T0 + 30,
            "waiting",
            &door::wake_heard_reason(3, wake_episode(T0 + 3, 3)),
        ),
    ]
    .join("\n");
    let rows = door::seed_rows_in(&body, Some("@s-1"));
    assert_eq!(
        rows.iter().map(|(_, _, o)| *o).collect::<Vec<_>>(),
        [SeedOutcome::Typed, SeedOutcome::Answered]
    );
    let book = WakesSeen {
        wakes: vec![wake(2)],
    };
    let next = |kind_wide: bool| {
        let mut facts = FactsState::default();
        if kind_wide {
            fault::with(Fault::KindWideWakeSeed, || facts.seed(&rows, T0 + 60_000));
        } else {
            facts.seed(&rows, T0 + 60_000);
        }
        let key = |id| EpisodeKey {
            kind: FactKind::Wake,
            id,
        };
        assert!(facts.is_told(key(wake_episode(T0 + 1, 1))), "#1 typed");
        assert!(facts.is_told(key(wake_episode(T0 + 3, 3))), "#3 heard");
        let told = |id| facts.is_told(key(id));
        let sight = WakeSight {
            seen: &book,
            wall_ms: T0 + 60_000,
            told: &told,
            ranked: true,
        };
        let hold = HoldSeen {
            since_ms: 0,
            declared: true,
        };
        decide(
            &OccupancyReading::default(),
            Some(hold),
            &Books::default(),
            &Guards::clear(Autopilot::Cruise),
            WAIT_NUDGE_AFTER_MS,
            60_000,
            &sight,
        )[RuleId::Wake.index()]
        .1
        .clone()
    };
    assert!(
        matches!(next(false), Verdict::Due(line) if line.fact().starts_with("wake #2 ")),
        "{:?}",
        next(false)
    );
    assert_eq!(next(true), Verdict::Quiet, "a kind-wide seed holds #2 told");
}

/// A wake's line left at a break, this loop's own, at count `submits`.
fn own_left(submits: Option<u64>) -> Left {
    use crate::supervise::occupancy::WakeDoor;
    use crate::supervise::occupancy::door::LeftRow;
    Left {
        row: LeftRow::Wake {
            ord: 1,
            held_ms: 0,
            door: WakeDoor::Break,
        },
        text: "the line".to_string(),
        at_ms: T0,
        submits,
        ran: false,
        seeded: false,
        counted: true,
    }
}

/// What the one line left settles to at a read of a clear composer at `submits`, a turn running
/// where `live`.
fn cleared_at(s: &mut FactsState, submits: Option<u64>, live: bool) -> Vec<Settled> {
    s.look(
        LeftLook::Read {
            draft: Some(false),
            submits,
            live,
        },
        T0 + 1,
    )
    .into_iter()
    .map(|(_, _, how)| how)
    .collect()
}

/// A COUNT NO LOOK MAY WEIGH TELLS THE LINE LEFT, NEVER RELEASES IT (the 2.9b safety review's
/// F2): each host process counts its own submits from 0, so a successor that read the line back
/// from its predecessor's row (a seamless update's re-adopted session counts again from 0: the
/// person sent the line, a turn runs, the composer is clear and the count is 1 against the 7 it
/// was left at), a loop whose host came back from an outage, and a count lower than the line was
/// left at each book a clear composer's line told (`settled=uncounted`), so it is never typed a
/// second time. NEGATIVE CONTROLS: this loop's own count, weighed, releases the cleared line; and
/// with `ReleaseUncounted` planted each of the three is released, the line the person sent typed
/// again.
#[test]
fn a_count_no_look_may_weigh_tells_the_line_left_and_never_releases_it() {
    use crate::supervise::occupancy::door::{LeftSaid, left_reason, left_rows_in};
    use crate::supervise::occupancy::fault::{self, Fault};
    /// How a case builds the account its line is left in.
    type Account<'a> = &'a dyn Fn() -> FactsState;
    let key = EpisodeKey {
        kind: FactKind::Wake,
        id: 0xabc,
    };
    let seeded = || {
        let reason = left_reason(own_left(None).row, key, LeftSaid::Left { submits: Some(7) });
        let line = format!(
            r#"{{"ts":{T0},"rule_id":"fact-wake@v1","decision":"waiting","command":"x","reason":"{reason}","sid":"s1"}}"#
        );
        let lefts = left_rows_in(&line, Some("s1"));
        assert_eq!(lefts.len(), 1, "{line}");
        assert!(
            lefts.iter().all(|(_, l)| l.seeded && !l.counted),
            "{lefts:?}"
        );
        let mut s = FactsState::default();
        s.seed_left(&[(key, T0, SeedOutcome::Left)], lefts);
        assert!(s.is_left(key));
        s
    };
    let recounted = || {
        let mut s = FactsState::default();
        assert!(s.leave(key, own_left(Some(7))));
        s.recount();
        s
    };
    let went_back = || {
        let mut s = FactsState::default();
        assert!(s.leave(key, own_left(Some(7))));
        s
    };
    let runs: [(&str, Account<'_>, Option<u64>, bool); 4] = [
        ("a successor, the count restarted", &seeded, Some(1), true),
        ("a successor, the count going on", &seeded, Some(7), false),
        ("back from an outage", &recounted, Some(7), false),
        ("a count that went back", &went_back, Some(1), false),
    ];
    for (case, account, submits, live) in runs {
        let told = cleared_at(&mut account(), submits, live);
        assert_eq!(told, [Settled::Told(UNCOUNTED)], "{case}");
        let released = fault::with(Fault::ReleaseUncounted, || {
            cleared_at(&mut account(), submits, live)
        });
        assert_eq!(
            released,
            [Settled::Released(CLEARED)],
            "NEGATIVE CONTROL {case}: weighed, released for a retell"
        );
    }
    let mut own = went_back();
    assert_eq!(
        cleared_at(&mut own, Some(7), true),
        [Settled::Released(CLEARED)],
        "NEGATIVE CONTROL: this loop's own count, weighed: cleared unsent"
    );
    let mut standing = seeded();
    let stands = standing.look(
        LeftLook::Read {
            draft: Some(true),
            submits: Some(1),
            live: false,
        },
        T0 + 1,
    );
    assert!(
        stands.is_empty() && standing.is_left(key),
        "nothing settles while it stands"
    );
}

/// A LINE LEFT IS TOLD BY NO DOOR WHILE IT STANDS: the hung call's fact whose line stands left in
/// the composer is spoken for (`FactsState::is_spoken_for`, the one test every door's pick makes)
/// and not the fact to tell (`FactsState::pending`), and once a look settles it, released, it is
/// again; told, it is spoken for for good. NEGATIVE CONTROL: the same reading with nothing left
/// tells it.
#[test]
fn a_fact_left_in_the_composer_is_not_the_fact_to_tell() {
    let hung = call(27090, Some(1), 31);
    let r = reading(T0, vec![hung.clone()]);
    let key = EpisodeKey::of_call(&hung).expect("born");
    let mut s = FactsState::default();
    assert!(!s.is_spoken_for(key));
    assert_eq!(
        s.pending(Some(&r), T0).map(|f| f.key),
        Some(key),
        "NEGATIVE CONTROL: nothing left, told"
    );
    assert!(s.leave(key, own_left(Some(5))));
    assert!(s.is_spoken_for(key), "left: spoken for");
    assert_eq!(s.pending(Some(&r), T0), None, "left: told by no door");
    assert_eq!(
        cleared_at(&mut s, Some(5), false),
        [Settled::Released(CLEARED)]
    );
    assert!(!s.is_spoken_for(key), "released: free again");
    assert_eq!(
        s.pending(Some(&r), T0).map(|f| f.key),
        Some(key),
        "released: again"
    );
    s.told(key, T0 + 2);
    assert!(s.is_spoken_for(key), "told: spoken for");
    assert_eq!(s.pending(Some(&r), T0), None, "told: never again");
}
