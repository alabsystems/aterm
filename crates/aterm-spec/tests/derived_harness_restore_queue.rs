// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::{derive::harness_restored_first_attempt_model, verify};

#[test]
fn restored_tabs_get_first_attempts_before_a_retry() {
    let model = harness_restored_first_attempt_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the restore queue must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "restored tabs' first attempts precede retry");
}

#[test]
fn network_probe_shutdown_cannot_notify_before_the_waiter_parks() {
    let model = aterm_spec::derive::netprobe_shutdown_park_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, "network probe shutdown cannot lose its wake");
}

/// HARNESS WORK IN FLIGHT AT A SEAMLESS UPDATE'S COMMIT (round four of the
/// 2026-09 update robustness work, plan item 7): a restored agent the park
/// caught mid-step is carried to the successor and relaunched there, no step
/// starts while the terminal is parked, and a restart record the outgoing
/// instance left in flight in a shell tab is swept at the Commit. The carry
/// is read live (round eleven): a step that lands while parked, or an agent
/// that runs in the tab — and stays a fact once it stopped again (round
/// twelve) — refuses a Commit whose captures it splits, and a tab an agent
/// ran in is never relaunched over; a relaunch that typed its line is not
/// overtaken by its own agent (round thirteen). NEGATIVE CONTROLS: the
/// mutants that shipped, each caught by its invariant.
#[test]
fn harness_work_in_flight_at_a_commit_is_carried_never_dropped() {
    use aterm_spec::derive::harness_restored_carry_model;
    let model = harness_restored_carry_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the restored carry must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "harness work in flight at a Commit");

    // The park lands mid-step: what it captures includes the agent whose
    // step is running, a step not possible yet leaves the two captures
    // agreeing, and the successor relaunches it.
    let mut state = model.init_state();
    for action in [
        "StepStart",
        "Park",
        "StepRetries",
        "Commit",
        "SuccRelaunches",
        "SweepCarriesOn",
    ] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!((state["resolved"], state["restart"]), (1, 0));
    // A step the park caught that relaunched after all: the re-capture
    // carries nothing, the Commit is refused, and the rollback finds it done.
    let mut landed = model.init_state();
    for action in ["StepStart", "Park", "StepResolves"] {
        assert!(model.fire(action, &mut landed), "{action}: {landed:?}");
    }
    assert!(!model.action_enabled("Commit", &landed), "{landed:?}");
    assert!(model.fire("Rollback", &mut landed));
    assert_eq!((landed["queued"], landed["resolved"]), (0, 1));
    // An agent a person started (and stopped again) in the tab during the
    // park: the re-capture carries nothing, the Commit is refused, and the
    // rollback's queue takes the relaunch off without a step.
    let mut ran = model.init_state();
    for action in ["Park", "AgentRuns"] {
        assert!(model.fire(action, &mut ran), "{action}: {ran:?}");
    }
    assert!(!model.action_enabled("Commit", &ran), "{ran:?}");
    assert!(model.fire("Rollback", &mut ran));
    assert!(!model.action_enabled("StepStart", &ran));
    assert!(model.fire("Overtaken", &mut ran));
    assert_eq!(ran["queued"], 0);
    // One that ran before the park: neither capture carries it, and the
    // Commit hands the successor nothing to step over it.
    let mut before = model.init_state();
    for action in ["AgentRuns", "Park", "Commit"] {
        assert!(model.fire(action, &mut before), "{action}: {before:?}");
    }
    assert_eq!(before["succ"], 0);
    // A relaunch that TYPED its line, then an agent in its tab: its own
    // (one seen during the step that typed included) — never overtaken,
    // stepped again (carrying its line on, typing nothing twice) and carried.
    for during in [false, true] {
        let mut typed = model.init_state();
        let actions: &[&str] = if during {
            &["StepStart", "AgentRuns", "StepTypes"]
        } else {
            &["StepStart", "StepTypes", "AgentRuns"]
        };
        for action in actions {
            assert!(model.fire(action, &mut typed), "{action}: {typed:?}");
        }
        assert_eq!((typed["ran"], typed["overtaken"]), (1, 0), "{typed:?}");
        assert!(model.action_enabled("StepStart", &typed), "{typed:?}");
        assert!(!model.action_enabled("Overtaken", &typed), "{typed:?}");
        for action in ["Park", "Commit", "SuccRelaunches"] {
            assert!(model.fire(action, &mut typed), "{action}: {typed:?}");
        }
        assert_eq!((typed["over"], typed["resolved"]), (0, 1), "{typed:?}");
    }
    // No step starts while parked; a rollback lets the queue run again.
    let mut parked = model.init_state();
    assert!(model.fire("Park", &mut parked));
    assert!(!model.action_enabled("StepStart", &parked));
    assert!(model.fire("Rollback", &mut parked));
    assert!(model.action_enabled("StepStart", &parked));

    // Each mutant, alone, breaks exactly its claim.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let replay = |actions: &[&str]| {
        let mut state = buggy.init_state();
        for action in actions {
            assert!(buggy.fire(action, &mut state), "{action}: {state:?}");
        }
        state
    };
    let dropped = replay(&["ParkDropsQueue", "Commit"]);
    assert!(!buggy.check_invariant("NoSilentLoss", &dropped));
    let late = replay(&["Park", "StepStartWhileParked"]);
    assert!(!buggy.check_invariant("NoStepWhileParked", &late));
    let stranded = replay(&["Park", "CommitWithoutSweep"]);
    assert!(!buggy.check_invariant("NoStrandedRestart", &stranded));
    // The frozen carry: both captures read the copy the park froze, so an
    // agent that ran in the tab since is relaunched over by the successor.
    let frozen = replay(&["ParkFreezes", "AgentRuns", "Commit", "SuccRelaunches"]);
    assert!(!buggy.check_invariant("NoRelaunchOverAgent", &frozen));
    // A queue that never asked the tab steps it here.
    let asked_nothing = replay(&["AgentRuns", "StepStartOvertaken"]);
    assert!(!buggy.check_invariant("NoRelaunchOverAgent", &asked_nothing));
    // Round eleven: the agent that ran is forgotten once it stopped, both
    // captures agree, and the successor types beside it.
    let forgotten = replay(&[
        "Park",
        "AgentRuns",
        "AgentStopsForgotten",
        "Commit",
        "SuccRelaunches",
    ]);
    assert!(!buggy.check_invariant("NoRelaunchOverAgent", &forgotten));
}

/// A CRASHED AGENT'S RELAUNCH THROUGH A PARK'S COMMIT THAT FAILS (F13,
/// rounds eleven to fourteen): whenever the stopped worker decides, before
/// the rollback or after it, what it owes is relaunched; an exit no worker
/// was handed — the agent crashed while the Commit held the host, or its
/// worker was stopped healthy, or handed over to a successor a changed
/// policy starts at the next pass — is handed by the host once it
/// supervises again; and the next park carries the relaunch the rollback
/// queued, an overtaken one of the same tab notwithstanding. NEGATIVE
/// CONTROLS: the mutants that shipped, each caught by its invariant.
#[test]
fn a_relaunch_owed_through_a_failed_commit_is_never_lost() {
    use aterm_spec::derive::harness_commit_owed_model;
    let model = harness_commit_owed_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the Commit's owed relaunch must stay enrolled in the spec-link registry"
    );
    verify::prove_and_catch_scalar(&model, "a relaunch owed through a failed Commit");

    let run = |model: &aterm_spec::derive::Model, actions: &[&str]| {
        let mut state = model.init_state();
        for action in actions {
            assert!(model.fire(action, &mut state), "{action}: {state:?}");
        }
        state
    };
    // The worker decides before the rollback: stranded, then queued by it.
    let early = run(
        &model,
        &[
            "Crash", "Hand", "Read", "Suspend", "Decide", "Resume", "Rollback",
        ],
    );
    assert_eq!((early["queued"], early["commit"]), (1, 0));
    // After it: told it is taken up, and queues it as it decides.
    let late = run(
        &model,
        &[
            "Crash", "Hand", "Read", "Suspend", "Resume", "Rollback", "Decide",
        ],
    );
    assert_eq!((late["queued"], late["commit"]), (1, 0));
    // The crash while the Commit holds the host: its worker ends handed
    // nothing, and the host hands the exit once it supervises again.
    let unhanded = run(
        &model,
        &[
            "Suspend",
            "Crash",
            "EndUnhanded",
            "Resume",
            "Rollback",
            "HandUnwatched",
            "Read",
            "Relaunch",
        ],
    );
    assert_eq!((unhanded["done"], unhanded["unwatched"]), (1, 0));
    // The agent alive across the Commit: its next worker resets the mark.
    let alive = run(&model, &["Suspend", "EndUnhanded", "Resume", "Start"]);
    assert_eq!(
        (alive["worker"], alive["unwatched"], alive["commit"]),
        (1, 0, 0)
    );
    assert!(!model.action_enabled("HandUnwatched", &alive));
    // Round fourteen: a changed policy hands the worker over just before the
    // Commit's stop, which then marks nothing (its stop is set already); the
    // agent crashes while the Commit holds. The worker handed over leaves
    // the exit to the host as it ends, which hands it once it supervises
    // again.
    let handed_over = run(
        &model,
        &[
            "HandOver",
            "Suspend",
            "Crash",
            "EndUnhanded",
            "Resume",
            "Rollback",
            "HandUnwatched",
            "Read",
            "Relaunch",
        ],
    );
    assert_eq!(
        (
            handed_over["done"],
            handed_over["unwatched"],
            handed_over["commit"]
        ),
        (1, 0, 0)
    );
    // No Commit: a crash between the hand-over and the pass that would start
    // the successor is handed at that pass.
    let one_pass = run(
        &model,
        &[
            "HandOver",
            "Crash",
            "EndUnhanded",
            "HandUnwatched",
            "Read",
            "Relaunch",
        ],
    );
    assert_eq!((one_pass["done"], one_pass["phase"]), (1, 0));
    // The agent alive: the successor starts and ends the mark.
    let succeeded = run(&model, &["HandOver", "EndUnhanded", "Start"]);
    assert_eq!(
        (
            succeeded["worker"],
            succeeded["stop"],
            succeeded["handover"],
            succeeded["unwatched"]
        ),
        (1, 0, 0, 0)
    );
    // The joint audit of 2026-10-01: the agent back — the host reads it and
    // marks it its own — and gone again before the worker read it. The
    // worker reads it gone and relaunches it itself — in its back-off, or
    // (round two) at the landing that started it: it never ends there,
    // leaving the exit to a worker the host would hand it; or a Commit's
    // stop came while it was back, and the rollback queues what it still
    // owes.
    let rearmed = run(
        &model,
        &[
            "Crash", "Hand", "Read", "Back", "Crash", "Rearm", "Relaunch",
        ],
    );
    assert_eq!((rearmed["done"], rearmed["unwatched"]), (1, 0));
    let gone = run(&model, &["Crash", "Hand", "Read", "Back", "Crash"]);
    assert!(!model.action_enabled("Settle", &gone), "{gone:?}");
    let stranded = run(
        &model,
        &[
            "Crash", "Hand", "Read", "Back", "Suspend", "Crash", "Rearm", "Decide", "Resume",
            "Rollback",
        ],
    );
    assert_eq!((stranded["queued"], stranded["commit"]), (1, 0));
    // An overtaken relaunch of the tab is queued too: the next park carries
    // the one the rollback queued, and its Commit hands it on.
    let carried = run(
        &model,
        &[
            "Overtook", "Crash", "Hand", "Read", "Suspend", "Decide", "Resume", "Rollback", "Park",
            "Commit",
        ],
    );
    assert_eq!((carried["succ"], carried["dropped"]), (1, 0));
    // One queued after the park splits the captures: that Commit is refused.
    let split = run(
        &model,
        &[
            "Crash", "Hand", "Read", "Suspend", "Resume", "Rollback", "Park", "Decide",
        ],
    );
    assert!(!model.action_enabled("Commit", &split), "{split:?}");

    // Each mutant, alone, breaks its claim.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let lost = |actions: &[&str]| {
        let state = run(&buggy, actions);
        assert!(
            !buggy.check_invariant("NoOwedRelaunchLost", &state),
            "{actions:?}: {state:?}"
        );
    };
    // Round ten: the stop read as the park, which the rollback lifted.
    lost(&[
        "Crash",
        "Hand",
        "Read",
        "Suspend",
        "Resume",
        "Rollback",
        "DecideByPark",
    ]);
    // Round eleven: the exit read from `left`, which the stop cleared.
    lost(&[
        "Crash",
        "Hand",
        "Suspend",
        "StopEndsUnread",
        "Resume",
        "Rollback",
    ]);
    // Round twelve: a worker stopped with no exit handed ended, and no pass
    // hands an exit to a stopped worker.
    lost(&[
        "Suspend",
        "Crash",
        "EndUnhandedForgotten",
        "Resume",
        "Rollback",
    ]);
    // Round thirteen: only a worker the Commit marked left the exit to the
    // host, and the Commit marks no worker a hand-over stopped first.
    lost(&[
        "HandOver",
        "Suspend",
        "Crash",
        "EndHandedOverForgotten",
        "Resume",
        "Rollback",
    ]);
    // And with no Commit at all, the crash in the hand-over's pass.
    lost(&[
        "HandOver",
        "Crash",
        "EndHandedOverForgotten",
        "Suspend",
        "Resume",
        "Rollback",
    ]);
    // Until the joint audit of 2026-10-01: the agent back and gone again
    // read as a stop for good, nothing holding its exit.
    lost(&[
        "Crash",
        "Hand",
        "Read",
        "Back",
        "Crash",
        "GoneReadAsStop",
        "Suspend",
        "Resume",
        "Rollback",
    ]);
    // Until round two of that audit: a landing whose agent was gone before
    // the worker read it ended the worker, and the worker the host handed
    // the exit could not decide it.
    lost(&[
        "Crash",
        "Hand",
        "Read",
        "Back",
        "Crash",
        "SettleGone",
        "HandUnwatched",
        "Suspend",
        "Resume",
        "Rollback",
    ]);
    // Round twelve: both captures read the overtaken relaunch first.
    let dropped = run(
        &buggy,
        &[
            "Overtook",
            "Crash",
            "Hand",
            "Read",
            "Suspend",
            "Decide",
            "Resume",
            "Rollback",
            "ParkFirstFound",
            "Commit",
        ],
    );
    assert!(
        !buggy.check_invariant("NoQueuedRelaunchDropped", &dropped),
        "{dropped:?}"
    );
}
