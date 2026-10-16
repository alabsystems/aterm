// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::{Model, atpkg_pending_wait_model};
use aterm_spec::interp::{self, State};
use aterm_spec::verify;

/// Fire `actions` in order from the initial state, each of which must be enabled.
fn run(model: &Model, actions: &[&str]) -> State {
    fire_all(model, model.init_state(), actions)
}

/// The pending stub's wait proves over the whole bounded space and catches each of the
/// readers' mistakes: looking at the shim before the pass (a pass that sets the program up
/// and ends between the reads made it say "the install stopped" about an installed
/// program), looking at the pass before the promise (a pass that begins and takes the
/// promise over between the reads made it say so about a running pass, and a pass read
/// before its plan, the promise read gone after it, made a plan that has the program read
/// as one that left it out), and taking a pass that ended without a recorded failure for an
/// install — and the one writer mistake the promise's hand-over between two passes exists to
/// avoid: retiring it while its pass runs unplanned.
#[test]
fn a_waiting_stub_reads_the_promise_then_the_pass_then_the_shim() {
    let model = atpkg_pending_wait_model();
    verify::prove_and_catch_scalar(&model, "atpkg pending stub wait");

    // The healthy race: the stub reads a running pass, the pass sets the program up and
    // ends, and the stub's last read finds the shim — it runs the program.
    let healthy = run(
        &model,
        &[
            "Begin",
            "Plan",
            "Retire",
            "Download",
            "ObserveOwed",
            "ObservePass",
            "SetUp",
            "Finish",
            "End",
        ],
    );
    assert!(model.action_enabled("DecideRun", &healthy));
    assert!(!model.action_enabled("DecideFail", &healthy));

    // The queued race: the stub reads the promise, the pass begins, plans and takes it over
    // before the stub looks at the pass — it waits, and its next look finds the pass running
    // with the program planned.
    let queued = run(
        &model,
        &["ObserveOwed", "Begin", "Plan", "Retire", "ObservePass"],
    );
    assert!(model.action_enabled("DecideWait", &queued));
    assert!(!model.action_enabled("DecideFail", &queued));
    let next = fire_all(
        &model,
        queued,
        &["DecideWait", "ObserveOwed", "ObservePass"],
    );
    assert!(model.action_enabled("DecideWait", &next), "{next:?}");

    // A pass read before its plan, the promise still up: a wait, never "left out"…
    let unplanned = run(&model, &["Begin", "ObserveOwed", "ObservePass"]);
    assert!(
        model.action_enabled("DecideWait", &unplanned),
        "{unplanned:?}"
    );
    // …and a plan that leaves the program out ends the wait at the next look, while the pass
    // still runs: nothing is bringing it, so the stub does not wait the pass out.
    let without = run(
        &model,
        &[
            "ObserveOwed",
            "Begin",
            "PlanWithout",
            "Retire",
            "ObservePass",
        ],
    );
    assert!(model.action_enabled("DecideWait", &without), "{without:?}");
    let without = fire_all(
        &model,
        without,
        &["DecideWait", "ObserveOwed", "ObservePass", "DecideFail"],
    );
    assert!(
        model.check_invariant("NoFailureWhileComing", &without),
        "{without:?}"
    );

    // A set completion that stopped before it planned (no channel for this machine), the
    // promise kept, then the pass that lands the vendors alone: the stub waits across the gap
    // between the two passes and through the second one's start, and runs the program once
    // that plan brings it.
    let gap = run(&model, &["ObserveOwed", "Begin", "End", "ObservePass"]);
    assert!(model.action_enabled("DecideWait", &gap), "{gap:?}");
    let alone = fire_all(
        &model,
        gap,
        &["DecideWait", "Rebegin", "ObserveOwed", "ObservePass"],
    );
    assert!(model.action_enabled("DecideWait", &alone), "{alone:?}");
    let landed = fire_all(
        &model,
        alone,
        &[
            "DecideWait",
            "Plan",
            "Retire",
            "Download",
            "SetUp",
            "ObserveOwed",
            "ObservePass",
        ],
    );
    assert!(model.action_enabled("DecideRun", &landed), "{landed:?}");

    // A pass that dies before it sets the program up, one that ends before it plans
    // (offline), and a promise broken before any pass began: the stub says so, and each is
    // true.
    for trace in [
        &[
            "Begin",
            "Plan",
            "Retire",
            "Download",
            "End",
            "ObserveOwed",
            "ObservePass",
            "DecideFail",
        ][..],
        &[
            "Begin",
            "End",
            "Retire",
            "ObserveOwed",
            "ObservePass",
            "DecideFail",
        ][..],
        &["Lapse", "ObserveOwed", "ObservePass", "DecideFail"][..],
    ] {
        let failed = run(&model, trace);
        assert!(
            model.check_invariant("NoFalseFailure", &failed),
            "{trace:?}"
        );
        assert!(
            model.check_invariant("NoFailureWhileComing", &failed),
            "{trace:?}"
        );
    }

    // The negative controls, under the buggy reader. The healthy race is a false failure…
    let buggy = interp::with_buggy(&model, 1);
    let race = run(
        &buggy,
        &[
            "Begin",
            "Plan",
            "Retire",
            "Download",
            "ObserveOwed",
            "SetUp",
            "Finish",
            "End",
            "ObservePass",
            "DecideFail",
        ],
    );
    assert!(!buggy.check_invariant("NoFalseFailure", &race));

    // …the queued race says the install stopped while the pass runs…
    let early = run(
        &buggy,
        &[
            "ObserveOwed",
            "ObservePass",
            "Begin",
            "Plan",
            "Retire",
            "DecideFail",
        ],
    );
    assert!(!buggy.check_invariant("NoFailureWhileComing", &early));

    // …a pass read before its plan, the promise read gone once the plan landed, takes a plan
    // that HAS the program for one that left it out…
    let unplanned = run(
        &buggy,
        &[
            "Begin",
            "ObserveOwed",
            "ObservePass",
            "Plan",
            "Retire",
            "DecideFail",
        ],
    );
    assert!(!buggy.check_invariant("NoFailureWhileComing", &unplanned));

    // …the writer that retires the promise while its pass runs unplanned — when the set
    // completion stops, before the vendors' plan lands — has a stub say a program is not
    // coming just before that plan brings it…
    let early = interp::with_buggy(&model, 2);
    assert!(interp::violated_invariants(&early).contains("NoFailureWhileComing"));
    let told = run(
        &early,
        &[
            "Begin",
            "Retire",
            "ObserveOwed",
            "ObservePass",
            "DecideFail",
            "Plan",
        ],
    );
    assert!(!early.check_invariant("NoFailureWhileComing", &told));

    // …and a pass that died before setting the program up is taken for an install.
    let dead = run(
        &buggy,
        &[
            "Begin",
            "Plan",
            "Retire",
            "Download",
            "End",
            "ObserveOwed",
            "ObservePass",
            "DecideRun",
        ],
    );
    assert!(!buggy.check_invariant("RunsOnlyInstalled", &dead));
}

/// Fire `actions` in order from `state`, each of which must be enabled.
fn fire_all(model: &Model, mut state: State, actions: &[&str]) -> State {
    for action in actions {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    state
}
