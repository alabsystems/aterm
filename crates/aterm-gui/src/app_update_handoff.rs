// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Seamless update handoff: `apply_staged_update_now` and the unix overlap
//! worker it starts (`run_handoff_worker`, the bounded readiness wait, the
//! unique-reaper and rejection plumbing), the Commit-time admission facts, and
//! `finish_update_handoff`'s drain gate, Commit, and rollback.
//! A verbatim inherent-impl split of `App`.

use winit::event_loop::ActiveEventLoop;

use crate::App;
#[cfg(unix)]
use crate::Wake;
#[cfg(unix)]
use crate::app_input::paste_order;

#[cfg(unix)]
struct HandoffWorkerJob {
    attempt_id: u64,
    current_build: u64,
    target_build: u64,
    target_commit: String,
    /// Run the staged-candidate pre-verification (codesign + sealed rebinding)
    /// as the worker's first action, off the GUI main thread. False for the
    /// same-binary debug re-exec, which has no staged `.app` to authenticate.
    verify_staged_candidate: bool,
    command: std::process::Command,
    manifest: crate::session_store::SessionHandoff,
    fds: crate::session_store::HandoffFds,
    screens: Vec<(u64, aterm_core::terminal::TerminalCheckpoint)>,
    window: Option<crate::session_store::WindowCarry>,
    layout: crate::restore::RestoreManifest,
    layout_digest: [u8; 32],
    screen_digest: [u8; 32],
    live: Vec<(u64, i32, i32)>,
    cleanup: HandoffWorkerCleanup,
    cancel: std::sync::mpsc::Receiver<()>,
    arbiter: crate::HandoffAttemptArbiter,
    _owned_masters: Vec<std::os::fd::OwnedFd>,
}

#[cfg(unix)]
#[derive(Clone, Default)]
struct HandoffWorkerCleanup {
    parent_socket: Option<(std::path::PathBuf, String)>,
    reconcile: Option<(
        crate::app_native::NativeUpdateReconcileSender,
        crate::app_native::NativeUpdateReconcileTicket,
    )>,
}

/// Fresh mutable facts required immediately before the attempt-wide Commit CAS.
/// Keeping this conjunction pure gives the derived handoff model a shipping
/// decision seam; `ProofReady` alone never grants replacement authority.
///
/// SEAMLESS ADMISSION (deliberate 2026-07 semantics change): queued PTY OUTPUT
/// is no longer a fact here at all. The screen-carry digest is captured
/// post-park at parser ground and the parent provably consumes no further PTY
/// bytes, so bytes queued in the kernel replay through the child's fresh
/// parser after Commit — the carried checkpoint stays a valid ground-state
/// prefix and nothing is lost. What remains from the old `ptys_still_quiet`
/// conjunct is its fail-closed core, `sessions_alive`: POLLHUP/POLLERR on a
/// master means the shell (live-set identity) died, which must still reject.
/// Queued-but-undispatched HARDWARE input is likewise no longer an admission
/// fact — the completion path DRAINS it (re-posting itself so the run loop
/// dispatches the queued events into the still-open masters) rather than
/// revoking; see `finish_update_handoff`'s drain gate.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandoffCommitFacts {
    exact_sessions: bool,
    exact_layout: bool,
    exact_activity: bool,
    teardown_allows_commit: bool,
    parent_still_parked: bool,
    sessions_alive: bool,
    /// The OS input queue has been DISPATCHED into the masters: the main thread
    /// ran the event loop for a bounded interval after ProofReady, so every
    /// hardware event CoreGraphics had already accepted has flowed through the
    /// tolerated input path to the still-open PTY masters. This replaced "no
    /// hardware event happened in the last 50 ms", which was unsatisfiable on a
    /// machine in use AND never the property that mattered (see the drain gate
    /// in `finish_update_handoff`).
    input_dispatch_fenced: bool,
    /// PROCESS-LOCAL egress is drained to the kernel: no tolerated keystroke is
    /// still sitting in this process's paste-order FIFO or a wedged-tty sink
    /// spill. Distinct from `input_dispatch_fenced` (the OS/AppKit hardware
    /// queue) — that input, once dispatched, may land in THESE queues, and they
    /// die with `_exit` unless flushed to the master first. Below the spec
    /// model's abstraction (the model's single "deliver queued input to the
    /// masters" step), so it is asserted directly rather than model-fired.
    egress_settled: bool,
    native_safe: bool,
    proof_exact: bool,
    commit_channel: bool,
}

#[cfg(unix)]
#[must_use]
fn handoff_commit_admitted(facts: HandoffCommitFacts) -> bool {
    facts.exact_sessions
        && facts.exact_layout
        && facts.exact_activity
        && facts.teardown_allows_commit
        && facts.parent_still_parked
        && facts.sessions_alive
        && facts.input_dispatch_fenced
        && facts.egress_settled
        && facts.native_safe
        && facts.proof_exact
        && facts.commit_channel
}

/// The human-readable reason a ProofReady completion did not Commit, derived
/// from the SAME [`HandoffCommitFacts`] the admission read so the string
/// cascade and the admission can never drift. The two Commit-race flags arrive
/// separately because they are decided after admission; `native_safety` rides
/// along for its `Err` reasons (`facts.native_safe` is its projection).
#[cfg(unix)]
fn handoff_rejection_reason(
    facts: HandoffCommitFacts,
    native_safety: &Result<crate::app_native::NativeUpdateSafetyToken, Vec<String>>,
    commit_lost_arbiter: bool,
    commit_write_failed: bool,
) -> String {
    if !facts.exact_sessions {
        "live terminal set changed during async preparation".to_string()
    } else if !facts.exact_layout {
        "window/tab/pane topology changed during async preparation".to_string()
    } else if !facts.exact_activity {
        "structural activity arrived before Commit".to_string()
    } else if !facts.teardown_allows_commit {
        "destructive intent revoked Commit before teardown replay".to_string()
    } else if !facts.sessions_alive {
        "a handed-off PTY session closed before Commit".to_string()
    } else if !facts.input_dispatch_fenced {
        "the OS input queue did not dispatch into the masters before Commit".to_string()
    } else if !facts.egress_settled {
        "tolerated input outlasted the pre-Commit egress-flush budget".to_string()
    } else if !facts.parent_still_parked {
        "a parent PTY reader resumed before Commit".to_string()
    } else if let Err(reasons) = native_safety {
        format!(
            "native safety changed before Commit: {}",
            reasons.join(" · ")
        )
    } else if commit_lost_arbiter {
        "worker atomically revoked the handoff before Commit".to_string()
    } else if commit_write_failed {
        "attempt-bound Commit pipe closed before its atomic write".to_string()
    } else {
        "attempt-bound Commit could not be delivered atomically".to_string()
    }
}

/// TYPED RETRY CLASSIFICATION: whether a rejection was activity-shaped (the
/// terminal's world moved — sessions, layout, epoch, deferred teardown,
/// undrainable typing) versus genuine (safety/proof/channel/arbiter faults).
/// Activity rollback is lossless and repeatable, so automatic mode may spend
/// bounded retry budget on it; the worker's later `Rejected` completion reads
/// the flag this sets in `finish_update_handoff`'s non-ready arm. Never
/// decided by string matching — derived from the same facts as the admission.
///
/// SESSION DEATH IS NOT ACTIVITY (consistency with the worker): a handed-off
/// shell dying mid-overlap is a GENUINE failure — exactly as
/// `wait_handoff_ready` and the worker decision loop classify it (a plain
/// `Rejected` with no activity flag → manual-only). The adoption proof's
/// live-set identity is gone, and reclassifying that as retry-eligible only
/// here — because the main thread happened to observe the HUP first — would
/// spend the automatic budget on a handoff that can never re-prove the same
/// set. `sessions_alive` is therefore deliberately absent from this set.
#[cfg(unix)]
fn handoff_rejection_activity_shaped(facts: HandoffCommitFacts) -> bool {
    !facts.exact_sessions
        || !facts.exact_layout
        || !facts.exact_activity
        || !facts.teardown_allows_commit
        || !facts.input_dispatch_fenced
        || !facts.egress_settled
}

/// The emergency reaper's one job, shared verbatim by the spawned reaper
/// thread and the spawn-failed inline fallback: kill/reap the readerless
/// child, release the emergency reaper claim, run the worker cleanup, and
/// report the rejected completion back to the event loop.
#[cfg(unix)]
fn emergency_reap_and_report(
    child_pid: u32,
    attempt_id: u64,
    arbiter: &crate::HandoffAttemptArbiter,
    cleanup: &HandoffWorkerCleanup,
    nonce: Option<String>,
    detail: String,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
) {
    emergency_kill_and_reap_handoff_child(child_pid);
    let completed = arbiter.finish_reap(crate::HandoffReaperOwner::Emergency);
    debug_assert!(completed, "emergency reaper retained sole ownership");
    cleanup.complete(nonce.as_deref());
    let _ = proxy.send_event(Wake::UpdateHandoffFinished(
        crate::UpdateHandoffCompletion {
            attempt_id,
            nonce,
            child_pid: Some(child_pid),
            outcome: crate::UpdateHandoffOutcome::Rejected,
            commit_fd: None,
            reject: None,
            reconcile: None,
            detail,
            input_drain_spins: 0,
        },
    ));
}

#[cfg(unix)]
impl HandoffWorkerCleanup {
    /// Complete all filesystem repair before the UI is told rollback is safe.
    /// Thus the event-loop completion performs no directory scan, unlink,
    /// symlink publication, status read, or child-process probe.
    fn complete(&self, nonce: Option<&str>) {
        if let Some(nonce) = nonce {
            crate::seamless::discard_outgoing(nonce);
        }
        if let Some((latest_link, socket_path)) = &self.parent_socket {
            crate::control_auth::publish_latest_link(latest_link, socket_path);
        }
    }
}

#[cfg(unix)]
fn make_cloexec_pipe() -> Option<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    use std::os::fd::FromRawFd as _;
    let mut raw = [0i32; 2];
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let created = unsafe { libc::pipe2(raw.as_mut_ptr(), libc::O_CLOEXEC) };
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let created = unsafe { libc::pipe(raw.as_mut_ptr()) };
    // A real one-way pipe gives every fixed <=PIPE_BUF proof/Commit wire the
    // atomic all-or-nothing write property used by `commit_and_exit`. A byte
    // stream socketpair does not provide that theorem and may short-write.
    if created != 0 {
        return None;
    }
    // SAFETY: fresh pipe fds, exclusively owned from here.
    let rd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw[0]) };
    let wr = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw[1]) };
    use std::os::fd::AsRawFd as _;
    if aterm_pty::set_cloexec(rd.as_raw_fd(), true).is_err()
        || aterm_pty::set_cloexec(wr.as_raw_fd(), true).is_err()
    {
        return None;
    }
    Some((rd, wr))
}

#[cfg(unix)]
fn kill_and_reap_handoff_child(child: &mut std::process::Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        // The candidate is placed in its own process group in `pre_exec`.
        // Kill the whole group so ditto/codesign/spctl descendants cannot keep
        // mutating fixed updater paths after the direct child is reaped.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    // This runs only on the handoff worker. `wait` is load-bearing: no rollback
    // Wake is emitted until the group was killed and its direct child reaped.
    let _ = child.wait();
}

/// Acquire the worker's unique reaper capability.  Losing to `Committing`
/// means exactly what it says: this worker must neither signal nor reap the
/// candidate while the UI thread is performing the atomic Commit write.
#[cfg(unix)]
fn worker_claim_handoff_reaper(arbiter: &crate::HandoffAttemptArbiter) -> bool {
    loop {
        match arbiter.phase() {
            crate::HandoffAttemptPhase::Waiting => {
                if !arbiter.try_begin_reject() {
                    continue;
                }
            }
            crate::HandoffAttemptPhase::Rejecting => {}
            crate::HandoffAttemptPhase::Committing => return false,
        }
        return arbiter.claim_reaper(crate::HandoffReaperOwner::Worker);
    }
}

#[cfg(unix)]
fn emergency_kill_and_reap_handoff_child(pid: u32) {
    let Ok(pid) = i32::try_from(pid) else {
        return;
    };
    let mut status = 0i32;
    // PRECONDITION: the caller won the attempt-wide Emergency reaper CAS. That
    // is the unique capability proving `pid` is still this attempt's unreaped
    // direct child; no worker can concurrently consume/reuse its identity.
    // Signal the process group BEFORE any wait: `waitpid(WNOHANG)` also reaps an
    // already-dead leader, and the old ordering then returned while its
    // ditto/codesign/spctl descendants continued mutating fixed updater paths.
    unsafe { libc::kill(-pid, libc::SIGKILL) };
    loop {
        // SAFETY: wait for this process's exact child. Under the unique reaper
        // capability ECHILD can only mean the child was already consumed during
        // process teardown; either way the group signal happened first.
        let waited = unsafe { libc::waitpid(pid, &mut status, 0) };
        if waited == pid
            || (waited < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD))
        {
            return;
        }
        if waited < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return;
        }
    }
}

/// PRE-PARK admission peek only: any readable byte OR error/hangup counts.
/// Automatic mode refuses to even BEGIN an overlap while output is actively
/// flowing (the quiet-epoch policy). Once an attempt is in flight this
/// function must NOT be used — mid-flight, queued output is tolerated and
/// only session death revokes; use [`handoff_masters_closed`] there.
#[cfg(unix)]
fn handoff_masters_have_activity(live: &[(u64, i32, i32)]) -> bool {
    let mut fds = live
        .iter()
        .map(|(_, fd, _)| libc::pollfd {
            fd: *fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect::<Vec<_>>();
    if fds.is_empty() {
        return false;
    }
    // SAFETY: initialized stable pollfd slice; timeout 0 is a non-consuming peek.
    let polled = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 0) };
    let activity = libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
    polled > 0 && fds.iter().any(|fd| fd.revents & activity != 0)
}

/// MID-FLIGHT death peek: true only when a handed-off master reports
/// POLLHUP/POLLERR/POLLNVAL — the session (the live-set identity the adoption
/// proof committed to) is gone or the descriptor is invalid. Readable output
/// deliberately does NOT count: post-park bytes wait gap-free in the kernel
/// queue for the child's fresh parser, so output during the overlap is
/// buffered through, never revoking. `events: POLLIN` is load-bearing despite
/// POLLIN being ignored in the answer: macOS's poll(2) evaluates a PTY
/// master's stream state only for requested events and reports a dead slave
/// as `POLLIN|POLLHUP` — with `events: 0` it reports NOTHING, ever (verified
/// by the paired unit test). The filter to HUP/ERR/NVAL in `revents` is what
/// makes plain readable output invisible here.
#[cfg(unix)]
fn handoff_masters_closed(live: &[(u64, i32, i32)]) -> bool {
    let mut fds = live
        .iter()
        .map(|(_, fd, _)| libc::pollfd {
            fd: *fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect::<Vec<_>>();
    if fds.is_empty() {
        return false;
    }
    // SAFETY: initialized stable pollfd slice; timeout 0 is a non-consuming peek.
    let polled = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 0) };
    let dead = libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
    polled > 0 && fds.iter().any(|fd| fd.revents & dead != 0)
}

/// Whether every handed-off session's PROCESS-LOCAL egress has reached the
/// kernel — no keystroke tolerated into the overlap is still queued in this
/// process (the paste-order FIFO or a wedged-tty sink spill) where `_exit`
/// would destroy it. A master no longer in the pool is treated as settled: its
/// session is gone, `sessions_alive`/`exact_sessions` own that rejection, and
/// there is no sink left to flush. Cheap on the fast path (no paste in flight,
/// nothing spilled → one relaxed atomic load + one empty-buffer check).
#[cfg(unix)]
fn handoff_egress_settled(pool: &crate::SessionPool, live: &[(u64, i32, i32)]) -> bool {
    live.iter().all(|(_, master, _)| {
        !paste_order::is_ordering(*master)
            && pool
                .iter()
                .find(|session| session.master == *master)
                .is_none_or(|session| session.ctx.sink.egress_drained_to_kernel())
    })
}

fn bind_expected_update_artifact(
    command: &mut std::process::Command,
    attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
) {
    const BUILD: &str = "ATERM_UPDATE_EXPECTED_BUILD";
    const COMMIT: &str = "ATERM_UPDATE_EXPECTED_COMMIT";
    const DIGEST: &str = "ATERM_UPDATE_EXPECTED_DMG_SHA256";
    command
        .env_remove(BUILD)
        .env_remove(COMMIT)
        .env_remove(DIGEST);
    if let Some(attempt) = attempt {
        command
            .env(BUILD, attempt.target_build().to_string())
            .env(COMMIT, attempt.target_commit())
            .env(DIGEST, attempt.target_dmg_sha256());
    }
}

#[cfg(unix)]
impl crate::UpdateHandoffCompletion {
    fn failure(
        attempt_id: u64,
        nonce: Option<String>,
        child_pid: Option<u32>,
        outcome: crate::UpdateHandoffOutcome,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            attempt_id,
            nonce,
            child_pid,
            outcome,
            commit_fd: None,
            reject: None,
            reconcile: None,
            detail: detail.into(),
            input_drain_spins: 0,
        }
    }
}

#[cfg(unix)]
fn send_reaped_handoff_failure(
    cleanup: &HandoffWorkerCleanup,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    current_build: u64,
    completion: crate::UpdateHandoffCompletion,
) {
    cleanup.complete(completion.nonce.as_deref());
    // Reader rollback and reducer re-arm are latency-critical. Publish the
    // child-reaped fact before waiting behind the updater FIFO for disk facts.
    if proxy
        .send_event(Wake::UpdateHandoffFinished(completion))
        .is_err()
    {
        return;
    }
    if let Some(facts) = cleanup.reconcile.as_ref().and_then(|(worker, ticket)| {
        crate::app_native::collect_native_update_reconcile_facts(worker, *ticket, current_build)
    }) {
        let _ = proxy.send_event(Wake::NativeUpdateReconcileFinished {
            purpose: crate::app_native::NativeUpdateReconcilePurpose::Startup,
            facts,
        });
    }
}

/// Reject, kill, and reap on the worker only after winning the attempt-wide
/// arbiter. `false` means Commit won the race and the caller must keep the child
/// untouched while waiting for Commit success or its explicit failure transfer.
#[cfg(unix)]
fn worker_reject_and_reap_handoff_child(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    child: &mut std::process::Child,
    nonce: &str,
    child_pid: u32,
    outcome: crate::UpdateHandoffOutcome,
    detail: String,
) -> bool {
    if !worker_claim_handoff_reaper(&job.arbiter) {
        return false;
    }
    kill_and_reap_handoff_child(child);
    let completed = job.arbiter.finish_reap(crate::HandoffReaperOwner::Worker);
    debug_assert!(
        completed,
        "the worker must retain its unique reaper ownership"
    );
    send_reaped_handoff_failure(
        &job.cleanup,
        proxy,
        job.current_build,
        crate::UpdateHandoffCompletion::failure(
            job.attempt_id,
            Some(nonce.to_string()),
            Some(child_pid),
            outcome,
            detail,
        ),
    );
    true
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandoffRejectDelivery {
    /// `Ok` and `Full` both prove that the worker receiver still owns the
    /// rejection. A full one-slot channel is an already-queued command, not a
    /// disconnected worker and never authority for an emergency reaper.
    WorkerOwned,
    Disconnected,
}

#[cfg(unix)]
fn deliver_handoff_rejection(
    reject: Option<std::sync::mpsc::SyncSender<()>>,
) -> HandoffRejectDelivery {
    let Some(reject) = reject else {
        return HandoffRejectDelivery::Disconnected;
    };
    match reject.try_send(()) {
        Ok(()) | Err(std::sync::mpsc::TrySendError::Full(())) => HandoffRejectDelivery::WorkerOwned,
        Err(std::sync::mpsc::TrySendError::Disconnected(())) => HandoffRejectDelivery::Disconnected,
    }
}

#[cfg(unix)]
fn handoff_preparation_cancelled(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    nonce: Option<String>,
) -> bool {
    if job.cancel.try_recv().is_err() {
        return false;
    }
    send_reaped_handoff_failure(
        &job.cleanup,
        proxy,
        job.current_build,
        crate::UpdateHandoffCompletion::failure(
            job.attempt_id,
            nonce,
            None,
            // Typed activity classification: a cancel poke during preparation
            // is user/structural activity, never evidence against the staged
            // artifact — automatic mode may re-attempt at a later quiet window.
            crate::UpdateHandoffOutcome::ActivityRevoked,
            "activity revoked handoff during physical preparation",
        ),
    );
    true
}

#[cfg(unix)]
fn send_handoff_preparation_failure(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    nonce: Option<String>,
    detail: impl Into<String>,
) {
    send_reaped_handoff_failure(
        &job.cleanup,
        proxy,
        job.current_build,
        crate::UpdateHandoffCompletion::failure(
            job.attempt_id,
            nonce,
            None,
            crate::UpdateHandoffOutcome::PreparationFailed,
            detail,
        ),
    );
}

#[cfg(unix)]
fn run_handoff_worker(mut job: HandoffWorkerJob, proxy: winit::event_loop::EventLoopProxy<Wake>) {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::CommandExt as _;

    if handoff_preparation_cancelled(&job, &proxy, None) {
        return;
    }

    // STAGED-CANDIDATE PRE-VERIFICATION (seamless seam 1), off the GUI thread.
    // The `codesign --deep` + bundle-flock authenticity check that used to freeze
    // the main thread before every handoff runs HERE, as the worker's first real
    // action, so a doomed candidate is refused before any manifest is written or
    // any child is spawned — and the UI thread never blocks on it. Still strictly
    // additive: the child re-runs the complete gate under the apply lock at swap
    // time. A refusal is an ordinary `PreparationFailed` (manual-only).
    if job.verify_staged_candidate
        && let Err(error) = aterm_update::preverify_staged_for_handoff(
            job.current_build,
            Some(job.target_build),
            Some(&job.target_commit),
        )
    {
        send_handoff_preparation_failure(
            &job,
            &proxy,
            None,
            format!("staged update failed pre-park verification: {error}"),
        );
        return;
    }
    if handoff_preparation_cancelled(&job, &proxy, None) {
        return;
    }

    let layout_roundtrip = job
        .layout
        .to_toml()
        .ok()
        .and_then(|wire| crate::restore::RestoreManifest::from_toml(&wire));
    if job.layout.is_empty() || layout_roundtrip.as_ref() != Some(&job.layout) {
        send_handoff_preparation_failure(
            &job,
            &proxy,
            None,
            "could not persist the bounded handoff layout",
        );
        return;
    }
    if handoff_preparation_cancelled(&job, &proxy, None) {
        return;
    }
    let Some(outgoing) =
        crate::seamless::write_outgoing(&job.manifest, &job.fds, &job.screens, job.window)
    else {
        send_handoff_preparation_failure(
            &job,
            &proxy,
            None,
            "could not write the authenticated handoff manifest",
        );
        return;
    };
    let crate::seamless::OutgoingHandoff {
        manifest_path: path,
        nonce,
        fds_wire: wire,
        screen_digest: carried_screen_digest,
    } = outgoing;
    // BYTE-IDENTITY ASSERTION. `job.screen_digest` was committed on the main
    // thread from the live checkpoints; `carried_screen_digest` was taken over
    // the bytes just written. The child hashes those SAME bytes, so a divergence
    // here would surface later as an unexplained `AdoptionMismatch`. Catch it now
    // as an ordinary, explained preparation failure instead.
    if carried_screen_digest != job.screen_digest {
        send_handoff_preparation_failure(
            &job,
            &proxy,
            Some(nonce),
            "handoff screen carry did not match the committed screen digest",
        );
        return;
    }
    if handoff_preparation_cancelled(&job, &proxy, Some(nonce.clone())) {
        return;
    }
    let layout_path = std::path::Path::new(&path).with_extension("layout.toml");
    if crate::restore::write_to(&layout_path, &job.layout).is_err() {
        send_handoff_preparation_failure(
            &job,
            &proxy,
            Some(nonce),
            "could not write the attempt-bound handoff layout",
        );
        return;
    }
    if handoff_preparation_cancelled(&job, &proxy, Some(nonce.clone())) {
        return;
    }
    let Some(expected) = crate::seamless::adoption_proof(
        &nonce,
        job.target_build,
        &job.target_commit,
        &job.layout_digest,
        &job.screen_digest,
        &job.live,
    ) else {
        send_handoff_preparation_failure(
            &job,
            &proxy,
            Some(nonce),
            "handoff identity set exceeds the proof format",
        );
        return;
    };
    let Some((proof_rd, proof_wr)) = make_cloexec_pipe() else {
        send_handoff_preparation_failure(
            &job,
            &proxy,
            Some(nonce),
            "could not create the adoption-proof channel",
        );
        return;
    };
    let Some((commit_rd, commit_wr)) = make_cloexec_pipe() else {
        send_handoff_preparation_failure(
            &job,
            &proxy,
            Some(nonce),
            "could not create the handoff-commit channel",
        );
        return;
    };
    if handoff_preparation_cancelled(&job, &proxy, Some(nonce.clone())) {
        return;
    }

    job.command
        .env("ATERM_SEAMLESS_MANIFEST", path)
        .env("ATERM_SEAMLESS_NONCE", &nonce)
        .env("ATERM_SEAMLESS_FDS", wire)
        .env("ATERM_SEAMLESS_LAYOUT", layout_path)
        // The candidate proves "I am the build you authorized" by comparison,
        // and logs which half disagreed when it is not. Forging this can only
        // LOSE a handoff — the parent still compares against its own ticket.
        .env(
            "ATERM_SEAMLESS_TARGET",
            crate::seamless::encode_target_identity(job.target_build, &job.target_commit),
        )
        .env("ATERM_HANDOFF_READY_FD", proof_wr.as_raw_fd().to_string())
        .env("ATERM_HANDOFF_COMMIT_FD", commit_rd.as_raw_fd().to_string());
    // Parental authority: our pid AND the kernel's birth record for it. The
    // record is what lets the successor watch us without being our fork child —
    // see `seamless::AttestedParent`. Encoded there, beside its decoder, so the
    // two halves of the wire cannot drift.
    for (key, value) in crate::seamless::outgoing_parent_env() {
        job.command.env(key, value);
    }
    // Parent descriptors remain CLOEXEC for the WHOLE asynchronous interval.
    // Clear only the fork child's copies immediately before its exec image.
    let mut child_inherit = job
        .live
        .iter()
        .map(|(_, master, _)| *master)
        .collect::<Vec<_>>();
    child_inherit.push(proof_wr.as_raw_fd());
    child_inherit.push(commit_rd.as_raw_fd());
    // SAFETY: the closure performs only async-signal-safe fcntl calls between
    // fork and exec and touches captured integer fd values, not shared memory.
    unsafe {
        job.command.pre_exec(move || {
            // Candidate and every helper it later launches inherit a dedicated
            // process group. Adopted shell PTYs/process groups are unrelated.
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            for fd in &child_inherit {
                let flags = libc::fcntl(*fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    if handoff_preparation_cancelled(&job, &proxy, Some(nonce.clone())) {
        return;
    }
    // KNOWN DEFECT — the survivor is not a launchd job (macOS).
    //
    // `spawn` makes the successor a fork CHILD of THIS process, and on macOS
    // this process is the process of the launchd job
    // `application.com.aterm.aterm.<hex>.<hex>` that LaunchServices created for
    // this app instance. `seamless::commit_and_exit` then `_exit(0)`s it, so
    // launchd tears that job down and the successor is re-parented to pid 1
    // while still holding a bootstrap (XPC) context belonging to a job that no
    // longer exists. Everything it later spawns inherits the dead domain:
    // `hdiutil` fails ENXIO ("Device not configured") — so the process that
    // just applied an update cannot apply the next one — and the same applies
    // to every other framework needing the app's XPC domain (user
    // notifications, LaunchServices opens).
    //
    // THE FIX is to launch the successor through LaunchServices instead (the
    // `open -n` equivalent, `NSWorkspace.openApplication` with
    // `createsNewApplicationInstance`), which mints it its OWN application job.
    // It is not a local edit: four properties this `spawn` gets for free must be
    // re-established first. `tests/handoff_launchd_job.rs` states all four and
    // is the guard — its ignored e2e is today's reproducer and tomorrow's
    // regression test. Their status here:
    //
    // * B1 — parent attestation. DONE (0.13): the successor's admission and its
    //   `_exit(74)` fail-stop no longer read `getppid()`, which a
    //   LaunchServices-launched process cannot answer (ppid 1 from birth). They
    //   watch the outgoing process's kernel BIRTH RECORD, published just above
    //   by `seamless::outgoing_parent_env` — see `seamless::AttestedParent` for
    //   the property and for what B4's socket adds on top of it.
    // * B2 — reap authority. `kill_and_reap_handoff_child` resumes the parked
    //   readers only once `wait` proved the rejected candidate gone and reaped;
    //   `waitpid` on a non-child answers `ECHILD` and that proof is lost.
    // * B3 — process-group containment. The `pre_exec` `setpgid(0, 0)` above,
    //   which is what makes `kill(-pid)` sweep the candidate's own updater
    //   helpers, has no LaunchServices equivalent.
    // * B4 — transport. A LaunchServices launch inherits no descriptors, so the
    //   three inherited fd channels below — the PTY masters
    //   (`ATERM_SEAMLESS_FDS`), the readiness pipe, and the Commit pipe — must
    //   move to an out-of-band `SCM_RIGHTS` transfer over the per-user control
    //   socket, which also changes what `seamless::adoption_proof` may hash (it
    //   hashes fd NUMBERS, and `SCM_RIGHTS` does not preserve them).
    let mut child = match job.command.spawn() {
        Ok(child) => child,
        Err(error) => {
            send_handoff_preparation_failure(
                &job,
                &proxy,
                Some(nonce),
                format!("handoff process could not start: {error}"),
            );
            return;
        }
    };
    let child_pid = child.id();
    drop(proof_wr);
    drop(commit_rd);
    let proof_outcome = wait_handoff_ready(
        &proof_rd,
        expected,
        &job.cancel,
        &job.live.iter().map(|(_, fd, _)| *fd).collect::<Vec<_>>(),
    );
    if proof_outcome != crate::UpdateHandoffOutcome::ProofReady {
        let rejected = worker_reject_and_reap_handoff_child(
            &job,
            &proxy,
            &mut child,
            &nonce,
            child_pid,
            proof_outcome,
            format!("handoff proof ended {proof_outcome:?}"),
        );
        debug_assert!(rejected, "Commit is unreachable before ProofReady");
        return;
    }

    let (reject, rejected) = std::sync::mpsc::sync_channel(1);
    let ready = Wake::UpdateHandoffFinished(crate::UpdateHandoffCompletion {
        attempt_id: job.attempt_id,
        nonce: Some(nonce.clone()),
        child_pid: Some(child_pid),
        outcome: crate::UpdateHandoffOutcome::ProofReady,
        commit_fd: Some(commit_wr),
        reject: Some(reject),
        reconcile: None,
        detail: "child painted and proved exact readerless adoption".to_string(),
        input_drain_spins: 0,
    });
    if proxy.send_event(ready).is_err() {
        // No main-thread final validation occurred, therefore Commit is impossible.
        // Kill/reap readerless child; never exit as though authority was granted.
        let rejected = worker_reject_and_reap_handoff_child(
            &job,
            &proxy,
            &mut child,
            &nonce,
            child_pid,
            crate::UpdateHandoffOutcome::Rejected,
            "event loop closed before final handoff admission".to_string(),
        );
        debug_assert!(rejected, "Commit is unreachable after a failed ready wake");
        return;
    }

    let decision_deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        // SEAMLESS: readable PTY output no longer revokes here — post-park
        // bytes wait gap-free in the kernel for the child. Only an explicit
        // cancel poke (structural/mode-changing activity, typed as
        // ActivityRevoked for the retry budget) or session DEATH (HUP/ERR —
        // the proof's live-set identity is stale) rejects before Commit.
        if job.cancel.try_recv().is_ok()
            && worker_reject_and_reap_handoff_child(
                &job,
                &proxy,
                &mut child,
                &nonce,
                child_pid,
                crate::UpdateHandoffOutcome::ActivityRevoked,
                "structural activity revoked handoff before Commit".to_string(),
            )
        {
            return;
        }
        if handoff_masters_closed(&job.live)
            && worker_reject_and_reap_handoff_child(
                &job,
                &proxy,
                &mut child,
                &nonce,
                child_pid,
                crate::UpdateHandoffOutcome::Rejected,
                "a handed-off PTY session closed before Commit".to_string(),
            )
        {
            return;
        }
        match rejected.try_recv() {
            Ok(()) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                if worker_reject_and_reap_handoff_child(
                    &job,
                    &proxy,
                    &mut child,
                    &nonce,
                    child_pid,
                    crate::UpdateHandoffOutcome::Rejected,
                    "final handoff admission rejected before Commit".to_string(),
                ) {
                    return;
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        if std::time::Instant::now() >= decision_deadline
            && worker_reject_and_reap_handoff_child(
                &job,
                &proxy,
                &mut child,
                &nonce,
                child_pid,
                crate::UpdateHandoffOutcome::Rejected,
                "main-thread final handoff decision timed out".to_string(),
            )
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

impl App {
    /// PROOF-CARRYING DSU (RFC Rung 1): APPLY a staged update now by re-execing — the
    /// staged build swaps in at the top of the new `main` (`apply_staged_if_ready`).
    /// Reached from `Wake::ApplyStagedUpdate` (the `aterm-ctl update apply` verb / the
    /// GUI [Relaunch] nudge). No-op unless a STRICTLY-NEWER build is actually staged
    /// (never a pointless restart). Rung 1b live wiring (DEFAULT-ON, opt out with
    /// `ATERM_NO_SEAMLESS_UPDATE`): hands every live PTY master, its exact visible-screen
    /// checkpoint, and a `SessionHandoff` manifest to the new process so the running shell survives
    /// (the round-trip that makes that safe is
    /// proven — `SessionHandoff` + `handoff_roundtrip_model`; the single-use nonce by
    /// `seamless_nonce_model`). Scope: the live process, visible rows, terminal modes/cursors,
    /// and output queued after reader park survive. Preexisting off-screen scrollback is
    /// deliberately excluded to keep capture latency bounded. A cold relaunch is
    /// permitted only when no foreground terminal job would be destroyed. Every path that
    /// leaves this process alive returns an actionable failure so the updater reducer can
    /// re-arm the verified stage instead of remaining stuck in `Applying`.
    pub(crate) fn apply_staged_update_now(
        &mut self,
        safety_token: crate::app_native::NativeUpdateSafetyToken,
        mode: crate::native_updater_service::ApplyMode,
        apply_attempt: Option<crate::native_updater_service::ApplyAttemptTicket>,
    ) -> Result<(), crate::UpdateHandoffStartError> {
        if self.pending_update_handoff.is_some() {
            return Err(crate::UpdateHandoffStartError::failed(
                "an update handoff is already in flight",
            ));
        }
        let build = crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0);
        // QA SEAM: `ATERM_DEBUG_SEAMLESS_REEXEC=1` re-execs the SAME binary (no staged
        // build, no bundle swap) but exercises the FULL seamless handoff + adopt path, so
        // the shell-survives-an-update contract is testable end-to-end without a release.
        let debug_seamless = std::env::var_os("ATERM_DEBUG_SEAMLESS_REEXEC").is_some();
        if !debug_seamless && apply_attempt.is_none() {
            let message = "no exact verified update authority was supplied".to_string();
            aterm_log::info!("update apply: {message}");
            return Err(crate::UpdateHandoffStartError::failed(message));
        }
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(error) => {
                let message = format!("current executable is unavailable: {error}");
                aterm_log::warn!("update apply: {message}; cannot re-exec");
                return Err(crate::UpdateHandoffStartError::failed(message));
            }
        };
        aterm_log::info!("update apply: authorized process replacement from build {build}");
        // Re-exec THIS binary; the swap to the staged build happens at the top of the
        // new `main`. `exec` never returns on success. (Rung 1b: serialize the session
        // + pass the PTY master fds here so the new process re-adopts them.)
        // Hand the OLD build number to the post-update process via `ATERM_UPDATED_FROM`
        // (inherited through `apply_staged_if_ready`'s own re-exec) so it shows the quiet
        // cursor-themed "leveled-up" notice on startup. Read + cleared at startup so it
        // never leaks into the user's shell children (`App::take_just_updated`).
        #[cfg(unix)]
        {
            return self.start_unix_update_handoff(
                exe,
                build,
                safety_token,
                mode,
                apply_attempt,
                debug_seamless,
            );
        }
        // Windows has no exec(2); the analog is spawn-the-new-then-exit. Dead in
        // practice today (the updater is macOS-only, so `staged` is always None
        // above), but kept correct for when a Windows update lane exists.
        #[cfg(windows)]
        {
            let live_ptys = self.pool.iter().count();
            let facts = crate::native_update_admission::AdmissionFacts {
                staged_verified: debug_seamless || apply_attempt.is_some(),
                seamless_capable: false,
                native_state_certified: safety_token.is_certified(),
                live_ptys,
                foreground_jobs: 0,
                // ConPTY currently has no exact foreground-pgrp proof. Any live
                // session is therefore both non-cold and foreground-unknown.
                unknown_foregrounds: live_ptys,
            };
            match crate::native_update_admission::classify(facts) {
                crate::native_update_admission::AdmissionDecision::Apply(
                    crate::native_update_admission::ApplyLane::Cold,
                ) if live_ptys == 0 => {}
                crate::native_update_admission::AdmissionDecision::Block(reason) => {
                    return Err(crate::UpdateHandoffStartError::failed(
                        reason.message(facts),
                    ));
                }
                _ => {
                    return Err(crate::UpdateHandoffStartError::failed(
                        "Windows update replacement requires an exact zero-session state",
                    ));
                }
            }
            self.shutdown_title_summaries();
            match {
                let mut cmd = std::process::Command::new(exe);
                cmd.args(std::env::args_os().skip(1))
                    .env("ATERM_UPDATED_FROM", build.to_string());
                bind_expected_update_artifact(&mut cmd, apply_attempt.as_ref());
                // Same headless re-injection as the unix exec path above.
                if self.headless {
                    cmd.env("ATERM_HEADLESS", "1");
                }
                cmd.spawn()
            } {
                Ok(_) => std::process::exit(0),
                Err(err) => {
                    // A failed spawn leaves this process live. Restore a fresh exact
                    // authority/worker after the pre-replacement shutdown.
                    self.reconfigure_title_summaries();
                    aterm_log::warn!("update apply: re-spawn failed: {err}");
                    return Err(crate::UpdateHandoffStartError::failed(format!(
                        "replacement process could not start: {err}"
                    )));
                }
            }
        }

        #[allow(unreachable_code)]
        Err(crate::UpdateHandoffStartError::failed(
            "process replacement returned without applying the update",
        ))
    }

    /// The control-socket pair a [`HandoffWorkerCleanup`] must republish after
    /// the overlap resolves: `Some((latest_link, sock_path))` only while the
    /// socket is actually bound and the plan mints a latest link. One
    /// projection for the worker cleanup and the emergency-reaper cleanup, so
    /// the two can never disagree about what gets republished.
    #[cfg(unix)]
    fn handoff_parent_socket(&self) -> Option<(std::path::PathBuf, String)> {
        self.sock_bound
            .load(std::sync::atomic::Ordering::Acquire)
            .then(|| {
                let plan = self.sock_plan.as_ref()?;
                Some((plan.latest_link.clone()?, plan.sock_path.clone()))
            })
            .flatten()
    }

    #[cfg(unix)]
    fn start_unix_update_handoff(
        &mut self,
        exe: std::path::PathBuf,
        build: u64,
        safety_token: crate::app_native::NativeUpdateSafetyToken,
        mode: crate::native_updater_service::ApplyMode,
        apply_attempt: Option<crate::native_updater_service::ApplyAttemptTicket>,
        debug_seamless: bool,
    ) -> Result<(), crate::UpdateHandoffStartError> {
        use std::os::unix::process::CommandExt as _;

        let live: Vec<(u64, i32, i32)> =
            self.pool.iter().map(|s| (s.id, s.master, s.pid)).collect();
        let automatic_activity_epoch = (mode
            == crate::native_updater_service::ApplyMode::Automatic)
            .then_some(self.update_handoff_activity_epoch);
        if automatic_activity_epoch.is_some()
            && !self.automatic_update_activity_quiet(std::time::Instant::now())
        {
            return Err(crate::UpdateHandoffStartError::activity(
                "terminal activity has not reached the automatic-update quiet epoch",
            ));
        }
        let (foreground_jobs, unknown_foregrounds) = live.iter().fold(
            (0usize, 0usize),
            |(jobs, unknown), (_, master, shell_pid)| {
                let foreground = crate::quit_safety::foreground_pgrp(*master);
                if foreground <= 0 {
                    (jobs, unknown + 1)
                } else if foreground != *shell_pid {
                    (jobs + 1, unknown)
                } else {
                    (jobs, unknown)
                }
            },
        );
        let overlap_available = !self.headless
            && std::env::var_os("ATERM_NO_SEAMLESS_UPDATE").is_none()
            && std::env::var_os("ATERM_CONTROL_SOCK").is_none()
            && std::env::var_os("ATERM_NO_OVERLAP_HANDOFF").is_none()
            && self.proxy.is_some();
        let facts = crate::native_update_admission::AdmissionFacts {
            staged_verified: debug_seamless || apply_attempt.is_some(),
            seamless_capable: overlap_available && !live.is_empty(),
            native_state_certified: safety_token.is_certified(),
            live_ptys: live.len(),
            foreground_jobs,
            unknown_foregrounds,
        };
        match crate::native_update_admission::classify(facts) {
            crate::native_update_admission::AdmissionDecision::Block(reason) => {
                return Err(crate::UpdateHandoffStartError::failed(
                    reason.message(facts),
                ));
            }
            crate::native_update_admission::AdmissionDecision::Apply(
                crate::native_update_admission::ApplyLane::Cold,
            ) => {
                // Direct exec is authorized only for an exact zero-PTY state.
                debug_assert!(live.is_empty());
                let mut command = std::process::Command::new(exe);
                command
                    .args(std::env::args_os().skip(1))
                    .env("ATERM_UPDATED_FROM", build.to_string());
                bind_expected_update_artifact(&mut command, apply_attempt.as_ref());
                if self.headless {
                    command.env("ATERM_HEADLESS", "1");
                }
                self.shutdown_title_summaries();
                let error = command.exec();
                // `exec` returns only on failure. Recreate the worker/authority so
                // Smart Titles continue in the still-running old process.
                self.reconfigure_title_summaries();
                return Err(crate::UpdateHandoffStartError::failed(format!(
                    "process replacement failed: {error}"
                )));
            }
            crate::native_update_admission::AdmissionDecision::Apply(
                crate::native_update_admission::ApplyLane::Seamless,
            ) => {}
        }

        // PRE-VERIFY THE STAGED CANDIDATE (seamless seam 1) — codesign policy +
        // sealed build/commit rebinding, bound to the exact authorized artifact.
        // This authenticates a doomed candidate BEFORE the worker spawns the
        // child, so no doomed child is ever launched. It is strictly ADDITIVE
        // authority: the child re-runs the complete gate at swap time under the
        // apply lock (the TOCTOU defence is unchanged; `Ok` here is a latency +
        // warm-cache optimization, never a grant).
        //
        // OFF THE UI THREAD: the check is `codesign --deep` plus a bundle flock —
        // unbounded, disk-bound work that MUST NOT run on the GUI main thread
        // (it froze every frame before every handoff). It now runs as the
        // worker's FIRST action (see `run_handoff_worker`), so the main thread
        // parks readers and returns without ever blocking on codesign. A failing
        // candidate is caught there and rolled back via the ordinary
        // `PreparationFailed` completion (manual-only, like every other worker-
        // stage preparation failure). The same-binary debug re-exec has no
        // staged bundle, so it carries `verify_staged_candidate: false`.
        //
        // …AND BETTER STILL, OUT OF THE PARKED WINDOW ENTIRELY: when this exact
        // artifact already passed `spawn_staged_handoff_preverification` while
        // every reader was still live, the worker skips the repeat and the
        // parked interval shrinks by the whole `codesign --deep` cost. A cached
        // REFUSAL short-circuits the attempt right here, before anything parks.
        // Never an authorization either way — the child re-runs the complete
        // gate under the apply lock at swap time.
        let preverified = apply_attempt.as_ref().and_then(|attempt| {
            let cached = self
                .handoff_preverified
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            cached
                .as_ref()
                .filter(|entry| {
                    entry.build == attempt.target_build()
                        && entry.commit == attempt.target_commit()
                        && entry.at.elapsed() < crate::HANDOFF_PREVERIFY_FRESHNESS
                })
                .map(|entry| entry.passed)
        });
        if preverified == Some(false) {
            return Err(crate::UpdateHandoffStartError::failed(
                "the staged update failed verification; the terminal was left untouched",
            ));
        }
        let verify_staged_candidate = !debug_seamless && preverified != Some(true);

        let Some(proxy) = self.proxy.clone() else {
            return Err(crate::UpdateHandoffStartError::failed(
                "overlap handoff has no event-loop completion channel",
            ));
        };
        let Some(reconcile_ticket) = self.mint_native_update_reconcile_ticket() else {
            return Err(crate::UpdateHandoffStartError::failed(
                "updater reconciliation identity space is exhausted",
            ));
        };
        let Some(reconcile_worker) = self.native_update_reconcile_worker() else {
            return Err(crate::UpdateHandoffStartError::failed(
                "updater reconciliation worker is unavailable",
            ));
        };
        // Parent master flags are an invariant, not mutable handoff state. Child
        // inheritance is installed later by `pre_exec` on child copies only.
        for (_, master, _) in &live {
            if aterm_pty::set_cloexec(*master, true).is_err() {
                return Err(crate::UpdateHandoffStartError::failed(
                    "could not enforce CLOEXEC on every parent PTY master",
                ));
            }
        }

        // Capture owned structural data before parking. These projections perform
        // no disk I/O; terminal metadata uses try_lock and degrades to empty rather
        // than waiting behind reflow/compression.
        let layout = self.capture_restore_manifest();
        let manifest = match self.store.try_read() {
            Ok(store) => crate::session_store::SessionHandoff::from_store(&store),
            Err(std::sync::TryLockError::Poisoned(poison)) => {
                let store = poison.into_inner();
                crate::session_store::SessionHandoff::from_store(&store)
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                return Err(crate::UpdateHandoffStartError::failed(
                    "session registry was busy; update handoff stayed in place",
                ));
            }
        };
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        let mut owned_masters = Vec::with_capacity(live.len());
        let mut adoption = Vec::with_capacity(live.len());
        for (local_id, master, pid) in &live {
            // SAFETY: duplicates one live parent master as an independent CLOEXEC
            // descriptor. The original can later close/reuse its number without
            // changing the open-file-description inherited by the child.
            let duplicate = unsafe { libc::fcntl(*master, libc::F_DUPFD_CLOEXEC, 3) };
            if duplicate < 0 {
                return Err(crate::UpdateHandoffStartError::failed(
                    "could not reserve child-only PTY descriptors",
                ));
            }
            // SAFETY: F_DUPFD_CLOEXEC returned a fresh descriptor owned here.
            let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicate) };
            adoption.push((*local_id, owned.as_raw_fd(), *pid));
            owned_masters.push(owned);
        }
        let fds = crate::session_store::HandoffFds {
            entries: adoption.clone(),
        };
        let window = self.windows.values().next().map(|state| {
            let position = state
                .os_window
                .as_ref()
                .and_then(|window| window.outer_position().ok());
            crate::session_store::WindowCarry {
                rows: state.rows,
                cols: state.cols,
                outer_x: position.map(|point| point.x),
                outer_y: position.map(|point| point.y),
            }
        });

        // Provision the worker BEFORE readers park. A resource-exhausted thread
        // creation therefore returns with the terminal completely untouched.
        let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
        let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<HandoffWorkerJob>(1);
        let worker_proxy = proxy.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("aterm-update-handoff".to_string())
            .spawn(move || {
                if let Ok(job) = job_rx.recv() {
                    run_handoff_worker(job, worker_proxy);
                }
            })
        {
            return Err(crate::UpdateHandoffStartError::failed(format!(
                "overlap handoff worker could not start: {error}"
            )));
        }

        // Close the synchronous-preparation TOCTOU immediately before the first
        // reader stop. Activity defers automatic apply while every reader is
        // still live; manual explicit apply bypasses only this quiet policy.
        if automatic_activity_epoch.is_some_and(|epoch| {
            self.update_handoff_activity_epoch != epoch
                || !self.automatic_update_activity_quiet(std::time::Instant::now())
                || handoff_masters_have_activity(&live)
        }) {
            return Err(crate::UpdateHandoffStartError::activity(
                "input or PTY output arrived before automatic reader park",
            ));
        }
        // How much of the capture window must remain before a session is willing to
        // serialize scrollback as well as its visible screen. Half the budget: the
        // visible screen is mandatory and cheap, history is optional and priced per
        // line, so once the window is half gone every remaining session drops to
        // visible-only rather than risking the deadline for a bonus. This is what
        // makes carrying history safe to enable by default — the failure mode is
        // "less scrollback", never "the update did not apply".
        const HANDOFF_HISTORY_COMFORT: std::time::Duration = std::time::Duration::from_millis(10);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(20);
        if !self.park_all_readers(deadline) {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::failed(
                "a PTY reader missed the 20 ms handoff park deadline",
            ));
        }
        // SEAMLESS: the post-park re-check tolerates activity that used to
        // revoke here. A final burst consumed by a reader during the bounded
        // park is already inside the engine, so the checkpoints captured next
        // carry it; bytes that arrived after park wait in the kernel for the
        // child; hardware input queued during the ~20 ms park is dispatched
        // normally after this function returns and delivers to the still-open
        // masters. (The activity EPOCH provably cannot move here: every bump
        // happens on this thread, which is inside this function.) What must
        // still reject mid-flight is session DEATH — a HUP/ERR master means
        // the live-set identity the proof would commit to is already stale.
        // Classified as an activity deferral (matching the old disposition for
        // a death observed here): intent is retained and the next quiet-window
        // attempt sees the post-exit session set.
        if automatic_activity_epoch.is_some() && handoff_masters_closed(&live) {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::activity(
                "a PTY session closed during automatic reader park",
            ));
        }
        let mut screens = Vec::new();
        if screens.try_reserve_exact(live.len()).is_err() {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::failed(
                "visible checkpoint set could not reserve bounded storage",
            ));
        }
        let mut capture_failed = None;
        let mut capture_budget = 0_u64;
        let mut capture_cells = 0_u64;
        for session in self.pool.iter() {
            if std::time::Instant::now() >= deadline {
                capture_failed = Some("bounded visible-screen capture exceeded 20 ms");
                break;
            }
            let terminal = match session.term.try_lock() {
                Ok(guard) => guard,
                Err(std::sync::TryLockError::Poisoned(poison)) => poison.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    capture_failed = Some("a terminal engine was busy during handoff capture");
                    break;
                }
            };
            if !terminal.parser_is_ground() {
                capture_failed = Some("a terminal parser was mid-sequence during handoff capture");
                break;
            }
            // SCROLLBACK IS BEST-EFFORT AND MUST NEVER COST THE HANDOFF.
            //
            // Carrying history is what stops an in-session update truncating every
            // tab to one screen, but it is strictly a bonus: the visible screen is
            // what adoption actually requires. So the depth is chosen per session
            // against the REMAINING time, and collapses to zero once the window is
            // more than half spent. Failing the handoff to protect scrollback would
            // trade the whole update for the thing the update was carrying.
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let history = if remaining >= HANDOFF_HISTORY_COMFORT {
                crate::seamless::max_handoff_history_lines()
            } else {
                0
            };
            // Reject decoded allocation dimensions BEFORE the checkpoint serializes
            // anything. Conservatively reserve main+alt because querying the copied
            // checkpoint to discover alt presence is exactly the potentially
            // expensive work this admission must precede.
            let per_grid = crate::seamless::admit_checkpoint_dimensions(
                &mut capture_cells,
                terminal.rows(),
                terminal.cols(),
                history,
                true,
            );
            capture_budget = per_grid
                .and_then(|bytes| bytes.checked_mul(2))
                .and_then(|bytes| capture_budget.checked_add(bytes))
                .unwrap_or(u64::MAX);
            if capture_budget > 256 * 1024 * 1024 {
                capture_failed = Some("aggregate visible-screen capture exceeded its memory cap");
                break;
            }
            let Some(checkpoint) = terminal.checkpoint_carry(history as usize) else {
                capture_failed = Some("a terminal parser left Ground during handoff capture");
                break;
            };
            screens.push((session.id, checkpoint));
            if std::time::Instant::now() >= deadline {
                capture_failed = Some("bounded visible-screen capture exceeded 20 ms");
                break;
            }
        }
        if capture_failed.is_none() && std::time::Instant::now() >= deadline {
            capture_failed = Some("bounded visible-screen capture exceeded 20 ms");
        }
        if let Some(reason) = capture_failed {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::failed(format!(
                "{reason}; update handoff stayed in place"
            )));
        }

        let attempt_id = self.next_update_handoff_id;
        let Some(next_attempt_id) = attempt_id.checked_add(1) else {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::failed(
                "handoff identity space is exhausted",
            ));
        };
        self.next_update_handoff_id = next_attempt_id;
        let mut command = std::process::Command::new(exe);
        command
            .args(std::env::args_os().skip(1))
            .env("ATERM_UPDATED_FROM", build.to_string());
        bind_expected_update_artifact(&mut command, apply_attempt.as_ref());
        let target_build = apply_attempt
            .as_ref()
            .map_or(build, |attempt| attempt.target_build());
        let target_commit = apply_attempt.as_ref().map_or_else(
            || crate::build_info::GIT_COMMIT.to_string(),
            |attempt| attempt.target_commit().to_string(),
        );
        let Some(layout_digest) = crate::seamless::layout_digest(&layout) else {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::failed(
                "handoff layout could not be committed canonically",
            ));
        };
        let Some(screen_digest) = crate::seamless::screen_digest(&screens) else {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::failed(
                "visible checkpoint set could not be committed canonically",
            ));
        };
        if std::time::Instant::now() >= deadline {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::failed(
                "handoff proof capture exceeded the 20 ms deadline",
            ));
        }
        if self.update_handoff_activity_epoch == u64::MAX {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::failed(
                "handoff activity identity space is exhausted",
            ));
        }
        let activity_epoch = self.update_handoff_activity_epoch;
        let arbiter = crate::HandoffAttemptArbiter::new();
        self.pending_update_handoff = Some(crate::PendingUpdateHandoff {
            attempt_id,
            nonce: None,
            live: live.clone(),
            adoption: adoption.clone(),
            child_pid: None,
            mode,
            apply_attempt,
            target_build,
            target_commit: target_commit.clone(),
            layout: layout.clone(),
            layout_digest,
            screen_digest,
            activity_epoch,
            cancel: cancel.clone(),
            arbiter: arbiter.clone(),
            teardown: if mode == crate::native_updater_service::ApplyMode::CleanQuit {
                crate::DeferredHandoffTeardown::CleanQuitReady
            } else {
                crate::DeferredHandoffTeardown::None
            },
            commit_drain_started: None,
            revoked_by_activity: false,
        });
        let cleanup = HandoffWorkerCleanup {
            parent_socket: self.handoff_parent_socket(),
            reconcile: Some((reconcile_worker, reconcile_ticket)),
        };
        let job = HandoffWorkerJob {
            attempt_id,
            current_build: build,
            target_build,
            target_commit,
            verify_staged_candidate,
            command,
            manifest,
            fds,
            screens,
            window,
            layout,
            layout_digest,
            screen_digest,
            live: adoption,
            cleanup,
            cancel: cancelled,
            arbiter,
            _owned_masters: owned_masters,
        };
        if job_tx.send(job).is_err() {
            self.pending_update_handoff = None;
            let live: Vec<_> = self
                .pool
                .iter()
                .map(|session| (session.id, session.master, session.pid))
                .collect();
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::failed(
                "overlap handoff worker stopped before preparation",
            ));
        }
        Ok(())
    }

    /// The pre-Commit input-drain gate. Returns `None` when the completion was
    /// re-posted for another drain spin (the caller must return), otherwise
    /// the completion handed back plus the `input_dispatch_fenced` /
    /// `egress_settled` admission facts.
    ///
    /// DRAIN, DON'T DIE (seamless: OS-accepted input): hardware events
    /// accepted immediately before this callback may not yet have been
    /// dispatched through winit and would die with `_exit`. Defer Commit
    /// through a short CoreGraphics quiet epoch, using a scalar event
    /// source clock that cannot recursively pump AppKit. Re-posting this
    /// exact completion gives the run loop time to dispatch those events —
    /// their bytes flow through the tolerated input path into the
    /// still-open PTY masters — and the re-post re-runs this admission
    /// against a drained queue. Bounded by the spin cap below (sustained
    /// typing exhausts it and is then treated as activity revocation,
    /// retaining the automatic retry budget) and absolutely by the
    /// worker's 15 s decision deadline. A failed re-post means the event
    /// loop is closing; dropping the completion drops the reject sender,
    /// which the worker observes as Disconnected and rejects/reaps.
    ///
    /// …AND DON'T LEAVE IT IN A RUST QUEUE EITHER: a tolerated keystroke,
    /// once dispatched, does not go straight to the master — under a live
    /// paste it rides the per-session paste-order FIFO, and against a
    /// wedged tty it lands in the sink's spill buffer. Both are
    /// PROCESS-LOCAL: they die with `_exit` exactly like the AppKit queue.
    /// So Commit also waits until every handed-off session's egress has
    /// reached the kernel (`handoff_egress_settled`) — the drainer/writer
    /// threads flush it to the still-open master between re-posts. Same
    /// bounded, lossless defer; the fact fences a budget-exhausted
    /// Commit so an unflushable spill fails closed (rollback) instead of
    /// `_exit`ing over undelivered bytes.
    ///
    /// The hard admission facts are the dispatch fence and settled
    /// egress (see `HandoffCommitFacts::input_dispatch_fenced` /
    /// `egress_settled`). A quiet window is still PREFERRED — it is
    /// simply no longer required, and we stop paying for it after a
    /// bounded budget.
    #[cfg(unix)]
    fn handoff_drain_gate(
        &mut self,
        completion: crate::UpdateHandoffCompletion,
    ) -> Option<(crate::UpdateHandoffCompletion, bool, bool)> {
        const HANDOFF_INPUT_DRAIN_SPIN_CAP: u32 = 4_000;
        /// Opportunistic gap we would LIKE to commit inside.
        const HANDOFF_INPUT_QUIET_EPOCH: std::time::Duration = std::time::Duration::from_millis(15);
        /// How long we are willing to wait for that gap before committing
        /// anyway. Under a continuously-driven terminal it never comes.
        const HANDOFF_INPUT_QUIET_BUDGET: std::time::Duration =
            std::time::Duration::from_millis(400);
        /// MANDATORY minimum event-loop time between ProofReady and Commit.
        const HANDOFF_INPUT_DISPATCH_FENCE: std::time::Duration =
            std::time::Duration::from_millis(30);
        /// Per-respin yield, so the fence is measured in loop iterations
        /// that really dispatched events instead of a busy spin.
        const HANDOFF_INPUT_DRAIN_YIELD: std::time::Duration = std::time::Duration::from_millis(2);
        /// Absolute wall-clock backstop. An egress queue that never settles
        /// must fail CLOSED (rollback), never `_exit` over undelivered bytes.
        const HANDOFF_INPUT_DRAIN_DEADLINE: std::time::Duration =
            std::time::Duration::from_millis(3_000);
        // A vanished pending attempt cannot be drained toward Commit; fall
        // straight through to the rejection path rather than respinning on
        // a clock that would restart every iteration.
        let drained_for = {
            let now = std::time::Instant::now();
            self.pending_update_handoff.as_mut().map(|pending| {
                now.saturating_duration_since(*pending.commit_drain_started.get_or_insert(now))
            })
        };
        let drained_for = drained_for.unwrap_or(HANDOFF_INPUT_DRAIN_DEADLINE);
        // The fence needs BOTH a completed re-post (so the loop really
        // iterated) and the elapsed floor.
        let input_dispatch_fenced =
            completion.input_drain_spins >= 1 && drained_for >= HANDOFF_INPUT_DISPATCH_FENCE;
        let input_quiet = !crate::platform::recent_user_input_event(HANDOFF_INPUT_QUIET_EPOCH);
        let quiet_window_settled = input_quiet || drained_for >= HANDOFF_INPUT_QUIET_BUDGET;
        let egress_settled = self
            .pending_update_handoff
            .as_ref()
            .map(|pending| pending.live.clone())
            .is_none_or(|live| handoff_egress_settled(&self.pool, &live));
        if (!input_dispatch_fenced || !quiet_window_settled || !egress_settled)
            && completion.input_drain_spins < HANDOFF_INPUT_DRAIN_SPIN_CAP
            && drained_for < HANDOFF_INPUT_DRAIN_DEADLINE
            && let Some(proxy) = self.proxy.clone()
        {
            // Yield the main thread so the run loop actually dispatches the
            // queued NSEvents before our re-post comes back around. Cheap
            // and bounded: the frozen frame is already parked.
            std::thread::sleep(HANDOFF_INPUT_DRAIN_YIELD);
            let respin = crate::UpdateHandoffCompletion {
                input_drain_spins: completion.input_drain_spins.saturating_add(1),
                ..completion
            };
            let _ = proxy.send_event(Wake::UpdateHandoffFinished(respin));
            return None;
        }
        Some((completion, input_dispatch_fenced, egress_settled))
    }

    /// Snapshot the pending attempt and collect every Commit admission fact in
    /// one place (see [`HandoffCommitFacts`]). `None` when the pending attempt
    /// vanished mid-drain — the caller rejects and returns. Also hands back
    /// the fresh native-safety evidence (its `Err` carries the human-readable
    /// reasons), the exact adoption proof, and the attempt arbiter.
    #[cfg(unix)]
    #[allow(clippy::type_complexity)]
    fn collect_handoff_commit_facts(
        &mut self,
        nonce: Option<&str>,
        input_dispatch_fenced: bool,
        egress_settled: bool,
        commit_channel: bool,
    ) -> Option<(
        HandoffCommitFacts,
        Result<crate::app_native::NativeUpdateSafetyToken, Vec<String>>,
        Option<crate::seamless::AdoptionProof>,
        crate::HandoffAttemptArbiter,
    )> {
        let pending = self.pending_update_handoff.as_ref()?;
        let pending_live = pending.live.clone();
        let pending_adoption = pending.adoption.clone();
        let pending_target_build = pending.target_build;
        let pending_target_commit = pending.target_commit.clone();
        let pending_layout = pending.layout.clone();
        let pending_layout_digest = pending.layout_digest;
        let pending_screen_digest = pending.screen_digest;
        let pending_activity_epoch = pending.activity_epoch;
        let arbiter = pending.arbiter.clone();
        let teardown_allows_commit = matches!(
            pending.teardown,
            crate::DeferredHandoffTeardown::None | crate::DeferredHandoffTeardown::CleanQuitReady
        );
        let exact_activity = self.update_handoff_activity_epoch == pending_activity_epoch;
        let mut current_live: Vec<(u64, i32, i32)> =
            self.pool.iter().map(|s| (s.id, s.master, s.pid)).collect();
        current_live.sort_unstable();
        let mut expected_live = pending_live.clone();
        expected_live.sort_unstable();
        let exact_sessions = current_live == expected_live;
        let exact_layout = self.capture_restore_manifest() == pending_layout;
        let parent_still_parked = self
            .pool
            .iter()
            .all(|session| session.reader_join.is_none());
        let native_safety = self.revalidate_native_update_safety();
        // Death-only peek: queued output on a master is tolerated (it
        // waits in the kernel for the child); HUP/ERR means the adopted
        // live-set identity is already stale and must reject.
        let sessions_alive = !handoff_masters_closed(&pending_live);
        let proof = nonce.and_then(|nonce| {
            crate::seamless::adoption_proof(
                nonce,
                pending_target_build,
                &pending_target_commit,
                &pending_layout_digest,
                &pending_screen_digest,
                &pending_adoption,
            )
        });
        let facts = HandoffCommitFacts {
            exact_sessions,
            exact_layout,
            exact_activity,
            teardown_allows_commit,
            parent_still_parked,
            sessions_alive,
            input_dispatch_fenced,
            egress_settled,
            native_safe: native_safety.is_ok(),
            proof_exact: proof.is_some(),
            commit_channel,
        };
        Some((facts, native_safety, proof, arbiter))
    }

    /// Main-thread completion of the asynchronous overlap proof. `ProofReady` is
    /// deliberately not sufficient by itself: native state and the exact live PTY
    /// identity set may have changed while the child booted. Only a fresh proof plus
    /// unchanged sessions authorizes the destructor-free parent exit.
    #[cfg(unix)]
    pub(crate) fn finish_update_handoff(
        &mut self,
        el: &ActiveEventLoop,
        completion: crate::UpdateHandoffCompletion,
    ) {
        let matches_pending = self
            .pending_update_handoff
            .as_ref()
            .is_some_and(|pending| pending.attempt_id == completion.attempt_id);
        if !matches_pending {
            // A stale proof can never authorize Commit. Ask its still-owning worker
            // to kill/reap readerless child; never wait on the event loop.
            let attempt_id = completion.attempt_id;
            let _ = deliver_handoff_rejection(completion.reject);
            aterm_log::warn!("update apply: ignored stale handoff completion {attempt_id}");
            return;
        }
        if completion.outcome == crate::UpdateHandoffOutcome::ProofReady {
            {
                let Some(pending) = self.pending_update_handoff.as_mut() else {
                    if let Some(reject) = completion.reject {
                        let _ = reject.try_send(());
                    }
                    return;
                };
                pending.nonce = completion.nonce.clone();
                pending.child_pid = completion.child_pid;
            }
            let Some((completion, input_dispatch_fenced, egress_settled)) =
                self.handoff_drain_gate(completion)
            else {
                // Re-posted for another drain spin (or the event loop is
                // closing and the drop rejects the attempt).
                return;
            };
            // Unused fields keep their underscore-prefixed bindings so their
            // drops still run at the end of this call, exactly as when the
            // whole completion was destructured on entry.
            let crate::UpdateHandoffCompletion {
                attempt_id,
                nonce,
                child_pid,
                outcome: _outcome,
                commit_fd,
                reject,
                reconcile: _reconcile,
                detail: _detail,
                input_drain_spins: _input_drain_spins,
            } = completion;
            let Some((facts, native_safety, proof, arbiter)) = self.collect_handoff_commit_facts(
                nonce.as_deref(),
                input_dispatch_fenced,
                egress_settled,
                commit_fd.is_some(),
            ) else {
                // The pending attempt vanished mid-drain: nothing can be
                // committed; ask the worker to reject and reap.
                if let Some(reject) = reject {
                    let _ = reject.try_send(());
                }
                return;
            };
            let commit_admitted = handoff_commit_admitted(facts);
            let mut commit_lost_arbiter = false;
            let mut commit_write_failed = false;
            if commit_admitted && let (Some(commit_fd), Some(proof)) = (commit_fd.as_ref(), proof) {
                if arbiter.try_begin_commit() {
                    aterm_log::info!(
                        "update apply: committing exact readerless handoff to child {:?}",
                        child_pid
                    );
                    // Success cannot return: `commit_and_exit` performs the one
                    // atomic <=PIPE_BUF write and `_exit(0)` in the same typed
                    // operation. EPIPE explicitly transfers Committing back to
                    // Rejecting so one reaper can restore the parent.
                    let Err(_) = crate::seamless::commit_and_exit(commit_fd, proof);
                    commit_write_failed = true;
                    let _ = arbiter.commit_failed_to_rejecting();
                } else {
                    commit_lost_arbiter = true;
                }
            }

            let rejection = handoff_rejection_reason(
                facts,
                &native_safety,
                commit_lost_arbiter,
                commit_write_failed,
            );
            let activity_shaped = handoff_rejection_activity_shaped(facts);
            if activity_shaped && let Some(pending) = self.pending_update_handoff.as_mut() {
                pending.revoked_by_activity = true;
            }
            aterm_log::warn!("update apply: {rejection}; rejecting readerless child");
            let rejection_started = arbiter.try_begin_reject()
                || arbiter.phase() == crate::HandoffAttemptPhase::Rejecting;
            let delivery = rejection_started.then(|| deliver_handoff_rejection(reject));
            if delivery == Some(HandoffRejectDelivery::Disconnected)
                && child_pid.is_some()
                && self.proxy.is_some()
                && arbiter.claim_reaper(crate::HandoffReaperOwner::Emergency)
            {
                aterm_log::warn!(
                    "update apply: handoff reaper channel closed; starting emergency reaper"
                );
                if let (Some(child_pid), Some(proxy)) = (child_pid, self.proxy.clone()) {
                    let cleanup = HandoffWorkerCleanup {
                        parent_socket: self.handoff_parent_socket(),
                        reconcile: None,
                    };
                    let emergency_nonce = nonce.clone();
                    let detail = format!("emergency reaper completed after: {rejection}");
                    let thread_arbiter = arbiter.clone();
                    let thread_cleanup = cleanup.clone();
                    let thread_nonce = emergency_nonce.clone();
                    let thread_detail = detail.clone();
                    let thread_proxy = proxy.clone();
                    let spawned = std::thread::Builder::new()
                        .name("aterm-handoff-emergency-reaper".to_string())
                        .spawn(move || {
                            emergency_reap_and_report(
                                child_pid,
                                attempt_id,
                                &thread_arbiter,
                                &thread_cleanup,
                                thread_nonce,
                                thread_detail,
                                &thread_proxy,
                            );
                        });
                    if spawned.is_err() {
                        // Resource exhaustion cannot strand a readerless child.
                        // This fail-safe blocks only after both the normal worker
                        // and emergency thread creation have failed.
                        emergency_reap_and_report(
                            child_pid,
                            attempt_id,
                            &arbiter,
                            &cleanup,
                            emergency_nonce,
                            detail,
                            &proxy,
                        );
                    }
                }
            } else if !rejection_started {
                // A live `Committing` owner is the only state in which rejection
                // may not proceed. Never signal or reap out from under its write.
                aterm_log::warn!(
                    "update apply: Commit owns the attempt; rejection left child untouched"
                );
            }
            return;
        }

        // Every non-ready completion is emitted only AFTER the worker killed and
        // reaped the child. It is now safe to restore parent readers and reduce the
        // exact ticket using disk facts that were also collected off the UI thread.
        // (Unused fields keep underscore-prefixed bindings so their drops still
        // run at the end of this call, as with the old entry destructure.)
        let crate::UpdateHandoffCompletion {
            attempt_id: _attempt_id,
            nonce,
            child_pid: _child_pid,
            outcome,
            commit_fd: _commit_fd,
            reject: _reject,
            reconcile,
            detail,
            input_drain_spins: _input_drain_spins,
        } = completion;
        let Some(pending) = self.pending_update_handoff.take() else {
            return;
        };
        // Activity classification for the bounded automatic retry budget: the
        // worker's typed `ActivityRevoked` outcome, or a main-thread rejection
        // this attempt recorded as activity-shaped. Only automatic mode owns a
        // timer budget; a manual attempt's failure surfaces to the user as
        // before. Genuine failures (ChildDied/TimedOut/AdoptionMismatch/
        // PreparationFailed/plain Rejected without the flag) stay manual-only.
        let activity_revoked = (outcome == crate::UpdateHandoffOutcome::ActivityRevoked
            || pending.revoked_by_activity)
            && pending.mode == crate::native_updater_service::ApplyMode::Automatic;
        let teardown = match (pending.mode, pending.teardown) {
            // Construction records this eagerly; keep a fail-safe derivation from
            // the typed mode so a future constructor cannot strand an authorized
            // clean quit merely by omitting the replay marker.
            (
                crate::native_updater_service::ApplyMode::CleanQuit,
                crate::DeferredHandoffTeardown::None,
            ) => crate::DeferredHandoffTeardown::CleanQuitReady,
            (_, teardown) => teardown,
        };
        self.rollback_overlap(nonce.as_deref(), &pending.live);
        let surfaced = match (pending.apply_attempt, reconcile) {
            (Some(attempt), Some(facts)) => self.finish_async_native_update_handoff(
                attempt,
                facts,
                format!("overlap handoff failed safely: {detail}"),
                activity_revoked,
            ),
            (None, _) => Some(crate::native_app::UpdateOutcome::Failed {
                message: format!("debug overlap handoff failed safely: {detail}"),
            }),
            (Some(attempt), None) => Some(self.abort_reaped_native_apply_before_reconcile(
                &attempt,
                format!("overlap handoff failed safely: {detail}"),
            )),
        };
        if let Some(surfaced) = surfaced {
            self.surface_update_apply_outcome("automatic handoff", surfaced, false);
        }
        // The child process group is reaped and overlap rollback has run. Only now
        // may the event-loop lane replay destructive intent. A whole-app request
        // dominates individual closes; AppKit generation ownership is preserved.
        match teardown {
            crate::DeferredHandoffTeardown::None => {
                let _ = crate::menu::cancel_current_native_termination();
            }
            crate::DeferredHandoffTeardown::Mutations(mutations) => {
                let _ = crate::menu::cancel_current_native_termination();
                let closing_windows: std::collections::BTreeSet<_> = mutations
                    .iter()
                    .filter_map(|mutation| match mutation {
                        crate::DeferredHandoffMutation::CloseWindow(window) => Some(*window),
                        _ => None,
                    })
                    .collect();
                let closing_tabs: std::collections::BTreeSet<_> = mutations
                    .iter()
                    .filter_map(|mutation| match mutation {
                        crate::DeferredHandoffMutation::CloseTab { window, tab } => {
                            Some((*window, *tab))
                        }
                        _ => None,
                    })
                    .collect();
                let mut replay_close_windows = closing_windows.clone();
                for mutation in mutations {
                    match mutation {
                        crate::DeferredHandoffMutation::ExitSession(session) => {
                            replay_close_windows.extend(self.exit_session_logical(session));
                        }
                        crate::DeferredHandoffMutation::CloseView { window, tab, view }
                            if !closing_windows.contains(&window)
                                && !closing_tabs.contains(&(window, tab)) =>
                        {
                            if let Some(window) =
                                self.replay_deferred_handoff_view_close(window, tab, view)
                            {
                                replay_close_windows.insert(window);
                            }
                        }
                        crate::DeferredHandoffMutation::CloseTab { window, tab }
                            if !closing_windows.contains(&window) =>
                        {
                            if self.replay_deferred_handoff_tab_close(window, tab) {
                                replay_close_windows.insert(window);
                            }
                        }
                        crate::DeferredHandoffMutation::CloseWindow(_)
                        | crate::DeferredHandoffMutation::CloseView { .. }
                        | crate::DeferredHandoffMutation::CloseTab { .. } => {}
                    }
                }
                for window in replay_close_windows {
                    self.close_window(el, window);
                }
            }
            crate::DeferredHandoffTeardown::QuitRequested => {
                let _ = crate::menu::cancel_current_native_termination();
                self.on_quit_requested(el);
            }
            crate::DeferredHandoffTeardown::NativeTerminate { generation } => {
                self.on_native_terminate_requested(el, generation);
            }
            crate::DeferredHandoffTeardown::CleanQuitReady => {
                // The rollback worker may have taken long enough for a native
                // view to acquire new unsaved work after the document barrier's
                // completion. Revalidate at the actual exit edge and surface the
                // reducer's recovery payload instead of replaying a stale Ready.
                match self.prepare_quit_native_shutdown() {
                    Ok(true) => {
                        let _ = crate::menu::complete_current_native_termination();
                        el.exit();
                    }
                    Ok(false) => {
                        let _ = crate::menu::cancel_current_native_termination();
                    }
                    Err(error) => {
                        aterm_log::warn!("deferred clean-quit native barrier: {error}");
                        let _ = crate::menu::cancel_current_native_termination();
                    }
                }
            }
        }
    }

    #[cfg(not(unix))]
    pub(crate) fn finish_update_handoff(
        &mut self,
        _el: &ActiveEventLoop,
        _completion: crate::UpdateHandoffCompletion,
    ) {
        aterm_log::warn!("ignored unix-only handoff completion on this platform");
    }

    /// OVERLAP-HANDOFF failure rollback: restore this (still-running) parent to
    /// a fully-working terminal after a child that never became ready. The
    /// caller has already KILLED + reaped the child (kill-before-resume — see
    /// the call site), so exactly zero readers exist when ours restart:
    /// * the worker has already retired attempt artifacts and republished the
    ///   parent socket link before emitting this completion;
    /// * re-arm CLOEXEC on every master (the exact pre-apply fd posture);
    /// * resume the parked readers ([`Self::attach_deferred_readers`] — parked
    ///   sessions are self-describing: `reader_join: None`);
    ///
    /// The event-loop half performs no filesystem I/O, update-status read, or
    /// child-process probe.
    #[cfg(unix)]
    fn rollback_overlap(&mut self, _nonce: Option<&str>, live: &[(u64, i32, i32)]) {
        for (_, master, _) in live {
            let _ = aterm_pty::set_cloexec(*master, true);
        }
        self.resume_deferred_readers_nonblocking();
    }
}

#[cfg(unix)]
#[must_use]
fn readiness_proof_matches(
    expected: crate::seamless::AdoptionProof,
    wire: &[u8; crate::seamless::READY_WIRE_LEN],
) -> bool {
    crate::seamless::AdoptionProof::from_wire(wire) == Some(expected)
}

/// Block (bounded) until the overlap-handoff child signals readiness, closes its
/// proof pipe, or times out. Crucially this never calls `Child::try_wait`: that
/// API reaps an exited group leader and destroys the PID identity before the
/// unique reaper can signal all descendants. Proof EOF detects pre-ready death;
/// after a full proof, an exited child is rejected by the atomic Commit pipe
/// write (EPIPE). The deadline defaults to 15 s — generous against a
/// staged-swap re-exec + GPU init + multi-window present (~1-2 s observed) —
/// and is tunable via `ATERM_HANDOFF_READY_TIMEOUT_MS` for the QA seam.
///
/// SEAMLESS: `masters` are watched for session DEATH (HUP/ERR/NVAL →
/// `Rejected` — the adoption proof's live-set identity is stale) but NOT for
/// readable output — shell bytes produced while the child boots wait in the
/// kernel queues and replay through the child's fresh parser after Commit, so
/// output during the overlap must never abort the wait. A cancel poke returns
/// the typed `ActivityRevoked` so the retry budget can classify it.
/// The three mutually exclusive verdicts on one `poll` answer during the ready
/// wait. Split out from the loop so the anti-spin contract is provable without
/// timing: queued master output must land on [`ReadyPollAction::NoProgress`]
/// (the yielding branch), NEVER on a path that re-polls immediately.
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReadyPollAction {
    /// A handed-off master reported HUP/ERR/NVAL — the adopted live-set identity
    /// is stale; reject.
    SessionDied,
    /// The proof fd is readable (POLLIN or its own HUP/ERR/NVAL) — read the wire.
    ReadProof,
    /// Neither: `poll` returned because a master had queued readable output
    /// (plain POLLIN — deliberately not an abort) or a bare slice. No progress
    /// toward readiness, so the caller must YIELD before re-polling; a master
    /// that stays readable would otherwise make `poll` return instantly forever.
    NoProgress,
}

/// Classify one `poll` answer. Death on ANY master dominates (the proof is
/// meaningless if the adopted set is already stale); otherwise a readable proof
/// fd is progress; otherwise there is nothing to do but yield. `pollfds[0]` is
/// the proof fd, `pollfds[1..]` the watched masters (see [`wait_handoff_ready`]).
#[cfg(unix)]
#[must_use]
fn classify_ready_poll(pollfds: &[libc::pollfd]) -> ReadyPollAction {
    let dead = libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
    if pollfds.iter().skip(1).any(|pfd| pfd.revents & dead != 0) {
        return ReadyPollAction::SessionDied;
    }
    let proof_readable = libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
    if pollfds
        .first()
        .is_some_and(|proof| proof.revents & proof_readable != 0)
    {
        return ReadyPollAction::ReadProof;
    }
    ReadyPollAction::NoProgress
}

#[cfg(unix)]
fn wait_handoff_ready(
    rd: &std::os::fd::OwnedFd,
    expected: crate::seamless::AdoptionProof,
    cancel: &std::sync::mpsc::Receiver<()>,
    masters: &[i32],
) -> crate::UpdateHandoffOutcome {
    use std::os::fd::AsRawFd as _;
    let timeout_ms = std::env::var("ATERM_HANDOFF_READY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(15_000)
        .clamp(1_000, 120_000);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let mut wire = [0u8; crate::seamless::READY_WIRE_LEN];
    let mut offset = 0usize;
    let mut pollfds = Vec::with_capacity(masters.len().saturating_add(1));
    pollfds.push(libc::pollfd {
        fd: rd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    });
    // Masters are watched for DEATH only: POLLIN must be REQUESTED (macOS
    // evaluates a PTY's stream state only for requested events and reports a
    // dead slave as POLLIN|POLLHUP; with `events: 0` it reports nothing) but
    // plain POLLIN in the answer is IGNORED below — queued shell output waits
    // in the kernel for the child and must not abort the wait.
    pollfds.extend(masters.iter().copied().map(|fd| libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }));
    loop {
        if cancel.try_recv().is_ok() {
            return crate::UpdateHandoffOutcome::ActivityRevoked;
        }
        if std::time::Instant::now() >= deadline {
            return crate::UpdateHandoffOutcome::TimedOut;
        }
        for pfd in &mut pollfds {
            pfd.revents = 0;
        }
        // SAFETY: a stable initialized pollfd slice; 10 ms bounds death abort.
        let n = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, 10) };
        if n <= 0 {
            continue; // timeout slice or EINTR — re-check child + deadline (poll already blocked)
        }
        match classify_ready_poll(&pollfds) {
            ReadyPollAction::SessionDied => return crate::UpdateHandoffOutcome::Rejected,
            ReadyPollAction::NoProgress => {
                // A booting child's shell can produce queued output on a handed-
                // off master (the tolerate-output contract). That master answers
                // plain POLLIN, which makes `poll` return IMMEDIATELY every
                // iteration but matches neither the dead mask nor the proof fd —
                // so without this yield the loop would busy-spin at 100% CPU,
                // starving the very child we are waiting on. The same ~2 ms sleep
                // the post-ProofReady decision loop uses bounds the spin while
                // staying an order of magnitude tighter than the ~1-2 s boot the
                // proof arrives after.
                std::thread::sleep(std::time::Duration::from_millis(2));
                continue;
            }
            ReadyPollAction::ReadProof => {}
        }
        // SAFETY: bounded read into the unfilled suffix of a fixed local wire.
        let r = unsafe {
            libc::read(
                rd.as_raw_fd(),
                wire[offset..].as_mut_ptr().cast(),
                wire.len() - offset,
            )
        };
        match r {
            n if n > 0 => {
                offset += usize::try_from(n).unwrap_or(0);
                if offset != wire.len() {
                    continue;
                }
                if !readiness_proof_matches(expected, &wire) {
                    return crate::UpdateHandoffOutcome::AdoptionMismatch;
                }
                // Recheck AFTER the complete proof. A child that raced death with
                // the final bytes is not an exit authority.
                if cancel.try_recv().is_ok() {
                    return crate::UpdateHandoffOutcome::ActivityRevoked;
                }
                return crate::UpdateHandoffOutcome::ProofReady;
            }
            // EOF: the only live write end was the child's, so it dropped the fd
            // (a failed validation) or died/malformed mid-proof.
            0 => return crate::UpdateHandoffOutcome::ChildDied,
            _ => continue, // EINTR/EAGAIN — re-loop
        }
    }
}

#[cfg(all(test, unix))]
mod handoff_process_group_tests {
    use super::{
        HandoffCommitFacts, HandoffRejectDelivery, ReadyPollAction, classify_ready_poll,
        deliver_handoff_rejection, emergency_kill_and_reap_handoff_child, handoff_commit_admitted,
        handoff_masters_closed, handoff_masters_have_activity, kill_and_reap_handoff_child,
        make_cloexec_pipe, wait_handoff_ready, worker_claim_handoff_reaper,
    };
    use std::io::{BufRead as _, Read as _};
    use std::os::unix::process::CommandExt as _;

    /// Panic-safe ownership for fixtures that deliberately leave a 30-second
    /// descendant alive while assertions exercise reaper ordering.
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

    fn assert_process_group_gone(leader: i32, descendant: i32, message: &str) {
        // Waits for launchd to REAP an orphaned descendant — work this process does not
        // control and cannot hurry. The old wait also busy-spun on `yield_now()` at
        // 100% CPU, delaying the very reaping it waited for. A real regression leaves
        // the descendant alive indefinitely, so this is a failure bound.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let group_gone = unsafe { libc::kill(-leader, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            let descendant_gone = unsafe { libc::kill(descendant, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if group_gone && descendant_gone {
                return;
            }
            if std::time::Instant::now() >= deadline {
                // Fail clean: no intentionally long-lived fixture escapes even
                // when the assertion is proving a real production regression.
                unsafe {
                    libc::kill(-leader, libc::SIGKILL);
                    libc::kill(descendant, libc::SIGKILL);
                }
                panic!("{message}");
            }
            // sleep, not yield_now(): yielding keeps this thread RUNNABLE, so on an
            // oversubscribed box it competes with the very work it is waiting for.
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn final_handoff_admission_conforms_to_every_model_guard() {
        let all = HandoffCommitFacts {
            exact_sessions: true,
            exact_layout: true,
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

        // Process-local egress (paste-order FIFO / sink spill) is one concrete
        // queue BEHIND the spec model's single "deliver queued input to the
        // masters" step, so it is fenced by direct admission assertion rather
        // than a model-fired action: an unflushed egress buffer must block
        // Commit exactly like an undrained OS input queue.
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
            let model = aterm_spec::derive::native_update_overlap_handoff_model();
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

        let model = aterm_spec::derive::native_update_overlap_handoff_model();
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

    /// Seamless seam 2, model level: queued PTY output and queued-then-drained
    /// hardware input BUFFER THROUGH the overlap — Commit stays reachable with
    /// output queued the whole way, while an UNDRAINED OS input queue parks
    /// (never fails) Commit until the drain action delivers it to the masters.
    #[test]
    fn queued_output_and_drained_input_buffer_through_commit_in_the_model() {
        let model = aterm_spec::derive::native_update_overlap_handoff_model();
        let mut state = model.init_state();
        for action in [
            "ParkParentReaders",
            "SpawnReaderlessChild",
            "ChildPaintsExactProof",
            "PtyOutputQueues",
            "QueueHardwareInput",
        ] {
            assert!(model.fire(action, &mut state), "{action}: {state:?}");
        }
        // Undispatched hardware input PARKS Commit (it would die with _exit)…
        assert!(
            model.successors("MainWinsCommitArbiter", &state).is_empty(),
            "commit admitted with an undrained OS input queue: {state:?}"
        );
        // …but is not a failure: the reject arbiter has no authority either.
        assert!(
            model
                .successors("WorkerWinsRejectArbiter", &state)
                .is_empty(),
            "queued input must defer, not fail, the attempt: {state:?}"
        );
        for action in [
            "DrainQueuedHardwareInput",
            "MainWinsCommitArbiter",
            "CommitModern",
            "ReleaseModernReaders",
        ] {
            assert!(model.fire(action, &mut state), "{action}: {state:?}");
        }
        assert_eq!(state["commit"], 1);
        assert_eq!(state["child_readers"], 1);
        assert_eq!(
            state["pty_output_queued"], 1,
            "the committed run carried queued output the whole way — the child drains it"
        );
    }

    /// Seamless seam 2, descriptor level: readable bytes on a handed-off
    /// master are visible to the PRE-PARK admission peek but invisible to the
    /// mid-flight death peek; closing the peer flips the death peek without
    /// consuming the queued bytes (they remain for the child).
    #[test]
    fn queued_output_is_buffered_mid_flight_but_peer_death_still_revokes() {
        // A REAL PTY pair — the deployed descriptor shape — not a pipe:
        // poll(2)'s HUP semantics differ between the two, and the death peek's
        // contract is stated for masters.
        let (mut master, mut slave) = (-1i32, -1i32);
        // SAFETY: openpty(3) into two valid out-slots; no termios/winsize.
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0, "openpty");
        let live = [(1u64, master, 4242i32)];

        assert!(!handoff_masters_have_activity(&live), "quiet pty is quiet");
        assert!(!handoff_masters_closed(&live), "live peer is not dead");

        // "Shell output" queued in the kernel for the (future) child.
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

        aterm_pty::close_fd(slave);
        assert!(
            handoff_masters_closed(&live),
            "peer death must still revoke — the live-set identity is stale"
        );
        aterm_pty::close_fd(master);
    }

    /// Seamless seam 2, worker level: the ready wait completes to `ProofReady`
    /// while a handed-off master has queued readable output, and a cancel poke
    /// is typed `ActivityRevoked` (the retry-budget classification), never a
    /// generic rejection.
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

        // Master with queued output the whole time.
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
            wait_handoff_ready(&proof_rd, expected, &cancel_rx, &[master_rd]),
            crate::UpdateHandoffOutcome::ProofReady,
            "queued shell output must not abort the ready wait"
        );

        // Cancel is the typed activity outcome.
        let (proof_rd, _proof_wr) = make_cloexec_pipe().expect("second proof pipe");
        let (cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
        cancel_tx.try_send(()).expect("queue cancel poke");
        assert_eq!(
            wait_handoff_ready(&proof_rd, expected, &cancel_rx, &[master_rd]),
            crate::UpdateHandoffOutcome::ActivityRevoked,
            "cancel must carry the typed activity classification"
        );

        aterm_pty::close_fd(master_rd);
        aterm_pty::close_fd(master_wr);
    }

    /// S0 ANTI-SPIN: the ready-wait poll classifier must route a master that is
    /// merely READABLE (queued shell output — the exact condition the tolerate-
    /// output contract newly allows) to `NoProgress`, the branch that YIELDS
    /// before re-polling. A plain-POLLIN master answer that fell through to an
    /// immediate `continue` is what pegged a core; proving it lands on the
    /// yielding verdict (and never on `ReadProof`/`SessionDied`) is the fix's
    /// standing guard, with none of a CPU/timing assertion's flakiness.
    #[test]
    fn ready_poll_classifies_queued_master_output_as_a_yield() {
        let pfd = |revents: libc::c_short| libc::pollfd {
            fd: -1,
            events: libc::POLLIN,
            revents,
        };
        // Proof idle, master carrying plain queued output → YIELD (the spin
        // condition). Before the fix this fell through to a sleepless continue.
        assert_eq!(
            classify_ready_poll(&[pfd(0), pfd(libc::POLLIN)]),
            ReadyPollAction::NoProgress,
            "queued master output must yield, never busy-spin"
        );
        // A bare wake with nothing readable is also just a yield.
        assert_eq!(
            classify_ready_poll(&[pfd(0), pfd(0)]),
            ReadyPollAction::NoProgress
        );
        // Death on a master dominates — even while it also reports readable
        // bytes (macOS reports a dead slave as POLLIN|POLLHUP).
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
        // A readable proof is progress — even with queued output alongside it.
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
            .arg("-c")
            .arg("sleep 30 & printf '%s\\n' \"$!\"; wait")
            .stdout(std::process::Stdio::piped());
        // SAFETY: async-signal-safe setpgid only; identical to the production
        // update child setup and executed after fork/before exec.
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

        kill_and_reap_handoff_child(&mut child);
        cleanup.disarm();
        assert_process_group_gone(
            leader,
            descendant,
            "kill/reap returned but a descendant process-group member survived",
        );
    }

    #[test]
    fn pre_ready_eof_preserves_leader_identity_until_normal_group_reap() {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("sleep 30 >/dev/null 2>&1 & printf '%s\\n' \"$!\"; exit 0")
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

        // Tier-1 projection: EOF observes an exited direct leader while retaining
        // its waitable identity and a live same-group descendant. Rollback is not
        // enabled until the shipping reaper has signaled that group and waited the
        // direct child, in that order.
        let model = aterm_spec::derive::native_update_overlap_handoff_model();
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

        let (proof_rd, proof_wr) = make_cloexec_pipe().expect("proof pipe");
        drop(proof_wr); // exact pre-ready child-death signal: proof EOF
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
            wait_handoff_ready(&proof_rd, expected, &cancel_rx, &[]),
            crate::UpdateHandoffOutcome::ChildDied,
            "proof EOF detects death without reaping the group leader"
        );

        let arbiter = crate::HandoffAttemptArbiter::new();
        assert!(worker_claim_handoff_reaper(&arbiter));
        assert!(model.fire("WorkerWinsRejectArbiter", &mut model_state));

        // Negative control: the removed wait-before-kill ordering is explicitly
        // executable only in the mutant and immediately violates the ordering
        // property while this real descendant is known live.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut old_order = model_state.clone();
        assert!(buggy.fire("BuggyWaitBeforeGroupSignal", &mut old_order));
        assert!(!buggy.check_invariant("ProcessGroupSignalPrecedesDirectChildReap", &old_order,));
        assert_eq!(old_order["descendant_live"], 1);

        kill_and_reap_handoff_child(&mut child);
        assert!(model.fire("KillRejectedChild", &mut model_state));
        assert!(model.fire("ReapKilledChild", &mut model_state));
        assert!(arbiter.finish_reap(crate::HandoffReaperOwner::Worker));
        cleanup.disarm();
        assert_process_group_gone(
            leader,
            descendant,
            "pre-ready EOF path reaped leader but left its descendant live",
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

    #[test]
    fn emergency_reaper_kills_group_even_after_leader_exits() {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .arg("-c")
            // The descendant does not retain stdout, so EOF proves the leader
            // completed its immediate exit without our test reaping it.
            .arg("sleep 30 >/dev/null 2>&1 & printf '%s\\n' \"$!\"; exit 0")
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
        emergency_kill_and_reap_handoff_child(child.id());
        // The raw emergency reaper consumed this exact child. An explicit wait
        // observes ECHILD and documents the `Child` handle's completed lifecycle.
        let _ = child.wait();
        assert!(arbiter.finish_reap(crate::HandoffReaperOwner::Emergency));
        cleanup.disarm();
        assert_process_group_gone(
            leader,
            descendant,
            "emergency reap returned but exited leader left a live descendant",
        );
    }
}
