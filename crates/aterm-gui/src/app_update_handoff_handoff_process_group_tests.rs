// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    HandoffCandidate, HandoffCandidateHandle, HandoffCommitFacts, HandoffRejectDelivery,
    HandoffRollbackWarrant, ProcessGroupContainment, ReadyPollAction, classify_ready_poll,
    contain_own_process_group, deliver_handoff_rejection, emergency_kill_and_reap_handoff_child,
    handoff_candidate_terminated, handoff_commit_admitted, handoff_masters_closed,
    handoff_masters_have_activity, handoff_ready_deadline, kill_and_reap_handoff_child,
    make_cloexec_pipe, wait_handoff_ready, worker_claim_handoff_reaper,
};
use aterm_uds::spawnfd::InheritNothing as _;
use std::io::{BufRead as _, Read as _};
use std::os::unix::process::CommandExt as _;

struct ProcessGroupCleanup {
    leader: i32,
    armed: bool,
}

impl ProcessGroupCleanup {
    fn new(leader: i32) -> Self {
        Self {
            leader,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProcessGroupCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        unsafe { libc::kill(-self.leader, libc::SIGKILL) };
        let mut status = 0;
        loop {
            let waited = unsafe { libc::waitpid(self.leader, &mut status, 0) };
            if waited == self.leader
                || (waited < 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD))
            {
                break;
            }
            if waited < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                break;
            }
        }
    }
}

fn parked() -> String {
    format!("sleep {}", aterm_spec::wait::PARKED_SECS)
}

fn assert_process_group_gone(leader: i32, descendant: i32, message: &str) {
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        let group_gone = unsafe { libc::kill(-leader, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        let descendant_gone = unsafe { libc::kill(descendant, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if group_gone && descendant_gone {
            return;
        }
        if std::time::Instant::now() >= deadline {
            unsafe {
                libc::kill(-leader, libc::SIGKILL);
                libc::kill(descendant, libc::SIGKILL);
            }
            panic!("{message}");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn final_handoff_admission_conforms_to_every_model_guard() {
    let all = HandoffCommitFacts {
        exact_sessions: true,
        exact_layout: true,
        exact_agent_carry: true,
        recovery_count_ready: true,
        exact_activity: true,
        teardown_allows_commit: true,
        parent_still_parked: true,
        sessions_alive: true,
        input_dispatch_fenced: true,
        egress_settled: true,
        native_safe: true,
        proof_exact: true,
        commit_channel: true,
    };
    assert!(handoff_commit_admitted(all));

    assert!(
        !handoff_commit_admitted(HandoffCommitFacts {
            egress_settled: false,
            ..all
        }),
        "Commit must not fire while tolerated input is still in a process-local queue"
    );

    let cases = [
        (
            "sessions",
            HandoffCommitFacts {
                exact_sessions: false,
                ..all
            },
            "SessionsChange",
        ),
        (
            "layout",
            HandoffCommitFacts {
                exact_layout: false,
                ..all
            },
            "LayoutChanges",
        ),
        (
            "agent carry",
            HandoffCommitFacts {
                exact_agent_carry: false,
                ..all
            },
            "LayoutChanges",
        ),
        (
            "activity epoch",
            HandoffCommitFacts {
                exact_activity: false,
                ..all
            },
            "ActivityRevokesEpoch",
        ),
        (
            "teardown",
            HandoffCommitFacts {
                teardown_allows_commit: false,
                ..all
            },
            "DestructiveIntentRevokesCommit",
        ),
        (
            "parent reader",
            HandoffCommitFacts {
                parent_still_parked: false,
                ..all
            },
            "ParentReaderResumesBeforeCommit",
        ),
        (
            "session death",
            HandoffCommitFacts {
                sessions_alive: false,
                ..all
            },
            "PtySessionDies",
        ),
        (
            "queued hardware input",
            HandoffCommitFacts {
                input_dispatch_fenced: false,
                ..all
            },
            "QueueHardwareInput",
        ),
        (
            "native safety",
            HandoffCommitFacts {
                native_safe: false,
                ..all
            },
            "RevokeNativeSafety",
        ),
        (
            "proof",
            HandoffCommitFacts {
                proof_exact: false,
                ..all
            },
            "ChildSendsMismatchedProof",
        ),
        (
            "commit channel",
            HandoffCommitFacts {
                commit_channel: false,
                ..all
            },
            "LoseCommitChannel",
        ),
    ];
    for (name, facts, revoked_action) in cases {
        assert!(
            !handoff_commit_admitted(facts),
            "missing {name} was admitted"
        );
        let model = aterm_models_misc::native_update_overlap_handoff_model();
        let mut state = model.init_state();
        assert!(model.fire("ParkParentReaders", &mut state));
        assert!(model.fire("SpawnReaderlessChild", &mut state));
        if revoked_action == "ChildSendsMismatchedProof" {
            assert!(model.fire(revoked_action, &mut state));
        } else {
            assert!(model.fire("ChildPaintsExactProof", &mut state));
            assert!(model.fire(revoked_action, &mut state), "{name}: {state:?}");
        }
        assert!(
            model.successors("MainWinsCommitArbiter", &state).is_empty(),
            "model still authorized Commit without {name}: {state:?}"
        );
    }

    let model = aterm_models_misc::native_update_overlap_handoff_model();
    let mut exact = model.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "ChildPaintsExactProof",
        "MainWinsCommitArbiter",
    ] {
        assert!(model.fire(action, &mut exact), "{action}: {exact:?}");
    }
    assert_eq!(exact["arbiter"], 1);
}

#[test]
fn queued_output_is_buffered_mid_flight_but_peer_death_still_revokes() {
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    let live = [(1u64, master, 4242i32)];

    assert!(!handoff_masters_have_activity(&live), "quiet pty is quiet");
    assert!(!handoff_masters_closed(&live), "live peer is not dead");

    // SAFETY: bounded write of a stack byte to the test-owned slave.
    assert_eq!(
        unsafe { libc::write(slave, [0x62u8].as_ptr().cast(), 1) },
        1
    );
    assert!(
        handoff_masters_have_activity(&live),
        "pre-park admission still refuses to START over flowing output"
    );
    assert!(
        !handoff_masters_closed(&live),
        "mid-flight, queued output buffers through instead of revoking"
    );

    aterm_pty::set_nonblocking(slave, true).expect("nonblocking slave");
    aterm_pty::close_fd(slave);
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !handoff_masters_closed(&live) {
        assert!(
            std::time::Instant::now() < deadline,
            "peer death must still revoke — the live-set identity is stale"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    aterm_pty::close_fd(master);
}

#[test]
fn a_hung_up_master_is_never_output_waiting() {
    let open = || aterm_pty::open_pty_pair().expect("open_pty_pair");
    let (exited, exited_slave) = open();
    let (alive, alive_slave) = open();
    // SAFETY: bounded write of a stack byte to the test-owned slave.
    assert_eq!(
        unsafe { libc::write(exited_slave, [0x62u8].as_ptr().cast(), 1) },
        1
    );
    aterm_pty::set_nonblocking(exited_slave, true).expect("nonblocking slave");
    aterm_pty::close_fd(exited_slave);
    let dead = [(1u64, exited, 4242i32)];
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !handoff_masters_closed(&dead) {
        assert!(std::time::Instant::now() < deadline, "the slave hung up");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        !handoff_masters_have_activity(&dead),
        "an exited pane is not output waiting"
    );
    let desk = [(1u64, exited, 4242i32), (2u64, alive, 4243i32)];
    assert!(
        !handoff_masters_have_activity(&desk),
        "a dead pane beside a quiet live one leaves the desk quiet"
    );
    // SAFETY: bounded write of a stack byte to the test-owned slave.
    assert_eq!(
        unsafe { libc::write(alive_slave, [0x62u8].as_ptr().cast(), 1) },
        1
    );
    assert!(
        handoff_masters_have_activity(&desk),
        "a live session's unread output is still output"
    );
    aterm_pty::close_fd(alive);
    aterm_pty::close_fd(alive_slave);
    aterm_pty::close_fd(exited);
}

#[test]
fn ready_wait_types_a_handed_session_s_death_as_activity() {
    let expected = crate::seamless::adoption_proof(
        "ready-wait-session-death",
        2,
        "abcdef0",
        &[0x11; 32],
        &[0x22; 32],
        &[],
    )
    .expect("bounded proof fixture");
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    aterm_pty::close_fd(slave);
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !handoff_masters_closed(&[(1, master, 4242)]) {
        assert!(std::time::Instant::now() < deadline, "the hang-up shows");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let (proof_rd, _proof_wr) = make_cloexec_pipe().expect("proof pipe");
    let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
    assert_eq!(
        wait_handoff_ready(
            &proof_rd,
            expected,
            &cancel_rx,
            &[master],
            handoff_ready_deadline(),
            &mut || false,
            #[cfg(target_os = "macos")]
            None,
        ),
        crate::UpdateHandoffOutcome::ActivityRevoked,
        "a session's death is the desk changing"
    );
    aterm_pty::close_fd(master);
}

#[test]
fn ready_wait_tolerates_queued_output_and_types_cancel_as_activity() {
    let expected = crate::seamless::adoption_proof(
        "ready-wait-buffered-output",
        2,
        "abcdef0",
        &[0x11; 32],
        &[0x22; 32],
        &[],
    )
    .expect("bounded proof fixture");

    let mut master = [0i32; 2];
    // SAFETY: plain pipe(2) into a valid 2-slot out-array.
    assert_eq!(unsafe { libc::pipe(master.as_mut_ptr()) }, 0, "master pipe");
    let (master_rd, master_wr) = (master[0], master[1]);
    // SAFETY: bounded write of a stack byte to the test-owned pipe.
    assert_eq!(
        unsafe { libc::write(master_wr, [0x62u8].as_ptr().cast(), 1) },
        1
    );

    let (proof_rd, proof_wr) = make_cloexec_pipe().expect("proof pipe");
    let wire = expected.to_wire();
    // SAFETY: bounded write of the fixed proof wire to the test-owned pipe.
    let wrote = unsafe {
        use std::os::fd::AsRawFd as _;
        libc::write(proof_wr.as_raw_fd(), wire.as_ptr().cast(), wire.len())
    };
    assert_eq!(wrote as usize, wire.len(), "one complete proof wire");
    let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
    assert_eq!(
        wait_handoff_ready(
            &proof_rd,
            expected,
            &cancel_rx,
            &[master_rd],
            handoff_ready_deadline(),
            &mut || false,
            #[cfg(target_os = "macos")]
            None,
        ),
        crate::UpdateHandoffOutcome::ProofReady,
        "queued shell output must not abort the ready wait"
    );

    let (proof_rd, _proof_wr) = make_cloexec_pipe().expect("second proof pipe");
    let (cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
    cancel_tx.try_send(()).expect("queue cancel poke");
    assert_eq!(
        wait_handoff_ready(
            &proof_rd,
            expected,
            &cancel_rx,
            &[master_rd],
            handoff_ready_deadline(),
            &mut || false,
            #[cfg(target_os = "macos")]
            None,
        ),
        crate::UpdateHandoffOutcome::ActivityRevoked,
        "cancel must carry the typed activity classification"
    );

    aterm_pty::close_fd(master_rd);
    aterm_pty::close_fd(master_wr);
}

#[test]
fn ready_poll_classifies_queued_master_output_as_a_yield() {
    let pfd = |revents: libc::c_short| libc::pollfd {
        fd: -1,
        events: libc::POLLIN,
        revents,
    };
    assert_eq!(
        classify_ready_poll(&[pfd(0), pfd(libc::POLLIN)]),
        ReadyPollAction::NoProgress,
        "queued master output must yield, never busy-spin"
    );
    assert_eq!(
        classify_ready_poll(&[pfd(0), pfd(0)]),
        ReadyPollAction::NoProgress
    );
    assert_eq!(
        classify_ready_poll(&[pfd(0), pfd(libc::POLLIN | libc::POLLHUP)]),
        ReadyPollAction::SessionDied,
        "a stale live-set identity must reject, not read the proof"
    );
    assert_eq!(
        classify_ready_poll(&[pfd(libc::POLLIN), pfd(libc::POLLERR)]),
        ReadyPollAction::SessionDied,
        "master death outranks a readable proof"
    );
    assert_eq!(
        classify_ready_poll(&[pfd(libc::POLLIN), pfd(libc::POLLIN)]),
        ReadyPollAction::ReadProof
    );
    assert_eq!(
        classify_ready_poll(&[pfd(libc::POLLHUP), pfd(libc::POLLIN)]),
        ReadyPollAction::ReadProof,
        "proof EOF (its write end dropped) is still a read, detecting ChildDied"
    );
}

#[test]
fn full_reject_channel_remains_worker_owned_but_disconnect_does_not() {
    let (full_sender, full_receiver) = std::sync::mpsc::sync_channel(1);
    full_sender.try_send(()).expect("fill rejection slot");
    assert_eq!(
        deliver_handoff_rejection(Some(full_sender)),
        HandoffRejectDelivery::WorkerOwned,
        "Full means a rejection is already queued for the live worker"
    );
    assert_eq!(full_receiver.try_recv(), Ok(()));

    let (disconnected_sender, disconnected_receiver) = std::sync::mpsc::sync_channel(1);
    drop(disconnected_receiver);
    assert_eq!(
        deliver_handoff_rejection(Some(disconnected_sender)),
        HandoffRejectDelivery::Disconnected,
        "only receiver loss transfers authority to an emergency reaper"
    );
    assert_eq!(
        deliver_handoff_rejection(None),
        HandoffRejectDelivery::Disconnected
    );
}

#[test]
fn cancellation_kills_descendant_process_group_before_returning() {
    let mut command = std::process::Command::new("/bin/sh");
    command
        .inherit_nothing()
        .arg("-c")
        .arg(format!("{} & printf '%s\\n' \"$!\"; wait", parked()))
        .stdout(std::process::Stdio::piped());
    // SAFETY: async-signal-safe setpgid only, executed after fork/before exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let mut child = command.spawn().expect("spawn process-group leader");
    let leader = i32::try_from(child.id()).expect("bounded child pid");
    let mut cleanup = ProcessGroupCleanup::new(leader);
    let mut descendant_line = String::new();
    std::io::BufReader::new(child.stdout.take().expect("child stdout"))
        .read_line(&mut descendant_line)
        .expect("descendant pid line");
    let descendant: i32 = descendant_line
        .trim()
        .parse()
        .expect("numeric descendant pid");
    assert_eq!(unsafe { libc::kill(descendant, 0) }, 0, "descendant live");

    let candidate = HandoffCandidate::of_unreaped_child(&child);
    let mut handle = HandoffCandidateHandle::Forked(child);
    assert_eq!(
        killed_and_reaped(candidate, &mut handle),
        HandoffRollbackWarrant::Reaped,
        "our own fork child must be licensed by the strongest authority"
    );
    cleanup.disarm();
    assert_process_group_gone(
        leader,
        descendant,
        "kill/reap returned but a descendant process-group member survived",
    );
}

#[test]
fn pre_ready_death_preserves_leader_identity_until_normal_group_reap() {
    let mut command = std::process::Command::new("/bin/sh");
    command
        .inherit_nothing()
        .arg("-c")
        .arg(format!(
            "{} >/dev/null 2>&1 & printf '%s\\n' \"$!\"; exit 0",
            parked()
        ))
        .stdout(std::process::Stdio::piped());
    // SAFETY: async-signal-safe setpgid only, matching production.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let mut child = command.spawn().expect("spawn exiting group leader");
    let leader = i32::try_from(child.id()).expect("bounded child pid");
    let mut cleanup = ProcessGroupCleanup::new(leader);
    let mut stdout = std::io::BufReader::new(child.stdout.take().expect("child stdout"));
    let mut descendant_line = String::new();
    stdout
        .read_line(&mut descendant_line)
        .expect("descendant pid line");
    let descendant: i32 = descendant_line
        .trim()
        .parse()
        .expect("numeric descendant pid");
    let mut eof = Vec::new();
    stdout.read_to_end(&mut eof).expect("leader stdout EOF");
    assert_eq!(unsafe { libc::kill(descendant, 0) }, 0, "descendant live");

    let model = aterm_models_misc::native_update_overlap_handoff_model();
    let mut model_state = model.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "SpawnProcessGroupDescendant",
        "LeaderDiesLeavingLiveDescendant",
    ] {
        assert!(
            model.fire(action, &mut model_state),
            "{action}: {model_state:?}"
        );
    }

    let (proof_rd, _stray_proof_wr) = make_cloexec_pipe().expect("proof pipe");
    let candidate = HandoffCandidate::of_unreaped_child(&child);
    let mut handle = HandoffCandidateHandle::Forked(child);
    let expected = crate::seamless::adoption_proof(
        "pre-ready-eof-test",
        2,
        "abcdef0",
        &[0x11; 32],
        &[0x22; 32],
        &[],
    )
    .expect("bounded proof fixture");
    let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
    assert_eq!(
        wait_handoff_ready(
            &proof_rd,
            expected,
            &cancel_rx,
            &[],
            handoff_ready_deadline(),
            &mut || handle.candidate_gone(candidate),
            #[cfg(target_os = "macos")]
            None,
        ),
        crate::UpdateHandoffOutcome::ChildDied,
        "the leader's own end is seen without reaping it"
    );

    let arbiter = crate::HandoffAttemptArbiter::new();
    assert!(worker_claim_handoff_reaper(&arbiter));
    assert!(model.fire("WorkerWinsRejectArbiter", &mut model_state));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut old_order = model_state.clone();
    assert!(buggy.fire("BuggyWaitBeforeGroupSignal", &mut old_order));
    assert!(!buggy.check_invariant("ProcessGroupSignalPrecedesDirectChildReap", &old_order,));
    assert_eq!(old_order["descendant_live"], 1);
    let mut leader_only = model_state.clone();
    assert!(buggy.fire("BuggySignalLeaderOnly", &mut leader_only));
    assert_eq!(leader_only["group_signaled"], 1);
    assert!(!buggy.check_invariant("GroupSignalEliminatesLiveDescendants", &leader_only));

    assert_eq!(
        kill_and_reap_handoff_child(candidate, &mut handle).warrant,
        HandoffRollbackWarrant::Reaped,
        "an exited-but-unreaped fork child is still ours to wait"
    );
    assert!(model.fire("KillRejectedChild", &mut model_state));
    assert!(model.fire("ReapKilledChild", &mut model_state));
    assert!(arbiter.finish_reap(crate::HandoffReaperOwner::Worker));
    cleanup.disarm();
    assert_process_group_gone(
        leader,
        descendant,
        "pre-ready death path reaped leader but left its descendant live",
    );
    assert_eq!(model_state["group_signaled"], 1);
    assert_eq!(model_state["descendant_live"], 0);
    assert_eq!(model_state["child_reaped"], 1);
    assert_eq!(
        model
            .successors("ResumeParentAfterReap", &model_state)
            .len(),
        1,
        "rollback becomes enabled only after group signal + direct-child reap"
    );
}

/// LaunchServices' answer is read beside the proof wait.
#[cfg(target_os = "macos")]
#[test]
fn a_launchservices_pid_mismatch_after_transfer_is_rejected_and_reaped() {
    let mut command = std::process::Command::new("/bin/sh");
    command.inherit_nothing().arg("-c").arg("sleep 30");
    // SAFETY: async-signal-safe setpgid only, matching production.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let child = command.spawn().expect("spawn group leader");
    let leader = i32::try_from(child.id()).expect("bounded child pid");
    let mut cleanup = ProcessGroupCleanup::new(leader);

    let model = aterm_models_misc::native_update_overlap_handoff_model();
    let mut model_state = model.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "ChildSendsMismatchedProof",
    ] {
        assert!(
            model.fire(action, &mut model_state),
            "{action}: {model_state:?}"
        );
    }

    let (proof_rd, _proof_wr) = make_cloexec_pipe().expect("proof pipe");
    let expected = crate::seamless::adoption_proof(
        "corroboration-test",
        2,
        "abcdef0",
        &[0x11; 32],
        &[0x22; 32],
        &[],
    )
    .expect("bounded proof fixture");
    let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
    let stranger = leader.saturating_add(1);
    let in_flight = crate::app_launch_successor::LaunchInFlight::scripted(Some(stranger));
    let started = std::time::Instant::now();
    assert_eq!(
        wait_handoff_ready(
            &proof_rd,
            expected,
            &cancel_rx,
            &[],
            std::time::Instant::now() + std::time::Duration::from_secs(120),
            &mut || false,
            Some(super::LaunchCorroboration::of_dialer(
                &in_flight,
                HandoffCandidate::of_attested_peer(
                    u32::try_from(leader).expect("a live pid is positive")
                ),
            )),
        ),
        crate::UpdateHandoffOutcome::Rejected,
        "a launch answer naming another pid refuses the handoff"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "the refusal is immediate, not at the deadline: {:?}",
        started.elapsed()
    );

    let arbiter = crate::HandoffAttemptArbiter::new();
    assert!(worker_claim_handoff_reaper(&arbiter));
    assert!(model.fire("WorkerWinsRejectArbiter", &mut model_state));
    let candidate = HandoffCandidate::of_unreaped_child(&child);
    let mut handle = HandoffCandidateHandle::Forked(child);
    assert_eq!(
        killed_and_reaped(candidate, &mut handle),
        HandoffRollbackWarrant::Reaped
    );
    assert!(model.fire("KillRejectedChild", &mut model_state));
    assert!(model.fire("ReapKilledChild", &mut model_state));
    assert!(arbiter.finish_reap(crate::HandoffReaperOwner::Worker));
    cleanup.disarm();
    assert_eq!(
        unsafe { libc::kill(leader, 0) },
        -1,
        "the rejected candidate is dead"
    );
    assert_eq!(
        model
            .successors("ResumeParentAfterReap", &model_state)
            .len(),
        1,
        "rollback becomes enabled only after the kill and the reap"
    );
}

#[test]
fn a_candidate_that_ends_behind_a_stray_proof_writer_ends_the_ready_wait() {
    let expected = crate::seamless::adoption_proof(
        "stray-writer-test",
        2,
        "abcdef0",
        &[0x11; 32],
        &[0x22; 32],
        &[],
    )
    .expect("bounded proof fixture");
    let child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("exit 7")
        .spawn()
        .expect("spawn the candidate");
    let candidate = HandoffCandidate::of_unreaped_child(&child);
    let handle = HandoffCandidateHandle::Forked(child);
    let ended_by = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !handle.candidate_gone(candidate) {
        assert!(
            std::time::Instant::now() < ended_by,
            "the candidate exits on its own"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let (proof_rd, _stray_proof_wr) = make_cloexec_pipe().expect("proof pipe");
    let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);

    assert_eq!(
        wait_handoff_ready(
            &proof_rd,
            expected,
            &cancel_rx,
            &[],
            std::time::Instant::now() + std::time::Duration::from_millis(200),
            &mut || false,
            #[cfg(target_os = "macos")]
            None,
        ),
        crate::UpdateHandoffOutcome::TimedOut,
        "a stray writer hides the death from an EOF-only wait"
    );
    assert_eq!(
        wait_handoff_ready(
            &proof_rd,
            expected,
            &cancel_rx,
            &[],
            std::time::Instant::now() + std::time::Duration::from_secs(60),
            &mut || handle.candidate_gone(candidate),
            #[cfg(target_os = "macos")]
            None,
        ),
        crate::UpdateHandoffOutcome::ChildDied,
        "the candidate's own end ends the wait"
    );

    #[cfg_attr(
        not(target_os = "macos"),
        expect(
            clippy::infallible_destructuring_match,
            reason = "`Launched` is the macOS-only arm"
        )
    )]
    let mut child = match handle {
        HandoffCandidateHandle::Forked(child) => child,
        #[cfg(target_os = "macos")]
        HandoffCandidateHandle::Launched(_) => unreachable!("built as the fork lane above"),
    };
    assert_eq!(
        child.wait().expect("reap the candidate").code(),
        Some(7),
        "asking reaped nothing: the status is still the candidate's own"
    );
}

#[test]
fn a_live_candidate_is_not_gone() {
    let (candidate, mut handle, mut cleanup) = sleeping_candidate();
    assert!(!handle.candidate_gone(candidate), "alive right now");
    assert_eq!(
        killed_and_reaped(candidate, &mut handle),
        HandoffRollbackWarrant::Reaped
    );
    cleanup.disarm();
}

fn killed_and_reaped(
    candidate: HandoffCandidate,
    handle: &mut HandoffCandidateHandle,
) -> HandoffRollbackWarrant {
    use std::os::unix::process::ExitStatusExt as _;
    let reaped = kill_and_reap_handoff_child(candidate, handle);
    assert_eq!(
        reaped.status.and_then(|status| status.signal()),
        Some(libc::SIGKILL),
        "the reap path killed the candidate: {:?}",
        reaped.status
    );
    reaped.warrant
}

fn sleeping_candidate() -> (
    HandoffCandidate,
    HandoffCandidateHandle,
    ProcessGroupCleanup,
) {
    candidate_in_own_group("exec sleep 30")
}

fn candidate_in_own_group(
    script: &str,
) -> (
    HandoffCandidate,
    HandoffCandidateHandle,
    ProcessGroupCleanup,
) {
    let child = std::process::Command::new("/bin/sh")
        .inherit_nothing()
        .arg("-c")
        .arg(script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn the candidate");
    let cleanup = ProcessGroupCleanup::new(i32::try_from(child.id()).expect("bounded pid"));
    let candidate = HandoffCandidate::of_unreaped_child(&child);
    (candidate, HandoffCandidateHandle::Forked(child), cleanup)
}

#[test]
fn a_stopped_candidate_is_not_gone() {
    let (candidate, mut handle, mut cleanup) = sleeping_candidate();
    let pid = libc::pid_t::try_from(candidate.pid).expect("bounded pid");
    // The test stops it, so the stop waits on no run of the candidate's own (a script's
    // `kill -STOP $$` stops it only once it has been scheduled through its exec and its shell).
    // SAFETY: SIGSTOP to this test's own unreaped child.
    let sent = unsafe { libc::kill(pid, libc::SIGSTOP) };
    assert_eq!(sent, 0, "stop our own child");
    let id = libc::id_t::try_from(pid).expect("positive pid");
    let reported = |options: libc::c_int| {
        // SAFETY: zeroed siginfo_t for waitid with WNOWAIT, which leaves the child waitable.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::waitid(libc::P_PID, id, &mut info, options | libc::WNOWAIT) };
        assert_eq!(rc, 0, "our own child");
        info
    };
    let stopped_by = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while reported(libc::WSTOPPED | libc::WNOHANG).si_code != libc::CLD_STOPPED {
        if std::time::Instant::now() >= stopped_by {
            let ended = reported(libc::WEXITED | libc::WNOHANG);
            // SAFETY: a waitid report of a child fills the SIGCHLD arm.
            let status = unsafe { ended.si_status() };
            panic!(
                "the candidate stops (ended: code {} status {status})",
                ended.si_code
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(!handle.candidate_gone(candidate), "stopped is not gone");
    assert_eq!(
        killed_and_reaped(candidate, &mut handle),
        HandoffRollbackWarrant::Reaped
    );
    cleanup.disarm();
}

/// The matching answer, and no answer at all, change nothing: the wait runs to its real outcome.
#[cfg(target_os = "macos")]
#[test]
fn a_matching_or_pending_launch_answer_leaves_the_proof_wait_to_its_outcome() {
    let expected = crate::seamless::adoption_proof(
        "corroboration-test",
        2,
        "abcdef0",
        &[0x11; 32],
        &[0x22; 32],
        &[],
    )
    .expect("bounded proof fixture");
    for answer in [Some(4242), None] {
        let (proof_rd, proof_wr) = make_cloexec_pipe().expect("proof pipe");
        drop(proof_wr);
        let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
        let in_flight = crate::app_launch_successor::LaunchInFlight::scripted(answer);
        assert_eq!(
            wait_handoff_ready(
                &proof_rd,
                expected,
                &cancel_rx,
                &[],
                std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                &mut || false,
                Some(super::LaunchCorroboration::of_dialer(
                    &in_flight,
                    HandoffCandidate::from_bare_pid(4242),
                )),
            ),
            crate::UpdateHandoffOutcome::ChildDied,
            "answer {answer:?}: the corroboration neither rejects nor ends the wait"
        );
    }
}

#[test]
fn emergency_reaper_kills_group_even_after_leader_exits() {
    let mut command = std::process::Command::new("/bin/sh");
    command
        .inherit_nothing()
        .arg("-c")
        .arg(format!(
            "{} >/dev/null 2>&1 & printf '%s\\n' \"$!\"; exit 0",
            parked()
        ))
        .stdout(std::process::Stdio::piped());
    // SAFETY: async-signal-safe setpgid only, matching production.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let mut child = command.spawn().expect("spawn exiting group leader");
    let leader = i32::try_from(child.id()).expect("bounded child pid");
    let mut cleanup = ProcessGroupCleanup::new(leader);
    let mut stdout = std::io::BufReader::new(child.stdout.take().expect("child stdout"));
    let mut descendant_line = String::new();
    stdout
        .read_line(&mut descendant_line)
        .expect("descendant pid line");
    let descendant: i32 = descendant_line
        .trim()
        .parse()
        .expect("numeric descendant pid");
    let mut eof = Vec::new();
    stdout.read_to_end(&mut eof).expect("leader stdout EOF");
    assert_eq!(unsafe { libc::kill(descendant, 0) }, 0, "descendant live");

    let arbiter = crate::HandoffAttemptArbiter::new();
    assert!(arbiter.try_begin_reject());
    assert!(arbiter.claim_reaper(crate::HandoffReaperOwner::Emergency));
    assert_eq!(
        emergency_kill_and_reap_handoff_child(child.id()),
        HandoffRollbackWarrant::Reaped,
        "the emergency reaper still prefers `waitpid` when the candidate is ours"
    );
    let _ = child.wait();
    assert!(arbiter.finish_reap(crate::HandoffReaperOwner::Emergency));
    cleanup.disarm();
    assert_process_group_gone(
        leader,
        descendant,
        "emergency reap returned but exited leader left a live descendant",
    );
}

#[test]
fn a_running_candidate_is_unproven_and_a_vacant_pid_is_the_proof() {
    let mut child = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg("sleep 30")
        .spawn()
        .expect("spawn a live candidate");
    let candidate = HandoffCandidate::of_unreaped_spawned(&child);
    assert!(
        !handoff_candidate_terminated(candidate),
        "a running candidate must never license a reader resume"
    );

    let pid = i32::try_from(child.id()).expect("bounded child pid");
    // SAFETY: SIGKILL to the test's own child.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0, "kill fixture");
    child.wait().expect("reap the test candidate");
    assert!(
        handoff_candidate_terminated(candidate),
        "a candidate whose pid is vacant has terminated, whoever reaped it"
    );
}

/// A candidate this process did not fork (the LaunchServices shape) is proven terminated anyway.
#[cfg(target_os = "macos")]
#[test]
fn a_candidate_we_never_forked_is_still_proven_terminated() {
    let mut parent = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg(format!(
            "{} >/dev/null 2>&1 & printf '%s\\n' \"$!\"; exit 0",
            parked()
        ))
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("spawn the orphan's parent");
    let mut stdout = std::io::BufReader::new(parent.stdout.take().expect("parent stdout"));
    let mut orphan_line = String::new();
    stdout.read_line(&mut orphan_line).expect("orphan pid line");
    let orphan: u32 = orphan_line.trim().parse().expect("numeric orphan pid");
    parent.wait().expect("reap the orphan's parent");
    let raw = i32::try_from(orphan).expect("bounded orphan pid");

    let candidate = HandoffCandidate {
        pid: orphan,
        birth: super::read_candidate_birth(orphan),
    };
    assert!(
        candidate.birth.is_some(),
        "a live process has a kernel birth record"
    );
    let mut status = 0i32;
    // SAFETY: a non-blocking wait for a pid that is not our child.
    let waited = unsafe { libc::waitpid(raw, &mut status, libc::WNOHANG) };
    let errno = std::io::Error::last_os_error().raw_os_error();
    assert_eq!(waited, -1, "the orphan must not be waitable by us");
    assert_eq!(errno, Some(libc::ECHILD), "…and the refusal is ECHILD");
    assert!(
        !handoff_candidate_terminated(candidate),
        "the orphan is still running"
    );

    super::signal_handoff_candidate(candidate);
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !handoff_candidate_terminated(candidate) {
        if std::time::Instant::now() >= deadline {
            // SAFETY: SIGKILL to the fixture's own orphan.
            unsafe { libc::kill(raw, libc::SIGKILL) };
            panic!("the outside proof never established that orphan {orphan} terminated");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// A pid whose birth stamp disagrees with the candidate's was recycled.
#[cfg(target_os = "macos")]
#[test]
fn a_pid_whose_birth_record_disagrees_is_proof_of_death_and_is_never_signalled() {
    use std::os::unix::process::ExitStatusExt as _;
    let mut bystander = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg("sleep 30")
        .spawn()
        .expect("spawn a stand-in for whoever recycled the pid");
    let candidate = HandoffCandidate {
        pid: bystander.id(),
        birth: Some(super::HandoffCandidateBirth {
            seconds: 1,
            microseconds: 1,
        }),
    };
    assert!(
        handoff_candidate_terminated(candidate),
        "a disagreeing birth stamp proves the pid was reallocated"
    );

    super::signal_handoff_candidate(candidate);
    let pid = i32::try_from(bystander.id()).expect("bounded child pid");
    // SAFETY: SIGTERM to the test's own unreaped child.
    assert_eq!(
        unsafe { libc::kill(pid, libc::SIGTERM) },
        0,
        "signal the fixture"
    );
    let status = bystander.wait().expect("reap the test child");
    assert_eq!(
        status.signal(),
        Some(libc::SIGTERM),
        "the process that recycled the pid must survive the reap path: {status:?}"
    );
}

/// The launched lane's group-sweep gate is a kernel read.
#[cfg(unix)]
#[test]
fn the_group_sweep_gate_reads_whether_the_candidate_leads_its_own_group() {
    let spawn = |own_group: bool| {
        let mut command = aterm_uds::spawnfd::command("/bin/sleep");
        command
            .arg("30")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null());
        if own_group {
            command.process_group(0);
        }
        command.spawn().expect("spawn a group fixture")
    };
    let mut leader = spawn(true);
    let mut member = spawn(false);
    let leader_pid = i32::try_from(leader.id()).expect("bounded pid");
    let member_pid = i32::try_from(member.id()).expect("bounded pid");

    assert!(
        super::candidate_leads_its_own_group(leader_pid),
        "a process that called setpgid(0, 0) leads group {leader_pid}"
    );
    assert!(
        !super::candidate_leads_its_own_group(member_pid),
        "a process that inherited our group leads no group numbered {member_pid}"
    );
    for refused in [-1, 0, 1] {
        assert!(
            !super::candidate_leads_its_own_group(refused),
            "pid {refused} is never a sweep target"
        );
    }

    leader.kill().expect("kill the leader fixture");
    member.kill().expect("kill the member fixture");
    leader.wait().expect("reap the leader fixture");
    member.wait().expect("reap the member fixture");
}

/// The corroborated arm's group sweep reaches the helpers of a fork-lane candidate whose leader
/// exited between the identity read and the sweep.
#[cfg(unix)]
#[test]
fn a_corroborated_candidate_whose_leader_already_exited_still_has_its_group_swept() {
    use std::io::BufRead as _;
    let helper_line = format!("{} </dev/null >/dev/null 2>&1 & echo $!", parked());
    let mut leader = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", &helper_line])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn a group leader with a helper");
    let leader_pid = i32::try_from(leader.id()).expect("bounded pid");
    let mut line = String::new();
    std::io::BufReader::new(leader.stdout.take().expect("leader stdout"))
        .read_line(&mut line)
        .expect("read the helper pid");
    let helper: libc::pid_t = line.trim().parse().expect("helper pid");
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while super::probe_handoff_candidate(leader_pid) != super::HandoffCandidateProbe::Exited {
        assert!(
            std::time::Instant::now() < deadline,
            "the leader never exited"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // SAFETY: getpgid is a side-effect-free getter.
    assert_eq!(
        unsafe { libc::getpgid(helper) },
        leader_pid,
        "the helper is in the leader's group"
    );

    super::kill_corroborated_candidate(leader_pid);

    let alive = |pid: libc::pid_t| {
        // SAFETY: signal 0 is kill(2)'s existence check; it delivers nothing.
        unsafe { libc::kill(pid, 0) == 0 }
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while alive(helper) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let survived = alive(helper);
    if survived {
        // SAFETY: SIGKILL to the fixture's own helper, just seen alive.
        unsafe { libc::kill(helper, libc::SIGKILL) };
    }
    leader.wait().expect("reap the leader fixture");
    assert!(
        !survived,
        "the group sweep must reach the helper of an exited, unreaped fork-lane leader"
    );
}

fn forked_child_contains_itself(become_session_leader: bool) -> bool {
    // SAFETY: fork from a multi-threaded harness; child runs async-signal-safe calls only.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed");
    if pid == 0 {
        if become_session_leader {
            // SAFETY: async-signal-safe setsid in a fresh child pid.
            unsafe { libc::setsid() };
        }
        let contained = contain_own_process_group() == ProcessGroupContainment::OwnGroupLeader;
        // SAFETY: async-signal-safe; targets the calling process only.
        let refused = unsafe { libc::setpgid(0, 0) } != 0;
        let as_expected = contained && refused == become_session_leader;
        // SAFETY: async-signal-safe process exit without unwinding or atexit handlers.
        unsafe { libc::_exit(i32::from(!as_expected)) }
    }
    let mut status = 0i32;
    loop {
        // SAFETY: blocking wait for one exact child pid into a local status slot.
        let waited = unsafe { libc::waitpid(pid, &mut status, 0) };
        if waited == pid {
            break;
        }
        assert!(
            waited < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted,
            "waitpid refused to answer for the forked child"
        );
    }
    libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
}

#[test]
fn a_successor_started_in_a_foreign_group_contains_itself() {
    assert!(
        forked_child_contains_itself(false),
        "setpgid(0, 0) must have moved the child into a group of its own, and \
         a repeat call must be ACCEPTED because it is not a session leader"
    );
}

#[test]
fn a_session_leader_is_already_contained_when_the_call_refuses_it() {
    assert!(
        forked_child_contains_itself(true),
        "a session leader must both REFUSE the repeat setpgid and still lead \
         its own process group"
    );
}
