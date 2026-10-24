// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::harness::repo_work::RepoWork;

const OTHER_DEFENSE: &str = "Merged the docs and pushed main.\nNothing left: all of it is merged into the remote main branch.";

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
        let mut st = TurnEndState::default();
        let then = start(&mut st);
        let a = at(&mut st, &land(idle(DEFENSE, Some(2 * MIN))), then + 2 * MIN);
        assert_eq!(a, TurnEndAction::Nothing, "{what}");
        assert!(st.landed(), "{what}");
    }
}

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
    let (mut st, now) = landed_at(t0());
    st.park_off();
    let r = land(idle(DEFENSE, None));
    assert!(!st.reads_record(&r), "its own words: nothing to read");
    let a = at(&mut st, &r, now + 10 * MIN);
    assert_eq!(a, TurnEndAction::Nothing);
    assert!(st.landed());
}

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
    let (mut st, _) = landed_at(t0());
    let r = recorded(Some(true), land(idle(quiet, worked)));
    st.took_quiet();
    let (a, _) = until_acted(&mut st, &r, wake_at);
    assert_eq!(a, TurnEndAction::Nothing);
    assert!(st.landed() && st.task_done());
    assert_eq!(st.face(), TaskFace::Landed);
}

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
    st.wake_began_over(crate::harness::repo_work::RepoRead::Found(theirs.clone()));
    let (a, _) = until_acted(&mut st, &ended_over(theirs.clone()), wake_at);
    assert_eq!(a, TurnEndAction::Nothing);
    assert!(st.landed() && st.task_done());
    assert_eq!(st.face(), TaskFace::Landed);
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
            st.wake_began_over(crate::harness::repo_work::RepoRead::of_found(began));
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
    let (mut st, _) = landed_at(t0());
    st.took();
    st.wake_began_over(crate::harness::repo_work::RepoRead::Found(theirs.clone()));
    let r = TurnEndReading {
        finished_record: Some(false),
        ..ended_over(theirs)
    };
    let (a, _) = until_acted(&mut st, &r, wake_at);
    assert!(!st.landed(), "{a:?}");
}

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
        st.wake_began_over(crate::harness::repo_work::RepoRead::of_found(began));
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

#[test]
fn an_off_and_back_re_arms_land_over_the_repository_as_it_is() {
    let mark = said_mark(Some(DEFENSE));
    let dirty = |r: TurnEndReading| over_repo(r, uncommitted_repo());
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
    let (st, a) = successor(aterm_phase::Program::Claude, true);
    assert!(!st.landed(), "{a:?}");
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
}

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
    let (st, a) = after_instruction(idle(DEFENSE, Some(3 * MIN)));
    assert!(st.landed(), "whole: {a:?}");
    let plain = TurnEndReading {
        turn_said: Some("Merged the lexer and ran its suite.".to_string()),
        ..idle(DEFENSE, Some(3 * MIN))
    };
    let (st, a) = after_instruction(plain);
    assert!(st.landed(), "an earlier message with no need: {a:?}");
}

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
        assert!(
            open_work(Some(&body)).is_empty(),
            "{row}: Cruise's reading found {:?} in {body}",
            open_work(Some(&body))
        );
    }
    assert!(missed.is_empty(), "{missed:#?}");
    for reply in [
        "Merged the parser.\nNothing left: nothing is parked or pending or left for later.",
        "Merged the parser.\nNothing left: no branch is unmerged and none is blocked.",
    ] {
        assert!(clean_defense(Some(reply)), "{reply}");
    }
}

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
    assert!(
        !open_work_strict(Some(
            "the WIP branch was merged, but the docs are not merged"
        ))
        .is_empty()
    );
    assert!(!open_work_strict(Some("the WIP branch is not merged")).is_empty());
}

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
        let spaced = check.replacen(' ', "      ", 1);
        assert!(!is_done_check(&spaced), "{spaced}");
    }
    let main = "If the task is finished, verified and has nothing left open, reply with \
                ATERM-TASK-DONE on a line by itself and stop; otherwise continue with what is \
                open.";
    assert!(RETIRED_DONE_CHECKS.contains(&main));
}

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

#[test]
fn an_off_and_back_then_the_tasks_own_wake_keeps_no_landing_over_the_persons_edit() {
    let wake_at = t0() + 30 * MIN;
    let quiet = "The monitor fired: the deploy finished.";
    let theirs = uncommitted_repo();
    let wake = recorded(Some(true), land(idle(quiet, Some(Duration::from_secs(20)))));
    let over = over_repo(wake.clone(), theirs.clone());
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
        st.wake_began_over(crate::harness::repo_work::RepoRead::Found(theirs.clone()));
        let (a, _) = until_acted(&mut st, &over, wake_at);
        assert!(!st.landed() && !st.task_done(), "{why}: {a:?}");
        assert!(matches!(a, TurnEndAction::Type { .. }), "{why}: {a:?}");
        let mut st = make();
        st.took_quiet();
        st.wake_began_over(crate::harness::repo_work::RepoRead::Clean);
        let (a, _) = until_acted(&mut st, &wake, wake_at);
        assert!(st.landed(), "{why}, clean: {a:?}");
        assert_eq!(a, TurnEndAction::Nothing, "{why}, clean");
    }
    let (mut st, _) = landed_at(t0());
    st.took_quiet();
    st.wake_began_over(crate::harness::repo_work::RepoRead::Found(theirs.clone()));
    let (a, _) = until_acted(&mut st, &over, wake_at);
    assert!(st.landed(), "no Off: {a:?}");
    assert_eq!(a, TurnEndAction::Nothing, "no Off");
}

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
    st.wake_began_over(crate::harness::repo_work::RepoRead::Found(found.clone()));
    let (a, _) = until_acted(&mut st, &over, t0());
    assert!(!st.landed(), "{a:?}");
    assert!(matches!(a, TurnEndAction::Type { .. }), "{a:?}");
    let (mut st, now) = landed_at(t0());
    st.took_quiet();
    assert!(st.wake_start_owed());
    st.wake_began_over(crate::harness::repo_work::RepoRead::Found(found.clone()));
    assert!(!st.wake_start_owed(), "taken once");
    let (a, _) = until_acted(&mut st, &over, now + 30 * MIN);
    assert!(st.landed(), "{a:?}");
}

#[test]
fn a_wakes_edit_to_a_path_already_changed_reopens_the_landing() {
    let quiet = "The monitor fired: the deploy finished.";
    let began = uncommitted_repo();
    let wake_at = t0() + 30 * MIN;
    let go = |at_start: crate::harness::repo_work::RepoWork,
              at_end: crate::harness::repo_work::RepoWork| {
        let (mut st, _) = landed_at(t0());
        st.took_quiet();
        st.wake_began_over(crate::harness::repo_work::RepoRead::Found(at_start));
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
    let (landed, a) = go(began.clone(), began.clone());
    assert!(landed, "{a:?}");
    let unread = crate::harness::repo_work::RepoWork {
        state: None,
        ..began.clone()
    };
    let (landed, a) = go(unread.clone(), unread);
    assert!(!landed, "{a:?}");
}

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
    let (st, a) = yield_of("NEEDS YOU: nothing. Please approve the deploy.");
    assert!(!st.landed(), "{a:?}");
    assert_eq!(st.standing_needs().len(), 1, "{:?}", st.standing_needs());
}

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
        for opened in [LAND, first.as_str()] {
            let (st, a) = successor(opened);
            assert_eq!(a, TurnEndAction::Nothing, "{program:?}: {opened:?}");
            assert!(st.landed() && st.task_done(), "{program:?}: {opened:?}");
        }
    }
}

#[test]
fn the_tasks_own_wake_read_under_off_keeps_the_landing_for_the_park() {
    let quiet = "The monitor fired: the deploy finished.";
    let wake = |mode: Autopilot| {
        let (mut st, now) = landed_at(t0());
        st.took_quiet();
        st.wake_began_over(crate::harness::repo_work::RepoRead::Clean);
        let r = under(
            mode,
            recorded(Some(true), idle(quiet, Some(Duration::from_secs(20)))),
        );
        st.observe(&r, now + 30 * MIN);
        st
    };
    let st = wake(Autopilot::Off);
    assert!(st.landed() && st.task_done(), "{st:?}");
    let st = wake(Autopilot::Cruise);
    assert!(!st.landed(), "{st:?}");
}
