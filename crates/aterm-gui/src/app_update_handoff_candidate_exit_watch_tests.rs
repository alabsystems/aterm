// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::CandidateExitWatch;

/// The orphan's gate: the write end of a pipe the orphan blocks on before its script, so a
/// caller can attach its [`CandidateExitWatch`] to a process provably still there (a timed
/// sleep would race launchd's reap under load).
///
/// On Darwin [`make_cloexec_pipe`](super::make_cloexec_pipe) sets CLOEXEC after `pipe()`, so
/// a sibling test's fork can carry a copy of either end; hence [`OrphanGate::release`] writes
/// a newline rather than closing to EOF. The orphan reads descriptor 3 (`read -r go <&3`),
/// which bash, dash and zsh leave alone, because a non-interactive shell points a background
/// job's stdin at `/dev/null`.
struct OrphanGate(std::os::fd::OwnedFd);

impl OrphanGate {
    /// Lets the orphan run its script. One newline satisfies `read` whether or
    /// not another holder of the read end is alive; a broken pipe means the
    /// orphan is already gone.
    fn release(self) {
        use std::io::Write as _;
        let mut writer = std::fs::File::from(self.0);
        let _ = writer.write_all(b"\n");
    }
}

/// Makes a process that is not our child: fork a middle process that forks the
/// orphan, then reap the middle so the orphan reparents to launchd. Returns the
/// orphan's pid (`waitid` answers `ECHILD` about it) and the [`OrphanGate`]
/// holding it before `script`.
fn spawn_orphan(script: &str) -> (u32, OrphanGate) {
    let (gate_read, gate_write) = super::make_cloexec_pipe().expect("a pipe for the orphan's gate");
    // The orphan's standard streams go to `/dev/null`, not this pipe: an
    // inherited write end would make `read_to_string` wait for the orphan to
    // exit. The gate's read end arrives as the middle's stdin and is moved to
    // descriptor 3 before the job is forked.
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

/// Waits for the watch to answer, or fails. Bounded here only; production reads
/// are single zero-timeout polls.
fn wait_for_status(watch: &CandidateExitWatch) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        if let Some(status) = watch.exit_status() {
            return status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "an exited process must report through EVFILT_PROC"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// The launched lane's liveness probe leaves the witness alone: the ready wait
/// asks `candidate_gone` every slice, and reading the kqueue would consume the one
/// exit event, turning every death on that lane into `Unobserved`.
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

/// A full `wait(2)` status for a process this one did not fork and may not reap,
/// distinguishing a candidate that chose to stop from one something else
/// stopped.
#[test]
fn a_non_parent_can_read_an_orphan_s_own_exit_status() {
    use std::os::unix::process::ExitStatusExt as _;

    let (refused, gate) = spawn_orphan("exit 7");
    // Attached while the orphan is provably alive: it cannot run `exit`
    // before the gate opens, and the gate opens after this returns.
    let watch = CandidateExitWatch::watch(refused)
        .expect("EVFILT_PROC attaches to a same-user process we may SIGKILL");
    gate.release();
    let status = wait_for_status(&watch);
    assert_eq!(
        status.code(),
        Some(7),
        "a clean exit reaches a watcher that is nobody's parent — which is \
         what lets the launched lane tell a refusal from a kill at all"
    );
    assert_eq!(
        watch.exit_status(),
        Some(status),
        "the knote is one-shot, so the watch must keep what it read: the hold \
         loop's liveness check is a read, and the stand-down after it must \
         still name the status"
    );

    let (starved, gate) = spawn_orphan("sleep 30");
    let watch = CandidateExitWatch::watch(starved)
        .expect("EVFILT_PROC attaches to a same-user process we may SIGKILL");
    gate.release();
    assert!(
        watch.exit_status().is_none(),
        "a LIVE candidate must report nothing: the pre-kill read is what \
         makes a SIGKILL attributable, so a false positive there would call \
         every rejection a machine kill"
    );
    let pid = libc::pid_t::try_from(starved).expect("pid fits");
    // SAFETY: `kill` against a positive pid we just created and still see.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    let status = wait_for_status(&watch);
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "and the machine's kill arrives with the same fidelity — this is the \
         field shape the launched lane could previously observe nothing about"
    );
}

/// The macOS lane end to end: a LaunchServices successor is launchd's child, so
/// `waitid` answers `ECHILD`. Drives the real [`super::observe_candidate_death`]
/// against an orphan ended by an external `SIGKILL` (the jetsam shape) and
/// asserts the death is read as the machine's and the automatic lane retries.
#[test]
fn a_starved_candidate_on_the_launched_lane_takes_the_retry_lane() {
    use super::{HandoffCandidate, HandoffCandidateHandle, observe_candidate_death};
    use crate::app_native::{HandoffFailureLane as Lane, PhysicalFailureShape as Shape};

    let (starved, gate) = spawn_orphan("sleep 30");
    // Registered while it is alive, exactly as the rendezvous accept does.
    let watch = CandidateExitWatch::watch(starved).expect("the candidate is alive");
    gate.release();
    let pid = libc::pid_t::try_from(starved).expect("pid fits");
    // Someone else's SIGKILL, indistinguishable from this lane's own rejection.
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

/// An exit not witnessed before the lane acted is recovered once the candidate is provably
/// gone: an `Unwitnessed` candidate (no birth stamp) is signalled nothing, the termination
/// proof waits it out, and the witness is read after. Sound for an exit code, since SIGKILL
/// never yields `WIFEXITED`.
#[test]
fn an_exit_the_parent_did_not_see_coming_is_still_recovered_after_the_wait() {
    use super::{HandoffCandidate, HandoffCandidateHandle, observe_candidate_death};

    let (refusing, gate) = spawn_orphan("exit 0");
    let watch = CandidateExitWatch::watch(refusing).expect("the candidate is alive");
    // A bare pid has no birth stamp, so nothing is signalled.
    let candidate = HandoffCandidate::from_bare_pid(refusing);
    let mut handle = HandoffCandidateHandle::Launched(Some(watch));
    // The gate is still shut, so the orphan cannot have reached its `exit`.
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

/// The failing direction costs only evidence: a pid naming nothing cannot be
/// attached to, and the answer is `None`, not an error.
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
