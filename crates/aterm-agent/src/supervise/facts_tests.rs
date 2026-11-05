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
        held: None,
        left: None,
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

fn held_calls(pids: &[(u32, u64)]) -> Vec<HeldCall> {
    pids.iter()
        .map(|&(pid, held_ms)| HeldCall {
            pid,
            exec_version: Some(1),
            held_ms,
        })
        .collect()
}

/// A reading of the held programs alone, as of `as_of_ms`.
fn held(as_of_ms: i64, pids: &[(u32, u64)]) -> FactsReading {
    FactsReading {
        as_of_ms,
        no_progress: Vec::new(),
        held: Some(HeldReading {
            as_of_ms,
            programs: held_calls(pids),
        }),
        left: None,
    }
}

/// Observes `r` at `now`, then the fact it tells.
fn ask(s: &mut FactsState, r: Option<&FactsReading>, now: i64) -> Option<Fact> {
    s.observe(r, now);
    s.pending(r, now)
}

/// ONE HELD EPISODE IS TOLD ONCE, WHICHEVER PROGRAMS COME AND GO, UNTIL A FRESH READING NAMES
/// NONE: the first fresh reading naming pid 1 opens it and tells it; pid 1 ending while pid 2 is
/// held is the same episode, still told; an absent held part, or one past the forgetting bound,
/// closes nothing; a fresh one naming none closes it, and the next held program is a new episode,
/// told again.
#[test]
fn a_held_episode_is_told_once_until_a_fresh_reading_names_none() {
    let mut s = FactsState::default();
    let first = ask(&mut s, Some(&held(T0, &[(1, 20_000)])), T0).expect("a fact");
    assert_eq!(first.key.kind, FactKind::ProgramCheck);
    assert_eq!(first.as_of_ms, T0);
    s.told(first.key, T0);
    let later = T0 + 60_000;
    assert_eq!(
        ask(&mut s, Some(&held(later, &[(2, 5_000)])), later),
        None,
        "pid 1 ended and pid 2 is held: the same episode, still told"
    );
    let stale = later + NO_PROGRESS_FRESH_MS + 1;
    for (seen, now) in [
        (None, later),
        (
            Some(FactsReading {
                held: None,
                ..held(later, &[])
            }),
            later,
        ),
        (Some(held(later, &[])), stale),
    ] {
        s.observe(seen.as_ref(), now);
        assert!(s.is_told(first.key), "{seen:?} at {now} closes nothing");
    }
    let clear = later + 2_000;
    assert_eq!(ask(&mut s, Some(&held(clear, &[])), clear), None);
    assert!(!s.is_told(first.key), "a fresh reading naming none: over");
    let again = clear + 2_000;
    let next = ask(&mut s, Some(&held(again, &[(3, 1_000)])), again).expect("a new episode");
    assert_ne!(next.key, first.key);
    assert_eq!(next.key.kind, FactKind::ProgramCheck);
}

/// THE HELD SENTENCE SAYS COUNTS, PIDS AND AGES ONLY: one program by its pid and its age; three by
/// theirs, oldest first, and the oldest's age; past four pids, four then `…`; the longest it can
/// be (a ten-digit count, four seven-digit pids then `…`, `9999999 h 59 m`) fits [`FACT_CAP`];
/// and none says nothing.
#[test]
fn the_held_sentence_says_counts_pids_and_ages_only() {
    let words = |pids: &[(u32, u64)]| {
        HeldReading {
            as_of_ms: T0,
            programs: held_calls(pids),
        }
        .told()
    };
    assert_eq!(
        words(&[(72955, 20 * 60_000 + 59_999)]),
        "macOS's program check is holding 1 program this tab started (pid 72955), for 20 m; a \
         retry waits in the same line"
    );
    assert_eq!(
        words(&[(1, 19 * 60_000), (2, 60_000), (3, 0)]),
        "macOS's program check is holding 3 programs this tab started (pid 1, pid 2, pid 3), the \
         oldest for 19 m; a retry waits in the same line"
    );
    let fifty: Vec<(u32, u64)> = (1..=50).map(|pid| (pid, 1_000)).collect();
    assert!(
        words(&fifty).chars().count() <= FACT_CAP,
        "{} chars: {}",
        words(&fifty).chars().count(),
        words(&fifty)
    );
    assert!(
        words(&fifty).contains("(pid 1, pid 2, pid 3, pid 4, …), the oldest for 1 s;"),
        "{}",
        words(&fifty)
    );
    let mut worst = HeldReading {
        as_of_ms: T0,
        programs: held_calls(&[(9_999_999, (9_999_999 * 3600 + 59 * 60) * 1000); 5]),
    }
    .told();
    worst = worst.replacen(" 5 programs ", " 4294967295 programs ", 1);
    assert!(
        worst.contains("pid 9999999, …), the oldest for 9999999 h 59 m;"),
        "{worst}"
    );
    assert!(
        worst.chars().count() <= FACT_CAP,
        "{} chars: {worst}",
        worst.chars().count()
    );
    assert_eq!(words(&[]), "");
}

/// A CALL AT NO PROGRESS IS TOLD BEFORE A HELD PROGRAM: with both untold in one fresh reading, the
/// call first; told, the held programs next.
#[test]
fn a_call_at_no_progress_is_told_before_a_held_program() {
    let r = FactsReading {
        no_progress: vec![call(27090, Some(1), 31)],
        ..held(T0, &[(72955, 600_000)])
    };
    let mut s = FactsState::default();
    let first = ask(&mut s, Some(&r), T0).expect("a fact");
    assert_eq!(first.key.kind, FactKind::NoProgress);
    s.told(first.key, T0);
    let second = ask(&mut s, Some(&r), T0).expect("the held programs");
    assert_eq!(second.key.kind, FactKind::ProgramCheck);
}

/// A HELD PART OLDER THAN THE GATE TELLS NOTHING: by its own time, whatever the no-progress
/// part's, a held part 6 s old tells nothing and one 4 s old tells; the no-progress part's age
/// does not hold it back.
#[test]
fn a_held_part_older_than_the_gate_tells_nothing() {
    for (age, tells) in [
        (FACT_FRESH_MS + 1_000, false),
        (FACT_FRESH_MS - 1_000, true),
    ] {
        let r = FactsReading {
            as_of_ms: T0 - NO_PROGRESS_FRESH_MS,
            no_progress: Vec::new(),
            held: Some(HeldReading {
                as_of_ms: T0 - age,
                programs: held_calls(&[(72955, 600_000)]),
            }),
            left: None,
        };
        let mut s = FactsState::default();
        assert_eq!(
            ask(&mut s, Some(&r), T0).map(|f| (f.key.kind, f.as_of_ms)),
            tells.then_some((FactKind::ProgramCheck, T0 - age)),
            "a held part {age} ms old"
        );
    }
}

/// A RESTART HOLDS A TOLD HELD EPISODE UNTIL IT CLEARS: a successor seeded from a typed
/// `fact-program-check@v1` row ten minutes old opens its own episode (another id) and still holds
/// it told; a fresh reading naming none releases the kind, and the next held program is told.
#[test]
fn a_restart_holds_a_told_held_episode_until_it_clears() {
    let told = EpisodeKey {
        kind: FactKind::ProgramCheck,
        id: (T0 - 600_000).unsigned_abs(),
    };
    let mut s = FactsState::default();
    s.seed(
        &[
            (told, T0 - 600_000, SeedOutcome::Intent),
            (told, T0 - 600_000, SeedOutcome::Typed),
        ],
        T0,
    );
    assert_eq!(
        ask(&mut s, Some(&held(T0, &[(72955, 600_000)])), T0),
        None,
        "the successor's episode is another id, held told"
    );
    let clear = T0 + 2_000;
    assert_eq!(ask(&mut s, Some(&held(clear, &[])), clear), None);
    let again = clear + 2_000;
    assert_eq!(
        ask(&mut s, Some(&held(again, &[(72956, 1_000)])), again).map(|f| f.key.kind),
        Some(FactKind::ProgramCheck),
        "seen clear: the next held program is told"
    );
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
            nudge_wake: true,
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

/// A reading of the agent's left rows alone, as of `as_of_ms`: `count` rows, the oldest 20
/// minutes old.
fn leftover(as_of_ms: i64, count: u32) -> FactsReading {
    FactsReading {
        as_of_ms,
        no_progress: Vec::new(),
        held: None,
        left: Some(LeftoverReading {
            as_of_ms,
            count,
            oldest_ms: Some(20 * 60_000),
        }),
    }
}

/// Asks at each of `readings` (with its time), telling what each gives: the episodes told.
fn tells(s: &mut FactsState, readings: &[(Option<FactsReading>, i64)]) -> Vec<EpisodeKey> {
    let mut told = Vec::new();
    for (r, now) in readings {
        if let Some(fact) = ask(s, r.as_ref(), *now) {
            s.told(fact.key, *now);
            told.push(fact.key);
        }
    }
    told
}

/// A LEFTOVER EPISODE IS TOLD ONCE: the first fresh reading naming a row opens it and tells it;
/// an absent reading, one whose left part was not read, and one naming none past the forgetting
/// bound neither close nor reopen it, so a fresh reading naming two rows after them tells nothing;
/// a fresh one naming none closes it, and the next row is a new episode, told again. NEGATIVE
/// CONTROL: let a stale reading close the episode, and it is told twice.
#[test]
fn a_leftover_episode_is_told_once() {
    let mut s = FactsState::default();
    let stale = T0 + NO_PROGRESS_FRESH_MS + 1;
    let unread = FactsReading {
        left: None,
        ..leftover(T0 + 1_000, 0)
    };
    let first = tells(
        &mut s,
        &[
            (Some(leftover(T0, 1)), T0),
            (None, T0 + 1_000),
            (Some(unread), T0 + 1_000),
            (Some(leftover(T0, 0)), stale),
            (Some(leftover(stale + 1_000, 2)), stale + 1_000),
        ],
    );
    assert_eq!(first.len(), 1, "told once: {first:?}");
    assert_eq!(first[0].kind, FactKind::Leftover);
    let clear = stale + 3_000;
    let again = tells(
        &mut s,
        &[
            (Some(leftover(clear, 0)), clear),
            (Some(leftover(clear + 2_000, 1)), clear + 2_000),
        ],
    );
    assert_eq!(again.len(), 1, "a fresh reading naming none closed it");
    assert_ne!(again[0], first[0], "the next row is a new episode");
}

/// A LEFTOVER IS TOLD ONLY ON A PROMPT TYPED ANYWAY: the turn end's pick gives it, a break's
/// stuck line never does, and still gives the held program beside it. NEGATIVE CONTROL: let the
/// leftover ride a break, and the break's line tells it.
#[test]
fn a_leftover_is_never_a_breaks_line() {
    let mut s = FactsState::default();
    let r = leftover(T0, 1);
    s.observe(Some(&r), T0);
    assert_eq!(
        s.pending(Some(&r), T0).map(|f| f.key.kind),
        Some(FactKind::Leftover)
    );
    assert_eq!(s.pending_at_break(Some(&r), T0), None);
    let both = FactsReading {
        held: held(T0, &[(72955, 600_000)]).held,
        ..r
    };
    s.observe(Some(&both), T0);
    assert_eq!(
        s.pending_at_break(Some(&both), T0).map(|f| f.key.kind),
        Some(FactKind::ProgramCheck)
    );
}

/// THE LEFTOVER SENTENCE NAMES COUNTS AND AGE ONLY: the closed template for one row, for three,
/// and for rows whose age was not read; none says nothing; the longest (a ten-digit count and
/// `9999999 h 59 m`) fits [`FACT_CAP`]. No sentence carries a path or any word outside the
/// template's own. NEGATIVE CONTROL: put an executable word in the sentence, and the vocabulary
/// check fails.
#[test]
fn the_leftover_fact_names_counts_and_age_only() {
    let words = |count, oldest_ms| {
        LeftoverReading {
            as_of_ms: T0,
            count,
            oldest_ms,
        }
        .told()
    };
    let one = words(1, Some(20 * 60_000 + 59_999));
    let three = words(3, Some(13 * 24 * 3_600_000));
    let unaged = words(2, None);
    let worst = words(u32::MAX, Some((9_999_999 * 3600 + 59 * 60) * 1000));
    let template = [
        "process",
        "processes",
        "this",
        "session",
        "started",
        "still",
        "run",
        "runs",
        "for",
        "the",
        "oldest",
        "aterm",
        "ctl",
        "@self",
        "sys",
        "left",
        "lists",
        "it",
        "them",
        "s",
        "m",
        "h",
    ];
    for sentence in [&one, &three, &unaged, &worst] {
        assert!(!sentence.contains('/'), "{sentence}");
        for word in sentence.split(|c: char| c.is_whitespace() || ",;`".contains(c)) {
            assert!(
                word.is_empty()
                    || word.chars().all(|c| c.is_ascii_digit())
                    || template.contains(&word),
                "{word:?} is outside the template: {sentence}"
            );
        }
    }
    assert_eq!(
        one,
        "1 process this session started still runs, for 20 m; `aterm ctl @self sys left` lists it"
    );
    assert_eq!(
        three,
        "3 processes this session started still run, the oldest for 312 h 00 m; `aterm ctl @self \
         sys left` lists them"
    );
    assert_eq!(
        unaged,
        "2 processes this session started still run; `aterm ctl @self sys left` lists them"
    );
    assert_eq!(words(0, Some(1_000)), "");
    assert!(worst.chars().count() <= FACT_CAP, "{worst}");
}

/// [`leftover`] whose left part is as of `left_ms`, in a reading taken at `as_of_ms`.
fn left_at(as_of_ms: i64, left_ms: i64, count: u32) -> FactsReading {
    let mut r = leftover(as_of_ms, count);
    if let Some(left) = r.left.as_mut() {
        left.as_of_ms = left_ms;
    }
    r
}

/// THE LEFTOVER PART IS JUDGED BY ITS OWN TIME, NOT THE READING'S: a fresh reading whose left part
/// is past the tell gate opens the episode and tells nothing; a reading past every gate whose
/// left part is fresh tells it, as of the part's time; once told, a fresh reading whose left part,
/// naming none, is past the forgetting bound closes nothing, and the episode stays told.
/// NEGATIVE CONTROL: judge the part by the reading's time in `pending`, and the first reading's
/// part is told; in `observe`, and the stale part naming none closes the episode.
#[test]
fn a_leftover_part_is_judged_by_its_own_time() {
    let mut s = FactsState::default();
    let past_tell = T0 - FACT_FRESH_MS - 1;
    assert_eq!(
        ask(&mut s, Some(&left_at(T0, past_tell, 1)), T0),
        None,
        "a fresh reading of a left part past the tell gate"
    );
    let stale = T0 - NO_PROGRESS_FRESH_MS - 1;
    let fact = ask(&mut s, Some(&left_at(stale, T0, 1)), T0).expect("a fresh left part");
    assert_eq!(
        (fact.key.kind, fact.key.id, fact.as_of_ms),
        (FactKind::Leftover, past_tell.unsigned_abs(), T0),
        "the episode the first reading opened, told as of the part's time"
    );
    s.told(fact.key, T0);
    let later = T0 + 1_000;
    s.observe(Some(&left_at(later, stale, 0)), later);
    assert!(
        s.is_told(fact.key),
        "a stale left part naming none closes nothing"
    );
    assert_eq!(ask(&mut s, Some(&leftover(later, 1)), later), None);
}

/// A RESTART HOLDS A TOLD LEFTOVER EPISODE UNTIL IT CLEARS: a successor seeded from a typed
/// `fact-leftover@v1` row ten minutes old opens its own episode (another id) and still holds it
/// told; a fresh reading naming none releases the kind, and the next row is told. NEGATIVE
/// CONTROL: seed the leftover kind by its key alone (`seeds_whole` false), and the successor tells
/// the same leftovers again.
#[test]
fn a_restart_holds_a_told_leftover_episode_until_it_clears() {
    let told = EpisodeKey {
        kind: FactKind::Leftover,
        id: (T0 - 600_000).unsigned_abs(),
    };
    let mut s = FactsState::default();
    s.seed(
        &[
            (told, T0 - 600_000, SeedOutcome::Intent),
            (told, T0 - 600_000, SeedOutcome::Typed),
        ],
        T0,
    );
    assert_eq!(
        ask(&mut s, Some(&leftover(T0, 2)), T0),
        None,
        "the successor's episode is another id, held told"
    );
    let clear = T0 + 2_000;
    assert_eq!(ask(&mut s, Some(&leftover(clear, 0)), clear), None);
    let again = clear + 2_000;
    assert_eq!(
        ask(&mut s, Some(&leftover(again, 1)), again).map(|f| f.key.kind),
        Some(FactKind::Leftover),
        "seen clear: the next row is told"
    );
}

use crate::supervise::overlap::OVERLAP_FRESH_MS;

/// Two minted sids, as `aterm_session::SessionId::generate` makes them, and a third.
const PEER_A: &str = "s-0a1b2c3d4e5f60718293";
const PEER_B: &str = "s-ffeeddccbbaa99887766";
const PEER_C: &str = "s-00000000000000000c0c";

/// An overlap reading as of `as_of_ms`: `files` files shared with `peers`.
fn shared(as_of_ms: i64, files: u32, peers: &[&str]) -> OverlapReading {
    OverlapReading::new(as_of_ms, files, peers).expect("minted sids")
}

/// Observes the overlap `r` at `now`, then the overlap it tells.
fn ask_overlap(s: &mut FactsState, r: Option<&OverlapReading>, now: i64) -> Option<Fact> {
    s.observe_overlap(r, now);
    s.pending_overlap(r, now)
}

/// AN OVERLAP EPISODE IS TOLD ONCE, AND A NEW PEER IS A NEW ONE: the first reading naming a peer
/// tells it (`fact-overlap@v1`, its sentence, the reading's time); the same peer with more files is
/// the same episode, not told again; an absent reading, or one older than [`OVERLAP_FRESH_MS`],
/// forgets nothing, and an old one tells nothing. NEGATIVE CONTROLS: a third session joining is a
/// new episode, told; a present reading with no overlap forgets it, and the next overlap with those
/// peers is told again.
#[test]
fn an_overlap_episode_is_told_once_and_a_new_peer_is_a_new_one() {
    assert_eq!(FactKind::Overlap.rule(), "fact-overlap@v1");
    assert_eq!(FactKind::of_word("overlap"), Some(FactKind::Overlap));
    assert_eq!(FactKind::ALL.last(), Some(&FactKind::Overlap));
    let mut s = FactsState::default();
    let first = ask_overlap(&mut s, Some(&shared(T0, 1, &[PEER_A])), T0).expect("told");
    assert_eq!(first.key.kind, FactKind::Overlap);
    assert_eq!(first.as_of_ms, T0);
    assert_eq!(
        first.text,
        format!("1 file this branch changes is also changed by {PEER_A}")
    );
    s.told(first.key, T0);
    let more = T0 + 60_000;
    assert_eq!(
        ask_overlap(&mut s, Some(&shared(more, 4, &[PEER_A])), more),
        None,
        "the same peer with more files: not told again"
    );
    let old = shared(more, 0, &[]);
    for (seen, now) in [(None, more), (Some(&old), more + OVERLAP_FRESH_MS + 1)] {
        s.observe_overlap(seen, now);
        assert!(s.is_told(first.key), "{seen:?} at {now} forgets nothing");
    }
    assert_eq!(
        s.pending_overlap(
            Some(&shared(more, 1, &[PEER_B])),
            more + OVERLAP_FRESH_MS + 1
        ),
        None,
        "an old reading tells nothing"
    );
    let joined = ask_overlap(&mut s, Some(&shared(more, 4, &[PEER_A, PEER_C])), more)
        .expect("CONTROL: a third session joining is told");
    assert_ne!(joined.key, first.key);
    s.told(joined.key, more);
    assert!(!s.is_told(first.key), "the set it was is over");
    let clear = more + 1_000;
    assert_eq!(
        ask_overlap(&mut s, Some(&shared(clear, 0, &[])), clear),
        None
    );
    assert!(
        !s.is_told(joined.key),
        "CONTROL: a present reading with no overlap forgets"
    );
    assert_eq!(
        ask_overlap(&mut s, Some(&shared(clear, 2, &[PEER_C, PEER_A])), clear).map(|f| f.key),
        Some(joined.key),
        "CONTROL: overlapping again, told again"
    );
}

/// AN OVERLAP TELLS FROM RECORDS AS OLD AS A TURN: its reading is as old as the oldest work record
/// it rests on, which its session read at its last turn end, so a reading two minutes old tells,
/// with that time, and one [`OVERLAP_FRESH_MS`] old still does. NEGATIVE CONTROL: a millisecond
/// older, a reading tells nothing and closes nothing.
#[test]
fn an_overlap_tells_from_records_as_old_as_a_turn() {
    let mut s = FactsState::default();
    let first = ask_overlap(&mut s, Some(&shared(T0, 1, &[PEER_A])), T0 + 120_000)
        .expect("two minutes old: told");
    assert_eq!(first.as_of_ms, T0);
    s.told(first.key, T0 + 120_000);
    let past = T0 + OVERLAP_FRESH_MS + 1;
    assert_eq!(
        ask_overlap(&mut s, Some(&shared(T0, 1, &[PEER_B])), past),
        None,
        "CONTROL: past the bound, nothing told"
    );
    assert!(
        s.is_told(first.key),
        "CONTROL: past the bound, nothing closed"
    );
    assert!(
        ask_overlap(
            &mut s,
            Some(&shared(T0, 1, &[PEER_B])),
            T0 + OVERLAP_FRESH_MS
        )
        .is_some(),
        "at the bound: told"
    );
}

/// A SEED HOLDS A TOLD OVERLAP BY ITS OWN KEY ALONE: the overlap's key is its peers' sids, which a
/// successor names again exactly, so a typed row holds that episode told and no other. NEGATIVE
/// CONTROL: an intent with no outcome after it leaves the episode untold.
#[test]
fn a_seed_holds_a_told_overlap_by_its_own_key_alone() {
    let told = shared(T0, 1, &[PEER_A]).episode().expect("an episode");
    let meant = shared(T0, 1, &[PEER_B]).episode().expect("an episode");
    let mut s = FactsState::default();
    s.seed(
        &[
            (told, T0, SeedOutcome::Intent),
            (told, T0, SeedOutcome::Typed),
            (meant, T0, SeedOutcome::Intent),
        ],
        T0 + 60_000,
    );
    assert_eq!(
        s.pending_overlap(Some(&shared(T0, 2, &[PEER_A])), T0),
        None,
        "told before the restart"
    );
    assert_eq!(
        s.pending_overlap(Some(&shared(T0, 1, &[PEER_B])), T0)
            .map(|f| f.key),
        Some(meant),
        "CONTROL: an intent alone was never told"
    );
}
