// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// WHY LAND STILL HOLDS (`docs/DESIGN-harness-autopilot-2026-10-15.md`
// §12.3): the hold the policy keeps at a land act's yield that did
// not land (`TurnEndState::land_held`, `hold_at_yield`), where it moved
// (`TurnEndState::held_moved`), its `since` (`TurnEndState::ledgered`,
// `TurnEndState::seed_land_hold`), where it begins again after a landing
// (`HoldStart`), and its open words as indices
// (`OpenWord`). Included from `turn_end_tests.rs` (`mod hold`), so its
// readings and acts are in scope. Each behaviour has its negative control.

use crate::harness::acts::{HoldLeg, LandHeld, OpenWord, TaskFace};
use crate::harness::repo_work::RepoWork;
use crate::supervise::policy::turn_end::HoldStart;

// THE FOREVER LAB'S OWN STUB REPLIES (`crates/aterm-lab/src/forever.rs`:
// `WORK`, `CLEAN`, `NEED`, `needs_defense`, `open_defense`, `tall_reply`),
// as its chains answer a land act. The lab depends on this crate, so they
// are spelled here as it spells them.
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

/// What the host's probe finds (`RepoWork`): the forever lab's repository
/// legs (`forever::Repo`), one count each.
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

/// The stamp the loop ledgers Land's instruction at, in these tests.
const SINCE_MS: i64 = 1_771_000_000_000;

/// A hold begun at a row ledgered at `since_ms`, no nudges before it.
fn begun(since_ms: Option<i64>) -> HoldStart {
    HoldStart {
        since_ms,
        ..HoldStart::default()
    }
}

/// A Land episode with its instruction typed and ledgered at [`SINCE_MS`]:
/// the state, and the time of the instruction.
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

/// The yield of a land act: `said`, the reply read cut where `cut`, over
/// the repository's work `repo`.
fn yield_of(said: &str, cut: bool, repo: Option<RepoWork>) -> TurnEndReading {
    land(TurnEndReading {
        said_cut: cut,
        repo,
        ..idle(said, Some(2 * MIN))
    })
}

/// The act owed at the point `r` was read at (`now`), past the short
/// streak's back-off where one stands (a done claim lengthens it): decided
/// and recorded as typed.
fn owed(st: &mut TurnEndState, r: &TurnEndReading, now: Instant) -> TurnEndAction {
    let later = now + st.short_wait().unwrap_or_default();
    let a = decide_turn_end(st, r, &cfg(), later);
    st.acted(&a, r, later);
    a
}

/// A hold read at a yield, its counts and words as given.
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

/// The verdict's own short circuit, as [`clean_defense`] and `observe`'s
/// `clean` read a yield: the FIRST leg that fails, and no other — the
/// builder the hold must not be.
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
    } else if r.repo.is_some() {
        Some(HoldLeg::Repo)
    } else {
        None
    };
    leg.map_or(0, HoldLeg::bit)
}

/// THE FOREVER STUB TABLE (§12.3.6, §12.3.7): each of the lab's land
/// chains, its reply to the land instruction and then to the nudge that
/// follows, over its repository, names every leg that held it and no other
/// — L1 no defense, L2 the open word, L3 the need, L4 one untracked file,
/// L6 the need and a reply too tall, L8 one changed file, L9 one stash, L10
/// one unpushed commit that is not on the remote main either, L11 one
/// commit not on the remote main — with the episode's `since` and the
/// nudges typed; each nudge's own row, ledgered later, leaves `since` at
/// the instruction's. Each chain is the live negative control of the others,
/// and L5, a clean defense over a clean repository, lands: no hold, the
/// face `Landed`. NEGATIVE CONTROL: the verdict's short circuit
/// ([`first_leg_only`]) reports one leg for a defense naming `still open`
/// over an untracked file, where the hold reports both.
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
        // The instruction's yield, then the nudge it is owed and its yield.
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
                // Every leg: the lab reads each chain's legs by these words.
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
            // The nudge's own row, ledgered later: `since` stays the
            // instruction's.
            st.ledgered(RULE_LAND_NUDGE, SINCE_MS + 60_000 * (i64::from(nudges) + 1));
            assert_eq!(
                st.land_held().map(|h| (h.since_ms, h.nudges)),
                Some((Some(SINCE_MS), nudges + 1)),
                "{case}: the nudge's row"
            );
        }
        // L5: the same chain's clean defense over a clean repository lands.
        now += 2 * MIN;
        at(&mut st, &yield_of(CLEAN, false, None), now);
        assert!(st.landed(), "{case}: L5");
        assert_eq!(st.land_held(), None, "{case}: a landing holds nothing");
        assert_eq!(st.face(), TaskFace::Landed, "{case}");
    }
    // The open word is the strict reading's own spelling, whole.
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
    // NEGATIVE CONTROL: the verdict's short circuit names one leg where two
    // held the landing.
    let both = yield_of(&open_defense(), false, Some(found(0, 1, 0, 0, 0)));
    assert_eq!(first_leg_only(&both), Open.bit());
    let (mut st, now) = instructed();
    at(&mut st, &both, now + 2 * MIN);
    let legs = st.land_held().map(|h| h.legs);
    assert_eq!(legs, Some(Open.bit() | Repo.bit()));
    assert_ne!(legs, Some(first_leg_only(&both)));
}

/// THE HOLD ENDS where the episode does or the landing is no longer Land's
/// to push: at a landing, at a person's own exchange (the face
/// [`TaskFace::Theirs`]), under Cruise, under Off and at a park. At SOMEONE
/// ELSE'S live turn the legs drop (`read` false) and `since` and the nudges
/// stay, through that turn's end, until Land's next yield names its legs
/// again. NEGATIVE CONTROL: the policy's own act's live turn (the nudge
/// running) keeps the last yield's legs — only someone else's drops them —
/// and a person's quick turn after the hold leaves no hold standing, where
/// a state that kept it there would show the last yield's legs through the
/// person's exchange.
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
    // At a landing.
    let (mut st, now) = open();
    owed(&mut st, &yield_of(&open_defense(), false, None), now);
    at(&mut st, &yield_of(CLEAN, false, None), now + 20 * MIN);
    assert!(st.landed());
    assert_eq!(st.land_held(), None, "landed");
    // At a person's own exchange.
    let (mut st, now) = open();
    let thanks = land(TurnEndReading {
        person: Some(Duration::from_secs(8)),
        ..idle("You're welcome!", Some(Duration::from_secs(5)))
    });
    at(&mut st, &thanks, now + 5 * MIN);
    assert_eq!(st.face(), TaskFace::Theirs);
    assert_eq!(st.land_held(), None, "theirs");
    // Under Cruise, and under Off.
    for mode in [Autopilot::Cruise, Autopilot::Off] {
        let (mut st, now) = open();
        at(
            &mut st,
            &under(mode, idle(WORK, Some(3 * MIN))),
            now + 5 * MIN,
        );
        assert_eq!(st.land_held(), None, "{mode:?}");
    }
    // At a park under Off.
    let (mut st, _) = open();
    st.park_off();
    assert_eq!(st.land_held(), None, "parked");
    // Someone else's live turn: the legs drop, `since` and the nudges stay.
    let (mut st, now) = open();
    let a = owed(&mut st, &yield_of(&open_defense(), false, None), now);
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    let read = st.land_held().expect("held");
    assert!(read.read && read.nudges == 1, "{read:?}");
    // NEGATIVE CONTROL: the nudge's own turn read live keeps the legs.
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
    // …through that turn's end (a job of theirs, no land act's yield).
    let job = land(idle(WORK, Some(20 * MIN)));
    at(&mut st, &job, now + 45 * MIN);
    assert_eq!(st.face(), TaskFace::Open);
    assert_eq!(st.land_held(), Some(dropped), "until Land's next yield");
}

/// A WORKER ON ITS FIRST TASK, spawned under Land, holds none through its
/// first working turn: no instruction typed, no episode begun. Its first
/// point gets the instruction, and from it the hold stands with no legs —
/// `since` from the row's stamp once ledgered, no nudges. NEGATIVE CONTROL:
/// a hold made from the mode alone would stand from the worker's first
/// turn; and a ledgered row of any other act moves no `since`.
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
    // Another act's row moves nothing.
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

/// A NEED SAID EARLIER IN THE TURN IS A LEG: a turn whose earlier message
/// names a need and whose last words are a clean defense does not land
/// (the verdict reads the turn's earlier words), and its hold says `needs`
/// — the reply's own words, said this turn. The leg is never the standing
/// set: a need an earlier report named stands across a later reply that
/// names none ([`TurnEndState::standing_needs`]), and that reply's hold has
/// no `needs` leg. NEGATIVE CONTROLS: reading the last words alone leaves
/// the first hold with no leg at all though it did not land, and reading
/// the standing set names a need the second reply never said.
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
    // NEGATIVE CONTROL: the last words alone hold no leg.
    assert_eq!(first_leg_only(&idle(CLEAN, None)), 0);
    // The standing set is not the leg: a need named by one report, then a
    // reply that names none and no defense.
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
    // NEGATIVE CONTROL: a leg read from the standing set says `needs`.
    assert!(!st.standing_needs().is_empty() && !second.holds(HoldLeg::Needs));
}

/// THE HOLD IS ASKED ONCE PER MOVE ([`TurnEndState::held_moved`]): the first
/// ask reports what stands (none, outside an episode), the same state asked
/// again reports nothing, and each move reports once — the instruction
/// typed, its row's stamp, the yield's legs, the nudge's count, someone
/// else's turn, the landing. NEGATIVE CONTROL: the same point read again
/// moves nothing.
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
    // NEGATIVE CONTROL: the same point read again.
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

/// A HOLD NOT THE LOOP'S TO TELL IS TOLD NONE, AND THE LOOP'S END ONCE
/// ([`TurnEndState::held_moved`]'s `here`, [`TurnEndState::held_ended`]):
/// a hold told, then asked where it is not this loop's (the word read in
/// another mode, a claim not its own) is told none once, the episode kept;
/// asked here again it is told again. At the loop's end the hold told is
/// told gone once, and from there the host is held told none: a later ask
/// with none to tell is no move. NEGATIVE CONTROL: the ask `here` again
/// tells the kept hold, so `here` false is what made it none; and the end
/// that forgot the host was told anything (`held_told` back to never
/// asked) tells none a second time.
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
    // NEGATIVE CONTROL: here again, the kept hold is told.
    assert_eq!(st.held_moved(true), Some(Some(held)), "here again");
    assert!(st.held_ended(), "the loop's end tells the hold gone");
    assert!(!st.held_ended(), "once");
    assert_eq!(st.held_moved(false), None, "told none already");
}

/// `since` IS THE EPISODE'S, seeded by a loop that follows
/// ([`TurnEndState::seed_land_hold`], after [`TurnEndState::seed_land`]):
/// it survives a restart and a seamless update, with the nudges, and the
/// legs come back at the next yield. An older ledger's episode, with no
/// time, seeds none. NEGATIVE CONTROL: a seed outside an episode moves
/// nothing.
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
    // Its first point: the seeded act's yield.
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

/// THE HOLD IS `Copy` AND SMALL: no `Instant`, no `String`, no path; its
/// open words two bytes each (an [`OpenWord`] is an index), so the whole
/// fact is at most 40 bytes, and its `Debug` names the words, never an
/// index. NEGATIVE CONTROL: the words as `&'static str` would take 16 bytes
/// each, 64 for the four.
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

/// EVERY WORD LAND'S STRICT READING NAMES HAS AN INDEX that gives it back
/// ([`open_word_index`], [`open_word_at`]): over the labelled defense table,
/// the needs table and the open-work corpus, each word [`land_open_words`]
/// and [`open_work_strict`] name. And every word of the tables keeps its
/// first place. NEGATIVE CONTROL: a word no table holds has none.
#[test]
fn every_word_the_strict_reading_names_has_an_index() {
    let mut replies: Vec<String> = Vec::new();
    for line in include_str!("fixtures/land-defense-table.txt").lines() {
        if !line.starts_with('#') && !line.starts_with("==") && !line.trim().is_empty() {
            replies.push(line.replace("\\n", "\n"));
        }
    }
    replies.extend(needs::needs_table().into_iter().map(|(_, reply)| reply));
    let mut excerpt = String::new();
    for line in include_str!("fixtures/open-work-corpus.txt").lines() {
        if line.starts_with("---") || line.starts_with("===") {
            replies.push(std::mem::take(&mut excerpt));
        } else if !line.starts_with('#') {
            excerpt.push_str(line);
            excerpt.push('\n');
        }
    }
    replies.push(excerpt);
    // The readings are pure and the corpus is thousands of replies: four readers side by side,
    // each every fourth reply (what is checked is the set of words, not their order).
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

/// A LANDING ENDS THE HOLD, AND WORK GIVEN AFTER IT IS HELD FROM LAND'S NEXT
/// ACT ([`HoldStart`]): the task lands, someone else's turn opens it again
/// (a person's job, hours later), and through that turn and its end no hold
/// stands — the finished landing's `since` and nudges are not the new
/// work's. Land's next act (the episode's nudge, by the engine's own count)
/// begins the hold: its `since` that act's row, no nudges, and each nudge
/// after it counted from there, its row leaving `since` where it is.
/// NEGATIVE CONTROL: a hold the landing does not end tells the new work as
/// `landing since` the first instruction with its nudges, from the first
/// read of the person's turn; and one a recorded landing does not end
/// (`seed_task_done`) does the same after a restart.
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
    // A person's job, hours later: their turn read live, then its end.
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
    // Land's next act: the episode's nudge, which begins the hold.
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
    // Its yield, and the nudge after it: counted from the act that began it.
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
    // A loop that follows seeds the same: the walk's hold
    // (`approvals::LandSeed::hold`), begun at that row with two nudges
    // before it.
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
    // …and one over a landing no land act followed holds none.
    let mut landed = TurnEndState::default();
    landed.seed_land(1, false, false, true);
    landed.seed_land_hold(HoldStart::LANDED);
    landed.face_start();
    assert_eq!(landed.land_held(), None, "a seeded landing's hold");
    // …nor one over a landing the task's record names, its turn reopened.
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

/// WHICH YIELDS ARE JUDGED (§12.3.9): a land act's yield names its legs only
/// where the act was taken and nobody else's turn ended there. A seeded
/// land act's reply read at a loop's first point, no turn seen run, names
/// its legs; the same reply where the screen and the record say the act
/// was never taken (the reply predates it) names none. A land act typed and
/// never taken leaves the last yield's legs as they were, whatever the
/// screen it was typed at shows. And the repository's counts saturate.
/// NEGATIVE CONTROLS: a first point that never judges leaves the seeded
/// act's legs unread; a first point that ignores the act's taking names a
/// reply that predates it; an act's arm that ignores the taking names the
/// screen the act was typed over.
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
    // A seeded act's reply at the first point, no turn seen run.
    let mut st = seeded();
    at(&mut st, &first, t0());
    assert_eq!(
        st.land_held(),
        Some(held(&[HoldLeg::Open], &["still open"], 2, [0; 5])),
        "the first point over a seeded land act"
    );
    // The same reply, where the act was never taken: none.
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
    // A nudge typed and never taken: the point at the screen it was typed
    // at, where the probe now finds an untracked file, judged once the
    // act's window to be taken has passed, keeps the last yield's legs.
    let (mut st, now) = instructed();
    let y = yield_of(&open_defense(), false, None);
    at(&mut st, &y, now + 2 * MIN);
    let last = st.land_held().expect("held");
    let a = owed(&mut st, &y, now + 2 * MIN);
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    let untaken = land(TurnEndReading {
        repo: Some(found(0, 1, 0, 0, 0)),
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
    // The counts saturate.
    let (mut st, now) = instructed();
    at(
        &mut st,
        &yield_of(CLEAN, false, Some(found(70_000, 0, 0, 0, 0))),
        now + 2 * MIN,
    );
    assert_eq!(st.land_held().map(|h| h.changed), Some(u16::MAX));
}

/// SOMEONE ELSE'S TURN SEEN ONLY AT ITS END DROPS THE LEGS, as its live read
/// does (the model's `TheHostIsToldWhatHoldsTheLanding` holds the
/// told legs to the state, and a turn no read saw run is as much someone
/// else's — the legs were the last land act's yield's, and that turn may
/// have changed every one of them): the instruction's yield names open
/// work, read; a job of theirs ends with no live read before it, and the
/// hold reads `since` and the nudges with no legs; Land's nudge typed, its
/// own yield (no live read either) is judged again. NEGATIVE CONTROL (a
/// mutant of `observe`'s arm for a turn nobody's act awaited): kept, the
/// job's point still reads the instruction's yield's open words.
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
    // Their job: no read saw it run, its end the first the loop reads.
    let job = land(idle(WORK, Some(20 * MIN)));
    let a = at(&mut st, &job, now + 25 * MIN);
    assert!(!st.other_turn(), "no live read");
    assert_eq!(st.face(), TaskFace::Open);
    let dropped = LandHeld {
        since_ms: Some(SINCE_MS),
        ..LandHeld::default()
    };
    assert_eq!(st.land_held(), Some(dropped), "someone else's turn");
    // Land's nudge, and its own yield read the same way: judged again.
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
