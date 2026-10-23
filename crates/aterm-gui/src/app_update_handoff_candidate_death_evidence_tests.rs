// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    HandoffCandidate, HandoffCandidateHandle, HandoffCandidateProbe, handoff_child_death,
    observe_candidate_death, probe_handoff_candidate,
};
use crate::ChildDeathEvidence as Death;
use std::os::unix::process::ExitStatusExt as _;

fn signalled(signal: i32) -> std::process::ExitStatus {
    std::process::ExitStatus::from_raw(signal)
}

fn wait_for_probe(pid: libc::pid_t, want: HandoffCandidateProbe) {
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while probe_handoff_candidate(pid) != want {
        assert!(
            std::time::Instant::now() < deadline,
            "the kernel must reach {want:?} for {pid}"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// `probe_handoff_candidate` distinguishes a running child, an exited child (`WNOWAIT`), and a non-child.
#[test]
fn the_pre_kill_probe_separates_a_running_candidate_from_a_dead_one() {
    let mut alive = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn a live candidate");
    let alive_pid = libc::pid_t::try_from(alive.id()).expect("pid fits");
    assert_eq!(
        probe_handoff_candidate(alive_pid),
        HandoffCandidateProbe::Running,
        "a child that has not exited has nothing to report yet"
    );

    let mut dead = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg("exit 3")
        .spawn()
        .expect("spawn a candidate that exits at once");
    let dead_pid = libc::pid_t::try_from(dead.id()).expect("pid fits");
    wait_for_probe(dead_pid, HandoffCandidateProbe::Exited);

    assert_eq!(
        probe_handoff_candidate(1),
        HandoffCandidateProbe::NotOurChild,
        "launchd is nobody's child of ours, and ECHILD is evidence of nothing"
    );

    assert_eq!(
        dead.wait().expect("the probe consumed nothing").code(),
        Some(3),
        "the probe must leave the candidate's own status intact"
    );
    alive.kill().expect("kill the live fixture");
    alive.wait().expect("reap the live fixture");
}

/// Only a bare `SIGKILL` requires `died_before_we_signalled`; exit codes and other signals are always the candidate's own.
#[test]
fn only_a_bare_sigkill_needs_proof_that_the_candidate_died_first() {
    assert_eq!(
        handoff_child_death(false, Some(signalled(libc::SIGKILL))),
        Death::Unobserved,
        "the candidate was still alive when the channel closed, so this \
         SIGKILL can only be ours"
    );
    assert_eq!(
        handoff_child_death(true, Some(signalled(libc::SIGKILL))),
        Death::Signalled {
            signal: libc::SIGKILL
        },
        "…and once the candidate is proven to have died first, the same \
         status IS its own death — the field case"
    );
    assert_eq!(
        handoff_child_death(false, Some(std::process::ExitStatus::from_raw(0))),
        Death::Exited { code: 0 },
        "SIGKILL is uncatchable and never yields WIFEXITED, so an exit code \
         cannot be ours however the race went — and `0` is the refusal"
    );
    assert_eq!(
        handoff_child_death(false, Some(signalled(libc::SIGSEGV))),
        Death::Signalled {
            signal: libc::SIGSEGV
        },
        "nothing in this file sends SIGSEGV either: a fault is always the \
         image's own"
    );
    assert_eq!(
        handoff_child_death(true, None),
        Death::Unobserved,
        "no status is no evidence, however certain the death"
    );
}

/// A candidate that exits on its own before the reject path runs is classified as `Death::Exited`.
#[test]
fn a_candidate_that_exits_on_its_own_is_read_as_its_own_exit_not_our_kill() {
    let child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .expect("spawn a refusing candidate");
    let candidate = HandoffCandidate::of_unreaped_child(&child);
    let pid = libc::pid_t::try_from(candidate.pid).expect("pid fits");
    let mut handle = HandoffCandidateHandle::Forked(child);
    wait_for_probe(pid, HandoffCandidateProbe::Exited);

    let (warrant, death) = observe_candidate_death(
        crate::UpdateHandoffOutcome::ChildDied,
        candidate,
        &mut handle,
    );
    assert_eq!(
        warrant,
        super::HandoffRollbackWarrant::Reaped,
        "our own fork child is still reaped by the strongest authority"
    );
    assert_eq!(
        death,
        Death::Exited { code: 0 },
        "THE REFUSAL: the candidate reached an `exit` instruction before this \
         process signalled anything, and the SIGKILL sent afterwards must not \
         overwrite that"
    );
}

/// A candidate killed with `SIGKILL` before the reject path runs is classified as `Death::Signalled { signal: SIGKILL }`.
#[test]
fn a_candidate_the_machine_killed_is_read_as_the_machine_s_kill() {
    let child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn a candidate to starve");
    let candidate = HandoffCandidate::of_unreaped_child(&child);
    let pid = libc::pid_t::try_from(candidate.pid).expect("pid fits");
    let mut handle = HandoffCandidateHandle::Forked(child);
    // SAFETY: `pid` names our own unreaped child, so the number is pinned.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    wait_for_probe(pid, HandoffCandidateProbe::Exited);

    let (_, death) = observe_candidate_death(
        crate::UpdateHandoffOutcome::ChildDied,
        candidate,
        &mut handle,
    );
    assert_eq!(
        death,
        Death::Signalled {
            signal: libc::SIGKILL
        },
        "THE FIELD CASE: the candidate was already dead when the reject path \
         looked, so the SIGKILL in its status is the machine's and the lane \
         must retry"
    );
}

/// A live candidate killed by the reject path is classified as `Death::Unobserved`.
#[test]
fn a_kill_this_process_sends_is_never_read_as_the_candidate_s_own_death() {
    let child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn a live candidate");
    let candidate = HandoffCandidate::of_unreaped_child(&child);
    let mut handle = HandoffCandidateHandle::Forked(child);

    let (_, death) = observe_candidate_death(
        crate::UpdateHandoffOutcome::ChildDied,
        candidate,
        &mut handle,
    );
    assert_eq!(
        death,
        Death::Unobserved,
        "it was alive when we looked, so the SIGKILL we then sent is ours and \
         claims nothing"
    );
}

/// Any outcome other than `ChildDied` reaps the candidate and returns `Death::Unobserved`.
#[test]
fn a_non_child_died_outcome_gathers_no_evidence_at_all() {
    let child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("exit 74")
        .spawn()
        .expect("spawn a candidate that exits at once");
    let candidate = HandoffCandidate::of_unreaped_child(&child);
    let pid = libc::pid_t::try_from(candidate.pid).expect("pid fits");
    let mut handle = HandoffCandidateHandle::Forked(child);
    wait_for_probe(pid, HandoffCandidateProbe::Exited);

    let (warrant, death) = observe_candidate_death(
        crate::UpdateHandoffOutcome::TimedOut,
        candidate,
        &mut handle,
    );
    assert_eq!(
        warrant,
        super::HandoffRollbackWarrant::Reaped,
        "the reap is unconditional; only the evidence is not"
    );
    assert_eq!(
        death,
        Death::Unobserved,
        "a candidate WE ended has no death of its own to describe, whatever \
         its status happens to say"
    );
}
