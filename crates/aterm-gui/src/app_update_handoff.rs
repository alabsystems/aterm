// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Seamless update handoff: `apply_staged_update_now` and the unix overlap
//! worker it starts (`run_handoff_worker`, the bounded readiness wait, the
//! unique-reaper and rejection plumbing), the reap authority that licenses a
//! rollback (`HandoffRollbackWarrant`), the Commit-time admission facts, and
//! `finish_update_handoff`'s drain gate, Commit, and rollback.
//! A verbatim inherent-impl split of `App`.
//!
//! The successor this module spawns races the parent's in-flight `atpkg` pass
//! exactly as a second window does: the parent's child KEEPS RUNNING once the
//! parent has exited (atpkg's orphan watch re-points its stdio at
//! `orphan-pass.log`, 2026-09-14 — it used to die at its next print), so it holds
//! the store lock for the whole of its pass, and the successor's own launch lanes
//! `--wait-lock` behind it. A successor on a NEW build runs one full update pass
//! after its seed whatever the store's record says (`crate::launch_update_park`,
//! 2026-09-22 — the seed absorbs the wait, so that pass is not the one that stands
//! down); a same-build relaunch owes no pass while the store's last pass is fresh
//! (the due gate, 2026-09-18); an update child that itself waited stands down when
//! the holder finished a pass (`atpkg::cli`); and NO row is painted for the wait
//! (`Wake::PkgLockWaiting` is a log line). A REJECTED candidate is the one shape that is not covered: its
//! sweep SIGKILLs the candidate's process group, atpkg child included (the store
//! is crash-consistent), and the parked parent's loop picks the remainder up at
//! its next tick or bump.

#[cfg(unix)]
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

/// Everything one attempt captured UNDER THE PARK — the facts that depend on
/// every reader being stopped, and nothing else.
///
/// Split out of [`HandoffWorkerJob`] for the late park (2026-09-19): on the
/// launched lane the worker is already running — it has launched the successor
/// and is holding its rendezvous claim — when the main thread finally parks, so
/// the park's product has to cross to the worker as a message of its own
/// ([`HandoffTransferJob`]). The fork lane fills it at the job's construction,
/// exactly as before, because there the park still precedes the spawn.
#[cfg(unix)]
struct HandoffCapture {
    /// When the parent STARTED parking its readers — the zero point of the
    /// freeze the user actually feels, and the same instant the 20 ms capture
    /// deadline is measured from. (It is stamped immediately BEFORE
    /// `park_all_readers`, not after, so the interval includes the park itself;
    /// the park is bounded by that same 20 ms, so the inclusion is negligible
    /// — but the zero point is the park's start.)
    ///
    /// Carried so the worker can report the park->transfer half on the launched
    /// lane at the moment it becomes observable. Data only: nothing branches on it.
    #[cfg(target_os = "macos")]
    park_at: std::time::Instant,
    /// When the main thread finished this capture and handed it over — the
    /// end of the `capture` slice of park->transfer (warm successor P0). Data
    /// only, like `park_at`: the worker logs the split and nothing branches on it.
    #[cfg(target_os = "macos")]
    captured_at: std::time::Instant,
    manifest: crate::session_store::SessionHandoff,
    fds: crate::session_store::HandoffFds,
    screens: Vec<(u64, aterm_core::terminal::TerminalCheckpoint)>,
    /// The sessions the capture carried with a screen that is not exactly what
    /// the program drew — a clamped or a blank screen, never one that lost only
    /// link destinations (`seamless::CarryRung::needs_repaint`) — which the writer
    /// marks `ScreenCarry::repaint` so the successor makes the program redraw
    /// (the 2026-09-22/23 update audit, plan P0-1e).
    repaint: Vec<u64>,
    /// Each session's CONTROL CARRY capture from the freeze — the archive's
    /// fence, counters and differ state, and the engine and ledger to export
    /// the rest from on this worker (`crate::handoff_carry`).
    carries: Vec<crate::handoff_carry::CarrySource>,
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
    _owned_masters: Vec<std::os::fd::OwnedFd>,
    /// THE HISTORY CARRY (`crate::handoff_history`): each handed session's
    /// history fence as the park saw it, and how the lane exports the history
    /// the worker joins it to ([`crate::handoff_history::HistoryPlan`]).
    history_heads: Vec<crate::handoff_history::HistoryHead>,
    history: crate::handoff_history::HistoryPlan,
    /// The final screens of the panes kept open after their commands exited
    /// (round five, item 19): best-effort, outside both digests, written by
    /// the worker as the manifest's side list (`seamless::write_outgoing_with_held`).
    held: Vec<crate::seamless::HeldScreen>,
}

/// The park's product, sent from the main thread to a worker that is already
/// holding a successor's claim (or already knows it must fork instead).
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) struct HandoffTransferJob {
    capture: HandoffCapture,
    /// When the post-park proof wait gives up — computed by the main thread
    /// from the park instant ([`handoff_proof_deadline`]), so the freeze the
    /// user feels is what the deadline bounds, not the launch.
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
/// main thread, so the launch environment can name the manifest before it is
/// written), the channel the park's product arrives on, the channel a typed
/// stand-down arrives on, and how long the worker will hold a dialled successor
/// for a park that never comes.
#[cfg(target_os = "macos")]
struct HandoffPrelaunchLane {
    nonce: String,
    transfer: std::sync::mpsc::Receiver<HandoffTransferJob>,
    stand_down: std::sync::mpsc::Receiver<HandoffStandDown>,
    /// Where the main thread answers `Wake::UpdateHandoffStandingDown`: the
    /// worker waits on this before it revokes, so the park gate is provably
    /// closed and any readers the park had stopped are provably resumed before
    /// anything is killed. See [`retire_ungranted_successor`].
    stand_down_ack: std::sync::mpsc::Receiver<()>,
    hold_bound: std::time::Duration,
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
    /// This attempt ACTIVATES the installed bundle at the executable's own path
    /// (`ApplyAttemptTicket::is_installed_activation`): the pre-verification runs
    /// against THAT bundle (there is no staged `.app`), and the successor is
    /// handed no expected-artifact triple (it has nothing to swap; it simply IS
    /// the newer build).
    installed_activation: bool,
    /// Where a PASS of that pre-verification is published, with the handoff
    /// policy it read (plan P0-5): the same cache the arm-time
    /// `App::spawn_staged_handoff_preverification` fills, under the attempt's
    /// own artifact key. `None` when there is no artifact to name (the
    /// same-image QA seam).
    preverified: Option<PreverifyPublisher>,
    /// The carry ceiling the fork lane's capture was taken under, before this
    /// worker read the candidate's policy — checked against it once read
    /// ([`capture_exceeds_policy`]). `None` on the launched lane, which parks
    /// after this worker's verification.
    parked_under: Option<aterm_update_core::handoff_policy::CarryCeiling>,
    /// The VERIFIED candidate declares the chunked rendezvous grant (its sealed
    /// `Info.plist`, read by a pre-verification after every check passed:
    /// this worker's own, or — when a fresh pass let it skip that — the cached
    /// pass the lane was chosen on). Only then does the launch environment carry
    /// `ATERM_HANDOFF_GRANT_CAPS`, so a successor that never declared it —
    /// every build before the chunked grant — is never offered one.
    successor_grant_chunks: bool,
    command: std::process::Command,
    /// What the park produced. `Some` from construction on the fork lane;
    /// `None` on the launched lane until the main thread's [`HandoffTransferJob`]
    /// arrives, which is AFTER the successor has been launched and has dialled.
    /// Every step from the artifact write onward reads it through
    /// [`Self::capture`], and the pre-grant stand-down never reads it at all.
    capture: Option<HandoffCapture>,
    /// The late park's channels, present exactly on the launched lane.
    #[cfg(target_os = "macos")]
    prelaunch: Option<HandoffPrelaunchLane>,
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
}

/// What the successor's signed handoff policy asks of ONE park (plan P0-5), as
/// [`crate::App::park_policy`] reads it for every lane.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ParkPolicy {
    /// The policy to follow, already narrowed to this build; `None` when there
    /// is none to follow.
    pub(crate) policy: Option<aterm_update_core::handoff_policy::HandoffPolicy>,
    /// Whether a PASSED pre-verification of the attempt's artifact is on record:
    /// `false` means the candidate's policy has not been read yet, so `policy`
    /// says nothing about what it asks.
    pub(crate) known: bool,
    /// The candidate's sealed `Info.plist` DECLARES the chunked rendezvous grant
    /// (read by the same passed pre-verification): the launched lane may then
    /// hand it up to [`crate::handoff_rendezvous::MAX_CHUNKED_SESSIONS`]
    /// sessions, not [`crate::handoff_rendezvous::MAX_RENDEZVOUS_SESSIONS`].
    /// `false` when nothing passed, which is the limit every build before the
    /// chunked grant had.
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
    #[must_use]
    pub(crate) fn relaxes_land_gate(self) -> bool {
        self.policy.is_some_and(|policy| policy.relaxes_land_gate())
    }
}

/// A FORK-LANE PARK TAKEN ABOVE ITS SUCCESSOR'S POLICY is refused before the child
/// starts (the review of plan P0-5): the reason, or `None` when the capture may be
/// handed over.
///
/// The fork lane parks and captures on the main thread BEFORE its worker runs, so
/// when no pass of the candidate was cached its policy was unknown at the park
/// (`parked_under` is the ceiling the capture was taken under: Full). The worker
/// then verifies the candidate and reads the policy — and a policy lower than that
/// ceiling is one this capture did not follow. It is not handed over: the attempt
/// fails, as a producer failure that retries, with the pass and its policy
/// published, so the next park follows it. `parked_under` is `None` on the
/// launched lane, whose park reads the published pass after this verification.
#[cfg(unix)]
fn capture_exceeds_policy(
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
/// (`crate::HandoffPreverification`) for the one artifact its attempt targets.
///
/// WHY THE WORKER PUBLISHES (the 2026-09-22/23 update audit, plan P0-5): the
/// cache used to be filled only when the stage was ARMED, and its verdict stands
/// in for re-running `codesign` for ten minutes. An attempt later than that —
/// a busy terminal, an explicit apply of a stage nothing armed — re-verified on
/// this worker and kept the answer to itself, so the successor's handoff policy
/// it read never reached the launched lane's park, which happens on the main
/// thread after this check. Published, the park reads it by the same key; a
/// REFUSAL is not published, exactly as before (it fails the attempt here).
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

    /// Publish a PASS, with the policy the verification read, narrowed to the
    /// running build `current_build` (and logged once, by `adopt`), and the
    /// candidate's chunked-grant declaration; returns that narrowed policy.
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
    /// The park's product. Every caller sits AFTER the park by construction —
    /// the fork lane fills the capture at construction, the launched lane only
    /// proceeds past its hold once the transfer job has landed — so an absent
    /// capture here is a logic error, not a runtime condition.
    fn capture(&self) -> &HandoffCapture {
        self.capture
            .as_ref()
            .expect("the handoff worker reads the capture only after the park delivered it")
    }
}

/// WHY an attempt may run with no staged artifact to authenticate.
///
/// It means one thing to every gate downstream — "this attempt re-runs the image we are
/// already executing, and swaps nothing". The type is a zero-field discriminant ON
/// PURPOSE: it carries no build, no commit, no digest and no path, so `target_build` and
/// `target_commit` fall through to this build's own by construction and there is no field
/// an authority could point at another image.
///
/// The QA seam is the only one. A provenance self-repair was the other until 2026-09-23,
/// and was retired because a relaunch was measured not to clear the tracking it existed
/// to shed ([`crate::provenance_repair`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SameImageHandoff {
    /// `ATERM_DEBUG_SEAMLESS_REEXEC=1` — the QA seam that exercises the full handoff and
    /// adopt path without a release. Byte-identical in behaviour to before this type.
    DebugSeam,
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
    /// The candidate DECLARES the chunked rendezvous grant: a passed
    /// pre-verification read `ATermHandoffGrantChunks` from its sealed
    /// `Info.plist` ([`ParkPolicy::grant_chunks`]). Then the grant may span
    /// several descriptor messages and the lane carries up to
    /// [`crate::handoff_rendezvous::MAX_CHUNKED_SESSIONS`]; otherwise one
    /// message, [`crate::handoff_rendezvous::MAX_RENDEZVOUS_SESSIONS`]. A
    /// capability the candidate's code declares, never a knob that relaxes a
    /// check: a candidate that did not declare it is handed exactly what every
    /// build before the chunked grant could take.
    grant_chunks: bool,
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

/// THE HOLD FENCE THE COMMIT RAISES ([`App::fence_hold_serials`]): every
/// session the Commit read a hold serial from, fenced so no `hold` lands
/// between that reading and the `_exit` — the stretch in which the successor
/// is activated, the harness suspended and the identity markers written, and
/// in which a halt answered `OK` used to be lost with this process. Dropping
/// it lifts every fence and lets the holds that waited land; a Commit that
/// lands never drops it.
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
    ///
    /// It also requires every session's HOLD to be where the park found it
    /// (`App::hold_serials`, the round-four plan's item 3): a `hold` verb runs
    /// on the control thread, which keeps serving through the overlap, and a
    /// halt that landed after the manifest was drawn would otherwise be
    /// acknowledged here and never reach the successor.
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
/// independently killing healthy in-flight seamless updates (the window's SHOW
/// state — maximized, and on macOS full screen, minimized, stack and key — is
/// the position's class and is normalized beside it):
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
///
/// A SETTINGS LEAF'S CARRIED DRAFTS ARE NOT NORMALIZED, and must not be (plan
/// P2-2). Both captures are `App::capture_handoff_layout`, which puts each
/// Settings view's unsaved field drafts on its leaf; the successor reopens the
/// view holding the PENDING layout's drafts. A key typed into a Settings field
/// while the successor booted therefore makes the two captures differ, and the
/// Commit is refused as activity: the outgoing process keeps the newer text
/// and the next attempt carries it. Normalizing them away would commit a
/// successor holding the older draft.
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
            }
            // Native/placeholder leaves carry no session-lock-derived field, so
            // they keep comparing in full — a native tab appearing or changing
            // IS structural. The terminal leaf's USER metadata
            // (`user_title`/`description`/`icon`/`role`/`attention`) is read
            // under a BLOCKING lock in `view_restore_descriptor`, so it cannot
            // degrade and is deliberately left in the comparison too. It has to
            // stay there: the successor puts those five fields back on the
            // sessions it adopts FROM THE PENDING LAYOUT
            // (`App::carry_restored_identity`, onto the shell each leaf's
            // `local_id` names), so a `meta set` THIS process answers between the
            // capture and the Commit-time re-capture must reject this Commit:
            // admitting it would commit a successor that shows the value from
            // before the edit. That is all the comparison covers. A `meta set`
            // this process answers AFTER the re-capture — its control workers
            // serve until `commit_and_exit` — is lost with it: nothing compares
            // again, and the successor restores the pending value. A `meta set`
            // the SUCCESSOR answers (it serves each adopted bootstrap's sid from
            // its own bind, before its deferred restore) never reaches this
            // process at all; the successor's graft keeps it instead
            // (`session_timeline::restore_carried_meta`).
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
        // Same class as the position: live SHOW STATE, not topology. Linux and
        // Windows captures record it (macOS does not — see
        // `app_restore::TRACKS_NORMAL_FRAME`), and the derived `PartialEq`
        // covers the field, so normalize it here: maximizing the window during
        // the successor's boot must not kill a healthy Commit, exactly the way
        // dragging it used to. (The GRID stays compared: a maximized capture
        // persists its tracked normal grid, which a maximize does not move.)
        window.maximized = None;
        // …and the rest of the macOS SHOW STATE (gap #29), which the unix
        // capture DOES record now: full screen, minimized, the window's place in
        // the stack and whether it is key. All four are live — entering full
        // screen, minimizing, or simply clicking another window while the
        // successor boots changes them — and none is topology. The successor
        // restores the PENDING capture's show state, exactly as it restores the
        // pending capture's position, so a change made during the boot is
        // cosmetic, while rejecting the Commit over it would cost the update.
        window.show = crate::restore::WindowShow::UNKNOWN;
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
#[cfg(unix)]
fn handoff_rejection_activity_shaped(facts: HandoffCommitFacts) -> bool {
    !facts.exact_sessions
        || !facts.exact_layout
        || !facts.exact_activity
        || !facts.teardown_allows_commit
        || !facts.sessions_alive
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
    #[cfg(target_os = "macos")]
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
/// process has to contain ITSELF. `seamless::prearm_incoming_fds` states what
/// this lane still cannot match (the B3 residual gap); the call lives in
/// [`crate::main_entry`] — still ahead of everything in that process able to run
/// another program — rather than in `prearm_incoming_fds`, because that function
/// is also exercised IN-PROCESS by unit tests, and a process-wide, irreversible `setpgid` inside it would move the
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
    /// Our child, and it has not exited — stopped counts as not exited: whatever
    /// ends it has not happened yet.
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
/// `si_code` rather than `si_pid`/`si_status` is the portability decision: it is
/// a PUBLIC FIELD of libc's `siginfo_t` on both platforms this crate builds for,
/// while the other two are fields on the BSDs and union accessors on Linux. A
/// child that has not changed state leaves the zeroed struct untouched, so
/// `si_code == 0`; an exit writes `CLD_EXITED`, `CLD_KILLED` or `CLD_DUMPED`.
/// NOT `si_signo != 0`, which this used to test: Darwin reports a STOPPED child
/// here even though only `WEXITED` was asked for (measured on Darwin 27.0.0:
/// `si_signo = SIGCHLD`, `si_code = CLD_STOPPED`), so a successor a debugger or
/// a job-control signal had paused read as dead. The exit STATUS is read later,
/// in safe Rust, from the `Child::wait` this lane already performs.
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
    match info.si_code {
        libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED => HandoffCandidateProbe::Exited,
        _ => HandoffCandidateProbe::Running,
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
///
/// THE KNOTE IS ONE-SHOT: the read that returns it dequeues it (`EV_ONESHOT |
/// EV_EOF`; measured 2026-09-27, a second zero-timeout read answers nothing). So
/// the watch KEEPS what it read, and asking twice answers twice. Without that,
/// the hold loop's "did it exit?" check spent the status and the stand-down after
/// it recorded the candidate's own exit as `Unobserved`.
#[cfg(target_os = "macos")]
struct CandidateExitWatch(aterm_uds::exitwatch::ExitWatch);

/// The kqueue half moved to `aterm_uds::exitwatch` (2026-09-28) so the PTY
/// keeper classifies a window's death with the same witness; what stays here is
/// the handoff's reading of it.
#[cfg(target_os = "macos")]
impl CandidateExitWatch {
    /// Register WHILE THE CANDIDATE IS STILL ALIVE. On the launched lane that is
    /// the rendezvous accept — the one instant the kernel has just attested the
    /// pid. Every failure answers `None` and is SAFE: it costs the evidence,
    /// never the reap ([`crate::ChildDeathEvidence::Unobserved`]).
    fn watch(pid: u32) -> Option<Self> {
        aterm_uds::exitwatch::ExitWatch::watch(pid).map(Self)
    }

    /// The candidate's own `wait(2)` status IF THE KERNEL HAS ALREADY RECORDED
    /// ONE; never blocks, never reaps, and answers the same on every later call.
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

/// SIGKILL a candidate whose identity was just CORROBORATED — its group when
/// `-pid` provably names the candidate's own group, then the candidate itself.
///
/// Two proofs license the group signal, and either suffices:
/// * [`candidate_leads_its_own_group`] — the kernel reports the candidate leads
///   group `pid` (the launched lane, whose candidate contained itself);
/// * [`candidate_is_our_child`] — the fork lane's pin. `pre_exec` put the child in
///   group `pid` before its image ran, and an unreaped child owns its number, so
///   `-pid` names that group or nothing. This one is not redundant with the
///   first: a leader that exited between the identity read and here is a zombie,
///   and Darwin answers `getpgid` on a zombie with ESRCH (measured) — without the
///   pin the helpers it left in its group would outlive the reject.
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
///   the same one. Our KNOWLEDGE of it is a kernel read at the moment of use:
///   the pid is identity-corroborated in that arm, so `getpgid(pid) == pid`
///   ([`candidate_leads_its_own_group`]; no same-session restriction on Darwin
///   or Linux) says whether the candidate leads a group of its own. The group
///   SIGKILL is sent only then — or when the candidate is our own unreaped child,
///   the fork lane's pin ([`kill_corroborated_candidate`]); a candidate that
///   never reached its containment gets the direct signal alone, and the log
///   says so. What licenses rollback
///   is still [`handoff_candidate_terminated`], never the group signal.
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
        #[cfg(target_os = "macos")]
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
    /// could not be registered. macOS only: nothing else launches a candidate.
    #[cfg(target_os = "macos")]
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
            #[cfg(target_os = "macos")]
            Self::Launched(watch) => watch.as_ref().and_then(CandidateExitWatch::exit_status),
        }
    }

    /// Has the candidate PROVABLY ENDED — asked without changing anything, so the
    /// reject path that follows still finds every fact it reads.
    ///
    /// WHY THE WAITS ASK THIS AT ALL, rather than trusting the pipes. The parent
    /// learns of a death from EOF on the readiness channel, and EOF needs EVERY
    /// copy of the write end closed. The candidate's copy dies with it, but a copy
    /// a sibling fork caught in its fork-to-exec window — or kept, where the
    /// pipe's close-on-exec flag was set after it was created — hides the death
    /// for as long as that sibling lives. The successor already refuses to trust
    /// the mirror-image pipe for the same reason (`seamless::CommitReceiver`
    /// probes its parent every tick); this is the parent's half.
    ///
    /// NON-DESTRUCTIVE ON BOTH LANES, and each answer is one the reject path
    /// already relies on:
    ///   * FORK LANE: [`probe_handoff_candidate`]'s `waitid(WNOWAIT)`. An exited
    ///     child stays waitable, so `Child::wait` still collects its status and
    ///     its unreaped entry still pins the pid for the group sweep;
    ///   * LAUNCHED LANE: [`handoff_candidate_terminated`], the pid-vacancy and
    ///     identity proofs. It never reads the [`CandidateExitWatch`]: that is the
    ///     only witness to how this candidate died, its one-shot knote is taken by
    ///     the first read (the watch keeps what it read), and the reads that own
    ///     it are the hold loop's and the stand-down's. A zombie launchd has not
    ///     reaped yet still answers `false` here; that costs only the wait until
    ///     launchd reaps it.
    fn candidate_gone(&self, candidate: HandoffCandidate) -> bool {
        match self {
            // Only while the child IS the candidate: a launcher-shaped child exits
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

/// PRE-PARK admission peek only: a readable byte on a LIVE master — output its
/// reader has not taken yet. Automatic mode refuses to even BEGIN an overlap
/// while output is actively flowing (the quiet-epoch policy). Once an attempt
/// is in flight this function must NOT be used — mid-flight, queued output is
/// tolerated and only session death revokes; use [`handoff_masters_closed`]
/// there.
///
/// POLLIN ONLY, AND NEVER FROM A HUNG-UP MASTER (the 2026-09-22/23 update
/// audit, plan P1-3). This used to count `POLLHUP`/`POLLERR`/`POLLNVAL` as
/// "output waiting" too. A dead master reports them forever — and macOS reports
/// a dead slave as `POLLIN|POLLHUP`, so even the `POLLIN` bit lies for it — so a
/// pane kept open after its command exited (`--hold`) answered "a session has
/// output waiting" at every park gate for as long as it stayed open: every
/// automatic attempt held its successor 120 s and stood down, every ~15 min,
/// forever. A dead session is not output; it is death, which
/// [`handoff_masters_closed`] and the post-park re-check already own.
#[cfg(unix)]
pub(crate) fn handoff_masters_have_activity(live: &[(u64, i32, i32)]) -> bool {
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
    polled > 0
        && fds
            .iter()
            .any(|fd| fd.revents & libc::POLLIN != 0 && fd.revents & dead == 0)
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
pub(crate) fn handoff_masters_closed(live: &[(u64, i32, i32)]) -> bool {
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
/// session is gone, `sessions_alive`/`exact_sessions` own that rejection. A
/// live pool entry uses its sink-local counter; only an orphaned master needs
/// the writer registry, because a queued Job can keep its sink alive after
/// the pool entry is removed. The ordinary key path never pays for that lock.
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
    if publish_warranted_handoff_failure(warrant, cleanup, proxy, completion) {
        send_post_failure_reconcile_facts(cleanup, proxy, current_build);
    }
}

/// The first half of [`send_warranted_handoff_failure`]: the completion
/// itself. `false` when the event loop is gone, and then no disk facts follow.
///
/// Split out for the two reject paths that may GIVE BACK the candidate's
/// counted trial launch: they settle the boot sentinel between the two halves,
/// so the disk facts that follow read the count the sentinel will keep. Read
/// before the give-back, those facts said one launch more than the trial
/// holds, and the automatic lane reads that count to decide whether its next
/// launch of the build is the one the trial reverts on (gap 14 review,
/// 2026-09-26).
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
        // WRITE THE EVIDENCE DOWN. The completion detail lands on the update bar's
        // row and stays short, so the durable log is where a future field report finds out which
        // of the three `ChildDied` events this was — and, when the answer is
        // `Unobserved`, that the answer is genuinely unknown rather than assumed.
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
    //
    // BUT BEFORE THE DISK FACTS (gap 14 review, 2026-09-26): they carry the
    // sentinel's count, and a structural latch reads it to decide whether its
    // next launch of the build is the one the trial reverts on. Collected first,
    // they said one launch more than the trial keeps — enough to withdraw a
    // retry the trial could afford.
    if forgives_the_counted_trial_launch(child_death, job.current_build, job.target_build) {
        aterm_update::forgive_trial_launch_if_advanced(job.target_build, job.trial_launches_before);
    }
    if published {
        send_post_failure_reconcile_facts(&job.cleanup, proxy, job.current_build);
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

/// THIS process could not prepare the handoff — a write, a descriptor, a
/// `spawn`, an event loop or an attempt record gone, no private directory.
/// Raised BEFORE any candidate held a master (so the warrant is
/// `NoCandidate`), and filed `ProducerFailed` — Transient — because none of it
/// is about the candidate's bytes (the 2026-09-22/23 update audit, plan P1-2).
/// [`send_handoff_preparation_failure`] is kept for the one failure that is:
/// the staged candidate's pre-park verification.
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

/// The staged candidate failed its PRE-PARK VERIFICATION — the one preparation
/// failure that is a verdict about the bytes, and so the only one still filed
/// `PreparationFailed` (Structural), unless the verifier never reached a verdict
/// ([`pre_park_refusal_outcome`]). Raised BEFORE `spawn`, so no candidate has
/// ever held a master.
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
/// `PreparationFailed` (STRUCTURAL) is the verifier's VERDICT on the candidate:
/// a codesign that refused, a sealed identity that does not rebind, an installed
/// bundle that cannot be the rollback source. A refusal it reached WITHOUT a
/// verdict — a helper that ran out of the apply budget, the apply lock held by a
/// sibling past its wait, a helper the kernel would not start just then
/// ([`aterm_update::is_passing_refusal`]) — is this process's afternoon, which is
/// what `ProducerFailed` (TRANSIENT) already means (round four, plan item 2).
/// Filed structural, two such moments converged a healthy build for a day.
#[cfg(unix)]
#[must_use]
pub(crate) fn pre_park_refusal_outcome(error: &str) -> crate::UpdateHandoffOutcome {
    if aterm_update::is_passing_refusal(error) {
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
/// reader for a successor whose gate 4b then retired the stage. `None` when the
/// worker verifies anyway, and for the same-image QA seam (no artifact, so no
/// publisher and nothing to yank). `floor` is
/// [`aterm_update::handoff_apply_floor_refusal`], injected for the tests.
#[cfg(unix)]
fn skipped_verification_floor_refusal(
    job: &HandoffWorkerJob,
    floor: impl FnOnce(u64) -> Option<String>,
) -> Option<String> {
    if job.verify_staged_candidate || job.preverified.is_none() {
        return None;
    }
    floor(job.target_build)
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
        // ACTIVATION: the artifact is the bundle under the running executable.
        // Prove it is exactly the authorized sealed identity, codesign-valid and
        // newer than this process — a bundle swapped again since the observation
        // is refused before a single reader is parked.
        aterm_update::preverify_installed_for_handoff(
            job.current_build,
            job.target_build,
            &job.target_commit,
        )
    } else {
        aterm_update::preverify_staged_for_handoff(
            job.current_build,
            Some(crate::build_info::GIT_COMMIT),
            Some(job.target_build),
            Some(&job.target_commit),
        )
    }
}

/// THE WORKER'S FIRST REAL ACTION (seamless seam 1), with its two readers
/// injected so a test drives the decision the worker ships (round seven, item
/// 106): `verify` is [`verify_candidate_for_handoff`], `floor`
/// [`aterm_update::handoff_apply_floor_refusal`].
///
/// The `codesign --deep` + bundle-flock authenticity check that used to freeze
/// the main thread before every handoff runs HERE, so a doomed candidate is
/// refused before any manifest is written or any child is spawned — and the UI
/// thread never blocks on it. Still strictly additive: the child re-runs the
/// complete gate under the apply lock at swap time. A refusal is an ordinary
/// `PreparationFailed` (Structural) — unless the verifier never reached a
/// verdict (a timeout, a held lock), which is `ProducerFailed` (Transient;
/// [`pre_park_refusal_outcome`]). A worker that skips the check on a fresh
/// cached pass still asks the floor ([`skipped_verification_floor_refusal`]).
#[cfg(unix)]
fn worker_pre_park_check(
    job: &mut HandoffWorkerJob,
    verify: impl FnOnce(&HandoffWorkerJob) -> Result<aterm_update::HandoffCandidateFacts, String>,
    floor: impl FnOnce(u64) -> Option<String>,
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
        let facts = verify(job).map_err(refused)?;
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
        // is the one fact the cache cannot vouch for: it ratchets on observation
        // and may have risen since the pass was cached (round six, item 26,
        // review round two). The same refusal, and the same outcome, the full
        // check gives.
        return Err(refused(error));
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

    // STAGED-CANDIDATE PRE-VERIFICATION (seamless seam 1), off the GUI thread
    // ([`worker_pre_park_check`]): a refusal is reported and ends the attempt
    // before any manifest is written or any child is spawned.
    if let Err(refusal) = worker_pre_park_check(
        &mut job,
        verify_candidate_for_handoff,
        aterm_update::handoff_apply_floor_refusal,
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

    // THE LANES SPLIT HERE, BEFORE ANYTHING IS WRITTEN (2026-09-19, the late
    // park). The launched lane arrives with NO capture: the main thread returned
    // with every reader live, and this worker now launches the successor, holds
    // its rendezvous claim, and asks the main thread to park only once the
    // successor has dialled — so the swap, the second `execve` and the boot check
    // all happen while the terminal still echoes. That lane finishes the attempt
    // itself (committed, rolled back or reported) and returns; only a runtime
    // refusal that happened before anything was launched (no bundle, no
    // rendezvous, LaunchServices) hands the attempt back to be FORKED, and then
    // it rejoins here WITH the main thread's capture, exactly where the fork lane
    // — which still parks before it spawns — starts.
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
                        // THE POST-PARK DEADLINE IS DROPPED ON PURPOSE. It bounds
                        // the freeze for a successor that has ALREADY swapped,
                        // re-exec'd and boot-checked with the readers live; the
                        // child forked below has done none of that yet and must
                        // do all of it under the park, which is what the fork
                        // lane has always cost and what `handoff_ready_deadline`
                        // (30 s) has always budgeted. Handing it 3 s would fail
                        // every fork-after-park attempt by construction.
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

    // SNAPSHOT THE TRIAL COUNTER HERE, on the WORKER, after the (slow) codesign
    // pre-verification and as late as the lane allows: taken on the main thread at
    // job construction it both froze the terminal for a `Staging::resolve` (which
    // chmods) and could attribute a THIRD party's launch — counted during our own
    // pre-verification — to this candidate (2026-08-19 round-4 skeptics). (The
    // launched lane takes its own snapshot immediately before its launch; a
    // launch LaunchServices refused counted nothing, so the fork below is the
    // first counted launch on that path too.)
    job.trial_launches_before = aterm_update::trial_launch_count(job.target_build);

    let PreparedArtifacts {
        manifest_path: path,
        layout_path,
        fds_wire: wire,
        expected,
        proof_rd,
        proof_wr,
        commit_rd,
        commit_wr,
    } = match prepare_outgoing_artifacts(&mut job, &nonce) {
        Ok(prepared) => prepared,
        Err(failure) => {
            report_preparation_failure(&job, &proxy, nonce, failure);
            return;
        }
    };

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
        // THE LAUNCHER'S SOFT DESCRIPTOR LIMIT, when a claim raised this
        // process's own (round seven, item 107): the forked child inherits the
        // raised limit and never claims, so without this its shells would get
        // the raise back instead of the launcher's.
        if let Some((key, value)) = crate::handoff_rendezvous::launcher_limit_env() {
            job.command.env(key, value);
        }
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
        .capture()
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
    // its OWN application job — is the `HandoffLane::OutOfBand` lane above,
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
    // B3 on the launched lane: `pre_exec` establishes the candidate's process
    // group before `spawn` returns, so on THIS lane `kill(-pid)` is a valid
    // handle from that instant. A launched successor contains itself instead
    // and reports nothing, so there the group sweep is gated on a kernel read
    // at the moment of use (`getpgid(pid) == pid`, against a pid the
    // rendezvous's kernel-attested `LOCAL_PEERPID` corroborated) —
    // `signal_handoff_candidate` states what each reaper may conclude.
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
    // Capture the candidate's kernel identity while its pid is still PINNED by
    // being an unreaped child of ours. Every later reap authority reads it, so
    // it has to be taken at the one instant the pid provably names the
    // candidate. On the launched lane there is no such pin, and the identity
    // arrives from the rendezvous accept instead (`of_attested_peer`); nothing
    // downstream of here can tell the difference.
    let candidate = HandoffCandidate::of_unreaped_child(&child);
    // THE PTY KEEPER'S PENDING (P3, opt-in; design §5.3 step 8): the fork lane's
    // grant is the Commit below, which lets the child read; its duplicates came
    // with the fork, so the keeper is told the child's pid here, before any
    // proof or Commit — a death of this window from here on is a handoff while
    // the child lives. Bounded; the holder scan stands alone if it failed.
    crate::keeper_link::pending_before_grant(candidate.pid);
    // THIS PROCESS STILL ANSWERS TO THE CARRIED IDS until the Commit, whatever
    // discovery entry the candidate publishes for them first (`identity_claim`,
    // "The other order"). Held to the end of this function: a Commit `_exit`s
    // with it held, and every rejection returns after the reap.
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
    /// The two private pipes of one attempt, before either end has been given
    /// away. Which END goes to the successor is the whole protocol (the successor
    /// writes the proof and reads the Commit), and a swapped pair would deadlock
    /// in a way that reads as a slow successor.
    proof_rd: std::os::fd::OwnedFd,
    proof_wr: std::os::fd::OwnedFd,
    commit_rd: std::os::fd::OwnedFd,
    commit_wr: std::os::fd::OwnedFd,
}

/// Why [`prepare_outgoing_artifacts`] stopped. Typed rather than reported so the
/// two callers can attach the warrant each of them actually holds: the fork lane
/// has no candidate at all, while the launched lane is holding a dialled
/// successor that must be stood down and proven gone before the attempt retires.
#[cfg(unix)]
enum PreparationFailure {
    /// The main thread poked the cancel channel: structural activity revoked
    /// the attempt (typed `ActivityRevoked` for the retry budget).
    Cancelled,
    /// A step of THIS process's own preparation refused — a write, a pipe, its
    /// own carry disagreeing with itself; the detail is the status-bar row's
    /// sentence, with the `std::io::ErrorKind` when the filesystem said one.
    /// Nothing here is about the candidate, so it is filed `ProducerFailed`
    /// (Transient), never `PreparationFailed` (the 2026-09-22/23 update audit,
    /// plan P1-2: an `ENOSPC` here latched the artifact after two attempts).
    Producer(String),
    /// The desk's own content refused the write DETERMINISTICALLY: the
    /// manifest would be over the cap its successor reads it under with every
    /// optional part shed ([`crate::seamless::WriteOutgoingFailure::ManifestOverCap`]).
    /// The next attempt captures the same desk and meets the same cap, so it is
    /// filed `CaptureRefused` — the refusal lane, retried once the desk changes
    /// — never `ProducerFailed`, whose transient retries would freeze every
    /// reader again for the same answer (round seven, item 108; law L4).
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
        crate::seamless::WriteOutgoingFailure::ManifestOverCap => {
            PreparationFailure::Refused(format!("{detail}; the lane retries once the desk changes"))
        }
        crate::seamless::WriteOutgoingFailure::NoPrivateDir
        | crate::seamless::WriteOutgoingFailure::Inconsistent
        | crate::seamless::WriteOutgoingFailure::Io(_) => PreparationFailure::Producer(detail),
    }
}

/// Every artifact one attempt publishes, in the order the successor consumes
/// them, from the park's capture: the layout round-trip, the control carry's
/// export, the authenticated manifest and its grid sidecars under `nonce`, the
/// layout sidecar, the adoption proof, and the two private pipes. Shared by both
/// lanes so the bytes the successor reads cannot differ by how it was started.
///
/// Reads the capture, so it runs AFTER the park on both lanes; nothing here
/// touches a master, and every reader is stopped (until Commit or rollback), so
/// exporting up to 1 MiB of archive rows per session costs the frozen terminal
/// nothing that a byte-for-byte copy would not.
#[cfg(unix)]
fn prepare_outgoing_artifacts(
    job: &mut HandoffWorkerJob,
    nonce: &str,
) -> Result<PreparedArtifacts, PreparationFailure> {
    let target_build = job.target_build;
    let target_commit = job.target_commit.clone();
    let capture = job
        .capture
        .as_mut()
        .expect("the artifacts are prepared from the park's capture");
    let layout_roundtrip = capture
        .layout
        .to_toml()
        .ok()
        .and_then(|wire| crate::restore::RestoreManifest::from_toml(&wire));
    if capture.layout.is_empty() || layout_roundtrip.as_ref() != Some(&capture.layout) {
        return Err(PreparationFailure::Producer(
            "could not persist the bounded handoff layout".to_string(),
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
    // so a ledger that could not be carried cannot lower it.
    let controls = crate::handoff_carry::export(&capture.carries);
    capture.manifest.next_turn_id =
        crate::handoff_carry::manifest_turn_id(crate::control::turn_ids_minted());
    // THE HISTORY CARRY'S JOIN (`crate::handoff_history`), here where the
    // capture meets the export that ran with every reader live: each session
    // whose export still describes its history, and meets the lines its
    // checkpoint carries, names its sidecar on its record; every other one
    // keeps the checkpoint's bounded history, and the lines that do not cross
    // are COUNTED on its record. Nothing here can fail the attempt.
    // AFTER the control carry's export on purpose: the fork lane's history
    // export runs here, and the two must not contend for the same locks.
    if let Some(dir) = crate::control_auth::socket_dir() {
        let results = std::mem::replace(
            &mut capture.history,
            crate::handoff_history::HistoryPlan::Unexported,
        )
        .results(&dir);
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
    // The attempt nonce was minted by the CALLER of this writer (2026-09-19): the
    // late park launches the successor with the manifest's path before the
    // manifest exists, so the name must be derivable before the write, and the
    // echoed `outgoing.nonce` below is that very value.
    let outgoing = crate::seamless::write_outgoing_with_held(
        &capture.manifest,
        &capture.fds,
        &capture.screens,
        &capture.repaint,
        capture.window.clone(),
        &controls,
        &capture.held,
        nonce,
    )
    .map_err(preparation_failure_of_write)?;
    let crate::seamless::OutgoingHandoff {
        manifest_path: path,
        nonce: echoed_nonce,
        fds_wire,
        screen_digest: carried_screen_digest,
    } = outgoing;
    // THE ONE NAME: the late park hands the successor this path BEFORE the
    // writer runs, so the writer's answer must be the path the launch
    // environment would have named from the same nonce.
    debug_assert_eq!(
        crate::seamless::outgoing_manifest_path(nonce).as_deref(),
        Some(std::path::Path::new(&path)),
        "the manifest writer and the launch environment name the manifest differently"
    );
    debug_assert_eq!(echoed_nonce, nonce, "the writer echoes the minted nonce");
    // BYTE-IDENTITY ASSERTION. `capture.screen_digest` was committed on the main
    // thread from the live checkpoints; `carried_screen_digest` was taken over
    // the bytes just written. The child hashes those SAME bytes, so a divergence
    // here would surface later as an unexplained `AdoptionMismatch`. Catch it now
    // as an ordinary, explained preparation failure instead.
    if carried_screen_digest != capture.screen_digest {
        return Err(PreparationFailure::Producer(
            "handoff screen carry did not match the committed screen digest".to_string(),
        ));
    }
    if job.cancel.try_recv().is_ok() {
        return Err(PreparationFailure::Cancelled);
    }
    let layout_path = std::path::Path::new(&path).with_extension("layout.toml");
    if let Err(error) = crate::restore::write_once_to(&layout_path, &capture.layout) {
        return Err(PreparationFailure::Producer(format!(
            "could not write the attempt-bound handoff layout ({error})"
        )));
    }
    if job.cancel.try_recv().is_ok() {
        return Err(PreparationFailure::Cancelled);
    }
    let Some(expected) = crate::seamless::adoption_proof(
        nonce,
        target_build,
        &target_commit,
        &capture.layout_digest,
        &capture.screen_digest,
        // NOT `capture.live` — the term the two sides can both compute depends
        // on how the descriptors travel. On the fork lane these are the same
        // vector; on the out-of-band lane the middle field is the PTY device.
        &capture.proof_identities,
    ) else {
        return Err(PreparationFailure::Producer(
            "handoff identity set exceeds the proof format".to_string(),
        ));
    };
    let Some((proof_rd, proof_wr)) = make_cloexec_pipe() else {
        let kind = std::io::Error::last_os_error().kind();
        return Err(PreparationFailure::Producer(format!(
            "could not create the adoption-proof channel ({kind})"
        )));
    };
    let Some((commit_rd, commit_wr)) = make_cloexec_pipe() else {
        let kind = std::io::Error::last_os_error().kind();
        return Err(PreparationFailure::Producer(format!(
            "could not create the handoff-commit channel ({kind})"
        )));
    };
    if job.cancel.try_recv().is_ok() {
        return Err(PreparationFailure::Cancelled);
    }
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
/// the warrant is `NoCandidate` and the completion is exactly what each inline
/// arm used to send — a cancel is `ActivityRevoked` — except that this
/// process's own refusal is `ProducerFailed` since the 2026-09-22/23 audit
/// (plan P1-2), where it used to be `PreparationFailed`.
#[cfg(unix)]
fn report_preparation_failure(
    job: &HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    nonce: String,
    failure: PreparationFailure,
) {
    match failure {
        PreparationFailure::Cancelled => send_warranted_handoff_failure(
            HandoffRollbackWarrant::NoCandidate,
            &job.cleanup,
            proxy,
            job.current_build,
            crate::UpdateHandoffCompletion::failure(
                job.attempt_id,
                Some(nonce),
                None,
                // Typed activity classification: a cancel poke during preparation
                // is user/structural activity, never evidence against the staged
                // artifact — automatic mode may re-attempt at a later quiet window.
                crate::UpdateHandoffOutcome::ActivityRevoked,
                "activity revoked handoff during physical preparation",
            ),
        ),
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
    /// A runtime refusal BEFORE anything was launched. The main thread has since
    /// parked and sent its capture, and the caller forks with it under the
    /// pre-minted nonce — the trade is the orphaned launchd domain the launched
    /// lane exists to avoid, which the fork lane already makes today, and it
    /// beats not updating at all on a machine LaunchServices refuses.
    ForkInstead {
        nonce: String,
        transfer: Box<HandoffTransferJob>,
    },
}

/// A successor that has DIALLED and is being HELD: the rendezvous it dialled,
/// the accepted claim, its kernel-attested identity and its exit witness. It
/// holds no descriptor — the one `sendmsg` that grants them is the transfer, and
/// that happens only after the park — so standing it down is closing the
/// rendezvous (EOF) and proving it gone, never a rollback of anything.
#[cfg(target_os = "macos")]
struct HeldSuccessor {
    rendezvous: crate::handoff_rendezvous::Rendezvous,
    peer: crate::handoff_rendezvous::ClaimedPeer,
    candidate: HandoffCandidate,
    exit_watch: Option<CandidateExitWatch>,
    dialled_at: std::time::Instant,
}

/// ONE STEP of a pre-grant stand-down, recorded as it is taken so the order can
/// be PROJECTED onto the derived overlap model rather than restated by a test.
///
/// The model's ungranted lane is `UnparkForRepark?` -> `RevokeUngrantedSuccessor`
/// -> (`SuccessorExitsOnRevoke` | `KillUngrantedSuccessor`) ->
/// `ReapUngrantedSuccessor` -> `RetireUngrantedAttempt`, and
/// `UngrantedRetireRequiresReap` is what forbids clearing an attempt whose
/// successor is still alive. A test that types that sequence proves nothing
/// about this file; one that replays what the code EMITTED does.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StandDownStep {
    /// The rendezvous closed: EOF for the successor, which is the revocation.
    Revoke,
    /// It exited on that EOF by its own rule, inside the grace.
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
    /// grace and had to be killed — the one case in which THIS process forgives
    /// its counted trial launch (a successor that exits on EOF forgives its own).
    signalled: bool,
    /// The steps this stand-down actually took, in order.
    steps: Vec<StandDownStep>,
}

/// How long a held successor is given to exit BY ITS OWN RULE after its
/// rendezvous closes, before it is killed. The successor maps a zero-byte
/// `recv` to "the parent closed the rendezvous", forgives its trial launch and
/// exits before any window (`main_entry`), which takes milliseconds; the grace
/// is for a machine under load. Readers are LIVE (or resumed) throughout, so the
/// grace costs the user nothing — it is spent only when the readers are not
/// parked, which the caller guarantees by unparking first.
#[cfg(target_os = "macos")]
const STAND_DOWN_SELF_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// How long the worker waits for the main thread to answer a stand-down
/// announcement before revoking anyway. Generous against a busy event loop (the
/// handler itself is a `take` and a reader resume), and far short of the grace
/// it precedes, so a wedged main thread costs a late reader resume rather than a
/// candidate left alive.
#[cfg(target_os = "macos")]
const STAND_DOWN_ACK_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);

/// How long the WORKER holds a dialled successor for a park that never comes:
/// the backstop behind the main thread's own hold cap
/// (`PRELAUNCH_HOLD_MAX`), and shorter than the successor's grant budget
/// (`GRANT_HOLD_BUDGET` in `main_entry`) so the parent stands the successor
/// down by EOF before the successor times out on its own.
#[cfg(target_os = "macos")]
const PRELAUNCH_HOLD_BOUND: std::time::Duration = std::time::Duration::from_secs(4 * 60);

/// Stand a held successor down and PROVE it gone: close the rendezvous (EOF is
/// the stand-down signal the successor already understands), wait the grace for
/// its own exit, then kill and prove termination if it is still there.
///
/// THE ORDER IS THE MODEL'S. `RevokeUngrantedSuccessor` (the EOF) precedes
/// `KillUngrantedSuccessor`, which precedes `ReapUngrantedSuccessor`, and only a
/// reaped attempt may retire (`UngrantedRetireRequiresReap`): a candidate that
/// ever dialled is provably dead before its attempt record is cleared or any
/// retry launches beside it. A successor that exits on the EOF within the grace
/// is proven gone the same way, without a signal.
#[cfg(target_os = "macos")]
fn stand_down_held_successor(
    held: HeldSuccessor,
    grace: std::time::Duration,
) -> StoodDownSuccessor {
    stand_down_held_successor_between(held, grace, &mut || {})
}

/// [`stand_down_held_successor`], with the one instant its evidence depends on
/// named: `between_read_and_probe` runs after every witness read that found
/// nothing and BEFORE the pid probe that follows it. The shipping call passes a
/// no-op. A test passes the candidate's exit and reap, which is the losing
/// interleaving a loaded machine produces one preemption at a time (2026-09-27:
/// 110 of 1500 exits) — so the evidence rule below is pinned deterministically,
/// not by a run under load.
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
    // EOF IS THE STAND-DOWN. Closing the accepted stream ends the successor's
    // `recv_with_fds` with zero bytes and no descriptors; dropping the rendezvous
    // closes the listener and unlinks the node, so a re-dial fails closed too.
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
        // candidate's OWN death, which is why `died_before_we_signalled` is true.
        steps.push(StandDownStep::SuccessorExited);
        let warrant = wait_for_handoff_candidate_to_terminate(candidate);
        steps.push(StandDownStep::Reap);
        // AFTER THE PROOF, ASK AGAIN. The loop reads the witness and THEN probes
        // the pid, so a candidate that exits and is reaped between the two — one
        // preemption wide — leaves the loop proven gone with the witness unread.
        // Measured 2026-09-27 under a loaded machine: 110 of 1500 exits landed
        // there, and `a_successor_that_exits_on_eof_is_proven_gone_without_a_signal`
        // saw the candidate's own `exit(0)`, the refusal this classification most
        // wants, recorded as `Unobserved` in one run in ten. XNU queues the knote
        // inside `proc_exit`, before the pid can fall vacant, so once termination
        // is proven this read finds it (110 of 110), and `kill(pid, 0)` answers
        // a ZOMBIE as alive (measured 2026-09-28), so no probe here can call the
        // pid vacant before that knote exists.
        // `the_exit_between_the_witness_read_and_the_pid_probe_is_still_witnessed`
        // forces the window through `between_read_and_probe`.
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

/// Retire a held, UNGRANTED successor: stand it down, prove it gone, and only
/// then publish the completion that clears the attempt. The sibling of
/// [`worker_reject_and_reap_handoff_child`] for the one state that function
/// cannot describe — a live candidate holding nothing.
///
/// THE MAIN THREAD GOES FIRST, and this is why the announcement is a wait and
/// not a poke (2026-09-19, review round 1). Whether the readers are parked when
/// a stand-down begins is NOT a fact this thread can know: the main thread's
/// park gate re-runs every 20 ms while the successor holds, so a park can land
/// at any instant up to and including the moment the hold loop decides to give
/// up. Passing that guess as a parameter was wrong twice over — a park that
/// landed during the grace would have kept the terminal frozen through the
/// grace, the SIGKILL and the reap, and a park that started afterwards would
/// have frozen it onto a successor that was already dead.
///
/// So `Wake::UpdateHandoffStandingDown` is sent UNCONDITIONALLY and waited for.
/// Its handler runs on the main thread, where the park gate also runs, so the
/// two cannot interleave: it closes the gate for good (`stood_down`) and, if a
/// park had landed, resumes the readers there and then (the model's
/// `UnparkForRepark`). Only after that answer does this thread revoke, which is
/// what makes the modelled order Unpark -> Revoke -> Kill -> Reap -> Retire real
/// rather than merely requested.
///
/// The wait is BOUNDED and fails open: a main thread that cannot answer within
/// [`STAND_DOWN_ACK_BUDGET`] is one that is not going to resume readers either,
/// and a candidate that is never killed is worse than a late unpark. The attempt
/// record outlives all of it until this completion, so no retry can launch
/// beside the candidate being stood down.
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
    // THE ORDER IT TOOK, in the log and in the completion's evidence. Each step
    // is a derived-model action (`StandDownStep::model_action`), and the tests
    // replay exactly this sequence against the model — so a field report of a
    // stand-down that went wrong names the step, not a symptom.
    aterm_log::info!(
        "update apply: stood the held successor (pid {}) down after holding its claim {} ms — \
         {detail}; steps {:?} ({})",
        candidate.pid,
        held_for.as_millis(),
        stood.steps,
        if stood.signalled {
            "it ignored the closed rendezvous for the grace and was killed and proven terminated"
        } else {
            "it exited on the closed rendezvous by its own rule"
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
    // THE COUNTED TRIAL LAUNCH IS FORGIVEN BY EXACTLY ONE PARTY: a successor that
    // exits on the EOF forgives its own (`main_entry`'s claim-error exit); this
    // process forgives only when it had to kill, and only when the death is not
    // the bytes' fault — `forgives_the_counted_trial_launch` is the decision.
    // Before the disk facts, so they carry the count the sentinel keeps (see
    // `publish_warranted_handoff_failure`).
    if stood.signalled
        && forgives_the_counted_trial_launch(stood.death, job.current_build, job.target_build)
    {
        aterm_update::forgive_trial_launch_if_advanced(job.target_build, job.trial_launches_before);
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

/// THE HOLD: wait, with every reader live, for the main thread's capture — or
/// for one of the things that end the attempt first. Each slice is a bounded
/// `recv_timeout` on the transfer channel (so the capture is picked up the
/// instant it is sent, which is inside the freeze) followed by the cheap
/// checks: a typed stand-down, the cancel poke, the dialer hanging up or
/// exiting, LaunchServices contradicting the dialer's identity, and the hold
/// bound.
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
            && !corroboration.poll()
        {
            return HoldOutcome::StandDown {
                outcome: crate::UpdateHandoffOutcome::Rejected,
                detail: "LaunchServices names a pid other than the dialer as the successor it \
                         launched"
                    .to_string(),
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
            // A HOLD THAT TIMED OUT IS TIMING, as the held lane's own hold
            // deadline already says (`HoldOutcome::StandDown { TimedOut }`):
            // it was `PreparationFailed` here, a verdict about bytes the fork
            // had not even started (the 2026-09-22/23 audit, plan P1-2).
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

/// The launched lane, in the late-park order (2026-09-19): bind, launch, accept
/// and HOLD the dial with every reader live; ask the main thread to park; then
/// write the artifacts under the pre-minted nonce, grant on the held stream and
/// hand over to the same decision tail the fork lane uses.
///
/// ORDER IS THE DESIGN. The listener is bound BEFORE the launch, because the
/// successor has to be told a name that already exists; the launch environment
/// names the manifest and layout by PATH before either file exists, because the
/// nonce is minted first; the dial is awaited under the dial deadline with the
/// terminal still echoing, because the successor's dial comes AFTER its whole
/// staged-swap boot apply (`ditto`/`hdiutil`/`codesign` plus a re-exec and the
/// count-only boot check) and that interval — ~1.4 s on this machine — used to
/// be the bulk of the freeze; and the park happens at the dial (or at the next
/// quiet moment after it), so what stays under the park is the capture, the
/// write, one `sendmsg`, and the successor's GUI boot to its first present.
///
/// EVERY FAILURE BEFORE THE TRANSFER KILLS NOTHING BY ROLLBACK AND STRANDS
/// NOTHING BY KILL: no descriptor of ours has left the process, so the
/// successor cannot be a reader; the rendezvous closes (EOF), the successor
/// exits by its own rule, and if it does not it is killed and PROVEN gone
/// before the attempt retires ([`retire_ungranted_successor`]).
#[cfg(target_os = "macos")]
fn run_prelaunched_handoff(
    job: &mut HandoffWorkerJob,
    proxy: &winit::event_loop::EventLoopProxy<Wake>,
    lane: HandoffPrelaunchLane,
) -> Prelaunched {
    use std::os::fd::AsFd as _;

    let Some(bundle) = job.bundle.clone() else {
        // Not a bundle: nothing has moved, so fork rather than skip the update.
        // The fork lane needs no bundle, and a machine that cannot be launched
        // through LaunchServices should still get its update.
        return fork_after_park(
            job,
            proxy,
            lane,
            "this process does not run from a .app bundle",
        );
    };
    let rendezvous = match crate::handoff_rendezvous::Rendezvous::bind(&lane.nonce) {
        Ok(rendezvous) => rendezvous,
        Err(error) => {
            // A stale node, an unwritable support dir, a path that would not fit
            // sun_path: none of it has moved a descriptor, and the fork lane needs
            // no socket at all.
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
        drop(rendezvous);
        return fork_after_park(
            job,
            proxy,
            lane,
            "the launch environment needs a removal a merged launch cannot express",
        );
    };
    // THE ARTIFACTS ARE NAMED BEFORE THEY EXIST: the writer derives the same
    // names from the same nonce after the park (`prepare_outgoing_artifacts`
    // asserts the identity), and the successor reads them only after the grant.
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
    environment.push(("ATERM_SEAMLESS_LAYOUT".into(), layout_path.into_os_string()));
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
    // THE CHUNKED GRANT IS OFFERED ONLY TO A CANDIDATE THAT DECLARED IT (its
    // verified `Info.plist`): such a successor claims `ATRZ2C`, and only then
    // may the grant span several messages. Any other successor — every build
    // before the chunked grant — never sees the variable, claims `ATRZ1C`, and
    // is granted exactly as before.
    if job.successor_grant_chunks {
        environment.push((
            crate::handoff_rendezvous::ENV_GRANT_CAPS.into(),
            crate::handoff_rendezvous::GRANT_CAPS_CHUNKS.into(),
        ));
    }
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
    // SNAPSHOT THE TRIAL COUNTER IMMEDIATELY BEFORE THE LAUNCH, so a third party's
    // launch counted during the (slow) pre-verification above cannot be
    // attributed to this candidate.
    job.trial_launches_before = aterm_update::trial_launch_count(job.target_build);
    let dial_deadline = handoff_ready_deadline();
    let launch_at = std::time::Instant::now();
    // THE LAUNCH IS ISSUED, NOT AWAITED (2026-09-14). The successor dials right
    // after its boot apply — before any NSApplication exists — and LaunchServices
    // answers only once the app has checked in, so the dial is structurally
    // ordered BEFORE the answer. The dial is the liveness signal and the claim
    // secret is the lock; the launch answer is corroboration, read AFTER the dial.
    //
    // A REFUSAL is still a refusal, not a slow answer: nothing was launched, so
    // no successor can dial and the fork lane is untouched and still able to
    // carry this attempt. The rendezvous is dropped on the way out, which closes
    // the listener and unlinks the node.
    let in_flight = match crate::app_launch_successor::begin_launch(
        &bundle,
        &arguments,
        &environment,
        // NEVER activated at launch (2026-09-19): the successor has no window
        // yet and the outgoing one is still the glass; the parent activates
        // it at Commit, when an aterm window had focus at the park
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
         awaiting its rendezvous dial"
    );
    // No expected pid at the gate: the claim secret is what admits a dialer, and
    // the kernel-attested peer pid is the identity everything below rests on.
    // The LaunchServices pid, when it lands, corroborates it after the dial.
    let launched: Option<i32> = None;
    let peer =
        match rendezvous.accept_claim(launched, dial_deadline, &|| job.cancel.try_recv().is_ok()) {
            Ok(peer) => peer,
            Err(crate::handoff_rendezvous::RendezvousError::Cancelled) => {
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
            Err(error) => {
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
                        format!("the launched successor never claimed the handoff: {error}"),
                    ),
                );
                return Prelaunched::Finished;
            }
        };
    // THE DIAL, with the terminal still echoing. Everything before it — the
    // launch prologue, the bundle swap, the second execve, the boot check — used
    // to sit inside the freeze; now it is the interval this line reports, and the
    // park has not happened yet.
    let dialled_at = std::time::Instant::now();
    aterm_log::info!(
        "update apply: successor dialled — launch->dial {} ms with every reader live; holding \
         its claim until the outgoing process parks",
        dialled_at.saturating_duration_since(launch_at).as_millis(),
    );
    // THE IDENTITY AND THE WITNESS, TAKEN FIRST — before any arm that can end
    // the attempt (2026-09-19, review round 1). A dialer that has been accepted
    // is a live process this attempt is responsible for, and EVERY exit from
    // here owes it the same proof of death that a granted one owes: the two arms
    // below used to publish a completion and return, which cleared the attempt
    // record and let the automatic lane launch a second successor beside a first
    // that was still running. The pid is the KERNEL's attestation for a process
    // we did not fork, plus the kernel's birth stamp for it — together they
    // survive pid reuse, which a bare pid from a completion wire does not — and
    // `EVFILT_PROC` can only be attached to a process that still exists, which
    // is now, while the accept has just proven it alive.
    let candidate = HandoffCandidate::of_attested_peer(pid_for_completion(peer.pid()));
    // Registered at the dial, before the grant hands it a single descriptor, and
    // held to this function's end: the successor publishes the carried ids before
    // the Commit, and until then THIS process answers to them (`identity_claim`,
    // "The other order"). Every return below comes after its proof of death.
    let _transfer = crate::identity_claim::register_handoff_candidate(candidate.pid);
    let exit_watch = CandidateExitWatch::watch(candidate.pid);
    if exit_watch.is_none() {
        aterm_log::warn!(
            "update apply: could not watch launched candidate {} for exit; a death on this \
             lane will be reported as Unobserved",
            candidate.pid
        );
    }
    let dialer_pid = peer.pid();
    let held = HeldSuccessor {
        rendezvous,
        peer,
        candidate,
        exit_watch,
        dialled_at,
    };
    // Corroboration, not the lock: if LaunchServices has answered by now, the pid
    // it names must be the dialer's. A mismatch is a stranger that knew the
    // secret — refuse this peer. No answer yet is what the ordering predicts
    // (the dial is structurally before the answer), so NOTHING WAITS HERE: the
    // answer is polled beside the hold and the proof wait instead
    // (`LaunchCorroboration`); a late mismatch still refuses before Commit.
    let mut corroborated = false;
    match in_flight.wait(std::time::Duration::ZERO) {
        Ok(successor) if successor.pid() == dialer_pid => {
            corroborated = true;
            aterm_log::info!(
                "update apply: LaunchServices corroborates the dialer as pid {}",
                successor.pid()
            );
        }
        // A STRANGER THAT KNEW THE SECRET. Refuse it — and prove THE DIALER
        // gone, because the dialer is the process this attempt accepted and is
        // responsible for; the pid LaunchServices names goes in the sentence,
        // not in the completion, so nothing downstream acts on a process this
        // parent never touched.
        Ok(successor) => {
            let detail = format!(
                "the dialer (pid {dialer_pid}) is not the successor LaunchServices launched \
                 (pid {}); refusing the handoff",
                successor.pid()
            );
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
        Err(crate::app_launch_successor::LaunchError::Timeout(_)) => aterm_log::info!(
            "update apply: LaunchServices has not answered yet; the dial is the liveness \
             signal — the answer is read beside the hold and the proof wait"
        ),
        // A late REFUSAL after a dial: the peer is real (it dialled with the
        // secret); LaunchServices' answer is about a job that evidently ran.
        // Log it, keep going — the kernel-attested pid is the identity.
        Err(error) => {
            corroborated = true;
            aterm_log::warn!(
                "update apply: LaunchServices answered the launch with an error after the \
                 successor had already dialled ({error}); proceeding on the attested dialer"
            );
        }
    }
    // A DIALER THAT ALREADY HUNG UP holds nothing — no descriptor has left — but
    // POLLHUP is not a death: it says the SOCKET closed, and the successor's own
    // error return closes it and then runs its exit path. Prove it gone like
    // every other stood-down dialer, which for one that really did exit is a
    // single `kill(pid, 0)`.
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
    let mut corroboration = (!corroborated).then_some(LaunchCorroboration {
        in_flight: &in_flight,
        dialer_pid,
        answered: false,
    });
    // THE CUE. The main thread parks when its gate admits — at once for an
    // explicit apply, at the next quiet moment for the automatic lane — and
    // sends the capture down the transfer channel.
    if proxy
        .send_event(Wake::UpdateHandoffAwaitingPark {
            attempt_id: job.attempt_id,
            dialer_pid: Some(candidate.pid),
            // What the dialer's CLAIM lets the grant carry — the one number
            // `transfer` will hold the pool to — so the park refuses a desk
            // that does not fit before it freezes anything.
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
    // THE PARK LANDED. From here the readers are parked and NOTHING has been
    // granted: every failure until the `sendmsg` releases the readers first and
    // then stands the successor down.
    let HandoffTransferJob {
        capture,
        proof_deadline,
    } = *transfer;
    let park_at = capture.park_at;
    // THE PARK->TRANSFER SPLIT (warm successor P0): each boundary the worker
    // crosses on the way to the grant, logged beside the total. Observation
    // only — nothing below reads these back.
    let captured_at = capture.captured_at;
    let picked_up_at = std::time::Instant::now();
    job.capture = Some(capture);
    aterm_log::info!(
        "update apply: the outgoing process parked {} ms after the dial; writing the \
         artifacts and granting on the held claim",
        park_at.saturating_duration_since(dialled_at).as_millis()
    );
    let prepared = match prepare_outgoing_artifacts(job, &lane.nonce) {
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
    // `capture.live` is `(local_id, child-owned master duplicate, shell pid)`;
    // the transfer wants `(local_id, shell pid, master)` in the order it will
    // send them, and that order is what addresses the descriptors on arrival.
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
    // THE PTY KEEPER'S PENDING (P3, opt-in; design §5.3 step 8), BEFORE the
    // grant: the keeper learns the successor's kernel pid before the successor
    // can hold a single master, so a death of this window while it holds them
    // is judged a handoff, never a crash with its masters on offer. Bounded;
    // the holder scan is the keeper's independent gate if it was not written.
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
        // WHICH SIDE OF THE `sendmsg` (2026-09-14, audit AH-6). Only a failure
        // BEFORE the descriptors left is an ungranted stand-down; after it the
        // successor holds duplicates of every master, and the rollback owes the
        // same candidate proof the fork lane owes from `execve` on — kill, reap
        // or witness, then resume the readers. In practice the partial shape is
        // EPIPE from a successor that already closed its socket (and, in code,
        // dropped what it received), so the proof is quick; it is still a proof.
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
        // MORE SESSIONS THAN THIS CLAIM'S GRANT CARRIES is this process's own
        // limit, not a verdict on the candidate (a successor that claimed the
        // one-message grant, with a pool that grew past it): refused before a
        // descriptor left, filed `ProducerFailed` and retried, as the park's own
        // session check files it.
        // THE PROOF DEADLINE RUNNING OUT BEFORE THE SEND is this process's own
        // lateness after a dial (round six of the update audit, item 48): the
        // artifacts took longer than the budget. Filed `TimedOut`, as the same
        // deadline expiring after the send is, never `Rejected` with words
        // blaming a successor that did dial.
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
    // From here the successor holds copies of everything, so this is the first
    // instant at which a rollback owes a candidate proof — and from here the
    // decision is identical to the fork lane's.
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
        // `ChildDied` IS the candidate ending before its proof and nothing more
        // (proof EOF, or its own termination seen while a stray copy of the write
        // end held EOF back): a successor that REFUSES this
        // handoff, one that CRASHED, and one the machine STARVED all close the
        // readiness channel identically. For five field failures the verdict did
        // not have to tell them apart, because the refusing successor said nothing
        // at all; it says so now (`seamless::take_target_identity`, and the
        // degraded-authority exit in `main_entry`), and the reject path below adds
        // what the PARENT saw (`handoff_child_death`). Point the next reader at
        // both rather than leaving `ChildDied` looking like an accusation against
        // the bytes. Log-only: the completion detail stays short because it is a
        // status-bar row's detail.
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
    // The proof checked out, so the successor has consumed everything this
    // attempt published. A successor built before the control carry never
    // reads the `.ctl` sidecars, and nothing else would ever remove the turn
    // text and scrolled-off rows in them: unlink them now, Commit or not.
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
        // Session death is the desk changing, typed as the park types it
        // (`handoff_rejection_activity_shaped`, round six, finding 37).
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
        // A SUCCESSOR THAT PROVED AND THEN ENDED must not be committed to: Commit
        // would `_exit` this process on a write only a stray copy of the Commit
        // pipe's read end can still accept, and nobody would own the terminals.
        // This NARROWS that window rather than closing it — the main thread can
        // win the arbiter between two probes, and a launched candidate launchd
        // has not reaped yet still answers "not gone" — and the atomic Commit
        // write's EPIPE stays the backstop behind it. Asked AFTER the reject channel, and filed `Rejected`, never `ChildDied`:
        // from here on the candidate exits by its own rule the moment THIS process
        // closes the Commit pipe on a rejection, so a death seen now may be one we
        // caused — evidence about the moment, not about the new bytes.
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

/// Acquire a parked session's engine within the freeze budget instead of failing
/// on the first contention (2026-09-14). After `park_all_readers` every reader is
/// joined, but the scrollback-compression worker (and the render/status paths)
/// can take the mutex the instant a reader releases it, and under a flood they
/// did so on every attempt: an explicit apply while any tab streamed output
/// failed deterministically in ~4 ms with "a terminal engine was busy", and the
/// automatic lane burned an attempt per cycle on it (QA runs 1–3 on this
/// machine; runs 4–6, idle or with the flood finished, committed). The budget
/// that already bounds the capture — 250 ms explicit, 20 ms first automatic —
/// is what the wait spends; a poisoned lock is taken as before.
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

/// The wall-clock window the park + capture of a seamless handoff must fit in, measured
/// from the instant the readers park — the freeze the user actually feels.
///
/// 20 ms is the imperceptible default and stays the FIRST automatic attempt's budget.
/// It is not a correctness bound, and treating it as one made an update refuse forever
/// on a machine that was merely busy: measured 2026-09-10 on m22 with two `trustc`
/// processes pinning the cores (load average 12.8), the automatic attempt missed the
/// park deadline and the owner's manual attempt three minutes later missed the capture
/// deadline — same 20 ms, same five sessions, and nothing about either would ever
/// change while the compiler ran. The module's own rule for the scrollback carry is
/// "the failure mode is less scrollback, never the update did not apply"; this is that
/// rule applied to time. A miss widens the window (80 ms, then a quarter second),
/// and the widening happens INSIDE one attempt — a park that missed re-parks on the
/// next rung 500 ms later with the successor still holding
/// (`PRELAUNCH_MAX_PARK_MISSES`) — as well as across attempts after a physical
/// failure. An EXPLICIT apply gets the widest window at once: the user asked for
/// the update, not for a sub-frame swap.
///
/// RUNG 0 IS SEEDED, NOT CONSTANT (gap #25, 2026-09-26): `seed` is the attempt's
/// [`FreezeSeed`] — the larger of a dry run of this very capture, timed while the
/// successor booted, and what the last park that landed really cost; else what
/// the ledger holds; else the 20 ms default — and the first automatic rung is
/// `clamp(1.5 × that, 20 ms, 80 ms)`. The 20 ms rung missed on both updates
/// before this (0.91 and 0.92 landed on the 80 ms rung in 22 and 20 ms), each
/// paying a stall, a 500 ms re-park and a second park. The next rung is twice
/// the seed and never under 80 ms, so the ladder stays strictly wider rung over
/// rung whatever the seed, and the last is a quarter second as before.
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

/// The first automatic rung's FLOOR: the imperceptible default it always was,
/// and what a desk nobody has measured still gets.
#[cfg(any(unix, test))]
pub(crate) const FREEZE_RUNG0_FLOOR: std::time::Duration = std::time::Duration::from_millis(20);

/// The first automatic rung's CEILING: the 80 ms the second rung used to be. A
/// capture that needs more than this on a first attempt is a busy moment, not a
/// desk to budget for — the ladder's wider rungs answer it, as before.
#[cfg(any(unix, test))]
pub(crate) const FREEZE_RUNG0_CEILING: std::time::Duration = std::time::Duration::from_millis(80);

/// The widest rung: an explicit apply's window, and the automatic ladder's top.
#[cfg(any(unix, test))]
const FREEZE_LAST_RUNG: std::time::Duration = std::time::Duration::from_millis(250);

/// Rung 0 from a measured freeze: `clamp(1.5 × measured, 20 ms, 80 ms)`. The
/// half again is the margin for a freeze that varies from one park to the next
/// (the machine's load, a reader's slice ending on the park's own schedule),
/// and the clamp keeps a desk nobody could time, or one that timed absurdly,
/// inside the ladder the rung has always been part of.
#[cfg(any(unix, test))]
#[must_use]
pub(crate) fn freeze_rung0_from_capture(capture: std::time::Duration) -> std::time::Duration {
    (capture.saturating_mul(3) / 2).clamp(FREEZE_RUNG0_FLOOR, FREEZE_RUNG0_CEILING)
}

/// Where an attempt's first automatic freeze rung came from (gap #25) — said in
/// the budget's log line and persisted in the health ledger, so a rung that
/// still misses can be read against the number it was seeded from.
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
    /// two-session desk took 22 and 20 ms.
    DryRun {
        capture: std::time::Duration,
        sessions: usize,
        last_park: Option<std::time::Duration>,
    },
    /// What the health ledger holds — the larger of its last dry run and its
    /// last landed park, as recorded by `build` — which a lane with no boot to
    /// time a dry run in (the fork lane), or an attempt whose dry run measured
    /// nothing, seeds from.
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
            Self::DryRun {
                capture, last_park, ..
            } => freeze_rung0_from_capture(capture.max(last_park.unwrap_or_default())),
            Self::Ledger { capture, .. } => freeze_rung0_from_capture(capture),
        }
    }

    /// The ledger's prior — the larger of its dry run and its landed park,
    /// named by the build that recorded that one — or the default when neither
    /// was ever measured.
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

    /// The last landed park the ledger holds, if any.
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

/// Keep what a LANDED park cost — `parked_in`, from the readers' stop to a
/// capture ready to hand over, the whole of what the freeze budget is spent on —
/// for the next update's first rung (gap #25), on a thread of its own: the
/// ledger lock is a file lock another process may hold, and this runs inside
/// the freeze. Returns the writer (a test joins it; the park lets it run).
///
/// The write is a temp-and-rename taking about a millisecond, and the outgoing
/// process exits at Commit, hundreds of milliseconds after the park (354 and
/// 403 ms park->proof on the owner's last two landed updates) — so it lands;
/// one that did not would cost the next update this one measurement, nothing
/// more.
#[cfg(unix)]
pub(crate) fn keep_landed_park(
    parked_in: std::time::Duration,
) -> Option<std::thread::JoinHandle<()>> {
    let build = crate::running_build_number();
    std::thread::Builder::new()
        .name("aterm-park-cost".to_string())
        .spawn(move || {
            // A small ledger write nobody waits on — but it must land before
            // Commit, so never the starvable Housekeeping class.
            crate::qos::set_self(crate::qos::Role::Background);
            aterm_update::record_handoff_park(build, parked_in);
        })
        .ok()
}

/// The dry run's whole window: two thirds of [`FREEZE_RUNG0_CEILING`], because
/// 1.5 × anything longer is the ceiling already. It bounds what the main thread
/// spends timing a capture with every reader live.
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

/// Why the park's capture (`App::capture_parked_screens`) could not produce a
/// committed screen set — TYPED, because the capture is now total over screen
/// content (the 2026-09-22/23 update audit, plan P0-1c) and what is left falls
/// into kinds that deserve different answers: the first three are timing or
/// this process's own memory, and retrying later is right; `Refused` is a
/// deterministic fact that retrying the same desk cannot change, and filing it
/// as "the machine is busy" is what looped v0.91 forever. Plan P0-3 gives it
/// its own lane: [`classify_capture_failure`] is the one place a failure is
/// sorted into a miss or a refusal, and [`std::fmt::Display`] keeps the
/// sentences the ledger and the log have always carried.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CaptureFailure {
    /// The capture outran the freeze budget.
    Deadline { freeze_ms: u128 },
    /// A session's engine stayed locked for the whole capture window.
    EngineBusy,
    /// This process could not reserve the capture's bounded storage.
    Storage,
    /// A session's parser was parked in the middle of an escape sequence
    /// whose REST is still queued on its PTY — the reader stopped at a read
    /// boundary inside a flood (a coloured build log, an inline redrawing UI,
    /// a kitty-graphics or sixel transfer).
    ///
    /// TIMING, never a refusal, and never carried (the 2026-09-24 review of
    /// the 2026-09-22/23 update audit fixes). The carry would abandon the
    /// partial sequence as CAN does, and the successor's fresh Ground parser
    /// would then read the queued tail as plain text: `6m` printed where a
    /// colour was meant, a split `ESC [ 2` + `K` printing `K` and skipping the
    /// erase, the rest of an image payload printed as screens of base64, a
    /// split mode set lost in silence — on a session carried at the Full rung
    /// with no repaint to cover it. The launched lane re-parks at the gate's
    /// next 20 ms re-run on the same rung ([`mid_sequence_miss_disposition`]),
    /// which almost always finds the reader on a sequence boundary; a stream
    /// that never offers one within that bound is a busy machine, which is
    /// what a miss says (the fork lane files it as one at once).
    /// A parser mid-sequence over a QUIET PTY (the stalled sequence the
    /// abandoning carry exists for) is still carried.
    ///
    /// Since the parser carry (the round-five plan's item 12) this names only
    /// a partial sequence NO CARRY HOLDS — a hooked DCS string
    /// (`Terminal::partial_sequence_uncarried`) — one the carry would only
    /// swallow (`Terminal::partial_sequence_swallowed`), and the engine state
    /// no checkpoint carries: an open OSC 8 link or a pending VT52 address
    /// (`Terminal::output_state_uncarried`). Every other partial sequence
    /// rides the checkpoint (`TerminalCheckpoint::parser`) and the successor
    /// continues it with the queued tail, so it is carried, not missed.
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

/// The park's capture: every session's committed screen, the control carries
/// that travel with them, the screen commitment `screen_digest` took over
/// exactly those screens (the capture's self-check computes it, so no caller
/// hashes the set twice under the freeze), and the sessions carried below the
/// exact rungs, which the successor must make redraw (`ScreenCarry::repaint`).
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
    /// Every handed session's history fence under the park
    /// (`handoff_history::capture_head`), what the worker joins its export to.
    history_heads: Vec<crate::handoff_history::HistoryHead>,
    /// Every held pane's visible screen the capture had the time and the
    /// budget for ([`App::capture_held_screens`]).
    held: Vec<crate::seamless::HeldScreen>,
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
/// Fixed control sockets support overlap too: the candidate's control worker
/// shares the authenticated reader gate and binds only after Commit and the
/// parent's exit. Before Commit the parent alone owns the socket and token.
///
/// There is NO deliberate opt-out any more: `$ATERM_NO_SEAMLESS_UPDATE`, which forced
/// every update onto the cold lane, is gone (2026-09-23 — the owner's R3 wants every
/// update seamless, and R2 "NOT ENV VARS those are for development"). Every reason
/// left is a fact about the launch or the process, never a preference.
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

    /// What is set — the clause the reader can act on, which is why every
    /// sentence built on it leads with it (the status bar truncates from the
    /// right when the window is narrow).
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

impl App {
    /// The ONE reading of whether the overlap handoff can run here — the first
    /// true reason in [`HandoffUnavailable::ALL`]'s order, or `None` when the
    /// seamless lane is available. Read by the apply gate
    /// (`start_unix_update_handoff`) and by the status bar's posture
    /// ([`App::apply_posture_for`]), and nowhere else, so the two cannot
    /// disagree. Plain reads only — environment, two fields and the published
    /// bound-socket witness; no filesystem probes or recursive memo: a
    /// memo whose initializer could call back into its owner parked the main
    /// thread forever on 2026-08-30.
    ///
    /// NOT folded into `arm_native_auto_apply`'s `enabled` on purpose: the cold
    /// lane is a real automatic path for a headless or opted-out process
    /// whose last terminal has closed (the classifier admits it at zero live
    /// PTYs, and the successor inherits `--headless` with the rest of argv), so
    /// a lane that never arms would never take it.
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
    /// (never a pointless re-exec). Rung 1b live wiring (ALWAYS ON — the
    /// `ATERM_NO_SEAMLESS_UPDATE` opt-out is gone, 2026-09-23): hands every live PTY master, its exact visible-screen
    /// checkpoint, and a `SessionHandoff` manifest to the new process so the running shell survives
    /// (the round-trip that makes that safe is
    /// proven — `SessionHandoff` + `handoff_roundtrip_model`; the single-use nonce by
    /// `seamless_nonce_model`). Scope: the live process, visible rows, terminal modes/cursors,
    /// and output queued after reader park survive, and so does the WHOLE off-screen
    /// scrollback (2026-09-26, `crate::handoff_history`): the screen checkpoint still
    /// carries at most `seamless::MAX_HANDOFF_HISTORY_LINES` (256) of it, fewer or none
    /// under the time ladder or the aggregate cell budget, and everything older rides a
    /// fenced sidecar exported with the readers live and imported by the successor after
    /// Commit. A history that moved under its export (a rewrap, a clear, a reset), that
    /// more output outran than the checkpoint carries, or whose sidecar fails its sha,
    /// crosses with the checkpoint's lines alone — and the lines left behind are COUNTED,
    /// per tab on `status` (`history_lost=`) and once on the band, never only in the log.
    /// The standing policy is unchanged: "the failure mode is less scrollback, never the
    /// update did not apply". (Until this date a tab on the default 100,000-line ring
    /// kept a screen and a half of its history, which this sentence said since
    /// 2026-08-19; it said "deliberately excluded" before that.) A cold relaunch is
    /// permitted only when no foreground terminal job would be destroyed. Every path that
    /// leaves this process alive returns an actionable failure so the updater reducer can
    /// re-arm the verified stage instead of remaining stuck in `Applying`.
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
        let build = crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0);
        // QA SEAM: `ATERM_DEBUG_SEAMLESS_REEXEC=1` re-execs the SAME binary (no staged
        // build, no bundle swap) but exercises the FULL seamless handoff + adopt path, so
        // the shell-survives-an-update contract is testable end-to-end without a release.
        // The seam is derived here from the environment alone: a caller that names it
        // without the variable armed gets no authority from the name.
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
        // Re-exec THIS binary; the swap to the staged build happens at the top of the
        // new `main`. `exec` never returns on success. (Rung 1b: serialize the session
        // + pass the PTY master fds here so the new process re-adopts them.)
        // Hand the OLD build number to the post-update process via `ATERM_UPDATED_FROM`
        // (inherited through `apply_staged_if_ready`'s own re-exec) so it records the
        // landing on startup. Read + cleared at startup so it
        // never leaks into the user's shell children (`JUST_UPDATED`).
        #[cfg(unix)]
        {
            // THE ROW IS RETIRED BY WHOEVER BROKE THE PROMISE. `start_unix_update_
            // handoff` puts the flow row's "installing…" on the update bar before
            // it builds the carry, and several of its later `Err`
            // exits (a missed 20 ms park deadline, masters closed under it, a
            // reservation failure) return WITHOUT producing a completion — so the
            // completion path cannot be the only place that clears it, or a refused
            // attempt leaves a row claiming an install that never started standing
            // to its `HANDOFF_STALE` cap. Binding the result at the ONE call site
            // covers every such exit and cannot rot as new ones are added.
            let started = self.start_unix_update_handoff(
                exe,
                build,
                safety_token,
                mode,
                apply_attempt,
                same_image,
            );
            if let Err(error) = &started {
                self.retire_update_installing();
                self.record_update_switch_stopped(error.stops_routinely(mode), &error.to_string());
            }
            return started;
        }
        // Windows has no exec(2); the analog is spawn-the-new-then-exit. Dead in
        // practice today (the updater is macOS-only, so `staged` is always None
        // above), but kept correct for when a Windows update lane exists.
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
                Some(control) => Some(control.try_begin_update_quiesce().map_err(|error| {
                    crate::UpdateHandoffStartError::failed(format!(
                        "resident operator could not quiesce for replacement: {error}"
                    ))
                })?),
                None => None,
            };
            self.shutdown_title_summaries();
            // The message log's last write from this process: the
            // `process::exit` after a successful spawn runs no destructor,
            // so the writer thread's queue would die with it (design §3.7).
            self.flush_messages_log();
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
        same_image: Option<SameImageHandoff>,
    ) -> Result<(), crate::UpdateHandoffStartError> {
        use std::os::unix::process::CommandExt as _;

        // Every session with a live process behind it; a pane kept open after
        // its command exited is not handed (`handoff_live_sessions`).
        let live: Vec<(u64, i32, i32)> = self.handoff_live_sessions();
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
        // the opt-out, `--control-sock`, `headless`, `proxy` — while the
        // status bar's posture folded only the first, so a `--control-sock` or
        // `--headless` process painted "applies in place within ~2 min" over an
        // apply this gate refused (2026-08-30). `seamless_handoff_unavailable`
        // is now the only place the remaining vetoes are read, and every reason is said in
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
        // NOTHING TO HAND OVER, SOMETHING TO LOSE. With every live session left
        // out (`handoff_live_sessions`) the classifier below would read an exact
        // zero-PTY desk and take the cold lane, whose `exec` closes every window —
        // including the exited panes the user kept open (`--hold`) to read. That
        // is not a desk with nothing on it, so it is a typed refusal instead:
        // recorded, retried on the block cooldown, and gone the moment a pane is
        // closed or a live tab opens (the 2026-09-24 review).
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
        if let crate::native_update_admission::AdmissionDecision::Apply(lane) = decision
            && let Some(refusal) = lane_carry_refusal(
                &safety_token,
                lane,
                build,
                apply_attempt
                    .as_ref()
                    .map_or(build, |attempt| attempt.target_build()),
            )
        {
            return Err(crate::UpdateHandoffStartError::refused(refusal));
        }
        match decision {
            crate::native_update_admission::AdmissionDecision::Block(reason) => {
                // A Block is a REFUSAL TO ATTEMPT, never a failed attempt — see
                // [`crate::UpdateHandoffStartError::Refused`]. Minting it as
                // `failed` counted every deterministic "the seamless lane is
                // unavailable here" as a hard apply failure (the field's
                // failing_applies=23) and erased the standing explanation.
                return Err(update_admission_refusal(reason, facts, handoff_unavailable));
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
                // Headless survives the re-exec with no help: `--headless` is a
                // launch flag, and the successor inherits our argv (minus the
                // leading pins), where it sits before any `-e` payload.
                let operator_quiesce = match self.operator_control.as_ref() {
                    Some(control) => Some(control.try_begin_update_quiesce().map_err(|error| {
                        crate::UpdateHandoffStartError::failed(format!(
                            "resident operator could not quiesce for replacement: {error}"
                        ))
                    })?),
                    None => None,
                };
                self.record_update_switch_started(build, same_image.is_some(), mode.is_automatic());
                self.shutdown_title_summaries();
                // The message log's last write from this process: `exec`
                // runs no destructor, so the writer thread's queue would die
                // with it (design §3.7). The writer stays open for the
                // failure arm below.
                self.flush_messages_log();
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
        // `PreparationFailed` completion (Structural — since the 2026-09-22/23
        // audit the ONLY preparation failure filed that way; this process's own
        // are `ProducerFailed`). The same-binary debug re-exec has no
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
        // ONLY THE AUTOMATIC MODES HONOUR A CACHED REFUSAL (2026-09-14). An
        // explicit apply is the person's own request, made — in the case this
        // was written for — right after they put the signed bundle back: a
        // refusal cached up to ten minutes earlier must not answer for the
        // bundle that is there now. The worker re-verifies for them; a cached
        // PASS is still honoured by every mode, since it only shrinks the park.
        if preverified == Some(false) && mode.is_automatic() {
            // The CAUSE the verifier gave, not a generic verdict: an installed
            // bundle that fails the signing policy (a local build swapped into
            // /Applications) is the owner's to fix, and only the message can
            // tell them so.
            let reason = apply_attempt
                .as_ref()
                .and_then(|attempt| self.cached_handoff_preverification_reason(attempt))
                .unwrap_or_else(|| "the staged update failed verification".to_string());
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
        // Parent master flags are an invariant, not mutable handoff state. Child
        // inheritance is installed later by `pre_exec` on child copies only.
        for (_, master, _) in &live {
            if aterm_pty::set_cloexec(*master, true).is_err() {
                return Err(crate::UpdateHandoffStartError::failed(
                    "could not enforce CLOEXEC on every parent PTY master",
                ));
            }
        }

        // APPLY BEGINS ON THE STATUS BAR (2026-09-07) — sited BEFORE the carry
        // below is built, so the row count and words the successor inherits are
        // the ones the user is looking at. An update row already up changes its
        // WORDS — "Updating to aterm vX · installing…" — a repaint, never a
        // re-grid. With no row up, an EXPLICIT
        // apply (the Version menu, Software Update, a clean quit) ADDS the row
        // here: its re-grid moves `ws.rows` before the carry reads it and before
        // `exact_layout` freezes what Commit compares, so the successor is sized
        // for the row it is told about. The AUTOMATIC lane never adds one: the
        // re-grid's SIGWINCH is PTY activity its own re-check (below) reads as a
        // revocation, so there the border SURGE — charging from this instant
        // until the successor takes over — is the whole explanation of the
        // frozen frame. Every refusal from here on puts the words (or the
        // absence of a row) back and ends the surge: the synchronous ones through
        // the caller's `retire_update_installing`, the asynchronous ones after
        // the park through `reduce_returned_handoff_completion`.
        self.begin_update_installing(
            build,
            matches!(mode, crate::native_updater_service::ApplyMode::Immediate),
        );
        // On record before the park: the park's budget has no room for a file write.
        self.record_update_switch_started(build, same_image.is_some(), mode.is_automatic());

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
        crate::control_socket_identity::bind_command(&mut command);
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
        // Decided on the LIVE snapshot (2026-09-19, the late park): the launched
        // lane duplicates its masters and computes its device-term proof
        // identities AT THE PARK, which is now seconds or minutes away, over
        // whatever the pool holds then; the fork lane duplicates them just below,
        // before it parks, exactly as before. The `fstat` probe here answers the
        // same for a live master as for its later duplicate (same device), so
        // the lane and the proof term cannot disagree.
        // THE CHUNKED-GRANT FACT, READ ONCE (round six of the update audit,
        // item 1): only a PASSED verification on record says the candidate
        // declares it, and with none yet (the worker verifies below) the
        // one-message limit stands. The lane is chosen on this value AND the
        // launched successor is offered the grant on it
        // (`HandoffWorkerJob::successor_grant_chunks`): a worker that skips its
        // own verification because a fresh pass is cached used to launch with
        // `false` after the lane admitted more than one message carries.
        #[cfg(target_os = "macos")]
        let lane_grant_chunks = self.park_policy(apply_attempt.as_ref()).grant_chunks;
        #[cfg(target_os = "macos")]
        let lane = {
            let facts = HandoffLaneFacts {
                bundled: bundle.is_some(),
                // A build for this platform has one compiled in. The fact is
                // still a field rather than an assumption so the refusal it
                // guards is reachable from a test.
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
                        // A master that will not answer `fstat` cannot be given a
                        // device term, and a proof missing one term is a proof over
                        // a different session set. Forking is exact here rather than
                        // degraded: the fd-number term needs nothing from the kernel.
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

        // THE LAUNCHED LANE LEAVES HERE WITH EVERY READER LIVE (2026-09-19, the
        // late park). Its worker launches the successor, holds its rendezvous
        // claim, and asks this thread to park only once the successor has
        // dialled; the park, the capture and the transfer follow in
        // `park_and_transfer_to_prelaunched_successor`. Everything below this
        // line is the fork lane, which still parks before it spawns.
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

        // RUNG 0'S SEED, READ HERE — before the activity re-check and the park,
        // so the one small ledger read this lane makes can never sit inside the
        // TOCTOU window below (gap #25). The fork lane has no successor boot to
        // time a dry run in, so it seeds from what the ledger holds: the larger
        // of the last dry run and the last park that landed.
        let freeze_seed = FreezeSeed::from_prior(aterm_update::handoff_capture_prior().as_ref());

        // Capture the session registry BEFORE parking, because this projection can
        // FAIL: a `WouldBlock` here must return with the terminal completely
        // untouched, which is only true while no reader has been stopped. It
        // performs no disk I/O.
        //
        // The restore manifest deliberately does NOT travel with it — see the
        // post-park capture below for why capturing it here revoked the handoff.
        //
        // The hold serials are read FIRST: a hold that moves between this read
        // and the manifest's own read of it refuses the Commit (a lossless
        // retry), and one that moves after it is not in the manifest and
        // refuses it too — no order of the two can let a halt the successor
        // never saw pass the Commit's comparison (`hold_serials`). And none
        // can land after that comparison either: the Commit reads the serials
        // under a fence that holds every later `hold` back until the attempt
        // has exited or stood down (`fence_hold_serials`).
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
        let window = self.handoff_window_carry(target_build);
        // The fork lane's proof term is the descriptor number: `execve` copies
        // the table verbatim, so the number itself is what both sides compute.
        let proof_identities = adoption.clone();

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
        // A HUNG-UP master is not output any more (plan P1-3), so the peek above
        // no longer catches it; it is caught here, still before any reader
        // stops, under a reason that says what it is — the post-park death check
        // below would refuse it anyway, after freezing the terminal to learn it.
        // Gated on the LANE, like that check, not on the activity epoch.
        if mode.is_automatic() && handoff_masters_closed(&live) {
            return Err(crate::UpdateHandoffStartError::activity(
                "a session's command has exited but its pane is still open",
            ));
        }
        // A LIVE `video` TAKE, before `Land` (round five, item 16): the same
        // typed wait the launched lane's gate applies, retried as activity. The
        // fork lane has no hold to bound it by, so the ladder does: it reaches
        // `Land` within `LANDS_WITHIN` of being armed; the park then proceeds
        // and Commit aborts the take with a reply.
        if let Some(reason) = recording_park_refusal(
            mode,
            self.automatic_phase_for(mode, std::time::Instant::now()),
            self.video_request_live(),
            std::time::Duration::ZERO,
        ) {
            return Err(crate::UpdateHandoffStartError::activity(reason));
        }
        // How much of the capture window must remain before a session is willing to
        // serialize scrollback as well as its visible screen. Half the freeze budget: the
        // visible screen is mandatory and cheap, history is optional and priced per
        // line, so once the window is half gone every remaining session drops to
        // visible-only rather than risking the deadline for a bonus. This is what
        // makes carrying history safe to enable by default — the failure mode is
        // "less scrollback", never "the update did not apply".
        // The budget this attempt gets: 20 ms on a first automatic attempt, wider after
        // a physical failure of these same bytes, widest for an explicit apply — see
        // `handoff_freeze_budget`. Everything below that used to spell "20 ms" now
        // spells the budget it actually had.
        // Keyed by the ATTEMPT'S TARGET (2026-09-14): every writer of the
        // physical-retry record stores the staged build it failed to reach,
        // while `build` here is the RUNNING one — so the filter never matched,
        // every automatic retry got the first attempt's 20 ms, and the wider
        // rungs (5f77ff084) were dead on the lane they were written for.
        // PLUS THIS LANE'S OWN PARK MISSES (plan P1-4): a fork-lane miss is no
        // longer charged as a physical failure, so the rung it used to buy is
        // counted here instead — see `App::fork_park_misses`.
        let fork_park_misses = self.fork_park_misses_of(target_build);
        // THE SUCCESSOR'S POLICY (plan P0-5), read before the park: a cache
        // lookup, nothing that belongs in the frozen window. The fork lane's
        // only knob is the carry ceiling — its entry gate has no `Land` wait
        // for `park_quiet_gate_at_land` to relax.
        let park_policy = self.park_policy(apply_attempt.as_ref());
        let ceiling = park_policy.carry_ceiling();
        if let Some(policy) = park_policy.policy {
            aterm_log::info!(
                "update apply: parking under build {target_build}'s handoff policy ({policy})"
            );
        }
        // UNKNOWN, NOT ABSENT: with no pass of this artifact on record, its policy
        // is read by this lane's worker only after the park. Said here; the worker
        // refuses a capture the policy turns out to forbid
        // (`capture_exceeds_policy`), and a failed capture below starts the
        // verification now, so the next park knows it either way.
        let policy_unread = apply_attempt.is_some() && same_image.is_none() && !park_policy.known;
        // …BUT AN AUTOMATIC ATTEMPT FIRST WAITS FOR IT WHILE IT IS READ (round
        // seven, item 36), bounded by the read's ceiling: the capture below
        // runs on this thread, and one the policy exists to route around (a
        // projection that panics or hangs) could otherwise never be routed
        // around here.
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
        // THE INSTANT THE TERMINAL STOPS ECHOING — the start of the freeze the
        // user experiences, and the zero point of the two numbers reported at
        // Commit. The capture deadline hangs off the same stamp.
        let park_at = std::time::Instant::now();
        let deadline = park_at + freeze_budget;
        if !self.park_all_readers(deadline) {
            self.rollback_overlap(None, &live);
            return Err(self.fork_park_missed(
                target_build,
                format!("a PTY reader missed the {freeze_ms} ms handoff park deadline"),
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
        let ParkedScreens {
            screens,
            carries,
            screen_digest,
            repaint,
            fg_holders,
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
                // The worker that would have read the candidate's policy never
                // runs for a capture that failed; read it now, off this thread, so
                // the next park follows it — a capture that fails at Full may be
                // exactly what the policy was sealed to route around.
                if policy_unread {
                    self.spawn_staged_handoff_preverification(target_build);
                }
                // ONE FACT, ONE VERDICT, WHICHEVER LANE (plan P0-3(d), P1-4):
                // the same classifier the launched lane's park reads. A miss
                // rides the activity spacing; a refusal the refusal lane. Both
                // used to be `failed` here — PHYSICAL, latching after nine.
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
        let layout = self.capture_handoff_layout();

        let Some(layout_digest) = crate::seamless::layout_digest(&layout) else {
            self.rollback_overlap(None, &live);
            return Err(crate::UpdateHandoffStartError::capture_refused(
                CaptureRefusal {
                    local_id: None,
                    cause: "handoff layout could not be committed canonically".to_string(),
                }
                .to_string(),
            ));
        };
        // The screen commitment was taken by the capture's own self-check
        // (`seamless::settle_wire_carries`), over exactly `screens`; a refusal
        // it could not lower is in the capture's `Err` above, named.
        if std::time::Instant::now() >= deadline {
            self.rollback_overlap(None, &live);
            // It said "20 ms" whatever the rung was.
            return Err(self.fork_park_missed(
                target_build,
                format!("handoff proof capture exceeded the {freeze_ms} ms deadline"),
            ));
        }
        // The park landed inside its budget: keep what it cost for the next
        // update's first rung (gap #25), as the launched lane does.
        let _ledger_write = keep_landed_park(park_at.elapsed());
        let preverified =
            PreverifyPublisher::for_attempt(&self.handoff_preverified, apply_attempt.as_ref());
        let activity_epoch = self.update_handoff_activity_epoch;
        let arbiter = crate::HandoffAttemptArbiter::new();
        self.pending_update_handoff = Some(crate::PendingUpdateHandoff {
            attempt_id,
            park_at,
            proof_ready_at: None,
            // Sampled AT THE PARK: whether the user was looking at aterm when the
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
            cancel: cancel.clone(),
            arbiter: arbiter.clone(),
            teardown: crate::DeferredHandoffTeardown::None,
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
                // Exported by the worker once this capture is done
                // (`HistoryPlan::Deferred`): never beside the park. Withheld
                // under a ceiling that carries no scrollback — the one this
                // capture was taken under, which its worker holds the policy
                // it reads to (`capture_exceeds_policy`).
                history: crate::handoff_history::HistoryPlan::deferred_under(ceiling, || {
                    self.history_sessions(target_build)
                }),
                held,
            }),
            #[cfg(target_os = "macos")]
            prelaunch: None,
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
        // A live `video` take is NOT answered here: the worker has still to
        // verify and launch the candidate and wait for its proof, and any of
        // those failing keeps this process running. Commit answers it
        // (`App::video_answer_before_commit`; round six of the update audit,
        // finding 22).
        Ok(())
    }

    /// THE SESSIONS AN OVERLAP HANDOFF HANDS OVER, as `(local_id, master, pid)`:
    /// every pooled session except a pane kept open after its command EXITED
    /// (`aterm --hold`, or any pane whose close did not follow its exit), which
    /// the session registry marks `Exited` at the reader's EOF.
    ///
    /// Why they are left out (the 2026-09-24 review of the 2026-09-22/23 update
    /// audit fixes, plan P1-3). Such a pane's master has hung up and stays hung
    /// up until a person closes the pane, and both lanes treated a hung-up
    /// master as a session that died under the attempt: the launched lane's
    /// park gate waited on it with no bound, held every automatic successor
    /// its full 120 s and stood it down as `ActivityRevoked` ("the terminal
    /// never offered a moment to pause in"), every fifteen minutes, forever;
    /// the fork lane deferred on it every 500 ms, recording nothing. The update
    /// could not land while one exited pane was open. There is no live process
    /// behind that pane to keep, so nothing is handed for it, and every live
    /// session goes across exactly. The layout still names the pane (the
    /// capture stamps every terminal view), and the successor keeps its place
    /// as a placeholder saying nothing was running there
    /// (`RestoreManifest::placed_for_handed`) — it used to drop the WHOLE
    /// layout for that one extra id, and the unsaved Settings drafts that ride
    /// only in it (round three of the 2026-09 update robustness work).
    ///
    /// Read ONCE per decision and passed down, so the gate, the capture, the
    /// descriptors and the manifest all describe the same set; the Commit
    /// check compares against it with [`Self::handoff_exited_session_ids`].
    #[cfg(unix)]
    pub(crate) fn handoff_live_sessions(&self) -> Vec<(u64, i32, i32)> {
        let exited = self.handoff_exited_session_ids();
        self.pool
            .iter()
            .filter(|session| !exited.contains(&session.id))
            .map(|session| (session.id, session.master, session.pid))
            .collect()
    }

    /// The window facts the successor sizes its first window from, as both
    /// lanes hand them over at the park: the front window's grid, its frame
    /// origin, and the message band's committed rows with the messages they
    /// show — so the successor reserves the same rows before it sizes its
    /// window and paints carried content in them until Commit; the lines the
    /// freeze kept from the log ride along (design §3.8) — and when this
    /// process finished downloading the build being installed, so the
    /// successor can record how long the update took.
    ///
    /// One projection for both lanes, and for the whole-App capture property
    /// (`seamless::app_capture_tests`, plan P1-6), which asserts the successor
    /// reads back exactly this after the message band took or gave back a row
    /// between the program's last frame and the park. The two lanes used to
    /// spell it inline, twice, identically.
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
            }
        })
    }

    /// THE LIVE FONT ZOOM, as the window carry writes it (the round-four plan,
    /// item 15): `(px, reset px)` in thousandths of a physical px while a
    /// person has the font zoomed — pinned (`font_px_explicit`, which every
    /// zoom sets) AND away from the size Cmd-0 resets to — else `(None, None)`.
    ///
    /// Only a zoom is written. An unzoomed window's size is exactly what the
    /// successor derives from the same config and display (a flag or config
    /// size, or the Retina auto-scale), so writing it would change the wire
    /// for nothing; and a pinned size that equals its reset size is not a
    /// zoom. The pair is written whole or not at all, because the successor
    /// needs both: the zoom to draw at, and the reset its Cmd-0 goes back to.
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

    /// Start the launched lane's HISTORY CARRY export (`crate::handoff_history`)
    /// for every handed session, with every reader live, beside the launch,
    /// FOLLOWING each history until the park stops it —
    /// `None`, and nothing exported, for [`Self::history_sessions`]'s reasons
    /// or when the worker cannot start (said in the log; the sessions then
    /// carry the screen carry's bounded history, and the join counts what that
    /// leaves behind). The fork lane exports on its worker instead
    /// (`HistoryPlan::Deferred`).
    ///
    /// `None` too, and nothing written to disk, when `ceiling` — the carry
    /// ceiling of the successor's handoff policy as far as it is known at the
    /// launch — carries no scrollback. A policy not yet read then reads
    /// `Full`, so the export starts; the park, which reads the policy after
    /// the worker has published it, stops it then
    /// ([`Self::withhold_history_export`]).
    #[cfg(any(target_os = "macos", all(test, unix)))]
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

    /// STOP THE LAUNCHED LANE'S HISTORY EXPORT under a successor's policy
    /// whose `ceiling` carries no scrollback: take it off the prelaunched
    /// attempt, give its worker [`crate::handoff_history::HALT_WAIT`] to stop
    /// (it takes no terminal lock after), and drop it, which removes every
    /// sidecar it wrote. Nothing when no export is running. The park then
    /// hands the worker [`crate::handoff_history::HistoryPlan::Withheld`].
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

    /// Every handed session's engine, for the history carry's export — `None`,
    /// and nothing exported, when the successor is OLDER than this build (a
    /// rollback has no importer) or there is no private control dir to write
    /// into (each said in the log; the sessions then carry the screen carry's
    /// bounded history, and the join counts what that leaves behind).
    #[cfg(unix)]
    fn history_sessions(
        &self,
        target_build: u64,
    ) -> Option<
        Vec<(
            u64,
            std::sync::Arc<std::sync::Mutex<aterm_core::terminal::Terminal>>,
        )>,
    > {
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
        let live = self.handoff_live_sessions();
        Some(
            self.pool
                .iter()
                .filter(|session| live.iter().any(|(id, _, _)| *id == session.id))
                .map(|session| (session.id, std::sync::Arc::clone(&session.term)))
                .collect(),
        )
    }

    /// THE SUCCESSOR'S HALF of the history carry, at Commit: take every
    /// adopted session's carry (`Session::handoff_history`) and import it on a
    /// worker — NEVER here: an import is O(the imported history), and this is
    /// the event loop. The worker counts what failed onto each session's
    /// `history_lost` itself (so a wake that never arrives cannot lose the
    /// count) and reports back as `Wake::HandoffHistorySettled`, which says it
    /// on the band. A worker that cannot start costs every carry its lines,
    /// counted and said here.
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

    /// Say what the history carry's imports left behind, once they settle:
    /// when this update left ANY line of any tab's history behind (the
    /// outgoing side's fallbacks, what a successor's handoff policy withheld,
    /// and the sidecars that failed here), one row on the band says how many,
    /// in how many tabs — and whether it was the policy's choice or a
    /// failure, because the words true of one are false of the other. The
    /// per-tab counts are already on the registry
    /// (`handoff_history::record_reports`).
    #[cfg(unix)]
    pub(crate) fn settle_handoff_history(
        &mut self,
        reports: &[crate::handoff_history::ImportReport],
    ) {
        let loss = crate::handoff_history::loss_summary(reports);
        if loss.lines > 0 {
            self.post_message(crate::update_words::scrollback_left_behind(
                loss.lines,
                loss.tabs,
                loss.withheld,
            ));
            self.sync_messages();
        }
    }

    /// THE CAPTURE LADDER, under the park: every session's screen (and as much
    /// scrollback as the remaining budget allows) plus the control carry's head,
    /// priced against the freeze budget and the aggregate cell and
    /// decode-authority ceilings, then committed by the build's own full
    /// predicate. Shared by both lanes (2026-09-19): the fork lane runs it
    /// inline before its spawn, the launched lane at the park it takes once the
    /// successor has dialled. The caller rolls the readers back on `Err`.
    ///
    /// TOTAL OVER SCREEN CONTENT (the 2026-09-22/23 update audit, plan P0-1c/d).
    /// Each session is carried by [`crate::seamless::carry_for_wire`] at the
    /// most faithful rung the build's predicates admit at the successor's
    /// `caps` — a parser mid-sequence, a screen over the per-grid cap, a grid
    /// the wire cannot shape and a meta out of bounds each used to refuse the
    /// whole update, every attempt, from the OLDER build where no release could
    /// fix it; now each costs that one session a rung, at worst a blank tab that
    /// redraws. The pool is then committed by
    /// [`crate::seamless::settle_wire_carries`], which lowers whatever session
    /// `screen_digest` still blames. So `Err` is only ever timing (the deadline,
    /// or an engine lock it could not take), storage this process could not
    /// reserve, or a refusal that is not about any one screen (too many
    /// sessions, a duplicate id) — or this build refusing its own blank screen,
    /// which a test pins unreachable.
    ///
    /// `ceiling` is the highest rung the successor's signed handoff policy lets
    /// this producer carry at (plan P0-5, [`crate::seamless::carry_for_wire_within`]):
    /// `Full` is the ladder as it ships; `Visible` prices and carries no
    /// scrollback; `Repaint` carries every session blank. It only ever lowers.
    /// Whether an OSC 8 link left open over queued output may still miss this
    /// park (round seven, item 109): only while the launched lane's attempt
    /// has free mid-sequence re-parks left
    /// ([`PRELAUNCH_MAX_MID_SEQUENCE_REPARKS`]), so the link's close can arrive
    /// at the next batch boundary. Past them, on the fork lane and in the dry
    /// run, the link is carried closed rather than holding the update.
    #[cfg(unix)]
    fn open_link_may_repark(&self) -> bool {
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

        let open_link_may_repark = self.open_link_may_repark();
        let mut wire: Vec<WireCarry> = Vec::new();
        // The control carry's captures. Storage it cannot reserve carries
        // nothing (`carry_room` false) — never a failed capture.
        let mut carries = Vec::new();
        let carry_room = carries.try_reserve_exact(live.len()).is_ok();
        // The foreground holders, as best-effort as the control carry, but
        // never tied to it (see the push below).
        let mut fg_holders = Vec::new();
        let holder_room = fg_holders.try_reserve_exact(live.len()).is_ok();
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
        // Latched once a session's carried history was refused — by the budget,
        // or by the shape of the produced blob — so every later session goes
        // straight to visible-only rather than paying a probe that this pool
        // has already proven cannot pass. Named for its EFFECT, not either cause.
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
        // ONLY THE HANDED SESSIONS (the 2026-09-24 review): `live` leaves out a
        // pane kept open after its command exited (`App::handoff_live_sessions`),
        // and a screen carried for a session with no descriptor on the wire is
        // one `write_outgoing` refuses as inconsistent.
        let handed = |id: u64| live.iter().any(|(local_id, _, _)| *local_id == id);
        // IN SESSION-ID ORDER, NOT THE POOL'S. The pool is a `HashMap` with a
        // random seed per process, and this walk spends pool-wide state in order —
        // the history latch, the byte and cell budgets, the decode authority — so
        // an unordered walk parked the same desk differently from run to run:
        // which session kept its scrollback, which one met a spent budget. Ids
        // are allocated in creation order, so the oldest session is priced first,
        // every time. Reserved like the rest of this capture's storage.
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
            // A PARSER PARKED INSIDE A SEQUENCE NO CARRY HOLDS, OVER QUEUED
            // OUTPUT, IS A MISS, NOT A CARRY (the 2026-09-24 review). Since the
            // parser carry (the round-five plan's item 12) that is only a hooked
            // DCS string: every other partial sequence rides the checkpoint and
            // the successor continues it, so its queued tail is no hazard. A
            // hooked DCS is abandoned as CAN does, and the successor's parser
            // would print the rest of it — still waiting in the kernel — as
            // text. Only when the PTY is quiet (a stalled sequence, which may
            // never end) is abandoning the answer. One `poll` per such session:
            // every other desk pays nothing here.
            //
            // AN OLDER SUCCESSOR CONTINUES NOTHING: one from before the parser
            // carry ignores it and resumes at Ground, so on such a hop
            // (`WireCaps::carries_parser`) every partial sequence is the hazard
            // a hooked DCS is here, and is re-parked on a boundary the same way.
            //
            // A CARRY THAT ONLY SWALLOWS IS NO CONTINUATION EITHER (round six of
            // the update audit, finding 30): an OSC over the carry's cap (an
            // OSC 52 clipboard write, an OSC 1337 image), an unhooked DCS header
            // and an APC string with its consumer started (a kitty-graphics
            // command) are carried as sequences to ignore to their terminator,
            // so the successor would swallow the queued tail and dispatch
            // nothing — the image never drawn, the copy never made — where the
            // miss re-parks on a boundary and loses nothing
            // (`Terminal::partial_sequence_swallowed`).
            //
            // NOR IS ENGINE STATE NO CHECKPOINT CARRIES (round six of the
            // update audit, finding 47 (b) and (d)): an OSC 8 link still open
            // — the successor would commit the rest of its text unlinked, to
            // scrollback for good — and a VT52 `ESC Y` waiting for its address,
            // whose two bytes the successor would print as text
            // (`Terminal::output_state_uncarried`). Every successor loses
            // them, so every hop asks.
            //
            // BUT AN OPEN LINK IS COSMETIC, AND IT MAY NEVER CLOSE (round
            // seven, item 109): a program killed between an OSC 8 open and its
            // close leaves the tab's link open through every later prompt, and
            // a build in that tab keeps output queued at every park. Its miss
            // spends only the launched lane's free mid-sequence re-parks
            // (`open_link_may_repark`); past them, and on the fork lane, which
            // has none, the capture carries the screen and the successor stops
            // linking the rest of the text — one session's cosmetic state never
            // vetoes the update (L2). A VT52 address keeps the hard miss.
            let partial = if caps.carries_parser() {
                terminal
                    .partial_sequence_uncarried()
                    .or_else(|| terminal.partial_sequence_swallowed())
            } else {
                terminal.partial_sequence_state()
            }
            .or_else(|| {
                terminal
                    .output_state_uncarried()
                    .filter(|state| open_link_may_repark || *state != OPEN_LINK_STATE)
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
            // THE DECODE-AUTHORITY BUDGET is the producer's own ceiling — no
            // consumer checks it — so it may choose a rung but never refuse the
            // update (plan P0-1c). A session whose visible screen alone would
            // take the pool past it is carried blank; the history it might have
            // carried was already priced above by `history_fits_bytes`.
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
            // A carry whose history a predicate refused lowers every later
            // session's too — the budget or the shape that refused it is the
            // pool's, not this session's alone. Only the VisibleOnly rung says
            // that and nothing else: a Sanitized or StrippedLinks carry may
            // still hold its history, and a StrippedLinks or Repaint carry is
            // about this one screen (a link-dense line past the record cap,
            // its geometry over a cap, a meta with no clamp), which says
            // nothing about what the next session's history costs.
            if history != 0 && rung == CarryRung::VisibleOnly {
                history_latched_off = true;
            }
            capture_budget = crate::seamless::checkpoint_capture_budget_bytes(
                checkpoint.rows,
                checkpoint.cols,
                checkpoint.history_lines,
            )
            .map_or(u64::MAX, |bytes| capture_budget.saturating_add(bytes));
            // THE CONTROL CARRY'S SHARE OF THE FREEZE (round 10), and all of it:
            // the archive's fence and counters, plus the differ's screen-sized
            // state — under this same lock, so it describes exactly the screen
            // this checkpoint carries. The rows and the ledger are exported on
            // the worker, behind the fence. It cannot fail the capture, and past
            // half the budget it leaves even the differ's state out — the worker
            // takes it then, while the fence's fingerprint of it holds (nothing
            // committed since), so the adopting engine still goes on exactly:
            // the carry is never worth the deadline. A session whose screen was
            // clamped or carried blank goes without: its differ state describes
            // a screen that was not carried (plan P0-1e). One that lost only
            // the links of its over-cap lines keeps it — the differ's state is
            // the rows' text, which is exact (`CarryRung::keeps_control_carry`).
            let differ = deadline.saturating_duration_since(std::time::Instant::now())
                >= handoff_history_comfort;
            // THE FOREGROUND HOLDER, for EVERY session and apart from the
            // control carry below (the 2026-09-25 review). The readers are
            // parked, so it is the last group the reader saw. A session at the
            // Sanitized or Repaint rung, or one whose sidecar the aggregate
            // budget drops, goes without the control carry but still restores
            // its modes: its holder is what lets the adopting reader hand back
            // a job that died during the handoff. Reserved above: no allocation.
            if holder_room {
                fg_holders.push((
                    session.id,
                    session.fg_holder.load(std::sync::atomic::Ordering::Acquire),
                ));
            }
            // THE HISTORY HEAD, under the same lock as the checkpoint: the
            // fence the worker joins this session's export to. A pure read.
            // Reserved for every live session above, so this never allocates.
            history_heads.push(crate::handoff_history::capture_head(session.id, &terminal));
            // Reserved for every live session above, so this never allocates.
            if carry_room && rung.keeps_control_carry() {
                carries.push(crate::handoff_carry::capture_head(
                    session.id,
                    &terminal,
                    &session.term,
                    &session.ctx.turns,
                    differ,
                ));
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
        // THE SELF-CHECK (plan P0-1d): commit the pool with `screen_digest`, and
        // lower whatever session it still blames instead of refusing the update.
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
        // its control carry too (`CarryRung::keeps_control_carry`).
        carries.retain(|source: &crate::handoff_carry::CarrySource| {
            wire.iter().any(|carry| {
                carry.local_id == source.local_id() && carry.rung.keeps_control_carry()
            })
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
        let held = if deadline.saturating_duration_since(std::time::Instant::now())
            >= handoff_history_comfort
        {
            let held_deadline = deadline
                .checked_sub(handoff_history_comfort / 2)
                .unwrap_or(deadline);
            self.capture_held_screens(live, held_deadline, &mut capture_cells, caps, ceiling)
        } else {
            Vec::new()
        };
        Ok(ParkedScreens {
            screens,
            carries,
            screen_digest,
            repaint,
            fg_holders,
            history_heads,
            held,
        })
    }

    /// The visible screen of every pooled session NOT in `live` — a pane kept
    /// open after its command exited, which is never handed (round five, item
    /// 19) — at the most faithful rung the successor's `caps` and the policy's
    /// `ceiling` admit, without scrollback. Best-effort in every way: a pane
    /// whose engine is busy, whose screen would go blank for a repaint no
    /// program can answer, or that the deadline reaches first is left out, and
    /// the successor shows it as the placeholder it always did. No lock but
    /// each engine's own, one `try_lock` each (the readers are parked, and an
    /// exited pane's reader is gone): nothing here can wait.
    ///
    /// BOUNDED BY WHAT CAN CROSS (round six of the update audit, finding 21):
    /// `deadline` is the held capture's own, short of the park's, and it stops
    /// at [`crate::seamless::MAX_HELD_PANES`] and skips a screen past what is
    /// left of [`crate::seamless::MAX_HELD_AGGREGATE_GRID_BYTES`] — the caps
    /// the manifest writer applies — so no pane is projected inside the freeze
    /// only to be dropped. Each carries how its command ended
    /// (`App::exit_status`, which an exited pane has already collected — or
    /// collects now, never waiting).
    #[cfg(unix)]
    fn capture_held_screens(
        &self,
        live: &[(u64, i32, i32)],
        deadline: std::time::Instant,
        capture_cells: &mut u64,
        caps: crate::seamless::WireCaps,
        ceiling: aterm_update_core::handoff_policy::CarryCeiling,
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
            drop(terminal);
            if rung.needs_repaint() || screens.try_reserve(1).is_err() {
                continue;
            }
            let screen = crate::seamless::HeldScreen {
                local_id: session.id,
                checkpoint,
                exit: self.exit_status(session.id, crate::app_tabs::ExitLook::Retry),
            };
            let bytes = screen.grid_bytes();
            if held_bytes.saturating_add(bytes) > crate::seamless::MAX_HELD_AGGREGATE_GRID_BYTES {
                continue;
            }
            held_bytes = held_bytes.saturating_add(bytes);
            *capture_cells = cells;
            screens.push(screen);
        }
        screens
    }

    /// THE DRY RUN (gap #25): the park's own capture — [`Self::capture_parked_screens`],
    /// the one the park runs, over the same handed set — timed with every reader
    /// LIVE and nothing parked, so the first freeze rung can be budgeted from what
    /// this desk costs instead of from a constant it had outgrown. What it cannot
    /// time — the readers' stop, the freeze itself — the last landed park's cost
    /// supplies (`keep_landed_park`).
    ///
    /// NOTHING IS PARKED AND NOTHING LEAVES. The capture is a pure read of each
    /// engine under its own lock (the checkpoint, the archive and history heads,
    /// the foreground holder), and its product is dropped here: no descriptor is
    /// duplicated, no artifact written, no reader stopped. So the handoff stays in
    /// the island's `launched` phase throughout — this is no protocol step — and a
    /// reader that wants its engine meanwhile waits one session's capture at most,
    /// which is what the lock would cost it on any redraw.
    ///
    /// It prices the WIDEST capture a rung would attempt: history is carried for
    /// every session the aggregate budgets admit (`Duration::ZERO` comfort), as
    /// on a rung with time to spare, and the window is the rung ceiling's two
    /// thirds — past that, 1.5 × the time already saturates the ceiling, so the
    /// main thread never spends more than [`DRY_RUN_CAPTURE_WINDOW`] learning it.
    ///
    /// UNDER THE SUCCESSOR'S HANDOFF POLICY ([`Self::dry_run_ceiling`]), like the
    /// park it prices. The policy exists for a capture path that is slow or
    /// panics, and this runs on the main thread for every automatic attempt: a
    /// dry run at `Full` would run the very path a `carry = "repaint"` policy is
    /// sealed to route around, on the one thread whose panic or hang takes the
    /// whole terminal with it. Pricing at the ceiling is also the right price:
    /// every park of this attempt captures under that same policy (the launched
    /// lane re-reads it at each gate run), so the ceiling's capture IS the
    /// widest one a rung will attempt — a `Full` price would budget for history
    /// no park will carry. With the attempt's policy not read yet (no pass of
    /// its artifact on record) no dry run is taken at all (`Unmeasured`), for
    /// the first reason: its ceiling is unknown, and the park, which reads it
    /// once the worker has published it, may be forbidden the very capture
    /// this would run. The seed then falls back to the ledger's last
    /// measurement, as for any dry run that measured nothing.
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

    /// The carry ceiling the dry run captures under: the successor's handoff
    /// policy for the prelaunched attempt's artifact, read by
    /// [`Self::park_policy`] — the call every park makes — so the dry run never
    /// captures above what the park will. `Full` with no attempt to name an
    /// artifact (the same-image seam, which has no policy), and `None` while an
    /// attempt's policy has not been read (no passed pre-verification of its
    /// artifact on record): its ceiling is unknown, so nothing is captured.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    fn dry_run_ceiling(&self) -> Option<aterm_update_core::handoff_policy::CarryCeiling> {
        let attempt = self
            .update_handoff_prelaunch
            .as_ref()
            .and_then(|prelaunch| prelaunch.apply_attempt.as_ref());
        let policy = self.park_policy(attempt);
        (attempt.is_none() || policy.known).then(|| policy.carry_ceiling())
    }

    /// Seed this attempt's first automatic freeze rung (gap #25): time the dry
    /// run while the successor boots, never budget below the last park that
    /// landed (which the dry run cannot see: the readers' stop), persist what it
    /// measured in the health ledger for the next update (off this thread — the
    /// ledger's lock is a file lock another process may hold), and fall back to
    /// what the ledger holds, then the 20 ms default, when it measured nothing.
    /// An explicit apply's window is the widest rung whatever rung 0 is, so it
    /// spends no dry run. The seed is stored on the prelaunched attempt, whose
    /// every park reads it ([`Self::prelaunched_park_budget`]), and returned.
    ///
    /// Also returns the ledger writer's handle when one was started: production
    /// lets it run detached; a test joins it before reading the ledger back.
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
                let seed = FreezeSeed::DryRun {
                    capture: took,
                    sessions,
                    last_park: FreezeSeed::last_park_of(prior.as_ref()),
                };
                let record = aterm_update::HandoffCaptureRecord {
                    capture_us: u64::try_from(took.as_micros()).unwrap_or(u64::MAX).max(1),
                    sessions: u32::try_from(sessions).unwrap_or(u32::MAX),
                    freeze_seed_ms: u64::try_from(seed.rung0().as_millis()).unwrap_or(u64::MAX),
                    ..aterm_update::HandoffCaptureRecord::default()
                };
                let build = crate::running_build_number();
                match std::thread::Builder::new()
                    .name("aterm-freeze-seed".to_string())
                    .spawn(move || {
                        // A small ledger write nobody waits on.
                        crate::qos::set_self(crate::qos::Role::Background);
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

    /// The prelaunched attempt's budget for its NEXT park: the ladder at the
    /// target's prior physical failures plus this attempt's own misses, with
    /// rung 0 the seed its dry run bought — `(prior failures incl. misses,
    /// budget, seed)`, or `None` with no attempt prelaunched. The one place the
    /// park reads its budget from.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn prelaunched_park_budget(&self) -> Option<(u8, std::time::Duration, FreezeSeed)> {
        let prelaunch = self.update_handoff_prelaunch.as_ref()?;
        let prior = self
            .prior_physical_failures_of(prelaunch.apply_attempt.as_ref())
            .saturating_add(prelaunch.park_misses);
        Some((
            prior,
            handoff_freeze_budget(prelaunch.mode, prior, prelaunch.freeze_seed),
            prelaunch.freeze_seed,
        ))
    }

    /// The launched lane's first half (2026-09-19, the late park): record the
    /// attempt, hand the worker everything it needs to launch and hold the
    /// successor, and RETURN WITH EVERY READER LIVE. The park is taken later, on
    /// this thread, when the worker reports the successor's dial
    /// (`Wake::UpdateHandoffAwaitingPark`) and the park gate admits.
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
        // MINTED HERE, BEFORE THE LAUNCH: the launch environment names the
        // manifest and layout by path, and the writer derives the same names
        // from this nonce after the park.
        let nonce = crate::seamless::mint_outgoing_nonce();
        let (stand_down_tx, stand_down_rx) = std::sync::mpsc::sync_channel(1);
        let (stand_down_ack_tx, stand_down_ack_rx) = std::sync::mpsc::sync_channel(1);
        let (transfer_tx, transfer_rx) = std::sync::mpsc::sync_channel(1);
        let arbiter = crate::HandoffAttemptArbiter::new();
        let preverified =
            PreverifyPublisher::for_attempt(&self.handoff_preverified, apply_attempt.as_ref());
        // Started now, beside the launch: the successor's whole boot runs
        // while the history is exported with every reader live — unless the
        // successor's policy, when a pass of it is already on record, carries
        // no scrollback.
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
            }),
            lane: HandoffLane::OutOfBand,
            // Set by the WORKER immediately before the candidate is launched; the
            // main thread must not touch the staging directory here.
            trial_launches_before: 0,
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
        // THE SUCCESSOR IS BOOTING (seconds: launch->dial was 1.1–4.6 s on the
        // owner's last four updates), and nothing waits on this thread until it
        // dials — so the park's capture is timed NOW, with every reader live, and
        // the first freeze rung is budgeted from it and from the last park that
        // landed (gap #25). No phase moves: the attempt is `launched` before
        // this and after it, which now carries the seed its parks read. The
        // ledger writer runs detached: nothing here waits on a file lock.
        let _seeded =
            self.seed_first_freeze_rung(mode, crate::seamless::WireCaps::for_target(target_build));
        Ok(())
    }

    /// `Wake::UpdateHandoffAwaitingPark`: the worker has the successor's dial in
    /// hand (or must fork), so the park may now be taken — at once for an
    /// explicit apply, at the next quiet moment for the automatic lane.
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

    /// THE PARK GATE for a prelaunched attempt, re-run from the event loop every
    /// [`PRELAUNCH_PARK_RETRY`] while the automatic lane waits for a quiet
    /// moment, and bounded by [`PRELAUNCH_HOLD_MAX`]: an attempt held past it
    /// stands down as activity-revoked (no physical budget spent), and the
    /// automatic lane retries at a later quiet window.
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
        // A PARK THAT DID NOT HAND ITS CAPTURE OVER RESUMES THE HISTORY EXPORT
        // it paused (round six of the update audit, finding 14): the readers
        // are live again, and an export left paused would meet the re-park's
        // heads with a fence that ended at this attempt — `Outrun` for any tab
        // that printed past the screen carry's lines in the retry delay. A
        // landed park took the export with its capture; one that never paused
        // it is not moved.
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
            // A REFUSED CAPTURE IS NOT A MISSED PARK (2026-09-22/23 update
            // audit, plan P0-3). The readers are back and nothing was granted,
            // exactly as for a miss — but a wider rung cannot change what the
            // capture refused, so re-parking would spend the user's freeze on
            // an answer already known, and standing down as `ActivityRevoked`
            // would tell the lane (and the bar) that the machine was busy.
            // The successor stands down typed `CaptureRefused` instead: the
            // refusal lane records it and retries once the desk changes.
            ParkAttempt::Refused(refusal) => {
                aterm_log::warn!(
                    "update apply: {refusal}; a wider freeze rung cannot change that, so the \
                     held successor stands down and the lane retries once the desk changes"
                );
                self.stand_down_prelaunched_successor(capture_refusal_stand_down(&refusal));
            }
            // A PARK THAT MISSED ITS BUDGET IS NOT A FAILED UPDATE (2026-09-19).
            // The readers are back (nothing was granted, so the rollback is
            // exact), the successor is still booted and holding, and the only
            // thing that went wrong is that a 20 ms window was not enough on a
            // busy machine. Re-park on the ladder's next rung rather than
            // throwing away a whole relaunch to buy the same rung later — and
            // once every rung has been tried, stand down as what it is: a busy
            // machine, retried on the activity spacing, never a latch
            // (`park_miss_disposition`).
            ParkAttempt::Missed(reason) => {
                // EVERY MISS IS PRE-RECORD. The parked attempt is stored at the
                // very end of the park half, after the last thing that can miss,
                // so a miss leaves only the prelaunch record and resumed readers
                // — which is exactly what makes re-parking under the same nonce
                // sound (no artifact has been written, nothing has been granted).
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

    /// Tell the worker to stand the held successor down with a typed reason.
    /// Idempotent per attempt; the completion the worker then sends is what
    /// clears the attempt record.
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
    /// UNGRANTED successor and is waiting for this answer before it does.
    ///
    /// Two things happen here and they happen on the main thread, which is where
    /// the park gate also runs — so a park can neither be in progress nor start
    /// afterwards:
    ///
    ///  1. THE GATE CLOSES FOR GOOD. Nothing may park onto a successor that is
    ///     being killed; before this existed a 20 ms gate tick could freeze the
    ///     terminal onto a candidate the worker had already given up on.
    ///  2. IF A PARK ALREADY LANDED, its readers resume NOW — the successor
    ///     holds no descriptor (the one `sendmsg` is the grant and it has not
    ///     happened), so this is exact, and it is the model's `UnparkForRepark`.
    ///     Without it the user paid the stand-down's grace, SIGKILL and reap as a
    ///     frozen terminal.
    ///
    /// The attempt RECORD outlives both, until the completion: it is what stops a
    /// retry launching beside the candidate being stood down. The parked record's
    /// deferred teardown and activity flag move onto it so the completion still
    /// replays them.
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
        // ANSWERED LAST, after the gate is shut and the readers are back: the
        // worker revokes on this, so everything it must not race has to be done
        // before it is sent. A full or disconnected channel means the worker
        // stopped waiting, which its own bounded wait already reports.
        if let Some(ack) = ack {
            let _ = ack.try_send(());
        }
    }

    /// The launched lane's second half: the park itself, at the dial or at the
    /// quiet moment after it. Everything that depends on the readers being
    /// stopped happens here — the registry projection, the descriptor
    /// duplicates, the window carry, the device-term proof identities, the
    /// capture ladder, the layout and both digests — and the product crosses to
    /// the worker holding the successor's claim as one [`HandoffTransferJob`].
    ///
    /// `NotYet` is "try again in a moment" (the gate, a busy registry);
    /// `Failed` stands the successor down with the outcome the completion should
    /// carry. Every `Failed` after `park_all_readers` has already rolled the
    /// readers back: the successor holds nothing, so the readers never wait for
    /// its stand-down.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    fn park_and_transfer_to_prelaunched_successor(
        &mut self,
        now: std::time::Instant,
    ) -> ParkAttempt {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};

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
        let build = crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0);
        // THE SUCCESSOR'S SIGNED HANDOFF POLICY (plan P0-5): read by the key of
        // exactly the artifact this attempt launched. Its two knobs only lower
        // what this park carries and relax a gate only this producer applies.
        // Re-read at every gate run: a lock and a compare, and the verdict the
        // worker published after re-verifying the candidate may have landed
        // since the last one.
        let policy = self.park_policy(apply_attempt.as_ref());
        let ceiling = policy.carry_ceiling();
        // A CEILING THAT CARRIES NO SCROLLBACK STOPS THE HISTORY EXPORT NOW,
        // before the gate below would wait on its first pass: the export was
        // started beside the launch, when the policy may not have been read
        // yet, and nothing it writes may cross (`HistoryPlan::Withheld`).
        if !ceiling.carries_scrollback() {
            self.withhold_history_export(target_build, ceiling);
        }

        // Every session with a live process behind it: a pane kept open after
        // its command exited is left out HERE, before the gate reads the
        // masters, so its hung-up master can neither hold the park nor fail it
        // (`handoff_live_sessions`).
        let live: Vec<(u64, i32, i32)> = self.handoff_live_sessions();
        if live.is_empty() {
            return ParkAttempt::Failed(HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
                detail: "every terminal session closed while the successor booted".to_string(),
            });
        }
        // THE SCROLLBACK EXPORT'S FIRST PASS FIRST (`crate::handoff_history`):
        // it runs with every reader live, and a park that does not wait for it
        // leaves each session it has not reached with the screen carry's
        // bounded history (counted). Bounded by
        // `handoff_history::EXPORT_PATIENCE` after the dial, in every mode —
        // waiting freezes nothing. After its first pass the export FOLLOWS
        // each history until the park stops it below.
        let exporting = self
            .update_handoff_prelaunch
            .as_mut()
            .and_then(|prelaunch| prelaunch.history_export.as_mut())
            .is_some_and(|export| !export.caught_up());
        if let Some(reason) =
            crate::handoff_history::park_wait(exporting, now.saturating_duration_since(dialled_at))
        {
            return ParkAttempt::NotYet(reason);
        }
        // THE GATE — the only decision that costs the user anything, taken here
        // and nowhere else (`prelaunch_park_admitted` says what each mode owes).
        // Session DEATH is re-checked under the park below for every automatic
        // mode; this is about idleness, which is a different question.
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
            ParkGate::Wait(reason) => return ParkAttempt::NotYet(reason),
            ParkGate::StandDown(reason) => {
                return ParkAttempt::Failed(HandoffStandDown {
                    outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
                    detail: hold_cap_stand_down_detail(reason, gate_facts.held_for, last_wait),
                });
            }
        }
        // THE EXPORT IS PAUSED BEFORE ANYTHING FREEZES: it follows every
        // history with the readers live, and a chunk read beside the park
        // would contend with the capture's locks and with the layout's
        // `try_lock` (a degraded layout reads as a moved topology at Commit).
        // The pause takes one bounded last look at what landed since the
        // export's last tick; what lands after it rides the screen carry. An
        // export whose FIRST pass outran its patience leaves the tabs it had
        // not reached with the screen carry's bounded history, counted by the
        // join. PAUSED, NOT STOPPED (round six of the update audit, finding
        // 14): every return below that does not hand the capture over leaves
        // the export on the attempt, and the gate resumes it with the readers
        // (`try_park_for_prelaunched_successor`), so a re-park joins an export
        // that kept following — not one frozen where this attempt left it.
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
        // THIS PROCESS'S OWN LIMITS ARE NOT A VERDICT ABOUT THE CANDIDATE
        // (2026-09-22/23 update audit, plan P1-2): each stand-down below the
        // gate that is not the park or the capture is `ProducerFailed` —
        // Transient — where it used to be `PreparationFailed`, which files a
        // user opening a 63rd tab during the hold, or an `EMFILE`, as the
        // staged bytes failing and latches after two.
        //
        // THE LIMIT IS THE DIALLED CLAIM'S, and only the rendezvous lane has one
        // (round six of the update audit, items 1 and 38). `transfer` holds the
        // pool to what the successor CLAIMED, not to what the cached
        // pre-verification says its `Info.plist` declares: the two used to
        // disagree whenever the worker skipped its own verification, and the
        // park then froze every reader for a grant `transfer` refused. And a
        // fork (`fork_after_park`, `grant_limit: None`) passes its masters by
        // inheritance, so no descriptor message bounds it at all — the capture
        // still refuses past `MAX_HANDOFF_SESSIONS`.
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
        // Capture the session registry BEFORE parking, because this projection can
        // FAIL: a `WouldBlock` here must return with the terminal completely
        // untouched, which is only true while no reader has been stopped. It
        // performs no disk I/O. The hold serials first, for the reason the fork
        // lane gives (`hold_serials`).
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
        let mut owned_masters = Vec::with_capacity(live.len());
        let mut adoption = Vec::with_capacity(live.len());
        for (local_id, master, pid) in &live {
            // SAFETY: duplicates one live parent master as an independent CLOEXEC
            // descriptor. The original can later close/reuse its number without
            // changing the open-file-description inherited by the child.
            let duplicate = unsafe { libc::fcntl(*master, libc::F_DUPFD_CLOEXEC, 3) };
            if duplicate < 0 {
                let kind = std::io::Error::last_os_error().kind();
                return ParkAttempt::Failed(HandoffStandDown {
                    outcome: crate::UpdateHandoffOutcome::ProducerFailed,
                    detail: format!("could not reserve child-only PTY descriptors ({kind})"),
                });
            }
            // SAFETY: F_DUPFD_CLOEXEC returned a fresh descriptor owned here.
            let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicate) };
            adoption.push((*local_id, owned.as_raw_fd(), *pid));
            owned_masters.push(owned);
        }
        let fds = crate::session_store::HandoffFds {
            entries: adoption.clone(),
        };
        let window = self.handoff_window_carry(target_build);
        // THE PROOF TERM IS THE PTY DEVICE on this lane (the descriptors travel
        // by `SCM_RIGHTS`, which installs the receiver's own numbers), and a
        // launched attempt that must fork at runtime keeps that term — the
        // worker says so in the successor's environment either way.
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
        // THE LADDER, PLUS THIS ATTEMPT'S OWN MISSES. A park that missed its
        // budget re-parks on the next rung (seed -> 2 × seed, at least 80 ->
        // 250 ms) without giving the booted successor back: the rung a physical
        // failure would have bought the NEXT attempt, bought here for 500 ms
        // instead of a relaunch. Rung 0 is the seed bought beside the launch —
        // the dry run, floored by the last landed park (gap #25).
        let Some((prior_physical_failures, freeze_budget, freeze_seed)) =
            self.prelaunched_park_budget()
        else {
            return ParkAttempt::NotYet("no attempt is prelaunched");
        };
        let freeze_ms = freeze_budget.as_millis();
        let handoff_history_comfort = freeze_budget / 2;
        aterm_log::info!(
            "update apply: freeze budget {freeze_ms} ms ({mode:?}, {prior_physical_failures} prior \
             physical failure(s) of the target artifact incl. {park_misses} park miss(es) of this \
             attempt; {freeze_seed}; running build {build}) — parking {} ms after the successor's \
             dial",
            now.saturating_duration_since(dialled_at).as_millis()
        );
        // THE INSTANT THE TERMINAL STOPS ECHOING — the start of the freeze the
        // user experiences, and the zero point of the numbers reported at Commit.
        let park_at = std::time::Instant::now();
        let deadline = park_at + freeze_budget;
        if !self.park_all_readers(deadline) {
            self.rollback_overlap(None, &live);
            return ParkAttempt::Missed(format!(
                "a PTY reader missed the {freeze_ms} ms handoff park deadline"
            ));
        }
        // The split the freeze's one number used to hide (gap #25): how much of
        // it stopping the readers took, and how much the capture after it.
        let readers_parked_at = std::time::Instant::now();
        // Session DEATH is a safety fact, not an idleness preference: every
        // automatic mode refuses to commit an adoption proof whose live set died
        // underneath it.
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
                // A parser caught mid-sequence over a flood is a miss whose
                // cure is the next batch boundary, not a wider rung: it re-parks
                // at the gate's own cadence, bounded
                // ([`mid_sequence_miss_disposition`]).
                if matches!(failure, CaptureFailure::MidSequence { .. }) {
                    return ParkAttempt::MissedMidSequence(failure.to_string());
                }
                // A MISS OR A REFUSAL, decided by the failure's TYPE (plan
                // P0-3): only timing and storage re-park on the next rung.
                return classify_capture_failure(&failure);
            }
        };
        // The foreground holders ride the manifest records, whatever rung.
        let manifest = stamp_fg_holders(manifest, &fg_holders);
        // POST-PARK, AND DELIBERATELY SO — see the fork lane for why a pre-park
        // layout capture revoked healthy attempts.
        let layout = self.capture_handoff_layout();
        let Some(layout_digest) = crate::seamless::layout_digest(&layout) else {
            self.rollback_overlap(None, &live);
            // Deterministic: the same layout refuses on every rung.
            return ParkAttempt::Refused(CaptureRefusal {
                local_id: None,
                cause: "handoff layout could not be committed canonically".to_string(),
            });
        };
        if std::time::Instant::now() >= deadline {
            self.rollback_overlap(None, &live);
            return ParkAttempt::Missed(format!(
                "handoff proof capture exceeded the {freeze_ms} ms deadline"
            ));
        }
        // THE PARK LANDED INSIDE ITS BUDGET: what it cost is the number the next
        // update's first rung should be budgeted from (gap #25) — the dry run
        // cannot time the readers' stop, and this can.
        let _ledger_write = keep_landed_park(park_at.elapsed());
        let activity_epoch = self.update_handoff_activity_epoch;
        let proof_deadline = handoff_proof_deadline(mode, prior_physical_failures, park_at);
        self.pending_update_handoff = Some(crate::PendingUpdateHandoff {
            attempt_id,
            park_at,
            proof_ready_at: None,
            // Sampled AT THE PARK: whether the user was looking at aterm when the
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
            cancel,
            arbiter,
            teardown: crate::DeferredHandoffTeardown::None,
            commit_drain_started: None,
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
                // Taken here, after every point a park can miss at: a re-park
                // keeps the export it waited for, resumed after the miss
                // (`HistoryExporter::resume`). Handed over paused; the worker's
                // `finish` stops it. Withheld — and any export stopped — under
                // the ceiling this capture was taken under when it carries no
                // scrollback.
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
        // A live `video` take is NOT answered here: the proof can still fail
        // to come, and the attempt then rolls back with this process running.
        // Commit answers it, and waits for the answer to reach its client
        // (`App::video_answer_before_commit`; round six of the update audit,
        // findings 22 and 31).
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

    /// Every pooled session's hold serial, summed
    /// ([`crate::fabric::SessionFabric::hold_serial`]) — the park's reading of
    /// "where every halt stands", compared again at Commit.
    ///
    /// A sum of per-session counters that only ever grow, so any hold moving
    /// anywhere changes it; a session joining or leaving the pool changes it
    /// too, which `exact_sessions` refuses on its own. Per session rather than
    /// process-wide, so the reading is this `App`'s and not a neighbour's in
    /// the same test binary. Leaf reads, one short fabric lock each — never
    /// under a `Terminal` or the registry.
    #[cfg(unix)]
    pub(crate) fn hold_serials(&self) -> u64 {
        self.pool
            .iter()
            .map(|session| session.ctx.fabric.hold_serial())
            .fold(0, u64::wrapping_add)
    }

    /// [`Self::hold_serials`] as the COMMIT reads it: every pooled session's
    /// serial read under the guard that also fences it
    /// ([`crate::fabric::SessionFabric::fence_hold`]), so from this reading
    /// until the process exits no `hold` can land unseen — one that took the
    /// guard first is in the sum and refuses the Commit, one after waits for
    /// the attempt. The fence lifts when the returned guard drops, which every
    /// path that stands the attempt down does; a Commit that lands `_exit`s
    /// with it still raised.
    #[cfg(unix)]
    fn fence_hold_serials(&self) -> (u64, HoldFence) {
        let fabrics: Vec<std::sync::Arc<crate::fabric::SessionFabric>> = self
            .pool
            .iter()
            .map(|session| std::sync::Arc::clone(&session.ctx.fabric))
            .collect();
        let serials = fabrics
            .iter()
            .map(|fabric| fabric.fence_hold())
            .fold(0, u64::wrapping_add);
        (serials, HoldFence(fabrics))
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
        let arbiter = pending.arbiter.clone();
        let teardown_allows_commit = matches!(
            pending.teardown,
            crate::DeferredHandoffTeardown::None | crate::DeferredHandoffTeardown::CleanQuitReady
        );
        // A HOLD THAT MOVED SINCE THE PARK IS STRUCTURAL ACTIVITY: the manifest
        // the successor adopted from carries the holds as the park drew them,
        // so committing now would hand over a session whose halt the successor
        // never saw — or resurrect one a bridge just lifted (`hold_serials`).
        // Read under the FENCE, which holds every later `hold` back until
        // this attempt either exits or stands down (`fence_hold_serials`):
        // this is the last time anything compares the serials.
        let (commit_hold_serials, hold_fence) = self.fence_hold_serials();
        let exact_activity = self.update_handoff_activity_epoch == pending_activity_epoch
            && commit_hold_serials == pending_hold_serials;
        // The live set as the park drew it: an exited pane the park left out
        // stays out, but a HANDED session that exited during the overlap stays
        // IN, so its death reaches `sessions_alive` — which names the reason
        // ("a handed-off PTY session closed") — rather than reading as the set
        // changing. Both are activity-shaped (round six, finding 37).
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
        // TOPOLOGY, not the raw capture — see `commit_layout_topology` for the
        // two fields it normalizes away and why each of them was rejecting
        // perfectly healthy attempts. The raw inequality is still worth one log
        // line: it is the only place a window drag or a contended session lock
        // becomes visible in the field, and the whole point of this change is
        // that neither may ever again be REPORTED as a topology change.
        let live_layout = self.capture_handoff_layout();
        let exact_layout =
            commit_layout_topology(&live_layout) == commit_layout_topology(&pending_layout);
        if exact_layout && live_layout != pending_layout {
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
        Some((facts, native_safety, proof, arbiter, hold_fence))
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
            // `_hold_fence` stays raised to the end of this arm: through the
            // Commit's `_exit`, or until the rejection below is under way.
            let Some((facts, native_safety, proof, arbiter, _hold_fence)) = self
                .collect_handoff_commit_facts(
                    nonce.as_deref(),
                    input_dispatch_fenced,
                    egress_settled,
                    commit_fd.is_some(),
                )
            else {
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
                    // THE SUCCESSOR TAKES THE FRONT AT COMMIT, and only when an
                    // aterm window had focus at the park (2026-09-19): the launch
                    // never activated, so this is the one instant the user's
                    // focus moves — onto the window that is about to be theirs,
                    // never onto a candidate. With no aterm window focused the
                    // user is in another app and keeps it.
                    #[cfg(target_os = "macos")]
                    if self
                        .pending_update_handoff
                        .as_ref()
                        .is_some_and(|pending| pending.activate_at_commit)
                        && let Some(pid) = child_pid.and_then(|pid| i32::try_from(pid).ok())
                    {
                        let accepted = crate::app_launch_successor::activate_running(pid);
                        aterm_log::info!(
                            "update apply: activating the successor at Commit (an aterm window \
                             had focus at the park): {}",
                            if accepted {
                                "accepted"
                            } else {
                                "refused by AppKit — the successor stays behind"
                            }
                        );
                    }
                    // Success cannot return: `commit_and_exit` performs the one
                    // atomic <=PIPE_BUF write and `_exit(0)` in the same typed
                    // operation. EPIPE explicitly transfers Committing back to
                    // Rejecting so one reaper can restore the parent.
                    // The supervisor host stops at Commit: `suspend` sets every
                    // worker's stop flag and cuts its connection before it
                    // returns, and a cut worker's transport refuses every press
                    // or typed request (one already on the wire is the only
                    // exception); the threads are reaped later, off this thread.
                    // The successor resumes its own at the same Commit. A Commit
                    // that returns (failed) resumes this one below.
                    if let Some(host) = self.harness.as_ref() {
                        host.suspend();
                    }
                    // EVERY LIVE `video` REQUEST IS ANSWERED HERE, AND ITS
                    // ANSWER WRITTEN, BEFORE THE `_exit` (round six of the
                    // update audit, findings 22 and 31). Not at the park: every
                    // step from there to this proof could still roll the
                    // attempt back with this process running, and a take
                    // aborted then was lost to an update that did not happen.
                    // Here the take is aborted and its dir removed, an export
                    // is cancelled and its encode worker waited for, and then
                    // every reply's write on its control connection — `_exit`
                    // before that write was a dropped connection. Bounded
                    // ([`VIDEO_EXPORT_COMMIT_WAIT`]) and said: the adoption
                    // proof is in hand, and what does not answer in time is
                    // left to the process's end, as before.
                    let video = self.video_answer_before_commit(VIDEO_EXPORT_COMMIT_WAIT);
                    if video.answered {
                        aterm_log::info!(
                            "update apply: answered a live video recording or export at Commit"
                        );
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
                    let commit_result = match operator_quiesce.as_ref() {
                        Some(quiesce) => quiesce.with_commit_permit(|| {
                            crate::seamless::commit_and_exit(commit_fd, proof)
                        }),
                        None => Ok(crate::seamless::commit_and_exit(commit_fd, proof)),
                    };
                    if let Some(host) = self.harness.as_ref() {
                        host.resume();
                    }
                    if let Some(pid) = child_pid {
                        crate::identity_claim::withdraw_successor_markers(&carried, pid);
                    }
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
            return;
        }

        // Every non-ready completion is emitted only AFTER the worker killed and
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
    /// The cached verdict's own reason for a refusal, when the entry is the
    /// fresh one for THIS artifact (the same key [`Self::cached_handoff_preverification`]
    /// reads by).
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn cached_handoff_preverification_reason(
        &self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
    ) -> Option<String> {
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
            .and_then(|entry| entry.reason.clone())
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

    /// THE FORK LANE WAITS FOR AN UNREAD POLICY WHILE IT IS BEING READ (round
    /// seven, item 36). `Some(deferral)` for an automatic fork-lane attempt at
    /// `target_build` with no verified pass on record while a policy read of
    /// that target is in flight: the first such attempt starts the read (off
    /// this thread, [`Self::spawn_staged_handoff_preverification`], which joins
    /// an arm-time read still running rather than starting a second), and every
    /// attempt is deferred as activity until the read ends, so the next one
    /// parks under what it answered. `None` otherwise — a person's apply (it
    /// does not wait, and fails exactly as before), a policy already known, no
    /// read running (none could start, or it ended with nothing on record: a
    /// check that ran out of time), or [`crate::HANDOFF_PREVERIFY_READ_CEILING`]
    /// spent since the first deferral at this target — and the attempt then
    /// parks as the fork lane always did. Bounded (L5): that ceiling, on the
    /// activity spacing, each deferral said in the log.
    #[cfg(unix)]
    fn fork_lane_policy_wait(
        &mut self,
        mode: crate::native_updater_service::ApplyMode,
        target_build: u64,
        policy_unread: bool,
    ) -> Option<crate::UpdateHandoffStartError> {
        if !policy_unread || !mode.is_automatic() {
            return None;
        }
        let now = std::time::Instant::now();
        // One wait per target — renewed only once a cached verdict would have
        // gone stale since it ended, when the policy is unread again for the
        // same reason a first attempt's was.
        let until = match self.fork_policy_read_waited {
            Some((target, until))
                if target == target_build && now < until + crate::HANDOFF_PREVERIFY_FRESHNESS =>
            {
                until
            }
            _ => {
                let until = now + crate::HANDOFF_PREVERIFY_READ_CEILING;
                self.fork_policy_read_waited = Some((target_build, until));
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
                    "update apply: build {target_build}'s handoff policy read is still marked \
                     running past the {} s the fork lane waits for it — past the lock wait \
                     and verification budget that bound it, so its thread never ran; \
                     parking without it",
                    crate::HANDOFF_PREVERIFY_READ_CEILING.as_secs()
                );
            }
            return None;
        }
        let reason = format!(
            "build {target_build}'s handoff policy is being read before the fork lane parks; \
             the attempt after it ends parks under it"
        );
        aterm_log::info!("update apply: {reason}");
        Some(crate::UpdateHandoffStartError::activity(reason))
    }

    /// A fork-lane park MISS: count the rung it buys the next attempt and
    /// return the typed start error that routes it to the activity-revoked
    /// spacing rather than the physical budget (the 2026-09-22/23 update audit,
    /// plan P1-4 — the fork lane charged a 20 ms stopwatch as a physical
    /// failure and latched manual-only after nine of them).
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

    /// The cached verdict's reason keyed by the ARTIFACT alone — for the
    /// submission-time failure arm, which has no ticket in hand (the attempt
    /// failed before any candidate was spawned) but knows the intent's build
    /// and digest. `Some(reason)` only for a fresh REFUSAL of that artifact.
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

    /// THE SUCCESSOR'S HANDOFF POLICY, as this park must follow it (the
    /// 2026-09-22/23 update audit, plan P0-5): the policy the pre-verification
    /// of exactly the artifact `attempt` targets read from that candidate's
    /// bundle, once it passed — THE one reading every park takes: the fork
    /// lane's, the launched lane's gate and capture, and the whole-App test seam
    /// ([`Self::park_desk_for_test`]), so no park can compute its own.
    /// [`ParkPolicy::policy`] is `None` — the park as this build ships — for the
    /// same-image QA seam (no attempt), for no verdict, for a refusal, and for a
    /// policy that asks this build for nothing; [`ParkPolicy::known`] says whether
    /// a pass for the artifact is on record at all, i.e. whether the policy has
    /// been read.
    ///
    /// NOT BOUND BY [`crate::HANDOFF_PREVERIFY_FRESHNESS`], deliberately. That
    /// window limits how long a verdict may stand in for re-running `codesign`
    /// against a disk that can change. The policy is not a verdict about the
    /// disk: it is a sealed resource of ONE release, named by its build, commit
    /// and artifact digest, so the entry that read it keeps saying what that
    /// release asks for. Requiring freshness would silently drop the policy
    /// whenever a busy terminal held an attempt past ten minutes after the
    /// stage was armed. The attempt itself is still authorized by the complete
    /// gate at swap time, whatever this returns.
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
        // STATE-KEYED (2026-09-19, the late park). Two records can describe one
        // attempt: the PARKED one (`pending_update_handoff`, readers stopped)
        // and the PRELAUNCHED one (`update_handoff_prelaunch`, a successor
        // launched and held). A completion rolls the readers back iff the
        // parked record exists — never by a worker-reported phase, because a
        // stand-down can land after the park — and clears both records here and
        // only here, after the worker's death proof, so a retry can never launch
        // beside a live candidate. The parked record's facts win when both
        // exist: it is the later and fuller of the two.
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
        // Lane classification for the bounded automatic retry budgets, from the two
        // typed facts this reduction still holds: the mode the apply was authorized
        // under, and the worker's outcome (plus the main thread's own
        // activity-shaped rejection flag, the other half of the activity
        // observation).
        //
        // THE MODE IS CARRIED, NOT DROPPED. It used to reach only this line and
        // then vanish, so a failure a PERSON asked for was charged to the
        // automatic budget — converging the background lane on human retries and
        // silencing the row for the human who asked.
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
        // classification of anything: `ChildDied` is the candidate ending before
        // its proof, which a successor that refused, one that faulted and one the
        // machine starved all produce identically. What the worker SAW at that instant travels here beside the
        // outcome rather than being re-inferred from a message string. See
        // [`crate::ChildDeathEvidence`].
        let lane = crate::app_native::HandoffFailureLane::classify(
            pending.mode,
            outcome,
            child_death,
            pending.revoked_by_activity,
        );
        let teardown = pending.teardown;
        // ROLLBACK IFF PARKED: a prelaunched attempt that never parked has
        // nothing to resume, and one whose readers were released ahead of the
        // stand-down (`Wake::UpdateHandoffStandingDown`) already resumed them.
        if let Some(live) = pending.parked_live.as_deref() {
            self.rollback_overlap(nonce.as_deref(), live);
        }
        // THE ATTEMPT RECORD IS GONE, and with it the fence that held draft
        // journals still: a re-seat the stand-down's rollback owed while the
        // prelaunch record stood, and the appends waiting behind it, run now
        // rather than at the person's next keystroke.
        self.drive_owed_document_journals();
        // THE ROW SAID "INSTALLING" FOR THIS ATTEMPT, and the attempt is over:
        // put the words (or the absence of a row) back and end the charging
        // surge BEFORE the outcome below decides whether to say anything — a
        // silent outcome (the automatic lane standing down on activity) must not
        // leave a frozen "Installing" on the bar.
        self.retire_update_installing();
        self.record_update_switch_stopped(
            matches!(lane, crate::app_native::HandoffFailureLane::ActivityRevoked),
            &detail,
        );
        // QA SEAM, READ BEFORE THE MATCH CONSUMES IT. A `None` ticket reaches this
        // reduction from exactly one place — `ATERM_DEBUG_SEAMLESS_REEXEC`, which
        // `start_native_update_handoff` is the only caller allowed to pair with a
        // missing attempt — so this outcome describes a SIMULATED apply of the
        // running binary, not a staged build that would not run. It must be logged
        // and shown, and it must not touch the durable ledger; the apply streak it
        // used to write is cleared only by a real successful apply, so QA runs
        // accrued forever and escalated to the persistent-failure notification.
        // WHICH same-image attempt this was, carried rather than inferred from the
        // absent ticket: the retired provenance repair carried no ticket either, and was
        // reported as a failed debug-seam UPDATE while the inference stood.
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
    /// * put each master back at the size its engine was parked at
    ///   ([`Self::reassert_parked_winsizes`]: the candidate may have resized it);
    /// * resume the parked readers ([`Self::attach_deferred_readers`] — parked
    ///   sessions are self-describing: `reader_join: None`);
    /// * owe every draft journal a re-seat: a candidate that got as far as its
    ///   restore republished the journal of each document it reopened, under
    ///   its own sequence numbers, and this process's next append would fail
    ///   against that image forever
    ///   ([`Self::reseat_document_journals_after_rollback`]).
    ///
    /// The event-loop half performs no filesystem I/O, update-status read, or
    /// child-process probe: the re-seats are queued to the document worker.
    #[cfg(unix)]
    pub(crate) fn rollback_overlap(&mut self, _nonce: Option<&str>, live: &[(u64, i32, i32)]) {
        for (_, master, _) in live {
            let _ = aterm_pty::set_cloexec(*master, true);
        }
        self.reassert_parked_winsizes(live);
        self.resume_deferred_readers_nonblocking();
        self.reseat_document_journals_after_rollback();
        // The cold restore's agents the park paused are this process's again
        // (round four, plan item 7) — and so is a relaunch on exit a worker's
        // back-off still owed when a Commit's stop took that worker (round
        // six, F13): no worker comes back for a tab at its shell.
        if let Some(host) = self.harness.clone() {
            host.resume_restored();
            host.relaunch_stranded(&|sid| self.upgrade_place(sid));
        }
    }
}

impl App {
    /// Put each handed PTY back at the size its engine was parked at (round
    /// six, finding 2). Before Commit the successor shares every handed PTY,
    /// and its adoption pulse and its own first layout resize them to ITS
    /// grids; a rejected Commit hands the programs back to engines still at
    /// the park's size, and this process's `resize_panes` skips a pane whose
    /// engine already matches its rect — so without this the program in a
    /// split pane kept the successor's size (the whole window's width) for
    /// the rest of the session. The kernel signals `SIGWINCH` only on a
    /// CHANGE, so a PTY the successor never touched is not disturbed.
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

/// What `lane` cannot carry of what the preflight's `token` let ride, as the
/// refusal the update row reads (the preflight's person-facing shapes), or
/// `None` when the lane carries all of it (plan P2-2).
///
/// * THE COLD LANE reopens nothing: it execs the new build over a desk with no
///   terminal session and no layout is handed over. Every carried editor
///   draft, Settings draft and failed checkpoint is refused — exactly the hold
///   the preflight itself placed before any of them rode.
/// * THE SEAMLESS LANE carries all of it to a successor at least this build.
///   An OLDER successor (a rollback hop, `target_build < running_build`)
///   parses the handoff layout but ignores the field that carries Settings
///   drafts — its layout struct denies no unknown field — so it would reopen
///   the view without them: refused. Editor drafts ride the draft journal,
///   not this field, so this gate leaves them as gap #32 left them. A hop to
///   an older build that does read the field is refused too: the build number
///   is all this process knows about its successor, and the refusal costs
///   only a wait.
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

/// The session registry's projection narrowed to the sessions this attempt
/// hands over — the ones in `live` ([`App::handoff_live_sessions`]).
///
/// The manifest, the descriptors and the screens must name the same sessions
/// (`seamless::write_outgoing` refuses the set otherwise), and the registry
/// still holds a pane kept open after its command exited, which the attempt
/// leaves out (the 2026-09-24 review). The connection rows are kept whole:
/// the successor re-mints only rows whose two endpoints it adopted and drops
/// the rest, audited, as it always has.
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

/// Stamp each record with its session's foreground holder
/// (`SessionRecord::fg_holder`), as the park capture read it
/// ([`ParkedScreens::fg_holders`]).
///
/// THE HOLDER IS A RECORD SCALAR (the 2026-09-25 foreground handback review).
/// It used to ride the control-carry sidecar, which is dropped for a session
/// carried at the Sanitized or Repaint rung, for one the receiver repaints, and
/// for one past the 16 MiB aggregate sidecar budget (about the fifth busy
/// session on). Every one of those sessions still restores its modes, so a job
/// that died while the readers were parked left its alt screen, mouse and
/// kitty modes armed under the prompt: the adopting reader got holder `0`,
/// probed afresh, saw the reclaimed shell, and never cut. A holder that is no
/// pgid (`<= 0`, nothing seen yet) is not written.
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

/// Block (bounded) until the overlap-handoff child signals readiness, closes its
/// proof pipe, or times out. Crucially this never calls `Child::try_wait`: that
/// API reaps an exited group leader and destroys the PID identity before the
/// unique reaper can signal all descendants. Pre-ready death is proof EOF, or
/// the candidate's own end asked WITHOUT reaping (`candidate_gone`), because a
/// stray copy of the write end can hold EOF back; after a full proof the
/// decision loop asks the same question, and an exit that slips past it is
/// rejected by the atomic Commit pipe write (EPIPE). The deadline defaults to 30 s — generous against a
/// staged-swap re-exec + GPU init + multi-window present (~1-2 s observed) —
/// and is tunable via `ATERM_HANDOFF_READY_TIMEOUT_MS`, a DEVELOPMENT SEAM
/// (`aterm_types::dev_seam!` — a shipped binary does not read it).
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

/// The facts the park gate reads, in the shape a test can enumerate.
///
/// Every one of them is measured on the main thread immediately before the gate
/// runs; none is remembered from the attempt's entry, because the entry is now
/// seconds or minutes earlier — the successor was launched there and has been
/// booting and holding since.
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
    /// `!handoff_masters_closed`: no handed master has hung up. A dead one is
    /// not output (plan P1-3) — it is a session the adoption proof would commit
    /// to after it died, which the post-park re-check refuses anyway — so the
    /// gate waits rather than freezing the terminal to find that out.
    ///
    /// BOUNDED BY CONSTRUCTION since the 2026-09-24 review: a pane kept open
    /// after its command exited is not a HANDED master at all
    /// (`App::handoff_live_sessions` leaves it out, re-read at every gate run),
    /// so this is false only for the instant between a command's exit and the
    /// registry marking it — one 20 ms re-run. It used to be false for as long
    /// as an `aterm --hold` pane stayed open, which held every automatic
    /// attempt to its cap, forever.
    pub(crate) masters_alive: bool,
    /// How many times in a row this attempt's gate has answered `Wait` in the
    /// `Land` phase — which, the ladder admitting everything there, can only be
    /// the masters holding it. Past [`PRELAUNCH_LAND_MAX_WAITS`] the park
    /// proceeds over queued output anyway (plan P1-3); never over a dead one.
    pub(crate) land_waits: u8,
    /// The successor's signed handoff policy says `park_quiet_gate_at_land =
    /// "relaxed"` (the 2026-09-22/23 update audit, plan P0-5): at `Land` the park
    /// proceeds over queued output at once, as it would after
    /// [`PRELAUNCH_LAND_MAX_WAITS`] waits. The same relaxation, due sooner —
    /// never over a dead master, never in an earlier phase, and the capture's
    /// mid-sequence check still answers a parser parked inside a sequence.
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

/// WHEN THE TERMINAL MAY BE FROZEN, now that freezing it is a separate decision
/// from starting the update (2026-09-19, the late park).
///
/// Before this, one gate at the attempt's ENTRY decided both, because the park
/// was the first thing an attempt did. The entry gate still stands and still
/// decides whether an attempt starts at all — `AutomaticAttemptRequiresQuiet\
/// OrClosedGraceWindow` binds the launch through it — but the launch costs the
/// user nothing: every reader stays live through the successor's swap, second
/// start and boot check. This gate decides the only thing the user feels.
///
/// So the rule is the one the ENTRY gate applied, re-read at the instant it
/// matters:
///
/// * `Immediate` — the person asked for this. Park at once; they
///   are waiting for it, and a gate here would be aterm deciding it knows better.
/// * the automatic modes — THE LADDER
///   (`native_update_auto_intent::automatic_park_refusal`), read at its live
///   phase: a quiet moment while the lane prefers idle, then a gap in output
///   while an aterm window is focused, then a gap in typing, then nothing. A
///   successor launched under `Automatic` and held into a later phase parks by
///   that later phase's rule; the mode only says which entry admitted it.
///
/// Plus one fact the ladder relaxes only at its bound: bytes waiting on a live
/// master that its reader has not taken. The park prefers a clean instant —
/// the reader takes bytes within microseconds, so the 20 ms re-run usually
/// finds one — but it is a PREFERENCE, not a correctness gate, and at `Land`
/// it gives way after [`PRELAUNCH_LAND_MAX_WAITS`] consecutive waits (the
/// 2026-09-22/23 update audit, plan P1-3). A producer faster than the parser
/// (`yes`, a build log, reader backpressure) keeps `POLLIN` set indefinitely,
/// and the gate that "never relaxed" held every automatic attempt its full
/// 120 s and stood it down, for as long as the job ran. Parking over those
/// bytes loses nothing AT A SEQUENCE BOUNDARY: the screen carry is captured
/// post-park from what the engine already has, the parent consumes no byte
/// after the park, and the successor's fresh parser replays everything still
/// queued in the kernel after Commit (`HandoffCommitFacts`' admission
/// contract). A reader parked INSIDE a sequence is the exception, and this
/// gate cannot see it: the successor's Ground parser would print the queued
/// rest of that sequence as text. So the capture checks each session's parser
/// under the park and answers a mid-sequence parser over queued output with a
/// timing miss (`CaptureFailure::MidSequence`), re-parked at the next 20 ms
/// re-run on the same rung ([`mid_sequence_miss_disposition`]), then on the
/// next rung —
/// it abandons a partial sequence only over a quiet PTY, the stalled case
/// (the 2026-09-24 review; this sentence used to say "loses nothing" with no
/// such check behind it).
///
/// The successor's signed handoff policy can bring that `Land` bound forward to
/// the first wait (`park_quiet_gate_at_land = "relaxed"`, plan P0-5,
/// [`ParkGateFacts::land_gate_relaxed`]) — the same relaxation, sooner, for a
/// release whose predecessors' gate is known to hold too long. Nothing else here
/// is the policy's to change.
///
/// MONOTONE IN `held_for`: past the cap every automatic mode stands down, and
/// nothing can make a stood-down gate admit again. That is what bounds the hold
/// — a booted successor is not left waiting behind a terminal that never goes
/// quiet; the ladder's next phase admits the next attempt sooner.
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
/// verb's own maximum duration ([`crate::control::control_media::VIDEO_MAX_DURATION`]),
/// so a take already running when the successor dialled can finish and answer
/// its client with the recording it asked for.
#[cfg(unix)]
pub(crate) const VIDEO_PARK_WAIT_MAX: std::time::Duration =
    crate::control::control_media::VIDEO_MAX_DURATION;

#[cfg(unix)]
const _: () = assert!(
    VIDEO_PARK_WAIT_MAX.as_secs() + 10 < PRELAUNCH_HOLD_MAX.as_secs(),
    "a recording's wait must come due well inside the hold, or a take stands the successor \
     down instead of being answered"
);

/// Whether a live `video` take holds this park (round five, item 16).
///
/// A take running when the readers park has no future once the update
/// commits: Commit ABORTS it, with a reply it waits to see written, before its
/// `_exit` ([`crate::App::video_answer_before_commit`]). Before that, the automatic
/// lanes give the take a chance to finish — in every phase but `Land`, and on
/// the launched lane for at most [`VIDEO_PARK_WAIT_MAX`] of the hold. Both
/// bounds end: the ladder reaches `Land` within
/// [`crate::native_update_auto_intent::LANDS_WITHIN`] of being armed, and the
/// hold's clock only runs forward. So this is a typed wait that always comes
/// due, never a pin; an explicit apply never waits for it at all.
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

/// The detail a hold-cap stand-down carries into the completion and the
/// ledger: the cap's reason, how long the successor was held, and — the part
/// that says what to do about it — the last thing the gate was waiting FOR.
///
/// Without that last clause every expired hold read "the terminal never
/// offered a moment to pause in", which files a pane kept open after its
/// command exited, or a flooded master, as the user being busy; the real
/// reason reached only a debug log (the 2026-09-24 review of the 2026-09-22/23
/// update audit fixes).
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

/// How many consecutive `Wait` answers the gate gives in the `Land` phase before
/// it parks over output still queued on a master (the 2026-09-22/23 update
/// audit, plan P1-3). At the 20 ms re-run that is one second: long enough for
/// any reader that is merely behind to catch up — it takes bytes within
/// microseconds — and short enough that a master a producer keeps permanently
/// readable costs the landing one second rather than the whole 120 s hold and a
/// stand-down every fifteen minutes.
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
    /// The park itself missed: the readers were parked and rolled back, nothing
    /// was granted, and the successor is still holding. Re-park once, after
    /// [`PRELAUNCH_REPARK_DELAY`] and on the ladder's NEXT freeze-budget rung.
    ///
    /// TIMING ONLY (the 2026-09-22/23 update audit, plan P0-3): a reader that
    /// missed the park deadline, a capture that outran it, an engine lock the
    /// capture could not take, storage this process could not reserve, a
    /// reader that parked in the middle of an escape sequence whose rest is
    /// still queued (`CaptureFailure::MidSequence`). Every
    /// one of those can come out differently 500 ms later, which is the whole
    /// premise of re-parking — and exactly why a deterministic refusal must
    /// never be one of them.
    Missed(String),
    /// The launched lane's park missed ONLY because a parser was caught
    /// mid-sequence with the rest of that sequence still queued
    /// (`CaptureFailure::MidSequence`): not a budget the machine missed, so it
    /// re-parks at the gate's 20 ms cadence without climbing the rung, for up to
    /// [`PRELAUNCH_MAX_MID_SEQUENCE_REPARKS`] ([`mid_sequence_miss_disposition`]);
    /// past that bound it is an ordinary [`Self::Missed`]. The fork lane never
    /// sees it: its capture's classifier files the same failure as a miss.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    MissedMidSequence(String),
    /// The capture REFUSED the desk: a fact about the screens, the session set
    /// or the layout that the next rung cannot change. Not re-parked (the
    /// freeze ladder buys time, and time is not what refused), never filed as
    /// the machine being busy: the successor stands down as
    /// [`crate::UpdateHandoffOutcome::CaptureRefused`], which the refusal lane
    /// records and retries when the desk changes.
    Refused(CaptureRefusal),
    /// Stand the held successor down with this outcome; readers (if they were
    /// parked) have already been rolled back.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    Failed(HandoffStandDown),
}

/// A deterministic refusal of the park's capture, with the session it names
/// when it names one — so the stand-down, the ledger and `update status` can
/// say WHICH session held the update back (2026-09-22/23 audit, plan P0-3).
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

/// WHICH LANE A CAPTURE FAILURE BELONGS TO — the one decision the 2026-09-22/23
/// update audit found made wrongly on every hop since 0.87 (plan P0-3).
///
/// Timing (`Deadline`, `EngineBusy`, and `MidSequence` — a reader parked inside
/// a flood, whose next park lands on a sequence boundary) and this process's
/// own storage (`Storage`) are a MISS: the same desk can capture 500 ms later
/// on a wider rung. A
/// `Refused` is a fact about the desk that no rung changes, and it is its own
/// attempt: the park never climbs the freeze ladder for it and never stands
/// down as `ActivityRevoked` ("the machine is busy, the candidate is not in
/// question") for it. v0.91 mapped EVERY capture `Err` to `Missed` at its one
/// call site, so a NUL in one tab's reported cwd relaunched a successor every
/// fifteen minutes, forever, with `failing_applies=0`. Typed on the variant,
/// never on the message.
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

/// THE TIER-1 SEAM for the capture's lane decision (the 2026-09-22/23 update
/// audit, plan P0-3): run the shipping park capture over the whole pool, with
/// a deadline generous enough that a loaded test machine cannot turn the answer
/// into timing, and answer what the park would do with the result — the repainted
/// session ids of a parked capture, or the [`ParkAttempt`] a failure classifies
/// into. `native_updater_conformance` drives real refused desks through it.
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

/// What one park hands the worker, projected from a whole [`App`] exactly as
/// the fork lane projects it: the handed set, the registry manifest over it,
/// the window carry, the capture, the history carry's heads and plan, and the
/// post-park layout with its digest. The PTY descriptors are the one thing
/// left to the caller — the lanes duplicate the masters, and a test that plays
/// the successor in the same process owns those duplicates itself.
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
    /// The held panes' screens the capture carried (round five, item 19).
    pub(crate) held: Vec<crate::seamless::HeldScreen>,
}

/// THE WHOLE-APP SEAM (the 2026-09-22/23 update audit, plan P1-6): the fork
/// lane's park, from the registry projection to the post-park layout, over
/// this App's real pool — every window, tab and split — with the successor's
/// caps for `target_build` and a deadline wide enough that a loaded test
/// machine cannot turn the answer into timing. Its other test callers drive
/// `capture_parked_screens` over one or two hand-built sessions;
/// `seamless::app_capture_tests` drives this through the manifest writer and
/// the successor's reader.
#[cfg(all(test, unix))]
impl App {
    /// The park the impl's doc describes. `attempt` is the attempt's ticket:
    /// the successor's handoff policy is read for it by [`Self::park_policy`] —
    /// the very call both lanes make at their park — and its carry ceiling
    /// bounds the capture (plan P0-5). `None` is the same-image seam: no policy.
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
        let window = self.handoff_window_carry(target_build);
        let budget = std::time::Duration::from_secs(30);
        let ParkedScreens {
            screens,
            carries,
            screen_digest,
            repaint,
            fg_holders,
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

/// The stand-down a [`ParkAttempt::Refused`] park sends the held successor:
/// typed `CaptureRefused`, with a detail that names the session, so the
/// completion reaches the refusal lane and the ledger says what held the update
/// back.
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

/// How many times one prelaunched attempt re-parks before it stands its
/// successor down. TWO: the whole freeze ladder (20 ms -> 80 ms -> 250 ms,
/// [`handoff_freeze_budget`]) is climbed INSIDE one attempt, 500 ms apart, with
/// the successor still booted and holding — three parks cost the user at most
/// 350 ms of stalled echo across a second, against a whole relaunch per rung.
///
/// It used to be ONE, with the second miss handed to "the ledger's own rung
/// widening": the stand-down was filed as `PreparationFailed`, which
/// [`crate::app_native::PhysicalFailureShape::of_outcome`] charges as
/// STRUCTURAL — a verdict about the BYTES, with a two-attempt lifetime. Two
/// busy moments ten minutes apart therefore converged the automatic lane to a
/// deadline-less manual-only latch for the artifact (2026-09-21, the second
/// finding of the ladder audit). A miss is the parent's scheduling failure and
/// says nothing about the candidate, so the ledger is exactly the wrong place
/// for it; the attempt climbs the rungs itself and, past the last one, stands
/// down as a busy-machine fact ([`park_miss_disposition`]).
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

/// How many mid-sequence misses one prelaunched attempt re-parks on for free
/// (2026-09-24, review of the Land relaxation). Past
/// [`PRELAUNCH_LAND_MAX_WAITS`] `Land` parks in the middle of a flood, and a
/// reader stops at a batch boundary that can fall inside an escape sequence;
/// charged as an ordinary miss, two of those stood the successor down and the
/// update landed only by luck across ladder retries. A flood's next boundary is
/// somewhere else, so these re-park at the 20 ms cadence without climbing the
/// freeze rung. Bounded, so a parser that stays mid-sequence over a PTY that
/// stays busy falls through to the ordinary ladder instead of parking forever;
/// the bound's whole cost fits in a second of the hold.
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) const PRELAUNCH_MAX_MID_SEQUENCE_REPARKS: u8 = 32;

/// [`aterm_core::terminal::Terminal::output_state_uncarried`]'s name for an
/// OSC 8 link still open — the one uncarried state whose miss is soft
/// ([`App::open_link_may_repark`]).
#[cfg(unix)]
const OPEN_LINK_STATE: &str = "Osc8HyperlinkOpen";

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

/// A PARK MISS IS A FACT ABOUT THE MACHINE'S MOMENT, NEVER ABOUT THE BYTES.
///
/// The readers came back, nothing was granted, no artifact was written; the only
/// thing that happened is that a busy machine did not stop its readers (or
/// capture its screens) inside a comfort budget. That is the same kind of fact
/// as a keystroke or the prelaunch hold cap — a scheduling fact about this
/// terminal — so past the last rung the successor stands down as
/// `ActivityRevoked`: retried on the activity spacing, the ladder's anchor
/// untouched, no physical budget spent, never a manual-only latch. The design's
/// law (docs/DESIGN-auto-apply-ladder-2026-09-21.md, law 2) reserves the latch
/// for a successor that died or a proof that did not match; a stopwatch the
/// parent missed is neither.
///
/// ONLY A MISS COMES HERE (the 2026-09-22/23 update audit, plan P0-3). Until
/// then every capture `Err` did — a NUL in a reported cwd, a grid over the
/// per-grid cap — so "the machine is busy" was said, re-parked and retried
/// every fifteen minutes about facts no amount of waiting changes. A refusal is
/// now [`ParkAttempt::Refused`] and never reaches this function, which is what
/// makes the sentence below true.
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

/// How long Commit waits, at most, for every live `video` request it answers
/// to reach its client before the `_exit` — an export's encode worker to see
/// its cancellation, answer and remove its unpublished directory, and each
/// reply to be written on its control connection (round six of the update
/// audit, finding 31; [`crate::App::video_answer_before_commit`]). An encode
/// worker looks at its cancellation between frames — one PNG encode and write
/// — and a reply write is one small socket write, so this is spent only on a
/// wedged worker or client.
#[cfg(unix)]
pub(crate) const VIDEO_EXPORT_COMMIT_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

/// How long a dialled successor is held for a quiet moment before the attempt
/// stands down (activity-revoked: no physical budget spent, retried later).
/// Longer than the whole ladder ([`crate::native_update_auto_intent::LANDS_WITHIN`])
/// with room to spare, so a successor launched in the ladder's first phase and
/// held through a busy stretch reaches `Land` and parks there instead of
/// standing down onto the retry spacing. Only a hold the ladder never relaxes
/// (the user's consent warm-up) can outlast it.
#[cfg(unix)]
pub(crate) const PRELAUNCH_HOLD_MAX: std::time::Duration = std::time::Duration::from_secs(120);

#[cfg(unix)]
const _: () = assert!(
    crate::native_update_auto_intent::LANDS_WITHIN.as_secs() + 10 < PRELAUNCH_HOLD_MAX.as_secs()
);

/// When the POST-PARK proof wait gives up on the launched lane (2026-09-19).
///
/// Under the late park everything slow — the launch, the bundle swap, the
/// second `execve`, the boot check — has already happened when the readers
/// park, so what this bounds is the REST of the freeze: the artifacts this
/// attempt still has to write (`prepare_outgoing_artifacts` — the control
/// carry's export, the manifest, a grid sidecar per session, the layout), the
/// one `sendmsg`, and the successor's GUI boot to its first present (~180 ms
/// measured idle, and the dominant term). Anchoring it at the park rather than
/// at the grant is deliberate: what must be bounded is the interval the user's
/// terminal is frozen, not the part of it this process finds convenient. A missed deadline is a kill, a
/// proof of death and a resumed terminal, charged to the physical retry ladder
/// like every other missed proof, so the ladder widens it: 3 s on a first
/// automatic attempt, 6 s after a physical failure of the same bytes, 30 s for
/// an explicit apply (the person asked, and a busy machine may take it).
/// `ATERM_HANDOFF_PROOF_TIMEOUT_MS` overrides all three — a DEVELOPMENT SEAM
/// (`aterm_types::dev_seam!`): a shipped binary does not read it.
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
    let timeout_ms = aterm_types::dev_seam!("ATERM_HANDOFF_READY_TIMEOUT_MS")
        .and_then(|v| v.to_str()?.parse::<u64>().ok())
        .unwrap_or(30_000)
        .clamp(1_000, 120_000);
    std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms)
}

/// LaunchServices' still-pending answer to the launch of the dialer the parent
/// already accepted on the kernel-attested rendezvous (2026-09-19): polled with a
/// ZERO budget on every slice of [`wait_handoff_ready`], so the corroboration
/// never holds the parked terminal, and a mismatch — LaunchServices naming a
/// different pid than the dialer, i.e. a stranger that knew the claim secret —
/// still refuses the handoff before Commit. The descriptors are the peer's by
/// then, so the refusal is a kill-and-reap like every other post-transfer
/// rejection, never a `NeverTransferred`.
#[cfg(target_os = "macos")]
struct LaunchCorroboration<'a> {
    in_flight: &'a crate::app_launch_successor::LaunchInFlight,
    dialer_pid: i32,
    /// Set once LaunchServices has spoken (a match, or an error about a job that
    /// evidently ran): the answer is consumed by the first `wait` that sees it,
    /// so it is never polled for twice.
    answered: bool,
}

#[cfg(target_os = "macos")]
impl LaunchCorroboration<'_> {
    /// One zero-budget poll. `false` means the handoff must be REFUSED: the
    /// launch answer names a pid other than the dialer's.
    fn poll(&mut self) -> bool {
        if self.answered {
            return true;
        }
        match self.in_flight.wait(std::time::Duration::ZERO) {
            Ok(successor) if successor.pid() == self.dialer_pid => {
                self.answered = true;
                aterm_log::info!(
                    "update apply: LaunchServices corroborates the dialer as pid {} (answered \
                     beside the proof wait)",
                    self.dialer_pid
                );
                true
            }
            Ok(successor) => {
                self.answered = true;
                aterm_log::warn!(
                    "update apply: LaunchServices names pid {} as the successor it launched; \
                     the dialer is pid {} — refusing the handoff",
                    successor.pid(),
                    self.dialer_pid
                );
                false
            }
            Err(crate::app_launch_successor::LaunchError::Timeout(_)) => true,
            Err(error) => {
                self.answered = true;
                aterm_log::warn!(
                    "update apply: LaunchServices answered the launch with an error after the \
                     successor had already dialled ({error}); proceeding on the attested dialer"
                );
                true
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
        #[cfg(target_os = "macos")]
        if let Some(corroboration) = corroboration.as_mut()
            && !corroboration.poll()
        {
            return crate::UpdateHandoffOutcome::Rejected;
        }
        // THE CANDIDATE'S OWN END, asked every slice rather than inferred from EOF
        // alone: a stray copy of the write end outlives the candidate and holds
        // EOF back, which used to park the terminal until the deadline
        // ([`HandoffCandidateHandle::candidate_gone`]). Nothing has touched the
        // candidate, so this is the same event EOF reports below. After the
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
            // (`handoff_rejection_activity_shaped`, round six, finding 37).
            ReadyPollAction::SessionDied => return crate::UpdateHandoffOutcome::ActivityRevoked,
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
            // EOF: every copy of the write end is closed, the child's included, so
            // it dropped the fd (a failed validation) or died/malformed mid-proof.
            0 => return crate::UpdateHandoffOutcome::ChildDied,
            _ => continue, // EINTR/EAGAIN — re-loop
        }
    }
}

/// THE LATE PARK'S STAND-DOWN, driven against real processes and the real
/// rendezvous (2026-09-19). A held successor holds nothing, so standing it down
/// is: close the rendezvous (EOF), give it the grace to exit by its own rule,
/// kill it if it does not, and prove it gone either way — and say which of the
/// two happened, because exactly one party forgives the counted trial launch.
#[cfg(all(test, target_os = "macos"))]
mod held_successor_stand_down_tests {
    use super::{
        CandidateExitWatch, HandoffCandidate, HandoffRollbackWarrant, HeldSuccessor, StandDownStep,
        StoodDownSuccessor,
    };
    use aterm_spec::derive::{Model, native_update_overlap_handoff_model};
    use aterm_spec::interp::{State, with_buggy};
    use std::io::Read as _;

    /// Bind a rendezvous, dial it from a test thread, accept and HOLD the claim,
    /// and spawn `script` as the process the held claim stands for. `None` when
    /// this machine has no private control dir (then no rendezvous binds at all).
    fn hold(script: &str) -> Option<(HeldSuccessor, std::process::Child, aterm_uds::CtlStream)> {
        hold_with_stdin(script, std::process::Stdio::null())
    }

    /// [`hold`], with the stand-in's stdin chosen by the caller: a PIPE lets a
    /// test decide the instant the stand-in exits (`read` returns on EOF), which
    /// is what makes an interleaving forced rather than hoped for.
    fn hold_with_stdin(
        script: &str,
        stdin: std::process::Stdio,
    ) -> Option<(HeldSuccessor, std::process::Child, aterm_uds::CtlStream)> {
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
        let peer = rendezvous
            .accept_claim(
                None,
                std::time::Instant::now() + std::time::Duration::from_secs(10),
                &|| false,
            )
            .expect("the test dialer presents the claim");
        let stream = dialer.join().expect("dialer thread");
        let child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .stdin(stdin)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the stand-in candidate");
        let candidate = HandoffCandidate::of_unreaped_child(&child);
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

    /// The candidate is our fork child here (the launched lane's is launchd's),
    /// so somebody must reap it or its pid never becomes vacant and the
    /// termination proof cannot complete — launchd does that for the real one.
    fn reap_in_background(
        mut child: std::process::Child,
    ) -> std::thread::JoinHandle<std::process::ExitStatus> {
        std::thread::spawn(move || child.wait().expect("reap the stand-in"))
    }

    /// THE PROJECTION. Replay the steps the REAL stand-down emitted against the
    /// derived overlap model, from the state a launched-but-ungranted successor
    /// is in, and require each one to be an enabled action there. This is what
    /// makes the model a claim about this file: a stand-down that skipped the
    /// kill, or retired before the reap, would produce a step sequence with no
    /// admitted trace and fail here.
    fn project_onto_the_model(model: &Model, stood: &StoodDownSuccessor) -> State {
        let mut state = model.init_state();
        // The lane this stand-down belongs to: launched before the park, nothing
        // granted, and the main thread's stand-down announcement already
        // answered (`answer_prelaunched_stand_down`, which is what the worker
        // waits for before the first step below).
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

    /// The negative control that makes the projection above a claim: retiring
    /// while the successor is still live is executable ONLY in the mutant, and
    /// the invariant the projection checks is what catches it.
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
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        matches!((&*stream).read(&mut byte), Ok(0))
    }

    /// (a) A successor that exits on the EOF by its own rule, inside the grace:
    /// proven gone WITHOUT a signal, its own exit status witnessed, and
    /// `signalled == false` — so the parent does NOT also forgive the trial
    /// launch the successor already forgave.
    #[test]
    fn a_successor_that_exits_on_eof_is_proven_gone_without_a_signal() {
        let Some((held, child, stream)) = hold("sleep 0.2") else {
            return;
        };
        let reaper = reap_in_background(child);
        let stood = super::stand_down_held_successor(held, std::time::Duration::from_secs(10));
        assert!(!stood.signalled, "an EOF exit needs no kill");
        assert_eq!(stood.warrant, HandoffRollbackWarrant::Vanished);
        assert_eq!(
            stood.death,
            crate::ChildDeathEvidence::Exited { code: 0 },
            "the witness records the candidate's OWN exit"
        );
        assert!(reads_eof(&stream), "the dialer read the stand-down as EOF");
        assert_eq!(reaper.join().expect("reaper").code(), Some(0));
        // THE ORDER IT TOOK, against the model: revoke, its own exit, the proof.
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
        assert_eq!(
            retired["group_signaled"], 0,
            "nothing of ours signalled it, which is why the PARENT forgives no \
             trial launch here"
        );
        assert_eq!(retired["successor_exited"], 1);
        assert_a_live_successor_cannot_retire(&model);
    }

    /// A stand-in that exits `0` exactly when the test closes its stdin, and
    /// never before: the losing interleavings below are FORCED, not waited for.
    const EXIT_ON_STDIN_EOF: &str = "read line; exit 0";

    /// (a′) THE LOSING INTERLEAVING, FORCED. The loop reads the witness (nothing
    /// yet: the stand-in is blocked on a stdin the test holds), and BEFORE its
    /// pid probe the stand-in exits and is reaped — so the probe proves it gone
    /// with the witness unread. That is the window the 2026-09-27 loaded runs
    /// lost one exit in ten to (`Unobserved` where `Exited { code: 0 }` was
    /// owed), and the stand-down must still name the candidate's own exit: XNU
    /// queued the knote in `proc_exit`, before the pid could fall vacant.
    #[test]
    fn the_exit_between_the_witness_read_and_the_pid_probe_is_still_witnessed() {
        let Some((held, mut child, stream)) =
            hold_with_stdin(EXIT_ON_STDIN_EOF, std::process::Stdio::piped())
        else {
            return;
        };
        let mut stdin = Some(child.stdin.take().expect("piped stdin"));
        let mut reaper = Some(reap_in_background(child));
        let mut reaped = None;
        let mut misses = 0_u32;
        let stood = super::stand_down_held_successor_between(
            held,
            std::time::Duration::from_secs(10),
            &mut || {
                misses += 1;
                // Only the FIRST miss exits it; a second one would mean the probe
                // failed to see a reaped pid as gone.
                if let Some(stdin) = stdin.take() {
                    drop(stdin);
                    reaped = Some(reaper.take().expect("one reaper").join().expect("reaper"));
                }
            },
        );
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

    /// (a″) THE OTHER WAY THE SAME EXIT WAS LOST, FORCED. The hold loop's "did it
    /// exit?" check (`hold_for_park`) READS the witness, and the knote is one-shot:
    /// the stand-down that follows reads the same queue again. The candidate has
    /// exited and been reaped before either read, so nothing about timing is left.
    #[test]
    fn a_witness_the_hold_loop_already_read_still_names_the_exit_at_the_stand_down() {
        let Some((held, mut child, stream)) =
            hold_with_stdin(EXIT_ON_STDIN_EOF, std::process::Stdio::piped())
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
        let stood = super::stand_down_held_successor_between(
            held,
            std::time::Duration::from_secs(10),
            &mut || misses += 1,
        );
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

    /// (b) A successor that ignores the EOF for the whole grace is killed and
    /// proven gone; the kill is our own, so the death is `Unobserved` and
    /// `signalled == true` is what licenses the parent's forgiveness.
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
        // THE ORDER IT TOOK, against the model: revoke, kill, the proof — the
        // order `UngrantedRetireRequiresReap` exists to enforce.
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
}

/// The late park's attempt records and the completion reducer (2026-09-19).
#[cfg(all(test, unix))]
mod late_park_record_tests {
    use crate::App;
    use crate::native_updater_service::ApplyMode;

    pub(super) fn prelaunch_record(
        attempt_id: u64,
        mode: ApplyMode,
    ) -> (
        crate::HandoffPrelaunch,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Receiver<super::HandoffStandDown>,
        std::sync::mpsc::Receiver<super::HandoffTransferJob>,
    ) {
        let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
        let (stand_down, stood_down) = std::sync::mpsc::sync_channel(1);
        let (transfer, transferred) = std::sync::mpsc::sync_channel(1);
        (
            crate::HandoffPrelaunch {
                stand_down_ack: std::sync::mpsc::sync_channel(1).0,
                attempt_id,
                nonce: "0123456789abcdef0123456789abcdef".to_string(),
                mode,
                apply_attempt: None,
                same_image: Some(super::SameImageHandoff::DebugSeam),
                target_build: crate::build_info::BUILD_NUMBER.parse().unwrap_or(0),
                target_commit: crate::build_info::GIT_COMMIT.to_string(),
                cancel,
                stand_down,
                transfer,
                arbiter: crate::HandoffAttemptArbiter::new(),
                launched_at: std::time::Instant::now(),
                dialled: None,
                park_retry_at: None,
                park_misses: 0,
                park_mid_sequence_reparks: 0,
                freeze_seed: super::FreezeSeed::Default,
                land_waits: 0,
                last_wait: None,
                stood_down: false,
                teardown: crate::DeferredHandoffTeardown::None,
                revoked_by_activity: false,
                history_export: None,
            },
            cancelled,
            stood_down,
            transferred,
        )
    }

    fn parked_record(attempt_id: u64, mode: ApplyMode) -> crate::PendingUpdateHandoff {
        let (cancel, _cancelled) = std::sync::mpsc::sync_channel(1);
        crate::PendingUpdateHandoff {
            park_at: std::time::Instant::now(),
            proof_ready_at: None,
            #[cfg(target_os = "macos")]
            activate_at_commit: false,
            attempt_id,
            nonce: None,
            live: Vec::new(),
            adoption: Vec::new(),
            child_pid: None,
            mode,
            apply_attempt: None,
            same_image: Some(super::SameImageHandoff::DebugSeam),
            target_build: 0,
            target_commit: String::new(),
            layout: crate::restore::RestoreManifest::new(Vec::new()),
            layout_digest: [0; 32],
            screen_digest: [0; 32],
            activity_epoch: 0,
            hold_serials: 0,
            cancel,
            arbiter: crate::HandoffAttemptArbiter::new(),
            teardown: crate::DeferredHandoffTeardown::None,
            commit_drain_started: None,
            revoked_by_activity: false,
        }
    }

    fn completion(attempt_id: u64) -> crate::UpdateHandoffCompletion {
        crate::UpdateHandoffCompletion {
            attempt_id,
            nonce: None,
            child_pid: None,
            outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
            commit_fd: None,
            reject: None,
            reconcile: None,
            detail: "stood down".to_string(),
            input_drain_spins: 0,
            child_death: crate::ChildDeathEvidence::Unobserved,
        }
    }

    /// A completion for an attempt that only ever PRELAUNCHED clears that
    /// record, so a retry may launch.
    #[test]
    fn a_prelaunched_attempt_is_cleared_by_its_completion() {
        let mut app = App::headless_for_test();
        let (record, _cancelled, _stood_down, _transferred) =
            prelaunch_record(9, ApplyMode::Immediate);
        app.update_handoff_prelaunch = Some(record);
        assert!(app.update_handoff_in_flight());
        let teardown = app
            .reduce_returned_handoff_completion(completion(9))
            .expect("the prelaunched attempt is reduced");
        assert_eq!(teardown, crate::DeferredHandoffTeardown::None);
        assert!(app.update_handoff_prelaunch.is_none());
        assert!(app.pending_update_handoff.is_none());
        assert!(!app.update_handoff_in_flight());
    }

    /// A HEALTH WARNING IS NOT THE LIVE ATTEMPT'S REFUSAL (round six, finding
    /// 44): one that lands while a prelaunched attempt is still in flight
    /// leaves the Installing row and the rim standing; the attempt's own
    /// completion is what retires them when it does not take over.
    #[test]
    fn a_health_warning_during_an_attempt_leaves_its_installing_row_and_rim() {
        use crate::messages_host::FlowPhase;
        let mut app = App::headless_for_test();
        app.begin_update_installing(41, true);
        let id = app
            .messages
            .live_by_key(crate::update_words::KEY_PROGRESS)
            .expect("the explicit apply's row")
            .id;
        let (record, _cancelled, _stood_down, _transferred) =
            prelaunch_record(9, ApplyMode::Immediate);
        app.update_handoff_prelaunch = Some(record);
        assert!(app.note_update_health(
            aterm_update::health_failing_title("manifest"),
            "3 failed checks in a row since 2026-09-28T00:00:00Z: x",
        ));
        assert!(app.messages.live(id).is_some(), "the install is in flight");
        assert!(matches!(
            app.live_update_flow().map(|f| f.phase),
            Some(FlowPhase::Installing)
        ));
        assert!(app.level_up.is_some(), "the rim still explains the freeze");
        // The attempt ends without taking over: ITS completion retires both.
        app.reduce_returned_handoff_completion(completion(9));
        assert!(app.messages.live(id).is_none());
        assert!(app.level_up.is_none());
    }

    /// A FLOOD'S MID-SEQUENCE PARK NEVER STANDS THE SUCCESSOR DOWN (2026-09-24,
    /// review of the Land relaxation). Driven through the production miss
    /// handler: a capture that caught a parser mid-sequence re-parks at the 20 ms
    /// cadence, spends no rung and keeps the successor holding, for the whole
    /// mid-sequence bound; only past it does the ordinary ladder take over — and
    /// even that re-parks before it stands down, so a parser that STAYS
    /// mid-sequence still ends in the bounded stand-down, never an endless loop.
    #[test]
    fn a_mid_sequence_park_miss_re_parks_on_the_same_rung_without_standing_down() {
        let mut app = App::headless_for_test();
        let (record, _cancelled, stood_down, _transferred) =
            prelaunch_record(9, ApplyMode::AutomaticPastGrace);
        app.update_handoff_prelaunch = Some(record);
        let now = std::time::Instant::now();
        let reason = || {
            "session 1's parser was parked mid-sequence (csi-param) with the rest of that \
             sequence still queued on its PTY"
                .to_string()
        };
        for n in 1..=super::PRELAUNCH_MAX_MID_SEQUENCE_REPARKS {
            let (misses, reparks) = app
                .update_handoff_prelaunch
                .as_ref()
                .map(|p| (p.park_misses, p.park_mid_sequence_reparks))
                .expect("still prelaunched");
            app.dispose_park_miss(
                now,
                super::mid_sequence_miss_disposition(misses, reparks, reason()),
            );
            let p = app
                .update_handoff_prelaunch
                .as_ref()
                .expect("still holding");
            assert!(!p.stood_down, "re-park {n}: the successor keeps holding");
            assert_eq!(p.park_misses, 0, "re-park {n}: no freeze rung spent");
            assert_eq!(p.park_mid_sequence_reparks, n);
            assert_eq!(
                p.park_retry_at,
                Some(now + super::PRELAUNCH_PARK_RETRY),
                "re-park {n}: at the next 20 ms re-run, not the 500 ms rung delay"
            );
        }
        assert!(stood_down.try_recv().is_err(), "nothing was stood down");
        // Past the bound: the ordinary ladder, which re-parks first.
        app.dispose_park_miss(
            now,
            super::mid_sequence_miss_disposition(
                0,
                super::PRELAUNCH_MAX_MID_SEQUENCE_REPARKS,
                reason(),
            ),
        );
        let p = app
            .update_handoff_prelaunch
            .as_ref()
            .expect("still holding");
        assert_eq!(p.park_misses, 1, "past the bound a miss climbs the ladder");
        assert!(!p.stood_down);
    }

    /// A FLOOD'S BATCH BOUNDARIES FIT THE FREE RE-PARK BOUND: a session fed an
    /// SGR-dense stream cut at arbitrary reader batch boundaries — the moment a
    /// Land park over queued output stops its reader at — has its parser inside
    /// a sequence (`Terminal::partial_sequence_state`, which the capture answers
    /// with `CaptureFailure::MidSequence` while that PTY still has output
    /// queued) at some boundaries and at ground at others, and the longest run of
    /// mid-sequence cuts in a row is far inside the free re-park bound, so the
    /// attempt lands inside one hold. Non-vacuous both ways.
    #[test]
    fn a_flood_cut_at_batch_boundaries_parks_well_inside_the_mid_sequence_bound() {
        let mut app = App::headless_for_test();
        app.push_stub_tab(crate::WindowId(0), crate::stub_session(app.next_session_id));
        // A coloured build log: every word in its own 256-colour SGR, varying
        // widths so no cut period aligns with the stream's.
        let mut flood = Vec::new();
        for i in 0..160_000_u32 {
            flood.extend_from_slice(
                format!(
                    "\x1b[38;5;{}m{}\x1b[0m ",
                    i % 256,
                    "x".repeat((i % 7) as usize + 1)
                )
                .as_bytes(),
            );
            if i % 13 == 0 {
                flood.extend_from_slice(b"\r\n");
            }
        }
        let (mut at_ground, mut mid, mut run, mut longest_run) = (0, 0, 0_u32, 0_u32);
        // Reader batches of assorted sizes, as the kernel hands them out.
        let batches = [4096_usize, 1024, 65_536, 777, 16_384, 3000];
        let mut offset = 0;
        let mut k = 0;
        while offset < flood.len() {
            let end = (offset + batches[k % batches.len()]).min(flood.len());
            k += 1;
            let session = app.pool.iter().next().expect("a session");
            let mut term = session.term.lock().unwrap_or_else(|p| p.into_inner());
            term.process(&flood[offset..end]);
            offset = end;
            if term.partial_sequence_state().is_some() {
                mid += 1;
                run += 1;
                longest_run = longest_run.max(run);
            } else {
                at_ground += 1;
                run = 0;
            }
        }
        assert!(mid > 0, "the flood is cut mid-sequence at some boundaries");
        assert!(at_ground > 0, "and at ground at others");
        assert!(
            longest_run < u32::from(super::PRELAUNCH_MAX_MID_SEQUENCE_REPARKS),
            "longest mid-sequence run {longest_run} must fit the free re-park bound \
             ({mid} mid, {at_ground} at ground)"
        );
    }

    /// A completion whose attempt matches NEITHER record is not reduced and
    /// clears nothing: a stale completion can never retire a newer attempt.
    #[test]
    fn a_stale_completion_clears_no_record() {
        let mut app = App::headless_for_test();
        let (record, _cancelled, _stood_down, _transferred) =
            prelaunch_record(9, ApplyMode::Automatic);
        app.update_handoff_prelaunch = Some(record);
        assert!(
            app.reduce_returned_handoff_completion(completion(8))
                .is_none()
        );
        assert!(app.update_handoff_prelaunch.is_some());
    }

    /// Both records for one attempt: the parked one's facts win, its teardown
    /// merges the prelaunch record's, and both are cleared by the one completion.
    #[test]
    fn both_records_of_one_attempt_are_cleared_together_and_their_teardowns_merge() {
        let mut app = App::headless_for_test();
        let (mut record, _cancelled, _stood_down, _transferred) =
            prelaunch_record(4, ApplyMode::Immediate);
        record.teardown = crate::DeferredHandoffTeardown::QuitRequested;
        app.update_handoff_prelaunch = Some(record);
        app.pending_update_handoff = Some(parked_record(4, ApplyMode::Immediate));
        let teardown = app
            .reduce_returned_handoff_completion(completion(4))
            .expect("reduced");
        assert_eq!(teardown, crate::DeferredHandoffTeardown::QuitRequested);
        assert!(app.update_handoff_prelaunch.is_none());
        assert!(app.pending_update_handoff.is_none());
    }

    /// `Wake::UpdateHandoffStandingDown`: the park gate closes for good, the
    /// parked record is released — its readers resume — and the attempt record
    /// stays, carrying the deferred teardown forward, so nothing can launch
    /// beside the successor being stood down.
    #[test]
    fn a_stand_down_announcement_closes_the_gate_and_releases_the_parked_record() {
        let mut app = App::headless_for_test();
        let (mut record, _cancelled, _stood_down, _transferred) =
            prelaunch_record(4, ApplyMode::Automatic);
        let (ack_tx, ack) = std::sync::mpsc::sync_channel(1);
        record.stand_down_ack = ack_tx;
        app.update_handoff_prelaunch = Some(record);
        let mut parked = parked_record(4, ApplyMode::Automatic);
        parked.teardown = crate::DeferredHandoffTeardown::QuitRequested;
        parked.revoked_by_activity = true;
        app.pending_update_handoff = Some(parked);
        app.answer_prelaunched_stand_down(3);
        assert!(
            app.pending_update_handoff.is_some(),
            "an announcement for another attempt releases nothing"
        );
        assert!(
            !app.update_handoff_prelaunch
                .as_ref()
                .expect("record")
                .stood_down,
            "and closes no gate"
        );
        app.answer_prelaunched_stand_down(4);
        assert!(app.pending_update_handoff.is_none());
        let prelaunch = app
            .update_handoff_prelaunch
            .as_ref()
            .expect("the attempt record outlives the release");
        assert!(
            prelaunch.stood_down,
            "the park gate is shut: nothing may park onto a successor being killed"
        );
        assert!(prelaunch.park_retry_at.is_none());
        assert_eq!(
            prelaunch.teardown,
            crate::DeferredHandoffTeardown::QuitRequested
        );
        assert!(prelaunch.revoked_by_activity);
        assert!(app.update_handoff_in_flight());
        // The worker is waiting on this answer before it revokes.
        assert!(ack.try_recv().is_ok(), "the worker is answered last");
    }

    /// A destructive intent against a HELD successor is no barrier: nothing is
    /// parked and nothing was granted, so the intent proceeds and the worker is
    /// asked to stand the successor down. Against a PARKED attempt it is the
    /// barrier it always was.
    #[test]
    fn a_teardown_is_no_barrier_for_a_held_successor_but_stands_it_down() {
        let mut app = App::headless_for_test();
        let (record, cancelled, stood_down, _transferred) =
            prelaunch_record(2, ApplyMode::Automatic);
        app.update_handoff_prelaunch = Some(record);
        assert!(
            !app.defer_pending_update_handoff_teardown(
                crate::DeferredHandoffTeardown::QuitRequested
            )
        );
        assert!(
            app.update_handoff_prelaunch
                .as_ref()
                .expect("the record outlives the teardown")
                .stood_down,
            "the park gate shuts: nothing may park onto a cancelled attempt"
        );
        assert_eq!(
            stood_down
                .try_recv()
                .expect("a typed stand-down reaches the worker")
                .outcome,
            crate::UpdateHandoffOutcome::ActivityRevoked
        );
        assert!(
            cancelled.try_recv().is_err(),
            "the typed channel carried it; the raw poke is only the fallback"
        );
        app.pending_update_handoff = Some(parked_record(2, ApplyMode::Automatic));
        assert!(
            app.defer_pending_update_handoff_teardown(
                crate::DeferredHandoffTeardown::QuitRequested
            )
        );
    }

    /// The main thread's typed stand-down reaches the worker once; a second
    /// stand-down for the same attempt is a no-op, and a worker that is past
    /// its hold (channel full or gone) is reached through the cancel poke.
    #[test]
    fn a_stand_down_is_typed_once_and_falls_back_to_the_cancel_poke() {
        let mut app = App::headless_for_test();
        let (record, cancelled, stood_down, _transferred) =
            prelaunch_record(6, ApplyMode::Automatic);
        app.update_handoff_prelaunch = Some(record);
        app.stand_down_prelaunched_successor(super::HandoffStandDown {
            outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
            detail: "first".to_string(),
        });
        app.stand_down_prelaunched_successor(super::HandoffStandDown {
            outcome: crate::UpdateHandoffOutcome::TimedOut,
            detail: "second".to_string(),
        });
        let received = stood_down
            .try_recv()
            .expect("the first stand-down is delivered");
        assert_eq!(received.detail, "first");
        assert!(stood_down.try_recv().is_err(), "the second is a no-op");
        assert!(cancelled.try_recv().is_err());
        // A full channel (the worker has not drained the first) falls back to
        // the cancel poke for a fresh attempt.
        let (record, cancelled, stood_down, _transferred) =
            prelaunch_record(7, ApplyMode::Automatic);
        app.update_handoff_prelaunch = Some(record);
        app.update_handoff_prelaunch
            .as_ref()
            .expect("record")
            .stand_down
            .try_send(super::HandoffStandDown {
                outcome: crate::UpdateHandoffOutcome::Rejected,
                detail: "occupying the slot".to_string(),
            })
            .expect("the slot was free");
        app.stand_down_prelaunched_successor(super::HandoffStandDown {
            outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
            detail: "cannot be delivered".to_string(),
        });
        assert!(
            cancelled.try_recv().is_ok(),
            "the cancel poke is the fallback"
        );
        drop(stood_down);
    }

    /// The park cue for the wrong attempt is ignored; the right one records the
    /// dial and runs the gate — which, with no session left to park, stands the
    /// attempt down as activity-revoked rather than parking nothing.
    #[test]
    fn the_park_cue_binds_to_its_attempt_and_an_empty_pool_stands_down() {
        let mut app = App::headless_for_test();
        app.pool.sessions.clear();
        let (record, _cancelled, stood_down, _transferred) =
            prelaunch_record(11, ApplyMode::Immediate);
        app.update_handoff_prelaunch = Some(record);
        app.on_update_handoff_awaiting_park(10, Some(4242), None);
        assert!(
            app.update_handoff_prelaunch
                .as_ref()
                .is_some_and(|prelaunch| prelaunch.dialled.is_none()),
            "a cue for another attempt records no dial"
        );
        app.on_update_handoff_awaiting_park(11, Some(4242), None);
        let prelaunch = app.update_handoff_prelaunch.as_ref().expect("record");
        assert_eq!(
            prelaunch.dialled.and_then(|dialled| dialled.pid),
            Some(4242)
        );
        assert!(prelaunch.stood_down, "nothing to park is a stand-down");
        let received = stood_down.try_recv().expect("typed stand-down");
        assert_eq!(
            received.outcome,
            crate::UpdateHandoffOutcome::ActivityRevoked
        );
    }

    /// The headless fixture's session has no real master: the park half's
    /// descriptor duplicate fails, the readers it parked are rolled back, and
    /// the held successor is stood down as THIS PROCESS's failure
    /// (`ProducerFailed`, Transient) — never left holding a claim for a park
    /// that cannot complete, and never filed as the candidate's bytes failing
    /// (`PreparationFailed`, Structural, which it was until the 2026-09-22/23
    /// update audit, plan P1-2: an `EMFILE` latched the artifact after two).
    #[test]
    fn a_park_half_failure_stands_the_held_successor_down_as_a_producer_failure() {
        let mut app = App::headless_for_test();
        let (record, _cancelled, stood_down, transferred) =
            prelaunch_record(12, ApplyMode::Immediate);
        app.update_handoff_prelaunch = Some(record);
        app.on_update_handoff_awaiting_park(12, Some(4242), None);
        let prelaunch = app.update_handoff_prelaunch.as_ref().expect("record");
        assert!(prelaunch.stood_down);
        assert!(app.pending_update_handoff.is_none(), "nothing stays parked");
        assert!(transferred.try_recv().is_err(), "no capture was sent");
        let received = stood_down.try_recv().expect("typed stand-down");
        assert_eq!(
            received.outcome,
            crate::UpdateHandoffOutcome::ProducerFailed
        );
        assert!(
            received
                .detail
                .starts_with("could not reserve child-only PTY descriptors ("),
            "the descriptor failure carries its io::ErrorKind: {}",
            received.detail
        );
    }

    /// The post-park proof deadline ladder: short on the automatic lane (the
    /// slow parts already happened with readers live), wider after a physical
    /// failure of the same bytes, widest for an explicit apply.
    #[test]
    fn the_post_park_proof_deadline_widens_on_retries_and_for_an_explicit_apply() {
        if std::env::var_os("ATERM_HANDOFF_PROOF_TIMEOUT_MS").is_some() {
            return;
        }
        let park_at = std::time::Instant::now();
        let budget = |mode, prior| {
            super::handoff_proof_deadline(mode, prior, park_at).saturating_duration_since(park_at)
        };
        assert_eq!(
            budget(ApplyMode::Automatic, 0),
            std::time::Duration::from_secs(3)
        );
        assert_eq!(
            budget(ApplyMode::AutomaticPastGrace, 1),
            std::time::Duration::from_secs(6)
        );
        assert_eq!(
            budget(ApplyMode::Immediate, 0),
            std::time::Duration::from_secs(30)
        );
        assert!(matches!(
            crate::update_handoff_wake_class(&crate::Wake::UpdateHandoffAwaitingPark {
                attempt_id: 1,
                dialer_pid: None,
                grant_limit: None,
            }),
            crate::UpdateHandoffEventClass::Exempt
        ));
        assert!(matches!(
            crate::update_handoff_wake_class(&crate::Wake::UpdateHandoffStandingDown {
                attempt_id: 1
            }),
            crate::UpdateHandoffEventClass::Exempt
        ));
    }

    /// THE RECORD NEVER LEAVES AN "INSTALLING" THAT WAS NOT (Settings ▸ Messages): a
    /// switch put on record and then stood down with this process still running is
    /// written down as stopped — once — and an attempt that never reached the record
    /// writes nothing when it ends.
    #[test]
    fn a_switch_on_record_that_stands_down_is_recorded_as_stopped() {
        let titles = |app: &App| {
            app.messages
                .log()
                .records()
                .map(|record| record.title.clone())
                .filter(|title| title.contains("reload") || title.contains("Reloading"))
                .collect::<Vec<_>>()
        };
        let mut app = App::headless_for_test();
        // Negative control: an attempt that never reached the record.
        let (record, _cancelled, _stood_down, _transferred) =
            prelaunch_record(8, ApplyMode::Automatic);
        app.update_handoff_prelaunch = Some(record);
        let _ = app.reduce_returned_handoff_completion(completion(8));
        assert!(titles(&app).is_empty(), "{:?}", titles(&app));

        app.record_update_switch_started(91, true, false);
        assert_eq!(titles(&app), vec!["Reloading aterm in place"]);
        let (record, _cancelled, _stood_down, _transferred) =
            prelaunch_record(9, ApplyMode::Automatic);
        app.update_handoff_prelaunch = Some(record);
        let _ = app.reduce_returned_handoff_completion(completion(9));
        assert_eq!(
            titles(&app),
            vec!["Reloading aterm in place", "Waiting to reload aterm"],
            "stood down on activity: routine, and said so"
        );
        // Once: the attempt is off the record now.
        app.record_update_switch_stopped(false, "again");
        assert_eq!(titles(&app).len(), 2);
    }

    /// A HALT THAT MOVES AFTER THE PARK REFUSES THE COMMIT (the round-four
    /// plan, item 3). The park draws the manifest — every standing hold in it
    /// — and the successor adopts from that manifest, while this process's
    /// control thread keeps serving `hold`. A `hold on` answered in that
    /// window would reach no one: the Commit would hand the session to a
    /// successor that never saw it, a seamless update lifting a halt it had
    /// just acknowledged. So the Commit's `exact_activity` fact requires every
    /// session's hold serial to be where the park read it; a hold set AND
    /// lifted inside the window moves it too.
    ///
    /// FAILS WITHOUT THE FIX: `exact_activity` read only the activity epoch,
    /// which a control-thread `hold` never moves, so both refusals below read
    /// `true` (measured by comparing only the epoch).
    #[test]
    fn a_hold_that_moves_after_the_park_refuses_the_commit() {
        use crate::fabric::{Hold, apply_hold_for_test};
        let mut app = App::headless_for_test();
        let ctx = app
            .pool
            .iter()
            .next()
            .expect("the boot session")
            .ctx
            .clone();
        let arm = |app: &mut App| {
            let mut pending = parked_record(9, ApplyMode::Immediate);
            pending.live = app
                .pool
                .iter()
                .map(|session| (session.id, session.master, session.pid))
                .collect();
            pending.layout = app.capture_handoff_layout();
            pending.activity_epoch = app.update_handoff_activity_epoch;
            pending.hold_serials = app.hold_serials();
            app.pending_update_handoff = Some(pending);
        };
        let exact_activity = |app: &mut App| {
            app.collect_handoff_commit_facts(None, true, true, true)
                .expect("an attempt is pending")
                .0
                .exact_activity
        };
        let halt = || Hold {
            reason: "stop".to_string(),
            origin: "local".to_string(),
        };

        arm(&mut app);
        assert!(
            exact_activity(&mut app),
            "control: nothing moved since the park"
        );

        assert!(apply_hold_for_test(&ctx, Some(halt())));
        assert!(
            !exact_activity(&mut app),
            "a halt set after the park is not in the manifest: no Commit"
        );

        // Re-armed with the halt standing (the park now draws it), a halt set
        // and lifted inside the window still refuses: the manifest carried a
        // hold the session no longer has.
        arm(&mut app);
        assert!(exact_activity(&mut app));
        assert!(apply_hold_for_test(&ctx, None));
        assert!(apply_hold_for_test(&ctx, Some(halt())));
        assert!(!exact_activity(&mut app), "a round trip is still a move");

        // A no-op act (the hold already stands) moves nothing.
        arm(&mut app);
        assert!(!apply_hold_for_test(&ctx, Some(halt())));
        assert!(exact_activity(&mut app));
    }

    /// A HALT ISSUED WHILE THE COMMIT RUNS IS NEVER ANSWERED AND THEN LOST
    /// (the round-four review). The Commit compares the hold serials once, in
    /// `collect_handoff_commit_facts`, and then still activates the successor,
    /// suspends the harness and writes the identity markers before its
    /// `_exit`; the control thread serves `hold` throughout. Now that reading
    /// raises a fence: a `hold on` issued after it gets no answer while the
    /// attempt can still land, and lands — `OK hold=1`, the serial moved — the
    /// moment the attempt stands down (its fence drops). A Commit that lands
    /// `_exit`s with the caller still waiting, so its connection closes with
    /// no `OK`. Past the verb's bound it is `ERR busy`, with nothing moved.
    ///
    /// FAILS WITHOUT THE FIX: `apply_hold` read no fence, so the verb below
    /// answered `OK hold=1` at once, inside the window, with the Commit's
    /// facts already collected and saying nothing had moved.
    #[test]
    fn a_hold_issued_during_the_commit_waits_for_the_attempt() {
        use crate::fabric::{HOLD_BUSY, HoldIssuer, cmd_hold, cmd_hold_within};
        let mut app = App::headless_for_test();
        let ctx = app
            .pool
            .iter()
            .next()
            .expect("the boot session")
            .ctx
            .clone();
        let sid = ctx.self_id.as_str().to_string();
        let mut pending = parked_record(9, ApplyMode::Immediate);
        pending.live = app
            .pool
            .iter()
            .map(|session| (session.id, session.master, session.pid))
            .collect();
        pending.layout = app.capture_handoff_layout();
        pending.activity_epoch = app.update_handoff_activity_epoch;
        pending.hold_serials = app.hold_serials();
        let parked_serials = pending.hold_serials;
        app.pending_update_handoff = Some(pending);

        let (facts, _, _, _, fence) = app
            .collect_handoff_commit_facts(None, true, true, true)
            .expect("an attempt is pending");
        assert!(
            facts.exact_activity,
            "control: nothing moved since the park"
        );

        // Past the bound, a committing session answers busy and moves nothing.
        assert_eq!(
            cmd_hold_within(
                &app.store,
                &format!("{sid} on reason=stop"),
                HoldIssuer::Owner,
                std::time::Duration::from_millis(50),
            ),
            HOLD_BUSY
        );
        assert_eq!(ctx.fabric.hold(), None);
        assert_eq!(app.hold_serials(), parked_serials);

        // Inside the bound, the act waits for the attempt.
        let store = app.store.clone();
        let line = format!("{sid} on reason=stop");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(cmd_hold(&store, &line, HoldIssuer::Owner));
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "no answer while the Commit that read the serials can still land"
        );
        assert_eq!(ctx.fabric.hold(), None, "and nothing applied");
        assert_eq!(app.hold_serials(), parked_serials);

        // The attempt stands down: the halt lands and is answered.
        drop(fence);
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(10))
                .expect("answered once the fence lifts"),
            "OK hold=1\n"
        );
        assert!(ctx.fabric.hold().is_some());
        assert_ne!(app.hold_serials(), parked_serials);
    }
}

/// The worker's own publication of a PASS for `attempt` — the publisher the
/// handoff worker builds from its attempt ([`PreverifyPublisher::for_attempt`])
/// and the call it makes on a pass — so a test seeds the cache exactly as the
/// shipping lane does. Returns the policy it published, narrowed to this build.
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

/// THE WORKER'S OWN PRE-VERIFICATION REACHES THE PARK (the 2026-09-22/23 update
/// audit, plan P0-5). An attempt later than the arm-time verdict's ten minutes
/// re-verifies the candidate on the handoff worker; its PASS — and the handoff
/// policy it read — is published into the App's cache under the attempt's own
/// artifact key, where the launched lane's park, on the main thread, reads it.
#[cfg(all(test, unix))]
mod preverify_publisher_tests {
    use super::{PreverifyPublisher, capture_exceeds_policy};
    use aterm_update_core::handoff_policy::{CarryCeiling, HandoffPolicy, PolicyRead};

    /// THE PUBLISHER AND THE PARK AGREE ON THE KEY BECAUSE BOTH TAKE IT FROM THE
    /// TICKET. The worker builds its publisher from the attempt
    /// (`PreverifyPublisher::for_attempt`), and every lane's park reads the
    /// policy for the same attempt (`App::park_policy`); this drives exactly
    /// those two calls. It also pins what a park knows before any pass: nothing
    /// — `known` is false, so the fork lane says so and has its worker check the
    /// capture against the policy once read.
    #[test]
    fn the_worker_publishes_and_the_park_reads_by_the_same_ticket() {
        let app = crate::App::headless_for_test();
        let running = crate::running_build_number();
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            running.saturating_add(1),
            COMMIT,
            &"ab".repeat(32),
        );
        let unread = app.park_policy(Some(&ticket));
        assert_eq!(unread, super::ParkPolicy::default());
        assert!(!unread.known, "no pass on record: the policy is not known");
        assert_eq!(unread.carry_ceiling(), CarryCeiling::Full);
        assert_eq!(app.park_policy(None), super::ParkPolicy::default());

        let relaxed_repaint = PolicyRead::Parsed(
            HandoffPolicy::parse(
                "schema = 1\ncarry = \"repaint\"\npark_quiet_gate_at_land = \"relaxed\"\n",
            )
            .expect("schema 1"),
        );
        PreverifyPublisher::for_attempt(&app.handoff_preverified, Some(&ticket))
            .expect("an attempt names its artifact")
            .publish_policy_pass(&relaxed_repaint, running, "the test candidate");
        let read = app.park_policy(Some(&ticket));
        assert!(read.known);
        assert_eq!(read.carry_ceiling(), CarryCeiling::Repaint);
        assert!(
            read.relaxes_land_gate(),
            "the launched lane's gate reads it too"
        );

        // A pass for a policy-free candidate is KNOWN to ask nothing.
        PreverifyPublisher::for_attempt(&app.handoff_preverified, Some(&ticket))
            .expect("an attempt names its artifact")
            .publish_policy_pass(&PolicyRead::Absent, running, "the test candidate");
        let nothing = app.park_policy(Some(&ticket));
        assert!(nothing.known && nothing.policy.is_none());
    }

    /// A FORK-LANE CAPTURE TAKEN ABOVE THE POLICY ITS WORKER THEN READS IS NEVER
    /// HANDED OVER. The fork lane parks before its worker verifies the candidate;
    /// with no pass on record it parked at Full, and before this check a policy
    /// read afterwards was published for the NEXT park while this one's capture,
    /// taken above it, went to the successor anyway — the producer bug the policy
    /// was sealed to avoid, with nothing in the log naming the policy.
    #[test]
    fn a_fork_capture_above_the_policy_read_after_it_is_refused() {
        let policy = |text: &str| Some(HandoffPolicy::parse(text).expect("schema 1"));
        let repaint = policy("schema = 1\ncarry = \"repaint\"\n");
        let visible = policy("schema = 1\ncarry = \"visible\"\n");
        let refusal = capture_exceeds_policy(Some(CarryCeiling::Full), repaint)
            .expect("a Full capture under a repaint policy is refused");
        assert!(
            refusal.contains("carry=repaint") && refusal.contains("next attempt parks under it"),
            "{refusal}"
        );
        assert!(capture_exceeds_policy(Some(CarryCeiling::Full), visible).is_some());
        assert_eq!(
            capture_exceeds_policy(Some(CarryCeiling::Repaint), repaint),
            None,
            "a park that already followed it"
        );
        assert_eq!(
            capture_exceeds_policy(Some(CarryCeiling::Repaint), visible),
            None,
            "a park below the ceiling"
        );
        assert_eq!(
            capture_exceeds_policy(Some(CarryCeiling::Full), None),
            None,
            "no policy for this build"
        );
        assert_eq!(
            capture_exceeds_policy(None, repaint),
            None,
            "the launched lane parks after reading it"
        );
    }

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn publisher(app: &crate::App, build: u64) -> PreverifyPublisher {
        PreverifyPublisher {
            slot: std::sync::Arc::clone(&app.handoff_preverified),
            build,
            commit: COMMIT.to_string(),
            artifact: "ab".repeat(32),
        }
    }

    #[test]
    fn a_published_pass_carries_the_policy_to_the_park_by_the_attempt_s_key() {
        let app = crate::App::headless_for_test();
        let running = crate::running_build_number();
        let target = running.saturating_add(1);
        let artifact = "ab".repeat(32);
        let repaint = PolicyRead::Parsed(
            HandoffPolicy::parse("schema = 1\ncarry = \"repaint\"\n").expect("schema 1"),
        );
        assert_eq!(app.handoff_policy_for(target, COMMIT, &artifact), None);
        publisher(&app, target).publish_policy_pass(&repaint, running, "the test candidate");
        assert_eq!(
            app.handoff_policy_for(target, COMMIT, &artifact)
                .map(|policy| policy.carry_ceiling()),
            Some(CarryCeiling::Repaint),
            "the park reads the published policy"
        );
        let cached = app
            .handoff_preverified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("published");
        assert!(cached.passed && cached.reason.is_none());
        assert_eq!(
            (
                cached.build,
                cached.commit.as_str(),
                cached.artifact.as_str()
            ),
            (target, COMMIT, artifact.as_str())
        );
        // The key is the attempt's: another build, commit or artifact reads none.
        assert_eq!(app.handoff_policy_for(target + 1, COMMIT, &artifact), None);
        assert_eq!(
            app.handoff_policy_for(target, &"f".repeat(40), &artifact),
            None
        );
        assert_eq!(
            app.handoff_policy_for(target, COMMIT, &"cd".repeat(32)),
            None
        );

        // A PASS whose policy file could not be followed, or is for other
        // builds, still publishes the pass — and no policy.
        let other = running.wrapping_add(7);
        for read in [
            PolicyRead::Ignored("not TOML".to_string()),
            PolicyRead::Absent,
            PolicyRead::Parsed(
                HandoffPolicy::parse(&format!(
                    "schema = 1\napplies_to_producers = [{other}, {other}]\ncarry = \"repaint\"\n"
                ))
                .expect("schema 1"),
            ),
        ] {
            publisher(&app, target).publish_policy_pass(&read, running, "the test candidate");
            assert_eq!(
                app.handoff_policy_for(target, COMMIT, &artifact),
                None,
                "{read:?}"
            );
            assert!(
                app.handoff_preverified
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .is_some_and(|entry| entry.passed),
                "{read:?}: the pass itself is published"
            );
        }
    }
}

/// THE PARK GATE, enumerated. The late park (2026-09-19) made "start the
/// update" and "freeze the terminal" two decisions; this is the second one, and
/// every rule in it is the ladder's rule for the live phase plus the one
/// correctness fact (unconsumed master bytes) the ladder never relaxes.
#[cfg(all(test, unix))]
mod park_gate_tests {
    use super::{
        PRELAUNCH_HOLD_MAX, ParkGate, ParkGateFacts, prelaunch_hold_cap, prelaunch_park_admitted,
    };
    use crate::native_update_auto_intent::{ActivityFacts, ApplyPhase, automatic_park_refusal};
    use crate::native_updater_service::ApplyMode;

    const MODES: [ApplyMode; 3] = [
        ApplyMode::Automatic,
        ApplyMode::AutomaticPastGrace,
        ApplyMode::Immediate,
    ];
    const PHASES: [ApplyPhase; 4] = [
        ApplyPhase::PreferIdle,
        ApplyPhase::PreferOutputGap,
        ApplyPhase::KeysOnly,
        ApplyPhase::Land,
    ];

    /// A machine that is idle in every sense: every mode parks on these.
    fn calm(mode: ApplyMode, phase: ApplyPhase) -> ParkGateFacts {
        ParkGateFacts {
            mode,
            phase,
            activity: ActivityFacts {
                quiet: true,
                hands_off_keys: true,
                output_quiet: true,
                focused: true,
                consent_warmup: false,
                harness_restored_pending: false,
            },
            masters_quiet: true,
            masters_alive: true,
            land_waits: 0,
            land_gate_relaxed: false,
            held_for: std::time::Duration::ZERO,
            recording: false,
        }
    }

    fn facts_from_bits(mode: ApplyMode, phase: ApplyPhase, bits: u32) -> ParkGateFacts {
        ParkGateFacts {
            mode,
            phase,
            activity: ActivityFacts {
                quiet: bits & 1 != 0,
                hands_off_keys: bits & 2 != 0,
                output_quiet: bits & 4 != 0,
                focused: bits & 8 != 0,
                consent_warmup: bits & 32 != 0,
                harness_restored_pending: bits & 64 != 0,
            },
            masters_quiet: bits & 16 != 0,
            masters_alive: true,
            land_waits: 0,
            land_gate_relaxed: false,
            held_for: std::time::Duration::ZERO,
            recording: false,
        }
    }

    fn gate(facts: ParkGateFacts) -> ParkGate {
        prelaunch_park_admitted(facts, prelaunch_hold_cap(facts.mode))
    }

    /// EVERY COMBINATION of the seven boolean facts, for every mode and every
    /// phase, against the rule written out independently here. A truth table
    /// rather than a handful of cases, because the failure this guards is one
    /// arm quietly admitting a park the ladder would have refused — or refusing
    /// one it owes.
    #[test]
    fn the_gate_admits_exactly_what_the_ladder_owes() {
        for mode in MODES {
            for phase in PHASES {
                for bits in 0..128u32 {
                    let facts = facts_from_bits(mode, phase, bits);
                    let a = facts.activity;
                    let expected = if !mode.is_automatic() {
                        // The person asked and is waiting for it.
                        true
                    } else {
                        facts.masters_quiet
                            && !a.consent_warmup
                            && !a.harness_restored_pending
                            && match phase {
                                ApplyPhase::PreferIdle => a.hands_off_keys && a.quiet,
                                ApplyPhase::PreferOutputGap => {
                                    a.hands_off_keys && !(a.focused && !a.output_quiet)
                                }
                                ApplyPhase::KeysOnly => a.hands_off_keys,
                                ApplyPhase::Land => true,
                            }
                    };
                    assert_eq!(
                        gate(facts) == ParkGate::Park,
                        expected,
                        "{mode:?} {phase:?} with {facts:?} disagreed with the rule"
                    );
                    // And the park's refusal is the ENTRY's refusal, word for
                    // word: one predicate, two instants.
                    if mode.is_automatic() && facts.masters_quiet {
                        let entry = automatic_park_refusal(phase, a);
                        match gate(facts) {
                            ParkGate::Park => assert_eq!(entry, None),
                            ParkGate::Wait(reason) => assert_eq!(entry, Some(reason)),
                            ParkGate::StandDown(_) => unreachable!("held_for is zero"),
                        }
                    }
                }
            }
        }
    }

    /// The typing gap holds every automatic phase but the last: a keystroke
    /// inside it means fingers are on the keys. `Land` is the bound that keeps
    /// the ladder from being a refusal that can repeat forever; the deferred
    /// input queue is what makes overriding the gap there safe.
    #[test]
    fn a_keystroke_inside_the_typing_gap_holds_every_automatic_phase_but_land() {
        for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
            for phase in [
                ApplyPhase::PreferIdle,
                ApplyPhase::PreferOutputGap,
                ApplyPhase::KeysOnly,
            ] {
                let facts = ParkGateFacts {
                    activity: ActivityFacts {
                        hands_off_keys: false,
                        ..calm(mode, phase).activity
                    },
                    ..calm(mode, phase)
                };
                assert!(
                    matches!(gate(facts), ParkGate::Wait(_)),
                    "{mode:?} {phase:?}"
                );
            }
            let landing = ParkGateFacts {
                activity: ActivityFacts {
                    hands_off_keys: false,
                    quiet: false,
                    output_quiet: false,
                    focused: true,
                    consent_warmup: false,
                    harness_restored_pending: false,
                },
                ..calm(mode, ApplyPhase::Land)
            };
            assert_eq!(gate(landing), ParkGate::Park, "{mode:?} lands at the bound");
            // …except on top of a consent dialog the user asked for: their own
            // warm-up holds every phase, `Land` included, capped by its setting.
            let warming = ParkGateFacts {
                activity: ActivityFacts {
                    consent_warmup: true,
                    ..landing.activity
                },
                ..landing
            };
            assert!(
                matches!(gate(warming), ParkGate::Wait(_)),
                "{mode:?} holds for the user's warm-up even at the bound"
            );
            // …and over the agents a cold restore is still relaunching (round
            // four, plan item 7): the host's bounded hold, never activity.
            let relaunching = ParkGateFacts {
                activity: ActivityFacts {
                    harness_restored_pending: true,
                    ..landing.activity
                },
                ..landing
            };
            assert_eq!(
                gate(relaunching),
                ParkGate::Wait(crate::native_update_auto_intent::RESTORED_PENDING_REFUSAL),
                "{mode:?} holds for the restored agents even at the bound"
            );
        }
        let mode = ApplyMode::Immediate;
        let facts = ParkGateFacts {
            activity: ActivityFacts {
                hands_off_keys: false,
                ..calm(mode, ApplyPhase::PreferIdle).activity
            },
            ..calm(mode, ApplyPhase::PreferIdle)
        };
        assert_eq!(
            gate(facts),
            ParkGate::Park,
            "{mode:?}: the person asked for this pause"
        );
    }

    /// THE MACHINE THIS LADDER EXISTS FOR: an agent streaming into a focused
    /// pane, never quiet, never pausing its output. It waits through the first
    /// two phases (the owner's 2026-09-18 ruling, kept as a bounded preference)
    /// and lands in the third, with the user's hands off the keys.
    #[test]
    fn a_focused_never_quiet_stream_lands_in_the_keys_only_phase() {
        let streaming = |phase| ParkGateFacts {
            activity: ActivityFacts {
                quiet: false,
                hands_off_keys: true,
                output_quiet: false,
                focused: true,
                consent_warmup: false,
                harness_restored_pending: false,
            },
            ..calm(ApplyMode::AutomaticPastGrace, phase)
        };
        assert!(matches!(
            gate(streaming(ApplyPhase::PreferIdle)),
            ParkGate::Wait(_)
        ));
        assert_eq!(
            gate(streaming(ApplyPhase::PreferOutputGap)),
            ParkGate::Wait("terminal output is still streaming")
        );
        assert_eq!(gate(streaming(ApplyPhase::KeysOnly)), ParkGate::Park);
        assert_eq!(gate(streaming(ApplyPhase::Land)), ParkGate::Park);
        // With no aterm window focused the stall is invisible: the output-gap
        // phase lands too.
        assert_eq!(
            gate(ParkGateFacts {
                activity: ActivityFacts {
                    focused: false,
                    ..streaming(ApplyPhase::PreferOutputGap).activity
                },
                ..streaming(ApplyPhase::PreferOutputGap)
            }),
            ParkGate::Park,
            "with the user in another app the stall is invisible"
        );
    }

    /// MONOTONE IN `held_for`, which is what bounds the hold: past the cap every
    /// automatic mode stands down whatever else is true, and nothing later can
    /// make it admit. The explicit modes have no cap because they never wait.
    #[test]
    fn the_hold_cap_stands_down_and_never_admits_again() {
        for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
            assert_eq!(prelaunch_hold_cap(mode), Some(PRELAUNCH_HOLD_MAX));
            for phase in PHASES {
                for bits in 0..128u32 {
                    let facts = ParkGateFacts {
                        held_for: PRELAUNCH_HOLD_MAX,
                        ..facts_from_bits(mode, phase, bits)
                    };
                    assert!(
                        matches!(gate(facts), ParkGate::StandDown(_)),
                        "{mode:?} past the cap must stand down: {facts:?}"
                    );
                    assert!(
                        matches!(
                            gate(ParkGateFacts {
                                held_for: PRELAUNCH_HOLD_MAX * 2,
                                ..facts
                            }),
                            ParkGate::StandDown(_)
                        ),
                        "and stays stood down"
                    );
                }
            }
            // Just inside the cap a calm machine still parks: the cap bounds the
            // wait, it does not shorten it.
            assert_eq!(
                gate(ParkGateFacts {
                    held_for: PRELAUNCH_HOLD_MAX - std::time::Duration::from_millis(1),
                    ..calm(mode, ApplyPhase::PreferIdle)
                }),
                ParkGate::Park
            );
        }
        let mode = ApplyMode::Immediate;
        assert_eq!(prelaunch_hold_cap(mode), None);
        assert_eq!(
            gate(ParkGateFacts {
                held_for: PRELAUNCH_HOLD_MAX * 10,
                ..calm(mode, ApplyPhase::PreferIdle)
            }),
            ParkGate::Park
        );
    }

    /// WHAT A RE-PARK BUYS. A park that missed its 20 ms budget re-parks on the
    /// ladder's next rung with the successor still booted and holding, and the
    /// attempt climbs the WHOLE freeze ladder that way — 20, 80, 250 ms — before
    /// it gives the successor back. Past the last rung it stands down as a
    /// busy-machine fact, in the ACTIVITY lane: no physical budget, no latch.
    ///
    /// THE DEAD MUTANT (2026-09-21): the second miss used to stand down as
    /// `PreparationFailed`, which the shape classifier files as STRUCTURAL — a
    /// verdict about the bytes with a two-attempt lifetime — so two busy moments
    /// ten minutes apart converged the automatic lane to a deadline-less
    /// manual-only latch. Every rung of that sequence is pinned here: the
    /// disposition at every miss count, the outcome it stands down with, and the
    /// lane the shipping classifier puts that outcome in.
    #[test]
    fn a_park_miss_buys_the_ladders_next_rung_without_a_relaunch() {
        use super::{
            FreezeSeed, PRELAUNCH_MAX_PARK_MISSES, PRELAUNCH_REPARK_DELAY, ParkMissDisposition,
            handoff_freeze_budget, park_miss_disposition,
        };
        use crate::app_native::{HandoffFailureLane, PhysicalFailureShape};

        let rung =
            |misses: u8| handoff_freeze_budget(ApplyMode::Automatic, misses, FreezeSeed::Default);
        assert_eq!(rung(0), std::time::Duration::from_millis(20));
        assert_eq!(rung(1), std::time::Duration::from_millis(80));
        assert_eq!(rung(2), std::time::Duration::from_millis(250));
        assert_eq!(
            PRELAUNCH_MAX_PARK_MISSES, 2,
            "the attempt climbs every rung itself; nothing is left for the ledger"
        );
        // Every miss short of the last rung re-parks one rung wider.
        for misses in 0..PRELAUNCH_MAX_PARK_MISSES {
            match park_miss_disposition(misses, "a PTY reader missed the deadline".into()) {
                ParkMissDisposition::Repark {
                    misses: next,
                    reason,
                } => {
                    assert_eq!(next, misses + 1, "the re-park is the next rung");
                    assert!(rung(next) > rung(misses), "and the next rung is wider");
                    assert_eq!(reason, "a PTY reader missed the deadline");
                }
                ParkMissDisposition::StandDown(stand_down) => panic!(
                    "miss {misses} of {PRELAUNCH_MAX_PARK_MISSES} must re-park, not stand \
                     down ({})",
                    stand_down.detail
                ),
                ParkMissDisposition::RetryMidSequence { .. } => {
                    panic!("an ordinary miss is never a free mid-sequence re-park")
                }
            }
        }
        // The last rung missed: the successor is stood down as ACTIVITY, and the
        // shipping classifier keeps it out of every physical shape.
        let ParkMissDisposition::StandDown(stand_down) =
            park_miss_disposition(PRELAUNCH_MAX_PARK_MISSES, "capture exceeded 250 ms".into())
        else {
            panic!("past the last rung the attempt must stand down");
        };
        assert_eq!(
            stand_down.outcome,
            crate::UpdateHandoffOutcome::ActivityRevoked,
            "a stopwatch the parent missed is a fact about the machine's moment, never \
             about the bytes"
        );
        assert!(stand_down.detail.contains("capture exceeded 250 ms"));
        for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
            let lane = HandoffFailureLane::classify(
                mode,
                stand_down.outcome,
                crate::ChildDeathEvidence::Unobserved,
                false,
            );
            assert_eq!(lane, HandoffFailureLane::ActivityRevoked, "{mode:?}");
            assert_ne!(
                lane,
                HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
                "{mode:?}: the two-attempt structural lifetime is the mutant"
            );
        }
        // A prior physical failure of the same bytes still compounds: an attempt
        // that already failed once starts one rung wider and saturates at 250 ms.
        assert_eq!(
            handoff_freeze_budget(ApplyMode::Automatic, 1 + 1, FreezeSeed::Default),
            std::time::Duration::from_millis(250)
        );
        assert_eq!(
            handoff_freeze_budget(
                ApplyMode::Automatic,
                1 + PRELAUNCH_MAX_PARK_MISSES,
                FreezeSeed::Default
            ),
            std::time::Duration::from_millis(250)
        );
        assert!(
            PRELAUNCH_REPARK_DELAY * u32::from(PRELAUNCH_MAX_PARK_MISSES) < PRELAUNCH_HOLD_MAX,
            "every re-park must fit inside the hold it is spending"
        );
    }

    /// A HUNG-UP master is not output, and it is not parked over either (plan
    /// P1-3): the gate waits — in every automatic phase, whatever it has
    /// waited — with a reason that says so, instead of "output waiting" (the
    /// old reason) or freezing the terminal only for the post-park death check
    /// to refuse. A person's apply still parks at once.
    ///
    /// The wait is unbounded HERE on purpose, and bounded one level up: a pane
    /// kept open after its command exited (`--hold`) is not among the masters
    /// this gate is shown (`App::handoff_live_sessions`, pinned by
    /// `an_exited_held_pane_is_not_handed_and_does_not_hold_the_park`), so a
    /// dead master reaches it only for the moment before the registry marks
    /// the exit (the 2026-09-24 review).
    #[test]
    fn a_dead_master_holds_every_automatic_phase_with_its_own_reason() {
        for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
            for phase in PHASES {
                for land_waits in [0, super::PRELAUNCH_LAND_MAX_WAITS, u8::MAX] {
                    assert_eq!(
                        gate(ParkGateFacts {
                            masters_alive: false,
                            land_waits,
                            ..calm(mode, phase)
                        }),
                        ParkGate::Wait("a session's command has exited but its pane is still open"),
                        "{mode:?} {phase:?} after {land_waits} waits"
                    );
                }
            }
        }
        let mode = ApplyMode::Immediate;
        assert_eq!(
            gate(ParkGateFacts {
                masters_alive: false,
                ..calm(mode, ApplyPhase::PreferIdle)
            }),
            ParkGate::Park,
            "{mode:?}: the person asked; Commit's own liveness check answers them"
        );
    }

    /// `park_quiet_gate_at_land = "relaxed"` IN THE SUCCESSOR'S SIGNED POLICY
    /// (the 2026-09-22/23 update audit, plan P0-5) brings the `Land` bound
    /// forward to the first wait and relaxes nothing else: queued output still
    /// holds every earlier phase, a dead master still holds every phase, and
    /// the hold cap still stands the successor down. RED without the policy's
    /// term in the gate (measured by dropping `|| facts.land_gate_relaxed`):
    /// the relaxed `Land` gate waits on its first run.
    #[test]
    fn a_relaxed_land_policy_parks_over_queued_output_at_land_and_nowhere_else() {
        for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
            for phase in PHASES {
                let relaxed = gate(ParkGateFacts {
                    masters_quiet: false,
                    land_gate_relaxed: true,
                    ..calm(mode, phase)
                });
                if phase == ApplyPhase::Land {
                    assert_eq!(relaxed, ParkGate::Park, "{mode:?}: parks at the first run");
                } else {
                    assert!(
                        matches!(relaxed, ParkGate::Wait(_)),
                        "{mode:?} {phase:?}: an earlier phase still prefers a clean instant"
                    );
                }
                assert_eq!(
                    gate(ParkGateFacts {
                        masters_quiet: false,
                        masters_alive: false,
                        land_gate_relaxed: true,
                        ..calm(mode, phase)
                    }),
                    ParkGate::Wait("a session's command has exited but its pane is still open"),
                    "{mode:?} {phase:?}: never over a dead master"
                );
            }
            assert!(matches!(
                gate(ParkGateFacts {
                    masters_quiet: false,
                    land_gate_relaxed: true,
                    held_for: PRELAUNCH_HOLD_MAX,
                    ..calm(mode, ApplyPhase::Land)
                }),
                ParkGate::StandDown(_)
            ));
        }
    }

    /// Bytes waiting on a master that its reader has not taken hold every
    /// automatic lane in every phase — and at `Land` only until
    /// `PRELAUNCH_LAND_MAX_WAITS` consecutive waits, after which the park
    /// proceeds (the 2026-09-22/23 update audit, plan P1-3). The gate used to
    /// hold at `Land` forever: a producer faster than the parser (`yes`, a
    /// build log) kept `POLLIN` set, every automatic attempt held its successor
    /// the full 120 s and stood down, every ~15 min, for as long as the job
    /// ran. The successor replays kernel-queued bytes after Commit, so parking
    /// over them loses nothing; the earlier phases still prefer a clean
    /// instant, and the count never relaxes them.
    #[test]
    fn unconsumed_master_output_holds_until_the_bound_has_waited_long_enough() {
        for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
            for phase in PHASES {
                for land_waits in [0, super::PRELAUNCH_LAND_MAX_WAITS - 1] {
                    assert!(
                        matches!(
                            gate(ParkGateFacts {
                                masters_quiet: false,
                                land_waits,
                                ..calm(mode, phase)
                            }),
                            ParkGate::Wait(_)
                        ),
                        "{mode:?} {phase:?} after {land_waits} waits"
                    );
                }
                let waited = gate(ParkGateFacts {
                    masters_quiet: false,
                    land_waits: super::PRELAUNCH_LAND_MAX_WAITS,
                    ..calm(mode, phase)
                });
                if phase == ApplyPhase::Land {
                    assert_eq!(
                        waited,
                        ParkGate::Park,
                        "{mode:?}: the bound parks over a master that never goes quiet"
                    );
                } else {
                    assert!(
                        matches!(waited, ParkGate::Wait(_)),
                        "{mode:?} {phase:?}: the count relaxes Land only"
                    );
                }
            }
        }
        // The hold cap still wins over everything: a successor held past it
        // stands down, however many waits it has banked.
        assert!(matches!(
            gate(ParkGateFacts {
                masters_quiet: false,
                land_waits: super::PRELAUNCH_LAND_MAX_WAITS,
                land_gate_relaxed: false,
                held_for: PRELAUNCH_HOLD_MAX,
                ..calm(ApplyMode::AutomaticPastGrace, ApplyPhase::Land)
            }),
            ParkGate::StandDown(_)
        ));
    }
}

/// The 2026-09-24 review of the 2026-09-22/23 update audit fixes, driven
/// through the shipping capture and gate with REAL PTY masters: a parser the
/// park catches mid-sequence over queued output is a timing miss, never a
/// carry whose successor prints the queued tail as text; and a pane kept open
/// after its command exited is not handed, so its hung-up master can neither
/// hold the park to its cap nor fail it.
#[cfg(all(test, unix))]
mod handed_set_and_mid_sequence_tests {
    use super::{
        CaptureFailure, ParkAttempt, ParkGate, ParkGateFacts, classify_capture_failure,
        handed_sessions_only, handoff_masters_closed, handoff_masters_have_activity,
        hold_cap_stand_down_detail, prelaunch_hold_cap, prelaunch_park_admitted, stamp_fg_holders,
    };
    use crate::native_update_auto_intent::{ActivityFacts, ApplyPhase};
    use crate::native_updater_service::ApplyMode;

    fn openpty() -> (i32, i32) {
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
        // openpty(3) opens both ends inheritable: a child another test spawns
        // meanwhile would keep the slave open past its exec, for its whole
        // life, and a closed slave would never read as hung up (the fd-copy
        // sweep of 2026-09-27).
        for fd in [master, slave] {
            aterm_pty::set_cloexec(fd, true).expect("close-on-exec");
        }
        (master, slave)
    }

    fn write_all(fd: i32, bytes: &[u8]) {
        // SAFETY: a bounded write of a live slice to a test-owned descriptor.
        let wrote = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(usize::try_from(wrote).ok(), Some(bytes.len()), "write");
    }

    /// Take every byte queued on `master` — the reader catching up — so the PTY
    /// is quiet again.
    fn drain(master: i32) {
        let entry = [(0u64, master, 0i32)];
        let mut buffer = [0u8; 4096];
        while handoff_masters_have_activity(&entry) {
            // SAFETY: a bounded read into a stack buffer from a readable master.
            let read = unsafe { libc::read(master, buffer.as_mut_ptr().cast(), buffer.len()) };
            assert!(read > 0, "a readable master reads");
        }
    }

    /// The shipping park capture over the shipping handed set, with a window
    /// wide enough that a loaded test machine cannot turn the answer into a
    /// deadline: the ids it carried, or its typed failure.
    fn capture(app: &mut crate::App) -> Result<Vec<u64>, CaptureFailure> {
        capture_for(app, crate::seamless::WireCaps::current())
    }

    /// [`capture`] for a successor with `caps` — an older build's, say.
    fn capture_for(
        app: &mut crate::App,
        caps: crate::seamless::WireCaps,
    ) -> Result<Vec<u64>, CaptureFailure> {
        let live = app.handoff_live_sessions();
        let window = std::time::Duration::from_secs(30);
        app.capture_parked_screens(
            &live,
            std::time::Instant::now() + window,
            window.as_millis(),
            window / 2,
            caps,
            aterm_update_core::handoff_policy::CarryCeiling::Full,
        )
        .map(|parked| parked.screens.iter().map(|(id, _)| *id).collect())
    }

    /// A HOP TO AN OLDER SUCCESSOR CARRIES NO PARSER STATE IT WILL READ. A build
    /// from before the parser carry ignores `CheckpointMeta.parser` and resumes
    /// at Ground, so a reader parked inside `ESC [ 38;5;19` with `6mcompiling`
    /// still queued would have that successor print `6mcompiling`. On such a
    /// hop the capture misses over ANY partial sequence with queued output —
    /// re-parked on a boundary, as a hooked DCS is on every hop — and still
    /// carries the stalled case and the boundary.
    ///
    /// RED before the fix: the capture asked only for the uncarried state, so
    /// the split CSI came back `Ok([0])` for the older successor too.
    #[test]
    fn an_older_successor_misses_a_split_csi_over_queued_output() {
        let running = 200;
        let older = crate::seamless::WireCaps::for_hop(running, running - 1);
        assert!(!older.carries_parser(), "PRECONDITION: an older hop");
        assert!(crate::seamless::WireCaps::for_hop(running, running).carries_parser());
        let mut app = crate::App::headless_for_test();
        let (master, slave) = openpty();
        set_master(&mut app, 0, master);
        let term = app.pool.get(0).expect("session 0").term.clone();
        crate::term_lock(&term).process(b"$ make\r\n\x1b[38;5;19");
        write_all(slave, b"6mcompiling\r\n");
        let failure = capture_for(&mut app, older).expect_err("a split CSI over queued output");
        assert!(
            matches!(failure, CaptureFailure::MidSequence { local_id: 0, .. }),
            "{failure:?}"
        );
        assert!(
            matches!(classify_capture_failure(&failure), ParkAttempt::Missed(_)),
            "timing: re-parked on the next rung, never a refusal"
        );
        assert_eq!(
            capture(&mut app),
            Ok(vec![0]),
            "the same parser is carried whole to a successor that reads it"
        );

        // The stalled case is carried to the older build too (abandoned there).
        drain(master);
        assert_eq!(capture_for(&mut app, older), Ok(vec![0]));
        // And a boundary over a busy PTY is no reason to miss.
        crate::term_lock(&term).process(b"6m");
        write_all(slave, b"compiling\r\n");
        assert_eq!(capture_for(&mut app, older), Ok(vec![0]));

        set_master(&mut app, 0, -1);
        drop(app);
        aterm_pty::close_fd(slave);
        aterm_pty::close_fd(master);
    }

    fn set_master(app: &mut crate::App, id: u64, master: i32) {
        app.pool
            .sessions
            .get_mut(&id)
            .expect("the session is pooled")
            .session
            .master = master;
    }

    /// A reader that parks inside a HOOKED DCS string (a sixel) with the rest
    /// of it still queued is the flood case the Land relaxation parks into.
    /// No carry holds a hooked DCS (its state is its handler's), so carrying it
    /// would abandon the string and the successor would print the rest of the
    /// payload as text; the capture must MISS instead (timing, re-parked on the
    /// next rung). The same parser over a quiet PTY is the stalled case and is
    /// carried; the same queued output at a sequence boundary is carried.
    ///
    /// A reader parked inside `ESC [ 38;5;19` with `6m` still queued — the case
    /// this test was written for — is CARRIED since the parser carry (the
    /// round-five plan's item 12): the checkpoint holds the partial CSI and the
    /// successor finishes it with the queued `6m`.
    ///
    /// RED on the code before the 2026-09-24 fix: the DCS capture came back
    /// `Ok([0])`.
    #[test]
    fn a_parser_parked_mid_sequence_over_queued_output_misses_the_park() {
        let mut app = crate::App::headless_for_test();
        let (master, slave) = openpty();
        set_master(&mut app, 0, master);
        let term = app.pool.get(0).expect("session 0").term.clone();
        crate::term_lock(&term).process(b"$ make\r\n\x1b[38;5;19");
        write_all(slave, b"6mcompiling\r\n");
        assert_eq!(
            capture(&mut app),
            Ok(vec![0]),
            "a split CSI over queued output is carried whole"
        );
        drain(master);
        crate::term_lock(&term).process(b"6m\x1bPq#0;2;0;0;0");
        write_all(slave, b"#0!10~-\x1b\\");

        let failure = capture(&mut app).expect_err("a hooked DCS over queued output");
        assert_eq!(
            failure,
            CaptureFailure::MidSequence {
                local_id: 0,
                state: "DcsPassthrough"
            }
        );
        assert!(
            matches!(classify_capture_failure(&failure), ParkAttempt::Missed(_)),
            "timing: re-parked on the next rung, never a refusal"
        );

        // The reader caught up and the program went quiet mid-sequence: the
        // stalled case, carried by abandoning the partial sequence.
        drain(master);
        assert_eq!(
            capture(&mut app),
            Ok(vec![0]),
            "a stalled sequence is carried"
        );

        // Output queued again, but the parser is at a boundary: nothing to
        // abandon, so queued output is no reason to miss.
        crate::term_lock(&term).process(b"\x1b\\");
        write_all(slave, b"more output\r\n");
        assert_eq!(
            capture(&mut app),
            Ok(vec![0]),
            "a boundary over a busy PTY is carried"
        );

        set_master(&mut app, 0, -1);
        drop(app);
        aterm_pty::close_fd(slave);
        aterm_pty::close_fd(master);
    }

    /// A CARRY THAT WOULD ONLY SWALLOW THE QUEUED TAIL IS A MISS, NOT A CARRY
    /// (round six of the update audit, finding 30). The parser carry holds an
    /// OSC over its 4 KiB cap, an unhooked DCS header and an APC string with
    /// its consumer started only as sequences to IGNORE to their terminator:
    /// parked inside one with the rest of it still queued, the successor would
    /// swallow that rest and dispatch nothing — the kitty image never drawn,
    /// the clipboard never set. Each re-parks on a boundary instead, as a
    /// hooked DCS does. Over a quiet PTY each is still carried (a stalled
    /// sequence may never end); and a short OSC over queued output, which the
    /// carry continues exactly, is the control.
    ///
    /// RED before the fix: every one of them came back `Ok([0])`.
    #[test]
    fn a_carry_that_only_swallows_the_queued_tail_misses_the_park() {
        let mut app = crate::App::headless_for_test();
        let (master, slave) = openpty();
        set_master(&mut app, 0, master);
        let term = app.pool.get(0).expect("session 0").term.clone();
        let mut clipboard = b"\x1b]52;c;".to_vec();
        clipboard.extend(std::iter::repeat_n(b'A', 5000));
        for (at, parked, queued, state) in [
            (
                "a kitty-graphics APC",
                &b"\x1b_Ga=T,f=100;"[..],
                &b"iVBORw0KGgo=\x1b\\"[..],
                "SosPmApcString",
            ),
            (
                "an OSC 52 over the carry's cap",
                &clipboard[..],
                &b"QUFBQQ==\x07"[..],
                "OscString",
            ),
            (
                "an unhooked DCS header",
                &b"\x1bP1;2"[..],
                &b"q#0;2;0;0;0#0!10~-\x1b\\"[..],
                "DcsParam",
            ),
        ] {
            crate::term_lock(&term).process(parked);
            write_all(slave, queued);
            assert_eq!(
                capture(&mut app),
                Err(CaptureFailure::MidSequence { local_id: 0, state }),
                "{at} over queued output"
            );
            // Quiet: the stalled case, carried.
            drain(master);
            assert_eq!(capture(&mut app), Ok(vec![0]), "{at}, stalled, is carried");
            // Back to a boundary for the next case.
            crate::term_lock(&term).process(b"\x18");
        }
        // CONTROL: a short OSC the carry continues exactly is carried over
        // queued output.
        crate::term_lock(&term).process(b"\x1b]0;a ti");
        write_all(slave, b"tle\x07");
        assert_eq!(
            capture(&mut app),
            Ok(vec![0]),
            "a short OSC over queued output is carried whole"
        );
        drain(master);

        set_master(&mut app, 0, -1);
        drop(app);
        aterm_pty::close_fd(slave);
        aterm_pty::close_fd(master);
    }

    /// THE HELD CAPTURE IS BOUNDED BY WHAT CAN CROSS AND STOPS SHORT OF THE
    /// DEADLINE (round six of the update audit, finding 21). One live session
    /// and more exited panes than a handoff carries: the capture projects at
    /// most `MAX_HELD_PANES` of them — the manifest writer would drop the rest
    /// — and, on a budget they cannot all fit in, returns inside it, so the
    /// callers' deadline check after the layout does not turn a landed park
    /// into a miss.
    ///
    /// RED before the fix: every exited pane was projected (70 of them), and
    /// the held capture's only stop was the park's own deadline.
    #[test]
    fn the_held_capture_stops_at_the_held_caps_and_short_of_the_deadline() {
        let mut app = crate::App::headless_for_test();
        let (master, slave) = openpty();
        set_master(&mut app, 0, master);
        let line = "held output ".repeat(24);
        for id in 1..=70_u64 {
            let mut terminal = aterm_core::terminal::Terminal::new(120, 300);
            for n in 0..120 {
                terminal.process(format!("{n:03} {line}\r\n").as_bytes());
            }
            let mut session = crate::stub_session(id);
            session.term = std::sync::Arc::new(std::sync::Mutex::new(terminal));
            crate::App::register_session(&app.store, &session, None);
            app.store
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .set_state(id, crate::session_store::SessionState::Exited);
            app.pool.insert(session);
            app.next_session_id = app.next_session_id.max(id + 1);
        }
        let live = app.handoff_live_sessions();
        assert_eq!(live.len(), 1, "PRECONDITION: only session 0 is handed");

        let window = std::time::Duration::from_secs(30);
        let parked = app
            .capture_parked_screens(
                &live,
                std::time::Instant::now() + window,
                window.as_millis(),
                window / 2,
                crate::seamless::WireCaps::current(),
                aterm_update_core::handoff_policy::CarryCeiling::Full,
            )
            .expect("the park lands");
        assert_eq!(
            parked.held.len(),
            crate::seamless::MAX_HELD_PANES,
            "no pane past the carry's cap is projected inside the freeze"
        );

        // A budget the held panes cannot all fit in: the capture still lands
        // inside it.
        let window = std::time::Duration::from_millis(400);
        let deadline = std::time::Instant::now() + window;
        let parked = app.capture_parked_screens(
            &live,
            deadline,
            window.as_millis(),
            window / 2,
            crate::seamless::WireCaps::current(),
            aterm_update_core::handoff_policy::CarryCeiling::Full,
        );
        assert!(
            parked.is_ok() && std::time::Instant::now() < deadline,
            "the held capture left the park its deadline"
        );

        app.pool.sessions.retain(|id, _| *id == 0);
        set_master(&mut app, 0, -1);
        drop(app);
        aterm_pty::close_fd(slave);
        aterm_pty::close_fd(master);
    }

    /// ENGINE STATE NO CHECKPOINT CARRIES, OVER QUEUED OUTPUT, IS A MISS
    /// (round six of the update audit, finding 47 (b) and (d)). Parked inside
    /// an OSC 8 link with the rest of its text and its close still queued,
    /// the successor — which resumes with no link open — would commit that
    /// text unlinked, to scrollback for good; parked between a VT52 `ESC Y`
    /// and its address bytes, it would print them as text. Each re-parks
    /// instead. Over a quiet PTY each is still carried (a link left open may
    /// never close); and a link already closed is the control.
    ///
    /// RED before the fix: both came back `Ok([0])`.
    #[test]
    fn an_open_link_or_a_pending_vt52_address_over_queued_output_misses_the_park() {
        let mut app = crate::App::headless_for_test();
        let (master, slave) = openpty();
        set_master(&mut app, 0, master);
        let term = app.pool.get(0).expect("session 0").term.clone();
        // The launched lane's attempt, with its free re-parks unspent: the
        // one place an open link still misses (round seven, item 109).
        let (prelaunch, ..) =
            super::late_park_record_tests::prelaunch_record(1, ApplyMode::AutomaticPastGrace);
        app.update_handoff_prelaunch = Some(prelaunch);
        for (at, parked, queued, reset, state) in [
            (
                "an open OSC 8 link",
                &b"\x1b]8;;https://example.com\x07src/ma"[..],
                &b"in.rs\x1b]8;;\x07\r\n"[..],
                &b"\x1b]8;;\x07"[..],
                "Osc8HyperlinkOpen",
            ),
            (
                "a VT52 ESC Y",
                &b"\x1b[?2l\x1bY"[..],
                &b"\x22\x24"[..],
                &b"\x22\x24\x1b<"[..],
                "Vt52CursorAddress",
            ),
        ] {
            crate::term_lock(&term).process(parked);
            write_all(slave, queued);
            assert_eq!(
                capture(&mut app),
                Err(CaptureFailure::MidSequence { local_id: 0, state }),
                "{at} over queued output"
            );
            // Quiet: the stalled case, carried.
            drain(master);
            assert_eq!(capture(&mut app), Ok(vec![0]), "{at}, stalled, is carried");
            crate::term_lock(&term).process(reset);
        }
        // CONTROL: a link opened and closed before the park is no hazard.
        crate::term_lock(&term).process(b"\x1b]8;;https://example.com\x07a\x1b]8;;\x07");
        write_all(slave, b"more output\r\n");
        assert_eq!(
            capture(&mut app),
            Ok(vec![0]),
            "a closed link over queued output is carried"
        );
        drain(master);
        app.update_handoff_prelaunch = None;

        set_master(&mut app, 0, -1);
        drop(app);
        aterm_pty::close_fd(slave);
        aterm_pty::close_fd(master);
    }

    /// AN OPEN LINK THAT NEVER CLOSES DOES NOT HOLD THE UPDATE (round seven,
    /// item 109). A program killed between an OSC 8 open and its close leaves
    /// the link open through every later prompt; a build in that tab keeps
    /// output queued at every park. The miss spends only the launched lane's
    /// free mid-sequence re-parks: once they are spent, and on the fork lane
    /// (no attempt record), the screen is carried and the successor stops
    /// linking the rest. NEGATIVE CONTROL: a VT52 address is still a miss with
    /// the re-parks spent.
    ///
    /// RED before the fix: every capture missed with `Osc8HyperlinkOpen`, so
    /// past the free re-parks two ordinary misses stood the successor down.
    #[test]
    fn an_open_link_past_the_free_reparks_is_carried_not_missed() {
        let mut app = crate::App::headless_for_test();
        let (master, slave) = openpty();
        set_master(&mut app, 0, master);
        let term = app.pool.get(0).expect("session 0").term.clone();
        crate::term_lock(&term).process(b"\x1b]8;;https://example.com/rg\x07src/ma");
        write_all(slave, b"compiling crate 1 of 400\r\n");
        // The fork lane: no attempt record, no free re-parks.
        assert_eq!(capture(&mut app), Ok(vec![0]), "the fork lane carries it");
        let (mut prelaunch, ..) =
            super::late_park_record_tests::prelaunch_record(1, ApplyMode::AutomaticPastGrace);
        prelaunch.park_mid_sequence_reparks = super::PRELAUNCH_MAX_MID_SEQUENCE_REPARKS - 1;
        app.update_handoff_prelaunch = Some(prelaunch);
        assert_eq!(
            capture(&mut app),
            Err(CaptureFailure::MidSequence {
                local_id: 0,
                state: "Osc8HyperlinkOpen"
            }),
            "a free re-park is left: the close may be at the next boundary"
        );
        if let Some(prelaunch) = app.update_handoff_prelaunch.as_mut() {
            prelaunch.park_mid_sequence_reparks = super::PRELAUNCH_MAX_MID_SEQUENCE_REPARKS;
        }
        assert_eq!(
            capture(&mut app),
            Ok(vec![0]),
            "the free re-parks are spent: carried, not a miss the ladder charges"
        );
        // NEGATIVE CONTROL: a VT52 address keeps its hard miss.
        crate::term_lock(&term).process(b"\x1b]8;;\x07\x1b[?2l\x1bY");
        write_all(slave, b"\x22\x24");
        assert_eq!(
            capture(&mut app),
            Err(CaptureFailure::MidSequence {
                local_id: 0,
                state: "Vt52CursorAddress"
            })
        );
        drain(master);
        app.update_handoff_prelaunch = None;

        set_master(&mut app, 0, -1);
        drop(app);
        aterm_pty::close_fd(slave);
        aterm_pty::close_fd(master);
    }

    fn calm_land(live: &[(u64, i32, i32)]) -> ParkGateFacts {
        ParkGateFacts {
            mode: ApplyMode::AutomaticPastGrace,
            phase: ApplyPhase::Land,
            activity: ActivityFacts {
                quiet: true,
                hands_off_keys: true,
                output_quiet: true,
                focused: true,
                consent_warmup: false,
                harness_restored_pending: false,
            },
            masters_quiet: !handoff_masters_have_activity(live),
            masters_alive: !handoff_masters_closed(live),
            land_waits: u8::MAX,
            land_gate_relaxed: false,
            held_for: std::time::Duration::ZERO,
            recording: false,
        }
    }

    /// `aterm --hold`, one pane's command exited: its master hangs up and stays
    /// hung up until a person closes the pane. Both lanes used to hand the
    /// WHOLE pool, so the launched lane's gate waited on that master in every
    /// phase with no bound (held the successor 120 s, stood down as
    /// `ActivityRevoked`, every ~15 min, forever) and the fork lane deferred on
    /// it every 500 ms. The pane is now left out of the handed set: the gate
    /// parks, the capture carries exactly the live sessions, and the manifest
    /// names exactly them.
    ///
    /// The CONTROL below is the old behaviour, reproduced: the whole pool, fed
    /// to the same gate, still waits.
    #[test]
    fn an_exited_held_pane_is_not_handed_and_does_not_hold_the_park() {
        let mut app = crate::App::headless_for_test();
        app.hold = true;
        let (exited_master, exited_slave) = openpty();
        write_all(exited_slave, b"build finished\r\n");
        aterm_pty::close_fd(exited_slave);
        let probe = [(1u64, exited_master, 0i32)];
        // 10 s, not 2: the hang-up arrives only once every copy of the slave is
        // closed, and a child another test is forking holds one until it execs
        // (the fd-copy sweep of 2026-09-27). A slave that never closes still fails.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !handoff_masters_closed(&probe) {
            assert!(
                std::time::Instant::now() < deadline,
                "the exited slave hung up"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let held = crate::stub_session(1);
        // Never signal a pid this test does not own on teardown.
        held.child_reaped
            .store(true, std::sync::atomic::Ordering::Release);
        let mut held = held;
        held.master = exited_master;
        let _ = app.insert_logical_window(held, 24, 80);
        // What the reader's EOF does under `--hold`: the registry marks the
        // session exited and the pane stays.
        app.store
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_state(1, crate::session_store::SessionState::Exited);

        let whole: Vec<(u64, i32, i32)> = app
            .pool
            .iter()
            .map(|session| (session.id, session.master, session.pid))
            .collect();
        assert!(
            matches!(
                prelaunch_park_admitted(
                    calm_land(&whole),
                    prelaunch_hold_cap(ApplyMode::AutomaticPastGrace)
                ),
                ParkGate::Wait(_)
            ),
            "CONTROL: the whole pool, as both lanes handed it, holds the park even at Land \
             past every bound"
        );

        let live = app.handoff_live_sessions();
        assert_eq!(
            live.iter().map(|(id, _, _)| *id).collect::<Vec<_>>(),
            vec![0],
            "the exited pane is not handed"
        );
        assert_eq!(
            prelaunch_park_admitted(
                calm_land(&live),
                prelaunch_hold_cap(ApplyMode::AutomaticPastGrace)
            ),
            ParkGate::Park,
            "and the handed set parks"
        );
        assert_eq!(
            capture(&mut app),
            Ok(vec![0]),
            "the capture carries the live set"
        );
        let manifest = handed_sessions_only(
            crate::session_store::SessionHandoff::from_store(
                &app.store
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            ),
            &live,
        );
        assert!(
            manifest.sessions.iter().all(|record| record.local_id != 1),
            "the manifest names only handed sessions"
        );

        set_master(&mut app, 1, -1);
        drop(app);
        aterm_pty::close_fd(exited_master);
    }

    /// THE FOREGROUND HOLDER CROSSES AT THE REPAINT RUNG (the 2026-09-25
    /// foreground handback review). A session whose screen the park carries
    /// blank goes without the control carry, yet the successor restores its
    /// modes. The first cut carried the holder only in that control carry, so
    /// such a session reached the adopting reader with holder `0`, the reader
    /// probed the reclaimed shell, and a job that died during the handoff kept
    /// its alt screen and mouse under the prompt. The park capture now reads
    /// every handed session's holder, whatever its rung, and the lanes stamp
    /// it on the manifest record.
    ///
    /// RED on the reviewed design: the capture had no `fg_holders`, and the
    /// holder rode `carries`, which is empty here.
    ///
    /// Carried blank by the successor's handoff-policy ceiling (`carry =
    /// "repaint"`, plan P0-5), which puts any screen at the Repaint rung. The
    /// link-dense line this test first used no longer does: since plan P2-4 it
    /// is carried at the StrippedLinks rung, screen and control carry intact.
    #[test]
    fn a_repaint_rung_session_still_carries_its_foreground_holder() {
        let mut app = crate::App::headless_for_test();
        let session = app.pool.get(0).expect("session 0");
        *crate::term_lock(&session.term) = aterm_core::terminal::Terminal::new(24, 80);
        crate::term_lock(&session.term).process(b"$ vim notes.txt\r\n");
        session
            .fg_holder
            .store(4242, std::sync::atomic::Ordering::Release);

        let live = app.handoff_live_sessions();
        let window = std::time::Duration::from_secs(30);
        let parked = app
            .capture_parked_screens(
                &live,
                std::time::Instant::now() + window,
                window.as_millis(),
                window / 2,
                crate::seamless::WireCaps::current(),
                aterm_update_core::handoff_policy::CarryCeiling::Repaint,
            )
            .unwrap_or_else(|failure| panic!("the desk parks: {failure}"));
        assert_eq!(parked.repaint, vec![0], "carried blank");
        assert!(
            parked.carries.is_empty(),
            "a blank carry goes without the control carry"
        );
        assert_eq!(
            parked.fg_holders,
            vec![(0, 4242)],
            "but its foreground holder is read all the same"
        );

        let manifest = stamp_fg_holders(
            handed_sessions_only(
                crate::session_store::SessionHandoff::from_store(
                    &app.store
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                ),
                &live,
            ),
            &parked.fg_holders,
        );
        assert_eq!(manifest.sessions.len(), 1);
        assert_eq!(
            manifest.sessions[0].fg_holder,
            Some(4242),
            "and rides the manifest record"
        );
        let wire = manifest.to_toml().expect("serialize");
        let read =
            crate::session_store::SessionHandoff::from_toml(&wire).expect("parse the manifest");
        assert_eq!(read.sessions[0].fg_holder, Some(4242), "round trip");

        // A holder that is no pgid (nothing seen yet) is not written.
        let none = stamp_fg_holders(manifest.clone(), &[(0, 0)]);
        assert_eq!(none.sessions[0].fg_holder, None);
        assert!(!none.to_toml().expect("serialize").contains("fg_holder"));
        let absent = stamp_fg_holders(manifest, &[(7, 4242)]);
        assert_eq!(absent.sessions[0].fg_holder, None, "another session's");
    }

    /// A hold-cap stand-down says what held the park — the ledger used to read
    /// "the terminal never offered a moment to pause in" for an exited pane's
    /// master, blaming the user.
    #[test]
    fn a_hold_cap_stand_down_names_what_held_the_park() {
        let held_for = std::time::Duration::from_secs(120);
        assert_eq!(
            hold_cap_stand_down_detail(
                "the terminal never offered a moment to pause in within the hold cap",
                held_for,
                Some("a session has output waiting that its reader has not taken"),
            ),
            "the terminal never offered a moment to pause in within the hold cap (120 s after \
             the successor dialled; the park was last held back because a session has output \
             waiting that its reader has not taken)"
        );
        assert_eq!(
            hold_cap_stand_down_detail("capped", held_for, None),
            "capped (120 s after the successor dialled)"
        );
    }
}

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
mod freeze_budget_tests {
    use super::{FreezeSeed, handoff_freeze_budget};
    use crate::native_updater_service::ApplyMode;
    use std::time::Duration;

    /// The first automatic attempt keeps the imperceptible 20 ms; a physical failure of
    /// the same bytes buys a wider window, then the widest; an explicit apply gets the
    /// widest at once. A monotone ladder: more failures never buy LESS time.
    #[test]
    fn freeze_budget_widens_on_retries_and_for_an_explicit_apply() {
        assert_eq!(
            handoff_freeze_budget(ApplyMode::Automatic, 0, FreezeSeed::Default),
            Duration::from_millis(20)
        );
        assert_eq!(
            handoff_freeze_budget(ApplyMode::AutomaticPastGrace, 0, FreezeSeed::Default),
            Duration::from_millis(20)
        );
        assert_eq!(
            handoff_freeze_budget(ApplyMode::Automatic, 1, FreezeSeed::Default),
            Duration::from_millis(80)
        );
        assert_eq!(
            handoff_freeze_budget(ApplyMode::Automatic, 2, FreezeSeed::Default),
            Duration::from_millis(250)
        );
        assert_eq!(
            handoff_freeze_budget(ApplyMode::AutomaticPastGrace, 7, FreezeSeed::Default),
            Duration::from_millis(250)
        );
        assert_eq!(
            handoff_freeze_budget(ApplyMode::Immediate, 0, FreezeSeed::Default),
            Duration::from_millis(250)
        );
        let mut last = Duration::ZERO;
        for prior in 0..=u8::MAX {
            let now = handoff_freeze_budget(ApplyMode::Automatic, prior, FreezeSeed::Default);
            assert!(
                now >= last,
                "the ladder must be monotone: {prior} prior failures gave {now:?} after {last:?}"
            );
            last = now;
        }
    }

    /// RUNG 0 IS SEEDED FROM A TIMED CAPTURE (gap #25): `clamp(1.5 × capture,
    /// 20 ms, 80 ms)`. A desk that captures in 22 ms — what 0.91's park took on
    /// the owner's desk once it had the room — gets 33 ms at once instead of
    /// missing 20 ms, re-parking half a second later and parking again. Nothing
    /// measured, or a measurement under 14 ms, is the 20 ms it always was.
    #[test]
    fn rung_zero_is_seeded_from_the_timed_capture_within_its_clamp() {
        use super::{FREEZE_RUNG0_CEILING, FREEZE_RUNG0_FLOOR, freeze_rung0_from_capture};
        let ms = Duration::from_millis;
        for (capture, rung0) in [
            (0, 20),
            (12, 20),
            (14, 21),
            (20, 30),
            (22, 33),
            (40, 60),
            (52, 78),
            (54, 80),
            (100, 80),
        ] {
            assert_eq!(
                freeze_rung0_from_capture(ms(capture)),
                ms(rung0),
                "a {capture} ms capture"
            );
        }
        assert_eq!(
            freeze_rung0_from_capture(Duration::MAX),
            FREEZE_RUNG0_CEILING,
            "an absurd reading saturates, never wraps"
        );
        let seed = FreezeSeed::DryRun {
            capture: ms(22),
            sessions: 2,
            last_park: None,
        };
        for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
            assert_eq!(handoff_freeze_budget(mode, 0, seed), ms(33), "{mode:?}");
            assert_eq!(handoff_freeze_budget(mode, 1, seed), ms(80), "{mode:?}");
            assert_eq!(handoff_freeze_budget(mode, 2, seed), ms(250), "{mode:?}");
        }
        assert_eq!(
            handoff_freeze_budget(ApplyMode::Immediate, 0, seed),
            ms(250),
            "an explicit apply's window is the widest whatever rung 0 is"
        );
        assert_eq!(FreezeSeed::Default.rung0(), FREEZE_RUNG0_FLOOR);
    }

    /// WHATEVER THE SEED, THE LADDER STAYS A LADDER: every rung strictly wider
    /// than the one before (a re-park never buys the same window twice), rung 0
    /// inside its clamp, rung 1 never under the 80 ms it always was, the top a
    /// quarter second.
    #[test]
    fn every_seed_keeps_the_ladder_strictly_wider_rung_over_rung() {
        use super::{FREEZE_RUNG0_CEILING, FREEZE_RUNG0_FLOOR};
        for capture_ms in 0..=200 {
            let seed = FreezeSeed::Ledger {
                capture: Duration::from_millis(capture_ms),
                build: 1,
            };
            let rung = |prior| handoff_freeze_budget(ApplyMode::Automatic, prior, seed);
            assert!(
                (FREEZE_RUNG0_FLOOR..=FREEZE_RUNG0_CEILING).contains(&rung(0)),
                "{capture_ms} ms: rung 0 {:?}",
                rung(0)
            );
            assert!(
                rung(0) < rung(1) && rung(1) < rung(2),
                "{capture_ms} ms: {:?} {:?} {:?}",
                rung(0),
                rung(1),
                rung(2)
            );
            assert!(rung(1) >= Duration::from_millis(80), "{capture_ms} ms");
            assert_eq!(rung(2), Duration::from_millis(250), "{capture_ms} ms");
        }
    }

    /// The ledger's record is a PRIOR: no record is the default, a record seeds
    /// rung 0 from its capture and names the build that took it.
    #[test]
    fn the_ledgers_last_timed_capture_seeds_a_lane_that_times_none() {
        assert_eq!(FreezeSeed::from_prior(None), FreezeSeed::Default);
        let record = aterm_update::HandoffCaptureRecord {
            capture_us: 22_000,
            sessions: 2,
            freeze_seed_ms: 33,
            build: 1_790_278_596,
            at: "2026-09-26T00:00:00Z".to_string(),
            park_us: 0,
            park_build: 0,
        };
        let seed = FreezeSeed::from_prior(Some(&record));
        assert_eq!(
            seed,
            FreezeSeed::Ledger {
                capture: Duration::from_millis(22),
                build: 1_790_278_596
            }
        );
        assert_eq!(seed.rung0(), Duration::from_millis(33));
        assert_eq!(FreezeSeed::last_park_of(Some(&record)), None);
        let said = seed.to_string();
        assert!(
            said.contains("22.0 ms") && said.contains("1790278596"),
            "the log line names the number and the build it came from: {said}"
        );
        // Neither measured: the default, not a zero-millisecond "measurement".
        let empty = aterm_update::HandoffCaptureRecord::default();
        assert_eq!(FreezeSeed::from_prior(Some(&empty)), FreezeSeed::Default);
    }

    /// THE PARK THE DRY RUN CANNOT TIME. The capture itself is about a
    /// millisecond on the owner's two-session desk (1.1 ms measured at
    /// opt-level 3), while 0.91's and 0.92's parks took 22 and 20 ms: most of
    /// the freeze is stopping the readers, which only a real park measures. So
    /// a landed park's cost is kept, and rung 0 is never budgeted below it —
    /// from the ledger alone (the fork lane) or beside this attempt's dry run.
    #[test]
    fn a_landed_parks_cost_is_the_floor_the_dry_run_cannot_see() {
        let ms = Duration::from_millis;
        let beside = |capture, last_park| FreezeSeed::DryRun {
            capture,
            sessions: 2,
            last_park,
        };
        assert_eq!(beside(ms(1), None).rung0(), ms(20), "the capture alone");
        assert_eq!(
            beside(ms(1), Some(ms(22))).rung0(),
            ms(33),
            "0.91's landed park: the next first rung fits it"
        );
        assert_eq!(
            beside(ms(40), Some(ms(22))).rung0(),
            ms(60),
            "whichever is larger"
        );
        let record = aterm_update::HandoffCaptureRecord {
            capture_us: 1_100,
            sessions: 2,
            freeze_seed_ms: 20,
            build: 8,
            at: String::new(),
            park_us: 22_000,
            park_build: 9,
        };
        assert_eq!(
            FreezeSeed::from_prior(Some(&record)),
            FreezeSeed::Ledger {
                capture: ms(22),
                build: 9
            },
            "the ledger seeds from the larger, named by the build that recorded it"
        );
        assert_eq!(FreezeSeed::last_park_of(Some(&record)), Some(ms(22)));
        let said = beside(ms(1), Some(ms(22))).to_string();
        assert!(said.contains("22.0 ms"), "{said}");
        let alone = beside(ms(1), None).to_string();
        assert!(alone.contains("no landed park"), "{alone}");
    }
}

/// The dry run of the park's capture (gap #25), over the shipping capture and
/// the shipping handed set: what it measures, what it saturates on, what it
/// cannot measure — and that it parks nothing.
#[cfg(all(test, unix))]
mod dry_run_capture_tests {
    use super::{DRY_RUN_CAPTURE_WINDOW, DryRunCapture, FREEZE_RUNG0_CEILING, FreezeSeed};
    use crate::native_updater_service::ApplyMode;
    use std::time::Duration;

    fn caps() -> crate::seamless::WireCaps {
        crate::seamless::WireCaps::current()
    }

    /// A desk with history to carry, through the real parser.
    fn desk_with_history(
        app: &crate::App,
    ) -> std::sync::Arc<std::sync::Mutex<aterm_core::terminal::Terminal>> {
        let term = app.pool.get(0).expect("session 0").term.clone();
        let mut terminal = crate::term_lock(&term);
        for n in 0..400 {
            terminal.process(format!("line {n}: the history a capture carries\r\n").as_bytes());
        }
        drop(terminal);
        term
    }

    /// THE DRY RUN TIMES THE PARK'S OWN CAPTURE AND PARKS NOTHING. The reader
    /// a park would stop is still attached, no attempt record exists, the
    /// engine is free again and unchanged — and the measurement is over every
    /// handed session.
    #[test]
    fn a_dry_run_times_the_parks_own_capture_and_parks_nothing() {
        let mut app = crate::App::headless_for_test();
        let term = desk_with_history(&app);
        let (scrollback, cursor) = {
            let terminal = crate::term_lock(&term);
            (terminal.grid().scrollback_lines(), terminal.cursor())
        };
        app.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .reader_join = Some(std::thread::spawn(|| {}));
        let handed = app.handoff_live_sessions().len();

        let dry = app.dry_run_handoff_capture(caps());
        let (DryRunCapture::Measured { took, sessions }
        | DryRunCapture::AtLeast { took, sessions }) = dry
        else {
            panic!("a quiet desk at parser ground is measured: {dry:?}");
        };
        assert_eq!(sessions, handed, "every handed session was captured");
        assert!(took > Duration::ZERO);

        assert!(
            app.pool.sessions[&0].session.reader_join.is_some(),
            "no reader was parked (`park_reader` takes this handle)"
        );
        assert!(app.pending_update_handoff.is_none() && app.update_handoff_prelaunch.is_none());
        let terminal = term.try_lock().expect("the engine is free again");
        assert_eq!(terminal.grid().scrollback_lines(), scrollback);
        assert_eq!(terminal.cursor(), cursor, "and read, not written");
    }

    /// A DRY RUN THAT RUNS OUT OF ITS WINDOW SATURATES THE RUNG. An engine lock
    /// held for the whole window — here by this thread, which is what a reader
    /// slice longer than the window would do — ends it as `EngineBusy` at the
    /// window's end, and 1.5 × the window is the ceiling: a desk that cannot be
    /// timed inside 53 ms gets the 80 ms rung 0, never the 20 ms it would miss.
    #[test]
    fn a_dry_run_that_runs_out_of_its_window_saturates_the_first_rung() {
        let mut app = crate::App::headless_for_test();
        let term = desk_with_history(&app);
        let held = crate::term_lock(&term);
        let dry = app.dry_run_handoff_capture(caps());
        drop(held);
        let DryRunCapture::AtLeast { took, sessions } = dry else {
            panic!("an engine held for the whole window is not measured: {dry:?}");
        };
        assert!(took >= DRY_RUN_CAPTURE_WINDOW, "{took:?}");
        assert_eq!(sessions, app.handoff_live_sessions().len());
        assert_eq!(
            FreezeSeed::DryRun {
                capture: took,
                sessions,
                last_park: None,
            }
            .rung0(),
            FREEZE_RUNG0_CEILING
        );
    }

    /// THE SEED, END TO END, over the shipping ledger writers: an automatic
    /// attempt's measured dry run seeds rung 0 — never below the last park that
    /// landed — and is kept in the ledger for the next update (read back once
    /// the writer has finished); an explicit apply spends no dry run; and a dry
    /// run that measured nothing — a parser mid-sequence over live output —
    /// falls back to that ledger record.
    #[test]
    fn the_first_rung_is_seeded_from_the_dry_run_and_kept_for_the_next_update() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = crate::App::headless_for_test();
        let term = desk_with_history(&app);

        let (seed, kept) = app.seed_first_freeze_rung(ApplyMode::Immediate, caps());
        assert_eq!(
            seed,
            FreezeSeed::Default,
            "an explicit apply spends no dry run"
        );
        assert!(kept.is_none());

        // A park that LANDED earlier in 30 ms: the part of the freeze the dry
        // run cannot time. The rung is never budgeted below it.
        super::keep_landed_park(Duration::from_millis(30))
            .expect("the park cost is kept")
            .join()
            .expect("the ledger writer finished");
        let (seed, kept) = app.seed_first_freeze_rung(ApplyMode::Automatic, caps());
        let FreezeSeed::DryRun {
            capture,
            sessions,
            last_park,
        } = seed
        else {
            panic!("a quiet desk seeds from its own dry run: {seed:?}");
        };
        assert_eq!(sessions, app.handoff_live_sessions().len());
        assert_eq!(
            seed.rung0(),
            super::freeze_rung0_from_capture(capture.max(last_park.unwrap_or_default()))
        );
        kept.expect("the measurement is kept")
            .join()
            .expect("the ledger writer finished");
        let prior = aterm_update::handoff_capture_prior();
        if cfg!(target_os = "macos") {
            assert_eq!(last_park, Some(Duration::from_millis(30)));
            assert!(
                seed.rung0() >= Duration::from_millis(45),
                "a 30 ms landed park buys at least 45 ms: {seed}"
            );
            let prior = prior.expect("the ledger holds the measurement");
            assert_eq!(
                prior.capture_us,
                u64::try_from(capture.as_micros()).expect("fits").max(1)
            );
            assert_eq!(
                u128::from(prior.freeze_seed_ms),
                seed.rung0().as_millis(),
                "and the rung it seeded"
            );
            assert_eq!(prior.build, crate::running_build_number());
            assert_eq!(
                (prior.park_us, prior.park_build),
                (30_000, crate::running_build_number())
            );
        } else {
            assert_eq!(last_park, None, "only macOS keeps an apply-lane ledger");
            assert!(prior.is_none(), "only macOS keeps an apply-lane ledger");
        }

        // The parser parks mid-sequence with the rest of the sequence queued:
        // the dry run cannot say how long a capture takes.
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
        // openpty(3) opens both ends inheritable: a child another test spawns
        // meanwhile would keep the slave open past its exec, for its whole
        // life, and a closed slave would never read as hung up (the fd-copy
        // sweep of 2026-09-27).
        for fd in [master, slave] {
            aterm_pty::set_cloexec(fd, true).expect("close-on-exec");
        }
        app.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .master = master;
        // A hooked DCS string over queued output: the one partial sequence no
        // carry holds (a split CSI is carried since the round-five plan's item
        // 12), so the capture misses.
        crate::term_lock(&term).process(b"\x1bPq#0;2;0;0;0");
        // SAFETY: a bounded write of a live slice to a test-owned descriptor.
        let wrote = unsafe { libc::write(slave, b"#0!9~".as_ptr().cast(), 5) };
        assert_eq!(wrote, 5);
        assert!(matches!(
            app.dry_run_handoff_capture(caps()),
            DryRunCapture::Unmeasured(_)
        ));
        let (fallback, kept) = app.seed_first_freeze_rung(ApplyMode::Automatic, caps());
        assert!(kept.is_none(), "nothing measured, nothing kept");
        assert_eq!(
            fallback,
            FreezeSeed::from_prior(aterm_update::handoff_capture_prior().as_ref())
        );
        if cfg!(target_os = "macos") {
            assert_eq!(
                fallback.rung0(),
                seed.rung0(),
                "the fallback is the measurement the ledger kept"
            );
        }
        // SAFETY: closing the two test-owned descriptors exactly once.
        unsafe {
            libc::close(slave);
            libc::close(master);
        }
        app.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .master = -1;
    }

    /// THE PARK READS THE SEED ITS ATTEMPT CARRIES: the launched lane's budget
    /// for its next park is rung 0 = the dry run's seed, then the ladder's next
    /// rungs as the attempt's own misses climb it.
    #[test]
    fn the_launched_lanes_park_budget_is_its_attempts_seeded_ladder() {
        let mut app = crate::App::headless_for_test();
        assert!(app.prelaunched_park_budget().is_none());
        let seed = FreezeSeed::DryRun {
            capture: Duration::from_millis(30),
            sessions: 1,
            last_park: None,
        };
        let (record, _transferred) = prelaunched(1, ApplyMode::Automatic, seed);
        app.update_handoff_prelaunch = Some(record);
        for (misses, budget_ms) in [(0_u8, 45_u64), (1, 90), (2, 250)] {
            app.update_handoff_prelaunch
                .as_mut()
                .expect("prelaunched")
                .park_misses = misses;
            assert_eq!(
                app.prelaunched_park_budget(),
                Some((misses, Duration::from_millis(budget_ms), seed)),
                "miss {misses}"
            );
        }
    }

    /// A prelaunched attempt as `prelaunch_out_of_band_handoff` records it,
    /// carrying `freeze_seed`, and the receiver its park sends the capture to.
    fn prelaunched(
        attempt_id: u64,
        mode: ApplyMode,
        freeze_seed: FreezeSeed,
    ) -> (
        crate::HandoffPrelaunch,
        std::sync::mpsc::Receiver<super::HandoffTransferJob>,
    ) {
        let (transfer, transferred) = std::sync::mpsc::sync_channel(1);
        let record = crate::HandoffPrelaunch {
            attempt_id,
            nonce: "0123456789abcdef0123456789abcdef".to_string(),
            mode,
            apply_attempt: None,
            same_image: Some(super::SameImageHandoff::DebugSeam),
            target_build: 7,
            target_commit: String::new(),
            cancel: std::sync::mpsc::sync_channel(1).0,
            stand_down: std::sync::mpsc::sync_channel(1).0,
            stand_down_ack: std::sync::mpsc::sync_channel(1).0,
            transfer,
            arbiter: crate::HandoffAttemptArbiter::new(),
            launched_at: std::time::Instant::now(),
            dialled: None,
            park_retry_at: None,
            park_misses: 0,
            park_mid_sequence_reparks: 0,
            freeze_seed,
            land_waits: 0,
            last_wait: None,
            stood_down: false,
            teardown: crate::DeferredHandoffTeardown::None,
            revoked_by_activity: false,
            history_export: None,
        };
        (record, transferred)
    }

    /// THE SEED LANDS ON THE ATTEMPT ITS PARKS READ: seeding a prelaunched
    /// attempt stores what it bought on that attempt, so the park's budget is
    /// the seeded ladder and not the 20 ms default the record was born with.
    #[test]
    fn seeding_stores_the_first_rung_on_the_prelaunched_attempt() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = crate::App::headless_for_test();
        let _term = desk_with_history(&app);
        let (record, _transferred) = prelaunched(2, ApplyMode::Automatic, FreezeSeed::Default);
        app.update_handoff_prelaunch = Some(record);
        let (seed, kept) = app.seed_first_freeze_rung(ApplyMode::Automatic, caps());
        if let Some(writer) = kept {
            writer.join().expect("the ledger writer finished");
        }
        assert!(
            matches!(seed, FreezeSeed::DryRun { .. }),
            "a quiet desk seeds from its own dry run: {seed:?}"
        );
        assert_eq!(
            app.prelaunched_park_budget(),
            Some((0, seed.rung0(), seed)),
            "the park reads the seed the attempt was given"
        );
    }

    /// A PARK THAT LANDS KEEPS ITS COST (gap #25). The launched lane's real
    /// park — through the shipping park cue, over a real PTY — writes what it
    /// cost into the ledger's landed-park slot: the floor the next update's
    /// first rung is budgeted from, and the one input that covers the readers'
    /// stop the dry run cannot time. The slot is written by nothing else, so
    /// this fails if the park stops keeping it.
    #[test]
    fn a_landed_park_keeps_its_cost_for_the_next_updates_first_rung() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        // A sentinel no real park costs, written and joined first: the wait
        // below is for THIS park's write, not an earlier one.
        super::keep_landed_park(Duration::from_micros(1))
            .expect("the sentinel is kept")
            .join()
            .expect("the ledger writer finished");
        let mut app = crate::App::headless_for_test();
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
        // openpty(3) opens both ends inheritable: a child another test spawns
        // meanwhile would keep the slave open past its exec, for its whole
        // life, and a closed slave would never read as hung up (the fd-copy
        // sweep of 2026-09-27).
        for fd in [master, slave] {
            aterm_pty::set_cloexec(fd, true).expect("close-on-exec");
        }
        app.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .master = master;
        let (record, transferred) = prelaunched(3, ApplyMode::Immediate, FreezeSeed::Default);
        app.update_handoff_prelaunch = Some(record);

        app.on_update_handoff_awaiting_park(3, Some(4242), None);
        let transfer = transferred
            .try_recv()
            .expect("the park landed and handed its capture over");
        assert!(
            app.pending_update_handoff.is_some(),
            "the attempt is parked"
        );
        if cfg!(target_os = "macos") {
            // The writer runs detached (the park never waits on the ledger's
            // file lock): wait for its write, the causal event, bounded only
            // so a park that never keeps its cost fails instead of hanging.
            let patience = std::time::Instant::now() + Duration::from_secs(10);
            let kept = loop {
                let prior = aterm_update::handoff_capture_prior().expect("the sentinel is held");
                if prior.park_us != 1 {
                    break prior;
                }
                assert!(
                    std::time::Instant::now() < patience,
                    "the landed park's cost never reached the ledger"
                );
                std::thread::sleep(Duration::from_millis(1));
            };
            assert!(
                kept.park_us > 1 && kept.park_us < 250_000,
                "a park that landed inside its 250 ms rung: {kept:?}"
            );
            assert_eq!(kept.park_build, crate::running_build_number());
        } else {
            assert!(
                aterm_update::handoff_capture_prior().is_none(),
                "only macOS keeps an apply-lane ledger"
            );
        }

        // The capture's descriptors are duplicates it owns; the session's are
        // the test's, closed once, here.
        drop(transfer);
        app.pending_update_handoff = None;
        app.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .master = -1;
        // SAFETY: closing the two test-owned descriptors exactly once.
        unsafe {
            libc::close(slave);
            libc::close(master);
        }
    }

    /// Session 0 on a real, quiet, close-on-exec PTY, as the park needs it (a
    /// stub's `-1` master reads as activity). Returns `(master, slave)`,
    /// which [`release_pty`] hands back.
    fn quiet_pty(app: &mut crate::App) -> (i32, i32) {
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
        for fd in [master, slave] {
            aterm_pty::set_cloexec(fd, true).expect("close-on-exec");
        }
        app.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .master = master;
        (master, slave)
    }

    fn release_pty(app: &mut crate::App, (master, slave): (i32, i32)) {
        app.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .master = -1;
        // SAFETY: closing the two test-owned descriptors exactly once.
        unsafe {
            libc::close(slave);
            libc::close(master);
        }
    }

    /// Publish a PASSED pre-verification of `ticket`'s artifact carrying
    /// `policy` (`None`: a pass whose bundle has no policy file), as the
    /// handoff worker publishes it, and return the policy the park will read.
    fn publish(
        app: &crate::App,
        ticket: &crate::native_updater_service::ApplyAttemptTicket,
        policy: Option<&str>,
    ) -> Option<aterm_update_core::handoff_policy::HandoffPolicy> {
        use aterm_update_core::handoff_policy::{HandoffPolicy, PolicyRead};
        let read = policy.map_or(PolicyRead::Absent, |text| {
            PolicyRead::Parsed(HandoffPolicy::parse(text).expect("a schema-1 policy"))
        });
        app.publish_preverified_pass_for_test(ticket, &read)
    }

    /// `extra` more live sessions (ids 1..=extra) beside session 0, each on its
    /// own duplicate of session 0's quiet master (so the park's master checks
    /// read the same quiet PTY), registered and pooled as spawned tabs are.
    /// Each session owns its duplicate, and its drop closes it.
    fn add_sessions_on(app: &mut crate::App, master: i32, extra: u64) {
        for id in 1..=extra {
            // SAFETY: `master` is the test's open PTY master; `dup` returns a
            // new descriptor this session owns.
            let dup = unsafe { libc::dup(master) };
            assert!(dup >= 0, "dup the quiet master");
            aterm_pty::set_cloexec(dup, true).expect("close-on-exec");
            let mut session = crate::stub_session(id);
            session.master = dup;
            crate::App::register_session(&app.store, &session, None);
            app.pool.insert(session);
            app.next_session_id = app.next_session_id.max(id + 1);
        }
    }

    /// Publish a PASS of `ticket`'s artifact that declares the chunked grant,
    /// as the arm-time pre-verification caches it for a chunks-capable
    /// candidate.
    fn publish_chunked_pass(
        app: &crate::App,
        ticket: &crate::native_updater_service::ApplyAttemptTicket,
    ) {
        super::PreverifyPublisher::for_attempt(&app.handoff_preverified, Some(ticket))
            .expect("an attempt names its artifact")
            .publish_pass(
                &aterm_update::HandoffCandidateFacts {
                    policy: aterm_update_core::handoff_policy::PolicyRead::Absent,
                    grant_chunks: true,
                },
                crate::running_build_number(),
                "the test candidate",
            );
    }

    /// THE PARK HOLDS THE POOL TO THE DIALLED CLAIM, BEFORE IT FREEZES (round six
    /// of the update audit, item 1). A fresh cached pass says the candidate
    /// declares the chunked grant, but the successor holding the rendezvous
    /// claimed `ATRZ1C` — it was never offered the grant, which is what a worker
    /// that skipped its own verification used to launch it with. `transfer` will
    /// refuse 63 sessions to that claim, so the park must refuse them first,
    /// with every reader live and nothing captured.
    ///
    /// RED before the fix: the park read the cached pass's `grant_chunks`,
    /// admitted 63 sessions, froze every reader and handed the capture over —
    /// and `transfer` then answered `TooManySessions` after the freeze.
    #[test]
    fn a_park_refuses_a_desk_the_dialled_claim_cannot_carry_before_freezing() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = crate::App::headless_for_test();
        let pty = quiet_pty(&mut app);
        add_sessions_on(&mut app, pty.0, 62);
        assert_eq!(app.handoff_live_sessions().len(), 63, "PRECONDITION");
        let ticket =
            crate::native_updater_service::ApplyAttemptTicket::for_test(7, "", &"ab".repeat(32));
        publish_chunked_pass(&app, &ticket);
        assert!(
            app.park_policy(Some(&ticket)).grant_chunks,
            "PRECONDITION — the cached pass declares the chunked grant"
        );
        let (mut record, transferred) = prelaunched(21, ApplyMode::Immediate, FreezeSeed::Default);
        record.apply_attempt = Some(ticket);
        app.update_handoff_prelaunch = Some(record);

        app.update_handoff_prelaunch
            .as_mut()
            .expect("prelaunched")
            .dialled = Some(crate::DialledSuccessor {
            pid: Some(4242),
            // What an `ATRZ1C` claim carries.
            grant_limit: Some(62),
            at: std::time::Instant::now(),
        });
        let attempt = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
        let super::ParkAttempt::Failed(stand_down) = attempt else {
            panic!("the park must refuse a desk the claim cannot carry: {attempt:?}");
        };
        assert_eq!(
            stand_down.outcome,
            crate::UpdateHandoffOutcome::ProducerFailed
        );
        assert!(
            stand_down.detail.contains("descriptor grant carries"),
            "{}",
            stand_down.detail
        );
        assert!(transferred.try_recv().is_err(), "nothing was captured");
        assert!(app.pending_update_handoff.is_none(), "no reader was parked");
        app.update_handoff_prelaunch = None;
        app.pool.sessions.retain(|id, _| *id == 0);
        release_pty(&mut app, pty);
    }

    /// A FORK HAS NO DESCRIPTOR-MESSAGE LIMIT (round six of the update audit,
    /// item 38). The launched lane refused at runtime (`fork_after_park`, which
    /// cues the park with no dialer and no grant limit), and the person opened a
    /// 63rd tab during the hold: the attempt that forks passes its masters by
    /// inheritance, so the park lands and hands the fork its capture.
    ///
    /// RED before the fix: the park refused it as "more sessions opened than one
    /// descriptor message carries" — a transport this attempt no longer used —
    /// and the attempt stood down and backed off.
    #[test]
    fn a_fork_after_park_is_not_held_to_the_rendezvous_session_limit() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        super::keep_landed_park(Duration::from_micros(1))
            .expect("the sentinel is kept")
            .join()
            .expect("the ledger writer finished");
        let mut app = crate::App::headless_for_test();
        let pty = quiet_pty(&mut app);
        add_sessions_on(&mut app, pty.0, 62);
        assert_eq!(app.handoff_live_sessions().len(), 63, "PRECONDITION");
        let (record, transferred) = prelaunched(22, ApplyMode::Immediate, FreezeSeed::Default);
        app.update_handoff_prelaunch = Some(record);

        app.on_update_handoff_awaiting_park(22, None, None);
        let transfer = transferred
            .try_recv()
            .expect("the park landed and handed the fork its capture");
        assert_eq!(transfer.capture.live.len(), 63, "every session is handed");
        if cfg!(target_os = "macos") {
            let patience = std::time::Instant::now() + Duration::from_secs(10);
            while aterm_update::handoff_capture_prior().is_none_or(|prior| prior.park_us == 1) {
                assert!(
                    std::time::Instant::now() < patience,
                    "the landed park's cost never reached the ledger"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        drop(transfer);
        app.pending_update_handoff = None;
        app.pool.sessions.retain(|id, _| *id == 0);
        release_pty(&mut app, pty);
    }

    /// The scrollback sidecars in `dir`.
    fn sidecars(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.to_string_lossy().ends_with(".hist"))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// THE LAUNCHED LANE SENDS NO SCROLLBACK UNDER A POLICY THAT CARRIES NONE
    /// (the 2026-09-27 review of the merge of main's history carry). The
    /// launched lane starts its history export beside the launch, before the
    /// successor's policy may have been read; its park reads the policy, and a
    /// `carry = "visible"` or `"repaint"` there promises "never scrollback".
    /// Driven through the shipping park cue over a real PTY, with a real
    /// export that has already written its sidecar: under either policy the
    /// park stops that export and removes its file, hands the worker
    /// `HistoryPlan::Withheld`, and the worker's join names no sidecar and
    /// COUNTS every line the park saw (`history_dropped`), said as the
    /// policy's. With no policy the same park hands the export on and the join
    /// names its sidecar — the control.
    ///
    /// RED before the fix: the park handed the export on under every policy
    /// (`HistoryPlan::Exported`), and the join named the whole scrollback's
    /// sidecar on the record, because under those ceilings the screen carried
    /// no history lines and the sidecar's `take` covered all of it.
    #[test]
    fn a_policy_without_scrollback_withholds_the_launched_lanes_history_carry() {
        use crate::handoff_history::{Fallback, HistoryExporter, HistoryPlan};
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let nonce = "0123456789abcdef0123456789abcdef";
        for (attempt_id, policy) in [
            (11_u64, None),
            (12, Some("schema = 1\ncarry = \"visible\"\n")),
            (13, Some("schema = 1\ncarry = \"repaint\"\n")),
        ] {
            let at = format!("policy {policy:?}");
            let withheld = policy.is_some();
            let mut app = crate::App::headless_for_test();
            let term = desk_with_history(&app);
            let seen = crate::term_lock(&term).history_fence().lines();
            assert!(seen > 0, "{at}: PRECONDITION — the tab holds scrollback");
            let pty = quiet_pty(&mut app);
            let dir = std::env::temp_dir().join(format!(
                "aterm-withheld-history-{}-{attempt_id}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");

            // The export the launch started, its first pass on disk.
            let mut export =
                HistoryExporter::start(dir.clone(), vec![(0, std::sync::Arc::clone(&term))], true)
                    .expect("the export starts");
            assert!(
                export.await_caught_up(Duration::from_secs(30)),
                "{at}: its first pass finishes"
            );
            assert_eq!(
                sidecars(&dir).len(),
                1,
                "{at}: PRECONDITION — its sidecar is on disk"
            );

            let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
                7,
                "",
                &"ab".repeat(32),
            );
            let read = publish(&app, &ticket, policy);
            assert_eq!(
                read.is_some_and(|read| !read.carry_ceiling().carries_scrollback()),
                withheld,
                "{at}: PRECONDITION — the park reads this policy"
            );
            let (mut record, transferred) =
                prelaunched(attempt_id, ApplyMode::Immediate, FreezeSeed::Default);
            record.apply_attempt = Some(ticket);
            record.history_export = Some(export);
            app.update_handoff_prelaunch = Some(record);

            // The landed park writes its cost to the ledger on a detached
            // thread; a sentinel first, so the wait below is for THIS park's
            // write and no later ledger test reads it landing late.
            super::keep_landed_park(Duration::from_micros(1))
                .expect("the sentinel is kept")
                .join()
                .expect("the ledger writer finished");
            app.on_update_handoff_awaiting_park(attempt_id, Some(4242), None);
            let transfer = transferred
                .try_recv()
                .unwrap_or_else(|_| panic!("{at}: the park landed and handed its capture over"));
            if cfg!(target_os = "macos") {
                let patience = std::time::Instant::now() + Duration::from_secs(10);
                while aterm_update::handoff_capture_prior().is_none_or(|prior| prior.park_us == 1) {
                    assert!(
                        std::time::Instant::now() < patience,
                        "{at}: the landed park's cost never reached the ledger"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            let mut capture = transfer.capture;
            assert_eq!(
                matches!(capture.history, HistoryPlan::Withheld),
                withheld,
                "{at}: the plan the park hands the worker ({:?})",
                capture.history
            );
            if withheld {
                // The export is stopped at the park, and a stopped export's
                // files go with it (bounded: its worker may finish its last
                // look just after the park's own bounded wait).
                let patience = std::time::Instant::now() + Duration::from_secs(10);
                while !sidecars(&dir).is_empty() {
                    assert!(
                        std::time::Instant::now() < patience,
                        "{at}: the withheld export's sidecar was never removed"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
            }

            // The worker's join, exactly as `prepare_outgoing_artifacts` runs it.
            let verdicts = crate::handoff_history::stamp_manifest(
                &mut capture.manifest,
                &capture.screens,
                &capture.history_heads,
                std::mem::replace(&mut capture.history, HistoryPlan::Unexported).results(&dir),
                &dir,
                nonce,
            );
            let record = capture
                .manifest
                .sessions
                .iter()
                .find(|record| record.local_id == 0)
                .expect("session 0 is handed");
            let (_, joined) = verdicts
                .iter()
                .find(|(local_id, _)| *local_id == 0)
                .expect("session 0 is joined");
            if withheld {
                assert_eq!(record.history, None, "{at}: NO SIDECAR IS NAMED");
                assert!(sidecars(&dir).is_empty(), "{at}: and none is on disk");
                assert_eq!(
                    (record.history_dropped, record.history_lost),
                    (seen, seen),
                    "{at}: every line the park saw is counted as left behind"
                );
                assert_eq!(
                    joined.fallback,
                    Some(Fallback::Withheld),
                    "{at}: said as the policy's"
                );
                assert!(
                    record.history_withheld,
                    "{at}: and the record tells the successor so"
                );
            } else {
                assert!(
                    record.history.is_some(),
                    "{at}: CONTROL — with no policy the sidecar is named"
                );
                assert_eq!(
                    record.history_dropped, 0,
                    "{at}: and nothing is left behind"
                );
                assert!(!record.history_withheld, "{at}: nor withheld");
            }

            drop(capture);
            app.pending_update_handoff = None;
            release_pty(&mut app, pty);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// Hold `term`'s engine from another thread for longer than the widest
    /// freeze rung, returning once it is held: a park's capture then misses
    /// (`CaptureFailure::EngineBusy`), and its rollback waits the hold out.
    pub(super) fn hold_engine_past_every_rung(
        term: &std::sync::Arc<std::sync::Mutex<aterm_core::terminal::Terminal>>,
    ) -> std::thread::JoinHandle<()> {
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let term = std::sync::Arc::clone(term);
        let holder = std::thread::spawn(move || {
            let guard = crate::term_lock(&term);
            held_tx.send(()).expect("the test waits");
            std::thread::sleep(Duration::from_millis(1500));
            drop(guard);
        });
        held_rx.recv().expect("the engine is held");
        holder
    }

    /// A MISSED PARK RESUMES THE HISTORY EXPORT IT PAUSED (round six of the
    /// update audit, finding 14). The launched lane's park stops the export
    /// before it freezes anything, then misses — here the capture meets an
    /// engine another thread holds past the rung's deadline — and re-parks
    /// after the retry delay with the readers live. The tab prints more than
    /// the screen carry's 256 lines in that delay. Driven through the shipping
    /// gate (`try_park_for_prelaunched_successor`) twice, with a real
    /// following export and the worker's own join over the second capture:
    /// the whole history crosses, nothing is counted as left behind.
    ///
    /// RED before the fix: the park HALTED the export (terminal), the re-park
    /// handed on that halted export, its fence ended where the first attempt
    /// stopped it, and the join fell back to `Outrun` — the whole exported
    /// history counted as lost.
    #[test]
    fn a_missed_park_resumes_the_history_export_for_the_repark() {
        use crate::handoff_history::{HistoryExporter, HistoryPlan};
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let nonce = "0123456789abcdef0123456789abcdef";
        let mut app = crate::App::headless_for_test();
        let term = app.pool.get(0).expect("session 0").term.clone();
        for n in 0..2000 {
            crate::term_lock(&term)
                .process(format!("line {n}: the history a capture carries\r\n").as_bytes());
        }
        let pty = quiet_pty(&mut app);
        let dir = std::env::temp_dir().join(format!("aterm-repark-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let mut export =
            HistoryExporter::start(dir.clone(), vec![(0, std::sync::Arc::clone(&term))], true)
                .expect("the export starts");
        assert!(export.await_caught_up(Duration::from_secs(30)));
        let (mut record, transferred) = prelaunched(31, ApplyMode::Immediate, FreezeSeed::Default);
        record.history_export = Some(export);
        record.dialled = Some(crate::DialledSuccessor {
            pid: Some(4242),
            grant_limit: None,
            at: std::time::Instant::now(),
        });
        app.update_handoff_prelaunch = Some(record);

        // THE FIRST PARK MISSES: the engine is held past every rung's deadline
        // (and released later by itself: the miss's rollback takes it too).
        let holder = hold_engine_past_every_rung(&term);
        let first = std::time::Instant::now();
        app.try_park_for_prelaunched_successor(first);
        holder.join().expect("the holder finished");
        assert!(
            transferred.try_recv().is_err(),
            "PRECONDITION — the park missed"
        );
        let prelaunch = app.update_handoff_prelaunch.as_ref().expect("still held");
        assert_eq!(
            prelaunch.park_misses, 1,
            "PRECONDITION — one miss, re-parked"
        );
        let retry_at = prelaunch.park_retry_at.expect("a re-park is scheduled");

        // The tab keeps printing through the retry delay, past the screen
        // carry's lines — and the export, resumed, follows it.
        for n in 2000..2600 {
            crate::term_lock(&term)
                .process(format!("line {n}: printed during the retry delay\r\n").as_bytes());
        }
        std::thread::sleep(crate::handoff_history::FOLLOW_EVERY * 4);

        super::keep_landed_park(Duration::from_micros(1))
            .expect("the sentinel is kept")
            .join()
            .expect("the ledger writer finished");
        app.try_park_for_prelaunched_successor(retry_at + Duration::from_millis(1));
        let transfer = transferred.try_recv().expect("the re-park landed");
        if cfg!(target_os = "macos") {
            let patience = std::time::Instant::now() + Duration::from_secs(10);
            while aterm_update::handoff_capture_prior().is_none_or(|prior| prior.park_us == 1) {
                assert!(
                    std::time::Instant::now() < patience,
                    "the landed park's cost never reached the ledger"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let mut capture = transfer.capture;
        let seen = capture
            .history_heads
            .iter()
            .find(|head| head.local_id == 0)
            .expect("session 0's head")
            .fence
            .lines();
        let verdicts = crate::handoff_history::stamp_manifest(
            &mut capture.manifest,
            &capture.screens,
            &capture.history_heads,
            std::mem::replace(&mut capture.history, HistoryPlan::Unexported).results(&dir),
            &dir,
            nonce,
        );
        let (_, joined) = verdicts
            .iter()
            .find(|(local_id, _)| *local_id == 0)
            .expect("session 0 is joined");
        let record = capture
            .manifest
            .sessions
            .iter()
            .find(|record| record.local_id == 0)
            .expect("session 0 is handed");
        assert_eq!(joined.fallback, None, "the export meets the re-park's head");
        assert!(joined.take > 0, "the sidecar carries the older history");
        assert_eq!(
            record.history_dropped, 0,
            "none of the {seen} lines the re-park saw is left behind"
        );

        drop(capture);
        app.pending_update_handoff = None;
        app.update_handoff_prelaunch = None;
        release_pty(&mut app, pty);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE DRY RUN CAPTURES UNDER THE SUCCESSOR'S POLICY (the 2026-09-27 merge
    /// review). It runs on the main thread beside every automatic launch, and it
    /// used to capture at `Full` whatever the policy said — the very path a
    /// `carry = "repaint"` policy is sealed to route around when a release finds
    /// it panicking. With that path made to panic (aterm-core's
    /// `checkpoint.carry_projection` fault point, the grid projection every
    /// non-blank carry takes):
    ///
    /// * with the attempt's policy not read yet, no dry run is taken
    ///   (`Unmeasured`), so nothing projects, and the seed falls back to the
    ///   ledger's last measurement;
    /// * under a read `carry = "repaint"`, the dry run is measured and projects
    ///   nothing — and the rung is seeded from it;
    /// * CONTROL: under a pass with no policy (`full`), the same armed dry run
    ///   projects and panics — the fault point fires where a Full capture runs.
    ///
    /// RED before the fix: the dry run passed `CarryCeiling::Full`, so the
    /// repaint case panicked on the main thread.
    #[test]
    fn the_dry_run_captures_under_the_successors_policy_and_not_before_it_is_read() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = crate::App::headless_for_test();
        let _term = desk_with_history(&app);
        let ticket =
            crate::native_updater_service::ApplyAttemptTicket::for_test(7, "", &"ef".repeat(32));
        let (mut record, _transferred) = prelaunched(21, ApplyMode::Automatic, FreezeSeed::Default);
        record.apply_attempt = Some(ticket.clone());
        app.update_handoff_prelaunch = Some(record);
        let armed = |app: &mut crate::App| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                aterm_core::fault::with_armed("checkpoint.carry_projection", || {
                    app.dry_run_handoff_capture(caps())
                })
            }))
        };

        // Unread: nothing is captured, so nothing projects.
        let unread = armed(&mut app).expect("an unread policy runs no capture");
        assert!(matches!(unread, DryRunCapture::Unmeasured(_)), "{unread:?}");
        let (fallback, kept) = app.seed_first_freeze_rung(ApplyMode::Automatic, caps());
        assert!(kept.is_none(), "nothing measured, nothing kept");
        assert_eq!(
            fallback,
            FreezeSeed::from_prior(aterm_update::handoff_capture_prior().as_ref()),
            "the seed falls back to the ledger"
        );

        // Repaint: measured, and never projected.
        publish(&app, &ticket, Some("schema = 1\ncarry = \"repaint\"\n"))
            .expect("the repaint policy applies to every build");
        let repaint = armed(&mut app).expect("a repaint policy's dry run never projects a grid");
        assert!(
            matches!(
                repaint,
                DryRunCapture::Measured { sessions: 1.., .. }
                    | DryRunCapture::AtLeast { sessions: 1.., .. }
            ),
            "{repaint:?}"
        );
        let (seed, kept) = aterm_core::fault::with_armed("checkpoint.carry_projection", || {
            app.seed_first_freeze_rung(ApplyMode::Automatic, caps())
        });
        if let Some(writer) = kept {
            writer.join().expect("the ledger writer finished");
        }
        assert!(
            matches!(seed, FreezeSeed::DryRun { .. }),
            "the rung is seeded from the policy's own capture: {seed:?}"
        );

        // CONTROL, last (a panic inside the capture poisons the engine's lock):
        // a pass with no policy is `full`, and that capture projects.
        assert_eq!(publish(&app, &ticket, None), None);
        assert!(
            armed(&mut app).is_err(),
            "the fault point must fire where a Full capture projects, or this test proves nothing"
        );
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

    /// REGRESSION (the 2026-09-22/23 update audit, plan P0-4a): the per-grid cell
    /// cap was 32 Ki cells (256x128), and one aterm window in native full screen
    /// on a 5K display at the default font is 99x338 = 33,462 cells — so every
    /// automatic and manual update refused on that desk, every attempt, and named
    /// the wrong cap ("aggregate") while doing it. The cap is a sanity bound on an
    /// absurd geometry, so it must admit EVERY window the GUI can produce.
    ///
    /// The geometry comes from the GUI's own law ([`crate::App::grid_dims_for`])
    /// on a headless App carrying the owner's measured chrome (pad 24, pad_top 4,
    /// head 80 at scale 2), for four real Mac displays, full screen and maximized
    /// (a 37 pt notched menu bar plus a 70 pt Dock at 2x), at every font size
    /// from `FONT_PX_MIN` to 48 px. A pool is that window split into 1..=8 side
    /// by side panes — the whole display's area, which is what a desk of panes
    /// costs — admitted exactly as the capture admits a visible carry
    /// (`history = 0`, the alt grid reserved).
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
            show: crate::restore::WindowShow::UNKNOWN,
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
                    questions: None,
                    identity: None,
                    agent: None,
                    held: false,
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

    /// Gap #29: the macOS show state is LIVE, like the position. Entering full
    /// screen, minimizing, or clicking another window while the successor boots
    /// changes every one of its fields, and none of them may reject the Commit
    /// — nor, on the other side, hide a real topology change riding with them.
    #[test]
    fn a_show_state_change_while_the_successor_boots_is_not_a_topology_change() {
        use crate::restore::WindowShow;
        let shown = |show: WindowShow| {
            let mut layout = captured(Some((120, 80)), Some("/work"), "zsh");
            layout.windows[0].show = show;
            layout
        };
        let committed = shown(WindowShow {
            fullscreen: Some(false),
            minimized: Some(false),
            z_order: Some(0),
            key: Some(true),
        });
        let changes = [
            WindowShow {
                fullscreen: Some(true),
                ..committed.windows[0].show
            },
            WindowShow {
                minimized: Some(true),
                ..committed.windows[0].show
            },
            WindowShow {
                z_order: Some(1),
                key: Some(false),
                ..committed.windows[0].show
            },
            // A parent that captured nothing (headless, or before the field).
            WindowShow::UNKNOWN,
        ];
        for changed in changes {
            let live = shown(changed);
            assert_ne!(
                committed, live,
                "PRECONDITION: the derived PartialEq sees the show-state difference {changed:?}"
            );
            assert_eq!(
                commit_layout_topology(&committed),
                commit_layout_topology(&live),
                "a show-state change ({changed:?}) is not topology and must not reject Commit"
            );
        }
        let mut resized = shown(WindowShow {
            fullscreen: Some(true),
            ..committed.windows[0].show
        });
        resized.windows[0].cols = 200;
        assert_ne!(
            commit_layout_topology(&committed),
            commit_layout_topology(&resized),
            "the normalization is of the show state alone: a re-grid riding with it rejects"
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
                questions: None,
                identity: None,
                agent: None,
                held: false,
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

    /// `captured`'s single terminal leaf, edited in place.
    fn with_leaf(
        mut layout: RestoreManifest,
        edit: impl FnOnce(&mut TerminalLeafRestore),
    ) -> RestoreManifest {
        let RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(terminal),
        } = &mut layout.windows[0].restored_tabs[0].root
        else {
            unreachable!("the fixture builds a single terminal leaf");
        };
        edit(terminal);
        layout
    }

    fn stamp_all_five(terminal: &mut TerminalLeafRestore) {
        terminal.user_title = Some("fable driver".to_string());
        terminal.description = Some("drives the satcomp worker".to_string());
        terminal.icon = Some("🦊".to_string());
        terminal.role = Some("agent:claude-driver-fable".to_string());
        terminal.attention = Some("waiting on a human review".to_string());
    }

    /// The invariant the comment in `commit_layout_topology` states, for ALL
    /// FIVE user fields rather than the title alone: each stays in the Commit
    /// comparison (so a `meta set` the parent answers before its Commit-time
    /// re-capture is a change that Commit sees), the cwd/title/position
    /// normalization never touches them, and the sidecar the child parses
    /// carries exactly what was compared — which is what lets the successor
    /// put them back on the adopted sessions.
    #[test]
    fn every_user_metadata_field_is_compared_and_rides_the_sidecar() {
        type Clear = fn(&mut TerminalLeafRestore);
        let stamped = with_leaf(
            captured(Some((120, 80)), Some("/work"), "zsh"),
            stamp_all_five,
        );

        let clears: [(&str, Clear); 5] = [
            ("user_title", |terminal| terminal.user_title = None),
            ("description", |terminal| terminal.description = None),
            ("icon", |terminal| terminal.icon = None),
            ("role", |terminal| terminal.role = None),
            ("attention", |terminal| terminal.attention = None),
        ];
        for (field, clear) in clears {
            assert_ne!(
                commit_layout_topology(&stamped),
                commit_layout_topology(&with_leaf(stamped.clone(), clear)),
                "{field} is read under a blocking lock and must stay in the comparison"
            );
        }

        let dragged_and_degraded = with_leaf(captured(Some((640, 310)), None, ""), stamp_all_five);
        assert_eq!(
            commit_layout_topology(&stamped),
            commit_layout_topology(&dragged_and_degraded),
            "the degradable-field normalization must not reach the user fields"
        );

        let wire = stamped.to_toml().expect("the parent serializes its layout");
        let parsed = RestoreManifest::from_toml(&wire)
            .filter(|layout| layout.covers_exact_seamless_ids(&[7]))
            .expect("the child accepts the sidecar");
        assert_eq!(
            commit_layout_topology(&parsed),
            commit_layout_topology(&stamped),
            "the child's layout carries every compared user field"
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

    /// THE ORPHAN'S GATE: the write end of a pipe the orphan blocks on before it
    /// runs the first command of its script. Nothing the orphan does — exiting
    /// least of all — can happen before the test releases it, which is what lets
    /// a caller attach its [`CandidateExitWatch`] to a process that is provably
    /// still there.
    ///
    /// WHY THE FIXTURE NEEDS ONE (2026-09-18). It used to be `sleep 0.3; exit 7`:
    /// the orphan started dying 300 ms after its fork, and the test thread had to
    /// read the pid, reap the middle, `kqueue()` and `kevent(EV_ADD)` inside that
    /// window. Under the full parallel suite that is a race the test cannot win
    /// by right — a stalled thread comes back to a pid launchd has already reaped,
    /// `proc_find` answers `ESRCH`, `watch` answers `None`, and the
    /// `expect("EVFILT_PROC attaches to a same-user process we may SIGKILL")`
    /// panics. That is the shape of the red main carried from fe4a680fd — this
    /// test passing alone and failing inside a full `test -p aterm-gui --lib` —
    /// though that run's own message was not kept, so the window is named here
    /// as the one hazard the fixture had, not as a captured trace. The same
    /// window sat under the sibling that asserts "it is alive right now"
    /// against a `sleep 0.3` orphan.
    ///
    /// MEASURED before the gate, on this Apple silicon Mac (2026-09-18): 80
    /// concurrent copies of this test under 28 CPU hogs, then two more full
    /// 4944-test runs of the old binary under 20 hogs, and the window did not
    /// fire once — which is why the fixture is fixed by construction rather
    /// than by a longer sleep: a window that fires only under a load this
    /// machine could not reproduce is still a window. The gate is a pipe this
    /// test owns (per test, never a shared path or a global), CLOEXEC on both
    /// ends — but on Darwin [`make_cloexec_pipe`](super::make_cloexec_pipe) is
    /// `pipe()` followed by a separate `set_cloexec` per end, so a sibling's fork on
    /// another thread inside that gap CAN carry a copy of either end. That is
    /// tolerated, not prevented: a carried read end is never read from, and a
    /// carried write end cannot stall the orphan because [`OrphanGate::release`]
    /// writes the newline `read` is waiting for rather than closing to EOF.
    /// The orphan's script waits on descriptor 3 (`read -r go <&3`), because a
    /// non-interactive shell points a background job's STDIN at `/dev/null` and
    /// descriptor 3 is untouched by that rule in bash, dash and zsh alike.
    struct OrphanGate(std::os::fd::OwnedFd);

    impl OrphanGate {
        /// Let the orphan run its script. One newline is what `read` is waiting
        /// for, so the orphan proceeds whether or not any other holder of the
        /// read end is still alive; a broken pipe means the orphan is already
        /// gone, which no caller here reaches and which is not the fixture's
        /// failure to report either way.
        fn release(self) {
            use std::io::Write as _;
            let mut writer = std::fs::File::from(self.0);
            let _ = writer.write_all(b"\n");
        }
    }

    /// Make a process that is NOT our child: fork a middle process, let IT fork the
    /// orphan, then reap the middle so the orphan reparents to launchd. Returns the
    /// orphan's pid, which `waitid` will answer `ECHILD` about forever after, and
    /// the [`OrphanGate`] that keeps the orphan from running `script` until the
    /// caller says so.
    fn spawn_orphan(script: &str) -> (u32, OrphanGate) {
        let (gate_read, gate_write) =
            super::make_cloexec_pipe().expect("a pipe for the orphan's gate");
        // THE ORPHAN'S STANDARD STREAMS GO TO `/dev/null`, not to the pipe this
        // reads: a background job inherits the pipe's write end, so leaving it
        // open would make `read_to_string` below wait for the ORPHAN to exit and
        // hand back a pid that is already gone — which is a fixture that silently
        // tests nothing. The gate's read end arrives as the middle's stdin and is
        // moved to descriptor 3 before the job is forked, so the job's stdin is
        // the `/dev/null` a background job gets anyway and its gate is the one
        // descriptor no shell rewrites.
        let mut middle = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "exec 3<&0 0</dev/null; {{ read -r go <&3; {script} ; }} >/dev/null 2>&1 & echo $!"
            ))
            .stdin(std::process::Stdio::from(gate_read))
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
        (pid.trim().parse().expect("a pid"), OrphanGate(gate_write))
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

    /// THE LAUNCHED LANE'S LIVENESS PROBE LEAVES THE WITNESS ALONE. The ready wait
    /// asks `candidate_gone` every slice, and reading the kqueue would consume the
    /// one exit event that says how the candidate died — so a probe that read it
    /// would turn every death on the shipping lane into `Unobserved`.
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
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

    /// THE CAPABILITY THE SHIPPING LANE HAD NO SUBSTITUTE FOR: a full `wait(2)`
    /// status for a process this one did not fork and may not reap.
    ///
    /// Both halves matter and they are the two verdicts that used to be
    /// indistinguishable there — a candidate that CHOSE to stop, and a candidate
    /// something else stopped.
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

        let (starved, gate) = spawn_orphan("sleep 30");
        // Registered while it is alive, exactly as the rendezvous accept does.
        let watch = CandidateExitWatch::watch(starved).expect("the candidate is alive");
        gate.release();
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

        let (refusing, gate) = spawn_orphan("exit 0");
        let watch = CandidateExitWatch::watch(refusing).expect("the candidate is alive");
        // A bare pid carries no birth stamp, so nothing is signalled and the
        // candidate reaches its own `exit` — the deterministic form of the race
        // this read exists to widen.
        let candidate = HandoffCandidate::from_bare_pid(refusing);
        let mut handle = HandoffCandidateHandle::Launched(Some(watch));
        // "ALIVE RIGHT NOW" IS A FACT HERE, NOT A HOPE: the gate is still shut,
        // so the orphan has not reached its `exit` and cannot until it opens.
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

    /// THE FAILING DIRECTION, which must cost only evidence. A pid that names
    /// nothing cannot be attached to, and the answer has to be `None` rather than
    /// an error path the reject lane would have to handle.
    #[test]
    fn watching_a_candidate_that_is_already_gone_answers_none() {
        let (gone, gate) = spawn_orphan("exit 0");
        gate.release();
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
        // openpty(3) opens both ends inheritable: a child another test spawns
        // meanwhile would keep the slave open past its exec, for its whole
        // life, and a closed slave would never read as hung up (the fd-copy
        // sweep of 2026-09-27).
        for fd in [master, slave] {
            aterm_pty::set_cloexec(fd, true).expect("close-on-exec");
        }
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
        // identity bug never reports closed, and ten seconds of grace cannot
        // mask it. Ten, not two: the hang-up waits for every copy of the slave,
        // and a child another test is forking holds one until it execs (the
        // fd-copy sweep of 2026-09-27).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !handoff_masters_closed(&live) {
            assert!(
                std::time::Instant::now() < deadline,
                "peer death must still revoke — the live-set identity is stale"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        aterm_pty::close_fd(master);
    }

    /// A HUNG-UP MASTER IS NOT OUTPUT WAITING (the 2026-09-22/23 update audit,
    /// plan P1-3). A pane kept open after its command exited (`--hold`) holds a
    /// master whose slave is gone: macOS reports it `POLLIN|POLLHUP` forever.
    /// The park gate's peek used to count that as "a session has output waiting
    /// that its reader has not taken", so every automatic attempt held its
    /// successor 120 s and stood down, every ~15 min, for as long as the pane
    /// stayed open. Death is `handoff_masters_closed`'s question; the activity
    /// peek answers only for live masters — including when the dead one still
    /// has bytes queued, and beside a live quiet one.
    #[test]
    fn a_hung_up_master_is_never_output_waiting() {
        let open = || {
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
            // openpty(3) opens both ends inheritable: a child another test spawns
            // meanwhile would keep the slave open past its exec, for its whole
            // life, and a closed slave would never read as hung up (the fd-copy
            // sweep of 2026-09-27).
            for fd in [master, slave] {
                aterm_pty::set_cloexec(fd, true).expect("close-on-exec");
            }
            (master, slave)
        };
        let (exited, exited_slave) = open();
        let (alive, alive_slave) = open();
        // The exited command's last words, still queued when it died.
        // SAFETY: bounded write of a stack byte to the test-owned slave.
        assert_eq!(
            unsafe { libc::write(exited_slave, [0x62u8].as_ptr().cast(), 1) },
            1
        );
        aterm_pty::close_fd(exited_slave);
        let dead = [(1u64, exited, 4242i32)];
        // PROMPT-EVENTUAL, as in the sibling test above: the HUP edge can
        // surface a scheduler quantum after close(2), or a sibling's exec after
        // it when a fork holds a copy of the slave — so 10 s, not 2.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
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
        // …and a LIVE master with bytes waiting still holds the gate.
        // SAFETY: bounded write of a stack byte to the test-owned slave.
        assert_eq!(
            unsafe { libc::write(alive_slave, [0x62u8].as_ptr().cast(), 1) },
            1
        );
        assert!(
            handoff_masters_have_activity(&desk),
            "a live session's unread output is still output"
        );
        aterm_pty::close_fd(alive_slave);
        aterm_pty::close_fd(alive);
        aterm_pty::close_fd(exited);
    }

    /// Seamless seam 2, worker level: the ready wait completes to `ProofReady`
    /// while a handed-off master has queued readable output, and a cancel poke
    /// is typed `ActivityRevoked` (the retry-budget classification), never a
    /// generic rejection.
    /// A handed session that ends while the proof is awaited is the desk
    /// changing, typed as the park types it (round six, finding 37).
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
        for fd in [master, slave] {
            aterm_pty::set_cloexec(fd, true).expect("close-on-exec");
        }
        aterm_pty::close_fd(slave);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
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
                handoff_ready_deadline(),
                &mut || false,
                #[cfg(target_os = "macos")]
                None,
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
    fn pre_ready_death_preserves_leader_identity_until_normal_group_reap() {
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

        // Tier-1 projection: the ready wait observes an exited direct leader while
        // retaining its waitable identity and a live same-group descendant. Rollback is not
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

        // The write end stays OPEN — the stray copy a sibling fork can hold — so
        // the death can only be observed by asking the leader, and what follows
        // proves that asking left its identity for the group sweep.
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

        // Negative control: the removed wait-before-kill ordering is explicitly
        // executable only in the mutant and immediately violates the ordering
        // property while this real descendant is known live.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut old_order = model_state.clone();
        assert!(buggy.fire("BuggyWaitBeforeGroupSignal", &mut old_order));
        assert!(!buggy.check_invariant("ProcessGroupSignalPrecedesDirectChildReap", &old_order,));
        assert_eq!(old_order["descendant_live"], 1);
        // Negative control: a sweep whose signal reaches only the leader
        // (`kill(pid)` where `kill(-pgid)` was meant) leaves exactly this
        // descendant running — the one the real sweep below must kill.
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

    /// LAUNCHSERVICES' ANSWER IS READ BESIDE THE PROOF WAIT (2026-09-19): a pid
    /// mismatch that lands AFTER the transfer still refuses the handoff before
    /// Commit, and the refusal is a kill-and-reap of the corroborated candidate
    /// (the descriptors are its by then), after which — and only after which —
    /// the parent may resume its readers.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_launchservices_pid_mismatch_after_transfer_is_rejected_and_reaped() {
        let mut command = std::process::Command::new("/bin/sh");
        command.arg("-c").arg("sleep 30");
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

        // Tier-1 projection: a launch answer naming another pid is the candidate
        // failing its identity check — the model's mismatched-proof failure —
        // and the worker then wins the reject arbiter, kills, reaps, and only
        // then may the parent resume.
        let model = aterm_spec::derive::native_update_overlap_handoff_model();
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

        // The proof pipe stays OPEN: nothing but the corroboration ends this wait.
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
        // A two-minute deadline, bounded at one: a wait that ignored the
        // corroboration would run to the deadline and answer `TimedOut` two
        // minutes on. The minute is a hang detector, not a latency budget.
        let started = std::time::Instant::now();
        assert_eq!(
            wait_handoff_ready(
                &proof_rd,
                expected,
                &cancel_rx,
                &[],
                std::time::Instant::now() + std::time::Duration::from_secs(120),
                &mut || false,
                Some(super::LaunchCorroboration {
                    in_flight: &in_flight,
                    dialer_pid: leader,
                    answered: false,
                }),
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
            kill_and_reap_handoff_child(candidate, &mut handle).warrant,
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

    /// THE STRAY WRITER. A copy of the proof pipe's write end that outlives the
    /// candidate — a sibling fork caught it — holds EOF back, and the ready wait
    /// used to learn of the death only from EOF: the user's parked terminal stayed
    /// frozen until the deadline. The candidate's own end now ends the wait as
    /// `ChildDied`, and asking leaves the status for the reap that follows.
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
        let ended_by = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !handle.candidate_gone(candidate) {
            assert!(
                std::time::Instant::now() < ended_by,
                "the candidate exits on its own"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // The stray copy: this test keeps the write end open, so no EOF can come.
        let (proof_rd, _stray_proof_wr) = make_cloexec_pipe().expect("proof pipe");
        let (_cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);

        // NEGATIVE CONTROL — the wait as it was, EOF its only news of a death: the
        // candidate is provably gone and the wait still runs to its deadline.
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
        // No timing claim: with the writer held, `ChildDied` can only come from
        // the candidate probe, never from EOF or the far deadline.
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

        let HandoffCandidateHandle::Forked(mut child) = handle else {
            unreachable!("built as the fork lane above");
        };
        assert_eq!(
            child.wait().expect("reap the candidate").code(),
            Some(7),
            "asking reaped nothing: the status is still the candidate's own"
        );
    }

    /// A LIVE candidate is never reported gone, whatever the pipes say.
    #[test]
    fn a_live_candidate_is_not_gone() {
        let (candidate, mut handle, mut cleanup) = sleeping_candidate();
        assert!(!handle.candidate_gone(candidate), "alive right now");
        assert_eq!(
            kill_and_reap_handoff_child(candidate, &mut handle).warrant,
            HandoffRollbackWarrant::Reaped
        );
        cleanup.disarm();
    }

    /// A 30-second candidate in a group of its own, with nothing of the test
    /// harness's: a failed assertion kills and reaps it through the guard rather
    /// than leaving it — stopped, in one test — holding the harness's output.
    fn sleeping_candidate() -> (
        HandoffCandidate,
        HandoffCandidateHandle,
        ProcessGroupCleanup,
    ) {
        let child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exec sleep 30")
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

    /// A STOPPED candidate is not a dead one. Darwin's `waitid` reports a stopped
    /// child even when only `WEXITED` is asked for, and the probe used to read any
    /// report as an exit — so a successor a debugger or a job-control signal had
    /// paused would have been rejected, killed, and filed as already gone.
    #[test]
    fn a_stopped_candidate_is_not_gone() {
        let (candidate, mut handle, mut cleanup) = sleeping_candidate();
        let pid = libc::pid_t::try_from(candidate.pid).expect("bounded pid");
        // SAFETY: a signal to our own unreaped child.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
        // Not vacuous: the kernel has REPORTED the stop before the probe is asked.
        let stopped_by = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            // SAFETY: a zeroed out-parameter of exactly the type waitid fills;
            // WNOWAIT consumes nothing.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    libc::id_t::try_from(pid).expect("positive pid"),
                    &mut info,
                    libc::WSTOPPED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            assert_eq!(rc, 0, "our own child");
            if info.si_code == libc::CLD_STOPPED {
                break;
            }
            assert!(
                std::time::Instant::now() < stopped_by,
                "the candidate stops"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!handle.candidate_gone(candidate), "stopped is not gone");
        assert_eq!(
            kill_and_reap_handoff_child(candidate, &mut handle).warrant,
            HandoffRollbackWarrant::Reaped
        );
        cleanup.disarm();
    }

    /// The matching answer, and no answer at all, change nothing: the wait runs to
    /// its real outcome (here proof EOF, the child's death).
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
                    std::time::Instant::now() + std::time::Duration::from_secs(10),
                    &mut || false,
                    Some(super::LaunchCorroboration {
                        in_flight: &in_flight,
                        dialer_pid: 4242,
                        answered: false,
                    }),
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

    /// The launched lane's group-sweep gate is a kernel read: a process that
    /// set up its own group (what `contain_own_process_group` does on a
    /// launched successor's entry) is reported as leading it, and one that
    /// inherited its parent's group is not — so `-pid` is sent only when it
    /// names the candidate's own group.
    ///
    /// Fixture cleanup is the `sleep` itself: both exit on their own within
    /// 30 s even if an assertion panics before the explicit kills.
    #[cfg(unix)]
    #[test]
    fn the_group_sweep_gate_reads_whether_the_candidate_leads_its_own_group() {
        use std::os::unix::process::CommandExt as _;
        let spawn = |own_group: bool| {
            let mut command = std::process::Command::new("/bin/sleep");
            command
                .arg("30")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
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

    /// The corroborated arm's group sweep still reaches the helpers of a FORK-lane
    /// candidate whose leader exited between the identity read and the sweep.
    /// The leader is then an unreaped zombie of ours, and Darwin answers `getpgid`
    /// on a zombie with ESRCH, so the `getpgid(pid) == pid` read alone withheld
    /// the group SIGKILL and the helper it left in its group outlived the reject.
    /// The fork lane's pin (`candidate_is_our_child`) licenses the sweep.
    ///
    /// Fixture cleanup: the helper is `sleep 30` and exits on its own.
    #[cfg(unix)]
    #[test]
    fn a_corroborated_candidate_whose_leader_already_exited_still_has_its_group_swept() {
        use std::io::BufRead as _;
        use std::os::unix::process::CommandExt as _;
        let mut leader = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 30 </dev/null >/dev/null 2>&1 & echo $!"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn a group leader with a helper");
        let leader_pid = i32::try_from(leader.id()).expect("bounded pid");
        let mut line = String::new();
        std::io::BufReader::new(leader.stdout.take().expect("leader stdout"))
            .read_line(&mut line)
            .expect("read the helper pid");
        let helper: libc::pid_t = line.trim().parse().expect("helper pid");
        // The leader exits right after the echo: wait for its zombie, UNREAPED.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
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
                    linux_host: false,
                    linux: None,
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
                    checked_at: None,
                },
            ),
            CheckCompletion::Reduced,
            "PRECONDITION: the check must reduce, or nothing is staged"
        );
        build
    }

    /// Whether a latch deadline is a STRUCTURAL verdict's one re-sample, a day
    /// out (gap 14, 2026-09-26) — the shape a structural convergence takes now
    /// that it is no longer `retry_at: None`. Named by the schedule it came
    /// from, not to the second: these are real `Instant`s.
    fn is_structural_resample(retry_at: Option<std::time::Instant>) -> bool {
        retry_at.is_some_and(|at| {
            let wait = at.saturating_duration_since(std::time::Instant::now());
            wait > crate::app_native::STRUCTURAL_RESAMPLE_AFTER - std::time::Duration::from_secs(60)
                && wait <= crate::app_native::STRUCTURAL_RESAMPLE_AFTER
        })
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
            #[cfg(target_os = "macos")]
            activate_at_commit: false,
            attempt_id: 1,
            nonce: None,
            live: Vec::new(),
            adoption: Vec::new(),
            child_pid: None,
            mode,
            apply_attempt: Some(ticket),
            same_image: None,
            target_build: build,
            target_commit: TEST_COMMIT.to_string(),
            layout: crate::restore::RestoreManifest::new(Vec::new()),
            layout_digest: [0; 32],
            screen_digest: [0; 32],
            activity_epoch: app.update_handoff_activity_epoch,
            hold_serials: 0,
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
            #[cfg(target_os = "macos")]
            activate_at_commit: false,
            attempt_id: 2,
            nonce: None,
            live: Vec::new(),
            adoption: Vec::new(),
            child_pid: None,
            mode: ApplyMode::Immediate,
            apply_attempt: Some(ticket),
            same_image: None,
            target_build: build,
            target_commit: TEST_COMMIT.to_string(),
            layout: crate::restore::RestoreManifest::new(Vec::new()),
            layout_digest: [0; 32],
            screen_digest: [0; 32],
            activity_epoch: app.update_handoff_activity_epoch,
            hold_serials: 0,
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

        // AdoptionMismatch, twice: a confirmation and then the end of the lane's
        // schedule for those bytes — converged onto the ONE re-sample a day out
        // (gap 14), which `arm` reads as `SuppressManualOnly` until it comes due.
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
        assert!(
            is_structural_resample(deadline(&app)),
            "two proofs that these two images disagree is not a busy afternoon; \
             the lane must be finished with the artifact's schedule — its one \
             re-sample a day out, not a third park/spawn/paint round trip inside \
             the hour: {:?}",
            deadline(&app)
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

    /// THE WHOLE SEQUENCE OF THE 2026-09-21 PARK-MISS FINDING, driven through
    /// the production reducer: an automatic attempt whose park missed every rung
    /// comes back with the outcome `park_miss_disposition` stands it down with,
    /// and the lane treats it as activity — a live retry intent on the spaced
    /// schedule, no manual-only latch, no physical budget spent, and the ladder's
    /// anchor exactly where it was. Twice, because two of them ten minutes apart
    /// was the pair that used to converge the artifact to `retry_at: None`.
    #[test]
    fn a_park_that_missed_every_rung_is_retried_as_activity_and_never_latches() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        // Inside KeysOnly, where a busy machine is admitted.
        let armed_at = std::time::Instant::now()
            - crate::native_update_auto_intent::PREFER_IDLE_WINDOW
            - crate::native_update_auto_intent::PREFER_OUTPUT_GAP_WINDOW
            - std::time::Duration::from_secs(1);
        app.auto_apply_ladder = Some(crate::AutoApplyLadder {
            build,
            armed_at,
            announced: crate::native_update_auto_intent::ApplyPhase::KeysOnly,
            restored_hold_said: false,
        });
        for miss_pair in 1..=2 {
            let super::ParkMissDisposition::StandDown(stand_down) = super::park_miss_disposition(
                super::PRELAUNCH_MAX_PARK_MISSES,
                "a PTY reader missed the 250 ms handoff park deadline".to_string(),
            ) else {
                panic!("past the last rung the attempt stands down");
            };
            reduce_one_returned_failure(
                &mut app,
                ApplyMode::AutomaticPastGrace,
                build,
                &"ab".repeat(32),
                stand_down.outcome,
                crate::ChildDeathEvidence::Unobserved,
            );
            assert!(
                app.auto_apply_manual_only.is_none(),
                "miss pair {miss_pair}: a busy machine never latches the lane manual-only"
            );
            assert!(
                app.auto_apply_intent.is_some(),
                "miss pair {miss_pair}: the lane keeps a live retry intent"
            );
            assert!(
                app.auto_apply_physical_retry.is_none(),
                "miss pair {miss_pair}: no physical budget is spent on a stopwatch"
            );
            assert_eq!(
                app.auto_apply_ladder.map(|ladder| ladder.armed_at),
                Some(armed_at),
                "miss pair {miss_pair}: the ladder's anchor is untouched"
            );
        }
        assert_eq!(
            app.auto_overlap_retry.map(|retry| retry.cycles),
            Some(2),
            "each stood-down attempt costs one rung of the ACTIVITY spacing, which \
             saturates and never ends the lane"
        );
    }

    /// A CAPTURE REFUSAL IS ITS OWN LANE (the 2026-09-22/23 update audit, plan
    /// P0-3), driven through the production reducer: RECORDED as an apply failure
    /// (so `failing_applies` moves and `update status` names it — v0.91 kept it at
    /// zero for a day of refused attempts), retried only once the desk changes or
    /// the hour's backstop passes, never on the activity spacing, never charged to
    /// the physical budget, and NEVER LATCHED — twice, since two refusals were the
    /// pair that latched 0.87-0.90.
    ///
    /// Fails on the code before this change: `CaptureRefused` did not exist, and
    /// the park stood the successor down as `ActivityRevoked` for this very fact,
    /// which leaves `auto_overlap_retry` spent and records nothing.
    #[test]
    fn a_capture_refusal_is_recorded_waits_for_the_desk_and_never_latches() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        let dmg = [0xab_u8; 32];
        let armed_at = std::time::Instant::now() - std::time::Duration::from_secs(400);
        app.auto_apply_ladder = Some(crate::AutoApplyLadder {
            build,
            armed_at,
            announced: crate::native_update_auto_intent::ApplyPhase::KeysOnly,
            restored_hold_said: false,
        });
        // The per-process scratch ledger is shared with sibling tests (all of
        // them under the lock held above), so the count is compared step to
        // step rather than from zero.
        let mut failing = None;
        for refusal in 1..=2_u32 {
            let before = std::time::Instant::now();
            reduce_one_returned_failure(
                &mut app,
                ApplyMode::AutomaticPastGrace,
                build,
                &"ab".repeat(32),
                crate::UpdateHandoffOutcome::CaptureRefused,
                crate::ChildDeathEvidence::Unobserved,
            );
            assert!(
                app.auto_apply_manual_only.is_none(),
                "refusal {refusal}: a capture refusal never latches the lane"
            );
            assert!(
                app.auto_apply_physical_retry.is_none(),
                "refusal {refusal}: it is not the candidate's bytes"
            );
            assert!(
                app.auto_overlap_retry.is_none(),
                "refusal {refusal}: it is not the machine being busy"
            );
            let intent = app.auto_apply_intent.expect("the lane keeps a live intent");
            assert_eq!((intent.build, intent.dmg_sha256), (build, dmg));
            assert!(
                intent.retry_at >= before + crate::app_native::CAPTURE_REFUSAL_PROBE,
                "refusal {refusal}: re-probed on the refusal lane's cadence"
            );
            assert!(app.auto_apply_capture_refusal.is_some());
            assert_eq!(
                app.auto_apply_ladder.map(|ladder| ladder.armed_at),
                Some(armed_at),
                "refusal {refusal}: the ladder's anchor is untouched"
            );
            let snapshot = app.native_updater_service.snapshot();
            assert_eq!(
                snapshot.failing_applies,
                failing.map_or(snapshot.failing_applies.max(1), |prior: u32| prior + 1),
                "refusal {refusal}: recorded as an apply failure, which is what moves \
                 `failing_applies` toward the persistent-failure notice"
            );
            failing = Some(snapshot.failing_applies);
            assert!(
                snapshot.apply_failure.contains("CaptureRefused"),
                "`update status` names the refusal: {:?}",
                snapshot.apply_failure
            );
        }

        // THE DESK UNCHANGED HOLDS the next attempt: no successor is launched into
        // the same refusal…
        let now = std::time::Instant::now();
        assert!(app.capture_refusal_holds(build, dmg, false, now));
        assert!(app.capture_refusal_holds(build, dmg, false, now));
        // …a DIFFERENT artifact is not held by it…
        assert!(!app.capture_refusal_holds(build + 1, dmg, false, now));
        // …and the desk moving releases it (a resize is one of the facts every
        // refusal the capture can make is about).
        for session in app.pool.iter() {
            crate::term_lock(&session.term).resize(30, 100);
        }
        assert!(!app.capture_refusal_holds(build, dmg, false, now));
        assert!(
            app.auto_apply_capture_refusal.is_none(),
            "a released refusal is cleared, so the attempt that follows is ordinary"
        );

        // THE BACKSTOP: an hour with the desk unchanged tries once more anyway.
        app.auto_apply_capture_refusal = Some(crate::AutoApplyCaptureRefusal {
            build,
            dmg_sha256: dmg,
            activation: false,
            desk: app.capture_desk_fingerprint(),
            refused_at: now,
        });
        assert!(app.capture_refusal_holds(build, dmg, false, now));
        assert!(!app.capture_refusal_holds(
            build,
            dmg,
            false,
            now + crate::app_native::CAPTURE_REFUSAL_BACKSTOP
        ));
    }

    /// THIS PROCESS'S OWN FAILURES ARE NOT THE CANDIDATE'S (the 2026-09-22/23
    /// update audit, plan P1-2), driven through the production reducer: two
    /// `ProducerFailed` completions — a full disk at the manifest write, an
    /// `EMFILE` at the descriptor duplicate — leave the lane on the TRANSIENT
    /// schedule with a deadline to lapse at, where the same two filed as
    /// `PreparationFailed` (the control, and what they were until this change)
    /// converge after two, onto a structural verdict's one re-sample a day out.
    #[test]
    fn a_producer_failure_retries_where_a_preparation_failure_converges() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        for (outcome, converges) in [
            (crate::UpdateHandoffOutcome::ProducerFailed, false),
            (crate::UpdateHandoffOutcome::PreparationFailed, true),
        ] {
            let mut app = App::headless_for_test();
            let build = stage_one_build(&mut app);
            for _ in 0..crate::app_native::STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS {
                reduce_one_returned_failure(
                    &mut app,
                    ApplyMode::AutomaticPastGrace,
                    build,
                    &"ab".repeat(32),
                    outcome,
                    crate::ChildDeathEvidence::Unobserved,
                );
            }
            let latch = app
                .auto_apply_manual_only
                .expect("a physical failure latches for its schedule's spacing");
            assert_eq!(
                is_structural_resample(latch.retry_at),
                converges,
                "{outcome:?}: converged={converges} after {} failures: {:?}",
                crate::app_native::STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS,
                latch.retry_at
            );
        }
    }

    /// THE FORK LANE CLIMBS THE FREEZE LADDER ACROSS ATTEMPTS (plan P1-4): a
    /// miss is no longer a physical failure — it rides the activity spacing — so
    /// the rung it used to buy through the physical count is counted per target
    /// build instead: 20 ms, then 80 ms, then a quarter second, and a different
    /// build starts over.
    #[cfg(unix)]
    #[test]
    fn a_fork_lane_park_miss_is_timing_and_buys_the_next_rung() {
        let mut app = App::headless_for_test();
        let target = 4_242_u64;
        assert_eq!(
            super::handoff_freeze_budget(
                ApplyMode::Automatic,
                app.fork_park_misses_of(target),
                super::FreezeSeed::Default,
            ),
            std::time::Duration::from_millis(20)
        );
        for (misses, rung_ms) in [(1_u8, 80_u64), (2, 250), (3, 250)] {
            let error = app.fork_park_missed(target, "a PTY reader missed".to_string());
            assert!(
                error.is_park_missed(),
                "a fork-lane miss is typed as timing, never `Failed` (physical)"
            );
            assert_eq!(app.fork_park_misses_of(target), misses);
            assert_eq!(
                super::handoff_freeze_budget(
                    ApplyMode::Automatic,
                    app.fork_park_misses_of(target),
                    super::FreezeSeed::Default,
                ),
                std::time::Duration::from_millis(rung_ms)
            );
        }
        assert_eq!(app.fork_park_misses_of(target + 1), 0);
    }

    /// ONE FACT, ONE RECORD: the fork lane's automatic park miss is the
    /// launched lane's activity stand-down (`HandoffFailureLane::ActivityRevoked`,
    /// recorded routine), so main's switch record files it as "Waiting to
    /// install", never "was not installed" — and a person's apply, which the
    /// launched lane classifies `Manual`, stays a stop worth a warning, as
    /// `fork_park_miss_outcome` tells that person it failed.
    #[cfg(unix)]
    #[test]
    fn a_fork_lane_park_miss_is_recorded_as_the_routine_wait_it_is() {
        let mut app = App::headless_for_test();
        let miss = app.fork_park_missed(7, "a PTY reader missed".to_string());
        for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
            assert!(miss.stops_routinely(mode), "{mode:?}: a timing miss");
        }
        let mode = ApplyMode::Immediate;
        assert!(!miss.stops_routinely(mode), "{mode:?}: a person asked");
        let activity = crate::UpdateHandoffStartError::activity("typing");
        assert!(activity.stops_routinely(ApplyMode::Immediate));
        for other in [
            crate::UpdateHandoffStartError::failed("masters closed"),
            crate::UpdateHandoffStartError::refused("unowned socket"),
            crate::UpdateHandoffStartError::capture_refused("session 3"),
        ] {
            assert!(!other.stops_routinely(ApplyMode::Automatic), "{other}");
        }
        app.update_switch_on_record = Some(Some("0.93.0".to_string()));
        app.record_update_switch_stopped(
            miss.stops_routinely(ApplyMode::Automatic),
            &miss.to_string(),
        );
        let record = app
            .messages
            .log()
            .records()
            .last()
            .expect("the stop is on record");
        assert!(
            record.title.starts_with("Waiting to install aterm"),
            "a routine wait, not a failed install: {}",
            record.title
        );
        let detail = record.detail.join(" ");
        assert!(!detail.contains("you were typing"), "{detail}");
        assert!(detail.contains("a PTY reader missed"), "{detail}");
    }

    /// A ROUTINE STAND-DOWN KEEPS ITS OWN REASON (round six, finding 43): the
    /// launched lane's `ActivityRevoked` covers a hold cap spent on a video take,
    /// a park that missed on a busy machine and every session closing — none of
    /// them typing. The record used to say "you were typing" for all of them and
    /// drop the reason the stand-down carried.
    #[cfg(unix)]
    #[test]
    fn a_routine_stand_down_is_recorded_with_its_own_reason_not_typing() {
        let super::ParkMissDisposition::StandDown(busy) = super::park_miss_disposition(
            super::PRELAUNCH_MAX_PARK_MISSES,
            "a PTY reader missed".into(),
        ) else {
            panic!("every rung missed: a stand-down");
        };
        let busy = busy.detail;
        for (detail, carried) in [
            (
                super::hold_cap_stand_down_detail(
                    "the terminal never offered a moment to pause in within the hold cap",
                    std::time::Duration::from_secs(120),
                    Some("a video recording is running"),
                ),
                "video recording",
            ),
            (busy, "the machine is busy"),
            (
                "every terminal session closed while the successor booted".to_string(),
                "every terminal session closed",
            ),
        ] {
            let mut app = App::headless_for_test();
            app.update_switch_on_record = Some(Some("0.93.0".into()));
            app.record_update_switch_stopped(true, &detail);
            let record = app.messages.log().records().last().expect("on record");
            let words = record.detail.join(" ");
            assert!(
                record.title.starts_with("Waiting to install"),
                "{}",
                record.title
            );
            assert!(!words.contains("you were typing"), "{words}");
            assert!(words.contains(carried), "{words}");
            assert!(words.contains(crate::update_words::TRIES_AGAIN), "{words}");
        }
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
            // Only the candidate failing its pre-park verification now.
            (
                Outcome::PreparationFailed,
                Lane::Physical(Shape::Structural),
            ),
            // THIS process's own write/descriptor failures: about the moment
            // and the machine, never the bytes (plan P1-2).
            (Outcome::ProducerFailed, Lane::Physical(Shape::Transient)),
            // A deterministic capture refusal: its own lane, never a physical
            // shape and never activity (plan P0-3).
            (Outcome::CaptureRefused, Lane::Refused),
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
            // rollback the terminal caused is not evidence about the artifact. It
            // does NOT dominate a capture refusal: that stand-down was the park's
            // own typed verdict about the desk, and re-filing it as activity is the
            // v0.91 loop (plan P0-3, `RefusalNeverRetriesAsActivity`).
            assert_eq!(
                Lane::classify(
                    ApplyMode::AutomaticPastGrace,
                    outcome,
                    Death::Unobserved,
                    true
                ),
                if outcome == Outcome::CaptureRefused {
                    Lane::Refused
                } else {
                    Lane::ActivityRevoked
                },
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
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
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
        // SCHEDULE ends: no more in-epoch rungs, and the lane says so once. What
        // follows is a quiet re-sample one epoch cooldown out — never a latch that
        // never lapses (the 2026-09-22/23 update audit, plan P1-1(d)): a starved
        // child is a fact about the machine's day, and `None` here kept a healthy
        // build off the machine until someone relaunched. At most one attempt per
        // six hours is the bound a lane that "retries forever" would not have.
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
        let resample = deadline(&app)
            .expect("a spent transient budget re-samples after the cooldown")
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            resample > std::time::Duration::from_secs(5 * 60 * 60 + 59 * 60)
                && resample <= std::time::Duration::from_secs(6 * 60 * 60),
            "the transient budget still ends: past it the lane waits a whole epoch \
             cooldown (6 h) per attempt, got {resample:?}"
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
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
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
        assert!(
            is_structural_resample(deadline(&app)),
            "twice told that this successor will not become our successor is not a \
             busy afternoon: the lane's schedule is done with these bytes, and only \
             the verdict's ONE re-sample, a day out, is left: {:?}",
            deadline(&app)
        );
        assert!(
            app.auto_apply_structural_verdict
                .is_some_and(|verdict| verdict.build == build),
            "the convergence is recorded as a structural verdict"
        );
    }

    /// A SUCCESSOR WHOSE SWAP WAS DEFERRED FOR A MOMENT IS RETRIED, NOT
    /// CONVERGED (round four of the 2026-09 update robustness work, plan item 2).
    ///
    /// The field shape: the download-lane successor's boot apply waited out a
    /// sibling's hold on the apply lock (or its `codesign` ran out of the apply
    /// budget), stayed the old build, refused the target — and exited `0`, which
    /// the test above rightly converges. It now exits
    /// [`crate::seamless::EXIT_SUCCESSOR_PASSING`], and the same two returned
    /// failures that converge a `0` ride the transient schedule instead: the
    /// second rung, no structural verdict, and the counted trial launch forgiven.
    /// RED before this change: every `Exited` was structural, so the second
    /// failure minted the verdict and its day's re-sample.
    #[test]
    fn a_successor_that_exits_75_is_retried_on_the_transient_schedule() {
        use crate::app_native::STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS;
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        let deferred = crate::ChildDeathEvidence::Exited {
            code: crate::seamless::EXIT_SUCCESSOR_PASSING,
        };
        for _ in 0..STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS {
            reduce_one_returned_failure(
                &mut app,
                ApplyMode::AutomaticPastGrace,
                build,
                &"ab".repeat(32),
                crate::UpdateHandoffOutcome::ChildDied,
                deferred,
            );
        }
        let wait = app
            .auto_apply_manual_only
            .expect("an automatic physical failure always latches")
            .retry_at
            .expect("a moment always has a deadline")
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            wait > std::time::Duration::from_secs(1700)
                && wait <= std::time::Duration::from_secs(1800),
            "the structural lane's whole lifetime later, a deferred swap is on the \
             transient schedule's second rung (~1800 s), got {wait:?}"
        );
        assert!(
            app.auto_apply_structural_verdict.is_none(),
            "no structural verdict is minted from a moment"
        );
        assert!(
            super::forgives_the_counted_trial_launch(deferred, build - 1, build),
            "the new image never ran, so its counted trial launch is given back"
        );
    }

    /// A PRE-PARK CHECK THAT DID NOT FINISH IS THIS PROCESS'S AFTERNOON (round
    /// four, plan item 2): the handoff worker files it `ProducerFailed`, which the
    /// completion classifies TRANSIENT, where a verdict stays `PreparationFailed`
    /// (STRUCTURAL). Before this change the worker sent `PreparationFailed` for
    /// every refusal (`pre_park_refusal_outcome` did not exist).
    #[test]
    fn a_pre_park_check_that_did_not_finish_is_filed_transient() {
        use crate::app_native::{HandoffFailureLane, PhysicalFailureShape};
        let passing = format!(
            "bundle policy: codesign --verify (team-pinned) ran past this apply's \
             verification budget; treating as a rejection ({})",
            aterm_update::PASSING_REFUSAL_KEY
        );
        let lock = format!(
            "pre-verify lock: another process has held the update lock for more than 10s ({})",
            aterm_update::PASSING_REFUSAL_KEY
        );
        for error in [passing.as_str(), lock.as_str()] {
            let outcome = super::pre_park_refusal_outcome(error);
            assert_eq!(
                outcome,
                crate::UpdateHandoffOutcome::ProducerFailed,
                "{error}"
            );
            assert_eq!(
                HandoffFailureLane::classify(
                    ApplyMode::AutomaticPastGrace,
                    outcome,
                    crate::ChildDeathEvidence::Unobserved,
                    false,
                ),
                HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
                "{error}"
            );
        }
        let verdict = "bundle policy: codesign --verify (team-pinned requirement) failed: code \
                       object is not signed at all";
        assert_eq!(
            super::pre_park_refusal_outcome(verdict),
            crate::UpdateHandoffOutcome::PreparationFailed
        );
        assert_eq!(
            HandoffFailureLane::classify(
                ApplyMode::AutomaticPastGrace,
                super::pre_park_refusal_outcome(verdict),
                crate::ChildDeathEvidence::Unobserved,
                false,
            ),
            HandoffFailureLane::Physical(PhysicalFailureShape::Structural)
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
    fn an_unexplained_child_died_gets_a_second_epoch_and_then_only_resamples() {
        use crate::app_native::{
            PHYSICAL_FAILURE_LIFETIME_ATTEMPTS, PHYSICAL_FAILURES_PER_EPOCH,
            UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS,
        };
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
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
        // Two independent samples of the machine is where 'the machine was having
        // a bad hour' stops being a credible explanation for SPENDING THE
        // SCHEDULE — {UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS} attempts, strictly
        // under the {PHYSICAL_FAILURE_LIFETIME_ATTEMPTS} a named transient failure
        // gets, which the compile-time assert beside the constants pins. It is not
        // where the lane may stop for good (the 2026-09-22/23 update audit, plan
        // P1-1(d)): the verdict is still unexplained, so what remains is a quiet
        // re-sample one epoch cooldown out, not the old `None`.
        let resample = deadline(&app)
            .expect("a spent unexplained budget re-samples after the cooldown")
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            resample > std::time::Duration::from_secs(5 * 60 * 60 + 59 * 60)
                && resample <= std::time::Duration::from_secs(6 * 60 * 60),
            "one epoch cooldown out: got {resample:?} after \
             {UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS} of {PHYSICAL_FAILURE_LIFETIME_ATTEMPTS}"
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
/// wrong. Each row of the table below is one field of [`HandoffLaneFacts`]
/// flipped against an otherwise-eligible attempt, because a predicate that
/// ignored a field would otherwise pass every test written against the happy path.
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
            grant_chunks: false,
            environment_is_a_merge: true,
        }
    }

    /// One field of [`HandoffLaneFacts`] flipped per row against an otherwise
    /// eligible attempt, each with the refusal it must produce — the table form
    /// of "a predicate that ignored a field would pass the happy path".
    #[test]
    fn each_fact_alone_decides_the_out_of_band_lane() {
        let limit = crate::handoff_rendezvous::MAX_RENDEZVOUS_SESSIONS;
        let chunked = crate::handoff_rendezvous::MAX_CHUNKED_SESSIONS;
        let rows: [(&str, HandoffLaneFacts, Option<&str>); 13] = [
            // An attempt with nothing wrong with it takes the lane.
            ("eligible", eligible(), None),
            // A platform with no LaunchServices launcher has nothing to start.
            (
                "no launcher",
                HandoffLaneFacts {
                    launcher_available: false,
                    ..eligible()
                },
                Some("this platform has no LaunchServices launcher"),
            ),
            // A dev build, `cargo run`, and the test harness all reach the
            // seamless lane; none of them is a bundle, and LaunchServices has
            // nothing to start.
            (
                "unbundled",
                HandoffLaneFacts {
                    bundled: false,
                    ..eligible()
                },
                Some("this process does not run from a .app bundle"),
            ),
            // THE 16 BYTES. `$HOME` decides whether the rendezvous path fits
            // `sun_path`, so on some perfectly ordinary machines this lane simply
            // does not exist — and the fallback has to be silent and total rather
            // than a bind that fails after the terminal has parked.
            (
                "rendezvous path too long",
                HandoffLaneFacts {
                    socket_path_fits: false,
                    ..eligible()
                },
                Some("the rendezvous path does not fit sun_path on this machine"),
            ),
            // THE DOWNGRADE GUARD the retired version advertisement carried.
            // "Presence of the transport IS the version" closes old-parent/new-
            // child and says NOTHING about new-parent/old-child — so an older
            // authorized target must fork, or a successor with no rendezvous code
            // is handed descriptors it cannot receive and every session is lost.
            (
                "older target",
                HandoffLaneFacts {
                    target_not_older: false,
                    ..eligible()
                },
                Some("the authorized target build is older than this build"),
            ),
            // The transport, not the protocol, bounds the pool: one `SCM_RIGHTS`
            // message carries 64 descriptors and two of them are the pipes.
            // Exactly the limit still fits…
            (
                "sessions == limit",
                HandoffLaneFacts {
                    sessions: limit,
                    ..eligible()
                },
                None,
            ),
            // …one more does not, and must fall back rather than fail at sendmsg…
            (
                "sessions == limit + 1",
                HandoffLaneFacts {
                    sessions: limit + 1,
                    ..eligible()
                },
                Some("the session count does not fit one descriptor message"),
            ),
            // THE CHUNKED GRANT (item 13 of the fifth update-robustness round): a
            // candidate whose verified `Info.plist` declares it takes 100
            // sessions over several messages…
            (
                "100 sessions, chunked grant declared",
                HandoffLaneFacts {
                    sessions: 100,
                    grant_chunks: true,
                    ..eligible()
                },
                None,
            ),
            // …and a candidate that did not declare it still forks them.
            (
                "100 sessions, no chunked grant",
                HandoffLaneFacts {
                    sessions: 100,
                    ..eligible()
                },
                Some("the session count does not fit one descriptor message"),
            ),
            // The protocol's own ceiling still binds a chunked grant.
            (
                "chunked grant at the protocol ceiling",
                HandoffLaneFacts {
                    sessions: chunked,
                    grant_chunks: true,
                    ..eligible()
                },
                None,
            ),
            (
                "chunked grant past the protocol ceiling",
                HandoffLaneFacts {
                    sessions: chunked + 1,
                    grant_chunks: true,
                    ..eligible()
                },
                Some("the session count does not fit the successor's chunked descriptor grant"),
            ),
            // …and an overlap with no sessions is not an overlap.
            (
                "no sessions",
                HandoffLaneFacts {
                    sessions: 0,
                    ..eligible()
                },
                Some("the session count does not fit one descriptor message"),
            ),
            // A LaunchServices launch MERGES its environment and cannot remove a
            // variable. `bind_expected_update_artifact` removes three, and a
            // successor that inherited a stale `ATERM_UPDATE_EXPECTED_*` would
            // authenticate its staged bundle against the wrong artifact — so a
            // removal that would actually remove something disqualifies the lane.
            (
                "environment needs a removal",
                HandoffLaneFacts {
                    environment_is_a_merge: false,
                    ..eligible()
                },
                Some("the launch environment needs a removal a merge cannot express"),
            ),
        ];
        for (label, facts, refusal) in rows {
            assert_eq!(out_of_band_lane_refusal(facts), refusal, "{label}");
        }
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
        crate::test_env::scoped("ATERM_LANE_TEST_PRESENT_KEY", "set", || {
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

    /// SESSION DEATH AT COMMIT IS THE SAME FACT AS AT THE PARK (round six,
    /// finding 37): a handed session that ended between the capture and Commit
    /// refuses the Commit, and the refusal is activity-shaped, so the automatic
    /// lane files it `ActivityRevoked` (the activity spacing, no physical
    /// budget) exactly as the park files the same death — never
    /// `Physical(Transient)`, the 600 s / 1800 s schedule against the build's
    /// lifetime budget. The worker's own detection is typed `ActivityRevoked`
    /// and lands in the same lane.
    #[test]
    fn session_death_before_commit_is_filed_as_the_activity_the_park_files_it_as() {
        use crate::app_native::HandoffFailureLane;
        use crate::native_updater_service::ApplyMode;
        let all = facts_from_bits((1 << 11) - 1);
        assert!(
            handoff_commit_admitted(all),
            "PRECONDITION: all true admits"
        );
        assert!(!handoff_rejection_activity_shaped(all));
        let died = HandoffCommitFacts {
            sessions_alive: false,
            ..all
        };
        assert!(!handoff_commit_admitted(died), "death still refuses");
        assert!(handoff_rejection_activity_shaped(died));
        for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
            for (outcome, main_thread_saw_it) in [
                (crate::UpdateHandoffOutcome::Rejected, true),
                (crate::UpdateHandoffOutcome::ActivityRevoked, false),
            ] {
                assert_eq!(
                    HandoffFailureLane::classify(
                        mode,
                        outcome,
                        crate::ChildDeathEvidence::Unobserved,
                        main_thread_saw_it && handoff_rejection_activity_shaped(died),
                    ),
                    HandoffFailureLane::ActivityRevoked,
                    "{mode:?} {outcome:?}"
                );
            }
        }
        // NEGATIVE CONTROL: a genuine fault (a proof that does not match) is
        // not activity, and stays on the physical schedule.
        let proof = HandoffCommitFacts {
            proof_exact: false,
            ..all
        };
        assert!(!handoff_rejection_activity_shaped(proof));
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

/// THE LIVE FONT ZOOM CROSSES THE HANDOFF (the round-four plan, item 15).
#[cfg(all(test, unix))]
mod font_zoom_carry_tests {
    use crate::App;
    use crate::app_config::{LaunchFont, successor_font_px};
    use crate::session_store::SessionHandoff;

    /// A Retina window whose font the display auto-scaled to 24 px, zoomed by
    /// a person to 26 px (Cmd-= as `set_font_px` makes it), is handed over:
    /// the window carry names the zoom AND the size Cmd-0 resets to, the pair
    /// crosses the manifest's TOML, and the successor — whose own config says
    /// the unpinned 12 px base — comes up at 26 px, pinned (the Retina
    /// auto-scale at its first window's attach leaves it be), with Cmd-0
    /// going back to 24 px and not to the zoom. The next handoff carries it
    /// on; a reset one carries nothing. An unzoomed window writes no key, so
    /// its carry is the wire an older build writes, and an older producer's
    /// carry keeps the config's font. NEGATIVE CONTROLS: a pair with a half
    /// missing or outside the zoom's bounds is ignored whole.
    ///
    /// FAILS WITHOUT THE FIX: the window carry has no zoom, the successor
    /// draws the carried (zoomed) grid at the config's font — a different
    /// frame, different text — and its Cmd-0 target is whatever its display
    /// derives. (Measured: with `handoff_font_zoom` answering `(None, None)`,
    /// the first zoomed-carry assertion fails.)
    #[test]
    fn a_zoomed_font_crosses_the_handoff() {
        let mut parent = App::headless_for_test();
        parent.font_px = 24.0;
        parent.default_font_px = 24.0;
        parent.font_px_explicit = false;
        let unzoomed = parent.handoff_window_carry(0).expect("a window");
        assert_eq!(
            (unzoomed.font_px_milli, unzoomed.font_reset_px_milli),
            (None, None)
        );
        let wire = SessionHandoff {
            schema: SessionHandoff::SCHEMA,
            sessions: Vec::new(),
            window: Some(unzoomed),
            connections: Vec::new(),
            next_turn_id: None,
            outgoing_build: None,
            held: Vec::new(),
        }
        .to_toml()
        .expect("serializes");
        assert!(!wire.contains("font_"), "no key for no zoom: {wire}");

        // Cmd-= (the rebuild seam standing in for the renderer).
        assert!(parent.set_font_px_with(26.0, |_| true));
        assert!(parent.font_px_explicit, "a zoom pins the size");
        let zoomed = parent.handoff_window_carry(0).expect("a window");
        assert_eq!(
            (zoomed.font_px_milli, zoomed.font_reset_px_milli),
            (Some(26_000), Some(24_000))
        );
        let wire = SessionHandoff {
            schema: SessionHandoff::SCHEMA,
            sessions: Vec::new(),
            window: Some(zoomed.clone()),
            connections: Vec::new(),
            next_turn_id: None,
            outgoing_build: None,
            held: Vec::new(),
        }
        .to_toml()
        .expect("serializes");
        let read = SessionHandoff::from_toml(&wire).expect("the successor reads it");
        let carried = read.window.expect("the window carry");
        assert_eq!(carried, zoomed, "the pair round-trips");

        // THE SUCCESSOR, whose config asks for the unpinned 12 px base.
        let launch = successor_font_px(12.0, false, Some(&carried));
        assert_eq!(
            launch,
            LaunchFont {
                px: 26.0,
                explicit: true,
                reset_px: 24.0
            }
        );
        assert_eq!(
            crate::app_window::hidpi_target_font_px(launch.explicit, 2.0),
            None,
            "the first window's Retina auto-scale leaves a carried zoom be"
        );
        let mut successor = App::headless_for_test();
        successor.font_px = launch.px;
        successor.font_px_explicit = launch.explicit;
        successor.default_font_px = launch.reset_px;
        assert_eq!(
            successor.handoff_font_zoom(),
            (Some(26_000), Some(24_000)),
            "and the next handoff carries it on"
        );
        // Cmd-0.
        assert!(successor.set_font_px_with(successor.default_font_px, |_| true));
        assert!((successor.font_px - 24.0).abs() < f32::EPSILON);
        assert_eq!(
            successor.handoff_font_zoom(),
            (None, None),
            "a font back at its reset size is no zoom"
        );

        // An older producer (no pair), and a fresh launch: the config's font.
        let mut older = carried.clone();
        older.font_px_milli = None;
        older.font_reset_px_milli = None;
        let config = LaunchFont {
            px: 14.0,
            explicit: true,
            reset_px: 14.0,
        };
        assert_eq!(successor_font_px(14.0, true, Some(&older)), config);
        assert_eq!(successor_font_px(14.0, true, None), config);
        // NEGATIVE CONTROLS: a pair with a half missing or out of bounds is
        // ignored whole, never clamped to a size nobody chose.
        for (px, reset) in [
            (Some(26_000), None),
            (None, Some(24_000)),
            (Some(0), Some(24_000)),
            (Some(26_000), Some(5_999)),
            (Some(200_001), Some(24_000)),
            (Some(u32::MAX), Some(24_000)),
        ] {
            let mut hostile = carried.clone();
            hostile.font_px_milli = px;
            hostile.font_reset_px_milli = reset;
            assert_eq!(
                successor_font_px(14.0, true, Some(&hostile)),
                config,
                "{px:?} {reset:?}"
            );
        }
        // The bounds themselves are admitted.
        let mut edge = carried;
        edge.font_px_milli = Some(200_000);
        edge.font_reset_px_milli = Some(6_000);
        assert_eq!(
            successor_font_px(14.0, true, Some(&edge)),
            LaunchFont {
                px: 200.0,
                explicit: true,
                reset_px: 6.0
            }
        );
    }
}

/// Round five, item 16: a `video` take running when an update parks is
/// answered at the park, never dropped with the connection at Commit.
#[cfg(all(test, unix))]
mod video_park_tests {
    use super::{
        PRELAUNCH_HOLD_MAX, ParkAttempt, ParkGate, ParkGateFacts, VIDEO_PARK_WAIT_MAX,
        prelaunch_hold_cap, prelaunch_park_admitted, recording_park_refusal,
    };
    use crate::native_update_auto_intent::{ActivityFacts, ApplyPhase};
    use crate::native_updater_service::ApplyMode;

    /// A machine idle in every sense, with a take running.
    fn recording(mode: ApplyMode, phase: ApplyPhase, held_for: std::time::Duration) -> ParkGate {
        prelaunch_park_admitted(
            ParkGateFacts {
                mode,
                phase,
                activity: ActivityFacts {
                    quiet: true,
                    hands_off_keys: true,
                    output_quiet: true,
                    focused: true,
                    consent_warmup: false,
                    harness_restored_pending: false,
                },
                masters_quiet: true,
                masters_alive: true,
                land_waits: 0,
                land_gate_relaxed: false,
                held_for,
                recording: true,
            },
            prelaunch_hold_cap(mode),
        )
    }

    /// The wait is typed and bounded twice over: `Land` and the verb's own
    /// maximum take both end it, and an explicit apply never waits at all.
    #[test]
    fn a_live_take_holds_the_automatic_park_before_land_and_only_until_its_bound() {
        let early = std::time::Duration::from_secs(1);
        for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
            for phase in [
                ApplyPhase::PreferIdle,
                ApplyPhase::PreferOutputGap,
                ApplyPhase::KeysOnly,
            ] {
                assert_eq!(
                    recording(mode, phase, early),
                    ParkGate::Wait("a video recording is running"),
                    "{mode:?} {phase:?}: a take that can still finish is waited for"
                );
                assert_eq!(
                    recording(mode, phase, VIDEO_PARK_WAIT_MAX),
                    ParkGate::Park,
                    "{mode:?} {phase:?}: past the longest take the park proceeds"
                );
            }
            assert_eq!(
                recording(mode, ApplyPhase::Land, early),
                ParkGate::Park,
                "{mode:?}: Land is not held by a take"
            );
        }
        for phase in [ApplyPhase::PreferIdle, ApplyPhase::Land] {
            assert_eq!(
                recording(ApplyMode::Immediate, phase, early),
                ParkGate::Park,
                "an explicit apply parks at once"
            );
        }
        // The bound comes due inside the hold: the take never stands the
        // successor down.
        assert!(VIDEO_PARK_WAIT_MAX < PRELAUNCH_HOLD_MAX);
        // CONTROL: no take, no wait.
        assert_eq!(
            recording_park_refusal(ApplyMode::Automatic, ApplyPhase::PreferIdle, false, early),
            None
        );
    }

    /// A take running when the successor's park lands is answered at COMMIT,
    /// not at the park (round six of the update audit, finding 22). Between
    /// the two the attempt can still fail — the successor's proof never
    /// comes, or on the fork lane the worker's verification or launch of the
    /// candidate fails — and every such failure rolls the readers back with
    /// this process running (`rollback_overlap`): the take must still be
    /// running then, its client told nothing. The Commit that follows a
    /// landed park answers it (`App::video_answer_before_commit`, which the
    /// Commit arm alone calls).
    ///
    /// RED before the fix: the take was answered "aterm is updating" as soon
    /// as the park landed, and a proof that then failed lost it.
    #[test]
    fn a_landed_park_keeps_a_live_take_until_commit_answers_it() {
        // The park keeps its landed cost in the update ledger
        // (`keep_landed_park`): held, as every ledger writer's test holds it, so
        // the dry-run seed tests never read this park's cost as their own.
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = crate::App::headless_for_test();
        // Session 0 on a real PTY, so the park's descriptor reservation and
        // proof term see a live master.
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
        for fd in [master, slave] {
            aterm_pty::set_cloexec(fd, true).expect("close-on-exec");
        }
        app.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .master = master;

        let root = std::env::temp_dir().join(format!(
            "aterm-video-park-{}-{}",
            std::process::id(),
            crate::metrics::now_us()
        ));
        let _ = std::fs::remove_dir_all(&root);
        crate::control_auth::ensure_private_dir(&root).expect("private recording root");
        let dir = crate::control_auth::confine_video_dir(&root).expect("confined recording dir");
        let (reply, replies) = std::sync::mpsc::channel();
        app.video_rec = Some(crate::VideoRec {
            window: crate::WindowId(0),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
            started_us: 0,
            keys: false,
            key_log: Vec::new(),
            trail: false,
            trail_log: Vec::new(),
            trail_seen: 0,
            trail_lost: 0,
            pace_ticks: Vec::new(),
            mode: crate::VideoMode::OffscreenPresentReal,
            next_frame: None,
            presented: None,
            unseamed_at_begin: 0,
            unlogged_other_window: 0,
            dir,
            handoff: None,
            cancel: crate::VideoCancellation::new(),
            reply,
        });
        assert!(app.video_request_live());

        // An explicit apply's launched lane: the gate parks at once.
        let (record, _cancelled, _stood_down, _transferred) =
            late_park_record(7, ApplyMode::Immediate);
        app.update_handoff_prelaunch = Some(record);
        let parked = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
        assert!(
            matches!(parked, ParkAttempt::Parked),
            "PRECONDITION: the park landed ({parked:?})"
        );
        assert!(
            app.video_rec.is_some() && replies.try_recv().is_err(),
            "a landed park leaves the take running and its client untold"
        );

        // THE PROOF FAILS: the attempt rolls back and the process runs on.
        let live = app.handoff_live_sessions();
        app.rollback_overlap(None, &live);
        app.pending_update_handoff = None;
        app.update_handoff_prelaunch = None;
        assert!(
            app.video_rec.is_some(),
            "an update that did not happen leaves the take running"
        );
        assert!(
            replies.try_recv().is_err(),
            "and its client is told nothing"
        );

        // CONTROL: a park that lands and reaches Commit answers it there.
        let (record, _cancelled, _stood_down, _transferred) =
            late_park_record(71, ApplyMode::Immediate);
        app.update_handoff_prelaunch = Some(record);
        let parked = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
        assert!(
            matches!(parked, ParkAttempt::Parked),
            "PRECONDITION: the re-park landed ({parked:?})"
        );
        assert!(replies.try_recv().is_err(), "still untold at the park");
        let answered = app.video_answer_before_commit(super::VIDEO_EXPORT_COMMIT_WAIT);
        assert_eq!(
            answered,
            crate::VideoCommitAnswer {
                answered: true,
                export_settled: true,
                unwritten: 0,
            }
        );
        let (body, _retention) = replies
            .try_recv()
            .expect("Commit answered the take")
            .into_parts();
        assert_eq!(body, "ERR video: recording aborted: aterm is updating\n");
        assert!(app.video_rec.is_none(), "the take is gone");
        assert!(!app.video_request_live());

        app.pending_update_handoff = None;
        app.update_handoff_prelaunch = None;
        drop(app);
        aterm_pty::close_fd(slave);
        aterm_pty::close_fd(master);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A live take on `session 0`'s window, answering on the returned channel.
    fn live_take(
        app: &mut crate::App,
    ) -> (
        std::path::PathBuf,
        std::sync::mpsc::Receiver<crate::control::Retained<String>>,
    ) {
        let root = std::env::temp_dir().join(format!(
            "aterm-video-park-{}-{}",
            std::process::id(),
            crate::metrics::now_us()
        ));
        let _ = std::fs::remove_dir_all(&root);
        crate::control_auth::ensure_private_dir(&root).expect("private recording root");
        let dir = crate::control_auth::confine_video_dir(&root).expect("confined recording dir");
        let (reply, replies) = std::sync::mpsc::channel();
        app.video_rec = Some(crate::VideoRec {
            window: crate::WindowId(0),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
            started_us: 0,
            keys: false,
            key_log: Vec::new(),
            trail: false,
            trail_log: Vec::new(),
            trail_seen: 0,
            trail_lost: 0,
            pace_ticks: Vec::new(),
            mode: crate::VideoMode::OffscreenPresentReal,
            next_frame: None,
            presented: None,
            unseamed_at_begin: 0,
            unlogged_other_window: 0,
            dir,
            handoff: None,
            cancel: crate::VideoCancellation::new(),
            reply,
        });
        (root, replies)
    }

    /// A PARK THAT MISSES KEEPS THE TAKE (round six of the update audit,
    /// finding 22). An explicit apply's park is admitted with a take running,
    /// then its capture meets an engine another thread holds past the rung's
    /// deadline: the park misses, the readers resume, and the process keeps
    /// running — so the take must too, its client not told "aterm is
    /// updating" for an update that did not happen. CONTROL: the re-park
    /// lands, and it is Commit that answers the take.
    ///
    /// RED before the fix: the take was aborted at park admission, before the
    /// capture could miss, and nothing restored it.
    #[test]
    fn a_park_that_misses_keeps_a_live_take() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = crate::App::headless_for_test();
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
        for fd in [master, slave] {
            aterm_pty::set_cloexec(fd, true).expect("close-on-exec");
        }
        app.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .master = master;
        let (root, replies) = live_take(&mut app);
        let (record, _cancelled, _stood_down, _transferred) =
            late_park_record(9, ApplyMode::Immediate);
        app.update_handoff_prelaunch = Some(record);

        let term = app.pool.get(0).expect("session 0").term.clone();
        let holder = super::dry_run_capture_tests::hold_engine_past_every_rung(&term);
        let missed = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
        holder.join().expect("the holder finished");
        assert!(
            matches!(missed, ParkAttempt::Missed(_)),
            "PRECONDITION: the park was admitted and missed ({missed:?})"
        );
        assert!(
            app.video_rec.is_some(),
            "a park that rolled back leaves the take running"
        );
        assert!(
            replies.try_recv().is_err(),
            "and its client is told nothing — no update happened"
        );

        // CONTROL: the park that lands, then its Commit, answers it.
        let parked = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
        assert!(
            matches!(parked, ParkAttempt::Parked),
            "PRECONDITION: the re-park landed ({parked:?})"
        );
        assert!(
            app.video_answer_before_commit(super::VIDEO_EXPORT_COMMIT_WAIT)
                .answered
        );
        let (body, _retention) = replies
            .try_recv()
            .expect("Commit answered the take")
            .into_parts();
        assert_eq!(body, "ERR video: recording aborted: aterm is updating\n");
        assert!(app.video_rec.is_none());

        app.pending_update_handoff = None;
        app.update_handoff_prelaunch = None;
        drop(app);
        aterm_pty::close_fd(slave);
        aterm_pty::close_fd(master);
        let _ = std::fs::remove_dir_all(root);
    }

    /// EVERY `video` REPLY COMMIT CAUSES IS ON ITS CONNECTION BEFORE THE
    /// `_exit` (round six of the update audit, finding 31). Commit aborts a
    /// live take and cancels an export; the export's encode worker sees that
    /// only between frames, and either answer reaches its client only when
    /// the connection thread writes it, after the channel hand-off. Each
    /// stand-in below is a connection thread that writes its reply 150 ms
    /// after the answer arrives (and releases the request's [`ReplyWire`] as
    /// the real write does, `write_control_reply_with_timeout_arm`), and the
    /// export worker answers 200 ms after the cancel: Commit's step
    /// (`App::video_answer_before_commit`, which the Commit arm alone calls)
    /// returns only once both replies are WRITTEN, inside its bound.
    ///
    /// RED before the fix: Commit went from the park's cancel straight to the
    /// `_exit`; with only the export's permit waited for, the export's reply
    /// was still unwritten when it returned.
    ///
    /// [`ReplyWire`]: crate::control::ReplyWire
    #[test]
    fn commit_waits_until_every_video_reply_is_written() {
        let mut app = crate::App::headless_for_test();
        let written = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // A connection thread: waits for its request's answer, writes it
        // 150 ms later, and releases the wire as the write does.
        let connection = |answers: std::sync::mpsc::Receiver<String>| {
            let (wire, release) = crate::control::ReplyWire::new();
            let written = std::sync::Arc::clone(&written);
            let thread = std::thread::spawn(move || {
                let answer = answers.recv().expect("the request is answered");
                std::thread::sleep(std::time::Duration::from_millis(150));
                written.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                drop(release);
                answer
            });
            (wire, thread)
        };

        // A live take, answered on its channel.
        let (root, take_replies) = live_take(&mut app);
        let (take_answers, take_answered) = std::sync::mpsc::channel();
        let forward = std::thread::spawn(move || {
            let (body, _retention) = take_replies
                .recv()
                .expect("the take is answered")
                .into_parts();
            let _ = take_answers.send(body);
        });
        let (take_wire, take_connection) = connection(take_answered);
        app.video_reply_wires.push(take_wire);

        // An export mid-frame: it answers at its next frame boundary, 200 ms
        // after the cancel, and only then drops its permit.
        let export = crate::VideoCancellation::new();
        let permit = app
            .video_export
            .try_begin(export.clone())
            .expect("an export starts");
        let (export_answers, export_answered) = std::sync::mpsc::channel();
        let worker = {
            let export = export.clone();
            std::thread::spawn(move || {
                while !export.is_cancelled() {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
                let _ = export_answers.send("ERR video: export failed".to_string());
                drop(permit);
            })
        };
        let (export_wire, export_connection) = connection(export_answered);
        app.video_reply_wires.push(export_wire);

        let started = std::time::Instant::now();
        let answered = app.video_answer_before_commit(super::VIDEO_EXPORT_COMMIT_WAIT);
        assert_eq!(
            written.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "both replies were written before Commit went on"
        );
        assert_eq!(
            answered,
            crate::VideoCommitAnswer {
                answered: true,
                export_settled: true,
                unwritten: 0,
            }
        );
        assert!(started.elapsed() < super::VIDEO_EXPORT_COMMIT_WAIT);
        assert!(export.is_cancelled() && !app.video_request_live());
        assert!(
            app.video_reply_wires.is_empty(),
            "released wires are pruned"
        );
        forward.join().expect("the take's answer was forwarded");
        worker.join().expect("the worker finished");
        assert_eq!(
            take_connection.join().expect("the take's connection"),
            "ERR video: recording aborted: aterm is updating\n"
        );
        assert_eq!(
            export_connection.join().expect("the export's connection"),
            "ERR video: export failed"
        );

        // BOUNDED: a reply nobody ever writes costs Commit its bound, and is
        // said, not waited on forever.
        let (stuck, _never_released) = crate::control::ReplyWire::new();
        app.video_reply_wires.push(stuck);
        let started = std::time::Instant::now();
        let answered = app.video_answer_before_commit(std::time::Duration::from_millis(50));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(answered.unwritten, 1);
        assert!(!answered.answered);

        // CONTROL: with nothing live the step costs nothing.
        app.video_reply_wires.clear();
        let started = std::time::Instant::now();
        assert_eq!(
            app.video_answer_before_commit(super::VIDEO_EXPORT_COMMIT_WAIT),
            crate::VideoCommitAnswer {
                answered: false,
                export_settled: true,
                unwritten: 0,
            }
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A `video` REQUEST ARRIVING AFTER THE PARK IS REFUSED, NOT ACCEPTED. One
    /// accepted while the parent waits for the successor's proof would only be
    /// cut off by Commit, which answers every live take before its `_exit`. The event loop
    /// asks [`crate::App::video_request_refusal`] before it starts any take.
    ///
    /// RED before the fix: the `Wake::Video` arm checked only for a take or
    /// export already running, so a parked process accepted a new one.
    #[test]
    fn a_video_request_after_the_park_is_refused() {
        let mut app = crate::App::headless_for_test();
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
        for fd in [master, slave] {
            aterm_pty::set_cloexec(fd, true).expect("close-on-exec");
        }
        app.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .master = master;
        assert_eq!(
            app.video_request_refusal(),
            None,
            "an idle, unparked process takes a recording"
        );

        let (record, _cancelled, _stood_down, _transferred) =
            late_park_record(8, ApplyMode::Immediate);
        app.update_handoff_prelaunch = Some(record);
        let parked = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
        assert!(
            matches!(parked, ParkAttempt::Parked),
            "PRECONDITION: the park landed ({parked:?})"
        );
        assert!(app.update_handoff_parked(), "PRECONDITION: parked");
        assert_eq!(
            app.video_request_refusal(),
            Some("aterm is updating"),
            "a take accepted now would die at Commit with no reply"
        );

        app.pending_update_handoff = None;
        app.update_handoff_prelaunch = None;
        assert_eq!(app.video_request_refusal(), None, "and taken again after");
        drop(app);
        aterm_pty::close_fd(slave);
        aterm_pty::close_fd(master);
    }

    fn late_park_record(
        attempt_id: u64,
        mode: ApplyMode,
    ) -> (
        crate::HandoffPrelaunch,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Receiver<super::HandoffStandDown>,
        std::sync::mpsc::Receiver<super::HandoffTransferJob>,
    ) {
        let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
        let (stand_down, stood_down) = std::sync::mpsc::sync_channel(1);
        let (transfer, transferred) = std::sync::mpsc::sync_channel(1);
        (
            crate::HandoffPrelaunch {
                stand_down_ack: std::sync::mpsc::sync_channel(1).0,
                attempt_id,
                nonce: "0123456789abcdef0123456789abcdef".to_string(),
                mode,
                apply_attempt: None,
                same_image: Some(super::SameImageHandoff::DebugSeam),
                target_build: crate::build_info::BUILD_NUMBER.parse().unwrap_or(0),
                target_commit: crate::build_info::GIT_COMMIT.to_string(),
                cancel,
                stand_down,
                transfer,
                arbiter: crate::HandoffAttemptArbiter::new(),
                launched_at: std::time::Instant::now(),
                dialled: None,
                park_retry_at: None,
                park_misses: 0,
                park_mid_sequence_reparks: 0,
                freeze_seed: super::FreezeSeed::Default,
                land_waits: 0,
                last_wait: None,
                stood_down: false,
                teardown: crate::DeferredHandoffTeardown::None,
                revoked_by_activity: false,
                history_export: None,
            },
            cancelled,
            stood_down,
            transferred,
        )
    }
}

/// THE LAUNCHED SUCCESSOR IS OFFERED THE GRANT THE LANE WAS CHOSEN ON (round six
/// of the update audit, item 1).
#[cfg(all(test, target_os = "macos"))]
mod launched_lane_grant_tests {
    use crate::native_updater_service::ApplyMode;

    /// A fresh cached pass that declares the chunked grant lets the lane admit
    /// more sessions than one descriptor message carries AND lets the worker
    /// skip its own verification (`verify_staged_candidate: false`). The job
    /// that worker runs must then carry the same fact, or the launch
    /// environment omits `ATERM_HANDOFF_GRANT_CAPS`, the successor claims
    /// `ATRZ1C`, and `transfer` refuses the desk the lane admitted.
    ///
    /// RED before the fix: both job constructors hard-coded
    /// `successor_grant_chunks: false`, which only the skipped verification
    /// ever overwrote.
    #[test]
    fn a_worker_that_skips_verification_offers_the_grant_the_lane_admitted() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        for grant_chunks in [true, false] {
            let mut app = crate::App::headless_for_test();
            let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
                crate::running_build_number().saturating_add(1),
                "",
                &"ab".repeat(32),
            );
            let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
            let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<super::HandoffWorkerJob>(1);
            let (reconcile_worker, _reconcile_rx) = std::sync::mpsc::sync_channel(1);
            let reconcile_ticket = app
                .mint_native_update_reconcile_ticket()
                .expect("a reconcile ticket");
            app.prelaunch_out_of_band_handoff(super::PrelaunchArgs {
                attempt_id: 31,
                build: crate::running_build_number(),
                mode: ApplyMode::Automatic,
                apply_attempt: Some(ticket.clone()),
                same_image: None,
                target_build: ticket.target_build(),
                target_commit: String::new(),
                command: std::process::Command::new("/usr/bin/true"),
                // A fresh cached pass: the worker does not verify again.
                verify_staged_candidate: false,
                grant_chunks,
                installed_activation: false,
                bundle: None,
                cancel,
                cancelled,
                job_tx,
                reconcile_worker,
                reconcile_ticket,
            })
            .expect("the launched lane records its attempt");
            let job = job_rx.try_recv().expect("the worker's job was sent");
            assert!(!job.verify_staged_candidate, "PRECONDITION");
            assert_eq!(
                job.successor_grant_chunks, grant_chunks,
                "the successor is offered exactly the grant the lane admitted"
            );
            app.update_handoff_prelaunch = None;
        }
    }

    /// A FRESH CACHED PASS DOES NOT OUTLIVE A RAISED FLOOR (round six, item 26,
    /// review round two). The pass was cached when the stage was armed; a later
    /// check raised the operator floor above it. The worker that skips its full
    /// verification on that pass must still ask the floor, for exactly the
    /// attempt's target build, and refuse — or every reader parks for a
    /// successor whose gate 4b retires the stage.
    ///
    /// RED before the fix: the floor was read only inside the full check,
    /// which a fresh pass skips.
    #[test]
    fn a_worker_that_skips_verification_still_refuses_a_raised_floor() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = crate::App::headless_for_test();
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            crate::running_build_number().saturating_add(1),
            "",
            &"ab".repeat(32),
        );
        let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
        let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<super::HandoffWorkerJob>(1);
        let (reconcile_worker, _reconcile_rx) = std::sync::mpsc::sync_channel(1);
        let reconcile_ticket = app
            .mint_native_update_reconcile_ticket()
            .expect("a reconcile ticket");
        app.prelaunch_out_of_band_handoff(super::PrelaunchArgs {
            attempt_id: 32,
            build: crate::running_build_number(),
            mode: ApplyMode::Automatic,
            apply_attempt: Some(ticket.clone()),
            same_image: None,
            target_build: ticket.target_build(),
            target_commit: String::new(),
            command: std::process::Command::new("/usr/bin/true"),
            // A fresh cached pass: the worker does not verify again.
            verify_staged_candidate: false,
            grant_chunks: false,
            installed_activation: false,
            bundle: None,
            cancel,
            cancelled,
            job_tx,
            reconcile_worker,
            reconcile_ticket,
        })
        .expect("the launched lane records its attempt");
        let mut job = job_rx.try_recv().expect("the worker's job was sent");
        assert!(!job.verify_staged_candidate, "PRECONDITION");
        let floor = job.target_build.saturating_add(1);
        let asked = std::cell::Cell::new(None);
        // THE WORKER'S OWN CHECK, as `run_handoff_worker` calls it (round
        // seven, item 106): the full verification is skipped, and the floor
        // still refuses, as the verification's own refusal.
        let refusal = super::worker_pre_park_check(
            &mut job,
            |_| panic!("a fresh cached pass skips the full verification"),
            |build| {
                asked.set(Some(build));
                (build < floor).then(|| format!("below the operator apply floor {floor}"))
            },
        );
        assert_eq!(
            asked.get(),
            Some(job.target_build),
            "the floor is asked of the target"
        );
        assert!(
            matches!(
                &refusal,
                Err(super::PreParkRefusal::Verification { outcome, detail })
                    if *outcome == crate::UpdateHandoffOutcome::PreparationFailed
                        && detail.contains("floor")
            ),
            "a raised floor refuses before anything is launched: {refusal:?}"
        );
        // At the floor, nothing is refused.
        assert_eq!(
            super::worker_pre_park_check(
                &mut job,
                |_| panic!("a fresh cached pass skips the full verification"),
                |_| None
            ),
            Ok(())
        );
        // NEGATIVE CONTROL: a worker that verifies never asks the floor
        // separately (the full check reads it), and files the verifier's word.
        job.verify_staged_candidate = true;
        assert_eq!(
            super::worker_pre_park_check(
                &mut job,
                |_| Err("codesign --verify (structural) failed".to_string()),
                |_| panic!("the full check reads the floor itself"),
            ),
            Err(super::PreParkRefusal::Verification {
                outcome: crate::UpdateHandoffOutcome::PreparationFailed,
                detail: "staged update failed pre-park verification: codesign --verify \
                         (structural) failed"
                    .to_string(),
            })
        );
        app.update_handoff_prelaunch = None;
    }
}

/// ROUND SIX, FINDING 2 (the rollback half): a successor may resize a carried
/// PTY before Commit — its adoption pulse, its first `resize_panes` — and a
/// rejected Commit then hands the program back to THIS process. Its engine is
/// still at the size it had at the park, and its own `resize_panes` skips a
/// pane whose engine already matches, so without a re-assert the program kept
/// the successor's size for the rest of the session.
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
        let (mut master, mut slave) = (-1i32, -1i32);
        let mut rc = -1;
        for _ in 0..20 {
            // SAFETY: `openpty` fills the two out-params; the optional
            // name/termios/winsize pointers are null.
            rc = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if rc == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(rc, 0, "openpty");
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
        // SAFETY: both fds were opened by `openpty` above and are still open;
        // the stub session's sink names no descriptor, so nothing else closes them.
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }
}

/// THE FORK LANE NEVER CAPTURES ABOVE A POLICY IT HAS NOT WAITED FOR (round
/// seven, item 36). With no verified pass on record the fork lane parked and
/// captured at `Full` on the main thread before its worker read the policy, so
/// a candidate sealed `carry = "repaint"` to route around a projection that
/// panics or hangs could not: the capture ran first. An automatic attempt now
/// defers as activity for as long as the policy read is in flight (an arm-time
/// `codesign` past its budget on a loaded desk included), up to the read's
/// ceiling, and the attempt after it ends parks under what it answered — or,
/// nothing on record, as before. NEGATIVE CONTROLS: a person's apply, a policy
/// already known, a read that ended with nothing, no read at all and the
/// ceiling spent do not defer.
///
/// RED before the fix: nothing deferred; the park ran at `Full`. RED against
/// the first fix (one deferral per target, spent whether or not the read had
/// ended): the second attempt, with the read still running, parked at `Full`.
#[cfg(all(test, unix))]
mod fork_lane_policy_wait_tests {
    use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode};
    use aterm_update_core::handoff_policy::{CarryCeiling, HandoffPolicy};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn artifact() -> String {
        "ab".repeat(32)
    }

    /// A read of `(build, COMMIT, artifact)` started now and still running —
    /// the arm-time read the spacing outran — and the flag that ends it.
    fn read_in_flight(app: &mut crate::App, build: u64) -> Arc<AtomicBool> {
        let done = Arc::new(AtomicBool::new(false));
        app.handoff_preverify_in_flight = Some(crate::PreverifyInFlight {
            build,
            commit: COMMIT.to_string(),
            artifact: artifact(),
            deadline: Instant::now() + crate::HANDOFF_PREVERIFY_READ_CEILING,
            done: Arc::clone(&done),
        });
        done
    }

    fn unread(app: &crate::App, ticket: &ApplyAttemptTicket) -> bool {
        !app.park_policy(Some(ticket)).known
    }

    #[test]
    fn the_fork_lane_waits_while_the_policy_read_runs_and_parks_under_its_answer() {
        let mut app = crate::App::headless_for_test();
        let target = crate::running_build_number().saturating_add(1);
        let ticket = ApplyAttemptTicket::for_test(target, COMMIT, &artifact());
        let done = read_in_flight(&mut app, target);
        assert!(unread(&app, &ticket));

        // Every attempt while the read runs defers — not only the first.
        for mode in [
            ApplyMode::Automatic,
            ApplyMode::Automatic,
            ApplyMode::AutomaticPastGrace,
        ] {
            let wait = app
                .fork_lane_policy_wait(mode, target, unread(&app, &ticket))
                .expect("an automatic attempt waits while the policy is being read");
            assert!(
                wait.stops_routinely(mode),
                "as routine activity, never a failure: {wait}"
            );
        }
        assert!(
            app.fork_lane_policy_wait(ApplyMode::Immediate, target, true)
                .is_none(),
            "a person's apply does not wait"
        );

        // The read ends: its verdict is published, then its flag raised.
        *app.handoff_preverified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(crate::HandoffPreverification {
                build: target,
                commit: COMMIT.to_string(),
                artifact: artifact(),
                at: Instant::now(),
                passed: true,
                reason: None,
                policy: Some(
                    HandoffPolicy::parse("schema = 1\ncarry = \"repaint\"\n").expect("schema 1"),
                ),
                grant_chunks: false,
            });
        done.store(true, Ordering::Release);

        // The next attempt parks — under the policy the read answered.
        let policy = app.park_policy(Some(&ticket));
        assert!(policy.known, "the policy is known by the next park");
        assert_eq!(policy.carry_ceiling(), CarryCeiling::Repaint);
        assert!(
            app.fork_lane_policy_wait(ApplyMode::Automatic, target, !policy.known)
                .is_none(),
            "a known policy needs no wait"
        );

        // Wired where the fork lane parks: before its readers stop.
        let src = include_str!("app_update_handoff.rs");
        let wait_at = src
            .find(
                "if let Some(wait) = self.fork_lane_policy_wait(mode, target_build, policy_unread)",
            )
            .expect("the fork lane asks");
        let park_at = src[wait_at..]
            .find("if !self.park_all_readers(deadline) {")
            .expect("and parks after");
        assert!(park_at > 0);
    }

    #[test]
    fn a_read_that_ended_is_absent_or_outran_its_ceiling_is_not_waited_for() {
        let mut app = crate::App::headless_for_test();
        let running = crate::running_build_number();

        // Ended with nothing on record (a check that did not finish).
        let target = running.saturating_add(1);
        read_in_flight(&mut app, target).store(true, Ordering::Release);
        assert!(
            app.fork_lane_policy_wait(ApplyMode::Automatic, target, true)
                .is_none(),
            "a read that ended with nothing: the fork lane parks as it always did"
        );

        // No read at all: none could start (a headless app stages nothing).
        let target = running.saturating_add(2);
        app.handoff_preverify_in_flight = None;
        assert!(
            app.fork_lane_policy_wait(ApplyMode::Automatic, target, true)
                .is_none(),
            "nothing is being read, so nothing is waited for"
        );
        assert!(app.handoff_preverify_in_flight.is_none());

        // Still running, but the fork lane's ceiling at this target is spent —
        // which the verifier's own bound says only a thread the scheduler never
        // ran can reach (`the_ceiling_outlasts_every_instant_a_read_can_hold_the_lock`):
        // the wait still ends, and the park is exactly the one before the fix.
        let target = running.saturating_add(3);
        read_in_flight(&mut app, target);
        let Some(spent) = Instant::now().checked_sub(Duration::from_millis(1)) else {
            return;
        };
        app.fork_policy_read_waited = Some((target, spent));
        assert!(
            app.fork_lane_policy_wait(ApplyMode::Automatic, target, true)
                .is_none(),
            "the wait is bounded: past its ceiling the fork lane parks"
        );
        // …and a read for ANOTHER target is not this target's.
        let other = running.saturating_add(4);
        assert!(
            app.fork_lane_policy_wait(ApplyMode::Automatic, other, true)
                .is_none(),
            "only a read of this target is waited for"
        );
    }

    /// THE CEILING IS A BOUND, NOT A HOPE (round seven, item 36, second review).
    /// A pre-verification of the target holds, or queues for, the staged apply
    /// lock its worker's in-window check must take; parking beside it froze every
    /// reader for the lock wait and then refused. The fork lane therefore must not
    /// park at any instant a same-key read can still hold that lock — and
    /// `aterm-update` bounds that: the lock wait plus the verification budget
    /// the check runs under (`HANDOFF_PREVERIFY_BOUND`, pinned there by
    /// `a_pre_verification_holds_the_lock_and_runs_under_the_verification_budget`).
    /// So the ceiling is derived from that bound and exceeds it, and at the LAST
    /// instant a read can hold the lock the lane still defers. RED against the
    /// second fix: the ceiling was a restated 20 s over a check that ran under no
    /// budget at all, so a read on a slow desk outlived it and the lane parked
    /// beside it.
    #[test]
    fn the_ceiling_outlasts_every_instant_a_read_can_hold_the_lock() {
        assert_eq!(
            crate::HANDOFF_PREVERIFY_READ_CEILING,
            aterm_update::HANDOFF_PREVERIFY_BOUND + crate::HANDOFF_PREVERIFY_READ_MARGIN,
            "the ceiling is derived from the verifier's bound, not restated"
        );
        assert!(crate::HANDOFF_PREVERIFY_READ_MARGIN > Duration::ZERO);

        let mut app = crate::App::headless_for_test();
        let target = crate::running_build_number().saturating_add(1);
        // The wait and the read began one whole bound ago: this is the last
        // instant the read can still be inside its lock wait and its budget.
        let now = Instant::now();
        if now
            .checked_sub(aterm_update::HANDOFF_PREVERIFY_BOUND)
            .is_none()
        {
            return;
        }
        let until = now + crate::HANDOFF_PREVERIFY_READ_MARGIN;
        let done = Arc::new(AtomicBool::new(false));
        app.handoff_preverify_in_flight = Some(crate::PreverifyInFlight {
            build: target,
            commit: COMMIT.to_string(),
            artifact: artifact(),
            deadline: until,
            done: Arc::clone(&done),
        });
        app.fork_policy_read_waited = Some((target, until));
        let deferred = app.fork_lane_policy_wait(ApplyMode::Automatic, target, true);
        assert!(
            deferred.is_some(),
            "a same-key read that may still hold the apply lock: the fork lane does not park"
        );
        assert!(
            app.handoff_preverify_in_flight
                .as_ref()
                .is_some_and(|read| Arc::ptr_eq(&read.done, &done)),
            "and no second read is started beside it"
        );
        // Once the read ends, the lane parks under whatever it published.
        done.store(true, Ordering::Release);
        assert!(
            app.fork_lane_policy_wait(ApplyMode::Automatic, target, true)
                .is_none()
        );
    }

    #[test]
    fn a_read_in_flight_is_not_started_twice() {
        use crate::app_native::preverify_read_needed;
        let target = crate::running_build_number().saturating_add(1);
        let now = Instant::now();
        let artifact = artifact();
        let flight = |build: u64, commit: &str, artifact: &str, deadline: Instant, ended: bool| {
            crate::PreverifyInFlight {
                build,
                commit: commit.to_string(),
                artifact: artifact.to_string(),
                deadline,
                done: Arc::new(AtomicBool::new(ended)),
            }
        };
        let later = now + crate::HANDOFF_PREVERIFY_READ_CEILING;
        assert!(preverify_read_needed(
            None, None, target, COMMIT, &artifact, now
        ));
        assert!(
            !preverify_read_needed(
                None,
                Some(&flight(target, COMMIT, &artifact, later, false)),
                target,
                COMMIT,
                &artifact,
                now,
            ),
            "the same key still in flight: no second codesign beside it"
        );
        for (read, why) in [
            (
                flight(target, COMMIT, &artifact, later, true),
                "a read that ended",
            ),
            (
                flight(target, COMMIT, &artifact, now, false),
                "one past its ceiling",
            ),
            (
                flight(target + 1, COMMIT, &artifact, later, false),
                "another build",
            ),
            (
                flight(target, &"f".repeat(40), &artifact, later, false),
                "another commit",
            ),
            (
                flight(target, COMMIT, &"cd".repeat(32), later, false),
                "another artifact",
            ),
        ] {
            assert!(
                preverify_read_needed(None, Some(&read), target, COMMIT, &artifact, now),
                "{why} does not stand in for a read of this key"
            );
        }
        let fresh = crate::HandoffPreverification {
            build: target,
            commit: COMMIT.to_string(),
            artifact: artifact.clone(),
            at: now,
            passed: true,
            reason: None,
            policy: None,
            grant_chunks: false,
        };
        assert!(!preverify_read_needed(
            Some(&fresh),
            None,
            target,
            COMMIT,
            &artifact,
            now
        ));

        // Wired: the spawn asks, records what it started, and its thread
        // raises the flag on every path out.
        let src = include_str!("app_native.rs");
        let body = &src[src
            .find("pub(crate) fn spawn_staged_handoff_preverification(")
            .expect("the spawn")..];
        let body = &body[..body.find("\n    }\n").expect("its end")];
        assert!(body.contains("if !preverify_read_needed("));
        assert!(body.contains("let _ends = RaiseOnDrop(done);"));
        assert!(body.contains("Ok(_) => self.handoff_preverify_in_flight = Some(flight),"));
    }
}

/// A MANIFEST THE DESK'S CONTENT PUTS OVER ITS CAP IS A REFUSAL, NOT THIS
/// PROCESS'S MOMENT (round seven, item 108). `write_outgoing_with_held` refuses
/// typed (`ManifestOverCap`) once every optional part is shed and the metas
/// are already settled under their budget: what is left is the desk itself,
/// which the next attempt captures again. Filed `ProducerFailed` (Transient),
/// every retry froze every reader for the same answer until the physical
/// budget converged — a deterministic failure booked as passing (L4). It is
/// now the refusal lane's, as a capture refusal is. NEGATIVE CONTROL: a
/// filesystem refusal, a missing private directory and this build's own
/// inconsistency stay this process's trouble.
///
/// RED before the fix: every write failure mapped to `Producer`.
#[cfg(all(test, unix))]
mod manifest_over_cap_tests {
    use super::{PreparationFailure, preparation_failure_of_write};
    use crate::seamless::WriteOutgoingFailure;

    #[test]
    fn an_over_cap_manifest_is_filed_on_the_refusal_lane() {
        let refused = preparation_failure_of_write(WriteOutgoingFailure::ManifestOverCap);
        assert!(
            matches!(&refused, PreparationFailure::Refused(detail)
                if detail.contains("cap") && detail.contains("once the desk changes")),
            "a deterministic refusal of the desk's content"
        );
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
