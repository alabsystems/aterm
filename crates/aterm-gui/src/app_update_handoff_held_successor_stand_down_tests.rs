// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    CandidateExitWatch, HandoffCandidate, HandoffRollbackWarrant, HeldSuccessor, StandDownStep,
    StoodDownSuccessor,
};
use aterm_spec::derive::{Model, native_update_overlap_handoff_model};
use aterm_spec::interp::{State, with_buggy};
use std::io::Read as _;

/// Binds a rendezvous, dials it from a test thread, accepts and holds the claim,
/// and spawns `script` as the process the claim stands for. `None` without a
/// private control dir.
fn hold(
    script: &str,
) -> Option<(
    HeldSuccessor,
    aterm_uds::spawnfd::Child,
    aterm_uds::CtlStream,
)> {
    hold_with_stdin(script, aterm_uds::spawnfd::Stdio::null())
}

/// [`hold`] with the stand-in's stdin chosen by the caller: a pipe lets the test
/// decide when the stand-in exits (`read` returns on EOF), forcing an
/// interleaving.
fn hold_with_stdin(
    script: &str,
    stdin: aterm_uds::spawnfd::Stdio,
) -> Option<(
    HeldSuccessor,
    aterm_uds::spawnfd::Child,
    aterm_uds::CtlStream,
)> {
    let nonce = aterm_uds::rand::hex_token::<16>().ok()?;
    let rendezvous = match crate::handoff_rendezvous::Rendezvous::bind(&nonce) {
        Ok(rendezvous) => rendezvous,
        Err(
            crate::handoff_rendezvous::RendezvousError::NoControlDir
            | crate::handoff_rendezvous::RendezvousError::PathTooLong { .. },
        ) => return None,
        Err(error) => panic!("bind: {error}"),
    };
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
    Some((
        HeldSuccessor {
            rendezvous,
            peer,
            candidate,
            exit_watch,
            dialled_at: std::time::Instant::now(),
        },
        child,
        stream,
    ))
}

/// The candidate is our fork child here, so someone must reap it or its pid
/// never becomes vacant (launchd reaps the real one).
fn reap_in_background(
    mut child: aterm_uds::spawnfd::Child,
) -> std::thread::JoinHandle<std::process::ExitStatus> {
    std::thread::spawn(move || child.wait().expect("reap the stand-in"))
}

/// Replays the steps the real stand-down emitted against the derived overlap
/// model from the launched-but-ungranted state, requiring each to be enabled. A
/// stand-down that skipped the kill or retired before the reap has no admitted
/// trace.
fn project_onto_the_model(model: &Model, stood: &StoodDownSuccessor) -> State {
    let mut state = model.init_state();
    // Launched before the park, nothing granted, and the main thread's
    // stand-down announcement answered (`answer_prelaunched_stand_down`).
    for action in ["LaunchSuccessorBeforePark", "RevokeUngrantedSuccessor"] {
        let next = model.successors(action, &state);
        assert_eq!(next.len(), 1, "{action} must be enabled at {state:?}");
        state = next[0].clone();
    }
    // `Revoke` is that step; the rest are what the code chose.
    assert_eq!(
        stood.steps.first(),
        Some(&StandDownStep::Revoke),
        "a stand-down always begins by closing the rendezvous"
    );
    for step in stood.steps.iter().skip(1) {
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
    retired
}

/// Negative control: retiring while the successor is live is executable only in
/// the mutant, and the invariant catches it.
fn assert_a_live_successor_cannot_retire(model: &Model) {
    let mut state = model.init_state();
    for action in ["LaunchSuccessorBeforePark", "RevokeUngrantedSuccessor"] {
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
    let Some((held, mut child, stream)) =
        hold_with_stdin(EXIT_ON_STDIN_EOF, aterm_uds::spawnfd::Stdio::piped())
    else {
        return;
    };
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
    let retired = project_onto_the_model(&model, &stood);
    assert_eq!(retired["group_signaled"], 0, "nothing of ours signalled it");
    assert_eq!(retired["successor_exited"], 1);
    assert_a_live_successor_cannot_retire(&model);
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
    let Some((held, mut child, stream)) =
        hold_with_stdin(EXIT_ON_STDIN_EOF, aterm_uds::spawnfd::Stdio::piped())
    else {
        return;
    };
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
    let retired = project_onto_the_model(&model, &stood);
    assert_eq!(retired["successor_exited"], 1);
    assert_eq!(retired["group_signaled"], 0);
}

/// The hold loop's "did it exit?" check reads the one-shot witness and the
/// stand-down reads the queue again; the candidate has exited and been reaped
/// before either read. The exit must still be witnessed.
#[test]
fn a_witness_the_hold_loop_already_read_still_names_the_exit_at_the_stand_down() {
    let Some((held, mut child, stream)) =
        hold_with_stdin(EXIT_ON_STDIN_EOF, aterm_uds::spawnfd::Stdio::piped())
    else {
        return;
    };
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
    let Some((held, child, stream)) = hold("sleep 30") else {
        return;
    };
    let reaper = reap_in_background(child);
    let started = std::time::Instant::now();
    let stood = super::stand_down_held_successor(held, std::time::Duration::from_millis(200));
    assert!(
        stood.signalled,
        "the grace expired, so the candidate was killed"
    );
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(200),
        "the grace is honoured before the kill"
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
    let retired = project_onto_the_model(&model, &stood);
    assert_eq!(retired["child_killed"], 1);
    assert_eq!(retired["group_signaled"], 1);
    assert_a_live_successor_cannot_retire(&model);
}

/// A held successor killed by the machine (a SIGKILL not ours) never ran its
/// EOF-rule exit, so the parent gives its counted launch back, as the pre-dial
/// arm does (`send_pre_dial_death`).
#[test]
fn a_held_successor_the_machine_killed_has_its_counted_launch_given_back() {
    use std::os::unix::process::ExitStatusExt as _;
    let Some((held, mut child, _stream)) = hold_with_stdin(
        "read line; kill -KILL $$",
        aterm_uds::spawnfd::Stdio::piped(),
    ) else {
        return;
    };
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
