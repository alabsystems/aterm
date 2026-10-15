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
/// queued, an overtaken one of the same tab notwithstanding; and one a quit
/// keeps the queue from taking stays owed for the quit layout. Round
/// seventeen: a panic of the host's loop loses no exit its workers held, and
/// an exit no pass sees as it comes is read as it is seen, whatever a Claude
/// Code started before the host hands it removes — and an agent a relaunch
/// lands is watched until a pass names it, the host down or not. Round
/// eighteen: the rollback queue's landing is watched as a worker's is, a
/// landing no read reaches keeps its worker — and the queue's keeps its
/// attempt — and a panic in the host's restart loses no worker. NEGATIVE
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
    // Round sixteen: a quit closes the queue before the worker told to take
    // its relaunch up decides; refused, it stays owed — stranded, for the
    // quit layout — and nothing more is queued, stepped or handed.
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
    // Round seventeen: the host's loop panics, and the agent crashes while
    // it waits out its back-off — a Claude Code started meanwhile sweeping
    // the record. The workers lived on: nothing is handed while the host is
    // down, and its first pass back hands the exit, read as it was seen.
    let down = run(&model, &["Panic", "Crash", "Sweep"]);
    assert!(!model.action_enabled("Hand", &down), "{down:?}");
    let back = run(
        &model,
        &["Panic", "Crash", "Sweep", "Up", "Hand", "Read", "Relaunch"],
    );
    assert_eq!((back["done"], back["down"]), (1, 0));
    // A crash while a Commit holds the host, swept during the hold: the exit
    // left to the host was read as it was seen, and is relaunched once the
    // Commit fails — and so when the panic and the Commit coincide, once the
    // host is back.
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
    // The review of round seventeen's first cut: a worker's relaunch lands
    // while the host is down, and the agent it started crashes before the
    // host is back — no pass read it back (`Back` needs one), so no worker
    // holds it and no mark, but its watch does: the host's first pass hands
    // that exit, read as it was seen, and it is relaunched again. With the
    // host up the pass that names it starts its worker and ends the watch.
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
    // Round eighteen: the rollback queue's landing is watched too — its
    // agent crashing before a pass names it is handed by the host and
    // relaunched once more.
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
    // A landing no read reaches keeps its worker, while the host is down
    // too: its agent's crash is its own relaunch's again, and a later read
    // is a landing watched as any.
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
    // The review of round eighteen's first cut: the rollback queue's landing
    // no read reaches keeps its attempt, the exit's holder — read later, it
    // is watched; crashed unread, it is owed again and stepped again.
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
    // Until round sixteen: a worker told `TakenUp` that decided after a quit
    // closed the queue had its relaunch refused, and owed it no more.
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
    // Until round seventeen: the host's panic dropped its workers, stopping
    // each — one supervising ended leaving no exit to the host, one handed
    // its exit settled what it owed — and the crash while the host was down
    // was never relaunched.
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
    // Until round seventeen: an exit left to the host while a Commit held it
    // was read only by the worker it was handed after the hold, the record
    // swept by then: a graceful exit.
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
    // Until the review of round seventeen's first cut: the agent a relaunch
    // landed was left to the roster with nothing watching it, and its crash
    // before a pass named it was read by nobody.
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
    // Until round eighteen: the rollback queue's landing was watched by
    // nobody, and its agent's crash before a pass named it was read by
    // nobody.
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
    // Until round eighteen: a landing no read reached ended its worker with
    // nothing watching its agent, whose crash was read by nobody.
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
    // Round eighteen's first cut: the queue's landing no read reached ended
    // its attempt, and its crash before any read was held by nobody.
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
    // Until round eighteen: a panic in the host's restart dropped its
    // workers, as the run's own panic had before round seventeen.
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
