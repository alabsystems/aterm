// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE LAND CHECK'S ACTS (§14.17 item 5, slice 13a): where Land types `land-check@v1`, what its
// answer does, and the bounds that keep it from a loop. The slice's As built note names the
// mutant each test was seen failing under.

use crate::harness::acts::HoldLeg;
use crate::harness::repo_work::{RepoRead, RepoWork};
use crate::supervise::policy::land_check::{self, CheckAnswer};

/// What the agent lists as left, answering the check.
const LIST: &str = "The docs PR is still open.";

/// A second defense, new words: a new reply.
const DEFENSE_2: &str = "Nothing left: the parser, the lexer and the docs are merged into the \
                         remote main branch and pushed.";

/// Land on, its instruction typed at `t`: the state and when the instruction's turn ends.
fn instructed(t: Instant) -> (TurnEndState, Instant) {
    let mut st = TurnEndState::default();
    assert_eq!(
        act(&mut st, &land(idle("Stage 1 landed.", Some(3 * MIN))), t),
        land_instruction()
    );
    (st, t + 3 * MIN)
}

/// A turn of real work that ended on `said`.
fn worked(said: &str) -> TurnEndReading {
    land(idle(said, Some(3 * MIN)))
}

/// A short answer `said`, no tools run: an act's no-work answer.
fn short(said: &str) -> TurnEndReading {
    TurnEndReading {
        ran_tools: false,
        ..land(idle(said, Some(Duration::from_secs(10))))
    }
}

/// The decision at `r` at `now`, a wait (the back-off, a retry) waited out on the same point
/// read again: the act and when it came.
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

/// The check typed at a clean defense (`r`, the instruction's answer) and answered `said` a
/// minute later: the decision at that answer, the answer's reading and its time.
fn checked(
    st: &mut TurnEndState,
    r: &TurnEndReading,
    now: Instant,
    said: &str,
) -> (TurnEndAction, TurnEndReading, Instant) {
    let a = at(st, r, now);
    assert_eq!(a.rule_id(), Some(RULE_LAND_CHECK), "the check: {a:?}");
    st.acted(&a, r, now);
    let answer = short(said);
    let (b, then) = decided(st, &answer, now + MIN);
    (b, answer, then)
}

/// A CLEAN DEFENSE GETS THE CHECK, NOT A LANDING: only the check's own `LANDED` lands.
/// Negative control: landing on the word reader's `clean_defense` lands at the defense.
#[test]
fn a_clean_defense_gets_the_land_check_not_a_landing() {
    let (mut st, now) = instructed(t0());
    assert!(clean_defense(Some(DEFENSE)), "the reader reads it clean");
    let r = worked(DEFENSE);
    let a = at(&mut st, &r, now);
    assert_eq!(
        a,
        TurnEndAction::Type {
            text: land_check::land_check_text(
                land_check::hint_clause(DEFENSE).as_deref(),
                false,
                None
            ),
            rule_id: RULE_LAND_CHECK,
        }
    );
    assert!(
        !st.landed() && !st.task_done(),
        "a defense lands nothing: {st:?}"
    );
    st.acted(&a, &r, now);
    let (b, _) = decided(&mut st, &short(LANDED_WORD), now + MIN);
    assert_eq!(b, TurnEndAction::Nothing, "{st:?}");
    assert!(st.landed() && st.task_done(), "LANDED lands: {st:?}");
}

/// A `LANDED` ANSWER KEEPS A NEED IT DID NOT RETRACT (the post-landing review, 2026-11-01): the
/// task lands, and the need its defense raised stays quietly on the menu bar beside the landing
/// until the agent's own `NEEDS YOU: nothing` takes it down, which then lands the same.
/// Negative control: the needs cleared at a `LANDED` (13a's first cut) fails the kept need.
#[test]
fn a_landed_answer_keeps_a_need_it_did_not_retract() {
    let defense =
        format!("{DEFENSE}\nNEEDS YOU: decide whether to publish 1.5.0 to the public tier");
    let (mut st, now) = instructed(t0());
    let r = worked(&defense);
    let a = at(&mut st, &r, now);
    assert_eq!(
        a.rule_id(),
        Some(RULE_LAND_CHECK),
        "a need does not bar the check: {a:?}"
    );
    assert!(typed(&a).contains(NEEDS_ACK), "{a:?}");
    st.acted(&a, &r, now);
    let (b, _) = decided(&mut st, &short(LANDED_WORD), now + MIN);
    assert_eq!(b, TurnEndAction::Nothing, "{st:?}");
    assert!(st.landed(), "LANDED lands: {st:?}");
    assert_eq!(
        st.standing_needs().len(),
        1,
        "the need stands beside the landing: {st:?}"
    );
    // The agent's own retraction takes it down, and lands the same.
    let (mut st, now) = instructed(t0());
    let a = at(&mut st, &r, now);
    st.acted(&a, &r, now);
    let retracted = format!("NEEDS YOU: nothing\n{LANDED_WORD}");
    let (b, _) = decided(&mut st, &short(&retracted), now + MIN);
    assert_eq!(b, TurnEndAction::Nothing, "{st:?}");
    assert!(st.landed(), "{st:?}");
    assert!(st.standing_needs().is_empty(), "{:?}", st.standing_needs());
}

/// AN EARLIER NEED IN THE CHECK'S TURN THAT ITS LAST WORD RETRACTS NEITHER STOPS THE LANDING NOR
/// STANDS (the post-landing review, 2026-11-01): the turn names a need in an interim message,
/// does the work itself, and ends `NEEDS YOU: nothing` then `LANDED`; the last word wins. A need
/// it did not retract still rests (`a_need_in_the_answer_is_a_need`).
/// Negative control: the earlier messages read for needs whatever the last one says
/// (`land_check_answer` and `fold_turn_needs` without `needs::standing_before`) rests on the stale
/// need and keeps it on the menu bar.
#[test]
fn an_earlier_need_the_checks_turn_retracted_lands() {
    let interim = "./check.sh fails on main in tests/test_clock.sh. NEEDS YOU: decide whether to \
                   fix or delete tests/test_clock.sh.";
    let last = format!("NEEDS YOU: nothing\n{LANDED_WORD}");
    assert_eq!(
        land_check::land_check_answer(&last, Some(interim), false),
        CheckAnswer::Landed
    );
    let (mut st, now) = instructed(t0());
    let r = worked(DEFENSE);
    let a = at(&mut st, &r, now);
    assert_eq!(a.rule_id(), Some(RULE_LAND_CHECK), "{a:?}");
    st.acted(&a, &r, now);
    let answer = TurnEndReading {
        turn_said: Some(interim.into()),
        ..worked(&last)
    };
    let (b, _) = decided(&mut st, &answer, now + 4 * MIN);
    assert_eq!(b, TurnEndAction::Nothing, "{st:?}");
    assert!(st.landed(), "{st:?}");
    assert!(!st.land_resting(), "{st:?}");
    assert!(st.standing_needs().is_empty(), "{:?}", st.standing_needs());
    // CONTROL: the same turn without the retraction rests on its need, which stands.
    let (mut st, now) = instructed(t0());
    let a = at(&mut st, &r, now);
    st.acted(&a, &r, now);
    let answer = TurnEndReading {
        turn_said: Some(interim.into()),
        ..worked(LANDED_WORD)
    };
    let (b, _) = decided(&mut st, &answer, now + 4 * MIN);
    assert_eq!(b, TurnEndAction::Nothing, "{st:?}");
    assert!(!st.landed() && st.land_resting(), "{st:?}");
    assert_eq!(st.standing_needs().len(), 1, "{st:?}");
}

/// A DIRTY OR UNREAD REPOSITORY NEVER GETS THE CHECK: the defense gets Land's nudge with the
/// repository's facts (over work git found, its contradiction of the defense, slice 4c).
/// Negative control: the check owed over a probe that found work.
#[test]
fn a_dirty_or_unread_repository_never_gets_the_check() {
    let dirty = RepoWork {
        repo: "/Users//a/Developer/parser".to_string(),
        changed: 2,
        main_ref: "origin/main".to_string(),
        ..RepoWork::default()
    };
    for (why, repo, says) in [
        (
            "found",
            RepoRead::Found(dirty.clone()),
            format!("{CONTRADICTED_GIT} ({})", dirty.summary()),
        ),
        ("unread", RepoRead::Unread, REPO_UNREAD.to_string()),
    ] {
        let (mut st, now) = instructed(t0());
        let r = TurnEndReading {
            repo,
            ..worked(DEFENSE)
        };
        let (a, _) = decided(&mut st, &r, now);
        assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{why}: {a:?}");
        assert!(typed(&a).contains(&says), "{why}: {a:?}");
        assert!(!st.land_check_owed() && !st.landed(), "{why}");
    }
}

/// ONE CHECK PER DEFENSE, AND NEVER AT THE CHECK'S OWN YIELD: an agent that answers the check
/// with a fresh `Nothing left:` defense gets the nudge quoting it, not a second check.
/// Negative control: the check's answer read as a defense (a check at every Land point).
#[test]
fn one_check_per_defense() {
    let (mut st, now) = instructed(t0());
    let (b, answer, then) = checked(&mut st, &worked(DEFENSE), now, DEFENSE_2);
    assert_eq!(b.rule_id(), Some(RULE_LAND_NUDGE), "{b:?}");
    let quoted = land_check::listed_suffix(DEFENSE_2).expect("quoted");
    assert!(typed(&b).contains(&quoted), "{b:?}");
    st.acted(&b, &answer, then);
    assert!(!st.land_check_owed(), "{st:?}");
}

/// A NEW CHECK NEEDS NEW WORK OR A NEW REPLY: the nudge's answer that is a new defense gets a new
/// check, and so does one after real work even in the words already checked.
/// Negative control: work that leaves the audited defenses audited.
#[test]
fn a_new_check_needs_new_work_or_a_new_reply() {
    // A new reply.
    let (mut st, now) = instructed(t0());
    let (b, answer, then) = checked(&mut st, &worked(DEFENSE), now, LIST);
    assert_eq!(b.rule_id(), Some(RULE_LAND_NUDGE), "{b:?}");
    st.acted(&b, &answer, then);
    let (c, _) = decided(&mut st, &short(DEFENSE_2), then + MIN);
    assert_eq!(c.rule_id(), Some(RULE_LAND_CHECK), "a new reply: {c:?}");
    // New work, the same words.
    let (mut st, now) = instructed(t0());
    let (b, answer, then) = checked(&mut st, &worked(DEFENSE), now, LIST);
    st.acted(&b, &answer, then);
    let (c, _) = decided(&mut st, &worked(DEFENSE), then + 4 * MIN);
    assert_eq!(c.rule_id(), Some(RULE_LAND_CHECK), "new work: {c:?}");
}

/// THE SAME LIST TWICE HOLDS, QUOTED: the second check's answer is the list it answered before,
/// so nothing more is typed, and the hold's Open leg is the agent's own list.
/// Negative control: the hold not taking its Open leg from the list.
#[test]
fn the_same_list_twice_holds_quoted() {
    let (mut st, now) = instructed(t0());
    let (b, answer, then) = checked(&mut st, &worked(DEFENSE), now, LIST);
    assert_eq!(b.rule_id(), Some(RULE_LAND_NUDGE), "{b:?}");
    st.acted(&b, &answer, then);
    let defended = short(DEFENSE_2);
    let (c, _) = decided(&mut st, &defended, then + MIN);
    assert_eq!(c.rule_id(), Some(RULE_LAND_CHECK), "{c:?}");
    st.acted(&c, &defended, then + MIN);
    let (d, _) = decided(&mut st, &short(LIST), then + 2 * MIN);
    assert!(
        !matches!(d, TurnEndAction::Type { .. }),
        "the same list holds: {d:?}"
    );
    let held = st.land_held().expect("held");
    assert!(
        held.holds(HoldLeg::Open),
        "the list is the Open leg: {held:?}"
    );
    assert_eq!(st.land_listed(), Some(LIST));
}

/// A NEED-ONLY ANSWER TYPES NOTHING MORE: Land rests on it, quietly, however long the point
/// stands. Negative control: a nudge typed in the rest.
#[test]
fn a_need_only_answer_types_nothing_more() {
    let need = "NEEDS YOU: rotate the deploy key";
    assert_eq!(
        land_check::land_check_answer(need, None, false),
        CheckAnswer::Needs { rest_landed: true }
    );
    let (mut st, now) = instructed(t0());
    let (b, answer, then) = checked(&mut st, &worked(DEFENSE), now, need);
    assert_eq!(b, TurnEndAction::Nothing, "{st:?}");
    assert!(st.land_resting(), "{st:?}");
    let still = TurnEndReading {
        worked: None,
        ..answer
    };
    for later in [10 * MIN, 60 * MIN, 600 * MIN] {
        let a = at(&mut st, &still, then + later);
        assert!(!matches!(a, TurnEndAction::Type { .. }), "{later:?}: {a:?}");
    }
}

/// A LOOP THAT FOLLOWS, seeded as `Session::seed_from_ledger` seeds a ledger whose last Land act
/// was `land_act` and whose last row is the point judged at its answer `ago` before `now`
/// (`approvals::land_episode_in`: one point judged after the act, `landing` dropped,
/// `seed_land_point`), with the land check's `books` that point's row carried.
fn following(
    land_act: &'static str,
    nudges: u32,
    books: LandBooks,
    ago: Duration,
    now: Instant,
) -> TurnEndState {
    let mut st = TurnEndState::default();
    st.seed_land(nudges, false, false, false);
    st.seed_land_books(books);
    if land_act == RULE_LAND_CHECK {
        st.seed_land_checked();
    }
    st.seed_land_owed(land_act);
    st.seed_short(1);
    st.seed_point(ago, now);
    st.seed_land_point();
    st
}

/// THE CHECK'S ANSWER SURVIVES A LOOP THAT FOLLOWS (the post-landing review, 2026-11-01): a rest
/// on a need types nothing, however long it stands; a list stays the check's answer, which Land's
/// next prompt quotes (never `land-answer@v1`, a list that reads as a question included).
/// Negative control: the seeded point re-judged as a defense (`land_defended` alone at
/// `rejudged`) types `land-nudge@v1` into the rest and `land-answer@v1` at the question.
#[test]
fn the_checks_answer_survives_a_loop_that_follows() {
    let need = "NEEDS YOU: rotate the deploy key";
    let (mut st, now) = instructed(t0());
    let (b, answer, then) = checked(&mut st, &worked(DEFENSE), now, need);
    assert_eq!(b, TurnEndAction::Nothing, "{st:?}");
    let later = then + 30 * MIN;
    let mut st = following(RULE_LAND_CHECK, 0, st.land_books(), 30 * MIN, later);
    let still = TurnEndReading {
        worked: None,
        ..answer
    };
    for wait in [Duration::ZERO, 10 * MIN, 600 * MIN] {
        let a = at(&mut st, &still, later + wait);
        assert!(!matches!(a, TurnEndAction::Type { .. }), "{wait:?}: {a:?}");
    }
    assert!(st.land_resting(), "{st:?}");
    for list in [
        LIST,
        "Should I merge the docs PR now, or leave it open for review?",
    ] {
        let (mut st, now) = instructed(t0());
        let (_, answer, then) = checked(&mut st, &worked(DEFENSE), now, list);
        let books = st.land_books();
        let mut st = following(RULE_LAND_CHECK, 0, books, MIN, then + MIN);
        let still = TurnEndReading {
            worked: None,
            ..answer
        };
        let (a, _) = decided(&mut st, &still, then + MIN);
        assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{list}: {a:?}");
        assert!(
            typed(&a).contains(&land_check::listed_suffix(list).expect("quoted")),
            "{list}: {a:?}"
        );
    }
}

/// A DEFENSE THE CHECK AUDITED STAYS AUDITED ACROSS A LOOP THAT FOLLOWS (the post-landing review,
/// 2026-11-01): checked once and answered with a list, nudged, then said again, it gets Land's
/// nudge from the restarted loop, not a second check. Negative control: the books not carried
/// (`seed_land_books` a no-op) types `land-check@v1` a second time for one defense.
#[test]
fn a_defense_the_check_audited_stays_audited_across_a_loop_that_follows() {
    let (mut st, now) = instructed(t0());
    let (b, answer, then) = checked(&mut st, &worked(DEFENSE), now, LIST);
    assert_eq!(b.rule_id(), Some(RULE_LAND_NUDGE), "{b:?}");
    st.acted(&b, &answer, then);
    let again = short(DEFENSE);
    let a = at(&mut st, &again, then + MIN);
    assert_ne!(a.rule_id(), Some(RULE_LAND_CHECK), "the live loop: {a:?}");
    let books = st.land_books();
    assert!(books.audited.iter().any(Option::is_some), "{books:?}");
    let mut st = following(RULE_LAND_NUDGE, 1, books, MIN, then + 2 * MIN);
    let still = TurnEndReading {
        worked: None,
        ..again
    };
    let (a, _) = decided(&mut st, &still, then + 2 * MIN);
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
}

/// A LIST IS QUOTED IN LAND'S NEXT PROMPT, never the word reader's open words.
/// Negative control: the reader's open-words suffix kept in Land's nudge.
#[test]
fn a_list_is_quoted_in_lands_next_prompt() {
    // A list the reader reads open, said with a done claim, so a reader's suffix would have words.
    let list = "All done except the docs PR, which is still open.";
    assert!(
        !open_work_strict(Some(list)).is_empty(),
        "the reader reads it open"
    );
    let (mut st, now) = instructed(t0());
    let (b, _, _) = checked(&mut st, &worked(DEFENSE), now, list);
    assert_eq!(b.rule_id(), Some(RULE_LAND_NUDGE), "{b:?}");
    let text = typed(&b);
    let quoted = land_check::listed_suffix(list).expect("quoted");
    assert!(text.contains(&quoted), "{text}");
    assert!(!text.contains("still names open work"), "{text}");
    assert_eq!(text, format!("{LAND_NUDGE} {quoted}"));
}

/// THE CHECK'S ANSWER IS NEVER TAKEN FOR A QUESTION: a list beside a need, worded as a choice
/// put to the person (the real-Claude drive's still-fails answer, cut), gets Land's nudge quoting
/// it, its need acknowledged, never Land's answer to a choice ("Nobody is here to choose"), which
/// talked that agent out of its own need.
/// Negative control: the question reader asked at the check's answer.
#[test]
fn a_checks_answer_is_never_taken_for_a_question() {
    let answer = "Not landed: ./check.sh still fails on main, so I can't report this as landed.\n\n\
                  - No fix I can make: making it pass would mean deleting or rewriting the test. \
                  Changing what the repo requires is your call.\n\n\
                  NEEDS YOU: decide what to do with tests/test_clock.sh (delete it, or rewrite it \
                  to test something real).";
    assert!(
        short(answer).asked().is_some(),
        "the question reader reads a choice"
    );
    assert_eq!(
        land_check::land_check_answer(answer, None, false),
        CheckAnswer::Needs { rest_landed: false }
    );
    let (mut st, now) = instructed(t0());
    let (b, _, _) = checked(&mut st, &worked(DEFENSE), now, answer);
    assert_eq!(b.rule_id(), Some(RULE_LAND_NUDGE), "{b:?}");
    let text = typed(&b);
    assert!(text.starts_with(LAND_NUDGE), "{text}");
    assert!(text.contains(NEEDS_ACK), "the need acknowledged: {text}");
    let list = needs::without_needs(answer);
    let quoted = land_check::listed_suffix(list.trim()).expect("quoted");
    assert!(text.contains(&quoted), "the list quoted: {text}");
}

/// THE CHECK IS NOT WITHHELD BY THE BACK-OFF: a short defense, the back-off armed for any other
/// act, gets the check at once. Negative control: the back-off's arm before the check.
#[test]
fn the_check_is_not_withheld_by_the_back_off() {
    let (mut st, now) = instructed(t0());
    let a = at(&mut st, &short("Stage 2 landed; the lexer is next."), now);
    assert!(
        matches!(
            a,
            TurnEndAction::WaitUntil {
                wait: WaitKind::BackOff { .. },
                ..
            }
        ),
        "a short answer waits the back-off: {a:?}"
    );
    let (mut st, now) = instructed(t0());
    let a = at(&mut st, &short(DEFENSE), now);
    assert_eq!(a.rule_id(), Some(RULE_LAND_CHECK), "at once: {a:?}");
}

/// EVERY LAND READER KNOWS THE CHECK by its head, in every form it is typed in: the policy's own
/// words, the journal's seed, the stats' prompts and the upgrade's transcript reader.
/// Negative control: each list without the head.
#[test]
fn every_land_reader_knows_the_check() {
    for text in [
        land_check::LAND_CHECK.to_string(),
        land_check::land_check_text(
            Some("the docs PR is still open"),
            true,
            Some("never force-push"),
        ),
    ] {
        assert!(is_land_words(&text), "is_land_words: {text}");
        assert!(
            crate::supervise::approvals::delivers_land(Some(&text)),
            "delivers_land: {text}"
        );
        assert!(
            crate::harness::stats::opens_with_land_words(&text),
            "opens_with_land_words: {text}"
        );
        assert!(
            crate::harness::upgrade::is_land_act(&text),
            "is_land_act: {text}"
        );
    }
    assert!(!is_land_words(land_check::LAND_CHECK_BODY));
}

/// A SEEDED CHECK READS THE NEXT ANSWER AS ITS OWN: where the journal's seed says the last Land
/// act was the check, a loop after it lands on the `LANDED` it reads; seeded without it, it does
/// not (`a_restarted_loop_lands_on_its_checks_landed_answer` holds the journal to the seed).
#[test]
fn a_seeded_check_reads_the_next_answer_as_its_own() {
    for checked in [true, false] {
        let mut st = TurnEndState::default();
        st.seed_land(0, true, false, false);
        if checked {
            st.seed_land_checked();
        }
        let a = at(&mut st, &short(LANDED_WORD), t0());
        assert_eq!(st.landed(), checked, "{checked}: {a:?}");
        if checked {
            assert_eq!(a, TurnEndAction::Nothing);
        }
    }
}

/// THE LANDING KEEPS THE DEFENSE ITS CHECK AUDITED: after the check's `LANDED`, the defense is
/// what the landing quotes, never the word (`the_receipt_quotes_the_audited_defense` reads the
/// result a host takes).
#[test]
fn the_landing_keeps_the_defense_it_audited() {
    let (mut st, now) = instructed(t0());
    let (b, _, _) = checked(&mut st, &worked(DEFENSE), now, LANDED_WORD);
    assert_eq!(b, TurnEndAction::Nothing);
    assert!(st.landed());
    assert_eq!(
        st.land_audited(),
        Some(land_check::audited(DEFENSE).as_str())
    );
}

/// A short answer `said` after a tool ran (a `git status`): a turn under the work floor.
fn tooled(said: &str) -> TurnEndReading {
    TurnEndReading {
        ran_tools: true,
        ..land(idle(said, Some(Duration::from_secs(20))))
    }
}

/// A GO-AHEAD PARK ANSWERING THE CHECK GETS THE GO-AHEAD, never a rest: the owner's standing
/// go-ahead (2026-10-29) answers a request for the agent's own reversible work, so it is no
/// person's need to wait on. Negative control: the rest's gate without the park (Land waits on
/// the request for good, the line left on the menu bar).
#[test]
fn a_go_ahead_park_answering_the_check_gets_the_go_ahead() {
    let park = "NEEDS YOU: say \"rerun the checks\" so I can build and test the upgrade fix \
                branch, push it to main and remove the worktree.";
    assert_eq!(
        land_check::land_check_answer(park, None, false),
        CheckAnswer::Needs { rest_landed: true }
    );
    let (mut st, now) = instructed(t0());
    let (b, answer, then) = checked(&mut st, &worked(DEFENSE), now, park);
    assert_eq!(b.rule_id(), Some(RULE_GO_AHEAD), "{b:?}");
    assert!(
        then.saturating_duration_since(now) <= MIN + GO_AHEAD_WAIT,
        "within the park's own wait: {:?}",
        then.saturating_duration_since(now)
    );
    st.acted(&b, &answer, then);
    assert!(!st.land_resting(), "{st:?}");
}

/// A SHORT TOOL TURN IS NO NEW WORK FOR THE CHECK: the same defense and the same list, each said
/// after a `git status`, get one check, then Land's nudge quoting the list (the check's own
/// answer), then, at each point the audited defense is said again over the standing list, Land's
/// contradiction quoting it (slice 4c), in that order, never the check again. Negative controls:
/// any tool run lifting the audit (a check after every nudge); the Open leg's contradiction left
/// out (the list quoted at every point).
#[test]
fn a_short_tool_turn_is_no_new_work_for_the_check() {
    let (mut st, mut now) = instructed(t0());
    let mut r = tooled(DEFENSE);
    let mut checks = 0;
    let quoted = land_check::listed_suffix(LIST).expect("quoted");
    let contradicted = format!("{LAND_CONTRADICTED_HEAD} {CONTRADICTED_LISTED}{LIST}\u{201d}");
    let mut nudges = Vec::new();
    for _ in 0..6 {
        let (a, then) = decided(&mut st, &r, now);
        if !matches!(a, TurnEndAction::Type { .. }) {
            break;
        }
        if a.rule_id() == Some(RULE_LAND_CHECK) {
            checks += 1;
        } else {
            assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
            let text = typed(&a);
            nudges.push(if text.starts_with(&contradicted) {
                "contradicted"
            } else if text.starts_with(LAND_NUDGE) && text.contains(&quoted) {
                "quoted"
            } else {
                panic!("the list unquoted: {a:?}")
            });
        }
        st.acted(&a, &r, then);
        now = then + MIN;
        r = if a.rule_id() == Some(RULE_LAND_CHECK) {
            tooled(LIST)
        } else {
            tooled(DEFENSE)
        };
    }
    assert_eq!(checks, 1, "one check for one defense: {st:?}");
    // The check's own answer is quoted; every defense said over the standing list after it (a
    // tool run each time, so no same reply holds it) is contradicted, one act per point.
    assert!(
        nudges.first() == Some(&"quoted")
            && nudges.len() > 1
            && nudges[1..].iter().all(|n| *n == "contradicted"),
        "the check's answer quoted, then the defense over it contradicted: {nudges:?}"
    );
}

/// A `LANDED` OVER A DIRTY REPOSITORY KEEPS THE NEED its defense raised: nothing landed, so the
/// person's part still stands, on the menu bar, beside Land's nudge with the repository's facts.
/// Negative control: the need taken down at every `LANDED`.
#[test]
fn a_landed_answer_over_a_dirty_repository_keeps_the_need() {
    let defense = format!("NEEDS YOU: rotate the deploy key\n\n{DEFENSE}");
    let (mut st, now) = instructed(t0());
    let r = worked(&defense);
    let a = at(&mut st, &r, now);
    assert_eq!(a.rule_id(), Some(RULE_LAND_CHECK), "{a:?}");
    st.acted(&a, &r, now);
    let answer = TurnEndReading {
        repo: RepoRead::Found(RepoWork {
            repo: "/Users//a/Developer/parser".to_string(),
            changed: 2,
            main_ref: "origin/main".to_string(),
            ..RepoWork::default()
        }),
        ..short(LANDED_WORD)
    };
    let (b, _) = decided(&mut st, &answer, now + MIN);
    assert_eq!(b.rule_id(), Some(RULE_LAND_NUDGE), "{b:?}");
    assert!(!st.landed(), "{st:?}");
    assert_eq!(
        st.standing_needs(),
        ["rotate the deploy key".to_string()],
        "the need stands"
    );
}

/// A CHECK THE AGENT NEVER TOOK leaves its defense owed the check: the point read again with no
/// turn run is no answer, so the same defense said again gets the check, and its `LANDED` lands.
/// Negative control: the defense checked when the check is typed (the clean defense nudged until
/// it holds, never landed).
#[test]
fn a_check_never_taken_leaves_its_defense_owed_the_check() {
    let (mut st, now) = instructed(t0());
    let r = worked(DEFENSE);
    let a = at(&mut st, &r, now);
    assert_eq!(a.rule_id(), Some(RULE_LAND_CHECK), "{a:?}");
    st.acted(&a, &r, now);
    let unanswered = TurnEndReading {
        worked: None,
        ..r.clone()
    };
    let (b, then) = decided(&mut st, &unanswered, now + MIN);
    assert_eq!(b.rule_id(), Some(RULE_LAND_NUDGE), "{b:?}");
    st.acted(&b, &unanswered, then);
    let again = short(DEFENSE);
    let (c, then) = decided(&mut st, &again, then + MIN);
    assert_eq!(c.rule_id(), Some(RULE_LAND_CHECK), "owed again: {c:?}");
    st.acted(&c, &again, then);
    let (d, _) = decided(&mut st, &short(LANDED_WORD), then + MIN);
    assert_eq!(d, TurnEndAction::Nothing, "{st:?}");
    assert!(st.landed(), "{st:?}");
}

/// A CHECK'S TURN THE PERSON STOPPED LANDS NOTHING: its last words, `LANDED` among them, are not
/// the agent's audit. Negative control: a stopped turn's `LANDED` read as the answer.
#[test]
fn a_stopped_check_turn_lands_nothing() {
    let (mut st, now) = instructed(t0());
    let r = worked(DEFENSE);
    let a = at(&mut st, &r, now);
    st.acted(&a, &r, now);
    let stopped = TurnEndReading {
        interrupted: true,
        ..short(LANDED_WORD)
    };
    let b = at(&mut st, &stopped, now + MIN);
    assert!(!st.landed() && !st.task_done(), "{b:?} {st:?}");
}

/// A `LANDED` BEHIND THE PERSON'S UNSENT INPUT lands once they clear it: the answer is not judged
/// while their input stands, and is judged at the same reply read again with the composer clear.
/// Negative control: the answer dropped at the input's yield (a nudge, then a new check round).
#[test]
fn a_landed_answer_behind_the_persons_unsent_input_lands_once_they_clear_it() {
    let (mut st, now) = instructed(t0());
    let r = worked(DEFENSE);
    let a = at(&mut st, &r, now);
    st.acted(&a, &r, now);
    let typing = TurnEndReading {
        pending_input: true,
        ..short(LANDED_WORD)
    };
    let b = at(&mut st, &typing, now + MIN);
    assert!(!matches!(b, TurnEndAction::Type { .. }), "{b:?}");
    assert!(!st.landed(), "not while their input stands: {st:?}");
    let cleared = TurnEndReading {
        worked: None,
        ..short(LANDED_WORD)
    };
    // The point is read again until it is judged (an act's answer waits its take window).
    let mut c = TurnEndAction::Nothing;
    for k in 0..4 {
        c = at(&mut st, &cleared, now + 2 * MIN + k * MIN);
        if st.landed() || matches!(c, TurnEndAction::Type { .. }) {
            break;
        }
    }
    assert_eq!(c, TurnEndAction::Nothing, "{st:?}");
    assert!(st.landed(), "{st:?}");
}

/// THE CHECK'S TURN MAY NAME WHAT IT THEN LANDED: the check asks the agent to land what it can
/// before it answers, so a turn whose earlier message names a leftover it went on to fix, and
/// whose answer is `LANDED`, lands on that answer. The grammar reads the answer, never the
/// turn's working (a `NEEDS YOU:` line anywhere in it still keeps it from landing).
/// Negative control: the earlier messages' open clauses read as the agent's list.
#[test]
fn the_checks_turn_may_name_what_it_then_landed() {
    let working = "All done except the docs PR, which is still open. Merging it now.";
    assert!(
        !open_work_strict(Some(working)).is_empty(),
        "the strict reader reads it open"
    );
    assert_eq!(
        land_check::land_check_answer(LANDED_WORD, Some(working), false),
        CheckAnswer::Landed
    );
    let (mut st, now) = instructed(t0());
    let r = worked(DEFENSE);
    let a = at(&mut st, &r, now);
    st.acted(&a, &r, now);
    let answer = TurnEndReading {
        turn_said: Some(working.to_string()),
        ..worked(LANDED_WORD)
    };
    let (b, _) = decided(&mut st, &answer, now + 4 * MIN);
    assert_eq!(b, TurnEndAction::Nothing, "{st:?}");
    assert!(st.landed(), "{st:?}");
}

/// A CHECK A FULL CONTEXT CUT, TYPED AGAIN AFTER THE COMPACTION, AUDITS THE SAME DEFENSE (the
/// post-landing review, 2026-11-01): the compaction's retype owes nothing afresh, so `LANDED`
/// keeps the audited defense, and a list answer leaves that defense audited: said again, it is
/// nudged, never checked twice.
#[test]
fn a_check_typed_again_after_a_compaction_audits_the_same_defense() {
    use aterm_phase::WallKind;
    let context = |worked| land(walled(WallKind::Context, "Context limit reached", worked));
    let compacted = land(idle(
        "Conversation compacted.",
        Some(Duration::from_secs(40)),
    ));
    for (said, lands) in [(LANDED_WORD, true), (LIST, false)] {
        let (mut st, now) = instructed(t0());
        let r = worked(DEFENSE);
        let a = at(&mut st, &r, now);
        assert_eq!(a.rule_id(), Some(RULE_LAND_CHECK), "{a:?}");
        st.acted(&a, &r, now);
        let mut now = now + MIN;
        let compact = act(&mut st, &context(Some(MIN)), now);
        assert!(
            matches!(
                compact,
                TurnEndAction::TypeCommand {
                    rule_id: RULE_COMPACT,
                    ..
                }
            ),
            "{compact:?}"
        );
        now += MIN;
        let again = act(&mut st, &compacted, now);
        assert!(
            again.rule_id() == Some(RULE_COMPACT) && typed(&again).starts_with(LAND_CHECK_HEAD),
            "the check again: {again:?}"
        );
        now += MIN;
        let answer = short(said);
        let (b, then) = decided(&mut st, &answer, now);
        if lands {
            assert!(st.landed(), "{b:?} {st:?}");
            assert_eq!(
                st.land_audited(),
                Some(land_check::audited(DEFENSE).as_str()),
                "the landing keeps the defense it audited"
            );
        } else {
            assert_eq!(b.rule_id(), Some(RULE_LAND_NUDGE), "{b:?}");
            st.acted(&b, &answer, then);
            let (c, _) = decided(&mut st, &short(DEFENSE), then + MIN);
            assert_ne!(c.rule_id(), Some(RULE_LAND_CHECK), "checked twice: {c:?}");
        }
    }
}

/// THE CHECK'S TURN, AN EARLIER MESSAGE OF IT RETRACTING ITS NEED: the need, the work done, then
/// `NEEDS YOU: nothing`, each said before the answer the check asks for alone.
const RETRACTED_BEFORE: &str = "./check.sh fails on main in tests/test_clock.sh. NEEDS YOU: decide \
                                whether to fix or delete tests/test_clock.sh.\n\nThe flake was \
                                mine: fixed it, committed and pushed. NEEDS YOU: nothing";

/// A RETRACTION IN AN EARLIER MESSAGE OF THE CHECK'S TURN IS ITS LAST WORD (the second review,
/// 2026-11-01): the check asks for `LANDED` alone, so an agent that obeys says its `NEEDS YOU:
/// nothing` in a message before it; the turn lands, rests on nothing, and the need a point before
/// it raised comes down. A need said after the retraction still stands, and rests.
/// Negative control: `needs::standing_before` reading the last message alone (the first round's
/// rule) rests on the retracted need and keeps it on the menu bar.
#[test]
fn a_need_an_earlier_message_of_the_checks_turn_retracted_lands() {
    assert_eq!(
        land_check::land_check_answer(LANDED_WORD, Some(RETRACTED_BEFORE), false),
        CheckAnswer::Landed
    );
    let defense =
        format!("{DEFENSE}\nNEEDS YOU: decide whether to fix or delete tests/test_clock.sh.");
    let (mut st, now) = instructed(t0());
    let r = worked(&defense);
    let a = at(&mut st, &r, now);
    assert_eq!(a.rule_id(), Some(RULE_LAND_CHECK), "{a:?}");
    assert_eq!(
        st.standing_needs().len(),
        1,
        "raised at the defense: {st:?}"
    );
    st.acted(&a, &r, now);
    let answer = TurnEndReading {
        turn_said: Some(RETRACTED_BEFORE.into()),
        ..worked(LANDED_WORD)
    };
    let (b, _) = decided(&mut st, &answer, now + 4 * MIN);
    assert_eq!(b, TurnEndAction::Nothing, "{st:?}");
    assert!(st.landed() && !st.land_resting(), "{st:?}");
    assert!(st.standing_needs().is_empty(), "{:?}", st.standing_needs());
    // CONTROL: a need said after the retraction stands, and the answer rests on it.
    let (mut st, now) = instructed(t0());
    let r = worked(DEFENSE);
    let a = at(&mut st, &r, now);
    st.acted(&a, &r, now);
    let answer = TurnEndReading {
        turn_said: Some(format!(
            "{RETRACTED_BEFORE}\n\nNEEDS YOU: rotate the deploy key"
        )),
        ..worked(LANDED_WORD)
    };
    let (b, _) = decided(&mut st, &answer, now + 4 * MIN);
    assert_eq!(b, TurnEndAction::Nothing, "{st:?}");
    assert!(!st.landed() && st.land_resting(), "{st:?}");
    assert_eq!(st.standing_needs(), ["rotate the deploy key"], "{st:?}");
}

/// AN OFF NO PERSON SET KEEPS LAND'S REST ON A NEED (the second review, 2026-11-01): a lead's
/// `meta set autopilot off`, then `land` (`autopilot_by=ctl`, no person's turn: R1-1), over the
/// rest types nothing, however long it stands, and the need stays on the menu bar; a loop the host
/// starts as that Off lifts, seeded from the ledger (`OffStood::Resting`), reads its point as the
/// check's answer again and types nothing either. The person's own lift to Land is their turn:
/// Land's act, as at their switch from Cruise. CONTROL: the person's own Off forgets the rest, and
/// Land then types its act. Negative control: `park_off_held` leaving the land episode and the
/// needs (the first round's park) types `land@v1` into the rest and takes the need down.
#[test]
fn an_off_no_person_set_keeps_lands_rest_on_a_need() {
    let need = "NEEDS YOU: rotate the deploy key";
    let rest = || {
        let (mut st, now) = instructed(t0());
        let (b, answer, then) = checked(&mut st, &worked(DEFENSE), now, need);
        assert_eq!(b, TurnEndAction::Nothing, "{st:?}");
        assert!(st.land_resting(), "{st:?}");
        let still = TurnEndReading {
            worked: None,
            ..answer
        };
        (st, still, then)
    };
    let (mut st, still, then) = rest();
    assert_eq!(st.park_off_held(), OffStood::Resting);
    for wait in [MIN, 10 * MIN, 60 * MIN] {
        let a = at(&mut st, &still, then + wait);
        assert!(!matches!(a, TurnEndAction::Type { .. }), "{wait:?}: {a:?}");
    }
    assert!(st.land_resting(), "{st:?}");
    assert_eq!(st.standing_needs(), ["rotate the deploy key"], "{st:?}");
    // The person's own lift from that Off.
    let (mut st, still, then) = rest();
    assert_eq!(st.park_off_held(), OffStood::Resting);
    let (a, _) = decided(&mut st, &by_person(still.clone()), then + MIN);
    assert!(
        matches!(
            a,
            TurnEndAction::Type {
                rule_id: RULE_LAND,
                ..
            }
        ),
        "the person's own lift: {a:?}"
    );
    // CONTROL: the person's own Off.
    let (mut st, still, then) = rest();
    st.park_off();
    let (a, _) = decided(&mut st, &still, then + MIN);
    assert_eq!(a, land_instruction(), "{st:?}");
    // A loop the host starts as the Off lifts: the books its point's row carried, the episode
    // `land_episode_in` keeps past the Off's row.
    let (st, still, then) = rest();
    for (kept, types) in [(OffStood::Resting, false), (OffStood::Held, true)] {
        let mut next = following(RULE_LAND_CHECK, 0, st.land_books(), MIN, then + MIN);
        next.start_lifted_off(kept);
        let (a, _) = decided(&mut next, &still, then + MIN);
        assert_eq!(
            matches!(a, TurnEndAction::Type { .. }),
            types,
            "{kept:?}: {a:?}"
        );
        assert_eq!(next.land_resting(), !types, "{kept:?}: {next:?}");
    }
}
