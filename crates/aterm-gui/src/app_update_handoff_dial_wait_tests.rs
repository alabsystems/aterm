// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Conformance tests for `native_update_launched_dial_wait_model` and `await_launched_dial`.

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
    DialerCode, Rendezvous, RendezvousError, admitted, dial_for_test, held_for_trustd,
    trustd_silent,
};
use aterm_models_update::native_update_launched_dial_wait_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use std::time::{Duration, Instant};

const ALIVE: i64 = 0;
const DIALLED: i64 = 1;
const DEAD: i64 = 2;
const NONE: i64 = 0;
const PROVES_NOTHING: i64 = 1;
const PROVES_EXIT: i64 = 2;
const NAMES_PID: i64 = 3;
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
    /// The abort in use asks the kernel about a named pid (the shipping one does).
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

fn step(model: &Model, prev: Observed, next: Observed, action: &str) {
    let (from, to) = (project(model, prev), project(model, next));
    crate::conformance_support::assert_transition(model, &from, &to, action);
}

fn answer_class(answer: &Result<LaunchedSuccessor, LaunchError>) -> i64 {
    if launch_answer_proves_exit(Some(answer)) {
        PROVES_EXIT
    } else if launch_answer_names_pid(Some(answer)).is_some() {
        NAMES_PID
    } else {
        PROVES_NOTHING
    }
}

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

fn bind() -> Rendezvous {
    let nonce = aterm_uds::rand::hex_token::<16>().expect("a nonce");
    Rendezvous::bind(&nonce).expect("bind a rendezvous for the Tier-1")
}

fn far() -> Instant {
    Instant::now() + Duration::from_secs(120)
}

fn named_stand_in() -> aterm_uds::spawnfd::Child {
    aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "read _; exit 70"])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("spawn the named stand-in")
}

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

fn launch_services_shapes() -> [(i32, bool, bool); 6] {
    [
        (-1, true, true),
        (4242, false, false),
        (4243, false, false),
        (-1, false, false),
        (4244, true, true),
        (own_pid(), false, false),
    ]
}

#[test]
fn the_real_dial_wait_refines_the_model() {
    let model = native_update_launched_dial_wait_model();
    assert!(
        aterm_spec::xref::reset_entered_anchors(),
        "the evidence window must open"
    );

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
                &Default::default(),
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
        let evidence = PreDialDeath::TerminatedBeforeCheckIn.evidence();
        assert_eq!(
            PhysicalFailureShape::of_child_death(evidence),
            PhysicalFailureShape::Unexplained
        );
        assert!(forgives_the_counted_trial_launch(evidence, 1, 2));
    }

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
        let evidence = ChildDeathEvidence::Exited { code: 70 };
        assert_eq!(
            PhysicalFailureShape::of_child_death(evidence),
            PhysicalFailureShape::Structural
        );
        assert!(!forgives_the_counted_trial_launch(evidence, 1, 2));
    }

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
            &Default::default(),
        );
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
                &Default::default(),
            );
            (accepted, dialer.join().expect("the dialer thread"))
        });
        let peer = match accepted {
            Ok(peer) => peer,
            Err(LaunchedDialEnd::Rendezvous(error)) if held_for_trustd(&error) => {
                trustd_silent(error)
            }
            Err(end @ LaunchedDialEnd::ExitedWhileHeld { .. }) => {
                trustd_silent(format_args!("{end:?}"))
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
            &Default::default(),
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
                &cancel_rx,
                &Default::default(),
            )
            .err(),
            Some(LaunchedDialEnd::Cancelled)
        );
    }

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

#[test]
fn the_models_mutants_are_the_defects_they_name() {
    let model = native_update_launched_dial_wait_model();

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
