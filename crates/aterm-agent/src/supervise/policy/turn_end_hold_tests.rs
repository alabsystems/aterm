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
    format!("{CLEAN}\nThe docs PR is still open.")
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
    let rows: Vec<(&str, String, bool, Option<RepoWork>, LandHeld)> = vec![
        (
            "L1",
            WORK.to_string(),
            false,
            None,
            held(&[Defense], &[], 0, [0; 5]),
        ),
        (
            "L2",
            open_defense(),
            false,
            None,
            held(&[Open], &["still open"], 0, [0; 5]),
        ),
        (
            "L3",
            needs_defense(),
            false,
            None,
            held(&[Needs], &[], 0, [0; 5]),
        ),
        (
            "L4",
            CLEAN.to_string(),
            false,
            Some(found(0, 1, 0, 0, 0)),
            held(&[Repo], &[], 0, [0, 1, 0, 0, 0]),
        ),
        ("L6", tall, true, None, held(&[Needs, Cut], &[], 0, [0; 5])),
        (
            "L8",
            CLEAN.to_string(),
            false,
            Some(found(1, 0, 0, 0, 0)),
            held(&[Repo], &[], 0, [1, 0, 0, 0, 0]),
        ),
        (
            "L9",
            CLEAN.to_string(),
            false,
            Some(found(0, 0, 1, 0, 0)),
            held(&[Repo], &[], 0, [0, 0, 1, 0, 0]),
        ),
        (
            "L10",
            CLEAN.to_string(),
            false,
            Some(found(0, 0, 0, 1, 1)),
            held(&[Repo], &[], 0, [0, 0, 0, 1, 1]),
        ),
        (
            "L11",
            CLEAN.to_string(),
            false,
            Some(found(0, 0, 0, 0, 1)),
            held(&[Repo], &[], 0, [0, 0, 0, 0, 1]),
        ),
    ];
    for (case, said, cut, repo, want) in &rows {
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
            let r = yield_of(said, *cut, repo.clone());
            at(&mut st, &r, now);
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
        at(&mut st, &yield_of(CLEAN, false, None), now);
        assert!(st.landed(), "{case}: L5");
        assert_eq!(st.land_held(), None, "{case}: a landing holds nothing");
        assert_eq!(st.face(), TaskFace::Landed, "{case}");
    }
    let (mut st, now) = instructed();
    at(
        &mut st,
        &yield_of(&open_defense(), false, None),
        now + 2 * MIN,
    );
    let words: Vec<&str> = st
        .land_held()
        .into_iter()
        .flat_map(LandHeld::open_words)
        .collect();
    assert_eq!(words, ["still open"]);
    let both = yield_of(&open_defense(), false, Some(found(0, 1, 0, 0, 0)));
    assert_eq!(first_leg_only(&both), Open.bit());
    let (mut st, now) = instructed();
    at(&mut st, &both, now + 2 * MIN);
    let legs = st.land_held().map(|h| h.legs);
    assert_eq!(legs, Some(Open.bit() | Repo.bit()));
    assert_ne!(legs, Some(first_leg_only(&both)));
}

#[test]
fn the_hold_ends_at_a_landing_another_mode_and_off() {
    let open = || {
        let (mut st, now) = instructed();
        at(
            &mut st,
            &yield_of(&open_defense(), false, None),
            now + 2 * MIN,
        );
        assert!(st.land_held().is_some_and(|h| h.read), "held");
        (st, now + 2 * MIN)
    };
    let (mut st, now) = open();
    owed(&mut st, &yield_of(&open_defense(), false, None), now);
    at(&mut st, &yield_of(CLEAN, false, None), now + 20 * MIN);
    assert!(st.landed());
    assert_eq!(st.land_held(), None, "landed");
    let (mut st, now) = open();
    let thanks = land(TurnEndReading {
        person: Some(Duration::from_secs(8)),
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
        let (mut st, now) = open();
        at(
            &mut st,
            &under(mode, idle(WORK, Some(3 * MIN))),
            now + 5 * MIN,
        );
        assert_eq!(st.land_held(), None, "{mode:?}");
    }
    let (mut st, _) = open();
    st.park_off();
    assert_eq!(st.land_held(), None, "parked");
    let (mut st, now) = open();
    let a = owed(&mut st, &yield_of(&open_defense(), false, None), now);
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
    let y = yield_of(&open_defense(), false, None);
    at(&mut st, &y, now + 2 * MIN);
    assert!(st.held_moved(true).flatten().is_some_and(|h| h.read));
    at(&mut st, &land(idle(&open_defense(), None)), now + 3 * MIN);
    assert_eq!(st.held_moved(true), None, "the same point read again");
    let a = owed(&mut st, &y, now + 4 * MIN);
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    assert!(st.held_moved(true).flatten().is_some_and(|h| h.nudges == 1));
    st.took();
    assert_eq!(st.held_moved(true), None, "its own turn moves nothing");
    at(&mut st, &yield_of(CLEAN, false, None), now + 30 * MIN);
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
    at(&mut st, &yield_of(&open_defense(), false, None), t0());
    assert_eq!(
        st.land_held(),
        Some(LandHeld {
            nudges: 3,
            ..held(&[HoldLeg::Open], &["still open"], 3, [0; 5])
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
        std::mem::size_of::<LandHeld>() <= 40,
        "{} bytes",
        std::mem::size_of::<LandHeld>()
    );
    let debug = format!("{a:?}");
    assert!(
        debug.contains("\"still open\"") && !debug.contains("Instant"),
        "{debug}"
    );
}

#[test]
fn every_word_the_strict_reading_names_has_an_index() {
    let mut replies: Vec<String> = super::corpora::land_table()
        .into_iter()
        .map(|(_, reply)| reply)
        .collect();
    replies.extend(needs::needs_table().into_iter().map(|(_, reply)| reply));
    let excerpts = super::corpora::open_work_reports()
        .into_iter()
        .flat_map(|(_, excerpts)| excerpts);
    replies.extend(excerpts.map(|(_, text)| text));
    let replies = &replies;
    let words: Vec<&'static str> = std::thread::scope(|scope| {
        let readers: Vec<_> = (0..4)
            .map(|first| {
                scope.spawn(move || {
                    let mut words = Vec::new();
                    for reply in replies.iter().skip(first).step_by(4) {
                        words.extend(land_open_words(Some(reply)));
                        words.extend(open_work_strict(Some(reply)));
                        words.extend(open_work_strict(Some(&needs::defense_body(reply))));
                    }
                    words
                })
            })
            .collect();
        readers
            .into_iter()
            .flat_map(|reader| reader.join().expect("a reader of the replies"))
            .collect()
    });
    let named = words.len();
    let mut missing: Vec<&str> = words
        .into_iter()
        .filter(|&w| !OpenWord::of(w).is_some_and(|i| i.word() == w))
        .collect();
    assert!(named > 200, "{named} words named");
    missing.sort_unstable();
    missing.dedup();
    assert!(missing.is_empty(), "no index: {missing:?}");
    for w in open_words() {
        let i = open_word_index(w).expect("indexed");
        assert_eq!(open_word_at(i), Some(w));
    }
    assert_eq!(OpenWord::of("a word no table holds"), None);
}

#[test]
fn work_after_a_landing_is_held_from_lands_next_act() {
    let (mut st, now) = instructed();
    at(
        &mut st,
        &yield_of(&open_defense(), false, None),
        now + 2 * MIN,
    );
    let a = owed(
        &mut st,
        &yield_of(&open_defense(), false, None),
        now + 2 * MIN,
    );
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    st.ledgered(RULE_LAND_NUDGE, SINCE_MS + 4 * 60_000);
    at(&mut st, &yield_of(CLEAN, false, None), now + 20 * MIN);
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
    let first = land(idle(&open_defense(), None));
    let mut st = seeded();
    at(&mut st, &first, t0());
    assert_eq!(
        st.land_held(),
        Some(held(&[HoldLeg::Open], &["still open"], 2, [0; 5])),
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
    let y = yield_of(&open_defense(), false, None);
    at(&mut st, &y, now + 2 * MIN);
    let last = st.land_held().expect("held");
    let a = owed(&mut st, &y, now + 2 * MIN);
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    let untaken = land(TurnEndReading {
        repo: crate::harness::repo_work::RepoRead::Found(found(0, 1, 0, 0, 0)),
        ..idle(&open_defense(), None)
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
    at(
        &mut st,
        &yield_of(&open_defense(), false, None),
        now + 2 * MIN,
    );
    let read = st.land_held().expect("held");
    assert!(read.read && read.holds(HoldLeg::Open), "{read:?}");
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
    at(
        &mut st,
        &yield_of(&open_defense(), false, None),
        now + 30 * MIN,
    );
    let again = st.land_held().expect("held");
    assert!(
        again.read && again.holds(HoldLeg::Open) && again.nudges == 1,
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
            match act(&mut st, &r, now) {
                TurnEndAction::Nothing => {
                    return (k + 1, exception_for(&st, &r, &cfg(), now), st.held_by());
                }
                // The back-off waited out, the same screen read again at its end.
                TurnEndAction::WaitUntil { until, .. } => {
                    now = until;
                    r.worked = None;
                }
                a => {
                    let rule = a.rule_id().unwrap_or_else(|| panic!("{mode:?}: {a:?}"));
                    st.ledgered(rule, SINCE_MS + 60_000 * i64::try_from(k + 1).unwrap_or(0));
                    break;
                }
            }
        }
    }
    panic!("{mode:?}: never held");
}

/// One case of the loud hold: its name, mode, answer, repository finding, whether a manager
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
fn the_one_loud_hold_is_lands_with_repository_work_in_a_session_no_manager_waits_on() {
    use crate::harness::acts::HeldBy::SameReply;
    let repo = found(0, 1, 0, 0, 0);
    let rows: [HoldCase; 6] = [
        (
            "land, a Repo leg",
            Autopilot::Land,
            CLEAN.to_string(),
            Some(repo.clone()),
            false,
            Exception::repeats_land(SameReply, Some(&repo)),
        ),
        (
            "land, an Open leg and a Repo leg",
            Autopilot::Land,
            open_defense(),
            Some(repo.clone()),
            false,
            Exception::repeats_land(SameReply, Some(&repo)),
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
            Exception::repeats(SameReply),
        ),
    ];
    for (case, mode, said, repo, managed, want) in rows {
        let (answers, told, held) = held_after(mode, managed, repo, |_| said.clone());
        assert_eq!(answers, 2, "{case}: held at the second same answer");
        assert_eq!(held, Some(SameReply), "{case}");
        assert_eq!(told.as_ref(), Some(&want), "{case}");
    }
    let loud = Exception::repeats_land(SameReply, Some(&repo));
    assert_eq!(
        (loud.kind, loud.when, loud.held, loud.title),
        (
            ExceptionKind::Stalled,
            Urgency::Now,
            Some(SameReply),
            Some(("aterm stopped typing", "Stopped typing"))
        )
    );
    assert!(
        loud.why
            .contains(&crate::supervise::policy::turn_end::repo_finding(&repo)),
        "its why names the repository's finding: {loud:?}"
    );
    let quiet = Exception::repeats(SameReply);
    assert_eq!((quiet.when, quiet.title), (Urgency::Later, None));
}

/// A LAND HOLD AT A SPENT API WALL IS THE WALL'S (limit 3 of §14.0): Land's nudge read the Repo
/// leg, then a cut reply (an API wall that is no outage) answered each of aterm's tries the same
/// until they were spent, which holds at the wall's reply (`Kept::holds`). That hold is told Later
/// at the wall, and stays Later once the wall's notice leaves the screen: no band says the agent
/// gave the same reply twice, and the repository's work is not what stopped it. (The same Land run
/// with no wall is `Urgency::Now`: the one loud hold above.)
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
        (Autopilot::Cruise, None, Exception::repeats(NoWork)),
        (
            Autopilot::Land,
            Some(repo.clone()),
            Exception::repeats_land(NoWork, Some(&repo)),
        ),
    ] {
        let (answers, told, held) = held_after(mode, false, repo, worded);
        assert_eq!(answers, NO_WORK_ACTS, "{mode:?}");
        assert_eq!(held, Some(NoWork), "{mode:?}");
        assert_eq!(told, Some(want), "{mode:?}");
    }
    // Each exception says its own arm, in the arm's one wording (`HeldBy::answered`): no
    // no-work hold is told as the same reply, quiet or loud.
    let quiet = Exception::repeats;
    let loud = |held: HeldBy| Exception::repeats_land(held, Some(&repo));
    assert_ne!(quiet(NoWork).why, quiet(HeldBy::SameReply).why);
    assert_ne!(loud(NoWork).why, loud(HeldBy::SameReply).why);
    for held in HeldBy::ALL {
        assert!(loud(held).why.contains(&held.answered()), "{held:?}");
    }
    assert!(quiet(NoWork).why.contains(&NoWork.answered()));
    assert_ne!(quiet(NoWork).clears, quiet(HeldBy::SameReply).clears);
}

#[test]
fn a_switch_to_land_over_a_standing_hold_types_nothing_and_off_then_land_types_lands_act() {
    let (mut st, mut now) = begun_under(Autopilot::Cruise, false);
    let r = |worked| under(Autopilot::Cruise, idle(CLEAN, worked));
    while st.held_by().is_none() {
        assert!(now < t0() + 60 * MIN, "held under Cruise within the hour");
        now += 2 * MIN;
        st.took();
        if let TurnEndAction::WaitUntil { until, .. } =
            act(&mut st, &r(Some(Duration::from_secs(30))), now)
        {
            now = until;
            act(&mut st, &r(None), now);
        }
    }
    assert_eq!(act(&mut st, &r(None), now + MIN), TurnEndAction::Nothing);
    let switched = land(idle(CLEAN, None));
    assert_eq!(
        act(&mut st, &switched, now + 2 * MIN),
        TurnEndAction::Nothing,
        "a switch to Land restarts no typing"
    );
    st.park_off();
    assert_eq!(st.held_by(), None, "Off's park clears the replies");
    // The short turns' back-off stands across the park, and Land's instruction waits none of it
    // (v6 §14.10).
    assert!(st.short_wait().is_some(), "{st:?}");
    assert_eq!(
        act(&mut st, &switched, now + 3 * MIN),
        land_instruction(),
        "Off, then Land, types Land's act"
    );
}
