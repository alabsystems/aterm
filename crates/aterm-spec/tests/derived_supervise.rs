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

/// The model's two ASSUMPTIONS are load-bearing, and it says so: taken from
/// the READ (the quiet not re-checked at the write — the round trip the
/// deferred `key if-input=` fence, R3b, would close), a person's key crosses
/// the Enter into the field and onto the review's cancel; and a retry
/// written while the first key is still unread (`TakenBeforeStill = 0`)
/// answers the next tab with it (and a retried `↑` still in flight moves
/// the focus off the option the next Enter was decided on).
#[test]
fn the_question_answer_rests_on_two_assumptions_each_refuted_without_it() {
    let m = aterm_spec::derive::supervisor_question_answer_model();
    for name in ["NoDigitInTheField", "NeverCancelsTheReview"] {
        assert!(
            refutes(&m, &[("QuietAtWrite", 0)], name),
            "{name} holds with the quiet read before the write"
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

/// The model's ASSUMPTION is load-bearing, and it says so: the quiet taken
/// from the READ rather than at the write (`QuietAtWrite = 0`), a person's
/// Tab written in that round trip shuts the input under the reason.
#[test]
fn the_decline_rests_on_the_quiet_at_the_write() {
    let m = aterm_spec::derive::supervisor_decline_keys_model();
    assert!(refutes(&m, &[("QuietAtWrite", 0)], "NeverChoosesTheAllow"));
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
