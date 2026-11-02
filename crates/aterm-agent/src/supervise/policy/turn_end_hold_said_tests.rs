// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE HOLD QUOTES THE AGENT, AND LAND KEEPS RESOLVING (§14.17 item 1, slice 4b): the hold's
// words are the agent's own list and the files git found, an item reworded refreshes them, and
// Land's prompt after a list asks the agent to resolve what it can before Land holds on the same
// answer, quoted. The slice's As built note names the mutant each test was seen failing under.

use crate::harness::acts::HoldLeg;
use crate::harness::hold_said::HoldSaid;
use crate::harness::repo_work::{RepoRead, RepoWork};
use crate::supervise::policy::land_check;

/// L2's defense: clean words, a leftover beside them (the forever lab's `open_defense`).
const OPEN_DEFENSE: &str = "Nothing left: the parser is committed and pushed to origin/main.\n\
                            The docs PR is still open.";

/// The agent's list, answering the check.
const LIST: &str = "The docs PR is still open.";

/// [`LIST`] reworded, its word family the same (`still open`).
const REWORDED: &str = "The pull request for the docs is still open.";

/// Land on, its instruction typed at `t`: the state and when the instruction's turn ends.
fn instructed(t: Instant) -> (TurnEndState, Instant) {
    let mut st = TurnEndState::default();
    assert_eq!(
        act(&mut st, &land(idle("Stage 1 landed.", Some(3 * MIN))), t),
        land_instruction()
    );
    (st, t + 3 * MIN)
}

/// A short answer `said`, no tools run: an act's no-work answer.
fn short(said: &str) -> TurnEndReading {
    TurnEndReading {
        ran_tools: false,
        ..land(idle(said, Some(Duration::from_secs(10))))
    }
}

/// The decision at `r` at `now`, a wait waited out on the same point read again.
fn decided(st: &mut TurnEndState, r: &TurnEndReading, now: Instant) -> (TurnEndAction, Instant) {
    let mut now = now;
    let mut a = at(st, r, now);
    let same = TurnEndReading {
        worked: None,
        ..r.clone()
    };
    for _ in 0..10 {
        let TurnEndAction::WaitUntil { until, .. } = a else {
            break;
        };
        now = until;
        a = at(st, &same, now);
    }
    (a, now)
}

/// The text `a` types.
fn typed(a: &TurnEndAction) -> &str {
    match a {
        TurnEndAction::Type { text, .. } => text,
        other => panic!("nothing typed: {other:?}"),
    }
}

/// Land's instruction answered by L2's defense, the check it owes answered by `list`: the nudge
/// typed at that answer (acted), and when.
fn listed(list: &str) -> (TurnEndState, TurnEndAction, Instant) {
    let (mut st, now) = instructed(t0());
    let r = land(idle(OPEN_DEFENSE, Some(3 * MIN)));
    let (a, now) = decided(&mut st, &r, now);
    assert_eq!(a.rule_id(), Some(RULE_LAND_CHECK), "the check: {a:?}");
    st.acted(&a, &r, now);
    let answer = short(list);
    let (b, now) = decided(&mut st, &answer, now + MIN);
    assert_eq!(b.rule_id(), Some(RULE_LAND_NUDGE), "the nudge: {b:?}");
    st.acted(&b, &answer, now);
    (st, b, now)
}

/// `replies` answered one by one, each a minute after the last: the last decision.
fn answered(st: &mut TurnEndState, now: Instant, replies: &[&str]) -> TurnEndAction {
    let mut now = now;
    let mut last = TurnEndAction::Nothing;
    for said in replies {
        let r = short(said);
        let (a, then) = decided(st, &r, now + MIN);
        if matches!(a, TurnEndAction::Type { .. }) {
            st.acted(&a, &r, then);
        }
        now = then;
        last = a;
    }
    last
}

/// LAND KEEPS RESOLVING, THEN HOLDS ON THE SAME ANSWER, QUOTED: the nudge quoting the agent's
/// list asks it to resolve each issue it can and name only what a person must give; the list
/// said again stands, so Land holds, and the hold's Open leg and quote are that answer.
/// Negative controls: the standing list left out of a later yield's legs fails the hold's Open
/// leg and its quote; the resolve sentence left out of the quoting suffix fails the nudge.
#[test]
fn land_keeps_resolving_then_holds_on_the_same_answer_quoted() {
    let (mut st, b, now) = listed(LIST);
    let text = typed(&b);
    assert!(text.contains(land_check::LISTED_RESOLVE), "{text}");
    assert!(
        text.contains(&land_check::listed_suffix(LIST).expect("quoted")),
        "{text}"
    );
    let c = answered(&mut st, now, &[LIST]);
    assert!(!matches!(c, TurnEndAction::Type { .. }), "it holds: {c:?}");
    let held = st.land_held().expect("held");
    assert!(held.holds(HoldLeg::Open), "the list stands: {held:?}");
    assert!(
        !held.holds(HoldLeg::Defense),
        "its standing list is its answer, no defense owed: {held:?}"
    );
    assert_eq!(
        st.land_said().map(|s| s.open.clone()),
        Some(vec![LIST.to_string()]),
        "the hold quotes that answer"
    );
}

/// A LIST REWORDED IN A REPLY TO THE NUDGE IS THE AGENT'S NEWEST ANSWER: answered with the list
/// in other words and no work, Land's next nudge quotes those words, never the check's, and the
/// same words again hold, quoted (§14.17 item 1: the hold's quote is that answer).
/// Negative control: the list kept as the check's answer only (no `land_relisted`) fails both.
#[test]
fn a_list_reworded_in_a_reply_to_the_nudge_is_the_newest_answer() {
    let (mut st, _, now) = listed(LIST);
    let r = short(REWORDED);
    let (a, now) = decided(&mut st, &r, now + MIN);
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    let text = typed(&a);
    assert!(
        text.contains(&land_check::listed_suffix(REWORDED).expect("quoted")),
        "{text}"
    );
    assert!(!text.contains(LIST), "the check's old words: {text}");
    st.acted(&a, &r, now);
    let c = answered(&mut st, now, &[REWORDED]);
    assert!(!matches!(c, TurnEndAction::Type { .. }), "it holds: {c:?}");
    assert!(st.land_held().is_some_and(|h| h.holds(HoldLeg::Open)));
    assert_eq!(
        st.land_said().map(|s| s.open.clone()),
        Some(vec![REWORDED.to_string()]),
        "the hold quotes the newest answer"
    );
}

/// LAND ASKS TO RESOLVE EVEN WHERE THE QUOTE HAS NO ROOM: the resolve sentence is a fixed part of
/// the nudge while a list stands, so the 800-unit fit that drops the quote for the repository's
/// ask never drops it.
/// Negative control: the sentence typed only inside the quote fails where the fit drops it.
#[test]
fn the_resolve_sentence_rides_though_the_quote_is_dropped() {
    let work = RepoWork {
        repo: "/Users//a/Developer/parser".to_string(),
        changed: 3,
        untracked: 2,
        paths: vec![
            "src/a_very_long_module_name_for_the_parser.rs".to_string(),
            "src/another_long_module_name_for_the_lexer.rs".to_string(),
            "docs/a_long_design_note_about_the_grammar.md".to_string(),
        ],
        main_ref: "origin/main".to_string(),
        ..RepoWork::default()
    };
    let repo = RepoRead::Found(work);
    let list = "The docs PR is still open and needs a review from the docs owners before it \
                can merge.\nThe version bump for the parser crate is not pushed yet.";
    let mut dropped = 0;
    for filler in (0..800).step_by(4) {
        let fill = "x".repeat(filler);
        let (text, _) = fit_act(
            LAND_NUDGE,
            &[
                Suffix::Listed(Some(list)),
                Suffix::Repo(&repo, RepoAsk::All),
                Suffix::Fixed(&fill),
            ],
            None,
        );
        assert!(
            text.contains(land_check::LISTED_RESOLVE),
            "filler {filler}: {text}"
        );
        if !text.contains(land_check::LISTED_OPEN) {
            dropped += 1;
        }
    }
    assert!(
        dropped > 0,
        "no fit dropped the quote: the case is not covered"
    );
}

/// A DEFENSE SAID AGAIN HOLDS ON THE LIST IT STANDS BY (the forever lab's L2): the nudge quoting
/// the list answered by the defense twice holds, on the list, quoted, as no work came between.
/// Negative control: the standing list left out of a later yield's legs fails the Open leg.
#[test]
fn a_defense_said_again_holds_on_the_list_it_stands_by() {
    let (mut st, _, now) = listed(LIST);
    let c = answered(&mut st, now, &[OPEN_DEFENSE, OPEN_DEFENSE]);
    assert!(!matches!(c, TurnEndAction::Type { .. }), "it holds: {c:?}");
    let held = st.land_held().expect("held");
    assert!(held.holds(HoldLeg::Open), "{held:?}");
    assert_eq!(
        st.land_said().map(|s| s.open.clone()),
        Some(vec![LIST.to_string()])
    );
}

/// REAL WORK TAKES THE LIST DOWN: a turn past the work floor is new work, so the list no longer
/// stands, and the hold neither holds its Open leg nor quotes it.
/// Negative control: the list kept past real work fails both.
#[test]
fn real_work_takes_the_list_down() {
    let (mut st, _, now) = listed(LIST);
    let r = land(idle("Rebased the docs branch onto main.", Some(3 * MIN)));
    let (a, then) = decided(&mut st, &r, now + 3 * MIN);
    st.acted(&a, &r, then);
    let held = st.land_held().expect("held");
    assert!(!held.holds(HoldLeg::Open), "{held:?}");
    assert_eq!(st.land_listed(), None);
    assert!(st.land_said().is_none(), "{:?}", st.land_said());
}

/// A REWORDED ITEM REFRESHES THE QUOTE: two holds whose agents listed the same leftover in other
/// words read the same [`LandHeld`] (its word family, its legs and counts), so a loop told the
/// first is told the second's words though not its hold again.
/// Negative control: the words told only where the hold moved fails the refresh.
#[test]
fn a_reworded_item_refreshes_the_quote() {
    let hold = |list: &str| {
        let (mut st, _, now) = listed(list);
        let c = answered(&mut st, now, &[list]);
        assert!(!matches!(c, TurnEndAction::Type { .. }), "it holds: {c:?}");
        st
    };
    let mut first = hold(LIST);
    let mut reworded = hold(REWORDED);
    assert_eq!(
        first.land_held(),
        reworded.land_held(),
        "the hold alone cannot tell the two apart"
    );
    assert!(first.held_moved(true).is_some());
    assert_eq!(
        first.said_moved(true),
        Some(Some(HoldSaid::of(Some(LIST), &[])))
    );
    assert_eq!(first.said_moved(true), None, "told once per move");
    // The loop that told the first hold reads the reworded one.
    reworded.held_told = first.held_told;
    reworded.said_told = first.said_told.clone();
    assert_eq!(reworded.held_moved(true), None, "the hold did not move");
    assert_eq!(
        reworded.said_moved(true),
        Some(Some(HoldSaid::of(Some(REWORDED), &[]))),
        "its words did"
    );
    assert_eq!(
        reworded.said_moved(false),
        Some(None),
        "a hold not here takes them down"
    );
    assert!(!reworded.said_ended(), "nothing left to take down");
}

/// THE FILES GIT FOUND are the hold's words where the repository's probe found work.
/// Negative control: the probe's paths left out of the yield's words fails the names.
#[test]
fn the_files_git_found_are_the_holds_words() {
    let work = RepoWork {
        repo: "/Users//a/Developer/parser".to_string(),
        untracked: 1,
        paths: vec!["wip-notes.txt".to_string()],
        main_ref: "origin/main".to_string(),
        ..RepoWork::default()
    };
    let (mut st, now) = instructed(t0());
    let r = TurnEndReading {
        repo: RepoRead::Found(work),
        ..land(idle(OPEN_DEFENSE, Some(3 * MIN)))
    };
    let (a, _) = decided(&mut st, &r, now);
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    let held = st.land_held().expect("held");
    assert!(held.holds(HoldLeg::Repo), "{held:?}");
    assert_eq!(
        st.land_said().map(|s| s.paths.clone()),
        Some(vec!["wip-notes.txt".to_string()])
    );
    assert!(
        st.land_said().is_some_and(|s| s.open.is_empty()),
        "no list was given: {:?}",
        st.land_said()
    );
}
