// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tier-1 for `NativeUpdateLaunchedDialWait` (aterm-spec's
//! `native_update_launched_dial_wait_model`, 2026-09-29): the outgoing
//! process's wait for its launched successor's dial, driven for real and
//! projected onto the model step by step (`interp::admits` names the action
//! the model admits the real step by, and every invariant holds after it).
//!
//! Driven for real: the completion's reading of an `NSRunningApplication`
//! (`admit_launch_answer`, through `LaunchInFlight::admitted_for_test`), the
//! verdicts (`launch_answer_proves_exit`, `launch_answer_names_pid`), the
//! kernel's word about a named pid (`NamedSuccessorWatch::gone`, against real
//! processes this test starts and ends), the wait itself
//! (`await_launched_dial`: a real `Rendezvous` bound in this process, its real
//! 10 ms accept loop and the real abort), a real dialer presenting the real
//! claim frame, and the corroboration after the accept
//! (`LaunchCorroboration::poll`). LaunchServices is SCRIPTED — with the
//! answers it was measured to give (see `LaunchError::ExitedBeforeCheckIn`
//! and `launch_answer_names_pid`) — because a unit test may not launch an
//! application (AGENTS.md). The live half, this build's own successor
//! dying before its dial through `ATERM_DEBUG_DIE_BEFORE_DIAL`, is measured in
//! the design doc.
//!
//! Negative controls: a classifier that reads a live application's unknown
//! pid as death is refused by the model; the retired cancel-only abort, run
//! for real, waits out its budget beside a death LaunchServices already
//! reported (the liveness mutant `PollCancelOnly`); the abort before the
//! named-pid watch, run for real, waits out its budget beside a named process
//! that is gone (`IgnoreNamedPid`); an abort that takes a pid it cannot watch
//! for gone, run for real, refuses a live process (`EndOnNamedPid`); and a
//! consuming pre-dial poll, run for real, leaves the corroboration nothing
//! (`ConsumingPoll`).

use super::{
    HandoffCandidate, LaunchCorroboration, LaunchedDialEnd, NamedSuccessorWatch, PreDialDeath,
    await_launched_dial, forgives_the_counted_trial_launch,
};
use crate::ChildDeathEvidence;
use crate::app_launch_successor::{
    LaunchError, LaunchInFlight, LaunchedSuccessor, launch_answer_names_pid,
    launch_answer_proves_exit,
};
use crate::app_native::PhysicalFailureShape;
use crate::handoff_rendezvous::{
    DialerCode, Rendezvous, RendezvousError, TRUSTD_SILENT, admitted, dial_for_test,
    held_for_trustd,
};
use aterm_spec::derive::{Model, native_update_launched_dial_wait_model};
use aterm_spec::interp::{self, State};
use std::time::{Duration, Instant};

/// The successor, as the harness scripted or drove it.
const ALIVE: i64 = 0;
const DIALLED: i64 = 1;
const DEAD: i64 = 2;
/// LaunchServices' answer, by what it proves.
const NONE: i64 = 0;
const PROVES_NOTHING: i64 = 1;
const PROVES_EXIT: i64 = 2;
const NAMES_PID: i64 = 3;
/// The wait.
const WAITING: i64 = 0;
const ACCEPTED: i64 = 1;
const TIMED_OUT: i64 = 2;
const ENDED_ON_EXIT: i64 = 3;

/// What the harness observed, in the model's terms.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Observed {
    successor: i64,
    answer: i64,
    /// LaunchServices' answer is still in the launch's slot.
    slot: bool,
    wait: i64,
    expired: bool,
    /// The abort in use peeks the answer (the shipping one does).
    polls_answer: bool,
    /// The abort in use asks the kernel about a named pid (the shipping one
    /// does).
    watches_named: bool,
    /// The corroboration took the answer.
    taken: bool,
}

impl Observed {
    fn start() -> Self {
        Self {
            successor: ALIVE,
            answer: NONE,
            slot: false,
            wait: WAITING,
            expired: false,
            polls_answer: true,
            watches_named: true,
            taken: false,
        }
    }
}

/// Project an observation onto the model's state.
pub(crate) fn project(model: &Model, seen: Observed) -> State {
    let mut state = model.init_state();
    state.insert("successor", seen.successor);
    state.insert("answer", seen.answer);
    state.insert("slot", i64::from(seen.slot));
    state.insert("wait", seen.wait);
    state.insert("expired", i64::from(seen.expired));
    state.insert("polls_answer", i64::from(seen.polls_answer));
    state.insert("watches_named", i64::from(seen.watches_named));
    state.insert("taken", i64::from(seen.taken));
    state
}

/// The model must admit `prev -> next` by exactly `action`, and every
/// invariant must hold at `next`.
fn step(model: &Model, prev: Observed, next: Observed, action: &str) {
    let (from, to) = (project(model, prev), project(model, next));
    crate::conformance_support::assert_transition(model, &from, &to, action);
}

/// What the real verdicts say of a real answer, in the model's answer terms.
fn answer_class(answer: &Result<LaunchedSuccessor, LaunchError>) -> i64 {
    if launch_answer_proves_exit(Some(answer)) {
        PROVES_EXIT
    } else if launch_answer_names_pid(Some(answer)).is_some() {
        NAMES_PID
    } else {
        PROVES_NOTHING
    }
}

/// The model action a real answer of `class` is.
fn answer_action(class: i64) -> &'static str {
    match class {
        PROVES_EXIT => "AnswerExited",
        NAMES_PID => "AnswerNamed",
        _ => "AnswerOther",
    }
}

fn own_pid() -> i32 {
    i32::try_from(std::process::id()).expect("a pid fits pid_t")
}

/// A real rendezvous bound in this process. Fails loudly rather than skipping:
/// a Tier-1 that could not bind proved nothing.
fn bind() -> Rendezvous {
    let nonce = aterm_uds::rand::hex_token::<16>().expect("a nonce");
    Rendezvous::bind(&nonce).expect("bind a rendezvous for the Tier-1")
}

/// A far deadline, bounded at a minute by the caller: a wait that ignored its
/// abort would run to it. The minute is a hang detector, not a latency budget.
fn far() -> Instant {
    Instant::now() + Duration::from_secs(120)
}

/// A real process standing in for a named successor: it lives until its
/// stdin closes, then exits 70 (`DEBUG_DEATH_EXIT_CODE`, a crash's shape).
fn named_stand_in() -> aterm_uds::spawnfd::Child {
    aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "read _; exit 70"])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("spawn the named stand-in")
}

/// End the stand-in by its own exit and reap it: once `wait` returns it has
/// passed `proc_exit` (its exit is on any watch) and its pid is vacant.
fn end_and_reap(mut child: aterm_uds::spawnfd::Child) {
    drop(child.stdin.take());
    let status = child.wait().expect("reap the named stand-in");
    assert_eq!(
        status.code(),
        Some(70),
        "the stand-in ended by its own exit"
    );
}

fn pid_of(child: &aterm_uds::spawnfd::Child) -> (u32, i32) {
    let pid = child.id();
    (pid, i32::try_from(pid).expect("a pid fits pid_t"))
}

/// The measured answers, as the completion reads an `NSRunningApplication`:
/// `(pid, isTerminated, the process is dead)`.
fn launch_services_shapes() -> [(i32, bool, bool); 6] {
    [
        // Died before it checked in: pid -1, terminated (measured).
        (-1, true, true),
        // Checked in: its pid, alive (measured: answered during its
        // `+sharedApplication`, before it can go on).
        (4242, false, false),
        // LaunchServices' own ~30 s check-in timeout on a live process.
        (4243, false, false),
        // An unknown pid on an application not terminated: proves nothing.
        (-1, false, false),
        // A known pid that has since terminated: admitted as a pid, as before;
        // whether it is gone is the kernel's to say (`NamedSuccessorWatch`).
        (4244, true, true),
        // LaunchServices substituting THIS process: names nothing to watch.
        (own_pid(), false, false),
    ]
}

/// THE WAIT, END TO END, PROJECTED: every anchored function of the machine
/// runs on this thread and is required to have been entered.
#[test]
fn the_real_dial_wait_refines_the_model() {
    let model = native_update_launched_dial_wait_model();
    assert!(
        aterm_spec::xref::reset_entered_anchors(),
        "the evidence window must open"
    );

    // 1. THE CLASSIFIER is the model's three answer actions: from a successor
    //    in the state the harness scripted, the real reading of each measured
    //    answer is admitted as AnswerExited exactly when it proves the exit,
    //    and as AnswerNamed exactly when it names a pid.
    for (pid, terminated, dead) in launch_services_shapes() {
        let answer = LaunchInFlight::admitted_for_test(pid, terminated, own_pid());
        let before = Observed {
            successor: if dead { DEAD } else { ALIVE },
            ..Observed::start()
        };
        let class = answer_class(&answer);
        let after = Observed {
            answer: class,
            slot: true,
            ..before
        };
        step(&model, before, after, answer_action(class));
    }

    // 2. DEAD BEFORE ITS DIAL: LaunchServices reports the terminated
    //    application while the real wait runs; the wait ends on it, not on its
    //    budget, and leaves the answer in the slot.
    {
        let rendezvous = bind();
        let in_flight = LaunchInFlight::scripted(None);
        let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let dead = Observed {
            successor: DEAD,
            ..Observed::start()
        };
        step(&model, Observed::start(), dead, "Die");
        let started = Instant::now();
        let ended = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                in_flight.answer_for_test(LaunchInFlight::admitted_for_test(-1, true, own_pid()));
            });
            await_launched_dial(
                &rendezvous,
                &in_flight,
                &DialerCode::this_process(),
                far(),
                &cancel_rx,
            )
        });
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the death ended the wait, not its budget: {:?}",
            started.elapsed()
        );
        assert_eq!(
            ended.err(),
            Some(LaunchedDialEnd::SuccessorExited(
                PreDialDeath::TerminatedBeforeCheckIn
            ))
        );
        let answered = Observed {
            answer: PROVES_EXIT,
            slot: true,
            ..dead
        };
        step(&model, dead, answered, "AnswerExited");
        let still_there = in_flight.exited_before_check_in();
        assert!(
            still_there,
            "the wait peeked: the answer is still in the slot"
        );
        let over = Observed {
            wait: ENDED_ON_EXIT,
            slot: still_there,
            ..answered
        };
        step(&model, answered, over, "EndOnExit");
        assert_eq!(
            in_flight.wait(Duration::ZERO),
            Err(LaunchError::ExitedBeforeCheckIn),
            "and the one consuming reader would still get it"
        );
        // THE CLASS AND THE ACCOUNTING AGREE: nothing observed is filed
        // Unexplained, and that lane's budget is safe only because its
        // counted trial launch is given back.
        let evidence = PreDialDeath::TerminatedBeforeCheckIn.evidence();
        assert_eq!(
            PhysicalFailureShape::of_child_death(evidence),
            PhysicalFailureShape::Unexplained
        );
        assert!(forgives_the_counted_trial_launch(evidence, 1, 2));
    }

    // 3. NAMED, THEN DEAD BEFORE ITS DIAL. LaunchServices named a real
    //    process and says nothing more; the kernel does. First the watch
    //    itself: registered while the process lives (nothing to say), then its
    //    own exit, recorded with its status.
    {
        let child = named_stand_in();
        let (pid, named) = pid_of(&child);
        let answered = Observed {
            answer: NAMES_PID,
            slot: true,
            ..Observed::start()
        };
        step(&model, Observed::start(), answered, "AnswerNamed");
        let mut watch = NamedSuccessorWatch::default();
        assert_eq!(
            watch.gone(Some(named)),
            None,
            "alive: the kernel has nothing to say"
        );
        assert!(!model.action_enabled("EndOnNamedExit", &project(&model, answered)));
        end_and_reap(child);
        let dead = Observed {
            successor: DEAD,
            ..answered
        };
        step(&model, answered, dead, "Die");
        let gone = watch.gone(Some(named));
        assert_eq!(
            gone.map(|(named, death)| (named.pid, death)),
            Some((pid, ChildDeathEvidence::Exited { code: 70 })),
            "the kernel recorded the named process's own exit"
        );
        step(
            &model,
            dead,
            Observed {
                wait: ENDED_ON_EXIT,
                ..dead
            },
            "EndOnNamedExit",
        );
        // An exit of its own is the bytes' answer: Structural, and the counted
        // trial launch is kept — the one decision every reject path uses.
        let evidence = ChildDeathEvidence::Exited { code: 70 };
        assert_eq!(
            PhysicalFailureShape::of_child_death(evidence),
            PhysicalFailureShape::Structural
        );
        assert!(!forgives_the_counted_trial_launch(evidence, 1, 2));
    }

    // …then the real wait, over a named process that is already gone and
    //    reaped: its pid is vacant, the watch could not even be registered,
    //    and the wait ends on the kernel's word at once, answer left in place.
    {
        let rendezvous = bind();
        let child = named_stand_in();
        let (pid, named) = pid_of(&child);
        let in_flight = LaunchInFlight::scripted(Some(named));
        let answered = Observed {
            answer: NAMES_PID,
            slot: true,
            ..Observed::start()
        };
        step(&model, Observed::start(), answered, "AnswerNamed");
        end_and_reap(child);
        let dead = Observed {
            successor: DEAD,
            ..answered
        };
        step(&model, answered, dead, "Die");
        let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let ended = await_launched_dial(
            &rendezvous,
            &in_flight,
            &DialerCode::this_process(),
            far(),
            &cancel_rx,
        );
        // Reaped before the wait first saw it: no birth stamp was read.
        let death = PreDialDeath::NamedProcessGone {
            candidate: HandoffCandidate::from_bare_pid(pid),
            death: ChildDeathEvidence::Unobserved,
        };
        assert_eq!(ended.err(), Some(LaunchedDialEnd::SuccessorExited(death)));
        let still_there = in_flight.named_pid() == Some(named);
        assert!(still_there, "the wait peeked the named pid");
        step(
            &model,
            dead,
            Observed {
                wait: ENDED_ON_EXIT,
                slot: still_there,
                ..dead
            },
            "EndOnNamedExit",
        );
        assert_eq!(
            PhysicalFailureShape::of_child_death(death.evidence()),
            PhysicalFailureShape::Unexplained
        );
        assert!(forgives_the_counted_trial_launch(death.evidence(), 1, 2));
    }

    // 4. ALIVE, ANSWERED WITH AN ERROR, or NAMED, before its dial (the error
    //    that exists for a process still running; the pid of one that checked
    //    in first): no arm ends the wait, the real dialer is accepted, and the
    //    corroboration takes the answer the wait left — proceeding on the
    //    attested dialer, or matching it.
    for named in [false, true] {
        let rendezvous = bind();
        let in_flight = LaunchInFlight::scripted(named.then(own_pid));
        let class = if named {
            NAMES_PID
        } else {
            let error = Err(LaunchError::Rejected {
                domain: "NSCocoaErrorDomain".to_string(),
                code: 256,
                message: "a miscellaneous error occurred".to_string(),
            });
            let class = answer_class(&error);
            in_flight.answer_for_test(error);
            class
        };
        let answered = Observed {
            answer: class,
            slot: true,
            ..Observed::start()
        };
        step(&model, Observed::start(), answered, answer_action(class));
        let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let path = rendezvous.path().to_path_buf();
        let claim = rendezvous.claim().to_string();
        let (accepted, _dialer) = std::thread::scope(|scope| {
            let dialer = scope.spawn(|| dial_for_test(&path, &claim).expect("dial and claim"));
            let accepted = await_launched_dial(
                &rendezvous,
                &in_flight,
                &DialerCode::this_process(),
                far(),
                &cancel_rx,
            );
            (accepted, dialer.join().expect("the dialer thread"))
        });
        let peer = match accepted {
            Ok(peer) => peer,
            Err(LaunchedDialEnd::Rendezvous(error)) if held_for_trustd(&error) => {
                panic!("{TRUSTD_SILENT}: {error}")
            }
            Err(end @ LaunchedDialEnd::ExitedWhileHeld { .. }) => {
                panic!("{TRUSTD_SILENT}: {end:?}")
            }
            Err(end) => {
                panic!("a live successor's dial is accepted, whatever was answered: {end:?}")
            }
        };
        assert_eq!(peer.pid(), own_pid(), "the kernel attests this process");
        let dialled = Observed {
            successor: DIALLED,
            ..answered
        };
        step(&model, answered, dialled, "Dial");
        let accepted = Observed {
            wait: ACCEPTED,
            ..dialled
        };
        step(&model, dialled, accepted, "Accept");
        let mut corroboration = LaunchCorroboration::of_dialer(
            &in_flight,
            HandoffCandidate::of_attested_peer(
                u32::try_from(peer.pid()).expect("an attested pid is positive"),
            ),
        );
        assert!(
            corroboration.poll().is_ok() && corroboration.answered,
            "an answer after a dial is read: the named pid is the dialer's, and an error \
             proceeds on the attested dialer"
        );
        let slot = in_flight.wait(Duration::ZERO) != Err(LaunchError::Timeout(Duration::ZERO));
        assert!(!slot, "the corroboration took the answer");
        let corroborated = Observed {
            slot,
            taken: true,
            ..accepted
        };
        step(&model, accepted, corroborated, "Corroborate");
    }

    // 5. NOBODY DIALS AND NOTHING PROVES A DEATH: LaunchServices named a live
    //    process this one may not even watch (pid 1), so only the budget ends
    //    the wait — a pid that cannot be watched is not a pid that is gone.
    {
        let rendezvous = bind();
        let in_flight = LaunchInFlight::scripted(Some(1));
        let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let answered = Observed {
            answer: NAMES_PID,
            slot: true,
            ..Observed::start()
        };
        step(&model, Observed::start(), answered, "AnswerNamed");
        let ended = await_launched_dial(
            &rendezvous,
            &in_flight,
            &DialerCode::this_process(),
            Instant::now() + Duration::from_millis(200),
            &cancel_rx,
        );
        assert_eq!(
            ended.err(),
            Some(LaunchedDialEnd::Rendezvous(RendezvousError::Deadline))
        );
        let expired = Observed {
            expired: true,
            ..answered
        };
        step(&model, answered, expired, "Expire");
        let timed_out = Observed {
            wait: TIMED_OUT,
            ..expired
        };
        step(&model, expired, timed_out, "TimeOut");
    }

    // 6. A REVOCATION KEEPS ITS OWN NAME, even beside a reported death: the
    //    cancel is asked first.
    {
        let rendezvous = bind();
        let in_flight = LaunchInFlight::scripted(None);
        in_flight.answer_for_test(Err(LaunchError::ExitedBeforeCheckIn));
        let (cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel::<()>(1);
        cancel_tx.send(()).expect("poke the cancel");
        assert_eq!(
            await_launched_dial(
                &rendezvous,
                &in_flight,
                &DialerCode::this_process(),
                far(),
                &cancel_rx
            )
            .err(),
            Some(LaunchedDialEnd::Cancelled)
        );
    }

    // Every anchored shipping function of the machine RAN, on this thread.
    let entered = aterm_spec::xref::entered_anchor_ids();
    aterm_spec::xref::disarm_entered_anchors();
    let anchors: Vec<_> = aterm_spec::xref::refinements()
        .filter(|anchor| {
            aterm_spec::xref::machine_matches(anchor.machine, "NativeUpdateLaunchedDialWait")
        })
        .collect();
    assert_eq!(
        anchors.len(),
        8,
        "AnswerExited, AnswerOther and AnswerNamed (admit_launch_answer), EndOnExit \
         (launch_answer_proves_exit), EndOnNamedExit (NamedSuccessorWatch::gone), Accept and \
         TimeOut (await_launched_dial) and Corroborate (LaunchCorroboration::poll) are anchored"
    );
    for anchor in anchors {
        assert!(
            !anchor.entry_id.is_empty() && entered.contains(anchor.entry_id),
            "{} was not entered: {entered:?}",
            anchor.entry_id
        );
    }
}

/// NEGATIVE CONTROLS: each defect the model was written against, run for
/// real where it can be, and refused by the model.
#[test]
fn the_models_mutants_are_the_defects_they_name() {
    let model = native_update_launched_dial_wait_model();

    // A classifier that reads a LIVE application's unknown pid as death — the
    // shape of "any error, or any unknown pid, is death" — produces a step the
    // committed model does not admit, and the buggy one admits only by
    // breaking NeverRefusesALiveSuccessor.
    let buggy_class = |answer: &Result<LaunchedSuccessor, LaunchError>| {
        if answer.is_err() {
            PROVES_EXIT
        } else {
            PROVES_NOTHING
        }
    };
    let live_unknown = LaunchInFlight::admitted_for_test(-1, false, own_pid());
    assert_eq!(buggy_class(&live_unknown), PROVES_EXIT);
    assert_eq!(answer_class(&live_unknown), PROVES_NOTHING);
    let from = project(&model, Observed::start());
    let to = project(
        &model,
        Observed {
            answer: PROVES_EXIT,
            slot: true,
            ..Observed::start()
        },
    );
    assert_eq!(
        interp::admits(&model, &from, &to),
        None,
        "no answer proves the exit of a live successor"
    );
    let buggy = interp::with_buggy(&model, 1);
    let mut refused = buggy.init_state();
    for action in ["AnswerOther", "EndOnAnyAnswer"] {
        assert!(buggy.fire(action, &mut refused), "{action}: {refused:?}");
    }
    assert!(!buggy.check_invariant("NeverRefusesALiveSuccessor", &refused));

    // THE RETIRED ABORT, run for real: the worker's cancel and nothing else.
    // LaunchServices has already reported the death, and the wait still runs
    // to its budget — the liveness mutant `PollCancelOnly`.
    {
        let rendezvous = bind();
        let in_flight = LaunchInFlight::scripted(None);
        in_flight.answer_for_test(Err(LaunchError::ExitedBeforeCheckIn));
        let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let retired = rendezvous.accept_claim(
            None,
            &DialerCode::this_process(),
            Instant::now() + Duration::from_millis(200),
            &|| cancel_rx.try_recv().is_ok(),
        );
        assert_eq!(
            retired.err(),
            Some(RendezvousError::Deadline),
            "the retired abort never looks at the answer"
        );
        let isolated = aterm_spec::verify::buggy_baseline_with(&model, "PollCancelOnly");
        let mut state = isolated.init_state();
        for action in ["Die", "AnswerExited", "PollCancelOnly"] {
            assert!(isolated.fire(action, &mut state), "{action}: {state:?}");
        }
        assert!(
            !isolated.action_enabled("EndOnExit", &state),
            "and neither does the mutant: only the budget ends it — {state:?}"
        );
        let projected = project(
            &model,
            Observed {
                successor: DEAD,
                answer: PROVES_EXIT,
                slot: true,
                polls_answer: false,
                ..Observed::start()
            },
        );
        assert!(
            !model.check_invariant("AReportedDeathIsSeen", &projected),
            "the retired abort's state is the one AReportedDeathIsSeen refuses"
        );
    }

    // THE ABORT BEFORE THE NAMED-PID WATCH, run for real: the cancel and
    // LaunchServices' "terminated", nothing about a pid it named. The named
    // process is gone and reaped, and the wait still runs to its budget — the
    // liveness mutant `IgnoreNamedPid`.
    {
        let rendezvous = bind();
        let child = named_stand_in();
        let (_, named) = pid_of(&child);
        let in_flight = LaunchInFlight::scripted(Some(named));
        end_and_reap(child);
        let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let before = rendezvous.accept_claim(
            None,
            &DialerCode::this_process(),
            Instant::now() + Duration::from_millis(200),
            &|| cancel_rx.try_recv().is_ok() || in_flight.exited_before_check_in(),
        );
        assert_eq!(
            before.err(),
            Some(RendezvousError::Deadline),
            "the abort before the watch never asks about the named pid"
        );
        let isolated = aterm_spec::verify::buggy_baseline_with(&model, "IgnoreNamedPid");
        let mut state = isolated.init_state();
        for action in ["IgnoreNamedPid", "AnswerNamed", "Die"] {
            assert!(isolated.fire(action, &mut state), "{action}: {state:?}");
        }
        assert!(
            !isolated.action_enabled("EndOnNamedExit", &state),
            "and neither does the mutant: only the budget ends it — {state:?}"
        );
        let projected = project(
            &model,
            Observed {
                successor: DEAD,
                answer: NAMES_PID,
                slot: true,
                watches_named: false,
                ..Observed::start()
            },
        );
        assert!(
            !model.check_invariant("ANamedDeathIsSeen", &projected),
            "that abort's state is the one ANamedDeathIsSeen refuses"
        );
    }

    // A PID THAT CANNOT BE WATCHED TAKEN FOR GONE, run for real: LaunchServices
    // names pid 1, which is alive (`kill` answers EPERM, not ESRCH) and which
    // this process may not watch. Such an abort ends the wait at once — a live
    // successor refused (`EndOnNamedPid`) — where the shipping wait runs to
    // its budget (case 5 above).
    {
        // SAFETY: signal 0 checks existence and permission, delivering nothing.
        let alive = unsafe { libc::kill(1, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        assert!(alive, "pid 1 is there");
        let rendezvous = bind();
        let in_flight = LaunchInFlight::scripted(Some(1));
        let unwatchable_is_gone = || {
            in_flight.named_pid().is_some_and(|pid| {
                u32::try_from(pid)
                    .ok()
                    .is_none_or(|pid| aterm_uds::exitwatch::ExitWatch::watch(pid).is_none())
            })
        };
        assert_eq!(
            rendezvous
                .accept_claim(
                    None,
                    &DialerCode::this_process(),
                    far(),
                    &unwatchable_is_gone
                )
                .err(),
            Some(RendezvousError::Cancelled),
            "that abort ends the wait on a live process"
        );
        let projected = project(
            &model,
            Observed {
                answer: NAMES_PID,
                slot: true,
                wait: ENDED_ON_EXIT,
                ..Observed::start()
            },
        );
        let from = project(
            &model,
            Observed {
                answer: NAMES_PID,
                slot: true,
                ..Observed::start()
            },
        );
        assert_eq!(
            interp::admits(&model, &from, &projected),
            None,
            "the committed model never ends the wait on a live named process"
        );
        assert!(!model.check_invariant("NeverRefusesALiveSuccessor", &projected));
    }

    // A CONSUMING PRE-DIAL POLL, run for real: an abort that reads the answer
    // with the consuming `wait` leaves the corroboration after the accept
    // nothing to read — `ConsumingPoll`, caught by AnswerKeptForTheCorroboration.
    {
        let rendezvous = bind();
        let in_flight = LaunchInFlight::scripted(Some(9_999_999));
        let path = rendezvous.path().to_path_buf();
        let claim = rendezvous.claim().to_string();
        let consuming = || {
            let _ = in_flight.wait(Duration::ZERO);
            false
        };
        let accepted = std::thread::scope(|scope| {
            let dialer = scope.spawn(|| dial_for_test(&path, &claim).expect("dial and claim"));
            let accepted =
                rendezvous.accept_claim(None, &DialerCode::this_process(), far(), &consuming);
            drop(dialer.join().expect("the dialer thread"));
            accepted
        });
        let peer = admitted(accepted, "the dialer is accepted");
        let mut corroboration = LaunchCorroboration::of_dialer(
            &in_flight,
            HandoffCandidate::of_attested_peer(
                u32::try_from(peer.pid()).expect("an attested pid is positive"),
            ),
        );
        assert!(
            corroboration.poll().is_ok() && !corroboration.answered,
            "the stranger's pid (9999999) never reached the corroboration: the poll before the \
             dial took it, so a stranger that knew the secret would pass"
        );
        let projected = project(
            &model,
            Observed {
                answer: NAMES_PID,
                slot: false,
                successor: DIALLED,
                wait: ACCEPTED,
                ..Observed::start()
            },
        );
        assert!(!model.check_invariant("AnswerKeptForTheCorroboration", &projected));
    }
}

/// The machine's actions with no shipping code of their own on the outgoing
/// side, each waived by name. Bound above: `AnswerExited`/`AnswerOther`/
/// `AnswerNamed` (`app_launch_successor::admit_launch_answer`), `EndOnExit`
/// (`launch_answer_proves_exit`), `EndOnNamedExit`
/// (`NamedSuccessorWatch::gone`), `Accept`/`TimeOut` (`await_launched_dial`)
/// and `Corroborate` (`LaunchCorroboration::poll`).
mod waivers {
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_launched_dial_wait",
        action = "Dial",
        reason = "The successor's dial (handoff_rendezvous::claim_incoming), bound by \
                  NativeUpdateSuccessorWarmBeforeClaim's Tier-1; this conformance dials the real \
                  listener with the real claim frame."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_launched_dial_wait",
        action = "Die",
        reason = "The environment: a successor that dies before its dial (a warm-prologue fatal, \
                  a boot crash). Driven live through the ATERM_DEBUG_DIE_BEFORE_DIAL seam in \
                  main_entry; a unit test may not launch an application (AGENTS.md), so \
                  the named-pid half ends a real stand-in process instead."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_launched_dial_wait",
        action = "Expire",
        reason = "The environment: the dial budget running out (handoff_ready_deadline)."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_launched_dial_wait",
        action = "EndOnAnyAnswer",
        reason = "The Buggy=1 mutant: an answer that proves nothing read as death; no shipping \
                  code, and the conformance's negative control replays it and sees it refused."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_launched_dial_wait",
        action = "ConsumingPoll",
        reason = "The Buggy=1 mutant: the pre-dial poll taking the answer; no shipping code (the \
                  wait peeks), replayed for real and caught by the conformance."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_launched_dial_wait",
        action = "PollCancelOnly",
        reason = "The Buggy=1 liveness mutant: the retired cancel-only abort; replayed for real \
                  by the conformance, which waits out its budget beside a reported death."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_launched_dial_wait",
        action = "EndOnNamedPid",
        reason = "The Buggy=1 mutant: a named pid taken for gone without the kernel's word; no \
                  shipping code, replayed for real on pid 1 and refused by the conformance."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_launched_dial_wait",
        action = "IgnoreNamedPid",
        reason = "The Buggy=1 liveness mutant: the abort before the named-pid watch; replayed \
                  for real by the conformance, which waits out its budget beside a named \
                  process that is gone."
    )]
    #[expect(
        dead_code,
        reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it"
    )]
    fn launched_dial_wait_waivers() {}
}
