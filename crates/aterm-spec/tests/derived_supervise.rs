// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for the supervisor's machines (aterm-agent `supervise`): each
//! proves at `Buggy = 0` and yields its counterexample at `Buggy = 1`. The
//! Tier-1 binds drive the real `Session` (aterm-agent
//! `supervise/run_engine_tests.rs`), and the question answer's pure decider
//! too (`supervise/policy/question_tests.rs`).

#[test]
fn the_supervisor_claim_proves_and_catches_a_press_on_a_stale_view() {
    aterm_spec::verify::prove_and_catch_scalar(
        &aterm_spec::derive::supervisor_claim_model(),
        "supervisor claim: renew before a press",
    );
}

#[test]
fn the_focus_choice_proves_and_catches_an_enter_on_an_unconfirmed_focus() {
    aterm_spec::verify::prove_and_catch_scalar(
        &aterm_spec::derive::supervisor_focus_choice_model(),
        "supervisor focus choice: confirm before Enter",
    );
}

#[test]
fn the_turn_end_policy_proves_and_catches_typing_over_a_person_and_escalating_done() {
    let m = aterm_spec::derive::supervisor_turn_end_model();
    aterm_spec::verify::prove_and_catch_scalar(
        &m,
        "supervisor turn end: never type under a box, a person, over a draft or before a \
         back-off; never escalate an answerable point",
    );
    // With a cap the owner wrote, the same machine proves too.
    aterm_spec::verify::prove_and_catch_scalar(
        &aterm_spec::interp::with_consts(&m, &[("Budget", 2)]),
        "supervisor turn end under a written cap",
    );
}

/// THE DONE CHECK (2026-09-27): a done report is typed the check, only the
/// check's own done yield ends the task, and nothing is typed into a done
/// task, and a wall someone else's turn ended on is acted on even after the
/// task was done — proven; the policy before it (`Buggy = 1`: a done report
/// continued like any point, and ended as soon as it repeats; the check's
/// first cut, which left a done task closed at a wall) is caught by each.
#[test]
fn the_done_check_proves_and_catches_nudging_a_finished_task() {
    aterm_spec::verify::prove_and_catch_scalar(
        &aterm_spec::derive::supervisor_done_check_model(),
        "supervisor done check: a done report is checked once, then left",
    );
    let m = aterm_spec::derive::supervisor_done_check_model();
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    for (path, caught) in [
        (&["OtherTurnDone", "Continue"][..], "ADoneReportIsChecked"),
        (
            &["OtherTurnDone", "Continue", "YieldDone"][..],
            "OnlyTheCheckEndsTheTask",
        ),
        (
            &["OtherTurnDone", "Check", "YieldDone", "Continue"][..],
            "NeverTypeIntoADoneTask",
        ),
        (
            &["OtherTurnDone", "Check", "YieldDone", "OtherTurnWall"][..],
            "AWallIsNeverLeftOnADoneTask",
        ),
    ] {
        let mut st = buggy.init_state();
        for action in path {
            assert!(buggy.fire(action, &mut st), "{action} at {st:?}");
        }
        assert!(!buggy.check_invariant(caught, &st), "{path:?}: {st:?}");
    }
    // The shipped machine ends the task at the check's done yield alone.
    let mut st = m.init_state();
    for action in ["OtherTurnDone", "Check", "YieldDone"] {
        assert!(m.fire(action, &mut st), "{action} at {st:?}");
    }
    assert_eq!(st["done"], 2);
    assert!(!m.action_enabled("Continue", &st) && !m.action_enabled("Check", &st));
    // …and someone else's turn that ended on a wall opens it: the wall's
    // act is owed (the review of 2026-09-27).
    assert!(m.fire("OtherTurnWall", &mut st));
    assert!(m.action_enabled("WallAct", &st), "{st:?}");
}

/// THE STALL'S REMEDY (D4, 2026-09-27, cut back by that day's review): one
/// `signal term` an episode, never under a person's hand, never a kill —
/// proven; a remedy that kills a minute on, one blind to a hand, and one
/// that forgets it signalled are each caught.
#[test]
fn the_stall_remedy_proves_and_catches_a_kill_a_blind_term_and_a_second_one() {
    let m = aterm_spec::derive::supervisor_stall_remedy_model();
    aterm_spec::verify::prove_and_catch_scalar(
        &m,
        "supervisor stall remedy: one term an episode, nobody there, never a kill",
    );
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    for (path, caught) in [
        (&["Freeze", "Age", "Term", "Kill"][..], "NeverAKill"),
        (
            &["HandOn", "Freeze", "Age", "TermBlind"][..],
            "NeverUnderAHand",
        ),
        (
            &["Freeze", "Age", "Term", "TermAgain"][..],
            "OneTermAnEpisode",
        ),
    ] {
        let mut st = buggy.init_state();
        for action in path {
            assert!(buggy.fire(action, &mut st), "{action} at {st:?}");
        }
        assert!(!buggy.check_invariant(caught, &st), "{path:?}: {st:?}");
    }
    // The shipped machine: a term once the stall has aged, and again only
    // in a new episode; never with the bound off, nowhere to relaunch, or
    // for a stopped job.
    let mut st = m.init_state();
    for action in ["Freeze", "Age", "Term"] {
        assert!(m.fire(action, &mut st), "{action} at {st:?}");
    }
    assert!(!m.action_enabled("Term", &st), "once an episode");
    for action in ["Thaw", "Freeze", "Age", "Term"] {
        assert!(m.fire(action, &mut st), "{action} at {st:?}");
    }
    for flip in [&["BoundFlips"][..], &["HostFlips"][..]] {
        let mut st = m.init_state();
        for action in flip.iter().chain(["Freeze", "Age"].iter()) {
            assert!(m.fire(action, &mut st), "{action} at {st:?}");
        }
        assert!(!m.action_enabled("Term", &st), "{flip:?}");
    }
    let mut st = m.init_state();
    for action in ["Stop", "Age"] {
        assert!(m.fire(action, &mut st), "{action} at {st:?}");
    }
    assert!(!m.action_enabled("Term", &st), "a stopped job");
}

/// N1 OF THE LIVE E2E OF 2026-09-26: the harness's own turns (the upgrade's
/// notice answered READY, its carry-on answered) are no work of the
/// worker's — after them the streak and its back-off are what the worker's
/// turns left, and a free point is continued; the policy that reads them as
/// short turns of someone else's (`Buggy = 1`) backs off for them, caught by
/// `NeverBackOffForTheHarness` on that path alone.
#[test]
fn the_turn_end_policy_backs_off_for_no_turn_of_the_harness() {
    let m = aterm_spec::derive::supervisor_turn_end_model();
    let asked = |mut st: std::collections::BTreeMap<&'static str, i64>| {
        st.insert("asked", 1);
        st.insert("tasked", 1);
        st
    };
    let mut st = asked(m.init_state());
    for action in ["HarnessTurn", "HarnessTurn"] {
        assert!(m.fire(action, &mut st), "{action} at {st:?}");
    }
    assert_eq!((st["short"], st["backoff"]), (0, 0), "{st:?}");
    assert!(m.action_enabled("Continue", &st), "{st:?}");
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut wrong = asked(buggy.init_state());
    assert!(buggy.fire("HarnessTurn", &mut wrong));
    assert!(
        !buggy.check_invariant("NeverBackOffForTheHarness", &wrong),
        "{wrong:?}"
    );
    for inv in &m.invariants {
        if inv.name != "NeverBackOffForTheHarness" {
            assert!(buggy.check_invariant(inv.name, &wrong), "{}", inv.name);
        }
    }
}

/// THE REVIEW OF THE N1 FIX: a harness turn answered with REAL WORK (the
/// carry-on's answer is the worker's own work, resumed) ends the streak as
/// any turn of real work does — after a short turn of the worker's has
/// started a back-off, the long answer leaves the point free; the policy
/// that leaves the streak standing for it (`Buggy = 1`) backs off after real
/// work, caught by `NeverBackOffAfterRealWork` on that path alone.
#[test]
fn a_harness_turn_answered_with_real_work_ends_the_streak() {
    let m = aterm_spec::derive::supervisor_turn_end_model();
    let short_turn = |m: &aterm_spec::derive::Model| {
        let mut st = m.init_state();
        st.insert("asked", 1);
        st.insert("tasked", 1);
        for action in ["Continue", "WorkedShort"] {
            assert!(m.fire(action, &mut st), "{action} at {st:?}");
        }
        assert_eq!(st["backoff"], 1, "{st:?}");
        st
    };
    let mut st = short_turn(&m);
    assert!(m.fire("HarnessTurnLong", &mut st));
    assert_eq!((st["short"], st["backoff"]), (0, 0), "{st:?}");
    assert!(m.action_enabled("Continue", &st), "{st:?}");
    // …and answered short, the back-off stands.
    let mut held = short_turn(&m);
    assert!(m.fire("HarnessTurn", &mut held));
    assert_eq!((held["short"], held["backoff"]), (1, 1), "{held:?}");
    assert!(!m.action_enabled("Continue", &held), "{held:?}");
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut wrong = short_turn(&buggy);
    assert!(buggy.fire("HarnessTurnLong", &mut wrong));
    assert!(
        !buggy.check_invariant("NeverBackOffAfterRealWork", &wrong),
        "{wrong:?}"
    );
    for inv in &m.invariants {
        if inv.name != "NeverBackOffAfterRealWork" {
            assert!(buggy.check_invariant(inv.name, &wrong), "{}", inv.name);
        }
    }
}

/// D3 OF THE LIVE E2E OF 2026-09-26: a turn end left to the session's host
/// is decided again once the host owns nothing, and one a host step moved
/// (it typed into the agent, or ended it) never is — proven; the loop that
/// decides each point once (`Buggy = 1`) waits on one nobody decides, and
/// the loop that decides again whatever a step left behind decides a point
/// that is gone — each caught by its own invariant.
#[test]
fn the_host_turn_end_proves_and_catches_a_point_left_to_nobody() {
    let model = aterm_spec::derive::supervisor_host_turn_end_model();
    aterm_spec::verify::prove_and_catch_scalar(
        &model,
        "supervisor host turn end: a point the host lets go is decided again",
    );
    // Each bug `Buggy = 1` admits is reached on its own path.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    for (path, invariant) in [
        (
            &["TurnEnds", "HostLetsGo", "LoopWaits"][..],
            "NoTurnEndLeftToNobody",
        ),
        (
            &["TurnEnds", "HostMoves", "Redecide"][..],
            "NeverDecideAPointAStepMoved",
        ),
    ] {
        let mut st = buggy.init_state();
        for action in path {
            assert!(buggy.fire(action, &mut st), "{action} at {st:?}");
        }
        assert!(
            !buggy.check_invariant(invariant, &st),
            "{invariant}: {st:?}"
        );
        // The fixed loop cannot take that path's last step.
        let mut fixed = model.init_state();
        for action in &path[..2] {
            assert!(model.fire(action, &mut fixed), "{action} at {fixed:?}");
        }
        assert!(
            !model.action_enabled(path[2], &fixed),
            "{invariant}: {fixed:?}"
        );
    }
}

/// THE QUESTION ANSWER (R3c of the critique of 2026-09-25): with a person's
/// quiet and the progression rule, every key the supervisor writes is the
/// next one Claude Code reads and meets the dialog as the read showed it —
/// proven; with the screen generation fence alone (`Buggy = 1`), a key
/// crosses one still in flight and is caught. Proven at every retry bound
/// the loop runs under (P3): one retry (`supervise`'s one look), and the
/// unattended loop's tries for as long as the dialog stands, checked to two
/// (committed) and three — each try guarded alike, so the next bound adds
/// no new way for a key to land.
#[test]
fn the_question_answer_proves_and_catches_a_key_that_meets_a_state_it_was_not_decided_on() {
    let m = aterm_spec::derive::supervisor_question_answer_model();
    aterm_spec::verify::prove_and_catch_scalar(
        &m,
        "supervisor question answer: a key meets the state it was decided on",
    );
    for bound in [1, 3] {
        aterm_spec::verify::prove_and_catch_scalar(
            &aterm_spec::interp::with_consts(&m, &[("MaxRetry", bound)]),
            "supervisor question answer under another retry bound",
        );
    }
}

/// `m` with only the invariant `name`: which of them a variant refutes.
fn only(m: &aterm_spec::derive::Model, name: &str) -> aterm_spec::derive::Model {
    let mut m = m.clone();
    m.invariants.retain(|i| i.name == name);
    assert_eq!(m.invariants.len(), 1, "no invariant {name}");
    m
}

/// Whether the variant of `m` with `consts` reaches a state that violates
/// `name` (the interpreter's exhaustive walk).
fn refutes(m: &aterm_spec::derive::Model, consts: &[(&str, i64)], name: &str) -> bool {
    let v = aterm_spec::interp::with_consts(&only(m, name), consts);
    aterm_spec::interp::bmc(&v).is_err()
}

const QUESTION_INVARIANTS: [&str; 4] = [
    "NoDigitInTheField",
    "NeverAnswersANextTabWithAStaleKey",
    "NeverCancelsTheReview",
    "NoKeyIntoTheComposer",
];

/// Non-vacuity, invariant by invariant: the fence alone (`Buggy = 1`)
/// refutes EACH of the four — a person's `↓` ahead of the Enter puts it in
/// the free-text field or on the review's cancel, and a second Enter behind
/// the first answers the next tab or lands in the composer — so no one of
/// them is proven only because it can never be broken.
#[test]
fn the_fence_alone_refutes_every_question_invariant() {
    let m = aterm_spec::derive::supervisor_question_answer_model();
    for name in QUESTION_INVARIANTS {
        assert!(
            !refutes(&m, &[], name),
            "{name} fails with the guards (Buggy = 0)"
        );
        assert!(
            refutes(&m, &[("Buggy", 1)], name),
            "{name} is not refuted by the fence alone: vacuous"
        );
    }
}

/// The person fence and the model's one ASSUMPTION are load-bearing, and it
/// says so: without the fence (`PersonFence = 0`, a host that sends no
/// person count — the quiet taken from the READ and not re-checked at the
/// write, the round trip `key if-human=` closes, R3b), a person's key
/// crosses the Enter into the field and onto the review's cancel; and a
/// retry written while the first key is still unread (`TakenBeforeStill =
/// 0`) answers the next tab with it (and a retried `↑` still in flight
/// moves the focus off the option the next Enter was decided on).
#[test]
fn the_question_answer_rests_on_the_person_fence_and_one_assumption() {
    let m = aterm_spec::derive::supervisor_question_answer_model();
    for name in ["NoDigitInTheField", "NeverCancelsTheReview"] {
        assert!(
            refutes(&m, &[("PersonFence", 0)], name),
            "{name} holds with no person fence at the write"
        );
    }
    assert!(
        refutes(
            &m,
            &[("TakenBeforeStill", 0)],
            "NeverAnswersANextTabWithAStaleKey"
        ),
        "the retry is safe with its key still unread"
    );
}

/// The guards leave the supervisor able to act — a model that never keys
/// would prove everything: alone with the dialog it answers every tab and
/// submits the review, moves off a row a person left the focus on once
/// they are quiet, and a key the dialog refused is sent once more after the
/// screen held still.
#[test]
fn the_question_answer_answers_every_tab_and_submits() {
    let m = aterm_spec::derive::supervisor_question_answer_model();
    let fire = |st: &mut std::collections::BTreeMap<&'static str, i64>, a: &str| {
        assert!(m.fire(a, st), "{a} is not enabled at {st:?}");
    };
    let mut st = m.init_state();
    // Tab 0: a key refused, the screen still, the Enter again and taken.
    for a in [
        "Read",
        "HarnessEnter",
        "KeyRefused",
        "Read",
        "StillScreen",
        "Read",
        "HarnessEnter",
        "EnterTaken",
    ] {
        fire(&mut st, a);
    }
    // Tab 1: a person moves the focus off the chosen option; once quiet,
    // the supervisor moves it back and answers.
    for a in [
        "PersonKey",
        "PersonMovesOff",
        "Read",
        "QuietReturns",
        "Read",
        "HarnessMove",
        "MoveLands",
        "Read",
        "HarnessEnter",
        "EnterTaken",
    ] {
        fire(&mut st, a);
    }
    // The review, submitted.
    for a in ["Read", "HarnessEnter", "EnterTaken"] {
        fire(&mut st, a);
    }
    assert_eq!(st["tab"], 3, "the dialog closed: {st:?}");
    for name in QUESTION_INVARIANTS {
        assert!(m.check_invariant(name, &st), "{name} at {st:?}");
    }
    // A box that takes no key (P3): the unattended loop keys it again each
    // time the screen held still — a third key is sent where the one look's
    // bound (`MaxRetry = 1`, its hand-over) sends none — and never before
    // the screen held still after the last one.
    let refused_twice = [
        "Read",
        "HarnessEnter",
        "KeyRefused",
        "Read",
        "StillScreen",
        "Read",
        "HarnessEnter",
        "KeyRefused",
        "Read",
    ];
    let one_look = aterm_spec::interp::with_consts(&m, &[("MaxRetry", 1)]);
    let mut st = m.init_state();
    for a in refused_twice {
        fire(&mut st, a);
    }
    assert!(
        !m.action_enabled("HarnessEnter", &st),
        "not before the screen held still: {st:?}"
    );
    assert!(one_look.fire("StillScreen", &mut st), "{st:?}");
    assert!(one_look.fire("Read", &mut st), "{st:?}");
    assert!(
        !one_look.action_enabled("HarnessEnter", &st),
        "the one look tries once: {st:?}"
    );
    assert!(
        m.action_enabled("HarnessEnter", &st),
        "the loop tries again: {st:?}"
    );
    fire(&mut st, "HarnessEnter");
    assert_eq!(st["retried"], 2, "{st:?}");
    for name in QUESTION_INVARIANTS {
        assert!(m.check_invariant(name, &st), "{name} at {st:?}");
    }
}

/// The two shapes the Tier-1 bind of the review of 2026-09-25 drives
/// through: a tab that OPENS with its focus off the chosen row
/// (`OpensOffChoice`: S2-01 draws the focus on option 1 with option 2
/// recommended; a multi-select's next choice after a toggle) is moved onto
/// it — by `↓` as much as `↑`: `HarnessMove` has no direction — and
/// answered; and a lone single-select question (`Review = 0`: no Submit
/// tab) closes on its answer. The four invariants hold for the lone
/// question too, and the fence alone still breaks it.
#[test]
fn a_tab_that_opens_off_the_choice_and_a_lone_question_are_answered_under_the_same_proof() {
    let m = aterm_spec::derive::supervisor_question_answer_model();
    let fire = |m: &aterm_spec::derive::Model,
                st: &mut std::collections::BTreeMap<&'static str, i64>,
                a: &str| {
        assert!(m.fire(a, st), "{a} is not enabled at {st:?}");
    };
    let mut st = m.init_state();
    for a in [
        "OpensOffChoice",
        "Read",
        "HarnessMove",
        "MoveLands",
        "Read",
        "HarnessEnter",
        "EnterTaken",
    ] {
        fire(&m, &mut st, a);
    }
    assert_eq!((st["tab"], st["focus"]), (1, 0), "{st:?}");
    // Once read, a tab opens no more.
    fire(&m, &mut st, "Read");
    assert!(!m.action_enabled("OpensOffChoice", &st), "{st:?}");

    let lone = aterm_spec::interp::with_consts(&m, &[("Tabs", 1), ("Review", 0)]);
    let mut st = lone.init_state();
    for a in [
        "OpensOffChoice",
        "Read",
        "HarnessMove",
        "MoveLands",
        "Read",
        "HarnessEnter",
        "EnterTaken",
    ] {
        fire(&lone, &mut st, a);
    }
    assert_eq!(st["tab"], 2, "the answer closed the dialog: {st:?}");
    for name in QUESTION_INVARIANTS {
        assert!(
            !refutes(&m, &[("Tabs", 1), ("Review", 0)], name),
            "{name} fails for a lone question"
        );
    }
    assert!(
        refutes(
            &m,
            &[("Tabs", 1), ("Review", 0), ("Buggy", 1)],
            "NoDigitInTheField"
        ),
        "the fence alone keys a lone question's free-text field"
    );
}

// ---- the decline's keystrokes (the review of 2026-09-25, F1 and F3) ------

const DECLINE_INVARIANTS: [&str; 5] = [
    "NeverChoosesTheAllow",
    "NeverAmendsTheAllow",
    "NeverTheBareNo",
    "NeverSubmitsAnotherText",
    "NoKeyIntoTheComposer",
];

/// THE DECLINE'S KEYSTROKES: with one keystroke in flight and a person's
/// quiet, every key the supervisor writes meets the box as the read it was
/// decided on showed it — proven; with the screen generation fence alone
/// (`Buggy = 1`, the port as first shipped) a second Tab behind one not yet
/// drawn shuts the input, and the reason typed after it lands on the closed
/// Select, whose `1` chooses the allow — caught.
#[test]
fn the_decline_proves_and_catches_a_keystroke_that_meets_a_state_it_was_not_decided_on() {
    aterm_spec::verify::prove_and_catch_scalar(
        &aterm_spec::derive::supervisor_decline_keys_model(),
        "supervisor decline: a keystroke meets the state it was decided on",
    );
}

/// Non-vacuity, invariant by invariant: the fence alone refutes EACH of the
/// five (a resent `↓` wraps onto `1. Yes` and the Tab behind it amends the
/// allow; a resent Tab shuts the input under the reason, whose `1` chooses
/// the allow; a person's Tab in flight shuts it under the Enter — the bare
/// `No`; a resent reason doubles the text the Enter behind it submits; a
/// resent Enter is read after the box left), and the guards hold each.
#[test]
fn the_fence_alone_refutes_every_decline_invariant() {
    let m = aterm_spec::derive::supervisor_decline_keys_model();
    for name in DECLINE_INVARIANTS {
        assert!(
            !refutes(&m, &[], name),
            "{name} fails with the guards (Buggy = 0)"
        );
        assert!(
            refutes(&m, &[("Buggy", 1)], name),
            "{name} is not refuted by the fence alone: vacuous"
        );
    }
}

/// The person fence is load-bearing, and the model says so: without it
/// (`PersonFence = 0`, a host that sends no person count — the quiet taken
/// from the READ and not re-checked at the write), a person's Tab written in
/// that round trip shuts the input under the reason.
#[test]
fn the_decline_rests_on_the_person_fence_at_the_write() {
    let m = aterm_spec::derive::supervisor_decline_keys_model();
    assert!(refutes(&m, &[("PersonFence", 0)], "NeverChoosesTheAllow"));
}

/// The guards leave the supervisor able to act — a model that never keys
/// would prove everything: alone with the box it declines it in four
/// keystrokes; a Tab not drawn yet is waited for (never written again) and
/// the reason goes once it shows; a person's key waits for their quiet; and
/// a keystroke the box dropped is never written a second time.
#[test]
fn the_decline_is_carried_out_and_a_keystroke_is_never_written_twice() {
    let m = aterm_spec::derive::supervisor_decline_keys_model();
    let fire = |st: &mut std::collections::BTreeMap<&'static str, i64>, a: &str| {
        assert!(m.fire(a, st), "{a} is not enabled at {st:?}");
    };
    let mut st = m.init_state();
    for a in [
        "Read",
        "HarnessDown",
        "DownTaken",
        "Read",
        "HarnessTab",
        // The Tab not drawn yet: the read shows the box as keyed.
        "Read",
    ] {
        fire(&mut st, a);
    }
    assert!(!m.action_enabled("HarnessTab", &st), "resent: {st:?}");
    for a in [
        "TabTaken",
        "Read",
        "HarnessText",
        "TextIntoTheInput",
        "Read",
        "HarnessEnter",
        "EnterSubmitsTheReason",
    ] {
        fire(&mut st, a);
    }
    assert_eq!(st["gone"], 1, "declined with the reason: {st:?}");
    for name in DECLINE_INVARIANTS {
        assert!(m.check_invariant(name, &st), "{name} at {st:?}");
    }

    // A person keys: nothing is written until they are quiet.
    let mut st = m.init_state();
    for a in ["Read", "PersonKey", "PersonKeyIgnored"] {
        fire(&mut st, a);
    }
    assert!(!m.action_enabled("HarnessDown", &st), "{st:?}");
    for a in ["QuietReturns", "Read", "HarnessDown"] {
        fire(&mut st, a);
    }

    // A keystroke the box dropped is never written again.
    let mut st = m.init_state();
    for a in ["Read", "HarnessDown", "KeyRefused", "Read", "Read"] {
        fire(&mut st, a);
    }
    assert!(!m.action_enabled("HarnessDown", &st), "{st:?}");
}

/// THE NETWORK WALL (the outage of 2026-09-27): a measured outage typed
/// into at most once a hold of wall time, and at most two acts at once per
/// episode, however the measure flaps, lies or is lost — proven; the
/// supervisor of that day (`Buggy = 1`: at once at every appearance,
/// whatever the measure) is caught on both. And the first is no restatement
/// of `Act`'s guard (the review of 2026-09-27): `Carry = 1` — the hold's
/// clock carried across an episode's appearances, the guard untouched — is
/// caught by it too. The machine stays enrolled in the spec-link registry,
/// so the non-vacuity sweep keeps both invariants falsifiable.
#[test]
fn the_network_wall_proves_and_catches_typing_into_a_measured_outage() {
    let m = aterm_spec::derive::supervisor_network_wall_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == m.name),
        "the network wall must stay enrolled in the spec-link registry"
    );
    aterm_spec::verify::prove_and_catch_scalar(
        &m,
        "supervisor network wall: a measured outage typed into at most once a hold; at most \
         two acts at once per episode",
    );
    // The hold measured from the episode's first appearance, not each one:
    // after one hold, the outage's next appearance is typed into at once —
    // with `Act`'s guard unchanged, so only an invariant over what the
    // episode accumulates can see it.
    let carry = aterm_spec::interp::with_consts(&m, &[("Carry", 1)]);
    let (at, broken) = aterm_spec::interp::bmc(&carry).expect_err("Carry = 1 is caught");
    assert_eq!(
        broken, "AMeasuredOutageIsTypedIntoAtMostOnceAHold",
        "{at:?}"
    );
    assert_eq!(at["dacts"], 2, "the second act into the outage: {at:?}");

    // The committed decision, step by step. The network fails and the host
    // measures it down: the wall's appearance waits the whole hold, then one
    // try all the same.
    let hold = m.consts.iter().find(|c| c.0 == "Hold").unwrap().1;
    let mut s = m.init_state();
    for a in ["NetFails", "MeasureDown", "Unreachable"] {
        assert!(m.fire(a, &mut s), "{a} at {s:?}");
    }
    for _ in 0..hold {
        assert!(!m.action_enabled("Act", &s), "{s:?}");
        assert!(m.fire("Tick", &mut s));
    }
    assert!(m.fire("Act", &mut s), "one try at the hold: {s:?}");
    // It meets the wall again, and the probe LIES — up while the agent's
    // route is down: continued at once, the once; met again, the next Up
    // act waits a rung, and a flap through Down does not buy another.
    for a in ["MetUnreachable", "MeasureLies"] {
        assert!(m.fire(a, &mut s), "{a} at {s:?}");
    }
    assert!(m.fire("Act", &mut s), "at once on the measure: {s:?}");
    assert!(m.fire("MetUnreachable", &mut s));
    assert!(!m.action_enabled("Act", &s), "a second Up act waits: {s:?}");
    for a in ["MeasureDown", "MeasureLies"] {
        assert!(m.fire(a, &mut s), "{a} at {s:?}");
    }
    assert!(!m.action_enabled("Act", &s), "the flap is spaced: {s:?}");
    // The API truly back while the wall shows: continued at once.
    let mut up = m.init_state();
    for a in [
        "NetFails",
        "MeasureDown",
        "Unreachable",
        "NetReturns",
        "MeasureUp",
    ] {
        assert!(m.fire(a, &mut up), "{a} at {up:?}");
    }
    assert!(m.action_enabled("Act", &up), "{up:?}");
    // A reply cut off after real work is a new episode: continued at once.
    let mut c = m.init_state();
    for a in ["CutOff", "Act", "WorkedThenCutOff"] {
        assert!(m.fire(a, &mut c), "{a} at {c:?}");
    }
    assert!(m.action_enabled("Act", &c), "{c:?}");

    // The day's supervisor, caught twice.
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut b = buggy.init_state();
    for a in ["NetFails", "MeasureDown", "Unreachable", "Act"] {
        assert!(buggy.fire(a, &mut b), "{a} at {b:?}");
    }
    assert!(!buggy.check_invariant("AMeasuredOutageIsTypedIntoAtMostOnceAHold", &b));
    let mut q = buggy.init_state();
    assert!(buggy.fire("NetFails", &mut q) && buggy.fire("Unreachable", &mut q));
    for _ in 0..3 {
        assert!(buggy.fire("Act", &mut q) && buggy.fire("MetUnreachable", &mut q));
    }
    assert!(!buggy.check_invariant("AtMostTwoActsAtOncePerEpisode", &q));
}

/// CODEX'S RATE-LIMIT NUDGE AND THE SAVE-THEN-WAIT SWITCH (2026-09-28): the
/// seven laws prove at `Buggy = 0` and each day's defect is caught at `Buggy =
/// 1`; the session comes back to its own model under the fairness the claim
/// names, each assumption load-bearing, and each mutant breaks it alone. The
/// incident, step by step: the goal's turn armed the nudge near the limit, it
/// showed at the turn's end with the next goal turn already running under
/// it; the switch leaves that turn running and the harness stops it and the
/// goal; no continuation is possible while switched; a goal that escapes its
/// stop is stopped again, and past the stops a person is told; nothing is
/// typed into a fallen sandbox; the wind-down is typed once; its end brings
/// the model back; the hold waits for the window; the reset resumes the
/// goal. And the 03:30Z press, at a far reading, is a keep; and the save's own
/// turn, past its bound, is stopped (the round-4 re-review).
#[test]
fn the_codex_rate_nudge_saves_the_work_and_comes_back() {
    let m = aterm_spec::derive::supervisor_codex_rate_nudge_model();
    let live = aterm_spec::derive::supervisor_codex_rate_nudge_liveness();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == m.name),
        "the switch must stay enrolled in the spec-link registry"
    );
    assert!(
        aterm_spec::xref::liveness_registry()
            .iter()
            .any(|(registered, l)| registered.name == m.name && l.name == live.name),
        "its liveness claim must stay enrolled"
    );
    aterm_spec::verify::prove_and_catch_scalar(
        &m,
        "codex rate nudge: no work while switched, no person's turn stopped, one wind-down per \
         switch, held on the original, switched only near the limit, nothing into a sandbox or \
         a pursued goal",
    );
    aterm_spec::verify::liveness_proves_and_catches_tiered(
        &m,
        &live,
        "codex rate nudge: the session comes back to its own model",
    );

    // The incident, the owner's way.
    let mut s = m.init_state();
    for a in ["GoalTurn", "Pend", "TurnEnds", "GoalTurn"] {
        assert!(m.fire(a, &mut s), "{a} at {s:?}");
    }
    assert_eq!((s["shown"], s["turn"], s["goal"]), (1, 1, 1), "{s:?}");
    assert!(m.fire("PressSwitch", &mut s), "{s:?}");
    assert_eq!(
        (s["run"], s["turn"], s["model"], s["ph"]),
        (1, 1, 1, 1),
        "the goal turn under the box runs on, Codex's own: {s:?}"
    );
    assert!(
        !m.action_enabled("TurnEnds", &s),
        "it is stopped before it ends"
    );
    assert!(m.fire("StopGoal", &mut s), "{s:?}");
    assert_eq!(
        (
            s["goal"],
            s["paused"],
            s["turn"],
            s["model"],
            s["ph"],
            s["stops"]
        ),
        (0, 1, 0, 1, 1, 1),
        "the goal turn stopped, the goal paused, owed: {s:?}"
    );
    assert!(!m.action_enabled("Continue", &s), "never `keep going` here");
    assert!(!m.action_enabled("GoalTurn", &s), "nor Codex's goal");
    // The goal escapes its stop: stopped again, up to `Stops`, then told.
    let mut esc = s.clone();
    for k in 2..=3 {
        for a in ["GoalEscapes", "GoalTurn", "StopGoal"] {
            assert!(m.fire(a, &mut esc), "{a} ({k}) at {esc:?}");
        }
    }
    for a in ["GoalEscapes", "GoalTurn"] {
        assert!(m.fire(a, &mut esc), "{a} at {esc:?}");
    }
    assert!(!m.action_enabled("StopGoal", &esc), "the stops are spent");
    assert!(!m.action_enabled("TurnEnds", &esc), "not ended untold");
    assert!(
        m.fire("Tell", &mut esc) && m.fire("TurnEnds", &mut esc),
        "{esc:?}"
    );
    assert_eq!((esc["told"], esc["work"]), (1, 0), "told, never silent");
    // Nothing of the switch into a fallen sandbox.
    let mut sand = s.clone();
    assert!(m.fire("SandboxFalls", &mut sand));
    assert!(
        !m.action_enabled("TypeWindDown", &sand),
        "no save into a sandbox"
    );
    for a in ["TypeWindDown", "TurnEnds", "Restore"] {
        assert!(m.fire(a, &mut s), "{a} at {s:?}");
    }
    assert_eq!(
        (s["ph"], s["model"]),
        (4, 0),
        "held on its own model: {s:?}"
    );
    assert!(!m.action_enabled("Resume", &s), "until the window resets");
    // A person's `/goal resume` during the hold: the goal's turn is theirs —
    // never stopped, however long it runs — and its end releases the hold.
    let mut theirs = s.clone();
    assert!(m.fire("PersonResumes", &mut theirs), "{theirs:?}");
    assert!(
        !m.action_enabled("StopGoal", &theirs),
        "never a person's turn"
    );
    assert!(!m.action_enabled("Tell", &theirs));
    assert!(m.fire("TurnEnds", &mut theirs), "{theirs:?}");
    assert_eq!(
        (theirs["ph"], theirs["paused"], theirs["model"]),
        (0, 0, 0),
        "released on its own model: {theirs:?}"
    );
    // The re-review's defect: the Esc into it once the grace ran out.
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut stopped = s.clone();
    assert!(buggy.fire("PersonResumes", &mut stopped));
    assert!(buggy.fire("StopPersonsTurn", &mut stopped));
    assert!(!buggy.check_invariant("NoPersonsTurnStopped", &stopped));
    assert!(m.fire("Far", &mut s) && m.fire("Resume", &mut s), "{s:?}");
    assert_eq!(
        (s["ph"], s["goal"], s["paused"]),
        (0, 1, 0),
        "`/goal resume`: {s:?}"
    );

    // The 03:30Z nudge, at 1%: kept.
    let mut far = m.init_state();
    for a in ["GoalTurn", "Pend", "TurnEnds", "Far"] {
        assert!(m.fire(a, &mut far), "{a} at {far:?}");
    }
    assert!(!m.action_enabled("PressSwitch", &far));
    assert!(m.action_enabled("PressKeep", &far));

    // The day's supervisor: switched at 1%, and `keep going` into the goal.
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut b = far.clone();
    assert!(buggy.fire("PressAtFar", &mut b));
    assert!(!buggy.check_invariant("SwitchedOnlyNearTheLimit", &b));
    let mut g = buggy.init_state();
    assert!(buggy.fire("ContinueUnderGoal", &mut g));
    assert!(!buggy.check_invariant("NoContinuationIntoAPursuedGoal", &g));
    // The save typed into a fallen sandbox, and a goal left to run on.
    assert!(buggy.fire("WindDownInSandbox", &mut sand));
    assert!(!buggy.check_invariant("NothingTypedIntoASandbox", &sand));
    let mut kept = m.init_state();
    for a in ["GoalTurn", "Pend", "TurnEnds", "GoalTurn", "PressSwitch"] {
        assert!(buggy.fire(a, &mut kept), "{a} at {kept:?}");
    }
    assert!(buggy.fire("SwitchKeepsGoal", &mut kept));
    assert!(!buggy.check_invariant("NoUntoldWorkWhileSwitched", &kept));

    // The round-3 re-review: a PERSON'S message began the turn the nudge's
    // box covered; Codex's goal starts its own turn under the box, and the
    // switch is pressed. That goal turn is none of theirs — stopped.
    let mut covered = m.init_state();
    for a in ["PersonTurn", "Pend", "TurnEnds"] {
        assert!(m.fire(a, &mut covered), "{a} at {covered:?}");
    }
    assert_eq!((covered["shown"], covered["cov"]), (1, 1), "{covered:?}");
    let mut escaped = covered.clone();
    for a in ["GoalTurn", "PressSwitch"] {
        assert!(m.fire(a, &mut escaped), "{a} at {escaped:?}");
    }
    assert_eq!(
        (escaped["person"], escaped["run"], escaped["cov"]),
        (0, 1, 1),
        "the goal turn under the box is Codex's own: {escaped:?}"
    );
    assert!(m.action_enabled("StopGoal", &escaped), "{escaped:?}");
    // The regression: the covered turn's hand spares it, untold.
    assert!(buggy.fire("CoveredHandSparesGoal", &mut escaped));
    assert!(!buggy.check_invariant("NoUntoldWorkWhileSwitched", &escaped));
    // The covered turn's end, answered with no goal turn under the box: its
    // hand is judged there, and nothing is left carried.
    let mut judged = covered.clone();
    assert!(m.fire("PressSwitch", &mut judged), "{judged:?}");
    assert_eq!(judged["cov"], 0, "{judged:?}");

    // The round-4 re-review: the save's OWN turn runs on the cheaper model
    // past its bound — the harness stops it with one Esc (none of the goal's
    // stops), and the switch goes on to the restore; it does not end by
    // itself before. The defect: it runs on, nobody stopping it.
    let mut wound = m.init_state();
    for a in [
        "GoalTurn",
        "Pend",
        "TurnEnds",
        "GoalTurn",
        "PressSwitch",
        "StopGoal",
        "TypeWindDown",
    ] {
        assert!(m.fire(a, &mut wound), "{a} at {wound:?}");
    }
    assert_eq!((wound["ph"], wound["turn"]), (2, 1), "{wound:?}");
    assert!(
        !m.action_enabled("StopWindDown", &wound),
        "within its bound: {wound:?}"
    );
    assert!(m.fire("WindDownRunsLong", &mut wound), "{wound:?}");
    assert!(
        !m.action_enabled("TurnEnds", &wound),
        "stopped before it ends: {wound:?}"
    );
    let mut runs_on = wound.clone();
    let stops = wound["stops"];
    assert!(m.fire("StopWindDown", &mut wound), "{wound:?}");
    assert_eq!(
        (wound["ph"], wound["turn"], wound["stops"], wound["work"]),
        (3, 0, stops, 0),
        "stopped, owed its model back: {wound:?}"
    );
    assert!(m.fire("Restore", &mut wound), "{wound:?}");
    assert_eq!((wound["ph"], wound["model"]), (4, 0), "held: {wound:?}");
    assert!(buggy.fire("WindDownRunsOn", &mut runs_on));
    assert!(!buggy.check_invariant("NoUntoldWorkWhileSwitched", &runs_on));
}

/// THE LOOP'S OWN ACT, ONCE THE AGENT HAS TAKEN IT, NEVER WITHHOLDS A BREAK
/// (2026-09-28: s-d3346 from 14:52:45 and s-5c03a from 18:21:04 had no look of
/// the live upgrade's for hours behind the loop's own `keep going`). Proved at
/// `Buggy = 0`, caught at `Buggy = 1`; each dial caught alone by the bounded
/// check and on its own path — `Judged = 1` (0.98's guard: the act's point
/// judged) withholds the break after a live turn, `Blind = 1` stacks the
/// host's line on an act the agent has not taken.
#[test]
fn a_taken_act_never_withholds_a_break_and_an_untaken_one_is_never_stacked_on() {
    use aterm_spec::{derive::supervisor_break_offer_model, interp, verify};
    let model = supervisor_break_offer_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the break-offer machine must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "supervisor break offer: taken, not judged");

    let walk = |m: &aterm_spec::derive::Model, path: &[&str]| {
        let mut s = m.init_state();
        for action in path {
            assert!(m.fire(action, &mut s), "{action} at {s:?}");
        }
        s
    };
    let taken = ["LoopActs", "LiveTurn", "BreakSettles"];
    let untaken = ["LoopActs", "BreakSettles"];

    // Fixed: a break after a live turn is offered, one straight after the act
    // is not.
    let s = walk(&model, &taken);
    assert!(model.action_enabled("Offer", &s), "{s:?}");
    assert!(!model.action_enabled("Withhold", &s), "{s:?}");
    let s = walk(&model, &untaken);
    assert!(!model.action_enabled("Offer", &s), "{s:?}");
    assert!(model.action_enabled("Withhold", &s), "{s:?}");

    // NEGATIVE CONTROL: 0.98's guard withholds the break the act led to.
    let judged = interp::with_consts(&model, &[("Judged", 1)]);
    let mut j = walk(&judged, &taken);
    assert!(!judged.action_enabled("Offer", &j), "{j:?}");
    assert!(judged.fire("Withhold", &mut j));
    assert!(
        !judged.check_invariant("NoBreakWithheldAfterTheActWasTaken", &j),
        "{j:?}"
    );
    assert!(interp::bmc(&judged).is_err(), "caught by the bounded check");

    // NEGATIVE CONTROL: a guard that asks nothing stacks on an untaken act.
    let blind = interp::with_consts(&model, &[("Blind", 1)]);
    let mut b = walk(&blind, &untaken);
    assert!(blind.fire("Offer", &mut b));
    assert!(
        !blind.check_invariant("NeverStackOnAnUntakenAct", &b),
        "{b:?}"
    );
    assert!(interp::bmc(&blind).is_err(), "caught by the bounded check");
}
