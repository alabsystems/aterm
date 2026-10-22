// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    CandidateExitWatch, HandoffCandidate, HandoffRollbackWarrant, HeldSuccessor, StandDownStep,
};
use aterm_models_misc::native_update_overlap_handoff_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, with_buggy};
use std::io::Read as _;

/// What [`hold`] hands a test: the held claim, the stand-in, the dialer's end of the
/// stream, and the private directory the rendezvous was bound in where production
/// had none (held as long as the test runs).
type Held = (
    HeldSuccessor,
    aterm_uds::spawnfd::Child,
    aterm_uds::CtlStream,
);

/// Binds a rendezvous (where production binds it: in this test binary, a private
/// directory of its own, so the test runs on every host), dials it from a test
/// thread, accepts and holds the claim, and spawns `script` as the process the
/// claim stands for.
fn hold(script: &str) -> Held {
    hold_with_stdin(script, aterm_uds::spawnfd::Stdio::null())
}

/// [`hold`] with the stand-in's stdin chosen by the caller: a pipe lets the test
/// decide when the stand-in exits (`read` returns on EOF), forcing an
/// interleaving.
fn hold_with_stdin(script: &str, stdin: aterm_uds::spawnfd::Stdio) -> Held {
    let nonce = aterm_uds::rand::hex_token::<16>().expect("a rendezvous nonce");
    let rendezvous =
        crate::handoff_rendezvous::Rendezvous::bind(&nonce).expect("bind a rendezvous");
    let path = rendezvous.path().to_path_buf();
    let secret = rendezvous.claim().to_string();
    let dialer = std::thread::spawn(move || {
        crate::handoff_rendezvous::dial_for_test(&path, &secret).expect("dial")
    });
    let peer = crate::handoff_rendezvous::admitted(
        rendezvous.accept_claim(
            None,
            &crate::handoff_rendezvous::DialerCode::this_process(),
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            &|| false,
        ),
        "the test dialer presents the claim",
    );
    let stream = dialer.join().expect("dialer thread");
    let child = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdin(stdin)
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the stand-in candidate");
    let candidate = HandoffCandidate::of_unreaped_spawned(&child);
    let exit_watch = CandidateExitWatch::watch(child.id());
    assert!(exit_watch.is_some(), "EVFILT_PROC attaches to a live child");
    (
        HeldSuccessor {
            rendezvous,
            peer,
            candidate,
            exit_watch,
            dialled_at: std::time::Instant::now(),
        },
        child,
        stream,
    )
}

/// The candidate is our fork child here, so someone must reap it or its pid
/// never becomes vacant (launchd reaps the real one).
fn reap_in_background(
    mut child: aterm_uds::spawnfd::Child,
) -> std::thread::JoinHandle<std::process::ExitStatus> {
    std::thread::spawn(move || child.wait().expect("reap the stand-in"))
}

/// Replays the steps the real stand-down emitted against the derived overlap
/// model from the launched-but-ungranted state — dialled and held, or (round
/// eight's lead U41) never dialled — requiring each to be enabled. A stand-down
/// that skipped the kill or retired before the reap has no admitted trace.
fn project_onto_the_model(model: &Model, steps: &[StandDownStep], dialled: bool) -> State {
    let mut state = model.init_state();
    // Launched before the park, dialled or not, nothing granted, and the
    // stand-down begun: the main thread's announcement answered
    // (`answer_prelaunched_stand_down`), or the dial wait ended without a dial.
    let dial = dialled.then_some("SuccessorDials");
    for action in ["LaunchSuccessorBeforePark"]
        .into_iter()
        .chain(dial)
        .chain(["RevokeUngrantedSuccessor"])
    {
        let next = model.successors(action, &state);
        assert_eq!(next.len(), 1, "{action} must be enabled at {state:?}");
        state = next[0].clone();
    }
    // `Revoke` is that step; the rest are what the code chose.
    assert_eq!(
        steps.first(),
        Some(&StandDownStep::Revoke),
        "a stand-down always begins by closing the rendezvous"
    );
    for step in steps.iter().skip(1) {
        let action = step.model_action();
        let next = model.successors(action, &state);
        assert_eq!(
            next.len(),
            1,
            "the code emitted {step:?}, which the model does not admit at {state:?}"
        );
        state = next[0].clone();
    }
    // And only now may the attempt retire — the completion the worker sends
    // next is what clears the record.
    let retired = model.successors("RetireUngrantedAttempt", &state);
    assert_eq!(
        retired.len(),
        1,
        "the steps the code took must license the retire: {state:?}"
    );
    let retired = retired[0].clone();
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, &retired),
            "the projected stand-down violates {}: {retired:?}",
            invariant.name
        );
    }
    assert_eq!(retired["parent_readers"], 1, "the readers are the user's");
    assert_eq!(retired["granted"], 0, "nothing was ever granted");
    assert_eq!(retired["child_live"], 0, "the successor is proven gone");
    assert_eq!(retired["dialled"], i64::from(dialled));
    retired
}

/// Negative control: retiring while the successor is live is executable only in
/// the mutant, and the invariant catches it — dialled or not: the undialled
/// trace is the dial-deadline arm as it shipped before lead U41.
fn assert_a_live_successor_cannot_retire(model: &Model, dialled: bool) {
    let mut state = model.init_state();
    let dial = dialled.then_some("SuccessorDials");
    for action in ["LaunchSuccessorBeforePark"]
        .into_iter()
        .chain(dial)
        .chain(["RevokeUngrantedSuccessor"])
    {
        state = model.successors(action, &state)[0].clone();
    }
    assert!(
        model
            .successors("RetireUngrantedAttempt", &state)
            .is_empty(),
        "the healthy model never retires over a live successor"
    );
    let buggy = with_buggy(model, 1);
    let mut early = state.clone();
    assert!(buggy.fire("BuggyRetireUngrantedWithSuccessorLive", &mut early));
    assert!(!buggy.check_invariant("UngrantedRetireRequiresReap", &early));
}

fn reads_eof(stream: &aterm_uds::CtlStream) -> bool {
    let mut byte = [0u8; 1];
    let _ = stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN));
    matches!((&*stream).read(&mut byte), Ok(0))
}

/// A successor that exits on the EOF within the grace is proven gone without a
/// signal and its own exit status witnessed, so the parent does not also give
/// back the trial launch.
#[test]
fn a_successor_that_exits_on_eof_is_proven_gone_without_a_signal() {
    // The stand-in exits when the hook below closes its stdin, once the stand-down has revoked
    // and found it running: forced inside the grace, with no timing for the exit watch to beat
    // (a timed stand-in could exit before `CandidateExitWatch::watch` attached under load).
    let (held, mut child, stream) =
        hold_with_stdin(EXIT_ON_STDIN_EOF, aterm_uds::spawnfd::Stdio::piped());
    let mut stdin = Some(child.stdin.take().expect("piped stdin"));
    let reaper = reap_in_background(child);
    let stood =
        super::stand_down_held_successor_between(held, aterm_spec::wait::MUST_HAPPEN, &mut || {
            drop(stdin.take());
        });
    assert!(!stood.signalled, "an EOF exit needs no kill");
    assert_eq!(stood.warrant, HandoffRollbackWarrant::Vanished);
    assert_eq!(
        stood.death,
        crate::ChildDeathEvidence::Exited { code: 0 },
        "the witness records the candidate's OWN exit"
    );
    assert!(
        !stood.forgives_the_counted_trial_launch(1, 2),
        "its own exit gave its own launch back"
    );
    assert!(reads_eof(&stream), "the dialer read the stand-down as EOF");
    assert_eq!(reaper.join().expect("reaper").code(), Some(0));
    // The order taken, against the model: revoke, its own exit, the proof.
    assert_eq!(
        stood.steps,
        vec![
            StandDownStep::Revoke,
            StandDownStep::SuccessorExited,
            StandDownStep::Reap
        ]
    );
    let model = native_update_overlap_handoff_model();
    let retired = project_onto_the_model(&model, &stood.steps, true);
    assert_eq!(retired["group_signaled"], 0, "nothing of ours signalled it");
    assert_eq!(retired["successor_exited"], 1);
    assert_a_live_successor_cannot_retire(&model, true);
}

/// A stand-in that exits `0` exactly when the test closes its stdin, and never before, so
/// the losing interleavings below are forced, not waited for.
const EXIT_ON_STDIN_EOF: &str = "read line; exit 0";

/// The losing interleaving, forced: the loop reads the witness (the stand-in is
/// blocked on stdin), then the stand-in exits and is reaped before the pid
/// probe. The stand-down must still name the candidate's own exit, because XNU
/// queued the knote before the pid fell vacant.
#[test]
fn the_exit_between_the_witness_read_and_the_pid_probe_is_still_witnessed() {
    let (held, mut child, stream) =
        hold_with_stdin(EXIT_ON_STDIN_EOF, aterm_uds::spawnfd::Stdio::piped());
    let mut stdin = Some(child.stdin.take().expect("piped stdin"));
    let mut reaper = Some(reap_in_background(child));
    let mut reaped = None;
    let mut misses = 0_u32;
    let stood =
        super::stand_down_held_successor_between(held, aterm_spec::wait::MUST_HAPPEN, &mut || {
            misses += 1;
            // Only the first miss exits it; a second would mean the probe failed to see a
            // reaped pid as gone.
            if let Some(stdin) = stdin.take() {
                drop(stdin);
                reaped = Some(reaper.take().expect("one reaper").join().expect("reaper"));
            }
        });
    assert_eq!(
        misses, 1,
        "exactly one witness miss, and the reap landed inside it — the window \
         this test exists to force"
    );
    assert_eq!(
        reaped
            .expect("the stand-in was reaped inside the window")
            .code(),
        Some(0)
    );
    assert!(!stood.signalled, "an exit of its own needs no kill");
    assert_eq!(stood.warrant, HandoffRollbackWarrant::Vanished);
    assert_eq!(
        stood.death,
        crate::ChildDeathEvidence::Exited { code: 0 },
        "the candidate's own exit, reaped between the read and the probe, is \
         still what the stand-down reports"
    );
    assert!(reads_eof(&stream), "the dialer read the stand-down as EOF");
    assert_eq!(
        stood.steps,
        vec![
            StandDownStep::Revoke,
            StandDownStep::SuccessorExited,
            StandDownStep::Reap
        ]
    );
    let model = native_update_overlap_handoff_model();
    let retired = project_onto_the_model(&model, &stood.steps, true);
    assert_eq!(retired["successor_exited"], 1);
    assert_eq!(retired["group_signaled"], 0);
}

/// The hold loop's "did it exit?" check reads the one-shot witness and the
/// stand-down reads the queue again; the candidate has exited and been reaped
/// before either read. The exit must still be witnessed.
#[test]
fn a_witness_the_hold_loop_already_read_still_names_the_exit_at_the_stand_down() {
    let (held, mut child, stream) =
        hold_with_stdin(EXIT_ON_STDIN_EOF, aterm_uds::spawnfd::Stdio::piped());
    drop(child.stdin.take().expect("piped stdin"));
    let status = reap_in_background(child).join().expect("reaper");
    assert_eq!(status.code(), Some(0));
    assert_eq!(
        held.exit_watch
            .as_ref()
            .and_then(CandidateExitWatch::exit_status),
        Some(status),
        "the hold loop's own read sees the exit"
    );
    let mut misses = 0_u32;
    let stood =
        super::stand_down_held_successor_between(held, aterm_spec::wait::MUST_HAPPEN, &mut || {
            misses += 1
        });
    assert!(!stood.signalled, "an exit of its own needs no kill");
    assert_eq!(stood.warrant, HandoffRollbackWarrant::Vanished);
    assert_eq!(
        stood.death,
        crate::ChildDeathEvidence::Exited { code: 0 },
        "a witness read twice names the exit twice"
    );
    assert_eq!(
        misses, 0,
        "the witness had already answered, so the stand-down's first read does too"
    );
    assert!(reads_eof(&stream), "the dialer read the stand-down as EOF");
    assert_eq!(
        stood.steps,
        vec![
            StandDownStep::Revoke,
            StandDownStep::SuccessorExited,
            StandDownStep::Reap
        ]
    );
}

/// A successor ignoring the EOF for the whole grace is killed and proven gone;
/// the kill is ours, so the death is `Unobserved`.
#[test]
fn a_successor_that_ignores_eof_is_killed_after_the_grace_and_proven_gone() {
    let (held, child, stream) = hold("sleep 30");
    let reaper = reap_in_background(child);
    let stood = super::stand_down_held_successor(held, std::time::Duration::from_millis(200));
    assert!(
        stood.signalled,
        "the grace expired, so the candidate was killed"
    );
    assert_eq!(stood.warrant, HandoffRollbackWarrant::Vanished);
    assert_eq!(
        stood.death,
        crate::ChildDeathEvidence::Unobserved,
        "our own SIGKILL is not evidence about the bytes"
    );
    assert!(reads_eof(&stream), "the dialer read the stand-down as EOF");
    use std::os::unix::process::ExitStatusExt as _;
    assert_eq!(reaper.join().expect("reaper").signal(), Some(libc::SIGKILL));
    // The order taken, against the model: revoke, kill, the proof
    // (`UngrantedRetireRequiresReap`).
    assert_eq!(
        stood.steps,
        vec![
            StandDownStep::Revoke,
            StandDownStep::Kill,
            StandDownStep::Reap
        ]
    );
    let model = native_update_overlap_handoff_model();
    let retired = project_onto_the_model(&model, &stood.steps, true);
    assert_eq!(retired["child_killed"], 1);
    assert_eq!(retired["group_signaled"], 1);
    assert_a_live_successor_cannot_retire(&model, true);
}

/// A held successor killed by the machine (a SIGKILL not ours) never ran its
/// EOF-rule exit, so the parent gives its counted launch back, as the pre-dial
/// arm does (`send_pre_dial_death`).
#[test]
fn a_held_successor_the_machine_killed_has_its_counted_launch_given_back() {
    use std::os::unix::process::ExitStatusExt as _;
    let (held, mut child, _stream) = hold_with_stdin(
        "read line; kill -KILL $$",
        aterm_uds::spawnfd::Stdio::piped(),
    );
    drop(child.stdin.take().expect("piped stdin"));
    let status = reap_in_background(child).join().expect("reaper");
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    let stood = super::stand_down_held_successor(held, aterm_spec::wait::MUST_HAPPEN);
    assert_eq!(
        stood.death,
        crate::ChildDeathEvidence::Signalled {
            signal: libc::SIGKILL
        },
        "its own death, witnessed before any signal of ours"
    );
    assert!(
        stood.forgives_the_counted_trial_launch(1, 2),
        "nobody else gives back the launch it counted"
    );
}

/// A bound rendezvous, where production binds it (in this test binary, a
/// private directory of its own: `handoff_rendezvous::rendezvous_dir_candidate`).
fn bind_rendezvous() -> crate::handoff_rendezvous::Rendezvous {
    let nonce = aterm_uds::rand::hex_token::<16>().expect("a rendezvous nonce");
    crate::handoff_rendezvous::Rendezvous::bind(&nonce).expect("bind a rendezvous")
}

/// A launched successor that never dials: alive until killed.
fn never_dials() -> aterm_uds::spawnfd::Child {
    aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "sleep 60"])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the undialled stand-in")
}

/// The stand-down of an undialled successor, checked against the overlap model
/// from the launched-but-undialled state, and its own negative control.
fn assert_the_undialled_stand_down_licenses_the_retire(steps: &[StandDownStep]) {
    let model = native_update_overlap_handoff_model();
    let retired = project_onto_the_model(&model, steps, false);
    assert_eq!(retired["dialled"], 0, "it never dialled");
    assert_a_live_successor_cannot_retire(&model, false);
}

/// ROUND EIGHT'S LEAD U41. The dial budget runs out with the launched successor
/// alive and undialled — inside its boot on a loaded desk, or wedged — and the
/// deadline arm retired the attempt over it, killing nothing: a retry launched
/// beside it, a wedged one lived on window-less, and one the machine killed
/// later kept its counted launch. Now it is named (here by the pid LaunchServices
/// named, as the wait first saw it), killed after the grace and proven gone,
/// and our kill gives its counted launch back, all before the retire.
///
/// FAILS WITHOUT THE FIX: the arm as it shipped is the state checked below
/// right after the wait, the stand-in still running, and its model trace
/// (retire straight after the revoke) breaks `UngrantedRetireRequiresReap`.
#[test]
fn a_successor_undialled_at_the_dial_deadline_is_killed_and_proven_gone_before_the_retire() {
    use std::os::unix::process::ExitStatusExt as _;
    let rendezvous = bind_rendezvous();
    let child = never_dials();
    let pid = child.id();
    let reaper = reap_in_background(child);
    let in_flight = crate::app_launch_successor::LaunchInFlight::scripted(Some(
        i32::try_from(pid).expect("a pid fits pid_t"),
    ));
    let named = std::cell::RefCell::new(super::NamedSuccessorWatch::default());
    let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let ended = super::await_launched_dial(
        &rendezvous,
        &in_flight,
        &crate::handoff_rendezvous::DialerCode::this_process(),
        std::time::Instant::now() + std::time::Duration::from_millis(200),
        &cancel_rx,
        &named,
    );
    assert_eq!(
        ended.err(),
        Some(super::LaunchedDialEnd::Rendezvous(
            crate::handoff_rendezvous::RendezvousError::Deadline
        ))
    );
    let named = named.into_inner();
    let seen = named
        .seen
        .as_ref()
        .map(|(candidate, _)| *candidate)
        .expect("the wait saw the pid LaunchServices named");
    assert_eq!(seen.pid, pid);
    assert!(
        !super::handoff_candidate_terminated(seen),
        "where the arm used to retire, the successor still runs"
    );
    let end = super::stand_down_undialled_successor(
        rendezvous,
        &in_flight,
        named,
        std::time::Duration::from_millis(200),
        aterm_spec::wait::MUST_HAPPEN,
    );
    let super::UndialledStandDown::Gone {
        named,
        death,
        steps,
    } = end
    else {
        panic!("the named successor was not stood down: {end:?}");
    };
    assert_eq!(named, Some(seen));
    assert!(super::handoff_candidate_terminated(seen), "proven gone");
    assert_eq!(reaper.join().expect("reaper").signal(), Some(libc::SIGKILL));
    assert_eq!(
        death,
        crate::ChildDeathEvidence::Unobserved,
        "our own SIGKILL is not evidence about the bytes"
    );
    assert!(
        super::forgives_the_counted_trial_launch(death, 1, 2),
        "a launch we killed is given back by us"
    );
    assert_eq!(
        steps,
        vec![
            StandDownStep::Revoke,
            StandDownStep::Kill,
            StandDownStep::Reap
        ]
    );
    assert_the_undialled_stand_down_licenses_the_retire(&steps);
}

/// BOTH ARMS, AS THE LAUNCHED LANE RUNS THEM: the cancel and the dial deadline are
/// filed by [`super::file_undialled_retire`], the one call both arms make, and its
/// rollback is made only once the successor is proven gone — here a live stand-in
/// LaunchServices named, killed and reaped by the time the rollback exists — with
/// the counted launch our kill ended given back by its name.
///
/// FAILS WITHOUT THE FIX: the arms published the rollback at once, the stand-in
/// still running and its launch kept; a filing that skips the stand-down gives
/// nothing back here.
#[test]
fn the_undialled_ends_retire_only_over_a_successor_proven_gone() {
    use std::os::unix::process::ExitStatusExt as _;
    let job = super::launched_dialer_admission_conformance::launched_job(true, false, false, false);
    assert!(
        job.target_build > job.current_build,
        "PRECONDITION: a real apply, whose trial launch is counted"
    );
    for (end, outcome) in [
        (
            super::LaunchedDialEnd::Cancelled,
            crate::UpdateHandoffOutcome::ActivityRevoked,
        ),
        (
            super::LaunchedDialEnd::Rendezvous(
                crate::handoff_rendezvous::RendezvousError::Deadline,
            ),
            crate::UpdateHandoffOutcome::TimedOut,
        ),
    ] {
        let rendezvous = bind_rendezvous();
        let child = never_dials();
        let pid = child.id();
        let reaper = reap_in_background(child);
        let in_flight = crate::app_launch_successor::LaunchInFlight::scripted(Some(
            i32::try_from(pid).expect("a pid fits pid_t"),
        ));
        let retire = super::file_undialled_retire(
            &job,
            "undialled".to_string(),
            &end,
            rendezvous,
            &in_flight,
            super::NamedSuccessorWatch::default(),
            std::time::Duration::from_millis(200),
            aterm_spec::wait::MUST_HAPPEN,
        );
        let given = retire
            .give_back
            .unwrap_or_else(|| panic!("{end:?}: the launch our kill ended is given back"));
        assert_eq!(given.pid, pid, "{end:?}");
        assert!(
            super::handoff_candidate_terminated(given),
            "{end:?}: proven gone before the rollback exists"
        );
        assert_eq!(
            reaper.join().expect("reaper").signal(),
            Some(libc::SIGKILL),
            "{end:?}"
        );
        let completion = &retire.completion;
        assert_eq!(completion.outcome, outcome, "{end:?}");
        assert_eq!(completion.attempt_id, job.attempt_id);
        assert_eq!(completion.nonce.as_deref(), Some("undialled"));
        assert_eq!(
            completion.child_pid, None,
            "nothing downstream acts on a process no dial attested"
        );
        assert_eq!(
            completion.child_death,
            crate::ChildDeathEvidence::Unobserved,
            "our own SIGKILL is not evidence about the bytes"
        );
    }
}

/// The cancel before the dial, LaunchServices not yet answered: the successor is
/// named by the launch it stamped beside the rendezvous (its pid and birth
/// stamp), killed and proven gone. FAILS WITHOUT THE FIX as the deadline case
/// does: the arm retired at once over the running stand-in.
#[test]
fn a_successor_undialled_at_the_cancel_is_named_by_its_launch_stamp() {
    let rendezvous = bind_rendezvous();
    let child = never_dials();
    let pid = child.id();
    let birth = super::read_candidate_birth(pid).expect("a live child's birth stamp");
    let reaper = reap_in_background(child);
    // What the successor's `stamp_this_launch` writes before its boot apply.
    std::fs::write(
        crate::handoff_rendezvous::launch_stamp_path(rendezvous.path()),
        format!("{pid} {} {}\n", birth.seconds, birth.microseconds),
    )
    .expect("stamp the stand-in's launch");
    let in_flight = crate::app_launch_successor::LaunchInFlight::scripted(None);
    let named = std::cell::RefCell::new(super::NamedSuccessorWatch::default());
    let (cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel::<()>(1);
    cancel_tx.send(()).expect("cancel");
    let ended = super::await_launched_dial(
        &rendezvous,
        &in_flight,
        &crate::handoff_rendezvous::DialerCode::this_process(),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        &cancel_rx,
        &named,
    );
    assert_eq!(ended.err(), Some(super::LaunchedDialEnd::Cancelled));
    let end = super::stand_down_undialled_successor(
        rendezvous,
        &in_flight,
        named.into_inner(),
        std::time::Duration::from_millis(200),
        aterm_spec::wait::MUST_HAPPEN,
    );
    let super::UndialledStandDown::Gone {
        named: Some(named),
        steps,
        ..
    } = end
    else {
        panic!("the stamped successor was not stood down: {end:?}");
    };
    assert_eq!(named.pid, pid);
    assert_eq!(named.birth, Some(birth), "named by its own stamp");
    assert!(super::handoff_candidate_terminated(named), "proven gone");
    use std::os::unix::process::ExitStatusExt as _;
    assert_eq!(reaper.join().expect("reaper").signal(), Some(libc::SIGKILL));
    assert_the_undialled_stand_down_licenses_the_retire(&steps);
}

/// LaunchServices' word that the successor terminated before it checked in is
/// the proof itself: nothing is signalled, and its stamped launch is the name
/// its counted launch is settled by. With no word and no stamp, nothing names it
/// and nothing is claimed.
#[test]
fn an_undialled_successor_reported_gone_is_not_signalled_and_an_unnamed_one_is_not_claimed() {
    let rendezvous = bind_rendezvous();
    let in_flight = crate::app_launch_successor::LaunchInFlight::scripted(None);
    in_flight.answer_for_test(
        crate::app_launch_successor::LaunchInFlight::admitted_for_test(
            -1,
            true,
            i32::try_from(std::process::id()).expect("a pid fits pid_t"),
        ),
    );
    // A stamp naming a launch whose process is gone: it names the counted launch.
    let gone = never_dials();
    let (pid, birth) = (
        gone.id(),
        super::read_candidate_birth(gone.id()).expect("a live child's birth stamp"),
    );
    std::fs::write(
        crate::handoff_rendezvous::launch_stamp_path(rendezvous.path()),
        format!("{pid} {} {}\n", birth.seconds, birth.microseconds),
    )
    .expect("stamp the launch");
    let mut gone = gone;
    let _ = gone.kill();
    let _ = gone.wait();
    let end = super::stand_down_undialled_successor(
        rendezvous,
        &in_flight,
        super::NamedSuccessorWatch::default(),
        std::time::Duration::from_millis(200),
        aterm_spec::wait::MUST_HAPPEN,
    );
    assert_eq!(
        end,
        super::UndialledStandDown::Gone {
            named: Some(HandoffCandidate {
                pid,
                birth: Some(birth)
            }),
            death: crate::ChildDeathEvidence::Unobserved,
            steps: vec![
                StandDownStep::Revoke,
                StandDownStep::SuccessorExited,
                StandDownStep::Reap
            ],
        }
    );
    assert_the_undialled_stand_down_licenses_the_retire(&[
        StandDownStep::Revoke,
        StandDownStep::SuccessorExited,
        StandDownStep::Reap,
    ]);

    let rendezvous = bind_rendezvous();
    let end = super::stand_down_undialled_successor(
        rendezvous,
        &crate::app_launch_successor::LaunchInFlight::scripted(None),
        super::NamedSuccessorWatch::default(),
        std::time::Duration::from_millis(200),
        std::time::Duration::from_millis(50),
    );
    assert_eq!(end, super::UndialledStandDown::Unnamed);
}
