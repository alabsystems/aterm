// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::harness::acts::{HeldBy, HoldLeg, LandHeld, OpenWord, TaskFace};
use crate::harness::repo_work::RepoWork;
use crate::supervise::policy::exception::{Exception, Urgency, exception_for};
use crate::supervise::policy::outlook::ExceptionKind;
use crate::supervise::policy::turn_end::HoldStart;

const WORK: &str = "Implemented the parser module and its unit tests in this turn.";
const CLEAN: &str = "Nothing left: the parser is committed and pushed to origin/main.";
const NEED: &str = "rotate the deploy key";

fn needs_defense() -> String {
    format!("NEEDS YOU: {NEED}\n{CLEAN}")
}

fn open_defense() -> String {
    format!("{CLEAN}\n{LIST}")
}

/// What the agent lists as left, answering the land check ([`RULE_LAND_CHECK`]).
const LIST: &str = "The docs PR is still open.";

/// THE OPEN DEFENSE'S CHECK, answered at `now` with `answer` over `repo`: the land check is typed
/// at the defense's yield (two minutes before), and the Open leg is the agent's own list.
fn checked_with(
    st: &mut TurnEndState,
    now: Instant,
    answer: &str,
    cut: bool,
    repo: Option<RepoWork>,
) -> TurnEndReading {
    let r = yield_of(&open_defense(), false, None);
    let a = at(st, &r, now);
    assert_eq!(a.rule_id(), Some(RULE_LAND_CHECK), "{a:?}");
    st.acted(&a, &r, now);
    let answered = TurnEndReading {
        worked: Some(Duration::from_secs(10)),
        ..yield_of(answer, cut, repo)
    };
    at(st, &answered, now + MIN);
    answered
}

/// [`checked_with`] the plain list, over a clean repository.
fn listed(st: &mut TurnEndState, now: Instant) -> TurnEndReading {
    checked_with(st, now, LIST, false, None)
}

fn tall_reply(lines: usize) -> String {
    let mut s = format!("Here is the landing report.\nNEEDS YOU: {NEED}\n");
    for i in 1..=lines {
        s.push_str(&format!("- check {i} passed\n"));
    }
    s.push_str(CLEAN);
    s
}

fn found(
    changed: usize,
    untracked: usize,
    stashes: usize,
    unpushed: usize,
    unlanded: usize,
) -> RepoWork {
    RepoWork {
        repo: "/r".to_string(),
        changed,
        untracked,
        stashes,
        unpushed,
        unlanded,
        main_ref: "origin/main".to_string(),
        ..RepoWork::default()
    }
}

const SINCE_MS: i64 = 1_771_000_000_000;

fn begun(since_ms: Option<i64>) -> HoldStart {
    HoldStart {
        since_ms,
        ..HoldStart::default()
    }
}

fn instructed() -> (TurnEndState, Instant) {
    let now = t0();
    let mut st = TurnEndState::default();
    st.face_start();
    assert_eq!(
        act(&mut st, &land(idle(WORK, Some(3 * MIN))), now),
        land_instruction()
    );
    st.ledgered(RULE_LAND, SINCE_MS);
    (st, now)
}

fn yield_of(said: &str, cut: bool, repo: Option<RepoWork>) -> TurnEndReading {
    land(TurnEndReading {
        said_cut: cut,
        repo: crate::harness::repo_work::RepoRead::of_found(repo),
        ..idle(said, Some(2 * MIN))
    })
}

fn owed(st: &mut TurnEndState, r: &TurnEndReading, now: Instant) -> TurnEndAction {
    let later = now + st.short_wait().unwrap_or_default();
    let a = decide_turn_end(st, r, &cfg(), later);
    st.acted(&a, r, later);
    a
}

fn held(legs: &[HoldLeg], open: &[&str], nudges: u32, counts: [u16; 5]) -> LandHeld {
    let mut words = [None; 4];
    for (slot, w) in words.iter_mut().zip(open) {
        *slot = Some(OpenWord::of(w).unwrap_or_else(|| panic!("{w:?} has no index")));
    }
    LandHeld {
        since_ms: Some(SINCE_MS),
        nudges,
        read: true,
        legs: legs.iter().fold(0, |b, l| b | l.bit()),
        open: words,
        changed: counts[0],
        untracked: counts[1],
        stashes: counts[2],
        unpushed: counts[3],
        unlanded: counts[4],
        sub_unpushed: 0,
        parked: 0,
        left: 0,
    }
}

fn first_leg_only(r: &TurnEndReading) -> u8 {
    let tail = r.said_tail.as_deref().unwrap_or_default();
    let leg = if needs::defense_line(tail).is_none() {
        Some(HoldLeg::Defense)
    } else if !needs::needs_you(tail).is_empty() {
        Some(HoldLeg::Needs)
    } else if !open_work_strict(Some(&needs::defense_body(tail))).is_empty() {
        Some(HoldLeg::Open)
    } else if r.said_cut {
        Some(HoldLeg::Cut)
    } else if r
        .turn_said
        .as_deref()
        .is_some_and(|t| !needs::needs_you(t).is_empty())
    {
        Some(HoldLeg::Needs)
    } else if r.repo.found().is_some() {
        Some(HoldLeg::Repo)
    } else {
        None
    };
    leg.map_or(0, HoldLeg::bit)
}

#[test]
fn a_land_yield_that_does_not_land_names_every_leg_that_held_it() {
    use HoldLeg::{Cut, Defense, Needs, Open, Repo};
    let tall = tall_reply(80);
    let listed_need = format!("NEEDS YOU: {NEED}\n{LIST}");
    // Each row: the reply at Land's yield, cut, the repository, the land check's answer where the
    // reply is a defense over a clean repository (its list is the Open leg), and the hold.
    type Row<'a> = (
        &'a str,
        String,
        bool,
        Option<RepoWork>,
        Option<&'a str>,
        LandHeld,
    );
    let rows: Vec<Row<'_>> = vec![
        (
            "L1",
            WORK.to_string(),
            false,
            None,
            None,
            held(&[Defense], &[], 0, [0; 5]),
        ),
        (
            "L2",
            open_defense(),
            false,
            None,
            Some(LIST),
            held(&[Open], &["still open"], 0, [0; 5]),
        ),
        (
            "L3",
            needs_defense(),
            false,
            None,
            Some(&listed_need),
            held(&[Needs, Open], &["still open"], 0, [0; 5]),
        ),
        (
            "L4",
            CLEAN.to_string(),
            false,
            Some(found(0, 1, 0, 0, 0)),
            None,
            held(&[Repo], &[], 0, [0, 1, 0, 0, 0]),
        ),
        (
            "L6",
            tall.clone(),
            true,
            None,
            Some(&tall),
            held(&[Needs, Open, Cut], &[], 0, [0; 5]),
        ),
        (
            "L8",
            CLEAN.to_string(),
            false,
            Some(found(1, 0, 0, 0, 0)),
            None,
            held(&[Repo], &[], 0, [1, 0, 0, 0, 0]),
        ),
        (
            "L9",
            CLEAN.to_string(),
            false,
            Some(found(0, 0, 1, 0, 0)),
            None,
            held(&[Repo], &[], 0, [0, 0, 1, 0, 0]),
        ),
        (
            "L10",
            CLEAN.to_string(),
            false,
            Some(found(0, 0, 0, 1, 1)),
            None,
            held(&[Repo], &[], 0, [0, 0, 0, 1, 1]),
        ),
        (
            "L11",
            CLEAN.to_string(),
            false,
            Some(found(0, 0, 0, 0, 1)),
            None,
            held(&[Repo], &[], 0, [0, 0, 0, 0, 1]),
        ),
        (
            "L12",
            CLEAN.to_string(),
            false,
            Some(RepoWork {
                sub_unpushed: 3,
                subs: vec!["vendor/lib".to_string()],
                ..found(0, 0, 0, 0, 0)
            }),
            None,
            LandHeld {
                sub_unpushed: 3,
                ..held(&[Repo], &[], 0, [0; 5])
            },
        ),
        (
            "L13",
            CLEAN.to_string(),
            false,
            Some(RepoWork {
                parked: 2,
                parked_on: vec!["feat".to_string()],
                ..found(0, 0, 0, 0, 0)
            }),
            None,
            LandHeld {
                parked: 2,
                ..held(&[Repo], &[], 0, [0; 5])
            },
        ),
    ];
    for (case, said, cut, repo, answer, want) in &rows {
        let (mut st, mut now) = instructed();
        assert_eq!(
            st.land_held(),
            Some(LandHeld {
                since_ms: Some(SINCE_MS),
                ..LandHeld::default()
            }),
            "{case}: the instruction typed, its yield not read yet"
        );
        for nudges in 0..2 {
            now += 2 * MIN;
            let mut r = yield_of(said, *cut, repo.clone());
            let a = at(&mut st, &r, now);
            if let Some(answer) = answer {
                assert_eq!(a.rule_id(), Some(RULE_LAND_CHECK), "{case}: {a:?}");
                st.acted(&a, &r, now);
                now += MIN;
                r = TurnEndReading {
                    worked: Some(Duration::from_secs(10)),
                    ..yield_of(answer, *cut, repo.clone())
                };
                at(&mut st, &r, now);
            }
            assert!(!st.landed(), "{case}");
            assert_eq!(st.face(), TaskFace::Open, "{case}");
            assert_eq!(
                st.land_held(),
                Some(LandHeld { nudges, ..*want }),
                "{case}, yield {nudges}"
            );
            if nudges == 0 {
                let words: Vec<&str> = st
                    .land_held()
                    .into_iter()
                    .flat_map(LandHeld::legs_held)
                    .map(HoldLeg::word)
                    .collect();
                assert!(!words.is_empty(), "{case}");
            }
            let a = owed(&mut st, &r, now);
            assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{case}: {a:?}");
            st.ledgered(RULE_LAND_NUDGE, SINCE_MS + 60_000 * (i64::from(nudges) + 1));
            assert_eq!(
                st.land_held().map(|h| (h.since_ms, h.nudges)),
                Some((Some(SINCE_MS), nudges + 1)),
                "{case}: the nudge's row"
            );
        }
        now += 2 * MIN;
        lands_on_check(&mut st, &yield_of(CLEAN, false, None), now);
        assert!(st.landed(), "{case}: L5");
        assert_eq!(st.land_held(), None, "{case}: a landing holds nothing");
        assert_eq!(st.face(), TaskFace::Landed, "{case}");
    }
    let (mut st, now) = instructed();
    let r = yield_of(&open_defense(), false, None);
    at(&mut st, &r, now + 2 * MIN);
    assert_eq!(
        st.land_held().map(|h| h.legs),
        Some(0),
        "the reader's open words hold nothing: the check decides"
    );
    let (mut st, now) = instructed();
    listed(&mut st, now + 2 * MIN);
    let words: Vec<&str> = st
        .land_held()
        .into_iter()
        .flat_map(LandHeld::open_words)
        .collect();
    assert_eq!(words, ["still open"], "the list's words");
    let (mut st, now) = instructed();
    let both = checked_with(
        &mut st,
        now + 2 * MIN,
        LIST,
        false,
        Some(found(0, 1, 0, 0, 0)),
    );
    let legs = st.land_held().map(|h| h.legs);
    assert_eq!(legs, Some(Open.bit() | Repo.bit()));
    assert_ne!(legs, Some(first_leg_only(&both)));
}

#[test]
fn the_hold_ends_at_a_landing_another_mode_and_off() {
    let open = || {
        let (mut st, now) = instructed();
        let answered = listed(&mut st, now + 2 * MIN);
        assert!(st.land_held().is_some_and(|h| h.read), "held");
        (st, now + 3 * MIN, answered)
    };
    let (mut st, now, answered) = open();
    owed(&mut st, &answered, now);
    lands_on_check(&mut st, &yield_of(CLEAN, false, None), now + 20 * MIN);
    assert!(st.landed());
    assert_eq!(st.land_held(), None, "landed");
    // A person's own exchange after a done claim is theirs: the defense, its check owed.
    let (mut st, now) = instructed();
    at(
        &mut st,
        &yield_of(&open_defense(), false, None),
        now + 2 * MIN,
    );
    assert!(st.land_held().is_some_and(|h| h.read), "held");
    let thanks = land(TurnEndReading {
        person: Some(Duration::from_secs(8)),
        key: Some(Duration::from_secs(8)),
        ..idle("You're welcome!", Some(Duration::from_secs(5)))
    });
    at(&mut st, &thanks, now + 5 * MIN);
    // A person's message mid-landing is no exchange of theirs: Land carries the task on (owner
    // ruling 2026-10-27).
    assert_eq!(st.face(), TaskFace::Open);
    assert!(!st.left_to_person());
    assert!(
        st.land_held().is_some(),
        "a person's message mid-landing: the hold stands"
    );
    for mode in [Autopilot::Cruise, Autopilot::Off] {
        let (mut st, now, _) = open();
        at(
            &mut st,
            &under(mode, idle(WORK, Some(3 * MIN))),
            now + 5 * MIN,
        );
        assert_eq!(st.land_held(), None, "{mode:?}");
    }
    let (mut st, _, _) = open();
    st.park_off();
    assert_eq!(st.land_held(), None, "parked");
    let (mut st, now, answered) = open();
    let a = owed(&mut st, &answered, now);
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    let read = st.land_held().expect("held");
    assert!(read.read && read.nudges == 1, "{read:?}");
    st.took();
    assert!(!st.other_turn());
    assert_eq!(st.land_held(), Some(read), "its own act's turn");
    at(
        &mut st,
        &yield_of(&open_defense(), false, None),
        now + 20 * MIN,
    );
    st.took();
    assert!(st.other_turn());
    let dropped = LandHeld {
        since_ms: Some(SINCE_MS),
        nudges: 1,
        ..LandHeld::default()
    };
    assert_eq!(st.land_held(), Some(dropped), "someone else's turn");
    let job = land(idle(WORK, Some(20 * MIN)));
    at(&mut st, &job, now + 45 * MIN);
    assert_eq!(st.face(), TaskFace::Open);
    assert_eq!(st.land_held(), Some(dropped), "until Land's next yield");
}

#[test]
fn a_worker_holds_none_before_its_instruction() {
    let mut st = TurnEndState::default();
    st.face_start();
    let first = land(idle(WORK, Some(3 * MIN)));
    st.took();
    assert_eq!(st.land_held(), None, "its first working turn");
    st.observe(&first, t0());
    assert_eq!(st.land_held(), None, "its first point, read under Land");
    assert_eq!(st.land_episode(), (false, 0));
    st.ledgered(RULE_LAND_NUDGE, SINCE_MS);
    assert_eq!(st.land_held(), None);
    let a = decide_turn_end(&st, &first, &cfg(), t0());
    assert_eq!(a, land_instruction());
    st.acted(&a, &first, t0());
    assert_eq!(
        st.land_held(),
        Some(LandHeld::default()),
        "typed, not yet ledgered"
    );
    st.ledgered(RULE_CONTINUE, SINCE_MS - 1);
    assert_eq!(st.land_held(), Some(LandHeld::default()), "another row");
    st.ledgered(RULE_LAND, SINCE_MS);
    assert_eq!(
        st.land_held(),
        Some(LandHeld {
            since_ms: Some(SINCE_MS),
            ..LandHeld::default()
        })
    );
}

#[test]
fn a_need_said_earlier_in_the_turn_is_a_leg() {
    let (mut st, now) = instructed();
    let r = land(TurnEndReading {
        turn_said: Some(format!("NEEDS YOU: {NEED}")),
        ..idle(CLEAN, Some(2 * MIN))
    });
    at(&mut st, &r, now + 2 * MIN);
    assert!(!st.landed());
    let hold = st.land_held().expect("held");
    assert_eq!(hold.legs, HoldLeg::Needs.bit(), "{hold:?}");
    assert_eq!(first_leg_only(&idle(CLEAN, None)), 0);
    let (mut st, now) = instructed();
    at(
        &mut st,
        &yield_of(&format!("NEEDS YOU: {NEED}\n{WORK}"), false, None),
        now + 2 * MIN,
    );
    let first = st.land_held().expect("held");
    assert_eq!(first.legs, HoldLeg::Needs.bit() | HoldLeg::Defense.bit());
    let a = owed(&mut st, &yield_of(WORK, false, None), now + 2 * MIN);
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    at(&mut st, &yield_of(WORK, false, None), now + 20 * MIN);
    assert_eq!(st.standing_needs(), [NEED.to_string()], "it still stands");
    let second = st.land_held().expect("held");
    assert_eq!(second.legs, HoldLeg::Defense.bit(), "{second:?}");
    assert!(!st.standing_needs().is_empty() && !second.holds(HoldLeg::Needs));
}

#[test]
fn the_hold_moves_once_per_change_and_is_asked_once() {
    let now = t0();
    let mut st = TurnEndState::default();
    st.face_start();
    assert_eq!(st.held_moved(true), Some(None), "the first ask");
    assert_eq!(st.held_moved(true), None, "asked again");
    let r = land(idle(WORK, Some(3 * MIN)));
    act(&mut st, &r, now);
    assert_eq!(st.held_moved(true), Some(Some(LandHeld::default())));
    st.ledgered(RULE_LAND, SINCE_MS);
    let since = st.held_moved(true).flatten().expect("since");
    assert_eq!(since.since_ms, Some(SINCE_MS));
    assert_eq!(st.held_moved(true), None);
    let y = listed(&mut st, now + 2 * MIN);
    assert!(st.held_moved(true).flatten().is_some_and(|h| h.read));
    at(&mut st, &land(idle(LIST, None)), now + 3 * MIN + MIN / 2);
    assert_eq!(st.held_moved(true), None, "the same point read again");
    let a = owed(&mut st, &y, now + 4 * MIN);
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    assert!(st.held_moved(true).flatten().is_some_and(|h| h.nudges == 1));
    st.took();
    assert_eq!(st.held_moved(true), None, "its own turn moves nothing");
    lands_on_check(&mut st, &yield_of(CLEAN, false, None), now + 30 * MIN);
    assert_eq!(st.held_moved(true), Some(None), "landed");
}

#[test]
fn a_hold_not_here_is_told_none_and_the_loops_end_once() {
    let now = t0();
    let mut st = TurnEndState::default();
    st.face_start();
    assert_eq!(st.held_moved(true), Some(None), "the first ask");
    act(&mut st, &land(idle(WORK, Some(3 * MIN))), now);
    st.ledgered(RULE_LAND, SINCE_MS);
    at(
        &mut st,
        &yield_of(&open_defense(), false, None),
        now + 2 * MIN,
    );
    let held = st.held_moved(true).flatten().expect("held, read");
    assert!(held.read, "{held:?}");
    assert_eq!(st.held_moved(false), Some(None), "not here: told none");
    assert_eq!(st.land_held(), Some(held), "the episode kept");
    assert_eq!(st.held_moved(false), None, "none again is no move");
    assert_eq!(st.held_moved(true), Some(Some(held)), "here again");
    assert!(st.held_ended(), "the loop's end tells the hold gone");
    assert!(!st.held_ended(), "once");
    assert_eq!(st.held_moved(false), None, "told none already");
}

#[test]
fn a_seeded_episode_holds_its_since_and_nudges_until_its_next_yield() {
    let mut st = TurnEndState::default();
    st.seed_land_hold(begun(Some(SINCE_MS)));
    assert_eq!(st.land_held(), None, "no episode seeded");
    st.seed_land(3, true, false, false);
    st.seed_land_hold(begun(Some(SINCE_MS)));
    let seeded = LandHeld {
        since_ms: Some(SINCE_MS),
        nudges: 3,
        ..LandHeld::default()
    };
    assert_eq!(st.face_start(), TaskFace::Open);
    assert_eq!(st.land_held(), Some(seeded));
    at(&mut st, &yield_of(WORK, false, None), t0());
    assert_eq!(
        st.land_held(),
        Some(LandHeld {
            nudges: 3,
            ..held(&[HoldLeg::Defense], &[], 3, [0; 5])
        })
    );
    let mut older = TurnEndState::default();
    older.seed_land(1, true, false, false);
    older.seed_land_hold(begun(None));
    assert!(
        older
            .land_held()
            .is_some_and(|h| h.since_ms.is_none() && h.nudges == 1)
    );
}

#[test]
fn a_land_hold_is_copy_and_small() {
    fn copy<T: Copy>(t: T) -> (T, T) {
        (t, t)
    }
    let (a, _) = copy(held(&[HoldLeg::Open], &["still open"], 2, [1, 2, 3, 4, 5]));
    assert_eq!(std::mem::size_of::<Option<OpenWord>>(), 2);
    assert!(
        std::mem::size_of::<LandHeld>() <= 48,
        "{} bytes",
        std::mem::size_of::<LandHeld>()
    );
    let debug = format!("{a:?}");
    assert!(
        debug.contains("\"still open\"") && !debug.contains("Instant"),
        "{debug}"
    );
}

/// Every word of the tables has its index, and no other word does. That every word a strict reading
/// names is among them is `every_phrase_keeps_its_contract_on_every_corpus_row`'s.
#[test]
fn every_word_of_the_tables_has_an_index() {
    for w in open_words() {
        let i = open_word_index(w).expect("indexed");
        assert_eq!(open_word_at(i), Some(w));
    }
    assert_eq!(OpenWord::of("a word no table holds"), None);
}

#[test]
fn work_after_a_landing_is_held_from_lands_next_act() {
    let (mut st, now) = instructed();
    let answered = listed(&mut st, now + 2 * MIN);
    let a = owed(&mut st, &answered, now + 3 * MIN);
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    st.ledgered(RULE_LAND_NUDGE, SINCE_MS + 4 * 60_000);
    lands_on_check(&mut st, &yield_of(CLEAN, false, None), now + 20 * MIN);
    assert!(st.landed());
    assert_eq!(st.land_held(), None, "landed");
    st.took();
    assert!(st.other_turn());
    assert_eq!(st.face(), TaskFace::Open);
    assert_eq!(st.land_held(), None, "their turn over a finished landing");
    let later = now + 180 * MIN;
    let job = land(idle(WORK, Some(20 * MIN)));
    at(&mut st, &job, later);
    assert_eq!(st.face(), TaskFace::Open);
    assert_eq!(st.land_held(), None, "their job's end");
    assert_eq!(st.held_moved(true), Some(None), "no hold to tell");
    let a = owed(&mut st, &job, later);
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    assert_eq!(st.land_episode(), (true, 2), "the engine's own count");
    assert_eq!(
        st.land_held(),
        Some(LandHeld::default()),
        "typed, not yet ledgered"
    );
    let since = SINCE_MS + 180 * 60_000;
    st.ledgered(RULE_LAND_NUDGE, since);
    let begun_here = LandHeld {
        since_ms: Some(since),
        ..LandHeld::default()
    };
    assert_eq!(st.land_held(), Some(begun_here));
    at(&mut st, &yield_of(WORK, false, None), later + 2 * MIN);
    assert_eq!(
        st.land_held(),
        Some(LandHeld {
            since_ms: Some(since),
            ..held(&[HoldLeg::Defense], &[], 0, [0; 5])
        })
    );
    let a = owed(&mut st, &yield_of(WORK, false, None), later + 2 * MIN);
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    st.ledgered(RULE_LAND_NUDGE, since + 10 * 60_000);
    assert_eq!(
        st.land_held().map(|h| (h.since_ms, h.nudges)),
        Some((Some(since), 1))
    );
    let mut next = TurnEndState::default();
    next.seed_land(3, true, false, false);
    next.seed_land_hold(HoldStart {
        since_ms: Some(since),
        base: 2,
        relanded: false,
    });
    next.face_start();
    assert_eq!(
        next.land_held().map(|h| (h.since_ms, h.nudges)),
        Some((Some(since), 1))
    );
    let mut landed = TurnEndState::default();
    landed.seed_land(1, false, false, true);
    landed.seed_land_hold(HoldStart::LANDED);
    landed.face_start();
    assert_eq!(landed.land_held(), None, "a seeded landing's hold");
    let mut recorded = TurnEndState::default();
    recorded.seed_land(1, false, false, false);
    recorded.seed_land_hold(begun(Some(SINCE_MS)));
    recorded.face_start();
    assert!(recorded.land_held().is_some(), "the seeded hold");
    assert!(recorded.seed_task_done());
    recorded.took();
    assert_eq!(recorded.face(), TaskFace::Open);
    assert_eq!(recorded.land_held(), None, "a recorded landing's hold");
}

#[test]
fn a_land_acts_yield_names_legs_only_where_the_act_was_taken() {
    let seeded = || {
        let mut st = TurnEndState::default();
        st.seed_land(2, true, false, false);
        st.seed_land_hold(begun(Some(SINCE_MS)));
        assert_eq!(st.face_start(), TaskFace::Open);
        st
    };
    let first = land(idle(WORK, None));
    let mut st = seeded();
    at(&mut st, &first, t0());
    assert_eq!(
        st.land_held(),
        Some(held(&[HoldLeg::Defense], &[], 2, [0; 5])),
        "the first point over a seeded land act"
    );
    let mut st = seeded();
    st.seed_land_act(0x5eed, 0x1ead, 2 * MIN, t0());
    assert!(st.land_act_seeded());
    at(&mut st, &first, t0());
    assert!(!st.landed());
    assert_eq!(
        st.land_held(),
        Some(LandHeld {
            since_ms: Some(SINCE_MS),
            nudges: 2,
            ..LandHeld::default()
        }),
        "a reply that predates its act"
    );
    let (mut st, now) = instructed();
    let y = yield_of(WORK, false, None);
    at(&mut st, &y, now + 2 * MIN);
    let last = st.land_held().expect("held");
    let a = owed(&mut st, &y, now + 2 * MIN);
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    let untaken = land(TurnEndReading {
        repo: crate::harness::repo_work::RepoRead::Found(found(0, 1, 0, 0, 0)),
        ..idle(WORK, None)
    });
    at(&mut st, &untaken, now + 6 * MIN);
    assert!(st.act_in_flight(), "within the act's window: no point yet");
    let past = now + 6 * MIN + st.timing.take_within + Duration::from_secs(1);
    at(&mut st, &untaken, past);
    assert!(!st.act_in_flight(), "the act's point, judged not taken");
    assert_eq!(
        st.land_held(),
        Some(LandHeld { nudges: 1, ..last }),
        "an act never taken said nothing"
    );
    let (mut st, now) = instructed();
    at(
        &mut st,
        &yield_of(CLEAN, false, Some(found(70_000, 0, 0, 0, 0))),
        now + 2 * MIN,
    );
    assert_eq!(st.land_held().map(|h| h.changed), Some(u16::MAX));
}

#[test]
fn someone_elses_turn_seen_only_at_its_end_drops_the_legs() {
    let (mut st, now) = instructed();
    at(&mut st, &yield_of(WORK, false, None), now + 2 * MIN);
    let read = st.land_held().expect("held");
    assert!(read.read && read.holds(HoldLeg::Defense), "{read:?}");
    let job = land(idle(WORK, Some(20 * MIN)));
    let a = at(&mut st, &job, now + 25 * MIN);
    assert!(!st.other_turn(), "no live read");
    assert_eq!(st.face(), TaskFace::Open);
    let dropped = LandHeld {
        since_ms: Some(SINCE_MS),
        ..LandHeld::default()
    };
    assert_eq!(st.land_held(), Some(dropped), "someone else's turn");
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    st.acted(&a, &job, now + 25 * MIN);
    at(&mut st, &yield_of(WORK, false, None), now + 30 * MIN);
    let again = st.land_held().expect("held");
    assert!(
        again.read && again.holds(HoldLeg::Defense) && again.nudges == 1,
        "{again:?}"
    );
}

/// A HOLD'S FIRST ACT under `mode`: the session at its first point, its work done, the act typed and ledgered.
fn begun_under(mode: Autopilot, managed: bool) -> (TurnEndState, Instant) {
    let now = t0();
    let mut st = TurnEndState::default();
    st.set_managed(managed);
    st.face_start();
    let a = act(&mut st, &under(mode, idle(WORK, Some(3 * MIN))), now);
    let rule = a
        .rule_id()
        .unwrap_or_else(|| panic!("{mode:?}: an act: {a:?}"));
    st.ledgered(rule, SINCE_MS);
    (st, now)
}

/// Each act answered quickly with no work by `answer(k)` under `mode`, until the point holds: the answers it took, the exception it stands under and the hold's arm.
fn held_after(
    mode: Autopilot,
    managed: bool,
    repo: Option<RepoWork>,
    answer: impl Fn(usize) -> String,
) -> (usize, Option<Exception>, Option<HeldBy>) {
    const QUICK: Duration = Duration::from_secs(30);
    let (mut st, mut now) = begun_under(mode, managed);
    for k in 0..4 * NO_WORK_ACTS {
        now += 2 * MIN;
        st.took();
        let mut r = under(
            mode,
            TurnEndReading {
                repo: crate::harness::repo_work::RepoRead::of_found(repo.clone()),
                ..idle(&answer(k), Some(QUICK))
            },
        );
        loop {
            match at(&mut st, &r, now) {
                TurnEndAction::Nothing => {
                    return (k + 1, exception_for(&st, &r, &cfg(), now), st.held_by());
                }
                // The back-off waited out, the same screen read again at its end.
                TurnEndAction::WaitUntil { until, .. } => {
                    now = until;
                    r.worked = None;
                }
                a => {
                    if mode == Autopilot::Cruise && st.held_by().is_some() {
                        assert_eq!(a.rule_id(), Some(RULE_CONTINUE), "{a:?}");
                        return (k + 1, exception_for(&st, &r, &cfg(), now), st.held_by());
                    }
                    let rule = a.rule_id().unwrap_or_else(|| panic!("{mode:?}: {a:?}"));
                    st.acted(&a, &r, now);
                    st.ledgered(rule, SINCE_MS + 60_000 * i64::try_from(k + 1).unwrap_or(0));
                    break;
                }
            }
        }
    }
    panic!("{mode:?}: never held");
}

/// One case of a recorded hold: its name, mode, answer, repository finding, whether a manager
/// waits on it, and the exception its hold raises.
type HoldCase = (
    &'static str,
    Autopilot,
    String,
    Option<RepoWork>,
    bool,
    Exception,
);

#[test]
fn land_holds_are_recorded_without_an_alert() {
    use crate::harness::acts::HeldBy::SameReply;
    let repo = found(0, 1, 0, 0, 0);
    let rows: [HoldCase; 6] = [
        (
            "land, a Repo leg",
            Autopilot::Land,
            CLEAN.to_string(),
            Some(repo.clone()),
            false,
            Exception::repeats(SameReply),
        ),
        (
            "land, an Open leg and a Repo leg",
            Autopilot::Land,
            open_defense(),
            Some(repo.clone()),
            false,
            Exception::repeats(SameReply),
        ),
        (
            "land, an Open leg alone",
            Autopilot::Land,
            open_defense(),
            None,
            false,
            Exception::repeats(SameReply),
        ),
        (
            "land, a Needs leg alone",
            Autopilot::Land,
            needs_defense(),
            None,
            false,
            Exception::repeats(SameReply),
        ),
        (
            "land, a Repo leg, a managed worker",
            Autopilot::Land,
            CLEAN.to_string(),
            Some(repo.clone()),
            true,
            Exception::repeats(SameReply),
        ),
        (
            "cruise, repository work",
            Autopilot::Cruise,
            CLEAN.to_string(),
            Some(repo.clone()),
            false,
            // Cruise's stop names the person's switch to Land, which asks (v6 §14.17 item 3).
            Exception::repeats_by(SameReply, true),
        ),
    ];
    for (case, mode, said, repo, managed, want) in rows {
        let (answers, told, held) = held_after(mode, managed, repo, |_| said.clone());
        assert_eq!(answers, 2, "{case}: held at the second same answer");
        assert_eq!(held, Some(SameReply), "{case}");
        assert_eq!(
            told.as_ref(),
            (mode != Autopilot::Cruise).then_some(&want),
            "{case}"
        );
    }
    let recorded = Exception::repeats(SameReply);
    assert_eq!(
        (recorded.kind, recorded.when, recorded.held),
        (ExceptionKind::Stalled, Urgency::Later, Some(SameReply))
    );
}

/// A LAND HOLD AT A SPENT API WALL IS THE WALL'S (limit 3 of §14.0): Land's nudge read the Repo
/// leg, then a cut reply (an API wall that is no outage) answered each of aterm's tries the same
/// until they were spent, which holds at the wall's reply (`Kept::holds`). That hold is told Later
/// at the wall, and stays Later once the wall's notice leaves the screen: no band says the agent
/// gave the same reply twice, and the repository's work is not what stopped it. (The same Land run
/// with no wall is recorded quietly too.)
#[test]
fn a_spent_api_walls_hold_under_land_with_repository_work_stays_a_fact_for_later() {
    let repo = found(0, 1, 0, 0, 0);
    let on_repo = |r: TurnEndReading| {
        land(TurnEndReading {
            repo: crate::harness::repo_work::RepoRead::of_found(Some(repo.clone())),
            ..r
        })
    };
    let (mut st, mut now) = begun_under(Autopilot::Land, false);
    now += 2 * MIN;
    st.took();
    let mut r = on_repo(idle(CLEAN, Some(Duration::from_secs(30))));
    let nudged = loop {
        match act(&mut st, &r, now) {
            TurnEndAction::WaitUntil { until, .. } => {
                now = until;
                r.worked = None;
            }
            a => break a,
        }
    };
    assert_eq!(
        nudged.rule_id(),
        Some(RULE_LAND_NUDGE),
        "Land's nudge at the first answer: {nudged:?}"
    );
    st.ledgered(RULE_LAND_NUDGE, SINCE_MS + 60_000);
    assert!(
        st.land_held()
            .is_some_and(|hold| hold.read && hold.holds(HoldLeg::Repo)),
        "the nudge read the Repo leg: {:?}",
        st.land_held()
    );
    let kind = WallKind::ApiError {
        code: None,
        retryable: true,
        cause: aterm_phase::ApiCause::CutOff,
    };
    let mut w = on_repo(walled(
        kind,
        "API Error: Your computer went to sleep mid-response. The response above may be \
         incomplete.",
        Some(Duration::from_secs(4)),
    ));
    let mut retries = Vec::new();
    loop {
        assert!(retries.len() <= 12, "the wall's tries end: {retries:?}");
        now += Duration::from_secs(5);
        match act(&mut st, &w, now) {
            TurnEndAction::WaitUntil { until, .. } => {
                now = until;
                w.worked = None;
            }
            TurnEndAction::Nothing => break,
            a => {
                retries.push(a.rule_id().unwrap_or("?"));
                w.worked = Some(Duration::from_secs(4));
            }
        }
    }
    assert!(
        !retries.is_empty() && retries.iter().all(|rule| *rule == "api-cutoff@v1"),
        "aterm retried the wall until its tries were spent: {retries:?}"
    );
    assert_eq!(st.held_by(), Some(HeldBy::SameReply));
    w.worked = None;
    now += MIN;
    st.observe(&w, now);
    assert_eq!(
        exception_for(&st, &w, &cfg(), now),
        Some(Exception::repeats(HeldBy::SameReply)),
        "at the wall"
    );
    let left = TurnEndReading {
        wall: None,
        wall_message: String::new(),
        ..w.clone()
    };
    now += MIN;
    st.observe(&left, now);
    assert_eq!(
        exception_for(&st, &left, &cfg(), now),
        Some(Exception::repeats(HeldBy::SameReply)),
        "once the wall's notice left the screen, the wall's hold is still a fact for later"
    );
}

#[test]
fn six_no_work_answers_each_worded_its_own_way_hold_as_no_work() {
    use crate::harness::acts::HeldBy::NoWork;
    let worded = |k: usize| {
        let case = char::from(b'a' + u8::try_from(k % 26).unwrap_or(0));
        format!("Nothing left: the parser is committed and pushed, case {case}.")
    };
    let repo = found(0, 1, 0, 0, 0);
    for (mode, repo, want) in [
        (Autopilot::Cruise, None, Exception::repeats_by(NoWork, true)),
        (
            Autopilot::Land,
            Some(repo.clone()),
            Exception::repeats(NoWork),
        ),
    ] {
        let (answers, told, held) = held_after(mode, false, repo, worded);
        assert_eq!(answers, NO_WORK_ACTS, "{mode:?}");
        assert_eq!(held, Some(NoWork), "{mode:?}");
        assert_eq!(
            told,
            (mode != Autopilot::Cruise).then_some(want),
            "{mode:?}"
        );
    }
    let quiet = Exception::repeats;
    assert_ne!(quiet(NoWork).why, quiet(HeldBy::SameReply).why);
    assert!(quiet(NoWork).why.contains(&NoWork.answered()));
    assert_ne!(quiet(NoWork).clears, quiet(HeldBy::SameReply).clears);
}

/// A done claim that is no `Nothing left:` defense: Land reads it unlanded.
const ALL_DONE: &str = "Merged the parser and pushed it to origin/main. All done.";

/// The replies' hold under `mode` from [`begun_under`]: each act answered quickly by `said`, the
/// back-off waited out, until a point types nothing.
fn held_on(mode: Autopilot, said: &str) -> (TurnEndState, Instant) {
    let (mut st, mut now) = begun_under(mode, false);
    while st.held_by().is_none() {
        assert!(now < t0() + 60 * MIN, "held under {mode:?} within the hour");
        now += 2 * MIN;
        st.took();
        let mut r = under(mode, idle(said, Some(Duration::from_secs(30))));
        loop {
            st.observe(&r, now);
            if st.held_by().is_some() {
                break;
            }
            let action = decide_turn_end(&st, &r, &cfg(), now);
            if let TurnEndAction::WaitUntil { until, .. } = action {
                now = until;
                r.worked = None;
            } else {
                st.acted(&action, &r, now);
                break;
            }
        }
    }
    (st, now)
}

/// THE STOP A HOST IS TOLD IS THE ONE THE DECISION APPLIES, NOT THE BOOKS BEHIND IT. Under Land a
/// held reply types nothing, and both say so. An outage's wall is waited out and its reply is no
/// repeat, and a repeated reply under Cruise gets another turn: the books still hold at each, and
/// the stop does not. MUTANT: `replies_stop` read off `held_by` fails both controls.
#[test]
fn the_stop_a_host_is_told_is_the_one_the_decision_applies() {
    let (st, now) = held_on(Autopilot::Cruise, ALL_DONE);
    let at = under(Autopilot::Cruise, idle(ALL_DONE, None));
    assert_eq!(st.held_by(), Some(HeldBy::SameReply));
    assert!(st.repeats(&at), "the same reply");
    assert!(
        !st.replies_stop(&at, now),
        "a repeated reply under Cruise gets another turn"
    );
    let (st, now) = held_on(Autopilot::Land, ALL_DONE);
    let at = under(Autopilot::Land, idle(ALL_DONE, None));
    assert_eq!(st.held_by(), Some(HeldBy::SameReply));
    assert!(st.replies_stop(&at, now), "the same reply: the stop holds");
    let outage = TurnEndReading {
        wall: Some(WallKind::ApiError {
            code: Some(529),
            retryable: true,
            cause: aterm_phase::ApiCause::Server,
        }),
        wall_message: "API Error: 529 Overloaded".to_string(),
        ..at
    };
    assert_eq!(st.held_by(), Some(HeldBy::SameReply), "the books");
    assert!(
        !st.replies_stop(&outage, now),
        "an outage's wall is waited out"
    );
}

/// The person's switch to Land over a held tab counts as their turn (v6 §14.17 item 3): Land types
/// `land@v1` once, opened by the one sentence that quotes where the agent stopped, and the same
/// answer holds again with no second prompt, and no second ask at a later switch over that same
/// reply (the stop under Cruise then names no switch). A switch to Cruise types nothing, and Off,
/// then Land, types Land's own act (§14.10).
#[test]
fn a_switch_to_land_over_a_standing_hold_asks_once_and_the_same_answer_holds_again() {
    let (mut st, mut now) = held_on(Autopilot::Cruise, ALL_DONE);
    let r = |mode, worked| by_person(under(mode, idle(ALL_DONE, worked)));
    assert!(
        matches!(
            at(&mut st, &r(Autopilot::Cruise, None), now + MIN),
            TurnEndAction::WaitUntil { .. }
                | TurnEndAction::Type {
                    rule_id: RULE_CONTINUE,
                    ..
                }
        ),
        "Cruise continues without waiting for a person"
    );
    assert_eq!(st.switch_ask(), SwitchAsk::None);
    let stop = exception_for(&st, &r(Autopilot::Cruise, None), &cfg(), now + MIN);
    assert_eq!(stop, None, "Cruise schedules recovery without an exception");
    assert!(stop.is_none());
    now += 2 * MIN;
    let asked = act(&mut st, &r(Autopilot::Land, None), now);
    let TurnEndAction::Type { text, rule_id } = &asked else {
        panic!("the switch types Land's act: {asked:?}");
    };
    assert_eq!(*rule_id, RULE_LAND, "{asked:?}");
    let why = format!("{} {}", harness_mark!(), switch_why(&held_quote(ALL_DONE)));
    assert!(text.starts_with(&why), "opened by the why: {text}");
    assert_eq!(
        past_switch_why(text),
        LAND,
        "Land's own words follow the why"
    );
    assert!(
        is_switch_ask(text) && is_land_words(text) && crate::harness::upgrade::is_land_act(text),
        "every reader keyed on Land's words reads it: {text}"
    );
    assert!(
        units(text) <= VERBATIM_UNITS,
        "{} units: {text}",
        units(text)
    );
    assert_eq!(st.switch_ask(), SwitchAsk::Typed);
    assert_eq!(st.switch_moved(), Some(true), "the ask is told standing");
    assert_eq!(st.switch_moved(), None, "once");
    // THE SAME ANSWER: held again, with no second prompt.
    now += 2 * MIN;
    st.took();
    let mut answer = r(Autopilot::Land, Some(Duration::from_secs(30)));
    let again = loop {
        match act(&mut st, &answer, now) {
            TurnEndAction::WaitUntil { until, .. } => {
                now = until;
                answer.worked = None;
            }
            a => break a,
        }
    };
    assert_eq!(
        again,
        TurnEndAction::Nothing,
        "held again at the same answer"
    );
    assert_eq!(st.held_by(), Some(HeldBy::SameReply));
    assert_eq!(st.switch_ask(), SwitchAsk::None, "the ask is spent");
    assert_eq!(st.switch_moved(), Some(false), "and told so");
    assert_eq!(
        exception_for(&st, &answer, &cfg(), now),
        Some(Exception::repeats(HeldBy::SameReply)),
        "the stop stands again"
    );
    for k in 1..=3 {
        assert_eq!(
            act(&mut st, &r(Autopilot::Land, None), now + k * MIN),
            TurnEndAction::Nothing,
            "no second prompt: read {k}"
        );
    }
    // A switch to Cruise over the hold is no turn of the person's.
    now += 5 * MIN;
    assert!(
        matches!(
            at(&mut st, &r(Autopilot::Cruise, None), now),
            TurnEndAction::WaitUntil { .. }
                | TurnEndAction::Type {
                    rule_id: RULE_CONTINUE,
                    ..
                }
        ),
        "Cruise continues without waiting for a person"
    );
    // THE ASK IS SPENT ON THIS REPLY: under Cruise the stop names no switch, and the person's
    // switch back to Land over the same reply types nothing.
    assert_eq!(
        exception_for(&st, &r(Autopilot::Cruise, None), &cfg(), now),
        None,
        "Cruise schedules recovery after the switch as well"
    );
    now += MIN;
    assert_eq!(
        act(&mut st, &r(Autopilot::Land, None), now),
        TurnEndAction::Nothing,
        "a second switch over the same held reply asks nothing"
    );
    assert_eq!(st.switch_ask(), SwitchAsk::None);
    st.park_off();
    assert_eq!(st.held_by(), None, "Off's park clears the replies");
    // The short turns' back-off stands across the park, and Land's instruction waits none of it
    // (v6 §14.10).
    assert!(st.short_wait().is_some(), "{st:?}");
    assert_eq!(
        act(&mut st, &r(Autopilot::Land, None), now + MIN),
        land_instruction(),
        "Off, then Land, types Land's act, with no why"
    );
}

/// The person's switch back to Land over Land's own hold asks too: Land held by its own reply,
/// switched to Cruise and back by the person, asks once; where the agent works at the ask, Land's
/// episode goes on as at any switch and the ask is spent.
#[test]
fn a_persons_switch_back_to_land_over_lands_own_hold_asks_once_and_work_at_the_ask_spends_it() {
    let (mut st, mut now) = held_on(Autopilot::Land, ALL_DONE);
    let r = |mode, said: &str, worked| by_person(under(mode, idle(said, worked)));
    assert_eq!(
        act(&mut st, &r(Autopilot::Land, ALL_DONE, None), now + MIN),
        TurnEndAction::Nothing,
        "Land's own hold types nothing while the word stays Land"
    );
    now += 2 * MIN;
    assert!(
        matches!(
            at(&mut st, &r(Autopilot::Cruise, ALL_DONE, None), now),
            TurnEndAction::WaitUntil { .. }
                | TurnEndAction::Type {
                    rule_id: RULE_CONTINUE,
                    ..
                }
        ),
        "Cruise continues without waiting for a person"
    );
    now += MIN;
    let asked = act(&mut st, &r(Autopilot::Land, ALL_DONE, None), now);
    assert!(
        asked.rule_id() == Some(RULE_LAND)
            && matches!(&asked, TurnEndAction::Type { text, .. } if is_switch_ask(text)),
        "{asked:?}"
    );
    // The agent works at the ask: the hold lifts, and Land goes on from its act.
    now += 5 * MIN;
    st.took();
    let worked = act(&mut st, &r(Autopilot::Land, WORK, Some(4 * MIN)), now);
    assert_eq!(st.switch_ask(), SwitchAsk::None, "{worked:?}");
    assert_eq!(st.held_by(), None, "{worked:?}");
    assert!(
        !matches!(&worked, TurnEndAction::Type { text, .. } if is_switch_ask(text)),
        "no second ask: {worked:?}"
    );
}

/// LIMIT 1 HOLDS AT A SWITCH NO PERSON MADE (v6 §14.17 item 3): a move to Land by `aterm ctl`, a
/// timer or the harness (meta `autopilot_by=` not `human:…`) over a held tab is no turn of the
/// person's, so Land types nothing and the stop stands; the person's own switch then asks.
#[test]
fn a_switch_to_land_no_person_made_over_a_held_tab_types_nothing() {
    let (mut st, mut now) = held_on(Autopilot::Cruise, ALL_DONE);
    let r = |mode| under(mode, idle(ALL_DONE, None));
    now += 2 * MIN;
    assert_eq!(
        act(&mut st, &r(Autopilot::Land), now),
        TurnEndAction::Nothing,
        "a ctl's or timer's switch to Land asks nothing"
    );
    assert_eq!(st.switch_ask(), SwitchAsk::None);
    assert_eq!(st.held_by(), Some(HeldBy::SameReply), "the stop stands");
    now += MIN;
    assert_eq!(
        act(&mut st, &r(Autopilot::Land), now),
        TurnEndAction::Nothing
    );
    now += MIN;
    assert!(
        matches!(
            at(&mut st, &r(Autopilot::Cruise), now),
            TurnEndAction::WaitUntil { .. }
                | TurnEndAction::Type {
                    rule_id: RULE_CONTINUE,
                    ..
                }
        ),
        "Cruise continues without waiting for a person"
    );
    now += MIN;
    let asked = act(&mut st, &by_person(r(Autopilot::Land)), now);
    assert!(
        matches!(&asked, TurnEndAction::Type { text, .. } if is_switch_ask(text)),
        "the person's own switch asks: {asked:?}"
    );
}

/// THE SWITCH'S ONE ASK STAYS SPENT ACROSS A RESTART (the post-landing review, 2026-11-01): the
/// ask typed and answered the same way, a loop that follows seeded with the replies and the
/// spent mark ([`TurnEndState::seed_switch_spent`]) types nothing at the person's flip to Cruise
/// and back over the same reply. CONTROL: seeded without the mark, the flip asks again; a mark
/// no seeded reply has is dropped.
#[test]
fn a_switch_ask_spent_before_a_restart_is_not_typed_again() {
    let (mut st, mut now) = held_on(Autopilot::Cruise, ALL_DONE);
    let r = |mode, worked| by_person(under(mode, idle(ALL_DONE, worked)));
    now += 2 * MIN;
    let asked = act(&mut st, &r(Autopilot::Land, None), now);
    assert!(
        matches!(&asked, TurnEndAction::Type { text, .. } if is_switch_ask(text)),
        "{asked:?}"
    );
    now += 2 * MIN;
    st.took();
    let mut answer = r(Autopilot::Land, Some(Duration::from_secs(30)));
    while let TurnEndAction::WaitUntil { until, .. } = act(&mut st, &answer, now) {
        now = until;
        answer.worked = None;
    }
    let spent = st.switch_spent.expect("spent on the held reply");
    let following = |mark| {
        let mut next = TurnEndState::default();
        next.seed_replies(st.replies, Duration::from_secs(60), now);
        next.seed_switch_spent(mark);
        next
    };
    for (mark, asks) in [(Some(spent), false), (None, true)] {
        let mut next = following(mark);
        let mut at = now + MIN;
        assert!(
            matches!(
                super::at(&mut next, &r(Autopilot::Cruise, None), at),
                TurnEndAction::WaitUntil { .. }
                    | TurnEndAction::Type {
                        rule_id: RULE_CONTINUE,
                        ..
                    }
            ),
            "Cruise continues without waiting for a person"
        );
        at += MIN;
        let flip = act(&mut next, &r(Autopilot::Land, None), at);
        assert_eq!(
            matches!(&flip, TurnEndAction::Type { text, .. } if is_switch_ask(text)),
            asks,
            "seeded {mark:?}: {flip:?}"
        );
    }
    let unknown = following(Some(spent ^ 1));
    assert_eq!(unknown.switch_spent, None, "no seeded reply has it");
}

/// AN OFF NO PERSON SET KEEPS THE HOLD (the post-landing review, 2026-11-01): `aterm ctl`'s Off,
/// then its Land, over a held tab types nothing, for the park forgot no reply that holds
/// ([`TurnEndState::park_off_held`]); the person's own switch then asks once. CONTROL: the person's
/// own Off ([`TurnEndState::park_off`]) forgets the hold, and Land types its act.
#[test]
fn an_off_then_land_no_person_made_over_a_held_tab_types_nothing() {
    let (mut st, mut now) = held_on(Autopilot::Cruise, ALL_DONE);
    let r = |mode| under(mode, idle(ALL_DONE, None));
    let mut persons = st.clone();
    let replies = st.replies;
    now += 2 * MIN;
    st.park_off_held();
    assert_eq!(
        st.held_by(),
        Some(HeldBy::SameReply),
        "the hold stands past Off"
    );
    assert_eq!(
        act(&mut st, &r(Autopilot::Land), now),
        TurnEndAction::Nothing,
        "Off, then Land, both by ctl, over the held reply"
    );
    now += MIN;
    assert!(
        matches!(
            at(&mut st, &r(Autopilot::Cruise), now),
            TurnEndAction::WaitUntil { .. }
                | TurnEndAction::Type {
                    rule_id: RULE_CONTINUE,
                    ..
                }
        ),
        "Cruise continues without waiting for a person"
    );
    now += MIN;
    let asked = act(&mut st, &by_person(r(Autopilot::Land)), now);
    assert!(
        matches!(&asked, TurnEndAction::Type { text, .. } if is_switch_ask(text)),
        "the person's own switch asks: {asked:?}"
    );
    persons.park_off();
    assert_eq!(
        persons.held_by(),
        None,
        "the person's Off clears the replies"
    );
    assert_eq!(
        act(&mut persons, &r(Autopilot::Land), now),
        land_instruction(),
        "the person's Off, then Land, types Land's act"
    );
    // A loop the host starts as such an Off lifts ([`TurnEndState::start_lifted_off`]) keeps the
    // hold the same way: ctl's lift to Land types nothing, the person's asks once.
    let seeded_at = now;
    now += MIN;
    let lifted = |held| {
        let mut next = TurnEndState::default();
        next.seed_replies(replies, Duration::from_secs(60), seeded_at);
        next.start_lifted_off(if held {
            OffStood::Held
        } else {
            OffStood::Person
        });
        next
    };
    assert_eq!(
        act(&mut lifted(true), &r(Autopilot::Land), now),
        TurnEndAction::Nothing,
        "ctl's lift to Land over the held reply"
    );
    let asked = act(&mut lifted(true), &by_person(r(Autopilot::Land)), now);
    assert!(
        matches!(&asked, TurnEndAction::Type { text, .. } if is_switch_ask(text)),
        "the person's own lift asks: {asked:?}"
    );
    assert_eq!(
        act(&mut lifted(false), &r(Autopilot::Land), now),
        land_instruction(),
        "CONTROL: lifted from the person's Off, Land types its act"
    );
}

/// THE ASK QUOTES A REPLY THE AGENT STOPPED AT: the person's switch to Land read at a point whose
/// reply is new (the agent worked past the hold) is Land's own act, never an ask quoting where it
/// "stopped".
#[test]
fn a_switch_at_a_point_past_the_hold_types_no_ask() {
    let (mut st, mut now) = held_on(Autopilot::Cruise, ALL_DONE);
    now += 2 * MIN;
    let a = act(&mut st, &by_person(land(idle(WORK, Some(4 * MIN)))), now);
    assert!(
        !matches!(&a, TurnEndAction::Type { text, .. } if is_switch_ask(text)),
        "an ask quoting a reply the agent did not stop at: {a:?}"
    );
}

/// THE SWITCH MADE WHILE THE AGENT ANSWERS, its answer forming the hold at that point, is weighed
/// again where the answer is counted (§14.17.10): Land asks once at once, rather than the tab
/// holding with nothing typed.
#[test]
fn a_switch_whose_answer_forms_the_hold_asks_once() {
    let (mut st, mut now) = begun_under(Autopilot::Cruise, false);
    let r = |mode, worked| by_person(under(mode, idle(ALL_DONE, worked)));
    loop {
        assert!(now < t0() + 60 * MIN, "held within the hour");
        now += 2 * MIN;
        st.took();
        let mut probe = st.clone();
        let a = act(
            &mut probe,
            &r(Autopilot::Cruise, Some(Duration::from_secs(30))),
            now,
        );
        if probe.held_by().is_some() {
            let b = act(
                &mut st,
                &r(Autopilot::Land, Some(Duration::from_secs(30))),
                now,
            );
            assert!(
                matches!(&b, TurnEndAction::Type { text, .. } if is_switch_ask(text)),
                "the switch at the answer that holds: {b:?} (under Cruise: {a:?})"
            );
            return;
        }
        if let TurnEndAction::WaitUntil { until, .. } = act(
            &mut st,
            &r(Autopilot::Cruise, Some(Duration::from_secs(30))),
            now,
        ) {
            now = until;
            act(&mut st, &r(Autopilot::Cruise, None), now);
        }
    }
}

/// A SWITCH SPENT BY ANOTHER ACT: a switch read past the hold (the agent worked) is spent by
/// Land's own instruction, so Land's own later hold is never answered with the switch's ask.
#[test]
fn a_switch_spent_by_another_act_never_asks_at_a_later_hold() {
    let (mut st, mut now) = held_on(Autopilot::Cruise, ALL_DONE);
    now += 2 * MIN;
    let a = act(&mut st, &by_person(land(idle(WORK, Some(4 * MIN)))), now);
    assert!(
        matches!(&a, TurnEndAction::Type { text, .. } if !is_switch_ask(text)),
        "Land's own act: {a:?}"
    );
    let said = "Pushed the branch. All done here.";
    let r = |worked| by_person(land(idle(said, worked)));
    for _ in 0..30 {
        now += 2 * MIN;
        st.took();
        let mut b = act(&mut st, &r(Some(Duration::from_secs(30))), now);
        if let TurnEndAction::WaitUntil { until, .. } = b {
            now = until;
            b = act(&mut st, &r(None), now);
        }
        assert!(
            !matches!(&b, TurnEndAction::Type { text, .. } if is_switch_ask(text)),
            "the switch's ask at Land's own hold, no switch since: {b:?}"
        );
        if st.held_by().is_some() && b == TurnEndAction::Nothing {
            return;
        }
    }
    panic!("never held under Land: {st:?}");
}

/// LIMIT 1 HOLDS AT A WALL'S HOLD (v6 §14.17 item 3): the person's switch to Land over a hold its
/// spent wall tries made is no prompt the agent can act on, at the wall or once its notice
/// leaves: the wall's arms answer its repeat before the switch's ask is weighed, and the stop
/// under Cruise names no switch (the same switch with no wall asks,
/// [`a_switch_to_land_over_a_standing_hold_asks_once_and_the_same_answer_holds_again`]).
#[test]
fn a_switch_to_land_over_a_walls_spent_hold_types_nothing() {
    let (mut st, mut now) = begun_under(Autopilot::Cruise, false);
    let kind = WallKind::ApiError {
        code: None,
        retryable: true,
        cause: aterm_phase::ApiCause::CutOff,
    };
    let mut w = by_person(walled(
        kind,
        "API Error: Your computer went to sleep mid-response. The response above may be \
         incomplete.",
        Some(Duration::from_secs(4)),
    ));
    for _ in 0..=12 {
        now += Duration::from_secs(5);
        match act(&mut st, &w, now) {
            TurnEndAction::WaitUntil { until, .. } => {
                now = until;
                w.worked = None;
            }
            TurnEndAction::Nothing => break,
            _ => w.worked = Some(Duration::from_secs(4)),
        }
    }
    assert_eq!(st.held_by(), Some(HeldBy::SameReply), "{st:?}");
    w.worked = None;
    assert!(!st.switch_would_ask(&w), "the wall's stop names no switch");
    now += MIN;
    assert_eq!(
        act(&mut st, &land(w.clone()), now),
        TurnEndAction::Nothing,
        "at the wall"
    );
    let left = TurnEndReading {
        wall: None,
        wall_message: String::new(),
        ..w
    };
    now += MIN;
    assert_eq!(
        act(&mut st, &land(left), now),
        TurnEndAction::Nothing,
        "once the wall's notice left the screen"
    );
}

/// THE SWITCH'S ASK FITS (v6 §14.17 item 3): the held quote is the defense's paragraph where one
/// stands, else the last, on one line with straight quotes and its one closing full stop left to
/// the why's (an ellipsis whole); a long one is cut to its room with `…`, so the act with a long
/// rules suffix fits [`VERBATIM_UNITS`], Land's own words whole.
#[test]
fn the_switchs_ask_quotes_the_held_reply_cut_to_fit_the_verbatim_bound() {
    assert_eq!(
        held_quote("Ran the suites.\n\nNothing left: all of it\n  is on \u{201c}main\u{201d}.\n"),
        "Nothing left: all of it is on \"main\""
    );
    assert_eq!(
        held_quote("Para one.\n\nPara\ttwo, line one\nline two\n\n"),
        "Para two, line one line two"
    );
    assert_eq!(
        held_quote("Waiting on the build..."),
        "Waiting on the build..."
    );
    assert_eq!(
        held_quote("Waiting on the build\u{2026}"),
        "Waiting on the build\u{2026}"
    );
    assert_eq!(held_quote("All done!"), "All done!");
    assert_eq!(held_quote(""), "");
    assert!(
        !switch_why(&held_quote(ALL_DONE)).contains(".\u{201d}."),
        "one full stop closes the quote"
    );
    let long = format!(
        "{}All done.",
        "Merged the parser and its tests into main. ".repeat(12)
    );
    let rules = "never push to main without a review; ".repeat(12);
    let (mut st, mut now) = held_on(Autopilot::Cruise, &long);
    now += 2 * MIN;
    let r = TurnEndReading {
        rules: Some(rules),
        ..by_person(land(idle(&long, None)))
    };
    let a = act(&mut st, &r, now);
    let TurnEndAction::Type { text, rule_id } = &a else {
        panic!("the switch types Land's act: {a:?}");
    };
    assert_eq!(*rule_id, RULE_LAND);
    assert!(
        units(text) <= VERBATIM_UNITS,
        "{} units: {text}",
        units(text)
    );
    assert!(
        text.contains(&format!("\u{2026}{SWITCH_WHY_TAIL}")),
        "the quote cut to its room: {text}"
    );
    assert!(
        past_switch_why(text).starts_with(LAND),
        "Land's words whole: {text}"
    );
}

/// THE PERSON'S OWN LIFT FROM AN OFF NO PERSON SET, OVER A TAB HELD UNDER LAND, ASKS ONCE (the
/// second review, 2026-11-01): the park reads its Off as the word it leaves
/// ([`TurnEndState::park_off_held`]), so their switch back to Land is a move from Off, as a loop
/// the host starts as that Off lifts reads it ([`TurnEndState::start_lifted_off`]); `aterm ctl`'s
/// Land types nothing. Negative control: the park leaving the word it saw (Land) reads the
/// person's Land as no move, and types nothing until they flip to Cruise and back.
#[test]
fn the_persons_own_lift_from_an_off_no_person_set_over_a_land_hold_asks_once() {
    let r = || under(Autopilot::Land, idle(ALL_DONE, None));
    for persons in [false, true] {
        let (mut st, now) = held_on(Autopilot::Land, ALL_DONE);
        assert_eq!(act(&mut st, &r(), now + MIN), TurnEndAction::Nothing);
        assert_eq!(st.park_off_held(), OffStood::Held);
        let lift = if persons { by_person(r()) } else { r() };
        let a = act(&mut st, &lift, now + 3 * MIN);
        if persons {
            assert!(
                matches!(&a, TurnEndAction::Type { text, .. } if is_switch_ask(text)),
                "the person's own lift asks: {a:?}"
            );
        } else {
            assert_eq!(a, TurnEndAction::Nothing, "ctl's lift");
        }
    }
}

#[test]
fn cruise_repeated_reports_are_paced_and_recover_without_a_person() {
    let (mut st, mut now) = begun_under(Autopilot::Cruise, false);
    let mut recoveries = 0;
    for _ in 0..12 {
        st.took();
        now += 2 * MIN;
        let mut r = under(
            Autopilot::Cruise,
            idle(CLEAN, Some(Duration::from_secs(10))),
        );
        loop {
            let action = act(&mut st, &r, now);
            match &action {
                TurnEndAction::WaitUntil { until, .. } => {
                    assert!(*until > now);
                    now = *until;
                    r.worked = None;
                }
                TurnEndAction::Type { text, rule_id } => {
                    if st.held_by().is_some() {
                        assert_eq!(*rule_id, RULE_CONTINUE);
                        assert!(text.contains("different safe approach"));
                        recoveries += 1;
                    }
                    st.ledgered(rule_id, SINCE_MS);
                    break;
                }
                other => panic!("Cruise must continue after repeated reports: {other:?}"),
            }
        }
    }
    assert!(
        recoveries >= 2,
        "recovery must continue across repeated failures"
    );
}
