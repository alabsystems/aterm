// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::verify;

/// Harness work in flight at a seamless update's Commit: a restored agent the park caught
/// mid-step is relaunched by the successor, no step starts while parked, a restart record left
/// in flight is swept, a step or agent that splits the captures refuses the Commit, and a tab an
/// agent ran in is never relaunched over. Each `Buggy = 1` mutant is caught by its invariant.
#[test]
fn harness_work_in_flight_at_a_commit_is_carried_never_dropped() {
    use aterm_spec::derive::harness_restored_carry_model;
    let model = harness_restored_carry_model();
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
    // A relaunch that typed its line, then an agent in its tab: its own (one seen during the
    // step that typed included), never overtaken, stepped again (carrying its line on, typing
    // nothing twice) and carried.
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
    // The agent that ran is forgotten once it stopped: both captures agree, and the successor
    // types beside it.
    let forgotten = replay(&[
        "Park",
        "AgentRuns",
        "AgentStopsForgotten",
        "Commit",
        "SuccRelaunches",
    ]);
    assert!(!buggy.check_invariant("NoRelaunchOverAgent", &forgotten));
}

/// A crashed agent's relaunch through a failed Commit is never lost: the stopped worker's debt
/// is relaunched whenever it decides, an exit no worker was handed is handed once the host
/// supervises again, the next park carries what the rollback queued, a host panic loses no exit
/// or worker, and a landing is watched until a pass names it. Each `Buggy = 1` mutant is caught
/// by its invariant.
#[test]
fn a_relaunch_owed_through_a_failed_commit_is_never_lost() {
    use aterm_spec::derive::harness_commit_owed_model;
    let model = harness_commit_owed_model();
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
    // A changed policy hands the worker over just before the Commit's stop, and the agent crashes
    // while the Commit holds: the handed-over worker leaves the exit to the host, which hands it
    // once it supervises again.
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
    // The agent is back and gone again before the worker read it: the worker relaunches it
    // itself, or, if a Commit's stop came while it was back, the rollback queues what it owes.
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
    // A quit closes the queue before the worker told to take its relaunch up decides: refused, it
    // stays owed for the quit layout, and nothing more is queued, stepped or handed.
    let quit = run(
        &model,
        &[
            "Crash", "Hand", "Read", "Suspend", "Resume", "Rollback", "Quit", "Decide",
        ],
    );
    assert_eq!((quit["queued"], quit["commit"]), (0, 2));
    for action in ["QueueSteps", "Park", "Start", "HandUnwatched"] {
        assert!(!model.action_enabled(action, &quit), "{action}: {quit:?}");
    }
    // The host's loop panics and the agent crashes in its back-off, a new Claude Code sweeping the
    // record: nothing is handed while the host is down, and its first pass back hands the exit,
    // read as it was seen.
    let down = run(&model, &["Panic", "Crash", "Sweep"]);
    assert!(!model.action_enabled("Hand", &down), "{down:?}");
    let back = run(
        &model,
        &["Panic", "Crash", "Sweep", "Up", "Hand", "Read", "Relaunch"],
    );
    assert_eq!((back["done"], back["down"]), (1, 0));
    // A crash while a Commit holds the host, swept during the hold: the exit was read as it was
    // seen and is relaunched once the Commit fails, or, when a panic coincides, once the host is
    // back.
    let held = run(
        &model,
        &[
            "Suspend",
            "EndUnhanded",
            "Crash",
            "Sweep",
            "Resume",
            "Rollback",
            "HandUnwatched",
            "Read",
            "Relaunch",
        ],
    );
    assert_eq!((held["done"], held["unwatched"]), (1, 0));
    let both = run(
        &model,
        &[
            "Panic",
            "Suspend",
            "EndUnhanded",
            "Crash",
            "Sweep",
            "Resume",
            "Rollback",
        ],
    );
    assert!(!model.action_enabled("HandUnwatched", &both), "{both:?}");
    let both = run(
        &model,
        &[
            "Panic",
            "Suspend",
            "EndUnhanded",
            "Crash",
            "Sweep",
            "Resume",
            "Rollback",
            "Up",
            "HandUnwatched",
            "Read",
            "Relaunch",
        ],
    );
    assert_eq!((both["done"], both["unwatched"]), (1, 0));
    // A relaunch lands while the host is down and its agent crashes before the host is back: no
    // pass read it back, but its watch holds it, so the host's first pass hands that exit. With
    // the host up, the pass that names it starts its worker and ends the watch.
    let landed = run(
        &model,
        &[
            "Crash", "Hand", "Read", "Panic", "Relaunch", "Crash", "Sweep",
        ],
    );
    assert!(!model.action_enabled("Back", &landed), "{landed:?}");
    assert!(
        !model.action_enabled("HandUnwatched", &landed),
        "{landed:?}"
    );
    let relaunched = run(
        &model,
        &[
            "Crash",
            "Hand",
            "Read",
            "Panic",
            "Relaunch",
            "Crash",
            "Sweep",
            "Up",
            "HandUnwatched",
            "Read",
            "Relaunch",
        ],
    );
    assert_eq!(
        (
            relaunched["done"],
            relaunched["landed"],
            relaunched["worker"]
        ),
        (1, 1, 0)
    );
    let named = run(&model, &["Crash", "Hand", "Read", "Relaunch", "Start"]);
    assert_eq!((named["worker"], named["landed"]), (1, 0));
    // The rollback queue's landing is watched too: its agent crashing before a pass names it
    // is handed by the host and relaunched once more.
    let queue_landed = run(
        &model,
        &[
            "Crash",
            "Hand",
            "Read",
            "Suspend",
            "Decide",
            "Resume",
            "Rollback",
            "QueueSteps",
        ],
    );
    assert_eq!(
        (queue_landed["landed"], queue_landed["queued"]),
        (1, 0),
        "{queue_landed:?}"
    );
    let queue_relaunched = run(
        &model,
        &[
            "Crash",
            "Hand",
            "Read",
            "Suspend",
            "Decide",
            "Resume",
            "Rollback",
            "QueueSteps",
            "Crash",
            "Sweep",
            "HandUnwatched",
            "Read",
            "Relaunch",
        ],
    );
    assert_eq!(
        (queue_relaunched["done"], queue_relaunched["landed"]),
        (1, 1)
    );
    // A landing no read reaches keeps its worker, the host down too: its agent's crash is its own
    // relaunch's again, and a later read is a landing watched as any.
    let unread = run(
        &model,
        &["Crash", "Hand", "Read", "Panic", "RelaunchUnread", "Crash"],
    );
    assert_eq!((unread["worker"], unread["agent"]), (3, 0), "{unread:?}");
    let reread = run(
        &model,
        &[
            "Crash",
            "Hand",
            "Read",
            "Panic",
            "RelaunchUnread",
            "Crash",
            "Relaunch",
        ],
    );
    assert_eq!(
        (reread["done"], reread["landed"], reread["worker"]),
        (1, 1, 0)
    );
    // The rollback queue's landing no read reaches keeps its attempt, the exit's holder: read
    // later, it is watched; crashed unread, it is owed and stepped again.
    let rollback_queued = [
        "Crash", "Hand", "Read", "Suspend", "Decide", "Resume", "Rollback",
    ];
    let unread_queued = run(
        &model,
        &[&rollback_queued[..], &["QueueStepsUnread"]].concat(),
    );
    assert_eq!(
        (
            unread_queued["queued"],
            unread_queued["qlanded"],
            unread_queued["agent"]
        ),
        (1, 1, 1),
        "{unread_queued:?}"
    );
    let read_late = run(
        &model,
        &[
            &rollback_queued[..],
            &["QueueStepsUnread", "QueueReadsLanded"],
        ]
        .concat(),
    );
    assert_eq!(
        (
            read_late["queued"],
            read_late["qlanded"],
            read_late["landed"]
        ),
        (0, 0, 1)
    );
    let crashed_unread = run(
        &model,
        &[&rollback_queued[..], &["QueueStepsUnread", "Crash"]].concat(),
    );
    assert_eq!(
        (
            crashed_unread["queued"],
            crashed_unread["qlanded"],
            crashed_unread["agent"]
        ),
        (1, 0, 0),
        "{crashed_unread:?}"
    );
    let stepped_again = run(
        &model,
        &[
            &rollback_queued[..],
            &["QueueStepsUnread", "Crash", "QueueSteps"],
        ]
        .concat(),
    );
    assert_eq!(
        (
            stepped_again["done"],
            stepped_again["landed"],
            stepped_again["queued"]
        ),
        (1, 1, 0)
    );
    // A pass that names the unread landing's agent ends the attempt: its
    // own worker's.
    let named_unread = run(
        &model,
        &[&rollback_queued[..], &["QueueStepsUnread", "Start"]].concat(),
    );
    assert_eq!(
        (
            named_unread["worker"],
            named_unread["queued"],
            named_unread["qlanded"]
        ),
        (1, 0, 0)
    );

    // Each mutant, alone, breaks its claim.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let lost = |actions: &[&str]| {
        let state = run(&buggy, actions);
        assert!(
            !buggy.check_invariant("NoOwedRelaunchLost", &state),
            "{actions:?}: {state:?}"
        );
    };
    // `DecideByPark`: the stop read as the park, which the rollback lifted.
    lost(&[
        "Crash",
        "Hand",
        "Read",
        "Suspend",
        "Resume",
        "Rollback",
        "DecideByPark",
    ]);
    // `StopEndsUnread`: the exit read from `left`, which the stop cleared.
    lost(&[
        "Crash",
        "Hand",
        "Suspend",
        "StopEndsUnread",
        "Resume",
        "Rollback",
    ]);
    // `EndUnhandedForgotten`: a worker stopped with no exit handed ends, and no pass hands an
    // exit to a stopped worker.
    lost(&[
        "Suspend",
        "Crash",
        "EndUnhandedForgotten",
        "Resume",
        "Rollback",
    ]);
    // `EndHandedOverForgotten`: only a worker the Commit marked leaves the exit to the host,
    // and the Commit marks no worker a hand-over stopped first.
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
    // `GoneReadAsStop`: the agent back and gone again is read as a stop for good, nothing
    // holding its exit.
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
    // `SettleGone`: a landing whose agent was gone before the worker read it ends the worker,
    // and the worker the host hands the exit cannot decide it.
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
    // `DecideClosedDrops`: a worker told `TakenUp` that decides after a quit closed the queue
    // has its relaunch refused, and owes it no more.
    lost(&[
        "Crash",
        "Hand",
        "Read",
        "Suspend",
        "Resume",
        "Rollback",
        "Quit",
        "DecideClosedDrops",
    ]);
    // `PanicStops`: the host's panic stops its workers, and the crash while the host is down is
    // never relaunched.
    lost(&[
        "PanicStops",
        "Crash",
        "EndUnhanded",
        "Up",
        "Suspend",
        "Resume",
        "Rollback",
    ]);
    lost(&[
        "Crash",
        "Hand",
        "Read",
        "PanicStops",
        "Decide",
        "Up",
        "Suspend",
        "Resume",
        "Rollback",
    ]);
    // `CrashUnseen`: an exit left to the host while a Commit held it is read only by the worker
    // it is handed after the hold, the record swept by then: a graceful exit.
    lost(&[
        "Suspend",
        "EndUnhanded",
        "CrashUnseen",
        "Sweep",
        "Resume",
        "Rollback",
        "HandUnwatched",
        "Read",
    ]);
    // `LandedLost`: the agent a relaunch landed is left to the roster with nothing watching it,
    // and nobody reads its crash before a pass names it.
    lost(&[
        "Crash",
        "Hand",
        "Read",
        "Panic",
        "Relaunch",
        "LandedLost",
        "Up",
        "Suspend",
        "Resume",
        "Rollback",
    ]);
    // `QueueLandedLost`: nobody watches the rollback queue's landing, and nobody reads its
    // agent's crash before a pass names it.
    lost(&[
        "Crash",
        "Hand",
        "Read",
        "Suspend",
        "Decide",
        "Resume",
        "Rollback",
        "QueueLandedLost",
    ]);
    // `UnreadLandedLost`: a landing no read reached ends its worker with nothing watching its
    // agent, whose crash is read by nobody.
    lost(&[
        "Crash",
        "Hand",
        "Read",
        "Panic",
        "UnreadLandedLost",
        "Up",
        "Suspend",
        "Resume",
        "Rollback",
    ]);
    // `QueueUnreadLost`: the queue's landing no read reached ends its attempt, and its crash
    // before any read is held by nobody.
    lost(&[
        "Crash",
        "Hand",
        "Read",
        "Suspend",
        "Decide",
        "Resume",
        "Rollback",
        "QueueStepsUnread",
        "QueueUnreadLost",
    ]);
    // `RestartPanicStops`: a panic in the host's restart drops its workers.
    lost(&[
        "Panic",
        "RestartPanicStops",
        "Crash",
        "EndUnhanded",
        "Up",
        "Suspend",
        "Resume",
        "Rollback",
    ]);
    // `ParkFirstFound`: both captures read the overtaken relaunch first.
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
