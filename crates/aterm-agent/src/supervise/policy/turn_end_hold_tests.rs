// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::harness::acts::{HoldLeg, LandHeld, OpenWord, TaskFace};
use crate::harness::repo_work::RepoWork;
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
fn the_hold_ends_at_a_landing_theirs_another_mode_and_off() {
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
    assert_eq!(st.face(), TaskFace::Theirs);
    assert_eq!(st.land_held(), None, "theirs");
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
    let (a, b) = copy(held(&[HoldLeg::Open], &["still open"], 2, [1, 2, 3, 4, 5]));
    assert_eq!(a, b);
    assert_eq!(std::mem::size_of::<Option<OpenWord>>(), 2);
    assert!(
        std::mem::size_of::<LandHeld>() <= 40,
        "{} bytes",
        std::mem::size_of::<LandHeld>()
    );
    assert!(std::mem::size_of::<[Option<&'static str>; 4]>() >= 64);
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
