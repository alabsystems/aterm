// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for the supervisor's machines (aterm-agent `supervise`): each
//! proves at `Buggy = 0` and yields its counterexample at `Buggy = 1`. The
//! Tier-1 binds drive the real `Session` (aterm-agent
//! `supervise/run_engine_tests.rs`), and the question answer's pure decider
//! too (`supervise/policy/question_tests.rs`).

use crate::walk::only;

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

/// The variant of `m` with `consts` — every one of which `m` must declare:
/// `with_consts` ignores a name it does not know, so a misspelt dial would
/// otherwise check the committed model and pass for the wrong reason.
fn at(m: &aterm_spec::derive::Model, consts: &[(&str, i64)]) -> aterm_spec::derive::Model {
    for (name, _) in consts {
        assert!(
            m.consts.iter().any(|(n, _)| n == name),
            "{} declares no constant {name}",
            m.name
        );
    }
    aterm_spec::interp::with_consts(m, consts)
}

/// Whether the variant of `m` with `consts` reaches a state that violates
/// `name` (the interpreter's exhaustive walk).
fn refutes(m: &aterm_spec::derive::Model, consts: &[(&str, i64)], name: &str) -> bool {
    let v = at(&only(m, name), consts);
    aterm_spec::interp::bmc(&v).is_err()
}

/// The recommended path's four laws (`Text = 0`, as committed).
const QUESTION_INVARIANTS: [&str; 4] = [
    "NoDigitInTheField",
    "NeverAnswersANextTabWithAStaleKey",
    "NeverCancelsTheReview",
    "NoKeyIntoTheComposer",
];

/// The policy text's six (`Text = 1`): the four, and the two only a
/// written text can break — so they are checked where one is written.
const TEXT_INVARIANTS: [&str; 6] = [
    "NoDigitInTheField",
    "NeverAnswersANextTabWithAStaleKey",
    "NeverCancelsTheReview",
    "NoKeyIntoTheComposer",
    "OwnTextOnlyIntoItsEmptyField",
    "TextNeverWrittenTwice",
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

// ---- THE POLICY TEXT (`Text = 1`) --------------------------------------

type State = std::collections::BTreeMap<&'static str, i64>;

/// Every state the interpreter reaches in `m` (its whole bounded space).
fn reachable(m: &aterm_spec::derive::Model) -> Vec<State> {
    aterm_spec::interp::reachable_states(m)
}

/// Fire `actions` in order from `st`, each of which must be enabled, and
/// hold every one of `laws` after each.
fn run(m: &aterm_spec::derive::Model, st: &mut State, actions: &[&str], laws: &[&str]) {
    for a in actions {
        assert!(m.fire(a, st), "{a} is not enabled at {st:?}");
        for name in laws {
            assert!(m.check_invariant(name, st), "{name} after {a} at {st:?}");
        }
    }
}

/// The recommended path's model IS today's, which every Tier-1 bind of the
/// recommended path drives: the reachable-state counts measured on main's
/// own model, without the text path (origin/main f37ddc740 at the merge of
/// 2026-09-29, `MaxRetry = 2`, with the person fence's `touched`: 12989 at
/// the committed two tabs and a review, 3197 for a lone single-select, 6295
/// for a lone multi-select's shape with its review — 10404, 2484 and 4880
/// before the fence; 7680, 1832 and 3576 before main's P3 retries) are
/// unchanged. And the text path left out of it changes nothing reachable:
/// the whole machine at `Text = 0` (the text model with its dial down)
/// reaches exactly as many states, the text's variables — the per-tab
/// route, the memory of a text written, a person's copy of it among them —
/// stay 0 in every one, and none of the text's actions ever fires, not even
/// past the guards (`Buggy = 1`). So nothing about the committed model
/// changed, and the four laws' tests above keep their meaning.
#[test]
fn at_text_zero_the_question_model_is_todays() {
    let m = aterm_spec::derive::supervisor_question_answer_model();
    for (name, committed) in [
        ("MaxRetry", 2),
        ("PersonFence", 1),
        ("Text", 0),
        ("Multi", 0),
        ("OwnTextRemembered", 1),
        ("EnterWhereTyped", 0),
    ] {
        assert!(
            m.consts.contains(&(name, committed)),
            "{name} is not committed at {committed}: {:?}",
            m.consts
        );
    }
    let laws: Vec<&str> = m.invariants.iter().map(|i| i.name).collect();
    assert_eq!(
        laws, QUESTION_INVARIANTS,
        "the recommended path's four laws"
    );
    let whole = at(
        &aterm_spec::derive::supervisor_question_text_model(),
        &[("Text", 0)],
    );
    let text_actions: Vec<&str> = whole
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|a| !m.actions.iter().any(|b| b.name == *a))
        .collect();
    assert_eq!(
        text_actions,
        [
            "PersonTypesPolicyText",
            "TextLands",
            "TextPartly",
            "FitLostOnIt",
            "FitLostOffIt",
            "HarnessType"
        ],
        "the recommended path leaves out the text's actions and nothing else"
    );
    for (shape, today) in [
        (&[][..], 12989),
        (&[("Tabs", 1), ("Review", 0)][..], 3197),
        (&[("Tabs", 1), ("Review", 1)][..], 6295),
    ] {
        let v = at(&m, shape);
        assert_eq!(
            aterm_spec::interp::bmc(&v).map_err(|(st, inv)| format!("{inv} at {st:?}")),
            Ok(today),
            "{shape:?}: not today's reachable space"
        );
        let w = at(&whole, shape);
        let states = reachable(&w);
        assert_eq!(
            states.len(),
            today,
            "{shape:?}: the whole machine at Text = 0"
        );
        for st in &states {
            for var in [
                "own",
                "seen_own",
                "astray",
                "fallback",
                "seen_fallback",
                "wrote",
                "twice",
                "exact",
                "seen_exact",
            ] {
                assert_eq!(st[var], 0, "{shape:?}: {var} at {st:?}");
            }
        }
        for buggy in [0, 1] {
            let fired = aterm_spec::interp::fired_actions(&at(&w, &[("Buggy", buggy)]));
            for a in &text_actions {
                assert!(
                    !fired.contains(a),
                    "{shape:?}: {a} fires at Text = 0 (Buggy = {buggy})"
                );
            }
        }
    }
}

/// EACH INSTANCE HOLDS ONLY LAWS ITS CONSTANTS CAN BREAK, AND EVERY ACTION
/// FIRES (the review of the question-lean re-port): what the workspace's
/// sweeps ask of a model in `xref::model_registry()` — the non-vacuity
/// ratchet, that the fence alone (`Buggy = 1`) breaks every law
/// (`verify::uncaught_invariants` names none), and `ty check
/// --strict-vacuity`, that no action is dead at the committed constants
/// (its one exception, a dead action that alone is the mutant, is not this
/// model's shape: its mutant is the guards'). The recommended path is
/// enrolled; the policy text is held to the same here, single- and
/// multi-select, while its `Buggy = 1` space is past the sweep's budget.
/// NEGATIVE CONTROLS: the whole machine at `Text = 0` — the one model the
/// two paths were until the review — has two ghosts, the text's laws; and
/// the consume of a key read after the dialog closed, as an action of its
/// own (`KeyIntoTheComposer` until the review), never fires at the
/// committed constants.
#[test]
fn each_question_instance_holds_only_laws_its_constants_can_break() {
    let answer = aterm_spec::derive::supervisor_question_answer_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == answer.name),
        "the recommended path is enrolled in the registry"
    );
    let text = aterm_spec::derive::supervisor_question_text_model();
    assert_eq!(text.name, "SupervisorQuestionText");
    assert!(text.consts.contains(&("Text", 1)), "{:?}", text.consts);
    let laws: Vec<&str> = text.invariants.iter().map(|i| i.name).collect();
    assert_eq!(laws, TEXT_INVARIANTS, "the policy text's six laws");
    for (label, v) in [
        ("the recommended path", answer.clone()),
        ("the policy text, single-select", text.clone()),
        ("the policy text, multi-select", at(&text, &[("Multi", 1)])),
    ] {
        assert_eq!(
            aterm_spec::verify::uncaught_invariants(&v),
            Vec::<&str>::new(),
            "{label}: a law the fence alone cannot break"
        );
        let fired = aterm_spec::interp::fired_actions(&v);
        let dead: Vec<&str> = v
            .actions
            .iter()
            .map(|a| a.name)
            .filter(|a| !fired.contains(a))
            .collect();
        assert!(
            dead.is_empty(),
            "{label}: dead at the committed constants: {dead:?}"
        );
    }
    assert_eq!(
        aterm_spec::verify::uncaught_invariants(&at(&text, &[("Text", 0)])),
        ["OwnTextOnlyIntoItsEmptyField", "TextNeverWrittenTwice"],
        "the whole machine at Text = 0 carries the text's laws as ghosts"
    );
    use aterm_spec::derive::{Action, Expr, Update};
    let b = Box::new;
    let mut own_action = answer.clone();
    own_action.actions.push(Action {
        name: "KeyIntoTheComposer",
        guard: Some(Expr::And(
            b(Expr::Le(b(Expr::Int(2)), b(Expr::Var("q1")))),
            b(Expr::Le(
                b(Expr::Add(b(Expr::ConstRef("Tabs")), b(Expr::Int(1)))),
                b(Expr::Var("tab")),
            )),
        )),
        updates: vec![
            Update {
                var: "composer",
                expr: Expr::Int(1),
            },
            Update {
                var: "q1",
                expr: Expr::Var("q2"),
            },
            Update {
                var: "q2",
                expr: Expr::Int(0),
            },
        ],
    });
    assert!(
        !aterm_spec::interp::fired_actions(&own_action).contains("KeyIntoTheComposer"),
        "a key read after the close, as its own action, is dead where committed"
    );
    assert!(
        aterm_spec::interp::fired_actions(&at(&own_action, &[("Buggy", 1)]))
            .contains("KeyIntoTheComposer"),
        "and reached past the guards"
    );
}

/// The policy text proves all six laws with the guards, and the fence
/// alone (`Buggy = 1`) refutes EACH — so no one of them holds only because
/// nothing can break it: a person's key ahead of the text puts it off its
/// field, one ahead of the Enter on its own text puts the Enter in the
/// field or on the review's cancel, a second Enter behind the first
/// answers the next tab or reaches the composer, and a second text behind
/// the first is written on its tab. For both a single- and a multi-select,
/// at the committed two tabs and a review; a lone question (with and
/// without its review) proves all six too, and the fence alone still
/// breaks its field. The `Buggy` pair is re-proved by `ty` wherever
/// it is installed.
///
/// One test per select kind (`Multi` 0 and 1), each its own whole space.
fn text_mode_proves_and_the_fence_alone_refutes_each_invariant(multi: i64) {
    let m = aterm_spec::derive::supervisor_question_text_model();
    let text = at(&m, &[("Multi", multi)]);
    assert_eq!(text.invariants.len(), TEXT_INVARIANTS.len());
    assert!(
        aterm_spec::interp::bmc(&text).is_ok(),
        "Multi = {multi}: a law fails with the guards: {:?}",
        aterm_spec::interp::bmc(&text).err()
    );
    for name in TEXT_INVARIANTS {
        assert!(
            refutes(&text, &[("Buggy", 1)], name),
            "Multi = {multi}: {name} is not refuted by the fence alone: vacuous"
        );
    }
    for shape in [[("Tabs", 1), ("Review", 0)], [("Tabs", 1), ("Review", 1)]] {
        let lone = at(&text, &shape);
        assert!(
            aterm_spec::interp::bmc(&lone).is_ok(),
            "Multi = {multi}, {shape:?}: {:?}",
            aterm_spec::interp::bmc(&lone).err()
        );
        for name in ["OwnTextOnlyIntoItsEmptyField", "NoDigitInTheField"] {
            assert!(
                refutes(&lone, &[("Buggy", 1)], name),
                "Multi = {multi}, {shape:?}: the fence alone keeps {name}"
            );
        }
    }
    aterm_spec::verify::prove_and_catch_scalar(
        &text,
        &format!("supervisor question answer, the policy text (Multi = {multi})"),
    );
}

#[test]
fn text_mode_proves_and_the_fence_alone_refutes_each_invariant_on_a_single_select() {
    text_mode_proves_and_the_fence_alone_refutes_each_invariant(0);
}

#[test]
fn text_mode_proves_and_the_fence_alone_refutes_each_invariant_on_a_multi_select() {
    text_mode_proves_and_the_fence_alone_refutes_each_invariant(1);
}

/// What the text path rests on — the person fence and its assumptions —
/// each refuted without it:
///
/// * `OwnTextRemembered = 0` — a loop with no memory of the text it wrote
///   takes a person's exact copy of it for its own and presses Enter on it
///   (single- and multi-select alike; that it needs that copy is
///   `the_memory_dial_is_refuted_only_through_a_persons_copy_of_the_text`);
/// * `PersonFence = 0` — a host that sends no person count, no `send
///   if-human=` at the write: a person's key written after the read puts
///   the text off its field (why decision 12 answers such a window's dialog
///   with its recommended option);
/// * `TakenBeforeStill = 0` — the move or Enter retry sent with its first
///   key unread answers the next tab with a stale key;
/// * and the multi-select Enter hazard as a dial: `EnterWhereTyped = 1`
///   (Enter where the text was typed) puts an Enter in a multi-select's
///   field (c3c: it unticks the row) with NO person acting — the model
///   with every `Person*` action removed still breaks it — while on a
///   single-select, whose field is the chosen row, the same dial needs a
///   person to break anything.
#[test]
fn text_mode_rests_on_the_person_fence_and_its_assumptions() {
    let m = aterm_spec::derive::supervisor_question_text_model();
    for multi in [0, 1] {
        let text = at(&m, &[("Multi", multi)]);
        assert!(
            refutes(&text, &[("OwnTextRemembered", 0)], "NoDigitInTheField"),
            "Multi = {multi}: a person's copy of the text is safe without the memory"
        );
    }
    let text = at(&m, &[]);
    assert!(
        refutes(&text, &[("PersonFence", 0)], "OwnTextOnlyIntoItsEmptyField"),
        "the text is safe with no person fence at the write"
    );
    assert!(
        refutes(
            &text,
            &[("TakenBeforeStill", 0)],
            "NeverAnswersANextTabWithAStaleKey"
        ),
        "the retry is safe with its key still unread"
    );
    let no_person = |multi: i64| {
        let mut v = at(&m, &[("Multi", multi), ("EnterWhereTyped", 1)]);
        let all = v.actions.len();
        v.actions.retain(|a| !a.name.starts_with("Person"));
        assert_eq!(all - v.actions.len(), 7, "the seven Person* actions");
        v
    };
    assert!(
        refutes(&no_person(1), &[], "NoDigitInTheField"),
        "an Enter where a multi-select's text was typed needs a person to go wrong"
    );
    assert!(
        !refutes(&no_person(0), &[], "NoDigitInTheField"),
        "an Enter on a single-select's own text went wrong with no person"
    );
    assert!(
        refutes(
            &at(&m, &[("Multi", 0)]),
            &[("EnterWhereTyped", 1)],
            "NoDigitInTheField"
        ),
        "on a single-select, Enter wherever own text was read is safe even \
         after a person moved the focus off it"
    );
}

/// THE PERSON FENCE ON THE TEXT (main's `send if-human=`): the loop has
/// moved onto a tab's empty free-text row and read it, a person keys (a key
/// Claude Code takes as nothing on the screen) and is quiet again before
/// the loop writes — every stamp the loop could ask says quiet, yet the
/// count moved past the read's (`touched`). No text, no Enter and no move
/// is enabled until a fresh read; then the text is. For both a single- and
/// a multi-select. NEGATIVE CONTROL: without the fence (`PersonFence = 0`,
/// a host that sends no count) the text is enabled at the very state the
/// fence holds it at — a count, not an age: the person's quiet has
/// returned.
#[test]
fn text_mode_writes_nothing_after_a_persons_key_until_it_reads_again() {
    let m = aterm_spec::derive::supervisor_question_text_model();
    for multi in [0, 1] {
        let v = at(&m, &[("Multi", multi)]);
        let mut st = v.init_state();
        run(
            &v,
            &mut st,
            &["OpensOffChoice", "Read", "HarnessMove", "MoveLands", "Read"],
            &TEXT_INVARIANTS,
        );
        assert!(
            v.action_enabled("HarnessType", &st),
            "Multi = {multi}: PRECONDITION: the text is due: {st:?}"
        );
        run(
            &v,
            &mut st,
            &["PersonKey", "PersonKeyIgnored", "QuietReturns"],
            &TEXT_INVARIANTS,
        );
        assert_eq!(
            (st["quiet"], st["touched"]),
            (1, 1),
            "Multi = {multi}: {st:?}"
        );
        for a in ["HarnessType", "HarnessEnter", "HarnessMove"] {
            assert!(
                !v.action_enabled(a, &st),
                "Multi = {multi}: {a} past a person's key: {st:?}"
            );
        }
        let unfenced = at(&v, &[("PersonFence", 0)]);
        assert!(
            unfenced.action_enabled("HarnessType", &st),
            "Multi = {multi}: without the fence the text goes at {st:?}"
        );
        run(&v, &mut st, &["Read"], &TEXT_INVARIANTS);
        assert!(
            v.action_enabled("HarnessType", &st),
            "Multi = {multi}: the fresh read lets the text go: {st:?}"
        );
    }
}

/// The guards leave the text path able to act — a model that never types
/// would prove everything: alone with the dialog it moves onto each tab's
/// free-text row, writes the text, sees it land as its own and presses
/// Enter — on a multi-select first moving off the field onto the button —
/// then submits the review; a lone single-select closes on its Enter. And
/// a text is never written twice: one the dialog dropped, the screen then
/// still, leaves no key to send on that box; nor does one whose head was
/// dropped.
#[test]
fn text_mode_answers_every_tab_and_never_types_twice() {
    let m = aterm_spec::derive::supervisor_question_text_model();
    let single = at(&m, &[("Multi", 0)]);
    let multi = at(&m, &[("Multi", 1)]);
    let onto_the_field = ["OpensOffChoice", "Read", "HarnessMove", "MoveLands", "Read"];
    for (v, answer) in [
        (
            &single,
            &[
                "HarnessType",
                "TextLands",
                "Read",
                "HarnessEnter",
                "EnterTaken",
            ][..],
        ),
        (
            &multi,
            &[
                "HarnessType",
                "TextLands",
                "Read",
                "HarnessMove",
                "MoveLands",
                "Read",
                "HarnessEnter",
                "EnterTaken",
            ][..],
        ),
    ] {
        let mut st = v.init_state();
        for tab in 0..2 {
            run(v, &mut st, &onto_the_field, &TEXT_INVARIANTS);
            run(v, &mut st, &answer[..2], &TEXT_INVARIANTS);
            assert_eq!((st["tab"], st["own"], st["typed"]), (tab, 1, 0), "{st:?}");
            run(v, &mut st, &answer[2..], &TEXT_INVARIANTS);
            assert_eq!(st["tab"], tab + 1, "{st:?}");
        }
        run(
            v,
            &mut st,
            &["Read", "HarnessEnter", "EnterTaken"],
            &TEXT_INVARIANTS,
        );
        assert_eq!(st["tab"], 3, "the dialog closed: {st:?}");
    }
    // The multi-select's text lands with the focus still in the field and
    // the button the chosen row: no Enter there, only the move off it.
    let mut st = multi.init_state();
    run(&multi, &mut st, &onto_the_field, &TEXT_INVARIANTS);
    run(
        &multi,
        &mut st,
        &["HarnessType", "TextLands", "Read"],
        &TEXT_INVARIANTS,
    );
    assert_eq!((st["focus"], st["seen_own"]), (1, 1), "{st:?}");
    assert!(!multi.action_enabled("HarnessEnter", &st), "{st:?}");
    assert!(multi.action_enabled("HarnessMove", &st), "{st:?}");
    // A lone single-select (c1): its Enter closes the dialog.
    let lone = at(&single, &[("Tabs", 1), ("Review", 0)]);
    let mut st = lone.init_state();
    run(&lone, &mut st, &onto_the_field, &TEXT_INVARIANTS);
    run(
        &lone,
        &mut st,
        &[
            "HarnessType",
            "TextLands",
            "Read",
            "HarnessEnter",
            "EnterTaken",
        ],
        &TEXT_INVARIANTS,
    );
    assert_eq!(st["tab"], 2, "the answer closed the dialog: {st:?}");
    // Never twice: dropped, the screen still — no key at all on that box.
    for v in [&single, &multi] {
        let mut st = v.init_state();
        run(
            v,
            &mut st,
            &[
                "Read",
                "HarnessType",
                "KeyRefused",
                "Read",
                "StillScreen",
                "Read",
            ],
            &TEXT_INVARIANTS,
        );
        for a in ["HarnessType", "HarnessEnter", "HarnessMove"] {
            assert!(
                !v.action_enabled(a, &st),
                "{a} after a dropped text: {st:?}"
            );
        }
        // Its head dropped: a suffix, not the supervisor's own — handed over.
        let mut st = v.init_state();
        run(
            v,
            &mut st,
            &["Read", "HarnessType", "TextPartly", "Read"],
            &TEXT_INVARIANTS,
        );
        assert_eq!((st["seen_typed"], st["seen_own"]), (1, 0), "{st:?}");
        for a in ["HarnessType", "HarnessEnter", "HarnessMove"] {
            assert!(
                !v.action_enabled(a, &st),
                "{a} after a partial text: {st:?}"
            );
        }
    }
}

/// A person's text is theirs in text mode too (R2 d): on a text tab the
/// free-text row is the chosen one, so a person may type there with the
/// focus on it — and the supervisor, once they are quiet, keys nothing
/// over it. Their keys added to the supervisor's own text make it no longer
/// its own, and it is handed over the same way.
#[test]
fn text_mode_hands_a_persons_text_over() {
    let m = aterm_spec::derive::supervisor_question_text_model();
    for multi in [0, 1] {
        let v = at(&m, &[("Multi", multi)]);
        let no_key = |st: &State| {
            for a in ["HarnessType", "HarnessEnter", "HarnessMove"] {
                assert!(!v.action_enabled(a, st), "Multi = {multi}: {a} at {st:?}");
            }
        };
        // Into the empty field, the focus on it.
        let mut st = v.init_state();
        run(
            &v,
            &mut st,
            &["Read", "PersonKey", "PersonTypes", "QuietReturns", "Read"],
            &TEXT_INVARIANTS,
        );
        assert_eq!((st["seen_focus"], st["seen_typed"]), (0, 1), "{st:?}");
        no_key(&st);
        // Onto the supervisor's own text, before its Enter.
        let mut st = v.init_state();
        run(
            &v,
            &mut st,
            &["Read", "HarnessType", "TextLands", "Read"],
            &TEXT_INVARIANTS,
        );
        assert_eq!(st["seen_own"], 1, "{st:?}");
        run(
            &v,
            &mut st,
            &["PersonKey", "PersonTypes", "QuietReturns", "Read"],
            &TEXT_INVARIANTS,
        );
        assert_eq!(
            (st["own"], st["typed"], st["seen_own"]),
            (0, 1, 0),
            "{st:?}"
        );
        no_key(&st);
        // A person answers the tab over the supervisor's text, before its
        // Enter: the next tab's field is empty, not its own — no Enter
        // there until its own text is.
        let mut st = v.init_state();
        run(
            &v,
            &mut st,
            &[
                "Read",
                "HarnessType",
                "TextLands",
                "Read",
                "PersonKey",
                "PersonAnswers",
                "QuietReturns",
                "Read",
            ],
            &TEXT_INVARIANTS,
        );
        assert_eq!((st["tab"], st["own"], st["seen_own"]), (1, 0, 0), "{st:?}");
        assert!(!v.action_enabled("HarnessEnter", &st), "{st:?}");
        assert!(v.action_enabled("HarnessType", &st), "{st:?}");
    }
}

/// A TEXT IS WRITTEN ONCE PER TAB — a law of the model, not only the loop's
/// rule (`TextNeverWrittenTwice`, the review of 2026-09-26): a text the
/// dialog dropped whole, after which a PERSON moved the focus off the field
/// and the loop moved it back — a box that changed, so `mine` is 0 — leaves
/// no second text on that tab: the loop remembers, per question, the text it
/// wrote (`wrote`, the same memory as `OwnTextRemembered`, by which the real
/// loop hands such a field over as the text "gone"). Without that memory
/// the same trace types again, and so does the fence alone. For both forms.
#[test]
fn text_mode_writes_one_text_per_tab_whatever_changes_after_it() {
    let m = aterm_spec::derive::supervisor_question_text_model();
    for multi in [0, 1] {
        let v = at(&m, &[("Multi", multi)]);
        let mut st = v.init_state();
        run(
            &v,
            &mut st,
            &[
                "Read",
                "HarnessType",
                "KeyRefused",
                "PersonKey",
                "PersonMovesOff",
                "QuietReturns",
                "Read",
                "HarnessMove",
                "MoveLands",
                "Read",
            ],
            &TEXT_INVARIANTS,
        );
        assert_eq!(
            (
                st["seen_focus"],
                st["seen_typed"],
                st["seen_own"],
                st["mine"]
            ),
            (0, 0, 0, 0),
            "Multi = {multi}: the field empty and focused again, the box changed: {st:?}"
        );
        assert!(
            !v.action_enabled("HarnessType", &st),
            "Multi = {multi}: a second text on the tab: {st:?}"
        );
        assert!(
            refutes(&v, &[("OwnTextRemembered", 0)], "TextNeverWrittenTwice"),
            "Multi = {multi}: a loop with no memory never types twice: vacuous"
        );
        assert!(
            refutes(&v, &[("Buggy", 1)], "TextNeverWrittenTwice"),
            "Multi = {multi}: the fence alone never types twice"
        );
    }
}

/// THE MEMORY DIAL IS THE MEMORY'S JOB, isolated (the review of 2026-09-26):
/// what only the loop's memory can tell from its own text is a PERSON'S
/// EXACT COPY of the policy text (`PersonTypesPolicyText`, `exact`): the
/// byte-exact read-back already refuses a suffix, the text plus more, or a
/// person's own words. So at `OwnTextRemembered = 0` `NoDigitInTheField` is
/// refuted with the head-dropped suffix (`TextPartly`) removed — through a
/// person's copy — and HOLDS with the person's copy removed: the dial is
/// refuted by the one case the memory decides, and by nothing else.
#[test]
fn the_memory_dial_is_refuted_only_through_a_persons_copy_of_the_text() {
    let m = aterm_spec::derive::supervisor_question_text_model();
    for multi in [0, 1] {
        let blind = at(&m, &[("Multi", multi), ("OwnTextRemembered", 0)]);
        let without = |names: &[&str]| {
            let mut v = blind.clone();
            let all = v.actions.len();
            v.actions.retain(|a| !names.contains(&a.name));
            assert_eq!(all - v.actions.len(), names.len(), "{names:?}");
            v
        };
        assert!(
            refutes(&blind, &[], "NoDigitInTheField"),
            "Multi = {multi}: no memory, and still no person's text submitted"
        );
        assert!(
            refutes(&without(&["TextPartly"]), &[], "NoDigitInTheField"),
            "Multi = {multi}: the refutation needed a head-dropped suffix"
        );
        assert!(
            !refutes(
                &without(&["PersonTypesPolicyText"]),
                &[],
                "NoDigitInTheField"
            ),
            "Multi = {multi}: refuted without a person's copy of the text — the dial is not \
             the memory's job"
        );
    }
}

/// A DIALOG OF TYPED AND FALLBACK TABS is inside the model (the review of
/// 2026-09-26): under a text word each TAB runs its own route — the text
/// (`fallback` 0), or its recommended option where the text would not fit
/// or the form has no free-text row (`FitLostOnIt` / `FitLostOffIt`: the
/// chosen row is that option from then on, decided before any text is
/// written there). One tab answered on its recommended option by an Enter
/// ON it, the next typed and entered, then the review — every law holding
/// after every step. On a fallback tab no text is ever enabled, and its
/// Enter is on its recommended option only.
///
/// One test per select kind (`Multi` 0 and 1), each its own whole space.
fn text_mode_answers_a_dialog_of_typed_and_fallback_tabs(multi: i64) {
    let m = aterm_spec::derive::supervisor_question_text_model();
    let v = at(&m, &[("Multi", multi)]);
    let mut st = v.init_state();
    // Tab 0 falls back, its focus off the recommended option.
    run(
        &v,
        &mut st,
        &["FitLostOffIt", "Read", "HarnessMove", "MoveLands", "Read"],
        &TEXT_INVARIANTS,
    );
    assert_eq!((st["seen_fallback"], st["seen_focus"]), (1, 0), "{st:?}");
    assert!(!v.action_enabled("HarnessType", &st), "{st:?}");
    run(
        &v,
        &mut st,
        &["HarnessEnter", "EnterTaken"],
        &TEXT_INVARIANTS,
    );
    assert_eq!((st["tab"], st["fallback"]), (1, 0), "{st:?}");
    // Tab 1 is typed.
    run(
        &v,
        &mut st,
        &[
            "OpensOffChoice",
            "Read",
            "HarnessMove",
            "MoveLands",
            "Read",
            "HarnessType",
            "TextLands",
            "Read",
        ],
        &TEXT_INVARIANTS,
    );
    assert_eq!(st["own"], 1, "{st:?}");
    let enter: &[&str] = if multi == 1 {
        &[
            "HarnessMove",
            "MoveLands",
            "Read",
            "HarnessEnter",
            "EnterTaken",
        ]
    } else {
        &["HarnessEnter", "EnterTaken"]
    };
    run(&v, &mut st, enter, &TEXT_INVARIANTS);
    run(
        &v,
        &mut st,
        &["Read", "HarnessEnter", "EnterTaken"],
        &TEXT_INVARIANTS,
    );
    assert_eq!(st["tab"], 3, "the dialog closed: {st:?}");
    // No text is ever enabled on a tab read as falling back.
    for s in reachable(&v) {
        if s["fresh"] == 1 && s["seen_fallback"] == 1 {
            assert!(!v.action_enabled("HarnessType", &s), "{s:?}");
        }
    }
}

#[test]
fn text_mode_answers_a_dialog_of_typed_and_fallback_tabs_on_a_single_select() {
    text_mode_answers_a_dialog_of_typed_and_fallback_tabs(0);
}

#[test]
fn text_mode_answers_a_dialog_of_typed_and_fallback_tabs_on_a_multi_select() {
    text_mode_answers_a_dialog_of_typed_and_fallback_tabs(1);
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

/// THE RATING CARD'S `0`: written only at an aged card, with nobody's hand on the
/// session, no draft, no box and no `0` of ours already there; a `0` the card dropped is
/// removed only once it STANDS (the vendor's debounce has run) and a backspace the server
/// refused leaves it owed; the `Buggy = 1` loop that presses whatever the state is, deletes
/// a pending dismissal, and forgets a refused backspace is caught by each invariant on its
/// own path.
#[test]
fn the_rating_cards_0_is_written_only_where_it_lands_nowhere_else() {
    let m = aterm_spec::derive::supervisor_survey_press_model();
    aterm_spec::verify::prove_and_catch_scalar(
        &m,
        "supervisor survey: a 0 only at an aged card, never over a hand, a draft or a box; \
         a dropped 0 removed only once it stands, and never forgotten",
    );
    // The disabled paths, one by one: each is a state the real loop refuses.
    let at = |card: i64, set: &[(&'static str, i64)]| {
        let mut st = m.init_state();
        st.insert("card", card);
        for (k, v) in set {
            st.insert(*k, *v);
        }
        st
    };
    assert!(m.action_enabled("Press", &at(2, &[])));
    for (why, st) in [
        ("inside the vendor's settle", at(1, &[])),
        ("under a person's hand", at(2, &[("hand", 1)])),
        ("over a draft", at(2, &[("draft", 1)])),
        ("under a box", at(2, &[("boxed", 1)])),
        (
            "on a 0 still pending",
            at(2, &[("zero", 1), ("takes", 1), ("owed", 1)]),
        ),
        ("on a 0 standing", at(2, &[("zero", 2), ("owed", 1)])),
    ] {
        assert!(!m.action_enabled("Press", &st), "{why}: {st:?}");
    }
    // The stray `0`: removed only once it STANDS, with nobody's hand and no draft; a pending
    // one is the vendor's to take (a backspace would cancel it).
    let standing = at(2, &[("zero", 2), ("owed", 1)]);
    assert!(m.action_enabled("Backspace", &standing));
    assert!(m.action_enabled("BackspaceRefused", &standing));
    for (why, st) in [
        (
            "a 0 still pending (the debounce may still take it)",
            at(2, &[("zero", 1), ("takes", 1), ("owed", 1)]),
        ),
        (
            "a dropped 0 not yet standing",
            at(1, &[("zero", 1), ("takes", 0), ("owed", 1)]),
        ),
        (
            "a person's hand",
            at(2, &[("zero", 2), ("owed", 1), ("hand", 1)]),
        ),
        ("a draft", at(2, &[("zero", 2), ("owed", 1), ("draft", 1)])),
        ("no 0 at all", at(2, &[])),
    ] {
        assert!(!m.action_enabled("Backspace", &st), "{why}: {st:?}");
        assert!(!m.action_enabled("BackspaceRefused", &st), "{why}: {st:?}");
    }
    // The vendor's side: a pending `0` is taken, or, when it was dropped, stands.
    let mut taken = at(2, &[("zero", 1), ("takes", 1), ("owed", 1)]);
    assert!(m.fire("DebounceRuns", &mut taken));
    assert_eq!((taken["card"], taken["zero"], taken["owed"]), (0, 0, 0));
    let mut dropped = at(1, &[("zero", 1), ("takes", 0), ("owed", 1)]);
    assert!(m.fire("DebounceRuns", &mut dropped));
    assert_eq!(
        (dropped["card"], dropped["zero"], dropped["owed"]),
        (1, 2, 1)
    );
    // A refused backspace changes nothing: the `0` stays, and stays owed.
    let mut refused = standing.clone();
    assert!(m.fire("BackspaceRefused", &mut refused));
    assert_eq!(refused, standing);

    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    for (invariant, action, st) in [
        ("NoZeroInsideTheSettle", "Press", at(1, &[])),
        ("NoZeroUnderAPersonsHand", "Press", at(2, &[("hand", 1)])),
        ("NoZeroOverADraftOrABox", "Press", at(2, &[("draft", 1)])),
        (
            "NoZeroOverAZero",
            "Press",
            at(2, &[("zero", 1), ("takes", 1), ("owed", 1)]),
        ),
        (
            "NoBackspaceOfAPendingZero",
            "Backspace",
            at(2, &[("zero", 1), ("takes", 1), ("owed", 1)]),
        ),
        ("NoForgottenZero", "BackspaceRefused", standing.clone()),
    ] {
        let mut wrong = st;
        assert!(
            m.check_invariant(invariant, &wrong),
            "{invariant}: holds before"
        );
        assert!(buggy.fire(action, &mut wrong), "{invariant}: {wrong:?}");
        assert!(
            !buggy.check_invariant(invariant, &wrong),
            "{invariant}: {wrong:?}"
        );
    }
}

/// THE USAGE WALL RESUMES (2026-09-28: a weekly wall behind a background shell
/// stood 4 h 44 m, nothing typed): the invariant proves at `Buggy = 0` and the
/// misplaced reset (a first act before the tokens are back) is caught at
/// `Buggy = 1`; the claim that a wall does not stand for good once its tokens
/// are back holds under the fairness it names (the clock, the loop's act, the
/// account answering), each assumption load-bearing, and each named mutant
/// breaks it alone: the masked wall (drawn behind background work, read as
/// busy, so no point ever forms) and the ladder with a last try. The
/// environment (background work that never ends, a wall that comes late) is
/// not fair. Tier-1 is aterm-agent's `supervise_conformance_usage_wall.rs`.
#[test]
fn the_usage_wall_ends_and_never_types_before_its_reset() {
    let m = aterm_spec::derive::supervisor_usage_wall_model();
    let live = aterm_spec::derive::supervisor_usage_wall_liveness();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == m.name),
        "the usage wall must stay enrolled in the spec-link registry"
    );
    assert!(
        aterm_spec::xref::liveness_registry()
            .iter()
            .any(|(registered, l)| registered.name == m.name && l.name == live.name),
        "its liveness claim must stay enrolled"
    );
    aterm_spec::verify::prove_and_catch_scalar(
        &m,
        "usage wall: no first act before the tokens are back",
    );
    aterm_spec::verify::liveness_proves_and_catches_tiered(
        &m,
        &live,
        "usage wall: a wall whose tokens are back does not stand for good",
    );

    // The stall, step by step: a wall drawn while the agent's shell runs is
    // masked; it lifts only when the shell ends, and no act is possible under
    // the mask.
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut s = buggy.init_state();
    for a in ["BackgroundStarts", "WallMasked"] {
        assert!(buggy.fire(a, &mut s), "{a} at {s:?}");
    }
    assert_eq!(s["wall"], 3, "{s:?}");
    for _ in 0..8 {
        assert!(buggy.fire("Tick", &mut s), "{s:?}");
    }
    assert!(
        !buggy.action_enabled("Act", &s),
        "a masked wall is no point: nothing is typed under it: {s:?}"
    );
    // The committed model has no mask at all.
    assert!(!m.action_enabled("WallMasked", &m.init_state()));
    // A wall whose rows leave the screen while the shell runs (a scroll back,
    // a `/model` under it) is acted on at its reset all the same, because the
    // loop remembers it; a loop that forgets it (an update, a restart) is the
    // 4 h 44 m again, and only the memory tells the two apart.
    let mut h = m.init_state();
    for a in [
        "BackgroundStarts",
        "WallAppears",
        "WallHides",
        "Tick",
        "Tick",
        "Tick",
    ] {
        assert!(m.fire(a, &mut h), "{a} at {h:?}");
    }
    assert_eq!(h["wall"], 5, "{h:?}");
    assert!(
        m.action_enabled("Act", &h),
        "remembered, it is acted on: {h:?}"
    );
    let mut lost = buggy.init_state();
    for a in [
        "BackgroundStarts",
        "WallAppears",
        "WallHides",
        "MemoryLost",
        "Tick",
        "Tick",
        "Tick",
    ] {
        assert!(buggy.fire(a, &mut lost), "{a} at {lost:?}");
    }
    assert!(
        !buggy.action_enabled("Act", &lost),
        "a wall nothing remembers is never acted on: {lost:?}"
    );
    // With the world's hiding switched off the model is the one the Tier-1 bind
    // walks against the real reader and decider.
    let plain = aterm_spec::interp::with_consts(&m, &[("Hidden", 0)]);
    let mut p = plain.init_state();
    assert!(plain.fire("BackgroundStarts", &mut p) && plain.fire("WallAppears", &mut p));
    assert!(!plain.action_enabled("WallHides", &p), "{p:?}");

    // The committed first act waits for the reset and its grace.
    let mut c = m.init_state();
    assert!(m.fire("WallAppears", &mut c));
    assert!(!m.action_enabled("Act", &c), "{c:?}");
    for _ in 0..3 {
        assert!(m.fire("Tick", &mut c), "{c:?}");
    }
    assert!(m.action_enabled("Act", &c), "{c:?}");
}

/// THE MODEL SWITCH (2026-09-22 to 09-27, 240 relaunches in five days): the
/// three laws prove at `Buggy = 0` — a return only from a switch that owed one
/// (a reset the notice named, still ahead when it was made), at most a
/// fallback, its return and one more fallback before the worker works, and a
/// return the host defers holds no point — and each defect is caught at
/// `Buggy = 1`; the fallback is made once the agent's jobs do not hold it,
/// under the fairness the claim names (the loop asks; at a reset the
/// continuation goes first), each assumption load-bearing, broken alone by the
/// asks that stop. Tier-1 is aterm-agent's `supervise_conformance_model_switch.rs`.
#[test]
fn the_model_switch_makes_the_fallback_and_returns_only_at_a_named_reset() {
    let m = aterm_spec::derive::supervisor_model_switch_model();
    let live = aterm_spec::derive::supervisor_model_switch_liveness();
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
        "model switch: no return without a named reset, no ping-pong, no held point",
    );
    aterm_spec::verify::liveness_proves_and_catches_tiered(
        &m,
        &live,
        "model switch: the fallback is made once the jobs allow it",
    );

    // The five days, step by step: a limit that names no reset, the fallback
    // made, a return guessed a tick later ends on the wall again, and again.
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut s = buggy.init_state();
    for a in [
        "BucketHitsUnnamed",
        "AskFallback",
        "Tick",
        "GuessedReturn",
        "AskFallback",
        "GuessedReturn",
    ] {
        assert!(buggy.fire(a, &mut s), "{a} at {s:?}");
    }
    assert!(!buggy.check_invariant("NoReturnWithoutAReset", &s), "{s:?}");
    assert!(!buggy.check_invariant("NoPingPong", &s), "{s:?}");

    // The committed switch, at a named reset ahead: the fallback is made, the
    // return is owed at the reset, and needs the host free.
    let mut c = m.init_state();
    for a in ["BucketHitsNamed", "AskFallback"] {
        assert!(m.fire(a, &mut c), "{a} at {c:?}");
    }
    assert_eq!((c["on"], c["back"]), (1, 1), "{c:?}");
    assert!(!m.action_enabled("Return", &c), "before the reset: {c:?}");
    for a in ["Tick", "Tick", "BucketLifts", "JobsStart"] {
        assert!(m.fire(a, &mut c), "{a} at {c:?}");
    }
    assert!(
        !m.action_enabled("Return", &c),
        "the host cannot make it now; nothing else is held: {c:?}"
    );
    assert!(m.fire("JobsEnd", &mut c) && m.fire("Return", &mut c));
    assert_eq!((c["on"], c["flips"]), (0, 2), "{c:?}");

    // At the reset the continuation goes first, and a fallback made after it
    // owes no return (the reset is not ahead).
    let mut c = m.init_state();
    for a in ["BucketHitsNamed", "Tick", "Tick"] {
        assert!(m.fire(a, &mut c), "{a} at {c:?}");
    }
    assert!(
        !m.action_enabled("AskFallback", &c),
        "the continuation goes first at the reset: {c:?}"
    );
    assert!(m.fire("Continue", &mut c) && m.fire("AskFallback", &mut c));
    assert_eq!((c["on"], c["back"]), (1, 0), "{c:?}");
    assert!(!m.action_enabled("Return", &c), "{c:?}");
}

/// The state after `path`, each step required to fire.
fn account_rotation_after(
    m: &aterm_spec::derive::Model,
    path: &[&str],
) -> std::collections::BTreeMap<&'static str, i64> {
    let mut s = m.init_state();
    for a in path {
        assert!(m.fire(a, &mut s), "{a} at {s:?}");
    }
    s
}

/// `defect`, fired after `path`, breaks `law` and no other invariant: the
/// state it reaches is caught by that one name.
fn account_rotation_defect_breaks_only(
    buggy: &aterm_spec::derive::Model,
    path: &[&str],
    defect: &str,
    law: &str,
) -> std::collections::BTreeMap<&'static str, i64> {
    let mut s = account_rotation_after(buggy, path);
    for inv in &buggy.invariants {
        assert!(
            buggy.check_invariant(inv.name, &s),
            "{} before {defect}: {s:?}",
            inv.name
        );
    }
    assert!(buggy.fire(defect, &mut s), "{defect} at {s:?}");
    let broken: Vec<&str> = buggy
        .invariants
        .iter()
        .map(|inv| inv.name)
        .filter(|name| !buggy.check_invariant(name, &s))
        .collect();
    assert_eq!(broken, [law], "{defect} at {s:?}");
    s
}

/// The nine defects of the rotation, each with the one law it breaks.
const ACCOUNT_ROTATION_DEFECTS: [(&str, &str); 9] = [
    ("RotatesIntoALimited", "NeverIntoALimitedAccount"),
    ("WallUnread", "NeverIntoALimitedAccount"),
    ("SharedFirst", "DedicatedBeforeShared"),
    ("LowFirst", "FreshBeforeLow"),
    ("FollowsUnproven", "OnlyAProvenActiveIsFollowed"),
    ("WaitsOnTheWall", "NoWaitWhileOneIsUsable"),
    ("MovesEarlyUnderAPin", "TheOwnersWordHolds"),
    ("RotatesPastTheBreaker", "NoPingPong"),
    ("WaitsAtTheLiftedWall", "NoWaitWhileOneIsUsable"),
];

/// ONE walk of `m`'s whole reachable space, going on past every violation:
/// how many states it holds, every invariant some state breaks, and every
/// action that fires.
///
/// EVERY MOVE LEAVES ITS RECORD, asserted at each transition: a step that
/// changes where a session is leaves `mv` naming that session, with `pa` and
/// `ph` what `act` and `hold` were before it. The four laws about a move
/// read that record and nothing else, so a step that moved a session and
/// wrote none (`mv = 0`) would be a move no law looks at.
fn account_rotation_space(
    m: &aterm_spec::derive::Model,
) -> (
    usize,
    std::collections::BTreeSet<&'static str>,
    std::collections::BTreeSet<&'static str>,
) {
    use std::collections::{BTreeSet, HashSet, VecDeque};
    // Walked resolved: the same states, steps and verdicts as by name
    // (`aterm_spec::interp::Resolved`), without a string-keyed map per step.
    let r = aterm_spec::interp::Resolved::new(m).expect("the rotation resolves");
    let [mv, pa, ph, act, hold] = ["mv", "pa", "ph", "act", "hold"].map(|v| r.slot(v));
    let sessions = [(r.slot("s1"), 1), (r.slot("s2"), 2)];
    let mut broken = BTreeSet::new();
    let mut fired = BTreeSet::new();
    let mut seen: HashSet<Vec<i64>> = HashSet::new();
    let init = r.init();
    seen.insert(init.clone());
    let mut queue = VecDeque::from([init]);
    while let Some(st) = queue.pop_front() {
        for (i, inv) in m.invariants.iter().enumerate() {
            if !r.holds(i, &st) {
                broken.insert(inv.name);
            }
        }
        for (a, action) in m.actions.iter().enumerate() {
            for next in r.successors(a, &st) {
                fired.insert(action.name);
                for (session, n) in sessions {
                    assert!(
                        next[session] == st[session]
                            || (next[mv], next[pa], next[ph]) == (n, st[act], st[hold]),
                        "{} moved session {n} and left no true record of it: {:?} -> {:?}",
                        action.name,
                        r.state(&st),
                        r.state(&next),
                    );
                }
                if !seen.contains(&next) {
                    seen.insert(next.clone());
                    queue.push_back(next);
                }
            }
        }
    }
    (seen.len(), broken, fired)
}

/// NEGATIVE CONTROL for the record the walk asserts. The herd's defect with
/// what it writes of itself blanked (`mv = 0`) moves session 2 onto an
/// account that has not answered and breaks NO law: the laws about a move
/// are stated over the record, and it left none. The walk's own assertion is
/// what catches it.
#[test]
#[should_panic(expected = "FollowsUnproven moved session 2 and left no true record of it")]
fn an_account_rotation_step_that_moves_a_session_without_its_record_is_caught() {
    use aterm_spec::derive::int;
    let m = aterm_spec::derive::supervisor_account_rotation_model();
    let herd = aterm_spec::verify::buggy_baseline_with(&m, "FollowsUnproven");
    let blank = |_| int(0);
    let unrecorded = account_rotation_edited(
        &herd,
        "FollowsUnproven",
        &|guard| guard,
        &[("mv", &blank), ("pa", &blank), ("ph", &blank)],
    );
    let broken = aterm_spec::interp::violated_invariants(&unrecorded);
    assert!(broken.is_empty(), "a law reads it after all: {broken:?}");
    account_rotation_space(&unrecorded);
}

/// `[]<>(the session is not at its wall \/ a named reason holds it)`: the
/// rotation's liveness goal over the session `on` (`"s1"` or `"s2"`), with
/// the agent's jobs and the canary's hold among the reasons where asked.
fn account_rotation_goal(on: &'static str, jobs: bool, hold: bool) -> aterm_spec::derive::Expr {
    use aterm_spec::derive::{and_, cst, eq, int, le, or_, var};
    let is = |account: i64| eq(var(on), int(account));
    let limited = |condition: &'static str| eq(var(condition), int(2));
    let mut goal = or_(
        or_(
            and_(is(1), le(var("d1"), int(1))),
            and_(is(2), le(var("d2"), int(1))),
        ),
        and_(is(3), le(var("sh"), int(1))),
    );
    if jobs {
        goal = or_(goal, eq(var("jobs"), int(1)));
    }
    let nothing_to_rotate_to = and_(
        eq(var("run"), int(0)),
        or_(
            le(cst("Budget"), var("rot")),
            and_(
                and_(or_(is(1), limited("d1")), or_(is(2), limited("d2"))),
                or_(is(3), limited("sh")),
            ),
        ),
    );
    if !hold {
        return or_(goal, nothing_to_rotate_to);
    }
    let the_canarys_hold_stands = and_(
        and_(le(int(1), var("act")), eq(var("prov"), int(0))),
        eq(var("hold"), int(1)),
    );
    or_(goal, or_(the_canarys_hold_stands, nothing_to_rotate_to))
}

/// ACCOUNT ROTATION (`docs/DESIGN-claude-accounts-2026-10-01.md` §6, §10): the
/// seven laws prove at `Buggy = 0` — nobody is moved into a limited account, a
/// dedicated account goes before the shared one and a fresh one before a low
/// one, only an account that has served a response is followed, a session
/// sleeps at its wall only for a reason the design names, the owner's pin
/// holds, and the hour's rotations are bounded — and each of the nine defects
/// is caught at `Buggy = 1` (by its own law and no other, over its whole
/// space: the test below). A session at the notice of a wall the owner lifted
/// is continued there (`Resume…`). A session at its wall is moved once an account it
/// could run on is usable, under the one fairness the claim names (the
/// supervisor takes the advice and makes the move), broken alone by today's
/// rule (the wall waited out); a canary that never answers holds it for the
/// canary's ten minutes and no longer. Tier-1 is aterm-agent's
/// `supervise_conformance_account_rotation.rs`.
///
/// Three tests, each its own checker run: the safety proof and the chosen
/// paths here, the registered liveness claim, and its readings below.
#[test]
fn the_account_rotation_moves_one_session_alone_and_the_rest_once_it_is_served() {
    let m = aterm_spec::derive::supervisor_account_rotation_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == m.name),
        "the rotation must stay enrolled in the spec-link registry"
    );
    aterm_spec::verify::prove_and_catch_scalar(
        &m,
        "account rotation: never into a limited account, dedicated and fresh first, \
         the herd waits for a served response, the pin holds, the hour is bounded",
    );
    account_rotation_paths(&m);
}

/// The rotation's registered liveness claim, tiered: a session at its wall is
/// moved once an account is usable, and each of its mutants breaks that.
#[test]
fn the_account_rotation_moves_a_session_at_its_wall_once_an_account_is_usable() {
    let m = aterm_spec::derive::supervisor_account_rotation_model();
    let live = aterm_spec::derive::supervisor_account_rotation_liveness();
    assert!(
        aterm_spec::xref::liveness_registry()
            .iter()
            .any(|(registered, l)| registered.name == m.name && l.name == live.name),
        "its liveness claim must stay enrolled"
    );
    aterm_spec::verify::liveness_proves_and_catches_tiered(
        &m,
        &live,
        "account rotation: a session at its wall is moved once an account is usable",
    );
    assert_eq!(
        live.goal.to_tla(),
        account_rotation_goal("s1", true, true).to_tla(),
        "the readings below are built on the registered claim's own goal"
    );
}

/// The registered claim's readings: session 2's twin, the conjunction that
/// is NOT a property, and the canary's hold once the clock is fair — each
/// with its negative control.
#[test]
fn the_account_rotation_s_liveness_readings_hold_only_under_their_fairness() {
    let m = aterm_spec::derive::supervisor_account_rotation_model();
    let live = aterm_spec::derive::supervisor_account_rotation_liveness();
    assert_eq!(
        live.goal.to_tla(),
        account_rotation_goal("s1", true, true).to_tla(),
        "the readings below are built on the registered claim's own goal"
    );

    // The readings below differ in goal and fairness, not in the model: walk
    // its space once and ask each of them of it.
    let space = aterm_spec::interp::Space::of(&m);

    // Session 2's twin of the claim: the same goal over `s2` (whose jobs are
    // not modelled), under `Move2`. Interpreter only, at the committed config,
    // and needing its fairness.
    let twin = aterm_spec::derive::Liveness {
        name: "EveryUsableAccountIsTriedByTheOther",
        goal: account_rotation_goal("s2", false, true),
        weak: vec!["Move2"],
        strong: vec![],
        mutants: vec![],
    };
    assert!(
        space.nonprogress_under(&twin).is_none(),
        "session 2 is moved under the same rule"
    );
    let unfair = aterm_spec::derive::Liveness {
        weak: vec![],
        ..twin.clone()
    };
    assert!(
        space.nonprogress_under(&unfair).is_some(),
        "NEGATIVE CONTROL: with nobody taking the advice it sits at its wall"
    );
    // The two claims are not ONE: walls that keep resetting can leave one
    // session or the other at its wall at every instant while each is moved.
    let both = aterm_spec::derive::Liveness {
        name: "BothAtOnce",
        goal: aterm_spec::derive::and_(live.goal.clone(), twin.goal.clone()),
        weak: vec!["Move1", "Move2"],
        strong: vec![],
        mutants: vec![],
    };
    assert!(
        space.nonprogress_under(&both).is_some(),
        "the conjunction is not a property of the fair model"
    );
    // THE CANARY IS NO EXCEPTION ONCE THE CLOCK IS FAIR. The registered claim
    // excepts the canary's hold while it stands. With the hold's end assumed
    // (`Lapse`: its ten minutes pass) the exception goes altogether: a canary
    // that never answers does not keep a session at its wall.
    let clock_fair = aterm_spec::derive::Liveness {
        name: "EveryUsableAccountIsTriedOnceTheHoldEnds",
        goal: account_rotation_goal("s1", true, false),
        weak: vec!["Move1", "Lapse"],
        strong: vec![],
        mutants: vec![],
    };
    assert!(
        space.nonprogress_under(&clock_fair).is_none(),
        "a silent canary holds nobody past its ten minutes"
    );
    let hold_for_good = aterm_spec::derive::Liveness {
        weak: vec!["Move1"],
        ..clock_fair.clone()
    };
    assert!(
        space.nonprogress_under(&hold_for_good).is_some(),
        "NEGATIVE CONTROL: it is the hold's end that frees the session"
    );
}

/// The rotation's chosen paths, each defect caught where it first fires.
fn account_rotation_paths(m: &aterm_spec::derive::Model) {
    let buggy = aterm_spec::interp::with_buggy(m, 1);

    // THE HERD. A wall on the account both sessions run on: one rotates alone
    // to the next dedicated account, and the other is held until that account
    // has served a response.
    let mut c = account_rotation_after(m, &["WallD1", "Move1"]);
    assert_eq!(
        (c["s1"], c["act"], c["prov"], c["prev"], c["hold"], c["rot"]),
        (2, 2, 0, 0, 1, 1),
        "{c:?}"
    );
    assert!(
        !m.action_enabled("Move2", &c),
        "nobody follows an unproven account: {c:?}"
    );
    assert!(
        m.action_enabled("Wait2", &c),
        "it waits for the answer: {c:?}"
    );
    assert!(m.fire("Wait2", &mut c) && m.fire("Served", &mut c));
    assert_eq!(
        (c["prov"], c["hold"], c["w2"]),
        (1, 0, 0),
        "the answer wakes whoever waits: {c:?}"
    );
    assert!(m.fire("Move2", &mut c));
    assert_eq!(
        (c["s2"], c["act"], c["rot"]),
        (2, 2, 1),
        "a follow is no rotation: {c:?}"
    );
    // The defect: the other session follows before the answer.
    let herd = account_rotation_defect_breaks_only(
        &buggy,
        &["WallD1", "Move1"],
        "FollowsUnproven",
        "OnlyAProvenActiveIsFollowed",
    );
    assert_eq!((herd["s2"], herd["prov"]), (2, 0), "{herd:?}");
    // …and the account that answers with a wall of its own is struck, `active`
    // goes back, and the session that never moved is where it was.
    let mut c = account_rotation_after(m, &["WallD1", "Move1", "Strike"]);
    assert_eq!(
        (c["d2"], c["act"], c["prov"], c["s2"]),
        (2, 0, 0, 1),
        "{c:?}"
    );
    assert!(m.fire("Move2", &mut c));
    assert_eq!((c["s2"], c["act"]), (3, 3), "the next candidate: {c:?}");

    // THE CANARY THAT NEVER ANSWERS. Its hold ends, `active` is not put back,
    // and the session that waited takes its place: no rotation, a hold of its
    // own, and its served response proves the account. The first canary is
    // where it was.
    let mut c = account_rotation_after(m, &["WallD1", "Move1", "Wait2", "Lapse"]);
    assert_eq!(
        (c["act"], c["prov"], c["hold"], c["w2"]),
        (2, 0, 0, 0),
        "the hold's end wakes whoever waited for it: {c:?}"
    );
    assert!(!m.action_enabled("Wait2", &c), "{c:?}");
    assert!(m.fire("Move2", &mut c));
    assert_eq!(
        (c["s2"], c["act"], c["prov"], c["hold"], c["rot"]),
        (2, 2, 0, 1, 1),
        "in the canary's place: {c:?}"
    );
    assert!(m.fire("Served", &mut c));
    assert_eq!((c["s1"], c["s2"], c["prov"]), (2, 2, 1), "{c:?}");
    // Ahead of a wall the place is taken on a fresh account…
    let mut c = account_rotation_after(m, &["WallD1", "Move1", "ResetD1"]);
    assert_eq!(
        (c["s2"], c["d1"], c["run"], c["hold"]),
        (1, 0, 2, 1),
        "{c:?}"
    );
    assert!(!m.action_enabled("Move2", &c), "the hold stands: {c:?}");
    assert!(m.fire("Lapse", &mut c) && m.fire("Move2", &mut c));
    assert_eq!((c["s2"], c["rot"]), (2, 1), "{c:?}");
    // …and never on a low one: that waits for a session at its wall.
    let low_canary = [
        "WallD1", "Move2", "Served", "LowD2", "UseSh", "Move1", "Move2", "Unpin", "WallSh",
        "Move1", "ResetSh", "Lapse",
    ];
    let mut c = account_rotation_after(m, &low_canary);
    assert_eq!(
        (c["s2"], c["sh"], c["act"], c["run"], c["prov"], c["hold"]),
        (3, 0, 2, 1, 0, 0),
        "{c:?}"
    );
    assert!(!m.action_enabled("Move2", &c), "{c:?}");
    assert!(m.fire("WallSh", &mut c) && m.fire("Move2", &mut c));
    assert_eq!((c["s2"], c["hold"], c["rot"]), (2, 1, 2), "{c:?}");

    // PROOF TRAVELS WITH THE ACCOUNT. A wall recorded on the canary's account
    // with no strike, a rotation past it, and a strike of the next candidate:
    // `active` goes back to an account that never served, and it is NOT
    // proven for having been gone back to. Once it can be run on, the session
    // at its wall takes the canary's place there; it does not follow.
    let unserved = ["WallD1", "Move1", "WallD2", "Move2", "Strike"];
    let mut c = account_rotation_after(m, &unserved[..4]);
    assert_eq!((c["act"], c["prev"], c["pp"]), (3, 2, 0), "{c:?}");
    assert!(m.fire("Strike", &mut c));
    assert_eq!(
        (c["act"], c["run"], c["prov"], c["prev"], c["hold"]),
        (2, 0, 0, 0, 0),
        "{c:?}"
    );
    assert!(m.fire("ResetD2", &mut c) && m.fire("Move2", &mut c));
    assert_eq!(
        (c["s2"], c["prov"], c["hold"]),
        (2, 0, 1),
        "taken, not followed: {c:?}"
    );
    // The account that HAD served is proven when it is gone back to.
    let served = ["WallD1", "Move1", "Served", "WallD2", "Move2", "Strike"];
    let c = account_rotation_after(m, &served);
    assert_eq!((c["act"], c["prov"]), (2, 1), "{c:?}");

    // THE SHARED-FIRST PICK. With both others fresh the dedicated one is taken.
    let c = account_rotation_after(m, &["WallD1", "Move1"]);
    assert_eq!(c["s1"], 2, "{c:?}");
    let shared = account_rotation_defect_breaks_only(
        &buggy,
        &["WallD1"],
        "SharedFirst",
        "DedicatedBeforeShared",
    );
    assert_eq!(shared["s1"], 3, "{shared:?}");

    // THE LOW-BEFORE-FRESH PICK. The dedicated account is low and the shared
    // one fresh: fresh goes first, whatever its class.
    let low = ["WallD1", "Move2", "Strike", "ResetD2", "LowD2"];
    let mut c = account_rotation_after(m, &low);
    assert_eq!(
        (c["d1"], c["d2"], c["sh"], c["s1"], c["rot"]),
        (2, 1, 0, 1, 1),
        "{c:?}"
    );
    assert!(m.fire("Move1", &mut c));
    assert_eq!(c["s1"], 3, "{c:?}");
    let lowered = account_rotation_defect_breaks_only(&buggy, &low, "LowFirst", "FreshBeforeLow");
    assert_eq!(lowered["s1"], 2, "{lowered:?}");
    // A low account IS taken at a wall when nothing is fresh — and never
    // ahead of one, nor at a plain turn end on a limited account.
    let mut c = account_rotation_after(m, &low_canary[..9]);
    assert_eq!((c["d1"], c["d2"], c["sh"], c["s1"]), (2, 1, 2, 3), "{c:?}");
    assert!(!m.action_enabled("Leave1", &c), "{c:?}");
    assert!(m.fire("Move1", &mut c));
    assert_eq!((c["s1"], c["act"], c["prov"]), (2, 2, 0), "{c:?}");

    // RULE 2 FOLLOWS A FRESH OR PINNED ACTIVE. A session on a fresh account is
    // not moved onto a proven `active` that is low…
    let mut c = account_rotation_after(m, &["WallD1", "Move1", "Served", "ResetD1", "LowD2"]);
    assert_eq!(
        (c["s2"], c["d1"], c["d2"], c["act"], c["run"], c["prov"]),
        (1, 0, 1, 2, 1, 1),
        "{c:?}"
    );
    assert!(!m.action_enabled("Move2", &c), "{c:?}");
    // …at its wall it rotates PAST that account for a fresh one…
    assert!(m.fire("WallD1", &mut c) && m.fire("Move2", &mut c));
    assert_eq!(
        (c["s2"], c["act"], c["prev"], c["pp"], c["rot"]),
        (3, 3, 2, 1, 2),
        "{c:?}"
    );
    // …and joins it, low as it is, only when no fresh account is left.
    assert!(m.fire("Strike", &mut c));
    assert_eq!(
        (c["sh"], c["act"], c["run"], c["prov"]),
        (2, 2, 1, 1),
        "{c:?}"
    );
    assert!(m.fire("Move2", &mut c));
    assert_eq!((c["s2"], c["act"], c["rot"]), (2, 2, 2), "{c:?}");
    // The owner's pin is followed whatever is left of the account.
    let mut c = account_rotation_after(m, &["UseSh", "Move1", "LowSh"]);
    assert_eq!((c["s2"], c["sh"], c["pin"]), (1, 1, 3), "{c:?}");
    assert!(m.fire("Move2", &mut c));
    assert_eq!(c["s2"], 3, "{c:?}");

    // A PLAIN TURN END ON A LIMITED ACCOUNT (the wall is another tab's): the
    // session is moved ahead of its own wall, to a fresh account.
    let mut c = account_rotation_after(m, &["WallD1"]);
    assert!(m.fire("Leave1", &mut c));
    assert_eq!((c["s1"], c["act"], c["rot"]), (2, 2, 1), "{c:?}");

    // NEVER INTO A LIMITED ACCOUNT: by a rotation on a stale reading, or by a
    // follow of an active account whose wall nobody read.
    let stale = ["WallD1", "Move2", "Strike"];
    let mut c = account_rotation_after(m, &stale);
    assert!(m.fire("Move1", &mut c));
    assert_eq!(c["s1"], 3, "the limited one is passed over: {c:?}");
    account_rotation_defect_breaks_only(
        &buggy,
        &stale,
        "RotatesIntoALimited",
        "NeverIntoALimitedAccount",
    );
    let mut unread = account_rotation_defect_breaks_only(
        &buggy,
        &["WallD1", "Move1", "Served"],
        "WallUnread",
        "NeverIntoALimitedAccount",
    );
    assert!(buggy.fire("Move2", &mut unread));
    assert_eq!(
        (unread["s2"], unread["d2"]),
        (2, 2),
        "followed into its wall: {unread:?}"
    );

    // TODAY'S RULE: the wall is waited out with a fresh account free, and the
    // session that sleeps is not moved.
    let waits = account_rotation_defect_breaks_only(
        &buggy,
        &["WallD1"],
        "WaitsOnTheWall",
        "NoWaitWhileOneIsUsable",
    );
    assert!(!buggy.action_enabled("Move1", &waits), "{waits:?}");
    // The committed waits: the agent's jobs hold the relaunch, and the move
    // is made once they end.
    let mut c = account_rotation_after(m, &["WallD1", "JobsStart"]);
    assert!(!m.action_enabled("Move1", &c), "{c:?}");
    assert!(m.fire("Wait1", &mut c) && m.fire("JobsEnd", &mut c));
    assert_eq!(c["w1"], 0, "{c:?}");
    assert!(m.fire("Move1", &mut c));
    // Every other account limited: the wait, until a window resets.
    let mut c = account_rotation_after(m, &["WallD1", "Move1", "Strike", "Move1", "Strike"]);
    assert_eq!((c["d1"], c["d2"], c["sh"], c["rot"]), (2, 2, 2, 2), "{c:?}");
    assert!(!m.action_enabled("Move1", &c) && m.fire("Wait1", &mut c));
    assert!(m.fire("ResetD2", &mut c));
    assert_eq!(c["w1"], 0, "a reset wakes it: {c:?}");

    // THE OWNER'S WORD. Everyone follows the pin, the early move is off while
    // it holds, and a wall on the pinned account ends it.
    let mut c = account_rotation_after(m, &["UseSh", "Move1", "Move2", "LowSh"]);
    assert_eq!((c["s1"], c["s2"], c["pin"], c["sh"]), (3, 3, 3, 1), "{c:?}");
    assert!(
        !m.action_enabled("Move1", &c),
        "pinned and low, it stays: {c:?}"
    );
    let mut unpinned = c.clone();
    assert!(m.fire("Unpin", &mut unpinned) && m.fire("Move1", &mut unpinned));
    assert_eq!(
        (unpinned["s1"], unpinned["act"], unpinned["prov"]),
        (1, 1, 0),
        "ahead of the wall, to a fresh account: {unpinned:?}"
    );
    assert!(m.fire("WallSh", &mut c));
    assert_eq!((c["pin"], c["run"]), (0, 0), "{c:?}");
    account_rotation_defect_breaks_only(
        &buggy,
        &["UseD1", "LowD1"],
        "MovesEarlyUnderAPin",
        "TheOwnersWordHolds",
    );

    // THE OWNER LIFTED THE WALL A SESSION STANDS AT. Both sessions are at
    // account 1's wall and the owner pins it: each stands at its notice on an
    // account that can be run on, and is CONTINUED there — no move, no wait —
    // once. The notice is the instant's: a step of the world's forgets it.
    let mut c = account_rotation_after(m, &["WallD1", "UseD1"]);
    assert_eq!((c["n1"], c["n2"], c["d1"], c["pin"]), (1, 1, 0, 1), "{c:?}");
    for step in ["Move1", "Leave1", "Wait1", "Move2", "Leave2", "Wait2"] {
        assert!(!m.action_enabled(step, &c), "{step} at {c:?}");
    }
    assert!(m.fire("Resume1", &mut c));
    assert_eq!(
        (c["n1"], c["n2"], c["s1"], c["act"], c["rot"]),
        (0, 1, 1, 1, 0),
        "continued where it is, nothing rotated: {c:?}"
    );
    assert!(!m.action_enabled("Resume1", &c), "once a clearing: {c:?}");
    assert!(m.fire("LowD1", &mut c));
    assert_eq!(c["n2"], 0, "{c:?}");
    assert!(!m.action_enabled("Resume2", &c), "{c:?}");
    // NEGATIVE CONTROL: a pin on an account nobody stands at the wall of
    // lifts no notice — the sessions at account 1's wall follow it.
    let mut c = account_rotation_after(m, &["WallD1", "UseD2"]);
    assert_eq!((c["n1"], c["n2"]), (0, 0), "{c:?}");
    assert!(!m.action_enabled("Resume1", &c) && m.fire("Move1", &mut c));
    // The defect: the lifted notice waited out, as it was until 2026-10-01.
    let slept = account_rotation_defect_breaks_only(
        &buggy,
        &["WallD1", "UseD1"],
        "WaitsAtTheLiftedWall",
        "NoWaitWhileOneIsUsable",
    );
    assert_eq!((slept["w1"], slept["d1"]), (1, 0), "{slept:?}");

    // THE BREAKER. Two rotations are the hour's; a third wall is waited out.
    let spent = [
        "WallD1", "Move1", "Served", "ResetD1", "WallD2", "Move1", "Served", "ResetD2", "WallD1",
    ];
    let c = account_rotation_after(m, &spent);
    assert_eq!((c["s1"], c["d1"], c["d2"], c["rot"]), (1, 2, 0, 2), "{c:?}");
    assert!(
        !m.action_enabled("Move1", &c) && m.action_enabled("Wait1", &c),
        "{c:?}"
    );
    let past =
        account_rotation_defect_breaks_only(&buggy, &spent, "RotatesPastTheBreaker", "NoPingPong");
    assert_eq!(past["rot"], 3, "{past:?}");
}

/// The sound rotation's own space (`Buggy` as committed), walked ONCE per
/// process however many of the tests below ask for it: its state count, the
/// laws it breaks and the actions that fire.
fn account_rotation_sound_space() -> &'static (
    usize,
    std::collections::BTreeSet<&'static str>,
    std::collections::BTreeSet<&'static str>,
) {
    static SOUND: std::sync::OnceLock<(
        usize,
        std::collections::BTreeSet<&'static str>,
        std::collections::BTreeSet<&'static str>,
    )> = std::sync::OnceLock::new();
    SOUND.get_or_init(|| {
        account_rotation_space(&aterm_spec::derive::supervisor_account_rotation_model())
    })
}

/// The sound rule breaks nothing, the defects and nothing else are dead in
/// it, every law is some defect's negative control, and the whole `Buggy = 1`
/// space, every defect together, fits the interpreter's budget with room to
/// spare. Each defect ALONE is the slices below.
#[test]
fn the_account_rotation_s_sound_space_breaks_nothing_and_only_its_defects_are_dead() {
    let m = aterm_spec::derive::supervisor_account_rotation_model();
    let (sound, broken, fired) = account_rotation_sound_space();
    assert!(broken.is_empty(), "{broken:?}");
    let dead: Vec<&str> = m
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|a| !fired.contains(a))
        .collect();
    let defects: Vec<&str> = ACCOUNT_ROTATION_DEFECTS.iter().map(|(d, _)| *d).collect();
    assert_eq!(dead, defects, "the defects, and nothing else, are dead");
    for inv in &m.invariants {
        assert!(
            ACCOUNT_ROTATION_DEFECTS
                .iter()
                .any(|(_, law)| *law == inv.name),
            "{}: no defect is its negative control",
            inv.name
        );
    }
    let (all, ..) = account_rotation_space(&aterm_spec::interp::with_buggy(&m, 1));
    eprintln!(
        "SupervisorAccountRotation: {sound} states at Buggy = 0, {all} at Buggy = 1 \
         (invariants not stopping the walk)."
    );
    assert!(all < 100_000, "{all}");
}

/// EACH DEFECT, ALONE, BREAKS ITS OWN LAW AND NO OTHER — over the whole space
/// it reaches, not at one chosen state. The sound rule plus that one defect
/// (`buggy_baseline_with`) is walked to the end, going on past every
/// violation: the defect fires, and the laws broken anywhere in that space
/// are exactly the one it is the negative control of.
///
/// What this does NOT say: that a defect fires only from a state the sound
/// rule reaches. It fires wherever its guard holds, and in its own space that
/// includes the states its earlier firings left; the claim covers those too.
///
/// Slice `slice` of `of`: the defects at indices `slice`, `slice + of`, … of
/// [`ACCOUNT_ROTATION_DEFECTS`], so the `of` slices below are disjoint and
/// together hold every defect once (each walk is its own whole space, so
/// libtest runs them side by side).
fn each_account_rotation_defect_breaks_its_own_law(slice: usize, of: usize) {
    let m = aterm_spec::derive::supervisor_account_rotation_model();
    let (sound, ..) = account_rotation_sound_space();
    for (defect, law) in ACCOUNT_ROTATION_DEFECTS.iter().skip(slice).step_by(of) {
        let alone = aterm_spec::verify::buggy_baseline_with(&m, defect);
        let (states, broken, fired) = account_rotation_space(&alone);
        assert!(fired.contains(defect), "{defect} never fires");
        assert!(states > *sound, "{defect} reaches nothing new");
        assert_eq!(
            broken.into_iter().collect::<Vec<_>>(),
            [*law],
            "{defect}, alone, over {states} states"
        );
    }
}

const ACCOUNT_ROTATION_DEFECT_SLICES: usize = 5;

#[test]
fn each_account_rotation_defect_breaks_its_own_law_slice_0_of_5() {
    each_account_rotation_defect_breaks_its_own_law(0, ACCOUNT_ROTATION_DEFECT_SLICES);
}

#[test]
fn each_account_rotation_defect_breaks_its_own_law_slice_1_of_5() {
    each_account_rotation_defect_breaks_its_own_law(1, ACCOUNT_ROTATION_DEFECT_SLICES);
}

#[test]
fn each_account_rotation_defect_breaks_its_own_law_slice_2_of_5() {
    each_account_rotation_defect_breaks_its_own_law(2, ACCOUNT_ROTATION_DEFECT_SLICES);
}

#[test]
fn each_account_rotation_defect_breaks_its_own_law_slice_3_of_5() {
    each_account_rotation_defect_breaks_its_own_law(3, ACCOUNT_ROTATION_DEFECT_SLICES);
}

#[test]
fn each_account_rotation_defect_breaks_its_own_law_slice_4_of_5() {
    each_account_rotation_defect_breaks_its_own_law(4, ACCOUNT_ROTATION_DEFECT_SLICES);
}

/// `m` with `action`'s guard and updates rewritten: the model as it would be
/// with that SOUND step edited. `guard` maps the guard; each `(var, f)` maps
/// that variable's update. Nothing else changes — the defects stay dead, and
/// what the step records of itself (`mv`, `pa`, `ph`) is not touched.
fn account_rotation_edited(
    m: &aterm_spec::derive::Model,
    action: &str,
    guard: &dyn Fn(aterm_spec::derive::Expr) -> aterm_spec::derive::Expr,
    updates: &[(
        &str,
        &dyn Fn(aterm_spec::derive::Expr) -> aterm_spec::derive::Expr,
    )],
) -> aterm_spec::derive::Model {
    let mut edited = m.clone();
    let step = edited
        .actions
        .iter_mut()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("no action {action}"));
    step.guard = Some(guard(step.guard.clone().expect("a guarded step")));
    for (var, f) in updates {
        let update = step
            .updates
            .iter_mut()
            .find(|u| u.var == *var)
            .unwrap_or_else(|| panic!("{action} does not write {var}"));
        update.expr = f(update.expr.clone());
    }
    edited
}

// THE LAWS ARE ABOUT THE SOUND MOVE, NOT ABOUT A DEFECT'S REPORT OF ITSELF.
// Each law over a session's move is stated over what can be seen after it
// against the record of the state before it, so `Move…` edited to pick wrong
// IS the violation: in the four tests below the model's own `Move1` / `Move2`
// is edited, at `Buggy = 0`, and the law breaks with NO defect action ever
// firing. (Split four ways because each edit walks its own whole space.)
//
// (The model's earlier form stated three of these laws over a flag that only
// the defects wrote: each of these edits then proved every law.)

/// What `action` of the sound rotation writes to `name`.
fn account_rotation_update(action: &str, name: &str) -> aterm_spec::derive::Expr {
    let m = aterm_spec::derive::supervisor_account_rotation_model();
    let step = m.actions.iter().find(|a| a.name == action).unwrap();
    let written = step.updates.iter().find(|u| u.var == name).unwrap();
    written.expr.clone()
}

/// "This step is a rotation", read off the step's own count of it.
fn account_rotation_rotating(action: &str) -> aterm_spec::derive::Expr {
    use aterm_spec::derive::{gt, var};
    gt(account_rotation_update(action, "rot"), var("rot"))
}

/// `name` is not `k`.
fn account_rotation_not(name: &'static str, k: i64) -> aterm_spec::derive::Expr {
    use aterm_spec::derive::{gt, int, or_, var};
    or_(gt(var(name), int(k)), gt(int(k), var(name)))
}

/// `session` runs on another account than `active`.
fn account_rotation_on_another(session: &'static str) -> aterm_spec::derive::Expr {
    use aterm_spec::derive::{gt, or_, var};
    or_(gt(var(session), var("act")), gt(var("act"), var(session)))
}

/// What the edited model breaks, with the defects dead throughout.
fn account_rotation_breaks(edited: &aterm_spec::derive::Model) -> Vec<&'static str> {
    let (_, broken, fired) = account_rotation_space(edited);
    for (defect, _) in ACCOUNT_ROTATION_DEFECTS {
        assert!(!fired.contains(defect), "{defect} fired at Buggy = 0");
    }
    broken.into_iter().collect::<Vec<_>>()
}

/// The shared account first, and a low account first.
#[test]
fn the_account_rotation_laws_break_when_the_sound_move_picks_shared_or_low_first() {
    use aterm_spec::derive::{Expr, and_, eq, if_, int, le, var};
    let m = aterm_spec::derive::supervisor_account_rotation_model();
    let keep = |e: Expr| e;

    // The shared account first: a rotation takes it whenever it is fresh.
    let shared_first = and_(
        account_rotation_rotating("Move1"),
        and_(eq(var("sh"), int(0)), le(var("s1"), int(2))),
    );
    let to_shared = |e: Expr| if_(shared_first.clone(), int(3), e);
    let edited = account_rotation_edited(
        &m,
        "Move1",
        &keep,
        &[("s1", &to_shared), ("act", &to_shared)],
    );
    assert_eq!(account_rotation_breaks(&edited), ["DedicatedBeforeShared"]);

    // A low account first: a rotation takes account 2 when it is low,
    // whatever is fresh.
    let low_first = and_(
        and_(account_rotation_rotating("Move1"), eq(var("d2"), int(1))),
        and_(
            account_rotation_not("s1", 2),
            account_rotation_not("act", 2),
        ),
    );
    let to_low = |e: Expr| if_(low_first.clone(), int(2), e);
    let reads_low = |e: Expr| if_(low_first.clone(), int(1), e);
    let edited = account_rotation_edited(
        &m,
        "Move1",
        &keep,
        &[("s1", &to_low), ("act", &to_low), ("run", &reads_low)],
    );
    assert_eq!(account_rotation_breaks(&edited), ["FreshBeforeLow"]);
}

/// A stale reading, and the herd.
#[test]
fn the_account_rotation_laws_break_when_the_sound_move_reads_stale_or_follows_the_herd() {
    use aterm_spec::derive::{Expr, and_, eq, if_, int, le, or_, var};
    let m = aterm_spec::derive::supervisor_account_rotation_model();
    let keep = |e: Expr| e;

    // A stale reading: a rotation takes account 2 though a wall stands on it.
    let stale = and_(
        and_(account_rotation_rotating("Move1"), eq(var("d2"), int(2))),
        and_(
            account_rotation_not("s1", 2),
            account_rotation_not("act", 2),
        ),
    );
    let to_limited = |e: Expr| if_(stale.clone(), int(2), e);
    let edited = account_rotation_edited(
        &m,
        "Move1",
        &keep,
        &[("s1", &to_limited), ("act", &to_limited)],
    );
    assert_eq!(
        account_rotation_breaks(&edited),
        ["NeverIntoALimitedAccount"]
    );

    // The herd: the take no longer asks whether the canary's hold has lapsed,
    // so a session joins an account that has not answered while it stands.
    let herd = and_(
        and_(eq(var("w2"), int(0)), account_rotation_on_another("s2")),
        and_(
            and_(le(int(1), var("run")), eq(var("prov"), int(0))),
            eq(var("hold"), int(1)),
        ),
    );
    let with_the_herd = |e: Expr| or_(e, herd.clone());
    let edited = account_rotation_edited(&m, "Move2", &with_the_herd, &[]);
    assert_eq!(
        account_rotation_breaks(&edited),
        ["OnlyAProvenActiveIsFollowed"]
    );
}

/// Rule 2 as it was (follow whatever `active` is): from a FRESH account a
/// session is moved onto a proven `active` that is low and not pinned.
#[test]
fn the_account_rotation_laws_break_when_the_sound_move_follows_a_low_active() {
    use aterm_spec::derive::{Expr, and_, eq, if_, int, or_, var};
    let m = aterm_spec::derive::supervisor_account_rotation_model();
    let own_fresh = or_(
        or_(
            and_(eq(var("s2"), int(1)), eq(var("d1"), int(0))),
            and_(eq(var("s2"), int(2)), eq(var("d2"), int(0))),
        ),
        and_(eq(var("s2"), int(3)), eq(var("sh"), int(0))),
    );
    let onto_low = and_(
        and_(eq(var("w2"), int(0)), own_fresh),
        and_(
            and_(eq(var("prov"), int(1)), eq(var("run"), int(1))),
            and_(eq(var("pin"), int(0)), account_rotation_on_another("s2")),
        ),
    );
    let or_onto_low = |e: Expr| or_(e, onto_low.clone());
    let joins = |e: Expr| if_(onto_low.clone(), var("act"), e);
    let unchanged = |name: &'static str| {
        let onto_low = onto_low.clone();
        move |e: Expr| if_(onto_low.clone(), var(name), e)
    };
    let (run, prov, prev, pp, rot, hold) = (
        unchanged("run"),
        unchanged("prov"),
        unchanged("prev"),
        unchanged("pp"),
        unchanged("rot"),
        unchanged("hold"),
    );
    let edited = account_rotation_edited(
        &m,
        "Move2",
        &or_onto_low,
        &[
            ("s2", &joins),
            ("act", &joins),
            ("run", &run),
            ("prov", &prov),
            ("prev", &prev),
            ("pp", &pp),
            ("rot", &rot),
            ("hold", &hold),
        ],
    );
    assert_eq!(account_rotation_breaks(&edited), ["FreshBeforeLow"]);
}

/// THE CANARY'S PLACE IS WHAT THE LIVENESS CLAIM RESTS ON. With the take
/// removed from `Move1` (the rule before the hold had an end) every safety
/// law still proves, and a session sits at its wall for good beside an
/// account whose canary never answers.
#[test]
fn the_account_rotation_liveness_breaks_when_the_sound_move_never_takes_the_canary_s_place() {
    use aterm_spec::derive::{Expr, and_, eq, int, or_, var};
    let m = aterm_spec::derive::supervisor_account_rotation_model();
    let never_takes = |e: Expr| {
        let not_the_take = or_(
            or_(eq(var("act"), var("s1")), eq(var("run"), int(0))),
            eq(var("prov"), int(1)),
        );
        and_(e, not_the_take)
    };
    let edited = account_rotation_edited(&m, "Move1", &never_takes, &[]);
    assert_eq!(account_rotation_breaks(&edited), [""; 0]);
    let live = aterm_spec::derive::supervisor_account_rotation_liveness();
    let stuck = aterm_spec::interp::nonprogress_under(&edited, &live)
        .expect("a canary that holds for good breaks the claim");
    let at = stuck.entry();
    assert_eq!((at["prov"], at["hold"], at["w1"]), (0, 0, 0), "{stuck}");
    assert!(aterm_spec::interp::nonprogress_under(&m, &live).is_none());
}

/// THE SAME BOX IS ONE POINT (FIX 3; the re-alert of 2026-09-25, `CLEARED`
/// then `ESCALATED` 7 s apart at seq 94641/95788 for one box): a request is
/// asked once however it is redrawn, and every request the loop looks at is
/// asked — proven. `Buggy = 1` (every slip at once) is caught, through `ty`
/// too where it is installed, and each slip the fix had to avoid is caught
/// ALONE, on its own invariant: the key of the DRAWING (`Drawing = 1`: the
/// key before the fix, a redraw read as a new box) asks one request twice;
/// the key of the head alone (`Coarse = 1`: the rejected plan) merges two
/// Edit boxes on one path, the second looked at and never asked; a
/// `writing` read taken for work (`WritingWorks = 1`, the review's (c))
/// asks the box redrawn through it twice. No invariant is a ghost: each is
/// broken by `Buggy = 1` alone (the workspace ratchet's own test, run here
/// too), and the `Buggy = 1` space fits the interpreter.
#[test]
fn the_box_ask_proves_and_catches_a_reask_on_a_redraw() {
    use aterm_spec::interp;
    let m = aterm_spec::derive::supervisor_box_ask_model();
    aterm_spec::verify::prove_and_catch_scalar(
        &m,
        "supervisor box ask: one ask per request, every looked request asked",
    );
    let states = interp::bmc(&m).expect("the shipped machine proves");
    assert!(states > 1, "{states}");
    assert!(
        aterm_spec::verify::uncaught_invariants(&m).is_empty(),
        "every invariant broken by Buggy = 1 alone"
    );
    let mut space = interp::with_buggy(&m, 1);
    space.invariants.clear();
    interp::bmc(&space).expect("the Buggy = 1 space fits the interpreter");
    for (dial, caught) in [
        ("Drawing", "OneAskPerRequest"),
        ("Coarse", "EveryLookedRequestIsAsked"),
        ("WritingWorks", "OneAskPerRequest"),
    ] {
        match interp::bmc(&interp::with_consts(&m, &[(dial, 1)])) {
            Err((st, name)) => assert_eq!(name, caught, "{dial}: {st:?}"),
            Ok(n) => panic!("{dial} = 1 is not caught ({n} states)"),
        }
    }
    // The shipped machine on the incident's shape: the box asked, redrawn,
    // looked at again — no second ask.
    let mut st = m.init_state();
    for action in [
        "Work", "NextBox", "Look", "Redraw", "Look", "Redraw", "Look",
    ] {
        assert!(m.fire(action, &mut st), "{action} at {st:?}");
    }
    assert_eq!(st["asks"], 1, "{st:?}");
    // …and two requests on one head, no read between: two asks.
    let mut st = m.init_state();
    for action in ["NextBox", "Look", "NextBox", "Look"] {
        assert!(m.fire(action, &mut st), "{action} at {st:?}");
    }
    assert_eq!((st["req"], st["asks"]), (2, 1), "{st:?}");
    // The same request again only after work, and asked again then.
    assert!(!m.action_enabled("NextBox", &st), "{st:?}");
    for action in ["Answer", "Work", "NextBox", "Look"] {
        assert!(m.fire(action, &mut st), "{action} at {st:?}");
    }
    assert_eq!((st["req"], st["asks"]), (3, 1), "{st:?}");
}

/// The usage probe's ten defects, in the model's order, each with the one
/// law it breaks.
const USAGE_PROBE_DEFECTS: [(&str, &str); 10] = [
    ("EscOnTheOpenTimeout", "EscOnlyAtAPanel"),
    ("EscUnderAHand", "NothingWrittenUnderAHand"),
    ("EscIntoATurn", "NoEscapeIntoARunningTurn"),
    ("TypeUnderAHand", "NothingWrittenUnderAHand"),
    ("TypeOverAPanel", "NoSecondUsageOverAPanel"),
    ("TypeBooksAnAct", "TheProbeIsNoAct"),
    ("SampleUnsettled", "OnlyASettledPanelIsASample"),
    ("SampleNotRead", "NoSampleFromAPanelNotRead"),
    ("EscFailsUntold", "AStuckPanelIsTold"),
    ("GoesIdleAtTheLastTry", "AnOpenPanelIsOwed"),
];

/// THE `/usage` PROBE (`docs/SPEC-claude-accounts-probe-2026-10-01.md` §5):
/// the nine laws prove at `Buggy = 0` — `/usage` and the Escape are written
/// only where they land on the harness's own panel (never under a person's
/// hand, never over a panel or one still owed, the Escape never without the
/// panel on the grid and never into a running turn), a sample is taken only
/// from a settled panel the step read whole, the probe is no act of the
/// policy's, a stuck panel is told, and a panel the loop opened is never
/// forgotten — and each of the ten defects is caught at `Buggy = 1`, alone,
/// by its own law (the test below sweeps each one's whole space). A panel
/// the harness opened does not stand for good while nothing stands in the
/// Escape's way, under the two fairness assumptions the claim names (the
/// read ends; an Escape pressed for ever is taken), each load-bearing, and
/// broken alone by the loop that goes idle at its last try.
#[test]
fn the_usage_probe_writes_only_onto_its_own_panel_and_never_leaves_it_open() {
    use aterm_spec::{interp, verify};
    let m = aterm_spec::derive::supervisor_usage_probe_model();
    let live = aterm_spec::derive::supervisor_usage_probe_liveness();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == m.name),
        "the usage probe must stay enrolled in the spec-link registry"
    );
    assert!(
        aterm_spec::xref::liveness_registry()
            .iter()
            .any(|(registered, l)| registered.name == m.name && l.name == live.name),
        "its liveness claim must stay enrolled"
    );
    verify::prove_and_catch_scalar(
        &m,
        "usage probe: /usage and Escape only onto the harness's own panel, a sample only \
         from a settled panel read whole, a stuck panel told and never forgotten",
    );
    verify::liveness_proves_and_catches_tiered(
        &m,
        &live,
        "usage probe: a panel the harness opened does not stand for good",
    );
    // Every defect is a negative control caught alone, and no law is a ghost.
    let dead: Vec<&str> = USAGE_PROBE_DEFECTS.iter().map(|(d, _)| *d).collect();
    assert_eq!(
        verify::audit_dead_negative_controls(&m, &dead),
        Ok(dead.len()),
        "every defect is a negative control caught alone"
    );
    assert_eq!(verify::uncaught_invariants(&m), Vec::<&str>::new());

    // The states the real loop refuses, one by one.
    let at = |set: &[(&'static str, i64)]| {
        let mut st = m.init_state();
        for (k, v) in set {
            st.insert(*k, *v);
        }
        st
    };
    let claimed = [("ph", 1)];
    assert!(m.action_enabled("Claim", &at(&[])));
    assert!(m.action_enabled("Type", &at(&claimed)));
    for (why, extra) in [
        ("under a person's hand", ("hand", 1)),
        ("under a hold", ("hold", 1)),
        ("into a running turn", ("turn", 1)),
        ("over a panel", ("pan", 1)),
    ] {
        let mut set = claimed.to_vec();
        set.push(extra);
        assert!(!m.action_enabled("Type", &at(&set)), "/usage {why}");
        assert!(!m.action_enabled("Claim", &at(&[extra])), "a claim {why}");
        // The write skipped is always possible, and owes nothing.
        let mut skipped = at(&set);
        assert!(m.fire("TypeSkipped", &mut skipped), "{why}");
        assert_eq!(skipped["ph"], 0, "{why}");
    }
    for ph in [0, 2, 3] {
        assert!(
            !m.action_enabled("Type", &at(&[("ph", ph)])),
            "/usage with no claim, or with a panel owed (ph = {ph})"
        );
    }

    // The Escape: only with the panel on the grid, nobody's hand, no hold,
    // no turn — and only once the host was told, or for a panel that is not
    // this step's to read.
    let closing = [("ph", 3), ("pan", 1)];
    for esc in ["EscCloses", "EscFails"] {
        assert!(m.action_enabled(esc, &at(&closing)), "{esc}");
        assert!(
            m.action_enabled(esc, &at(&[("ph", 2), ("pan", 2), ("own", 0)])),
            "{esc}: a panel taken up from the ledger"
        );
        for (why, set) in [
            ("no panel", vec![("ph", 3), ("pan", 0)]),
            ("no panel yet", vec![("ph", 2), ("pan", 0), ("own", 0)]),
            ("a person's hand", vec![("ph", 3), ("pan", 1), ("hand", 1)]),
            ("a hold", vec![("ph", 3), ("pan", 1), ("hold", 1)]),
            ("a running turn", vec![("ph", 3), ("pan", 1), ("turn", 1)]),
            (
                "the read still in flight",
                vec![("ph", 2), ("pan", 2), ("own", 1)],
            ),
            ("nothing owed", vec![("ph", 0), ("pan", 1)]),
        ] {
            assert!(!m.action_enabled(esc, &at(&set)), "{esc}: {why}");
        }
    }

    // The sample: a settled panel this step read whole, and nothing else.
    let reading = [("ph", 2), ("pan", 2), ("own", 1)];
    assert!(m.action_enabled("Sample", &at(&reading)));
    for (why, set) in [
        (
            "an unsettled panel",
            vec![("ph", 2), ("pan", 1), ("own", 1)],
        ),
        (
            "a panel not read whole",
            vec![("ph", 2), ("pan", 2), ("own", 0)],
        ),
        (
            "a person's hand",
            vec![("ph", 2), ("pan", 2), ("own", 1), ("hand", 1)],
        ),
        ("no panel", vec![("ph", 2), ("pan", 0), ("own", 1)]),
        (
            "a panel already told of",
            vec![("ph", 3), ("pan", 2), ("own", 1)],
        ),
    ] {
        assert!(!m.action_enabled("Sample", &at(&set)), "{why}");
    }
    // A break in the read ends it: a person's key, a hold, a turn, a restart.
    for breaks in ["PersonTypes", "HoldOn", "TurnStarts", "LoopRestarts"] {
        let mut st = at(&reading);
        assert!(m.fire(breaks, &mut st), "{breaks}");
        assert_eq!(st["own"], 0, "{breaks}");
        assert!(!m.action_enabled("Sample", &st), "{breaks}: {st:?}");
    }

    // The panel that never opens costs no key: the loop goes idle, and no
    // mark of a write is left.
    let mut never = at(&[("ph", 2), ("own", 1)]);
    assert!(m.fire("GiveUpNotOpened", &mut never));
    assert_eq!(never, m.init_state());

    // THE LADDER: an Escape that does not take is tried again, the host is
    // told at the second failure — once — and the tries go on.
    let mut stuck = at(&closing);
    assert!(m.fire("EscFails", &mut stuck));
    assert_eq!((stuck["tries"], stuck["told"]), (1, 0));
    assert!(m.fire("EscFails", &mut stuck));
    assert_eq!((stuck["tries"], stuck["told"]), (2, 1));
    let told = stuck.clone();
    assert!(m.fire("EscFails", &mut stuck));
    assert_eq!(stuck, told, "past the last rung nothing more is told");
    assert!(
        m.action_enabled("EscCloses", &stuck),
        "and it is still tried"
    );
    assert!(
        !m.action_enabled("GoesIdleAtTheLastTry", &stuck),
        "the committed loop never gives a panel up"
    );

    // THE LOOP DIED: the successor owes the panel (the ledger's row), closes
    // it, and records nothing from it.
    let mut successor = stuck.clone();
    successor.insert("pan", 2);
    assert!(m.fire("LoopRestarts", &mut successor));
    assert_eq!(
        (
            successor["ph"],
            successor["own"],
            successor["tries"],
            successor["told"]
        ),
        (2, 0, 0, 0)
    );
    assert!(!m.action_enabled("Sample", &successor), "{successor:?}");
    assert!(m.fire("EscCloses", &mut successor));
    assert!(m.fire("SeesGone", &mut successor));
    assert_eq!(successor, m.init_state());
    // A claim not typed for is nothing to a successor.
    let mut unclaimed = at(&claimed);
    assert!(m.fire("LoopRestarts", &mut unclaimed));
    assert_eq!(unclaimed, m.init_state());

    // One probe end to end leaves no mark: the sound steps write the same
    // history every step of their kind does, and it is clean.
    let mut st = m.init_state();
    for action in [
        "Claim",
        "Type",
        "Opens",
        "Settles",
        "Sample",
        "EscCloses",
        "SeesGone",
    ] {
        assert!(m.fire(action, &mut st), "{action} at {st:?}");
    }
    assert_eq!(st, m.init_state());

    // EACH DEFECT, at the state it is the negative control of: the law holds
    // before, the committed loop has no such step, and the defect breaks that
    // law and no other.
    let buggy = interp::with_buggy(&m, 1);
    for (defect, law, set) in [
        (
            "EscOnTheOpenTimeout",
            "EscOnlyAtAPanel",
            vec![("ph", 2), ("own", 1)],
        ),
        (
            "EscUnderAHand",
            "NothingWrittenUnderAHand",
            vec![("ph", 3), ("pan", 1), ("hand", 1)],
        ),
        (
            "EscIntoATurn",
            "NoEscapeIntoARunningTurn",
            vec![("ph", 3), ("pan", 1), ("turn", 1)],
        ),
        (
            "TypeUnderAHand",
            "NothingWrittenUnderAHand",
            vec![("ph", 1), ("hand", 1)],
        ),
        (
            "TypeOverAPanel",
            "NoSecondUsageOverAPanel",
            vec![("ph", 3), ("pan", 2)],
        ),
        ("TypeBooksAnAct", "TheProbeIsNoAct", vec![("ph", 1)]),
        (
            "SampleUnsettled",
            "OnlyASettledPanelIsASample",
            vec![("ph", 2), ("pan", 1), ("own", 1)],
        ),
        (
            "SampleNotRead",
            "NoSampleFromAPanelNotRead",
            vec![("ph", 2), ("pan", 2), ("own", 0)],
        ),
        (
            "EscFailsUntold",
            "AStuckPanelIsTold",
            vec![("ph", 3), ("pan", 1), ("tries", 1)],
        ),
        (
            "GoesIdleAtTheLastTry",
            "AnOpenPanelIsOwed",
            vec![("ph", 3), ("pan", 1), ("tries", 2), ("told", 1)],
        ),
    ] {
        let mut wrong = at(&set);
        for inv in &m.invariants {
            assert!(
                m.check_invariant(inv.name, &wrong),
                "{defect}: {}",
                inv.name
            );
        }
        assert!(!m.action_enabled(defect, &wrong), "{defect} at Buggy = 0");
        assert!(buggy.fire(defect, &mut wrong), "{defect}: {wrong:?}");
        let broken: Vec<&str> = buggy
            .invariants
            .iter()
            .map(|inv| inv.name)
            .filter(|name| !buggy.check_invariant(name, &wrong))
            .collect();
        assert_eq!(broken, [law], "{defect}: {wrong:?}");
        // A defect fires only from a state no defect has touched.
        assert!(!buggy.action_enabled(defect, &wrong), "{defect} twice");
    }
}

/// EACH DEFECT OF THE USAGE PROBE, ALONE, OVER ITS WHOLE SPACE: the sound
/// loop with that one defect added breaks exactly the law it is the negative
/// control of, wherever the defect fires and whatever the sound steps do
/// after it. The defects, and nothing else, are dead at the committed
/// config; every law has a defect; and the whole `Buggy = 1` space fits the
/// interpreter's budget.
#[test]
fn each_usage_probe_defect_breaks_its_own_law_and_no_other_over_its_whole_space() {
    use aterm_spec::interp;
    let m = aterm_spec::derive::supervisor_usage_probe_model();
    let space = |m: &aterm_spec::derive::Model| {
        let mut bare = m.clone();
        bare.invariants.clear();
        interp::bmc(&bare).expect("no invariant, no violation")
    };
    let fired = interp::fired_actions(&m);
    let dead: Vec<&str> = m
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|a| !fired.contains(a))
        .collect();
    let defects: Vec<&str> = USAGE_PROBE_DEFECTS.iter().map(|(d, _)| *d).collect();
    assert_eq!(dead, defects, "the defects, and nothing else, are dead");
    assert!(interp::violated_invariants(&m).is_empty());
    for inv in &m.invariants {
        assert!(
            USAGE_PROBE_DEFECTS.iter().any(|(_, law)| *law == inv.name),
            "{}: no defect is its negative control",
            inv.name
        );
    }
    let sound = space(&m);
    for (defect, law) in USAGE_PROBE_DEFECTS {
        let alone = aterm_spec::verify::buggy_baseline_with(&m, defect);
        assert!(
            interp::fired_actions(&alone).contains(defect),
            "{defect} never fires"
        );
        let states = space(&alone);
        assert!(states > sound, "{defect} reaches nothing new");
        assert_eq!(
            interp::violated_invariants(&alone)
                .into_iter()
                .collect::<Vec<_>>(),
            [law],
            "{defect}, alone, over {states} states"
        );
    }
    let all = space(&interp::with_buggy(&m, 1));
    eprintln!(
        "SupervisorUsageProbe: {sound} states at Buggy = 0, {all} at Buggy = 1 (invariants \
         not stopping the walk)."
    );
    assert!(all < 100_000, "{all}");
}

/// The limit reset's seventeen defects, in the model's order, each with the
/// law it is the negative control of.
const LIMIT_RESET_DEFECTS: [(&str, &str); 17] = [
    ("TakeRowUnclaimed", "OneAttemptPerWall"),
    ("NeverReadsTheLine", "OneAttemptPerWall"),
    (
        "EnterTheInexactConfirmation",
        "EnterOnlyOnTheExactConfirmation",
    ),
    ("EnterInsideTheGuard", "EnterOnlyPastTheGuardOnYes"),
    ("EnterOffYes", "EnterOnlyPastTheGuardOnYes"),
    ("EnterAsASuccessor", "EnterOnlyPastTheGuardOnYes"),
    ("EnterOnALapsedClaim", "EnterOnlyPastTheGuardOnYes"),
    ("TakeRowByItsPlace", "NeverTheWeeklyRow"),
    ("TypesTheCommand", "NeverTheTypedCommand"),
    ("S2MovesUnderAReset", "NoMoveOffAnAccountBeingReset"),
    ("TakeRowUnderAHand", "NothingKeyedUnderAHand"),
    ("EnterUnderAHand", "NothingKeyedUnderAHand"),
    ("EscapeUnderAHand", "NothingKeyedUnderAHand"),
    ("PressWaitUnderAHand", "NothingKeyedUnderAHand"),
    ("UsedRecordedAtEnter", "OnlyTheLineSaysSpent"),
    ("ClaimsAgainAfterAFailure", "AFailureIsNotRetried"),
    ("StopsTryingItsEscape", "AnOpenConfirmationIsOwed"),
];

/// The one defect a second law sees: a wall claimed again after a failure
/// breaks `AFailureIsNotRetried` where the claim is made, and
/// `OneAttemptPerWall` a step later, where the sound `TakeRow` takes the row
/// of that claim.
const LIMIT_RESET_SEEN_TWICE: (&str, &str) = ("ClaimsAgainAfterAFailure", "OneAttemptPerWall");

/// The invariants of `m` that `st` breaks, by name.
fn limit_reset_broken(
    m: &aterm_spec::derive::Model,
    st: &std::collections::BTreeMap<&'static str, i64>,
) -> Vec<&'static str> {
    m.invariants
        .iter()
        .map(|inv| inv.name)
        .filter(|name| !m.check_invariant(name, st))
        .collect()
}

/// THE LIMIT RESET (`docs/SPEC-claude-accounts-resets-2026-10-02.md` §5):
/// the ten laws prove at `Buggy = 0` — the grant row is taken once a wall,
/// on a claim made for it, and never the weekly row in its slot; Enter is
/// written only on the exact confirmation of a row this loop took, past the
/// vendor's guard, with the focus on Yes and the claim in flight; no command
/// is typed and nothing is keyed under a person's hand; only the vendor's
/// line says a reset was spent; a kept or failed attempt is not claimed
/// again; nobody is moved off an account while its reset is in flight or
/// made; and a confirmation the loop opened is never forgotten — and each of
/// the seventeen defects is caught at `Buggy = 1` (the test below sweeps
/// each one's whole space). A claim does not stay in flight for good, and a
/// confirmation the harness opened does not stand for good while nothing
/// stands in the Escape's way, each under the three fairness assumptions its
/// claim names, each load-bearing, and each broken alone by its own mutant.
///
/// Nothing of this has been seen on a real wall: the vendor's side of the
/// model is READ in the 2.1.287 bundle.
#[test]
fn the_limit_reset_takes_one_row_a_wall_and_enters_only_its_own_exact_confirmation() {
    use aterm_spec::{interp, verify};
    let m = aterm_spec::derive::supervisor_limit_reset_model();
    let ends = aterm_spec::derive::supervisor_limit_reset_claim_liveness();
    let closes = aterm_spec::derive::supervisor_limit_reset_confirmation_liveness();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == m.name),
        "the limit reset must stay enrolled in the spec-link registry"
    );
    for live in [&ends, &closes] {
        assert!(
            aterm_spec::xref::liveness_registry()
                .iter()
                .any(|(registered, l)| registered.name == m.name && l.name == live.name),
            "{} must stay enrolled",
            live.name
        );
    }
    verify::prove_and_catch_scalar(
        &m,
        "limit reset: one row a wall, Enter only on the loop's own exact confirmation past \
         the guard, nothing typed or keyed under a hand, a confirmation never forgotten",
    );
    verify::liveness_proves_and_catches_tiered(
        &m,
        &ends,
        "limit reset: a claim does not stay in flight for good",
    );
    verify::liveness_proves_and_catches_tiered(
        &m,
        &closes,
        "limit reset: a confirmation the harness opened does not stand for good",
    );
    // Every defect is a negative control caught alone, and no law is a ghost.
    let dead: Vec<&str> = LIMIT_RESET_DEFECTS.iter().map(|(d, _)| *d).collect();
    assert_eq!(
        verify::audit_dead_negative_controls(&m, &dead),
        Ok(dead.len()),
        "every defect is a negative control caught alone"
    );
    assert_eq!(verify::uncaught_invariants(&m), Vec::<&str>::new());
    // Each liveness claim is broken by its own mutant and not by the other's.
    for (live, other) in [
        (&ends, "StopsTryingItsEscape"),
        (&closes, "NeverReadsTheLine"),
    ] {
        assert!(
            interp::nonprogress_under(&verify::buggy_baseline_with(&m, other), live).is_none(),
            "{other} breaks {} too",
            live.name
        );
    }

    let at = |set: &[(&'static str, i64)]| {
        let mut st = m.init_state();
        for (k, v) in set {
            st.insert(*k, *v);
        }
        st
    };
    let with = |base: &[(&'static str, i64)], extra: &[(&'static str, i64)]| {
        let mut set = base.to_vec();
        set.extend_from_slice(extra);
        at(&set)
    };

    // THE CLAIM AND THE ROW: only at the grant row, with nobody's hand on
    // the session, no hold, no step owed, and no claim made for this wall.
    let wall = [("dlg", 1)];
    let claimed = [("dlg", 1), ("claim", 1)];
    assert!(m.action_enabled("Claim", &at(&wall)));
    assert!(m.action_enabled("TakeRow", &at(&claimed)));
    for (why, extra) in [
        ("under a person's hand", ("hand", 1)),
        ("under a hold", ("hold", 1)),
        ("with the weekly row in the slot", ("dlg", 2)),
        ("with no dialog", ("dlg", 0)),
        ("under a step still owed", ("ph", 1)),
        ("on a wall already attempted", ("claim", 2)),
    ] {
        assert!(
            !m.action_enabled("Claim", &with(&wall, &[extra])),
            "a claim {why}"
        );
        assert!(
            !m.action_enabled("TakeRow", &with(&claimed, &[extra])),
            "the row {why}"
        );
    }
    assert!(
        !m.action_enabled("Claim", &at(&claimed)),
        "a second claim on one wall"
    );
    assert!(
        !m.action_enabled("TakeRow", &at(&wall)),
        "the row with no claim made for it"
    );

    // THE ENTER: the exact confirmation of a row this loop took, past the
    // guard, the focus on Yes, the claim in flight, nobody's hand, no hold.
    let exact = [
        ("dlg", 4),
        ("focus", 1),
        ("own", 1),
        ("claim", 1),
        ("ph", 1),
    ];
    for enter in ["Enter", "EnterDropped"] {
        assert!(m.action_enabled(enter, &at(&exact)), "{enter}");
        assert!(
            m.action_enabled(enter, &with(&exact, &[("ph", 2)])),
            "{enter}: a dropped Enter is judged again"
        );
        for (why, extra) in [
            ("inside the vendor's guard", ("guard", 1)),
            ("with the focus on No", ("focus", 0)),
            ("on a row an earlier loop took", ("own", 0)),
            ("on a claim that lapsed", ("claim", 2)),
            ("on a confirmation it must keep", ("dlg", 5)),
            ("under a person's hand", ("hand", 1)),
            ("under a hold", ("hold", 1)),
            ("after its Escape", ("ph", 3)),
            ("with no step owed", ("ph", 0)),
            ("at `Checking…`", ("dlg", 3)),
            ("at `Resetting…`", ("dlg", 6)),
        ] {
            assert!(
                !m.action_enabled(enter, &with(&exact, &[extra])),
                "{enter} {why}"
            );
        }
    }

    // THE ESCAPE: at any confirmation of a step that is owed — the exact one
    // too, a successor's too, a lapsed claim's too — and at `Checking…`.
    for (what, set) in [
        ("the exact confirmation", exact.to_vec()),
        ("one it must keep", with_vec(&exact, ("dlg", 5))),
        ("the focus on No", with_vec(&exact, ("focus", 0))),
        ("a successor's", with_vec(&exact, ("own", 0))),
        ("a lapsed claim's", with_vec(&exact, ("claim", 2))),
        ("after a dropped Enter", with_vec(&exact, ("ph", 2))),
        ("after a dropped Escape", with_vec(&exact, ("ph", 3))),
    ] {
        for esc in ["Escape", "EscapeDropped"] {
            assert!(m.action_enabled(esc, &at(&set)), "{esc}: {what}");
            for (why, extra) in [
                ("inside the vendor's guard", ("guard", 1)),
                ("under a person's hand", ("hand", 1)),
                ("under a hold", ("hold", 1)),
                ("with no step owed", ("ph", 0)),
                ("at `Resetting…`", ("dlg", 6)),
                ("with no dialog", ("dlg", 0)),
                ("at the wall dialog", ("dlg", 1)),
            ] {
                assert!(
                    !m.action_enabled(esc, &with(&set, &[extra])),
                    "{esc} at {what}, {why}"
                );
            }
        }
    }
    let checking = [("dlg", 3), ("claim", 1), ("ph", 1), ("own", 1)];
    assert!(m.action_enabled("Escape", &at(&checking)));
    assert!(
        !m.action_enabled("EscapeDropped", &at(&checking)),
        "an Escape at `Checking…` closes it"
    );
    for extra in [("hand", 1), ("hold", 1), ("ph", 0)] {
        assert!(
            !m.action_enabled("Escape", &with(&checking, &[extra])),
            "{extra:?}"
        );
    }

    // `Resetting…` IS NEVER KEYED, and nothing is keyed under a hand or a
    // hold wherever the loop stands.
    let keys = [
        "TakeRow",
        "Enter",
        "EnterDropped",
        "Escape",
        "EscapeDropped",
        "PressWait",
    ];
    for key in keys {
        assert!(
            !m.action_enabled(key, &at(&[("dlg", 6), ("claim", 1), ("ph", 2), ("own", 1)])),
            "{key} at `Resetting…`"
        );
        for dlg in 0..=6 {
            for ph in 0..=3 {
                for stop in ["hand", "hold"] {
                    let st = at(&[
                        ("dlg", dlg),
                        ("ph", ph),
                        ("claim", 1),
                        ("own", 1),
                        ("focus", 1),
                        (stop, 1),
                    ]);
                    assert!(!m.action_enabled(key, &st), "{key} under a {stop}: {st:?}");
                }
            }
        }
    }

    // THE STEP'S END: read only once no pending line and no confirmation is
    // on the screen, and only for a step that is owed.
    for dlg in 0..=6 {
        for ph in 0..=3 {
            assert_eq!(
                m.action_enabled("ReadLine", &at(&[("dlg", dlg), ("ph", ph), ("claim", 1)])),
                ph > 0 && dlg <= 2,
                "dlg = {dlg}, ph = {ph}"
            );
            // The wait row: at the wall dialog, with no step owed.
            assert_eq!(
                m.action_enabled("PressWait", &at(&[("dlg", dlg), ("ph", ph)])),
                ph == 0 && (dlg == 1 || dlg == 2),
                "dlg = {dlg}, ph = {ph}"
            );
        }
    }

    // THE OTHER SESSION AT THE WALL gets exactly one advice: a moment's wait
    // while the claim is in flight, its continuation once a spent or lifted
    // line is recorded, a move otherwise.
    for claim in 0..=2 {
        for line in 0..=4 {
            let st = at(&[("s2", 1), ("claim", claim), ("line", line)]);
            let advice: Vec<&str> = ["S2Waits", "S2Resumes", "S2Moves"]
                .into_iter()
                .filter(|a| m.action_enabled(a, &st))
                .collect();
            let expected = if claim == 1 {
                "S2Waits"
            } else if line == 1 || line == 2 {
                "S2Resumes"
            } else {
                "S2Moves"
            };
            // A line with no claim is no state of the model's.
            if claim > 0 || line == 0 {
                assert_eq!(advice, [expected], "claim = {claim}, line = {line}");
            }
        }
    }

    // THE CLOCK does not run out over a reset in the making (the model's one
    // assumption about time), and ends every other claim.
    for dlg in 0..=6 {
        for line in 0..=4 {
            assert_eq!(
                m.action_enabled(
                    "ClaimLapses",
                    &at(&[("claim", 1), ("dlg", dlg), ("line", line)])
                ),
                dlg <= 5 && line != 1 && line != 2,
                "dlg = {dlg}, line = {line}"
            );
        }
    }

    let run = |path: &[&str]| {
        let mut st = m.init_state();
        for action in path {
            assert!(m.fire(action, &mut st), "{action} at {st:?}");
        }
        st
    };
    let opened = [
        "WallOpens",
        "Claim",
        "TakeRow",
        "OpensTheConfirmation",
        "GuardEnds",
    ];
    let then = |more: &[&'static str]| {
        let mut path = opened.to_vec();
        path.extend_from_slice(more);
        run(&path)
    };

    // ONE RESET END TO END leaves no mark: the claim is done, the line says
    // spent, the other session continues where it is — and the dialog's
    // next mount at that wall is answered by the wait row.
    let mut used = then(&["Enter", "ServerAnswersSpent", "ReadLine"]);
    let mut clean = m.init_state();
    clean.insert("claim", 2);
    clean.insert("line", 1);
    assert_eq!(used, clean);
    for action in ["S2AtWall", "S2Resumes", "WallOpens"] {
        assert!(m.fire(action, &mut used), "{action} at {used:?}");
    }
    assert!(!m.action_enabled("Claim", &used) && !m.action_enabled("TakeRow", &used));
    assert!(m.fire("PressWait", &mut used));
    assert_eq!((used["s2"], used["dlg"]), (3, 0));

    // THE EARLY-USE CONFIRMATION is Escaped and never Entered; the kept
    // line is final, and the other session is moved as ever.
    let mut kept = run(&[
        "WallOpens",
        "Claim",
        "TakeRow",
        "OpensAnInexactConfirmation",
        "GuardEnds",
    ]);
    assert!(!m.action_enabled("Enter", &kept) && !m.action_enabled("EnterDropped", &kept));
    for action in ["Escape", "ReadLine", "WallOpens"] {
        assert!(m.fire(action, &mut kept), "{action} at {kept:?}");
    }
    assert_eq!((kept["line"], kept["claim"], kept["ph"]), (3, 2, 0));
    assert!(!m.action_enabled("Claim", &kept) && !m.action_enabled("TakeRow", &kept));
    for action in ["PressWait", "S2AtWall", "S2Moves"] {
        assert!(m.fire(action, &mut kept), "{action} at {kept:?}");
    }
    assert_eq!(limit_reset_broken(&m, &kept), Vec::<&str>::new());

    // AN ENTER THE VENDOR DROPS is judged again, and an Escape may follow.
    let mut dropped = then(&["EnterDropped"]);
    assert_eq!((dropped["dlg"], dropped["ph"]), (4, 2));
    assert!(m.action_enabled("Enter", &dropped));
    assert!(m.fire("Escape", &mut dropped) && m.fire("ReadLine", &mut dropped));
    assert_eq!((dropped["line"], dropped["claim"]), (3, 2));

    // AN ESCAPE THAT DOES NOT TAKE is pressed again, and never an Enter.
    let mut stuck = then(&["EscapeDropped"]);
    assert_eq!((stuck["dlg"], stuck["ph"]), (4, 3));
    assert!(!m.action_enabled("Enter", &stuck), "{stuck:?}");
    assert!(
        !m.action_enabled("StopsTryingItsEscape", &stuck),
        "the committed loop never gives a confirmation up"
    );
    assert!(m.fire("EscapeDropped", &mut stuck) && m.fire("Escape", &mut stuck));

    // THE LOOP DIED: the successor owes the confirmation (the ledger's row),
    // never Enters it, and Escapes it.
    let mut successor = then(&["LoopRestarts"]);
    assert_eq!((successor["ph"], successor["own"]), (1, 0));
    assert!(!m.action_enabled("Enter", &successor), "{successor:?}");
    assert!(m.fire("Escape", &mut successor) && m.fire("ReadLine", &mut successor));
    assert_eq!((successor["line"], successor["claim"]), (3, 2));
    // …and one that died after its Enter waits the claiming line out.
    let mut waited = then(&["Enter", "LoopRestarts"]);
    for key in keys {
        assert!(!m.action_enabled(key, &waited), "{key} at {waited:?}");
    }
    assert!(m.fire("ServerAnswersLifted", &mut waited) && m.fire("ReadLine", &mut waited));
    assert_eq!((waited["line"], waited["claim"], waited["ph"]), (2, 2, 0));

    // THE CLAIM LAPSED under the confirmation (a person held it ten
    // minutes): the other session is free to move, so the Enter is gone.
    let mut lapsed = then(&["S2AtWall"]);
    assert!(m.action_enabled("S2Waits", &lapsed) && !m.action_enabled("S2Moves", &lapsed));
    assert!(m.fire("ClaimLapses", &mut lapsed) && m.fire("S2Moves", &mut lapsed));
    assert!(!m.action_enabled("Enter", &lapsed), "{lapsed:?}");
    assert!(m.fire("Escape", &mut lapsed));
    assert_eq!(limit_reset_broken(&m, &lapsed), Vec::<&str>::new());

    // A PERSON'S KEY at the confirmation holds every key until the grace
    // has passed; then the same confirmation is judged again.
    let mut held = then(&["PersonTypes"]);
    for key in keys {
        assert!(!m.action_enabled(key, &held), "{key} at {held:?}");
    }
    assert!(m.fire("GraceEnds", &mut held) && m.fire("Enter", &mut held));

    // THE ROW'S CHECK may answer with a line and no confirmation, and a
    // `Checking…` that outlasts its bound is Escaped, which says nothing.
    let answered = run(&["WallOpens", "Claim", "TakeRow", "OpensAnAnswer", "ReadLine"]);
    assert_eq!((answered["line"], answered["claim"]), (4, 2));
    let unchecked = run(&["WallOpens", "Claim", "TakeRow", "Escape", "ReadLine"]);
    assert_eq!((unchecked["line"], unchecked["claim"]), (0, 2));

    // A CLAIM WHOSE ROW WAS NEVER TAKEN is ended by the clock, and the
    // wall is attempted all the same.
    let mut untaken = run(&[
        "WallOpens",
        "Claim",
        "PersonTypes",
        "ClaimLapses",
        "GraceEnds",
    ]);
    assert!(!m.action_enabled("TakeRow", &untaken) && !m.action_enabled("Claim", &untaken));
    assert!(m.fire("PressWait", &mut untaken));

    // EACH DEFECT, at the state it is the negative control of: every law
    // holds before, the committed loop has no such step, and the defect
    // breaks its law and no other — at once, or (the two noted) at the
    // sound step that follows it.
    let buggy = interp::with_buggy(&m, 1);
    let conf = |extra: &[(&'static str, i64)]| with_vec_all(&exact, extra);
    for (defect, law, set, after) in [
        (
            "TakeRowUnclaimed",
            "OneAttemptPerWall",
            vec![("dlg", 1), ("claim", 2), ("line", 3)],
            &[][..],
        ),
        (
            "NeverReadsTheLine",
            "OneAttemptPerWall",
            vec![("claim", 1), ("ph", 2), ("own", 1), ("line", 1)],
            &["WallOpens", "TakeRow"][..],
        ),
        (
            "EnterTheInexactConfirmation",
            "EnterOnlyOnTheExactConfirmation",
            conf(&[("dlg", 5)]),
            &[][..],
        ),
        (
            "EnterInsideTheGuard",
            "EnterOnlyPastTheGuardOnYes",
            conf(&[("guard", 1)]),
            &[][..],
        ),
        (
            "EnterOffYes",
            "EnterOnlyPastTheGuardOnYes",
            conf(&[("focus", 0)]),
            &[][..],
        ),
        (
            "EnterAsASuccessor",
            "EnterOnlyPastTheGuardOnYes",
            conf(&[("own", 0)]),
            &[][..],
        ),
        (
            "EnterOnALapsedClaim",
            "EnterOnlyPastTheGuardOnYes",
            conf(&[("claim", 2)]),
            &[][..],
        ),
        (
            "TakeRowByItsPlace",
            "NeverTheWeeklyRow",
            vec![("dlg", 2), ("claim", 1)],
            &[][..],
        ),
        ("TypesTheCommand", "NeverTheTypedCommand", vec![], &[][..]),
        (
            "S2MovesUnderAReset",
            "NoMoveOffAnAccountBeingReset",
            vec![("s2", 1), ("dlg", 1), ("claim", 1)],
            &[][..],
        ),
        (
            "TakeRowUnderAHand",
            "NothingKeyedUnderAHand",
            vec![("dlg", 1), ("claim", 1), ("hand", 1)],
            &[][..],
        ),
        (
            "EnterUnderAHand",
            "NothingKeyedUnderAHand",
            conf(&[("hand", 1)]),
            &[][..],
        ),
        (
            "EscapeUnderAHand",
            "NothingKeyedUnderAHand",
            conf(&[("dlg", 5), ("hand", 1)]),
            &[][..],
        ),
        (
            "PressWaitUnderAHand",
            "NothingKeyedUnderAHand",
            vec![("dlg", 2), ("hand", 1)],
            &[][..],
        ),
        (
            "UsedRecordedAtEnter",
            "OnlyTheLineSaysSpent",
            conf(&[]),
            &[][..],
        ),
        (
            "ClaimsAgainAfterAFailure",
            "AFailureIsNotRetried",
            vec![("dlg", 1), ("claim", 2), ("line", 3)],
            &[][..],
        ),
        (
            "StopsTryingItsEscape",
            "AnOpenConfirmationIsOwed",
            conf(&[("ph", 3)]),
            &[][..],
        ),
    ] {
        let mut wrong = at(&set);
        assert_eq!(
            limit_reset_broken(&m, &wrong),
            Vec::<&str>::new(),
            "before {defect}"
        );
        assert!(!m.action_enabled(defect, &wrong), "{defect} at Buggy = 0");
        assert!(buggy.fire(defect, &mut wrong), "{defect}: {wrong:?}");
        if !after.is_empty() {
            assert_eq!(
                limit_reset_broken(&buggy, &wrong),
                Vec::<&str>::new(),
                "{defect} is caught at the step that follows it: {wrong:?}"
            );
        }
        for action in after {
            assert!(buggy.fire(action, &mut wrong), "{action}: {wrong:?}");
        }
        assert_eq!(
            limit_reset_broken(&buggy, &wrong),
            [law],
            "{defect}: {wrong:?}"
        );
        // A defect fires only from a state no defect has touched.
        assert!(!buggy.action_enabled(defect, &wrong), "{defect} twice");
    }
    // THE FOUR MUTANTS THE TIER-1 WALK NAMES (the spec's §5): the loop that
    // Enters the early-use confirmation, the one that takes the row by its
    // place, the one that keys under a hand, the one that Enters inside the
    // guard. At the state its key arrives in, the sound key is not enabled
    // and exactly one defect of that key is: the name a projection reports.
    for (key, set, defect) in [
        ("Enter", conf(&[("dlg", 5)]), "EnterTheInexactConfirmation"),
        (
            "TakeRow",
            vec![("dlg", 2), ("claim", 1)],
            "TakeRowByItsPlace",
        ),
        (
            "TakeRow",
            vec![("dlg", 1), ("claim", 1), ("hand", 1)],
            "TakeRowUnderAHand",
        ),
        ("Enter", conf(&[("hand", 1)]), "EnterUnderAHand"),
        (
            "Escape",
            conf(&[("dlg", 5), ("hand", 1)]),
            "EscapeUnderAHand",
        ),
        (
            "PressWait",
            vec![("dlg", 1), ("claim", 2), ("hand", 1)],
            "PressWaitUnderAHand",
        ),
        ("Enter", conf(&[("guard", 1)]), "EnterInsideTheGuard"),
    ] {
        let st = at(&set);
        assert!(!m.action_enabled(key, &st), "{key} at {st:?}");
        let named: Vec<&str> = dead
            .iter()
            .copied()
            .filter(|d| d.starts_with(key) && buggy.action_enabled(d, &st))
            .collect();
        assert_eq!(named, [defect], "{key} at {st:?}");
    }
    // The claim made again is seen a second time where its row is taken.
    let (again, second) = LIMIT_RESET_SEEN_TWICE;
    let mut retried = at(&[("dlg", 1), ("claim", 2), ("line", 3)]);
    assert!(buggy.fire(again, &mut retried) && buggy.fire("TakeRow", &mut retried));
    assert_eq!(
        limit_reset_broken(&buggy, &retried),
        [second, "AFailureIsNotRetried"],
        "{retried:?}"
    );
}

/// `set` with one more variable written (a later write of a name wins).
fn with_vec(set: &[(&'static str, i64)], extra: (&'static str, i64)) -> Vec<(&'static str, i64)> {
    with_vec_all(set, &[extra])
}

/// `set` with `extra` written after it.
fn with_vec_all(
    set: &[(&'static str, i64)],
    extra: &[(&'static str, i64)],
) -> Vec<(&'static str, i64)> {
    let mut all = set.to_vec();
    all.extend_from_slice(extra);
    all
}

/// EACH DEFECT OF THE LIMIT RESET, ALONE, OVER ITS WHOLE SPACE: the sound
/// loop with that one defect added breaks the law it is the negative control
/// of and no other, wherever the defect fires and whatever the sound steps
/// do after it (the claim made again after a failure is seen by a second
/// law, where its row is taken: [`LIMIT_RESET_SEEN_TWICE`]). The defects,
/// and nothing else, are dead at the committed config; every law has a
/// defect; and the whole `Buggy = 1` space fits the interpreter's budget.
#[test]
fn each_limit_reset_defect_breaks_its_own_law_over_its_whole_space() {
    use aterm_spec::interp;
    let started = std::time::Instant::now();
    let m = aterm_spec::derive::supervisor_limit_reset_model();
    let space = |m: &aterm_spec::derive::Model| {
        let mut bare = m.clone();
        bare.invariants.clear();
        interp::bmc(&bare).expect("no invariant, no violation")
    };
    let fired = interp::fired_actions(&m);
    let dead: Vec<&str> = m
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|a| !fired.contains(a))
        .collect();
    let defects: Vec<&str> = LIMIT_RESET_DEFECTS.iter().map(|(d, _)| *d).collect();
    assert_eq!(dead, defects, "the defects, and nothing else, are dead");
    assert!(interp::violated_invariants(&m).is_empty());
    for inv in &m.invariants {
        assert!(
            LIMIT_RESET_DEFECTS.iter().any(|(_, law)| *law == inv.name),
            "{}: no defect is its negative control",
            inv.name
        );
    }
    let sound = space(&m);
    for (defect, law) in LIMIT_RESET_DEFECTS {
        let alone = aterm_spec::verify::buggy_baseline_with(&m, defect);
        assert!(
            interp::fired_actions(&alone).contains(defect),
            "{defect} never fires"
        );
        let states = space(&alone);
        assert!(states > sound, "{defect} reaches nothing new");
        let mut laws = vec![law];
        if defect == LIMIT_RESET_SEEN_TWICE.0 {
            laws.push(LIMIT_RESET_SEEN_TWICE.1);
        }
        laws.sort_unstable();
        assert_eq!(
            interp::violated_invariants(&alone)
                .into_iter()
                .collect::<Vec<_>>(),
            laws,
            "{defect}, alone, over {states} states"
        );
    }
    let all = space(&interp::with_buggy(&m, 1));
    eprintln!(
        "SupervisorLimitReset: {sound} states at Buggy = 0, {all} at Buggy = 1 (invariants \
         not stopping the walk), every defect's own space walked in {:?}.",
        started.elapsed()
    );
    assert!(all < 100_000, "{all}");
}

/// THE LAWS ARE ABOUT THE SOUND KEYS, NOT ABOUT A DEFECT'S REPORT OF ITSELF.
/// Every take of a row, every Enter, every Escape, every press of the wait
/// row, every claim and every move of the other session writes its history
/// from the state before it, so the model's OWN step with a guard dropped IS
/// the violation: here each sound step is rebuilt at `Buggy = 0` with one
/// conjunct of its guard gone, and its law breaks with no defect action ever
/// firing. The same step rebuilt with nothing dropped breaks nothing and
/// walks the committed space, state for state.
///
/// Two laws have no sound step to edit: `NeverTheTypedCommand` and
/// `OnlyTheLineSaysSpent` are over a mark only a defect writes — the sound
/// loop has no step that types, and none that records a spend but
/// `ReadLine` at the spent line.
#[test]
fn the_limit_reset_laws_break_when_a_sound_step_drops_a_guard() {
    use aterm_spec::derive::{Expr, Model, and_, eq, gt, int, le, or_, var};
    use aterm_spec::interp;
    let m = aterm_spec::derive::supervisor_limit_reset_model();
    let committed = interp::bmc(&m).expect("the committed model proves");
    let is = |name: &'static str, k: i64| eq(var(name), int(k));
    let all = |conjuncts: Vec<Expr>| conjuncts.into_iter().reduce(and_).expect("a conjunct");
    // `m` with each named step's guard replaced, and what that breaks.
    let breaks = |guards: Vec<(&str, Expr)>| {
        let mut edited: Model = m.clone();
        for (action, guard) in guards {
            let step = edited
                .actions
                .iter_mut()
                .find(|a| a.name == action)
                .unwrap_or_else(|| panic!("no action {action}"));
            step.guard = Some(guard);
        }
        let fired = interp::fired_actions(&edited);
        for (defect, _) in LIMIT_RESET_DEFECTS {
            assert!(!fired.contains(defect), "{defect} fired at Buggy = 0");
        }
        let mut bare = edited.clone();
        bare.invariants.clear();
        (
            interp::violated_invariants(&edited)
                .into_iter()
                .collect::<Vec<_>>(),
            interp::bmc(&bare).expect("no invariant, no violation"),
        )
    };
    // Each sound guard, conjunct by conjunct; `Some(i)` drops the i-th.
    let unless = |dropped: Option<usize>, conjuncts: Vec<Expr>| {
        all(conjuncts
            .into_iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != dropped)
            .map(|(_, c)| c)
            .collect())
    };
    let no_hand = || vec![is("hand", 0), is("hold", 0)];
    let claim = |dropped, dlg: Expr| {
        unless(
            dropped,
            [vec![is("claim", 0), is("ph", 0), dlg], no_hand()].concat(),
        )
    };
    let take = |dropped, dlg: Expr| {
        unless(
            dropped,
            [vec![is("claim", 1), is("ph", 0), dlg], no_hand()].concat(),
        )
    };
    let enter = |dropped, dlg: Expr| {
        unless(
            dropped,
            [
                vec![
                    dlg,
                    is("guard", 0),
                    is("focus", 1),
                    is("own", 1),
                    is("claim", 1),
                    or_(is("ph", 1), is("ph", 2)),
                ],
                no_hand(),
            ]
            .concat(),
        )
    };
    let escape = |dropped| {
        unless(
            dropped,
            [
                vec![
                    gt(var("ph"), int(0)),
                    or_(
                        is("dlg", 3),
                        and_(or_(is("dlg", 4), is("dlg", 5)), is("guard", 0)),
                    ),
                ],
                no_hand(),
            ]
            .concat(),
        )
    };
    let wait = |dropped| {
        unless(
            dropped,
            [
                vec![or_(is("dlg", 1), is("dlg", 2)), is("ph", 0)],
                no_hand(),
            ]
            .concat(),
        )
    };
    let either_row = || or_(is("dlg", 1), is("dlg", 2));

    // NOTHING DROPPED: the rebuilt guards are the model's own.
    assert_eq!(
        breaks(vec![
            ("Claim", claim(None, is("dlg", 1))),
            ("TakeRow", take(None, is("dlg", 1))),
            ("Enter", enter(None, is("dlg", 4))),
            ("EnterDropped", enter(None, is("dlg", 4))),
            ("Escape", escape(None)),
            ("PressWait", wait(None)),
        ]),
        (vec![], committed)
    );

    for (what, guards, laws) in [
        (
            "the row taken with no claim in flight for it",
            vec![("TakeRow", take(Some(0), is("dlg", 1)))],
            vec!["OneAttemptPerWall"],
        ),
        (
            "the row taken under a step still owed",
            vec![("TakeRow", take(Some(1), is("dlg", 1)))],
            vec!["OneAttemptPerWall"],
        ),
        (
            "the row taken under a person's hand",
            vec![("TakeRow", take(Some(3), is("dlg", 1)))],
            vec!["NothingKeyedUnderAHand"],
        ),
        (
            "the row matched by its place",
            vec![
                ("Claim", claim(None, either_row())),
                ("TakeRow", take(None, either_row())),
            ],
            vec!["NeverTheWeeklyRow"],
        ),
        (
            "a wall claimed again",
            vec![(
                "Claim",
                all([
                    vec![
                        or_(is("claim", 0), is("claim", 2)),
                        is("ph", 0),
                        is("dlg", 1),
                    ],
                    no_hand(),
                ]
                .concat()),
            )],
            vec!["AFailureIsNotRetried", "OneAttemptPerWall"],
        ),
        (
            "Enter on a confirmation it must keep",
            vec![("Enter", enter(None, or_(is("dlg", 4), is("dlg", 5))))],
            vec!["EnterOnlyOnTheExactConfirmation"],
        ),
        (
            "Enter inside the guard",
            vec![("Enter", enter(Some(1), is("dlg", 4)))],
            vec!["EnterOnlyPastTheGuardOnYes"],
        ),
        (
            "Enter with the focus on No",
            vec![("Enter", enter(Some(2), is("dlg", 4)))],
            vec!["EnterOnlyPastTheGuardOnYes"],
        ),
        (
            "Enter by a successor",
            vec![("Enter", enter(Some(3), is("dlg", 4)))],
            vec!["EnterOnlyPastTheGuardOnYes"],
        ),
        (
            "Enter on a lapsed claim",
            vec![("Enter", enter(Some(4), is("dlg", 4)))],
            vec!["EnterOnlyPastTheGuardOnYes"],
        ),
        (
            "Enter under a person's hand",
            vec![("Enter", enter(Some(6), is("dlg", 4)))],
            vec!["NothingKeyedUnderAHand"],
        ),
        (
            "a dropped Enter under a person's hand",
            vec![("EnterDropped", enter(Some(6), is("dlg", 4)))],
            vec!["NothingKeyedUnderAHand"],
        ),
        (
            "Escape under a person's hand",
            vec![("Escape", escape(Some(2)))],
            vec!["NothingKeyedUnderAHand"],
        ),
        (
            "the wait row under a person's hand",
            vec![("PressWait", wait(Some(2)))],
            vec!["NothingKeyedUnderAHand"],
        ),
        (
            "the other session moved whatever the claim says",
            vec![("S2Moves", is("s2", 1))],
            vec!["NoMoveOffAnAccountBeingReset"],
        ),
        (
            "the step ended with its confirmation up",
            vec![("ReadLine", gt(var("ph"), int(0)))],
            vec!["AnOpenConfirmationIsOwed"],
        ),
    ] {
        let (broken, states) = breaks(guards);
        assert_eq!(broken, laws, "{what}, over {states} states");
    }
    // The control of the control: `ReadLine`'s own guard, rebuilt, is clean.
    assert_eq!(
        breaks(vec![(
            "ReadLine",
            and_(gt(var("ph"), int(0)), le(var("dlg"), int(2)))
        )]),
        (vec![], committed)
    );
}
