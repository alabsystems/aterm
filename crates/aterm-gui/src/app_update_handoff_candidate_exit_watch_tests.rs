// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::CandidateExitWatch;

struct OrphanGate(std::os::fd::OwnedFd);

impl OrphanGate {
    fn release(self) {
        use std::io::Write as _;
        let mut writer = std::fs::File::from(self.0);
        let _ = writer.write_all(b"\n");
    }
}

fn spawn_orphan(script: &str) -> (u32, OrphanGate) {
    let (gate_read, gate_write) = super::make_cloexec_pipe().expect("a pipe for the orphan's gate");
    let mut middle = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg(format!(
            "exec 3<&0 0</dev/null; {{ read -r go <&3; {script} ; }} >/dev/null 2>&1 & echo $!"
        ))
        .stdin(aterm_uds::spawnfd::Stdio::from(gate_read))
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("spawn the middle process");
    let mut pid = String::new();
    {
        use std::io::Read as _;
        middle
            .stdout
            .as_mut()
            .expect("piped")
            .read_to_string(&mut pid)
            .expect("the middle process reports the orphan's pid");
    }
    middle.wait().expect("reap the middle process");
    (pid.trim().parse().expect("a pid"), OrphanGate(gate_write))
}

/// Probing `candidate_gone` on the launched lane preserves the queued exit status.
#[test]
fn asking_whether_a_launched_candidate_is_gone_keeps_its_exit_status() {
    let (orphan, gate) = spawn_orphan("exit 7");
    let candidate = super::HandoffCandidate::of_attested_peer(orphan);
    let handle = super::HandoffCandidateHandle::Launched(Some(
        CandidateExitWatch::watch(orphan)
            .expect("EVFILT_PROC attaches to a same-user process we may SIGKILL"),
    ));
    assert!(
        !handle.candidate_gone(candidate),
        "held at its gate, the orphan is alive"
    );
    gate.release();
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !handle.candidate_gone(candidate) {
        assert!(
            std::time::Instant::now() < deadline,
            "launchd reaps an exited orphan, and its pid falls vacant"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(
        handle
            .witnessed_exit_status()
            .expect("the exit event is still queued after the probe")
            .code(),
        Some(7)
    );
}

/// An orphan candidate killed by `SIGKILL` on the launched lane is classified as retryable.
#[test]
fn a_starved_candidate_on_the_launched_lane_takes_the_retry_lane() {
    use super::{HandoffCandidate, HandoffCandidateHandle, observe_candidate_death};
    use crate::app_native::{HandoffFailureLane as Lane, PhysicalFailureShape as Shape};

    let (starved, gate) = spawn_orphan("sleep 30");
    let watch = CandidateExitWatch::watch(starved).expect("the candidate is alive");
    gate.release();
    let pid = libc::pid_t::try_from(starved).expect("pid fits");
    // SAFETY: `pid` is a positive pid we created and can still see.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    // SAFETY: signal 0 performs the existence check only.
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(std::time::Instant::now() < deadline, "the orphan must die");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    let candidate = HandoffCandidate::of_attested_peer(starved);
    let mut handle = HandoffCandidateHandle::Launched(Some(watch));
    let (_, death) = observe_candidate_death(
        crate::UpdateHandoffOutcome::ChildDied,
        candidate,
        &mut handle,
    );
    assert_eq!(
        death,
        crate::ChildDeathEvidence::Signalled {
            signal: libc::SIGKILL
        },
        "THE FIELD CASE ON THE SHIPPING LANE: a candidate this process never \
         forked, ended by the machine, and the parent can finally say so"
    );
    assert_eq!(
        Lane::classify(
            crate::native_updater_service::ApplyMode::AutomaticPastGrace,
            crate::UpdateHandoffOutcome::ChildDied,
            death,
            false
        ),
        Lane::Physical(Shape::Transient),
        "…and it must reach the RETRY lane. `Structural` here is the defect: \
         two of these converged the automatic lane on bytes that applied \
         perfectly once the machine stopped being busy"
    );
}

/// An unwitnessed exit is recovered from kqueue after termination is proven.
#[test]
fn an_exit_the_parent_did_not_see_coming_is_still_recovered_after_the_wait() {
    use super::{HandoffCandidate, HandoffCandidateHandle, observe_candidate_death};

    let (refusing, gate) = spawn_orphan("exit 0");
    let watch = CandidateExitWatch::watch(refusing).expect("the candidate is alive");
    let candidate = HandoffCandidate::from_bare_pid(refusing);
    let mut handle = HandoffCandidateHandle::Launched(Some(watch));
    assert!(
        handle.witnessed_exit_status().is_none(),
        "it is alive right now: the pre-kill read must claim nothing"
    );
    gate.release();
    let (_, death) = observe_candidate_death(
        crate::UpdateHandoffOutcome::ChildDied,
        candidate,
        &mut handle,
    );
    assert_eq!(
        death,
        crate::ChildDeathEvidence::Exited { code: 0 },
        "the knote XNU queued inside `proc_exit` is still there once \
         termination is proven, and an exit code can never be our SIGKILL"
    );
}

/// Watching a candidate that has already exited returns `None`.
#[test]
fn watching_a_candidate_that_is_already_gone_answers_none() {
    let (gone, gate) = spawn_orphan("exit 0");
    gate.release();
    let pid = libc::pid_t::try_from(gone).expect("pid fits");
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    // SAFETY: signal 0 performs the existence check only and delivers nothing.
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the orphan must be reaped by launchd"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(
        CandidateExitWatch::watch(gone).is_none(),
        "ESRCH is not an error here: it is the launched lane losing its \
         witness, which degrades to Unobserved and a bounded retry"
    );
    assert!(
        CandidateExitWatch::watch(1).is_none(),
        "and launchd itself is never a candidate"
    );
}
