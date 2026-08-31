// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Seamless update handoff: `apply_staged_update_now` and the unix overlap
//! worker it starts (`run_handoff_worker`, the bounded readiness wait, the
//! unique-reaper and rejection plumbing), the reap authority that licenses a
//! rollback (`HandoffRollbackWarrant`), the Commit-time admission facts, and
//! `finish_update_handoff`'s drain gate, Commit, and rollback.
//! A verbatim inherent-impl split of `App`.

use winit::event_loop::ActiveEventLoop;

use crate::App;
#[cfg(unix)]
use crate::Wake;
#[cfg(unix)]
use crate::app_input::paste_order;

// THE SEAMLESSNESS INVARIANT, PROVED BY THE COMPILER — see the sibling
// `app_update_handoff_island.rs`, which carries the `clean { … }` island and
// the full account of what it proves and what it does not.
//
// WHY IT IS A SIBLING FILE AND NOT INLINE HERE, which is where it lived until
// 2026-08-12: an island body reaches the Clean parser as a RUST token stream,
// so a compiler without the Clean surface does not skip the island — it fails
// to LEX it, reporting `missing \`enum\` for enum definition` at the `clean`
// keyword. That kills the whole file, and with it every method this module
// defines. `cfg` cannot help inline, because cfg-stripping happens after the
// file is parsed; only a `mod` declaration decides whether a file is READ at
// all. So the island moves behind one, and the gate is `clean_islands` —
// set in .cargo/config.toml on the native triple, which is exactly the set of
// lanes that run the Trust toolchain.
//
// The lanes this rescues are real and were both broken: the release's
// x86_64-apple-darwin compat slice and the Windows cfg-validation build both
// run upstream stable BY DESIGN (Trust has no std for either), and neither
// could compile aterm-gui at all while the island was inline. The proof is not
// weakened by this — it is checked on every native build, which is every build
// that can check it, and a sabotaged protocol still stops the crate compiling.
#[cfg(clean_islands)]
#[path = "app_update_handoff_island.rs"]
mod island;

#[cfg(unix)]
struct HandoffWorkerJob {
    attempt_id: u64,
    /// When the parent STARTED parking its readers — the zero point of the
    /// freeze the user actually feels, and the same instant the 20 ms capture
    /// deadline is measured from. (It is stamped immediately BEFORE
    /// `park_all_readers`, not after, so `park->dial` includes the park itself;
    /// the park is bounded by that same 20 ms, so the inclusion is negligible
    /// against a multi-second dial — but the zero point is the park's start.)
    ///
    /// Carried so the worker can SPLIT the one number the main thread later logs
    /// as park->proof into its two halves at the moment the split becomes
    /// observable (the successor's dial). Data only: nothing branches on it.
    park_at: std::time::Instant,
    current_build: u64,
    target_build: u64,
    target_commit: String,
    /// Run the staged-candidate pre-verification (codesign + sealed rebinding)
    /// as the worker's first action, off the GUI main thread. False for the
    /// same-binary debug re-exec, which has no staged `.app` to authenticate.
    verify_staged_candidate: bool,
    /// This attempt ACTIVATES the installed bundle at the executable's own path
    /// (`ApplyAttemptTicket::is_installed_activation`): the pre-verification runs
    /// against THAT bundle (there is no staged `.app`), and the successor is
    /// handed no expected-artifact triple (it has nothing to swap; it simply IS
    /// the newer build).
    installed_activation: bool,
    command: std::process::Command,
    manifest: crate::session_store::SessionHandoff,
    fds: crate::session_store::HandoffFds,
    screens: Vec<(u64, aterm_core::terminal::TerminalCheckpoint)>,
    window: Option<crate::session_store::WindowCarry>,
    layout: crate::restore::RestoreManifest,
    layout_digest: [u8; 32],
    screen_digest: [u8; 32],
    live: Vec<(u64, i32, i32)>,
    /// The identity triples the adoption proof hashes, which are NOT `live`.
    ///
    /// `live` is transport: real descriptor numbers this process can `poll` and
    /// hand over. The proof's middle term is whatever BOTH sides can compute
    /// independently, and that depends on how the descriptors travel — the fork
    /// lane's `execve` copies the table verbatim so the number itself works,
    /// while `SCM_RIGHTS` installs the receiver's own numbers and the term
    /// becomes the PTY device (`handoff_rendezvous::pty_device_term`). Keeping
    /// the two vectors separate is what stops a lane change from silently
    /// pointing `poll` at a device number.
    proof_identities: Vec<(u64, i32, i32)>,
    /// Which transport this attempt chose, decided on the main thread before
    /// anything parked (see [`out_of_band_lane_refusal`]). macOS-only because
    /// the alternative to forking is macOS-only; every other unix has exactly
    /// one lane and carries no field for it.
    #[cfg(target_os = "macos")]
    lane: HandoffLane,
    /// Boot-trial launches the sentinel had counted for `target_build` BEFORE this
    /// candidate was launched. Forgiveness compares against it, so a candidate
    /// killed before it ever reached `check_boot_health` cannot give back a launch
    /// some earlier, genuinely crashed candidate observed.
    trial_launches_before: u32,
    /// The `.app` ROOT to hand LaunchServices on the out-of-band lane. `None`
    /// whenever this process is not running from a bundle, which is one of the
    /// reasons that lane is refused.
    #[cfg(target_os = "macos")]
    bundle: Option<std::path::PathBuf>,
    cleanup: HandoffWorkerCleanup,
    cancel: std::sync::mpsc::Receiver<()>,
    arbiter: crate::HandoffAttemptArbiter,
    _owned_masters: Vec<std::os::fd::OwnedFd>,
}

/// How this attempt hands its descriptors to the successor.
///
/// The two lanes are not a preference, and this is not a feature flag: the fork
/// lane is the only one that exists on a machine without a bundle, and the
/// out-of-band lane is the only one that produces a successor with a launchd
/// application job of its own (`tests/handoff_launchd_job.rs`). The choice is
/// made ONCE, on the main thread, before any reader parks, because it decides
/// which term the adoption proof hashes — and that term has to be recorded in
/// the pending attempt the main thread will later re-derive the proof from.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandoffLane {
    /// `fork` + `execve`: descriptors travel by inheritance and the proof hashes
    /// the descriptor NUMBER, which both sides agree on because `execve` copies
    /// the table verbatim. Every build in the field speaks this and only this.
    Fork,
    /// LaunchServices + a single-use `SCM_RIGHTS` rendezvous. The successor is
    /// launchd's child, so it gets its own application job; the descriptors
    /// arrive at numbers the receiver's kernel chose, so the proof hashes the
    /// PTY device instead.
    OutOfBand,
}

/// Facts the lane choice is made from. Pure, so the whole decision — including
/// every reason to fall back — is testable without a bundle, a socket or a
/// terminal.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandoffLaneFacts {
    /// This process runs from inside a `.app` whose root we could name. Only a
    /// bundle gets an `application.<bundle-id>.<hex>` job, which is the entire
    /// point of the lane.
    bundled: bool,
    /// A launcher for this platform is compiled in.
    launcher_available: bool,
    /// The composed rendezvous path fits `sun_path` on this machine. It ALMOST
    /// does not — see `handoff_rendezvous`'s module docs for the 16 bytes of
    /// headroom `$HOME` has.
    socket_path_fits: bool,
    /// The authorized `target_build` is at least this build.
    ///
    /// THE ONE GUARD THE RETIRED VERSION ADVERTISEMENT CARRIED. "Presence of the
    /// transport IS the version" settles old-parent/new-child — a parent with no
    /// out-of-band code sends `ATERM_SEAMLESS_FDS` and is answered in v1 — and
    /// says nothing at all about NEW-parent/OLD-child. So the guard moves here,
    /// to the transport choice: an older successor is never handed descriptors
    /// it has no code to receive.
    target_not_older: bool,
    /// Sessions to hand over.
    sessions: usize,
    /// The launch environment can be expressed as a MERGE. A LaunchServices
    /// launch merges over this process's environment and cannot express a
    /// removal, so a command that needs one is not representable on this lane.
    environment_is_a_merge: bool,
}

/// Why this attempt may NOT take the out-of-band lane, or `None` when it may.
///
/// Returning the reason rather than a bare bool is what makes a fallback
/// diagnosable: "the update applied but the survivor is still an orphan" and
/// "the update applied through the new lane" look identical from the outside,
/// and the difference is one of these strings.
#[cfg(target_os = "macos")]
#[must_use]
fn out_of_band_lane_refusal(facts: HandoffLaneFacts) -> Option<&'static str> {
    if !facts.launcher_available {
        return Some("this platform has no LaunchServices launcher");
    }
    if !facts.bundled {
        return Some("this process does not run from a .app bundle");
    }
    if !facts.socket_path_fits {
        return Some("the rendezvous path does not fit sun_path on this machine");
    }
    if !facts.target_not_older {
        return Some("the authorized target build is older than this build");
    }
    if facts.sessions == 0 || facts.sessions > crate::handoff_rendezvous::MAX_RENDEZVOUS_SESSIONS {
        return Some("the session count does not fit one descriptor message");
    }
    if !facts.environment_is_a_merge {
        return Some("the launch environment needs a removal a merge cannot express");
    }
    None
}

/// The `.app` ROOT containing `exe`, when there is one.
///
/// `<bundle>.app/Contents/MacOS/<bin>` — the same two-levels-up shape
/// `menu.rs::bundled_resource` uses for `Contents/Resources`, plus one. The
/// `.app` suffix is CHECKED rather than assumed: `cargo run`, a dev build and
/// the test harness all live three levels below some directory too, and handing
/// LaunchServices one of those would turn a wiring mistake into an opaque
/// launch failure a whole deadline later instead of an immediate fallback.
#[cfg(target_os = "macos")]
#[must_use]
fn app_bundle_root(exe: &std::path::Path) -> Option<std::path::PathBuf> {
    let bundle = exe.parent()?.parent()?.parent()?;
    (bundle
        .extension()
        .is_some_and(|extension| extension == "app")
        && bundle.is_dir())
    .then(|| bundle.to_path_buf())
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
    /// The activity epoch is unchanged since the attempt was armed.
    ///
    /// MODE-BLIND ON PURPOSE, and mandatory in every lane — including
    /// `AutomaticPastGrace`. That lane drops the idle WAIT at the entry; it does
    /// not get to commit against a snapshot that activity has already moved out
    /// from under. (Several comments in this tree used to claim otherwise; they
    /// were corrected on 2026-08-28, and this is the sentence to trust.)
    exact_activity: bool,
    teardown_allows_commit: bool,
    parent_still_parked: bool,
    sessions_alive: bool,
    /// The OS input queue has been DISPATCHED into the masters: the main thread
    /// ran the event loop for a bounded interval after ProofReady, so every
    /// hardware event the OS had already accepted has flowed through the
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

/// The Commit-time layout comparison, reduced to the fields that actually mean
/// "window/tab/pane topology". Two of them are normalized away, and each was
/// independently killing healthy in-flight seamless updates:
///
/// WINDOW POSITION. `capture_restore_manifest` reads `outer_position()` LIVE
/// into `outer_x`/`outer_y`, and `WindowLayout` derives `PartialEq` over them,
/// so simply DRAGGING the window during the successor's boot rejected the
/// Commit as "window/tab/pane topology changed". `WindowEvent::Moved` is
/// classified `Tolerated` in `lib.rs` precisely so a drag can never revoke an
/// overlap, and that classification was being defeated right here. The carried
/// position is NOT re-read at spawn: `start_unix_update_handoff` snapshots the
/// `WindowCarry` once, before the readers park, and the worker ships that
/// snapshot verbatim — so a drag during the boot was never going to follow the
/// window across the swap anyway. The successor reappearing at the pre-drag
/// position is cosmetic; losing the whole update over it was not.
///
/// PER-SESSION cwd/title. The post-park capture is proof-protected — the
/// checkpoint loop above it proved every `term` was lockable — but this
/// re-capture has no such proof: it runs later, on the live event loop, where
/// the scrollback-compression worker can still hold a `term` transiently.
/// `restore_session_meta` degrades to `(None, String::new())` on a `WouldBlock`
/// try_lock, so a contended mutex turned real cwd/title into empty ones and the
/// derived `PartialEq` reported that DEGRADATION as a topology change —
/// nondeterministically, for a session that had not changed at all.
///
/// Comparing the projection rather than probing the locks is the race-free fix:
/// a probe-then-capture would only move the contention window, while full
/// equality implies projection equality, so this can only ever admit
/// differences confined to those degradable metadata fields. Admitting them is
/// safe: the child inherits the live shells (cwd/title matter only to a cold
/// respawn), and the digest the child re-proves is taken over the PENDING
/// layout captured at attempt start, never over this re-capture.
#[cfg(unix)]
fn commit_layout_topology(
    layout: &crate::restore::RestoreManifest,
) -> crate::restore::RestoreManifest {
    fn strip_pane(node: &mut crate::restore::PaneLayout) {
        match node {
            crate::restore::PaneLayout::Leaf { cwd, title, .. } => {
                *cwd = None;
                title.clear();
            }
            crate::restore::PaneLayout::Split { first, second, .. } => {
                strip_pane(first);
                strip_pane(second);
            }
        }
    }

    fn strip_tree(node: &mut crate::restore::RestoredSplitTree) {
        match node {
            crate::restore::RestoredSplitTree::Leaf {
                view: crate::restore::RestoredView::Terminal(terminal),
            } => {
                terminal.cwd = None;
                terminal.title.clear();
            }
            // Native/placeholder leaves carry no session-lock-derived field, so
            // they keep comparing in full — a native tab appearing or changing
            // IS structural. The terminal leaf's USER metadata
            // (`user_title`/`description`/`icon`/`role`/`attention`) is read
            // under a BLOCKING lock in `view_restore_descriptor`, so it cannot
            // degrade and is deliberately left in the comparison too.
            crate::restore::RestoredSplitTree::Leaf { .. } => {}
            crate::restore::RestoredSplitTree::Split { first, second, .. } => {
                strip_tree(first);
                strip_tree(second);
            }
        }
    }

    let mut topology = layout.clone();
    for window in &mut topology.windows {
        window.outer_x = None;
        window.outer_y = None;
        // Same class as the position: live SHOW STATE, not topology. Captures
        // currently write it on Windows only (this fn is unix-gated), but the
        // derived `PartialEq` covers the field, so normalize it here too —
        // otherwise the day a unix capture starts recording it, zooming the
        // window during the successor's boot would kill a healthy Commit
        // exactly the way dragging it used to.
        window.maximized = None;
        // BOTH projections, deliberately: the capture writes the same live
        // session's cwd/title into the legacy `tabs` mirror and the canonical
        // `restored_tabs` tree, so normalizing only one of them would leave the
        // other still reporting a degraded read as a changed layout.
        for tab in &mut window.tabs {
            strip_pane(tab);
        }
        for tab in &mut window.restored_tabs {
            strip_tree(&mut tab.root);
        }
    }
    topology
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
/// thread and the spawn-failed inline fallback: kill the readerless candidate
/// and PROVE it terminated, release the emergency reaper claim, run the worker
/// cleanup, and report the rejected completion back to the event loop. The
/// completion is what licenses rollback, so it is emitted only after the
/// warrant exists.
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
    emergency_kill_and_reap_handoff_child(child_pid).announce(Some(child_pid));
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
            // WE ended this candidate, off a bare pid from the completion wire.
            // Nothing here witnessed a death of its own.
            child_death: crate::ChildDeathEvidence::Unobserved,
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

/// WHY a parked PTY reader may be resumed. Rollback restarts a reader on every
/// handed-off master, so it is sound only while nothing else can still be
/// reading those masters: two readers on one master silently interleave and
/// destroy the stream, which is unrecoverable and invisible. Each variant names
/// a fact that rules the overlap candidate out as such a reader.
///
/// There is deliberately NO variant for "we waited as long as we were willing
/// to". A candidate that MIGHT still be alive is exactly the case rollback must
/// not run in, so the functions below have no give-up path — `Child::wait`, the
/// authority they generalize, has none either.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
enum HandoffRollbackWarrant {
    /// The attempt failed before any candidate was spawned, so nothing outside
    /// this process has ever held a handed-off master.
    NoCandidate,
    /// A successor was LAUNCHED, but the out-of-band transfer never completed —
    /// no descriptor of ours left this process, so the successor cannot be
    /// holding, let alone reading, a handed-off master.
    ///
    /// This is the one warrant that does not rest on the candidate being gone,
    /// and it is sound for a reason the others cannot use: on this lane the
    /// descriptors move in ONE `sendmsg`, so "the send did not happen" is a
    /// complete account of what the successor holds. That is strictly stronger
    /// than what the fork lane can say at the same point, where `execve` has
    /// already copied the table. Nothing is killed or waited on here — the
    /// successor discovers the vanished rendezvous, refuses itself, and exits.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    NeverTransferred,
    /// `wait` consumed the candidate: it terminated and THIS process reaped it.
    /// Available only to its parent, and strictly the best answer — reaping is
    /// what frees the pid, so nothing can recycle it between proof and use.
    Reaped,
    /// We were not the candidate's parent, so no `wait` could answer for it, and
    /// the pid it was born at provably no longer names it. See
    /// [`handoff_candidate_terminated`] for the two proofs and why they
    /// establish the same fact `Reaped` does.
    Vanished,
}

#[cfg(unix)]
impl HandoffRollbackWarrant {
    /// Say which authority licensed a rollback, once, at the moment it is used.
    /// The outside proof is worth a line — it is the difference between "we
    /// reaped the candidate" and "the candidate was never ours to reap", i.e.
    /// which launch shape this build actually ran — while the ordinary parent
    /// reap stays as quiet as it has always been.
    fn announce(self, candidate_pid: Option<u32>) {
        if self == Self::Vanished {
            aterm_log::info!(
                "update apply: rollback licensed by outside proof; candidate {candidate_pid:?} \
                 terminated without being ours to reap"
            );
        }
    }
}

/// The kernel's birth record for whatever process currently occupies a pid: the
/// microsecond instant it was created. The kernel assigns it, so no process can
/// choose its own, and two processes that reuse a pid cannot share one. Compared
/// for EQUALITY only — it is an identity token, never a clock reading.
///
/// `seamless::ProcessBirth` reads the same kernel fact for the OPPOSITE
/// conclusion — "is my handoff parent still ALIVE" — and the two are not
/// negations of each other. An unreadable record must make that probe answer
/// dead (fail-safe there: the readerless successor kills itself) and must make
/// this one answer "not proven" (fail-safe here: the parent keeps its readers
/// parked). One shared probe would put one of the two lanes on the dangerous
/// default, so this lane carries its own with its own failure direction.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandoffCandidateBirth {
    seconds: u64,
    microseconds: u64,
}

/// WHICH process is at `pid` right now — not whether it is alive. Liveness is
/// `kill(2)`'s answer; this is the identity half, and it is deliberately not
/// filtered by process status, because every reason the kernel might decline to
/// answer — including a zombie, which libproc may or may not report — carries
/// the same meaning for this lane: nothing is concluded either way.
#[cfg(target_os = "macos")]
fn read_candidate_birth(pid: u32) -> Option<HandoffCandidateBirth> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    if pid <= 1 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` points at `size` writable bytes of exactly the structure
    // PROC_PIDTBSDINFO fills; libproc returns the number of bytes it wrote.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    // SAFETY: the exact-size success above initialized the whole record.
    let info = unsafe { info.assume_init() };
    // A record about some OTHER pid could only mislead the comparison this
    // feeds, so a kernel that disagrees about the pid it was asked about is no
    // witness at all.
    if u32::try_from(pid).ok()? != info.pbi_pid {
        return None;
    }
    Some(HandoffCandidateBirth {
        seconds: info.pbi_start_tvsec,
        microseconds: info.pbi_start_tvusec,
    })
}

/// Off macOS there is no birth-record primitive wired here, and none is needed:
/// the successor is unconditionally a fork child there (the `spawn` in
/// [`run_handoff_worker`] is the only launch shape, as the matching stub in
/// `seamless::read_process_birth` records), so `wait` always answers and the
/// fallback authority never has to. A non-fork transport off macOS must
/// implement this FIRST — `/proc/<pid>/stat` field 22 is Linux's equivalent —
/// because without it the fallback can prove termination only by pid vacancy.
#[cfg(all(unix, not(target_os = "macos")))]
fn read_candidate_birth(_pid: u32) -> Option<HandoffCandidateBirth> {
    None
}

/// The process that must be proven terminated before any parked reader resumes,
/// plus whatever identity keeps a RECYCLED pid from impersonating it.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandoffCandidate {
    pid: u32,
    /// The kernel birth stamp for `pid`, captured while the pid provably still
    /// named the candidate. `None` never weakens the PROOF (pid vacancy alone is
    /// sound — see [`handoff_candidate_terminated`]); it costs only the two
    /// things identity buys: concluding termination from a recycled pid instead
    /// of waiting for a number that will never come free, and keeping a SIGKILL
    /// off whoever recycled it.
    birth: Option<HandoffCandidateBirth>,
}

/// What the kernel says about the pid the candidate was born at, right now.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandoffCandidateIdentity {
    /// The pid still names the candidate: the kernel's stamp for it equals the
    /// captured one.
    Corroborated,
    /// The pid names a DIFFERENT process. The kernel does not reallocate a pid
    /// before its previous owner is reaped, so the candidate has terminated.
    Recycled,
    /// No stamp was captured, or the kernel will not produce one now. Nothing is
    /// concluded in either direction.
    Unwitnessed,
}

#[cfg(unix)]
impl HandoffCandidate {
    /// Capture the identity of a fork child this process has NOT reaped. The
    /// unreaped entry pins the number — the kernel cannot reallocate a pid a
    /// zombie still owns — so the stamp read here is provably that child's own.
    fn of_unreaped_child(child: &std::process::Child) -> Self {
        let pid = child.id();
        Self {
            pid,
            birth: read_candidate_birth(pid),
        }
    }

    /// Capture the identity of a candidate this process did NOT fork, from the
    /// pid the kernel attested at the rendezvous accept (`LOCAL_PEERPID`).
    ///
    /// The pid is not PINNED the way an unreaped child's is — launchd may reap
    /// the successor at any moment and free the number — so the stamp read here
    /// is what turns a recyclable integer back into an identity. Strictly better
    /// than [`Self::from_bare_pid`], which is what this lane would otherwise be
    /// reduced to, and the reason `signal_handoff_candidate` may aim a DIRECT
    /// signal at a launched candidate at all.
    #[cfg(target_os = "macos")]
    fn of_attested_peer(pid: u32) -> Self {
        Self {
            pid,
            birth: read_candidate_birth(pid),
        }
    }

    /// A candidate known only by its pid. This is what the emergency reaper gets:
    /// the completion wire carries `child_pid`, a bare number, so no stamp can
    /// ride along with it. Sound — vacancy is what proves termination — but it
    /// cannot conclude termination FROM a recycled pid, and it signals exactly
    /// as the pre-0.14 code did (the process group only).
    fn from_bare_pid(pid: u32) -> Self {
        Self { pid, birth: None }
    }

    fn identity(self) -> HandoffCandidateIdentity {
        match (self.birth, read_candidate_birth(self.pid)) {
            (Some(captured), Some(current)) if captured == current => {
                HandoffCandidateIdentity::Corroborated
            }
            (Some(_), Some(_)) => HandoffCandidateIdentity::Recycled,
            _ => HandoffCandidateIdentity::Unwitnessed,
        }
    }
}

/// Has the candidate TERMINATED — can it no longer be holding, let alone
/// reading, the handed-off PTY masters?
///
/// Two independent proofs, each about the candidate itself:
///
/// * PID VACANCY. `kill(pid, 0)` answering ESRCH means the kernel will deliver
///   nothing at that number. A running process's pid never changes, so the only
///   way to get that answer about the candidate is for the candidate to have
///   terminated — and a terminated process runs no further user code and holds
///   no descriptors. pid REUSE can only HIDE this answer (somebody else now
///   answers to the number), never fabricate it, so this proof needs no identity
///   check to be sound.
/// * IDENTITY. Something does answer at the pid, but the kernel's birth stamp
///   for it disagrees with the candidate's. A pid is not reallocated until its
///   previous owner has been reaped, so the candidate terminated.
///
/// Both therefore establish what `Child::wait` establishes; they differ only in
/// WHO reaped it. Every other answer — a zombie, an unreadable record, a pid
/// this build cannot convert — is UNPROVEN, and unproven never resumes a reader.
#[cfg(unix)]
#[must_use]
fn handoff_candidate_terminated(candidate: HandoffCandidate) -> bool {
    let Ok(pid) = libc::pid_t::try_from(candidate.pid) else {
        return false;
    };
    // SAFETY: signal 0 performs kill(2)'s existence/permission check only and
    // delivers nothing.
    if unsafe { libc::kill(pid, 0) } != 0
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    {
        return true;
    }
    candidate.identity() == HandoffCandidateIdentity::Recycled
}

/// Does THIS process lead its own process group — the precondition that makes a
/// rejecting parent's `kill(-pid)` reach the updater helpers a candidate forks,
/// and not only the candidate itself?
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessGroupContainment {
    /// The kernel reports `getpgrp() == getpid()`. A child forked from here
    /// inherits that group, so `kill(-pid)` names a group whose every member
    /// descends from this process.
    OwnGroupLeader,
    /// The kernel reports a group this process does not lead. Two things fail at
    /// once: `kill(-pid)` would not reach a helper forked from here (it is in
    /// the OTHER group), and the group it does name belongs to whoever leads it.
    /// Nothing may fork an updater helper from this state. The two numbers are
    /// carried so the refusal can say what the kernel actually answered — the
    /// errno cannot, since it is not what this decision is read from.
    Foreign {
        group: libc::pid_t,
        own: libc::pid_t,
    },
}

/// Put the calling process in a process group of its own and PROVE it, so that
/// helpers it forks later are inside the group a rejecting parent sweeps.
///
/// This is the successor-side twin of the `pre_exec` `setpgid(0, 0)` in
/// [`run_handoff_worker`], for the launch shape that has no pre-exec hook: a
/// successor started through LaunchServices is launchd's child rather than a
/// fork of ours, so no fork-time hook of the parent's can run inside it and the
/// process has to contain ITSELF. `seamless::prearm_incoming_fds` carries the
/// design note that proposed this, but the call lands in [`crate::main_entry`]
/// instead — still ahead of everything in that process able to run another
/// program — because `prearm_incoming_fds` is also exercised IN-PROCESS by unit
/// tests, and a process-wide, irreversible `setpgid` inside it would move the
/// test binary out of its harness's process group. The ordering obligation the
/// call site owes is stated at that call site.
///
/// THE RETURN VALUE OF `setpgid` IS NOT THE ANSWER; `getpgrp()` IS. At this one
/// call shape — target self, requested group "my own pid" — the only failure the
/// macOS contract admits is EPERM "the process indicated by the pid argument is a
/// session leader". Every other documented error is out of reach here: `EACCES`
/// and `ESRCH` need `pid` to name a CHILD, the other two `EPERM` clauses need a
/// different euid or a `pgid` naming somebody else's group, and `EINVAL` needs a
/// negative or unsupported `pgid`, which 0 — "the target's own pid" — is not.
/// That one refusal reports the property already holding rather than denying it,
/// because a session leader has `pgid == sid == pid`: `setsid` sets the three
/// equal, and `setpgid` refusing session leaders is exactly what stops anything
/// from moving one out again. But this function does not rest on that reading, or
/// on any other enumeration of errnos: it reads the postcondition back from the
/// kernel, so an errno this code did not anticipate is caught by the check
/// instead of being argued away.
///
/// Idempotent, which is what lets callers run it unconditionally on their lane:
/// a process already leading its own group gets a second no-op success, and the
/// updater's boot-apply re-exec preserves the process group across `execve`, so
/// the re-exec'd image re-running this sees the group it established before.
#[cfg(unix)]
#[must_use]
pub(crate) fn contain_own_process_group() -> ProcessGroupContainment {
    // SAFETY: `setpgid(0, 0)` acts on the calling process only; `getpgrp` and
    // `getpid` are side-effect-free getters. The `setpgid` result is discarded
    // deliberately — what this function answers with is the postcondition read
    // back from the kernel immediately after it, for the reason stated above.
    let (group, own) = unsafe {
        let _ = libc::setpgid(0, 0);
        (libc::getpgrp(), libc::getpid())
    };
    if group == own {
        ProcessGroupContainment::OwnGroupLeader
    } else {
        ProcessGroupContainment::Foreign { group, own }
    }
}

/// SIGKILL the candidate, before anything waits on it.
///
/// The GROUP sweep (`-pid`) is the pre-existing behaviour and the reason
/// `pre_exec` puts the candidate in a group of its own: it is what stops the
/// candidate's ditto/codesign/spctl descendants from continuing to mutate fixed
/// updater paths after the leader is gone.
///
/// WHICH CONTAINMENT THE SWEEP RELIES ON — the two are not equally strong, and a
/// reader must not assume the second one is the first:
///
/// * A CANDIDATE WE FORKED (today's `spawn`). `run_handoff_worker`'s `pre_exec`
///   `setpgid(0, 0)` runs between fork and exec, so the candidate leads its own
///   group BEFORE its image runs: `-pid` is a valid handle from the instant
///   `spawn` returns, and there is provably no instant at which a helper of the
///   candidate's exists outside that group.
/// * A CANDIDATE WE DID NOT FORK (the LaunchServices lane B3 exists for). The
///   candidate contains ITSELF with [`contain_own_process_group`] on entry,
///   before its own update logic can fork the first helper, and refuses to
///   continue when it cannot — so the "no helper outside the group" property is
///   the same one. What is NOT the same is our knowledge of it: the readiness
///   wire is a fixed proof record with no field for a process-group id, so this
///   process cannot distinguish "the successor contained itself" from "the
///   successor never reached that instruction". On that lane `-pid` is an
///   UNPROVEN sweep and nothing may be concluded from it; what licenses rollback
///   is [`handoff_candidate_terminated`], never the group signal. Carrying an
///   attested pgid is B4's control-socket work.
///
/// The DIRECT signal is what a candidate this process did not fork needs. Such a
/// candidate may not be a group leader at all, and then `-pid` names no group and
/// sweeps nothing. It is withheld unless the identity is CORROBORATED, because a
/// bare pid that has been recycled names a stranger and this lane must never
/// SIGKILL one. With no witness the behaviour is exactly what it has always been:
/// the group sweep alone, aimed at a pid that today's unreaped fork child keeps
/// pinned. That PIN is what keeps an unwitnessed sweep aimed at us, and it is
/// precisely what a candidate launchd owns lacks — once launchd reaps it the
/// number is free, and `-pid` then names whatever group its new owner leads. So
/// on that lane an unwitnessed sweep is not merely unproven, it is unsafe, and
/// the candidate has to arrive with an identity (B2/B4) rather than as a bare
/// pid.
/// Is `pid` still a CHILD of this process, and therefore pinned to its number?
///
/// `false` is the safe direction: it only ever withholds a signal. A child that
/// has already EXITED is still pinned — an unreaped zombie owns its number — so
/// both affirmative answers of [`probe_handoff_candidate`] count here; only
/// `ECHILD` withholds.
#[cfg(unix)]
fn candidate_is_our_child(pid: libc::pid_t) -> bool {
    matches!(
        probe_handoff_candidate(pid),
        HandoffCandidateProbe::Running | HandoffCandidateProbe::Exited
    )
}

/// What this process can say about a candidate WITHOUT changing anything.
///
/// One `waitid(WNOHANG | WNOWAIT)` answers two questions, and both have a caller:
///   * IS IT OURS — `ECHILD` is the discriminator, and measured on this platform
///     it is what both a launchd-owned process and an already-reaped one answer.
///     [`candidate_is_our_child`] needs it before a process-group sweep;
///   * HAS IT DIED YET — [`worker_reject_and_reap_handoff_child`] needs it to know
///     whether the status it is about to reap belongs to the CANDIDATE or to the
///     SIGKILL it is about to send.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandoffCandidateProbe {
    /// `waitid` refused: not a child of ours (the launched lane), or already
    /// reaped. Nothing may be concluded, and nothing is pinned.
    NotOurChild,
    /// Our child, and it has not exited: whatever ends it has not happened yet.
    ///
    /// NOT "IT REFUSED", however tempting. A candidate observed alive at proof EOF
    /// really has closed the readiness channel without dying — which is what a
    /// deliberate refusal does — but it is also what a process being torn down
    /// looks like for the microseconds between the kernel closing its descriptors
    /// and its zombie state becoming visible. Under exactly the pathological load
    /// this classification exists for, a dying candidate is likelier to be caught
    /// in that window, so reading `Running` as a refusal would put a race back at
    /// the seam a race already broke once.
    Running,
    /// Our child, already exited, and still waitable — so its status is intact for
    /// the reap that follows.
    Exited,
}

/// NON-DESTRUCTIVE BY CONSTRUCTION. `WNOWAIT` leaves an already-exited child in
/// its waitable state, so the reaper's own `wait` still collects it — and still
/// collects the status this probe deliberately does not read.
///
/// `si_signo` rather than `si_pid`/`si_status` is the portability decision: it is
/// a PUBLIC FIELD of libc's `siginfo_t` on both platforms this crate builds for,
/// while the other two are fields on the BSDs and union accessors on Linux. A
/// child that has not changed state leaves the zeroed struct untouched, so
/// `si_signo == 0`; a reportable exit writes `SIGCHLD` there, which is non-zero
/// everywhere. The exit STATUS is read later, in safe Rust, from the
/// `Child::wait` this lane already performs.
#[cfg(unix)]
fn probe_handoff_candidate(pid: libc::pid_t) -> HandoffCandidateProbe {
    let Ok(id) = libc::id_t::try_from(pid) else {
        return HandoffCandidateProbe::NotOurChild;
    };
    // SAFETY: `info` is a zeroed out-parameter of exactly the type waitid fills,
    // and WNOWAIT means this call consumes no child. Zeroing FIRST is what makes
    // the `si_signo` read below meaningful: POSIX does not require an
    // implementation to write that field when WNOHANG finds nothing to report.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            id,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if rc != 0 {
        return HandoffCandidateProbe::NotOurChild;
    }
    if info.si_signo == 0 {
        HandoffCandidateProbe::Running
    } else {
        HandoffCandidateProbe::Exited
    }
}

/// A NON-PARENT'S WITNESS TO HOW A CANDIDATE DIED.
///
/// THE LAUNCHED LANE HAS NO `wait`, and that is not a gap in this file — it is
/// what "launchd's child" means. `waitid` answers `ECHILD` about somebody else's
/// child, so on the SHIPPING macOS lane the parent could observe nothing whatever
/// about a `ChildDied` and every one of them had to be classified from inference.
/// That is precisely how a starved candidate came to be charged as a refusing
/// successor (see [`crate::ChildDeathEvidence`]).
///
/// DARWIN HAS A SECOND CHANNEL FOR EXACTLY THIS FACT. `kqueue`'s `EVFILT_PROC`
/// with `NOTE_EXIT | NOTE_EXITSTATUS` delivers a process's FULL `wait(2)`-encoded
/// status to a watcher that is not its parent. XNU's `filt_procattach` gates
/// `NOTE_EXITSTATUS` on being the parent, the tracer, OR being permitted to
/// `SIGKILL` the target — and this lane SIGKILLs the candidate a few lines later,
/// so the permission it needs is one it demonstrably already holds.
///
/// MEASURED ON THIS PLATFORM (Darwin 25.5.0) against a process deliberately
/// reparented to launchd, i.e. the exact shape of a LaunchServices-launched
/// successor, watched by a process that is not its parent:
///   * `exit(7)` → `fflags` carries `NOTE_EXIT|NOTE_EXITSTATUS` and `data` is
///     `0x0700`: `WIFEXITED`, code 7;
///   * `SIGKILL` → `data` is `0x9`: `WIFSIGNALED`, signal 9.
///
/// Registration succeeded on the orphan in both cases. The failing direction is a
/// registration `ESRCH` — the candidate is already gone — which answers `None` and
/// leaves the verdict exactly where it was before this type existed.
///
/// TWO PROPERTIES MAKE IT STRICTLY BETTER THAN [`probe_handoff_candidate`], not a
/// substitute for it:
///   * THE EVENT IS DURABLE. XNU queues the knote inside `proc_exit` and it stays
///     in THIS process's queue until read, so launchd reaping the candidate cannot
///     take the fact away. A zombie probe loses the same fact to a reap it does
///     not control.
///   * ASKING LATE COSTS NOTHING. So [`observe_candidate_death`] reads it once
///     BEFORE its own SIGKILL — anything there is unambiguously the candidate's
///     own death — and again after the candidate is provably gone, which is sound
///     for every status except a bare `SIGKILL`, the only signal this process
///     ever sends.
#[cfg(target_os = "macos")]
struct CandidateExitWatch {
    kq: std::os::fd::OwnedFd,
    ident: libc::uintptr_t,
}

#[cfg(target_os = "macos")]
impl CandidateExitWatch {
    /// Register WHILE THE CANDIDATE IS STILL ALIVE. On the launched lane that is
    /// the rendezvous accept — the one instant the kernel has just attested the
    /// pid — and the registration is the last thing that has to happen before the
    /// candidate can start dying.
    ///
    /// Every failure answers `None`, and every failure is SAFE: it costs the
    /// evidence, never the reap, and the classification degrades to
    /// [`crate::ChildDeathEvidence::Unobserved`], which retries on a bounded
    /// budget.
    fn watch(pid: u32) -> Option<Self> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        let pid = libc::pid_t::try_from(pid).ok()?;
        // 0, -1 and launchd are `kill`'s special targets and the init process;
        // none is ever a candidate, and none is a process to attach a filter to.
        if pid <= 1 {
            return None;
        }
        let ident = libc::uintptr_t::try_from(pid).ok()?;
        // SAFETY: `kqueue()` takes no arguments and returns a new descriptor or -1.
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return None;
        }
        // SAFETY: `raw` is a fresh descriptor this process exclusively owns.
        let kq = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        // CLOSE-ON-EXEC BY HOUSE RULE RATHER THAN BY NECESSITY: Darwin does not
        // inherit a kqueue across `fork` at all, so nothing this process spawns can
        // carry it and the non-atomic gap after `kqueue()` cannot leak. Set it
        // anyway, so an audit of "which descriptors can leave this process" needs
        // no platform footnote.
        // SAFETY: `F_SETFD` takes an int and touches only this descriptor's flags.
        unsafe { libc::fcntl(kq.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        let change = libc::kevent {
            ident,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ENABLE,
            fflags: libc::NOTE_EXIT | libc::NOTE_EXITSTATUS,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // NO OUTPUT SLOT, ON PURPOSE. With `nevents == 0` a registration failure
        // comes back through `kevent`'s own return value instead of being buried in
        // an `EV_ERROR` event, which is what keeps "the candidate is already gone"
        // from being confused with "the candidate exited while we were registering".
        // SAFETY: one change entry, live for the call; no event list is requested.
        let rc = unsafe {
            libc::kevent(
                kq.as_raw_fd(),
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        (rc == 0).then_some(Self { kq, ident })
    }

    /// The candidate's own `wait(2)` status IF THE KERNEL HAS ALREADY RECORDED
    /// ONE. Never blocks (zero timeout) and never reaps — the candidate is not
    /// this process's child to reap.
    fn exit_status(&self) -> Option<std::process::ExitStatus> {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::process::ExitStatusExt as _;
        // SAFETY: a zeroed out-parameter of exactly the type `kevent` fills.
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: one event slot and one timespec, both live for the call; no
        // changes are submitted.
        let rc = unsafe {
            libc::kevent(
                self.kq.as_raw_fd(),
                std::ptr::null(),
                0,
                &mut event,
                1,
                &timeout,
            )
        };
        if rc != 1 || event.flags & libc::EV_ERROR != 0 {
            return None;
        }
        // ABOUT THE RIGHT PROCESS, AND ABOUT AN EXIT. Neither can currently be
        // otherwise — one registration, one filter — and both are cheap to keep
        // true if a second registration is ever added to this queue.
        if event.ident != self.ident
            || event.fflags & libc::NOTE_EXIT == 0
            || event.fflags & libc::NOTE_EXITSTATUS == 0
        {
            return None;
        }
        // The status is the low 32 bits of `data`, in the same `wait(2)` encoding
        // `Child::wait` yields. MEASURED: `exit(7)` gives `0x0700`, `SIGKILL`
        // gives `0x9`.
        let raw = i32::try_from(event.data & 0xffff_ffff).ok()?;
        Some(std::process::ExitStatus::from_raw(raw))
    }
}

/// Off macOS every handoff candidate is a fork child of this process, so `wait`
/// always answers and there is no lane for a non-parent witness to serve. The
/// type stays so the decision tail is ONE function on every unix; nothing here
/// can produce one, because the `watch` constructor is macOS-only and there is no
/// other.
#[cfg(all(unix, not(target_os = "macos")))]
#[allow(dead_code)]
struct CandidateExitWatch;

#[cfg(all(unix, not(target_os = "macos")))]
impl CandidateExitWatch {
    fn exit_status(&self) -> Option<std::process::ExitStatus> {
        None
    }
}

#[cfg(unix)]
fn signal_handoff_candidate(candidate: HandoffCandidate) {
    let Ok(pid) = libc::pid_t::try_from(candidate.pid) else {
        return;
    };
    // `-pid` is kill(2)'s process-GROUP target and -1 is its BROADCAST target,
    // so a pid below 2 must never reach it.
    if pid <= 1 {
        return;
    }
    match candidate.identity() {
        // Somebody else answers to the number now. There is nothing of ours to
        // signal, and signalling would land on them.
        HandoffCandidateIdentity::Recycled => (),
        HandoffCandidateIdentity::Corroborated => {
            // SAFETY: SIGKILL to the candidate's process group and then to the
            // candidate itself, both against a pid just proven to name it.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
        }
        HandoffCandidateIdentity::Unwitnessed => {
            // A GROUP KILL IS ONLY SOUND WHILE THE PID IS PINNED, and unwitnessed
            // means we cannot tell from the candidate alone. The fork lane pins it:
            // an unreaped child owns its number until it is waited on, so `-pid`
            // still names the group `pre_exec` put it in. A LAUNCHED candidate is
            // nobody's child — launchd may reap it at any moment and free the
            // number — so the same `-pid` can come to name a stranger's group, and
            // SIGKILL to a stranger is not a best-effort sweep, it is damage.
            //
            // The wire cannot answer this: `child_pid` arrives as a bare integer.
            // So ask the KERNEL instead, at the moment of use, with the one probe
            // that is both non-destructive and unforgeable. `WNOWAIT` leaves an
            // exited child waitable — the later `waitpid` still reaps it — and
            // ECHILD is precisely "not a child of mine", which is precisely
            // "not pinned".
            if candidate_is_our_child(pid) {
                // SAFETY: SIGKILL to the candidate's process group, whose number
                // the kernel just confirmed is still held by a child of ours, and
                // then to the candidate ITSELF.
                //
                // The group sweep alone reaches a candidate only while it leads a
                // group of its own, which is true of the lane's own `pre_exec`
                // children and of nothing else: a child that inherited this
                // process's group has no group numbered `pid`, so `-pid` finds
                // nothing and the candidate lives. Nothing downstream survives
                // that — the reject path's `wait` has no deadline, and
                // `wait_for_handoff_candidate_to_terminate` is unbounded ON
                // PURPOSE, so a candidate that is never signalled parks the
                // terminal for as long as it chooses to run.
                //
                // The direct signal needs no argument the sweep did not already
                // need: `candidate_is_our_child` just pinned this number, and a
                // pinned pid is exactly what makes `kill(pid, …)` land on the
                // candidate rather than on a stranger. It is the same pair the
                // corroborated arm sends, for the same reason.
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                    libc::kill(pid, libc::SIGKILL);
                }
            } else {
                aterm_log::warn!(
                    "handoff candidate {pid} is not our child, so its pid is not pinned and \
                     -{pid} may name a stranger; skipping the process-group sweep"
                );
            }
        }
    }
}

/// Block until the candidate is provably terminated, then license rollback.
///
/// UNBOUNDED ON PURPOSE. The proof is a PRECONDITION for resuming the parent's
/// parked readers, not a preference: resuming while the candidate might still be
/// reading the masters is the two-readers-on-one-master corruption the overlap
/// protocol exists to prevent, and it is silent when it happens. So there is no
/// deadline after which this returns anyway — `Child::wait`, the authority it
/// stands in for, blocks without one for exactly the same reason, and a bounded
/// probe that gave up would be strictly weaker than the code it replaces.
///
/// The interval below therefore decides only when a candidate that will not die
/// starts SAYING SO. The parked terminal is the visible symptom either way; the
/// log line is what makes it diagnosable rather than mysterious.
#[cfg(unix)]
fn wait_for_handoff_candidate_to_terminate(candidate: HandoffCandidate) -> HandoffRollbackWarrant {
    /// Probe cadence — the same ~2 ms yield the worker's decision loop uses.
    const PROBE: std::time::Duration = std::time::Duration::from_millis(2);
    /// How long a SIGKILLed candidate may take before this becomes loud, and how
    /// often it repeats afterwards.
    const COMPLAIN_EVERY: std::time::Duration = std::time::Duration::from_secs(10);
    let mut complain_at = std::time::Instant::now() + COMPLAIN_EVERY;
    loop {
        if handoff_candidate_terminated(candidate) {
            return HandoffRollbackWarrant::Vanished;
        }
        let now = std::time::Instant::now();
        if now >= complain_at {
            aterm_log::warn!(
                "update apply: handoff candidate {} has not terminated; the parked PTY readers \
                 stay parked until it does",
                candidate.pid
            );
            complain_at = now + COMPLAIN_EVERY;
        }
        std::thread::sleep(PROBE);
    }
}

/// The reap's two products: the rollback warrant, and — when this process's own
/// `wait` answered for the CANDIDATE — the status that `wait` collected.
///
/// THE STATUS USED TO BE DISCARDED (`child.wait().is_ok()`), which threw away the
/// only direct statement a candidate ever makes about why it stopped. It is
/// carried now, and it is an `Option` because on the launched lane nobody's `wait`
/// answers, and because a launcher-shaped child's status belongs to the launcher.
/// It is only ever READ once [`HandoffCandidateProbe::Exited`] has proved the
/// candidate was already dead before this lane signalled it — see
/// [`worker_reject_and_reap_handoff_child`].
#[cfg(unix)]
struct ReapedHandoffCandidate {
    warrant: HandoffRollbackWarrant,
    status: Option<std::process::ExitStatus>,
}

/// Kill the rejected candidate and prove it gone. Runs only on the handoff
/// worker, and the returned warrant is what licenses [`App::rollback_overlap`]
/// to resume the parked readers.
///
/// CONTAINMENT THIS RELIES ON: the FORK lane's. `child` is our own `spawn`, so
/// `run_handoff_worker`'s `pre_exec` `setpgid(0, 0)` established the candidate's
/// group before its image ran, and the opening group sweep in
/// [`signal_handoff_candidate`] therefore reaches its ditto/codesign/spctl
/// helpers. Reached with a candidate this process did not fork, that sweep would
/// be the weaker, unobserved kind — read [`signal_handoff_candidate`] before
/// assuming otherwise. The warrant returned here never rests on the sweep either
/// way: it comes from `wait` on our own child, or from
/// [`wait_for_handoff_candidate_to_terminate`]'s outside proof.
/// Also hands back the candidate's own exit status when our `wait` answered for
/// it; see [`ReapedHandoffCandidate`].
#[cfg(unix)]
fn kill_and_reap_handoff_child(
    candidate: HandoffCandidate,
    handle: &mut HandoffCandidateHandle,
) -> ReapedHandoffCandidate {
    let child = match handle {
        HandoffCandidateHandle::Forked(child) => child,
        HandoffCandidateHandle::Launched(_) => {
            // NOBODY'S `wait` ANSWERS FOR THIS ONE. `waitpid` is not an
            // authority about somebody else's child (it says `ECHILD`, which is
            // not evidence of anything), so the outside proof stands in for it —
            // exactly the substitution B2 built `handoff_candidate_terminated`
            // for. The signal still goes first, for the same reason it does
            // below: descendants must be condemned before anything blocks.
            signal_handoff_candidate(candidate);
            return ReapedHandoffCandidate {
                warrant: wait_for_handoff_candidate_to_terminate(candidate),
                status: None,
            };
        }
    };
    // Signal BEFORE any wait, so descendants are already condemned when the
    // direct child is reaped.
    signal_handoff_candidate(candidate);
    let child_is_candidate = child.id() == candidate.pid;
    // PREFERRED AUTHORITY: `wait` on our own fork child proves termination AND
    // consumes the identity in one step, so nothing can recycle the pid between
    // the proof and its use. It answers only for a child of THIS process
    // (`ECHILD` otherwise), and only about the candidate when the child IS the
    // candidate — a launcher-shaped child (`open -n`, which exits as soon as
    // LaunchServices holds the successor) would be reaped here while proving
    // nothing about the process holding the masters.
    let reaped = child.wait().ok();
    if let Some(status) = reaped
        && child_is_candidate
    {
        return ReapedHandoffCandidate {
            warrant: HandoffRollbackWarrant::Reaped,
            status: Some(status),
        };
    }
    // FALLBACK: no `wait` of ours answers for the candidate. Prove it terminated
    // from the outside instead. The status is withheld with it: a launcher-shaped
    // child's exit describes the launcher, not the process holding the masters.
    ReapedHandoffCandidate {
        warrant: wait_for_handoff_candidate_to_terminate(candidate),
        status: None,
    }
}

/// WHAT THIS PROCESS MAY DO TO THE CANDIDATE — which is a property of how the
/// candidate was started, not of what we would like to do.
///
/// `Child::wait` is the best reap authority there is: it proves termination AND
/// consumes the identity in one step, so nothing can recycle the pid between the
/// proof and its use. It exists only for a process we forked. Modelling that as
/// a type rather than an `Option<&mut Child>` is what keeps the launched lane
/// from silently inheriting a `wait` that would answer `ECHILD` and prove
/// nothing — the compiler makes the caller say which world it is in.
#[cfg(unix)]
enum HandoffCandidateHandle {
    /// Our own `spawn`. `wait` is available and is the preferred warrant.
    Forked(std::process::Child),
    /// launchd's, not ours. Nothing here may `wait`, and the warrant comes from
    /// [`handoff_candidate_terminated`]'s outside proof.
    ///
    /// The [`CandidateExitWatch`] is this lane's ONLY possible statement about how
    /// the candidate died, registered at the rendezvous accept. `None` whenever it
    /// could not be registered, and always `None` off macOS, where nothing
    /// launches a candidate at all.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Launched(Option<CandidateExitWatch>),
}

#[cfg(unix)]
impl HandoffCandidateHandle {
    /// The candidate's own status IF the kernel has already recorded one, WITHOUT
    /// blocking and without reaping.
    ///
    /// `None` on the fork lane by construction: its status comes from `wait`,
    /// which is a stronger authority and the one [`kill_and_reap_handoff_child`]
    /// already collects. Only the launched lane, which has no `wait` to collect,
    /// answers from a witness.
    fn witnessed_exit_status(&self) -> Option<std::process::ExitStatus> {
        match self {
            Self::Forked(_) => None,
            Self::Launched(watch) => watch.as_ref().and_then(CandidateExitWatch::exit_status),
        }
    }
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

/// The emergency reaper's kill, off a bare `child_pid` from the completion wire
/// (on its own thread, or inline when that thread cannot be created).
///
/// CONTAINMENT THIS RELIES ON: whichever one made the candidate a group leader —
/// and this function cannot tell which, because a bare pid carries no evidence
/// of either. On the fork lane it is `pre_exec`'s, established before the
/// candidate's image ran. On a lane where the candidate was launched instead of
/// forked it would be the candidate's own [`contain_own_process_group`], which
/// no wire reports to us, so the group sweep below would be an unproven
/// best-effort rather than the helper kill it is today. The returned warrant is
/// unaffected either way: it comes from `waitpid` or from the outside proof, and
/// [`signal_handoff_candidate`] states what the sweep does and does not buy.
#[cfg(unix)]
fn emergency_kill_and_reap_handoff_child(pid: u32) -> HandoffRollbackWarrant {
    // PRECONDITION: the caller won the attempt-wide Emergency reaper CAS, so no
    // worker can concurrently consume the candidate's identity. While the
    // candidate is a fork child, that claim also pins `pid` to it (an unreaped
    // zombie owns its number); a candidate launchd owns has no such pin, which
    // is why the fallback below re-derives the fact rather than assuming it.
    // Signal the process group BEFORE any wait: `waitpid` also reaps an
    // already-dead leader, and the old ordering then returned while its
    // ditto/codesign/spctl descendants continued mutating fixed updater paths.
    let candidate = HandoffCandidate::from_bare_pid(pid);
    signal_handoff_candidate(candidate);
    let Ok(raw) = libc::pid_t::try_from(pid) else {
        return wait_for_handoff_candidate_to_terminate(candidate);
    };
    let mut status = 0i32;
    let reaped = loop {
        // SAFETY: blocking wait for one exact pid into a local status slot.
        let waited = unsafe { libc::waitpid(raw, &mut status, 0) };
        if waited == raw {
            break true;
        }
        if waited < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        // ECHILD — and, defensively, any other refusal. In the fork lane this
        // can only be a teardown-time reap that already happened. A candidate
        // launchd owns answers this way from the start, and THAT is blocker B2:
        // `waitpid` is simply not an authority about somebody else's child, so
        // the outside proof has to stand in for it. What must never happen is
        // treating the refusal itself as evidence the candidate is gone.
        break false;
    };
    if reaped {
        return HandoffRollbackWarrant::Reaped;
    }
    wait_for_handoff_candidate_to_terminate(candidate)
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
            // NOTHING OBSERVED is the right default for every producer but one.
            // Preparation failures and cancels have no candidate at all, and every
            // other returned outcome describes a candidate THIS process ended — a
            // status that is ours, not evidence. Only the `ChildDied` producer
            // overrides it, and only with what it actually saw.
            child_death: crate::ChildDeathEvidence::Unobserved,
        }
    }

    /// Attach what the worker observed about a candidate that died on its own.
    #[must_use]
    fn with_child_death(mut self, death: crate::ChildDeathEvidence) -> Self {
        self.child_death = death;
        self
    }
}

/// Publish a non-ready completion — the event-loop message that runs
/// [`App::rollback_overlap`] and therefore RESUMES the parked readers. The
/// warrant is the point of the name: this must not be called until the caller
/// holds one, because the completion crosses a channel and cannot carry the
/// proof with it.
#[cfg(unix)]
fn send_warranted_handoff_failure(
    warrant: HandoffRollbackWarrant,
    cleanup: &HandoffWorkerCleanup,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    current_build: u64,
    completion: crate::UpdateHandoffCompletion,
) {
    warrant.announce(completion.child_pid);
    cleanup.complete(completion.nonce.as_deref());
    // Reader rollback and reducer re-arm are latency-critical. Publish the
    // candidate-terminated fact before waiting behind the updater FIFO for disk
    // facts.
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

/// Reject, kill, and prove terminated on the worker, only after winning the
/// attempt-wide arbiter. `false` means Commit won the race and the caller must
/// keep the candidate untouched while waiting for Commit success or its explicit
/// failure transfer.
#[cfg(unix)]
fn worker_reject_and_reap_handoff_child(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    handle: &mut HandoffCandidateHandle,
    candidate: HandoffCandidate,
    nonce: &str,
    outcome: crate::UpdateHandoffOutcome,
    detail: String,
) -> bool {
    if !worker_claim_handoff_reaper(&job.arbiter) {
        return false;
    }
    let (warrant, child_death) = observe_candidate_death(outcome, candidate, handle);
    let completed = job.arbiter.finish_reap(crate::HandoffReaperOwner::Worker);
    debug_assert!(
        completed,
        "the worker must retain its unique reaper ownership"
    );
    if outcome == crate::UpdateHandoffOutcome::ChildDied {
        // WRITE THE EVIDENCE DOWN. The completion detail paints a pill and stays
        // short, so the durable log is where a future field report finds out which
        // of the three `ChildDied` events this was — and, when the answer is
        // `Unobserved`, that the answer is genuinely unknown rather than assumed.
        aterm_log::warn!(
            "update apply: candidate {} died before proving adoption; observed {child_death:?} \
             (this decides whether the automatic lane retries these bytes or converges on them)",
            candidate.pid
        );
    }
    send_warranted_handoff_failure(
        warrant,
        &job.cleanup,
        proxy,
        job.current_build,
        crate::UpdateHandoffCompletion::failure(
            job.attempt_id,
            Some(nonce.to_string()),
            Some(candidate.pid),
            outcome,
            detail,
        )
        .with_child_death(child_death),
    );
    // GIVE THE COUNTED TRIAL LAUNCH BACK unless the bytes answer for this death —
    // see `forgives_the_counted_trial_launch`, which is the whole of the decision.
    //
    // AFTER THE WAKE, DELIBERATELY, and for the same reason
    // `send_warranted_handoff_failure` publishes before it collects reconcile
    // facts: the rollback resumes the user's parked readers and is
    // latency-critical, while this is a durable ledger edit nothing is waiting on.
    // It is not a free read — `forgive_trial_launch_if_advanced` resolves the
    // staging root (a `mkdir` + `stat` + `chmod`), reads the sentinel, and takes
    // the apply lock to write — and it now runs on the `ChildDied` path too, which
    // is precisely the path a machine under pathological load reaches. The
    // candidate is already dead and the next attempt is minutes away, so nothing
    // depends on this having happened first.
    if forgives_the_counted_trial_launch(child_death, job.current_build, job.target_build) {
        aterm_update::forgive_trial_launch_if_advanced(job.target_build, job.trial_launches_before);
    }
    true
}

/// KILL THE CANDIDATE, PROVE IT TERMINATED, AND SAY WHAT KILLED IT — the whole of
/// the reject path's evidence gathering, in one place so a test can drive it end
/// to end against a real process.
///
/// It was three statements inlined in [`worker_reject_and_reap_handoff_child`],
/// which needs a worker job, an event-loop proxy and an arbiter to call at all;
/// nothing could reach them, and a one-token mutation in any of them inverted the
/// field classification with the whole suite green.
///
/// THE ORDER IS THE PROTOCOL, and each step is here because the step before it
/// destroys something:
///
///  1. BEFORE ANY SIGNAL, ask what is already known. On the fork lane that is
///     [`probe_handoff_candidate`]'s `waitid(WNOWAIT)`; on the launched lane it is
///     the [`CandidateExitWatch`] registered at the accept. Whatever answers here
///     is UNAMBIGUOUSLY the candidate's own death, because this process has not
///     yet touched it.
///  2. THE KILL AND THE TERMINATION PROOF, unchanged, and still the thing that
///     licenses the parked readers to resume.
///  3. AFTER IT IS PROVABLY GONE, ask again. This is not a second guess at step 1
///     — it is a strictly later question with a strictly weaker answer, and
///     [`handoff_child_death`] is what knows which parts of that answer survive.
///     On the fork lane it is the status `wait` just collected; on the launched
///     lane the kqueue knote, which XNU queued inside `proc_exit` and which is
///     therefore GUARANTEED to be there once termination is proven — no race, only
///     an attribution question.
///
/// Only [`crate::UpdateHandoffOutcome::ChildDied`] has a death of the candidate's
/// OWN to describe. Every other outcome is a candidate this process decided to
/// end, so its status is our SIGKILL and is evidence about nothing.
#[cfg(unix)]
fn observe_candidate_death(
    outcome: crate::UpdateHandoffOutcome,
    candidate: HandoffCandidate,
    handle: &mut HandoffCandidateHandle,
) -> (HandoffRollbackWarrant, crate::ChildDeathEvidence) {
    if outcome != crate::UpdateHandoffOutcome::ChildDied {
        let reaped = kill_and_reap_handoff_child(candidate, handle);
        return (reaped.warrant, crate::ChildDeathEvidence::Unobserved);
    }
    let witnessed_before_the_kill = handle.witnessed_exit_status();
    let died_before_we_signalled = witnessed_before_the_kill.is_some()
        || libc::pid_t::try_from(candidate.pid)
            .is_ok_and(|pid| probe_handoff_candidate(pid) == HandoffCandidateProbe::Exited);
    let reaped = kill_and_reap_handoff_child(candidate, handle);
    // FIRST ANSWER WINS, in the order they were asked: the pre-kill witness is the
    // only one that needs no attribution argument at all, `wait` is this process's
    // own authority over its own child, and the post-termination knote is the
    // launched lane's last resort.
    let status = witnessed_before_the_kill
        .or(reaped.status)
        .or_else(|| handle.witnessed_exit_status());
    (
        reaped.warrant,
        handoff_child_death(died_before_we_signalled, status),
    )
}

/// Does the reject path GIVE BACK the boot-trial launch this candidate counted?
///
/// Which is one question with the polarity that matters here: the launch stays
/// counted only when the NEW BYTES ANSWER FOR THE DEATH, and every other answer —
/// including "we could not tell" — hands it back.
///
/// ONE QUESTION, ASKED ONCE, so the retry budget and the boot sentinel can never
/// drift apart. Both are counters over the same artifact and both end in the same
/// place if they disagree: the retry schedule keeps launching a candidate, every
/// launch stays counted, and on the third one `check_boot_health` reverts the
/// bundle and marks the build failed — poisoning bytes that never failed, which is
/// exactly what `forgive_trial_launch_if_advanced` was added to prevent.
///
/// The answer is the SHAPE, not the outcome. Gating on `outcome != ChildDied`
/// (what this used to do) exempted the whole of `ChildDied` on the argument that
/// it "IS the crash signal", and that argument died with the classification above
/// it: a starved candidate produces proof EOF as readily as a faulting one, and
/// its launch must be given back. The exemption was SAFE only while `ChildDied`
/// was also capped at two attempts; the moment the classification could retry one
/// six or nine times, an unforgiven count reached `MAX_BOOT_ATTEMPTS` on the third
/// and the retry lane itself became the thing that reverted a healthy bundle.
///
/// EVERY OTHER OUTCOME KEEPS THE ANSWER IT ALWAYS HAD, through the same predicate
/// rather than beside it: a candidate THIS process ended carries
/// [`crate::ChildDeathEvidence::Unobserved`], which is never `Structural`, so the
/// bounded automatic re-attempts a busy machine legitimately makes (`TimedOut` and
/// `ActivityRevoked` are scheduling facts, not evidence against the artifact) go
/// on forgiving exactly as before.
///
/// `false` for everything the parent could not attribute — `Unobserved` above all
/// — because forgiving costs the sentinel nothing it can prove it is owed
/// (`forgive_trial_launch_if_advanced` gives back only a launch that MOVED past
/// this attempt's pre-launch snapshot), while withholding it spends a budget
/// against bytes nobody has evidence against. And the sentinel keeps working
/// either way: the swap is already durable, so a successor that truly cannot boot
/// counts its launches on the user's ordinary relaunches, where nothing forgives.
///
/// THE WHOLE DECISION LIVES HERE, real-apply guard included, so none of it is left
/// at a call site no test can reach: the reject path either calls this and acts on
/// it, or the sentinel keeps the launch.
#[cfg(unix)]
#[must_use]
fn forgives_the_counted_trial_launch(
    death: crate::ChildDeathEvidence,
    current_build: u64,
    target_build: u64,
) -> bool {
    // REAL APPLY ONLY. The QA seam authorizes no newer target, so nothing armed a
    // sentinel for it and there is no counted launch to give back.
    target_build > current_build
        && crate::app_native::PhysicalFailureShape::of_child_death(death)
            != crate::app_native::PhysicalFailureShape::Structural
}

/// TURN ONE `wait(2)` STATUS INTO EVIDENCE, or refuse to.
///
/// THIS PROCESS ONLY EVER SENDS `SIGKILL` (`signal_handoff_candidate`, every arm),
/// and that single fact decides the whole function:
///
///   * AN EXIT CODE CANNOT BE OURS. `SIGKILL` is uncatchable and never yields
///     `WIFEXITED`, so `status.code()` being `Some` is by itself proof that the
///     candidate reached an `exit` instruction — which a starved process never
///     does. This is the tree's COMMONEST refusal (`main_entry` returns without a
///     window when the overlap authority is incomplete, exit code `0`) and it is
///     read unconditionally.
///   * A SIGNAL THAT IS NOT `SIGKILL` CANNOT BE OURS EITHER. `SIGSEGV`, `SIGBUS`,
///     `SIGABRT` and friends arrive from the image executing itself into a wall;
///     nothing in this file sends them.
///   * A BARE `SIGKILL` IS THE ONLY AMBIGUOUS ANSWER, and it is the one
///     `died_before_we_signalled` exists for. With it, the kill is the machine's
///     (macOS jetsam reclaiming memory — the field case). Without it, the honest
///     answer is that we cannot tell ours from theirs, so nothing is claimed.
///
/// THE PRECONDITION USED TO GUARD ALL THREE, and that is what this function's
/// previous version got wrong: it discarded status bits that provably could not be
/// this process's own signal, so a deliberate `exit(0)` — the refusal the whole
/// classification most wants to catch — degraded to `Unobserved` whenever the
/// parent lost a race it deliberately refuses to wait out. The gate now guards
/// exactly the one arm that needs it.
#[cfg(unix)]
#[must_use]
fn handoff_child_death(
    died_before_we_signalled: bool,
    status: Option<std::process::ExitStatus>,
) -> crate::ChildDeathEvidence {
    use std::os::unix::process::ExitStatusExt as _;
    let Some(status) = status else {
        return crate::ChildDeathEvidence::Unobserved;
    };
    if let Some(code) = status.code() {
        return crate::ChildDeathEvidence::Exited { code };
    }
    if let Some(signal) = status.signal()
        && (died_before_we_signalled || signal != libc::SIGKILL)
    {
        return crate::ChildDeathEvidence::Signalled { signal };
    }
    crate::ChildDeathEvidence::Unobserved
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
    // Cancellation during PREPARATION precedes the spawn, so there is no
    // candidate to prove anything about.
    send_warranted_handoff_failure(
        HandoffRollbackWarrant::NoCandidate,
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

/// Every preparation failure is raised BEFORE `spawn`, including the one for a
/// `spawn` that itself failed, so no candidate has ever held a master.
#[cfg(unix)]
fn send_handoff_preparation_failure(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    nonce: Option<String>,
    detail: impl Into<String>,
) {
    send_warranted_handoff_failure(
        HandoffRollbackWarrant::NoCandidate,
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
// The worker's muts serve the macOS handoff arms; on other platforms those arms
// are configured out and the bindings are read-only.
#[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
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
    if job.verify_staged_candidate {
        let verified = if job.installed_activation {
            // ACTIVATION: the artifact is the bundle under the running executable.
            // Prove it is exactly the authorized sealed identity, codesign-valid and
            // newer than this process — a bundle swapped again since the observation
            // is refused before a single reader is parked.
            aterm_update::preverify_installed_for_handoff(
                job.current_build,
                job.target_build,
                &job.target_commit,
            )
            .map_err(|error| format!("installed bundle failed pre-park verification: {error}"))
        } else {
            aterm_update::preverify_staged_for_handoff(
                job.current_build,
                Some(crate::build_info::GIT_COMMIT),
                Some(job.target_build),
                Some(&job.target_commit),
            )
            .map_err(|error| format!("staged update failed pre-park verification: {error}"))
        };
        if let Err(error) = verified {
            send_handoff_preparation_failure(&job, &proxy, None, error);
            return;
        }
    }
    if handoff_preparation_cancelled(&job, &proxy, None) {
        return;
    }
    // SNAPSHOT THE TRIAL COUNTER HERE, on the WORKER, after the (slow) codesign
    // pre-verification and as late as the lane allows: taken on the main thread at
    // job construction it both froze the terminal for a `Staging::resolve` (which
    // chmods) and could attribute a THIRD party's launch — counted during our own
    // pre-verification — to this candidate (2026-08-19 round-4 skeptics).
    job.trial_launches_before = aterm_update::trial_launch_count(job.target_build);

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
        manifest_path: mut path,
        mut nonce,
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
    let mut layout_path = std::path::Path::new(&path).with_extension("layout.toml");
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
    let Some(mut expected) = crate::seamless::adoption_proof(
        &nonce,
        job.target_build,
        &job.target_commit,
        &job.layout_digest,
        &job.screen_digest,
        // NOT `job.live` — the term the two sides can both compute depends on
        // how the descriptors travel. On the fork lane these are the same
        // vector; on the out-of-band lane the middle field is the PTY device.
        &job.proof_identities,
    ) else {
        send_handoff_preparation_failure(
            &job,
            &proxy,
            Some(nonce),
            "handoff identity set exceeds the proof format",
        );
        return;
    };
    let Some((mut proof_rd, mut proof_wr)) = make_cloexec_pipe() else {
        send_handoff_preparation_failure(
            &job,
            &proxy,
            Some(nonce),
            "could not create the adoption-proof channel",
        );
        return;
    };
    let Some((mut commit_rd, mut commit_wr)) = make_cloexec_pipe() else {
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

    // THE LANE SPLITS HERE, and everything above it is shared: the manifest, the
    // layout sidecar, the proof, and both private pipes are identical whichever
    // way the descriptors travel. The out-of-band lane owns its own environment
    // (it publishes a rendezvous instead of three descriptor numbers) and its own
    // candidate start, then rejoins at `run_handoff_decision` — which is the
    // WHOLE of the readiness wait, the ProofReady wake and the decision loop, so
    // the two lanes cannot drift on the part that decides whether a Commit
    // happens.
    #[cfg(target_os = "macos")]
    {
        if job.lane == HandoffLane::OutOfBand {
            // `Some` means the lane refused BEFORE anything moved — no descriptor
            // sent, no successor launched — and handed everything back so this
            // attempt can still happen the old way. Forking then costs the
            // orphaned launchd domain the out-of-band lane exists to avoid, which
            // is the trade the fork lane already makes, and it beats telling a
            // user with a staged build that nothing happened.
            match run_out_of_band_handoff(
                job,
                &proxy,
                OutgoingArtifacts {
                    manifest_path: path,
                    layout_path,
                    nonce,
                },
                expected,
                HandoffChannels {
                    proof_rd,
                    proof_wr,
                    commit_rd,
                    commit_wr,
                },
            ) {
                None => return,
                Some(ForkInstead {
                    job: returned_job,
                    artifacts,
                    expected: returned_expected,
                    channels,
                }) => {
                    job = returned_job;
                    expected = returned_expected;
                    path = artifacts.manifest_path;
                    layout_path = artifacts.layout_path;
                    nonce = artifacts.nonce;
                    proof_rd = channels.proof_rd;
                    proof_wr = channels.proof_wr;
                    commit_rd = channels.commit_rd;
                    commit_wr = channels.commit_wr;
                }
            }
        }
    }

    // THE PROOF TERM TRAVELS WITH THE LANE. `expected` was computed once, over the
    // terms the lane choice picked (PTY DEVICE terms for the launched lane, fd
    // numbers for the fork lane). A launched attempt that falls back to the fork
    // lane at runtime (`ForkInstead`: the rendezvous could not bind, LaunchServices
    // refused) keeps that device-term proof — and a forked successor that inferred
    // its term from "no rendezvous in the environment" hashed fd numbers, so every
    // fallback ended in AdoptionMismatch after the child had already swapped the
    // bundle (2026-08-19 round-3 audit). Say the term explicitly; the successor
    // computes device terms on inherited masters just as well.
    #[cfg(target_os = "macos")]
    {
        let proof_term = match job.lane {
            HandoffLane::OutOfBand => "device",
            HandoffLane::Fork => "fd",
        };
        job.command
            .env(crate::handoff_rendezvous::ENV_PROOF_TERM, proof_term);
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
            //
            // STRICTLY STRONGER than the successor-side
            // `contain_own_process_group` that covers the launch shape with no
            // pre-exec hook, and the reason this stays here rather than being
            // replaced by it: this runs between fork and exec, so the group
            // exists before the candidate's image does — no ordering argument
            // about "the first thing that can fork a helper" is needed, and the
            // failure below aborts the spawn instead of having to be handled by
            // a process that is already running. Keep both.
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
    // WHY THIS `spawn` IS STILL HERE, AND WHAT NOW STANDS BESIDE IT.
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
    // notifications, LaunchServices opens). `tests/handoff_launchd_job.rs` is
    // that defect's reproducer and regression guard.
    //
    // THE FIX — launch the successor through LaunchServices so launchd mints it
    // its OWN application job — is the `HandoffLane::OutOfBand` branch above,
    // and it needed four properties this `spawn` gets for free. All four now
    // exist: B1 parent attestation (`seamless::AttestedParent`, the kernel birth
    // record, because a launched successor has ppid 1 from birth), B2 reap
    // authority (`HandoffRollbackWarrant` + `HandoffCandidateHandle`, because
    // `waitpid` answers `ECHILD` about somebody else's child and that is not
    // evidence of anything), B3 process-group containment
    // (`contain_own_process_group` on the successor's entry, since no `pre_exec`
    // hook of ours can run inside a process we did not fork), and B4 transport
    // (`handoff_rendezvous`, since a LaunchServices launch inherits no
    // descriptors at all).
    //
    // SO WHY KEEP FORKING? Because the launched lane is refused for four honest
    // reasons — no `.app` bundle (`cargo run`, a dev binary, the test harness),
    // a `$HOME` long enough to push the rendezvous past `sun_path`, more panes
    // than one `SCM_RIGHTS` message carries, and an authorized target build
    // OLDER than this one (a successor with no out-of-band code must never be
    // handed descriptors it cannot receive). `out_of_band_lane_refusal` names
    // which. On every one of those this lane is the only lane, so it stays
    // exactly as it was: byte-identical behaviour, and the two paths rejoin at
    // `run_handoff_decision` so nothing about the Commit decision can drift
    // between them.
    //
    // B3's RESIDUAL, on the launched lane only: `pre_exec` establishes the
    // candidate's process group before `spawn` returns, so on THIS lane
    // `kill(-pid)` is a valid handle from that instant. A launched successor
    // contains itself instead, and reports nothing, so there `-pid` is an
    // unproven sweep — `signal_handoff_candidate` states what each reaper may
    // conclude. What the rendezvous did close is the identity half: the accept
    // hands back a kernel-attested `LOCAL_PEERPID`, so the DIRECT signal is
    // aimed at a corroborated candidate rather than withheld for lack of one.
    let child = match job.command.spawn() {
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
    // Capture the candidate's kernel identity while its pid is still PINNED by
    // being an unreaped child of ours. Every later reap authority reads it, so
    // it has to be taken at the one instant the pid provably names the
    // candidate. On the launched lane there is no such pin, and the identity
    // arrives from the rendezvous accept instead (`of_attested_peer`); nothing
    // downstream of here can tell the difference.
    let candidate = HandoffCandidate::of_unreaped_child(&child);
    let mut handle = HandoffCandidateHandle::Forked(child);
    drop(proof_wr);
    drop(commit_rd);
    run_handoff_decision(
        &job,
        &proxy,
        &nonce,
        expected,
        &proof_rd,
        commit_wr,
        candidate,
        &mut handle,
        handoff_ready_deadline(),
    );
}

/// The physical artifacts one attempt published, threaded into the out-of-band
/// lane so its environment can name them exactly as the fork lane's does.
#[cfg(target_os = "macos")]
struct OutgoingArtifacts {
    manifest_path: String,
    layout_path: std::path::PathBuf,
    nonce: String,
}

/// The two private pipes of one attempt, before either end has been given away.
///
/// Grouped so the out-of-band lane cannot be handed three of the four by
/// accident: which END goes to the successor is the whole protocol (the
/// successor writes the proof and reads the Commit), and a swapped pair would
/// deadlock in a way that reads as a slow successor.
#[cfg(target_os = "macos")]
struct HandoffChannels {
    proof_rd: std::os::fd::OwnedFd,
    proof_wr: std::os::fd::OwnedFd,
    commit_rd: std::os::fd::OwnedFd,
    commit_wr: std::os::fd::OwnedFd,
}

/// Everything the out-of-band lane was handed, given back UNUSED so the caller can
/// fork instead.
///
/// A refusal BEFORE the rendezvous is published and the successor launched has
/// changed nothing: no descriptor has moved, no candidate exists, and the fork lane
/// below is still perfectly able to carry this attempt. Abandoning the update there
/// kept the user's windows (every refusal does) but cost them the update for no
/// reason — on a machine whose LaunchServices refuses us, for as long as it refuses.
/// So those refusals return this instead of reporting a failure.
#[cfg(target_os = "macos")]
struct ForkInstead {
    job: HandoffWorkerJob,
    artifacts: OutgoingArtifacts,
    expected: crate::seamless::AdoptionProof,
    channels: HandoffChannels,
}

/// `None` once the attempt is this lane's to finish — committed, rolled back, or
/// reported. `Some` only for a refusal that happened before anything moved.
/// The out-of-band lane: bind, launch, accept, transfer — then hand over to the
/// same decision tail the fork lane uses.
///
/// ORDER IS THE DESIGN. The listener is bound BEFORE the launch, because the
/// successor has to be told a name that already exists; the launch budget is a
/// slice of the shared readiness deadline, because a launch that never answers
/// must not eat the whole of it; and the accept then spends what is left,
/// because the successor's dial happens AFTER its own staged-swap boot apply
/// (`ditto`/`hdiutil`/`codesign` plus a re-exec) and that is exactly the
/// interval the fork lane already budgets 15 s for.
///
/// EVERY FAILURE BEFORE THE TRANSFER KEEPS THE WINDOWS AND KILLS NOTHING. No
/// descriptor of ours has left the process, so the successor cannot be a reader;
/// the rendezvous is dropped (which closes the listener and unlinks the node),
/// so the successor's dial fails closed and it exits by its own rule rather than
/// presenting itself as a fresh terminal over sessions this process still owns.
/// That is why the warrant here is `NeverTransferred` and not a kill-and-prove:
/// waiting for a successor that is still inside `hdiutil` to die would park the
/// user's terminal for the length of an update.
#[cfg(target_os = "macos")]
fn run_out_of_band_handoff(
    job: HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    artifacts: OutgoingArtifacts,
    expected: crate::seamless::AdoptionProof,
    channels: HandoffChannels,
) -> Option<ForkInstead> {
    use std::os::fd::AsFd as _;

    /// How long LaunchServices gets to answer. An answer normally lands well
    /// under a second; the rest of the deadline belongs to the dial, which has a
    /// whole boot apply in front of it.
    const LAUNCH_ANSWER_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

    let OutgoingArtifacts {
        manifest_path,
        layout_path,
        nonce,
    } = artifacts;
    let HandoffChannels {
        proof_rd,
        proof_wr,
        commit_rd,
        commit_wr,
    } = channels;

    // Every refusal down to the transfer is an ordinary PREPARATION failure with
    // a `NoCandidate` warrant, exactly like a `spawn` that would not start: at
    // each of them either no successor exists yet, or one exists and has been
    // given nothing.
    let Some(bundle) = job.bundle.clone() else {
        // Not a bundle: nothing has moved, so fork rather than skip the update.
        // The fork lane needs no bundle, and a machine that cannot be launched
        // through LaunchServices should still get its update.
        return Some(ForkInstead {
            job,
            artifacts: OutgoingArtifacts {
                manifest_path,
                layout_path,
                nonce,
            },
            expected,
            channels: HandoffChannels {
                proof_rd,
                proof_wr,
                commit_rd,
                commit_wr,
            },
        });
    };
    // Bound in its own statement so the borrow of `nonce` provably ends before
    // the refusal below consumes it — the same reason the transfer's result is
    // taken before it is matched on.
    let bound = crate::handoff_rendezvous::Rendezvous::bind(&nonce);
    let rendezvous = match bound {
        Ok(rendezvous) => rendezvous,
        Err(error) => {
            // A stale node, an unwritable support dir, a path that would not fit
            // sun_path: none of it has moved a descriptor, and the fork lane needs
            // no socket at all.
            aterm_log::warn!(
                "update apply: handoff rendezvous could not be bound ({error}); forking instead"
            );
            return Some(ForkInstead {
                job,
                artifacts: OutgoingArtifacts {
                    manifest_path,
                    layout_path,
                    nonce,
                },
                expected,
                channels: HandoffChannels {
                    proof_rd,
                    proof_wr,
                    commit_rd,
                    commit_wr,
                },
            });
        }
    };
    let Some(rendezvous_path) = rendezvous.path().to_str().map(str::to_string) else {
        aterm_log::warn!("update apply: the handoff rendezvous path is not UTF-8; forking instead");
        return Some(ForkInstead {
            job,
            artifacts: OutgoingArtifacts {
                manifest_path,
                layout_path,
                nonce,
            },
            expected,
            channels: HandoffChannels {
                proof_rd,
                proof_wr,
                commit_rd,
                commit_wr,
            },
        });
    };
    // The launch environment is DERIVED from the very `Command` the fork lane
    // would have used, so argv and every inherited-authority variable are the
    // same on both lanes by construction rather than by two lists staying in
    // sync. What differs is only the transport: no descriptor NUMBER appears
    // here — a launched process inherits none, so those integers would name
    // whatever LaunchServices left in the successor's table — and a rendezvous
    // plus its claim secret appear instead.
    let Some(mut environment) = launch_environment(&job.command) else {
        // A merged launch cannot express a REMOVAL, and the fork lane can — this is
        // the one refusal where forking is not merely acceptable but correct.
        aterm_log::warn!(
            "update apply: the launch environment needs a removal a merged launch cannot \
             express; forking instead"
        );
        return Some(ForkInstead {
            job,
            artifacts: OutgoingArtifacts {
                manifest_path,
                layout_path,
                nonce,
            },
            expected,
            channels: HandoffChannels {
                proof_rd,
                proof_wr,
                commit_rd,
                commit_wr,
            },
        });
    };
    environment.push((
        "ATERM_SEAMLESS_MANIFEST".into(),
        std::ffi::OsString::from(manifest_path.clone()),
    ));
    environment.push(("ATERM_SEAMLESS_NONCE".into(), nonce.clone().into()));
    // Cloned, not moved: a LaunchServices refusal below hands these back to the
    // fork lane, which names the same two artifacts. One String and one PathBuf
    // per attempt is not a cost worth trading a skipped update for.
    environment.push((
        "ATERM_SEAMLESS_LAYOUT".into(),
        layout_path.clone().into_os_string(),
    ));
    environment.push((
        "ATERM_SEAMLESS_TARGET".into(),
        crate::seamless::encode_target_identity(job.target_build, &job.target_commit).into(),
    ));
    environment.push((
        crate::handoff_rendezvous::ENV_RENDEZVOUS.into(),
        rendezvous_path.into(),
    ));
    environment.push((
        crate::handoff_rendezvous::ENV_CLAIM.into(),
        rendezvous.claim().into(),
    ));
    for (key, value) in crate::seamless::outgoing_parent_env() {
        environment.push((key.into(), value.into()));
    }
    let arguments = job
        .command
        .get_args()
        .map(std::ffi::OsStr::to_os_string)
        .collect::<Vec<_>>();

    if handoff_preparation_cancelled(&job, proxy, Some(nonce.clone())) {
        return None;
    }
    let deadline = handoff_ready_deadline();
    let launch_at = std::time::Instant::now();
    let launched = match crate::app_launch_successor::launch_app_bundle(
        &bundle,
        &arguments,
        &environment,
        LAUNCH_ANSWER_BUDGET.min(deadline.saturating_duration_since(std::time::Instant::now())),
    ) {
        Ok(launched) => Some(launched.pid()),
        // A TIMEOUT IS NOT A FAILURE, AND TREATING IT AS ONE THREW AWAY A LIVE
        // SUCCESSOR. Measured on this machine: a bundle's first launch took
        // longer than this budget to ANSWER, the parent tore the rendezvous down
        // on the way out, and the successor — which had started, and was correct
        // — dialed into a closed socket and exited with
        // "the parent closed the rendezvous". Nothing was lost (that exit is
        // before any window, by design) except the update.
        //
        // The launch answer was never the liveness signal; the DIAL is, which is
        // what this lane exists to arrange. So a timeout keeps the rendezvous
        // open for the rest of the deadline and gives up only if nobody arrives.
        // What is lost is the pid to compare the dialer against, and that is
        // defence in depth rather than the lock: the claim secret is checked
        // first, and the socket is 0700 inside a 0700 directory.
        Err(crate::app_launch_successor::LaunchError::Timeout(_)) => {
            aterm_log::info!(
                "update apply: LaunchServices has not answered yet; keeping the rendezvous open \
                 for the rest of the deadline — the dial is the liveness signal, not the answer"
            );
            None
        }
        Err(error) => {
            // A REFUSAL, not a slow answer: nothing was launched, so no successor
            // can dial and the fork lane is untouched and still able to carry this
            // attempt. Hand the job back rather than reporting a failure — the
            // trade is the orphaned launchd domain this lane exists to avoid,
            // which is exactly the trade the fork lane already makes today, and it
            // beats not updating at all on a machine LaunchServices refuses.
            //
            // The rendezvous is dropped on the way out, which closes the listener
            // and unlinks the node, so nothing is left for a late dialer to find.
            aterm_log::warn!(
                "update apply: LaunchServices refused the successor ({error}); forking instead"
            );
            return Some(ForkInstead {
                job,
                artifacts: OutgoingArtifacts {
                    manifest_path,
                    layout_path,
                    nonce,
                },
                expected,
                channels: HandoffChannels {
                    proof_rd,
                    proof_wr,
                    commit_rd,
                    commit_wr,
                },
            });
        }
    };
    match launched {
        Some(pid) => aterm_log::info!(
            "update apply: launched successor pid {pid} as its own launchd application job; \
             awaiting its rendezvous dial"
        ),
        None => aterm_log::info!("update apply: awaiting the successor's rendezvous dial"),
    }
    let peer = match rendezvous.accept_claim(launched, deadline, &|| job.cancel.try_recv().is_ok())
    {
        Ok(peer) => peer,
        Err(crate::handoff_rendezvous::RendezvousError::Cancelled) => {
            send_warranted_handoff_failure(
                HandoffRollbackWarrant::NeverTransferred,
                &job.cleanup,
                proxy,
                job.current_build,
                crate::UpdateHandoffCompletion::failure(
                    job.attempt_id,
                    Some(nonce),
                    launched.map(pid_for_completion),
                    crate::UpdateHandoffOutcome::ActivityRevoked,
                    "structural activity revoked the handoff before any descriptor was sent",
                ),
            );
            return None;
        }
        Err(error) => {
            send_warranted_handoff_failure(
                HandoffRollbackWarrant::NeverTransferred,
                &job.cleanup,
                proxy,
                job.current_build,
                crate::UpdateHandoffCompletion::failure(
                    job.attempt_id,
                    Some(nonce),
                    launched.map(pid_for_completion),
                    crate::UpdateHandoffOutcome::TimedOut,
                    format!("the launched successor never claimed the handoff: {error}"),
                ),
            );
            return None;
        }
    };
    // THE SPLIT, at the one instant it exists. `park->proof` (logged by the main
    // thread at Commit) is one number covering the launch, the successor's boot
    // apply, its second execve, its cold GUI boot, its first present and the
    // proof — and the two competing designs for shrinking the freeze attack
    // OPPOSITE halves of it. The dial is the boundary between them: everything
    // before it is the successor's pre-window work (which a pre-apply or a late
    // park could remove), everything after it is GUI boot and first present
    // (which neither can touch). Measured here, on the worker, so the parked
    // main thread pays nothing for the reading.
    let dial_at = std::time::Instant::now();
    aterm_log::info!(
        "update apply: successor dialled — park->dial {} ms, launch->dial {} ms",
        dial_at.saturating_duration_since(job.park_at).as_millis(),
        dial_at.saturating_duration_since(launch_at).as_millis(),
    );
    // The identity the whole launched lane rests on, taken at the one instant it
    // is available: a pid the KERNEL attested for a process we did not fork,
    // plus the kernel's birth stamp for it. Together they survive pid reuse,
    // which a bare pid from a completion wire does not.
    let candidate = HandoffCandidate::of_attested_peer(pid_for_completion(peer.pid()));
    // AND THE ONLY DEATH-WITNESS THIS LANE CAN EVER HAVE, registered at the same
    // instant and for the same reason: the candidate is provably alive right now
    // (it just dialled and was accepted), and `EVFILT_PROC` can only be attached to
    // a process that still exists. Registered BEFORE the descriptor transfer, so
    // there is no window in which the successor holds the readiness channel and
    // nothing is watching it. `None` costs only evidence — see
    // [`CandidateExitWatch`].
    let exit_watch = CandidateExitWatch::watch(candidate.pid);
    if exit_watch.is_none() {
        aterm_log::warn!(
            "update apply: could not watch launched candidate {} for exit; a death on this \
             lane will be reported as Unobserved",
            candidate.pid
        );
    }
    // `job.live` is `(local_id, child-owned master duplicate, shell pid)`; the
    // transfer wants `(local_id, shell pid, master)` in the order it will send
    // them, and that order is what addresses the descriptors on arrival.
    let sessions = job
        .live
        .iter()
        .map(|(local_id, master, pid)| {
            // SAFETY: `job` owns these duplicates for the whole attempt
            // (`_owned_masters`), so the borrow cannot outlive them.
            (*local_id, *pid, unsafe {
                std::os::fd::BorrowedFd::borrow_raw(*master)
            })
        })
        .collect::<Vec<_>>();
    let transfer_failed = peer
        .transfer(
            &nonce,
            &sessions,
            proof_wr.as_fd(),
            commit_rd.as_fd(),
            deadline,
        )
        .err();
    if let Some(error) = transfer_failed {
        send_warranted_handoff_failure(
            HandoffRollbackWarrant::NeverTransferred,
            &job.cleanup,
            proxy,
            job.current_build,
            crate::UpdateHandoffCompletion::failure(
                job.attempt_id,
                Some(nonce),
                Some(candidate.pid),
                crate::UpdateHandoffOutcome::Rejected,
                format!("the handoff descriptors could not be delivered: {error}"),
            ),
        );
        return None;
    }
    // From here the successor holds copies of everything, so this is the first
    // instant at which a rollback owes a candidate proof — and from here the
    // decision is identical to the fork lane's.
    drop(rendezvous);
    drop(peer);
    drop(proof_wr);
    drop(commit_rd);
    let mut handle = HandoffCandidateHandle::Launched(exit_watch);
    run_handoff_decision(
        &job,
        proxy,
        &nonce,
        expected,
        &proof_rd,
        commit_wr,
        candidate,
        &mut handle,
        deadline,
    );
    // The attempt reached the shared decision tail, so it is finished either way.
    None
}

/// A launched pid in the `u32` shape the completion wire and [`HandoffCandidate`]
/// use.
///
/// The conversion cannot fail in practice — `app_launch_successor` refuses any
/// pid that is not positive, and the rendezvous only accepts a peer whose
/// kernel-attested pid equals that one — so this exists for the case where it
/// somehow does. Zero is the safe answer rather than a wrapped one because every
/// consumer already refuses it: `signal_handoff_candidate` returns for `pid <= 1`
/// (0 and -1 are `kill`'s "my whole process group" and "broadcast"), and
/// `handoff_candidate_terminated` reads a vacancy it can never prove.
#[cfg(target_os = "macos")]
#[must_use]
fn pid_for_completion(pid: i32) -> u32 {
    u32::try_from(pid).unwrap_or_default()
}

/// The launch environment a `Command` describes, or `None` when it cannot be
/// expressed as a MERGE.
///
/// `NSWorkspaceOpenConfiguration.environment` MERGES over the launching
/// process's environment; it cannot REMOVE a variable. `Command::env_remove`
/// can, and `bind_expected_update_artifact` uses it: a successor that inherits a
/// stale `ATERM_UPDATE_EXPECTED_*` would authenticate its staged bundle against
/// the wrong artifact. So a removal that would actually remove something — the
/// variable is set in THIS process — makes the whole lane unavailable, and the
/// attempt forks instead. A removal of something not set is a no-op and is
/// ignored, which is the common case (`env_remove` is called unconditionally).
#[cfg(target_os = "macos")]
fn launch_environment(
    command: &std::process::Command,
) -> Option<Vec<(std::ffi::OsString, std::ffi::OsString)>> {
    let mut pairs = Vec::new();
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => pairs.push((key.to_os_string(), value.to_os_string())),
            None if std::env::var_os(key).is_some() => return None,
            None => (),
        }
    }
    Some(pairs)
}

/// Everything after a candidate exists: the bounded readiness wait, the
/// ProofReady wake, and the decision loop that ends in Commit or a warranted
/// rejection.
///
/// SHARED BY BOTH LANES ON PURPOSE. This is the part that decides whether the
/// user's sessions change hands, and it must not be able to differ between a
/// forked and a launched successor — the only thing that legitimately differs is
/// HOW the candidate is reaped, and that is `handle`'s job, not this function's.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn run_handoff_decision(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    nonce: &str,
    expected: crate::seamless::AdoptionProof,
    proof_rd: &std::os::fd::OwnedFd,
    commit_wr: std::os::fd::OwnedFd,
    candidate: HandoffCandidate,
    handle: &mut HandoffCandidateHandle,
    deadline: std::time::Instant,
) {
    let proof_outcome = wait_handoff_ready(
        proof_rd,
        expected,
        &job.cancel,
        &job.live.iter().map(|(_, fd, _)| *fd).collect::<Vec<_>>(),
        deadline,
    );
    if proof_outcome != crate::UpdateHandoffOutcome::ProofReady {
        // `ChildDied` IS proof EOF and nothing more: a successor that REFUSES this
        // handoff, one that CRASHED, and one the machine STARVED all close the
        // readiness channel identically. For five field failures the verdict did
        // not have to tell them apart, because the refusing successor said nothing
        // at all; it says so now (`seamless::take_target_identity`, and the
        // degraded-authority exit in `main_entry`), and the reject path below adds
        // what the PARENT saw (`handoff_child_death`). Point the next reader at
        // both rather than leaving `ChildDied` looking like an accusation against
        // the bytes. Log-only: the completion detail stays short because it paints
        // a pill.
        if proof_outcome == crate::UpdateHandoffOutcome::ChildDied {
            aterm_log::warn!(
                "update apply: the successor closed the readiness channel without proving \
                 adoption. A REFUSAL, a CRASH and a STARVED child look identical here — read \
                 this log just above for the successor's own `seamless handoff refused:` / \
                 `overlap handoff:` line, which names the reason (most often: it never became \
                 the authorized build because its boot apply could not swap), and just below \
                 for the death this process could witness (`observed …`)."
            );
        }
        let rejected = worker_reject_and_reap_handoff_child(
            job,
            proxy,
            handle,
            candidate,
            nonce,
            proof_outcome,
            format!("handoff proof ended {proof_outcome:?}"),
        );
        debug_assert!(rejected, "Commit is unreachable before ProofReady");
        return;
    }

    let (reject, rejected) = std::sync::mpsc::sync_channel(1);
    let ready = Wake::UpdateHandoffFinished(crate::UpdateHandoffCompletion {
        attempt_id: job.attempt_id,
        nonce: Some(nonce.to_string()),
        child_pid: Some(candidate.pid),
        outcome: crate::UpdateHandoffOutcome::ProofReady,
        commit_fd: Some(commit_wr),
        reject: Some(reject),
        reconcile: None,
        detail: "child painted and proved exact readerless adoption".to_string(),
        input_drain_spins: 0,
        // A candidate that PROVED adoption is alive and holding the masters; it
        // has no death to describe.
        child_death: crate::ChildDeathEvidence::Unobserved,
    });
    if proxy.send_event(ready).is_err() {
        // No main-thread final validation occurred, therefore Commit is impossible.
        // Kill/reap readerless child; never exit as though authority was granted.
        let rejected = worker_reject_and_reap_handoff_child(
            job,
            proxy,
            handle,
            candidate,
            nonce,
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
                job,
                proxy,
                handle,
                candidate,
                nonce,
                crate::UpdateHandoffOutcome::ActivityRevoked,
                "structural activity revoked handoff before Commit".to_string(),
            )
        {
            return;
        }
        if handoff_masters_closed(&job.live)
            && worker_reject_and_reap_handoff_child(
                job,
                proxy,
                handle,
                candidate,
                nonce,
                crate::UpdateHandoffOutcome::Rejected,
                "a handed-off PTY session closed before Commit".to_string(),
            )
        {
            return;
        }
        match rejected.try_recv() {
            Ok(()) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                if worker_reject_and_reap_handoff_child(
                    job,
                    proxy,
                    handle,
                    candidate,
                    nonce,
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
                job,
                proxy,
                handle,
                candidate,
                nonce,
                crate::UpdateHandoffOutcome::Rejected,
                "main-thread final handoff decision timed out".to_string(),
            )
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// Ceiling on the DECODE AUTHORITY one capture may hand its successor: the sum,
/// over every carried session, of twice `dimension_grid_cap`.
///
/// This is authority, not memory. `dimension_grid_cap` prices a carried line at
/// 512 bytes per cell plus 16 KiB of framing precisely so a hostile `meta` cannot
/// authorize a huge decode, and a real screen encodes an order of magnitude or two
/// smaller than that ceiling — so this sum overstates what a capture actually
/// costs by roughly the same factor.
///
/// It was 256 MiB, the number the EXACT guards use on REAL bytes, and that made it
/// the next rung of the same ladder `MAX_HANDOFF_AGGREGATE_GRID_CELLS` was on: one
/// session carrying 256 lines at 110 columns books ~42 MiB of it, so SEVEN
/// ordinary panes hard-failed the capture — with no degrade rung at all. Raising
/// only the cell aggregate would have moved the field failure from five panes to
/// seven instead of removing it.
///
/// 16x that number is the pessimism this pre-check carries over the bound it is
/// standing in for, so at 4 GiB it goes back to being what it is: a cheap early
/// refusal for an absurd pool, with a clearer message than the seams downstream.
/// The exact bound is untouched and still decides — `screen_digest_refs` and
/// `write_outgoing` both charge `MAX_HANDOFF_AGGREGATE_GRID_BYTES` (256 MiB)
/// against the REAL serialized bytes before anything is committed or written.
#[cfg(unix)]
const MAX_HANDOFF_CAPTURE_BUDGET_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Whether one session may spend `optional` on a budget, on top of the
/// `own_mandatory` it must spend regardless.
///
/// `used` is what the pool has already been charged, and `later_mandatory` is the
/// summed non-degradable cost of every session NOT yet admitted. Overflow is a
/// refusal, never a wrap.
///
/// That last term is the whole point, and it is what both capture budgets were
/// missing. They were handed out greedily in pool order: the first sessions each
/// took a full 256 lines of scrollback, and a later session then found no room
/// even for its VISIBLE screen — which is not degradable, so the capture failed
/// and the update did not apply, deterministically, on every retry, for anyone
/// with more than a handful of panes. Reserving the mandatory total up front
/// inverts that: a session can be refused only when the pool's visible-only total
/// genuinely exceeds the ceiling, which is what makes "the failure mode is *less
/// scrollback*, never *the update did not apply*" true rather than aspirational.
#[cfg(unix)]
fn optional_carry_fits(
    used: u64,
    own_mandatory: u64,
    optional: u64,
    later_mandatory: u64,
    ceiling: u64,
) -> bool {
    used.checked_add(own_mandatory)
        .and_then(|total| total.checked_add(optional))
        .and_then(|total| total.checked_add(later_mandatory))
        .is_some_and(|total| total <= ceiling)
}

/// `$ATERM_NO_SEAMLESS_UPDATE` is set: the ONE deliberate opt-out from the overlap
/// handoff. A plain read every time, never memoised. Folded into
/// [`App::seamless_handoff_unavailable`], the reading the apply gate and the
/// status bar's posture share.
#[must_use]
pub(crate) fn seamless_handoff_opted_out() -> bool {
    std::env::var_os("ATERM_NO_SEAMLESS_UPDATE").is_some()
}

/// Why the in-session overlap handoff cannot run in THIS process. ONE predicate
/// ([`App::seamless_handoff_unavailable`]) answers it for both readers — the
/// apply gate in `start_unix_update_handoff` (`seamless_capable`) and the status
/// bar's posture (`App::apply_posture_for`) — so the bar can never promise a
/// landing, or a manual affordance, that the gate refuses. With any of these the
/// admission classifier (`native_update_admission::classify`) admits only the
/// cold lane, which it grants at exactly zero live PTYs: the apply is REFUSED
/// while any terminal is open, no shell dies, and the build lands at the next
/// launch (or once every terminal is closed). Until 2026-08-30 the gate AND-ed
/// four conjuncts while the posture folded only the first, so a `--control-sock`
/// or `--headless` process painted "applies in place within ~2 min" over an apply
/// the gate refused with any terminal open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandoffUnavailable {
    /// `$ATERM_NO_SEAMLESS_UPDATE` — the one deliberate opt-out.
    OptedOut,
    /// `--control-sock <path>` / `$ATERM_CONTROL_SOCK`: an explicit control-socket
    /// path. The successor inherits the variable and would come up on the same
    /// path, which explicit paths keep under the strict never-hijack probe
    /// (`control.rs`) while this process still holds it.
    ExplicitControlSock,
    /// `--headless` / `$ATERM_HEADLESS`: no window to hand across.
    Headless,
    /// No event-loop proxy to drive the handoff's wakes — only
    /// `headless_for_test` builds such an App.
    NoEventLoopProxy,
}

impl HandoffUnavailable {
    /// Every reason, in the order the predicate asks: the deliberate opt-out
    /// first (the one a person set on purpose), then the launch shape, then the
    /// process shape.
    pub(crate) const ALL: [Self; 4] = [
        Self::OptedOut,
        Self::ExplicitControlSock,
        Self::Headless,
        Self::NoEventLoopProxy,
    ];

    /// What is set — the clause the reader can act on, which is why every
    /// sentence built on it leads with it (the status bar truncates from the
    /// right when the window is narrow).
    #[must_use]
    pub(crate) fn cause(self) -> &'static str {
        match self {
            Self::OptedOut => "$ATERM_NO_SEAMLESS_UPDATE is set",
            Self::ExplicitControlSock => "$ATERM_CONTROL_SOCK is set (--control-sock)",
            Self::Headless => "--headless (no window)",
            Self::NoEventLoopProxy => "no event loop",
        }
    }

    /// The log line's remedy, for the reasons that have one.
    #[must_use]
    pub(crate) fn remedy(self) -> &'static str {
        match self {
            Self::OptedOut => " Unset it to restore the default.",
            Self::ExplicitControlSock => {
                " Launch without --control-sock / $ATERM_CONTROL_SOCK to restore the default."
            }
            Self::Headless => " A windowed launch restores the default.",
            Self::NoEventLoopProxy => "",
        }
    }

    /// Whether this reason holds for `app` — a plain read each time.
    fn holds_for(self, app: &App) -> bool {
        match self {
            Self::OptedOut => seamless_handoff_opted_out(),
            Self::ExplicitControlSock => std::env::var_os("ATERM_CONTROL_SOCK").is_some(),
            Self::Headless => app.headless,
            Self::NoEventLoopProxy => app.proxy.is_none(),
        }
    }
}

impl App {
    /// The ONE reading of whether the overlap handoff can run here — the first
    /// true reason in [`HandoffUnavailable::ALL`]'s order, or `None` when the
    /// seamless lane is available. Read by the apply gate
    /// (`start_unix_update_handoff`) and by the status bar's posture
    /// ([`App::apply_posture_for`]), and nowhere else, so the two cannot
    /// disagree. Plain reads only — `var_os` and two fields — never a memo: a
    /// memo whose initializer could call back into its owner parked the main
    /// thread forever on 2026-08-30.
    ///
    /// NOT folded into `arm_native_auto_apply`'s `enabled` on purpose: the cold
    /// lane is a real automatic path for a headless or `--control-sock` process
    /// whose last terminal has closed (the classifier admits it at zero live
    /// PTYs, and the cold spawn re-injects `ATERM_HEADLESS`), so a lane that
    /// never arms would never take it.
    #[must_use]
    pub(crate) fn seamless_handoff_unavailable(&self) -> Option<HandoffUnavailable> {
        HandoffUnavailable::ALL
            .into_iter()
            .find(|why| why.holds_for(self))
    }
}

impl App {
    /// PROOF-CARRYING DSU (RFC Rung 1): APPLY a staged update now by re-execing — the
    /// staged build swaps in at the top of the new `main` (`apply_staged_if_ready`).
    /// Reached from `Wake::ApplyStagedUpdate` (the `aterm-ctl update apply` verb / the
    /// GUI's update-ready nudge). No-op unless a STRICTLY-NEWER build is actually staged
    /// (never a pointless re-exec). Rung 1b live wiring (DEFAULT-ON, opt out with
    /// `ATERM_NO_SEAMLESS_UPDATE`): hands every live PTY master, its exact visible-screen
    /// checkpoint, and a `SessionHandoff` manifest to the new process so the running shell survives
    /// (the round-trip that makes that safe is
    /// proven — `SessionHandoff` + `handoff_roundtrip_model`; the single-use nonce by
    /// `seamless_nonce_model`). Scope: the live process, visible rows, terminal modes/cursors,
    /// and output queued after reader park survive. Preexisting off-screen scrollback is
    /// carried only up to `seamless::MAX_HANDOFF_HISTORY_LINES` (256) per session, and
    /// DROPPED ENTIRELY for a session when the time ladder or the aggregate cell budget
    /// says so — the standing policy is "the failure mode is less scrollback, never the
    /// update did not apply". A tab configured for the default 100,000-line ring
    /// therefore keeps its processes, its visible screen and its queued output across an
    /// in-session update, but not its history. (This sentence used to say scrollback was
    /// "deliberately excluded", which stopped being true when the 256-line carry landed
    /// and understated what survives; 2026-08-19.) A cold relaunch is
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
        let debug_seamless = crate::app_update_screen::debug_seamless_reexec_armed();
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
            // THE CARD IS RETIRED BY WHOEVER BROKE THE PROMISE. `start_unix_update_
            // handoff` raises "Installing update…" just before it parks, and several
            // of its later `Err` exits (a missed 20 ms park deadline, masters closed
            // under it, a reservation failure) return WITHOUT producing a completion
            // — so the completion path cannot be the only place that clears it, or a
            // refused attempt leaves a card claiming an install that never started
            // standing for its whole (deliberately long) lifetime. Binding the result
            // at the ONE call site covers every such exit and cannot rot as new ones
            // are added.
            let started = self.start_unix_update_handoff(
                exe,
                build,
                safety_token,
                mode,
                apply_attempt,
                debug_seamless,
            );
            if started.is_err() {
                self.notice = None;
                self.request_redraw_all_windows();
            }
            return started;
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
            let operator_quiesce = match self.operator_control.as_ref() {
                Some(control) => Some(control.try_begin_update_quiesce().map_err(|error| {
                    crate::UpdateHandoffStartError::failed(format!(
                        "resident operator could not quiesce for replacement: {error}"
                    ))
                })?),
                None => None,
            };
            self.shutdown_title_summaries();
            let spawn = || {
                let mut cmd = std::process::Command::new(exe);
                // Forward argv minus the leading `--window` pins earlier boot
                // swaps prepended — every successor lane strips them so no
                // relaunch path can re-grow the argv (aterm-update's
                // reexec_forwarded_args doc has the accumulation story).
                cmd.args(aterm_update::reexec_forwarded_args(
                    std::env::args_os().skip(1),
                ))
                .env("ATERM_UPDATED_FROM", build.to_string());
                bind_expected_update_artifact(&mut cmd, apply_attempt.as_ref());
                // Same headless re-injection as the unix exec path above (and for
                // the same reason: argv passes through verbatim, so the env
                // channel — the flag's exact equivalent — is what survives an
                // `-e` boundary).
                if self.headless {
                    cmd.env("ATERM_HEADLESS", "1");
                }
                match cmd.spawn() {
                    Ok(_) => std::process::exit(0),
                    Err(error) => Err(error),
                }
            };
            let spawn_result = match operator_quiesce.as_ref() {
                Some(quiesce) => quiesce.with_commit_permit(spawn),
                None => Ok(spawn()),
            };
            match spawn_result {
                Ok(Ok(())) => {
                    return Err(crate::UpdateHandoffStartError::failed(
                        "replacement returned after a successful Windows spawn",
                    ));
                }
                Ok(Err(err)) => {
                    // A failed spawn leaves this process live. Restore a fresh exact
                    // authority/worker after the pre-replacement shutdown.
                    self.reconfigure_title_summaries();
                    aterm_log::warn!("update apply: re-spawn failed: {err}");
                    return Err(crate::UpdateHandoffStartError::failed(format!(
                        "replacement process could not start: {err}"
                    )));
                }
                Err(error) => {
                    self.reconfigure_title_summaries();
                    return Err(crate::UpdateHandoffStartError::failed(format!(
                        "resident operator revoked replacement: {error}"
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
        // THIS EPOCH GATES THE ENTRY, NOT THE FLIGHT — and the comment that
        // stood here said the opposite, four lines above the code that refutes
        // it. `automatic_activity_epoch` reaches exactly two places, both BEFORE
        // the readers park: the quiet-epoch admission just below, and the
        // pre-park TOCTOU re-check. The watcher that can revoke a parked handoff
        // mid-flight is `pending.activity_epoch`, armed UNCONDITIONALLY when the
        // attempt is stored, and read at Commit as the mandatory `exact_activity`
        // fact. So `AutomaticPastGrace` opts out of WAITING for a quiet moment;
        // it does not, and cannot, opt out of being revoked by one that arrives.
        // That is deliberate: the forced lane's job is to land on a machine that
        // is never quiet, not to outrank what the adoption proof must compare.
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
        // ONE name for one boolean. `ATERM_NO_OVERLAP_HANDOFF` used to sit two
        // lines below this, AND-ed into the same value: two spellings of the same
        // opt-out, only one of which appeared in any document, neither of which
        // said anything when honoured. It is gone; this is the opt-out.
        //
        // And it is LOUD. Suppressing the overlap leaves the update only the
        // cold lane — which the classifier below admits for an exact zero-PTY
        // state and nothing else, so with any terminal open the apply is REFUSED
        // (`LivePtysNeedSeamless`), no shell dies, and the build waits for the
        // next launch. A different route from the one the release was tested
        // on, so a binary that takes it says so, once, by name. A silent env var
        // that reroutes an update is how a shipped binary comes to behave
        // differently from the one that was proven. (This line used to say live
        // shells would NOT survive — a kill the classifier never permits; the
        // status bar repeated it until 2026-08-30.)
        //
        // ONE PREDICATE FOR BOTH READERS. Four conjuncts were AND-ed right here —
        // the opt-out, `$ATERM_CONTROL_SOCK`, `headless`, `proxy` — while the
        // status bar's posture folded only the first, so a `--control-sock` or
        // `--headless` process painted "applies in place within ~2 min" over an
        // apply this gate refused (2026-08-30). `seamless_handoff_unavailable`
        // is now the only place the four are read, and every reason is said in
        // the log the way the opt-out always was.
        let handoff_unavailable = self.seamless_handoff_unavailable();
        if let Some(why) = handoff_unavailable {
            aterm_log::warn!(
                "update apply: {} — the seamless overlap handoff is DISABLED for this \
                 process. The update is refused while any terminal session is open; \
                 with none open it re-execs cold (the staged build swaps in at the top \
                 of the new main), which is not the path this build's handoff proofs \
                 cover.{}",
                why.cause(),
                why.remedy()
            );
        }
        let overlap_available = handoff_unavailable.is_none();
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
                    // Leading `--window` pins from earlier boot swaps are
                    // stripped (see the Windows spawn above) — the successor's
                    // own boot swap re-pins exactly one when it needs it.
                    .args(aterm_update::reexec_forwarded_args(
                        std::env::args_os().skip(1),
                    ))
                    .env("ATERM_UPDATED_FROM", build.to_string());
                // Same rule as the seamless lane below: an ACTIVATION binds no
                // expected artifact. The successor swaps nothing; binding the
                // activation digest would make its `apply_staged_if_ready` refuse a
                // newer `ready.toml` as "no longer matches" and write a spurious
                // apply refusal into the ledger.
                bind_expected_update_artifact(
                    &mut command,
                    apply_attempt
                        .as_ref()
                        .filter(|attempt| !attempt.is_installed_activation()),
                );
                // Headless survives the re-exec. The ENV channel, not the flag,
                // is right here even though `--headless` is the canonical
                // user-facing spelling: the successor inherits our argv (minus
                // the leading pins), and argv may carry an `-e`/`--command`
                // boundary that swallows every token after it — an appended flag
                // would become part of the child's command line, and a prepended
                // one would reorder a payload we must pass through unchanged.
                // `$ATERM_HEADLESS=1` is an exact equivalent of the flag and is
                // consumed once at the successor's boot, so this re-injection is
                // the whole handoff and it leaks nowhere further.
                if self.headless {
                    command.env("ATERM_HEADLESS", "1");
                }
                let operator_quiesce = match self.operator_control.as_ref() {
                    Some(control) => Some(control.try_begin_update_quiesce().map_err(|error| {
                        crate::UpdateHandoffStartError::failed(format!(
                            "resident operator could not quiesce for replacement: {error}"
                        ))
                    })?),
                    None => None,
                };
                self.shutdown_title_summaries();
                let error = match operator_quiesce.as_ref() {
                    Some(quiesce) => match quiesce.with_commit_permit(|| command.exec()) {
                        Ok(error) => error,
                        Err(error) => {
                            self.reconfigure_title_summaries();
                            return Err(crate::UpdateHandoffStartError::failed(format!(
                                "resident operator revoked replacement: {error}"
                            )));
                        }
                    },
                    None => command.exec(),
                };
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
        let preverified = apply_attempt
            .as_ref()
            .and_then(|attempt| self.cached_handoff_preverification(attempt));
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

        // Capture the session registry BEFORE parking, because this projection can
        // FAIL: a `WouldBlock` here must return with the terminal completely
        // untouched, which is only true while no reader has been stopped. It
        // performs no disk I/O.
        //
        // The restore manifest deliberately does NOT travel with it — see the
        // post-park capture below for why capturing it here revoked the handoff.
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

        // PROBED BEFORE THE PARK, and this is the reason: resolving the control
        // directory can create and `chmod` it, and every instruction between
        // `park_all_readers` and the capture deadline is spent inside a 20 ms
        // budget with the user's terminal frozen. The answer cannot change in
        // that window — it is a function of `$HOME` and this process's pid — so
        // taking it here costs the attempt nothing and keeps a filesystem
        // round-trip out of the parked interval. (The rest of the lane decision
        // — arithmetic plus one `fstat` per session — now sits below the worker
        // spawn for the same reason; this fact moves further up only because it
        // can touch the filesystem.)
        #[cfg(target_os = "macos")]
        let rendezvous_path_fits = crate::handoff_rendezvous::rendezvous_path_fits();
        // Same reasoning, same window: `app_bundle_root` ends in an `is_dir`.
        #[cfg(target_os = "macos")]
        let bundle = app_bundle_root(&exe);

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

        // HOISTED OUT OF THE PARKED WINDOW. Nothing below this comment consumes
        // a byte from a master, and nothing above the park has stopped one yet:
        // the attempt's identity, its successor `Command`, and the LANE DECISION
        // with its proof identities are all pure functions of facts settled
        // BEFORE the park — the pool snapshot, the dup'd masters, the apply
        // ticket. What remains under the park is exactly what DEPENDS on the
        // parked state: the two digests over the captured screens and layout.
        //
        // WHAT THIS BUYS, precisely — an earlier version of this comment (and of
        // the CHANGELOG and the RFC) claimed the capture ladder "gets that
        // budget back", and that was FALSE: in the parent revision this block
        // ran AFTER the ladder had already finished, so the ladder was never
        // charged a microsecond of it and its `park_at + 20ms` window is
        // byte-identical either way. What the move actually does is (a) take
        // this work out of the frozen interval altogether, which is the freeze
        // the user feels, and (b) stop attempts dying at the deadline check that
        // follows the digests, which this block used to push past 20 ms.
        //
        // It also mints the attempt id before the park, so an attempt that dies
        // after this point burns one `u64` id. Nothing keys on id contiguity.
        //
        // These early returns therefore do NOT roll back the overlap — no
        // reader is parked, so there is nothing to re-attach. (What it would do
        // here is not nothing: `rollback_overlap` re-asserts a `set_cloexec` the
        // mandatory loop above already established, and schedules a reader
        // resume for readers that were never stopped. Both are harmless; neither
        // describes what happened at this point, which is why the call is gone
        // rather than kept "just in case".) A return also
        // drops `job_tx`, which is how the worker spawned just above learns to
        // exit: the same shape the activity re-check below already relies on.

        let attempt_id = self.next_update_handoff_id;
        let Some(next_attempt_id) = attempt_id.checked_add(1) else {
            return Err(crate::UpdateHandoffStartError::failed(
                "handoff identity space is exhausted",
            ));
        };
        self.next_update_handoff_id = next_attempt_id;
        let mut command = std::process::Command::new(exe);
        command
            // Leading `--window` pins stripped, as on the cold/Windows lanes.
            .args(aterm_update::reexec_forwarded_args(
                std::env::args_os().skip(1),
            ))
            .env("ATERM_UPDATED_FROM", build.to_string());
        // An ACTIVATION binds no expected artifact: the successor has nothing to swap
        // (its `apply_staged_if_ready` finds no newer stage and returns NoUpdate) and
        // it simply IS the authorized build — the identity check below still names it.
        let installed_activation = apply_attempt
            .as_ref()
            .is_some_and(|attempt| attempt.is_installed_activation());
        bind_expected_update_artifact(
            &mut command,
            apply_attempt.as_ref().filter(|_| !installed_activation),
        );
        let target_build = apply_attempt
            .as_ref()
            .map_or(build, |attempt| attempt.target_build());
        let target_commit = apply_attempt.as_ref().map_or_else(
            || crate::build_info::GIT_COMMIT.to_string(),
            |attempt| attempt.target_commit().to_string(),
        );
        // THE LANE IS DECIDED HERE AND NOWHERE ELSE, and it has to be settled
        // before the pending attempt is recorded: it selects which term the
        // adoption proof hashes, and the main thread re-derives that proof from
        // the pending attempt at Commit time. A lane chosen later would leave
        // the two halves of one proof speaking different terms — the exact
        // failure that shows up as an unexplained `AdoptionMismatch`.
        //
        // Both arms produce `(lane, proof identities)` together for the same
        // reason: they are one decision, not two that have to agree.
        #[cfg(target_os = "macos")]
        let (lane, proof_identities) = {
            let facts = HandoffLaneFacts {
                bundled: bundle.is_some(),
                // A build for this platform has one compiled in. The fact is
                // still a field rather than an assumption so the refusal it
                // guards is reachable from a test.
                launcher_available: true,
                socket_path_fits: rendezvous_path_fits,
                target_not_older: target_build >= build,
                sessions: adoption.len(),
                environment_is_a_merge: launch_environment(&command).is_some(),
            };
            match out_of_band_lane_refusal(facts) {
                None => {
                    match crate::handoff_rendezvous::proof_identities_in_device_terms(&adoption) {
                        Some(terms) => (HandoffLane::OutOfBand, terms),
                        // A master that will not answer `fstat` cannot be given a
                        // device term, and a proof missing one term is a proof over
                        // a different session set. Forking is exact here rather than
                        // degraded: the fd-number term needs nothing from the kernel.
                        None => {
                            aterm_log::warn!(
                                "update apply: forking instead of launching — a handed-off PTY would \
                             not answer fstat, so the out-of-band proof term cannot be computed"
                            );
                            (HandoffLane::Fork, adoption.clone())
                        }
                    }
                }
                Some(reason) => {
                    aterm_log::info!("update apply: forking instead of launching — {reason}");
                    (HandoffLane::Fork, adoption.clone())
                }
            }
        };
        // Every other unix has exactly one transport, so the proof term is the
        // descriptor number and there is no choice to record.
        #[cfg(not(target_os = "macos"))]
        let proof_identities = adoption.clone();
        if self.update_handoff_activity_epoch == u64::MAX {
            return Err(crate::UpdateHandoffStartError::failed(
                "handoff activity identity space is exhausted",
            ));
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
        // SAY IT BEFORE THE SCREEN STOPS. Everything below parks the readers, and
        // from here until the successor attaches its own the terminal echoes
        // nothing — and, once the kernel PTY buffer fills, the user's own programs
        // stall against it. That is a defensible few seconds; being given it with
        // no explanation is not. Raised HERE on purpose: above this line every
        // early return is a refusal that never froze anything (the card would be a
        // lie), and below it the allocation would land inside the 20 ms budget this
        // function hoists work out of.
        //
        // A CARD, NEVER A STATUS-BAR ROW. A row re-grids, which moves `ws.rows` —
        // the exact value `exact_layout` compares un-normalized at Commit — so the
        // bar explaining the freeze would REFUSE the update after the user sat
        // through it; and the re-grid's SIGWINCH is itself activity the plain
        // automatic lane's own re-check reads as a revocation.
        self.surface_update_status_for(
            "\u{2191} Installing update — your shells are safe; the screen pauses for a moment.",
            crate::HANDOFF_PENDING_NOTICE_TTL,
        );
        const HANDOFF_HISTORY_COMFORT: std::time::Duration = std::time::Duration::from_millis(10);
        // THE INSTANT THE TERMINAL STOPS ECHOING — the start of the freeze the
        // user experiences, and the zero point of the two numbers reported at
        // Commit. The 20 ms capture deadline hangs off the same stamp.
        let park_at = std::time::Instant::now();
        let deadline = park_at + std::time::Duration::from_millis(20);
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
        // GATED ON THE LANE, NOT ON THE ACTIVITY EPOCH. Session DEATH is a
        // safety fact, not an idleness preference: `AutomaticPastGrace` opts out
        // of activity revocation above, but it must still refuse to commit an
        // adoption proof whose live set died underneath it.
        if mode.is_automatic() && handoff_masters_closed(&live) {
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
        // Latched once the aggregate cell budget has refused a carried-history
        // checkpoint: every later session goes straight to visible-only rather than
        // paying a probe that this pool has already proven cannot pass. TWO causes
        // arm it — the budget refusing the carry, and the produced blob refusing the
        // wire's shape — which is why it is named for its EFFECT and not for either
        // one of them.
        let mut history_latched_off = false;
        // MANDATORY FIRST, OPTIONAL SECOND. Price the visible+alt grids of the WHOLE
        // POOL before charging any of it, in both budgets. Without this the budgets
        // are spent greedily in pool order and a later session finds nothing left
        // for the one thing it cannot degrade — see `optional_carry_fits`. Readers
        // are parked, so no geometry can move between this pass and the capture loop
        // below, and the two passes therefore price the same pool.
        //
        // An unreadable engine reserves EVERYTHING, i.e. nobody carries scrollback:
        // the capture loop reports that busy engine itself a moment later, and a
        // guess made here must never be the optimistic one.
        let mut cells_reserve = 0_u64;
        let mut bytes_reserve = 0_u64;
        for session in self.pool.iter() {
            let terminal = match session.term.try_lock() {
                Ok(guard) => guard,
                Err(std::sync::TryLockError::Poisoned(poison)) => poison.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    cells_reserve = u64::MAX;
                    bytes_reserve = u64::MAX;
                    break;
                }
            };
            let (rows, cols) = (terminal.rows(), terminal.cols());
            let mandatory_bytes =
                crate::seamless::checkpoint_capture_budget_bytes(rows, cols, 0).unwrap_or(u64::MAX);
            cells_reserve = cells_reserve
                .saturating_add(crate::seamless::mandatory_checkpoint_cells(rows, cols));
            bytes_reserve = bytes_reserve.saturating_add(mandatory_bytes);
        }
        for session in self.pool.iter() {
            if std::time::Instant::now() >= deadline {
                capture_failed = Some("bounded visible-screen capture exceeded 20 ms".to_string());
                break;
            }
            let terminal = match session.term.try_lock() {
                Ok(guard) => guard,
                Err(std::sync::TryLockError::Poisoned(poison)) => poison.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    capture_failed =
                        Some("a terminal engine was busy during handoff capture".to_string());
                    break;
                }
            };
            if !terminal.parser_is_ground() {
                capture_failed =
                    Some("a terminal parser was mid-sequence during handoff capture".to_string());
                break;
            }
            // SCROLLBACK IS BEST-EFFORT AND MUST NEVER COST THE HANDOFF.
            //
            // Carrying history is what stops an in-session update truncating every
            // tab to one screen, but it is strictly a bonus: the visible screen is
            // what adoption actually requires. So the depth is chosen per session
            // against the REMAINING time, and collapses to zero once the window is
            // more than half spent — and against the REMAINING budget, once the
            // pool's own mandatory reservation is taken off the top. Failing the
            // handoff to protect scrollback would trade the whole update for the
            // thing the update was carrying.
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let (rows, cols) = (terminal.rows(), terminal.cols());
            let own_cells = crate::seamless::mandatory_checkpoint_cells(rows, cols);
            let own_bytes =
                crate::seamless::checkpoint_capture_budget_bytes(rows, cols, 0).unwrap_or(u64::MAX);
            // Both reserves now cover only the sessions AFTER this one, so the two
            // checks below price this session's scrollback against what everybody
            // still waiting genuinely needs, and never against its own mandatory
            // cost twice.
            cells_reserve = cells_reserve.saturating_sub(own_cells);
            bytes_reserve = bytes_reserve.saturating_sub(own_bytes);
            let carry = crate::seamless::max_handoff_history_lines();
            let history_cells = u64::from(cols).saturating_mul(u64::from(carry));
            let history_bytes = crate::seamless::checkpoint_capture_budget_bytes(rows, cols, carry)
                .map_or(u64::MAX, |carried| carried.saturating_sub(own_bytes));
            let history_fits_cells = optional_carry_fits(
                capture_cells,
                own_cells,
                history_cells,
                cells_reserve,
                crate::seamless::max_handoff_aggregate_grid_cells(),
            );
            let history_fits_bytes = optional_carry_fits(
                capture_budget,
                own_bytes,
                history_bytes,
                bytes_reserve,
                MAX_HANDOFF_CAPTURE_BUDGET_BYTES,
            );
            let mut history = if remaining >= HANDOFF_HISTORY_COMFORT
                && !history_latched_off
                && history_fits_cells
                && history_fits_bytes
            {
                carry
            } else {
                0
            };
            // Reject decoded allocation dimensions BEFORE the checkpoint serializes
            // anything. Conservatively reserve main+alt because querying the copied
            // checkpoint to discover alt presence is exactly the potentially
            // expensive work this admission must precede.
            let mut per_grid = crate::seamless::admit_checkpoint_dimensions(
                &mut capture_cells,
                rows,
                cols,
                history,
                true,
            );
            // DEGRADE ON BUDGET, exactly as the note above promises — this is the
            // arm that was missing, and its absence is why an update could refuse
            // to install forever.
            //
            // The ladder above reacts only to TIME. `admit_checkpoint_dimensions`
            // also refuses on SPACE: the process-wide
            // `MAX_HANDOFF_AGGREGATE_GRID_CELLS` is charged
            // `cols * (2*rows + history)` per session across every tab and pane of
            // every window, and carrying 256 history lines multiplies what a session
            // costs. So a handful of ordinary sessions exhausts it — and a refusal
            // there used to fall straight through to `capture_failed`, trading the
            // whole update for the scrollback it was carrying. That is the precise
            // failure this comment's own "MUST NEVER COST THE HANDOFF" forbids, and
            // it is deterministic: same sessions, same geometry, same constants
            // every attempt, so the automatic lane retried it indefinitely.
            //
            // A refusal costs nothing to recover from: the admission mutates the
            // aggregate ONLY on success ("an over-budget checkpoint cannot leave
            // partial authority"), so re-probing visible-only is exact, not
            // approximate. The latch stops every later session paying the same
            // doomed probe once the budget is known to be tight.
            if per_grid.is_none() && history != 0 {
                // SAY SO. Dropping a tab's history is invisible to the user until they
                // scroll up and find it gone, on a machine configured for 100,000
                // lines. The decision is deliberate and stays deliberate — but it is
                // recorded where every other handoff decision is (2026-08-19, raised
                // by an external reviewer of the product's public claim).
                aterm_log::info!(
                    "update apply: carrying no scrollback for this session — the \
                     handoff's aggregate cell budget cannot fit {history} history \
                     line(s) beside the visible {rows}x{cols} screen. Processes, the \
                     visible screen and queued output still survive the update."
                );
                history = 0;
                history_latched_off = true;
                per_grid = crate::seamless::admit_checkpoint_dimensions(
                    &mut capture_cells,
                    rows,
                    cols,
                    0,
                    true,
                );
            }
            // Only now is a refusal real: the VISIBLE screen is what adoption
            // requires, so a checkpoint that cannot be admitted even without history
            // is a genuine blocker rather than a carried bonus. Name the cap that
            // actually bound — the aggregate is in grid CELLS, and reporting every
            // refusal as the byte cap sent this exact investigation looking for a
            // 256 MiB allocation that was never involved.
            if per_grid.is_none() {
                capture_failed = Some(
                    "visible-screen capture exceeded the aggregate grid-cell budget".to_string(),
                );
                break;
            }
            capture_budget = per_grid
                .and_then(|bytes| bytes.checked_mul(2))
                .and_then(|bytes| capture_budget.checked_add(bytes))
                .unwrap_or(u64::MAX);
            // Same ladder, one budget over. This charge is not degradable here —
            // the history that would have been dropped was already refused above by
            // `history_fits`, which prices this ceiling too — so reaching it means
            // the pool's MANDATORY visible screens alone do not fit, which is a
            // genuine blocker rather than a carried bonus. Name the authority, not
            // "memory": nothing here allocates these bytes, they are the decode
            // ceiling the successor would be granted.
            if capture_budget > MAX_HANDOFF_CAPTURE_BUDGET_BYTES {
                capture_failed = Some(
                    "visible-screen capture exceeded the aggregate decode-authority budget"
                        .to_string(),
                );
                break;
            }
            let Some(mut checkpoint) = terminal.checkpoint_carry(history as usize) else {
                capture_failed =
                    Some("a terminal parser left Ground during handoff capture".to_string());
                break;
            };
            // DEGRADE ON SHAPE — the sibling of the budget arm above, and the arm
            // whose absence stranded a machine on an update it had already
            // downloaded and verified.
            //
            // The budget ladder prices TIME and SPACE. It cannot see the third way
            // a carry can be wrong: the produced blob not matching the shape the
            // wire allows. That is what `checkpoint_shape_refusal` asks, and until
            // now nobody asked it here — the checkpoint went straight into
            // `screens` and the first shape check ran in `screen_digest`, past the
            // point where the carry could still be lowered, with nothing left to do
            // but refuse the whole update. It refused every ~10 minutes for four
            // days.
            //
            // Lowering the carry is the cure for this whole CLASS, not one bug: the
            // inactive grid's blob is the only one whose record count the wire
            // cannot describe, and at `history == 0` it is exactly `rows` records by
            // construction, whatever the producer believed about the slot.
            //
            // Re-probing is exact rather than approximate, for the same reason the
            // budget arm gives: nothing here has been published yet. The aggregates
            // are function locals, `screen_digest`'s own aggregate is fresh per
            // call, and this session's cells were already admitted at the HIGHER
            // carry — so a re-carry at 0 leaves the pool charged for history it no
            // longer holds. That over-charge is deliberate: it is conservative, it
            // only ever makes a LATER session degrade sooner, and refunding a
            // transactional admission would be the kind of partial-authority
            // bookkeeping `admit_checkpoint_dimensions` exists to avoid.
            if let Some(refusal) =
                crate::seamless::checkpoint_shape_refusal(session.id, &checkpoint)
            {
                if history == 0 {
                    // Nothing left to lower. A visible-only capture that still will
                    // not take the wire's shape is a genuine blocker, and now it
                    // says which check refused instead of dying anonymously.
                    capture_failed = Some(format!(
                        "a visible checkpoint could not be shaped for the wire even without carried scrollback ({refusal})"
                    ));
                    break;
                }
                aterm_log::warn!(
                    "update apply: carrying no scrollback for the rest of this handoff — the carried shape was refused ({refusal}). Processes, the visible screen and queued output still survive the update."
                );
                // Only the LATCH carries this decision forward. Unlike the budget
                // arm — which lowers `history` before the carry that reads it —
                // this one runs after, and re-carries explicitly below; assigning
                // `history` here would be writing to a value nothing reads again.
                history_latched_off = true;
                let Some(visible_only) = terminal.checkpoint_carry(0) else {
                    capture_failed =
                        Some("a terminal parser left Ground during handoff capture".to_string());
                    break;
                };
                if let Some(refusal) =
                    crate::seamless::checkpoint_shape_refusal(session.id, &visible_only)
                {
                    capture_failed = Some(format!(
                        "a visible checkpoint could not be shaped for the wire even without carried scrollback ({refusal})"
                    ));
                    break;
                }
                checkpoint = visible_only;
            }
            screens.push((session.id, checkpoint));
            if std::time::Instant::now() >= deadline {
                capture_failed = Some("bounded visible-screen capture exceeded 20 ms".to_string());
                break;
            }
        }
        if capture_failed.is_none() && std::time::Instant::now() >= deadline {
            capture_failed = Some("bounded visible-screen capture exceeded 20 ms".to_string());
        }
        if let Some(reason) = capture_failed {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::failed(format!(
                "{reason}; update handoff stayed in place"
            )));
        }

        // POST-PARK, AND DELIBERATELY SO. Every reader is stopped and joined, and
        // the capture loop above just proved every session's `term` was lockable,
        // so `restore_session_meta`'s try_lock cannot silently degrade cwd/title
        // here.
        //
        // This used to be captured before the park, and that is a self-inflicted
        // revocation: `restore_session_meta` returns `(None, String::new())` on a
        // contended try_lock, and a producing session's reader thread holds its
        // term mutex for the whole of each `process()` slice. So a busy machine
        // captured a DEGRADED layout, `collect_handoff_commit_facts` later compared
        // it against the free post-park projection, `exact_layout` went false, and
        // the attempt was reported as "window/tab/pane topology changed during
        // async preparation" — a wrong cause, on a lane (`AutomaticPastGrace`,
        // `Immediate`) whose ENTRY is not gated on a quiet epoch — the guard
        // itself still applies to all of them — burning an
        // `ActivityRevoked` cycle every time. A shell writing OSC 7 during the park
        // did the same thing with no lock contention involved.
        //
        // After the capture loop rather than immediately after `park_all_readers`:
        // the scrollback-compression worker can still transiently hold a `term`
        // mutex once readers park, and if it did, the loop above has already
        // aborted the attempt cleanly ("a terminal engine was busy during handoff
        // capture") instead of producing a degraded layout here.
        let layout = self.capture_restore_manifest();

        let Some(layout_digest) = crate::seamless::layout_digest(&layout) else {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::failed(
                "handoff layout could not be committed canonically",
            ));
        };
        // The refusal REASON rides the message: this arm used to surface a dozen
        // distinct causes as one opaque sentence, so a stuck update was invisible
        // until someone read the source. It reaches the user through
        // `aterm ctl update status`'s `apply_failure=`.
        let screen_digest = match crate::seamless::screen_digest(&screens) {
            Ok(digest) => digest,
            Err(refusal) => {
                self.rollback_overlap(None, &live);
                return Err(crate::UpdateHandoffStartError::failed(format!(
                    "visible checkpoint set could not be committed canonically: {refusal}"
                )));
            }
        };
        if std::time::Instant::now() >= deadline {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::failed(
                "handoff proof capture exceeded the 20 ms deadline",
            ));
        }
        let activity_epoch = self.update_handoff_activity_epoch;
        let arbiter = crate::HandoffAttemptArbiter::new();
        self.pending_update_handoff = Some(crate::PendingUpdateHandoff {
            attempt_id,
            park_at,
            proof_ready_at: None,
            nonce: None,
            live: live.clone(),
            // The PROOF identities, in whichever term this attempt's lane can
            // prove — see `HandoffWorkerJob::proof_identities`. Never `live`.
            adoption: proof_identities.clone(),
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
            park_at,
            current_build: build,
            target_build,
            target_commit,
            verify_staged_candidate,
            installed_activation,
            command,
            manifest,
            fds,
            screens,
            window,
            layout,
            layout_digest,
            screen_digest,
            live: adoption,
            proof_identities,
            #[cfg(target_os = "macos")]
            lane,
            // Set by the WORKER immediately before the candidate is launched; the
            // main thread must not touch the staging directory here.
            trial_launches_before: 0,
            #[cfg(target_os = "macos")]
            bundle,
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
    /// across a mandatory dispatch FENCE — measured in loop iterations that
    /// really dispatched events, not in wall clock alone. Re-posting this
    /// exact completion gives the run loop time to dispatch those events —
    /// their bytes flow through the tolerated input path into the
    /// still-open PTY masters — and the re-post re-runs this admission
    /// against a drained queue. Bounded by the drain DEADLINE below (3 s,
    /// which at the 2 ms yield is reached around spin ~1500 — the 4000
    /// spin cap is the backstop behind it, not the operative bound; this
    /// comment used to name the cap and claim sustained typing exhausts
    /// it into an activity revocation, which was unreachable arithmetic)
    /// and absolutely by the worker's 15 s decision deadline. A failed re-post means the event
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
    /// `egress_settled`). A quiet window used to be PREFERRED on top of
    /// them — waited for up to 400 ms, then abandoned.
    ///
    /// THAT WAIT IS GONE (2026-08-27), and deleting it weakened no
    /// admission fact: `quiet_window_settled` was read in the respin
    /// condition and nowhere else, and `handoff_commit_admitted`'s
    /// eleven-way conjunction never mentioned it. What it cost was real.
    /// `user_input_recent` reads the MACHINE-WIDE kernel HID idle clock,
    /// so any mouse twitch anywhere on the desktop held the gate to its
    /// full 400 ms ceiling; and after the reveal the successor is the key
    /// window, so the keystrokes it was DEFERRING held the PARENT's gate
    /// open on a queue the parent could no longer receive from — typing
    /// lengthened the very freeze that was swallowing the typing. The
    /// gate now settles at its ~30-45 ms floor: the mandatory dispatch
    /// fence, and egress that has actually reached the kernel.
    #[cfg(unix)]
    fn handoff_drain_gate(
        &mut self,
        completion: crate::UpdateHandoffCompletion,
    ) -> Option<(crate::UpdateHandoffCompletion, bool, bool)> {
        const HANDOFF_INPUT_DRAIN_SPIN_CAP: u32 = 4_000;
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
                // Same edge, so the same stamp: the first ProofReady observation
                // is when the successor's proof landed, which closes the park→proof
                // half of the freeze and opens proof→commit.
                pending.proof_ready_at.get_or_insert(now);
                now.saturating_duration_since(*pending.commit_drain_started.get_or_insert(now))
            })
        };
        let drained_for = drained_for.unwrap_or(HANDOFF_INPUT_DRAIN_DEADLINE);
        // The fence needs BOTH a completed re-post (so the loop really
        // iterated) and the elapsed floor.
        let input_dispatch_fenced =
            completion.input_drain_spins >= 1 && drained_for >= HANDOFF_INPUT_DISPATCH_FENCE;
        let egress_settled = self
            .pending_update_handoff
            .as_ref()
            .map(|pending| pending.live.clone())
            .is_none_or(|live| handoff_egress_settled(&self.pool, &live));
        if (!input_dispatch_fenced || !egress_settled)
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
        // TOPOLOGY, not the raw capture — see `commit_layout_topology` for the
        // two fields it normalizes away and why each of them was rejecting
        // perfectly healthy attempts. The raw inequality is still worth one log
        // line: it is the only place a window drag or a contended session lock
        // becomes visible in the field, and the whole point of this change is
        // that neither may ever again be REPORTED as a topology change.
        let live_layout = self.capture_restore_manifest();
        let exact_layout =
            commit_layout_topology(&live_layout) == commit_layout_topology(&pending_layout);
        if exact_layout && live_layout != pending_layout {
            aterm_log::info!(
                "update apply: the Commit-time capture differs from the committed snapshot \
                 only in window position or degradable session metadata — topology is \
                 unchanged, so Commit stays admitted"
            );
        }
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
                child_death: _child_death,
            } = completion;
            // Quiesce the resident operator only for the final admission seam.
            // A rejection drops this reversible token; a successful Commit
            // `_exit`s while its gate is still held by `with_commit_permit`.
            let (operator_quiesce, mut operator_quiesce_error) =
                match self.operator_control.as_ref() {
                    Some(control) => match control.try_begin_update_quiesce() {
                        Ok(quiesce) => (Some(quiesce), None),
                        Err(error) => (None, Some(error)),
                    },
                    None => (None, None),
                };
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
            let commit_admitted =
                handoff_commit_admitted(facts) && operator_quiesce_error.is_none();
            let mut commit_lost_arbiter = false;
            let mut commit_write_failed = false;
            if commit_admitted && let (Some(commit_fd), Some(proof)) = (commit_fd.as_ref(), proof) {
                if arbiter.try_begin_commit() {
                    // THE FREEZE, IN TWO NUMBERS. Everything from the park to here
                    // is a terminal that echoed nothing; the split says which half
                    // to attack. park→proof is the successor's bundle swap, second
                    // `execve` and cold GUI boot; proof→commit is this process's
                    // dispatch fence plus egress settle. Emitted once per apply, on
                    // the path that ends in `_exit`, so it is the last thing this
                    // process says about a freeze the user just sat through.
                    let (park_to_proof_ms, proof_to_commit_ms) = self
                        .pending_update_handoff
                        .as_ref()
                        .map(|pending| {
                            let now = std::time::Instant::now();
                            let proof = pending.proof_ready_at.unwrap_or(now);
                            (
                                proof.saturating_duration_since(pending.park_at).as_millis(),
                                now.saturating_duration_since(proof).as_millis(),
                            )
                        })
                        .unwrap_or((0, 0));
                    aterm_log::info!(
                        "update apply: committing exact readerless handoff to child {:?} \
                         (screen was frozen {park_to_proof_ms}ms park->proof + \
                         {proof_to_commit_ms}ms proof->commit)",
                        child_pid
                    );
                    // Success cannot return: `commit_and_exit` performs the one
                    // atomic <=PIPE_BUF write and `_exit(0)` in the same typed
                    // operation. EPIPE explicitly transfers Committing back to
                    // Rejecting so one reaper can restore the parent.
                    let commit_result = match operator_quiesce.as_ref() {
                        Some(quiesce) => quiesce.with_commit_permit(|| {
                            crate::seamless::commit_and_exit(commit_fd, proof)
                        }),
                        None => Ok(crate::seamless::commit_and_exit(commit_fd, proof)),
                    };
                    match commit_result {
                        Ok(Err(_)) => commit_write_failed = true,
                        Err(error) => operator_quiesce_error = Some(error),
                        Ok(Ok(never)) => match never {},
                    }
                    let _ = arbiter.commit_failed_to_rejecting();
                } else {
                    commit_lost_arbiter = true;
                }
            }

            let rejection = operator_quiesce_error.map_or_else(
                || {
                    handoff_rejection_reason(
                        facts,
                        &native_safety,
                        commit_lost_arbiter,
                        commit_write_failed,
                    )
                },
                |error| format!("resident operator could not quiesce for Commit: {error}"),
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
        let Some(teardown) = self.reduce_returned_handoff_completion(completion) else {
            return;
        };
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

    /// The cached pre-park verification verdict for THIS exact artifact, or `None`
    /// when there is no fresh entry bound to its build and commit.
    ///
    /// `Some(false)` is the only answer with authority: it short-circuits
    /// [`Self::start_unix_update_handoff`] before a single reader parks. Split out
    /// of that function so the verdict a fixture seeds can be read back the way
    /// production reads it — a test that parks nothing (every headless one, since
    /// `native_update_admission` refuses the seamless lane without an event-loop
    /// proxy) cannot otherwise tell a healthy candidate from one production would
    /// decline outright, which is how a whole retry-policy suite came to be written
    /// against a `passed: false` fixture.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn cached_handoff_preverification(
        &self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
    ) -> Option<bool> {
        let cached = self
            .handoff_preverified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cached
            .as_ref()
            .filter(|entry| {
                entry.build == attempt.target_build()
                    && entry.commit == attempt.target_commit()
                    && entry.artifact == attempt.target_dmg_sha256()
                    && entry.at.elapsed() < crate::HANDOFF_PREVERIFY_FRESHNESS
            })
            .map(|entry| entry.passed)
    }

    /// Reduce one RETURNED (non-ready) handoff completion: resume the parked
    /// readers, reduce the exact apply ticket against the lane its mode
    /// authorized, surface the verdict, and hand back the structural teardown the
    /// caller must replay. `None` means there was no matching pending attempt to
    /// reduce and nothing may be replayed.
    ///
    /// SPLIT OUT OF [`Self::finish_update_handoff`] SO IT CAN BE PROVEN. Every
    /// line here is event-loop-free — the `&ActiveEventLoop` the caller holds is
    /// needed only by the teardown replay below it — and this is the only place
    /// the attempt's `ApplyMode` still exists, which makes it the only place the
    /// automatic/person-initiated split can be gotten right (see
    /// [`crate::app_native::HandoffFailureLane`]).
    #[cfg(unix)]
    fn reduce_returned_handoff_completion(
        &mut self,
        completion: crate::UpdateHandoffCompletion,
    ) -> Option<crate::DeferredHandoffTeardown> {
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
            child_death,
        } = completion;
        let pending = self.pending_update_handoff.take()?;
        // Lane classification for the bounded automatic retry budgets, from the two
        // typed facts this reduction still holds: the mode the apply was authorized
        // under, and the worker's outcome (plus the main thread's own
        // activity-shaped rejection flag, the other half of the activity
        // observation).
        //
        // THE MODE IS CARRIED, NOT DROPPED. It used to reach only this line and
        // then vanish, so a failure a PERSON asked for was charged to the
        // automatic budget — converging the background lane on human retries and
        // silencing the pill for the human who asked.
        //
        // AND NEITHER IS THE OUTCOME, WHICH IS THE SAME BUG ONE LEVEL DOWN. The
        // outcome used to be collapsed into a bare `activity_revoked` bool here,
        // so `TimedOut` (a missed deadline on a busy machine) and
        // `AdoptionMismatch` (two images that cannot agree on a proof) arrived at
        // the budget indistinguishable and were charged the same nine attempts
        // across fourteen hours. `classify` now sees the kind and
        // `PhysicalFailureShape` decides what it costs.
        //
        // …AND `child_death` IS THE THIRD FACT, for the one outcome that is not a
        // classification of anything: `ChildDied` is proof EOF, which a successor
        // that refused, one that faulted and one the machine starved all produce
        // identically. What the worker SAW at that instant travels here beside the
        // outcome rather than being re-inferred from a message string. See
        // [`crate::ChildDeathEvidence`].
        let lane = crate::app_native::HandoffFailureLane::classify(
            pending.mode,
            outcome,
            child_death,
            pending.revoked_by_activity,
        );
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
        // QA SEAM, READ BEFORE THE MATCH CONSUMES IT. A `None` ticket reaches this
        // reduction from exactly one place — `ATERM_DEBUG_SEAMLESS_REEXEC`, which
        // `start_native_update_handoff` is the only caller allowed to pair with a
        // missing attempt — so this outcome describes a SIMULATED apply of the
        // running binary, not a staged build that would not run. It must be logged
        // and shown, and it must not touch the durable ledger; the apply streak it
        // used to write is cleared only by a real successful apply, so QA runs
        // accrued forever and escalated to the persistent-failure notification.
        let debug_seam = pending.apply_attempt.is_none();
        let surfaced = match (pending.apply_attempt, reconcile) {
            (Some(attempt), Some(facts)) => self.finish_async_native_update_handoff(
                attempt,
                facts,
                format!("overlap handoff failed safely: {detail}"),
                lane,
            ),
            (None, _) => Some(crate::native_app::UpdateOutcome::Failed {
                message: format!("debug overlap handoff failed safely: {detail}"),
            }),
            (Some(attempt), None) => Some(self.abort_reaped_native_apply_before_reconcile(
                &attempt,
                format!("overlap handoff failed safely: {detail}"),
                lane,
            )),
        };
        if let Some(surfaced) = surfaced {
            // The source names the lane the attempt actually rode, so the log can
            // no longer report a person's Version-menu apply as background work.
            let source = if pending.mode.is_automatic() {
                "automatic handoff"
            } else {
                "manual handoff"
            };
            if debug_seam {
                self.react_to_update_apply_outcome(source, surfaced, false);
            } else {
                self.surface_update_apply_outcome(source, surfaced, false);
            }
        }
        Some(teardown)
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
    /// a fully-working terminal after a candidate that never became ready.
    ///
    /// PRECONDITION — the candidate was SIGNALLED and then proven TERMINATED, so
    /// exactly zero readers exist when ours restart. The proof is a
    /// [`HandoffRollbackWarrant`], minted on the worker (or the emergency
    /// reaper) before the completion that reaches this function is ever sent —
    /// see [`send_warranted_handoff_failure`]. It cannot be re-checked here: the
    /// completion crosses a channel and a warrant is not a value that survives
    /// the trip, so the ordering at the send site IS the guarantee. The
    /// pre-spawn failure paths in [`App::start_unix_update_handoff`] call this
    /// directly under the same rule, with no candidate to prove anything about.
    ///
    /// With that established:
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

/// When this attempt stops waiting for its successor to prove readiness.
///
/// A DEADLINE RATHER THAN A TIMEOUT, because on the out-of-band lane the wait
/// has two stages and they share one budget: the successor has to dial the
/// rendezvous (behind its whole staged-swap boot apply) and only then paint and
/// prove. Two independent 15 s timeouts would let a slow successor park the
/// user's terminal for thirty seconds; one deadline computed once is what makes
/// "the fork lane's budget" mean the same thing on both lanes. The fork lane
/// computes it immediately before its single wait, which is exactly where the
/// function it replaced computed it.
#[cfg(unix)]
#[must_use]
fn handoff_ready_deadline() -> std::time::Instant {
    // DEFAULT RAISED 15 s → 30 s (2026-08-15). The one deadline covers
    // LaunchServices latency, first-launch Gatekeeper assessment, the
    // successor's ENTIRE staged-swap boot apply (ditto/codesign + re-exec),
    // adoption, and the paint of every window — the codebase's own cold
    // measurement is 4.5 s with nothing else running, and this machine's
    // ledger carried a real TimedOut streak whose live failures all landed
    // under compile load (reproduced on demand: a rehearsal handoff during a
    // workspace build times out at 15 s and completes with headroom at more).
    // The cost of a longer deadline is bounded and safe — the parked parent
    // un-parks on expiry exactly as before — while the cost of a short one is
    // an update lane that quietly never succeeds on a busy machine.
    let timeout_ms = std::env::var("ATERM_HANDOFF_READY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(30_000)
        .clamp(1_000, 120_000);
    std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms)
}

#[cfg(unix)]
fn wait_handoff_ready(
    rd: &std::os::fd::OwnedFd,
    expected: crate::seamless::AdoptionProof,
    cancel: &std::sync::mpsc::Receiver<()>,
    masters: &[i32],
    deadline: std::time::Instant,
) -> crate::UpdateHandoffOutcome {
    use std::os::fd::AsRawFd as _;
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
mod capture_budget_reservation_tests {
    use super::{MAX_HANDOFF_CAPTURE_BUDGET_BYTES, optional_carry_fits};
    use crate::seamless::{
        admit_checkpoint_dimensions, checkpoint_capture_budget_bytes, mandatory_checkpoint_cells,
        max_handoff_aggregate_grid_cells, max_handoff_history_lines,
    };

    /// Walk a pool of `sessions` identical panes through the producer's
    /// reserve-then-history rule, charging the REAL admission seam and the REAL
    /// budgets `start_unix_update_handoff` charges.
    ///
    /// The per-session degrade rung is deliberately NOT replicated: if the
    /// reservation is right, nothing ever needs it, so `Ok` here is the stronger
    /// claim. `Err(index)` is the session the capture would have refused, which is
    /// the whole update.
    fn walk_pool(rows: u16, cols: u16, sessions: u64) -> Result<u64, u64> {
        let carry = max_handoff_history_lines();
        let mandatory_cells = mandatory_checkpoint_cells(rows, cols);
        let mandatory_bytes = checkpoint_capture_budget_bytes(rows, cols, 0)
            .expect("PRECONDITION: the test geometry must be admissible at all");
        let history_cells = u64::from(cols) * u64::from(carry);
        let history_bytes = checkpoint_capture_budget_bytes(rows, cols, carry)
            .expect("PRECONDITION: the test geometry must be admissible with history")
            - mandatory_bytes;

        let mut cells_reserve = mandatory_cells * sessions;
        let mut bytes_reserve = mandatory_bytes * sessions;
        let mut used_cells = 0_u64;
        let mut used_bytes = 0_u64;
        let mut carried = 0_u64;
        for index in 0..sessions {
            cells_reserve -= mandatory_cells;
            bytes_reserve -= mandatory_bytes;
            let cells_fit = optional_carry_fits(
                used_cells,
                mandatory_cells,
                history_cells,
                cells_reserve,
                max_handoff_aggregate_grid_cells(),
            );
            let bytes_fit = optional_carry_fits(
                used_bytes,
                mandatory_bytes,
                history_bytes,
                bytes_reserve,
                MAX_HANDOFF_CAPTURE_BUDGET_BYTES,
            );
            let history = if cells_fit && bytes_fit { carry } else { 0 };
            let Some(per_grid) =
                admit_checkpoint_dimensions(&mut used_cells, rows, cols, history, true)
            else {
                return Err(index);
            };
            used_bytes += per_grid * 2;
            if used_bytes > MAX_HANDOFF_CAPTURE_BUDGET_BYTES {
                return Err(index);
            }
            carried += u64::from(history != 0);
        }
        Ok(carried)
    }

    /// REGRESSION (the desk that could not update). Both capture budgets used to be
    /// handed out greedily in pool order: the first sessions each took a full 256
    /// lines of scrollback, and a later session then found nothing left for its
    /// MANDATORY visible screen. That is not degradable, so the capture failed and
    /// the seamless update did not apply — deterministically, on every retry, for
    /// anyone past a handful of panes (five at the reported geometry).
    ///
    /// Twelve, twenty-four and sixty-four panes, at the reported geometry and at a
    /// maximized one, must all be admitted in full. Carrying less scrollback is an
    /// allowed answer; refusing the update is not.
    #[test]
    fn a_heavy_pool_is_admitted_even_when_it_must_drop_history() {
        // 49x110 is the window from the field report. 60x200 is a maximized window
        // on a large display, where one session costs more than twice as much.
        for (rows, cols) in [(49_u16, 110_u16), (60, 200)] {
            for sessions in [12_u64, 24, 64] {
                let carried = walk_pool(rows, cols, sessions).unwrap_or_else(|index| {
                    panic!(
                        "session {index} of {sessions} at {rows}x{cols} was refused; a pool \
                         whose visible screens fit must never cost the update"
                    )
                });
                assert!(
                    carried > 0,
                    "{sessions} panes at {rows}x{cols} carried no scrollback at all — the \
                     reservation must buy the pool a SMALLER carry, not abolish it"
                );
            }
        }
    }

    /// The exact boundary the reservation establishes: a session is refused if and
    /// only if the POOL's mandatory visible+alt total genuinely does not fit. The
    /// count is derived from the constant, so raising the aggregate later moves the
    /// boundary instead of reddening this test.
    ///
    /// The `Err(fits)` is the load-bearing half. Under the old greedy rule the
    /// refusal landed on an EARLY session — one whose own screen fit perfectly well,
    /// but whose budget an earlier pane had already spent on optional scrollback.
    #[test]
    fn a_pool_is_refused_only_when_its_mandatory_total_does_not_fit() {
        let (rows, cols) = (60_u16, 200_u16);
        let fits = max_handoff_aggregate_grid_cells() / mandatory_checkpoint_cells(rows, cols);
        assert!(
            walk_pool(rows, cols, fits).is_ok(),
            "{fits} panes at {rows}x{cols} are exactly what the aggregate holds \
             visible-only, so every one of them must be admitted"
        );
        assert_eq!(
            walk_pool(rows, cols, fits + 1),
            Err(fits),
            "one pane past the visible-only ceiling must be refused, and refused as the \
             LAST session — never an earlier one that lost its budget to somebody \
             else's scrollback"
        );
    }
}

#[cfg(all(test, unix))]
mod commit_layout_topology_tests {
    use super::commit_layout_topology;
    use crate::restore::{
        PaneLayout, RestoreManifest, RestoredSplitTree, RestoredTab, RestoredView, TabOrderEntry,
        TerminalLeafRestore, WindowLayout,
    };

    /// One window, one terminal tab, in the shape `capture_restore_manifest`
    /// really produces: the legacy `tabs` mirror and the canonical
    /// `restored_tabs` tree both carry the SAME live session's cwd/title, so a
    /// degraded read corrupts two places at once and a projection that missed
    /// either one would still reject the Commit.
    fn captured(position: Option<(i32, i32)>, cwd: Option<&str>, title: &str) -> RestoreManifest {
        RestoreManifest::new(vec![WindowLayout {
            rows: 40,
            cols: 120,
            active_tab: 0,
            outer_x: position.map(|(x, _)| x),
            outer_y: position.map(|(_, y)| y),
            maximized: None,
            tabs: vec![PaneLayout::Leaf {
                cwd: cwd.map(str::to_string),
                title: title.to_string(),
                focused: true,
                local_id: Some(7),
            }],
            native_tabs: Vec::new(),
            tab_order: vec![TabOrderEntry::Terminal { index: 0 }],
            active_item: Some(0),
            restored_tabs: vec![RestoredTab {
                root: RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
                    cwd: cwd.map(str::to_string),
                    title: title.to_string(),
                    profile: None,
                    local_id: Some(7),
                    user_title: None,
                    description: None,
                    icon: None,
                    role: None,
                    attention: None,
                })),
                focused_path: Vec::new(),
                zoomed: false,
            }],
        }])
    }

    #[test]
    fn dragging_the_window_while_the_successor_boots_is_not_a_topology_change() {
        let committed = captured(Some((120, 80)), Some("/work"), "zsh");
        let dragged = captured(Some((640, 310)), Some("/work"), "zsh");
        assert_ne!(
            committed, dragged,
            "PRECONDITION: the derived PartialEq must still see the raw position difference — \
             that is exactly what used to reject the Commit"
        );
        assert_eq!(
            commit_layout_topology(&committed),
            commit_layout_topology(&dragged),
            "a drag during the child's boot must not be reported as changed topology; \
             `WindowEvent::Moved` is classified Tolerated for this very reason"
        );
    }

    #[test]
    fn a_contended_session_lock_that_empties_cwd_and_title_is_not_a_topology_change() {
        let committed = captured(Some((120, 80)), Some("/work"), "zsh");
        // EXACTLY what `restore_session_meta` yields on a `WouldBlock` try_lock
        // (a scrollback drain holding the `term` mutex): no cwd, empty title,
        // every structural field untouched.
        let degraded = captured(Some((120, 80)), None, "");
        assert_ne!(
            committed, degraded,
            "PRECONDITION: a degraded capture is not equal to the committed one"
        );
        assert_eq!(
            commit_layout_topology(&committed),
            commit_layout_topology(&degraded),
            "a degraded metadata read must never masquerade as a changed layout — it made the \
             rejection nondeterministic for a session that did not change"
        );
    }

    #[test]
    fn a_real_topology_change_still_rejects_the_commit() {
        let committed = captured(Some((120, 80)), Some("/work"), "zsh");

        let mut resized = captured(Some((120, 80)), Some("/work"), "zsh");
        resized.windows[0].cols = 200;
        assert_ne!(
            commit_layout_topology(&committed),
            commit_layout_topology(&resized),
            "the grid the proof committed to is structural and must still reject"
        );

        let mut readopted = captured(Some((120, 80)), Some("/work"), "zsh");
        readopted.windows[0].restored_tabs[0].root =
            RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
                cwd: Some("/work".to_string()),
                title: "zsh".to_string(),
                profile: None,
                local_id: Some(9),
                user_title: None,
                description: None,
                icon: None,
                role: None,
                attention: None,
            }));
        assert_ne!(
            commit_layout_topology(&committed),
            commit_layout_topology(&readopted),
            "`local_id` is the layout↔live-fd bridge the child adopts by, not degradable \
             metadata, so it must survive the projection"
        );

        let mut relabelled = captured(Some((120, 80)), Some("/work"), "zsh");
        let RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(terminal),
        } = &mut relabelled.windows[0].restored_tabs[0].root
        else {
            unreachable!("the fixture builds a single terminal leaf");
        };
        terminal.user_title = Some("deploy".to_string());
        assert_ne!(
            commit_layout_topology(&committed),
            commit_layout_topology(&relabelled),
            "USER metadata is captured under a BLOCKING lock, so it cannot degrade and must \
             not be swept up by the cwd/title normalization"
        );

        let mut extra_tab = captured(Some((120, 80)), Some("/work"), "zsh");
        let tab = extra_tab.windows[0].restored_tabs[0].clone();
        extra_tab.windows[0].restored_tabs.push(tab);
        assert_ne!(
            commit_layout_topology(&committed),
            commit_layout_topology(&extra_tab),
            "a tab that appeared during async preparation is what this gate exists to catch"
        );
    }
}

#[cfg(all(test, unix))]
mod pinnedness_tests {
    use super::candidate_is_our_child;

    /// THE PRECONDITION FOR A GROUP KILL, and the reason the emergency reaper may
    /// no longer take it on trust.
    ///
    /// `kill(-pid)` is sound only while the number is PINNED to our candidate. A
    /// fork child pins it — an unreaped child owns its pid until it is waited on.
    /// A launchd-owned successor does not: it is nobody's child, launchd may reap
    /// it at any moment, and the freed number can then name a stranger whose
    /// process GROUP we would be signalling.
    ///
    /// So the reaper asks the kernel. This pins the two answers it relies on: a
    /// live child of ours reads as pinned, and a process that is not our child
    /// (here, this very process, which is certainly alive) does not — the latter
    /// standing in for the launchd-owned successor, which no test can conjure.
    #[test]
    fn only_a_child_of_ours_pins_its_pid() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn a child to own a pid");
        let pid = libc::pid_t::try_from(child.id()).expect("pid fits");
        assert!(
            candidate_is_our_child(pid),
            "a live child of ours pins its number, so -pid still names its group"
        );

        // Not our child: alive, real, and never ours to sweep.
        let own = libc::pid_t::try_from(std::process::id()).expect("pid fits");
        assert!(
            !candidate_is_our_child(own),
            "a process that is not our child must never license a group kill"
        );
        assert!(!candidate_is_our_child(1), "launchd is not ours either");

        // WNOWAIT is load-bearing: the probe must not consume the child, or the
        // reaper's own wait would block forever on a pid it had already reaped.
        child.kill().expect("kill the probe child");
        let status = child.wait().expect("the probe left it waitable");
        assert!(!status.success(), "it was killed, not exited cleanly");
    }
}

#[cfg(all(test, unix))]
mod candidate_death_evidence_tests {
    use super::{
        HandoffCandidate, HandoffCandidateHandle, HandoffCandidateProbe, handoff_child_death,
        observe_candidate_death, probe_handoff_candidate,
    };
    use crate::ChildDeathEvidence as Death;
    use std::os::unix::process::ExitStatusExt as _;

    /// One `wait(2)` status in the encoding `ExitStatus` carries, so the unit
    /// tests below can state a signal death and a clean exit without a real corpse.
    fn signalled(signal: i32) -> std::process::ExitStatus {
        std::process::ExitStatus::from_raw(signal)
    }

    /// Spin until `probe` agrees, or fail. Bounded HERE ONLY, so a broken probe
    /// fails a test instead of hanging the suite; nothing in production polls this.
    fn wait_for_probe(pid: libc::pid_t, want: HandoffCandidateProbe) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while probe_handoff_candidate(pid) != want {
            assert!(
                std::time::Instant::now() < deadline,
                "the kernel must reach {want:?} for {pid}"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// THE THREE ANSWERS THE PRE-KILL PROBE HAS TO SEPARATE, against real
    /// processes, because the whole value of the probe is that it is the KERNEL
    /// talking and not us.
    ///
    /// `Running` versus `Exited` is the load-bearing pair: it is what tells the
    /// reject path whether a bare `SIGKILL` in the status it collects belongs to
    /// the candidate or to the signal it is about to send.
    #[test]
    fn the_pre_kill_probe_separates_a_running_candidate_from_a_dead_one() {
        let mut alive = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn a live candidate");
        let alive_pid = libc::pid_t::try_from(alive.id()).expect("pid fits");
        assert_eq!(
            probe_handoff_candidate(alive_pid),
            HandoffCandidateProbe::Running,
            "a child that has not exited has nothing to report yet"
        );

        let mut dead = std::process::Command::new("/bin/sh")
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

        // WNOWAIT IS LOAD-BEARING, and this is the assertion that proves it: the
        // probe ran against an exited child and the status is STILL collectable,
        // which is exactly what `kill_and_reap_handoff_child` does next.
        assert_eq!(
            dead.wait().expect("the probe consumed nothing").code(),
            Some(3),
            "the probe must leave the candidate's own status intact"
        );
        alive.kill().expect("kill the live fixture");
        alive.wait().expect("reap the live fixture");
    }

    /// WHICH STATUS BITS CAN POSSIBLY BE OURS, stated as the whole of the
    /// attribution rule, because this process sends exactly one signal ever.
    ///
    /// THE BUG THIS PINS: an earlier draft made `died_before_we_signalled` a
    /// precondition on reading the status AT ALL. That threw away bits that
    /// provably could not be our SIGKILL — an exit code above all — so this tree's
    /// commonest refusal (a deliberate `exit(0)`) degraded to `Unobserved`
    /// whenever the parent lost a race it deliberately refuses to wait out, and
    /// the refusal the classification most wants to catch became the one it could
    /// least often see.
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

    /// THE PRODUCTION ASSEMBLY, against a REAL child, through the same function
    /// the reject path calls — probe, kill, reap, classify.
    ///
    /// Every other test here hands [`handoff_child_death`] three facts it made up.
    /// This one makes the kernel produce them, which is the only way the ORDER of
    /// the steps is under test at all: swap the pre-kill probe for anything that
    /// answers "already dead" too eagerly and the `exit(0)` below turns into
    /// `Signalled { SIGKILL }` — the machine — with every hand-built assertion in
    /// this module still green.
    #[test]
    fn a_candidate_that_exits_on_its_own_is_read_as_its_own_exit_not_our_kill() {
        // A child that ends itself with the tree's commonest refusal code, then a
        // wait for the kernel to agree it is dead — which is the state the reject
        // path finds a refusing successor in.
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

    /// THE OTHER HALF OF THE SAME ASSEMBLY: a candidate somebody ELSE killed.
    ///
    /// This is the field shape — macOS jetsam reclaiming memory from a process the
    /// machine was starving — reproduced with the one signal that is
    /// indistinguishable from the reject path's own, so the pre-kill probe is the
    /// only thing that can tell them apart.
    #[test]
    fn a_candidate_the_machine_killed_is_read_as_the_machine_s_kill() {
        let child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn a candidate to starve");
        let candidate = HandoffCandidate::of_unreaped_child(&child);
        let pid = libc::pid_t::try_from(candidate.pid).expect("pid fits");
        let mut handle = HandoffCandidateHandle::Forked(child);
        // SOMEBODY ELSE'S SIGKILL, delivered before the reject path runs — exactly
        // what a machine out of memory does, and exactly what the parent's own
        // rejection would look like if the order of the steps were wrong.
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

    /// AND THE CANDIDATE THIS PROCESS ENDED ITSELF, which must claim nothing.
    ///
    /// A live candidate, rejected: the reject path SIGKILLs it and reaps a status
    /// that says `SIGKILL` — its own signal, wearing the candidate's clothes.
    /// Reading that as evidence would file every deliberate rejection as "the
    /// machine reclaimed it" and hand a broken successor the transient lane's nine
    /// attempts.
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

    /// EVERY OTHER OUTCOME DESCRIBES A CANDIDATE THIS PROCESS DECIDED TO END, so
    /// the assembly must not even ask — and must not spend a probe on it.
    ///
    /// The child here exits on its own with a code the classifier would happily
    /// call STRUCTURAL; the outcome is what makes that irrelevant.
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
}

#[cfg(all(test, unix))]
mod trial_launch_forgiveness_tests {
    use super::forgives_the_counted_trial_launch;
    use crate::ChildDeathEvidence as Death;

    /// The shape of a real apply: a strictly newer authorized target.
    const CURRENT: u64 = 1_787_699_398;
    const TARGET: u64 = 1_787_699_399;

    /// Forgive, or keep the count, for a real apply.
    fn forgives(death: Death) -> bool {
        forgives_the_counted_trial_launch(death, CURRENT, TARGET)
    }

    /// THE DEFECT THIS TABLE EXISTS TO STOP, stated before the table: a `ChildDied`
    /// whose launch stays counted spends the SAME counter the boot sentinel
    /// reverts on. Retry it more times than `MAX_BOOT_ATTEMPTS` and the automatic
    /// lane reverts the bundle and marks the build failed — for bytes that never
    /// failed. `ChildDied` used to be exempted from forgiveness wholesale, which
    /// was safe only while it was also capped at two attempts; the moment it could
    /// be retried six or nine times, the exemption became the bug.
    ///
    /// So forgiveness follows the SHAPE, and only the shape that converges under
    /// `MAX_BOOT_ATTEMPTS` may keep its count.
    #[test]
    fn only_a_death_the_bytes_answer_for_keeps_its_counted_trial_launch() {
        for (death, keeps_its_count, why) in [
            (
                Death::Exited { code: 0 },
                true,
                "the image reached an `exit` instruction: it ran, it decided, and \
                 a launch that ended that way is the sentinel's to keep",
            ),
            (
                Death::Exited { code: 74 },
                true,
                "same reading; the code is recorded, not judged",
            ),
            (
                Death::Signalled {
                    signal: libc::SIGSEGV,
                },
                true,
                "a fault is the image executing itself into a wall",
            ),
            (
                Death::Signalled {
                    signal: libc::SIGKILL,
                },
                false,
                "THE FIELD CASE: macOS jetsam reclaiming memory says nothing about \
                 the bytes, so the launch goes back — otherwise three busy \
                 afternoons revert a healthy build",
            ),
            (
                Death::Unobserved,
                false,
                "and an unattributed death least of all: this is the majority \
                 answer on the shipping lane, and it is retried six times",
            ),
        ] {
            assert_eq!(forgives(death), !keeps_its_count, "{death:?}: {why}");
        }
    }

    /// AND THE REAL-APPLY GUARD, which is part of the same decision: with no
    /// strictly newer authorized target nothing armed a sentinel, so there is no
    /// counted launch to give back and the answer is no whatever the death was.
    #[test]
    fn an_attempt_that_authorizes_no_newer_build_has_no_launch_to_forgive() {
        for death in [
            Death::Unobserved,
            Death::Exited { code: 0 },
            Death::Signalled {
                signal: libc::SIGKILL,
            },
        ] {
            assert!(
                !forgives_the_counted_trial_launch(death, CURRENT, CURRENT),
                "{death:?}: an installed activation is its own target, and the QA \
                 seam authorizes none at all"
            );
            assert!(
                !forgives_the_counted_trial_launch(death, CURRENT, CURRENT - 1),
                "{death:?}: and an older target is never applied"
            );
        }
    }

    /// THE INVARIANT THAT MAKES THE TABLE ABOVE SAFE, checked against the real
    /// constant rather than restated. Mirrors the compile-time assert beside
    /// `STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS`, so a reader of this module sees WHY
    /// only the structural shape may keep its count.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_one_shape_that_keeps_its_count_converges_before_the_sentinel_reverts() {
        assert!(
            u32::from(crate::app_native::STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS)
                < aterm_update::MAX_BOOT_ATTEMPTS,
            "a shape whose launches stay counted must be finished with the \
             artifact before the boot sentinel would revert it; {} vs {}",
            crate::app_native::STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS,
            aterm_update::MAX_BOOT_ATTEMPTS
        );
        for shape in [
            crate::app_native::PhysicalFailureShape::Transient,
            crate::app_native::PhysicalFailureShape::Unexplained,
        ] {
            assert!(
                u32::from(shape.lifetime_attempts()) >= aterm_update::MAX_BOOT_ATTEMPTS,
                "{shape:?} is retried far enough to reach the revert threshold, \
                 which is precisely why its deaths must forgive"
            );
        }
    }
}

/// THE NON-PARENT WITNESS, against real processes reparented to launchd — the
/// exact shape of a LaunchServices-launched successor, which is what the shipping
/// macOS lane hands this file.
#[cfg(all(test, target_os = "macos"))]
mod candidate_exit_watch_tests {
    use super::CandidateExitWatch;

    /// Make a process that is NOT our child: fork a middle process, let IT fork the
    /// orphan, then reap the middle so the orphan reparents to launchd. Returns the
    /// orphan's pid, which `waitid` will answer `ECHILD` about forever after.
    fn spawn_orphan(script: &str) -> u32 {
        // THE ORPHAN'S STANDARD STREAMS GO TO `/dev/null`, not to the pipe this
        // reads: a background job inherits the pipe's write end, so leaving it
        // open would make `read_to_string` below wait for the ORPHAN to exit and
        // hand back a pid that is already gone — which is a fixture that silently
        // tests nothing.
        let mut middle = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("{{ {script} ; }} >/dev/null 2>&1 & echo $!"))
            .stdout(std::process::Stdio::piped())
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
        pid.trim().parse().expect("a pid")
    }

    /// Wait for the watch to answer, or fail. Bounded HERE ONLY; the production
    /// reads are both single zero-timeout polls.
    fn wait_for_status(watch: &CandidateExitWatch) -> std::process::ExitStatus {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
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

    /// THE CAPABILITY THE SHIPPING LANE HAD NO SUBSTITUTE FOR: a full `wait(2)`
    /// status for a process this one did not fork and may not reap.
    ///
    /// Both halves matter and they are the two verdicts that used to be
    /// indistinguishable there — a candidate that CHOSE to stop, and a candidate
    /// something else stopped.
    #[test]
    fn a_non_parent_can_read_an_orphan_s_own_exit_status() {
        use std::os::unix::process::ExitStatusExt as _;

        let refused = spawn_orphan("sleep 0.3; exit 7");
        let watch = CandidateExitWatch::watch(refused)
            .expect("EVFILT_PROC attaches to a same-user process we may SIGKILL");
        let status = wait_for_status(&watch);
        assert_eq!(
            status.code(),
            Some(7),
            "a clean exit reaches a watcher that is nobody's parent — which is \
             what lets the launched lane tell a refusal from a kill at all"
        );

        let starved = spawn_orphan("sleep 30");
        let watch = CandidateExitWatch::watch(starved)
            .expect("EVFILT_PROC attaches to a same-user process we may SIGKILL");
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

    /// THE SHIPPING macOS LANE, END TO END, WITH THE FIELD SHAPE.
    ///
    /// A LaunchServices successor is launchd's child: `waitid` answers `ECHILD`
    /// forever, so before the witness above this assembly could observe NOTHING
    /// here and every `ChildDied` on the lane the defect was reported on was
    /// classified from inference. This drives the real
    /// [`super::observe_candidate_death`] against a real orphan that a real
    /// external `SIGKILL` ended — macOS jetsam reclaiming memory from a process
    /// the machine was starving — and asserts the two things that have to follow:
    /// the death is read as the MACHINE's, and the automatic lane RETRIES.
    #[test]
    fn a_starved_candidate_on_the_launched_lane_takes_the_retry_lane() {
        use super::{HandoffCandidate, HandoffCandidateHandle, observe_candidate_death};
        use crate::app_native::{HandoffFailureLane as Lane, PhysicalFailureShape as Shape};

        let starved = spawn_orphan("sleep 30");
        // Registered while it is alive, exactly as the rendezvous accept does.
        let watch = CandidateExitWatch::watch(starved).expect("the candidate is alive");
        let pid = libc::pid_t::try_from(starved).expect("pid fits");
        // SOMEBODY ELSE'S SIGKILL — the field shape, and the one signal that is
        // indistinguishable from this lane's own rejection.
        // SAFETY: `pid` is a positive pid we created and can still see.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
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

    /// AND THE OTHER HALF OF THE SAME ASSEMBLY: an exit this process did NOT
    /// witness before it acted, recovered after the candidate is provably gone.
    ///
    /// A launched candidate whose kernel birth stamp cannot be read is
    /// `Unwitnessed`, and this lane deliberately signals such a candidate NOTHING
    /// (a `-pid` group kill on an unpinned number can land on a stranger). So the
    /// termination proof waits the candidate out and the witness is read AFTER it
    /// — which is sound for an exit code, because `SIGKILL` is the only signal
    /// this process sends and it never yields `WIFEXITED`.
    ///
    /// Deleting that second read leaves this `Unobserved`: six bounded retries for
    /// a successor that stated in as many words that it had decided to stop.
    #[test]
    fn an_exit_the_parent_did_not_see_coming_is_still_recovered_after_the_wait() {
        use super::{HandoffCandidate, HandoffCandidateHandle, observe_candidate_death};

        let refusing = spawn_orphan("sleep 0.3; exit 0");
        let watch = CandidateExitWatch::watch(refusing).expect("the candidate is alive");
        // A bare pid carries no birth stamp, so nothing is signalled and the
        // candidate reaches its own `exit` — the deterministic form of the race
        // this read exists to widen.
        let candidate = HandoffCandidate::from_bare_pid(refusing);
        let mut handle = HandoffCandidateHandle::Launched(Some(watch));
        assert!(
            handle.witnessed_exit_status().is_none(),
            "it is alive right now: the pre-kill read must claim nothing"
        );
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

    /// THE FAILING DIRECTION, which must cost only evidence. A pid that names
    /// nothing cannot be attached to, and the answer has to be `None` rather than
    /// an error path the reject lane would have to handle.
    #[test]
    fn watching_a_candidate_that_is_already_gone_answers_none() {
        let gone = spawn_orphan("exit 0");
        let pid = libc::pid_t::try_from(gone).expect("pid fits");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
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
}

#[cfg(all(test, unix))]
mod handoff_process_group_tests {
    use super::{
        HandoffCandidate, HandoffCandidateHandle, HandoffCommitFacts, HandoffRejectDelivery,
        HandoffRollbackWarrant, ProcessGroupContainment, ReadyPollAction, classify_ready_poll,
        contain_own_process_group, deliver_handoff_rejection,
        emergency_kill_and_reap_handoff_child, handoff_candidate_terminated,
        handoff_commit_admitted, handoff_masters_closed, handoff_masters_have_activity,
        handoff_ready_deadline, kill_and_reap_handoff_child, make_cloexec_pipe, wait_handoff_ready,
        worker_claim_handoff_reaper,
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
        // PROMPT-EVENTUAL, not same-instant: production peeks repeatedly
        // across the overlap, so the contract is that peer death becomes
        // visible to the peek promptly — and under full-suite scheduler load
        // macOS can surface the HUP edge a quantum after close(2), which is
        // exactly where the same-instant version of this assert flaked (twice,
        // never solo). The bounded retry keeps the teeth: a REAL stale-
        // identity bug never reports closed, and two seconds of grace cannot
        // mask it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !handoff_masters_closed(&live) {
            assert!(
                std::time::Instant::now() < deadline,
                "peer death must still revoke — the live-set identity is stale"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
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
            wait_handoff_ready(
                &proof_rd,
                expected,
                &cancel_rx,
                &[master_rd],
                handoff_ready_deadline()
            ),
            crate::UpdateHandoffOutcome::ProofReady,
            "queued shell output must not abort the ready wait"
        );

        // Cancel is the typed activity outcome.
        let (proof_rd, _proof_wr) = make_cloexec_pipe().expect("second proof pipe");
        let (cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
        cancel_tx.try_send(()).expect("queue cancel poke");
        assert_eq!(
            wait_handoff_ready(
                &proof_rd,
                expected,
                &cancel_rx,
                &[master_rd],
                handoff_ready_deadline()
            ),
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

        let candidate = HandoffCandidate::of_unreaped_child(&child);
        let mut handle = HandoffCandidateHandle::Forked(child);
        assert_eq!(
            kill_and_reap_handoff_child(candidate, &mut handle).warrant,
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
            wait_handoff_ready(
                &proof_rd,
                expected,
                &cancel_rx,
                &[],
                handoff_ready_deadline()
            ),
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

        let candidate = HandoffCandidate::of_unreaped_child(&child);
        let mut handle = HandoffCandidateHandle::Forked(child);
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
        assert_eq!(
            emergency_kill_and_reap_handoff_child(child.id()),
            HandoffRollbackWarrant::Reaped,
            "the emergency reaper still prefers `waitpid` when the candidate is ours"
        );
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

    /// B2, soundness half: the fallback authority's PID-VACANCY proof, in both
    /// directions. A running candidate must never satisfy it (resuming a reader
    /// then is the corruption the overlap exists to prevent), and a candidate
    /// whose pid has come free must always satisfy it — that vacancy is the
    /// same fact `wait` returns, reached without being the parent.
    #[test]
    fn a_running_candidate_is_unproven_and_a_vacant_pid_is_the_proof() {
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .spawn()
            .expect("spawn a live candidate");
        let candidate = HandoffCandidate::of_unreaped_child(&child);
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

    /// B2, the blocker itself: a candidate this process did NOT fork — the shape
    /// a LaunchServices-launched successor has — is proven terminated anyway.
    /// `waitpid` answers `ECHILD` for it (asserted below, because that ECHILD is
    /// exactly what costs the old authority its proof), and the orphan is not a
    /// process-group leader either (`sh -c` runs without job control), so
    /// `kill(-pid)` names no group and sweeps nothing: only the
    /// identity-corroborated DIRECT signal can end it. Both halves of the
    /// fallback are therefore load-bearing here.
    ///
    /// Fixture cleanup is the `sleep` itself: every process this spawns exits on
    /// its own within 30 s even if an assertion panics first.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_candidate_we_never_forked_is_still_proven_terminated() {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("sleep 30 >/dev/null 2>&1 & printf '%s\\n' \"$!\"; exit 0")
            .stdout(std::process::Stdio::piped());
        let mut parent = command.spawn().expect("spawn the orphan's parent");
        let mut stdout = std::io::BufReader::new(parent.stdout.take().expect("parent stdout"));
        let mut orphan_line = String::new();
        stdout.read_line(&mut orphan_line).expect("orphan pid line");
        let orphan: u32 = orphan_line.trim().parse().expect("numeric orphan pid");
        // Reaping the middle process is what reparents the orphan to launchd.
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
        // Bounded HERE ONLY: a broken fallback must fail this test rather than
        // hang the suite. `wait_for_handoff_candidate_to_terminate` deliberately
        // carries no such bound, because in production the alternative to
        // waiting is resuming a reader without proof.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !handoff_candidate_terminated(candidate) {
            if std::time::Instant::now() >= deadline {
                // Fail clean: no fixture outlives its assertion.
                // SAFETY: SIGKILL to the fixture's own orphan.
                unsafe { libc::kill(raw, libc::SIGKILL) };
                panic!("the outside proof never established that orphan {orphan} terminated");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// The other edge of the identity check: a pid whose kernel birth stamp
    /// DISAGREES with the candidate's was recycled, which both proves the
    /// candidate terminated and makes signalling that pid an attack on a
    /// bystander. Without a parent's unreaped-child pin — the LaunchServices
    /// shape — this is the only thing standing between the reap path and
    /// SIGKILLing whatever inherited the number.
    ///
    /// Fixture cleanup is the `sleep` itself: the bystander exits on its own
    /// within 30 s even if an assertion panics before the explicit kill.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_pid_whose_birth_record_disagrees_is_proof_of_death_and_is_never_signalled() {
        let mut bystander = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .spawn()
            .expect("spawn a stand-in for whoever recycled the pid");
        let candidate = HandoffCandidate {
            pid: bystander.id(),
            // The pid the candidate was born at, paired with a birth instant
            // that is not the one the kernel reports for it now.
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
        // SIGKILL delivery is not synchronous with `kill(2)` returning, so give
        // a wrongly-sent signal time to actually land before concluding none was.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let pid = i32::try_from(bystander.id()).expect("bounded child pid");
        // SAFETY: signal 0 is kill(2)'s existence check; it delivers nothing.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            0,
            "the process that recycled the pid must survive the reap path"
        );

        // SAFETY: SIGKILL to the test's own child.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0, "kill fixture");
        bystander.wait().expect("reap the test child");
    }

    /// Run [`contain_own_process_group`] in a FORKED CHILD and report whether the
    /// child came out leading its own process group.
    ///
    /// The fork is not incidental: `setpgid` is a process-wide, irreversible
    /// change, so calling it on the test binary itself would move `cargo test`
    /// out of the process group whoever launched it may later sweep. The child
    /// answers through its exit status instead.
    ///
    /// `become_session_leader` selects the shape under test, and the child also
    /// checks that a SECOND identical `setpgid(0, 0)` is refused exactly when it
    /// is a session leader. That second call changes nothing (the process already
    /// leads its own group either way); it is a witness of WHICH kernel path the
    /// first call took, which is what makes ignoring the errno legitimate.
    ///
    /// libtest runs tests on a THREAD POOL, so this process is multi-threaded at
    /// fork time. That is safe here only because the child touches nothing but
    /// async-signal-safe calls — `setsid`, the `setpgid`/`getpgrp`/`getpid`
    /// inside `contain_own_process_group`, and `_exit`. It never allocates,
    /// locks, or calls back into std.
    fn forked_child_contains_itself(become_session_leader: bool) -> bool {
        // SAFETY: fork from a multi-threaded harness, with a child that performs
        // async-signal-safe calls only — see the doc comment above.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            if become_session_leader {
                // SAFETY: async-signal-safe. The child inherited the harness's
                // process group and its own pid is fresh, so it cannot be that
                // group's leader and `setsid` is permitted. Should it fail
                // anyway, the child is not a session leader, the refusal check
                // below disagrees with the requested shape, and the test fails.
                unsafe { libc::setsid() };
            }
            let contained = contain_own_process_group() == ProcessGroupContainment::OwnGroupLeader;
            // SAFETY: async-signal-safe; targets the calling process only.
            let refused = unsafe { libc::setpgid(0, 0) } != 0;
            let as_expected = contained && refused == become_session_leader;
            // SAFETY: async-signal-safe process exit; nothing is unwound and no
            // atexit handler of the harness's may run in this child.
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
                waited < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted,
                "waitpid refused to answer for the forked child"
            );
        }
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
    }

    /// The launch shape with no `pre_exec` hook: a successor that starts inside
    /// somebody else's process group must end up leading its own, or a rejecting
    /// parent's `kill(-pid)` reaches none of the helpers it forks afterwards.
    #[test]
    fn a_successor_started_in_a_foreign_group_contains_itself() {
        assert!(
            forked_child_contains_itself(false),
            "setpgid(0, 0) must have moved the child into a group of its own, and \
             a repeat call must be ACCEPTED because it is not a session leader"
        );
    }

    /// The objection this closes: `setpgid(0, 0)` can be refused, and the refusal
    /// is not a failure to contain. A session leader is the one process the call
    /// refuses, and it already has `pgid == sid == pid` — so the postcondition
    /// this code reads back from the kernel holds on exactly the path where the
    /// return value says it does not.
    #[test]
    fn a_session_leader_is_already_contained_when_the_call_refuses_it() {
        assert!(
            forked_child_contains_itself(true),
            "a session leader must both REFUSE the repeat setpgid and still lead \
             its own process group"
        );
    }
}

/// The returned-completion reducer's ONE remaining piece of attempt identity: the
/// [`crate::native_updater_service::ApplyMode`] the apply was authorized under.
#[cfg(all(test, unix))]
mod returned_handoff_completion_lane_tests {
    use crate::App;
    use crate::native_updater_service::{
        ApplyAttemptTicket, ApplyMode, CheckCompletion, CheckStart, DurableUpdateStatus,
    };

    const TEST_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    /// Stage one strictly-newer build through the REAL check reducer, so the
    /// artifact the completion reduces below is the one production would hold.
    fn stage_one_build(app: &mut App) -> u64 {
        let current_build = app.native_updater_service.snapshot().current_build;
        let build = current_build + 1;
        let CheckStart::Start(ticket) = app.native_updater_service.request_check() else {
            panic!("a fresh service must start exactly one check");
        };
        assert_eq!(
            app.native_updater_service.finish_check(
                ticket,
                DurableUpdateStatus {
                    enabled: true,
                    current_build,
                    staged_build: Some(build),
                    staged_version: Some(format!("1.0.{build}")),
                    staged_commit: Some(TEST_COMMIT.to_string()),
                    staged_dmg_sha256: Some("ab".repeat(32)),
                    changelog: None,
                    outcome: "staged".to_string(),
                    failing_checks: 0,
                    failing_persistent: false,
                    failing_kind: String::new(),
                    failing_applies: 0,
                    apply_failure: String::new(),
                    apply_failure_build: 0,
                    apply_failures_for_target: 0,
                    installable: true,
                    channel_unreadable: false,
                },
            ),
            CheckCompletion::Reduced,
            "PRECONDITION: the check must reduce, or nothing is staged"
        );
        build
    }

    /// Drive ONE returned handoff for `mode` end to end through the production
    /// reducer: an authorized attempt is pending, the worker reports `outcome`
    /// against the artifact `digest` names, and the reducer decides what that
    /// costs.
    ///
    /// Everything here is what a real attempt leaves behind, not a hand-built
    /// verdict: the ticket is the reducer's own current apply, the pending record
    /// carries the mode the apply was authorized under, and the completion has
    /// `reconcile: None` exactly as every construction site in this crate emits.
    ///
    /// The OUTCOME is a parameter because it is now load-bearing twice over — it
    /// carries the activity classification AND the physical shape — so a fixture
    /// that pinned it to one kind could only ever exercise one of the budgets.
    ///
    /// So is the DEATH EVIDENCE, for the same reason one level down: `ChildDied`
    /// alone decides nothing now, and a fixture that pinned the evidence to
    /// `Unobserved` could only ever exercise the unexplained budget.
    fn reduce_one_returned_failure(
        app: &mut App,
        mode: ApplyMode,
        build: u64,
        digest: &str,
        outcome: crate::UpdateHandoffOutcome,
        child_death: crate::ChildDeathEvidence,
    ) {
        let ticket = ApplyAttemptTicket::for_test(build, TEST_COMMIT, digest);
        ticket.make_current_apply_for_test(&mut app.native_updater_service);
        let (cancel, _cancelled) = std::sync::mpsc::sync_channel(1);
        app.pending_update_handoff = Some(crate::PendingUpdateHandoff {
            park_at: std::time::Instant::now(),
            proof_ready_at: None,
            attempt_id: 1,
            nonce: None,
            live: Vec::new(),
            adoption: Vec::new(),
            child_pid: None,
            mode,
            apply_attempt: Some(ticket),
            target_build: build,
            target_commit: TEST_COMMIT.to_string(),
            layout: crate::restore::RestoreManifest::new(Vec::new()),
            layout_digest: [0; 32],
            screen_digest: [0; 32],
            activity_epoch: app.update_handoff_activity_epoch,
            cancel,
            arbiter: crate::HandoffAttemptArbiter::new(),
            teardown: crate::DeferredHandoffTeardown::None,
            commit_drain_started: None,
            revoked_by_activity: false,
        });
        let teardown = app
            .reduce_returned_handoff_completion(crate::UpdateHandoffCompletion {
                attempt_id: 1,
                nonce: None,
                child_pid: None,
                outcome,
                commit_fd: None,
                reject: None,
                reconcile: None,
                detail: format!("handoff proof ended {outcome:?}"),
                input_drain_spins: 0,
                child_death,
            })
            .expect("a matching pending attempt is always reduced");
        assert_eq!(
            teardown,
            crate::DeferredHandoffTeardown::None,
            "PRECONDITION: this fixture requests no structural teardown, so the \
             replay branch cannot be what changed the state asserted on"
        );
    }

    /// THE FINDING: the completion path took `pending.mode`, used it for the
    /// activity classification, and then dropped it — so a physical failure a
    /// PERSON asked for (Version menu, palette, `aterm-ctl update apply`, an
    /// install-on-clean-quit gesture) was charged to the AUTOMATIC lane's
    /// converging budget and latched automatic apply off on their behalf.
    ///
    /// The two lanes are driven here against the SAME artifact in the SAME `App`,
    /// with identical worker reports, and the contrast is the assertion. A test
    /// that only drove one of them would pass with the mode dropped.
    #[test]
    fn a_person_s_returned_handoff_never_spends_the_automatic_lane_s_budget() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        assert!(
            app.arm_native_auto_apply(build, &"ab".repeat(32)),
            "PRECONDITION: the background lane is armed for these exact bytes, \
             which is the state a person's click has to leave alone"
        );

        // A PERSON'S FAILURE: nothing is charged. Not the physical budget, not the
        // manual-only latch, and not the live automatic intent — the background
        // lane is still going to try this artifact at its own next window.
        reduce_one_returned_failure(
            &mut app,
            ApplyMode::Immediate,
            build,
            &"ab".repeat(32),
            crate::UpdateHandoffOutcome::TimedOut,
            crate::ChildDeathEvidence::Unobserved,
        );
        assert_eq!(
            app.auto_apply_physical_retry.map(|retry| retry.cycles),
            None,
            "a person-initiated handoff failure must not open (or advance) the \
             automatic artifact's converging physical budget"
        );
        assert!(
            app.auto_apply_manual_only.is_none(),
            "a person's failure must not latch automatic apply off — a manual \
             retry that converges the background lane is the finding"
        );
        assert!(
            app.auto_apply_intent
                .is_some_and(|intent| intent.build == build),
            "and it must not retire the live automatic intent either"
        );

        // THE SAME FAILURE, AUTHORIZED IN THE BACKGROUND: charged in full. This is
        // the discriminator — flip the classification to ignore the mode and the
        // block above starts producing exactly this state.
        reduce_one_returned_failure(
            &mut app,
            ApplyMode::AutomaticPastGrace,
            build,
            &"ab".repeat(32),
            crate::UpdateHandoffOutcome::TimedOut,
            crate::ChildDeathEvidence::Unobserved,
        );
        assert_eq!(
            app.auto_apply_physical_retry.map(|retry| retry.cycles),
            Some(1),
            "the automatic lane's own failure is what the budget is for"
        );
        let latched = app
            .auto_apply_manual_only
            .expect("an automatic physical failure latches manual-only")
            .retry_at
            .expect("with a deadline, on its first failure")
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            latched > std::time::Duration::from_secs(500)
                && latched <= std::time::Duration::from_secs(600),
            "and on the physical schedule's first rung (~600s), got {latched:?}"
        );
        assert!(
            app.auto_apply_intent.is_none(),
            "an automatic physical failure retires the intent it was spent on"
        );
    }

    /// The activity classification must ALSO stay pointed at the background lane:
    /// a person's attempt does not arm the revocation watcher at all, so an
    /// activity-shaped rejection recorded against one can only be noise — and it
    /// must not mint an `arm_activity_revoked_overlap_retry` cycle that re-arms
    /// automatic apply on a schedule nobody asked for.
    #[test]
    fn a_person_s_activity_revoked_completion_arms_no_automatic_retry() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        let ticket = ApplyAttemptTicket::for_test(build, TEST_COMMIT, &"ab".repeat(32));
        ticket.make_current_apply_for_test(&mut app.native_updater_service);
        let (cancel, _cancelled) = std::sync::mpsc::sync_channel(1);
        app.pending_update_handoff = Some(crate::PendingUpdateHandoff {
            park_at: std::time::Instant::now(),
            proof_ready_at: None,
            attempt_id: 2,
            nonce: None,
            live: Vec::new(),
            adoption: Vec::new(),
            child_pid: None,
            mode: ApplyMode::Immediate,
            apply_attempt: Some(ticket),
            target_build: build,
            target_commit: TEST_COMMIT.to_string(),
            layout: crate::restore::RestoreManifest::new(Vec::new()),
            layout_digest: [0; 32],
            screen_digest: [0; 32],
            activity_epoch: app.update_handoff_activity_epoch,
            cancel,
            arbiter: crate::HandoffAttemptArbiter::new(),
            teardown: crate::DeferredHandoffTeardown::None,
            commit_drain_started: None,
            revoked_by_activity: true,
        });
        let _ = app.reduce_returned_handoff_completion(crate::UpdateHandoffCompletion {
            attempt_id: 2,
            nonce: None,
            child_pid: None,
            outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
            commit_fd: None,
            reject: None,
            reconcile: None,
            detail: "activity revoked handoff during physical preparation".to_string(),
            input_drain_spins: 0,
            child_death: crate::ChildDeathEvidence::Unobserved,
        });
        assert!(
            app.auto_overlap_retry.is_none(),
            "a person's revoked attempt must not consume — or create — the \
             automatic artifact's activity-revoked retry budget"
        );
        assert!(
            app.auto_apply_manual_only.is_none(),
            "nor may it latch the background lane off"
        );
    }

    /// THE TYPED KIND MUST REACH THE BUDGET, NOT JUST THE LOG LINE.
    ///
    /// `UpdateHandoffOutcome` distinguishes four physical failures and the
    /// completion path collapsed all of them into one bool on its way to the
    /// schedule, so the budget could not tell "the machine missed a 15 s deadline"
    /// from "these two images cannot agree on an adoption proof" and charged both
    /// the nine-attempt, fourteen-hour transient schedule.
    ///
    /// Driven through `reduce_returned_handoff_completion` — the reduction that
    /// owns the classification — with two artifacts in one `App` so each has its
    /// own budget and the only difference between the two arcs is the worker's
    /// verdict.
    #[test]
    fn a_structural_worker_outcome_reaches_a_different_schedule_from_a_transient_one() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        let deadline = |app: &App| {
            app.auto_apply_manual_only
                .expect("an automatic physical failure always latches manual-only")
                .retry_at
        };

        // AdoptionMismatch, twice: a confirmation and then the end of the lane for
        // those bytes. `retry_at: None` is what `arm` reads as `SuppressManualOnly`
        // until a strictly newer build ships.
        for _ in 0..2 {
            reduce_one_returned_failure(
                &mut app,
                ApplyMode::AutomaticPastGrace,
                build,
                &"ab".repeat(32),
                crate::UpdateHandoffOutcome::AdoptionMismatch,
                crate::ChildDeathEvidence::Unobserved,
            );
        }
        assert_eq!(
            deadline(&app),
            None,
            "two proofs that these two images disagree is not a busy afternoon; \
             the lane must be finished with the artifact, not scheduling a third \
             park/spawn/paint round trip"
        );

        // TimedOut, twice, same `App` and same build — different bytes, so a
        // different budget. Still scheduled, and on the SECOND rung, which is what
        // proves the two arcs really did diverge rather than one of them silently
        // inheriting the other's counter.
        for _ in 0..2 {
            reduce_one_returned_failure(
                &mut app,
                ApplyMode::AutomaticPastGrace,
                build,
                &"cd".repeat(32),
                crate::UpdateHandoffOutcome::TimedOut,
                crate::ChildDeathEvidence::Unobserved,
            );
        }
        let transient = deadline(&app)
            .expect("a transient failure two attempts in is still coming back")
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            transient > std::time::Duration::from_secs(1700)
                && transient <= std::time::Duration::from_secs(1800),
            "the transient lane must be on its second rung (~1800s), got {transient:?}"
        );
    }

    /// The classification itself, stated as a table so a future outcome variant
    /// has to be placed deliberately rather than falling into whichever arm the
    /// compiler allows. Three axes now, and every one of them used to be lossy:
    /// WHO asked (the mode), WHAT happened (the worker's typed outcome), and — for
    /// the one outcome that is not itself a classification — WHAT THE WORKER SAW.
    ///
    /// The whole table is driven with `Unobserved` evidence, which is the honest
    /// default for every outcome except `ChildDied` (no candidate died, or this
    /// process is the one that ended it). `ChildDied` therefore lands in
    /// `Unexplained` HERE, and its evidence-bearing arms are the table below.
    #[test]
    fn every_handoff_outcome_lands_in_the_lane_its_evidence_earns() {
        use crate::ChildDeathEvidence as Death;
        use crate::UpdateHandoffOutcome as Outcome;
        use crate::app_native::{HandoffFailureLane as Lane, PhysicalFailureShape as Shape};

        for (outcome, expected) in [
            (Outcome::AdoptionMismatch, Lane::Physical(Shape::Structural)),
            (
                Outcome::PreparationFailed,
                Lane::Physical(Shape::Structural),
            ),
            // NOT `Structural` any more, and not `Transient` either: with nothing
            // observed, proof EOF is a failure whose KIND is unknown.
            (Outcome::ChildDied, Lane::Physical(Shape::Unexplained)),
            (Outcome::TimedOut, Lane::Physical(Shape::Transient)),
            (Outcome::Rejected, Lane::Physical(Shape::Transient)),
            (Outcome::ActivityRevoked, Lane::ActivityRevoked),
            // Unreachable as a FAILURE (the commit path handles it), and it fails
            // closed to the forgiving shape rather than converging an artifact on
            // a state nobody understands.
            (Outcome::ProofReady, Lane::Physical(Shape::Transient)),
        ] {
            assert_eq!(
                Lane::classify(
                    ApplyMode::AutomaticPastGrace,
                    outcome,
                    Death::Unobserved,
                    false
                ),
                expected,
                "{outcome:?} in the background lane"
            );
            // A PERSON'S APPLY CHARGES NOTHING, whatever the worker reported: the
            // shape decides how much an AUTOMATIC failure costs, never whether a
            // human's click may converge the background lane.
            assert_eq!(
                Lane::classify(ApplyMode::Immediate, outcome, Death::Unobserved, false),
                Lane::Manual,
                "{outcome:?} from a person"
            );
            // The main thread's own activity-shaped rejection is the other half of
            // the activity observation and dominates the physical shape: a lossless
            // rollback the terminal caused is not evidence about the artifact.
            assert_eq!(
                Lane::classify(
                    ApplyMode::AutomaticPastGrace,
                    outcome,
                    Death::Unobserved,
                    true
                ),
                Lane::ActivityRevoked,
                "{outcome:?} with a main-thread activity revocation"
            );
        }
    }

    /// A STARVED CHILD RETRIES. The field defect, stated as the smallest thing
    /// that has to be true.
    ///
    /// 2026-08-21, owner's desk: build 1787699398 staged, verified,
    /// `relaunch_ready=true`, and twice `apply_failure = "overlap handoff failed
    /// safely: handoff proof ended ChildDied"` — recorded while the machine carried
    /// a load average of 140-160. When that load stopped, THE SAME BUILDS applied
    /// with no intervention. The old classification charged `ChildDied` the
    /// structural budget, so by the second failure the automatic lane was finished
    /// with those bytes and the user was left on "staged, applies on relaunch" —
    /// the exact state the seamless lane exists to delete — for a machine that was
    /// merely busy.
    ///
    /// `SIGKILL` is what that looks like when the parent CAN see it (macOS jetsam
    /// reclaiming memory; no process raises it on itself), so this drives the
    /// structural budget's worth of them and asserts the lane is still coming back.
    /// Then it spends the rest, because a lane that retried forever would be the
    /// opposite defect.
    #[test]
    fn a_starved_child_died_keeps_retrying_past_the_budget_a_refusal_would_spend() {
        use crate::app_native::{
            PHYSICAL_FAILURE_LIFETIME_ATTEMPTS, STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS,
        };
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        let starved = crate::ChildDeathEvidence::Signalled {
            signal: libc::SIGKILL,
        };
        let deadline = |app: &App| {
            app.auto_apply_manual_only
                .expect("an automatic physical failure always latches manual-only")
                .retry_at
        };

        for _ in 0..STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS {
            reduce_one_returned_failure(
                &mut app,
                ApplyMode::AutomaticPastGrace,
                build,
                &"ab".repeat(32),
                crate::UpdateHandoffOutcome::ChildDied,
                starved,
            );
        }
        let still_coming = deadline(&app)
            .expect(
                "THE REGRESSION: a machine that killed our candidate has said \
                 nothing about the new bytes, so the automatic lane must still be \
                 scheduled — `None` here is the converged latch the old \
                 unconditional Structural classification minted after two of these",
            )
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            still_coming > std::time::Duration::from_secs(1700)
                && still_coming <= std::time::Duration::from_secs(1800),
            "and it rides the epoch schedule's second rung (~1800s), which is what \
             proves it took the machine-shaped lane rather than a shortened one: \
             got {still_coming:?}"
        );

        // BOUNDED ALL THE SAME. Spend the rest of the transient lifetime and the
        // answer becomes the deadline-less latch: nine failures across three
        // independent epochs is evidence about the artifact however each one died.
        for _ in STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS..PHYSICAL_FAILURE_LIFETIME_ATTEMPTS {
            reduce_one_returned_failure(
                &mut app,
                ApplyMode::AutomaticPastGrace,
                build,
                &"ab".repeat(32),
                crate::UpdateHandoffOutcome::ChildDied,
                starved,
            );
        }
        assert_eq!(
            deadline(&app),
            None,
            "a lane that retries a starved child forever is the opposite defect; \
             the transient budget still ends"
        );
    }

    /// A SUCCESSOR THAT REFUSES CONVERGES, on the structural budget, and the FIRST
    /// failure is not what converges it.
    ///
    /// `Exited { code: 0 }` is the refusal this tree actually produces: a candidate
    /// whose boot apply could not swap stays the OLD build, refuses the authorized
    /// target identity, closes every adopted master and RETURNS from `main_entry`
    /// — a clean exit that the parent sees only as proof EOF. Five field failures
    /// in 2026-08 were exactly that. Reaching an `exit` instruction is what
    /// separates it from the starved child above: a process that never ran decides
    /// nothing.
    #[test]
    fn a_refusing_child_died_converges_to_manual_only_within_its_budget() {
        use crate::app_native::STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS;
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        let refused = crate::ChildDeathEvidence::Exited { code: 0 };
        let deadline = |app: &App| {
            app.auto_apply_manual_only
                .expect("an automatic physical failure always latches manual-only")
                .retry_at
        };

        reduce_one_returned_failure(
            &mut app,
            ApplyMode::AutomaticPastGrace,
            build,
            &"ab".repeat(32),
            crate::UpdateHandoffOutcome::ChildDied,
            refused,
        );
        let confirming = deadline(&app)
            .expect("ONE unlucky handoff never converges a lane, whatever it said")
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            confirming > std::time::Duration::from_secs(500)
                && confirming <= std::time::Duration::from_secs(600),
            "the confirming retry is the schedule's first rung (~600s), got \
             {confirming:?}"
        );

        for _ in 1..STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS {
            reduce_one_returned_failure(
                &mut app,
                ApplyMode::AutomaticPastGrace,
                build,
                &"ab".repeat(32),
                crate::UpdateHandoffOutcome::ChildDied,
                refused,
            );
        }
        assert_eq!(
            deadline(&app),
            None,
            "twice told that this successor will not become our successor is not a \
             busy afternoon: the lane is done with these bytes, and `retry_at: \
             None` is what `arm` reads as SuppressManualOnly"
        );
    }

    /// AND THE ONE IN THE MIDDLE — a death nobody witnessed, which is where a
    /// `ChildDied` lands whenever the evidence runs out
    /// ([`crate::ChildDeathEvidence::Unobserved`]). Reachable on both lanes and
    /// still the commonest answer on the shipping macOS one: a successor that
    /// refuses closes the readiness channel and keeps running, so the pre-kill
    /// look often finds it alive and the status that comes back afterwards is this
    /// process's own SIGKILL, which claims nothing.
    ///
    /// That verdict must be weaker than both of the ones above: it converges, but
    /// only after a genuinely independent sample of the machine. The stand-down at
    /// the end of the first epoch is that sample, and it is what the field case
    /// needed — the retry that finally worked came HOURS later, not at the 600 s
    /// and 1800 s rungs that re-ran the same pathological hour.
    #[test]
    fn an_unexplained_child_died_gets_a_second_epoch_and_then_stops() {
        use crate::app_native::{
            PHYSICAL_FAILURE_LIFETIME_ATTEMPTS, PHYSICAL_FAILURES_PER_EPOCH,
            UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS,
        };
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        let deadline = |app: &App| {
            app.auto_apply_manual_only
                .expect("an automatic physical failure always latches manual-only")
                .retry_at
        };
        let spend = |app: &mut App, times: u8| {
            for _ in 0..times {
                reduce_one_returned_failure(
                    app,
                    ApplyMode::AutomaticPastGrace,
                    build,
                    &"ab".repeat(32),
                    crate::UpdateHandoffOutcome::ChildDied,
                    crate::ChildDeathEvidence::Unobserved,
                );
            }
        };

        spend(&mut app, PHYSICAL_FAILURES_PER_EPOCH);
        let stand_down = deadline(&app)
            .expect("the first epoch ends in a stand-down, not in convergence")
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            stand_down > std::time::Duration::from_secs(5 * 60 * 60),
            "and the stand-down is hours, because re-sampling the SAME loaded hour \
             is what the in-epoch rungs already did: got {stand_down:?}"
        );

        spend(
            &mut app,
            UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS - PHYSICAL_FAILURES_PER_EPOCH,
        );
        assert_eq!(
            deadline(&app),
            None,
            "two independent samples of the machine is where 'the machine was \
             having a bad hour' stops being a credible explanation — and \
             {UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS} attempts is strictly under \
             the {PHYSICAL_FAILURE_LIFETIME_ATTEMPTS} a named transient failure \
             gets, which the compile-time assert beside the constants pins"
        );
    }

    /// THE EVIDENCE AXIS, as its own total table: every `ChildDeathEvidence` a
    /// worker can produce, and the shape it earns.
    ///
    /// THE FINDING: `ChildDied` was `Structural` unconditionally, on the argument
    /// that a candidate which exits before writing its readiness proof "is the
    /// successor image refusing to boot as a successor … the strongest statement
    /// about the new bytes available at this seam". A child starved of CPU exits
    /// before writing a readiness proof too — it never ran — and the owner's desk
    /// produced exactly that on 2026-08-21: two `ChildDied` applies under a load
    /// average of 140-160, then the SAME builds applying with no intervention once
    /// the load stopped. The verdict was about the machine and was charged to the
    /// bytes.
    ///
    /// Every arm below is a fact the worker OBSERVED, never a duration threshold
    /// and never a message string.
    #[test]
    fn a_dead_candidate_is_classified_by_what_the_worker_saw() {
        use crate::ChildDeathEvidence as Death;
        use crate::UpdateHandoffOutcome as Outcome;
        use crate::app_native::{HandoffFailureLane as Lane, PhysicalFailureShape as Shape};

        for (death, expected, why) in [
            (
                Death::Unobserved,
                Shape::Unexplained,
                "no witnessed status, or one that could only have been our own \
                 SIGKILL — and a verdict nobody witnessed must not be filed as \
                 one somebody did",
            ),
            (
                Death::Exited { code: 0 },
                Shape::Structural,
                "a clean exit is this tree's COMMONEST refusal — `main_entry` \
                 returns without a window when the overlap authority is \
                 incomplete — and reaching an exit instruction at all is proof \
                 the image ran",
            ),
            (
                Death::Exited { code: 74 },
                Shape::Structural,
                "the fail-stop exit a candidate takes when it can never become \
                 authoritative; same reading, which is why the CODE is recorded \
                 and not judged",
            ),
            (
                Death::Signalled {
                    signal: libc::SIGSEGV,
                },
                Shape::Structural,
                "a fault is the image executing itself into a wall, which is the \
                 bytes",
            ),
            (
                Death::Signalled {
                    signal: libc::SIGABRT,
                },
                Shape::Structural,
                "so is an abort",
            ),
            (
                Death::Signalled {
                    signal: libc::SIGKILL,
                },
                Shape::Transient,
                "THE FIELD CASE: no process raises SIGKILL on itself, and macOS \
                 jetsam sends it to reclaim memory. The next attempt on a calmer \
                 machine is the one that wins",
            ),
            (
                Death::Signalled {
                    signal: libc::SIGTERM,
                },
                Shape::Transient,
                "and any other signal from outside the image is equally not a \
                 statement about the bytes",
            ),
        ] {
            assert_eq!(
                Lane::classify(
                    ApplyMode::AutomaticPastGrace,
                    Outcome::ChildDied,
                    death,
                    false
                ),
                Lane::Physical(expected),
                "{death:?}: {why}"
            );
            // THE EVIDENCE IS READ FOR `ChildDied` AND FOR NOTHING ELSE. Every
            // other outcome describes a candidate THIS process ended, so its exit
            // status is our SIGKILL and says nothing about the artifact.
            assert_eq!(
                Lane::classify(
                    ApplyMode::AutomaticPastGrace,
                    Outcome::TimedOut,
                    death,
                    false
                ),
                Lane::Physical(Shape::Transient),
                "{death:?} must not move a TimedOut, whose candidate we killed"
            );
        }
    }
}

/// THE LANE CHOICE, and every reason it falls back.
///
/// This is the decision that says whether the user's next update produces a
/// survivor with a launchd application job or another pid-1 orphan, and it is
/// made from facts that are individually cheap and collectively easy to get
/// wrong. Each test below is one field of [`HandoffLaneFacts`] flipped against
/// an otherwise-eligible attempt, because a predicate that ignored a field would
/// otherwise pass every test written against the happy path.
#[cfg(all(test, target_os = "macos"))]
mod handoff_lane_tests {
    use super::{HandoffLaneFacts, app_bundle_root, launch_environment, out_of_band_lane_refusal};

    /// An attempt with nothing wrong with it.
    fn eligible() -> HandoffLaneFacts {
        HandoffLaneFacts {
            bundled: true,
            launcher_available: true,
            socket_path_fits: true,
            target_not_older: true,
            sessions: 3,
            environment_is_a_merge: true,
        }
    }

    #[test]
    fn an_eligible_attempt_takes_the_out_of_band_lane() {
        assert_eq!(out_of_band_lane_refusal(eligible()), None);
    }

    /// A dev build, `cargo run`, and the test harness all reach the seamless
    /// lane; none of them is a bundle, and LaunchServices has nothing to start.
    #[test]
    fn an_unbundled_process_forks() {
        let facts = HandoffLaneFacts {
            bundled: false,
            ..eligible()
        };
        assert!(out_of_band_lane_refusal(facts).is_some());
    }

    /// THE 16 BYTES. `$HOME` decides whether the rendezvous path fits
    /// `sun_path`, so on some perfectly ordinary machines this lane simply does
    /// not exist — and the fallback has to be silent and total rather than a
    /// bind that fails after the terminal has parked.
    #[test]
    fn a_rendezvous_path_that_does_not_fit_forks() {
        let facts = HandoffLaneFacts {
            socket_path_fits: false,
            ..eligible()
        };
        assert!(out_of_band_lane_refusal(facts).is_some());
    }

    /// THE DOWNGRADE GUARD the retired version advertisement carried. "Presence
    /// of the transport IS the version" closes old-parent/new-child and says
    /// NOTHING about new-parent/old-child — so an older authorized target must
    /// fork, or a successor with no rendezvous code is handed descriptors it
    /// cannot receive and every session is lost to a hang.
    #[test]
    fn an_older_authorized_target_forks() {
        let facts = HandoffLaneFacts {
            target_not_older: false,
            ..eligible()
        };
        assert!(out_of_band_lane_refusal(facts).is_some());
    }

    /// The transport, not the protocol, bounds the pool: one `SCM_RIGHTS`
    /// message carries 64 descriptors and two of them are the pipes.
    #[test]
    fn a_pool_too_wide_for_one_message_forks() {
        let limit = crate::handoff_rendezvous::MAX_RENDEZVOUS_SESSIONS;
        assert_eq!(
            out_of_band_lane_refusal(HandoffLaneFacts {
                sessions: limit,
                ..eligible()
            }),
            None,
            "exactly the limit still fits"
        );
        assert!(
            out_of_band_lane_refusal(HandoffLaneFacts {
                sessions: limit + 1,
                ..eligible()
            })
            .is_some(),
            "one more does not, and must fall back rather than fail at sendmsg"
        );
        assert!(
            out_of_band_lane_refusal(HandoffLaneFacts {
                sessions: 0,
                ..eligible()
            })
            .is_some(),
            "an overlap with no sessions is not an overlap"
        );
    }

    /// A LaunchServices launch MERGES its environment and cannot remove a
    /// variable. `bind_expected_update_artifact` removes three, and a successor
    /// that inherited a stale `ATERM_UPDATE_EXPECTED_*` would authenticate its
    /// staged bundle against the wrong artifact — so a removal that would
    /// actually remove something disqualifies the lane.
    #[test]
    fn an_environment_that_needs_a_removal_forks() {
        let facts = HandoffLaneFacts {
            environment_is_a_merge: false,
            ..eligible()
        };
        assert!(out_of_band_lane_refusal(facts).is_some());
    }

    /// The predicate that produces that fact, against a real `Command`: a
    /// removal of something this process does not have is a no-op (which is the
    /// common case, since `env_remove` is called unconditionally), and a removal
    /// of something it DOES have is what cannot be expressed.
    #[test]
    fn only_a_removal_that_would_remove_something_disqualifies_the_launch() {
        let mut command = std::process::Command::new("/bin/echo");
        command.env("ATERM_LANE_TEST_KEY", "value");
        command.env_remove("ATERM_LANE_TEST_ABSENT_KEY_THAT_IS_NOT_SET");
        let carried = launch_environment(&command).expect("a no-op removal is expressible");
        assert_eq!(
            carried,
            vec![(
                std::ffi::OsString::from("ATERM_LANE_TEST_KEY"),
                std::ffi::OsString::from("value")
            )],
            "only real assignments travel; a vacuous removal contributes nothing"
        );

        // The removal now names something this process really carries. Scoped
        // through the workspace's one lock-scoped env helper so no concurrent
        // test observes the mutation.
        aterm_log::env::scoped("ATERM_LANE_TEST_PRESENT_KEY", "set", || {
            let mut command = std::process::Command::new("/bin/echo");
            command.env_remove("ATERM_LANE_TEST_PRESENT_KEY");
            assert!(
                launch_environment(&command).is_none(),
                "a merge cannot un-set a variable the launcher's own environment has"
            );
        });
    }

    /// The bundle root is `<bundle>.app/Contents/MacOS/<bin>` and the `.app`
    /// suffix is CHECKED — a dev binary also sits three levels below something.
    #[test]
    fn only_a_dot_app_three_levels_up_is_a_bundle_root() {
        let temp = std::env::temp_dir().join(format!("aterm-lane-{}", std::process::id()));
        let bundle = temp.join("aterm.app");
        let macos = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).expect("fixture bundle");
        assert_eq!(
            app_bundle_root(&macos.join("aterm")).as_deref(),
            Some(bundle.as_path())
        );

        let plain = temp.join("target/debug/deps");
        std::fs::create_dir_all(&plain).expect("fixture dev tree");
        assert_eq!(
            app_bundle_root(&plain.join("aterm-gui")),
            None,
            "a dev build is three levels below a directory too, and is not a bundle"
        );
        assert_eq!(
            app_bundle_root(std::path::Path::new("aterm")),
            None,
            "and a bare name has no three levels at all"
        );
        let _ = std::fs::remove_dir_all(&temp);
    }
}

/// TIER-1 CONFORMANCE for the seamless handoff's PER-SESSION ownership model
/// ([`aterm_spec::derive::native_update_seamless_handoff_ownership_model`]).
///
/// A model that is green with no bind proves a property of the DESCRIPTION of
/// the handoff, not of the handoff. These tests drive the GENUINE decision
/// function out of this module over its whole bounded input space and check
/// that what the shipping code decides is exactly what the model admits.
///
/// IT LIVES HERE, INSIDE THE MODULE IT BINDS, on purpose: as a sibling file it
/// needed ten private items widened to `pub(crate)` — a production diff whose
/// only consumer was a test, which is exactly how a private seam stops being
/// private. A child module sees its parent's privates for free.
///
/// WHAT IS BOUND, and what is not, because that distinction is the whole value.
/// Bound: the final Commit admission — the predicate that decides whether an
/// attempt may transfer ownership at all. Modelled but NOT bound: the physical
/// surface those decisions gate (the ownership transfer and `_exit` inside
/// `seamless::commit_and_exit`, the reader park/resume, the `F_DUPFD_CLOEXEC`
/// duplication), because each either replaces this process or needs a live
/// event loop with real sessions. Those are the QA-seam tests' job.
///
/// Three further binds were written and DELETED rather than shipped: their model
/// assertions were loop-invariant constants sitting beside the real-code
/// assertions, which reads as conformance and is not — the model half asserted
/// the same thing on every iteration regardless of what the code answered. One
/// honest bind is worth more than four that look thorough.
#[cfg(all(test, unix))]
mod ownership_conformance {
    use super::{HandoffCommitFacts, handoff_commit_admitted, handoff_rejection_activity_shaped};
    use aterm_spec::derive::{Model, native_update_seamless_handoff_ownership_model};
    use aterm_spec::interp::{State, admits, with_buggy};

    const TO_DESCRIPTORS_TRANSFERRED: [&str; 4] = [
        "ParkOutgoingReaders",
        "CaptureCheckpoints",
        "StartReaderlessCandidate",
        "DuplicateDescriptorsToCandidate",
    ];

    /// The same prefix plus the proof, i.e. the model's `ProofMatched`.
    const TO_PROOF_MATCHED: [&str; 5] = [
        "ParkOutgoingReaders",
        "CaptureCheckpoints",
        "StartReaderlessCandidate",
        "DuplicateDescriptorsToCandidate",
        "MatchAdoptionProof",
    ];

    /// Fire a deterministic sequence, checking at every step that the model admits
    /// exactly the named action — never that it admits *something*.
    fn walk(model: &Model, actions: &[&'static str]) -> State {
        let mut state = model.init_state();
        for &action in actions {
            let next = model.successors(action, &state);
            assert_eq!(
                next.len(),
                1,
                "{action} must be deterministically enabled at {state:?}"
            );
            assert_eq!(admits(model, &state, &next[0]), Some(action));
            state = next[0].clone();
        }
        state
    }

    fn assert_every_invariant_holds(model: &Model, state: &State) {
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, state),
                "state violates {}::{}: {state:?}",
                model.name,
                invariant.name,
            );
        }
    }

    /// Every reachable state, by BFS over the same successor relation the
    /// interpreter's own bounded model check uses. Small by construction (two
    /// sessions, and the protocol is a chain), so the bound is a regression
    /// tripwire rather than a real limit.
    fn reachable(model: &Model) -> Vec<State> {
        let mut seen: std::collections::BTreeSet<State> = std::collections::BTreeSet::new();
        let mut queue: std::collections::VecDeque<State> = std::collections::VecDeque::new();
        let init = model.init_state();
        seen.insert(init.clone());
        queue.push_back(init);
        while let Some(state) = queue.pop_front() {
            for action in &model.actions {
                for next in model.successors(action.name, &state) {
                    if seen.insert(next.clone()) {
                        queue.push_back(next);
                    }
                }
            }
        }
        assert!(
            seen.len() < 20_000,
            "the ownership model's bounded space regressed to {} states",
            seen.len()
        );
        seen.into_iter().collect()
    }

    /// Is a state satisfying `goal` reachable from `from`?
    fn reaches(model: &Model, from: &State, goal: impl Fn(&State) -> bool) -> bool {
        let mut seen: std::collections::BTreeSet<State> = std::collections::BTreeSet::new();
        let mut queue: std::collections::VecDeque<State> = std::collections::VecDeque::new();
        seen.insert(from.clone());
        queue.push_back(from.clone());
        while let Some(state) = queue.pop_front() {
            if goal(&state) {
                return true;
            }
            for action in &model.actions {
                for next in model.successors(action.name, &state) {
                    if seen.insert(next.clone()) {
                        queue.push_back(next);
                    }
                }
            }
        }
        false
    }

    fn facts_from_bits(bits: u32) -> HandoffCommitFacts {
        HandoffCommitFacts {
            exact_sessions: bits & (1 << 0) != 0,
            exact_layout: bits & (1 << 1) != 0,
            exact_activity: bits & (1 << 2) != 0,
            teardown_allows_commit: bits & (1 << 3) != 0,
            parent_still_parked: bits & (1 << 4) != 0,
            sessions_alive: bits & (1 << 5) != 0,
            input_dispatch_fenced: bits & (1 << 6) != 0,
            egress_settled: bits & (1 << 7) != 0,
            native_safe: bits & (1 << 8) != 0,
            proof_exact: bits & (1 << 9) != 0,
            commit_channel: bits & (1 << 10) != 0,
        }
    }

    /// The model's `revoked` is "some mutable pre-Commit admission fact turned
    /// false". `parent_still_parked` and `proof_exact` are deliberately NOT folded
    /// into it: the model carries those two as variables of their own
    /// (`out_readers`, `proof_matched`) because they are the two facts the ownership
    /// invariants are actually about — who is reading, and what licensed a transfer.
    fn model_revoked(facts: HandoffCommitFacts) -> bool {
        !(facts.exact_sessions
            && facts.exact_layout
            && facts.exact_activity
            && facts.teardown_allows_commit
            && facts.sessions_alive
            && facts.input_dispatch_fenced
            && facts.egress_settled
            && facts.native_safe
            && facts.commit_channel)
    }

    /// Project one bounded fact combination onto the model's pre-Commit state.
    ///
    /// A projection can express states the healthy model proves unreachable — a
    /// parent reader that resumed mid-attempt is exactly one — and that is the
    /// point: the bind then checks that the shipping admission and the model's guard
    /// refuse the SAME combinations.
    fn project_commit_facts(
        transferred: &State,
        matched: &State,
        facts: HandoffCommitFacts,
    ) -> State {
        let mut state = if facts.proof_exact {
            matched.clone()
        } else {
            transferred.clone()
        };
        state.insert("revoked", i64::from(model_revoked(facts)));
        state.insert("out_readers", i64::from(!facts.parent_still_parked));
        state
    }

    /// THE FINAL ADMISSION, exhaustively. `handoff_commit_admitted` is the compiled
    /// conjunction standing immediately before the attempt-wide Commit CAS, so it —
    /// and nothing in this file — decides whether a session's ownership moves. All
    /// 2^11 of its bounded fact combinations are driven through it and through the
    /// model's `CommitAtomically` guard, and the two must agree combination for
    /// combination.
    #[test]
    fn real_commit_admission_conforms_to_the_ownership_model_over_every_bounded_fact_combination() {
        let model = native_update_seamless_handoff_ownership_model();
        let transferred = walk(&model, &TO_DESCRIPTORS_TRANSFERRED);
        let matched = walk(&model, &TO_PROOF_MATCHED);
        let mut admitted_combinations = 0usize;
        let mut refused_parked: std::collections::BTreeSet<State> =
            std::collections::BTreeSet::new();

        for bits in 0..(1u32 << 11) {
            let facts = facts_from_bits(bits);
            let before = project_commit_facts(&transferred, &matched, facts);
            let admitted = handoff_commit_admitted(facts);
            let successors = model.successors("CommitAtomically", &before);
            assert_eq!(
                admitted,
                !successors.is_empty(),
                "the shipping admission and the model's Commit guard disagree for {facts:?}"
            );

            // An activity-shaped rejection is a REFUSAL first and a retry
            // classification second: it may never coexist with an admitted Commit,
            // or the automatic lane would spend budget re-attempting a handoff that
            // already landed.
            if handoff_rejection_activity_shaped(facts) {
                assert!(
                    !admitted,
                    "an activity-shaped rejection cannot also be admitted: {facts:?}"
                );
            }

            if !admitted {
                // The parent's own readers are the one fact that does not describe
                // the attempt's rollback prospects: once a reader has resumed, that
                // axis of the rollback has already happened. Every refusal with the
                // readers still parked must have a path back to Resumed.
                if facts.parent_still_parked {
                    refused_parked.insert(before);
                }
                continue;
            }

            admitted_combinations += 1;
            let after = successors[0].clone();
            assert_eq!(admits(&model, &before, &after), Some("CommitAtomically"));
            assert_every_invariant_holds(&model, &after);
            assert_eq!(after["commits"], 1);
            assert_eq!(after["owner_a"], 2, "session a moved to the candidate");
            assert_eq!(after["owner_b"], 2, "session b moved to the candidate");
            assert_eq!(after["out_live"], 0, "the outgoing process exited");
            assert_eq!(
                after["cand_readers"], 0,
                "the candidate's reader gate opens after Commit, never with it"
            );
        }

        // ROLLBACK IS AVAILABLE FOR EVERY REFUSAL — genuine or activity-shaped, and
        // whichever fact turned false. Checked once per DISTINCT projected state
        // rather than once per fact combination; the projection is many-to-one.
        assert!(refused_parked.len() > 1, "vacuous refusal projection");
        for state in &refused_parked {
            assert!(
                reaches(&model, state, |candidate| {
                    candidate["phase"] == 7
                        && candidate["out_live"] == 1
                        && candidate["owner_a"] == 1
                        && candidate["owner_b"] == 1
                        && candidate["commits"] == 0
                }),
                "no rollback path to Resumed from the refused state {state:?}"
            );
        }

        // Non-vacuity: the compiled conjunction admits EXACTLY the
        // all-facts-hold combination, so the sweep above is not passing because
        // everything happens to be refused.
        assert_eq!(
            admitted_combinations, 1,
            "exactly one bounded combination may authorize a Commit"
        );
    }

    /// THE READINESS WAIT'S VERDICT. `classify_ready_poll` is the compiled function
    /// that turns one `poll` answer into "the adopted set is stale", "read the
    /// proof", or "yield". Exhausted over every `revents` combination for the proof
    /// fd and a two-master pool — the same pool cardinality the model carries.
    #[test]
    fn every_pre_commit_state_has_a_path_back_to_resumed_with_both_sessions_still_ours() {
        let model = native_update_seamless_handoff_ownership_model();
        let states = reachable(&model);
        assert!(states.len() > 10, "vacuous reachable space");

        let (mut pre_commit, mut committed, mut settled) = (0usize, 0usize, 0usize);
        for state in &states {
            assert_every_invariant_holds(&model, state);
            if state["commits"] > 0 {
                committed += 1;
                continue;
            }
            if state["phase"] == 7 || state["phase"] == 8 {
                settled += 1;
                continue;
            }
            if state["phase"] == 0 {
                continue;
            }
            pre_commit += 1;
            assert!(
                reaches(&model, state, |candidate| {
                    candidate["phase"] == 7
                        && candidate["out_live"] == 1
                        && candidate["out_readers"] == 1
                        && candidate["owner_a"] == 1
                        && candidate["owner_b"] == 1
                        && candidate["commits"] == 0
                }),
                "no rollback path to Resumed from {state:?}"
            );
        }
        assert!(
            pre_commit > 0 && committed > 0 && settled > 0,
            "the sweep must see all three lanes: {pre_commit} pre-commit, \
             {committed} committed, {settled} settled"
        );

        let buggy = with_buggy(&model, 1);
        let broken = reachable(&buggy);
        assert!(
            broken
                .iter()
                .any(|state| !buggy.check_invariant("NoSessionIsEverOrphaned", state)),
            "the mutant must be able to orphan a session"
        );
        assert!(
            broken
                .iter()
                .any(|state| !buggy.check_invariant("NeverTwoReadersOnOneMaster", state)),
            "the mutant must be able to put two readers on one master"
        );
    }
}
