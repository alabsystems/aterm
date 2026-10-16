// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Seamless update handoff, an inherent-impl split of `App`: `apply_staged_update_now`, the
//! unix overlap worker (`run_handoff_worker`), the reap authority that licenses a rollback
//! (`HandoffRollbackWarrant`), the Commit-time admission facts, and `finish_update_handoff`'s
//! drain gate, Commit and rollback.
//!
//! The successor races the parent's in-flight `atpkg` pass like a second window: the orphaned
//! child keeps the store lock, so the successor's launch lanes `--wait-lock` behind it. A
//! new-build successor runs one full update pass after its seed (`crate::launch_update_park`).
//! A rejected candidate's sweep SIGKILLs its process group, atpkg child included (the store is
//! crash-consistent).

#[cfg(unix)]
use winit::event_loop::ActiveEventLoop;

use crate::App;
#[cfg(unix)]
use crate::Wake;
#[cfg(unix)]
use crate::app_input::paste_order;

// The seamlessness invariant is proved in `app_update_handoff_island.rs` (a `clean { … }`
// island), a separate file because a compiler without the Clean surface cannot lex an island
// body and only a `mod` declaration decides whether a file is read. build.rs sets
// `clean_islands` when the compiler is Trust, so the stable cross lanes still compile aterm-gui.
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

/// Everything one attempt captured under the park. Separate from [`HandoffWorkerJob`]
/// because on the launched lane the worker is already running when the main thread parks,
/// so the capture reaches it as its own message ([`HandoffTransferJob`]).
#[cfg(unix)]
struct HandoffCapture {
    /// When the parent started parking its readers: the zero point of the user's freeze and
    /// of the capture deadline. Data only.
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
    /// The successor runs an update candidate rather than this image (the same-image QA
    /// seam), settled from the attempt's ticket and seam, never from whether the worker
    /// re-verifies. Decides the code its dialer must run ([`Self::dialer_code`]).
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
#[expect(
    clippy::disallowed_types,
    reason = "the update successor's raw Command: it hands over exactly its own descriptors, which a strip at the exec would undo"
)]
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
    /// `Info.plist`; only then is it offered one (`ATERM_HANDOFF_GRANT_CAPS`).
    successor_grant_chunks: bool,
    command: std::process::Command,
    /// What the park produced: `Some` from construction on the fork lane, and on the
    /// launched lane once the main thread's [`HandoffTransferJob`] arrives. Read through
    /// [`Self::capture`].
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
    /// The candidate's sealed `Info.plist` declares the chunked rendezvous grant, so the
    /// launched lane may hand it up to [`crate::handoff_rendezvous::MAX_CHUNKED_SESSIONS`]
    /// sessions. `false` when nothing passed.
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

/// The held panes' window, a pure function of the clock: the held capture runs only when at
/// least `comfort` (half the freeze budget) is left before the park's `deadline`, and stops a
/// quarter of the budget (`comfort / 2`) short of it, because the layout capture and the
/// callers' deadline check still follow. `None`: no time for held panes.
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

/// Refuses a fork-lane park taken above its successor's policy, before the child starts:
/// with no cached pass the fork lane parks under `parked_under` (Full) before the worker
/// reads the policy, and a lower policy fails the attempt with the pass published, so the
/// next park follows it. `None` on the launched lane.
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

/// The worker's handle on the App's pre-verification cache (`crate::HandoffPreverification`)
/// for its attempt's artifact. A worker that re-verifies publishes what it read, so the
/// launched lane's park finds the policy by the same key; a refusal that is a verdict is
/// published too ([`Self::publish_refusal`]), answering the automatic lane's later attempts
/// until a newer verdict replaces it. A person's press always verifies afresh, and a
/// passing refusal is never published.
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

    /// Publish the worker's own refusal `reason` when it is a verdict, never a passing moment,
    /// exactly as the arm-time read caches one (`App::spawn_staged_handoff_preverification`),
    /// so the returned attempt is booked by the verifier's word as the synchronous arm books it.
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
    /// `ATERM_DEBUG_SEAMLESS_REEXEC=1`: the QA seam that exercises the full handoff and adopt
    /// path without a release.
    DebugSeam,
}

/// How this attempt hands its descriptors to the successor, chosen once on the main thread
/// before any reader parks because it decides the term the adoption proof hashes.
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
    /// The authorized `target_build` is at least this build, so an older successor is
    /// never offered descriptors it cannot receive.
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

/// The hold fence the Commit raises ([`App::fence_hold_serials`]): no `hold` or direct
/// reset is answered between the Commit's hold-serial read and `_exit`, where it would be
/// lost. Dropping it lifts every fence; a Commit that lands never drops it.
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

/// A Commit that holds its claim through one more input-dispatch fence: what it answered
/// at the claim (`App::answer_at_commit_claim`) took real time, and a key queued meanwhile
/// would die with the `_exit`. It keeps what its admission took (the operator's quiesce,
/// the hold fence), so nothing fenced off lands before the write, and the next pass reads
/// the admission again and commits or rolls back. The supervisor host runs through the
/// fence, and the successor is not activated until the write.
#[cfg(unix)]
pub(crate) struct HeldCommit {
    /// The resident operator's quiesce (`None`: there is no operator).
    operator_quiesce: Option<crate::operator_host::UpdateQuiesce>,
    /// The hold fence the claim's admission raised.
    hold_fence: HoldFence,
}

/// Fresh mutable facts required immediately before the attempt-wide Commit CAS; a pure
/// conjunction. Queued PTY output and hardware input are not facts here: kernel-queued
/// bytes replay through the child's parser, and the drain gate flushes input into the
/// still-open masters.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandoffCommitFacts {
    exact_sessions: bool,
    exact_layout: bool,
    /// The agents the handoff layout carries for the successor to relaunch
    /// ([`commit_agent_carry`]) match the pending layout's: otherwise a relaunch
    /// owed in between would be lost or run twice. Activity.
    exact_agent_carry: bool,
    /// The activity epoch is unchanged since the attempt was armed, and every session's
    /// hold serial is where the park found it (`App::hold_serials`): a `hold`, or a direct
    /// `reset`, after the carry was drawn would never reach the successor.
    exact_activity: bool,
    teardown_allows_commit: bool,
    parent_still_parked: bool,
    sessions_alive: bool,
    /// The OS input queue has been dispatched into the masters: the main thread ran
    /// the event loop for a bounded interval after ProofReady, so every hardware
    /// event the OS had accepted flowed through the tolerated input path to the
    /// still-open PTY masters (see the drain gate in `finish_update_handoff`).
    input_dispatch_fenced: bool,
    /// Process-local egress is drained to the kernel: no tolerated keystroke left in a
    /// process-local queue and no frame part-way into a master, either of which `_exit`
    /// would lose. Read last, under the Commit's egress fence (`fence_handoff_egress`).
    egress_settled: bool,
    native_safe: bool,
    proof_exact: bool,
    commit_channel: bool,
    /// The resident operator, as two facts because its refusal asks two different things
    /// ([`crate::operator_host::UpdateQuiesceRefusal`]). `operator_idle`: no transaction
    /// was in flight and its gate was free (or there is no operator); false is activity.
    operator_idle: bool,
    /// `operator_quiesced`: the operator is fenced for the replacement (or there is none).
    /// False with `operator_idle` true is a fault: it is shutting down, or a quiesce is already
    /// active or went stale.
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

/// The Commit-time layout comparison, reduced to window/tab/pane topology: window
/// position, show state and raw inner size are normalized away (the successor restores the
/// pending capture's), and so are cwd/title (`restore_session_meta` degrades on
/// `WouldBlock`). Agents are compared apart ([`commit_agent_carry`]); Settings drafts are
/// compared, so a key typed into Settings during the boot refuses as activity.
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
                // The agent the successor relaunches is not topology, nor the one it
                // watches; each is compared on its own (`commit_agent_carry`).
                terminal.agent = None;
                terminal.supervised = None;
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
        // Show state, raw inner size and the presence row are live, not topology: a drag
        // inside one cell changes the size and no grid. The successor restores the pending
        // capture's (macOS records no maximized state: `app_restore::TRACKS_NORMAL_FRAME`),
        // and sizes from it only when it holds that capture's grid, which stays compared.
        window.maximized = None;
        window.show = crate::restore::WindowShow::UNKNOWN;
        window.inner_px = None;
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

/// What the handoff layout's leaves carry of the supervisor host's agents, in layout
/// order: each terminal leaf's session, the agent the successor relaunches there, and the
/// process it watches (pid and kernel start). Compared at Commit apart from topology
/// (`HandoffCommitFacts::exact_agent_carry`): it moves with nothing on screen moving (an
/// agent crashing or a relaunch landing during the boot), and a change missed here would
/// be lost or decided twice.
#[cfg(unix)]
pub(crate) fn commit_agent_carry(layout: &crate::restore::RestoreManifest) -> Vec<AgentCarry> {
    layout
        .handed_off_leaves()
        .into_iter()
        .map(|leaf| {
            let watched = leaf.supervised.map(|agent| (agent.pid, agent.start));
            (leaf.local_id, leaf.agent, watched)
        })
        .collect()
}

/// One handoff leaf's carry of the supervisor host's agents
/// ([`commit_agent_carry`]): its session, the relaunch the successor makes
/// there, and the process it watches there.
#[cfg(unix)]
pub(crate) type AgentCarry = (
    Option<u64>,
    Option<Box<crate::restore::AgentRestore>>,
    Option<(u32, String)>,
);

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

/// The human-readable reason a ProofReady completion did not Commit, derived from the same
/// [`HandoffCommitFacts`] the admission read so the two cannot drift; the Commit-race
/// flags arrive separately, decided after admission.
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
        "tolerated input was still on its way to a PTY at Commit (it outlasted the \
         egress-flush budget, or began after it)"
            .to_string()
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

/// Whether a rejection was activity (lossless, so automatic mode may spend bounded retries
/// on it) or genuine, derived from the admission's facts, never from strings. Session
/// death is activity wherever it is seen (the park, the worker's pre-Commit loop and
/// `wait_handoff_ready` file it the same way): a shell ending mid-overlap is the desk
/// changing, not evidence against the build, and filing it physical would push a healthy
/// build back by half-hours. A busy operator is activity; one that refused otherwise is a
/// fault.
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
            // We ended this candidate, off a bare pid from the completion wire. Nothing here
            // witnessed a death of its own.
            child_death: crate::ChildDeathEvidence::Unobserved,
        },
    ));
}

#[cfg(unix)]
impl HandoffWorkerCleanup {
    /// All filesystem repair, done before the UI is told rollback is safe so the event-loop
    /// completion touches no file and probes no process.
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
    // A pipe, not a socketpair: a write of at most PIPE_BUF to a pipe is atomic, which
    // `commit_and_exit`'s fixed-size proof/Commit wires rely on; a stream socket may
    // short-write.
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

/// Why a parked PTY reader may be resumed: only while no other process can read the
/// handed-off masters, since two readers interleave the stream unrecoverably. There is no
/// "waited long enough" variant.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
enum HandoffRollbackWarrant {
    /// The attempt failed before any candidate was spawned, so nothing outside
    /// this process has ever held a handed-off master.
    NoCandidate,
    /// A successor was launched but the out-of-band transfer never started, so no
    /// descriptor left this process: none leaves before the grant's first `sendmsg`.
    /// The successor finds the rendezvous gone and exits.
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

/// The kernel's birth record for the process at a pid, unique across pid reuse and
/// compared for equality only. `seamless::ProcessBirth` reads the same fact with the
/// opposite fail-safe direction, so each keeps its own probe.
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
    // Only a record that names `pid` answers: one about another pid would mislead the
    // comparison this feeds.
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

/// The process that must be proven terminated before any parked reader resumes, plus whatever
/// identity keeps a recycled pid from impersonating it.
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
    /// The pid names a different process. The kernel does not reallocate a pid before its
    /// previous owner is reaped, so the candidate has terminated.
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

    /// The identity of a candidate this process did not fork, from the pid the kernel
    /// attested at the rendezvous accept (`LOCAL_PEERPID`); launchd may free the pid at
    /// any moment, so the stamp is what makes it an identity.
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

/// Whether the candidate has terminated, so it cannot hold or read the handed-off masters.
/// Two proofs, each what `Child::wait` establishes: pid vacancy (`kill(pid, 0)` answering
/// ESRCH, which pid reuse can hide but never fabricate), or a different birth stamp at the
/// pid (a pid is not reallocated until its owner is reaped). Anything else is unproven,
/// and unproven never resumes a reader.
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

/// Puts the calling process in its own process group and proves it, so helpers it forks
/// are inside the group a rejecting parent sweeps: the successor-side twin of the fork
/// lane's `pre_exec` `setpgid(0, 0)`, for a launched successor no parent hook runs in.
/// Called from [`crate::main_entry`] before anything can run another program, not from
/// `seamless::prearm_incoming_fds`, which unit tests run in-process. The answer is
/// `getpgrp()` read back, not `setpgid`'s return: the one expected failure, a session
/// leader's EPERM, already leads its group. Idempotent; the boot-apply re-exec keeps the
/// group.
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

/// What this process can learn about a candidate without changing anything: one
/// `waitid(WNOHANG | WNOWAIT)` says whether it is ours (`ECHILD` otherwise) and whether it
/// has died.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandoffCandidateProbe {
    /// `waitid` refused: not a child of ours (the launched lane), or already
    /// reaped. Nothing may be concluded, and nothing is pinned.
    NotOurChild,
    /// Our child, and it has not exited (stopped counts as not exited); a dying candidate
    /// looks like this until its zombie state is visible.
    Running,
    /// Our child, already exited, and still waitable — so its status is intact for
    /// the reap that follows.
    Exited,
}

/// Non-destructive: `WNOWAIT` leaves an exited child waitable for the reaper. Reads
/// `si_code` (a plain field everywhere; `si_pid`/`si_status` are union accessors on
/// Linux), which stays 0 in the zeroed struct for an unchanged child; `si_signo` is
/// unusable because Darwin reports a stopped child even when only `WEXITED` is asked for.
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

/// A non-parent's witness to how a candidate died: launchd's child answers `waitid` with
/// `ECHILD`, but `kqueue` `EVFILT_PROC` with `NOTE_EXIT | NOTE_EXITSTATUS` delivers its
/// full `wait(2)` status to a watcher permitted to `SIGKILL` it. The event stays queued
/// until read, so launchd reaping the candidate cannot take it away. The knote is
/// one-shot, so the watch keeps what it read.
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

/// SIGKILLs a candidate whose identity was just corroborated: its group when `-pid`
/// provably names it ([`candidate_leads_its_own_group`], or [`candidate_is_our_child`],
/// since Darwin answers `getpgid` on a zombie leader with ESRCH), then the candidate.
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

/// SIGKILLs the candidate before anything waits on it. The group sweep (`-pid`) stops its
/// ditto/codesign/spctl descendants from mutating updater paths after the leader is gone;
/// it is sent only when the kernel says the candidate leads its group or is our unreaped
/// child (forked: `pre_exec`'s `setpgid`; launched: [`contain_own_process_group`]). The
/// direct signal needs a corroborated identity, since a recycled bare pid names a
/// stranger. Rollback is licensed by [`handoff_candidate_terminated`], never by the signal.
#[cfg(unix)]
fn signal_handoff_candidate(candidate: HandoffCandidate) {
    let Ok(pid) = libc::pid_t::try_from(candidate.pid) else {
        return;
    };
    // `-pid` is kill(2)'s process-group target and -1 its broadcast target, so a pid
    // below 2 must never reach it.
    if pid <= 1 {
        return;
    }
    match candidate.identity() {
        // Somebody else answers to the number now. There is nothing of ours to
        // signal, and signalling would land on them.
        HandoffCandidateIdentity::Recycled => (),
        HandoffCandidateIdentity::Corroborated => kill_corroborated_candidate(pid),
        HandoffCandidateIdentity::Unwitnessed => {
            // A group kill is sound only while the pid is pinned: a launched candidate can
            // be reaped by launchd, and `-pid` would then name a stranger's group. So ask
            // the kernel now (`WNOWAIT`; ECHILD means not pinned).
            if candidate_is_our_child(pid) {
                // SAFETY: SIGKILL to the candidate's process group, whose number the
                // kernel just confirmed is held by a child of ours, then to the candidate
                // itself (the pinned pid makes that land on it; the group sweep alone
                // misses a child that inherited this process's group).
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

/// Blocks until the candidate is provably terminated, then licenses rollback. Unbounded on
/// purpose, like `Child::wait`: resuming while the candidate might still read the masters
/// corrupts the stream. The interval only decides when a candidate that will not die is
/// logged.
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

/// The reap's two products: the rollback warrant and, when our own `wait` answered for the
/// candidate, the status it collected (`None` on the launched lane and for a
/// launcher-shaped child).
#[cfg(unix)]
struct ReapedHandoffCandidate {
    warrant: HandoffRollbackWarrant,
    status: Option<std::process::ExitStatus>,
}

/// Kills the rejected candidate and proves it gone, on the handoff worker; the warrant
/// licenses [`App::rollback_overlap`]. It never rests on the group sweep: it comes from
/// `wait` on our own child or from [`wait_for_handoff_candidate_to_terminate`].
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

/// What this process may do to the candidate, which depends on how it was started: only a
/// forked one has a `wait`, and the type makes the caller say which lane it is on.
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

    /// Whether the candidate has provably ended, asked without changing anything: EOF on
    /// the readiness channel needs every copy of the write end closed, and a sibling fork
    /// in its fork-to-exec window can hide the death. The fork lane asks `waitid(WNOWAIT)`;
    /// the launched lane [`handoff_candidate_terminated`], never the one-shot
    /// [`CandidateExitWatch`] the hold loop owns.
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

/// The emergency reaper's kill from a bare `child_pid` (on its own thread, or inline when
/// that thread cannot be created); the warrant comes from `waitpid` or the outside proof.
#[cfg(unix)]
fn emergency_kill_and_reap_handoff_child(pid: u32) -> HandoffRollbackWarrant {
    // The caller won the Emergency reaper CAS, so nothing else consumes the identity. The
    // group is signalled before any wait: `waitpid` reaps a dead leader, and its helpers
    // would otherwise keep mutating updater paths.
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

/// Pre-park admission peek: a readable byte its reader has not taken on a live master, so
/// automatic mode does not begin an overlap while output flows (mid-flight, queued output
/// is tolerated: use [`handoff_masters_closed`]). A hung-up master (macOS reports
/// `POLLIN|POLLHUP`) does not count, or a `--hold` pane would block every attempt.
#[cfg(unix)]
pub(crate) fn handoff_masters_have_activity(live: &[(u64, i32, i32)]) -> bool {
    peek_handoff_masters(live, |revents| {
        revents & libc::POLLIN != 0 && revents & HANDOFF_MASTER_DEAD == 0
    })
}

/// Mid-flight death peek: true only when a handed-off master reports
/// POLLHUP/POLLERR/POLLNVAL; queued output waits in the kernel for the child. Requests
/// `events: POLLIN` anyway: macOS evaluates a PTY master only for requested events.
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

/// Whether every handed-off session's process-local egress has reached the kernel
/// ([`fence_session_egress`]), where `_exit` cannot destroy it. A master no longer in the
/// pool is settled (`sessions_alive` rejects its death).
#[cfg(unix)]
fn handoff_egress_settled(pool: &crate::SessionPool, live: &[(u64, i32, i32)]) -> bool {
    live.iter().all(|(_, master, _)| {
        if let Some(session) = pool.iter().find(|session| session.master == *master) {
            fence_session_egress(&session.ctx.sink).is_some()
        } else {
            !paste_order::is_master_ordering_for_handoff(*master)
        }
    })
}

/// One handed-off session's egress, stopped at a frame boundary
/// ([`aterm_session::sink::SinkWriter::try_fence_egress`]): `None` while a job waits in its
/// paste-order FIFO, bytes wait in its spill, or a frame is being written (a control verb's
/// blocking write into a program not reading), which `_exit` would cut mid-write, leaving a
/// bracketed paste open. The Commit holds every session's through its write
/// ([`fence_handoff_egress`]).
#[cfg(unix)]
fn fence_session_egress(
    sink: &aterm_session::sink::SinkWriter,
) -> Option<aterm_session::sink::EgressFence<'_>> {
    if paste_order::is_ordering(sink) {
        return None;
    }
    sink.try_fence_egress()
}

/// The sinks of the handed-off sessions still in the pool.
#[cfg(unix)]
fn handoff_sinks(
    pool: &crate::SessionPool,
    live: &[(u64, i32, i32)],
) -> Vec<std::sync::Arc<aterm_session::sink::SinkWriter>> {
    pool.iter()
        .filter(|session| live.iter().any(|(_, master, _)| *master == session.master))
        .map(|session| session.ctx.sink.clone())
        .collect()
}

/// The Commit's egress fence: every handed-off session's egress stopped at a frame boundary
/// ([`fence_session_egress`]), or `None` when one still has input on its way. Held through the
/// Commit's write, so no frame that begins after the drain gate's last reading is cut by the
/// `_exit` either: it waits before its first byte, and goes on if the Commit stands down.
#[cfg(unix)]
fn fence_handoff_egress(
    sinks: &[std::sync::Arc<aterm_session::sink::SinkWriter>],
) -> Option<Vec<aterm_session::sink::EgressFence<'_>>> {
    sinks
        .iter()
        .map(|sink| fence_session_egress(sink))
        .collect()
}

/// The names the artifact a successor must swap in travels under — read and
/// cleared by its boot apply (`aterm_update`'s `take_expected_artifact`).
const EXPECTED_BUILD_ENV: &str = "ATERM_UPDATE_EXPECTED_BUILD";
const EXPECTED_COMMIT_ENV: &str = "ATERM_UPDATE_EXPECTED_COMMIT";
const EXPECTED_DIGEST_ENV: &str = "ATERM_UPDATE_EXPECTED_DMG_SHA256";

#[expect(
    clippy::disallowed_types,
    reason = "the update successor's raw Command: it hands over exactly its own descriptors, which a strip at the exec would undo"
)]
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

/// The name the cold lane's budget travels under across its `exec`: written by the cold arm
/// beside the expected-artifact binding, and read and cleared by the relaunched image before
/// its boot apply ([`ColdRelaunch::of_this_launch`]), so neither a swapped-in build's re-exec
/// nor any shell is born with it.
#[cfg(unix)]
const COLD_LANE_BUDGET_ENV: &str = "ATERM_UPDATE_COLD_LANE_BUDGET";

/// The automatic lane's memory of one artifact, carried across the cold lane's `exec`: the
/// successor is this build in a new process, and without the physical-failure count and
/// the structural verdict a boot swap that keeps refusing would be booked as a first
/// failure on every relaunch, for ever. Carried exactly: the count, how long ago the last
/// failure was, which side of the swap each record keys, and the verdict's re-sample.
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

    /// [`Self::encode`]'s inverse: `None` unless the value is about exactly `build`'s
    /// `artifact`, so a budget never answers for other bytes. A half that does not parse is
    /// dropped on its own, and the lane books that half from an empty record.
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
#[expect(
    clippy::disallowed_types,
    reason = "the update successor's raw Command: it hands over exactly its own descriptors, which a strip at the exec would undo"
)]
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

/// The name the cold-lane parent's unanswered update-health warnings travel under across
/// its `exec`: written by the cold arm, taken by the window's entry after the boot apply
/// (whose re-exec inherits it), so no shell or window is born with it.
pub(crate) const COLD_LANE_HEALTH_SAID_ENV: &str = "ATERM_UPDATE_HEALTH_SAID";

/// Carries the parent's unanswered update-health warnings across the cold lane, as the
/// seamless lanes do in the window carry, so the landing can record their healing. JSON,
/// the window carry's shape; none when nothing is unanswered.
#[cfg(unix)]
#[expect(
    clippy::disallowed_types,
    reason = "the update successor's raw Command: it hands over exactly its own descriptors, which a strip at the exec would undo"
)]
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

/// The cold-lane parent's unanswered update-health warnings, taken from this process's
/// environment whatever this launch is, and returned only when this run is an update's
/// landing (`just_updated`). Read by the window's entry, single-threaded, after the boot
/// apply.
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

/// This launch is the cold lane's relaunch of its own build: the artifact the outgoing
/// image bound for it to swap in (no handoff descriptors, `ATERM_UPDATED_FROM` naming this
/// build, the whole binding present), with the lane's budget for it. The process that
/// decided to relaunch is gone, so when this boot swap refuses, [`ColdRelaunch::seed`]
/// turns the refusal into the verdict the lane reads before it relaunches again
/// ([`crate::App::cold_lane_verdict_gate`]).
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
    /// The boot swap's refusal when it was only the wait for an update check to confirm the
    /// stage's signer ([`aterm_update::is_roster_confirmation_wait`]): no verdict at all, so
    /// the lane holds for the check instead of reading [`Self::verdict`].
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
    /// Read from this process's environment before the boot apply consumes the binding;
    /// the budget alone is taken here, whatever this launch is. `handoff_successor`: the
    /// launch carries a handoff's descriptors, whose parent owns the verdict.
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

    /// What this relaunch's boot apply leaves its automatic lane, `Some` when the swap
    /// refused (`Deferred`) or was rolled back (`ReExecFailed`): the refusal as the
    /// pre-park verdict on these bytes, with the outgoing process's budget, so the first
    /// automatic attempt books it where the schedule stands instead of relaunching into
    /// it. A passing refusal is held too, since this one already cost a relaunch. The wait
    /// for a roster confirmation is no verdict and is carried as
    /// [`ColdRelaunchSeed::roster_wait`]: the lane holds for the check, charged nothing.
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

/// The first half of [`send_warranted_handoff_failure`]: the completion itself, `false`
/// when the event loop is gone. Split out so a reject path can give back the counted trial
/// launch before the disk facts read the sentinel's count.
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
        // The completion's detail is short; the log says which death this was.
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

/// Kills the candidate, proves it terminated, and says what killed it, in protocol order:
/// ask what is known before any signal (anything then is its own death), kill and prove
/// termination, then ask again; [`handoff_child_death`] decides what is attributable. Only
/// [`crate::UpdateHandoffOutcome::ChildDied`] has the candidate's own death to describe.
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
    // First answer wins, in the order they were asked: the pre-kill witness needs no
    // attribution argument at all, `wait` is this process's own authority over its own
    // child, and the post-termination knote is the launched lane's last resort.
    let status = witnessed_before_the_kill
        .or(reaped.status)
        .or_else(|| handle.witnessed_exit_status());
    (
        reaped.warrant,
        handoff_child_death(died_before_we_signalled, status),
    )
}

/// Whether the reject path gives back the boot-trial launch this candidate counted: kept
/// only when the new bytes answer for the death, by the evidence's shape, not the outcome
/// (a starved candidate produces proof EOF as readily as a faulting one). One predicate
/// keeps the retry budget and the boot sentinel from drifting apart, or retries of a
/// healthy candidate would stay counted until `check_boot_health` reverts it. Forgiving is
/// cheap: it returns only this candidate's launch, once, and a successor that truly cannot
/// boot still counts its ordinary relaunches.
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

/// Turns one `wait(2)` status into evidence, or refuses to. This process only ever sends
/// `SIGKILL`, so an exit code or any other signal is the candidate's own; a bare `SIGKILL`
/// is the machine's (macOS jetsam) only with `died_before_we_signalled`, else nothing is
/// claimed.
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

/// A cancel during preparation: it precedes the spawn, so there is no candidate to prove
/// anything about (warrant `NoCandidate`).
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
            // A cancel is activity, never evidence against the artifact.
            crate::UpdateHandoffOutcome::ActivityRevoked,
            "activity revoked handoff during physical preparation",
        ),
    );
}

/// This process could not prepare the handoff: raised before any candidate held a master
/// (warrant `NoCandidate`) and filed `ProducerFailed`, since none of it is about the
/// candidate's bytes.
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

/// The outcome a refused pre-park verification is filed under, read off the verifier's own
/// words: `PreparationFailed` (Structural) for a verdict on the candidate, `ProducerFailed`
/// (Transient) for a refusal without one ([`aterm_update::is_passing_refusal`]), and
/// `StageUnconfirmed`, no failure at all, for the stage's wait for an update check to
/// confirm its signer ([`aterm_update::is_roster_confirmation_wait`]).
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

/// The refusal a worker owes when a fresh cached pass let it skip the full
/// pre-verification: the cache cannot vouch for an operator apply floor raised, or a stage
/// revoked, since it was taken, so a staged candidate is asked both under the commit the
/// pass was cached for, and an activation the floor alone. `None` when the worker verifies
/// anyway, and for the same-image QA seam. `floor` is
/// [`aterm_update::handoff_cached_pass_refusal`].
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

/// The worker's first real action, with its readers injected so a test drives the shipped
/// decision: the full verification (`verify`: codesign and sealed rebinding, under the apply
/// lock) off the UI thread, so a doomed candidate is refused before anything is written or
/// spawned; or, after a fresh cached pass, the floor alone (`floor`,
/// [`skipped_verification_floor_refusal`]).
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
        // The candidate passed, so the policy it carries was read. Published before the
        // launch: the launched lane's park reads it on the main thread once the successor
        // has dialled.
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
        // A fresh cached pass skipped the full check; the floor and the stage are what the
        // cache cannot vouch for, refused as the full check would refuse them.
        return Err(refused(error));
    }
    Ok(())
}

/// The cold lane's table at its `exec`: every descriptor above stdio marked close-on-exec,
/// so nothing stray rides into the successor, then the harness's lifeline made inheritable
/// (the guard flags it again if the `exec` returns). Safe on the live table: a descriptor
/// meant for a child is close-on-exec until its hook clears it.
#[cfg(unix)]
fn cold_exec_hand_over() -> Option<crate::lifeline::InheritedAcrossExec> {
    aterm_uds::spawnfd::InheritStrip::prepare().apply();
    crate::lifeline::inherit_across_exec()
}

/// The fork lane's child, between its fork and its `exec`: its own process group (before
/// the candidate's image exists, which the successor-side `contain_own_process_group`
/// cannot promise; keep both), then the whole table marked close-on-exec
/// ([`aterm_uds::spawnfd::InheritStrip`]) with the flag cleared on exactly the `handed`
/// descriptors, so nothing the fork copied without close-on-exec (a shader-cache file, a
/// bridge socket before its `F_SETFD`) rides into the successor. Only the child's copies
/// change.
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

    // The lanes split here, before anything is written. The launched lane launches and
    // holds the successor before asking the main thread to park, so the swap, re-exec and
    // boot check run while the terminal still echoes. It finishes the attempt itself unless
    // refused before anything launched; then it rejoins here with the main thread's
    // capture, where the fork lane starts.
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

    // The fork lane parks before it spawns, so every artifact is written here.
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
            .envs([crate::handoff_rendezvous::ENV_PROOF_TERM.pair(proof_term)]);
        // The launcher's soft descriptor limit, when a claim or a park raised this
        // process's own: the forked child inherits the raised limit and never claims, so
        // without this its shells would get the raise instead of the launcher's.
        crate::handoff_rendezvous::carry_launcher_limit(&mut job.command);
    }
    job.command.envs([
        crate::seamless::ENV_MANIFEST.pair(path),
        crate::seamless::ENV_NONCE.pair(&nonce),
        crate::seamless::ENV_FDS.pair(wire),
        crate::seamless::ENV_LAYOUT.pair(layout_path),
        // The candidate proves it is the authorized build by comparison and logs
        // which half disagreed. Forging this can only lose a handoff; the parent
        // compares against its own ticket.
        crate::seamless::ENV_TARGET.pair(crate::seamless::encode_target_identity(
            job.target_build,
            &job.target_commit,
        )),
        crate::seamless::ENV_READY_FD.pair(proof_wr.as_raw_fd().to_string()),
        crate::seamless::ENV_COMMIT_FD.pair(commit_rd.as_raw_fd().to_string()),
    ]);
    // Parental authority: our pid and its kernel birth record, which lets the
    // successor watch us without being our fork child (`seamless::AttestedParent`,
    // encoded beside its decoder).
    for (key, value) in crate::seamless::outgoing_parent_env() {
        job.command.env(key, value);
    }
    // Parent descriptors remain CLOEXEC for the whole asynchronous interval. Clear only
    // the fork child's copies immediately before its exec image.
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
    // `fork_lane_child_holds` strips the child's table before it hands it these; the
    // strip's bound is read here, before the fork.
    let strip = aterm_uds::spawnfd::InheritStrip::prepare();
    // SAFETY: the closure runs between fork and exec, and
    // `fork_lane_child_holds` makes only async-signal-safe calls (`setpgid`,
    // `fcntl`, `InheritStrip::apply`) on captured integers, not shared memory.
    unsafe {
        job.command
            .pre_exec(move || fork_lane_child_holds(strip, &child_inherit));
    }
    // An older target starts at the limit this process held before a park's raise: an
    // earlier attempt's raise is never lowered here, and an older successor hands its
    // shells whatever it inherits. Registered after the hook above, so the child's last
    // look at its table sees what it is handed.
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
    // The fork lane. On macOS a forked successor keeps the bootstrap context of this app's
    // launchd job, which dies with this process, so `hdiutil`, notifications and
    // LaunchServices opens fail in it (`tests/handoff_launchd_job.rs`); the out-of-band
    // lane avoids that, and forking remains for the cases `out_of_band_lane_refusal`
    // names. `pre_exec` sets the process group before `spawn` returns, so `kill(-pid)` is
    // valid at once.
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
    /// The attempt's two pipes, as [`AttemptPipes`]: a swapped pair deadlocks like a slow
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
    /// The desk's own content refused the write deterministically
    /// ([`crate::seamless::WriteOutgoingFailure::ManifestOverCap`]): filed `CaptureRefused`,
    /// retried once the desk changes, never `ProducerFailed`, whose retries would freeze
    /// every reader again for the same answer.
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

/// Every artifact one attempt publishes, in the order the successor consumes them, shared
/// by both lanes so the successor's bytes cannot depend on how it was started. Runs after
/// the park and touches no master. `early` is what the launched lane prepared before the
/// park: its pipes are used as they are, and its layout sidecar kept only when byte-equal
/// to the park's layout.
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
    // The control carry's export runs on the worker, not in the freeze: every reader is
    // still parked, so no engine moves. The turn-id count rides the manifest with a
    // reserve for the turns this process still serves until its `_exit` (the Commit checks
    // it held), so the successor never mints an id this one already replied with; the
    // export takes the same ceiling. Records are spelled in the words the target reads.
    let turn_id_ceiling = crate::control::turn_ids_for_handoff();
    let controls = crate::handoff_carry::export_for(
        &capture.carries,
        turn_id_ceiling,
        crate::handoff_carry::StatusWords::for_target(target_build),
    );
    capture.manifest.next_turn_id = crate::handoff_carry::manifest_turn_id(turn_id_ceiling);
    // The ceiling rides this attempt, which the Commit reads it from.
    job.arbiter.set_turn_id_ceiling(turn_id_ceiling);
    // The history carry's join (`crate::handoff_history`): a session whose export meets
    // its checkpoint names its sidecar; the others keep the checkpoint's bounded history
    // and count the lines that do not cross. Never fails the attempt. Runs after the
    // control carry's export so the fork lane's history export does not contend for the
    // same locks. The held panes' exports go to their own carry, with the reason a pane has
    // none.
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
    // What the manifest left out rides this attempt to its Commit, which audits it; an
    // attempt that stands down dissolves none of it.
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

/// One step of a pre-grant stand-down, recorded so tests can replay the order against the
/// derived overlap model (`UngrantedRetireRequiresReap`).
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
    /// Whether this process gives back the stood-down successor's counted trial launch,
    /// decided by the death's evidence ([`forgives_the_counted_trial_launch`]), not by
    /// whether we signalled it. Exactly one party gives it back: its own EOF-rule exit is
    /// witnessed as `Exited { code: 0 }` and answers `false`; where the witness cannot
    /// tell, a second give-back is a no-op.
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

/// Stands a held successor down and proves it gone: close the rendezvous (EOF), wait the
/// grace for its own exit, then kill and prove termination, in the model's order.
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
        // Ask again after the proof: an exit between the witness read and the pid probe left
        // the witness unread, and XNU queues the knote in `proc_exit` before the pid falls
        // vacant, so this read finds it.
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

/// Retires a held, ungranted successor: stand it down, prove it gone, publish the
/// completion. The main thread goes first: it is told (`Wake::UpdateHandoffStandingDown`)
/// unconditionally, since this thread cannot know whether the readers are parked, closes
/// the park gate and resumes any parked readers, and only then does this thread revoke.
/// The wait is bounded by [`STAND_DOWN_ACK_BUDGET`] and fails open, since a candidate never
/// killed is worse than a late unpark.
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
    // Each step is a derived-model action (`StandDownStep::model_action`).
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

/// The boundaries of the parent's park->transfer interval on the launched lane, in the order
/// the attempt crosses them.
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

/// The hold: wait, with every reader live, for the main thread's capture or whatever ends
/// the attempt first (a stand-down, the cancel, the dialer leaving, a LaunchServices
/// contradiction, the hold bound).
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

/// The launched lane's early artifacts, made before the park while the successor boots:
/// the two pipes and the layout sidecar (`layout_path`) from the layout at launch. A step
/// that refuses is left to the park.
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

/// The launched lane in late-park order: bind, launch, accept and hold the dial with every
/// reader live, so the successor's swap, re-exec and boot check cost no freeze; ask the
/// main thread to park; then write the artifacts under the pre-minted nonce, grant on the
/// held stream, and hand over to the shared decision tail. The listener is bound before
/// the launch because the successor is told an existing name, and the nonce is minted
/// first so the launch environment can name the manifest and layout before they exist.
/// Before the grant no descriptor has left the process, so every failure stands the
/// successor down ([`retire_ungranted_successor`]).
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
        // A merged launch cannot express a removal and the fork lane can: the one
        // refusal where forking is not merely acceptable but correct.
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
    environment.push(crate::seamless::ENV_MANIFEST.pair(manifest_path));
    environment.push(crate::seamless::ENV_NONCE.pair(&lane.nonce));
    environment.push(crate::seamless::ENV_LAYOUT.pair(&layout_path));
    environment.push(
        crate::seamless::ENV_TARGET.pair(crate::seamless::encode_target_identity(
            job.target_build,
            &job.target_commit,
        )),
    );
    environment.push(crate::handoff_rendezvous::ENV_RENDEZVOUS.pair(rendezvous_path));
    environment.push(crate::handoff_rendezvous::ENV_CLAIM.pair(rendezvous.claim()));
    // The chunked grant is offered only to a candidate that declared it in its
    // verified `Info.plist`; it claims `ATRZ2C`. Any other successor never sees the
    // variable and claims `ATRZ1C`.
    if job.successor_grant_chunks {
        environment.push(
            crate::handoff_rendezvous::ENV_GRANT_CAPS
                .pair(crate::handoff_rendezvous::GRANT_CAPS_CHUNKS),
        );
    }
    // The warm hint, read by the successor's warm prologue before it dials.
    // Advisory: an older successor ignores it, and none authenticates by it.
    environment.push(crate::handoff_warm_hint::ENV_WARM_HINT.pair(&lane.warm_hint));
    for (key, value) in crate::seamless::outgoing_parent_env() {
        environment.push((key, value));
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
    // The launch is issued, not awaited: the successor dials before LaunchServices
    // answers. A refusal means nothing was launched, so the fork lane can still carry it.
    let in_flight = match crate::app_launch_successor::begin_launch(
        &bundle,
        &arguments,
        &environment,
        // Never activated at launch: the successor has no window yet. The parent
        // activates it at the Commit's write when an aterm window has focus there
        // (`App::activate_successor_at_write`).
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
    // The pipes and the speculative layout sidecar are made while the successor boots
    // (`handoff_diet`), before the accept, so no failure path sweeps the attempt's files
    // while a write is in flight.
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
    // Identity and witness first: every exit from here must prove the accepted dialer
    // dead, or the automatic lane could launch a second successor beside it. The exit
    // watch is registered now, since `EVFILT_PROC` attaches only to a process that exists.
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
    // Corroboration, not the lock: LaunchServices' answer, polled beside the hold and the
    // proof wait (`LaunchCorroboration`), must name the dialer and must not say the process
    // it launched exited while the dialer lives. A refusal here proves the dialer gone; the
    // pid LaunchServices names goes in the message only, so nothing acts on a process this
    // parent never touched.
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
    // A dialer that hung up during the hold or the write holds nothing and is owed
    // nothing but a proof of its death.
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
        // Only a failure before the descriptors left is an ungranted stand-down; after a
        // partial send the successor holds duplicates, and rollback owes the candidate
        // proof.
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

/// A launched pid in the `u32` shape the completion wire uses; zero, which every consumer
/// refuses, if it cannot convert.
#[cfg(target_os = "macos")]
#[must_use]
fn pid_for_completion(pid: i32) -> u32 {
    u32::try_from(pid).unwrap_or_default()
}

/// The launch environment a `Command` describes, or `None` when it cannot be expressed as
/// a merge: LaunchServices merges over the launcher's environment and cannot remove a
/// variable, so removing one set here (`bind_expected_update_artifact`'s `env_remove`)
/// makes the lane unavailable.
#[cfg(target_os = "macos")]
#[expect(
    clippy::disallowed_types,
    reason = "the update successor's raw Command: it hands over exactly its own descriptors, which a strip at the exec would undo"
)]
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
        // A refusing, crashed or starved successor all close the readiness channel alike;
        // the successor logs its own refusal and the reject path adds what this process
        // saw.
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
        // A candidate that proved adoption is alive and holding the masters; it has no
        // death to describe.
        child_death: crate::ChildDeathEvidence::Unobserved,
    });
    if proxy.send_event(ready).is_err() {
        // The main thread never validated, so no Commit can happen: reap the child.
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
        // A successor that proved and then ended must not be committed to (nobody would
        // own the terminals). This narrows the window; the atomic Commit write's EPIPE is
        // the backstop. Filed `Rejected`, never `ChildDied`: from here closing the Commit
        // pipe makes it exit, so a death may be ours.
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

/// Ceiling on the decode authority one capture may hand its successor, priced per session at
/// twice `dimension_grid_cap` (`seamless::checkpoint_capture_budget_bytes`), which overstates a
/// real screen by orders of magnitude so a hostile `meta` cannot authorize a huge decode. A
/// screen past it is carried blank; the exact bound is `MAX_HANDOFF_AGGREGATE_GRID_BYTES`,
/// charged against the real bytes.
#[cfg(unix)]
pub(crate) const MAX_HANDOFF_CAPTURE_BUDGET_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Acquires a parked session's engine within the freeze budget instead of failing on the
/// first contention: after `park_all_readers` the scrollback-compression worker and
/// render/status paths can still take the mutex, and under a flood do so on every attempt.
/// The wait spends the capture's budget ([`handoff_freeze_budget`]); a poisoned lock is taken.
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

/// The wall-clock window the park and capture must fit in, from the readers' park: the
/// freeze the user feels. Not a correctness bound: a miss costs scrollback, never the
/// update, and widens the window within one attempt (a re-park on the next rung) and
/// across attempts, up to a quarter second; an explicit apply gets the widest at once.
/// Rung 0 is `clamp(1.5 × seed, 20 ms, 80 ms)` from the attempt's [`FreezeSeed`]; the next
/// is twice rung 0 and at least 80 ms.
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
    /// This attempt's own dry run, timed while its successor booted, never budgeted below
    /// the last landed park (`last_park`): the dry run cannot time stopping the readers,
    /// the bulk of a real park (a two-session desk's capture measured 1.1 ms against a 20 ms
    /// park). Only the launched lane (macOS) times one.
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

/// Records what a landed park cost, for the next update's first rung, on its own thread:
/// the ledger lock is a file lock another process may hold, and this runs inside the
/// freeze. A lost write costs one measurement.
#[cfg(unix)]
pub(crate) fn keep_landed_park(
    parked_in: std::time::Duration,
) -> Option<std::thread::JoinHandle<()>> {
    let build = crate::running_build_number();
    // Nobody waits on it, but a Commit's `_exit` cuts it short, so it runs at Background,
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

/// The child-only duplicate of every live master (close-on-exec, at 3 or above, owned by
/// the caller) beside the `(local_id, duplicate, pid)` table that names them, so the
/// original can close or reuse its number freely. The first duplicate with no room
/// (`EMFILE`) is the error, and the ones made before it are closed again.
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

/// The detached ledger writers a test build has in flight, so
/// [`crate::app_update_screen::hold_update_ledger_for_test`] can wait them out before it
/// hands the per-process ledger lock to the next test.
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

    /// Block until no detached ledger writer is in flight; a one-minute hang detector,
    /// since at Background QoS a loaded machine can starve a writer for seconds.
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

/// Whether one session may spend `optional` on a budget on top of its `own_mandatory`,
/// with `used` charged so far and `later_mandatory` reserved for the sessions not yet
/// admitted, so early scrollback cannot starve a later visible screen. Overflow is a
/// refusal, never a wrap.
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

/// Why the park's capture could not produce a committed screen set: timing or this
/// process's storage, retried; or `Refused`, deterministic for this desk
/// ([`classify_capture_failure`]).
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CaptureFailure {
    /// The capture outran the freeze budget.
    Deadline { freeze_ms: u128 },
    /// A session's engine stayed locked for the whole capture window.
    EngineBusy,
    /// This process could not reserve the capture's bounded storage.
    Storage,
    /// A session's parser was parked mid-sequence with the rest still queued on its PTY
    /// (over a quiet PTY the sequence is abandoned and the screen carried). Timing, never a
    /// refusal, and never carried: the successor's fresh parser would print the queued tail
    /// as text. The launched lane re-parks on the same rung ([`mid_sequence_miss_disposition`]);
    /// the fork lane files a miss. Names only a partial sequence no carry holds
    /// (`Terminal::partial_sequence_uncarried`, `Terminal::partial_sequence_swallowed`,
    /// `Terminal::output_state_in_flight`); the rest ride the checkpoint.
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

/// Why the overlap handoff cannot run in this process. One predicate
/// ([`App::seamless_handoff_unavailable`]) serves the apply gate and the status bar's
/// posture, so the bar never promises what the gate refuses. With any of these only the
/// cold lane is admitted, at zero live PTYs. Every reason is a fact about the launch or the
/// process; there is no opt-out. A fixed control socket the process owns is no reason: the
/// candidate's control worker binds only after Commit and the parent's exit.
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

/// Under `cfg(test)` the prelaunched park stamps its deadline from a one-minute hang
/// detector ([`park_budget_for_test::deadline_budget`]), since the freeze budget is a
/// product constant a loaded debug build misses; a test whose subject is a miss holds
/// [`park_budget_for_test::real`]. Per thread, so no parallel test sees it.
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
    /// [`HandoffUnavailable::ALL`]'s order, or `None`. Read by the apply gate and the status
    /// bar's posture; plain reads only, no filesystem probes. Not folded into
    /// `arm_native_auto_apply`'s `enabled`: the cold lane is a real automatic path once the
    /// last terminal of such a process has closed.
    #[must_use]
    pub(crate) fn seamless_handoff_unavailable(&self) -> Option<HandoffUnavailable> {
        HandoffUnavailable::ALL
            .into_iter()
            .find(|why| why.holds_for(self))
    }
}

impl App {
    /// Applies a staged update now by re-executing; the staged build swaps in at the top of
    /// the new `main` (`apply_staged_if_ready`). Hands every live PTY master, its screen
    /// checkpoint, scrollback (`crate::handoff_history`) and a `SessionHandoff` manifest to
    /// the new process so shells survive; a cold relaunch only when no foreground job would
    /// be destroyed. Every path that leaves this process alive returns an actionable failure
    /// so the updater can re-arm the stage.
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
        #[cfg(target_os = "macos")]
        if self.askpass_sheet.is_some() {
            return Err(crate::UpdateHandoffStartError::activity(
                "a sudo password prompt is still open",
            ));
        }
        // Its session is not registered yet, so neither the handoff manifest
        // nor a cold replacement can carry the tab, its input or its waiter.
        if !self.tab_spawns.is_empty() {
            #[cfg(unix)]
            return Err(crate::UpdateHandoffStartError::activity(
                "a new tab is still starting",
            ));
            #[cfg(not(unix))]
            return Err(crate::UpdateHandoffStartError::refused(
                "a new tab is still starting",
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
            // The row is cleared here: `start_unix_update_handoff` shows "installing…"
            // before building the carry, and several of its `Err` exits send no completion.
            let started = self.start_unix_update_handoff(
                exe,
                build,
                safety_token,
                mode,
                apply_attempt,
                same_image,
            );
            // A person's press held for the candidate's read is no stop: its row keeps
            // saying it installs and its switch stays on record, for the press that runs
            // when the read ends (`App::run_held_person_apply`).
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
        // Windows has no exec(2): spawn the new process, then exit.
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
                // ConPTY has no foreground-pgrp proof: every live session is unknown.
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
            // The message log's last write from this process: the `process::exit` after a
            // successful spawn runs no destructor, so the writer thread's queue would die
            // with it.
            self.flush_messages_log_within(crate::messages_host::MESSAGES_FLUSH_EXIT_WAIT);
            let spawn = || {
                // `spawnfd::command` gives a console successor this process's console or a
                // windowless one, never a visible console window of its own.
                let mut cmd = aterm_uds::spawnfd::command(exe);
                // Forward argv minus the leading `--window` pins earlier boot swaps
                // prepended, so no relaunch path can re-grow the argv
                // (`aterm_update::reexec_forwarded_args`).
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
                    // A failed spawn leaves this process live.
                    aterm_log::warn!("update apply: re-spawn failed: {err}");
                    return Err(crate::UpdateHandoffStartError::failed(format!(
                        "replacement process could not start: {err}"
                    )));
                }
                Err(refusal) => {
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

    /// The cold lane's steps before the commit permit: the switch on record and the
    /// message log's last write. Each is undone or harmless if the `exec` does not
    /// happen ([`Self::resume_after_cold_exec`]).
    #[cfg(unix)]
    pub(crate) fn prepare_cold_exec(&mut self, build: u64, same_image: bool, automatic: bool) {
        self.note_update_switch_started(build, same_image, automatic);
        // The message log's last write from this process: `exec` runs no
        // destructor. The writer stays open for a failed `exec`. Bounded, since a
        // saturated machine can starve the utility writer thread while the window
        // is frozen.
        self.flush_messages_log_within(MESSAGES_FLUSH_COMMIT_WAIT);
        // `exec` runs no destructor, so the consent sentinel's `log stream` child is
        // signalled now (its `--timeout` bounds it anyway).
        self.consent_observer.stop();
    }

    /// The cold `exec`, under the resident operator's commit permit when there is one,
    /// retiring the control socket last inside it, so a refused permit leaves every
    /// connection served. `Ok` carries the error of an `exec` that returned, `Err` the
    /// permit's refusal; [`Self::resume_after_cold_exec`] follows either.
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
    /// path with its token if it was retired.
    #[cfg(unix)]
    pub(crate) fn resume_after_cold_exec(&mut self) {
        if !self.control_endpoint.reopen() {
            aterm_log::warn!(
                "update apply: the control socket could not be reopened after the exec \
                 returned; its watchdog rebinds it"
            );
        }
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
    #[expect(
        clippy::disallowed_types,
        reason = "the update successor's raw Command: it hands over exactly its own descriptors, which a strip at the exec would undo"
    )]
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
        // This epoch gates entry only; mid-flight revocation is `pending.activity_epoch`,
        // read at Commit. So `AutomaticPastGrace` skips the quiet wait but can still be
        // revoked.
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
        // Without the overlap only the cold lane remains, admitted at zero PTYs, so with
        // any terminal open the apply is refused; the reason is logged by name.
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
        // What the token lets ride must be carried by the lane that runs: a cold exec hands
        // no layout over, and an older successor drops Settings drafts
        // (`lane_carry_refusal`). The one target build of the attempt is settled here, so
        // no two steps can disagree on whether it is older.
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
                // The verdict first, before anything is written or replaced: the cold
                // lane's successor is this build, whose boot swap runs the complete gate,
                // and a refusal there comes back as a fresh desk that would re-arm and
                // exec again every few seconds.
                self.cold_lane_verdict_gate(mode, apply_attempt.as_ref())?;
                // A harness's lifeline crosses the exec on a descriptor kept open for it;
                // the number it was launched with is `/dev/null` or closed by now.
                let lifeline_fd = crate::lifeline::relaunch_descriptor();
                let mut command = std::process::Command::new(exe);
                command
                    // Exactly one `--window` pin, so the router cannot make the successor a
                    // nested session; no windowing verb or stale lifeline number
                    // (`running_window_successor_args`); the `-e` command rides for new
                    // tabs, marked as already run (`RELAUNCH_OF_RUNNING_ENV`).
                    .args(aterm_update::running_window_successor_args(
                        std::env::args_os().skip(1),
                        lifeline_fd,
                    ))
                    .env("ATERM_UPDATED_FROM", build.to_string())
                    .env(aterm_update::RELAUNCH_OF_RUNNING_ENV, "1");
                // Same rule as the seamless lane below: an activation binds no expected
                // artifact. The successor swaps nothing; binding the activation digest
                // would make its `apply_staged_if_ready` refuse a newer `ready.toml` as
                // "no longer matches" and write a spurious apply refusal into the ledger.
                let bound = apply_attempt
                    .as_ref()
                    .filter(|attempt| !attempt.is_installed_activation());
                bind_expected_update_artifact(&mut command, bound);
                // …and what this lane has booked against those bytes, so a relaunch whose
                // boot swap refuses again books that refusal where the schedule stands
                // instead of as a first failure (`ColdLaneBudget`).
                bind_cold_lane_budget(
                    &mut command,
                    bound,
                    bound.map(|attempt| self.cold_lane_budget(attempt, std::time::Instant::now())),
                );
                // …and what its warnings had said, so the landing records their healing.
                bind_cold_lane_health_said(&mut command, &self.carried_health_said());
                // `--headless` rides the argv. Document and Settings tabs are left as the
                // quit layout the successor reads at boot, or the lane refuses.
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
                // The write waits a bounded time for the restore lock: this is the GUI
                // main thread. A layout written for an exec that then does not happen is
                // taken back, this run's desk alone (`App::take_back_cold_lane_layout`).
                let left = leave_cold_lane_layout(layout.as_ref(), |layout| {
                    crate::restore::write_quit_within(
                        layout,
                        run.as_deref(),
                        COLD_LANE_LAYOUT_LOCK_WAIT,
                    )
                })?;
                // This run's desk, written again, replaces the one a take-back still owed
                // from an earlier attempt. With no newer desk of this run written (nothing
                // to leave, or a layout not kept), the successor would reopen that stale
                // one at its boot: the owed take-back is tried first, as a park tries it.
                if left == LayoutLeft::Written {
                    self.cold_lane_layout_rewritten();
                } else if let Err(refusal) =
                    self.settle_owed_layout_take_back_before_switch(std::time::Instant::now())
                {
                    return Err(crate::UpdateHandoffStartError::refused(refusal));
                }
                // The launcher's soft descriptor limit crosses this exec as it crosses the
                // fork lane's: `execve` keeps a limit a claim or a park raised, and the new
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
                // Tabs not kept go on record once the switch is granted, just before the
                // steps whose message-log flush carries it. Recorded before the quiesce,
                // every attempt a busy operator turned back would say the new version
                // "starts without them" while the old one ran on, one record per retry.
                if let LayoutLeft::NotKept(cause) = &left {
                    self.say_cold_lane_tabs_not_kept(target_build, cause);
                }
                self.prepare_cold_exec(build, same_image.is_some(), mode.is_automatic());
                // `exec` returns only on failure.
                let why = match self.run_cold_exec(operator_quiesce.as_ref(), || {
                    // The table the successor gets: the lifeline, inheritable for the
                    // `exec` alone (close-on-exec again if it returns, so nothing this
                    // process spawns next holds it), and nothing else this process holds
                    // (`cold_exec_hand_over`).
                    let _lifeline = cold_exec_hand_over();
                    // An older target starts at the limit this process held before a park's
                    // raise: lowered for the `exec` alone, after the lifeline counts, and
                    // raised back if the `exec` returns.
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
        // A take-back still owed from a cold attempt that did not `exec` is settled now: a
        // stale desk left in `session.toml` would be reopened by the next cold launch.
        if let Err(refusal) =
            self.settle_owed_layout_take_back_before_switch(std::time::Instant::now())
        {
            return Err(crate::UpdateHandoffStartError::refused(format!(
                "{refusal}; the terminal was left untouched"
            )));
        }

        // The staged candidate's pre-verification (codesign, sealed rebinding) is an
        // optimization, never a grant: the child re-runs the complete gate at swap time. It
        // is unbounded disk work, so it runs on the worker; a cached pass from
        // `spawn_staged_handoff_preverification` skips it, and a cached refusal
        // short-circuits the automatic attempt here.
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
            // The wait for a roster confirmation is no verdict: held for the check, never
            // booked on the transient budget, as the cold lane's gate holds it.
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
        // Room for the park before either lane reserves anything: a Dock-started window
        // runs at launchd's soft 256, and duplicating every master passes it at about 60
        // sessions. Only toward a target no older than this build, which reads the
        // launcher's limit it is told (`start_older_image_at_its_limit`).
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

        // Apply begins on the status bar before the carry is built, so the successor
        // inherits the row count and words. An explicit apply adds a row if none is up;
        // the automatic lane never does (its SIGWINCH would read as activity) and the
        // upgrade rim explains the frozen frame. Every refusal from here restores both.
        self.begin_update_installing(matches!(
            mode,
            crate::native_updater_service::ApplyMode::Immediate
        ));
        // On record before the park: the park's budget has no room for a file write.
        self.record_update_switch_started(build, same_image.is_some(), mode.is_automatic());

        // Probed before the park: resolving the control directory can create and `chmod`
        // it, which must stay out of the frozen interval. The answer depends only on
        // `$HOME` and this pid, so it cannot change meanwhile.
        #[cfg(target_os = "macos")]
        let rendezvous_path_fits = crate::handoff_rendezvous::rendezvous_path_fits();
        // Same reasoning, same window: `app_bundle_root` ends in an `is_dir`.
        #[cfg(target_os = "macos")]
        let bundle = app_bundle_root(&exe);

        // Spawned before any reader parks, so a failed thread creation leaves the terminal
        // untouched.
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

        // Everything up to the park depends only on facts settled before it (only the two
        // digests are taken under it). No reader is parked yet, so an early return drops
        // `job_tx` and the worker exits. An attempt dying after its id is minted burns one;
        // nothing keys on contiguity.

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
            // One `--window` pin and no windowing verb, as on the cold lane: an unpinned
            // candidate of a shell-started window would route itself to a terminal session
            // and never dial. No lifeline rides this lane (it is `--headless`'s).
            .args(aterm_update::running_window_successor_args(
                std::env::args_os().skip(1),
                None,
            ))
            .env("ATERM_UPDATED_FROM", build.to_string())
            .env(aterm_update::RELAUNCH_OF_RUNNING_ENV, "1");
        // An activation binds no expected artifact: the successor has nothing to swap (its
        // `apply_staged_if_ready` finds no newer stage and returns NoUpdate) and it is the
        // authorized build; the identity check below still names it.
        let installed_activation = apply_attempt
            .as_ref()
            .is_some_and(|attempt| attempt.is_installed_activation());
        bind_expected_update_artifact(
            &mut command,
            apply_attempt.as_ref().filter(|_| !installed_activation),
        );
        // The lane is decided here and nowhere else, before the pending attempt is
        // recorded: it selects the term the adoption proof hashes, which the main thread
        // re-derives at Commit, so a lane chosen later would split one proof across two
        // terms. Decided on the live snapshot: `fstat` answers the same for a master and
        // its duplicate. The chunked-grant fact is read once, from a passed verification
        // on record.
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

        // The registry projection can fail (`WouldBlock`), so it is taken before parking.
        // Hold serials first: a hold or a direct reset moving them after this read refuses
        // the Commit (a lossless retry), and none lands after the Commit's fenced read
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
        // (`session_store::CarriedClaims`).
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
        // The freeze budget is keyed by the target build, plus this lane's own park misses
        // (`App::fork_park_misses`), which are not physical failures. Past half of it the
        // remaining sessions carry only their visible screen.
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
        // An attempt (a person's included: held, then run again once the read ends) first
        // waits for the policy while it is read, bounded by the read's ceiling: the capture
        // runs on this thread, and a projection that panics or hangs could otherwise never
        // be routed around.
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
        // The instant the terminal stops echoing: the start of the user's freeze, and of
        // the rim that explains it.
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
        // The post-park re-check tolerates output but not session death, which makes the
        // proof's live set stale: activity. The activity epoch cannot move here (every bump
        // is on this thread).
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

        // Captured post-park, once every `term` proved lockable, so cwd/title cannot
        // degrade into a false topology change.
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
            nonce: None,
            live: live.clone(),
            // The proof identities, in whichever term this attempt's lane can prove
            // (`HandoffCapture::proof_identities`). Never `live`.
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
                // Exported by the worker after this capture, never beside the park;
                // withheld under a ceiling with no scrollback.
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
        // A live `video` take is not answered here: the worker may still fail. Commit
        // answers it.
        Ok(())
    }

    /// The sessions an overlap handoff hands over, as `(local_id, master, pid)`: every
    /// pooled session except a pane kept open after its command exited (`aterm --hold`),
    /// whose hung-up master would read as a session dying under every attempt. Read once per
    /// decision and passed down, so the gate, capture, descriptors and manifest name one set.
    #[cfg(unix)]
    pub(crate) fn handoff_live_sessions(&self) -> Vec<(u64, i32, i32)> {
        let exited = self.handoff_exited_session_ids();
        self.pool
            .iter()
            .filter(|session| !exited.contains(&session.id))
            .map(|session| (session.id, session.master, session.pid))
            .collect()
    }

    /// The window facts the successor sizes its first window from: the front window's
    /// grid, origin, inner size and presence row, the message band's rows and messages, the
    /// font zoom, and when the build was verified.
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

    /// [`Self::handoff_window_carry`] at a park, noting which messages crossed for
    /// [`Self::close_uncarried_messages`] at the Commit.
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

    /// The advisory warm hint a launched successor starts with: what
    /// [`Self::handoff_window_carry`] will write, read at launch. Stale by design: a zoom
    /// during the hold costs one warm miss, corrected at the backend join.
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

    /// The live font zoom as the window carry writes it, `(px, reset px)` in thousandths of
    /// a px, only while zoomed: an unzoomed size is what the successor derives from the same
    /// config and display. Always a pair: the size to draw at and the one Cmd-0 returns to.
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

    /// The pooled sessions whose command has exited, which
    /// [`Self::handoff_live_sessions`] leaves out; a registry read, never a `Terminal` lock.
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

    /// Starts the launched lane's history export for every handed session, with readers
    /// live; `None` for [`Self::history_sessions`]'s reasons, a worker that cannot start,
    /// or a `ceiling` with no scrollback (the park stops one started under an unread
    /// policy, [`Self::withhold_history_export`]).
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

    /// Stops and removes the launched lane's history export under a `ceiling` with no
    /// scrollback ([`crate::handoff_history::HistoryPlan::Withheld`]).
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

    /// Every pooled session's engine for the history export, or `None` for a successor
    /// older than this build or no private control dir.
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
        // Every pooled session in id order, handed and held apart: a held pane's export
        // only takes room the handed sessions do not need.
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

    /// The successor's half of the history carry, at Commit: imports every adopted
    /// session's carry on a worker, never the event loop.
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

    /// Says what the history imports left behind once they settle, in one band row, posting
    /// only a change from [`Self::say_carried_history_loss`].
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

    /// Posts the history loss the parent counted, inside the Commit's re-grid batch, for
    /// [`Self::settle_handoff_history`] to compare against.
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

    /// Whether a cosmetic partial state (an open OSC 8 link, a swallowed picture) may still
    /// miss the park: only within [`PRELAUNCH_MAX_MID_SEQUENCE_REPARKS`] on the launched lane.
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

    /// The capture ladder, under the park: every session's screen and as much scrollback as
    /// the budgets allow, plus the control carry's head. Each session is carried at the most
    /// faithful rung `caps` and `ceiling` admit, at worst blank, so `Err` is only timing,
    /// storage, or a refusal about no single screen; the caller rolls the readers back on
    /// `Err`.
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
        // The history heads are not best-effort: a session without one would lose its
        // history uncounted.
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
        // Mandatory first: the whole pool's visible and alt grids are reserved before any
        // scrollback is charged (`optional_carry_fits`). Readers are parked, so geometry
        // cannot move between the two passes.
        let mut cells_reserve = 0_u64;
        let mut bytes_reserve = 0_u64;
        // Only the handed sessions: a screen with no descriptor on the wire is refused by
        // `write_outgoing`.
        let handed = |id: u64| live.iter().any(|(local_id, _, _)| *local_id == id);
        // In session-id order, not the pool's hash order, so the same desk is priced the
        // same way every time.
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
            // A parser parked inside a sequence no carry holds, over queued output, is a
            // miss: the successor's parser would print the queued tail as text. Over a quiet
            // PTY the sequence is abandoned. An open link or a swallowed picture is cosmetic,
            // so its miss spends only the launched lane's free mid-sequence re-parks
            // (`soft_miss_may_repark`); past them, and on the fork lane, the screen is carried
            // with the link closed or the picture's frame dropped (the stream's next replaces
            // it). Every other partial sequence keeps the hard miss on every lane.
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
            // Scrollback is best-effort: depth is chosen per session against the remaining
            // time and the budget left after the pool's mandatory reservation.
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let (rows, cols) = (terminal.rows(), terminal.cols());
            let own_cells = crate::seamless::mandatory_checkpoint_cells(rows, cols);
            let own_bytes =
                crate::seamless::checkpoint_capture_budget_bytes(rows, cols, 0).unwrap_or(u64::MAX);
            // Both reserves now cover only the sessions after this one, so this
            // session's scrollback is priced against what the rest still need.
            cells_reserve = cells_reserve.saturating_sub(own_cells);
            bytes_reserve = bytes_reserve.saturating_sub(own_bytes);
            // A ceiling below Full carries no scrollback, so none is priced either.
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
            // The decode-authority budget is the producer's own ceiling, so it chooses a
            // rung but never refuses: a screen past it is carried blank.
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
            // A refused history (`VisibleOnly`) lowers every later session's too: the budget
            // or shape that refused it is the pool's.
            if history != 0 && rung == CarryRung::VisibleOnly {
                history_latched_off = true;
            }
            capture_budget = crate::seamless::checkpoint_capture_budget_bytes(
                checkpoint.rows,
                checkpoint.cols,
                checkpoint.history_lines,
            )
            .map_or(u64::MAX, |bytes| capture_budget.saturating_add(bytes));
            // The control carry's share of the freeze, under the checkpoint's lock; the
            // differ's state only within half the budget and on a rung that keeps it.
            let differ = deadline.saturating_duration_since(std::time::Instant::now())
                >= handoff_history_comfort;
            // The foreground holder, for every session and apart from the control carry,
            // so a job that died during the handoff is handed back even without it.
            if holder_room {
                fg_holders.push((
                    session.id,
                    session.fg_holder.load(std::sync::atomic::Ordering::Acquire),
                ));
            }
            // The render debt the engine owes this session's alternate screen, under the
            // checkpoint's lock; none for a blank carry or the history-backed main screen.
            if debt_room && rung != CarryRung::Repaint && terminal.modes().alternate_screen {
                let debt = terminal.render_debt();
                if debt != (0, 0) {
                    render_debts.push((session.id, debt));
                }
            }
            // The history head, under the checkpoint's lock: the fence the worker joins this
            // session's export to.
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
        // A session the self-check lowered to a clamped or blank screen loses its differ's
        // state too.
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
        // The held panes, last and optional, never a reason to miss the park
        // (`held_capture_deadline`).
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

    /// The visible screen of every pooled session not in `live` (exited panes), without
    /// scrollback, which crosses in its own sidecar joined against the fence taken here.
    /// Best-effort, one `try_lock` per engine: a busy engine, a screen that would go blank,
    /// or one reached after the deadline is left a placeholder; bounded by
    /// [`crate::seamless::MAX_HELD_PANES`] and
    /// [`crate::seamless::MAX_HELD_AGGREGATE_GRID_BYTES`]. Each pane adds its control carry
    /// (no differ state) to `carries`, so its turns still read in the successor.
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
            // The held screen is the visible rows only; what of its scrollback does not
            // cross is counted on the successor's band.
            let history_dropped =
                u64::try_from(terminal.main_grid().scrollback_lines()).unwrap_or(u64::MAX);
            // …and its fence, which its export is joined against when the manifest is
            // written, so its scrollback crosses as a handed session's does.
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

    /// The dry run: the park's own capture over the same handed set, timed with every
    /// reader live, so the first freeze rung is budgeted from this desk's cost. Nothing is
    /// parked or kept, it is bounded by [`DRY_RUN_CAPTURE_WINDOW`], and it runs under the
    /// successor's policy ([`Self::dry_run_ceiling`]) or not at all.
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
            // A mid-sequence parser is ordinary while readers run; none of these says how
            // long a capture takes.
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

    /// The carry ceiling the dry run captures under, the same policy every park reads:
    /// `Full` with no artifact, `None` while the policy is unread.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    fn dry_run_ceiling(&self) -> Option<aterm_update_core::handoff_policy::CarryCeiling> {
        let attempt = self
            .update_handoff_prelaunch
            .as_ref()
            .and_then(|prelaunch| prelaunch.apply_attempt.as_ref());
        let policy = self.park_policy(attempt);
        (attempt.is_none() || policy.known).then(|| policy.carry_ceiling())
    }

    /// Seeds this attempt's first automatic freeze rung from the dry run (never below the
    /// last landed park), kept in the health ledger off this thread, else from the ledger,
    /// else 20 ms. Returns the seed with the ledger writer's handle for tests to join.
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
                // In the ledger's unit, whole microseconds, so the rung seeded now is the
                // one the kept record seeds next time.
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

    /// The prelaunched attempt's budget for its next park, as `(prior failures incl.
    /// misses, budget, seed)`; the park reads its budget only from here.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn prelaunched_park_budget(&self) -> Option<(u8, std::time::Duration, FreezeSeed)> {
        let prelaunch = self.update_handoff_prelaunch.as_ref()?;
        let prior = self
            .prior_physical_failures_of(prelaunch.apply_attempt.as_ref())
            .saturating_add(prelaunch.park_misses);
        let budget = handoff_freeze_budget(prelaunch.mode, prior, prelaunch.freeze_seed);
        Some((prior, budget, prelaunch.freeze_seed))
    }

    /// The launched lane's first half: records the attempt and hands the worker what it
    /// needs to launch and hold the successor, returning with every reader live; the park
    /// comes when the worker reports the dial and the gate admits.
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
        // Written as the layout sidecar while the successor boots, then checked by the
        // park (`handoff_diet`): a degraded read costs a rewrite, never a wrong sidecar.
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
        // While the successor boots, time the park's capture with readers live and seed
        // the first freeze rung.
        let _seeded =
            self.seed_first_freeze_rung(mode, crate::seamless::WireCaps::for_target(target_build));
        Ok(())
    }

    /// `Wake::UpdateHandoffAwaitingPark`: the worker holds the successor's dial (or must
    /// fork), so the park may be taken when the gate admits.
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

    /// The park gate for a prelaunched attempt, re-run every [`PRELAUNCH_PARK_RETRY`] while
    /// the automatic lane waits for a quiet moment, bounded by [`PRELAUNCH_HOLD_MAX`].
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
        // A park that did not hand its capture over resumes the export it paused, or the
        // re-park would meet a stale fence (`Outrun`).
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
            // A refused capture is not a missed park: no rung changes it, so the successor
            // stands down as `CaptureRefused`, retried once the desk changes.
            ParkAttempt::Refused(refusal) => {
                aterm_log::warn!(
                    "update apply: {refusal}; a wider freeze rung cannot change that, so the \
                     held successor stands down and the lane retries once the desk changes"
                );
                self.stand_down_prelaunched_successor(capture_refusal_stand_down(&refusal));
            }
            // A missed park is not a failed update: re-park on the next rung, and once
            // every rung is tried stand down as a busy machine (`park_miss_disposition`).
            ParkAttempt::Missed(reason) => {
                // Every miss is pre-record (the parked attempt is stored after the last
                // point that can miss), so re-parking under the same nonce is sound.
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

    /// While the gate holds the park back with readers live, a rim a missed park started
    /// leaves (it would pulse around a working terminal); the next admitted park starts it
    /// again.
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

    /// `Wake::UpdateHandoffStandingDown`: the worker is about to revoke a held, ungranted
    /// successor and waits for this answer. On the main thread beside the park gate, so no
    /// park is in progress: closes the gate for good and resumes the readers of a landed
    /// park, so the user does not wait out the grace and reap frozen. The attempt record
    /// outlives both until the completion, so no retry launches beside the candidate.
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
        // Answered last, after the gate is shut and readers are back, since the worker
        // revokes on this.
        if let Some(ack) = ack {
            let _ = ack.try_send(());
        }
    }

    /// The launched lane's second half: the park itself. Everything that needs stopped
    /// readers happens here and crosses to the worker as one [`HandoffTransferJob`].
    /// `NotYet` means try again shortly; every `Failed` after `park_all_readers` has rolled
    /// the readers back.
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
        // The successor's signed handoff policy, re-read at every gate run since the
        // worker's verdict may have landed; it only lowers what this park carries.
        let policy = self.park_policy(apply_attempt.as_ref());
        let ceiling = policy.carry_ceiling();
        // A ceiling carrying no scrollback stops the history export now, before
        // the gate waits on its first pass (`HistoryPlan::Withheld`).
        if !ceiling.carries_scrollback() {
            self.withhold_history_export(target_build, ceiling);
        }

        // Sessions with a live process only, so a hung-up master cannot hold or fail the
        // park.
        let live: Vec<(u64, i32, i32)> = self.handoff_live_sessions();
        if live.is_empty() {
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
                detail: "every terminal session closed while the successor booted".to_string(),
            });
        }
        // The scrollback export's first pass (the held panes' included) comes first, with
        // readers live, bounded by `handoff_history::EXPORT_PATIENCE` after the dial;
        // sessions it has not reached carry the screen carry's bounded history, counted.
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
        // The gate, the only decision that costs the user anything, is about idleness;
        // session death is re-checked under the park.
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
        // Consecutive `Land` waits, counted here where the gate is read: any other answer,
        // or any other phase, starts the count over.
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
        // Pause the export before anything freezes, so it does not contend with the
        // capture's locks; later output rides the screen carry. Paused, not stopped: a
        // park that does not hand the capture over leaves it to resume.
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
        // This process's own limits are not a verdict about the candidate
        // (`ProducerFailed`): here, more sessions than the dialled claim's grant carries.
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
        // The registry projection can fail (`WouldBlock`), so it is taken before parking;
        // hold serials first, as on the fork lane.
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
        // (`session_store::CarriedClaims`).
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
        // receiver's own numbers), kept if the attempt must fork.
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
        // The ladder plus this attempt's own misses: a missed park re-parks on the next
        // rung without giving the booted successor back.
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
        // The instant the terminal stops echoing: the start of the user's freeze. The rim
        // starts here, not at apply begin, since everything before ran with readers live;
        // a re-park keeps the rim the first park started, its cap counted from this park.
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
                // A parser mid-sequence is cured by the next batch boundary, not a wider
                // rung ([`mid_sequence_miss_disposition`]).
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
        // Post-park, as on the fork lane: before the park a reader's term lock can degrade
        // the layout into a false topology change.
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
            nonce: Some(nonce),
            live: live.clone(),
            // The proof identities, in the device term (`HandoffCapture::proof_identities`).
            // Never `live`.
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
                // Taken after every point a park can miss; handed over paused, withheld
                // under a ceiling with no scrollback.
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
        // A live `video` take is not answered here, since the proof may never come; Commit
        // answers it (`App::video_answer_before_commit`).
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

    /// The pre-Commit input-drain gate: `None` when the completion was re-posted for
    /// another spin, otherwise the completion with the `input_dispatch_fenced` and
    /// `egress_settled` facts. Hardware events not yet dispatched would die with `_exit`, so
    /// Commit waits across a fence counted in loop iterations, and for every handed
    /// session's egress to reach the kernel; bounded by the 3 s drain deadline, failing
    /// closed. No quiet window is waited for: the successor's own keys would hold it open.
    #[cfg(unix)]
    fn handoff_drain_gate(
        &mut self,
        completion: crate::UpdateHandoffCompletion,
    ) -> Option<(crate::UpdateHandoffCompletion, bool, bool)> {
        const HANDOFF_INPUT_DRAIN_SPIN_CAP: u32 = 4_000;
        /// Mandatory minimum event-loop time between ProofReady and Commit.
        const HANDOFF_INPUT_DISPATCH_FENCE: std::time::Duration =
            std::time::Duration::from_millis(30);
        /// Absolute wall-clock backstop. An egress queue that never settles must fail
        /// closed (rollback), never `_exit` over undelivered bytes.
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
        // The fence needs both a completed re-post (so the loop really iterated) and the
        // elapsed floor.
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
                // The loop is closing. A held claim has nobody else to finish it (the
                // worker cannot reject a claimed attempt), so its admission is read now,
                // unfenced, and refuses it.
                Err(respin) if held => Some((*respin, false, egress_settled)),
                // Otherwise the dropped completion's reject sender makes the
                // worker reject and reap.
                Err(_) => None,
            };
        }
        Some((completion, input_dispatch_fenced, egress_settled))
    }

    /// Whether a handoff completion can go back through this thread's event loop
    /// ([`Self::repost_handoff_completion`]).
    #[cfg(unix)]
    fn handoff_loop_present(&self) -> bool {
        #[cfg(test)]
        if self.handoff_reposts.is_some() {
            return true;
        }
        self.proxy.is_some()
    }

    /// Post `completion` back through this thread's event loop, which dispatches queued
    /// input first; `Err` hands it back when the loop is closing.
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

    /// The Commit's messages flush, once per attempt before the claim (what it lands is
    /// lost to no rollback): `true` when it held the main thread a fence grain
    /// ([`HANDOFF_INPUT_DRAIN_YIELD`]) or more, so input may have queued behind it and the
    /// attempt goes through the drain gate again.
    #[cfg(unix)]
    fn commit_flush_refences(&mut self) -> bool {
        if self
            .pending_update_handoff
            .as_ref()
            .is_none_or(|pending| pending.commit_flush.is_some())
        {
            return false;
        }
        // With no writer the flush waits on nothing, so its hold is none whatever the
        // scheduler did.
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

    /// Keeps the writer's answer (`landed`) on the attempt, and restarts the fence's clock
    /// when the flush `held` the main thread a fence grain or more.
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

    /// The pairs this process's crash journal keys its handed shells by, while that journal
    /// still holds the image this process took over at its own Commit: each session's
    /// `journal_key` from the parent, else the parent's own pair. Empty otherwise.
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

    /// Every pooled session's hold serial, summed: the counters only grow, so any hold
    /// moving changes the sum. One short fabric lock each, never under a `Terminal` or the
    /// registry.
    #[cfg(unix)]
    pub(crate) fn hold_serials(&self) -> u64 {
        self.pool
            .iter()
            .map(|session| session.ctx.fabric.carry_serial())
            .fold(0, u64::wrapping_add)
    }

    /// Whether every handed session's [`crate::session_store::CarriedClaims`] (topics,
    /// keyed escalations, supervisor claim) still reads as the park drew it (`drawn`); a
    /// session no longer pooled reads as moved. One short meta lock and one topics lock
    /// each, never under a `Terminal` or the registry.
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

    /// Whether any session's timeline or inbox has handed out an id past the manifest's
    /// last id plus its overlap reserve (`past_handoff_ceiling`): the successor would reuse
    /// it, so the Commit stands down. Read after the fence.
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

    /// [`Self::hold_serials`] as the Commit reads it, each under the guard that fences it
    /// ([`crate::fabric::SessionFabric::fence_hold`]), so no `hold` lands unseen before the
    /// exit; the fence lifts when the guard drops. For a same-or-newer successor
    /// (`successor_reads_carry`) the fence also holds the bridge's retirements back; for an
    /// older one they are answered here, or a sequence pin would swallow its own post.
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

    /// Collects every Commit admission fact ([`HandoffCommitFacts`]) with the native-safety
    /// evidence, the adoption proof and the arbiter; `None` when the attempt vanished.
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
        // A hold that moved since the park is structural activity. Read under the fence
        // (`fence_hold_serials`), which holds later holds back; a held claim reads them
        // under the fence it raised at the claim.
        let (commit_hold_serials, hold_fence) = match held_fence {
            Some(fence) => (fence.serials(), fence),
            None => self.fence_hold_serials(successor_reads_fabric_carry(
                pending_target_build,
                crate::running_build_number(),
            )),
        };
        // More turn ids minted in the overlap than the manifest's reserve would share one
        // with the successor: a lossless retry.
        let turn_ids_held =
            !crate::control::turn_ids_past_handoff_ceiling(arbiter.turn_id_ceiling());
        if !turn_ids_held {
            aterm_log::warn!(
                "update apply: this process minted more turn ids since the handoff manifest \
                 than its reserve holds; standing the Commit down so no id is minted twice"
            );
        }
        // A topic, escalation or claim that moved since the park's draw is in no manifest:
        // a lossless retry. Read after the fence, under which no `topic` change lands
        // (`fabric::cmd_topic`).
        let claims_held = self.carried_claims_held(&pending_carried_claims);
        if !claims_held {
            aterm_log::warn!(
                "update apply: a session's topics, escalations or supervisor claim moved since \
                 the handoff manifest was drawn; standing the Commit down so the next attempt \
                 carries them"
            );
        }
        let exact_activity = self.update_handoff_activity_epoch == pending_activity_epoch
            // A prelaunch can capture its epoch after a tab was queued. The
            // epoch alone cannot attest that unfinished tab's ownership.
            && self.tab_spawns.is_empty()
            && {
                #[cfg(target_os = "macos")]
                { self.askpass_sheet.is_none() }
                #[cfg(not(target_os = "macos"))]
                { true }
            }
            && commit_hold_serials == pending_hold_serials
            && !self.ids_past_handoff_ceiling()
            && turn_ids_held
            && claims_held;
        // The live set as the park drew it: an exited pane left out stays out, a handed
        // session that exited during the overlap stays in for `sessions_alive` to name.
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
        // Compare topology, not the raw capture (`commit_layout_topology`); a raw
        // difference is only logged.
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
        // Death-only peek: queued output is tolerated (it waits in the kernel for the
        // child); HUP/ERR means the adopted live set is stale. A program that exits with
        // output still queued waits in the tty drain, so its master shows no hangup and the
        // Commit lands; the successor closes that pane after the keys typed into it replay.
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

    /// Main-thread completion of the overlap proof: only a fresh admission over unchanged
    /// sessions and native state authorizes the destructor-free parent exit.
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
            // A stale proof never authorizes a Commit: its worker rejects and reaps.
            let attempt_id = completion.attempt_id;
            let _ = deliver_handoff_rejection(completion.reject);
            aterm_log::warn!("update apply: ignored stale handoff completion {attempt_id}");
            return;
        }
        if completion.outcome == crate::UpdateHandoffOutcome::ProofReady {
            self.finish_proven_handoff(completion);
            return;
        }

        // Every non-ready completion comes after the worker killed and reaped the child, so
        // the readers may resume.
        let Some(teardown) = self.reduce_returned_handoff_completion(completion) else {
            return;
        };
        // A returned attempt is still this build: undo the successor's title the
        // ProofReady edge may have published (`publish_handoff_target_to_menu_bar`).
        self.refresh_version_menu();
        // Only now, with the child reaped and the rollback run, may destructive intent be
        // replayed; a whole-app request dominates individual closes.
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
                // A native view or document may have gained unsaved work during the
                // rollback, so both are asked again at the exit edge.
                if self.replay_clean_quit_ready() {
                    el.exit();
                }
            }
        }
    }

    /// The `ProofReady` arm of [`Self::finish_update_handoff`]: the drain gate, the final
    /// admission, the messages flush, then the claim and the Commit, or the rejection.
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
        // The bar names the successor before this process dies, at the proof's first
        // observation, so the dispatch fence gives AppKit time to publish it: the bar may
        // keep this process's last menu long after `_exit`.
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
        // A held claim ([`HeldCommit`]) comes back here once more, keeping the operator's
        // quiesce and hold fence it took at the claim, and reads the admission again.
        let held = self
            .pending_update_handoff
            .as_mut()
            .and_then(|pending| pending.held_commit.take());
        let claimed = held.is_some();
        // Quiesce the resident operator only for the final admission seam; a successful
        // Commit `_exit`s while its gate is still held by `with_commit_permit`. A busy
        // operator refuses the Commit as activity.
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
        // `hold_fence` stays raised to the end of this arm: through the `_exit`, or until
        // the rejection below is under way.
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
        // The messages flush runs before the claim, under the fence: a flush that took real
        // time sends the attempt through the drain gate again so input queued meanwhile
        // reaches the masters. Nothing it does is lost to a rollback; the video answer is,
        // so it stays after the claim.
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
        // Unused fields keep their underscore-prefixed bindings so their drops still run
        // at the end of this call.
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
        // The claim: a held claim has it already; otherwise it is taken here, and what
        // must be answered before the `_exit` is answered under it.
        let claim = claimed || (admitted && arbiter.try_begin_commit());
        let commit_lost_arbiter = admitted && !claim;
        let mut commit_write_failed = false;
        if claim && !claimed {
            let answered_in = self.answer_at_commit_claim(child_pid);
            // What the claim answered took real time, and a key queued meanwhile would die
            // with the `_exit`. So the claim is held ([`HeldCommit`]) through one more
            // dispatch fence, and the next pass reads the admission again.
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
            // The rows the carry did not take end with this process, recorded and flushed
            // below, after the admission's last reading: a refused held claim keeps this
            // process running with its rows.
            let closed = self.close_uncarried_messages();
            // A held claim's records and the endings just closed are flushed, waiting one
            // fence grain at most (`HANDOFF_INPUT_DRAIN_YIELD`) so keys have no new interval
            // to queue in and die; a writer that did not answer before is not waited for.
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
            // Until the successor publishes its own entries, name it the holder of every
            // carried id, so a launch landing in between is refused (`identity_claim`);
            // withdrawn below if the Commit fails.
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
            // The supervisor host stops at the write, after the admission's last reading;
            // a Commit that returns resumes it below. Not at the claim: a stopped worker
            // clears the badges it raised, which a held claim's next admission would read
            // as an escalation moved since the park.
            if let Some(host) = self.harness.as_ref() {
                host.suspend();
            }
            // The connections the manifest left out are audited as dropped here, right
            // before the one write, so an attempt that stands down records none.
            let left_out = arbiter.connections_left_out();
            #[cfg(test)]
            if let Some(begin) = self.handoff_before_egress_fence.take() {
                begin();
            }
            // Success cannot return: `commit_and_exit` performs the one atomic <=PIPE_BUF
            // write and `_exit(0)`. EPIPE transfers Committing back to Rejecting so one
            // reaper can restore the parent. The successor is activated inside the operator's
            // permit, right before the write, so a refused permit activates nothing. The
            // egress fence is taken before the permit and held through the write
            // (`fence_handoff_egress`): a frame begun after the drain gate's last reading
            // stands the Commit down as unsettled egress (`Ok(None)`), with nothing activated
            // or audited.
            let sinks = self
                .pending_update_handoff
                .as_ref()
                .map(|pending| handoff_sinks(&self.pool, &pending.live));
            let commit_result = match sinks.as_deref().and_then(fence_handoff_egress) {
                None => Ok(None),
                Some(_egress) => match operator_quiesce.as_ref() {
                    Some(quiesce) => quiesce.with_commit_permit(|| {
                        #[cfg(target_os = "macos")]
                        self.activate_successor_at_write(child_pid);
                        Self::audit_connections_left_out(left_out);
                        Some(crate::seamless::commit_and_exit(commit_fd, proof))
                    }),
                    None => {
                        #[cfg(target_os = "macos")]
                        self.activate_successor_at_write(child_pid);
                        Self::audit_connections_left_out(left_out);
                        Ok(Some(crate::seamless::commit_and_exit(commit_fd, proof)))
                    }
                },
            };
            if commit_result
                .as_ref()
                .is_ok_and(|written| written.as_ref().is_some_and(Result::is_err))
                && !left_out.is_empty()
            {
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
                Ok(Some(Err(_))) => commit_write_failed = true,
                // The egress fence is the last reading of the egress fact.
                Ok(None) => facts.egress_settled = false,
                // The permit is the operator's last answer before the `_exit`, so it
                // re-reads the operator's two facts.
                Err(refusal) => {
                    operator_refusal = Some(refusal);
                    (facts.operator_idle, facts.operator_quiesced) =
                        operator_commit_facts(operator_refusal);
                }
                Ok(Some(Ok(never))) => match never {},
            }
            let _ = arbiter.commit_failed_to_rejecting();
        } else if claim {
            // A held claim the admission refused after its fence goes to the rejection;
            // the successor was never activated.
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
                        // A reaper is `Background`, never `Housekeeping`: it enforces a
                        // kill-and-reap deadline (`qos::Role`).
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
                    // Resource exhaustion cannot strand a readerless child: reap inline
                    // when the emergency thread cannot be created.
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

    /// What a Commit answers once it has claimed the attempt, before its write: the
    /// freeze's numbers and the landing in the log, every live `video` request, and a
    /// messages flush. Returns how long it took; real time sends the Commit through the
    /// dispatch fence again.
    #[cfg(unix)]
    fn answer_at_commit_claim(&mut self, child_pid: Option<u32>) -> std::time::Duration {
        let started = std::time::Instant::now();
        // The freeze, in two numbers: park→proof is the successor's swap, re-exec and GUI
        // boot; proof→commit is this process's dispatch fence and egress settle.
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
        // The landing, on the arming's clock: the log carries arm → start → land for the
        // automatic lane, so how long an update waited is one subtraction.
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
        // Every live `video` request is answered here, before the `_exit`: until the claim
        // the attempt could still roll back, and a take aborted then would be lost to an
        // update that did not happen. Bounded ([`VIDEO_EXPORT_COMMIT_WAIT`]).
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
        // The rows the carry did not take close at the write, not here: a held claim's
        // admission may still refuse. The parent's record lands before its `_exit` (the
        // writer's queue dies with the process, and the successor re-reads the file after
        // Commit); a writer that did not answer before the claim is not waited for twice.
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

    /// Activates the successor at the Commit's write, and only when an aterm window has
    /// focus there: nothing before asks for it (LaunchServices' `activates` is false, the
    /// successor's launch posture asks nothing), so this is the one instant the user's
    /// focus moves. Focus is read here, not at the park, since the person may switch apps
    /// while the terminal is frozen; `WindowEvent::Focused` is exempt through the overlap,
    /// so the windows' focus is current. Never at the claim: a claim held through one more
    /// dispatch fence ([`HeldCommit`]) may still be refused, and an active successor would
    /// take the person's keys and leave a focus-out report (DEC 1004) after its focus-in.
    #[cfg(target_os = "macos")]
    fn activate_successor_at_write(&mut self, child_pid: Option<u32>) {
        let Some(pid) = child_pid.and_then(|pid| i32::try_from(pid).ok()) else {
            return;
        };
        if !self.any_os_window_focused() {
            aterm_log::info!(
                "update apply: not activating the successor at Commit (no aterm \
                 window has focus at the write; the person keeps the app they are \
                 in)"
            );
            return;
        }
        // A test records the activation in place of AppKit's: no unit test
        // reaches WindowServer (AGENTS.md).
        #[cfg(test)]
        let accepted = {
            self.handoff_activations.push(pid);
            true
        };
        #[cfg(not(test))]
        let accepted = crate::app_launch_successor::activate_running(pid);
        aterm_log::info!(
            "update apply: activating the successor at Commit (an aterm \
             window has focus at the write): {}",
            if accepted {
                "accepted"
            } else {
                "refused by AppKit — the successor stays behind"
            }
        );
    }

    /// The cached verdict's own reason for a refusal of this artifact, read by the same key
    /// as [`Self::cached_handoff_preverification`].
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn cached_handoff_preverification_reason(
        &self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
    ) -> Option<String> {
        self.read_fresh_preverification(attempt, |entry| entry.reason.clone())
    }

    /// How many physical handoff failures this process has booked against `attempt`'s
    /// artifact, which the freeze budget widens on.
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

    /// How many times the fork lane's park has missed its freeze budget for
    /// `target_build`: the rungs it climbs across attempts.
    #[must_use]
    #[cfg(unix)]
    pub(crate) fn fork_park_misses_of(&self, target_build: u64) -> u8 {
        self.fork_park_misses
            .filter(|(build, _)| *build == target_build)
            .map_or(0, |(_, misses)| misses)
    }

    /// The fork lane waits for an unread policy while it is being read: `Some(wait)` for an
    /// attempt with no verified pass on record while a read is in flight, the first such
    /// attempt starting it. `None` for a known policy, no read, or once
    /// [`crate::HANDOFF_PREVERIFY_READ_CEILING`] has passed. A person's apply is held
    /// ([`crate::UpdateHandoffStartError::HeldForRead`]) rather than deferred: a Full capture
    /// before the policy is read would be refused by it with every reader already frozen.
    #[cfg(unix)]
    fn fork_lane_policy_wait(
        &mut self,
        mode: crate::native_updater_service::ApplyMode,
        target_build: u64,
        policy_unread: bool,
    ) -> Option<crate::UpdateHandoffStartError> {
        self.preverify_read_wait(mode, target_build, policy_unread, PreverifyWaiter::ForkPark)
    }

    /// The cold lane answers to the pre-park verdict before it writes the quit layout and
    /// replaces this process: its successor is this build, whose boot swap would refuse
    /// what the verdict already knew and come back to re-arm and exec again.
    ///
    /// In the automatic modes: a fresh refusal of this artifact is the answer (`Failed`,
    /// booked by its typed fact); with no verdict yet and a read in flight or startable,
    /// the attempt defers while it runs, up to [`crate::HANDOFF_PREVERIFY_READ_CEILING`]; a
    /// read that ended without a verdict (a passing refusal) is a transient `Failed`, since
    /// the boot swap would wait on the same lock. A person's apply is not answered by a
    /// cached verdict.
    ///
    /// In every mode, a stage known to wait for an update check to confirm its signer
    /// ([`Self::known_roster_wait`]) is not relaunched into: a deferral, charged nothing. A
    /// read that ended with that wait but began before a check reported the stage is stale,
    /// and the stage is read again.
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
                // A roster confirmation wait is no verdict: hold for the check rather than
                // book it on the transient budget.
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
                // A roster wait [`Self::known_roster_wait`] did not take is stale (the read
                // began before a check reported the stage): no `Failed` to book; the stage
                // is read again and the attempt defers.
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
                // This target's read still running is waited for even past the per-target
                // window; its own deadline bounds the wait.
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

    /// One bounded wait for a pre-park verification read, for the lane about to act
    /// without its answer (`waiter`): one window per target (`preverify_read_waited`),
    /// shared by both lanes. An automatic attempt defers as activity; a person's is held
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
        // One wait per target, renewed only once a cached verdict would have gone stale
        // since it ended.
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
        // A person's press is held, not deferred: the automatic lane comes back for its
        // own deferral, and nothing does for a person's when `[update] auto_apply` is off.
        Some(if mode.is_automatic() {
            preverify_read_activity(target_build, waiter)
        } else {
            preverify_read_hold(target_build, waiter, until)
        })
    }

    /// Audits every connection the manifest left out as dropped
    /// (`action=drop origin=handoff`). Called by the Commit alone, right before its write:
    /// this process holds them until then, and an attempt that stands down dissolves none.
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

    /// Hold a person's press for the candidate's pre-park read; `about_to_wait` runs it
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

    /// The event loop's next look at a held press: every [`HELD_PERSON_APPLY_POLL`], never
    /// later than the hold's window ends.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn held_person_apply_deadline(
        &self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        self.person_apply_held
            .map(|held| held.until.min(now + HELD_PERSON_APPLY_POLL))
    }

    /// Runs a person's held Install ([`Self::hold_person_apply`]) as their own `Immediate`
    /// apply once the candidate's read has ended or its window `until` has passed, its
    /// outcome surfaced as a row. A stage that is no longer the held target drops the
    /// press.
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

    /// A fork-lane park miss: counts the rung it buys the next attempt, routed to the
    /// activity spacing rather than the physical budget.
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

    /// The cached verdict's reason keyed by the artifact alone, for the submission-time
    /// failure arm: `Some` only for a fresh refusal of the staged artifact of `build`.
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

    /// Hands a refused cold relaunch's refusal to this process's first automatic read of
    /// its artifact, whenever that comes, so the verdict cannot lapse before it is booked.
    /// Taken once.
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

    /// The cached pre-park verification verdict for this exact artifact, or `None` with no
    /// fresh entry. `Some(false)` short-circuits [`Self::start_unix_update_handoff`] before
    /// any reader parks.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn cached_handoff_preverification(
        &self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
    ) -> Option<bool> {
        self.read_fresh_preverification(attempt, |entry| Some(entry.passed))
    }

    /// Reads the cached pre-park verification entry under its lock when it is the fresh one
    /// for exactly `attempt`'s artifact (build, commit, digest).
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

    /// The successor's handoff policy as every park follows it, from the passed
    /// pre-verification of exactly the targeted artifact. Not bound by
    /// [`crate::HANDOFF_PREVERIFY_FRESHNESS`]: the policy is a sealed resource of one release.
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

    /// Reduces one returned (non-ready) completion: resumes the parked readers, reduces the
    /// apply ticket against the lane its mode authorized, surfaces the verdict, and returns
    /// the teardown the caller must replay (`None`: no attempt matches). The only place the
    /// attempt's `ApplyMode` still exists, so the automatic/person split is decided here.
    #[cfg(unix)]
    pub(crate) fn reduce_returned_handoff_completion(
        &mut self,
        completion: crate::UpdateHandoffCompletion,
    ) -> Option<crate::DeferredHandoffTeardown> {
        // Unused fields keep underscore-prefixed bindings so their drops still run at the
        // end of this call.
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
        // Two records can describe one attempt: the parked one (readers stopped) and the
        // prelaunched one (a successor held). Readers roll back iff the parked record
        // exists, and both are cleared only here, after the worker's death proof, so no
        // retry launches beside a live candidate. The parked record's facts win.
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
        // The lane for the bounded automatic retry budgets, from the authorized mode (a
        // person's failure is never charged to the automatic budget), the outcome, and the
        // child-death evidence.
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
        // The attempt record is gone, and with it the fence holding draft journals: owed
        // re-seats run now.
        self.drive_owed_document_journals();
        // The attempt is over: restore the row and end the surge before the outcome decides
        // whether to say anything.
        self.retire_update_installing();
        // QA seam: a `None` ticket comes only from `ATERM_DEBUG_SEAMLESS_REEXEC`, and never
        // touches the durable ledger.
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
        // The switch that did not happen goes on record, before the outcome is surfaced: an
        // attempt the automatic lane retries by itself is a wait; one that stopped the lane,
        // or a person's press, could not install.
        let routine = matches!(
            lane,
            crate::app_native::HandoffFailureLane::ActivityRevoked
                | crate::app_native::HandoffFailureLane::AwaitsRosterConfirmation { .. }
        ) || (pending.mode.is_automatic()
            && attempted_build.is_some_and(|build| self.automatic_apply_retry_scheduled(build)));
        // A capture refusal's stop is said by its outcome's own record, which knows what
        // the retry waits for: one record per attempt.
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

    /// Overlap-handoff failure rollback: restores this still-running parent to a working
    /// terminal once the candidate was proven terminated, or there never was one. The
    /// [`HandoffRollbackWarrant`] is minted before the completion is sent
    /// ([`send_warranted_handoff_failure`]) and cannot cross the channel, so the send-site
    /// ordering is the guarantee. Re-arms CLOEXEC, puts back each parked size, resumes the
    /// readers, and owes every draft journal a re-seat (queued to the document worker: this
    /// half does no filesystem I/O), since a candidate that restored may have republished it.
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
    /// Puts each handed PTY back at the size its engine was parked at, undoing the
    /// successor's resize (`resize_panes` skips panes whose engine already matches).
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

/// How often the event loop looks at a person's held Install: the read it waits on raises
/// only a flag.
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

/// What a person's press answers with while `target_build`'s pre-park verification is
/// read: held until the read ends or `until` passes, never deferred, since nothing would
/// come back for a deferral with `[update] auto_apply = false`.
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

/// The cold lane's refusal of a token carrying unsaved editor drafts, in the preflight's
/// person-facing shape ([`crate::app_native::DIRTY_DOCUMENTS_BLOCK`]).
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
    /// What the cold lane leaves its successor to reopen: the quit layout (`Some`), so
    /// document and Settings tabs and window geometry survive; `None` with nothing to
    /// leave. With session restore off the lane refuses, as it does for `--hold` panes.
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

    /// Says in `aterm.log`, and once per target build and cause in Messages, that the cold
    /// lane goes on without its quit layout (`cause`), so a person missing their tabs can
    /// read why.
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

    /// Takes back this run's cold-lane layout an `exec` did not use (this run's desk alone),
    /// waiting at most [`COLD_LANE_LAYOUT_LOCK_WAIT`]. A refusal is owed until it lands or a
    /// newer write of this run's desk replaces it, or the stale desk would be reopened.
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

    /// The event loop's try of an owed take-back once due; a refusal spaces the next try
    /// further ([`OwedTakeBack::spacing`]).
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

    /// Before a switch that writes no newer desk of this run, whose successor would reopen
    /// the stale one: an owed take-back is tried now. A passing refusal refuses the attempt;
    /// a settled one goes on, logged, since no retry changes it.
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

    /// At quit: an owed take-back is replaced by the quit's own write of this run's desk
    /// (`quit_wrote_its_desk`), or else tried once more, waiting at most
    /// [`QUIT_TAKE_BACK_LOCK_WAIT`].
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

/// How long the cold lane's quit-layout write and take-back wait for the restore lock on
/// the main thread: a peer holds it for one or two `F_FULLFSYNC`s (worst measured 280 ms),
/// and one stopped while holding it is a passing refusal, never a frozen window.
#[cfg(unix)]
const COLD_LANE_LAYOUT_LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// The wait before an owed take-back is tried again after its first refusal, doubled per
/// refusal up to [`OWED_TAKE_BACK_RETRY_CAP`].
#[cfg(unix)]
const OWED_TAKE_BACK_RETRY_FIRST: std::time::Duration = std::time::Duration::from_secs(2);

/// The longest spacing of an owed take-back's tries
/// ([`OWED_TAKE_BACK_RETRY_FIRST`]).
#[cfg(unix)]
const OWED_TAKE_BACK_RETRY_CAP: std::time::Duration = std::time::Duration::from_secs(60);

/// How long the quit's try of an owed take-back waits for the restore lock, as long as the
/// quit's crash-journal retire.
#[cfg(unix)]
const QUIT_TAKE_BACK_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// What the cold lane's quit layout still owes after an attempt that did not `exec`.
#[cfg(unix)]
#[derive(Debug, Default)]
pub(crate) struct ColdLaneLayoutDebt {
    /// The target build and cause whose tabs-not-kept record is in Messages, so a retry
    /// does not write it again.
    not_kept_said: Option<(u64, String)>,
    /// This run's desk, written for an `exec` that did not happen, whose take-back the
    /// restore lock refused ([`App::take_back_cold_lane_layout`]).
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
    /// The wait after the `refusals`-th refusal: [`OWED_TAKE_BACK_RETRY_FIRST`] doubled per
    /// refusal after the first, at most [`OWED_TAKE_BACK_RETRY_CAP`].
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

/// Writes the cold lane's quit layout (`App::cold_lane_layout`) through `write`; a failure
/// is about this machine's data directory, never the candidate. A passing cause refuses,
/// retried on the block cooldown; a settled one, which no retry changes, goes on without
/// the layout and says so (`NotKept`), losing which tabs were open but never unsaved text.
/// A write that failed after it published is `Written`, taken back if the `exec` does not
/// happen.
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

/// What `lane` cannot carry of what the preflight's `token` let ride, as the refusal the
/// update row reads: the cold lane's quit layout carries no unsaved text, so it refuses
/// every editor draft, Settings draft and failed checkpoint; the seamless lane refuses
/// Settings drafts on a hop to an older build, which ignores that field (editor drafts ride
/// the draft journal).
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

/// Each handed record's [`crate::session_store::CarriedClaims`] as the park drew it, for
/// the Commit to read again ([`App::carried_claims_held`]).
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

/// The registry's projection narrowed to the sessions in `live`: the manifest, descriptors
/// and screens must name the same sessions (`seamless::write_outgoing` refuses otherwise).
/// Connection rows stay whole; the successor re-mints only rows whose endpoints it adopted.
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

/// Stamps each record with its session's foreground holder as the park read it: the
/// control-carry sidecar is dropped for many sessions, and without a holder a job that died
/// during the park would leave its alt screen and mouse modes armed under the prompt.
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

/// Stamp every handed session's crash journal key on its manifest record (`keys`:
/// [`crate::App::journal_keys`]); a record with none carries no key, so an older reader
/// sees the wire it always saw.
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
/// ([`ParkedScreens::render_debts`]) as `"<shift>,<pushed>"`, only when the engine owed
/// rows on the alternate screen; a record with none carries no key, read as owing nothing.
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

/// Keep the actionable handoff cause in the returned refusal, which `update status` reads
/// from the updater's durable status; an unrelated admission failure keeps its own reason.
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
            // The admission's own message keeps the counts but neither actionable fact.
            assert!(!reason.message(facts).contains(cause));
            assert!(!reason.message(facts).contains(remedy));
        }
    }

    /// An unreadable foreground does not hide why the handoff is unavailable: the refusal
    /// still carries the cause and remedy.
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

/// The verdicts on one `poll` answer during the ready wait ([`wait_handoff_ready`]), split
/// out so the anti-spin contract is testable: queued master output lands on
/// [`ReadyPollAction::NoProgress`], the yielding branch.
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReadyPollAction {
    /// A handed-off master reported HUP/ERR/NVAL — the adopted live-set identity
    /// is stale; reject.
    SessionDied,
    /// The proof fd is readable (POLLIN or its own HUP/ERR/NVAL) — read the wire.
    ReadProof,
    /// Neither: queued output or a bare slice. The caller must yield, or a master that
    /// stays readable makes `poll` return instantly forever.
    NoProgress,
}

/// Classifies one `poll` answer: death on any master (`pollfds[1..]`) dominates, then a
/// readable proof fd (`pollfds[0]`), else yield.
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

/// What the main thread hands `prelaunch_out_of_band_handoff`: the pre-park facts the
/// launched lane needs.
#[cfg(target_os = "macos")]
#[expect(
    clippy::disallowed_types,
    reason = "the update successor's raw Command: it hands over exactly its own descriptors, which a strip at the exec would undo"
)]
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

/// The facts the park gate reads, measured on the main thread right before it runs, in a
/// shape a test can enumerate.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ParkGateFacts {
    pub(crate) mode: crate::native_updater_service::ApplyMode,
    /// `App::automatic_phase_for`: where the automatic lane stands on its
    /// ladder at this instant. Ignored by the explicit modes.
    pub(crate) phase: crate::native_update_auto_intent::ApplyPhase,
    /// `App::automatic_activity_facts`: the terminal's quiet / keys / output /
    /// focus facts, sampled at this instant.
    pub(crate) activity: crate::native_update_auto_intent::ActivityFacts,
    /// `!handoff_masters_have_activity`: no live master has bytes waiting that
    /// a reader has not consumed.
    pub(crate) masters_quiet: bool,
    /// `!handoff_masters_closed`: no handed master has hung up. A dead one would fail the
    /// post-park re-check, so the gate waits rather than freezing the terminal to find out.
    pub(crate) masters_alive: bool,
    /// How many times in a row the gate has answered `Wait` in `Land`, which only the
    /// masters can cause; past [`PRELAUNCH_LAND_MAX_WAITS`] the park proceeds.
    pub(crate) land_waits: u8,
    /// The successor's policy says `park_quiet_gate_at_land = "relaxed"`: at `Land` the
    /// park proceeds over queued output at once, never over a dead master.
    pub(crate) land_gate_relaxed: bool,
    /// How long the successor has been holding its claim.
    pub(crate) held_for: std::time::Duration,
    /// A `video` take (or its export) is live and owed a reply ([`recording_park_refusal`]).
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
    /// The hold cap expired without a moment to park in: stand down as activity-revoked,
    /// so the automatic lane tries again later.
    StandDown(&'static str),
}

/// When the terminal may be frozen, decided separately from starting the update (the
/// launch costs nothing: readers stay live through the successor's boot). `Immediate`
/// parks at once; the automatic modes follow the ladder
/// (`native_update_auto_intent::automatic_park_refusal`) at its live phase.
///
/// Unread master bytes are a preference, not a correctness gate: at `Land` the gate gives
/// way after [`PRELAUNCH_LAND_MAX_WAITS`] waits (at once under a `relaxed` policy), since a
/// fast producer keeps `POLLIN` set; the successor replays the kernel queue. Monotone in
/// `held_for`: past the cap every automatic mode stands down.
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

/// How long a live `video` take may hold an automatic park before `Land`: the verb's
/// maximum duration ([`crate::control::control_media::VIDEO_MAX_DURATION`]).
#[cfg(unix)]
pub(crate) const VIDEO_PARK_WAIT_MAX: std::time::Duration =
    crate::control::control_media::VIDEO_MAX_DURATION;

#[cfg(unix)]
const _: () = assert!(
    VIDEO_PARK_WAIT_MAX.as_secs() + 10 < PRELAUNCH_HOLD_MAX.as_secs(),
    "a recording's wait must come due well inside the hold, or a take stands the successor \
     down instead of being answered"
);

/// Whether a live `video` take holds this park: the automatic lanes let it finish before
/// `Land`, for at most [`VIDEO_PARK_WAIT_MAX`] of the hold; after that Commit aborts it
/// with a reply ([`crate::App::video_answer_before_commit`]). An explicit apply never waits.
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

/// The detail a hold-cap stand-down carries: the cap's reason, how long the successor was
/// held, and what the gate last waited for, so a flooded master is not reported as the
/// user being busy.
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

/// How many consecutive `Wait` answers the gate gives in `Land` before parking over queued
/// output: one second at the 20 ms re-run, so a permanently readable master costs no more.
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
    /// The park missed (timing or storage): the readers were rolled back, nothing was
    /// granted, and the successor still holds. Re-park after [`PRELAUNCH_REPARK_DELAY`] on
    /// the next freeze-budget rung; a deterministic refusal never belongs here.
    Missed(String),
    /// A miss only because a parser was mid-sequence over queued output: re-parks at the
    /// gate's cadence on the same rung, up to [`PRELAUNCH_MAX_MID_SEQUENCE_REPARKS`], then
    /// becomes [`Self::Missed`]. The fork lane files it as a miss directly.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    MissedMidSequence(String),
    /// The capture refused the desk, which no rung changes: the successor stands down as
    /// [`crate::UpdateHandoffOutcome::CaptureRefused`], retried when the desk changes.
    Refused(CaptureRefusal),
    /// Stand the held successor down with this outcome; readers (if they were
    /// parked) have already been rolled back.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    Failed(HandoffStandDown),
}

/// A deterministic refusal of the park's capture, naming the session that held the update
/// back if any.
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

/// Which lane a capture failure belongs to, typed on the variant: timing and this
/// process's storage are a miss (the same desk can capture on a wider rung); `Refused` is a
/// fact about the desk that no rung changes.
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

/// The park's commitment to its post-park handoff layout: the digest the adoption proof is
/// taken over, or `CaptureRefused` for a layout still past the caps after
/// `restore::RestoreManifest::fold_into_caps`, which would otherwise fail the worker's
/// round trip on every attempt (`restore::RestoreManifest::why_not_whole`).
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

/// Test seam for the capture's lane decision: the shipping park capture over the whole
/// pool, with a deadline no loaded machine can miss, answering the repainted ids or the
/// [`ParkAttempt`] a failure classifies into.
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

/// What one park hands the worker, projected from a whole [`App`] as the fork lane does;
/// the PTY descriptors are left to the caller.
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
    /// The fork lane's history plan for this park, for the caller to run as the worker
    /// does (`HistoryPlan::results`, then `stamp_manifest`).
    pub(crate) history: crate::handoff_history::HistoryPlan,
    pub(crate) layout: crate::restore::RestoreManifest,
    pub(crate) layout_digest: Option<[u8; 32]>,
    /// The held panes' screens the capture carried.
    pub(crate) held: Vec<crate::seamless::HeldScreen>,
}

/// Whole-app test seam: the fork lane's park over this App's real pool, with the
/// successor's caps for `target_build` and a deadline no loaded machine can miss.
#[cfg(all(test, unix))]
impl App {
    /// The park described above, under the successor's policy for `attempt`
    /// ([`Self::park_policy`]); `None` is the same-image seam.
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

/// The stand-down a [`ParkAttempt::Refused`] park sends: `CaptureRefused`, naming the
/// session.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[must_use]
pub(crate) fn capture_refusal_stand_down(refusal: &CaptureRefusal) -> HandoffStandDown {
    HandoffStandDown {
        outcome: crate::UpdateHandoffOutcome::CaptureRefused,
        detail: format!("{refusal}; the lane retries once the desk changes"),
    }
}

/// How often the event loop re-runs the park gate while a prelaunched attempt waits for a
/// quiet moment: well inside the 500 ms quiet epoch it waits for.
#[cfg(unix)]
const PRELAUNCH_PARK_RETRY: std::time::Duration = std::time::Duration::from_millis(20);

/// How long the gate waits before re-parking after a missed budget: long enough for
/// whatever held the engine lock to finish.
#[cfg(any(target_os = "macos", all(test, unix)))]
const PRELAUNCH_REPARK_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// How many times one prelaunched attempt re-parks before standing its successor down: the
/// whole freeze ladder ([`handoff_freeze_budget`]) is climbed within one attempt, with the
/// successor still holding. A miss is the parent's scheduling failure and says nothing
/// about the candidate, so it never reaches the ledger ([`park_miss_disposition`]).
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) const PRELAUNCH_MAX_PARK_MISSES: u8 = 2;

/// What one park that missed its freeze budget costs the prelaunched attempt.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Debug)]
pub(crate) enum ParkMissDisposition {
    /// Re-park after [`PRELAUNCH_REPARK_DELAY`] on the next rung; `misses` is the new miss
    /// count, which is also that rung.
    Repark { misses: u8, reason: String },
    /// A mid-sequence miss inside its own bound: re-park after [`PRELAUNCH_PARK_RETRY`] on
    /// the same rung; `reparks` is the attempt's new mid-sequence count.
    RetryMidSequence { reparks: u8, reason: String },
    /// Every rung was tried. Stand the successor down.
    StandDown(HandoffStandDown),
}

/// How many mid-sequence misses one prelaunched attempt re-parks on for free, at the gate's
/// cadence without climbing the rung: past [`PRELAUNCH_LAND_MAX_WAITS`], `Land` parks
/// inside a flood, where a reader can stop inside an escape sequence.
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) const PRELAUNCH_MAX_MID_SEQUENCE_REPARKS: u8 = 32;

/// [`aterm_core::terminal::Terminal::output_state_in_flight`]'s name for an open OSC 8
/// link, the one dropped state whose miss is soft ([`App::soft_miss_may_repark`]).
#[cfg(unix)]
const OPEN_LINK_STATE: &str = "Osc8HyperlinkOpen";

/// The drain gate's per-respin yield ([`App::handoff_drain_gate`]), and the grain under
/// which a blocking wait at Commit opens no new window ([`App::commit_flush_refences`]).
#[cfg(unix)]
const HANDOFF_INPUT_DRAIN_YIELD: std::time::Duration = std::time::Duration::from_millis(2);

#[cfg(any(target_os = "macos", all(test, unix)))]
const _: () = assert!(
    (PRELAUNCH_MAX_MID_SEQUENCE_REPARKS as u64) * 20 <= 1_000
        && PRELAUNCH_PARK_RETRY.as_millis() == 20
);

/// A mid-sequence miss: a free re-park while `reparks` is inside
/// [`PRELAUNCH_MAX_MID_SEQUENCE_REPARKS`], else the ordinary ladder.
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

/// A park miss is a fact about the machine's moment, never about the bytes: past the last
/// rung the successor stands down as `ActivityRevoked`, with no physical budget spent
/// (docs/DESIGN-auto-apply-ladder-2026-09-21.md, law 2).
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

/// How long Commit waits at most for every live `video` request it answers to reach its
/// client before `_exit` ([`crate::App::video_answer_before_commit`]).
#[cfg(unix)]
pub(crate) const VIDEO_EXPORT_COMMIT_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

/// How long the seamless parent at Commit, and the cold lane before `exec`, wait for the
/// message-log writer ([`crate::App::flush_messages_log_within`]).
#[cfg(unix)]
pub(crate) const MESSAGES_FLUSH_COMMIT_WAIT: std::time::Duration =
    std::time::Duration::from_millis(250);

/// How long a dialled successor is held for a quiet moment before the attempt stands down
/// (activity-revoked, retried later): longer than the whole ladder
/// ([`crate::native_update_auto_intent::LANDS_WITHIN`]), so a held successor reaches `Land`.
#[cfg(unix)]
pub(crate) const PRELAUNCH_HOLD_MAX: std::time::Duration = std::time::Duration::from_secs(120);

#[cfg(unix)]
const _: () = assert!(
    crate::native_update_auto_intent::LANDS_WITHIN.as_secs() + 10 < PRELAUNCH_HOLD_MAX.as_secs()
);

/// When the post-park proof wait gives up on the launched lane, anchored at the park: 3 s
/// on a first automatic attempt, 6 s after a physical failure of the same bytes, 30 s for
/// an explicit apply; `ATERM_HANDOFF_PROOF_TIMEOUT_MS` (a development seam) overrides all
/// three.
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

/// When this attempt stops waiting for its successor to prove readiness: a deadline, not a
/// timeout, which on the launched lane bounds both the dial and the proof.
#[cfg(unix)]
#[must_use]
fn handoff_ready_deadline() -> std::time::Instant {
    // 30 s: LaunchServices latency, a first-launch Gatekeeper assessment, the successor's
    // whole boot apply, adoption and painting can exceed 15 s under load, and expiry only
    // un-parks the parent, while a short deadline would never succeed on a busy machine.
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
    /// The launched successor dialled, the code gate turned it away
    /// ([`crate::handoff_rendezvous::CodeRefusal`]), and it exited by its own rule; filed
    /// by the refusal's cause ([`code_refused_completion`]).
    CodeRefused {
        dialer: HandoffCandidate,
        refusal: crate::handoff_rendezvous::CodeRefusal,
        death: PreDialDeath,
    },
    /// The launched successor dialled, was held while `trustd` did not answer its code
    /// check, and left: filed by its own evidence, in words that say it dialled.
    ExitedWhileHeld {
        dialer: HandoffCandidate,
        death: PreDialDeath,
    },
    /// The rendezvous ended it: the dial budget ran out, or it refused one dialer too many.
    Rendezvous(crate::handoff_rendezvous::RendezvousError),
}

/// Whether `dialer`, turned away by the code gate and witnessed alive then, is the launched
/// process whose death `death` reports: the pid LaunchServices named, or, for a death with
/// no pid, a dialer that is gone too.
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
    /// LaunchServices answered that the application it launched terminated before it
    /// checked in; nothing was observed about how it ended.
    TerminatedBeforeCheckIn,
    /// The process LaunchServices named is gone ([`NamedSuccessorWatch`]): `candidate` is
    /// its pid and first-sight birth stamp, `death` the kernel's record.
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

/// The pid LaunchServices named, watched until the dial: LaunchServices says nothing if a
/// named process later dies, which would otherwise be found only at the dial deadline. Asks
/// the [`CandidateExitWatch`] registered at first sight, then
/// [`handoff_candidate_terminated`]. A live process is never called gone: a same-pid
/// `execve` raises only `NOTE_EXEC`, a failed registration (the kernel refuses a zombie) is
/// not death, and pid reuse can only hide one. The pid is never signalled or admitted.
#[cfg(target_os = "macos")]
#[derive(Default)]
struct NamedSuccessorWatch {
    /// The named process, its birth read at first sight, and its exit watch
    /// (`None` when it could not be registered).
    seen: Option<(HandoffCandidate, Option<CandidateExitWatch>)>,
}

#[cfg(target_os = "macos")]
impl NamedSuccessorWatch {
    /// One look between accept slices: is the named process (`named`, peeked from the
    /// answer) gone? LaunchServices answers once, so the first pid seen is the only one.
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
            // Ask the watch again after the proof: the knote is queued before the pid can
            // fall vacant, so a death between the two reads still carries its status.
            let status = watch.as_ref().and_then(CandidateExitWatch::exit_status);
            return Some((*candidate, handoff_child_death(true, status)));
        }
        None
    }
}

/// The code the launched successor must run to pass the gate on its dial, which keeps out a
/// same-uid process that copied the claim secret.
///
/// * When the successor runs an update candidate (`successor_runs_candidate`) and a release
///   team is pinned: the release requirement
///   ([`crate::handoff_rendezvous::DialerCode::release`]), which the candidate passed
///   (`codesign --verify -R`) before this lane was chosen. That check does not test the
///   signing identifier, so it is read here, before the launch: another identifier would be
///   refused at every dial, so it is `Err` and the caller forks instead.
/// * Otherwise (the same-image QA seam, or no team pinned): the designated requirement of
///   the code at `bundle`, read when a dialer presents the secret, after the successor's
///   boot swap has put its build there.
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

/// The outgoing process's wait for its launched successor's dial, with every reader live.
///
/// The accept's abort peeks LaunchServices' answer between poll slices (after the cancel),
/// so a successor that dies before its dial is seen at once: "terminated before check-in",
/// or a named pid the kernel says is gone ([`NamedSuccessorWatch`]). Any other answer
/// leaves the wait alone, so a live successor is never refused here. The peeks consume
/// nothing, so the corroboration after the dial still reads the answer.
///
/// A dialer is accepted only if its code is `code` ([`launched_dialer_code`]). A death of
/// the launched process after the code gate turned it away ends the wait on the refusal
/// ([`LaunchedDialEnd::CodeRefused`] or [`LaunchedDialEnd::ExitedWhileHeld`]).
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
    // The last dialer the code gate turned away, witnessed while still connected, so a
    // later death is laid at the refusal's door only when that dialer is the launched one.
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

/// The rollback of a successor that died before its dial (`NeverTransferred`: nothing was
/// accepted, so no descriptor left), filed `ChildDied` with the wait's evidence, and the
/// counted trial launch settled by it ([`forgives_the_counted_trial_launch`]) before the
/// disk facts that carry the sentinel's count. The unexplained lane's budget
/// (`app_native::UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS`) is safe only because such failures
/// forgive their launch. A death with no pid is settled by the name the successor stamped
/// (`handoff_rendezvous::stamp_this_launch`). `held` names a successor that dialled and
/// left while the code gate held it for a `trustd` check, filed the same way.
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

/// The outcome a code-gate refusal is filed under ([`code_refused_completion`]): a check
/// that did not happen (`TrustdSilent`, `Unchecked`) is this process's moment,
/// `ProducerFailed`; a verdict (`Refused`) means the launched process was not running the
/// candidate, so `Rejected`. Both transient, neither a verdict on the candidate.
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

/// The completion of an attempt whose launched successor the code gate turned away and
/// which then exited by its own rule: the refusal's own outcome ([`code_refusal_outcome`])
/// and words. Never `ChildDied`: the exit is the refusal's consequence.
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

/// The rollback of a launched successor the code gate turned away, filed by
/// [`code_refused_completion`]. The successor gives its counted trial launch back as it
/// leaves; it is given back here too, by the dialer's name, for one that could not, since
/// the refusal is no evidence against the candidate's bytes.
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

/// LaunchServices' pending answer to the launch of the dialer already accepted, polled with
/// a zero budget beside the hold and [`wait_handoff_ready`] so a contradiction refuses
/// before Commit: a pid other than the dialer's, or "terminated before it checked in"
/// ([`crate::app_launch_successor::launch_answer_proves_exit`]) while the dialer lives.
/// After the grant the descriptors are the peer's, so a refusal there is a kill-and-reap,
/// never `NeverTransferred`.
#[cfg(target_os = "macos")]
struct LaunchCorroboration<'a> {
    in_flight: &'a crate::app_launch_successor::LaunchInFlight,
    dialer_pid: i32,
    /// The dialer as the kernel attested it at the accept (pid and birth stamp), asked
    /// whether it still lives when LaunchServices says its process exited.
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
    /// One zero-budget poll, the only reader that takes LaunchServices' answer
    /// ([`await_launched_dial`] only peeks). `Err` refuses the handoff: the answer names
    /// another pid, or says the launched process exited while the dialer lives. A dialer
    /// gone by then is no contradiction (the death checks name it); a zombie has no birth
    /// stamp, so a dead, unreaped successor reads as gone.
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

/// Blocks until the overlap-handoff child proves readiness, closes its proof pipe, or
/// `deadline` passes. Never calls `Child::try_wait`, which would reap an exited group
/// leader and lose the pid before the reaper signals its descendants. Pre-ready death is
/// proof EOF or `candidate_gone` (a stray write-end copy can hold EOF back). A master's
/// HUP/ERR/NVAL or a cancel poke is `ActivityRevoked`; queued output on a master is ignored.
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
    // Masters are watched for death only: POLLIN must be requested (macOS evaluates a PTY
    // only for requested events), but plain POLLIN in the answer is ignored.
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
        // The candidate's own end, asked every slice since a stray write-end copy can hold
        // EOF back; after the corroboration, so a disowned dialer is still `Rejected`.
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
                // A handed master with queued output answers plain POLLIN at once; without
                // this yield the loop would busy-spin and starve the child it waits on.
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

/// The late park's stand-down against real processes and the real rendezvous, and how the
/// death it reports settles the counted trial launch.
#[cfg(all(test, target_os = "macos"))]
#[path = "app_update_handoff_held_successor_stand_down_tests.rs"]
mod held_successor_stand_down_tests;

/// The late park's attempt records and the completion reducer.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_late_park_record_tests.rs"]
mod late_park_record_tests;

/// The worker's own publication of a pass for `attempt` ([`PreverifyPublisher::for_attempt`]),
/// so a test seeds the cache as the shipping lane does.
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

/// The worker's own pre-verification reaches the park: its pass and handoff policy are
/// published under the attempt's artifact key, where the launched lane's park reads them.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_preverify_publisher_tests.rs"]
mod preverify_publisher_tests;

/// The park gate, enumerated: the ladder's rule for the live phase, plus unconsumed master
/// bytes, relaxed only at its bound.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_park_gate_tests.rs"]
mod park_gate_tests;

/// With real PTY masters: a parser caught mid-sequence over queued output is a timing miss,
/// never a carry, and an exited `--hold` pane is not handed, so its hung-up master can
/// neither hold the park nor fail it.
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

/// The dry run of the park's capture: what it measures, what it saturates on, what it
/// cannot measure, and that it parks nothing.
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

    /// Walks a pool of identical panes through the producer's reserve-then-history rule
    /// with the real admission seam and budgets. `Err(index)` is the session the capture
    /// would refuse.
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

    /// 12, 24 and 64 panes, at a typical and a maximized geometry, are all admitted: less
    /// scrollback is allowed, refusing the update is not.
    #[test]
    fn a_heavy_pool_is_admitted_even_when_it_must_drop_history() {
        // 49x110 is a typical window; 60x200 a maximized window on a large display,
        // where one session costs more than twice as much.
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

    /// A session is refused only when the pool's mandatory total does not fit, and then the
    /// first session past it, never an earlier one whose budget went to scrollback.
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

    /// The per-grid cell cap admits every window the GUI can produce: four Mac displays,
    /// full screen and maximized, at every font size from `FONT_PX_MIN` to 48 px
    /// ([`crate::App::grid_dims_for`]), split into 1..=8 panes.
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

    /// `kill(-pid)` is sound only while the pid is pinned: a live child of ours is, a
    /// process that is not our child is not.
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

    /// A death whose launch stays counted spends the boot sentinel's counter, so only the
    /// shape that converges under `MAX_BOOT_ATTEMPTS` keeps its count.
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

    /// The invariant that makes the table above safe, against the real constants (mirrors
    /// the compile-time assert beside `STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS`).
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

/// The lane choice and every reason it falls back, each row flipping one field of
/// [`HandoffLaneFacts`] against an otherwise eligible attempt.
#[cfg(all(test, target_os = "macos"))]
#[path = "app_update_handoff_handoff_lane_tests.rs"]
mod handoff_lane_tests;

/// Tier-1 conformance for the seamless handoff's ownership model
/// ([`aterm_spec::derive::native_update_seamless_handoff_ownership_model`]): the final
/// Commit admission, over its whole bounded input space, decides exactly what the model
/// admits.
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

    /// Each window's presence row crosses the handoff: window 0's frame in the window carry,
    /// window 1's in its layout row, so each comes up at its parent's size and grid and the
    /// Commit's lift re-grids nothing. The durable layout carries no row.
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

        // The successor: window 0 from the carry, window 1 from its layout row; each takes
        // its frame before its first attach sizes it.
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

        // Commit: each window's story still wants its row (a settling turn's lease,
        // sensed under the freeze), and the real lift commits it.
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

/// The held capture applies the manifest writer's own bounds, so a screen the writer would
/// drop spends none of the held budget and a later pane that fits is carried.
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
        // PRECONDITION: the three dense screens fit the held budget at their checkpoint
        // size, and with the fourth they would not.
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

/// A paste in flight on a parked session is never cut by the `_exit`, and a reset is
/// never undone by the successor's restore.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_parked_commit_tests.rs"]
mod parked_commit_tests;

/// A park and Commit around a live `video` take, the upgrade rim and a held claim: the take
/// is waited for before `Land`, kept by a park, and answered by the Commit before its
/// `_exit`, never dropped with its connection.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_video_park_tests.rs"]
mod video_park_tests;

/// The launched successor is offered the grant the lane was chosen on.
#[cfg(all(test, target_os = "macos"))]
#[path = "app_update_handoff_launched_lane_grant_tests.rs"]
mod launched_lane_grant_tests;

/// A rejected Commit puts each parked PTY back at its engine's size, which the successor
/// may have changed before Commit.
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

/// The fork lane never captures above a policy it has not waited for, up to the read's
/// ceiling. Negative controls: a known policy, a read that ended with nothing, no read, and
/// a spent ceiling do not wait.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_fork_lane_policy_wait_tests.rs"]
mod fork_lane_policy_wait_tests;

/// A manifest the desk's content puts over its cap (`ManifestOverCap`) goes to the refusal
/// lane, as a capture refusal does; a filesystem refusal, a missing private directory and
/// this build's own inconsistency stay this process's trouble.
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

/// The cold lane leaves the quit layout its successor's boot reopens, native tabs
/// included, or refuses when session restore is off.
#[cfg(all(test, unix))]
#[path = "app_update_handoff_cold_lane_layout_tests.rs"]
mod cold_lane_layout_tests;

/// A refused cold update does not loop: the cold lane reads the verdict before its `exec`
/// ([`crate::App::cold_lane_verdict_gate`]), and a relaunch whose boot swap refused hands
/// its automatic lane that refusal as the verdict ([`super::ColdRelaunch`]).
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
            // The control is `transparent`: under Trust's SP-1 std a `Command` child gets
            // fds 0-2 alone. Its child writes on this process's stdout, so the probe reports
            // into the file `$1` names.
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

    /// The fork-lane successor holds what it is handed and nothing else: a stray descriptor
    /// rides a spawn that hands on the whole table (the control) but not the lane's hook,
    /// while a handed one, close-on-exec here, reaches the successor.
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

    /// The cold lane's `exec` hands over nothing this process holds: a helper re-exec of
    /// this binary holding a stray descriptor replaces itself with a probe, which holds it
    /// through a plain `exec` (the control) and not through [`super::cold_exec_hand_over`].
    /// The helper's exit is the probe's, read only once libtest is seen to have selected
    /// exactly this test ([`crate::test_alone::Running::selected`]).
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

/// The connections a manifest left out are audited as dropped by the Commit alone, right
/// before its write, so an attempt that stands down puts no dissolution on record.
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

    /// Wired where it holds: the worker records what the writer returned, and both of the
    /// Commit's arms audit it right before the one write, past the claim.
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
