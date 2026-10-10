// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Seamless update handoff, an inherent-impl split of `App`:
//! `apply_staged_update_now`, the unix overlap worker (`run_handoff_worker`, the
//! bounded readiness wait, the unique reaper and rejection plumbing), the reap
//! authority that licenses a rollback (`HandoffRollbackWarrant`), the Commit-time
//! admission facts, and `finish_update_handoff`'s drain gate, Commit and rollback.
//!
//! The successor races the parent's in-flight `atpkg` pass like a second window:
//! the orphaned child keeps the store lock for its pass, so the successor's launch
//! lanes `--wait-lock` behind it (no row is painted for the wait). A new-build
//! successor runs one full update pass after its seed
//! (`crate::launch_update_park`). A rejected candidate's sweep SIGKILLs its process
//! group, atpkg child included (the store is crash-consistent).

#[cfg(unix)]
use winit::event_loop::ActiveEventLoop;

use crate::App;
#[cfg(unix)]
use crate::Wake;
#[cfg(unix)]
use crate::app_input::paste_order;

// The seamlessness invariant is proved by the compiler in the sibling
// `app_update_handoff_island.rs` (`clean { … }` island). It is a separate file
// because a compiler without the Clean surface cannot lex an island body, and only
// a `mod` declaration decides whether a file is read. The `clean_islands` cfg is
// set for the Trust lanes in .cargo/config.toml, so the stable cross lanes still
// compile aterm-gui.
#[cfg(clean_islands)]
#[path = "app_update_handoff_island.rs"]
mod island;

/// Tier-1 for `NativeUpdateLaunchedDialWait`: the outgoing process's wait for its
/// launched successor's dial ends on LaunchServices' "terminated before it
/// checked in" and on the kernel's word that a pid LaunchServices named is gone,
/// and on no answer a live successor gets.
#[cfg(all(test, target_os = "macos"))]
#[path = "app_update_handoff_dial_wait_tests.rs"]
pub(crate) mod launched_dial_wait_conformance;

#[cfg(all(test, target_os = "macos"))]
#[path = "app_update_handoff_dialer_admission_tests.rs"]
pub(crate) mod launched_dialer_admission_conformance;

/// Everything one attempt captured under the park: the facts that depend on
/// every reader being stopped.
///
/// Separate from [`HandoffWorkerJob`] because on the launched lane the worker is
/// already running when the main thread parks, so the park's product reaches it as
/// its own message ([`HandoffTransferJob`]). The fork lane fills it at the job's
/// construction.
#[cfg(unix)]
struct HandoffCapture {
    /// When the parent started parking its readers: the zero point of the user's
    /// freeze and of the 20 ms capture deadline. Data only (the worker reports the
    /// park->transfer half on the launched lane).
    #[cfg(target_os = "macos")]
    park_at: std::time::Instant,
    /// When the main thread finished this capture and handed it over: the end of
    /// the `capture` slice of park->transfer. Data only, like `park_at`.
    #[cfg(target_os = "macos")]
    captured_at: std::time::Instant,
    manifest: crate::session_store::SessionHandoff,
    fds: crate::session_store::HandoffFds,
    screens: Vec<(u64, aterm_core::terminal::TerminalCheckpoint)>,
    /// The sessions carried with a screen that is not exactly what the program
    /// drew (clamped or blank, never one that lost only link destinations:
    /// `seamless::CarryRung::needs_repaint`); the writer marks them
    /// `ScreenCarry::repaint` so the successor makes the program redraw.
    repaint: Vec<u64>,
    /// Each session's control-carry capture from the freeze: the archive's fence,
    /// counters and differ state, and the engine and ledger to export the rest
    /// from on this worker (`crate::handoff_carry`).
    carries: Vec<crate::handoff_carry::CarrySource>,
    window: Option<crate::session_store::WindowCarry>,
    layout: crate::restore::RestoreManifest,
    layout_digest: [u8; 32],
    screen_digest: [u8; 32],
    live: Vec<(u64, i32, i32)>,
    /// The identity triples the adoption proof hashes, which are not `live`.
    ///
    /// `live` holds descriptor numbers this process can `poll` and hand over. The
    /// proof's middle term is the descriptor number on the fork lane (`execve`
    /// copies the table) and the PTY device on the `SCM_RIGHTS` lane
    /// (`handoff_rendezvous::pty_device_term`).
    proof_identities: Vec<(u64, i32, i32)>,
    _owned_masters: Vec<std::os::fd::OwnedFd>,
    /// The history carry (`crate::handoff_history`): each handed session's history
    /// fence as the park saw it, and how the lane exports the history the worker
    /// joins it to ([`crate::handoff_history::HistoryPlan`]).
    history_heads: Vec<crate::handoff_history::HistoryHead>,
    history: crate::handoff_history::HistoryPlan,
    /// The final screens of panes kept open after their commands exited:
    /// best-effort, outside both digests, written as the manifest's side list
    /// (`seamless::write_outgoing_with_held`).
    held: Vec<crate::seamless::HeldScreen>,
}

/// The park's product, sent from the main thread to a worker that is already
/// holding a successor's claim (or already knows it must fork instead).
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) struct HandoffTransferJob {
    capture: HandoffCapture,
    /// When the post-park proof wait gives up, computed from the park instant
    /// ([`handoff_proof_deadline`]) so the deadline bounds the freeze the user
    /// feels, not the launch.
    proof_deadline: std::time::Instant,
}

/// The main thread's typed reason for standing a held successor down before the
/// park delivered anything: which outcome the completion should carry and why.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Debug)]
pub(crate) struct HandoffStandDown {
    pub(crate) outcome: crate::UpdateHandoffOutcome,
    pub(crate) detail: String,
}

/// The launched lane's side of the late park: the attempt nonce (minted on the
/// main thread so the launch environment can name the manifest), the channels the
/// park's product and a stand-down arrive on, and how long the worker holds a
/// dialled successor for a park that never comes.
#[cfg(target_os = "macos")]
struct HandoffPrelaunchLane {
    nonce: String,
    transfer: std::sync::mpsc::Receiver<HandoffTransferJob>,
    stand_down: std::sync::mpsc::Receiver<HandoffStandDown>,
    /// Where the main thread answers `Wake::UpdateHandoffStandingDown`: the worker
    /// waits on this before it revokes, so the park gate is closed and any parked
    /// readers are resumed before anything is killed. See
    /// [`retire_ungranted_successor`].
    stand_down_ack: std::sync::mpsc::Receiver<()>,
    hold_bound: std::time::Duration,
    /// The advisory warm hint the successor is launched with (`handoff_warm_hint`):
    /// this process's window facts and font zoom at launch time. A guess the
    /// successor warms at, never trusted; the park's capture is what it adopts.
    warm_hint: String,
    /// The layout at launch time (`App::capture_handoff_layout`, every reader
    /// live), which the worker writes as the layout sidecar before the park
    /// (`handoff_diet`). A guess, never handed over as one: the park's own
    /// capture is sealed against it byte for byte, and wins.
    speculative_layout: Option<crate::restore::RestoreManifest>,
    /// The successor runs an update CANDIDATE — a staged build its boot swap
    /// puts in place, or the installed bundle an activation names — rather than
    /// this image (the same-image QA seam). Settled on the main thread from the
    /// attempt's ticket and seam, never from whether the worker re-verifies: a
    /// candidate a fresh cached pass lets the worker skip is still a candidate
    /// (round eight of the update audit, U36's review). Decides the code its
    /// dialer must run ([`Self::dialer_code`]).
    successor_runs_candidate: bool,
}

#[cfg(target_os = "macos")]
impl HandoffPrelaunchLane {
    /// The code this lane's dialer must run to take the grant
    /// ([`launched_dialer_code`]), for the release team `team`, the bundle
    /// LaunchServices launches and the candidate's signing identifier, read
    /// only when the release rule is stated.
    fn dialer_code(
        &self,
        team: &str,
        bundle: &std::path::Path,
        candidate_identifier: impl FnOnce() -> Result<String, String>,
    ) -> Result<crate::handoff_rendezvous::DialerCode, String> {
        launched_dialer_code(
            self.successor_runs_candidate,
            team,
            bundle,
            candidate_identifier,
        )
    }
}

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
    /// This attempt activates the installed bundle at the executable's own path
    /// (`ApplyAttemptTicket::is_installed_activation`): the pre-verification runs
    /// against that bundle (there is no staged `.app`), and the successor gets no
    /// expected-artifact triple (it swaps nothing; it is the newer build).
    installed_activation: bool,
    /// Where a pass of that pre-verification is published, with the handoff policy
    /// it read: the cache `App::spawn_staged_handoff_preverification` fills at arm
    /// time, under the attempt's own artifact key. `None` when there is no
    /// artifact to name (the same-image QA seam).
    preverified: Option<PreverifyPublisher>,
    /// The carry ceiling the fork lane's capture was taken under, before this
    /// worker read the candidate's policy — checked against it once read
    /// ([`capture_exceeds_policy`]). `None` on the launched lane, which parks
    /// after this worker's verification.
    parked_under: Option<aterm_update_core::handoff_policy::CarryCeiling>,
    /// The verified candidate declares the chunked rendezvous grant in its sealed
    /// `Info.plist` (read by a passed pre-verification: this worker's own, or the
    /// cached pass the lane was chosen on). Only then does the launch environment
    /// carry `ATERM_HANDOFF_GRANT_CAPS`, so a successor that never declared it is
    /// never offered one.
    successor_grant_chunks: bool,
    command: std::process::Command,
    /// What the park produced. `Some` from construction on the fork lane; `None`
    /// on the launched lane until the main thread's [`HandoffTransferJob`] arrives,
    /// after the successor has launched and dialled. Every step from the artifact
    /// write onward reads it through [`Self::capture`]; the pre-grant stand-down
    /// never reads it.
    capture: Option<HandoffCapture>,
    /// The late park's channels, present exactly on the launched lane.
    #[cfg(target_os = "macos")]
    prelaunch: Option<HandoffPrelaunchLane>,
    /// Which transport this attempt chose, decided on the main thread before
    /// anything parked (see [`out_of_band_lane_refusal`]). macOS-only: every other
    /// unix has one lane.
    #[cfg(target_os = "macos")]
    lane: HandoffLane,
    /// The `.app` root to hand LaunchServices on the out-of-band lane. `None` when
    /// this process is not running from a bundle, which refuses that lane.
    #[cfg(target_os = "macos")]
    bundle: Option<std::path::PathBuf>,
    cleanup: HandoffWorkerCleanup,
    cancel: std::sync::mpsc::Receiver<()>,
    arbiter: crate::HandoffAttemptArbiter,
}

/// What the successor's signed handoff policy asks of one park, as
/// [`crate::App::park_policy`] reads it for every lane.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ParkPolicy {
    /// The policy to follow, already narrowed to this build; `None` when there
    /// is none to follow.
    pub(crate) policy: Option<aterm_update_core::handoff_policy::HandoffPolicy>,
    /// Whether a passed pre-verification of the attempt's artifact is on record;
    /// `false` means the candidate's policy is unread, so `policy` says nothing.
    pub(crate) known: bool,
    /// The candidate's sealed `Info.plist` declares the chunked rendezvous grant
    /// (read by the same passed pre-verification), so the launched lane may hand
    /// it up to [`crate::handoff_rendezvous::MAX_CHUNKED_SESSIONS`] sessions
    /// rather than [`crate::handoff_rendezvous::MAX_RENDEZVOUS_SESSIONS`]. `false`
    /// when nothing passed.
    pub(crate) grant_chunks: bool,
}

#[cfg(unix)]
impl ParkPolicy {
    /// The highest rung the park may carry a session at.
    #[must_use]
    pub(crate) fn carry_ceiling(self) -> aterm_update_core::handoff_policy::CarryCeiling {
        self.policy.map_or(
            aterm_update_core::handoff_policy::CarryCeiling::Full,
            |policy| policy.carry_ceiling(),
        )
    }

    /// Whether the automatic lane's `Land` phase may park over queued output.
    /// Read only by the launched lane's park, which runs on macOS (and in the
    /// unix tests that drive it).
    #[cfg(any(target_os = "macos", all(test, unix)))]
    #[must_use]
    pub(crate) fn relaxes_land_gate(self) -> bool {
        self.policy.is_some_and(|policy| policy.relaxes_land_gate())
    }
}

/// THE HELD PANES' OWN WINDOW (round five, item 19; round six of the update
/// audit, finding 21), as a pure function of the clock so it is pinned
/// without one: the held capture runs only when at least `comfort` (half the
/// freeze budget) is still left before the park's `deadline`, and then stops
/// a QUARTER of the budget (`comfort / 2`) short of it — the layout capture
/// and the callers' deadline check still come after it, and a held capture
/// that ran to the deadline itself turned every landed park into a miss.
/// `None` means there is no time for held panes at all.
#[cfg(unix)]
fn held_capture_deadline(
    deadline: std::time::Instant,
    now: std::time::Instant,
    comfort: std::time::Duration,
) -> Option<std::time::Instant> {
    if deadline.saturating_duration_since(now) < comfort {
        return None;
    }
    Some(deadline.checked_sub(comfort / 2).unwrap_or(deadline))
}

/// A FORK-LANE PARK TAKEN ABOVE ITS SUCCESSOR'S POLICY is refused before the child
/// starts (the review of plan P0-5): the reason, or `None` when the capture may be
/// handed over.
///
/// With no cached pass the fork lane parks under `parked_under` (Full) before the
/// worker reads the policy; a lower policy fails the attempt as a retrying
/// producer failure with the pass published, so the next park follows it. `None`
/// on the launched lane.
#[cfg(unix)]
pub(crate) fn capture_exceeds_policy(
    parked_under: Option<aterm_update_core::handoff_policy::CarryCeiling>,
    adopted: Option<aterm_update_core::handoff_policy::HandoffPolicy>,
) -> Option<String> {
    let parked_under = parked_under?;
    let policy = adopted?;
    (policy.carry_ceiling() > parked_under).then(|| {
        format!(
            "the successor's handoff policy ({policy}) asks for less than this park carried \
             (carry={}), and the fork lane read it only after parking; nothing was handed \
             over, and the next attempt parks under it",
            parked_under.as_str()
        )
    })
}

/// The worker's handle on the App's pre-verification cache
/// (`crate::HandoffPreverification`) for the artifact its attempt targets.
///
/// WHY THE WORKER PUBLISHES (the 2026-09-22/23 update audit, plan P0-5): the
/// cache used to be filled only when the stage was ARMED, and its verdict stands
/// in for re-running `codesign` for ten minutes. An attempt later than that —
/// a busy terminal, an explicit apply of a stage nothing armed — re-verified on
/// this worker and kept the answer to itself, so the successor's handoff policy
/// it read never reached the launched lane's park, which happens on the main
/// thread after this check. Published, the park reads it by the same key.
///
/// A REFUSAL IS PUBLISHED TOO, when it is a verdict (round eight, finding 14):
/// [`Self::publish_refusal`] keeps every refusal that is not a passing moment
/// exactly as the arm-time read caches one, and it fails the attempt here as
/// well. From then on it is the cache's word on that artifact: for
/// [`crate::HANDOFF_PREVERIFY_FRESHNESS`] the automatic lane's short-circuits
/// (`start_unix_update_handoff`, `App::cold_lane_verdict_gate`) answer its
/// attempts with it before anything parks or relaunches,
/// `App::settle_rearmed_apply_failure` books a returned automatic attempt by it (a
/// refusal only a person can clear takes the environment block, not the
/// structural budget), until a later verdict replaces it. A person's press is
/// never answered by it: their worker verifies afresh and publishes what it
/// finds — and a pass published after the folder's block was taken is what the
/// next reconcile releases that block on (`App::install_location_answered_since`).
/// A passing refusal is still never published: the next attempt measures the
/// moment again.
#[cfg(unix)]
struct PreverifyPublisher {
    slot: std::sync::Arc<std::sync::Mutex<Option<crate::HandoffPreverification>>>,
    build: u64,
    commit: String,
    artifact: String,
}

#[cfg(unix)]
impl PreverifyPublisher {
    /// The publisher for `attempt`'s artifact, or `None` without one.
    fn for_attempt(
        slot: &std::sync::Arc<std::sync::Mutex<Option<crate::HandoffPreverification>>>,
        attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
    ) -> Option<Self> {
        let attempt = attempt?;
        Some(Self {
            slot: std::sync::Arc::clone(slot),
            build: attempt.target_build(),
            commit: attempt.target_commit().to_string(),
            artifact: attempt.target_dmg_sha256().to_string(),
        })
    }

    /// Publish a pass with the policy the verification read, narrowed to the
    /// running build `current_build` (logged once, by `adopt`), and the candidate's
    /// chunked-grant declaration; returns the narrowed policy.
    fn publish_pass(
        &self,
        facts: &aterm_update::HandoffCandidateFacts,
        current_build: u64,
        candidate: &str,
    ) -> Option<aterm_update_core::handoff_policy::HandoffPolicy> {
        let policy =
            aterm_update_core::handoff_policy::adopt(&facts.policy, current_build, candidate);
        *self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(crate::HandoffPreverification {
                build: self.build,
                commit: self.commit.clone(),
                artifact: self.artifact.clone(),
                at: std::time::Instant::now(),
                passed: true,
                reason: None,
                policy,
                grant_chunks: facts.grant_chunks,
            });
        policy
    }

    /// Publish the worker's own REFUSAL `reason` — when it is a verdict, never a
    /// passing moment, exactly as the arm-time read caches one
    /// (`App::spawn_staged_handoff_preverification`) — so the returned attempt
    /// is booked by the verifier's word as the synchronous arm books it (round
    /// eight of the update audit, finding 14): an attempt that outran the
    /// arm-time read met a refusal only a person can clear here, filed
    /// `PreparationFailed` (structural), and two of them stranded the build
    /// as one that "couldn't take over".
    fn publish_refusal(&self, reason: &str) {
        if aterm_update::is_passing_refusal(reason) {
            return;
        }
        *self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(crate::HandoffPreverification {
                build: self.build,
                commit: self.commit.clone(),
                artifact: self.artifact.clone(),
                at: std::time::Instant::now(),
                passed: false,
                reason: Some(reason.to_string()),
                policy: None,
                grant_chunks: false,
            });
    }

    /// [`Self::publish_pass`] of a candidate that declares no chunked grant —
    /// for the policy tests, which are about the policy alone.
    #[cfg(test)]
    fn publish_policy_pass(
        &self,
        read: &aterm_update_core::handoff_policy::PolicyRead,
        current_build: u64,
        candidate: &str,
    ) -> Option<aterm_update_core::handoff_policy::HandoffPolicy> {
        self.publish_pass(
            &aterm_update::HandoffCandidateFacts {
                policy: read.clone(),
                grant_chunks: false,
            },
            current_build,
            candidate,
        )
    }
}

#[cfg(unix)]
impl HandoffWorkerJob {
    /// The park's product. Every caller runs after the park by construction (the
    /// fork lane fills it at construction; the launched lane proceeds past its
    /// hold only once the transfer job lands), so absence is a logic error.
    fn capture(&self) -> &HandoffCapture {
        self.capture
            .as_ref()
            .expect("the handoff worker reads the capture only after the park delivered it")
    }
}

/// Why an attempt may run with no staged artifact to authenticate: it re-runs the
/// image already executing and swaps nothing. Carries no build, commit, digest or
/// path, so no authority can point at another image. The QA seam is the only case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SameImageHandoff {
    /// `ATERM_DEBUG_SEAMLESS_REEXEC=1` — the QA seam that exercises the full handoff and
    /// adopt path without a release. Byte-identical in behaviour to before this type.
    DebugSeam,
}

/// How this attempt hands its descriptors to the successor.
///
/// The fork lane is the only one without a bundle; the out-of-band lane is the
/// only one whose successor gets its own launchd application job. Chosen once on
/// the main thread before any reader parks, because it decides the term the
/// adoption proof hashes.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandoffLane {
    /// `fork` + `execve`: descriptors travel by inheritance and the proof hashes the
    /// descriptor number, which both sides agree on because `execve` copies the
    /// table verbatim.
    Fork,
    /// LaunchServices + a single-use `SCM_RIGHTS` rendezvous. The successor is
    /// launchd's child with its own application job; descriptors arrive at numbers
    /// the receiver's kernel chose, so the proof hashes the PTY device instead.
    OutOfBand,
}

/// Facts the lane choice is made from. Pure, so the decision and every fallback
/// reason are testable without a bundle, socket or terminal.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandoffLaneFacts {
    /// This process runs from inside a `.app` whose root we could name; only a
    /// bundle gets an `application.<bundle-id>.<hex>` job.
    bundled: bool,
    /// A launcher for this platform is compiled in.
    launcher_available: bool,
    /// The composed rendezvous path fits `sun_path` on this machine (narrowly: see
    /// `handoff_rendezvous`'s module docs for `$HOME`'s 16 bytes of headroom).
    socket_path_fits: bool,
    /// The authorized `target_build` is at least this build. The transport's
    /// presence settles old-parent/new-child (a parent without out-of-band code
    /// sends `ATERM_SEAMLESS_FDS`), but not new-parent/old-child, so an older
    /// successor is never offered descriptors it cannot receive.
    target_not_older: bool,
    /// Sessions to hand over.
    sessions: usize,
    /// The candidate declares the chunked rendezvous grant (`ATermHandoffGrantChunks`
    /// in its sealed `Info.plist`, [`ParkPolicy::grant_chunks`]): the grant may span
    /// several messages, up to [`crate::handoff_rendezvous::MAX_CHUNKED_SESSIONS`];
    /// otherwise [`crate::handoff_rendezvous::MAX_RENDEZVOUS_SESSIONS`].
    grant_chunks: bool,
    /// The launch environment can be expressed as a merge. A LaunchServices launch
    /// merges over this process's environment and cannot express a removal.
    environment_is_a_merge: bool,
}

/// Why this attempt may not take the out-of-band lane, or `None` when it may. A
/// reason rather than a bool makes a fallback diagnosable.
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
    if facts.sessions == 0
        || facts.sessions > crate::handoff_rendezvous::grant_session_limit(facts.grant_chunks)
    {
        return Some(if facts.grant_chunks {
            "the session count does not fit the successor's chunked descriptor grant"
        } else {
            "the session count does not fit one descriptor message"
        });
    }
    if !facts.environment_is_a_merge {
        return Some("the launch environment needs a removal a merge cannot express");
    }
    None
}

/// The `.app` root containing `exe` (`<bundle>.app/Contents/MacOS/<bin>`), if any.
/// The suffix is checked because dev builds and test harnesses also sit three
/// levels deep, and LaunchServices would fail on them only after a deadline.
#[cfg(target_os = "macos")]
#[must_use]
pub(crate) fn app_bundle_root(exe: &std::path::Path) -> Option<std::path::PathBuf> {
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

/// Whether a successor of `target_build` reads the mailbox a handoff carries
/// (`SessionRecord::fabric`): one at least as new as this build. An older number is
/// a downgrade or channel switch, a build that may predate the carry.
#[cfg(unix)]
fn successor_reads_fabric_carry(target_build: u64, running_build: u64) -> bool {
    target_build >= running_build
}

/// The hold fence the Commit raises ([`App::fence_hold_serials`]): no `hold` lands
/// between the Commit's hold-serial read and `_exit`, where a halt answered `OK`
/// would be lost. Dropping it lifts every fence; a Commit that lands never drops it.
#[cfg(unix)]
pub(crate) struct HoldFence(Vec<std::sync::Arc<crate::fabric::SessionFabric>>);

#[cfg(unix)]
impl Drop for HoldFence {
    fn drop(&mut self) {
        for fabric in &self.0 {
            fabric.lift_hold_fence();
        }
    }
}

#[cfg(unix)]
impl HoldFence {
    /// The hold serials read under this fence, still raised: what raising it
    /// returned, read again ([`crate::fabric::SessionFabric::carry_serial`]).
    /// Raising a second fence over it instead would leave two guards to lift
    /// one flag, and the first to drop would lift it under the other.
    fn serials(&self) -> u64 {
        self.0
            .iter()
            .map(|fabric| fabric.carry_serial())
            .fold(0, u64::wrapping_add)
    }
}

/// A COMMIT THAT HOLDS ITS CLAIM THROUGH ONE MORE INPUT-DISPATCH FENCE (round
/// eight of the update audit, #18): what it answered once it had claimed the
/// attempt (`App::answer_at_commit_claim`: a live `video` request above all,
/// whose answer may not come before the claim) took real time, and a key the OS
/// queued to this process meanwhile would die with the `_exit`. The claim is
/// kept across the run loop's dispatch — the worker cannot reject a claimed
/// attempt — with what the claim's admission took, so nothing it fenced off
/// lands before the write: the resident operator's quiesce and the hold fence.
/// The next pass of `App::finish_proven_handoff` reads the admission again
/// under them and commits, or rolls the claim back. The supervisor host is not
/// among them: it runs through the fence, as before any claim, and stops at the
/// write — its stop clears the badges its workers raised, which the held pass
/// would read as an escalation moved since the park (the review of #18). Nor
/// is the successor activated before the write (`App::activate_successor_at_write`,
/// the second review of #18): through the fence the keyboard and the focus
/// stay this window's, so its run loop delivers the keys to the masters and a
/// refusal kills a successor that never had them.
#[cfg(unix)]
pub(crate) struct HeldCommit {
    /// The resident operator's quiesce (`None`: there is no operator).
    operator_quiesce: Option<crate::operator_host::UpdateQuiesce>,
    /// The hold fence the claim's admission raised.
    hold_fence: HoldFence,
}

/// Fresh mutable facts required immediately before the attempt-wide Commit CAS. A
/// pure conjunction; `ProofReady` alone never grants replacement authority.
///
/// Queued PTY output and queued hardware input are not facts here: the carry
/// digest is taken at parser ground and kernel-queued bytes replay through the
/// child's parser, and the drain gate in `finish_update_handoff` flushes input
/// into the still-open masters. `sessions_alive` rejects a dead shell.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandoffCommitFacts {
    exact_sessions: bool,
    exact_layout: bool,
    /// The agents the handoff layout carries for the successor to relaunch
    /// ([`commit_agent_carry`]) match the pending layout's: otherwise a relaunch
    /// owed in between would be lost or run twice. Activity.
    exact_agent_carry: bool,
    /// The activity epoch is unchanged since the attempt was armed, in every lane.
    ///
    /// It also requires every session's hold serial to be where the park found it
    /// (`App::hold_serials`): a `hold` answered after the manifest was drawn would
    /// never reach the successor.
    exact_activity: bool,
    teardown_allows_commit: bool,
    parent_still_parked: bool,
    sessions_alive: bool,
    /// The OS input queue has been dispatched into the masters: the main thread ran
    /// the event loop for a bounded interval after ProofReady, so every hardware
    /// event the OS had accepted flowed through the tolerated input path to the
    /// still-open PTY masters (see the drain gate in `finish_update_handoff`).
    input_dispatch_fenced: bool,
    /// Process-local egress is drained to the kernel: no tolerated keystroke is
    /// left in the paste-order FIFO or a wedged-tty sink spill, which would die
    /// with `_exit`. Asserted directly; below the spec model's abstraction.
    egress_settled: bool,
    native_safe: bool,
    proof_exact: bool,
    commit_channel: bool,
    /// THE RESIDENT OPERATOR, INSIDE THE ADMISSION (round eight of the update
    /// audit, #70). It used to be a thirteenth conjunct beside this struct —
    /// `handoff_commit_admitted(facts) && operator_quiesce_error.is_none()` —
    /// so neither the ownership model's Tier-1 nor the retry taxonomy below
    /// saw it, and a momentarily busy operator refused an all-true fact set
    /// that was then filed as a physical failure against the build. Two facts,
    /// because its refusal asks two different things
    /// ([`crate::operator_host::UpdateQuiesceRefusal`]):
    ///
    /// `operator_idle` — no operator transaction was in flight and its gate
    /// was free when the Commit asked (or there is no operator). False is
    /// ACTIVITY: the operator doing work, as a person typing is, and the next
    /// attempt finds it idle.
    operator_idle: bool,
    /// `operator_quiesced` — the operator is fenced for the replacement (or
    /// there is none). False with `operator_idle` true is a FAULT: it is
    /// shutting down, or a quiesce is already active or went stale.
    operator_quiesced: bool,
}

/// The resident operator's two Commit facts ([`HandoffCommitFacts::operator_idle`],
/// [`HandoffCommitFacts::operator_quiesced`]) from its answer to the quiesce or
/// to the Commit permit: `None` is a fenced operator, or none at all.
#[cfg(unix)]
fn operator_commit_facts(
    refusal: Option<crate::operator_host::UpdateQuiesceRefusal>,
) -> (bool, bool) {
    (
        !refusal.is_some_and(crate::operator_host::UpdateQuiesceRefusal::is_busy),
        refusal.is_none(),
    )
}

/// The Commit-time layout comparison, reduced to window/tab/pane topology.
/// Normalized away:
///
/// * Window position, show state and raw inner size: a drag or maximize during
///   the boot is tolerated and the successor restores the pending capture's.
/// * Per-session cwd/title: `restore_session_meta` degrades to empty on a
///   `WouldBlock` try_lock; the child inherits the live shells anyway.
///
/// A terminal leaf's carried agent is compared apart ([`commit_agent_carry`]).
/// Settings drafts are compared: a key typed into Settings during the boot refuses
/// the Commit as activity, and the next attempt carries the newer text.
#[cfg(unix)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "NativeUpdateSettingsDraftCarry",
        action = "Commit",
        project = "aterm_gui::settings_draft_carry_conformance::Rig::project"
    )
)]
pub(crate) fn commit_layout_topology(
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
                // The agent the successor relaunches is not topology; it is
                // compared on its own (`commit_agent_carry`).
                terminal.agent = None;
            }
            // Native/placeholder leaves compare in full. The terminal leaf's user
            // metadata is read under a blocking lock, so it cannot degrade, and
            // stays compared: a `meta set` answered between the two captures must
            // reject the Commit, since the successor restores the pending layout's.
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
        // Live show state, not topology (macOS does not record it:
        // `app_restore::TRACKS_NORMAL_FRAME`). The grid stays compared.
        window.maximized = None;
        // The rest of the macOS show state (full screen, minimized, stack place,
        // key) is live too and not topology. The successor restores the pending
        // capture's show state, as it does its position.
        window.show = crate::restore::WindowShow::UNKNOWN;
        // The raw inner size is live like the position: a drag inside one cell
        // changes it and no grid. The successor sizes from the pending capture's,
        // and only when it holds that capture's grid, which stays compared.
        window.inner_px = None;
        // …and the presence row that size holds, a sizing fact of the same
        // capture: the grid it took stays compared.
        window.presence_row = false;
        // Both projections: the capture writes each session's cwd/title into the
        // legacy `tabs` mirror and the canonical `restored_tabs` tree.
        for tab in &mut window.tabs {
            strip_pane(tab);
        }
        for tab in &mut window.restored_tabs {
            strip_tree(&mut tab.root);
        }
    }
    topology
}

/// WHAT THE HANDOFF LAYOUT'S LEAVES CARRY OF THE SUPERVISOR HOST'S AGENTS
/// (round seven, finding 111): each terminal leaf's session and the agent the
/// successor is to relaunch there (`TerminalLeafRestore::agent`,
/// `HostHandle::pending_restored_for`), in layout order — the half of a
/// handoff leaf [`commit_layout_topology`] leaves out, compared at Commit as
/// a fact of its own (`HandoffCommitFacts::exact_agent_carry`).
///
/// It is not topology, and it can move between the two captures with nothing
/// on screen moving: the cold-restore queue and a relaunch a worker's
/// back-off owes ([`crate::harness_host`]'s `Kept::owed`) are read live — an
/// agent that crashes while the successor boots becomes owed once its exit is
/// read and settled, a restored relaunch in flight lands, and a tab where a
/// person starts an agent again is owed no more.
#[cfg(unix)]
pub(crate) fn commit_agent_carry(
    layout: &crate::restore::RestoreManifest,
) -> Vec<(Option<u64>, Option<Box<crate::restore::AgentRestore>>)> {
    layout
        .handed_off_leaves()
        .into_iter()
        .map(|leaf| (leaf.local_id, leaf.agent))
        .collect()
}

#[cfg(unix)]
#[must_use]
fn handoff_commit_admitted(facts: HandoffCommitFacts) -> bool {
    facts.exact_sessions
        && facts.exact_layout
        && facts.exact_agent_carry
        && facts.exact_activity
        && facts.teardown_allows_commit
        && facts.parent_still_parked
        && facts.sessions_alive
        && facts.input_dispatch_fenced
        && facts.egress_settled
        && facts.native_safe
        && facts.proof_exact
        && facts.commit_channel
        && facts.operator_idle
        && facts.operator_quiesced
}

/// The human-readable reason a ProofReady completion did not Commit, derived
/// from the SAME [`HandoffCommitFacts`] the admission read so the string
/// cascade and the admission can never drift. The two Commit-race flags arrive
/// separately because they are decided after admission; `native_safety` and
/// `operator` ride along for their own words (`facts.native_safe` and the two
/// operator facts are their projections).
#[cfg(unix)]
fn handoff_rejection_reason(
    facts: HandoffCommitFacts,
    native_safety: &Result<crate::app_native::NativeUpdateSafetyToken, Vec<String>>,
    operator: Option<crate::operator_host::UpdateQuiesceRefusal>,
    commit_lost_arbiter: bool,
    commit_write_failed: bool,
) -> String {
    if !facts.operator_idle || !facts.operator_quiesced {
        format!(
            "resident operator could not quiesce for Commit: {}",
            operator.map_or_else(
                || "it refused the quiesce".to_string(),
                |refusal| refusal.to_string()
            )
        )
    } else if !facts.exact_sessions {
        "live terminal set changed during async preparation".to_string()
    } else if !facts.exact_layout {
        "window/tab/pane topology changed during async preparation".to_string()
    } else if !facts.exact_agent_carry {
        "an agent's relaunch became owed or was settled during async preparation".to_string()
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

/// Whether a rejection was activity (sessions, layout, epoch, deferred teardown,
/// undrainable typing) or genuine (safety, proof, channel, arbiter faults).
/// Activity rollback is lossless, so automatic mode may spend bounded retries on
/// it. Derived from the admission's facts, never from strings.
///
/// SESSION DEATH IS ACTIVITY, WHEREVER IT IS SEEN (round six, finding 37). A
/// handed-off shell ending mid-overlap — `exit` typed during the freeze, a task
/// pane's command finishing, an ssh link dropping — is the desk changing, not
/// evidence against the build: the adoption proof's live set is stale, the
/// attempt is refused (`sessions_alive` stays in the admission), and the next
/// attempt proves the post-exit set. The park files the very same fact as
/// `ActivityRevoked` ("a PTY session closed during automatic reader park"), so
/// this set, the worker's pre-Commit loop and `wait_handoff_ready` file it the
/// same way. It used to be left out here and filed `Rejected` there, which the
/// automatic lane booked as a TRANSIENT physical failure — 600 s, then 1800 s,
/// against the build's nine-attempt lifetime — so a machine whose agents run
/// short-lived panes pushed a healthy build back by half-hours and could spend
/// its whole budget. A person's apply is `Manual` whatever this says.
///
/// A BUSY RESIDENT OPERATOR IS ACTIVITY TOO (round eight, #70): a proposal or
/// observer transaction in flight at the Commit is the operator doing work,
/// and the next attempt finds it idle. It used to sit outside these facts, so
/// an all-true fact set refused by it read as no activity at all and was
/// filed `Physical(Transient)`. An operator that refused for any other reason
/// (`operator_quiesced` alone false) is a fault, and stays on the physical
/// schedule.
#[cfg(unix)]
fn handoff_rejection_activity_shaped(facts: HandoffCommitFacts) -> bool {
    !facts.exact_sessions
        || !facts.exact_layout
        || !facts.exact_agent_carry
        || !facts.exact_activity
        || !facts.teardown_allows_commit
        || !facts.sessions_alive
        || !facts.input_dispatch_fenced
        || !facts.egress_settled
        || !facts.operator_idle
}

/// The emergency reaper's job (reaper thread or inline fallback): kill the
/// readerless candidate and prove it terminated, release the claim, clean up, and
/// report the rejected completion, which is sent only after the warrant exists.
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

/// Why a parked PTY reader may be resumed. Rollback restarts a reader on every
/// handed-off master, so it is sound only while no other process can read them:
/// two readers on one master interleave the stream unrecoverably. Each variant
/// rules the candidate out as such a reader.
///
/// There is no "waited long enough" variant: a candidate that might be alive is
/// exactly when rollback must not run, so there is no give-up path.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
enum HandoffRollbackWarrant {
    /// The attempt failed before any candidate was spawned, so nothing outside
    /// this process has ever held a handed-off master.
    NoCandidate,
    /// A successor was launched but the out-of-band transfer never completed, so
    /// no descriptor left this process. Sound because this lane moves descriptors
    /// in one `sendmsg`; the successor finds the rendezvous gone and exits.
    #[cfg(target_os = "macos")]
    NeverTransferred,
    /// `wait` consumed the candidate: this process reaped it, so the pid cannot be
    /// recycled between proof and use.
    Reaped,
    /// We were not the candidate's parent, so no `wait` could answer for it, and
    /// the pid it was born at provably no longer names it. See
    /// [`handoff_candidate_terminated`] for the two proofs and why they
    /// establish the same fact `Reaped` does.
    Vanished,
}

#[cfg(unix)]
impl HandoffRollbackWarrant {
    /// Logs which authority licensed a rollback, once, when it is used. The
    /// outside proof gets a line (it shows which launch shape ran); the ordinary
    /// parent reap stays quiet.
    fn announce(self, candidate_pid: Option<u32>) {
        if self == Self::Vanished {
            aterm_log::info!(
                "update apply: rollback licensed by outside proof; candidate {candidate_pid:?} \
                 terminated without being ours to reap"
            );
        }
    }
}

/// The kernel's birth record for the process currently at a pid: its creation
/// instant in microseconds. Kernel-assigned and unique across pid reuse; compared
/// for equality only, never as a clock.
///
/// `seamless::ProcessBirth` reads the same fact to ask whether the handoff parent
/// is alive, with the opposite fail-safe direction (unreadable means dead there,
/// not proven here), so each lane keeps its own probe.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandoffCandidateBirth {
    seconds: u64,
    microseconds: u64,
}

/// Which process is at `pid` now, not whether it is alive. Not filtered by process
/// status: any reason the kernel declines to answer, including a zombie, means
/// nothing is concluded.
#[cfg(target_os = "macos")]
fn read_candidate_birth(pid: u32) -> Option<HandoffCandidateBirth> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    if pid <= 1 {
        return None;
    }
    // Only a record that names `pid` answers: one about some OTHER pid could
    // only mislead the comparison this feeds.
    let info = crate::quit_safety::pid_bsdinfo(pid)?;
    Some(HandoffCandidateBirth {
        seconds: info.pbi_start_tvsec,
        microseconds: info.pbi_start_tvusec,
    })
}

/// Off macOS the successor is always a fork child, so `wait` always answers and
/// this fallback is never needed. A non-fork transport there must implement it
/// first (`/proc/<pid>/stat` field 22 on Linux); otherwise termination is provable
/// only by pid vacancy.
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
    /// The kernel birth stamp for `pid`, captured while the pid provably named the
    /// candidate. `None` never weakens the proof (pid vacancy is sound alone); it
    /// only loses concluding termination from a recycled pid and keeping a SIGKILL
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
    /// The identity of a fork child this process has not reaped. The zombie entry
    /// pins the pid, so the stamp read here is that child's.
    fn of_unreaped_child(child: &std::process::Child) -> Self {
        let pid = child.id();
        Self {
            pid,
            birth: read_candidate_birth(pid),
        }
    }

    /// [`Self::of_unreaped_child`] for a stand-in a test started with
    /// `aterm_uds::spawnfd::command` (so it holds no other test's descriptor).
    #[cfg(test)]
    fn of_unreaped_spawned(child: &aterm_uds::spawnfd::Child) -> Self {
        let pid = child.id();
        Self {
            pid,
            birth: read_candidate_birth(pid),
        }
    }

    /// The identity of a candidate this process did not fork, from the pid the
    /// kernel attested at the rendezvous accept (`LOCAL_PEERPID`).
    ///
    /// launchd may reap the successor and free the pid at any moment, so the stamp
    /// is what makes the pid an identity, and what allows `signal_handoff_candidate`
    /// to signal a launched candidate directly.
    #[cfg(target_os = "macos")]
    fn of_attested_peer(pid: u32) -> Self {
        Self {
            pid,
            birth: read_candidate_birth(pid),
        }
    }

    /// The identity of the process LaunchServices named for a launch whose dial has
    /// not come. Unattested, so only watched ([`NamedSuccessorWatch`]), never
    /// signalled or admitted; the stamp lets a recycled pid read as gone.
    #[cfg(target_os = "macos")]
    fn of_named_pid(pid: u32) -> Self {
        Self {
            pid,
            birth: read_candidate_birth(pid),
        }
    }

    /// A candidate known only by its pid (the emergency reaper's `child_pid`).
    /// Sound, since vacancy proves termination, but it cannot conclude termination
    /// from a recycled pid and signals only the process group.
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

/// Whether the candidate has terminated, so it cannot hold or read the handed-off
/// PTY masters. Two independent proofs:
///
/// * Pid vacancy: `kill(pid, 0)` answering ESRCH. A running process keeps its pid,
///   so only termination produces it; pid reuse can hide this answer, never
///   fabricate it.
/// * Identity: something answers at the pid but its birth stamp differs. A pid is
///   not reallocated until its previous owner is reaped.
///
/// Both establish what `Child::wait` does. Anything else (zombie, unreadable
/// record, unconvertible pid) is unproven, and unproven never resumes a reader.
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

/// Whether this process leads its own process group, so a rejecting parent's
/// `kill(-pid)` reaches the updater helpers a candidate forks.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessGroupContainment {
    /// The kernel reports `getpgrp() == getpid()`. A child forked from here
    /// inherits that group, so `kill(-pid)` names a group whose every member
    /// descends from this process.
    OwnGroupLeader,
    /// The kernel reports a group this process does not lead: `kill(-pid)` would
    /// miss helpers forked from here and hit whoever leads that group, so no
    /// updater helper may be forked. Carries both numbers for the refusal message.
    Foreign {
        group: libc::pid_t,
        own: libc::pid_t,
    },
}

/// Puts the calling process in its own process group and proves it, so helpers
/// it forks later are inside the group a rejecting parent sweeps.
///
/// The successor-side twin of [`run_handoff_worker`]'s `pre_exec`
/// `setpgid(0, 0)`, for a LaunchServices-launched successor (launchd's child, so
/// no parent hook runs in it). Called from [`crate::main_entry`] before anything
/// can run another program, not from `seamless::prearm_incoming_fds`, which unit
/// tests run in-process.
///
/// The answer is `getpgrp()`, not `setpgid`'s return value: the only failure this
/// call shape admits is EPERM for a session leader, which already leads its group,
/// and reading the postcondition back catches any errno not anticipated.
///
/// Idempotent; the boot-apply re-exec keeps the process group across `execve`.
#[cfg(unix)]
#[must_use]
pub(crate) fn contain_own_process_group() -> ProcessGroupContainment {
    // SAFETY: `setpgid(0, 0)` acts on the calling process only; `getpgrp` and
    // `getpid` are side-effect-free. The `setpgid` result is ignored: the answer is
    // the postcondition read back right after.
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

/// Whether `pid` is still a child of this process, and so pinned to its number.
/// `false` only withholds a signal; an exited unreaped child is still pinned, so
/// only `ECHILD` answers `false`.
#[cfg(unix)]
fn candidate_is_our_child(pid: libc::pid_t) -> bool {
    matches!(
        probe_handoff_candidate(pid),
        HandoffCandidateProbe::Running | HandoffCandidateProbe::Exited
    )
}

/// What this process can learn about a candidate without changing anything.
///
/// One `waitid(WNOHANG | WNOWAIT)` answers two questions:
///   * Is it ours: `ECHILD` (a launchd-owned or already-reaped process) is the
///     discriminator [`candidate_is_our_child`] needs before a group sweep.
///   * Has it died: [`worker_reject_and_reap_handoff_child`] needs to know whether
///     the status it reaps is the candidate's own or its SIGKILL's.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandoffCandidateProbe {
    /// `waitid` refused: not a child of ours (the launched lane), or already
    /// reaped. Nothing may be concluded, and nothing is pinned.
    NotOurChild,
    /// Our child, and it has not exited (stopped counts as not exited).
    ///
    /// Not "it refused": a candidate being torn down looks the same between the
    /// kernel closing its descriptors and its zombie state becoming visible, and
    /// under heavy load a dying candidate is likelier to be caught there.
    Running,
    /// Our child, already exited, and still waitable — so its status is intact for
    /// the reap that follows.
    Exited,
}

/// Non-destructive: `WNOWAIT` leaves an exited child waitable, so the reaper's
/// `wait` still collects it and its status.
///
/// Reads `si_code`, a public `siginfo_t` field on every target (`si_pid` and
/// `si_status` are union accessors on Linux). An unchanged child leaves the zeroed
/// struct at `si_code == 0`; an exit writes `CLD_EXITED`, `CLD_KILLED` or
/// `CLD_DUMPED`. `si_signo` is not used because Darwin reports a stopped child
/// (`CLD_STOPPED`) even when only `WEXITED` is asked for.
#[cfg(unix)]
fn probe_handoff_candidate(pid: libc::pid_t) -> HandoffCandidateProbe {
    let Ok(id) = libc::id_t::try_from(pid) else {
        return HandoffCandidateProbe::NotOurChild;
    };
    // SAFETY: `info` is a zeroed out-parameter of exactly the type waitid fills,
    // and WNOWAIT consumes no child. Zeroing first makes the read below meaningful:
    // POSIX need not write the struct when WNOHANG finds nothing.
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
    match info.si_code {
        libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED => HandoffCandidateProbe::Exited,
        _ => HandoffCandidateProbe::Running,
    }
}

/// A non-parent's witness to how a candidate died.
///
/// The launched lane has no `wait`: `waitid` answers `ECHILD` about launchd's
/// child, so a `ChildDied` there could only be inferred (see
/// [`crate::ChildDeathEvidence`]). Darwin's `kqueue` `EVFILT_PROC` with
/// `NOTE_EXIT | NOTE_EXITSTATUS` delivers the full `wait(2)` status to a watcher
/// permitted to `SIGKILL` the target, which this lane is.
///
/// Registration `ESRCH` (the candidate is already gone) answers `None`. Compared
/// with [`probe_handoff_candidate`]:
///   * The event is durable: it stays queued in this process until read, so
///     launchd reaping the candidate cannot take it away.
///   * Asking late costs nothing: [`observe_candidate_death`] reads it before its
///     own SIGKILL and again once the candidate is gone.
///
/// The knote is one-shot (dequeued on read), so the watch keeps what it read and
/// answers the same on every call.
#[cfg(target_os = "macos")]
struct CandidateExitWatch(aterm_uds::exitwatch::ExitWatch);

/// The handoff's reading of `aterm_uds::exitwatch`, which the PTY keeper shares.
#[cfg(target_os = "macos")]
impl CandidateExitWatch {
    /// Registers while the candidate is alive (on the launched lane, at the
    /// rendezvous accept, when the kernel has just attested the pid). Every failure
    /// answers `None`, which costs the evidence, never the reap.
    fn watch(pid: u32) -> Option<Self> {
        aterm_uds::exitwatch::ExitWatch::watch(pid).map(Self)
    }

    /// The candidate's own `wait(2)` status if the kernel has recorded one; never
    /// blocks or reaps, and answers the same on every later call.
    fn exit_status(&self) -> Option<std::process::ExitStatus> {
        use std::os::unix::process::ExitStatusExt as _;
        self.0.exit_status().map(std::process::ExitStatus::from_raw)
    }
}

/// Does `pid` lead a process group of its own — is `-pid` a handle on the
/// candidate's helpers rather than on nothing? A kernel read at the moment of
/// use; `false` (including a failed read) only ever withholds the group sweep.
#[cfg(unix)]
fn candidate_leads_its_own_group(pid: libc::pid_t) -> bool {
    // SAFETY: `getpgid` is a side-effect-free libc getter.
    pid > 1 && unsafe { libc::getpgid(pid) } == pid
}

/// SIGKILLs a candidate whose identity was just corroborated: its group when
/// `-pid` provably names the candidate's own group, then the candidate itself.
///
/// Either proof licenses the group signal:
/// * [`candidate_leads_its_own_group`]: the kernel reports it leads group `pid`
///   (the launched lane).
/// * [`candidate_is_our_child`]: the fork lane's pin (`pre_exec` set the group;
///   an unreaped child owns its number). Needed because Darwin answers `getpgid`
///   on a zombie leader with ESRCH, which would leave its helpers alive.
#[cfg(unix)]
fn kill_corroborated_candidate(pid: libc::pid_t) {
    if pid <= 1 {
        return;
    }
    if candidate_leads_its_own_group(pid) || candidate_is_our_child(pid) {
        // SAFETY: SIGKILL to the process group one of the two proofs above just
        // showed is the candidate's own, against a pid just proven to name it.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    } else {
        aterm_log::warn!(
            "handoff candidate {pid} leads no process group of its own (it never \
             reached its containment); sending the direct signal only"
        );
    }
    // SAFETY: SIGKILL to the candidate itself, a pid just proven to name it.
    unsafe { libc::kill(pid, libc::SIGKILL) };
}

/// SIGKILLs the candidate before anything waits on it.
///
/// The group sweep (`-pid`) stops the candidate's ditto/codesign/spctl
/// descendants from mutating fixed updater paths after the leader is gone. Its
/// containment differs by lane:
///
/// * A forked candidate: `pre_exec` `setpgid(0, 0)` makes it lead its own group
///   before its image runs, so `-pid` is valid from the moment `spawn` returns.
/// * A launched candidate contains itself with [`contain_own_process_group`]
///   before forking any helper, and refuses to run when it cannot. The parent
///   reads `getpgid(pid) == pid` at the moment of use
///   ([`candidate_leads_its_own_group`]) and sends the group SIGKILL only then or
///   when the candidate is our unreaped child ([`kill_corroborated_candidate`]).
///
/// The direct signal is sent only when the identity is corroborated, since a
/// recycled bare pid names a stranger. Unwitnessed, only the group sweep is sent,
/// which is safe only while the pid is pinned by our unreaped fork child; a
/// launched candidate must therefore arrive with an identity. Rollback is licensed
/// by [`handoff_candidate_terminated`], never by the signal.
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
        HandoffCandidateIdentity::Corroborated => kill_corroborated_candidate(pid),
        HandoffCandidateIdentity::Unwitnessed => {
            // A group kill is sound only while the pid is pinned. The fork lane
            // pins it (an unreaped child owns its number); a launched candidate can
            // be reaped by launchd and `-pid` could then name a stranger's group.
            // The wire carries a bare integer, so ask the kernel at the moment of
            // use: `WNOWAIT` leaves an exited child waitable, and ECHILD means not
            // pinned.
            if candidate_is_our_child(pid) {
                // SAFETY: SIGKILL to the candidate's process group, whose number
                // the kernel just confirmed is held by a child of ours, then to the
                // candidate itself.
                //
                // The group sweep alone misses a child that inherited this
                // process's group, and the reject path's wait is unbounded, so the
                // candidate is also signalled directly; the pinned pid makes that
                // land on it rather than on a stranger.
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

/// Blocks until the candidate is provably terminated, then licenses rollback.
///
/// Unbounded on purpose: the proof is a precondition for resuming the parked
/// readers, since resuming while the candidate might still read the masters
/// silently corrupts the stream. `Child::wait` has no deadline either. The
/// interval below only decides when a candidate that will not die gets logged.
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

/// The reap's two products: the rollback warrant and, when this process's own
/// `wait` answered for the candidate, the status it collected. `None` on the
/// launched lane and for a launcher-shaped child. Read only once
/// [`HandoffCandidateProbe::Exited`] proved the candidate died before this lane
/// signalled it (see [`worker_reject_and_reap_handoff_child`]).
#[cfg(unix)]
struct ReapedHandoffCandidate {
    warrant: HandoffRollbackWarrant,
    status: Option<std::process::ExitStatus>,
}

/// Kills the rejected candidate and proves it gone, on the handoff worker; the
/// returned warrant licenses [`App::rollback_overlap`] to resume the readers.
///
/// Relies on the fork lane's containment (`pre_exec` `setpgid(0, 0)`), so the
/// group sweep in [`signal_handoff_candidate`] reaches the candidate's helpers.
/// The warrant never rests on the sweep: it comes from `wait` on our own child or
/// from [`wait_for_handoff_candidate_to_terminate`]'s outside proof. Also returns
/// the candidate's exit status when our `wait` answered for it.
#[cfg(unix)]
fn kill_and_reap_handoff_child(
    candidate: HandoffCandidate,
    handle: &mut HandoffCandidateHandle,
) -> ReapedHandoffCandidate {
    let child = match handle {
        HandoffCandidateHandle::Forked(child) => child,
        #[cfg(target_os = "macos")]
        HandoffCandidateHandle::Launched(_) => {
            // No `wait` answers for launchd's child (`ECHILD` is not evidence), so
            // the outside proof stands in. Signal first so descendants are killed
            // before anything blocks.
            signal_handoff_candidate(candidate);
            return ReapedHandoffCandidate {
                warrant: wait_for_handoff_candidate_to_terminate(candidate),
                status: None,
            };
        }
    };
    // Signal before any wait, so descendants are already condemned when the
    // direct child is reaped.
    signal_handoff_candidate(candidate);
    let child_is_candidate = child.id() == candidate.pid;
    // Preferred authority: `wait` on our own fork child proves termination and
    // consumes the pid in one step. It says something about the candidate only
    // when the child is the candidate; a launcher-shaped child (`open -n`) exits
    // once LaunchServices holds the successor.
    let reaped = child.wait().ok();
    if let Some(status) = reaped
        && child_is_candidate
    {
        return ReapedHandoffCandidate {
            warrant: HandoffRollbackWarrant::Reaped,
            status: Some(status),
        };
    }
    // Fallback: prove termination from outside. A launcher-shaped child's status
    // describes the launcher, so it is withheld.
    ReapedHandoffCandidate {
        warrant: wait_for_handoff_candidate_to_terminate(candidate),
        status: None,
    }
}

/// What this process may do to the candidate, which depends on how it was started.
///
/// `Child::wait` proves termination and consumes the pid in one step, but exists
/// only for a process we forked. A type rather than an `Option<&mut Child>` makes
/// the caller say which lane it is on, so the launched lane cannot inherit a
/// `wait` that would only answer `ECHILD`.
#[cfg(unix)]
enum HandoffCandidateHandle {
    /// Our own `spawn`. `wait` is available and is the preferred warrant.
    Forked(std::process::Child),
    /// launchd's, not ours: nothing may `wait`, and the warrant comes from
    /// [`handoff_candidate_terminated`]'s outside proof. The [`CandidateExitWatch`]
    /// (registered at the rendezvous accept, `None` if that failed) is this lane's
    /// only statement of how the candidate died. macOS only.
    #[cfg(target_os = "macos")]
    Launched(Option<CandidateExitWatch>),
}

#[cfg(unix)]
impl HandoffCandidateHandle {
    /// The candidate's own status if the kernel has recorded one, without blocking
    /// or reaping. `None` on the fork lane, whose status comes from `wait`
    /// ([`kill_and_reap_handoff_child`]).
    fn witnessed_exit_status(&self) -> Option<std::process::ExitStatus> {
        match self {
            Self::Forked(_) => None,
            #[cfg(target_os = "macos")]
            Self::Launched(watch) => watch.as_ref().and_then(CandidateExitWatch::exit_status),
        }
    }

    /// Whether the candidate has provably ended, asked without changing anything
    /// so the reject path still finds every fact it reads.
    ///
    /// The waits ask because EOF on the readiness channel needs every copy of the
    /// write end closed, and a sibling fork caught in its fork-to-exec window can
    /// hide the death. (`seamless::CommitReceiver` probes its parent for the same
    /// reason.)
    ///
    ///   * Fork lane: [`probe_handoff_candidate`]'s `waitid(WNOWAIT)`; the child
    ///     stays waitable and its pid stays pinned.
    ///   * Launched lane: [`handoff_candidate_terminated`]. It never reads the
    ///     one-shot [`CandidateExitWatch`], which the hold loop and stand-down own;
    ///     an unreaped zombie answers `false` until launchd reaps it.
    fn candidate_gone(&self, candidate: HandoffCandidate) -> bool {
        match self {
            // Only while the child is the candidate: a launcher-shaped child exits
            // as soon as the real successor is running (see
            // `kill_and_reap_handoff_child`), and its exit is nobody's death.
            Self::Forked(child) if child.id() == candidate.pid => {
                libc::pid_t::try_from(candidate.pid)
                    .is_ok_and(|pid| probe_handoff_candidate(pid) == HandoffCandidateProbe::Exited)
            }
            Self::Forked(_) => handoff_candidate_terminated(candidate),
            #[cfg(target_os = "macos")]
            Self::Launched(_) => handoff_candidate_terminated(candidate),
        }
    }
}

/// Acquires the worker's unique reaper capability. Losing to `Committing` means
/// this worker must neither signal nor reap the candidate while the UI thread
/// performs the Commit write.
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

/// The emergency reaper's kill from a bare `child_pid` (on its own thread, or
/// inline when that thread cannot be created).
///
/// A bare pid carries no evidence of the candidate's group containment: on the
/// fork lane it is `pre_exec`'s; for a launched candidate the group sweep is
/// best-effort. The warrant comes from `waitpid` or the outside proof either way
/// (see [`signal_handoff_candidate`]).
#[cfg(unix)]
fn emergency_kill_and_reap_handoff_child(pid: u32) -> HandoffRollbackWarrant {
    // Precondition: the caller won the Emergency reaper CAS, so no worker consumes
    // the candidate's identity concurrently. A fork child is also pinned to `pid`;
    // a launchd-owned one is not, so the fallback re-derives termination. The
    // group is signalled before any wait: `waitpid` reaps a dead leader, and its
    // helpers would otherwise keep mutating updater paths.
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
        // ECHILD or any other refusal: on the fork lane a reap already happened;
        // for a launchd-owned candidate `waitpid` is no authority. The refusal is
        // never evidence the candidate is gone; the outside proof decides.
        break false;
    };
    if reaped {
        return HandoffRollbackWarrant::Reaped;
    }
    wait_for_handoff_candidate_to_terminate(candidate)
}

/// Pre-park admission peek only: a readable byte on a live master, output its
/// reader has not taken yet. Automatic mode does not begin an overlap while output
/// flows. Mid-flight, queued output is tolerated; use [`handoff_masters_closed`]
/// there.
///
/// POLLIN only, never from a hung-up master: a dead master (macOS reports a dead
/// slave as `POLLIN|POLLHUP`) would make a `--hold` pane block every automatic
/// attempt. Death belongs to [`handoff_masters_closed`] and the post-park re-check.
#[cfg(unix)]
pub(crate) fn handoff_masters_have_activity(live: &[(u64, i32, i32)]) -> bool {
    peek_handoff_masters(live, |revents| {
        revents & libc::POLLIN != 0 && revents & HANDOFF_MASTER_DEAD == 0
    })
}

/// Mid-flight death peek: true only when a handed-off master reports
/// POLLHUP/POLLERR/POLLNVAL, meaning the session is gone or the descriptor is
/// invalid. Readable output does not count; it waits in the kernel queue for the
/// child's parser. `events: POLLIN` is still required: macOS evaluates a PTY
/// master's state only for requested events, and with `events: 0` reports nothing
/// (pinned by the paired unit test).
#[cfg(unix)]
pub(crate) fn handoff_masters_closed(live: &[(u64, i32, i32)]) -> bool {
    peek_handoff_masters(live, |revents| revents & HANDOFF_MASTER_DEAD != 0)
}

/// The `revents` bits of a handed-off master whose session is gone or whose
/// descriptor is invalid.
#[cfg(unix)]
const HANDOFF_MASTER_DEAD: libc::c_short = libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;

/// The two peeks' one `poll(2)`: every master in `live` with `events: POLLIN`
/// (load-bearing — see [`handoff_masters_closed`]) and timeout 0, a
/// non-consuming peek. Whether any master's `revents` satisfies `hit`; no
/// masters, or nothing reported, is `false`.
#[cfg(unix)]
fn peek_handoff_masters(live: &[(u64, i32, i32)], hit: impl Fn(libc::c_short) -> bool) -> bool {
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
    polled > 0 && fds.iter().any(|fd| hit(fd.revents))
}

/// Whether every handed-off session's process-local egress has reached the
/// kernel: no tolerated keystroke is still in the paste-order FIFO or a wedged-tty
/// sink spill, where `_exit` would destroy it. A master no longer in the pool is
/// settled (`sessions_alive`/`exact_sessions` reject its death). Live entries use
/// the sink-local counter; only an orphaned master consults the writer registry.
#[cfg(unix)]
fn handoff_egress_settled(pool: &crate::SessionPool, live: &[(u64, i32, i32)]) -> bool {
    live.iter().all(|(_, master, _)| {
        if let Some(session) = pool.iter().find(|session| session.master == *master) {
            !paste_order::is_ordering(&session.ctx.sink)
                && session.ctx.sink.egress_drained_to_kernel()
        } else {
            !paste_order::is_master_ordering_for_handoff(*master)
        }
    })
}

/// The names the artifact a successor must swap in travels under — read and
/// cleared by its boot apply (`aterm_update`'s `take_expected_artifact`).
const EXPECTED_BUILD_ENV: &str = "ATERM_UPDATE_EXPECTED_BUILD";
const EXPECTED_COMMIT_ENV: &str = "ATERM_UPDATE_EXPECTED_COMMIT";
const EXPECTED_DIGEST_ENV: &str = "ATERM_UPDATE_EXPECTED_DMG_SHA256";

fn bind_expected_update_artifact(
    command: &mut std::process::Command,
    attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
) {
    command
        .env_remove(EXPECTED_BUILD_ENV)
        .env_remove(EXPECTED_COMMIT_ENV)
        .env_remove(EXPECTED_DIGEST_ENV);
    if let Some(attempt) = attempt {
        command
            .env(EXPECTED_BUILD_ENV, attempt.target_build().to_string())
            .env(EXPECTED_COMMIT_ENV, attempt.target_commit())
            .env(EXPECTED_DIGEST_ENV, attempt.target_dmg_sha256());
    }
}

/// The name the cold lane's budget travels under across its `exec` (round eight,
/// #1's review): written by the cold arm beside the expected-artifact binding, and
/// read and CLEARED by the relaunched image before its boot apply
/// ([`ColdRelaunch::of_this_launch`]), so neither a swapped-in build's re-exec nor
/// any shell is born with it.
#[cfg(unix)]
const COLD_LANE_BUDGET_ENV: &str = "ATERM_UPDATE_COLD_LANE_BUDGET";

/// THE AUTOMATIC LANE'S MEMORY OF ONE ARTIFACT, carried across the cold lane's
/// `exec` (round eight, #1's review).
///
/// The cold lane's successor is this build in a NEW process, and every budget that
/// makes the automatic lane converge — the physical-failure count
/// (`auto_apply_physical_retry`) and the structural verdict
/// (`auto_apply_structural_verdict`) — lived in the process the `exec` replaced. In
/// the relaunched one they started empty, so a boot swap that kept refusing for a
/// reason the pre-park read cannot see (a stopped sibling holding the apply lock,
/// whose timeout is a passing refusal and is never cached; a candidate that cannot
/// start, which only the swap's own probe tries) was booked as the FIRST failure on
/// every relaunch: retry in 600 s — the moment the seeded verdict lapsed — and a
/// relaunch into the same refusal, about every ten minutes, for ever. Carried, the
/// relaunched process books its refusal where the schedule stands: 600 s, 1800 s,
/// the stand-down, convergence and its notice, as failures inside one process are.
///
/// Carried exactly: the count, how long ago its last failure was (so the
/// replenish window keeps its clock), which side of the swap each record keys, and
/// when the verdict's one re-sample is due (or that it is spent).
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ColdLaneBudget {
    pub(crate) failures: Option<CarriedFailures>,
    pub(crate) verdict: Option<CarriedVerdict>,
}

/// `auto_apply_physical_retry`, as [`ColdLaneBudget`] carries it.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CarriedFailures {
    pub(crate) cycles: u8,
    /// Seconds since the last of them (`AutoOverlapRetry::last_attempt`).
    pub(crate) since_secs: u64,
    pub(crate) activation: bool,
}

/// `auto_apply_structural_verdict`, as [`ColdLaneBudget`] carries it.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CarriedVerdict {
    /// Seconds until the one re-sample is due; `None` once it is spent.
    pub(crate) resample_in_secs: Option<u64>,
    pub(crate) activation: bool,
}

#[cfg(unix)]
impl ColdLaneBudget {
    /// `field=value` pairs joined by `;`, keyed by the artifact they are about. The
    /// producer and the consumer are the same build (the cold lane execs itself), and
    /// a name the reader does not know is skipped.
    pub(crate) fn encode(&self, build: u64, artifact: &str) -> String {
        let mut fields = vec![format!("build={build}"), format!("artifact={artifact}")];
        if let Some(failures) = self.failures {
            fields.push(format!("cycles={}", failures.cycles));
            fields.push(format!("since={}", failures.since_secs));
            fields.push(format!("activation={}", u8::from(failures.activation)));
        }
        if let Some(verdict) = self.verdict {
            fields.push(format!(
                "verdict={}",
                verdict
                    .resample_in_secs
                    .map_or_else(|| "spent".to_string(), |secs| secs.to_string())
            ));
            fields.push(format!(
                "verdict_activation={}",
                u8::from(verdict.activation)
            ));
        }
        fields.join(";")
    }

    /// [`Self::encode`]'s inverse — `None` unless the value is about exactly
    /// `build`'s `artifact`, so a budget can never answer for other bytes. A half
    /// that does not parse is dropped on its own: the lane then books from an empty
    /// record, as it did before anything was carried.
    pub(crate) fn decode(value: &str, build: u64, artifact: &str) -> Option<Self> {
        let field = |name: &str| {
            value
                .split(';')
                .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
        };
        let flag = |name: &str| match field(name) {
            Some("1") => Some(true),
            Some("0") => Some(false),
            _ => None,
        };
        if field("build")?.parse::<u64>().ok()? != build || field("artifact")? != artifact {
            return None;
        }
        let failures = (|| {
            Some(CarriedFailures {
                cycles: field("cycles")?.parse().ok()?,
                since_secs: field("since")?.parse().ok()?,
                activation: flag("activation")?,
            })
        })();
        let verdict = (|| {
            Some(CarriedVerdict {
                resample_in_secs: match field("verdict")? {
                    "spent" => None,
                    secs => Some(secs.parse().ok()?),
                },
                activation: flag("verdict_activation")?,
            })
        })();
        Some(Self { failures, verdict })
    }
}

/// The cold arm's half of [`ColdLaneBudget`]: `budget`, encoded for `attempt`'s
/// artifact, on the successor's environment — and never an inherited one.
#[cfg(unix)]
fn bind_cold_lane_budget(
    command: &mut std::process::Command,
    attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
    budget: Option<ColdLaneBudget>,
) {
    command.env_remove(COLD_LANE_BUDGET_ENV);
    if let (Some(attempt), Some(budget)) = (attempt, budget) {
        command.env(
            COLD_LANE_BUDGET_ENV,
            budget.encode(attempt.target_build(), attempt.target_dmg_sha256()),
        );
    }
}

/// The name the update-health warnings a cold-lane parent had said, and no
/// proof had answered, travel under across its `exec` (round eight of the
/// update audit, finding 79): written by the cold arm
/// ([`bind_cold_lane_health_said`]) and TAKEN by the window's entry after the
/// boot apply ([`take_cold_lane_health_said`]), whose re-exec into the swapped-in
/// build inherits it, as it inherits `ATERM_UPDATED_FROM` — so no shell and no
/// window opened from one is born with it. The consumer is always this build or
/// a newer one, as `aterm_update::RELAUNCH_OF_RUNNING_ENV`'s is.
pub(crate) const COLD_LANE_HEALTH_SAID_ENV: &str = "ATERM_UPDATE_HEALTH_SAID";

/// WHAT THE PARENT HAD SAID CROSSES THE COLD LANE TOO (round eight of the update
/// audit, finding 79). The seamless lanes carry it in the window carry
/// ([`crate::session_store::WindowCarry::update_health_said`], round six's
/// finding 54); the cold lane's `exec` carried nothing, so its successor started
/// with no memory of "Couldn't install updates", its landing recorded no
/// healing, and Settings ▸ Messages kept that warning as the last word on the
/// install lane — and a check or download warning the landing does not answer
/// was never taken up by the successor's own producer either. The parent's
/// `update_health_said` rides as JSON, the window carry's own shape; none rides
/// when nothing is unanswered.
#[cfg(unix)]
pub(crate) fn bind_cold_lane_health_said(
    command: &mut std::process::Command,
    said: &[crate::session_store::CarriedHealthSaid],
) {
    command.env_remove(COLD_LANE_HEALTH_SAID_ENV);
    if said.is_empty() {
        return;
    }
    if let Ok(wire) = aterm_json::to_string(said) {
        command.env(COLD_LANE_HEALTH_SAID_ENV, wire);
    }
}

/// The cold-lane parent's unanswered update-health warnings
/// ([`bind_cold_lane_health_said`]), TAKEN from this process's environment
/// whatever this launch is — nothing after it may be born with them — and
/// returned only when this run is an update's landing (`just_updated`: a refused
/// relaunch comes back as the same build, which lands nothing). Read by the
/// window's entry while it is still single-threaded, after the boot apply; a
/// value that does not parse is no warning, never a guess.
pub(crate) fn take_cold_lane_health_said(
    just_updated: bool,
) -> Vec<crate::session_store::CarriedHealthSaid> {
    let wire = aterm_log::env::take(COLD_LANE_HEALTH_SAID_ENV);
    if !just_updated {
        return Vec::new();
    }
    parse_cold_lane_health_said(wire.as_deref())
}

/// [`take_cold_lane_health_said`]'s reading of one value.
pub(crate) fn parse_cold_lane_health_said(
    wire: Option<&std::ffi::OsStr>,
) -> Vec<crate::session_store::CarriedHealthSaid> {
    wire.and_then(std::ffi::OsStr::to_str)
        .and_then(|wire| aterm_json::from_str(wire).ok())
        .unwrap_or_default()
}

/// THIS LAUNCH IS THE COLD LANE'S RELAUNCH OF ITS OWN BUILD (round eight, #1):
/// the artifact the outgoing image bound for it to swap in, when the launch is
/// that and nothing else — no handoff descriptors, `ATERM_UPDATED_FROM` naming
/// the very build now running (the cold lane execs itself; its boot swap is what
/// makes it the new build), and the whole binding present — with the lane's
/// budget for that artifact ([`ColdLaneBudget`]).
///
/// Why it matters: the process that decided to relaunch is gone, and with it
/// every memory of the attempt. When this launch's boot swap REFUSES, the image
/// that comes up is the old build again, on a desk with no terminal again — the
/// exact state that took the cold lane — and nothing told its automatic lane that
/// the relaunch had just been refused, nor how many times before. It re-armed the
/// same artifact and exec'd again, every few seconds, for as long as the cause
/// stood. [`ColdRelaunch::seed`] turns the refusal into the verdict this lane reads
/// before it relaunches ([`crate::App::cold_lane_verdict_gate`]), and hands back
/// the budget it is booked against.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ColdRelaunch {
    build: u64,
    commit: String,
    artifact: String,
    budget: Option<ColdLaneBudget>,
}

/// What a refused cold relaunch hands its automatic lane
/// ([`crate::App::seed_refused_cold_relaunch`]).
#[cfg(unix)]
#[derive(Clone, Debug)]
pub(crate) struct ColdRelaunchSeed {
    /// The boot swap's refusal, as the pre-park verdict on its artifact.
    pub(crate) verdict: crate::HandoffPreverification,
    /// The boot swap's refusal when it was only the wait for an update check to
    /// confirm the stage's signer ([`aterm_update::is_roster_confirmation_wait`];
    /// round eight, #503's second review): then it is no verdict at all, and the
    /// lane holds for the check instead of reading [`Self::verdict`].
    pub(crate) roster_wait: Option<String>,
    /// The artifact's digest as the automatic lane keys it; `None` when the
    /// binding's digest does not decode (no automatic attempt could have armed for
    /// it then).
    pub(crate) dmg_sha256: Option<[u8; 32]>,
    /// What the outgoing process had booked against these bytes.
    pub(crate) budget: ColdLaneBudget,
}

#[cfg(unix)]
impl ColdRelaunch {
    /// Read from this process's environment, BEFORE the boot apply consumes the
    /// binding, and never cleared here — except the budget, which is TAKEN here
    /// whatever this launch is: it is this function's alone, and nothing after it
    /// (a swapped-in build's re-exec, a shell) may be born with it.
    /// `handoff_successor`: the launch carries a handoff's descriptors (a seamless
    /// candidate, whose parent owns the verdict).
    pub(crate) fn of_this_launch(handoff_successor: bool) -> Option<Self> {
        let budget = aterm_log::env::take(COLD_LANE_BUDGET_ENV);
        if handoff_successor {
            return None;
        }
        Self::from_values(
            std::env::var_os("ATERM_UPDATED_FROM").as_deref(),
            std::env::var_os(EXPECTED_BUILD_ENV).as_deref(),
            std::env::var_os(EXPECTED_COMMIT_ENV).as_deref(),
            std::env::var_os(EXPECTED_DIGEST_ENV).as_deref(),
            budget.as_deref(),
            crate::build_info::build_number(),
        )
    }

    /// [`Self::of_this_launch`] over explicit values: `running` is this build's
    /// number as the successor's `ATERM_UPDATED_FROM` spells it.
    pub(crate) fn from_values(
        updated_from: Option<&std::ffi::OsStr>,
        build: Option<&std::ffi::OsStr>,
        commit: Option<&std::ffi::OsStr>,
        digest: Option<&std::ffi::OsStr>,
        budget: Option<&std::ffi::OsStr>,
        running: &str,
    ) -> Option<Self> {
        if updated_from? != std::ffi::OsStr::new(running) {
            return None;
        }
        let build = build?.to_str()?.parse::<u64>().ok().filter(|b| *b != 0)?;
        let commit = commit?.to_str().filter(|c| !c.is_empty())?.to_string();
        let artifact = digest?.to_str().filter(|d| !d.is_empty())?.to_string();
        let budget = budget
            .and_then(std::ffi::OsStr::to_str)
            .and_then(|value| ColdLaneBudget::decode(value, build, &artifact));
        Some(Self {
            build,
            commit,
            artifact,
            budget,
        })
    }

    /// What this relaunch's boot apply leaves its automatic lane — `Some` when the
    /// swap REFUSED (`Deferred`) or was rolled back (`ReExecFailed`): the refusal as
    /// the pre-park verification entry the automatic lane reads — the complete
    /// gate's own answer about these bytes on this machine, minutes fresher than any
    /// cached read — and the budget the outgoing process had booked against them.
    /// So the first automatic attempt of this process books the refusal by its typed
    /// fact, where the schedule stands, instead of relaunching into it: a person's to
    /// clear latches the environment block, a passing moment rides the transient
    /// schedule, anything else the structural one. A passing refusal is held too —
    /// unlike a read that did not finish, this one already cost a relaunch. Stamped
    /// `now`, keyed like every verdict, so it answers for nothing else.
    ///
    /// EXCEPT THE WAIT FOR A ROSTER CONFIRMATION (round eight, #503's second review):
    /// gate 4d's refusal says only that no check has confirmed the stage's signer for
    /// a day, which the check that runs right after this launch ends. Seeded as a
    /// refusal it was booked by the first automatic attempt as a transient PHYSICAL
    /// failure (the budget, the 600 s / 1800 s / 6 h latch, the `apply` streak) even
    /// when that check had already confirmed the stage, and its fresh verdict kept
    /// the arm-time read, and with it the lane's hold, from ever forming. It is
    /// carried as [`ColdRelaunchSeed::roster_wait`] instead: the lane holds for the
    /// check, charged nothing.
    pub(crate) fn seed(&self, outcome: &aterm_update::ApplyOutcome) -> Option<ColdRelaunchSeed> {
        let reason = match outcome {
            aterm_update::ApplyOutcome::Deferred(reason)
            | aterm_update::ApplyOutcome::ReExecFailed(reason) => reason,
            aterm_update::ApplyOutcome::NotApplicable | aterm_update::ApplyOutcome::NoUpdate => {
                return None;
            }
        };
        let budget = self.budget.unwrap_or_default();
        let roster_wait = matches!(outcome, aterm_update::ApplyOutcome::Deferred(_))
            .then_some(reason)
            .filter(|reason| aterm_update::is_roster_confirmation_wait(reason))
            .cloned();
        if roster_wait.is_some() {
            aterm_log::info!(
                "update apply: this launch is the cold relaunch of build {} for build {}, and \
                 its boot swap waits for an update check to confirm the stage's signer \
                 ({reason}); the automatic lane holds for that check, charged nothing, \
                 instead of relaunching into it",
                crate::build_info::build_number(),
                self.build,
            );
        } else {
            aterm_log::warn!(
                "update apply: this launch is the cold relaunch of build {} for build {}, and \
                 its boot swap refused ({reason}); the automatic lane books that refusal ({} \
                 earlier failure(s) of these bytes carried across the relaunch) instead of \
                 relaunching into it again",
                crate::build_info::build_number(),
                self.build,
                budget.failures.map_or(0, |failures| failures.cycles)
            );
        }
        Some(ColdRelaunchSeed {
            verdict: crate::HandoffPreverification {
                build: self.build,
                commit: self.commit.clone(),
                artifact: self.artifact.clone(),
                at: std::time::Instant::now(),
                passed: false,
                reason: Some(format!(
                    "the relaunched app's install of build {} refused: {reason}",
                    self.build
                )),
                policy: None,
                grant_chunks: false,
            },
            roster_wait,
            dmg_sha256: crate::app_native::decode_dmg_sha256(&self.artifact),
            budget,
        })
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
            // Nothing observed is the default: other outcomes either have no
            // candidate or describe one this process ended. Only the `ChildDied`
            // producer overrides it.
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

/// Publishes a non-ready completion, which runs [`App::rollback_overlap`] and
/// resumes the parked readers. Call only while holding a warrant; the completion
/// crosses a channel and cannot carry the proof.
#[cfg(unix)]
fn send_warranted_handoff_failure(
    warrant: HandoffRollbackWarrant,
    cleanup: &HandoffWorkerCleanup,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    current_build: u64,
    completion: crate::UpdateHandoffCompletion,
) {
    if publish_warranted_handoff_failure(warrant, cleanup, proxy, completion) {
        send_post_failure_reconcile_facts(cleanup, proxy, current_build);
    }
}

/// The first half of [`send_warranted_handoff_failure`]: the completion itself.
/// `false` when the event loop is gone, and then no disk facts follow.
///
/// Split out for the reject paths that may give back the candidate's counted
/// trial launch between the two halves, so the disk facts read the count the
/// sentinel keeps; the automatic lane uses that count to decide whether its next
/// launch is the one the trial reverts on.
#[cfg(unix)]
fn publish_warranted_handoff_failure(
    warrant: HandoffRollbackWarrant,
    cleanup: &HandoffWorkerCleanup,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    completion: crate::UpdateHandoffCompletion,
) -> bool {
    warrant.announce(completion.child_pid);
    cleanup.complete(completion.nonce.as_deref());
    // Reader rollback and reducer re-arm are latency-critical. Publish the
    // candidate-terminated fact before waiting behind the updater FIFO for disk
    // facts.
    proxy
        .send_event(Wake::UpdateHandoffFinished(completion))
        .is_ok()
}

/// The second half of [`send_warranted_handoff_failure`]: the disk as the
/// failed attempt left it, collected behind the updater FIFO and posted as the
/// reducer's next observation.
#[cfg(unix)]
fn send_post_failure_reconcile_facts(
    cleanup: &HandoffWorkerCleanup,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    current_build: u64,
) {
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
        // Log the evidence: the completion detail is short, so the log records
        // which `ChildDied` case this was, and when it is `Unobserved`, that the
        // cause is unknown.
        aterm_log::warn!(
            "update apply: candidate {} died before proving adoption; observed {child_death:?} \
             (this decides whether the automatic lane retries these bytes or converges on them)",
            candidate.pid
        );
    }
    let published = publish_warranted_handoff_failure(
        warrant,
        &job.cleanup,
        proxy,
        crate::UpdateHandoffCompletion::failure(
            job.attempt_id,
            Some(nonce.to_string()),
            Some(candidate.pid),
            outcome,
            detail,
        )
        .with_child_death(child_death),
    );
    // Give the counted trial launch back unless the bytes answer for this death
    // (`forgives_the_counted_trial_launch`). After the wake, because the rollback
    // is latency-critical and this ledger edit is not; before the disk facts,
    // because they carry the sentinel's count.
    if forgives_the_counted_trial_launch(child_death, job.current_build, job.target_build) {
        forgive_candidate_trial_launch(job, candidate);
    }
    if published {
        send_post_failure_reconcile_facts(&job.cleanup, proxy, job.current_build);
    }
    true
}

/// Gives back the boot-trial launch this candidate counted, by name: its pid and
/// birth stamp ([`aterm_update::forgive_trial_launch_of`]). Several windows share
/// the trial's count, so only the name says whose launch moved it. A launch never
/// counted or already given back is not given back again.
#[cfg(unix)]
fn forgive_candidate_trial_launch(job: &HandoffWorkerJob, candidate: HandoffCandidate) {
    let started = candidate
        .birth
        .map(|birth| (birth.seconds, birth.microseconds));
    let _ = aterm_update::forgive_trial_launch_of(job.target_build, candidate.pid, started);
}

/// Kills the candidate, proves it terminated, and says what killed it: the reject
/// path's evidence gathering, in one place so a test can drive it against a real
/// process.
///
/// The order is the protocol:
///
///  1. Before any signal, ask what is known: [`probe_handoff_candidate`]'s
///     `waitid(WNOWAIT)` on the fork lane, the [`CandidateExitWatch`] on the
///     launched lane. Anything here is the candidate's own death.
///  2. The kill and the termination proof, which license the readers to resume.
///  3. Once it is provably gone, ask again: the status `wait` collected, or the
///     kqueue knote (guaranteed queued once termination is proven).
///     [`handoff_child_death`] decides which parts of that answer are attributable.
///
/// Only [`crate::UpdateHandoffOutcome::ChildDied`] has the candidate's own death
/// to describe; for every other outcome the status is our SIGKILL.
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

/// Whether the reject path gives back the boot-trial launch this candidate
/// counted. The launch stays counted only when the new bytes answer for the death;
/// every other answer, including "unknown", gives it back.
///
/// One predicate keeps the retry budget and the boot sentinel from drifting apart:
/// otherwise retries of a healthy candidate would stay counted until
/// `check_boot_health` reverts the bundle and marks the build failed. It decides on
/// the evidence's shape, not the outcome: a starved candidate produces proof EOF
/// as readily as a faulting one. A candidate this process ended carries
/// [`crate::ChildDeathEvidence::Unobserved`], so `TimedOut` and `ActivityRevoked`
/// retries are forgiven.
///
/// Forgiving is cheap (`aterm_update::forgive_trial_launch_of` returns only the
/// candidate's own launch, once), and a successor that truly cannot boot still
/// counts its launches on ordinary relaunches. The real-apply guard lives here
/// too, so the whole decision is testable.
#[cfg(unix)]
#[must_use]
fn forgives_the_counted_trial_launch(
    death: crate::ChildDeathEvidence,
    current_build: u64,
    target_build: u64,
) -> bool {
    // Real apply only: the QA seam arms no sentinel, so there is no counted launch.
    target_build > current_build
        && crate::app_native::PhysicalFailureShape::of_child_death(death)
            != crate::app_native::PhysicalFailureShape::Structural
}

/// Turns one `wait(2)` status into evidence, or refuses to.
///
/// This process only ever sends `SIGKILL` (`signal_handoff_candidate`), so:
///
///   * An exit code cannot be ours: the candidate reached `exit`, which a starved
///     process never does. The commonest refusal (`main_entry` returning without a
///     window, exit code `0`) is read unconditionally.
///   * A signal other than `SIGKILL` (`SIGSEGV`, `SIGBUS`, `SIGABRT`, ...) cannot
///     be ours either.
///   * A bare `SIGKILL` is ambiguous. With `died_before_we_signalled` it is the
///     machine's (macOS jetsam); without it nothing is claimed.
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
    send_preparation_cancelled(job, proxy, nonce);
    true
}

/// A cancel during PREPARATION: it precedes the spawn, so there is no
/// candidate to prove anything about (warrant `NoCandidate`).
#[cfg(unix)]
fn send_preparation_cancelled(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    nonce: Option<String>,
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
            // Typed activity classification: a cancel poke during preparation
            // is user/structural activity, never evidence against the staged
            // artifact — automatic mode may re-attempt at a later quiet window.
            crate::UpdateHandoffOutcome::ActivityRevoked,
            "activity revoked handoff during physical preparation",
        ),
    );
}

/// This process could not prepare the handoff (a write, a descriptor, a `spawn`,
/// the event loop or attempt record gone, no private directory). Raised before any
/// candidate held a master (warrant `NoCandidate`) and filed `ProducerFailed`
/// (Transient), since none of it is about the candidate's bytes.
/// [`send_handoff_preparation_failure`] is for the pre-park verification failure.
#[cfg(unix)]
fn send_handoff_producer_failure(
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
            crate::UpdateHandoffOutcome::ProducerFailed,
            detail,
        ),
    );
}

/// The staged candidate failed its pre-park verification: a verdict about the
/// bytes, filed `PreparationFailed` (Structural) unless the verifier reached no
/// verdict ([`pre_park_refusal_outcome`]). Raised before `spawn`.
#[cfg(unix)]
fn send_handoff_preparation_failure(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    nonce: Option<String>,
    outcome: crate::UpdateHandoffOutcome,
    detail: impl Into<String>,
) {
    send_warranted_handoff_failure(
        HandoffRollbackWarrant::NoCandidate,
        &job.cleanup,
        proxy,
        job.current_build,
        crate::UpdateHandoffCompletion::failure(job.attempt_id, nonce, None, outcome, detail),
    );
}

/// The outcome a refused pre-park verification is filed under, read off the
/// verifier's own words.
///
/// `PreparationFailed` (Structural) is a verdict on the candidate: codesign
/// refused, the sealed identity does not rebind, the installed bundle cannot be
/// the rollback source. A refusal without a verdict (apply budget exhausted, apply
/// lock held past its wait, a helper that would not start;
/// [`aterm_update::is_passing_refusal`]) is `ProducerFailed` (Transient) — except
/// the stage's wait for an update check to confirm its signer
/// ([`aterm_update::is_roster_confirmation_wait`]), which no attempt can end and
/// is `StageUnconfirmed`, no failure at all (round eight, #503's review).
#[cfg(unix)]
#[must_use]
pub(crate) fn pre_park_refusal_outcome(error: &str) -> crate::UpdateHandoffOutcome {
    if aterm_update::is_roster_confirmation_wait(error) {
        crate::UpdateHandoffOutcome::StageUnconfirmed
    } else if aterm_update::is_passing_refusal(error) {
        crate::UpdateHandoffOutcome::ProducerFailed
    } else {
        crate::UpdateHandoffOutcome::PreparationFailed
    }
}

/// The operator-apply-floor refusal a worker owes when a fresh cached pass let
/// it skip the full pre-verification (round six of the update audit, item 26,
/// review round two). The full check reads the floor itself; the cached pass
/// was taken at arm time and nothing drops it when a later check raises the
/// floor, so without this a yank inside the freshness window parked every
/// reader for a successor whose gate 4b then retired the stage. The same for the
/// stage itself (round eight, #36): a check that observes a revocation retires
/// it inside the freshness window, so a staged candidate is asked under the
/// commit the pass was cached for whether it is still the published stage; an
/// activation has no stage and is asked the floor alone. `None` when the
/// worker verifies anyway, and for the same-image QA seam (no artifact, so no
/// publisher and nothing to yank). `floor` is
/// [`aterm_update::handoff_cached_pass_refusal`], injected for the tests.
#[cfg(unix)]
fn skipped_verification_floor_refusal(
    job: &HandoffWorkerJob,
    floor: impl FnOnce(u64, Option<&str>) -> Option<String>,
) -> Option<String> {
    if job.verify_staged_candidate || job.preverified.is_none() {
        return None;
    }
    let staged_commit = (!job.installed_activation).then_some(job.target_commit.as_str());
    floor(job.target_build, staged_commit)
}

/// Why [`worker_pre_park_check`] refused the attempt, and so which completion
/// the worker sends: the verifier's (or the floor's) refusal, filed by
/// [`pre_park_refusal_outcome`], or a capture the candidate's policy forbids,
/// this process's own (`ProducerFailed`).
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum PreParkRefusal {
    Verification {
        outcome: crate::UpdateHandoffOutcome,
        detail: String,
    },
    Policy(String),
}

/// The staged candidate's full pre-park verification, as the worker runs it:
/// [`aterm_update::preverify_installed_for_handoff`] for an activation, else
/// [`aterm_update::preverify_staged_for_handoff`].
#[cfg(unix)]
fn verify_candidate_for_handoff(
    job: &HandoffWorkerJob,
) -> Result<aterm_update::HandoffCandidateFacts, String> {
    if job.installed_activation {
        // Activation: the artifact is the bundle under the running executable.
        // Prove it is the authorized sealed identity, codesign-valid and newer than
        // this process, before any reader is parked.
        aterm_update::preverify_installed_for_handoff(
            job.current_build,
            job.target_build,
            &job.target_commit,
        )
    } else {
        aterm_update::preverify_staged_for_handoff(
            job.current_build,
            Some(crate::build_info::git_commit()),
            Some(job.target_build),
            Some(&job.target_commit),
        )
    }
}

/// THE WORKER'S FIRST REAL ACTION (seamless seam 1), with its two readers
/// injected so a test drives the decision the worker ships (round seven, item
/// 106): `verify` is [`verify_candidate_for_handoff`], `floor`
/// [`aterm_update::handoff_cached_pass_refusal`].
///
/// The `codesign --deep` + bundle-flock check runs here, off the UI thread, so a
/// doomed candidate is refused before any manifest is written or child spawned.
/// Additive: the child re-runs the complete gate under the apply lock at swap
/// time. Refusals are filed by [`pre_park_refusal_outcome`]; a worker that skips
/// the check on a fresh cached pass still asks the floor
/// ([`skipped_verification_floor_refusal`]).
#[cfg(unix)]
fn worker_pre_park_check(
    job: &mut HandoffWorkerJob,
    verify: impl FnOnce(&HandoffWorkerJob) -> Result<aterm_update::HandoffCandidateFacts, String>,
    floor: impl FnOnce(u64, Option<&str>) -> Option<String>,
) -> Result<(), PreParkRefusal> {
    let which = if job.installed_activation {
        "installed bundle"
    } else {
        "staged update"
    };
    let refused = |error: String| PreParkRefusal::Verification {
        outcome: pre_park_refusal_outcome(&error),
        detail: format!("{which} failed pre-park verification: {error}"),
    };
    if job.verify_staged_candidate {
        let facts = match verify(job) {
            Ok(facts) => facts,
            Err(error) => {
                // The verdict is the arm-time read's word too, and is kept
                // where that read keeps it ([`PreverifyPublisher::publish_refusal`]).
                if let Some(publisher) = job.preverified.as_ref() {
                    publisher
                        .publish_refusal(&format!("{which} failed pre-park verification: {error}"));
                }
                return Err(refused(error));
            }
        };
        // The candidate passed, so the policy it carries was read (plan P0-5).
        // Published before the launch: the launched lane's park reads it on the
        // main thread once the successor has dialled.
        job.successor_grant_chunks = facts.grant_chunks;
        let adopted = job.preverified.as_ref().and_then(|publisher| {
            publisher.publish_pass(
                &facts,
                job.current_build,
                &format!("{which} build {}", job.target_build),
            )
        });
        if let Some(refusal) = capture_exceeds_policy(job.parked_under, adopted) {
            return Err(PreParkRefusal::Policy(refusal));
        }
    } else if let Some(error) = skipped_verification_floor_refusal(job, floor) {
        // A FRESH CACHED PASS LET THIS WORKER SKIP THE FULL CHECK, and the floor
        // and the stage are the facts the cache cannot vouch for: the floor
        // ratchets on observation and may have risen since the pass was cached
        // (round six, item 26, review round two), and a check that observes a
        // revocation retires the stage (round eight, #36). The same refusal, and
        // the same outcome, the full check gives.
        return Err(refused(error));
    }
    Ok(())
}

/// THE COLD LANE'S TABLE AT ITS `exec`, which replaces this process: every
/// descriptor above stdio marked close-on-exec, then the harness's lifeline made
/// inheritable — the guard, dropped when the `exec` returns, flags it again.
///
/// The successor is this app, living on as the window, and an `exec` keeps
/// every descriptor without close-on-exec: Metal's shader-cache files, a fabric
/// bridge lane caught between its `socketpair` and its `F_SETFD` (2026-09-30
/// review). Marking the LIVE table is safe here: every descriptor this process
/// means to hand a child is close-on-exec until the child's own hook (or this
/// function) clears it, so a failed `exec` carries on with nothing lost.
#[cfg(unix)]
fn cold_exec_hand_over() -> Option<crate::lifeline::InheritedAcrossExec> {
    aterm_uds::spawnfd::InheritStrip::prepare().apply();
    crate::lifeline::inherit_across_exec()
}

/// THE FORK LANE'S CHILD, between its fork and its `exec`: its own process
/// group, then a table holding what it is `handed` and NOTHING ELSE above stdio.
///
/// * The group: the candidate and every helper it later launches inherit a
///   dedicated process group; adopted shell PTYs/process groups are unrelated.
///   STRICTLY STRONGER than the successor-side `contain_own_process_group`
///   that covers the launch shape with no pre-exec hook, and the reason this
///   stays here rather than being replaced by it: this runs between fork and
///   exec, so the group exists before the candidate's image does — no ordering
///   argument about "the first thing that can fork a helper" is needed, and the
///   failure aborts the spawn instead of having to be handled by a process that
///   is already running. Keep both.
/// * The strip (2026-09-30 review): the fork copied every descriptor this
///   process holds, and the ones without close-on-exec — Metal's shader-cache
///   files, a fabric bridge lane caught between its `socketpair` and its
///   `F_SETFD` — rode the `exec` into the successor, which lives on as the
///   window. So the whole table is marked close-on-exec first
///   ([`aterm_uds::spawnfd::InheritStrip`]), and only then is the flag cleared
///   on exactly the `handed` descriptors (parent descriptors remain CLOEXEC for
///   the whole asynchronous interval; only the child's copies change).
///
/// # Safety
/// Only in a forked child, before its `exec`: it makes only async-signal-safe
/// calls (`setpgid`, `fcntl`, `InheritStrip::apply`) over plain integers.
#[cfg(unix)]
unsafe fn fork_lane_child_holds(
    strip: aterm_uds::spawnfd::InheritStrip,
    handed: &[std::os::fd::RawFd],
) -> std::io::Result<()> {
    // SAFETY: the function's contract — a forked child before its `exec`.
    unsafe {
        if libc::setpgid(0, 0) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        strip.apply();
        for fd in handed {
            let flags = libc::fcntl(*fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
// The worker's muts serve the macOS handoff arms; on other platforms those arms
// are configured out and the bindings are read-only.
fn run_handoff_worker(mut job: HandoffWorkerJob, proxy: winit::event_loop::EventLoopProxy<Wake>) {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::CommandExt as _;

    if handoff_preparation_cancelled(&job, &proxy, None) {
        return;
    }

    // Staged-candidate pre-verification ([`worker_pre_park_check`]): a refusal
    // ends the attempt before any manifest is written or child spawned.
    if let Err(refusal) = worker_pre_park_check(
        &mut job,
        verify_candidate_for_handoff,
        aterm_update::handoff_cached_pass_refusal,
    ) {
        match refusal {
            PreParkRefusal::Verification { outcome, detail } => {
                send_handoff_preparation_failure(&job, &proxy, None, outcome, detail);
            }
            PreParkRefusal::Policy(detail) => {
                send_handoff_producer_failure(&job, &proxy, None, detail);
            }
        }
        return;
    }
    if handoff_preparation_cancelled(&job, &proxy, None) {
        return;
    }

    // The lanes split here, before anything is written. The launched lane has no
    // capture yet: this worker launches the successor, holds its rendezvous claim
    // and asks the main thread to park only once the successor has dialled, so
    // the swap, re-exec and boot check happen while the terminal still echoes. It
    // finishes the attempt itself; only a refusal before anything was launched
    // (no bundle, no rendezvous, LaunchServices) hands it back to be forked, and
    // it rejoins here with the main thread's capture, where the fork lane starts.
    let nonce;
    #[cfg(target_os = "macos")]
    {
        if job.lane == HandoffLane::OutOfBand {
            let lane = job
                .prelaunch
                .take()
                .expect("the out-of-band lane is constructed with its prelaunch channels");
            match run_prelaunched_handoff(&mut job, &proxy, lane) {
                Prelaunched::Finished => return,
                Prelaunched::ForkInstead {
                    nonce: returned_nonce,
                    transfer,
                } => {
                    let HandoffTransferJob {
                        capture,
                        // The post-park deadline is dropped: it bounds the freeze
                        // for a successor that already swapped and boot-checked.
                        // The forked child does all that under the park, which
                        // `handoff_ready_deadline` budgets.
                        proof_deadline: _,
                    } = *transfer;
                    job.capture = Some(capture);
                    nonce = returned_nonce;
                }
            }
        } else {
            nonce = crate::seamless::mint_outgoing_nonce();
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        nonce = crate::seamless::mint_outgoing_nonce();
    }

    // The fork lane parks before it spawns, so nothing was prepared before the
    // park: every artifact is written here, as it always was.
    let PreparedArtifacts {
        manifest_path: path,
        layout_path,
        fds_wire: wire,
        expected,
        proof_rd,
        proof_wr,
        commit_rd,
        commit_wr,
    } = match prepare_outgoing_artifacts(&mut job, &nonce, EarlyArtifacts::default()) {
        Ok(prepared) => prepared,
        Err(failure) => {
            report_preparation_failure(&job, &proxy, nonce, failure);
            return;
        }
    };

    // The proof term travels with the lane: `expected` was computed over the lane
    // choice's terms (PTY device on the launched lane, fd numbers on the fork
    // lane). A launched attempt that falls back to forking keeps the device-term
    // proof, so the term is named explicitly for the successor.
    #[cfg(target_os = "macos")]
    {
        let proof_term = match job.lane {
            HandoffLane::OutOfBand => "device",
            HandoffLane::Fork => "fd",
        };
        job.command
            .env(crate::handoff_rendezvous::ENV_PROOF_TERM, proof_term);
        // THE LAUNCHER'S SOFT DESCRIPTOR LIMIT, when a claim or a park raised
        // this process's own (round seven, item 107): the forked child inherits
        // the raised limit and never claims, so without this its shells would
        // get the raise back instead of the launcher's.
        crate::handoff_rendezvous::carry_launcher_limit(&mut job.command);
    }
    job.command
        .env("ATERM_SEAMLESS_MANIFEST", path)
        .env("ATERM_SEAMLESS_NONCE", &nonce)
        .env("ATERM_SEAMLESS_FDS", wire)
        .env("ATERM_SEAMLESS_LAYOUT", layout_path)
        // The candidate proves it is the authorized build by comparison and logs
        // which half disagreed. Forging this can only lose a handoff; the parent
        // compares against its own ticket.
        .env(
            "ATERM_SEAMLESS_TARGET",
            crate::seamless::encode_target_identity(job.target_build, &job.target_commit),
        )
        .env("ATERM_HANDOFF_READY_FD", proof_wr.as_raw_fd().to_string())
        .env("ATERM_HANDOFF_COMMIT_FD", commit_rd.as_raw_fd().to_string());
    // Parental authority: our pid and its kernel birth record, which lets the
    // successor watch us without being our fork child (`seamless::AttestedParent`,
    // encoded beside its decoder).
    for (key, value) in crate::seamless::outgoing_parent_env() {
        job.command.env(key, value);
    }
    // Parent descriptors remain CLOEXEC for the WHOLE asynchronous interval.
    // Clear only the fork child's copies immediately before its exec image.
    let mut child_inherit = job
        .capture()
        .live
        .iter()
        .map(|(_, master, _)| *master)
        .collect::<Vec<_>>();
    child_inherit.push(proof_wr.as_raw_fd());
    child_inherit.push(commit_rd.as_raw_fd());
    #[cfg(target_os = "macos")]
    let handed = child_inherit.clone();
    // AND NOTHING ELSE (2026-09-30 review): `fork_lane_child_holds` strips the
    // child's table before it hands it these. The strip's bound is read here,
    // before the fork.
    let strip = aterm_uds::spawnfd::InheritStrip::prepare();
    // SAFETY: the closure runs between fork and exec, and
    // `fork_lane_child_holds` makes only async-signal-safe calls (`setpgid`,
    // `fcntl`, `InheritStrip::apply`) on captured integers, not shared memory.
    unsafe {
        job.command
            .pre_exec(move || fork_lane_child_holds(strip, &child_inherit));
    }
    // AN OLDER TARGET STARTS AT THE LIMIT THIS PROCESS HELD BEFORE A PARK'S
    // RAISE (round eight's second review of #3): the raise an earlier attempt
    // made is never lowered here, and a successor built before 06b06e74e
    // would hand its shells whatever it inherits. Registered after the hook
    // above, so the child's last look at its table sees what it is handed.
    #[cfg(target_os = "macos")]
    crate::handoff_rendezvous::start_older_image_at_its_limit(
        &mut job.command,
        job.target_build,
        job.current_build,
        &handed,
    );
    if handoff_preparation_cancelled(&job, &proxy, Some(nonce.clone())) {
        return;
    }
    // The fork lane. `spawn` makes the successor our fork child; on macOS, once
    // `seamless::commit_and_exit` exits, launchd tears down this app instance's
    // application job and the successor keeps a bootstrap (XPC) context of a dead
    // job, so `hdiutil` (ENXIO), notifications and LaunchServices opens fail in it
    // (`tests/handoff_launchd_job.rs`). The `HandoffLane::OutOfBand` lane avoids
    // that: launchd gives the successor its own job, with parent attestation
    // (`seamless::AttestedParent`), reap authority (`HandoffRollbackWarrant`),
    // self-containment (`contain_own_process_group`) and transport
    // (`handoff_rendezvous`).
    //
    // Forking remains the only lane when `out_of_band_lane_refusal` names a
    // reason: no `.app` bundle, a `$HOME` pushing the rendezvous past `sun_path`,
    // more panes than the grant carries, or an older target build. Both lanes
    // rejoin at `run_handoff_decision`. Here `pre_exec` sets the process group
    // before `spawn` returns, so `kill(-pid)` is valid immediately;
    // `signal_handoff_candidate` covers the launched lane's case.
    let child = match job.command.spawn() {
        Ok(child) => child,
        Err(error) => {
            // The candidate passed its pre-park verification; a `spawn` that
            // fails now is this process's (`EAGAIN`, `ENOMEM`, `EMFILE`).
            send_handoff_producer_failure(
                &job,
                &proxy,
                Some(nonce),
                format!("handoff process could not start: {error}"),
            );
            return;
        }
    };
    // Capture the candidate's kernel identity while its pid is pinned as our
    // unreaped child; every later reap authority reads it. The launched lane gets
    // it from the rendezvous accept (`of_attested_peer`).
    let candidate = HandoffCandidate::of_unreaped_child(&child);
    // Tell the PTY keeper the child's pid before any proof or Commit (the fork
    // lane's duplicates came with the fork): a death of this window from here on
    // is a handoff while the child lives. Bounded; the holder scan stands alone if
    // it fails.
    crate::keeper_link::pending_before_grant(candidate.pid);
    // This process answers to the carried ids until the Commit, whatever discovery
    // entry the candidate publishes first (`identity_claim`). Held to the end: a
    // Commit `_exit`s with it held, and every rejection returns after the reap.
    let _transfer = crate::identity_claim::register_handoff_candidate(candidate.pid);
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
        // A forked candidate is our own child: the kernel pinned its identity at
        // the fork, and there is no launch answer to corroborate it with.
        #[cfg(target_os = "macos")]
        None,
    );
}

/// The physical artifacts and private channels one attempt publishes for its
/// successor, produced by [`prepare_outgoing_artifacts`] from the park's capture.
#[cfg(unix)]
struct PreparedArtifacts {
    manifest_path: String,
    layout_path: std::path::PathBuf,
    fds_wire: String,
    expected: crate::seamless::AdoptionProof,
    /// The two private pipes before either end is given away. The successor writes
    /// the proof and reads the Commit; a swapped pair deadlocks like a slow
    /// successor.
    proof_rd: std::os::fd::OwnedFd,
    proof_wr: std::os::fd::OwnedFd,
    commit_rd: std::os::fd::OwnedFd,
    commit_wr: std::os::fd::OwnedFd,
}

/// The two private pipes of one attempt, not yet given away: the successor
/// writes the proof and reads the Commit. Stateless, so the launched lane makes
/// them before the park; an attempt ending before its grant drops them.
#[cfg(unix)]
struct AttemptPipes {
    proof_rd: std::os::fd::OwnedFd,
    proof_wr: std::os::fd::OwnedFd,
    commit_rd: std::os::fd::OwnedFd,
    commit_wr: std::os::fd::OwnedFd,
}

#[cfg(unix)]
impl AttemptPipes {
    /// Both pipes, close-on-exec, or the refusal the park reports.
    fn make() -> Result<Self, String> {
        let Some((proof_rd, proof_wr)) = make_cloexec_pipe() else {
            let kind = std::io::Error::last_os_error().kind();
            return Err(format!(
                "could not create the adoption-proof channel ({kind})"
            ));
        };
        let Some((commit_rd, commit_wr)) = make_cloexec_pipe() else {
            let kind = std::io::Error::last_os_error().kind();
            return Err(format!(
                "could not create the handoff-commit channel ({kind})"
            ));
        };
        Ok(Self {
            proof_rd,
            proof_wr,
            commit_rd,
            commit_wr,
        })
    }
}

/// What the launched lane prepared before the park (`handoff_diet`): the
/// attempt's pipes and layout sidecar, made while the successor boots and every
/// reader is live. Empty on the fork lane and wherever a step refused; the park
/// then makes them itself.
#[cfg(unix)]
#[derive(Default)]
struct EarlyArtifacts {
    pipes: Option<AttemptPipes>,
    layout: Option<crate::handoff_diet::SpeculativeLayout>,
}

/// Why [`prepare_outgoing_artifacts`] stopped. Typed so each caller attaches the
/// warrant it holds: the fork lane has no candidate, while the launched lane holds
/// a dialled successor that must be stood down and proven gone first.
#[cfg(unix)]
enum PreparationFailure {
    /// The main thread poked the cancel channel: structural activity revoked
    /// the attempt (typed `ActivityRevoked` for the retry budget).
    Cancelled,
    /// This process's own preparation refused (a write, a pipe, its own carry
    /// disagreeing with itself); the detail is the status-bar sentence, with the
    /// `std::io::ErrorKind` when there is one. Filed `ProducerFailed` (Transient),
    /// never `PreparationFailed`, since it is not about the candidate.
    Producer(String),
    /// The desk's own content refused the write deterministically: the manifest is
    /// over its cap with every optional part shed
    /// ([`crate::seamless::WriteOutgoingFailure::ManifestOverCap`]). Filed
    /// `CaptureRefused`, retried once the desk changes, not `ProducerFailed`, whose
    /// retries would freeze every reader again for the same answer.
    Refused(String),
}

/// What a refused manifest write is, for the retry budget: the one refusal
/// that is about the desk's content ([`PreparationFailure::Refused`]), and
/// this process's own trouble for every other.
#[cfg(unix)]
fn preparation_failure_of_write(
    failure: crate::seamless::WriteOutgoingFailure,
) -> PreparationFailure {
    let detail = format!("could not write the authenticated handoff manifest ({failure})");
    match failure {
        // No retry clause: a person's press is answered `Failed` with this detail,
        // and the automatic lane's surfaces state their own retry.
        crate::seamless::WriteOutgoingFailure::ManifestOverCap => {
            PreparationFailure::Refused(detail)
        }
        crate::seamless::WriteOutgoingFailure::NoPrivateDir
        | crate::seamless::WriteOutgoingFailure::Inconsistent
        | crate::seamless::WriteOutgoingFailure::Io(_) => PreparationFailure::Producer(detail),
    }
}

/// Every artifact one attempt publishes, in the order the successor consumes
/// them: the layout round-trip, the control carry's export, the authenticated
/// manifest and its grid sidecars under `nonce`, the layout sidecar, the adoption
/// proof and the two private pipes. Shared by both lanes so the successor's bytes
/// cannot depend on how it was started.
///
/// Runs after the park on both lanes; nothing here touches a master. `early` is
/// what the launched lane prepared before the park: its pipes are used as they
/// are, and its layout sidecar is kept only when it reads back byte-equal to the
/// park's layout (`handoff_diet`). The manifest, screen sidecars and adoption
/// proof are written here on both lanes.
#[cfg(unix)]
fn prepare_outgoing_artifacts(
    job: &mut HandoffWorkerJob,
    nonce: &str,
    early: EarlyArtifacts,
) -> Result<PreparedArtifacts, PreparationFailure> {
    let started = std::time::Instant::now();
    let target_build = job.target_build;
    let target_commit = job.target_commit.clone();
    let capture = job
        .capture
        .as_mut()
        .expect("the artifacts are prepared from the park's capture");
    // The park's layout, serialized once: the round trip, the digest check and the
    // sidecar all use this wire.
    let Some(layout_wire) = capture.layout.to_toml().ok().filter(|wire| {
        !capture.layout.is_empty()
            && crate::restore::RestoreManifest::from_toml(wire).as_ref() == Some(&capture.layout)
    }) else {
        return Err(PreparationFailure::Producer(
            "could not persist the bounded handoff layout".to_string(),
        ));
    };
    // The main thread committed `layout_digest` over its own serialization and the
    // successor hashes the sidecar's bytes; a difference is a preparation failure
    // here rather than an unexplained `AdoptionMismatch` later.
    if crate::seamless::layout_wire_digest(&layout_wire) != Some(capture.layout_digest) {
        return Err(PreparationFailure::Producer(
            "handoff layout carry did not match the committed layout digest".to_string(),
        ));
    }
    if job.cancel.try_recv().is_ok() {
        return Err(PreparationFailure::Cancelled);
    }
    // THE CONTROL CARRY'S EXPORT (round 10), HERE on the worker and not in the
    // freeze: every reader is still parked (until Commit or rollback), so no
    // engine moves, and cloning up to 1 MiB of archive rows per session costs
    // the frozen terminal nothing. What the freeze took is the fence; a
    // session whose archive moved since, or whose lock another thread keeps,
    // carries its counters alone. The turn-id count rides the manifest itself,
    // so a ledger that could not be carried cannot lower it — with a reserve
    // for the turns this process still serves until its `_exit`, so the
    // successor never mints an id this one already replied with (round
    // seven, finding 22; the Commit checks the reserve held). The export
    // takes the same ceiling: a turn open here can record under any id up
    // to it, which no carried ledger may vouch for (round eight, finding 40).
    // Each record is spelled in the words the target reads: a build from
    // before `hangup` leaves out a record whose status it does not know
    // (round eight, finding 714).
    let turn_id_ceiling = crate::control::turn_ids_for_handoff();
    let controls = crate::handoff_carry::export_for(
        &capture.carries,
        turn_id_ceiling,
        crate::handoff_carry::StatusWords::for_target(target_build),
    );
    capture.manifest.next_turn_id = crate::handoff_carry::manifest_turn_id(turn_id_ceiling);
    // The ceiling rides this attempt, which the Commit reads it from.
    job.arbiter.set_turn_id_ceiling(turn_id_ceiling);
    // The history carry's join (`crate::handoff_history`): a session whose export
    // still describes its history and meets its checkpoint's lines names its
    // sidecar on its record; the others keep the checkpoint's bounded history and
    // count the lines that do not cross. Cannot fail the attempt. Runs after the
    // control carry's export so the fork lane's history export does not contend
    // for the same locks.
    // The held panes' exports go to their own carry (round eight, finding 39),
    // joined where their screens are written, with why a pane with none has
    // none: a successor older than this build takes a held pane's screen alone,
    // a policy that carries no scrollback withheld it, and anything else is a
    // carry that failed — each said as what it was.
    let mut held_history = crate::handoff_history::HeldExports::default();
    if let Some(dir) = crate::control_auth::socket_dir() {
        let mut results = std::mem::replace(
            &mut capture.history,
            crate::handoff_history::HistoryPlan::Unexported,
        )
        .results(&dir);
        held_history = results.take_held(
            &capture
                .held
                .iter()
                .map(|pane| pane.local_id)
                .collect::<Vec<_>>(),
            !crate::handoff_history::exports_for_target(
                crate::running_build_number(),
                target_build,
            ),
        );
        let verdicts = crate::handoff_history::stamp_manifest(
            &mut capture.manifest,
            &capture.screens,
            &capture.history_heads,
            results,
            &dir,
            nonce,
        );
        crate::handoff_history::log_verdicts(&verdicts);
    }
    let carried_at = std::time::Instant::now();
    // The caller minted the attempt nonce: the launched lane names the manifest's
    // path to the successor before the manifest exists, and `outgoing.nonce`
    // echoes that value.
    let outgoing = crate::seamless::write_outgoing_with_held(
        &capture.manifest,
        &capture.fds,
        &capture.screens,
        &capture.repaint,
        capture.window.clone(),
        &controls,
        &capture.held,
        held_history,
        nonce,
    )
    .map_err(preparation_failure_of_write)?;
    let crate::seamless::OutgoingHandoff {
        manifest_path: path,
        nonce: echoed_nonce,
        fds_wire,
        screen_digest: carried_screen_digest,
        connections_left_out,
    } = outgoing;
    // What the manifest left out rides this attempt to its Commit, which audits
    // it (round eight of the update audit, U23's review): an attempt that stands
    // down dissolves none of it.
    job.arbiter.set_connections_left_out(connections_left_out);
    // The launch environment named this path from the same nonce before the
    // writer ran, so they must agree.
    debug_assert_eq!(
        crate::seamless::outgoing_manifest_path(nonce).as_deref(),
        Some(std::path::Path::new(&path)),
        "the manifest writer and the launch environment name the manifest differently"
    );
    debug_assert_eq!(echoed_nonce, nonce, "the writer echoes the minted nonce");
    // `capture.screen_digest` was committed on the main thread from the live
    // checkpoints and `carried_screen_digest` over the bytes just written, which
    // the child hashes. A divergence is an explained preparation failure here
    // rather than an `AdoptionMismatch` later.
    if carried_screen_digest != capture.screen_digest {
        return Err(PreparationFailure::Producer(
            "handoff screen carry did not match the committed screen digest".to_string(),
        ));
    }
    let manifest_at = std::time::Instant::now();
    if job.cancel.try_recv().is_ok() {
        return Err(PreparationFailure::Cancelled);
    }
    // The layout sidecar, sealed: one written before the park is kept only when it
    // reads back byte-equal to `layout_wire`, otherwise rewritten
    // (`handoff_diet::seal_layout`). The successor reads it only after the grant.
    let EarlyArtifacts {
        pipes: early_pipes,
        layout: speculative_layout,
    } = early;
    let layout_path = std::path::Path::new(&path).with_extension("layout.toml");
    let layout_seal =
        match crate::handoff_diet::seal_layout(&layout_path, &layout_wire, speculative_layout) {
            Ok(seal) => seal,
            Err(error) => {
                return Err(PreparationFailure::Producer(format!(
                    "could not write the attempt-bound handoff layout ({error})"
                )));
            }
        };
    let layout_at = std::time::Instant::now();
    aterm_log::info!("update apply: layout sidecar — {layout_seal}");
    if job.cancel.try_recv().is_ok() {
        return Err(PreparationFailure::Cancelled);
    }
    // The refusal names which input the proof could not carry. A commit it cannot
    // name is refused before the launch (`App::start_unix_update_handoff`), so
    // this is reached only if that gate is bypassed.
    let expected = crate::seamless::adoption_proof(
        nonce,
        target_build,
        &target_commit,
        &capture.layout_digest,
        &capture.screen_digest,
        // Not `capture.live`: the fork lane uses descriptor numbers, the
        // out-of-band lane the PTY device.
        &capture.proof_identities,
    )
    .map_err(|refusal| PreparationFailure::Producer(refusal.to_string()))?;
    // The pipes: made before the park on the launched lane, here otherwise.
    let pipes_made_early = early_pipes.is_some();
    let AttemptPipes {
        proof_rd,
        proof_wr,
        commit_rd,
        commit_wr,
    } = match early_pipes {
        Some(pipes) => pipes,
        None => AttemptPipes::make().map_err(PreparationFailure::Producer)?,
    };
    if job.cancel.try_recv().is_ok() {
        return Err(PreparationFailure::Cancelled);
    }
    // The split of this interval, beside the park->transfer split, so the
    // artifacts slice can be read off one line. Observation only.
    let ms = |from: std::time::Instant, to: std::time::Instant| {
        to.saturating_duration_since(from).as_secs_f64() * 1000.0
    };
    aterm_log::info!(
        "update apply: artifacts split — carry export {:.1} + manifest {:.1} + layout {:.1} + \
         proof and pipes {:.1} ms (pipes made {})",
        ms(started, carried_at),
        ms(carried_at, manifest_at),
        ms(manifest_at, layout_at),
        ms(layout_at, std::time::Instant::now()),
        if pipes_made_early {
            "before the park"
        } else {
            "at the park"
        }
    );
    Ok(PreparedArtifacts {
        manifest_path: path,
        layout_path,
        fds_wire,
        expected,
        proof_rd,
        proof_wr,
        commit_rd,
        commit_wr,
    })
}

/// The fork lane's report of a [`PreparationFailure`]: no candidate exists, so
/// the warrant is `NoCandidate`; a cancel is `ActivityRevoked` and this process's
/// own refusal `ProducerFailed`.
#[cfg(unix)]
fn report_preparation_failure(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    nonce: String,
    failure: PreparationFailure,
) {
    match failure {
        PreparationFailure::Cancelled => send_preparation_cancelled(job, proxy, Some(nonce)),
        PreparationFailure::Producer(detail) => {
            send_handoff_producer_failure(job, proxy, Some(nonce), detail);
        }
        PreparationFailure::Refused(detail) => send_warranted_handoff_failure(
            HandoffRollbackWarrant::NoCandidate,
            &job.cleanup,
            proxy,
            job.current_build,
            crate::UpdateHandoffCompletion::failure(
                job.attempt_id,
                Some(nonce),
                None,
                crate::UpdateHandoffOutcome::CaptureRefused,
                detail,
            ),
        ),
    }
}

/// How the launched lane's prelaunch phase ended.
#[cfg(target_os = "macos")]
enum Prelaunched {
    /// The attempt is finished either way — committed (this process is gone),
    /// rolled back, or reported — and the worker has nothing left to do.
    Finished,
    /// A runtime refusal before anything was launched. The main thread has parked
    /// and sent its capture, and the caller forks with it under the pre-minted
    /// nonce: the fork lane's orphaned launchd domain beats not updating on a
    /// machine LaunchServices refuses.
    ForkInstead {
        nonce: String,
        transfer: Box<HandoffTransferJob>,
    },
}

/// A successor that has dialled and is being held: the rendezvous, the accepted
/// claim, its kernel-attested identity and its exit witness. It holds no
/// descriptor (the `sendmsg` grant comes after the park), so standing it down is
/// closing the rendezvous and proving it gone.
#[cfg(target_os = "macos")]
struct HeldSuccessor {
    rendezvous: crate::handoff_rendezvous::Rendezvous,
    peer: crate::handoff_rendezvous::ClaimedPeer,
    candidate: HandoffCandidate,
    exit_watch: Option<CandidateExitWatch>,
    dialled_at: std::time::Instant,
}

/// One step of a pre-grant stand-down, recorded as taken so the order can be
/// projected onto the derived overlap model.
///
/// The model's ungranted lane is `UnparkForRepark?` -> `RevokeUngrantedSuccessor`
/// -> (`SuccessorExitsOnRevoke` | `KillUngrantedSuccessor`) ->
/// `ReapUngrantedSuccessor` -> `RetireUngrantedAttempt`;
/// `UngrantedRetireRequiresReap` forbids clearing an attempt whose successor is
/// alive. Tests replay what the code emitted.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StandDownStep {
    /// The rendezvous closed: EOF for the successor, which is the revocation.
    Revoke,
    /// It was gone inside the grace with no signal of ours: it exited on that
    /// EOF by its own rule, or it had died before it.
    SuccessorExited,
    /// It did not, so it was signalled.
    Kill,
    /// Its termination was proven (a vacant or recycled pid).
    Reap,
}

#[cfg(all(test, target_os = "macos"))]
impl StandDownStep {
    /// The derived model action this step is.
    #[must_use]
    fn model_action(self) -> &'static str {
        match self {
            Self::Revoke => "RevokeUngrantedSuccessor",
            Self::SuccessorExited => "SuccessorExitsOnRevoke",
            Self::Kill => "KillUngrantedSuccessor",
            Self::Reap => "ReapUngrantedSuccessor",
        }
    }
}

/// What [`stand_down_held_successor`] proved.
#[cfg(target_os = "macos")]
struct StoodDownSuccessor {
    warrant: HandoffRollbackWarrant,
    death: crate::ChildDeathEvidence,
    /// `true` when the successor ignored the closed rendezvous for the whole
    /// grace and had to be killed.
    signalled: bool,
    /// The steps this stand-down actually took, in order.
    steps: Vec<StandDownStep>,
}

#[cfg(target_os = "macos")]
impl StoodDownSuccessor {
    /// Whether this process gives back the boot-trial launch the stood-down
    /// successor counted, decided by the death's evidence through
    /// [`forgives_the_counted_trial_launch`], as after the grant and before the
    /// dial ([`send_pre_dial_death`]).
    ///
    /// Not whether we signalled it: a held successor the machine SIGKILLed never
    /// ran its own EOF-rule exit, so nobody else gives its launch back. Exactly one
    /// party gives it back: the EOF-rule exit is witnessed as `Exited { code: 0 }`
    /// (`Structural`), so this answers `false`; where the witness cannot tell (lost,
    /// or our kill racing that exit) it answers `true`, and a second give-back is a
    /// no-op ([`aterm_update::forgive_trial_launch_of`]).
    #[must_use]
    fn forgives_the_counted_trial_launch(&self, current_build: u64, target_build: u64) -> bool {
        forgives_the_counted_trial_launch(self.death, current_build, target_build)
    }
}

/// How long a held successor gets to exit by its own rule after its rendezvous
/// closes, before it is killed. The successor forgives its trial launch and
/// exits in milliseconds (`main_entry`); the grace covers a loaded machine.
/// Readers are live throughout, so it costs the user nothing.
#[cfg(target_os = "macos")]
const STAND_DOWN_SELF_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// How long the worker waits for the main thread to answer a stand-down before
/// revoking anyway: generous for a busy event loop, far shorter than the grace,
/// so a wedged main thread costs a late reader resume, not a live candidate.
#[cfg(target_os = "macos")]
const STAND_DOWN_ACK_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);

/// How long the worker holds a dialled successor for a park that never comes:
/// the backstop behind the main thread's `PRELAUNCH_HOLD_MAX`, and shorter than
/// the successor's `GRANT_HOLD_BUDGET` (`main_entry`) so the parent stands it
/// down by EOF first.
#[cfg(target_os = "macos")]
const PRELAUNCH_HOLD_BOUND: std::time::Duration = std::time::Duration::from_secs(4 * 60);

/// Stands a held successor down and proves it gone: close the rendezvous (EOF),
/// wait the grace for its own exit, then kill and prove termination.
///
/// The order is the model's: `RevokeUngrantedSuccessor` precedes
/// `KillUngrantedSuccessor` precedes `ReapUngrantedSuccessor`, and only a reaped
/// attempt may retire (`UngrantedRetireRequiresReap`).
#[cfg(target_os = "macos")]
fn stand_down_held_successor(
    held: HeldSuccessor,
    grace: std::time::Duration,
) -> StoodDownSuccessor {
    stand_down_held_successor_between(held, grace, &mut || {})
}

/// [`stand_down_held_successor`] with its one sensitive instant injectable:
/// `between_read_and_probe` runs after a witness read that found nothing and
/// before the pid probe. Production passes a no-op; a test passes the candidate's
/// exit and reap to pin the evidence rule deterministically.
#[cfg(target_os = "macos")]
fn stand_down_held_successor_between(
    held: HeldSuccessor,
    grace: std::time::Duration,
    between_read_and_probe: &mut dyn FnMut(),
) -> StoodDownSuccessor {
    const PROBE: std::time::Duration = std::time::Duration::from_millis(2);
    let HeldSuccessor {
        rendezvous,
        peer,
        candidate,
        exit_watch,
        ..
    } = held;
    // EOF is the stand-down: the successor's `recv_with_fds` gets zero bytes;
    // dropping the rendezvous unlinks the node, so a re-dial fails closed.
    drop(peer);
    drop(rendezvous);
    let mut steps = vec![StandDownStep::Revoke];
    let grace_deadline = std::time::Instant::now() + grace;
    let mut witnessed = None;
    let mut exited_on_its_own = false;
    loop {
        if let Some(status) = exit_watch
            .as_ref()
            .and_then(CandidateExitWatch::exit_status)
        {
            witnessed = Some(status);
            exited_on_its_own = true;
            break;
        }
        between_read_and_probe();
        if handoff_candidate_terminated(candidate) {
            exited_on_its_own = true;
            break;
        }
        if std::time::Instant::now() >= grace_deadline {
            break;
        }
        std::thread::sleep(PROBE);
    }
    if exited_on_its_own {
        // Gone before any signal of ours: whatever the witness recorded is the
        // candidate's own death, which is why `died_before_we_signalled` is true.
        steps.push(StandDownStep::SuccessorExited);
        let warrant = wait_for_handoff_candidate_to_terminate(candidate);
        steps.push(StandDownStep::Reap);
        // After the proof, ask again: a candidate that exits and is reaped between
        // the witness read and the pid probe leaves the loop with the witness
        // unread. XNU queues the knote in `proc_exit` before the pid can fall
        // vacant, and `kill(pid, 0)` answers a zombie as alive, so this read finds
        // it. `the_exit_between_the_witness_read_and_the_pid_probe_is_still_witnessed`
        // forces the window.
        let witnessed = witnessed.or_else(|| {
            exit_watch
                .as_ref()
                .and_then(CandidateExitWatch::exit_status)
        });
        return StoodDownSuccessor {
            warrant,
            death: handoff_child_death(true, witnessed),
            signalled: false,
            steps,
        };
    }
    signal_handoff_candidate(candidate);
    steps.push(StandDownStep::Kill);
    let warrant = wait_for_handoff_candidate_to_terminate(candidate);
    steps.push(StandDownStep::Reap);
    let status = exit_watch
        .as_ref()
        .and_then(CandidateExitWatch::exit_status);
    StoodDownSuccessor {
        warrant,
        // Our own SIGKILL is not evidence about the bytes; `handoff_child_death`
        // reads a bare SIGKILL after our signal as `Unobserved`.
        death: handoff_child_death(false, status),
        signalled: true,
        steps,
    }
}

/// Retires a held, ungranted successor: stand it down, prove it gone, then publish
/// the completion that clears the attempt. The sibling of
/// [`worker_reject_and_reap_handoff_child`] for a live candidate holding nothing.
///
/// The main thread goes first. This thread cannot know whether the readers are
/// parked (the park gate re-runs every 20 ms while the successor holds), so
/// `Wake::UpdateHandoffStandingDown` is sent unconditionally and awaited. Its
/// handler runs on the main thread beside the park gate: it closes the gate
/// (`stood_down`) and resumes any parked readers (`UnparkForRepark`). Only then
/// does this thread revoke, making Unpark -> Revoke -> Kill -> Reap -> Retire real.
///
/// The wait is bounded by [`STAND_DOWN_ACK_BUDGET`] and fails open: a candidate
/// never killed is worse than a late unpark. The attempt record outlives it all,
/// so no retry launches beside the candidate.
#[cfg(target_os = "macos")]
fn retire_ungranted_successor(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    lane: &HandoffPrelaunchLane,
    held: HeldSuccessor,
    outcome: crate::UpdateHandoffOutcome,
    detail: String,
) {
    let nonce = lane.nonce.as_str();
    if proxy
        .send_event(Wake::UpdateHandoffStandingDown {
            attempt_id: job.attempt_id,
        })
        .is_ok()
        && let Err(error) = lane.stand_down_ack.recv_timeout(STAND_DOWN_ACK_BUDGET)
    {
        aterm_log::warn!(
            "update apply: the outgoing process did not answer the stand-down within {} ms \
             ({error}); revoking anyway — a candidate left alive is worse than a late reader \
             resume",
            STAND_DOWN_ACK_BUDGET.as_millis()
        );
    }
    if !worker_claim_handoff_reaper(&job.arbiter) {
        // Commit is unreachable before a grant: nothing has been given to the
        // successor, so no proof — and no Commit — can exist for this attempt.
        debug_assert!(false, "Commit is unreachable before the grant");
        return;
    }
    let candidate = held.candidate;
    let held_for = held.dialled_at.elapsed();
    let stood = stand_down_held_successor(held, STAND_DOWN_SELF_EXIT_GRACE);
    let completed = job.arbiter.finish_reap(crate::HandoffReaperOwner::Worker);
    debug_assert!(
        completed,
        "the worker must retain its unique reaper ownership"
    );
    // Log the steps taken; each is a derived-model action
    // (`StandDownStep::model_action`) that the tests replay against the model.
    aterm_log::info!(
        "update apply: stood the held successor (pid {}) down after holding its claim {} ms — \
         {detail}; steps {:?} ({})",
        candidate.pid,
        held_for.as_millis(),
        stood.steps,
        if stood.signalled {
            "it ignored the closed rendezvous for the grace and was killed and proven terminated"
        } else {
            "it was gone with no signal of ours"
        }
    );
    let published = publish_warranted_handoff_failure(
        stood.warrant,
        &job.cleanup,
        proxy,
        crate::UpdateHandoffCompletion::failure(
            job.attempt_id,
            Some(nonce.to_string()),
            Some(candidate.pid),
            outcome,
            detail,
        )
        .with_child_death(stood.death),
    );
    // Exactly one party gives the counted trial launch back
    // (`StoodDownSuccessor::forgives_the_counted_trial_launch`), before the disk
    // facts so they carry the sentinel's count.
    if stood.forgives_the_counted_trial_launch(job.current_build, job.target_build) {
        forgive_candidate_trial_launch(job, candidate);
    }
    if published {
        send_post_failure_reconcile_facts(&job.cleanup, proxy, job.current_build);
    }
}

/// The boundaries of the parent's park->transfer interval on the launched lane
/// (warm successor P0), in the order the attempt crosses them.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug)]
struct ParkTransferSplit {
    /// The main thread began parking the readers.
    park_at: std::time::Instant,
    /// The capture was done and handed to the worker.
    captured_at: std::time::Instant,
    /// The worker took the capture off the transfer channel.
    picked_up_at: std::time::Instant,
    /// `prepare_outgoing_artifacts` returned.
    artifacts_at: std::time::Instant,
    /// The `sendmsg` of the grant began (after the hang-up poll and the
    /// keeper's pending).
    sendmsg_at: std::time::Instant,
    /// The grant was delivered.
    granted_at: std::time::Instant,
}

/// `capture 0.6 + hand-over 0.1 + artifacts 20.3 + pre-grant 0.4 + sendmsg 0.9 ms`:
/// five consecutive slices that sum to park->transfer. A slice whose ends are
/// out of order reads 0.0 rather than wrapping.
#[cfg(target_os = "macos")]
fn park_transfer_split_text(split: ParkTransferSplit) -> String {
    let slice = |from: std::time::Instant, to: std::time::Instant| {
        to.saturating_duration_since(from).as_secs_f64() * 1000.0
    };
    format!(
        "capture {:.1} + hand-over {:.1} + artifacts {:.1} + pre-grant {:.1} + sendmsg {:.1} ms",
        slice(split.park_at, split.captured_at),
        slice(split.captured_at, split.picked_up_at),
        slice(split.picked_up_at, split.artifacts_at),
        slice(split.artifacts_at, split.sendmsg_at),
        slice(split.sendmsg_at, split.granted_at),
    )
}

/// How the worker's hold ended.
#[cfg(target_os = "macos")]
enum HoldOutcome {
    /// The main thread parked and captured; the grant follows.
    Transfer(Box<HandoffTransferJob>),
    /// The successor must be stood down before anything was granted.
    StandDown {
        outcome: crate::UpdateHandoffOutcome,
        detail: String,
    },
}

/// The hold: wait, with every reader live, for the main thread's capture or for
/// whatever ends the attempt first. Each slice is a bounded `recv_timeout` on the
/// transfer channel followed by the cheap checks: a stand-down, the cancel poke,
/// the dialer hanging up or exiting, LaunchServices contradicting the dialer's
/// identity, and the hold bound.
#[cfg(target_os = "macos")]
fn hold_for_park(
    job: &HandoffWorkerJob,
    lane: &HandoffPrelaunchLane,
    held: &HeldSuccessor,
    mut corroboration: Option<&mut LaunchCorroboration<'_>>,
) -> HoldOutcome {
    const SLICE: std::time::Duration = std::time::Duration::from_millis(2);
    let hold_deadline = held.dialled_at + lane.hold_bound;
    loop {
        match lane.transfer.recv_timeout(SLICE) {
            Ok(transfer) => return HoldOutcome::Transfer(Box::new(transfer)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return HoldOutcome::StandDown {
                    outcome: crate::UpdateHandoffOutcome::Rejected,
                    detail: "the attempt record was dropped before the park".to_string(),
                };
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Ok(stand_down) = lane.stand_down.try_recv() {
            return HoldOutcome::StandDown {
                outcome: stand_down.outcome,
                detail: stand_down.detail,
            };
        }
        if job.cancel.try_recv().is_ok() {
            return HoldOutcome::StandDown {
                outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
                detail: "structural activity revoked the handoff while the successor held the \
                         claim"
                    .to_string(),
            };
        }
        if held.peer.poll_hangup() {
            return HoldOutcome::StandDown {
                outcome: crate::UpdateHandoffOutcome::ChildDied,
                detail: "the successor closed the rendezvous while holding the claim".to_string(),
            };
        }
        if held
            .exit_watch
            .as_ref()
            .and_then(CandidateExitWatch::exit_status)
            .is_some()
        {
            return HoldOutcome::StandDown {
                outcome: crate::UpdateHandoffOutcome::ChildDied,
                detail: "the successor exited while holding the claim".to_string(),
            };
        }
        if let Some(corroboration) = corroboration.as_deref_mut()
            && let Err(detail) = corroboration.poll()
        {
            return HoldOutcome::StandDown {
                outcome: crate::UpdateHandoffOutcome::Rejected,
                detail,
            };
        }
        if std::time::Instant::now() >= hold_deadline {
            return HoldOutcome::StandDown {
                outcome: crate::UpdateHandoffOutcome::TimedOut,
                detail: format!(
                    "the outgoing process did not park within {} s of the successor's dial",
                    lane.hold_bound.as_secs()
                ),
            };
        }
    }
}

/// A runtime refusal of the launched lane before anything was launched: ask the
/// main thread to park exactly as the dial would have, wait for its capture with
/// every reader live, and hand the attempt to the fork lane.
#[cfg(target_os = "macos")]
fn fork_after_park(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    lane: HandoffPrelaunchLane,
    why: &str,
) -> Prelaunched {
    const SLICE: std::time::Duration = std::time::Duration::from_millis(2);
    let HandoffPrelaunchLane {
        nonce,
        transfer,
        stand_down,
        // Nothing has been launched on this path, so nothing will be stood
        // down: the fork below is the attempt's only candidate.
        stand_down_ack: _,
        hold_bound,
        // The fork lane inherits its environment and warms after its intake.
        warm_hint: _,
        // The fork lane parks before it spawns: it writes every artifact
        // after the park, the layout included (`prepare_outgoing_artifacts`).
        speculative_layout: _,
        // A forked successor is this process's own child: no rendezvous, so no
        // dialer's code to check.
        successor_runs_candidate: _,
    } = lane;
    aterm_log::warn!("update apply: {why}; forking instead — asking the outgoing process to park");
    if proxy
        .send_event(Wake::UpdateHandoffAwaitingPark {
            attempt_id: job.attempt_id,
            dialer_pid: None,
            // A forked successor inherits its masters: no descriptor message,
            // so no grant limit, binds this park (the capture still refuses
            // past `MAX_HANDOFF_SESSIONS`).
            grant_limit: None,
        })
        .is_err()
    {
        send_handoff_producer_failure(job, proxy, Some(nonce), "event loop closed before the park");
        return Prelaunched::Finished;
    }
    let hold_deadline = std::time::Instant::now() + hold_bound;
    loop {
        match transfer.recv_timeout(SLICE) {
            Ok(transfer) => {
                return Prelaunched::ForkInstead {
                    nonce,
                    transfer: Box::new(transfer),
                };
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                send_handoff_producer_failure(
                    job,
                    proxy,
                    Some(nonce),
                    "the attempt record was dropped before the park",
                );
                return Prelaunched::Finished;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Ok(stand_down) = stand_down.try_recv() {
            send_warranted_handoff_failure(
                HandoffRollbackWarrant::NoCandidate,
                &job.cleanup,
                proxy,
                job.current_build,
                crate::UpdateHandoffCompletion::failure(
                    job.attempt_id,
                    Some(nonce),
                    None,
                    stand_down.outcome,
                    stand_down.detail,
                ),
            );
            return Prelaunched::Finished;
        }
        if job.cancel.try_recv().is_ok() {
            send_warranted_handoff_failure(
                HandoffRollbackWarrant::NoCandidate,
                &job.cleanup,
                proxy,
                job.current_build,
                crate::UpdateHandoffCompletion::failure(
                    job.attempt_id,
                    Some(nonce),
                    None,
                    crate::UpdateHandoffOutcome::ActivityRevoked,
                    "structural activity revoked the handoff before the park",
                ),
            );
            return Prelaunched::Finished;
        }
        if std::time::Instant::now() >= hold_deadline {
            // A hold that timed out is timing (`HoldOutcome::StandDown { TimedOut }`),
            // not a verdict about bytes the fork had not started.
            send_warranted_handoff_failure(
                HandoffRollbackWarrant::NoCandidate,
                &job.cleanup,
                proxy,
                job.current_build,
                crate::UpdateHandoffCompletion::failure(
                    job.attempt_id,
                    Some(nonce),
                    None,
                    crate::UpdateHandoffOutcome::TimedOut,
                    format!(
                        "the outgoing process did not park within {} s",
                        hold_bound.as_secs()
                    ),
                ),
            );
            return Prelaunched::Finished;
        }
    }
}

/// The launched lane's early artifacts: the attempt's two pipes and its layout
/// sidecar, made before the park while the successor boots. `layout_path` is the
/// sidecar's name (the launch environment's `ATERM_SEAMLESS_LAYOUT`) and `layout`
/// the layout at launch. A step that refuses is left to the park; nothing here
/// can fail the attempt.
#[cfg(target_os = "macos")]
fn prepare_early_artifacts(
    layout_path: &std::path::Path,
    layout: Option<crate::restore::RestoreManifest>,
) -> EarlyArtifacts {
    let started = std::time::Instant::now();
    let pipes = match AttemptPipes::make() {
        Ok(pipes) => Some(pipes),
        Err(why) => {
            aterm_log::warn!(
                "update apply: {why} before the park; the park makes the pipes itself"
            );
            None
        }
    };
    let layout = layout.and_then(|layout| {
        match crate::handoff_diet::speculate_layout(layout_path, &layout) {
            Ok(speculative) => Some(speculative),
            Err(why) => {
                aterm_log::info!(
                    "update apply: the layout sidecar was not written before the park ({why}); \
                     the park writes it"
                );
                None
            }
        }
    });
    aterm_log::info!(
        "update apply: before the park, in {:.1} ms with every reader live: {} and {}",
        started.elapsed().as_secs_f64() * 1000.0,
        if pipes.is_some() {
            "made the attempt's pipes"
        } else {
            "made no pipes"
        },
        if layout.is_some() {
            "wrote the layout sidecar the park will verify"
        } else {
            "wrote no layout sidecar"
        },
    );
    EarlyArtifacts { pipes, layout }
}

/// The launched lane in late-park order: bind, launch, accept and hold the dial
/// with every reader live; ask the main thread to park; then write the artifacts
/// under the pre-minted nonce, grant on the held stream, and hand over to the
/// fork lane's decision tail.
///
/// The listener is bound before the launch because the successor is told an
/// existing name; the launch environment names the manifest and layout by path
/// before they exist because the nonce is minted first; the dial is awaited with
/// the terminal still echoing because it follows the successor's whole staged-swap
/// boot apply; and the park happens at the dial, so only the capture, the write,
/// one `sendmsg` and the successor's GUI boot remain under the freeze.
///
/// Before the transfer no descriptor has left the process, so every failure closes
/// the rendezvous (EOF), lets the successor exit by its own rule, and kills and
/// proves it gone otherwise before the attempt retires
/// ([`retire_ungranted_successor`]).
#[cfg(target_os = "macos")]
fn run_prelaunched_handoff(
    job: &mut HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    mut lane: HandoffPrelaunchLane,
) -> Prelaunched {
    use std::os::fd::AsFd as _;

    let Some(bundle) = job.bundle.clone() else {
        // Not a bundle: nothing has moved, so fork rather than skip the update.
        return fork_after_park(
            job,
            proxy,
            lane,
            "this process does not run from a .app bundle",
        );
    };
    // The code the dialer must run, settled before anything exists to clean up: a
    // requirement that cannot be stated, or one the candidate is not signed to
    // meet, launches nothing, and the fork lane, whose successor is this process's
    // own child, carries the attempt instead.
    let activation = job.installed_activation.then_some(bundle.as_path());
    let dialer_code = match lane.dialer_code(aterm_update::effective_team_id(), &bundle, || {
        aterm_update::handoff_candidate_signing_identifier(activation)
    }) {
        Ok(code) => code,
        Err(error) => {
            return fork_after_park(
                job,
                proxy,
                lane,
                &format!("the launched lane cannot gate this successor's dial ({error})"),
            );
        }
    };
    let rendezvous = match crate::handoff_rendezvous::Rendezvous::bind(&lane.nonce) {
        Ok(rendezvous) => rendezvous,
        Err(error) => {
            // A stale node, an unwritable support dir, a path over sun_path: no
            // descriptor has moved, and the fork lane needs no socket.
            return fork_after_park(
                job,
                proxy,
                lane,
                &format!("the handoff rendezvous could not be bound ({error})"),
            );
        }
    };
    let Some(rendezvous_path) = rendezvous.path().to_str().map(str::to_string) else {
        drop(rendezvous);
        return fork_after_park(job, proxy, lane, "the handoff rendezvous path is not UTF-8");
    };
    // The launch environment is derived from the fork lane's `Command`, so argv and
    // every inherited-authority variable match by construction. Only the transport
    // differs: no descriptor numbers (a launched process inherits none), and a
    // rendezvous plus its claim secret instead.
    let Some(mut environment) = launch_environment(&job.command) else {
        // A merged launch cannot express a REMOVAL, and the fork lane can — this is
        // the one refusal where forking is not merely acceptable but correct.
        drop(rendezvous);
        return fork_after_park(
            job,
            proxy,
            lane,
            "the launch environment needs a removal a merged launch cannot express",
        );
    };
    // The artifacts are named before they exist: the writer derives the same names
    // from the same nonce after the park (`prepare_outgoing_artifacts` asserts it),
    // and the successor reads them only after the grant. Only the layout sidecar is
    // written before the park, and sealed against the park's layout.
    let Some(manifest_path) = crate::seamless::outgoing_manifest_path(&lane.nonce) else {
        send_handoff_producer_failure(
            job,
            proxy,
            Some(lane.nonce.clone()),
            "no private control directory to write the handoff manifest into",
        );
        return Prelaunched::Finished;
    };
    let layout_path = manifest_path.with_extension("layout.toml");
    environment.push((
        "ATERM_SEAMLESS_MANIFEST".into(),
        manifest_path.into_os_string(),
    ));
    environment.push(("ATERM_SEAMLESS_NONCE".into(), lane.nonce.clone().into()));
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
    // The chunked grant is offered only to a candidate that declared it in its
    // verified `Info.plist`; it claims `ATRZ2C`. Any other successor never sees the
    // variable and claims `ATRZ1C`.
    if job.successor_grant_chunks {
        environment.push((
            crate::handoff_rendezvous::ENV_GRANT_CAPS.into(),
            crate::handoff_rendezvous::GRANT_CAPS_CHUNKS.into(),
        ));
    }
    // The warm hint, read by the successor's warm prologue before it dials.
    // Advisory: an older successor ignores it, and none authenticates by it.
    environment.push((
        crate::handoff_warm_hint::ENV_WARM_HINT.into(),
        lane.warm_hint.clone().into(),
    ));
    for (key, value) in crate::seamless::outgoing_parent_env() {
        environment.push((key.into(), value.into()));
    }
    let arguments = job
        .command
        .get_args()
        .map(std::ffi::OsStr::to_os_string)
        .collect::<Vec<_>>();

    if handoff_preparation_cancelled(job, proxy, Some(lane.nonce.clone())) {
        return Prelaunched::Finished;
    }
    let dial_deadline = handoff_ready_deadline();
    let launch_at = std::time::Instant::now();
    // The launch is issued, not awaited: the successor dials after its boot apply,
    // before LaunchServices answers (which needs the app to check in). The dial is
    // the liveness signal and the claim secret the lock; the launch answer
    // corroborates after the dial.
    //
    // A refusal means nothing was launched, so the fork lane can still carry this
    // attempt. Dropping the rendezvous unlinks the node.
    let in_flight = match crate::app_launch_successor::begin_launch(
        &bundle,
        &arguments,
        &environment,
        // Never activated at launch: the successor has no window yet. The parent
        // activates it at Commit when an aterm window had focus at the park
        // (`PendingUpdateHandoff::activate_at_commit`).
        false,
    ) {
        Ok(in_flight) => in_flight,
        Err(error) => {
            drop(rendezvous);
            return fork_after_park(
                job,
                proxy,
                lane,
                &format!("LaunchServices refused the successor ({error})"),
            );
        }
    };
    aterm_log::info!(
        "update apply: asked LaunchServices for the successor as its own launchd application \
         job with every reader live (not activated; activation is the parent's at Commit); \
         awaiting its rendezvous dial from code that meets {}",
        dialer_code.describe()
    );
    // Artifacts that do not depend on the park (`handoff_diet`) are made now, while
    // the successor boots: the two pipes and the layout sidecar from the layout at
    // launch, which the park keeps only when byte-equal to its own. Done before the
    // accept on this thread, so a dial waits in the backlog and no failure path
    // sweeps the attempt's files (by prefix) while a write is in flight.
    let early = prepare_early_artifacts(&layout_path, lane.speculative_layout.take());
    // No expected pid at the gate: LaunchServices names the successor only when
    // it checks in, after its grant. The claim secret binds a dialer to this
    // attempt, its code (`dialer_code`) admits it, and the kernel-attested peer
    // pid is its identity; the LaunchServices pid corroborates it later.
    let launched: Option<i32> = None;
    let peer = match await_launched_dial(
        &rendezvous,
        &in_flight,
        &dialer_code,
        dial_deadline,
        &job.cancel,
    ) {
        Ok(peer) => peer,
        Err(LaunchedDialEnd::Cancelled) => {
            send_warranted_handoff_failure(
                HandoffRollbackWarrant::NeverTransferred,
                &job.cleanup,
                proxy,
                job.current_build,
                crate::UpdateHandoffCompletion::failure(
                    job.attempt_id,
                    Some(lane.nonce),
                    launched.map(pid_for_completion),
                    crate::UpdateHandoffOutcome::ActivityRevoked,
                    "structural activity revoked the handoff before the successor dialled",
                ),
            );
            return Prelaunched::Finished;
        }
        // The successor died before its dial: LaunchServices reports it terminated
        // before checking in, or the named process is gone. Rolled back like the
        // deadline below (nothing accepted, `NeverTransferred`) but at once, with
        // the counted trial launch settled by the evidence ([`send_pre_dial_death`]).
        Err(LaunchedDialEnd::SuccessorExited(death)) => {
            send_pre_dial_death(
                job,
                proxy,
                lane.nonce,
                death,
                None,
                launch_at.elapsed(),
                &crate::handoff_rendezvous::launch_stamp_path(rendezvous.path()),
            );
            return Prelaunched::Finished;
        }
        // The successor dialled and the code gate turned it away; it left by its
        // rule. Filed by the refusal's cause ([`send_code_refused_dial`]).
        Err(LaunchedDialEnd::CodeRefused {
            dialer,
            refusal,
            death,
        }) => {
            send_code_refused_dial(job, proxy, lane.nonce, dialer, &refusal, death);
            return Prelaunched::Finished;
        }
        Err(LaunchedDialEnd::ExitedWhileHeld { dialer, death }) => {
            send_pre_dial_death(
                job,
                proxy,
                lane.nonce,
                death,
                Some(dialer.pid),
                launch_at.elapsed(),
                &crate::handoff_rendezvous::launch_stamp_path(rendezvous.path()),
            );
            return Prelaunched::Finished;
        }
        Err(LaunchedDialEnd::Rendezvous(error)) => {
            // A deadline that ended with a dialer turned away (or held) for its code
            // says so; "never claimed" is false of a successor whose code check
            // `trustd` would not answer.
            let detail = if matches!(
                error,
                crate::handoff_rendezvous::RendezvousError::PeerCode { .. }
            ) {
                format!("no rendezvous dialer was admitted before the dial deadline: {error}")
            } else {
                format!("the launched successor never claimed the handoff: {error}")
            };
            send_warranted_handoff_failure(
                HandoffRollbackWarrant::NeverTransferred,
                &job.cleanup,
                proxy,
                job.current_build,
                crate::UpdateHandoffCompletion::failure(
                    job.attempt_id,
                    Some(lane.nonce),
                    launched.map(pid_for_completion),
                    crate::UpdateHandoffOutcome::TimedOut,
                    detail,
                ),
            );
            return Prelaunched::Finished;
        }
    };
    // The dial happened with the terminal still echoing; the launch prologue, bundle
    // swap, re-exec and boot check are the interval this line reports, before the
    // park.
    let dialled_at = std::time::Instant::now();
    aterm_log::info!(
        "update apply: successor dialled — launch->dial {} ms with every reader live; holding \
         its claim until the outgoing process parks",
        dialled_at.saturating_duration_since(launch_at).as_millis(),
    );
    // Identity and witness first, before any arm that can end the attempt: an
    // accepted dialer is a live process every exit from here must prove dead, or
    // the automatic lane could launch a second successor beside it. The pid is the
    // kernel's attestation plus its birth stamp, which survive pid reuse, and
    // `EVFILT_PROC` attaches only to a process that exists, as it does now.
    let candidate = HandoffCandidate::of_attested_peer(pid_for_completion(peer.pid()));
    // Registered at the dial, before any descriptor is granted, and held to the end:
    // until the Commit this process answers to the carried ids (`identity_claim`).
    // Every return below comes after its proof of death.
    let _transfer = crate::identity_claim::register_handoff_candidate(candidate.pid);
    let exit_watch = CandidateExitWatch::watch(candidate.pid);
    if exit_watch.is_none() {
        aterm_log::warn!(
            "update apply: could not watch launched candidate {} for exit; a death on this \
             lane will be reported as Unobserved",
            candidate.pid
        );
    }
    let held = HeldSuccessor {
        rendezvous,
        peer,
        candidate,
        exit_watch,
        dialled_at,
    };
    // Corroboration, not the lock: if LaunchServices has answered, its pid must be
    // the dialer's (a mismatch is a stranger that knew the secret), and its word
    // that the process it launched exited must not meet a live dialer. No answer
    // yet is expected, so nothing waits here: the answer is polled beside the hold
    // and the proof wait (`LaunchCorroboration`), and a late contradiction refuses
    // before Commit. A refusal here proves the dialer gone; the pid LaunchServices
    // names goes in the message only, so nothing acts on a process this parent
    // never touched.
    let mut corroboration = LaunchCorroboration::of_dialer(&in_flight, candidate);
    if let Err(detail) = corroboration.poll() {
        retire_ungranted_successor(
            job,
            proxy,
            &lane,
            held,
            crate::UpdateHandoffOutcome::Rejected,
            detail,
        );
        return Prelaunched::Finished;
    }
    if !corroboration.answered {
        aterm_log::info!(
            "update apply: LaunchServices has not answered yet; the dial is the liveness \
             signal — the answer is read beside the hold and the proof wait"
        );
    }
    // A dialer that hung up holds nothing, but POLLHUP only means the socket
    // closed; prove it gone like every stood-down dialer (for one that exited, a
    // single `kill(pid, 0)`).
    if held.peer.poll_hangup() {
        retire_ungranted_successor(
            job,
            proxy,
            &lane,
            held,
            crate::UpdateHandoffOutcome::ChildDied,
            "the successor closed the rendezvous after dialling, before any descriptor was sent"
                .to_string(),
        );
        return Prelaunched::Finished;
    }
    let mut corroboration = (!corroboration.answered).then_some(corroboration);
    // The cue: the main thread parks when its gate admits (at once for an explicit
    // apply, at the next quiet moment for the automatic lane) and sends the
    // capture down the transfer channel.
    if proxy
        .send_event(Wake::UpdateHandoffAwaitingPark {
            attempt_id: job.attempt_id,
            dialer_pid: Some(candidate.pid),
            // The session count the dialer's claim lets the grant carry, so the
            // park refuses a desk that does not fit before freezing anything.
            grant_limit: Some(held.peer.grant_session_limit()),
        })
        .is_err()
    {
        retire_ungranted_successor(
            job,
            proxy,
            &lane,
            held,
            crate::UpdateHandoffOutcome::Rejected,
            "event loop closed before the park".to_string(),
        );
        return Prelaunched::Finished;
    }
    let transfer = match hold_for_park(job, &lane, &held, corroboration.as_mut()) {
        HoldOutcome::Transfer(transfer) => transfer,
        HoldOutcome::StandDown { outcome, detail } => {
            retire_ungranted_successor(job, proxy, &lane, held, outcome, detail);
            return Prelaunched::Finished;
        }
    };
    // The park landed: readers are parked and nothing is granted. Every failure
    // until the `sendmsg` releases the readers, then stands the successor down.
    let HandoffTransferJob {
        capture,
        proof_deadline,
    } = *transfer;
    let park_at = capture.park_at;
    // The park->transfer split: each boundary to the grant, logged beside the
    // total. Observation only.
    let captured_at = capture.captured_at;
    let picked_up_at = std::time::Instant::now();
    job.capture = Some(capture);
    aterm_log::info!(
        "update apply: the outgoing process parked {} ms after the dial; writing the \
         artifacts and granting on the held claim",
        park_at.saturating_duration_since(dialled_at).as_millis()
    );
    let prepared = match prepare_outgoing_artifacts(job, &lane.nonce, early) {
        Ok(prepared) => prepared,
        Err(PreparationFailure::Cancelled) => {
            retire_ungranted_successor(
                job,
                proxy,
                &lane,
                held,
                crate::UpdateHandoffOutcome::ActivityRevoked,
                "activity revoked handoff during physical preparation".to_string(),
            );
            return Prelaunched::Finished;
        }
        Err(PreparationFailure::Producer(detail)) => {
            retire_ungranted_successor(
                job,
                proxy,
                &lane,
                held,
                crate::UpdateHandoffOutcome::ProducerFailed,
                detail,
            );
            return Prelaunched::Finished;
        }
        Err(PreparationFailure::Refused(detail)) => {
            retire_ungranted_successor(
                job,
                proxy,
                &lane,
                held,
                crate::UpdateHandoffOutcome::CaptureRefused,
                detail,
            );
            return Prelaunched::Finished;
        }
    };
    let artifacts_at = std::time::Instant::now();
    // A dialer that hung up DURING the hold or the write holds nothing and is
    // owed nothing but a proof of its death.
    if held.peer.poll_hangup() {
        retire_ungranted_successor(
            job,
            proxy,
            &lane,
            held,
            crate::UpdateHandoffOutcome::ChildDied,
            "the successor closed the rendezvous before the grant".to_string(),
        );
        return Prelaunched::Finished;
    }
    // `capture.live` is `(local_id, child-owned master duplicate, shell pid)`; the
    // transfer wants `(local_id, shell pid, master)` in send order, which addresses
    // the descriptors on arrival.
    let sessions = job
        .capture()
        .live
        .iter()
        .map(|(local_id, master, pid)| {
            // SAFETY: the capture owns these duplicates for the whole attempt
            // (`_owned_masters`), so the borrow cannot outlive them.
            (*local_id, *pid, unsafe {
                std::os::fd::BorrowedFd::borrow_raw(*master)
            })
        })
        .collect::<Vec<_>>();
    // Tell the PTY keeper the successor's pid before the grant, so a death of this
    // window while the successor holds the masters is judged a handoff, not a
    // crash. Bounded; the holder scan is the keeper's independent gate.
    crate::keeper_link::pending_before_grant(held.candidate.pid);
    let sendmsg_at = std::time::Instant::now();
    let transfer_failed = held
        .peer
        .transfer(
            &lane.nonce,
            &sessions,
            prepared.proof_wr.as_fd(),
            prepared.commit_rd.as_fd(),
            proof_deadline,
        )
        .err();
    if let Some(error) = transfer_failed {
        // Which side of the `sendmsg`: only a failure before the descriptors left is
        // an ungranted stand-down. After it the successor holds duplicates of every
        // master, and rollback owes the fork lane's candidate proof (kill, reap or
        // witness) before resuming the readers. The partial case is usually EPIPE
        // from a successor that already closed its socket.
        if matches!(
            error,
            crate::handoff_rendezvous::RendezvousError::TransferPartial(_)
        ) {
            let HeldSuccessor {
                rendezvous,
                peer,
                candidate,
                exit_watch,
                ..
            } = held;
            drop(rendezvous);
            drop(peer);
            drop(prepared.proof_wr);
            drop(prepared.commit_rd);
            let mut handle = HandoffCandidateHandle::Launched(exit_watch);
            let rejected = worker_reject_and_reap_handoff_child(
                job,
                proxy,
                &mut handle,
                candidate,
                &lane.nonce,
                crate::UpdateHandoffOutcome::Rejected,
                format!("the handoff descriptors were delivered but the grant was not: {error}"),
            );
            debug_assert!(rejected, "Commit is unreachable before the grant body");
            return Prelaunched::Finished;
        }
        // More sessions than this claim's grant carries is this process's limit,
        // refused before a descriptor left and filed `ProducerFailed` like the park's
        // own session check. The proof deadline running out before the send is this
        // process's lateness, filed `TimedOut`, never `Rejected`.
        let outcome = match error {
            crate::handoff_rendezvous::RendezvousError::TooManySessions { .. } => {
                crate::UpdateHandoffOutcome::ProducerFailed
            }
            crate::handoff_rendezvous::RendezvousError::GrantDeadline => {
                crate::UpdateHandoffOutcome::TimedOut
            }
            _ => crate::UpdateHandoffOutcome::Rejected,
        };
        retire_ungranted_successor(
            job,
            proxy,
            &lane,
            held,
            outcome,
            format!("the handoff descriptors could not be delivered: {error}"),
        );
        return Prelaunched::Finished;
    }
    let granted_at = std::time::Instant::now();
    aterm_log::info!(
        "update apply: granted on the held claim — park->transfer {} ms (launch->dial was {} \
         ms, with every reader live)",
        granted_at.saturating_duration_since(park_at).as_millis(),
        dialled_at.saturating_duration_since(launch_at).as_millis(),
    );
    aterm_log::info!(
        "update apply: park->transfer split — {}",
        park_transfer_split_text(ParkTransferSplit {
            park_at,
            captured_at,
            picked_up_at,
            artifacts_at,
            sendmsg_at,
            granted_at,
        })
    );
    // From here the successor holds copies of everything, so a rollback owes a
    // candidate proof, and the decision is identical to the fork lane's.
    let HeldSuccessor {
        rendezvous,
        peer,
        candidate,
        exit_watch,
        ..
    } = held;
    drop(rendezvous);
    drop(peer);
    drop(prepared.proof_wr);
    drop(prepared.commit_rd);
    let mut handle = HandoffCandidateHandle::Launched(exit_watch);
    run_handoff_decision(
        job,
        proxy,
        &lane.nonce,
        prepared.expected,
        &prepared.proof_rd,
        prepared.commit_wr,
        candidate,
        &mut handle,
        proof_deadline,
        corroboration,
    );
    // The attempt reached the shared decision tail, so it is finished either way.
    Prelaunched::Finished
}

/// A launched pid in the `u32` shape the completion wire and [`HandoffCandidate`]
/// use. The conversion cannot fail in practice (`app_launch_successor` refuses
/// non-positive pids); zero is the fallback because every consumer refuses it:
/// `signal_handoff_candidate` returns for `pid <= 1`, and
/// `handoff_candidate_terminated` never proves its vacancy.
#[cfg(target_os = "macos")]
#[must_use]
fn pid_for_completion(pid: i32) -> u32 {
    u32::try_from(pid).unwrap_or_default()
}

/// The launch environment a `Command` describes, or `None` when it cannot be
/// expressed as a merge.
///
/// `NSWorkspaceOpenConfiguration.environment` merges over the launcher's
/// environment and cannot remove a variable, but `bind_expected_update_artifact`
/// uses `env_remove` so a successor never authenticates against a stale
/// `ATERM_UPDATE_EXPECTED_*`. A removal of a variable set in this process makes
/// the lane unavailable (the attempt forks); removing an unset one is ignored.
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
/// ProofReady wake, and the decision loop ending in Commit or a warranted
/// rejection. Shared by both lanes so the decision cannot differ; only how the
/// candidate is reaped differs, which is `handle`'s job.
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
    #[cfg(target_os = "macos")] corroboration: Option<LaunchCorroboration<'_>>,
) {
    let live = job.capture().live.clone();
    let proof_outcome = wait_handoff_ready(
        proof_rd,
        expected,
        &job.cancel,
        &live.iter().map(|(_, fd, _)| *fd).collect::<Vec<_>>(),
        deadline,
        &mut || handle.candidate_gone(candidate),
        #[cfg(target_os = "macos")]
        corroboration,
    );
    if proof_outcome != crate::UpdateHandoffOutcome::ProofReady {
        // `ChildDied` is the candidate ending before its proof: a refusing, crashed
        // or starved successor all close the readiness channel identically. The
        // successor states refusals itself (`seamless::take_target_identity`,
        // `main_entry`'s degraded-authority exit) and the reject path adds what the
        // parent saw (`handoff_child_death`). Log-only; the completion detail stays
        // short.
        if proof_outcome == crate::UpdateHandoffOutcome::ChildDied {
            aterm_log::warn!(
                "update apply: the successor ended or closed the readiness channel without proving \
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
    // The proof checked out, so the successor consumed everything published. A
    // successor that predates the control carry never reads the `.ctl` sidecars,
    // so unlink them now, Commit or not.
    crate::seamless::retire_outgoing_controls(nonce);

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
        // Readable PTY output does not revoke here (post-park bytes wait in the
        // kernel for the child). Only a cancel poke (`ActivityRevoked`) or session
        // death (HUP/ERR) rejects before Commit.
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
        // Session death is the desk changing, typed as the park types it
        // (`handoff_rejection_activity_shaped`).
        if handoff_masters_closed(&live)
            && worker_reject_and_reap_handoff_child(
                job,
                proxy,
                handle,
                candidate,
                nonce,
                crate::UpdateHandoffOutcome::ActivityRevoked,
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
        // A successor that proved and then ended must not be committed to: Commit
        // would `_exit` on a write only a stray pipe copy accepts, and nobody would
        // own the terminals. This narrows the window (the atomic Commit write's
        // EPIPE is the backstop). Asked after the reject channel and filed
        // `Rejected`, never `ChildDied`: from here the candidate exits when this
        // process closes the Commit pipe, so a death may be ours.
        if handle.candidate_gone(candidate)
            && worker_reject_and_reap_handoff_child(
                job,
                proxy,
                handle,
                candidate,
                nonce,
                crate::UpdateHandoffOutcome::Rejected,
                "the successor ended after proving adoption, before Commit".to_string(),
            )
        {
            return;
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

/// Ceiling on the decode authority one capture may hand its successor: the sum,
/// over every carried session, of twice `dimension_grid_cap`.
///
/// Authority, not memory: `dimension_grid_cap` prices a line at 512 bytes per
/// cell plus 16 KiB of framing so a hostile `meta` cannot authorize a huge decode,
/// which overstates a real screen by one or two orders of magnitude. At 4 GiB it
/// is a cheap early refusal for an absurd pool; the exact bound is
/// `MAX_HANDOFF_AGGREGATE_GRID_BYTES` (256 MiB), charged by `screen_digest_refs`
/// and `write_outgoing` against the real serialized bytes.
#[cfg(unix)]
pub(crate) const MAX_HANDOFF_CAPTURE_BUDGET_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Acquires a parked session's engine within the freeze budget instead of failing
/// on the first contention: after `park_all_readers` the scrollback-compression
/// worker and render/status paths can still take the mutex, and under a flood do
/// so on every attempt. The wait spends the capture's budget (250 ms explicit,
/// 20 ms first automatic); a poisoned lock is taken.
#[cfg(unix)]
fn try_lock_by<T>(
    lock: &std::sync::Mutex<T>,
    deadline: std::time::Instant,
) -> Option<std::sync::MutexGuard<'_, T>> {
    loop {
        match lock.try_lock() {
            Ok(guard) => return Some(guard),
            Err(std::sync::TryLockError::Poisoned(poison)) => return Some(poison.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                if std::time::Instant::now() >= deadline {
                    return None;
                }
                std::thread::yield_now();
            }
        }
    }
}

/// The wall-clock window the park and capture of a seamless handoff must fit in,
/// measured from the readers' park: the freeze the user feels.
///
/// Not a correctness bound: a busy machine must still update ("the failure mode is
/// less scrollback, never the update did not apply"). A miss widens the window
/// within one attempt (a missed park re-parks on the next rung 500 ms later while
/// the successor holds, `PRELAUNCH_MAX_PARK_MISSES`) and across attempts, up to a
/// quarter second. An explicit apply gets the widest window at once.
///
/// Rung 0 is seeded: `seed` is the attempt's [`FreezeSeed`] (the larger of a dry
/// run of this capture and the last landed park, else the ledger, else 20 ms), and
/// the first automatic rung is `clamp(1.5 × seed, 20 ms, 80 ms)`. The next rung is
/// twice the seed and at least 80 ms, so the ladder widens rung over rung.
#[cfg(any(unix, test))]
pub(crate) fn handoff_freeze_budget(
    mode: crate::native_updater_service::ApplyMode,
    prior_physical_failures: u8,
    seed: FreezeSeed,
) -> std::time::Duration {
    use crate::native_updater_service::ApplyMode;
    let rung0 = seed.rung0();
    match mode {
        ApplyMode::Immediate => FREEZE_LAST_RUNG,
        ApplyMode::Automatic | ApplyMode::AutomaticPastGrace => match prior_physical_failures {
            0 => rung0,
            1 => rung0.saturating_mul(2).max(FREEZE_RUNG0_CEILING),
            _ => FREEZE_LAST_RUNG,
        },
    }
}

/// The first automatic rung's floor, and what an unmeasured desk gets.
#[cfg(any(unix, test))]
pub(crate) const FREEZE_RUNG0_FLOOR: std::time::Duration = std::time::Duration::from_millis(20);

/// The first automatic rung's ceiling. A capture needing more on a first attempt
/// is a busy moment, answered by the ladder's wider rungs.
#[cfg(any(unix, test))]
pub(crate) const FREEZE_RUNG0_CEILING: std::time::Duration = std::time::Duration::from_millis(80);

/// The widest rung: an explicit apply's window, and the automatic ladder's top.
#[cfg(any(unix, test))]
const FREEZE_LAST_RUNG: std::time::Duration = std::time::Duration::from_millis(250);

/// Rung 0 from a measured freeze: `clamp(1.5 × measured, 20 ms, 80 ms)`. The extra
/// half is margin for park-to-park variance; the clamp keeps an unmeasurable or
/// absurd timing inside the ladder.
#[cfg(any(unix, test))]
#[must_use]
pub(crate) fn freeze_rung0_from_capture(capture: std::time::Duration) -> std::time::Duration {
    (capture.saturating_mul(3) / 2).clamp(FREEZE_RUNG0_FLOOR, FREEZE_RUNG0_CEILING)
}

/// Where an attempt's first automatic freeze rung came from, logged and persisted
/// in the health ledger so a rung that misses can be read against its seed.
#[cfg(any(unix, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FreezeSeed {
    /// Nothing measured: the 20 ms default ([`FREEZE_RUNG0_FLOOR`]).
    Default,
    /// This attempt's own dry run, timed while its successor booted — and
    /// never budgeted below the last park that really LANDED (`last_park`, from
    /// the ledger), because the dry run cannot time the part of the park that
    /// is the freeze itself: stopping the readers. Measured 2026-09-26 at
    /// opt-level 3, the capture of a two-session 50x200 desk with 10 000
    /// scrollback lines each is 1.1 ms; 0.91's and 0.92's parks of a
    /// two-session desk took 22 and 20 ms. Only the launched lane times a dry
    /// run ([`crate::App::seed_first_freeze_rung`]), and that lane is macOS's.
    #[cfg(any(target_os = "macos", test))]
    DryRun {
        capture: std::time::Duration,
        sessions: usize,
        last_park: Option<std::time::Duration>,
    },
    /// The health ledger's value (the larger of its last dry run and last landed
    /// park), used by the fork lane and by attempts whose dry run measured nothing.
    Ledger {
        capture: std::time::Duration,
        build: u64,
    },
}

#[cfg(any(unix, test))]
impl FreezeSeed {
    /// The first automatic rung this seed buys.
    #[must_use]
    pub(crate) fn rung0(self) -> std::time::Duration {
        match self {
            Self::Default => FREEZE_RUNG0_FLOOR,
            #[cfg(any(target_os = "macos", test))]
            Self::DryRun {
                capture, last_park, ..
            } => freeze_rung0_from_capture(capture.max(last_park.unwrap_or_default())),
            Self::Ledger { capture, .. } => freeze_rung0_from_capture(capture),
        }
    }

    /// The ledger's prior (the larger of its dry run and landed park, named by the
    /// build that recorded it), or the default when neither was measured.
    #[must_use]
    pub(crate) fn from_prior(prior: Option<&aterm_update::HandoffCaptureRecord>) -> Self {
        let Some(record) = prior else {
            return Self::Default;
        };
        let (need_us, build) = if record.park_us >= record.capture_us {
            (record.park_us, record.park_build)
        } else {
            (record.capture_us, record.build)
        };
        if need_us == 0 {
            return Self::Default;
        }
        Self::Ledger {
            capture: std::time::Duration::from_micros(need_us),
            build,
        }
    }

    /// The last landed park the ledger holds, if any — what a
    /// [`Self::DryRun`] seed is never budgeted below.
    #[cfg(any(target_os = "macos", test))]
    #[must_use]
    pub(crate) fn last_park_of(
        prior: Option<&aterm_update::HandoffCaptureRecord>,
    ) -> Option<std::time::Duration> {
        prior
            .filter(|record| record.park_us > 0)
            .map(|record| std::time::Duration::from_micros(record.park_us))
    }
}

#[cfg(any(unix, test))]
impl std::fmt::Display for FreezeSeed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Default => formatter.write_str("rung 0 at its default: no capture was timed"),
            #[cfg(any(target_os = "macos", test))]
            Self::DryRun {
                capture,
                sessions,
                last_park,
            } => {
                write!(
                    formatter,
                    "rung 0 seeded from this attempt's dry-run capture of {:.1} ms over \
                     {sessions} session(s)",
                    capture.as_secs_f64() * 1e3
                )?;
                match last_park {
                    Some(park) => write!(
                        formatter,
                        " and the last landed park's {:.1} ms",
                        park.as_secs_f64() * 1e3
                    ),
                    None => formatter.write_str(" (no landed park is on record)"),
                }
            }
            Self::Ledger { capture, build } => write!(
                formatter,
                "rung 0 seeded from the ledger's last timed freeze, {:.1} ms (build {build})",
                capture.as_secs_f64() * 1e3
            ),
        }
    }
}

/// Records what a landed park cost (`parked_in`, from the readers' stop to a
/// capture ready to hand over) for the next update's first rung, on its own
/// thread: the ledger lock is a file lock another process may hold, and this runs
/// inside the freeze. Returns the writer so a test can join it. The outgoing
/// process exits well after this millisecond write; a lost write costs one
/// measurement.
#[cfg(unix)]
pub(crate) fn keep_landed_park(
    parked_in: std::time::Duration,
) -> Option<std::thread::JoinHandle<()>> {
    let build = crate::running_build_number();
    // A small ledger write nobody waits on — but it must land before Commit, so
    // never the starvable Housekeeping class.
    spawn_ledger_writer("aterm-park-cost", move || {
        aterm_update::record_handoff_park(build, parked_in);
    })
    .ok()
}

/// Start one detached health-ledger write (`keep_landed_park`, the dry run's
/// seed) at Background QoS. In a test build each writer is counted while it
/// runs ([`ledger_writers`]): the park drops its handle, so a writer started
/// by one test can land after that test ends — inside the next one's reads.
#[cfg(unix)]
fn spawn_ledger_writer(
    name: &str,
    write: impl FnOnce() + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    #[cfg(test)]
    let in_flight = ledger_writers::InFlight::enter();
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            #[cfg(test)]
            let _in_flight = in_flight;
            crate::qos::set_self(crate::qos::Role::Background);
            write();
        })
}

/// The duplicates [`reserve_child_only_masters`] owns, and the
/// `(local_id, duplicate, pid)` adoption table that names them.
#[cfg(unix)]
type ChildOnlyMasters = (Vec<std::os::fd::OwnedFd>, Vec<(u64, i32, i32)>);

/// The child-only duplicate of every live master, for either lane's park:
/// each an independent close-on-exec descriptor at 3 or above, owned by the
/// caller, beside the `(local_id, duplicate, pid)` adoption table that names
/// them. The original can later close or reuse its number without changing
/// the open file description the successor is handed. The first duplicate
/// this process has no room for is the error (`EMFILE` at a launcher's
/// descriptor limit), and every duplicate made before it is closed again.
#[cfg(unix)]
fn reserve_child_only_masters(live: &[(u64, i32, i32)]) -> std::io::Result<ChildOnlyMasters> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    let mut owned_masters = Vec::with_capacity(live.len());
    let mut adoption = Vec::with_capacity(live.len());
    for (local_id, master, pid) in live {
        // SAFETY: duplicates one live parent master as an independent CLOEXEC
        // descriptor; the call reads no memory.
        let duplicate = unsafe { libc::fcntl(*master, libc::F_DUPFD_CLOEXEC, 3) };
        if duplicate < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: F_DUPFD_CLOEXEC returned a fresh descriptor owned here.
        let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicate) };
        adoption.push((*local_id, owned.as_raw_fd(), *pid));
        owned_masters.push(owned);
    }
    Ok((owned_masters, adoption))
}

/// The detached ledger writers a test build has in flight. The park lets its
/// writer run (`let _ledger_write = …`), and the update ledger is one file per
/// test process, so a writer started by a test that has already let go of the
/// ledger lock could land inside the next holder's write-then-read;
/// [`crate::app_update_screen::hold_update_ledger_for_test`] waits them out
/// before it hands the lock over. A hazard seen only with an injected writer
/// delay; it is not what failed
/// `the_first_rung_is_seeded_from_the_dry_run_and_kept_for_the_next_update`
/// (that was the seed's sub-microsecond capture, see `seed_first_freeze_rung`).
#[cfg(all(test, unix))]
pub(crate) mod ledger_writers {
    use std::sync::{Condvar, Mutex, PoisonError};

    static IN_FLIGHT: Mutex<usize> = Mutex::new(0);
    static LANDED: Condvar = Condvar::new();

    /// One writer in flight, until it is dropped (the thread ends, or never
    /// started).
    pub(crate) struct InFlight(());

    impl InFlight {
        pub(crate) fn enter() -> Self {
            *IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            Self(())
        }
    }

    impl Drop for InFlight {
        fn drop(&mut self) {
            let mut in_flight = IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner);
            *in_flight -= 1;
            drop(in_flight);
            LANDED.notify_all();
        }
    }

    /// Block until no detached ledger writer is in flight — the causal wait
    /// for a write a park started on this thread (its writer is counted
    /// before the park returns), bounded only as a hang detector (a minute):
    /// a writer is one small file write, but at Background QoS a loaded
    /// machine can starve it for seconds.
    pub(crate) fn wait_until_landed() {
        let in_flight = IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner);
        let (_in_flight, waited) = LANDED
            .wait_timeout_while(in_flight, std::time::Duration::from_secs(60), |n| *n > 0)
            .unwrap_or_else(PoisonError::into_inner);
        assert!(
            !waited.timed_out(),
            "a detached update-ledger writer never finished"
        );
    }
}

/// The dry run's whole window: two thirds of [`FREEZE_RUNG0_CEILING`], since
/// 1.5 × anything longer is the ceiling. Bounds the main thread's timing with
/// readers live.
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) const DRY_RUN_CAPTURE_WINDOW: std::time::Duration =
    std::time::Duration::from_micros(53_334);

// A window that ran out must saturate the rung: 1.5 × the window ≥ the ceiling.
#[cfg(any(target_os = "macos", all(test, unix)))]
const _: () =
    assert!(DRY_RUN_CAPTURE_WINDOW.as_micros() * 3 >= FREEZE_RUNG0_CEILING.as_micros() * 2);

/// What one dry run of the park's capture measured
/// ([`crate::App::dry_run_handoff_capture`]).
#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DryRunCapture {
    /// It captured `sessions` session(s) in `took`.
    Measured {
        took: std::time::Duration,
        sessions: usize,
    },
    /// It ran out of its window: at least `took`, which already saturates rung 0.
    AtLeast {
        took: std::time::Duration,
        sessions: usize,
    },
    /// It stopped for a reason that says nothing about time; nothing measured.
    Unmeasured(&'static str),
}

/// Whether one session may spend `optional` on a budget, on top of the
/// `own_mandatory` it must spend regardless.
///
/// `used` is what the pool has been charged, and `later_mandatory` the summed
/// non-degradable cost of every session not yet admitted. Overflow is a refusal,
/// never a wrap. Reserving later sessions' visible screens up front means a
/// session is refused only when the pool's visible-only total exceeds the ceiling,
/// so early sessions' scrollback cannot starve a later visible screen.
#[cfg(unix)]
pub(crate) fn optional_carry_fits(
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

/// Why the park's capture (`App::capture_parked_screens`) could not produce a
/// committed screen set. The first three kinds are timing or this process's own
/// memory, and retrying is right; `Refused` is deterministic and retrying the same
/// desk cannot change it. [`classify_capture_failure`] sorts a failure into a miss
/// or a refusal; [`std::fmt::Display`] gives the ledger and log sentences.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CaptureFailure {
    /// The capture outran the freeze budget.
    Deadline { freeze_ms: u128 },
    /// A session's engine stayed locked for the whole capture window.
    EngineBusy,
    /// This process could not reserve the capture's bounded storage.
    Storage,
    /// A session's parser was parked mid-escape-sequence with the rest still
    /// queued on its PTY (a flood: coloured log, redrawing UI, image transfer).
    ///
    /// Timing, never a refusal, and never carried: the successor's fresh parser
    /// would print the queued tail as text. The launched lane re-parks at the
    /// gate's next 20 ms re-run on the same rung ([`mid_sequence_miss_disposition`]);
    /// the fork lane files a miss at once. A mid-sequence parser over a quiet PTY is
    /// carried.
    ///
    /// Names only a partial sequence no carry holds: a hooked DCS query
    /// (`Terminal::partial_sequence_uncarried`), one the carry would swallow
    /// (`Terminal::partial_sequence_swallowed`), or, on a hop to a successor
    /// older than the output-state carry, engine state it would resume without:
    /// an open OSC 8 link or a pending VT52 address
    /// (`Terminal::output_state_in_flight`). Other partial sequences ride
    /// `TerminalCheckpoint::parser`, and the engine's in-flight output state
    /// `TerminalCheckpoint::output_state`.
    MidSequence { local_id: u64, state: &'static str },
    /// The build's own predicate refused the committed set: too many sessions,
    /// a duplicate id, or (pinned unreachable) its own blank screen.
    Refused {
        local_id: Option<u64>,
        cause: String,
    },
}

#[cfg(unix)]
impl std::fmt::Display for CaptureFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deadline { freeze_ms } => write!(
                formatter,
                "bounded visible-screen capture exceeded {freeze_ms} ms"
            ),
            Self::EngineBusy => formatter
                .write_str("a terminal engine stayed busy for the whole handoff capture window"),
            Self::Storage => {
                formatter.write_str("visible checkpoint set could not reserve bounded storage")
            }
            Self::MidSequence { local_id, state } => write!(
                formatter,
                "session {local_id}'s parser was parked mid-sequence ({state}) with the rest of \
                 that sequence still queued on its PTY; the park retries on a sequence boundary"
            ),
            Self::Refused { cause, .. } => formatter.write_str(cause),
        }?;
        formatter.write_str("; update handoff stayed in place")
    }
}

/// The park's capture: every session's committed screen, the control carries that
/// travel with them, `screen_digest` over exactly those screens (computed by the
/// capture's self-check), and the sessions carried below the exact rungs, which the
/// successor must make redraw (`ScreenCarry::repaint`).
#[cfg(unix)]
struct ParkedScreens {
    screens: Vec<(u64, aterm_core::terminal::TerminalCheckpoint)>,
    carries: Vec<crate::handoff_carry::CarrySource>,
    screen_digest: [u8; 32],
    repaint: Vec<u64>,
    /// Every handed session's foreground holder (`Session::fg_holder`), read
    /// under the park, whatever rung its screen is carried at. The lanes stamp
    /// it on the manifest records ([`stamp_fg_holders`]).
    fg_holders: Vec<(u64, i32)>,
    /// The engine's render debt of every handed session that owes rows on the
    /// alternate screen and whose screen is carried (`Terminal::render_debt`),
    /// read under the same lock as the checkpoint. The lanes stamp it on the
    /// manifest records ([`stamp_render_debts`]).
    render_debts: Vec<(u64, (i32, i32))>,
    /// Every handed session's history fence under the park
    /// (`handoff_history::capture_head`), what the worker joins its export to.
    history_heads: Vec<crate::handoff_history::HistoryHead>,
    /// Every held pane's visible screen the capture had the time and the
    /// budget for ([`App::capture_held_screens`]).
    held: Vec<crate::seamless::HeldScreen>,
}

/// Why the in-session overlap handoff cannot run in this process. One predicate
/// ([`App::seamless_handoff_unavailable`]) serves both the apply gate in
/// `start_unix_update_handoff` and the status bar's posture
/// (`App::apply_posture_for`), so the bar never promises what the gate refuses.
/// With any of these the admission classifier (`native_update_admission::classify`)
/// admits only the cold lane, at zero live PTYs: the apply is refused while any
/// terminal is open and the build lands at the next launch.
///
/// Fixed control sockets support overlap: the candidate's control worker binds
/// only after Commit and the parent's exit. There is no opt-out (owner,
/// 2026-09-23: every update seamless, and no env vars); every reason is a fact
/// about the launch or the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandoffUnavailable {
    /// A fixed path is configured, but this process never proved it bound it.
    UnownedControlSocket,
    /// `--headless`: no window to hand across.
    Headless,
    /// No event-loop proxy to drive the handoff's wakes — only
    /// `headless_for_test` builds such an App.
    NoEventLoopProxy,
}

impl HandoffUnavailable {
    /// Every reason, in the order the predicate asks: the launch shape, then the
    /// process shape.
    pub(crate) const ALL: [Self; 3] = [
        Self::UnownedControlSocket,
        Self::Headless,
        Self::NoEventLoopProxy,
    ];

    /// What is set: the actionable clause, which every sentence leads with because
    /// the status bar truncates from the right.
    #[must_use]
    pub(crate) fn cause(self) -> &'static str {
        match self {
            Self::UnownedControlSocket => {
                "the configured control socket is not owned by this process"
            }
            Self::Headless => "--headless (no window)",
            Self::NoEventLoopProxy => "no event loop",
        }
    }

    /// The log line's remedy, for the reasons that have one.
    #[must_use]
    #[cfg(unix)]
    pub(crate) fn remedy(self) -> &'static str {
        match self {
            Self::UnownedControlSocket => {
                " Resolve the control-socket ownership conflict before updating."
            }
            Self::Headless => " A windowed launch restores the default.",
            Self::NoEventLoopProxy => "",
        }
    }

    /// Whether this reason holds for `app` — a plain read each time.
    fn holds_for(self, app: &App) -> bool {
        match self {
            Self::UnownedControlSocket => {
                use aterm_types::control_socket::SocketDirective;
                match crate::cli::launch_flags().socket_directive() {
                    SocketDirective::Explicit(path) => {
                        let plan = crate::control_auth::SocketPlan {
                            sock_path: path.to_string(),
                            token_path: crate::control_auth::token_path_for_socket(&path),
                            latest_link: None,
                        };
                        !crate::control_socket_identity::published()
                            .is_some_and(|identity| identity.matches_plan_path(&plan))
                    }
                    _ => false,
                }
            }
            Self::Headless => app.headless,
            Self::NoEventLoopProxy => app.proxy.is_none(),
        }
    }
}

/// A TEST'S PARK MUST LAND: the freeze budget is a product constant (a
/// quarter second for an explicit apply), not a hang detector, and a debug
/// build on a loaded machine misses it now and then. So under `cfg(test)` the
/// prelaunched park turns its budget into a one-minute deadline
/// ([`park_budget_for_test::deadline_budget`], read where the park stamps its
/// deadline): it lands on any machine and only a hang fails it. The budget
/// itself (`App::prelaunched_park_budget`) stays the real rung. A test whose
/// subject is a MISS holds [`park_budget_for_test::real`] around that park,
/// and the park then runs on the real rung. Per thread, as the park runs on
/// the test's own thread, so no parallel test sees it.
#[cfg(test)]
pub(crate) mod park_budget_for_test {
    use std::cell::Cell;
    use std::time::Duration;

    thread_local! {
        static REAL: Cell<bool> = const { Cell::new(false) };
    }

    /// A test park's deadline unless [`real`] is held: a hang detector.
    pub(crate) const HANG_DETECTOR: Duration = Duration::from_secs(60);

    /// Holds the real budget on this thread until dropped.
    #[must_use = "the park runs on the real budget only while the guard is held"]
    pub(crate) struct Real(());

    impl Drop for Real {
        fn drop(&mut self) {
            REAL.with(|real| real.set(false));
        }
    }

    pub(crate) fn real() -> Real {
        REAL.with(|real| real.set(true));
        Real(())
    }

    /// The budget the park's deadline is stamped from: the hang detector, or
    /// `budget` itself while [`real`] is held.
    pub(super) fn deadline_budget(budget: Duration) -> Duration {
        if REAL.with(Cell::get) {
            budget
        } else {
            HANG_DETECTOR
        }
    }
}

impl App {
    /// Whether the overlap handoff can run here: the first true reason in
    /// [`HandoffUnavailable::ALL`]'s order, or `None`. Read only by the apply gate
    /// (`start_unix_update_handoff`) and the status bar's posture
    /// ([`App::apply_posture_for`]). Plain reads only (environment, two fields, the
    /// bound-socket witness): no filesystem probes and no memo that could call back
    /// into its owner.
    ///
    /// Not folded into `arm_native_auto_apply`'s `enabled`: the cold lane is a real
    /// automatic path once the last terminal of such a process has closed.
    #[must_use]
    pub(crate) fn seamless_handoff_unavailable(&self) -> Option<HandoffUnavailable> {
        HandoffUnavailable::ALL
            .into_iter()
            .find(|why| why.holds_for(self))
    }
}

impl App {
    /// Applies a staged update now by re-executing; the staged build swaps in at the
    /// top of the new `main` (`apply_staged_if_ready`). Reached from
    /// `Wake::ApplyStagedUpdate` (`aterm-ctl update apply` or the update-ready
    /// nudge). A no-op unless a strictly newer build is staged.
    ///
    /// Hands every live PTY master, its visible-screen checkpoint and a
    /// `SessionHandoff` manifest to the new process so shells survive (proven by
    /// `handoff_roundtrip_model` and `seamless_nonce_model`). Modes, cursors, output
    /// queued after the park and the whole scrollback cross: the checkpoint carries
    /// up to `seamless::MAX_HANDOFF_HISTORY_LINES`, and older history rides a fenced
    /// sidecar (`crate::handoff_history`). History that moved under its export or
    /// fails its sha crosses with the checkpoint's lines alone, and the lost lines
    /// are counted (`history_lost=` on `status`, and on the band). A cold relaunch is
    /// permitted only when no foreground job would be destroyed. Every path that
    /// leaves this process alive returns an actionable failure so the updater
    /// reducer can re-arm the verified stage.
    pub(crate) fn apply_staged_update_now(
        &mut self,
        safety_token: crate::app_native::NativeUpdateSafetyToken,
        mode: crate::native_updater_service::ApplyMode,
        apply_attempt: Option<crate::native_updater_service::ApplyAttemptTicket>,
        same_image: Option<SameImageHandoff>,
    ) -> Result<(), crate::UpdateHandoffStartError> {
        if self.update_handoff_in_flight() {
            return Err(crate::UpdateHandoffStartError::failed(
                "an update handoff is already in flight",
            ));
        }
        let build = crate::build_info::build_number()
            .parse::<u64>()
            .unwrap_or(0);
        // QA seam: `ATERM_DEBUG_SEAMLESS_REEXEC=1` re-execs the same binary through
        // the full seamless handoff and adopt path, testing shell survival without a
        // release. Derived from the environment alone: naming it without the
        // variable grants nothing.
        let same_image = match same_image {
            Some(SameImageHandoff::DebugSeam) | None => {
                crate::app_update_screen::debug_seamless_reexec_armed()
                    .then_some(SameImageHandoff::DebugSeam)
            }
        };
        if same_image.is_none() && apply_attempt.is_none() {
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
        // Re-exec this binary; the staged build swaps in at the top of the new
        // `main`, and `exec` does not return on success. `ATERM_UPDATED_FROM` hands
        // the old build number to the successor (read and cleared at startup, so it
        // never reaches shell children).
        #[cfg(unix)]
        {
            // The caller clears the row: `start_unix_update_handoff` shows
            // "installing…" before building the carry, and several `Err` exits return
            // without a completion, so binding the result here covers every such
            // exit.
            let started = self.start_unix_update_handoff(
                exe,
                build,
                safety_token,
                mode,
                apply_attempt,
                same_image,
            );
            // A PERSON'S PRESS HELD FOR THE CANDIDATE'S READ is no stop (round
            // eight, U35's review): its row keeps saying it installs and its
            // switch stays on record, for the press that runs when the read ends
            // (`App::run_held_person_apply`).
            if let Err(error) = &started
                && error.held_for_read_of().is_none()
            {
                self.retire_update_installing();
                // The automatic lane's capture refusal is surfaced as its own
                // outcome, whose record says this stop (one record per attempt).
                if !(error.is_capture_refused() && mode.is_automatic()) {
                    self.record_update_switch_stopped(
                        error.stops_routinely(mode),
                        &error.to_string(),
                    );
                }
            }
            return started;
        }
        // Windows has no exec(2); spawn the new process, then exit. Unreached while
        // the updater is macOS-only (`staged` is None).
        #[cfg(windows)]
        {
            // One cold replacement lane: an automatic and a manual apply replace alike.
            let _ = mode;
            let live_ptys = self.pool.iter().count();
            let facts = crate::native_update_admission::AdmissionFacts {
                staged_verified: same_image.is_some() || apply_attempt.is_some(),
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
                    // Refusal, not failure — the unix arm's law.
                    return Err(crate::UpdateHandoffStartError::refused(
                        reason.message(facts),
                    ));
                }
                _ => {
                    return Err(crate::UpdateHandoffStartError::refused(
                        "Windows update replacement requires an exact zero-session state",
                    ));
                }
            }
            let operator_quiesce = match self.operator_control.as_ref() {
                Some(control) => Some(control.try_begin_update_quiesce().map_err(|refusal| {
                    crate::UpdateHandoffStartError::operator_refused(
                        refusal,
                        "could not quiesce for replacement",
                    )
                })?),
                None => None,
            };
            self.shutdown_title_summaries();
            // The message log's last write from this process: the
            // `process::exit` after a successful spawn runs no destructor,
            // so the writer thread's queue would die with it (design §3.7).
            self.flush_messages_log();
            let spawn = || {
                // On Windows the successor is this image again, and when that is
                // the CONSOLE image (`aterm.exe --headless`, the in-process
                // fallback window) started bare by a process with no console, it
                // opened a visible console window for its whole life:
                // `spawnfd::command` gives it this process's console or a
                // windowless one.
                let mut cmd = aterm_uds::spawnfd::command(exe);
                // Forward argv minus the leading `--window` pins earlier boot
                // swaps prepended — every successor lane strips them so no
                // relaunch path can re-grow the argv (aterm-update's
                // reexec_forwarded_args doc has the accumulation story).
                cmd.args(aterm_update::reexec_forwarded_args(
                    std::env::args_os().skip(1),
                ))
                .env("ATERM_UPDATED_FROM", build.to_string());
                bind_expected_update_artifact(&mut cmd, apply_attempt.as_ref());
                // `--headless` rides the forwarded argv like every launch flag.
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
                Err(refusal) => {
                    self.reconfigure_title_summaries();
                    return Err(crate::UpdateHandoffStartError::operator_refused(
                        refusal,
                        "revoked replacement",
                    ));
                }
            }
        }

        #[allow(unreachable_code)]
        Err(crate::UpdateHandoffStartError::failed(
            "process replacement returned without applying the update",
        ))
    }

    /// The cold lane's steps before the commit permit: the switch on record, the
    /// title-summary worker stopped, and the message log's last write. Each is
    /// undone or harmless if the `exec` does not happen
    /// ([`Self::resume_after_cold_exec`]). The control socket is retired inside the
    /// permit ([`Self::run_cold_exec`]).
    #[cfg(unix)]
    pub(crate) fn prepare_cold_exec(&mut self, build: u64, same_image: bool, automatic: bool) {
        self.note_update_switch_started(build, same_image, automatic);
        self.shutdown_title_summaries();
        // The message log's last write from this process: `exec` runs no
        // destructor. The writer stays open for a failed `exec`. Bounded, since a
        // saturated machine can starve the utility writer thread while the window
        // is frozen.
        self.flush_messages_log_within(MESSAGES_FLUSH_COMMIT_WAIT);
        // The consent sentinel's `log stream` child is signalled by pid (a
        // non-blocking kill; its worker reaps it): `exec` runs no destructor,
        // and a quiet stream would otherwise outlive this process until the
        // next dialog. The 15-minute `--timeout` on every generation bounds it
        // even if this never ran. A failed `exec` re-arms the observer's start
        // ([`Self::resume_after_cold_exec`]).
        self.consent_observer.stop();
    }

    /// The cold `exec`, under the resident operator's commit permit when there is
    /// one. The control socket is retired inside the permit as the last step, so a
    /// refused permit leaves every connection served. `Ok` carries the error of an
    /// `exec` that returned, `Err` the permit's refusal; either way
    /// [`Self::resume_after_cold_exec`] follows and reopens the socket if it was
    /// retired.
    #[cfg(unix)]
    pub(crate) fn run_cold_exec(
        &self,
        quiesce: Option<&crate::operator_host::UpdateQuiesce>,
        exec: impl FnOnce() -> std::io::Error,
    ) -> Result<std::io::Error, crate::operator_host::UpdateQuiesceRefusal> {
        let retire_then_exec = || {
            // Last, so the socket is deaf for the shortest time: `exec` completes
            // only after every other thread is terminated (seconds on a saturated
            // machine). Closed here, a new client is refused at once and a held
            // connection reads EOF; the successor binds the same path.
            let retired = self.control_endpoint.retire();
            aterm_log::info!(
                "update apply: control socket retired for the exec (listener closed: {}, \
                 connections shut down: {})",
                retired.listener,
                retired.connections
            );
            exec()
        };
        match quiesce {
            Some(quiesce) => quiesce.with_commit_permit(retire_then_exec),
            None => Ok(retire_then_exec()),
        }
    }

    /// A cold `exec` that did not happen (the permit refused, or it returned an
    /// error) leaves this process running: the control socket serves again at its
    /// path with its token if it was retired, and the title-summary worker is
    /// recreated.
    #[cfg(unix)]
    pub(crate) fn resume_after_cold_exec(&mut self) {
        if !self.control_endpoint.reopen() {
            aterm_log::warn!(
                "update apply: the control socket could not be reopened after the exec \
                 returned; its watchdog rebinds it"
            );
        }
        self.reconfigure_title_summaries();
        // The window carries on: its consent observer starts again at the next
        // park.
        self.consent_observer_started = false;
    }

    /// The control-socket pair a [`HandoffWorkerCleanup`] republishes after the
    /// overlap resolves: `Some((latest_link, sock_path))` only while the socket is
    /// bound and the plan mints a latest link. Shared by the worker and
    /// emergency-reaper cleanups.
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
        same_image: Option<SameImageHandoff>,
    ) -> Result<(), crate::UpdateHandoffStartError> {
        // Every session with a live process behind it; a pane kept open after
        // its command exited is not handed (`handoff_live_sessions`).
        let live: Vec<(u64, i32, i32)> = self.handoff_live_sessions();
        // This epoch gates the entry, not the flight: `automatic_activity_epoch` is
        // read only before the park (the quiet-epoch admission below and the
        // pre-park re-check). Mid-flight revocation is `pending.activity_epoch`,
        // armed unconditionally and read at Commit as `exact_activity`. So
        // `AutomaticPastGrace` skips waiting for a quiet moment but can still be
        // revoked by activity.
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
        // One predicate for the apply gate and the status bar's posture
        // (`seamless_handoff_unavailable`), and every reason is logged by name:
        // without the overlap only the cold lane remains, which the classifier
        // admits at exactly zero PTYs, so with any terminal open the apply is
        // refused (`LivePtysNeedSeamless`) and the build waits for the next launch.
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
        // Nothing to hand over but something to lose: with every live session left
        // out (`handoff_live_sessions`), the cold lane's `exec` would close the
        // exited panes kept open (`--hold`). A typed refusal instead, retried on the
        // block cooldown and gone once a pane is closed or a live tab opens.
        if live.is_empty() && !self.handoff_exited_session_ids().is_empty() {
            return Err(crate::UpdateHandoffStartError::refused(
                "every open terminal pane's command has exited (panes kept open by --hold); \
                 the update applies once one is closed or a new tab opens, because replacing \
                 the app now would close them",
            ));
        }
        let overlap_available = handoff_unavailable.is_none();
        let facts = crate::native_update_admission::AdmissionFacts {
            staged_verified: same_image.is_some() || apply_attempt.is_some(),
            seamless_capable: overlap_available && !live.is_empty(),
            native_state_certified: safety_token.is_certified(),
            live_ptys: live.len(),
            foreground_jobs,
            unknown_foregrounds,
        };
        let decision = crate::native_update_admission::classify(facts);
        // WHAT THE TOKEN SAYS RIDES MUST BE CARRIED BY THE LANE THAT RUNS. The
        // preflight let unsaved editor drafts, Settings drafts and failed
        // checkpoints ride because a successor reopens them from the handed-over
        // layout; the lane is only decided here. A cold exec hands no layout
        // over, and an OLDER successor ignores the layout field that carries
        // Settings drafts — so either refuses them, in the preflight's
        // person-facing shapes, and the row says what to do (plan P2-2).
        //
        // The ONE target build of this attempt, settled here: the carry check,
        // the cold lane's `exec`, the park's raise and the seamless lane's
        // decision all read this binding, so none of them can call a target
        // older that another calls newer (round eight's reviews of #3).
        let target_build = apply_attempt
            .as_ref()
            .map_or(build, |attempt| attempt.target_build());
        if let crate::native_update_admission::AdmissionDecision::Apply(lane) = decision
            && let Some(refusal) = lane_carry_refusal(&safety_token, lane, build, target_build)
        {
            return Err(crate::UpdateHandoffStartError::refused(refusal));
        }
        match decision {
            crate::native_update_admission::AdmissionDecision::Block(reason) => {
                // A Block is a refusal to attempt, never a failed attempt (see
                // [`crate::UpdateHandoffStartError::Refused`]); counting it as a
                // failure would inflate the failed-apply count.
                return Err(update_admission_refusal(reason, facts, handoff_unavailable));
            }
            crate::native_update_admission::AdmissionDecision::Apply(
                crate::native_update_admission::ApplyLane::Cold,
            ) => {
                // Direct exec is authorized only for an exact zero-PTY state.
                debug_assert!(live.is_empty());
                // THE VERDICT FIRST, BEFORE ANYTHING IS WRITTEN OR REPLACED (round
                // eight, #1): the cold lane's successor is THIS build, whose boot
                // swap then runs the complete gate — and a refusal there came back
                // as a fresh desk that re-armed and exec'd again, every few seconds.
                self.cold_lane_verdict_gate(mode, apply_attempt.as_ref())?;
                // A harness's lifeline crosses the exec on a descriptor kept open
                // for it (round eight, #28); the number it was launched with is
                // `/dev/null` or closed by now.
                let lifeline_fd = crate::lifeline::relaunch_descriptor();
                let mut command = std::process::Command::new(exe);
                command
                    // Exactly one `--window` pin, then the window's flags: the
                    // router chooses window or session before any boot swap runs,
                    // and a window started from a shell keeps that terminal on
                    // stdin, so an unpinned exec came back as a nested SESSION
                    // (round-seven #11). Neither a windowing verb nor a stale
                    // lifeline number rides along (round eight, #6/#28;
                    // `running_window_successor_args`); the `-e` command does, for
                    // the tabs the window opens, and the mark says it already ran
                    // in the first one (#7's review; `RELAUNCH_OF_RUNNING_ENV`).
                    .args(aterm_update::running_window_successor_args(
                        std::env::args_os().skip(1),
                        lifeline_fd,
                    ))
                    .env("ATERM_UPDATED_FROM", build.to_string())
                    .env(aterm_update::RELAUNCH_OF_RUNNING_ENV, "1");
                // Same rule as the seamless lane below: an ACTIVATION binds no
                // expected artifact. The successor swaps nothing; binding the
                // activation digest would make its `apply_staged_if_ready` refuse a
                // newer `ready.toml` as "no longer matches" and write a spurious
                // apply refusal into the ledger.
                let bound = apply_attempt
                    .as_ref()
                    .filter(|attempt| !attempt.is_installed_activation());
                bind_expected_update_artifact(&mut command, bound);
                // …and what this lane has booked against those bytes, so a relaunch
                // whose boot swap refuses again books that refusal where the
                // schedule stands instead of as a first failure (round eight, #1's
                // review; `ColdLaneBudget`).
                bind_cold_lane_budget(
                    &mut command,
                    bound,
                    bound.map(|attempt| self.cold_lane_budget(attempt, std::time::Instant::now())),
                );
                // …and what its warnings had said, so the landing records their
                // healing (round eight, finding 79).
                bind_cold_lane_health_said(&mut command, &self.carried_health_said());
                // Headless survives the re-exec with no help: `--headless` is a
                // launch flag, and the successor's argv carries every one.
                //
                // A window of document and Settings tabs is left to the successor as
                // the quit layout it reads at boot, or, where it could not be
                // reopened, the lane refuses as the `--hold` case above does.
                let layout = self
                    .cold_lane_layout()
                    .map_err(crate::UpdateHandoffStartError::refused)?;
                // Stamped with this run, as a quit's layout is, so the successor's
                // claim sets this run's journal aside by name and never reopens the
                // desk twice.
                let run = self
                    .crash_journal
                    .run()
                    .map(crate::crash_journal::JournalId::stem);
                // The write waits a bounded time for the restore lock: this is
                // the GUI main thread (round eight, U49). A layout written for an
                // exec that then does not happen is taken back, this run's desk
                // alone (`App::take_back_cold_lane_layout`).
                let left = leave_cold_lane_layout(layout.as_ref(), |layout| {
                    crate::restore::write_quit_within(
                        layout,
                        run.as_deref(),
                        COLD_LANE_LAYOUT_LOCK_WAIT,
                    )
                })?;
                // This run's desk, written again: it replaced the one a take-back
                // still owed from an earlier attempt (round eight, U49's review).
                // With no newer desk of this run written (nothing to leave, or a
                // layout not kept), the successor would reopen that stale one at
                // its boot: the owed take-back is tried first, as a park tries it.
                if left == LayoutLeft::Written {
                    self.cold_lane_layout_rewritten();
                } else if let Err(refusal) =
                    self.settle_owed_layout_take_back_before_switch(std::time::Instant::now())
                {
                    return Err(crate::UpdateHandoffStartError::refused(refusal));
                }
                // THE LAUNCHER'S SOFT DESCRIPTOR LIMIT crosses this exec as it
                // crosses the fork lane's (round eight of the update audit, #81):
                // `execve` keeps a limit a claim or a park raised, and the new
                // image's shells would get the raise without it.
                #[cfg(target_os = "macos")]
                crate::handoff_rendezvous::carry_launcher_limit(&mut command);
                let operator_quiesce = match self
                    .operator_control
                    .as_ref()
                    .map(crate::operator_host::ControlHandle::try_begin_update_quiesce)
                    .transpose()
                {
                    Ok(quiesce) => quiesce,
                    Err(refusal) => {
                        if left == LayoutLeft::Written {
                            self.take_back_cold_lane_layout(run, std::time::Instant::now());
                        }
                        return Err(crate::UpdateHandoffStartError::operator_refused(
                            refusal,
                            "could not quiesce for replacement",
                        ));
                    }
                };
                // TABS NOT KEPT, ON RECORD ONCE THE SWITCH IS GRANTED (round eight,
                // U48's review): just before the steps whose message-log flush
                // carries it. Recorded before the quiesce, every attempt a busy
                // operator turned back said the new version "starts without them"
                // while the old one ran on, one record per retry.
                if let LayoutLeft::NotKept(cause) = &left {
                    self.say_cold_lane_tabs_not_kept(target_build, cause);
                }
                self.prepare_cold_exec(build, same_image.is_some(), mode.is_automatic());
                // `exec` returns only on failure.
                let why = match self.run_cold_exec(operator_quiesce.as_ref(), || {
                    // The table the successor gets: the lifeline, inheritable for
                    // the `exec` alone (close-on-exec again if it returns, so
                    // nothing this process spawns next holds it), and NOTHING
                    // ELSE this process holds (`cold_exec_hand_over`).
                    let _lifeline = cold_exec_hand_over();
                    // AN OLDER TARGET'S IMAGE STARTS AT THE LIMIT THIS PROCESS
                    // HELD BEFORE A PARK'S RAISE (round eight's second review of
                    // #3): `execve` keeps the raise, and a build before 06b06e74e
                    // ignores the launcher's limit named above. Lowered for the
                    // `exec` alone, after the lifeline is made inheritable (so it
                    // counts), and raised back if the `exec` returns.
                    #[cfg(target_os = "macos")]
                    let _older_image =
                        crate::handoff_rendezvous::lower_for_older_image(target_build, build);
                    // SIGPIPE is put back to the ignored disposition
                    // `Command::exec` reset if the `exec` returns.
                    aterm_update::exec_keeping_sigpipe(&mut command)
                }) {
                    Ok(error) => crate::UpdateHandoffStartError::failed(format!(
                        "process replacement failed: {error}"
                    )),
                    Err(refusal) => crate::UpdateHandoffStartError::operator_refused(
                        refusal,
                        "revoked replacement",
                    ),
                };
                if left == LayoutLeft::Written {
                    self.take_back_cold_lane_layout(run, std::time::Instant::now());
                }
                self.resume_after_cold_exec();
                return Err(why);
            }
            crate::native_update_admission::AdmissionDecision::Apply(
                crate::native_update_admission::ApplyLane::Seamless,
            ) => {}
        }

        // The commit the adoption proof will name, asked before anything is
        // launched or parked. The proof hashes the successor's commit stamp, so a
        // target with none (the same-image seam on a build without git facts) can
        // never prove; it is refused here and names its cause.
        let target_commit = apply_attempt.as_ref().map_or_else(
            || crate::build_info::git_commit().to_string(),
            |attempt| attempt.target_commit().to_string(),
        );
        if let Err(refusal) = crate::seamless::handoff_commit_stamp(&target_commit) {
            return Err(crate::UpdateHandoffStartError::refused(format!(
                "{refusal}; the terminal was left untouched"
            )));
        }
        // THIS RUN'S DESK FROM A COLD ATTEMPT THAT DID NOT `exec`, STILL OWED ITS
        // TAKE-BACK (round eight, U49's review): the switch ends this run, and a
        // desk left in `session.toml` would read as another instance's, kept by
        // every later quit and reopened by the next cold launch beside the
        // windows it stands for. Taken back now; a refusal that passes refuses
        // the attempt, retried with nothing touched.
        if let Err(refusal) =
            self.settle_owed_layout_take_back_before_switch(std::time::Instant::now())
        {
            return Err(crate::UpdateHandoffStartError::refused(format!(
                "{refusal}; the terminal was left untouched"
            )));
        }

        // Pre-verification of the staged candidate: codesign policy and sealed
        // build/commit rebinding, bound to the authorized artifact. Additive: the
        // child re-runs the complete gate at swap time under the apply lock, so
        // `Ok` here is a latency and warm-cache optimization, never a grant.
        //
        // The check (`codesign --deep` plus a bundle flock) is unbounded disk work,
        // so it runs as the worker's first action (`run_handoff_worker`), not on
        // the main thread. A failing candidate is rolled back via
        // `PreparationFailed` (Structural). The same-binary debug re-exec has no
        // staged bundle (`verify_staged_candidate: false`).
        //
        // …AND BETTER STILL, OUT OF THE PARKED WINDOW ENTIRELY: when this exact
        // artifact already passed `spawn_staged_handoff_preverification` while
        // every reader was still live, the worker skips the repeat and the
        // parked interval shrinks by the whole `codesign --deep` cost. A cached
        // REFUSAL short-circuits the attempt right here, before anything parks.
        // Never an authorization either way — the child re-runs the complete
        // gate under the apply lock at swap time.
        if mode.is_automatic()
            && let Some(attempt) = apply_attempt.as_ref()
        {
            self.answer_with_refused_cold_relaunch(attempt);
        }
        let preverified = apply_attempt
            .as_ref()
            .and_then(|attempt| self.cached_handoff_preverification(attempt));
        // Only the automatic modes honour a cached refusal: an explicit apply may
        // follow the person restoring the signed bundle, so the worker re-verifies.
        // A cached pass is honoured by every mode, since it only shrinks the park.
        if preverified == Some(false) && mode.is_automatic() {
            // The verifier's cause, not a generic verdict: an installed bundle
            // failing the signing policy (a local build in /Applications) is the
            // owner's to fix, and only the message says so.
            let reason = apply_attempt
                .as_ref()
                .and_then(|attempt| self.cached_handoff_preverification_reason(attempt))
                .unwrap_or_else(|| "the staged update failed verification".to_string());
            // The wait for a roster confirmation is no verdict (round eight, #503's
            // second review): held for the check, never booked on the transient
            // budget, as the cold lane's gate holds it.
            if aterm_update::is_roster_confirmation_wait(&reason)
                && let Some(attempt) = apply_attempt.as_ref()
            {
                if let Some(dmg_sha256) =
                    crate::app_native::decode_dmg_sha256(attempt.target_dmg_sha256())
                {
                    let _ = self.hold_for_roster_confirmation(
                        attempt.target_build(),
                        dmg_sha256,
                        &reason,
                        std::time::Instant::now(),
                        true,
                        true,
                    );
                }
                return Err(crate::UpdateHandoffStartError::activity(format!(
                    "{reason}; the terminal was left untouched"
                )));
            }
            return Err(crate::UpdateHandoffStartError::failed(format!(
                "{reason}; the terminal was left untouched"
            )));
        }
        let verify_staged_candidate = same_image.is_none() && preverified != Some(true);

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
        let Some(reconcile_worker) = self.native_update_reconcile_worker.clone() else {
            return Err(crate::UpdateHandoffStartError::failed(
                "updater reconciliation worker is unavailable",
            ));
        };
        // ROOM FOR THE PARK, BEFORE EITHER LANE RESERVES ANYTHING (round eight of
        // the update audit, #3): a window the Dock started runs at launchd's
        // soft 256, and both lanes duplicate every master on top of the three
        // descriptors each session already holds — past 256 from about 60
        // sessions, so the desks the chunked grant exists for could never
        // park. A forked successor inherits the room and is told the
        // launcher's limit; new tabs keep getting the launcher's. Only toward
        // a target no older than this build, which reads that told limit. An
        // older one (the fork lane's, or the cold lane's) is not raised for
        // here, and a raise an earlier attempt left is not handed to it: its
        // image starts at the limit this process held before that raise
        // (`start_older_image_at_its_limit`, `lower_for_older_image`).
        #[cfg(target_os = "macos")]
        crate::handoff_rendezvous::raise_descriptor_limit_for_park(target_build, build);
        // Parent master flags are an invariant, not mutable handoff state. Child
        // inheritance is installed later by `pre_exec` on child copies only.
        for (_, master, _) in &live {
            if aterm_pty::set_cloexec(*master, true).is_err() {
                return Err(crate::UpdateHandoffStartError::failed(
                    "could not enforce CLOEXEC on every parent PTY master",
                ));
            }
        }

        // Apply begins on the status bar, before the carry is built, so the
        // successor inherits the row count and words on screen. A visible update
        // row changes its words ("Updating to aterm vX · installing…"), a repaint.
        // With no row up, an explicit apply adds one here, before the carry reads
        // `ws.rows` and `exact_layout` freezes, so the successor is sized for it.
        // The automatic lane never adds a row (its SIGWINCH would read as activity
        // and revoke); the upgrade rim, started where each lane's readers park
        // (`charge_upgrade_rim_at_park`), alone explains the frozen frame. Every
        // refusal from here restores the words and ends the rim, synchronously
        // via `retire_update_installing` or after the park via
        // `reduce_returned_handoff_completion`.
        self.begin_update_installing(matches!(
            mode,
            crate::native_updater_service::ApplyMode::Immediate
        ));
        // On record before the park: the park's budget has no room for a file write.
        self.record_update_switch_started(build, same_image.is_some(), mode.is_automatic());

        // Probed before the park: resolving the control directory can create and
        // `chmod` it, which must stay out of the 20 ms frozen interval. The answer
        // depends only on `$HOME` and this pid, so it cannot change meanwhile.
        #[cfg(target_os = "macos")]
        let rendezvous_path_fits = crate::handoff_rendezvous::rendezvous_path_fits();
        // Same reasoning, same window: `app_bundle_root` ends in an `is_dir`.
        #[cfg(target_os = "macos")]
        let bundle = app_bundle_root(&exe);

        // Provision the worker before readers park. A resource-exhausted thread
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

        // Hoisted out of the parked window: the attempt's identity, its successor
        // `Command`, and the lane decision with its proof identities depend only on
        // facts settled before the park (pool snapshot, dup'd masters, apply
        // ticket). Only the two digests over the captured screens and layout stay
        // under the park. This shortens the freeze and keeps the digests' deadline
        // check from being pushed past 20 ms; it does not change the capture
        // ladder's budget.
        //
        // The attempt id is minted before the park; an attempt dying after this
        // burns one id, and nothing keys on contiguity.
        //
        // These early returns do not roll back the overlap, since no reader is
        // parked. A return drops `job_tx`, which tells the spawned worker to exit.

        let attempt_id = self.next_update_handoff_id;
        let Some(next_attempt_id) = attempt_id.checked_add(1) else {
            return Err(crate::UpdateHandoffStartError::failed(
                "handoff identity space is exhausted",
            ));
        };
        self.next_update_handoff_id = next_attempt_id;
        let mut command = std::process::Command::new(exe);
        crate::control_socket_identity::bind_command(&mut command);
        command
            // One `--window` pin, as on the cold lane (round-seven #11): an
            // unpinned candidate of a shell-started window routed itself to a
            // terminal SESSION and never dialed, so every attempt timed out. No
            // windowing verb (round eight, #6): behind the pin it was the window
            // parser's exit 2. The `-e` command rides along for the tabs the
            // window opens, and the mark keeps a fresh bootstrap from running it
            // again (#7 and its review; `RELAUNCH_OF_RUNNING_ENV`). No lifeline
            // rides this lane — it is `--headless`'s, which never hands off
            // seamlessly.
            .args(aterm_update::running_window_successor_args(
                std::env::args_os().skip(1),
                None,
            ))
            .env("ATERM_UPDATED_FROM", build.to_string())
            .env(aterm_update::RELAUNCH_OF_RUNNING_ENV, "1");
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
        // THE LANE IS DECIDED HERE AND NOWHERE ELSE, and it has to be settled
        // before the pending attempt is recorded: it selects which term the
        // adoption proof hashes, and the main thread re-derives that proof from
        // the pending attempt at Commit time. A lane chosen later would leave
        // the two halves of one proof speaking different terms — the exact
        // failure that shows up as an unexplained `AdoptionMismatch`.
        //
        // Decided on the live snapshot: the launched lane duplicates its masters
        // and computes device-term identities at the park; the fork lane
        // duplicates them just below. The `fstat` probe answers the same for a
        // master and its duplicate, so lane and proof term agree.
        //
        // The chunked-grant fact is read once: only a passed verification on record
        // declares it, otherwise the one-message limit stands. The lane is chosen
        // on it and the launched successor is offered the grant on it
        // (`HandoffWorkerJob::successor_grant_chunks`).
        #[cfg(target_os = "macos")]
        let lane_grant_chunks = self.park_policy(apply_attempt.as_ref()).grant_chunks;
        #[cfg(target_os = "macos")]
        let lane = {
            let facts = HandoffLaneFacts {
                bundled: bundle.is_some(),
                // A field rather than an assumption so the refusal it guards is
                // reachable from a test.
                launcher_available: true,
                socket_path_fits: rendezvous_path_fits,
                target_not_older: target_build >= build,
                sessions: live.len(),
                grant_chunks: lane_grant_chunks,
                environment_is_a_merge: launch_environment(&command).is_some(),
            };
            match out_of_band_lane_refusal(facts) {
                None => {
                    if crate::handoff_rendezvous::proof_identities_in_device_terms(&live).is_some()
                    {
                        HandoffLane::OutOfBand
                    } else {
                        // A master that will not answer `fstat` has no device term,
                        // so fork: the fd-number term needs nothing from the kernel.
                        aterm_log::warn!(
                            "update apply: forking instead of launching — a handed-off PTY would \
                             not answer fstat, so the out-of-band proof term cannot be computed"
                        );
                        HandoffLane::Fork
                    }
                }
                Some(reason) => {
                    aterm_log::info!("update apply: forking instead of launching — {reason}");
                    HandoffLane::Fork
                }
            }
        };
        if self.update_handoff_activity_epoch == u64::MAX {
            return Err(crate::UpdateHandoffStartError::failed(
                "handoff activity identity space is exhausted",
            ));
        }

        // The launched lane leaves here with every reader live: its worker launches
        // the successor, holds its rendezvous claim, and asks this thread to park
        // once the successor has dialled (`park_and_transfer_to_prelaunched_successor`).
        // Everything below is the fork lane, which parks before it spawns.
        #[cfg(target_os = "macos")]
        if lane == HandoffLane::OutOfBand {
            return self.prelaunch_out_of_band_handoff(PrelaunchArgs {
                attempt_id,
                build,
                mode,
                apply_attempt,
                same_image,
                target_build,
                target_commit,
                command,
                verify_staged_candidate,
                grant_chunks: lane_grant_chunks,
                installed_activation,
                bundle,
                cancel,
                cancelled,
                job_tx,
                reconcile_worker,
                reconcile_ticket,
            });
        }

        // Rung 0's seed, read before the activity re-check and the park so this
        // ledger read stays out of the TOCTOU window. The fork lane has no successor
        // boot to time a dry run in, so it seeds from the ledger.
        let freeze_seed = FreezeSeed::from_prior(aterm_update::handoff_capture_prior().as_ref());

        // Capture the session registry before parking: this projection can fail
        // (`WouldBlock`), and must then return with the terminal untouched. No disk
        // I/O. The restore manifest is captured after the park (see below).
        //
        // The hold serials are read first: a hold moving before or after the
        // manifest's own read refuses the Commit (a lossless retry), and none can
        // land after the Commit's comparison, which reads the serials under a fence
        // (`fence_hold_serials`).
        let hold_serials = self.hold_serials();
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
        let manifest = handed_sessions_only(manifest, &live);
        // What the Commit compares again of each record, as drawn here
        // (`session_store::CarriedClaims`, round eight, finding 751).
        let carried_claims = carried_claims_of(&manifest);
        let Ok((owned_masters, adoption)) = reserve_child_only_masters(&live) else {
            return Err(crate::UpdateHandoffStartError::failed(
                "could not reserve child-only PTY descriptors",
            ));
        };
        let fds = crate::session_store::HandoffFds {
            entries: adoption.clone(),
        };
        let window = self.park_window_carry(target_build);
        // The fork lane's proof term is the descriptor number: `execve` copies
        // the table verbatim, so the number itself is what both sides compute.
        let proof_identities = adoption.clone();

        // Close the synchronous-preparation TOCTOU just before the first reader
        // stop. Activity defers automatic apply; an explicit apply bypasses only
        // this quiet policy.
        if automatic_activity_epoch.is_some_and(|epoch| {
            self.update_handoff_activity_epoch != epoch
                || !self.automatic_update_activity_quiet(std::time::Instant::now())
                || handoff_masters_have_activity(&live)
        }) {
            return Err(crate::UpdateHandoffStartError::activity(
                "input or PTY output arrived before automatic reader park",
            ));
        }
        // A hung-up master is not output, so the peek above does not catch it; it
        // is caught here before any reader stops, rather than by the post-park
        // death check after freezing the terminal. Gated on the lane, like that
        // check.
        if mode.is_automatic() && handoff_masters_closed(&live) {
            return Err(crate::UpdateHandoffStartError::activity(
                "a session's command has exited but its pane is still open",
            ));
        }
        // A live `video` take before `Land`: the launched lane's typed wait,
        // retried as activity. The fork lane has no hold to bound it; the ladder
        // reaches `Land` within `LANDS_WITHIN`, the park proceeds, and Commit aborts
        // the take with a reply.
        if let Some(reason) = recording_park_refusal(
            mode,
            self.automatic_phase_for(mode, std::time::Instant::now()),
            self.video_request_live(),
            std::time::Duration::ZERO,
        ) {
            return Err(crate::UpdateHandoffStartError::activity(reason));
        }
        // Once half the capture window is gone, remaining sessions carry only
        // their visible screen: history is optional and priced per line, so the
        // failure mode is less scrollback, never a missed update.
        //
        // The budget (`handoff_freeze_budget`) is keyed by the attempt's target
        // build, which is what the physical-retry record stores, plus this lane's
        // own park misses (`App::fork_park_misses`), which are not charged as
        // physical failures.
        let fork_park_misses = self.fork_park_misses_of(target_build);
        // The successor's policy, read before the park (a cache lookup). The fork
        // lane's only knob is the carry ceiling; its entry gate has no `Land` wait.
        let park_policy = self.park_policy(apply_attempt.as_ref());
        let ceiling = park_policy.carry_ceiling();
        if let Some(policy) = park_policy.policy {
            aterm_log::info!(
                "update apply: parking under build {target_build}'s handoff policy ({policy})"
            );
        }
        // Unknown, not absent: with no pass on record the worker reads the policy
        // after the park and refuses a capture it forbids (`capture_exceeds_policy`);
        // a failed capture starts the verification now.
        let policy_unread = apply_attempt.is_some() && same_image.is_none() && !park_policy.known;
        // An attempt — a person's included (round eight, U35: held, then run again
        // once the read ends) — first waits for the policy while it is read,
        // bounded by the read's ceiling: the capture runs on this thread, and a
        // projection that panics or hangs could otherwise never be routed around.
        if let Some(wait) = self.fork_lane_policy_wait(mode, target_build, policy_unread) {
            return Err(wait);
        }
        if policy_unread {
            aterm_log::info!(
                "update apply: build {target_build} has no verified pass on record, so its \
                 handoff policy is read after this park; a capture it forbids is not handed \
                 over"
            );
        }
        let prior_physical_failures = self
            .prior_physical_failures_of(apply_attempt.as_ref())
            .saturating_add(fork_park_misses);
        let freeze_budget = handoff_freeze_budget(mode, prior_physical_failures, freeze_seed);
        let freeze_ms = freeze_budget.as_millis();
        let handoff_history_comfort = freeze_budget / 2;
        aterm_log::info!(
            "update apply: freeze budget {freeze_ms} ms ({mode:?}, {prior_physical_failures} prior \
             physical failure(s) of the target artifact incl. {fork_park_misses} fork-lane park \
             miss(es); {freeze_seed}; running build {build})"
        );
        // The instant the terminal stops echoing: the start of the user's freeze,
        // the zero point of the numbers reported at Commit and of the capture
        // deadline — and of the rim that explains it.
        self.charge_upgrade_rim_at_park(build);
        let park_at = std::time::Instant::now();
        let deadline = park_at + freeze_budget;
        if !self.park_all_readers(deadline) {
            self.rollback_overlap(None, &live);
            return Err(self.fork_park_missed(
                target_build,
                format!("a PTY reader missed the {freeze_ms} ms handoff park deadline"),
            ));
        }
        // The post-park re-check tolerates activity: a burst consumed during the
        // park is in the engine and carried; later bytes wait in the kernel for
        // the child; hardware input queued meanwhile reaches the still-open masters
        // after this returns. The activity epoch cannot move here (every bump is on
        // this thread). Session death still rejects: a HUP/ERR master makes the
        // proof's live set stale. Classified as activity so the next attempt sees
        // the new set. Gated on the lane, not the epoch: death is a safety fact.
        if mode.is_automatic() && handoff_masters_closed(&live) {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::activity(
                "a PTY session closed during automatic reader park",
            ));
        }
        let ParkedScreens {
            screens,
            carries,
            screen_digest,
            repaint,
            fg_holders,
            render_debts,
            history_heads,
            held,
        } = match self.capture_parked_screens(
            &live,
            deadline,
            freeze_ms,
            handoff_history_comfort,
            crate::seamless::WireCaps::for_target(target_build),
            ceiling,
        ) {
            Ok(captured) => captured,
            Err(failure) => {
                self.rollback_overlap(None, &live);
                // The worker that would read the policy never runs for a failed
                // capture; read it now, off this thread, so the next park follows it.
                if policy_unread {
                    self.spawn_staged_handoff_preverification(target_build);
                }
                // The same classifier as the launched lane's park: a miss rides the
                // activity spacing, a refusal the refusal lane.
                return Err(match classify_capture_failure(&failure) {
                    ParkAttempt::Refused(refusal) => {
                        crate::UpdateHandoffStartError::capture_refused(refusal.to_string())
                    }
                    ParkAttempt::Missed(reason) => self.fork_park_missed(target_build, reason),
                    // The classifier answers only the two above; anything else
                    // it ever grows is timing until proven otherwise.
                    #[cfg(any(target_os = "macos", all(test, unix)))]
                    ParkAttempt::Parked
                    | ParkAttempt::NotYet(_)
                    | ParkAttempt::MissedMidSequence(_)
                    | ParkAttempt::Failed(_) => {
                        self.fork_park_missed(target_build, failure.to_string())
                    }
                });
            }
        };
        // The foreground holders ride the manifest records, whatever rung.
        let manifest = stamp_fg_holders(manifest, &fg_holders);
        let manifest = stamp_render_debts(manifest, &render_debts);
        let manifest = stamp_journal_keys(manifest, &self.journal_keys());

        // Captured post-park: every reader is joined and the capture loop proved
        // every `term` lockable, so `restore_session_meta`'s try_lock cannot degrade
        // cwd/title here. Captured before the park, a reader holding its term mutex
        // (or OSC 7 during the park) degraded the layout, `exact_layout` went false,
        // and the attempt was misreported as a topology change.
        //
        // After the capture loop rather than right after `park_all_readers`: if the
        // scrollback-compression worker still held a `term`, the loop already aborted
        // the attempt cleanly.
        let layout = self.capture_handoff_layout();

        let layout_digest = match commit_handoff_layout(&layout) {
            Ok(digest) => digest,
            Err(refusal) => {
                self.rollback_overlap(None, &live);
                return Err(crate::UpdateHandoffStartError::capture_refused(
                    refusal.to_string(),
                ));
            }
        };
        // The screen commitment was taken by the capture's self-check
        // (`seamless::settle_wire_carries`) over exactly `screens`.
        if std::time::Instant::now() >= deadline {
            self.rollback_overlap(None, &live);
            // It said "20 ms" whatever the rung was.
            return Err(self.fork_park_missed(
                target_build,
                format!("handoff proof capture exceeded the {freeze_ms} ms deadline"),
            ));
        }
        // The park landed within budget: keep its cost for the next update's first
        // rung, as the launched lane does.
        let _ledger_write = keep_landed_park(park_at.elapsed());
        let preverified =
            PreverifyPublisher::for_attempt(&self.handoff_preverified, apply_attempt.as_ref());
        let activity_epoch = self.update_handoff_activity_epoch;
        let arbiter = crate::HandoffAttemptArbiter::new();
        self.pending_update_handoff = Some(crate::PendingUpdateHandoff {
            attempt_id,
            park_at,
            proof_ready_at: None,
            // Sampled at the park: whether the user was looking at aterm when the
            // screen it will hand over was the one on glass.
            #[cfg(target_os = "macos")]
            activate_at_commit: self.any_os_window_focused(),
            nonce: None,
            live: live.clone(),
            // The PROOF identities, in whichever term this attempt's lane can
            // prove — see `HandoffWorkerJob::proof_identities`. Never `live`.
            adoption: proof_identities.clone(),
            child_pid: None,
            mode,
            apply_attempt,
            same_image,
            target_build,
            target_commit: target_commit.clone(),
            layout: layout.clone(),
            layout_digest,
            screen_digest,
            activity_epoch,
            hold_serials,
            carried_claims,
            cancel: cancel.clone(),
            arbiter: arbiter.clone(),
            teardown: crate::DeferredHandoffTeardown::None,
            commit_drain_started: None,
            commit_flush: None,
            held_commit: None,
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
            installed_activation,
            preverified,
            // The ceiling this capture was taken under, for the worker to check
            // against the policy it reads when no pass was on record.
            parked_under: Some(ceiling),
            // Learned by the worker's own verification of the candidate.
            successor_grant_chunks: false,
            command,
            // The fork lane parks first, so its capture travels with the job.
            capture: Some(HandoffCapture {
                #[cfg(target_os = "macos")]
                park_at,
                #[cfg(target_os = "macos")]
                captured_at: std::time::Instant::now(),
                manifest,
                fds,
                screens,
                repaint,
                carries,
                window,
                layout,
                layout_digest,
                screen_digest,
                live: adoption,
                proof_identities,
                _owned_masters: owned_masters,
                history_heads,
                // Exported by the worker after this capture, never beside the park
                // (`HistoryPlan::Deferred`). Withheld under a ceiling that carries
                // no scrollback, which the worker checks (`capture_exceeds_policy`).
                history: crate::handoff_history::HistoryPlan::deferred_under(ceiling, || {
                    self.history_sessions(target_build)
                }),
                held,
            }),
            #[cfg(target_os = "macos")]
            prelaunch: None,
            #[cfg(target_os = "macos")]
            lane,
            #[cfg(target_os = "macos")]
            bundle,
            cleanup,
            cancel: cancelled,
            arbiter,
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
        // A live `video` take is not answered here: the worker may still fail and
        // keep this process running. Commit answers it
        // (`App::video_answer_before_commit`).
        Ok(())
    }

    /// The sessions an overlap handoff hands over, as `(local_id, master, pid)`:
    /// every pooled session except a pane kept open after its command exited
    /// (`aterm --hold`), which the session registry marks `Exited`.
    ///
    /// An exited pane's master stays hung up until closed, and would read as a
    /// session dying under every attempt, so the update could never land. There is
    /// no live process to keep. The layout still names the pane, and the successor
    /// keeps its place as a placeholder (`RestoreManifest::placed_for_handed`).
    ///
    /// Read once per decision and passed down, so the gate, capture, descriptors
    /// and manifest describe the same set; the Commit compares against
    /// [`Self::handoff_exited_session_ids`].
    #[cfg(unix)]
    pub(crate) fn handoff_live_sessions(&self) -> Vec<(u64, i32, i32)> {
        let exited = self.handoff_exited_session_ids();
        self.pool
            .iter()
            .filter(|session| !exited.contains(&session.id))
            .map(|session| (session.id, session.master, session.pid))
            .collect()
    }

    /// The window facts the successor sizes its first window from, handed over at
    /// the park by both lanes: the front window's grid, frame origin and raw inner
    /// size (unchanged by font zoom); the message band's committed rows and their
    /// messages, so the successor reserves and paints them until Commit; the log
    /// lines the freeze kept; and when this process finished downloading the
    /// build, so the successor can record how long the update took.
    ///
    /// One projection for both lanes and for `seamless::app_capture_tests`, which
    /// asserts the successor reads back exactly this after the band changed rows.
    #[cfg(unix)]
    fn handoff_window_carry(&self, target_build: u64) -> Option<crate::session_store::WindowCarry> {
        let carried = self.carried_messages();
        let (font_px_milli, font_reset_px_milli) = self.handoff_font_zoom();
        self.windows.values().next().map(|state| {
            let position = state
                .os_window
                .as_ref()
                .and_then(|window| window.outer_position().ok());
            crate::session_store::WindowCarry {
                rows: state.rows,
                cols: state.cols,
                outer_x: position.map(|point| point.x),
                outer_y: position.map(|point| point.y),
                inner_px: state
                    .win_px
                    .filter(|size| size.width > 0 && size.height > 0)
                    .map(|size| [size.width, size.height]),
                presence_row: state.presence.rows > 0,
                status_bar_rows: self.message_band_rows,
                messages: carried.messages,
                next_message_id: carried.next_message_id,
                update_verified_unix_ms: crate::update_words::verified_unix_ms(
                    self.update_verified,
                    target_build,
                    std::time::Instant::now(),
                    std::time::SystemTime::now(),
                ),
                font_px_milli,
                font_reset_px_milli,
                update_health_said: carried.update_health_said,
                messages_captured_unix_ms: carried.captured_unix_ms,
            }
        })
    }

    /// [`Self::handoff_window_carry`] at a park, noting which messages crossed:
    /// at that attempt's Commit every live row the carry did not take ends with
    /// this process ([`Self::close_uncarried_messages`], ruling 441 (d)).
    #[cfg(unix)]
    fn park_window_carry(
        &mut self,
        target_build: u64,
    ) -> Option<crate::session_store::WindowCarry> {
        let window = self.handoff_window_carry(target_build);
        self.handoff_carried_messages = window
            .as_ref()
            .map(|window| window.messages.iter().map(|message| message.id).collect())
            .unwrap_or_default();
        window
    }

    /// The advisory warm hint a launched successor starts with
    /// (`handoff_warm_hint`): what [`Self::handoff_window_carry`] will write at the
    /// park, read at launch. The successor's warm prologue builds its backend at
    /// the font zoom, and only compares and logs window 0's grid and the counts.
    /// Stale by design: a zoom during the hold costs one warm miss, corrected at
    /// the backend join.
    #[cfg(target_os = "macos")]
    pub(crate) fn handoff_warm_hint(&self) -> crate::handoff_warm_hint::WarmHint {
        let (px, reset) = self.handoff_font_zoom();
        let (cols, rows) = self
            .windows
            .values()
            .next()
            .map_or((0, 0), |state| (state.cols, state.rows));
        crate::handoff_warm_hint::WarmHint {
            windows: u32::try_from(self.windows.len()).unwrap_or(u32::MAX),
            sessions: u32::try_from(self.pool.iter().count()).unwrap_or(u32::MAX),
            cols,
            rows,
            zoom: px.zip(reset),
        }
    }

    /// The live font zoom as the window carry writes it: `(px, reset px)` in
    /// thousandths of a physical px while the font is zoomed (pinned by
    /// `font_px_explicit` and away from the Cmd-0 size), else `(None, None)`.
    ///
    /// An unzoomed size is what the successor derives from the same config and
    /// display, so only a zoom is written, and always as a pair: the size to draw
    /// at and the one Cmd-0 returns to.
    #[cfg(unix)]
    pub(crate) fn handoff_font_zoom(&self) -> (Option<u32>, Option<u32>) {
        let zoomed = self.font_px_explicit && (self.font_px - self.default_font_px).abs() >= 0.5;
        match (
            crate::app_config::font_px_milli(self.font_px),
            crate::app_config::font_px_milli(self.default_font_px),
        ) {
            (Some(px), Some(reset)) if zoomed => (Some(px), Some(reset)),
            _ => (None, None),
        }
    }

    /// The pooled sessions whose command has exited (registry state
    /// `Exited`) — the panes [`Self::handoff_live_sessions`] leaves out. A
    /// leaf read of the session registry, never a `Terminal` lock.
    #[cfg(unix)]
    pub(crate) fn handoff_exited_session_ids(&self) -> Vec<u64> {
        let store = self
            .store
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.pool
            .iter()
            .map(|session| session.id)
            .filter(|id| {
                store.by_local(*id).is_some_and(|handle| {
                    handle.state == crate::session_store::SessionState::Exited
                })
            })
            .collect()
    }

    /// Starts the launched lane's history carry export (`crate::handoff_history`)
    /// for every handed session, with readers live, following each history until
    /// the park stops it. `None` for [`Self::history_sessions`]'s reasons or when
    /// the worker cannot start (logged; sessions then carry the screen carry's
    /// bounded history). The fork lane exports on its worker
    /// (`HistoryPlan::Deferred`).
    ///
    /// `None` too, and nothing written to disk, when `ceiling` — the carry
    /// ceiling of the successor's handoff policy as far as it is known at the
    /// launch — carries no scrollback. A policy not yet read then reads
    /// `Full`, so the export starts; the park, which reads the policy after
    /// the worker has published it, stops it then
    /// ([`Self::withhold_history_export`]). Its one caller, the launched
    /// lane's prelaunch, is macOS's.
    #[cfg(target_os = "macos")]
    fn start_history_export(
        &self,
        target_build: u64,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling,
    ) -> Option<crate::handoff_history::HistoryExporter> {
        if !ceiling.carries_scrollback() {
            aterm_log::info!(
                "update apply: build {target_build}'s handoff policy (carry={}) carries no \
                 scrollback; no scrollback export is started",
                ceiling.as_str()
            );
            return None;
        }
        let sessions = self.history_sessions(target_build)?;
        let dir = crate::control_auth::socket_dir()?;
        match crate::handoff_history::HistoryExporter::start(dir, sessions, true) {
            Ok(export) => Some(export),
            Err(error) => {
                aterm_log::warn!(
                    "update apply: the scrollback export could not start ({error}); each tab \
                     carries the screen carry's bounded history"
                );
                None
            }
        }
    }

    /// Stops the launched lane's history export under a policy whose `ceiling`
    /// carries no scrollback: takes it off the prelaunched attempt, gives its worker
    /// [`crate::handoff_history::HALT_WAIT`] to stop, and drops it, removing its
    /// sidecars. The park then hands the worker
    /// [`crate::handoff_history::HistoryPlan::Withheld`].
    #[cfg(any(target_os = "macos", all(test, unix)))]
    fn withhold_history_export(
        &mut self,
        target_build: u64,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling,
    ) {
        let Some(mut export) = self
            .update_handoff_prelaunch
            .as_mut()
            .and_then(|prelaunch| prelaunch.history_export.take())
        else {
            return;
        };
        let stopped = export.halt(crate::handoff_history::HALT_WAIT);
        drop(export);
        aterm_log::info!(
            "update apply: build {target_build}'s handoff policy (carry={}) carries no \
             scrollback; the scrollback export started beside the launch {}, and every file it \
             wrote is removed as it stops",
            ceiling.as_str(),
            if stopped {
                "has stopped"
            } else {
                "was asked to stop (it had not answered)"
            }
        );
    }

    /// Every pooled session's engine for the history export — the handed
    /// sessions' and the held panes' — or `None` when the successor is older
    /// than this build (no importer) or there is no private control dir
    /// (logged; sessions carry the screen carry's bounded history).
    #[cfg(unix)]
    fn history_sessions(
        &self,
        target_build: u64,
    ) -> Option<crate::handoff_history::ExportSessions> {
        if !crate::handoff_history::exports_for_target(crate::running_build_number(), target_build)
        {
            aterm_log::info!(
                "update apply: the successor (build {target_build}) is older than this build and \
                 imports no scrollback sidecar; each tab carries the screen carry's bounded \
                 history"
            );
            return None;
        }
        if crate::control_auth::socket_dir().is_none() {
            aterm_log::warn!(
                "update apply: no private control dir for the scrollback export; each tab \
                 carries the screen carry's bounded history"
            );
            return None;
        }
        // EVERY POOLED SESSION: the handed ones, and the panes kept open after
        // their command ended, whose scrollback crosses beside their held
        // screen (round eight of the update audit, finding 39), each in id
        // order. Kept apart, because a held pane's export only ever takes
        // room the handed sessions do not need: its first pass comes after
        // theirs, as the successor's read of the aggregate does (sessions,
        // then held panes), and a handed session that keeps printing evicts
        // held panes' exports when its follow would not fit otherwise (the
        // review of finding 39). Which is which is the park's to say.
        let live = self.handoff_live_sessions();
        let mut sessions = crate::handoff_history::ExportSessions::default();
        for session in self.pool.iter() {
            let entry = (session.id, std::sync::Arc::clone(&session.term));
            if live.iter().any(|(id, _, _)| *id == session.id) {
                sessions.handed.push(entry);
            } else {
                sessions.held.push(entry);
            }
        }
        sessions.handed.sort_unstable_by_key(|(id, _)| *id);
        sessions.held.sort_unstable_by_key(|(id, _)| *id);
        Some(sessions)
    }

    /// The successor's half of the history carry, at Commit: takes every adopted
    /// session's carry (`Session::handoff_history`) and imports it on a worker,
    /// never on the event loop (an import is O(the history)). The worker counts
    /// failures onto each session's `history_lost` itself and reports
    /// `Wake::HandoffHistorySettled`. A worker that cannot start costs every carry
    /// its lines, counted and said here.
    #[cfg(unix)]
    pub(crate) fn start_handoff_history_imports(&mut self) {
        let jobs = self.pool.take_handoff_histories();
        if jobs.is_empty() {
            return;
        }
        let not_run: Vec<crate::handoff_history::ImportReport> = jobs
            .iter()
            .map(|job| {
                crate::handoff_history::ImportReport::not_run(
                    job,
                    "the import worker could not start",
                )
            })
            .collect();
        let store = std::sync::Arc::clone(&self.store);
        let proxy = self.proxy.clone();
        let spawned = std::thread::Builder::new()
            .name("aterm-history-import".to_string())
            .spawn(move || {
                // The attach takes the terminal lock the UI thread contends:
                // Responsive is the floor for such a holder (`qos`).
                crate::qos::set_self(crate::qos::Role::Responsive);
                let reports = crate::handoff_history::run_imports(jobs);
                crate::handoff_history::record_reports(&store, &reports);
                if let Some(proxy) = proxy {
                    let _ = proxy.send_event(Wake::HandoffHistorySettled(reports));
                }
            });
        if let Err(error) = spawned {
            aterm_log::warn!("overlap handoff: the scrollback import could not start ({error})");
            crate::handoff_history::record_reports(&self.store, &not_run);
            self.settle_handoff_history(&not_run);
        }
    }

    /// Says what the history carry's imports left behind once they settle: when
    /// any line of any tab's history was left behind (outgoing fallbacks, policy
    /// withholding, sidecars that failed here), one band row says how many lines in
    /// how many tabs, and whether it was the policy's choice or a failure. Per-tab
    /// counts are on the registry (`handoff_history::record_reports`).
    ///
    /// The outgoing side's count is already on the band from the Commit's re-grid
    /// ([`Self::say_carried_history_loss`]); this settle posts only a change. A
    /// different summary re-words that row in place (same key, so no re-grid), or
    /// posts the new count if the row is gone.
    #[cfg(unix)]
    pub(crate) fn settle_handoff_history(
        &mut self,
        reports: &[crate::handoff_history::ImportReport],
    ) {
        let loss = crate::handoff_history::loss_summary(reports);
        let said = std::mem::take(&mut self.handoff_history_loss_said);
        if loss.lines > 0 && loss != said {
            self.post_message(crate::update_words::scrollback_left_behind(
                loss.lines,
                loss.tabs,
                loss.withheld,
                loss.held,
                loss.held_failed,
            ));
        }
    }

    /// Posts the loss the parent counted inside the Commit's one re-grid batch
    /// (`release_successor_commit_freeze`). Each adopted session arrives with the
    /// lines its outgoing process left behind (`AdoptedHistory::dropped`), which an
    /// import only adds to; noted for [`Self::settle_handoff_history`], which then
    /// re-grids nothing for it.
    #[cfg(unix)]
    pub(crate) fn say_carried_history_loss(&mut self) {
        let loss = crate::handoff_history::carried_loss(
            self.pool
                .iter()
                .filter_map(|session| session.handoff_history.as_ref()),
        );
        if loss.lines > 0 {
            self.post_message(crate::update_words::scrollback_left_behind(
                loss.lines,
                loss.tabs,
                loss.withheld,
                loss.held,
                loss.held_failed,
            ));
        }
        self.handoff_history_loss_said = loss;
    }

    /// The capture ladder, under the park: every session's screen (plus as much
    /// scrollback as the budget allows) and the control carry's head, priced
    /// against the freeze budget and the aggregate cell and decode-authority
    /// ceilings, then committed by the build's full predicate. Shared by both lanes
    /// (the fork lane before its spawn, the launched lane at its park); the caller
    /// rolls the readers back on `Err`.
    ///
    /// Total over screen content: each session is carried by
    /// [`crate::seamless::carry_for_wire`] at the most faithful rung the successor's
    /// `caps` admit, at worst a blank tab that redraws, and
    /// [`crate::seamless::settle_wire_carries`] lowers whatever session
    /// `screen_digest` still blames. So `Err` is only timing (the deadline, an
    /// engine lock), storage this process could not reserve, or a refusal about
    /// no single screen (too many sessions, a duplicate id).
    ///
    /// `ceiling` is the highest rung the successor's signed handoff policy allows
    /// ([`crate::seamless::carry_for_wire_within`]): `Full`; `Visible` (no
    /// scrollback); `Repaint` (every session blank). It only lowers. A cosmetic
    /// partial state over queued output (an OSC 8 link left open, or a picture the
    /// carry would only swallow: a kitty-graphics APC or an OSC 1337 image) may miss
    /// the park only while the launched lane has free mid-sequence re-parks
    /// ([`PRELAUNCH_MAX_MID_SEQUENCE_REPARKS`]); otherwise the screen is carried, the
    /// link closed and the in-flight frame dropped (the stream's next frame replaces
    /// it).
    #[cfg(unix)]
    fn soft_miss_may_repark(&self) -> bool {
        #[cfg(any(target_os = "macos", all(test, unix)))]
        {
            self.update_handoff_prelaunch
                .as_ref()
                .is_some_and(|prelaunch| {
                    prelaunch.park_mid_sequence_reparks < PRELAUNCH_MAX_MID_SEQUENCE_REPARKS
                })
        }
        #[cfg(not(any(target_os = "macos", all(test, unix))))]
        {
            false
        }
    }

    #[cfg(unix)]
    fn capture_parked_screens(
        &mut self,
        live: &[(u64, i32, i32)],
        deadline: std::time::Instant,
        freeze_ms: u128,
        handoff_history_comfort: std::time::Duration,
        caps: crate::seamless::WireCaps,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling,
    ) -> Result<ParkedScreens, CaptureFailure> {
        use crate::seamless::{CarryRung, WireCarry};

        let soft_miss_may_repark = self.soft_miss_may_repark();
        let mut wire: Vec<WireCarry> = Vec::new();
        // The control carry's captures. Storage it cannot reserve carries
        // nothing (`carry_room` false) — never a failed capture.
        let mut carries = Vec::new();
        let carry_room = carries.try_reserve_exact(live.len()).is_ok();
        // The foreground holders, as best-effort as the control carry, but
        // never tied to it (see the push below).
        let mut fg_holders = Vec::new();
        let holder_room = fg_holders.try_reserve_exact(live.len()).is_ok();
        // The render debts, as best-effort: a debt not carried is a tab the
        // successor cannot repair, never a failed capture.
        let mut render_debts = Vec::new();
        let debt_room = render_debts.try_reserve_exact(live.len()).is_ok();
        // The history heads are not best-effort: a session without one would
        // lose its history with nothing counting it, so storage for them is
        // reserved like the screens' own.
        let mut history_heads = Vec::new();
        if wire.try_reserve_exact(live.len()).is_err()
            || history_heads.try_reserve_exact(live.len()).is_err()
        {
            return Err(CaptureFailure::Storage);
        }
        let mut capture_failed = None;
        let mut capture_budget = 0_u64;
        let mut capture_cells = 0_u64;
        // Latched once a session's carried history was refused (by budget or blob
        // shape), so later sessions go straight to visible-only.
        let mut history_latched_off = false;
        // Mandatory first, optional second: price the visible and alt grids of the
        // whole pool in both budgets before charging any of it, so a later session
        // always has room for its non-degradable screen (`optional_carry_fits`).
        // Readers are parked, so geometry cannot move between the two passes.
        //
        // An unreadable engine reserves everything, so nobody carries scrollback;
        // the capture loop reports the busy engine a moment later.
        let mut cells_reserve = 0_u64;
        let mut bytes_reserve = 0_u64;
        // Only the handed sessions: `live` omits exited `--hold` panes
        // (`App::handoff_live_sessions`), and a screen carried for a session with no
        // descriptor on the wire is refused by `write_outgoing`.
        let handed = |id: u64| live.iter().any(|(local_id, _, _)| *local_id == id);
        // In session-id order, not the pool's: the pool is a randomly seeded
        // `HashMap`, and this walk spends pool-wide state (history latch, byte and
        // cell budgets, decode authority), so the same desk must be priced the same
        // way every time. Ids follow creation order. Reserved like the rest.
        let mut sessions = Vec::new();
        if sessions.try_reserve_exact(live.len()).is_err() {
            return Err(CaptureFailure::Storage);
        }
        sessions.extend(self.pool.iter().filter(|session| handed(session.id)));
        sessions.sort_unstable_by_key(|session| session.id);
        for session in sessions.iter().copied() {
            let terminal = match try_lock_by(&session.term, deadline) {
                Some(guard) => guard,
                None => {
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
        for session in sessions.iter().copied() {
            if std::time::Instant::now() >= deadline {
                capture_failed = Some(CaptureFailure::Deadline { freeze_ms });
                break;
            }
            let Some(terminal) = try_lock_by(&session.term, deadline) else {
                capture_failed = Some(CaptureFailure::EngineBusy);
                break;
            };
            // A parser parked inside a sequence no carry holds, over queued output,
            // is a miss, not a carry: the successor's parser would print the queued
            // tail as text. That covers:
            // - a hooked DCS query, whose reply lives in its handler (every other
            //   partial sequence rides the checkpoint, continued or swallowed; a
            //   hooked string the engine only consumes, such as a tmux passthrough
            //   wrapper, is swallowed exactly — round eight, finding 57);
            // - any partial sequence when the successor predates the parser carry
            //   (`WireCaps::carries_parser`);
            // - a carry that only swallows (an over-cap OSC such as OSC 52 or 1337,
            //   an unhooked DCS header, a started APC such as kitty graphics, a
            //   hooked sixel string; `Terminal::partial_sequence_swallowed`);
            // - when the successor predates the output-state carry
            //   (`WireCaps::carries_output_state`), engine state it would resume
            //   without: an open OSC 8 link or a VT52 `ESC Y` awaiting its address
            //   (`Terminal::output_state_in_flight`). A successor that carries it
            //   continues both (round eight of the update audit, finding 502).
            // Over a quiet PTY (a stalled sequence) abandoning is the answer. One
            // `poll` per such session.
            //
            // An open link is cosmetic and may never close, so on such a hop its
            // miss spends only the launched lane's free mid-sequence re-parks
            // (`soft_miss_may_repark`); after that, and on the fork lane, the
            // screen is carried and that successor writes the rest of the text
            // unlinked. A swallowed picture (kitty APC chunks, OSC 1337
            // frames, sixel strings) takes the same rule: a graphics stream never
            // pauses at a batch edge, and its next frame replaces the one dropped.
            // Every other swallowed sequence (an OSC 52 clipboard write, a DCS
            // header), a DCS query and a VT52 address keep the hard miss on every
            // lane.
            let partial = if caps.carries_parser() {
                terminal.partial_sequence_uncarried().or_else(|| {
                    terminal.partial_sequence_swallowed().filter(|_| {
                        soft_miss_may_repark || !terminal.partial_sequence_swallows_picture()
                    })
                })
            } else {
                terminal.partial_sequence_state()
            }
            .or_else(|| {
                (!caps.carries_output_state())
                    .then(|| terminal.output_state_in_flight())
                    .flatten()
                    .filter(|state| soft_miss_may_repark || *state != OPEN_LINK_STATE)
            });
            if let Some(state) = partial
                && live
                    .iter()
                    .find(|(local_id, _, _)| *local_id == session.id)
                    .is_some_and(|entry| handoff_masters_have_activity(std::slice::from_ref(entry)))
            {
                capture_failed = Some(CaptureFailure::MidSequence {
                    local_id: session.id,
                    state,
                });
                break;
            }
            // Scrollback is best-effort and must never cost the handoff: only the
            // visible screen is required for adoption. Depth is chosen per session
            // against the remaining time (zero once over half the window is spent)
            // and the remaining budget after the pool's mandatory reservation.
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let (rows, cols) = (terminal.rows(), terminal.cols());
            let own_cells = crate::seamless::mandatory_checkpoint_cells(rows, cols);
            let own_bytes =
                crate::seamless::checkpoint_capture_budget_bytes(rows, cols, 0).unwrap_or(u64::MAX);
            // Both reserves now cover only the sessions after this one, so this
            // session's scrollback is priced against what the rest still need.
            cells_reserve = cells_reserve.saturating_sub(own_cells);
            bytes_reserve = bytes_reserve.saturating_sub(own_bytes);
            // A policy ceiling below Full carries no scrollback, so none is
            // priced either: the budgets stay for the sessions that follow. The
            // history carry reads the same answer (`HistoryPlan::Withheld`).
            let carry = if ceiling.carries_scrollback() {
                crate::seamless::max_handoff_history_lines()
            } else {
                0
            };
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
            let history = if remaining >= handoff_history_comfort
                && !history_latched_off
                && history_fits_cells
                && history_fits_bytes
            {
                carry
            } else {
                0
            };
            // The decode-authority budget is the producer's own ceiling (no consumer
            // checks it), so it chooses a rung but never refuses the update: a
            // session whose visible screen alone would exceed it is carried blank.
            let visible_authority = crate::seamless::checkpoint_capture_budget_bytes(rows, cols, 0)
                .and_then(|bytes| capture_budget.checked_add(bytes))
                .filter(|total| *total <= MAX_HANDOFF_CAPTURE_BUDGET_BYTES);
            let (checkpoint, rung, _cause) = if visible_authority.is_some() {
                crate::seamless::carry_for_wire_within(
                    &terminal,
                    session.id,
                    history,
                    &mut capture_cells,
                    caps,
                    ceiling,
                )
            } else {
                crate::seamless::repaint_carry_for_wire(
                    &terminal,
                    session.id,
                    &mut capture_cells,
                    caps,
                    format!(
                        "{rows}x{cols} would take the pool past the \
                         {MAX_HANDOFF_CAPTURE_BUDGET_BYTES}-byte decode-authority budget"
                    ),
                )
            };
            // A refused history lowers every later session's too, since the budget
            // or shape that refused it is the pool's. Only the VisibleOnly rung means
            // that; Sanitized, StrippedLinks and Repaint are about this one screen.
            if history != 0 && rung == CarryRung::VisibleOnly {
                history_latched_off = true;
            }
            capture_budget = crate::seamless::checkpoint_capture_budget_bytes(
                checkpoint.rows,
                checkpoint.cols,
                checkpoint.history_lines,
            )
            .map_or(u64::MAX, |bytes| capture_budget.saturating_add(bytes));
            // The control carry's share of the freeze: the archive's fence and
            // counters, plus the differ's screen-sized state, under the same lock as
            // the checkpoint. Rows and ledger are exported on the worker behind the
            // fence. Past half the budget even the differ's state is left for the
            // worker (the fence's fingerprint keeps it exact). A clamped or blank
            // screen drops only the differ's state; ledger and archive rows still
            // travel. A screen that lost only links keeps it
            // (`CarryRung::keeps_differ_state`).
            let differ = deadline.saturating_duration_since(std::time::Instant::now())
                >= handoff_history_comfort;
            // The foreground holder, for every session and apart from the control
            // carry: the last group the parked reader saw. It lets the adopting
            // reader hand back a job that died during the handoff even when the
            // control carry was dropped. Reserved above: no allocation.
            if holder_room {
                fg_holders.push((
                    session.id,
                    session.fg_holder.load(std::sync::atomic::Ordering::Acquire),
                ));
            }
            // THE RENDER DEBT (R08): what the engine owes this session's alternate
            // screen, under the lock the checkpoint was taken under. A session
            // carried blank on purpose (the Repaint rung) has no screen the debt
            // could be about, and one on the main screen owes nothing (the
            // primary screen is history-backed).
            if debt_room && rung != CarryRung::Repaint && terminal.modes().alternate_screen {
                let debt = terminal.render_debt();
                if debt != (0, 0) {
                    render_debts.push((session.id, debt));
                }
            }
            // THE HISTORY HEAD, under the same lock as the checkpoint: the
            // fence the worker joins this session's export to. A pure read.
            // Reserved for every live session above, so this never allocates.
            history_heads.push(crate::handoff_history::capture_head(session.id, &terminal));
            // Reserved for every live session above, so this never allocates.
            if carry_room {
                let mut source = crate::handoff_carry::capture_head(
                    session.id,
                    &terminal,
                    &session.term,
                    &session.ctx.turns,
                    differ && rung.keeps_differ_state(),
                );
                if !rung.keeps_differ_state() {
                    source.without_differ();
                }
                carries.push(source);
            }
            wire.push(WireCarry {
                local_id: session.id,
                checkpoint,
                rung,
            });
            if std::time::Instant::now() >= deadline {
                capture_failed = Some(CaptureFailure::Deadline { freeze_ms });
                break;
            }
        }
        if capture_failed.is_none() && std::time::Instant::now() >= deadline {
            capture_failed = Some(CaptureFailure::Deadline { freeze_ms });
        }
        if let Some(failure) = capture_failed {
            return Err(failure);
        }
        // The self-check: commit the pool with `screen_digest`, lowering whatever
        // session it blames instead of refusing the update.
        let screen_digest =
            crate::seamless::settle_wire_carries(&mut wire, caps).map_err(|refusal| {
                CaptureFailure::Refused {
                    local_id: refusal.local_id(),
                    cause: format!(
                        "visible checkpoint set could not be committed canonically: {refusal}"
                    ),
                }
            })?;
        // A session the self-check lowered to a clamped or blank screen loses
        // its differ's state too (`CarryRung::keeps_differ_state`); its ledger
        // and archive rows still travel.
        carries.retain_mut(|source: &mut crate::handoff_carry::CarrySource| {
            let Some(carry) = wire
                .iter()
                .find(|carry| carry.local_id == source.local_id())
            else {
                return false;
            };
            if !carry.rung.keeps_differ_state() {
                source.without_differ();
            }
            true
        });
        let mut screens = Vec::new();
        let mut repaint = Vec::new();
        if screens.try_reserve_exact(wire.len()).is_err()
            || repaint.try_reserve_exact(wire.len()).is_err()
        {
            return Err(CaptureFailure::Storage);
        }
        for carry in wire {
            if carry.rung.needs_repaint() {
                repaint.push(carry.local_id);
            }
            screens.push((carry.local_id, carry.checkpoint));
        }
        // THE HELD PANES, last and optional (round five, item 19): only with
        // half the budget still to spare, from what the handed screens left
        // of the aggregate cells, and never a reason to miss the park — so
        // they stop a QUARTER of the budget short of the deadline (round six
        // of the update audit, finding 21): the layout capture and the
        // callers' deadline check still come after them, and a held capture
        // that ran to the deadline itself turned every landed park into a
        // miss.
        let held = match held_capture_deadline(
            deadline,
            std::time::Instant::now(),
            handoff_history_comfort,
        ) {
            Some(held_deadline) => self.capture_held_screens(
                live,
                held_deadline,
                &mut capture_cells,
                caps,
                ceiling,
                &mut carries,
            ),
            None => Vec::new(),
        };
        Ok(ParkedScreens {
            screens,
            carries,
            screen_digest,
            repaint,
            fg_holders,
            render_debts,
            history_heads,
            held,
        })
    }

    /// The visible screen of every pooled session not in `live` (exited panes,
    /// never handed), at the most faithful rung `caps` and `ceiling` admit, without
    /// scrollback in it — its scrollback's fence is taken here, and its export is
    /// joined against it when the manifest is written (round eight, finding 39).
    /// Best-effort: a busy engine, a screen that would go blank, or one
    /// reached after the deadline is left out and shown as a placeholder. One
    /// `try_lock` per engine; nothing waits.
    ///
    /// Bounded by what can cross: stops at [`crate::seamless::MAX_HELD_PANES`] and
    /// skips a screen past what remains of
    /// [`crate::seamless::MAX_HELD_AGGREGATE_GRID_BYTES`]. Each carries how its
    /// command ended (`App::exit_status`, collected without waiting).
    ///
    /// AND ITS CONTROL CARRY (round eight of the update audit, finding 730): a
    /// pane carried here adds its turn ledger and archive fence to `carries`,
    /// under the lock its screen was taken under, so the export carries them as a
    /// handed session's ([`crate::handoff_carry::export_for`]) and `history`,
    /// `aterm drive report` and a resumed `since-turn=` still read the turns a
    /// manager drove into it. Without the differ's state: nothing draws there.
    #[cfg(unix)]
    fn capture_held_screens(
        &self,
        live: &[(u64, i32, i32)],
        deadline: std::time::Instant,
        capture_cells: &mut u64,
        caps: crate::seamless::WireCaps,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling,
        carries: &mut Vec<crate::handoff_carry::CarrySource>,
    ) -> Vec<crate::seamless::HeldScreen> {
        let mut held: Vec<_> = self
            .pool
            .iter()
            .filter(|session| !live.iter().any(|(id, _, _)| *id == session.id))
            .collect();
        held.sort_unstable_by_key(|session| session.id);
        let mut screens = Vec::new();
        let mut held_bytes = 0_u64;
        for session in held {
            if std::time::Instant::now() >= deadline
                || screens.len() >= crate::seamless::MAX_HELD_PANES
            {
                break;
            }
            let Ok(terminal) = session.term.try_lock() else {
                continue;
            };
            let mut cells = *capture_cells;
            let (checkpoint, rung, _cause) = crate::seamless::carry_for_wire_within(
                &terminal, session.id, 0, &mut cells, caps, ceiling,
            );
            // The held screen is the visible rows only: its scrollback rides
            // its own sidecar, and what does not cross is counted on the
            // successor's band, never dropped in silence (round eight,
            // finding 39).
            let history_dropped =
                u64::try_from(terminal.main_grid().scrollback_lines()).unwrap_or(u64::MAX);
            // …and its fence, which its export is joined against when the
            // manifest is written, so its scrollback crosses as a handed
            // session's does (round eight, finding 39).
            let history_head = Some(terminal.history_fence());
            // …and its control carry's head, under the same lock (no differ).
            let mut control = crate::handoff_carry::capture_head(
                session.id,
                &terminal,
                &session.term,
                &session.ctx.turns,
                false,
            );
            control.without_differ();
            drop(terminal);
            if rung.needs_repaint() || screens.try_reserve(1).is_err() {
                continue;
            }
            if let Err(why) = crate::seamless::held_screen_meta(&checkpoint) {
                aterm_log::warn!(
                    "update handoff: held pane {} is not carried: {why}",
                    session.id
                );
                continue;
            }
            let screen = crate::seamless::HeldScreen {
                local_id: session.id,
                sid: session.ctx.self_id.clone(),
                checkpoint,
                exit: self.exit_status(session.id, crate::app_tabs::ExitLook::Retry),
                history_dropped,
                history_head,
                failure_seen: self.session_status.failure_seen(session.id),
            };
            let bytes = screen.grid_bytes();
            if held_bytes.saturating_add(bytes) > crate::seamless::MAX_HELD_AGGREGATE_GRID_BYTES {
                continue;
            }
            held_bytes = held_bytes.saturating_add(bytes);
            *capture_cells = cells;
            screens.push(screen);
            // Best-effort like the screen: a pane whose carry cannot be kept
            // crosses without it, and its ledger says so in the successor.
            if carries.try_reserve(1).is_ok() {
                carries.push(control);
            }
        }
        screens
    }

    /// The dry run: the park's own capture ([`Self::capture_parked_screens`]) over
    /// the same handed set, timed with every reader live, so the first freeze rung
    /// is budgeted from this desk's cost. The readers' stop, which it cannot time,
    /// comes from the last landed park (`keep_landed_park`).
    ///
    /// Nothing is parked and nothing leaves: each engine is read under its own lock
    /// and the product dropped, so this is no protocol step, and a reader waits at
    /// most one session's capture.
    ///
    /// It prices the widest capture a rung would attempt (history for every
    /// session the budgets admit), within [`DRY_RUN_CAPTURE_WINDOW`], under the
    /// successor's handoff policy ([`Self::dry_run_ceiling`]): the policy exists to
    /// route around a slow or panicking capture path, and this runs on the main
    /// thread. With the policy not yet read, no dry run is taken (`Unmeasured`) and
    /// the seed falls back to the ledger.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn dry_run_handoff_capture(
        &mut self,
        caps: crate::seamless::WireCaps,
    ) -> DryRunCapture {
        let live = self.handoff_live_sessions();
        if live.is_empty() {
            return DryRunCapture::Unmeasured("no live session to capture");
        }
        let Some(ceiling) = self.dry_run_ceiling() else {
            return DryRunCapture::Unmeasured(
                "the successor's handoff policy has not been read yet",
            );
        };
        let started = std::time::Instant::now();
        let captured = self.capture_parked_screens(
            &live,
            started + DRY_RUN_CAPTURE_WINDOW,
            DRY_RUN_CAPTURE_WINDOW.as_millis(),
            std::time::Duration::ZERO,
            caps,
            ceiling,
        );
        let took = started.elapsed();
        match captured {
            Ok(parked) => DryRunCapture::Measured {
                took,
                sessions: parked.screens.len(),
            },
            // Out of time — the capture itself, or an engine lock a reader held
            // the whole window: at least the window, which saturates the rung.
            Err(CaptureFailure::Deadline { .. } | CaptureFailure::EngineBusy) => {
                DryRunCapture::AtLeast {
                    took: took.max(DRY_RUN_CAPTURE_WINDOW),
                    sessions: live.len(),
                }
            }
            // A reader mid-sequence over live output is ordinary while readers
            // run; storage and a refusal are the park's to report. None of them
            // says how long a capture takes.
            Err(CaptureFailure::MidSequence { .. }) => {
                DryRunCapture::Unmeasured("a parser was mid-sequence over live output")
            }
            Err(CaptureFailure::Storage) => {
                DryRunCapture::Unmeasured("the capture could not reserve its storage")
            }
            Err(CaptureFailure::Refused { .. }) => {
                DryRunCapture::Unmeasured("the capture refused the desk")
            }
        }
    }

    /// The carry ceiling the dry run captures under: the successor's handoff policy
    /// for the prelaunched attempt's artifact via [`Self::park_policy`], as every
    /// park reads it, so the dry run never captures above the park. `Full` with no
    /// artifact (the same-image seam), `None` while the policy is unread.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    fn dry_run_ceiling(&self) -> Option<aterm_update_core::handoff_policy::CarryCeiling> {
        let attempt = self
            .update_handoff_prelaunch
            .as_ref()
            .and_then(|prelaunch| prelaunch.apply_attempt.as_ref());
        let policy = self.park_policy(attempt);
        (attempt.is_none() || policy.known).then(|| policy.carry_ceiling())
    }

    /// Seeds this attempt's first automatic freeze rung: times the dry run while the
    /// successor boots, never below the last landed park, persists the measurement
    /// in the health ledger off this thread (its lock is a file lock), and falls
    /// back to the ledger, then 20 ms, when nothing was measured. An explicit apply
    /// uses the widest rung and spends no dry run. The seed is stored on the
    /// prelaunched attempt ([`Self::prelaunched_park_budget`]) and returned, with
    /// the ledger writer's handle for tests to join.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn seed_first_freeze_rung(
        &mut self,
        mode: crate::native_updater_service::ApplyMode,
        caps: crate::seamless::WireCaps,
    ) -> (FreezeSeed, Option<std::thread::JoinHandle<()>>) {
        if !mode.is_automatic() {
            return (FreezeSeed::Default, None);
        }
        let dry = self.dry_run_handoff_capture(caps);
        // One small read, while the successor boots: the last landed park, which
        // the dry run cannot time, and the fallback when it measured nothing.
        let prior = aterm_update::handoff_capture_prior();
        let mut kept = None;
        let seed = match dry {
            DryRunCapture::Measured { took, sessions }
            | DryRunCapture::AtLeast { took, sessions } => {
                // In the ledger's unit, whole microseconds (at least one), so
                // the rung seeded now is the rung the kept record seeds next
                // time: 1.5 × a capture between 13.3 and 53.3 ms is unclamped,
                // and its nanoseconds would otherwise buy a rung the ledger's
                // copy cannot.
                let took = std::time::Duration::from_micros(
                    u64::try_from(took.as_micros()).unwrap_or(u64::MAX).max(1),
                );
                let seed = FreezeSeed::DryRun {
                    capture: took,
                    sessions,
                    last_park: FreezeSeed::last_park_of(prior.as_ref()),
                };
                let record = aterm_update::HandoffCaptureRecord {
                    capture_us: u64::try_from(took.as_micros()).unwrap_or(u64::MAX),
                    sessions: u32::try_from(sessions).unwrap_or(u32::MAX),
                    freeze_seed_ms: u64::try_from(seed.rung0().as_millis()).unwrap_or(u64::MAX),
                    ..aterm_update::HandoffCaptureRecord::default()
                };
                let build = crate::running_build_number();
                // A small ledger write nobody waits on.
                match spawn_ledger_writer("aterm-freeze-seed", move || {
                    aterm_update::record_handoff_capture(build, &record);
                }) {
                    Ok(handle) => kept = Some(handle),
                    Err(error) => aterm_log::warn!(
                        "update apply: the timed capture could not be kept for the next update \
                         ({error}); this attempt still seeds from it"
                    ),
                }
                seed
            }
            DryRunCapture::Unmeasured(why) => {
                aterm_log::info!("update apply: the dry-run capture measured nothing ({why})");
                FreezeSeed::from_prior(prior.as_ref())
            }
        };
        let saturated = if matches!(dry, DryRunCapture::AtLeast { .. }) {
            " — it ran out of its window, so the rung takes its ceiling"
        } else {
            ""
        };
        aterm_log::info!(
            "update apply: first freeze rung {} ms — {seed}{saturated}",
            seed.rung0().as_millis()
        );
        if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() {
            prelaunch.freeze_seed = seed;
        }
        (seed, kept)
    }

    /// The prelaunched attempt's budget for its next park: the ladder at the
    /// target's prior physical failures plus this attempt's misses, with rung 0 the
    /// dry-run seed, as `(prior failures incl. misses, budget, seed)`; `None` with
    /// no attempt prelaunched. The park reads its budget only from here.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn prelaunched_park_budget(&self) -> Option<(u8, std::time::Duration, FreezeSeed)> {
        let prelaunch = self.update_handoff_prelaunch.as_ref()?;
        let prior = self
            .prior_physical_failures_of(prelaunch.apply_attempt.as_ref())
            .saturating_add(prelaunch.park_misses);
        let budget = handoff_freeze_budget(prelaunch.mode, prior, prelaunch.freeze_seed);
        Some((prior, budget, prelaunch.freeze_seed))
    }

    /// The launched lane's first half: records the attempt, hands the worker what
    /// it needs to launch and hold the successor, and returns with every reader
    /// live. The park is taken later on this thread, when the worker reports the
    /// dial (`Wake::UpdateHandoffAwaitingPark`) and the park gate admits.
    #[cfg(target_os = "macos")]
    fn prelaunch_out_of_band_handoff(
        &mut self,
        args: PrelaunchArgs,
    ) -> Result<(), crate::UpdateHandoffStartError> {
        let PrelaunchArgs {
            attempt_id,
            build,
            mode,
            apply_attempt,
            same_image,
            target_build,
            target_commit,
            command,
            verify_staged_candidate,
            grant_chunks,
            installed_activation,
            bundle,
            cancel,
            cancelled,
            job_tx,
            reconcile_worker,
            reconcile_ticket,
        } = args;
        // Minted before the launch: the launch environment names the manifest and
        // layout by path, and the writer derives the same names after the park.
        let nonce = crate::seamless::mint_outgoing_nonce();
        // Read here, on the main thread, where the windows are: the worker
        // that launches the successor never touches `App`.
        let warm_hint = self.handoff_warm_hint();
        // The worker writes this as the layout sidecar while the successor boots;
        // the park only checks it (`handoff_diet`). With readers live a busy tab's
        // folder or title may read degraded, which costs a rewrite at the park,
        // never a wrong sidecar.
        let speculative_layout = Some(self.capture_handoff_layout());
        let (stand_down_tx, stand_down_rx) = std::sync::mpsc::sync_channel(1);
        let (stand_down_ack_tx, stand_down_ack_rx) = std::sync::mpsc::sync_channel(1);
        let (transfer_tx, transfer_rx) = std::sync::mpsc::sync_channel(1);
        let arbiter = crate::HandoffAttemptArbiter::new();
        let preverified =
            PreverifyPublisher::for_attempt(&self.handoff_preverified, apply_attempt.as_ref());
        // A ticket names a candidate unless the same-image seam re-runs this image:
        // the dialer's code is the candidate's whether or not the worker verifies.
        let successor_runs_candidate = apply_attempt.is_some() && same_image.is_none();
        // Started beside the launch, so the history exports during the successor's
        // boot, unless a policy already on record carries no scrollback.
        let history_export = self.start_history_export(
            target_build,
            self.park_policy(apply_attempt.as_ref()).carry_ceiling(),
        );
        self.update_handoff_prelaunch = Some(crate::HandoffPrelaunch {
            attempt_id,
            nonce: nonce.clone(),
            mode,
            apply_attempt,
            same_image,
            target_build,
            target_commit: target_commit.clone(),
            cancel: cancel.clone(),
            stand_down: stand_down_tx,
            stand_down_ack: stand_down_ack_tx,
            transfer: transfer_tx,
            arbiter: arbiter.clone(),
            launched_at: std::time::Instant::now(),
            dialled: None,
            park_retry_at: None,
            park_misses: 0,
            park_mid_sequence_reparks: 0,
            // Seeded below, once the successor is booting.
            freeze_seed: FreezeSeed::Default,
            land_waits: 0,
            last_wait: None,
            stood_down: false,
            teardown: crate::DeferredHandoffTeardown::None,
            revoked_by_activity: false,
            history_export,
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
            installed_activation,
            preverified,
            parked_under: None,
            // The fact the lane was chosen on; the worker's own verification,
            // when it runs, re-reads it from the candidate it just verified.
            successor_grant_chunks: grant_chunks,
            command,
            // The park has not happened: the capture arrives on the transfer
            // channel once it has.
            capture: None,
            prelaunch: Some(HandoffPrelaunchLane {
                nonce,
                transfer: transfer_rx,
                stand_down: stand_down_rx,
                stand_down_ack: stand_down_ack_rx,
                hold_bound: PRELAUNCH_HOLD_BOUND,
                warm_hint: warm_hint.encode(),
                speculative_layout,
                successor_runs_candidate,
            }),
            lane: HandoffLane::OutOfBand,
            bundle,
            cleanup,
            cancel: cancelled,
            arbiter,
        };
        if job_tx.send(job).is_err() {
            self.update_handoff_prelaunch = None;
            return Err(crate::UpdateHandoffStartError::failed(
                "overlap handoff worker stopped before preparation",
            ));
        }
        aterm_log::info!(
            "update apply: launching the successor with every reader live (the late park); \
             the outgoing process parks once it has dialled"
        );
        // The successor is booting and nothing waits on this thread until it dials,
        // so time the park's capture now with readers live and seed the first
        // freeze rung. The attempt stays `launched`; the ledger writer runs
        // detached.
        let _seeded =
            self.seed_first_freeze_rung(mode, crate::seamless::WireCaps::for_target(target_build));
        Ok(())
    }

    /// `Wake::UpdateHandoffAwaitingPark`: the worker holds the successor's dial (or
    /// must fork), so the park may be taken, at once for an explicit apply and at
    /// the next quiet moment for the automatic lane.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn on_update_handoff_awaiting_park(
        &mut self,
        attempt_id: u64,
        dialer_pid: Option<u32>,
        grant_limit: Option<usize>,
    ) {
        let now = std::time::Instant::now();
        let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() else {
            aterm_log::warn!(
                "update apply: ignored a park cue for attempt {attempt_id} — no attempt is \
                 prelaunched"
            );
            return;
        };
        if prelaunch.attempt_id != attempt_id {
            aterm_log::warn!(
                "update apply: ignored a stale park cue for attempt {attempt_id} (the \
                 prelaunched attempt is {})",
                prelaunch.attempt_id
            );
            return;
        }
        prelaunch.dialled = Some(crate::DialledSuccessor {
            pid: dialer_pid,
            grant_limit,
            at: now,
        });
        match dialer_pid {
            Some(pid) => aterm_log::info!(
                "update apply: the successor (pid {pid}) dialled {} ms after the launch and \
                 holds its claim; parking when the gate admits",
                now.saturating_duration_since(prelaunch.launched_at)
                    .as_millis()
            ),
            None => aterm_log::info!(
                "update apply: the launched lane refused at runtime; the worker forks once \
                 the outgoing process has parked"
            ),
        }
        self.try_park_for_prelaunched_successor(now);
    }

    /// The park gate for a prelaunched attempt, re-run from the event loop every
    /// [`PRELAUNCH_PARK_RETRY`] while the automatic lane waits for a quiet moment,
    /// bounded by [`PRELAUNCH_HOLD_MAX`]: an attempt held past it stands down as
    /// activity-revoked (no physical budget spent) and retries later.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn try_park_for_prelaunched_successor(&mut self, now: std::time::Instant) {
        let Some(prelaunch) = self.update_handoff_prelaunch.as_ref() else {
            return;
        };
        if prelaunch.dialled.is_none() {
            // The worker has not reported the dial yet: nothing has been
            // launched-and-held to park for.
            return;
        }
        if prelaunch.stood_down || prelaunch.park_retry_at.is_some_and(|at| now < at) {
            return;
        }
        let attempt = self.park_and_transfer_to_prelaunched_successor(now);
        // A park that did not hand its capture over resumes the history export it
        // paused: readers are live again, and a paused export would meet the
        // re-park's heads with a stale fence (`Outrun`). A landed park took the
        // export with its capture.
        if !matches!(attempt, ParkAttempt::Parked)
            && let Some(export) = self
                .update_handoff_prelaunch
                .as_mut()
                .and_then(|prelaunch| prelaunch.history_export.as_mut())
        {
            export.resume();
        }
        match attempt {
            ParkAttempt::Parked => {
                if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() {
                    prelaunch.park_retry_at = None;
                }
            }
            ParkAttempt::NotYet(reason) => {
                if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() {
                    prelaunch.park_retry_at = Some(now + PRELAUNCH_PARK_RETRY);
                    aterm_log::debug!("update apply: not parking yet — {reason}");
                }
            }
            // A refused capture is not a missed park: a wider rung cannot change
            // it, and `ActivityRevoked` would claim the machine was busy. The
            // successor stands down as `CaptureRefused`; the refusal lane retries
            // once the desk changes.
            ParkAttempt::Refused(refusal) => {
                aterm_log::warn!(
                    "update apply: {refusal}; a wider freeze rung cannot change that, so the \
                     held successor stands down and the lane retries once the desk changes"
                );
                self.stand_down_prelaunched_successor(capture_refusal_stand_down(&refusal));
            }
            // A park that missed its budget is not a failed update: readers are back
            // (nothing granted), the successor still holds. Re-park on the next rung,
            // and once every rung is tried stand down as a busy machine, retried on
            // the activity spacing, never a latch (`park_miss_disposition`).
            ParkAttempt::Missed(reason) => {
                // Every miss is pre-record: the parked attempt is stored at the end
                // of the park half, after the last point that can miss, so a miss
                // leaves only the prelaunch record and resumed readers. That makes
                // re-parking under the same nonce sound; the layout sidecar is sealed
                // against whichever park lands (`handoff_diet::seal_layout`).
                debug_assert!(
                    self.pending_update_handoff.is_none(),
                    "a park that missed its budget must leave no parked attempt behind"
                );
                let misses = self
                    .update_handoff_prelaunch
                    .as_ref()
                    .map_or(u8::MAX, |prelaunch| prelaunch.park_misses);
                self.dispose_park_miss(now, park_miss_disposition(misses, reason));
            }
            ParkAttempt::MissedMidSequence(reason) => {
                debug_assert!(
                    self.pending_update_handoff.is_none(),
                    "a park that missed its budget must leave no parked attempt behind"
                );
                let (misses, reparks) = self
                    .update_handoff_prelaunch
                    .as_ref()
                    .map_or((u8::MAX, u8::MAX), |prelaunch| {
                        (prelaunch.park_misses, prelaunch.park_mid_sequence_reparks)
                    });
                self.dispose_park_miss(now, mid_sequence_miss_disposition(misses, reparks, reason));
            }
            ParkAttempt::Failed(stand_down) => self.stand_down_prelaunched_successor(stand_down),
        }
    }

    /// THE LAUNCHED LANE'S GATE HOLDS THE PARK BACK with every reader live (the
    /// review of round eight's leads U60 and U40): a rim a missed park started
    /// leaves, since the terminal it would explain echoes again and the gate may
    /// wait until [`PRELAUNCH_HOLD_MAX`] — the U60 symptom, a rim pulsing around a
    /// working terminal. The park the gate admits next starts it again
    /// (`charge_upgrade_rim_at_park`). Before the first park there is none.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    fn retire_rim_while_the_gate_waits(&mut self) {
        if self.pending_update_handoff.is_none() {
            self.retire_charging_surge();
        }
    }

    /// Apply one missed park's disposition to the prelaunched attempt.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    fn dispose_park_miss(&mut self, now: std::time::Instant, disposition: ParkMissDisposition) {
        match disposition {
            ParkMissDisposition::StandDown(stand_down) => {
                self.stand_down_prelaunched_successor(stand_down);
            }
            ParkMissDisposition::RetryMidSequence { reparks, reason } => {
                if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() {
                    prelaunch.park_mid_sequence_reparks = reparks;
                    prelaunch.park_retry_at = Some(now + PRELAUNCH_PARK_RETRY);
                    aterm_log::debug!(
                        "update apply: {reason}; re-parking at the next batch boundary \
                         (mid-sequence re-park {reparks} of \
                         {PRELAUNCH_MAX_MID_SEQUENCE_REPARKS}, no rung spent)"
                    );
                }
            }
            ParkMissDisposition::Repark { misses, reason } => {
                if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() {
                    prelaunch.park_misses = misses;
                    prelaunch.park_retry_at = Some(now + PRELAUNCH_REPARK_DELAY);
                    aterm_log::warn!(
                        "update apply: the park missed its budget ({reason}); the \
                         successor keeps holding and the park retries on the next \
                         rung (miss {misses} of {PRELAUNCH_MAX_PARK_MISSES})"
                    );
                }
            }
        }
    }

    /// Tells the worker to stand the held successor down with a typed reason.
    /// Idempotent per attempt; the worker's completion clears the attempt record.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn stand_down_prelaunched_successor(&mut self, stand_down: HandoffStandDown) {
        let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() else {
            return;
        };
        if prelaunch.stood_down {
            return;
        }
        prelaunch.stood_down = true;
        prelaunch.park_retry_at = None;
        aterm_log::warn!(
            "update apply: standing the prelaunched successor down — {}",
            stand_down.detail
        );
        if prelaunch.stand_down.try_send(stand_down).is_err() {
            // The worker is past its hold (or gone): the cancel poke reaches every
            // later wait it might be in.
            let _ = prelaunch.cancel.try_send(());
        }
    }

    /// `Wake::UpdateHandoffStandingDown`: the worker is about to revoke a held,
    /// ungranted successor and waits for this answer.
    ///
    /// Runs on the main thread beside the park gate, so no park can be in progress
    /// or start afterwards:
    ///
    ///  1. The gate closes for good: nothing parks onto a successor being killed.
    ///  2. If a park landed, its readers resume now (`UnparkForRepark`); exact,
    ///     since no descriptor was granted, and the user does not wait out the
    ///     grace, SIGKILL and reap frozen.
    ///
    /// The attempt record outlives both until the completion, so no retry launches
    /// beside the candidate; the parked record's deferred teardown and activity
    /// flag move onto it for the completion to replay.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn answer_prelaunched_stand_down(&mut self, attempt_id: u64) {
        let mut ack = None;
        if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut()
            && prelaunch.attempt_id == attempt_id
        {
            prelaunch.stood_down = true;
            prelaunch.park_retry_at = None;
            ack = Some(prelaunch.stand_down_ack.clone());
        }
        if let Some(pending) = self
            .pending_update_handoff
            .take_if(|pending| pending.attempt_id == attempt_id)
        {
            if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut()
                && prelaunch.attempt_id == attempt_id
            {
                prelaunch.teardown.merge(pending.teardown);
                prelaunch.revoked_by_activity |= pending.revoked_by_activity;
            }
            self.rollback_overlap(pending.nonce.as_deref(), &pending.live);
            aterm_log::info!(
                "update apply: readers resumed {} ms after the park — the held successor is \
                 being stood down before the attempt retires",
                pending.park_at.elapsed().as_millis()
            );
        }
        // Answered last, after the gate is shut and readers are back, since the
        // worker revokes on this. A full or disconnected channel means the worker
        // stopped waiting, which it reports itself.
        if let Some(ack) = ack {
            let _ = ack.try_send(());
        }
    }

    /// The launched lane's second half: the park itself, at the dial or the quiet
    /// moment after. Everything that needs stopped readers happens here (registry
    /// projection, descriptor duplicates, window carry, device-term proof
    /// identities, capture ladder, layout, both digests) and crosses to the worker
    /// as one [`HandoffTransferJob`].
    ///
    /// `NotYet` means try again shortly; `Failed` stands the successor down with
    /// the completion's outcome. Every `Failed` after `park_all_readers` has already
    /// rolled the readers back, since the successor holds nothing.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    fn park_and_transfer_to_prelaunched_successor(
        &mut self,
        now: std::time::Instant,
    ) -> ParkAttempt {
        let Some(prelaunch) = self.update_handoff_prelaunch.as_ref() else {
            return ParkAttempt::NotYet("no attempt is prelaunched");
        };
        let attempt_id = prelaunch.attempt_id;
        let mode = prelaunch.mode;
        let apply_attempt = prelaunch.apply_attempt.clone();
        let same_image = prelaunch.same_image;
        let target_build = prelaunch.target_build;
        let target_commit = prelaunch.target_commit.clone();
        let nonce = prelaunch.nonce.clone();
        let cancel = prelaunch.cancel.clone();
        let arbiter = prelaunch.arbiter.clone();
        let dialled = prelaunch.dialled;
        let dialer_pid = dialled.and_then(|dialled| dialled.pid);
        let grant_limit = dialled.and_then(|dialled| dialled.grant_limit);
        let dialled_at = dialled.map_or(now, |dialled| dialled.at);
        let park_misses = prelaunch.park_misses;
        let land_waits = prelaunch.land_waits;
        let build = crate::build_info::build_number()
            .parse::<u64>()
            .unwrap_or(0);
        // The successor's signed handoff policy, keyed by the launched artifact. It
        // only lowers what this park carries and relaxes gates only this producer
        // applies. Re-read at every gate run, since the worker's verdict may have
        // landed since.
        let policy = self.park_policy(apply_attempt.as_ref());
        let ceiling = policy.carry_ceiling();
        // A ceiling carrying no scrollback stops the history export now, before
        // the gate waits on its first pass (`HistoryPlan::Withheld`).
        if !ceiling.carries_scrollback() {
            self.withhold_history_export(target_build, ceiling);
        }

        // Sessions with a live process only: exited panes are left out before the
        // gate reads the masters, so a hung-up master cannot hold or fail the park
        // (`handoff_live_sessions`).
        let live: Vec<(u64, i32, i32)> = self.handoff_live_sessions();
        if live.is_empty() {
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
                detail: "every terminal session closed while the successor booted".to_string(),
            });
        }
        // The scrollback export's first pass comes first (`crate::handoff_history`):
        // it runs with readers live, and sessions it has not reached get only the
        // screen carry's bounded history (counted). Bounded by
        // `handoff_history::EXPORT_PATIENCE` after the dial; waiting freezes nothing.
        // Afterwards the export follows each history until the park stops it. The
        // held panes' passes are waited for too (`HistoryExporter::passed`): they
        // run between the handed sessions' follows (round eight of the update
        // audit, #801), so the wait costs a handed session nothing, and a held pass
        // the patience runs out on stays behind, counted.
        let exporting = self
            .update_handoff_prelaunch
            .as_mut()
            .and_then(|prelaunch| prelaunch.history_export.as_mut())
            .is_some_and(|export| !export.passed());
        if let Some(reason) =
            crate::handoff_history::park_wait(exporting, now.saturating_duration_since(dialled_at))
        {
            self.retire_rim_while_the_gate_waits();
            return ParkAttempt::NotYet(reason);
        }
        // The gate: the only decision that costs the user anything
        // (`prelaunch_park_admitted`). This is about idleness; session death is
        // re-checked under the park below.
        let gate_facts = ParkGateFacts {
            mode,
            phase: self.automatic_phase_for(mode, now),
            activity: self.automatic_activity_facts(mode, now),
            masters_quiet: !handoff_masters_have_activity(&live),
            masters_alive: !handoff_masters_closed(&live),
            land_waits,
            land_gate_relaxed: policy.relaxes_land_gate(),
            held_for: now.saturating_duration_since(dialled_at),
            recording: self.video_request_live(),
        };
        let gate = prelaunch_park_admitted(gate_facts, prelaunch_hold_cap(mode));
        // CONSECUTIVE `Land` waits, counted here where the gate is read (plan
        // P1-3): any other answer, or any other phase, starts the count over.
        let last_wait = self
            .update_handoff_prelaunch
            .as_ref()
            .and_then(|prelaunch| prelaunch.last_wait);
        if let Some(prelaunch) = self.update_handoff_prelaunch.as_mut() {
            prelaunch.land_waits = match gate {
                ParkGate::Wait(_)
                    if gate_facts.phase == crate::native_update_auto_intent::ApplyPhase::Land =>
                {
                    land_waits.saturating_add(1)
                }
                _ => 0,
            };
            if let ParkGate::Wait(reason) = gate {
                prelaunch.last_wait = Some(reason);
            }
        }
        if !gate_facts.masters_quiet && gate == ParkGate::Park && mode.is_automatic() {
            if land_waits < PRELAUNCH_LAND_MAX_WAITS && gate_facts.land_gate_relaxed {
                aterm_log::info!(
                    "update apply: parking over output still queued on a master after \
                     {land_waits} consecutive waits, as build {target_build}'s handoff policy \
                     asks; the successor replays it after Commit"
                );
            } else {
                aterm_log::info!(
                    "update apply: parking at the bound over output still queued on a master \
                     after {land_waits} consecutive waits; the successor replays it after Commit"
                );
            }
        }
        if gate == ParkGate::Park
            && let Some(policy) = policy.policy
        {
            aterm_log::info!(
                "update apply: parking under build {target_build}'s handoff policy ({policy})"
            );
        }
        match gate {
            ParkGate::Park => {}
            ParkGate::Wait(reason) => {
                self.retire_rim_while_the_gate_waits();
                return ParkAttempt::NotYet(reason);
            }
            ParkGate::StandDown(reason) => {
                return ParkAttempt::Failed(HandoffStandDown {
                    outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
                    detail: hold_cap_stand_down_detail(reason, gate_facts.held_for, last_wait),
                });
            }
        }
        // Pause the export before anything freezes: a chunk read beside the park
        // would contend with the capture's locks and the layout's `try_lock`. The
        // pause takes one bounded last look; later output rides the screen carry.
        // Paused, not stopped: any return below that does not hand the capture
        // over leaves the export on the attempt, and the gate resumes it
        // (`try_park_for_prelaunched_successor`), so a re-park joins an export that
        // kept following.
        if let Some(export) = self
            .update_handoff_prelaunch
            .as_mut()
            .and_then(|prelaunch| prelaunch.history_export.as_mut())
        {
            let caught_up = export.caught_up();
            let paused = export.pause(crate::handoff_history::HALT_WAIT);
            if !caught_up {
                aterm_log::warn!(
                    "update apply: the scrollback export ran past {} s after the dial; {} it \
                     before the park — the tabs it had not reached carry the screen carry's \
                     bounded history, counted",
                    crate::handoff_history::EXPORT_PATIENCE.as_secs(),
                    if paused {
                        "paused"
                    } else {
                        "asked to pause (it had not answered)"
                    }
                );
            } else if !paused {
                aterm_log::warn!(
                    "update apply: the scrollback export had not paused {} ms after the park \
                     asked; the park goes ahead, and the worker hands over what it finished",
                    crate::handoff_history::HALT_WAIT.as_millis()
                );
            }
        }
        // This process's own limits are not a verdict about the candidate: each
        // stand-down here other than the park or capture is `ProducerFailed`
        // (Transient), e.g. a tab opened during the hold or `EMFILE`.
        //
        // The limit is the dialled claim's: `transfer` holds the pool to what the
        // successor claimed, not to the cached `Info.plist` declaration. A fork
        // (`grant_limit: None`) passes masters by inheritance, bounded only by the
        // capture's `MAX_HANDOFF_SESSIONS`.
        if let Some(limit) = grant_limit
            && live.len() > limit
        {
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::ProducerFailed,
                detail: format!(
                    "more sessions opened than the successor's descriptor grant carries ({} \
                     open, its rendezvous claim carries at most {limit})",
                    live.len()
                ),
            });
        }
        // Capture the session registry before parking: this projection can fail
        // (`WouldBlock`) and must then leave the terminal untouched. No disk I/O.
        // Hold serials first, as on the fork lane (`hold_serials`).
        let hold_serials = self.hold_serials();
        let manifest = match self.store.try_read() {
            Ok(store) => crate::session_store::SessionHandoff::from_store(&store),
            Err(std::sync::TryLockError::Poisoned(poison)) => {
                let store = poison.into_inner();
                crate::session_store::SessionHandoff::from_store(&store)
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                return ParkAttempt::NotYet("the session registry is busy");
            }
        };
        let manifest = handed_sessions_only(manifest, &live);
        // What the Commit compares again of each record, as drawn here
        // (`session_store::CarriedClaims`, round eight, finding 751).
        let carried_claims = carried_claims_of(&manifest);
        let (owned_masters, adoption) = match reserve_child_only_masters(&live) {
            Ok(reserved) => reserved,
            Err(error) => {
                let kind = error.kind();
                return ParkAttempt::Failed(HandoffStandDown {
                    outcome: crate::UpdateHandoffOutcome::ProducerFailed,
                    detail: format!("could not reserve child-only PTY descriptors ({kind})"),
                });
            }
        };
        let fds = crate::session_store::HandoffFds {
            entries: adoption.clone(),
        };
        let window = self.park_window_carry(target_build);
        // The proof term is the PTY device on this lane (`SCM_RIGHTS` installs the
        // receiver's own numbers); a launched attempt that must fork keeps that
        // term, and the worker names it in the successor's environment.
        #[cfg(target_os = "macos")]
        let Some(proof_identities) =
            crate::handoff_rendezvous::proof_identities_in_device_terms(&adoption)
        else {
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::ProducerFailed,
                detail: "a handed-off PTY would not answer fstat, so the out-of-band proof \
                         term cannot be computed"
                    .to_string(),
            });
        };
        #[cfg(not(target_os = "macos"))]
        let proof_identities = adoption.clone();
        if self.update_handoff_activity_epoch == u64::MAX {
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::ProducerFailed,
                detail: "handoff activity identity space is exhausted".to_string(),
            });
        }
        // The ladder plus this attempt's own misses: a missed park re-parks on the
        // next rung (seed -> 2 × seed, at least 80 -> 250 ms) without giving the
        // booted successor back. Rung 0 is the seed from beside the launch.
        let Some((prior_physical_failures, freeze_budget, freeze_seed)) =
            self.prelaunched_park_budget()
        else {
            return ParkAttempt::NotYet("no attempt is prelaunched");
        };
        #[cfg(test)]
        let freeze_budget = park_budget_for_test::deadline_budget(freeze_budget);
        let freeze_ms = freeze_budget.as_millis();
        let handoff_history_comfort = freeze_budget / 2;
        aterm_log::info!(
            "update apply: freeze budget {freeze_ms} ms ({mode:?}, {prior_physical_failures} prior \
             physical failure(s) of the target artifact incl. {park_misses} park miss(es) of this \
             attempt; {freeze_seed}; running build {build}) — parking {} ms after the successor's \
             dial",
            now.saturating_duration_since(dialled_at).as_millis()
        );
        // The instant the terminal stops echoing: the start of the user's freeze
        // and the zero point of the numbers reported at Commit. The rim starts
        // HERE, not at apply begin (round eight of the update audit, leads U60 and
        // U40): the boot, the dial and the gate's hold before this ran with every
        // reader live, and a rim charged through them pulsed around a working
        // terminal and could reach its cap before this freeze. A re-park after a
        // miss keeps the rim the first park started, its cap counted again from
        // this park.
        self.charge_upgrade_rim_at_park(build);
        let park_at = std::time::Instant::now();
        let deadline = park_at + freeze_budget;
        if !self.park_all_readers(deadline) {
            self.rollback_overlap(None, &live);
            return ParkAttempt::Missed(format!(
                "a PTY reader missed the {freeze_ms} ms handoff park deadline"
            ));
        }
        // Splits the freeze into stopping the readers and the capture after it.
        let readers_parked_at = std::time::Instant::now();
        // Session death is a safety fact: every automatic mode refuses to commit a
        // proof whose live set died.
        if mode.is_automatic() && handoff_masters_closed(&live) {
            self.rollback_overlap(None, &live);
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
                detail: "a PTY session closed during automatic reader park".to_string(),
            });
        }
        let ParkedScreens {
            screens,
            carries,
            screen_digest,
            repaint,
            fg_holders,
            render_debts,
            history_heads,
            held,
        } = match self.capture_parked_screens(
            &live,
            deadline,
            freeze_ms,
            handoff_history_comfort,
            crate::seamless::WireCaps::for_target(target_build),
            ceiling,
        ) {
            Ok(captured) => captured,
            Err(failure) => {
                // Stamped before the rollback, which resumes readers: the split
                // is the freeze's, not the resume's.
                let readers_ms = readers_parked_at
                    .saturating_duration_since(park_at)
                    .as_millis();
                let capture_ms = readers_parked_at.elapsed().as_millis();
                self.rollback_overlap(None, &live);
                aterm_log::info!(
                    "update apply: the park stopped the readers in {readers_ms} ms and its \
                     capture ran {capture_ms} ms before it stopped ({freeze_ms} ms budget)"
                );
                // A parser mid-sequence over a flood is cured by the next batch
                // boundary, not a wider rung: it re-parks at the gate's cadence,
                // bounded ([`mid_sequence_miss_disposition`]).
                if matches!(failure, CaptureFailure::MidSequence { .. }) {
                    return ParkAttempt::MissedMidSequence(failure.to_string());
                }
                // A miss or a refusal, decided by the failure's type: only timing
                // and storage re-park on the next rung.
                return classify_capture_failure(&failure);
            }
        };
        // The foreground holders ride the manifest records, whatever rung.
        let manifest = stamp_fg_holders(manifest, &fg_holders);
        let manifest = stamp_render_debts(manifest, &render_debts);
        let manifest = stamp_journal_keys(manifest, &self.journal_keys());
        // POST-PARK, AND DELIBERATELY SO — see the fork lane for why a pre-park
        // layout capture revoked healthy attempts.
        let layout = self.capture_handoff_layout();
        let layout_digest = match commit_handoff_layout(&layout) {
            Ok(digest) => digest,
            Err(refusal) => {
                self.rollback_overlap(None, &live);
                // Deterministic: the same layout refuses on every rung.
                return ParkAttempt::Refused(refusal);
            }
        };
        if std::time::Instant::now() >= deadline {
            self.rollback_overlap(None, &live);
            return ParkAttempt::Missed(format!(
                "handoff proof capture exceeded the {freeze_ms} ms deadline"
            ));
        }
        // The park landed within budget: its cost (including the readers' stop,
        // which the dry run cannot time) seeds the next update's first rung.
        let _ledger_write = keep_landed_park(park_at.elapsed());
        let activity_epoch = self.update_handoff_activity_epoch;
        let proof_deadline = handoff_proof_deadline(mode, prior_physical_failures, park_at);
        self.pending_update_handoff = Some(crate::PendingUpdateHandoff {
            attempt_id,
            park_at,
            proof_ready_at: None,
            // Sampled at the park: whether the user was looking at aterm when the
            // screen it will hand over was the one on glass.
            #[cfg(target_os = "macos")]
            activate_at_commit: self.any_os_window_focused(),
            nonce: Some(nonce),
            live: live.clone(),
            // The PROOF identities, in the device term — see
            // `HandoffCapture::proof_identities`. Never `live`.
            adoption: proof_identities.clone(),
            child_pid: dialer_pid,
            mode,
            apply_attempt,
            same_image,
            target_build,
            target_commit,
            layout: layout.clone(),
            layout_digest,
            screen_digest,
            activity_epoch,
            hold_serials,
            carried_claims,
            cancel,
            arbiter,
            teardown: crate::DeferredHandoffTeardown::None,
            commit_drain_started: None,
            commit_flush: None,
            held_commit: None,
            revoked_by_activity: false,
        });
        let transfer = HandoffTransferJob {
            capture: HandoffCapture {
                #[cfg(target_os = "macos")]
                park_at,
                #[cfg(target_os = "macos")]
                captured_at: std::time::Instant::now(),
                manifest,
                fds,
                screens,
                repaint,
                carries,
                window,
                layout,
                layout_digest,
                screen_digest,
                live: adoption,
                proof_identities,
                _owned_masters: owned_masters,
                history_heads,
                // Taken here, after every point a park can miss: a re-park keeps
                // the export it waited for, resumed after the miss
                // (`HistoryExporter::resume`). Handed over paused; the worker's
                // `finish` stops it. Withheld, and stopped, under a ceiling that
                // carries no scrollback.
                history: crate::handoff_history::HistoryPlan::exported_under(
                    ceiling,
                    self.update_handoff_prelaunch
                        .as_mut()
                        .and_then(|prelaunch| prelaunch.history_export.take()),
                ),
                held,
            },
            proof_deadline,
        };
        let carried = transfer.capture.screens.len();
        let proof_budget_ms = transfer
            .proof_deadline
            .saturating_duration_since(park_at)
            .as_millis();
        let sent = self
            .update_handoff_prelaunch
            .as_ref()
            .is_some_and(|prelaunch| prelaunch.transfer.try_send(transfer).is_ok());
        if !sent {
            self.pending_update_handoff = None;
            self.rollback_overlap(None, &live);
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::Rejected,
                detail: "overlap handoff worker stopped before the transfer".to_string(),
            });
        }
        // A live `video` take is not answered here: the proof may never come and
        // the attempt roll back. Commit answers it and waits for the reply to
        // reach its client (`App::video_answer_before_commit`).
        aterm_log::info!(
            "update apply: parked and captured {carried} screen(s) in {} ms (the readers stopped \
             in {} ms, the capture and its hand-over took {} ms); the capture is on its way to \
             the held successor (proof due {proof_budget_ms} ms after the park)",
            park_at.elapsed().as_millis(),
            readers_parked_at
                .saturating_duration_since(park_at)
                .as_millis(),
            readers_parked_at.elapsed().as_millis()
        );
        ParkAttempt::Parked
    }

    /// The pre-Commit input-drain gate. Returns `None` when the completion was
    /// re-posted for another drain spin (the caller must return), otherwise the
    /// completion plus the `input_dispatch_fenced` / `egress_settled` admission
    /// facts.
    ///
    /// Hardware events accepted just before this callback may not have been
    /// dispatched through winit yet and would die with `_exit`. Commit waits across
    /// a dispatch fence counted in loop iterations that really dispatched events:
    /// re-posting this completion lets the run loop deliver them to the still-open
    /// masters, and the re-post re-runs this admission. Bounded by the 3 s drain
    /// deadline (the spin cap is a backstop) and by the worker's 15 s decision
    /// deadline. A failed re-post means the loop is closing; the dropped reject
    /// sender makes the worker reject and reap.
    ///
    /// Dispatched keystrokes can also sit in process-local queues (the paste-order
    /// FIFO, a wedged tty's sink spill) that die with `_exit`, so Commit also waits
    /// for every handed session's egress to reach the kernel
    /// (`handoff_egress_settled`); an unflushable spill fails closed (rollback).
    ///
    /// No quiet window is waited for: `user_input_recent` reads the machine-wide
    /// HID idle clock, and after the reveal the successor's own keystrokes would
    /// hold the parent's gate open. The gate settles at its ~30-45 ms floor.
    #[cfg(unix)]
    fn handoff_drain_gate(
        &mut self,
        completion: crate::UpdateHandoffCompletion,
    ) -> Option<(crate::UpdateHandoffCompletion, bool, bool)> {
        const HANDOFF_INPUT_DRAIN_SPIN_CAP: u32 = 4_000;
        /// MANDATORY minimum event-loop time between ProofReady and Commit.
        const HANDOFF_INPUT_DISPATCH_FENCE: std::time::Duration =
            std::time::Duration::from_millis(30);
        /// Absolute wall-clock backstop. An egress queue that never settles
        /// must fail CLOSED (rollback), never `_exit` over undelivered bytes.
        const HANDOFF_INPUT_DRAIN_DEADLINE: std::time::Duration =
            std::time::Duration::from_millis(3_000);
        // A vanished pending attempt cannot be drained toward Commit; fall through
        // to the rejection path rather than respin.
        let drained_for = {
            let now = std::time::Instant::now();
            self.pending_update_handoff.as_mut().map(|pending| {
                // The first ProofReady observation stamps when the successor's proof
                // landed, splitting park→proof from proof→commit.
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
        let held = self
            .pending_update_handoff
            .as_ref()
            .is_some_and(|pending| pending.held_commit.is_some());
        if (!input_dispatch_fenced || !egress_settled)
            && completion.input_drain_spins < HANDOFF_INPUT_DRAIN_SPIN_CAP
            && drained_for < HANDOFF_INPUT_DRAIN_DEADLINE
            && self.handoff_loop_present()
        {
            // Yield so the run loop dispatches the queued NSEvents before the
            // re-post comes back. The frozen frame is already parked.
            std::thread::sleep(HANDOFF_INPUT_DRAIN_YIELD);
            let respin = crate::UpdateHandoffCompletion {
                input_drain_spins: completion.input_drain_spins.saturating_add(1),
                ..completion
            };
            return match self.repost_handoff_completion(respin) {
                Ok(()) => None,
                // The loop is closing. An attempt that holds its Commit's claim
                // ([`HeldCommit`]) has nobody else to finish it — the worker
                // cannot reject a claimed attempt — so its admission is read
                // now, unfenced, and refuses it: the claim goes to the
                // rejection, as any refusal's does.
                Err(respin) if held => Some((*respin, false, egress_settled)),
                // Otherwise the dropped completion's reject sender makes the
                // worker reject and reap.
                Err(_) => None,
            };
        }
        Some((completion, input_dispatch_fenced, egress_settled))
    }

    /// Whether a handoff completion can go back through this thread's event loop
    /// ([`Self::repost_handoff_completion`]): there is one (`headless_for_test`
    /// has none, unless the test takes the re-posts itself).
    #[cfg(unix)]
    fn handoff_loop_present(&self) -> bool {
        #[cfg(test)]
        if self.handoff_reposts.is_some() {
            return true;
        }
        self.proxy.is_some()
    }

    /// Post `completion` back to this thread's event loop, which dispatches the
    /// input queued to this process before it comes back ([`Self::handoff_drain_gate`]);
    /// `Err` hands it back when the loop is closing. A test that set
    /// `handoff_reposts` takes it there and feeds it back itself.
    #[cfg(unix)]
    fn repost_handoff_completion(
        &mut self,
        completion: crate::UpdateHandoffCompletion,
    ) -> Result<(), Box<crate::UpdateHandoffCompletion>> {
        #[cfg(test)]
        if let Some(reposts) = self.handoff_reposts.as_mut() {
            reposts.push(completion);
            return Ok(());
        }
        let Some(proxy) = self.proxy.clone() else {
            return Err(Box::new(completion));
        };
        proxy
            .send_event(Wake::UpdateHandoffFinished(completion))
            .map_err(|closed| {
                let Wake::UpdateHandoffFinished(completion) = closed.0 else {
                    unreachable!("a closed loop hands back the wake it was sent");
                };
                Box::new(completion)
            })
    }

    /// The Commit's messages flush, run once per attempt BEFORE the claim
    /// ([`Self::finish_proven_handoff`], round eight of the update audit,
    /// finding 18): `true` when it took long enough that input may have
    /// queued behind it ([`HANDOFF_INPUT_DRAIN_YIELD`], the grain the dispatch
    /// fence is measured in), so the attempt goes through the drain gate again,
    /// its clock restarted. What the flush lands is lost to no rollback, which
    /// is why it may run before the claim; whether the writer answered decides
    /// the flush after it.
    #[cfg(unix)]
    fn commit_flush_refences(&mut self) -> bool {
        if self
            .pending_update_handoff
            .as_ref()
            .is_none_or(|pending| pending.commit_flush.is_some())
        {
            return false;
        }
        // With no writer behind it the flush waits on nothing: its hold is
        // none, whatever the scheduler did meanwhile (a descheduled main
        // thread here is no different from one anywhere else in the loop).
        let writer = self.messages_log.is_some();
        let started = std::time::Instant::now();
        let landed = self.flush_messages_log_within(MESSAGES_FLUSH_COMMIT_WAIT);
        let held = if writer {
            started.elapsed()
        } else {
            std::time::Duration::ZERO
        };
        self.settle_commit_flush(landed, held)
    }

    /// What the Commit's flush leaves on the attempt, given whether the writer
    /// answered (`landed`) and how long waiting on it `held` the main thread:
    /// the writer's answer is kept, and a hold of a fence grain or more
    /// ([`HANDOFF_INPUT_DRAIN_YIELD`]) restarts the fence's clock.
    #[cfg(unix)]
    fn settle_commit_flush(&mut self, landed: bool, held: std::time::Duration) -> bool {
        let refence = held >= HANDOFF_INPUT_DRAIN_YIELD;
        if let Some(pending) = self.pending_update_handoff.as_mut() {
            pending.commit_flush = Some(landed);
            if refence {
                pending.commit_drain_started = None;
            }
        }
        refence
    }

    /// THE PAIRS THIS PROCESS'S CRASH JOURNAL KEYS ITS HANDED SHELLS BY, when
    /// that journal still holds the image this process took over at its own
    /// Commit (`crash_journal::Lane::inherited_image_parent`; round seven of the
    /// update audit, leftover I2b): for each session the update's parent
    /// handed it, the pair the parent's record said that image keys it by
    /// (`Session::journal_key`, when the parent had not replaced ITS parent's
    /// image either), else the parent's own pair for it. Empty once this
    /// process has an image of its own on disk — its successor's registrations
    /// then name this process's pairs, which that image's leaves carry.
    #[cfg(unix)]
    pub(crate) fn journal_keys(&self) -> Vec<(u64, (u32, u64))> {
        let Some(parent) = self.crash_journal.inherited_image_parent() else {
            return Vec::new();
        };
        self.pool
            .sessions
            .iter()
            .filter_map(|(id, pooled)| {
                let session = &pooled.session;
                session
                    .journal_key
                    .or_else(|| session.handoff_local_id.map(|was| (parent, was)))
                    .map(|key| (*id, key))
            })
            .collect()
    }

    /// Every pooled session's hold serial, summed
    /// ([`crate::fabric::SessionFabric::hold_serial`]): the park's reading of where
    /// every halt stands, compared again at Commit.
    ///
    /// The counters only grow, so any hold moving changes the sum (a session
    /// joining or leaving is refused by `exact_sessions`). Per session, so the
    /// reading is this `App`'s. One short fabric lock each, never under a
    /// `Terminal` or the registry.
    #[cfg(unix)]
    pub(crate) fn hold_serials(&self) -> u64 {
        self.pool
            .iter()
            .map(|session| session.ctx.fabric.carry_serial())
            .fold(0, u64::wrapping_add)
    }

    /// Whether every handed session's [`crate::session_store::CarriedClaims`] —
    /// its topics, keyed escalations and supervisor claim — still reads as the
    /// park drew it (`drawn`, the pending attempt's). A session no longer pooled
    /// reads as moved; `exact_sessions` names that too. One short meta lock and
    /// one topics lock each, never under a `Terminal` or the registry.
    #[cfg(unix)]
    pub(crate) fn carried_claims_held(
        &self,
        drawn: &[crate::session_store::CarriedClaims],
    ) -> bool {
        let now_us = crate::metrics::now_us();
        let own_holder = crate::harness_host::holder();
        drawn.iter().all(|claims| {
            self.pool.get(claims.local_id).is_some_and(|session| {
                crate::session_store::CarriedClaims::of_session(
                    claims.local_id,
                    &session.ctx,
                    now_us,
                    &own_holder,
                ) == *claims
            })
        })
    }

    /// Whether any pooled session's timeline or inbox has handed out an id past the
    /// manifest's last id plus its overlap reserve
    /// ([`crate::session_timeline::SessionTimeline::past_handoff_ceiling`],
    /// [`crate::fabric::SessionFabric::past_handoff_ceiling`]): the successor would
    /// reuse it, so the Commit stands down. Ordinary overlap traffic never does.
    /// Read after the fence; one short lock each, in turn.
    #[cfg(unix)]
    fn ids_past_handoff_ceiling(&self) -> bool {
        self.pool.iter().any(|session| {
            let timeline = session
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .past_handoff_ceiling();
            timeline || session.ctx.fabric.past_handoff_ceiling()
        })
    }

    /// [`Self::hold_serials`] as the Commit reads it: each serial read under the
    /// guard that fences it ([`crate::fabric::SessionFabric::fence_hold`]), so no
    /// `hold` lands unseen until the process exits. The fence lifts when the guard
    /// drops, on every stand-down path; a landed Commit `_exit`s with it raised.
    ///
    /// `successor_reads_carry`: a same-or-newer successor carries the outbox on, so
    /// the fence also holds the bridge's retirements back; for an older successor
    /// (downgrade, channel switch) they are answered here, since an unanswered one
    /// would leave a sequence pin that swallows its own post.
    #[cfg(unix)]
    fn fence_hold_serials(&self, successor_reads_carry: bool) -> (u64, HoldFence) {
        let fabrics: Vec<std::sync::Arc<crate::fabric::SessionFabric>> = self
            .pool
            .iter()
            .map(|session| std::sync::Arc::clone(&session.ctx.fabric))
            .collect();
        let serials = fabrics
            .iter()
            .map(|fabric| fabric.fence_hold(successor_reads_carry))
            .fold(0, u64::wrapping_add);
        (serials, HoldFence(fabrics))
    }

    /// Snapshots the pending attempt and collects every Commit admission fact
    /// ([`HandoffCommitFacts`]). `None` when the pending attempt vanished mid-drain
    /// (the caller rejects). Also returns the fresh native-safety evidence (`Err`
    /// carries the reasons), the exact adoption proof and the attempt arbiter.
    #[cfg(unix)]
    #[allow(clippy::type_complexity)]
    fn collect_handoff_commit_facts(
        &mut self,
        nonce: Option<&str>,
        input_dispatch_fenced: bool,
        egress_settled: bool,
        commit_channel: bool,
        operator: Option<crate::operator_host::UpdateQuiesceRefusal>,
        held_fence: Option<HoldFence>,
    ) -> Option<(
        HandoffCommitFacts,
        Result<crate::app_native::NativeUpdateSafetyToken, Vec<String>>,
        Option<crate::seamless::AdoptionProof>,
        crate::HandoffAttemptArbiter,
        HoldFence,
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
        let pending_hold_serials = pending.hold_serials;
        let pending_carried_claims = pending.carried_claims.clone();
        let arbiter = pending.arbiter.clone();
        let teardown_allows_commit = matches!(
            pending.teardown,
            crate::DeferredHandoffTeardown::None | crate::DeferredHandoffTeardown::CleanQuitReady
        );
        // A hold that moved since the park is structural activity: the successor's
        // manifest has the holds as the park drew them. Read under the fence
        // (`fence_hold_serials`), which holds later holds back until the attempt
        // exits or stands down; this is the last comparison. A held claim
        // ([`HeldCommit`]) reads them under the fence it raised at the claim.
        let (commit_hold_serials, hold_fence) = match held_fence {
            Some(fence) => (fence.serials(), fence),
            None => self.fence_hold_serials(successor_reads_fabric_carry(
                pending_target_build,
                crate::running_build_number(),
            )),
        };
        // Minting more turn ids in the overlap than the manifest's reserve
        // (`control::TURN_ID_RESERVE`) would share one with the successor: a
        // lossless retry. Other overlap turns are tolerated.
        let turn_ids_held =
            !crate::control::turn_ids_past_handoff_ceiling(arbiter.turn_id_ceiling());
        if !turn_ids_held {
            aterm_log::warn!(
                "update apply: this process minted more turn ids since the handoff manifest \
                 than its reserve holds; standing the Commit down so no id is minted twice"
            );
        }
        // A topic, escalation or claim that moved since the park's draw is in no
        // manifest: a lossless retry. Read after the fence, under which no
        // `topic` change lands (`fabric::cmd_topic`).
        let claims_held = self.carried_claims_held(&pending_carried_claims);
        if !claims_held {
            aterm_log::warn!(
                "update apply: a session's topics, escalations or supervisor claim moved since \
                 the handoff manifest was drawn; standing the Commit down so the next attempt \
                 carries them"
            );
        }
        let exact_activity = self.update_handoff_activity_epoch == pending_activity_epoch
            && commit_hold_serials == pending_hold_serials
            && !self.ids_past_handoff_ceiling()
            && turn_ids_held
            && claims_held;
        // The live set as the park drew it: an exited pane left out stays out, but a
        // handed session that exited during the overlap stays in, so
        // `sessions_alive` names its death. Both are activity.
        let exited = self.handoff_exited_session_ids();
        let mut current_live: Vec<(u64, i32, i32)> = self
            .pool
            .iter()
            .filter(|s| {
                !exited.contains(&s.id) || pending_live.iter().any(|(id, _, _)| *id == s.id)
            })
            .map(|s| (s.id, s.master, s.pid))
            .collect();
        current_live.sort_unstable();
        let mut expected_live = pending_live.clone();
        expected_live.sort_unstable();
        let exact_sessions = current_live == expected_live;
        // Compare topology, not the raw capture (`commit_layout_topology`). The raw
        // inequality is still logged: it is where a window drag or contended session
        // lock shows up, neither of which may be reported as a topology change.
        let live_layout = self.capture_handoff_layout();
        let exact_layout =
            commit_layout_topology(&live_layout) == commit_layout_topology(&pending_layout);
        // The carried agents are compared apart: a relaunch owed or settled between
        // the two captures is not topology (`commit_agent_carry`).
        let exact_agent_carry =
            commit_agent_carry(&live_layout) == commit_agent_carry(&pending_layout);
        if !exact_agent_carry {
            aterm_log::info!(
                "update apply: a hosted agent's relaunch became owed (or was settled) after the \
                 handoff layout was captured; standing the Commit down so the successor is not \
                 handed a layout without it — this process relaunches it, and the next attempt \
                 carries what is still owed"
            );
        }
        if exact_layout && exact_agent_carry && live_layout != pending_layout {
            aterm_log::info!(
                "update apply: the Commit-time capture differs from the committed snapshot \
                 only in window position, window show state or degradable session metadata \
                 — topology is unchanged, so Commit stays admitted"
            );
        }
        let parent_still_parked = self
            .pool
            .iter()
            .all(|session| session.reader_join.is_none());
        let native_safety = self.revalidate_native_update_safety();
        // Death-only peek: queued output on a master is tolerated (it
        // waits in the kernel for the child); HUP/ERR means the adopted
        // live-set identity is already stale and must reject. A program that
        // exits with output still queued is not seen here: its leader waits in
        // the tty drain (`?Es`, measured in 706eb1ea0) until the successor's
        // reader takes the output, so its master shows no hangup and the
        // Commit lands. The successor then closes that pane only after the keys
        // typed into it during the freeze replayed into it (round eight of the
        // update audit, finding 752: `App::defers_wake_until_activation`).
        let sessions_alive = !handoff_masters_closed(&pending_live);
        let (operator_idle, operator_quiesced) = operator_commit_facts(operator);
        let proof = nonce.and_then(|nonce| {
            crate::seamless::adoption_proof(
                nonce,
                pending_target_build,
                &pending_target_commit,
                &pending_layout_digest,
                &pending_screen_digest,
                &pending_adoption,
            )
            .ok()
        });
        let facts = HandoffCommitFacts {
            exact_sessions,
            exact_layout,
            exact_agent_carry,
            exact_activity,
            teardown_allows_commit,
            parent_still_parked,
            sessions_alive,
            input_dispatch_fenced,
            egress_settled,
            native_safe: native_safety.is_ok(),
            proof_exact: proof.is_some(),
            commit_channel,
            operator_idle,
            operator_quiesced,
        };
        Some((facts, native_safety, proof, arbiter, hold_fence))
    }

    /// Main-thread completion of the asynchronous overlap proof. `ProofReady` alone
    /// is not enough: native state and the live PTY set may have changed while the
    /// child booted. Only a fresh proof plus unchanged sessions authorizes the
    /// destructor-free parent exit.
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
        #[cfg(any(target_os = "macos", all(test, unix)))]
        let matches_pending = matches_pending
            || self
                .update_handoff_prelaunch
                .as_ref()
                .is_some_and(|prelaunch| prelaunch.attempt_id == completion.attempt_id);
        if !matches_pending {
            // A stale proof can never authorize Commit. Ask its still-owning worker
            // to kill/reap readerless child; never wait on the event loop.
            let attempt_id = completion.attempt_id;
            let _ = deliver_handoff_rejection(completion.reject);
            aterm_log::warn!("update apply: ignored stale handoff completion {attempt_id}");
            return;
        }
        if completion.outcome == crate::UpdateHandoffOutcome::ProofReady {
            self.finish_proven_handoff(completion);
            return;
        }

        // Every non-ready completion is emitted only after the worker killed and
        // reaped the child. It is now safe to restore parent readers and reduce the
        // exact ticket using disk facts that were also collected off the UI thread.
        let Some(teardown) = self.reduce_returned_handoff_completion(completion) else {
            return;
        };
        // A returned attempt is still this build: undo the successor's title the
        // ProofReady edge may have published (`publish_handoff_target_to_menu_bar`).
        self.refresh_version_menu();
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

    /// The `ProofReady` arm of [`Self::finish_update_handoff`]: the drain gate,
    /// the final admission, the messages flush under its own fence, then the
    /// claim and the Commit — or the rejection.
    #[cfg(unix)]
    fn finish_proven_handoff(&mut self, completion: crate::UpdateHandoffCompletion) {
        let first_proof = {
            let Some(pending) = self.pending_update_handoff.as_mut() else {
                if let Some(reject) = completion.reject {
                    let _ = reject.try_send(());
                }
                return;
            };
            pending.nonce = completion.nonce.clone();
            pending.child_pid = completion.child_pid;
            pending.proof_ready_at.is_none()
        };
        // THE BAR NAMES THE SUCCESSOR BEFORE THIS PROCESS DIES (2026-09-24):
        // once, at the proof's first observation, so the dispatch fence below
        // gives AppKit loop time to publish it before Commit. The bar may keep
        // this process's last menu long after `_exit` — nothing need claim it —
        // so that menu must already read the build that is about to run.
        if first_proof && let Some(title) = self.publish_handoff_target_to_menu_bar() {
            aterm_log::info!("update apply: Version menu now reads the successor's {title}");
        }
        let Some((completion, input_dispatch_fenced, egress_settled)) =
            self.handoff_drain_gate(completion)
        else {
            // Re-posted for another drain spin (or the event loop is
            // closing and the drop rejects the attempt).
            return;
        };
        // A COMMIT THAT HOLDS ITS CLAIM comes back here once more ([`HeldCommit`];
        // round eight of the update audit, #18): what it answered at the claim
        // took real time, so it went through the dispatch fence again. It keeps
        // the operator's quiesce and the hold fence it took at the claim, reads
        // the admission again, and commits; refused, its claim goes to the
        // rejection below.
        let held = self
            .pending_update_handoff
            .as_mut()
            .and_then(|pending| pending.held_commit.take());
        let claimed = held.is_some();
        // Quiesce the resident operator only for the final admission seam.
        // A rejection drops this reversible token; a successful Commit
        // `_exit`s while its gate is still held by `with_commit_permit`.
        // Its answer is two of the admission's facts (round eight, #70):
        // a busy operator refuses the Commit as activity, never as a fault.
        let (operator_quiesce, mut operator_refusal, held_fence) = match held {
            Some(held) => (held.operator_quiesce, None, Some(held.hold_fence)),
            None => match self.operator_control.as_ref() {
                Some(control) => match control.try_begin_update_quiesce() {
                    Ok(quiesce) => (Some(quiesce), None, None),
                    Err(refusal) => (None, Some(refusal), None),
                },
                None => (None, None, None),
            },
        };
        // `hold_fence` stays raised to the end of this arm: through the
        // Commit's `_exit`, or until the rejection below is under way (a held
        // claim keeps the one it raised at the claim).
        let Some((mut facts, native_safety, proof, arbiter, hold_fence)) = self
            .collect_handoff_commit_facts(
                completion.nonce.as_deref(),
                input_dispatch_fenced,
                egress_settled,
                completion.commit_fd.is_some(),
                operator_refusal,
                held_fence,
            )
        else {
            // The pending attempt vanished mid-drain: nothing can be
            // committed; ask the worker to reject and reap.
            if let Some(reject) = completion.reject {
                let _ = reject.try_send(());
            }
            return;
        };
        let commit_admitted = handoff_commit_admitted(facts);
        // THE MESSAGES FLUSH RUNS BEFORE THE CLAIM, UNDER THE FENCE (round
        // eight, finding 18): it blocks the main thread for up to
        // `MESSAGES_FLUSH_COMMIT_WAIT`, and a key the OS queued meanwhile
        // would die with the `_exit`. A flush that took real time sends the
        // attempt through the drain gate again, which dispatches that input
        // into the masters, and the admission is read again. Nothing it does
        // is lost to a rollback, so it may run before the claim; the video
        // answer may not, and stays after it.
        let completion =
            if !claimed && commit_admitted && proof.is_some() && self.commit_flush_refences() {
                match self.handoff_drain_gate(crate::UpdateHandoffCompletion {
                    input_drain_spins: 0,
                    ..completion
                }) {
                    Some((completion, ..)) => completion,
                    None => return,
                }
            } else {
                completion
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
        let admitted = commit_admitted && commit_fd.is_some() && proof.is_some();
        // THE CLAIM: a held claim has it already; otherwise it is taken here, and
        // what must be answered before the `_exit` is answered under it.
        let claim = claimed || (admitted && arbiter.try_begin_commit());
        let commit_lost_arbiter = admitted && !claim;
        let mut commit_write_failed = false;
        if claim && !claimed {
            let answered_in = self.answer_at_commit_claim(child_pid);
            // WHAT THE CLAIM ANSWERED TOOK REAL TIME, SO THE COMMIT GOES THROUGH
            // THE DISPATCH FENCE AGAIN, HOLDING ITS CLAIM (round eight of the
            // update audit, #18). The video answer runs only under the claim
            // (round six, #22: a take aborted before it was lost to every
            // rollback after it), and it can block this thread for up to
            // `VIDEO_EXPORT_COMMIT_WAIT` on a wedged encode worker or client, the
            // messages flush for up to `MESSAGES_FLUSH_COMMIT_WAIT`. A key the OS
            // queued to this process meanwhile died with the `_exit` whenever the
            // successor had not taken the keyboard. So the claim is kept
            // ([`HeldCommit`]: the operator's quiesce and the hold fence with
            // it; the worker cannot reject a claimed attempt), the run loop
            // dispatches that input into the masters, and the next pass reads
            // the admission again and commits — or, refused, rolls back like an
            // EPIPE after the claim, the answered take gone with it. The same
            // grain as the flush's own fence (`commit_flush_refences`).
            if answered_in >= HANDOFF_INPUT_DRAIN_YIELD {
                aterm_log::info!(
                    "update apply: the Commit's answers took {} ms; its claim is held through \
                     one more input-dispatch fence before the write",
                    answered_in.as_millis()
                );
                if let Some(pending) = self.pending_update_handoff.as_mut() {
                    pending.commit_drain_started = None;
                    pending.held_commit = Some(HeldCommit {
                        operator_quiesce,
                        hold_fence,
                    });
                }
                // Into the held pass: the drain gate re-posts it and returns,
                // and with no loop to go through it reads the admission at once.
                self.finish_proven_handoff(crate::UpdateHandoffCompletion {
                    attempt_id,
                    nonce,
                    child_pid,
                    outcome: _outcome,
                    commit_fd,
                    reject,
                    reconcile: _reconcile,
                    detail: _detail,
                    input_drain_spins: 0,
                    child_death: _child_death,
                });
                return;
            }
        }
        if claim
            && admitted
            && let (Some(commit_fd), Some(proof)) = (commit_fd.as_ref(), proof)
        {
            // THE ROWS THE CARRY DID NOT TAKE END WITH THIS PROCESS (ruling
            // 441 (d)): an unrevealed row, the strain level, a row posted or
            // revealed since the park (a held claim's fence among it), recorded
            // as ended here and flushed below, never replayed open in the
            // successor as cut off by a crash. Here, after the admission's LAST
            // reading, never at the claim: a held claim reads it again after its
            // fence, and a refusal there keeps this process running with its
            // rows, which their reporters still restate and end by id. Only the
            // operator's permit and the write itself can still return (an
            // EPIPE); the rejection's caller syncs the band after either.
            let closed = self.close_uncarried_messages();
            // A HELD CLAIM'S RECORD LANDS TOO (round-seven update audit, finding
            // 53), and so do the endings just closed: what the fence's turns of
            // the loop recorded is flushed, waited for one grain of the fence at
            // most (`HANDOFF_INPUT_DRAIN_YIELD`), which an idle writer answers
            // well within: a longer wait would be one more interval for keys to
            // queue in and die (#18). A writer that did not answer before is
            // not waited for again.
            if (claimed || closed > 0)
                && !matches!(
                    self.pending_update_handoff
                        .as_ref()
                        .map(|pending| pending.commit_flush),
                    Some(Some(false))
                )
            {
                self.flush_messages_log_within(HANDOFF_INPUT_DRAIN_YIELD);
            }
            // THE HANDOFF WINDOW: between this process's `_exit` and the
            // successor publishing its own entries, name the successor as
            // the holder of every carried id, so a launch landing there is
            // refused (`identity_claim` module header, the third gate).
            // Withdrawn below if the Commit fails and we keep the ids.
            let carried: Vec<aterm_session::SessionId> = self
                .store
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .snapshot()
                .into_iter()
                .map(|h| h.sid.clone())
                .collect();
            if let Some(pid) = child_pid {
                crate::identity_claim::mark_successor(&carried, pid);
            }
            // THE SUPERVISOR HOST STOPS AT THE WRITE, after the admission's
            // last reading (the review of round eight's #18). `suspend` sets
            // every worker's stop flag and cuts its connection before it
            // returns, and a cut worker's transport refuses every press or
            // typed request (one already on the wire is the only exception);
            // the threads are reaped later, off this thread. The successor
            // resumes its own at the same Commit, and a Commit that returns
            // resumes this one, below. It used to stop at the claim, before the
            // answers: a stopped worker's last act clears the badges it raised
            // (`meta unset attention owner=supervisor`, on a fresh connection
            // the cut allows), and a claim held through the dispatch fence
            // ([`HeldCommit`]) read its own stop's clearing as a supervisor
            // escalation moved since the park (`carried_claims_held`): the
            // Commit was refused as activity, its successor killed, at every
            // retry while a supervised tab waited at a question box. Through
            // the fence the workers run as they do before any claim, and
            // whatever they move there is the held pass's admission to read.
            if let Some(host) = self.harness.as_ref() {
                host.suspend();
            }
            // THE CONNECTIONS THE MANIFEST LEFT OUT ARE AUDITED AS DROPPED
            // HERE (round eight of the update audit, U23's review), with the
            // Commit's permit in hand, right before its one write: not at the
            // write of the manifest, where an attempt that then stood down
            // put a dissolution that never happened on record, again at
            // every retry.
            let left_out = arbiter.connections_left_out();
            // Success cannot return: `commit_and_exit` performs the one
            // atomic <=PIPE_BUF write and `_exit(0)` in the same typed
            // operation. EPIPE explicitly transfers Committing back to
            // Rejecting so one reaper can restore the parent. The successor
            // is activated inside the operator's permit, right before the
            // write: a permit refused activated nothing.
            let commit_result = match operator_quiesce.as_ref() {
                Some(quiesce) => quiesce.with_commit_permit(|| {
                    #[cfg(target_os = "macos")]
                    self.activate_successor_at_write(child_pid);
                    Self::audit_connections_left_out(left_out);
                    crate::seamless::commit_and_exit(commit_fd, proof)
                }),
                None => {
                    #[cfg(target_os = "macos")]
                    self.activate_successor_at_write(child_pid);
                    Self::audit_connections_left_out(left_out);
                    Ok(crate::seamless::commit_and_exit(commit_fd, proof))
                }
            };
            if commit_result.as_ref().is_ok_and(Result::is_err) && !left_out.is_empty() {
                aterm_log::warn!(
                    "update apply: the Commit was not written, so the {} connection \
                     drop(s) just audited for it did not happen: this process keeps \
                     every one of those connections",
                    left_out.len()
                );
            }
            if let Some(host) = self.harness.as_ref() {
                host.resume();
            }
            if let Some(pid) = child_pid {
                crate::identity_claim::withdraw_successor_markers(&carried, pid);
            }
            match commit_result {
                Ok(Err(_)) => commit_write_failed = true,
                // The permit is the operator's last answer before the
                // `_exit`, so it re-reads the operator's two facts: a
                // gate held for a moment is activity here as well.
                Err(refusal) => {
                    operator_refusal = Some(refusal);
                    (facts.operator_idle, facts.operator_quiesced) =
                        operator_commit_facts(operator_refusal);
                }
                Ok(Ok(never)) => match never {},
            }
            let _ = arbiter.commit_failed_to_rejecting();
        } else if claim {
            // A HELD CLAIM THE ADMISSION REFUSED after its fence (#18): the desk
            // changed while the run loop dispatched, or the input did not reach
            // the masters in time. The claim goes to the rejection, as a Commit
            // write's EPIPE does; what the claim answered stays answered. The
            // supervisor host was never stopped, and the successor never
            // activated: both happen only at the write, so the successor this
            // rejection kills never had the keyboard.
            aterm_log::warn!(
                "update apply: the Commit's admission, read again after the fence its claim \
                 was held through, refused it; a live video request it answered stays \
                 answered"
            );
            let _ = arbiter.commit_failed_to_rejecting();
        }

        let rejection = handoff_rejection_reason(
            facts,
            &native_safety,
            operator_refusal,
            commit_lost_arbiter,
            commit_write_failed,
        );
        let activity_shaped = handoff_rejection_activity_shaped(facts);
        if activity_shaped && let Some(pending) = self.pending_update_handoff.as_mut() {
            pending.revoked_by_activity = true;
        }
        aterm_log::warn!("update apply: {rejection}; rejecting readerless child");
        #[cfg(test)]
        self.handoff_rejections.push(rejection.clone());
        let rejection_started =
            arbiter.try_begin_reject() || arbiter.phase() == crate::HandoffAttemptPhase::Rejecting;
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
                        // QoS (port of 61a6c8b62): a REAPER is `Background`,
                        // never `Housekeeping` — it enforces a kill-and-reap
                        // deadline, and a starved reaper leaks the process
                        // whose CPU use is the problem (see `qos::Role`).
                        crate::qos::set_self(crate::qos::Role::Background);
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
    }

    /// What a Commit answers once it has claimed the attempt, before its write
    /// ([`Self::finish_proven_handoff`]): the freeze's two numbers and the
    /// landing in the log, every live `video` request answered, and what the
    /// message log still holds flushed. Returns how long it took: real time
    /// means input may have queued to this process meanwhile, and the Commit
    /// goes through the dispatch fence again before it writes (round eight of
    /// the update audit, #18). The supervisor host is stopped, and the
    /// successor activated, at the write, not here (the reviews of #18).
    #[cfg(unix)]
    fn answer_at_commit_claim(&mut self, child_pid: Option<u32>) -> std::time::Duration {
        let started = std::time::Instant::now();
        // THE FREEZE, IN TWO NUMBERS. Everything from the park to here
        // is a terminal that echoed nothing; the split says which half
        // to attack. park→proof is the successor's bundle swap, second
        // `execve` and cold GUI boot; proof→commit is this process's
        // dispatch fence plus egress settle. Emitted once per claim,
        // which ends in `_exit` but on a write that fails or an admission
        // refused after the claim's fence, so it is the last thing this
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
        // THE LANDING, on the arming's clock: the log carries arm →
        // start → land for the automatic lane, so how long an update
        // waited is one subtraction, not an archaeology of phases.
        if let Some(pending) = self
            .pending_update_handoff
            .as_ref()
            .filter(|pending| pending.mode.is_automatic())
            && let Some(ladder) = self
                .auto_apply_ladder
                .filter(|ladder| ladder.build == pending.target_build)
        {
            aterm_log::info!(
                "update auto-apply landed for build {}: committing {:.1} s after \
                 it was armed",
                ladder.build,
                ladder.armed_at.elapsed().as_secs_f64()
            );
        }
        // The successor is NOT activated here, and the supervisor host is
        // NOT stopped here: both happen at the write itself, after the last
        // reading of the admission (`finish_proven_handoff`). A claim these
        // answers hold through one more dispatch fence ([`HeldCommit`]) is
        // still a candidate: activated now, it would take the person's keys
        // through the fence (lost with it if the held pass refuses), and the
        // fence's turns would tell this process's focus-tracking tabs they
        // lost focus after the successor told them they have it
        // ([`Self::activate_successor_at_write`]). The host's workers' last
        // act as they stop clears the badges they raised, which that reading
        // compares.
        // EVERY LIVE `video` REQUEST IS ANSWERED HERE, AND ITS
        // ANSWER WRITTEN, BEFORE THE `_exit` (round six of the
        // update audit, findings 22 and 31). Not at the park, nor
        // before the claim: every step until it could still
        // roll the attempt back with this process running, and a
        // take aborted then was lost to an update that did not
        // happen. Here the take is aborted and its dir removed, an
        // export is cancelled and its encode worker waited for, and
        // then every reply's write on its control connection —
        // `_exit` before that write was a dropped connection. Bounded
        // ([`VIDEO_EXPORT_COMMIT_WAIT`]) and said: the adoption
        // proof is in hand, and what does not answer in time is
        // left to the process's end, as before.
        let video = self.video_answer_before_commit(VIDEO_EXPORT_COMMIT_WAIT);
        if video.answered {
            aterm_log::info!("update apply: answered a live video recording or export at Commit");
        }
        if !video.export_settled || video.unwritten > 0 {
            aterm_log::warn!(
                "update apply: a video reply was still owed {} ms into Commit (export \
                 settled: {}, replies unwritten: {}); committing without it",
                VIDEO_EXPORT_COMMIT_WAIT.as_millis(),
                video.export_settled,
                video.unwritten
            );
        }
        // The rows the park's carry did not take are NOT closed here (ruling
        // 441 (d)): these answers can hold the claim through one more fence,
        // and the admission read after it may still refuse, with this process
        // running on. They close at the write, after its last reading.
        // THE PARENT'S RECORD LANDS BEFORE ITS `_exit` (round-seven
        // update audit, finding 53): the writer thread's queue dies
        // with the process, and the successor reads the file again
        // after Commit for what this process recorded since its
        // boot. Flushed before the claim; what was recorded since,
        // the video answer's lines among it, lands here — at once
        // from a writer that answered then, and a writer that did
        // not is not waited for twice.
        if !matches!(
            self.pending_update_handoff
                .as_ref()
                .map(|pending| pending.commit_flush),
            Some(Some(false))
        ) {
            self.flush_messages_log_within(MESSAGES_FLUSH_COMMIT_WAIT);
        }
        started.elapsed()
    }

    /// THE SUCCESSOR TAKES THE FRONT AT THE COMMIT'S WRITE, and only when an
    /// aterm window had focus at the park (2026-09-19). Nothing before this
    /// asked for it, on either half: LaunchServices' `activates` is false
    /// (`begin_launch(.., false)`), and the successor's own launch posture
    /// asks for nothing (`launch_posture(_, true)`, 2026-09-24) — the fork
    /// lane has only that second half. So this is the one instant the user's
    /// focus moves — onto the window that is about to be theirs, never onto a
    /// candidate. With no aterm window focused the user is in another app and
    /// keeps it, and the log says that too, so an update's record shows which.
    ///
    /// CALLED INSIDE THE WRITE, after the admission's last reading and the
    /// operator's permit (the review of round eight's #18), never at the
    /// claim. A claim whose answers took real time is held through one more
    /// input-dispatch fence ([`HeldCommit`]) and is still a candidate there:
    /// its held pass may refuse it, and the rejection kills the successor.
    /// Activated at the claim, the successor took the person's keys through
    /// the fence into its pre-Commit queue, all of them lost with it on such a
    /// refusal, with focus left on no aterm window; and the fence's run-loop
    /// turns dispatched this process's own `Focused(false)`, which wrote a
    /// focus-out report (DEC 1004 `ESC [ O`) to the masters after the
    /// successor's focus-in, so a focus-tracking program in the front tab
    /// (vim's `FocusLost`, tmux, Claude Code) was left believing the terminal
    /// unfocused, the successor having taken that report as already sent.
    /// Here no run-loop turn follows: the write `_exit`s, and a write that
    /// fails found the successor gone (EPIPE). A key the OS still queues to
    /// this process while the activation is under way dies with it, as it
    /// always did; the long waits the dispatch fence exists for (the video
    /// answer, the messages flush) are behind it.
    #[cfg(target_os = "macos")]
    fn activate_successor_at_write(&mut self, child_pid: Option<u32>) {
        let Some(pid) = child_pid.and_then(|pid| i32::try_from(pid).ok()) else {
            return;
        };
        if !self
            .pending_update_handoff
            .as_ref()
            .is_some_and(|pending| pending.activate_at_commit)
        {
            aterm_log::info!(
                "update apply: not activating the successor at Commit (no aterm \
                 window had focus at the park; the person keeps the app they are \
                 in)"
            );
            return;
        }
        // A test records the activation in place of AppKit's: no unit test
        // reaches WindowServer (AGENTS.md, build coordination rule 5).
        #[cfg(test)]
        let accepted = {
            self.handoff_activations.push(pid);
            true
        };
        #[cfg(not(test))]
        let accepted = crate::app_launch_successor::activate_running(pid);
        aterm_log::info!(
            "update apply: activating the successor at Commit (an aterm \
             window had focus at the park): {}",
            if accepted {
                "accepted"
            } else {
                "refused by AppKit — the successor stays behind"
            }
        );
    }

    /// The cached verdict's own reason for a refusal, when the entry is the
    /// fresh one for this artifact (the same key [`Self::cached_handoff_preverification`]
    /// reads by).
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn cached_handoff_preverification_reason(
        &self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
    ) -> Option<String> {
        self.read_fresh_preverification(attempt, |entry| entry.reason.clone())
    }

    /// How many physical handoff failures this process has already booked
    /// against the artifact `attempt` targets — the number the freeze budget
    /// widens on. `0` with no attempt (the QA seam) or no record.
    #[must_use]
    #[cfg(unix)]
    pub(crate) fn prior_physical_failures_of(
        &self,
        attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
    ) -> u8 {
        let Some(attempt) = attempt else {
            return 0;
        };
        self.auto_apply_physical_retry
            .filter(|retry| retry.build == attempt.target_build())
            .map_or(0, |retry| retry.cycles)
    }

    /// How many times the FORK lane's park has missed its freeze budget for
    /// `target_build` — the rungs it climbs across attempts, since it cannot
    /// climb them inside one (see `App::fork_park_misses`).
    #[must_use]
    #[cfg(unix)]
    pub(crate) fn fork_park_misses_of(&self, target_build: u64) -> u8 {
        self.fork_park_misses
            .filter(|(build, _)| *build == target_build)
            .map_or(0, |(_, misses)| misses)
    }

    /// The fork lane waits for an unread policy while it is being read.
    /// `Some(wait)` for a fork-lane attempt at `target_build` with no verified
    /// pass on record while a policy read is in flight: the first such attempt
    /// starts the read off this thread
    /// ([`Self::spawn_staged_handoff_preverification`], which joins a running arm-time
    /// read), and attempts wait until it ends — an automatic one deferred as
    /// activity, a person's held. `None` for a known policy, no read running, or
    /// once [`crate::HANDOFF_PREVERIFY_READ_CEILING`] has passed since the first
    /// wait; the attempt then parks as usual. Each wait is logged.
    ///
    /// A PERSON'S APPLY WAITS TOO (round eight of the update audit, U35). The fork
    /// lane parks before its worker verifies, so a person's Install with no pass on
    /// record ran the Full capture — on this thread, every reader parked — and only
    /// then learned the policy the candidate sealed to route around it, and refused
    /// itself (`capture_exceeds_policy`): a frozen desk and a failed install. Now
    /// its press is HELD while the read runs (seconds), its installing row up —
    /// [`crate::UpdateHandoffStartError::HeldForRead`], never a deferral, which
    /// nothing came back for with `[update] auto_apply = false` (U35's review) —
    /// and runs again, parking under the policy, once the read ends
    /// ([`Self::run_held_person_apply`]). With the read started when a stage is
    /// armed whatever `[update] auto_apply` says ([`Self::arm_native_auto_apply`]),
    /// a press seldom finds one still running.
    #[cfg(unix)]
    fn fork_lane_policy_wait(
        &mut self,
        mode: crate::native_updater_service::ApplyMode,
        target_build: u64,
        policy_unread: bool,
    ) -> Option<crate::UpdateHandoffStartError> {
        self.preverify_read_wait(mode, target_build, policy_unread, PreverifyWaiter::ForkPark)
    }

    /// THE COLD LANE ANSWERS TO THE PRE-PARK VERDICT (round eight, #1): what it
    /// owes the verification of the artifact `attempt` targets before it writes
    /// the quit layout and replaces this process. The seamless lane has read that
    /// verdict since 2026-09-14; the cold lane went straight to `exec`, and its
    /// successor is THIS build, whose boot swap then runs the complete gate — so a
    /// refusal the verdict already knew (an install location a standard user
    /// cannot write, a hand-installed bundle that cannot be the rollback source)
    /// came back as a desk with no terminal again, which re-armed and exec'd
    /// again: the window closed and reopened every few seconds, for ever.
    ///
    /// In the AUTOMATIC modes, exactly as the seamless lane reads it:
    /// * a fresh REFUSAL of this artifact is the lane's answer — the verifier's
    ///   own words, minted `Failed` like the seamless lane's, so the synchronous
    ///   arm books it by the same typed fact (a person's refusal latches the
    ///   environment block; any other is the cached-refusal shape's budget);
    /// * no verdict yet, with a read of this target in flight (or startable): the
    ///   attempt defers as activity while it runs, bounded by
    ///   [`crate::HANDOFF_PREVERIFY_READ_CEILING`] — the read's own deadline, and
    ///   the fork lane's wait, one window per target — and the attempt after it
    ///   relaunches under what it answered — or, nothing on record, as the lane
    ///   always did;
    /// * that read ENDED WITHOUT A VERDICT (a passing refusal: another aterm held
    ///   the apply lock past its wait, a helper ran out of its budget — round eight,
    ///   S1's second review): the moment it measured is the attempt's answer, a
    ///   `Failed` the synchronous arm books TRANSIENT (no cached refusal stands
    ///   behind it), and nothing is relaunched. The successor would be this build,
    ///   whose boot swap waits on the same lock: the window closed and reopened
    ///   into that refusal at every retry — every six hours for good once the lane
    ///   converged — for as long as the lock stayed held.
    ///
    /// A person's apply is not answered by a cached verdict (the seamless lane's
    /// rule: the bundle may have been put right since), nor is the QA seam's
    /// same-image relaunch, which has no artifact.
    ///
    /// IN EVERY MODE, A STAGE THIS PROCESS KNOWS IS WAITING FOR AN UPDATE CHECK TO
    /// CONFIRM ITS SIGNER IS NOT RELAUNCHED INTO (round eight, #503's review and
    /// its second review; [`Self::known_roster_wait`]). The successor is this
    /// build, whose boot swap (gate 4d) would wait the same way: a person's press
    /// closed and reopened the window and installed nothing, where before #503 that
    /// relaunch installed the stage. It is a deferral (activity's error: charged
    /// nothing) that names the wait; the automatic lane holds for the check with its
    /// intent kept, and a person's press re-arms nothing. That knowledge is not a
    /// verdict on the bytes but how long ago a check ran, so it binds a person's
    /// press too, and every check that has reported the stage since retires it. A
    /// read that ended with the wait but BEGAN before such a report is stale: the
    /// automatic lane reads the stage again before it decides, and books nothing.
    #[cfg(unix)]
    pub(crate) fn cold_lane_verdict_gate(
        &mut self,
        mode: crate::native_updater_service::ApplyMode,
        attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
    ) -> Result<(), crate::UpdateHandoffStartError> {
        let Some(attempt) = attempt else {
            return Ok(());
        };
        let now = std::time::Instant::now();
        if let Some(known) = self.known_roster_wait(attempt, now) {
            if !known.held
                && let Some(dmg_sha256) =
                    crate::app_native::decode_dmg_sha256(attempt.target_dmg_sha256())
            {
                let _ = self.hold_for_roster_confirmation(
                    attempt.target_build(),
                    dmg_sha256,
                    &known.reason,
                    known.answered_at,
                    mode.is_automatic(),
                    true,
                );
            }
            return Err(crate::UpdateHandoffStartError::activity(format!(
                "{}; the app was not relaunched",
                known.reason
            )));
        }
        if !mode.is_automatic() {
            return Ok(());
        }
        self.answer_with_refused_cold_relaunch(attempt);
        match self.cached_handoff_preverification(attempt) {
            Some(true) => Ok(()),
            Some(false) => {
                let reason = self
                    .cached_handoff_preverification_reason(attempt)
                    .unwrap_or_else(|| "the staged update failed verification".to_string());
                // Never the wait for a roster confirmation, which no verdict is (a
                // refused cold relaunch seeds it as a hold); the backstop holds for
                // the check rather than book it on the transient budget.
                if aterm_update::is_roster_confirmation_wait(&reason) {
                    if let Some(dmg_sha256) =
                        crate::app_native::decode_dmg_sha256(attempt.target_dmg_sha256())
                    {
                        let _ = self.hold_for_roster_confirmation(
                            attempt.target_build(),
                            dmg_sha256,
                            &reason,
                            now,
                            true,
                            true,
                        );
                    }
                    return Err(crate::UpdateHandoffStartError::activity(format!(
                        "{reason}; the app was not relaunched"
                    )));
                }
                Err(crate::UpdateHandoffStartError::failed(format!(
                    "{reason}; the app was not relaunched"
                )))
            }
            None => {
                let read = self
                    .handoff_preverify_in_flight
                    .as_ref()
                    .filter(|read| read.reads(attempt));
                // A wait [`Self::known_roster_wait`] did not take is a stale one: the
                // read began before a check reported the stage, which may have
                // confirmed it. It is no answer, and above all no `Failed` to book:
                // the stage is read again, and the attempt defers while that runs.
                if read.is_some_and(|read| {
                    read.ended_passing(now)
                        .is_some_and(aterm_update::is_roster_confirmation_wait)
                }) {
                    let started = crate::app_native::decode_dmg_sha256(attempt.target_dmg_sha256())
                        .is_some_and(|dmg_sha256| {
                            self.start_fresh_preverification(attempt.target_build(), dmg_sha256)
                        });
                    if started {
                        return Err(preverify_read_activity(
                            attempt.target_build(),
                            PreverifyWaiter::ColdRelaunch,
                        ));
                    }
                    return Ok(());
                }
                if let Some(reason) = read.and_then(|read| read.ended_passing(now)) {
                    return Err(crate::UpdateHandoffStartError::failed(format!(
                        "{reason}; the app was not relaunched, and the update is tried again \
                         later"
                    )));
                }
                // This target's read still running is waited for whether or not
                // the per-target window has been spent: a later attempt's arm-time
                // read is the only measure of the moment the last one refused for,
                // and its own deadline bounds the wait.
                if read.is_some_and(|read| read.running_for(attempt.target_build(), now)) {
                    return Err(preverify_read_activity(
                        attempt.target_build(),
                        PreverifyWaiter::ColdRelaunch,
                    ));
                }
                self.preverify_read_wait(
                    mode,
                    attempt.target_build(),
                    true,
                    PreverifyWaiter::ColdRelaunch,
                )
                .map_or(Ok(()), Err)
            }
        }
    }

    /// ONE BOUNDED WAIT FOR A PRE-PARK VERIFICATION READ, for whichever lane is
    /// about to act without its answer (`waiter`): the fork lane's park
    /// ([`Self::fork_lane_policy_wait`], a person's apply included) and the cold
    /// lane's relaunch ([`Self::cold_lane_verdict_gate`], which asks only in the
    /// automatic modes). One window per target (`preverify_read_waited`), shared by
    /// both — the desk decides the lane, and a target's read is the same read
    /// whichever lane waits on it. `mode` says whose attempt it is: an automatic
    /// one defers as activity, a person's is held
    /// ([`crate::UpdateHandoffStartError::HeldForRead`]).
    #[cfg(unix)]
    fn preverify_read_wait(
        &mut self,
        mode: crate::native_updater_service::ApplyMode,
        target_build: u64,
        unread: bool,
        waiter: PreverifyWaiter,
    ) -> Option<crate::UpdateHandoffStartError> {
        if !unread {
            return None;
        }
        let now = std::time::Instant::now();
        // One wait per target — renewed only once a cached verdict would have
        // gone stale since it ended, when the policy is unread again for the
        // same reason a first attempt's was.
        let until = match self.preverify_read_waited {
            Some((target, until))
                if target == target_build && now < until + crate::HANDOFF_PREVERIFY_FRESHNESS =>
            {
                until
            }
            _ => {
                let until = now + crate::HANDOFF_PREVERIFY_READ_CEILING;
                self.preverify_read_waited = Some((target_build, until));
                self.spawn_staged_handoff_preverification(target_build);
                until
            }
        };
        let reading = self
            .handoff_preverify_in_flight
            .as_ref()
            .is_some_and(|read| read.running_for(target_build, now));
        if !reading || now >= until {
            if reading {
                aterm_log::info!(
                    "update apply: build {target_build}'s pre-park verification is still marked \
                     running past the {} s {} waits for it — past the lock wait and \
                     verification budget that bound it, so its thread never ran; {} without it",
                    crate::HANDOFF_PREVERIFY_READ_CEILING.as_secs(),
                    waiter.lane(),
                    waiter.acting(),
                );
            }
            return None;
        }
        // A PERSON'S PRESS IS HELD, NOT DEFERRED (round eight, U35's review):
        // the automatic lane comes back for its own deferral, and nothing does
        // for a person's when `[update] auto_apply` is off.
        Some(if mode.is_automatic() {
            preverify_read_activity(target_build, waiter)
        } else {
            preverify_read_hold(target_build, waiter, until)
        })
    }

    /// THE DISSOLUTION OF EVERY CONNECTION THE MANIFEST LEFT OUT, ON RECORD AT
    /// THE COMMIT (round eight of the update audit, U23's review): one
    /// `session_edge` drop per row (`action=drop origin=handoff`), as the
    /// successor audits a carried row it cannot re-mint. Called by the Commit
    /// alone, right before its write: the manifest's writer left these rows out
    /// to fit its cap, but this process holds them until the Commit, and an
    /// attempt that stands down dissolves none.
    #[cfg(unix)]
    pub(crate) fn audit_connections_left_out(rows: &[crate::session_store::ConnectionCarry]) {
        for row in rows {
            crate::session_edge_audit::emit(
                crate::session_edge_audit::EdgeAction::Drop,
                "handoff",
                &aterm_session::SessionId::new(row.src.clone()),
                &aterm_session::SessionId::new(row.dst.clone()),
                &row.op,
            );
        }
    }

    /// Hold a person's press for the candidate's pre-park read
    /// ([`crate::UpdateHandoffStartError::HeldForRead`]): `about_to_wait` runs it
    /// again once that read ends ([`Self::run_held_person_apply`]).
    #[cfg(unix)]
    pub(crate) fn hold_person_apply(&mut self, held: crate::HeldPersonApply) {
        aterm_log::info!(
            "update apply: a person's Install is held for build {}'s pre-park read, at most \
             {} ms more; its row stays up and it runs as soon as the read ends",
            held.target_build,
            held.until
                .saturating_duration_since(std::time::Instant::now())
                .as_millis()
        );
        self.person_apply_held = Some(held);
    }

    /// The event loop's next look at a held press ([`Self::run_held_person_apply`]):
    /// the read raises only a flag, so the loop looks every
    /// [`HELD_PERSON_APPLY_POLL`], and never later than the hold's window ends.
    /// `None` with no press held.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn held_person_apply_deadline(
        &self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        self.person_apply_held
            .map(|held| held.until.min(now + HELD_PERSON_APPLY_POLL))
    }

    /// THE PERSON'S HELD INSTALL RUNS ONCE THE CANDIDATE'S READ ENDS (round eight
    /// of the update audit, U35's review). A press that reached the fork lane while
    /// the read of its target ran was deferred, and nothing came back for it with
    /// `[update] auto_apply = false`, while its one record said it would; from the
    /// Version menu or the status bar it opened Software Update, which showed
    /// nothing of why. Now the press is held ([`Self::hold_person_apply`]), its
    /// installing row up and its switch on record, and run here, as the person's
    /// own `Immediate` apply, once the read has ended — or its window `until`
    /// (at most [`crate::HANDOFF_PREVERIFY_READ_CEILING`]) has passed, when the
    /// fork lane parks as it always did.
    ///
    /// What it comes to is surfaced as a row, the way a person's press that
    /// cannot open a page is told ([`Self::surface_update_apply_outcome`], no
    /// details page seconds after the press). An outcome that returned before the
    /// handoff began had not put the row back nor closed the record the first
    /// run opened, so this does, as a stop the person is told of (both
    /// idempotent). A stage that is no longer the held target drops the press:
    /// the new stage's own row offers it.
    #[cfg(unix)]
    pub(crate) fn run_held_person_apply(&mut self, now: std::time::Instant) {
        use crate::native_app::UpdateOutcome;
        let Some(held) = self.person_apply_held else {
            return;
        };
        let reading = self
            .handoff_preverify_in_flight
            .as_ref()
            .is_some_and(|read| read.running_for(held.target_build, now));
        if reading && now < held.until {
            return;
        }
        self.person_apply_held = None;
        let staged = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|staged| staged.build);
        if staged != Some(held.target_build) {
            aterm_log::info!(
                "update apply: the Install held for build {}'s pre-park read is dropped: the \
                 staged build is now {staged:?}",
                held.target_build
            );
            self.retire_update_installing();
            self.record_update_switch_stopped(false, HELD_PRESS_SUPERSEDED);
            return;
        }
        aterm_log::info!(
            "update apply: build {}'s pre-park read {}; the Install a person pressed runs now",
            held.target_build,
            if reading {
                "outran its window"
            } else {
                "ended"
            }
        );
        let outcome = self.apply_native_update(crate::native_updater_service::ApplyMode::Immediate);
        // Held again: the stage it runs for is being read now.
        if self.person_apply_held.is_some() {
            return;
        }
        let stopped = match &outcome {
            UpdateOutcome::Deferred { reason } => Some(reason.clone()),
            UpdateOutcome::Blocked { reasons } => Some(reasons.join(" \u{b7} ")),
            UpdateOutcome::Failed { message } | UpdateOutcome::CaptureRefused { message } => {
                Some(message.clone())
            }
            UpdateOutcome::Accepted | UpdateOutcome::InstalledNeedsRelaunch { .. } => None,
        };
        if let Some(why) = stopped {
            self.retire_update_installing();
            self.record_update_switch_stopped(false, &why);
        }
        self.surface_update_apply_outcome(HELD_PRESS_SOURCE, outcome, false);
    }

    /// A fork-lane park miss: counts the rung it buys the next attempt and returns
    /// the typed start error that routes it to the activity-revoked spacing rather
    /// than the physical budget.
    #[cfg(unix)]
    fn fork_park_missed(
        &mut self,
        target_build: u64,
        reason: String,
    ) -> crate::UpdateHandoffStartError {
        let misses = self.fork_park_misses_of(target_build).saturating_add(1);
        self.fork_park_misses = Some((target_build, misses));
        aterm_log::warn!(
            "update apply: the fork lane's park missed its budget ({reason}); the next attempt \
             widens to the next rung (miss {misses}) and is retried on the activity spacing"
        );
        crate::UpdateHandoffStartError::park_missed(reason)
    }

    /// The cached verdict's reason keyed by the artifact alone, for the
    /// submission-time failure arm, which has no ticket but knows the intent's build
    /// and digest. `Some(reason)` only for a fresh refusal of that artifact.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn cached_handoff_refusal_reason(&self, build: u64) -> Option<String> {
        // The artifact the verdict is about is the live stage's digest for
        // this build — the same stage `poll` admitted the intent against.
        let snapshot = self.native_updater_service.snapshot();
        let artifact = snapshot
            .staged
            .as_ref()
            .filter(|staged| staged.build == build)
            .map(|staged| staged.dmg_sha256.clone())?;
        let cached = self
            .handoff_preverified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cached
            .as_ref()
            .filter(|entry| {
                !entry.passed
                    && entry.build == build
                    && entry.artifact == artifact
                    && entry.at.elapsed() < crate::HANDOFF_PREVERIFY_FRESHNESS
            })
            .and_then(|entry| entry.reason.clone())
    }

    /// A REFUSED COLD RELAUNCH ANSWERS THIS PROCESS'S FIRST AUTOMATIC READ OF ITS
    /// ARTIFACT, WHENEVER THAT COMES (round eight, #1's review). The boot's refusal
    /// is put in the verdict cache at launch
    /// ([`Self::seed_refused_cold_relaunch`]), where it lapses with every verdict's
    /// freshness — and a first attempt that came later (a person typing in a
    /// document tab holds the automatic lane back for up to its ladder) found no
    /// verdict, relaunched, and carried a budget that never counted the refusal, so
    /// the lane went round again unconverged. Until an automatic read of the
    /// artifact has taken it, the refusal is kept apart, and that read — the cold
    /// lane's gate or the seamless lane's pre-park check — finds it fresh. Taken
    /// once: the booking it answers is the refusal's one entry in the budget.
    #[cfg(unix)]
    fn answer_with_refused_cold_relaunch(
        &mut self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
    ) {
        let Some(refused) = self.cold_relaunch_refused.take_if(|refused| {
            refused.build == attempt.target_build()
                && refused.commit == attempt.target_commit()
                && refused.artifact == attempt.target_dmg_sha256()
        }) else {
            return;
        };
        *self
            .handoff_preverified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(crate::HandoffPreverification {
                at: std::time::Instant::now(),
                ..refused
            });
    }

    /// The cached pre-park verification verdict for THIS exact artifact, or `None`
    /// when there is no fresh entry bound to its build and commit.
    ///
    /// `Some(false)` is the only answer with authority: it short-circuits
    /// [`Self::start_unix_update_handoff`] before any reader parks. Separate so a
    /// test can read a seeded verdict the way production does; headless tests park
    /// nothing and could not otherwise tell a healthy candidate from a refused one.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn cached_handoff_preverification(
        &self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
    ) -> Option<bool> {
        self.read_fresh_preverification(attempt, |entry| Some(entry.passed))
    }

    /// Reads the cached pre-park verification entry under the cache's lock when it
    /// is the fresh one bound to exactly the artifact `attempt` targets (build,
    /// commit and digest, within [`crate::HANDOFF_PREVERIFY_FRESHNESS`]).
    #[cfg(unix)]
    fn read_fresh_preverification<T>(
        &self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
        read: impl FnOnce(&crate::HandoffPreverification) -> Option<T>,
    ) -> Option<T> {
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
            .and_then(read)
    }

    /// The successor's handoff policy as this park must follow it: what the passed
    /// pre-verification of exactly the targeted artifact read from its bundle. Every
    /// park takes this one reading (the fork lane, the launched lane's gate and
    /// capture, and [`Self::park_desk_for_test`]). [`ParkPolicy::policy`] is `None`
    /// for the same-image QA seam, no verdict, a refusal, or a policy that asks
    /// nothing of this build; [`ParkPolicy::known`] says whether a pass is on record.
    ///
    /// Not bound by [`crate::HANDOFF_PREVERIFY_FRESHNESS`]: that window limits how
    /// long a verdict stands in for re-running `codesign`, while the policy is a
    /// sealed resource of one release, named by build, commit and digest. The
    /// attempt is still authorized by the complete gate at swap time.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn park_policy(
        &self,
        attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
    ) -> ParkPolicy {
        let Some(attempt) = attempt else {
            return ParkPolicy::default();
        };
        let cached = self
            .handoff_preverified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cached
            .as_ref()
            .filter(|entry| {
                entry.passed
                    && entry.build == attempt.target_build()
                    && entry.commit == attempt.target_commit()
                    && entry.artifact == attempt.target_dmg_sha256()
            })
            .map_or_else(ParkPolicy::default, |entry| ParkPolicy {
                policy: entry.policy,
                known: true,
                grant_chunks: entry.grant_chunks,
            })
    }

    /// [`Self::park_policy`]'s policy by the cache's own key — for tests of the
    /// cache itself.
    #[cfg(all(test, unix))]
    #[must_use]
    pub(crate) fn handoff_policy_for(
        &self,
        build: u64,
        commit: &str,
        artifact: &str,
    ) -> Option<aterm_update_core::handoff_policy::HandoffPolicy> {
        self.park_policy(Some(
            &crate::native_updater_service::ApplyAttemptTicket::for_test(build, commit, artifact),
        ))
        .policy
    }

    /// Reduces one returned (non-ready) handoff completion: resumes the parked
    /// readers, reduces the exact apply ticket against the lane its mode authorized,
    /// surfaces the verdict, and returns the structural teardown the caller must
    /// replay. `None` when no pending attempt matches; nothing may be replayed.
    ///
    /// Split out of [`Self::finish_update_handoff`] so it can be tested without an
    /// event loop. It is the only place the attempt's `ApplyMode` still exists, so
    /// the automatic/person-initiated split is decided here
    /// ([`crate::app_native::HandoffFailureLane`]).
    #[cfg(unix)]
    pub(crate) fn reduce_returned_handoff_completion(
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
        // State-keyed: two records can describe one attempt, the parked one
        // (`pending_update_handoff`, readers stopped) and the prelaunched one
        // (`update_handoff_prelaunch`, a successor held). Readers roll back iff the
        // parked record exists, never by a worker-reported phase, and both records
        // are cleared only here, after the worker's death proof, so no retry
        // launches beside a live candidate. The parked record's facts win.
        let parked = self
            .pending_update_handoff
            .take_if(|pending| pending.attempt_id == _attempt_id);
        #[cfg(any(target_os = "macos", all(test, unix)))]
        let prelaunch = self
            .update_handoff_prelaunch
            .take_if(|prelaunch| prelaunch.attempt_id == _attempt_id);
        let pending = match parked {
            Some(parked) => {
                #[cfg(any(target_os = "macos", all(test, unix)))]
                let parked = {
                    let mut parked = parked;
                    if let Some(prelaunch) = prelaunch {
                        parked.teardown.merge(prelaunch.teardown);
                        parked.revoked_by_activity |= prelaunch.revoked_by_activity;
                    }
                    parked
                };
                crate::ReturnedHandoffRecord {
                    mode: parked.mode,
                    apply_attempt: parked.apply_attempt,
                    same_image: parked.same_image,
                    teardown: parked.teardown,
                    revoked_by_activity: parked.revoked_by_activity,
                    parked_live: Some(parked.live),
                }
            }
            #[cfg(any(target_os = "macos", all(test, unix)))]
            None => {
                let prelaunch = prelaunch?;
                crate::ReturnedHandoffRecord {
                    mode: prelaunch.mode,
                    apply_attempt: prelaunch.apply_attempt,
                    same_image: prelaunch.same_image,
                    teardown: prelaunch.teardown,
                    revoked_by_activity: prelaunch.revoked_by_activity,
                    parked_live: None,
                }
            }
            #[cfg(not(any(target_os = "macos", all(test, unix))))]
            None => return None,
        };
        // Lane classification for the bounded automatic retry budgets, from three
        // typed facts:
        // - the mode the apply was authorized under, so a person's failure is not
        //   charged to the automatic budget;
        // - the worker's outcome (plus the main thread's activity-rejection flag),
        //   so `TimedOut` and `AdoptionMismatch` cost what `PhysicalFailureShape`
        //   says;
        // - `child_death`, what the worker saw when a candidate ended before its
        //   proof ([`crate::ChildDeathEvidence`]).
        let lane = crate::app_native::HandoffFailureLane::classify(
            pending.mode,
            outcome,
            child_death,
            pending.revoked_by_activity,
        );
        let teardown = pending.teardown;
        // Rollback iff parked: a prelaunched attempt that never parked has nothing
        // to resume, and one released ahead of the stand-down already resumed.
        if let Some(live) = pending.parked_live.as_deref() {
            self.rollback_overlap(nonce.as_deref(), live);
        }
        // The attempt record is gone, and with it the fence holding draft journals:
        // owed re-seats and the appends behind them run now, not at the next
        // keystroke.
        self.drive_owed_document_journals();
        // The attempt is over: restore the row's words (or its absence) and end the
        // surge before the outcome decides whether to say anything, so a silent
        // outcome does not leave "Installing" on the bar.
        self.retire_update_installing();
        // QA seam, read before the match consumes it: a `None` ticket comes only
        // from `ATERM_DEBUG_SEAMLESS_REEXEC`, a simulated apply of the running
        // binary. It is logged and shown but never touches the durable ledger.
        // Which same-image attempt it was is carried, not inferred.
        let debug_seam = matches!(pending.same_image, Some(SameImageHandoff::DebugSeam));
        let attempted_build = pending
            .apply_attempt
            .as_ref()
            .map(|attempt| attempt.target_build());
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
        // THE SWITCH THAT DID NOT HAPPEN, on record once the lane has decided what
        // comes next (round eight of the update audit, lead U39): an automatic
        // attempt the lane tries again by itself — a stand-down for activity, a
        // capture refusal retried when the desk changes, a physical failure inside
        // its budget — asks nothing of the person and is a wait, in Info; one that
        // stopped the lane, or a person's press, could not install. Before the
        // outcome is surfaced, so the record reads in the order it happened.
        let routine = matches!(
            lane,
            crate::app_native::HandoffFailureLane::ActivityRevoked
                | crate::app_native::HandoffFailureLane::AwaitsRosterConfirmation { .. }
        ) || (pending.mode.is_automatic()
            && attempted_build.is_some_and(|build| self.automatic_apply_retry_scheduled(build)));
        // A capture refusal's stop is said by its outcome's own record, which
        // knows what the retry waits for (`react_to_update_apply_outcome_for`):
        // one record per attempt.
        if !matches!(
            surfaced,
            Some(crate::native_app::UpdateOutcome::CaptureRefused { .. })
        ) {
            self.record_update_switch_stopped(routine, &detail);
        }
        if let Some(surfaced) = surfaced {
            // The source names the lane the attempt rode, so a person's apply is not
            // logged as background work.
            let source = if pending.mode.is_automatic() {
                "automatic handoff"
            } else {
                "manual handoff"
            };
            if debug_seam {
                self.react_to_update_apply_outcome(source, surfaced, false);
            } else {
                self.surface_update_apply_outcome_for_target(
                    source,
                    surfaced,
                    false,
                    attempted_build.unwrap_or(0),
                );
            }
        }
        Some(teardown)
    }

    /// Overlap-handoff failure rollback: restores this still-running parent to a
    /// working terminal after a candidate that never became ready.
    ///
    /// Precondition: the candidate was signalled and proven terminated, so no other
    /// reader exists when ours restart. The proof is a [`HandoffRollbackWarrant`]
    /// minted on the worker (or emergency reaper) before the completion is sent
    /// ([`send_warranted_handoff_failure`]); it cannot cross the channel, so the
    /// send-site ordering is the guarantee. The pre-spawn failure paths in
    /// [`App::start_unix_update_handoff`] call this with no candidate.
    ///
    /// Then:
    /// * the worker has already retired the attempt's artifacts and republished the
    ///   parent socket link;
    /// * CLOEXEC is re-armed on every master;
    /// * each master gets back the size its engine was parked at
    ///   ([`Self::reassert_parked_winsizes`]);
    /// * the parked readers resume ([`Self::attach_deferred_readers`]);
    /// * every draft journal is owed a re-seat, since a candidate that restored may
    ///   have republished it under its own sequence numbers
    ///   ([`Self::reseat_document_journals_after_rollback`]).
    ///
    /// The event-loop half does no filesystem I/O, status read or child probe; the
    /// re-seats are queued to the document worker.
    #[cfg(unix)]
    pub(crate) fn rollback_overlap(&mut self, _nonce: Option<&str>, live: &[(u64, i32, i32)]) {
        for (_, master, _) in live {
            let _ = aterm_pty::set_cloexec(*master, true);
        }
        self.reassert_parked_winsizes(live);
        self.resume_deferred_readers_nonblocking();
        self.reseat_document_journals_after_rollback();
        // The cold restore's agents the park paused are this process's again, as is
        // a relaunch a worker's back-off still owed when the Commit's stop took it.
        if let Some(host) = self.harness.clone() {
            host.resume_restored();
            host.relaunch_stranded(&|sid| self.upgrade_place(sid));
        }
    }
}

impl App {
    /// Puts each handed PTY back at the size its engine was parked at. Before
    /// Commit the successor resizes every handed PTY to its own grids; after a
    /// rejected Commit, `resize_panes` skips panes whose engine already matches, so
    /// without this a split pane's program would keep the successor's size. The
    /// kernel sends `SIGWINCH` only on a change.
    #[cfg(unix)]
    fn reassert_parked_winsizes(&self, live: &[(u64, i32, i32)]) {
        for (id, master, _) in live {
            let Some(session) = self
                .pool
                .get(*id)
                .filter(|session| session.master == *master)
            else {
                continue;
            };
            let (rows, cols, cell_px) = {
                let engine = crate::term_lock(&session.term);
                (engine.rows(), engine.cols(), engine.host_cell_pixel_size())
            };
            aterm_pty::resize_with_cell_px(*master, rows, cols, cell_px);
        }
    }
}

/// How often the event loop looks at a person's held Install
/// ([`App::run_held_person_apply`]): the read it waits on raises only a flag. A
/// read takes a fraction of a second, so a press runs at most this long after it
/// ends.
#[cfg(unix)]
const HELD_PERSON_APPLY_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// The `source` a held press's outcome is surfaced under: a person's, so never
/// [`crate::app_update_screen`]'s automatic words.
#[cfg(unix)]
const HELD_PRESS_SOURCE: &str = "manual, held for the candidate's read";

/// Why a held press stopped when the stage moved on under it.
#[cfg(unix)]
const HELD_PRESS_SUPERSEDED: &str =
    "the update it was installing is no longer the one ready to install";

/// The deferral an automatic lane (`waiter`) answers with while `target_build`'s
/// pre-park verification is being read: routine activity, logged.
#[cfg(unix)]
fn preverify_read_activity(
    target_build: u64,
    waiter: PreverifyWaiter,
) -> crate::UpdateHandoffStartError {
    let reason = format!(
        "build {target_build}'s {} is being read before {} {}; the attempt after it ends \
         {} under it",
        waiter.what(),
        waiter.lane(),
        waiter.acts(),
        waiter.acts(),
    );
    aterm_log::info!("update apply: {reason}");
    crate::UpdateHandoffStartError::activity(reason)
}

/// What a PERSON'S press answers with while `target_build`'s pre-park
/// verification is being read (round eight of the update audit, U35's review):
/// held, never deferred — with `[update] auto_apply = false` nothing would come
/// back for a deferral, and its only record said it would. The caller holds the
/// press until the read ends or `until` passes ([`App::run_held_person_apply`]).
#[cfg(unix)]
fn preverify_read_hold(
    target_build: u64,
    waiter: PreverifyWaiter,
    until: std::time::Instant,
) -> crate::UpdateHandoffStartError {
    let reason = format!(
        "build {target_build}'s {} is being read before {} {}; this Install runs as soon \
         as the read ends",
        waiter.what(),
        waiter.lane(),
        waiter.acts(),
    );
    aterm_log::info!("update apply: {reason}");
    crate::UpdateHandoffStartError::held_for_read(target_build, until, reason)
}

/// Which lane waits on a pre-park verification read
/// ([`App::preverify_read_wait`]) — the words its log lines use.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreverifyWaiter {
    /// The fork lane, before it parks: it needs the candidate's handoff policy.
    ForkPark,
    /// The cold lane, before it relaunches: it needs the verdict.
    ColdRelaunch,
}

#[cfg(unix)]
impl PreverifyWaiter {
    fn lane(self) -> &'static str {
        match self {
            Self::ForkPark => "the fork lane",
            Self::ColdRelaunch => "the cold lane",
        }
    }

    fn what(self) -> &'static str {
        match self {
            Self::ForkPark => "handoff policy",
            Self::ColdRelaunch => "pre-park verification",
        }
    }

    fn acts(self) -> &'static str {
        match self {
            Self::ForkPark => "parks",
            Self::ColdRelaunch => "relaunches",
        }
    }

    fn acting(self) -> &'static str {
        match self {
            Self::ForkPark => "parking",
            Self::ColdRelaunch => "relaunching",
        }
    }
}

/// The cold lane's refusal of a safety token that carries `carried` unsaved
/// editor drafts: the preflight's person-facing shape
/// ([`crate::app_native::DIRTY_DOCUMENTS_BLOCK`]), so the row says what to do.
#[cfg(unix)]
pub(crate) fn cold_lane_carried_drafts_refusal(carried: usize) -> String {
    format!(
        "{} {carried} document(s) have uncheckpointed edits an update cannot carry: no \
         terminal session is open to carry the editor across, so replacing the app now would \
         close it",
        crate::app_native::DIRTY_DOCUMENTS_BLOCK,
    )
}

#[cfg(unix)]
impl crate::App {
    /// What the cold lane leaves its successor to reopen.
    ///
    /// A windowed desk with windows open writes the same quit layout a quit writes
    /// (`Some`), which the successor's boot reopens, so document and Settings tabs
    /// and window geometry survive. With session restore off the lane refuses, as it
    /// does for `--hold` panes. `None`: nothing to leave (headless, or no window).
    pub(crate) fn cold_lane_layout(
        &self,
    ) -> Result<Option<crate::restore::RestoreManifest>, String> {
        if self.headless {
            return Ok(None);
        }
        let layout = self.capture_restore_manifest();
        if layout.is_empty() {
            return Ok(None);
        }
        if !self.config.restore_session_or_default() {
            return Err(
                "open tabs (documents or Settings) would close: session restore is \
                        off, so the relaunched app could not reopen them; the update applies \
                        once they are closed or a terminal tab opens"
                    .to_string(),
            );
        }
        Ok(Some(layout))
    }

    /// The cold lane goes on without its quit layout, which the data folder
    /// refused for good (`cause`; [`LayoutLeft::NotKept`]): said in `aterm.log`
    /// and on record in Messages, written before the `exec` (whose flush
    /// carries it), so the person who finds the new version without their
    /// document and Settings tabs can read why (round eight, U48).
    ///
    /// Called once the switch is granted (the operator's quiesce), and on
    /// record ONCE per target build and cause (U48's review): an `exec` that
    /// then fails is retried, and a record is never folded — each post is a
    /// new one — so every retry of a settled fault added another. The log
    /// line is written for every attempt.
    fn say_cold_lane_tabs_not_kept(&mut self, target_build: u64, cause: &str) {
        aterm_log::warn!(
            "update apply: the open windows could not be saved for the new build ({cause}); \
             the cold lane goes on without them"
        );
        let said = (target_build, cause.to_string());
        if self.cold_lane_layout_debt.not_kept_said.as_ref() != Some(&said) {
            self.record_message(crate::update_words::cold_lane_tabs_not_kept(cause));
            self.cold_lane_layout_debt.not_kept_said = Some(said);
        }
    }

    /// TAKE BACK THIS RUN'S COLD-LANE LAYOUT AN `exec` DID NOT USE, waiting
    /// for the restore lock at most the main thread's bound
    /// ([`COLD_LANE_LAYOUT_LOCK_WAIT`]; round eight, U49): this run's desk
    /// alone ([`crate::restore::unwrite_quit_within`]), another instance's
    /// quit the write kept stays.
    ///
    /// A refused take-back is OWED, never dropped (U49's review): the event
    /// loop tries it again ([`Self::retry_owed_layout_take_back`]), and so do
    /// a seamless park and a cold `exec` that writes no newer desk
    /// ([`Self::settle_owed_layout_take_back_before_switch`]) and the quit
    /// ([`Self::settle_owed_layout_take_back_at_quit`]), until it
    /// lands or a newer write of this run's desk replaces it
    /// ([`Self::cold_lane_layout_rewritten`]). Dropped, the desk stayed in
    /// `session.toml` while this process ran on: a later crash's claim set the
    /// journal's newer desk aside by this run's name and reopened the stale
    /// one, and after a seamless update it read as another instance's desk,
    /// which every later quit kept and the next cold launch reopened again.
    fn take_back_cold_lane_layout(&mut self, run: Option<String>, now: std::time::Instant) {
        let _ = self.take_back_cold_lane_layout_with(run, now, |run| {
            crate::restore::unwrite_quit_within(run, COLD_LANE_LAYOUT_LOCK_WAIT)
        });
    }

    /// [`Self::take_back_cold_lane_layout`] through `unwrite`; the refusal,
    /// now owed, is returned.
    fn take_back_cold_lane_layout_with(
        &mut self,
        run: Option<String>,
        now: std::time::Instant,
        unwrite: impl FnOnce(Option<&str>) -> Result<(), crate::restore::RestoreFault>,
    ) -> Result<(), crate::restore::RestoreFault> {
        let refused_before = self
            .cold_lane_layout_debt
            .owed_take_back
            .as_ref()
            .map_or(0, |owed| owed.refusals);
        match unwrite(run.as_deref()) {
            Err(fault) if !fault.published() => {
                let refusals = refused_before.saturating_add(1);
                let spacing = OwedTakeBack::spacing(refusals);
                aterm_log::warn!(
                    "update apply: the cold lane's layout was not taken back ({fault}); \
                     owed, tried again in {} s",
                    spacing.as_secs()
                );
                self.cold_lane_layout_debt.owed_take_back = Some(OwedTakeBack {
                    run,
                    retry_at: now + spacing,
                    refusals,
                });
                Err(fault)
            }
            // Taken back — or rewritten without the desk and published before
            // a failure (its directory's sync): the file a reader sees.
            _ => {
                if refused_before > 0 {
                    aterm_log::info!(
                        "update apply: the cold lane's layout was taken back after \
                         {refused_before} refusal(s)"
                    );
                }
                self.cold_lane_layout_debt.owed_take_back = None;
                Ok(())
            }
        }
    }

    /// A newer write of this run's desk replaced the one a take-back still
    /// owed ([`Self::take_back_cold_lane_layout`]).
    fn cold_lane_layout_rewritten(&mut self) {
        self.cold_lane_layout_debt.owed_take_back = None;
    }

    /// When the event loop tries an owed take-back again
    /// ([`Self::take_back_cold_lane_layout`]); `None` when none is owed.
    pub(crate) fn owed_layout_take_back_at(&self) -> Option<std::time::Instant> {
        self.cold_lane_layout_debt
            .owed_take_back
            .as_ref()
            .map(|owed| owed.retry_at)
    }

    /// The event loop's try of an owed take-back, once due
    /// ([`Self::owed_layout_take_back_at`]), bounded as the first was; a
    /// refusal spaces the next try further ([`OwedTakeBack::spacing`]).
    pub(crate) fn retry_owed_layout_take_back(&mut self, now: std::time::Instant) {
        self.retry_owed_layout_take_back_with(now, |run| {
            crate::restore::unwrite_quit_within(run, COLD_LANE_LAYOUT_LOCK_WAIT)
        });
    }

    /// [`Self::retry_owed_layout_take_back`] through `unwrite`.
    fn retry_owed_layout_take_back_with(
        &mut self,
        now: std::time::Instant,
        unwrite: impl FnOnce(Option<&str>) -> Result<(), crate::restore::RestoreFault>,
    ) {
        let Some(run) = self
            .cold_lane_layout_debt
            .owed_take_back
            .as_ref()
            .filter(|owed| now >= owed.retry_at)
            .map(|owed| owed.run.clone())
        else {
            return;
        };
        let _ = self.take_back_cold_lane_layout_with(run, now, unwrite);
    }

    /// Before a switch that ends this run with no newer desk of it written — a
    /// seamless park, or a cold `exec` that leaves no layout (nothing to leave,
    /// or one not kept), whose successor would reopen the stale desk at its
    /// boot: an owed take-back is tried now, whatever its schedule. A refusal
    /// whose cause passes is returned, and the attempt refuses (retried,
    /// nothing touched); a settled one goes on, logged — no retry changes it,
    /// and an update held on a settled cause had no end (U48).
    fn settle_owed_layout_take_back_before_switch(
        &mut self,
        now: std::time::Instant,
    ) -> Result<(), String> {
        self.settle_owed_layout_take_back_before_switch_with(now, |run| {
            crate::restore::unwrite_quit_within(run, COLD_LANE_LAYOUT_LOCK_WAIT)
        })
    }

    /// [`Self::settle_owed_layout_take_back_before_switch`] through `unwrite`.
    fn settle_owed_layout_take_back_before_switch_with(
        &mut self,
        now: std::time::Instant,
        unwrite: impl FnOnce(Option<&str>) -> Result<(), crate::restore::RestoreFault>,
    ) -> Result<(), String> {
        let Some(run) = self
            .cold_lane_layout_debt
            .owed_take_back
            .as_ref()
            .map(|owed| owed.run.clone())
        else {
            return Ok(());
        };
        match self.take_back_cold_lane_layout_with(run, now, unwrite) {
            Ok(()) => Ok(()),
            Err(fault) if fault.passing() => Err(format!(
                "this window's layout, written for an update that did not start, could not \
                 be taken back from the restore file ({fault}); the update applies once it is"
            )),
            Err(fault) => {
                aterm_log::warn!(
                    "update apply: the switch goes on with the cold lane's earlier layout \
                     still in the restore file ({fault})"
                );
                Ok(())
            }
        }
    }

    /// At quit: an owed take-back is replaced by the quit's own write of this
    /// run's desk (`quit_wrote_its_desk`: the quit wrote, or tried to write,
    /// its layout, stamped with this run), or else tried once more, waiting
    /// for the lock at most [`QUIT_TAKE_BACK_LOCK_WAIT`]: a quit that writes no
    /// layout (nothing to reopen, or restore off) left this run's stale desk
    /// for the next launch to reopen.
    pub(crate) fn settle_owed_layout_take_back_at_quit(&mut self, quit_wrote_its_desk: bool) {
        self.settle_owed_layout_take_back_at_quit_with(quit_wrote_its_desk, |run| {
            crate::restore::unwrite_quit_within(run, QUIT_TAKE_BACK_LOCK_WAIT)
        });
    }

    /// [`Self::settle_owed_layout_take_back_at_quit`] through `unwrite`.
    fn settle_owed_layout_take_back_at_quit_with(
        &mut self,
        quit_wrote_its_desk: bool,
        unwrite: impl FnOnce(Option<&str>) -> Result<(), crate::restore::RestoreFault>,
    ) {
        if quit_wrote_its_desk {
            self.cold_lane_layout_rewritten();
            return;
        }
        let Some(run) = self
            .cold_lane_layout_debt
            .owed_take_back
            .as_ref()
            .map(|owed| owed.run.clone())
        else {
            return;
        };
        let _ = self.take_back_cold_lane_layout_with(run, std::time::Instant::now(), unwrite);
    }
}

/// How long the cold lane's quit-layout write, and its take-back, wait for the
/// restore lock on the GUI main thread (round eight of the update audit, U49).
/// Another instance holds it across its boot's take or its quit's write — one
/// or two `F_FULLFSYNC`s, whose worst peer hold measured is 280 ms
/// (`native_document_journal::JournalLockPatience`) — so this covers a holder
/// at work; one stopped while it holds the lock is a passing refusal the lane
/// retries on its schedule, never a frozen window. The figure the document
/// save's outer retry allows for the same hazard.
#[cfg(unix)]
const COLD_LANE_LAYOUT_LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// The wait before an owed take-back of the cold lane's layout is tried again
/// after its first refusal ([`App::take_back_cold_lane_layout`]), doubled per
/// refusal up to [`OWED_TAKE_BACK_RETRY_CAP`]; each try waits for the lock at
/// most [`COLD_LANE_LAYOUT_LOCK_WAIT`], so a holder that never lets go costs
/// the window that much once a minute, as the lane's own retries do.
#[cfg(unix)]
const OWED_TAKE_BACK_RETRY_FIRST: std::time::Duration = std::time::Duration::from_secs(2);

/// The longest spacing of an owed take-back's tries
/// ([`OWED_TAKE_BACK_RETRY_FIRST`]).
#[cfg(unix)]
const OWED_TAKE_BACK_RETRY_CAP: std::time::Duration = std::time::Duration::from_secs(60);

/// How long the quit's try of an owed take-back waits for the restore lock
/// ([`App::settle_owed_layout_take_back_at_quit`]): the event loop is gone, and
/// the quit's crash-journal retire waits as long.
#[cfg(unix)]
const QUIT_TAKE_BACK_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// What the cold lane's quit layout still owes after an attempt that did not
/// `exec` (round eight of the update audit, the reviews of U48 and U49).
#[cfg(unix)]
#[derive(Debug, Default)]
pub(crate) struct ColdLaneLayoutDebt {
    /// The target build and cause whose tabs-not-kept record is in Messages
    /// ([`App::say_cold_lane_tabs_not_kept`]): an attempt that then did not
    /// `exec` is retried, and its record is not written again.
    not_kept_said: Option<(u64, String)>,
    /// This run's desk, written for an `exec` that did not happen, whose
    /// take-back the restore lock refused past its bound: still in
    /// `session.toml` while this process runs
    /// ([`App::take_back_cold_lane_layout`]).
    owed_take_back: Option<OwedTakeBack>,
}

/// A take-back of this run's desk the restore lock refused
/// ([`ColdLaneLayoutDebt::owed_take_back`]).
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
struct OwedTakeBack {
    /// The run the desk is stamped with; `None` for an unstamped write, which
    /// replaced the whole file.
    run: Option<String>,
    /// When the event loop tries it again.
    retry_at: std::time::Instant,
    /// Its refusals so far, which space the tries.
    refusals: u32,
}

#[cfg(unix)]
impl OwedTakeBack {
    /// The wait after the `refusals`-th refusal: [`OWED_TAKE_BACK_RETRY_FIRST`]
    /// doubled per refusal after the first, at most
    /// [`OWED_TAKE_BACK_RETRY_CAP`].
    fn spacing(refusals: u32) -> std::time::Duration {
        let doublings = refusals.saturating_sub(1).min(16);
        OWED_TAKE_BACK_RETRY_FIRST
            .saturating_mul(1 << doublings)
            .min(OWED_TAKE_BACK_RETRY_CAP)
    }
}

/// What the cold lane left its successor ([`leave_cold_lane_layout`]).
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
enum LayoutLeft {
    /// Nothing to leave: headless, or no window.
    Nothing,
    /// The quit layout is on disk; an `exec` that does not happen takes it back.
    Written,
    /// The data folder refused it for a reason that answers the same on every
    /// try (the cause): the update goes on without it, and says so.
    NotKept(String),
}

/// Writes the cold lane's quit layout (`App::cold_lane_layout`) through `write`.
///
/// A failed write is a fact about this machine's data directory, never the
/// candidate, so nothing is charged to the artifact. What it does next turns on
/// whether its cause passes ([`crate::restore::RestoreFault`]):
///
/// * PASSING (another aterm holding the restore lock past the main thread's
///   bound, a full disk, an I/O error): it refuses — no retry budget spent,
///   auto intent kept, retried on the block cooldown, as the `--hold` and
///   restore-off refusals are.
/// * SETTLED (no data dir, a data folder this user cannot write, a read-only
///   volume, a volume without the locks or syncs the write needs): no retry
///   changes it. Refusing held the update with no end, re-probed every
///   minute, while the posture still said it installs by itself (round eight
///   of the update audit, U48). The lane goes on without the layout, as a quit
///   on this machine goes on without its own, and says so (`NotKept`): what
///   the successor does not get back is which document and Settings tabs were
///   open, never unsaved text, which [`lane_carry_refusal`] already refused.
///
/// A write that failed AFTER it published (its directory's sync) left the
/// layout a reader sees: `Written`, which the successor that starts next reads
/// and an `exec` that does not happen takes back. Returned as a refusal, it
/// stayed on disk while this process stayed up, for a later launch to open
/// beside it.
#[cfg(unix)]
fn leave_cold_lane_layout(
    layout: Option<&crate::restore::RestoreManifest>,
    write: impl FnOnce(&crate::restore::RestoreManifest) -> Result<(), crate::restore::RestoreFault>,
) -> Result<LayoutLeft, crate::UpdateHandoffStartError> {
    let Some(layout) = layout else {
        return Ok(LayoutLeft::Nothing);
    };
    match write(layout) {
        Ok(()) => Ok(LayoutLeft::Written),
        Err(fault) if fault.published() => {
            aterm_log::warn!("update apply: the cold lane's layout is on disk, but {fault}");
            Ok(LayoutLeft::Written)
        }
        Err(fault) if fault.passing() => Err(crate::UpdateHandoffStartError::refused(format!(
            "could not save the open windows for the relaunch: {fault}; the update \
             applies once the layout can be saved"
        ))),
        Err(fault) => Ok(LayoutLeft::NotKept(fault.to_string())),
    }
}

/// What `lane` cannot carry of what the preflight's `token` let ride, as the
/// refusal the update row reads, or `None` when the lane carries all of it.
///
/// * The cold lane reopens no draft: its quit layout (`App::cold_lane_layout`)
///   reopens tabs but carries no unsaved text, so every carried editor draft,
///   Settings draft and failed checkpoint is refused.
/// * The seamless lane carries everything to a successor at least this build. An
///   older successor ignores the Settings-drafts field, so a hop to an older build
///   is refused (the build number is all that is known of it). Editor drafts ride
///   the draft journal, not this field.
#[cfg(unix)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "NativeUpdateSettingsDraftCarry",
        action = "ColdExec",
        project = "aterm_gui::settings_draft_carry_conformance::Rig::project"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "NativeUpdateSettingsDraftCarry",
        action = "ParkOlder",
        project = "aterm_gui::settings_draft_carry_conformance::Rig::project"
    )
)]
pub(crate) fn lane_carry_refusal(
    token: &crate::app_native::NativeUpdateSafetyToken,
    lane: crate::native_update_admission::ApplyLane,
    running_build: u64,
    target_build: u64,
) -> Option<String> {
    use crate::native_update_admission::ApplyLane;
    match lane {
        ApplyLane::Cold if token.carried_drafts() > 0 => {
            Some(cold_lane_carried_drafts_refusal(token.carried_drafts()))
        }
        ApplyLane::Cold if token.carried_settings_drafts() > 0 => Some(format!(
            "{} {} unsaved Settings draft(s) an update cannot carry: no terminal session is \
             open to carry Settings across, so replacing the app now would close it",
            crate::app_native::SETTINGS_DRAFTS_BLOCK,
            token.carried_settings_drafts()
        )),
        ApplyLane::Cold if token.failed_checkpoints() > 0 => Some(format!(
            "{} {} document checkpoint(s) previously failed",
            crate::app_native::FAILED_CHECKPOINTS_BLOCK,
            token.failed_checkpoints()
        )),
        ApplyLane::Seamless
            if token.carried_settings_drafts() > 0 && target_build < running_build =>
        {
            Some(format!(
                "{} {} unsaved Settings draft(s) an update cannot carry: the build it installs \
                 is older than this one and would reopen Settings without them",
                crate::app_native::SETTINGS_DRAFTS_BLOCK,
                token.carried_settings_drafts()
            ))
        }
        ApplyLane::Cold | ApplyLane::Seamless => None,
    }
}

/// Each handed record's [`crate::session_store::CarriedClaims`], as the park
/// drew it, in record order: what the Commit reads again
/// ([`App::carried_claims_held`]).
#[cfg(unix)]
fn carried_claims_of(
    manifest: &crate::session_store::SessionHandoff,
) -> Vec<crate::session_store::CarriedClaims> {
    manifest
        .sessions
        .iter()
        .map(crate::session_store::CarriedClaims::of_record)
        .collect()
}

/// The session registry's projection narrowed to the sessions this attempt hands
/// over, those in `live` ([`App::handoff_live_sessions`]).
///
/// The manifest, descriptors and screens must name the same sessions
/// (`seamless::write_outgoing` refuses otherwise), and the registry still holds
/// exited panes. Connection rows are kept whole: the successor re-mints only rows
/// whose two endpoints it adopted.
#[cfg(unix)]
fn handed_sessions_only(
    mut manifest: crate::session_store::SessionHandoff,
    live: &[(u64, i32, i32)],
) -> crate::session_store::SessionHandoff {
    manifest.sessions.retain(|record| {
        live.iter()
            .any(|(local_id, _, _)| *local_id == record.local_id)
    });
    manifest
}

/// Stamps each record with its session's foreground holder
/// (`SessionRecord::fg_holder`) as the park capture read it
/// ([`ParkedScreens::fg_holders`]).
///
/// A record scalar because the control-carry sidecar is dropped for many sessions
/// (Sanitized or Repaint rung, receiver repaint, past the aggregate budget) that
/// still restore their modes; without a holder, a job that died during the park
/// would leave its alt screen and mouse modes armed under the prompt. A holder
/// that is no pgid (`<= 0`) is not written.
#[cfg(unix)]
fn stamp_fg_holders(
    mut manifest: crate::session_store::SessionHandoff,
    holders: &[(u64, i32)],
) -> crate::session_store::SessionHandoff {
    for record in &mut manifest.sessions {
        record.fg_holder = holders
            .iter()
            .find(|(local_id, _)| *local_id == record.local_id)
            .map(|(_, holder)| *holder)
            .filter(|holder| *holder > 0);
    }
    manifest
}

/// Stamp every handed session's crash journal key on its manifest record
/// (`SessionRecord::journal_key`, round seven of the update audit, leftover
/// I2b): `keys` is [`crate::App::journal_keys`], empty unless this process's
/// journal still holds the image it took over at its own Commit. A record with
/// none carries no key, so an older reader sees the wire it always saw.
#[cfg(unix)]
fn stamp_journal_keys(
    mut manifest: crate::session_store::SessionHandoff,
    keys: &[(u64, (u32, u64))],
) -> crate::session_store::SessionHandoff {
    for record in &mut manifest.sessions {
        record.journal_key = keys
            .iter()
            .find(|(local_id, _)| *local_id == record.local_id)
            .map(|(_, key)| crate::session_store::journal_key_row(*key));
    }
    manifest
}

/// Stamp every handed session's render debt on its manifest record
/// ([`ParkedScreens::render_debts`]): `"<shift>,<pushed>"`, only when the
/// engine owed rows on the alternate screen. A record with none carries no key,
/// so an older reader sees the wire it always saw, and a successor reads a
/// missing key as "owes nothing" - the debt is carried, never a verdict.
#[cfg(unix)]
fn stamp_render_debts(
    mut manifest: crate::session_store::SessionHandoff,
    debts: &[(u64, (i32, i32))],
) -> crate::session_store::SessionHandoff {
    for record in &mut manifest.sessions {
        record.render_debt = debts
            .iter()
            .find(|(local_id, _)| *local_id == record.local_id)
            .filter(|(_, debt)| *debt != (0, 0))
            .map(|(_, (shift, pushed))| crate::session_store::render_debt_row(*shift, *pushed));
    }
    manifest
}

/// Keep the actionable handoff cause in the returned refusal, which the updater
/// records in its durable status. A separate warning cannot answer a later
/// `update status`, and an unrelated admission failure must keep its own reason.
#[cfg(unix)]
fn update_admission_refusal(
    reason: crate::native_update_admission::AdmissionBlock,
    facts: crate::native_update_admission::AdmissionFacts,
    unavailable: Option<HandoffUnavailable>,
) -> crate::UpdateHandoffStartError {
    let message = if reason == crate::native_update_admission::AdmissionBlock::LivePtysNeedSeamless
        && facts.foreground_jobs <= facts.live_ptys
        && let Some(why) = unavailable
    {
        format!(
            "Update kept {} live terminal session(s), including {} foreground job(s), running: seamless handoff unavailable because {}.{}",
            facts.live_ptys,
            facts.foreground_jobs,
            why.cause(),
            why.remedy(),
        )
    } else {
        reason.message(facts)
    };
    crate::UpdateHandoffStartError::refused(message)
}

#[cfg(all(test, unix))]
mod admission_refusal_detail_tests {
    use super::{HandoffUnavailable, update_admission_refusal};
    use crate::native_update_admission::{
        AdmissionBlock, AdmissionDecision, AdmissionFacts, classify,
    };

    fn live_facts() -> AdmissionFacts {
        AdmissionFacts {
            staged_verified: true,
            seamless_capable: false,
            native_state_certified: true,
            live_ptys: 3,
            foreground_jobs: 2,
            unknown_foregrounds: 0,
        }
    }

    #[test]
    fn live_session_refusal_retains_exact_handoff_cause_and_remedy() {
        for (unavailable, cause, remedy) in [
            (
                HandoffUnavailable::UnownedControlSocket,
                "the configured control socket is not owned by this process",
                "Resolve the control-socket ownership conflict before updating.",
            ),
            (
                HandoffUnavailable::Headless,
                "--headless (no window)",
                "A windowed launch restores the default.",
            ),
        ] {
            let facts = live_facts();
            let AdmissionDecision::Block(reason) = classify(facts) else {
                panic!("live terminals without seamless handoff must block");
            };
            let refusal = update_admission_refusal(reason, facts, Some(unavailable));
            assert!(
                refusal.is_refused(),
                "a refusal cannot increment a failure streak"
            );
            let message = refusal.into_message();
            assert!(message.contains(cause), "{message}");
            assert!(message.contains(remedy), "{message}");
            assert!(message.contains("3 live terminal session(s)"), "{message}");
            assert!(message.contains("2 foreground job(s)"), "{message}");
            assert!(
                message.chars().count() <= 400,
                "durable health truncates at 400 characters"
            );
            // Historical construction kept the counts but lost both actionable facts.
            assert!(!reason.message(facts).contains(cause));
            assert!(!reason.message(facts).contains(remedy));
        }
    }

    /// ROUND EIGHT, U26: A FOREGROUND THE PROBE COULD NOT READ DOES NOT HIDE
    /// WHY THE HANDOFF IS UNAVAILABLE. With the seamless lane gone, every live
    /// terminal blocks the update whatever its foreground is, so the probe
    /// changes no decision; it only chose the words. Its refusal said the
    /// foreground "could not be proven safe", which nothing the person does
    /// changes, and dropped the cause and the remedy (`--headless`, a windowed
    /// launch) the live-session refusal carries — and the recorded reason
    /// flipped between the two across retries as the probe answered.
    ///
    /// RED before the fix: the message was the probe's, without either.
    #[test]
    fn an_unreadable_foreground_keeps_the_handoffs_cause_and_remedy() {
        for (unavailable, cause, remedy) in [
            (
                HandoffUnavailable::Headless,
                "--headless (no window)",
                "A windowed launch restores the default.",
            ),
            (
                HandoffUnavailable::UnownedControlSocket,
                "the configured control socket is not owned by this process",
                "Resolve the control-socket ownership conflict before updating.",
            ),
        ] {
            for (foreground_jobs, unknown_foregrounds) in [(0, 1), (1, 2), (0, 3)] {
                let facts = AdmissionFacts {
                    foreground_jobs,
                    unknown_foregrounds,
                    ..live_facts()
                };
                let AdmissionDecision::Block(reason) = classify(facts) else {
                    panic!("live terminals without seamless handoff must block");
                };
                let refusal = update_admission_refusal(reason, facts, Some(unavailable));
                assert!(refusal.is_refused(), "a refusal, never a failure");
                let message = refusal.into_message();
                assert!(message.contains(cause), "{facts:?}: {message}");
                assert!(message.contains(remedy), "{facts:?}: {message}");
                assert!(message.contains("3 live terminal session(s)"), "{message}");
            }
        }
    }

    #[test]
    fn unrelated_admission_refusals_keep_the_primary_reason() {
        let base = live_facts();
        for (facts, expected) in [
            (
                AdmissionFacts {
                    staged_verified: false,
                    ..base
                },
                AdmissionBlock::UnverifiedStage,
            ),
            (
                AdmissionFacts {
                    native_state_certified: false,
                    ..base
                },
                AdmissionBlock::NativeStateUncertified,
            ),
            (
                AdmissionFacts {
                    live_ptys: 0,
                    foreground_jobs: 0,
                    unknown_foregrounds: 1,
                    ..base
                },
                AdmissionBlock::ForegroundProbeUnknown,
            ),
            (
                AdmissionFacts {
                    foreground_jobs: 4,
                    ..base
                },
                AdmissionBlock::LivePtysNeedSeamless,
            ),
        ] {
            assert_eq!(classify(facts), AdmissionDecision::Block(expected));
            let refusal =
                update_admission_refusal(expected, facts, Some(HandoffUnavailable::Headless));
            assert!(refusal.is_refused());
            assert_eq!(refusal.into_message(), expected.message(facts));
        }
        assert_eq!(
            update_admission_refusal(AdmissionBlock::LivePtysNeedSeamless, base, None)
                .into_message(),
            AdmissionBlock::LivePtysNeedSeamless.message(base),
        );
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

/// Blocks (bounded) until the overlap-handoff child signals readiness, closes
/// its proof pipe, or times out. Never calls `Child::try_wait`, which would reap
/// an exited group leader and lose the pid before the unique reaper signals its
/// descendants. Pre-ready death is proof EOF, or the candidate's end asked
/// without reaping (`candidate_gone`), since a stray write-end copy can hold EOF
/// back; an exit after the proof is rejected by the atomic Commit write (EPIPE).
/// The deadline defaults to 30 s and is tunable via
/// `ATERM_HANDOFF_READY_TIMEOUT_MS`, a development seam (`aterm_types::dev_seam!`).
///
/// `masters` are watched for session death (HUP/ERR/NVAL → `Rejected`) but not
/// for readable output, which waits in the kernel for the child's parser. A
/// cancel poke returns `ActivityRevoked`.
///
/// The three mutually exclusive verdicts on one `poll` answer during the ready
/// wait, split out so the anti-spin contract is testable: queued master output
/// must land on [`ReadyPollAction::NoProgress`] (the yielding branch), never on
/// an immediate re-poll.
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReadyPollAction {
    /// A handed-off master reported HUP/ERR/NVAL — the adopted live-set identity
    /// is stale; reject.
    SessionDied,
    /// The proof fd is readable (POLLIN or its own HUP/ERR/NVAL) — read the wire.
    ReadProof,
    /// Neither: `poll` returned for queued readable output (plain POLLIN, not an
    /// abort) or a bare slice. The caller must yield before re-polling, or a master
    /// that stays readable would make `poll` return instantly forever.
    NoProgress,
}

/// Classifies one `poll` answer. Death on any master dominates; otherwise a
/// readable proof fd is progress; otherwise yield. `pollfds[0]` is the proof fd,
/// `pollfds[1..]` the watched masters (see [`wait_handoff_ready`]).
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

/// What the main thread hands `prelaunch_out_of_band_handoff`: every pre-park
/// fact the launched lane needs, taken from `start_unix_update_handoff` at the
/// point the lane was decided.
#[cfg(target_os = "macos")]
struct PrelaunchArgs {
    attempt_id: u64,
    build: u64,
    mode: crate::native_updater_service::ApplyMode,
    apply_attempt: Option<crate::native_updater_service::ApplyAttemptTicket>,
    same_image: Option<SameImageHandoff>,
    target_build: u64,
    target_commit: String,
    command: std::process::Command,
    verify_staged_candidate: bool,
    /// The chunked-grant fact the lane choice admitted this attempt on.
    grant_chunks: bool,
    installed_activation: bool,
    bundle: Option<std::path::PathBuf>,
    cancel: std::sync::mpsc::SyncSender<()>,
    cancelled: std::sync::mpsc::Receiver<()>,
    job_tx: std::sync::mpsc::SyncSender<HandoffWorkerJob>,
    reconcile_worker: crate::app_native::NativeUpdateReconcileSender,
    reconcile_ticket: crate::app_native::NativeUpdateReconcileTicket,
}

/// The facts the park gate reads, in a shape a test can enumerate. Each is
/// measured on the main thread right before the gate runs, never remembered from
/// the attempt's entry, which may be minutes earlier.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ParkGateFacts {
    pub(crate) mode: crate::native_updater_service::ApplyMode,
    /// `App::automatic_apply_phase`: where the automatic lane stands on its
    /// ladder at this instant. Ignored by the explicit modes.
    pub(crate) phase: crate::native_update_auto_intent::ApplyPhase,
    /// `App::automatic_activity_facts`: the terminal's quiet / keys / output /
    /// focus facts, sampled at this instant.
    pub(crate) activity: crate::native_update_auto_intent::ActivityFacts,
    /// `!handoff_masters_have_activity`: no live master has bytes waiting that
    /// a reader has not consumed.
    pub(crate) masters_quiet: bool,
    /// `!handoff_masters_closed`: no handed master has hung up. A dead one would be
    /// refused by the post-park re-check anyway, so the gate waits rather than
    /// freezing the terminal to find out. Exited panes are not handed
    /// (`App::handoff_live_sessions`, re-read every gate run), so this is false only
    /// between a command's exit and the registry marking it.
    pub(crate) masters_alive: bool,
    /// How many times in a row the gate has answered `Wait` in the `Land` phase,
    /// which can only be the masters holding it. Past [`PRELAUNCH_LAND_MAX_WAITS`]
    /// the park proceeds over queued output, never over a dead master.
    pub(crate) land_waits: u8,
    /// The successor's signed handoff policy says `park_quiet_gate_at_land =
    /// "relaxed"`: at `Land` the park proceeds over queued output at once rather
    /// than after [`PRELAUNCH_LAND_MAX_WAITS`] waits. Never over a dead master or in
    /// an earlier phase; the capture's mid-sequence check still applies.
    pub(crate) land_gate_relaxed: bool,
    /// How long the successor has been holding its claim.
    pub(crate) held_for: std::time::Duration,
    /// A `video` take (or its export) is live and owed a reply
    /// ([`crate::App::video_request_live`]). Before `Land` the automatic lanes
    /// wait for it, for at most [`VIDEO_PARK_WAIT_MAX`] of the hold
    /// ([`recording_park_refusal`]); after that the park proceeds and Commit
    /// aborts the take with a reply.
    pub(crate) recording: bool,
}

/// What the park gate decides.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParkGate {
    /// Park now.
    Park,
    /// Not now; the successor keeps holding and the gate re-runs.
    Wait(&'static str),
    /// The hold cap expired without a moment to park in: stand the successor
    /// down. Activity-revoked, so the intent is retained and the automatic lane
    /// tries again at a later quiet window.
    StandDown(&'static str),
}

/// When the terminal may be frozen, decided separately from starting the update.
///
/// The entry gate still decides whether an attempt starts
/// (`AutomaticAttemptRequiresQuietOrClosedGraceWindow` binds the launch), but the
/// launch costs the user nothing: readers stay live through the successor's
/// swap, re-exec and boot check. This gate decides what the user feels:
///
/// * `Immediate`: the person asked; park at once.
/// * The automatic modes: the ladder
///   (`native_update_auto_intent::automatic_park_refusal`) at its live phase: a
///   quiet moment while idle is preferred, then a gap in output while an aterm
///   window is focused, then a gap in typing, then nothing. A successor held into a
///   later phase parks by that phase's rule.
///
/// Bytes waiting unread on a live master are a preference, not a correctness
/// gate: at `Land` the gate gives way after [`PRELAUNCH_LAND_MAX_WAITS`] waits,
/// since a producer faster than the parser keeps `POLLIN` set indefinitely.
/// Parking over them loses nothing at a sequence boundary: the carry is taken
/// from the engine and the successor replays the kernel queue after Commit
/// (`HandoffCommitFacts`). A reader parked inside a sequence is caught by the
/// capture as `CaptureFailure::MidSequence` and re-parked
/// ([`mid_sequence_miss_disposition`]); a partial sequence is abandoned only over
/// a quiet PTY.
///
/// The successor's signed policy can bring the `Land` bound forward to the first
/// wait (`park_quiet_gate_at_land = "relaxed"`,
/// [`ParkGateFacts::land_gate_relaxed`]); nothing else here is the policy's.
///
/// Monotone in `held_for`: past the cap every automatic mode stands down and
/// never admits again, which bounds the hold.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
pub(crate) fn prelaunch_park_admitted(
    facts: ParkGateFacts,
    hold_cap: Option<std::time::Duration>,
) -> ParkGate {
    use crate::native_updater_service::ApplyMode;
    if matches!(facts.mode, ApplyMode::Immediate) {
        return ParkGate::Park;
    }
    if hold_cap.is_some_and(|cap| facts.held_for >= cap) {
        return ParkGate::StandDown(
            "the terminal never offered a moment to pause in within the hold cap",
        );
    }
    if let Some(reason) =
        crate::native_update_auto_intent::automatic_park_refusal(facts.phase, facts.activity)
    {
        return ParkGate::Wait(reason);
    }
    if let Some(reason) =
        recording_park_refusal(facts.mode, facts.phase, facts.recording, facts.held_for)
    {
        return ParkGate::Wait(reason);
    }
    if !facts.masters_alive {
        return ParkGate::Wait("a session's command has exited but its pane is still open");
    }
    if !facts.masters_quiet
        && !(facts.phase == crate::native_update_auto_intent::ApplyPhase::Land
            && (facts.land_waits >= PRELAUNCH_LAND_MAX_WAITS || facts.land_gate_relaxed))
    {
        return ParkGate::Wait("a session has output waiting that its reader has not taken");
    }
    ParkGate::Park
}

/// How long a live `video` take may hold an automatic park before `Land`: the
/// verb's maximum duration ([`crate::control::control_media::VIDEO_MAX_DURATION`]),
/// so a running take can finish and answer its client.
#[cfg(unix)]
pub(crate) const VIDEO_PARK_WAIT_MAX: std::time::Duration =
    crate::control::control_media::VIDEO_MAX_DURATION;

#[cfg(unix)]
const _: () = assert!(
    VIDEO_PARK_WAIT_MAX.as_secs() + 10 < PRELAUNCH_HOLD_MAX.as_secs(),
    "a recording's wait must come due well inside the hold, or a take stands the successor \
     down instead of being answered"
);

/// Whether a live `video` take holds this park.
///
/// Commit aborts a running take with a reply it waits to see written
/// ([`crate::App::video_answer_before_commit`]). Before that the automatic lanes
/// let the take finish, in every phase but `Land` and on the launched lane for at
/// most [`VIDEO_PARK_WAIT_MAX`] of the hold. Both bounds come due (the ladder
/// reaches `Land` within [`crate::native_update_auto_intent::LANDS_WITHIN`]); an
/// explicit apply never waits.
#[cfg(unix)]
#[must_use]
pub(crate) fn recording_park_refusal(
    mode: crate::native_updater_service::ApplyMode,
    phase: crate::native_update_auto_intent::ApplyPhase,
    recording: bool,
    held_for: std::time::Duration,
) -> Option<&'static str> {
    (recording
        && mode.is_automatic()
        && phase != crate::native_update_auto_intent::ApplyPhase::Land
        && held_for < VIDEO_PARK_WAIT_MAX)
        .then_some("a video recording is running")
}

/// The detail a hold-cap stand-down carries into the completion and ledger: the
/// cap's reason, how long the successor was held, and the last thing the gate was
/// waiting for, so a flooded master is not reported as the user being busy.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
pub(crate) fn hold_cap_stand_down_detail(
    reason: &str,
    held_for: std::time::Duration,
    last_wait: Option<&str>,
) -> String {
    let held = held_for.as_secs();
    match last_wait {
        Some(waiting_for) => format!(
            "{reason} ({held} s after the successor dialled; the park was last held back \
             because {waiting_for})"
        ),
        None => format!("{reason} ({held} s after the successor dialled)"),
    }
}

/// How many consecutive `Wait` answers the gate gives in `Land` before parking
/// over output still queued on a master: one second at the 20 ms re-run, enough
/// for a lagging reader, so a permanently readable master costs one second rather
/// than the whole hold.
#[cfg(unix)]
pub(crate) const PRELAUNCH_LAND_MAX_WAITS: u8 = 50;

#[cfg(unix)]
const _: () = assert!(
    PRELAUNCH_PARK_RETRY.as_millis() * (PRELAUNCH_LAND_MAX_WAITS as u128)
        < PRELAUNCH_HOLD_MAX.as_millis(),
    "the Land relaxation must come due well inside the hold it is spending, or a \
     flooded master still stands the successor down"
);

/// The hold cap for `mode`: the automatic lanes are bounded, the explicit ones
/// park at once and so can never reach a cap.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
pub(crate) fn prelaunch_hold_cap(
    mode: crate::native_updater_service::ApplyMode,
) -> Option<std::time::Duration> {
    mode.is_automatic().then_some(PRELAUNCH_HOLD_MAX)
}

/// One run of the park gate for a prelaunched attempt.
#[cfg(unix)]
#[derive(Debug)]
pub(crate) enum ParkAttempt {
    /// Parked, captured, and the capture is on its way to the worker.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    Parked,
    /// Not now — re-run the gate after [`PRELAUNCH_PARK_RETRY`].
    #[cfg(any(target_os = "macos", all(test, unix)))]
    NotYet(&'static str),
    /// The park missed: the readers were parked and rolled back, nothing granted,
    /// and the successor still holds. Re-park once, after [`PRELAUNCH_REPARK_DELAY`]
    /// and on the next freeze-budget rung.
    ///
    /// Timing only: a reader that missed the deadline, a capture that outran it, an
    /// engine lock not taken, storage not reserved, or a reader parked mid-sequence
    /// (`CaptureFailure::MidSequence`). Each can come out differently 500 ms later;
    /// a deterministic refusal never belongs here.
    Missed(String),
    /// The park missed only because a parser was mid-sequence with the rest still
    /// queued (`CaptureFailure::MidSequence`): it re-parks at the gate's 20 ms
    /// cadence without climbing the rung, up to [`PRELAUNCH_MAX_MID_SEQUENCE_REPARKS`]
    /// ([`mid_sequence_miss_disposition`]), then becomes [`Self::Missed`]. The fork
    /// lane files it as a miss directly.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    MissedMidSequence(String),
    /// The capture refused the desk: a fact about the screens, session set or
    /// layout that no rung changes. Not re-parked and never filed as a busy machine:
    /// the successor stands down as [`crate::UpdateHandoffOutcome::CaptureRefused`],
    /// retried when the desk changes.
    Refused(CaptureRefusal),
    /// Stand the held successor down with this outcome; readers (if they were
    /// parked) have already been rolled back.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    Failed(HandoffStandDown),
}

/// A deterministic refusal of the park's capture, with the session it names if
/// any, so the stand-down, ledger and `update status` say which session held the
/// update back.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaptureRefusal {
    /// The refusing session's local id, when the refusal is about one.
    pub(crate) local_id: Option<u64>,
    /// The build's own sentence for the refusal (the session, the field, the
    /// bound).
    pub(crate) cause: String,
}

#[cfg(unix)]
impl std::fmt::Display for CaptureRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.local_id {
            Some(local_id) => write!(
                formatter,
                "the park's capture refused session {local_id}: {}",
                self.cause
            ),
            None => write!(formatter, "the park's capture refused: {}", self.cause),
        }
    }
}

/// Which lane a capture failure belongs to.
///
/// Timing (`Deadline`, `EngineBusy`, `MidSequence`) and this process's storage
/// (`Storage`) are a miss: the same desk can capture 500 ms later on a wider
/// rung. `Refused` is a fact about the desk that no rung changes, so it never
/// climbs the freeze ladder or stands down as `ActivityRevoked`. Typed on the
/// variant, never the message.
#[cfg(unix)]
#[must_use]
pub(crate) fn classify_capture_failure(failure: &CaptureFailure) -> ParkAttempt {
    match failure {
        CaptureFailure::Deadline { .. }
        | CaptureFailure::EngineBusy
        | CaptureFailure::Storage
        | CaptureFailure::MidSequence { .. } => ParkAttempt::Missed(failure.to_string()),
        CaptureFailure::Refused { local_id, .. } => ParkAttempt::Refused(CaptureRefusal {
            local_id: *local_id,
            cause: failure.to_string(),
        }),
    }
}

/// The park's commitment to its post-park handoff layout, both lanes': the
/// digest the adoption proof is taken over, or the deterministic refusal the
/// lane stands down with (`CaptureRefused`, retried once the desk changes).
///
/// A LAYOUT NO BUILD READS WHOLE IS REFUSED HERE (round eight of the update
/// audit, D5, and the lead U33). The handoff capture folds a desk past the
/// window and tab caps into what one layout holds
/// (`restore::RestoreManifest::fold_into_caps`); what is still past them —
/// more than 64 windows of 256 tabs, or a tab past 256 panes or 31 nested
/// splits — fails the worker's round trip on every attempt
/// (`from_toml(to_toml(layout)) == layout`), which files it as this process's
/// own transient trouble (`ProducerFailed`) and froze every tab again on each
/// retry. It is a fact about the desk, so it is said as one, before the worker
/// starts, in words that name the cap and where it is passed
/// (`restore::RestoreManifest::why_not_whole`). A layout this passes is
/// exactly one the park committed to before.
#[cfg(unix)]
pub(crate) fn commit_handoff_layout(
    layout: &crate::restore::RestoreManifest,
) -> Result<[u8; 32], CaptureRefusal> {
    if let Some(cause) = layout.why_not_whole() {
        return Err(CaptureRefusal {
            local_id: None,
            cause,
        });
    }
    crate::seamless::layout_digest(layout).ok_or_else(|| CaptureRefusal {
        local_id: None,
        cause: "handoff layout could not be committed canonically".to_string(),
    })
}

/// Test seam for the capture's lane decision: runs the shipping park capture over
/// the whole pool with a deadline wide enough that a loaded machine cannot make
/// the answer timing, and returns what the park would do: the repainted session
/// ids of a parked capture, or the [`ParkAttempt`] a failure classifies into.
/// `native_updater_conformance` drives refused desks through it.
#[cfg(all(test, unix))]
impl App {
    pub(crate) fn capture_park_outcome_for_conformance(
        &mut self,
        caps: crate::seamless::WireCaps,
    ) -> Result<Vec<u64>, ParkAttempt> {
        // The set the shipping lanes hand over, exited panes left out.
        let live: Vec<(u64, i32, i32)> = self.handoff_live_sessions();
        let window = std::time::Duration::from_secs(30);
        self.capture_parked_screens(
            &live,
            std::time::Instant::now() + window,
            window.as_millis(),
            window / 2,
            caps,
            aterm_update_core::handoff_policy::CarryCeiling::Full,
        )
        .map(|parked| parked.repaint)
        .map_err(|failure| classify_capture_failure(&failure))
    }
}

/// What one park hands the worker, projected from a whole [`App`] as the fork
/// lane does: the handed set, the registry manifest, the window carry, the
/// capture, the history heads and plan, and the post-park layout with its digest.
/// The PTY descriptors are left to the caller.
#[cfg(all(test, unix))]
pub(crate) struct ParkedDeskForTest {
    pub(crate) live: Vec<(u64, i32, i32)>,
    pub(crate) manifest: crate::session_store::SessionHandoff,
    pub(crate) window: Option<crate::session_store::WindowCarry>,
    pub(crate) screens: Vec<(u64, aterm_core::terminal::TerminalCheckpoint)>,
    pub(crate) carries: Vec<crate::handoff_carry::CarrySource>,
    pub(crate) screen_digest: [u8; 32],
    pub(crate) repaint: Vec<u64>,
    /// Each handed session's history fence under the park, as the worker's
    /// join takes it.
    pub(crate) history_heads: Vec<crate::handoff_history::HistoryHead>,
    /// The fork lane's history plan for this park, built by the call the lane
    /// makes (`HistoryPlan::deferred_under`), for the caller to run as the
    /// worker does (`HistoryPlan::results`, then `stamp_manifest`).
    pub(crate) history: crate::handoff_history::HistoryPlan,
    pub(crate) layout: crate::restore::RestoreManifest,
    pub(crate) layout_digest: Option<[u8; 32]>,
    /// The held panes' screens the capture carried.
    pub(crate) held: Vec<crate::seamless::HeldScreen>,
}

/// Whole-app test seam: the fork lane's park, from registry projection to the
/// post-park layout, over this App's real pool, with the successor's caps for
/// `target_build` and a deadline wide enough that a loaded machine cannot make the
/// answer timing. `seamless::app_capture_tests` drives it through the manifest
/// writer and the successor's reader.
#[cfg(all(test, unix))]
impl App {
    /// The park described above. The successor's handoff policy for `attempt` is
    /// read by [`Self::park_policy`], as both lanes do, and its carry ceiling bounds
    /// the capture. `None` is the same-image seam: no policy.
    pub(crate) fn park_desk_for_test(
        &mut self,
        attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
    ) -> Result<ParkedDeskForTest, CaptureFailure> {
        let target_build = attempt.map_or_else(crate::running_build_number, |attempt| {
            attempt.target_build()
        });
        let ceiling = self.park_policy(attempt).carry_ceiling();
        let live = self.handoff_live_sessions();
        let manifest = handed_sessions_only(
            crate::session_store::SessionHandoff::from_store(
                &self
                    .store
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            ),
            &live,
        );
        let window = self.park_window_carry(target_build);
        let budget = std::time::Duration::from_secs(30);
        let ParkedScreens {
            screens,
            carries,
            screen_digest,
            repaint,
            fg_holders,
            render_debts,
            history_heads,
            held,
        } = self.capture_parked_screens(
            &live,
            std::time::Instant::now() + budget,
            budget.as_millis(),
            budget / 2,
            crate::seamless::WireCaps::for_target(target_build),
            ceiling,
        )?;
        // The foreground holders ride the manifest records, whatever rung, as
        // both lanes stamp them.
        let manifest = stamp_fg_holders(manifest, &fg_holders);
        let manifest = stamp_render_debts(manifest, &render_debts);
        let manifest = stamp_journal_keys(manifest, &self.journal_keys());
        let layout = self.capture_handoff_layout();
        let layout_digest = crate::seamless::layout_digest(&layout);
        // The fork lane's plan, by the fork lane's own call: exported by the
        // caller, as the lane's worker exports it once the capture is done.
        let history = crate::handoff_history::HistoryPlan::deferred_under(ceiling, || {
            self.history_sessions(target_build)
        });
        Ok(ParkedDeskForTest {
            live,
            manifest,
            window,
            screens,
            carries,
            screen_digest,
            repaint,
            history_heads,
            history,
            layout,
            layout_digest,
            held,
        })
    }
}

/// The stand-down a [`ParkAttempt::Refused`] park sends: typed `CaptureRefused`,
/// naming the session, so the refusal lane and the ledger say what held the
/// update back.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
pub(crate) fn capture_refusal_stand_down(refusal: &CaptureRefusal) -> HandoffStandDown {
    HandoffStandDown {
        outcome: crate::UpdateHandoffOutcome::CaptureRefused,
        detail: format!("{refusal}; the lane retries once the desk changes"),
    }
}

/// How often the event loop re-runs the park gate while a prelaunched attempt
/// waits for a quiet moment. Short enough that the park lands inside the quiet
/// epoch it is waiting for (500 ms), long enough to be no load.
#[cfg(unix)]
const PRELAUNCH_PARK_RETRY: std::time::Duration = std::time::Duration::from_millis(20);

/// How long the gate waits before re-parking after a park that missed its
/// budget. Long enough for whatever took the engine lock (the scrollback
/// compressor, a render) to finish, short enough that the successor's hold is
/// not visibly spent on it.
#[cfg(any(target_os = "macos", all(test, unix)))]
const PRELAUNCH_REPARK_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// How many times one prelaunched attempt re-parks before standing its successor
/// down. Two: the whole freeze ladder ([`handoff_freeze_budget`]) is climbed within
/// one attempt, 500 ms apart, with the successor still holding, rather than a
/// relaunch per rung. A miss is the parent's scheduling failure and says nothing
/// about the candidate, so it never reaches the ledger; past the last rung the
/// attempt stands down as a busy-machine fact ([`park_miss_disposition`]).
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) const PRELAUNCH_MAX_PARK_MISSES: u8 = 2;

/// What one park that missed its freeze budget costs the prelaunched attempt.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Debug)]
pub(crate) enum ParkMissDisposition {
    /// Re-park after [`PRELAUNCH_REPARK_DELAY`] on the ladder's next rung;
    /// `misses` is the attempt's new miss count, which is also the rung the
    /// re-park widens to.
    Repark { misses: u8, reason: String },
    /// A mid-sequence miss inside its own bound: re-park after
    /// [`PRELAUNCH_PARK_RETRY`] on the SAME rung; `reparks` is the attempt's new
    /// mid-sequence count.
    RetryMidSequence { reparks: u8, reason: String },
    /// Every rung was tried. Stand the successor down.
    StandDown(HandoffStandDown),
}

/// How many mid-sequence misses one prelaunched attempt re-parks on for free. Past
/// [`PRELAUNCH_LAND_MAX_WAITS`], `Land` parks inside a flood, where a reader can
/// stop inside an escape sequence; the flood's next boundary is elsewhere, so these
/// re-park at the 20 ms cadence without climbing the rung. Bounded (within a second
/// of the hold), after which the ordinary ladder applies.
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) const PRELAUNCH_MAX_MID_SEQUENCE_REPARKS: u8 = 32;

/// [`aterm_core::terminal::Terminal::output_state_in_flight`]'s name for an
/// OSC 8 link still open — on a hop to a successor older than the output-state
/// carry, the one state it drops whose miss is soft
/// ([`App::soft_miss_may_repark`]).
#[cfg(unix)]
const OPEN_LINK_STATE: &str = "Osc8HyperlinkOpen";

/// The drain gate's per-respin yield ([`App::handoff_drain_gate`]), so its
/// fence is measured in loop iterations that really dispatched events instead
/// of a busy spin — and the grain under which a blocking wait at Commit opened
/// no window the fence did not already have ([`App::commit_flush_refences`]).
#[cfg(unix)]
const HANDOFF_INPUT_DRAIN_YIELD: std::time::Duration = std::time::Duration::from_millis(2);

#[cfg(any(target_os = "macos", all(test, unix)))]
const _: () = assert!(
    (PRELAUNCH_MAX_MID_SEQUENCE_REPARKS as u64) * 20 <= 1_000
        && PRELAUNCH_PARK_RETRY.as_millis() == 20
);

/// A miss that was only a parser mid-sequence: a free re-park while `reparks`
/// is inside [`PRELAUNCH_MAX_MID_SEQUENCE_REPARKS`], otherwise the ordinary
/// ladder ([`park_miss_disposition`]).
#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
pub(crate) fn mid_sequence_miss_disposition(
    misses: u8,
    reparks: u8,
    reason: String,
) -> ParkMissDisposition {
    if reparks < PRELAUNCH_MAX_MID_SEQUENCE_REPARKS {
        ParkMissDisposition::RetryMidSequence {
            reparks: reparks.saturating_add(1),
            reason,
        }
    } else {
        park_miss_disposition(misses, reason)
    }
}

/// A park miss is a fact about the machine's moment, never about the bytes.
///
/// The readers came back and nothing was granted or written; a busy machine only
/// failed to park or capture inside a comfort budget. Like a keystroke or the hold
/// cap, that is a scheduling fact, so past the last rung the successor stands down
/// as `ActivityRevoked`: retried on the activity spacing, no physical budget spent,
/// never a manual-only latch (docs/DESIGN-auto-apply-ladder-2026-09-21.md, law 2).
///
/// Only a miss comes here; a refusal is [`ParkAttempt::Refused`].
#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
pub(crate) fn park_miss_disposition(misses: u8, reason: String) -> ParkMissDisposition {
    if misses >= PRELAUNCH_MAX_PARK_MISSES {
        ParkMissDisposition::StandDown(HandoffStandDown {
            outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
            detail: format!(
                "the park missed its budget on every rung ({reason}); the machine is busy, \
                 the candidate is not in question, and the ladder retries"
            ),
        })
    } else {
        ParkMissDisposition::Repark {
            misses: misses.saturating_add(1),
            reason,
        }
    }
}

/// How long Commit waits at most for every live `video` request it answers to
/// reach its client before `_exit`: an export's encode worker seeing its
/// cancellation and removing its directory, and each reply written
/// ([`crate::App::video_answer_before_commit`]). Spent only on a wedged worker or
/// client.
#[cfg(unix)]
pub(crate) const VIDEO_EXPORT_COMMIT_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

/// How long the seamless parent waits at Commit, and the cold lane before `exec`
/// (`prepare_cold_exec`), for the message-log writer to land what it holds
/// ([`crate::App::flush_messages_log_within`]). Spent only on a throttled or
/// starved writer; what does not land is noted in `aterm.log`.
#[cfg(unix)]
pub(crate) const MESSAGES_FLUSH_COMMIT_WAIT: std::time::Duration =
    std::time::Duration::from_millis(250);

/// How long a dialled successor is held for a quiet moment before the attempt
/// stands down (activity-revoked, retried later). Longer than the whole ladder
/// ([`crate::native_update_auto_intent::LANDS_WITHIN`]), so a successor held
/// through a busy stretch reaches `Land` and parks. Only a hold the ladder never
/// relaxes (the user's consent warm-up) can outlast it.
#[cfg(unix)]
pub(crate) const PRELAUNCH_HOLD_MAX: std::time::Duration = std::time::Duration::from_secs(120);

#[cfg(unix)]
const _: () = assert!(
    crate::native_update_auto_intent::LANDS_WITHIN.as_secs() + 10 < PRELAUNCH_HOLD_MAX.as_secs()
);

/// When the post-park proof wait gives up on the launched lane.
///
/// Everything slow has happened before the park, so this bounds the rest of the
/// freeze: the artifacts still to write (`prepare_outgoing_artifacts`), the one
/// `sendmsg`, and the successor's GUI boot to its first present (the dominant
/// term). Anchored at the park because the frozen interval is what must be bounded.
/// A missed deadline is a kill, proof of death and resumed terminal, charged to the
/// physical retry ladder: 3 s on a first automatic attempt, 6 s after a physical
/// failure of the same bytes, 30 s for an explicit apply.
/// `ATERM_HANDOFF_PROOF_TIMEOUT_MS` overrides all three; it is a development seam
/// (`aterm_types::dev_seam!`).
#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
fn handoff_proof_deadline(
    mode: crate::native_updater_service::ApplyMode,
    prior_physical_failures: u8,
    park_at: std::time::Instant,
) -> std::time::Instant {
    use crate::native_updater_service::ApplyMode;
    let ms = aterm_types::dev_seam!("ATERM_HANDOFF_PROOF_TIMEOUT_MS")
        .and_then(|value| value.to_str()?.parse::<u64>().ok())
        .unwrap_or(match mode {
            ApplyMode::Immediate => 30_000,
            ApplyMode::Automatic | ApplyMode::AutomaticPastGrace => match prior_physical_failures {
                0 => 3_000,
                _ => 6_000,
            },
        })
        .clamp(1_000, 120_000);
    park_at + std::time::Duration::from_millis(ms)
}

/// When this attempt stops waiting for its successor to prove readiness.
///
/// A deadline rather than a timeout: on the out-of-band lane the successor must
/// dial (after its staged-swap boot apply) and then paint and prove, and one
/// deadline keeps both stages within the fork lane's budget. The fork lane
/// computes it right before its single wait.
#[cfg(unix)]
#[must_use]
fn handoff_ready_deadline() -> std::time::Instant {
    // 30 s default: the deadline covers LaunchServices latency, a first-launch
    // Gatekeeper assessment, the successor's whole staged-swap boot apply,
    // adoption and painting every window, which can exceed 15 s under compile
    // load. Expiry un-parks the parent safely, while a short deadline would make
    // the update lane never succeed on a busy machine.
    let timeout_ms = aterm_types::dev_seam!("ATERM_HANDOFF_READY_TIMEOUT_MS")
        .and_then(|v| v.to_str()?.parse::<u64>().ok())
        .unwrap_or(30_000)
        .clamp(1_000, 120_000);
    std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms)
}

/// How the outgoing process's wait for its launched successor's dial ended,
/// when no dialer was accepted ([`await_launched_dial`]).
#[cfg(target_os = "macos")]
#[derive(Debug, PartialEq, Eq)]
enum LaunchedDialEnd {
    /// The worker's cancel arrived first: structural activity revoked it.
    Cancelled,
    /// The successor is gone and no dial can come: see [`PreDialDeath`].
    SuccessorExited(PreDialDeath),
    /// The launched successor DIALLED and presented the claim, the code gate
    /// turned it away ([`crate::handoff_rendezvous::CodeRefusal`]), and it
    /// exited by its own rule — a failed claim exits before any window — which
    /// `death` reports. Filed by the refusal's cause
    /// ([`code_refused_completion`]), never as a death nobody explained (round
    /// eight of the update audit, U36's review). `dialer` is the process as the
    /// kernel attested it at the refusal.
    CodeRefused {
        dialer: HandoffCandidate,
        refusal: crate::handoff_rendezvous::CodeRefusal,
        death: PreDialDeath,
    },
    /// The launched successor dialled, was held while macOS's code-signing
    /// service did not answer its code check, and left while held: no refusal
    /// of ours ended it, so its death is filed by its own evidence, in words
    /// that say it dialled.
    ExitedWhileHeld {
        dialer: HandoffCandidate,
        death: PreDialDeath,
    },
    /// The rendezvous ended it: the dial budget ran out (with the last refusal,
    /// a dialer still held for its code check among them), or it refused one
    /// dialer too many.
    Rendezvous(crate::handoff_rendezvous::RendezvousError),
}

/// Whether `dialer` — a dialer the code gate turned away, witnessed alive at that
/// moment — is the launched process whose death `death` reports: the very process
/// LaunchServices named, or, for a death reported with no pid, a dialer that is
/// gone too. A dialer that still lives is not the process LaunchServices says
/// terminated, and one that could not be witnessed is not concluded to be it.
#[cfg(target_os = "macos")]
fn code_refused_dialer_was_launched(dialer: HandoffCandidate, death: PreDialDeath) -> bool {
    match death {
        PreDialDeath::NamedProcessGone { candidate, .. } => {
            candidate.pid == dialer.pid
                && (candidate.birth.is_none()
                    || dialer.birth.is_none()
                    || candidate.birth == dialer.birth)
        }
        PreDialDeath::TerminatedBeforeCheckIn => {
            dialer.birth.is_some() && dialer.identity() != HandoffCandidateIdentity::Corroborated
        }
    }
}

/// How the wait for the dial learned that the launched successor is gone.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreDialDeath {
    /// LaunchServices answered that the application it launched TERMINATED
    /// before it checked in (`app_launch_successor::launch_answer_proves_exit`).
    /// No pid and no status: nothing was observed about how it ended.
    TerminatedBeforeCheckIn,
    /// The process LaunchServices named for the launch is gone: the kernel recorded
    /// its exit on the watch, or its pid fell vacant or was recycled
    /// ([`NamedSuccessorWatch`]). `candidate` is its pid and first-sight birth
    /// stamp; `death` is the kernel's record, read via [`handoff_child_death`].
    NamedProcessGone {
        candidate: HandoffCandidate,
        death: crate::ChildDeathEvidence,
    },
}

#[cfg(target_os = "macos")]
impl PreDialDeath {
    /// The evidence the rollback files, and the one the counted trial launch
    /// is settled by ([`send_pre_dial_death`]).
    fn evidence(self) -> crate::ChildDeathEvidence {
        match self {
            Self::TerminatedBeforeCheckIn => crate::ChildDeathEvidence::Unobserved,
            Self::NamedProcessGone { death, .. } => death,
        }
    }
}

/// Why the accept's abort predicate fired, recorded by the predicate itself.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DialWaitAbort {
    Cancelled,
    SuccessorExited(PreDialDeath),
}

/// The pid LaunchServices named, watched until the dial.
///
/// LaunchServices answers with the pid of a process that checks in (during its
/// `+sharedApplication`) or at its ~30 s check-in timeout, and says nothing if that
/// process later dies. A successor dials before checking in, so a named process
/// dying before its dial would otherwise be found only when the dial budget ran out.
///
/// At first sight this reads the named pid's birth stamp and registers a
/// [`CandidateExitWatch`]; each later look asks, without blocking or signalling:
///
/// * the watch: XNU queues the exit in `proc_exit`, with the full status;
/// * then [`handoff_candidate_terminated`]: the pid vacant or recycled. This covers
///   a death before registration; the kernel refuses to register on a zombie while
///   `kill(pid, 0)` still calls it alive, so a failed registration is not death.
///
/// A live process is never called gone: a same-pid `execve` raises only
/// `NOTE_EXEC`, an unwatchable process is merely unwatched, and pid reuse can only
/// hide a death. The pid is watched, never signalled or admitted: only the dial and
/// the kernel's attestation at the accept grant identity.
#[cfg(target_os = "macos")]
#[derive(Default)]
struct NamedSuccessorWatch {
    /// The named process, its birth read at first sight, and its exit watch
    /// (`None` when it could not be registered).
    seen: Option<(HandoffCandidate, Option<CandidateExitWatch>)>,
}

#[cfg(target_os = "macos")]
impl NamedSuccessorWatch {
    /// One look between two accept slices: is the process LaunchServices named
    /// (`named`, peeked from its answer) gone? `None` while it lives, while nothing
    /// is named, and while the kernel has not said. LaunchServices answers once, so
    /// the first pid seen is the only one.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "native_update_launched_dial_wait",
            action = "EndOnNamedExit",
            project = "aterm_gui::app_update_handoff::launched_dial_wait_conformance::project"
        )
    )]
    fn gone(
        &mut self,
        named: Option<i32>,
    ) -> Option<(HandoffCandidate, crate::ChildDeathEvidence)> {
        if self.seen.is_none() {
            let pid = u32::try_from(named?).ok()?;
            let candidate = HandoffCandidate::of_named_pid(pid);
            self.seen = Some((candidate, CandidateExitWatch::watch(pid)));
        }
        let (candidate, watch) = self.seen.as_ref()?;
        // Nothing of ours ever signals a named process, so any status the
        // kernel recorded is its own death (`died_before_we_signalled`).
        if let Some(status) = watch.as_ref().and_then(CandidateExitWatch::exit_status) {
            return Some((*candidate, handoff_child_death(true, Some(status))));
        }
        if handoff_candidate_terminated(*candidate) {
            // After the proof, ask the watch again: the knote is queued in
            // `proc_exit` before the pid can fall vacant, so a death between the
            // read above and the probe still carries its status.
            let status = watch.as_ref().and_then(CandidateExitWatch::exit_status);
            return Some((*candidate, handoff_child_death(true, status)));
        }
        None
    }
}

/// THE CODE THE LAUNCHED SUCCESSOR RUNS, as the gate on its dial checks it (round
/// eight of the update audit, U36): the one gate before the grant that keeps out
/// a same-uid process that copied the claim secret and dials with its own code
/// (`handoff_rendezvous`'s module docs state what it leaves).
///
/// * The successor runs an update candidate (`successor_runs_candidate`: a staged
///   build, or the installed bundle an activation names — the worker verifying it
///   again or honouring a fresh cached pass alike) and a release team is pinned:
///   an aterm release's own requirement, the identifier every cut stamps and a
///   Developer ID of `team` ([`crate::handoff_rendezvous::DialerCode::release`]).
///   The candidate passed that Developer ID requirement (`codesign --verify -R`)
///   before this lane was chosen for it, and its identifier is read here, so
///   every successor gated by this rule meets it whatever its build — and so does
///   one whose boot swap was deferred and which still runs this image, when this
///   image is itself a release, as every installed build is. Every shipped build
///   pins its team, so this is the rule of every real update from a build that
///   has it. A dialer that does not meet it (a dev image whose swap was deferred,
///   a process that is not the candidate) is turned away and filed by that
///   refusal ([`LaunchedDialEnd::CodeRefused`], `Rejected`, transient), never
///   granted. The candidate's own identifier is read BEFORE the launch
///   (`candidate_identifier`; U36's second review): the Developer ID check every
///   candidate passed does not test it, so a candidate cut with another
///   identifier would pass every check, launch, dial and be refused on every
///   attempt, a deterministic failure booked as a passing one, and a veto the
///   old build holds over the new one's content that no new build can lift
///   (design laws L4 and L1). Such a candidate, or one whose identifier could
///   not be read just then, is `Err` here, and the caller hands the attempt to
///   the fork lane, whose gate does not ask: the update still happens, as it
///   did before the gate existed. A refusal at the dial is then never a verdict
///   about the candidate's bytes (a deferred swap running a dev image, or a
///   dialer that is not the candidate), which is why it stays transient.
/// * Otherwise — the same-image QA seam (the successor re-runs this image, which
///   a dev build signs ad hoc) or no team pinned: the designated requirement of
///   the code at `bundle`, read when a dialer presents the secret, which is after
///   the successor's boot swap put its build there.
///
/// Until U36's review this was chosen on whether the WORKER re-verifies the
/// candidate (`HandoffWorkerJob::verify_staged_candidate`), which is false for a
/// fresh cached pass too — the usual case, since the automatic lane pre-verifies
/// at arm time — so most real updates got the code-at-path rule, which admits
/// whatever code a same-uid process put at the install path before it dialled.
#[cfg(target_os = "macos")]
fn launched_dialer_code(
    successor_runs_candidate: bool,
    team: &str,
    bundle: &std::path::Path,
    candidate_identifier: impl FnOnce() -> Result<String, String>,
) -> Result<crate::handoff_rendezvous::DialerCode, String> {
    if successor_runs_candidate && !team.is_empty() {
        let release = crate::handoff_rendezvous::RELEASE_SIGNING_IDENTIFIER;
        let identifier = candidate_identifier()
            .map_err(|error| format!("the candidate's signing identifier was not read: {error}"))?;
        if identifier != release {
            return Err(format!(
                "the candidate is signed as {identifier:?}, and the rendezvous admits only code \
                 signed as {release:?}"
            ));
        }
        crate::handoff_rendezvous::DialerCode::release(team)
    } else {
        Ok(crate::handoff_rendezvous::DialerCode::code_at(
            bundle.to_path_buf(),
        ))
    }
}

/// The outgoing process's wait for its launched successor's dial, with every
/// reader live.
///
/// A successor that dies before its dial (a containment-deny fatal in the warm
/// prologue, a boot crash, a refused re-exec) would otherwise be seen only at the
/// dial deadline. The accept's abort peeks LaunchServices' answer between its 10 ms
/// poll slices ([`crate::app_launch_successor::LaunchInFlight::exited_before_check_in`]):
/// a process that dies before checking in (every successor dials before its
/// `NSApplication` exists) is answered as terminated with no pid, and nothing
/// alive is answered that way.
///
/// An answer naming a pid is a process seen alive; the abort then asks the kernel
/// about it ([`NamedSuccessorWatch`]), with the same rollback on its end. Every
/// other answer (an error, an unknown pid, this process's own, none yet) leaves the
/// wait unchanged, so a live successor is never refused here
/// (`NativeUpdateLaunchedDialWait`'s `NeverRefusesALiveSuccessor`). An error names
/// no process and its codes are generic, so it is not evidence. The peeks consume
/// nothing, so the corroboration after the dial still reads the answer
/// (`AnswerKeptForTheCorroboration`).
///
/// The cancel is checked first, so a revocation keeps its own name.
///
/// A dialer is accepted only if its code is `code` ([`launched_dialer_code`]): a
/// same-uid process that copied the claim out of the successor's launch
/// environment is refused at the gate, and the wait goes on for the successor
/// (round eight of the update audit, U36). The gate tells this wait about every
/// dialer it turns away while that dialer is still connected, and the wait
/// witnesses it then (pid and birth stamp): when a death ends the wait and the
/// last dialer turned away is that launched process — the pid LaunchServices
/// named, or gone too when the death names none — the wait ends on the refusal
/// ([`LaunchedDialEnd::CodeRefused`], or [`LaunchedDialEnd::ExitedWhileHeld`]
/// for one that left while held for a check `trustd` did not answer), so a
/// successor the gate turned away is filed by that cause, never as a death
/// before its dial (U36's review).
#[cfg(target_os = "macos")]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "native_update_launched_dial_wait",
        action = "Accept",
        project = "aterm_gui::app_update_handoff::launched_dial_wait_conformance::project"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "native_update_launched_dial_wait",
        action = "TimeOut",
        project = "aterm_gui::app_update_handoff::launched_dial_wait_conformance::project"
    )
)]
fn await_launched_dial(
    rendezvous: &crate::handoff_rendezvous::Rendezvous,
    in_flight: &crate::app_launch_successor::LaunchInFlight,
    code: &crate::handoff_rendezvous::DialerCode,
    deadline: std::time::Instant,
    cancel: &std::sync::mpsc::Receiver<()>,
) -> Result<crate::handoff_rendezvous::ClaimedPeer, LaunchedDialEnd> {
    let fired = std::cell::Cell::new(None);
    let named = std::cell::RefCell::new(NamedSuccessorWatch::default());
    let abort = || {
        if cancel.try_recv().is_ok() {
            fired.set(Some(DialWaitAbort::Cancelled));
            return true;
        }
        if in_flight.exited_before_check_in() {
            fired.set(Some(DialWaitAbort::SuccessorExited(
                PreDialDeath::TerminatedBeforeCheckIn,
            )));
            return true;
        }
        if let Some((candidate, death)) = named.borrow_mut().gone(in_flight.named_pid()) {
            fired.set(Some(DialWaitAbort::SuccessorExited(
                PreDialDeath::NamedProcessGone { candidate, death },
            )));
            return true;
        }
        false
    };
    // The last dialer the code gate turned away, witnessed while its connection
    // was still open (pid and birth stamp), so a death reported after it is laid
    // at the refusal's door only when that dialer is the launched process.
    let turned_away = std::cell::RefCell::new(None);
    let refused = |dialer: &crate::handoff_rendezvous::CodeRefusedDialer| {
        *turned_away.borrow_mut() = u32::try_from(dialer.pid)
            .ok()
            .map(|pid| (HandoffCandidate::of_attested_peer(pid), dialer.clone()));
    };
    // No expected pid at the gate (see the caller); the dialer's code is checked
    // there, so a stranger holding the secret is refused and the wait goes on.
    match rendezvous.accept_claim_observed(None, code, deadline, &abort, &refused) {
        Ok(peer) => Ok(peer),
        Err(crate::handoff_rendezvous::RendezvousError::Cancelled) => Err(match fired.get() {
            Some(DialWaitAbort::SuccessorExited(death)) => {
                let launched = turned_away
                    .take()
                    .filter(|(witness, _)| code_refused_dialer_was_launched(*witness, death));
                match launched {
                    Some((witness, turned))
                        if turned.end == crate::handoff_rendezvous::CodeRefusedEnd::Closed =>
                    {
                        LaunchedDialEnd::CodeRefused {
                            dialer: witness,
                            refusal: turned.refusal,
                            death,
                        }
                    }
                    Some((witness, _)) => LaunchedDialEnd::ExitedWhileHeld {
                        dialer: witness,
                        death,
                    },
                    None => LaunchedDialEnd::SuccessorExited(death),
                }
            }
            Some(DialWaitAbort::Cancelled) | None => LaunchedDialEnd::Cancelled,
        }),
        Err(error) => Err(LaunchedDialEnd::Rendezvous(error)),
    }
}

/// The rollback of a successor that died before its dial, and the settling of
/// the boot-trial launch it counted.
///
/// Rolled back like the dial deadline: nothing accepted, no descriptor left, so
/// the readers never stopped (`NeverTransferred`). Filed `ChildDied` with the
/// wait's evidence: `Unobserved` when LaunchServices reported the termination, the
/// kernel's record when the named process's exit watch saw it.
///
/// The counted trial launch is settled by that evidence through
/// [`forgives_the_counted_trial_launch`]: kept only when the bytes answer for the
/// death, given back otherwise. The unexplained lane's budget
/// (`app_native::UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS`) is safe only because such
/// failures forgive their launch; `MAX_BOOT_ATTEMPTS` is 3.
///
/// Settled before the disk facts, which carry the sentinel's count
/// ([`publish_warranted_handoff_failure`]). A death reported with no pid is settled
/// by the name the successor stamped (`handoff_rendezvous::stamp_this_launch`).
///
/// `held` names a successor that DID dial (its kernel-attested pid) and left while
/// the code gate held it for a check `trustd` did not answer
/// ([`LaunchedDialEnd::ExitedWhileHeld`]): filed the same way, by its own
/// evidence, since no refusal of ours ended it, but in words that say it dialled.
#[cfg(target_os = "macos")]
fn send_pre_dial_death(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    nonce: String,
    death: PreDialDeath,
    held: Option<u32>,
    since_launch: std::time::Duration,
    launch_stamp: &std::path::Path,
) {
    let evidence = death.evidence();
    let seen = pre_dial_death_seen(death);
    let what = held.map_or_else(
        || "the launched successor exited before it dialled".to_string(),
        |pid| {
            format!(
                "the launched successor dialled (pid {pid}) and exited while its code check \
                 waited on macOS's code-signing service"
            )
        },
    );
    aterm_log::warn!(
        "update apply: {what} — {seen} ({evidence:?}), {} ms after the launch; rolling back \
         with every reader live instead of waiting out the dial budget",
        since_launch.as_millis()
    );
    let published = publish_warranted_handoff_failure(
        HandoffRollbackWarrant::NeverTransferred,
        &job.cleanup,
        proxy,
        crate::UpdateHandoffCompletion::failure(
            job.attempt_id,
            Some(nonce),
            // The named pid goes in the sentence, not the completion: nothing
            // downstream may act on a process no dial ever attested.
            None,
            crate::UpdateHandoffOutcome::ChildDied,
            format!("{what}: {seen}"),
        )
        .with_child_death(evidence),
    );
    if forgives_the_counted_trial_launch(evidence, job.current_build, job.target_build) {
        match death {
            PreDialDeath::NamedProcessGone { candidate, .. } => {
                forgive_candidate_trial_launch(job, candidate);
            }
            // No pid was named, but the successor stamped its own name before its
            // boot apply could count a launch: give back exactly that one, if any.
            PreDialDeath::TerminatedBeforeCheckIn => {
                aterm_update::forgive_stamped_trial_launch(job.target_build, launch_stamp);
            }
        }
    }
    if published {
        send_post_failure_reconcile_facts(&job.cleanup, proxy, job.current_build);
    }
}

/// What the dial wait saw of a launched successor's death, in words.
#[cfg(target_os = "macos")]
fn pre_dial_death_seen(death: PreDialDeath) -> String {
    match death {
        PreDialDeath::TerminatedBeforeCheckIn => {
            "LaunchServices reports the application terminated before it checked in".to_string()
        }
        PreDialDeath::NamedProcessGone { candidate, .. } => {
            format!(
                "the process LaunchServices named (pid {}) is gone",
                candidate.pid
            )
        }
    }
}

/// The outcome a code-gate refusal of the launched successor is filed under
/// ([`code_refused_completion`]).
///
/// * A check that did not happen ([`crate::handoff_rendezvous::CodeRefusal::TrustdSilent`],
///   [`crate::handoff_rendezvous::CodeRefusal::Unchecked`]) is this process's
///   moment: `ProducerFailed`, transient. A `trustd` that did not answer used to end
///   as the successor's own passing refusal (its boot swap's verification ran out of
///   time, it exited 75), and a lookup that failed because a sibling launch swapped
///   the same build in under the successor (round eight, #75) is the moment that
///   successor would have reported as its 75.
/// * A verdict ([`crate::handoff_rendezvous::CodeRefusal::Refused`]): the launched
///   process ran code the candidate's requirement does not admit, so it was not
///   running the candidate, whose bytes met that requirement — the Developer ID
///   by the candidate's check, the identifier by [`launched_dialer_code`]'s read —
///   before the launch. `Rejected`, as the corroboration files a dialer that is
///   not the launched successor: transient, and no verdict on the candidate.
#[cfg(target_os = "macos")]
fn code_refusal_outcome(
    refusal: &crate::handoff_rendezvous::CodeRefusal,
) -> crate::UpdateHandoffOutcome {
    if refusal.is_verdict() {
        crate::UpdateHandoffOutcome::Rejected
    } else {
        crate::UpdateHandoffOutcome::ProducerFailed
    }
}

/// The completion of an attempt whose launched successor the code gate turned
/// away and which then exited by its own rule ([`LaunchedDialEnd::CodeRefused`]):
/// the refusal's own outcome ([`code_refusal_outcome`]) and its own words, with
/// what the dial wait saw of the exit after them. Never `ChildDied`: the exit is
/// the refusal's consequence, not a death to explain.
#[cfg(target_os = "macos")]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "native_update_rendezvous_admission",
        action = "FileByCause",
        project = "aterm_gui::app_update_handoff::launched_dialer_admission_conformance::project"
    )
)]
fn code_refused_completion(
    attempt_id: u64,
    nonce: String,
    dialer_pid: u32,
    refusal: &crate::handoff_rendezvous::CodeRefusal,
    death: PreDialDeath,
) -> crate::UpdateHandoffCompletion {
    crate::UpdateHandoffCompletion::failure(
        attempt_id,
        Some(nonce),
        // The pid goes in the sentence, not the completion: no descriptor reached
        // it, and nothing downstream acts on a process that holds nothing.
        None,
        code_refusal_outcome(refusal),
        format!(
            "the launched successor dialled (pid {dialer_pid}) and was not admitted: {refusal}; \
             it then left by its own rule ({})",
            pre_dial_death_seen(death)
        ),
    )
}

/// The rollback of a launched successor the code gate turned away
/// ([`LaunchedDialEnd::CodeRefused`]), filed by [`code_refused_completion`].
///
/// Rolled back like the dial deadline (`NeverTransferred`: nothing was accepted,
/// no descriptor left). The successor gave its counted trial launch back as it
/// left (`forgive_own_trial_launch`, the failed-claim exit); the refusal is no
/// evidence against the candidate's bytes, so the same launch is given back here
/// by the dialer's name too, for a successor that could not.
#[cfg(target_os = "macos")]
fn send_code_refused_dial(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    nonce: String,
    dialer: HandoffCandidate,
    refusal: &crate::handoff_rendezvous::CodeRefusal,
    death: PreDialDeath,
) {
    let completion = code_refused_completion(job.attempt_id, nonce, dialer.pid, refusal, death);
    aterm_log::warn!(
        "update apply: {}; rolling back with every reader live ({:?}, retried on the transient \
         schedule)",
        completion.detail,
        completion.outcome
    );
    let published = publish_warranted_handoff_failure(
        HandoffRollbackWarrant::NeverTransferred,
        &job.cleanup,
        proxy,
        completion,
    );
    if forgives_the_counted_trial_launch(
        crate::ChildDeathEvidence::Unobserved,
        job.current_build,
        job.target_build,
    ) {
        forgive_candidate_trial_launch(job, dialer);
    }
    if published {
        send_post_failure_reconcile_facts(&job.cleanup, proxy, job.current_build);
    }
}

/// LaunchServices' pending answer to the launch of the dialer already accepted on
/// the kernel-attested rendezvous. Polled once right after the accept, then with a
/// zero budget on every slice of the hold and of [`wait_handoff_ready`], so it never
/// holds the parked terminal; a contradiction still refuses before Commit. After the
/// grant the descriptors are the peer's, so a refusal there is a kill-and-reap,
/// never `NeverTransferred`.
///
/// Two answers contradict the dialer: a pid other than the dialer's (a stranger
/// that knew the claim secret), and "the application terminated before it checked
/// in" while the dialer lives (round eight of the update audit, U36). That answer
/// is LaunchServices' positive word that the process it launched is DEAD
/// ([`crate::app_launch_successor::launch_answer_proves_exit`]), so a live dialer
/// cannot be it; until 2026-10-02 it was read, like any error, as corroborating the
/// attested dialer, and a stranger that killed the real successor was granted
/// every master on the strength of the successor's death.
#[cfg(target_os = "macos")]
struct LaunchCorroboration<'a> {
    in_flight: &'a crate::app_launch_successor::LaunchInFlight,
    dialer_pid: i32,
    /// The dialer as the kernel attested it at the accept (pid and birth stamp):
    /// asked whether it still lives when LaunchServices says the process it
    /// launched has exited.
    dialer: HandoffCandidate,
    /// Set once LaunchServices has answered (a match, or an error about a job that
    /// evidently ran), so the answer is never polled for twice.
    answered: bool,
}

#[cfg(target_os = "macos")]
impl<'a> LaunchCorroboration<'a> {
    /// The corroboration of the dialer the kernel attested as `dialer`.
    fn of_dialer(
        in_flight: &'a crate::app_launch_successor::LaunchInFlight,
        dialer: HandoffCandidate,
    ) -> Self {
        Self {
            in_flight,
            dialer_pid: i32::try_from(dialer.pid).unwrap_or(-1),
            dialer,
            answered: false,
        }
    }
}

#[cfg(target_os = "macos")]
impl LaunchCorroboration<'_> {
    /// One zero-budget poll. `Err` means the handoff must be refused, and says why:
    /// the launch answer names a pid other than the dialer's, or says the launched
    /// process exited while the dialer lives. The only reader that takes
    /// LaunchServices' answer
    /// ([`crate::app_launch_successor::LaunchInFlight::wait`]);
    /// [`await_launched_dial`] only peeks.
    ///
    /// A dialer that is gone when LaunchServices reports the exit WAS the launched
    /// process (or is gone either way): that is no contradiction, and the death
    /// checks beside this poll name it (`ChildDied`). "Lives" is the kernel's
    /// birth stamp for the pid still matching the one taken at the accept, which a
    /// zombie does not answer (libproc does not report one), so a successor that
    /// died and is not yet reaped reads as gone.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "native_update_launched_dial_wait",
            action = "Corroborate",
            project = "aterm_gui::app_update_handoff::launched_dial_wait_conformance::project"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "native_update_rendezvous_admission",
            action = "Commit",
            project = "aterm_gui::app_update_handoff::launched_dialer_admission_conformance::project"
        )
    )]
    fn poll(&mut self) -> Result<(), String> {
        if self.answered {
            return Ok(());
        }
        match self.in_flight.wait(std::time::Duration::ZERO) {
            Ok(successor) if successor.pid() == self.dialer_pid => {
                self.answered = true;
                aterm_log::info!(
                    "update apply: LaunchServices corroborates the dialer as pid {}",
                    self.dialer_pid
                );
                Ok(())
            }
            Ok(successor) => {
                self.answered = true;
                let detail = format!(
                    "the dialer (pid {}) is not the successor LaunchServices launched (pid {}); \
                     refusing the handoff",
                    self.dialer_pid,
                    successor.pid()
                );
                aterm_log::warn!("update apply: {detail}");
                Err(detail)
            }
            Err(crate::app_launch_successor::LaunchError::Timeout(_)) => Ok(()),
            Err(crate::app_launch_successor::LaunchError::ExitedBeforeCheckIn) => {
                self.answered = true;
                if self.dialer.identity() == HandoffCandidateIdentity::Corroborated {
                    let detail = format!(
                        "LaunchServices reports the process it launched exited before it checked \
                         in, and the dialer (pid {}) is alive, so it is not the launched \
                         successor; refusing the handoff",
                        self.dialer_pid
                    );
                    aterm_log::warn!("update apply: {detail}");
                    Err(detail)
                } else {
                    aterm_log::info!(
                        "update apply: LaunchServices reports the launched process exited before \
                         it checked in, and the dialer (pid {}) is gone too: its death ends the \
                         attempt",
                        self.dialer_pid
                    );
                    Ok(())
                }
            }
            Err(error) => {
                self.answered = true;
                aterm_log::warn!(
                    "update apply: LaunchServices answered the launch with an error after the \
                     successor had already dialled ({error}); proceeding on the attested dialer"
                );
                Ok(())
            }
        }
    }
}

#[cfg(unix)]
fn wait_handoff_ready(
    rd: &std::os::fd::OwnedFd,
    expected: crate::seamless::AdoptionProof,
    cancel: &std::sync::mpsc::Receiver<()>,
    masters: &[i32],
    deadline: std::time::Instant,
    candidate_gone: &mut dyn FnMut() -> bool,
    #[cfg(target_os = "macos")] mut corroboration: Option<LaunchCorroboration<'_>>,
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
    // Masters are watched for death only. POLLIN must be requested (macOS evaluates
    // a PTY's state only for requested events and reports a dead slave as
    // POLLIN|POLLHUP), but plain POLLIN in the answer is ignored: queued output
    // waits in the kernel for the child.
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
        #[cfg(target_os = "macos")]
        if let Some(corroboration) = corroboration.as_mut()
            && corroboration.poll().is_err()
        {
            return crate::UpdateHandoffOutcome::Rejected;
        }
        // The candidate's own end, asked every slice rather than inferred from EOF:
        // a stray write-end copy can hold EOF back
        // ([`HandoffCandidateHandle::candidate_gone`]). Asked after the
        // corroboration, so a dialer LaunchServices disowns is still `Rejected`.
        if candidate_gone() {
            return crate::UpdateHandoffOutcome::ChildDied;
        }
        for pfd in &mut pollfds {
            pfd.revents = 0;
        }
        // SAFETY: a stable initialized pollfd slice; 10 ms bounds death abort.
        let n = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, 10) };
        if n <= 0 {
            continue; // timeout slice or EINTR — re-check the candidate + deadline (poll already blocked)
        }
        match classify_ready_poll(&pollfds) {
            // A handed session ended: the desk changed, as the park files it
            // (`handoff_rejection_activity_shaped`).
            ReadyPollAction::SessionDied => return crate::UpdateHandoffOutcome::ActivityRevoked,
            ReadyPollAction::NoProgress => {
                // A booting child's shell can queue output on a handed master, which
                // answers plain POLLIN and makes `poll` return immediately without
                // matching the dead mask or the proof fd. Without this 2 ms yield the
                // loop would busy-spin and starve the child it waits on.
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
                // Recheck after the complete proof. A child that raced death with
                // the final bytes is not an exit authority.
                if cancel.try_recv().is_ok() {
                    return crate::UpdateHandoffOutcome::ActivityRevoked;
                }
                return crate::UpdateHandoffOutcome::ProofReady;
            }
            // EOF: every copy of the write end is closed, the child's included, so
            // it dropped the fd (a failed validation) or died/malformed mid-proof.
            0 => return crate::UpdateHandoffOutcome::ChildDied,
            _ => continue, // EINTR/EAGAIN — re-loop
        }
    }
}

/// The late park's stand-down against real processes and the real rendezvous: close
/// the rendezvous (EOF), give the grace, kill if needed, prove it gone either way,
/// and report how it died, which decides whether the counted trial launch is given
/// back.
#[cfg(all(test, target_os = "macos"))]
#[path = "app_update_handoff_held_successor_stand_down_tests.rs"]
mod held_successor_stand_down_tests;

/// The late park's attempt records and the completion reducer.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_late_park_record_tests.rs"]
mod late_park_record_tests;

/// The worker's own publication of a pass for `attempt`
/// ([`PreverifyPublisher::for_attempt`]), so a test seeds the cache as the shipping
/// lane does. Returns the policy published, narrowed to this build.
#[cfg(all(test, unix))]
impl App {
    pub(crate) fn publish_preverified_pass_for_test(
        &self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
        read: &aterm_update_core::handoff_policy::PolicyRead,
    ) -> Option<aterm_update_core::handoff_policy::HandoffPolicy> {
        PreverifyPublisher::for_attempt(&self.handoff_preverified, Some(attempt))
            .expect("an attempt names its artifact")
            .publish_policy_pass(read, crate::running_build_number(), "the test candidate")
    }
}

/// The worker's own pre-verification reaches the park: an attempt past the
/// arm-time verdict's freshness re-verifies on the worker, and its pass and
/// handoff policy are published under the attempt's artifact key, where the
/// launched lane's park reads them.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_preverify_publisher_tests.rs"]
mod preverify_publisher_tests;

/// The park gate, enumerated: freezing the terminal is decided separately from
/// starting the update, by the ladder's rule for the live phase plus the one fact
/// (unconsumed master bytes) it relaxes only at its bound.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_park_gate_tests.rs"]
mod park_gate_tests;

/// Driven through the shipping capture and gate with real PTY masters: a parser
/// caught mid-sequence over queued output is a timing miss, never a carry whose
/// successor prints the tail as text; and an exited `--hold` pane is not handed,
/// so its hung-up master can neither hold the park nor fail it.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_handed_set_and_mid_sequence_tests.rs"]
mod handed_set_and_mid_sequence_tests;

#[cfg(all(test, target_os = "macos"))]
mod park_transfer_split_tests {
    use super::{ParkTransferSplit, park_transfer_split_text};
    use std::time::{Duration, Instant};

    /// The five slices are consecutive, so they sum to park->transfer; a pair
    /// out of order saturates to zero instead of wrapping (the negative control).
    #[test]
    fn the_split_names_five_consecutive_slices() {
        let park_at = Instant::now();
        let at = |us: u64| park_at + Duration::from_micros(us);
        let split = ParkTransferSplit {
            park_at,
            captured_at: at(600),
            picked_up_at: at(700),
            artifacts_at: at(21_000),
            sendmsg_at: at(21_400),
            granted_at: at(22_300),
        };
        assert_eq!(
            park_transfer_split_text(split),
            "capture 0.6 + hand-over 0.1 + artifacts 20.3 + pre-grant 0.4 + sendmsg 0.9 ms"
        );
        let reversed = ParkTransferSplit {
            picked_up_at: park_at,
            ..split
        };
        assert!(
            park_transfer_split_text(reversed).contains("hand-over 0.0 + artifacts 21.0"),
            "{}",
            park_transfer_split_text(reversed)
        );
    }
}

#[cfg(test)]
#[path = "app_update_handoff_freeze_budget_tests.rs"]
mod freeze_budget_tests;

/// The dry run of the park's capture, over the shipping capture and handed set:
/// what it measures, what it saturates on, what it cannot measure, and that it
/// parks nothing.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_dry_run_capture_tests.rs"]
mod dry_run_capture_tests;

#[cfg(all(test, unix))]
mod capture_budget_reservation_tests {
    use super::{MAX_HANDOFF_CAPTURE_BUDGET_BYTES, optional_carry_fits};
    use crate::seamless::{
        admit_checkpoint_dimensions, checkpoint_capture_budget_bytes, mandatory_checkpoint_cells,
        max_handoff_aggregate_grid_cells, max_handoff_history_lines,
    };

    /// Walks a pool of `sessions` identical panes through the producer's
    /// reserve-then-history rule, charging the real admission seam and budgets
    /// `start_unix_update_handoff` charges. The per-session degrade rung is not
    /// replicated, so `Ok` is the stronger claim. `Err(index)` is the session the
    /// capture would refuse.
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
            let Ok(per_grid) =
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

    /// Twelve, twenty-four and sixty-four panes, at a typical and at a maximized
    /// geometry, must all be admitted in full: reserving every visible screen first
    /// means early scrollback cannot starve a later mandatory screen. Carrying less
    /// scrollback is allowed; refusing the update is not.
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
    /// only if the pool's mandatory visible+alt total does not fit. The count is
    /// derived from the constant. `Err(fits)` pins that the refusal lands on the
    /// first session that genuinely does not fit, not an early one whose budget was
    /// spent on scrollback.
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

    /// The per-grid cell cap admits every window the GUI can produce (a 5K display
    /// in full screen at the default font is 33,462 cells).
    ///
    /// Geometry comes from the GUI's own law ([`crate::App::grid_dims_for`]) on a
    /// headless App with real chrome (pad 24, pad_top 4, head 80 at scale 2), for
    /// four real Mac displays, full screen and maximized (37 pt notched menu bar,
    /// 70 pt Dock at 2x), at every font size from `FONT_PX_MIN` to 48 px. A pool is
    /// that window split into 1..=8 side-by-side panes, admitted as the capture
    /// admits a visible carry (`history = 0`, alt grid reserved).
    #[test]
    fn every_full_screen_display_geometry_is_admitted_for_a_pool_of_panes() {
        use winit::dpi::PhysicalSize;

        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        {
            let ws = app.windows.get_mut(&wid).expect("the headless window");
            ws.metrics.pad = 24;
            ws.metrics.pad_top = 4;
            ws.metrics.head = 80;
        }
        let mut over_the_old_cap = Vec::new();
        for (width, height) in [
            (3024_u32, 1964_u32),
            (3456, 2234),
            (5120, 2880),
            (6016, 3384),
        ] {
            for (shape, size) in [
                ("full screen", PhysicalSize::new(width, height)),
                ("maximized", PhysicalSize::new(width, height - 74 - 140)),
            ] {
                // FONT_PX_MIN is a whole number of pixels (6).
                let smallest = crate::FONT_PX_MIN as u32;
                for font_px in smallest..=48 {
                    app.windows
                        .get_mut(&wid)
                        .expect("the headless window")
                        .metrics
                        .font_px = font_px as f32;
                    let (rows, cols) = app.grid_dims_for(wid, size);
                    let cells = u64::from(rows) * u64::from(cols);
                    if cells > 32 * 1024 {
                        over_the_old_cap.push((width, height, shape, font_px, rows, cols));
                    }
                    for panes in 1..=8_u16 {
                        let pane_cols = (cols / panes).max(1);
                        let mut used = 0_u64;
                        for pane in 0..panes {
                            assert!(
                                admit_checkpoint_dimensions(&mut used, rows, pane_cols, 0, true)
                                    .is_ok(),
                                "{width}x{height} {shape} at font {font_px} px ({rows}x{cols}): \
                                 pane {pane} of {panes} ({rows}x{pane_cols}) was refused"
                            );
                        }
                    }
                }
            }
        }
        assert!(
            !over_the_old_cap.is_empty(),
            "PRECONDITION: the table must reach past the old 32 Ki per-grid cap, or it \
             could not have caught the incident"
        );
    }
}

#[cfg(all(test, unix))]
#[path = "app_update_handoff_commit_layout_topology_tests.rs"]
mod commit_layout_topology_tests;

#[cfg(all(test, unix))]
mod pinnedness_tests {
    use super::candidate_is_our_child;

    /// The precondition for a group kill: `kill(-pid)` is sound only while the pid is
    /// pinned to our candidate. A fork child pins it; a launchd-owned successor does
    /// not, so the reaper asks the kernel. Pins both answers: a live child of ours is
    /// pinned, and a process that is not our child (this process, standing in for a
    /// launchd-owned successor) is not.
    #[test]
    fn only_a_child_of_ours_pins_its_pid() {
        let mut child = aterm_uds::spawnfd::command("/bin/sleep")
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
#[path = "app_update_handoff_candidate_death_evidence_tests.rs"]
mod candidate_death_evidence_tests;

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

    /// A `ChildDied` whose launch stays counted spends the counter the boot sentinel
    /// reverts on; retried more than `MAX_BOOT_ATTEMPTS` times it would revert a
    /// healthy bundle and mark the build failed. So forgiveness follows the shape,
    /// and only the shape that converges under `MAX_BOOT_ATTEMPTS` keeps its count.
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

    /// The real-apply guard: with no strictly newer authorized target nothing armed a
    /// sentinel, so nothing is given back whatever the death.
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

    /// The invariant that makes the table above safe, checked against the real
    /// constant; mirrors the compile-time assert beside
    /// `STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS`.
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

/// The non-parent witness against real processes reparented to launchd, the shape
/// of a LaunchServices-launched successor.
#[cfg(all(test, target_os = "macos"))]
#[path = "app_update_handoff_candidate_exit_watch_tests.rs"]
mod candidate_exit_watch_tests;

#[cfg(all(test, unix))]
#[path = "app_update_handoff_handoff_process_group_tests.rs"]
mod handoff_process_group_tests;

/// The returned-completion reducer's one remaining piece of attempt identity: the
/// [`crate::native_updater_service::ApplyMode`] the apply was authorized under.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_returned_handoff_completion_lane_tests.rs"]
mod returned_handoff_completion_lane_tests;

/// The lane choice and every reason it falls back: whether the next update's
/// successor gets its own launchd application job or is another pid-1 orphan.
/// Each row flips one field of [`HandoffLaneFacts`] against an otherwise eligible
/// attempt, so a predicate ignoring a field fails.
#[cfg(all(test, target_os = "macos"))]
#[path = "app_update_handoff_handoff_lane_tests.rs"]
mod handoff_lane_tests;

/// Tier-1 conformance for the seamless handoff's per-session ownership model
/// ([`aterm_spec::derive::native_update_seamless_handoff_ownership_model`]): the
/// real decision functions of this module are driven over their whole bounded
/// input space and must decide exactly what the model admits.
///
/// A child module so it sees the parent's private items without widening them.
///
/// Bound: the final Commit admission, which decides whether an attempt may transfer
/// ownership. Modelled but not bound: the physical surface it gates (the transfer
/// and `_exit` in `seamless::commit_and_exit`, reader park/resume, the
/// `F_DUPFD_CLOEXEC` duplication), which replaces this process or needs a live event
/// loop; the QA-seam tests cover it.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_ownership_conformance_tests.rs"]
mod ownership_conformance;

/// The live font zoom crosses the handoff.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_font_zoom_carry_tests.rs"]
mod font_zoom_carry_tests;

/// The rows the park's carry did not take end with the parent at Commit.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_uncarried_messages_tests.rs"]
mod uncarried_messages_tests;

/// The presence row crosses the handoff inside its window.
#[cfg(all(test, unix))]
mod presence_row_carry_tests {
    use crate::app_window::CarriedFrame;
    use crate::session_store::SessionHandoff;
    use crate::{App, WindowId};
    use winit::dpi::PhysicalSize;

    /// Windows 0 and 1 each show a presence row (an agent's status line under the
    /// chrome) that takes a grid row inside the window. The update carries window 0's
    /// frame in the window carry and window 1's in its layout row. The successor's
    /// rows are frozen until Commit and it takes each carried frame before the first
    /// attach, so every window comes up at its parent's size and grid, and the
    /// Commit's lift re-grids no window. The durable layout carries no row.
    #[test]
    fn every_window_with_a_presence_row_crosses_at_its_parents_size_and_grid() {
        let (w0, w1) = (WindowId(0), WindowId(1));
        let grid = |app: &App, wid: WindowId| (app.windows[&wid].rows, app.windows[&wid].cols);
        // A real window's chrome re-grid: the grid re-fitted inside its size.
        let size_to = |app: &mut App, wid: WindowId, size: PhysicalSize<u32>| {
            app.windows.get_mut(&wid).expect("a window").win_px = Some(size);
            app.on_resize(wid, size);
        };

        // The parent: window 0 at 29x100, window 1 at 40x120, then a row is
        // born in each, inside the window it has.
        let mut parent = App::headless_for_test();
        assert_eq!(
            parent.insert_logical_window(crate::stub_session(1), 40, 120),
            w1
        );
        let desk = [
            (w0, parent.window_frame_px_for(w0, 29, 100)),
            (w1, parent.window_frame_px_for(w1, 40, 120)),
        ];
        for (wid, size) in desk {
            parent
                .windows
                .get_mut(&wid)
                .expect("a window")
                .presence
                .rows = 1;
            size_to(&mut parent, wid, size);
        }
        let parent_grids = [(28, 100), (39, 120)];
        assert_eq!(
            [grid(&parent, w0), grid(&parent, w1)],
            parent_grids,
            "PRECONDITION: each row took a grid row"
        );

        let wire = SessionHandoff {
            schema: SessionHandoff::SCHEMA,
            sessions: Vec::new(),
            window: parent.handoff_window_carry(0),
            connections: Vec::new(),
            next_turn_id: None,
            outgoing_build: None,
            held: Vec::new(),
            roster_seq: None,
            fabric_attached: Vec::new(),
        }
        .to_toml()
        .expect("serializes");
        let carry = SessionHandoff::from_toml(&wire)
            .expect("the successor reads it")
            .window
            .expect("the window carry");
        let layout = crate::restore::RestoreManifest::from_toml(
            &parent
                .capture_handoff_layout()
                .to_toml()
                .expect("the parent serializes its layout"),
        )
        .expect("the successor parses it");

        // The successor: window 0 at the carry's grid (`main_entry`), window 1 from
        // its row (`create_window_internal_connected`); each takes its frame, then its
        // first attach sizes it (`attach_os_window`).
        let mut successor = App::headless_for_test();
        // Its rows frozen until Commit, as `main_entry` starts an adopter.
        successor.incoming_handoff_pending = true;
        successor
            .windows
            .get_mut(&w0)
            .expect("window 0")
            .take_carried_frame(
                carry
                    .inner_px
                    .map(|[width, height]| PhysicalSize::new(width, height)),
                carry.presence_row,
            );
        let row = CarriedFrame::of(&layout.windows[1]);
        let ((rows, cols), inner) = successor.new_window_frame(row);
        assert_eq!(
            successor.insert_logical_window(crate::stub_session(1), rows, cols),
            w1
        );
        successor
            .windows
            .get_mut(&w1)
            .expect("window 1")
            .take_carried_frame(inner, row.is_some_and(|row| row.presence_row));
        for ((wid, parent_size), (rows, cols)) in desk.into_iter().zip(parent_grids) {
            let carried = successor
                .windows
                .get_mut(&wid)
                .and_then(|ws| ws.carried_inner_px.take());
            let frame = successor.attach_frame_px(wid, rows, cols, carried);
            assert_eq!(
                frame, parent_size,
                "window {} comes up at its parent's size",
                wid.0
            );
            size_to(&mut successor, wid, frame);
            assert_eq!(
                grid(&successor, wid),
                (rows, cols),
                "window {} holds its parent's grid",
                wid.0
            );
        }

        // COMMIT: each window's story still wants its row (a settling turn's
        // lease, sensed under the freeze), and the real lift commits it.
        for session in [0, 1] {
            *successor
                .pool
                .get(session)
                .expect("a session")
                .ctx
                .turn_lease
                .lock()
                .unwrap() = Some(crate::Lease::Turn {
                id: 7,
                driver: None,
                typing: false,
            });
            successor.refresh_presence_session(session, false);
        }
        let before = crate::app_presence::presence_regrids();
        successor.incoming_handoff_pending = false;
        successor.release_successor_commit_freeze();
        assert_eq!(
            crate::app_presence::presence_regrids(),
            before,
            "the Commit re-grids no window: the row it commits is the one reserved"
        );

        assert!(
            !parent.capture_restore_manifest().windows[1].presence_row,
            "the durable layout carries no row: a cold restore judges its rows afresh"
        );
    }
}

/// Round eight, finding 52: the held capture applies the manifest writer's own
/// bounds, so screens the writer would drop (past the link-strip bound) spend
/// none of the held budget, and a later pane that fits is carried.
#[cfg(all(test, unix))]
mod held_capture_bound_tests {
    use std::sync::Arc;

    fn linked(rows: u16) -> aterm_core::terminal::Terminal {
        let mut t = aterm_core::terminal::Terminal::new(rows, 200);
        let url_tail = "a".repeat(2090);
        for row in 0..rows {
            let mut line = Vec::new();
            for link in 0..45 {
                line.extend_from_slice(
                    format!("\x1b]8;;https://e/{row}-{link}-{url_tail}\x07X\x1b]8;;\x07")
                        .as_bytes(),
                );
            }
            if row + 1 < rows {
                line.extend_from_slice(b"\r\n");
            }
            t.process(&line);
        }
        t
    }

    #[test]
    fn a_held_screen_the_writer_refuses_spends_none_of_the_held_budget() {
        // PRECONDITION: on the old capture the three dense screens fit the held
        // budget and crowded the fourth out of it.
        let grid = |rows| {
            u64::try_from(
                linked(rows)
                    .checkpoint_carry_abandoning_partial(0)
                    .0
                    .grid
                    .len(),
            )
            .expect("a length")
        };
        assert!(
            3 * grid(50) <= crate::seamless::MAX_HELD_AGGREGATE_GRID_BYTES
                && 3 * grid(50) + grid(30) > crate::seamless::MAX_HELD_AGGREGATE_GRID_BYTES,
            "{} {}",
            grid(50),
            grid(30)
        );
        let mut app = crate::App::headless_for_test();
        let mut ids = Vec::new();
        for rows in [50, 50, 50, 30] {
            let id = app.next_session_id;
            let session = crate::ptyless_session(
                id,
                aterm_session::SessionId::generate(),
                linked(rows),
                Arc::new(crate::SinkWriter::new(-1)),
            );
            app.push_stub_tab(crate::WindowId(0), session);
            ids.push(id);
        }
        let (dense, fits) = (&ids[..3], ids[3]);
        let held = app.capture_held_screens(
            &[],
            std::time::Instant::now() + std::time::Duration::from_secs(60),
            &mut 0,
            crate::seamless::WireCaps::current(),
            aterm_update_core::handoff_policy::CarryCeiling::Full,
            &mut Vec::new(),
        );
        let captured: Vec<u64> = held.iter().map(|screen| screen.local_id).collect();
        assert!(
            captured.contains(&fits),
            "the pane that fits is carried: {captured:?}"
        );
        assert!(
            !captured.iter().any(|id| dense.contains(id)),
            "screens the writer refuses are not captured: {captured:?}"
        );
    }
}

/// Round five, item 16: a `video` take running when an update parks is
/// answered at the park, never dropped with the connection at Commit.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_video_park_tests.rs"]
mod video_park_tests;

/// The launched successor is offered the grant the lane was chosen on.
#[cfg(all(test, target_os = "macos"))]
#[path = "app_update_handoff_launched_lane_grant_tests.rs"]
mod launched_lane_grant_tests;

/// A successor may resize a carried PTY before Commit, and a rejected Commit hands
/// the program back to this process, whose engine is still at the park's size.
/// `resize_panes` skips panes whose engine already matches, so the size must be
/// re-asserted.
#[cfg(all(test, unix))]
mod rollback_winsize_tests {
    use crate::{App, term_lock};

    fn winsize_of(fd: i32) -> (u16, u16) {
        // SAFETY: `winsize` is four integers; zeroed is valid, and
        // `TIOCGWINSZ` fills it for an open pty descriptor.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: as above; `fd` is open for the whole test.
        assert_eq!(unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) }, 0);
        (ws.ws_row, ws.ws_col)
    }

    #[test]
    fn a_rolled_back_commit_puts_each_parked_ptys_own_size_back() {
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        let mut app = App::headless_for_test();
        let session = &mut app.pool.sessions.get_mut(&0).expect("session 0").session;
        session.master = master;
        let parked = {
            let t = term_lock(&session.term);
            (t.rows(), t.cols())
        };
        aterm_pty::resize(master, parked.0, parked.1);
        // The successor's pulse, at a grid that is not this pane's.
        aterm_pty::resize(master, parked.0 + 6, parked.1 + 20);
        assert_ne!(winsize_of(slave), parked, "the successor really moved it");
        app.rollback_overlap(None, &[(0, master, -1)]);
        assert_eq!(
            winsize_of(slave),
            parked,
            "the program resumes at the size its engine was parked at"
        );
        // SAFETY: both fds were opened above and are still open;
        // the stub session's sink names no descriptor, so nothing else closes them.
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }
}

/// The fork lane never captures above a policy it has not waited for: with no
/// verified pass on record, an attempt — automatic or a person's (round eight,
/// U35) — defers as activity while the policy read is in flight (including an
/// arm-time `codesign` past its budget), up to the read's ceiling, and the next
/// attempt parks under what the read answered, or as before if nothing is on
/// record. Negative controls: a known policy, a read that ended with nothing, no
/// read, and a spent ceiling do not defer.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_fork_lane_policy_wait_tests.rs"]
mod fork_lane_policy_wait_tests;

/// A manifest the desk's content puts over its cap is a refusal, not this
/// process's moment: `write_outgoing_with_held` refuses typed (`ManifestOverCap`)
/// once every optional part is shed, and the next attempt would capture the same
/// desk. It goes to the refusal lane, as a capture refusal does. Negative control:
/// a filesystem refusal, a missing private directory and this build's own
/// inconsistency stay this process's trouble.
#[cfg(all(test, unix))]
mod manifest_over_cap_tests {
    use super::{PreparationFailure, preparation_failure_of_write};
    use crate::seamless::WriteOutgoingFailure;

    #[test]
    fn an_over_cap_manifest_is_filed_on_the_refusal_lane() {
        let refused = preparation_failure_of_write(WriteOutgoingFailure::ManifestOverCap);
        let PreparationFailure::Refused(detail) = &refused else {
            panic!("a deterministic refusal of the desk's content");
        };
        assert!(detail.contains("records are over the"), "{detail}");
        // A person's press is answered `Failed` with this detail and arms no retry,
        // so the detail promises none.
        for promise in ["retries", "desk", "lane", "successor"] {
            assert!(!detail.contains(promise), "{promise:?} in {detail}");
        }
        for own in [
            WriteOutgoingFailure::NoPrivateDir,
            WriteOutgoingFailure::Inconsistent,
            WriteOutgoingFailure::Io(std::io::ErrorKind::StorageFull),
        ] {
            assert!(
                matches!(
                    preparation_failure_of_write(own),
                    PreparationFailure::Producer(_)
                ),
                "{own}: this process's own trouble"
            );
        }
        // The outcome the two lanes file it as reaches the refusal lane.
        assert_eq!(
            crate::app_native::HandoffFailureLane::classify(
                crate::native_updater_service::ApplyMode::Automatic,
                crate::UpdateHandoffOutcome::CaptureRefused,
                crate::ChildDeathEvidence::Unobserved,
                false,
            ),
            crate::app_native::HandoffFailureLane::Refused
        );
    }
}

/// The Commit fence holds the bridge's retirements back only for a successor that
/// reads the mailbox carry (this build or newer); a downgrade gets them answered.
#[cfg(all(test, unix))]
mod fabric_carry_successor_tests {
    use super::successor_reads_fabric_carry;

    #[test]
    fn only_a_successor_at_least_as_new_holds_retirements_back() {
        assert!(successor_reads_fabric_carry(1_790_700_000, 1_790_607_299));
        assert!(successor_reads_fabric_carry(1_790_607_299, 1_790_607_299));
        assert!(!successor_reads_fabric_carry(1_790_585_188, 1_790_607_299));
    }
}

/// The cold lane over a windowed desk with open native tabs leaves the quit layout
/// its successor's boot reopens, native leaf included, or refuses when session
/// restore is off and nothing would reopen them.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_cold_lane_layout_tests.rs"]
mod cold_lane_layout_tests;

/// Round eight, #1: a refused cold update was an endless relaunch loop. The cold
/// lane exec'd before reading any pre-park verdict, its successor is this very
/// build, and when that successor's boot swap refused (an install location a
/// standard user cannot write, a bundle that cannot be the rollback source) it came
/// up on the same terminal-less desk, re-armed and exec'd again — every few seconds.
///
/// Two halves, one per side of the `exec`: the lane reads the verdict first
/// ([`crate::App::cold_lane_verdict_gate`]), and a relaunch whose swap was refused
/// hands its automatic lane that refusal as the verdict ([`super::ColdRelaunch`]).
///
/// RED before the fix: the gate did not exist and the cold arm reached `exec`
/// with a cached refusal on record; a refused relaunch left an empty cache, so the
/// first attempt of the new process relaunched again.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_cold_lane_verdict_tests.rs"]
mod cold_lane_verdict_tests;

#[cfg(all(test, unix))]
mod fork_lane_child_tests {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::process::CommandExt as _;

    /// A copy of `/dev/null` at 64 or above, close-on-exec or not.
    fn stand_in(cloexec: bool) -> OwnedFd {
        let null = std::fs::File::open("/dev/null").expect("open /dev/null");
        let cmd = if cloexec {
            libc::F_DUPFD_CLOEXEC
        } else {
            libc::F_DUPFD
        };
        // SAFETY: F_DUPFD(_CLOEXEC) on a descriptor this test owns.
        let fd = unsafe { libc::fcntl(null.as_raw_fd(), cmd, 64) };
        assert!(fd >= 64, "a stand-in at 64 or above: {fd}");
        // SAFETY: a fresh descriptor this test exclusively owns.
        unsafe { OwnedFd::from_raw_fd(fd) }
    }

    /// What `/bin/sh` finds at `numbers`: with no `hook`, started by
    /// [`aterm_uds::spawnfd::transparent`], which hands on every descriptor this
    /// process holds without close-on-exec whatever std it links; with one, by
    /// std's fork with `hook` as its `pre_exec`, the fork lane's own shape.
    fn child_holds(numbers: &[i32], hook: Option<Vec<i32>>) -> String {
        let probe = numbers
            .iter()
            .map(|fd| format!("[ -e /dev/fd/{fd} ] && printf 'held:{fd} '; "))
            .collect::<String>();
        let Some(handed) = hook else {
            // The control is `transparent`, not a std `Command`: under Trust's
            // SP-1 std hands a `Command` child fds 0-2 alone, so a plain std
            // spawn no longer carries the stray. Its child writes on this
            // process's stdout, so the probe reports into the file `$1` names.
            let dir = aterm_tempfile::tempdir().expect("a temp dir");
            let report = dir.path().join("holds");
            let status = aterm_uds::spawnfd::transparent("/bin/sh")
                .args([
                    "-c",
                    &format!("{{ {probe}echo end; }} >\"$1\" </dev/null"),
                    "sh",
                ])
                .arg(&report)
                .status()
                .expect("run the control");
            assert!(status.success(), "{status}");
            return std::fs::read_to_string(&report)
                .expect("the control's report")
                .trim()
                .to_string();
        };
        // std's fork on purpose: the hook is a `pre_exec`, as the fork lane's is.
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", &format!("{probe}echo end")]);
        let strip = aterm_uds::spawnfd::InheritStrip::prepare();
        // SAFETY: the hook runs between fork and exec; see the function.
        unsafe {
            cmd.pre_exec(move || super::fork_lane_child_holds(strip, &handed));
        }
        let out = cmd.output().expect("run the stand-in successor");
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// **THE FORK-LANE SUCCESSOR HOLDS WHAT IT IS HANDED AND NOTHING ELSE**
    /// (2026-09-30 review, F1). A descriptor this process holds without
    /// close-on-exec rides a spawn that hands on the whole table (the control)
    /// and not the lane's hook, while a handed one — close-on-exec here, as the
    /// masters and the proof pipe are — reaches the successor.
    #[test]
    fn the_fork_lane_successor_holds_what_it_is_handed_and_nothing_else() {
        let stray = stand_in(false);
        let handed = stand_in(true);
        let (s, h) = (stray.as_raw_fd(), handed.as_raw_fd());
        assert_eq!(
            child_holds(&[s, h], None),
            format!("held:{s} end"),
            "CONTROL: the transparent spawn carries the stray and not the flagged one"
        );
        assert_eq!(
            child_holds(&[s, h], Some(vec![h])),
            format!("held:{h} end"),
            "the successor holds the handed descriptor and not the stray"
        );
        drop((stray, handed));
    }

    /// **THE COLD LANE'S `exec` HANDS OVER NOTHING THIS PROCESS HOLDS** (2026-09-30
    /// review, F1). In a helper re-exec of this binary, a descriptor without
    /// close-on-exec is held when the helper replaces itself with a probe:
    /// through a plain `exec` (the control) the probe holds it, through
    /// [`super::cold_exec_hand_over`] it does not. The helper's exit IS the
    /// probe's, so it is judged by that status — once libtest is seen to have
    /// selected exactly this test ([`crate::test_alone::Running::selected`]),
    /// never by a status a name that selects nothing also exits with.
    #[test]
    fn the_cold_lane_exec_hands_over_nothing_this_process_holds() {
        const STAGE: &str = "ATERM_TEST_COLD_EXEC_HAND_OVER";
        if let Ok(stage) = std::env::var(STAGE) {
            let stray = stand_in(false);
            let fd = stray.as_raw_fd();
            let _lifeline = (stage == "hand-over").then(super::cold_exec_hand_over);
            // std on purpose: the control is std's `exec` carrying the stray descriptor.
            let error = std::process::Command::new("/bin/sh")
                .args(["-c", &format!("[ -e /dev/fd/{fd} ] && exit 3; exit 0")])
                .exec();
            panic!("the probe did not replace this helper: {error}");
        }
        let probe = |stage: &str| {
            let mut helper = crate::test_alone::StandIn::this_test();
            helper.command().env(STAGE, stage);
            helper
                .spawn()
                .selected()
                .status
                .and_then(|status| status.code())
        };
        assert_eq!(
            probe("plain"),
            Some(3),
            "CONTROL: a plain exec hands the probe the stray"
        );
        assert_eq!(
            probe("hand-over"),
            Some(0),
            "the cold lane's exec handed the probe the stray"
        );
    }
}

/// THE CONNECTIONS A MANIFEST LEFT OUT ARE AUDITED AS DROPPED AT THE COMMIT, AND
/// ONLY THERE (round eight of the update audit, U23's review). The writer left
/// them out to fit its cap and audited each as a `session_edge` drop as it
/// wrote, before the attempt's outcome was known: an attempt that stood down
/// (a proof mismatch, activity, a successor that died, a rollback) left this
/// process holding every one of them, with their dissolution on record, and
/// each retry wrote the same burst again. Now the writer returns them, the
/// worker hands them to the attempt's arbiter, and the Commit audits each once,
/// right before its write.
#[cfg(all(test, unix))]
mod connection_drop_audit_tests {
    use crate::session_store::ConnectionCarry;

    fn row(src: &str, op: &str) -> ConnectionCarry {
        ConnectionCarry {
            src: src.to_string(),
            dst: "s-0000000001000007d7d7".to_string(),
            op: op.to_string(),
        }
    }

    #[test]
    fn the_attempt_carries_what_its_manifest_left_out_to_the_commit_which_audits_it() {
        crate::test_log::install();
        let src = "s-0000000001000006c6c6";
        let arbiter = crate::HandoffAttemptArbiter::new();
        assert!(arbiter.connections_left_out().is_empty(), "nothing yet");
        arbiter.set_connections_left_out(Vec::new());
        assert!(arbiter.connections_left_out().is_empty(), "none left out");
        let left_out = vec![row(src, "read-screen"), row(src, "signal")];
        arbiter.set_connections_left_out(left_out.clone());
        arbiter.set_connections_left_out(vec![row(src, "write-input")]);
        assert_eq!(
            arbiter.connections_left_out(),
            left_out.as_slice(),
            "one manifest per attempt: the first record stands"
        );
        let mark = format!("EDGE: action=drop origin=handoff src={src}");
        assert!(crate::test_log::containing(&mark).is_empty());
        crate::App::audit_connections_left_out(arbiter.connections_left_out());
        let audited = crate::test_log::containing(&mark);
        assert_eq!(audited.len(), 2, "one drop per row: {audited:?}");
        assert!(audited.iter().all(|record| record.target == "session_edge"));
        assert!(
            audited[0].message.ends_with("op=read-screen"),
            "{audited:?}"
        );
        assert!(audited[1].message.ends_with("op=signal"), "{audited:?}");
    }

    /// Wired where it holds: the worker records what the writer returned, and
    /// the Commit audits it on both of its arms — inside the operator's permit,
    /// right before the one write — past the claim that makes it the Commit.
    #[test]
    fn the_drops_are_audited_by_the_commit_alone() {
        let src = include_str!("app_update_handoff.rs");
        let wrote = src
            .find("let outgoing = crate::seamless::write_outgoing_with_held(")
            .expect("the worker writes the manifest");
        let recorded = src
            .find("job.arbiter.set_connections_left_out(connections_left_out);")
            .expect("and records what it left out");
        assert!(wrote < recorded);
        let claim = src
            .find("let claim = claimed || (admitted && arbiter.try_begin_commit());")
            .expect("the Commit's claim");
        let commit_at = |from: usize| {
            from + src[from..]
                .find("crate::seamless::commit_and_exit(commit_fd, proof)")
                .expect("a Commit write")
        };
        // Spelled in halves so this test's own text is not counted.
        let call = ["Self::audit_connections", "_left_out("].concat();
        let first = commit_at(claim);
        let second = commit_at(first + 1);
        for write in [first, second] {
            let audit = src[..write]
                .rfind(call.as_str())
                .expect("audited before the write");
            assert!(audit > claim && write - audit < 160, "right before it");
        }
        assert_eq!(
            src.matches(call.as_str()).count(),
            2,
            "nowhere but the Commit's two arms"
        );
        let seamless = include_str!("seamless.rs");
        let shed = seamless
            .find("fn shed_connections_to_fit(")
            .expect("the writer's shed");
        let end = shed + seamless[shed..].find("\n}\n").expect("its end");
        assert!(
            !seamless[shed..end].contains("session_edge_audit::emit("),
            "the writer audits nothing"
        );
    }
}
