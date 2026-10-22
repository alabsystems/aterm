// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Autopilot Land'S LANDING, HELD TO ITS WORDS (the adversarial review of
// 2026-10-20): what lands a task and what keeps it landed — an act the worker
// took, a defense still the reply's own, a report read whole, a turn someone
// asked — Land's strict reading of open work, and the acts' words fitted to
// what the composer draws as typed. Included from `turn_end_tests.rs`
// (`mod land_review`), so its readings and acts are in scope. Each behaviour
// has its negative control.

use crate::harness::repo_work::RepoWork;

/// A clean defense other than [`DEFENSE`].
const OTHER_DEFENSE: &str = "Merged the docs and pushed main.\nNothing left: all of it is merged into the remote main branch.";

/// AN ACT NEVER TAKEN LANDS NOTHING ([`Awaited::taken`]): Land's instruction
/// typed at a first point whose reply is already a clean defense (said
/// before Land was chosen), or its nudge typed at a defense the probe found
/// work under, and no read since sees the act's turn run — the same words on
/// the screen, no work, past [`TurnEndTiming::take_within`], the probe now
/// finding nothing — is no landing: the defense predates the act, and the
/// task stays open. The review found the stale defense landed the task.
/// NEGATIVE CONTROL: the same defense at the yield of the act taken (a read
/// saw its turn run) lands.
#[test]
fn an_untaken_land_act_lands_nothing_on_the_defense_it_was_typed_over() {
    let take = TurnEndTiming::default().take_within;
    let instructed = |st: &mut TurnEndState| {
        let (a, then) = until_acted(st, &land(idle(DEFENSE, Some(3 * MIN))), t0());
        assert_eq!(a, land_instruction());
        then
    };
    let nudged = |st: &mut TurnEndState| {
        act(st, &land(idle("Stage 1 landed.", Some(3 * MIN))), t0());
        let r = over_repo(land(idle(DEFENSE, Some(3 * MIN))), uncommitted_repo());
        let (a, then) = past_backoff(st, &r, t0() + 3 * MIN);
        assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
        st.acted(&a, &r, then);
        then
    };
    type Start = fn(&mut TurnEndState) -> Instant;
    let starts: [(&str, Start); 2] = [("instruction", instructed), ("nudge", nudged)];
    for (what, start) in starts {
        let mut st = TurnEndState::default();
        let then = start(&mut st);
        let same = land(idle(DEFENSE, None));
        at(&mut st, &same, then + MIN);
        let a = at(&mut st, &same, then + MIN + take + Duration::from_secs(1));
        assert!(!st.landed() && !st.task_done(), "{what}: {a:?}");
        assert_ne!(
            a,
            TurnEndAction::Nothing,
            "{what}: the claim is owed its act"
        );
        // NEGATIVE CONTROL: the act taken.
        let mut st = TurnEndState::default();
        let then = start(&mut st);
        let a = at(&mut st, &land(idle(DEFENSE, Some(2 * MIN))), then + 2 * MIN);
        assert_eq!(a, TurnEndAction::Nothing, "{what}");
        assert!(st.landed(), "{what}");
    }
}

/// A LANDING OFF STOOD OVER ([`TurnEndState::park_off`]) holds when Land
/// comes back only while the last words are still its own clean defense: a
/// turn the person drove meanwhile, unseen (the park folds no reading in),
/// left words of its own — a report that is no defense, or another defense
/// they asked for — and the task is open again, Land's act owed after the
/// back-off. The review found the landing held. NEGATIVE CONTROL: the
/// landing's own words, unchanged, hold it: nothing typed.
#[test]
fn a_landing_off_stood_over_holds_only_on_its_own_words() {
    for theirs in ["Renamed the lexer's tokens.", OTHER_DEFENSE] {
        let (mut st, now) = landed_at(t0());
        st.park_off();
        let r = land(idle(theirs, None));
        assert!(st.reads_record(&r), "{theirs}: the record is read there");
        let (a, _) = until_acted(&mut st, &r, now + 10 * MIN);
        assert!(!st.landed(), "{theirs}: {a:?}");
        assert!(matches!(a, TurnEndAction::Type { .. }), "{theirs}: {a:?}");
    }
    // NEGATIVE CONTROL: the landing's own words.
    let (mut st, now) = landed_at(t0());
    st.park_off();
    let r = land(idle(DEFENSE, None));
    assert!(!st.reads_record(&r), "its own words: nothing to read");
    let a = at(&mut st, &r, now + 10 * MIN);
    assert_eq!(a, TurnEndAction::Nothing);
    assert!(st.landed());
}

/// THE RECORD'S LANDING HOLDS ONLY AT A POINT THAT IS STILL A CLEAN DEFENSE
/// ([`TurnEndState::record_landing_holds`], the loop's gate before it takes
/// a `done` record on): a first point whose reply is no defense — a turn
/// after the landing no loop watched, the retired token — or one a wall
/// stands over, or a defense the probe finds work under, holds none.
/// NEGATIVE CONTROL: a first point whose reply is a clean defense over a
/// clean repository holds it.
#[test]
fn a_records_landing_holds_only_at_a_clean_defense() {
    let point = |r: TurnEndReading| {
        let mut st = TurnEndState::default();
        st.observe(&r, t0());
        st.record_landing_holds()
    };
    assert!(point(land(idle(DEFENSE, None))));
    for (why, r) in [
        ("a report", land(idle("Renamed the lexer's tokens.", None))),
        ("the token", land(idle(FINISHED_REPORT, None))),
        (
            "a wall",
            land(TurnEndReading {
                wall: Some(WallKind::Overloaded),
                wall_message: "API Error: 529 Overloaded.".to_string(),
                ..idle(DEFENSE, None)
            }),
        ),
        (
            "the repository",
            over_repo(land(idle(DEFENSE, None)), uncommitted_repo()),
        ),
        ("a turn running into it", land(idle(DEFENSE, Some(3 * MIN)))),
    ] {
        assert!(!point(r), "{why}");
    }
}

/// A LANDED TASK'S OWN WAKE KEEPS THE LANDING
/// ([`TurnEndReading::finished_record`]): a turn the conversation's record
/// says nobody asked — Claude Code's own wake for a background task — that
/// ends on any reply, on a 529 or on a usage limit leaves the task landed,
/// and nothing is typed, then or a day later past the reset. The review
/// found the wake reopened the landing: a retry or a land nudge typed into
/// landed work (main kept a finished task through it, and the forever
/// redesign dropped that). NEGATIVE CONTROLS: no record, and a record saying
/// someone asked since (a person's prompt), reopen the task, and Land's act
/// or the wall's ladder types.
#[test]
fn a_landed_tasks_own_wake_keeps_the_landing() {
    let wake_at = t0() + 30 * MIN;
    let worked = Some(Duration::from_secs(20));
    let overloaded = land(quiet_walled(
        WallKind::Overloaded,
        "API Error: 529 Overloaded.",
        worked,
    ));
    let mut usage = land(quiet_walled(
        WallKind::UsageSession,
        "You've hit your session limit · resets 3pm",
        worked,
    ));
    usage.phase = Phase::Limited {
        message: usage.wall_message.clone(),
        reset: Some("3pm".to_string()),
    };
    usage.reset_at = Some(wake_at + 60 * MIN);
    let quiet = land(idle(QUIET_WAKE, worked));
    for wake in [&quiet, &overloaded, &usage] {
        let what = wake.said_tail.clone().unwrap_or_default() + " " + &wake.wall_message;
        let (mut st, _) = landed_at(t0());
        let r = recorded(Some(true), wake.clone());
        assert!(st.reads_record(&r), "{what}");
        // The wake's live frames, the record asked at the first: the face
        // never reads open (the review's flicker: a fresh landing logged at
        // every background wake).
        assert!(st.wake_may_keep_landing(), "{what}");
        st.took_quiet();
        assert_eq!(st.face(), TaskFace::Landed, "{what}: its first frame");
        assert!(!st.wake_may_keep_landing(), "{what}: asked once a turn");
        st.took();
        assert_eq!(st.face(), TaskFace::Landed, "{what}: a later frame");
        let (a, _) = until_acted(&mut st, &r, wake_at);
        assert_eq!(a, TurnEndAction::Nothing, "{what}");
        assert!(st.landed() && st.task_done(), "{what}");
        assert_eq!(st.face(), TaskFace::Landed, "{what}: its point");
        assert_eq!(
            decide_turn_end(&st, wake, &cfg(), wake_at + 24 * 60 * MIN),
            TurnEndAction::Nothing,
            "{what}: a day later"
        );
        // NEGATIVE CONTROLS.
        for (why, r) in [
            ("no record", wake.clone()),
            ("asked since", recorded(Some(false), wake.clone())),
        ] {
            let (mut st, _) = landed_at(t0());
            st.took();
            assert_eq!(st.face(), TaskFace::Open, "{what}: {why}: someone's turn");
            let (a, _) = until_acted(&mut st, &r, wake_at);
            assert!(!st.landed(), "{what}: {why}");
            assert!(
                matches!(a, TurnEndAction::Type { .. }),
                "{what}: {why}: {a:?}"
            );
        }
    }
}

/// A LANDED TASK'S OWN WAKE OVER OPEN WORK REOPENS IT (spec §9.6: under
/// Land the repository check gates the landing, whoever's turn it was): the
/// wake the record says nobody asked, ending over new uncommitted work in
/// the repository, or naming open work or a need in its words, is no longer
/// landed, and Land's nudge (`land-nudge@v1`) is typed; the face reads open
/// at its point. The recheck of d88a7bf289 found the record's landing kept
/// over such a wake. NEGATIVE CONTROL: the same wake over a clean
/// repository, its words naming nothing open, stays landed: nothing typed.
#[test]
fn a_landed_tasks_own_wake_over_open_work_gets_the_nudge() {
    let wake_at = t0() + 30 * MIN;
    let worked = Some(Duration::from_secs(20));
    let quiet = "The monitor fired: the deploy finished.";
    for (why, wake) in [
        (
            "a dirty repository",
            over_repo(land(idle(quiet, worked)), uncommitted_repo()),
        ),
        (
            "open words",
            land(idle(
                "The monitor fired: the deploy failed, and the fix is still open.",
                worked,
            )),
        ),
        (
            "a need",
            land(idle(
                "The monitor fired: the deploy needs a key.\nNEEDS YOU: the deploy key",
                worked,
            )),
        ),
        (
            "a need said earlier in the wake",
            land(TurnEndReading {
                turn_said: Some("NEEDS YOU: the deploy key".to_string()),
                ..idle(quiet, worked)
            }),
        ),
    ] {
        let (mut st, _) = landed_at(t0());
        let r = recorded(Some(true), wake);
        assert!(st.wake_may_keep_landing(), "{why}");
        st.took_quiet();
        let (a, _) = until_acted(&mut st, &r, wake_at);
        assert!(!st.landed() && !st.task_done(), "{why}: {a:?}");
        assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{why}: {a:?}");
        assert_eq!(st.face(), TaskFace::Open, "{why}");
    }
    // NEGATIVE CONTROL: a clean repository, nothing open.
    let (mut st, _) = landed_at(t0());
    let r = recorded(Some(true), land(idle(quiet, worked)));
    st.took_quiet();
    let (a, _) = until_acted(&mut st, &r, wake_at);
    assert_eq!(a, TurnEndAction::Nothing);
    assert!(st.landed() && st.task_done());
    assert_eq!(st.face(), TaskFace::Landed);
}

/// A LANDED TASK'S OWN WAKE OVER SOMEONE ELSE'S EDITS KEEPS THE LANDING
/// ([`TurnEndState::wake_began_over`]): the work the probe found as the wake
/// began, found unchanged at its point, is none of the wake's — someone
/// edited the checkout by hand after the landing — so the landing holds and
/// nothing is typed, as a restart with no turn run holds it
/// ([`TurnEndState::landing_words_read`]). The recheck of ea564676a3 found
/// the landing reopened there and the worker told to commit the person's
/// edits. NEGATIVE CONTROLS: work that appeared during the wake (the probe
/// clean as it began), and work the wake changed (a second file), reopen
/// it: Land's nudge; and a wake whose start was never probed is judged by
/// its point's probe alone, as before.
#[test]
fn a_landed_tasks_own_wake_over_someone_elses_edits_keeps_the_landing() {
    let wake_at = t0() + 30 * MIN;
    let quiet = "The monitor fired: the deploy finished.";
    let ended_over = |repo: RepoWork| {
        over_repo(
            recorded(Some(true), land(idle(quiet, Some(Duration::from_secs(20))))),
            repo,
        )
    };
    let theirs = uncommitted_repo();
    let (mut st, _) = landed_at(t0());
    st.took_quiet();
    st.wake_began_over(Some(theirs.clone()));
    let (a, _) = until_acted(&mut st, &ended_over(theirs.clone()), wake_at);
    assert_eq!(a, TurnEndAction::Nothing);
    assert!(st.landed() && st.task_done());
    assert_eq!(st.face(), TaskFace::Landed);
    // NEGATIVE CONTROLS.
    let more = RepoWork {
        changed: theirs.changed + 1,
        paths: [theirs.paths.clone(), vec!["src/wake.rs".to_string()]].concat(),
        ..theirs.clone()
    };
    for (why, began) in [
        ("appeared during the wake", Some(None)),
        ("changed by the wake", Some(Some(theirs.clone()))),
        ("never probed", None),
    ] {
        let (mut st, _) = landed_at(t0());
        st.took_quiet();
        if let Some(began) = began {
            st.wake_began_over(began);
        }
        let ended = if why == "changed by the wake" {
            more.clone()
        } else {
            theirs.clone()
        };
        let (a, _) = until_acted(&mut st, &ended_over(ended), wake_at);
        assert!(!st.landed(), "{why}: {a:?}");
        assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{why}: {a:?}");
    }
    // Someone's live turn is no quiet wake: its probe is never kept.
    let (mut st, _) = landed_at(t0());
    st.took();
    st.wake_began_over(Some(theirs.clone()));
    let r = TurnEndReading {
        finished_record: Some(false),
        ..ended_over(theirs)
    };
    let (a, _) = until_acted(&mut st, &r, wake_at);
    assert!(!st.landed(), "{a:?}");
}

/// A WAKE THAT REOPENS A LANDING OVER SOMEONE ELSE'S EDITS NAMES NONE OF THEM
/// (the merge of Autopilot Land's F1 with the wake's own probe): the wake began
/// over the person's edit and ends over the same one, but its words leave
/// work open, so the landing is over and Land's nudge is owed — for the
/// wake's words, never for the edit: the nudge carries no repository ask, so
/// the worker is not told to commit work it did not make. NEGATIVE CONTROL:
/// the probe clean as the wake began, the same finding at its point is the
/// wake's own, and the nudge names it (F1's repository suffix).
#[test]
fn a_wake_that_reopens_a_landing_over_someone_elses_edit_names_none_of_it() {
    let theirs = uncommitted_repo();
    let ask = "commit, merge and push it to the remote main first";
    let open = "The monitor fired: the deploy finished. The docs fix is parked for later.";
    for (why, began, named) in [
        ("over their edit", Some(theirs.clone()), false),
        ("work the wake made", None, true),
    ] {
        let (mut st, _) = landed_at(t0());
        st.took_quiet();
        st.wake_began_over(began);
        let r = over_repo(
            recorded(Some(true), land(idle(open, Some(Duration::from_secs(20))))),
            theirs.clone(),
        );
        let (a, _) = until_acted(&mut st, &r, t0() + 30 * MIN);
        assert!(!st.landed(), "{why}: {a:?}");
        let TurnEndAction::Type { text, rule_id } = &a else {
            panic!("{why}: {a:?}");
        };
        assert_eq!(*rule_id, RULE_LAND_NUDGE, "{why}: {a:?}");
        assert_eq!(text.contains(ask), named, "{why}: {text}");
    }
}

/// AN OFF AND BACK RE-ARMS LAND OVER THE REPOSITORY AS IT IS (a person's act;
/// the merge of Autopilot Land's F1/P6 with the restart's hold on a landing's
/// own words): once Off stood over a landing ([`TurnEndState::park_off`]),
/// its words read again are no reason to leave the repository unasked
/// ([`TurnEndState::landing_words_read`]) — this loop's landing or one the
/// journal seeded (a loop started as Off lifts) — and over the person's edit
/// the landing is over: Land's instruction again (the Off ended the
/// episode). NEGATIVE CONTROLS: with no Off (a restart, nobody's act) the
/// seeded landing holds over the same edit on its words alone, nothing
/// typed; after an Off over a clean repository the landing holds.
#[test]
fn an_off_and_back_re_arms_land_over_the_repository_as_it_is() {
    let mark = said_mark(Some(DEFENSE));
    let dirty = |r: TurnEndReading| over_repo(r, uncommitted_repo());
    // This loop's own landing.
    let (mut st, now) = landed_at(t0());
    assert!(
        st.landing_words_read(Some(DEFENSE)),
        "no Off: nothing to ask"
    );
    st.park_off();
    assert!(
        !st.landing_words_read(Some(DEFENSE)),
        "after an Off the repository is asked"
    );
    let (a, _) = until_acted(&mut st, &dirty(land(idle(DEFENSE, None))), now + 10 * MIN);
    assert_eq!(a, land_instruction());
    assert!(!st.landed() && !st.task_done());
    // A landing the journal seeded, the loop started as Off lifted.
    for off in [true, false] {
        let mut st = TurnEndState::default();
        st.seed_landed(mark);
        if off {
            st.park_off();
        }
        assert_eq!(st.landing_words_read(Some(DEFENSE)), !off, "off: {off}");
        let (a, _) = until_acted(&mut st, &dirty(land(idle(DEFENSE, None))), t0());
        if off {
            assert_eq!(a, land_instruction(), "after an Off");
            assert!(!st.landed(), "after an Off: {a:?}");
        } else {
            assert_eq!(a, TurnEndAction::Nothing, "a restart");
            assert!(st.landed(), "a restart: {a:?}");
        }
    }
    // NEGATIVE CONTROL: an Off over a clean repository keeps both.
    let (mut st, now) = landed_at(t0());
    st.park_off();
    let (a, _) = until_acted(&mut st, &land(idle(DEFENSE, None)), now + 10 * MIN);
    assert_eq!(a, TurnEndAction::Nothing);
    assert!(st.landed());
    let mut st = TurnEndState::default();
    st.seed_landed(mark);
    st.park_off();
    let (a, _) = until_acted(&mut st, &land(idle(DEFENSE, None)), t0());
    assert_eq!(a, TurnEndAction::Nothing);
    assert!(st.landed());
}

/// A SUCCESSOR WHOSE FIRST READ IS THE LANDED TASK'S OWN WAKE KEEPS THE
/// SEEDED LANDING (the recheck of ea564676a3): a loop restarted over a
/// landing (the journal's seed, [`TurnEndState::seed_landed`]) whose first
/// read is Claude Code's background-task wake, the record saying nobody
/// asked since: the wake is read as the task's own
/// ([`TurnEndState::wake_may_keep_landing`] holds for the armed seed too),
/// the seed is kept through its frames, the face stays landed, and the
/// wake's point — nothing open, the repository clean — takes the landing
/// up: nothing typed. The build before dropped the seed at the wake's
/// first frame and typed Land's act into the landed work. NEGATIVE
/// CONTROLS: someone's live turn (no quiet wake) drops the seed; the same
/// wake over a dirty repository, or one the record says someone asked,
/// lands nothing and gets Land's act.
#[test]
fn a_successors_first_read_of_the_landed_tasks_own_wake_keeps_the_seed() {
    let quiet = "The monitor fired: the deploy finished.";
    let worked = Some(Duration::from_secs(20));
    let seeded = || {
        let mut st = TurnEndState::default();
        st.seed_landed(said_mark(Some(DEFENSE)));
        st
    };
    let mut st = seeded();
    assert!(st.wake_may_keep_landing(), "the armed seed");
    st.took_quiet();
    assert_eq!(st.face(), TaskFace::Landed, "its first frame");
    st.took();
    assert_eq!(st.face(), TaskFace::Landed, "a later frame");
    let wake = recorded(Some(true), land(idle(quiet, worked)));
    let (a, then) = until_acted(&mut st, &wake, t0());
    assert_eq!(a, TurnEndAction::Nothing);
    assert!(st.landed() && st.task_done());
    assert_eq!(st.face(), TaskFace::Landed, "its point");
    let again = TurnEndReading {
        worked: None,
        ..wake.clone()
    };
    assert_eq!(
        at(&mut st, &again, then + 30 * MIN),
        TurnEndAction::Nothing,
        "read again"
    );
    // NEGATIVE CONTROLS.
    let mut st = seeded();
    st.took();
    assert_eq!(st.face(), TaskFace::Open, "someone's turn");
    for (why, r) in [
        (
            "a dirty repository",
            over_repo(
                recorded(Some(true), land(idle(quiet, worked))),
                uncommitted_repo(),
            ),
        ),
        (
            "asked since",
            recorded(Some(false), land(idle(quiet, worked))),
        ),
    ] {
        let mut st = seeded();
        st.took_quiet();
        let (a, _) = until_acted(&mut st, &r, t0());
        assert!(!st.landed() && !st.task_done(), "{why}: {a:?}");
        assert!(matches!(a, TurnEndAction::Type { .. }), "{why}: {a:?}");
    }
}

/// A HANDOFF AFTER AN UNTAKEN LAND ACT TYPES LAND'S ACT AGAIN
/// ([`TurnEndState::seed_land_act`]): a loop typed Land's instruction and
/// stopped before any read saw it taken; its successor's first point reads a
/// clean defense with no turn seen run and no sign the act was taken — no `❯`
/// row of the act over it, no record of its turn — so the defense predates
/// the act (said before Land was chosen), lands nothing, and the claim gets
/// Land's act again. The recheck of d88a7bf289 found the stale defense
/// landed in the successor, where the loop that typed the act would have
/// typed it again (`an_untaken_land_act_lands_nothing_on_the_defense_it_was_typed_over`).
/// NEGATIVE CONTROLS: the act's own `❯` row over the defense, the record's
/// last turn the act's and ended, and a turn the successor sees run, each
/// land it; a record turn of other words, or one recorded before the act,
/// does not.
#[test]
fn a_handoff_after_an_untaken_land_act_types_it_again() {
    let said = crate::supervise::approvals::words_hash(LAND);
    let lead = crate::supervise::approvals::words_lead(LAND);
    let typed_ago = 5 * MIN;
    let successor = |r: &TurnEndReading| {
        let mut st = TurnEndState::default();
        st.seed_land(0, true, false, false);
        st.seed_land_act(said, lead, typed_ago, t0());
        let (a, _) = until_acted(&mut st, r, t0());
        (st, a)
    };
    let stale = land(idle(DEFENSE, None));
    let (st, a) = successor(&stale);
    assert!(!st.landed() && !st.task_done(), "{a:?}");
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    for (why, r) in [
        (
            "a record turn of other words",
            TurnEndReading {
                record_turn: Some(RecordTurn {
                    prompt: crate::supervise::approvals::words_hash("Fix the parser."),
                    age: MIN,
                    ended: Some(2 * MIN),
                }),
                ..stale.clone()
            },
        ),
        (
            "the act's words recorded before it was typed",
            TurnEndReading {
                record_turn: Some(RecordTurn {
                    prompt: said,
                    age: 20 * MIN,
                    ended: Some(2 * MIN),
                }),
                ..stale.clone()
            },
        ),
    ] {
        let (st, a) = successor(&r);
        assert!(!st.landed(), "{why}: {a:?}");
    }
    // NEGATIVE CONTROLS: the act taken.
    for (why, r) in [
        (
            "its own ❯ row over the defense",
            TurnEndReading {
                answered: Some(ScreenAnswer {
                    prompt: said,
                    work: Some(3 * MIN),
                }),
                ..stale.clone()
            },
        ),
        (
            "the record's last turn is the act's",
            TurnEndReading {
                record_turn: Some(RecordTurn {
                    prompt: said,
                    age: 4 * MIN,
                    ended: Some(3 * MIN),
                }),
                ..stale.clone()
            },
        ),
        ("a turn seen run", land(idle(DEFENSE, Some(3 * MIN)))),
    ] {
        let (st, a) = successor(&r);
        assert_eq!(a, TurnEndAction::Nothing, "{why}");
        assert!(st.landed(), "{why}");
    }
}

/// A HANDOFF WHERE NOTHING CAN TELL WHETHER THE LAND ACT WAS TAKEN LANDS ITS
/// CLEAN DEFENSE (the recheck of ea564676a3): on Gemini CLI, agy and Muse
/// Code no reader reads the answered prompt over a reply and no transcript
/// record is read ([`TurnEndReading::answered`], `record_turn`: Claude Code's
/// alone) — and here no message that opened the turn was read either
/// ([`TurnEndReading::opened_by`]: where it is, it tells,
/// `an_untaken_land_act_is_told_by_the_message_that_opened_the_turn`) — and
/// a ledger row with no act (`seed_land_act` never called) has nothing to
/// compare. There the successor's first point lands the clean
/// defense, as the build before act-seen gating did: a land act's yield it
/// cannot read is judged by its words. Gated, each typed one more nudge at
/// every restart between the act's answer and its point. NEGATIVE CONTROL:
/// on Claude Code, whose screen and record can speak, the same seeded act
/// with neither saying it was taken still lands nothing.
#[test]
fn a_handoff_nothing_can_read_the_land_act_on_lands_its_defense() {
    let said = crate::supervise::approvals::words_hash(LAND);
    let lead = crate::supervise::approvals::words_lead(LAND);
    let successor = |program: aterm_phase::Program, act: bool| {
        let mut st = TurnEndState::default();
        st.seed_land(0, true, false, false);
        if act {
            st.seed_land_act(said, lead, 5 * MIN, t0());
        }
        let r = land(TurnEndReading {
            program,
            ..idle(DEFENSE, None)
        });
        let (a, _) = until_acted(&mut st, &r, t0());
        (st, a)
    };
    for program in [
        aterm_phase::Program::Gemini,
        aterm_phase::Program::Agy,
        aterm_phase::Program::Muse,
    ] {
        let (st, a) = successor(program, true);
        assert_eq!(a, TurnEndAction::Nothing, "{program:?}");
        assert!(st.landed() && st.task_done(), "{program:?}: {a:?}");
    }
    let (st, a) = successor(aterm_phase::Program::Claude, false);
    assert_eq!(a, TurnEndAction::Nothing, "no act seeded");
    assert!(st.landed(), "no act seeded: {a:?}");
    // NEGATIVE CONTROL: Claude Code's screen and record can speak.
    let (st, a) = successor(aterm_phase::Program::Claude, true);
    assert!(!st.landed(), "{a:?}");
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
}

/// A SEEDED LANDING HOLDS OVER SOMEONE ELSE'S EDITS UNTIL SOMEONE'S TURN
/// ([`TurnEndState::landing_words_read`]): after a loop restart, the
/// landing the journal seeded, its first point the landing's own words (the
/// mark the journal kept) with no turn run since, holds — nothing typed, the
/// record's landing standing — though the repository now holds work: no turn
/// of the worker's made it, so it is someone else's, and the loop asks the
/// repository nothing there. The recheck of d88a7bf289 found the landed
/// worker reopened and told to commit and push that work (`land@v1`).
/// Someone's next turn re-probes: over the same work, Land's nudge with the
/// repository's ask. NEGATIVE CONTROLS: other words at the first point (a
/// turn no loop watched) over the same work, and an older journal's seed
/// with no words to compare, hold nothing.
#[test]
fn a_seeded_landing_holds_over_someone_elses_edits_until_a_turn() {
    let mark = said_mark(Some(DEFENSE));
    let dirty = |r: TurnEndReading| over_repo(r, uncommitted_repo());
    let mut st = TurnEndState::default();
    st.seed_landed(mark);
    let r = dirty(land(idle(DEFENSE, None)));
    assert!(st.landing_words_read(r.said_tail.as_deref()));
    let a = at(&mut st, &r, t0());
    assert_eq!(a, TurnEndAction::Nothing);
    assert!(st.landed() && st.task_done());
    assert!(st.record_landing_holds(), "the record's landing stands");
    assert!(
        st.landing_words_read(r.said_tail.as_deref()),
        "read again: nothing to ask"
    );
    // Someone's next turn: the repository asked again.
    let turn = dirty(land(idle(DEFENSE, Some(3 * MIN))));
    assert!(!st.landing_words_read(Some(OTHER_DEFENSE)));
    let (a, _) = until_acted(&mut st, &turn, t0() + 10 * MIN);
    assert!(!st.landed(), "{a:?}");
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    let TurnEndAction::Type { text, .. } = &a else {
        panic!("{a:?}");
    };
    assert!(
        text.contains("commit, merge and push it to the remote main first"),
        "{text}"
    );
    // NEGATIVE CONTROLS.
    for (why, words, said) in [
        ("other words", mark, OTHER_DEFENSE),
        ("no words kept", None, DEFENSE),
    ] {
        let mut st = TurnEndState::default();
        st.seed_landed(words);
        let r = dirty(land(idle(said, None)));
        let (a, _) = until_acted(&mut st, &r, t0());
        assert!(!st.landed(), "{why}: {a:?}");
        assert!(matches!(a, TurnEndAction::Type { .. }), "{why}: {a:?}");
    }
}

/// A REPORT READ IN PART LANDS NOTHING: a message taller than the rows read
/// ([`TurnEndReading::said_cut`]: its head scrolled off, so a NEEDS YOU line
/// or open work above them is unread), and a NEEDS YOU line said in an
/// earlier message of the same turn ([`TurnEndReading::turn_said`]), are
/// no clean defense, though the last rows are one; the need said earlier
/// stands as the reply's own. NEGATIVE CONTROLS: the same defense read whole,
/// and an earlier message of the turn naming no need, land it.
#[test]
fn a_report_read_in_part_or_a_need_said_earlier_lands_nothing() {
    let after_instruction = |r: TurnEndReading| {
        let mut st = TurnEndState::default();
        assert_eq!(
            act(&mut st, &land(idle("Stage 1 landed.", Some(3 * MIN))), t0()),
            land_instruction()
        );
        let a = at(&mut st, &land(r), t0() + 3 * MIN);
        (st, a)
    };
    let cut = TurnEndReading {
        said_cut: true,
        ..idle(DEFENSE, Some(3 * MIN))
    };
    let (st, a) = after_instruction(cut);
    assert!(!st.landed(), "cut: {a:?}");
    let earlier = TurnEndReading {
        turn_said: Some("Merged the lexer.\nNEEDS YOU: the signing credential".to_string()),
        ..idle(DEFENSE, Some(3 * MIN))
    };
    let (st, a) = after_instruction(earlier);
    assert!(!st.landed(), "a need said earlier: {a:?}");
    assert_eq!(st.standing_needs(), ["the signing credential"]);
    // NEGATIVE CONTROLS.
    let (st, a) = after_instruction(idle(DEFENSE, Some(3 * MIN)));
    assert!(st.landed(), "whole: {a:?}");
    let plain = TurnEndReading {
        turn_said: Some("Merged the lexer and ran its suite.".to_string()),
        ..idle(DEFENSE, Some(3 * MIN))
    };
    let (st, a) = after_instruction(plain);
    assert!(st.landed(), "an earlier message with no need: {a:?}");
}

/// The rows the first Land reading kept as its own beside [`OPEN_STATUS`]
/// (the review of 2026-10-20): Land's strict reading by word family
/// ([`LAND_FAMILIES`]) reads each of them open too, wherever its family
/// holds it.
const FIRST_LAND_ROWS: &[&str] = &[
    "not landed",
    "unlanded",
    "not pushed",
    "unpushed",
    "not merged",
    "unmerged",
    "not committed",
    "uncommitted",
    "not on main",
    "not on origin",
    "not on the remote main",
    "parked",
    "wip",
    "work in progress",
    "left on a branch",
    "on its own branch",
    "for later",
    "later",
    "except",
    "blocked",
    "pending",
    "awaiting",
    "in review",
    "under review",
    "for review",
    "for your review",
    "for you to review",
    "to review",
    "still needs",
    "still need",
    "still has to",
    "still have to",
    "did not get to",
    "red",
    "separate task",
    "known issue",
    "known issues",
    "next session",
];

/// EVERY ROW OF [`LAND_STATUS`], AND OF THE FIRST LAND READING
/// ([`FIRST_LAND_ROWS`]), IS OPEN WORK UNDER LAND: a defense whose body
/// names it is no clean defense ([`open_work_strict`]), and the nudge names
/// it. NEGATIVE CONTROL: Cruise's reading ([`open_work`]) finds none of them
/// but its own rows ([`OPEN_STATUS`]: `follow-up`) — they are Land's alone
/// — and the same row negated is clean.
#[test]
fn every_land_status_row_is_open_under_land_and_not_under_cruise() {
    let said = |row: &str| -> String {
        match row {
            "except" => "all of it is merged except the docs".to_string(),
            "later" => "the docs come later".to_string(),
            "red" => "the e2e suite is red on main".to_string(),
            "separate task" => "the docs are a separate task".to_string(),
            "known issue" | "known issues" => format!("the flaky lexer test is a {row}"),
            "next session" => "the docs wait for the next session".to_string(),
            "still needs" | "still has to" => format!("the docs {row} be written"),
            "still need" | "still have to" => format!("the docs {row} be written"),
            "did not get to" => "I did not get to the docs".to_string(),
            "to review" => "the docs are up to review".to_string(),
            "awaiting" => "the docs are awaiting a reviewer".to_string(),
            "pending" => "the docs are pending".to_string(),
            "blocked" => "the docs are blocked on the key".to_string(),
            "wip" => "the WIP docs sit on a branch".to_string(),
            "parked" | "unlanded" | "unpushed" | "unmerged" | "uncommitted" => {
                format!("the docs are {row}")
            }
            "work in progress" => "the docs are a work in progress".to_string(),
            _ => format!("the docs are {row}"),
        }
    };
    let mut missed = Vec::new();
    for row in LAND_STATUS.iter().chain(FIRST_LAND_ROWS) {
        let reply = format!("Merged the parser.\nNothing left: {}.", said(row));
        let body = needs::defense_body(&reply);
        let strict = open_work_strict(Some(&body));
        if strict.is_empty() || clean_defense(Some(&reply)) {
            missed.push(format!("{row}: {reply}"));
        }
        if OPEN_STATUS.contains(row) {
            continue;
        }
        // NEGATIVE CONTROL: Cruise's reading.
        assert!(
            open_work(Some(&body)).is_empty(),
            "{row}: Cruise's reading found {:?} in {body}",
            open_work(Some(&body))
        );
    }
    assert!(missed.is_empty(), "{missed:#?}");
    // NEGATIVE CONTROL: negated, clean.
    for reply in [
        "Merged the parser.\nNothing left: nothing is parked or pending or left for later.",
        "Merged the parser.\nNothing left: no branch is unmerged and none is blocked.",
    ] {
        assert!(clean_defense(Some(reply)), "{reply}");
    }
}

/// A STATED REASON EXCUSES NO UNLANDED WORK UNDER LAND: `not merged,
/// because no reviewer…`, `there is no reviewer so … not merged`, `not
/// pushed: no remote` — the negation governs the reason, not the merge —
/// are open under Land's strict reading, as are the phrasings the review
/// found missed (`blocked`, `pending`, `awaiting`, `still has to`, `did
/// not get to`, `skipped`, `red`, `a separate task`, `left … for`). A thing
/// named unlanded and landed later in its clause (`the earlier WIP branch
/// was merged and deleted`) is landed. NEGATIVE CONTROLS: Cruise's reading
/// still takes such a reason (`not merged, because no reviewer`: a stated
/// blocker it reads only through its own rules), and `not merged` still
/// reads open after `merged` in another clause.
#[test]
fn land_reads_a_reason_as_no_excuse_and_a_landed_thing_as_landed() {
    for body in [
        "the docs branch is not merged, because no reviewer was available",
        "there is no reviewer so the docs branch is not merged",
        "the docs are not pushed: no remote is set",
        "the docs are blocked on the signing key",
        "the docs are pending a review",
        "the docs are awaiting a reviewer",
        "the docs still has to be written",
        "I didn't get to the docs",
        "the lexer's e2e suite is skipped",
        "CI is red on main",
        "the docs are a separate task",
        "the docs are left for a follow-up session",
    ] {
        let reply = format!("Merged the parser.\nNothing left: {body}.");
        assert!(
            !open_work_strict(Some(&needs::defense_body(&reply))).is_empty(),
            "{body}"
        );
        assert!(!clean_defense(Some(&reply)), "{body}");
    }
    for body in [
        "the earlier WIP branch was merged and deleted",
        "the parked lexer branch is now merged",
    ] {
        let reply = format!("Merged the parser.\nNothing left: {body}.");
        assert!(
            clean_defense(Some(&reply)),
            "{body}: {:?}",
            land_open_words(Some(&reply))
        );
    }
    // NEGATIVE CONTROLS.
    assert!(
        !open_work_strict(Some(
            "the WIP branch was merged, but the docs are not merged"
        ))
        .is_empty()
    );
    assert!(!open_work_strict(Some("the WIP branch is not merged")).is_empty());
}

/// EVERY ACT FITS WHAT THE COMPOSER DRAWS AS TYPED ([`VERBATIM_UNITS`]) with
/// all its suffixes at once — the open words (four named), the repository's
/// finding (160 units), the reversible clause, the needs ack — and the
/// standing rules: Cruise's check and Land's nudge, each with and without
/// rules. The findings give way (the repository's summary cut, then named no
/// more, then the open words fewer); the act's own words, the reversible
/// clause and the ack are typed whole. The review found the suffixes alone
/// past the bound: `[Pasted text #N]`, the fenced write failed, the act
/// escalated. NEGATIVE CONTROL: the suffixes joined whole overrun the bound.
#[test]
fn every_act_fits_the_verbatim_bound_with_all_its_suffixes() {
    let repo = RepoWork {
        repo: format!(
            "/Users//a/Developer/{}",
            "a-rather-long-checkout-name-".repeat(3)
        ),
        changed: 41,
        untracked: 17,
        stashes: 3,
        unpushed: 9,
        paths: (0..12)
            .map(|i| format!("crates/a-long-crate-name-{i}/src/a_rather_long_module_{i}.rs"))
            .collect(),
        ..Default::default()
    };
    let claim = "All done.\nStill open: the lexer.\nLeft: the docs.\nRemaining work: the e2e \
                 suite.\nNext steps: port the reader.\nTODO: wire the flag.\nNEEDS YOU: approve \
                 the force-push to main";
    let rules = "keep each commit small and name the stage it lands ".repeat(8);
    let rules = rules.trim_end();
    for (mode, base) in [
        (Autopilot::Cruise, DONE_CHECK),
        (Autopilot::Land, LAND_NUDGE),
    ] {
        for rules in [None, Some(rules)] {
            let mut st = TurnEndState::default();
            act(
                &mut st,
                &under(mode, idle("Stage 1 landed.", Some(3 * MIN))),
                t0(),
            );
            let r = over_repo(
                under(
                    mode,
                    TurnEndReading {
                        rules: rules.map(str::to_string),
                        ..idle(claim, Some(3 * MIN))
                    },
                ),
                repo.clone(),
            );
            let (a, _) = past_backoff(&mut st, &r, t0() + 3 * MIN);
            let TurnEndAction::Type { text, .. } = &a else {
                panic!("{mode:?}: {a:?}");
            };
            let units = text.encode_utf16().count();
            let what = format!("{mode:?} rules={}", rules.is_some());
            assert!(units <= VERBATIM_UNITS, "{what}: {units} units: {text}");
            assert!(text.starts_with(base), "{what}: {text}");
            for whole in [DONE_CHECK_REVERSIBLE, NEEDS_ACK] {
                assert!(text.contains(whole), "{what}: {whole:?} in {text}");
            }
            assert!(
                text.contains("commit, merge and push it to the remote main first"),
                "{what}: the repository's ask: {text}"
            );
        }
    }
    // NEGATIVE CONTROL: joined whole.
    let found = open_work(Some(&needs::without_needs(claim)));
    assert!(found.len() > OPEN_NAMED, "{found:?}");
    let whole = format!(
        "{DONE_CHECK} {} {} {DONE_CHECK_REVERSIBLE} {NEEDS_ACK}",
        open_work_suffix(&found).expect("open"),
        repo_work_suffix(&repo)
    );
    assert!(
        whole.encode_utf16().count() > VERBATIM_UNITS,
        "{} units",
        whole.encode_utf16().count()
    );
}

/// THE RETIRED DONE CHECKS ARE THE WORDS THEY WERE TYPED AS
/// ([`RETIRED_DONE_CHECKS`]): one space between words — a continuation line
/// that kept its indent inside the literal spelled a check no record holds
/// — and each read as a check ([`is_done_check`]) alone, with its fixed
/// reversible clause, and with the standing rules; main's check from
/// `d92d817dc` among them. NEGATIVE CONTROL: a check's words with a space
/// run in them are no check.
#[test]
fn the_retired_done_checks_read_as_typed() {
    for check in RETIRED_DONE_CHECKS {
        assert!(!check.contains("  "), "a space run: {check:?}");
        assert!(is_done_check(check), "{check}");
        assert!(
            is_done_check(&format!("{check} {DONE_CHECK_REVERSIBLE}")),
            "{check}"
        );
        assert!(
            is_done_check(&format!("{check} (standing rules: commit often)")),
            "{check}"
        );
        // NEGATIVE CONTROL.
        let spaced = check.replacen(' ', "      ", 1);
        assert!(!is_done_check(&spaced), "{spaced}");
    }
    let main = "If the task is finished, verified and has nothing left open, reply with \
                ATERM-TASK-DONE on a line by itself and stop; otherwise continue with what is \
                open.";
    assert!(RETIRED_DONE_CHECKS.contains(&main));
}

/// LAND'S NUDGE NAMES THE OPEN WORDS OF A DONE CLAIM THAT IS NO DEFENSE
/// ([`land_open_words`]: Land's strict reading over the claim, its NEEDS YOU
/// lines left out): `All done … still open: the docs` at the land act's
/// yield gets the nudge with the open-work suffix, and no landing. NEGATIVE
/// CONTROLS: a done claim naming no open work gets the plain nudge, and the
/// open words said only on a NEEDS YOU line name none.
#[test]
fn a_land_done_claim_gets_the_nudge_with_its_open_words() {
    let nudge_at = |reply: &str| {
        let mut st = TurnEndState::default();
        act(&mut st, &land(idle("Stage 1 landed.", Some(3 * MIN))), t0());
        let r = land(idle(reply, Some(3 * MIN)));
        let (a, _) = past_backoff(&mut st, &r, t0() + 3 * MIN);
        assert!(!st.landed(), "{reply}");
        a
    };
    let claim = "Merged the parser. All done.\nThe docs are still open.";
    assert!(!clean_defense(Some(claim)) && says_done(Some(claim)));
    let open = land_open_words(Some(claim));
    assert_eq!(open, ["still open"], "{claim}");
    assert_eq!(
        nudge_at(claim),
        TurnEndAction::Type {
            text: act_words(LAND_NUDGE, &[&open_work_suffix(&open).expect("open")], None),
            rule_id: RULE_LAND_NUDGE,
        }
    );
    // NEGATIVE CONTROLS.
    assert_eq!(nudge_at("Merged the parser. All done."), land_nudged());
    let needs_only = "Merged the parser. All done.\nNEEDS YOU: the docs' sign-off is still open";
    assert!(land_open_words(Some(needs_only)).is_empty());
    assert_eq!(
        nudge_at(needs_only),
        TurnEndAction::Type {
            text: act_words(LAND_NUDGE, &[NEEDS_ACK], None),
            rule_id: RULE_LAND_NUDGE,
        }
    );
}

/// CRUISE READS NO OPEN WORK OUT OF A NEEDS YOU LINE: a done claim whose
/// only open words stand on its NEEDS YOU line gets the check acked, with no
/// open-work suffix — the need is the person's, not work the worker left.
/// NEGATIVE CONTROL: the same words on a line of their own get the suffix.
#[test]
fn cruise_reads_no_open_work_out_of_a_needs_you_line() {
    let check_at = |reply: &str| {
        let mut st = TurnEndState::default();
        act(&mut st, &idle("Stage 1 landed.", Some(3 * MIN)), t0());
        past_backoff(&mut st, &idle(reply, Some(3 * MIN)), t0() + 3 * MIN).0
    };
    let needs_only = "Merged the parser. All done.\nNEEDS YOU: the docs' sign-off is still open";
    assert_eq!(
        check_at(needs_only),
        TurnEndAction::Type {
            text: act_words(DONE_CHECK, &[NEEDS_ACK], None),
            rule_id: RULE_DONE_CHECK,
        }
    );
    // NEGATIVE CONTROL.
    let plain = "Merged the parser. All done.\nThe docs' sign-off is still open.";
    let open = open_work(Some(plain));
    assert_eq!(open, ["still open"]);
    assert_eq!(
        check_at(plain),
        TurnEndAction::Type {
            text: act_words(DONE_CHECK, &[&open_work_suffix(&open).expect("open")], None),
            rule_id: RULE_DONE_CHECK,
        }
    );
}

/// A SEEDED LANDING'S OWN WORDS HOLD OVER A TURN READ CUT
/// ([`TurnEndState::landing_words_read`]): after a loop restart, the first
/// point's words the landing's own (the journal's mark), no turn since, read
/// with the turn's start out of reach ([`TurnEndReading::said_cut`]: no
/// history to read, a turn taller than the history is looked through) —
/// the landing holds, nothing typed. No turn ran since it was judged, so
/// nothing was said above those words the loop that landed did not read.
/// The regate of the b8 residual fixes found a real restart over a landed
/// Claude Code screen reopened (`open` told, the nudge owed) once a cut
/// turn's words landed nothing. NEGATIVE CONTROL: the same cut read with a
/// need in the turn's earlier words holds nothing.
#[test]
fn a_seeded_landings_own_words_hold_over_a_turn_read_cut() {
    let mut st = TurnEndState::default();
    st.seed_landed(said_mark(Some(DEFENSE)));
    let r = TurnEndReading {
        said_cut: true,
        ..land(idle(DEFENSE, None))
    };
    let a = at(&mut st, &r, t0());
    assert_eq!(a, TurnEndAction::Nothing);
    assert!(st.landed() && st.task_done(), "{a:?}");
    // NEGATIVE CONTROL: a need said earlier in the turn.
    let mut st = TurnEndState::default();
    st.seed_landed(said_mark(Some(DEFENSE)));
    let r = TurnEndReading {
        said_cut: true,
        turn_said: Some("NEEDS YOU: the signing credential".to_string()),
        ..land(idle(DEFENSE, None))
    };
    let (a, _) = until_acted(&mut st, &r, t0());
    assert!(!st.landed(), "{a:?}");
    assert!(matches!(a, TurnEndAction::Type { .. }), "{a:?}");
}

/// AN OFF AND BACK, THEN THE LANDED TASK'S OWN WAKE, KEEPS NO LANDING OVER
/// THE PERSON'S EDIT (`TurnEndState::observe`'s `off_stood`): Off is a
/// person's act, and the work the wake began over may be their own edit
/// made under Off — so the wake's start forgives nothing, and the landing
/// holds only over a clean repository, as an Off and back read idle holds
/// it (P6). The review of b8/residuals found the wake's tolerance of work it
/// began over (`wake_repo_kept`) keep the landing over the person's
/// uncommitted edit, live and for a loop started as the session's Off
/// lifted. Both now reopen and type Land's act. NEGATIVE CONTROLS: with no
/// Off (nobody's act) the wake over someone else's unchanged edit keeps the
/// landing, as before; and after an Off the same wake over a clean
/// repository keeps it.
#[test]
fn an_off_and_back_then_the_tasks_own_wake_keeps_no_landing_over_the_persons_edit() {
    let wake_at = t0() + 30 * MIN;
    let quiet = "The monitor fired: the deploy finished.";
    let theirs = uncommitted_repo();
    let wake = recorded(Some(true), land(idle(quiet, Some(Duration::from_secs(20)))));
    let over = over_repo(wake.clone(), theirs.clone());
    // Live landing, the loop's park; then a seeded landing, the loop started
    // as Off lifted.
    let live = || {
        let (mut st, _) = landed_at(t0());
        st.park_off();
        st
    };
    let seeded = || {
        let mut st = TurnEndState::default();
        st.seed_landed(said_mark(Some(DEFENSE)));
        st.park_off();
        st
    };
    for (why, make) in [
        ("live", &live as &dyn Fn() -> TurnEndState),
        ("seeded", &seeded),
    ] {
        let mut st = make();
        st.took_quiet();
        st.wake_began_over(Some(theirs.clone()));
        let (a, _) = until_acted(&mut st, &over, wake_at);
        assert!(!st.landed() && !st.task_done(), "{why}: {a:?}");
        assert!(matches!(a, TurnEndAction::Type { .. }), "{why}: {a:?}");
        // NEGATIVE CONTROL: a clean repository after the Off.
        let mut st = make();
        st.took_quiet();
        st.wake_began_over(None);
        let (a, _) = until_acted(&mut st, &wake, wake_at);
        assert!(st.landed(), "{why}, clean: {a:?}");
        assert_eq!(a, TurnEndAction::Nothing, "{why}, clean");
    }
    // NEGATIVE CONTROL: no Off — the edit someone else's, unchanged by the wake.
    let (mut st, _) = landed_at(t0());
    st.took_quiet();
    st.wake_began_over(Some(theirs.clone()));
    let (a, _) = until_acted(&mut st, &over, wake_at);
    assert!(st.landed(), "no Off: {a:?}");
    assert_eq!(a, TurnEndAction::Nothing, "no Off");
}

/// A SUCCESSOR THAT ATTACHES MID-WAKE TAKES NO PROBE AS THE WAKE'S START
/// ([`TurnEndState::wake_start_owed`]): its first read is the wake running,
/// no point of its own judged, so it never saw the wake begin — what the
/// probe finds then may already be the wake's work. The review of
/// b8/residuals found it taken as the wake's start: the wake's own edit
/// read as someone else's, and the seeded landing kept over it. Now no
/// start is owed: the work found at the wake's end reopens the landing.
/// NEGATIVE CONTROL: a live landing's wake (its loop judged the landing's
/// point) owes its start, and a wake over work it did not change keeps it.
#[test]
fn a_successor_attached_mid_wake_takes_no_probe_as_the_wakes_start() {
    let quiet = "The monitor fired: the deploy finished.";
    let found = uncommitted_repo();
    let over = over_repo(
        recorded(Some(true), land(idle(quiet, Some(Duration::from_secs(20))))),
        found.clone(),
    );
    let mut st = TurnEndState::default();
    st.seed_landed(said_mark(Some(DEFENSE)));
    st.took_quiet();
    assert!(!st.wake_start_owed());
    st.wake_began_over(Some(found.clone()));
    let (a, _) = until_acted(&mut st, &over, t0());
    assert!(!st.landed(), "{a:?}");
    assert!(matches!(a, TurnEndAction::Type { .. }), "{a:?}");
    // NEGATIVE CONTROL: a live landing's wake.
    let (mut st, now) = landed_at(t0());
    st.took_quiet();
    assert!(st.wake_start_owed());
    st.wake_began_over(Some(found.clone()));
    assert!(!st.wake_start_owed(), "taken once");
    let (a, _) = until_acted(&mut st, &over, now + 30 * MIN);
    assert!(st.landed(), "{a:?}");
}

/// A WAKE'S EDIT TO A PATH ALREADY CHANGED REOPENS THE LANDING
/// ([`crate::harness::repo_work::RepoWork::same`]: the work's state, not its
/// counts and first paths): someone else's edit stood when the wake began,
/// and the wake edited the same file again — the counts and the paths read
/// the same, the work's state does not. The review of b8/residuals found
/// the two compared equal on counts and three paths, so the wake's edit was
/// forgiven as someone else's and the landing kept over it. NEGATIVE
/// CONTROLS: the work's state unchanged keeps it; a state the probe could
/// not read (too many files) forgives nothing.
#[test]
fn a_wakes_edit_to_a_path_already_changed_reopens_the_landing() {
    let quiet = "The monitor fired: the deploy finished.";
    let began = uncommitted_repo();
    let wake_at = t0() + 30 * MIN;
    let go = |at_start: crate::harness::repo_work::RepoWork,
              at_end: crate::harness::repo_work::RepoWork| {
        let (mut st, _) = landed_at(t0());
        st.took_quiet();
        st.wake_began_over(Some(at_start));
        let r = over_repo(
            recorded(Some(true), land(idle(quiet, Some(Duration::from_secs(20))))),
            at_end,
        );
        let (a, _) = until_acted(&mut st, &r, wake_at);
        (st.landed(), a)
    };
    let edited = crate::harness::repo_work::RepoWork {
        state: Some(0xed17),
        ..began.clone()
    };
    let (landed, a) = go(began.clone(), edited);
    assert!(!landed, "{a:?}");
    assert!(matches!(a, TurnEndAction::Type { .. }), "{a:?}");
    // NEGATIVE CONTROLS.
    let (landed, a) = go(began.clone(), began.clone());
    assert!(landed, "{a:?}");
    let unread = crate::harness::repo_work::RepoWork {
        state: None,
        ..began.clone()
    };
    let (landed, a) = go(unread.clone(), unread);
    assert!(!landed, "{a:?}");
}

/// A DEFENSE WITH A NONE LINE IN COMMON WORDS LANDS, NO NEED STANDING (the
/// review of b8/residuals): `NEEDS YOU: no action needed from you.`,
/// `nothing at this point.`, `nothing to do.`, `nothing from my side.`, a
/// dash alone — each beside a clean defense at the land act's yield read as
/// a need: the nudge typed, and a phantom need on the person's screen. Now
/// each lands with no need standing. NEGATIVE CONTROL: a none that asks for
/// something after all (`nothing. Please approve the deploy.`) is a need: no
/// landing, the need standing.
#[test]
fn a_defense_with_a_none_line_in_common_words_lands() {
    let yield_of = |none: &str| {
        let defense = format!("{DEFENSE}\n{none}");
        let mut st = TurnEndState::default();
        assert_eq!(
            act(&mut st, &land(idle("Stage 1 landed.", Some(3 * MIN))), t0()),
            land_instruction()
        );
        let a = at(
            &mut st,
            &land(idle(&defense, Some(3 * MIN))),
            t0() + 3 * MIN,
        );
        (st, a)
    };
    for none in [
        "NEEDS YOU: no action needed from you.",
        "NEEDS YOU: nothing at this point.",
        "NEEDS YOU: nothing to do.",
        "NEEDS YOU: nothing from my side.",
        "NEEDS YOU: —",
    ] {
        let (st, a) = yield_of(none);
        assert_eq!(a, TurnEndAction::Nothing, "{none:?}");
        assert!(st.landed(), "{none:?}");
        assert!(
            st.standing_needs().is_empty(),
            "{none:?}: {:?}",
            st.standing_needs()
        );
    }
    // NEGATIVE CONTROL.
    let (st, a) = yield_of("NEEDS YOU: nothing. Please approve the deploy.");
    assert!(!st.landed(), "{a:?}");
    assert_eq!(st.standing_needs().len(), 1, "{:?}", st.standing_needs());
}

/// AN UNTAKEN LAND ACT IS TOLD BY THE MESSAGE THAT OPENED THE TURN on Gemini
/// CLI, agy and Muse Code ([`TurnEndReading::opened_by`],
/// [`TurnEndState::seed_land_act`]): a loop typed Land's instruction and
/// stopped before any read saw it taken; its successor's first point reads a
/// clean defense with no turn seen run, under the message of the person's
/// that opened that turn — the defense predates the act, lands nothing, and
/// the claim gets Land's act again. The review of b8/residuals found the
/// stale defense landed on each of the three (nothing could say there, so
/// the act counted as taken). NEGATIVE CONTROLS: the act's own message over
/// the defense — whole, or only its first words where the band or the pane
/// cut it — lands it, as on Claude Code its `❯` row does.
#[test]
fn an_untaken_land_act_is_told_by_the_message_that_opened_the_turn() {
    use crate::supervise::approvals::{LEAD_WORDS, words_hash, words_lead};
    let said = words_hash(LAND);
    let lead = words_lead(LAND);
    let first: Vec<&str> = LAND.split_whitespace().take(LEAD_WORDS).collect();
    let first = first.join(" ");
    for program in [
        aterm_phase::Program::Gemini,
        aterm_phase::Program::Agy,
        aterm_phase::Program::Muse,
    ] {
        let successor = |opened: &str| {
            let mut st = TurnEndState::default();
            st.seed_land(0, true, false, false);
            st.seed_land_act(said, lead, 5 * MIN, t0());
            let r = TurnEndReading {
                program,
                opened_by: Some(words_lead(opened)),
                ..land(idle(DEFENSE, None))
            };
            let (a, _) = until_acted(&mut st, &r, t0());
            (st, a)
        };
        let (st, a) = successor("Fix the parser and push it to main.");
        assert!(!st.landed() && !st.task_done(), "{program:?}: {a:?}");
        assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{program:?}: {a:?}");
        // NEGATIVE CONTROLS: the act's own message.
        for opened in [LAND, first.as_str()] {
            let (st, a) = successor(opened);
            assert_eq!(a, TurnEndAction::Nothing, "{program:?}: {opened:?}");
            assert!(st.landed() && st.task_done(), "{program:?}: {opened:?}");
        }
    }
}

/// THE LANDED TASK'S OWN WAKE READ UNDER OFF KEEPS THE LANDING FOR THE PARK
/// (`TurnEndState::observe`'s `record_says`): the loop reads the session's
/// word fresh at a wake's end over a landing (`Session::turn_end_at_point`),
/// so an Off chosen on the landed screen is read there — and Off takes the
/// landing to its park ([`TurnEndState::park_off`]), judged again as Off
/// lifts, never drops it at the wake: dropped there, the lift typed Land's
/// instruction into a clean landing. NEGATIVE CONTROL: under Cruise, which
/// takes nothing as the end, the same wake reopens the task.
#[test]
fn the_tasks_own_wake_read_under_off_keeps_the_landing_for_the_park() {
    let quiet = "The monitor fired: the deploy finished.";
    let wake = |mode: Autopilot| {
        let (mut st, now) = landed_at(t0());
        st.took_quiet();
        st.wake_began_over(None);
        let r = under(
            mode,
            recorded(Some(true), idle(quiet, Some(Duration::from_secs(20)))),
        );
        st.observe(&r, now + 30 * MIN);
        st
    };
    let st = wake(Autopilot::Off);
    assert!(st.landed() && st.task_done(), "{st:?}");
    // NEGATIVE CONTROL: Cruise.
    let st = wake(Autopilot::Cruise);
    assert!(!st.landed(), "{st:?}");
}
