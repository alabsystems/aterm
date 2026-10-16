// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `NativeUpdateLaunchedDialWait`: the outgoing
//! process's wait for its launched successor's dial ends on LaunchServices'
//! "terminated before it checked in", and on the kernel's word that a pid
//! LaunchServices named is gone — promptly, without the dial budget — and
//! never on an answer a live successor gets; the answer it peeks stays for the
//! corroboration after a dial. Each mutant (any answer read as death, a named
//! pid read as death, a consuming pre-dial poll, the retired cancel-only
//! abort, the abort that never asked about a named pid) is caught.
//! Tier-1 is aterm-gui's `app_update_handoff::launched_dial_wait_conformance`.

use aterm_spec::{derive::native_update_launched_dial_wait_model, interp, verify};

/// Each invariant mutant with the invariant it was written for.
const MUTANTS: [(&str, &str); 5] = [
    ("EndOnAnyAnswer", "NeverRefusesALiveSuccessor"),
    ("EndOnNamedPid", "NeverRefusesALiveSuccessor"),
    ("ConsumingPoll", "AnswerKeptForTheCorroboration"),
    // These two are also the liveness property's mutants (below).
    ("PollCancelOnly", "AReportedDeathIsSeen"),
    ("IgnoreNamedPid", "ANamedDeathIsSeen"),
];

/// Every invariant is falsified by its own mutant ALONE, so none is carried by
/// another's counterexample.
#[test]
fn each_mutant_alone_breaks_the_invariant_it_was_written_for() {
    for (mutant, invariant) in MUTANTS {
        match interp::bmc(&verify::buggy_baseline_with(
            &native_update_launched_dial_wait_model(),
            mutant,
        )) {
            Err((_, violated)) => assert_eq!(
                violated, invariant,
                "{mutant} alone breaks {invariant}, not {violated}"
            ),
            Ok(states) => panic!("{mutant} alone broke nothing over {states} states"),
        }
    }
    // And with no mutant live the Buggy = 1 model is clean: the catches above
    // are the mutants'.
    let mut clean = interp::with_buggy(&native_update_launched_dial_wait_model(), 1);
    clean
        .actions
        .retain(|action| !MUTANTS.iter().any(|(name, _)| *name == action.name));
    assert!(interp::bmc(&clean).is_ok());
}

/// The walks the change is about, fired action by action.
#[test]
fn a_dead_successor_ends_the_wait_and_a_live_one_is_accepted() {
    let model = native_update_launched_dial_wait_model();

    // Dead before the dial: LaunchServices says so, and the wait ends on it
    // with the budget unspent; the answer is still in the slot.
    let mut dead = model.init_state();
    for action in ["Die", "AnswerExited", "EndOnExit"] {
        assert!(model.fire(action, &mut dead), "{action}: {dead:?}");
    }
    assert_eq!(
        (dead["wait"], dead["expired"], dead["slot"]),
        (3, 0, 1),
        "{dead:?}"
    );

    // Named, then dead before the dial: LaunchServices says nothing more, and
    // the kernel's word about the named pid ends the wait, budget unspent.
    let mut named = model.init_state();
    assert!(model.fire("AnswerNamed", &mut named));
    assert!(
        !model.action_enabled("EndOnNamedExit", &named),
        "a named pid alone never ends the wait: {named:?}"
    );
    for action in ["Die", "EndOnNamedExit"] {
        assert!(model.fire(action, &mut named), "{action}: {named:?}");
    }
    assert_eq!(
        (named["wait"], named["expired"], named["slot"]),
        (3, 0, 1),
        "{named:?}"
    );

    // Alive, answered with an error or its pid before its dial: neither arm
    // ends the wait, the dial is accepted, and the corroboration takes the
    // answer the wait left.
    for answer in ["AnswerOther", "AnswerNamed"] {
        let mut live = model.init_state();
        assert!(model.fire(answer, &mut live));
        for arm in ["EndOnExit", "EndOnNamedExit"] {
            assert!(
                !model.action_enabled(arm, &live),
                "{arm} never ends the wait for a live successor: {live:?}"
            );
        }
        for action in ["Dial", "Accept", "Corroborate"] {
            assert!(model.fire(action, &mut live), "{action}: {live:?}");
        }
        assert_eq!((live["wait"], live["taken"]), (1, 1), "{live:?}");
    }

    // Dead after LaunchServices answered something that names nothing (an
    // error): nothing proves the death, so only the budget ends the wait (the
    // stated limit of the liveness claim).
    let mut late = model.init_state();
    for action in ["AnswerOther", "Die"] {
        assert!(model.fire(action, &mut late), "{action}: {late:?}");
    }
    for arm in ["EndOnExit", "EndOnNamedExit", "AnswerExited"] {
        assert!(!model.action_enabled(arm, &late), "{arm}: {late:?}");
    }
    for action in ["Expire", "TimeOut"] {
        assert!(model.fire(action, &mut late), "{action}: {late:?}");
    }
    // The negative controls: each mutant alone, by BMC, above.
}
